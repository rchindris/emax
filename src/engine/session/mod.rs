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
#[cfg(test)]
mod daemon_tests;
pub mod protocol;

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
    /// A directory listing (dired): read again.
    Directory { path: PathBuf },
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

/// Looks up an environment variable (tests pass their own environment).
type Env<'a> = &'a dyn Fn(&str) -> Option<std::ffi::OsString>;

fn process_env(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(name)
}

/// A non-empty environment variable as a path.
fn env_path(env: Env, name: &str) -> Option<PathBuf> {
    env(name).filter(|p| !p.is_empty()).map(PathBuf::from)
}

/// Where the daemon listens when nothing is configured:
/// `$EMAX_PTYD_SOCKET`, else `$TMPDIR/emax-$UID/ptyd.sock`.
pub fn default_socket_path() -> PathBuf {
    socket_path(None)
}

/// Where the daemon listens: `$EMAX_PTYD_SOCKET`, else `configured`
/// (`terminal.socket`), else `$TMPDIR/emax-$UID/ptyd.sock` (a directory
/// private to this user). The environment wins, as for [`state_dir`].
pub fn socket_path(configured: Option<&Path>) -> PathBuf {
    socket_path_in(&process_env, configured, &std::env::temp_dir())
}

fn socket_path_in(env: Env, configured: Option<&Path>, tmp: &Path) -> PathBuf {
    if let Some(p) = env_path(env, "EMAX_PTYD_SOCKET") {
        return p;
    }
    if let Some(p) = configured {
        return p.to_path_buf();
    }
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    tmp.join(format!("emax-{uid}")).join("ptyd.sock")
}

/// Where session state is kept: `$EMAX_STATE_DIR`, else `configured`
/// (`session.state_dir`), else `$XDG_STATE_HOME/emax`, else
/// `~/.local/state/emax`.
pub fn state_dir(configured: Option<&Path>) -> Option<PathBuf> {
    state_dir_in(&process_env, configured)
}

fn state_dir_in(env: Env, configured: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = env_path(env, "EMAX_STATE_DIR") {
        return Some(p);
    }
    if let Some(p) = configured {
        return Some(p.to_path_buf());
    }
    env_path(env, "XDG_STATE_HOME")
        .map(|p| p.join("emax"))
        .or_else(|| env_path(env, "HOME").map(|h| h.join(".local").join("state").join("emax")))
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
                if let Some(d) = self.direds.get(&id) {
                    return Some(SavedBuffer {
                        id,
                        name: buf.name.clone(),
                        point,
                        kind: SavedBufferKind::Directory { path: d.dir.clone() },
                    });
                }
                let kind = match (self.terminals.get(&id), &buf.path) {
                    (Some(t), _) if persistent => {
                        SavedBufferKind::Terminal { session: t.session, spec: t.spec.clone() }
                    }
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
            point: if w.id == selected {
                primary_point(&self.cursors, self.primary)
            } else {
                primary_point(&w.cursors, w.primary)
            },
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
        SessionFile {
            version: SESSION_VERSION,
            buffers,
            current_buffer: self.buf_id,
            tabs,
            current_tab: self.layout.current_tab,
        }
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
                let open = self
                    .buffer_ids()
                    .into_iter()
                    .find(|&id| self.find_buffer(id).is_some_and(|v| v.buf.path.as_deref() == Some(path)));
                if let Some(id) = open {
                    return Ok(id);
                }
                if !path.is_file() {
                    return Err("file not found".into());
                }
                Buffer::open(path.clone()).map(|buf| self.add_buffer(buf)).map_err(|e| e.to_string())
            }
            SavedBufferKind::Directory { path } => {
                let open = self.direds.iter().find(|(_, d)| d.dir == *path).map(|(id, _)| *id);
                match open {
                    Some(id) => Ok(id),
                    None => self.make_dired_buffer(path),
                }
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
                && self
                    .find_buffer(id)
                    .is_some_and(|v| v.buf.name == name && v.buf.path.is_none() && v.buf.kind == BufferKind::Text)
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

    /// The startup policy for the saved session: restore it from `file` when
    /// `session.restore_session` is on and no files were named on the
    /// command line, and then save it there on quit. A quick `emax file`
    /// neither restores nor overwrites the saved workspace (its terminal
    /// processes live on in the daemon either way). The echo area keeps the
    /// startup message when there is nothing to restore, and always when
    /// the configuration had problems (they matter more than the restore
    /// summary). Returns the restore error, if any, for the caller to log.
    pub fn startup_session(
        &mut self,
        file: Option<PathBuf>,
        files_named: bool,
        config_warnings: bool,
    ) -> Option<io::Error> {
        let file = file.filter(|_| self.session_config.restore_session && !files_named)?;
        let startup = self.message.take();
        let mut error = None;
        match self.restore_session_file(&file) {
            Ok(true) => {}
            Ok(false) => self.message = startup.clone(),
            Err(e) => {
                self.message = Some(format!("Session not restored: {e}"));
                error = Some(e);
            }
        }
        if config_warnings {
            self.message = startup;
        }
        self.save_session_on_quit(file);
        error
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
                SavedBuffer {
                    id: 1,
                    name: "main.rs".into(),
                    point: 5,
                    kind: SavedBufferKind::File { path: "/x/main.rs".into() },
                },
                SavedBuffer {
                    id: 2,
                    name: "*scratch*".into(),
                    point: 0,
                    kind: SavedBufferKind::Text { text: "hi".into() },
                },
                SavedBuffer {
                    id: 3,
                    name: "*term*".into(),
                    point: 0,
                    kind: SavedBufferKind::Terminal {
                        session: SessionId(12),
                        spec: SpawnSpec {
                            program: "/bin/zsh".into(),
                            size: PtySize::new(80, 24),
                            ..Default::default()
                        },
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

    /// An environment with just `vars` (empty values count as unset).
    fn env_of(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<std::ffi::OsString> {
        let vars: Vec<(String, String)> = vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |name| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.into())
    }

    #[test]
    fn state_dir_precedence() {
        let cfg = Some(Path::new("/cfg/state"));
        let all = env_of(&[("EMAX_STATE_DIR", "/env/state"), ("XDG_STATE_HOME", "/xdg"), ("HOME", "/home/u")]);
        assert_eq!(state_dir_in(&all, cfg), Some(PathBuf::from("/env/state")), "the environment wins");
        let no_override = env_of(&[("EMAX_STATE_DIR", ""), ("XDG_STATE_HOME", "/xdg"), ("HOME", "/home/u")]);
        assert_eq!(state_dir_in(&no_override, cfg), Some(PathBuf::from("/cfg/state")), "then the configuration");
        assert_eq!(state_dir_in(&no_override, None), Some(PathBuf::from("/xdg/emax")), "then XDG_STATE_HOME");
        let home = env_of(&[("XDG_STATE_HOME", ""), ("HOME", "/home/u")]);
        assert_eq!(state_dir_in(&home, None), Some(PathBuf::from("/home/u/.local/state/emax")), "then ~/.local/state");
        assert_eq!(state_dir_in(&env_of(&[]), None), None);
        assert_eq!(session_file_path(Path::new("/s")), PathBuf::from("/s/session.json"));
    }

    #[test]
    fn socket_path_precedence() {
        let cfg = Some(Path::new("/cfg/d.sock"));
        let tmp = Path::new("/tmpdir");
        let env = env_of(&[("EMAX_PTYD_SOCKET", "/env/d.sock")]);
        assert_eq!(
            socket_path_in(&env, cfg, tmp),
            PathBuf::from("/env/d.sock"),
            "the environment wins, as for state_dir"
        );
        let none = env_of(&[("EMAX_PTYD_SOCKET", "")]);
        assert_eq!(socket_path_in(&none, cfg, tmp), PathBuf::from("/cfg/d.sock"));
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        assert_eq!(
            socket_path_in(&none, None, tmp),
            PathBuf::from(format!("/tmpdir/emax-{uid}/ptyd.sock")),
            "a per-user directory"
        );
    }

    // ------------------------------------------------- snapshot and restore

    use std::cell::RefCell;

    use crate::engine::cursor::Cursor;
    use crate::engine::editor::Args;
    use crate::engine::pty::local::LocalBackend;
    use crate::engine::pty::mock::MockBackend;
    use crate::engine::pty::noop_waker;
    use crate::engine::term::Terminal;
    use crate::engine::term::buffer::{InputMode, TermState};
    use crate::engine::window::SplitDir;

    /// The id of the buffer called `name`.
    fn id_of(ed: &Editor, name: &str) -> BufferId {
        ed.buffer_ids()
            .into_iter()
            .find(|&id| ed.find_buffer(id).unwrap().buf.name == name)
            .unwrap_or_else(|| panic!("no buffer {name}"))
    }

    fn name_of(ed: &Editor, id: BufferId) -> String {
        ed.find_buffer(id).map(|v| v.buf.name.clone()).unwrap_or_else(|| format!("#{id}"))
    }

    /// Show `buf` in a new window placed after the selected one (split `dir`),
    /// with point at `point`; returns the window id.
    fn split(ed: &mut Editor, dir: SplitDir, buf: BufferId, point: usize) -> WindowId {
        let id = ed.layout.alloc_window_id();
        let mut w = Window::new(id, buf);
        w.cursors = vec![Cursor::new(1, point)];
        ed.layout.windows.insert(id, w);
        let sel = ed.layout.selected();
        let tab = ed.layout.tab_mut();
        let old = std::mem::replace(&mut tab.root, Node::Leaf(sel));
        tab.root = Node::Split { dir, children: vec![old, Node::Leaf(id)], ratios: vec![0.6, 0.4] };
        id
    }

    /// A session file with ids replaced by buffer names, for comparing
    /// sessions across editors (ids are fresh after a restore).
    fn by_name(s: &SessionFile) -> serde_json::Value {
        let names: HashMap<BufferId, String> = s.buffers.iter().map(|b| (b.id, b.name.clone())).collect();
        let mut buffers: Vec<serde_json::Value> = s
            .buffers
            .iter()
            .map(|b| {
                let mut v = serde_json::to_value(b).unwrap();
                v.as_object_mut().unwrap().remove("id");
                v
            })
            .collect();
        buffers.sort_by_key(|v| v["name"].as_str().unwrap().to_string());
        let tabs: Vec<serde_json::Value> = s
            .tabs
            .iter()
            .map(|t| {
                let mut v = serde_json::to_value(t).unwrap();
                for (w, saved) in v["windows"].as_array_mut().unwrap().iter_mut().zip(&t.windows) {
                    w["buffer"] = names[&saved.buffer].clone().into();
                }
                v
            })
            .collect();
        serde_json::json!({ "buffers": buffers, "current": names[&s.current_buffer], "tabs": tabs, "current_tab": s.current_tab })
    }

    /// An editor with two files, a text buffer, a split and a second tab.
    fn workspace(dir: &Path) -> Editor {
        std::fs::write(dir.join("a.txt"), "alpha\nbeta\ngamma\n").unwrap();
        std::fs::write(dir.join("b.txt"), "0123456789").unwrap();
        let mut ed = Editor::new(Buffer::from_str("*scratch*", "my notes"));
        let notes = ed.add_buffer(Buffer::from_str("*notes*", "remember\n"));
        let b = ed.add_buffer(Buffer::open(dir.join("b.txt")).unwrap());
        let a = ed.add_buffer(Buffer::open(dir.join("a.txt")).unwrap());
        ed.switch_to_buffer(a);
        ed.cursors = vec![Cursor::new(1, 7)];
        ed.primary = 1;
        let w2 = split(&mut ed, SplitDir::Horizontal, b, 4);
        ed.layout.windows.get_mut(&w2).unwrap().scroll_line = 2;
        // A second tab, named, showing *notes*, then back to the first.
        let w3 = ed.layout.alloc_window_id();
        let mut w = Window::new(w3, notes);
        w.cursors = vec![Cursor::new(1, 3)];
        ed.layout.windows.insert(w3, w);
        ed.layout.tabs.push(Tab { name: Some("notes".into()), root: Node::Leaf(w3), selected: w3 });
        ed
    }

    #[test]
    fn snapshot_lists_buffers_windows_and_points() {
        let dir = temp_dir("snap");
        let ed = workspace(&dir);
        let s = ed.session_snapshot();
        assert_eq!(s.version, SESSION_VERSION);
        assert_eq!(s.buffers[0].name, "a.txt", "most recent first");
        assert_eq!(s.buffers[0].point, 7);
        assert_eq!(s.buffers[0].kind, SavedBufferKind::File { path: dir.join("a.txt") });
        let notes = s.buffers.iter().find(|b| b.name == "*notes*").unwrap();
        assert_eq!(notes.kind, SavedBufferKind::Text { text: "remember\n".into() });
        assert_eq!(s.current_buffer, id_of(&ed, "a.txt"));
        assert_eq!(s.tabs.len(), 2);
        assert_eq!(s.current_tab, 0);
        let t = &s.tabs[0];
        assert_eq!(t.windows.len(), 2);
        assert_eq!((t.windows[0].buffer, t.windows[0].point), (id_of(&ed, "a.txt"), 7), "selected: the editor's point");
        assert_eq!((t.windows[1].buffer, t.windows[1].point, t.windows[1].scroll_line), (id_of(&ed, "b.txt"), 4, 2));
        assert_eq!(s.tabs[1].name.as_deref(), Some("notes"));
        assert!(validate(&s).is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn restore_round_trips_through_the_file() {
        let dir = temp_dir("restore");
        let ed = workspace(&dir);
        let saved = ed.session_snapshot();
        let path = session_file_path(&dir.join("state"));
        save(&path, &saved).unwrap();

        // A fresh editor, as at startup.
        let mut fresh = Editor::new(Buffer::from_str("*scratch*", ";; welcome"));
        let changes = Rc::new(RefCell::new(0));
        let c = changes.clone();
        fresh.hooks.add(
            EventKind::WindowConfigurationChanged,
            "count",
            Rc::new(move |_, _| {
                *c.borrow_mut() += 1;
                Ok(())
            }),
        );
        assert!(fresh.restore_session_file(&path).unwrap());
        fresh.run_hooks();
        assert_eq!(*changes.borrow(), 1);
        assert!(fresh.message.as_deref().unwrap().starts_with("Session restored"), "{:?}", fresh.message);

        // Same buffers (the saved *scratch* replaced the new one), same layout.
        assert_eq!(fresh.buffer_count(), 4);
        assert_eq!(fresh.find_buffer(id_of(&fresh, "*scratch*")).unwrap().buf.rope.to_string(), "my notes");
        assert_eq!(fresh.buf.name, "a.txt");
        assert_eq!(fresh.cursors[0].point, 7);
        assert_eq!(by_name(&fresh.session_snapshot()), by_name(&saved));

        // The other window has its own point and scroll position.
        let other = fresh.layout.tab().root.windows()[1];
        let w = fresh.layout.window(other).unwrap();
        assert_eq!((name_of(&fresh, w.buffer), w.cursors[0].point, w.scroll_line), ("b.txt".into(), 4, 2));
        // Window ids keep counting from the restored ones.
        let next = fresh.layout.alloc_window_id();
        assert!(fresh.layout.windows.keys().all(|w| *w < next));
        // Tabs: selecting the second shows *notes* at its point.
        assert_eq!(fresh.layout.tabs[1].name.as_deref(), Some("notes"));
        assert_eq!(fresh.layout.windows.len(), 3);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn restore_skips_files_that_are_gone() {
        let dir = temp_dir("gone");
        let ed = workspace(&dir);
        let saved = ed.session_snapshot();
        std::fs::remove_file(dir.join("b.txt")).unwrap();
        let mut fresh = Editor::new(Buffer::from_str("*scratch*", ""));
        fresh.restore_session(&saved).unwrap();
        let msg = fresh.message.clone().unwrap();
        assert!(msg.contains("skipped b.txt (file not found)"), "{msg}");
        assert!(fresh.buffer_ids().iter().all(|&id| name_of(&fresh, id) != "b.txt"));
        // Its window shows the restored current buffer instead.
        let other = fresh.layout.tab().root.windows()[1];
        assert_eq!(name_of(&fresh, fresh.layout.window(other).unwrap().buffer), "a.txt");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn restore_clamps_points_and_normalizes_ratios() {
        let s = SessionFile {
            version: SESSION_VERSION,
            buffers: vec![SavedBuffer {
                id: 9,
                name: "t".into(),
                point: 1000,
                kind: SavedBufferKind::Text { text: "short".into() },
            }],
            current_buffer: 9,
            tabs: vec![SavedTab {
                name: None,
                root: Node::Split {
                    dir: SplitDir::Vertical,
                    children: vec![Node::Leaf(WindowId(4)), Node::Leaf(WindowId(7))],
                    ratios: vec![1.0, 3.0],
                },
                selected: WindowId(7),
                windows: vec![
                    SavedWindow { id: WindowId(4), buffer: 9, point: 2, scroll_line: 0 },
                    SavedWindow { id: WindowId(7), buffer: 9, point: 99, scroll_line: 0 },
                ],
            }],
            current_tab: 5,
        };
        let mut ed = Editor::new(Buffer::from_str("*scratch*", ""));
        ed.restore_session(&s).unwrap();
        assert_eq!(ed.buf.name, "t");
        assert_eq!(ed.cursors[0].point, 5, "clamped to the buffer");
        assert_eq!(ed.layout.current_tab, 0);
        assert_eq!(ed.layout.selected(), WindowId(7));
        let Node::Split { ratios, .. } = &ed.layout.tab().root else { panic!() };
        assert_eq!(ratios, &[0.25, 0.75]);
        assert_eq!(ed.layout.window(WindowId(4)).unwrap().cursors[0].point, 2);
        assert_eq!(ed.layout.alloc_window_id(), WindowId(8));
    }

    #[test]
    fn malformed_layouts_are_refused_and_change_nothing() {
        let base = SessionFile {
            version: SESSION_VERSION,
            buffers: vec![SavedBuffer {
                id: 1,
                name: "x".into(),
                point: 0,
                kind: SavedBufferKind::Text { text: "x".into() },
            }],
            current_buffer: 1,
            tabs: vec![SavedTab {
                name: None,
                root: Node::Leaf(WindowId(1)),
                selected: WindowId(1),
                windows: vec![SavedWindow { id: WindowId(1), buffer: 1, point: 0, scroll_line: 0 }],
            }],
            current_tab: 0,
        };
        assert!(validate(&base).is_ok());
        let leaf = |n| Node::Leaf(WindowId(n));
        let mut bad = Vec::new();
        let mut s = base.clone();
        s.tabs.clear();
        bad.push((s, "no tabs"));
        let mut s = base.clone();
        s.tabs[0].selected = WindowId(2);
        bad.push((s, "selected window"));
        let mut s = base.clone();
        s.tabs[0].windows.push(SavedWindow { id: WindowId(2), buffer: 1, point: 0, scroll_line: 0 });
        bad.push((s, "does not match"));
        let mut s = base.clone();
        s.tabs[0].root = Node::Split { dir: SplitDir::Vertical, children: vec![leaf(1)], ratios: vec![1.0] };
        bad.push((s, "two or more"));
        let mut s = base.clone();
        s.tabs[0].root = Node::Split { dir: SplitDir::Vertical, children: vec![leaf(1), leaf(2)], ratios: vec![1.0] };
        s.tabs[0].windows.push(SavedWindow { id: WindowId(2), buffer: 1, point: 0, scroll_line: 0 });
        bad.push((s, "one ratio per child"));
        let mut s = base.clone();
        s.tabs[0].root =
            Node::Split { dir: SplitDir::Vertical, children: vec![leaf(1), leaf(2)], ratios: vec![1.0, -1.0] };
        s.tabs[0].windows.push(SavedWindow { id: WindowId(2), buffer: 1, point: 0, scroll_line: 0 });
        bad.push((s, "positive"));
        let mut s = base.clone();
        s.tabs.push(s.tabs[0].clone());
        bad.push((s, "twice"));
        for (s, why) in bad {
            let e = validate(&s).unwrap_err();
            assert!(e.contains(why), "{e} (expected {why})");
            let mut ed = Editor::new(Buffer::from_str("*scratch*", "untouched"));
            assert_eq!(ed.restore_session(&s).unwrap_err().kind(), io::ErrorKind::InvalidData);
            assert_eq!(ed.buffer_count(), 1);
            assert_eq!(ed.rope().to_string(), "untouched");
        }
    }

    #[test]
    fn unusable_session_files_are_set_aside() {
        let dir = temp_dir("corrupt");
        let path = session_file_path(&dir);
        let mut ed = Editor::new(Buffer::from_str("*scratch*", ""));
        assert!(!ed.restore_session_file(&path).unwrap(), "no file: nothing to do");

        for content in [
            "{ not json",
            "",
            "{\"version\": 1}",
            "[1, 2, 3]",
            "{\"version\": 99, \"buffers\": [], \"current_buffer\": 1, \"tabs\": [], \"current_tab\": 0}",
        ] {
            std::fs::write(&path, content).unwrap();
            let e = ed.restore_session_file(&path).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{content}: {e}");
            assert!(e.to_string().contains("corrupt"), "{e}");
            assert!(!path.exists());
            assert_eq!(std::fs::read_to_string(path.with_extension("json.corrupt")).unwrap(), content);
        }
        // A well-formed file with a broken layout too.
        let mut s = sample();
        s.tabs[0].selected = WindowId(42);
        save(&path, &s).unwrap();
        assert_eq!(ed.restore_session_file(&path).unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert_eq!(ed.buffer_count(), 1, "nothing restored");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Add a terminal buffer for `session` (as the term code does).
    fn add_terminal(ed: &mut Editor, session: SessionId) -> BufferId {
        let mut b = Buffer::from_str("*term*", "");
        b.kind = BufferKind::Terminal;
        let id = ed.add_buffer(b);
        let spec = SpawnSpec { program: "/bin/zsh".into(), size: PtySize::new(100, 30), ..Default::default() };
        ed.terminals.insert(
            id,
            TermState { term: Terminal::new(100, 30, 100), session, spec, mode: InputMode::Char, exited: None },
        );
        id
    }

    #[test]
    fn terminal_buffers_are_saved_only_with_a_persistent_backend() {
        let mut ed = Editor::new(Buffer::from_str("*scratch*", ""));
        let (mock, _h) = MockBackend::new();
        ed.set_pty_backend(Box::new(mock));
        add_terminal(&mut ed, SessionId(12));
        let s = ed.session_snapshot();
        let t = s.buffers.iter().find(|b| b.name == "*term*").expect("saved");
        let SavedBufferKind::Terminal { session, spec } = &t.kind else { panic!("{t:?}") };
        assert_eq!((*session, spec.program.as_str(), spec.size), (SessionId(12), "/bin/zsh", PtySize::new(100, 30)));

        // Processes that end with the editor are not saved.
        let mut ed = Editor::new(Buffer::from_str("*scratch*", ""));
        ed.set_pty_backend(Box::new(LocalBackend::new(noop_waker())));
        let term = add_terminal(&mut ed, SessionId(3));
        // Even a window showing it is kept (it will show another buffer).
        split(&mut ed, SplitDir::Vertical, term, 0);
        let s = ed.session_snapshot();
        assert!(s.buffers.iter().all(|b| b.name != "*term*"));
        assert!(validate(&s).is_ok());
        let mut fresh = Editor::new(Buffer::from_str("*scratch*", ""));
        fresh.restore_session(&s).unwrap();
        assert_eq!(fresh.layout.windows.len(), 2);
        assert!(fresh.layout.windows.values().all(|w| w.buffer == fresh.buf_id));
    }

    #[test]
    fn terminal_buffers_reattach_or_are_reported() {
        let (mock, h) = MockBackend::new();
        let spec = SpawnSpec { program: "/bin/sh".into(), size: PtySize::new(80, 24), ..Default::default() };
        let live = h.add_detached(spec.clone(), b"$ earlier output");
        let saved = SessionFile {
            version: SESSION_VERSION,
            buffers: vec![
                SavedBuffer {
                    id: 1,
                    name: "*scratch*".into(),
                    point: 0,
                    kind: SavedBufferKind::Text { text: String::new() },
                },
                SavedBuffer {
                    id: 2,
                    name: "*term*".into(),
                    point: 0,
                    kind: SavedBufferKind::Terminal { session: live, spec: spec.clone() },
                },
                SavedBuffer {
                    id: 3,
                    name: "*term*<2>".into(),
                    point: 0,
                    kind: SavedBufferKind::Terminal { session: SessionId(77), spec: spec.clone() },
                },
            ],
            current_buffer: 1,
            tabs: vec![SavedTab {
                name: None,
                root: Node::Leaf(WindowId(1)),
                selected: WindowId(1),
                windows: vec![SavedWindow { id: WindowId(1), buffer: 1, point: 0, scroll_line: 0 }],
            }],
            current_tab: 0,
        };
        let mut ed = Editor::new(Buffer::from_str("*scratch*", ""));
        ed.set_pty_backend(Box::new(mock));
        ed.restore_session(&saved).unwrap();
        let msg = ed.message.clone().unwrap();
        // The ended session is always reported.
        assert!(msg.contains("*term*<2> (session 77"), "{msg}");
        // The live one is reattached, under its name, and shows its replay.
        assert!(!msg.contains(&format!("*term* (session {}", live.0)), "{msg}");
        let id = ed.buffer_ids().into_iter().find(|&id| ed.is_terminal(id)).expect("the live terminal is reattached");
        assert_eq!(ed.buffer_name(id), Some("*term*"));
        assert_eq!(ed.terminal(id).unwrap().session, live);
        assert!(h.session(live).unwrap().attached);
        assert_eq!(ed.buffer_ids().into_iter().filter(|&id| ed.is_terminal(id)).count(), 1, "the ended one is not");
        ed.poll_pty();
        assert!(ed.terminal(id).unwrap().term.text().0.contains("$ earlier output"));

        // Without a persistent backend, terminals are not reattached.
        let mut ed = Editor::new(Buffer::from_str("*scratch*", ""));
        ed.set_pty_backend(Box::new(LocalBackend::new(noop_waker())));
        ed.restore_session(&saved).unwrap();
        assert!(ed.message.clone().unwrap().contains("session daemon"));
        assert_eq!(ed.buffer_count(), 1);
    }

    /// `main` restores (and so may create the backend) before the view
    /// installs its waker; the backend must still wake the view.
    #[test]
    fn a_backend_made_before_the_waker_is_set_still_wakes_the_ui() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let mut ed = Editor::new(Buffer::from_str("*scratch*", ""));
        let (mock, h) = MockBackend::with_waker(ed.waker.clone());
        ed.set_pty_backend(Box::new(mock));
        let wakes = Arc::new(AtomicUsize::new(0));
        let w = wakes.clone();
        ed.set_waker(Arc::new(move || {
            w.fetch_add(1, Ordering::SeqCst);
        }));
        // Installing a waker wakes the view once (output may be waiting).
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        ed.tick(std::time::Instant::now());
        let id = ed.pty_backend().unwrap().spawn(SpawnSpec::default()).unwrap();
        h.output(id, b"x");
        assert_eq!(wakes.load(Ordering::SeqCst), 2);
    }

    /// `main`'s startup: restore only with `restore_session` on, no files
    /// named and a session file present; `emax file` neither restores nor
    /// overwrites the saved workspace; config problems keep the echo area.
    #[test]
    fn startup_restores_and_saves_only_without_files() {
        let dir = temp_dir("startup");
        let path = session_file_path(&dir.join("state"));
        save(&path, &workspace(&dir).session_snapshot()).unwrap();
        let saved = std::fs::read(&path).unwrap();
        let fresh = |restore: bool| {
            let mut ed = Editor::new(Buffer::from_str("*scratch*", ""));
            ed.session_config.restore_session = restore;
            ed.message = Some("startup".into());
            ed
        };
        let saves_on_quit = |ed: &Editor| ed.hooks.handler_names(EventKind::KillEmacs).contains(&SAVE_HOOK);
        let has_notes = |ed: &Editor| ed.buffer_ids().into_iter().any(|id| ed.buffer_name(id) == Some("*notes*"));

        // `emax file`: nothing restored, and quitting leaves the file alone.
        let mut ed = fresh(true);
        assert!(ed.startup_session(Some(path.clone()), true, false).is_none());
        assert!(!has_notes(&ed) && !saves_on_quit(&ed));
        assert_eq!(ed.message.as_deref(), Some("startup"));
        ed.execute("save-buffers-kill-terminal", Args::default());
        assert_eq!(std::fs::read(&path).unwrap(), saved, "the saved workspace is untouched");

        // restore_session off, or no state directory: the same.
        let mut ed = fresh(false);
        assert!(ed.startup_session(Some(path.clone()), false, false).is_none());
        assert!(!has_notes(&ed) && !saves_on_quit(&ed));
        let mut ed = fresh(true);
        assert!(ed.startup_session(None, false, false).is_none());
        assert!(!saves_on_quit(&ed));

        // Plain `emax`: restored, the summary shown, saved on quit.
        let mut ed = fresh(true);
        assert!(ed.startup_session(Some(path.clone()), false, false).is_none());
        assert!(has_notes(&ed) && saves_on_quit(&ed));
        assert_ne!(ed.message.as_deref(), Some("startup"));
        // Config problems keep their message.
        let mut ed = fresh(true);
        assert!(ed.startup_session(Some(path.clone()), false, true).is_none());
        assert!(has_notes(&ed));
        assert_eq!(ed.message.as_deref(), Some("startup"));

        // No session yet: the startup message stays; it is saved on quit.
        let new_path = session_file_path(&dir.join("other"));
        let mut ed = fresh(true);
        assert!(ed.startup_session(Some(new_path.clone()), false, false).is_none());
        assert_eq!(ed.message.as_deref(), Some("startup"));
        ed.execute("save-buffers-kill-terminal", Args::default());
        assert!(new_path.exists());

        // A corrupt file: reported (and returned for logging), set aside.
        std::fs::write(&path, "{ corrupt").unwrap();
        let mut ed = fresh(true);
        let e = ed.startup_session(Some(path.clone()), false, false).expect("an error");
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        assert!(ed.message.clone().unwrap().starts_with("Session not restored"));
        assert!(saves_on_quit(&ed));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_session_is_saved_on_quit_by_a_named_hook() {
        let dir = temp_dir("quit");
        let path = session_file_path(&dir.join("state"));
        let mut ed = workspace(&dir);
        ed.save_session_on_quit(path.clone());
        assert!(ed.hooks.handler_names(EventKind::KillEmacs).contains(&SAVE_HOOK));
        ed.execute("save-buffers-kill-terminal", Args::default());
        assert!(ed.quit_requested);
        assert_eq!(load(&path).unwrap(), ed.session_snapshot());

        // Removing the hook by name stops the saving.
        std::fs::remove_file(&path).unwrap();
        assert!(ed.hooks.remove(EventKind::KillEmacs, SAVE_HOOK));
        ed.execute("save-buffers-kill-terminal", Args::default());
        assert!(!path.exists());

        // A failed save is reported in the echo area, not fatal.
        let blocked = dir.join("blocked");
        std::fs::write(&blocked, "a file, not a directory").unwrap();
        ed.save_session_on_quit(blocked.join("session.json"));
        ed.execute("save-buffers-kill-terminal", Args::default());
        assert!(ed.message.clone().unwrap().contains("session-save"), "{:?}", ed.message);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
