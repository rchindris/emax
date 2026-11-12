//! The session daemon (`emax --pty-daemon`): owns PTY sessions so they
//! survive the editor, keeps the last `replay_bytes` of each session's output
//! in a ring buffer, and serves clients over a Unix socket (see
//! [`super::protocol`]).
//!
//! Threads: one accepts connections; each connection has a reader (frames in)
//! and a writer (frames out, from a queue). Everything they receive, plus the
//! PTY backend's wake-ups, goes through one channel to the main loop, which
//! alone owns the sessions and the backend. So "send the replay, then go
//! live" on attach is atomic: no output can slip in between.
//!
//! Several clients: every client attached to a session gets all of its
//! output (broadcast); the last resize wins. A client that disconnects is
//! detached from its sessions; the processes keep running and their output
//! keeps going into the ring.
//!
//! Flow control: if a client stops reading, its queue grows; past
//! [`HIGH_WATER`] the daemon stops reading from the backend, whose bounded
//! channel then fills, so the programs block on output instead of the
//! daemon's memory growing without bound. Reading resumes when the client
//! catches up (or disconnects).
//!
//! The socket's directory is created with mode 0700; a directory owned by
//! another user (other than root, e.g. `/tmp`) is refused. A lock file next
//! to the socket makes sure only one daemon serves it; a socket file left by
//! a daemon that died is replaced.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use super::protocol::{
    ClientMsg, DaemonMsg, MAX_OUTPUT_CHUNK, Op, PROTOCOL_VERSION, Reply, encode_frame, read_frame,
};
use crate::engine::pty::local::LocalBackend;
use crate::engine::pty::{ExitStatus, PtyBackend, PtyEvent, PtySize, SessionId, SessionInfo, SpawnSpec, Waker};

/// How a daemon runs.
#[derive(Clone, Debug, PartialEq)]
pub struct DaemonOptions {
    pub socket: PathBuf,
    /// Output kept per session for replay on attach.
    pub replay_bytes: usize,
    /// Exit when there have been no sessions and no clients for this long
    /// (`None`: never).
    pub idle_exit: Option<Duration>,
}

/// The replay buffer size when `--replay-bytes` is not given.
pub const DEFAULT_REPLAY_BYTES: usize = 2 << 20;

/// Once a client has this many bytes queued, the daemon stops reading
/// program output until it catches up.
pub const HIGH_WATER: usize = 8 << 20;

/// Backend events handled before the main loop looks at its channel again.
const DRAIN_BATCH: usize = 256;

/// Exit codes of `emax --pty-daemon`.
pub mod exit_code {
    pub const OK: i32 = 0;
    pub const ERROR: i32 = 1;
    pub const USAGE: i32 = 2;
    /// Another daemon already serves the socket.
    pub const IN_USE: i32 = 3;
}

// ------------------------------------------------------------ replay ring

/// The last `cap` bytes of a session's output.
#[derive(Clone, Debug, Default)]
pub struct Ring {
    buf: VecDeque<u8>,
    cap: usize,
}

impl Ring {
    pub fn new(cap: usize) -> Self {
        Self { buf: VecDeque::new(), cap }
    }

    /// Append `data`, dropping the oldest bytes beyond the capacity.
    pub fn push(&mut self, data: &[u8]) {
        if data.len() >= self.cap {
            self.buf.clear();
            self.buf.extend(&data[data.len() - self.cap..]);
            return;
        }
        let over = (self.buf.len() + data.len()).saturating_sub(self.cap);
        self.buf.drain(..over);
        self.buf.extend(data);
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// The contents in chunks of at most `max` bytes (oldest first).
    pub fn chunks(&self, max: usize) -> Vec<Vec<u8>> {
        let (a, b) = self.buf.as_slices();
        let all: Vec<u8> = [a, b].concat();
        all.chunks(max.max(1)).map(<[u8]>::to_vec).collect()
    }
}

// --------------------------------------------------------------- binding

/// A bound daemon socket: the listener plus the lock that makes it ours.
/// Dropping it removes the socket file.
pub struct Bound {
    listener: UnixListener,
    path: PathBuf,
    _lock: File,
}

impl Drop for Bound {
    fn drop(&mut self) {
        // Still ours: we hold the lock, so nobody else can have bound it.
        let _ = fs::remove_file(&self.path);
    }
}

/// Bind the daemon socket at `path`: create its directory (0700) if needed,
/// take the lock file next to it, replace a stale socket and refuse a live
/// one (`AddrInUse`).
pub fn bind(path: &Path) -> io::Result<Bound> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    prepare_dir(dir)?;
    let lock = take_lock(&path.with_extension("lock"))?;
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_socket() => {
            if UnixStream::connect(path).is_ok() {
                return Err(io::Error::new(io::ErrorKind::AddrInUse, format!("a daemon is already listening on {}", path.display())));
            }
            // Nobody listens: left behind by a daemon that died.
            fs::remove_file(path)?;
        }
        Ok(_) => {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} exists and is not a socket", path.display())));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(Bound { listener, path: path.to_path_buf(), _lock: lock })
}

/// Create `dir` private to this user, or check that an existing one is not
/// someone else's (root's is fine: e.g. `/tmp`, which is sticky).
fn prepare_dir(dir: &Path) -> io::Result<()> {
    match fs::metadata(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
            // The umask may have taken bits away, never added: make it exact.
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
        }
        Err(e) => Err(e),
        Ok(m) if !m.is_dir() => Err(io::Error::new(io::ErrorKind::NotADirectory, format!("{} is not a directory", dir.display()))),
        Ok(m) => {
            // SAFETY: getuid has no preconditions and cannot fail.
            let uid = unsafe { libc::getuid() };
            if m.uid() != uid && m.uid() != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("{} belongs to another user", dir.display()),
                ));
            }
            // Ours but writable by others and not sticky: they could replace the socket.
            if m.uid() == uid && m.mode() & 0o022 != 0 && m.mode() & 0o1000 == 0 {
                fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
            }
            Ok(())
        }
    }
}

/// An exclusive `flock` on `path`, held as long as the file is open
/// (inherited across `fork`, so the daemonized child keeps it).
fn take_lock(path: &Path) -> io::Result<File> {
    let f = OpenOptions::new().create(true).truncate(false).write(true).mode(0o600).open(path)?;
    // SAFETY: flock on a valid descriptor we own.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let e = io::Error::last_os_error();
        return Err(if e.kind() == io::ErrorKind::WouldBlock {
            io::Error::new(io::ErrorKind::AddrInUse, format!("another daemon holds {}", path.display()))
        } else {
            e
        });
    }
    Ok(f)
}

// ---------------------------------------------------------------- serving

/// Serve on `opts.socket` with processes run by a [`LocalBackend`], until
/// shut down or idle. Blocks.
pub fn run(opts: DaemonOptions) -> io::Result<()> {
    run_with(opts, |w| Box::new(LocalBackend::new(w)))
}

/// [`run`] with the backend made by `make_backend` (given the waker that
/// wakes the main loop). Tests use a mock backend.
pub fn run_with(opts: DaemonOptions, make_backend: impl FnOnce(Waker) -> Box<dyn PtyBackend>) -> io::Result<()> {
    let bound = bind(&opts.socket)?;
    serve(bound, &opts, make_backend)
}

/// Serve on an already bound socket (`main` binds before detaching, so bind
/// errors reach the starter). Blocks.
pub fn serve(bound: Bound, opts: &DaemonOptions, make_backend: impl FnOnce(Waker) -> Box<dyn PtyBackend>) -> io::Result<()> {
    let (tx, rx) = mpsc::channel();
    let wake = tx.clone();
    let backend = make_backend(Arc::new(move || {
        let _ = wake.send(Input::Wake);
    }));
    let stop = Arc::new(AtomicBool::new(false));
    let listener = bound.listener.try_clone()?;
    let acceptor = {
        let (tx, stop) = (tx.clone(), stop.clone());
        thread::Builder::new().name("ptyd-accept".into()).spawn(move || accept_loop(listener, tx, stop))?
    };
    let mut d = Daemon {
        backend,
        sessions: BTreeMap::new(),
        conns: HashMap::new(),
        replay_bytes: opts.replay_bytes,
        tx,
        more: true,
        stop: false,
    };
    d.main_loop(&rx, opts.idle_exit);
    // Kill what's left (shutdown already did) and close every connection:
    // their writers flush what is queued, then hang up.
    drop(d);
    stop.store(true, Ordering::SeqCst);
    let _ = UnixStream::connect(&bound.path); // wake the acceptor
    let _ = acceptor.join();
    drop(bound);
    Ok(())
}

/// What the main loop is told.
enum Input {
    Connected(u64, Conn),
    Msg(u64, ClientMsg),
    Gone(u64),
    /// The backend has events (or a client's queue drained below the high water).
    Wake,
}

/// The main loop's side of a connection.
struct Conn {
    tx: Sender<Vec<u8>>,
    /// Bytes queued for the writer.
    queued: Arc<AtomicUsize>,
    greeted: bool,
}

struct Session {
    spec: SpawnSpec,
    size: PtySize,
    ring: Ring,
    exited: Option<ExitStatus>,
    /// Connections attached to it.
    clients: BTreeSet<u64>,
}

struct Daemon {
    backend: Box<dyn PtyBackend>,
    sessions: BTreeMap<SessionId, Session>,
    conns: HashMap<u64, Conn>,
    replay_bytes: usize,
    /// For waking itself (to continue a long drain after other inputs).
    tx: Sender<Input>,
    /// The backend may have events waiting.
    more: bool,
    stop: bool,
}

fn accept_loop(listener: UnixListener, tx: Sender<Input>, stop: Arc<AtomicBool>) {
    let mut next = 0u64;
    for stream in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let Ok(stream) = stream else { continue };
        next += 1;
        if start_conn(next, stream, &tx).is_err() {
            continue;
        }
    }
}

/// Register connection `id` with the main loop and start its threads.
fn start_conn(id: u64, stream: UnixStream, tx: &Sender<Input>) -> io::Result<()> {
    let reader = stream.try_clone()?;
    let (ftx, frx) = mpsc::channel();
    let queued = Arc::new(AtomicUsize::new(0));
    // Registered before its reader starts, so the main loop sees it first.
    if tx.send(Input::Connected(id, Conn { tx: ftx, queued: queued.clone(), greeted: false })).is_err() {
        return Ok(());
    }
    let wake = tx.clone();
    thread::Builder::new().name("ptyd-write".into()).spawn(move || write_loop(stream, frx, queued, wake))?;
    let tx = tx.clone();
    thread::Builder::new().name("ptyd-read".into()).spawn(move || {
        let mut r = BufReader::new(reader);
        while let Ok(Some(msg)) = read_frame::<ClientMsg>(&mut r) {
            if tx.send(Input::Msg(id, msg)).is_err() {
                return;
            }
        }
        let _ = tx.send(Input::Gone(id));
    })?;
    Ok(())
}

/// Write queued frames until the main loop drops the connection (then hang
/// up, which also ends the reader) or the client goes away.
fn write_loop(mut stream: UnixStream, frames: Receiver<Vec<u8>>, queued: Arc<AtomicUsize>, wake: Sender<Input>) {
    for frame in frames {
        let ok = stream.write_all(&frame).is_ok();
        let before = queued.fetch_sub(frame.len(), Ordering::AcqRel);
        if before >= HIGH_WATER && before - frame.len() < HIGH_WATER {
            let _ = wake.send(Input::Wake);
        }
        if !ok {
            break;
        }
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

fn no_session(id: SessionId) -> String {
    format!("no session {}", id.0)
}

impl Daemon {
    fn main_loop(&mut self, rx: &Receiver<Input>, idle_exit: Option<Duration>) {
        let mut idle_since = Some(Instant::now());
        while !self.stop {
            let input = match (idle_exit, idle_since) {
                (Some(limit), Some(since)) => match rx.recv_timeout((since + limit).saturating_duration_since(Instant::now())) {
                    Ok(i) => i,
                    Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => return,
                },
                _ => match rx.recv() {
                    Ok(i) => i,
                    Err(_) => return,
                },
            };
            self.handle(input);
            self.drain();
            if self.sessions.is_empty() && self.conns.is_empty() {
                idle_since.get_or_insert_with(Instant::now);
            } else {
                idle_since = None;
            }
        }
    }

    fn handle(&mut self, input: Input) {
        match input {
            Input::Connected(id, conn) => {
                self.conns.insert(id, conn);
            }
            Input::Gone(id) => self.drop_conn(id),
            Input::Wake => self.more = true,
            Input::Msg(id, msg) => self.message(id, msg),
        }
    }

    /// Forget connection `id`: detach it from its sessions and hang up.
    fn drop_conn(&mut self, id: u64) {
        self.conns.remove(&id);
        for s in self.sessions.values_mut() {
            s.clients.remove(&id);
        }
    }

    fn message(&mut self, conn: u64, msg: ClientMsg) {
        let Some(c) = self.conns.get_mut(&conn) else { return };
        match msg {
            ClientMsg::Hello { version, .. } if !c.greeted => {
                c.greeted = true;
                self.send(conn, &DaemonMsg::Hello { version: PROTOCOL_VERSION, daemon_pid: std::process::id() });
                if version != PROTOCOL_VERSION {
                    // It learns our version from the Hello, then we hang up.
                    self.drop_conn(conn);
                }
            }
            ClientMsg::Request { req, op } if c.greeted => {
                let result = self.op(conn, op);
                if req != 0 {
                    self.send(conn, &DaemonMsg::Reply { req, result });
                }
            }
            // A request before the handshake, or a second Hello.
            _ => self.drop_conn(conn),
        }
    }

    /// Queue `msg` for connection `conn`.
    fn send(&mut self, conn: u64, msg: &DaemonMsg) {
        let Some(c) = self.conns.get(&conn) else { return };
        let Ok(frame) = encode_frame(msg) else { return };
        let n = frame.len();
        c.queued.fetch_add(n, Ordering::AcqRel);
        if c.tx.send(frame).is_err() {
            c.queued.fetch_sub(n, Ordering::AcqRel);
        }
    }

    /// Send `data` as `Output` frames of at most `MAX_OUTPUT_CHUNK` bytes.
    fn send_output(&mut self, conn: u64, id: SessionId, data: &[u8]) {
        for chunk in data.chunks(MAX_OUTPUT_CHUNK) {
            self.send(conn, &DaemonMsg::Output { id, data: chunk.to_vec() });
        }
    }

    fn op(&mut self, conn: u64, op: Op) -> Result<Reply, String> {
        match op {
            Op::Spawn(spec) => self.spawn(conn, spec),
            Op::Attach { id, size } => self.attach(conn, id, size),
            Op::Detach { id } => {
                self.sessions.get_mut(&id).ok_or_else(|| no_session(id))?.clients.remove(&conn);
                Ok(Reply::Ok)
            }
            Op::Write { id, data } => {
                let s = self.sessions.get(&id).ok_or_else(|| no_session(id))?;
                if s.exited.is_some() {
                    return Err(format!("session {}: the process has exited", id.0));
                }
                self.backend.write(id, &data).map(|_| Reply::Ok).map_err(|e| e.to_string())
            }
            Op::Resize { id, size } => {
                let s = self.sessions.get_mut(&id).ok_or_else(|| no_session(id))?;
                s.size = size;
                if s.exited.is_some() {
                    return Ok(Reply::Ok);
                }
                self.backend.resize(id, size).map(|_| Reply::Ok).map_err(|e| e.to_string())
            }
            Op::Kill { id } => self.kill(conn, id),
            Op::List => Ok(Reply::Sessions(self.list())),
            Op::Shutdown => {
                for id in self.sessions.keys().copied().collect::<Vec<_>>() {
                    let _ = self.kill(conn, id);
                }
                self.stop = true;
                Ok(Reply::Ok)
            }
        }
    }

    fn spawn(&mut self, conn: u64, spec: SpawnSpec) -> Result<Reply, String> {
        let id = self.backend.spawn(spec.clone()).map_err(|e| format!("{}: {e}", spec.program))?;
        let size = spec.size;
        let mut clients = BTreeSet::new();
        clients.insert(conn);
        self.sessions.insert(id, Session { spec, size, ring: Ring::new(self.replay_bytes), exited: None, clients });
        Ok(Reply::Spawned(id))
    }

    /// Replay the ring (and the exit), then attach: all before the reply and
    /// before any live output, since nothing else runs meanwhile.
    fn attach(&mut self, conn: u64, id: SessionId, size: PtySize) -> Result<Reply, String> {
        let s = self.sessions.get_mut(&id).ok_or_else(|| no_session(id))?;
        s.clients.insert(conn);
        s.size = size;
        let replay = s.ring.chunks(MAX_OUTPUT_CHUNK);
        let exited = s.exited.clone();
        for chunk in replay {
            self.send(conn, &DaemonMsg::Output { id, data: chunk });
        }
        match exited {
            Some(status) => self.send(conn, &DaemonMsg::Exited { id, status }),
            // Full-screen programs redraw on the size change.
            None => {
                let _ = self.backend.resize(id, size);
            }
        }
        Ok(Reply::Ok)
    }

    /// Kill a session; other clients attached to it see it exit.
    fn kill(&mut self, conn: u64, id: SessionId) -> Result<Reply, String> {
        let s = self.sessions.remove(&id).ok_or_else(|| no_session(id))?;
        // An exited process may already be gone from the backend.
        let _ = self.backend.kill(id);
        if s.exited.is_none() {
            let status = ExitStatus { code: 1, signal: Some("Killed".into()) };
            for c in s.clients.into_iter().filter(|&c| c != conn) {
                self.send(c, &DaemonMsg::Exited { id, status: status.clone() });
            }
        }
        Ok(Reply::Ok)
    }

    fn list(&mut self) -> Vec<SessionInfo> {
        let pids: HashMap<SessionId, Option<u32>> =
            self.backend.list().unwrap_or_default().into_iter().map(|i| (i.id, i.pid)).collect();
        self.sessions
            .iter()
            .map(|(id, s)| SessionInfo {
                id: *id,
                program: s.spec.program.clone(),
                args: s.spec.args.clone(),
                cwd: s.spec.cwd.clone(),
                pid: pids.get(id).copied().flatten(),
                size: s.size,
                exited: s.exited.clone(),
            })
            .collect()
    }

    /// Some client is too far behind: stop reading program output for now.
    fn throttled(&self) -> bool {
        self.conns.values().any(|c| c.queued.load(Ordering::Acquire) >= HIGH_WATER)
    }

    /// Move a batch of backend events into the rings and out to clients.
    fn drain(&mut self) {
        if !self.more || self.throttled() {
            return;
        }
        for _ in 0..DRAIN_BATCH {
            let Some(e) = self.backend.try_recv() else {
                self.more = false;
                return;
            };
            self.event(e);
            if self.throttled() {
                return; // the writer wakes us when it catches up
            }
        }
        // More may be waiting: continue after whatever else is queued.
        let _ = self.tx.send(Input::Wake);
    }

    fn event(&mut self, e: PtyEvent) {
        match e {
            PtyEvent::Output(id, data) => {
                let Some(s) = self.sessions.get_mut(&id) else { return };
                s.ring.push(&data);
                for c in s.clients.clone() {
                    self.send_output(c, id, &data);
                }
            }
            PtyEvent::Exited(id, status) => {
                let Some(s) = self.sessions.get_mut(&id) else { return };
                s.exited = Some(status.clone());
                for c in s.clients.clone() {
                    self.send(c, &DaemonMsg::Exited { id, status: status.clone() });
                }
            }
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        for id in self.sessions.keys().copied().collect::<Vec<_>>() {
            if self.sessions[&id].exited.is_none() {
                let _ = self.backend.kill(id);
            }
        }
    }
}

// ------------------------------------------------------------ entry point

/// Parsed `--pty-daemon` arguments.
#[derive(Clone, Debug, PartialEq)]
pub struct Cli {
    pub opts: DaemonOptions,
    pub foreground: bool,
}

/// Parse the arguments after `--pty-daemon`:
/// `[--socket PATH] [--replay-bytes N] [--idle-exit SECS] [--foreground]`
/// (`--idle-exit 0`, the default, means never).
pub fn parse_args(args: &[String]) -> Result<Cli, String> {
    let mut cli = Cli {
        opts: DaemonOptions { socket: super::default_socket_path(), replay_bytes: DEFAULT_REPLAY_BYTES, idle_exit: None },
        foreground: false,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{a} needs a value"));
        match a.as_str() {
            "--socket" => cli.opts.socket = PathBuf::from(value()?),
            "--replay-bytes" => {
                let v = value()?;
                cli.opts.replay_bytes = v.parse().map_err(|_| format!("--replay-bytes: not a number: {v}"))?;
            }
            "--idle-exit" => {
                let v = value()?;
                let secs: u64 = v.parse().map_err(|_| format!("--idle-exit: not a number of seconds: {v}"))?;
                cli.opts.idle_exit = (secs > 0).then(|| Duration::from_secs(secs));
            }
            "--foreground" => cli.foreground = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(cli)
}

/// The `emax --pty-daemon [--socket PATH] [--replay-bytes N]
/// [--idle-exit SECS] [--foreground]` entry point: parse `args`
/// (everything after `--pty-daemon`), bind the socket, detach from the
/// terminal (unless `--foreground`) and serve. Returns the process exit code
/// (see [`exit_code`]); once detached, the starter only sees the bind result.
pub fn main(args: &[String]) -> i32 {
    let cli = match parse_args(args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("emax --pty-daemon: {e}");
            return exit_code::USAGE;
        }
    };
    let bound = match bind(&cli.opts.socket) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("emax --pty-daemon: {}: {e}", cli.opts.socket.display());
            return if e.kind() == io::ErrorKind::AddrInUse { exit_code::IN_USE } else { exit_code::ERROR };
        }
    };
    if !cli.foreground {
        let log = cli.opts.socket.with_extension("log");
        if let Err(e) = daemonize(&log) {
            eprintln!("emax --pty-daemon: detaching: {e}");
            return exit_code::ERROR;
        }
    }
    match serve(bound, &cli.opts, |w| Box::new(LocalBackend::new(w))) {
        Ok(()) => exit_code::OK,
        Err(e) => {
            eprintln!("emax --pty-daemon: {e}");
            exit_code::ERROR
        }
    }
}

/// Fork; the parent exits at once (so whoever started us can wait for it),
/// the child leaves the terminal's session, moves to `$HOME` and sends its
/// output to `log`. Must run before any thread is started.
fn daemonize(log: &Path) -> io::Result<()> {
    // SAFETY: called from `main` before any other thread exists.
    match unsafe { libc::fork() } {
        -1 => return Err(io::Error::last_os_error()),
        0 => {}
        // SAFETY: _exit skips destructors (the child owns the socket now).
        _ => unsafe { libc::_exit(exit_code::OK) },
    }
    // SAFETY: setsid has no preconditions; the forked child is never a group leader.
    unsafe { libc::setsid() };
    let _ = std::env::set_current_dir(std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/".into()));
    let null = File::open("/dev/null")?;
    let out = OpenOptions::new().create(true).append(true).mode(0o600).open(log).or_else(|_| OpenOptions::new().write(true).open("/dev/null"))?;
    // SAFETY: dup2 onto the standard descriptors with valid open files.
    unsafe {
        libc::dup2(null.as_raw_fd(), 0);
        libc::dup2(out.as_raw_fd(), 1);
        libc::dup2(out.as_raw_fd(), 2);
    }
    Ok(())
}
