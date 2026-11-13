//! Behavioural tests: drive the editor through real key sequences.
//! In buffer strings, `|` marks a cursor (several `|` = several cursors).

use super::cursor::Cursor;
use super::*;

struct T {
    ed: Editor,
    d: Dispatcher,
    picker_keys: Keymap,
    search_keys: Keymap,
}

fn t(s: &str) -> T {
    let mut text = String::new();
    let mut points = Vec::new();
    for c in s.chars() {
        if c == '|' {
            points.push(text.chars().count());
        } else {
            text.push(c);
        }
    }
    let mut ed = Editor::new(Buffer::from_str("test", &text));
    ed.cursors[0].point = points.first().copied().unwrap_or(0);
    for &p in points.iter().skip(1) {
        ed.add_cursor(Cursor::new(0, p));
    }
    ed.normalize_cursors();
    let k = Keymaps::builtin();
    T { ed, d: Dispatcher::new(), picker_keys: k.picker, search_keys: k.search }
}

impl T {
    fn keys(&mut self, keys: &str) -> &mut Self {
        for k in keys.split_whitespace() {
            self.key(Key::parse(k));
        }
        self
    }

    fn typ(&mut self, s: &str) -> &mut Self {
        for c in s.chars() {
            let k = match c {
                ' ' => Key::parse("SPC"),
                '\n' => Key::parse("RET"),
                c => Key { name: c.to_lowercase().to_string(), text: Some(c.to_string()), ..Default::default() },
            };
            self.key(k);
        }
        self
    }

    fn key(&mut self, k: Key) {
        if self.ed.picker.is_some() {
            self.ed.picker_key(&self.picker_keys, &k);
            if let Some((cmd, args)) = self.ed.pending_command.take()
                && let Outcome::Run(cmd, args) = self.d.invoke(cmd, args)
            {
                self.ed.execute(cmd, args);
            }
        } else if self.ed.swiper.is_some() {
            self.ed.swiper_key(&self.search_keys, &k);
        } else if let Outcome::Run(cmd, args) = self.d.dispatch(&k, &self.ed.active_keymaps()) {
            self.ed.execute(cmd, args);
        }
    }

    fn names(&self) -> Vec<String> {
        let mut v = vec![self.ed.buf.name.clone()];
        v.extend(self.ed.others.iter().map(|s| s.buf.name.clone()));
        v
    }

    fn show(&self) -> String {
        let mut s = self.ed.buf.rope.to_string();
        let mut pts: Vec<usize> = self.ed.cursors.iter().map(|c| c.point).collect();
        pts.sort_unstable();
        for p in pts.into_iter().rev() {
            let b = s.char_indices().nth(p).map(|(b, _)| b).unwrap_or(s.len());
            s.insert(b, '|');
        }
        s
    }

    fn msg(&self) -> String {
        self.ed.message.clone().unwrap_or_default()
    }
}

#[test]
fn motion_basics() {
    let mut x = t("|hello world\nsecond line");
    x.keys("M-f");
    assert_eq!(x.show(), "hello| world\nsecond line");
    x.keys("C-e C-n");
    assert_eq!(x.show(), "hello world\nsecond line|");
    x.keys("M-<");
    assert_eq!(x.show(), "|hello world\nsecond line");
    x.keys("M->");
    assert_eq!(x.show(), "hello world\nsecond line|");
    x.keys("C-a M-b");
    assert_eq!(x.show(), "hello |world\nsecond line");
}

#[test]
fn goal_column_survives_short_lines() {
    let mut x = t("abcdef|gh\nab\nabcdefghij");
    x.keys("C-n");
    assert_eq!(x.show(), "abcdefgh\nab|\nabcdefghij");
    x.keys("C-n");
    assert_eq!(x.show(), "abcdefgh\nab\nabcdef|ghij");
}

#[test]
fn prefix_arguments() {
    let mut x = t("|abcdefghijklmnopqrstuvwxyz");
    x.keys("C-u C-f");
    assert_eq!(x.show(), "abcd|efghijklmnopqrstuvwxyz");
    x.keys("M-1 M-0 C-f");
    assert_eq!(x.show(), "abcdefghijklmn|opqrstuvwxyz");
    x.keys("C-u 3 C-d");
    assert_eq!(x.show(), "abcdefghijklmn|rstuvwxyz");
    x.keys("C-u 3 -");
    assert_eq!(x.show(), "abcdefghijklmn---|rstuvwxyz");
}

#[test]
fn kill_line_appends_and_yanks() {
    let mut x = t("|one\ntwo\nthree");
    x.keys("C-k C-k");
    assert_eq!(x.show(), "|two\nthree");
    assert_eq!(x.ed.kill_ring.last().unwrap(), "one\n");
    x.keys("C-e C-k");
    // not consecutive with the earlier kills -> new entry ("" at eol kills newline)
    assert_eq!(x.show(), "two|three");
    x.keys("C-y");
    assert_eq!(x.show(), "two\n|three");
    x.keys("M-y");
    assert_eq!(x.show(), "twoone\n|three");
}

#[test]
fn kill_line_with_arg() {
    let mut x = t("a\n|b\nc\nd");
    x.keys("C-u 2 C-k");
    assert_eq!(x.show(), "a\n|d");
}

#[test]
fn region_kill_copy_yank() {
    let mut x = t("|alpha beta gamma");
    x.keys("C-SPC M-f M-f C-w");
    assert_eq!(x.show(), "| gamma");
    x.keys("C-e C-y");
    assert_eq!(x.show(), " gammaalpha beta|");
    x.keys("C-a C-SPC M-f M-w C-e C-y");
    // " gammaalpha" is one word after the earlier yank
    assert_eq!(x.show(), " gammaalpha beta gammaalpha|");
}

#[test]
fn backward_kill_word_prepends() {
    let mut x = t("one two three|");
    x.keys("M-DEL M-DEL");
    assert_eq!(x.show(), "one |");
    assert_eq!(x.ed.kill_ring.last().unwrap(), "two three");
}

#[test]
fn shift_selection() {
    let mut x = t("|hello world");
    x.keys("S-<right> S-<right> S-M-f");
    let c = x.ed.cursors[0].clone();
    assert_eq!(c.region(), Some(0..5));
    x.keys("<right>");
    assert!(x.ed.cursors[0].region().is_none());
}

#[test]
fn delete_selection_replaces_region() {
    let mut x = t("|hello world");
    x.keys("C-SPC M-f");
    x.typ("bye");
    assert_eq!(x.show(), "bye| world");
}

#[test]
fn undo_redo_groups() {
    let mut x = t("|");
    x.typ("abc");
    x.keys("RET");
    x.typ("de");
    assert_eq!(x.show(), "abc\nde|");
    x.keys("C-/");
    assert_eq!(x.show(), "abc\n|");
    x.keys("C-/");
    assert_eq!(x.show(), "abc|");
    x.keys("C-/");
    assert_eq!(x.show(), "|");
    assert!(!x.ed.buf.modified());
    x.keys("C-?");
    assert_eq!(x.show(), "abc|");
    x.keys("C-? C-?");
    assert_eq!(x.show(), "abc\nde|");
    x.keys("C-?");
    assert_eq!(x.msg(), "No further redo information");
}

#[test]
fn transpose() {
    let mut x = t("ab|cd");
    x.keys("C-t");
    assert_eq!(x.show(), "acb|d");
    let mut x = t("abcd|");
    x.keys("C-t");
    assert_eq!(x.show(), "abdc|");
    let mut x = t("foo ba|r baz");
    x.keys("M-t");
    assert_eq!(x.show(), "foo baz bar|");
    let mut x = t("one\ntw|o\nthree");
    x.keys("C-x C-t");
    assert_eq!(x.show(), "two\none\n|three");
    let mut x = t("(a b) |(c d)");
    x.keys("C-M-t");
    assert_eq!(x.show(), "(c d) (a b)|");
}

#[test]
fn case_commands() {
    let mut x = t("|hello wORLD foo");
    x.keys("M-u");
    assert_eq!(x.show(), "HELLO| wORLD foo");
    x.keys("M-c");
    assert_eq!(x.show(), "HELLO World| foo");
    x.keys("M-- M-u");
    assert_eq!(x.show(), "HELLO WORLD| foo");
    x.keys("M-l");
    assert_eq!(x.show(), "HELLO WORLD foo|");
}

#[test]
fn whitespace_commands() {
    let mut x = t("a   |   b");
    x.keys("M-\\");
    assert_eq!(x.show(), "a|b");
    let mut x = t("a   |   b");
    x.keys("M-SPC");
    assert_eq!(x.show(), "a |b");
    let mut x = t("foo\n    |bar");
    x.keys("M-^");
    assert_eq!(x.show(), "foo| bar");
    let mut x = t("a\n\n|\n\nb");
    x.keys("C-x C-o");
    assert_eq!(x.show(), "a\n|\nb");
    x.keys("C-x C-o");
    assert_eq!(x.show(), "a\n|b");
}

#[test]
fn newline_keeps_indentation_and_open_line() {
    let mut x = t("    foo|");
    x.keys("RET");
    assert_eq!(x.show(), "    foo\n    |");
    let mut x = t("ab|cd");
    x.keys("C-o");
    assert_eq!(x.show(), "ab|\ncd");
}

#[test]
fn zap() {
    let mut x = t("|hello, world");
    x.keys("M-z ,");
    assert_eq!(x.show(), "| world");
    assert_eq!(x.ed.kill_ring.last().unwrap(), "hello,");
}

#[test]
fn quoted_insert_control_char() {
    let mut x = t("|");
    x.keys("C-q C-l");
    assert_eq!(x.show(), "\u{c}|");
}

#[test]
fn mark_ring_and_exchange() {
    let mut x = t("ab|cdef");
    x.keys("C-SPC C-f C-f C-x C-x");
    assert_eq!(x.show(), "ab|cdef");
    assert_eq!(x.ed.cursors[0].region(), Some(2..4));
    x.keys("C-g M-> C-u C-SPC");
    assert_eq!(x.show(), "ab|cdef"); // M-> pushed the mark; C-u C-SPC jumps back
}

#[test]
fn fill_paragraph() {
    let mut x = t("|aaa bbb ccc ddd eee fff");
    x.ed.buf.modes.local.fill_column = Some(11);
    x.keys("M-q");
    assert_eq!(x.ed.buf.rope.to_string(), "aaa bbb ccc\nddd eee fff");
}

#[test]
fn multiple_cursors_next_like_this() {
    let mut x = t("|foo bar foo baz foo");
    x.keys("C-SPC M-f C->  C->");
    assert_eq!(x.ed.cursors.len(), 3);
    x.typ("X");
    assert_eq!(x.show(), "X| bar X| baz X|");
    x.keys("C-/");
    assert_eq!(x.ed.buf.rope.to_string(), "foo bar foo baz foo");
}

#[test]
fn multiple_cursors_lines_and_kill_yank() {
    let mut x = t("|alpha\nbeta\ngamma");
    x.keys("C-> C->");
    assert_eq!(x.show(), "|alpha\n|beta\n|gamma");
    x.keys("M-d");
    assert_eq!(x.show(), "|\n|\n|");
    x.keys("C-y");
    assert_eq!(x.show(), "alpha|\nbeta|\ngamma|");
    x.keys("C-g");
    assert_eq!(x.ed.cursors.len(), 1);
}

#[test]
fn multiple_cursors_mark_all_and_edit_lines() {
    let mut x = t("|x = 1; x = 2; y = x;");
    x.keys("C-f S-C-b C-c C-<");
    assert_eq!(x.ed.cursors.len(), 3);
    x.typ("v");
    assert_eq!(x.ed.buf.rope.to_string(), "v = 1; v = 2; y = v;");

    let mut x = t("a|bc\ndef\nghi");
    x.keys("C-SPC C-n C-n C-S-c C-S-c");
    assert_eq!(x.show(), "a|bc\nd|ef\ng|hi");
    x.keys("C-d");
    assert_eq!(x.show(), "a|c\nd|f\ng|i");
}

#[test]
fn cursors_merge_when_they_meet() {
    let mut x = t("|ab|c");
    x.keys("C-e");
    assert_eq!(x.ed.cursors.len(), 1);
}

#[test]
fn sexp_and_paragraph_motion() {
    let mut x = t("|(a (b c) d) e\n\npara two\nmore\n\nthree");
    x.keys("C-M-f");
    assert_eq!(x.show(), "(a (b c) d)| e\n\npara two\nmore\n\nthree");
    x.keys("M-}");
    assert_eq!(x.show(), "(a (b c) d) e\n|\npara two\nmore\n\nthree");
    x.keys("M-}");
    assert_eq!(x.show(), "(a (b c) d) e\n\npara two\nmore\n|\nthree");
    x.keys("M-{ M-{");
    assert_eq!(x.show(), "|(a (b c) d) e\n\npara two\nmore\n\nthree");
}

#[test]
fn quit_requires_confirmation_when_modified() {
    let mut x = t("|");
    x.ed.buf.path = Some("/nonexistent/x".into());
    x.typ("a");
    x.keys("C-x C-c");
    assert!(!x.ed.quit_requested);
    x.keys("C-x C-c");
    assert!(x.ed.quit_requested);
}

// ------------------------------------------------------------------ buffers

fn three_buffers() -> T {
    let mut x = t("|main text");
    x.ed.buf.name = "main.rs".into();
    x.ed.add_buffer(Buffer::from_str("editor.rs", "editor text"));
    x.ed.add_buffer(Buffer::from_str("notes.md", "notes text"));
    x // order: main.rs (current), notes.md, editor.rs
}

#[test]
fn switch_buffer_picker_default_is_other_buffer() {
    let mut x = three_buffers();
    x.keys("C-x b");
    assert!(x.ed.picker.is_some());
    let labels: Vec<_> =
        x.ed.picker
            .as_ref()
            .unwrap()
            .matches
            .iter()
            .map(|m| x.ed.picker.as_ref().unwrap().candidates[m.index].label.clone())
            .collect();
    assert_eq!(labels, ["notes.md", "editor.rs", "main.rs"]);
    x.keys("RET");
    assert!(x.ed.picker.is_none());
    assert_eq!(x.names(), ["notes.md", "main.rs", "editor.rs"]);
    // C-x b RET toggles back
    x.keys("C-x b RET");
    assert_eq!(x.ed.buf.name, "main.rs");
}

#[test]
fn picker_filters_previews_and_cancels() {
    let mut x = three_buffers();
    x.keys("C-x b");
    x.typ("edi");
    assert_eq!(x.ed.display_view().buf.name, "editor.rs"); // live preview
    assert_eq!(x.ed.buf.name, "main.rs"); // not switched yet
    x.keys("C-g");
    assert!(x.ed.picker.is_none());
    assert_eq!(x.ed.display_view().buf.name, "main.rs");
    assert_eq!(x.names(), ["main.rs", "notes.md", "editor.rs"]); // MRU untouched
}

#[test]
fn buffers_keep_their_own_cursors_and_undo() {
    let mut x = three_buffers();
    x.keys("C-e");
    x.typ("!");
    x.keys("C-x b");
    x.typ("ed");
    x.keys("RET");
    assert_eq!(x.show(), "|editor text");
    x.keys("C-x b RET");
    assert_eq!(x.show(), "main text!|");
    x.keys("C-/");
    assert_eq!(x.show(), "main text|");
}

#[test]
fn picker_creates_and_kills_buffers() {
    let mut x = three_buffers();
    x.keys("C-x b");
    x.typ("todo");
    x.keys("RET"); // no match -> create
    assert_eq!(x.ed.buf.name, "todo");
    assert_eq!(x.ed.buffer_count(), 4);
    x.keys("C-x b");
    x.typ("ma");
    x.keys("C-j"); // literal name even though "main.rs" matches
    assert_eq!(x.ed.buf.name, "ma");
    x.keys("C-x b");
    x.typ("notes");
    x.keys("C-k");
    assert!(!x.names().contains(&"notes.md".to_string()));
    x.keys("C-g C-x k");
    assert_eq!(x.ed.buf.name, "todo");
    assert_eq!(x.ed.buffer_count(), 3);
}

#[test]
fn previous_and_next_buffer_cycle() {
    let mut x = three_buffers();
    x.keys("C-x <left>");
    assert_eq!(x.ed.buf.name, "notes.md");
    x.keys("C-x <left>");
    assert_eq!(x.ed.buf.name, "editor.rs");
    x.keys("C-x <left>");
    assert_eq!(x.ed.buf.name, "main.rs");
    x.keys("C-x <right>");
    assert_eq!(x.ed.buf.name, "editor.rs");
}

#[test]
fn killing_last_buffer_leaves_scratch_and_names_are_unique() {
    let mut x = t("|");
    x.ed.add_buffer(Buffer::from_str("test", ""));
    assert_eq!(x.ed.others[0].buf.name, "test<2>");
    x.keys("C-x k C-x k");
    assert_eq!(x.ed.buf.name, "*scratch*");
    assert_eq!(x.ed.buffer_count(), 1);
}

// ------------------------------------------------------------------ search

const HAYSTACK: &str =
    "fn main() {\n    let foo = 1;\n|    let bar = foo + 2;\n    println!(\"{foo} {bar}\");\n}\n// Foo is done\n";

fn swiper_lines(x: &T) -> Vec<usize> {
    x.ed.swiper.as_ref().unwrap().matches.clone()
}

#[test]
fn swiper_lists_matching_lines_and_jumps_to_the_selection() {
    let mut x = t(HAYSTACK);
    x.keys("C-s");
    assert_eq!(swiper_lines(&x).len(), 7); // empty query: every line
    x.typ("foo");
    assert_eq!(swiper_lines(&x), [1, 2, 3, 5]); // smart case: "foo" ignores case
    // Preselects the first match at or after point and previews it.
    assert_eq!(x.ed.swiper.as_ref().unwrap().selected_line(), Some(2));
    assert!(x.show().contains("let bar = foo| + 2"));
    x.keys("C-s"); // next match
    assert!(x.show().contains("{foo|}"));
    x.keys("RET");
    assert!(x.ed.swiper.is_none());
    assert!(x.show().contains("{foo|}"));
    assert_eq!(x.msg(), "Mark saved where search started");
    x.keys("C-u C-SPC"); // back to where the search started
    assert!(x.show().contains("\n|    let bar"));
}

#[test]
fn swiper_terms_are_orderless_and_smart_case() {
    let mut x = t(HAYSTACK);
    x.keys("C-s");
    x.typ("bar let");
    assert_eq!(swiper_lines(&x), [2]);
    x.keys("C-u");
    x.typ("Foo");
    assert_eq!(swiper_lines(&x), [5]);
    let s = x.ed.swiper.as_ref().unwrap();
    assert_eq!(s.highlights(5), vec![3..6]);
    x.keys("DEL DEL DEL");
    x.typ("o");
    assert_eq!(x.ed.swiper.as_ref().unwrap().highlights(3), vec![16..18]); // merged "o" "o"
}

#[test]
fn swiper_cancel_restores_cursors() {
    let mut x = t("|alpha\nbeta\n|gamma beta\n");
    x.keys("C-s");
    x.typ("beta");
    assert_eq!(x.ed.cursors.len(), 1); // previewing collapses to one cursor
    x.keys("C-g");
    assert!(x.ed.swiper.is_none());
    assert_eq!(x.show(), "|alpha\nbeta\n|gamma beta\n");
    assert_eq!(x.msg(), "Quit");
}

#[test]
fn swiper_backward_repeat_and_no_match() {
    let mut x = t("one x\ntwo\nthree x\nfour|\nfive x\n");
    x.keys("C-r");
    x.typ("x");
    assert_eq!(x.ed.swiper.as_ref().unwrap().selected_line(), Some(2)); // before point
    x.keys("C-r C-r"); // previous, then wraps to the last
    assert_eq!(x.ed.swiper.as_ref().unwrap().selected_line(), Some(4));
    x.keys("RET");
    assert_eq!(x.ed.last_search, "x");
    // C-s C-s with an empty query repeats the last search.
    x.keys("M-< C-s C-s");
    assert_eq!(x.ed.swiper.as_ref().unwrap().query, "x");
    x.keys("C-g");
    x.keys("C-s");
    x.typ("zzz");
    x.keys("RET");
    assert_eq!(x.show(), "|one x\ntwo\nthree x\nfour\nfive x\n");
    assert_eq!(x.msg(), "No match for “zzz”");
}

#[test]
fn swiper_yanks_symbol_at_point() {
    let mut x = t("let my_var = 1;\nmy_var += 2;\nother\n");
    x.keys("C-f C-f C-f C-f C-f C-s M-n");
    assert_eq!(x.ed.swiper.as_ref().unwrap().query, "my_var");
    assert_eq!(swiper_lines(&x), [0, 1]);
}

// =================================================================== modes

/// An editor on a buffer visiting `path` (not read from disk) with `s` as text.
fn file(path: &str, s: &str) -> T {
    let mut x = t(s);
    x.ed.buf.path = Some(std::path::PathBuf::from(path));
    x.ed.buf.name = path.to_string();
    let modes = x.ed.modes().initial(&x.ed.buf);
    x.ed.buf.modes = modes;
    x
}

fn mode_line(x: &T) -> String {
    x.ed.mode_lighters(&x.ed.buf)
}

#[test]
fn major_mode_selection() {
    assert_eq!(file("main.rs", "").ed.major_mode(), "rust-mode");
    assert_eq!(file("notes.md", "").ed.major_mode(), "markdown-mode");
    assert_eq!(file("README", "").ed.major_mode(), "text-mode");
    assert_eq!(file("data.json", "").ed.major_mode(), "json-mode");
    assert_eq!(file("Cargo.lock", "").ed.major_mode(), "toml-mode");
    assert_eq!(file("notes.txt", "").ed.major_mode(), "text-mode");
    // A marker beats the file name; a shebang beats it too.
    assert_eq!(file("main.rs", "// -*- mode: lisp -*-\n").ed.major_mode(), "lisp-mode");
    assert_eq!(file("script", "#!/usr/bin/env steel\n").ed.major_mode(), "scheme-mode");
    assert_eq!(file("script", "#!/usr/bin/env sbcl\n;; -*- text -*-\n").ed.major_mode(), "text-mode");
    // An unknown marker falls through to the file name.
    assert_eq!(file("main.rs", "// -*- cobol -*-\n").ed.major_mode(), "rust-mode");
    // Non-file buffers: fundamental, except *scratch*.
    assert_eq!(t("").ed.major_mode(), "fundamental-mode");
    assert_eq!(Editor::new(Buffer::from_str("*scratch*", "")).major_mode(), "text-mode");
}

#[test]
fn modes_bring_settings_and_minor_modes() {
    let x = file("main.rs", "");
    assert_eq!((x.ed.fill_column(), x.ed.tab_width()), (100, 4)); // rust-mode's own
    assert!(x.ed.minor_on(MinorMode::ElectricPair)); // from prog-mode, its parent
    assert_eq!(mode_line(&x), "Rust");
    let x = file("notes.md", "");
    assert_eq!((x.ed.fill_column(), x.ed.tab_width()), (70, 4)); // global
    assert!(x.ed.minor_on(MinorMode::AutoFill)); // from text-mode, its parent
    assert_eq!(mode_line(&x), "Markdown Fill");
    assert!(x.ed.modes().derives_from("markdown-mode", "text-mode"));
}

#[test]
fn switching_major_mode() {
    let mut x = file("main.rs", "");
    x.keys("C-u 5 C-x f"); // buffer-local fill column
    assert_eq!(x.ed.fill_column(), 5);
    x.keys("C-x C-q"); // read-only survives a mode switch, other locals don't
    x.ed.execute("text-mode", Args::default());
    assert_eq!(x.ed.major_mode(), "text-mode");
    assert_eq!(x.ed.fill_column(), 70);
    assert!(!x.ed.minor_on(MinorMode::ElectricPair));
    assert!(x.ed.minor_on(MinorMode::AutoFill));
    assert_eq!(mode_line(&x), "Text Fill RO");
}

#[test]
fn minor_mode_toggle_arguments() {
    let mut x = t("");
    x.keys("<insert>");
    assert!(x.ed.minor_on(MinorMode::Overwrite));
    assert_eq!(x.msg(), "Overwrite mode enabled in current buffer");
    x.keys("<insert>");
    assert!(!x.ed.minor_on(MinorMode::Overwrite));
    x.keys("C-u <insert> C-u <insert>"); // positive: on, stays on
    assert!(x.ed.minor_on(MinorMode::Overwrite));
    x.keys("M-- <insert>");
    assert!(!x.ed.minor_on(MinorMode::Overwrite));
    x.keys("M-0 <insert>");
    assert!(!x.ed.minor_on(MinorMode::Overwrite));
}

#[test]
fn read_only_refuses_changes() {
    let mut x = t("ab|c");
    x.keys("C-x C-q");
    x.typ("x").keys("C-d C-a");
    assert_eq!(x.show(), "|abc");
    x.keys("C-e DEL");
    assert_eq!(x.show(), "abc|");
    assert_eq!(x.msg(), "Buffer is read-only: test");
    x.keys("C-a C-k"); // the kill reaches the kill ring, the text stays
    assert_eq!(x.show(), "|abc");
    assert_eq!(x.ed.kill_ring.last().unwrap(), "abc");
    x.keys("C-/");
    assert_eq!(x.msg(), "Buffer is read-only: test");
    x.keys("C-x C-q C-k");
    assert_eq!(x.show(), "|");
}

#[test]
fn overwrite_mode_types_over() {
    let mut x = t("|abcd\nef");
    x.keys("<insert>");
    x.typ("XY");
    assert_eq!(x.show(), "XY|cd\nef");
    x.typ("ZZZ"); // never eats the newline
    assert_eq!(x.show(), "XYZZZ|\nef");
    x.keys("C-/");
    assert_eq!(x.show(), "|abcd\nef");
}

#[test]
fn electric_pair_mode() {
    let mut x = file("main.rs", "|");
    x.typ("f(");
    assert_eq!(x.show(), "f(|)");
    x.typ("\"a");
    assert_eq!(x.show(), "f(\"a|\")");
    x.typ("\")"); // steps over the closers
    assert_eq!(x.show(), "f(\"a\")|");
    x.typ(" [");
    assert_eq!(x.show(), "f(\"a\") [|]");
    x.keys("DEL"); // deleting an empty pair deletes both halves
    assert_eq!(x.show(), "f(\"a\") |");
    // No pairing in front of a word; a region gets wrapped.
    let mut x = file("main.rs", "|word");
    x.typ("(");
    assert_eq!(x.show(), "(|word");
    let mut x = file("main.rs", "|word");
    x.keys("M-@").typ("[");
    assert_eq!(x.show(), "[|word]");
    // Off in text buffers.
    let mut x = t("|");
    x.typ("(");
    assert_eq!(x.show(), "(|");
}

#[test]
fn auto_fill_breaks_lines() {
    let mut x = file("notes.md", "|");
    x.ed.buf.modes.local.fill_column = Some(20);
    x.typ("  the quick brown fox jumps over the lazy dog ");
    assert_eq!(x.show(), "  the quick brown\n  fox jumps over the\n  lazy dog |");
    // A word longer than the line goes on a line of its own.
    let mut x = file("notes.md", "|");
    x.ed.buf.modes.local.fill_column = Some(5);
    x.typ("a abcdefghij b\n");
    assert_eq!(x.show(), "a\nabcdefghij\nb\n|");
    // Off: nothing happens.
    let mut x = t("|");
    x.ed.buf.modes.local.fill_column = Some(5);
    x.typ("aaa bbb ccc ");
    assert_eq!(x.show(), "aaa bbb ccc |");
}

#[test]
fn mode_keymaps_layer_over_global() {
    let user = r#"{
        "modes": {
            "major": {
                "rust-mode": { "keymap": { "C-c C-f": "fill-paragraph", "C-f": "forward-word" } },
                "prog-mode": { "keymap": { "C-c C-k": "kill-whole-line", "C-f": "end-of-buffer" } },
                "go-mode": { "parent": "prog-mode", "files": ["*.go"], "editor": { "tab_width": 8 } }
            },
            "minor": { "overwrite-mode": { "lighter": "OVR", "keymap": { "C-f": "backward-char" } } }
        },
        "keymaps": { "global": { "C-c m": "go-mode" } }
    }"#;
    let c = Config::from_layers(&[("built-in", config::DEFAULT_JSON), ("user", user)]);
    assert!(c.warnings.is_empty(), "{:#?}", c.warnings);
    let mut x = file("main.rs", "|ab cd\nline");
    x.ed.set_modes(std::sync::Arc::new(c.modes));
    assert_eq!(x.ed.major_mode(), "rust-mode");
    x.keys("C-f"); // the major mode's own binding beats its parent's and the global one
    assert_eq!(x.show(), "ab| cd\nline");
    x.keys("C-c C-k"); // inherited from prog-mode
    assert_eq!(x.show(), "|line");
    x.keys("<insert> C-e C-f"); // a minor mode's keymap beats the major mode's
    assert_eq!(x.show(), "lin|e");
    assert_eq!(mode_line(&x), "Rust OVR");
    x.keys("C-c m"); // a user-defined major mode, bound globally
    assert_eq!(x.ed.major_mode(), "go-mode");
    assert_eq!(x.ed.tab_width(), 8);
    assert_eq!(mode_line(&x), "Go"); // lighter from the name; overwrite was dropped
}

#[test]
fn modes_config_problems_are_reported() {
    let user = r#"{
        "modes": {
            "default_major_mode": "nope-mode",
            "major": {
                "a-mode": { "parent": "b-mode" },
                "b-mode": { "parent": "a-mode" },
                "c-mode": { "parent": "missing-mode", "minor_modes": ["sparkle-mode"], "editor": { "tab_width": 0 } },
                "badname": {},
                "undo-mode": {},
                "auto-fill-mode": {}
            },
            "minor": { "sparkle-mode": {} }
        }
    }"#;
    let c = Config::from_layers(&[("built-in", config::DEFAULT_JSON), ("user", user)]);
    let w = c.warnings.join("\n");
    for needle in [
        "unknown major mode \"nope-mode\"",
        "makes a cycle",
        "unknown parent mode \"missing-mode\"",
        "unknown minor mode \"sparkle-mode\"",
        "tab_width must be at least 1",
        "modes.major.badname: a major mode's name must end in \"-mode\"",
        "modes.major.auto-fill-mode: the name is already a command",
    ] {
        assert!(w.contains(needle), "missing {needle:?} in:\n{w}");
    }
    let m = &c.modes;
    assert_eq!(m.default_major, "fundamental-mode");
    assert!(m.major("c-mode").is_some_and(|c| c.parent.is_none()));
    assert!(m.chain("a-mode").len() <= 2);
    assert!(m.major("undo-mode").is_some()); // "undo-mode" isn't a command, so it's allowed
}

#[test]
fn load_theme_previews_loads_and_cancels() {
    let mut x = t("|text");
    x.ed.themes = vec![("ymacs-dark".into(), true), ("ymacs-light".into(), false), ("solarized-light".into(), false)];
    x.ed.active_theme = "ymacs-dark".into();
    x.keys("M-x").typ("load-theme").keys("RET");
    // Opens on the theme on screen, which previews unchanged.
    let p = x.ed.picker.as_ref().unwrap();
    assert_eq!(p.selected_candidate().unwrap().label, "ymacs-dark");
    assert!(p.selected_candidate().unwrap().current);
    assert_eq!(p.selected_candidate().unwrap().annotation, "dark");
    assert_eq!(x.ed.theme_preview(), Some(ThemeRequest::Named("ymacs-dark".into())));
    // Moving previews; cancelling loads nothing.
    x.keys("C-n");
    assert_eq!(x.ed.theme_preview(), Some(ThemeRequest::Named("ymacs-light".into())));
    x.keys("C-g");
    assert!(x.ed.picker.is_none() && x.ed.theme_request.is_none() && x.ed.theme_preview().is_none());
    // Filtering and RET loads the theme.
    x.keys("M-x").typ("load-theme").keys("RET").typ("sol light").keys("RET");
    assert!(x.ed.picker.is_none());
    assert_eq!(x.ed.theme_request.take(), Some(ThemeRequest::Named("solarized-light".into())));
    assert_eq!(x.ed.message.as_deref(), Some("Loaded theme solarized-light"));
    // The last entry goes back to the configured theme.
    x.keys("M-x").typ("load-theme").keys("RET").typ("configured").keys("RET");
    assert_eq!(x.ed.theme_request.take(), Some(ThemeRequest::Configured));
    // No match keeps the picker open; the buffer is untouched throughout.
    x.keys("M-x").typ("load-theme").keys("RET").typ("zzqq").keys("RET");
    assert!(x.ed.picker.is_some());
    x.keys("C-g");
    assert_eq!(x.show(), "|text");
}

#[test]
fn load_theme_without_themes() {
    let mut x = t("|text");
    x.keys("M-x").typ("load-theme").keys("RET");
    assert!(x.ed.picker.is_none());
    assert_eq!(x.ed.message.as_deref(), Some("No themes are defined"));
}

#[test]
fn execute_extended_command() {
    let mut x = t("hello |world");
    x.keys("M-x");
    assert!(x.ed.picker.is_some());
    x.typ("upcase-word").keys("RET");
    assert!(x.ed.picker.is_none());
    assert_eq!(x.show(), "hello WORLD|");
    // The prefix argument typed before M-x reaches the command.
    let mut x = t("|abcdef");
    x.keys("C-u 3 M-x").typ("forward-char").keys("RET");
    assert_eq!(x.show(), "abc|def");
    // Commands that read a character still do.
    x.keys("M-x").typ("zap-to-char").keys("RET").typ("e");
    assert_eq!(x.show(), "abc|f");
    // Mode commands are there too; the last command used comes first.
    x.keys("M-x").typ("rust-mode").keys("RET");
    assert_eq!(x.ed.major_mode(), "rust-mode");
    x.keys("M-x");
    let p = x.ed.picker.as_ref().unwrap();
    assert_eq!(p.selected_candidate().unwrap().label, "rust-mode");
    assert!(p.selected_candidate().unwrap().current);
    // No match: the picker stays open.
    x.typ("zzzzqqq").keys("RET");
    assert!(x.ed.picker.is_some());
    x.keys("C-g");
    assert!(x.ed.picker.is_none());
}

fn file_picker_labels(x: &T) -> Vec<String> {
    let p = x.ed.picker.as_ref().expect("picker open");
    p.matches.iter().map(|m| p.candidates[m.index].label.clone()).collect()
}

/// A directory with `src/main.rs`, `README.md` and a hidden file, and an
/// editor whose current buffer visits a file in it.
fn file_tree() -> (T, std::path::PathBuf) {
    let d = super::testutil::temp_dir("ff");
    std::fs::create_dir(d.join("src")).unwrap();
    std::fs::write(d.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(d.join("README.md"), "readme\n").unwrap();
    std::fs::write(d.join(".hidden"), "").unwrap();
    let mut x = t("|");
    x.ed.buf.path = Some(d.join("notes.txt"));
    (x, d)
}

#[test]
fn find_file_navigates_and_opens() {
    let (mut x, d) = file_tree();
    x.keys("C-x C-f");
    let q = x.ed.picker.as_ref().unwrap().query.clone();
    assert_eq!(q, format!("{}/", d.display())); // starts in the buffer's directory
    // Directories first, hidden entries last.
    assert_eq!(file_picker_labels(&x), ["src/", "README.md", ".hidden"]);
    // RET on a directory enters it; the last component filters.
    x.typ("sr").keys("RET");
    assert_eq!(x.ed.picker.as_ref().unwrap().query, format!("{}/src/", d.display()));
    assert_eq!(file_picker_labels(&x), ["main.rs"]);
    x.typ("mn").keys("RET");
    assert!(x.ed.picker.is_none());
    assert_eq!(x.ed.buf.name, "main.rs");
    assert_eq!(x.ed.rope().to_string(), "fn main() {}\n");
    // Visiting it again switches to the same buffer.
    let n = x.ed.buffer_count();
    x.keys("C-x d").typ("main").keys("RET");
    assert_eq!(x.ed.buffer_count(), n);
    assert_eq!(x.ed.buf.name, "main.rs");
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn find_file_editing_the_path() {
    let (mut x, d) = file_tree();
    x.keys("C-x C-f").typ("src/");
    assert_eq!(file_picker_labels(&x), ["main.rs"]);
    // DEL after a `/` goes up a directory; M-DEL clears the typed component.
    x.keys("DEL");
    assert_eq!(x.ed.picker.as_ref().unwrap().query, format!("{}/", d.display()));
    assert_eq!(file_picker_labels(&x).len(), 3);
    x.typ("REA").keys("M-DEL");
    assert_eq!(x.ed.picker.as_ref().unwrap().query, format!("{}/", d.display()));
    // `//` restarts at the root.
    x.typ("/");
    assert_eq!(x.ed.picker.as_ref().unwrap().query, "/");
    x.keys("C-g");
    assert!(x.ed.picker.is_none());
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn find_file_creates_new_files() {
    let (mut x, d) = file_tree();
    // No match: RET starts a new file; C-j uses the path as typed even if it matches.
    x.keys("C-x C-f").typ("todo.txt").keys("RET");
    assert_eq!(x.ed.buf.name, "todo.txt");
    assert_eq!(x.ed.buf.path.as_deref(), Some(d.join("todo.txt").as_path()));
    assert_eq!(x.ed.message.as_deref(), Some("(New file)"));
    x.keys("C-x C-f").typ("READ").keys("C-j");
    assert_eq!(x.ed.buf.name, "READ");
    // A directory typed literally is entered, not opened.
    x.keys("C-x C-f").typ("src").keys("C-j");
    assert_eq!(x.ed.picker.as_ref().unwrap().query, format!("{}/src/", d.display()));
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn tab_completes_the_selected_candidate() {
    let (mut x, d) = file_tree();
    x.keys("C-x C-f").typ("sr").keys("TAB");
    // A directory is entered...
    assert_eq!(x.ed.picker.as_ref().unwrap().query, format!("{}/src/", d.display()));
    assert_eq!(file_picker_labels(&x), ["main.rs"]);
    // ...a file is only completed: RET then opens it.
    x.keys("TAB");
    assert_eq!(x.ed.picker.as_ref().unwrap().query, format!("{}/src/main.rs", d.display()));
    x.keys("RET");
    assert!(x.ed.picker.is_none());
    assert_eq!(x.ed.buf.name, "main.rs");
    // In other pickers TAB puts the candidate in the query.
    x.keys("M-x").typ("forwrdchar").keys("TAB");
    assert_eq!(x.ed.picker.as_ref().unwrap().query, "forward-char");
    x.keys("C-g");
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn picker_queries_take_the_global_editing_keys() {
    let (mut x, d) = file_tree();
    let dir = format!("{}/", d.display());
    x.keys("C-x C-f").typ("READ");
    // Typing and deleting happen at the cursor.
    x.keys("C-b C-b C-d");
    x.typ("X");
    assert_eq!(x.ed.picker.as_ref().unwrap().query, format!("{dir}REXD"));
    x.keys("DEL M-b");
    assert_eq!(x.ed.picker.as_ref().unwrap().query, format!("{dir}RED"));
    // DEL in the middle deletes a character, not a directory level.
    x.keys("C-f DEL");
    assert_eq!(x.ed.picker.as_ref().unwrap().query, format!("{dir}ED"));
    // C-k kills to the end (it only kills buffers in the buffer picker); C-y yanks it back.
    x.keys("C-k");
    assert_eq!(x.ed.picker.as_ref().unwrap().query, dir);
    assert_eq!(x.ed.kill_ring.last().map(String::as_str), Some("ED"));
    x.keys("C-e C-y");
    assert_eq!(x.ed.picker.as_ref().unwrap().query, format!("{dir}ED"));
    // C-a then C-k empties the query; DEL at the end still goes up a directory.
    x.keys("C-a C-k").typ(&dir).keys("DEL");
    assert_eq!(x.ed.picker.as_ref().unwrap().query, super::files::abbreviate(d.parent().unwrap()) + "/");
    x.keys("C-g");
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn save_buffer_without_a_file_asks_for_one() {
    let d = super::testutil::temp_dir("save");
    let mut x = t("hello|");
    assert!(x.ed.buf.path.is_none());
    x.keys("C-x C-s");
    let p = x.ed.picker.as_ref().expect("save picker open");
    assert_eq!(p.prompt, "Save buffer to");
    assert!(p.saving());
    // `//` restarts the path at the root, so an absolute path can be typed.
    x.typ(&format!("/{}/notes.rs", d.display())).keys("RET");
    assert!(x.ed.picker.is_none());
    let file = d.join("notes.rs");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello");
    // The buffer now visits the file: renamed, unmodified, in the file's mode.
    assert_eq!(x.ed.buf.path.as_deref(), Some(file.as_path()));
    assert_eq!(x.ed.buf.name, "notes.rs");
    assert!(!x.ed.buf.modified());
    assert_eq!(x.ed.major_mode(), "rust-mode");
    assert_eq!(x.ed.message.as_deref(), Some(format!("Wrote {}", file.display()).as_str()));
    // Later saves go straight to the file.
    x.typ("!").keys("C-x C-s");
    assert!(x.ed.picker.is_none());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello!");
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn save_buffer_confirms_overwriting_and_rejects_missing_directories() {
    let d = super::testutil::temp_dir("save");
    let file = d.join("old.txt");
    std::fs::write(&file, "old").unwrap();
    let mut x = t("new|");
    x.keys("C-x C-s").typ(&format!("/{}/nope/a.txt", d.display())).keys("RET");
    let msg = x.ed.picker.as_ref().unwrap().message.clone().unwrap();
    assert!(msg.contains("does not exist"), "{msg}");
    assert!(x.ed.buf.path.is_none());
    x.keys("C-g").keys("C-x C-s").typ(&format!("/{}/old.txt", d.display())).keys("C-j");
    // The first accept only warns; the second overwrites.
    let msg = x.ed.picker.as_ref().unwrap().message.clone().unwrap();
    assert!(msg.contains("exists"), "{msg}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "old");
    x.keys("C-j");
    assert!(x.ed.picker.is_none());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");
    assert_eq!(x.ed.buf.name, "old.txt");
    std::fs::remove_dir_all(d).unwrap();
}

// ================================================================ dired

/// The file name on the line at point in a listing.
fn dired_at(x: &T) -> String {
    let r = &x.ed.buf.rope;
    let p = x.ed.primary_cursor().point;
    let line = r.line(r.char_to_line(p)).to_string();
    assert_eq!(p - r.line_to_char(r.char_to_line(p)), dired::NAME_COL, "point is on the name");
    line.chars().skip(dired::NAME_COL).collect::<String>().trim_end().to_string()
}

fn dired_names(x: &T) -> Vec<String> {
    x.ed.dired_state().expect("a listing").entries.iter().map(|e| e.name.clone()).collect()
}

#[test]
fn dired_lists_visits_and_goes_up() {
    let (mut x, d) = file_tree();
    x.keys("C-x d");
    assert_eq!(x.ed.picker.as_ref().unwrap().prompt, "Dired");
    // RET with nothing typed lists the directory typed.
    x.keys("RET");
    assert!(x.ed.picker.is_none());
    let dir_name = format!("{}/", d.file_name().unwrap().to_string_lossy());
    assert_eq!(x.ed.buf.name, dir_name);
    assert_eq!(x.ed.buf.kind, BufferKind::Dired);
    assert_eq!(x.ed.major_mode(), "dired-mode");
    assert!(x.ed.minor_on(MinorMode::ReadOnly));
    assert_eq!(dired_names(&x), ["..", "src", ".hidden", "README.md"]);
    assert!(x.ed.buf.rope.line(0).to_string().contains(&*d.file_name().unwrap().to_string_lossy()));
    // Point starts on the first entry after `..`.
    assert_eq!(dired_at(&x), "src");
    // The listing can't be typed into.
    let before = x.ed.buf.rope.to_string();
    x.typ("a");
    assert_eq!(x.ed.buf.rope.to_string(), before);
    assert!(!x.ed.buf.modified());
    // Visit a file, then jump back to its entry.
    x.keys("n n");
    assert_eq!(dired_at(&x), "README.md");
    x.keys("RET");
    assert_eq!(x.ed.buf.name, "README.md");
    x.keys("C-x C-j");
    assert_eq!(x.ed.buf.name, dir_name);
    assert_eq!(dired_at(&x), "README.md");
    // Into a subdirectory and back up: point returns to it.
    x.keys("M-< p p p p n");
    assert_eq!(dired_at(&x), "src");
    x.keys("RET");
    assert_eq!(x.ed.buf.name, "src/");
    assert_eq!(dired_at(&x), "main.rs");
    assert_eq!(x.ed.default_directory(), d.join("src"));
    x.keys("^");
    assert_eq!(x.ed.buf.name, dir_name);
    assert_eq!(dired_at(&x), "src");
    // Listings are reused, not duplicated.
    x.keys("RET ^");
    assert_eq!(x.names().iter().filter(|n| **n == dir_name).count(), 1);
    // find-file on a directory typed with a slash lists it.
    x.keys("C-x C-f").typ("src/").keys("C-j");
    assert_eq!(x.ed.buf.name, "src/");
    // q buries the listing.
    x.keys("q");
    assert_ne!(x.ed.buf.name, "src/");
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn dired_marks_flags_and_deletes() {
    let (mut x, d) = file_tree();
    std::fs::write(d.join("a.txt"), "a").unwrap();
    std::fs::write(d.join("b.txt"), "b").unwrap();
    x.keys("C-x d RET");
    assert_eq!(dired_names(&x), ["..", "src", ".hidden", "a.txt", "b.txt", "README.md"]);
    // m marks and moves down; the mark shows in the first column.
    x.keys("n n m");
    assert_eq!(dired_at(&x), "b.txt");
    let line = |x: &T, n: usize| x.ed.buf.rope.line(n).to_string();
    assert!(line(&x, 4).starts_with("* "), "{}", line(&x, 4));
    // `..` can't be marked; U clears every mark; t toggles.
    x.keys("M-< n m");
    assert_eq!(x.msg(), "Can't mark ..");
    x.keys("S-u");
    assert!(x.ed.dired_state().unwrap().entries.iter().all(|e| e.mark == ' '));
    x.keys("t");
    assert_eq!(x.ed.dired_state().unwrap().entries.iter().filter(|e| e.mark == '*').count(), 5);
    x.keys("S-u");
    // d flags; x asks first, deletes when repeated, and asks again next time.
    x.keys("M-< n n n n d");
    assert!(line(&x, 4).starts_with("D "));
    x.keys("x");
    assert!(x.msg().contains("Delete 1 file (a.txt)"), "{}", x.msg());
    assert!(d.join("a.txt").exists());
    x.keys("x");
    assert!(!d.join("a.txt").exists());
    assert_eq!(x.msg(), "Deleted 1 file");
    assert_eq!(dired_names(&x), ["..", "src", ".hidden", "b.txt", "README.md"]);
    // D deletes the entry at point (a directory recursively), after a repeat;
    // anything in between cancels.
    x.keys("M-< n n");
    assert_eq!(dired_at(&x), "src");
    x.keys("S-d");
    assert!(x.msg().contains("recursively"), "{}", x.msg());
    x.keys("g S-d");
    assert!(d.join("src").exists());
    x.keys("S-d");
    assert!(!d.join("src").exists());
    assert_eq!(dired_names(&x), ["..", ".hidden", "b.txt", "README.md"]);
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn dired_renames_copies_and_creates_directories() {
    let (mut x, d) = file_tree();
    x.keys("C-x C-f").typ("README.md").keys("RET");
    assert_eq!(x.ed.buf.name, "README.md");
    x.keys("C-x C-j");
    // + creates a directory and puts point on it.
    x.keys("+");
    assert_eq!(x.ed.picker.as_ref().unwrap().prompt, "Create directory");
    x.typ("docs").keys("RET");
    assert!(d.join("docs").is_dir());
    assert_eq!(dired_at(&x), "docs");
    // C copies the file at point; the target is taken as typed.
    x.keys("M-9 n");
    assert_eq!(dired_at(&x), "README.md");
    x.keys("S-c");
    assert_eq!(x.ed.picker.as_ref().unwrap().prompt, "Copy README.md to");
    x.typ("R").keys("RET");
    assert_eq!(std::fs::read_to_string(d.join("R")).unwrap(), "readme\n");
    assert_eq!(dired_at(&x), "R");
    // Overwriting needs a second RET.
    x.keys("S-c").typ("README.md").keys("RET");
    assert!(x.ed.picker.as_ref().unwrap().message.as_deref().unwrap().contains("exists"));
    x.keys("RET");
    assert!(x.ed.picker.is_none());
    // R renames; a buffer visiting the file follows it.
    x.keys("M-9 n");
    assert_eq!(dired_at(&x), "README.md");
    x.keys("S-r").typ("INTRO.md").keys("RET");
    assert!(!d.join("README.md").exists());
    assert!(d.join("INTRO.md").exists());
    assert!(x.names().contains(&"INTRO.md".to_string()), "{:?}", x.names());
    let visitor = x.ed.others.iter().find(|s| s.buf.name == "INTRO.md").unwrap();
    assert_eq!(visitor.buf.path.as_deref(), Some(d.join("INTRO.md").as_path()));
    // Marked files move into a directory.
    x.keys("M-< n");
    while dired_at(&x) != "INTRO.md" {
        x.keys("n");
    }
    x.keys("m m");
    x.keys("S-r");
    assert_eq!(x.ed.picker.as_ref().unwrap().prompt, "Rename 2 files to");
    x.typ("docs").keys("RET");
    assert!(d.join("docs/INTRO.md").exists() && d.join("docs/R").exists());
    assert_eq!(x.msg(), "Moved 2 files");
    assert!(x.ed.dired_state().unwrap().entries.iter().all(|e| e.mark == ' '));
    // The session remembers listings.
    let s = x.ed.session_snapshot();
    assert!(s.buffers.iter().any(|b| b.kind == session::SavedBufferKind::Directory { path: d.clone() }));
    std::fs::remove_dir_all(d).unwrap();
}
