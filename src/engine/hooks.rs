//! Hooks: named handlers that run when something happens in the editor, and
//! timers.
//!
//! - **Events** ([`Event`]) describe what happened (a buffer was created, a
//!   command ran, a terminal printed something...). Each has a kind
//!   ([`EventKind`]) with a stable kebab-case name (`after-command`), so a
//!   scripting layer can register for it by name, and serializes to JSON
//!   (`{"event": "after-command", "name": "forward-char"}`) as plain data.
//! - **Handlers** are registered per event kind under a name. Adding a handler
//!   with a name that is already registered for that kind replaces it (like
//!   `add-hook` with the same symbol), so re-running a setup is harmless.
//!   Handlers run in the order they were first added.
//! - **The queue**: [`Editor::emit`] only queues an event; handlers never run
//!   re-entrantly inside the code that emitted it. The queue is drained by
//!   [`Editor::run_hooks`] after every command and on every UI tick. Handlers
//!   may emit more events; they are handled in the same drain, up to
//!   [`MAX_EVENTS_PER_DRAIN`] events, after which the rest are dropped and a
//!   warning is shown (this stops handler -> event -> handler loops).
//! - **Timers** fire once ([`Hooks::run_at`]) or repeatedly
//!   ([`Hooks::run_every`]). A due timer queues [`Event::Timer`]; its own
//!   callback (if any) runs first, then the `timer` handlers. Time is always
//!   passed in, so tests are deterministic; the UI asks
//!   [`Hooks::next_deadline`] when to tick next.

use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

use serde::Serialize;

use super::buffer::BufferId;
use super::editor::Editor;
use super::pty::ExitStatus;

/// At most this many events are handled per drain; the rest are dropped.
pub const MAX_EVENTS_PER_DRAIN: usize = 1000;

/// Something that happened. Handlers receive it by reference.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum Event {
    /// A buffer was added to the buffer list (also the first buffer).
    BufferCreated { buf: BufferId },
    /// A buffer was removed from the buffer list.
    BufferKilled { buf: BufferId, name: String },
    /// A command finished (successfully or not), like Emacs' `post-command-hook`.
    AfterCommand { name: String },
    /// Windows were split, deleted or resized, a window showed another
    /// buffer, the selected window changed, or a tab was added, closed,
    /// renamed or selected.
    WindowConfigurationChanged,
    /// A terminal buffer received output (at most once per buffer per tick).
    TerminalOutput { buf: BufferId },
    /// A terminal program set its title (OSC 0/2).
    TerminalTitle { buf: BufferId, title: String },
    /// A terminal program rang the bell.
    TerminalBell { buf: BufferId },
    /// The process in a terminal buffer exited.
    ProcessExited { buf: BufferId, status: ExitStatus },
    /// A timer is due.
    Timer { id: TimerId, name: String },
    /// The editor is about to exit (like `kill-emacs-hook`).
    KillEmacs,
}

/// The kind of an [`Event`]: what handlers are registered for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventKind {
    BufferCreated,
    BufferKilled,
    AfterCommand,
    WindowConfigurationChanged,
    TerminalOutput,
    TerminalTitle,
    TerminalBell,
    ProcessExited,
    Timer,
    KillEmacs,
}

impl EventKind {
    pub const ALL: [EventKind; 10] = [
        EventKind::BufferCreated,
        EventKind::BufferKilled,
        EventKind::AfterCommand,
        EventKind::WindowConfigurationChanged,
        EventKind::TerminalOutput,
        EventKind::TerminalTitle,
        EventKind::TerminalBell,
        EventKind::ProcessExited,
        EventKind::Timer,
        EventKind::KillEmacs,
    ];

    /// The stable name scripts use, e.g. `after-command`.
    pub fn name(self) -> &'static str {
        match self {
            EventKind::BufferCreated => "buffer-created",
            EventKind::BufferKilled => "buffer-killed",
            EventKind::AfterCommand => "after-command",
            EventKind::WindowConfigurationChanged => "window-configuration-changed",
            EventKind::TerminalOutput => "terminal-output",
            EventKind::TerminalTitle => "terminal-title",
            EventKind::TerminalBell => "terminal-bell",
            EventKind::ProcessExited => "process-exited",
            EventKind::Timer => "timer",
            EventKind::KillEmacs => "kill-emacs",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.name() == name)
    }
}

impl Event {
    pub fn kind(&self) -> EventKind {
        match self {
            Event::BufferCreated { .. } => EventKind::BufferCreated,
            Event::BufferKilled { .. } => EventKind::BufferKilled,
            Event::AfterCommand { .. } => EventKind::AfterCommand,
            Event::WindowConfigurationChanged => EventKind::WindowConfigurationChanged,
            Event::TerminalOutput { .. } => EventKind::TerminalOutput,
            Event::TerminalTitle { .. } => EventKind::TerminalTitle,
            Event::TerminalBell { .. } => EventKind::TerminalBell,
            Event::ProcessExited { .. } => EventKind::ProcessExited,
            Event::Timer { .. } => EventKind::Timer,
            Event::KillEmacs => EventKind::KillEmacs,
        }
    }

    /// The event as plain data, e.g. `{"event": "buffer-created", "buf": 3}`.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

/// A hook handler. An error is shown in the echo area; other handlers still run.
pub type HookFn = Rc<dyn Fn(&mut Editor, &Event) -> Result<(), String>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct TimerId(pub u64);

struct Timer {
    id: TimerId,
    name: String,
    due: Instant,
    /// `Some` for a repeating timer.
    every: Option<Duration>,
    callback: Option<HookFn>,
}

/// A registered handler, as listed by [`Hooks::list`].
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct HookInfo {
    pub kind: EventKind,
    pub handlers: Vec<String>,
}

/// A timer, as listed by [`Hooks::timers`].
#[derive(Clone, Debug, PartialEq)]
pub struct TimerInfo {
    pub id: TimerId,
    pub name: String,
    pub due: Instant,
    pub every: Option<Duration>,
}

/// The hook registry, the event queue and the timers (one per editor).
#[derive(Default)]
pub struct Hooks {
    handlers: HashMap<EventKind, Vec<(String, HookFn)>>,
    queue: VecDeque<Event>,
    timers: Vec<Timer>,
    next_timer: u64,
    /// Callbacks of one-shot timers that fired, until their event is handled.
    fired: HashMap<TimerId, HookFn>,
    draining: bool,
}

impl Hooks {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `f` for `kind` under `name`, replacing a handler of that name
    /// (which keeps its place in the order).
    pub fn add(&mut self, kind: EventKind, name: &str, f: HookFn) {
        let list = self.handlers.entry(kind).or_default();
        match list.iter_mut().find(|(n, _)| n == name) {
            Some(slot) => slot.1 = f,
            None => list.push((name.to_string(), f)),
        }
    }

    /// Register a handler by event kind name (`"after-command"`).
    pub fn add_named(&mut self, kind: &str, name: &str, f: HookFn) -> Result<(), String> {
        let k = EventKind::from_name(kind).ok_or_else(|| format!("Unknown hook: {kind}"))?;
        self.add(k, name, f);
        Ok(())
    }

    /// Remove the handler `name` from `kind`. Returns whether there was one.
    pub fn remove(&mut self, kind: EventKind, name: &str) -> bool {
        let Some(list) = self.handlers.get_mut(&kind) else { return false };
        let before = list.len();
        list.retain(|(n, _)| n != name);
        before != list.len()
    }

    /// Handler names for `kind`, in the order they run.
    pub fn handler_names(&self, kind: EventKind) -> Vec<&str> {
        self.handlers.get(&kind).map_or_else(Vec::new, |l| l.iter().map(|(n, _)| n.as_str()).collect())
    }

    /// Every event kind with its handlers (kinds without handlers included).
    pub fn list(&self) -> Vec<HookInfo> {
        EventKind::ALL
            .iter()
            .map(|k| HookInfo { kind: *k, handlers: self.handler_names(*k).into_iter().map(String::from).collect() })
            .collect()
    }

    fn handlers_for(&self, kind: EventKind) -> Vec<(String, HookFn)> {
        self.handlers.get(&kind).cloned().unwrap_or_default()
    }

    /// Queue an event (see [`Editor::emit`]).
    pub fn push(&mut self, e: Event) {
        self.queue.push_back(e);
    }

    /// Events waiting to be handled.
    pub fn pending(&self) -> usize {
        self.queue.len()
    }

    // ------------------------------------------------------------- timers

    fn add_timer(&mut self, name: &str, due: Instant, every: Option<Duration>, callback: Option<HookFn>) -> TimerId {
        self.next_timer += 1;
        let id = TimerId(self.next_timer);
        self.timers.push(Timer { id, name: name.to_string(), due, every, callback });
        id
    }

    /// A timer that fires once at `due`.
    pub fn run_at(&mut self, name: &str, due: Instant, callback: Option<HookFn>) -> TimerId {
        self.add_timer(name, due, None, callback)
    }

    /// A timer that first fires at `first` and then every `every` (at least 1 ms).
    pub fn run_every(&mut self, name: &str, first: Instant, every: Duration, callback: Option<HookFn>) -> TimerId {
        self.add_timer(name, first, Some(every.max(Duration::from_millis(1))), callback)
    }

    /// Cancel a timer. Returns whether it existed.
    pub fn cancel_timer(&mut self, id: TimerId) -> bool {
        let before = self.timers.len();
        self.timers.retain(|t| t.id != id);
        before != self.timers.len()
    }

    pub fn timers(&self) -> Vec<TimerInfo> {
        self.timers.iter().map(|t| TimerInfo { id: t.id, name: t.name.clone(), due: t.due, every: t.every }).collect()
    }

    /// When the earliest timer is due (the UI wakes up then).
    pub fn next_deadline(&self) -> Option<Instant> {
        self.timers.iter().map(|t| t.due).min()
    }

    /// Queue a [`Event::Timer`] for every timer due at `now`. One-shot timers
    /// are removed; repeating ones are rescheduled once (a timer that fell far
    /// behind fires once, not once per missed interval). Returns how many fired.
    pub fn fire_due(&mut self, now: Instant) -> usize {
        let mut due: Vec<(Instant, TimerId, String)> = Vec::new();
        let fired = &mut self.fired;
        self.timers.retain_mut(|t| {
            if t.due > now {
                return true;
            }
            due.push((t.due, t.id, t.name.clone()));
            match t.every {
                Some(every) => {
                    t.due += every;
                    if t.due <= now {
                        t.due = now + every;
                    }
                    true
                }
                None => {
                    if let Some(cb) = t.callback.take() {
                        fired.insert(t.id, cb);
                    }
                    false
                }
            }
        });
        due.sort_by_key(|(at, id, _)| (*at, *id));
        let n = due.len();
        for (_, id, name) in due {
            self.queue.push_back(Event::Timer { id, name });
        }
        n
    }

    /// The callback of a timer whose event is being handled.
    fn timer_callback(&mut self, id: TimerId) -> Option<HookFn> {
        self.fired.remove(&id).or_else(|| self.timers.iter().find(|t| t.id == id).and_then(|t| t.callback.clone()))
    }
}

impl Editor {
    /// Queue `e` for the hook handlers. They run when the queue is next
    /// drained (after the current command, or on the next UI tick), never
    /// inside the caller.
    pub fn emit(&mut self, e: Event) {
        self.hooks.push(e);
    }

    /// Handle queued events until the queue is empty (see the module docs for
    /// the loop cap). Calls made while already draining (a handler running a
    /// command) return 0 at once: the outer drain handles their events.
    /// Returns how many events were handled.
    pub fn run_hooks(&mut self) -> usize {
        if self.hooks.draining {
            return 0;
        }
        self.hooks.draining = true;
        let mut handled = 0;
        let mut errors: Vec<String> = Vec::new();
        while let Some(e) = self.hooks.queue.pop_front() {
            if handled == MAX_EVENTS_PER_DRAIN {
                let dropped = 1 + self.hooks.queue.len();
                self.hooks.queue.clear();
                errors.push(format!(
                    "Hooks emitted more than {MAX_EVENTS_PER_DRAIN} events in a row; dropped {dropped} (a handler loop?)"
                ));
                break;
            }
            handled += 1;
            let mut handlers = Vec::new();
            if let Event::Timer { id, name } = &e {
                if let Some(cb) = self.hooks.timer_callback(*id) {
                    handlers.push((name.clone(), cb));
                }
            }
            handlers.extend(self.hooks.handlers_for(e.kind()));
            for (name, f) in handlers {
                if let Err(msg) = f(self, &e) {
                    errors.push(format!("Error in {} hook {name}: {msg}", e.kind().name()));
                }
            }
        }
        self.hooks.draining = false;
        if !errors.is_empty() {
            self.message = Some(errors.join("; "));
        }
        handled
    }

    /// Fire the timers due at `now` and handle the resulting events. Returns
    /// how many timers fired.
    pub fn run_timers(&mut self, now: Instant) -> usize {
        let n = self.hooks.fire_due(now);
        self.run_hooks();
        n
    }

    /// One UI tick: read terminal output, fire due timers, handle queued
    /// events. Returns whether anything happened (the view should redraw).
    pub fn tick(&mut self, now: Instant) -> bool {
        let output = self.poll_pty();
        let fired = self.hooks.fire_due(now);
        let handled = self.run_hooks();
        output || fired > 0 || handled > 0
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::engine::{Args, Buffer};

    fn editor() -> Editor {
        let mut ed = Editor::new(Buffer::from_str("test", "hello"));
        ed.run_hooks(); // the initial BufferCreated
        ed
    }

    /// A handler that records every event it sees into `log`.
    fn recorder(log: &Rc<RefCell<Vec<String>>>, tag: &str) -> HookFn {
        let log = log.clone();
        let tag = tag.to_string();
        Rc::new(move |_, e| {
            log.borrow_mut().push(format!("{tag}:{}", e.to_json()));
            Ok(())
        })
    }

    fn log() -> Rc<RefCell<Vec<String>>> {
        Rc::new(RefCell::new(Vec::new()))
    }

    #[test]
    fn kinds_have_stable_names() {
        for k in EventKind::ALL {
            assert_eq!(EventKind::from_name(k.name()), Some(k));
        }
        assert_eq!(EventKind::from_name("no-such-hook"), None);
        let e = Event::AfterCommand { name: "forward-char".into() };
        assert_eq!(e.kind(), EventKind::AfterCommand);
        assert_eq!(e.to_json(), serde_json::json!({"event": "after-command", "name": "forward-char"}));
        assert_eq!(Event::KillEmacs.to_json(), serde_json::json!({"event": "kill-emacs"}));
        let exited = Event::ProcessExited { buf: 4, status: ExitStatus { code: 1, signal: None } };
        assert_eq!(exited.to_json()["event"], "process-exited");
        assert_eq!(exited.to_json()["status"]["code"], 1);
        // Every kind's serialized name matches `name()`.
        for k in EventKind::ALL {
            assert_eq!(serde_json::to_value(k).unwrap(), serde_json::json!(k.name()));
        }
    }

    #[test]
    fn add_replace_remove_and_order() {
        let mut h = Hooks::new();
        let l = log();
        h.add(EventKind::AfterCommand, "a", recorder(&l, "a"));
        h.add(EventKind::AfterCommand, "b", recorder(&l, "b"));
        h.add(EventKind::AfterCommand, "a", recorder(&l, "a2")); // replaces, keeps its place
        assert_eq!(h.handler_names(EventKind::AfterCommand), ["a", "b"]);
        assert!(h.add_named("buffer-killed", "k", recorder(&l, "k")).is_ok());
        assert!(h.add_named("bogus", "k", recorder(&l, "k")).unwrap_err().contains("Unknown hook"));
        assert_eq!(h.handler_names(EventKind::BufferKilled), ["k"]);
        assert!(h.remove(EventKind::AfterCommand, "a"));
        assert!(!h.remove(EventKind::AfterCommand, "a"));
        assert!(!h.remove(EventKind::Timer, "x"));
        assert_eq!(h.handler_names(EventKind::AfterCommand), ["b"]);
        let listed = h.list();
        assert_eq!(listed.len(), EventKind::ALL.len());
        assert_eq!(listed.iter().find(|i| i.kind == EventKind::BufferKilled).unwrap().handlers, ["k"]);
    }

    #[test]
    fn handlers_run_in_order_with_the_event() {
        let mut ed = editor();
        let l = log();
        ed.hooks.add(EventKind::AfterCommand, "first", recorder(&l, "1"));
        ed.hooks.add(EventKind::AfterCommand, "second", recorder(&l, "2"));
        ed.hooks.add(EventKind::AfterCommand, "first", recorder(&l, "1b"));
        ed.execute("forward-char", Args::default());
        assert_eq!(
            *l.borrow(),
            [
                r#"1b:{"event":"after-command","name":"forward-char"}"#,
                r#"2:{"event":"after-command","name":"forward-char"}"#
            ]
        );
    }

    #[test]
    fn emit_queues_without_running() {
        let mut ed = editor();
        let l = log();
        ed.hooks.add(EventKind::WindowConfigurationChanged, "w", recorder(&l, "w"));
        ed.emit(Event::WindowConfigurationChanged);
        ed.emit(Event::WindowConfigurationChanged);
        assert!(l.borrow().is_empty());
        assert_eq!(ed.hooks.pending(), 2);
        assert_eq!(ed.run_hooks(), 2);
        assert_eq!(l.borrow().len(), 2);
        assert_eq!(ed.run_hooks(), 0);
    }

    #[test]
    fn after_command_runs_even_when_the_command_fails() {
        let mut ed = editor();
        let l = log();
        ed.hooks.add(EventKind::AfterCommand, "x", recorder(&l, "x"));
        ed.execute("backward-char", Args::default()); // at the start: an error
        assert!(ed.message.is_some());
        assert_eq!(l.borrow().len(), 1);
        // Unknown commands don't run, so no hook.
        ed.execute("no-such-command", Args::default());
        assert_eq!(l.borrow().len(), 1);
    }

    /// A handler that records the events it sees.
    fn events(log: &Rc<RefCell<Vec<Event>>>) -> HookFn {
        let log = log.clone();
        Rc::new(move |_, e| {
            log.borrow_mut().push(e.clone());
            Ok(())
        })
    }

    #[test]
    fn buffer_created_and_killed() {
        let mut ed = Editor::new(Buffer::from_str("first", ""));
        let l = Rc::new(RefCell::new(Vec::new()));
        ed.hooks.add(EventKind::BufferCreated, "c", events(&l));
        ed.hooks.add(EventKind::BufferKilled, "k", events(&l));
        ed.run_hooks();
        assert_eq!(*l.borrow(), [Event::BufferCreated { buf: 1 }]);
        l.borrow_mut().clear();

        let id = ed.add_buffer(Buffer::from_str("second", ""));
        ed.kill_buffer(id);
        ed.run_hooks();
        assert_eq!(*l.borrow(), [Event::BufferCreated { buf: id }, Event::BufferKilled { buf: id, name: "second".into() }]);
        l.borrow_mut().clear();

        // Killing the last buffer makes a fresh *scratch*: created, then the old one killed.
        ed.execute("kill-buffer", Args::default());
        assert_eq!(
            *l.borrow(),
            [Event::BufferCreated { buf: ed.buf_id }, Event::BufferKilled { buf: 1, name: "first".into() }]
        );
        assert_eq!(ed.buf.name, "*scratch*");

        // Switching to a new named buffer creates it.
        l.borrow_mut().clear();
        ed.switch_to_buffer_named("third");
        ed.run_hooks();
        assert_eq!(l.borrow().len(), 1);
        // Killing an unknown id is a no-op: no event.
        l.borrow_mut().clear();
        ed.kill_buffer(9999);
        ed.run_hooks();
        assert!(l.borrow().is_empty());
    }

    #[test]
    fn after_command_on_every_path() {
        let mut ed = editor();
        let l = Rc::new(RefCell::new(Vec::new()));
        ed.hooks.add(EventKind::AfterCommand, "log", events(&l));
        ed.execute("read-only-mode", Args::default());
        ed.execute("undo", Args::default()); // refused early in a read-only buffer
        ed.execute("self-insert-command", Args { text: Some("x".into()), ..Default::default() }); // refused edit
        assert_eq!(ed.rope().to_string(), "hello");
        let names: Vec<String> = l
            .borrow()
            .iter()
            .map(|e| match e {
                Event::AfterCommand { name } => name.clone(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(names, ["read-only-mode", "undo", "self-insert-command"]);
    }

    #[test]
    fn handlers_can_emit_and_run_commands() {
        let mut ed = editor();
        let l = log();
        // A command handler that runs another command: its AfterCommand is
        // handled in the same drain, after the current one.
        ed.hooks.add(
            EventKind::AfterCommand,
            "chain",
            Rc::new(|ed, e| {
                if let Event::AfterCommand { name } = e {
                    if name == "end-of-buffer" {
                        ed.execute("backward-char", Args::default());
                    }
                }
                Ok(())
            }),
        );
        ed.hooks.add(EventKind::AfterCommand, "log", recorder(&l, "log"));
        ed.execute("end-of-buffer", Args::default());
        assert_eq!(ed.primary_cursor().point, 4);
        let got = l.borrow().clone();
        assert_eq!(got.len(), 2);
        assert!(got[0].contains("end-of-buffer") && got[1].contains("backward-char"), "{got:?}");
    }

    #[test]
    fn handler_loops_are_capped() {
        let mut ed = editor();
        let count = Rc::new(RefCell::new(0usize));
        let c = count.clone();
        ed.hooks.add(
            EventKind::WindowConfigurationChanged,
            "loop",
            Rc::new(move |ed, _| {
                *c.borrow_mut() += 1;
                ed.emit(Event::WindowConfigurationChanged);
                ed.emit(Event::WindowConfigurationChanged);
                Ok(())
            }),
        );
        ed.emit(Event::WindowConfigurationChanged);
        assert_eq!(ed.run_hooks(), MAX_EVENTS_PER_DRAIN);
        assert_eq!(*count.borrow(), MAX_EVENTS_PER_DRAIN);
        assert_eq!(ed.hooks.pending(), 0);
        assert!(ed.message.as_deref().unwrap().contains("dropped"), "{:?}", ed.message);
        // The editor carries on normally afterwards.
        ed.hooks.remove(EventKind::WindowConfigurationChanged, "loop");
        ed.execute("forward-char", Args::default());
        assert_eq!(ed.primary_cursor().point, 1);
    }

    #[test]
    fn errors_are_reported_and_other_handlers_still_run() {
        let mut ed = editor();
        let l = log();
        ed.hooks.add(EventKind::AfterCommand, "bad", Rc::new(|_, _| Err("boom".into())));
        ed.hooks.add(EventKind::AfterCommand, "good", recorder(&l, "good"));
        ed.execute("forward-char", Args::default());
        assert_eq!(l.borrow().len(), 1);
        assert_eq!(ed.message.as_deref(), Some("Error in after-command hook bad: boom"));
    }

    #[test]
    fn handler_can_remove_itself() {
        let mut ed = editor();
        let count = Rc::new(RefCell::new(0));
        let c = count.clone();
        ed.hooks.add(
            EventKind::AfterCommand,
            "once",
            Rc::new(move |ed, _| {
                *c.borrow_mut() += 1;
                ed.hooks.remove(EventKind::AfterCommand, "once");
                Ok(())
            }),
        );
        ed.execute("forward-char", Args::default());
        ed.execute("forward-char", Args::default());
        assert_eq!(*count.borrow(), 1);
        assert!(ed.hooks.handler_names(EventKind::AfterCommand).is_empty());
    }

    #[test]
    fn nested_run_hooks_is_not_reentrant() {
        let mut ed = editor();
        let inner = Rc::new(RefCell::new(Vec::new()));
        let i = inner.clone();
        ed.hooks.add(
            EventKind::BufferKilled,
            "nested",
            Rc::new(move |ed, _| {
                ed.emit(Event::KillEmacs);
                i.borrow_mut().push(ed.run_hooks()); // must not handle KillEmacs here
                Ok(())
            }),
        );
        let l = log();
        ed.hooks.add(EventKind::KillEmacs, "k", recorder(&l, "k"));
        ed.emit(Event::BufferKilled { buf: 7, name: "x".into() });
        assert_eq!(ed.run_hooks(), 2);
        assert_eq!(*inner.borrow(), [0]);
        assert_eq!(l.borrow().len(), 1);
    }

    #[test]
    fn one_shot_and_repeating_timers() {
        let mut ed = editor();
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let l = log();
        ed.hooks.add(EventKind::Timer, "all", recorder(&l, "all"));
        let fired = Rc::new(RefCell::new(Vec::new()));
        let f = fired.clone();
        let once = ed.hooks.run_at(
            "once",
            t0 + ms(100),
            Some(Rc::new(move |_, e| {
                f.borrow_mut().push(e.clone());
                Ok(())
            })),
        );
        let every = ed.hooks.run_every("every", t0 + ms(50), ms(50), None);
        assert_eq!(ed.hooks.next_deadline(), Some(t0 + ms(50)));

        assert_eq!(ed.run_timers(t0 + ms(10)), 0);
        assert_eq!(ed.run_timers(t0 + ms(50)), 1);
        assert_eq!(ed.hooks.next_deadline(), Some(t0 + ms(100)));
        // Both due at 100: in due order, then by id.
        assert_eq!(ed.run_timers(t0 + ms(100)), 2);
        assert_eq!(*fired.borrow(), [Event::Timer { id: once, name: "once".into() }]);
        assert_eq!(ed.hooks.timers().len(), 1);
        assert_eq!(ed.hooks.next_deadline(), Some(t0 + ms(150)));
        let got = l.borrow().clone();
        assert_eq!(got.len(), 3);
        assert!(got[1].contains(r#""name":"once""#) && got[2].contains(r#""name":"every""#), "{got:?}");

        // Far behind: fires once and is rescheduled from now.
        assert_eq!(ed.run_timers(t0 + ms(1000)), 1);
        assert_eq!(ed.hooks.next_deadline(), Some(t0 + ms(1050)));
        assert!(ed.hooks.cancel_timer(every));
        assert!(!ed.hooks.cancel_timer(every));
        assert!(!ed.hooks.cancel_timer(once));
        assert_eq!(ed.hooks.next_deadline(), None);
        assert_eq!(ed.run_timers(t0 + ms(5000)), 0);
    }

    #[test]
    fn timer_callbacks_can_reschedule_and_cancel() {
        let mut ed = editor();
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let count = Rc::new(RefCell::new(0));
        let now = Rc::new(std::cell::Cell::new(t0));
        // A one-shot timer whose callback schedules the next one (a chain).
        fn chain(c: Rc<RefCell<i32>>, now: Rc<std::cell::Cell<Instant>>) -> HookFn {
            Rc::new(move |ed, _| {
                *c.borrow_mut() += 1;
                if *c.borrow() < 3 {
                    let at = now.get() + Duration::from_millis(10);
                    ed.hooks.run_at("chain", at, Some(chain(c.clone(), now.clone())));
                }
                Ok(())
            })
        }
        ed.hooks.run_at("chain", t0, Some(chain(count.clone(), now.clone())));
        for step in 0..5u64 {
            now.set(t0 + ms(10 * step));
            ed.run_timers(now.get());
        }
        assert_eq!(*count.borrow(), 3);
        assert!(ed.hooks.timers().is_empty());

        // A repeating timer that cancels itself on its second run.
        let runs = Rc::new(RefCell::new(0));
        let r = runs.clone();
        ed.hooks.run_every(
            "self-cancel",
            t0,
            ms(1),
            Some(Rc::new(move |ed, e| {
                *r.borrow_mut() += 1;
                if *r.borrow() == 2 {
                    if let Event::Timer { id, .. } = e {
                        ed.hooks.cancel_timer(*id);
                    }
                }
                Ok(())
            })),
        );
        for step in 0..5u64 {
            ed.run_timers(t0 + ms(step));
        }
        assert_eq!(*runs.borrow(), 2);
    }

    #[test]
    fn tick_reports_activity() {
        let mut ed = editor();
        let t0 = Instant::now();
        assert!(!ed.tick(t0));
        ed.emit(Event::KillEmacs);
        assert!(ed.tick(t0));
        ed.hooks.run_at("t", t0 + Duration::from_millis(5), None);
        assert!(!ed.tick(t0));
        assert!(ed.tick(t0 + Duration::from_millis(5)));
    }
}
