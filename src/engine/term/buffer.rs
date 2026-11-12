//! Terminal buffers: a buffer whose contents is a [`Terminal`] attached to a
//! PTY session. In *char mode* keys go to the program, except the escape keys
//! from the configuration (`terminal.escape_keys`, by default `C-x`, `M-x`
//! and `C-c`) which start an editor key sequence looked up in the `terminal`
//! keymap first. *Copy mode* (`C-c C-j`) puts the scrollback and screen
//! text into the buffer, read-only, for normal motion, search and copying;
//! `C-c C-k` goes back to char mode.

use std::io;
use std::path::PathBuf;

use super::input::{encode_key, encode_paste};
use super::{Palette, Snapshot, Terminal};
use crate::engine::buffer::{Buffer, BufferId, BufferKind};
use crate::engine::commands::{CmdResult, Command, flags};
use crate::engine::cursor::Cursor;
use crate::engine::editor::{Args, Editor};
use crate::engine::hooks::Event;
use crate::engine::keymap::Key;
use crate::engine::modes::MinorMode;
use crate::engine::pty::{ExitStatus, PtyBackend, PtyEvent, PtySize, SessionId, SpawnSpec};
use crate::engine::window::Window;

/// Output `poll_pty` feeds per call before yielding (another tick follows),
/// so a flood can't freeze the UI.
pub const POLL_BUDGET_BYTES: usize = 4 << 20;

/// The name `term` gives new terminal buffers (made unique: `*term*<2>`...).
pub const TERM_BUFFER_NAME: &str = "*term*";

/// How keys reach a terminal buffer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputMode {
    /// Keys go to the program (except the escape keys).
    #[default]
    Char,
    /// The buffer holds a text copy of the terminal; keys are editor keys.
    Copy,
}

/// The terminal side of a terminal buffer (in `Editor.terminals`, by buffer id).
pub struct TermState {
    pub term: Terminal,
    pub session: SessionId,
    /// What was run (shown in the mode line, saved in the session file).
    /// `spec.size` follows resizes.
    pub spec: SpawnSpec,
    pub mode: InputMode,
    /// Set when the process has exited (the buffer stays, like Emacs).
    pub exited: Option<ExitStatus>,
}

/// Terminal buffer commands (looked up by `commands::lookup`).
pub static COMMANDS: &[Command] = &[
    Command { name: "term", run: term, flags: flags::NO_UNDO, doc: "Start a shell in a new terminal buffer (*term*)." },
    Command { name: "ansi-term", run: term, flags: flags::NO_UNDO, doc: "Start a shell in a new terminal buffer (same as term)." },
    Command { name: "term-char-mode", run: term_char_mode, flags: flags::NO_UNDO, doc: "Send keys to the terminal's program (leave copy mode)." },
    Command { name: "term-copy-mode", run: term_copy_mode, flags: flags::NO_UNDO, doc: "Copy mode: move, search and copy in the terminal's text." },
    Command { name: "term-interrupt-subjob", run: term_interrupt_subjob, flags: flags::NO_UNDO, doc: "Send an interrupt (C-c) to the terminal's program." },
    Command { name: "term-send-eof", run: term_send_eof, flags: flags::NO_UNDO, doc: "Send end-of-file (C-d) to the terminal's program." },
    Command { name: "term-stop-subjob", run: term_stop_subjob, flags: flags::NO_UNDO, doc: "Send a stop (C-z) to the terminal's program." },
    Command { name: "term-quit-subjob", run: term_quit_subjob, flags: flags::NO_UNDO, doc: "Send a quit (C-\\) to the terminal's program." },
    Command { name: "term-paste", run: term_paste, flags: flags::NO_UNDO, doc: "Paste the last kill into the terminal (bracketed if the program asked)." },
    Command { name: "term-scroll-up", run: term_scroll_up, flags: flags::NO_UNDO, doc: "Scroll the terminal view back into the history (a page, or N lines)." },
    Command { name: "term-scroll-down", run: term_scroll_down, flags: flags::NO_UNDO, doc: "Scroll the terminal view toward the live screen (a page, or N lines)." },
];

fn term(ed: &mut Editor, _: &Args) -> CmdResult {
    let spec = ed.shell_spec();
    ed.open_terminal(TERM_BUFFER_NAME, spec).map(|_| ()).map_err(|e| format!("Can't start a terminal: {e}"))
}

/// The current buffer's terminal, or an error naming the command.
fn current<'a>(ed: &'a mut Editor, cmd: &str) -> Result<&'a mut TermState, String> {
    let id = ed.buf_id;
    ed.terminals.get_mut(&id).ok_or_else(|| format!("{cmd}: not a terminal buffer"))
}

fn term_char_mode(ed: &mut Editor, _: &Args) -> CmdResult {
    current(ed, "term-char-mode")?;
    ed.set_input_mode(InputMode::Char);
    Ok(())
}

fn term_copy_mode(ed: &mut Editor, _: &Args) -> CmdResult {
    current(ed, "term-copy-mode")?;
    ed.set_input_mode(InputMode::Copy);
    Ok(())
}

/// Send `bytes` to the current terminal's program.
fn send(ed: &mut Editor, cmd: &str, bytes: &[u8]) -> CmdResult {
    let t = current(ed, cmd)?;
    if t.exited.is_some() {
        return Err(format!("{cmd}: the process has exited"));
    }
    let session = t.session;
    t.term.scroll_to_bottom();
    ed.write_session(session, bytes)
}

fn term_interrupt_subjob(ed: &mut Editor, _: &Args) -> CmdResult {
    send(ed, "term-interrupt-subjob", &[0x03])
}

fn term_send_eof(ed: &mut Editor, _: &Args) -> CmdResult {
    send(ed, "term-send-eof", &[0x04])
}

fn term_stop_subjob(ed: &mut Editor, _: &Args) -> CmdResult {
    send(ed, "term-stop-subjob", &[0x1a])
}

fn term_quit_subjob(ed: &mut Editor, _: &Args) -> CmdResult {
    send(ed, "term-quit-subjob", &[0x1c])
}

fn term_paste(ed: &mut Editor, _: &Args) -> CmdResult {
    let text = ed.kill_ring.last().cloned().ok_or("Kill ring is empty")?;
    let t = current(ed, "term-paste")?;
    let bytes = encode_paste(&text, t.term.modes());
    send(ed, "term-paste", &bytes)
}

/// Lines to scroll: N with a prefix argument, else a page (the screen less two lines).
fn scroll_amount(t: &TermState, a: &Args) -> i32 {
    match a.raw {
        Some(_) => a.n().clamp(i32::MIN as i64 + 1, i32::MAX as i64) as i32,
        None => (t.term.size().1 as i32 - 2).max(1),
    }
}

fn term_scroll_up(ed: &mut Editor, a: &Args) -> CmdResult {
    let t = current(ed, "term-scroll-up")?;
    let n = scroll_amount(t, a);
    t.term.scroll(n);
    Ok(())
}

fn term_scroll_down(ed: &mut Editor, a: &Args) -> CmdResult {
    let t = current(ed, "term-scroll-down")?;
    let n = scroll_amount(t, a);
    t.term.scroll(-n);
    Ok(())
}

impl Editor {
    pub fn is_terminal(&self, buf: BufferId) -> bool {
        self.terminals.contains_key(&buf)
    }

    pub fn terminal(&self, buf: BufferId) -> Option<&TermState> {
        self.terminals.get(&buf)
    }

    /// What to draw for terminal buffer `buf`. `None` if it isn't one, or in
    /// copy mode (the view then draws the buffer's text).
    pub fn terminal_snapshot(&self, buf: BufferId, palette: &Palette) -> Option<Snapshot> {
        self.terminals.get(&buf).filter(|t| t.mode == InputMode::Char).map(|t| t.term.snapshot(palette))
    }

    /// What `term` runs: the configured shell in the current buffer's
    /// directory (else the editor's), at the size of the current terminal
    /// (else of the newest one, else 80×24).
    pub fn shell_spec(&self) -> SpawnSpec {
        let cfg = &self.terminal_config;
        let cwd = self
            .buf
            .path
            .as_deref()
            .and_then(|p| p.parent())
            .filter(|d| d.is_dir())
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok());
        let size = self
            .terminals
            .get(&self.buf_id)
            .or_else(|| self.terminals.iter().max_by_key(|(id, _)| **id).map(|(_, t)| t))
            .map(|t| t.spec.size)
            .unwrap_or_default();
        SpawnSpec {
            program: cfg.shell_program(),
            args: cfg.shell_args.clone(),
            cwd,
            env: vec![("TERM".into(), cfg.term.clone()), ("COLORTERM".into(), "truecolor".into())],
            size,
        }
    }

    /// A new terminal buffer called `name` (made unique) for `session`.
    fn add_terminal_buffer(&mut self, name: &str, session: SessionId, spec: SpawnSpec) -> BufferId {
        let mut buf = Buffer::from_str(name, "");
        buf.kind = BufferKind::Terminal;
        let id = self.add_buffer(buf);
        let term = Terminal::new(spec.size.cols, spec.size.rows, self.terminal_config.scrollback_lines);
        self.terminals.insert(id, TermState { term, session, spec, mode: InputMode::Char, exited: None });
        id
    }

    /// Start `spec` in a new terminal buffer called `name` (made unique) and
    /// make it current. Returns the buffer id.
    pub fn open_terminal(&mut self, name: &str, spec: SpawnSpec) -> io::Result<BufferId> {
        let session = self.pty_backend()?.spawn(spec.clone())?;
        let id = self.add_terminal_buffer(name, session, spec);
        self.switch_to_buffer(id);
        Ok(id)
    }

    /// Make a terminal buffer for a session that already runs in the backend
    /// (session restore). The buffer is added behind the current one. Fails
    /// (and adds nothing) if the backend doesn't know the session.
    pub fn attach_terminal(&mut self, name: &str, session: SessionId, spec: SpawnSpec) -> io::Result<BufferId> {
        self.pty_backend()?.attach(session, spec.size)?;
        Ok(self.add_terminal_buffer(name, session, spec))
    }

    /// Write to a session, reporting failure as a command error.
    fn write_session(&mut self, session: SessionId, bytes: &[u8]) -> CmdResult {
        let backend = self.pty.as_deref_mut().ok_or("No terminal backend is running")?;
        backend.write(session, bytes).map_err(|e| format!("Can't write to the terminal: {e}"))
    }

    /// Whether the char-mode dispatcher should see `key` instead of the
    /// program: Super chords, escape keys, and keys the `terminal` keymap
    /// binds or starts a sequence with.
    fn key_for_editor(&self, key: &Key) -> bool {
        let k = key.canonical();
        let keymap = &self.terminal_config.keymap;
        let seq = std::slice::from_ref(&k);
        key.sup
            || self.terminal_config.escape_keys.iter().any(|e| *e == k)
            || keymap.lookup(seq).is_some()
            || keymap.is_prefix(seq)
    }

    /// Handle a key typed in the current buffer before the key dispatcher
    /// sees it. Returns true if it went to the terminal's program (or was
    /// swallowed because the program has exited).
    pub fn terminal_key(&mut self, key: &Key) -> bool {
        let id = self.buf_id;
        let Some(t) = self.terminals.get(&id).filter(|t| t.mode == InputMode::Char) else { return false };
        if self.key_for_editor(key) {
            return false;
        }
        let Some(bytes) = encode_key(key, t.term.modes()) else { return false };
        if t.exited.is_some() {
            return true;
        }
        let session = t.session;
        if let Some(t) = self.terminals.get_mut(&id) {
            t.term.scroll_to_bottom();
        }
        if let Err(e) = self.write_session(session, &bytes) {
            self.message = Some(e);
        }
        true
    }

    /// Paste `text` into the current terminal buffer (bracketed if the
    /// program asked). False if it isn't one or its process has exited.
    pub fn terminal_paste(&mut self, text: &str) -> bool {
        let Some(t) = self.terminals.get_mut(&self.buf_id).filter(|t| t.exited.is_none()) else { return false };
        let bytes = encode_paste(text, t.term.modes());
        t.term.scroll_to_bottom();
        let session = t.session;
        if let Err(e) = self.write_session(session, &bytes) {
            self.message = Some(e);
        }
        true
    }

    /// The window showing `buf` is `cols`×`rows` cells. Resizes the terminal
    /// and the PTY when that changes (repeated sizes do nothing).
    pub fn resize_terminal(&mut self, buf: BufferId, cols: u16, rows: u16) {
        let Some(t) = self.terminals.get_mut(&buf) else { return };
        let before = t.term.size();
        t.term.resize(cols, rows);
        let (cols, rows) = t.term.size();
        if (cols, rows) == before {
            return;
        }
        t.spec.size = PtySize { cols, rows, ..t.spec.size };
        let (session, size, exited) = (t.session, t.spec.size, t.exited.is_some());
        if let (false, Some(b)) = (exited, self.pty.as_deref_mut()) {
            // A session that just ended can't be resized; nothing to report.
            let _ = b.resize(session, size);
        }
    }

    /// Switch the current terminal buffer between char and copy mode.
    fn set_input_mode(&mut self, mode: InputMode) {
        let id = self.buf_id;
        let Some(t) = self.terminals.get_mut(&id) else { return };
        t.mode = mode;
        let (text, (line, col)) = match mode {
            InputMode::Copy => t.term.text(),
            InputMode::Char => (String::new(), (0, 0)),
        };
        self.set_terminal_text(&text);
        let point = self.buf.rope.line_to_char(line.min(self.buf.rope.len_lines() - 1)) + col;
        self.cursors = vec![Cursor::new(1, point.min(self.buf.rope.len_chars()))];
        self.primary = 1;
        self.next_cursor_id = 2;
        self.set_minor_mode(MinorMode::ReadOnly, mode == InputMode::Copy);
        self.message = Some(match mode {
            InputMode::Copy => "Copy mode: C-c C-k returns to the terminal".into(),
            InputMode::Char => "Char mode".into(),
        });
    }

    /// Replace the current buffer's text without undo history (the old
    /// history doesn't apply to the new text); other windows showing it
    /// start at its beginning.
    fn set_terminal_text(&mut self, text: &str) {
        let fresh = Buffer::from_str(&self.buf.name, text);
        let old = std::mem::replace(&mut self.buf, fresh);
        self.buf.kind = old.kind;
        self.buf.path = old.path;
        self.buf.modes = old.modes;
        let (id, selected) = (self.buf_id, self.layout.selected());
        for w in self.layout.windows.values_mut().filter(|w| w.buffer == id && w.id != selected) {
            *w = Window::new(w.id, id);
        }
    }

    /// Called by `kill_buffer`: forget the terminal state and kill the session.
    pub(crate) fn terminal_buffer_killed(&mut self, buf: BufferId) {
        if let Some(t) = self.terminals.remove(&buf) {
            if let Some(b) = self.pty.as_mut() {
                let _ = b.kill(t.session);
            }
        }
    }

    /// Read waiting backend events: feed output to the terminals, write
    /// their replies back, emit terminal hooks. Returns whether anything
    /// arrived (the view should redraw). Stops after [`POLL_BUDGET_BYTES`]
    /// of output and wakes the view for another round.
    pub fn poll_pty(&mut self) -> bool {
        let Some(backend) = self.pty.as_deref_mut() else { return false };
        let mut any = false;
        let mut fed = 0;
        let mut events = Vec::new();
        let mut output_bufs: Vec<BufferId> = Vec::new();
        let mut exited = Vec::new();
        loop {
            if fed >= POLL_BUDGET_BYTES {
                (self.waker)();
                break;
            }
            let Some(e) = backend.try_recv() else { break };
            any = true;
            let session = match &e {
                PtyEvent::Output(s, _) | PtyEvent::Exited(s, _) => *s,
            };
            let Some((&buf, t)) = self.terminals.iter_mut().find(|(_, t)| t.session == session) else { continue };
            match e {
                PtyEvent::Output(_, bytes) => {
                    fed += bytes.len();
                    feed_output(backend, buf, t, &bytes, &mut events);
                    if !output_bufs.contains(&buf) {
                        output_bufs.push(buf);
                    }
                }
                PtyEvent::Exited(_, status) => {
                    t.exited = Some(status.clone());
                    exited.push((buf, status));
                }
            }
        }
        for buf in output_bufs {
            self.emit(Event::TerminalOutput { buf });
        }
        for e in events {
            self.emit(e);
        }
        for (buf, status) in exited {
            self.message = Some(exit_message(&self.buffer_name(buf), &status));
            self.emit(Event::ProcessExited { buf, status });
        }
        any
    }

    fn buffer_name(&self, id: BufferId) -> String {
        self.find_buffer(id).map(|v| v.buf.name.clone()).unwrap_or_default()
    }
}

/// Feed output to a terminal, write its replies back, and collect the title
/// and bell events it caused.
fn feed_output(backend: &mut dyn PtyBackend, buf: BufferId, t: &mut TermState, bytes: &[u8], events: &mut Vec<Event>) {
    let feed = t.term.feed(bytes);
    if !feed.replies.is_empty() && t.exited.is_none() {
        // A reply to a process that is going away is not worth reporting.
        let _ = backend.write(t.session, &feed.replies);
    }
    if let Some(title) = feed.title {
        events.push(Event::TerminalTitle { buf, title });
    }
    if feed.bell {
        events.push(Event::TerminalBell { buf });
    }
}

/// The echo-area note for a process that ended.
fn exit_message(name: &str, status: &ExitStatus) -> String {
    match (&status.signal, status.code) {
        (Some(sig), _) => format!("Process {name} exited (killed by signal {sig})"),
        (None, 0) => format!("Process {name} exited"),
        (None, code) => format!("Process {name} exited with code {code}"),
    }
}

#[cfg(test)]
#[path = "buffer_tests.rs"]
mod tests;
