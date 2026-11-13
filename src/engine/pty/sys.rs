//! Thin wrappers over the Unix calls [`super::local`] needs and
//! `portable-pty` doesn't offer: non-blocking descriptors, waiting on a
//! descriptor *or* a cancel pipe (so I/O threads can always be stopped and
//! joined), `waitpid` with a portable exit status, and signals.
//!
//! Waiting uses `select` rather than `poll` because macOS `poll` does not
//! support character devices such as PTYs.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::PathBuf;

use super::ExitStatus;

/// Duplicate `fd` (close-on-exec, so no spawned child inherits it).
pub fn dup_cloexec(fd: RawFd) -> io::Result<OwnedFd> {
    let new = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if new < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(new) })
}

/// Make reads and writes on `fd` return `WouldBlock` instead of blocking.
/// The flag belongs to the open file description, so it applies to every
/// duplicate of `fd` too.
pub fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `select` can only watch descriptors below `FD_SETSIZE`.
pub fn check_selectable(fd: RawFd) -> io::Result<()> {
    if fd < 0 || fd as usize >= libc::FD_SETSIZE {
        return Err(io::Error::other(format!("file descriptor {fd} is too large to wait on")));
    }
    Ok(())
}

/// A pipe used to stop a thread waiting in [`wait_fd`]: once [`Cancel::fire`]
/// is called the read end stays readable forever, so every waiter sees it.
pub struct Cancel {
    read: OwnedFd,
    write: OwnedFd,
}

impl Cancel {
    pub fn new() -> io::Result<Self> {
        let mut fds = [0 as RawFd; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        for fd in [&read, &write] {
            let raw = fd.as_raw_fd();
            unsafe { libc::fcntl(raw, libc::F_SETFD, libc::FD_CLOEXEC) };
            check_selectable(raw)?;
        }
        set_nonblocking(write.as_raw_fd())?;
        Ok(Self { read, write })
    }

    /// Wake every current and future waiter. Idempotent (a full pipe is fine).
    pub fn fire(&self) {
        let byte = 1u8;
        unsafe { libc::write(self.write.as_raw_fd(), (&byte as *const u8).cast(), 1) };
    }

    pub fn fd(&self) -> RawFd {
        self.read.as_raw_fd()
    }
}

/// What [`wait_fd`] saw.
#[derive(Debug, PartialEq, Eq)]
pub enum Ready {
    /// `fd` is ready for the requested direction.
    Io,
    /// The cancel pipe fired.
    Cancelled,
}

/// Block until `fd` is readable (or writable, if `write`) or `cancel` is
/// readable. Cancellation wins when both are ready.
pub fn wait_fd(fd: RawFd, write: bool, cancel: RawFd) -> io::Result<Ready> {
    loop {
        let mut rd: libc::fd_set = unsafe { std::mem::zeroed() };
        let mut wr: libc::fd_set = unsafe { std::mem::zeroed() };
        unsafe {
            libc::FD_ZERO(&mut rd);
            libc::FD_ZERO(&mut wr);
            libc::FD_SET(cancel, &mut rd);
            if write {
                libc::FD_SET(fd, &mut wr);
            } else {
                libc::FD_SET(fd, &mut rd);
            }
        }
        let n =
            unsafe { libc::select(fd.max(cancel) + 1, &mut rd, &mut wr, std::ptr::null_mut(), std::ptr::null_mut()) };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if unsafe { libc::FD_ISSET(cancel, &rd) } {
            return Ok(Ready::Cancelled);
        }
        return Ok(Ready::Io);
    }
}

/// One `read(2)`; `Ok(0)` is end of file.
pub fn read(fd: RawFd, buf: &mut [u8]) -> io::Result<usize> {
    let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
    if n < 0 { Err(io::Error::last_os_error()) } else { Ok(n as usize) }
}

/// One `write(2)`.
pub fn write(fd: RawFd, buf: &[u8]) -> io::Result<usize> {
    let n = unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) };
    if n < 0 { Err(io::Error::last_os_error()) } else { Ok(n as usize) }
}

/// Write all of `buf` to the non-blocking `fd`, waiting for room as needed.
/// Fails with `Interrupted` if `cancel` fires first.
pub fn write_all(fd: RawFd, mut buf: &[u8], cancel: RawFd) -> io::Result<()> {
    while !buf.is_empty() {
        match write(fd, buf) {
            Ok(n) => buf = &buf[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                if wait_fd(fd, true, cancel)? == Ready::Cancelled {
                    return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
                }
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Set the terminal size of the PTY whose master is `fd` (the kernel then
/// sends `SIGWINCH` to the foreground process group).
pub fn set_winsize(fd: RawFd, cols: u16, rows: u16, pixel_width: u16, pixel_height: u16) -> io::Result<()> {
    let ws = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: pixel_width, ws_ypixel: pixel_height };
    if unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &ws) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Block until child `pid` exits and reap it.
pub fn wait_pid(pid: u32) -> io::Result<ExitStatus> {
    let mut status = 0;
    loop {
        let r = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, 0) };
        if r >= 0 {
            return Ok(decode_status(status));
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// Turn a `waitpid` status into an [`ExitStatus`]: the exit code, or code 1
/// plus the signal's name (as `portable-pty` reports it).
pub fn decode_status(status: libc::c_int) -> ExitStatus {
    if libc::WIFSIGNALED(status) {
        ExitStatus { code: 1, signal: Some(signal_name(libc::WTERMSIG(status))) }
    } else if libc::WIFEXITED(status) {
        ExitStatus { code: libc::WEXITSTATUS(status) as u32, signal: None }
    } else {
        ExitStatus { code: 1, signal: None }
    }
}

/// A stable, platform-independent name for a signal (`strsignal` differs
/// between systems, e.g. "Hangup: 1" on macOS).
pub fn signal_name(sig: libc::c_int) -> String {
    let name = match sig {
        libc::SIGHUP => "Hangup",
        libc::SIGINT => "Interrupt",
        libc::SIGQUIT => "Quit",
        libc::SIGILL => "Illegal instruction",
        libc::SIGTRAP => "Trace/BPT trap",
        libc::SIGABRT => "Abort trap",
        libc::SIGBUS => "Bus error",
        libc::SIGFPE => "Floating point exception",
        libc::SIGKILL => "Killed",
        libc::SIGUSR1 => "User defined signal 1",
        libc::SIGSEGV => "Segmentation fault",
        libc::SIGUSR2 => "User defined signal 2",
        libc::SIGPIPE => "Broken pipe",
        libc::SIGALRM => "Alarm clock",
        libc::SIGTERM => "Terminated",
        _ => return format!("Signal {sig}"),
    };
    name.to_string()
}

/// Send `sig` to process `pid` and its process group (a PTY child leads its
/// own session and group, so this reaches its pipelines too). Errors are
/// ignored: the process may already be gone.
pub fn signal_group(pid: u32, sig: libc::c_int) {
    let pid = pid as libc::pid_t;
    if pid <= 0 {
        return;
    }
    unsafe {
        libc::kill(-pid, sig);
        libc::kill(pid, sig);
    }
}

/// The executable and command line of process `pid` (`None` if it is gone
/// or not ours to look at). The executable is the file it runs (symlinks
/// resolved), the command line what it was started with (`argv[0]` may be
/// the name it was invoked by, e.g. a symlink).
pub fn process_command(pid: u32) -> Option<(PathBuf, Vec<String>)> {
    let pid = libc::pid_t::try_from(pid).ok().filter(|&p| p > 0)?;
    imp::process_command(pid)
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;

    pub fn process_command(pid: libc::pid_t) -> Option<(PathBuf, Vec<String>)> {
        let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        // SAFETY: the buffer is as large as we say.
        let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
        if n <= 0 {
            return None;
        }
        let exe = PathBuf::from(OsStr::from_bytes(&buf[..n as usize]));
        Some((exe, argv(pid).unwrap_or_default()))
    }

    /// `KERN_PROCARGS2`: argc (native int), the exec path, NUL padding, then
    /// argc NUL-terminated arguments (then the environment, not read).
    fn argv(pid: libc::pid_t) -> Option<Vec<String>> {
        let mut max: libc::c_int = 0;
        let mut size = std::mem::size_of::<libc::c_int>();
        let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
        // SAFETY: valid mib and an out-parameter of the size given.
        let r = unsafe { libc::sysctl(mib.as_mut_ptr(), 2, (&raw mut max).cast(), &mut size, std::ptr::null_mut(), 0) };
        if r != 0 || max <= 0 {
            return None;
        }
        let mut buf = vec![0u8; max as usize];
        let mut size = buf.len();
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        // SAFETY: valid mib and a buffer of the size given.
        let r =
            unsafe { libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast(), &mut size, std::ptr::null_mut(), 0) };
        if r != 0 {
            return None;
        }
        parse_procargs(&buf[..size])
    }

    pub(super) fn parse_procargs(buf: &[u8]) -> Option<Vec<String>> {
        let int = std::mem::size_of::<libc::c_int>();
        let argc = libc::c_int::from_ne_bytes(buf.get(..int)?.try_into().ok()?);
        let mut rest = &buf[int..];
        // Skip the exec path and the padding after it.
        let path_end = rest.iter().position(|&b| b == 0)?;
        rest = &rest[path_end..];
        let start = rest.iter().position(|&b| b != 0)?;
        rest = &rest[start..];
        let args = rest
            .split(|&b| b == 0)
            .take(argc.max(0) as usize)
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect();
        Some(args)
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use std::path::PathBuf;

    pub fn process_command(pid: libc::pid_t) -> Option<(PathBuf, Vec<String>)> {
        let dir = PathBuf::from(format!("/proc/{pid}"));
        let exe = std::fs::read_link(dir.join("exe")).ok().or_else(|| {
            // Not readable (another user's): the 15-character `comm` at least.
            std::fs::read_to_string(dir.join("comm")).ok().map(|c| PathBuf::from(c.trim_end()))
        })?;
        let argv = std::fs::read(dir.join("cmdline"))
            .map(|b| {
                b.split(|&c| c == 0)
                    .filter(|a| !a.is_empty())
                    .map(|a| String::from_utf8_lossy(a).into_owned())
                    .collect()
            })
            .unwrap_or_default();
        Some((exe, argv))
    }
}

/// Whether a process with this pid exists (zombies count).
#[cfg(test)]
pub fn pid_exists(pid: u32) -> bool {
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_decoding() {
        // Encoded by hand as the kernel does: exit code in bits 8..16, signal in 0..7.
        assert_eq!(decode_status(0), ExitStatus { code: 0, signal: None });
        assert_eq!(decode_status(7 << 8), ExitStatus { code: 7, signal: None });
        assert_eq!(decode_status(255 << 8), ExitStatus { code: 255, signal: None });
        assert_eq!(decode_status(libc::SIGHUP), ExitStatus { code: 1, signal: Some("Hangup".into()) });
        assert_eq!(decode_status(libc::SIGKILL), ExitStatus { code: 1, signal: Some("Killed".into()) });
        // Core dump flag (0x80) doesn't change the signal.
        assert_eq!(decode_status(libc::SIGSEGV | 0x80).signal.as_deref(), Some("Segmentation fault"));
    }

    #[test]
    fn signal_names_are_stable() {
        assert_eq!(signal_name(libc::SIGTERM), "Terminated");
        assert_eq!(signal_name(libc::SIGINT), "Interrupt");
        assert_eq!(signal_name(1000), "Signal 1000");
    }

    #[test]
    fn cancel_wakes_every_waiter() {
        let c = Cancel::new().unwrap();
        let other = Cancel::new().unwrap();
        c.fire();
        c.fire();
        // The other pipe never becomes readable; cancellation still returns.
        assert_eq!(wait_fd(other.fd(), false, c.fd()).unwrap(), Ready::Cancelled);
        assert_eq!(wait_fd(other.fd(), false, c.fd()).unwrap(), Ready::Cancelled);
    }

    #[test]
    fn write_all_through_a_pipe_and_cancel_when_full() {
        let c = Cancel::new().unwrap();
        let p = Cancel::new().unwrap(); // reuse as a plain pipe: write end -> read end
        set_nonblocking(p.write.as_raw_fd()).unwrap();
        write_all(p.write.as_raw_fd(), b"hello", c.fd()).unwrap();
        let mut buf = [0u8; 16];
        assert_eq!(read(p.fd(), &mut buf).unwrap(), 5);
        assert_eq!(&buf[..5], b"hello");
        // Nobody reads: the pipe fills up and the write waits until cancelled.
        let big = vec![b'x'; 4 << 20];
        let fd = p.write.as_raw_fd();
        let cancel = c.fd();
        let t = std::thread::spawn(move || write_all(fd, &big, cancel));
        c.fire();
        assert_eq!(t.join().unwrap().unwrap_err().kind(), io::ErrorKind::Interrupted);
    }

    #[test]
    fn own_pid_exists() {
        assert!(pid_exists(std::process::id()));
        assert!(!pid_exists(0x7fff_fff0));
        assert!(check_selectable(3).is_ok());
        assert!(check_selectable(libc::FD_SETSIZE as RawFd).is_err());
        assert!(check_selectable(-1).is_err());
    }

    #[test]
    fn our_own_command_is_readable() {
        let (exe, argv) = process_command(std::process::id()).unwrap();
        assert_eq!(exe, std::env::current_exe().unwrap().canonicalize().unwrap());
        assert_eq!(argv, std::env::args().collect::<Vec<_>>());
        assert_eq!(process_command(0), None);
        assert_eq!(process_command(u32::MAX), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn procargs_are_parsed() {
        let mut buf = 2i32.to_ne_bytes().to_vec();
        buf.extend(b"/bin/exe\0\0\0arg0\0arg one\0HOME=/x\0");
        assert_eq!(imp::parse_procargs(&buf).unwrap(), ["arg0", "arg one"]);
        assert_eq!(imp::parse_procargs(&[1, 0]), None);
    }
}
