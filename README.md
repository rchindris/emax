# ymacs

A fast, modern Emacs-style text editor written in Rust.

ymacs keeps Emacs' editing model and key bindings, rendered on the GPU with
[GPUI](https://www.gpui.rs/) and backed by a [ropey](https://github.com/cessen/ropey)
rope. It is **not** an Emacs Lisp clone: the goal is the Emacs editing experience,
without the Emacs runtime.

> **Status:** early prototype. Expect missing features and breaking changes.

## Features

- **GPU-rendered UI.** Lines are virtualised, so large files stay responsive.
  The window has a mode line and an echo area.
- **Rope-backed buffers** with grouped undo/redo.
- **Syntax highlighting** with tree-sitter: Rust, Python, shell, JSON, TOML,
  YAML, Markdown, Scheme and Emacs Lisp. Code embedded in Markdown (fenced
  blocks, inline formatting) is highlighted in its own language. Edits
  re-parse incrementally, and only the lines on screen are highlighted.
- **Emacs key bindings.** Prefix keys (`C-x …`), prefix arguments (`C-u`,
  `C-u 12`, `M-5`, `M--`), shift-selection and transient mark.
- **Structural motion** by word, sentence, paragraph and balanced expression (sexp).
- **Kill ring** with `yank-pop`, connected to the system clipboard.
- **Multiple cursors**, using the bindings from `multiple-cursors.el`. Every
  command runs at every cursor.
- **Swiper-style search** (`C-s`): every matching line is listed in a panel and
  previewed in the buffer as you move through it. Space-separated terms match
  in any order, with smart case.
- **Multiple buffers** with a fuzzy-filtering switcher (`C-x b`).
- **Dired** (`C-x d`): a directory listing to visit, mark, flag, delete,
  rename, copy and create files and directories, with Emacs' keys.
- **Major and minor modes.** Every buffer has one major mode (`rust-mode`,
  `markdown-mode`, …), chosen from a `-*- mode -*-` marker, the `#!` line or
  the file name, plus any number of minor modes (`auto-fill-mode`,
  `read-only-mode`, `overwrite-mode`, `electric-pair-mode`). Major modes are
  plain data in the configuration, so you can add your own.
- **Commands use their GNU Emacs names** (`forward-sexp`, `kill-ring-save`, …).
  `M-x` runs any of them by name, with fuzzy matching.

## Installing

On macOS (Apple Silicon or Intel) with Homebrew:

```sh
brew install rchindris/ymacs/ymacs
```

On Debian and Ubuntu, download the `.deb` for your architecture from the
[latest release](https://github.com/rchindris/ymacs/releases/latest) and run
`sudo apt install ./ymacs_*.deb`.

## Building and running

ymacs needs a recent stable Rust toolchain (Rust 1.88 or newer).

```sh
cargo run --release               # opens an empty *scratch* buffer
cargo run --release -- path/file  # opens a file
```

Dependencies are optimised even in debug builds, so plain `cargo run` is also
fast enough for daily use while you work on the editor.

Run the tests with:

```sh
cargo test
```

## Keys

Option is **Meta** (`M-`) and Cmd is **Super** (`s-`). The common macOS
shortcuts (`s-c`, `s-v`, `s-x`, `s-z`, `s-s`, `s-a`, `s-q`) also work.

| Area | Keys |
| --- | --- |
| Motion | `C-f` `C-b` `M-f` `M-b` `C-n` `C-p` `C-a` `C-e` `M-m` `M-a` `M-e` `M-{` `M-}` |
| Sexps | `C-M-f` `C-M-b` `C-M-u` `C-M-d` |
| Buffer and screen | `M-<` `M->` `C-v` `M-v` `C-l` `M-g g` |
| Mark | `C-SPC` `C-x C-x` `C-x h` `M-@` `M-h` `C-M-SPC`; `C-u C-SPC` jumps to the previous mark |
| Kill and yank | `C-k` `C-w` `M-w` `M-d` `M-DEL` `M-k` `C-M-k` `M-z` `C-y` `M-y` |
| Delete | `C-d` `DEL` `M-\` `M-SPC` `M-^` `C-x C-o` |
| Edit | `C-t` `M-t` `C-x C-t` `C-M-t` `M-u` `M-l` `M-c` `C-x C-u` `C-x C-l` `M-q` `C-o` `C-q` |
| Undo and redo | `C-/` (undo), `C-?` (redo) |
| Multiple cursors | `C->` `C-<` (next/previous like this), `C-c C-<` (all like this), `C-S-c C-S-c` (one per line), `C-g` (back to one cursor) |
| Search | `C-s` (`s-f`) search, `C-r` search backward; inside: `C-s`/`C-r` next/previous, `RET` jump, `C-g` cancel, `M-n` symbol at point, `C-s C-s` repeat the last search |
| Buffers | `C-x b` `C-x C-b` `C-x <left>` `C-x <right>` `C-x k` |
| Files | `C-x C-f` (open: type a path, fuzzy completion), `C-x C-s` (save; asks for a file name if the buffer has none), `C-x C-c` (quit) |
| Dired | `C-x d` (list a directory), `C-x C-j` (list the current file's directory); inside: `RET` visit, `o` in other window, `^` up, `n`/`p` move, `m` mark, `u`/`DEL` unmark, `U` unmark all, `t` toggle marks, `d` flag, `x` delete flagged, `D` delete, `R` rename/move, `C` copy, `+` new directory, `g` refresh, `s` sort by name/date, `w` copy names, `q` bury. Deleting asks you to repeat the key to confirm. |
| Commands | `M-x` (run a command by name; `C-u M-x` passes the prefix argument on) |
| Settings | `s-,` (Cmd-,) opens the settings editor; `M-x open-init-file` visits your `init.scm` |
| Modes | `C-x C-q` (read-only), `<insert>` (overwrite); `M-x rust-mode` etc. switch the major mode |

These are the default bindings. The full set is in
[`config/default.scm`](config/default.scm), and every key can be changed (see
[Configuration](#configuration)).

## Configuration

ymacs is configured in [Steel](https://github.com/mattwparas/steel), a Scheme.
The defaults are in [`config/default.scm`](config/default.scm), which is built
into the binary (`ymacs --print-default-config` prints it). Two files of yours
are loaded after it, if they exist:

1. `init.scm`, which you write, from the first of these that is set:
   `$YMACS_CONFIG`, `$XDG_CONFIG_HOME/ymacs/init.scm`, on Windows
   `%APPDATA%\ymacs\init.scm`, else `~/.config/ymacs/init.scm`;
2. `settings.scm` in the same directory, which the settings editor writes.

The easiest way to change settings is the settings editor, `s-,` (Cmd-,), which
works like VS Code's: search settings by name or description (`@modified` lists
the ones you have changed), then `RET` to edit, toggle or choose a value, `C-k`
to reset one, `TAB` to jump between sections, and `C-o` to open `init.scm`.
Changes are written to `settings.scm` at once and apply right away. Because
it's loaded last, what you set there wins over `init.scm`; resetting a setting
goes back to the value `init.scm` (or the default) gives it. Key bindings, modes
and theme colors are set in `init.scm`, and saving that file applies it too.

Your `init.scm` only needs what you want to change. It's a Scheme program, so
you can define helpers, use `if`, and so on:

```scheme
(setq fill-column 80
      tab-width 2)
(global-set-key "C-z" 'undo)
(global-unset-key "C-x C-c")
(define-key 'search "C-j" 'search-accept)
(define-keys 'rust-mode
  "C-c C-f" 'fill-paragraph
  "C-c C-k" #f)
```

| Form | Does |
| --- | --- |
| `(setq option value ...)` | Set options (below); `(set-option! 'option value)` is the same as a function |
| `(define-keys 'keymap "keys" 'command ...)` | Bind keys in a keymap. A binding replaces any for the same keys, and `#f` instead of a command removes it |
| `(define-key 'keymap "keys" 'command)` | Bind one key sequence |
| `(global-set-key "keys" 'command)`, `(global-unset-key "keys")` | The same for the `global` keymap |
| `(define-major-mode 'name-mode #:property value ...)` | Define or change a major mode; see [Modes](#modes) |
| `(define-minor-mode 'name-mode #:lighter "Name")` | Change a minor mode's mode-line name |
| `(define-theme 'name #:color "#rrggbb" ...)` | Define or change a theme; see [Themes](#themes) |

| Keymap | What it holds |
| --- | --- |
| `global` | Editing keys; multi-key sequences like `"C-x C-s"` work. Values are command names (`forward-char`, `swiper`, …) |
| `picker` | Keys while a picker is open; values are `picker-*` actions (`TAB` is `picker-complete`). Keys left unbound edit the query like the global ones (`C-a`, `C-d`, `M-f`, `C-y`…) |
| `search` | Keys while the search panel is open; values are `search-*` actions |
| `settings` | Keys in the settings editor; values are `settings-*` actions |
| `terminal` | Keys in terminal buffers, looked up before the others |
| a mode's name | Keys in buffers with that major or minor mode on |

| Option | Meaning |
| --- | --- |
| `fill-column` | Column for `fill-paragraph` (default 70) |
| `tab-width` | Display width of a tab and the indent step (default 4) |
| `default-major-mode`, `initial-major-mode` | See [Modes](#modes) |
| `font-family` | Font for buffer text (default `"monospace"`: Menlo on macOS, Consolas on Windows, DejaVu Sans Mono on Linux) |
| `font-ui-family` | Font for the buffer picker and panel hints (default `"system-ui"`, the platform's UI font) |
| `font-size` | Buffer text size in pixels (default 14). The mode line and echo area use it too |
| `font-ui-size` | Size the buffer picker, `M-x`, the search panel and the tab bar are drawn at, in pixels. Their text, spacing and rows scale with it (default `#f`: same as `font-size`) |
| `line-height` | Line height as a multiple of the size (default 1.5) |
| `theme` | The theme to use (default `'ymacs-dark`), or `'((light . name) (dark . name))` to follow the system appearance. `M-x load-theme` switches while running |
| `terminal-shell`, `terminal-shell-args`, `terminal-term`, … | Terminal buffers; `ymacs --print-default-config` lists them all |
| `restore-session`, `session-state-dir` | Saving and restoring tabs, windows and buffers |

Keys use Emacs notation: `C-` Control, `M-` Meta (Option), `S-` Shift, `s-`
Super (Cmd), and names like `RET`, `TAB`, `DEL`, `ESC`, `SPC`, `<left>`,
`<next>`. Mistakes such as unknown options or commands and malformed keys don't
stop the editor: that entry is skipped, and the problem shows in the echo area
and on stderr. A Scheme error (say, an unclosed parenthesis) skips the rest of
its file, keeping what ran before it, as in Emacs.

### Modes

Each buffer has one **major mode** and any number of **minor modes**, shown in
the mode line as `(Rust Fill)`.

When a file is opened, its major mode is the first of these that applies:

1. a marker on the first line (or the second, after `#!`), as in Emacs:
   `-*- rust -*-` or `-*- mode: rust -*-`
2. the program on a `#!` line (`#!/usr/bin/env steel` gives `lisp-mode`)
3. the file name, matched against each mode's `files` patterns (the most
   specific pattern wins)
4. `default-major-mode` (`fundamental-mode`)

`*scratch*` starts in `initial-major-mode` (`text-mode`). The built-in major
modes are `fundamental-mode`, `text-mode`, `markdown-mode`, `prog-mode`,
`rust-mode`, `json-mode` and `lisp-mode`. Each mode is also a command:
`M-x markdown-mode` switches the buffer to it. Like in Emacs, switching drops
the buffer's minor modes and buffer-local settings (except `read-only-mode`),
then turns on the ones the new mode wants.

A major mode is defined like this; every property is optional:

```scheme
(define-major-mode 'go-mode
  #:lighter "Go"
  #:parent 'prog-mode
  #:files '("*.go")
  #:interpreters '("gorun")
  #:tab-width 8
  #:minor-modes '(auto-fill-mode))
(define-keys 'go-mode "C-c C-f" 'fill-paragraph)
```

| Property | Meaning |
| --- | --- |
| `#:lighter` | Mode-line name (default: from the mode's name, `go-mode` gives `Go`) |
| `#:parent` | Mode to inherit from: its settings, keymap and minor modes apply unless this mode overrides them (`#f` removes an inherited parent) |
| `#:files` | File name patterns with `*` and `?`; lowercase patterns ignore case |
| `#:interpreters` | Programs on a `#!` line that select the mode (trailing version numbers are ignored, so `python` matches `python3`) |
| `#:syntax` | Language to highlight with, inherited from the parent (`#f` turns it off). Built in: `rust`, `json`, `toml`, `yaml`, `bash`, `python`, `markdown`, `scheme`, `elisp` |
| `#:fill-column`, `#:tab-width` | Settings for buffers in this mode |
| `#:minor-modes` | Minor modes to turn on, after the parent's (Emacs' mode hooks) |

Its keymap, which applies over the parent's and the global one, is the keymap
with the mode's name. Defining a mode that already exists changes only the
properties you give: `(define-major-mode 'rust-mode #:fill-column 80)` changes
only rust-mode's fill column.

The **minor modes** are built in:

| Mode | What it does | Default lighter |
| --- | --- | --- |
| `auto-fill-mode` | Breaks the line at `fill-column` when you type a space or newline past it. On in `text-mode` | `Fill` |
| `read-only-mode` | Refuses every change (what a kill copies still goes to the kill ring). On for files you can't write | `RO` |
| `overwrite-mode` | Typed characters replace the ones after point, up to the end of the line | `Ovwrt` |
| `electric-pair-mode` | Typing `(`, `[`, `{` or `"` also inserts the closing half. Typing the closer steps over it, `DEL` in an empty pair removes both halves, and with a region the pair wraps it. On in `prog-mode` | none |

`define-minor-mode` sets a minor mode's `#:lighter`, and the keymap with its
name holds its keys. Running a
minor mode's command toggles it. With a prefix argument it turns the mode on
if the argument is positive (`C-u` counts) and off otherwise.

Keys are looked up in this order: the minor modes' keymaps (the most recently
enabled first), the major mode's keymap and then its parents', and finally
the `global` keymap. Settings resolve the same way: a buffer-local value (e.g.
from `C-x f`), the major mode and its parents, then the options.

### Themes

ymacs comes with `ymacs-dark`, `ymacs-light`, `solarized-dark` and
`solarized-light`. A theme is a set of colors
written as `#rrggbb` or `#rrggbbaa`. `ymacs --print-default-config` lists every
color name with its value in both built-in themes.

To change a few colors of a built-in theme, define just those colors under its
name. To make a new theme, give it a name and let it `#:inherits` an existing one
so you only list what differs:

```scheme
(setq theme '((light . paper) (dark . ymacs-dark)))
(define-theme 'ymacs-dark #:cursor "#ff6c6b")
(define-theme 'paper
  #:inherits 'ymacs-light
  #:background "#fdf6e3"
  #:current-line "#f5eedc")
```

A theme that doesn't inherit should define every color. Any color it leaves
out is taken from `ymacs-dark`, and the echo area warns about it.

Syntax highlighting colors are `syntax.<capture>` keys, named after
tree-sitter's highlight captures: `syntax.keyword`, `syntax.string`,
`syntax.comment`, `syntax.function`, `syntax.function.macro`, `syntax.type`…
They are optional. A capture without its own color uses the one for a shorter
name (`function.method` falls back to `function`), and text with no color at all
uses `foreground`:

```scheme
(define-theme 'ymacs-dark #:syntax.keyword "#ff79c6" #:syntax.comment "#6272a4")
```

`M-x load-theme` lists every theme, with each one shown live as you move
through the list. `RET` keeps the theme until you quit (it then stays fixed, even
when the system switches between light and dark), and `C-g` goes back to the one
you had. The `configured` entry returns to the theme from your config.

## Project layout

```
config/
  default.scm        built-in settings, key bindings, modes and themes
steel/               built-in Steel (Scheme) scripts, compiled into the binary
packaging/           Homebrew formula and Debian build helpers used by the release workflow
.github/workflows/   CI (fmt, clippy, audit, tests) and release (brew + deb) pipelines
src/
  main.rs            app entry point: opens the file or *scratch* buffer and the window
  ui.rs              GPUI view: rendering and platform-key to Emacs-key translation
  engine/            the editing engine, with no GUI dependencies
    theme.rs         themes, colors and fonts
    buffer.rs        rope, file association, undo/redo history
    buffers.rs       multiple buffers in most-recently-used order; picker actions, M-x
    cursor.rs        point, mark and mark ring for each cursor
    editor.rs        editor state: buffer, cursors, kill ring, command bookkeeping
    config.rs        configuration: settings, keymaps, modes, themes, user overrides
    config/eval.rs   the Scheme forms a configuration is written in
    modes.rs         major and minor modes: selection, settings and keymap layering
    commands.rs      interactive commands, named after their GNU Emacs counterparts
    keymap.rs        key parsing, keymaps, prefix keys and arguments, dispatcher
    search.rs        swiper-style line search (C-s)
    dired.rs         directory listings (C-x d): marks, file operations
    files.rs         find-file's path picker (also dired's target prompts)
    settings.rs      the settings editor (s-,): setting schema, search, writes settings.scm
    syntax.rs        tree-sitter parsing and highlighting; the list of languages
    script/          embedded Steel engine: the Scheme API, event handlers, timers
    picker.rs        fuzzy "completing read" picker (buffer switching, M-x, load-theme)
    line_edit.rs     Emacs line editing in picker and settings text fields
    text.rs          pure motion primitives: words, sentences, paragraphs, sexps
    tests.rs         behavioural tests driven by real key sequences
```

The engine does not depend on the GUI, so you can test it without opening a
window. In `tests.rs`, buffer strings mark cursor positions with `|`, and
several `|` mean several cursors.

## License

ymacs is released under the [MIT License](LICENSE).
