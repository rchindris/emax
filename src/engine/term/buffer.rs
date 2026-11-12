//! Terminal buffers: a buffer whose contents is a [`Terminal`] attached to a
//! PTY session. In *char mode* keys go to the program, except the escape keys
//! from the configuration (`terminal.escape_keys`, by default `C-x`, `M-x`
//! and `C-c`) which start an editor key sequence looked up in the `terminal`
//! keymap first. *Copy mode* (`C-c C-j`) puts the scrollback and screen
//! text into the buffer, read-only, for normal motion, search and copying;
//! `C-c C-k` goes back to char mode.
//!
//! SKELETON: owned by the term builder (see docs/design/terminal-substrate.md).
//! Commands are registered and fail with "not implemented yet".

use std::io;

use super::{Palette, Snapshot, Terminal};
use crate::engine::buffer::BufferId;
use crate::engine::commands::{CmdResult, Command, flags};
use crate::engine::editor::{Args, Editor};
use crate::engine::keymap::Key;
use crate::engine::pty::{ExitStatus, PtyEvent, SessionId, SpawnSpec};

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
    pub spec: SpawnSpec,
    pub mode: InputMode,
    /// Set when the process has exited (the buffer stays, like Emacs).
    pub exited: Option<ExitStatus>,
}

fn not_yet(name: &str) -> CmdResult {
    Err(format!("{name}: not implemented yet"))
}

macro_rules! stub_commands {
    ($( $name:literal => $f:ident, $doc:literal; )*) => {
        $( fn $f(_: &mut Editor, _: &Args) -> CmdResult { not_yet($name) } )*
        /// Terminal buffer commands (looked up by `commands::lookup`).
        pub static COMMANDS: &[Command] = &[
            $( Command { name: $name, run: $f, flags: flags::NO_UNDO, doc: $doc }, )*
        ];
    };
}

stub_commands! {
    "term" => term, "Start a shell in a new terminal buffer (*term*).";
    "ansi-term" => ansi_term, "Start a shell in a new terminal buffer (same as term).";
    "term-char-mode" => term_char_mode, "Send keys to the terminal's program (leave copy mode).";
    "term-copy-mode" => term_copy_mode, "Copy mode: move, search and copy in the terminal's text.";
    "term-interrupt-subjob" => term_interrupt_subjob, "Send an interrupt (C-c) to the terminal's program.";
    "term-send-eof" => term_send_eof, "Send end-of-file (C-d) to the terminal's program.";
    "term-stop-subjob" => term_stop_subjob, "Send a stop (C-z) to the terminal's program.";
    "term-quit-subjob" => term_quit_subjob, "Send a quit (C-\\) to the terminal's program.";
    "term-paste" => term_paste, "Paste the last kill into the terminal (bracketed if the program asked).";
    "term-scroll-up" => term_scroll_up, "Scroll the terminal view back into the history.";
    "term-scroll-down" => term_scroll_down, "Scroll the terminal view forward toward the live screen.";
}

impl Editor {
    pub fn is_terminal(&self, buf: BufferId) -> bool {
        self.terminals.contains_key(&buf)
    }

    pub fn terminal(&self, buf: BufferId) -> Option<&TermState> {
        self.terminals.get(&buf)
    }

    /// What to draw for terminal buffer `buf`.
    pub fn terminal_snapshot(&self, buf: BufferId, palette: &Palette) -> Option<Snapshot> {
        self.terminals.get(&buf).map(|t| t.term.snapshot(palette))
    }

    /// Start `spec` in a new terminal buffer called `name` (made unique) and
    /// make it current. Returns the buffer id.
    pub fn open_terminal(&mut self, _name: &str, _spec: SpawnSpec) -> io::Result<BufferId> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "terminal buffers not implemented yet"))
    }

    /// Make a terminal buffer for a session that already runs in the backend
    /// (session restore). The buffer is added behind the current one.
    pub fn attach_terminal(&mut self, _name: &str, _session: SessionId, _spec: SpawnSpec) -> io::Result<BufferId> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "terminal buffers not implemented yet"))
    }

    /// Handle a key typed in the current buffer before the key dispatcher
    /// sees it. Returns true if it went to the terminal's program.
    pub fn terminal_key(&mut self, _key: &Key) -> bool {
        false
    }

    /// Paste `text` into the current terminal buffer. False if it isn't one.
    pub fn terminal_paste(&mut self, _text: &str) -> bool {
        false
    }

    /// The window showing `buf` is `cols`×`rows` cells. Resizes the terminal
    /// and the PTY when that changes.
    pub fn resize_terminal(&mut self, _buf: BufferId, _cols: u16, _rows: u16) {}

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
    /// arrived (the view should redraw).
    pub fn poll_pty(&mut self) -> bool {
        let Some(b) = self.pty.as_mut() else { return false };
        let mut any = false;
        while let Some(e) = b.try_recv() {
            any = true;
            match e {
                PtyEvent::Output(..) | PtyEvent::Exited(..) => {}
            }
        }
        any
    }
}
