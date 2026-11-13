//! Configuration: editor settings, keymaps, modes and themes, written in
//! Steel (Scheme).
//!
//! The built-in defaults live in `config/default.scm` (compiled into the
//! binary). The user's files, if present, run after them, in this order:
//!
//! 1. `init.scm`, written by the user: `$YMACS_CONFIG`, else
//!    `$XDG_CONFIG_HOME/ymacs/init.scm`, else on Windows
//!    `%APPDATA%\ymacs\init.scm`, else `~/.config/ymacs/init.scm`;
//! 2. `settings.scm` next to it, written by the settings editor.
//!
//! A file only needs what it changes. Options replace the defaults; key
//! bindings are added to the keymaps, replacing a binding for the same keys,
//! and binding keys to `#f` removes a default binding:
//!
//! ```scheme
//! (setq fill-column 80)
//! (global-set-key "C-z" 'undo)
//! (global-unset-key "C-x C-c")
//! ```
//!
//! [`eval`] has the forms. `define-major-mode` defines major modes and
//! `define-minor-mode` configures minor modes (see [`super::modes`]); for a
//! mode that exists, only the properties given change. `define-theme` is
//! merged color by color into the theme of the same name (see
//! [`super::theme`]).
//!
//! Under the forms, each file is a list of patches to a JSON-shaped document
//! (the shape [`RawConfig`] reads), applied in order. The `terminal` keymap
//! holds the keys of terminal buffers (sequences starting with one of
//! `terminal-escape-keys` in char mode; any key in copy mode).
//!
//! Problems (Scheme errors, unknown commands, malformed keys) never stop the
//! editor: the offending entry is skipped and reported in `warnings`; a
//! Scheme error skips the rest of its file.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde::Deserialize;

use super::buffers::PICKER_ACTIONS;
use super::commands::{self, Command, flags};
use super::keymap::{Key, Keymap, PREFIX_ARG_COMMANDS};
use super::modes::{self, FUNDAMENTAL, LocalSettings, MajorMode, MinorMode, MinorModeDef, Modes, intern};
use super::search::SEARCH_ACTIONS;
use super::settings::SETTINGS_ACTIONS;
use super::syntax;
use super::theme::{self, DEFAULT_THEME, Fonts, RawThemes, Theme, Themes};

pub mod eval;

pub use eval::{Layer, OPTIONS, OptionDef, option};

/// The built-in configuration.
pub const DEFAULT_SCM: &str = include_str!("../../config/default.scm");

/// The name of the settings editor's file, next to `init.scm`.
pub const SETTINGS_FILE: &str = "settings.scm";

/// The keymaps a configuration can define.
pub const KEYMAP_NAMES: [&str; 5] = ["global", "picker", "search", "settings", "terminal"];

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct RawConfig {
    editor: RawEditor,
    font: RawFont,
    theme: Option<RawThemeChoice>,
    /// theme name -> color name (or `inherits`) -> value.
    themes: RawThemes,
    /// keymap name -> key sequence -> command (`null` unbinds).
    keymaps: BTreeMap<String, RawKeymap>,
    modes: RawModes,
    terminal: RawTerminal,
    session: RawSession,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct RawTerminal {
    /// `null` (the default) means `$SHELL`, else `/bin/sh`.
    #[serde(deserialize_with = "nullable")]
    shell: Option<Option<String>>,
    shell_args: Option<Vec<String>>,
    term: Option<String>,
    scrollback_lines: Option<usize>,
    backend: Option<String>,
    replay_bytes: Option<usize>,
    #[serde(deserialize_with = "nullable")]
    socket: Option<Option<PathBuf>>,
    escape_keys: Option<Vec<String>>,
    daemon_idle_exit: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct RawSession {
    restore_session: Option<bool>,
    #[serde(deserialize_with = "nullable")]
    state_dir: Option<Option<PathBuf>>,
}

/// A field that may be absent (`None`), `null` (`Some(None)`) or set.
fn nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

type RawKeymap = BTreeMap<String, Option<String>>;

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct RawModes {
    default_major_mode: Option<String>,
    initial_major_mode: Option<String>,
    major: BTreeMap<String, RawMajor>,
    minor: BTreeMap<String, RawMinor>,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct RawMajor {
    lighter: Option<String>,
    /// `""` removes an inherited parent.
    parent: Option<String>,
    files: Option<Vec<String>>,
    interpreters: Option<Vec<String>>,
    /// The language to highlight with; `""` turns off an inherited one.
    syntax: Option<String>,
    editor: RawEditor,
    minor_modes: Option<Vec<String>>,
    keymap: RawKeymap,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct RawMinor {
    lighter: Option<String>,
    keymap: RawKeymap,
}

/// A major mode merged from all layers, not yet checked.
#[derive(Default)]
struct MajorAcc {
    lighter: Option<String>,
    parent: Option<String>,
    files: Vec<String>,
    interpreters: Vec<String>,
    syntax: Option<String>,
    settings: LocalSettings,
    minor_modes: Vec<String>,
    keymap: BTreeMap<String, String>,
}

#[derive(Default)]
struct ModesAcc {
    default_major: Option<String>,
    initial_major: Option<String>,
    major: BTreeMap<String, MajorAcc>,
    minor: BTreeMap<String, (Option<String>, BTreeMap<String, String>)>,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct RawEditor {
    fill_column: Option<usize>,
    tab_width: Option<usize>,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct RawFont {
    family: Option<String>,
    ui_family: Option<String>,
    size: Option<f32>,
    /// `null` (the default): the panels follow `size`.
    ui_size: Option<f32>,
    line_height: Option<f32>,
}

#[derive(Deserialize, Clone)]
#[serde(untagged)]
enum RawThemeChoice {
    One(String),
    ByAppearance { light: String, dark: String },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub fill_column: usize,
    pub tab_width: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self { fill_column: 70, tab_width: 4 }
    }
}

/// Where terminal processes run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PtyBackendKind {
    /// In the session daemon: they survive the editor (started when needed).
    #[default]
    Daemon,
    /// As children of the editor.
    Local,
}

/// The `terminal` section, and the `terminal` keymap.
pub struct TerminalConfig {
    /// The program `term` runs (`None`: `$SHELL`, else `/bin/sh`).
    pub shell: Option<String>,
    pub shell_args: Vec<String>,
    /// `TERM` for terminal programs.
    pub term: String,
    /// History kept per terminal, in lines.
    pub scrollback_lines: usize,
    pub backend: PtyBackendKind,
    /// Output the daemon keeps per session to replay on attach, in bytes.
    pub replay_bytes: usize,
    /// The daemon's socket (`$YMACS_PTYD_SOCKET` overrides it; `None`: `session::default_socket_path`).
    pub socket: Option<PathBuf>,
    /// A daemon the editor starts exits after this long with no sessions
    /// and no editor connected (`None`: never; `0` in the file).
    pub daemon_idle_exit: Option<Duration>,
    /// Keys that start an editor key sequence in char mode (canonical form).
    pub escape_keys: Vec<String>,
    /// Bindings for terminal buffers, looked up before the other keymaps.
    pub keymap: Keymap,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            shell: None,
            shell_args: Vec::new(),
            term: "xterm-256color".into(),
            scrollback_lines: 10_000,
            backend: PtyBackendKind::Daemon,
            replay_bytes: 2 << 20,
            socket: None,
            daemon_idle_exit: Some(Duration::from_secs(60)),
            escape_keys: vec!["C-x".into(), "M-x".into(), "C-c".into()],
            keymap: Keymap::new(),
        }
    }
}

impl TerminalConfig {
    /// The terminal configuration of the built-in configuration (shared, built once).
    pub fn builtin() -> Arc<TerminalConfig> {
        static BUILTIN: OnceLock<Arc<TerminalConfig>> = OnceLock::new();
        BUILTIN.get_or_init(|| Arc::new(Config::builtin().terminal)).clone()
    }

    /// The program `term` runs: `shell`, else `$SHELL`, else `/bin/sh`.
    pub fn shell_program(&self) -> String {
        self.shell
            .clone()
            .or_else(|| std::env::var("SHELL").ok().filter(|s| !s.is_empty()))
            .unwrap_or_else(|| "/bin/sh".into())
    }
}

/// The `session` section.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionConfig {
    /// Save tabs, windows and buffers on quit and restore them on start.
    pub restore_session: bool,
    /// Where the session file lives (`None`: see `session::state_dir`).
    pub state_dir: Option<PathBuf>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self { restore_session: true, state_dir: None }
    }
}

/// The keymaps of the panels. The global keymap and the modes' keymaps are
/// in [`Modes`].
pub struct Keymaps {
    /// Keys while the buffer picker (C-x b) is open; single keys only.
    pub picker: Keymap,
    /// Keys while the search panel (C-s) is open; single keys only.
    pub search: Keymap,
    /// Keys while the settings editor (s-,) is open; single keys only.
    pub settings: Keymap,
}

impl Keymaps {
    /// The keymaps from the built-in configuration only.
    pub fn builtin() -> Self {
        Config::builtin().keymaps
    }
}

/// How the editor looks.
#[derive(Clone, Debug, PartialEq)]
pub struct Appearance {
    pub fonts: Fonts,
    /// The configured themes (`theme`): one for a light and one for a dark system appearance.
    pub themes: Themes,
    /// Every theme defined, sorted by name (for `load-theme`).
    pub available: Vec<Theme>,
}

impl Appearance {
    /// The theme called `name`, if it is defined.
    pub fn theme_named(&self, name: &str) -> Option<&Theme> {
        self.available.iter().find(|t| t.name == name)
    }
}

pub struct Config {
    /// Global settings and keymap, major and minor modes.
    pub modes: Modes,
    pub appearance: Appearance,
    pub keymaps: Keymaps,
    pub terminal: TerminalConfig,
    pub session: SessionConfig,
    /// The user file that was loaded, if any.
    pub user_file: Option<PathBuf>,
    /// Everything that was wrong with the configuration (each entry skipped).
    pub warnings: Vec<String>,
}

impl Config {
    /// Only the built-in configuration.
    pub fn builtin() -> Self {
        Self::from_evaluated(&[builtin_layer()])
    }

    /// The built-in configuration plus the user's files, if there are any.
    pub fn load() -> Self {
        Self::load_from(user_config_path())
    }

    /// The built-in configuration plus the init file at `init` and the
    /// settings file next to it, those that exist.
    pub fn load_from(init: Option<PathBuf>) -> Self {
        let Some(init) = init else { return Self::builtin() };
        let mut user = Vec::new();
        let mut problems = Vec::new();
        let mut user_file = None;
        for path in [init.clone(), settings_path(&init)] {
            match read_layer(&path) {
                Ok(Some(l)) => {
                    user.push(l);
                    if path == init {
                        user_file = Some(init.clone());
                    }
                }
                Ok(None) => {}
                Err(e) => problems.push(e),
            }
        }
        if let Some(old) = init.parent().map(|d| d.join("config.json")).filter(|p| p.exists()) {
            problems.push(format!(
                "{}: JSON configuration is no longer read; move it to {} (see the README)",
                super::files::abbreviate(&old),
                super::files::abbreviate(&init)
            ));
        }
        let mut layers = vec![builtin_layer()];
        layers.extend(&user);
        let mut c = Self::from_evaluated(&layers);
        c.user_file = user_file;
        c.warnings.splice(0..0, problems);
        c
    }

    /// The built-in configuration plus `src` (a user file's text), for tests.
    pub fn with_user(src: &str) -> Self {
        Self::from_layers(&[("user", src)])
    }

    /// The built-in configuration plus each `(label, source)` in order (later
    /// layers override earlier ones).
    pub fn from_layers(layers: &[(&str, &str)]) -> Self {
        let user: Vec<Layer> = layers.iter().map(|(label, src)| eval::eval(label, src, None)).collect();
        let mut all = vec![builtin_layer()];
        all.extend(&user);
        Self::from_evaluated(&all)
    }

    /// Apply evaluated layers in order.
    pub fn from_evaluated(layers: &[&Layer]) -> Self {
        let mut warnings = Vec::new();
        let mut settings = Settings::default();
        let (mut family, mut ui_family) = ("monospace".to_string(), "system-ui".to_string());
        let (mut size, mut line_height) = (14.0_f32, 1.5_f32);
        let mut ui_size: Option<f32> = None;
        let mut theme_choice = RawThemeChoice::One(DEFAULT_THEME.to_string());
        let mut raw_themes = RawThemes::new();
        let mut maps: BTreeMap<&'static str, BTreeMap<String, String>> =
            KEYMAP_NAMES.iter().map(|n| (*n, BTreeMap::new())).collect();
        let mut modes_acc = ModesAcc::default();
        let mut terminal = TerminalConfig::default();
        let mut session = SessionConfig::default();

        for layer in layers {
            warnings.extend(layer.warnings.iter().cloned());
        }
        let patches = layers.iter().flat_map(|l| l.patches.iter().map(move |p| (l.label.as_str(), p)));
        for (label, patch) in patches {
            let raw = match RawConfig::deserialize(&patch.value) {
                Ok(r) => r,
                Err(e) => {
                    warnings.push(format!("{label}: {}: {e}", patch.what));
                    continue;
                }
            };
            if let Some(v) = raw.editor.fill_column {
                settings.fill_column = v;
            }
            match raw.editor.tab_width {
                Some(0) => warnings.push(format!("{label}: editor.tab_width must be at least 1")),
                Some(v) => settings.tab_width = v,
                None => {}
            }
            let font = raw.font;
            for (field, value) in [("family", font.family), ("ui_family", font.ui_family)] {
                match value {
                    Some(v) if v.trim().is_empty() => warnings.push(format!("{label}: font.{field} is empty")),
                    Some(v) => *(if field == "family" { &mut family } else { &mut ui_family }) = v,
                    None => {}
                }
            }
            match font.size {
                Some(v) if !(4.0..=200.0).contains(&v) => {
                    warnings.push(format!("{label}: font.size must be between 4 and 200"))
                }
                Some(v) => size = v,
                None => {}
            }
            match font.ui_size {
                Some(v) if !(4.0..=200.0).contains(&v) => {
                    warnings.push(format!("{label}: font.ui_size must be between 4 and 200"))
                }
                Some(v) => ui_size = Some(v),
                None => {}
            }
            match font.line_height {
                Some(v) if !(1.0..=4.0).contains(&v) => {
                    warnings.push(format!("{label}: font.line_height must be between 1 and 4 (a multiple of the size)"))
                }
                Some(v) => line_height = v,
                None => {}
            }
            if let Some(t) = raw.theme {
                theme_choice = t;
            }
            for (name, entries) in raw.themes {
                raw_themes.entry(name).or_default().extend(entries);
            }
            for (name, entries) in raw.keymaps {
                let Some(map) = KEYMAP_NAMES.iter().find(|n| **n == name).and_then(|n| maps.get_mut(n)) else {
                    warnings.push(format!(
                        "{label}: unknown keymap \"{name}\" (expected one of {})",
                        KEYMAP_NAMES.join(", ")
                    ));
                    continue;
                };
                merge_keymap(map, entries, &format!("{label}: keymaps.{name}"), &mut warnings);
            }
            merge_modes(&mut modes_acc, raw.modes, label, &mut warnings);
            merge_terminal(&mut terminal, raw.terminal, label, &mut warnings);
            if let Some(v) = raw.session.restore_session {
                session.restore_session = v;
            }
            if let Some(v) = raw.session.state_dir {
                session.state_dir = v;
            }
        }

        let keymaps = Keymaps {
            picker: build_keymap(
                "keymaps.picker",
                &maps["picker"],
                &|a| PICKER_ACTIONS.iter().find(|x| **x == a).copied(),
                true,
                &mut warnings,
            ),
            search: build_keymap(
                "keymaps.search",
                &maps["search"],
                &|a| SEARCH_ACTIONS.iter().find(|x| **x == a).copied(),
                true,
                &mut warnings,
            ),
            settings: build_keymap(
                "keymaps.settings",
                &maps["settings"],
                &|a| SETTINGS_ACTIONS.iter().find(|x| **x == a).copied(),
                true,
                &mut warnings,
            ),
        };
        let modes = build_modes(modes_acc, settings, &maps["global"], &mut warnings);
        terminal.keymap = build_keymap("keymaps.terminal", &maps["terminal"], &resolve_global, false, &mut warnings);
        let fallback = builtin_default_theme();
        let available = theme::resolve_all(&raw_themes, &fallback, &mut warnings);
        let mut pick = |name: &str| {
            available.iter().find(|t| t.name == name).cloned().unwrap_or_else(|| {
                if !raw_themes.contains_key(name) {
                    let known: Vec<&str> = raw_themes.keys().map(String::as_str).collect();
                    warnings.push(format!("theme: unknown theme \"{name}\" (known: {})", known.join(", ")));
                }
                fallback.clone()
            })
        };
        let themes = match theme_choice {
            RawThemeChoice::One(n) => {
                let t = pick(&n);
                Themes { light: t.clone(), dark: t }
            }
            RawThemeChoice::ByAppearance { light, dark } => Themes { light: pick(&light), dark: pick(&dark) },
        };
        let fonts = Fonts::new(&family, &ui_family, size, ui_size, line_height);
        let appearance = Appearance { fonts, themes, available };
        Self { modes, appearance, keymaps, terminal, session, user_file: None, warnings }
    }
}

/// Merge one layer's `terminal` section.
fn merge_terminal(t: &mut TerminalConfig, raw: RawTerminal, label: &str, warnings: &mut Vec<String>) {
    let ctx = format!("{label}: terminal");
    if let Some(v) = raw.shell {
        t.shell = v.filter(|s| !s.trim().is_empty());
    }
    if let Some(v) = raw.shell_args {
        t.shell_args = v;
    }
    match raw.term {
        Some(v) if v.trim().is_empty() => warnings.push(format!("{ctx}.term is empty")),
        Some(v) => t.term = v,
        None => {}
    }
    if let Some(v) = raw.scrollback_lines {
        t.scrollback_lines = v;
    }
    match raw.backend.as_deref() {
        Some("daemon") => t.backend = PtyBackendKind::Daemon,
        Some("local") => t.backend = PtyBackendKind::Local,
        Some(other) => warnings.push(format!("{ctx}.backend: \"{other}\" (expected \"daemon\" or \"local\")")),
        None => {}
    }
    match raw.replay_bytes {
        Some(v) if v > 256 << 20 => warnings.push(format!("{ctx}.replay_bytes must be at most 268435456")),
        Some(v) => t.replay_bytes = v,
        None => {}
    }
    if let Some(v) = raw.socket {
        t.socket = v;
    }
    if let Some(secs) = raw.daemon_idle_exit {
        t.daemon_idle_exit = (secs > 0).then(|| Duration::from_secs(secs));
    }
    if let Some(keys) = raw.escape_keys {
        t.escape_keys = keys
            .iter()
            .filter_map(|k| match normalize_keys(k) {
                Ok(n) if !n.contains(' ') => Some(n),
                Ok(_) => {
                    warnings.push(format!("{ctx}.escape_keys: \"{k}\": only single keys can be escape keys"));
                    None
                }
                Err(e) => {
                    warnings.push(format!("{ctx}.escape_keys: {e}"));
                    None
                }
            })
            .collect();
    }
}

/// The built-in configuration, evaluated once.
pub fn builtin_layer() -> &'static Layer {
    static BUILTIN: OnceLock<Layer> = OnceLock::new();
    BUILTIN.get_or_init(|| {
        let l = eval::eval("built-in config", DEFAULT_SCM, None);
        assert!(!l.failed, "built-in config: {:#?}", l.warnings);
        l
    })
}

/// The built-in themes as they are written (before inheriting).
fn builtin_raw_themes() -> RawThemes {
    let doc = eval::document([builtin_layer()]);
    RawThemes::deserialize(&doc["themes"]).expect("built-in themes are well-formed")
}

/// The default theme exactly as the built-in configuration defines it: the
/// base for themes that don't name one to inherit.
fn builtin_default_theme() -> Theme {
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME
        .get_or_init(|| {
            Theme::complete(DEFAULT_THEME, &builtin_raw_themes()[DEFAULT_THEME])
                .unwrap_or_else(|e| panic!("built-in {DEFAULT_THEME}: {e}"))
        })
        .clone()
}

/// Evaluate the file at `path`: `None` if it doesn't exist.
pub(crate) fn read_layer(path: &std::path::Path) -> Result<Option<Layer>, String> {
    let label = super::files::abbreviate(path);
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(eval::eval(&label, &text, Some(path)))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{label}: {e}")),
    }
}

fn resolve_global(name: &str) -> Option<&'static str> {
    commands::lookup(name).map(|c| c.name).or_else(|| PREFIX_ARG_COMMANDS.iter().find(|c| **c == name).copied())
}

/// Add a layer's entries to a keymap being merged (`null` unbinds).
fn merge_keymap(map: &mut BTreeMap<String, String>, entries: RawKeymap, ctx: &str, warnings: &mut Vec<String>) {
    for (keys, command) in entries {
        let keys = match normalize_keys(&keys) {
            Ok(k) => k,
            Err(e) => {
                warnings.push(format!("{ctx}: {e}"));
                continue;
            }
        };
        match command {
            Some(c) => map.insert(keys, c),
            None => map.remove(&keys),
        };
    }
}

fn build_keymap(
    ctx: &str,
    entries: &BTreeMap<String, String>,
    resolve: &dyn Fn(&str) -> Option<&'static str>,
    single_key: bool,
    warnings: &mut Vec<String>,
) -> Keymap {
    let mut km = Keymap::new();
    for (keys, command) in entries {
        if single_key && keys.contains(' ') {
            warnings.push(format!("{ctx}: \"{keys}\": only single keys can be bound here"));
            continue;
        }
        match resolve(command) {
            Some(c) => km.bind(keys, c),
            None => warnings.push(format!("{ctx}: \"{keys}\": unknown command \"{command}\"")),
        }
    }
    km
}

fn check_tab_width(v: Option<usize>, ctx: &str, warnings: &mut Vec<String>) -> Option<usize> {
    if v == Some(0) {
        warnings.push(format!("{ctx}: tab_width must be at least 1"));
        return None;
    }
    v
}

/// Merge one layer's `modes` section into what earlier layers defined.
fn merge_modes(acc: &mut ModesAcc, raw: RawModes, label: &str, warnings: &mut Vec<String>) {
    if raw.default_major_mode.is_some() {
        acc.default_major = raw.default_major_mode;
    }
    if raw.initial_major_mode.is_some() {
        acc.initial_major = raw.initial_major_mode;
    }
    for (name, m) in raw.major {
        let ctx = format!("{label}: modes.major.{name}");
        let a = acc.major.entry(name).or_default();
        if m.lighter.is_some() {
            a.lighter = m.lighter;
        }
        if let Some(p) = m.parent {
            a.parent = (!p.is_empty()).then_some(p);
        }
        if let Some(f) = m.files {
            a.files = f;
        }
        if let Some(i) = m.interpreters {
            a.interpreters = i;
        }
        if m.syntax.is_some() {
            a.syntax = m.syntax;
        }
        if m.editor.fill_column.is_some() {
            a.settings.fill_column = m.editor.fill_column;
        }
        if let Some(t) = check_tab_width(m.editor.tab_width, &format!("{ctx}.editor"), warnings) {
            a.settings.tab_width = Some(t);
        }
        if let Some(mm) = m.minor_modes {
            a.minor_modes = mm;
        }
        merge_keymap(&mut a.keymap, m.keymap, &format!("{ctx}.keymap"), warnings);
    }
    for (name, m) in raw.minor {
        if MinorMode::from_name(&name).is_none() {
            let known: Vec<&str> = MinorMode::ALL.iter().map(|m| m.name()).collect();
            warnings.push(format!("{label}: modes.minor: unknown minor mode \"{name}\" (known: {})", known.join(", ")));
            continue;
        }
        let a = acc.minor.entry(name.clone()).or_default();
        if m.lighter.is_some() {
            a.0 = m.lighter;
        }
        merge_keymap(&mut a.1, m.keymap, &format!("{label}: modes.minor.{name}.keymap"), warnings);
    }
}

/// Check the merged modes and build them, together with the global keymap
/// (which can bind major mode commands).
fn build_modes(
    mut acc: ModesAcc,
    settings: Settings,
    global: &BTreeMap<String, String>,
    warnings: &mut Vec<String>,
) -> Modes {
    acc.major.entry(FUNDAMENTAL.to_string()).or_default();
    acc.major.retain(|name, _| {
        let problem = if !name.ends_with("-mode") {
            Some("a major mode's name must end in \"-mode\"")
        } else if commands::lookup(name).is_some() || MinorMode::from_name(name).is_some() {
            Some("the name is already a command")
        } else {
            None
        };
        if let Some(p) = problem {
            warnings.push(format!("modes.major.{name}: {p} (mode skipped)"));
        }
        problem.is_none()
    });
    let names: HashMap<String, &'static str> = acc.major.keys().map(|n| (n.clone(), intern(n))).collect();
    let resolve = |c: &str| resolve_global(c).or_else(|| names.get(c).copied());

    let mut major: HashMap<&'static str, MajorMode> = HashMap::new();
    for (name, a) in acc.major {
        let ctx = format!("modes.major.{name}");
        let parent = a.parent.and_then(|p| match names.get(&p) {
            Some(n) if *n == name => {
                warnings.push(format!("{ctx}: a mode can't be its own parent"));
                None
            }
            Some(n) => Some(*n),
            None => {
                warnings.push(format!("{ctx}: unknown parent mode \"{p}\""));
                None
            }
        });
        let minor_modes = a
            .minor_modes
            .iter()
            .filter_map(|m| {
                let found = MinorMode::from_name(m);
                if found.is_none() {
                    warnings.push(format!("{ctx}.minor_modes: unknown minor mode \"{m}\""));
                }
                found
            })
            .collect();
        let name = names[&name];
        let lighter = a.lighter.unwrap_or_else(|| {
            let base = name.trim_end_matches("-mode");
            let mut c = base.chars();
            c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
        });
        let command = Command {
            name,
            run: modes::major_mode_command,
            flags: flags::NO_UNDO,
            doc: intern(&format!("Major mode: switch this buffer to {lighter} mode.")),
        };
        let keymap = build_keymap(&format!("{ctx}.keymap"), &a.keymap, &resolve, false, warnings);
        let syntax = a.syntax.filter(|l| {
            let known = l.is_empty() || syntax::language(l).is_some();
            if !known {
                warnings.push(format!(
                    "{ctx}.syntax: unknown language \"{l}\" (known: {})",
                    syntax::language_names().join(", ")
                ));
            }
            known
        });
        major.insert(
            name,
            MajorMode {
                name,
                lighter,
                parent,
                files: a.files,
                interpreters: a.interpreters,
                syntax,
                settings: a.settings,
                keymap,
                minor_modes,
                command,
            },
        );
    }
    // Break parent cycles at the link that closes them.
    let mut sorted: Vec<&'static str> = major.keys().copied().collect();
    sorted.sort_unstable();
    for start in sorted {
        let mut seen = vec![start];
        let mut cur = start;
        while let Some(p) = major[cur].parent {
            if seen.contains(&p) {
                warnings.push(format!("modes.major.{cur}: parent \"{p}\" makes a cycle (parent ignored)"));
                major.get_mut(cur).unwrap().parent = None;
                break;
            }
            seen.push(p);
            cur = p;
        }
    }

    let minor = MinorMode::ALL
        .iter()
        .map(|m| {
            let (lighter, keymap) = acc.minor.remove(m.name()).unwrap_or_default();
            MinorModeDef {
                lighter: lighter.unwrap_or_else(|| m.default_lighter().to_string()),
                keymap: build_keymap(&format!("modes.minor.{}.keymap", m.name()), &keymap, &resolve, false, warnings),
            }
        })
        .collect();

    let mut pick = |field: &str, v: Option<String>| -> &'static str {
        match v {
            None => FUNDAMENTAL,
            Some(n) => names.get(&n).copied().unwrap_or_else(|| {
                warnings.push(format!("modes.{field}: unknown major mode \"{n}\""));
                FUNDAMENTAL
            }),
        }
    };
    let default_major = pick("default_major_mode", acc.default_major);
    let initial_major = pick("initial_major_mode", acc.initial_major);
    let global = build_keymap("keymaps.global", global, &resolve, false, warnings);
    Modes { settings, global, major, minor, default_major: intern(default_major), initial_major: intern(initial_major) }
}

/// Check a key sequence and write it in canonical form (`M-C-x` -> `C-M-x`).
fn normalize_keys(keys: &str) -> Result<String, String> {
    let parts: Vec<&str> = keys.split_whitespace().collect();
    if parts.is_empty() {
        return Err("empty key sequence".into());
    }
    let mut out = Vec::with_capacity(parts.len());
    for p in parts {
        let k = Key::parse(p);
        if !is_key_name(&k.name) {
            return Err(format!("\"{keys}\": \"{p}\" is not a key (use Emacs notation like C-x, M-<left>, RET)"));
        }
        out.push(k.canonical());
    }
    Ok(out.join(" "))
}

fn is_key_name(name: &str) -> bool {
    name.chars().count() == 1
        || matches!(name, "SPC" | "RET" | "TAB" | "DEL" | "ESC")
        || (name.len() > 2 && name.starts_with('<') && name.ends_with('>'))
}

/// The settings editor's file that goes with the init file at `init`.
pub fn settings_path(init: &std::path::Path) -> PathBuf {
    init.with_file_name(SETTINGS_FILE)
}

/// Where the user's init file is read from.
pub fn user_config_path() -> Option<PathBuf> {
    user_config_path_in(&|name| std::env::var_os(name), cfg!(windows))
}

fn user_config_path_in(env: &dyn Fn(&str) -> Option<std::ffi::OsString>, windows: bool) -> Option<PathBuf> {
    let var = |name: &str| env(name).filter(|p| !p.is_empty()).map(PathBuf::from);
    if let Some(p) = var("YMACS_CONFIG") {
        return Some(p);
    }
    let base = var("XDG_CONFIG_HOME")
        .or_else(|| if windows { var("APPDATA") } else { None })
        .or_else(|| var("HOME").map(|h| h.join(".config")))?;
    Some(base.join("ymacs").join("init.scm"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_config_is_clean() {
        let c = Config::builtin();
        assert!(c.warnings.is_empty(), "{:#?}", c.warnings);
        assert_eq!(c.modes.settings, Settings { fill_column: 70, tab_width: 4 });
        assert_eq!(c.modes.global.lookup(&["C-x".into(), "C-s".into()]), Some("save-buffer"));
        assert_eq!(c.keymaps.picker.lookup(&["RET".into()]), Some("picker-accept"));
        assert_eq!(c.keymaps.search.lookup(&["C-s".into()]), Some("search-next-or-history"));
        let t = &c.terminal;
        assert_eq!(t.escape_keys, ["C-x", "M-x", "C-c"]);
        assert_eq!((t.backend, t.scrollback_lines, t.replay_bytes), (PtyBackendKind::Daemon, 10_000, 2 << 20));
        assert_eq!((t.shell.as_deref(), t.term.as_str(), t.socket.as_deref()), (None, "xterm-256color", None));
        assert_eq!(t.keymap.lookup(&["C-c".into(), "C-j".into()]), Some("term-copy-mode"));
        assert_eq!(c.session, SessionConfig { restore_session: true, state_dir: None });
    }

    #[test]
    fn terminal_and_session_sections() {
        let user = r#"
            (setq terminal-shell "/bin/zsh" terminal-shell-args '("-l") terminal-backend 'local
                  terminal-scrollback-lines 500 terminal-escape-keys '("C-x" "M-C-z") terminal-socket "/tmp/s.sock"
                  restore-session #f session-state-dir "/tmp/state")
            (define-keys 'terminal "C-c C-j" #f "C-c j" 'term-copy-mode)
        "#;
        let c = Config::with_user(user);
        assert!(c.warnings.is_empty(), "{:#?}", c.warnings);
        let t = &c.terminal;
        assert_eq!(t.shell_program(), "/bin/zsh");
        assert_eq!(t.shell_args, ["-l"]);
        assert_eq!(t.backend, PtyBackendKind::Local);
        assert_eq!(t.scrollback_lines, 500);
        assert_eq!(t.replay_bytes, 2 << 20); // untouched default
        assert_eq!(t.escape_keys, ["C-x", "C-M-z"]);
        assert_eq!(t.socket.as_deref(), Some(std::path::Path::new("/tmp/s.sock")));
        assert_eq!(t.keymap.lookup(&["C-c".into(), "C-j".into()]), None);
        assert_eq!(t.keymap.lookup(&["C-c".into(), "j".into()]), Some("term-copy-mode"));
        assert_eq!(c.session, SessionConfig { restore_session: false, state_dir: Some("/tmp/state".into()) });

        // #f goes back to the default.
        let reset = Config::from_layers(&[("a", user), ("b", "(setq terminal-shell #f terminal-socket #f)")]);
        assert_eq!(reset.terminal.shell, None);
        assert_eq!(reset.terminal.socket, None);

        let bad = r#"(setq terminal-backend 'ssh terminal-escape-keys '("C-x C-y" "Ctrl-q") terminal-term ""
                            terminal-replay-bytes 999999999999)"#;
        let c = Config::with_user(bad);
        let w = c.warnings.join("\n");
        assert!(w.contains("terminal.backend"), "{w}");
        assert!(w.contains("only single keys can be escape keys"), "{w}");
        assert!(w.contains("\"Ctrl-q\" is not a key"), "{w}");
        assert!(w.contains("terminal.term is empty"), "{w}");
        assert!(w.contains("replay_bytes"), "{w}");
        assert_eq!(c.terminal.backend, PtyBackendKind::Daemon);
        assert!(c.terminal.escape_keys.is_empty());
    }

    #[test]
    fn daemon_idle_exit() {
        use std::time::Duration;
        assert_eq!(Config::builtin().terminal.daemon_idle_exit, Some(Duration::from_secs(60)));
        let c = Config::with_user("(setq terminal-daemon-idle-exit 5)");
        assert!(c.warnings.is_empty(), "{:#?}", c.warnings);
        assert_eq!(c.terminal.daemon_idle_exit, Some(Duration::from_secs(5)));
        // 0: never exit.
        let c = Config::with_user("(setq terminal-daemon-idle-exit 0)");
        assert_eq!(c.terminal.daemon_idle_exit, None);
        // Not a number of seconds: the entry is reported.
        let c = Config::with_user("(setq terminal-daemon-idle-exit -3)");
        assert!(c.warnings.iter().any(|w| w.contains("(setq terminal-daemon-idle-exit)")), "{:#?}", c.warnings);
        assert_eq!(c.terminal.daemon_idle_exit, Some(Duration::from_secs(60)));
    }

    #[test]
    fn every_action_has_a_default_key() {
        let c = Config::builtin();
        for a in PICKER_ACTIONS {
            assert!(!c.keymaps.picker.keys_for(a).is_empty(), "picker action {a} is unbound");
        }
        for a in SEARCH_ACTIONS {
            assert!(!c.keymaps.search.keys_for(a).is_empty(), "search action {a} is unbound");
        }
        for a in SETTINGS_ACTIONS {
            assert!(!c.keymaps.settings.keys_for(a).is_empty(), "settings action {a} is unbound");
        }
    }

    #[test]
    fn user_layer_overrides_adds_and_unbinds() {
        let user = r#"
            (setq fill-column 80)
            (global-set-key "C-z" 'undo)
            (global-unset-key "C-x C-c")
            (define-key 'global "M-C-f" 'forward-word)
            (define-key 'search "C-j" 'search-accept)
        "#;
        let c = Config::with_user(user);
        assert!(c.warnings.is_empty(), "{:#?}", c.warnings);
        assert_eq!(c.modes.settings, Settings { fill_column: 80, tab_width: 4 });
        let g = &c.modes.global;
        assert_eq!(g.lookup(&["C-z".into()]), Some("undo"));
        assert_eq!(g.lookup(&["C-x".into(), "C-c".into()]), None);
        assert_eq!(g.lookup(&["C-M-f".into()]), Some("forward-word")); // normalized from M-C-f
        assert_eq!(g.lookup(&["C-f".into()]), Some("forward-char")); // untouched default
        assert_eq!(c.keymaps.search.lookup(&["C-j".into()]), Some("search-accept"));
    }

    #[test]
    fn problems_are_reported_and_skipped() {
        let user = r#"
            (define-keys 'global "C-z" 'no-such-command "Ctrl-x" 'undo "C-t" 'undo)
            (define-keys 'search "C-x C-x" 'search-accept)
            (define-keys 'minibuffer)
        "#;
        let c = Config::with_user(user);
        let w = c.warnings.join("\n");
        assert!(w.contains("unknown command \"no-such-command\""), "{w}");
        assert!(w.contains("\"Ctrl-x\" is not a key"), "{w}");
        assert!(w.contains("only single keys"), "{w}");
        assert!(w.contains("unknown keymap \"minibuffer\""), "{w}");
        assert_eq!(c.modes.global.lookup(&["C-t".into()]), Some("undo")); // valid entries still apply

        // A Scheme error skips the rest of the file, keeping what ran before it.
        let broken =
            Config::with_user("(global-set-key \"C-z\" 'undo)\n(setq fill-column (car 5))\n(setq tab-width 8)");
        assert!(broken.warnings[0].starts_with("user:2:"), "{:#?}", broken.warnings);
        assert_eq!(broken.modes.global.lookup(&["C-z".into()]), Some("undo"));
        assert_eq!(broken.modes.settings.tab_width, 4);
        assert_eq!(broken.modes.global.lookup(&["C-f".into()]), Some("forward-char"));
        let unclosed = Config::with_user("(setq fill-column 80");
        assert!(unclosed.warnings[0].starts_with("user:"), "{:#?}", unclosed.warnings);
    }

    #[test]
    fn builtin_appearance() {
        let a = Config::builtin().appearance;
        assert_eq!(a.fonts.family, theme::PLATFORM_MONOSPACE);
        assert_eq!((a.fonts.size, a.fonts.line_height), (14.0, 21.0));
        assert_eq!(a.themes.dark, builtin_default_theme());
        assert_eq!(a.themes.dark.background, 0x1e1f22ff);
        assert_eq!(a.themes.dark.overlay, 0x0b0c0f80);
        // The built-in light theme is complete on its own.
        for (name, entries) in &builtin_raw_themes() {
            Theme::complete(name, entries).unwrap_or_else(|e| panic!("built-in theme {name}: {e}"));
        }
    }

    #[test]
    fn user_fonts_and_themes() {
        let user = r##"
            (setq font-family "Iosevka" font-size 16 line-height 1.25
                  theme '((light . paper) (dark . ymacs-dark)))
            (define-theme 'ymacs-dark #:cursor "#ff0000")
            (define-theme 'paper #:inherits 'ymacs-light #:background "#fffff0")
        "##;
        let c = Config::with_user(user);
        assert!(c.warnings.is_empty(), "{:#?}", c.warnings);
        let a = &c.appearance;
        assert_eq!((a.fonts.family.as_str(), a.fonts.ui_family.as_str()), ("Iosevka", ".SystemUIFont"));
        assert_eq!((a.fonts.size, a.fonts.line_height), (16.0, 20.0));
        // A single color overridden in a built-in theme; the rest kept.
        assert_eq!(a.themes.dark.cursor, 0xff0000ff);
        assert_eq!(a.themes.dark.background, 0x1e1f22ff);
        // A new theme built on the light one.
        assert_eq!(a.themes.light.background, 0xfffff0ff);
        assert_eq!(a.themes.light.foreground, 0x383a42ff);
        assert_ne!(a.themes.get(false).background, a.themes.get(true).background);
    }

    #[test]
    fn every_theme_is_available_by_name() {
        let c = Config::with_user("(setq font-ui-size 12) (define-theme 'paper #:inherits 'ymacs-light)");
        assert!(c.warnings.is_empty(), "{:#?}", c.warnings);
        let a = &c.appearance;
        let names: Vec<&str> = a.available.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["paper", "solarized-dark", "solarized-light", "ymacs-dark", "ymacs-light"]);
        assert_eq!(a.theme_named("paper").unwrap().background, a.theme_named("ymacs-light").unwrap().background);
        assert!(a.theme_named("solarized-dark").unwrap().is_dark());
        assert!(!a.theme_named("paper").unwrap().is_dark());
        assert_eq!(a.themes.dark.name, "ymacs-dark");
        assert_eq!((a.fonts.size, a.fonts.ui_size), (14.0, 12.0));
        assert_eq!(Config::builtin().appearance.fonts.ui_size, 14.0, "follows font.size by default");
    }

    #[test]
    fn user_config_path_precedence() {
        let env = |vars: &'static [(&'static str, &'static str)]| {
            move |name: &str| vars.iter().find(|(k, _)| *k == name).map(|(_, v)| std::ffi::OsString::from(v))
        };
        let all =
            env(&[("YMACS_CONFIG", "/e.scm"), ("XDG_CONFIG_HOME", "/xdg"), ("APPDATA", "C:/AppData"), ("HOME", "/h")]);
        assert_eq!(user_config_path_in(&all, true), Some(PathBuf::from("/e.scm")));
        let no_override = env(&[("XDG_CONFIG_HOME", "/xdg"), ("APPDATA", "C:/AppData"), ("HOME", "/h")]);
        assert_eq!(user_config_path_in(&no_override, true), Some(PathBuf::from("/xdg/ymacs/init.scm")));
        let windows = env(&[("APPDATA", "C:/AppData"), ("HOME", "/h")]);
        assert_eq!(user_config_path_in(&windows, true), Some(PathBuf::from("C:/AppData/ymacs/init.scm")));
        assert_eq!(user_config_path_in(&windows, false), Some(PathBuf::from("/h/.config/ymacs/init.scm")));
        assert_eq!(
            settings_path(std::path::Path::new("/xdg/ymacs/init.scm")),
            PathBuf::from("/xdg/ymacs/settings.scm")
        );
        assert_eq!(user_config_path_in(&env(&[]), true), None);
    }

    #[test]
    fn theme_problems_are_reported_and_skipped() {
        let user = r##"
            (setq font-size 0 line-height 9 theme 'mine)
            (define-theme 'mine #:background "#123" #:foreground "blue" #:sparkle "#fff")
            (define-theme 'a #:inherits 'b)
            (define-theme 'b #:inherits 'a)
        "##;
        let c = Config::with_user(user);
        let w = c.warnings.join("\n");
        assert!(w.contains("font.size"), "{w}");
        assert!(w.contains("font.line_height"), "{w}");
        assert!(w.contains("\"blue\" is not a color"), "{w}");
        assert!(w.contains("unknown color \"sparkle\""), "{w}");
        assert!(w.contains("no color for foreground"), "{w}");
        assert_eq!((c.appearance.fonts.size, c.appearance.fonts.line_height), (14.0, 21.0));
        let t = &c.appearance.themes.dark;
        assert_eq!(t.background, 0x112233ff);
        assert_eq!(t.foreground, builtin_default_theme().foreground);

        let cyclic =
            Config::with_user("(setq theme 'a) (define-theme 'a #:inherits 'b) (define-theme 'b #:inherits 'a)");
        assert!(cyclic.warnings.iter().any(|w| w.contains("cycle")), "{:#?}", cyclic.warnings);
        let unknown = Config::with_user("(setq theme 'nope)");
        assert!(unknown.warnings.iter().any(|w| w.contains("unknown theme \"nope\"")), "{:#?}", unknown.warnings);
        assert_eq!(unknown.appearance.themes.dark, builtin_default_theme());
    }
}
