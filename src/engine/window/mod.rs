//! Windows and tabs, Emacs-style.
//!
//! A *window* shows a buffer. Each tab (as in Emacs' tab-bar-mode) has its
//! own tree of windows: a node is either one window ([`Node::Leaf`]) or a
//! split ([`Node::Split`]) of two or more nodes side by side or stacked,
//! with the share of the space each child gets (`ratios`, summing to 1).
//!
//! One window is *selected* (in the current tab): its buffer is the editor's
//! current buffer (`Editor.buf`) and its point and cursors are
//! `Editor.cursors`, so every command works on it unchanged. Other windows
//! keep their own cursors in [`Window::cursors`]; the same buffer can be shown
//! in several windows, each with its own point. Selecting another window
//! swaps these over.
//!
//! The pure tree operations and geometry are in [`tree`]; the editor side
//! (selecting windows, switching tabs) and the commands in `ops`.
//!
//! Invariants (see [`Layout::check`]): there is always at least one tab and
//! each tab has at least one window; a tab's `selected` is one of its
//! leaves; `Layout.windows` holds exactly the leaves of all tabs.

mod ops;
#[cfg(test)]
mod tests;
pub mod tree;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::buffer::BufferId;
use super::cursor::Cursor;

pub use ops::COMMANDS;
pub use tree::{Direction, MIN_COLS, MIN_ROWS, ResizeError, compute_rects, window_rect};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct WindowId(pub u64);

/// How a split arranges its children.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SplitDir {
    /// Side by side, left to right (`split-window-right`, C-x 3).
    Horizontal,
    /// Stacked, top to bottom (`split-window-below`, C-x 2).
    Vertical,
}

/// A window tree.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Node {
    Leaf(WindowId),
    Split {
        dir: SplitDir,
        /// At least two.
        children: Vec<Node>,
        /// One per child, each > 0, summing to 1.
        ratios: Vec<f32>,
    },
}

impl Node {
    /// The windows in this tree, in order (left to right, top to bottom).
    pub fn windows(&self) -> Vec<WindowId> {
        let mut v = Vec::new();
        self.collect(&mut v);
        v
    }

    fn collect(&self, out: &mut Vec<WindowId>) {
        match self {
            Node::Leaf(w) => out.push(*w),
            Node::Split { children, .. } => children.iter().for_each(|c| c.collect(out)),
        }
    }
}

/// A window: which buffer it shows and, when not selected, where its cursors are.
#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub id: WindowId,
    pub buffer: BufferId,
    /// Stale while the window is selected (then `Editor.cursors` is live).
    pub cursors: Vec<Cursor>,
    pub primary: u64,
    pub next_cursor_id: u64,
    /// First line shown (kept by the view; saved in the session file).
    pub scroll_line: usize,
}

impl Window {
    pub fn new(id: WindowId, buffer: BufferId) -> Self {
        Self { id, buffer, cursors: vec![Cursor::new(1, 0)], primary: 1, next_cursor_id: 2, scroll_line: 0 }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tab {
    /// Shown in the tab bar: the explicit name, or the selected window's buffer name.
    pub name: Option<String>,
    pub root: Node,
    pub selected: WindowId,
}

/// A rectangle in the view's coordinates (pixels or cells, as the caller likes).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// All tabs and windows.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub tabs: Vec<Tab>,
    pub current_tab: usize,
    pub windows: HashMap<WindowId, Window>,
    next_window_id: u64,
    /// The size of the windows' area in cells, as last reported by the view
    /// (resizing and windmove work in cells).
    frame: (f32, f32),
}

/// The frame size assumed until the view reports one.
pub const DEFAULT_FRAME: (f32, f32) = (80.0, 24.0);

impl Layout {
    /// One tab with one window showing `buffer`.
    pub fn new(buffer: BufferId) -> Self {
        let id = WindowId(1);
        let mut windows = HashMap::new();
        windows.insert(id, Window::new(id, buffer));
        Self {
            tabs: vec![Tab { name: None, root: Node::Leaf(id), selected: id }],
            current_tab: 0,
            windows,
            next_window_id: 2,
            frame: DEFAULT_FRAME,
        }
    }

    /// A fresh window id.
    pub fn alloc_window_id(&mut self) -> WindowId {
        let id = WindowId(self.next_window_id);
        self.next_window_id += 1;
        id
    }

    /// Make sure future ids are above `id` (after restoring windows).
    pub fn reserve_window_id(&mut self, id: WindowId) {
        self.next_window_id = self.next_window_id.max(id.0 + 1);
    }

    pub fn tab(&self) -> &Tab {
        &self.tabs[self.current_tab]
    }

    pub fn tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.current_tab]
    }

    /// The selected window of the current tab.
    pub fn selected(&self) -> WindowId {
        self.tab().selected
    }

    pub fn window(&self, id: WindowId) -> Option<&Window> {
        self.windows.get(&id)
    }

    /// Every window of every tab showing `buf`.
    pub fn windows_showing(&self, buf: BufferId) -> Vec<WindowId> {
        let mut v: Vec<WindowId> = self.windows.values().filter(|w| w.buffer == buf).map(|w| w.id).collect();
        v.sort();
        v
    }

    /// Keep the points of unselected windows on `buf` in place across an edit
    /// replacing `start..end` with `inserted` chars (the selected window's
    /// cursors are the editor's, adjusted by the edit itself).
    pub fn adjust_for_edit(&mut self, buf: BufferId, start: usize, end: usize, inserted: usize) {
        let selected = self.selected();
        for w in self.windows.values_mut().filter(|w| w.buffer == buf && w.id != selected) {
            for c in &mut w.cursors {
                c.adjust(start, end, inserted);
            }
        }
    }

    /// The cursors of the unselected windows showing `buf` (to put back
    /// with [`restore_cursors`](Self::restore_cursors)).
    pub fn other_windows_cursors(&self, buf: BufferId) -> Vec<(WindowId, Vec<Cursor>)> {
        let selected = self.selected();
        self.windows
            .values()
            .filter(|w| w.buffer == buf && w.id != selected)
            .map(|w| (w.id, w.cursors.clone()))
            .collect()
    }

    /// Put back cursors saved by [`other_windows_cursors`](Self::other_windows_cursors)
    /// in the windows that still show `buf`.
    pub fn restore_cursors(&mut self, buf: BufferId, saved: Vec<(WindowId, Vec<Cursor>)>) {
        for (id, cursors) in saved {
            if let Some(w) = self.windows.get_mut(&id).filter(|w| w.buffer == buf) {
                w.cursors = cursors;
            }
        }
    }

    /// Unselected windows showing the killed buffer `buf` show `replacement`
    /// instead (with point at its start). Returns whether any window changed.
    pub fn replace_buffer(&mut self, buf: BufferId, replacement: BufferId) -> bool {
        let selected = self.selected();
        let mut changed = false;
        for w in self.windows.values_mut().filter(|w| w.buffer == buf && w.id != selected) {
            *w = Window { scroll_line: 0, ..Window::new(w.id, replacement) };
            changed = true;
        }
        changed
    }

    /// The view reports the size of the windows' area in cells (columns,
    /// lines; the tab bar and echo area excluded). Kept whole and at least 1.
    pub fn set_frame_size(&mut self, cols: f32, rows: f32) {
        let whole = |v: f32| if v.is_finite() { v.floor().max(1.0) } else { 1.0 };
        self.frame = (whole(cols), whole(rows));
    }

    /// The last reported frame size in cells (columns, lines).
    pub fn frame_size(&self) -> (f32, f32) {
        self.frame
    }

    /// The windows' area in cells.
    pub fn frame_rect(&self) -> Rect {
        Rect { x: 0.0, y: 0.0, w: self.frame.0, h: self.frame.1 }
    }

    /// The current tab's windows and their rectangles in cells.
    pub fn window_rects(&self) -> Vec<(WindowId, Rect)> {
        compute_rects(&self.tab().root, self.frame_rect())
    }

    /// The tab holding window `id`.
    pub fn tab_of(&self, id: WindowId) -> Option<usize> {
        self.tabs.iter().position(|t| t.root.contains(id))
    }

    /// The current tab's windows in tree order (`other-window` order).
    pub fn window_order(&self) -> Vec<WindowId> {
        self.tab().root.windows()
    }

    /// Check the layout invariants (see the module docs); for tests and for
    /// validating a restored session.
    pub fn check(&self) -> Result<(), String> {
        if self.tabs.is_empty() {
            return Err("no tabs".into());
        }
        if self.current_tab >= self.tabs.len() {
            return Err(format!("current tab {} of {}", self.current_tab, self.tabs.len()));
        }
        let mut leaves = Vec::new();
        for (i, t) in self.tabs.iter().enumerate() {
            t.root.check().map_err(|e| format!("tab {i}: {e}"))?;
            if !t.root.contains(t.selected) {
                return Err(format!("tab {i}: selected window {:?} is not in it", t.selected));
            }
            leaves.extend(t.root.windows());
        }
        leaves.sort();
        let n = leaves.len();
        leaves.dedup();
        if leaves.len() != n {
            return Err("a window is in two tabs".into());
        }
        let mut known: Vec<WindowId> = self.windows.keys().copied().collect();
        known.sort();
        if known != leaves {
            return Err(format!("windows {known:?} but leaves {leaves:?}"));
        }
        if let Some((key, w)) = self.windows.iter().find(|(id, w)| **id != w.id) {
            return Err(format!("window {:?} stored under {key:?}", w.id));
        }
        if leaves.last().is_some_and(|w| w.0 >= self.next_window_id) {
            return Err("window ids not reserved".into());
        }
        Ok(())
    }
}
