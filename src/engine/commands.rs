//! Interactive commands, named after their GNU Emacs counterparts. Every
//! command runs on all cursors. `lookup` finds a command by name (for keymaps
//! and, later, M-x).

use std::collections::HashMap;
use std::sync::OnceLock;

use super::cursor::Cursor;
use super::editor::{Args, Editor, RawArg, Scroll};
use super::modes::MinorMode;
use super::text::{self, *};

pub type CmdResult = Result<(), String>;
pub type CmdFn = fn(&mut Editor, &Args) -> CmdResult;

pub mod flags {
    /// Point motion: shift-selectable, keeps the region.
    pub const MOTION: u32 = 1;
    /// Vertical motion: preserves the goal column.
    pub const VERTICAL: u32 = 2;
    /// Does not create an undo group (undo itself, save, ...).
    pub const NO_UNDO: u32 = 4;
    /// Editing doesn't deactivate the region (e.g. indent-rigidly).
    pub const KEEP_REGION: u32 = 8;
    /// Reads one more key as a character argument before running.
    pub const READ_CHAR: u32 = 16;
}
use flags::*;

pub struct Command {
    pub name: &'static str,
    pub run: CmdFn,
    pub flags: u32,
    pub doc: &'static str,
}

macro_rules! commands {
    ($( $name:literal => $f:ident, $flags:expr, $doc:literal; )*) => {
        pub static COMMANDS: &[Command] = &[
            $( Command { name: $name, run: $f, flags: $flags, doc: $doc }, )*
        ];
    };
}

commands! {
    // motion
    "forward-char" => forward_char, MOTION, "Move point N characters forward.";
    "backward-char" => backward_char, MOTION, "Move point N characters backward.";
    "forward-word" => forward_word_cmd, MOTION, "Move point forward N words.";
    "backward-word" => backward_word_cmd, MOTION, "Move point backward N words.";
    "next-line" => next_line, MOTION | VERTICAL, "Move cursor vertically down N lines.";
    "previous-line" => previous_line, MOTION | VERTICAL, "Move cursor vertically up N lines.";
    "move-beginning-of-line" => beginning_of_line, MOTION, "Move point to beginning of current line.";
    "move-end-of-line" => end_of_line, MOTION, "Move point to end of current line.";
    "back-to-indentation" => back_to_indentation, MOTION, "Move point to the first non-whitespace character on this line.";
    "forward-sentence" => forward_sentence_cmd, MOTION, "Move forward to next end of sentence.";
    "backward-sentence" => backward_sentence_cmd, MOTION, "Move backward to start of sentence.";
    "forward-paragraph" => forward_paragraph_cmd, MOTION, "Move forward to end of paragraph.";
    "backward-paragraph" => backward_paragraph_cmd, MOTION, "Move backward to start of paragraph.";
    "forward-sexp" => forward_sexp_cmd, MOTION, "Move forward across one balanced expression.";
    "backward-sexp" => backward_sexp_cmd, MOTION, "Move backward across one balanced expression.";
    "backward-up-list" => backward_up_list_cmd, MOTION, "Move backward out of one level of parentheses.";
    "up-list" => up_list_cmd, MOTION, "Move forward out of one level of parentheses.";
    "down-list" => down_list_cmd, MOTION, "Move forward down one level of parentheses.";
    "beginning-of-buffer" => beginning_of_buffer, MOTION, "Move point to the beginning of the buffer.";
    "end-of-buffer" => end_of_buffer, MOTION, "Move point to the end of the buffer.";
    "scroll-up-command" => scroll_up, MOTION | VERTICAL, "Scroll text upward a page (move forward).";
    "scroll-down-command" => scroll_down, MOTION | VERTICAL, "Scroll text downward a page (move backward).";
    "goto-line" => goto_line, 0, "Go to line N (given as a prefix argument).";
    "recenter-top-bottom" => recenter_top_bottom, NO_UNDO, "Cycle the current line between center, top and bottom of the window.";

    // mark & region
    "set-mark-command" => set_mark_command, NO_UNDO, "Set the mark where point is; with a prefix argument, jump to the previous mark.";
    "exchange-point-and-mark" => exchange_point_and_mark, NO_UNDO, "Put the mark where point is and point where the mark was.";
    "mark-whole-buffer" => mark_whole_buffer, NO_UNDO, "Put point at the beginning and mark at the end of the buffer.";
    "mark-word" => mark_word, NO_UNDO, "Set mark N words away from point.";
    "mark-paragraph" => mark_paragraph, NO_UNDO, "Put point at the beginning of this paragraph, mark at the end.";
    "mark-sexp" => mark_sexp, NO_UNDO, "Set mark N sexps from point.";
    "keyboard-quit" => keyboard_quit, NO_UNDO, "Deactivate the region; with no region, remove extra cursors.";

    // killing & yanking
    "kill-line" => kill_line, 0, "Kill the rest of the current line; with N, kill N lines.";
    "kill-whole-line" => kill_whole_line, 0, "Kill the current line, including its newline.";
    "kill-region" => kill_region, 0, "Kill the text between point and mark.";
    "kill-ring-save" => kill_ring_save, NO_UNDO, "Save the region to the kill ring without killing it.";
    "kill-word" => kill_word, 0, "Kill characters forward until the end of a word.";
    "backward-kill-word" => backward_kill_word, 0, "Kill characters backward until the start of a word.";
    "kill-sentence" => kill_sentence, 0, "Kill from point to end of sentence.";
    "backward-kill-sentence" => backward_kill_sentence, 0, "Kill back from point to start of sentence.";
    "kill-sexp" => kill_sexp, 0, "Kill the sexp following point.";
    "backward-kill-sexp" => backward_kill_sexp, 0, "Kill the sexp preceding point.";
    "zap-to-char" => zap_to_char, READ_CHAR, "Kill up to and including the Nth occurrence of CHAR.";
    "zap-up-to-char" => zap_up_to_char, READ_CHAR, "Kill up to, but not including, the Nth occurrence of CHAR.";
    "yank" => yank, 0, "Reinsert the most recently killed text.";
    "yank-pop" => yank_pop, 0, "Replace the just-yanked text with an earlier kill.";

    // deletion
    "delete-char" => delete_char, 0, "Delete the following N characters (or the active region).";
    "delete-backward-char" => delete_backward_char, 0, "Delete the previous N characters (or the active region).";
    "delete-horizontal-space" => delete_horizontal_space, 0, "Delete all spaces and tabs around point.";
    "just-one-space" => just_one_space, 0, "Delete all spaces and tabs around point, leaving one space.";
    "delete-indentation" => delete_indentation, 0, "Join this line to the previous one, fixing up whitespace.";
    "delete-blank-lines" => delete_blank_lines, 0, "On a blank line, delete surrounding blank lines leaving one; else delete following ones.";
    "delete-trailing-whitespace" => delete_trailing_whitespace, 0, "Delete trailing whitespace on every line.";

    // insertion
    "self-insert-command" => self_insert, 0, "Insert the character you typed.";
    "newline" => newline, 0, "Insert a newline and indent like the previous line.";
    "electric-newline-and-maybe-indent" => plain_newline, 0, "Insert a newline without indenting.";
    "open-line" => open_line, 0, "Insert a newline after point, leaving point where it is.";
    "indent-for-tab-command" => indent_for_tab, 0, "Indent to the next tab stop (or indent the active region).";
    "indent-rigidly" => indent_rigidly, KEEP_REGION, "Indent all lines in the region by N columns (default tab width).";
    "quoted-insert" => quoted_insert, READ_CHAR, "Read the next key and insert it literally.";

    // transposition & case
    "transpose-chars" => transpose_chars, 0, "Interchange the characters around point.";
    "transpose-words" => transpose_words, 0, "Interchange the words around point.";
    "transpose-lines" => transpose_lines, 0, "Exchange the current line and the previous one.";
    "transpose-sexps" => transpose_sexps, 0, "Interchange the sexps around point.";
    "upcase-word" => upcase_word, 0, "Convert the following N words to upper case.";
    "downcase-word" => downcase_word, 0, "Convert the following N words to lower case.";
    "capitalize-word" => capitalize_word, 0, "Capitalize the following N words.";
    "upcase-region" => upcase_region, 0, "Convert the region to upper case.";
    "downcase-region" => downcase_region, 0, "Convert the region to lower case.";
    "capitalize-region" => capitalize_region, 0, "Capitalize every word in the region.";

    // filling
    "fill-paragraph" => fill_paragraph, 0, "Fill the paragraph at point to fill-column.";
    "set-fill-column" => set_fill_column, NO_UNDO, "Set fill-column to N, or to the current column.";

    // undo
    "undo" => undo, NO_UNDO, "Undo the last change.";
    "undo-redo" => undo_redo, NO_UNDO, "Redo the last undone change.";

    // multiple cursors
    "mc/mark-next-like-this" => mc_mark_next, NO_UNDO, "Add a cursor at the next occurrence of the region (or the next line).";
    "mc/mark-previous-like-this" => mc_mark_previous, NO_UNDO, "Add a cursor at the previous occurrence of the region (or the previous line).";
    "mc/mark-all-like-this" => mc_mark_all, NO_UNDO, "Add a cursor at every occurrence of the region.";
    "mc/skip-to-next-like-this" => mc_skip_next, NO_UNDO, "Move the last-added cursor to the following occurrence.";
    "mc/unmark-next-like-this" => mc_unmark_next, NO_UNDO, "Remove the cursor furthest down the buffer.";
    "mc/edit-lines" => mc_edit_lines, NO_UNDO, "Add a cursor on each line of the region.";

    // search
    "swiper" => swiper, NO_UNDO, "Search: list every line matching the query, jump to the chosen one.";
    "swiper-backward" => swiper_backward, NO_UNDO, "Like swiper, but start from the match before point.";

    // buffers
    "switch-to-buffer" => switch_to_buffer, NO_UNDO, "Pick a buffer to show, with fuzzy filtering and live preview.";
    "list-buffers" => switch_to_buffer, NO_UNDO, "Show the buffer picker.";
    "previous-buffer" => previous_buffer, NO_UNDO, "Show the most recently used other buffer; repeat to cycle back.";
    "next-buffer" => next_buffer, NO_UNDO, "Cycle forward through buffers (inverse of previous-buffer).";
    "kill-buffer" => kill_buffer, NO_UNDO, "Kill the current buffer (press twice if it has unsaved changes).";

    // files & app
    "save-buffer" => save_buffer, NO_UNDO, "Save the current buffer to its file.";
    "save-buffers-kill-terminal" => quit, NO_UNDO, "Exit (asks for a second press if the buffer is modified).";
    "execute-extended-command" => execute_extended, NO_UNDO, "Pick a command by name (fuzzy) and run it.";

    // minor modes (major mode commands come from the configuration)
    "auto-fill-mode" => auto_fill_mode, NO_UNDO, "Toggle breaking lines at fill-column while typing (with N: on if N > 0).";
    "read-only-mode" => read_only_mode, NO_UNDO, "Toggle refusing changes to this buffer (with N: on if N > 0).";
    "overwrite-mode" => overwrite_mode, NO_UNDO, "Toggle typing over the text after point (with N: on if N > 0).";
    "electric-pair-mode" => electric_pair_mode, NO_UNDO, "Toggle inserting closing brackets and quotes (with N: on if N > 0).";
}

pub fn lookup(name: &str) -> Option<&'static Command> {
    static INDEX: OnceLock<HashMap<&'static str, &'static Command>> = OnceLock::new();
    INDEX
        .get_or_init(|| COMMANDS.iter().map(|c| (c.name, c)).collect())
        .get(name)
        .copied()
}

fn err<T>(s: &str) -> Result<T, String> {
    Err(s.to_string())
}

fn repeat(n: i64, p: usize, fwd: impl Fn(usize) -> usize, back: impl Fn(usize) -> usize) -> usize {
    let mut p = p;
    for _ in 0..n.unsigned_abs() {
        p = if n >= 0 { fwd(p) } else { back(p) };
    }
    p
}

fn repeat_sexp(r: &ropey::Rope, n: i64, p: usize) -> Result<usize, String> {
    let mut p = p;
    for _ in 0..n.unsigned_abs() {
        p = if n >= 0 { forward_sexp(r, p) } else { backward_sexp(r, p) }.map_err(String::from)?;
    }
    Ok(p)
}

// =================================================================== motion

fn forward_char(ed: &mut Editor, a: &Args) -> CmdResult {
    let n = a.n();
    ed.move_each(|r, c| {
        let t = c.point as i64 + n;
        if t < 0 {
            err("Beginning of buffer")
        } else if t as usize > r.len_chars() {
            err("End of buffer")
        } else {
            Ok(t as usize)
        }
    })
}

fn backward_char(ed: &mut Editor, a: &Args) -> CmdResult {
    forward_char(ed, &Args { raw: Some(RawArg::Number(-a.n())), ..a.clone() })
}

fn forward_word_cmd(ed: &mut Editor, a: &Args) -> CmdResult {
    let n = a.n();
    ed.move_each(|r, c| Ok(forward_word_n(r, c.point, n)))
}

fn backward_word_cmd(ed: &mut Editor, a: &Args) -> CmdResult {
    let n = a.n();
    ed.move_each(|r, c| Ok(forward_word_n(r, c.point, -n)))
}

fn vertical(ed: &mut Editor, n: i64) -> CmdResult {
    ed.each(|ed, i| {
        let r = &ed.buf.rope;
        let c = &ed.cursors[i];
        let line = line_of(r, c.point) as i64;
        let goal = c.goal_col.unwrap_or_else(|| column(r, c.point));
        let last = r.len_lines() as i64 - 1;
        let target = line + n;
        let (p, e) = if target < 0 {
            (0, Some("Beginning of buffer"))
        } else if target > last {
            (r.len_chars(), Some("End of buffer"))
        } else {
            (pos_at(r, target as usize, goal), None)
        };
        let c = &mut ed.cursors[i];
        c.point = p;
        c.goal_col = Some(goal);
        e.map_or(Ok(()), err)
    })
}

fn next_line(ed: &mut Editor, a: &Args) -> CmdResult {
    vertical(ed, a.n())
}

fn previous_line(ed: &mut Editor, a: &Args) -> CmdResult {
    vertical(ed, -a.n())
}

fn beginning_of_line(ed: &mut Editor, a: &Args) -> CmdResult {
    let n = a.n() - 1;
    ed.move_each(|r, c| {
        let line = (line_of(r, c.point) as i64 + n).clamp(0, r.len_lines() as i64 - 1);
        Ok(r.line_to_char(line as usize))
    })
}

fn end_of_line(ed: &mut Editor, a: &Args) -> CmdResult {
    let n = a.n() - 1;
    ed.move_each(|r, c| {
        let line = (line_of(r, c.point) as i64 + n).clamp(0, r.len_lines() as i64 - 1);
        Ok(pos_at(r, line as usize, usize::MAX))
    })
}

fn back_to_indentation(ed: &mut Editor, _: &Args) -> CmdResult {
    ed.move_each(|r, c| Ok(indentation_end(r, c.point)))
}

fn forward_sentence_cmd(ed: &mut Editor, a: &Args) -> CmdResult {
    let n = a.n();
    ed.move_each(|r, c| Ok(repeat(n, c.point, |p| forward_sentence(r, p), |p| backward_sentence(r, p))))
}

fn backward_sentence_cmd(ed: &mut Editor, a: &Args) -> CmdResult {
    let n = -a.n();
    ed.move_each(|r, c| Ok(repeat(n, c.point, |p| forward_sentence(r, p), |p| backward_sentence(r, p))))
}

fn forward_paragraph_cmd(ed: &mut Editor, a: &Args) -> CmdResult {
    let n = a.n();
    ed.move_each(|r, c| Ok(repeat(n, c.point, |p| forward_paragraph(r, p), |p| backward_paragraph(r, p))))
}

fn backward_paragraph_cmd(ed: &mut Editor, a: &Args) -> CmdResult {
    let n = -a.n();
    ed.move_each(|r, c| Ok(repeat(n, c.point, |p| forward_paragraph(r, p), |p| backward_paragraph(r, p))))
}

fn forward_sexp_cmd(ed: &mut Editor, a: &Args) -> CmdResult {
    let n = a.n();
    ed.move_each(|r, c| repeat_sexp(r, n, c.point))
}

fn backward_sexp_cmd(ed: &mut Editor, a: &Args) -> CmdResult {
    let n = -a.n();
    ed.move_each(|r, c| repeat_sexp(r, n, c.point))
}

fn backward_up_list_cmd(ed: &mut Editor, _: &Args) -> CmdResult {
    ed.move_each(|r, c| text::backward_up_list(r, c.point).map_err(String::from))
}

fn up_list_cmd(ed: &mut Editor, _: &Args) -> CmdResult {
    ed.move_each(|r, c| text::up_list(r, c.point).map_err(String::from))
}

fn down_list_cmd(ed: &mut Editor, _: &Args) -> CmdResult {
    ed.move_each(|r, c| text::down_list(r, c.point).map_err(String::from))
}

/// With a numeric prefix N, go to N/10 of the way through the buffer (like Emacs).
fn buffer_fraction(ed: &Editor, a: &Args, from_end: bool) -> usize {
    let len = ed.len();
    match a.raw {
        Some(RawArg::Number(n)) if (0..=10).contains(&n) => {
            let n = n as usize;
            if from_end { len - len * n / 10 } else { len * n / 10 }
        }
        _ if from_end => len,
        _ => 0,
    }
}

fn beginning_of_buffer(ed: &mut Editor, a: &Args) -> CmdResult {
    let target = buffer_fraction(ed, a, false);
    ed.keep_only_primary();
    let c = &mut ed.cursors[0];
    if !c.active {
        c.push_mark(c.point, false);
    }
    c.point = target;
    Ok(())
}

fn end_of_buffer(ed: &mut Editor, a: &Args) -> CmdResult {
    let target = buffer_fraction(ed, a, true);
    ed.keep_only_primary();
    let c = &mut ed.cursors[0];
    if !c.active {
        c.push_mark(c.point, false);
    }
    c.point = target;
    Ok(())
}

fn page_size(ed: &Editor) -> i64 {
    // Emacs keeps `next-screen-context-lines` (2) lines of overlap.
    (ed.page_lines.saturating_sub(2)).max(1) as i64
}

fn scroll_up(ed: &mut Editor, a: &Args) -> CmdResult {
    let lines = if a.raw.is_some() { a.n() } else { page_size(ed) };
    let res = vertical(ed, lines);
    ed.scroll = if a.raw.is_some() { Scroll::EnsureVisible } else { Scroll::Page(1) };
    res
}

fn scroll_down(ed: &mut Editor, a: &Args) -> CmdResult {
    let lines = if a.raw.is_some() { a.n() } else { page_size(ed) };
    let res = vertical(ed, -lines);
    ed.scroll = if a.raw.is_some() { Scroll::EnsureVisible } else { Scroll::Page(-1) };
    res
}

fn goto_line(ed: &mut Editor, a: &Args) -> CmdResult {
    if a.raw.is_none() {
        return err("Give the line number as a prefix: M-4 M-2 M-g g (minibuffer prompt comes next)");
    }
    let line = (a.n().max(1) - 1) as usize;
    ed.keep_only_primary();
    let r = &ed.buf.rope;
    let target = pos_at(r, line, 0);
    let c = &mut ed.cursors[0];
    if !c.active {
        c.push_mark(c.point, false);
    }
    c.point = target;
    ed.scroll = Scroll::Center;
    Ok(())
}

fn recenter_top_bottom(ed: &mut Editor, _: &Args) -> CmdResult {
    ed.recenter_step = if ed.last_command == "recenter-top-bottom" { (ed.recenter_step + 1) % 3 } else { 0 };
    ed.scroll = [Scroll::Center, Scroll::Top, Scroll::Bottom][ed.recenter_step];
    Ok(())
}

// ============================================================ mark & region

fn set_mark_command(ed: &mut Editor, a: &Args) -> CmdResult {
    if matches!(a.raw, Some(RawArg::Universal(_))) {
        // Pop the mark ring: jump to the mark, rotate the ring.
        return ed.each(|ed, i| {
            let c = &mut ed.cursors[i];
            let Some(m) = c.mark else { return err("No mark set in this buffer") };
            c.point = m;
            if !c.mark_ring.is_empty() {
                let next = c.mark_ring.pop().unwrap();
                c.mark_ring.insert(0, m);
                c.mark = Some(next);
            }
            c.deactivate();
            Ok(())
        });
    }
    let repeat_empty = ed.last_command == "set-mark-command"
        && ed.cursors.iter().all(|c| c.region().is_some_and(|r| r.is_empty()));
    for c in &mut ed.cursors {
        if repeat_empty {
            c.deactivate();
        } else {
            c.push_mark(c.point, true);
        }
    }
    ed.message = Some(if repeat_empty { "Mark deactivated" } else { "Mark set" }.into());
    Ok(())
}

fn exchange_point_and_mark(ed: &mut Editor, a: &Args) -> CmdResult {
    let activate = a.raw.is_none();
    ed.each(|ed, i| {
        let c = &mut ed.cursors[i];
        let Some(m) = c.mark else { return err("No mark set in this buffer") };
        c.mark = Some(c.point);
        c.point = m;
        if activate {
            c.active = true;
        }
        Ok(())
    })
}

fn mark_whole_buffer(ed: &mut Editor, _: &Args) -> CmdResult {
    ed.keep_only_primary();
    let len = ed.len();
    let c = &mut ed.cursors[0];
    c.push_mark(len, true);
    c.point = 0;
    Ok(())
}

fn mark_word(ed: &mut Editor, a: &Args) -> CmdResult {
    let n = a.n();
    ed.each(|ed, i| {
        let r = &ed.buf.rope;
        let c = &ed.cursors[i];
        let from = if c.active { c.mark.unwrap_or(c.point) } else { c.point };
        let to = forward_word_n(r, from, n);
        let c = &mut ed.cursors[i];
        if c.active { c.mark = Some(to) } else { c.push_mark(to, true) }
        Ok(())
    })
}

fn mark_paragraph(ed: &mut Editor, _: &Args) -> CmdResult {
    ed.each(|ed, i| {
        let r = &ed.buf.rope;
        let end = forward_paragraph(r, ed.cursors[i].point);
        let start = backward_paragraph(r, end);
        let c = &mut ed.cursors[i];
        c.push_mark(end, true);
        c.point = start;
        Ok(())
    })
}

fn mark_sexp(ed: &mut Editor, a: &Args) -> CmdResult {
    let n = a.n();
    ed.each(|ed, i| {
        let c = &ed.cursors[i];
        let from = if c.active { c.mark.unwrap_or(c.point) } else { c.point };
        let to = repeat_sexp(&ed.buf.rope, n, from)?;
        let c = &mut ed.cursors[i];
        if c.active { c.mark = Some(to) } else { c.push_mark(to, true) }
        Ok(())
    })
}

fn keyboard_quit(ed: &mut Editor, _: &Args) -> CmdResult {
    if ed.cursors.iter().any(|c| c.active) {
        ed.cursors.iter_mut().for_each(Cursor::deactivate);
    } else if ed.cursors.len() > 1 {
        ed.keep_only_primary();
    }
    err("Quit")
}

// ========================================================== killing/yanking

fn kill_line(ed: &mut Editor, a: &Args) -> CmdResult {
    let raw = a.raw;
    let n = a.n();
    ed.each(|ed, i| {
        let r = &ed.buf.rope;
        let p = ed.cursors[i].point;
        let len = r.len_chars();
        let line = line_of(r, p);
        let (range, prepend) = match raw {
            None => {
                let eol = line_end(r, p);
                let rest_blank = r.slice(p..eol).chars().all(char::is_whitespace);
                let next = if line + 1 < r.len_lines() { r.line_to_char(line + 1) } else { len };
                let end = if rest_blank { next } else { eol };
                (p..end, false)
            }
            Some(_) if n > 0 => {
                let target = line + n as usize;
                let end = if target >= r.len_lines() { len } else { r.line_to_char(target) };
                (p..end, false)
            }
            Some(_) => {
                let target = (line as i64 + n).max(0) as usize;
                (r.line_to_char(target)..p, true)
            }
        };
        if range.is_empty() {
            return err(if prepend { "Beginning of buffer" } else { "End of buffer" });
        }
        ed.kill(i, range, prepend);
        Ok(())
    })
}

fn kill_whole_line(ed: &mut Editor, _: &Args) -> CmdResult {
    ed.each(|ed, i| {
        let r = &ed.buf.rope;
        let p = ed.cursors[i].point;
        let line = line_of(r, p);
        let start = r.line_to_char(line);
        let end = if line + 1 < r.len_lines() { r.line_to_char(line + 1) } else { r.len_chars() };
        // On the last line, take the preceding newline instead so no empty line remains.
        let start = if end == r.len_chars() && start > 0 && line + 1 >= r.len_lines() { start - 1 } else { start };
        if start == end {
            return err("End of buffer");
        }
        ed.kill(i, start..end, false);
        Ok(())
    })
}

fn kill_region(ed: &mut Editor, _: &Args) -> CmdResult {
    ed.each(|ed, i| {
        let c = &ed.cursors[i];
        let Some(r) = c.mark_region() else {
            return err("The mark is not set now, so there is no region");
        };
        let prepend = c.point < c.mark.unwrap();
        ed.kill(i, r, prepend);
        Ok(())
    })
}

fn kill_ring_save(ed: &mut Editor, _: &Args) -> CmdResult {
    ed.each(|ed, i| {
        let Some(r) = ed.cursors[i].mark_region() else {
            return err("The mark is not set now, so there is no region");
        };
        let s = ed.text(r);
        ed.record_kill(i, s, false);
        ed.cursors[i].deactivate();
        Ok(())
    })
}

fn kill_to(ed: &mut Editor, i: usize, target: usize) {
    let p = ed.cursors[i].point;
    if target >= p {
        ed.kill(i, p..target, false);
    } else {
        ed.kill(i, target..p, true);
    }
}

fn kill_word(ed: &mut Editor, a: &Args) -> CmdResult {
    let n = a.n();
    ed.each(|ed, i| {
        let t = forward_word_n(&ed.buf.rope, ed.cursors[i].point, n);
        kill_to(ed, i, t);
        Ok(())
    })
}

fn backward_kill_word(ed: &mut Editor, a: &Args) -> CmdResult {
    let n = a.n();
    ed.each(|ed, i| {
        let t = forward_word_n(&ed.buf.rope, ed.cursors[i].point, -n);
        kill_to(ed, i, t);
        Ok(())
    })
}

fn kill_sentence(ed: &mut Editor, a: &Args) -> CmdResult {
    let n = a.n();
    ed.each(|ed, i| {
        let r = &ed.buf.rope;
        let t = repeat(n, ed.cursors[i].point, |p| forward_sentence(r, p), |p| backward_sentence(r, p));
        kill_to(ed, i, t);
        Ok(())
    })
}

fn backward_kill_sentence(ed: &mut Editor, a: &Args) -> CmdResult {
    let n = -a.n();
    ed.each(|ed, i| {
        let r = &ed.buf.rope;
        let t = repeat(n, ed.cursors[i].point, |p| forward_sentence(r, p), |p| backward_sentence(r, p));
        kill_to(ed, i, t);
        Ok(())
    })
}

fn kill_sexp(ed: &mut Editor, a: &Args) -> CmdResult {
    let n = a.n();
    ed.each(|ed, i| {
        let t = repeat_sexp(&ed.buf.rope, n, ed.cursors[i].point)?;
        kill_to(ed, i, t);
        Ok(())
    })
}

fn backward_kill_sexp(ed: &mut Editor, a: &Args) -> CmdResult {
    let n = -a.n();
    ed.each(|ed, i| {
        let t = repeat_sexp(&ed.buf.rope, n, ed.cursors[i].point)?;
        kill_to(ed, i, t);
        Ok(())
    })
}

fn zap(ed: &mut Editor, a: &Args, inclusive: bool) -> CmdResult {
    let Some(ch) = a.text.clone() else { return err("No character given") };
    let n = a.n();
    let clen = ch.chars().count();
    ed.each(|ed, i| {
        let r = &ed.buf.rope;
        let p = ed.cursors[i].point;
        let (mut at, mut last) = (p, p);
        for k in 0..n.unsigned_abs() {
            // zap-up-to-char ignores an occurrence right at point.
            let skip = k == 0 && !inclusive;
            let found = if n > 0 {
                find_forward(r, &ch, (if skip { at + 1 } else { at }).min(r.len_chars()))
            } else {
                find_backward(r, &ch, if skip { at.saturating_sub(1) } else { at })
            };
            let Some(f) = found else { return Err(format!("Search failed: \"{ch}\"")) };
            last = f;
            at = if n > 0 { f + clen } else { f };
        }
        let target = match (n > 0, inclusive) {
            (true, true) | (false, false) => last + clen,
            (true, false) | (false, true) => last,
        };
        kill_to(ed, i, target);
        Ok(())
    })
}

fn zap_to_char(ed: &mut Editor, a: &Args) -> CmdResult {
    zap(ed, a, true)
}

fn zap_up_to_char(ed: &mut Editor, a: &Args) -> CmdResult {
    zap(ed, a, false)
}

fn yank(ed: &mut Editor, _: &Args) -> CmdResult {
    let Some(texts) = ed.yank_texts(0) else { return err("Kill ring is empty") };
    ed.set_yank_index(0);
    ed.each(|ed, i| {
        ed.delete_active_region(i);
        let start = ed.cursors[i].point;
        ed.insert_at_cursor(i, &texts[i]);
        let c = &mut ed.cursors[i];
        c.push_mark(start, false);
        Ok(())
    })
}

fn yank_pop(ed: &mut Editor, a: &Args) -> CmdResult {
    if !matches!(ed.last_command, "yank" | "yank-pop") {
        return err("Previous command was not a yank");
    }
    let len = ed.kill_ring.len().max(1) as i64;
    let idx = (ed.yank_index() as i64 + a.n()).rem_euclid(len) as usize;
    ed.set_yank_index(idx);
    let texts = ed.yank_texts(idx).unwrap_or_default();
    ed.each(|ed, i| {
        let Some(r) = ed.cursors[i].mark_region() else { return Ok(()) };
        ed.replace(r.clone(), &texts[i]);
        let c = &mut ed.cursors[i];
        c.mark = Some(r.start);
        c.point = r.start + texts[i].chars().count();
        Ok(())
    })
}

// ================================================================ deletion

fn delete_char(ed: &mut Editor, a: &Args) -> CmdResult {
    let n = a.n();
    ed.each(|ed, i| {
        if ed.delete_active_region(i) {
            return Ok(());
        }
        let p = ed.cursors[i].point as i64;
        let len = ed.len() as i64;
        let t = p + n;
        if t > len {
            return err("End of buffer");
        }
        if t < 0 {
            return err("Beginning of buffer");
        }
        let (s, e) = (p.min(t) as usize, p.max(t) as usize);
        ed.replace(s..e, "");
        ed.cursors[i].point = s;
        Ok(())
    })
}

fn delete_backward_char(ed: &mut Editor, a: &Args) -> CmdResult {
    if a.n() == 1 && ed.minor_on(MinorMode::ElectricPair) {
        // Deleting the opening half of an empty pair deletes both halves.
        return ed.each(|ed, i| {
            if ed.delete_active_region(i) {
                return Ok(());
            }
            let p = ed.cursors[i].point;
            let r = &ed.buf.rope;
            let (s, e) = match (char_before(r, p), char_at(r, p)) {
                (Some(o), Some(c)) if PAIRS.contains(&(o, c)) => (p - 1, p + 1),
                (Some(_), _) => (p - 1, p),
                (None, _) => return err("Beginning of buffer"),
            };
            ed.replace(s..e, "");
            ed.cursors[i].point = s;
            Ok(())
        });
    }
    delete_char(ed, &Args { raw: Some(RawArg::Number(-a.n())), ..a.clone() })
}

fn hspace_around(r: &ropey::Rope, p: usize) -> (usize, usize) {
    let (mut s, mut e) = (p, p);
    while s > 0 && is_hspace(r.char(s - 1)) {
        s -= 1;
    }
    while e < r.len_chars() && is_hspace(r.char(e)) {
        e += 1;
    }
    (s, e)
}

fn delete_horizontal_space(ed: &mut Editor, a: &Args) -> CmdResult {
    let backward_only = a.raw.is_some();
    ed.each(|ed, i| {
        let p = ed.cursors[i].point;
        let (s, e) = hspace_around(&ed.buf.rope, p);
        let e = if backward_only { p } else { e };
        ed.replace(s..e, "");
        ed.cursors[i].point = s;
        Ok(())
    })
}

fn just_one_space(ed: &mut Editor, a: &Args) -> CmdResult {
    let n = a.n().max(0) as usize;
    ed.each(|ed, i| {
        let (s, e) = hspace_around(&ed.buf.rope, ed.cursors[i].point);
        ed.replace(s..e, &" ".repeat(n));
        ed.cursors[i].point = s + n;
        Ok(())
    })
}

fn delete_indentation(ed: &mut Editor, a: &Args) -> CmdResult {
    let with_next = a.raw.is_some();
    ed.each(|ed, i| {
        let r = &ed.buf.rope;
        let mut line = line_of(r, ed.cursors[i].point);
        if with_next {
            line += 1;
        }
        if line == 0 || line >= r.len_lines() {
            return Ok(());
        }
        let this_start = r.line_to_char(line);
        let indent_end = indentation_end(r, this_start);
        let mut join = this_start - 1; // the newline
        while join > 0 && is_hspace(r.char(join - 1)) {
            join -= 1;
        }
        let before = char_before(r, join);
        let after = char_at(r, indent_end);
        let sep = match (before, after) {
            (None, _) | (_, None) | (Some('\n'), _) | (_, Some('\n')) => "",
            (Some('(') | Some('['), _) | (_, Some(')') | Some(']')) => "",
            _ => " ",
        };
        ed.replace(join..indent_end, sep);
        ed.cursors[i].point = join;
        Ok(())
    })
}

fn delete_blank_lines(ed: &mut Editor, _: &Args) -> CmdResult {
    ed.each(|ed, i| {
        let r = &ed.buf.rope;
        let n = r.len_lines();
        let line = line_of(r, ed.cursors[i].point);
        let line_start_of = |l: usize| if l >= n { r.len_chars() } else { r.line_to_char(l) };
        if is_blank_line(r, line) {
            let (mut a, mut b) = (line, line + 1);
            while a > 0 && is_blank_line(r, a - 1) {
                a -= 1;
            }
            while b < n && is_blank_line(r, b) {
                b += 1;
            }
            let (s, e) = (line_start_of(a), line_start_of(b));
            if b - a > 1 {
                // keep one blank line
                ed.replace(s..e, "\n");
                ed.cursors[i].point = s;
            } else {
                ed.replace(s..e, "");
                ed.cursors[i].point = s;
            }
        } else {
            let mut b = line + 1;
            while b < n && is_blank_line(r, b) {
                b += 1;
            }
            let (s, e) = (line_start_of(line + 1), line_start_of(b));
            if e > s {
                ed.replace(s..e, "");
            }
        }
        Ok(())
    })
}

fn delete_trailing_whitespace(ed: &mut Editor, _: &Args) -> CmdResult {
    let mut line = ed.rope().len_lines();
    while line > 0 {
        line -= 1;
        let r = ed.rope();
        let s = r.line_to_char(line);
        let e = s + line_len(r, line);
        let mut t = e;
        while t > s && is_hspace(r.char(t - 1)) {
            t -= 1;
        }
        if t < e {
            ed.replace(t..e, "");
        }
    }
    Ok(())
}

// =============================================================== insertion

fn self_insert(ed: &mut Editor, a: &Args) -> CmdResult {
    let Some(t) = a.text.clone() else { return Ok(()) };
    let n = a.n().max(0) as usize;
    let s = t.repeat(n);
    let fill = (ed.minor_on(MinorMode::AutoFill) && t.chars().all(is_hspace)).then(|| ed.fill_column());
    let overwrite = ed.minor_on(MinorMode::Overwrite);
    let mut chars = t.chars();
    let pair = match (chars.next(), chars.next()) {
        (Some(c), None) if n == 1 && ed.minor_on(MinorMode::ElectricPair) => Some(c),
        _ => None,
    };
    ed.each(|ed, i| {
        if let Some(c) = pair {
            if electric_pair(ed, i, c) {
                return Ok(());
            }
        }
        let had_region = ed.delete_active_region(i);
        if let Some(width) = fill {
            auto_fill(ed, i, width);
        }
        if overwrite && !had_region {
            // Replace as many chars as are typed, but never the end of the line.
            let p = ed.cursors[i].point;
            let end = (p + s.chars().count()).min(line_end(&ed.buf.rope, p));
            ed.replace(p..end, &s);
            ed.cursors[i].point = p + s.chars().count();
        } else {
            ed.insert_at_cursor(i, &s);
        }
        Ok(())
    })
}

/// The pairs electric-pair-mode completes.
const PAIRS: [(char, char); 4] = [('(', ')'), ('[', ']'), ('{', '}'), ('"', '"')];

/// electric-pair-mode for cursor `i` typing `c`: wrap the region in a pair,
/// step over a closer that is already there, or insert both halves of a
/// pair. Returns false when `c` should just be inserted.
fn electric_pair(ed: &mut Editor, i: usize, c: char) -> bool {
    let close = PAIRS.iter().find(|p| p.0 == c).map(|p| p.1);
    let region = ed.cursors[i].region().filter(|r| !r.is_empty());
    if let Some(r) = region {
        let Some(close) = close else { return false };
        let at_end = ed.cursors[i].point == r.end;
        let inner = ed.text(r.clone());
        let len = inner.chars().count();
        ed.replace(r.clone(), &format!("{c}{inner}{close}"));
        let cur = &mut ed.cursors[i];
        cur.point = if at_end { r.start + len + 2 } else { r.start + 1 };
        cur.deactivate();
        return true;
    }
    let p = ed.cursors[i].point;
    let r = &ed.buf.rope;
    let (prev, next) = (char_before(r, p), char_at(r, p));
    if next == Some(c) && PAIRS.iter().any(|p| p.1 == c) {
        ed.cursors[i].point = p + 1;
        return true;
    }
    let Some(close) = close else { return false };
    // Don't pair in front of a word, or a quote right after one (an apostrophe-like use).
    if next.is_some_and(is_word) || (close == c && prev.is_some_and(is_word)) {
        return false;
    }
    ed.replace(p..p, &format!("{c}{close}"));
    ed.cursors[i].point = p + 1;
    true
}

/// auto-fill-mode for cursor `i`: while point is past `width`, break its line
/// at the last space that keeps the text before it within `width` (or after
/// the first word, if that alone is too long), indenting like the line.
fn auto_fill(ed: &mut Editor, i: usize, width: usize) {
    loop {
        let r = &ed.buf.rope;
        let p = ed.cursors[i].point;
        if column(r, p) <= width {
            return;
        }
        let ls = line_start(r, p);
        let ie = indentation_end(r, ls).min(p);
        let blank = |b: usize| char_at(r, b).is_some_and(is_hspace);
        let Some(b) = (ie..p).rev().find(|&b| blank(b) && b - ls <= width).or_else(|| (ie..p).find(|&b| blank(b))) else {
            return;
        };
        let (mut s, mut e) = (b, b);
        while s > ie && blank(s - 1) {
            s -= 1;
        }
        while e < p && blank(e) {
            e += 1;
        }
        if e >= p {
            return; // nothing but spaces after the break
        }
        let indent = r.slice(ls..ie).to_string();
        ed.replace(s..e, &format!("\n{indent}"));
    }
}

fn auto_fill_column(ed: &Editor) -> Option<usize> {
    ed.minor_on(MinorMode::AutoFill).then(|| ed.fill_column())
}

fn newline(ed: &mut Editor, a: &Args) -> CmdResult {
    let n = a.n().max(0) as usize;
    let fill = auto_fill_column(ed);
    ed.each(|ed, i| {
        ed.delete_active_region(i);
        if let Some(width) = fill {
            auto_fill(ed, i, width);
        }
        let p = ed.cursors[i].point;
        // Like electric-indent: drop trailing space before point, copy indentation.
        let indent = {
            let r = &ed.buf.rope;
            let s = line_start(r, p);
            let ie = indentation_end(r, p).min(p);
            r.slice(s..ie).to_string()
        };
        let (hs, _) = hspace_around(&ed.buf.rope, p);
        if hs < p && hs > line_start(&ed.buf.rope, p) {
            ed.replace(hs..p, "");
            ed.cursors[i].point = hs;
        }
        ed.insert_at_cursor(i, &"\n".repeat(n));
        ed.insert_at_cursor(i, &indent);
        Ok(())
    })
}

fn plain_newline(ed: &mut Editor, a: &Args) -> CmdResult {
    let s = "\n".repeat(a.n().max(0) as usize);
    let fill = auto_fill_column(ed);
    ed.each(|ed, i| {
        ed.delete_active_region(i);
        if let Some(width) = fill {
            auto_fill(ed, i, width);
        }
        ed.insert_at_cursor(i, &s);
        Ok(())
    })
}

fn open_line(ed: &mut Editor, a: &Args) -> CmdResult {
    let s = "\n".repeat(a.n().max(0) as usize);
    ed.each(|ed, i| {
        let p = ed.cursors[i].point;
        ed.replace(p..p, &s);
        ed.cursors[i].point = p;
        Ok(())
    })
}

fn indent_lines(ed: &mut Editor, i: usize, range: std::ops::Range<usize>, by: i64) {
    let r = &ed.buf.rope;
    let first = line_of(r, range.start);
    let mut last = line_of(r, range.end);
    if last > first && range.end == r.line_to_char(last) {
        last -= 1; // region ends at column 0: that line isn't included
    }
    let _ = i;
    for line in (first..=last).rev() {
        let r = &ed.buf.rope;
        if is_blank_line(r, line) {
            continue;
        }
        let s = r.line_to_char(line);
        if by >= 0 {
            ed.replace(s..s, &" ".repeat(by as usize));
        } else {
            let ie = indentation_end(r, s);
            let remove = ((-by) as usize).min(ie - s);
            ed.replace(s..s + remove, "");
        }
    }
}

fn indent_for_tab(ed: &mut Editor, _: &Args) -> CmdResult {
    let tw = ed.tab_width();
    ed.each(|ed, i| {
        if let Some(r) = ed.cursors[i].region() {
            indent_lines(ed, i, r, tw as i64);
            return Ok(());
        }
        let col = column(&ed.buf.rope, ed.cursors[i].point);
        let pad = tw - col % tw;
        ed.insert_at_cursor(i, &" ".repeat(pad));
        Ok(())
    })
}

fn indent_rigidly(ed: &mut Editor, a: &Args) -> CmdResult {
    let by = if a.raw.is_some() { a.n() } else { ed.tab_width() as i64 };
    ed.each(|ed, i| {
        let Some(r) = ed.cursors[i].mark_region() else {
            return err("The mark is not set now, so there is no region");
        };
        indent_lines(ed, i, r, by);
        Ok(())
    })
}

fn quoted_insert(ed: &mut Editor, a: &Args) -> CmdResult {
    let Some(t) = a.text.clone() else { return Ok(()) };
    let s = t.repeat(a.n().max(0) as usize);
    ed.each(|ed, i| {
        ed.insert_at_cursor(i, &s);
        Ok(())
    })
}

// ====================================================== transpose & case

fn transpose_chars(ed: &mut Editor, a: &Args) -> CmdResult {
    let n = a.n().max(1);
    ed.each(|ed, i| {
        for _ in 0..n {
            let r = &ed.buf.rope;
            let mut p = ed.cursors[i].point;
            let len = r.len_chars();
            if p == 0 {
                return err("Beginning of buffer");
            }
            // At end of line, transpose the two chars before point.
            let at_eol = p == len || r.char(p) == '\n';
            if at_eol {
                if p < 2 {
                    return err("Beginning of buffer");
                }
                p -= 1;
            }
            let (a1, b1) = (r.char(p - 1), r.char(p));
            ed.replace(p - 1..p + 1, &format!("{b1}{a1}"));
            ed.cursors[i].point = p + 1;
            if at_eol {
                break;
            }
        }
        Ok(())
    })
}

/// Swap two adjacent "things" found by `fwd`/`back` motions (words, sexps).
fn transpose_things(
    ed: &mut Editor,
    fwd: impl Fn(&ropey::Rope, usize) -> Result<usize, String>,
    back: impl Fn(&ropey::Rope, usize) -> Result<usize, String>,
    inside: impl Fn(&ropey::Rope, usize) -> bool,
) -> CmdResult {
    ed.each(|ed, i| {
        let r = &ed.buf.rope;
        let p = ed.cursors[i].point;
        let e1 = if inside(r, p) { fwd(r, p)? } else { fwd(r, back(r, p)?)? };
        let s1 = back(r, e1)?;
        let e2 = fwd(r, e1)?;
        let s2 = back(r, e2)?;
        if e2 == e1 || s2 < e1 || s1 == s2 {
            return err("Don't have two things to transpose");
        }
        let w1 = ed.text(s1..e1);
        let mid = ed.text(e1..s2);
        let w2 = ed.text(s2..e2);
        ed.replace(s1..e2, &format!("{w2}{mid}{w1}"));
        ed.cursors[i].point = e2;
        Ok(())
    })
}

fn transpose_words(ed: &mut Editor, _: &Args) -> CmdResult {
    transpose_things(
        ed,
        |r, p| Ok(forward_word(r, p)),
        |r, p| Ok(backward_word(r, p)),
        |r, p| char_before(r, p).is_some_and(is_word) && char_at(r, p).is_some_and(is_word),
    )
}

fn transpose_sexps(ed: &mut Editor, _: &Args) -> CmdResult {
    transpose_things(
        ed,
        |r, p| forward_sexp(r, p).map_err(String::from),
        |r, p| backward_sexp(r, p).map_err(String::from),
        |r, p| char_before(r, p).is_some_and(is_symbol) && char_at(r, p).is_some_and(is_symbol),
    )
}

fn transpose_lines(ed: &mut Editor, _: &Args) -> CmdResult {
    ed.each(|ed, i| {
        let r = &ed.buf.rope;
        let line = line_of(r, ed.cursors[i].point);
        if line == 0 {
            return err("Don't have two things to transpose");
        }
        let s = r.line_to_char(line - 1);
        let prev = r.slice(s..s + line_len(r, line - 1)).to_string();
        let cs = r.line_to_char(line);
        let cur = r.slice(cs..cs + line_len(r, line)).to_string();
        let e = cs + line_len(r, line);
        ed.replace(s..e, &format!("{cur}\n{prev}"));
        let r = &ed.buf.rope;
        ed.cursors[i].point = if line + 1 < r.len_lines() { r.line_to_char(line + 1) } else { r.len_chars() };
        Ok(())
    })
}

fn capitalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_word = false;
    for c in s.chars() {
        if is_word(c) {
            if in_word {
                out.extend(c.to_lowercase());
            } else {
                out.extend(c.to_uppercase());
            }
            in_word = true;
        } else {
            out.push(c);
            in_word = false;
        }
    }
    out
}

fn case_words(ed: &mut Editor, a: &Args, f: fn(&str) -> String) -> CmdResult {
    let n = a.n();
    ed.each(|ed, i| {
        let p = ed.cursors[i].point;
        let t = forward_word_n(&ed.buf.rope, p, n);
        let (s, e) = (p.min(t), p.max(t));
        let new = f(&ed.text(s..e));
        ed.replace(s..e, &new);
        // Positive arg moves past the words; negative leaves point alone.
        ed.cursors[i].point = if n >= 0 { e } else { p };
        Ok(())
    })
}

fn case_region(ed: &mut Editor, f: fn(&str) -> String) -> CmdResult {
    ed.each(|ed, i| {
        let Some(r) = ed.cursors[i].mark_region() else {
            return err("The mark is not set now, so there is no region");
        };
        let new = f(&ed.text(r.clone()));
        ed.replace(r, &new);
        Ok(())
    })
}

fn upcase_word(ed: &mut Editor, a: &Args) -> CmdResult {
    case_words(ed, a, |s| s.to_uppercase())
}
fn downcase_word(ed: &mut Editor, a: &Args) -> CmdResult {
    case_words(ed, a, |s| s.to_lowercase())
}
fn capitalize_word(ed: &mut Editor, a: &Args) -> CmdResult {
    case_words(ed, a, capitalize)
}
fn upcase_region(ed: &mut Editor, _: &Args) -> CmdResult {
    case_region(ed, |s| s.to_uppercase())
}
fn downcase_region(ed: &mut Editor, _: &Args) -> CmdResult {
    case_region(ed, |s| s.to_lowercase())
}
fn capitalize_region(ed: &mut Editor, _: &Args) -> CmdResult {
    case_region(ed, capitalize)
}

// ================================================================= filling

fn fill_paragraph(ed: &mut Editor, _: &Args) -> CmdResult {
    let width = ed.fill_column();
    ed.each(|ed, i| {
        let r = &ed.buf.rope;
        let (s, e) = paragraph_bounds(r, ed.cursors[i].point);
        if s >= e {
            return Ok(());
        }
        let indent = indentation_string(r, s);
        let old = ed.text(s..e);
        let new = fill(&old, &indent, width);
        if new != old {
            ed.replace(s..e, &new);
        }
        Ok(())
    })
}

fn set_fill_column(ed: &mut Editor, a: &Args) -> CmdResult {
    let col = if a.raw.is_some() { a.n().max(1) as usize } else { ed.primary_line_col().1 };
    ed.buf.modes.local.fill_column = Some(col);
    ed.message = Some(format!("Fill column set to {col}"));
    Ok(())
}

// ==================================================================== undo

fn restore(ed: &mut Editor, cursors: Vec<Cursor>) {
    if !cursors.is_empty() {
        ed.cursors = cursors;
        if !ed.cursors.iter().any(|c| c.id == ed.primary) {
            ed.primary = ed.cursors[0].id;
        }
    }
    let len = ed.len();
    for c in &mut ed.cursors {
        c.deactivate();
        c.point = c.point.min(len);
        c.mark = c.mark.map(|m| m.min(len));
    }
}

fn undo(ed: &mut Editor, a: &Args) -> CmdResult {
    for _ in 0..a.n().max(1) {
        let Some(cursors) = ed.buf.undo() else { return err("No further undo information") };
        restore(ed, cursors);
    }
    ed.message = Some("Undo".into());
    Ok(())
}

fn undo_redo(ed: &mut Editor, a: &Args) -> CmdResult {
    for _ in 0..a.n().max(1) {
        let Some(cursors) = ed.buf.redo() else { return err("No further redo information") };
        restore(ed, cursors);
    }
    ed.message = Some("Redo".into());
    Ok(())
}

// ======================================================= multiple cursors

/// The text the primary cursor's active region covers, plus whether point is at its end.
fn primary_selection(ed: &Editor) -> Option<(String, bool)> {
    let c = ed.primary_cursor();
    let r = c.region().filter(|r| !r.is_empty())?;
    Some((ed.text(r), c.point >= c.mark.unwrap_or(0)))
}

fn cursor_for_match(start: usize, len: usize, point_at_end: bool) -> Cursor {
    let mut c = Cursor::new(0, if point_at_end { start + len } else { start });
    c.mark = Some(if point_at_end { start } else { start + len });
    c.active = true;
    c
}

fn mc_mark_next(ed: &mut Editor, _: &Args) -> CmdResult {
    let r = &ed.buf.rope;
    if let Some((needle, at_end)) = primary_selection(ed) {
        let from = ed.cursors.iter().map(|c| c.mark_region().map_or(c.point, |r| r.end)).max().unwrap();
        let Some(m) = find_forward(r, &needle, from) else {
            return Err(format!("No more matches for \"{needle}\""));
        };
        let len = needle.chars().count();
        ed.add_cursor(cursor_for_match(m, len, at_end));
        ed.reveal = Some(m);
    } else {
        let last = ed.cursors.iter().max_by_key(|c| c.point).unwrap();
        let line = line_of(r, last.point) + 1;
        if line >= r.len_lines() {
            return err("End of buffer");
        }
        let col = column(r, ed.primary_cursor().point);
        let p = pos_at(r, line, col);
        ed.add_cursor(Cursor::new(0, p));
        ed.reveal = Some(p);
    }
    ed.message = Some(format!("{} cursors", ed.cursors.len()));
    Ok(())
}

fn mc_mark_previous(ed: &mut Editor, _: &Args) -> CmdResult {
    let r = &ed.buf.rope;
    if let Some((needle, at_end)) = primary_selection(ed) {
        let before = ed.cursors.iter().map(|c| c.mark_region().map_or(c.point, |r| r.start)).min().unwrap();
        let Some(m) = find_backward(r, &needle, before) else {
            return Err(format!("No more matches for \"{needle}\""));
        };
        let len = needle.chars().count();
        ed.add_cursor(cursor_for_match(m, len, at_end));
        ed.reveal = Some(m);
    } else {
        let first = ed.cursors.iter().min_by_key(|c| c.point).unwrap();
        let line = line_of(r, first.point);
        if line == 0 {
            return err("Beginning of buffer");
        }
        let col = column(r, ed.primary_cursor().point);
        let p = pos_at(r, line - 1, col);
        ed.add_cursor(Cursor::new(0, p));
        ed.reveal = Some(p);
    }
    ed.message = Some(format!("{} cursors", ed.cursors.len()));
    Ok(())
}

fn mc_mark_all(ed: &mut Editor, _: &Args) -> CmdResult {
    let Some((needle, at_end)) = primary_selection(ed) else {
        return err("Mark a region first (C-SPC + motion) to find all occurrences");
    };
    let len = needle.chars().count();
    let primary_start = ed.primary_cursor().region().unwrap().start;
    ed.keep_only_primary();
    let mut from = 0;
    let mut found = Vec::new();
    while let Some(m) = find_forward(&ed.buf.rope, &needle, from) {
        if m != primary_start {
            found.push(m);
        }
        from = m + len.max(1);
    }
    for m in found {
        ed.add_cursor(cursor_for_match(m, len, at_end));
    }
    ed.message = Some(format!("{} cursors", ed.cursors.len()));
    Ok(())
}

fn furthest_non_primary(ed: &Editor) -> Option<usize> {
    ed.cursors
        .iter()
        .enumerate()
        .filter(|(_, c)| c.id != ed.primary)
        .max_by_key(|(_, c)| c.id)
        .map(|(i, _)| i)
}

fn mc_unmark_next(ed: &mut Editor, _: &Args) -> CmdResult {
    let Some(i) = furthest_non_primary(ed) else { return err("No extra cursors") };
    ed.cursors.remove(i);
    ed.message = Some(format!("{} cursors", ed.cursors.len()));
    Ok(())
}

fn mc_skip_next(ed: &mut Editor, a: &Args) -> CmdResult {
    if primary_selection(ed).is_none() {
        return err("Mark a region first");
    }
    let removed = furthest_non_primary(ed).map(|i| ed.cursors.remove(i));
    match mc_mark_next(ed, a) {
        Ok(()) => Ok(()),
        Err(e) => {
            if let Some(c) = removed {
                ed.cursors.push(c);
            }
            Err(e)
        }
    }
}

fn mc_edit_lines(ed: &mut Editor, _: &Args) -> CmdResult {
    let c = ed.primary_cursor().clone();
    let Some(m) = c.mark else { return err("Mark a region spanning several lines first") };
    let r = &ed.buf.rope;
    let col = column(r, c.point);
    let (l1, l2) = (line_of(r, m.min(c.point)), line_of(r, m.max(c.point)));
    if l1 == l2 {
        return err("Region must span more than one line");
    }
    let positions: Vec<usize> = (l1..=l2).map(|l| pos_at(r, l, col)).collect();
    ed.keep_only_primary();
    let pl = line_of(&ed.buf.rope, c.point);
    for (k, p) in positions.into_iter().enumerate() {
        if l1 + k == pl {
            continue;
        }
        ed.add_cursor(Cursor::new(0, p));
    }
    for c in &mut ed.cursors {
        c.deactivate();
    }
    ed.message = Some(format!("{} cursors", ed.cursors.len()));
    Ok(())
}

// ================================================================= search

fn swiper(ed: &mut Editor, _: &Args) -> CmdResult {
    ed.open_swiper(false);
    Ok(())
}

fn swiper_backward(ed: &mut Editor, _: &Args) -> CmdResult {
    ed.open_swiper(true);
    Ok(())
}

// ================================================================ buffers

fn switch_to_buffer(ed: &mut Editor, _: &Args) -> CmdResult {
    ed.open_buffer_picker();
    Ok(())
}

fn previous_buffer(ed: &mut Editor, _: &Args) -> CmdResult {
    let Some(id) = ed.others.first().map(|s| s.id) else { return err("No other buffer") };
    ed.switch_to_buffer_ex(id, true);
    Ok(())
}

fn next_buffer(ed: &mut Editor, _: &Args) -> CmdResult {
    let Some(id) = ed.others.last().map(|s| s.id) else { return err("No other buffer") };
    ed.switch_to_buffer_ex(id, false);
    Ok(())
}

fn kill_buffer(ed: &mut Editor, _: &Args) -> CmdResult {
    let name = ed.buf.name.clone();
    if ed.buf.modified() && ed.buf.path.is_some() && ed.last_command != "kill-buffer" {
        return Err(format!("{name} has unsaved changes; repeat the command to kill it anyway"));
    }
    let id = ed.buf_id;
    ed.kill_buffer(id);
    ed.message = Some(format!("Killed {name}"));
    Ok(())
}

// ============================================================ files & app

fn save_buffer(ed: &mut Editor, _: &Args) -> CmdResult {
    if !ed.buf.modified() && ed.buf.path.is_some() {
        return err("(No changes need to be saved)");
    }
    match ed.buf.save() {
        Ok(msg) => {
            ed.message = Some(msg);
            Ok(())
        }
        Err(e) => Err(format!("Save failed: {e}")),
    }
}

fn quit(ed: &mut Editor, _: &Args) -> CmdResult {
    if ed.buf.modified() && ed.buf.path.is_some() && ed.last_command != "save-buffers-kill-terminal" {
        return err("Buffer modified; save it first, or repeat the command to quit without saving");
    }
    ed.quit_requested = true;
    Ok(())
}

fn execute_extended(ed: &mut Editor, a: &Args) -> CmdResult {
    ed.open_command_picker(Args::with_raw(a.raw));
    Ok(())
}

// ============================================================ minor modes

fn auto_fill_mode(ed: &mut Editor, a: &Args) -> CmdResult {
    ed.minor_mode_command(MinorMode::AutoFill, a);
    Ok(())
}

fn read_only_mode(ed: &mut Editor, a: &Args) -> CmdResult {
    ed.minor_mode_command(MinorMode::ReadOnly, a);
    Ok(())
}

fn overwrite_mode(ed: &mut Editor, a: &Args) -> CmdResult {
    ed.minor_mode_command(MinorMode::Overwrite, a);
    Ok(())
}

fn electric_pair_mode(ed: &mut Editor, a: &Args) -> CmdResult {
    ed.minor_mode_command(MinorMode::ElectricPair, a);
    Ok(())
}
