//! Terminal buffers end to end against `pty::mock`: spawning, key routing,
//! output, replies, hooks, exit, copy mode, kill and resize.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use super::*;
use crate::engine::buffer::Buffer;
use crate::engine::config::{Config, TerminalConfig};
use crate::engine::hooks::{Event, EventKind};
use crate::engine::keymap::{Dispatcher, Outcome};
use crate::engine::pty::mock::{MockBackend, MockHandle};

/// An editor with a mock PTY backend, driven key by key like the view does.
struct T {
    ed: Editor,
    h: MockHandle,
    d: Dispatcher,
    /// Every non-command event, in order.
    events: Rc<RefCell<Vec<Event>>>,
    wakes: Arc<AtomicUsize>,
}

fn config(shell: &str) -> TerminalConfig {
    TerminalConfig { shell: Some(shell.into()), shell_args: vec!["-l".into()], ..Config::builtin().terminal }
}

fn t() -> T {
    let wakes = Arc::new(AtomicUsize::new(0));
    let w = wakes.clone();
    let waker: crate::engine::pty::Waker = Arc::new(move || {
        w.fetch_add(1, Ordering::SeqCst);
    });
    let (backend, h) = MockBackend::with_waker(waker.clone());
    let mut ed = Editor::new(Buffer::from_str("notes", "some text"));
    ed.set_waker(waker);
    ed.set_pty_backend(Box::new(backend));
    ed.set_terminal_config(Arc::new(config("/bin/test-shell")));
    let events = Rc::new(RefCell::new(Vec::new()));
    for kind in EventKind::ALL.into_iter().filter(|k| !matches!(k, EventKind::AfterCommand | EventKind::Timer)) {
        let log = events.clone();
        ed.hooks.add(
            kind,
            "record",
            Rc::new(move |_, e| {
                log.borrow_mut().push(e.clone());
                Ok(())
            }),
        );
    }
    ed.run_hooks();
    events.borrow_mut().clear();
    T { ed, h, d: Dispatcher::new(), events, wakes }
}

impl T {
    /// Like `YmacsView::handle_key`: char-mode terminals first, then the dispatcher.
    fn key(&mut self, k: &str) -> &mut Self {
        let key = Key::parse(k);
        if self.ed.picker.is_some() {
            return self;
        }
        if !self.d.is_pending() && self.ed.terminal_key(&key) {
            return self;
        }
        if let Outcome::Run(cmd, args) = self.d.dispatch(&key, &self.ed.active_keymaps()) {
            self.ed.execute(cmd, args);
        }
        self
    }

    fn keys(&mut self, keys: &str) -> &mut Self {
        for k in keys.split_whitespace() {
            self.key(k);
        }
        self
    }

    /// Start a terminal with `M-x term` (the command, as the picker would
    /// run it). Forgets the events that caused.
    fn term(&mut self) -> BufferId {
        self.ed.execute("term", Args::default());
        assert!(self.ed.is_terminal(self.ed.buf_id), "{:?}", self.ed.message);
        self.take_events();
        self.ed.buf_id
    }

    fn session(&self, buf: BufferId) -> SessionId {
        self.ed.terminal(buf).unwrap().session
    }

    fn input(&self, buf: BufferId) -> Vec<u8> {
        self.h.input(self.session(buf))
    }

    fn output(&mut self, buf: BufferId, bytes: &str) -> &mut Self {
        self.h.output(self.session(buf), bytes.as_bytes());
        self
    }

    fn tick(&mut self) -> bool {
        self.ed.tick(Instant::now())
    }

    fn rows(&self, buf: BufferId) -> Vec<String> {
        let s = self.ed.terminal_snapshot(buf, &Palette::xterm()).expect("char mode snapshot");
        (0..s.rows).map(|r| s.row_text(r)).collect()
    }

    fn take_events(&self) -> Vec<Event> {
        std::mem::take(&mut *self.events.borrow_mut())
    }
}

// ------------------------------------------------------------------ spawn

#[test]
fn term_spawns_the_shell_in_a_new_current_buffer() {
    let mut x = t();
    let dir = crate::engine::testutil::temp_dir("termcwd");
    let file = dir.join("f.txt");
    std::fs::write(&file, "x").unwrap();
    let id = x.ed.add_buffer(Buffer::open(file).unwrap());
    x.ed.switch_to_buffer(id);
    x.ed.run_hooks();
    x.take_events();

    x.ed.execute("term", Args::default());
    let term = x.ed.buf_id;
    assert_eq!(x.ed.buf.name, "*term*");
    assert_eq!(x.ed.buf.kind, BufferKind::Terminal);
    assert_eq!(x.ed.buf.rope.len_chars(), 0);
    let spec = x.h.session(x.session(term)).unwrap().spec;
    assert_eq!(spec.program, "/bin/test-shell");
    assert_eq!(spec.args, ["-l"]);
    assert_eq!(spec.cwd.as_deref(), Some(dir.as_path()));
    assert_eq!(
        spec.env,
        [("TERM".to_string(), "xterm-256color".to_string()), ("COLORTERM".into(), "truecolor".into())]
    );
    assert_eq!(spec.size, PtySize::new(80, 24));
    assert_eq!(x.ed.terminal(term).unwrap().term.size(), (80, 24));
    assert_eq!(x.ed.terminal(term).unwrap().spec, spec);
    assert_eq!(x.ed.terminal(term).unwrap().mode, InputMode::Char);
    assert_eq!(
        x.take_events(),
        [Event::BufferCreated { buf: term }, Event::WindowConfigurationChanged],
        "hooks ran after the command; the window now shows the terminal"
    );

    // Another one gets a unique name, the first one's size, and ansi-term is the same.
    x.ed.resize_terminal(term, 100, 30);
    x.ed.execute("ansi-term", Args::default());
    assert_eq!(x.ed.buf.name, "*term*<2>");
    let second = x.ed.buf_id;
    assert_eq!(x.ed.terminal(second).unwrap().spec.size, PtySize::new(100, 30));
    assert_ne!(x.session(term), x.session(second));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn term_without_a_file_runs_in_the_editors_directory() {
    let x = t();
    let spec = x.ed.shell_spec();
    assert_eq!(spec.cwd, std::env::current_dir().ok());
    assert_eq!(spec.size, PtySize::default());
}

#[test]
fn spawn_failure_is_reported() {
    let mut x = t();
    x.h.fail_next_spawn("no such shell");
    x.ed.execute("term", Args::default());
    assert_eq!(x.ed.message.as_deref(), Some("Can't start a terminal: no such shell"));
    assert_eq!(x.ed.buf.name, "notes");
    assert!(x.ed.terminals.is_empty());
    assert_eq!(x.ed.buffer_count(), 1);
}

#[test]
fn open_terminal_with_an_explicit_spec() {
    let mut x = t();
    let spec = SpawnSpec { program: "htop".into(), size: PtySize::new(120, 40), ..SpawnSpec::default() };
    let id = x.ed.open_terminal("*htop*", spec.clone()).unwrap();
    assert_eq!(x.ed.buf_id, id);
    assert_eq!(x.ed.buf.name, "*htop*");
    assert_eq!(x.ed.terminal(id).unwrap().term.size(), (120, 40));
    assert_eq!(x.h.session(x.session(id)).unwrap().spec, spec);
}

// ---------------------------------------------------------- key routing

#[test]
fn typed_keys_go_to_the_program() {
    let mut x = t();
    let term = x.term();
    x.keys("l s SPC - l RET");
    assert_eq!(x.input(term), b"ls -l\r");
    x.keys("C-a C-e C-f C-b C-k C-y C-w C-g M-f M-b M-DEL C-SPC TAB S-TAB <up> <f5> DEL ESC");
    let want: &[u8] = b"\x01\x05\x06\x02\x0b\x19\x17\x07\x1bf\x1bb\x1b\x7f\x00\t\x1b[Z\x1b[A\x1b[15~\x7f\x1b";
    assert_eq!(&x.input(term)[6..], want);
    assert_eq!(x.ed.buf.rope.len_chars(), 0, "nothing was inserted into the buffer");
}

#[test]
fn application_cursor_mode_changes_arrow_keys() {
    let mut x = t();
    let term = x.term();
    x.output(term, "\x1b[?1h").tick();
    x.keys("<up> <down>");
    assert_eq!(x.input(term), b"\x1bOA\x1bOB");
}

#[test]
fn escape_keys_reach_the_editor() {
    let mut x = t();
    let term = x.term();
    // C-x b opens the buffer picker.
    x.keys("C-x b");
    assert!(x.ed.picker.is_some());
    x.ed.picker = None;
    // M-x opens the command picker.
    x.keys("M-x");
    assert!(x.ed.picker.is_some());
    x.ed.picker = None;
    assert!(x.input(term).is_empty(), "no escape key leaked to the program");
}

#[test]
fn global_tab_keys_switch_tabs_from_a_terminal() {
    let mut x = t();
    let term = x.term();
    x.ed.execute("tab-new", Args::default());
    x.ed.switch_to_buffer(term);
    assert_eq!(x.ed.layout.current_tab, 1);
    x.keys("C-TAB");
    assert_eq!(x.ed.layout.current_tab, 0, "C-TAB is tab-next, not a TAB for the program");
    x.ed.switch_to_buffer(term);
    x.keys("C-S-TAB");
    assert_eq!(x.ed.layout.current_tab, 1);
    assert!(x.input(term).is_empty(), "nothing reached the program: {:?}", x.input(term));
    // Plain TAB and S-TAB are still the program's.
    x.ed.switch_to_buffer(term);
    x.keys("TAB S-TAB");
    assert_eq!(x.input(term), b"\t\x1b[Z");
}

#[test]
fn terminal_keymap_commands() {
    let mut x = t();
    let term = x.term();
    x.keys("C-c C-c");
    assert_eq!(x.input(term), [0x03]);
    x.keys("C-c C-d C-c C-z C-c C-\\");
    assert_eq!(x.input(term), [0x03, 0x04, 0x1a, 0x1c]);
    // An unbound C-c sequence is undefined, and nothing is sent.
    x.keys("C-c a");
    assert_eq!(x.input(term), [0x03, 0x04, 0x1a, 0x1c]);
    // The next key is the program's again.
    x.keys("a");
    assert_eq!(x.input(term), [0x03, 0x04, 0x1a, 0x1c, b'a']);
}

#[test]
fn super_chords_stay_with_the_editor() {
    let mut x = t();
    let term = x.term();
    assert!(!x.ed.terminal_key(&Key::parse("s-c")));
    assert!(!x.ed.terminal_key(&Key::parse("s-<up>")));
    assert!(x.input(term).is_empty());
}

#[test]
fn custom_escape_keys() {
    let mut x = t();
    let cfg = TerminalConfig { escape_keys: vec!["C-z".into()], ..config("/bin/sh") };
    x.ed.set_terminal_config(Arc::new(cfg));
    let term = x.term();
    assert!(!x.ed.terminal_key(&Key::parse("C-z")));
    // C-x is no longer an escape key; C-c still is a terminal keymap prefix.
    assert!(x.ed.terminal_key(&Key::parse("C-x")));
    assert!(!x.ed.terminal_key(&Key::parse("C-c")));
    assert_eq!(x.input(term), [0x18]);
}

#[test]
fn keys_in_other_buffers_are_not_terminal_keys() {
    let mut x = t();
    assert!(!x.ed.terminal_key(&Key::parse("a")));
    let term = x.term();
    x.ed.switch_to_buffer(1);
    assert!(!x.ed.terminal_key(&Key::parse("a")));
    x.keys("x");
    assert_eq!(x.ed.buf.rope.to_string(), "xsome text");
    assert!(x.input(term).is_empty());
}

#[test]
fn unencodable_keys_go_to_the_dispatcher() {
    let mut x = t();
    let term = x.term();
    assert!(!x.ed.terminal_key(&Key::parse("<f13>")));
    assert!(x.input(term).is_empty());
}

#[test]
fn typing_scrolls_back_to_the_live_screen() {
    let mut x = t();
    let term = x.term();
    x.ed.resize_terminal(term, 20, 3);
    for i in 0..10 {
        x.output(term, &format!("{i}\r\n"));
    }
    x.tick();
    x.keys("S-<prior>");
    assert!(x.ed.terminal(term).unwrap().term.display_offset() > 0);
    x.keys("a");
    assert_eq!(x.ed.terminal(term).unwrap().term.display_offset(), 0);
    assert_eq!(x.input(term), b"a");
}

#[test]
fn scrolling_commands() {
    let mut x = t();
    let term = x.term();
    x.ed.resize_terminal(term, 20, 5);
    for i in 0..40 {
        x.output(term, &format!("{i}\r\n"));
    }
    x.tick();
    let offset = |x: &T| x.ed.terminal(term).unwrap().term.display_offset();
    x.keys("S-<prior>");
    assert_eq!(offset(&x), 3, "a page is the screen less two lines");
    x.keys("S-<prior>");
    assert_eq!(offset(&x), 6);
    x.keys("S-<next>");
    assert_eq!(offset(&x), 3);
    x.ed.execute("term-scroll-up", Args::with_raw(Some(RawArg::Number(10))));
    assert_eq!(offset(&x), 13);
    x.ed.execute("term-scroll-down", Args::with_raw(Some(RawArg::Number(100))));
    assert_eq!(offset(&x), 0);
    assert!(x.input(term).is_empty());
    // A scrolled view shows history.
    x.ed.execute("term-scroll-up", Args::with_raw(Some(RawArg::Number(2))));
    assert_eq!(x.rows(term)[0], "34");
}

use crate::engine::editor::RawArg;

#[test]
fn commands_outside_terminal_buffers_fail() {
    let mut x = t();
    for cmd in [
        "term-char-mode",
        "term-copy-mode",
        "term-interrupt-subjob",
        "term-send-eof",
        "term-stop-subjob",
        "term-quit-subjob",
        "term-scroll-up",
        "term-scroll-down",
    ] {
        x.ed.execute(cmd, Args::default());
        assert_eq!(x.ed.message.as_deref(), Some(format!("{cmd}: not a terminal buffer").as_str()));
    }
    x.ed.kill_ring.push("text".into());
    x.ed.execute("term-paste", Args::default());
    assert_eq!(x.ed.message.as_deref(), Some("term-paste: not a terminal buffer"));
    assert_eq!(x.ed.buf.rope.to_string(), "some text");
}

// ------------------------------------------------------------------ paste

#[test]
fn paste_from_the_kill_ring() {
    let mut x = t();
    let term = x.term();
    x.ed.execute("term-paste", Args::default());
    assert_eq!(x.ed.message.as_deref(), Some("Kill ring is empty"));
    x.ed.kill_ring.push("old".into());
    x.ed.clipboard_in("echo hi\n".into());
    x.keys("s-v");
    assert_eq!(x.input(term), b"echo hi\r");
    x.output(term, "\x1b[?2004h").tick();
    x.keys("C-c C-y");
    assert_eq!(x.input(term), b"echo hi\r\x1b[200~echo hi\r\x1b[201~");
}

#[test]
fn terminal_paste_api() {
    let mut x = t();
    assert!(!x.ed.terminal_paste("x"));
    let term = x.term();
    assert!(x.ed.terminal_paste("multi\nline"));
    assert_eq!(x.input(term), b"multi\rline");
    x.h.exit(x.session(term), ExitStatus::default());
    x.tick();
    assert!(!x.ed.terminal_paste("late"));
}

// --------------------------------------------------- output, replies, hooks

#[test]
fn output_reaches_the_snapshot() {
    let mut x = t();
    let term = x.term();
    x.ed.resize_terminal(term, 20, 3);
    assert!(!x.tick(), "nothing to do");
    x.output(term, "$ echo hi\r\n").output(term, "\x1b[32mhi\x1b[0m\r\n$ ");
    assert!(x.tick());
    assert_eq!(x.rows(term), ["$ echo hi", "hi", "$"]);
    let s = x.ed.terminal_snapshot(term, &Palette::xterm()).unwrap();
    assert_eq!(s.lines[1][0].fg, Palette::xterm().ansi[2]);
    assert_eq!((s.cursor.row, s.cursor.col), (2, 2));
    assert_eq!(x.take_events(), [Event::TerminalOutput { buf: term }], "one output event per buffer per poll");
}

#[test]
fn output_events_per_buffer() {
    let mut x = t();
    let a = x.term();
    let b = x.term();
    x.output(a, "1").output(b, "2").output(a, "3");
    x.tick();
    assert_eq!(x.take_events(), [Event::TerminalOutput { buf: a }, Event::TerminalOutput { buf: b }]);
    assert_eq!(x.rows(a)[0], "13");
    assert_eq!(x.rows(b)[0], "2");
}

#[test]
fn replies_are_written_back() {
    let mut x = t();
    let term = x.term();
    x.output(term, "abc\x1b[6n\x1b[c").tick();
    assert_eq!(x.input(term), b"\x1b[1;4R\x1b[?6c");
}

#[test]
fn title_and_bell_hooks() {
    let mut x = t();
    let term = x.term();
    x.output(term, "\x1b]2;vim main.rs\x07ding\x07");
    x.tick();
    assert_eq!(
        x.take_events(),
        [
            Event::TerminalOutput { buf: term },
            Event::TerminalTitle { buf: term, title: "vim main.rs".into() },
            Event::TerminalBell { buf: term },
        ]
    );
    assert_eq!(x.ed.terminal(term).unwrap().term.title(), "vim main.rs");
    // Events are plain data for scripts.
    let json = Event::TerminalTitle { buf: term, title: "t".into() }.to_json();
    assert_eq!(json["event"], "terminal-title");
}

#[test]
fn events_for_unknown_sessions_are_ignored() {
    let mut x = t();
    let term = x.term();
    let b = SpawnSpec { program: "other".into(), ..Default::default() };
    // A session the editor has no buffer for (spawned directly on the backend).
    let stray = x.ed.pty_backend().unwrap().spawn(b).unwrap();
    x.h.output(stray, b"stray");
    x.h.exit(stray, ExitStatus::default());
    assert!(x.tick(), "the events were read");
    assert!(x.take_events().is_empty());
    assert_eq!(x.rows(term)[0], "");
    assert_eq!(x.ed.terminal(term).unwrap().exited, None);
}

#[test]
fn poll_without_a_backend_does_nothing() {
    let mut ed = Editor::new(Buffer::from_str("x", ""));
    assert!(!ed.poll_pty());
}

#[test]
fn polling_is_bounded_and_wakes_for_more() {
    let mut x = t();
    let term = x.term();
    let chunk = "x".repeat(1 << 20);
    for _ in 0..10 {
        x.output(term, &chunk);
    }
    let wakes = x.wakes.load(Ordering::SeqCst);
    assert!(x.ed.poll_pty());
    assert_eq!(x.wakes.load(Ordering::SeqCst), wakes + 1, "woke the view for another round");
    assert!(x.ed.poll_pty());
    assert!(x.ed.poll_pty());
    assert!(!x.ed.poll_pty(), "drained in three rounds of 4 MiB");
}

// ------------------------------------------------------------------- exit

#[test]
fn process_exit() {
    let mut x = t();
    let term = x.term();
    x.output(term, "bye\r\n");
    x.h.exit(x.session(term), ExitStatus { code: 0, signal: None });
    x.tick();
    assert_eq!(x.ed.terminal(term).unwrap().exited, Some(ExitStatus::default()));
    assert_eq!(x.ed.message.as_deref(), Some("Process *term* exited"));
    assert_eq!(
        x.take_events(),
        [Event::TerminalOutput { buf: term }, Event::ProcessExited { buf: term, status: ExitStatus::default() }]
    );
    // The buffer stays with its output; typed keys do nothing...
    assert_eq!(x.ed.buf_id, term);
    assert_eq!(x.rows(term)[0], "bye");
    assert!(x.ed.terminal_key(&Key::parse("a")));
    x.ed.execute("term-interrupt-subjob", Args::default());
    assert_eq!(x.ed.message.as_deref(), Some("term-interrupt-subjob: the process has exited"));
    // ... but the escape keys still work.
    x.keys("C-x b");
    assert!(x.ed.picker.is_some());
    // Resizing an exited terminal doesn't touch the session.
    let resizes = x.h.session(x.session(term)).unwrap().resizes.len();
    x.ed.resize_terminal(term, 50, 10);
    assert_eq!(x.ed.terminal(term).unwrap().term.size(), (50, 10));
    assert_eq!(x.h.session(x.session(term)).unwrap().resizes.len(), resizes);
}

#[test]
fn exit_messages() {
    let mut x = t();
    let term = x.term();
    x.h.exit(x.session(term), ExitStatus { code: 3, signal: None });
    x.tick();
    assert_eq!(x.ed.message.as_deref(), Some("Process *term* exited with code 3"));
    let second = x.term();
    x.h.exit(x.session(second), ExitStatus { code: 1, signal: Some("Hangup".into()) });
    x.tick();
    assert_eq!(x.ed.message.as_deref(), Some("Process *term*<2> exited (killed by signal Hangup)"));
}

// -------------------------------------------------------------- copy mode

#[test]
fn copy_mode_round_trip() {
    let mut x = t();
    let term = x.term();
    x.ed.resize_terminal(term, 20, 3);
    x.output(term, "one\r\ntwo\r\nthree\r\n$ ls").tick();

    x.keys("C-c C-j");
    assert_eq!(x.ed.terminal(term).unwrap().mode, InputMode::Copy);
    assert_eq!(x.ed.buf.rope.to_string(), "one\ntwo\nthree\n$ ls");
    assert_eq!(x.ed.cursors[0].point, "one\ntwo\nthree\n$ ls".chars().count(), "point at the terminal cursor");
    assert!(x.ed.minor_on(MinorMode::ReadOnly));
    assert!(x.ed.terminal_snapshot(term, &Palette::xterm()).is_none(), "the view draws the text");
    assert!(!x.ed.buf.modified());

    // Keys are editor keys now: motion works, edits are refused, nothing is sent.
    x.keys("M-< C-n C-f");
    assert_eq!(x.ed.cursors[0].point, 5);
    x.keys("x");
    assert_eq!(x.ed.buf.rope.to_string(), "one\ntwo\nthree\n$ ls");
    assert!(x.input(term).is_empty());
    // Copying works.
    x.keys("C-SPC C-e M-w");
    assert_eq!(x.ed.kill_ring.last().map(String::as_str), Some("wo"));

    // Output meanwhile keeps updating the terminal.
    x.output(term, " -l\r\nfile\r\n$ ").tick();
    assert_eq!(x.ed.buf.rope.to_string(), "one\ntwo\nthree\n$ ls", "the copy isn't refreshed live");

    x.keys("C-c C-k");
    assert_eq!(x.ed.terminal(term).unwrap().mode, InputMode::Char);
    assert_eq!(x.ed.buf.rope.len_chars(), 0);
    assert!(!x.ed.minor_on(MinorMode::ReadOnly));
    assert_eq!(x.rows(term), ["$ ls -l", "file", "$"]);
    x.keys("a");
    assert_eq!(x.input(term), b"a");

    // Entering again shows the new text; undo has nothing stale to apply.
    x.keys("C-c C-j");
    assert!(x.ed.buf.rope.to_string().ends_with("$ ls -l\nfile\n$"));
    x.keys("C-c C-k C-x u");
    assert_eq!(x.ed.buf.rope.len_chars(), 0);
}

#[test]
fn copy_mode_survives_switching_buffers() {
    let mut x = t();
    let term = x.term();
    x.output(term, "hello").tick();
    x.keys("C-c C-j");
    x.ed.switch_to_buffer(1);
    assert_eq!(x.ed.buf.rope.to_string(), "some text");
    x.ed.switch_to_buffer(term);
    assert_eq!(x.ed.buf.rope.to_string(), "hello");
    assert!(!x.ed.terminal_key(&Key::parse("a")), "copy mode: keys are the editor's");
    x.keys("C-c C-k");
    assert!(x.ed.terminal_key(&Key::parse("a")));
}

// ------------------------------------------------------------------- kill

#[test]
fn killing_the_buffer_kills_the_session() {
    let mut x = t();
    let term = x.term();
    let session = x.session(term);
    x.take_events();
    x.ed.kill_buffer(term);
    assert!(!x.ed.is_terminal(term));
    assert!(x.ed.terminal(term).is_none());
    assert!(x.h.session(session).is_none(), "the backend forgot the session");
    assert_eq!(x.ed.buf.name, "notes");
    // Late events from the dead session are ignored.
    x.h.output(session, b"late");
    x.tick();
    // The window switched away from the killed buffer before it went.
    assert_eq!(
        x.take_events(),
        [Event::WindowConfigurationChanged, Event::BufferKilled { buf: term, name: "*term*".into() }]
    );
}

// ----------------------------------------------------------------- resize

#[test]
fn resize_is_deduplicated() {
    let mut x = t();
    let term = x.term();
    let sizes = |x: &T| x.h.session(x.session(term)).unwrap().resizes;
    x.ed.resize_terminal(term, 80, 24);
    assert!(sizes(&x).is_empty(), "already that size");
    x.ed.resize_terminal(term, 100, 30);
    x.ed.resize_terminal(term, 100, 30);
    assert_eq!(sizes(&x), [PtySize::new(100, 30)]);
    assert_eq!(x.ed.terminal(term).unwrap().spec.size, PtySize::new(100, 30));
    assert_eq!(x.ed.terminal(term).unwrap().term.size(), (100, 30));
    // Tiny sizes are clamped, and the clamped size is what the PTY gets.
    x.ed.resize_terminal(term, 0, 0);
    x.ed.resize_terminal(term, 1, 1);
    assert_eq!(sizes(&x), [PtySize::new(100, 30), PtySize::new(2, 1)]);
    // Unknown buffers are ignored.
    x.ed.resize_terminal(999, 10, 10);
    x.ed.resize_terminal(1, 10, 10);
}

// ---------------------------------------------------------------- restore

#[test]
fn attach_terminal_replays_into_a_background_buffer() {
    let mut x = t();
    let spec = SpawnSpec { program: "/bin/zsh".into(), size: PtySize::new(30, 4), ..SpawnSpec::default() };
    let session = x.h.add_detached(spec.clone(), b"earlier output\r\n$ ");
    let id = x.ed.attach_terminal("*term*", session, spec).unwrap();
    assert_eq!(x.ed.buf.name, "notes", "added behind the current buffer");
    assert_eq!(x.ed.others[0].id, id);
    assert!(x.ed.is_terminal(id));
    assert_eq!(x.h.session(session).unwrap().resizes, [PtySize::new(30, 4)]);
    x.tick();
    assert_eq!(x.rows(id), ["earlier output", "$", "", ""]);
    // Missing sessions fail and add nothing.
    assert!(x.ed.attach_terminal("*term*", SessionId(999), SpawnSpec::default()).is_err());
    assert_eq!(x.ed.buffer_count(), 2);
}

#[test]
fn attach_terminal_of_an_exited_session() {
    let mut x = t();
    let session = x.h.add_detached(SpawnSpec::default(), b"done\r\n");
    x.h.exit(session, ExitStatus { code: 0, signal: None });
    let id = x.ed.attach_terminal("*old*", session, SpawnSpec::default()).unwrap();
    x.tick();
    assert_eq!(x.rows(id)[0], "done");
    assert!(x.ed.terminal(id).unwrap().exited.is_some());
}

#[test]
fn one_window_sizes_each_shared_terminal() {
    let mut x = t();
    let a = x.term();
    let b = x.term();
    let notes = 1;
    assert_eq!(x.ed.buffer_name(notes), Some("notes"));
    // Three windows: b (selected), b, then notes; then a in a fourth.
    x.keys("C-x 2 C-x 2");
    let order = x.ed.layout.window_order();
    assert_eq!(order.len(), 3);
    x.ed.select_window(order[2]);
    x.ed.switch_to_buffer(notes);
    x.ed.select_window(order[1]);
    let owners = x.ed.terminal_owners(&order);
    assert_eq!(owners.len(), 1, "notes is no terminal: {owners:?}");
    assert_eq!(owners[&b], order[1], "the selected window, though another shows it first");
    // Not selected: the first window (in order) showing it.
    x.ed.select_window(order[2]);
    assert_eq!(x.ed.terminal_owners(&order)[&b], order[0]);
    // Each terminal has its own owner.
    x.ed.switch_to_buffer(a);
    let owners = x.ed.terminal_owners(&order);
    assert_eq!((owners[&a], owners[&b]), (order[2], order[0]));
    // Only windows on screen count (plus the selected one).
    assert_eq!(x.ed.terminal_owners(&[]).keys().copied().collect::<Vec<_>>(), [a]);
}

#[test]
fn grid_size_counts_whole_cells() {
    use crate::engine::term::grid_size;
    assert_eq!(grid_size(800.0, 480.0, 8.0, 20.0), (100, 24));
    assert_eq!(grid_size(807.9, 499.0, 8.0, 20.0), (100, 24), "partial cells don't count");
    assert_eq!(grid_size(3.0, 3.0, 8.0, 20.0), (1, 1), "at least one cell");
    assert_eq!(grid_size(-50.0, 0.0, 8.0, 20.0), (1, 1));
    assert_eq!(grid_size(100.0, 100.0, 0.0, 0.0), (100, 100), "cells are at least a pixel");
    assert_eq!(grid_size(f32::INFINITY, f32::NAN, 8.0, 20.0), (1, 1));
    assert_eq!(grid_size(1e9, 1e9, 1.0, 1.0), (u16::MAX, u16::MAX));
}

#[test]
fn an_unreachable_daemon_falls_back_to_local_processes() {
    let dir = crate::engine::testutil::temp_dir("fallback");
    let not_a_dir = dir.join("file");
    std::fs::write(&not_a_dir, "x").unwrap();
    let mut ed = Editor::new(Buffer::from_str("notes", ""));
    let cfg = TerminalConfig {
        backend: crate::engine::config::PtyBackendKind::Daemon,
        socket: Some(not_a_dir.join("d.sock")),
        ..config("/bin/sh")
    };
    ed.set_terminal_config(Arc::new(cfg));
    let b = ed.pty_backend().expect("a backend all the same");
    assert!(!b.persistent(), "processes run locally");
    let msg = ed.message.clone().unwrap_or_default();
    assert!(msg.starts_with("Session daemon unavailable") && msg.contains("end with the editor"), "{msg}");
    // Terminals work.
    ed.execute("term", Args::default());
    assert!(ed.is_terminal(ed.buf_id), "{:?}", ed.message);
    let buf = ed.buf_id;
    ed.kill_buffer(buf);
    std::fs::remove_dir_all(dir).unwrap();
}
