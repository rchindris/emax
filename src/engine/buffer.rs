//! Text buffer: a rope plus file association and a grouped undo/redo history.

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::ops::Range;
use std::path::PathBuf;

use ropey::Rope;

use super::cursor::Cursor;

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

pub struct Buffer {
    pub rope: Rope,
    pub path: Option<PathBuf>,
    pub name: String,
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
            path: None,
            name: name.into(),
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

    /// Break amalgamation so the next self-insert starts a new undo group.
    pub fn undo_boundary(&mut self) {
        if let Some(top) = self.undo.last_mut() {
            top.amalgamate = false;
        }
    }

    fn apply_inverse(&mut self, edits: &[Edit]) {
        for e in edits.iter().rev() {
            let n = e.new.chars().count();
            self.rope.remove(e.pos..e.pos + n);
            self.rope.insert(e.pos, &e.old);
        }
    }

    fn apply_forward(&mut self, edits: &[Edit]) {
        for e in edits {
            let n = e.old.chars().count();
            self.rope.remove(e.pos..e.pos + n);
            self.rope.insert(e.pos, &e.new);
        }
    }

    /// Undo one group; returns the cursors to restore.
    pub fn undo(&mut self) -> Option<Vec<Cursor>> {
        let g = self.undo.pop()?;
        self.apply_inverse(&g.edits);
        let cursors = g.before.clone();
        self.redo.push(g);
        Some(cursors)
    }

    pub fn redo(&mut self) -> Option<Vec<Cursor>> {
        let mut g = self.redo.pop()?;
        self.apply_forward(&g.edits);
        let cursors = g.after.clone();
        g.amalgamate = false;
        self.undo.push(g);
        Some(cursors)
    }
}
