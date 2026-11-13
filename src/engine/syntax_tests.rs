use super::*;
use crate::engine::config::DEFAULT_JSON;
use crate::engine::{Buffer, Config};

fn theme() -> Theme {
    Config::builtin().appearance.themes.dark
}

/// A fresh syntax tree for `text` in `lang`.
fn parsed(lang: &str, text: &str) -> (Rope, Syntax) {
    let rope = Rope::from_str(text);
    let mut s = Syntax::new(language(lang).unwrap());
    s.update(&rope);
    (rope, s)
}

fn sexp(s: &Syntax) -> String {
    s.tree().expect("parsed").root_node().to_sexp()
}

/// The color of the text at (`line`, `col`), if it has one.
fn color_at(colors: &[LineColors], line: usize, col: usize) -> Option<Rgba> {
    colors[line].iter().find(|(r, _)| r.contains(&col)).map(|(_, c)| *c)
}

#[test]
fn every_language_compiles() {
    for l in LANGUAGES {
        let (_, s) = parsed(l.name, "");
        assert!(s.tree().is_some(), "{}", l.name);
    }
    assert!(language("cobol").is_none());
}

#[test]
fn rust_highlights() {
    let text = "fn main() {\n    let s = \"hi\"; // note\n    println!(\"{s}\");\n}\n";
    let (rope, s) = parsed("rust", text);
    let t = theme();
    let c = s.highlight(&rope, 0..4, &t);
    assert_eq!(c.len(), 4);
    let col = |name: &str| t.syntax_color(name);
    assert_eq!(color_at(&c, 0, 0), col("keyword")); // fn
    assert_eq!(color_at(&c, 0, 3), col("function")); // main
    assert_eq!(color_at(&c, 1, 4), col("keyword")); // let
    assert_eq!(color_at(&c, 1, 13), col("string")); // "hi"
    assert_eq!(color_at(&c, 1, 20), col("comment"));
    assert_eq!(color_at(&c, 2, 4), col("function.macro")); // println
    assert_eq!(color_at(&c, 1, 8), None); // s: a plain variable
    // Just some lines: only those come back, at their own columns.
    let part = s.highlight(&rope, 1..2, &t);
    assert_eq!(part.len(), 1);
    assert_eq!(part[0], c[1]);
    // Out of range is clamped.
    assert_eq!(s.highlight(&rope, 3..100, &t).len(), 2);
}

#[test]
fn json_keys_are_not_strings() {
    let (rope, s) = parsed("json", "{ \"a\": \"b\", \"n\": 1, \"t\": true }");
    let t = theme();
    let c = s.highlight(&rope, 0..1, &t);
    assert_eq!(color_at(&c, 0, 3), t.syntax_color("string.special.key"));
    assert_ne!(t.syntax_color("string.special.key"), t.syntax_color("string"));
    assert_eq!(color_at(&c, 0, 8), t.syntax_color("string"));
    assert_eq!(color_at(&c, 0, 17), t.syntax_color("number"));
    assert_eq!(color_at(&c, 0, 26), t.syntax_color("constant.builtin"));
}

#[test]
fn multibyte_text_maps_bytes_to_columns() {
    let (rope, s) = parsed("rust", "let é = \"ünïcödé\"; // ✓\n");
    let t = theme();
    let c = s.highlight(&rope, 0..1, &t);
    // Columns are chars: the string starts at col 8 and is 9 chars long.
    let string = c[0].iter().find(|(_, col)| Some(*col) == t.syntax_color("string")).unwrap();
    assert_eq!(string.0, 8..17);
    assert_eq!(color_at(&c, 0, 19), t.syntax_color("comment"));
}

#[test]
fn incremental_edits_match_a_fresh_parse() {
    let mut b = Buffer::from_str("t.rs", "fn a() {}\n");
    b.syntax = Some(Box::new(Syntax::new(language("rust").unwrap())));
    let check = |b: &mut Buffer| {
        b.syntax.as_mut().unwrap().update(&b.rope);
        let (_, fresh) = parsed("rust", &b.rope.to_string());
        assert_eq!(sexp(b.syntax.as_ref().unwrap()), sexp(&fresh), "after edits: {:?}", b.rope.to_string());
    };
    check(&mut b);
    b.begin_group(&[], false);
    b.replace(8..8, "\n    let x = \"é\";\n");
    b.replace(0..0, "// ✓ héader\n");
    b.end_group(&[]);
    check(&mut b);
    // Several edits before one update (one command with many cursors).
    b.begin_group(&[], false);
    let end = b.rope.len_chars();
    b.replace(end..end, "struct S { f: u8 }\n");
    b.replace(3..4, "");
    b.replace(0..2, "/*");
    b.replace(2..2, " */");
    b.end_group(&[]);
    check(&mut b);
    b.undo().unwrap();
    check(&mut b);
    b.undo().unwrap();
    check(&mut b);
    b.redo().unwrap();
    check(&mut b);
    // A change that bypassed `replace` (different length): parsed afresh.
    b.rope.insert(0, "fn z() {}\n");
    check(&mut b);
}

#[test]
fn modes_choose_the_language() {
    let mut ed = Editor::new(Buffer::from_str("t", "fn main() {}\n"));
    ed.update_syntax(&[ed.buf_id]);
    assert!(ed.buf.syntax.is_none());
    ed.set_major_mode("rust-mode");
    ed.update_syntax(&[ed.buf_id]);
    assert_eq!(ed.buf.syntax.as_ref().map(|s| s.language.name), Some("rust"));
    assert!(ed.buf.syntax.as_ref().unwrap().tree().is_some());
    // Buffers not on screen are left alone.
    ed.set_major_mode("json-mode");
    ed.update_syntax(&[]);
    assert_eq!(ed.buf.syntax.as_ref().map(|s| s.language.name), Some("rust"));
    ed.update_syntax(&[ed.buf_id]);
    assert_eq!(ed.buf.syntax.as_ref().map(|s| s.language.name), Some("json"));
    ed.set_major_mode("text-mode");
    ed.update_syntax(&[ed.buf_id]);
    assert!(ed.buf.syntax.is_none());
}

#[test]
fn syntax_is_inherited_and_checked_in_the_config() {
    let user = r#"{ "modes": { "major": {
        "my-rust-mode": { "parent": "rust-mode" },
        "plain-rust-mode": { "parent": "rust-mode", "syntax": "" },
        "cobol-mode": { "syntax": "cobol" }
    } } }"#;
    let c = Config::from_layers(&[("built-in", DEFAULT_JSON), ("user", user)]);
    assert_eq!(c.warnings.len(), 1, "{:#?}", c.warnings);
    assert!(c.warnings[0].contains("unknown language \"cobol\""), "{}", c.warnings[0]);
    let m = &c.modes;
    assert_eq!(m.syntax("rust-mode"), Some("rust"));
    assert_eq!(m.syntax("json-mode"), Some("json"));
    assert_eq!(m.syntax("my-rust-mode"), Some("rust"));
    assert_eq!(m.syntax("plain-rust-mode"), None);
    assert_eq!(m.syntax("cobol-mode"), None);
    assert_eq!(m.syntax("text-mode"), None);
}

#[test]
fn theme_syntax_colors_fall_back_and_merge() {
    let t = theme();
    assert_eq!(t.syntax_color("function.method"), t.syntax_color("function"));
    assert_eq!(t.syntax_color("keyword.control.flow"), t.syntax_color("keyword"));
    assert_eq!(t.syntax_color("nothing.like.this"), None);
    let user = r##"{ "themes": { "ymacs-dark": { "syntax.keyword": "#ff0000", "syntax.function.method": "#00ff00", "syntax.oops": "red" } } }"##;
    let c = Config::from_layers(&[("built-in", DEFAULT_JSON), ("user", user)]);
    assert!(c.warnings.iter().any(|w| w.contains("syntax.oops")), "{:#?}", c.warnings);
    let u = &c.appearance.themes.dark;
    assert_eq!(u.syntax_color("keyword"), Some(0xff0000ff));
    assert_eq!(u.syntax_color("function.method"), Some(0x00ff00ff));
    assert_eq!(u.syntax_color("function"), t.syntax_color("function")); // untouched
    // Every built-in theme colors the common captures.
    let raw: serde_json::Value = serde_json::from_str(DEFAULT_JSON).unwrap();
    for (name, theme) in raw["themes"].as_object().unwrap() {
        for capture in ["keyword", "string", "comment", "function", "type", "constant"] {
            assert!(theme.get(format!("syntax.{capture}")).is_some(), "{name} has no syntax.{capture}");
        }
    }
}
