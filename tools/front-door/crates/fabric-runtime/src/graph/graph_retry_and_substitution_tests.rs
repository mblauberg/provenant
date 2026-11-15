use super::*;

#[test]
fn cancelled_graph_admits_no_work_and_resume_reuses_the_attempt_key() {
    let mut child = node("child", route("route"));
    child.depends_on.push("node".into());
    let document = graph(vec![node("node", route("route")), child]);
    let mut owner = FakeOwner {
        observations: VecDeque::from([
            AttemptObservation::Pending {
                attempt_ref: Some("host/attempt".into()),
            },
            terminal(AttemptOutcome::Succeeded),
            terminal(AttemptOutcome::Succeeded),
        ]),
        ..FakeOwner::default()
    };
    let mut journal = MemoryJournal::default();
    let pending = advance(&document, &mut owner, &mut journal).unwrap();
    journal.0.as_mut().unwrap().cancelled = true;
    let stopped = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(stopped.status, GraphStatus::Cancelled);
    assert_eq!(stopped.checkpoint.nodes[0].state, NodeState::Running);
    assert_eq!(stopped.checkpoint.nodes[1].state, NodeState::Skipped);
    assert!(stopped.checkpoint.cancelled);
    owner.cancelled_attempt_settled = true;
    let settled = advance(&document, &mut owner, &mut journal).unwrap();
    assert!(
        settled
            .checkpoint
            .nodes
            .iter()
            .all(|node| node.state == NodeState::Skipped)
    );
    assert!(settled.checkpoint.cancelled);
    let completed = resume(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(completed.status, GraphStatus::Complete);
    assert_eq!(
        pending.checkpoint.nodes[0].attempts[0].key,
        completed.checkpoint.nodes[0].attempts[0].key
    );
}

#[test]
fn resume_after_a_crash_before_the_reference_save_replays_under_the_launch_actor() {
    let document = graph(vec![node("node", route("route"))]);
    let mut journal = MemoryJournal::default();
    let mut first = FakeOwner {
        actor: "actor-a".into(),
        observations: VecDeque::from([AttemptObservation::Pending { attempt_ref: None }]),
        ..FakeOwner::default()
    };
    advance(&document, &mut first, &mut journal).unwrap();
    // The crash window: the run store accepted the attempt but no reference was saved.
    let attempt = &mut journal.0.as_mut().unwrap().nodes[0].attempts[0];
    assert_eq!(attempt.launch_actor, "actor-a");
    attempt.attempt_ref = None;
    let mut second = FakeOwner {
        actor: "actor-b".into(),
        observations: VecDeque::from([terminal(AttemptOutcome::Succeeded)]),
        ..FakeOwner::default()
    };
    resume(&document, &mut second, &mut journal).unwrap();
    assert_eq!(second.launch_actors, vec!["actor-a".to_owned()]);
    assert_eq!(second.stored_refs, vec![None]);
}

#[test]
fn terminal_task_failure_does_not_try_a_fallback_route() {
    let mut task = node("node", route("first"));
    task.route = None;
    task.routes = Some(vec![route("first"), route("second")]);
    let document = graph(vec![task]);
    let mut owner = FakeOwner {
        observations: VecDeque::from([terminal(AttemptOutcome::TaskFailed)]),
        ..FakeOwner::default()
    };
    let mut journal = MemoryJournal::default();

    let run = advance(&document, &mut owner, &mut journal).expect("terminal task failure");

    assert_eq!(run.status, GraphStatus::Complete);
    assert_eq!(run.checkpoint.nodes[0].state, NodeState::Failed);
    let progress = &run.checkpoint.nodes[0];
    assert!(progress.previous_attempts.is_empty());
    let failure = progress
        .attempts
        .last()
        .expect("failed attempt is recorded");
    assert_eq!(failure.route, route("first"));
    assert_eq!(failure.outcome, Some(AttemptOutcome::TaskFailed));
}

#[test]
fn review_task_substitution_is_bounded_and_replays() {
    for role in ["lens.tests", "review.verify"] {
        for cap in [0, 1] {
            let mut review = node("review", route("first"));
            review.role = Some(role.into());
            review.route = None;
            review.routes = Some(vec![route("first"), route("second"), route("third")]);
            let document = graph(vec![review]);
            let mut owner = FakeOwner {
                observations: VecDeque::from([
                    terminal(AttemptOutcome::TaskFailed),
                    terminal(AttemptOutcome::Succeeded),
                ]),
                ..Default::default()
            };
            let mut journal = MemoryJournal::default();
            let limits = crate::config::GraphLimits {
                review_substitutions: cap,
                ..Default::default()
            };
            let run = advance_with_budget(
                &document,
                &mut owner,
                &mut journal,
                &limits,
                &Default::default(),
            )
            .unwrap();
            assert_eq!(run.checkpoint.nodes.len(), 1);
            let substitutions = run.checkpoint.nodes[0]
                .previous_attempts
                .iter()
                .chain(&run.checkpoint.nodes[0].attempts)
                .filter(|attempt| attempt.review_substitution)
                .count();
            assert!(substitutions <= usize::from(cap));
            assert_eq!(substitutions > 0, cap > 0);
            assert_eq!(
                run.checkpoint.nodes[0].state,
                if cap == 0 {
                    NodeState::Failed
                } else {
                    NodeState::Succeeded
                }
            );
            let replay = advance_with_budget(
                &document,
                &mut FakeOwner::default(),
                &mut journal,
                &limits,
                &Default::default(),
            )
            .unwrap();
            assert_eq!(replay.checkpoint, run.checkpoint);
        }
    }
}

#[test]
fn lowered_substitution_limit_settles_an_unstarted_retry() {
    let mut review = node("review", route("first"));
    review.role = Some("lens.tests".into());
    review.route = None;
    review.routes = Some(vec![route("first"), route("second")]);
    let document = graph(vec![review]);
    let mut owner = FakeOwner {
        observations: VecDeque::from([
            terminal(AttemptOutcome::TaskFailed),
            AttemptObservation::Pending { attempt_ref: None },
        ]),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    advance(&document, &mut owner, &mut journal).unwrap();
    let progress = &mut journal.0.as_mut().unwrap().nodes[0];
    progress.attempts.pop();
    progress.outputs = None;
    progress.state = NodeState::Running;
    let limits = crate::config::GraphLimits {
        review_substitutions: 0,
        ..Default::default()
    };
    let mut owner = FakeOwner::default();
    let resumed = advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &limits,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(resumed.checkpoint.nodes[0].state, NodeState::Failed);
    assert!(resumed.checkpoint.nodes[0].attempts.is_empty());
    assert_eq!(
        resumed.checkpoint.nodes[0].advance_error_code.as_deref(),
        Some("review_substitution_limit")
    );
}

#[test]
fn uncertain_review_launch_does_not_substitute() {
    let mut review = node("review", route("first"));
    review.role = Some("lens.tests".into());
    review.route = None;
    review.routes = Some(vec![route("first"), route("second")]);
    let mut owner = FakeOwner {
        observations: VecDeque::from([AttemptObservation::Unknown { attempt_ref: None }]),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![review]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert_eq!(
        run.checkpoint.nodes[0].attempts[0].state,
        AttemptState::Unresolved
    );
    assert!(
        run.checkpoint.nodes[0]
            .attempts
            .iter()
            .all(|attempt| !attempt.review_substitution)
    );
}

#[test]
fn review_substitution_stops_at_the_limit_and_verifier_unblocks_synthesis() {
    let mut verify = node("verify", route("first"));
    verify.role = Some("review.verify".into());
    verify.route = None;
    verify.routes = Some(vec![route("first"), route("second"), route("third")]);
    let mut synthesis = node("synthesis", route("synth"));
    synthesis.depends_on.push("verify".into());
    let document = graph(vec![verify, synthesis]);
    for succeeds in [true, false] {
        let mut owner = FakeOwner {
            observations: VecDeque::from([
                terminal(AttemptOutcome::TaskFailed),
                terminal(if succeeds {
                    AttemptOutcome::Succeeded
                } else {
                    AttemptOutcome::TaskFailed
                }),
                terminal(AttemptOutcome::Succeeded),
            ]),
            ..Default::default()
        };
        let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
        assert_eq!(run.checkpoint.nodes[0].attempts.len(), 2);
        assert_eq!(
            run.checkpoint.nodes[1].state,
            if succeeds {
                NodeState::Succeeded
            } else {
                NodeState::Blocked
            }
        );
    }
}

#[test]
fn verifier_preparation_substitution_requires_evidence_and_respects_limit() {
    let mut verify = node("verify", route("first"));
    verify.role = Some("review.verify".into());
    verify.route = None;
    verify.routes = Some(vec![route("first"), route("second"), route("third")]);
    let mut synthesis = node("synthesis", route("synth"));
    synthesis.depends_on.push("verify".into());
    let document = graph(vec![verify, synthesis]);
    for (limit, evidence, succeeds) in [(1, true, true), (0, true, false), (1, false, false)] {
        let mut owner = FakeOwner {
            omit_fallback_evidence: !evidence,
            observations: VecDeque::from([
                preparation_refusal("host/first", AttemptOutcome::AdapterUnavailable),
                terminal(AttemptOutcome::Succeeded),
                terminal(AttemptOutcome::Succeeded),
            ]),
            ..Default::default()
        };
        let limits = crate::config::GraphLimits {
            review_substitutions: limit,
            ..Default::default()
        };
        let mut journal = MemoryJournal::default();
        let run = advance_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &limits,
            &Default::default(),
        )
        .unwrap();
        assert_eq!(
            run.checkpoint.nodes[1].state,
            if succeeds {
                NodeState::Succeeded
            } else {
                NodeState::Blocked
            }
        );
        assert_eq!(
            run.checkpoint.nodes[0].attempts.len(),
            if succeeds { 2 } else { 1 }
        );
        if succeeds {
            assert!(run.checkpoint.nodes[0].attempts[1].review_substitution);
        }
        assert_eq!(
            advance_with_budget(
                &document,
                &mut FakeOwner::default(),
                &mut journal,
                &limits,
                &Default::default()
            )
            .unwrap()
            .checkpoint,
            run.checkpoint
        );
    }
    let mut owner = FakeOwner {
        observations: VecDeque::from([
            preparation_refusal("host/first", AttemptOutcome::AdapterUnavailable),
            terminal(AttemptOutcome::TaskFailed),
            terminal(AttemptOutcome::Succeeded),
        ]),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.checkpoint.nodes[0].attempts.len(), 2);
    assert_eq!(run.checkpoint.nodes[1].state, NodeState::Blocked);
}

#[test]
fn review_substitute_keeps_writer_chain_exclusion() {
    let mut write = node("write", route("first"));
    write.access = AccessMode::WorktreeWrite;
    write.worktree = Some(".worktrees/write".into());
    let mut review = node("review", route("first"));
    review.role = Some("lens.tests".into());
    review.source = Some(SourceBinding::Node {
        node: "write".into(),
        extra: Default::default(),
    });
    review.distinct_from = Some(DistinctFrom::Writer);
    review.depends_on.push("write".into());
    review.route = None;
    review.routes = Some(vec![route("first"), route("second")]);
    let writer = AttemptObservation::Terminal {
        attempt_ref: Some("host/writer".into()),
        outcome: AttemptOutcome::Succeeded,
        launched: true,
        result: None,
        result_warning: None,
        result_path: None,
        branch: Some("writer".into()),
        commit: Some("3".repeat(40)),
        settlement_reason: Some(provenant_records::run::SettlementReason::Completed),
        stop: None,
    };
    let mut owner = FakeOwner {
        observations: VecDeque::from([
            writer,
            terminal(AttemptOutcome::TaskFailed),
            terminal(AttemptOutcome::Succeeded),
        ]),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![write, review]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert!(run.checkpoint.successful());
    assert_eq!(owner.distinct_from[1], ["host/writer"]);
    assert_eq!(owner.distinct_from[2], ["host/writer"]);
}

#[test]
fn review_substitute_prefers_a_new_family_but_can_use_the_same_family() {
    for (replacement, outcome) in [
        ("different", SeparationOutcome::Separated),
        ("same", SeparationOutcome::Unavailable),
    ] {
        let mut review = node("review", route("first"));
        review.role = Some("lens.tests".into());
        review.route = None;
        let alias: GraphRoute =
            serde_json::from_value(serde_json::json!({"alias":"senior"})).unwrap();
        review.routes = Some(vec![alias.clone(), alias]);
        let mut first = done_as("host/first");
        if let AttemptObservation::Terminal { outcome, .. } = &mut first {
            *outcome = AttemptOutcome::TaskFailed;
        }
        let mut owner = FakeOwner {
            observations: VecDeque::from([first, done_as("host/second")]),
            families: [
                ("host/first".into(), "same".into()),
                ("host/second".into(), replacement.into()),
            ]
            .into(),
            models: [("host/first".into(), "same/model".into())].into(),
            ..Default::default()
        };
        let run = advance(
            &graph(vec![review]),
            &mut owner,
            &mut MemoryJournal::default(),
        )
        .unwrap();
        assert!(run.checkpoint.successful());
        assert_eq!(owner.family_peers[1], ["same"]);
        assert!(owner.prior_attempts[1].is_empty());
        assert_eq!(
            run.checkpoint.nodes[0].separation.as_ref().unwrap().outcome,
            outcome
        );
    }
}

#[test]
fn configured_graph_concurrency_refusal_uses_the_owner_policy() {
    use provenant_records::policy::{Policy, Source, Strength};
    let document = graph(vec![
        node("one", route("first")),
        node("two", route("first")),
    ]);
    let limits = crate::config::GraphLimits {
        concurrency: Some(1),
        ..Default::default()
    };
    let mut policy = Policy::default();
    policy.overlay(
        &[("GRAPH_CONCURRENCY_CAP".to_owned(), Strength::Refuse)].into(),
        Source::Project,
    );
    let mut owner = FakeOwner {
        admission_policy: Some(policy),
        observations: VecDeque::from([AttemptObservation::Pending { attempt_ref: None }]),
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
    let refused = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "two")
        .unwrap();
    assert_eq!(refused.state, NodeState::Failed);
    assert_eq!(
        refused.advance_error_code.as_deref(),
        Some("concurrency_cap")
    );
    assert!(refused.concurrency_warning.is_none());
    assert!(
        run.checkpoint
            .nodes
            .iter()
            .any(|node| { node.id == "one" && node.state == NodeState::Running })
    );
    assert!(refused.attempts.is_empty());
}

#[test]
fn graph_concurrency_cap_warns_and_admits_the_next_node() {
    let document = graph(vec![
        node("one", route("first")),
        node("two", route("first")),
        node("three", route("first")),
    ]);
    let limits = crate::config::GraphLimits {
        concurrency: Some(2),
        ..Default::default()
    };
    let mut owner = FakeOwner {
        observations: VecDeque::from(vec![AttemptObservation::Pending { attempt_ref: None }; 3]),
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
    assert_eq!(run.status, GraphStatus::Pending);
    assert!(
        run.checkpoint
            .nodes
            .iter()
            .all(|node| { node.state == NodeState::Running && !node.attempts.is_empty() })
    );
    let third = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "three")
        .unwrap();
    assert!(third.concurrency_warning.is_some());
}
