//! The session daemon (`emax --pty-daemon`): owns PTY sessions so they
//! survive the editor, keeps the last `replay_bytes` of each session's output
//! in a ring buffer, and serves clients over a Unix socket (see
//! [`super::protocol`]).
//!
//! SKELETON: owned by the persistence builder (see
//! docs/design/terminal-substrate.md). Running it currently fails.

use std::io;
use std::path::PathBuf;

/// How a daemon runs.
#[derive(Clone, Debug, PartialEq)]
pub struct DaemonOptions {
    pub socket: PathBuf,
    /// Output kept per session for replay on attach.
    pub replay_bytes: usize,
    /// Exit when there have been no sessions and no clients for this long
    /// (`None`: never).
    pub idle_exit: Option<std::time::Duration>,
}

/// Serve on `opts.socket` until shut down. Blocks.
pub fn run(_opts: DaemonOptions) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "the PTY daemon is not implemented yet"))
}

/// The `emax --pty-daemon [--socket PATH] [--replay-bytes N]` entry point:
/// parse `args` (everything after `--pty-daemon`), detach from the
/// terminal and run. Returns the process exit code.
pub fn main(args: &[String]) -> i32 {
    let _ = args;
    match run(DaemonOptions { socket: super::default_socket_path(), replay_bytes: 2 << 20, idle_exit: None }) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("emax --pty-daemon: {e}");
            1
        }
    }
}
