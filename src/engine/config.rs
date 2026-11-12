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
//! `font`, `theme` and `themes` set the look (see [`super::theme`]); a user
//! theme entry is merged color by color into the theme of the same name.
//!
//! Problems (bad JSON, unknown commands, malformed keys) never stop the
//! editor: the offending entry is skipped and reported in `warnings`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use super::buffers::PICKER_ACTIONS;
use super::commands;
use super::keymap::{Key, Keymap, PREFIX_ARG_COMMANDS};
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
    keymaps: BTreeMap<String, BTreeMap<String, Option<String>>>,
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

pub struct Keymaps {
    /// Editing keys (multi-key sequences like `C-x C-s` allowed).
    pub global: Keymap,
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
    pub settings: Settings,
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
                for (keys, command) in entries {
                    let keys = match normalize_keys(&keys) {
                        Ok(k) => k,
                        Err(e) => {
                            warnings.push(format!("{label}: keymaps.{name}: {e}"));
                            continue;
                        }
                    };
                    match command {
                        Some(c) => map.insert(keys, c),
                        None => map.remove(&keys),
                    };
                }
            }
        }

        let mut build = |name: &str, resolve: &dyn Fn(&str) -> Option<&'static str>, single_key: bool| {
            let mut km = Keymap::new();
            for (keys, command) in &maps[name] {
                if single_key && keys.contains(' ') {
                    warnings.push(format!("keymaps.{name}: \"{keys}\": only single keys can be bound here"));
                    continue;
                }
                match resolve(command) {
                    Some(c) => km.bind(keys, c),
                    None => warnings.push(format!("keymaps.{name}: \"{keys}\": unknown command \"{command}\"")),
                }
            }
            km
        };
        let keymaps = Keymaps {
            global: build("global", &resolve_global, false),
            picker: build("picker", &|a| PICKER_ACTIONS.iter().find(|x| **x == a).copied(), true),
            search: build("search", &|a| SEARCH_ACTIONS.iter().find(|x| **x == a).copied(), true),
        };
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
        Self { settings, appearance, keymaps, user_file: None, warnings }
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
        assert_eq!(c.settings, Settings { fill_column: 70, tab_width: 4 });
        assert_eq!(c.keymaps.global.lookup(&["C-x".into(), "C-s".into()]), Some("save-buffer"));
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
        assert_eq!(c.settings, Settings { fill_column: 80, tab_width: 4 });
        let g = &c.keymaps.global;
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
        assert_eq!(c.keymaps.global.lookup(&["C-t".into()]), Some("undo")); // valid entries still apply

        let broken = Config::from_layers(&[("built-in", DEFAULT_JSON), ("user", "{ not json")]);
        assert!(broken.warnings[0].starts_with("user:"));
        assert_eq!(broken.keymaps.global.lookup(&["C-f".into()]), Some("forward-char"));
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
