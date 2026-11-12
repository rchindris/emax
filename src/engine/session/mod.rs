//! Session persistence, tmux-style: terminal processes live in a daemon
//! ([`daemon`], reached through [`client::DaemonBackend`]) so they survive
//! the editor, and the editor's tabs, windows and buffers are saved to a
//! session file on quit and restored on start (`session.restore_session`).
//!
//! The session file ([`SessionFile`], JSON) is written atomically to
//! [`session_file_path`]. Buffer and window ids in it are the saved editor's;
//! restoring maps them to fresh ids.
//!
//! Restoring is forgiving: missing files and ended terminal sessions are
//! skipped (and reported); a corrupt file or one of another version is set
//! aside and the editor starts fresh.

pub mod client;
pub mod daemon;
pub mod protocol;
#[cfg(test)]
mod daemon_tests;

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde::{Deserialize, Serialize};

use super::buffer::{Buffer, BufferId, BufferKind};
use super::cursor::Cursor;
use super::editor::Editor;
use super::hooks::{Event, EventKind, HookFn};
use super::pty::{SessionId, SpawnSpec};
use super::window::{Layout, Node, Tab, Window, WindowId};

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

/// Whether `s` is structurally sound: at least one tab; every split has two
/// or more children with one positive ratio each; each tab lists exactly the
/// windows of its tree, its selected window among them; no window id twice.
pub fn validate(s: &SessionFile) -> Result<(), String> {
    if s.tabs.is_empty() {
        return Err("no tabs".into());
    }
    let mut seen = HashSet::new();
    for (i, t) in s.tabs.iter().enumerate() {
        check_node(&t.root).map_err(|e| format!("tab {i}: {e}"))?;
        let mut leaves = t.root.windows();
        if !leaves.contains(&t.selected) {
            return Err(format!("tab {i}: selected window {} is not in it", t.selected.0));
        }
        let mut listed: Vec<WindowId> = t.windows.iter().map(|w| w.id).collect();
        leaves.sort();
        listed.sort();
        if leaves != listed {
            return Err(format!("tab {i}: its window list does not match its window tree"));
        }
        if let Some(w) = leaves.into_iter().find(|w| !seen.insert(*w)) {
            return Err(format!("window {} appears twice", w.0));
        }
    }
    Ok(())
}

fn check_node(n: &Node) -> Result<(), String> {
    match n {
        Node::Leaf(_) => Ok(()),
        Node::Split { children, ratios, .. } => {
            if children.len() < 2 || ratios.len() != children.len() {
                return Err("a split needs two or more children and one ratio per child".into());
            }
            if ratios.iter().any(|r| !r.is_finite() || *r <= 0.0) {
                return Err("split ratios must be positive".into());
            }
            children.iter().try_for_each(check_node)
        }
    }
}

/// `n` with every split's ratios scaled to sum to 1 (a hand-edited file
/// may say 1 and 2).
fn normalized(n: &Node) -> Node {
    match n {
        Node::Leaf(w) => Node::Leaf(*w),
        Node::Split { dir, children, ratios } => {
            let sum: f32 = ratios.iter().sum();
            Node::Split {
                dir: *dir,
                children: children.iter().map(normalized).collect(),
                ratios: ratios.iter().map(|r| r / sum).collect(),
            }
        }
    }
}

/// The point of the cursor `primary` among `cursors` (else of the first one).
fn primary_point(cursors: &[Cursor], primary: u64) -> usize {
    cursors.iter().find(|c| c.id == primary).or(cursors.first()).map_or(0, |c| c.point)
}

/// The name of the `kill-emacs` handler that saves the session.
pub const SAVE_HOOK: &str = "session-save";

impl Editor {
    /// The editor's tabs, windows and buffers as a session file. Buffers are
    /// listed most recently used first. Terminal buffers are included only
    /// when the PTY backend keeps sessions alive (the daemon).
    pub fn session_snapshot(&self) -> SessionFile {
        let persistent = self.pty.as_ref().is_some_and(|p| p.persistent());
        let current = std::iter::once((self.buf_id, &self.buf, primary_point(&self.cursors, self.primary)));
        let others = self.others.iter().map(|s| (s.id, &s.buf, primary_point(&s.cursors, s.primary)));
        let buffers = current
            .chain(others)
            .filter_map(|(id, buf, point)| {
                let kind = match (self.terminals.get(&id), &buf.path) {
                    (Some(t), _) if persistent => SavedBufferKind::Terminal { session: t.session, spec: t.spec.clone() },
                    (Some(_), _) => return None,
                    (None, Some(path)) => SavedBufferKind::File { path: path.clone() },
                    (None, None) => SavedBufferKind::Text { text: buf.rope.to_string() },
                };
                Some(SavedBuffer { id, name: buf.name.clone(), point, kind })
            })
            .collect();
        let selected = self.layout.selected();
        let window = |w: &Window| SavedWindow {
            id: w.id,
            buffer: w.buffer,
            // The selected window's live cursors are the editor's.
            point: if w.id == selected { primary_point(&self.cursors, self.primary) } else { primary_point(&w.cursors, w.primary) },
            scroll_line: w.scroll_line,
        };
        let tabs = self
            .layout
            .tabs
            .iter()
            .map(|t| SavedTab {
                name: t.name.clone(),
                root: t.root.clone(),
                selected: t.selected,
                windows: t.root.windows().into_iter().filter_map(|id| self.layout.window(id)).map(window).collect(),
            })
            .collect();
        SessionFile { version: SESSION_VERSION, buffers, current_buffer: self.buf_id, tabs, current_tab: self.layout.current_tab }
    }

    /// Recreate the buffers, tabs and windows of `s`, reattaching terminal
    /// buffers to their daemon sessions. Buffers get fresh ids. Files that
    /// are gone and terminal sessions that can't be reattached are skipped,
    /// and their windows show the restored current buffer instead; the echo
    /// area lists what was skipped. A text buffer whose name is taken by a
    /// text buffer visiting no file (e.g. the new *scratch*) replaces it.
    /// Only a malformed layout is an error (`InvalidData`); nothing changes then.
    pub fn restore_session(&mut self, s: &SessionFile) -> io::Result<()> {
        validate(s).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let wants_terminals = s.buffers.iter().any(|b| matches!(b.kind, SavedBufferKind::Terminal { .. }));
        // Only start a backend (maybe a daemon) when there is something to reattach.
        let persistent = wants_terminals && self.pty_backend().is_ok_and(|b| b.persistent());
        let mut ids: HashMap<BufferId, BufferId> = HashMap::new();
        let mut skipped = Vec::new();
        // Oldest first, so the first saved buffer ends up the most recent.
        for b in s.buffers.iter().rev() {
            match self.restore_buffer(b, persistent) {
                Ok(id) => {
                    ids.insert(b.id, id);
                    self.set_buffer_point(id, b.point);
                }
                Err(why) => skipped.push(format!("{} ({why})", b.name)),
            }
        }
        skipped.reverse();
        let fallback = ids.get(&s.current_buffer).copied().unwrap_or(self.buf_id);
        self.restore_layout(s, &ids, fallback);
        self.message = Some(match skipped.as_slice() {
            [] => format!("Session restored: {} buffer{}", ids.len(), if ids.len() == 1 { "" } else { "s" }),
            _ => format!("Session restored; skipped {}", skipped.join(", ")),
        });
        Ok(())
    }

    /// Recreate one saved buffer; `Err` says why it was skipped.
    fn restore_buffer(&mut self, b: &SavedBuffer, persistent: bool) -> Result<BufferId, String> {
        match &b.kind {
            SavedBufferKind::File { path } => {
                let open = self.buffer_ids().into_iter().find(|&id| self.find_buffer(id).is_some_and(|v| v.buf.path.as_deref() == Some(path)));
                if let Some(id) = open {
                    return Ok(id);
                }
                if !path.is_file() {
                    return Err("file not found".into());
                }
                Buffer::open(path.clone()).map(|buf| self.add_buffer(buf)).map_err(|e| e.to_string())
            }
            SavedBufferKind::Text { text } => {
                let mut buf = Buffer::from_str(&b.name, text);
                match self.scratch_named(&b.name) {
                    Some(id) => {
                        buf.modes = self.modes.initial(&buf);
                        self.replace_buffer_contents(id, buf);
                        Ok(id)
                    }
                    None => Ok(self.add_buffer(buf)),
                }
            }
            SavedBufferKind::Terminal { session, spec } => {
                if !persistent {
                    return Err("terminals don't outlive the editor without the session daemon".into());
                }
                self.attach_terminal(&b.name, *session, spec.clone()).map_err(|e| format!("session {}: {e}", session.0))
            }
        }
    }

    /// Replace the tabs and windows with the saved ones (buffer ids mapped by
    /// `ids`; windows whose buffer wasn't restored show `fallback`), then show
    /// the selected window's buffer at its point.
    fn restore_layout(&mut self, s: &SessionFile, ids: &HashMap<BufferId, BufferId>, fallback: BufferId) {
        let mut layout = Layout::new(fallback);
        layout.tabs.clear();
        layout.windows.clear();
        for t in &s.tabs {
            for w in &t.windows {
                let buffer = ids.get(&w.buffer).copied().unwrap_or(fallback);
                let len = self.find_buffer(buffer).map_or(0, |v| v.buf.rope.len_chars());
                let mut win = Window::new(w.id, buffer);
                win.cursors[0].point = w.point.min(len);
                win.scroll_line = w.scroll_line;
                layout.reserve_window_id(w.id);
                layout.windows.insert(w.id, win);
            }
            layout.tabs.push(Tab { name: t.name.clone(), root: normalized(&t.root), selected: t.selected });
        }
        layout.current_tab = s.current_tab.min(layout.tabs.len() - 1);
        self.layout = layout;
        let win = self.layout.windows[&self.layout.selected()].clone();
        self.switch_to_buffer(win.buffer);
        self.sync_selected_window();
        self.cursors = win.cursors;
        self.primary = win.primary;
        self.next_cursor_id = win.next_cursor_id;
        self.emit(Event::WindowConfigurationChanged);
    }

    /// Every buffer id, the current one first.
    fn buffer_ids(&self) -> Vec<BufferId> {
        std::iter::once(self.buf_id).chain(self.others.iter().map(|s| s.id)).collect()
    }

    /// A text buffer called `name` that visits no file.
    fn scratch_named(&self, name: &str) -> Option<BufferId> {
        self.buffer_ids().into_iter().find(|&id| {
            !self.terminals.contains_key(&id)
                && self.find_buffer(id).is_some_and(|v| v.buf.name == name && v.buf.path.is_none() && v.buf.kind == BufferKind::Text)
        })
    }

    /// Swap buffer `id`'s contents for `buf` (a fresh undo history too).
    fn replace_buffer_contents(&mut self, id: BufferId, buf: Buffer) {
        if id == self.buf_id {
            self.buf = buf;
        } else if let Some(slot) = self.others.iter_mut().find(|s| s.id == id) {
            slot.buf = buf;
        }
    }

    /// Give buffer `id` a single cursor at `point` (clamped to the buffer).
    fn set_buffer_point(&mut self, id: BufferId, point: usize) {
        if id == self.buf_id {
            self.cursors = vec![Cursor::new(1, point.min(self.buf.rope.len_chars()))];
            self.primary = 1;
            self.next_cursor_id = 2;
        } else if let Some(slot) = self.others.iter_mut().find(|s| s.id == id) {
            slot.cursors = vec![Cursor::new(1, point.min(slot.buf.rope.len_chars()))];
            slot.primary = 1;
            slot.next_cursor_id = 2;
        }
    }

    /// Restore the session saved at `path`, if there is one (`Ok(false)`
    /// when there is no file). A file that can't be used (corrupt, another
    /// version, a malformed layout) is renamed to `*.json.corrupt`, so the
    /// next save doesn't silently replace it, and reported as an error.
    pub fn restore_session_file(&mut self, path: &Path) -> io::Result<bool> {
        match load(path).and_then(|s| self.restore_session(&s)) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                let aside = path.with_extension("json.corrupt");
                let _ = std::fs::rename(path, &aside);
                Err(io::Error::new(e.kind(), format!("{}: {e} (moved to {})", path.display(), aside.display())))
            }
            Err(e) => Err(e),
        }
    }

    /// Save the session to `path` when the editor quits: a `kill-emacs`
    /// handler named [`SAVE_HOOK`] (remove it by that name to stop saving).
    pub fn save_session_on_quit(&mut self, path: PathBuf) {
        let f: HookFn = Rc::new(move |ed: &mut Editor, _: &Event| {
            save(&path, &ed.session_snapshot()).map_err(|e| format!("saving the session to {}: {e}", path.display()))
        });
        self.hooks.add(EventKind::KillEmacs, SAVE_HOOK, f);
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
