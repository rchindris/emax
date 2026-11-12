//! An in-memory [`PtyBackend`] for tests: no processes. The test keeps a
//! [`MockHandle`] to see what was spawned, written and resized, and to make
//! sessions "print" output or exit.

use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::sync::{Arc, Mutex, MutexGuard};

use super::{ExitStatus, Notifier, PtyBackend, PtyEvent, PtySize, SessionId, SessionInfo, SpawnSpec, Waker, noop_waker};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MockSession {
    pub spec: SpawnSpec,
    pub size: PtySize,
    /// Everything written to the session, in order.
    pub input: Vec<u8>,
    pub attached: bool,
    pub exited: Option<ExitStatus>,
    /// Sizes requested by `resize`/`attach`, in order.
    pub resizes: Vec<PtySize>,
    /// Output held back (`pause_output`): its events stay queued.
    pub paused: bool,
}

#[derive(Default)]
struct State {
    sessions: BTreeMap<SessionId, MockSession>,
    events: VecDeque<PtyEvent>,
    next_id: u64,
    /// Bytes `attach` replays first (per session).
    replay: BTreeMap<SessionId, Vec<u8>>,
    /// Make the next `spawn` fail with this message.
    fail_spawn: Option<String>,
}

/// The test's view of a [`MockBackend`] (cheap to clone).
#[derive(Clone)]
pub struct MockHandle {
    state: Arc<Mutex<State>>,
    notifier: Notifier,
}

pub struct MockBackend {
    handle: MockHandle,
}

impl MockBackend {
    pub fn new() -> (Self, MockHandle) {
        Self::with_waker(noop_waker())
    }

    pub fn with_waker(waker: Waker) -> (Self, MockHandle) {
        let handle = MockHandle { state: Arc::default(), notifier: Notifier::new(waker) };
        (Self { handle: handle.clone() }, handle)
    }
}

impl MockHandle {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn push(&self, e: PtyEvent) {
        self.lock().events.push_back(e);
        self.notifier.notify();
    }

    /// The session "prints" `bytes` (reported only while attached).
    pub fn output(&self, id: SessionId, bytes: &[u8]) {
        let attached = self.lock().sessions.get(&id).is_some_and(|s| s.attached && s.exited.is_none());
        if attached {
            self.push(PtyEvent::Output(id, bytes.to_vec()));
        }
    }

    /// The session's process exits.
    pub fn exit(&self, id: SessionId, status: ExitStatus) {
        let attached = {
            let mut st = self.lock();
            let Some(s) = st.sessions.get_mut(&id) else { return };
            s.exited = Some(status.clone());
            s.attached
        };
        if attached {
            self.push(PtyEvent::Exited(id, status));
        }
    }

    /// Add a session that is already running but not attached (as a daemon
    /// would have after an editor restart); `attach` replays `replay`.
    pub fn add_detached(&self, spec: SpawnSpec, replay: &[u8]) -> SessionId {
        let mut st = self.lock();
        st.next_id += 1;
        let id = SessionId(st.next_id);
        let size = spec.size;
        st.sessions.insert(id, MockSession { spec, size, ..Default::default() });
        st.replay.insert(id, replay.to_vec());
        id
    }

    pub fn session(&self, id: SessionId) -> Option<MockSession> {
        self.lock().sessions.get(&id).cloned()
    }

    pub fn sessions(&self) -> Vec<SessionId> {
        self.lock().sessions.keys().copied().collect()
    }

    /// Everything written to `id` so far.
    pub fn input(&self, id: SessionId) -> Vec<u8> {
        self.session(id).map(|s| s.input).unwrap_or_default()
    }

    pub fn fail_next_spawn(&self, msg: &str) {
        self.lock().fail_spawn = Some(msg.to_string());
    }
}

fn event_session(e: &PtyEvent) -> SessionId {
    match e {
        PtyEvent::Output(id, _) | PtyEvent::Exited(id, _) => *id,
    }
}

fn not_found(id: SessionId) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("no session {}", id.0))
}

impl PtyBackend for MockBackend {
    fn spawn(&mut self, spec: SpawnSpec) -> io::Result<SessionId> {
        let mut st = self.handle.lock();
        if let Some(msg) = st.fail_spawn.take() {
            return Err(io::Error::other(msg));
        }
        st.next_id += 1;
        let id = SessionId(st.next_id);
        let size = spec.size;
        st.sessions.insert(id, MockSession { spec, size, attached: true, ..Default::default() });
        Ok(id)
    }

    fn attach(&mut self, id: SessionId, size: PtySize) -> io::Result<()> {
        let (replay, exited) = {
            let mut st = self.handle.lock();
            let s = st.sessions.get_mut(&id).ok_or_else(|| not_found(id))?;
            s.attached = true;
            s.size = size;
            s.resizes.push(size);
            let exited = s.exited.clone();
            (st.replay.get(&id).cloned().unwrap_or_default(), exited)
        };
        if !replay.is_empty() {
            self.handle.push(PtyEvent::Output(id, replay));
        }
        if let Some(status) = exited {
            self.handle.push(PtyEvent::Exited(id, status));
        }
        Ok(())
    }

    fn detach(&mut self, id: SessionId) -> io::Result<()> {
        let mut st = self.handle.lock();
        st.sessions.get_mut(&id).ok_or_else(|| not_found(id))?.attached = false;
        Ok(())
    }

    fn write(&mut self, id: SessionId, bytes: &[u8]) -> io::Result<()> {
        let mut st = self.handle.lock();
        let s = st.sessions.get_mut(&id).ok_or_else(|| not_found(id))?;
        if s.exited.is_some() {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "process has exited"));
        }
        s.input.extend_from_slice(bytes);
        Ok(())
    }

    fn resize(&mut self, id: SessionId, size: PtySize) -> io::Result<()> {
        let mut st = self.handle.lock();
        let s = st.sessions.get_mut(&id).ok_or_else(|| not_found(id))?;
        s.size = size;
        s.resizes.push(size);
        Ok(())
    }

    fn kill(&mut self, id: SessionId) -> io::Result<()> {
        let mut st = self.handle.lock();
        st.sessions.remove(&id).ok_or_else(|| not_found(id))?;
        st.replay.remove(&id);
        Ok(())
    }

    fn list(&mut self) -> io::Result<Vec<SessionInfo>> {
        let st = self.handle.lock();
        Ok(st
            .sessions
            .iter()
            .map(|(id, s)| SessionInfo {
                id: *id,
                program: s.spec.program.clone(),
                args: s.spec.args.clone(),
                cwd: s.spec.cwd.clone(),
                pid: None,
                size: s.size,
                exited: s.exited.clone(),
            })
            .collect())
    }

    fn pause_output(&mut self, id: SessionId, paused: bool) -> io::Result<()> {
        let mut st = self.handle.lock();
        st.sessions.get_mut(&id).ok_or_else(|| not_found(id))?.paused = paused;
        let waiting = st.events.iter().any(|e| event_session(e) == id);
        drop(st);
        if !paused && waiting {
            self.handle.notifier.notify();
        }
        Ok(())
    }

    /// The oldest event of a session that isn't paused (a paused session's
    /// events wait, as a real program's output would in its PTY).
    fn try_recv(&mut self) -> Option<PtyEvent> {
        let mut st = self.handle.lock();
        let ready = |st: &State, e: &PtyEvent| !st.sessions.get(&event_session(e)).is_some_and(|s| s.paused);
        let next = st.events.iter().position(|e| ready(&st, e));
        match next {
            Some(i) => st.events.remove(i),
            None => {
                self.handle.notifier.clear();
                None
            }
        }
    }

    fn persistent(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn records_and_reports() {
        let wakes = Arc::new(AtomicUsize::new(0));
        let w = wakes.clone();
        let (mut b, h) = MockBackend::with_waker(Arc::new(move || {
            w.fetch_add(1, Ordering::SeqCst);
        }));
        let id = b.spawn(SpawnSpec { program: "sh".into(), ..Default::default() }).unwrap();
        b.write(id, b"ls\r").unwrap();
        b.resize(id, PtySize::new(100, 30)).unwrap();
        assert_eq!(h.input(id), b"ls\r");
        assert_eq!(h.session(id).unwrap().size.cols, 100);

        h.output(id, b"a");
        h.output(id, b"b");
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        assert_eq!(b.try_recv(), Some(PtyEvent::Output(id, b"a".to_vec())));
        assert_eq!(b.try_recv(), Some(PtyEvent::Output(id, b"b".to_vec())));
        assert_eq!(b.try_recv(), None);
        h.exit(id, ExitStatus { code: 2, signal: None });
        assert_eq!(wakes.load(Ordering::SeqCst), 2);
        assert_eq!(b.try_recv(), Some(PtyEvent::Exited(id, ExitStatus { code: 2, signal: None })));
        assert!(b.write(id, b"x").is_err());
        assert_eq!(b.list().unwrap()[0].exited.as_ref().unwrap().code, 2);
        b.kill(id).unwrap();
        assert!(b.kill(id).is_err());
        assert!(b.list().unwrap().is_empty());
    }

    #[test]
    fn detached_sessions_replay_on_attach() {
        let (mut b, h) = MockBackend::new();
        let id = h.add_detached(SpawnSpec::default(), b"earlier output");
        h.output(id, b"lost: not attached");
        assert_eq!(b.try_recv(), None);
        b.attach(id, PtySize::new(90, 20)).unwrap();
        assert_eq!(b.try_recv(), Some(PtyEvent::Output(id, b"earlier output".to_vec())));
        assert_eq!(h.session(id).unwrap().resizes, [PtySize::new(90, 20)]);
        b.detach(id).unwrap();
        h.output(id, b"x");
        assert_eq!(b.try_recv(), None);
        // Paused output waits; other sessions' doesn't.
        b.attach(id, PtySize::new(90, 20)).unwrap();
        assert_eq!(b.try_recv(), Some(PtyEvent::Output(id, b"earlier output".to_vec())));
        let other = b.spawn(SpawnSpec::default()).unwrap();
        b.pause_output(id, true).unwrap();
        h.output(id, b"held");
        h.output(other, b"flows");
        assert_eq!(b.try_recv(), Some(PtyEvent::Output(other, b"flows".to_vec())));
        assert_eq!(b.try_recv(), None);
        b.pause_output(id, false).unwrap();
        assert_eq!(b.try_recv(), Some(PtyEvent::Output(id, b"held".to_vec())));
        h.fail_next_spawn("nope");
        assert!(b.spawn(SpawnSpec::default()).is_err());
        assert!(b.spawn(SpawnSpec::default()).is_ok());
    }
}
