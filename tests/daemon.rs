//! The real session daemon: the `emax` binary started with `--pty-daemon`
//! on a temporary socket, reached through `DaemonBackend` like the editor
//! does. Every daemon started here also has an idle exit, so none outlives
//! the tests even if one fails half-way.

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use emax::engine::pty::{PtyBackend, PtyEvent, PtySize, SessionId, SpawnSpec, noop_waker};
use emax::engine::session::client::DaemonBackend;
use emax::engine::session::daemon::exit_code;

const TIMEOUT: Duration = Duration::from_secs(10);
const EMAX: &str = env!("CARGO_BIN_EXE_emax");

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(start.elapsed() < TIMEOUT, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A fresh directory for one test's socket (short: socket paths are limited).
fn temp_dir(tag: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let d =
        std::env::temp_dir().join(format!("emax-i{}-{tag}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn start(socket: &Path) -> DaemonBackend {
    DaemonBackend::connect_or_start_with(Path::new(EMAX), socket, 1 << 20, Some(Duration::from_secs(20)), noop_waker())
        .expect("start the daemon")
}

/// Read events until session `id`'s output contains `needle`; all its output so far.
fn read_until(b: &mut DaemonBackend, id: SessionId, needle: &str) -> String {
    let mut out = Vec::new();
    wait_until(&format!("{needle:?} in the output"), || {
        while let Some(e) = b.try_recv() {
            if let PtyEvent::Output(s, d) = e
                && s == id
            {
                out.extend_from_slice(&d);
            }
        }
        String::from_utf8_lossy(&out).contains(needle)
    });
    String::from_utf8_lossy(&out).into_owned()
}

fn shell(script: &str) -> SpawnSpec {
    SpawnSpec {
        program: "/bin/sh".into(),
        args: vec!["-c".into(), script.into()],
        env: vec![("TERM".into(), "dumb".into())],
        size: PtySize::new(80, 24),
        ..Default::default()
    }
}

fn shut_down(mut b: DaemonBackend, socket: &Path) {
    b.shutdown().unwrap();
    wait_until("the socket to be removed", || !socket.exists());
}

#[test]
fn sessions_outlive_their_client_and_replay_on_reattach() {
    let dir = temp_dir("replay");
    let socket = dir.join("run").join("d.sock");
    let mut b = start(&socket);
    assert_ne!(b.daemon_pid(), std::process::id(), "a separate process");
    assert_eq!(std::fs::metadata(socket.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);

    let id = match b.spawn(shell("printf 'ready-%s\\n' $((6*7)); exec cat")) {
        Ok(id) => id,
        Err(e) if e.to_string().contains("not implemented") => {
            // The daemon runs programs with the local PTY backend; until it
            // exists only the plumbing can be checked.
            eprintln!("skipping the process part: {e}");
            assert!(b.list().unwrap().is_empty());
            shut_down(b, &socket);
            return std::fs::remove_dir_all(dir).unwrap();
        }
        Err(e) => panic!("spawn: {e}"),
    };
    read_until(&mut b, id, "ready-42");
    b.write(id, b"ping\n").unwrap();
    read_until(&mut b, id, "ping");
    drop(b); // the editor quits

    let mut b = DaemonBackend::connect(&socket, noop_waker()).unwrap();
    let list = b.list().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!((list[0].id, list[0].exited.clone()), (id, None), "still running");
    let pid = list[0].pid.expect("a pid");
    b.attach(id, PtySize::new(100, 30)).unwrap();
    let replay = read_until(&mut b, id, "ping");
    assert!(replay.contains("ready-42"), "{replay:?}");
    b.write(id, b"pong\n").unwrap();
    read_until(&mut b, id, "pong");

    shut_down(b, &socket);
    // SAFETY: kill with signal 0 only checks whether the process exists.
    wait_until("the session's process to end", || unsafe { libc::kill(pid as i32, 0) } != 0);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_stale_socket_is_replaced_by_a_new_daemon() {
    let dir = temp_dir("stale");
    let socket = dir.join("d.sock");
    // The socket file of a daemon that died.
    drop(UnixListener::bind(&socket).unwrap());
    let mut b = start(&socket);
    assert!(b.list().unwrap().is_empty());
    // A second editor reaches the same daemon instead of starting another.
    let mut c = start(&socket);
    assert_eq!(b.daemon_pid(), c.daemon_pid());
    assert!(c.list().unwrap().is_empty());
    drop(c);
    shut_down(b, &socket);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn an_idle_daemon_goes_away() {
    let dir = temp_dir("idle");
    let socket = dir.join("d.sock");
    let b = DaemonBackend::connect_or_start_with(
        Path::new(EMAX),
        &socket,
        1024,
        Some(Duration::from_secs(1)),
        noop_waker(),
    )
    .unwrap();
    drop(b);
    wait_until("the idle daemon to exit", || !socket.exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn the_daemon_command_line() {
    let dir = temp_dir("cli");
    let run = |args: &[&str]| Command::new(EMAX).arg("--pty-daemon").args(args).output().unwrap();
    let out = run(&["--bogus"]);
    assert_eq!(out.status.code(), Some(exit_code::USAGE));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--bogus"));

    // Can't bind: a file where the socket's directory should be.
    std::fs::write(dir.join("file"), "").unwrap();
    let bad = dir.join("file").join("d.sock");
    assert_eq!(run(&["--socket", bad.to_str().unwrap()]).status.code(), Some(exit_code::ERROR));

    // A second daemon for a live socket says so (and the first keeps serving).
    let socket = dir.join("d.sock");
    let b = start(&socket);
    let out = run(&["--socket", socket.to_str().unwrap(), "--foreground"]);
    assert_eq!(out.status.code(), Some(exit_code::IN_USE), "{}", String::from_utf8_lossy(&out.stderr));
    shut_down(b, &socket);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_detached_daemon_with_a_relative_socket_path_still_serves_and_exits() {
    // The detached daemon moves to $HOME; the relative path must keep
    // naming the socket it bound (for its log, its idle exit and cleanup).
    let dir = temp_dir("rel");
    let out = Command::new(EMAX)
        .current_dir(&dir)
        .args(["--pty-daemon", "--socket", "d.sock", "--idle-exit", "1"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(exit_code::OK), "{}", String::from_utf8_lossy(&out.stderr));
    let socket = dir.join("d.sock");
    assert!(socket.exists());
    let mut b = DaemonBackend::connect(&socket, noop_waker()).expect("connect to the detached daemon");
    let id = b.spawn(shell("echo rel-$((20+1))")).unwrap();
    read_until(&mut b, id, "rel-21");
    b.kill(id).unwrap();
    drop(b);
    wait_until("the idle daemon to exit and remove its socket", || !socket.exists());
    assert!(dir.join("d.log").exists(), "the log sits next to the socket");
    std::fs::remove_dir_all(dir).unwrap();
}
