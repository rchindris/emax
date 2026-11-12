//! Processes that are children of this process, on PTYs from `portable-pty`.
//!
//! SKELETON: owned by the pty builder (see docs/design/terminal-substrate.md).
//! Every operation currently fails with `Unsupported`.

use std::io;

use super::{PtyBackend, PtyEvent, PtySize, SessionId, SessionInfo, SpawnSpec, Waker};

pub struct LocalBackend {
    _waker: Waker,
}

impl LocalBackend {
    /// A backend that calls `waker` when output or exits are waiting.
    pub fn new(waker: Waker) -> Self {
        Self { _waker: waker }
    }
}

fn unsupported<T>() -> io::Result<T> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "local PTY backend not implemented yet"))
}

impl PtyBackend for LocalBackend {
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
        Ok(Vec::new())
    }

    fn try_recv(&mut self) -> Option<PtyEvent> {
        None
    }
}
