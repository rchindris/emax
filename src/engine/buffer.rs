//! Text buffer: a rope plus file association and a grouped undo/redo history.

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::ops::Range;
use std::path::PathBuf;

use ropey::Rope;

use super::cursor::Cursor;
use super::modes::BufferModes;

/// A change undo or redo made to the text: chars `start..end` were replaced
/// by `inserted` chars (the shape `Cursor::adjust` takes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Change {
    pub start: usize,
    pub end: usize,
    pub inserted: usize,
}

/// A primitive change: at `pos`, `old` was replaced by `new`.
#[derive(Clone, Debug)]
pub struct Edit {
    pub pos: usize,
    pub old: String,
    pub new: String,
}

/// All edits made by one command, plus cursor snapshots to restore on undo/redo.
#[derive(Clone, Debug)]
struct Group {
    id: u64,
    edits: Vec<Edit>,
    before: Vec<Cursor>,
    after: Vec<Cursor>,
    /// Consecutive self-inserts are merged into one group (like Emacs' amalgamation).
    amalgamate: bool,
    count: usize,
}

/// Identifies a buffer for as long as the editor runs.
pub type BufferId = u64;

/// What a buffer holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BufferKind {
    /// Text (a file, *scratch*...).
    #[default]
    Text,
    /// A terminal: its screen is in `Editor.terminals`; the rope holds the
    /// text copy only in copy mode.
    Terminal,
}

pub struct Buffer {
    pub rope: Rope,
    pub kind: BufferKind,
    pub path: Option<PathBuf>,
    pub name: String,
    /// Major and minor modes, buffer-local settings (set by the editor).
    pub modes: BufferModes,
    undo: Vec<Group>,
    redo: Vec<Group>,
    open: Option<Group>,
    next_id: u64,
    saved_id: Option<u64>,
}

const AMALGAMATE_LIMIT: usize = 20;

impl Buffer {
    pub fn from_str(name: &str, text: &str) -> Self {
        Self {
            rope: Rope::from_str(text),
            kind: BufferKind::Text,
            path: None,
            name: name.into(),
            modes: BufferModes::default(),
            undo: Vec::new(),
            redo: Vec::new(),
            open: None,
            next_id: 1,
            saved_id: None,
        }
    }

    pub fn open(path: PathBuf) -> std::io::Result<Self> {
        let rope = if path.exists() {
            Rope::from_reader(BufReader::new(File::open(&path)?))?
        } else {
            Rope::new()
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let mut b = Self::from_str(&name, "");
        b.rope = rope;
        b.path = Some(path);
        Ok(b)
    }

    pub fn save(&mut self) -> std::io::Result<String> {
        let Some(path) = &self.path else {
            return Ok("Buffer is not visiting a file".into());
        };
        self.rope.write_to(BufWriter::new(File::create(path)?))?;
        self.saved_id = self.undo.last().map(|g| g.id);
        Ok(format!("Wrote {}", path.display()))
    }

    pub fn modified(&self) -> bool {
        self.undo.last().map(|g| g.id) != self.saved_id
    }

    // ------------------------------------------------------------ editing

    /// Replace `range` with `text`, recording it in the open undo group.
    /// Returns the replaced text.
    pub fn replace(&mut self, range: Range<usize>, text: &str) -> String {
        let old = self.rope.slice(range.clone()).to_string();
        if !range.is_empty() {
            self.rope.remove(range.clone());
        }
        if !text.is_empty() {
            self.rope.insert(range.start, text);
        }
        if let Some(g) = &mut self.open {
            g.edits.push(Edit { pos: range.start, old: old.clone(), new: text.to_string() });
        }
        old
    }

    // --------------------------------------------------------------- undo

    pub fn begin_group(&mut self, cursors: &[Cursor], amalgamate: bool) {
        self.open = Some(Group {
            id: 0,
            edits: Vec::new(),
            before: cursors.to_vec(),
            after: Vec::new(),
            amalgamate,
            count: 1,
        });
    }

    /// Close the open group. Returns true if the command changed the buffer.
    pub fn end_group(&mut self, cursors: &[Cursor]) -> bool {
        let Some(mut g) = self.open.take() else { return false };
        if g.edits.is_empty() {
            return false;
        }
        g.after = cursors.to_vec();
        self.redo.clear();
        if let Some(top) = self.undo.last_mut() {
            let mergeable = g.amalgamate
                && top.amalgamate
                && top.count < AMALGAMATE_LIMIT
                && Some(top.id) != self.saved_id;
            if mergeable {
                top.edits.extend(g.edits);
                top.after = g.after;
                top.count += 1;
                return true;
            }
        }
        g.id = self.next_id;
        self.next_id += 1;
        self.undo.push(g);
        true
    }

    /// Take back the open group's changes (a read-only buffer refused them).
    /// Returns the cursors from before, if anything had changed.
    pub fn abort_group(&mut self) -> Option<Vec<Cursor>> {
        let g = self.open.take()?;
        if g.edits.is_empty() {
            return None;
        }
        let _ = self.apply_inverse(&g.edits);
        Some(g.before)
    }

    /// Break amalgamation so the next self-insert starts a new undo group.
    pub fn undo_boundary(&mut self) {
        if let Some(top) = self.undo.last_mut() {
            top.amalgamate = false;
        }
    }

    /// Undo `edits`; returns the changes made, in order.
    fn apply_inverse(&mut self, edits: &[Edit]) -> Vec<Change> {
        let mut changes = Vec::with_capacity(edits.len());
        for e in edits.iter().rev() {
            let n = e.new.chars().count();
            self.rope.remove(e.pos..e.pos + n);
            self.rope.insert(e.pos, &e.old);
            changes.push(Change { start: e.pos, end: e.pos + n, inserted: e.old.chars().count() });
        }
        changes
    }

    /// Redo `edits`; returns the changes made, in order.
    fn apply_forward(&mut self, edits: &[Edit]) -> Vec<Change> {
        let mut changes = Vec::with_capacity(edits.len());
        for e in edits {
            let n = e.old.chars().count();
            self.rope.remove(e.pos..e.pos + n);
            self.rope.insert(e.pos, &e.new);
            changes.push(Change { start: e.pos, end: e.pos + n, inserted: e.new.chars().count() });
        }
        changes
    }

    /// Undo one group; returns the cursors to restore and the changes made
    /// (so other windows showing the buffer can follow them).
    pub fn undo(&mut self) -> Option<(Vec<Cursor>, Vec<Change>)> {
        let g = self.undo.pop()?;
        let changes = self.apply_inverse(&g.edits);
        let cursors = g.before.clone();
        self.redo.push(g);
        Some((cursors, changes))
    }

    pub fn redo(&mut self) -> Option<(Vec<Cursor>, Vec<Change>)> {
        let mut g = self.redo.pop()?;
        let changes = self.apply_forward(&g.edits);
        let cursors = g.after.clone();
        g.amalgamate = false;
        self.undo.push(g);
        Some((cursors, changes))
    }
}
