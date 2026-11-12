//! find-file (C-x C-f): the picker's query is a path. Its candidates are the
//! entries of the directory typed so far, fuzzy-filtered by the text after
//! the last `/`; RET on a directory descends into it, RET on a file visits it.
//! Like Emacs, `//` restarts the path at `/` and `/~/` at the home directory.
//!
//! The same picker chooses where to save a buffer that has no file yet.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::buffer::Buffer;
use super::editor::Editor;
use super::picker::{Candidate, FilePurpose, Picker, PickerKind};

impl Editor {
    /// The directory find-file starts in: the one a listing shows, the
    /// current buffer's file's, else the working directory.
    pub fn default_directory(&self) -> PathBuf {
        if let Some(d) = self.dired_state() {
            return d.dir.clone();
        }
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        match self.buf.path.as_deref().and_then(Path::parent) {
            Some(d) if !d.as_os_str().is_empty() => cwd.join(d),
            _ => cwd,
        }
    }

    /// Open the find-file picker in the default directory.
    pub fn open_file_picker(&mut self) {
        self.open_file_picker_for("Find file", FilePurpose::Visit);
    }

    /// Open the picker that chooses a file to save the current buffer to.
    pub fn open_save_picker(&mut self) {
        self.open_file_picker_for("Save buffer to", FilePurpose::Save { overwrite: None });
    }

    fn open_file_picker_for(&mut self, prompt: &str, purpose: FilePurpose) {
        let dir = self.default_directory();
        self.open_file_picker_in(prompt, &dir, purpose);
    }

    /// Open a file picker for `purpose` with `dir` typed.
    pub(crate) fn open_file_picker_in(&mut self, prompt: &str, dir: &Path, purpose: FilePurpose) {
        let mut query = abbreviate(dir);
        if !query.ends_with('/') {
            query.push('/');
        }
        let mut p = Picker::new(prompt, Vec::new()).with_kind(PickerKind::Files { dir: String::new(), purpose });
        p.query = query;
        self.picker = Some(p);
        self.sync_file_picker();
    }

    /// After the query changed: drop a shadowed prefix (`//`, `/~/`) and list
    /// the typed directory again if it is a different one.
    pub(crate) fn sync_file_picker(&mut self) {
        let base = self.default_directory();
        let open = self.open_files();
        let current = self.buf.path.as_deref().map(|p| truename(&base.join(p)));
        let Some(p) = self.picker.as_mut() else { return };
        let PickerKind::Files { dir, .. } = &p.kind else { return };
        let dir = dir.clone();
        if let Some(q) = unshadow(&p.query) {
            p.query = q;
        }
        let typed_dir = dir_part(&p.query).to_string();
        if dir != typed_dir {
            let path = expand(&typed_dir, &base);
            p.candidates = list_dir(&path, &open, current.as_deref());
            if let PickerKind::Files { dir, .. } = &mut p.kind {
                *dir = typed_dir;
            }
        }
        p.refilter();
    }

    /// What the open file picker's path is for.
    pub(crate) fn file_purpose(&self) -> Option<&FilePurpose> {
        self.picker.as_ref()?.purpose()
    }

    pub(crate) fn file_purpose_mut(&mut self) -> Option<&mut FilePurpose> {
        match &mut self.picker.as_mut()?.kind {
            PickerKind::Files { purpose, .. } => Some(purpose),
            _ => None,
        }
    }

    /// The paths of files visited by buffers (resolved), and whether each has unsaved changes.
    fn open_files(&self) -> HashMap<PathBuf, bool> {
        let base = std::env::current_dir().unwrap_or_default();
        std::iter::once(&self.buf)
            .chain(self.others.iter().map(|s| &s.buf))
            .filter_map(|b| Some((truename(&base.join(b.path.as_ref()?)), b.modified())))
            .collect()
    }

    /// Handle a picker action for find-file; `false` for the ones that work
    /// the same in every picker (moving the selection, quitting).
    pub(crate) fn file_picker_action(&mut self, action: &str) -> bool {
        let Some(p) = self.picker.as_mut() else { return false };
        p.message = None;
        let literal = p.purpose().is_some_and(FilePurpose::literal);
        let list_dir = p.purpose() == Some(&FilePurpose::Dired) && p.filter().is_empty();
        match action {
            // Rename, copy, mkdir: the path as typed; dired: the directory typed.
            "picker-accept" if literal || list_dir => {
                let typed = p.query.clone();
                self.visit_typed(&typed);
            }
            "picker-accept" => {
                if p.selected_candidate().is_some() {
                    let i = p.selected;
                    self.file_picker_accept(i);
                } else if !p.filter().is_empty() {
                    let typed = p.query.clone();
                    self.visit_typed(&typed);
                } else {
                    p.message = Some("No match".into());
                }
            }
            // The path as typed, even if it fuzzy-matches something: creates new files.
            "picker-accept-input" => {
                let typed = p.query.clone();
                self.visit_typed(&typed);
            }
            "picker-delete-char" if p.filter().is_empty() => {
                p.query = parent_query(&p.query);
                self.sync_file_picker();
            }
            "picker-delete-char" => {
                p.query.pop();
                self.sync_file_picker();
            }
            // The last component: what is typed of it, else the directory itself.
            "picker-delete-word" => {
                let keep = p.query.len() - p.filter().len();
                if keep == p.query.len() {
                    p.query = parent_query(&p.query);
                } else {
                    p.query.truncate(keep);
                }
                self.sync_file_picker();
            }
            "picker-kill-buffer" => {}
            _ => return false,
        }
        true
    }

    /// Accept match `i`: descend into a directory, visit a file.
    pub(crate) fn file_picker_accept(&mut self, i: usize) {
        let Some(p) = self.picker.as_mut() else { return };
        let Some(label) = p.matches.get(i).map(|m| p.candidates[m.index].label.clone()) else { return };
        let PickerKind::Files { dir, .. } = &p.kind else { return };
        p.query = format!("{dir}{label}");
        if label.ends_with('/') {
            self.sync_file_picker();
        } else {
            let typed = p.query.clone();
            self.visit_typed(&typed);
        }
    }

    /// Act on the typed path as the picker's purpose says, closing it: visit
    /// the file (or save to it); for dired, list the directory; for rename,
    /// copy and mkdir, hand it to dired. Visiting or saving enters a
    /// directory instead.
    fn visit_typed(&mut self, typed: &str) {
        let path = expand(typed, &self.default_directory());
        match self.file_purpose() {
            Some(FilePurpose::Transfer { .. }) => return self.dired_transfer_to(super::dired::normalize(&path)),
            Some(FilePurpose::Mkdir) => return self.dired_create_directory_at(super::dired::normalize(&path)),
            Some(FilePurpose::Dired) if path.is_dir() => {
                self.picker = None;
                if let Err(e) = self.dired(&path) {
                    self.message = Some(e);
                }
                return;
            }
            Some(FilePurpose::Dired) if !path.exists() => {
                if let Some(p) = self.picker.as_mut() {
                    p.message = Some(format!("No such directory: {}", abbreviate(&path)));
                }
                return;
            }
            _ => {}
        }
        if path.is_dir() {
            let Some(p) = self.picker.as_mut() else { return };
            if typed.ends_with('/') && !p.saving() {
                // As in Emacs, visiting a directory lists it.
                self.picker = None;
                if let Err(e) = self.dired(&path) {
                    self.message = Some(e);
                }
            } else if typed.ends_with('/') {
                p.message = Some(format!("{} is a directory", abbreviate(&path)));
            } else {
                p.query.push('/');
                self.sync_file_picker();
            }
            return;
        }
        if self.picker.as_ref().is_some_and(Picker::saving) {
            return self.save_typed(path);
        }
        match self.find_file(&path) {
            Ok(()) => self.picker = None,
            Err(e) => {
                if let Some(p) = self.picker.as_mut() {
                    p.message = Some(e);
                }
            }
        }
    }

    /// Save the current buffer to `path` and make it visit that file, renamed
    /// after it and in the major mode the name selects (Emacs'
    /// `set-visited-file-name`). An existing file is overwritten only when
    /// accepted twice in a row.
    fn save_typed(&mut self, path: PathBuf) {
        let name = abbreviate(&path);
        let refuse = |ed: &mut Self, msg: String| {
            if let Some(p) = ed.picker.as_mut() {
                p.message = Some(msg);
            }
        };
        if !path.parent().is_some_and(Path::is_dir) {
            let dir = path.parent().map_or_else(String::new, abbreviate);
            return refuse(self, format!("Directory {dir} does not exist"));
        }
        let real = truename(&path);
        let cwd = std::env::current_dir().unwrap_or_default();
        let visitor = self.others.iter().find(|s| s.buf.path.as_ref().is_some_and(|p| truename(&cwd.join(p)) == real));
        if let Some(s) = visitor {
            let msg = format!("{name} is already visited by buffer {}", s.buf.name);
            return refuse(self, msg);
        }
        if path.exists() {
            let Some(PickerKind::Files { purpose: FilePurpose::Save { overwrite }, .. }) =
                self.picker.as_mut().map(|p| &mut p.kind)
            else {
                return;
            };
            if overwrite.as_ref() != Some(&path) {
                *overwrite = Some(path);
                return refuse(self, format!("{name} exists; accept again to overwrite it"));
            }
        }
        let previous = self.buf.path.replace(path.clone());
        match self.buf.save() {
            Ok(msg) => {
                self.picker = None;
                let file_name = path.file_name().map_or_else(|| name.clone(), |n| n.to_string_lossy().into_owned());
                if self.buf.name != file_name {
                    self.buf.name = self.unique_name(&file_name);
                }
                if let Some(mode) = self.modes.detect(&self.buf)
                    && mode != self.major_mode()
                {
                    self.set_major_mode(mode);
                }
                self.message = Some(msg);
            }
            Err(e) => {
                self.buf.path = previous;
                refuse(self, format!("Cannot write {name}: {e}"));
            }
        }
    }

    /// Show the buffer visiting `path`, opening the file (or starting a new
    /// one) if no buffer visits it yet.
    pub fn find_file(&mut self, path: &Path) -> Result<(), String> {
        let cwd = std::env::current_dir().unwrap_or_default();
        let path = cwd.join(path);
        let name = abbreviate(&path);
        if path.is_dir() {
            return Err(format!("{name} is a directory"));
        }
        let real = truename(&path);
        let visiting = |b: &Buffer| b.path.as_ref().is_some_and(|p| truename(&cwd.join(p)) == real);
        if visiting(&self.buf) {
            return Ok(());
        }
        if let Some(id) = self.others.iter().find(|s| visiting(&s.buf)).map(|s| s.id) {
            self.switch_to_buffer(id);
            return Ok(());
        }
        let exists = path.exists();
        let buf = Buffer::open(path.clone()).map_err(|e| format!("Cannot open {name}: {e}"))?;
        let id = self.add_buffer(buf);
        self.switch_to_buffer(id);
        if !exists {
            let dir_missing = path.parent().is_some_and(|d| !d.is_dir());
            self.message =
                Some(if dir_missing { "(New file; its directory does not exist yet)" } else { "(New file)" }.into());
        }
        Ok(())
    }
}

/// The resolved path (symlinks, `..`), or the path itself if it does not exist.
pub(crate) fn truename(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from)
}

/// `path` with the home directory written as `~`.
pub fn abbreviate(path: &Path) -> String {
    if let Some(h) = home()
        && let Ok(rest) = path.strip_prefix(&h)
    {
        return if rest.as_os_str().is_empty() { "~/".into() } else { format!("~/{}", rest.display()) };
    }
    path.display().to_string()
}

/// A typed path made absolute: `~` is the home directory, relative paths are
/// relative to `base`.
fn expand(typed: &str, base: &Path) -> PathBuf {
    let home_rel = if typed == "~" { Some("") } else { typed.strip_prefix("~/") };
    match (home_rel, home()) {
        (Some(rest), Some(h)) => h.join(rest),
        _ => base.join(typed),
    }
}

/// The query restarted where Emacs would shadow what comes before: at the
/// last `//` (the root) or `/~/` (home), whichever comes later.
fn unshadow(q: &str) -> Option<String> {
    let root = q.rfind("//").map(|i| i + 1);
    let home = q.rfind("/~/").map(|i| i + 1);
    root.max(home).map(|i| q[i..].to_string())
}

/// The directory part of a typed path, up to and including the last `/`.
fn dir_part(q: &str) -> &str {
    q.rfind('/').map_or("", |i| &q[..=i])
}

/// The query for the parent of the directory typed (which ends with `/`).
fn parent_query(q: &str) -> String {
    let q = if q == "~/" { home().map_or(q.to_string(), |h| format!("{}/", h.display())) } else { q.to_string() };
    let trimmed = q.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(i) => q[..=i].to_string(),
        None if q.starts_with('/') => "/".into(),
        None => String::new(),
    }
}

/// The entries of `dir`: visible before hidden, directories (labelled with
/// a trailing `/`) before files, then by name.
fn list_dir(dir: &Path, open: &HashMap<PathBuf, bool>, current: Option<&Path>) -> Vec<Candidate> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let real_dir = truename(dir);
    let mut entries: Vec<(bool, bool, String, Option<u64>)> = rd
        .filter_map(Result::ok)
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            // Follow symlinks, so a link to a directory can be entered.
            let meta = std::fs::metadata(e.path()).ok();
            let is_dir = meta.as_ref().is_some_and(|m| m.is_dir());
            let size = meta.filter(|m| m.is_file()).map(|m| m.len());
            (name.starts_with('.'), !is_dir, name, size)
        })
        .collect();
    entries.sort_by_cached_key(|e| (e.0, e.1, e.2.to_lowercase()));
    entries
        .into_iter()
        .enumerate()
        .map(|(i, (_, is_file, name, size))| {
            let path = real_dir.join(&name);
            Candidate {
                id: i as u64,
                label: if is_file { name } else { format!("{name}/") },
                detail: String::new(),
                annotation: size.map(human_size).unwrap_or_default(),
                modified: open.get(&path).copied().unwrap_or(false),
                current: current == Some(path.as_path()),
            }
        })
        .collect()
}

pub(crate) fn human_size(n: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64 / 1024.0;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if v < 10.0 { format!("{v:.1} {}", UNITS[u]) } else { format!("{v:.0} {}", UNITS[u]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_helpers() {
        assert_eq!(unshadow("/a/b//etc/"), Some("/etc/".into()));
        assert_eq!(unshadow("/a//b/~/x"), Some("~/x".into()));
        assert_eq!(unshadow("/a/b/"), None);
        assert_eq!(dir_part("/a/b/ma"), "/a/b/");
        assert_eq!(dir_part("ma"), "");
        assert_eq!(parent_query("/a/b/"), "/a/");
        assert_eq!(parent_query("/a/"), "/");
        assert_eq!(parent_query("/"), "/");
        assert_eq!(expand("x/y", Path::new("/base")), PathBuf::from("/base/x/y"));
        assert_eq!(expand("/x", Path::new("/base")), PathBuf::from("/x"));
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(20 * 1024 * 1024), "20 MB");
    }
}
