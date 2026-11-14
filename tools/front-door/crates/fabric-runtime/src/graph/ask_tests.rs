use super::*;
use serde_json::{Value, json};

fn fixture_route() -> Value {
    json!({"adapter":"fixture", "model":"fixture/model"})
}

fn decide(id: &str, timeout: Option<u64>) -> Value {
    let mut spec = json!({"question":"Ship the change?", "on_unanswered":"block_dependants", "default":"hold",
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
    run.checkpoint
        .nodes
        .iter()
        .find(|n| n.id == id)
        .unwrap()
        .state
}

/// Seconds in a Julian year.
const YEAR: u64 = 31_557_600;

fn at(seconds: i64) -> provenant_core::WallTime {
    provenant_core::WallTime::from_micros(1_800_000_000_000_000 + seconds * 1_000_000).unwrap()
}

#[test]
fn decide_answer_becomes_the_verdict_and_routes_the_chosen_edge() {
    let document = routed(None);
    let mut owner = FakeOwner {
        now: Some(at(0)),
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
    assert_eq!(ask.request, "choose.ask-1");
    assert_eq!(ask.timeout.unwrap().seconds, crate::config::DEFAULT_DECIDE_TIMEOUT_SECONDS);
    assert_eq!(ask.timeout.unwrap().source, DecideTimeoutSource::DomainDefault);
    assert!(owner.filed[0].body.contains("Ship the change?"));
    assert_eq!(ask.wake_at, waiting.checkpoint.wake_at);
    assert!(ask.wake_at.is_some());
    assert!(owner.calls.is_empty());
    assert_eq!(
        advance(&document, &mut owner, &mut journal)
            .unwrap()
            .checkpoint,
        waiting.checkpoint
    );

    owner
        .answers
        .insert("choose.ask-1".into(), reply(Some("ship"), None));
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert_eq!(state(&run, "ship-work"), NodeState::Succeeded);
    assert_eq!(state(&run, "hold-work"), NodeState::Skipped);
    let choose = run
        .checkpoint
        .nodes
        .iter()
        .find(|n| n.id == "choose")
        .unwrap();
    assert_eq!(
        choose.outputs.as_ref().unwrap().verdict.as_deref(),
        Some("ship")
    );
    assert_eq!(
        choose.outputs.as_ref().unwrap().decide_outcome,
        Some(DecideOutcome::Answered {
            option: "ship".into(),
            actor: "chair".into(),
        })
    );
    assert!(
        choose
            .deterministic_summary()
            .unwrap()
            .contains("answered by chair")
    );
    // The inbox keeps the answer; the checkpoint keeps no link once it is applied.
    assert!(run.checkpoint.asks.is_empty());
    assert!(owner.closed_asks.is_empty());
    assert_eq!(owner.filed.len(), 1);
    let replay = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(replay.checkpoint, run.checkpoint);
    assert_eq!(owner.calls.len(), 1);
}

#[test]
fn decide_reply_text_naming_an_option_is_accepted_and_other_text_asks_again() {
    let document = routed(None);
    let mut owner = FakeOwner {
        now: Some(at(0)),
        observations: [terminal(AttemptOutcome::Succeeded)].into(),
        ..Default::default()
    };
    owner
        .answers
        .insert("choose.ask-1".into(), reply(None, Some(" hold\n")));
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    let choose = run
        .checkpoint
        .nodes
        .iter()
        .find(|n| n.id == "choose")
        .unwrap();
    assert_eq!(
        choose.outputs.as_ref().unwrap().verdict.as_deref(),
        Some("hold")
    );
    assert_eq!(state(&run, "hold-work"), NodeState::Succeeded);

    // A rejected answer keeps the node open and replaces the request at once.
    let mut owner = FakeOwner {
        now: Some(at(0)),
        observations: [terminal(AttemptOutcome::Succeeded)].into(),
        ..Default::default()
    };
    owner
        .answers
        .insert("choose.ask-1".into(), reply(None, Some("maybe later")));
    let mut journal = MemoryJournal::default();
    let rejected = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(rejected.status, GraphStatus::Pending);
    assert_eq!(state(&rejected, "choose"), NodeState::Running);
    assert!(owner.calls.is_empty());
    let [second] = &rejected.checkpoint.asks[..] else {
        panic!("one link per node");
    };
    assert_eq!(second.ask_id, "choose.ask-2");
    assert_eq!(owner.filed.len(), 2);
    assert!(
        owner.filed[1]
            .body
            .ends_with("The previous answer was not applied: answer must choose one of ship, hold")
    );
    let again = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(again.checkpoint, rejected.checkpoint);
    assert_eq!(owner.filed.len(), 2);
    owner
        .answers
        .insert("choose.ask-2".into(), reply(Some("ship"), None));
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert_eq!(state(&run, "ship-work"), NodeState::Succeeded);
}

#[test]
fn decide_ids_stay_unique_across_repeated_cancel_and_resume() {
    let document = routed(None);
    let mut owner = FakeOwner { now: Some(at(0)), ..Default::default() };
    let mut journal = MemoryJournal::default();
    advance(&document, &mut owner, &mut journal).unwrap();
    for _ in 0..2 {
        journal.0.as_mut().unwrap().cancelled = true;
        let cancelled = advance(&document, &mut owner, &mut journal).unwrap();
        assert_eq!(cancelled.status, GraphStatus::Cancelled);
        resume(&document, &mut owner, &mut journal).unwrap();
    }
    let checkpoint = journal.0.as_ref().unwrap();
    let ids: Vec<_> = checkpoint
        .asks
        .iter()
        .map(|ask| ask.ask_id.as_str())
        .collect();
    assert_eq!(ids, ["choose.ask-3"]);
    assert_eq!(owner.closed_asks, ["choose.ask-1", "choose.ask-2"]);
    assert_eq!(
        owner
            .filed
            .iter()
            .map(|ask| ask.ask_id.as_str())
            .collect::<Vec<_>>(),
        ["choose.ask-1", "choose.ask-2", "choose.ask-3"]
    );
}

#[test]
fn waiting_decide_nodes_do_not_use_up_concurrency() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1, "graph_id":"decide-concurrency", "decision_session":"chair-session", "nodes":[
            decide("choose", None),
            {"id":"work", "access":"read_only", "prompt":"work", "route":fixture_route()}
        ]
    }))
    .unwrap();
    let mut owner = FakeOwner {
        now: Some(at(0)),
        observations: [terminal(AttemptOutcome::Succeeded)].into(),
        ..Default::default()
    };
    let limits = crate::config::GraphLimits {
        concurrency: Some(1),
        ..Default::default()
    };
    let run = advance_with_budget(
        &document,
        &mut owner,
        &mut MemoryJournal::default(),
        &limits,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(state(&run, "choose"), NodeState::Running);
    assert_eq!(state(&run, "work"), NodeState::Succeeded);
    assert_eq!(owner.calls.len(), 1);
}

#[test]
fn decide_deadlines_wait_for_a_clock_instead_of_dropping_the_timeout() {
    let document = routed(Some(60));
    let mut owner = FakeOwner::default();
    let mut journal = MemoryJournal::default();
    let error = advance(&document, &mut owner, &mut journal).unwrap_err();
    assert_eq!(error.code(), "graph_refused");
    assert!(
        journal
            .0
            .as_ref()
            .is_none_or(|checkpoint| checkpoint.asks.is_empty())
    );
    assert!(owner.asked.is_empty());

    owner.now = Some(at(0));
    let waiting = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(waiting.checkpoint.asks[0].wake_at, Some(at(60)));
}

#[test]
fn decide_timeouts_past_a_century_are_kept_until_the_deadline_overflows() {
    let owner = || FakeOwner {
        now: Some(at(0)),
        ..Default::default()
    };
    let long = routed(Some(YEAR * 200));
    let waiting = advance(&long, &mut owner(), &mut MemoryJournal::default()).unwrap();
    assert_eq!(
        waiting.checkpoint.asks[0].wake_at,
        Some(at(YEAR as i64 * 200))
    );

    // Valid against the wall-time range, but past it from the present time.
    let late = routed(Some(YEAR * 8_000));
    validate_plan(&late).unwrap();
    let mut journal = MemoryJournal::default();
    let error = advance(&late, &mut owner(), &mut journal).unwrap_err();
    assert_eq!(error.code(), "graph_refused");
    assert!(
        journal
            .0
            .as_ref()
            .is_none_or(|checkpoint| checkpoint.asks.is_empty())
    );
}

#[test]
fn completion_closes_a_pause_ask_left_by_lowered_limits() {
    let mut document = graph(vec![
        node("one", route("first")),
        node("two", route("first")),
    ]);
    document.decision_session = Some("chair-session".into());
    let pending = || AttemptObservation::Pending {
        attempt_ref: Some("host/attempt".into()),
    };
    let mut owner = FakeOwner {
        now: Some(at(0)),
        observations: [pending(), pending(), pending(), pending()].into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    advance(&document, &mut owner, &mut journal).unwrap();
    let lowered = crate::config::GraphLimits {
        nodes: 1,
        ..Default::default()
    };
    let paused = advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &lowered,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(paused.status, GraphStatus::Paused);
    assert_eq!(owner.asked, ["pause-1"]);
    assert_eq!(paused.checkpoint.asks.len(), 1);

    owner.observations = [
        terminal(AttemptOutcome::Succeeded),
        terminal(AttemptOutcome::Succeeded),
    ]
    .into();
    let run = advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &lowered,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(run.checkpoint.pause.is_none());
    assert!(run.checkpoint.pause.is_none());
    assert!(run.checkpoint.asks.is_empty());
    assert_eq!(owner.closed_asks, ["pause-1"]);
}

#[test]
fn decide_inbox_error_at_deadline_keeps_the_ask_open_for_each_default() {
    for (default, disposition) in [
        (Some("hold"), UnansweredDisposition::BlockDependants),
        (None, UnansweredDisposition::BlockDependants),
        (None, UnansweredDisposition::SkipDependants),
    ] {
        let mut document = routed(Some(60));
        let spec = document.nodes[0].decide.as_mut().unwrap();
        spec.default = default.map(str::to_owned);
        spec.on_unanswered = disposition;
        let mut owner = FakeOwner {
            now: Some(at(0)),
            observations: [terminal(AttemptOutcome::Succeeded)].into(),
            ..Default::default()
        };
        let mut journal = MemoryJournal::default();
        let waiting = advance(&document, &mut owner, &mut journal).unwrap();
        let ask_id = waiting.checkpoint.asks[0].ask_id.clone();
        let deadline = waiting.checkpoint.asks[0].wake_at.unwrap();
        owner.now = Some(deadline);
        owner.fail_node_ask_once = true;

        let unread = advance(&document, &mut owner, &mut journal).unwrap();
        assert_eq!(unread.status, GraphStatus::Pending);
        assert_eq!(state(&unread, "choose"), NodeState::Running);
        assert_eq!(unread.checkpoint.asks.len(), 1);
        assert_eq!(unread.checkpoint.asks[0].ask_id, ask_id);
        assert!(unread.checkpoint.nodes[0].outputs.is_none());
        assert!(owner.closed_asks.is_empty());
        assert_ne!(state(&unread, "ship-work"), NodeState::Succeeded);
        assert_ne!(state(&unread, "hold-work"), NodeState::Succeeded);

        owner
            .answers
            .insert(ask_id, reply(Some("ship"), None));
        let answered = advance(&document, &mut owner, &mut journal).unwrap();
        assert_eq!(answered.status, GraphStatus::Complete);
        assert_eq!(state(&answered, "ship-work"), NodeState::Succeeded);
        assert_eq!(state(&answered, "hold-work"), NodeState::Skipped);
    }
}

#[test]
fn handoff_missing_classification_does_not_release_a_dependant() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version": 1,
        "graph_id": "handoff-missing-dependant",
        "nodes": [
            {"id":"writer", "access":"read_only", "prompt":"review", "route":fixture_route()},
            {"id":"release", "access":"read_only", "prompt":"release", "depends_on":["writer"], "route":fixture_route()}
        ]
    }))
    .unwrap();
    let mut owner = FakeOwner {
        observations: [AttemptObservation::Terminal {
            attempt_ref: Some("host/handoff-missing".into()),
            outcome: AttemptOutcome::ReviewRequired,
            launched: true,
            result: None,
            result_warning: None,
            result_path: None,
            branch: None,
            commit: None,
            stop: Some(provenant_records::StopCause::HandoffMissing),
            settlement_reason: Some(provenant_records::run::SettlementReason::HandoffMissing),
        }]
        .into(),
        ..Default::default()
    };

    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(state(&run, "writer"), NodeState::Failed);
    assert_ne!(state(&run, "release"), NodeState::Succeeded);
}

#[test]
fn repair_scope_timeout_default_cannot_approve() {
    let mut document: GraphDocument = serde_json::from_value(json!({
        "schema_version": 1,
        "graph_id": "repair-scope-timeout",
        "nodes": [{
            "id": "scope",
            "kind": "decide",
            "access": "read_only",
            "repair_scope_gate": true,
            "decide": {
                "question": "Approve the changed slice?",
                "on_unanswered": "block_dependants",
                "default": "hold",
                "options": [
                    {"id": "approve", "label": "Approve"},
                    {"id": "hold", "label": "Hold"}
                ]
            }
        }]
    }))
    .unwrap();

    assert!(validate_flat_plan(&document, &Default::default()).is_ok());
    document.nodes[0].decide.as_mut().unwrap().default = Some("approve".into());
    let error = validate_flat_plan(&document, &Default::default()).unwrap_err();
    assert!(error.to_string().contains(
        "scope approval requires an owner answer, never a timeout default"
    ));
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
    assert_eq!(owner.filed[0].default.as_deref(), Some("hold"));
    assert_eq!(owner.filed[0].wake_at, Some(at(60)));
    assert_eq!(owner.filed[0].timeout.unwrap().source, DecideTimeoutSource::Graph);
    assert!(owner.filed[0].body.contains("from the decide declaration"));
    assert_eq!(journal.0.as_ref().unwrap().wake_at, Some(at(60)));
    owner.now = Some(at(59));
    assert_eq!(
        advance(&document, &mut owner, &mut journal)
            .unwrap()
            .checkpoint,
        waiting.checkpoint
    );

    owner.now = Some(at(61));
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert_eq!(state(&run, "hold-work"), NodeState::Succeeded);
    assert_eq!(state(&run, "ship-work"), NodeState::Skipped);
    assert!(run.checkpoint.asks.is_empty());
    assert!(run.checkpoint.wake_at.is_none());
    let choose = run
        .checkpoint
        .nodes
        .iter()
        .find(|n| n.id == "choose")
        .unwrap();
    assert_eq!(
        choose.outputs.as_ref().unwrap().decide_outcome,
        Some(DecideOutcome::AnsweredByDefault {
            option: "hold".into(),
        })
    );
}

#[test]
fn several_decide_asks_are_open_at_once_and_resolve_independently() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1, "graph_id":"two-asks", "decision_session":"chair-session",
        "nodes":[decide("first", Some(120)), decide("second", Some(30))]
    }))
    .unwrap();
    let mut owner = FakeOwner {
        now: Some(at(0)),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let waiting = advance(&document, &mut owner, &mut journal).unwrap();
    let ids: Vec<_> = waiting
        .checkpoint
        .asks
        .iter()
        .map(|ask| ask.ask_id.as_str())
        .collect();
    assert_eq!(ids, ["first.ask-1", "second.ask-1"]);
    assert_eq!(waiting.checkpoint.wake_at, Some(at(30)));

    owner
        .answers
        .insert("second.ask-1".into(), reply(Some("ship"), None));
    let partial = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(partial.status, GraphStatus::Pending);
    assert_eq!(state(&partial, "second"), NodeState::Succeeded);
    assert_eq!(state(&partial, "first"), NodeState::Running);
    assert_eq!(partial.checkpoint.wake_at, Some(at(120)));
    assert!(owner.closed_asks.is_empty());

    owner
        .answers
        .insert("first.ask-1".into(), reply(Some("hold"), None));
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
    let mut owner = FakeOwner { now: Some(at(0)), ..Default::default() };
    owner
        .answers
        .insert("revise--r1--choose.ask-1".into(), reply(Some("hold"), None));
    owner
        .answers
        .insert("revise--r2--choose.ask-1".into(), reply(Some("ship"), None));
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(run.checkpoint.successful());
    let ids: Vec<_> = owner.filed.iter().map(|ask| ask.ask_id.as_str()).collect();
    assert_eq!(
        ids,
        ["revise--r1--choose.ask-1", "revise--r2--choose.ask-1"]
    );
    assert!(run.checkpoint.asks.is_empty());
}

#[test]
fn decide_configuration_is_checked_against_the_inbox_contract() {
    let base = routed(Some(60));
    validate_plan(&base).unwrap();
    type Change = fn(&mut GraphNode);
    let cases: &[(&str, Change)] = &[
        ("one option", |n| {
            n.decide.as_mut().unwrap().options.truncate(1)
        }),
        ("duplicate IDs", |n| {
            n.decide.as_mut().unwrap().options[1].id = "ship".into()
        }),
        ("bad option ID", |n| {
            n.decide.as_mut().unwrap().options[1].id = "has space".into()
        }),
        ("unknown default", |n| {
            n.decide.as_mut().unwrap().default = Some("later".into())
        }),
        ("zero timeout", |n| {
            n.decide.as_mut().unwrap().timeout_seconds = Some(0)
        }),
        ("empty question", |n| {
            n.decide.as_mut().unwrap().question = " ".into()
        }),
        ("timeout past the wall-time range", |n| {
            n.decide.as_mut().unwrap().timeout_seconds = Some(YEAR * 9_000)
        }),
        ("unrepresentable timeout", |n| {
            n.decide.as_mut().unwrap().timeout_seconds = Some(u64::MAX)
        }),
        ("body past the inbox limit", |n| {
            n.decide.as_mut().unwrap().question = "q".repeat(8_000)
        }),
        ("prompt", |n| n.prompt = "run an agent".into()),
        ("route", |n| n.route = Some(route("fixture"))),
    ];
    for (name, change) in cases {
        let mut document = base.clone();
        change(&mut document.nodes[0]);
        assert!(validate_plan(&document).is_err(), "{name}");
    }
    let mut longest = base.clone();
    longest.nodes[0].decide.as_mut().unwrap().timeout_seconds = Some(YEAR * 1_000);
    // The question may use whatever the rendered body leaves of the inbox limit.
    longest.nodes[0].decide.as_mut().unwrap().question = "q".repeat(7_800);
    validate_plan(&longest).unwrap();
    let mut sessionless = base.clone();
    sessionless.decision_session = None;
    assert!(validate_plan(&sessionless).is_err());
    sessionless.nodes[0].decide.as_mut().unwrap().session = Some("node-session".into());
    validate_plan(&sessionless).unwrap();
}

fn budget_pause() -> (GraphDocument, FakeOwner, crate::config::GraphBudget) {
    let mut document = graph(vec![
        node("one", route("first")),
        node("two", route("first")),
    ]);
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
        settlement_reason: Some(provenant_records::run::SettlementReason::Completed),
        stop: None,
    };
    let owner = FakeOwner {
        now: Some(at(0)),
        observations: [observation(), observation()].into(),
        usage: AttemptUsage {
            tokens: Some(10),
            ..Default::default()
        },
        ..Default::default()
    };
    let budget = crate::config::GraphBudget {
        per_graph: crate::config::BudgetLimit {
            tokens: Some(10),
            ..Default::default()
        },
        ..Default::default()
    };
    (document, owner, budget)
}

#[test]
fn a_budget_pause_raises_one_ask_and_an_extend_answer_resumes() {
    let (document, mut owner, budget) = budget_pause();
    let mut journal = MemoryJournal::default();
    let limits = Default::default();
    let paused =
        advance_with_budget(&document, &mut owner, &mut journal, &limits, &budget).unwrap();
    assert_eq!(paused.status, GraphStatus::Paused);
    let ask = &paused.checkpoint.asks[0];
    assert_eq!((ask.ask_id.as_str(), ask.kind), ("pause-1", AskKind::Pause));
    assert!(
        owner.filed[0]
            .body
            .contains(paused.checkpoint.pause_reason().unwrap())
    );
    assert_eq!(
        advance_with_budget(&document, &mut owner, &mut journal, &limits, &budget)
            .unwrap()
            .checkpoint,
        paused.checkpoint
    );

    owner.answers.insert(
        "pause-1".into(),
        reply(
            None,
            Some(r#"{"decision":"extend","extensions":["budget.per_graph.tokens=30"]}"#),
        ),
    );
    let run = advance_with_budget(&document, &mut owner, &mut journal, &limits, &budget).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert_eq!(run.checkpoint.extensions, ["budget.per_graph.tokens=30"]);
    assert_eq!(owner.calls.len(), 2);
    assert!(run.checkpoint.asks.is_empty());
    assert!(owner.closed_asks.is_empty());
}

#[test]
fn a_pause_answer_can_cancel_and_a_rejected_answer_asks_again() {
    let (document, mut owner, budget) = budget_pause();
    let mut journal = MemoryJournal::default();
    let limits = Default::default();
    advance_with_budget(&document, &mut owner, &mut journal, &limits, &budget).unwrap();
    owner.answers.insert(
        "pause-1".into(),
        reply(
            None,
            Some(r#"{"decision":"extend","extensions":["limits.nodes=99"]}"#),
        ),
    );
    let rejected =
        advance_with_budget(&document, &mut owner, &mut journal, &limits, &budget).unwrap();
    assert_eq!(rejected.status, GraphStatus::Paused);
    // The rejected answer is replaced by a request that says why.
    assert_eq!(rejected.checkpoint.asks.len(), 1);
    assert_eq!(rejected.checkpoint.asks[0].ask_id, "pause-2");
    assert!(
        owner.filed[1]
            .body
            .contains("The previous answer was not applied")
    );
    let again = advance_with_budget(&document, &mut owner, &mut journal, &limits, &budget).unwrap();
    assert_eq!(again.checkpoint, rejected.checkpoint);

    owner
        .answers
        .insert("pause-2".into(), reply(Some("cancel"), None));
    let cancelled =
        advance_with_budget(&document, &mut owner, &mut journal, &limits, &budget).unwrap();
    assert_eq!(cancelled.status, GraphStatus::Cancelled);
    assert_eq!(owner.calls.len(), 1);
    assert!(cancelled.checkpoint.asks.is_empty());
}

#[test]
fn a_planner_fragment_can_add_a_decide_node() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1, "graph_id":"planned-decide", "decision_session":"chair-session", "nodes":[
            {"id":"plan", "kind":"task", "access":"read_only", "prompt":"Slice the work",
             "route":fixture_route(), "plan":{"kinds":["task", "decide"], "max_nodes":2}},
            {"id":"after", "access":"read_only", "prompt":"Continue", "depends_on":["plan"], "route":fixture_route()}
        ]
    }))
    .unwrap();
    let fragment = format!(
        "```fragment\n{}\n```",
        json!({"schema_version":1, "nodes":[decide("choose", None)]})
    );
    let mut owner = FakeOwner {
        now: Some(at(0)),
        observations: [
            terminal_with_result(AttemptOutcome::Succeeded, &fragment),
            terminal(AttemptOutcome::Succeeded),
        ]
        .into(),
        ..Default::default()
    };
    owner
        .answers
        .insert("plan--choose.ask-1".into(), reply(Some("ship"), None));
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(
        run.status,
        GraphStatus::Complete,
        "{:?}",
        run.checkpoint.pause_reason()
    );
    assert_eq!(state(&run, "plan--choose"), NodeState::Succeeded);
    assert_eq!(owner.calls.len(), 2);
}


#[test]
fn principal_scope_gate_notifies_once_and_only_waits_outside_approved_scope() {
    for classification in [
        Some(json!({"within_scope":true,"one_way_door":false})),
        Some(json!({"within_scope":false,"one_way_door":false})),
        Some(json!({"within_scope":true,"one_way_door":true})),
        None,
    ] {
        let within_scope = classification == Some(json!({"within_scope":true,"one_way_door":false}));
        let document: GraphDocument = serde_json::from_value(json!({
            "schema_version":1,"graph_id":"principal-scope","nodes":[
                {"id":"design","access":"read_only","prompt":"design","route":fixture_route()},
                {"id":"scope","kind":"decide","access":"read_only","depends_on":["design"],
                 "repair_scope_gate":true,"decide":{"on_unanswered":"block_dependants", "question":"Approve the changed slice?",
                    "options":[{"id":"approve","label":"Approve"},{"id":"hold","label":"Hold"}]}},
                {"id":"approved","kind":"gate","access":"read_only","depends_on":["scope"],
                 "inputs":[{"node":"scope","outputs":["verdict"]}],
                 "condition":{"type":"verdict","expected":"approve","threshold":"all"}},
                {"id":"writer","access":"read_only","depends_on":["approved"],"prompt":"continue","route":fixture_route()}
            ]
        })).unwrap();
        let report = format!("Principal options and smallest slice\n```findings\n{}\n```",
            json!({"schema_version":1,"findings":[],"repair_design":classification}));
        let mut owner = FakeOwner {
        now: Some(at(0)), actor:"chair".into(), observations:[
            terminal_with_result(AttemptOutcome::Succeeded, &report),
            terminal(AttemptOutcome::Succeeded),
        ].into(), ..Default::default() };
        let mut journal = MemoryJournal::default();
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        assert_eq!(owner.owner_notifications, ["scope.owner-notification"]);
        assert_eq!(state(&run, "writer") == NodeState::Succeeded, within_scope);
        if within_scope {
            assert_eq!(run.status, GraphStatus::Complete);
            assert!(run.checkpoint.asks.is_empty());
        } else {
            assert_eq!(run.status, GraphStatus::Pending);
            assert_eq!(run.checkpoint.asks[0].session, "principal-scope");
            assert_eq!(run.checkpoint.asks[0].kind, AskKind::Decide);
            assert_eq!(owner.calls.len(), 1);
        }
        advance(&document, &mut owner, &mut journal).unwrap();
        assert_eq!(owner.owner_notifications.len(), 1);
        if within_scope {
            let library = template::TemplateLibrary::bundled();
            let resolvers = std::collections::BTreeMap::new();
            let limits = crate::config::GraphLimits::default();
            let context = patch::PatchContext { library:&library, resolvers:&resolvers,
                scan:&patch::scan_for_secrets, journal:None, limits:&limits };
            patch::retry_node(&mut patch::LiveGraph { document:&document,
                checkpoint:journal.0.as_mut().unwrap() }, "scope", None, &context).unwrap();
            advance(&document, &mut owner, &mut journal).unwrap();
            assert_eq!(owner.owner_notifications, ["scope.owner-notification", "scope.owner-notification.retry-1"]);
        }
        if !within_scope {
            let mut unanswered_journal = MemoryJournal(journal.0.clone());
            let mut unanswered_owner = FakeOwner {
                now: run.checkpoint.asks[0].wake_at,
                ..Default::default()
            };
            let unanswered = advance(&document, &mut unanswered_owner, &mut unanswered_journal).unwrap();
            assert_eq!(state(&unanswered, "writer"), NodeState::Blocked);
            let scope = unanswered.checkpoint.nodes.iter().find(|node| node.id == "scope").unwrap();
            assert_eq!(scope.state, NodeState::Failed);
            assert_eq!(scope.outputs.as_ref().unwrap().verdict, None);
            assert_eq!(scope.outputs.as_ref().unwrap().decide_outcome, Some(DecideOutcome::Unanswered));
            assert!(unanswered_owner.calls.is_empty());
            advance(&document, &mut unanswered_owner, &mut unanswered_journal).unwrap();
            let mut approved_journal = MemoryJournal(journal.0.clone());
            let mut approved_owner = FakeOwner {
        now: Some(at(0)), actor:"chair".into(),
                observations:[terminal(AttemptOutcome::Succeeded)].into(),
                answers:std::collections::BTreeMap::from([("scope.ask-1".into(), reply(Some("approve"), None))]),
                ..Default::default() };
            let approved = advance(&document, &mut approved_owner, &mut approved_journal).unwrap();
            assert_eq!(state(&approved, "writer"), NodeState::Succeeded);
            assert_eq!(approved_owner.calls.len(), 1);
            assert_eq!(approved_owner.asked, ["scope.ask-1"]);
            assert!(approved_owner.owner_notifications.is_empty());
            let library = template::TemplateLibrary::bundled();
            let resolvers = std::collections::BTreeMap::new();
            let limits = crate::config::GraphLimits::default();
            let context = patch::PatchContext { library:&library, resolvers:&resolvers,
                scan:&patch::scan_for_secrets, journal:None, limits:&limits };
            patch::retry_node(&mut patch::LiveGraph { document:&document,
                checkpoint:approved_journal.0.as_mut().unwrap() }, "scope", None, &context).unwrap();
            let retried = advance(&document, &mut approved_owner, &mut approved_journal).unwrap();
            assert_eq!(state(&retried, "scope"), NodeState::Running);
            assert!(retried.checkpoint.asks.iter().any(|ask| ask.ask_id == "scope.ask-1.retry-1"));
            assert_eq!(approved_owner.owner_notifications, ["scope.owner-notification.retry-1"]);
            owner.answers.insert("scope.ask-1".into(), reply(Some("hold"), None));
            let held = advance(&document, &mut owner, &mut journal).unwrap();
            assert_ne!(state(&held, "writer"), NodeState::Succeeded);
            assert_eq!(owner.calls.len(), 1);
        }
    }
}


#[test]
fn principal_container_retry_does_not_reuse_owner_decision() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1,"graph_id":"principal-retry","nodes":[
            {"id":"redesign","kind":"composite","access":"read_only","composite":{"exports":[],"nodes":[
                {"id":"design","access":"read_only","prompt":"design","route":fixture_route()},
                {"id":"scope","kind":"decide","access":"read_only","depends_on":["design"],
                 "repair_scope_gate":true,"decide":{"on_unanswered":"block_dependants", "question":"Approve the changed slice?",
                    "options":[{"id":"approve","label":"Approve"},{"id":"hold","label":"Hold"}]}}
            ]}}
        ]
    })).unwrap();
    let report = "First principal slice\n```findings\n{\"schema_version\":1,\"findings\":[],\"repair_design\":{\"within_scope\":false,\"one_way_door\":true}}\n```";
    let mut owner = FakeOwner {
        now: Some(at(0)), actor:"chair".into(),
        observations:[terminal_with_result(AttemptOutcome::Succeeded, report)].into(),
        answers:std::collections::BTreeMap::from([("redesign--scope.ask-1".into(), reply(Some("approve"), None))]),
        ..Default::default() };
    let mut journal = MemoryJournal::default();
    assert_eq!(advance(&document, &mut owner, &mut journal).unwrap().status, GraphStatus::Complete);
    let library = template::TemplateLibrary::bundled();
    let resolvers = std::collections::BTreeMap::new();
    let limits = crate::config::GraphLimits::default();
    let context = patch::PatchContext { library:&library, resolvers:&resolvers,
        scan:&patch::scan_for_secrets, journal:None, limits:&limits };
    patch::retry_node(&mut patch::LiveGraph { document:&document,
        checkpoint:journal.0.as_mut().unwrap() }, "redesign", None, &context).unwrap();
    owner.observations.push_back(terminal_with_result(AttemptOutcome::Succeeded,
        &report.replace("First principal slice", "Revised principal slice")));
    let retried = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(retried.status, GraphStatus::Pending);
    assert_eq!(state(&retried, "redesign--scope"), NodeState::Running);
    assert_eq!(retried.checkpoint.asks[0].ask_id, "redesign--scope.ask-1.retry-1");
    assert!(owner.filed.last().unwrap().body.contains("Revised principal slice"));
    assert_eq!(owner.owner_notifications, ["redesign--scope.owner-notification", "redesign--scope.owner-notification.retry-1"]);
}

/// Every declared unanswered disposition records no option and prevents dependants running.
#[test]
fn unanswered_decides_follow_their_declared_disposition_without_selecting_an_option() {
    for (disposition, expected) in [
        (UnansweredDisposition::BlockDependants, NodeState::Failed),
        (UnansweredDisposition::SkipDependants, NodeState::Skipped),
    ] {
        let mut document = routed(None);
        let decide = document.nodes[0].decide.as_mut().unwrap();
        decide.default = None;
        decide.on_unanswered = disposition;
        let mut owner = FakeOwner {
            now: Some(at(0)),
            observations: [terminal(AttemptOutcome::Succeeded)].into(),
            ..Default::default()
        };
        let mut journal = MemoryJournal::default();
        let waiting = advance(&document, &mut owner, &mut journal).unwrap();
        let deadline = waiting.checkpoint.asks[0].wake_at.expect("decide deadline");
        assert!(deadline > at(0));
        owner.now = Some(deadline);
        let settled = advance(&document, &mut owner, &mut journal).unwrap();
        let choose = settled.checkpoint.nodes.iter().find(|node| node.id == "choose").unwrap();
        assert_eq!(choose.state, expected);
        let outputs = choose.outputs.as_ref().unwrap();
        assert_eq!(outputs.decide_outcome, Some(DecideOutcome::Unanswered));
        assert_eq!(outputs.verdict, None);
        assert!(settled.checkpoint.asks.is_empty());
        assert_ne!(state(&settled, "ship-work"), NodeState::Succeeded);
        assert_ne!(state(&settled, "hold-work"), NodeState::Succeeded);
    }
}

#[test]
fn every_decide_outcome_round_trips_and_unanswered_has_no_option() {
    let outcomes = [
        DecideOutcome::Answered { option: "ship".into(), actor: "chair".into() },
        DecideOutcome::AnsweredByDefault { option: "hold".into() },
        DecideOutcome::AnsweredByPolicy { option: "approve".into(), rule: "approved_repair_scope".into() },
        DecideOutcome::Unanswered,
    ];
    for outcome in outcomes {
        let encoded = serde_json::to_value(&outcome).unwrap();
        let decoded: DecideOutcome = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(decoded, outcome);
        if outcome == DecideOutcome::Unanswered {
            assert!(encoded.get("option").is_none());
            assert_eq!(outcome.option(), None);
        } else {
            assert!(outcome.option().is_some());
        }
    }
}

#[test]
fn every_unanswered_disposition_is_encodable() {
    for disposition in [
        UnansweredDisposition::BlockDependants,
        UnansweredDisposition::SkipDependants,
    ] {
        let encoded = serde_json::to_value(disposition).unwrap();
        let decoded: UnansweredDisposition = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, disposition);
    }
}
