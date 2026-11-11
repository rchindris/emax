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
//! Problems (bad JSON, unknown commands, malformed keys) never stop the
//! editor: the offending entry is skipped and reported in `warnings`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use super::buffers::PICKER_ACTIONS;
use super::commands;
use super::keymap::{Key, Keymap, PREFIX_ARG_COMMANDS};
use super::search::SEARCH_ACTIONS;

/// The built-in configuration.
pub const DEFAULT_JSON: &str = include_str!("../../config/default.json");

/// The keymaps a configuration can define.
pub const KEYMAP_NAMES: [&str; 3] = ["global", "picker", "search"];

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct RawConfig {
    editor: RawEditor,
    /// keymap name -> key sequence -> command (`null` unbinds).
    keymaps: BTreeMap<String, BTreeMap<String, Option<String>>>,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct RawEditor {
    fill_column: Option<usize>,
    tab_width: Option<usize>,
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

pub struct Config {
    pub settings: Settings,
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
        Self { settings, keymaps, user_file: None, warnings }
    }
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
}
