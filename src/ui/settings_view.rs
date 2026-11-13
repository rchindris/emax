//! The settings editor's panel, laid out like VS Code's settings UI: a title
//! bar with a button that opens the settings file, a search field, the
//! "User" scope tab with the file's path, a table of contents of sections on
//! the left and the settings on the right (each with its title, description
//! and control; a bar marks the ones the user file sets), and a footer with
//! the keys. Also reloads the configuration after the editor writes it.

use std::sync::Arc;

use gpui::{AnyElement, Context, FontWeight, SharedString, div, prelude::*, px, rgba};

use ymacs::engine::settings::{Edit, Kind, MODIFIED_FILTER, Row, SETTINGS, Setting, SettingsEditor};
use ymacs::engine::{Config, Theme, config};

use super::{YmacsView, hints, query_with_caret};

impl YmacsView {
    /// After the settings editor handled a key or a click: apply what it
    /// wrote, update the echo area and, for keys, bring the selection on screen.
    pub(super) fn after_settings(&mut self, reveal: bool, cx: &mut Context<Self>) {
        let changed = self.ed.settings.as_mut().is_some_and(|s| std::mem::take(&mut s.changed));
        if changed {
            let path = self.ed.settings.as_ref().and_then(|s| s.path.clone());
            let problem = self.reload_config(path);
            if let Some(s) = self.ed.settings.as_mut()
                && problem.is_some()
            {
                s.message = problem;
            }
        }
        match &self.ed.settings {
            Some(s) => {
                self.echo = None;
                if reveal {
                    let rows = s.rows();
                    if let Some(ix) = rows.iter().position(|r| *r == Row::Setting(s.selected)) {
                        // The first setting of a section brings its header along.
                        let first = ix > 0 && matches!(rows[ix - 1], Row::Header(_));
                        self.settings_scroll.scroll_to_item(if first { ix - 1 } else { ix });
                    }
                }
            }
            None => {
                // Closed, or the settings file was opened in a buffer.
                self.echo = self.ed.message.take().map(Into::into);
                self.apply_scroll();
            }
        }
        cx.notify();
    }

    /// Read the configuration again (the built-in one plus the file at
    /// `path`) and use it: modes and settings, keymaps, terminal and session
    /// settings, fonts and theme. Buffers keep their modes. Returns the first
    /// problem found in the configuration, if any.
    pub(super) fn reload_config(&mut self, path: Option<std::path::PathBuf>) -> Option<String> {
        let c = Config::load_from(path);
        self.ed.reload_modes(Arc::new(c.modes));
        self.ed.set_terminal_config(Arc::new(c.terminal));
        self.ed.session_config = c.session;
        self.picker_keys = c.keymaps.picker;
        self.search_keys = c.keymaps.search;
        self.settings_keys = c.keymaps.settings;
        // Scroll positions are in pixels: with another line height they point elsewhere.
        if c.appearance.fonts.line_height != self.appearance.fonts.line_height {
            self.scrolls.clear();
        }
        self.appearance = c.appearance;
        match c.warnings.as_slice() {
            [] => None,
            [w] => Some(format!("Config problem (entry skipped): {w}")),
            [w, rest @ ..] => Some(format!("Config problem (entry skipped): {w} — and {} more", rest.len())),
        }
    }

    /// After a buffer was saved: if it is the settings file, apply it (like
    /// saving settings.json in VS Code).
    pub(super) fn after_save(&mut self) {
        let Some(user) = config::user_config_path() else { return };
        let same = |p: &std::path::Path| match (p.canonicalize(), user.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => p == user,
        };
        if self.ed.buf.path.as_deref().is_some_and(same) && !self.ed.buf.modified() {
            let problem = self.reload_config(Some(user));
            self.ed.message = Some(problem.unwrap_or_else(|| "Settings applied".into()));
            self.echo = self.ed.message.clone().map(Into::into);
        }
    }

    /// Run `f` on the settings editor (a click), then update the view.
    fn settings_click(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut SettingsEditor)) {
        if let Some(s) = self.ed.settings.as_mut() {
            f(s);
        }
        self.after_settings(false, cx);
    }

    pub(super) fn render_settings(&self, s: &SettingsEditor, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme();
        let title_bar = div()
            .flex()
            .items_center()
            .gap_2()
            .px_5()
            .pt_4()
            .pb_2()
            .child(div().text_size(self.ui(20.0)).text_color(rgba(t.emphasis)).child("Settings"))
            .child(div().flex_1())
            .child(self.button("open-json", "{ }  Open settings.json", t).on_click(cx.listener(|this, _, _, cx| {
                this.ed.settings_action("settings-open-json");
                this.after_settings(false, cx);
            })))
            .child(self.button("close-settings", "✕", t).on_click(cx.listener(|this, _, _, cx| {
                this.ed.settings_action("settings-close");
                this.after_settings(false, cx);
            })));

        let searching = s.edit.is_none();
        let query: SharedString = s.query.clone().into();
        let found = if s.query.trim().is_empty() {
            String::new()
        } else {
            match s.matches.len() {
                0 => "No settings found".into(),
                1 => "1 setting found".into(),
                n => format!("{n} settings found"),
            }
        };
        let search = div()
            .mx_5()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .h(self.ui(34.0))
            .rounded_md()
            .bg(rgba(t.background))
            .border_1()
            .border_color(rgba(if searching { t.cursor } else { t.panel_border }))
            .text_size(self.ui(14.0))
            .child(div().text_color(rgba(t.dim)).child("⌕"))
            .child(if searching {
                query_with_caret(&query, s.query_tail, self.ui(17.0), t).into_any_element()
            } else {
                div().text_color(rgba(t.foreground)).child(query.clone()).into_any_element()
            })
            .when(query.is_empty(), |d| {
                d.child(div().text_color(rgba(t.dim)).child(format!("Search settings  (try {MODIFIED_FILTER})")))
            })
            .child(div().flex_1())
            .child(div().text_size(self.ui(12.0)).text_color(rgba(t.dim)).child(found));

        let scope = div()
            .mx_5()
            .mt_3()
            .flex()
            .items_end()
            .gap_3()
            .border_b_1()
            .border_color(rgba(t.panel_border))
            .child(
                div()
                    .pb_1p5()
                    .border_b_2()
                    .border_color(rgba(t.accent))
                    .text_color(rgba(t.emphasis))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("User"),
            )
            .child(div().pb_1p5().text_size(self.ui(12.0)).text_color(rgba(t.dim)).child(s.path_label()))
            .when_some(s.file_error.clone(), |d, e| {
                d.child(div().pb_1p5().text_size(self.ui(12.0)).text_color(rgba(t.term_red)).child(e))
            });

        let body = div()
            .flex_1()
            .min_h_0()
            .flex()
            .child(self.render_settings_toc(s, cx))
            .child(self.render_settings_list(s, cx));

        let footer = div()
            .flex()
            .gap_4()
            .px_5()
            .py_2()
            .border_t_1()
            .border_color(rgba(t.panel_border))
            .text_size(self.ui(12.0))
            .text_color(rgba(t.dim))
            .child(div().flex_1().child(self.settings_hints(s)))
            .when_some(s.message.clone(), |d, m| d.child(div().text_color(rgba(t.secondary_cursor)).child(m)));

        let panel = div()
            .id("settings")
            .occlude()
            .size_full()
            .max_w(self.ui(1040.0))
            .flex()
            .flex_col()
            .bg(rgba(t.panel))
            .border_1()
            .border_color(rgba(t.panel_border))
            .rounded_xl()
            .shadow_2xl()
            .overflow_hidden()
            .font_family(self.appearance.fonts.ui_family.clone())
            .text_size(self.ui(13.5))
            .line_height(self.ui(20.0))
            .child(title_bar)
            .child(search)
            .child(scope)
            .child(body)
            .child(footer);

        div()
            .absolute()
            .inset_0()
            .bg(rgba(t.overlay))
            .flex()
            .justify_center()
            .p(self.ui(36.0))
            .child(panel)
            .into_any_element()
    }

    /// The table of contents: each section with its number of matches.
    fn render_settings_toc(&self, s: &SettingsEditor, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme();
        let current = s.selected_section();
        let items = s.section_counts().into_iter().map(|(sec, n)| {
            let cur = Some(sec) == current;
            div()
                .id(SharedString::from(format!("toc-{sec}")))
                .flex()
                .items_center()
                .px_3()
                .py_1()
                .rounded_md()
                .when(n > 0, |d| d.cursor_pointer().hover(|st| st.bg(rgba(t.row_hover))))
                .when(cur, |d| {
                    d.bg(rgba(t.row_selected)).text_color(rgba(t.emphasis)).font_weight(FontWeight::SEMIBOLD)
                })
                .when(!cur, |d| d.text_color(rgba(if n > 0 { t.foreground } else { t.dim })))
                .on_click(cx.listener(move |this, _, _, cx| this.settings_click(cx, |s| s.select_section(sec))))
                .child(div().flex_1().child(sec))
                .when(!s.query.is_empty(), |d| {
                    d.child(div().text_size(self.ui(11.5)).text_color(rgba(t.dim)).child(format!("{n}")))
                })
        });
        div()
            .flex_none()
            .w(self.ui(200.0))
            .p_3()
            .flex()
            .flex_col()
            .gap_0p5()
            .border_r_1()
            .border_color(rgba(t.panel_border))
            .children(items)
            .into_any_element()
    }

    /// The settings (and section headers), scrolled to keep the selection visible.
    fn render_settings_list(&self, s: &SettingsEditor, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme();
        let rows: Vec<AnyElement> = s
            .rows()
            .into_iter()
            .map(|row| match row {
                Row::Header(sec) => div()
                    .px_4()
                    .pt_4()
                    .pb_1()
                    .text_size(self.ui(17.0))
                    .text_color(rgba(t.emphasis))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(sec)
                    .into_any_element(),
                Row::Setting(m) => self.render_setting(s, m, cx),
            })
            .collect();
        let empty = rows.is_empty();
        div()
            .id("settings-list")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .track_scroll(&self.settings_scroll)
            .px_3()
            .pb_4()
            .flex()
            .flex_col()
            .gap_1()
            .children(rows)
            .when(empty, |d| d.child(div().p_4().text_color(rgba(t.dim)).child("No settings found")))
            .into_any_element()
    }

    /// One setting: the modified bar, its title and reset button, its
    /// description and its control.
    fn render_setting(&self, s: &SettingsEditor, m: usize, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme();
        let st = &SETTINGS[s.matches[m]];
        let selected = m == s.selected;
        let modified = s.is_modified(st.key);
        let (group, name) = st.title.split_once(": ").map_or(("", st.title), |(g, n)| (g, n));
        let error = match &s.edit {
            Some(Edit::Text { error: Some(e), .. }) if selected => Some(e.clone()),
            _ => None,
        };
        let title = div()
            .flex()
            .items_center()
            .gap_1()
            .when(!group.is_empty(), |d| d.child(div().text_color(rgba(t.foreground)).child(format!("{group}:"))))
            .child(div().text_color(rgba(t.emphasis)).font_weight(FontWeight::SEMIBOLD).child(name))
            .when(modified, |d| {
                d.child(
                    div()
                        .ml_2()
                        .px_1p5()
                        .rounded_sm()
                        .bg(rgba(t.badge))
                        .text_size(self.ui(11.0))
                        .text_color(rgba(t.dim))
                        .child("Modified"),
                )
            })
            .child(div().flex_1())
            .when(modified, |d| {
                d.child(
                    self.button(("reset", m), "↺ Reset", t)
                        .text_size(self.ui(11.5))
                        .on_click(cx.listener(move |this, _, _, cx| this.settings_click(cx, |s| s.reset(m)))),
                )
            });

        div()
            .id(("setting", m))
            .flex()
            .gap_3()
            .px_3()
            .py_2p5()
            .rounded_md()
            .when(selected, |d| d.bg(rgba(t.row_selected)))
            .when(!selected, |d| d.hover(|st| st.bg(rgba(t.row_hover))))
            .on_click(cx.listener(move |this, _, _, cx| this.settings_click(cx, |s| s.select(m))))
            .child(div().flex_none().w(px(2.0)).rounded_sm().when(modified, |d| d.bg(rgba(t.accent))))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(title)
                    .child(div().text_size(self.ui(12.5)).text_color(rgba(t.dim)).child(st.doc))
                    .child(self.render_control(s, st, m, cx))
                    .when_some(error, |d, e| {
                        d.child(div().text_size(self.ui(12.0)).text_color(rgba(t.term_red)).child(e))
                    }),
            )
            .into_any_element()
    }

    /// The control that edits a setting: a checkbox, a dropdown or a text field.
    fn render_control(&self, s: &SettingsEditor, st: &Setting, m: usize, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme();
        let selected = m == s.selected;
        let value = s.display(st);
        let field_h = self.ui(28.0);

        if st.kind == Kind::Bool {
            let on = value == "true";
            return div()
                .id(("check", m))
                .flex()
                .items_center()
                .gap_2()
                .mt_1()
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| this.settings_click(cx, |s| s.set_bool(m, !on))))
                .child(
                    div()
                        .size(self.ui(17.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_sm()
                        .border_1()
                        .border_color(rgba(if on { t.accent } else { t.panel_border }))
                        .bg(rgba(if on { t.accent } else { t.background }))
                        .text_size(self.ui(12.0))
                        .text_color(rgba(t.background))
                        .when(on, |d| d.child("✓")),
                )
                .child(div().text_color(rgba(t.foreground)).child(if on { "On" } else { "Off" }))
                .into_any_element();
        }

        if st.kind.is_choice() {
            let open = match &s.edit {
                Some(Edit::Choice { options, index }) if selected => Some((options.clone(), *index)),
                _ => None,
            };
            let boxed = div()
                .id(("choice", m))
                .w(self.ui(280.0))
                .h(field_h)
                .mt_1()
                .px_2()
                .flex()
                .items_center()
                .rounded_md()
                .bg(rgba(t.background))
                .border_1()
                .border_color(rgba(if open.is_some() { t.cursor } else { t.panel_border }))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.settings_click(cx, |s| {
                        s.select(m);
                        if s.edit.is_some() {
                            s.edit = None;
                        } else {
                            s.activate();
                        }
                    })
                }))
                .child(div().flex_1().overflow_hidden().whitespace_nowrap().text_ellipsis().child(value.clone()))
                .child(div().text_color(rgba(t.dim)).child("▾"));
            let list = open.map(|(options, index)| {
                div()
                    .w(self.ui(280.0))
                    .p_1()
                    .flex()
                    .flex_col()
                    .rounded_md()
                    .bg(rgba(t.background))
                    .border_1()
                    .border_color(rgba(t.panel_border))
                    .shadow_lg()
                    .children(options.into_iter().enumerate().map(|(i, o)| {
                        let hi = i == index;
                        div()
                            .id(("option", i))
                            .px_2()
                            .py_0p5()
                            .rounded_sm()
                            .cursor_pointer()
                            .when(hi, |d| d.bg(rgba(t.row_selected)).text_color(rgba(t.emphasis)))
                            .when(!hi, |d| d.hover(|st| st.bg(rgba(t.row_hover))))
                            .on_click(cx.listener(move |this, _, _, cx| this.settings_click(cx, |s| s.choose(i))))
                            .child(if o == value { format!("{o}  ✓") } else { o })
                    }))
            });
            return div().flex().flex_col().gap_1().child(boxed).children(list).into_any_element();
        }

        let editing = match &s.edit {
            Some(Edit::Text { input, .. }) if selected => Some(input.clone()),
            _ => None,
        };
        let numeric = matches!(st.kind, Kind::Int { .. } | Kind::Float { .. });
        let placeholder = match st.kind {
            Kind::Text { placeholder, .. } => placeholder.to_string(),
            Kind::List => "none".to_string(),
            _ => s.default_display(st),
        };
        let content = match &editing {
            Some(input) => query_with_caret(input, s.input_tail, self.ui(16.0), t).into_any_element(),
            None if value.is_empty() => div().text_color(rgba(t.dim)).child(placeholder).into_any_element(),
            None => div()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_color(rgba(t.foreground))
                .child(value)
                .into_any_element(),
        };
        div()
            .id(("field", m))
            .w(self.ui(if numeric { 140.0 } else { 420.0 }))
            .max_w_full()
            .h(field_h)
            .mt_1()
            .px_2()
            .flex()
            .items_center()
            .overflow_hidden()
            .rounded_md()
            .bg(rgba(t.background))
            .border_1()
            .border_color(rgba(if editing.is_some() { t.cursor } else { t.panel_border }))
            .font_family(self.appearance.fonts.family.clone())
            .text_size(self.ui(13.0))
            .cursor_text()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.settings_click(cx, |s| {
                    let already = s.selected == m && s.edit.is_some();
                    s.select(m);
                    if !already {
                        s.activate();
                    }
                })
            }))
            .child(content)
            .into_any_element()
    }

    /// A small bordered button.
    fn button(&self, id: impl Into<gpui::ElementId>, label: &str, t: &Theme) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .flex_none()
            .px_2()
            .py_0p5()
            .rounded_md()
            .border_1()
            .border_color(rgba(t.panel_border))
            .text_size(self.ui(12.5))
            .text_color(rgba(t.foreground))
            .cursor_pointer()
            .hover(|st| st.bg(rgba(t.row_hover)))
            .child(label.to_string())
    }

    /// The footer's key hints for what the keys do right now.
    fn settings_hints(&self, s: &SettingsEditor) -> String {
        let accept = match s.selected_setting().map(|st| st.kind) {
            Some(Kind::Bool) => "toggle",
            Some(k) if k.is_choice() => "choose",
            Some(_) => "edit",
            None => "",
        };
        let modified = s.selected_setting().is_some_and(|st| s.is_modified(st.key));
        let mut items: Vec<(&str, &str)> = match &s.edit {
            Some(Edit::Text { .. }) => {
                vec![("settings-accept", "save"), ("settings-clear", "clear"), ("settings-quit", "cancel")]
            }
            Some(Edit::Choice { .. }) => vec![
                ("settings-accept", "choose"),
                ("settings-next", "next"),
                ("settings-previous", "prev"),
                ("settings-quit", "cancel"),
            ],
            None => vec![
                ("settings-accept", accept),
                ("settings-next", "next"),
                ("settings-previous", "prev"),
                ("settings-next-section", "section"),
                ("settings-reset", if modified { "reset" } else { "" }),
                ("settings-open-json", "edit JSON"),
                ("settings-quit", if s.query.is_empty() { "close" } else { "clear" }),
            ],
        };
        items.retain(|(_, l)| !l.is_empty());
        hints(&self.settings_keys, &items)
    }
}
