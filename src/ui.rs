//! GPUI view: renders the editor (virtualised lines, regions, multiple cursors,
//! mode line, echo area, the buffer picker overlay, the swiper search panel)
//! and translates platform
//! key events into Emacs keys. Also runs the editor's ticks: when a PTY
//! backend has output (it calls the waker from its own thread) and when the
//! next hook timer is due.

mod term_view;

use std::cell::Cell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt;

use gpui::{
    AnyElement, App, ClipboardItem, Context, FocusHandle, Focusable, FontWeight, HighlightStyle, KeyDownEvent,
    Keystroke, Pixels, Point, SharedString, StyledText, Task, UniformListScrollHandle, Window, div,
    point, prelude::*, px, rgba, uniform_list,
};

use emax::engine::picker::{Picker, PickerKind};
use emax::engine::{Swiper, SwiperExit};
use emax::engine::term::Palette;
use emax::engine::{Appearance, Args, BufView, Dispatcher, Editor, Key, Keymap, Keymaps, Outcome, Scroll, Theme, text};

// ------------------------------------------------------------------ layout

/// The font size the panel sizes below are written for. Panels scale with
/// `font.size` relative to this, so they look the same at any size.
const DESIGN_FONT_SIZE: f32 = 14.0;
/// GPUI's default rem size; spacing utilities (`px_3`, `gap_2`…) are in rems.
const DESIGN_REM: f32 = 16.0;
/// Lines longer than this are truncated on screen (prototype limitation).
const MAX_RENDER_COLS: usize = 2000;
const PICKER_ROWS: usize = 12;
/// Candidate rows in the search panel (the panel is this + 1 lines tall).
const SWIPER_ROWS: usize = 10;
/// Search panel candidates longer than this are cut off.
const SWIPER_COLS: usize = 400;

pub struct EmaxView {
    ed: Editor,
    keys: Dispatcher,
    /// Keys while the buffer picker / the search panel is open.
    picker_keys: Keymap,
    search_keys: Keymap,
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
    appearance: Appearance,
    /// Whether the window has a dark appearance (picks the dark theme).
    dark: bool,
    /// Wakes the view when the next hook timer is due (dropping it cancels).
    timer_task: Option<Task<()>>,
    /// The deadline `timer_task` waits for.
    timer_deadline: Option<Instant>,
}

impl EmaxView {
    pub fn new(mut ed: Editor, keymaps: Keymaps, appearance: Appearance, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let echo = ed.message.clone().map(SharedString::from);
        // PTY backends call the waker from their threads; the view ticks the
        // editor on its own thread when it fires (wake-ups coalesce).
        let (wake_tx, mut wake_rx) = futures::channel::mpsc::unbounded::<()>();
        ed.set_waker(Arc::new(move || {
            let _ = wake_tx.unbounded_send(());
        }));
        cx.spawn(async move |this, cx| {
            while wake_rx.next().await.is_some() {
                while wake_rx.try_recv().is_ok() {}
                if this.update(cx, |view, cx| view.tick(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        // Follow the system's light/dark switch while running.
        cx.observe_window_appearance(window, |this, window, cx| {
            this.dark = is_dark(window.appearance());
            cx.notify();
        })
        .detach();
        Self {
            ed,
            keys: Dispatcher::new(),
            picker_keys: keymaps.picker,
            search_keys: keymaps.search,
            echo,
            scrolls: HashMap::new(),
            focus: cx.focus_handle(),
            panel_at_layout: false,
            key_started: None,
            last_latency_us: 0,
            rows_us: Rc::new(Cell::new(0)),
            swiper_scroll: None,
            appearance,
            dark: is_dark(window.appearance()),
            timer_task: None,
            timer_deadline: None,
        }
    }

    /// Let the editor read terminal output, fire due timers and run hooks;
    /// redraw if anything happened.
    fn tick(&mut self, cx: &mut Context<Self>) {
        if self.ed.tick(Instant::now()) {
            if let Some(m) = self.ed.message.clone() {
                self.echo = Some(m.into());
            }
            if self.ed.quit_requested {
                cx.quit();
            }
            cx.notify();
        }
        self.schedule_timer(cx);
    }

    /// Make sure the view ticks when the editor's next timer is due.
    fn schedule_timer(&mut self, cx: &mut Context<Self>) {
        let next = self.ed.hooks.next_deadline();
        if next == self.timer_deadline {
            return;
        }
        self.timer_deadline = next;
        self.timer_task = next.map(|due| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(due.saturating_duration_since(Instant::now())).await;
                let _ = this.update(cx, |view, cx| {
                    view.timer_deadline = None;
                    view.tick(cx);
                });
            })
        });
    }

    fn theme(&self) -> &Theme {
        self.appearance.themes.get(self.dark)
    }

    fn line_height(&self) -> f32 {
        self.appearance.fonts.line_height
    }

    /// How much larger than the design size the UI is drawn.
    fn ui_scale(&self) -> f32 {
        self.appearance.fonts.size / DESIGN_FONT_SIZE
    }

    /// A size in the panels, given at the design font size.
    fn ui(&self, design_px: f32) -> Pixels {
        px(design_px * self.ui_scale())
    }

    /// Height of the search panel: the query line, the rows, and two 1px borders.
    fn swiper_panel_height(&self) -> f32 {
        (SWIPER_ROWS as f32 + 1.0) * self.line_height() + 2.0
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
            self.ed.picker_key(&self.picker_keys, &key);
            self.after_picker(cx);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if self.ed.swiper.is_some() {
            let exit = self.ed.swiper_key(&self.search_keys, &key);
            self.after_swiper(exit);
            cx.stop_propagation();
            cx.notify();
            return;
        }

        // Terminal buffers in char mode take most keys themselves.
        if !self.keys.is_pending() && self.ed.terminal_key(&key) {
            self.echo = None;
            cx.stop_propagation();
            cx.notify();
            return;
        }

        let outcome = self.keys.dispatch(&key, &self.ed.active_keymaps());
        self.outcome(outcome, cx);
        self.schedule_timer(cx);
        cx.stop_propagation();
        cx.notify();
    }

    fn outcome(&mut self, outcome: Outcome, cx: &mut Context<Self>) {
        match outcome {
            Outcome::Pending(echo) => self.echo = Some(echo.into()),
            Outcome::Undefined(seq) => self.echo = Some(format!("{seq} is undefined").into()),
            Outcome::Run(cmd, args) => self.run_command(cmd, args, cx),
        }
    }

    fn run_command(&mut self, cmd: &'static str, args: Args, cx: &mut Context<Self>) {
                if matches!(cmd, "yank" | "term-paste") {
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

    /// After the picker handled a key (or a click): update echo and scrolling,
    /// and run the command M-x chose.
    fn after_picker(&mut self, cx: &mut Context<Self>) {
        match &self.ed.picker {
            Some(p) => self.echo = p.message.clone().map(Into::into),
            None => {
                self.echo = self.ed.message.take().map(Into::into);
                self.apply_scroll();
            }
        }
        if let Some((cmd, args)) = self.ed.pending_command.take() {
            let outcome = self.keys.invoke(cmd, args);
            self.outcome(outcome, cx);
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
        let (lh, panel_h) = (self.line_height(), self.swiper_panel_height());
        let handle = self.scroll_for(self.ed.buf_id);
        let state = handle.0.borrow();
        let top = -f32::from(state.base_handle.offset().y);
        let mut h = state.last_item_size.map_or(30.0 * lh, |s| f32::from(s.item.height));
        match (self.panel_at_layout, self.ed.swiper.is_some()) {
            (false, true) => h -= panel_h,
            (true, false) => h += panel_h,
            _ => {}
        }
        let h = h.max(lh);
        (top.max(0.0), h, ((h / lh).floor() as usize).max(1))
    }

    /// Scroll the current buffer as the last command asked (`Editor.scroll`),
    /// by setting the list's offset directly.
    fn apply_scroll(&mut self) {
        if self.ed.scroll == Scroll::Keep {
            return;
        }
        let (top, h, rows) = self.viewport();
        let lh = self.line_height();
        let r = self.ed.rope();
        let target = self.ed.reveal.unwrap_or(self.ed.primary_cursor().point);
        let line = r.char_to_line(target.min(r.len_chars()));
        let max_top = (r.len_lines() as f32 * lh - h).max(0.0);
        let line_top = line as f32 * lh;
        let line_bottom = line_top + lh;
        let center = line_top + lh / 2.0 - h / 2.0;
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
            Scroll::Page(dir) => top + dir as f32 * rows.saturating_sub(2).max(1) as f32 * lh,
        };
        let handle = self.scroll_for(self.ed.buf_id);
        let state = handle.0.borrow();
        let x = state.base_handle.offset().x;
        state.base_handle.set_offset(point(x, px(-new_top.clamp(0.0, max_top))));
    }

    fn render_picker(&self, p: &Picker, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme();
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
                    .h(self.ui(32.0))
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |d| d.bg(rgba(t.row_selected)))
                    .when(!selected, |d| d.hover(|s| s.bg(rgba(t.row_hover))))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.ed.picker_accept(i);
                        this.after_picker(cx);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .w(self.ui(3.0))
                            .h(self.ui(16.0))
                            .rounded_sm()
                            .when(selected, |d| d.bg(rgba(t.cursor))),
                    )
                    .child(div().w(self.ui(10.0)).text_color(rgba(t.secondary_cursor)).child(if c.modified { "●" } else { "" }))
                    .child(
                        div()
                            .flex_none()
                            .text_color(rgba(if selected { t.emphasis } else { t.foreground }))
                            .when(selected, |d| d.font_weight(FontWeight::SEMIBOLD))
                            .child(highlighted(&c.label, &m.positions, t)),
                    )
                    .when(c.current, |d| {
                        d.child(
                            div()
                                .px_1p5()
                                .rounded_sm()
                                .bg(rgba(t.badge))
                                .text_size(self.ui(11.0))
                                .text_color(rgba(t.dim))
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
                            .text_size(self.ui(12.5))
                            .text_color(rgba(t.dim))
                            .child(c.detail.clone()),
                    )
                    .child(div().flex_none().text_size(self.ui(12.0)).text_color(rgba(t.dim)).child(c.annotation.clone()))
            })
            .collect();

        let query: SharedString = p.query.clone().into();
        let empty = n == 0;
        let commands = matches!(p.kind, PickerKind::Commands { .. });
        let footer_hint = if commands {
            hints(&self.picker_keys, &[("picker-accept", "run"), ("picker-next", "next"), ("picker-previous", "prev"), ("picker-quit", "cancel")])
        } else if empty && !p.query.trim().is_empty() {
            hints(&self.picker_keys, &[("picker-accept", &format!("create buffer “{}”", p.query.trim())), ("picker-quit", "cancel")])
        } else {
            hints(
                &self.picker_keys,
                &[
                    ("picker-accept", "switch"),
                    ("picker-next", "next"),
                    ("picker-previous", "prev"),
                    ("picker-accept-input", "create"),
                    ("picker-kill-buffer", "kill"),
                    ("picker-quit", "cancel"),
                ],
            )
        };

        let panel = div()
            .id("picker")
            .occlude()
            .w(self.ui(720.0))
            .max_w_full()
            .flex()
            .flex_col()
            .bg(rgba(t.panel))
            .border_1()
            .border_color(rgba(t.panel_border))
            .rounded_xl()
            .shadow_2xl()
            .overflow_hidden()
            .font_family(self.appearance.fonts.ui_family.clone())
            .text_size(self.ui(14.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .pt_3()
                    .text_size(self.ui(11.5))
                    .text_color(rgba(t.dim))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(if commands { p.prompt.clone() } else { p.prompt.to_uppercase() }))
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
                    .border_color(rgba(t.panel_border))
                    .text_size(self.ui(16.0))
                    .child(div().text_color(rgba(t.accent)).child("❯"))
                    .child(query_with_caret(&query, self.ui(18.0), t))
                    .when(query.is_empty(), |d| {
                        d.child(div().text_color(rgba(t.dim)).child(if commands { "Type to filter commands…" } else { "Type to filter buffers…" }))
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
                                .text_color(rgba(t.dim))
                                .child(match (commands, p.query.trim().is_empty()) {
                                    (true, _) => "No matching command",
                                    (false, true) => "No buffers",
                                    (false, false) => "No matching buffer",
                                }),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .px_4()
                    .py_2()
                    .border_t_1()
                    .border_color(rgba(t.panel_border))
                    .text_size(self.ui(12.0))
                    .text_color(rgba(t.dim))
                    .child(div().flex_1().child(footer_hint))
                    .when_some(p.message.clone(), |d, m| d.child(div().text_color(rgba(t.secondary_cursor)).child(m))),
            );

        div()
            .absolute()
            .inset_0()
            .bg(rgba(t.overlay))
            .flex()
            .justify_center()
            .items_start()
            .pt(self.ui(90.0))
            .px_4()
            .child(panel)
            .into_any_element()
    }
}

impl EmaxView {
    /// The search panel, docked under the buffer like Emacs' minibuffer: the
    /// query line, then a window of matching lines around the selection.
    fn render_swiper(&self, s: &Swiper, gutter_w: gpui::Pixels, cx: &mut Context<Self>) -> AnyElement {
        let (t, lh) = (self.theme(), self.line_height());
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
                    .h(px(lh))
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .cursor_pointer()
                    .when(selected, |d| d.bg(rgba(t.row_selected)))
                    .when(!selected, |d| d.hover(|st| st.bg(rgba(t.row_hover))))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let exit = this.ed.swiper_accept_index(i);
                        this.after_swiper(exit);
                        cx.notify();
                    }))
                    .child(div().w(self.ui(3.0)).flex_none().when(selected, |d| d.bg(rgba(t.cursor))))
                    .child(
                        div()
                            .w(gutter_w - self.ui(3.0))
                            .flex_none()
                            .pr_3()
                            .flex()
                            .justify_end()
                            .text_color(rgba(if selected { t.foreground } else { t.dim }))
                            .child(format!("{}", line + 1)),
                    )
                    .child(
                        div()
                            .text_color(rgba(if selected { t.emphasis } else { t.foreground }))
                            .child(highlighted(&text, &positions, t)),
                    )
                    .into_any_element()
            })
            .collect();
        if n == 0 {
            rows.push(
                div()
                    .h(px(lh))
                    .pl(gutter_w)
                    .text_color(rgba(t.dim))
                    .child(if s.has_terms() { "No match" } else { "Empty buffer" })
                    .into_any_element(),
            );
        }
        // Pad so the panel keeps a fixed height while the list shrinks.
        while rows.len() < SWIPER_ROWS {
            rows.push(div().h(px(lh)).into_any_element());
        }

        let query: SharedString = s.query.clone().into();
        let count = if n == 0 { "0/0".to_string() } else { format!("{}/{}", s.selected + 1, n) };
        let input = div()
            .flex()
            .items_center()
            .gap_2()
            .h(px(lh))
            .px_3()
            .border_b_1()
            .border_color(rgba(t.panel_border))
            .child(div().text_color(rgba(t.accent)).font_weight(FontWeight::SEMIBOLD).child(if s.backward { "Swiper ↑" } else { "Swiper" }))
            .child(div().text_color(rgba(t.dim)).child(count))
            .child(div().text_color(rgba(t.accent)).child("❯"))
            .child(query_with_caret(&query, self.ui(16.0).min(px(lh)), t))
            .when(query.is_empty(), |d| d.child(div().text_color(rgba(t.dim)).child("Search…  (space separates terms)")))
            .child(div().flex_1())
            .child(
                div()
                    .font_family(self.appearance.fonts.ui_family.clone())
                    .text_size(self.ui(12.0))
                    .text_color(rgba(t.dim))
                    .child(hints(
                        &self.search_keys,
                        &[
                            ("search-accept", "jump"),
                            ("search-next-or-history", "next"),
                            ("search-previous-or-history", "prev"),
                            ("search-insert-symbol", "symbol"),
                            ("search-quit", "cancel"),
                        ],
                    )),
            );

        div()
            .id("swiper")
            .occlude()
            .flex_none()
            .flex()
            .flex_col()
            .bg(rgba(t.panel))
            .border_t_1()
            .border_color(rgba(t.panel_border))
            .child(input)
            .children(rows)
            .into_any_element()
    }
}

/// The typed query followed directly by the caret. Spaces are shown as
/// non-breaking spaces so a trailing space still moves the caret.
fn query_with_caret(query: &str, caret_h: gpui::Pixels, t: &Theme) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .when(!query.is_empty(), |d| d.child(div().text_color(rgba(t.emphasis)).child(query.replace(' ', "\u{a0}"))))
        .child(div().w(px(2.0)).h(caret_h).bg(rgba(t.cursor)))
}

/// Footer hints like "RET jump    C-s next", using the keys currently bound to
/// each action (actions with no key are left out).
fn hints(keymap: &Keymap, items: &[(&str, &str)]) -> String {
    items
        .iter()
        .filter_map(|(action, label)| {
            let key = keymap.keys_for(action).into_iter().next()?;
            Some(format!("{} {label}", if key == "RET" { "↵".to_string() } else { key }))
        })
        .collect::<Vec<_>>()
        .join("    ")
}

/// A label with the fuzzy-matched characters highlighted.
fn highlighted(label: &str, positions: &[usize], t: &Theme) -> StyledText {
    let style = HighlightStyle { color: Some(rgba(t.match_text).into()), font_weight: Some(FontWeight::BOLD), ..Default::default() };
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
        "up" | "down" | "left" | "right" | "home" | "end" | "insert" => format!("<{}>", ks.key),
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

fn is_dark(a: gpui::WindowAppearance) -> bool {
    matches!(a, gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark)
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
fn render_line(
    v: &BufView,
    line: usize,
    tab_width: usize,
    show_cursors: bool,
    matches: &[Range<usize>],
    current: bool,
    t: &Theme,
) -> StyledText {
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
            display.extend(std::iter::repeat_n(' ', tab_width - col % tab_width));
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
        CellKind::Match => Some(HighlightStyle { background_color: Some(rgba(t.search_match).into()), ..Default::default() }),
        CellKind::CurrentMatch => Some(HighlightStyle {
            background_color: Some(rgba(t.search_current).into()),
            color: Some(rgba(t.background).into()),
            ..Default::default()
        }),
        CellKind::Region => Some(HighlightStyle { background_color: Some(rgba(t.region).into()), ..Default::default() }),
        CellKind::Cursor => Some(HighlightStyle {
            background_color: Some(rgba(t.cursor).into()),
            color: Some(rgba(t.background).into()),
            ..Default::default()
        }),
        CellKind::Cursor2 => Some(HighlightStyle {
            background_color: Some(rgba(t.secondary_cursor).into()),
            color: Some(rgba(t.background).into()),
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Spacing utilities are in rems: scale them with the font as well.
        window.set_rem_size(px(DESIGN_REM * self.ui_scale()));
        let t0 = Instant::now();
        if let Some(start) = self.key_started.take() {
            self.last_latency_us = start.elapsed().as_micros();
        }

        let (view_id, view_name, lighters, total, cur_line, cur_col, cursor_count, region_len, modified, previewing) = {
            let v = self.ed.display_view();
            let r = &v.buf.rope;
            let p = v.cursors.iter().find(|c| c.id == v.primary).map_or(0, |c| c.point).min(r.len_chars());
            let line = r.char_to_line(p);
            let region_len: usize = v.cursors.iter().filter_map(|c| c.region()).map(|r| r.len()).sum();
            (
                v.id,
                v.buf.name.clone(),
                self.ed.mode_lighters(v.buf),
                r.len_lines(),
                line,
                p - r.line_to_char(line),
                v.cursors.len(),
                region_len,
                v.buf.modified(),
                v.id != self.ed.buf_id,
            )
        };
        let t = self.theme().clone();
        let fonts = self.appearance.fonts.clone();
        let lh = fonts.line_height;
        let gutter_w = px(((total.to_string().len().max(3) + 2) as f32) * fonts.size * 0.62);
        let rows_us = self.rows_us.clone();
        self.panel_at_layout = self.ed.swiper.is_some();
        let scroll = self.scroll_for(view_id);

        let list = uniform_list(
            "buffer",
            total,
            cx.processor(move |this: &mut EmaxView, range: Range<usize>, _w, _cx| {
                let started = Instant::now();
                let t = this.appearance.themes.get(this.dark);
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
                            .h(px(lh))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .when(is_cur, |d| d.bg(rgba(t.current_line)))
                            .child(
                                div()
                                    .w(gutter_w)
                                    .flex_none()
                                    .pr_3()
                                    .flex()
                                    .justify_end()
                                    .text_color(rgba(if is_cur { t.foreground } else { t.dim }))
                                    .child(format!("{}", ix + 1)),
                            )
                            .child({
                                let hl = search.map(|s| s.highlights(ix)).unwrap_or_default();
                                render_line(&v, ix, this.ed.tab_width_of(v.buf), true, &hl, selected_match == Some(ix), t)
                            })
                    })
                    .collect();
                rows_us.set(started.elapsed().as_micros());
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
            .bg(rgba(if previewing { t.modeline_preview } else { t.modeline }))
            .text_color(rgba(t.foreground))
            .child(div().text_color(rgba(t.accent)).child(if modified { "●" } else { "○" }))
            .child(div().font_weight(FontWeight::BOLD).child(view_name))
            .child(div().text_color(rgba(t.dim)).child(format!("({lighters})")))
            .when(previewing, |d| d.child(div().text_color(rgba(t.accent)).child("preview")))
            .child(format!("L{} C{}", cur_line + 1, cur_col))
            .when(cursor_count > 1, |d| {
                d.child(div().text_color(rgba(t.secondary_cursor)).child(format!("{cursor_count} cursors")))
            })
            .when(region_len > 0, |d| d.child(div().text_color(rgba(t.cursor)).child(format!("{region_len} selected"))))
            .child(format!("{total} lines"))
            .when(self.ed.buffer_count() > 1, |d| {
                d.child(div().text_color(rgba(t.dim)).child(format!("{} buffers", self.ed.buffer_count())))
            })
            .child(div().flex_1())
            .child(div().text_color(rgba(t.dim)).child(format!(
                "key→view {} µs · rows {} µs · view {} µs",
                self.last_latency_us,
                self.rows_us.get(),
                t0.elapsed().as_micros()
            )));

        let echo = div()
            .h(px(lh) + self.ui(6.0))
            .px_3()
            .flex()
            .items_center()
            .text_color(rgba(t.foreground))
            .child(self.echo.clone().unwrap_or_default());

        // A terminal buffer shows its screen instead of the text list.
        let body = match self.ed.terminal_snapshot(view_id, &Palette::from_theme(&t)) {
            Some(_) => {
                let chrome = 2.0 * lh + f32::from(self.ui(6.0)) + f32::from(self.ui(8.0));
                let size = window.viewport_size();
                let (cols, rows) = term_view::grid_size(
                    f32::from(size.width),
                    f32::from(size.height) - chrome,
                    term_view::cell_width(fonts.size),
                    lh,
                );
                self.ed.resize_terminal(view_id, cols, rows);
                let snap = self.ed.terminal_snapshot(view_id, &Palette::from_theme(&t));
                div().flex_1().min_h_0().children(snap.map(|s| term_view::render_terminal(&s, lh))).into_any_element()
            }
            None => list.into_any_element(),
        };

        let picker = self.ed.picker.as_ref().map(|p| self.render_picker(p, cx));
        let search = self.ed.swiper.as_ref().map(|s| self.render_swiper(s, gutter_w, cx));

        div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgba(t.background))
            .text_color(rgba(t.foreground))
            .font_family(fonts.family)
            .text_size(px(fonts.size))
            .line_height(px(lh))
            .child(body)
            .children(search)
            .child(modeline)
            .child(echo)
            .children(picker)
    }
}
