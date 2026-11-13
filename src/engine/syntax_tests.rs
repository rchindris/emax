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

/// The color of the first occurrence of `needle` in `text` (`lang`), if it has one.
fn color_of(lang: &str, text: &str, needle: &str) -> Option<Rgba> {
    let (rope, s) = parsed(lang, text);
    let colors = s.highlight(&rope, 0..rope.len_lines(), &theme());
    let at = rope.byte_to_char(text.find(needle).unwrap_or_else(|| panic!("{needle:?} in {text:?}")));
    let line = rope.char_to_line(at);
    color_at(&colors, line, at - rope.line_to_char(line))
}

#[test]
fn every_language_highlights_its_basics() {
    let t = theme();
    let (kw, string, comment) = (t.syntax_color("keyword"), t.syntax_color("string"), t.syntax_color("comment"));
    let cases: &[(&str, &str, &str, Option<Rgba>)] = &[
        ("bash", "if true; then echo \"hi\"; fi # done\n", "if", kw),
        ("bash", "if true; then echo \"hi\"; fi # done\n", "# done", comment),
        ("python", "def f(x):\n    return 'a'  # c\n", "def", kw),
        ("python", "def f(x):\n    return 'a'  # c\n", "'a'", string),
        ("toml", "[package]\nname = \"ymacs\" # c\n", "\"ymacs\"", string),
        ("toml", "[package]\nname = \"ymacs\" # c\n", "name", t.syntax_color("type")),
        ("yaml", "key: \"value\" # c\n", "# c", comment),
        ("yaml", "key: \"value\" # c\n", "\"value\"", string),
        ("scheme", "(define (f x) \"s\") ; c\n", "\"s\"", string),
        ("scheme", "(define (f x) \"s\") ; c\n", "; c", comment),
        ("elisp", "(defun f (x) \"doc\") ; c\n", "defun", kw),
        ("elisp", "(defun f (x) \"doc\") ; c\n", "; c", comment),
    ];
    for &(lang, text, needle, want) in cases {
        assert!(want.is_some(), "the theme colors {needle:?}");
        assert_eq!(color_of(lang, text, needle), want, "{lang}: {needle:?} in {text:?}");
    }
}

#[test]
fn markdown_highlights_inline_text_and_fenced_code_in_its_language() {
    let t = theme();
    let text = "# Title\n\nSome *emph* and `code` [link](http://x.org).\n\n```rust\nfn main() {}\n```\n\n```nosuchlang\nfn main() {}\n```\n";
    assert_eq!(color_of("markdown", text, "Title"), t.syntax_color("text.title"));
    assert_eq!(color_of("markdown", text, "emph"), t.syntax_color("text.emphasis"));
    assert_eq!(color_of("markdown", text, "code`"), t.syntax_color("text.literal"));
    assert_eq!(color_of("markdown", text, "http"), t.syntax_color("text.uri"));
    assert_eq!(color_of("markdown", text, "Some"), None);
    // The fence is Rust: its keyword is a keyword, painted over the block's color.
    assert_eq!(color_of("markdown", text, "fn main"), t.syntax_color("keyword"));
    // An unknown language keeps the code block's own color.
    let unknown = text.rfind("fn main").unwrap();
    let (rope, s) = parsed("markdown", text);
    let colors = s.highlight(&rope, 0..rope.len_lines(), &t);
    let line = rope.byte_to_line(unknown);
    assert_eq!(color_at(&colors, line, 0), t.syntax_color("text.literal"));
    // Only the code on screen is kept parsed.
    // (the heading's and the paragraph's inline text, and the Rust fence)
    assert_eq!(s.injected.lock().unwrap().len(), 3);
    s.highlight(&rope, 0..1, &t);
    assert_eq!(s.injected.lock().unwrap().len(), 1);
}

#[test]
fn embedded_languages_are_found_by_alias() {
    assert_eq!(language_named("sh").map(|l| l.name), Some("bash"));
    assert_eq!(language_named("Python").map(|l| l.name), Some("python"));
    assert_eq!(language_named("yml").map(|l| l.name), Some("yaml"));
    assert!(language_named("cobol").is_none());
}

#[test]
fn file_names_and_interpreters_pick_the_new_modes() {
    let ed = Editor::new(Buffer::from_str("t", ""));
    let detect = |name: &str, text: &str| {
        let mut b = Buffer::from_str(name, text);
        b.path = Some(std::path::PathBuf::from(format!("/tmp/{name}")));
        ed.modes.detect(&b).and_then(|m| ed.modes.syntax(m)).map(str::to_string)
    };
    for (name, text, want) in [
        ("notes.md", "", "markdown"),
        ("Cargo.toml", "", "toml"),
        ("ci.yml", "", "yaml"),
        ("build.sh", "", "bash"),
        ("run", "#!/usr/bin/env bash\n", "bash"),
        (".zshrc", "", "bash"),
        ("tool.py", "", "python"),
        ("tool", "#!/usr/bin/env python3\n", "python"),
        ("init.scm", "", "scheme"),
        ("init.el", "", "elisp"),
    ] {
        assert_eq!(detect(name, text).as_deref(), Some(want), "{name}");
    }
}
