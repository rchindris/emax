//! End-to-end tests of the session daemon and its client: a daemon runs
//! in-process on a temporary socket with a mock PTY backend (so the tests
//! control exactly what the "programs" print), and clients talk to it over
//! the real socket, through [`DaemonBackend`] or raw frames.

use std::fs;
use std::io::{self, BufReader, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::client::DaemonBackend;
use super::daemon::{DaemonOptions, HIGH_WATER, bind, parse_args, serve};
use super::protocol::{ClientMsg, DaemonMsg, MAX_OUTPUT_CHUNK, Op, PROTOCOL_VERSION, Reply, read_frame, write_frame};
use crate::engine::pty::mock::{MockBackend, MockHandle};
use crate::engine::pty::{ExitStatus, PtyBackend, PtyEvent, PtySize, SessionId, SpawnSpec, noop_waker};
use crate::engine::testutil::{TIMEOUT, temp_dir, wait_until};

// ---------------------------------------------------------------- harness

/// An in-process daemon on a temporary socket, with a mock backend.
struct TestDaemon {
    dir: PathBuf,
    socket: PathBuf,
    mock: MockHandle,
    thread: Option<JoinHandle<io::Result<()>>>,
}

impl TestDaemon {
    fn start(replay_bytes: usize) -> Self {
        Self::start_with(replay_bytes, None)
    }

    fn start_with(replay_bytes: usize, idle_exit: Option<Duration>) -> Self {
        let dir = temp_dir("ptyd");
        let socket = dir.join("s.sock");
        // Bound here, so clients can connect as soon as this returns.
        let bound = bind(&socket).expect("bind");
        let opts = DaemonOptions { socket: socket.clone(), replay_bytes, idle_exit };
        let (htx, hrx) = mpsc::channel();
        let thread = thread::spawn(move || {
            serve(bound, &opts, move |w| {
                let (b, h) = MockBackend::with_waker(w);
                htx.send(h).unwrap();
                Box::new(b)
            })
        });
        let mock = hrx.recv_timeout(TIMEOUT).expect("daemon backend");
        Self { dir, socket, mock, thread: Some(thread) }
    }

    fn client(&self) -> DaemonBackend {
        DaemonBackend::connect(&self.socket, noop_waker()).expect("connect")
    }

    /// A raw connection that has done the handshake.
    fn raw(&self) -> UnixStream {
        let mut s = UnixStream::connect(&self.socket).unwrap();
        s.set_read_timeout(Some(TIMEOUT)).unwrap();
        write_frame(&mut s, &ClientMsg::Hello { version: PROTOCOL_VERSION, client_pid: 1 }).unwrap();
        match read_frame::<DaemonMsg>(&mut s).unwrap() {
            Some(DaemonMsg::Hello { version, .. }) => assert_eq!(version, PROTOCOL_VERSION),
            other => panic!("expected a greeting, got {other:?}"),
        }
        s
    }

    fn finished(&self) -> bool {
        self.thread.as_ref().is_none_or(|t| t.is_finished())
    }

    /// Wait for the daemon to exit and return its result.
    fn join(&mut self) -> io::Result<()> {
        wait_until("the daemon to exit", TIMEOUT, || self.finished());
        self.thread.take().unwrap().join().unwrap()
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        if !self.finished() {
            if let Ok(mut c) = DaemonBackend::connect(&self.socket, noop_waker()) {
                let _ = c.shutdown();
            }
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Read `b`'s events into `events` until `cond` holds.
fn recv_until(b: &mut impl PtyBackend, events: &mut Vec<PtyEvent>, what: &str, cond: impl Fn(&[PtyEvent]) -> bool) {
    wait_until(what, TIMEOUT, || {
        while let Some(e) = b.try_recv() {
            events.push(e);
        }
        cond(events)
    });
}

/// All output of session `id` in `events`, in order.
fn output(events: &[PtyEvent], id: SessionId) -> Vec<u8> {
    events
        .iter()
        .filter_map(|e| match e {
            PtyEvent::Output(s, d) if *s == id => Some(d.as_slice()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .concat()
}

fn exits(events: &[PtyEvent], id: SessionId) -> Vec<ExitStatus> {
    events
        .iter()
        .filter_map(|e| match e {
            PtyEvent::Exited(s, st) if *s == id => Some(st.clone()),
            _ => None,
        })
        .collect()
}

/// Receive until session `id`'s output is exactly `want`.
fn expect_output(b: &mut impl PtyBackend, events: &mut Vec<PtyEvent>, id: SessionId, want: &[u8]) {
    recv_until(b, events, &format!("output {:?}", String::from_utf8_lossy(want)), |ev| output(ev, id).len() >= want.len());
    assert_eq!(output(events, id), want);
}

fn spec(program: &str) -> SpawnSpec {
    SpawnSpec { program: program.into(), args: vec!["-i".into()], size: PtySize::new(80, 24), ..Default::default() }
}

/// Wait until the daemon has drained output pushed to the mock: it is in
/// the replay ring once another client's attach replays it. Instead of
/// sleeping, round-trip a request (the daemon handles inputs in order, and
/// the mock's wake-up was queued before the request).
fn sync(b: &mut DaemonBackend) {
    b.list().unwrap();
}

// ------------------------------------------------------------- basic ops

#[test]
fn spawn_write_resize_list_kill() {
    let d = TestDaemon::start(1 << 20);
    let mut c = d.client();
    assert!(c.persistent());
    assert_eq!(c.daemon_pid(), std::process::id());
    let id = c.spawn(spec("/bin/sh")).unwrap();
    assert_eq!(d.mock.session(id).unwrap().spec, spec("/bin/sh"));

    c.write(id, b"ls\r").unwrap();
    c.write(id, b"\xff\x00 raw").unwrap();
    wait_until("input to arrive", TIMEOUT, || d.mock.input(id) == b"ls\r\xff\x00 raw");

    c.resize(id, PtySize::new(120, 40)).unwrap();
    wait_until("the resize", TIMEOUT, || d.mock.session(id).unwrap().size == PtySize::new(120, 40));

    let list = c.list().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!((list[0].id, list[0].program.as_str(), list[0].size), (id, "/bin/sh", PtySize::new(120, 40)));
    assert_eq!(list[0].args, ["-i"]);
    assert_eq!(list[0].exited, None);

    c.kill(id).unwrap();
    assert!(d.mock.session(id).is_none(), "the process was killed");
    assert!(c.list().unwrap().is_empty());
    assert_eq!(c.kill(id).unwrap_err().kind(), io::ErrorKind::NotFound);
}

#[test]
fn unknown_sessions_and_failed_spawns_are_errors() {
    let d = TestDaemon::start(1 << 20);
    let mut c = d.client();
    let nope = SessionId(999);
    for e in [c.attach(nope, PtySize::default()).unwrap_err(), c.detach(nope).unwrap_err(), c.kill(nope).unwrap_err()] {
        assert_eq!(e.kind(), io::ErrorKind::NotFound, "{e}");
        assert!(e.to_string().contains("no session 999"), "{e}");
    }
    // Writes and resizes ask for no reply: they fail silently, and the
    // connection stays usable.
    c.write(nope, b"x").unwrap();
    c.resize(nope, PtySize::default()).unwrap();
    d.mock.fail_next_spawn("no such program");
    let e = c.spawn(spec("/nope")).unwrap_err();
    assert!(e.to_string().contains("no such program") && e.to_string().contains("/nope"), "{e}");
    assert!(c.spawn(spec("/bin/sh")).is_ok());
}

#[test]
fn output_reaches_the_client_in_bounded_chunks() {
    let d = TestDaemon::start(1 << 20);
    let mut c = d.client();
    let id = c.spawn(spec("sh")).unwrap();
    let mut ev = Vec::new();
    d.mock.output(id, b"hello ");
    d.mock.output(id, b"\x1b[31mworld\xff");
    expect_output(&mut c, &mut ev, id, b"hello \x1b[31mworld\xff");

    // One big write from the program arrives whole, in frames of at most MAX_OUTPUT_CHUNK.
    let big: Vec<u8> = (0..MAX_OUTPUT_CHUNK * 3 + 17).map(|i| (i * 7 % 251) as u8).collect();
    let mut ev = Vec::new();
    d.mock.output(id, &big);
    expect_output(&mut c, &mut ev, id, &big);
    assert_eq!(ev.len(), 4);
    assert!(ev.iter().all(|e| matches!(e, PtyEvent::Output(_, d) if d.len() <= MAX_OUTPUT_CHUNK)));

    // Lots of small writes: all of it, in order.
    let mut want = Vec::new();
    for i in 0..2000 {
        let line = format!("line {i}\n");
        d.mock.output(id, line.as_bytes());
        want.extend_from_slice(line.as_bytes());
    }
    let mut ev = Vec::new();
    expect_output(&mut c, &mut ev, id, &want);
}

#[test]
fn large_writes_are_split_and_arrive_whole() {
    let d = TestDaemon::start(1 << 20);
    let mut c = d.client();
    let id = c.spawn(spec("cat")).unwrap();
    let data: Vec<u8> = (0..3 * (1 << 20) + 5).map(|i| (i % 253) as u8).collect();
    c.write(id, &data).unwrap();
    wait_until("3 MiB of input", TIMEOUT, || d.mock.input(id).len() == data.len());
    assert_eq!(d.mock.input(id), data);
}

// ------------------------------------------------------- detach / replay

#[test]
fn reattaching_replays_the_ring_then_live_output() {
    let d = TestDaemon::start(1 << 20);
    let mut a = d.client();
    let id = a.spawn(spec("sh")).unwrap();
    let mut ev = Vec::new();
    d.mock.output(id, b"one ");
    expect_output(&mut a, &mut ev, id, b"one ");
    drop(a); // the editor quits

    // Output while nobody is attached still goes into the ring.
    d.mock.output(id, b"two ");
    assert!(d.mock.session(id).is_some(), "the session outlives its client");

    let mut b = d.client();
    sync(&mut b);
    let list = b.list().unwrap();
    assert_eq!(list.iter().map(|s| s.id).collect::<Vec<_>>(), [id]);
    b.attach(id, PtySize::new(100, 30)).unwrap();
    // The replay is complete as soon as attach returns.
    let mut ev = Vec::new();
    while let Some(e) = b.try_recv() {
        ev.push(e);
    }
    assert_eq!(output(&ev, id), b"one two ");
    assert_eq!(d.mock.session(id).unwrap().resizes.last(), Some(&PtySize::new(100, 30)), "attach resizes");
    d.mock.output(id, b"three");
    expect_output(&mut b, &mut ev, id, b"one two three");
}

#[test]
fn detach_stops_output_but_the_ring_keeps_it() {
    let d = TestDaemon::start(1 << 20);
    let mut c = d.client();
    let id = c.spawn(spec("sh")).unwrap();
    c.detach(id).unwrap();
    d.mock.output(id, b"while detached");
    sync(&mut c);
    assert_eq!(c.try_recv(), None);
    c.attach(id, PtySize::default()).unwrap();
    let mut ev = Vec::new();
    expect_output(&mut c, &mut ev, id, b"while detached");
}

#[test]
fn the_replay_ring_is_bounded() {
    let d = TestDaemon::start(10);
    let mut a = d.client();
    let id = a.spawn(spec("sh")).unwrap();
    for piece in [&b"0123"[..], b"4567", b"89ab", b"cdef"] {
        d.mock.output(id, piece);
    }
    sync(&mut a);
    let mut b = d.client();
    b.attach(id, PtySize::default()).unwrap();
    let mut ev = Vec::new();
    while let Some(e) = b.try_recv() {
        ev.push(e);
    }
    assert_eq!(output(&ev, id), b"6789abcdef");

    // A single write larger than the ring keeps only its tail.
    let mut c = d.client();
    d.mock.output(id, b"ABCDEFGHIJKLMNOPQRSTUVWXYZ");
    sync(&mut c);
    c.attach(id, PtySize::default()).unwrap();
    let mut ev = Vec::new();
    while let Some(e) = c.try_recv() {
        ev.push(e);
    }
    assert_eq!(output(&ev, id), b"QRSTUVWXYZ");
}

#[test]
fn a_zero_byte_ring_replays_nothing() {
    let d = TestDaemon::start(0);
    let mut a = d.client();
    let id = a.spawn(spec("sh")).unwrap();
    d.mock.output(id, b"gone");
    sync(&mut a);
    let mut b = d.client();
    b.attach(id, PtySize::default()).unwrap();
    d.mock.output(id, b"live");
    let mut ev = Vec::new();
    expect_output(&mut b, &mut ev, id, b"live");
}

#[test]
fn a_big_replay_is_sent_in_chunks() {
    let d = TestDaemon::start(1 << 20);
    let mut a = d.client();
    let id = a.spawn(spec("sh")).unwrap();
    let data: Vec<u8> = (0..(1 << 20) + 100).map(|i| (i % 241) as u8).collect();
    d.mock.output(id, &data);
    sync(&mut a);
    let mut b = d.client();
    b.attach(id, PtySize::default()).unwrap();
    let mut ev = Vec::new();
    while let Some(e) = b.try_recv() {
        ev.push(e);
    }
    assert_eq!(output(&ev, id), &data[100..]);
    assert_eq!(ev.len(), (1 << 20) / MAX_OUTPUT_CHUNK);
}

/// On attach the daemon sends the replay, then the exit, then the reply.
#[test]
fn attach_sends_replay_exit_then_reply() {
    let d = TestDaemon::start(1 << 20);
    let mut a = d.client();
    let id = a.spawn(spec("sh")).unwrap();
    d.mock.output(id, b"bye");
    d.mock.exit(id, ExitStatus { code: 3, signal: None });
    sync(&mut a);

    let mut raw = d.raw();
    write_frame(&mut raw, &ClientMsg::Request { req: 7, op: Op::Attach { id, size: PtySize::default() } }).unwrap();
    let mut r = BufReader::new(raw);
    assert_eq!(read_frame::<DaemonMsg>(&mut r).unwrap(), Some(DaemonMsg::Output { id, data: b"bye".to_vec() }));
    assert_eq!(read_frame::<DaemonMsg>(&mut r).unwrap(), Some(DaemonMsg::Exited { id, status: ExitStatus { code: 3, signal: None } }));
    assert_eq!(read_frame::<DaemonMsg>(&mut r).unwrap(), Some(DaemonMsg::Reply { req: 7, result: Ok(Reply::Ok) }));
}

// ---------------------------------------------------------- many clients

#[test]
fn every_attached_client_gets_output_and_the_last_resize_wins() {
    let d = TestDaemon::start(1 << 20);
    let mut a = d.client();
    let mut b = d.client();
    let id = a.spawn(spec("sh")).unwrap();
    b.attach(id, PtySize::new(90, 20)).unwrap();
    d.mock.output(id, b"for both");
    let (mut ea, mut eb) = (Vec::new(), Vec::new());
    expect_output(&mut a, &mut ea, id, b"for both");
    expect_output(&mut b, &mut eb, id, b"for both");

    // Input from either goes to the process.
    a.write(id, b"a").unwrap();
    sync(&mut a);
    b.write(id, b"b").unwrap();
    sync(&mut b);
    assert_eq!(d.mock.input(id), b"ab");

    a.resize(id, PtySize::new(70, 10)).unwrap();
    sync(&mut a);
    b.resize(id, PtySize::new(71, 11)).unwrap();
    sync(&mut b);
    assert_eq!(d.mock.session(id).unwrap().size, PtySize::new(71, 11));
    assert_eq!(a.list().unwrap()[0].size, PtySize::new(71, 11));

    // One detaches; the other still gets output.
    b.detach(id).unwrap();
    d.mock.output(id, b"!");
    expect_output(&mut a, &mut ea, id, b"for both!");
    sync(&mut b);
    assert_eq!(b.try_recv(), None);
}

#[test]
fn many_clients_in_parallel() {
    let d = TestDaemon::start(1 << 20);
    let socket = d.socket.clone();
    let threads: Vec<_> = (0..8)
        .map(|i| {
            let socket = socket.clone();
            thread::spawn(move || {
                let mut c = DaemonBackend::connect(&socket, noop_waker()).unwrap();
                let id = c.spawn(spec(&format!("prog{i}"))).unwrap();
                for _ in 0..50 {
                    c.write(id, format!("{i}").as_bytes()).unwrap();
                }
                c.list().unwrap();
                id
            })
        })
        .collect();
    let ids: Vec<SessionId> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    for (i, id) in ids.iter().enumerate() {
        assert_eq!(d.mock.session(*id).unwrap().spec.program, format!("prog{i}"));
        assert_eq!(d.mock.input(*id), format!("{i}").repeat(50).as_bytes());
    }
    let mut c = d.client();
    assert_eq!(c.list().unwrap().len(), 8);
}

#[test]
fn a_crashed_client_leaves_its_sessions_running() {
    let d = TestDaemon::start(1 << 20);
    let mut a = d.client();
    let id = a.spawn(spec("sh")).unwrap();
    // Another client attaches, then dies in the middle of sending a frame.
    let mut raw = d.raw();
    write_frame(&mut raw, &ClientMsg::Request { req: 1, op: Op::Attach { id, size: PtySize::default() } }).unwrap();
    raw.write_all(&[0, 0, 0, 50, 1, 2]).unwrap();
    drop(raw);
    d.mock.output(id, b"still here");
    let mut ev = Vec::new();
    expect_output(&mut a, &mut ev, id, b"still here");
    assert!(d.mock.session(id).is_some());
    assert_eq!(a.list().unwrap().len(), 1);
}

#[test]
fn an_oversized_frame_drops_only_that_client() {
    let d = TestDaemon::start(1 << 20);
    let mut a = d.client();
    let mut raw = d.raw();
    raw.write_all(&u32::MAX.to_be_bytes()).unwrap();
    let mut r = BufReader::new(raw);
    assert!(matches!(read_frame::<DaemonMsg>(&mut r), Ok(None) | Err(_)), "the daemon hangs up");
    assert!(a.spawn(spec("sh")).is_ok());
}

#[test]
fn the_handshake_comes_first_and_versions_must_match() {
    let d = TestDaemon::start(1 << 20);
    // A newer client: told the daemon's version, then hung up on.
    let mut s = UnixStream::connect(&d.socket).unwrap();
    s.set_read_timeout(Some(TIMEOUT)).unwrap();
    write_frame(&mut s, &ClientMsg::Hello { version: PROTOCOL_VERSION + 1, client_pid: 1 }).unwrap();
    assert!(matches!(read_frame::<DaemonMsg>(&mut s).unwrap(), Some(DaemonMsg::Hello { version: PROTOCOL_VERSION, .. })));
    assert!(matches!(read_frame::<DaemonMsg>(&mut s), Ok(None) | Err(_)));

    // A request before the greeting: hung up on, no reply.
    let mut s = UnixStream::connect(&d.socket).unwrap();
    s.set_read_timeout(Some(TIMEOUT)).unwrap();
    write_frame(&mut s, &ClientMsg::Request { req: 1, op: Op::List }).unwrap();
    assert!(matches!(read_frame::<DaemonMsg>(&mut s), Ok(None) | Err(_)));

    // A second greeting is a protocol error too.
    let mut s = d.raw();
    write_frame(&mut s, &ClientMsg::Hello { version: PROTOCOL_VERSION, client_pid: 1 }).unwrap();
    assert!(matches!(read_frame::<DaemonMsg>(&mut s), Ok(None) | Err(_)));

    // The daemon is fine.
    assert!(d.client().list().unwrap().is_empty());
}

// ------------------------------------------------------ exits and the end

#[test]
fn exited_sessions_stay_listed_until_killed() {
    let d = TestDaemon::start(1 << 20);
    let mut a = d.client();
    let mut b = d.client();
    let id = a.spawn(spec("sh")).unwrap();
    b.attach(id, PtySize::default()).unwrap();
    let status = ExitStatus { code: 0, signal: Some("Hangup".into()) };
    d.mock.output(id, b"last words");
    d.mock.exit(id, status.clone());
    let mut ev = Vec::new();
    recv_until(&mut a, &mut ev, "the exit", |ev| !exits(ev, id).is_empty());
    // All output comes before the exit.
    assert!(matches!(ev.last(), Some(PtyEvent::Exited(..))));
    assert_eq!(output(&ev, id), b"last words");
    let mut ev = Vec::new();
    recv_until(&mut b, &mut ev, "the exit (second client)", |ev| !exits(ev, id).is_empty());

    assert_eq!(a.list().unwrap()[0].exited, Some(status.clone()));
    assert!(a.write(id, b"x").is_ok(), "writes ask for no reply");
    let e = a.request_for_tests(Op::Write { id, data: b"x".to_vec() }).unwrap_err();
    assert!(e.to_string().contains("exited"), "{e}");
    a.resize(id, PtySize::new(10, 10)).unwrap();

    // A new client sees the replay and the exit.
    let mut c = d.client();
    c.attach(id, PtySize::default()).unwrap();
    let mut ev = Vec::new();
    while let Some(e) = c.try_recv() {
        ev.push(e);
    }
    assert_eq!(output(&ev, id), b"last words");
    assert_eq!(exits(&ev, id), [status]);

    a.kill(id).unwrap();
    assert!(c.list().unwrap().is_empty());
}

#[test]
fn killing_a_session_tells_the_other_clients() {
    let d = TestDaemon::start(1 << 20);
    let mut a = d.client();
    let mut b = d.client();
    let id = a.spawn(spec("sh")).unwrap();
    b.attach(id, PtySize::default()).unwrap();
    d.mock.output(id, b"x");
    let mut ea = Vec::new();
    expect_output(&mut a, &mut ea, id, b"x");
    a.kill(id).unwrap();
    // The killer hears nothing more about it.
    d.mock.output(id, b"after");
    sync(&mut a);
    assert_eq!(a.try_recv(), None);
    let mut eb = Vec::new();
    recv_until(&mut b, &mut eb, "the kill", |ev| !exits(ev, id).is_empty());
    assert_eq!(exits(&eb, id)[0].signal.as_deref(), Some("Killed"));
}

#[test]
fn shutdown_kills_every_session_and_removes_the_socket() {
    let d = TestDaemon::start(1 << 20);
    let mut a = d.client();
    let mut other = d.client();
    let ids = [a.spawn(spec("a")).unwrap(), a.spawn(spec("b")).unwrap()];
    a.shutdown().unwrap();
    for id in ids {
        assert!(d.mock.session(id).is_none());
    }
    // Everyone is hung up on.
    wait_until("the other client to be disconnected", TIMEOUT, || !other.is_connected());
    assert_eq!(other.list().unwrap_err().kind(), io::ErrorKind::NotConnected);
    assert_eq!(other.write(ids[0], b"x").unwrap_err().kind(), io::ErrorKind::NotConnected);
    let mut d = d;
    d.join().unwrap();
    assert!(!d.socket.exists(), "the socket file is removed");
    // The lock file stays (harmless); a new daemon can serve the same path.
    assert!(d.dir.join("s.lock").exists());
    let bound = bind(&d.socket).unwrap();
    drop(bound);
}

#[test]
fn an_idle_daemon_exits_but_not_while_in_use() {
    let d = TestDaemon::start_with(1 << 20, Some(Duration::from_millis(150)));
    // In use: a client with a session. It must outlive several idle periods.
    let mut c = d.client();
    let id = c.spawn(spec("sh")).unwrap();
    drop(c);
    // Absence of an exit can only be checked over time.
    thread::sleep(Duration::from_millis(600));
    assert!(!d.finished(), "a daemon with sessions stays");
    let mut c = d.client();
    c.kill(id).unwrap();
    thread::sleep(Duration::from_millis(600));
    assert!(!d.finished(), "a daemon with a client stays");
    drop(c);
    let start = Instant::now();
    let mut d = d;
    d.join().unwrap();
    assert!(start.elapsed() < TIMEOUT);
}

// -------------------------------------------------------------- flow

/// A client that stops reading makes the daemon hold back the output of
/// its sessions (so programs block, instead of the daemon buffering without
/// bound); nothing is lost, and everything flows again once it goes away.
#[test]
fn a_stalled_client_holds_output_back_without_losing_any() {
    let d = TestDaemon::start(1 << 10);
    let mut a = d.client();
    let id = a.spawn(spec("yes")).unwrap();
    let mut stalled = d.raw();
    write_frame(&mut stalled, &ClientMsg::Request { req: 1, op: Op::Attach { id, size: PtySize::default() } }).unwrap();
    // Attached once the reply is in; from then on it reads nothing.
    assert_eq!(read_frame::<DaemonMsg>(&mut stalled).unwrap(), Some(DaemonMsg::Reply { req: 1, result: Ok(Reply::Ok) }));

    // Produce well over the high-water mark, reading it all from `a`.
    let chunk: Vec<u8> = (0..64 << 10).map(|i| (i % 256) as u8).collect();
    let n = 3 * HIGH_WATER / chunk.len();
    let total = n * chunk.len();
    for _ in 0..n {
        d.mock.output(id, &chunk);
    }
    // The stalled client falls HIGH_WATER behind: the session is held back
    // for good. (`a` lagging can hold it back for a moment too; then a
    // round trip brings more output.) Everything the daemon sent `a` is in
    // once a request round-trips: its reply is queued behind the output.
    let mut ev = Vec::new();
    wait_until("the session to be held back with `a` caught up", TIMEOUT, || {
        let before = output(&ev, id).len();
        sync(&mut a);
        while let Some(e) = a.try_recv() {
            ev.push(e);
        }
        d.mock.session(id).unwrap().paused && output(&ev, id).len() == before
    });
    let held = output(&ev, id).len();
    assert!(held >= HIGH_WATER && held < total, "{held} of {total} bytes flowed");
    assert!(d.mock.session(id).unwrap().paused, "held back while the client stalls");

    // It goes away: everything arrives, in order.
    drop(stalled);
    recv_until(&mut a, &mut ev, "all output", |ev| output(ev, id).len() >= total);
    let all = output(&ev, id);
    assert_eq!(all.len(), total);
    assert!(all.chunks(chunk.len()).all(|c| c == chunk.as_slice()));
    assert!(!d.mock.session(id).unwrap().paused);
}

/// One stalled client holds back only the sessions it is attached to:
/// another client's sessions keep flowing (no head-of-line blocking).
#[test]
fn a_stalled_client_does_not_hold_back_other_sessions() {
    let d = TestDaemon::start(1 << 10);
    let mut stalled = d.raw();
    write_frame(&mut stalled, &ClientMsg::Request { req: 1, op: Op::Spawn(spec("x")) }).unwrap();
    let x = match read_frame::<DaemonMsg>(&mut stalled).unwrap() {
        Some(DaemonMsg::Reply { req: 1, result: Ok(Reply::Spawned(id)) }) => id,
        other => panic!("expected a spawn reply, got {other:?}"),
    };
    let mut b = d.client();
    let y = b.spawn(spec("y")).unwrap();
    let chunk = vec![b'x'; 64 << 10];
    for _ in 0..3 * HIGH_WATER / chunk.len() {
        d.mock.output(x, &chunk);
    }
    wait_until("the stalled session to be held back", TIMEOUT, || d.mock.session(x).unwrap().paused);
    d.mock.output(y, b"hello-from-y");
    let mut ev = Vec::new();
    expect_output(&mut b, &mut ev, y, b"hello-from-y");
    assert!(!d.mock.session(y).unwrap().paused);
    drop(stalled);
    wait_until("the session to flow again", TIMEOUT, || !d.mock.session(x).unwrap().paused);
}

// ---------------------------------------------------------- binding

/// Wait until nobody listens on `path`. A listener this process just closed
/// can live on for a moment in a child another test is forking (until it
/// execs), so a socket isn't stale the instant its listener is dropped.
fn wait_stale(path: &std::path::Path) {
    wait_until("the socket to go stale", TIMEOUT, || {
        UnixStream::connect(path).is_err_and(|e| e.kind() == io::ErrorKind::ConnectionRefused)
    });
}

#[test]
fn bind_replaces_a_stale_socket_and_refuses_a_live_one() {
    let dir = temp_dir("bind");
    let path = dir.join("d.sock");
    // A socket file nobody listens on (its daemon died).
    drop(UnixListener::bind(&path).unwrap());
    assert!(fs::symlink_metadata(&path).unwrap().file_type().is_socket());
    wait_stale(&path);
    let bound = bind(&path).unwrap();
    assert!(UnixStream::connect(&path).is_ok());
    // A second daemon on the same path: refused, the first keeps it.
    assert_eq!(bind(&path).err().unwrap().kind(), io::ErrorKind::AddrInUse);
    drop(bound);
    assert!(!path.exists());

    // Someone else's live socket without our lock: also refused.
    let other = dir.join("o.sock");
    let _l = UnixListener::bind(&other).unwrap();
    assert_eq!(bind(&other).err().unwrap().kind(), io::ErrorKind::AddrInUse);

    // Not a socket: left alone.
    let file = dir.join("f.sock");
    fs::write(&file, "data").unwrap();
    assert_eq!(bind(&file).err().unwrap().kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read(&file).unwrap(), b"data");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn the_socket_directory_is_private() {
    let dir = temp_dir("perm");
    let path = dir.join("a").join("b").join("d.sock");
    let bound = bind(&path).unwrap();
    let mode = |p: &std::path::Path| fs::metadata(p).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode(path.parent().unwrap()), 0o700);
    assert_eq!(fs::symlink_metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    drop(bound);

    // An existing directory of ours that others could write to is tightened.
    let open = dir.join("open");
    fs::create_dir(&open).unwrap();
    fs::set_permissions(&open, fs::Permissions::from_mode(0o777)).unwrap();
    drop(bind(&open.join("d.sock")).unwrap());
    assert_eq!(mode(&open), 0o700);
    // A sticky shared directory (like /tmp) is left as it is.
    let sticky = dir.join("sticky");
    fs::create_dir(&sticky).unwrap();
    fs::set_permissions(&sticky, fs::Permissions::from_mode(0o1777)).unwrap();
    drop(bind(&sticky.join("d.sock")).unwrap());
    assert_eq!(mode(&sticky), 0o1777);
    // A file where the directory should be.
    fs::write(dir.join("file"), "").unwrap();
    assert!(bind(&dir.join("file").join("d.sock")).is_err());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_client_finds_a_stale_socket_refused() {
    let dir = temp_dir("stale");
    let path = dir.join("d.sock");
    assert_eq!(DaemonBackend::connect(&path, noop_waker()).err().unwrap().kind(), io::ErrorKind::NotFound);
    drop(UnixListener::bind(&path).unwrap());
    wait_stale(&path);
    assert_eq!(DaemonBackend::connect(&path, noop_waker()).err().unwrap().kind(), io::ErrorKind::ConnectionRefused);
    // A daemon started there takes over, and clients reach it.
    let d = {
        let bound = bind(&path).unwrap();
        let opts = DaemonOptions { socket: path.clone(), replay_bytes: 100, idle_exit: None };
        thread::spawn(move || serve(bound, &opts, |w| Box::new(MockBackend::with_waker(w).0)))
    };
    let mut c = DaemonBackend::connect(&path, noop_waker()).unwrap();
    c.shutdown().unwrap();
    d.join().unwrap().unwrap();
    fs::remove_dir_all(dir).unwrap();
}

// ------------------------------------------------------------- CLI

#[test]
fn daemon_arguments() {
    let args = |v: &[&str]| parse_args(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    let cli = args(&[]).unwrap();
    assert!(!cli.foreground);
    assert_eq!(cli.opts.idle_exit, None);
    assert_eq!(cli.opts.replay_bytes, super::daemon::DEFAULT_REPLAY_BYTES);
    assert!(cli.opts.socket.ends_with("ptyd.sock"));
    let cli = args(&["--socket", "/x/y.sock", "--replay-bytes", "123", "--idle-exit", "30", "--foreground"]).unwrap();
    assert_eq!(cli.opts.socket, PathBuf::from("/x/y.sock"));
    assert_eq!(cli.opts.replay_bytes, 123);
    assert_eq!(cli.opts.idle_exit, Some(Duration::from_secs(30)));
    assert!(cli.foreground);
    assert_eq!(args(&["--idle-exit", "0"]).unwrap().opts.idle_exit, None);
    assert!(args(&["--socket"]).unwrap_err().contains("needs a value"));
    assert!(args(&["--replay-bytes", "lots"]).unwrap_err().contains("not a number"));
    assert!(args(&["--idle-exit", "-1"]).is_err());
    assert!(args(&["--bogus"]).unwrap_err().contains("--bogus"));
    // Bad arguments exit with the usage code before touching anything.
    assert_eq!(super::daemon::main(&["--bogus".to_string()]), super::daemon::exit_code::USAGE);
}
