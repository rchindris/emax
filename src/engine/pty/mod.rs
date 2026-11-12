//! Processes on pseudo-terminals.
//!
//! A [`PtyBackend`] runs programs on PTYs and reports what they print. The
//! editor owns one backend (created lazily, see `Editor::pty_backend`) and
//! talks to it only through this trait, so terminal buffers work the same
//! whether the processes are children of the editor ([`local::LocalBackend`])
//! or of the session daemon (`session::client::DaemonBackend`), and tests can
//! use [`mock::MockBackend`].
//!
//! Threading: backends do their blocking I/O on their own threads and hand
//! events over through an internal channel. [`PtyBackend::try_recv`] never
//! blocks. When events arrive the backend calls the [`Waker`] it was created
//! with — at most once until the receiver drains (see [`Notifier`]) — so the
//! UI can schedule a tick on its own thread. Output is raw bytes (never
//! assumed to be UTF-8) and arrives in chunks, never one event per byte.

pub mod local;
pub mod mock;
mod sys;

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

/// Identifies a process session within one backend (for the daemon backend,
/// ids are the daemon's and stay valid across editor restarts).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SessionId(pub u64);

/// Size of a terminal in cells (and, optionally, pixels).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PtySize {
    pub cols: u16,
    pub rows: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
}

impl PtySize {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self { cols: cols.max(1), rows: rows.max(1), pixel_width: 0, pixel_height: 0 }
    }
}

impl Default for PtySize {
    fn default() -> Self {
        Self::new(80, 24)
    }
}

/// What to run.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SpawnSpec {
    pub program: String,
    pub args: Vec<String>,
    /// Working directory (`None`: the backend's current directory).
    pub cwd: Option<PathBuf>,
    /// Variables set on top of the inherited environment (`TERM` included).
    pub env: Vec<(String, String)>,
    pub size: PtySize,
}

/// How a process ended.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitStatus {
    /// Exit code (0 is success; 1 when killed by a signal, as portable-pty reports).
    pub code: u32,
    /// The signal that killed it, if any (e.g. `"Hangup"`).
    pub signal: Option<String>,
}

impl ExitStatus {
    pub fn success(&self) -> bool {
        self.code == 0 && self.signal.is_none()
    }
}

/// Something a backend reports.
#[derive(Clone, Debug, PartialEq)]
pub enum PtyEvent {
    /// Bytes the process wrote (a coalesced chunk).
    Output(SessionId, Vec<u8>),
    /// The process exited; no more output follows for this session.
    Exited(SessionId, ExitStatus),
}

/// A session as listed by [`PtyBackend::list`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub pid: Option<u32>,
    pub size: PtySize,
    /// `Some` once the process has exited (exited sessions stay listed until killed).
    pub exited: Option<ExitStatus>,
}

/// Called (from any thread) when a backend has events to read.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// A waker that does nothing (tests that poll).
pub fn noop_waker() -> Waker {
    Arc::new(|| {})
}

/// Runs processes on PTYs. See the module docs for the threading model.
pub trait PtyBackend {
    /// Start a process. The new session is attached: its output is reported.
    fn spawn(&mut self, spec: SpawnSpec) -> io::Result<SessionId>;
    /// Start reporting an existing session's output: first what it printed
    /// recently (the backend's replay buffer; a backend without one replays
    /// nothing), then live output; if it has exited, an `Exited` event
    /// follows the replay. Also resizes it to `size` (so full-screen programs
    /// redraw). Attaching an attached session is not an error.
    fn attach(&mut self, id: SessionId, size: PtySize) -> io::Result<()>;
    /// Stop reporting a session's output (output meanwhile is dropped, except
    /// what a replay buffer keeps); the process keeps running.
    fn detach(&mut self, id: SessionId) -> io::Result<()>;
    /// Hold back a session's output (`paused`), or let it flow again. While
    /// held, the backend stops reading it: the program blocks once the PTY
    /// buffer is full, and nothing is lost. Other sessions keep flowing (the
    /// daemon holds back only the sessions a lagging client is attached to).
    /// A killed session is never held. Backends that can't hold output back
    /// ignore this.
    fn pause_output(&mut self, id: SessionId, paused: bool) -> io::Result<()> {
        let _ = (id, paused);
        Ok(())
    }
    /// Send input to the process.
    fn write(&mut self, id: SessionId, bytes: &[u8]) -> io::Result<()>;
    fn resize(&mut self, id: SessionId, size: PtySize) -> io::Result<()>;
    /// Kill the process (SIGHUP, then SIGKILL as needed) and forget the
    /// session: no events for it are reported afterwards.
    fn kill(&mut self, id: SessionId) -> io::Result<()>;
    fn list(&mut self) -> io::Result<Vec<SessionInfo>>;
    /// The next event, if one is waiting. Never blocks.
    fn try_recv(&mut self) -> Option<PtyEvent>;
    /// Whether sessions survive the editor exiting (the daemon backend).
    fn persistent(&self) -> bool {
        false
    }
}

/// Calls a [`Waker`] once per batch of events: the first [`Notifier::notify`]
/// after the receiver called [`Notifier::clear`] wakes; later ones don't.
/// Producers notify after queueing an event; the receiver clears *before*
/// draining, so nothing queued during the drain is missed.
#[derive(Clone)]
pub struct Notifier {
    pending: Arc<AtomicBool>,
    waker: Waker,
}

impl Notifier {
    pub fn new(waker: Waker) -> Self {
        Self { pending: Arc::new(AtomicBool::new(false)), waker }
    }

    pub fn notify(&self) {
        if !self.pending.swap(true, Ordering::AcqRel) {
            (self.waker)();
        }
    }

    pub fn clear(&self) {
        self.pending.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    #[test]
    fn notifier_wakes_once_per_batch() {
        let n = Arc::new(AtomicUsize::new(0));
        let c = n.clone();
        let notifier = Notifier::new(Arc::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
        }));
        let other = notifier.clone();
        notifier.notify();
        other.notify();
        notifier.notify();
        assert_eq!(n.load(Ordering::SeqCst), 1);
        notifier.clear();
        other.notify();
        assert_eq!(n.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn sizes_and_status() {
        assert_eq!(PtySize::new(0, 0), PtySize { cols: 1, rows: 1, pixel_width: 0, pixel_height: 0 });
        assert_eq!(PtySize::default().cols, 80);
        assert!(ExitStatus::default().success());
        assert!(!ExitStatus { code: 1, signal: None }.success());
        assert!(!ExitStatus { code: 0, signal: Some("Hangup".into()) }.success());
    }
}
