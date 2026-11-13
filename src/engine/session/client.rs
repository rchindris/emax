//! [`DaemonBackend`]: a [`PtyBackend`] whose sessions live in the session
//! daemon, reached over its Unix socket.
//!
//! A reader thread turns the daemon's frames into a bounded channel of
//! events and replies (bounded so a flood the editor can't keep up with
//! backs up into the socket and on to the daemon, which then stops reading
//! from the programs). A request is written from the caller's thread, which
//! then reads the channel until its reply arrives, setting aside the events
//! it meets for [`PtyBackend::try_recv`] (so events keep their order: e.g.
//! an attach's replay comes before anything after it).
//!
//! If the connection is lost, the sessions live on in the daemon; this
//! backend reports nothing more, and every later call fails with
//! `NotConnected`.

use std::collections::VecDeque;
use std::io::{self, BufReader};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use super::daemon::exit_code;
use super::protocol::{ClientMsg, DaemonMsg, Op, PROTOCOL_VERSION, Reply, read_frame, write_frame};
use crate::engine::pty::{Notifier, PtyBackend, PtyEvent, PtySize, SessionId, SessionInfo, SpawnSpec, Waker};

/// How long a request waits for its reply (and the handshake for the daemon).
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// How long `connect_or_start` waits for a daemon it started to listen.
pub const START_TIMEOUT: Duration = Duration::from_secs(5);

/// Events (and replies) the reader thread may queue before it stops reading.
const CHANNEL_DEPTH: usize = 1024;

/// The largest input written in one `Write` request (bigger writes are split).
const MAX_WRITE_CHUNK: usize = 1 << 20;

/// What the reader thread hands over.
enum Incoming {
    Event(PtyEvent),
    Reply(u64, Result<Reply, String>),
}

pub struct DaemonBackend {
    stream: UnixStream,
    incoming: Receiver<Incoming>,
    /// Events met while waiting for a reply, delivered before newer ones.
    backlog: VecDeque<PtyEvent>,
    notifier: Notifier,
    connected: Arc<AtomicBool>,
    next_req: u64,
    daemon_pid: u32,
    reply_timeout: Duration,
}

fn not_connected() -> io::Error {
    io::Error::new(io::ErrorKind::NotConnected, "lost the connection to the session daemon")
}

/// An error message from the daemon as an `io::Error`.
fn remote_error(msg: String) -> io::Error {
    let kind = if msg.starts_with("no session") { io::ErrorKind::NotFound } else { io::ErrorKind::Other };
    io::Error::new(kind, msg)
}

impl DaemonBackend {
    /// Connect to a running daemon at `socket`.
    pub fn connect(socket: &Path, waker: Waker) -> io::Result<Self> {
        let stream = UnixStream::connect(socket)?;
        super::daemon::check_peer(&stream)?;
        Self::handshake(stream, waker)
    }

    fn handshake(mut stream: UnixStream, waker: Waker) -> io::Result<Self> {
        stream.set_read_timeout(Some(REPLY_TIMEOUT))?;
        write_frame(&mut stream, &ClientMsg::Hello { version: PROTOCOL_VERSION, client_pid: std::process::id() })?;
        let daemon_pid = match read_frame::<DaemonMsg>(&mut stream)? {
            Some(DaemonMsg::Hello { version, daemon_pid }) if version == PROTOCOL_VERSION => daemon_pid,
            Some(DaemonMsg::Hello { version, .. }) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("the session daemon speaks protocol {version}, this editor {PROTOCOL_VERSION}"),
                ));
            }
            Some(_) => return Err(io::Error::new(io::ErrorKind::InvalidData, "the session daemon did not greet")),
            None => return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "the session daemon hung up")),
        };
        // macOS refuses socket options once the peer has hung up; the reader
        // then finds the end of the stream, so that is not an error here.
        let _ = stream.set_read_timeout(None);
        // A daemon that stopped reading (hung, SIGSTOPped) must not freeze
        // the editor: a write that can't finish in time loses the connection.
        let _ = stream.set_write_timeout(Some(REPLY_TIMEOUT));
        let notifier = Notifier::new(waker);
        let connected = Arc::new(AtomicBool::new(true));
        let (tx, incoming) = mpsc::sync_channel(CHANNEL_DEPTH);
        let reader = stream.try_clone()?;
        {
            let (notifier, connected) = (notifier.clone(), connected.clone());
            thread::Builder::new()
                .name("ptyd-client".into())
                .spawn(move || read_loop(reader, tx, notifier, connected))?;
        }
        Ok(Self {
            stream,
            incoming,
            backlog: VecDeque::new(),
            notifier,
            connected,
            next_req: 0,
            daemon_pid,
            reply_timeout: REPLY_TIMEOUT,
        })
    }

    /// Connect, first starting a daemon (this executable with `--pty-daemon`)
    /// if none is running, waiting up to a few seconds for it to listen. An
    /// idle daemon exits after `idle_exit` (`None`: never).
    pub fn connect_or_start(
        socket: &Path,
        replay_bytes: usize,
        idle_exit: Option<Duration>,
        waker: Waker,
    ) -> io::Result<Self> {
        let exe = std::env::current_exe()?;
        Self::connect_or_start_with(&exe, socket, replay_bytes, idle_exit, waker)
    }

    /// [`connect_or_start`](Self::connect_or_start) running `program` as the daemon.
    pub fn connect_or_start_with(
        program: &Path,
        socket: &Path,
        replay_bytes: usize,
        idle_exit: Option<Duration>,
        waker: Waker,
    ) -> io::Result<Self> {
        let deadline = Instant::now() + START_TIMEOUT;
        let mut last_start: Option<Instant> = None;
        loop {
            let e = match Self::connect(socket, waker.clone()) {
                Ok(b) => return Ok(b),
                Err(e) if no_daemon(&e) => e,
                Err(e) => return Err(e),
            };
            if Instant::now() >= deadline {
                return Err(e);
            }
            // Start one, again now and then: a daemon that said "in use" may
            // have been looking at a socket that was just going away.
            if last_start.is_none_or(|t| t.elapsed() >= RESTART_INTERVAL) {
                start_daemon(program, socket, replay_bytes, idle_exit)?;
                last_start = Some(Instant::now());
                continue;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// The daemon's process id.
    pub fn daemon_pid(&self) -> u32 {
        self.daemon_pid
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    /// Kill every session and stop the daemon.
    pub fn shutdown(&mut self) -> io::Result<()> {
        self.request(Op::Shutdown).map(|_| ())
    }

    /// How long requests wait for a reply, and writes for the daemon to
    /// take them (tests shorten it).
    pub fn set_reply_timeout(&mut self, t: Duration) {
        self.reply_timeout = t;
        let _ = self.stream.set_write_timeout(Some(t));
    }

    fn send(&mut self, msg: &ClientMsg) -> io::Result<()> {
        if !self.is_connected() {
            return Err(not_connected());
        }
        write_frame(&mut self.stream, msg).map_err(|e| {
            // A frame may be half written: hang up rather than go on.
            self.connected.store(false, Ordering::Release);
            let _ = self.stream.shutdown(std::net::Shutdown::Both);
            io::Error::new(io::ErrorKind::NotConnected, e)
        })
    }

    /// Send `op` and wait for its reply.
    fn request(&mut self, op: Op) -> io::Result<Reply> {
        self.next_req += 1;
        let req = self.next_req;
        self.send(&ClientMsg::Request { req, op })?;
        let deadline = Instant::now() + self.reply_timeout;
        loop {
            match self.incoming.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(Incoming::Event(e)) => self.backlog.push_back(e),
                Ok(Incoming::Reply(r, result)) if r == req => return result.map_err(remote_error),
                Ok(Incoming::Reply(..)) => {} // for a request that timed out
                Err(RecvTimeoutError::Timeout) => {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "the session daemon did not answer"));
                }
                Err(RecvTimeoutError::Disconnected) => return Err(not_connected()),
            }
        }
    }

    /// Any request, with its reply (tests use it to see errors that the
    /// trait's fire-and-forget operations don't report).
    #[cfg(test)]
    pub(crate) fn request_for_tests(&mut self, op: Op) -> io::Result<Reply> {
        self.request(op)
    }

    /// Send `op` without waiting (no reply is asked for).
    fn post(&mut self, op: Op) -> io::Result<()> {
        self.send(&ClientMsg::Request { req: 0, op })
    }

    /// The next event from the channel, skipping stale replies.
    fn next_incoming(&mut self) -> Result<PtyEvent, TryRecvError> {
        loop {
            match self.incoming.try_recv()? {
                Incoming::Event(e) => return Ok(e),
                Incoming::Reply(..) => {}
            }
        }
    }
}

/// How often `connect_or_start` tries starting a daemon while it can't connect.
const RESTART_INTERVAL: Duration = Duration::from_millis(500);

/// Whether a failed connect means no daemon is serving the socket: nothing
/// there, a socket file nobody listens on (a new daemon replaces it), or a
/// daemon that hung up at once (it was exiting, e.g. idle).
fn no_daemon(e: &io::Error) -> bool {
    use io::ErrorKind::*;
    matches!(e.kind(), NotFound | ConnectionRefused | ConnectionAborted | ConnectionReset | BrokenPipe | UnexpectedEof)
}

/// Start the daemon and wait for its starter process, which exits once the
/// socket is bound (or binding failed).
fn start_daemon(program: &Path, socket: &Path, replay_bytes: usize, idle_exit: Option<Duration>) -> io::Result<()> {
    let out = Command::new(program)
        .arg("--pty-daemon")
        .arg("--socket")
        .arg(socket)
        .arg("--replay-bytes")
        .arg(replay_bytes.to_string())
        .arg("--idle-exit")
        .arg(idle_exit.map_or(0, |d| d.as_secs().max(1)).to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    match out.status.code() {
        Some(exit_code::OK) => Ok(()),
        // Another daemon got there first: connect to it.
        Some(exit_code::IN_USE) => Ok(()),
        _ => {
            let err = String::from_utf8_lossy(&out.stderr);
            Err(io::Error::other(format!("could not start the session daemon ({}): {}", out.status, err.trim())))
        }
    }
}

/// The reader thread: frames in, events and replies out.
fn read_loop(stream: UnixStream, tx: SyncSender<Incoming>, notifier: Notifier, connected: Arc<AtomicBool>) {
    let mut r = BufReader::new(stream);
    while let Ok(Some(msg)) = read_frame::<DaemonMsg>(&mut r) {
        let item = match msg {
            DaemonMsg::Output { id, data } => Incoming::Event(PtyEvent::Output(id, data)),
            DaemonMsg::Exited { id, status } => Incoming::Event(PtyEvent::Exited(id, status)),
            DaemonMsg::Reply { req, result } => Incoming::Reply(req, result),
            DaemonMsg::Hello { .. } => continue,
        };
        let is_event = matches!(item, Incoming::Event(_));
        if tx.send(item).is_err() {
            return; // the backend is gone
        }
        if is_event {
            notifier.notify();
        }
    }
    connected.store(false, Ordering::Release);
    // Let the UI tick so it can notice.
    notifier.notify();
}

impl Drop for DaemonBackend {
    fn drop(&mut self) {
        // Ends the reader; the daemon detaches us and the sessions live on.
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

impl PtyBackend for DaemonBackend {
    fn spawn(&mut self, spec: SpawnSpec) -> io::Result<SessionId> {
        match self.request(Op::Spawn(spec))? {
            Reply::Spawned(id) => Ok(id),
            other => Err(io::Error::new(io::ErrorKind::InvalidData, format!("unexpected reply {other:?}"))),
        }
    }

    fn attach(&mut self, id: SessionId, size: PtySize) -> io::Result<()> {
        self.request(Op::Attach { id, size }).map(|_| ())
    }

    fn detach(&mut self, id: SessionId) -> io::Result<()> {
        self.request(Op::Detach { id }).map(|_| ())
    }

    fn write(&mut self, id: SessionId, bytes: &[u8]) -> io::Result<()> {
        for chunk in bytes.chunks(MAX_WRITE_CHUNK) {
            self.post(Op::Write { id, data: chunk.to_vec() })?;
        }
        Ok(())
    }

    fn resize(&mut self, id: SessionId, size: PtySize) -> io::Result<()> {
        self.post(Op::Resize { id, size })
    }

    fn kill(&mut self, id: SessionId) -> io::Result<()> {
        self.request(Op::Kill { id })?;
        // Drop what was already queued for it: no events after a kill.
        self.backlog.retain(|e| !matches!(e, PtyEvent::Output(s, _) | PtyEvent::Exited(s, _) if *s == id));
        Ok(())
    }

    fn list(&mut self) -> io::Result<Vec<SessionInfo>> {
        match self.request(Op::List)? {
            Reply::Sessions(v) => Ok(v),
            other => Err(io::Error::new(io::ErrorKind::InvalidData, format!("unexpected reply {other:?}"))),
        }
    }

    fn try_recv(&mut self) -> Option<PtyEvent> {
        if let Some(e) = self.backlog.pop_front() {
            return Some(e);
        }
        match self.next_incoming() {
            Ok(e) => Some(e),
            Err(_) => {
                // Clear, then look once more: an event queued just before the
                // clear would otherwise wait for the next wake.
                self.notifier.clear();
                self.next_incoming().ok()
            }
        }
    }

    fn persistent(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicUsize;
    use std::thread::JoinHandle;

    use super::*;
    use crate::engine::pty::noop_waker;
    use crate::engine::testutil::{TIMEOUT, temp_dir, wait_until};

    /// A scripted daemon for one connection on a temporary socket.
    fn fake(script: impl FnOnce(UnixStream) + Send + 'static) -> (PathBuf, PathBuf, JoinHandle<()>) {
        let dir = temp_dir("fake");
        let socket = dir.join("f.sock");
        let l = UnixListener::bind(&socket).unwrap();
        let t = thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            s.set_read_timeout(Some(TIMEOUT)).unwrap();
            script(s)
        });
        (dir, socket, t)
    }

    /// Read the client's greeting and answer it.
    fn greet(s: &mut UnixStream) {
        assert!(matches!(
            read_frame::<ClientMsg>(s).unwrap(),
            Some(ClientMsg::Hello { version: PROTOCOL_VERSION, .. })
        ));
        write_frame(s, &DaemonMsg::Hello { version: PROTOCOL_VERSION, daemon_pid: 4242 }).unwrap();
    }

    fn next_req(s: &mut UnixStream) -> (u64, Op) {
        match read_frame::<ClientMsg>(s).unwrap() {
            Some(ClientMsg::Request { req, op }) => (req, op),
            other => panic!("expected a request, got {other:?}"),
        }
    }

    fn counting_waker() -> (Waker, Arc<AtomicUsize>) {
        let wakes = Arc::new(AtomicUsize::new(0));
        let w = wakes.clone();
        let waker: Waker = Arc::new(move || {
            w.fetch_add(1, Ordering::SeqCst);
        });
        (waker, wakes)
    }

    #[test]
    fn handshake_problems_are_errors() {
        let (dir, socket, t) = fake(|mut s| {
            let _ = read_frame::<ClientMsg>(&mut s);
            write_frame(&mut s, &DaemonMsg::Hello { version: PROTOCOL_VERSION + 1, daemon_pid: 1 }).unwrap();
        });
        let e = DaemonBackend::connect(&socket, noop_waker()).err().unwrap();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        assert!(e.to_string().contains("protocol"), "{e}");
        t.join().unwrap();
        std::fs::remove_dir_all(dir).unwrap();

        let (dir, socket, t) = fake(|mut s| {
            let _ = read_frame::<ClientMsg>(&mut s);
        });
        assert_eq!(
            DaemonBackend::connect(&socket, noop_waker()).err().unwrap().kind(),
            io::ErrorKind::ConnectionAborted
        );
        t.join().unwrap();
        std::fs::remove_dir_all(dir).unwrap();

        let (dir, socket, t) = fake(|mut s| {
            let _ = read_frame::<ClientMsg>(&mut s);
            write_frame(&mut s, &DaemonMsg::Output { id: SessionId(1), data: vec![] }).unwrap();
        });
        assert_eq!(DaemonBackend::connect(&socket, noop_waker()).err().unwrap().kind(), io::ErrorKind::InvalidData);
        t.join().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn replies_time_out_and_late_replies_are_ignored() {
        let (dir, socket, t) = fake(|mut s| {
            greet(&mut s);
            let (r1, _) = next_req(&mut s); // not answered in time
            let (r2, op) = next_req(&mut s);
            assert_eq!(op, Op::List);
            write_frame(&mut s, &DaemonMsg::Reply { req: r1, result: Ok(Reply::Ok) }).unwrap();
            write_frame(&mut s, &DaemonMsg::Output { id: SessionId(5), data: b"meanwhile".to_vec() }).unwrap();
            write_frame(&mut s, &DaemonMsg::Reply { req: r2, result: Ok(Reply::Sessions(Vec::new())) }).unwrap();
            let (r3, _) = next_req(&mut s);
            write_frame(&mut s, &DaemonMsg::Reply { req: r3, result: Err("no session 9".into()) }).unwrap();
            let (r4, _) = next_req(&mut s);
            write_frame(&mut s, &DaemonMsg::Reply { req: r4, result: Err("disk on fire".into()) }).unwrap();
            let (r5, _) = next_req(&mut s);
            write_frame(&mut s, &DaemonMsg::Reply { req: r5, result: Ok(Reply::Ok) }).unwrap();
            let _ = read_frame::<ClientMsg>(&mut s);
        });
        let mut c = DaemonBackend::connect(&socket, noop_waker()).unwrap();
        assert_eq!(c.daemon_pid(), 4242);
        c.set_reply_timeout(Duration::from_millis(50));
        assert_eq!(c.list().unwrap_err().kind(), io::ErrorKind::TimedOut);
        c.set_reply_timeout(TIMEOUT);
        assert_eq!(c.list().unwrap(), []);
        // The event that came with it is kept for try_recv.
        assert_eq!(c.try_recv(), Some(PtyEvent::Output(SessionId(5), b"meanwhile".to_vec())));
        assert_eq!(c.try_recv(), None);
        assert_eq!(c.kill(SessionId(9)).unwrap_err().kind(), io::ErrorKind::NotFound);
        let e = c.detach(SessionId(9)).unwrap_err();
        assert_eq!((e.kind(), e.to_string()), (io::ErrorKind::Other, "disk on fire".to_string()));
        // A reply of the wrong shape.
        assert_eq!(c.spawn(SpawnSpec::default()).unwrap_err().kind(), io::ErrorKind::InvalidData);
        drop(c);
        t.join().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A daemon that stops reading (hung, stopped) can't freeze the editor:
    /// writes give up and the connection is dropped.
    #[test]
    fn writes_to_a_daemon_that_stopped_reading_time_out() {
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let (dir, socket, t) = fake(move |mut s| {
            greet(&mut s);
            let _ = done_rx.recv_timeout(TIMEOUT); // reads nothing more
        });
        let mut c = DaemonBackend::connect(&socket, noop_waker()).unwrap();
        c.set_reply_timeout(Duration::from_millis(100));
        let chunk = vec![b'x'; MAX_WRITE_CHUNK];
        let start = Instant::now();
        let e = loop {
            match c.write(SessionId(1), &chunk) {
                Ok(()) => assert!(start.elapsed() < TIMEOUT, "writes never gave up"),
                Err(e) => break e,
            }
        };
        assert_eq!(e.kind(), io::ErrorKind::NotConnected);
        assert!(!c.is_connected());
        assert_eq!(c.write(SessionId(1), b"x").unwrap_err().kind(), io::ErrorKind::NotConnected);
        done_tx.send(()).unwrap();
        t.join().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_lost_connection_fails_later_calls_and_wakes_the_ui() {
        let (dir, socket, t) = fake(|mut s| {
            greet(&mut s);
            write_frame(&mut s, &DaemonMsg::Output { id: SessionId(1), data: b"last".to_vec() }).unwrap();
        });
        let (waker, wakes) = counting_waker();
        let mut c = DaemonBackend::connect(&socket, waker).unwrap();
        t.join().unwrap();
        wait_until("the disconnect to be noticed", TIMEOUT, || !c.is_connected());
        assert!(wakes.load(Ordering::SeqCst) >= 1);
        // What arrived before is still delivered; then nothing.
        assert_eq!(c.try_recv(), Some(PtyEvent::Output(SessionId(1), b"last".to_vec())));
        assert_eq!(c.try_recv(), None);
        for e in [
            c.spawn(SpawnSpec::default()).unwrap_err(),
            c.write(SessionId(1), b"x").unwrap_err(),
            c.resize(SessionId(1), PtySize::default()).unwrap_err(),
            c.list().unwrap_err(),
        ] {
            assert_eq!(e.kind(), io::ErrorKind::NotConnected, "{e}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_waker_is_called_once_per_batch() {
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let (dir, socket, t) = fake(move |mut s| {
            greet(&mut s);
            for i in 0..5u8 {
                write_frame(&mut s, &DaemonMsg::Output { id: SessionId(1), data: vec![i] }).unwrap();
            }
            go_rx.recv().unwrap();
            write_frame(&mut s, &DaemonMsg::Exited { id: SessionId(1), status: Default::default() }).unwrap();
            let _ = read_frame::<ClientMsg>(&mut s);
        });
        let (waker, wakes) = counting_waker();
        let mut c = DaemonBackend::connect(&socket, waker).unwrap();
        let mut got = Vec::new();
        wait_until("five outputs", TIMEOUT, || {
            while let Some(e) = c.try_recv() {
                got.push(e);
            }
            got.len() == 5
        });
        assert!(wakes.load(Ordering::SeqCst) <= 5);
        let before = wakes.load(Ordering::SeqCst);
        go_tx.send(()).unwrap();
        wait_until("a wake for the exit", TIMEOUT, || wakes.load(Ordering::SeqCst) == before + 1);
        assert_eq!(c.try_recv(), Some(PtyEvent::Exited(SessionId(1), Default::default())));
        drop(c);
        t.join().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn connect_or_start_uses_a_running_daemon_and_reports_start_failures() {
        // A daemon is listening: the program is not run.
        let (dir, socket, t) = fake(|mut s| {
            greet(&mut s);
            let _ = read_frame::<ClientMsg>(&mut s);
        });
        let c = DaemonBackend::connect_or_start_with(Path::new("/nonexistent/ymacs"), &socket, 10, None, noop_waker())
            .unwrap();
        drop(c);
        t.join().unwrap();

        // Nobody is listening and the program can't be run.
        let missing = dir.join("none.sock");
        let e = DaemonBackend::connect_or_start_with(Path::new("/nonexistent/ymacs"), &missing, 10, None, noop_waker())
            .err()
            .unwrap();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
        // It runs but fails.
        let e = DaemonBackend::connect_or_start_with(Path::new("/usr/bin/false"), &missing, 10, None, noop_waker())
            .err()
            .unwrap();
        assert!(e.to_string().contains("could not start the session daemon"), "{e}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
