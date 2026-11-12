use super::*;

/// A host with one fake terminal (id 1) that records what scripts did.
#[derive(Default)]
struct TestHost {
    screen: Vec<String>,
    process: Option<String>,
    env: HashMap<String, String>,
    sent: Vec<String>,
    keys: Vec<Vec<String>>,
    agents: HashMap<TerminalId, (String, String)>,
    log: Vec<String>,
}

impl TestHost {
    fn known(&self, id: TerminalId) -> bool {
        id == 1
    }
}

impl Host for TestHost {
    fn terminal_screen(&mut self, id: TerminalId, rows: usize) -> Option<Vec<String>> {
        let skip = self.screen.len().saturating_sub(rows);
        self.known(id).then(|| self.screen[skip..].to_vec())
    }
    fn terminal_process(&mut self, id: TerminalId) -> Option<String> {
        self.process.clone().filter(|_| self.known(id))
    }
    fn terminal_env(&mut self, id: TerminalId, key: &str) -> Option<String> {
        self.env.get(key).cloned().filter(|_| self.known(id))
    }
    fn terminal_send(&mut self, id: TerminalId, text: &str) -> Result<(), String> {
        if !self.known(id) {
            return Err(format!("no terminal {id}"));
        }
        self.sent.push(text.to_string());
        Ok(())
    }
    fn terminal_keys(&mut self, id: TerminalId, keys: &[String]) -> Result<(), String> {
        if !self.known(id) {
            return Err(format!("no terminal {id}"));
        }
        self.keys.push(keys.to_vec());
        Ok(())
    }
    fn agent_set(&mut self, id: TerminalId, kind: &str, state: &str) -> Result<(), String> {
        if !matches!(state, "idle" | "working" | "blocked" | "done" | "unknown") {
            return Err(format!("bad state {state}"));
        }
        self.agents.insert(id, (kind.to_string(), state.to_string()));
        Ok(())
    }
    fn agent_clear(&mut self, id: TerminalId) {
        self.agents.remove(&id);
    }
    fn log(&mut self, msg: &str) {
        self.log.push(msg.to_string());
    }
}

fn setup() -> (ScriptHost, TestHost) {
    let mut h = TestHost::default();
    let (s, errors) = ScriptHost::with_builtins(&mut h);
    assert!(errors.is_empty(), "{errors:#?}");
    (s, h)
}

fn eval(s: &mut ScriptHost, h: &mut TestHost, src: &str) -> SteelVal {
    s.eval(h, src).unwrap_or_else(|e| panic!("{src}: {e}"))
}

fn string(v: SteelVal) -> String {
    match v {
        SteelVal::StringV(s) => s.to_string(),
        other => panic!("expected a string, got {other}"),
    }
}

#[test]
fn builtins_load_cleanly() {
    let (s, _) = setup();
    assert!(!s.handles("output"));
}

#[test]
fn scripts_query_the_host() {
    let (mut s, mut h) = setup();
    h.screen = vec!["one".into(), "two".into(), "> ".into()];
    h.process = Some("claude".into());
    h.env.insert("YMACS_AGENT".into(), "codex".into());

    assert_eq!(eval(&mut s, &mut h, "(terminal-screen 1 2)").to_string(), r#"("two" "> ")"#);
    assert_eq!(eval(&mut s, &mut h, "(terminal-screen 9 2)"), SteelVal::BoolV(false));
    assert_eq!(string(eval(&mut s, &mut h, "(terminal-process 1)")), "claude");
    assert_eq!(string(eval(&mut s, &mut h, r#"(terminal-env 1 "YMACS_AGENT")"#)), "codex");
    assert_eq!(eval(&mut s, &mut h, r#"(terminal-env 1 "NOPE")"#), SteelVal::BoolV(false));
}

#[test]
fn scripts_act_through_the_host() {
    let (mut s, mut h) = setup();
    eval(&mut s, &mut h, r#"(terminal-send 1 "hello")"#);
    eval(&mut s, &mut h, r#"(terminal-keys 1 "enter")"#);
    eval(&mut s, &mut h, r#"(terminal-keys 1 '(esc "ctrl+c"))"#);
    eval(&mut s, &mut h, "(agent-set! 1 'claude 'working)");
    eval(&mut s, &mut h, r#"(log "hi")"#);
    assert_eq!(h.sent, ["hello"]);
    assert_eq!(h.keys, [vec!["enter".to_string()], vec!["esc".to_string(), "ctrl+c".to_string()]]);
    assert_eq!(h.agents[&1], ("claude".to_string(), "working".to_string()));
    assert_eq!(h.log, ["hi"]);

    eval(&mut s, &mut h, "(agent-clear! 1)");
    assert!(h.agents.is_empty());
}

#[test]
fn host_errors_become_scheme_errors() {
    let (mut s, mut h) = setup();
    let e = s.eval(&mut h, r#"(terminal-send 7 "x")"#).unwrap_err();
    assert!(e.contains("no terminal 7"), "{e}");
    let e = s.eval(&mut h, "(agent-set! 1 'claude 'sleepy)").unwrap_err();
    assert!(e.contains("bad state sleepy"), "{e}");
    let e = s.eval(&mut h, "(terminal-send -1 \"x\")").unwrap_err();
    assert!(e.contains("bad terminal id"), "{e}");
    // A Scheme error can be caught like any other.
    let v = eval(&mut s, &mut h, r#"(with-handler (lambda (e) "caught") (terminal-send 7 "x"))"#);
    assert_eq!(string(v), "caught");
}

#[test]
fn load_reports_errors_with_the_script_name() {
    let (mut s, mut h) = setup();
    let e = s.load(&mut h, "broken.scm", "(define (f x) (+ x 1)").unwrap_err();
    assert_eq!(e.script, "broken.scm");
    let e = s.load(&mut h, "unbound.scm", "(no-such-function 1)").unwrap_err();
    assert!(e.message.contains("no-such-function"), "{e}");
    // The engine still works afterwards.
    assert_eq!(eval(&mut s, &mut h, "(+ 1 2)"), SteelVal::IntV(3));
}

#[test]
fn handlers_run_on_emit_and_failures_are_isolated() {
    let (mut s, mut h) = setup();
    s.load(
        &mut h,
        "t.scm",
        r#"
        (on 'output (lambda (id text) (terminal-send id (string-append "echo:" text))))
        (on 'output (lambda (id text) (error "boom")))
        (on "output" (lambda (id text) (log text)))
        "#,
    )
    .unwrap();
    assert!(s.handles("output"));
    assert!(!s.handles("exit"));

    s.emit(&mut h, "output", vec![SteelVal::IntV(1), SteelVal::StringV("hi".into())]);
    assert_eq!(h.sent, ["echo:hi"]);
    assert_eq!(h.log.len(), 2, "{:?}", h.log);
    assert!(h.log[0].contains("'output' handler failed") && h.log[0].contains("boom"), "{}", h.log[0]);
    assert_eq!(h.log[1], "hi");

    s.emit(&mut h, "exit", vec![]);
    let e = s.eval(&mut h, "(on 'output 42)").unwrap_err();
    assert!(e.contains("expected a procedure"), "{e}");
}

#[test]
fn timers_fire_in_due_order_and_can_be_cancelled() {
    let (mut s, mut h) = setup();
    assert_eq!(s.next_deadline(), None);
    eval(
        &mut s,
        &mut h,
        r#"
        (run-after 20 (lambda () (log "late")))
        (run-after 0 (lambda () (log "soon") (run-after 0 (lambda () (log "chained")))))
        (define dropped (run-after 5 (lambda () (log "dropped"))))
        (cancel-timer dropped)
        "#,
    );
    let first = s.next_deadline().unwrap();
    assert!(first <= Instant::now());

    s.fire_due(&mut h, Instant::now());
    assert_eq!(h.log, ["soon"], "a timer set while firing waits for the next round");

    s.fire_due(&mut h, Instant::now() + Duration::from_secs(1));
    assert_eq!(h.log, ["soon", "chained", "late"], "the chained timer was due first");
    assert_eq!(s.next_deadline(), None);
}

#[test]
fn failing_timers_are_logged() {
    let (mut s, mut h) = setup();
    eval(&mut s, &mut h, r#"(run-after 0 (lambda () (car '())))"#);
    s.fire_due(&mut h, Instant::now() + Duration::from_secs(1));
    assert_eq!(h.log.len(), 1);
    assert!(h.log[0].starts_with("script: timer failed"), "{}", h.log[0]);
}

#[test]
fn regexes_and_line_helpers() {
    let (mut s, mut h) = setup();
    assert_eq!(
        eval(&mut s, &mut h, r#"(regex-match? "esc to interrupt" "(esc to interrupt)")"#),
        SteelVal::BoolV(true)
    );
    assert_eq!(string(eval(&mut s, &mut h, r#"(regex-find "[0-9]+s" "working 12s")"#)), "12s");
    assert_eq!(eval(&mut s, &mut h, r#"(regex-find "x" "abc")"#), SteelVal::BoolV(false));
    assert!(s.eval(&mut h, r#"(regex-match? "(" "abc")"#).unwrap_err().contains("bad regex"));
    assert_eq!(
        string(eval(&mut s, &mut h, r#"(find-line "^Do you want" '("x" "Do you want to proceed?" "y"))"#)),
        "Do you want to proceed?"
    );
    assert_eq!(eval(&mut s, &mut h, r#"(any-line? "nope" '("x" "y"))"#), SteelVal::BoolV(false));
}

#[test]
fn debounce_keeps_only_the_last_call_per_key() {
    let (mut s, mut h) = setup();
    eval(
        &mut s,
        &mut h,
        r#"
        (debounce 1 50 (lambda () (log "first")))
        (debounce 1 50 (lambda () (log "second")))
        (debounce 2 50 (lambda () (log "other")))
        "#,
    );
    s.fire_due(&mut h, Instant::now() + Duration::from_secs(1));
    h.log.sort();
    assert_eq!(h.log, ["other", "second"]);
    assert_eq!(s.next_deadline(), None);
}

#[test]
fn user_scripts_load_in_name_order_and_override_builtins() {
    let dir = std::env::temp_dir().join(format!("ymacs-steel-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("b.scm"), "(set! who (string-append who \"+b\"))").unwrap();
    std::fs::write(dir.join("a.scm"), "(define who \"a\") (define (any-line? re lines) 'mine)").unwrap();
    std::fs::write(dir.join("c.scm"), "(oops").unwrap();
    std::fs::write(dir.join("notes.txt"), "(not scheme").unwrap();

    let (mut s, mut h) = setup();
    let errors = s.load_dir(&mut h, &dir);
    std::fs::remove_dir_all(&dir).unwrap();

    assert_eq!(errors.len(), 1, "{errors:#?}");
    assert!(errors[0].script.ends_with("c.scm"));
    assert_eq!(string(eval(&mut s, &mut h, "who")), "a+b");
    assert_eq!(eval(&mut s, &mut h, "(any-line? \"x\" '())").to_string(), "mine");
    assert!(s.load_dir(&mut h, Path::new("/no/such/dir")).is_empty());
}

#[test]
fn host_functions_fail_outside_a_call() {
    // Nothing is current between calls, so a stray closure cannot reach a
    // host that is gone.
    assert!(host(|_| ()).is_err());
    let (mut s, mut h) = setup();
    eval(&mut s, &mut h, "(+ 1 1)");
    assert!(host(|_| ()).is_err());
}

#[test]
fn call_invokes_a_global_function() {
    let (mut s, mut h) = setup();
    eval(&mut s, &mut h, "(define (classify lines) (if (any-line? \"esc to interrupt\" lines) 'working 'idle))");
    let lines = vec!["x".to_string(), "esc to interrupt".to_string()];
    let lines = ::steel::rvals::IntoSteelVal::into_steelval(lines).unwrap();
    let v = s.call(&mut h, "classify", vec![lines]).unwrap();
    assert_eq!(v.to_string(), "working");
    assert!(s.call(&mut h, "no-such", vec![]).is_err());
}
