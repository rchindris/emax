//! Configuration: editor settings and keymaps, read from JSON.
//!
//! The built-in defaults live in `config/default.json` (compiled into the
//! binary). A user file, if present, is layered on top of them:
//!
//! - `$EMAX_CONFIG`, or
//! - `$XDG_CONFIG_HOME/emax/config.json`, or
//! - `~/.config/emax/config.json`.
//!
//! The user file only needs what it changes. Settings replace the defaults;
//! keymap entries are added to the default keymaps, replacing a binding for
//! the same keys, and an entry set to `null` removes a default binding:
//!
//! ```json
//! {
//!   "editor": { "fill_column": 80 },
//!   "keymaps": { "global": { "C-z": "undo", "C-x C-c": null } }
//! }
//! ```
//!
//! `modes` defines major modes and configures minor modes (see
//! [`super::modes`]). A user entry for an existing mode is merged into it
//! field by field; its keymap is merged like the keymaps above.
//!
//! `font`, `theme` and `themes` set the look (see [`super::theme`]); a user
//! theme entry is merged color by color into the theme of the same name.
//!
//! `terminal` configures terminal buffers and the PTY backend, `session`
//! session persistence (both merged field by field). The `terminal` keymap
//! holds the keys of terminal buffers (sequences starting with one of
//! `terminal.escape_keys` in char mode; any key in copy mode).
//!
//! Problems (bad JSON, unknown commands, malformed keys) never stop the
//! editor: the offending entry is skipped and reported in `warnings`.

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

/// The built-in configuration.
pub const DEFAULT_JSON: &str = include_str!("../../config/default.json");

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
    /// The daemon's socket (`$EMAX_PTYD_SOCKET` overrides it; `None`: `session::default_socket_path`).
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
    pub themes: Themes,
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
        Self::from_layers(&[("built-in config", DEFAULT_JSON)])
    }

    /// The built-in configuration plus the user's file, if there is one.
    pub fn load() -> Self {
        Self::load_from(user_config_path())
    }

    /// The built-in configuration plus the file at `path`, if it exists.
    pub fn load_from(path: Option<PathBuf>) -> Self {
        let Some(path) = path else { return Self::builtin() };
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let label = path.display().to_string();
                let mut c = Self::from_layers(&[("built-in config", DEFAULT_JSON), (&label, &text)]);
                c.user_file = Some(path);
                c
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::builtin(),
            Err(e) => {
                let mut c = Self::builtin();
                c.warnings.push(format!("{}: {e}", path.display()));
                c
            }
        }
    }

    /// Merge JSON layers in order (later layers override earlier ones).
    pub fn from_layers(layers: &[(&str, &str)]) -> Self {
        let mut warnings = Vec::new();
        let mut settings = Settings::default();
        let (mut family, mut ui_family) = ("monospace".to_string(), "system-ui".to_string());
        let (mut size, mut line_height) = (14.0_f32, 1.5_f32);
        let mut theme_choice = RawThemeChoice::One(DEFAULT_THEME.to_string());
        let mut raw_themes = RawThemes::new();
        let mut maps: BTreeMap<&'static str, BTreeMap<String, String>> =
            KEYMAP_NAMES.iter().map(|n| (*n, BTreeMap::new())).collect();
        let mut modes_acc = ModesAcc::default();
        let mut terminal = TerminalConfig::default();
        let mut session = SessionConfig::default();

        for (label, text) in layers {
            let raw: RawConfig = match serde_json::from_str(text) {
                Ok(r) => r,
                Err(e) => {
                    warnings.push(format!("{label}: {e} (file ignored)"));
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
        let mut pick = |name: &str| {
            theme::resolve(name, &raw_themes, &fallback, &mut warnings).unwrap_or_else(|| {
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
        let appearance = Appearance { fonts: Fonts::new(&family, &ui_family, size, line_height), themes };
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

/// The default theme exactly as the built-in configuration defines it: the
/// base for themes that don't name one to inherit.
fn builtin_default_theme() -> Theme {
    let raw: RawConfig = serde_json::from_str(DEFAULT_JSON).expect("built-in config is valid JSON");
    Theme::complete(&raw.themes[DEFAULT_THEME]).unwrap_or_else(|e| panic!("built-in {DEFAULT_THEME}: {e}"))
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

/// Where the user's configuration file is read from.
pub fn user_config_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("EMAX_CONFIG").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("emax").join("config.json"))
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
        let user = r#"{
            "terminal": { "shell": "/bin/zsh", "shell_args": ["-l"], "backend": "local", "scrollback_lines": 500,
                          "escape_keys": ["C-x", "M-C-z"], "socket": "/tmp/s.sock" },
            "session": { "restore_session": false, "state_dir": "/tmp/state" },
            "keymaps": { "terminal": { "C-c C-j": null, "C-c j": "term-copy-mode" } }
        }"#;
        let c = Config::from_layers(&[("built-in", DEFAULT_JSON), ("user", user)]);
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

        // `null` goes back to the default.
        let reset = Config::from_layers(&[
            ("built-in", DEFAULT_JSON),
            ("a", user),
            ("b", r#"{"terminal": {"shell": null, "socket": null}}"#),
        ]);
        assert_eq!(reset.terminal.shell, None);
        assert_eq!(reset.terminal.socket, None);

        let bad = r#"{ "terminal": { "backend": "ssh", "escape_keys": ["C-x C-y", "Ctrl-q"], "term": "", "replay_bytes": 999999999999 } }"#;
        let c = Config::from_layers(&[("built-in", DEFAULT_JSON), ("user", bad)]);
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
        let c =
            Config::from_layers(&[("built-in", DEFAULT_JSON), ("user", r#"{"terminal": {"daemon_idle_exit": 5}}"#)]);
        assert!(c.warnings.is_empty(), "{:#?}", c.warnings);
        assert_eq!(c.terminal.daemon_idle_exit, Some(Duration::from_secs(5)));
        // 0: never exit.
        let c =
            Config::from_layers(&[("built-in", DEFAULT_JSON), ("user", r#"{"terminal": {"daemon_idle_exit": 0}}"#)]);
        assert_eq!(c.terminal.daemon_idle_exit, None);
        // Not a number of seconds: the layer is reported.
        let c =
            Config::from_layers(&[("built-in", DEFAULT_JSON), ("user", r#"{"terminal": {"daemon_idle_exit": -3}}"#)]);
        assert!(!c.warnings.is_empty());
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
        let user = r#"{
            "editor": { "fill_column": 80 },
            "keymaps": {
                "global": { "C-z": "undo", "C-x C-c": null, "M-C-f": "forward-word" },
                "search": { "C-j": "search-accept" }
            }
        }"#;
        let c = Config::from_layers(&[("built-in", DEFAULT_JSON), ("user", user)]);
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
        let user = r#"{
            "keymaps": {
                "global": { "C-z": "no-such-command", "Ctrl-x": "undo", "C-t": "undo" },
                "search": { "C-x C-x": "search-accept" },
                "minibuffer": {}
            }
        }"#;
        let c = Config::from_layers(&[("built-in", DEFAULT_JSON), ("user", user)]);
        let w = c.warnings.join("\n");
        assert!(w.contains("unknown command \"no-such-command\""), "{w}");
        assert!(w.contains("\"Ctrl-x\" is not a key"), "{w}");
        assert!(w.contains("only single keys"), "{w}");
        assert!(w.contains("unknown keymap \"minibuffer\""), "{w}");
        assert_eq!(c.modes.global.lookup(&["C-t".into()]), Some("undo")); // valid entries still apply

        let broken = Config::from_layers(&[("built-in", DEFAULT_JSON), ("user", "{ not json")]);
        assert!(broken.warnings[0].starts_with("user:"));
        assert_eq!(broken.modes.global.lookup(&["C-f".into()]), Some("forward-char"));
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
        let raw: RawConfig = serde_json::from_str(DEFAULT_JSON).unwrap();
        for (name, entries) in &raw.themes {
            Theme::complete(entries).unwrap_or_else(|e| panic!("built-in theme {name}: {e}"));
        }
    }

    #[test]
    fn user_fonts_and_themes() {
        let user = r##"{
            "font": { "family": "Iosevka", "size": 16, "line_height": 1.25 },
            "theme": { "light": "paper", "dark": "emax-dark" },
            "themes": {
                "emax-dark": { "cursor": "#ff0000" },
                "paper": { "inherits": "emax-light", "background": "#fffff0" }
            }
        }"##;
        let c = Config::from_layers(&[("built-in", DEFAULT_JSON), ("user", user)]);
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
    fn theme_problems_are_reported_and_skipped() {
        let user = r##"{
            "font": { "size": 0, "line_height": 9 },
            "theme": "mine",
            "themes": {
                "mine": { "background": "#123", "foreground": "blue", "sparkle": "#fff" },
                "a": { "inherits": "b" },
                "b": { "inherits": "a" }
            }
        }"##;
        let c = Config::from_layers(&[("built-in", DEFAULT_JSON), ("user", user)]);
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

        let cyclic = Config::from_layers(&[
            ("built-in", DEFAULT_JSON),
            ("user", r#"{"theme": "a", "themes": {"a": {"inherits": "b"}, "b": {"inherits": "a"}}}"#),
        ]);
        assert!(cyclic.warnings.iter().any(|w| w.contains("cycle")), "{:#?}", cyclic.warnings);
        let unknown = Config::from_layers(&[("built-in", DEFAULT_JSON), ("user", r#"{"theme": "nope"}"#)]);
        assert!(unknown.warnings.iter().any(|w| w.contains("unknown theme \"nope\"")), "{:#?}", unknown.warnings);
        assert_eq!(unknown.appearance.themes.dark, builtin_default_theme());
    }
}
