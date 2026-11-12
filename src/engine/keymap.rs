//! Keys, keymaps and the key dispatcher (prefix keys, prefix arguments,
//! shift-translation, read-char commands). Key names follow Emacs notation:
//! `C-` control, `M-` meta (Option), `S-` shift, `s-` super (Cmd), e.g. `C-x C-s`.

use std::collections::{HashMap, HashSet};

use super::commands::{self, flags};
use super::editor::{Args, RawArg};

#[derive(Clone, Debug, Default)]
pub struct Key {
    pub ctrl: bool,
    pub meta: bool,
    pub shift: bool,
    pub sup: bool,
    /// Emacs-style key name: `a`, `<`, `SPC`, `RET`, `TAB`, `DEL`, `ESC`, `<up>`, `<delete>`…
    pub name: String,
    /// Text the key would type (for self-insert), if any.
    pub text: Option<String>,
}

impl Key {
    pub fn canonical(&self) -> String {
        let mut s = String::new();
        if self.ctrl { s.push_str("C-"); }
        if self.meta { s.push_str("M-"); }
        if self.shift { s.push_str("S-"); }
        if self.sup { s.push_str("s-"); }
        s.push_str(&self.name);
        s
    }

    /// Parse Emacs notation (`C-M-x`, `S-<right>`, `a`) into a key.
    pub fn parse(s: &str) -> Key {
        let mut k = Key::default();
        let mut rest = s;
        loop {
            if let Some(r) = rest.strip_prefix("C-") { k.ctrl = true; rest = r; continue; }
            if let Some(r) = rest.strip_prefix("M-") { k.meta = true; rest = r; continue; }
            if let Some(r) = rest.strip_prefix("S-") { k.shift = true; rest = r; continue; }
            if let Some(r) = rest.strip_prefix("s-") { k.sup = true; rest = r; continue; }
            break;
        }
        // A bare "-" after modifiers (e.g. "M--") is the key itself.
        if rest.is_empty() && s.ends_with('-') {
            rest = "-";
        }
        k.name = rest.to_string();
        if k.is_plain() && rest.chars().count() == 1 {
            k.text = Some(if k.shift { rest.to_uppercase() } else { rest.to_string() });
        } else if k.is_plain() && rest == "SPC" {
            k.text = Some(" ".into());
        }
        k
    }

    fn unshifted(&self) -> Option<String> {
        self.shift.then(|| Key { shift: false, ..self.clone() }.canonical())
    }

    /// The character this key stands for when a command reads a char.
    fn as_char(&self) -> Option<String> {
        if self.ctrl && self.name.len() == 1 {
            let c = self.name.chars().next()?.to_ascii_lowercase();
            if c.is_ascii_lowercase() {
                return Some(((c as u8 - b'a' + 1) as char).to_string());
            }
        }
        match self.name.as_str() {
            "RET" => return Some("\n".into()),
            "TAB" => return Some("\t".into()),
            "SPC" => return Some(" ".into()),
            _ => {}
        }
        if let Some(t) = &self.text {
            if !t.is_empty() && !t.chars().any(char::is_control) {
                return Some(t.clone());
            }
        }
        (self.name.chars().count() == 1).then(|| self.name.clone())
    }

    /// Text typing this key inserts into a query (plain printable keys only).
    pub fn insertable_text(&self) -> Option<&str> {
        if !self.is_plain() {
            return None;
        }
        self.text.as_deref().filter(|t| !t.is_empty() && !t.chars().any(char::is_control))
    }

    fn is_plain(&self) -> bool {
        !self.ctrl && !self.meta && !self.sup
    }
}

/// Key sequences (canonical Emacs notation, space-separated) to command names.
/// Built from the JSON configuration (see `config.rs`).
#[derive(Default)]
pub struct Keymap {
    map: HashMap<Vec<String>, &'static str>,
    prefixes: HashSet<Vec<String>>,
}

impl Keymap {
    pub fn new() -> Self {
        Self { map: HashMap::new(), prefixes: HashSet::new() }
    }

    pub fn bind(&mut self, keys: &str, command: &'static str) {
        let seq: Vec<String> = keys.split_whitespace().map(String::from).collect();
        for i in 1..seq.len() {
            self.prefixes.insert(seq[..i].to_vec());
        }
        self.map.insert(seq, command);
    }

    pub fn lookup(&self, seq: &[String]) -> Option<&'static str> {
        self.map.get(seq).copied()
    }

    pub fn is_prefix(&self, seq: &[String]) -> bool {
        self.prefixes.contains(seq)
    }

    /// All key sequences bound to `command`, for help/discoverability.
    pub fn keys_for(&self, command: &str) -> Vec<String> {
        let mut v: Vec<String> = self.map.iter().filter(|(_, c)| **c == command).map(|(k, _)| k.join(" ")).collect();
        v.sort_by_key(|k| (k.len(), k.clone()));
        v
    }
}

/// What a key sequence means in a stack of keymaps (highest priority first):
/// the first keymap that binds the sequence, or a longer one starting with
/// it, decides.
#[derive(Debug, PartialEq)]
pub enum Binding {
    Command(&'static str),
    Prefix,
    Unbound,
}

pub fn lookup_in(maps: &[&Keymap], seq: &[String]) -> Binding {
    for m in maps {
        if let Some(c) = m.lookup(seq) {
            return Binding::Command(c);
        }
        if m.is_prefix(seq) {
            return Binding::Prefix;
        }
    }
    Binding::Unbound
}

/// The first key sequence that runs `command` in a stack of keymaps (one
/// shadowed by a higher keymap doesn't count).
pub fn where_is(maps: &[&Keymap], command: &str) -> Option<String> {
    maps.iter().flat_map(|m| m.keys_for(command)).find(|keys| {
        let seq: Vec<String> = keys.split(' ').map(String::from).collect();
        matches!(lookup_in(maps, &seq), Binding::Command(c) if c == command)
    })
}

/// Commands the dispatcher handles itself (prefix arguments); they can be
/// bound in the global keymap like any command.
pub const PREFIX_ARG_COMMANDS: [&str; 3] = ["universal-argument", "digit-argument", "negative-argument"];

/// What the view should do after a key.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Run(&'static str, Args),
    /// More keys needed; echo this.
    Pending(String),
    Undefined(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ArgState {
    Universal(u32),
    Digits { neg: bool, value: Option<i64> },
}

/// Turns keys into commands. The keymaps are passed in with each key, since
/// they depend on the current buffer's modes.
#[derive(Default)]
pub struct Dispatcher {
    pending: Vec<String>,
    arg: Option<ArgState>,
    read_char: Option<(&'static str, Args)>,
}

impl Dispatcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Run a command chosen by name (M-x): if it reads a character, wait for it.
    pub fn invoke(&mut self, cmd: &'static str, args: Args) -> Outcome {
        self.reset();
        if commands::lookup(cmd).is_some_and(|c| c.flags & flags::READ_CHAR != 0) {
            self.read_char = Some((cmd, args));
            return Outcome::Pending(format!("{cmd}: "));
        }
        Outcome::Run(cmd, args)
    }

    fn take_args(&mut self) -> Args {
        let raw = match self.arg.take() {
            None => None,
            Some(ArgState::Universal(k)) => Some(RawArg::Universal(k)),
            Some(ArgState::Digits { neg, value: Some(v) }) => Some(RawArg::Number(if neg { -v } else { v })),
            Some(ArgState::Digits { neg: true, value: None }) => Some(RawArg::Minus),
            Some(ArgState::Digits { neg: false, value: None }) => Some(RawArg::Universal(1)),
        };
        Args::with_raw(raw)
    }

    fn arg_echo(&self) -> String {
        match self.arg {
            None => String::new(),
            Some(ArgState::Universal(k)) => vec!["C-u"; k as usize].join(" ") + "-",
            Some(ArgState::Digits { neg, value }) => {
                format!("C-u {}{}-", if neg { "-" } else { "" }, value.map(|v| v.to_string()).unwrap_or_default())
            }
        }
    }

    fn echo(&self) -> String {
        let a = self.arg_echo();
        let p = self.pending.join(" ");
        match (a.is_empty(), p.is_empty()) {
            (true, _) => format!("{p}-"),
            (false, true) => a,
            (false, false) => format!("{} {p}-", a.trim_end_matches('-')),
        }
    }

    /// Whether a multi-key sequence or prefix argument is in progress.
    pub fn is_pending(&self) -> bool {
        !self.pending.is_empty() || self.arg.is_some() || self.read_char.is_some()
    }

    pub fn reset(&mut self) {
        self.pending.clear();
        self.arg = None;
        self.read_char = None;
    }

    /// Handle a key, looking it up in `maps` (highest priority first).
    pub fn dispatch(&mut self, key: &Key, maps: &[&Keymap]) -> Outcome {
        let k = key.canonical();

        if let Some((cmd, mut args)) = self.read_char.take() {
            if lookup_in(maps, std::slice::from_ref(&k)) == Binding::Command("keyboard-quit") {
                return Outcome::Run("keyboard-quit", Args::default());
            }
            return match key.as_char() {
                Some(c) => {
                    args.text = Some(c);
                    Outcome::Run(cmd, args)
                }
                None => Outcome::Undefined(k),
            };
        }

        // Digits and '-' continue a prefix argument.
        if self.pending.is_empty() && key.is_plain() && !key.shift {
            if let Some(st) = self.arg {
                if let Some(d) = key.name.chars().next().and_then(|c| c.to_digit(10)).filter(|_| key.name.len() == 1) {
                    let (neg, value) = match st {
                        ArgState::Universal(_) => (false, None),
                        ArgState::Digits { neg, value } => (neg, value),
                    };
                    let v = value.unwrap_or(0).saturating_mul(10).saturating_add(d as i64);
                    self.arg = Some(ArgState::Digits { neg, value: Some(v) });
                    return Outcome::Pending(self.echo());
                }
                if key.name == "-" && matches!(st, ArgState::Universal(_) | ArgState::Digits { value: None, .. }) {
                    self.arg = Some(ArgState::Digits { neg: true, value: None });
                    return Outcome::Pending(self.echo());
                }
            }
        }

        self.pending.push(k.clone());
        let mut found = match lookup_in(maps, &self.pending) {
            Binding::Command(c) => Some((c, false)),
            Binding::Prefix => return Outcome::Pending(self.echo()),
            Binding::Unbound => None,
        };
        // Shift-translation: S-<right> runs <right>'s command with shift selection.
        if found.is_none() {
            if let Some(unshifted) = key.unshifted() {
                let mut seq = self.pending.clone();
                *seq.last_mut().unwrap() = unshifted;
                if let Binding::Command(c) = lookup_in(maps, &seq) {
                    found = Some((c, true));
                }
            }
        }
        let seq = std::mem::take(&mut self.pending);

        let Some((cmd, shift)) = found else {
            if seq.len() == 1 && key.is_plain() {
                if let Some(t) = key.text.clone().filter(|t| !t.is_empty() && !t.chars().any(char::is_control)) {
                    let mut args = self.take_args();
                    args.text = Some(t);
                    return Outcome::Run("self-insert-command", args);
                }
            }
            self.arg = None;
            return Outcome::Undefined(seq.join(" "));
        };

        match cmd {
            "universal-argument" => {
                self.arg = Some(match self.arg {
                    Some(ArgState::Universal(n)) => ArgState::Universal(n + 1),
                    _ => ArgState::Universal(1),
                });
                Outcome::Pending(self.echo())
            }
            "digit-argument" => {
                let d = k.chars().last().and_then(|c| c.to_digit(10)).unwrap_or(0) as i64;
                let neg = matches!(self.arg, Some(ArgState::Digits { neg: true, .. }));
                let prev = match self.arg {
                    Some(ArgState::Digits { value, .. }) => value,
                    _ => None,
                };
                self.arg = Some(ArgState::Digits { neg, value: Some(prev.unwrap_or(0) * 10 + d) });
                Outcome::Pending(self.echo())
            }
            "negative-argument" => {
                self.arg = Some(ArgState::Digits { neg: true, value: None });
                Outcome::Pending(self.echo())
            }
            _ => {
                let mut args = self.take_args();
                args.shift = shift;
                let reads_char = commands::lookup(cmd).is_some_and(|c| c.flags & flags::READ_CHAR != 0);
                if reads_char {
                    self.read_char = Some((cmd, args));
                    return Outcome::Pending(format!("{}: ", cmd));
                }
                Outcome::Run(cmd, args)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> Key {
        Key::parse(s)
    }

    fn global() -> Keymap {
        super::super::config::Config::builtin().modes.global
    }

    fn run_in(d: &mut Dispatcher, maps: &[&Keymap], keys: &str) -> Outcome {
        let mut last = Outcome::Pending(String::new());
        for k in keys.split_whitespace() {
            last = d.dispatch(&key(k), maps);
        }
        last
    }

    #[test]
    fn sequences_and_args() {
        let g = global();
        let maps = [&g];
        let mut d = Dispatcher::new();
        let run = |d: &mut Dispatcher, keys: &str| run_in(d, &maps, keys);
        assert!(matches!(run(&mut d, "C-x C-s"), Outcome::Run("save-buffer", _)));
        assert!(matches!(run(&mut d, "C-x"), Outcome::Pending(ref s) if s == "C-x-"));
        assert!(matches!(run(&mut d, "C-z"), Outcome::Undefined(ref s) if s == "C-x C-z"));

        let Outcome::Run(c, a) = run(&mut d, "C-u C-u C-f") else { panic!() };
        assert_eq!((c, a.n()), ("forward-char", 16));
        let Outcome::Run(_, a) = run(&mut d, "C-u 1 2 C-f") else { panic!() };
        assert_eq!(a.n(), 12);
        let Outcome::Run(_, a) = run(&mut d, "M-3 M-4 C-n") else { panic!() };
        assert_eq!(a.n(), 34);
        let Outcome::Run(_, a) = run(&mut d, "M-- M-d") else { panic!() };
        assert_eq!(a.n(), -1);
        let Outcome::Run(c, a) = run(&mut d, "C-u 3 x") else { panic!() };
        assert_eq!((c, a.n(), a.text.as_deref()), ("self-insert-command", 3, Some("x")));
    }

    #[test]
    fn shift_translation_and_read_char() {
        let g = global();
        let maps = [&g];
        let mut d = Dispatcher::new();
        let run = |d: &mut Dispatcher, keys: &str| run_in(d, &maps, keys);
        let Outcome::Run(c, a) = run(&mut d, "S-<right>") else { panic!() };
        assert_eq!((c, a.shift), ("forward-char", true));
        let Outcome::Run(c, a) = run(&mut d, "S-a") else { panic!() };
        assert_eq!((c, a.text.as_deref()), ("self-insert-command", Some("A")));
        assert!(matches!(run(&mut d, "M-z"), Outcome::Pending(_)));
        let Outcome::Run(c, a) = run(&mut d, "e") else { panic!() };
        assert_eq!((c, a.text.as_deref()), ("zap-to-char", Some("e")));
    }

    #[test]
    fn layers_shadow_and_share_prefixes() {
        let g = global();
        let mut mode = Keymap::new();
        mode.bind("C-c C-c", "fill-paragraph");
        mode.bind("C-f", "forward-word");
        let maps = [&mode, &g];
        let mut d = Dispatcher::new();
        assert!(matches!(run_in(&mut d, &maps, "C-f"), Outcome::Run("forward-word", _)));
        assert!(matches!(run_in(&mut d, &maps, "C-c C-c"), Outcome::Run("fill-paragraph", _)));
        // A prefix the mode map uses still reaches the global bindings under it.
        assert!(matches!(run_in(&mut d, &maps, "C-c C-<"), Outcome::Run("mc/mark-all-like-this", _)));
        assert!(matches!(run_in(&mut d, &maps, "C-b"), Outcome::Run("backward-char", _)));
        assert_eq!(where_is(&maps, "forward-char").as_deref(), Some("<right>"));
        assert_eq!(where_is(&maps, "forward-word").as_deref(), Some("C-f"));
        assert!(matches!(d.invoke("zap-to-char", Args::default()), Outcome::Pending(_)));
        assert!(matches!(run_in(&mut d, &maps, "x"), Outcome::Run("zap-to-char", ref a) if a.text.as_deref() == Some("x")));
    }
}
