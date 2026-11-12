//! The editor side of windows and tabs: selecting a window swaps its cursors
//! with the editor's live ones (and switches buffer), tabs save and restore
//! their selected window the same way, and the window and tab commands.
//!
//! Every change to the layout queues one `WindowConfigurationChanged` event.

use super::super::buffers::BufView;
use super::super::commands::{CmdResult, Command, flags};
use super::super::cursor::Cursor;
use super::super::editor::{Args, Editor, Scroll};
use super::super::hooks::Event;
use super::tree::{self, Direction, ResizeError};
use super::{Node, SplitDir, Tab, Window, WindowId};

impl Editor {
    /// Record that the selected window now shows the current buffer (called
    /// whenever the current buffer changes). A window that changes buffer
    /// starts at the top of the new one.
    pub(crate) fn sync_selected_window(&mut self) {
        if self.sync_selected_window_quietly() {
            self.window_changed();
        }
    }

    /// [`sync_selected_window`](Self::sync_selected_window) without the
    /// event, for callers that make several changes and report them once.
    /// Returns whether the window changed.
    pub(crate) fn sync_selected_window_quietly(&mut self) -> bool {
        let sel = self.layout.selected();
        let buf = self.buf_id;
        match self.layout.windows.get_mut(&sel) {
            Some(w) if w.buffer != buf => {
                w.buffer = buf;
                w.scroll_line = 0;
                true
            }
            _ => false,
        }
    }

    fn window_changed(&mut self) {
        self.emit(Event::WindowConfigurationChanged);
    }

    /// Save the editor's live cursors into the selected window.
    fn store_selected(&mut self) {
        let sel = self.layout.selected();
        let (buf, primary, next) = (self.buf_id, self.primary, self.next_cursor_id);
        let cursors = self.cursors.clone();
        if let Some(w) = self.layout.windows.get_mut(&sel) {
            w.buffer = buf;
            w.cursors = cursors;
            w.primary = primary;
            w.next_cursor_id = next;
        }
    }

    /// Make the (newly) selected window's buffer current and its cursors live.
    fn install_selected(&mut self) {
        let sel = self.layout.selected();
        let Some(w) = self.layout.windows.get(&sel).cloned() else { return };
        if !self.switch_to_buffer(w.buffer) {
            // Its buffer is gone (killing a buffer normally replaces it in
            // every window): show the current buffer instead.
            self.sync_selected_window();
            return;
        }
        let len = self.len();
        let mut cursors: Vec<Cursor> = w.cursors.into_iter().map(|c| clamp_cursor(c, len)).collect();
        if cursors.is_empty() {
            cursors.push(Cursor::new(w.primary, 0));
        }
        let max_id = cursors.iter().map(|c| c.id).max().unwrap_or(0);
        self.cursors = cursors;
        self.primary = w.primary;
        self.next_cursor_id = w.next_cursor_id.max(max_id + 1);
        self.normalize_cursors();
        self.scroll = Scroll::EnsureVisible;
    }

    /// Select window `id` (in any tab; its tab becomes current): its buffer
    /// becomes current with its own point and cursors. Returns false if there
    /// is no such window.
    pub fn select_window(&mut self, id: WindowId) -> bool {
        let Some(tab) = self.layout.tab_of(id) else { return false };
        if tab == self.layout.current_tab && id == self.layout.selected() {
            return true;
        }
        self.store_selected();
        self.layout.current_tab = tab;
        self.layout.tab_mut().selected = id;
        self.install_selected();
        self.window_changed();
        true
    }

    /// Split the selected window along `dir`. The new window (below or to
    /// the right) shows the same buffer with a copy of the cursors; the
    /// selected window stays selected.
    pub fn split_window(&mut self, dir: SplitDir) -> Result<WindowId, String> {
        let sel = self.layout.selected();
        let rect =
            tree::window_rect(&self.layout.tab().root, self.layout.frame_rect(), sel).ok_or("No selected window")?;
        if dir.extent(rect) < 2.0 * dir.min_extent() {
            return Err("Window too small for splitting".into());
        }
        let id = self.layout.alloc_window_id();
        let scroll_line = self.layout.window(sel).map_or(0, |w| w.scroll_line);
        let w = Window {
            id,
            buffer: self.buf_id,
            cursors: self.cursors.clone(),
            primary: self.primary,
            next_cursor_id: self.next_cursor_id,
            scroll_line,
        };
        self.layout.windows.insert(id, w);
        self.layout.tab_mut().root.split_window(sel, id, dir);
        self.window_changed();
        Ok(id)
    }

    /// Delete window `id` (in any tab). Its space goes to a neighbour, which
    /// is selected instead if `id` was. The sole window of a tab can't go.
    pub fn delete_window(&mut self, id: WindowId) -> CmdResult {
        let tab = self.layout.tab_of(id).ok_or("No such window")?;
        let next = self.layout.tabs[tab]
            .root
            .remove_window(id)
            .ok_or("Attempt to delete minibuffer or sole ordinary window")?;
        self.layout.windows.remove(&id);
        if self.layout.tabs[tab].selected == id {
            self.layout.tabs[tab].selected = next;
            if tab == self.layout.current_tab {
                // The deleted window's cursors die with it.
                self.install_selected();
            }
        }
        self.window_changed();
        Ok(())
    }

    /// Make the selected window the only one in its tab.
    pub fn delete_other_windows(&mut self) {
        let sel = self.layout.selected();
        let others: Vec<WindowId> = self.layout.window_order().into_iter().filter(|w| *w != sel).collect();
        if others.is_empty() {
            return;
        }
        for w in &others {
            self.layout.windows.remove(w);
        }
        self.layout.tab_mut().root = Node::Leaf(sel);
        self.window_changed();
    }

    /// Select the `n`th next window of the current tab in tree order
    /// (negative goes back), wrapping around.
    pub fn other_window(&mut self, n: i64) {
        let order = self.layout.window_order();
        let len = order.len() as i64;
        let Some(i) = order.iter().position(|w| *w == self.layout.selected()) else { return };
        let target = order[(i as i64 + n).rem_euclid(len) as usize];
        self.select_window(target);
    }

    /// Give every window of the current tab the same size.
    pub fn balance_windows(&mut self) {
        self.layout.tab_mut().root.balance();
        self.window_changed();
    }

    /// Make the selected window `delta` cells bigger along `dir` (smaller if
    /// negative), as far as the minimum window sizes allow.
    pub fn resize_window(&mut self, dir: SplitDir, delta: i64) -> CmdResult {
        let (sel, area) = (self.layout.selected(), self.layout.frame_rect());
        match tree::resize_window(&mut self.layout.tab_mut().root, area, sel, dir, delta) {
            Ok(0) => Ok(()),
            Ok(_) => {
                self.window_changed();
                Ok(())
            }
            Err(ResizeError::CannotShrink) => Err("Cannot shrink selected window".into()),
            Err(ResizeError::NoSplit) if delta < 0 => Err("Cannot shrink selected window".into()),
            Err(_) => Err("Cannot enlarge selected window".into()),
        }
    }

    /// Select the window on screen next to the selected one in direction
    /// `towards` (the one at the cursor's row or column if several are).
    pub fn windmove(&mut self, towards: Direction) -> CmdResult {
        let (sel, area) = (self.layout.selected(), self.layout.frame_rect());
        let root = &self.layout.tab().root;
        let r = tree::window_rect(root, area, sel).ok_or("No selected window")?;
        let (line, col) = self.primary_line_col();
        let scroll_line = self.layout.window(sel).map_or(0, |w| w.scroll_line);
        // The cursor's cell on screen (text rows exclude the mode line).
        let reference = match towards {
            Direction::Left | Direction::Right => {
                r.y + (line.saturating_sub(scroll_line) as f32).min((r.h - 2.0).max(0.0)) + 0.5
            }
            Direction::Up | Direction::Down => r.x + (col as f32).min((r.w - 1.0).max(0.0)) + 0.5,
        };
        let target = tree::neighbour(root, area, sel, towards, reference)
            .ok_or_else(|| format!("No window {} from selected window", towards.name()))?;
        self.select_window(target);
        Ok(())
    }

    /// The selected window's buffer and point if `id` is selected (with the
    /// picker's live preview), else window `id`'s buffer with its own cursors.
    pub fn window_view(&self, id: WindowId) -> Option<BufView<'_>> {
        if id == self.layout.selected() {
            return Some(self.display_view());
        }
        let w = self.layout.window(id)?;
        let b = self.find_buffer(w.buffer)?;
        Some(BufView { id: w.buffer, buf: b.buf, cursors: &w.cursors, primary: w.primary })
    }

    /// Window `id`'s point (the live one for the selected window).
    pub fn window_point(&self, id: WindowId) -> Option<usize> {
        if id == self.layout.selected() {
            return Some(self.primary_cursor().point);
        }
        let w = self.layout.window(id)?;
        w.cursors.iter().find(|c| c.id == w.primary).or(w.cursors.first()).map(|c| c.point)
    }

    /// The view reports window `id`'s first visible line.
    pub fn set_window_scroll(&mut self, id: WindowId, line: usize) {
        if let Some(w) = self.layout.windows.get_mut(&id) {
            w.scroll_line = line;
        }
    }

    /// The name of buffer `id`, if it exists.
    pub fn buffer_name(&self, id: u64) -> Option<&str> {
        self.find_buffer(id).map(|v| v.buf.name.as_str())
    }

    // ------------------------------------------------------------------ tabs

    /// Make tab `i` current, restoring its selected window. False if out of range.
    pub fn select_tab(&mut self, i: usize) -> bool {
        if i >= self.layout.tabs.len() {
            return false;
        }
        if i == self.layout.current_tab {
            return true;
        }
        self.store_selected();
        self.layout.current_tab = i;
        self.install_selected();
        self.window_changed();
        true
    }

    /// Add a tab after the current one with one window on the current buffer
    /// (same point), and make it current. Returns its index.
    pub fn new_tab(&mut self) -> usize {
        self.store_selected();
        let id = self.layout.alloc_window_id();
        let sel = self.layout.selected();
        let w = Window { id, ..self.layout.windows[&sel].clone() };
        self.layout.windows.insert(id, w);
        let i = self.layout.current_tab + 1;
        self.layout.tabs.insert(i, Tab { name: None, root: Node::Leaf(id), selected: id });
        self.layout.current_tab = i;
        self.window_changed();
        i
    }

    /// Close tab `i`; its windows go, their buffers stay. Closing the current
    /// tab selects the one after it (or before, if it was last).
    pub fn close_tab(&mut self, i: usize) -> CmdResult {
        let len = self.layout.tabs.len();
        if i >= len {
            return Err(format!("No tab {}", i + 1));
        }
        if len == 1 {
            return Err("Attempt to delete the sole tab".into());
        }
        let tab = self.layout.tabs.remove(i);
        for w in tab.root.windows() {
            self.layout.windows.remove(&w);
        }
        let cur = self.layout.current_tab;
        if i < cur {
            self.layout.current_tab = cur - 1;
        } else if i == cur {
            self.layout.current_tab = i.min(len - 2);
            self.install_selected();
        }
        self.window_changed();
        Ok(())
    }

    /// Select the `n`th next tab (negative goes back), wrapping around.
    pub fn next_tab(&mut self, n: i64) {
        let len = self.layout.tabs.len() as i64;
        let i = (self.layout.current_tab as i64 + n).rem_euclid(len) as usize;
        self.select_tab(i);
    }

    /// Name tab `i`; `None` (or a blank name) goes back to showing its buffer's name.
    pub fn rename_tab(&mut self, i: usize, name: Option<&str>) -> bool {
        let Some(tab) = self.layout.tabs.get_mut(i) else { return false };
        tab.name = name.map(str::trim).filter(|n| !n.is_empty()).map(String::from);
        self.window_changed();
        true
    }

    /// What tab `i` is called: its explicit name, else its selected window's buffer.
    pub fn tab_name(&self, i: usize) -> String {
        let Some(tab) = self.layout.tabs.get(i) else { return String::new() };
        if let Some(n) = &tab.name {
            return n.clone();
        }
        let buf = if i == self.layout.current_tab {
            Some(self.buf_id)
        } else {
            self.layout.window(tab.selected).map(|w| w.buffer)
        };
        buf.and_then(|b| self.buffer_name(b)).unwrap_or_default().to_string()
    }

    /// Every tab's name, in order (for the tab bar).
    pub fn tab_names(&self) -> Vec<String> {
        (0..self.layout.tabs.len()).map(|i| self.tab_name(i)).collect()
    }
}

/// `c` with every position kept within a buffer of `len` chars.
fn clamp_cursor(mut c: Cursor, len: usize) -> Cursor {
    c.point = c.point.min(len);
    c.mark = c.mark.map(|m| m.min(len));
    c.mark_ring.iter_mut().for_each(|m| *m = (*m).min(len));
    c
}

// -------------------------------------------------------------- commands

fn split_window_below(ed: &mut Editor, _: &Args) -> CmdResult {
    ed.split_window(SplitDir::Vertical).map(|_| ())
}

fn split_window_right(ed: &mut Editor, _: &Args) -> CmdResult {
    ed.split_window(SplitDir::Horizontal).map(|_| ())
}

fn delete_window(ed: &mut Editor, _: &Args) -> CmdResult {
    let sel = ed.layout.selected();
    ed.delete_window(sel)
}

fn delete_other_windows(ed: &mut Editor, _: &Args) -> CmdResult {
    ed.delete_other_windows();
    Ok(())
}

fn other_window(ed: &mut Editor, a: &Args) -> CmdResult {
    ed.other_window(a.n());
    Ok(())
}

fn balance_windows(ed: &mut Editor, _: &Args) -> CmdResult {
    ed.balance_windows();
    Ok(())
}

fn enlarge_window(ed: &mut Editor, a: &Args) -> CmdResult {
    ed.resize_window(SplitDir::Vertical, a.n())
}

fn shrink_window(ed: &mut Editor, a: &Args) -> CmdResult {
    ed.resize_window(SplitDir::Vertical, -a.n())
}

fn enlarge_window_horizontally(ed: &mut Editor, a: &Args) -> CmdResult {
    ed.resize_window(SplitDir::Horizontal, a.n())
}

fn shrink_window_horizontally(ed: &mut Editor, a: &Args) -> CmdResult {
    ed.resize_window(SplitDir::Horizontal, -a.n())
}

fn windmove_left(ed: &mut Editor, _: &Args) -> CmdResult {
    ed.windmove(Direction::Left)
}

fn windmove_right(ed: &mut Editor, _: &Args) -> CmdResult {
    ed.windmove(Direction::Right)
}

fn windmove_up(ed: &mut Editor, _: &Args) -> CmdResult {
    ed.windmove(Direction::Up)
}

fn windmove_down(ed: &mut Editor, _: &Args) -> CmdResult {
    ed.windmove(Direction::Down)
}

fn tab_new(ed: &mut Editor, _: &Args) -> CmdResult {
    ed.new_tab();
    Ok(())
}

fn tab_close(ed: &mut Editor, _: &Args) -> CmdResult {
    let i = ed.layout.current_tab;
    ed.close_tab(i)
}

fn tab_next(ed: &mut Editor, a: &Args) -> CmdResult {
    ed.next_tab(a.n());
    Ok(())
}

fn tab_previous(ed: &mut Editor, a: &Args) -> CmdResult {
    ed.next_tab(-a.n());
    Ok(())
}

fn tab_rename(ed: &mut Editor, a: &Args) -> CmdResult {
    let i = ed.layout.current_tab;
    ed.rename_tab(i, a.text.as_deref());
    Ok(())
}

macro_rules! window_commands {
    ($( $name:literal => $f:ident, $doc:literal; )*) => {
        /// Window and tab commands (looked up by `commands::lookup`).
        pub static COMMANDS: &[Command] = &[
            $( Command { name: $name, run: $f, flags: flags::NO_UNDO, doc: $doc }, )*
        ];
    };
}

window_commands! {
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
