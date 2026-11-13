//! Evaluating configuration files: the Scheme forms a configuration is
//! written in, and the patches they produce.
//!
//! A file runs in a Steel engine with these forms defined:
//!
//! | form | does |
//! |---|---|
//! | `(setq option value ...)` | set options (names in [`OPTIONS`]) |
//! | `(set-option! 'option value)` | the same, as a function |
//! | `(define-keys 'keymap "keys" 'command ...)` | bind keys (`#f` unbinds) |
//! | `(define-key 'keymap "keys" 'command)` | bind one key sequence |
//! | `(global-set-key "keys" 'command)`, `(global-unset-key "keys")` | the global keymap |
//! | `(define-major-mode 'name #:lighter "N" ...)` | define or change a major mode |
//! | `(define-minor-mode 'name #:lighter "N")` | change a minor mode |
//! | `(define-theme 'name #:inherits 'other #:color "#rrggbb" ...)` | define or change a theme |
//!
//! Each form records one or more *patches*: fragments of the configuration
//! document (the shape [`super::RawConfig`] reads), applied in order the
//! way the layers are (settings replace, keymaps and modes merge). A form
//! with a bad argument records a warning and the rest of the file still
//! runs; a Scheme error stops the file there, keeping what ran before it.
//!
//! The engine is created once per thread and reused: creating one takes
//! tens of milliseconds, evaluating a configuration a few.

use std::cell::RefCell;
use std::mem::ManuallyDrop;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use ::steel::SteelVal;
use ::steel::rerrs::SteelErr;
use ::steel::steel_vm::engine::Engine;
use ::steel::steel_vm::register_fn::RegisterFn;
use serde_json::{Map, Value};

use super::KEYMAP_NAMES;
use crate::engine::modes::MinorMode;

/// An option `setq` can set, and where it goes in the document.
pub struct OptionDef {
    /// The name in Scheme, e.g. `fill-column`.
    pub name: &'static str,
    /// The dotted path in the document, e.g. `editor.fill_column`.
    pub path: &'static str,
    /// `#f` means "the default" (`null`) rather than false.
    pub nullable: bool,
    /// The value is a name, written back as a symbol (`'ymacs-dark`).
    pub symbol: bool,
}

macro_rules! options {
    ($( $name:literal => $path:literal $(, $flag:ident)* ; )*) => {
        /// Every option, in the order the settings file lists them.
        pub const OPTIONS: &[OptionDef] = &[
            $( OptionDef { name: $name, path: $path, nullable: options!(@has nullable $($flag)*), symbol: options!(@has symbol $($flag)*) }, )*
        ];
    };
    (@has $want:ident) => { false };
    (@has nullable nullable $($rest:ident)*) => { true };
    (@has symbol symbol $($rest:ident)*) => { true };
    (@has $want:ident $other:ident $($rest:ident)*) => { options!(@has $want $($rest)*) };
}

options! {
    "fill-column" => "editor.fill_column";
    "tab-width" => "editor.tab_width";
    "default-major-mode" => "modes.default_major_mode", symbol;
    "initial-major-mode" => "modes.initial_major_mode", symbol;
    "font-family" => "font.family";
    "font-ui-family" => "font.ui_family";
    "font-size" => "font.size";
    "font-ui-size" => "font.ui_size", nullable;
    "line-height" => "font.line_height";
    "theme" => "theme", symbol;
    "terminal-shell" => "terminal.shell", nullable;
    "terminal-shell-args" => "terminal.shell_args";
    "terminal-term" => "terminal.term";
    "terminal-scrollback-lines" => "terminal.scrollback_lines";
    "terminal-backend" => "terminal.backend", symbol;
    "terminal-replay-bytes" => "terminal.replay_bytes";
    "terminal-socket" => "terminal.socket", nullable;
    "terminal-daemon-idle-exit" => "terminal.daemon_idle_exit";
    "terminal-escape-keys" => "terminal.escape_keys";
    "restore-session" => "session.restore_session";
    "session-state-dir" => "session.state_dir", nullable;
}

/// The option called `name`.
pub fn option(name: &str) -> Option<&'static OptionDef> {
    OPTIONS.iter().find(|o| o.name == name)
}

/// A fragment of the configuration document, from one form.
#[derive(Clone, Debug)]
pub struct Patch {
    /// The form that made it, for messages: `(setq fill-column)`.
    pub what: String,
    /// The option it sets, if it came from `setq`.
    pub option: Option<&'static str>,
    pub value: Value,
}

/// What evaluating one file produced.
#[derive(Clone, Debug, Default)]
pub struct Layer {
    /// The file's name in messages.
    pub label: String,
    pub patches: Vec<Patch>,
    /// Problems, each already prefixed with `label`.
    pub warnings: Vec<String>,
    /// The file stopped at a Scheme error (it is also in `warnings`).
    pub failed: bool,
}

impl Layer {
    /// Whether the file holds nothing but `setq`s (the settings editor can
    /// rewrite it without losing anything).
    pub fn only_options(&self) -> bool {
        self.patches.iter().all(|p| p.option.is_some())
    }
}

/// The document the patches of `layers` add up to: objects merge key by
/// key, anything else replaces.
pub fn document<'a>(layers: impl IntoIterator<Item = &'a Layer>) -> Value {
    let mut doc = Value::Object(Map::new());
    for p in layers.into_iter().flat_map(|l| &l.patches) {
        merge(&mut doc, &p.value);
    }
    doc
}

fn merge(into: &mut Value, from: &Value) {
    match (into, from) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in b {
                match a.get_mut(k) {
                    Some(slot) => merge(slot, v),
                    None => {
                        a.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (slot, v) => *slot = v.clone(),
    }
}

/// Runs `src` (from `path`, if it is a file: Steel then resolves relative
/// `require`s against it).
pub fn eval(label: &str, src: &str, path: Option<&Path>) -> Layer {
    ENGINE.with(|cell| {
        let mut slot = cell.borrow_mut();
        let ConfigEngine { engine, collect } =
            &mut **slot.get_or_insert_with(|| ManuallyDrop::new(ConfigEngine::new()));
        *lock(collect) = Collect::default();
        let r = match path {
            Some(p) => engine.compile_and_run_raw_program_with_path(src.to_string(), p.to_path_buf()),
            None => engine.run(src.to_string()),
        };
        let Collect { patches, warnings } = std::mem::take(&mut *lock(collect));
        let mut layer = Layer {
            label: label.to_string(),
            patches,
            warnings: warnings.into_iter().map(|w| format!("{label}: {w}")).collect(),
            failed: false,
        };
        if let Err(e) = r {
            layer.failed = true;
            layer.warnings.push(format!("{label}:{} (the rest of the file was skipped)", describe(&e, src)));
        }
        layer
    })
}

/// `line: message` for a Scheme error.
fn describe(e: &SteelErr, src: &str) -> String {
    let msg = e.to_string();
    let msg = msg.strip_prefix("Error: ").unwrap_or(&msg).trim().to_string();
    match e.span() {
        Some(span) if (span.start as usize) <= src.len() => {
            let line = src.as_bytes()[..span.start as usize].iter().filter(|&&b| b == b'\n').count() + 1;
            format!("{line}: {msg}")
        }
        _ => format!(" {msg}"),
    }
}

thread_local! {
    /// Never dropped: dropping a Steel engine uses Steel's own thread-locals,
    /// which may be gone by the time this one is destroyed.
    static ENGINE: RefCell<Option<ManuallyDrop<ConfigEngine>>> = const { RefCell::new(None) };
}

struct ConfigEngine {
    engine: Engine,
    collect: Arc<Mutex<Collect>>,
}

/// What the forms of the file being evaluated recorded.
#[derive(Default)]
struct Collect {
    patches: Vec<Patch>,
    warnings: Vec<String>,
}

impl Collect {
    fn patch(&mut self, what: String, option: Option<&'static str>, value: Value) {
        self.patches.push(Patch { what, option, value });
    }
}

fn lock(c: &Mutex<Collect>) -> MutexGuard<'_, Collect> {
    c.lock().unwrap_or_else(|e| e.into_inner())
}

/// The forms, on top of the primitives registered in Rust.
const PRELUDE: &str = r#"
(define (set-option! name value) (%set-option name value))
(define-syntax setq
  (syntax-rules ()
    [(_ name value) (set-option! 'name value)]
    [(_ name value rest ...) (begin (set-option! 'name value) (setq rest ...))]))
(define (define-keys keymap . bindings) (%define-keys keymap bindings))
(define (define-key keymap keys command) (%define-keys keymap (list keys command)))
(define (global-set-key keys command) (define-key 'global keys command))
(define (global-unset-key keys) (define-key 'global keys #f))
(define (define-major-mode name . props) (%define-mode 'major name props))
(define (define-minor-mode name . props) (%define-mode 'minor name props))
(define (define-theme name . props) (%define-theme name props))
"#;

impl ConfigEngine {
    fn new() -> Self {
        let mut engine = Engine::new();
        let collect = Arc::new(Mutex::new(Collect::default()));
        let c = collect.clone();
        engine
            .register_fn("%set-option", move |name: SteelVal, value: SteelVal| set_option(&mut lock(&c), name, value));
        let c = collect.clone();
        engine.register_fn("%define-keys", move |map: SteelVal, bindings: SteelVal| {
            define_keys(&mut lock(&c), map, bindings)
        });
        let c = collect.clone();
        engine.register_fn("%define-mode", move |kind: SteelVal, name: SteelVal, props: SteelVal| {
            define_mode(&mut lock(&c), kind, name, props)
        });
        let c = collect.clone();
        engine.register_fn("%define-theme", move |name: SteelVal, props: SteelVal| {
            define_theme(&mut lock(&c), name, props)
        });
        if let Err(e) = engine.run(PRELUDE) {
            panic!("config prelude: {e}");
        }
        ConfigEngine { engine, collect }
    }
}

// ------------------------------------------------------------------ forms

fn set_option(c: &mut Collect, name: SteelVal, value: SteelVal) {
    let Some(name) = text(&name) else {
        return c.warnings.push(format!("set-option!: expected an option name, got {name}"));
    };
    let Some(opt) = option(&name) else {
        return c.warnings.push(format!("setq: unknown option {name}"));
    };
    let value = match value {
        SteelVal::BoolV(false) if opt.nullable => Value::Null,
        v => match to_json(&v) {
            Ok(v) => v,
            Err(e) => return c.warnings.push(format!("setq {name}: {e}")),
        },
    };
    let mut doc = Value::Object(Map::new());
    set_path(&mut doc, opt.path, value);
    c.patch(format!("(setq {name})"), Some(opt.name), doc);
}

/// Where the keymap called `name` is in the document.
fn keymap_path(name: &str) -> Vec<String> {
    let s = |x: &str| x.to_string();
    if KEYMAP_NAMES.contains(&name) {
        vec![s("keymaps"), s(name)]
    } else if MinorMode::from_name(name).is_some() {
        vec![s("modes"), s("minor"), s(name), s("keymap")]
    } else if name.ends_with("-mode") {
        vec![s("modes"), s("major"), s(name), s("keymap")]
    } else {
        // Reported as an unknown keymap when the layers are merged.
        vec![s("keymaps"), s(name)]
    }
}

fn define_keys(c: &mut Collect, map: SteelVal, bindings: SteelVal) {
    let Some(map) = text(&map) else {
        return c.warnings.push(format!("define-keys: expected a keymap name, got {map}"));
    };
    let ctx = format!("define-keys {map}");
    let items = list(&bindings).unwrap_or_default();
    if items.len() % 2 == 1 {
        c.warnings.push(format!("{ctx}: {} has no command", items[items.len() - 1]));
    }
    let mut keymap = Map::new();
    for pair in items.as_chunks::<2>().0 {
        let Some(keys) = text(&pair[0]) else {
            c.warnings.push(format!("{ctx}: expected a key sequence like \"C-x C-s\", got {}", pair[0]));
            continue;
        };
        let command = match &pair[1] {
            SteelVal::BoolV(false) => Value::Null,
            v => match text(v) {
                Some(cmd) => Value::String(cmd),
                None => {
                    c.warnings.push(format!("{ctx}: \"{keys}\": expected a command, got {v}"));
                    continue;
                }
            },
        };
        keymap.insert(keys, command);
    }
    c.patch(format!("({ctx})"), None, nest(&keymap_path(&map), Value::Object(keymap)));
}

fn define_mode(c: &mut Collect, kind: SteelVal, name: SteelVal, props: SteelVal) {
    let major = text(&kind).as_deref() == Some("major");
    let form = if major { "define-major-mode" } else { "define-minor-mode" };
    let Some(name) = text(&name) else {
        return c.warnings.push(format!("{form}: expected a mode name, got {name}"));
    };
    let ctx = format!("{form} {name}");
    let path = ["modes", if major { "major" } else { "minor" }, &name].map(str::to_string);
    // The mode exists even if no property is given.
    c.patch(format!("({ctx})"), None, nest(&path, Value::Object(Map::new())));
    for (key, value) in plist(c, &ctx, &props) {
        // A name, where #f clears an inherited one.
        let name_or_off = |v: &SteelVal| match v {
            SteelVal::BoolV(false) => Ok(Value::String(String::new())),
            v => to_json(v),
        };
        let (field, value): (&[&str], _) = match (major, key.as_str()) {
            (_, "lighter") => (&["lighter"], to_json(&value)),
            (true, "parent") => (&["parent"], name_or_off(&value)),
            (true, "syntax") => (&["syntax"], name_or_off(&value)),
            (true, "files") => (&["files"], to_json(&value)),
            (true, "interpreters") => (&["interpreters"], to_json(&value)),
            (true, "minor-modes") => (&["minor_modes"], to_json(&value)),
            (true, "fill-column") => (&["editor", "fill_column"], to_json(&value)),
            (true, "tab-width") => (&["editor", "tab_width"], to_json(&value)),
            _ => {
                let known = if major {
                    "lighter, parent, files, interpreters, syntax, fill-column, tab-width, minor-modes"
                } else {
                    "lighter"
                };
                c.warnings.push(format!("{ctx}: unknown property #:{key} (known: {known})"));
                continue;
            }
        };
        match value {
            Ok(v) => {
                let mut full: Vec<String> = path.to_vec();
                full.extend(field.iter().map(|f| f.to_string()));
                c.patch(format!("({ctx} #:{key})"), None, nest(&full, v));
            }
            Err(e) => c.warnings.push(format!("{ctx} #:{key}: {e}")),
        }
    }
}

fn define_theme(c: &mut Collect, name: SteelVal, props: SteelVal) {
    let Some(name) = text(&name) else {
        return c.warnings.push(format!("define-theme: expected a theme name, got {name}"));
    };
    let ctx = format!("define-theme {name}");
    let mut colors = Map::new();
    for (key, value) in plist(c, &ctx, &props) {
        let key = if key == "inherits" { key } else { key.replace('-', "_") };
        match text(&value) {
            Some(v) => {
                colors.insert(key, Value::String(v));
            }
            None => c.warnings.push(format!("{ctx}: #:{key}: expected a color like \"#rrggbb\", got {value}")),
        }
    }
    c.patch(format!("({ctx})"), None, nest(&["themes".to_string(), name], Value::Object(colors)));
}

/// `#:key value ...` (or `'key value ...`) as pairs, keys without `#:`.
fn plist(c: &mut Collect, ctx: &str, props: &SteelVal) -> Vec<(String, SteelVal)> {
    let items = list(props).unwrap_or_default();
    let mut out = Vec::new();
    let mut it = items.into_iter();
    while let Some(k) = it.next() {
        let Some(key) = text(&k).map(|k| k.strip_prefix("#:").map(str::to_string).unwrap_or(k)) else {
            c.warnings.push(format!("{ctx}: expected a property like #:lighter, got {k}"));
            it.next();
            continue;
        };
        match it.next() {
            Some(v) => out.push((key, v)),
            None => c.warnings.push(format!("{ctx}: #:{key} has no value")),
        }
    }
    out
}

// ------------------------------------------------------------- conversion

/// A string or symbol's text.
fn text(v: &SteelVal) -> Option<String> {
    match v {
        SteelVal::StringV(s) | SteelVal::SymbolV(s) => Some(s.to_string()),
        _ => None,
    }
}

fn list(v: &SteelVal) -> Option<Vec<SteelVal>> {
    match v {
        SteelVal::ListV(l) => Some(l.iter().cloned().collect()),
        SteelVal::VectorV(v) => Some(v.iter().cloned().collect()),
        _ => None,
    }
}

/// A Scheme value as JSON: symbols become strings, lists arrays, and
/// association lists (`'((light . a) (dark . b))`) objects.
fn to_json(v: &SteelVal) -> Result<Value, String> {
    Ok(match v {
        SteelVal::BoolV(b) => Value::Bool(*b),
        SteelVal::IntV(n) => Value::from(*n as i64),
        SteelVal::NumV(f) => {
            serde_json::Number::from_f64(*f).map(Value::Number).ok_or(format!("{f} is not a number"))?
        }
        SteelVal::StringV(s) | SteelVal::SymbolV(s) => Value::String(s.to_string()),
        SteelVal::CharV(ch) => Value::String(ch.to_string()),
        SteelVal::ListV(_) | SteelVal::VectorV(_) => {
            let items = list(v).unwrap_or_default();
            if !items.is_empty() && items.iter().all(|i| matches!(i, SteelVal::Pair(_))) {
                let mut m = Map::new();
                for i in &items {
                    let SteelVal::Pair(p) = i else { unreachable!() };
                    let key = text(p.car_ref()).ok_or(format!("{} is not a name", p.car_ref()))?;
                    m.insert(key, to_json(p.cdr_ref())?);
                }
                Value::Object(m)
            } else {
                Value::Array(items.iter().map(to_json).collect::<Result<_, _>>()?)
            }
        }
        other => return Err(format!("can't use {other} here")),
    })
}

/// `value` under the keys of `path`, outermost first.
fn nest(path: &[String], value: Value) -> Value {
    path.iter().rev().fold(value, |v, k| {
        let mut m = Map::new();
        m.insert(k.clone(), v);
        Value::Object(m)
    })
}

// --------------------------------------------------------- document paths

/// The value at dotted `path`.
pub fn get_path<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(v, |v, k| v.as_object()?.get(k))
}

/// Set dotted `path`, creating (or replacing non-object) parents as needed.
pub fn set_path(v: &mut Value, path: &str, value: Value) {
    let mut cur = v;
    let parts: Vec<&str> = path.split('.').collect();
    for p in &parts[..parts.len() - 1] {
        if !cur.as_object().is_some_and(|m| m.get(*p).is_some_and(Value::is_object)) {
            cur.as_object_mut().expect("parent is an object").insert(p.to_string(), Value::Object(Map::new()));
        }
        cur = cur.as_object_mut().unwrap().get_mut(*p).unwrap();
    }
    cur.as_object_mut().expect("parent is an object").insert(parts[parts.len() - 1].to_string(), value);
}

/// Remove dotted `path`, and any parent objects that become empty.
pub fn remove_path(v: &mut Value, path: &str) {
    fn go(v: &mut Value, parts: &[&str]) {
        let Some(m) = v.as_object_mut() else { return };
        if let [last] = parts {
            m.shift_remove(*last);
            return;
        }
        if let Some(child) = m.get_mut(parts[0]) {
            go(child, &parts[1..]);
            if child.as_object().is_some_and(Map::is_empty) {
                m.shift_remove(parts[0]);
            }
        }
    }
    go(v, &path.split('.').collect::<Vec<_>>());
}

// ---------------------------------------------------------------- writing

/// The settings file for the options set in `doc` (only options are kept).
pub fn settings_source(doc: &Value) -> String {
    let mut out = String::from(
        ";;; settings.scm --- written by the settings editor (s-,).\n\
         ;;;\n\
         ;;; Loaded after init.scm. The settings editor rewrites this file when a\n\
         ;;; setting changes, keeping only (setq ...) forms: put anything else in\n\
         ;;; init.scm.\n\n",
    );
    for o in OPTIONS {
        if let Some(v) = get_path(doc, o.path) {
            out.push_str(&format!("(setq {} {})\n", o.name, literal(v, o.symbol)));
        }
    }
    out
}

/// `v` as a Scheme expression that evaluates to it.
fn literal(v: &Value, symbol: bool) -> String {
    match v {
        Value::Array(_) | Value::Object(_) => format!("'{}", datum(v, symbol)),
        Value::String(s) if symbol && is_symbol(s) => format!("'{s}"),
        _ => datum(v, symbol),
    }
}

/// `v` as a quoted datum (inside a `'`).
fn datum(v: &Value, symbol: bool) -> String {
    match v {
        Value::Null | Value::Bool(false) => "#f".into(),
        Value::Bool(true) => "#t".into(),
        Value::Number(n) => n.to_string(),
        Value::String(s) if symbol && is_symbol(s) => s.clone(),
        Value::String(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        Value::Array(items) => {
            format!("({})", items.iter().map(|i| datum(i, symbol)).collect::<Vec<_>>().join(" "))
        }
        Value::Object(m) => format!(
            "({})",
            m.iter().map(|(k, v)| format!("({} . {})", k, datum(v, symbol))).collect::<Vec<_>>().join(" ")
        ),
    }
}

fn is_symbol(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with(|c: char| c.is_ascii_digit() || c == '#')
        && s.chars().all(|c| c.is_alphanumeric() || "-_.+*/<>=!?".contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(src: &str) -> (Value, Vec<String>) {
        let l = eval("test", src, None);
        (document([&l]), l.warnings)
    }

    #[test]
    fn forms_build_the_document() {
        let (d, w) = doc(r##"
            (setq fill-column 80 theme '((light . paper) (dark . ymacs-dark)) terminal-shell #f restore-session #f)
            (define-keys 'global "C-z" 'undo "C-x C-c" #f)
            (global-set-key "C-." "redo")
            (define-key 'rust-mode "C-c C-c" 'compile)
            (define-key 'auto-fill-mode "M-q" 'fill-paragraph)
            (define-major-mode 'foo-mode #:parent 'prog-mode #:files '("*.foo") #:syntax #f #:tab-width 2)
            (define-theme 'paper #:inherits 'ymacs-light #:current-line "#eee" #:syntax.keyword "#f00")
        "##);
        assert!(w.is_empty(), "{w:#?}");
        let expected = serde_json::json!({
            "editor": { "fill_column": 80 },
            "theme": { "light": "paper", "dark": "ymacs-dark" },
            "terminal": { "shell": null },
            "session": { "restore_session": false },
            "keymaps": { "global": { "C-z": "undo", "C-x C-c": null, "C-.": "redo" } },
            "modes": {
                "major": {
                    "rust-mode": { "keymap": { "C-c C-c": "compile" } },
                    "foo-mode": { "parent": "prog-mode", "files": ["*.foo"], "syntax": "", "editor": { "tab_width": 2 } }
                },
                "minor": { "auto-fill-mode": { "keymap": { "M-q": "fill-paragraph" } } }
            },
            "themes": { "paper": { "inherits": "ymacs-light", "current_line": "#eee", "syntax.keyword": "#f00" } }
        });
        assert_eq!(d, expected);
    }

    #[test]
    fn bad_forms_warn_and_the_file_goes_on() {
        let l = eval(
            "init.scm",
            r#"(setq no-such-option 1)
               (define-keys 'global "C-a" 5 "C-b")
               (define-major-mode 'x-mode #:lightr "X")
               (setq fill-column 90)
               (car 5)
               (setq tab-width 8)"#,
            None,
        );
        let w = l.warnings.join("\n");
        assert!(w.contains("init.scm: setq: unknown option no-such-option"), "{w}");
        assert!(w.contains("\"C-a\": expected a command, got 5"), "{w}");
        assert!(w.contains("\"C-b\" has no command"), "{w}");
        assert!(w.contains("unknown property #:lightr"), "{w}");
        assert!(w.contains("init.scm:5: TypeMismatch"), "{w}");
        assert!(l.failed);
        let d = document([&l]);
        assert_eq!(d["editor"], serde_json::json!({ "fill_column": 90 })); // tab-width never ran
    }

    #[test]
    fn settings_round_trip() {
        let (d, _) = doc(r#"(setq fill-column 80 theme 'solarized-dark terminal-shell-args '("-l" "a \"b\"")
                     terminal-socket #f line-height 1.25 default-major-mode 'text-mode)"#);
        let src = settings_source(&d);
        assert!(src.contains("(setq theme 'solarized-dark)"), "{src}");
        assert!(src.contains("(setq terminal-socket #f)"), "{src}");
        let l = eval("settings.scm", &src, None);
        assert!(l.warnings.is_empty() && l.only_options(), "{:#?}", l.warnings);
        assert_eq!(document([&l]), d);
    }
}
