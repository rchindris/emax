//! ymacs — a fast, modern Emacs-style editor (prototype).

mod ui;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use gpui::{App, Application, Bounds, Focusable, WindowBounds, WindowOptions, prelude::*, px, size};

use ymacs::engine::{Buffer, Config, Editor, config, session};

const SCRATCH: &str = ";; This buffer is for text that is not saved.
;; Visit a file with C-x C-f
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--print-default-config") {
        print!("{}", config::DEFAULT_JSON);
        return;
    }
    // The session daemon that keeps terminal processes alive across editor runs.
    if args.first().map(String::as_str) == Some("--pty-daemon") {
        std::process::exit(session::daemon::main(&args[1..]));
    }
    let paths: Vec<PathBuf> = args.iter().map(PathBuf::from).collect();
    let t_open = Instant::now();
    let config = Config::load();
    for w in &config.warnings {
        eprintln!("ymacs: config: {w}");
    }
    let mut ed = Editor::new(Buffer::from_str("*scratch*", SCRATCH));
    // Like Emacs, there is always a *scratch* buffer; files keep command-line
    // order (the first is current) and directories are listed.
    for p in paths.iter().rev() {
        let r = if p.is_dir() { ed.dired(p) } else { ed.find_file(p) };
        if let Err(e) = r {
            eprintln!("ymacs: {e}");
            std::process::exit(1)
        }
    }
    ed.set_modes(Arc::new(config.modes));
    ed.set_terminal_config(Arc::new(config.terminal));
    let session_file = session::state_dir(config.session.state_dir.as_deref()).map(|d| session::session_file_path(&d));
    ed.session_config = config.session;
    let lines: usize = ed.rope().len_lines();
    ed.message = Some(match config.warnings.as_slice() {
        [] => format!(
            "{} · {} lines · {} buffer{} · loaded in {} ms",
            ed.buf.name,
            lines,
            ed.buffer_count(),
            if ed.buffer_count() == 1 { "" } else { "s" },
            t_open.elapsed().as_millis()
        ),
        [w] => format!("Config problem (entry skipped): {w}"),
        [w, rest @ ..] => format!("Config problem (entry skipped): {w} — and {} more, see stderr", rest.len()),
    });
    // The last session comes back when no files were named, and is saved on quit.
    if let Some(e) = ed.startup_session(session_file, !paths.is_empty(), !config.warnings.is_empty()) {
        eprintln!("ymacs: session: {e}");
    }
    let keymaps = config.keymaps;
    let appearance = config.appearance;
    ed.themes = appearance.available.iter().map(|t| (t.name.clone(), t.is_dark())).collect();

    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1100.0), px(760.0)), cx);
        let window = cx
            .open_window(
                WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
                |window, cx| cx.new(|cx| ui::YmacsView::new(ed, keymaps, appearance, window, cx)),
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
