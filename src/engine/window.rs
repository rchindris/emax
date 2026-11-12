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
//! SKELETON: the data types and the buffer-list integration are final; the
//! commands and geometry are owned by the windows builder (see
//! docs/design/terminal-substrate.md) and fail with "not implemented yet".

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::buffer::BufferId;
use super::commands::{CmdResult, Command, flags};
use super::cursor::Cursor;
use super::editor::{Args, Editor};

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
}

impl Layout {
    /// One tab with one window showing `buffer`.
    pub fn new(buffer: BufferId) -> Self {
        let id = WindowId(1);
        let mut windows = HashMap::new();
        windows.insert(id, Window::new(id, buffer));
        Self { tabs: vec![Tab { name: None, root: Node::Leaf(id), selected: id }], current_tab: 0, windows, next_window_id: 2 }
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

    /// Unselected windows showing the killed buffer `buf` show `replacement`
    /// instead (with point at its start).
    pub fn replace_buffer(&mut self, buf: BufferId, replacement: BufferId) {
        let selected = self.selected();
        for w in self.windows.values_mut().filter(|w| w.buffer == buf && w.id != selected) {
            *w = Window { scroll_line: 0, ..Window::new(w.id, replacement) };
        }
    }
}

/// The window rectangles of `root` laid out in `area`, in tree order.
/// SKELETON: the windows builder implements this (every window gets `area` for now).
pub fn compute_rects(root: &Node, area: Rect) -> Vec<(WindowId, Rect)> {
    root.windows().into_iter().map(|w| (w, area)).collect()
}

fn not_yet(name: &str) -> CmdResult {
    Err(format!("{name}: not implemented yet"))
}

macro_rules! stub_commands {
    ($( $name:literal => $f:ident, $doc:literal; )*) => {
        $( fn $f(_: &mut Editor, _: &Args) -> CmdResult { not_yet($name) } )*
        /// Window and tab commands (looked up by `commands::lookup`).
        pub static COMMANDS: &[Command] = &[
            $( Command { name: $name, run: $f, flags: flags::NO_UNDO, doc: $doc }, )*
        ];
    };
}

stub_commands! {
    "split-window-below" => split_window_below, "Split the selected window into two, one above the other.";
    "split-window-right" => split_window_right, "Split the selected window into two side by side.";
    "delete-window" => delete_window, "Delete the selected window.";
    "delete-other-windows" => delete_other_windows, "Make the selected window fill its tab.";
    "other-window" => other_window, "Select the next window (with N: the Nth next; negative goes back).";
    "balance-windows" => balance_windows, "Make all windows in the tab the same size.";
    "enlarge-window" => enlarge_window, "Make the selected window N lines taller.";
    "shrink-window" => shrink_window, "Make the selected window N lines shorter.";
    "enlarge-window-horizontally" => enlarge_window_horizontally, "Make the selected window N columns wider.";
    "shrink-window-horizontally" => shrink_window_horizontally, "Make the selected window N columns narrower.";
    "windmove-left" => windmove_left, "Select the window to the left.";
    "windmove-right" => windmove_right, "Select the window to the right.";
    "windmove-up" => windmove_up, "Select the window above.";
    "windmove-down" => windmove_down, "Select the window below.";
    "tab-new" => tab_new, "Add a tab showing the current buffer, and select it.";
    "tab-close" => tab_close, "Close the current tab.";
    "tab-next" => tab_next, "Select the next tab (with N: the Nth next).";
    "tab-previous" => tab_previous, "Select the previous tab.";
    "tab-rename" => tab_rename, "Name the current tab (with no name: named after its buffer again).";
}

impl Editor {
    /// Record that the selected window now shows the current buffer (called
    /// whenever the current buffer changes).
    pub(crate) fn sync_selected_window(&mut self) {
        let sel = self.layout.selected();
        let buf = self.buf_id;
        if let Some(w) = self.layout.windows.get_mut(&sel) {
            w.buffer = buf;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_layout_and_ids() {
        let mut l = Layout::new(7);
        assert_eq!(l.selected(), WindowId(1));
        assert_eq!(l.tab().root.windows(), [WindowId(1)]);
        assert_eq!(l.windows_showing(7), [WindowId(1)]);
        assert_eq!(l.alloc_window_id(), WindowId(2));
        l.reserve_window_id(WindowId(10));
        assert_eq!(l.alloc_window_id(), WindowId(11));
    }

    #[test]
    fn unselected_windows_follow_edits_and_kills() {
        let mut l = Layout::new(1);
        let other = l.alloc_window_id();
        let mut w = Window::new(other, 1);
        w.cursors[0].point = 10;
        l.windows.insert(other, w);
        l.adjust_for_edit(1, 2, 4, 5); // replace 2 chars by 5 before point
        assert_eq!(l.window(other).unwrap().cursors[0].point, 13);
        l.adjust_for_edit(2, 0, 0, 100); // another buffer: untouched
        assert_eq!(l.window(other).unwrap().cursors[0].point, 13);
        l.replace_buffer(1, 5);
        assert_eq!(l.window(other).unwrap().buffer, 5);
        assert_eq!(l.window(other).unwrap().cursors[0].point, 0);
        assert_eq!(l.window(WindowId(1)).unwrap().buffer, 1); // selected: the editor handles it
    }

    #[test]
    fn editor_keeps_the_selected_window_in_sync() {
        use crate::engine::Buffer;
        let mut ed = Editor::new(Buffer::from_str("a", "0123456789"));
        let b = ed.add_buffer(Buffer::from_str("b", ""));
        let sel = ed.layout.selected();
        assert_eq!(ed.layout.window(sel).unwrap().buffer, 1);
        ed.switch_to_buffer(b);
        assert_eq!(ed.layout.window(sel).unwrap().buffer, b);
        ed.switch_to_buffer(1);

        // An unselected window on the same buffer keeps its point across edits.
        let other = ed.layout.alloc_window_id();
        let mut w = Window::new(other, 1);
        w.cursors[0].point = 8;
        ed.layout.windows.insert(other, w);
        ed.replace(0..0, "abc");
        assert_eq!(ed.layout.window(other).unwrap().cursors[0].point, 11);
        // Killing its buffer shows the replacement there too.
        ed.kill_buffer(1);
        assert_eq!(ed.buf_id, b);
        assert_eq!(ed.layout.window(other).unwrap().buffer, b);
        assert_eq!(ed.layout.window(sel).unwrap().buffer, b);
    }

    #[test]
    fn node_serializes_as_plain_data() {
        let n = Node::Split {
            dir: SplitDir::Horizontal,
            children: vec![Node::Leaf(WindowId(1)), Node::Leaf(WindowId(2))],
            ratios: vec![0.5, 0.5],
        };
        let j = serde_json::to_value(&n).unwrap();
        assert_eq!(j["split"]["dir"], "horizontal");
        assert_eq!(serde_json::from_value::<Node>(j).unwrap(), n);
        assert_eq!(n.windows(), [WindowId(1), WindowId(2)]);
    }
}
