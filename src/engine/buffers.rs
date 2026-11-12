//! Multiple buffers: the current one lives in `Editor.buf`/`cursors`; the
//! others are parked in `Editor.others` in most-recently-used order, each with
//! its own cursors. Also the picker's actions, for switch-to-buffer and M-x.

use super::buffer::Buffer;
use super::commands;
use super::cursor::Cursor;
use super::editor::{Args, Editor, RawArg, Scroll};
use super::hooks::Event;
use super::keymap::{Key, Keymap, where_is};
use super::picker::{Candidate, Picker, PickerKind};

/// What the picker keymap can bind keys to.
pub const PICKER_ACTIONS: &[&str] = &[
    "picker-quit",
    "picker-accept",
    "picker-accept-input",
    "picker-next",
    "picker-previous",
    "picker-page-down",
    "picker-page-up",
    "picker-first",
    "picker-last",
    "picker-delete-char",
    "picker-delete-word",
    "picker-kill-buffer",
];

pub struct BufferSlot {
    pub id: u64,
    pub buf: Buffer,
    pub cursors: Vec<Cursor>,
    pub primary: u64,
    pub next_cursor_id: u64,
}

/// Read-only view of a buffer and its cursors, for rendering.
pub struct BufView<'a> {
    pub id: u64,
    pub buf: &'a Buffer,
    pub cursors: &'a [Cursor],
    pub primary: u64,
}

impl Editor {
    pub fn buffer_count(&self) -> usize {
        1 + self.others.len()
    }

    fn name_taken(&self, name: &str) -> bool {
        self.buf.name == name || self.others.iter().any(|s| s.buf.name == name)
    }

    fn unique_name(&self, base: &str) -> String {
        if !self.name_taken(base) {
            return base.to_string();
        }
        (2..).map(|n| format!("{base}<{n}>")).find(|n| !self.name_taken(n)).unwrap()
    }

    /// Add a buffer behind the current one (it becomes the "other" buffer). Returns its id.
    pub fn add_buffer(&mut self, mut buf: Buffer) -> u64 {
        buf.name = self.unique_name(&buf.name);
        buf.modes = self.modes.initial(&buf);
        let id = self.next_buf_id;
        self.next_buf_id += 1;
        self.others.insert(0, BufferSlot { id, buf, cursors: vec![Cursor::new(1, 0)], primary: 1, next_cursor_id: 2 });
        self.emit(Event::BufferCreated { buf: id });
        id
    }

    fn park_current(&mut self, replacement: BufferSlot) -> BufferSlot {
        let old = BufferSlot {
            id: self.buf_id,
            buf: std::mem::replace(&mut self.buf, replacement.buf),
            cursors: std::mem::replace(&mut self.cursors, replacement.cursors),
            primary: self.primary,
            next_cursor_id: self.next_cursor_id,
        };
        self.buf_id = replacement.id;
        self.primary = replacement.primary;
        self.next_cursor_id = replacement.next_cursor_id;
        old
    }

    /// Make buffer `id` current. With `bury`, the old current buffer goes to the
    /// end of the list (so repeated previous-buffer cycles); otherwise to the front.
    pub fn switch_to_buffer_ex(&mut self, id: u64, bury: bool) -> bool {
        match self.switch_quietly(id, bury) {
            Some(true) => {
                self.emit(Event::WindowConfigurationChanged);
                true
            }
            Some(false) => true,
            None => false,
        }
    }

    /// Make buffer `id` current without reporting the window change: `None`
    /// if there is no such buffer, else whether the selected window changed.
    fn switch_quietly(&mut self, id: u64, bury: bool) -> Option<bool> {
        if id == self.buf_id {
            return Some(false);
        }
        let i = self.others.iter().position(|s| s.id == id)?;
        let slot = self.others.remove(i);
        let old = self.park_current(slot);
        if bury {
            self.others.push(old);
        } else {
            self.others.insert(0, old);
        }
        self.scroll = Scroll::Keep;
        self.last_command = "";
        Some(self.sync_selected_window_quietly())
    }

    pub fn switch_to_buffer(&mut self, id: u64) -> bool {
        self.switch_to_buffer_ex(id, false)
    }

    /// Switch to the buffer called `name`, creating an empty one if needed.
    pub fn switch_to_buffer_named(&mut self, name: &str) {
        if self.buf.name == name {
            return;
        }
        let id = match self.others.iter().find(|s| s.buf.name == name) {
            Some(s) => s.id,
            None => {
                let id = self.add_buffer(Buffer::from_str(name, ""));
                self.message = Some(format!("New buffer {name}"));
                id
            }
        };
        self.switch_to_buffer(id);
    }

    /// Kill buffer `id`. Killing the current buffer shows the most recent other
    /// one (or a fresh *scratch* if it was the last).
    ///
    /// Windows showing it show the buffer that replaces it in the selected
    /// window; a terminal buffer's process is killed; `BufferKilled` is emitted.
    pub fn kill_buffer(&mut self, id: u64) {
        let name = if id == self.buf_id {
            self.buf.name.clone()
        } else {
            match self.others.iter().find(|s| s.id == id) {
                Some(s) => s.buf.name.clone(),
                None => return,
            }
        };
        // Every window that showed it changes: one event for all of them.
        let mut changed = false;
        if id == self.buf_id {
            if self.others.is_empty() {
                self.add_buffer(Buffer::from_str("*scratch*", ""));
            }
            let next = self.others[0].id;
            changed |= self.switch_quietly(next, false).unwrap_or(false);
        }
        self.others.retain(|s| s.id != id);
        let replacement = self.buf_id;
        changed |= self.layout.replace_buffer(id, replacement);
        if changed {
            self.emit(Event::WindowConfigurationChanged);
        }
        self.terminal_buffer_killed(id);
        self.emit(Event::BufferKilled { buf: id, name });
    }

    pub fn find_buffer(&self, id: u64) -> Option<BufView<'_>> {
        if id == self.buf_id {
            return Some(self.current_view());
        }
        self.others.iter().find(|s| s.id == id).map(|s| BufView {
            id: s.id,
            buf: &s.buf,
            cursors: &s.cursors,
            primary: s.primary,
        })
    }

    pub fn current_view(&self) -> BufView<'_> {
        BufView { id: self.buf_id, buf: &self.buf, cursors: &self.cursors, primary: self.primary }
    }

    /// The buffer to display: the picker's selection (live preview) or the current one.
    pub fn display_view(&self) -> BufView<'_> {
        self.picker
            .as_ref()
            .filter(|p| p.kind == PickerKind::Buffers)
            .and_then(|p| p.selected_candidate())
            .and_then(|c| self.find_buffer(c.id))
            .unwrap_or_else(|| self.current_view())
    }

    // ---------------------------------------------------------- the picker

    fn buffer_candidates(&self) -> Vec<Candidate> {
        let home = std::env::var("HOME").unwrap_or_default();
        let cand = |id: u64, b: &Buffer, current: bool| {
            let detail = b
                .path
                .as_ref()
                .and_then(|p| p.parent())
                .map(|d| {
                    let d = d.display().to_string();
                    match d.strip_prefix(&home) {
                        Some(rest) if !home.is_empty() => format!("~{rest}"),
                        _ => d,
                    }
                })
                .unwrap_or_default();
            let lines = b.rope.len_lines();
            Candidate {
                id,
                label: b.name.clone(),
                detail,
                annotation: format!("{} line{}", group_digits(lines), if lines == 1 { "" } else { "s" }),
                modified: b.modified() && b.path.is_some(),
                current,
            }
        };
        // Most recent other buffer first; the current buffer last (like Emacs' default).
        let mut v: Vec<Candidate> = self.others.iter().map(|s| cand(s.id, &s.buf, false)).collect();
        v.push(cand(self.buf_id, &self.buf, true));
        v
    }

    pub fn open_buffer_picker(&mut self) {
        self.picker = Some(Picker::new("Switch to buffer", self.buffer_candidates()));
    }

    // ------------------------------------------------------------- M-x

    /// Every command M-x offers: the ones used from M-x before (most recent
    /// first), then the rest alphabetically.
    fn command_names(&self) -> Vec<&'static str> {
        let mut all: Vec<&'static str> = commands::all()
            .map(|c| c.name)
            .filter(|n| !matches!(*n, "self-insert-command" | "execute-extended-command"))
            .chain(self.modes.major_names())
            .collect();
        all.sort_unstable();
        let mut v: Vec<&'static str> = self.command_history.iter().copied().filter(|n| all.contains(n)).collect();
        v.extend(all.into_iter().filter(|n| !self.command_history.contains(n)));
        v
    }

    /// Open M-x: pick a command to run with `args` (the prefix argument typed before M-x).
    pub fn open_command_picker(&mut self, args: Args) {
        let names = self.command_names();
        let maps = self.active_keymaps();
        let candidates = names
            .iter()
            .enumerate()
            .map(|(i, &name)| {
                let doc = commands::lookup(name).or_else(|| self.modes.command(name)).map_or("", |c| c.doc);
                Candidate {
                    id: i as u64,
                    label: name.to_string(),
                    detail: doc.to_string(),
                    annotation: where_is(&maps, name).unwrap_or_default(),
                    modified: false,
                    current: name == self.buf.modes.major,
                }
            })
            .collect();
        let prompt = match args.raw {
            None => "M-x".to_string(),
            Some(RawArg::Universal(k)) => format!("{} M-x", vec!["C-u"; k as usize].join(" ")),
            Some(RawArg::Number(n)) => format!("C-u {n} M-x"),
            Some(RawArg::Minus) => "C-u - M-x".to_string(),
        };
        self.picker = Some(Picker::new(&prompt, candidates).with_kind(PickerKind::Commands { names, args }));
    }

    /// Close M-x and hand `name` to the view to run.
    fn run_from_picker(&mut self, name: &'static str) {
        let Some(Picker { kind: PickerKind::Commands { args, .. }, .. }) = self.picker.take() else { return };
        self.command_history.retain(|n| *n != name);
        self.command_history.insert(0, name);
        self.pending_command = Some((name, args));
    }

    /// The command named exactly like the picker's query, if any.
    fn typed_command(p: &Picker) -> Option<&'static str> {
        let PickerKind::Commands { names, .. } = &p.kind else { return None };
        names.iter().copied().find(|n| *n == p.query.trim())
    }

    /// Accept match `i` of the open picker (mouse click / RET).
    pub fn picker_accept(&mut self, i: usize) {
        let Some(p) = self.picker.as_ref() else { return };
        let Some(id) = p.matches.get(i).map(|m| p.candidates[m.index].id) else { return };
        match &p.kind {
            PickerKind::Buffers => {
                self.picker = None;
                self.switch_to_buffer(id);
            }
            PickerKind::Commands { names, .. } => {
                let name = names[id as usize];
                self.run_from_picker(name);
            }
        }
    }

    /// Handle a key while the picker is open: run the action `keymap` binds
    /// it to, or type it into the query.
    pub fn picker_key(&mut self, keymap: &Keymap, key: &Key) {
        match keymap.lookup(&[key.canonical()]) {
            Some(action) => self.picker_action(action),
            None => {
                if let Some(t) = key.insertable_text() {
                    let t = t.to_string();
                    self.picker_insert(&t);
                }
            }
        }
    }

    pub fn picker_insert(&mut self, text: &str) {
        let Some(p) = self.picker.as_mut() else { return };
        p.confirm_kill = None;
        p.message = None;
        p.push_str(text);
    }

    /// Run a picker action (one of `PICKER_ACTIONS`).
    pub fn picker_action(&mut self, action: &str) {
        let Some(p) = self.picker.as_mut() else { return };
        if action != "picker-kill-buffer" {
            p.confirm_kill = None;
        }
        p.message = None;
        if let PickerKind::Commands { .. } = p.kind {
            match action {
                "picker-accept" if p.selected_candidate().is_some() => {
                    let i = p.selected;
                    return self.picker_accept(i);
                }
                "picker-accept" | "picker-accept-input" => {
                    match Self::typed_command(p) {
                        Some(name) => self.run_from_picker(name),
                        None => p.message = Some("No match".into()),
                    }
                    return;
                }
                "picker-kill-buffer" => return,
                _ => {}
            }
        }
        match action {
            "picker-quit" => {
                self.picker = None;
                self.message = Some("Quit".into());
            }
            "picker-accept" => {
                if p.selected_candidate().is_some() {
                    let i = p.selected;
                    self.picker_accept(i);
                } else if !p.query.trim().is_empty() {
                    let name = p.query.trim().to_string();
                    self.picker = None;
                    self.switch_to_buffer_named(&name);
                }
            }
            // Use the input literally, even if it fuzzy-matches something (like vertico's M-RET).
            "picker-accept-input" => {
                let name = p.query.trim().to_string();
                if !name.is_empty() {
                    self.picker = None;
                    self.switch_to_buffer_named(&name);
                }
            }
            "picker-next" => p.move_by(1),
            "picker-previous" => p.move_by(-1),
            "picker-page-down" => p.move_clamped(10),
            "picker-page-up" => p.move_clamped(-10),
            "picker-first" => p.selected = 0,
            "picker-last" => p.selected = p.matches.len().saturating_sub(1),
            "picker-delete-char" => p.delete_char(),
            "picker-delete-word" => p.delete_word(),
            "picker-kill-buffer" => {
                let Some(c) = p.selected_candidate().cloned() else { return };
                if c.modified && p.confirm_kill != Some(c.id) {
                    p.confirm_kill = Some(c.id);
                    p.message = Some(format!("{} has unsaved changes — kill again to confirm", c.label));
                    return;
                }
                self.kill_buffer(c.id);
                let cands = self.buffer_candidates();
                if let Some(p) = self.picker.as_mut() {
                    p.confirm_kill = None;
                    p.set_candidates(cands);
                    p.message = Some(format!("Killed {}", c.label));
                }
            }
            _ => {}
        }
    }
}

fn group_digits(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}
