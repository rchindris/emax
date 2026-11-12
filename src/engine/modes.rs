//! Major and minor modes.
//!
//! Every buffer has exactly one major mode and any number of minor modes.
//!
//! - **Major modes** are data, defined in the configuration (`modes.major`):
//!   a mode-line lighter, an optional parent mode it inherits from, file name
//!   patterns and interpreters that select it, settings, a keymap, and the
//!   minor modes it turns on (Emacs' mode hooks). Each one is also a command
//!   (`rust-mode`) that switches the current buffer to it.
//! - **Minor modes** are a fixed set implemented in Rust ([`MinorMode`]); the
//!   configuration gives each a lighter and a keymap. Each is a command that
//!   toggles it (with a prefix argument: on if positive, off otherwise).
//!
//! Settings resolve buffer-local value → major mode → its parents → global.
//! Keys resolve through the minor modes' keymaps (most recently enabled
//! first), then the major mode's and its parents', then the global keymap.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use ropey::Rope;

use super::buffer::Buffer;
use super::commands::{CmdResult, Command};
use super::config::{Config, Settings};
use super::editor::{Args, Editor, RawArg};
use super::keymap::Keymap;

/// The major mode every configuration has, used when nothing else applies.
pub const FUNDAMENTAL: &str = "fundamental-mode";

/// How far a parent chain is followed (cycles are removed when loading, this is a backstop).
const MAX_DEPTH: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MinorMode {
    /// Break lines at `fill_column` while typing.
    AutoFill,
    /// Refuse every change to the buffer.
    ReadOnly,
    /// Typed characters replace the ones after point instead of pushing them along.
    Overwrite,
    /// Typing an opening bracket or quote also inserts the closing one.
    ElectricPair,
}

impl MinorMode {
    /// In mode-line order.
    pub const ALL: [MinorMode; 4] = [MinorMode::AutoFill, MinorMode::ReadOnly, MinorMode::Overwrite, MinorMode::ElectricPair];

    pub fn name(self) -> &'static str {
        match self {
            MinorMode::AutoFill => "auto-fill-mode",
            MinorMode::ReadOnly => "read-only-mode",
            MinorMode::Overwrite => "overwrite-mode",
            MinorMode::ElectricPair => "electric-pair-mode",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.name() == name)
    }

    /// The name in messages: "Auto-Fill mode enabled in current buffer".
    fn title(self) -> &'static str {
        match self {
            MinorMode::AutoFill => "Auto-Fill",
            MinorMode::ReadOnly => "Read-Only",
            MinorMode::Overwrite => "Overwrite",
            MinorMode::ElectricPair => "Electric-Pair",
        }
    }

    pub(crate) fn default_lighter(self) -> &'static str {
        match self {
            MinorMode::AutoFill => "Fill",
            MinorMode::ReadOnly => "RO",
            MinorMode::Overwrite => "Ovwrt",
            MinorMode::ElectricPair => "",
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// Settings a major mode or a single buffer can override (`None`: inherit).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LocalSettings {
    pub fill_column: Option<usize>,
    pub tab_width: Option<usize>,
}

/// A buffer's modes and buffer-local settings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BufferModes {
    pub major: &'static str,
    /// In the order they were enabled.
    pub minor: Vec<MinorMode>,
    pub local: LocalSettings,
}

impl BufferModes {
    pub fn has(&self, m: MinorMode) -> bool {
        self.minor.contains(&m)
    }
}

pub struct MajorMode {
    pub name: &'static str,
    pub lighter: String,
    pub parent: Option<&'static str>,
    /// File name patterns (`*` and `?` wildcards), e.g. `*.rs`.
    pub files: Vec<String>,
    /// Programs named on a `#!` line, e.g. `python` (trailing version digits are ignored).
    pub interpreters: Vec<String>,
    pub settings: LocalSettings,
    pub keymap: Keymap,
    /// Minor modes turned on when the mode starts (after the parent's).
    pub minor_modes: Vec<MinorMode>,
    pub(crate) command: Command,
}

pub struct MinorModeDef {
    pub lighter: String,
    pub keymap: Keymap,
}

/// Everything from the configuration that decides how a buffer behaves: the
/// global settings and keymap, and the major and minor modes.
pub struct Modes {
    pub settings: Settings,
    pub global: Keymap,
    pub(crate) major: HashMap<&'static str, MajorMode>,
    /// Indexed by `MinorMode`.
    pub(crate) minor: Vec<MinorModeDef>,
    /// Major mode for buffers nothing else decides.
    pub default_major: &'static str,
    /// Major mode for `*scratch*`.
    pub initial_major: &'static str,
}

impl Modes {
    /// The modes from the built-in configuration (shared, built once).
    pub fn builtin() -> Arc<Modes> {
        static BUILTIN: OnceLock<Arc<Modes>> = OnceLock::new();
        BUILTIN.get_or_init(|| Arc::new(Config::builtin().modes)).clone()
    }

    pub fn major(&self, name: &str) -> Option<&MajorMode> {
        self.major.get(name)
    }

    pub fn minor(&self, m: MinorMode) -> &MinorModeDef {
        &self.minor[m.index()]
    }

    /// Major mode names, sorted.
    pub fn major_names(&self) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = self.major.keys().copied().collect();
        v.sort_unstable();
        v
    }

    /// The command that switches to major mode `name`.
    pub fn command(&self, name: &str) -> Option<&Command> {
        self.major.get(name).map(|m| &m.command)
    }

    /// `name`, its parent, the parent's parent, ...
    pub fn chain(&self, name: &str) -> Vec<&MajorMode> {
        let mut v = Vec::new();
        let mut cur = self.major.get(name);
        while let Some(m) = cur {
            if v.len() == MAX_DEPTH {
                break;
            }
            v.push(m);
            cur = m.parent.and_then(|p| self.major.get(p));
        }
        v
    }

    /// Whether `name` is `ancestor` or derives from it.
    pub fn derives_from(&self, name: &str, ancestor: &str) -> bool {
        self.chain(name).iter().any(|m| m.name == ancestor)
    }

    fn setting(&self, b: &BufferModes, get: fn(&LocalSettings) -> Option<usize>) -> Option<usize> {
        get(&b.local).or_else(|| self.chain(b.major).iter().find_map(|m| get(&m.settings)))
    }

    pub fn fill_column(&self, b: &BufferModes) -> usize {
        self.setting(b, |s| s.fill_column).unwrap_or(self.settings.fill_column)
    }

    pub fn tab_width(&self, b: &BufferModes) -> usize {
        self.setting(b, |s| s.tab_width).unwrap_or(self.settings.tab_width).max(1)
    }

    /// The keymaps a buffer's keys are looked up in, highest priority first.
    pub fn keymaps(&self, b: &BufferModes) -> Vec<&Keymap> {
        let mut v: Vec<&Keymap> = b.minor.iter().rev().map(|m| &self.minor(*m).keymap).collect();
        v.extend(self.chain(b.major).into_iter().map(|m| &m.keymap));
        v.push(&self.global);
        v
    }

    /// Mode-line text: the major mode's lighter, then the minor modes' (e.g. `Rust Fill`).
    pub fn lighters(&self, b: &BufferModes) -> String {
        let mut parts: Vec<&str> = Vec::new();
        parts.push(self.major.get(b.major).map_or(b.major, |m| m.lighter.as_str()));
        for m in MinorMode::ALL {
            let l = self.minor(m).lighter.as_str();
            if b.has(m) && !l.is_empty() {
                parts.push(l);
            }
        }
        parts.join(" ")
    }

    /// A buffer's state right after major mode `name` starts: no buffer-local
    /// settings, and the minor modes the mode and its parents turn on.
    pub fn enter(&self, name: &str) -> BufferModes {
        let name = self.major.get(name).map_or(self.default_major, |m| m.name);
        let mut minor = Vec::new();
        for m in self.chain(name).iter().rev() {
            for mm in &m.minor_modes {
                if !minor.contains(mm) {
                    minor.push(*mm);
                }
            }
        }
        BufferModes { major: name, minor, local: LocalSettings::default() }
    }

    /// The modes a newly created or opened buffer starts with.
    pub fn initial(&self, buf: &Buffer) -> BufferModes {
        let major = self.detect(buf).unwrap_or(if buf.path.is_none() && buf.name == "*scratch*" {
            self.initial_major
        } else {
            self.default_major
        });
        let mut modes = self.enter(major);
        let read_only_file = buf.path.as_deref().and_then(|p| p.metadata().ok()).is_some_and(|m| m.permissions().readonly());
        if read_only_file {
            modes.minor.push(MinorMode::ReadOnly);
        }
        modes
    }

    /// Pick a major mode the way Emacs' `set-auto-mode` does: a `-*- mode -*-`
    /// marker on the first line (or the second, after `#!`), then the
    /// interpreter on a `#!` line, then the file name.
    pub fn detect(&self, buf: &Buffer) -> Option<&'static str> {
        let first = first_line(&buf.rope, 0);
        let marker = mode_marker(&first).or_else(|| {
            first.starts_with("#!").then(|| first_line(&buf.rope, 1)).and_then(|l| mode_marker(&l))
        });
        if let Some(name) = marker.and_then(|n| self.major.get(n.as_str())) {
            return Some(name.name);
        }
        if let Some(prog) = interpreter(&first) {
            let base = prog.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
            let mut found: Vec<&MajorMode> =
                self.major.values().filter(|m| m.interpreters.iter().any(|i| *i == prog || *i == base)).collect();
            found.sort_by_key(|m| m.name);
            if let Some(m) = found.first() {
                return Some(m.name);
            }
        }
        let file = buf.path.as_deref().and_then(Path::file_name)?.to_string_lossy().into_owned();
        // The most specific pattern wins (the most literal characters), then the name.
        self.major
            .values()
            .filter_map(|m| {
                let best = m.files.iter().filter(|p| glob(p, &file)).map(|p| p.chars().filter(|c| *c != '*' && *c != '?').count()).max()?;
                Some((best, m.name))
            })
            .max_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(a.1)))
            .map(|(_, name)| name)
    }
}

/// Up to 256 chars of line `n` (enough for markers and `#!`; long lines aren't copied).
fn first_line(r: &Rope, n: usize) -> String {
    if n >= r.len_lines() {
        return String::new();
    }
    r.line(n).chars().take(256).take_while(|c| *c != '\n' && *c != '\r').collect()
}

/// The mode a `-*- rust -*-` or `-*- mode: rust; ... -*-` marker names, as `rust-mode`.
fn mode_marker(line: &str) -> Option<String> {
    let start = line.find("-*-")? + 3;
    let len = line[start..].find("-*-")?;
    let inner = line[start..start + len].trim();
    let value = if inner.contains(':') {
        inner.split(';').find_map(|part| {
            let (k, v) = part.split_once(':')?;
            k.trim().eq_ignore_ascii_case("mode").then(|| v.trim())
        })?
    } else {
        inner
    };
    let value = value.to_lowercase();
    if value.is_empty() {
        return None;
    }
    Some(if value.ends_with("-mode") { value } else { format!("{value}-mode") })
}

/// The program a `#!` line runs (looking through `/usr/bin/env`).
fn interpreter(line: &str) -> Option<String> {
    let rest = line.strip_prefix("#!")?;
    let mut words = rest.split_whitespace();
    let base = |w: &str| w.rsplit('/').next().unwrap_or(w).to_string();
    let prog = base(words.next()?);
    if prog == "env" {
        return words.find(|w| !w.starts_with('-') && !w.contains('=')).map(base);
    }
    Some(prog)
}

/// File name matching with `*` (any run of chars) and `?` (one char).
/// Case-insensitive for lowercase patterns, so `*.md` matches `README.MD`.
pub fn glob(pattern: &str, name: &str) -> bool {
    fn go(p: &[char], n: &[char]) -> bool {
        match p.split_first() {
            None => n.is_empty(),
            Some(('*', rest)) => (0..=n.len()).any(|i| go(rest, &n[i..])),
            Some(('?', rest)) => !n.is_empty() && go(rest, &n[1..]),
            Some((c, rest)) => n.first() == Some(c) && go(rest, &n[1..]),
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    go(&p, &n) || (!pattern.chars().any(char::is_uppercase) && go(&p, &name.to_lowercase().chars().collect::<Vec<_>>()))
}

/// A `&'static str` for a name from the configuration (each distinct name is
/// leaked once; there are only ever a few).
pub(crate) fn intern(s: &str) -> &'static str {
    static NAMES: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut names = NAMES.get_or_init(Default::default).lock().unwrap();
    if let Some(n) = names.get(s) {
        return n;
    }
    let n: &'static str = Box::leak(s.to_string().into_boxed_str());
    names.insert(n);
    n
}

/// What every major mode command runs: switch to the mode it is named after.
pub(crate) fn major_mode_command(ed: &mut Editor, _: &Args) -> CmdResult {
    let name = ed.this_command;
    ed.set_major_mode(name);
    Ok(())
}

// ================================================================ editor

impl Editor {
    pub fn modes(&self) -> &Modes {
        &self.modes
    }

    /// Use a new configuration's modes: every buffer starts over in the mode it selects.
    pub fn set_modes(&mut self, modes: Arc<Modes>) {
        self.modes = modes;
        self.buf.modes = self.modes.initial(&self.buf);
        for s in &mut self.others {
            s.buf.modes = self.modes.initial(&s.buf);
        }
    }

    /// Switch the current buffer to major mode `name`. Like Emacs, this drops
    /// buffer-local settings and minor modes (except read-only), then turns on
    /// the minor modes the new mode wants.
    pub fn set_major_mode(&mut self, name: &str) {
        let read_only = self.minor_on(MinorMode::ReadOnly);
        self.buf.modes = self.modes.enter(name);
        if read_only && !self.buf.modes.has(MinorMode::ReadOnly) {
            self.buf.modes.minor.push(MinorMode::ReadOnly);
        }
    }

    pub fn major_mode(&self) -> &'static str {
        self.buf.modes.major
    }

    pub fn minor_on(&self, m: MinorMode) -> bool {
        self.buf.modes.has(m)
    }

    pub fn set_minor_mode(&mut self, m: MinorMode, on: bool) {
        let list = &mut self.buf.modes.minor;
        list.retain(|x| *x != m);
        if on {
            list.push(m);
        }
    }

    /// A minor mode command: toggle without an argument; with one, enable if
    /// it is positive (`C-u` counts) and disable otherwise.
    pub fn minor_mode_command(&mut self, m: MinorMode, a: &Args) {
        let on = match a.raw {
            None => !self.minor_on(m),
            Some(RawArg::Universal(_)) => true,
            Some(_) => a.n() > 0,
        };
        self.set_minor_mode(m, on);
        self.message = Some(format!("{} mode {} in current buffer", m.title(), if on { "enabled" } else { "disabled" }));
    }

    pub fn fill_column(&self) -> usize {
        self.modes.fill_column(&self.buf.modes)
    }

    pub fn tab_width(&self) -> usize {
        self.modes.tab_width(&self.buf.modes)
    }

    /// Tab width of any buffer (e.g. one shown as a preview).
    pub fn tab_width_of(&self, buf: &Buffer) -> usize {
        self.modes.tab_width(&buf.modes)
    }

    /// The current buffer's keymaps, highest priority first (a terminal
    /// buffer's `terminal` keymap before its modes').
    pub fn active_keymaps(&self) -> Vec<&Keymap> {
        let mut maps = self.modes.keymaps(&self.buf.modes);
        if self.buf.kind == super::buffer::BufferKind::Terminal {
            maps.insert(0, &self.terminal_config.keymap);
        }
        maps
    }

    /// Mode-line text for a buffer's modes, e.g. `Rust Fill`.
    pub fn mode_lighters(&self, buf: &Buffer) -> String {
        self.modes.lighters(&buf.modes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_and_interpreters() {
        assert_eq!(mode_marker(";; -*- lisp -*-").as_deref(), Some("lisp-mode"));
        assert_eq!(mode_marker("// -*- mode: Rust; fill-column: 80 -*-").as_deref(), Some("rust-mode"));
        assert_eq!(mode_marker("/* -*- text-mode -*- */").as_deref(), Some("text-mode"));
        assert_eq!(mode_marker("no marker here"), None);
        assert_eq!(interpreter("#!/usr/bin/env -S steel --flag").as_deref(), Some("steel"));
        assert_eq!(interpreter("#!/usr/local/bin/sbcl --script").as_deref(), Some("sbcl"));
        assert_eq!(interpreter("not a shebang"), None);
    }

    #[test]
    fn globs() {
        assert!(glob("*.rs", "main.rs"));
        assert!(glob("*.md", "README.MD"));
        assert!(!glob("*.rs", "main.rsx"));
        assert!(glob("README", "README"));
        assert!(glob("?akefile", "Makefile"));
        assert!(!glob("Makefile", "makefile"));
    }
}
