//! Real-process tests for [`LocalBackend`] (with `/bin/sh`, `cat`, `stty`).
//! Everything synchronises by polling with a timeout, never by sleeping.

use std::sync::atomic::AtomicUsize;

use super::*;
use crate::engine::pty::sys::pid_exists;
use crate::engine::testutil::{TIMEOUT, temp_dir, wait_until};

fn spec(program: &str, args: &[&str]) -> SpawnSpec {
    SpawnSpec { program: program.into(), args: args.iter().map(|a| a.to_string()).collect(), ..Default::default() }
}

fn sh(script: &str) -> SpawnSpec {
    spec("/bin/sh", &["-c", script])
}

fn backend() -> LocalBackend {
    LocalBackend::new(noop_waker()).with_kill_grace(Duration::from_millis(200))
}

fn noop_waker() -> Waker {
    super::super::noop_waker()
}

/// What a session reported, gathered by draining the backend.
#[derive(Default)]
struct Seen {
    out: BTreeMap<SessionId, Vec<u8>>,
    exits: BTreeMap<SessionId, Vec<ExitStatus>>,
    events: BTreeMap<SessionId, usize>,
    /// Output that arrived after its session's `Exited`.
    late: usize,
}

impl Seen {
    fn drain(&mut self, b: &mut LocalBackend) {
        while let Some(e) = b.try_recv() {
            match e {
                PtyEvent::Output(id, bytes) => {
                    if self.exits.contains_key(&id) {
                        self.late += 1;
                    }
                    assert!(!bytes.is_empty(), "empty output event");
                    *self.events.entry(id).or_default() += 1;
                    self.out.entry(id).or_default().extend(bytes);
                }
                PtyEvent::Exited(id, status) => {
                    *self.events.entry(id).or_default() += 1;
                    self.exits.entry(id).or_default().push(status);
                }
            }
        }
    }

    fn text(&self, id: SessionId) -> String {
        String::from_utf8_lossy(self.out.get(&id).map(|v| v.as_slice()).unwrap_or_default()).into_owned()
    }

    fn exited(&self, id: SessionId) -> Option<&ExitStatus> {
        self.exits.get(&id).and_then(|v| v.first())
    }

    /// Drain until `cond` holds.
    fn until(&mut self, b: &mut LocalBackend, what: &str, mut cond: impl FnMut(&Self) -> bool) {
        wait_until(what, TIMEOUT, || {
            self.drain(b);
            cond(self)
        });
    }

    fn until_exit(&mut self, b: &mut LocalBackend, id: SessionId) -> ExitStatus {
        self.until(b, "process exit", |s| s.exited(id).is_some());
        assert_eq!(self.late, 0, "output after Exited");
        assert_eq!(self.exits[&id].len(), 1, "exactly one Exited");
        self.exited(id).unwrap().clone()
    }
}

fn count(hay: &str, needle: &str) -> usize {
    hay.matches(needle).count()
}

/// Run a script to completion; its output and exit status.
fn run(script: &str) -> (String, ExitStatus) {
    let mut b = backend();
    let id = b.spawn(sh(script)).unwrap();
    let mut seen = Seen::default();
    let status = seen.until_exit(&mut b, id);
    (seen.text(id), status)
}

#[test]
fn output_then_exit_code() {
    let (out, status) = run("echo hello; echo world; exit 3");
    assert_eq!(out, "hello\r\nworld\r\n");
    assert_eq!(status, ExitStatus { code: 3, signal: None });
    let (_, ok) = run("true");
    assert!(ok.success());
}

#[test]
fn exit_codes_and_signals() {
    for code in [0u32, 1, 2, 42, 255] {
        assert_eq!(run(&format!("exit {code}")).1, ExitStatus { code, signal: None });
    }
    for (sig, name) in [("TERM", "Terminated"), ("KILL", "Killed"), ("HUP", "Hangup"), ("INT", "Interrupt")] {
        let status = run(&format!("kill -{sig} $$")).1;
        assert_eq!(status, ExitStatus { code: 1, signal: Some(name.into()) }, "SIG{sig}");
        assert!(!status.success());
    }
}

#[test]
fn output_always_precedes_exit() {
    // Property-style: many sizes, all output delivered, Exited last and once.
    let mut b = backend();
    let mut seen = Seen::default();
    let mut ids = Vec::new();
    for i in 0..24usize {
        let n = 1 + i * 3001;
        let id = b.spawn(sh(&format!("head -c {n} /dev/zero | tr '\\0' x; exit {i}"))).unwrap();
        ids.push((id, n, i as u32));
    }
    for &(id, n, code) in &ids {
        let status = seen.until_exit(&mut b, id);
        assert_eq!(status.code, code);
        assert_eq!(seen.out.get(&id).map_or(0, |o| o.len()), n, "session {} lost output", id.0);
    }
}

#[test]
fn echo_round_trip_through_cat() {
    let mut b = backend();
    let id = b.spawn(spec("/bin/cat", &[])).unwrap();
    let mut seen = Seen::default();
    b.write(id, b"hello\n").unwrap();
    // Once echoed by the terminal, once printed by cat.
    seen.until(&mut b, "echo and cat output", |s| count(&s.text(id), "hello\r\n") == 2);
    b.write(id, b"second line\n").unwrap();
    seen.until(&mut b, "second line", |s| count(&s.text(id), "second line\r\n") == 2);
    // ^D at the start of a line: end of file for cat.
    b.write(id, b"\x04").unwrap();
    assert!(seen.until_exit(&mut b, id).success());
}

#[test]
fn interactive_shell() {
    let mut b = backend();
    let id = b.spawn(spec("/bin/sh", &[])).unwrap();
    let mut seen = Seen::default();
    b.write(id, b"echo $((6 * 7))x\n").unwrap();
    seen.until(&mut b, "arithmetic result", |s| s.text(id).contains("42x"));
    b.write(id, b"exit 5\n").unwrap();
    assert_eq!(seen.until_exit(&mut b, id).code, 5);
}

#[test]
fn large_output_is_complete_and_coalesced() {
    let mut b = backend();
    // 15,000,000 zero bytes -> 20,000,000 base64 'A's (no padding).
    let id = b.spawn(sh("head -c 15000000 /dev/zero | base64")).unwrap();
    let mut seen = Seen::default();
    let start = Instant::now();
    wait_until("20 MB of output", Duration::from_secs(60), || {
        seen.drain(&mut b);
        seen.exited(id).is_some()
    });
    assert!(seen.exited(id).unwrap().success());
    let out = &seen.out[&id];
    let a = out.iter().filter(|&&c| c == b'A').count();
    let other = out.iter().filter(|&&c| !matches!(c, b'A' | b'\r' | b'\n')).count();
    assert_eq!(a, 20_000_000);
    assert_eq!(other, 0);
    let events = seen.events[&id];
    assert!(events < 20_000_000 / 1024, "{events} events for 20 MB: not coalesced");
    eprintln!("20 MB in {events} events, {:?}", start.elapsed());
}

#[test]
fn backpressure_blocks_the_child_and_loses_nothing() {
    let mut b = backend();
    // ~40 MB: more than the channel can hold (EVENT_QUEUE chunks).
    let total = 40_000_000;
    let id = b.spawn(sh(&format!("head -c {total} /dev/zero | tr '\\0' z"))).unwrap();
    wait_until("the event queue to fill", TIMEOUT, || b.queued() >= EVENT_QUEUE);
    // The reader is stuck on the full queue, so the child can't finish.
    assert!(b.list().unwrap()[0].exited.is_none());
    assert!(b.bytes_read(id).unwrap() < total);
    let mut seen = Seen::default();
    wait_until("all output", Duration::from_secs(60), || {
        seen.drain(&mut b);
        seen.exited(id).is_some()
    });
    assert_eq!(seen.out[&id].len(), total);
    assert!(seen.out[&id].iter().all(|&c| c == b'z'));
    assert_eq!(b.queued(), 0);
}

#[test]
fn binary_output_passes_through() {
    let mut b = backend();
    // Raw mode: no newline translation, so the bytes come back exactly.
    let id = b.spawn(sh(r"stty raw -echo; printf '\377\376\200\000\033[31mX\302'")).unwrap();
    let mut seen = Seen::default();
    seen.until_exit(&mut b, id);
    assert_eq!(seen.out[&id], b"\xff\xfe\x80\x00\x1b[31mX\xc2");
}

#[test]
fn binary_input_passes_through() {
    let mut b = backend();
    let id = b.spawn(sh("stty raw -echo; echo ready; od -An -tx1 -N6")).unwrap();
    let mut seen = Seen::default();
    seen.until(&mut b, "ready", |s| s.text(id).contains("ready"));
    b.write(id, b"\xff\x00\x80\xc3(\x01").unwrap();
    seen.until_exit(&mut b, id);
    let words: Vec<String> = seen.text(id).split_whitespace().map(String::from).collect();
    assert!(words.ends_with(&["ff", "00", "80", "c3", "28", "01"].map(String::from)), "{words:?}");
}

#[test]
fn resize_is_visible_to_stty() {
    let mut b = backend();
    let mut s = sh("stty -echo; stty size; read x; stty size; read y");
    s.size = PtySize::new(80, 24);
    let id = b.spawn(s).unwrap();
    let mut seen = Seen::default();
    seen.until(&mut b, "initial size", |s| s.text(id).contains("24 80"));
    b.resize(id, PtySize::new(132, 43)).unwrap();
    assert_eq!(b.list().unwrap()[0].size, PtySize::new(132, 43));
    b.write(id, b"\n").unwrap();
    seen.until(&mut b, "new size", |s| s.text(id).contains("43 132"));
    // Zero sizes are clamped rather than rejected.
    b.resize(id, PtySize { cols: 0, rows: 0, pixel_width: 0, pixel_height: 0 }).unwrap();
    assert_eq!(b.list().unwrap()[0].size, PtySize::new(1, 1));
    b.write(id, b"\n").unwrap();
    assert!(seen.until_exit(&mut b, id).success());
}

#[test]
fn resize_sends_sigwinch() {
    let mut b = backend();
    let id = b.spawn(sh("trap 'stty size; exit 0' WINCH; stty -echo; echo armed; while :; do read x; done")).unwrap();
    let mut seen = Seen::default();
    seen.until(&mut b, "trap armed", |s| s.text(id).contains("armed"));
    b.resize(id, PtySize::new(100, 30)).unwrap();
    // `read` is interrupted by the signal only after a line on some shells:
    // nudge it so the trap runs.
    b.write(id, b"\n").unwrap();
    assert!(seen.until_exit(&mut b, id).success());
    assert!(seen.text(id).contains("30 100"), "{:?}", seen.text(id));
}

#[test]
fn environment_defaults_overrides_and_inheritance() {
    let mut b = backend();
    let mut seen = Seen::default();
    let script = r#"printf '%s|%s|%s|%s\n' "$TERM" "$COLORTERM" "$EMAX_TEST_VAR" "${PATH:+path}""#;
    let mut s = sh(script);
    s.env = vec![("EMAX_TEST_VAR".into(), "héllo wörld".into())];
    let a = b.spawn(s).unwrap();
    let mut s = sh(script);
    s.env = vec![("TERM".into(), "dumb".into()), ("COLORTERM".into(), "".into())];
    let c = b.spawn(s).unwrap();
    seen.until_exit(&mut b, a);
    seen.until_exit(&mut b, c);
    assert_eq!(seen.text(a), "xterm-256color|truecolor|héllo wörld|path\r\n");
    assert_eq!(seen.text(c), "dumb|||path\r\n");
}

#[test]
fn working_directory() {
    let dir = temp_dir("cwd");
    let mut b = backend();
    let mut seen = Seen::default();
    let mut s = sh("pwd -P");
    s.cwd = Some(dir.clone());
    let id = b.spawn(s).unwrap();
    seen.until_exit(&mut b, id);
    assert_eq!(seen.text(id).trim_end(), dir.canonicalize().unwrap().to_str().unwrap());
    assert_eq!(b.list().unwrap()[0].cwd.as_deref(), Some(dir.as_path()));

    // No cwd: this process's directory, not $HOME.
    let id = b.spawn(sh("pwd -P")).unwrap();
    seen.until_exit(&mut b, id);
    let here = std::env::current_dir().unwrap().canonicalize().unwrap();
    assert_eq!(seen.text(id).trim_end(), here.to_str().unwrap());

    let mut s = sh("pwd");
    s.cwd = Some(dir.join("missing"));
    assert_eq!(b.spawn(s).unwrap_err().kind(), io::ErrorKind::NotFound);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn spawn_failures_are_errors_not_panics() {
    let mut b = backend();
    let live = b.live_threads();
    let err = b.spawn(spec("/definitely/not/a/program", &[])).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::NotFound, "{err}");
    let err = b.spawn(spec("emax-no-such-program-on-path", &[])).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::NotFound, "{err}");
    assert_eq!(b.spawn(spec("", &[])).unwrap_err().kind(), io::ErrorKind::InvalidInput);
    let dir = temp_dir("notexec");
    let file = dir.join("script");
    std::fs::write(&file, "#!/bin/sh\necho hi\n").unwrap();
    assert!(b.spawn(spec(file.to_str().unwrap(), &[])).is_err());
    assert!(b.spawn(spec(dir.to_str().unwrap(), &[])).is_err());
    assert!(b.list().unwrap().is_empty());
    assert_eq!(b.try_recv(), None);
    assert_eq!(live.load(Ordering::SeqCst), 0);
    // The backend still works afterwards.
    let id = b.spawn(spec("/bin/echo", &["still", "fine"])).unwrap();
    let mut seen = Seen::default();
    seen.until_exit(&mut b, id);
    assert_eq!(seen.text(id), "still fine\r\n");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn unknown_sessions_are_not_found() {
    let mut b = backend();
    let id = SessionId(99);
    let nf = io::ErrorKind::NotFound;
    assert_eq!(b.write(id, b"x").unwrap_err().kind(), nf);
    assert_eq!(b.resize(id, PtySize::default()).unwrap_err().kind(), nf);
    assert_eq!(b.attach(id, PtySize::default()).unwrap_err().kind(), nf);
    assert_eq!(b.detach(id).unwrap_err().kind(), nf);
    assert_eq!(b.kill(id).unwrap_err().kind(), nf);
    assert!(!b.persistent());
    assert_eq!(b.pid(id), None);
}

#[test]
fn kill_ends_the_process_and_silences_it() {
    let mut b = backend();
    let id = b.spawn(sh("echo started; while :; do echo spam; sleep 0.01; done")).unwrap();
    let mut seen = Seen::default();
    seen.until(&mut b, "start", |s| s.text(id).contains("started"));
    let pid = b.pid(id).unwrap();
    b.kill(id).unwrap();
    wait_until("killed process to be reaped", TIMEOUT, || !pid_exists(pid));
    // Nothing more for the killed session, ever.
    let before = seen.events[&id];
    seen.drain(&mut b);
    assert_eq!(seen.events[&id], before);
    assert!(seen.exited(id).is_none());
    assert_eq!(b.write(id, b"x").unwrap_err().kind(), io::ErrorKind::NotFound);
    assert_eq!(b.kill(id).unwrap_err().kind(), io::ErrorKind::NotFound);
    assert!(b.list().unwrap().is_empty());
    let live = b.live_threads();
    wait_until("killed session's threads to end", TIMEOUT, || {
        b.reap_dying();
        live.load(Ordering::SeqCst) == 0 && b.dying.is_empty()
    });
}

#[test]
fn kill_escalates_to_sigkill() {
    let mut b = backend();
    // Ignores SIGHUP (and the SIGCONT is harmless): only SIGKILL ends it.
    let id = b.spawn(sh("trap '' HUP; echo ready; while :; do sleep 0.05; done")).unwrap();
    let mut seen = Seen::default();
    seen.until(&mut b, "ready", |s| s.text(id).contains("ready"));
    let pid = b.pid(id).unwrap();
    b.kill(id).unwrap();
    wait_until("HUP-ignoring process to be killed", TIMEOUT, || !pid_exists(pid));
}

#[test]
fn kill_stopped_process() {
    let mut b = backend();
    let id = b.spawn(sh("echo stopping; kill -STOP $$; echo resumed")).unwrap();
    let mut seen = Seen::default();
    seen.until(&mut b, "stopping", |s| s.text(id).contains("stopping"));
    let pid = b.pid(id).unwrap();
    b.kill(id).unwrap();
    wait_until("stopped process to be killed", TIMEOUT, || !pid_exists(pid));
}

#[test]
fn kill_reaches_the_process_group() {
    let mut b = backend();
    let id = b.spawn(sh("sleep 300 & echo bg=$!; wait")).unwrap();
    let mut seen = Seen::default();
    seen.until(&mut b, "background pid", |s| s.text(id).contains("\r\n"));
    let text = seen.text(id);
    let bg: u32 = text.trim().strip_prefix("bg=").unwrap().parse().unwrap();
    assert!(pid_exists(bg));
    b.kill(id).unwrap();
    // `sleep` is not our child, so it may linger as a zombie until init reaps it;
    // signal 0 still succeeds on zombies, so check its state with ps.
    wait_until("background job to die", TIMEOUT, || !process_alive(bg));
}

/// Running (not a zombie, not gone).
fn process_alive(pid: u32) -> bool {
    let out = std::process::Command::new("ps").args(["-o", "stat=", "-p", &pid.to_string()]).output().unwrap();
    let stat = String::from_utf8_lossy(&out.stdout);
    let stat = stat.trim();
    !stat.is_empty() && !stat.starts_with('Z')
}

#[test]
fn drop_kills_children_and_joins_threads() {
    let mut b = backend();
    let mut pids = Vec::new();
    let mut ids = Vec::new();
    for script in ["while :; do sleep 0.05; done", "trap '' HUP; echo ready; while :; do sleep 0.05; done", "cat"] {
        let id = b.spawn(sh(script)).unwrap();
        pids.push(b.pid(id).unwrap());
        ids.push(id);
    }
    // One more that is killed but whose threads may still be running.
    let k = b.spawn(sh("trap '' HUP; echo ready; while :; do sleep 0.05; done")).unwrap();
    pids.push(b.pid(k).unwrap());
    let mut seen = Seen::default();
    seen.until(&mut b, "traps installed", |s| s.text(ids[1]).contains("ready") && s.text(k).contains("ready"));
    b.kill(k).unwrap();
    let live = b.live_threads();
    // Three threads per running session (the killed one is still in its grace period).
    assert!(live.load(Ordering::SeqCst) >= 9);
    drop(b);
    assert_eq!(live.load(Ordering::SeqCst), 0, "threads left running after drop");
    for pid in pids {
        assert!(!pid_exists(pid), "process {pid} survived the backend");
    }
}

#[test]
fn drop_with_a_full_queue_does_not_hang() {
    let mut b = backend();
    let id = b.spawn(sh("while :; do head -c 100000 /dev/zero; done")).unwrap();
    wait_until("the event queue to fill", TIMEOUT, || b.queued() >= EVENT_QUEUE);
    let pid = b.pid(id).unwrap();
    let live = b.live_threads();
    drop(b);
    assert_eq!(live.load(Ordering::SeqCst), 0);
    assert!(!pid_exists(pid));
}

#[test]
fn waker_is_called_once_per_batch() {
    let wakes = Arc::new(AtomicUsize::new(0));
    let w = wakes.clone();
    let mut b = LocalBackend::new(Arc::new(move || {
        w.fetch_add(1, Ordering::SeqCst);
    }));
    let id = b.spawn(sh("head -c 30000000 /dev/zero")).unwrap();
    // Many events pile up without draining: still a single wake-up.
    wait_until("many events queued", TIMEOUT, || b.queued() >= EVENT_QUEUE);
    assert_eq!(wakes.load(Ordering::SeqCst), 1);
    // Draining to empty re-arms the waker; more output wakes again.
    let mut seen = Seen::default();
    wait_until("exit", Duration::from_secs(60), || {
        seen.drain(&mut b);
        seen.exited(id).is_some()
    });
    let n = wakes.load(Ordering::SeqCst);
    assert!(n >= 2);
    // At most one wake-up per event (each needs an empty drain in between).
    assert!(n <= seen.events[&id] + 1, "{n} wakes for {} events", seen.events[&id]);
}

#[test]
fn waker_fires_for_output_and_exit() {
    let wakes = Arc::new(AtomicUsize::new(0));
    let w = wakes.clone();
    let mut b = LocalBackend::new(Arc::new(move || {
        w.fetch_add(1, Ordering::SeqCst);
    }));
    let id = b.spawn(spec("/bin/cat", &[])).unwrap();
    b.write(id, b"ping\n").unwrap();
    wait_until("wake for output", TIMEOUT, || wakes.load(Ordering::SeqCst) == 1);
    let mut seen = Seen::default();
    seen.until(&mut b, "ping", |s| count(&s.text(id), "ping") == 2);
    seen.drain(&mut b); // empty: re-arms
    let n = wakes.load(Ordering::SeqCst);
    b.write(id, b"\x04").unwrap();
    wait_until("wake for exit", TIMEOUT, || wakes.load(Ordering::SeqCst) > n);
    assert!(seen.until_exit(&mut b, id).success());
}

#[test]
fn many_sessions_in_parallel() {
    let mut b = backend();
    let ids: Vec<SessionId> =
        (0..32).map(|i| b.spawn(sh(&format!("echo session-$((40 + {i})); read x; echo got-$x; exit {i}"))).unwrap()).collect();
    let mut seen = Seen::default();
    seen.until(&mut b, "every session started", |s| {
        ids.iter().enumerate().all(|(i, id)| s.text(*id).contains(&format!("session-{}", 40 + i)))
    });
    let pids: std::collections::BTreeSet<u32> = ids.iter().map(|id| b.pid(*id).unwrap()).collect();
    assert_eq!(pids.len(), 32);
    for (i, id) in ids.iter().enumerate() {
        b.write(*id, format!("{i}\n").as_bytes()).unwrap();
    }
    for (i, id) in ids.iter().enumerate() {
        assert_eq!(seen.until_exit(&mut b, *id).code, i as u32);
        assert!(seen.text(*id).contains(&format!("got-{i}\r\n")));
        // No cross-talk.
        assert_eq!(count(&seen.text(*id), "session-"), 1);
    }
    assert_eq!(b.list().unwrap().iter().filter(|s| s.exited.is_some()).count(), 32);
}

#[test]
fn large_input_does_not_block_the_caller() {
    let mut b = backend();
    let id = b.spawn(sh("stty raw -echo; echo ready; exec cat")).unwrap();
    let mut seen = Seen::default();
    seen.until(&mut b, "ready", |s| s.text(id).contains("ready"));
    let start = seen.out[&id].len();
    // Far more than the PTY's input buffer; cat's output isn't being drained
    // while we write, so a blocking write here would deadlock.
    let line = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ\n";
    let mut total = 0;
    for _ in 0..16_000 {
        b.write(id, line).unwrap();
        total += line.len();
    }
    seen.until(&mut b, "everything echoed back", |s| s.out[&id].len() - start >= total);
    assert_eq!(seen.out[&id].len() - start, total);
    assert!(seen.out[&id][start..].chunks(line.len()).all(|c| c == line));
    b.kill(id).unwrap();
}

#[test]
fn writes_from_another_thread_reach_the_process() {
    // The backend is not shared, but it can live on (and be used from) a
    // thread other than the one that created it.
    let mut b = backend();
    let id = b.spawn(spec("/bin/cat", &[])).unwrap();
    let mut b = std::thread::spawn(move || {
        b.write(id, b"from a thread\n").unwrap();
        b
    })
    .join()
    .unwrap();
    let mut seen = Seen::default();
    seen.until(&mut b, "thread input echoed", |s| count(&s.text(id), "from a thread") == 2);
}

#[test]
fn detach_drops_output_and_attach_resumes() {
    let mut b = backend();
    let id = b.spawn(sh("stty -echo; echo ready; read x; echo after; read y; stty size; echo final; read z")).unwrap();
    let mut seen = Seen::default();
    seen.until(&mut b, "ready", |s| s.text(id).contains("ready"));
    b.detach(id).unwrap();
    b.write(id, b"\n").unwrap();
    // "ready\r\n" + "after\r\n" read by the reader while detached.
    wait_until("detached output read", TIMEOUT, || b.bytes_read(id) == Some(14));
    seen.drain(&mut b);
    assert!(!seen.text(id).contains("after"));
    b.attach(id, PtySize::new(70, 20)).unwrap();
    b.attach(id, PtySize::new(71, 21)).unwrap(); // attaching twice is fine
    b.write(id, b"\n").unwrap();
    seen.until(&mut b, "final", |s| s.text(id).contains("final"));
    assert!(seen.text(id).contains("21 71"));
    assert!(!seen.text(id).contains("after"));
    b.write(id, b"\n").unwrap();
    assert!(seen.until_exit(&mut b, id).success());
}

#[test]
fn exit_while_detached_is_reported_on_attach() {
    let mut b = backend();
    let id = b.spawn(sh("echo ready; read x; echo bye; exit 4")).unwrap();
    let mut seen = Seen::default();
    seen.until(&mut b, "ready", |s| s.text(id).contains("ready"));
    b.detach(id).unwrap();
    b.write(id, b"\n").unwrap();
    wait_until("exit while detached", TIMEOUT, || b.list().unwrap()[0].exited.is_some());
    wait_until("reader finished", TIMEOUT, || b.sessions[&id].shared.lock().done);
    seen.drain(&mut b);
    assert!(seen.exited(id).is_none());
    assert_eq!(b.write(id, b"x").unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    // Resizing an exited session just records the size.
    b.resize(id, PtySize::new(10, 10)).unwrap();
    b.attach(id, PtySize::new(90, 30)).unwrap();
    assert_eq!(seen.until_exit(&mut b, id).code, 4);
    // Exactly once, even after re-attaching.
    b.attach(id, PtySize::new(90, 30)).unwrap();
    seen.drain(&mut b);
    assert_eq!(seen.exits[&id].len(), 1);
    // Exited sessions stay listed until killed.
    let info = &b.list().unwrap()[0];
    assert_eq!(info.exited, Some(ExitStatus { code: 4, signal: None }));
    assert_eq!(info.size, PtySize::new(90, 30));
    b.kill(id).unwrap();
    assert!(b.list().unwrap().is_empty());
}

#[test]
fn quick_exits_never_lose_output() {
    // Programs that print and exit at once race the reader's start-up.
    let mut b = backend();
    let mut seen = Seen::default();
    let ids: Vec<SessionId> = (0..200).map(|i| b.spawn(spec("/bin/echo", &["quick", &i.to_string()])).unwrap()).collect();
    for (i, id) in ids.iter().enumerate() {
        seen.until_exit(&mut b, *id);
        assert_eq!(seen.text(*id), format!("quick {i}\r\n"), "session {i} {:?}", seen.exited(*id));
    }
}

#[test]
fn list_describes_sessions() {
    let mut b = backend();
    let mut s = spec("/bin/cat", &["-u"]);
    s.size = PtySize::new(100, 40);
    let id = b.spawn(s).unwrap();
    let info = b.list().unwrap();
    assert_eq!(info.len(), 1);
    assert_eq!(info[0].id, id);
    assert_eq!(info[0].program, "/bin/cat");
    assert_eq!(info[0].args, ["-u"]);
    assert_eq!(info[0].cwd, None);
    assert_eq!(info[0].size, PtySize::new(100, 40));
    assert_eq!(info[0].exited, None);
    let pid = info[0].pid.unwrap();
    assert!(pid_exists(pid));
    assert_eq!(b.pid(id), Some(pid));
    // Ids are never reused.
    b.kill(id).unwrap();
    let id2 = b.spawn(spec("/bin/cat", &[])).unwrap();
    assert!(id2 > id);
}

#[test]
fn spawn_after_many_kills_does_not_leak() {
    let mut b = backend();
    let live = b.live_threads();
    for _ in 0..20 {
        let id = b.spawn(spec("/bin/cat", &[])).unwrap();
        b.kill(id).unwrap();
    }
    wait_until("killed sessions' threads to end", TIMEOUT, || {
        b.reap_dying();
        live.load(Ordering::SeqCst) == 0
    });
    assert!(b.dying.is_empty());
    assert_eq!(b.try_recv(), None);
}
