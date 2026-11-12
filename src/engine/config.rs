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
//! Problems (bad JSON, unknown commands, malformed keys) never stop the
//! editor: the offending entry is skipped and reported in `warnings`.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use serde::Deserialize;

use super::buffers::PICKER_ACTIONS;
use super::commands::{self, flags, Command};
use super::keymap::{Key, Keymap, PREFIX_ARG_COMMANDS};
use super::modes::{self, FUNDAMENTAL, LocalSettings, MajorMode, MinorMode, MinorModeDef, Modes, intern};
use super::search::SEARCH_ACTIONS;
use super::theme::{self, DEFAULT_THEME, Fonts, RawThemes, Theme, Themes};

/// The built-in configuration.
pub const DEFAULT_JSON: &str = include_str!("../../config/default.json");

/// The keymaps a configuration can define.
pub const KEYMAP_NAMES: [&str; 3] = ["global", "picker", "search"];

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

/// The keymaps of the panels. The global keymap and the modes' keymaps are
/// in [`Modes`].
pub struct Keymaps {
    /// Keys while the buffer picker (C-x b) is open; single keys only.
    pub picker: Keymap,
    /// Keys while the search panel (C-s) is open; single keys only.
    pub search: Keymap,
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
        let Some(path) = user_config_path() else { return Self::builtin() };
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
                Some(v) if !(4.0..=200.0).contains(&v) => warnings.push(format!("{label}: font.size must be between 4 and 200")),
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
                    warnings.push(format!("{label}: unknown keymap \"{name}\" (expected one of {})", KEYMAP_NAMES.join(", ")));
                    continue;
                };
                merge_keymap(map, entries, &format!("{label}: keymaps.{name}"), &mut warnings);
            }
            merge_modes(&mut modes_acc, raw.modes, label, &mut warnings);
        }

        let keymaps = Keymaps {
            picker: build_keymap("keymaps.picker", &maps["picker"], &|a| PICKER_ACTIONS.iter().find(|x| **x == a).copied(), true, &mut warnings),
            search: build_keymap("keymaps.search", &maps["search"], &|a| SEARCH_ACTIONS.iter().find(|x| **x == a).copied(), true, &mut warnings),
        };
        let modes = build_modes(modes_acc, settings, &maps["global"], &mut warnings);
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
        Self { modes, appearance, keymaps, user_file: None, warnings }
    }
}

/// The default theme exactly as the built-in configuration defines it: the
/// base for themes that don't name one to inherit.
fn builtin_default_theme() -> Theme {
    let raw: RawConfig = serde_json::from_str(DEFAULT_JSON).expect("built-in config is valid JSON");
    Theme::complete(&raw.themes[DEFAULT_THEME]).unwrap_or_else(|e| panic!("built-in {DEFAULT_THEME}: {e}"))
}

fn resolve_global(name: &str) -> Option<&'static str> {
    commands::lookup(name)
        .map(|c| c.name)
        .or_else(|| PREFIX_ARG_COMMANDS.iter().find(|c| **c == name).copied())
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
fn build_modes(mut acc: ModesAcc, settings: Settings, global: &BTreeMap<String, String>, warnings: &mut Vec<String>) -> Modes {
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
        major.insert(
            name,
            MajorMode { name, lighter, parent, files: a.files, interpreters: a.interpreters, settings: a.settings, keymap, minor_modes, command },
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

        let cyclic = Config::from_layers(&[("built-in", DEFAULT_JSON), ("user", r#"{"theme": "a", "themes": {"a": {"inherits": "b"}, "b": {"inherits": "a"}}}"#)]);
        assert!(cyclic.warnings.iter().any(|w| w.contains("cycle")), "{:#?}", cyclic.warnings);
        let unknown = Config::from_layers(&[("built-in", DEFAULT_JSON), ("user", r#"{"theme": "nope"}"#)]);
        assert!(unknown.warnings.iter().any(|w| w.contains("unknown theme \"nope\"")), "{:#?}", unknown.warnings);
        assert_eq!(unknown.appearance.themes.dark, builtin_default_theme());
    }
}
