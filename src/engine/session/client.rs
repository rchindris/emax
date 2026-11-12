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
        stream.set_read_timeout(None)?;
        let notifier = Notifier::new(waker);
        let connected = Arc::new(AtomicBool::new(true));
        let (tx, incoming) = mpsc::sync_channel(CHANNEL_DEPTH);
        let reader = stream.try_clone()?;
        {
            let (notifier, connected) = (notifier.clone(), connected.clone());
            thread::Builder::new().name("ptyd-client".into()).spawn(move || read_loop(reader, tx, notifier, connected))?;
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
    pub fn connect_or_start(socket: &Path, replay_bytes: usize, idle_exit: Option<Duration>, waker: Waker) -> io::Result<Self> {
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
        match Self::connect(socket, waker.clone()) {
            Ok(b) => return Ok(b),
            // Nothing there, or a socket file nobody listens on (the daemon
            // replaces it): start one.
            Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused) => {}
            Err(e) => return Err(e),
        }
        start_daemon(program, socket, replay_bytes, idle_exit)?;
        let deadline = Instant::now() + START_TIMEOUT;
        loop {
            match Self::connect(socket, waker.clone()) {
                Ok(b) => return Ok(b),
                Err(e) if Instant::now() >= deadline => return Err(e),
                Err(_) => thread::sleep(Duration::from_millis(10)),
            }
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

    /// How long requests wait for a reply (tests shorten it).
    pub fn set_reply_timeout(&mut self, t: Duration) {
        self.reply_timeout = t;
    }

    fn send(&mut self, msg: &ClientMsg) -> io::Result<()> {
        if !self.is_connected() {
            return Err(not_connected());
        }
        write_frame(&mut self.stream, msg).map_err(|e| {
            self.connected.store(false, Ordering::Release);
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
