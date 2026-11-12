//! Processes that are children of this process, on PTYs from `portable-pty`.
//!
//! Each session has three threads:
//! - a **reader** that waits on the PTY master and turns what it reads into
//!   [`PtyEvent::Output`] chunks (each read fills up to 64 KiB before it is
//!   sent, so a flood becomes a few large events, never one per byte);
//! - a **writer** that takes input queued by [`PtyBackend::write`] and writes
//!   it to the master, so the caller never blocks on a child that isn't
//!   reading its input;
//! - a **waiter** that blocks in `waitpid` (so no zombies) and then tells the
//!   reader to finish: it drains what is left and sends
//!   [`PtyEvent::Exited`], always after all of that session's output.
//!
//! Events go through one **bounded** channel per backend: when the consumer
//! falls behind, the reader blocks, the kernel's PTY buffer fills and the
//! child blocks writing — memory stays bounded and nothing is lost. The
//! [`Notifier`] wakes the consumer once per batch.
//!
//! The reader and writer wait with `select` on the master *and* a pipe the
//! waiter fires once the process is reaped, so they always finish even if
//! a background grandchild keeps the terminal open. [`PtyBackend::kill`] and
//! dropping the backend signal the process group (SIGHUP, then SIGKILL after
//! a grace period); the reader keeps reading (and discarding) meanwhile,
//! because on macOS a process can't finish exiting while its terminal has
//! unread output. Dropping the backend joins every thread, leaving no
//! threads or zombies behind.

use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, MasterPty, native_pty_system};

use super::sys::{self, Cancel, Ready};
use super::{ExitStatus, Notifier, PtyBackend, PtyEvent, PtySize, SessionId, SessionInfo, SpawnSpec, Waker};

/// Bytes per read (and so the largest `Output` chunk).
const READ_CHUNK: usize = 64 * 1024;
/// Events the channel holds before readers block (bounds memory to about
/// `EVENT_QUEUE * READ_CHUNK`).
pub const EVENT_QUEUE: usize = 256;
/// How much a reader still drains once its process has exited (a background
/// grandchild could otherwise keep it busy forever).
const DRAIN_LIMIT: usize = 4 << 20;
/// How long a killed process gets to exit after SIGHUP before SIGKILL.
pub const DEFAULT_KILL_GRACE: Duration = Duration::from_secs(1);

/// Environment every session gets unless its spec sets these names.
const DEFAULT_ENV: [(&str, &str); 2] = [("TERM", "xterm-256color"), ("COLORTERM", "truecolor")];

pub struct LocalBackend {
    sessions: BTreeMap<SessionId, Session>,
    next_id: u64,
    /// Senders are cloned into reader threads.
    tx: SyncSender<PtyEvent>,
    /// `None` only while dropping (so blocked readers fail and finish).
    rx: Option<Receiver<PtyEvent>>,
    /// Events produced on this thread (an `Exited` reported by `attach`);
    /// delivered after the channel is empty.
    local: VecDeque<PtyEvent>,
    notifier: Notifier,
    /// Events sent but not yet received (to observe backpressure).
    queued: Arc<AtomicUsize>,
    /// Threads of killed sessions, joined once finished (or on drop).
    dying: Vec<JoinHandle<()>>,
    /// Live I/O threads of this backend (for leak checks).
    live_threads: Arc<AtomicUsize>,
    kill_grace: Duration,
}

/// What a session's threads and the backend share.
struct Shared {
    state: Mutex<State>,
    /// Signalled when the process exits, the session is killed, or its
    /// output is resumed.
    changed: Condvar,
    /// Fired by the waiter once the process is reaped (stops reader and writer).
    cancel: Cancel,
    /// The child's pid; 0 until it has been spawned.
    pid: AtomicU32,
    /// Bytes read from the PTY so far (attached or not).
    bytes_read: AtomicUsize,
}

#[derive(Default)]
struct State {
    attached: bool,
    killed: bool,
    /// Output held back (`PtyBackend::pause_output`): the reader waits.
    paused: bool,
    /// Set by the waiter when the process has been reaped.
    exit: Option<ExitStatus>,
    /// Set by the reader once all output has been read and handled.
    done: bool,
    /// Whether an `Exited` event was queued (exactly one is).
    exit_reported: bool,
}

struct Session {
    spec: SpawnSpec,
    size: PtySize,
    /// Kept open so the PTY lives as long as the process; closed (with
    /// `fd`) once it has exited and all its output has been read.
    master: Option<Box<dyn MasterPty + Send>>,
    /// The duplicate of the master the I/O threads use (non-blocking).
    fd: Option<Arc<OwnedFd>>,
    input: Sender<Input>,
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
}

impl Session {
    /// Once the process has exited and its output has all been read, close
    /// the PTY and join the finished I/O threads (an exited session stays
    /// listed until it is killed, possibly for a long time in the daemon).
    fn release_if_finished(&mut self) {
        if !self.shared.lock().done {
            return;
        }
        self.master = None;
        self.fd = None;
        let (done, running): (Vec<_>, Vec<_>) = self.threads.drain(..).partition(|h| h.is_finished());
        self.threads = running;
        for h in done {
            let _ = h.join();
        }
    }
}

impl Shared {
    fn pid(&self) -> u32 {
        self.pid.load(Ordering::Acquire)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Wait up to `deadline` for the process to exit; whether it has.
    fn wait_exit_until(&self, deadline: Instant) -> bool {
        let mut st = self.lock();
        while st.exit.is_none() {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            st = self.changed.wait_timeout(st, deadline - now).unwrap_or_else(|e| e.into_inner()).0;
        }
        true
    }
}

impl LocalBackend {
    /// A backend that calls `waker` when output or exits are waiting.
    pub fn new(waker: Waker) -> Self {
        let (tx, rx) = mpsc::sync_channel(EVENT_QUEUE);
        Self {
            sessions: BTreeMap::new(),
            next_id: 0,
            tx,
            rx: Some(rx),
            local: VecDeque::new(),
            notifier: Notifier::new(waker),
            queued: Arc::default(),
            dying: Vec::new(),
            live_threads: Arc::default(),
            kill_grace: DEFAULT_KILL_GRACE,
        }
    }

    /// How long `kill` (and dropping the backend) waits after SIGHUP before
    /// sending SIGKILL.
    pub fn with_kill_grace(mut self, grace: Duration) -> Self {
        self.kill_grace = grace;
        self
    }

    /// The process id of a session.
    pub fn pid(&self, id: SessionId) -> Option<u32> {
        self.sessions.get(&id).map(|s| s.shared.pid())
    }

    /// Events waiting in the channel.
    pub fn queued(&self) -> usize {
        self.queued.load(Ordering::Acquire)
    }

    /// Bytes read from a session's PTY so far, delivered or not.
    pub fn bytes_read(&self, id: SessionId) -> Option<usize> {
        self.sessions.get(&id).map(|s| s.shared.bytes_read.load(Ordering::Acquire))
    }

    /// A counter of this backend's live I/O threads (it outlives the backend,
    /// so tests can check that dropping it joined them all).
    pub fn live_threads(&self) -> Arc<AtomicUsize> {
        self.live_threads.clone()
    }

    fn session(&self, id: SessionId) -> io::Result<&Session> {
        self.sessions.get(&id).ok_or_else(|| not_found(id))
    }

    /// Join the threads of killed sessions that have finished, and release
    /// what exited sessions no longer need.
    fn reap_dying(&mut self) {
        let (done, running): (Vec<_>, Vec<_>) = self.dying.drain(..).partition(|h| h.is_finished());
        self.dying = running;
        for h in done {
            let _ = h.join();
        }
        for s in self.sessions.values_mut() {
            s.release_if_finished();
        }
    }

    /// Whether a session still holds its PTY open (tests check that an
    /// exited session lets go of it).
    #[cfg(test)]
    pub(crate) fn holds_pty(&self, id: SessionId) -> bool {
        self.sessions.get(&id).is_some_and(|s| s.master.is_some() || s.fd.is_some())
    }

    fn spawn_thread(&self, name: String, f: impl FnOnce() + Send + 'static) -> io::Result<JoinHandle<()>> {
        let live = self.live_threads.clone();
        live.fetch_add(1, Ordering::AcqRel);
        let r = std::thread::Builder::new().name(name).spawn(move || {
            f();
            live.fetch_sub(1, Ordering::AcqRel);
        });
        if r.is_err() {
            self.live_threads.fetch_sub(1, Ordering::AcqRel);
        }
        r
    }

    /// The command to run for `spec` (validated: a program, an existing directory).
    fn command(spec: &SpawnSpec) -> io::Result<CommandBuilder> {
        if spec.program.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "no program to run"));
        }
        let cwd = resolved_cwd(spec)?;
        // portable-pty silently falls back to $HOME for a missing directory.
        if !cwd.is_dir() {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("no such directory: {}", cwd.display())));
        }
        let mut cmd = CommandBuilder::new(&spec.program);
        cmd.args(&spec.args);
        cmd.cwd(&cwd);
        for (k, v) in DEFAULT_ENV {
            if !spec.env.iter().any(|(name, _)| name == k) {
                cmd.env(k, v);
            }
        }
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        Ok(cmd)
    }

    /// The master's descriptor, duplicated for the I/O threads and made non-blocking.
    fn io_fd(master: &dyn MasterPty) -> io::Result<Arc<OwnedFd>> {
        let raw = master.as_raw_fd().ok_or_else(|| io::Error::other("PTY master has no descriptor"))?;
        let fd = sys::dup_cloexec(raw)?;
        sys::check_selectable(fd.as_raw_fd())?;
        sys::set_nonblocking(fd.as_raw_fd())?;
        Ok(Arc::new(fd))
    }
}

fn to_pp_size(s: PtySize) -> portable_pty::PtySize {
    portable_pty::PtySize { cols: s.cols, rows: s.rows, pixel_width: s.pixel_width, pixel_height: s.pixel_height }
}

/// Keep the `io::ErrorKind` when portable-pty wraps an `io::Error` (its
/// errors are `anyhow::Error`s); a program that can't be found becomes
/// `NotFound`.
fn anyhow_to_io(e: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    let e: Box<dyn std::error::Error + Send + Sync> = e.into();
    match e.downcast::<io::Error>() {
        Ok(io) => *io,
        Err(e) => {
            let msg = e.to_string();
            // portable-pty reports every program-lookup failure as "Unable to spawn ...".
            let kind = if msg.starts_with("Unable to spawn") { io::ErrorKind::NotFound } else { io::ErrorKind::Other };
            io::Error::new(kind, msg)
        }
    }
}

fn not_found(id: SessionId) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("no session {}", id.0))
}

/// Everything a reader thread needs.
struct Reader {
    id: SessionId,
    fd: Arc<OwnedFd>,
    shared: Arc<Shared>,
    tx: SyncSender<PtyEvent>,
    notifier: Notifier,
    queued: Arc<AtomicUsize>,
}

impl Reader {
    fn run(self) {
        let fd = self.fd.as_raw_fd();
        let mut buf = vec![0u8; READ_CHUNK];
        let mut draining = false;
        let mut drained = 0;
        loop {
            self.wait_while_paused();
            let n = match fill(fd, &mut buf) {
                Ok(0) if draining => break,
                Ok(0) => match sys::wait_fd(fd, false, self.shared.cancel.fd()) {
                    Ok(Ready::Io) => continue,
                    // The process exited: take what is left.
                    Ok(Ready::Cancelled) => {
                        draining = true;
                        continue;
                    }
                    Err(_) => break,
                },
                Ok(n) => n,
                // End of file (EIO on Linux once the slave side is closed).
                Err(_) => break,
            };
            self.deliver(&buf[..n]);
            if draining {
                drained += n;
                if drained >= DRAIN_LIMIT {
                    break;
                }
            }
        }
        self.finish();
    }

    /// Block while the session's output is held back (a killed session is
    /// never held: its output must be read for it to finish exiting).
    fn wait_while_paused(&self) {
        let mut st = self.shared.lock();
        while st.paused && !st.killed {
            st = self.shared.changed.wait(st).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Report a chunk if the session is attached. Killed sessions (and all
    /// sessions once the backend is gone) are still read, but the output is
    /// discarded: on macOS a process can't finish exiting while its terminal
    /// has unread output, so we must keep reading until it is reaped.
    fn deliver(&self, bytes: &[u8]) {
        self.shared.bytes_read.fetch_add(bytes.len(), Ordering::AcqRel);
        let st = self.shared.lock();
        if !st.attached || st.killed {
            return;
        }
        drop(st);
        if !self.send(PtyEvent::Output(self.id, bytes.to_vec())) {
            self.shared.lock().killed = true;
        }
    }

    fn send(&self, e: PtyEvent) -> bool {
        // Count first so `queued` never underflows in `try_recv`.
        self.queued.fetch_add(1, Ordering::AcqRel);
        let ok = match self.tx.try_send(e) {
            Ok(()) => true,
            Err(TrySendError::Full(e)) => {
                // Make sure the consumer knows there is something to drain
                // before blocking (backpressure).
                self.notifier.notify();
                self.tx.send(e).is_ok()
            }
            Err(TrySendError::Disconnected(_)) => false,
        };
        if ok {
            self.notifier.notify();
        } else {
            self.queued.fetch_sub(1, Ordering::AcqRel);
        }
        ok
    }

    /// All output is handled: wait for the exit status and report it once.
    fn finish(&self) {
        let mut st = self.shared.lock();
        while st.exit.is_none() {
            st = self.shared.changed.wait(st).unwrap_or_else(|e| e.into_inner());
        }
        st.done = true;
        if st.attached && !st.killed && !st.exit_reported {
            st.exit_reported = true;
            let status = st.exit.clone().expect("exit status present");
            drop(st);
            self.send(PtyEvent::Exited(self.id, status));
        }
    }
}

/// Read as much as is available (up to `buf.len()`) without blocking:
/// `Ok(0)` means nothing right now, `Err` end of file or a real error.
fn fill(fd: i32, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match sys::read(fd, &mut buf[n..]) {
            Ok(0) if n == 0 => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
            Err(e) if n == 0 => return Err(e),
            Err(_) => break,
        }
    }
    Ok(n)
}

fn writer_loop(fd: Arc<OwnedFd>, input: Receiver<Input>, shared: Arc<Shared>) {
    // Ends when the session is forgotten (sender dropped) or the process
    // exits (the waiter sends `Input::Exited`, so an exited session that is
    // never killed doesn't keep this thread waiting for input).
    while let Ok(Input::Bytes(bytes)) = input.recv() {
        if sys::write_all(fd.as_raw_fd(), &bytes, shared.cancel.fd()).is_err() {
            break;
        }
    }
}

/// What a session's writer thread is sent.
enum Input {
    Bytes(Vec<u8>),
    /// The process has exited: stop.
    Exited,
}

fn waiter_loop(shared: Arc<Shared>, writer: Option<Sender<Input>>) {
    let status = sys::wait_pid(shared.pid()).unwrap_or(ExitStatus { code: 1, signal: None });
    shared.lock().exit = Some(status);
    shared.changed.notify_all();
    // Wake the reader (to drain and finish) and the writer (to give up,
    // whether it is writing or waiting for input).
    shared.cancel.fire();
    if let Some(w) = writer {
        let _ = w.send(Input::Exited);
    }
}

/// Signal a session's process group: SIGHUP (plus SIGCONT, so a stopped
/// job can act on it) now, SIGKILL if it is still alive after `grace`.
fn escalate(shared: &Shared, grace: Duration) {
    if shared.lock().exit.is_some() {
        return;
    }
    sys::signal_group(shared.pid(), libc::SIGHUP);
    sys::signal_group(shared.pid(), libc::SIGCONT);
    if !shared.wait_exit_until(Instant::now() + grace) {
        // Still not reaped, so the pid can't have been reused.
        sys::signal_group(shared.pid(), libc::SIGKILL);
    }
}

impl PtyBackend for LocalBackend {
    fn spawn(&mut self, spec: SpawnSpec) -> io::Result<SessionId> {
        self.reap_dying();
        let cmd = Self::command(&spec)?;
        let pair = native_pty_system().openpty(to_pp_size(spec.size)).map_err(anyhow_to_io)?;
        let fd = Self::io_fd(&*pair.master)?;
        let shared = Arc::new(Shared {
            state: Mutex::new(State { attached: true, ..Default::default() }),
            changed: Condvar::new(),
            cancel: Cancel::new()?,
            pid: AtomicU32::new(0),
            bytes_read: AtomicUsize::new(0),
        });
        let id = SessionId(self.next_id + 1);
        // The reader is waiting before the process starts: on macOS a
        // session leader's exit flushes output nobody has read yet, so a
        // quick program's output is only safe if someone reads it at once.
        let reader = Reader {
            id,
            fd: fd.clone(),
            shared: shared.clone(),
            tx: self.tx.clone(),
            notifier: self.notifier.clone(),
            queued: self.queued.clone(),
        };
        let reader = self.spawn_thread(format!("pty-read-{}", id.0), move || reader.run())?;
        let started = pair
            .slave
            .spawn_command(cmd)
            .map_err(anyhow_to_io)
            .and_then(|child| child.process_id().ok_or_else(|| io::Error::other("spawned process has no pid")));
        // Close our copy of the slave so the master sees end-of-file when the
        // child (and anything it started) closes theirs. (The child handle
        // is dropped too: we reap with waitpid ourselves.)
        drop(pair.slave);
        let pid = match started {
            Ok(pid) => pid,
            Err(e) => {
                // Nothing ran: stop the reader as if a process had exited.
                shared.lock().exit = Some(ExitStatus { code: 127, signal: None });
                shared.changed.notify_all();
                shared.cancel.fire();
                let _ = reader.join();
                return Err(e);
            }
        };
        shared.pid.store(pid, Ordering::Release);
        self.next_id += 1;
        let mut threads = vec![reader];
        let waiter = shared.clone();
        let (input, input_rx) = mpsc::channel();
        let (wfd, wshared, exit_tx) = (fd.clone(), shared.clone(), input.clone());
        let more =
            self.spawn_thread(format!("pty-wait-{pid}"), move || waiter_loop(waiter, Some(exit_tx))).and_then(|w| {
                threads.push(w);
                self.spawn_thread(format!("pty-write-{pid}"), move || writer_loop(wfd, input_rx, wshared))
            });
        match more {
            Ok(w) => threads.push(w),
            Err(e) => {
                // Out of threads: don't leave the process running unreaped.
                sys::signal_group(pid, libc::SIGKILL);
                if threads.len() == 1 {
                    waiter_loop(shared.clone(), None);
                }
                threads.into_iter().for_each(|t| drop(t.join()));
                return Err(e);
            }
        }
        let size = spec.size;
        self.sessions
            .insert(id, Session { spec, size, master: Some(pair.master), fd: Some(fd), input, shared, threads });
        Ok(id)
    }

    fn attach(&mut self, id: SessionId, size: PtySize) -> io::Result<()> {
        let s = self.session(id)?;
        let report = {
            let mut st = s.shared.lock();
            st.attached = true;
            // The reader finished while detached: report the exit here.
            let report = st.done && !st.exit_reported;
            if report {
                st.exit_reported = true;
            }
            report.then(|| st.exit.clone()).flatten()
        };
        if let Some(status) = report {
            self.local.push_back(PtyEvent::Exited(id, status));
            self.notifier.notify();
        }
        self.resize(id, size)
    }

    fn detach(&mut self, id: SessionId) -> io::Result<()> {
        self.session(id)?.shared.lock().attached = false;
        Ok(())
    }

    fn pause_output(&mut self, id: SessionId, paused: bool) -> io::Result<()> {
        let s = self.session(id)?;
        s.shared.lock().paused = paused;
        if !paused {
            s.shared.changed.notify_all();
        }
        Ok(())
    }

    fn write(&mut self, id: SessionId, bytes: &[u8]) -> io::Result<()> {
        let s = self.session(id)?;
        if s.shared.lock().exit.is_some() {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "process has exited"));
        }
        if bytes.is_empty() {
            return Ok(());
        }
        s.input
            .send(Input::Bytes(bytes.to_vec()))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "process input is closed"))
    }

    fn resize(&mut self, id: SessionId, size: PtySize) -> io::Result<()> {
        let size = PtySize { cols: size.cols.max(1), rows: size.rows.max(1), ..size };
        let s = self.sessions.get_mut(&id).ok_or_else(|| not_found(id))?;
        if let Some(fd) = s.fd.as_ref().filter(|_| s.shared.lock().exit.is_none()) {
            sys::set_winsize(fd.as_raw_fd(), size.cols, size.rows, size.pixel_width, size.pixel_height)?;
        }
        s.size = size;
        Ok(())
    }

    fn kill(&mut self, id: SessionId) -> io::Result<()> {
        let s = self.sessions.remove(&id).ok_or_else(|| not_found(id))?;
        s.shared.lock().killed = true;
        s.shared.changed.notify_all(); // a paused reader goes on (discarding)
        self.local.retain(|e| event_session(e) != id);
        let Session { input, shared, mut threads, .. } = s;
        // The writer stops; the reader keeps discarding output until the
        // waiter has reaped the process, then both finish.
        drop(input);
        let grace = self.kill_grace;
        threads.push(self.spawn_thread(format!("pty-kill-{}", shared.pid()), move || escalate(&shared, grace))?);
        self.dying.extend(threads);
        self.reap_dying();
        Ok(())
    }

    fn list(&mut self) -> io::Result<Vec<SessionInfo>> {
        Ok(self
            .sessions
            .iter()
            .map(|(id, s)| SessionInfo {
                id: *id,
                program: s.spec.program.clone(),
                args: s.spec.args.clone(),
                cwd: s.spec.cwd.clone(),
                pid: Some(s.shared.pid()),
                size: s.size,
                exited: s.shared.lock().exit.clone(),
            })
            .collect())
    }

    fn try_recv(&mut self) -> Option<PtyEvent> {
        loop {
            let e = match self.rx.as_ref()?.try_recv() {
                Ok(e) => {
                    self.queued.fetch_sub(1, Ordering::AcqRel);
                    e
                }
                Err(_) => match self.local.pop_front() {
                    Some(e) => e,
                    None => {
                        self.notifier.clear();
                        // A reader may have queued between the check and the clear.
                        match self.rx.as_ref()?.try_recv() {
                            Ok(e) => {
                                self.queued.fetch_sub(1, Ordering::AcqRel);
                                e
                            }
                            Err(_) => return None,
                        }
                    }
                },
            };
            // Killed sessions are forgotten: drop whatever they still queued.
            if let Some(s) = self.sessions.get_mut(&event_session(&e)) {
                if matches!(e, PtyEvent::Exited(..)) {
                    s.release_if_finished();
                }
                return Some(e);
            }
        }
    }
}

fn event_session(e: &PtyEvent) -> SessionId {
    match e {
        PtyEvent::Output(id, _) | PtyEvent::Exited(id, _) => *id,
    }
}

impl Drop for LocalBackend {
    fn drop(&mut self) {
        // Readers blocked on a full channel fail instead of waiting forever.
        self.rx = None;
        let sessions = std::mem::take(&mut self.sessions);
        let mut threads = std::mem::take(&mut self.dying);
        let mut shared = Vec::new();
        for (_, s) in sessions {
            s.shared.lock().killed = true;
            s.shared.changed.notify_all();
            let Session { input, shared: sh, threads: t, .. } = s;
            drop(input);
            if sh.lock().exit.is_none() {
                sys::signal_group(sh.pid(), libc::SIGHUP);
                sys::signal_group(sh.pid(), libc::SIGCONT);
            }
            threads.extend(t);
            shared.push(sh);
        }
        // One grace period for all of them, then SIGKILL the stragglers.
        let deadline = Instant::now() + self.kill_grace;
        for sh in &shared {
            if !sh.wait_exit_until(deadline) {
                sys::signal_group(sh.pid(), libc::SIGKILL);
            }
        }
        for t in threads {
            let _ = t.join();
        }
    }
}

/// Where a session runs: its `cwd`, else this process's current directory
/// (portable-pty would default to `$HOME`).
pub fn resolved_cwd(spec: &SpawnSpec) -> io::Result<PathBuf> {
    match &spec.cwd {
        Some(d) => Ok(d.clone()),
        None => std::env::current_dir(),
    }
}

#[cfg(test)]
#[path = "local_tests.rs"]
mod tests;
