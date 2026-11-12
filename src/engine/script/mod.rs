//! Scripting: an embedded [Steel](https://github.com/mattwparas/steel)
//! (Scheme) engine.
//!
//! Rust provides mechanisms (screens, processes, input, timers, events);
//! policy lives in Scheme. The built-in scripts are in `steel/` at the repo
//! root and compiled into the binary; user scripts
//! (`~/.config/ymacs/steel/*.scm`) are loaded after them and can redefine
//! anything they define.
//!
//! [`ScriptHost`] owns the engine. Whoever embeds it (the PTY daemon, later
//! the editor) implements [`Host`]; every entry point takes the host for the
//! duration of the call, and the Scheme functions that need it reach it
//! through a scoped pointer (see [`with_host`]). The engine is not `Send`: it
//! is created on, and never leaves, the embedder's thread.
//!
//! Script errors never take the host down: a failing load is returned to
//! the caller, a failing event handler or timer is reported through
//! [`Host::log`] and skipped.
//!
//! Scheme API (generic part; the host adds its own through [`Host`]):
//!
//! | function | does |
//! |---|---|
//! | `(on 'event f)` | call `f` with the event's arguments whenever the host emits it |
//! | `(run-after ms f)` | call `(f)` once, `ms` milliseconds from now; returns a timer id |
//! | `(cancel-timer id)` | forget a pending timer |
//! | `(regex-match? re text)` | whether `re` matches somewhere in `text` |
//! | `(regex-find re text)` | the first match of `re` in `text`, or `#f` |
//! | `(log msg)` | a message in the host's log |
//! | `(terminal-screen id rows)` | the bottom `rows` rows of a terminal, as a list of strings, or `#f` |
//! | `(terminal-process id)` | the terminal's foreground process name, or `#f` |
//! | `(terminal-env id key)` | a variable of the terminal's spawn environment, or `#f` |
//! | `(terminal-send id text)` | type `text` into the terminal |
//! | `(terminal-keys id keys)` | send named keys (`"enter"`, `"esc"`, `"ctrl+c"`, ...) |
//! | `(agent-set! id kind state)` | publish that terminal `id` runs agent `kind` in `state` |
//! | `(agent-clear! id)` | terminal `id` no longer runs an agent |

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use ::steel::SteelVal;
use ::steel::rerrs::SteelErr;
use ::steel::steel_vm::engine::Engine;
use ::steel::steel_vm::register_fn::RegisterFn;
use regex::Regex;

#[cfg(test)]
mod tests;

/// Built-in scripts, loaded in this order before any user script.
pub const BUILTIN: &[(&str, &str)] = &[("core.scm", include_str!("../../../steel/core.scm"))];

/// A terminal, as scripts name it: the daemon's session id.
pub type TerminalId = u64;

/// What a script can ask of whoever embeds the engine.
///
/// Queries answer `None` for a terminal that does not exist (Scheme sees
/// `#f`); actions answer an error message, which Scheme sees as an error.
pub trait Host {
    /// The bottom `rows` rows of the terminal's live screen (not a scrolled
    /// view), top to bottom, trailing blanks trimmed.
    fn terminal_screen(&mut self, id: TerminalId, rows: usize) -> Option<Vec<String>>;
    /// The name of the terminal's foreground process.
    fn terminal_process(&mut self, id: TerminalId) -> Option<String>;
    /// A variable of the environment the terminal was spawned with.
    fn terminal_env(&mut self, id: TerminalId, key: &str) -> Option<String>;
    /// Types `text` into the terminal.
    fn terminal_send(&mut self, id: TerminalId, text: &str) -> Result<(), String>;
    /// Sends named keys (`"enter"`, `"esc"`, `"ctrl+c"`, ...), in order.
    fn terminal_keys(&mut self, id: TerminalId, keys: &[String]) -> Result<(), String>;
    /// Terminal `id` runs agent `kind`, currently in `state`.
    fn agent_set(&mut self, id: TerminalId, kind: &str, state: &str) -> Result<(), String>;
    /// Terminal `id` no longer runs an agent.
    fn agent_clear(&mut self, id: TerminalId);
    /// A message from a script, or about one (a failing handler).
    fn log(&mut self, msg: &str);
}

/// A script that failed to load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptError {
    /// The script's name (built-ins) or path (user scripts).
    pub script: String,
    pub message: String,
}

impl std::fmt::Display for ScriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.script, self.message)
    }
}

/// Where user scripts live: `steel/` next to the user's configuration file.
pub fn user_script_dir() -> Option<PathBuf> {
    super::config::user_config_path()?.parent().map(|d| d.join("steel"))
}

/// The part of the API that keeps Scheme values (handlers, timer
/// callbacks): they stay in the engine, Rust only keeps names, ids and
/// deadlines. Loaded before anything else.
const PRELUDE: &str = r#"
(define *ymacs-handlers* (hash))
(define *ymacs-timers* (hash))

(define (on event f)
  (unless (procedure? f) (error "on: expected a procedure, got" f))
  (let ([name (%event-name event)])
    (set! *ymacs-handlers*
          (hash-insert *ymacs-handlers* name
                       (cons f (if (hash-contains? *ymacs-handlers* name)
                                   (hash-ref *ymacs-handlers* name)
                                   '()))))
    (%note-handler name)))

(define (%handlers name)
  (if (hash-contains? *ymacs-handlers* name)
      (reverse (hash-ref *ymacs-handlers* name))
      '()))

(define (run-after ms f)
  (unless (procedure? f) (error "run-after: expected a procedure, got" f))
  (let ([id (%schedule ms)])
    (set! *ymacs-timers* (hash-insert *ymacs-timers* id f))
    id))

(define (cancel-timer id)
  (let ([had (hash-contains? *ymacs-timers* id)])
    (set! *ymacs-timers* (hash-remove *ymacs-timers* id))
    (%unschedule id)
    had))

(define (%take-timer id)
  (if (hash-contains? *ymacs-timers* id)
      (let ([f (hash-ref *ymacs-timers* id)])
        (set! *ymacs-timers* (hash-remove *ymacs-timers* id))
        f)
      #f))
"#;

/// State the registered Rust functions share with [`ScriptHost`].
/// Registered functions must be `Send + Sync`, so nothing here is a Scheme
/// value.
#[derive(Default)]
struct Shared {
    /// Events with at least one handler.
    events: HashSet<String>,
    /// Pending timers: id and deadline.
    timers: Vec<(u64, Instant)>,
    next_timer: u64,
    regexes: HashMap<String, Arc<Regex>>,
}

/// The engine plus what Rust tracks of the handlers and timers scripts
/// registered.
pub struct ScriptHost {
    engine: Engine,
    shared: Arc<Mutex<Shared>>,
}

impl ScriptHost {
    /// A fresh engine with the API registered and no script loaded.
    pub fn new() -> Self {
        let mut engine = Engine::new();
        let shared = Arc::new(Mutex::new(Shared::default()));
        register_api(&mut engine, &shared);
        if let Err(e) = engine.run(PRELUDE) {
            panic!("script prelude: {e}");
        }
        ScriptHost { engine, shared }
    }

    /// A fresh engine with the built-in scripts loaded. A built-in failing
    /// to load is a bug; it is reported like a user script's error.
    pub fn with_builtins(host: &mut dyn Host) -> (Self, Vec<ScriptError>) {
        let mut s = Self::new();
        let errors = BUILTIN.iter().filter_map(|(name, src)| s.load(host, name, src).err()).collect();
        (s, errors)
    }

    /// Runs a script's top level. `name` labels errors.
    pub fn load(&mut self, host: &mut dyn Host, name: &str, src: &str) -> Result<(), ScriptError> {
        let src = src.to_string();
        let r = with_host(host, || self.engine.run(src));
        r.map(|_| ()).map_err(|e| ScriptError { script: name.to_string(), message: self.describe(e) })
    }

    /// Runs the script in file `path` (Steel then knows the file, for
    /// error spans and relative `require`s).
    pub fn load_file(&mut self, host: &mut dyn Host, path: &Path) -> Result<(), ScriptError> {
        let script = path.display().to_string();
        let src = std::fs::read_to_string(path)
            .map_err(|e| ScriptError { script: script.clone(), message: e.to_string() })?;
        let r = with_host(host, || self.engine.compile_and_run_raw_program_with_path(src, path.to_path_buf()));
        r.map(|_| ()).map_err(|e| ScriptError { script, message: self.describe(e) })
    }

    /// Loads every `*.scm` file in `dir`, in name order. A missing directory
    /// is not an error; each script that fails is reported and skipped.
    pub fn load_dir(&mut self, host: &mut dyn Host, dir: &Path) -> Vec<ScriptError> {
        let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
        let mut paths: Vec<PathBuf> = rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "scm") && p.is_file())
            .collect();
        paths.sort();
        paths.into_iter().filter_map(|p| self.load_file(host, &p).err()).collect()
    }

    /// Evaluates `src` and returns the last value (tests, a future REPL).
    pub fn eval(&mut self, host: &mut dyn Host, src: &str) -> Result<SteelVal, String> {
        let src = src.to_string();
        let r = with_host(host, || self.engine.run(src));
        match r {
            Ok(vals) => Ok(vals.into_iter().last().unwrap_or(SteelVal::Void)),
            Err(e) => Err(self.describe(e)),
        }
    }

    /// Calls the global function `name` with `args`.
    pub fn call(&mut self, host: &mut dyn Host, name: &str, args: Vec<SteelVal>) -> Result<SteelVal, String> {
        let r = with_host(host, || self.engine.call_function_by_name_with_args(name, args));
        r.map_err(|e| self.describe(e))
    }

    /// Whether any handler is registered for `event`; lets the host skip
    /// building arguments nobody will see.
    pub fn handles(&self, event: &str) -> bool {
        lock(&self.shared).events.contains(event)
    }

    /// Calls every handler of `event` with `args`, in registration order.
    /// A failing handler is logged and the others still run.
    pub fn emit(&mut self, host: &mut dyn Host, event: &str, args: Vec<SteelVal>) {
        if !self.handles(event) {
            return;
        }
        let handlers = match self.call(host, "%handlers", vec![SteelVal::StringV(event.into())]) {
            Ok(SteelVal::ListV(l)) => l.iter().cloned().collect::<Vec<_>>(),
            Ok(_) => Vec::new(),
            Err(e) => {
                host.log(&format!("script: '{event}' handlers unavailable: {e}"));
                Vec::new()
            }
        };
        for f in handlers {
            let r = with_host(host, || self.engine.call_function_with_args(f, args.clone()));
            if let Err(e) = r {
                let msg = format!("script: '{event}' handler failed: {}", self.describe(e));
                host.log(&msg);
            }
        }
    }

    /// When the next timer is due, if any; the host sleeps until then.
    pub fn next_deadline(&self) -> Option<Instant> {
        lock(&self.shared).timers.iter().map(|&(_, due)| due).min()
    }

    /// Runs the timers due at `now`, in due order. Timers a handler sets
    /// while running wait for the next call, even if already due.
    pub fn fire_due(&mut self, host: &mut dyn Host, now: Instant) {
        let mut due: Vec<(u64, Instant)> = {
            let mut sh = lock(&self.shared);
            let (due, pending) = std::mem::take(&mut sh.timers).into_iter().partition(|&(_, d)| d <= now);
            sh.timers = pending;
            due
        };
        due.sort_by_key(|&(id, d)| (d, id));
        for (id, _) in due {
            let f = match self.call(host, "%take-timer", vec![SteelVal::IntV(id as isize)]) {
                Ok(SteelVal::BoolV(false)) => continue,
                Ok(f) => f,
                Err(e) => {
                    host.log(&format!("script: timer {id} unavailable: {e}"));
                    continue;
                }
            };
            let r = with_host(host, || self.engine.call_function_with_args(f, Vec::new()));
            if let Err(e) = r {
                let msg = format!("script: timer failed: {}", self.describe(e));
                host.log(&msg);
            }
        }
    }

    /// An error as a readable message (with the source span when Steel
    /// knows it).
    fn describe(&self, e: SteelErr) -> String {
        let plain = e.to_string();
        self.engine.raise_error_to_string(e).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).unwrap_or(plain)
    }
}

impl Default for ScriptHost {
    fn default() -> Self {
        Self::new()
    }
}

thread_local! {
    /// The host of the call in progress on this thread (see [`with_host`]).
    static CURRENT: Cell<Option<NonNull<dyn Host>>> = const { Cell::new(None) };
}

/// Runs `f` with `host` reachable from the registered Scheme functions.
///
/// The pointer is only stored for the duration of `f` (a guard restores
/// the previous one, even on unwind), and [`host`] takes it out while it
/// uses it, so two `&mut` to the host never coexist even if a host method
/// re-enters the engine.
fn with_host<R>(host: &mut dyn Host, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<NonNull<dyn Host>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CURRENT.with(|c| c.set(self.0));
        }
    }
    let ptr: NonNull<dyn Host + '_> = NonNull::from(host);
    // SAFETY: only the lifetime is erased. The pointer is dereferenced only
    // inside `f` (through `host`), while the borrow it came from is alive;
    // the guard removes it before `with_host` returns.
    let ptr: NonNull<dyn Host> = unsafe { std::mem::transmute(ptr) };
    let _restore = Restore(CURRENT.with(|c| c.replace(Some(ptr))));
    f()
}

/// Calls `f` with the host of the current call, or fails if there is none
/// (a host function called outside [`with_host`], or re-entered).
fn host<R>(f: impl FnOnce(&mut dyn Host) -> R) -> Result<R, String> {
    let Some(mut ptr) = CURRENT.with(|c| c.take()) else {
        return Err("no host available here".to_string());
    };
    struct Restore(NonNull<dyn Host>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CURRENT.with(|c| c.set(Some(self.0)));
        }
    }
    let _restore = Restore(ptr);
    // SAFETY: set by `with_host` from a live `&mut`, and taken out of
    // `CURRENT` above, so this is the only reference until `_restore` puts
    // it back.
    Ok(f(unsafe { ptr.as_mut() }))
}

/// A Scheme symbol or string as a Rust string.
fn name_of(v: &SteelVal) -> Result<String, String> {
    match v {
        SteelVal::SymbolV(s) | SteelVal::StringV(s) => Ok(s.to_string()),
        other => Err(format!("expected a symbol or string, got {other}")),
    }
}

/// The shared state. Only the engine thread touches it, so the lock never
/// waits; a poisoned one (a panicking handler) is still consistent.
fn lock(sh: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    sh.lock().unwrap_or_else(|e| e.into_inner())
}

fn terminal_id(id: isize) -> Result<TerminalId, String> {
    TerminalId::try_from(id).map_err(|_| format!("bad terminal id {id}"))
}

fn register_api(engine: &mut Engine, shared: &Arc<Mutex<Shared>>) {
    let sh = shared.clone();
    engine.register_fn("%note-handler", move |event: String| {
        lock(&sh).events.insert(event);
    });
    engine.register_fn("%event-name", |event: SteelVal| name_of(&event));

    let sh = shared.clone();
    engine.register_fn("%schedule", move |ms: isize| -> isize {
        let mut sh = lock(&sh);
        sh.next_timer += 1;
        let id = sh.next_timer;
        let due = Instant::now() + Duration::from_millis(ms.max(0) as u64);
        sh.timers.push((id, due));
        id as isize
    });
    let sh = shared.clone();
    engine.register_fn("%unschedule", move |id: isize| {
        lock(&sh).timers.retain(|&(t, _)| t as isize != id);
    });

    let sh = shared.clone();
    let compile = move |re: &str| -> Result<Arc<Regex>, String> {
        let mut sh = lock(&sh);
        if let Some(r) = sh.regexes.get(re) {
            return Ok(r.clone());
        }
        let r = Arc::new(Regex::new(re).map_err(|e| format!("bad regex: {e}"))?);
        sh.regexes.insert(re.to_string(), r.clone());
        Ok(r)
    };
    let compile = Arc::new(compile);
    let c = compile.clone();
    engine.register_fn("regex-match?", move |re: String, text: String| -> Result<bool, String> {
        Ok(c(&re)?.is_match(&text))
    });
    let c = compile;
    engine.register_fn("regex-find", move |re: String, text: String| -> Result<Option<String>, String> {
        Ok(c(&re)?.find(&text).map(|m| m.as_str().to_string()))
    });

    engine.register_fn("log", |msg: SteelVal| -> Result<(), String> {
        let msg = match msg {
            SteelVal::StringV(s) => s.to_string(),
            other => other.to_string(),
        };
        host(|h| h.log(&msg))
    });

    engine.register_fn("terminal-screen", |id: isize, rows: isize| -> Result<Option<Vec<String>>, String> {
        let id = terminal_id(id)?;
        host(|h| h.terminal_screen(id, rows.max(0) as usize))
    });
    engine.register_fn("terminal-process", |id: isize| -> Result<Option<String>, String> {
        let id = terminal_id(id)?;
        host(|h| h.terminal_process(id))
    });
    engine.register_fn("terminal-env", |id: isize, key: String| -> Result<Option<String>, String> {
        let id = terminal_id(id)?;
        host(|h| h.terminal_env(id, &key))
    });
    engine.register_fn("terminal-send", |id: isize, text: String| -> Result<(), String> {
        let id = terminal_id(id)?;
        host(|h| h.terminal_send(id, &text))?
    });
    engine.register_fn("terminal-keys", |id: isize, keys: SteelVal| -> Result<(), String> {
        let id = terminal_id(id)?;
        let keys = match &keys {
            SteelVal::ListV(l) => l.iter().map(name_of).collect::<Result<Vec<_>, _>>()?,
            other => vec![name_of(other)?],
        };
        host(|h| h.terminal_keys(id, &keys))?
    });
    engine.register_fn("agent-set!", |id: isize, kind: SteelVal, state: SteelVal| -> Result<(), String> {
        let id = terminal_id(id)?;
        let (kind, state) = (name_of(&kind)?, name_of(&state)?);
        host(|h| h.agent_set(id, &kind, &state))?
    });
    engine.register_fn("agent-clear!", |id: isize| -> Result<(), String> {
        let id = terminal_id(id)?;
        host(|h| h.agent_clear(id))
    });
}
