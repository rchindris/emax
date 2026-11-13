//! End-to-end tests across the terminal substrate with real processes:
//! terminal buffers on `LocalBackend` running `/bin/sh` driven key by key the
//! way the view does it, two terminals in split windows, a session saved and
//! restored into a fresh editor that reattaches through an in-process daemon,
//! and the order hooks see events in.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::buffer::{Buffer, BufferId};
use super::config::{Config, PtyBackendKind, TerminalConfig};
use super::editor::{Args, Editor};
use super::hooks::{Event, EventKind};
use super::keymap::{Dispatcher, Key, Outcome};
use super::pty::local::LocalBackend;
use super::pty::{PtyBackend, Waker};
use super::session::client::DaemonBackend;
use super::session::daemon::{DaemonOptions, bind, serve};
use super::session::{self, SavedBufferKind};
use super::term::buffer::InputMode;
use super::testutil::{TIMEOUT, temp_dir, wait_until};
use super::window::{Rect, compute_rects};

// ---------------------------------------------------------------- harness

/// An editor driven like `YmacsView`: keys go to a char-mode terminal first,
/// then to the dispatcher; `tick` is what the waker leads to.
struct E {
    ed: Editor,
    d: Dispatcher,
    wakes: Arc<AtomicUsize>,
}

fn sh_config(backend: PtyBackendKind, socket: Option<PathBuf>, args: &[&str]) -> TerminalConfig {
    TerminalConfig {
        shell: Some("/bin/sh".into()),
        shell_args: args.iter().map(|s| s.to_string()).collect(),
        backend,
        socket,
        ..Config::builtin().terminal
    }
}

impl E {
    fn new(cfg: TerminalConfig) -> Self {
        let wakes = Arc::new(AtomicUsize::new(0));
        let w = wakes.clone();
        let mut ed = Editor::new(Buffer::from_str("notes", "some text"));
        ed.set_waker(Arc::new(move || {
            w.fetch_add(1, Ordering::SeqCst);
        }));
        ed.set_terminal_config(Arc::new(cfg));
        ed.run_hooks();
        Self { ed, d: Dispatcher::new(), wakes }
    }

    fn key(&mut self, k: Key) {
        if !self.d.is_pending() && self.ed.terminal_key(&k) {
            return;
        }
        if let Outcome::Run(cmd, args) = self.d.dispatch(&k, &self.ed.active_keymaps()) {
            self.ed.execute(cmd, args);
        }
    }

    /// Key sequence in Emacs notation (`C-x 2`).
    fn keys(&mut self, keys: &str) {
        for k in keys.split_whitespace() {
            self.key(Key::parse(k));
        }
    }

    /// Type `line` character by character, then RET.
    fn type_line(&mut self, line: &str) {
        for c in line.chars() {
            self.key(Key::parse(&if c == ' ' { "SPC".to_string() } else { c.to_string() }));
        }
        self.key(Key::parse("RET"));
    }

    /// The terminal's text (history and screen).
    fn text(&self, buf: BufferId) -> String {
        self.ed.terminal(buf).expect("a terminal buffer").term.text().0
    }

    /// Tick until `buf`'s text has a line equal to `line` (the command's
    /// output, not the echo of the command line that produced it).
    fn wait_for_line(&mut self, buf: BufferId, line: &str) {
        wait_until(&format!("a line {line:?} in buffer {buf}"), TIMEOUT, || {
            self.ed.tick(Instant::now());
            self.text(buf).lines().any(|l| l.trim_end() == line)
        });
    }

    fn wait_exit(&mut self, buf: BufferId) {
        wait_until("the process to exit", TIMEOUT, || {
            self.ed.tick(Instant::now());
            self.ed.terminal(buf).is_some_and(|t| t.exited.is_some())
        });
    }
}

/// An in-process session daemon running real processes on a temp socket.
struct Daemon {
    dir: PathBuf,
    socket: PathBuf,
    thread: Option<JoinHandle<std::io::Result<()>>>,
}

impl Daemon {
    fn start() -> Self {
        let dir = temp_dir("e2e");
        let socket = dir.join("d.sock");
        let bound = bind(&socket).expect("bind");
        let opts = DaemonOptions { socket: socket.clone(), replay_bytes: 1 << 20, idle_exit: None };
        let thread = thread::spawn(move || {
            serve(bound, &opts, |w: Waker| Box::new(LocalBackend::new(w)) as Box<dyn PtyBackend>)
        });
        Self { dir, socket, thread: Some(thread) }
    }

    fn live_sessions(&self) -> usize {
        let mut c = DaemonBackend::connect(&self.socket, super::pty::noop_waker()).expect("connect");
        c.list().expect("list").iter().filter(|s| s.exited.is_none()).count()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Ok(mut c) = DaemonBackend::connect(&self.socket, super::pty::noop_waker()) {
            let _ = c.shutdown();
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// ------------------------------------------------------------------ tests

#[test]
fn a_shell_in_a_terminal_buffer_runs_typed_commands() {
    let mut x = E::new(sh_config(PtyBackendKind::Local, None, &[]));
    x.ed.execute("term", Args::default());
    let buf = x.ed.buf_id;
    assert_eq!(x.ed.buf.name, "*term*");
    assert!(x.ed.pty_backend_if_running().is_some_and(|b| !b.persistent()));

    // The arithmetic only appears expanded in the output, never in the echo.
    x.type_line("echo out-$((6*7))");
    x.wait_for_line(buf, "out-42");
    assert!(x.wakes.load(Ordering::SeqCst) > 0, "the backend woke the view");

    // The snapshot (what the view draws) shows it too.
    let snap = x.ed.terminal_snapshot(buf, &super::term::Palette::xterm()).unwrap();
    assert!((0..snap.rows).any(|r| snap.row_text(r).trim_end() == "out-42"));

    // C-x is an escape key: C-x b reaches the editor, not the shell.
    x.keys("C-x 2");
    assert_eq!(x.ed.layout.window_order().len(), 2);

    // Copy mode gives the text to the editor; char mode hands keys back.
    x.keys("C-c C-j");
    assert_eq!(x.ed.terminal(buf).unwrap().mode, InputMode::Copy);
    assert!(x.ed.buf.rope.to_string().lines().any(|l| l == "out-42"));
    x.keys("C-c C-k");
    assert_eq!(x.ed.terminal(buf).unwrap().mode, InputMode::Char);

    // The process ends: the buffer stays and says so.
    x.type_line("exit 7");
    x.wait_exit(buf);
    assert_eq!(x.ed.terminal(buf).unwrap().exited.as_ref().unwrap().code, 7);
    assert_eq!(x.ed.message.as_deref(), Some("Process *term* exited with code 7"));
}

#[test]
fn bracketed_paste_and_interrupt_reach_the_program() {
    let mut x = E::new(sh_config(PtyBackendKind::Local, None, &[]));
    x.ed.execute("term", Args::default());
    let buf = x.ed.buf_id;
    // `cat` echoes what it reads; C-c C-c interrupts it and the shell is back.
    // Until the shell has started it does not ignore SIGINT itself.
    x.type_line("echo ready-$((0+1))");
    x.wait_for_line(buf, "ready-1");
    x.type_line("cat");
    x.ed.terminal_paste("pasted-line\n");
    // Once from the terminal's echo, once from cat.
    wait_until("cat to echo the paste", TIMEOUT, || {
        x.ed.tick(Instant::now());
        x.text(buf).lines().filter(|l| l.trim_end() == "pasted-line").count() == 2
    });
    x.keys("C-c C-c");
    x.type_line("echo back-$((1+1))");
    x.wait_for_line(buf, "back-2");
    assert!(x.ed.terminal(buf).unwrap().exited.is_none(), "the shell survived the interrupt");
}

#[test]
fn two_terminals_in_split_windows_are_independent_and_sized_by_their_windows() {
    let mut x = E::new(sh_config(PtyBackendKind::Local, None, &[]));
    x.ed.execute("term", Args::default());
    let a = x.ed.buf_id;
    x.keys("C-x 3"); // side by side; the left window stays selected
    x.keys("C-x o");
    x.ed.execute("term", Args::default());
    let b = x.ed.buf_id;
    assert_ne!(a, b);
    assert_eq!(x.ed.buf.name, "*term*<2>");

    let order = x.ed.layout.window_order();
    assert_eq!(order.len(), 2);
    let shown: Vec<BufferId> = order.iter().map(|w| x.ed.layout.window(*w).unwrap().buffer).collect();
    assert_eq!(shown, [a, b]);

    // Size each terminal to its window, as the view does every render.
    let rects = compute_rects(&x.ed.layout.tab().root, Rect { x: 0.0, y: 0.0, w: 120.0, h: 40.0 });
    for (win, r) in &rects {
        let buf = x.ed.layout.window(*win).unwrap().buffer;
        x.ed.resize_terminal(buf, r.w as u16, r.h as u16 - 1);
    }
    let size_of = |x: &E, buf| x.ed.terminal(buf).unwrap().term.size();
    assert_eq!(size_of(&x, a), (60, 39));
    assert_eq!(size_of(&x, b), (60, 39));

    // Typing goes to the selected window's terminal only.
    x.type_line("echo bee-$((2*3)); stty size");
    x.wait_for_line(b, "bee-6");
    x.wait_for_line(b, "39 60");
    assert!(!x.text(a).contains("bee-6"));

    x.keys("C-x o");
    assert_eq!(x.ed.buf_id, a);
    x.type_line("echo ay-$((3*3))");
    x.wait_for_line(a, "ay-9");
    assert!(!x.text(b).contains("ay-9"));

    // Splitting the right window below makes it shorter; the PTY follows.
    x.keys("C-x o C-x 2");
    let rects = compute_rects(&x.ed.layout.tab().root, Rect { x: 0.0, y: 0.0, w: 120.0, h: 40.0 });
    let (_, r) = rects.iter().find(|(w, _)| *w == x.ed.layout.selected()).unwrap();
    x.ed.resize_terminal(b, r.w as u16, r.h as u16 - 1);
    x.type_line("stty size");
    x.wait_for_line(b, "19 60");

    // Killing a terminal buffer ends its process; the other one lives on.
    x.ed.kill_buffer(b);
    assert!(!x.ed.is_terminal(b));
    x.ed.switch_to_buffer(a);
    x.type_line("echo still-$((4+4))");
    x.wait_for_line(a, "still-8");
    x.ed.layout.check().expect("layout invariants");
}

#[test]
fn sessions_are_restored_into_a_new_editor_and_reattach_through_the_daemon() {
    let daemon = Daemon::start();
    let state = temp_dir("e2e-state");
    let file = session::session_file_path(&state);
    let cfg = || sh_config(PtyBackendKind::Daemon, Some(daemon.socket.clone()), &[]);

    // First editor: a terminal and a text buffer in a split, in a second tab.
    let (term_name, session_id) = {
        let mut x = E::new(cfg());
        x.keys("C-x t 2");
        x.ed.execute("term", Args::default());
        let buf = x.ed.buf_id;
        assert!(x.ed.pty_backend_if_running().is_some_and(|b| b.persistent()), "the daemon backend");
        x.type_line("echo first-$((1+1))");
        x.wait_for_line(buf, "first-2");
        x.keys("C-x 2");
        x.ed.save_session_on_quit(file.clone());
        x.ed.execute("save-buffers-kill-terminal", Args::default());
        assert!(x.ed.quit_requested);
        let id = x.ed.terminal(buf).unwrap().session;
        (x.ed.buf.name.clone(), id)
        // The editor and its daemon connection go away here.
    };
    assert_eq!(term_name, "*term*");
    wait_until("the client to detach", TIMEOUT, || daemon.live_sessions() == 1);

    let saved = session::load(&file).expect("the session file was written");
    assert_eq!(saved.tabs.len(), 2);
    assert!(
        saved
            .buffers
            .iter()
            .any(|b| matches!(b.kind, SavedBufferKind::Terminal { session, .. } if session == session_id))
    );

    // The process lives on while no editor is attached.
    {
        let mut c = DaemonBackend::connect(&daemon.socket, super::pty::noop_waker()).unwrap();
        let s = c.list().unwrap();
        assert!(s.iter().any(|s| s.id == session_id && s.exited.is_none() && s.pid.is_some()));
    }

    // Second editor: restore, see the replayed output, then live output.
    let mut y = E::new(cfg());
    assert!(y.ed.restore_session_file(&file).expect("restore"));
    assert_eq!(y.ed.layout.tabs.len(), 2);
    assert_eq!(y.ed.layout.current_tab, 1);
    assert_eq!(y.ed.layout.window_order().len(), 2, "the split came back");
    let buf = y.ed.buf_id;
    assert_eq!(y.ed.buf.name, "*term*");
    assert!(y.ed.is_terminal(buf));
    y.wait_for_line(buf, "first-2");
    y.type_line("echo second-$((2+2))");
    y.wait_for_line(buf, "second-4");

    // Both windows of the split show the same terminal.
    for w in y.ed.layout.window_order() {
        assert_eq!(y.ed.layout.window(w).unwrap().buffer, buf);
    }
    y.ed.layout.check().expect("layout invariants");

    // Killing the buffer ends the daemon session for good.
    y.ed.kill_buffer(buf);
    wait_until("the session to end", TIMEOUT, || daemon.live_sessions() == 0);
    let _ = std::fs::remove_dir_all(state);
}

/// What the view does: restore the session *before* it exists (main.rs),
/// install its waker afterwards, and from then on tick only when woken.
/// Output the reattach replayed while no waker was installed must not leave
/// the backend thinking a wake-up is still pending.
#[test]
fn a_session_restored_before_the_view_exists_still_wakes_it() {
    let daemon = Daemon::start();
    let state = temp_dir("e2e-wake");
    let file = session::session_file_path(&state);
    let cfg = || sh_config(PtyBackendKind::Daemon, Some(daemon.socket.clone()), &[]);
    {
        let mut x = E::new(cfg());
        x.ed.execute("term", Args::default());
        let buf = x.ed.buf_id;
        x.type_line("echo before-$((1+1))");
        x.wait_for_line(buf, "before-2");
        x.ed.save_session_on_quit(file.clone());
        x.ed.execute("save-buffers-kill-terminal", Args::default());
    }
    wait_until("the client to detach", TIMEOUT, || daemon.live_sessions() == 1);

    // No waker yet: the restore's replay wakes nobody.
    let mut ed = Editor::new(Buffer::from_str("notes", ""));
    ed.set_terminal_config(Arc::new(cfg()));
    assert!(ed.restore_session_file(&file).expect("restore"));
    let buf = ed.buf_id;
    assert!(ed.is_terminal(buf));

    let wakes = Arc::new(AtomicUsize::new(0));
    let w = wakes.clone();
    ed.set_waker(Arc::new(move || {
        w.fetch_add(1, Ordering::SeqCst);
    }));
    let mut x = E { ed, d: Dispatcher::new(), wakes: wakes.clone() };
    // Like the view: tick only after a wake-up.
    let mut seen = 0;
    let mut tick_when_woken = |x: &mut E, what: &str, done: &dyn Fn(&E) -> bool| {
        wait_until(what, TIMEOUT, || {
            let n = x.wakes.load(Ordering::SeqCst);
            if n > seen {
                seen = n;
                x.ed.tick(Instant::now());
            }
            done(x)
        });
    };
    let has_line = |x: &E, line: &str| x.text(buf).lines().any(|l| l.trim_end() == line);
    tick_when_woken(&mut x, "the replay after a wake-up", &|x| has_line(x, "before-2"));
    x.type_line("echo live-$((2+2))");
    tick_when_woken(&mut x, "live output after a wake-up", &|x| has_line(x, "live-4"));
    x.ed.kill_buffer(buf);
    wait_until("the session to end", TIMEOUT, || daemon.live_sessions() == 0);
    let _ = std::fs::remove_dir_all(state);
}

#[test]
fn hooks_see_terminal_events_in_order() {
    let script = "printf 'out\\033]2;the-title\\007'; printf '\\a'; exit 3";
    let mut x = E::new(sh_config(PtyBackendKind::Local, None, &["-c", script]));
    let log = Rc::new(RefCell::new(Vec::<Event>::new()));
    for kind in EventKind::ALL {
        let log = log.clone();
        x.ed.hooks.add(
            kind,
            "record",
            Rc::new(move |_, e| {
                log.borrow_mut().push(e.clone());
                Ok(())
            }),
        );
    }
    // A timer the tick fires alongside terminal output.
    let t0 = Instant::now();
    x.ed.hooks.run_at("soon", t0, None);

    x.ed.execute("term", Args::default());
    let buf = x.ed.buf_id;
    x.wait_exit(buf);

    let events = log.borrow().clone();
    let pos = |pred: &dyn Fn(&Event) -> bool| events.iter().position(pred);
    let created = pos(&|e| matches!(e, Event::BufferCreated { buf: b } if *b == buf)).expect("BufferCreated");
    let window = pos(&|e| matches!(e, Event::WindowConfigurationChanged)).expect("WindowConfigurationChanged");
    let command = pos(&|e| matches!(e, Event::AfterCommand { name } if name == "term")).expect("AfterCommand");
    let output = pos(&|e| matches!(e, Event::TerminalOutput { buf: b } if *b == buf)).expect("TerminalOutput");
    let title = pos(&|e| matches!(e, Event::TerminalTitle { buf: b, title } if *b == buf && title == "the-title"))
        .expect("TerminalTitle");
    let bell = pos(&|e| matches!(e, Event::TerminalBell { buf: b } if *b == buf)).expect("TerminalBell");
    let exited = pos(&|e| matches!(e, Event::ProcessExited { buf: b, status } if *b == buf && status.code == 3))
        .expect("ProcessExited");
    assert!(
        pos(&|e| matches!(e, Event::Timer { name, .. } if name == "soon")).is_some(),
        "the timer fired: {events:?}"
    );

    assert!(created < window && window < command, "{events:?}");
    assert!(command < output && output < title && title < bell && bell < exited, "{events:?}");
    let last_output = events.iter().rposition(|e| matches!(e, Event::TerminalOutput { .. })).unwrap();
    assert!(last_output < exited, "all output comes before the exit: {events:?}");
    assert_eq!(events.iter().filter(|e| matches!(e, Event::ProcessExited { .. })).count(), 1);
    assert_eq!(x.ed.terminal(buf).unwrap().term.title(), "the-title");

    // Nothing more happens after the exit.
    log.borrow_mut().clear();
    x.ed.tick(Instant::now() + Duration::from_millis(1));
    assert!(log.borrow().iter().all(|e| matches!(e, Event::Timer { .. })), "{:?}", log.borrow());

    // Split windows are a layout change hooks hear about exactly once.
    log.borrow_mut().clear();
    x.ed.execute("split-window-below", Args::default());
    let changes = log.borrow().iter().filter(|e| matches!(e, Event::WindowConfigurationChanged)).count();
    assert_eq!(changes, 1);
    assert_eq!(x.ed.layout.tab().root.windows().len(), 2);
}
