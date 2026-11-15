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
    super::model::document_fixture(json!({
        "graph_id":"decide-routes", "decision_session":"chair-session", "nodes":[
            decide("choose", timeout),
            {"id":"shipping", "kind":"gate", "access":"read_only", "depends_on":["choose"],
             "inputs":[{"node":"choose", "outputs":["verdict"]}], "on_fail":"hold-work",
             "condition":{"type":"verdict", "expected":"ship", "threshold":"all"}},
            {"id":"ship-work", "access":"read_only", "prompt":"ship", "depends_on":["shipping"], "route":fixture_route()},
            {"id":"hold-work", "access":"read_only", "prompt":"hold", "depends_on":["shipping"], "route":fixture_route()}
        ]
    }))
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
    assert_eq!(ask.kind, AskKind::Decide);
    assert_eq!(ask.session, "chair-session");
    let ask_id = ask.ask_id.clone();
    assert_eq!(owner.filed[0].ask_id, ask_id);
    assert_eq!(
        ask.timeout.as_ref().unwrap().seconds,
        crate::config::DEFAULT_DECIDE_TIMEOUT_SECONDS
    );
    assert_eq!(
        ask.timeout.as_ref().unwrap().source,
        DecideTimeoutSource::DomainDefault
    );
    assert_eq!(ask.wake_at, waiting.checkpoint.wake_at);
    assert!(ask.wake_at.is_some());
    assert_eq!(
        advance(&document, &mut owner, &mut journal)
            .unwrap()
            .checkpoint,
        waiting.checkpoint
    );

    owner.answers.insert(ask_id, reply(Some("ship"), None));
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
            extra: Default::default(),
            option: "ship".into(),
            actor: "chair".into(),
        })
    );
    // The inbox keeps the answer; the checkpoint keeps no link once it is applied.
    assert!(run.checkpoint.asks.is_empty());
    assert!(owner.closed_asks.is_empty());
    let replay = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(replay.checkpoint, run.checkpoint);
}

#[test]
fn decide_reply_text_is_normalised_and_invalid_text_reopens_the_ask() {
    let document = routed(None);
    let mut owner = FakeOwner {
        now: Some(at(0)),
        observations: [terminal(AttemptOutcome::Succeeded)].into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let waiting = advance(&document, &mut owner, &mut journal).unwrap();
    let accepted_id = waiting.checkpoint.asks[0].ask_id.clone();
    owner
        .answers
        .insert(accepted_id, reply(None, Some(" hold\n")));
    let run = advance(&document, &mut owner, &mut journal).unwrap();
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
    let mut journal = MemoryJournal::default();
    let waiting = advance(&document, &mut owner, &mut journal).unwrap();
    let first_id = waiting.checkpoint.asks[0].ask_id.clone();
    owner
        .answers
        .insert(first_id.clone(), reply(None, Some("maybe later")));
    let rejected = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(rejected.status, GraphStatus::Pending);
    assert_eq!(state(&rejected, "choose"), NodeState::Running);
    let [second] = &rejected.checkpoint.asks[..] else {
        panic!("one link per node");
    };
    let replacement_id = second.ask_id.clone();
    assert_ne!(replacement_id, first_id);
    let again = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(again.checkpoint, rejected.checkpoint);
    owner
        .answers
        .insert(replacement_id, reply(Some("ship"), None));
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert_eq!(state(&run, "ship-work"), NodeState::Succeeded);
}

#[test]
fn decide_ids_stay_unique_across_repeated_cancel_and_resume() {
    let document = routed(None);
    let mut owner = FakeOwner {
        now: Some(at(0)),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let first = advance(&document, &mut owner, &mut journal).unwrap();
    let mut ids = vec![first.checkpoint.asks[0].ask_id.clone()];
    for _ in 0..2 {
        journal.0.as_mut().unwrap().cancelled = true;
        let cancelled = advance(&document, &mut owner, &mut journal).unwrap();
        assert_eq!(cancelled.status, GraphStatus::Cancelled);
        let resumed = resume(&document, &mut owner, &mut journal).unwrap();
        let new_id = resumed.checkpoint.asks[0].ask_id.clone();
        assert!(!ids.contains(&new_id));
        ids.push(new_id);
    }
    let checkpoint = journal.0.as_ref().unwrap();
    assert_eq!(checkpoint.asks.len(), 1);
    assert_eq!(checkpoint.asks[0].ask_id, *ids.last().unwrap());
    for (index, id) in ids.iter().enumerate() {
        assert!(!ids[..index].contains(id));
    }
    for closed in &ids[..2] {
        assert!(owner.closed_asks.contains(closed));
    }
    for id in &ids {
        assert!(owner.filed.iter().any(|ask| &ask.ask_id == id));
    }
}

#[test]
fn waiting_decide_nodes_do_not_use_up_concurrency() {
    let document: GraphDocument = super::model::document_fixture(json!({
        "graph_id":"decide-concurrency", "decision_session":"chair-session", "nodes":[
            decide("choose", None),
            {"id":"work", "access":"read_only", "prompt":"work", "route":fixture_route()}
        ]
    }));
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
    let pause_id = paused.checkpoint.asks[0].ask_id.clone();
    assert_eq!(owner.asked, [pause_id.as_str()]);
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
    assert!(run.checkpoint.asks.is_empty());
    assert_eq!(owner.closed_asks, [pause_id.as_str()]);
}

#[test]
fn decide_deadline_and_inbox_failure_follow_the_declared_default_and_disposition() {
    for (default, disposition, expected_state, expected_outcome) in [
        (
            Some("hold"),
            UnansweredDisposition::BlockDependants,
            NodeState::Succeeded,
            DecideOutcome::AnsweredByDefault {
                extra: Default::default(),
                option: "hold".into(),
            },
        ),
        (
            None,
            UnansweredDisposition::BlockDependants,
            NodeState::Failed,
            DecideOutcome::Unanswered {
                extra: Default::default(),
            },
        ),
        (
            None,
            UnansweredDisposition::SkipDependants,
            NodeState::Skipped,
            DecideOutcome::Unanswered {
                extra: Default::default(),
            },
        ),
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

        owner.answers.insert(ask_id, reply(Some("ship"), None));
        let answered = advance(&document, &mut owner, &mut journal).unwrap();
        assert_eq!(answered.status, GraphStatus::Complete);
        assert_eq!(state(&answered, "ship-work"), NodeState::Succeeded);
        assert_eq!(state(&answered, "hold-work"), NodeState::Skipped);

        let mut timeout_owner = FakeOwner {
            now: Some(at(0)),
            observations: [terminal(AttemptOutcome::Succeeded)].into(),
            ..Default::default()
        };
        let mut timeout_journal = MemoryJournal::default();
        let waiting = advance(&document, &mut timeout_owner, &mut timeout_journal).unwrap();
        timeout_owner.now = waiting.checkpoint.asks[0].wake_at;
        let settled = advance(&document, &mut timeout_owner, &mut timeout_journal).unwrap();
        let choose = settled
            .checkpoint
            .nodes
            .iter()
            .find(|node| node.id == "choose")
            .unwrap();
        assert_eq!(choose.state, expected_state);
        assert_eq!(
            choose.outputs.as_ref().unwrap().decide_outcome,
            Some(expected_outcome)
        );
        if default.is_some() {
            assert_eq!(state(&settled, "hold-work"), NodeState::Succeeded);
            assert_eq!(state(&settled, "ship-work"), NodeState::Skipped);
        } else {
            assert_ne!(state(&settled, "ship-work"), NodeState::Succeeded);
            assert_ne!(state(&settled, "hold-work"), NodeState::Succeeded);
        }
    }
}

#[test]
fn handoff_missing_classification_does_not_release_a_dependant() {
    let document: GraphDocument = super::model::document_fixture(json!({

        "graph_id": "handoff-missing-dependant",
        "nodes": [
            {"id":"writer", "access":"read_only", "prompt":"review", "route":fixture_route()},
            {"id":"release", "access":"read_only", "prompt":"release", "depends_on":["writer"], "route":fixture_route()}
        ]
    }));
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
fn repair_scope_decides_reject_timeout_defaults() {
    let make_scope_gate = |option_ids: [&str; 2], default: Option<&str>| -> GraphDocument {
        let mut decide = json!({
            "question": "Approve the changed slice?",
            "on_unanswered": "block_dependants",
            "options": option_ids.map(|id| json!({"id": id, "label": id}))
        });
        if let Some(default) = default {
            decide["default"] = json!(default);
        }
        super::model::document_fixture(json!({

            "graph_id": "repair-scope-timeout",
            "nodes": [{
                "id": "scope",
                "kind": "decide",
                "access": "read_only",
                "repair_scope_gate": true,
                "decide": decide
            }]
        }))
    };

    let scope_without_default = make_scope_gate(["approve", "hold"], None);
    assert!(validate_flat_plan(&scope_without_default, &Default::default()).is_ok());
    // Ordinary decisions retain their explicit default behavior.
    assert!(validate_flat_plan(&routed(Some(60)), &Default::default()).is_ok());

    for (default, options) in [
        ("approve", ["approve", "hold"]),
        ("hold", ["approve", "hold"]),
        ("ship", ["ship", "wait"]),
    ] {
        let document = make_scope_gate(options, Some(default));
        let error = validate_flat_plan(&document, &Default::default()).unwrap_err();
        assert!(matches!(error, AdmissionError::Plan(_)));
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
    assert_eq!(owner.filed[0].default.as_deref(), Some("hold"));
    assert_eq!(owner.filed[0].wake_at, Some(at(60)));
    assert_eq!(
        owner.filed[0].timeout.as_ref().unwrap().source,
        DecideTimeoutSource::Graph
    );
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
            extra: Default::default(),
            option: "hold".into(),
        })
    );
}

#[test]
fn several_decide_asks_are_open_at_once_and_resolve_independently() {
    let document: GraphDocument = super::model::document_fixture(json!({
        "graph_id":"two-asks", "decision_session":"chair-session",
        "nodes":[decide("first", Some(120)), decide("second", Some(30))]
    }));
    let mut owner = FakeOwner {
        now: Some(at(0)),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let waiting = advance(&document, &mut owner, &mut journal).unwrap();
    let first_id = waiting
        .checkpoint
        .asks
        .iter()
        .find(|ask| ask.node.as_deref() == Some("first"))
        .unwrap()
        .ask_id
        .clone();
    let second_id = waiting
        .checkpoint
        .asks
        .iter()
        .find(|ask| ask.node.as_deref() == Some("second"))
        .unwrap()
        .ask_id
        .clone();
    assert_ne!(first_id, second_id);
    assert_eq!(waiting.checkpoint.wake_at, Some(at(30)));

    owner.answers.insert(second_id, reply(Some("ship"), None));
    let partial = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(partial.status, GraphStatus::Pending);
    assert_eq!(state(&partial, "second"), NodeState::Succeeded);
    assert_eq!(state(&partial, "first"), NodeState::Running);
    assert_eq!(partial.checkpoint.wake_at, Some(at(120)));
    assert!(owner.closed_asks.is_empty());

    owner.answers.insert(first_id, reply(Some("hold"), None));
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(run.checkpoint.wake_at.is_none());
}

#[test]
fn decide_nodes_run_inside_loop_bodies_once_per_round() {
    let document: GraphDocument = super::model::document_fixture(json!({
        "graph_id":"decide-loop", "decision_session":"chair-session", "nodes":[
            {"id":"revise", "kind":"loop", "access":"read_only", "loop":{"max_rounds":2, "until":"approved", "body":[
                decide("choose", None),
                {"id":"approved", "kind":"gate", "access":"read_only", "depends_on":["choose"],
                 "inputs":[{"node":"choose", "outputs":["verdict"]}],
                 "condition":{"type":"verdict", "expected":"ship", "threshold":"all"}}
            ]}}
        ]
    }));
    let mut owner = FakeOwner {
        now: Some(at(0)),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let first_wait = advance(&document, &mut owner, &mut journal).unwrap();
    let first_id = first_wait.checkpoint.asks[0].ask_id.clone();
    owner
        .answers
        .insert(first_id.clone(), reply(Some("hold"), None));
    let second_wait = advance(&document, &mut owner, &mut journal).unwrap();
    let second_id = second_wait.checkpoint.asks[0].ask_id.clone();
    assert_ne!(first_id, second_id);
    owner
        .answers
        .insert(second_id.clone(), reply(Some("ship"), None));
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(run.checkpoint.successful());
    assert!(owner.filed.iter().any(|ask| ask.ask_id == first_id));
    assert!(owner.filed.iter().any(|ask| ask.ask_id == second_id));
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
    assert_eq!(ask.kind, AskKind::Pause);
    let ask_id = ask.ask_id.clone();
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
        ask_id,
        reply(
            None,
            Some(r#"{"decision":"extend","extensions":["budget.per_graph.tokens=30"]}"#),
        ),
    );
    let run = advance_with_budget(&document, &mut owner, &mut journal, &limits, &budget).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert_eq!(run.checkpoint.extensions, ["budget.per_graph.tokens=30"]);
    assert!(run.checkpoint.asks.is_empty());
    assert!(owner.closed_asks.is_empty());
}

#[test]
fn a_pause_answer_can_cancel_and_a_rejected_answer_asks_again() {
    let (document, mut owner, budget) = budget_pause();
    let mut journal = MemoryJournal::default();
    let limits = Default::default();
    let waiting =
        advance_with_budget(&document, &mut owner, &mut journal, &limits, &budget).unwrap();
    let first_id = waiting.checkpoint.asks[0].ask_id.clone();
    owner.answers.insert(
        first_id.clone(),
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
    let replacement_id = rejected.checkpoint.asks[0].ask_id.clone();
    assert_ne!(replacement_id, first_id);
    let again = advance_with_budget(&document, &mut owner, &mut journal, &limits, &budget).unwrap();
    assert_eq!(again.checkpoint, rejected.checkpoint);

    owner
        .answers
        .insert(replacement_id, reply(Some("cancel"), None));
    let cancelled =
        advance_with_budget(&document, &mut owner, &mut journal, &limits, &budget).unwrap();
    assert_eq!(cancelled.status, GraphStatus::Cancelled);
    assert!(cancelled.checkpoint.asks.is_empty());
}

#[test]
fn a_planner_fragment_can_add_a_decide_node() {
    let document: GraphDocument = super::model::document_fixture(json!({
        "graph_id":"planned-decide", "decision_session":"chair-session", "nodes":[
            {"id":"plan", "kind":"task", "access":"read_only", "prompt":"Slice the work",
             "route":fixture_route(), "plan":{"kinds":["task", "decide"], "max_nodes":2}},
            {"id":"after", "access":"read_only", "prompt":"Continue", "depends_on":["plan"], "route":fixture_route()}
        ]
    }));
    let fragment = format!(
        "```fragment\n{}\n```",
        json!({"nodes":[decide("choose", None)]})
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
    let mut journal = MemoryJournal::default();
    let waiting = advance(&document, &mut owner, &mut journal).unwrap();
    let ask_id = waiting.checkpoint.asks[0].ask_id.clone();
    owner.answers.insert(ask_id, reply(Some("ship"), None));
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(
        run.status,
        GraphStatus::Complete,
        "{:?}",
        run.checkpoint.pause_reason()
    );
    assert_eq!(state(&run, "plan--choose"), NodeState::Succeeded);
}

#[test]
fn principal_scope_gate_notifies_once_and_only_waits_outside_approved_scope() {
    for classification in [
        Some(json!({"within_scope":true,"one_way_door":false})),
        Some(json!({"within_scope":false,"one_way_door":false})),
        Some(json!({"within_scope":true,"one_way_door":true})),
        None,
    ] {
        let within_scope =
            classification == Some(json!({"within_scope":true,"one_way_door":false}));
        let document: GraphDocument = super::model::document_fixture(json!({
            "graph_id":"principal-scope","nodes":[
                {"id":"design","access":"read_only","prompt":"design","route":fixture_route()},
                {"id":"scope","kind":"decide","access":"read_only","depends_on":["design"],
                 "repair_scope_gate":true,"decide":{"on_unanswered":"block_dependants", "question":"Approve the changed slice?",
                    "options":[{"id":"approve","label":"Approve"},{"id":"hold","label":"Hold"}]}},
                {"id":"approved","kind":"gate","access":"read_only","depends_on":["scope"],
                 "inputs":[{"node":"scope","outputs":["verdict"]}],
                 "condition":{"type":"verdict","expected":"approve","threshold":"all"}},
                {"id":"writer","access":"read_only","depends_on":["approved"],"prompt":"continue","route":fixture_route()}
            ]
        }));
        let report = format!(
            "Principal options and smallest slice\n```findings\n{}\n```",
            json!({"schema_version":1,"findings":[],"repair_design":classification})
        );
        let mut owner = FakeOwner {
            now: Some(at(0)),
            actor: "chair".into(),
            observations: [
                terminal_with_result(AttemptOutcome::Succeeded, &report),
                terminal(AttemptOutcome::Succeeded),
            ]
            .into(),
            ..Default::default()
        };
        let mut journal = MemoryJournal::default();
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        assert_eq!(owner.owner_notifications.len(), 1);
        let first_notification = owner.owner_notifications[0].clone();
        let scope_ask_id = run.checkpoint.asks.first().map(|ask| ask.ask_id.clone());
        assert_eq!(state(&run, "writer") == NodeState::Succeeded, within_scope);
        if within_scope {
            assert_eq!(run.status, GraphStatus::Complete);
            assert!(run.checkpoint.asks.is_empty());
        } else {
            assert_eq!(run.status, GraphStatus::Pending);
            assert_eq!(run.checkpoint.asks[0].session, "principal-scope");
            assert_eq!(run.checkpoint.asks[0].kind, AskKind::Decide);
        }
        let notifications_before_replay = owner.owner_notifications.clone();
        advance(&document, &mut owner, &mut journal).unwrap();
        assert_eq!(owner.owner_notifications, notifications_before_replay);
        if within_scope {
            let library = template::TemplateLibrary::bundled();
            let resolvers = std::collections::BTreeMap::new();
            let limits = crate::config::GraphLimits::default();
            let context = patch::PatchContext {
                library: &library,
                resolvers: &resolvers,
                scan: &patch::scan_for_secrets,
                journal: None,
                limits: &limits,
                worktree_layout: provenant_config::PortTable::builtin().worktree_layout,
            };
            patch::retry_node(
                &mut patch::LiveGraph {
                    document: &document,
                    checkpoint: journal.0.as_mut().unwrap(),
                },
                "scope",
                None,
                &context,
            )
            .unwrap();
            advance(&document, &mut owner, &mut journal).unwrap();
            assert_eq!(owner.owner_notifications.len(), 2);
            assert_eq!(owner.owner_notifications[0], first_notification);
            assert_ne!(owner.owner_notifications[1], first_notification);
        }
        if !within_scope {
            let mut unanswered_journal = MemoryJournal(journal.0.clone());
            let mut unanswered_owner = FakeOwner {
                now: run.checkpoint.asks[0].wake_at,
                ..Default::default()
            };
            let unanswered =
                advance(&document, &mut unanswered_owner, &mut unanswered_journal).unwrap();
            assert_eq!(state(&unanswered, "writer"), NodeState::Blocked);
            let scope = unanswered
                .checkpoint
                .nodes
                .iter()
                .find(|node| node.id == "scope")
                .unwrap();
            assert_eq!(scope.state, NodeState::Failed);
            assert_eq!(scope.outputs.as_ref().unwrap().verdict, None);
            assert_eq!(
                scope.outputs.as_ref().unwrap().decide_outcome,
                Some(DecideOutcome::Unanswered {
                    extra: Default::default(),
                })
            );
            advance(&document, &mut unanswered_owner, &mut unanswered_journal).unwrap();
            let mut approved_journal = MemoryJournal(journal.0.clone());
            let mut approved_owner = FakeOwner {
                now: Some(at(0)),
                actor: "chair".into(),
                observations: [terminal(AttemptOutcome::Succeeded)].into(),
                answers: std::collections::BTreeMap::from([(
                    scope_ask_id.clone().expect("the out-of-scope gate asks"),
                    reply(Some("approve"), None),
                )]),
                ..Default::default()
            };
            let approved = advance(&document, &mut approved_owner, &mut approved_journal).unwrap();
            assert_eq!(state(&approved, "writer"), NodeState::Succeeded);
            assert_eq!(approved_owner.asked, [scope_ask_id.clone().unwrap()]);
            assert!(approved_owner.owner_notifications.is_empty());
            let library = template::TemplateLibrary::bundled();
            let resolvers = std::collections::BTreeMap::new();
            let limits = crate::config::GraphLimits::default();
            let context = patch::PatchContext {
                library: &library,
                resolvers: &resolvers,
                scan: &patch::scan_for_secrets,
                journal: None,
                limits: &limits,
                worktree_layout: provenant_config::PortTable::builtin().worktree_layout,
            };
            patch::retry_node(
                &mut patch::LiveGraph {
                    document: &document,
                    checkpoint: approved_journal.0.as_mut().unwrap(),
                },
                "scope",
                None,
                &context,
            )
            .unwrap();
            let retried = advance(&document, &mut approved_owner, &mut approved_journal).unwrap();
            assert_eq!(state(&retried, "scope"), NodeState::Running);
            assert_ne!(
                retried.checkpoint.asks[0].ask_id,
                scope_ask_id.clone().unwrap()
            );
            assert_eq!(approved_owner.owner_notifications.len(), 1);
            assert_ne!(approved_owner.owner_notifications[0], first_notification);
            owner
                .answers
                .insert(scope_ask_id.unwrap(), reply(Some("hold"), None));
            let held = advance(&document, &mut owner, &mut journal).unwrap();
            assert_ne!(state(&held, "writer"), NodeState::Succeeded);
        }
    }
}

#[test]
fn principal_container_retry_does_not_reuse_owner_decision() {
    let document: GraphDocument = super::model::document_fixture(json!({
        "graph_id":"principal-retry","nodes":[
            {"id":"redesign","kind":"composite","access":"read_only","composite":{"exports":[],"nodes":[
                {"id":"design","access":"read_only","prompt":"design","route":fixture_route()},
                {"id":"scope","kind":"decide","access":"read_only","depends_on":["design"],
                 "repair_scope_gate":true,"decide":{"on_unanswered":"block_dependants", "question":"Approve the changed slice?",
                    "options":[{"id":"approve","label":"Approve"},{"id":"hold","label":"Hold"}]}}
            ]}}
        ]
    }));
    let report = "First principal slice\n```findings\n{\"schema_version\":1,\"findings\":[],\"repair_design\":{\"within_scope\":false,\"one_way_door\":true}}\n```";
    let mut owner = FakeOwner {
        now: Some(at(0)),
        actor: "chair".into(),
        observations: [terminal_with_result(AttemptOutcome::Succeeded, report)].into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let waiting = advance(&document, &mut owner, &mut journal).unwrap();
    let first_ask_id = waiting.checkpoint.asks[0].ask_id.clone();
    let first_notification = owner.owner_notifications[0].clone();
    owner
        .answers
        .insert(first_ask_id.clone(), reply(Some("approve"), None));
    assert_eq!(
        advance(&document, &mut owner, &mut journal).unwrap().status,
        GraphStatus::Complete
    );
    let library = template::TemplateLibrary::bundled();
    let resolvers = std::collections::BTreeMap::new();
    let limits = crate::config::GraphLimits::default();
    let context = patch::PatchContext {
        library: &library,
        resolvers: &resolvers,
        scan: &patch::scan_for_secrets,
        journal: None,
        limits: &limits,
        worktree_layout: provenant_config::PortTable::builtin().worktree_layout,
    };
    patch::retry_node(
        &mut patch::LiveGraph {
            document: &document,
            checkpoint: journal.0.as_mut().unwrap(),
        },
        "redesign",
        None,
        &context,
    )
    .unwrap();
    owner.observations.push_back(terminal_with_result(
        AttemptOutcome::Succeeded,
        &report.replace("First principal slice", "Revised principal slice"),
    ));
    let retried = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(retried.status, GraphStatus::Pending);
    assert_eq!(state(&retried, "redesign--scope"), NodeState::Running);
    assert_ne!(retried.checkpoint.asks[0].ask_id, first_ask_id);
    assert!(
        owner
            .filed
            .last()
            .unwrap()
            .body
            .contains("Revised principal slice")
    );
    assert_eq!(owner.owner_notifications.len(), 2);
    assert_eq!(owner.owner_notifications[0], first_notification);
    assert_ne!(owner.owner_notifications[1], first_notification);
}

#[test]
fn every_decide_outcome_round_trips_and_unanswered_has_no_option() {
    let outcomes = [
        DecideOutcome::Answered {
            extra: Default::default(),
            option: "ship".into(),
            actor: "chair".into(),
        },
        DecideOutcome::AnsweredByDefault {
            extra: Default::default(),
            option: "hold".into(),
        },
        DecideOutcome::AnsweredByPolicy {
            extra: Default::default(),
            option: "approve".into(),
            rule: "approved_repair_scope".into(),
        },
        DecideOutcome::Unanswered {
            extra: Default::default(),
        },
    ];
    for outcome in outcomes {
        let encoded = serde_json::to_value(&outcome).unwrap();
        let decoded: DecideOutcome = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(decoded, outcome);
        if matches!(&outcome, DecideOutcome::Unanswered { .. }) {
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
