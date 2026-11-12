//! [`DaemonBackend`]: a [`PtyBackend`] whose sessions live in the session
//! daemon, reached over its Unix socket.
//!
//! SKELETON: owned by the persistence builder (see
//! docs/design/terminal-substrate.md). Connecting currently fails.

use std::io;
use std::path::Path;

use crate::engine::pty::{PtyBackend, PtyEvent, PtySize, SessionId, SessionInfo, SpawnSpec, Waker};

pub struct DaemonBackend {
    _waker: Waker,
}

fn unsupported<T>() -> io::Result<T> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "the daemon backend is not implemented yet"))
}

impl DaemonBackend {
    /// Connect to a running daemon at `socket`.
    pub fn connect(_socket: &Path, _waker: Waker) -> io::Result<Self> {
        unsupported()
    }

    /// Connect, first starting a daemon (this executable with `--pty-daemon`)
    /// if none is running, waiting up to a few seconds for it to listen.
    pub fn connect_or_start(socket: &Path, replay_bytes: usize, waker: Waker) -> io::Result<Self> {
        let _ = replay_bytes;
        Self::connect(socket, waker)
    }
}

impl PtyBackend for DaemonBackend {
    fn spawn(&mut self, _spec: SpawnSpec) -> io::Result<SessionId> {
        unsupported()
    }

    fn attach(&mut self, _id: SessionId, _size: PtySize) -> io::Result<()> {
        unsupported()
    }

    fn detach(&mut self, _id: SessionId) -> io::Result<()> {
        unsupported()
    }

    fn write(&mut self, _id: SessionId, _bytes: &[u8]) -> io::Result<()> {
        unsupported()
    }

    fn resize(&mut self, _id: SessionId, _size: PtySize) -> io::Result<()> {
        unsupported()
    }

    fn kill(&mut self, _id: SessionId) -> io::Result<()> {
        unsupported()
    }

    fn list(&mut self) -> io::Result<Vec<SessionInfo>> {
        unsupported()
    }

    fn try_recv(&mut self) -> Option<PtyEvent> {
        None
    }

    fn persistent(&self) -> bool {
        true
    }
}
