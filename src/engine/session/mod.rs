//! Session persistence, tmux-style: terminal processes live in a daemon
//! ([`daemon`], reached through [`client::DaemonBackend`]) so they survive
//! the editor, and the editor's tabs, windows and buffers are saved to a
//! session file on quit and restored on start (`session.restore_session`).
//!
//! The session file ([`SessionFile`], JSON) is written atomically to
//! [`session_file_path`]. Buffer and window ids in it are the saved editor's;
//! restoring maps them to fresh ids.
//!
//! SKELETON: the paths, file schema and save/load are in place; snapshot and
//! restore are owned by the persistence builder (see
//! docs/design/terminal-substrate.md).

pub mod client;
pub mod daemon;
pub mod protocol;

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::buffer::BufferId;
use super::editor::Editor;
use super::pty::{SessionId, SpawnSpec};
use super::window::{Node, WindowId};

/// Bumped on incompatible changes; files with another version are ignored.
pub const SESSION_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionFile {
    pub version: u32,
    pub buffers: Vec<SavedBuffer>,
    /// The buffer that was current (shown in the selected window).
    pub current_buffer: BufferId,
    pub tabs: Vec<SavedTab>,
    pub current_tab: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedBuffer {
    pub id: BufferId,
    pub name: String,
    /// Point of the buffer's primary cursor when it was saved.
    pub point: usize,
    #[serde(flatten)]
    pub kind: SavedBufferKind,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SavedBufferKind {
    /// Visiting a file: reopened from disk (unsaved changes are not kept).
    File { path: PathBuf },
    /// A buffer with no file (e.g. *scratch*): its text is saved.
    Text { text: String },
    /// A terminal buffer: reattached to the daemon session (or, if that is
    /// gone, kept as a dead terminal buffer).
    Terminal { session: SessionId, spec: SpawnSpec },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedTab {
    pub name: Option<String>,
    pub root: Node,
    pub selected: WindowId,
    pub windows: Vec<SavedWindow>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedWindow {
    pub id: WindowId,
    pub buffer: BufferId,
    pub point: usize,
    pub scroll_line: usize,
}

/// Where the daemon listens: `$EMAX_PTYD_SOCKET`, else
/// `$TMPDIR/emax-$UID/ptyd.sock` (the configuration can override it too:
/// `terminal.socket`).
pub fn default_socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("EMAX_PTYD_SOCKET").filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    std::env::temp_dir().join(format!("emax-{uid}")).join("ptyd.sock")
}

/// Where session state is kept: `$EMAX_STATE_DIR`, else `configured`, else
/// `$XDG_STATE_HOME/emax`, else `~/.local/state/emax`.
pub fn state_dir(configured: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("EMAX_STATE_DIR").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    if let Some(p) = configured {
        return Some(p.to_path_buf());
    }
    std::env::var_os("XDG_STATE_HOME")
        .filter(|p| !p.is_empty())
        .map(|p| PathBuf::from(p).join("emax"))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("state").join("emax")))
}

/// The session file in `dir`.
pub fn session_file_path(dir: &Path) -> PathBuf {
    dir.join("session.json")
}

/// Write `s` to `path` atomically (a temporary file renamed over it),
/// creating the directory if needed.
pub fn save(path: &Path, s: &SessionFile) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_vec_pretty(s).map_err(io::Error::other)?;
    let tmp = path.with_extension(format!("json.tmp{}", std::process::id()));
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)
}

/// Read a session file. A file of another version is `InvalidData`.
pub fn load(path: &Path) -> io::Result<SessionFile> {
    let text = std::fs::read(path)?;
    let s: SessionFile = serde_json::from_slice(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if s.version != SESSION_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("session file version {} (expected {SESSION_VERSION})", s.version),
        ));
    }
    Ok(s)
}

impl Editor {
    /// The editor's tabs, windows and buffers as a session file.
    /// SKELETON: the persistence builder implements this.
    pub fn session_snapshot(&self) -> SessionFile {
        SessionFile { version: SESSION_VERSION, buffers: Vec::new(), current_buffer: self.buf_id, tabs: Vec::new(), current_tab: 0 }
    }

    /// Recreate the buffers, tabs and windows of `s`, reattaching terminal
    /// buffers to their daemon sessions.
    /// SKELETON: the persistence builder implements this.
    pub fn restore_session(&mut self, _s: &SessionFile) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "session restore not implemented yet"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::pty::PtySize;
    use crate::engine::testutil::temp_dir;

    fn sample() -> SessionFile {
        SessionFile {
            version: SESSION_VERSION,
            buffers: vec![
                SavedBuffer { id: 1, name: "main.rs".into(), point: 5, kind: SavedBufferKind::File { path: "/x/main.rs".into() } },
                SavedBuffer { id: 2, name: "*scratch*".into(), point: 0, kind: SavedBufferKind::Text { text: "hi".into() } },
                SavedBuffer {
                    id: 3,
                    name: "*term*".into(),
                    point: 0,
                    kind: SavedBufferKind::Terminal {
                        session: SessionId(12),
                        spec: SpawnSpec { program: "/bin/zsh".into(), size: PtySize::new(80, 24), ..Default::default() },
                    },
                },
            ],
            current_buffer: 1,
            tabs: vec![SavedTab {
                name: Some("work".into()),
                root: Node::Leaf(WindowId(1)),
                selected: WindowId(1),
                windows: vec![SavedWindow { id: WindowId(1), buffer: 1, point: 5, scroll_line: 0 }],
            }],
            current_tab: 0,
        }
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = temp_dir("session");
        let path = session_file_path(&dir.join("nested"));
        let s = sample();
        save(&path, &s).unwrap();
        assert_eq!(load(&path).unwrap(), s);
        let json: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(json["buffers"][0]["kind"], "file");
        assert_eq!(json["buffers"][2]["kind"], "terminal");
        assert_eq!(json["buffers"][2]["session"], 12);

        let mut old = s.clone();
        old.version = 0;
        save(&path, &old).unwrap();
        assert_eq!(load(&path).unwrap_err().kind(), io::ErrorKind::InvalidData);
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(load(&path).unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert_eq!(load(&dir.join("missing.json")).unwrap_err().kind(), io::ErrorKind::NotFound);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn paths() {
        assert!(state_dir(Some(Path::new("/cfg/state"))).is_some());
        assert!(default_socket_path().ends_with("ptyd.sock"));
        assert_eq!(session_file_path(Path::new("/s")), PathBuf::from("/s/session.json"));
    }
}
