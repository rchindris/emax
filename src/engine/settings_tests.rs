use super::*;
use crate::engine::testutil::temp_dir;
use crate::engine::{Buffer, Config, Keymaps};

/// An editor with the settings editor open in a fresh directory, with
/// `settings` as the settings file if given. Returns the settings file's path.
fn open(settings: Option<&str>) -> (Editor, Keymap, PathBuf) {
    open_with(None, settings)
}

/// Likewise, with `init` as the init file if given.
fn open_with(init: Option<&str>, settings: Option<&str>) -> (Editor, Keymap, PathBuf) {
    let dir = temp_dir("settings");
    let path = dir.join("settings.scm");
    if let Some(c) = init {
        std::fs::write(dir.join("init.scm"), c).unwrap();
    }
    if let Some(c) = settings {
        std::fs::write(&path, c).unwrap();
    }
    let mut ed = Editor::new(Buffer::from_str("test", ""));
    ed.open_settings_at(Some(dir.join("init.scm")));
    (ed, Keymaps::builtin().settings, path)
}

/// The init file next to settings file `path`.
fn init_of(path: &Path) -> PathBuf {
    path.with_file_name("init.scm")
}

fn keys(ed: &mut Editor, km: &Keymap, keys: &str) {
    for k in keys.split_whitespace() {
        ed.settings_key(km, &Key::parse(k));
    }
}

fn typ(ed: &mut Editor, km: &Keymap, text: &str) {
    for c in text.chars() {
        let k = Key { name: c.to_string(), text: Some(c.to_string()), ..Default::default() };
        ed.settings_key(km, &k);
    }
}

fn s(ed: &Editor) -> &SettingsEditor {
    ed.settings.as_ref().expect("settings editor open")
}

/// What the settings file sets, as a document.
fn file(path: &Path) -> Value {
    let l = eval::eval("settings.scm", &std::fs::read_to_string(path).unwrap(), None);
    assert!(l.warnings.is_empty() && l.only_options(), "{:#?}", l.warnings);
    eval::document([&l])
}

#[test]
fn every_setting_has_a_default_and_a_section() {
    let (ed, _, _) = open(None);
    let st = s(&ed);
    for set in SETTINGS {
        assert!(SECTIONS.contains(&set.section), "{}", set.key);
        assert!(st.default_value(set.key).is_some(), "{} has no built-in value", set.key);
        assert!(!st.is_modified(set.key));
        if set.kind.is_choice() {
            assert!(st.options(set).contains(&st.display(set)), "{}: default not among options", set.key);
        }
    }
    assert_eq!(st.matches.len(), SETTINGS.len());
}

#[test]
fn search_filters_and_modified_filter() {
    let (mut ed, km, _) = open(Some("(setq tab-width 8)"));
    typ(&mut ed, &km, "tab");
    // Title matches first (as a flat list), description matches after.
    let titles: Vec<&str> = s(&ed).matches.iter().map(|&i| SETTINGS[i].title).collect();
    assert_eq!(titles[0], "Editor: Tab Width");
    assert!(titles.contains(&"Session: Restore Session"), "{titles:?}"); // "tabs" in its description
    assert!(s(&ed).rows().iter().all(|r| matches!(r, Row::Setting(_))));
    // Terms in any order, matching the description too.
    keys(&mut ed, &km, "C-u");
    typ(&mut ed, &km, "survive daemon");
    assert!(s(&ed).matches.iter().any(|&i| SETTINGS[i].key == "terminal-backend"));
    keys(&mut ed, &km, "C-u");
    typ(&mut ed, &km, "@modified");
    let keys_: Vec<&str> = s(&ed).matches.iter().map(|&i| SETTINGS[i].key).collect();
    assert_eq!(keys_, ["tab-width"]);
    assert_eq!(s(&ed).rows(), [Row::Header("Text Editor"), Row::Setting(0)]);
    // ESC clears the search first, then closes.
    keys(&mut ed, &km, "ESC");
    assert!(s(&ed).query.is_empty());
    assert_eq!(s(&ed).matches.len(), SETTINGS.len());
    keys(&mut ed, &km, "ESC");
    assert!(ed.settings.is_none());
}

#[test]
fn editing_a_number_writes_only_that_key() {
    let (mut ed, km, path) = open(Some("(setq theme 'ymacs-light)"));
    typ(&mut ed, &km, "fill column");
    keys(&mut ed, &km, "RET");
    assert_eq!(s(&ed).edit, Some(Edit::Text { input: "70".into(), error: None }));
    keys(&mut ed, &km, "DEL DEL");
    typ(&mut ed, &km, "abc");
    keys(&mut ed, &km, "RET");
    assert!(matches!(&s(&ed).edit, Some(Edit::Text { error: Some(e), .. }) if e.contains("whole number")));
    keys(&mut ed, &km, "C-u");
    typ(&mut ed, &km, "0");
    keys(&mut ed, &km, "RET");
    assert!(matches!(&s(&ed).edit, Some(Edit::Text { error: Some(e), .. }) if e.contains("between")));
    keys(&mut ed, &km, "C-u");
    typ(&mut ed, &km, "80");
    keys(&mut ed, &km, "RET");
    assert_eq!(s(&ed).edit, None);
    assert!(s(&ed).changed);
    assert!(s(&ed).is_modified("fill-column"));
    // The rest of the file is kept.
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("\n(setq fill-column 80)\n(setq theme 'ymacs-light)\n"), "{text}");
    // And the configuration reads it.
    let c = Config::load_from(Some(init_of(&path)));
    assert!(c.warnings.is_empty(), "{:#?}", c.warnings);
    assert_eq!(c.modes.settings.fill_column, 80);
}

#[test]
fn confirming_an_unchanged_field_does_not_modify() {
    let (mut ed, km, path) = open(None);
    typ(&mut ed, &km, "tab width");
    keys(&mut ed, &km, "RET RET");
    assert!(!s(&ed).changed);
    assert!(!path.exists());
}

#[test]
fn toggles_choices_lists_and_nulls() {
    let (mut ed, km, path) = open(None);
    // A checkbox toggles on RET.
    typ(&mut ed, &km, "restore session");
    keys(&mut ed, &km, "RET");
    assert_eq!(file(&path)["session"]["restore_session"], false);
    // A dropdown: RET opens it, C-n moves, RET picks.
    keys(&mut ed, &km, "C-u");
    typ(&mut ed, &km, "backend");
    keys(&mut ed, &km, "RET");
    assert_eq!(s(&ed).edit, Some(Edit::Choice { options: vec!["daemon".into(), "local".into()], index: 0 }));
    keys(&mut ed, &km, "C-n RET");
    assert_eq!(file(&path)["terminal"]["backend"], "local");
    // Themes come from the built-in and the user's themes.
    keys(&mut ed, &km, "C-u");
    typ(&mut ed, &km, "theme");
    let theme = s(&ed).selected_setting().unwrap();
    assert_eq!(theme.key, "theme");
    assert!(s(&ed).options(theme).contains(&"solarized-light".to_string()));
    // A list is comma-separated.
    keys(&mut ed, &km, "C-u");
    typ(&mut ed, &km, "shell arguments");
    keys(&mut ed, &km, "RET");
    typ(&mut ed, &km, "-l, -i,");
    keys(&mut ed, &km, "RET");
    assert_eq!(file(&path)["terminal"]["shell_args"], serde_json::json!(["-l", "-i"]));
    // A nullable text field: empty writes null.
    keys(&mut ed, &km, "C-u");
    typ(&mut ed, &km, "terminal shell");
    let sel = s(&ed).selected_setting().unwrap().key;
    assert_eq!(sel, "terminal-shell");
    keys(&mut ed, &km, "RET");
    typ(&mut ed, &km, "/bin/zsh");
    keys(&mut ed, &km, "RET");
    assert_eq!(file(&path)["terminal"]["shell"], "/bin/zsh");
    keys(&mut ed, &km, "RET C-u RET");
    assert_eq!(file(&path)["terminal"]["shell"], Value::Null);
    let c = Config::load_from(Some(init_of(&path)));
    assert!(c.warnings.is_empty(), "{:#?}", c.warnings);
    assert_eq!(c.terminal.shell_args, ["-l", "-i"]);
    assert!(!c.session.restore_session);
}

#[test]
fn reset_removes_the_key_and_empty_parents() {
    let (mut ed, km, path) = open(Some("(setq font-size 18 tab-width 2 fill-column 90)"));
    typ(&mut ed, &km, "font size");
    keys(&mut ed, &km, "C-k");
    assert!(!s(&ed).is_modified("font-size"));
    assert_eq!(s(&ed).display(s(&ed).selected_setting().unwrap()), "14");
    keys(&mut ed, &km, "C-u");
    typ(&mut ed, &km, "tab width");
    keys(&mut ed, &km, "C-k");
    assert_eq!(file(&path), serde_json::json!({ "editor": { "fill_column": 90 } }));
}

#[test]
fn navigation_moves_by_setting_and_section() {
    let (mut ed, km, _) = open(None);
    assert_eq!(s(&ed).selected_section(), Some("Text Editor"));
    keys(&mut ed, &km, "TAB");
    assert_eq!(s(&ed).selected_setting().unwrap().key, "font-family");
    keys(&mut ed, &km, "S-TAB S-TAB");
    assert_eq!(s(&ed).selected_section(), Some("Session"));
    keys(&mut ed, &km, "M-<");
    assert_eq!(s(&ed).selected, 0);
    keys(&mut ed, &km, "C-p C-n");
    assert_eq!(s(&ed).selected, 1);
    keys(&mut ed, &km, "M->");
    assert_eq!(s(&ed).selected, SETTINGS.len() - 1);
    // Moving away from a text field commits it.
    keys(&mut ed, &km, "M-<");
    keys(&mut ed, &km, "RET C-u");
    typ(&mut ed, &km, "72");
    keys(&mut ed, &km, "C-n");
    assert_eq!(s(&ed).selected, 1);
    assert_eq!(s(&ed).display(&SETTINGS[0]), "72");
}

#[test]
fn a_broken_file_is_not_overwritten() {
    for text in ["(setq fill-column", "(setq fill-column 80)\n(global-set-key \"C-z\" 'undo)\n"] {
        let (mut ed, km, path) = open(Some(text));
        assert!(s(&ed).file_error.is_some(), "{text}");
        typ(&mut ed, &km, "restore session");
        keys(&mut ed, &km, "RET");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        assert!(s(&ed).message.as_deref().unwrap_or("").contains("settings-open-init-file"));
        // Opening the init file closes the editor and visits the broken file instead.
        keys(&mut ed, &km, "C-o");
        assert!(ed.settings.is_none());
        assert_eq!(ed.buf.rope.to_string(), text);
    }
}

#[test]
fn open_init_file_creates_it() {
    let (mut ed, km, path) = open(None);
    keys(&mut ed, &km, "C-o");
    assert!(ed.settings.is_none());
    let init = init_of(&path);
    assert!(std::fs::read_to_string(&init).unwrap().starts_with(";;; init.scm"));
    assert_eq!(ed.buf.path.as_deref().map(|p| p.canonicalize().unwrap()), Some(init.canonicalize().unwrap()));
    // The new file is a valid, empty configuration.
    assert!(Config::load_from(Some(init)).warnings.is_empty());
}

#[test]
fn init_file_values_are_the_defaults() {
    let (mut ed, km, path) = open_with(Some("(setq fill-column 100)"), None);
    typ(&mut ed, &km, "fill column");
    let fill = s(&ed).selected_setting().unwrap();
    assert_eq!((s(&ed).display(fill), s(&ed).default_display(fill)), ("100".into(), "100".into()));
    assert!(!s(&ed).is_modified("fill-column"));
    // The settings file wins over the init file...
    keys(&mut ed, &km, "RET C-u");
    typ(&mut ed, &km, "80");
    keys(&mut ed, &km, "RET");
    assert_eq!(file(&path)["editor"]["fill_column"], 80);
    assert_eq!(Config::load_from(Some(init_of(&path))).modes.settings.fill_column, 80);
    // ...and resetting goes back to the init file's value.
    keys(&mut ed, &km, "C-k");
    assert_eq!(s(&ed).display(fill), "100");
    assert_eq!(Config::load_from(Some(init_of(&path))).modes.settings.fill_column, 100);
}

#[test]
fn super_comma_opens_settings() {
    let ks = Keymaps::builtin();
    let c = Config::builtin();
    assert_eq!(c.modes.global.lookup(&["s-,".into()]), Some("open-settings"));
    assert_eq!(ks.settings.lookup(&["s-,".into()]), Some("settings-close"));
}

#[test]
fn text_fields_take_the_global_editing_keys() {
    let (mut ed, km, path) = open(None);
    // In the search field.
    typ(&mut ed, &km, "fil column");
    keys(&mut ed, &km, "C-a M-f");
    typ(&mut ed, &km, "l");
    assert_eq!(s(&ed).query, "fill column");
    keys(&mut ed, &km, "C-a C-d C-e");
    assert_eq!(s(&ed).query, "ill column");
    keys(&mut ed, &km, "C-a");
    typ(&mut ed, &km, "f");
    // In a value being edited: the cursor starts at the end.
    keys(&mut ed, &km, "RET");
    assert_eq!(s(&ed).edit, Some(Edit::Text { input: "70".into(), error: None }));
    keys(&mut ed, &km, "C-b DEL");
    typ(&mut ed, &km, "8");
    keys(&mut ed, &km, "C-e");
    typ(&mut ed, &km, "0");
    assert_eq!(s(&ed).edit, Some(Edit::Text { input: "800".into(), error: None }));
    keys(&mut ed, &km, "C-a C-d C-d DEL C-f C-k");
    assert_eq!(s(&ed).edit, Some(Edit::Text { input: "0".into(), error: None }));
    // C-y yanks what M-DEL killed.
    keys(&mut ed, &km, "C-e M-DEL");
    assert_eq!(s(&ed).edit, Some(Edit::Text { input: "".into(), error: None }));
    typ(&mut ed, &km, "9");
    keys(&mut ed, &km, "C-a");
    ed.clipboard_in("1".into());
    keys(&mut ed, &km, "C-y RET");
    assert_eq!(file(&path)["editor"]["fill_column"], 19);
}
