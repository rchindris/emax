use super::*;
use crate::engine::testutil::temp_dir;
use crate::engine::{Buffer, Config, Keymaps};

/// An editor with the settings editor open on `file` in a fresh directory
/// (written first if `contents` is given).
fn open(contents: Option<&str>) -> (Editor, Keymap, PathBuf) {
    let path = temp_dir("settings").join("config.json");
    if let Some(c) = contents {
        std::fs::write(&path, c).unwrap();
    }
    let mut ed = Editor::new(Buffer::from_str("test", ""));
    ed.open_settings_at(Some(path.clone()));
    (ed, Keymaps::builtin().settings, path)
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

fn file(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
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
    let (mut ed, km, _) = open(Some(r#"{ "editor": { "tab_width": 8 } }"#));
    typ(&mut ed, &km, "tab");
    // Title matches first (as a flat list), description matches after.
    let titles: Vec<&str> = s(&ed).matches.iter().map(|&i| SETTINGS[i].title).collect();
    assert_eq!(titles[0], "Editor: Tab Width");
    assert!(titles.contains(&"Session: Restore Session"), "{titles:?}"); // "tabs" in its description
    assert!(s(&ed).rows().iter().all(|r| matches!(r, Row::Setting(_))));
    // Terms in any order, matching the description too.
    keys(&mut ed, &km, "C-u");
    typ(&mut ed, &km, "survive daemon");
    assert!(s(&ed).matches.iter().any(|&i| SETTINGS[i].key == "terminal.backend"));
    keys(&mut ed, &km, "C-u");
    typ(&mut ed, &km, "@modified");
    let keys_: Vec<&str> = s(&ed).matches.iter().map(|&i| SETTINGS[i].key).collect();
    assert_eq!(keys_, ["editor.tab_width"]);
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
    let (mut ed, km, path) = open(Some(r#"{ "theme": "emax-light", "keymaps": { "global": { "C-z": "undo" } } }"#));
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
    assert!(s(&ed).is_modified("editor.fill_column"));
    // The rest of the file is untouched and keeps its order.
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.find("\"theme\"").unwrap() < text.find("\"keymaps\"").unwrap(), "{text}");
    let v = file(&path);
    assert_eq!(v["editor"]["fill_column"], 80);
    assert_eq!(v["keymaps"]["global"]["C-z"], "undo");
    // And the configuration reads it.
    let c = Config::load_from(Some(path.clone()));
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
    assert_eq!(sel, "terminal.shell");
    keys(&mut ed, &km, "RET");
    typ(&mut ed, &km, "/bin/zsh");
    keys(&mut ed, &km, "RET");
    assert_eq!(file(&path)["terminal"]["shell"], "/bin/zsh");
    keys(&mut ed, &km, "RET C-u RET");
    assert_eq!(file(&path)["terminal"]["shell"], Value::Null);
    let c = Config::load_from(Some(path.clone()));
    assert!(c.warnings.is_empty(), "{:#?}", c.warnings);
    assert_eq!(c.terminal.shell_args, ["-l", "-i"]);
    assert!(!c.session.restore_session);
}

#[test]
fn reset_removes_the_key_and_empty_parents() {
    let (mut ed, km, path) =
        open(Some(r#"{ "font": { "size": 18 }, "editor": { "tab_width": 2, "fill_column": 90 } }"#));
    typ(&mut ed, &km, "font size");
    keys(&mut ed, &km, "C-k");
    assert!(!s(&ed).is_modified("font.size"));
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
    assert_eq!(s(&ed).selected_setting().unwrap().key, "font.family");
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
    let (mut ed, km, path) = open(Some("{ not json"));
    assert!(s(&ed).file_error.is_some());
    typ(&mut ed, &km, "restore session");
    keys(&mut ed, &km, "RET");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
    assert!(s(&ed).message.as_deref().unwrap_or("").contains("settings-open-json"));
    // Opening the file closes the editor and visits it.
    keys(&mut ed, &km, "C-o");
    assert!(ed.settings.is_none());
    assert_eq!(ed.buf.rope.to_string(), "{ not json");
}

#[test]
fn open_json_creates_the_file() {
    let (mut ed, km, path) = open(None);
    keys(&mut ed, &km, "C-o");
    assert!(ed.settings.is_none());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\n}\n");
    assert_eq!(ed.buf.path.as_deref().map(|p| p.canonicalize().unwrap()), Some(path.canonicalize().unwrap()));
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
