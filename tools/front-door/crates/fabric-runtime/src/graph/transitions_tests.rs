//! Transition behavior tests.

#![allow(clippy::unwrap_used, reason = "test fixtures and assertions")]
use super::super::next_attempt::{
    NextAttemptDecision, NextAttemptFacts, PreparationObservation, next_attempt_transition,
};
use super::*;

fn fixture() -> (
    PlannedGraph,
    std::collections::BTreeMap<String, usize>,
    GraphCheckpoint,
    crate::config::GraphLimits,
) {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "graph_id": "pure-transitions",
            "nodes": [{"id":"one", "access":"read_only", "prompt":"do the work",
                "routes":[{"adapter":"first", "model":"first/model"}, {"adapter":"second", "model":"second/model"}]}]
        })).unwrap();
    let limits = crate::config::GraphLimits::default();
    let (plan, positions) = validate_flat_plan(&document, &limits).unwrap();
    let checkpoint = initial_checkpoint(&document, &plan, "digest".into(), None, None, None);
    (plan, positions, checkpoint, limits)
}

fn delivery_loop_fixture() -> (
    PlannedGraph,
    std::collections::BTreeMap<String, usize>,
    GraphCheckpoint,
    crate::config::GraphLimits,
) {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version": 2,
            "graph_id": "delivery-loop",
            "nodes": [{
                "id": "cycle", "kind": "loop", "access": "read_only",
                "escalate": {"ladder": ["junior", "senior"], "max_ci_rounds": 1},
                "loop": {"until": "ci", "max_rounds": 2, "body": [
                    {"id": "repair", "access": "worktree_write", "worktree": "/fixture/writer", "prompt": "repair",
                        "write_scope": ["src"], "route": {"alias": "junior"}},
                    {"id": "ci", "kind": "gate", "access": "read_only",
                        "depends_on": ["repair"], "condition": {"type": "budget"}}
                ]}
            }]
        }))
        .unwrap();
    let limits = crate::config::GraphLimits::default();
    let expanded = loops::expand(&document, &[], &limits).unwrap();
    let (plan, positions) = validate_flat_plan(&expanded, &limits).unwrap();
    let checkpoint = initial_checkpoint(&expanded, &plan, "digest".into(), None, None, None);
    (plan, positions, checkpoint, limits)
}

fn attempt_key(plan: &PlannedGraph, node_id: &str, attempt: u32) -> AttemptKey {
    AttemptKey {
        extra: Default::default(),
        retry_generation: None,
        previous_attempt_ref: None,
        graph_id: plan.graph_id.clone(),
        node_id: node_id.into(),
        attempt,
        route_index: 0,
    }
}

fn start(
    plan: &PlannedGraph,
    positions: &std::collections::BTreeMap<String, usize>,
    checkpoint: &mut GraphCheckpoint,
    limits: &crate::config::GraphLimits,
) -> AttemptKey {
    let key = super::attempt_key(plan, 0, checkpoint, 0).unwrap();
    begin_attempt(
        0,
        plan,
        checkpoint,
        positions,
        &key,
        &plan.nodes[0].routes[0],
        LaunchFacts {
            input_cap: limits.node_prompt_bytes,
            placement_host_id: None,
            launch_actor: "original-actor".into(),
            input_cap_source: Layer::Host,
            timing: None,
        },
    );
    key
}

#[test]
fn planned_escalation_preserves_the_sealed_policy() {
    let (old_plan, _, _, limits) = fixture();
    assert!(
        serde_json::to_value(&old_plan).unwrap()["nodes"][0]
            .get("escalate")
            .is_none()
    );
    let mut document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 2, "graph_id": "delivery-policy",
        "nodes": [{"id":"repair", "access":"read_only", "prompt":"repair",
            "route":{"alias":"junior"},
            "escalate":{"ladder":["junior","senior"], "max_ci_rounds":2}}]
    }))
    .unwrap();
    let (plan, _) = validate_flat_plan(&document, &limits).unwrap();
    assert_eq!(plan.nodes[0].escalate, document.nodes[0].escalate);
    assert_eq!(
        serde_json::to_value(&plan).unwrap()["nodes"][0]["escalate"]["max_ci_rounds"],
        2
    );
    document.nodes[0].escalate = None;
    let (old_plan, _) = validate_flat_plan(&document, &limits).unwrap();
    assert!(
        serde_json::to_value(&old_plan).unwrap()["nodes"][0]
            .get("escalate")
            .is_none()
    );
}

#[test]
fn escalation_owner_walks_through_hygiene_and_publication_dependencies() {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 2, "graph_id": "delivery-ancestor-route",
        "nodes": [
            {"id":"writer", "access":"worktree_write", "worktree":"/fixture/writer",
                "write_scope":["src"], "prompt":"repair", "route":{"alias":"junior"},
                "escalate":{"ladder":["junior","senior"], "max_ci_rounds":1}},
            {"id":"hygiene", "access":"read_only", "prompt":"hygiene",
                "route":{"alias":"junior"}, "depends_on":["writer"]},
            {"id":"publish", "access":"read_only", "prompt":"publish",
                "route":{"alias":"junior"}, "depends_on":["hygiene"]},
            {"id":"ci", "kind":"gate", "access":"read_only",
                "depends_on":["publish"], "condition":{"type":"budget"}}
        ]
    }))
    .unwrap();
    let limits = crate::config::GraphLimits::default();
    let (plan, positions) = validate_flat_plan(&document, &limits).unwrap();

    assert_eq!(
        loops::nearest_writer_ancestor(positions["ci"], &plan),
        Some(positions["writer"])
    );
    assert_eq!(
        loops::escalation_owner(positions["ci"], &plan)
            .map(|(index, _)| plan.nodes[index].id.as_str()),
        Some("writer")
    );
}

#[test]
fn loop_attempts_are_counted_once_and_older_replays_cannot_reopen_rounds() {
    let (plan, positions, mut checkpoint, _) = delivery_loop_fixture();
    let owner = positions["cycle"];
    let first_node = "cycle--r1--repair";
    let second_node = "cycle--r2--repair";
    let first = attempt_key(&plan, first_node, 1);
    let second = attempt_key(&plan, second_node, 2);

    record_delivery_loop_attempt(positions[first_node], &plan, &mut checkpoint, &first);
    assert_eq!(
        checkpoint.nodes[owner]
            .delivery_loop
            .as_ref()
            .unwrap()
            .round,
        1
    );
    record_delivery_loop_attempt(positions[first_node], &plan, &mut checkpoint, &first);
    assert_eq!(
        checkpoint.nodes[owner]
            .delivery_loop
            .as_ref()
            .unwrap()
            .round,
        1
    );

    record_delivery_loop_attempt(positions[second_node], &plan, &mut checkpoint, &second);
    assert_eq!(
        checkpoint.nodes[owner]
            .delivery_loop
            .as_ref()
            .unwrap()
            .round,
        2
    );
    record_delivery_loop_attempt(positions[first_node], &plan, &mut checkpoint, &first);
    let saved = checkpoint.nodes[owner].delivery_loop.as_ref().unwrap();
    assert_eq!(saved.round, 2);
    assert_eq!(saved.last_attempt, second);
}

#[test]
fn later_admitted_attempt_on_the_same_loop_node_counts_once() {
    let (plan, positions, mut checkpoint, _) = delivery_loop_fixture();
    let owner = positions["cycle"];
    let writer = "cycle--r1--repair";
    let first = attempt_key(&plan, writer, 1);
    let second = attempt_key(&plan, writer, 2);

    record_delivery_loop_attempt(positions[writer], &plan, &mut checkpoint, &first);
    record_delivery_loop_attempt(positions[writer], &plan, &mut checkpoint, &second);
    assert_eq!(
        checkpoint.nodes[owner]
            .delivery_loop
            .as_ref()
            .unwrap()
            .round,
        2
    );
    assert_eq!(
        checkpoint.nodes[owner]
            .delivery_loop
            .as_ref()
            .unwrap()
            .last_attempt,
        second
    );

    record_delivery_loop_attempt(positions[writer], &plan, &mut checkpoint, &first);
    record_delivery_loop_attempt(positions[writer], &plan, &mut checkpoint, &second);
    assert_eq!(
        checkpoint.nodes[owner]
            .delivery_loop
            .as_ref()
            .unwrap()
            .round,
        2
    );
}

#[test]
fn red_at_ci_limit_advances_once_then_exhausts_the_final_rung() {
    use provenant_records::graph::EscalationCause;
    let (plan, positions, mut checkpoint, _) = delivery_loop_fixture();
    let owner = positions["cycle"];
    let first_writer = "cycle--r1--repair";
    let first_gate = positions["cycle--r1--ci"];
    let first = attempt_key(&plan, first_writer, 1);
    record_delivery_loop_attempt(positions[first_writer], &plan, &mut checkpoint, &first);

    assert_eq!(
        advance_delivery_escalation(first_gate, &plan, &mut checkpoint, EscalationCause::CiRed),
        Some(EscalationTransition::Advanced { rung: 1 })
    );
    assert_eq!(
        checkpoint.nodes[owner]
            .delivery_loop
            .as_ref()
            .unwrap()
            .round,
        0
    );
    assert_eq!(
        loops::delivery_route(positions["cycle--r2--repair"], &plan, &checkpoint)
            .map(|(route, rung)| (route.alias, rung)),
        Some((Some("senior".into()), 1))
    );
    assert_eq!(
        advance_delivery_escalation(first_gate, &plan, &mut checkpoint, EscalationCause::CiRed),
        Some(EscalationTransition::AlreadyApplied { rung: 1 })
    );

    let second_writer = "cycle--r2--repair";
    let second_gate = positions["cycle--r2--ci"];
    let second = attempt_key(&plan, second_writer, 2);
    record_delivery_loop_attempt(positions[second_writer], &plan, &mut checkpoint, &second);
    assert_eq!(
        advance_delivery_escalation(second_gate, &plan, &mut checkpoint, EscalationCause::CiRed),
        Some(EscalationTransition::Exhausted { rung: 1 })
    );
    assert_eq!(
        advance_delivery_escalation(second_gate, &plan, &mut checkpoint, EscalationCause::CiRed),
        Some(EscalationTransition::AlreadyApplied { rung: 1 })
    );
}

#[test]
fn failed_review_repair_advances_once_then_exhausts_on_replay() {
    use provenant_records::graph::EscalationCause;
    let (plan, positions, mut checkpoint, _) = delivery_loop_fixture();
    let owner = positions["cycle"];
    let first_writer = "cycle--r1--repair";
    let first_gate = positions["cycle--r1--ci"];
    let first = attempt_key(&plan, first_writer, 1);
    record_delivery_loop_attempt(positions[first_writer], &plan, &mut checkpoint, &first);

    assert_eq!(
        advance_delivery_escalation(
            first_gate,
            &plan,
            &mut checkpoint,
            EscalationCause::ReviewRepair,
        ),
        Some(EscalationTransition::Advanced { rung: 1 })
    );
    let saved = checkpoint.nodes[owner].escalation.as_ref().unwrap();
    assert_eq!(saved.cause, EscalationCause::ReviewRepair);
    assert_eq!(saved.transition_attempt, first);
    assert_eq!(
        advance_delivery_escalation(
            first_gate,
            &plan,
            &mut checkpoint,
            EscalationCause::ReviewRepair,
        ),
        Some(EscalationTransition::AlreadyApplied { rung: 1 })
    );

    let second_writer = "cycle--r2--repair";
    let second_gate = positions["cycle--r2--ci"];
    let second = attempt_key(&plan, second_writer, 2);
    record_delivery_loop_attempt(positions[second_writer], &plan, &mut checkpoint, &second);
    assert_eq!(
        advance_delivery_escalation(
            second_gate,
            &plan,
            &mut checkpoint,
            EscalationCause::ReviewRepair,
        ),
        Some(EscalationTransition::Exhausted { rung: 1 })
    );
    assert_eq!(
        checkpoint.nodes[owner]
            .escalation
            .as_ref()
            .unwrap()
            .transition_attempt,
        second
    );
    assert_eq!(
        advance_delivery_escalation(
            second_gate,
            &plan,
            &mut checkpoint,
            EscalationCause::ReviewRepair,
        ),
        Some(EscalationTransition::AlreadyApplied { rung: 1 })
    );
}

#[test]
fn shared_worktree_writer_is_admitted_while_sibling_runs() {
    // Spec: docs/specs/supervisor.md#shared-occupancy-removal-never-forces
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "graph_id": "shared-worktree-admission",
        "nodes": [
            {"id":"one", "access":"worktree_write", "worktree":"/fixture/shared",
                "write_scope":["src/one"], "prompt":"edit the first path",
                "routes":[{"adapter":"first", "model":"first/model"}]},
            {"id":"two", "access":"worktree_write", "worktree":"/fixture/shared",
                "write_scope":["src/one"], "prompt":"edit the same claimed path",
                "routes":[{"adapter":"first", "model":"first/model"}]}
        ]
    }))
    .unwrap();
    let limits = crate::config::GraphLimits {
        concurrency: None,
        ..Default::default()
    };
    let (plan, positions) = validate_flat_plan(&document, &limits).unwrap();
    let mut checkpoint = initial_checkpoint(&document, &plan, "digest".into(), None, None, None);
    let budget = crate::config::GraphBudget::default();
    let policy = provenant_records::policy::Policy::default();

    assert!(matches!(
        admit_attempt(
            0,
            &plan,
            &mut checkpoint,
            &positions,
            &limits,
            &budget,
            &policy
        ),
        Admission::Ready
    ));
    checkpoint.nodes[0].state = NodeState::Running;

    assert!(matches!(
        admit_attempt(
            1,
            &plan,
            &mut checkpoint,
            &positions,
            &limits,
            &budget,
            &policy
        ),
        Admission::Ready
    ));
}

#[test]
fn complete_attempt_transition_needs_no_owner_or_journal() {
    // Exercise the terminal contract and fallback decisions with no fake
    // process owner or storage implementation to accidentally depend on.
    for (outcome, contract, expected_state, expected_outcome, again) in [
        (
            GraphAttemptOutcome::Succeeded,
            None,
            NodeState::Succeeded,
            GraphAttemptOutcome::Succeeded,
            false,
        ),
        (
            GraphAttemptOutcome::Succeeded,
            Some("VERDICT: PASS"),
            NodeState::Failed,
            GraphAttemptOutcome::TaskFailed,
            false,
        ),
        (
            GraphAttemptOutcome::AuthRequired,
            None,
            NodeState::Failed,
            GraphAttemptOutcome::AuthRequired,
            false,
        ),
        (
            GraphAttemptOutcome::DeadlineExceeded,
            None,
            NodeState::Failed,
            GraphAttemptOutcome::DeadlineExceeded,
            false,
        ),
    ] {
        let (mut plan, positions, mut checkpoint, limits) = fixture();
        plan.nodes[0].required_verdict = contract.map(str::to_owned);
        checkpoint.extra.insert("future_graph".into(), true.into());
        checkpoint.nodes[0]
            .extra
            .insert("future_node".into(), true.into());
        let key = start(&plan, &positions, &mut checkpoint, &limits);
        let recorded = checkpoint.nodes[0].attempts[0].clone();
        assert_eq!(recorded.launch_actor, "original-actor");
        assert!(recorded.input_manifest.is_some());
        let applied = apply_attempt_observation(
            &plan,
            &mut checkpoint,
            &positions,
            0,
            &limits,
            AttemptObservation::Terminal {
                attempt_ref: Some("host/attempt".into()),
                outcome,
                launched: true,
                result: None,
                result_warning: None,
                result_path: None,
                branch: None,
                commit: None,
                stop: None,
                settlement_class: Some(provenant_records::run::SettlementClass::Completed),
                cause: None,
            },
            ObservationFacts {
                route_index: 0,
                usage: None,
                fallback_evidence: None,
                evidence_unreadable: false,
                fallback_evidence_retry: None,
                fallback_cause: None,
                skipped_accounts: Vec::new(),
                review_range: ReviewRange::Unknown,
            },
        )
        .unwrap();
        assert_eq!(applied.should_continue, again);
        let progress = &checkpoint.nodes[0];
        assert_eq!(progress.state, expected_state);
        assert_eq!(progress.attempts[0].state, AttemptState::Terminal);
        assert_eq!(progress.attempts[0].outcome, Some(expected_outcome));
        assert_eq!(progress.attempts[0].key, key);
        assert_eq!(progress.attempts[0].launch_actor, recorded.launch_actor);
        assert_eq!(progress.attempts[0].input_manifest, recorded.input_manifest);
        assert_eq!(checkpoint.extra["future_graph"], true);
        assert_eq!(progress.extra["future_node"], true);
        if expected_outcome == GraphAttemptOutcome::DeadlineExceeded {
            assert_eq!(progress.advance_code.as_deref(), Some("deadline_exceeded"));
        }
    }
}

#[test]
fn review_settlement_records_subject_head_separately_from_base() {
    let base = "1".repeat(40);
    let head = "2".repeat(40);
    let library = super::super::template::TemplateLibrary::bundled();
    let mut node = super::super::template::Compiler::new(&library)
        .compile(&serde_json::json!({
            "id":"review", "use":"review.tests", "route":{"alias":"fixture"}
        }))
        .unwrap();
    node.source = Some(SourceBinding::Commit {
        commit: base.clone(),
    });
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 2,
        "graph_id": "review-subject-head",
        "nodes": [node]
    }))
    .unwrap();
    let limits = crate::config::GraphLimits::default();
    let (plan, positions) = validate_flat_plan(&document, &limits).unwrap();
    let mut checkpoint = initial_checkpoint(&document, &plan, "digest".into(), None, None, None);
    let key = start(&plan, &positions, &mut checkpoint, &limits);

    let mut capture = crate::result_capture::ResultCapture::new(None);
    // review.tests requires a valid findings block; without one the
    // settlement is not a review result and records no review settlement.
    capture.push(
        "```findings\n{\"schema_version\":1,\"findings\":[]}\n```\nVERDICT: PASS\n".as_bytes(),
    );
    let attempt_id = provenant_core::AttemptId::from_bytes([7; 16]);
    let observation = AttemptObservation::Terminal {
        attempt_ref: Some(format!("fixture/{attempt_id}")),
        outcome: GraphAttemptOutcome::Succeeded,
        launched: true,
        result: Some(Box::new(capture.finish())),
        result_warning: None,
        result_path: None,
        branch: None,
        commit: Some(base.clone()),
        stop: None,
        settlement_class: Some(provenant_records::run::SettlementClass::Completed),
        cause: None,
    };
    apply_attempt_observation(
        &plan,
        &mut checkpoint,
        &positions,
        0,
        &limits,
        observation,
        ObservationFacts {
            route_index: usize::from(key.route_index),
            usage: None,
            fallback_evidence: None,
            evidence_unreadable: false,
            fallback_evidence_retry: None,
            fallback_cause: None,
            skipped_accounts: Vec::new(),
            review_range: ReviewRange::Resolved {
                base: base.clone(),
                head: head.clone(),
            },
        },
    )
    .unwrap();

    let settlement = checkpoint.nodes[0].review_settlement.as_ref().unwrap();
    assert_eq!(settlement.head.to_string(), head);
    assert_eq!(settlement.base.map(|oid| oid.to_string()), Some(base));
}

/// The projected outcome is the one fact behind a writer stop: the restart
/// code follows it, and pushed refs with a done declaration no longer
/// revise it. A blocked declaration keeps its note whatever the refs (#4010).
#[test]
fn writer_stop_code_follows_the_projected_outcome() {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "graph_id": "writer-stop-code",
        "nodes": [{"id":"one", "access":"worktree_write", "worktree":"/fixture/writer", "write_scope":["src"], "prompt":"do the work",
            "routes":[{"adapter":"first", "model":"first/model"}]}]
    }))
    .unwrap();
    let limits = crate::config::GraphLimits::default();
    let (plan, positions) = validate_flat_plan(&document, &limits).unwrap();
    let base = "1111111111111111111111111111111111111111";
    let head = "2222222222222222222222222222222222222222";
    let done = provenant_records::handoff::Handoff::from_json(
        br#"{"schema_version":2,"status":"done","note":"brief complete","remaining":[],"decisions":[],"verify":[]}"#,
    )
    .unwrap();
    let blocked = provenant_records::handoff::Handoff::from_json(
        br#"{"schema_version":2,"status":"blocked","note":"waiting on owner","remaining":["ask the owner"],"decisions":[],"verify":[]}"#,
    )
    .unwrap();
    for (outcome, commit, handoff, expected_code, expected_note) in [
        (
            GraphAttemptOutcome::DeclaredStop,
            Some(head),
            None,
            Some("declared_stop"),
            None,
        ),
        (
            GraphAttemptOutcome::DeclaredStop,
            Some(base),
            None,
            Some("declared_stop"),
            None,
        ),
        (
            GraphAttemptOutcome::StoppedNoWork,
            Some(head),
            None,
            Some("stopped_no_work"),
            None,
        ),
        (
            GraphAttemptOutcome::StoppedNoWork,
            None,
            None,
            Some("stopped_no_work"),
            None,
        ),
        // Pushed refs and a done declaration no longer close the node.
        (
            GraphAttemptOutcome::DeclaredStop,
            Some(head),
            Some(done.clone()),
            Some("declared_stop"),
            None,
        ),
        // A blocked declaration travels with its note whatever the refs.
        (
            GraphAttemptOutcome::DeclaredStop,
            Some(head),
            Some(blocked.clone()),
            Some("declared_stop"),
            Some("waiting on owner"),
        ),
    ] {
        let mut checkpoint =
            initial_checkpoint(&document, &plan, "digest".into(), None, None, None);
        let _ = start(&plan, &positions, &mut checkpoint, &limits);
        checkpoint.nodes[0].source_commit = Some(base.into());
        let result = handoff.map(|handoff| {
            let mut capture = crate::result_capture::ResultCapture::new(None);
            capture.push(b"work left for the chair");
            let mut result = capture.finish();
            result.handoff = Ok(Some(handoff));
            result
        });
        apply_attempt_observation(
            &plan,
            &mut checkpoint,
            &positions,
            0,
            &limits,
            AttemptObservation::Terminal {
                attempt_ref: Some("host/attempt".into()),
                outcome,
                launched: true,
                result: result.map(Box::new),
                result_warning: None,
                result_path: None,
                branch: None,
                commit: commit.map(str::to_owned),
                stop: None,
                settlement_class: Some(provenant_records::run::SettlementClass::Completed),
                cause: None,
            },
            ObservationFacts {
                route_index: 0,
                usage: None,
                fallback_evidence: None,
                evidence_unreadable: false,
                fallback_evidence_retry: None,
                fallback_cause: None,
                skipped_accounts: Vec::new(),
                review_range: ReviewRange::Unknown,
            },
        )
        .unwrap();
        assert_eq!(
            checkpoint.nodes[0].advance_code.as_deref(),
            expected_code,
            "outcome={outcome:?} commit={commit:?}"
        );
        assert_eq!(
            checkpoint.nodes[0].attempts[0].outcome,
            Some(outcome),
            "outcome={outcome:?} commit={commit:?}"
        );
        if let Some(note) = expected_note {
            assert_eq!(
                checkpoint.nodes[0].advance_note.as_deref(),
                Some(note),
                "outcome={outcome:?} commit={commit:?}"
            );
        }
    }
}

/// #4065: an inline implement writer whose sealed settlement completed, with a
/// done handoff and a pushed delivery head, settles succeeded and unblocks its
/// dependant. No findings block is present: findings are not a writer success
/// criterion. The sealed class decides: refs and a done handoff never promote
/// a sealed failure, and an attempt with no sealed class releases nothing.
#[test]
fn inline_implement_pushed_head_and_done_handoff_unblock_dependants() {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "graph_id": "inline-implement-pushed",
        "nodes": [
            {"id":"work", "access":"worktree_write", "worktree":"/fixture/writer",
             "write_scope":["src"], "prompt":"do the work",
             "routes":[{"adapter":"first", "model":"first/model"}]},
            {"id":"after", "access":"read_only", "prompt":"follow up",
             "depends_on":["work"],
             "routes":[{"adapter":"first", "model":"first/model"}]}
        ]
    }))
    .unwrap();
    let limits = crate::config::GraphLimits::default();
    let (plan, positions) = validate_flat_plan(&document, &limits).unwrap();
    let base = "1111111111111111111111111111111111111111";
    let pushed = "2222222222222222222222222222222222222222";
    // The handoff arrives by declaration, never from the result text.
    let declared = serde_json::json!({
        "schema_version": 2, "status": "done", "note": "brief complete",
        "remaining": [], "decisions": [], "verify": []
    });
    let mut capture = crate::result_capture::ResultCapture::new(None);
    capture.push(b"brief complete");
    let mut result = capture.finish();
    result.handoff =
        provenant_records::handoff::Handoff::from_json(declared.to_string().as_bytes()).map(Some);
    for (settlement_class, releases) in [
        (
            Some(provenant_records::run::SettlementClass::Completed),
            true,
        ),
        (Some(provenant_records::run::SettlementClass::Failed), false),
        (None, false),
    ] {
        let mut checkpoint =
            initial_checkpoint(&document, &plan, "digest".into(), None, None, None);
        let _ = start(&plan, &positions, &mut checkpoint, &limits);
        checkpoint.nodes[0].source_commit = Some(base.into());
        apply_attempt_observation(
            &plan,
            &mut checkpoint,
            &positions,
            0,
            &limits,
            AttemptObservation::Terminal {
                attempt_ref: Some("host/attempt".into()),
                outcome: GraphAttemptOutcome::Succeeded,
                launched: true,
                result: Some(Box::new(result.clone())),
                result_warning: None,
                result_path: None,
                branch: Some("fix/4065".into()),
                // owner.rs supplies the pushed delivery head here (#4065).
                commit: Some(pushed.into()),
                stop: None,
                settlement_class,
                cause: None,
            },
            ObservationFacts {
                route_index: 0,
                usage: None,
                fallback_evidence: None,
                evidence_unreadable: false,
                fallback_evidence_retry: None,
                fallback_cause: None,
                skipped_accounts: Vec::new(),
                review_range: ReviewRange::Unknown,
            },
        )
        .unwrap();
        let dependant = dependency_state(
            &plan.nodes[positions["after"]],
            &plan,
            &checkpoint,
            &positions,
        );
        assert_eq!(
            checkpoint.nodes[0].state == NodeState::Succeeded,
            releases,
            "settlement_class={settlement_class:?}"
        );
        assert_eq!(
            dependant.is_none(),
            releases,
            "dependant release follows the sealed class; settlement_class={settlement_class:?}"
        );
    }
}

#[test]
fn handoff_missing_classification_keeps_dependants_closed() {
    let (plan, positions, mut checkpoint, limits) = fixture();
    start(&plan, &positions, &mut checkpoint, &limits);
    let applied = apply_attempt_observation(
        &plan,
        &mut checkpoint,
        &positions,
        0,
        &limits,
        AttemptObservation::Terminal {
            attempt_ref: Some("host/handoff-missing".into()),
            outcome: GraphAttemptOutcome::ReviewRequired,
            launched: true,
            result: None,
            result_warning: None,
            result_path: None,
            branch: None,
            commit: None,
            stop: Some(provenant_records::StopCause::HandoffMissing),
            settlement_class: Some(provenant_records::run::SettlementClass::HandoffMissing),
            cause: None,
        },
        ObservationFacts {
            route_index: 0,
            usage: None,
            fallback_evidence: None,
            evidence_unreadable: false,
            fallback_evidence_retry: None,
            fallback_cause: None,
            skipped_accounts: Vec::new(),
            review_range: ReviewRange::Unknown,
        },
    )
    .unwrap();
    assert!(!applied.should_continue);
    let progress = &checkpoint.nodes[0];
    assert_eq!(progress.state, NodeState::Failed);
    assert_eq!(
        progress.attempts[0].outcome,
        Some(GraphAttemptOutcome::OtherFailure)
    );
}

#[test]
fn next_attempt_transition_requests_only_the_facts_its_disposition_needs() {
    let (plan, positions, mut checkpoint, limits) = fixture();
    start(&plan, &positions, &mut checkpoint, &limits);
    let node = &plan.nodes[0];
    let progress = &mut checkpoint.nodes[0];
    let at = |seconds| provenant_core::WallTime::from_unix_seconds(seconds).unwrap();
    progress.launch_retry = Some(LaunchRetry {
        extra: Default::default(),
        policy: Default::default(),
        failures: 1,
        deadline: Some(at(10)),
        wake_at: Some(at(5)),
        reason: Some("busy".into()),
    });
    let mut observed = NextAttemptFacts::default();
    assert_eq!(
        next_attempt_transition(node, progress, &limits, Some(at(1)), None, &observed),
        NextAttemptDecision::Ready(NextAttempt::WaitRetryWake)
    );
    assert_eq!(
        next_attempt_transition(node, progress, &limits, Some(at(10)), None, &observed),
        NextAttemptDecision::ObserveLaunchAcceptance
    );
    observed.launch_accepted = Some(false);
    assert_eq!(
        next_attempt_transition(node, progress, &limits, Some(at(10)), None, &observed),
        NextAttemptDecision::Ready(NextAttempt::LaunchRetryExhausted {
            reason: "busy".into()
        })
    );
    observed.launch_accepted = Some(true);
    assert_eq!(
        next_attempt_transition(node, progress, &limits, Some(at(10)), None, &observed),
        NextAttemptDecision::Ready(NextAttempt::Candidate {
            route_index: 0,
            settlement_retries: None
        })
    );
    progress.launch_retry = None;
    let attempt = &mut progress.attempts[0];
    attempt.state = AttemptState::Terminal;
    attempt.launched = false;
    attempt.outcome = Some(GraphAttemptOutcome::AuthRequired);
    attempt.attempt_ref = Some("host/attempt".into());
    assert_eq!(
        next_attempt_transition(node, progress, &limits, None, None, &observed),
        NextAttemptDecision::ObservePreparationFallback
    );
    observed.fallback = Some(PreparationObservation {
        evidence: None,
        unreadable: true,
        cause: None,
    });
    assert!(matches!(
        next_attempt_transition(node, progress, &limits, None, None, &observed),
        NextAttemptDecision::Ready(NextAttempt::Fail {
            code: Some("fallback_evidence_unreadable"),
            ..
        })
    ));
}

#[test]
fn preparation_transitions_enforce_pinned_sources_and_input_capacity_without_io() {
    let (mut plan, positions, mut checkpoint, limits) = fixture();
    plan.nodes[0].kind = Some(NodeKind::Synthesise);
    checkpoint.integration_pin = Some("pinned".into());
    checkpoint.nodes[0].source_commit = Some("pinned".into());
    let expected = integration_expectations(0, &plan, &checkpoint);
    assert_eq!(
        expected,
        vec![IntegrationExpectation::Pinned("pinned".into())]
    );
    assert!(!apply_integration_observation(
        &plan.nodes[0],
        &mut checkpoint.nodes[0],
        &expected[0],
        Some("moved")
    ));
    assert_eq!(checkpoint.nodes[0].state, NodeState::Failed);
    assert_eq!(
        checkpoint.nodes[0].advance_code.as_deref(),
        Some("integration_head_moved")
    );
    plan.nodes[0].inputs.push(GraphInput {
        extra: Default::default(),
        node: "producer".into(),
        outputs: vec![OutputKind::Result],
        required_outputs: vec![],
    });
    assert!(!file_inputs_fit(
        0,
        &plan,
        &mut checkpoint,
        &positions,
        limits.node_prompt_bytes,
        limits.node_prompt_bytes
    ));
    assert_eq!(
        checkpoint.nodes[0].advance_code.as_deref(),
        Some("upstream_outputs_too_large")
    );
}

#[test]
fn sealed_cause_lands_on_the_attempt_and_the_code_comes_from_the_outcome() {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "graph_id": "sealed-cause",
        "nodes": [{"id":"one", "access":"worktree_write", "worktree":".worktrees/one",
            "prompt":"do the work", "routes":[{"adapter":"first", "model":"first/model"}]}]
    }))
    .unwrap();
    let limits = crate::config::GraphLimits::default();
    let (plan, positions) = validate_flat_plan(&document, &limits).unwrap();
    let mut checkpoint = initial_checkpoint(&document, &plan, "digest".into(), None, None, None);
    start(&plan, &positions, &mut checkpoint, &limits);
    apply_attempt_observation(
        &plan,
        &mut checkpoint,
        &positions,
        0,
        &limits,
        AttemptObservation::Terminal {
            attempt_ref: Some("host/no-work".into()),
            outcome: GraphAttemptOutcome::StoppedNoWork,
            launched: true,
            result: None,
            result_warning: None,
            result_path: None,
            branch: None,
            commit: None,
            stop: Some(provenant_records::StopCause::StoppedNoWork),
            settlement_class: Some(provenant_records::run::SettlementClass::DeclaredStop),
            cause: Some(Box::new(provenant_records::run::CauseKind::StoppedNoWork)),
        },
        ObservationFacts {
            route_index: 0,
            usage: None,
            fallback_evidence: None,
            evidence_unreadable: false,
            fallback_evidence_retry: None,
            fallback_cause: None,
            skipped_accounts: Vec::new(),
            review_range: ReviewRange::Unknown,
        },
    )
    .unwrap();
    let attempt = &checkpoint.nodes[0].attempts[0];
    assert_eq!(
        attempt.cause,
        Some(provenant_records::run::CauseKind::StoppedNoWork)
    );
    assert_eq!(attempt.outcome, Some(GraphAttemptOutcome::StoppedNoWork));
    assert_eq!(
        checkpoint.nodes[0].advance_code.as_deref(),
        Some("stopped_no_work")
    );
    let status = serde_json::to_value(&checkpoint).unwrap();
    assert_eq!(
        status["nodes"][0]["attempts"][0]["cause"],
        "stopped_no_work"
    );
}
