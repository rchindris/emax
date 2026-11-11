//! The editor state that commands operate on: one buffer, one or more cursors,
//! the kill ring, and per-command bookkeeping (undo groups, kill appending,
//! transient-mark deactivation, shift-selection).

use std::ops::Range;

use ropey::Rope;

use super::buffer::Buffer;
use super::buffers::BufferSlot;
use super::picker::Picker;
use super::search::Swiper;
use super::commands::{self, flags, Command};
use super::cursor::Cursor;
use super::text;

/// The raw prefix argument, as typed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RawArg {
    /// `C-u` pressed `n` times (value 4^n).
    Universal(u32),
    Number(i64),
    /// Just `M--` / `C-u -`.
    Minus,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Args {
    pub raw: Option<RawArg>,
    /// Shift-translated key (shift+motion selects).
    pub shift: bool,
    /// Text typed (self-insert) or char read (zap-to-char, quoted-insert).
    pub text: Option<String>,
}

impl Args {
    pub fn n(&self) -> i64 {
        match self.raw {
            None => 1,
            Some(RawArg::Universal(k)) => 4i64.pow(k),
            Some(RawArg::Number(n)) => n,
            Some(RawArg::Minus) => -1,
        }
    }
    pub fn with_raw(raw: Option<RawArg>) -> Self {
        Self { raw, ..Default::default() }
    }
}

/// What the view should do with scrolling after a command.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scroll {
    /// Keep the primary cursor (or `reveal`) on screen with minimal movement.
    EnsureVisible,
    Center,
    Top,
    Bottom,
    /// Scroll by a page (+1 forward, -1 back); point already moved by the same amount.
    Page(i64),
    /// Leave the scroll position alone (e.g. after switching buffers).
    Keep,
}

pub const KILL_RING_MAX: usize = 120;

pub struct Editor {
    pub buf: Buffer,
    /// Always sorted by point, never two at the same point.
    pub cursors: Vec<Cursor>,
    pub primary: u64,
    pub(crate) next_cursor_id: u64,
    /// Id of the current buffer; other buffers are in `others`, most recent first.
    pub buf_id: u64,
    pub others: Vec<BufferSlot>,
    pub(crate) next_buf_id: u64,
    /// An open completing-read picker (e.g. C-x b). Takes the keyboard while open.
    pub picker: Option<Picker>,
    /// An open swiper search (C-s). Takes the keyboard while open.
    pub swiper: Option<Swiper>,
    /// The last accepted search query (C-s C-s repeats it).
    pub last_search: String,

    pub kill_ring: Vec<String>,
    /// When the last kill came from several cursors: one entry per cursor.
    kill_multi: Option<Vec<String>>,
    yank_index: usize,
    kill_acc: Vec<String>,
    kill_prepend: bool,
    kill_happened: bool,
    last_was_kill: bool,

    pub message: Option<String>,
    pub fill_column: usize,
    pub tab_width: usize,
    pub last_command: &'static str,
    pub this_command: &'static str,
    /// Text to publish to the system clipboard (set on kill/copy).
    pub clipboard_out: Option<String>,
    /// Lines visible on screen, maintained by the view (for C-v / M-v).
    pub page_lines: usize,
    pub scroll: Scroll,
    /// A position the view should bring on screen (e.g. a newly added cursor).
    pub reveal: Option<usize>,
    pub recenter_step: usize,
    pub quit_requested: bool,
}

impl Editor {
    pub fn new(buf: Buffer) -> Self {
        Self {
            buf,
            cursors: vec![Cursor::new(1, 0)],
            primary: 1,
            next_cursor_id: 2,
            buf_id: 1,
            others: Vec::new(),
            next_buf_id: 2,
            picker: None,
            swiper: None,
            last_search: String::new(),
            kill_ring: Vec::new(),
            kill_multi: None,
            yank_index: 0,
            kill_acc: Vec::new(),
            kill_prepend: false,
            kill_happened: false,
            last_was_kill: false,
            message: None,
            fill_column: 70,
            tab_width: 4,
            last_command: "",
            this_command: "",
            clipboard_out: None,
            page_lines: 30,
            scroll: Scroll::EnsureVisible,
            reveal: None,
            recenter_step: 0,
            quit_requested: false,
        }
    }

    // ------------------------------------------------------------- accessors

    pub fn rope(&self) -> &Rope {
        &self.buf.rope
    }

    pub fn len(&self) -> usize {
        self.buf.rope.len_chars()
    }

    pub fn primary_index(&self) -> usize {
        self.cursors.iter().position(|c| c.id == self.primary).unwrap_or(0)
    }

    pub fn primary_cursor(&self) -> &Cursor {
        &self.cursors[self.primary_index()]
    }

    pub fn text(&self, r: Range<usize>) -> String {
        self.buf.rope.slice(r).to_string()
    }

    // ------------------------------------------------------------- cursors

    pub fn add_cursor(&mut self, mut c: Cursor) -> u64 {
        c.id = self.next_cursor_id;
        self.next_cursor_id += 1;
        let id = c.id;
        self.cursors.push(c);
        id
    }

    pub fn keep_only_primary(&mut self) {
        let p = self.primary_cursor().clone();
        self.cursors = vec![p];
    }

    /// Sort cursors and merge any that landed on the same point.
    pub fn normalize_cursors(&mut self) {
        let primary = self.primary;
        self.cursors.sort_by_key(|c| (c.point, c.id != primary));
        self.cursors.dedup_by(|b, a| {
            if a.point == b.point {
                if b.id == primary {
                    std::mem::swap(a, b);
                }
                true
            } else {
                false
            }
        });
        if !self.cursors.iter().any(|c| c.id == primary) {
            self.primary = self.cursors[0].id;
        }
    }

    // ------------------------------------------------------------- editing

    /// Replace `range` with `text`, keeping every cursor's point/mark consistent.
    pub fn replace(&mut self, range: Range<usize>, text: &str) -> String {
        let inserted = text.chars().count();
        let old = self.buf.replace(range.clone(), text);
        for c in &mut self.cursors {
            c.adjust(range.start, range.end, inserted);
        }
        old
    }

    pub fn insert_at_cursor(&mut self, i: usize, text: &str) {
        let p = self.cursors[i].point;
        self.replace(p..p, text);
        self.cursors[i].point = p + text.chars().count();
    }

    /// Delete the active region of cursor `i` (delete-selection-mode). Returns true if it did.
    pub fn delete_active_region(&mut self, i: usize) -> bool {
        match self.cursors[i].region() {
            Some(r) if !r.is_empty() => {
                self.replace(r.clone(), "");
                self.cursors[i].point = r.start;
                self.cursors[i].deactivate();
                true
            }
            _ => false,
        }
    }

    // ------------------------------------------------------------ kill ring

    /// Record killed text for cursor `i`; consecutive kills append (or prepend).
    pub fn kill(&mut self, i: usize, range: Range<usize>, prepend: bool) {
        let s = self.replace(range.clone(), "");
        self.cursors[i].point = range.start;
        self.record_kill(i, s, prepend);
    }

    pub fn record_kill(&mut self, i: usize, s: String, prepend: bool) {
        if self.kill_acc.len() < self.cursors.len() {
            self.kill_acc.resize(self.cursors.len(), String::new());
        }
        if prepend {
            self.kill_acc[i] = s + &self.kill_acc[i];
        } else {
            self.kill_acc[i].push_str(&s);
        }
        self.kill_prepend = prepend;
        self.kill_happened = true;
    }

    fn commit_kills(&mut self) {
        let texts = std::mem::take(&mut self.kill_acc);
        let prepend = self.kill_prepend;
        let join = |p: &str, t: &str| if prepend { format!("{t}{p}") } else { format!("{p}{t}") };
        let appending = self.last_was_kill && !self.kill_ring.is_empty();

        if appending {
            if texts.len() > 1 {
                if let Some(m) = &mut self.kill_multi {
                    if m.len() == texts.len() {
                        for (old, t) in m.iter_mut().zip(&texts) {
                            *old = join(old, t);
                        }
                        let joined = m.join("\n");
                        *self.kill_ring.last_mut().unwrap() = joined.clone();
                        self.clipboard_out = Some(joined);
                        return;
                    }
                }
            } else if self.kill_multi.is_none() {
                let top = self.kill_ring.last_mut().unwrap();
                *top = join(top, &texts[0]);
                self.clipboard_out = Some(top.clone());
                return;
            }
        }
        let joined = texts.join("\n");
        self.kill_multi = (texts.len() > 1).then_some(texts);
        self.push_kill(joined);
    }

    fn push_kill(&mut self, s: String) {
        self.kill_ring.push(s.clone());
        if self.kill_ring.len() > KILL_RING_MAX {
            self.kill_ring.remove(0);
        }
        self.yank_index = 0;
        self.clipboard_out = Some(s);
    }

    /// Text arriving from the system clipboard (another app copied something).
    pub fn clipboard_in(&mut self, s: String) {
        if s.is_empty() || self.kill_ring.last() == Some(&s) {
            return;
        }
        self.kill_ring.push(s);
        self.kill_multi = None;
        self.yank_index = 0;
    }

    /// Text each cursor should yank (per-cursor when the kill came from as many cursors).
    pub fn yank_texts(&self, index: usize) -> Option<Vec<String>> {
        let len = self.kill_ring.len();
        if len == 0 {
            return None;
        }
        let i = index % len;
        if i == 0 {
            if let Some(m) = &self.kill_multi {
                if m.len() == self.cursors.len() {
                    return Some(m.clone());
                }
            }
        }
        Some(vec![self.kill_ring[len - 1 - i].clone(); self.cursors.len()])
    }

    pub fn yank_index(&self) -> usize {
        self.yank_index
    }

    pub fn set_yank_index(&mut self, i: usize) {
        self.yank_index = i;
    }

    // --------------------------------------------------------- run command

    pub fn execute(&mut self, name: &str, args: Args) {
        match commands::lookup(name) {
            Some(cmd) => self.run(cmd, args),
            None => self.message = Some(format!("Unknown command: {name}")),
        }
    }

    pub fn run(&mut self, cmd: &Command, args: Args) {
        self.this_command = cmd.name;
        self.message = None;
        self.reveal = None;
        self.scroll = Scroll::EnsureVisible;
        self.kill_acc = vec![String::new(); self.cursors.len()];
        self.kill_happened = false;

        let undoable = cmd.flags & flags::NO_UNDO == 0;
        let self_insert = cmd.name == "self-insert-command";
        if self_insert && self.last_command != "self-insert-command" {
            self.buf.undo_boundary();
        }
        if undoable {
            self.buf.begin_group(&self.cursors, self_insert);
        }

        // Shift-selection (Emacs' shift-select-mode).
        if cmd.flags & flags::MOTION != 0 {
            for c in &mut self.cursors {
                if args.shift {
                    if !c.active {
                        c.mark = Some(c.point);
                        c.active = true;
                        c.shift_selected = true;
                    }
                } else if c.shift_selected {
                    c.deactivate();
                }
            }
        }

        if let Err(e) = (cmd.run)(self, &args) {
            self.message = Some(e);
        }

        let edited = undoable && self.buf.end_group(&self.cursors);
        if cmd.flags & flags::VERTICAL == 0 {
            for c in &mut self.cursors {
                c.goal_col = None;
            }
        }
        if edited && cmd.flags & flags::KEEP_REGION == 0 {
            for c in &mut self.cursors {
                c.deactivate();
            }
        }
        if self.kill_happened {
            self.commit_kills();
        }
        self.last_was_kill = self.kill_happened;
        self.normalize_cursors();
        self.last_command = cmd.name;
    }

    /// Apply `f` to every cursor, collecting the first error.
    pub fn each<F>(&mut self, mut f: F) -> Result<(), String>
    where
        F: FnMut(&mut Editor, usize) -> Result<(), String>,
    {
        let mut err = None;
        for i in 0..self.cursors.len() {
            if let Err(e) = f(self, i) {
                err.get_or_insert(e);
            }
        }
        err.map_or(Ok(()), Err)
    }

    /// Move every cursor to the position `f` computes. On error the cursor stays.
    pub fn move_each<F>(&mut self, f: F) -> Result<(), String>
    where
        F: Fn(&Rope, &Cursor) -> Result<usize, String>,
    {
        self.each(|ed, i| {
            let p = f(&ed.buf.rope, &ed.cursors[i])?;
            ed.cursors[i].point = p;
            Ok(())
        })
    }

    /// Line/column of the primary cursor (0-based).
    pub fn primary_line_col(&self) -> (usize, usize) {
        let p = self.primary_cursor().point;
        let line = text::line_of(self.rope(), p);
        (line, p - self.rope().line_to_char(line))
    }
}
