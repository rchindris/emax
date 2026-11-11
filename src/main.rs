//! emax — a fast, modern Emacs-style editor (prototype).

mod ui;

use std::path::PathBuf;
use std::time::Instant;

use gpui::{App, Application, Bounds, Focusable, WindowBounds, WindowOptions, prelude::*, px, size};

use emax::engine::{Buffer, Editor};

const SCRATCH: &str = ";; This buffer is for text that is not saved.
;; emax — GPU-rendered, rope-backed, Emacs keys. Option is Meta, Cmd is Super.
;;
;; Motion     C-f C-b M-f M-b C-n C-p C-a C-e M-m M-a M-e M-{ M-}
;;            C-M-f C-M-b C-M-u C-M-d  M-< M->  C-v M-v  C-l
;; Mark       C-SPC (or C-@)  C-x C-x  C-x h  M-@  M-h  C-M-SPC
;;            Shift+motion selects.  C-u C-SPC jumps to the previous mark.
;; Kill/yank  C-k C-w M-w M-d M-DEL M-k C-M-k M-z  C-y M-y  (system clipboard too)
;; Delete     C-d DEL M-\\ M-SPC M-^ C-x C-o
;; Edit       C-t M-t C-x C-t C-M-t  M-u M-l M-c  C-x C-u C-x C-l  M-q  C-o C-q
;; Search     C-s C-r  (swiper: terms in any order; C-s/C-r next/prev, RET jump, C-g back)
;; Undo       C-/ (undo)  C-? (redo)  — also Cmd-Z / Cmd-Shift-Z
;; Cursors    C->  C-<  next/previous like this (or next/previous line)
;;            C-c C-<  all like this   C-S-c C-S-c  one per line   C-g  back to one
;; Args       C-u, C-u C-u, C-u 12, M-5, M--
;; Buffers    C-x b  switch (fuzzy, live preview; C-j create, C-k kill)
;;            C-x C-b  same   C-x <left>/<right> cycle   C-x k kill
;; Files      C-x C-s save   C-x C-c quit
;;
;; Run `emax <file>...` to open files.

Paragraphs are separated by blank lines. Sentences end with a period,
question mark or exclamation point.  Try M-e, M-a, M-} and M-q here.

(defun example (x)
  (let ((y (* x 2)))
    (list x y \"a string (with parens)\")))
";

fn main() {
    let paths: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    let t_open = Instant::now();
    let open = |p: &PathBuf| {
        Buffer::open(p.clone()).unwrap_or_else(|e| {
            eprintln!("emax: {}: {e}", p.display());
            std::process::exit(1)
        })
    };
    let mut ed = match paths.first() {
        Some(p) => Editor::new(open(p)),
        None => Editor::new(Buffer::from_str("*scratch*", SCRATCH)),
    };
    if !paths.is_empty() {
        // Like Emacs, there is always a *scratch* buffer; files keep command-line order.
        ed.add_buffer(Buffer::from_str("*scratch*", SCRATCH));
        for p in paths[1..].iter().rev() {
            ed.add_buffer(open(p));
        }
    }
    let lines: usize = ed.rope().len_lines();
    ed.message = Some(format!(
        "{} · {} lines · {} buffer{} · loaded in {} ms",
        ed.buf.name,
        lines,
        ed.buffer_count(),
        if ed.buffer_count() == 1 { "" } else { "s" },
        t_open.elapsed().as_millis()
    ));

    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1100.0), px(760.0)), cx);
        let window = cx
            .open_window(
                WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
                |_, cx| cx.new(|cx| ui::EmaxView::new(ed, cx)),
            )
            .unwrap();
        window
            .update(cx, |view, window, cx| {
                window.focus(&view.focus_handle(cx));
                cx.activate(true);
            })
            .unwrap();
    });
}
