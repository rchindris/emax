//! Terminal emulator tests: recorded escape sequences in, screen contents,
//! colors, modes and replies out.

use super::*;

fn term(cols: u16, rows: u16) -> Terminal {
    Terminal::new(cols, rows, 100)
}

/// A terminal that has been fed `bytes`.
fn fed(cols: u16, rows: u16, bytes: &str) -> Terminal {
    let mut t = term(cols, rows);
    t.feed(bytes.as_bytes());
    t
}

fn snap(t: &Terminal) -> Snapshot {
    t.snapshot(&Palette::xterm())
}

/// The visible rows as text (trailing blanks trimmed).
fn rows(t: &Terminal) -> Vec<String> {
    let s = snap(t);
    (0..s.rows).map(|r| s.row_text(r)).collect()
}

fn cursor(t: &Terminal) -> (usize, usize) {
    let c = snap(t).cursor;
    (c.row, c.col)
}

fn cell(t: &Terminal, row: usize, col: usize) -> Cell {
    snap(t).lines[row][col].clone()
}

const P: fn() -> Palette = Palette::xterm;

// ------------------------------------------------------------------ text

#[test]
fn blank_terminal() {
    let t = term(10, 3);
    let s = snap(&t);
    assert_eq!((s.cols, s.rows), (10, 3));
    assert_eq!(s.lines.len(), 3);
    assert!(s.lines.iter().all(|l| l.len() == 10));
    assert!(s.lines.iter().flatten().all(|c| c.ch == ' ' && c.fg == P().foreground && c.bg == P().background));
    assert_eq!(s.cursor, CursorState { row: 0, col: 0, shape: CursorShape::Block, visible: true });
    assert_eq!(s.display_offset, 0);
    assert_eq!(s.background, P().background);
    assert_eq!(t.title(), "");
    assert_eq!(t.modes(), Modes::default());
    assert_eq!(t.text(), (String::new(), (0, 0)));
    assert_eq!(t.scrollback_limit(), 100);
}

#[test]
fn plain_text_and_cursor() {
    let t = fed(10, 3, "hello");
    assert_eq!(rows(&t), ["hello", "", ""]);
    assert_eq!(cursor(&t), (0, 5));
}

#[test]
fn line_feed_and_carriage_return() {
    // LF alone moves down without returning (no ONLCR in a raw terminal).
    let t = fed(10, 3, "ab\ncd");
    assert_eq!(rows(&t), ["ab", "  cd", ""]);
    let t = fed(10, 3, "ab\r\ncd");
    assert_eq!(rows(&t), ["ab", "cd", ""]);
    // CR alone overwrites the line.
    let t = fed(10, 3, "abcdef\rXY");
    assert_eq!(rows(&t), ["XYcdef", "", ""]);
    assert_eq!(cursor(&t), (0, 2));
}

#[test]
fn backspace_and_tab() {
    let t = fed(20, 2, "abc\x08\x08X");
    assert_eq!(rows(&t)[0], "aXc");
    let t = fed(20, 2, "a\tb");
    assert_eq!(rows(&t)[0], "a       b");
    assert_eq!(cursor(&t), (0, 9));
    assert_eq!(t.text(), ("a       b".into(), (0, 9)), "tabs copy as the spaces shown");
}

#[test]
fn wrapping_at_the_right_margin() {
    let t = fed(5, 3, "abcdefgh");
    assert_eq!(rows(&t), ["abcde", "fgh", ""]);
    assert_eq!(cursor(&t), (1, 3));
    // Exactly filling a line leaves the cursor pending on the last column.
    let t = fed(5, 3, "abcde");
    assert_eq!(rows(&t), ["abcde", "", ""]);
    assert_eq!(cursor(&t), (0, 4));
    // ... and a CR LF then doesn't produce an extra blank line.
    let t = fed(5, 3, "abcde\r\nx");
    assert_eq!(rows(&t), ["abcde", "x", ""]);
}

#[test]
fn autowrap_off() {
    let t = fed(5, 2, "\x1b[?7labcdefgh");
    assert_eq!(rows(&t), ["abcdh", ""]);
}

#[test]
fn scrolling_past_the_bottom() {
    let t = fed(10, 3, "1\r\n2\r\n3\r\n4\r\n5");
    assert_eq!(rows(&t), ["3", "4", "5"]);
    assert_eq!(t.history_len(), 2);
    assert_eq!(cursor(&t), (2, 1));
}

// ------------------------------------------------------- cursor movement

#[test]
fn cursor_position_cup() {
    let t = fed(10, 5, "\x1b[3;4HX");
    assert_eq!(rows(&t)[2], "   X");
    assert_eq!(cursor(&t), (2, 4));
    // Defaults to 1;1; clamps beyond the screen.
    let t = fed(10, 5, "abc\x1b[HX");
    assert_eq!(rows(&t)[0], "Xbc");
    let t = fed(10, 5, "\x1b[99;99HX");
    assert_eq!(rows(&t)[4], "         X");
    let t = fed(10, 5, "\x1b[2;3fY");
    assert_eq!(rows(&t)[1], "  Y");
}

#[test]
fn relative_cursor_movement() {
    // CUU, CUD, CUF, CUB with counts, clamped at the edges.
    let mut t = fed(10, 5, "\x1b[3;3H");
    t.feed(b"\x1b[A");
    assert_eq!(cursor(&t), (1, 2));
    t.feed(b"\x1b[2B");
    assert_eq!(cursor(&t), (3, 2));
    t.feed(b"\x1b[3C");
    assert_eq!(cursor(&t), (3, 5));
    t.feed(b"\x1b[4D");
    assert_eq!(cursor(&t), (3, 1));
    t.feed(b"\x1b[99A");
    assert_eq!(cursor(&t), (0, 1));
    t.feed(b"\x1b[99D");
    assert_eq!(cursor(&t), (0, 0));
    t.feed(b"\x1b[99C\x1b[99B");
    assert_eq!(cursor(&t), (4, 9));
    // CHA (column absolute), VPA (row absolute), CNL/CPL.
    t.feed(b"\x1b[3G");
    assert_eq!(cursor(&t), (4, 2));
    t.feed(b"\x1b[2d");
    assert_eq!(cursor(&t), (1, 2));
    t.feed(b"\x1b[2E");
    assert_eq!(cursor(&t), (3, 0));
    t.feed(b"\x1b[5;5H\x1b[F");
    assert_eq!(cursor(&t), (3, 0));
}

#[test]
fn save_and_restore_cursor() {
    let t = fed(10, 3, "ab\x1b7\x1b[3;1Hzz\x1b8X");
    assert_eq!(rows(&t), ["abX", "", "zz"]);
    let t = fed(10, 3, "ab\x1b[s\x1b[3;1H\x1b[uY");
    assert_eq!(rows(&t)[0], "abY");
}

// ------------------------------------------------------------------ erase

#[test]
fn erase_in_line() {
    let t = fed(10, 1, "abcdefgh\x1b[1;4H\x1b[K");
    assert_eq!(rows(&t)[0], "abc");
    let t = fed(10, 1, "abcdefgh\x1b[1;4H\x1b[1K");
    assert_eq!(rows(&t)[0], "    efgh");
    let t = fed(10, 1, "abcdefgh\x1b[1;4H\x1b[2K");
    assert_eq!(rows(&t)[0], "");
    assert_eq!(cursor(&t), (0, 3));
    // ECH erases n characters without moving; DCH deletes; ICH inserts.
    let t = fed(10, 1, "abcdefgh\x1b[1;2H\x1b[3X");
    assert_eq!(rows(&t)[0], "a   efgh");
    let t = fed(10, 1, "abcdefgh\x1b[1;2H\x1b[2P");
    assert_eq!(rows(&t)[0], "adefgh");
    let t = fed(10, 1, "abcdef\x1b[1;2H\x1b[2@");
    assert_eq!(rows(&t)[0], "a  bcdef");
}

#[test]
fn erase_in_display() {
    let screen = "111\r\n222\r\n333\x1b[2;2H";
    let t = fed(5, 3, &format!("{screen}\x1b[J"));
    assert_eq!(rows(&t), ["111", "2", ""]);
    // (alacritty leaves the first row alone when the cursor is on the second.)
    let t = fed(5, 4, "111\r\n222\r\n333\r\n444\x1b[3;2H\x1b[1J");
    assert_eq!(rows(&t), ["", "", "  3", "444"]);
    let t = fed(5, 3, &format!("{screen}\x1b[2J"));
    assert_eq!(rows(&t), ["", "", ""]);
    assert_eq!(cursor(&t), (1, 1));
}

#[test]
fn erase_scrollback_ed3() {
    let mut t = fed(5, 2, "1\r\n2\r\n3\r\n4");
    assert_eq!(t.history_len(), 2);
    t.feed(b"\x1b[3J");
    assert_eq!(t.history_len(), 0);
    assert_eq!(rows(&t), ["3", "4"]);
}

#[test]
fn insert_and_delete_lines() {
    let t = fed(5, 4, "a\r\nb\r\nc\r\nd\x1b[2;1H\x1b[L");
    assert_eq!(rows(&t), ["a", "", "b", "c"]);
    let t = fed(5, 4, "a\r\nb\r\nc\r\nd\x1b[2;1H\x1b[2M");
    assert_eq!(rows(&t), ["a", "d", "", ""]);
}

// ---------------------------------------------------------------- colors

#[test]
fn sgr_16_colors() {
    let p = P();
    let t = fed(20, 1, "\x1b[31mr\x1b[42mg\x1b[0mn\x1b[94mB\x1b[103mY\x1b[39;49md");
    assert_eq!((cell(&t, 0, 0).fg, cell(&t, 0, 0).bg), (p.ansi[1], p.background));
    assert_eq!((cell(&t, 0, 1).fg, cell(&t, 0, 1).bg), (p.ansi[1], p.ansi[2]));
    assert_eq!((cell(&t, 0, 2).fg, cell(&t, 0, 2).bg), (p.foreground, p.background));
    assert_eq!(cell(&t, 0, 3).fg, p.ansi[12]);
    assert_eq!(cell(&t, 0, 4).bg, p.ansi[11]);
    assert_eq!((cell(&t, 0, 5).fg, cell(&t, 0, 5).bg), (p.foreground, p.background));
    for i in 0..8 {
        let t = fed(5, 1, &format!("\x1b[{}m\x1b[{}mx", 30 + i, 40 + i));
        assert_eq!((cell(&t, 0, 0).fg, cell(&t, 0, 0).bg), (p.ansi[i], p.ansi[i]), "color {i}");
        let t = fed(5, 1, &format!("\x1b[{}m\x1b[{}mx", 90 + i, 100 + i));
        assert_eq!((cell(&t, 0, 0).fg, cell(&t, 0, 0).bg), (p.ansi[i + 8], p.ansi[i + 8]), "bright {i}");
    }
}

#[test]
fn sgr_256_colors() {
    let p = P();
    let fg = |n: u8| cell(&fed(5, 1, &format!("\x1b[38;5;{n}mx")), 0, 0).fg;
    let bg = |n: u8| cell(&fed(5, 1, &format!("\x1b[48;5;{n}mx")), 0, 0).bg;
    assert_eq!(fg(1), p.ansi[1]);
    assert_eq!(fg(15), p.ansi[15]);
    assert_eq!(fg(16), 0x000000ff);
    assert_eq!(fg(196), 0xff0000ff);
    assert_eq!(fg(21), 0x0000ffff);
    assert_eq!(fg(110), 0x87afd7ff); // 110 - 16 = 94 = 2*36 + 3*6 + 4
    assert_eq!(fg(231), 0xffffffff);
    assert_eq!(fg(232), 0x080808ff);
    assert_eq!(fg(244), 0x808080ff);
    assert_eq!(fg(255), 0xeeeeeeff);
    assert_eq!(bg(46), 0x00ff00ff);
    // Colon-separated form.
    assert_eq!(cell(&fed(5, 1, "\x1b[38:5:196mx"), 0, 0).fg, 0xff0000ff);
}

#[test]
fn palette_indexed_matches_xterm_formula() {
    let p = P();
    for i in 16..=231u8 {
        let n = i - 16;
        let level = |v: u8| if v == 0 { 0 } else { 55 + 40 * v as u32 };
        let want = level(n / 36) << 24 | level(n / 6 % 6) << 16 | level(n % 6) << 8 | 0xff;
        assert_eq!(p.indexed(i), want, "{i}");
    }
    for i in 232..=255u8 {
        let v = 8 + 10 * (i as u32 - 232);
        assert_eq!(p.indexed(i), v << 24 | v << 16 | v << 8 | 0xff, "{i}");
    }
}

#[test]
fn sgr_truecolor() {
    let t = fed(5, 1, "\x1b[38;2;10;20;30m\x1b[48;2;200;100;0mx");
    assert_eq!(cell(&t, 0, 0).fg, 0x0a141eff);
    assert_eq!(cell(&t, 0, 0).bg, 0xc86400ff);
    let t = fed(5, 1, "\x1b[38:2::1:2:3mx");
    assert_eq!(cell(&t, 0, 0).fg, 0x010203ff);
    // Bold doesn't brighten truecolor.
    let t = fed(5, 1, "\x1b[1;38;2;1;2;3mx");
    assert_eq!(cell(&t, 0, 0).fg, 0x010203ff);
}

#[test]
fn sgr_attributes() {
    let t = fed(20, 1, "\x1b[1ma\x1b[22;3mb\x1b[23;4mc\x1b[24;9md\x1b[29;2me\x1b[0;8mf\x1b[0;4:3mg\x1b[0mh");
    let f = |c| cell(&t, 0, c).flags;
    assert!(f(0).bold && !f(0).italic);
    assert!(!f(1).bold && f(1).italic);
    assert!(f(2).underline && !f(2).italic);
    assert!(f(3).strikethrough && !f(3).underline);
    assert!(f(4).dim && !f(4).strikethrough);
    assert!(f(5).hidden && !f(5).dim);
    assert!(f(6).underline, "curly underline counts as underline");
    assert_eq!(f(7), CellFlags::default());
}

#[test]
fn bold_is_bright_for_the_first_eight_colors() {
    let p = P();
    let t = fed(10, 1, "\x1b[1;31ma\x1b[1;38;5;2mb\x1b[1;91mc\x1b[1;38;5;100md\x1b[0;1me");
    assert_eq!(cell(&t, 0, 0).fg, p.ansi[9]);
    assert_eq!(cell(&t, 0, 1).fg, p.ansi[10]);
    assert_eq!(cell(&t, 0, 2).fg, p.ansi[9]);
    assert_eq!(cell(&t, 0, 3).fg, p.indexed(100));
    assert_eq!(cell(&t, 0, 4).fg, p.foreground);
    // The background is never brightened.
    let t = fed(5, 1, "\x1b[1;41mx");
    assert_eq!(cell(&t, 0, 0).bg, p.ansi[1]);
}

#[test]
fn dim_and_hidden_colors() {
    let p = P();
    let t = fed(5, 1, "\x1b[2;38;2;150;90;30mx");
    assert_eq!(cell(&t, 0, 0).fg, 0x643c14ff);
    let t = fed(5, 1, "\x1b[2;31mx");
    assert_eq!(cell(&t, 0, 0).fg, dimmed(p.ansi[1]));
    // Bold and dim: dim wins, no brightening.
    let t = fed(5, 1, "\x1b[1;2;31mx");
    assert_eq!(cell(&t, 0, 0).fg, dimmed(p.ansi[1]));
    let t = fed(5, 1, "\x1b[8;31;42mx");
    assert_eq!(cell(&t, 0, 0).fg, p.ansi[2]);
    assert_eq!(cell(&t, 0, 0).bg, p.ansi[2]);
}

#[test]
fn inverse_video() {
    let p = P();
    let t = fed(5, 1, "\x1b[7mx\x1b[31;42my\x1b[27mz");
    assert_eq!((cell(&t, 0, 0).fg, cell(&t, 0, 0).bg), (p.background, p.foreground));
    assert_eq!((cell(&t, 0, 1).fg, cell(&t, 0, 1).bg), (p.ansi[2], p.ansi[1]));
    assert_eq!((cell(&t, 0, 2).fg, cell(&t, 0, 2).bg), (p.ansi[1], p.ansi[2]));
}

#[test]
fn snapshot_uses_the_theme_palette() {
    use crate::engine::config::Config;
    let themes = Config::builtin().appearance.themes;
    for theme in [&themes.dark, &themes.light] {
        let p = Palette::from_theme(theme);
        assert_eq!(p.ansi, theme.ansi());
        let t = fed(10, 1, "\x1b[31ma\x1b[1;32mb\x1b[0;7mc\x1b[0md");
        let s = t.snapshot(&p);
        assert_eq!(s.background, theme.background);
        assert_eq!(s.lines[0][0].fg, theme.term_red);
        assert_eq!(s.lines[0][1].fg, theme.term_bright_green);
        assert_eq!((s.lines[0][2].fg, s.lines[0][2].bg), (theme.background, theme.foreground));
        assert_eq!((s.lines[0][3].fg, s.lines[0][3].bg), (theme.foreground, theme.background));
    }
}

#[test]
fn programs_can_redefine_colors() {
    let p = P();
    // OSC 4 redefines color 1; OSC 10/11 the default foreground/background.
    let t = fed(5, 1, "\x1b]4;1;rgb:12/34/56\x07\x1b]11;rgb:01/02/03\x07\x1b]10;#aabbcc\x07\x1b[31ma\x1b[0mb");
    assert_eq!(cell(&t, 0, 0).fg, 0x123456ff);
    assert_eq!(cell(&t, 0, 1).fg, 0xaabbccff);
    assert_eq!(cell(&t, 0, 1).bg, 0x010203ff);
    assert_eq!(snap(&t).background, 0x010203ff);
    // OSC 104 resets.
    let mut t = t;
    t.feed(b"\x1b]104;1\x07");
    assert_eq!(cell(&t, 0, 0).fg, p.ansi[1]);
}

#[test]
fn color_queries_are_answered_with_the_drawn_palette() {
    let mut t = term(10, 2);
    let reply = t.feed(b"\x1b]11;?\x07").replies;
    assert_eq!(String::from_utf8(reply).unwrap(), "\x1b]11;rgb:0000/0000/0000\x07");
    let mut p = P();
    p.background = 0xfdf6e3ff;
    p.ansi[1] = 0xdc322fff;
    t.snapshot(&p);
    let reply = t.feed(b"\x1b]11;?\x1b\\").replies;
    assert_eq!(String::from_utf8(reply).unwrap(), "\x1b]11;rgb:fdfd/f6f6/e3e3\x1b\\");
    let reply = t.feed(b"\x1b]4;1;?\x07").replies;
    assert_eq!(String::from_utf8(reply).unwrap(), "\x1b]4;1;rgb:dcdc/3232/2f2f\x07");
}

// ------------------------------------------------------------ alt screen

#[test]
fn alternate_screen_enter_and_exit() {
    let mut t = fed(10, 3, "shell$ vim");
    t.feed(b"\x1b[?1049h");
    assert!(t.modes().alt_screen);
    assert_eq!(rows(&t), ["", "", ""], "the alternate screen starts blank");
    t.feed(b"\x1b[Hfull screen\x1b[3;1H~");
    assert_eq!(rows(&t)[2], "~");
    assert_eq!(t.text().0, "full screen\n~", "the wrapped line is joined");
    t.feed(b"\x1b[?1049l");
    assert!(!t.modes().alt_screen);
    assert_eq!(rows(&t), ["shell$ vim", "", ""]);
    assert_eq!(cursor(&t), (0, 9), "the cursor is restored (pending wrap at the margin)");
}

#[test]
fn alternate_screen_keeps_no_scrollback() {
    let mut t = fed(10, 2, "a\r\nb\r\nc");
    let history = t.history_len();
    t.feed(b"\x1b[?1049h");
    for i in 0..20 {
        t.feed(format!("{i}\r\n").as_bytes());
    }
    assert_eq!(t.history_len(), 0);
    t.scroll(5);
    assert_eq!(t.display_offset(), 0, "nothing to scroll back to on the alternate screen");
    t.feed(b"\x1b[?1049l");
    assert_eq!(t.history_len(), history);
    assert_eq!(rows(&t), ["b", "c"]);
}

// ---------------------------------------------------------- scroll region

#[test]
fn scroll_region() {
    // Rows 2–3 scroll; rows 1 and 4 stay put.
    let mut t = fed(10, 4, "top\r\nr2\r\nr3\r\nbottom");
    t.feed(b"\x1b[2;3r\x1b[3;1H\nnew");
    assert_eq!(rows(&t), ["top", "r3", "new", "bottom"]);
    // Text scrolled out of a region (not at the top) is not history.
    assert_eq!(t.history_len(), 0);
    // Reverse index at the region's top scrolls it down.
    t.feed(b"\x1b[2;1H\x1bM");
    assert_eq!(rows(&t), ["top", "", "r3", "bottom"]);
    // SU / SD scroll the region.
    t.feed(b"\x1b[S");
    assert_eq!(rows(&t), ["top", "r3", "", "bottom"]);
    t.feed(b"\x1b[T");
    assert_eq!(rows(&t), ["top", "", "r3", "bottom"]);
    // Resetting the region.
    t.feed(b"\x1b[r\x1b[4;1H\n");
    assert_eq!(rows(&t), ["", "r3", "bottom", ""]);
}

// ------------------------------------------------- scrollback & viewport

#[test]
fn scrollback_is_limited() {
    let mut t = Terminal::new(10, 3, 5);
    for i in 0..50 {
        t.feed(format!("line {i}\r\n").as_bytes());
    }
    assert_eq!(t.history_len(), 5);
    let (text, _) = t.text();
    assert_eq!(text.lines().next(), Some("line 43"));
    assert_eq!(text.lines().filter(|l| !l.is_empty()).count(), 7);

    let mut none = Terminal::new(10, 3, 0);
    for i in 0..10 {
        none.feed(format!("{i}\r\n").as_bytes());
    }
    assert_eq!(none.history_len(), 0);
    assert_eq!(rows(&none), ["8", "9", ""]);
}

#[test]
fn viewport_scrolling() {
    let mut t = Terminal::new(10, 3, 100);
    for i in 0..10 {
        t.feed(format!("{i}\r\n").as_bytes());
    }
    t.feed(b"$");
    assert_eq!(rows(&t), ["8", "9", "$"]);
    t.scroll(2);
    assert_eq!(t.display_offset(), 2);
    assert_eq!(rows(&t), ["6", "7", "8"]);
    let s = snap(&t);
    assert_eq!(s.display_offset, 2);
    assert!(!s.cursor.visible, "the cursor is below the view");
    t.scroll(-1);
    assert_eq!(rows(&t), ["7", "8", "9"]);
    t.scroll(1000);
    assert_eq!(t.display_offset(), 8, "clamped to the history");
    assert_eq!(rows(&t)[0], "0");
    t.scroll(-1000);
    assert_eq!(t.display_offset(), 0);
    t.scroll(3);
    t.scroll_to_bottom();
    assert_eq!(t.display_offset(), 0);
    assert!(snap(&t).cursor.visible);
}

#[test]
fn scrolled_view_stays_put_while_output_arrives() {
    let mut t = Terminal::new(10, 3, 100);
    for i in 0..10 {
        t.feed(format!("{i}\r\n").as_bytes());
    }
    t.scroll(3);
    let before = rows(&t);
    t.feed(b"more\r\nand more\r\n");
    assert_eq!(rows(&t), before, "the history view doesn't move under the reader");
    assert_eq!(t.display_offset(), 5);
}

#[test]
fn region_scrolls_in_a_scrolled_back_view_keep_the_offset_within_the_history() {
    // alacritty grows the display offset on some region scrolls without
    // capping it; reading rows above the history must not happen.
    let mut t = Terminal::new(10, 3, 100);
    for i in 0..20 {
        t.feed(format!("{i}\r\n").as_bytes());
    }
    t.scroll(1000);
    let top = t.display_offset();
    assert_eq!(top, t.history_len());
    t.feed(b"\x1b[2;3r\x1b[2H\x1b[M");
    assert!(t.display_offset() <= t.history_len());
    let s = snap(&t);
    assert_eq!(s.lines.len(), 3);
    assert!(s.display_offset <= t.history_len());
    // Origin mode and deleted lines after scrolling back, too.
    t.scroll(1000);
    t.feed(b"\x1b[?6h\x1b[2;3r\x1b[H\x1b[2M\x1b[?6l\x1b[r");
    assert!(t.display_offset() <= t.history_len());
    let _ = snap(&t);
}

// ------------------------------------------------------------------ resize

#[test]
fn resize_reflows_wrapped_lines() {
    let mut t = fed(10, 3, "abcdefghijkl");
    assert_eq!(rows(&t), ["abcdefghij", "kl", ""]);
    t.resize(6, 3);
    assert_eq!(t.size(), (6, 3));
    assert_eq!(rows(&t), ["abcdef", "ghijkl", ""]);
    t.resize(20, 3);
    assert_eq!(rows(&t), ["abcdefghijkl", "", ""]);
    assert_eq!(t.text().0, "abcdefghijkl");
}

#[test]
fn resize_rows_keeps_the_cursor_line() {
    let mut t = fed(10, 5, "1\r\n2\r\n3\r\n4\r\n5");
    t.resize(10, 2);
    assert_eq!(rows(&t), ["4", "5"]);
    assert_eq!(cursor(&t), (1, 1));
    assert_eq!(t.history_len(), 3);
    t.resize(10, 4);
    assert_eq!(rows(&t), ["2", "3", "4", "5"], "history comes back down");
}

#[test]
fn resize_is_clamped() {
    let mut t = term(10, 3);
    t.resize(0, 0);
    assert_eq!(t.size(), (2, 1));
    t.resize(u16::MAX, u16::MAX);
    assert_eq!(t.size(), (4096, 2048));
    let t = Terminal::new(1, 0, 0);
    assert_eq!(t.size(), (2, 1));
    // A cursor past the new edge is pulled in.
    let mut t = fed(20, 5, "\x1b[5;20H");
    t.resize(5, 2);
    let (r, c) = cursor(&t);
    assert!(r < 2 && c < 5, "cursor at {r},{c}");
}

#[test]
fn resize_with_wide_chars_does_not_split_them() {
    let mut t = fed(10, 4, "ab日本語cd");
    for cols in [2u16, 3, 4, 5, 7, 9, 10, 15] {
        t.resize(cols, 4);
        let s = snap(&t);
        for line in &s.lines {
            for (i, c) in line.iter().enumerate() {
                if c.flags.wide {
                    assert!(i + 1 < line.len() && line[i + 1].flags.wide_spacer, "{cols} cols: wide char without spacer");
                }
            }
        }
        assert_eq!(t.text().0.replace('\n', ""), "ab日本語cd", "{cols} cols");
    }
}

// ----------------------------------------------- wide & combining chars

#[test]
fn wide_characters() {
    let t = fed(10, 2, "a日b");
    let s = snap(&t);
    assert_eq!(s.lines[0][1].ch, '日');
    assert!(s.lines[0][1].flags.wide);
    assert!(s.lines[0][2].flags.wide_spacer);
    assert_eq!(s.lines[0][3].ch, 'b');
    assert_eq!(s.row_text(0), "a日b");
    assert_eq!(cursor(&t), (0, 4));
    assert_eq!(t.text(), ("a日b".into(), (0, 3)));
}

#[test]
fn emoji_are_wide() {
    let t = fed(10, 1, "😀x");
    let s = snap(&t);
    assert_eq!(s.lines[0][0].ch, '😀');
    assert!(s.lines[0][0].flags.wide);
    assert!(s.lines[0][1].flags.wide_spacer);
    assert_eq!(s.lines[0][2].ch, 'x');
}

#[test]
fn wide_char_at_the_margin_wraps_whole() {
    let t = fed(5, 2, "abcd日");
    let s = snap(&t);
    assert_eq!(s.row_text(0), "abcd");
    assert!(s.lines[0][4].flags.wide_spacer, "leading spacer fills the last column");
    assert_eq!(s.lines[1][0].ch, '日');
    assert_eq!(t.text().0, "abcd日");
}

#[test]
fn cursor_on_a_wide_char_spacer_points_at_the_char() {
    let t = fed(10, 1, "日本\x1b[1;2H");
    assert_eq!(cursor(&t), (0, 0));
}

#[test]
fn combining_characters() {
    let t = fed(10, 1, "e\u{301}a\u{308}\u{323}x");
    let s = snap(&t);
    assert_eq!(s.lines[0][0].ch, 'e');
    assert_eq!(s.lines[0][0].zerowidth, ['\u{301}']);
    assert_eq!(s.lines[0][1].zerowidth, ['\u{308}', '\u{323}']);
    assert_eq!(s.lines[0][2].ch, 'x');
    assert_eq!(cursor(&t), (0, 3));
    assert_eq!(t.text().0, "e\u{301}a\u{308}\u{323}x");
    assert_eq!(t.text().1, (0, 6), "the cursor column counts characters");
}

#[test]
fn utf8_split_across_feeds() {
    let mut t = term(10, 1);
    let bytes = "日本😀é".as_bytes();
    for b in bytes {
        t.feed(std::slice::from_ref(b));
    }
    assert_eq!(snap(&t).row_text(0), "日本😀é");
}

#[test]
fn escape_sequence_split_across_feeds() {
    let mut t = term(10, 2);
    for b in b"\x1b[2;3H\x1b[31mX" {
        t.feed(std::slice::from_ref(b));
    }
    assert_eq!(rows(&t)[1], "  X");
    assert_eq!(cell(&t, 1, 2).fg, P().ansi[1]);
}

#[test]
fn invalid_utf8_shows_replacement_characters() {
    let t = {
        let mut t = term(10, 1);
        t.feed(b"a\xffb\xc3");
        t.feed(b"(c");
        t
    };
    assert_eq!(snap(&t).row_text(0), "a\u{fffd}b\u{fffd}(c");
}

// ----------------------------------------------------- title, bell, modes

#[test]
fn titles() {
    let mut t = term(10, 1);
    let f = t.feed(b"\x1b]0;first\x07");
    assert_eq!(f.title.as_deref(), Some("first"));
    assert_eq!(t.title(), "first");
    let f = t.feed(b"\x1b]2;second\x1b\\");
    assert_eq!(f.title.as_deref(), Some("second"));
    assert_eq!(t.title(), "second");
    // OSC 1 (icon name) doesn't change the title.
    let f = t.feed(b"\x1b]1;icon\x07plain text");
    assert_eq!(f.title, None);
    assert_eq!(t.title(), "second");
    // Split across feeds; the last of several wins.
    assert_eq!(t.feed(b"\x1b]2;ti").title, None);
    let f = t.feed(b"tle\x07\x1b]2;last\x07");
    assert_eq!(f.title.as_deref(), Some("last"));
    // UTF-8 titles.
    assert_eq!(t.feed("\x1b]2;日本 ✓\x07".as_bytes()).title.as_deref(), Some("日本 ✓"));
    // Push/pop the title stack (CSI 22/23 t).
    t.feed(b"\x1b[22t\x1b]2;temp\x07");
    assert_eq!(t.title(), "temp");
    t.feed(b"\x1b[23t");
    assert_eq!(t.title(), "日本 ✓");
}

#[test]
fn bell() {
    let mut t = term(10, 1);
    assert!(t.feed(b"ding\x07").bell);
    assert!(!t.feed(b"quiet").bell);
    // A BEL that ends an OSC is not a bell.
    assert!(!t.feed(b"\x1b]2;x\x07").bell);
    assert_eq!(snap(&t).row_text(0), "dingquiet");
}

#[test]
fn device_status_report_replies() {
    let mut t = fed(10, 5, "\x1b[3;7H");
    assert_eq!(t.feed(b"\x1b[6n").replies, b"\x1b[3;7R");
    assert_eq!(t.feed(b"\x1b[5n").replies, b"\x1b[0n");
    // Several queries in one feed reply in order.
    assert_eq!(t.feed(b"\x1b[H\x1b[6nab\x1b[6n").replies, b"\x1b[1;1R\x1b[1;3R");
    assert!(t.feed(b"plain").replies.is_empty());
}

#[test]
fn device_attributes_replies() {
    let mut t = term(10, 1);
    assert_eq!(t.feed(b"\x1b[c").replies, b"\x1b[?6c");
    assert_eq!(t.feed(b"\x1b[0c").replies, b"\x1b[?6c");
    let secondary = String::from_utf8(t.feed(b"\x1b[>c").replies).unwrap();
    assert!(secondary.starts_with("\x1b[>0;") && secondary.ends_with(";1c"), "{secondary:?}");
}

#[test]
fn mode_and_size_queries_reply() {
    let mut t = term(80, 24);
    t.feed(b"\x1b[?2004h");
    assert_eq!(t.feed(b"\x1b[?2004$p").replies, b"\x1b[?2004;1$y");
    assert_eq!(t.feed(b"\x1b[18t").replies, b"\x1b[8;24;80t");
}

#[test]
fn bracketed_paste_mode() {
    let mut t = term(10, 1);
    assert!(!t.modes().bracketed_paste);
    t.feed(b"\x1b[?2004h");
    assert!(t.modes().bracketed_paste);
    t.feed(b"\x1b[?2004l");
    assert!(!t.modes().bracketed_paste);
}

#[test]
fn application_cursor_and_keypad_modes() {
    use crate::engine::keymap::Key;
    let mut t = term(10, 1);
    let up = Key::parse("<up>");
    assert_eq!(input::encode_key(&up, t.modes()).unwrap(), b"\x1b[A");
    t.feed(b"\x1b[?1h\x1b=");
    assert!(t.modes().app_cursor && t.modes().app_keypad);
    assert_eq!(input::encode_key(&up, t.modes()).unwrap(), b"\x1bOA");
    t.feed(b"\x1b[?1l\x1b>");
    assert!(!t.modes().app_cursor && !t.modes().app_keypad);
    assert_eq!(input::encode_key(&up, t.modes()).unwrap(), b"\x1b[A");
}

#[test]
fn mouse_modes() {
    let mut t = term(10, 1);
    for (on, off) in [("\x1b[?1000h", "\x1b[?1000l"), ("\x1b[?1002h", "\x1b[?1002l"), ("\x1b[?1003h", "\x1b[?1003l")] {
        t.feed(on.as_bytes());
        assert!(t.modes().mouse, "{on:?}");
        t.feed(off.as_bytes());
        assert!(!t.modes().mouse, "{off:?}");
    }
}

#[test]
fn cursor_visibility_and_shape() {
    let mut t = term(10, 1);
    t.feed(b"\x1b[?25l");
    assert!(!snap(&t).cursor.visible);
    t.feed(b"\x1b[?25h");
    assert!(snap(&t).cursor.visible);
    for (seq, shape) in [
        ("\x1b[2 q", CursorShape::Block),
        ("\x1b[4 q", CursorShape::Underline),
        ("\x1b[6 q", CursorShape::Beam),
        ("\x1b[3 q", CursorShape::Underline),
        ("\x1b[5 q", CursorShape::Beam),
        ("\x1b[0 q", CursorShape::Block),
    ] {
        t.feed(seq.as_bytes());
        assert_eq!(snap(&t).cursor.shape, shape, "{seq:?}");
    }
}

#[test]
fn full_reset() {
    let mut t = fed(10, 2, "\x1b]2;title\x07\x1b[?1h\x1b[?2004h\x1b[31mabc\r\n\r\n\r\n");
    t.feed(b"\x1bc");
    assert_eq!(rows(&t), ["", ""]);
    assert!(!t.modes().app_cursor && !t.modes().bracketed_paste);
    assert_eq!(t.history_len(), 0);
    t.feed(b"x");
    assert_eq!(cell(&t, 0, 0).fg, P().foreground);
}

// -------------------------------------------------------------- copy text

#[test]
fn text_joins_wrapped_lines_and_trims() {
    let t = fed(5, 4, "abcdefg  \r\nxy   \r\n");
    assert_eq!(t.text(), ("abcdefg\nxy\n".into(), (2, 0)));
    // Trailing blank lines below the cursor are dropped.
    let t = fed(5, 4, "ab");
    assert_eq!(t.text(), ("ab".into(), (0, 2)));
    // Blank lines before the cursor are kept.
    let t = fed(5, 4, "\r\n\r\nz");
    assert_eq!(t.text(), ("\n\nz".into(), (2, 1)));
}

#[test]
fn text_includes_history_and_tracks_the_cursor() {
    let mut t = Terminal::new(10, 2, 100);
    t.feed(b"one\r\ntwo\r\nthree\r\n$ ls");
    assert_eq!(t.text(), ("one\ntwo\nthree\n$ ls".into(), (3, 4)));
    // Scrolling the view doesn't change the text or the cursor position.
    t.scroll(2);
    assert_eq!(t.text(), ("one\ntwo\nthree\n$ ls".into(), (3, 4)));
}

#[test]
fn text_cursor_column_is_clamped_to_trimmed_text() {
    let t = fed(10, 1, "$ \x1b[1;8H");
    assert_eq!(t.text(), ("$".into(), (0, 1)));
}

#[test]
fn text_cursor_in_a_wrapped_line() {
    let t = fed(4, 3, "abcdefgh\x1b[2;2H");
    assert_eq!(t.text(), ("abcdefgh".into(), (0, 5)));
}

// ----------------------------------------------------------------- fuzzing

/// A small deterministic PRNG (xorshift), so failures reproduce.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Pieces that make random input reach deep into the parser.
const FRAGMENTS: &[&[u8]] = &[
    b"\x1b[", b"\x1b]", b"\x1bP", b"\x1b[?", b"\x07", b"\x1b\\", b";", b"1049h", b"1049l", b"2004h", b"m", b"H",
    b"J", b"K", b"r", b"38;5;", b"48;2;", b"\x1b[6n", b"\x1b[c", b"\x1b]4;1;?\x07", b"\x1b]11;?\x07", "日本".as_bytes(),
    "😀".as_bytes(), "e\u{301}".as_bytes(), b"\r\n", b"\x1bc", b"\x1b[3J", b"\x1b[?1h", b"\x1b7", b"\x1b8", b"\x1b[999;999H",
    b"\x1b[4294967295A", b"\x1b[2;1r", b"\x1b[L", b"\x1b[M", b"\x1b[@", b"\x1b[P", b"\x1b[X", b"\x1b[b", b"\t",
    b"\x1b[22t", b"\x1b[23t", b"\x1b[?2026h", b"\x1b[?2026l", b"\x1b]8;;http://x\x07", b"\x1b]52;c;aGk=\x07",
];

#[test]
fn random_input_never_panics() {
    let mut rng = Rng(0x9e3779b97f4a7c15);
    let palette = P();
    for round in 0..40 {
        let mut t = Terminal::new(1 + rng.below(100) as u16, 1 + rng.below(40) as u16, rng.below(200) as usize);
        for _ in 0..300 {
            let mut chunk = Vec::new();
            for _ in 0..rng.below(64) {
                if rng.below(3) == 0 {
                    chunk.extend_from_slice(FRAGMENTS[rng.below(FRAGMENTS.len() as u64) as usize]);
                } else {
                    chunk.push(rng.next() as u8);
                }
            }
            t.feed(&chunk);
            match rng.below(20) {
                0 => t.resize(rng.below(120) as u16, rng.below(50) as u16),
                1..=3 => t.scroll(rng.below(200) as i32 - 100),
                4 => t.scroll_to_bottom(),
                _ => {}
            }
            // The view renders between feeds, often scrolled back.
            assert!(t.display_offset() <= t.history_len(), "round {round}");
            let _ = t.snapshot(&palette);
        }
        let s = t.snapshot(&palette);
        let (cols, rows) = t.size();
        assert_eq!((s.cols, s.rows), (cols as usize, rows as usize), "round {round}");
        assert!(s.lines.iter().all(|l| l.len() == s.cols));
        assert!(s.cursor.col < s.cols && s.cursor.row < s.rows);
        let (text, (line, col)) = t.text();
        let lines: Vec<&str> = text.split('\n').collect();
        assert!(line < lines.len() && col <= lines[line].chars().count(), "round {round}");
    }
}

#[test]
fn random_bytes_flood_never_panics() {
    let mut rng = Rng(42);
    let mut t = Terminal::new(80, 24, 1000);
    let mut buf = vec![0u8; 1 << 16];
    for _ in 0..64 {
        for b in buf.iter_mut() {
            *b = rng.next() as u8;
        }
        t.feed(&buf);
    }
    t.snapshot(&P());
    t.text();
}

// ------------------------------------------------------ real terminfo

/// What `tput` prints for `args` with `TERM=xterm-256color` (what terminal
/// buffers advertise), or `None` if tput isn't available.
fn tput(args: &[&str]) -> Option<Vec<u8>> {
    let out = std::process::Command::new("tput").args(args).env("TERM", "xterm-256color").output().ok()?;
    out.status.success().then_some(out.stdout)
}

#[test]
fn sequences_from_the_advertised_terminfo_work() {
    let Some(clear) = tput(&["clear"]) else { return };
    let seq = |args: &[&str]| tput(args).expect("tput capability");
    let mut t = term(20, 5);
    t.feed(b"junk\r\nmore junk");
    t.feed(&clear);
    assert_eq!(rows(&t), ["", "", "", "", ""]);
    t.feed(&seq(&["cup", "2", "3"]));
    assert_eq!(cursor(&t), (2, 3));
    t.feed(&seq(&["setaf", "1"]));
    t.feed(b"r");
    t.feed(&seq(&["setab", "202"]));
    t.feed(b"o");
    t.feed(&seq(&["bold"]));
    t.feed(b"b");
    t.feed(&seq(&["sgr0"]));
    t.feed(b"n");
    assert_eq!(cell(&t, 2, 3).fg, P().ansi[1]);
    assert_eq!(cell(&t, 2, 4).bg, P().indexed(202));
    assert!(cell(&t, 2, 5).flags.bold);
    assert_eq!(cell(&t, 2, 6).flags, CellFlags::default());
    t.feed(&seq(&["smcup"]));
    assert!(t.modes().alt_screen);
    t.feed(&seq(&["rmcup"]));
    assert!(!t.modes().alt_screen);
    assert_eq!(rows(&t)[2], "   robn");
    t.feed(&seq(&["smkx"]));
    assert!(t.modes().app_cursor, "keypad transmit mode turns on application cursor keys");
    t.feed(&seq(&["rmkx"]));
    assert!(!t.modes().app_cursor);
    t.feed(&seq(&["civis"]));
    assert!(!snap(&t).cursor.visible);
    t.feed(&seq(&["cnorm"]));
    assert!(snap(&t).cursor.visible);
}

// -------------------------------------------------------------- draw runs

#[test]
fn row_runs_split_on_style_and_wide_chars() {
    let p = P();
    let t = fed(12, 1, "ab\x1b[31mcd\x1b[1me\x1b[0m日f😀");
    let runs = snap(&t).row_runs(0);
    let summary: Vec<(usize, usize, &str)> = runs.iter().map(|r| (r.col, r.width, r.text.as_str())).collect();
    assert_eq!(summary, [(0, 2, "ab"), (2, 2, "cd"), (4, 1, "e"), (5, 2, "日"), (7, 1, "f"), (8, 2, "😀"), (10, 2, "  ")]);
    assert_eq!(runs[1].fg, p.ansi[1]);
    assert!(runs[2].flags.bold);
    assert_eq!(runs[2].fg, p.ansi[9]);
    assert!(!runs[3].flags.wide && !runs[3].flags.wide_spacer);
    assert!(runs[6].is_blank(p.background));
    assert!(!runs[0].is_blank(p.background));
}

#[test]
fn row_runs_cover_every_column_once() {
    let mut rng = Rng(7);
    for _ in 0..200 {
        let cols = 2 + rng.below(30) as u16;
        let mut t = term(cols, 3);
        for _ in 0..rng.below(40) {
            let piece = match rng.below(6) {
                0 => "日".to_string(),
                1 => "😀".to_string(),
                2 => format!("\x1b[3{}m", rng.below(8)),
                3 => "e\u{301}".to_string(),
                4 => "\x1b[7m".to_string(),
                _ => "x".to_string(),
            };
            t.feed(piece.as_bytes());
        }
        let s = snap(&t);
        for row in 0..s.rows {
            let mut next = 0;
            for r in s.row_runs(row) {
                assert_eq!(r.col, next, "runs are contiguous");
                assert!(r.width >= 1);
                next += r.width;
            }
            assert_eq!(next, s.cols, "runs cover the row");
        }
    }
}

#[test]
fn row_runs_keep_combining_chars_and_blank_leading_spacers() {
    let t = fed(5, 2, "e\u{301}cde日");
    let s = snap(&t);
    let runs = s.row_runs(0);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].text, "e\u{301}cde ");
    assert_eq!(runs[0].width, 5);
    assert_eq!(s.row_runs(1)[0].text, "日");
    assert!(s.row_runs(9).is_empty());
}
