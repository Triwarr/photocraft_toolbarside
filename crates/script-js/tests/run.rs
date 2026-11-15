//! End-to-end runs through the public API.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use photocraft_engine::Session;
use photocraft_script_js::session_host::SessionHost;
use photocraft_script_js::{Host, HostError, ModalKind, RunOptions, Runner, Step, Unlimited, WallClock};
use serde_json::{Value, json};

type Log = Rc<RefCell<Vec<String>>>;

/// Counts commands and keeps output, answering every command with `{}`.
#[derive(Default)]
struct Probe {
    calls: Log,
    lines: Log,
}

impl Host for Probe {
    fn execute(&mut self, id: &str, _: Value) -> Result<Value, HostError> {
        self.calls.borrow_mut().push(id.to_owned());
        if id == "fail" { Err(HostError("refused".into())) } else { Ok(json!({"active": null, "documents": []})) }
    }

    fn write_line(&mut self, line: &str) {
        self.lines.borrow_mut().push(line.to_owned());
    }
}

fn probe_run(src: &str, options: RunOptions) -> (Runner, Log, Log) {
    let p = Probe::default();
    let (calls, lines) = (p.calls.clone(), p.lines.clone());
    (Runner::new(src, Box::new(p), Box::new(Unlimited), options).unwrap(), calls, lines)
}

fn lines_of(src: &str) -> Vec<String> {
    let (mut r, _, lines) = probe_run(src, RunOptions::default());
    assert_eq!(r.step(), Step::Done);
    lines.borrow().clone()
}

#[test]
fn app_name_defaults_and_takes_the_run_override() {
    assert_eq!(lines_of("$.writeln(app.name, ' ', app.version)"), ["PhotoCraft 11.0"]);
    let (mut r, _, lines) = probe_run("$.writeln(app.name)", RunOptions { app_name: Some("Tagged".into()) });
    assert_eq!(r.step(), Step::Done);
    assert_eq!(*lines.borrow(), ["Tagged"]);
}

#[test]
fn a_dialog_pauses_and_effects_before_it_happen_once() {
    let src = "$.writeln('before'); var ok = confirm('Go?'); var n = prompt('Name', 'x'); alert('hi'); $.writeln(ok, ' ', n);";
    let (mut r, _, lines) = probe_run(src, RunOptions::default());
    let Step::Paused(m) = r.step() else { panic!("confirm pauses") };
    assert_eq!((m.kind, m.args.clone()), (ModalKind::Confirm, json!(["Go?"])));
    assert_eq!(r.step(), Step::Paused(m.clone()), "stepping again without an answer stays paused");
    r.answer(json!(true)).unwrap();
    let Step::Paused(m) = r.step() else { panic!("prompt pauses") };
    assert_eq!((m.kind, m.args), (ModalKind::Prompt, json!(["Name", "x"])));
    r.answer(json!("Ann")).unwrap();
    assert!(matches!(r.step(), Step::Paused(m) if m.kind == ModalKind::Alert));
    r.answer(Value::Null).unwrap();
    assert_eq!(r.step(), Step::Done);
    assert_eq!(*lines.borrow(), ["before", "true Ann"], "replays don't repeat output");
    assert_eq!(r.attempts(), 4);
}

#[test]
fn prompt_cancel_is_null() {
    let (mut r, _, lines) = probe_run("$.writeln(prompt('q') === null)", RunOptions::default());
    assert!(matches!(r.step(), Step::Paused(_)));
    r.answer(Value::Null).unwrap();
    assert_eq!(r.step(), Step::Done);
    assert_eq!(*lines.borrow(), ["true"]);
}

#[test]
fn scripts_cannot_catch_a_pause_or_run_finally() {
    let src = "try { alert('x'); } catch (e) { $.writeln('caught'); } finally { $.writeln('finally'); } $.writeln('after');";
    let (mut r, _, lines) = probe_run(src, RunOptions::default());
    assert!(matches!(r.step(), Step::Paused(_)));
    assert!(lines.borrow().is_empty(), "neither catch nor finally ran at the pause");
    r.answer(Value::Null).unwrap();
    assert_eq!(r.step(), Step::Done);
    assert_eq!(*lines.borrow(), ["finally", "after"]);
}

#[test]
fn clock_and_random_replay_identically() {
    // A replay that saw a different time or number would write a different line and diverge.
    let src = "var a = Math.random(), d = Date.now(), n = new Date().getTime(); $.writeln(a, d, n); alert(1); $.writeln(a < 1 && a >= 0, d <= n);";
    let (mut r, _, lines) = probe_run(src, RunOptions::default());
    assert!(matches!(r.step(), Step::Paused(_)));
    r.answer(Value::Null).unwrap();
    assert_eq!(r.step(), Step::Done);
    assert_eq!(lines.borrow()[1], "truetrue");
}

#[test]
fn host_errors_are_catchable() {
    let (mut r, calls, _) = probe_run("try { app.documents } catch (e) { throw e }", RunOptions::default());
    assert_eq!(r.step(), Step::Done);
    assert_eq!(*calls.borrow(), ["session.inspect"]);
    assert_eq!(lines_of("try { $.writeln('\\uD800') } catch (e) { $.writeln(e instanceof TypeError) }"), ["true"]);
}

#[test]
fn the_time_limit_stops_the_run_and_cannot_be_caught() {
    let p = Probe::default();
    let lines = p.lines.clone();
    let src = "for (;;) { try { $.writeln('x') } catch (e) {} }";
    let mut r = Runner::new(src, Box::new(p), Box::new(WallClock::new(Duration::from_millis(20), 1)), RunOptions::default()).unwrap();
    let Step::Failed(m) = r.step() else { panic!("stopped") };
    assert!(m.contains("time limit"), "{m}");
    assert!(!lines.borrow().is_empty());
}

#[test]
fn unsupported_include_is_reported_up_front() {
    let err = Runner::new("#include \"a.jsx\"", Box::new(Probe::default()), Box::new(Unlimited), RunOptions::default()).err();
    assert_eq!(err.as_deref(), Some("line 1: #include is not supported yet"));
}

#[test]
fn script_errors_fail_the_run() {
    let (mut r, _, _) = probe_run("null.x", RunOptions::default());
    assert!(matches!(r.step(), Step::Failed(_)));
}

/// Fresh-context replay is O(N²) in dialogs; this measures 50 (R1's phase 1 check).
#[test]
fn fifty_alerts_in_a_loop() {
    let (mut r, _, lines) = probe_run("for (var i = 0; i < 50; i++) { $.writeln(i); alert(i); }", RunOptions::default());
    let started = std::time::Instant::now();
    let mut longest = Duration::ZERO;
    loop {
        let t = std::time::Instant::now();
        let step = r.step();
        longest = longest.max(t.elapsed());
        match step {
            Step::Paused(_) => r.answer(Value::Null).unwrap(),
            Step::Done => break,
            Step::Failed(m) => panic!("{m}"),
        }
    }
    eprintln!("50 alerts: {} attempts, total {:?}, longest step {:?}", r.attempts(), started.elapsed(), longest);
    assert_eq!(r.attempts(), 51);
    assert_eq!(lines.borrow().len(), 50);
}

fn session_with_two_documents() -> Rc<RefCell<Session>> {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 64, "height": 48, "name": "One"})).unwrap();
    s.execute("file.new", json!({"width": 20, "height": 10, "name": "Two"})).unwrap();
    Rc::new(RefCell::new(s))
}

#[test]
fn documents_and_active_document_against_a_real_session() {
    let s = session_with_two_documents();
    let host = SessionHost::new(s.clone(), "1");
    let out = host.output();
    let src = "var d = app.documents; $.writeln(d.length, ' ', d[0].name, ' ', d[0].width, 'x', d[0].height);
               $.writeln(app.activeDocument.name); app.activeDocument = d[0]; $.writeln(app.activeDocument.name);";
    let mut r = Runner::new(src, Box::new(host), Box::new(Unlimited), RunOptions::default()).unwrap();
    assert_eq!(r.step(), Step::Done);
    assert_eq!(*out.borrow(), ["2 One 64x48", "Two", "One"]);
    assert_eq!(s.borrow().active_index(), Some(0));
}

fn history_len(s: &mut Session) -> usize {
    s.execute("document.inspect", json!({})).unwrap()["history"].as_array().map_or(0, Vec::len)
}

fn layer_doc() -> (Rc<RefCell<Session>>, u64) {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 16, "height": 16})).unwrap();
    s.execute("layer.new.layer", json!({})).unwrap();
    let id = s.active().unwrap().active_layer.unwrap().0;
    (Rc::new(RefCell::new(s)), id)
}

#[test]
fn edits_of_one_run_share_one_history_step() {
    let (s, id) = layer_doc();
    let before = history_len(&mut s.borrow_mut());
    let mut h = SessionHost::new(s.clone(), "run-1");
    h.execute("layer.setProps", json!({"layer": id, "opacity": 0.5})).unwrap();
    h.execute("layer.translate", json!({"dx": 1, "dy": 0})).unwrap();
    assert_eq!(history_len(&mut s.borrow_mut()), before + 1);
}

/// R1: an unrecorded visibility change clears the key (execute.rs `if !record`).
#[test]
#[ignore = "known split: unrecorded visibility edit resets the coalesce key"]
fn an_unrecorded_visibility_change_keeps_one_step() {
    let (s, id) = layer_doc();
    s.borrow_mut().execute("history.options", json!({"makeLayerVisibilityChangesUndoable": false})).unwrap();
    let before = history_len(&mut s.borrow_mut());
    let mut h = SessionHost::new(s.clone(), "run-2");
    h.execute("layer.setProps", json!({"layer": id, "opacity": 0.5})).unwrap();
    h.execute("layer.setProps", json!({"layer": id, "visible": false})).unwrap();
    h.execute("layer.setProps", json!({"layer": id, "opacity": 0.25})).unwrap();
    assert_eq!(history_len(&mut s.borrow_mut()), before + 1);
}

/// R1: Paste Into sets its own step key.
#[test]
#[ignore = "known split: Paste Into overrides the coalesce key"]
fn paste_into_keeps_one_step() {
    let (s, id) = layer_doc();
    let mut h = SessionHost::new(s.clone(), "run-3");
    h.execute("select.rect", json!({"x": 0, "y": 0, "width": 4, "height": 4})).unwrap();
    h.execute("edit.fill", json!({"color": "#336699"})).unwrap();
    h.execute("edit.copy", json!({})).unwrap();
    let before = history_len(&mut s.borrow_mut());
    h.execute("layer.setProps", json!({"layer": id, "opacity": 0.5})).unwrap();
    h.execute("edit.pasteSpecial.pasteInto", json!({})).unwrap();
    h.execute("layer.setProps", json!({"layer": id, "opacity": 0.25})).unwrap();
    assert_eq!(history_len(&mut s.borrow_mut()), before + 1);
}

#[test]
fn time_at_a_dialog_does_not_count_against_the_limit() {
    let p = Probe::default();
    let lines = p.lines.clone();
    let budget = WallClock::new(Duration::from_millis(200), 1);
    let mut r = Runner::new("alert('a'); $.writeln('after');", Box::new(p), Box::new(budget), RunOptions::default()).unwrap();
    assert!(matches!(r.step(), Step::Paused(_)));
    std::thread::sleep(Duration::from_millis(400));
    r.answer(Value::Null).unwrap();
    assert_eq!(r.step(), Step::Done);
    assert_eq!(*lines.borrow(), ["after"]);
}

#[test]
fn fifty_alerts_fit_under_a_time_limit() {
    let src = "for (var i = 0; i < 50; i++) { alert(i); }";
    let budget = WallClock::new(Duration::from_secs(2), 1);
    let mut r = Runner::new(src, Box::new(Probe::default()), Box::new(budget), RunOptions::default()).unwrap();
    while let Step::Paused(_) = r.step() {
        r.answer(Value::Null).unwrap();
    }
    assert_eq!(r.step(), Step::Done);
}
