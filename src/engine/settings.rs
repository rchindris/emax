//! The settings editor (`open-settings`, `s-,`): a searchable list of the
//! configuration's settings with a control for each, like VS Code's settings
//! UI. It shows each setting's effective value (the user file's, else the
//! built-in default), marks the ones the user file sets, and writes changes
//! straight to the user file — only the keys the user changed, so the file
//! stays a small diff on top of the defaults. The view reloads the
//! configuration after each write so changes apply at once.
//!
//! Keymaps, modes and theme colors have no controls here: `settings-open-json`
//! opens the file itself for those.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::config::{DEFAULT_JSON, user_config_path};
use super::editor::Editor;
use super::files::abbreviate;
use super::keymap::{Key, Keymap};
use super::line_edit::{self, FieldEdit};

/// What the settings keymap can bind keys to.
pub const SETTINGS_ACTIONS: &[&str] = &[
    "settings-quit",
    "settings-close",
    "settings-accept",
    "settings-next",
    "settings-previous",
    "settings-next-section",
    "settings-previous-section",
    "settings-page-down",
    "settings-page-up",
    "settings-first",
    "settings-last",
    "settings-delete-char",
    "settings-delete-word",
    "settings-clear",
    "settings-reset",
    "settings-open-json",
];

/// The search term that keeps only settings the user file sets.
pub const MODIFIED_FILTER: &str = "@modified";

/// Rows moved by a page key.
const PAGE: usize = 6;

/// The sections, in the order the table of contents lists them.
pub const SECTIONS: &[&str] = &["Text Editor", "Font", "Appearance", "Terminal", "Session"];

/// What kind of value a setting holds, and so which control edits it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Bool,
    Int {
        min: i64,
        max: i64,
    },
    Float {
        min: f64,
        max: f64,
    },
    /// A string; when `nullable`, an empty input writes `null` (the default
    /// behaviour that `placeholder` describes).
    Text {
        nullable: bool,
        placeholder: &'static str,
    },
    /// One of a fixed set of strings.
    Choice(&'static [&'static str]),
    /// A theme name (the built-in themes and the user's).
    Theme,
    /// A major mode name (the built-in modes and the user's).
    MajorMode,
    /// A list of strings, edited comma-separated.
    List,
}

impl Kind {
    /// Whether the value is picked from a list of options.
    pub fn is_choice(&self) -> bool {
        matches!(self, Kind::Choice(_) | Kind::Theme | Kind::MajorMode)
    }
}

pub struct Setting {
    /// The dotted path in the configuration file, e.g. `editor.fill_column`.
    pub key: &'static str,
    pub section: &'static str,
    /// `Group: Name`; the view shows the group dimmed.
    pub title: &'static str,
    pub doc: &'static str,
    pub kind: Kind,
}

macro_rules! settings {
    ($( $key:literal, $section:literal, $title:literal, $kind:expr, $doc:literal; )*) => {
        pub static SETTINGS: &[Setting] = &[
            $( Setting { key: $key, section: $section, title: $title, kind: $kind, doc: $doc }, )*
        ];
    };
}

settings! {
    "editor.fill_column", "Text Editor", "Editor: Fill Column", Kind::Int { min: 1, max: 10_000 },
        "Column at which fill-paragraph (M-q) and auto-fill mode break lines. Major modes can override it.";
    "editor.tab_width", "Text Editor", "Editor: Tab Width", Kind::Int { min: 1, max: 64 },
        "Number of columns between tab stops. Major modes can override it.";
    "modes.default_major_mode", "Text Editor", "Modes: Default Major Mode", Kind::MajorMode,
        "Major mode for files that no file pattern, interpreter or mode marker assigns one to.";
    "modes.initial_major_mode", "Text Editor", "Modes: Initial Major Mode", Kind::MajorMode,
        "Major mode of the *scratch* buffer.";

    "font.family", "Font", "Font: Family", Kind::Text { nullable: false, placeholder: "monospace" },
        "Font of buffers and terminals. \"monospace\" picks the platform's monospace font.";
    "font.ui_family", "Font", "Font: UI Family", Kind::Text { nullable: false, placeholder: "system-ui" },
        "Font of panels, tabs and the mode line. \"system-ui\" picks the platform's interface font.";
    "font.size", "Font", "Font: Size", Kind::Float { min: 4.0, max: 200.0 },
        "Font size in pixels. Panels scale with it unless font.ui_size is set in the settings file.";
    "font.line_height", "Font", "Font: Line Height", Kind::Float { min: 1.0, max: 4.0 },
        "Line height as a multiple of the font size.";

    "theme", "Appearance", "Theme", Kind::Theme,
        "Color theme. Define new themes, or change single colors of a theme, under \"themes\" in the settings file.";

    "terminal.shell", "Terminal", "Terminal: Shell", Kind::Text { nullable: true, placeholder: "$SHELL, else /bin/sh" },
        "Program a new terminal runs. Leave empty to use $SHELL.";
    "terminal.shell_args", "Terminal", "Terminal: Shell Arguments", Kind::List,
        "Arguments passed to the shell, separated by commas.";
    "terminal.term", "Terminal", "Terminal: TERM", Kind::Text { nullable: false, placeholder: "xterm-256color" },
        "Value of the TERM environment variable for terminal programs.";
    "terminal.scrollback_lines", "Terminal", "Terminal: Scrollback Lines", Kind::Int { min: 0, max: 10_000_000 },
        "Lines of history kept per terminal.";
    "terminal.backend", "Terminal", "Terminal: Backend", Kind::Choice(&["daemon", "local"]),
        "daemon: terminal processes run in the session daemon and survive the editor. local: they run as children of the editor and end with it. Takes effect after a restart.";
    "terminal.replay_bytes", "Terminal", "Terminal: Replay Bytes", Kind::Int { min: 0, max: 268_435_456 },
        "Output the daemon keeps per terminal to redraw it when the editor reattaches, in bytes.";
    "terminal.socket", "Terminal", "Terminal: Daemon Socket", Kind::Text { nullable: true, placeholder: "default location" },
        "Path of the session daemon's socket. Leave empty for the default ($EMAX_PTYD_SOCKET overrides both).";
    "terminal.daemon_idle_exit", "Terminal", "Terminal: Daemon Idle Exit", Kind::Int { min: 0, max: 31_536_000 },
        "Seconds the daemon waits with no terminals and no editor attached before it exits. 0: never.";
    "terminal.escape_keys", "Terminal", "Terminal: Escape Keys", Kind::List,
        "Keys that reach the editor instead of the program in char mode (Emacs notation, separated by commas), e.g. C-x, M-x.";

    "session.restore_session", "Session", "Session: Restore Session", Kind::Bool,
        "Save tabs, windows and buffers on quit and restore them when emax starts without files.";
    "session.state_dir", "Session", "Session: State Directory", Kind::Text { nullable: true, placeholder: "platform default" },
        "Directory the session file is kept in. Takes effect after a restart.";
}

/// A setting being edited in place.
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    /// A text field (strings, numbers, lists) and the last validation error.
    Text { input: String, error: Option<String> },
    /// An open dropdown, with the highlighted option.
    Choice { options: Vec<String>, index: usize },
}

/// One line of the settings list.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Row {
    Header(&'static str),
    /// The setting at this index of `matches`.
    Setting(usize),
}

pub struct SettingsEditor {
    /// The user file changes go to (`None`: no home directory to put it in).
    pub path: Option<PathBuf>,
    defaults: Value,
    /// The user file's contents (an empty object if it doesn't exist).
    user: Value,
    /// Why the user file can't be edited (it isn't a JSON object).
    pub file_error: Option<String>,
    pub query: String,
    /// The cursor in `query`, as bytes after it (see `line_edit`).
    pub query_tail: usize,
    /// The cursor in the text field being edited, likewise.
    pub input_tail: usize,
    /// Indices into `SETTINGS` that the query keeps, in section order.
    pub matches: Vec<usize>,
    /// Index into `matches`.
    pub selected: usize,
    /// `matches` is ordered by relevance to the search, not by section.
    pub ranked: bool,
    pub edit: Option<Edit>,
    pub message: Option<String>,
    /// The user file was written since the view last looked (it reloads the configuration).
    pub changed: bool,
}

impl SettingsEditor {
    /// A settings editor writing to `path`, reading its current contents.
    pub fn new(path: Option<PathBuf>) -> Self {
        let defaults: Value = serde_json::from_str(DEFAULT_JSON).expect("built-in config is valid JSON");
        let (user, file_error) = match path.as_deref().map(std::fs::read_to_string) {
            None => (Value::Object(Map::new()), Some("No settings file location ($HOME is not set)".into())),
            Some(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => (Value::Object(Map::new()), None),
            Some(Err(e)) => (Value::Object(Map::new()), Some(format!("Cannot read the settings file: {e}"))),
            Some(Ok(text)) if text.trim().is_empty() => (Value::Object(Map::new()), None),
            Some(Ok(text)) => match serde_json::from_str::<Value>(&text) {
                Ok(v @ Value::Object(_)) => (v, None),
                Ok(_) => (Value::Object(Map::new()), Some("The settings file is not a JSON object".into())),
                Err(e) => (Value::Object(Map::new()), Some(format!("The settings file has an error: {e}"))),
            },
        };
        let mut s = Self {
            path,
            defaults,
            user,
            file_error,
            query: String::new(),
            query_tail: 0,
            input_tail: 0,
            matches: Vec::new(),
            selected: 0,
            ranked: false,
            edit: None,
            message: None,
            changed: false,
        };
        s.refilter();
        s
    }

    /// The path shown in the header, `~` abbreviated.
    pub fn path_label(&self) -> String {
        self.path.as_deref().map(abbreviate).unwrap_or_default()
    }

    // ------------------------------------------------------------ values

    /// The value the user file sets for `key`, if it sets one.
    pub fn user_value(&self, key: &str) -> Option<&Value> {
        get_path(&self.user, key)
    }

    /// The built-in value of `key`.
    pub fn default_value(&self, key: &str) -> Option<&Value> {
        get_path(&self.defaults, key)
    }

    /// The value in effect: the user's, else the default.
    pub fn value(&self, key: &str) -> Value {
        self.user_value(key).or_else(|| self.default_value(key)).cloned().unwrap_or(Value::Null)
    }

    /// Whether the user file sets `key` (VS Code's "Modified").
    pub fn is_modified(&self, key: &str) -> bool {
        self.user_value(key).is_some()
    }

    /// The setting's value as its control shows it.
    pub fn display(&self, s: &Setting) -> String {
        display_value(&self.value(s.key))
    }

    /// The default value as text, for "Default: …" hints.
    pub fn default_display(&self, s: &Setting) -> String {
        self.default_value(s.key).map(display_value).unwrap_or_default()
    }

    /// The options of a choice setting.
    pub fn options(&self, s: &Setting) -> Vec<String> {
        let keys_of = |path: &str| -> Vec<String> {
            let mut names: Vec<String> = [&self.defaults, &self.user]
                .into_iter()
                .filter_map(|v| get_path(v, path)?.as_object())
                .flat_map(|m| m.keys().cloned())
                .collect();
            names.sort();
            names.dedup();
            names
        };
        match s.kind {
            Kind::Choice(opts) => opts.iter().map(|o| o.to_string()).collect(),
            Kind::Theme => keys_of("themes"),
            Kind::MajorMode => keys_of("modes.major"),
            _ => Vec::new(),
        }
    }

    // ---------------------------------------------------------- filtering

    /// Re-run the query.
    pub fn refilter(&mut self) {
        let current = self.selected_setting().map(|s| s.key);
        let mut modified_only = false;
        let terms: Vec<String> = self
            .query
            .split_whitespace()
            .filter(|t| {
                let m = t.eq_ignore_ascii_case(MODIFIED_FILTER);
                modified_only |= m;
                !m
            })
            .map(str::to_lowercase)
            .collect();
        // Every term must match somewhere; settings that match in their
        // title or key rank above those that only match in the description.
        let mut scored: Vec<(usize, usize)> = SECTIONS
            .iter()
            .flat_map(|sec| SETTINGS.iter().enumerate().filter(move |(_, s)| s.section == *sec))
            .filter(|(_, s)| !modified_only || self.is_modified(s.key))
            .filter_map(|(i, s)| {
                let (title, key) = (s.title.to_lowercase(), s.key.to_lowercase());
                let (section, doc) = (s.section.to_lowercase(), s.doc.to_lowercase());
                terms
                    .iter()
                    .try_fold(0, |score, t| {
                        let t = t.as_str();
                        let hit = if title.contains(t) || key.contains(t) {
                            2
                        } else if section.contains(t) {
                            1
                        } else if doc.contains(t) {
                            0
                        } else {
                            return None;
                        };
                        Some(score + hit)
                    })
                    .map(|score| (i, score))
            })
            .collect();
        self.ranked = !terms.is_empty();
        if self.ranked {
            scored.sort_by_key(|&(_, score)| std::cmp::Reverse(score));
        }
        self.matches = scored.into_iter().map(|(i, _)| i).collect();
        // A new search starts at its best match; clearing it keeps the place.
        self.selected = current
            .filter(|_| !self.ranked)
            .and_then(|k| self.matches.iter().position(|&i| SETTINGS[i].key == k))
            .unwrap_or(0)
            .min(self.matches.len().saturating_sub(1));
    }

    pub fn selected_setting(&self) -> Option<&'static Setting> {
        self.matches.get(self.selected).map(|&i| &SETTINGS[i])
    }

    /// The list as shown: each section that has matches, with its header;
    /// while searching, the matches by relevance.
    pub fn rows(&self) -> Vec<Row> {
        if self.ranked {
            return (0..self.matches.len()).map(Row::Setting).collect();
        }
        let mut rows = Vec::new();
        let mut last = "";
        for (m, &i) in self.matches.iter().enumerate() {
            let sec = SETTINGS[i].section;
            if sec != last {
                rows.push(Row::Header(sec));
                last = sec;
            }
            rows.push(Row::Setting(m));
        }
        rows
    }

    /// How many matches each section has (the table of contents).
    pub fn section_counts(&self) -> Vec<(&'static str, usize)> {
        SECTIONS
            .iter()
            .map(|sec| (*sec, self.matches.iter().filter(|&&i| SETTINGS[i].section == *sec).count()))
            .collect()
    }

    /// The section of the selected setting.
    pub fn selected_section(&self) -> Option<&'static str> {
        self.selected_setting().map(|s| s.section)
    }

    /// Select the first match in `section` (a click in the table of contents).
    pub fn select_section(&mut self, section: &str) {
        if let Some(m) = self.matches.iter().position(|&i| SETTINGS[i].section == section) {
            self.select(m);
        }
    }

    /// Select match `m`. An edit in progress on another setting is
    /// committed if it is valid, else dropped (like a field losing focus).
    pub fn select(&mut self, m: usize) {
        if m < self.matches.len() && m != self.selected {
            if matches!(self.edit, Some(Edit::Text { .. })) {
                self.commit();
            }
            self.edit = None;
            self.selected = m;
        }
    }

    fn move_by(&mut self, d: isize) {
        let n = self.matches.len();
        if n > 0 {
            self.select((self.selected as isize + d).clamp(0, n as isize - 1) as usize);
        }
    }

    fn move_section(&mut self, d: isize) {
        let present: Vec<&str> = self.section_counts().into_iter().filter(|(_, n)| *n > 0).map(|(s, _)| s).collect();
        let Some(cur) = self.selected_section().and_then(|s| present.iter().position(|p| *p == s)) else { return };
        let next = (cur as isize + d).rem_euclid(present.len() as isize) as usize;
        self.select_section(present[next]);
    }

    // ------------------------------------------------------------ editing

    /// Start editing the selected setting: toggle a checkbox, open a
    /// dropdown, or put its value in a text field.
    pub fn activate(&mut self) {
        let Some(s) = self.selected_setting() else { return };
        if s.kind == Kind::Bool {
            let on = self.value(s.key).as_bool().unwrap_or(false);
            self.write(s.key, Some(Value::Bool(!on)));
        } else if s.kind.is_choice() {
            let options = self.options(s);
            let cur = self.display(s);
            let index = options.iter().position(|o| *o == cur).unwrap_or(0);
            self.edit = Some(Edit::Choice { options, index });
        } else {
            self.edit = Some(Edit::Text { input: self.display(s), error: None });
            self.input_tail = 0;
        }
    }

    /// Commit the edit in progress. A text field that doesn't hold a valid
    /// value stays open with the problem shown.
    pub fn commit(&mut self) -> bool {
        let Some(s) = self.selected_setting() else { return false };
        match self.edit.take() {
            None => true,
            Some(Edit::Choice { options, index }) => {
                if let Some(o) = options.get(index) {
                    self.write(s.key, Some(Value::String(o.clone())));
                }
                true
            }
            Some(Edit::Text { input, .. }) => match parse_input(&s.kind, &input) {
                Ok(v) => {
                    // Confirming an unchanged field doesn't mark the setting modified.
                    if v != self.value(s.key) {
                        self.write(s.key, Some(v));
                    }
                    true
                }
                Err(e) => {
                    self.edit = Some(Edit::Text { input, error: Some(e) });
                    false
                }
            },
        }
    }

    /// Choose option `index` of the open dropdown and commit it (a click).
    pub fn choose(&mut self, index: usize) {
        if let Some(Edit::Choice { index: i, .. }) = &mut self.edit {
            *i = index;
        }
        self.commit();
    }

    /// Set a checkbox (a click).
    pub fn set_bool(&mut self, m: usize, on: bool) {
        self.select(m);
        if let Some(s) = self.selected_setting().filter(|s| s.kind == Kind::Bool) {
            self.write(s.key, Some(Value::Bool(on)));
        }
    }

    /// Remove match `m`'s setting from the user file, back to the default.
    pub fn reset(&mut self, m: usize) {
        self.select(m);
        self.edit = None;
        if let Some(s) = self.selected_setting().filter(|s| self.is_modified(s.key)) {
            self.write(s.key, None);
            self.message = Some(format!("{} reset to the default", s.title));
        }
    }

    /// Type into the search field, or into the field being edited, at the cursor.
    pub fn insert(&mut self, text: &str) {
        self.message = None;
        self.edit_field(|s, tail| line_edit::insert(s, *tail, text));
    }

    /// The text the editing keys work on and its cursor: the field being
    /// edited, else the query.
    pub(crate) fn field(&mut self) -> Option<(&mut String, &mut usize)> {
        match &mut self.edit {
            Some(Edit::Text { input, error }) => {
                *error = None;
                Some((input, &mut self.input_tail))
            }
            Some(Edit::Choice { .. }) => None,
            None => Some((&mut self.query, &mut self.query_tail)),
        }
    }

    fn edit_field(&mut self, f: impl FnOnce(&mut String, &mut usize)) {
        let searching = self.edit.is_none();
        if let Some((s, tail)) = self.field() {
            f(s, tail);
        }
        if searching {
            self.refilter();
        }
    }

    /// Set `key` in the user file (`None` removes it) and write the file.
    fn write(&mut self, key: &str, value: Option<Value>) {
        if let Some(e) = &self.file_error {
            self.message = Some(format!("{e} — open it with settings-open-json to fix it"));
            return;
        }
        let Some(path) = self.path.clone() else { return };
        let mut user = self.user.clone();
        match value {
            Some(v) => set_path(&mut user, key, v),
            None => remove_path(&mut user, key),
        }
        match write_json(&path, &user) {
            Ok(()) => {
                self.user = user;
                self.changed = true;
            }
            Err(e) => self.message = Some(format!("Cannot write {}: {e}", abbreviate(&path))),
        }
    }

    /// Run a settings action (one of `SETTINGS_ACTIONS`) other than
    /// opening the file. Returns false when the editor should close.
    fn action(&mut self, action: &str) -> bool {
        if action != "settings-reset" {
            self.message = None;
        }
        let choosing = matches!(self.edit, Some(Edit::Choice { .. }));
        match action {
            "settings-close" => return false,
            // Back out one step at a time: the edit, then the search, then the editor.
            "settings-quit" => {
                if self.edit.is_some() {
                    self.edit = None;
                } else if !self.query.is_empty() {
                    self.query.clear();
                    self.query_tail = 0;
                    self.refilter();
                } else {
                    return false;
                }
            }
            "settings-accept" if self.edit.is_some() => {
                self.commit();
            }
            "settings-accept" => self.activate(),
            "settings-next" | "settings-previous" if choosing => {
                let d = if action == "settings-next" { 1 } else { -1 };
                if let Some(Edit::Choice { options, index }) = &mut self.edit {
                    *index = (*index as isize + d).clamp(0, options.len().saturating_sub(1) as isize) as usize;
                }
            }
            // Leaving a text field commits it, as clicking elsewhere would.
            "settings-next"
            | "settings-previous"
            | "settings-next-section"
            | "settings-previous-section"
            | "settings-page-down"
            | "settings-page-up"
            | "settings-first"
            | "settings-last" => {
                if !self.commit() {
                    return true;
                }
                match action {
                    "settings-next" => self.move_by(1),
                    "settings-previous" => self.move_by(-1),
                    "settings-next-section" => self.move_section(1),
                    "settings-previous-section" => self.move_section(-1),
                    "settings-page-down" => self.move_by(PAGE as isize),
                    "settings-page-up" => self.move_by(-(PAGE as isize)),
                    "settings-first" => self.select(0),
                    _ => self.select(self.matches.len().saturating_sub(1)),
                }
            }
            "settings-delete-char" => self.edit_field(|s, tail| {
                line_edit::delete_backward(s, *tail);
            }),
            "settings-delete-word" => self.edit_field(|s, tail| {
                line_edit::kill_word_backward(s, *tail);
            }),
            "settings-clear" => self.edit_field(|s, tail| {
                s.clear();
                *tail = 0;
            }),
            "settings-reset" => self.reset(self.selected),
            _ => {}
        }
        true
    }
}

impl Editor {
    /// Open the settings editor on the user's configuration file.
    pub fn open_settings(&mut self) {
        self.open_settings_at(user_config_path());
    }

    /// Open the settings editor on `path` (tests use a temporary file).
    pub fn open_settings_at(&mut self, path: Option<PathBuf>) {
        self.picker = None;
        let s = SettingsEditor::new(path);
        self.message = s.file_error.clone();
        self.settings = Some(s);
    }

    /// Handle a key while the settings editor is open: run the action
    /// `keymap` binds it to, or type it into the field.
    pub fn settings_key(&mut self, keymap: &Keymap, key: &Key) {
        match keymap.lookup(&[key.canonical()]) {
            Some(action) => self.settings_action(action),
            None if self.field_command(key).is_some() => {
                let cmd = self.field_command(key).unwrap();
                self.settings_field_command(cmd);
            }
            None => {
                if let Some(t) = key.insertable_text().map(str::to_string)
                    && let Some(s) = self.settings.as_mut()
                {
                    s.insert(&t);
                }
            }
        }
    }

    /// Run text-field command `cmd` (`C-a`, `C-d`, `M-f`, `C-y`...) on the
    /// field being edited, else the search.
    pub fn settings_field_command(&mut self, cmd: &str) {
        let Some(mut s) = self.settings.take() else { return };
        s.message = None;
        let searching = s.edit.is_none();
        let edit = s.field().and_then(|(text, tail)| self.run_field_command(text, tail, cmd));
        if searching && edit.is_some_and(|e| e != FieldEdit::Moved) {
            s.refilter();
        }
        self.settings = Some(s);
    }

    /// Run a settings action (one of `SETTINGS_ACTIONS`).
    pub fn settings_action(&mut self, action: &str) {
        if action == "settings-open-json" {
            return self.open_settings_json();
        }
        // In a text field, the reset key kills to the end of the field instead.
        if action == "settings-reset"
            && self.settings.as_ref().is_some_and(|s| matches!(s.edit, Some(Edit::Text { .. })))
        {
            return self.settings_field_command("kill-line");
        }
        let Some(s) = self.settings.as_mut() else { return };
        if !s.action(action) {
            self.settings = None;
        }
    }

    /// Close the settings editor and visit the settings file, creating it
    /// (as an empty object) if it doesn't exist yet.
    pub fn open_settings_json(&mut self) {
        let path = match self.settings.take() {
            Some(s) => s.path,
            None => user_config_path(),
        };
        let Some(path) = path else {
            self.message = Some("No settings file location ($HOME is not set)".into());
            return;
        };
        if !path.exists()
            && let Err(e) =
                path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(&path, "{\n}\n"))
        {
            self.message = Some(format!("Cannot create {}: {e}", abbreviate(&path)));
            return;
        }
        if let Err(e) = self.find_file(&path) {
            self.message = Some(e);
        }
    }
}

// ---------------------------------------------------------------- JSON paths

fn get_path<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    key.split('.').try_fold(v, |v, k| v.as_object()?.get(k))
}

/// Set `key`, creating (or replacing non-object) parents as needed.
fn set_path(v: &mut Value, key: &str, value: Value) {
    let mut cur = v;
    let parts: Vec<&str> = key.split('.').collect();
    for p in &parts[..parts.len() - 1] {
        if !cur.as_object().is_some_and(|m| m.get(*p).is_some_and(Value::is_object)) {
            cur.as_object_mut().expect("parent is an object").insert(p.to_string(), Value::Object(Map::new()));
        }
        cur = cur.as_object_mut().unwrap().get_mut(*p).unwrap();
    }
    cur.as_object_mut().expect("parent is an object").insert(parts[parts.len() - 1].to_string(), value);
}

/// Remove `key`, and any parent objects that become empty.
fn remove_path(v: &mut Value, key: &str) {
    fn go(v: &mut Value, parts: &[&str]) {
        let Some(m) = v.as_object_mut() else { return };
        if let [last] = parts {
            m.shift_remove(*last);
            return;
        }
        if let Some(child) = m.get_mut(parts[0]) {
            go(child, &parts[1..]);
            if child.as_object().is_some_and(Map::is_empty) {
                m.shift_remove(parts[0]);
            }
        }
    }
    go(v, &key.split('.').collect::<Vec<_>>());
}

/// Write `v` to `path` pretty-printed, through a temporary file so a crash
/// never leaves half a file.
fn write_json(path: &Path, v: &Value) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut text = serde_json::to_string_pretty(v).map_err(std::io::Error::other)?;
    text.push('\n');
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// A value as a control shows it.
fn display_value(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().map(display_value).collect::<Vec<_>>().join(", "),
        Value::Object(m) => m.iter().map(|(k, v)| format!("{k}: {}", display_value(v))).collect::<Vec<_>>().join(", "),
        other => other.to_string(),
    }
}

/// Check what was typed into a setting's field and turn it into its JSON value.
pub fn parse_input(kind: &Kind, input: &str) -> Result<Value, String> {
    let t = input.trim();
    match *kind {
        Kind::Int { min, max } => match t.parse::<i64>() {
            Ok(n) if (min..=max).contains(&n) => Ok(Value::from(n)),
            Ok(_) => Err(format!("Must be between {min} and {max}")),
            Err(_) => Err("Must be a whole number".into()),
        },
        Kind::Float { min, max } => match t.parse::<f64>() {
            Ok(n) if n.is_finite() && (min..=max).contains(&n) => {
                // Whole numbers stay integers in the file (14, not 14.0).
                Ok(if n.fract() == 0.0 { Value::from(n as i64) } else { Value::from(n) })
            }
            Ok(_) => Err(format!("Must be between {min} and {max}")),
            Err(_) => Err("Must be a number".into()),
        },
        Kind::Text { nullable: true, .. } if t.is_empty() => Ok(Value::Null),
        Kind::Text { .. } if t.is_empty() => Err("Can't be empty".into()),
        Kind::Text { .. } => Ok(Value::String(t.to_string())),
        Kind::List => Ok(Value::Array(
            t.split(',').map(str::trim).filter(|s| !s.is_empty()).map(|s| Value::String(s.to_string())).collect(),
        )),
        Kind::Bool => match t {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            _ => Err("Must be true or false".into()),
        },
        Kind::Choice(_) | Kind::Theme | Kind::MajorMode => Ok(Value::String(t.to_string())),
    }
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
