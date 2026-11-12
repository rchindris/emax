//! Dired (C-x d): a buffer listing a directory, one entry per line, where
//! keys act on the entry at point or on the marked ones: visit, go up, mark,
//! flag for deletion, delete, rename, copy, create a directory.
//!
//! The listing is read-only text (in `dired-mode`, whose keymap is in the
//! configuration); [`DiredState`] in `Editor.direds` holds what each line
//! stands for. Marks live in the state and are mirrored in the first column.
//! Rename, copy and create-directory read their target with the find-file
//! picker (see `files.rs`), which then calls back into this module.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ropey::Rope;

use super::buffer::{Buffer, BufferId, BufferKind};
use super::commands::{CmdResult, Command, flags};
use super::cursor::Cursor;
use super::editor::{Args, Editor, Scroll};
use super::files::{abbreviate, human_size, truename};
use super::picker::{FilePurpose, Transfer};
use super::window::SplitDir;

/// Lines before the first entry (the directory's name).
pub const HEADER_LINES: usize = 1;
/// The column file names start in: `M PERMISSIONS    SIZE  MON DD HH:MM  NAME`.
pub const NAME_COL: usize = 37;

/// The mark on a marked entry.
pub const MARK: char = '*';
/// The mark on an entry flagged for deletion.
pub const FLAG: char = 'D';

/// How a listing is ordered (directories always come first).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sort {
    #[default]
    Name,
    /// Newest first.
    Time,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    File {
        executable: bool,
    },
    /// `to_dir`: the link leads to a directory.
    Symlink {
        to_dir: bool,
    },
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    pub kind: EntryKind,
    /// The `ls -l` style first column, e.g. `drwxr-xr-x`.
    pub perms: String,
    pub size: Option<u64>,
    pub mtime: Option<SystemTime>,
    pub link_target: Option<String>,
    /// `' '`, [`MARK`] or [`FLAG`].
    pub mark: char,
}

impl Entry {
    /// `..`, which can be visited but not marked.
    pub fn is_parent(&self) -> bool {
        self.name == ".."
    }

    pub fn is_dir(&self) -> bool {
        matches!(self.kind, EntryKind::Dir | EntryKind::Symlink { to_dir: true })
    }
}

/// What a dired buffer lists (in `Editor.direds`, by buffer id).
#[derive(Clone, Debug, PartialEq)]
pub struct DiredState {
    /// Absolute, without `.` or `..` components (symlinks are kept).
    pub dir: PathBuf,
    /// `..` first (except at the root), then directories, then the rest.
    pub entries: Vec<Entry>,
    pub sort: Sort,
    /// What a delete command asked to confirm; repeating it deletes these.
    pub pending_delete: Option<Vec<PathBuf>>,
}

/// What part of a line to draw how (the view picks the colors).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Face {
    Header,
    Directory,
    Symlink,
    Executable,
    /// Permissions, size and date.
    Details,
    Marked,
    Flagged,
}

impl DiredState {
    /// The entry shown on `line`.
    pub fn entry_at(&self, line: usize) -> Option<&Entry> {
        line.checked_sub(HEADER_LINES).and_then(|i| self.entries.get(i))
    }

    pub fn line_of(&self, name: &str) -> Option<usize> {
        self.entries.iter().position(|e| e.name == name).map(|i| i + HEADER_LINES)
    }

    pub fn path_of(&self, e: &Entry) -> PathBuf {
        if e.is_parent() { self.dir.parent().unwrap_or(&self.dir).to_path_buf() } else { self.dir.join(&e.name) }
    }

    fn marked(&self, mark: char) -> Vec<&Entry> {
        self.entries.iter().filter(|e| e.mark == mark).collect()
    }

    /// The whole buffer text.
    pub fn render(&self) -> String {
        let now = SystemTime::now();
        let mut s = format!("  {}:", abbreviate(&self.dir));
        if self.sort == Sort::Time {
            s.push_str("  (by date)");
        }
        for e in &self.entries {
            s.push('\n');
            s.push_str(&render_entry(e, now));
        }
        s.push('\n');
        s
    }

    /// The faces of `line`, as char ranges.
    pub fn faces(&self, line: usize) -> Vec<(std::ops::Range<usize>, Face)> {
        if line < HEADER_LINES {
            return vec![(2..2 + abbreviate(&self.dir).chars().count() + 1, Face::Header)];
        }
        let Some(e) = self.entry_at(line) else { return Vec::new() };
        let name_end = NAME_COL + display_name(&e.name).chars().count();
        let name_face = match (e.mark, e.kind) {
            (MARK, _) => Some(Face::Marked),
            (FLAG, _) => Some(Face::Flagged),
            (_, EntryKind::Dir) => Some(Face::Directory),
            (_, EntryKind::Symlink { .. }) => Some(Face::Symlink),
            (_, EntryKind::File { executable: true }) => Some(Face::Executable),
            _ => None,
        };
        let mut v = Vec::new();
        match e.mark {
            MARK => v.push((0..1, Face::Marked)),
            FLAG => v.push((0..1, Face::Flagged)),
            _ => {}
        }
        v.push((2..NAME_COL, Face::Details));
        if let Some(f) = name_face {
            v.push((NAME_COL..name_end, f));
        }
        v
    }
}

/// A name as shown: control characters (e.g. a newline in a name) as `?`.
fn display_name(name: &str) -> String {
    name.chars().map(|c| if c.is_control() { '?' } else { c }).collect()
}

fn render_entry(e: &Entry, now: SystemTime) -> String {
    let size = match (e.kind, e.size) {
        (EntryKind::File { .. }, Some(n)) => human_size(n),
        _ => "-".into(),
    };
    let date = e.mtime.map_or_else(|| " ".repeat(12), |t| format_time(t, now));
    let mut line = format!("{} {:<10} {:>8}  {:<12}  {}", e.mark, e.perms, size, date, display_name(&e.name));
    if let Some(t) = &e.link_target {
        line.push_str(" -> ");
        line.push_str(&display_name(t));
    }
    line
}

/// `ls -l` style: `Nov 12 10:04` within half a year of now, else `Nov 12  2024`.
fn format_time(t: SystemTime, now: SystemTime) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let secs = |t: SystemTime| match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    };
    let (s, n) = (secs(t), secs(now));
    let tt = s as libc::time_t;
    // SAFETY: an all-zero `tm` is a valid value, and both pointers are valid for the call.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&tt, &mut tm) }.is_null() {
        return format!("{:<12}", "?");
    }
    let month = MONTHS[tm.tm_mon.clamp(0, 11) as usize];
    const HALF_YEAR: i64 = 182 * 24 * 3600;
    if (n - s).abs() < HALF_YEAR {
        format!("{month} {:>2} {:02}:{:02}", tm.tm_mday, tm.tm_hour, tm.tm_min)
    } else {
        format!("{month} {:>2}  {:>4}", tm.tm_mday, tm.tm_year + 1900)
    }
}

fn perms_string(m: &fs::Metadata) -> String {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    let ft = m.file_type();
    let kind = if ft.is_dir() {
        'd'
    } else if ft.is_symlink() {
        'l'
    } else if ft.is_fifo() {
        'p'
    } else if ft.is_socket() {
        's'
    } else if ft.is_char_device() {
        'c'
    } else if ft.is_block_device() {
        'b'
    } else {
        '-'
    };
    let mode = m.permissions().mode();
    let mut s = String::with_capacity(10);
    s.push(kind);
    for (i, c) in ['r', 'w', 'x', 'r', 'w', 'x', 'r', 'w', 'x'].into_iter().enumerate() {
        s.push(if mode & (0o400 >> i) != 0 { c } else { '-' });
    }
    s
}

fn read_entry(path: &Path, name: String) -> Option<Entry> {
    use std::os::unix::fs::PermissionsExt;
    let m = fs::symlink_metadata(path).ok()?;
    let ft = m.file_type();
    let (kind, link_target) = if ft.is_symlink() {
        let to_dir = fs::metadata(path).is_ok_and(|t| t.is_dir());
        let target = fs::read_link(path).ok().map(|t| t.to_string_lossy().into_owned());
        (EntryKind::Symlink { to_dir }, target)
    } else if ft.is_dir() {
        (EntryKind::Dir, None)
    } else if ft.is_file() {
        (EntryKind::File { executable: m.permissions().mode() & 0o111 != 0 }, None)
    } else {
        (EntryKind::Other, None)
    };
    Some(Entry {
        name,
        kind,
        perms: perms_string(&m),
        size: ft.is_file().then_some(m.len()),
        mtime: m.modified().ok(),
        link_target,
        mark: ' ',
    })
}

/// The entries of `dir`, ordered by `sort`.
pub fn read_entries(dir: &Path, sort: Sort) -> io::Result<Vec<Entry>> {
    let mut entries: Vec<Entry> = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter_map(|e| read_entry(&e.path(), e.file_name().to_string_lossy().into_owned()))
        .collect();
    match sort {
        Sort::Name => entries.sort_by_cached_key(|e| (!e.is_dir(), e.name.to_lowercase(), e.name.clone())),
        Sort::Time => entries.sort_by(|a, b| (!a.is_dir()).cmp(&!b.is_dir()).then(b.mtime.cmp(&a.mtime))),
    }
    if let Some(parent) = dir.parent()
        && let Some(mut up) = read_entry(parent, "..".into())
    {
        up.kind = EntryKind::Dir;
        entries.insert(0, up);
    }
    Ok(entries)
}

/// `p` made absolute (against the working directory) with `.` and `..`
/// removed textually, the way Emacs' `expand-file-name` does.
pub fn normalize(p: &Path) -> PathBuf {
    let abs = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")).join(p);
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

/// The buffer name for a listing of `dir`: its last component and a `/`.
fn buffer_name(dir: &Path) -> String {
    match dir.file_name() {
        Some(n) => format!("{}/", n.to_string_lossy()),
        None => "/".into(),
    }
}

/// Remove `path`: a directory with everything in it, a symlink itself.
fn remove_path(path: &Path) -> io::Result<()> {
    let m = fs::symlink_metadata(path)?;
    if m.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) }
}

/// Copy `src` to `dst` (which must not exist): directories recursively,
/// symlinks as symlinks.
fn copy_tree(src: &Path, dst: &Path) -> io::Result<()> {
    let m = fs::symlink_metadata(src)?;
    if m.file_type().is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(src)?, dst)
    } else if m.is_dir() {
        fs::create_dir(dst)?;
        for e in fs::read_dir(src)? {
            let e = e?;
            copy_tree(&e.path(), &dst.join(e.file_name()))?;
        }
        fs::set_permissions(dst, m.permissions())
    } else {
        fs::copy(src, dst).map(|_| ())
    }
}

/// Move `src` to `dst`, copying and deleting when they are on different file systems.
fn move_path(src: &Path, dst: &Path) -> io::Result<()> {
    match fs::rename(src, dst) {
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
            copy_tree(src, dst)?;
            remove_path(src)
        }
        r => r,
    }
}

fn plural(n: usize, what: &str) -> String {
    format!("{n} {what}{}", if n == 1 { "" } else { "s" })
}

// ================================================================ editor

impl Editor {
    /// The dired state of the current buffer, if it is a listing.
    pub fn dired_state(&self) -> Option<&DiredState> {
        self.direds.get(&self.buf_id)
    }

    /// The faces of `line` of buffer `buf` (only listings have any yet).
    pub fn line_faces(&self, buf: BufferId, line: usize) -> Vec<(std::ops::Range<usize>, Face)> {
        self.direds.get(&buf).map(|d| d.faces(line)).unwrap_or_default()
    }

    /// Show a listing of `dir`, reusing (and re-reading) a buffer that
    /// already lists it. Point goes to the first entry after `..`.
    pub fn dired(&mut self, dir: &Path) -> Result<(), String> {
        let dir = normalize(dir);
        let real = truename(&dir);
        let open = self.direds.iter().find(|(_, d)| truename(&d.dir) == real).map(|(id, _)| *id);
        match open {
            Some(id) => {
                self.switch_to_buffer(id);
                self.dired_revert_buffer(id)?;
            }
            None => {
                let id = self.make_dired_buffer(&dir)?;
                self.switch_to_buffer(id);
                let skip_parent = self.dired_state().is_some_and(|d| d.entries.len() > 1 && d.entries[0].is_parent());
                let first = usize::from(skip_parent);
                self.dired_goto_line(HEADER_LINES + first);
            }
        }
        self.scroll = Scroll::EnsureVisible;
        Ok(())
    }

    /// A new buffer listing `dir` (behind the current one). Returns its id.
    pub(crate) fn make_dired_buffer(&mut self, dir: &Path) -> Result<BufferId, String> {
        if !dir.is_dir() {
            return Err(format!("{} is not a directory", abbreviate(dir)));
        }
        let entries = read_entries(dir, Sort::Name).map_err(|e| format!("Cannot read {}: {e}", abbreviate(dir)))?;
        let state = DiredState { dir: dir.to_path_buf(), entries, sort: Sort::Name, pending_delete: None };
        let mut buf = Buffer::from_str(&buffer_name(dir), &state.render());
        buf.kind = BufferKind::Dired;
        // The state goes in first: `add_buffer` emits BufferCreated, and the
        // id it will get is the next one.
        let id = self.next_buf_id;
        self.direds.insert(id, state);
        let got = self.add_buffer(buf);
        debug_assert_eq!(id, got);
        Ok(id)
    }

    /// Put the (single) cursor on the file name of `line`.
    fn dired_goto_line(&mut self, line: usize) {
        let Some(d) = self.dired_state() else { return };
        let last = HEADER_LINES + d.entries.len().saturating_sub(1);
        let line = line.clamp(HEADER_LINES.min(last), last);
        let p = name_point(&self.buf.rope, line);
        self.keep_only_primary();
        let c = &mut self.cursors[0];
        c.point = p;
        c.deactivate();
    }

    fn dired_line(&self) -> usize {
        self.buf.rope.char_to_line(self.primary_cursor().point.min(self.buf.rope.len_chars()))
    }

    /// The entry at point (`None` on the header).
    fn dired_entry_at_point(&self) -> Option<&Entry> {
        self.dired_state()?.entry_at(self.dired_line())
    }

    /// Re-read buffer `id`'s directory, keeping marks and each window's point
    /// on the entry it was on.
    pub fn dired_revert_buffer(&mut self, id: BufferId) -> Result<(), String> {
        let Some(old) = self.direds.get(&id).cloned() else { return Ok(()) };
        let mut entries =
            read_entries(&old.dir, old.sort).map_err(|e| format!("Cannot read {}: {e}", abbreviate(&old.dir)))?;
        for e in &mut entries {
            if let Some(o) = old.entries.iter().find(|o| o.name == e.name) {
                e.mark = o.mark;
            }
        }
        let new = DiredState { entries, pending_delete: None, ..old.clone() };
        let rope = Rope::from_str(&new.render());
        let follow = |r: &Rope, c: &mut Cursor| {
            let line = r.char_to_line(c.point.min(r.len_chars()));
            let target = old.entry_at(line).and_then(|e| new.line_of(&e.name)).unwrap_or(line);
            let target = target.min(HEADER_LINES + new.entries.len().saturating_sub(1));
            *c = Cursor::new(c.id, name_point(&rope, target));
        };
        if id == self.buf_id {
            let old_rope = std::mem::replace(&mut self.buf.rope, rope.clone());
            for c in &mut self.cursors {
                follow(&old_rope, c);
            }
            self.normalize_cursors();
            self.keep_only_primary();
            let selected = self.layout.selected();
            for w in self.layout.windows.values_mut().filter(|w| w.buffer == id && w.id != selected) {
                w.cursors.iter_mut().for_each(|c| follow(&old_rope, c));
            }
        } else if let Some(slot) = self.others.iter_mut().find(|s| s.id == id) {
            let old_rope = std::mem::replace(&mut slot.buf.rope, rope.clone());
            for c in &mut slot.cursors {
                follow(&old_rope, c);
            }
            for w in self.layout.windows.values_mut().filter(|w| w.buffer == id) {
                w.cursors.iter_mut().for_each(|c| follow(&old_rope, c));
            }
        }
        self.direds.insert(id, new);
        Ok(())
    }

    /// Re-read every listing (after files changed).
    fn dired_revert_all(&mut self) {
        let mut ids: Vec<BufferId> = self.direds.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            if self.dired_revert_buffer(id).is_err() {
                // The directory is gone: the listing goes too, like Emacs.
                self.kill_buffer(id);
            }
        }
    }

    /// Set the mark of the entry on `line` (the buffer must be current).
    fn dired_set_mark(&mut self, line: usize, mark: char) -> bool {
        let Some(d) = self.direds.get_mut(&self.buf_id) else { return false };
        let Some(e) = line.checked_sub(HEADER_LINES).and_then(|i| d.entries.get_mut(i)) else { return false };
        if e.is_parent() {
            return false;
        }
        e.mark = mark;
        let start = self.buf.rope.line_to_char(line);
        // Not `Editor::replace`: no cursor moves, and nothing to undo.
        self.buf.rope.remove(start..start + 1);
        self.buf.rope.insert_char(start, mark);
        true
    }

    /// Mark `n` entries from point (backwards if `n` < 0), moving past them.
    fn dired_mark_lines(&mut self, n: i64, mark: char) -> CmdResult {
        if self.dired_state().is_none() {
            return Err("Not in a Dired buffer".into());
        }
        let mut line = self.dired_line();
        if n < 0 {
            for _ in 0..n.unsigned_abs() {
                if line <= HEADER_LINES {
                    break;
                }
                line -= 1;
                self.dired_set_mark(line, mark);
            }
        } else {
            for _ in 0..n {
                if self.dired_state().and_then(|d| d.entry_at(line)).is_none() {
                    break;
                }
                if !self.dired_set_mark(line, mark) && mark != ' ' {
                    self.message = Some("Can't mark ..".into());
                }
                line += 1;
            }
        }
        self.dired_goto_line(line);
        Ok(())
    }

    /// The paths an operation acts on: the marked entries, else the one at point.
    fn dired_targets(&self) -> Result<Vec<PathBuf>, String> {
        let d = self.dired_state().ok_or("Not in a Dired buffer")?;
        let marked = d.marked(MARK);
        if !marked.is_empty() {
            return Ok(marked.iter().map(|e| d.path_of(e)).collect());
        }
        match self.dired_entry_at_point() {
            Some(e) if !e.is_parent() => Ok(vec![d.path_of(e)]),
            Some(_) => Err("Can't operate on ..".into()),
            None => Err("No file on this line".into()),
        }
    }

    /// Delete `paths` once the command was repeated (the confirmation), then
    /// re-read the listings.
    fn dired_delete(&mut self, paths: Vec<PathBuf>, what: &str) -> CmdResult {
        if paths.is_empty() {
            return Err(format!("No {what}"));
        }
        let confirmed = self.last_command == self.this_command
            && self.dired_state().and_then(|d| d.pending_delete.as_ref()) == Some(&paths);
        if let Some(d) = self.direds.get_mut(&self.buf_id) {
            d.pending_delete = (!confirmed).then(|| paths.clone());
        }
        if !confirmed {
            let names: Vec<String> = paths
                .iter()
                .take(3)
                .map(|p| p.file_name().unwrap_or_default().to_string_lossy().into_owned())
                .collect();
            let more = if paths.len() > 3 { format!(" and {} more", paths.len() - 3) } else { String::new() };
            let dirs = paths.iter().any(|p| fs::symlink_metadata(p).is_ok_and(|m| m.is_dir()));
            return Err(format!(
                "Delete {} ({}{more}){}? Repeat the command to confirm",
                plural(paths.len(), "file"),
                names.join(", "),
                if dirs { ", directories recursively" } else { "" },
            ));
        }
        let mut failed = Vec::new();
        let mut done = 0;
        for p in &paths {
            match remove_path(p) {
                Ok(()) => done += 1,
                Err(e) => failed.push(format!("{} ({e})", p.file_name().unwrap_or_default().to_string_lossy())),
            }
        }
        self.dired_revert_all();
        self.message = Some(if failed.is_empty() {
            format!("Deleted {}", plural(done, "file"))
        } else {
            format!("Deleted {}; failed: {}", plural(done, "file"), failed.join(", "))
        });
        Ok(())
    }

    /// The directory a rename or copy proposes: another window's listing in
    /// this tab (Emacs' `dired-dwim-target`), else this one.
    fn dired_default_target(&self) -> PathBuf {
        let selected = self.layout.selected();
        let other = self
            .layout
            .tab()
            .root
            .windows()
            .into_iter()
            .filter(|w| *w != selected)
            .filter_map(|w| self.layout.window(w))
            .find_map(|w| self.direds.get(&w.buffer).filter(|_| w.buffer != self.buf_id));
        other.or(self.dired_state()).map_or_else(|| self.default_directory(), |d| d.dir.clone())
    }

    fn dired_ask_target(&mut self, op: Transfer) -> CmdResult {
        let sources = self.dired_targets()?;
        let what = match sources.as_slice() {
            [one] => one.file_name().unwrap_or_default().to_string_lossy().into_owned(),
            many => plural(many.len(), "file"),
        };
        let prompt = format!("{} {what} to", op.title());
        let dir = self.dired_default_target();
        self.open_file_picker_in(&prompt, &dir, FilePurpose::Transfer { op, sources, overwrite: None });
        Ok(())
    }

    /// Rename or copy the sources of the open picker to `target` (typed in
    /// it): into it if it is a directory, else to it (one source only). An
    /// existing file is overwritten only when accepted twice in a row.
    pub(crate) fn dired_transfer_to(&mut self, target: PathBuf) {
        let Some(FilePurpose::Transfer { op, sources, overwrite }) = self.file_purpose().cloned() else { return };
        let refuse = |ed: &mut Self, msg: String| {
            if let Some(p) = ed.picker.as_mut() {
                p.message = Some(msg);
            }
        };
        let into_dir = target.is_dir();
        if !into_dir {
            if sources.len() > 1 {
                return refuse(self, format!("{} is not a directory", abbreviate(&target)));
            }
            if !target.parent().is_some_and(Path::is_dir) {
                let dir = target.parent().map_or_else(String::new, abbreviate);
                return refuse(self, format!("Directory {dir} does not exist"));
            }
            if fs::symlink_metadata(&target).is_ok() && overwrite.as_ref() != Some(&target) {
                if let Some(FilePurpose::Transfer { overwrite, .. }) = self.file_purpose_mut() {
                    *overwrite = Some(target.clone());
                }
                return refuse(self, format!("{} exists; accept again to overwrite it", abbreviate(&target)));
            }
        }
        self.picker = None;
        let mut done = 0;
        let mut failed = Vec::new();
        let mut last_dest = None;
        for src in &sources {
            let name = src.file_name().unwrap_or_default();
            let dest = if into_dir { target.join(name) } else { target.clone() };
            let shown = name.to_string_lossy().into_owned();
            let result = if truename(&dest) == truename(src) {
                Err("same file".to_string())
            } else if truename(&dest).starts_with(truename(src)) && src.is_dir() {
                Err("can't put a directory inside itself".to_string())
            } else if into_dir && fs::symlink_metadata(&dest).is_ok() {
                Err(format!("{} exists", abbreviate(&dest)))
            } else {
                let r = match op {
                    Transfer::Rename => move_path(src, &dest),
                    Transfer::Copy => {
                        if !into_dir && fs::symlink_metadata(&dest).is_ok() {
                            let _ = remove_path(&dest);
                        }
                        copy_tree(src, &dest)
                    }
                };
                r.map_err(|e| e.to_string())
            };
            match result {
                Ok(()) => {
                    done += 1;
                    if op == Transfer::Rename {
                        self.follow_rename(src, &dest);
                    }
                    last_dest = Some(dest);
                }
                Err(e) => failed.push(format!("{shown} ({e})")),
            }
        }
        // Unmark what was acted on, then show the result.
        if let Some(d) = self.direds.get_mut(&self.buf_id) {
            for e in &mut d.entries {
                if e.mark == MARK {
                    e.mark = ' ';
                }
            }
        }
        self.dired_revert_all();
        if let (Some(dest), Some(d)) = (last_dest, self.dired_state())
            && dest.parent() == Some(d.dir.as_path())
            && let Some(line) = d.line_of(&dest.file_name().unwrap_or_default().to_string_lossy())
        {
            self.dired_goto_line(line);
        }
        let verb = match op {
            Transfer::Rename => "Moved",
            Transfer::Copy => "Copied",
        };
        self.message = Some(if failed.is_empty() {
            format!("{verb} {}", plural(done, "file"))
        } else {
            format!("{verb} {}; failed: {}", plural(done, "file"), failed.join(", "))
        });
        self.scroll = Scroll::EnsureVisible;
    }

    /// Buffers visiting `from` (or files under it) visit the same files at `to`.
    fn follow_rename(&mut self, from: &Path, to: &Path) {
        let cwd = std::env::current_dir().unwrap_or_default();
        let moved = |p: &Path| cwd.join(p).strip_prefix(from).ok().map(|rest| to.join(rest));
        let mut renamed = Vec::new();
        let bufs =
            std::iter::once((self.buf_id, &mut self.buf)).chain(self.others.iter_mut().map(|s| (s.id, &mut s.buf)));
        for (id, b) in bufs {
            if let Some(new) = b.path.as_deref().and_then(moved) {
                if b.path.as_deref().and_then(Path::file_name) != new.file_name() {
                    renamed.push((id, new.file_name().unwrap_or_default().to_string_lossy().into_owned()));
                }
                b.path = Some(new);
            }
        }
        for (id, d) in self.direds.iter_mut() {
            if let Some(new) = moved(&d.dir) {
                renamed.push((*id, buffer_name(&new)));
                d.dir = new;
            }
        }
        for (id, name) in renamed {
            let name = self.unique_name(&name);
            if id == self.buf_id {
                self.buf.name = name;
            } else if let Some(s) = self.others.iter_mut().find(|s| s.id == id) {
                s.buf.name = name;
            }
        }
    }

    /// Create the directory typed in the open picker (and its parents).
    pub(crate) fn dired_create_directory_at(&mut self, path: PathBuf) {
        if fs::symlink_metadata(&path).is_ok() {
            if let Some(p) = self.picker.as_mut() {
                p.message = Some(format!("{} already exists", abbreviate(&path)));
            }
            return;
        }
        if let Err(e) = fs::create_dir_all(&path) {
            if let Some(p) = self.picker.as_mut() {
                p.message = Some(format!("Cannot create {}: {e}", abbreviate(&path)));
            }
            return;
        }
        self.picker = None;
        self.dired_revert_all();
        if let Some(d) = self.dired_state()
            && path.parent() == Some(d.dir.as_path())
            && let Some(line) = d.line_of(&path.file_name().unwrap_or_default().to_string_lossy())
        {
            self.dired_goto_line(line);
        }
        self.message = Some(format!("Created {}", abbreviate(&path)));
        self.scroll = Scroll::EnsureVisible;
    }

    /// Visit the entry at point: a directory in a listing, a file in its buffer.
    fn dired_visit(&mut self, other_window: bool) -> CmdResult {
        let (d, e) = match (self.dired_state(), self.dired_entry_at_point()) {
            (Some(d), Some(e)) => (d, e),
            (Some(_), None) => return Err("No file on this line".into()),
            _ => return Err("Not in a Dired buffer".into()),
        };
        let path = d.path_of(e);
        let from = (e.is_parent()).then(|| d.dir.clone());
        let is_dir = e.is_dir();
        if other_window {
            if self.layout.window_order().len() == 1 {
                self.split_window(SplitDir::Vertical)?;
            }
            self.other_window(1);
        }
        if is_dir {
            self.dired(&path)?;
            if let Some(from) = from {
                self.dired_point_on(&from);
            }
            Ok(())
        } else {
            self.find_file(&path)
        }
    }

    /// Put point on `path`'s entry, if the current listing shows it.
    fn dired_point_on(&mut self, path: &Path) {
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else { return };
        if let Some(line) = self.dired_state().and_then(|d| d.line_of(&name)) {
            self.dired_goto_line(line);
        }
    }
}

/// The position of `line`'s file name (or its end, if shorter).
fn name_point(r: &Rope, line: usize) -> usize {
    let line = line.min(r.len_lines().saturating_sub(1));
    let start = r.line_to_char(line);
    start + NAME_COL.min(super::text::line_len(r, line))
}

// ============================================================== commands

/// Dired commands (looked up by `commands::lookup`).
pub static COMMANDS: &[Command] = &[
    Command {
        name: "dired",
        run: dired_cmd,
        flags: flags::NO_UNDO,
        doc: "List a directory to visit, mark and operate on its files.",
    },
    Command {
        name: "dired-jump",
        run: dired_jump,
        flags: flags::NO_UNDO,
        doc: "List the current file's directory with point on the file (in a listing: go up).",
    },
    Command {
        name: "dired-find-file",
        run: dired_find_file,
        flags: flags::NO_UNDO,
        doc: "Visit the file or directory at point.",
    },
    Command {
        name: "dired-find-file-other-window",
        run: dired_find_file_other_window,
        flags: flags::NO_UNDO,
        doc: "Visit the file or directory at point in another window.",
    },
    Command {
        name: "dired-up-directory",
        run: dired_up_directory,
        flags: flags::NO_UNDO,
        doc: "List the parent directory, with point on this one.",
    },
    Command {
        name: "dired-next-line",
        run: dired_next_line,
        flags: flags::NO_UNDO,
        doc: "Move down N entries, to the file name.",
    },
    Command {
        name: "dired-previous-line",
        run: dired_previous_line,
        flags: flags::NO_UNDO,
        doc: "Move up N entries, to the file name.",
    },
    Command {
        name: "dired-mark",
        run: dired_mark,
        flags: flags::NO_UNDO,
        doc: "Mark the entry at point (N entries) and move down.",
    },
    Command {
        name: "dired-unmark",
        run: dired_unmark,
        flags: flags::NO_UNDO,
        doc: "Unmark the entry at point (N entries) and move down.",
    },
    Command {
        name: "dired-unmark-backward",
        run: dired_unmark_backward,
        flags: flags::NO_UNDO,
        doc: "Move up and unmark the entry there (N entries).",
    },
    Command {
        name: "dired-unmark-all-marks",
        run: dired_unmark_all,
        flags: flags::NO_UNDO,
        doc: "Remove every mark and deletion flag.",
    },
    Command {
        name: "dired-toggle-marks",
        run: dired_toggle_marks,
        flags: flags::NO_UNDO,
        doc: "Mark the unmarked entries and unmark the marked ones.",
    },
    Command {
        name: "dired-flag-file-deletion",
        run: dired_flag_file_deletion,
        flags: flags::NO_UNDO,
        doc: "Flag the entry at point (N entries) for deletion and move down.",
    },
    Command {
        name: "dired-do-flagged-delete",
        run: dired_do_flagged_delete,
        flags: flags::NO_UNDO,
        doc: "Delete the entries flagged for deletion (repeat to confirm).",
    },
    Command {
        name: "dired-do-delete",
        run: dired_do_delete,
        flags: flags::NO_UNDO,
        doc: "Delete the marked entries, or the one at point (repeat to confirm).",
    },
    Command {
        name: "dired-do-rename",
        run: dired_do_rename,
        flags: flags::NO_UNDO,
        doc: "Rename or move the marked entries, or the one at point.",
    },
    Command {
        name: "dired-do-copy",
        run: dired_do_copy,
        flags: flags::NO_UNDO,
        doc: "Copy the marked entries, or the one at point.",
    },
    Command {
        name: "dired-create-directory",
        run: dired_create_directory,
        flags: flags::NO_UNDO,
        doc: "Create a directory (and any missing parents).",
    },
    Command {
        name: "dired-revert",
        run: dired_revert,
        flags: flags::NO_UNDO,
        doc: "Read the directory again (marks are kept).",
    },
    Command {
        name: "dired-sort-toggle",
        run: dired_sort_toggle,
        flags: flags::NO_UNDO,
        doc: "Sort the listing by name or by date, newest first.",
    },
    Command {
        name: "dired-copy-filename-as-kill",
        run: dired_copy_filename,
        flags: flags::NO_UNDO,
        doc: "Put the marked entries' names (or the one at point) on the kill ring; with C-u 0, absolute paths.",
    },
];

fn dired_cmd(ed: &mut Editor, _: &Args) -> CmdResult {
    let dir = ed.default_directory();
    ed.open_file_picker_in("Dired", &dir, FilePurpose::Dired);
    Ok(())
}

fn dired_jump(ed: &mut Editor, _: &Args) -> CmdResult {
    if ed.dired_state().is_some() {
        return dired_up_directory(ed, &Args::default());
    }
    let file = ed.buf.path.as_deref().map(normalize);
    let dir = ed.default_directory();
    ed.dired(&dir)?;
    if let Some(f) = file {
        ed.dired_point_on(&f);
    }
    Ok(())
}

fn dired_find_file(ed: &mut Editor, _: &Args) -> CmdResult {
    ed.dired_visit(false)
}

fn dired_find_file_other_window(ed: &mut Editor, _: &Args) -> CmdResult {
    ed.dired_visit(true)
}

fn dired_up_directory(ed: &mut Editor, _: &Args) -> CmdResult {
    let d = ed.dired_state().ok_or("Not in a Dired buffer")?;
    let here = d.dir.clone();
    let parent = here.parent().ok_or("Already at the root")?.to_path_buf();
    ed.dired(&parent)?;
    ed.dired_point_on(&here);
    Ok(())
}

fn dired_move(ed: &mut Editor, n: i64) -> CmdResult {
    if ed.dired_state().is_none() {
        return Err("Not in a Dired buffer".into());
    }
    let line = (ed.dired_line() as i64 + n).max(0) as usize;
    ed.dired_goto_line(line);
    Ok(())
}

fn dired_next_line(ed: &mut Editor, a: &Args) -> CmdResult {
    dired_move(ed, a.n())
}

fn dired_previous_line(ed: &mut Editor, a: &Args) -> CmdResult {
    dired_move(ed, -a.n())
}

fn dired_mark(ed: &mut Editor, a: &Args) -> CmdResult {
    ed.dired_mark_lines(a.n(), MARK)
}

fn dired_unmark(ed: &mut Editor, a: &Args) -> CmdResult {
    ed.dired_mark_lines(a.n(), ' ')
}

fn dired_unmark_backward(ed: &mut Editor, a: &Args) -> CmdResult {
    ed.dired_mark_lines(-a.n(), ' ')
}

fn dired_flag_file_deletion(ed: &mut Editor, a: &Args) -> CmdResult {
    ed.dired_mark_lines(a.n(), FLAG)
}

fn for_each_entry_line(ed: &mut Editor, f: impl Fn(char) -> Option<char>) -> usize {
    let n = ed.dired_state().map_or(0, |d| d.entries.len());
    let mut changed = 0;
    for i in 0..n {
        let line = HEADER_LINES + i;
        let Some(e) = ed.dired_state().and_then(|d| d.entry_at(line)) else { break };
        if e.is_parent() {
            continue;
        }
        if let Some(m) = f(e.mark)
            && m != e.mark
        {
            ed.dired_set_mark(line, m);
            changed += 1;
        }
    }
    changed
}

fn dired_unmark_all(ed: &mut Editor, _: &Args) -> CmdResult {
    if ed.dired_state().is_none() {
        return Err("Not in a Dired buffer".into());
    }
    let n = for_each_entry_line(ed, |_| Some(' '));
    ed.message = Some(if n == 0 { "No marks".into() } else { format!("Removed {}", plural(n, "mark")) });
    Ok(())
}

fn dired_toggle_marks(ed: &mut Editor, _: &Args) -> CmdResult {
    if ed.dired_state().is_none() {
        return Err("Not in a Dired buffer".into());
    }
    for_each_entry_line(ed, |m| match m {
        ' ' => Some(MARK),
        MARK => Some(' '),
        _ => None,
    });
    Ok(())
}

fn dired_do_flagged_delete(ed: &mut Editor, _: &Args) -> CmdResult {
    let d = ed.dired_state().ok_or("Not in a Dired buffer")?;
    let paths = d.marked(FLAG).iter().map(|e| d.path_of(e)).collect();
    ed.dired_delete(paths, "files flagged for deletion")
}

fn dired_do_delete(ed: &mut Editor, _: &Args) -> CmdResult {
    let paths = ed.dired_targets()?;
    ed.dired_delete(paths, "files to delete")
}

fn dired_do_rename(ed: &mut Editor, _: &Args) -> CmdResult {
    ed.dired_ask_target(Transfer::Rename)
}

fn dired_do_copy(ed: &mut Editor, _: &Args) -> CmdResult {
    ed.dired_ask_target(Transfer::Copy)
}

fn dired_create_directory(ed: &mut Editor, _: &Args) -> CmdResult {
    let d = ed.dired_state().ok_or("Not in a Dired buffer")?;
    let dir = d.dir.clone();
    ed.open_file_picker_in("Create directory", &dir, FilePurpose::Mkdir);
    Ok(())
}

fn dired_revert(ed: &mut Editor, _: &Args) -> CmdResult {
    if ed.dired_state().is_none() {
        return Err("Not in a Dired buffer".into());
    }
    let id = ed.buf_id;
    ed.dired_revert_buffer(id)
}

fn dired_sort_toggle(ed: &mut Editor, _: &Args) -> CmdResult {
    let id = ed.buf_id;
    let d = ed.direds.get_mut(&id).ok_or("Not in a Dired buffer")?;
    d.sort = match d.sort {
        Sort::Name => Sort::Time,
        Sort::Time => Sort::Name,
    };
    ed.message = Some(format!("Sorted by {}", if d.sort == Sort::Time { "date" } else { "name" }));
    ed.dired_revert_buffer(id)
}

fn dired_copy_filename(ed: &mut Editor, a: &Args) -> CmdResult {
    let paths = ed.dired_targets()?;
    let absolute = a.raw.is_some() && a.n() == 0;
    let names: Vec<String> = paths
        .iter()
        .map(|p| {
            if absolute {
                p.display().to_string()
            } else {
                p.file_name().unwrap_or_default().to_string_lossy().into_owned()
            }
        })
        .collect();
    let text = names.join(" ");
    ed.record_kill(0, text.clone(), false);
    ed.message = Some(text);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_lines_put_names_in_their_column() {
        let e = Entry {
            name: "main.rs".into(),
            kind: EntryKind::File { executable: false },
            perms: "-rw-r--r--".into(),
            size: Some(1536),
            mtime: Some(SystemTime::now()),
            link_target: None,
            mark: '*',
        };
        let line = render_entry(&e, SystemTime::now());
        assert_eq!(line.chars().position(|c| c == 'm'), Some(NAME_COL), "{line}");
        assert!(line.starts_with("* -rw-r--r--   1.5 KB  "), "{line}");
        let old = render_entry(
            &Entry { mtime: Some(UNIX_EPOCH + std::time::Duration::from_secs(86400 * 400)), ..e },
            SystemTime::now(),
        );
        assert_eq!(old.chars().position(|c| c == 'm'), Some(NAME_COL), "{old}");
    }

    #[test]
    fn paths_are_normalized_textually() {
        assert_eq!(normalize(Path::new("/a/./b/../c")), PathBuf::from("/a/c"));
        assert_eq!(normalize(Path::new("/..")), PathBuf::from("/"));
        assert_eq!(buffer_name(Path::new("/a/src")), "src/");
        assert_eq!(buffer_name(Path::new("/")), "/");
    }
}
