use super::*;
use serde_json::{Value, json};

fn fixture_route() -> Value {
    json!({"adapter":"fixture", "model":"fixture/model"})
}

fn decide(id: &str, timeout: Option<u64>) -> Value {
    let mut spec = json!({"question":"Ship the change?", "default":"hold",
        "options":[{"id":"ship", "label":"Ship"}, {"id":"hold", "label":"Hold"}]});
    if let Some(seconds) = timeout {
        spec["timeout_seconds"] = json!(seconds);
    }
    json!({"id":id, "kind":"decide", "access":"read_only", "decide":spec})
}

/// A decide node whose answer routes through a verdict gate to one of two tasks.
fn routed(timeout: Option<u64>) -> GraphDocument {
    serde_json::from_value(json!({
        "schema_version":1, "graph_id":"decide-routes", "decision_session":"chair-session", "nodes":[
            decide("choose", timeout),
            {"id":"shipping", "kind":"gate", "access":"read_only", "depends_on":["choose"],
             "inputs":[{"node":"choose", "outputs":["verdict"]}], "on_fail":"hold-work",
             "condition":{"type":"verdict", "expected":"ship", "threshold":"all"}},
            {"id":"ship-work", "access":"read_only", "prompt":"ship", "depends_on":["shipping"], "route":fixture_route()},
            {"id":"hold-work", "access":"read_only", "prompt":"hold", "depends_on":["shipping"], "route":fixture_route()}
        ]
    }))
    .unwrap()
}

fn state(run: &GraphRun, id: &str) -> NodeState {
    run.checkpoint.nodes.iter().find(|n| n.id == id).unwrap().state
}

fn at(seconds: i64) -> provenant_core::WallTime {
    provenant_core::WallTime::from_micros(1_800_000_000_000_000 + seconds * 1_000_000).unwrap()
}

#[test]
fn decide_answer_becomes_the_verdict_and_routes_the_chosen_edge() {
    let document = routed(None);
    let mut owner = FakeOwner {
        actor: "chair".into(),
        observations: [terminal(AttemptOutcome::Succeeded)].into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let waiting = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(waiting.status, GraphStatus::Pending);
    assert_eq!(state(&waiting, "choose"), NodeState::Running);
    let ask = &waiting.checkpoint.asks[0];
    assert_eq!(ask.ask_id, "choose.ask-1");
    assert_eq!(ask.kind, AskKind::Decide);
    assert_eq!(ask.session, "chair-session");
    assert!(ask.body.contains("Ship the change?"));
    assert!(ask.wake_at.is_none() && waiting.checkpoint.wake_at.is_none());
    assert!(owner.calls.is_empty());
    assert_eq!(advance(&document, &mut owner, &mut journal).unwrap().checkpoint, waiting.checkpoint);

    owner.answers.insert("choose.ask-1".into(), reply(Some("ship"), None));
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert_eq!(state(&run, "ship-work"), NodeState::Succeeded);
    assert_eq!(state(&run, "hold-work"), NodeState::Skipped);
    let choose = run.checkpoint.nodes.iter().find(|n| n.id == "choose").unwrap();
    assert_eq!(choose.outputs.as_ref().unwrap().verdict.as_deref(), Some("ship"));
    assert!(choose.deterministic_summary().unwrap().contains("answered by chair"));
    let answer = run.checkpoint.asks[0].answer.as_ref().unwrap();
    assert_eq!(answer.source, AnswerSource::Answered);
    assert_eq!(answer.actor.as_deref(), Some("chair"));
    assert!(run.checkpoint.asks[0].resolved && run.checkpoint.asks[0].closed);
    assert_eq!(owner.closed_asks, ["choose.ask-1"]);
    let replay = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(replay.checkpoint, run.checkpoint);
    assert_eq!(owner.calls.len(), 1);
}

#[test]
fn decide_reply_text_naming_an_option_is_accepted_and_other_text_fails_the_node() {
    for (text, chosen) in [(" hold\n", Some("hold")), ("maybe later", None)] {
        let document = routed(None);
        let mut owner = FakeOwner {
            observations: [terminal(AttemptOutcome::Succeeded)].into(),
            ..Default::default()
        };
        owner.answers.insert("choose.ask-1".into(), reply(None, Some(text)));
        let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
        let choose = run.checkpoint.nodes.iter().find(|n| n.id == "choose").unwrap();
        match chosen {
            Some(option) => {
                assert_eq!(choose.outputs.as_ref().unwrap().verdict.as_deref(), Some(option));
                assert_eq!(state(&run, "hold-work"), NodeState::Succeeded);
            }
            None => {
                assert_eq!(choose.state, NodeState::Failed);
                let error = choose.advance_error.as_deref().unwrap();
                assert!(error.contains("answer must choose one of ship, hold"), "{error}");
                let answer = run.checkpoint.asks[0].answer.as_ref().unwrap();
                assert!(answer.rejected.is_some());
                assert!(owner.calls.is_empty());
            }
        }
    }
}

#[test]
fn decide_timeout_persists_wake_at_and_takes_the_default() {
    let document = routed(Some(60));
    let mut owner = FakeOwner {
        now: Some(at(0)),
        observations: [terminal(AttemptOutcome::Succeeded)].into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let waiting = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(waiting.checkpoint.asks[0].wake_at, Some(at(60)));
    assert_eq!(waiting.checkpoint.wake_at, Some(at(60)));
    assert_eq!(waiting.checkpoint.asks[0].default.as_deref(), Some("hold"));
    assert_eq!(journal.0.as_ref().unwrap().wake_at, Some(at(60)));
    owner.now = Some(at(59));
    assert_eq!(advance(&document, &mut owner, &mut journal).unwrap().checkpoint, waiting.checkpoint);

    owner.now = Some(at(61));
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert_eq!(state(&run, "hold-work"), NodeState::Succeeded);
    assert_eq!(state(&run, "ship-work"), NodeState::Skipped);
    let answer = run.checkpoint.asks[0].answer.as_ref().unwrap();
    assert_eq!(answer.source, AnswerSource::Default);
    assert_eq!((answer.option.as_deref(), answer.actor.as_deref()), (Some("hold"), None));
    assert_eq!(answer.at, Some(at(61)));
    assert!(run.checkpoint.wake_at.is_none());
    let choose = run.checkpoint.nodes.iter().find(|n| n.id == "choose").unwrap();
    assert!(choose.deterministic_summary().unwrap().contains("the default at the deadline"));
}

#[test]
fn several_decide_asks_are_open_at_once_and_resolve_independently() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1, "graph_id":"two-asks", "decision_session":"chair-session",
        "nodes":[decide("first", Some(120)), decide("second", Some(30))]
    }))
    .unwrap();
    let mut owner = FakeOwner { now: Some(at(0)), ..Default::default() };
    let mut journal = MemoryJournal::default();
    let waiting = advance(&document, &mut owner, &mut journal).unwrap();
    let ids: Vec<_> = waiting.checkpoint.asks.iter().map(|ask| ask.ask_id.as_str()).collect();
    assert_eq!(ids, ["first.ask-1", "second.ask-1"]);
    assert_eq!(waiting.checkpoint.wake_at, Some(at(30)));

    owner.answers.insert("second.ask-1".into(), reply(Some("ship"), None));
    let partial = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(partial.status, GraphStatus::Pending);
    assert_eq!(state(&partial, "second"), NodeState::Succeeded);
    assert_eq!(state(&partial, "first"), NodeState::Running);
    assert_eq!(partial.checkpoint.wake_at, Some(at(120)));
    assert_eq!(owner.closed_asks, ["second.ask-1"]);

    owner.answers.insert("first.ask-1".into(), reply(Some("hold"), None));
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(run.checkpoint.wake_at.is_none());
}

#[test]
fn decide_nodes_run_inside_loop_bodies_once_per_round() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1, "graph_id":"decide-loop", "decision_session":"chair-session", "nodes":[
            {"id":"revise", "kind":"loop", "access":"read_only", "loop":{"max_rounds":2, "until":"approved", "body":[
                decide("choose", None),
                {"id":"approved", "kind":"gate", "access":"read_only", "depends_on":["choose"],
                 "inputs":[{"node":"choose", "outputs":["verdict"]}],
                 "condition":{"type":"verdict", "expected":"ship", "threshold":"all"}}
            ]}}
        ]
    }))
    .unwrap();
    let mut owner = FakeOwner::default();
    owner.answers.insert("revise--r1--choose.ask-1".into(), reply(Some("hold"), None));
    owner.answers.insert("revise--r2--choose.ask-1".into(), reply(Some("ship"), None));
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(run.checkpoint.successful());
    let ids: Vec<_> = run.checkpoint.asks.iter().map(|ask| ask.ask_id.as_str()).collect();
    assert_eq!(ids, ["revise--r1--choose.ask-1", "revise--r2--choose.ask-1"]);
}

#[test]
fn decide_configuration_is_checked_against_the_inbox_contract() {
    let base = routed(Some(60));
    validate_plan(&base).unwrap();
    type Change = fn(&mut GraphNode);
    let cases: [(&str, Change); 9] = [
        ("one option", |n| n.decide.as_mut().unwrap().options.truncate(1)),
        ("duplicate IDs", |n| n.decide.as_mut().unwrap().options[1].id = "ship".into()),
        ("bad option ID", |n| n.decide.as_mut().unwrap().options[1].id = "has space".into()),
        ("unknown default", |n| n.decide.as_mut().unwrap().default = Some("later".into())),
        ("timeout without default", |n| n.decide.as_mut().unwrap().default = None),
        ("zero timeout", |n| n.decide.as_mut().unwrap().timeout_seconds = Some(0)),
        ("empty question", |n| n.decide.as_mut().unwrap().question = " ".into()),
        ("prompt", |n| n.prompt = "run an agent".into()),
        ("route", |n| n.route = Some(route("fixture"))),
    ];
    for (name, change) in cases {
        let mut document = base.clone();
        change(&mut document.nodes[0]);
        assert!(validate_plan(&document).is_err(), "{name}");
    }
    let mut sessionless = base.clone();
    sessionless.decision_session = None;
    assert!(validate_plan(&sessionless).is_err());
    sessionless.nodes[0].decide.as_mut().unwrap().session = Some("node-session".into());
    validate_plan(&sessionless).unwrap();
}

fn budget_pause() -> (GraphDocument, FakeOwner, crate::config::GraphBudget) {
    let mut document = graph(vec![node("one", route("first")), node("two", route("first"))]);
    document.decision_session = Some("chair-session".into());
    let observation = || AttemptObservation::Terminal {
        attempt_ref: Some("host/attempt".into()),
        outcome: AttemptOutcome::Succeeded,
        launched: true,
        result: None,
        result_warning: None,
        result_path: None,
        branch: None,
        commit: None,
    };
    let owner = FakeOwner {
        observations: [observation(), observation()].into(),
        usage: AttemptUsage { tokens: Some(10), ..Default::default() },
        ..Default::default()
    };
    let budget = crate::config::GraphBudget {
        per_graph: crate::config::BudgetLimit { tokens: Some(10), ..Default::default() },
        ..Default::default()
    };
    (document, owner, budget)
}

#[test]
fn a_budget_pause_raises_one_ask_and_an_extend_answer_resumes() {
    let (document, mut owner, budget) = budget_pause();
    let mut journal = MemoryJournal::default();
    let limits = Default::default();
    let paused = advance_with_budget(&document, &mut owner, &mut journal, &limits, &budget).unwrap();
    assert_eq!(paused.status, GraphStatus::Paused);
    let ask = &paused.checkpoint.asks[0];
    assert_eq!((ask.ask_id.as_str(), ask.kind), ("pause-1", AskKind::Pause));
    assert!(ask.body.contains(paused.checkpoint.pause_reason.as_deref().unwrap()));
    assert_eq!(advance_with_budget(&document, &mut owner, &mut journal, &limits, &budget).unwrap().checkpoint, paused.checkpoint);

    owner.answers.insert(
        "pause-1".into(),
        reply(None, Some(r#"{"decision":"extend","extensions":["budget.per_graph.tokens=30"]}"#)),
    );
    let run = advance_with_budget(&document, &mut owner, &mut journal, &limits, &budget).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert_eq!(run.checkpoint.extensions, ["budget.per_graph.tokens=30"]);
    assert_eq!(owner.calls.len(), 2);
    assert_eq!(run.checkpoint.asks[0].answer.as_ref().unwrap().actor.as_deref(), Some("chair"));
    assert_eq!(owner.closed_asks, ["pause-1"]);
}

#[test]
fn a_pause_answer_can_cancel_and_a_rejected_answer_asks_again() {
    let (document, mut owner, budget) = budget_pause();
    let mut journal = MemoryJournal::default();
    let limits = Default::default();
    advance_with_budget(&document, &mut owner, &mut journal, &limits, &budget).unwrap();
    owner.answers.insert(
        "pause-1".into(),
        reply(None, Some(r#"{"decision":"extend","extensions":["limits.nodes=99"]}"#)),
    );
    let rejected = advance_with_budget(&document, &mut owner, &mut journal, &limits, &budget).unwrap();
    assert_eq!(rejected.status, GraphStatus::Paused);
    let first = &rejected.checkpoint.asks[0];
    assert!(first.answer.as_ref().unwrap().rejected.as_deref().unwrap().contains("extends budgets only"));
    let again = advance_with_budget(&document, &mut owner, &mut journal, &limits, &budget).unwrap();
    let second = again.checkpoint.asks.iter().find(|ask| ask.ask_id == "pause-2").unwrap();
    assert!(second.body.contains("The previous answer was not applied"));

    owner.answers.insert("pause-2".into(), reply(Some("cancel"), None));
    let cancelled = advance_with_budget(&document, &mut owner, &mut journal, &limits, &budget).unwrap();
    assert_eq!(cancelled.status, GraphStatus::Cancelled);
    assert_eq!(owner.calls.len(), 1);
    assert!(cancelled.checkpoint.asks.iter().all(|ask| ask.closed));
}

#[test]
fn a_planner_fragment_can_add_a_decide_node_when_its_kinds_allow_it() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1, "graph_id":"planned-decide", "decision_session":"chair-session", "nodes":[
            {"id":"plan", "kind":"plan", "access":"read_only", "prompt":"Slice the work",
             "route":fixture_route(), "plan":{"kinds":["task", "decide"], "max_nodes":2}},
            {"id":"after", "access":"read_only", "prompt":"Continue", "depends_on":["plan"], "route":fixture_route()}
        ]
    }))
    .unwrap();
    let fragment = format!("```fragment\n{}\n```", json!({"schema_version":1, "nodes":[decide("choose", None)]}));
    let mut owner = FakeOwner {
        observations: [
            terminal_with_result(AttemptOutcome::Succeeded, &fragment),
            terminal(AttemptOutcome::Succeeded),
        ]
        .into(),
        ..Default::default()
    };
    owner.answers.insert("plan--choose.ask-1".into(), reply(Some("ship"), None));
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Complete, "{:?}", run.checkpoint.pause_reason);
    assert_eq!(state(&run, "plan--choose"), NodeState::Succeeded);
    assert_eq!(owner.calls.len(), 2);

    let mut narrow = document.clone();
    narrow.nodes[0].plan.as_mut().unwrap().kinds = vec![NodeKind::Task];
    let mut owner = FakeOwner {
        observations: [terminal_with_result(AttemptOutcome::Succeeded, &fragment)].into(),
        ..Default::default()
    };
    let paused = advance(&narrow, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(paused.status, GraphStatus::Paused);
    let reason = paused.checkpoint.pause_reason.as_deref().unwrap();
    assert!(reason.contains("not declared"), "{reason}");
    // The graph's decision session turns the expansion pause into a pause ask.
    assert_eq!(owner.asked, ["pause-1"]);
}
