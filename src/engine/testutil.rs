//! Helpers shared by tests: polling with a timeout (never fixed sleeps) and
//! unique temporary directories (never the user's real config or state).

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Poll `cond` until it holds, failing the test after `timeout`.
pub fn wait_until(what: &str, timeout: Duration, mut cond: impl FnMut() -> bool) {
    let start = Instant::now();
    let mut pause = Duration::from_millis(1);
    while !cond() {
        if start.elapsed() > timeout {
            panic!("timed out after {timeout:?} waiting for {what}");
        }
        std::thread::sleep(pause);
        pause = (pause * 2).min(Duration::from_millis(20));
    }
}

/// The default timeout for `wait_until` in tests that involve processes.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// For waits on megabytes of output: a few seconds on a laptop, several
/// times that on a shared CI runner, more in debug builds where the daemon
/// also emulates a terminal for every byte. These waits are about
/// throughput, not about catching a hang, so they get much longer.
pub const THROUGHPUT_TIMEOUT: Duration = Duration::from_secs(60);

/// A fresh, empty directory unique to this call, under the system temp dir.
/// Kept short: Unix socket paths must fit in ~100 bytes on macOS.
pub fn temp_dir(tag: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let d = std::env::temp_dir().join(format!("ymacs-t{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("create temp dir");
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_dirs_are_unique_and_short() {
        let a = temp_dir("x");
        let b = temp_dir("x");
        assert_ne!(a, b);
        assert!(a.join("ptyd.sock").as_os_str().len() < 100, "{}", a.display());
        std::fs::remove_dir_all(a).unwrap();
        std::fs::remove_dir_all(b).unwrap();
    }

    #[test]
    fn wait_until_polls() {
        let start = Instant::now();
        wait_until("a short while", TIMEOUT, || start.elapsed() > Duration::from_millis(5));
    }

    #[test]
    #[should_panic(expected = "timed out")]
    fn wait_until_times_out() {
        wait_until("never", Duration::from_millis(10), || false);
    }
}
