//! Program status reports: OSC 7501, Herdr's open "Program Status
//! Protocol". A program that knows what it is doing says so on its output:
//!
//! ```text
//! ESC ] 7501 ; state=blocked:app=claude ESC \      (or BEL instead of ESC \)
//! ```
//!
//! The payload is `key=value` fields separated by `:`; `state` is the one
//! that matters (`idle`, `working`, `blocked`, `done`, ...; empty or missing
//! clears a previous report). The terminal emulator ignores unknown OSCs, so
//! [`StatusScanner`] looks for these in the raw bytes, keeping its state
//! across chunks (a report can be split anywhere).

/// One report.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProgramStatus {
    /// `state`, or `""` (clears an earlier report).
    pub state: String,
    /// `app`, if given: what the program says it is (`claude`, `codex`, ...).
    pub app: Option<String>,
    /// Every field, in order (`state` and `app` included).
    pub fields: Vec<(String, String)>,
}

impl ProgramStatus {
    /// Parse a payload (`state=x:app=y`). Fields without `=` are ignored.
    pub fn parse(payload: &str) -> Self {
        let fields: Vec<(String, String)> = payload
            .split(':')
            .filter_map(|f| f.split_once('='))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .collect();
        let get = |key: &str| fields.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
        Self { state: get("state").unwrap_or_default(), app: get("app").filter(|a| !a.is_empty()), fields }
    }
}

/// The OSC number this scanner reports.
const PREFIX: &[u8] = b"\x1b]7501;";

/// Longest payload kept; a longer one is not a status report and is dropped.
const MAX_PAYLOAD: usize = 512;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum State {
    /// Looking for `ESC`; `n` bytes of [`PREFIX`] matched so far.
    #[default]
    Ground,
    Prefix(usize),
    /// Inside a payload.
    Payload,
    /// Inside a payload, just after an `ESC` (the start of `ESC \`).
    PayloadEsc,
    /// A payload too long to be a report: skip to its terminator.
    Skip,
    SkipEsc,
}

/// Finds OSC 7501 reports in a byte stream (see the module docs).
#[derive(Clone, Debug, Default)]
pub struct StatusScanner {
    state: State,
    payload: Vec<u8>,
}

impl StatusScanner {
    /// The reports completed in `bytes`, in order.
    pub fn scan(&mut self, bytes: &[u8]) -> Vec<ProgramStatus> {
        let mut out = Vec::new();
        // Fast path: nothing pending and no ESC in this chunk.
        if self.state == State::Ground && !bytes.contains(&0x1b) {
            return out;
        }
        for &b in bytes {
            self.state = match std::mem::take(&mut self.state) {
                State::Ground | State::Prefix(0) => {
                    if b == 0x1b {
                        State::Prefix(1)
                    } else {
                        State::Ground
                    }
                }
                State::Prefix(n) => {
                    if b == PREFIX[n] {
                        if n + 1 == PREFIX.len() {
                            self.payload.clear();
                            State::Payload
                        } else {
                            State::Prefix(n + 1)
                        }
                    } else if b == 0x1b {
                        State::Prefix(1)
                    } else {
                        State::Ground
                    }
                }
                State::Payload => match b {
                    0x07 => {
                        out.push(self.finish());
                        State::Ground
                    }
                    0x1b => State::PayloadEsc,
                    // Other controls end an OSC without completing it (CAN, SUB).
                    0x18 | 0x1a => State::Ground,
                    _ if self.payload.len() >= MAX_PAYLOAD => State::Skip,
                    _ => {
                        self.payload.push(b);
                        State::Payload
                    }
                },
                State::PayloadEsc => {
                    if b == b'\\' {
                        out.push(self.finish());
                        State::Ground
                    } else if b == b']' {
                        // An unterminated OSC followed by a new one.
                        State::Prefix(2)
                    } else {
                        State::Ground
                    }
                }
                State::Skip => match b {
                    0x07 | 0x18 | 0x1a => State::Ground,
                    0x1b => State::SkipEsc,
                    _ => State::Skip,
                },
                State::SkipEsc => State::Ground,
            };
        }
        out
    }

    fn finish(&mut self) -> ProgramStatus {
        let s = ProgramStatus::parse(&String::from_utf8_lossy(&self.payload));
        self.payload.clear();
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn states(r: Vec<ProgramStatus>) -> Vec<String> {
        r.into_iter().map(|s| s.state).collect()
    }

    #[test]
    fn parses_payloads() {
        let s = ProgramStatus::parse("state=blocked:app=claude:message=approve edit");
        assert_eq!(s.state, "blocked");
        assert_eq!(s.app.as_deref(), Some("claude"));
        assert_eq!(s.fields.len(), 3);
        assert_eq!(ProgramStatus::parse("app=x").state, "");
        assert_eq!(ProgramStatus::parse("state=idle:app=").app, None);
        assert_eq!(ProgramStatus::parse("junk:state=working").state, "working");
    }

    #[test]
    fn finds_reports_with_either_terminator() {
        let mut sc = StatusScanner::default();
        let r = sc.scan(b"hello\x1b]7501;state=working\x07 and \x1b]7501;state=idle:app=codex\x1b\\ bye");
        assert_eq!(states(r.clone()), ["working", "idle"]);
        assert_eq!(r[1].app.as_deref(), Some("codex"));
    }

    #[test]
    fn reports_split_anywhere_are_found() {
        let input: &[u8] = b"ab\x1b[1m\x1b]7501;state=blocked:app=claude\x1b\\cd";
        for split in 0..input.len() {
            let mut sc = StatusScanner::default();
            let mut r = sc.scan(&input[..split]);
            r.extend(sc.scan(&input[split..]));
            assert_eq!(states(r), ["blocked"], "split at {split}");
        }
        // Byte by byte.
        let mut sc = StatusScanner::default();
        let r: Vec<_> = input.iter().flat_map(|b| sc.scan(std::slice::from_ref(b))).collect();
        assert_eq!(states(r), ["blocked"]);
    }

    #[test]
    fn other_sequences_are_ignored() {
        let mut sc = StatusScanner::default();
        assert!(sc.scan(b"\x1b]0;title\x07\x1b]750;state=x\x07\x1b]75011;state=x\x07\x1b[7501m").is_empty());
        // ESC ESC ] 7501 still starts a report.
        assert_eq!(states(sc.scan(b"\x1b\x1b]7501;state=done\x07")), ["done"]);
        // An unterminated report interrupted by another one.
        assert_eq!(states(sc.scan(b"\x1b]7501;state=a\x1b]7501;state=b\x07")), ["b"]);
        // Cancelled.
        assert!(sc.scan(b"\x1b]7501;state=a\x18 state=b\x07").is_empty());
    }

    #[test]
    fn overlong_payloads_are_dropped() {
        let mut sc = StatusScanner::default();
        let mut long = b"\x1b]7501;state=".to_vec();
        long.extend(std::iter::repeat_n(b'x', MAX_PAYLOAD * 2));
        long.extend(b"\x07\x1b]7501;state=idle\x07");
        assert_eq!(states(sc.scan(&long)), ["idle"]);
    }
}
