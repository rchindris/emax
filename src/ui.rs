//! GPUI view: renders the editor (virtualised lines, regions, multiple cursors,
//! mode line, echo area, the buffer picker overlay, the swiper search panel)
//! and translates platform
//! key events into Emacs keys.

use std::cell::Cell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::time::Instant;

use gpui::{
    AnyElement, App, ClipboardItem, Context, FocusHandle, Focusable, FontWeight, HighlightStyle, KeyDownEvent,
    Keystroke, Pixels, Point, SharedString, StyledText, UniformListScrollHandle, Window, div,
    point, prelude::*, px, rgb, rgba, uniform_list,
};

use emax::engine::picker::Picker;
use emax::engine::{Swiper, SwiperExit};
use emax::engine::{BufView, Dispatcher, Editor, Key, Keymap, Outcome, Scroll, text};

// ------------------------------------------------------------------ theme

const BG: u32 = 0x1e1f22;
const FG: u32 = 0xd4d7dd;
const DIM: u32 = 0x5c6370;
const CUR_LINE: u32 = 0x26282e;
const CURSOR: u32 = 0x61afef;
const CURSOR2: u32 = 0xe5c07b;
const REGION: u32 = 0x3a4458;
const MODELINE_BG: u32 = 0x2b2d33;
const ACCENT: u32 = 0xc678dd;
const PANEL_BG: u32 = 0x25272c;
const PANEL_BORDER: u32 = 0x3b3f48;
const ROW_SELECTED: u32 = 0x2f3646;
const ROW_HOVER: u32 = 0x2a2d35;
const MATCH: u32 = 0x61afef;
/// Search matches in the buffer, and the selected one.
const SEARCH_HL: u32 = 0x3e4a3a;
const SEARCH_CUR: u32 = 0xd19a66;
const FONT: &str = "Menlo";
const UI_FONT: &str = ".SystemUIFont";
const FONT_SIZE: f32 = 14.0;
const LINE_HEIGHT: f32 = 21.0;
const TAB_WIDTH: usize = 4;
/// Lines longer than this are truncated on screen (prototype limitation).
const MAX_RENDER_COLS: usize = 2000;
const PICKER_ROWS: usize = 12;
/// Candidate rows in the search panel (the panel is this + 1 lines tall).
const SWIPER_ROWS: usize = 10;
/// Search panel candidates longer than this are cut off.
const SWIPER_COLS: usize = 400;
/// Height of the search panel: the query line, the rows, and two 1px borders.
const SWIPER_PANEL_H: f32 = (SWIPER_ROWS as f32 + 1.0) * LINE_HEIGHT + 2.0;

pub struct EmaxView {
    ed: Editor,
    keys: Dispatcher,
    echo: Option<SharedString>,
    /// One scroll position per buffer, so switching back restores the view.
    scrolls: HashMap<u64, UniformListScrollHandle>,
    focus: FocusHandle,
    /// Whether the search panel was on screen in the last rendered frame (the
    /// buffer list's last measured height includes or excludes it).
    panel_at_layout: bool,
    key_started: Option<Instant>,
    last_latency_us: u128,
    rows_us: Rc<Cell<u128>>,
    /// Scroll offset when the search opened, restored if it is cancelled.
    swiper_scroll: Option<Point<Pixels>>,
}

impl EmaxView {
    pub fn new(ed: Editor, cx: &mut Context<Self>) -> Self {
        let echo = ed.message.clone().map(SharedString::from);
        Self {
            ed,
            keys: Dispatcher::new(Keymap::emacs_default()),
            echo,
            scrolls: HashMap::new(),
            focus: cx.focus_handle(),
            panel_at_layout: false,
            key_started: None,
            last_latency_us: 0,
            rows_us: Rc::new(Cell::new(0)),
            swiper_scroll: None,
        }
    }

    fn scroll_for(&mut self, id: u64) -> UniformListScrollHandle {
        self.scrolls.entry(id).or_default().clone()
    }

    fn on_key(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let key = to_key(&ev.keystroke);
        self.handle_key(key, cx);
    }

    fn handle_key(&mut self, key: Key, cx: &mut Context<Self>) {
        self.key_started = Some(Instant::now());

        if self.ed.picker.is_some() {
            self.ed.picker_key(&key);
            self.after_picker();
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if self.ed.swiper.is_some() {
            let exit = self.ed.swiper_key(&key);
            self.after_swiper(exit);
            cx.stop_propagation();
            cx.notify();
            return;
        }

        match self.keys.dispatch(&key) {
            Outcome::Pending(echo) => self.echo = Some(echo.into()),
            Outcome::Undefined(seq) => self.echo = Some(format!("{seq} is undefined").into()),
            Outcome::Run(cmd, args) => {
                if cmd == "yank" {
                    // interprogram-paste: pick up text copied in other apps.
                    if let Some(t) = cx.read_from_clipboard().and_then(|c| c.text()) {
                        self.ed.clipboard_in(t);
                    }
                }
                self.ed.page_lines = self.viewport().2;
                self.ed.execute(cmd, args);
                if let Some(s) = self.ed.clipboard_out.take() {
                    cx.write_to_clipboard(ClipboardItem::new_string(s));
                }
                if self.ed.quit_requested {
                    cx.quit();
                }
                self.echo = self.ed.message.clone().map(Into::into);
                if self.ed.swiper.is_some() && self.swiper_scroll.is_none() {
                    self.swiper_scroll = Some(self.scroll_for(self.ed.buf_id).0.borrow().base_handle.offset());
                }
                self.apply_scroll();
            }
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// After the picker handled a key (or a click): update echo and scrolling.
    fn after_picker(&mut self) {
        match &self.ed.picker {
            Some(p) => self.echo = p.message.clone().map(Into::into),
            None => {
                self.echo = self.ed.message.take().map(Into::into);
                self.apply_scroll();
            }
        }
    }

    /// After the search handled a key (or a click): update echo and scrolling.
    fn after_swiper(&mut self, exit: SwiperExit) {
        match exit {
            SwiperExit::Open => {
                self.echo = None;
                self.apply_scroll();
            }
            SwiperExit::Accepted => {
                self.swiper_scroll = None;
                self.echo = self.ed.message.take().map(Into::into);
                self.apply_scroll();
            }
            SwiperExit::Cancelled => {
                self.echo = self.ed.message.take().map(Into::into);
                if let Some(offset) = self.swiper_scroll.take() {
                    self.scroll_for(self.ed.buf_id).0.borrow().base_handle.set_offset(offset);
                }
            }
        }
    }

    /// The current buffer's viewport: (top in px, height in px, whole rows).
    ///
    /// Read from the list's live scroll state rather than from the last painted
    /// frame, so several keys handled before the next frame each see the scroll
    /// the previous one set. The height comes from the last layout, corrected
    /// when the search panel has opened or closed since.
    fn viewport(&mut self) -> (f32, f32, usize) {
        let handle = self.scroll_for(self.ed.buf_id);
        let state = handle.0.borrow();
        let top = -f32::from(state.base_handle.offset().y);
        let mut h = state.last_item_size.map_or(30.0 * LINE_HEIGHT, |s| f32::from(s.item.height));
        match (self.panel_at_layout, self.ed.swiper.is_some()) {
            (false, true) => h -= SWIPER_PANEL_H,
            (true, false) => h += SWIPER_PANEL_H,
            _ => {}
        }
        let h = h.max(LINE_HEIGHT);
        (top.max(0.0), h, ((h / LINE_HEIGHT).floor() as usize).max(1))
    }

    /// Scroll the current buffer as the last command asked (`Editor.scroll`),
    /// by setting the list's offset directly.
    fn apply_scroll(&mut self) {
        if self.ed.scroll == Scroll::Keep {
            return;
        }
        let (top, h, rows) = self.viewport();
        let r = self.ed.rope();
        let target = self.ed.reveal.unwrap_or(self.ed.primary_cursor().point);
        let line = r.char_to_line(target.min(r.len_chars()));
        let max_top = (r.len_lines() as f32 * LINE_HEIGHT - h).max(0.0);
        let line_top = line as f32 * LINE_HEIGHT;
        let line_bottom = line_top + LINE_HEIGHT;
        let center = line_top + LINE_HEIGHT / 2.0 - h / 2.0;
        let new_top = match self.ed.scroll {
            Scroll::Keep => return,
            Scroll::EnsureVisible => {
                if line_top < top {
                    // Just above: scroll minimally; far away: center it.
                    if top - line_top <= h / 2.0 { line_top } else { center }
                } else if line_bottom > top + h {
                    if line_bottom - (top + h) <= h / 2.0 { line_bottom - h } else { center }
                } else {
                    return;
                }
            }
            Scroll::Center => center,
            Scroll::Top => line_top,
            Scroll::Bottom => line_bottom - h,
            Scroll::Page(dir) => top + dir as f32 * rows.saturating_sub(2).max(1) as f32 * LINE_HEIGHT,
        };
        let handle = self.scroll_for(self.ed.buf_id);
        let state = handle.0.borrow();
        let x = state.base_handle.offset().x;
        state.base_handle.set_offset(point(x, px(-new_top.clamp(0.0, max_top))));
    }

    fn render_picker(&self, p: &Picker, cx: &mut Context<Self>) -> AnyElement {
        let n = p.matches.len();
        let start = (p.selected + 1).saturating_sub(PICKER_ROWS);
        let rows: Vec<_> = p
            .matches
            .iter()
            .enumerate()
            .skip(start)
            .take(PICKER_ROWS)
            .map(|(i, m)| {
                let c = &p.candidates[m.index];
                let selected = i == p.selected;
                div()
                    .id(("pick", i))
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .h(px(32.0))
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |d| d.bg(rgb(ROW_SELECTED)))
                    .when(!selected, |d| d.hover(|s| s.bg(rgb(ROW_HOVER))))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.ed.picker_accept(i);
                        this.after_picker();
                        cx.notify();
                    }))
                    .child(
                        div()
                            .w(px(3.0))
                            .h(px(16.0))
                            .rounded_sm()
                            .when(selected, |d| d.bg(rgb(CURSOR))),
                    )
                    .child(div().w(px(10.0)).text_color(rgb(CURSOR2)).child(if c.modified { "●" } else { "" }))
                    .child(
                        div()
                            .flex_none()
                            .text_color(rgb(if selected { 0xffffff } else { FG }))
                            .when(selected, |d| d.font_weight(FontWeight::SEMIBOLD))
                            .child(highlighted(&c.label, &m.positions)),
                    )
                    .when(c.current, |d| {
                        d.child(
                            div()
                                .px_1p5()
                                .rounded_sm()
                                .bg(rgb(0x353a45))
                                .text_size(px(11.0))
                                .text_color(rgb(DIM))
                                .child("current"),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(12.5))
                            .text_color(rgb(DIM))
                            .child(c.detail.clone()),
                    )
                    .child(div().flex_none().text_size(px(12.0)).text_color(rgb(DIM)).child(c.annotation.clone()))
            })
            .collect();

        let query: SharedString = p.query.clone().into();
        let empty = n == 0;
        let footer_hint = if empty && !p.query.trim().is_empty() {
            format!("↵ create buffer “{}”    C-g cancel", p.query.trim())
        } else {
            "↵ switch    C-n/C-p move    C-j create    C-k kill    C-g cancel".to_string()
        };

        let panel = div()
            .id("picker")
            .occlude()
            .w(px(720.0))
            .max_w_full()
            .flex()
            .flex_col()
            .bg(rgb(PANEL_BG))
            .border_1()
            .border_color(rgb(PANEL_BORDER))
            .rounded_xl()
            .shadow_2xl()
            .overflow_hidden()
            .font_family(UI_FONT)
            .text_size(px(14.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .pt_3()
                    .text_size(px(11.5))
                    .text_color(rgb(DIM))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(p.prompt.to_uppercase()))
                    .child(div().flex_1())
                    .child(format!("{n} / {}", p.candidates.len())),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .py_2p5()
                    .border_b_1()
                    .border_color(rgb(PANEL_BORDER))
                    .text_size(px(16.0))
                    .child(div().text_color(rgb(ACCENT)).child("❯"))
                    .child(query_with_caret(&query, px(18.0)))
                    .when(query.is_empty(), |d| {
                        d.child(div().text_color(rgb(DIM)).child("Type to filter buffers…"))
                    }),
            )
            .child(
                div()
                    .p_1p5()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .children(rows)
                    .when(empty, |d| {
                        d.child(
                            div()
                                .px_3()
                                .py_4()
                                .text_color(rgb(DIM))
                                .child(if p.query.trim().is_empty() { "No buffers".to_string() } else { "No matching buffer".to_string() }),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .px_4()
                    .py_2()
                    .border_t_1()
                    .border_color(rgb(PANEL_BORDER))
                    .text_size(px(12.0))
                    .text_color(rgb(DIM))
                    .child(div().flex_1().child(footer_hint))
                    .when_some(p.message.clone(), |d, m| d.child(div().text_color(rgb(CURSOR2)).child(m))),
            );

        div()
            .absolute()
            .inset_0()
            .bg(rgba(0x0b0c0f80))
            .flex()
            .justify_center()
            .items_start()
            .pt(px(90.0))
            .px_4()
            .child(panel)
            .into_any_element()
    }
}

impl EmaxView {
    /// The search panel, docked under the buffer like Emacs' minibuffer: the
    /// query line, then a window of matching lines around the selection.
    fn render_swiper(&self, s: &Swiper, gutter_w: gpui::Pixels, cx: &mut Context<Self>) -> AnyElement {
        let n = s.matches.len();
        let start = s.selected.saturating_sub(SWIPER_ROWS / 2).min(n.saturating_sub(SWIPER_ROWS));
        let mut rows: Vec<AnyElement> = s
            .matches
            .iter()
            .enumerate()
            .skip(start)
            .take(SWIPER_ROWS)
            .map(|(i, &line)| {
                let selected = i == s.selected;
                // Tabs become one space so match columns still line up.
                let text: String = s.line_text(line).chars().take(SWIPER_COLS).map(|c| if c == '\t' { ' ' } else { c }).collect();
                let positions: Vec<usize> =
                    s.highlights(line).into_iter().flatten().filter(|&p| p < SWIPER_COLS).collect();
                div()
                    .id(("swipe", i))
                    .flex()
                    .h(px(LINE_HEIGHT))
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .cursor_pointer()
                    .when(selected, |d| d.bg(rgb(ROW_SELECTED)))
                    .when(!selected, |d| d.hover(|st| st.bg(rgb(ROW_HOVER))))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let exit = this.ed.swiper_accept_index(i);
                        this.after_swiper(exit);
                        cx.notify();
                    }))
                    .child(div().w(px(3.0)).flex_none().when(selected, |d| d.bg(rgb(CURSOR))))
                    .child(
                        div()
                            .w(gutter_w - px(3.0))
                            .flex_none()
                            .pr_3()
                            .flex()
                            .justify_end()
                            .text_color(rgb(if selected { FG } else { DIM }))
                            .child(format!("{}", line + 1)),
                    )
                    .child(
                        div()
                            .text_color(rgb(if selected { 0xffffff } else { FG }))
                            .child(highlighted(&text, &positions)),
                    )
                    .into_any_element()
            })
            .collect();
        if n == 0 {
            rows.push(
                div()
                    .h(px(LINE_HEIGHT))
                    .pl(gutter_w)
                    .text_color(rgb(DIM))
                    .child(if s.has_terms() { "No match" } else { "Empty buffer" })
                    .into_any_element(),
            );
        }
        // Pad so the panel keeps a fixed height while the list shrinks.
        while rows.len() < SWIPER_ROWS {
            rows.push(div().h(px(LINE_HEIGHT)).into_any_element());
        }

        let query: SharedString = s.query.clone().into();
        let count = if n == 0 { "0/0".to_string() } else { format!("{}/{}", s.selected + 1, n) };
        let input = div()
            .flex()
            .items_center()
            .gap_2()
            .h(px(LINE_HEIGHT))
            .px_3()
            .border_b_1()
            .border_color(rgb(PANEL_BORDER))
            .child(div().text_color(rgb(ACCENT)).font_weight(FontWeight::SEMIBOLD).child(if s.backward { "Swiper ↑" } else { "Swiper" }))
            .child(div().text_color(rgb(DIM)).child(count))
            .child(div().text_color(rgb(ACCENT)).child("❯"))
            .child(query_with_caret(&query, px(16.0)))
            .when(query.is_empty(), |d| d.child(div().text_color(rgb(DIM)).child("Search…  (space separates terms)")))
            .child(div().flex_1())
            .child(
                div()
                    .font_family(UI_FONT)
                    .text_size(px(12.0))
                    .text_color(rgb(DIM))
                    .child("↵ jump    C-s/C-r next/prev    M-n symbol    C-g cancel"),
            );

        div()
            .id("swiper")
            .occlude()
            .flex_none()
            .flex()
            .flex_col()
            .bg(rgb(PANEL_BG))
            .border_t_1()
            .border_color(rgb(PANEL_BORDER))
            .child(input)
            .children(rows)
            .into_any_element()
    }
}

/// The typed query followed directly by the caret. Spaces are shown as
/// non-breaking spaces so a trailing space still moves the caret.
fn query_with_caret(query: &str, caret_h: gpui::Pixels) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .when(!query.is_empty(), |d| d.child(div().text_color(rgb(0xffffff)).child(query.replace(' ', "\u{a0}"))))
        .child(div().w(px(2.0)).h(caret_h).bg(rgb(CURSOR)))
}

/// A label with the fuzzy-matched characters highlighted.
fn highlighted(label: &str, positions: &[usize]) -> StyledText {
    let style = HighlightStyle { color: Some(rgb(MATCH).into()), font_weight: Some(FontWeight::BOLD), ..Default::default() };
    let mut ranges: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    let offsets: Vec<(usize, char)> = label.char_indices().collect();
    for &p in positions {
        let Some(&(b, ch)) = offsets.get(p) else { continue };
        let r = b..b + ch.len_utf8();
        match ranges.last_mut() {
            Some((last, _)) if last.end == r.start => last.end = r.end,
            _ => ranges.push((r, style)),
        }
    }
    StyledText::new(label.to_string()).with_highlights(ranges)
}

/// Translate a platform keystroke into an Emacs key (Option = Meta, Cmd = Super).
fn to_key(ks: &Keystroke) -> Key {
    let m = ks.modifiers;
    let mut shift = m.shift;
    let mut name = match ks.key.as_str() {
        "space" => "SPC".to_string(),
        "enter" => "RET".to_string(),
        "tab" => "TAB".to_string(),
        "backspace" => "DEL".to_string(),
        "delete" => "<delete>".to_string(),
        "escape" => "ESC".to_string(),
        "up" | "down" | "left" | "right" | "home" | "end" => format!("<{}>", ks.key),
        "pageup" => "<prior>".to_string(),
        "pagedown" => "<next>".to_string(),
        k if k.len() > 1 && k.starts_with('f') && k[1..].parse::<u8>().is_ok() => format!("<{k}>"),
        k => k.to_string(),
    };
    // Fold shifted punctuation into the character it types (US layout): C-S-. -> C->.
    if name.chars().count() == 1 {
        let c = name.chars().next().unwrap();
        if !c.is_alphabetic() {
            if shift {
                name = shifted(c).to_string();
            }
            shift = false;
        }
    }
    let text = ks.key_char.clone().or_else(|| (name == "SPC").then(|| " ".into()));
    Key { ctrl: m.control, meta: m.alt, shift, sup: m.platform, name, text }
}

fn shifted(c: char) -> char {
    const PAIRS: &[(char, char)] = &[
        ('1', '!'), ('2', '@'), ('3', '#'), ('4', '$'), ('5', '%'), ('6', '^'), ('7', '&'),
        ('8', '*'), ('9', '('), ('0', ')'), ('-', '_'), ('=', '+'), ('[', '{'), (']', '}'),
        ('\\', '|'), (';', ':'), ('\'', '"'), (',', '<'), ('.', '>'), ('/', '?'), ('`', '~'),
    ];
    PAIRS.iter().find(|(a, _)| *a == c).map(|(_, b)| *b).unwrap_or(c)
}

// ---------------------------------------------------------------- rendering

#[derive(Clone, Copy, PartialEq)]
enum CellKind {
    Plain,
    Match,
    CurrentMatch,
    Region,
    Cursor,
    Cursor2,
}

/// Build the styled text for one line: tabs expanded; search matches (char
/// columns, `current` for the selected one), region and cursor cells highlighted.
fn render_line(v: &BufView, line: usize, show_cursors: bool, matches: &[Range<usize>], current: bool) -> StyledText {
    let r = &v.buf.rope;
    let start = r.line_to_char(line);
    let len = text::line_len(r, line);
    let shown = len.min(MAX_RENDER_COLS);

    // Display string plus byte offset of each cell (one cell per char, +1 trailing cell).
    let mut display = String::with_capacity(shown + 1);
    let mut offs = Vec::with_capacity(shown + 2);
    for ch in r.slice(start..start + shown).chars() {
        offs.push(display.len());
        if ch == '\t' {
            let col = display.chars().count();
            display.extend(std::iter::repeat_n(' ', TAB_WIDTH - col % TAB_WIDTH));
        } else {
            display.push(ch);
        }
    }
    offs.push(display.len());
    display.push(' '); // cell for a cursor at end of line / region over the newline
    offs.push(display.len());

    let cells = shown + 1;
    let mut class = vec![CellKind::Plain; cells];
    let hl = if current { CellKind::CurrentMatch } else { CellKind::Match };
    for m in matches {
        for col in m.start.min(shown)..m.end.min(shown) {
            class[col] = hl;
        }
    }
    if show_cursors {
        let line_end_incl = start + len + 1; // includes the newline position
        for c in v.cursors {
            if let Some(reg) = c.region() {
                for p in reg.start.max(start)..reg.end.min(line_end_incl) {
                    class[(p - start).min(cells - 1)] = CellKind::Region;
                }
            }
        }
        for c in v.cursors {
            if c.point >= start && c.point <= start + len && c.point - start < cells {
                class[c.point - start] = if c.id == v.primary { CellKind::Cursor } else { CellKind::Cursor2 };
            }
        }
    }

    let style = |k: CellKind| match k {
        CellKind::Plain => None,
        CellKind::Match => Some(HighlightStyle { background_color: Some(rgb(SEARCH_HL).into()), ..Default::default() }),
        CellKind::CurrentMatch => Some(HighlightStyle {
            background_color: Some(rgb(SEARCH_CUR).into()),
            color: Some(rgb(BG).into()),
            ..Default::default()
        }),
        CellKind::Region => Some(HighlightStyle { background_color: Some(rgb(REGION).into()), ..Default::default() }),
        CellKind::Cursor => Some(HighlightStyle {
            background_color: Some(rgb(CURSOR).into()),
            color: Some(rgb(BG).into()),
            ..Default::default()
        }),
        CellKind::Cursor2 => Some(HighlightStyle {
            background_color: Some(rgb(CURSOR2).into()),
            color: Some(rgb(BG).into()),
            ..Default::default()
        }),
    };
    let mut highlights: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    let mut i = 0;
    while i < cells {
        let k = class[i];
        let mut j = i + 1;
        while j < cells && class[j] == k {
            j += 1;
        }
        if let Some(s) = style(k) {
            highlights.push((offs[i]..offs[j], s));
        }
        i = j;
    }
    StyledText::new(display).with_highlights(highlights)
}

impl Focusable for EmaxView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for EmaxView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t0 = Instant::now();
        if let Some(start) = self.key_started.take() {
            self.last_latency_us = start.elapsed().as_micros();
        }

        let (view_id, view_name, total, cur_line, cur_col, cursor_count, region_len, modified, previewing) = {
            let v = self.ed.display_view();
            let r = &v.buf.rope;
            let p = v.cursors.iter().find(|c| c.id == v.primary).map_or(0, |c| c.point).min(r.len_chars());
            let line = r.char_to_line(p);
            let region_len: usize = v.cursors.iter().filter_map(|c| c.region()).map(|r| r.len()).sum();
            (
                v.id,
                v.buf.name.clone(),
                r.len_lines(),
                line,
                p - r.line_to_char(line),
                v.cursors.len(),
                region_len,
                v.buf.modified(),
                v.id != self.ed.buf_id,
            )
        };
        let gutter_w = px(((total.to_string().len().max(3) + 2) as f32) * FONT_SIZE * 0.62);
        let rows_us = self.rows_us.clone();
        self.panel_at_layout = self.ed.swiper.is_some();
        let scroll = self.scroll_for(view_id);

        let list = uniform_list(
            "buffer",
            total,
            cx.processor(move |this: &mut EmaxView, range: Range<usize>, _w, _cx| {
                let t = Instant::now();
                let v = this.ed.display_view();
                let search = this.ed.swiper.as_ref();
                let selected_match = search.and_then(|s| s.selected_line());
                let rows: Vec<_> = range
                    .map(|ix| {
                        let is_cur = ix == cur_line;
                        div()
                            .id(ix)
                            .flex()
                            .w_full()
                            .h(px(LINE_HEIGHT))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .when(is_cur, |d| d.bg(rgb(CUR_LINE)))
                            .child(
                                div()
                                    .w(gutter_w)
                                    .flex_none()
                                    .pr_3()
                                    .flex()
                                    .justify_end()
                                    .text_color(rgb(if is_cur { FG } else { DIM }))
                                    .child(format!("{}", ix + 1)),
                            )
                            .child({
                                let hl = search.map(|s| s.highlights(ix)).unwrap_or_default();
                                render_line(&v, ix, true, &hl, selected_match == Some(ix))
                            })
                    })
                    .collect();
                rows_us.set(t.elapsed().as_micros());
                rows
            }),
        )
        .track_scroll(scroll)
        .flex_1();

        let modeline = div()
            .flex()
            .gap_4()
            .px_3()
            .py_1()
            .bg(rgb(if previewing { 0x33304a } else { MODELINE_BG }))
            .text_color(rgb(FG))
            .child(div().text_color(rgb(ACCENT)).child(if modified { "●" } else { "○" }))
            .child(div().font_weight(FontWeight::BOLD).child(view_name))
            .when(previewing, |d| d.child(div().text_color(rgb(ACCENT)).child("preview")))
            .child(format!("L{} C{}", cur_line + 1, cur_col))
            .when(cursor_count > 1, |d| {
                d.child(div().text_color(rgb(CURSOR2)).child(format!("{cursor_count} cursors")))
            })
            .when(region_len > 0, |d| d.child(div().text_color(rgb(CURSOR)).child(format!("{region_len} selected"))))
            .child(format!("{total} lines"))
            .when(self.ed.buffer_count() > 1, |d| {
                d.child(div().text_color(rgb(DIM)).child(format!("{} buffers", self.ed.buffer_count())))
            })
            .child(div().flex_1())
            .child(div().text_color(rgb(DIM)).child(format!(
                "key→view {} µs · rows {} µs · view {} µs",
                self.last_latency_us,
                self.rows_us.get(),
                t0.elapsed().as_micros()
            )));

        let echo = div()
            .h(px(LINE_HEIGHT + 6.0))
            .px_3()
            .flex()
            .items_center()
            .text_color(rgb(FG))
            .child(self.echo.clone().unwrap_or_default());

        let picker = self.ed.picker.as_ref().map(|p| self.render_picker(p, cx));
        let search = self.ed.swiper.as_ref().map(|s| self.render_swiper(s, gutter_w, cx));

        div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BG))
            .text_color(rgb(FG))
            .font_family(FONT)
            .text_size(px(FONT_SIZE))
            .line_height(px(LINE_HEIGHT))
            .child(list)
            .children(search)
            .child(modeline)
            .child(echo)
            .children(picker)
    }
}
