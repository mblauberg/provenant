use super::*;

#[test]
fn reduced_review_uses_worst_verdict_and_requires_coverage() {
    for (answers, minimum, expected, passes) in [
        (
            vec![Some("VERDICT: PASS"), Some("VERDICT: FAIL")],
            1,
            Some("VERDICT: REQUEST_CHANGES"),
            false,
        ),
        (
            vec![Some("VERDICT: FAIL"), Some("VERDICT: PASS")],
            1,
            Some("VERDICT: REQUEST_CHANGES"),
            false,
        ),
        (
            vec![Some("VERDICT: PASS"), Some("no verdict")],
            1,
            Some("VERDICT: INCONCLUSIVE"),
            false,
        ),
        (
            vec![Some("VERDICT: PASS"), Some("VERDICT: PASS")],
            1,
            Some("VERDICT: APPROVE"),
            true,
        ),
        (vec![None, None], 0, None, false),
        (vec![Some("VERDICT: PASS"), None], 2, None, false),
        (
            vec![Some("VERDICT: PASS"), Some("VERDICT: PASS")],
            3,
            None,
            false,
        ),
    ] {
        let document: GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "graph_id": "reduced-review", "nodes": [
                {"id":"reviews", "kind":"composite", "access":"read_only",
                 "composite":{"min_successful":minimum,"reduce_verdict":true,
                     "nodes":[{"id":"a","access":"read_only","prompt":"a","route":{"alias":"fixture"}},
                              {"id":"b","access":"read_only","prompt":"b","route":{"alias":"fixture"}}],
                     "exports":[{"node":"a","outputs":["result"]},{"node":"b","outputs":["result"]}]}},
                {"id":"gate","kind":"gate","access":"read_only","depends_on":["reviews"],
                 "inputs":[{"node":"reviews","outputs":["verdict"]}],
                 "condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"}}
            ]
        })).unwrap();
        let mut owner = FakeOwner {
            observations: answers
                .into_iter()
                .map(|answer| match answer {
                    Some(text) => terminal_with_result(AttemptOutcome::Succeeded, text),
                    None => terminal(AttemptOutcome::AdapterUnavailable),
                })
                .collect(),
            ..Default::default()
        };
        let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
        let composite = run
            .checkpoint
            .nodes
            .iter()
            .find(|n| n.id == "reviews")
            .unwrap();
        assert!(composite.attempts.is_empty());
        assert_eq!(
            composite
                .outputs
                .as_ref()
                .and_then(|out| out.verdict.as_deref()),
            expected
        );
        if expected.is_none() {
            assert_eq!(
                composite.advance_error_code.as_deref(),
                Some("coverage_below_minimum")
            );
        }
        let gate = run
            .checkpoint
            .nodes
            .iter()
            .find(|n| n.id == "gate")
            .unwrap();
        assert_eq!(gate.gate.as_ref().is_some_and(|gate| gate.passed), passes);
    }
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version":1, "graph_id":"blocked-review", "nodes":[
            {"id":"reviews", "kind":"composite", "access":"read_only",
             "composite":{"reduce_verdict":true,"min_successful":2,
                "nodes":[{"id":"a","access":"read_only","prompt":"a","route":{"alias":"fixture"}},
                         {"id":"b","access":"read_only","prompt":"b","route":{"alias":"fixture"},"depends_on":["a"]}],
                "exports":[{"node":"a","outputs":["result"]},{"node":"b","outputs":["result"]}]}}
        ]
    })).unwrap();
    let mut owner = FakeOwner {
        observations: [terminal(AttemptOutcome::AdapterUnavailable)].into(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    let composite = run
        .checkpoint
        .nodes
        .iter()
        .find(|n| n.id == "reviews")
        .unwrap();
    assert_eq!(
        composite.advance_error_code.as_deref(),
        Some("coverage_below_minimum")
    );
    assert!(composite.outputs.is_none());
}

#[test]
fn malformed_finding_fields_reach_composite_and_consumer() {
    let (document, mut owner) = partial_review(1);
    let malformed = findings_text(
        r#"{"id":"F1","severity":42,"file":"a.rs","summary":"issue","status":1},{"id":"F2","severity":"mystery","file":"b.rs","summary":"other","status":"open"},{"severity":"high"},{"id":"F3","severity":"high","file":"c.rs","summary":"valid finding","status":"open"}"#,
    );
    owner.observations = VecDeque::from([
        terminal_with_result(AttemptOutcome::Succeeded, &malformed),
        terminal_with_result(AttemptOutcome::Succeeded, &findings_text("")),
        terminal(AttemptOutcome::AdapterUnavailable),
        terminal_with_result(AttemptOutcome::Succeeded, "verified"),
    ]);
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    let lens = run
        .checkpoint
        .nodes
        .iter()
        .find(|n| n.id.ends_with("--lens-a"))
        .unwrap();
    assert_eq!(lens.state, NodeState::Succeeded);
    let outputs = lens.outputs.as_ref().unwrap();
    assert_eq!(outputs.findings.len(), 3);
    assert_eq!(outputs.findings[0].severity, "note");
    assert_eq!(outputs.findings[1].severity, "note");
    assert_eq!(outputs.findings[0].status, "open");
    assert_eq!(outputs.findings[2].id, "F3");
    assert_eq!(outputs.findings[2].severity, "note");
    assert_eq!(outputs.findings[2].summary, "valid finding");
    assert!(outputs.warnings.iter().any(|w| w.contains("field status")));
    assert!(
        outputs
            .warnings
            .iter()
            .any(|w| w.contains("entry 3") && w.contains("field id"))
    );
    assert!(outputs.result.is_some());
    let composite = run
        .checkpoint
        .nodes
        .iter()
        .find(|n| n.id == "lenses")
        .unwrap()
        .outputs
        .as_ref()
        .unwrap();
    assert!(
        composite
            .warnings
            .iter()
            .any(|w| w.contains("malformed finding:"))
    );
    assert!(
        composite
            .warnings
            .iter()
            .any(|w| w.contains("field status"))
    );
    assert!(owner.prompts[3].contains("malformed finding:"));
    assert!(owner.prompts[3].contains("field status"));
}

#[test]
fn fan_in_requires_all_members_unless_a_minimum_is_declared() {
    for minimum in [None, Some(2)] {
        let (mut document, mut owner) = partial_review(2);
        document.nodes[0].composite.as_mut().unwrap().min_successful = minimum;
        owner.observations[1] = terminal_with_result(AttemptOutcome::Succeeded, &findings_text(""));
        let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
        let synthesis = run
            .checkpoint
            .nodes
            .iter()
            .find(|node| node.id == "verify")
            .unwrap();
        if minimum.is_some() {
            assert_eq!(synthesis.state, NodeState::Succeeded);
            assert_eq!(run.outcome, Some(GraphOutcome::Partial));
            assert!(owner.prompts[3].contains("Unavailable members:"));
        } else {
            assert_eq!(synthesis.state, NodeState::Blocked);
            assert_eq!(run.outcome, Some(GraphOutcome::Failed));
        }
    }
}

#[test]
fn a_failed_nested_minimum_cannot_be_hidden_by_an_outer_group() {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "graph_id": "nested-minimum", "nodes": [
            {"id":"outer", "kind":"composite", "access":"read_only", "composite": {
                "min_successful":1, "nodes":[
                    {"id":"inner", "kind":"composite", "access":"read_only", "composite": {
                        "min_successful":2, "nodes":[
                            {"id":"a", "access":"read_only", "prompt":"a", "route":{"alias":"fixture"}},
                            {"id":"b", "access":"read_only", "prompt":"b", "route":{"alias":"fixture"}}
                        ], "exports":[{"node":"a","outputs":["result"]},{"node":"b","outputs":["result"]}]
                    }},
                    {"id":"c", "access":"read_only", "prompt":"c", "route":{"alias":"fixture"}}
                ], "exports":[{"node":"inner","outputs":["result"]},{"node":"c","outputs":["result"]}]
            }}
        ]
    })).unwrap();
    let mut owner = FakeOwner {
        observations: VecDeque::from([
            terminal(AttemptOutcome::OtherFailure),
            terminal_with_result(AttemptOutcome::Succeeded, "b"),
            terminal_with_result(AttemptOutcome::Succeeded, "c"),
        ]),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(
        run.checkpoint
            .nodes
            .iter()
            .find(|node| node.id == "outer")
            .unwrap()
            .state,
        NodeState::Succeeded
    );
    assert_eq!(run.outcome, Some(GraphOutcome::Failed));
}

#[test]
fn cancelled_member_counts_against_the_group_minimum() {
    // The host maps a cancelled attempt to OtherFailure.
    for (minimum, outcome) in [(2, GraphOutcome::Partial), (3, GraphOutcome::Failed)] {
        let (document, mut owner) = partial_review(minimum);
        owner.observations[1] = terminal_with_result(AttemptOutcome::Succeeded, &findings_text(""));
        owner.observations[2] = terminal(AttemptOutcome::OtherFailure);
        let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
        assert_eq!(run.outcome, Some(outcome));
    }
}

#[test]
fn review_fail_and_one_unavailable_reach_evaluator() {
    let (mut document, mut owner) = partial_review(2);
    let spec = document.nodes[0].composite.as_mut().unwrap();
    for (i, node) in spec.nodes.iter_mut().enumerate() {
        node.role = Some(format!("lens.test{i}"));
    }
    let fail = findings_text(&entry("")).replace("VERDICT: PASS", "VERDICT: FAIL");
    owner.observations = VecDeque::from([
        terminal_with_result(AttemptOutcome::DeclaredStop, &fail),
        terminal_with_result(AttemptOutcome::Succeeded, &findings_text("")),
        terminal(AttemptOutcome::AdapterUnavailable),
        terminal_with_result(AttemptOutcome::Succeeded, "evaluated"),
    ]);
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    let lens = run
        .checkpoint
        .nodes
        .iter()
        .find(|n| n.id.ends_with("--lens-a"))
        .unwrap();
    assert_eq!(lens.state, NodeState::Succeeded);
    assert_eq!(
        lens.outputs.as_ref().unwrap().verdict.as_deref(),
        Some("VERDICT: FAIL")
    );
    assert_eq!(
        run.checkpoint
            .nodes
            .iter()
            .find(|n| n.id == "verify")
            .unwrap()
            .state,
        NodeState::Succeeded
    );
    assert!(owner.prompts[3].contains("F1"));
    assert!(owner.prompts[3].contains("lens-c"));
}

#[test]
fn successful_review_with_fail_verdict_is_a_result() {
    let mut review = node("review", route("fixture"));
    review.role = Some("review.evaluator".into());
    review.required_verdict = Some("VERDICT: PASS".into());
    let result = findings_text("").replace("VERDICT: PASS", "VERDICT: FAIL");
    let mut owner = FakeOwner {
        observations: VecDeque::from([terminal_with_result(AttemptOutcome::Succeeded, &result)]),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![review]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert_eq!(run.checkpoint.nodes[0].state, NodeState::Succeeded);
    assert_eq!(
        run.checkpoint.nodes[0]
            .outputs
            .as_ref()
            .unwrap()
            .verdict
            .as_deref(),
        Some("VERDICT: FAIL")
    );
}

#[test]
fn declared_stop_requires_a_valid_review_result_and_never_accepts_a_writer() {
    for (role, text) in [
        ("lens.test", "VERDICT: FAIL".to_owned()),
        (
            "lens.test",
            "```findings\n{\"schema_version\":1,\"findings\":[]}\n```".to_owned(),
        ),
        ("write.implement", findings_text("")),
    ] {
        let mut subject = node("subject", route("fixture"));
        subject.role = Some(role.into());
        if role.starts_with("write.") {
            subject.access = AccessMode::WorktreeWrite;
            subject.worktree = Some(".worktrees/write".into());
        }
        let mut owner = FakeOwner {
            observations: VecDeque::from([terminal_with_result(
                AttemptOutcome::DeclaredStop,
                &text,
            )]),
            ..Default::default()
        };
        let run = advance(
            &graph(vec![subject]),
            &mut owner,
            &mut MemoryJournal::default(),
        )
        .unwrap();
        assert_eq!(
            run.checkpoint.nodes[0].state,
            NodeState::Failed,
            "{role}: {text}"
        );
    }
}

#[test]
fn review_all_members_quorum_blocks_evaluator_and_names_upstream() {
    let (document, mut owner) = partial_review(3);
    owner.observations[1] = terminal_with_result(AttemptOutcome::Succeeded, &findings_text(""));
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    let verify = run
        .checkpoint
        .nodes
        .iter()
        .find(|n| n.id == "verify")
        .unwrap();
    assert_eq!(verify.state, NodeState::Blocked);
    assert!(
        verify
            .advance_error
            .as_deref()
            .unwrap()
            .contains("lenses (Failed)")
    );
}

#[test]
fn replay_attributes_an_unexplained_blocked_node_without_overwriting_other_reasons() {
    let (document, mut owner) = partial_review(3);
    owner.observations[1] = terminal_with_result(AttemptOutcome::Succeeded, &findings_text(""));
    let mut journal = MemoryJournal::default();
    advance(&document, &mut owner, &mut journal).unwrap();
    let checkpoint = journal.0.as_mut().unwrap();
    let verify = checkpoint
        .nodes
        .iter_mut()
        .find(|n| n.id == "verify")
        .unwrap();
    assert_eq!(verify.state, NodeState::Blocked);
    verify.advance_error = None;
    verify.advance_error_code = None;

    let replay = advance(&document, &mut FakeOwner::default(), &mut journal).unwrap();
    let verify = replay
        .checkpoint
        .nodes
        .iter()
        .find(|n| n.id == "verify")
        .unwrap();
    assert_eq!(
        verify.advance_error_code.as_deref(),
        Some("upstream_blocked")
    );
    assert_eq!(
        verify.advance_error.as_deref(),
        Some("blocked by lenses (Failed)")
    );
    assert_eq!(journal.0.as_ref().unwrap(), &replay.checkpoint);

    let verify = journal
        .0
        .as_mut()
        .unwrap()
        .nodes
        .iter_mut()
        .find(|n| n.id == "verify")
        .unwrap();
    verify.advance_error = Some("existing reason".into());
    verify.advance_error_code = Some("other_reason".into());
    let replay = advance(&document, &mut FakeOwner::default(), &mut journal).unwrap();
    let verify = replay
        .checkpoint
        .nodes
        .iter()
        .find(|n| n.id == "verify")
        .unwrap();
    assert_eq!(verify.advance_error.as_deref(), Some("existing reason"));
    assert_eq!(verify.advance_error_code.as_deref(), Some("other_reason"));
}

#[test]
fn partial_composite_exports_only_successful_members_and_lists_the_unavailable() {
    let (document, mut owner) = partial_review(1);
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    let node = |id: &str| {
        let scoped = format!("--{id}");
        (run.checkpoint.nodes.iter())
            .find(|n| n.id == id || n.id.ends_with(&scoped))
            .unwrap()
    };
    assert_eq!(run.status, GraphStatus::Complete);
    assert_eq!(run.outcome, Some(GraphOutcome::Partial));
    assert_eq!(node("lens-a").state, NodeState::Succeeded);
    assert_eq!(node("lens-b").state, NodeState::Failed);
    assert_eq!(
        node("lens-b").advance_error_code.as_deref(),
        Some("findings_required")
    );
    assert_eq!(node("verify").state, NodeState::Succeeded);
    let lenses = node("lenses").outputs.as_ref().unwrap();
    assert_eq!(lenses.findings_available, Some(false));
    assert_eq!(lenses.findings.len(), 1);
    let excerpt = &lenses.result.as_ref().unwrap().excerpt;
    assert!(excerpt.starts_with("Unavailable members:"), "{excerpt}");
    assert!(
        excerpt
            .contains("lens-b: findings block missing or invalid: field findings: block missing")
    );
    assert!(excerpt.contains("lens-c: adapter unavailable"));
    assert!(excerpt.contains("LENS-A-OK"));
    assert!(!excerpt.contains("LENS-B-NO-FINDINGS"));
    let verify_input = &owner.prompts[3];
    assert!(verify_input.contains("lenses findings availability: unavailable"));
    assert!(verify_input.contains("LENS-A-OK") && verify_input.contains("lens-c"));
    assert!(!verify_input.contains("LENS-B-NO-FINDINGS"));
    let mut outside = run.checkpoint.clone();
    let mut failed = node("lens-b").clone();
    failed.id = "outside".into();
    outside.nodes.push(failed);
    assert_eq!(outside.outcome(), GraphOutcome::Failed);
}

#[test]
fn partial_composite_replay_recovers_legacy_minimum_and_wording() {
    let (document, mut owner) = partial_review(1);
    let mut journal = MemoryJournal::default();
    advance(&document, &mut owner, &mut journal).unwrap();
    let checkpoint = journal.0.as_mut().unwrap();
    let group = checkpoint
        .nodes
        .iter_mut()
        .find(|node| node.id == "lenses")
        .unwrap();
    group.min_successful = None;
    let result = group.outputs.as_mut().unwrap().result.as_mut().unwrap();
    result.excerpt = result
        .excerpt
        .replace("Unavailable members:", "Unavailable lenses:");
    result.digest = CanonicalV1::file_digest(result.excerpt.as_bytes()).to_string();
    let replay = advance(&document, &mut FakeOwner::default(), &mut journal).unwrap();
    assert_eq!(replay.outcome, Some(GraphOutcome::Partial));
    assert_eq!(
        replay
            .checkpoint
            .nodes
            .iter()
            .find(|node| node.id == "lenses")
            .unwrap()
            .min_successful,
        Some(1)
    );
}

#[test]
fn status_recovers_old_partial_composite_minimum() {
    let (document, mut owner) = partial_review(1);
    let mut checkpoint = advance(&document, &mut owner, &mut MemoryJournal::default())
        .unwrap()
        .checkpoint;
    checkpoint
        .nodes
        .iter_mut()
        .find(|node| node.id == "lenses")
        .unwrap()
        .min_successful = None;
    assert_eq!(checkpoint.outcome(), GraphOutcome::Failed);
    use provenant_core::Clock;
    let temp = crate::test_support::tempdir().unwrap();
    let base = std::fs::canonicalize(temp.path()).unwrap();
    let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("checkout"));
    inputs.state_dir = Some(base.clone());
    let root = StateRoot::resolve(&inputs, &provenant_os::fs::SystemPathResolver).unwrap();
    let project = provenant_core::ProjectId::from_bytes([1; 16]);
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
    let mut journal =
        FileJournal::open(&root, project, &document.graph_id, budget, &cancel, &clock).unwrap();
    journal
        .bind(GraphExecution {
            review_base: None,
            extra: Default::default(),
            integration_base: None,
            directory: base,
            document: document.clone(),
        })
        .unwrap();
    journal.save(&checkpoint).unwrap();
    let display = FileJournal::read_status_checkpoint(&root, project, &document.graph_id)
        .unwrap()
        .unwrap();
    assert_eq!(display.outcome(), GraphOutcome::Partial);
    assert_eq!(
        FileJournal::read_checkpoint(&root, project, &document.graph_id)
            .unwrap()
            .unwrap()
            .outcome(),
        GraphOutcome::Failed
    );
}

#[test]
fn legacy_partial_output_replays_with_warnings_and_truncated_authored_text() {
    let mut members: Vec<_> = (0..17).map(|i| serde_json::json!({
        "id":format!("worker-{i}"), "access":"read_only", "prompt":"work", "route":{"alias":"fixture"}
    })).collect();
    members.push(serde_json::json!({"id":"failed", "access":"read_only", "prompt":"work", "route":{"alias":"fixture"}}));
    let exports: Vec<_> = members
        .iter()
        .map(|member| serde_json::json!({"node":member["id"],"outputs":["result"]}))
        .collect();
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version":1, "graph_id":"legacy-partial", "nodes":[{
            "id":"group", "kind":"composite", "access":"read_only",
            "composite":{"min_successful":1,"nodes":members,"exports":exports}
        }]
    }))
    .unwrap();
    let body = format!("member failed {}", "x".repeat(4096));
    let mut observations: VecDeque<_> = (0..17)
        .map(|_| terminal_with_result(AttemptOutcome::Succeeded, &body))
        .collect();
    observations.push_back(terminal(AttemptOutcome::TaskFailed));
    let mut owner = FakeOwner {
        observations,
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    advance(&document, &mut owner, &mut journal).unwrap();
    let checkpoint = journal.0.as_mut().unwrap();
    let mut body = String::new();
    for i in 0..17 {
        let id = format!("group--worker-{i}");
        let member = checkpoint.nodes.iter().find(|node| node.id == id).unwrap();
        body.push_str(&format!(
            "{id}:\n{}\n",
            member
                .outputs
                .as_ref()
                .unwrap()
                .result
                .as_ref()
                .unwrap()
                .excerpt
        ));
    }
    let group = checkpoint
        .nodes
        .iter_mut()
        .find(|node| node.id == "group")
        .unwrap();
    let output = group.outputs.as_mut().unwrap();
    assert_eq!(
        output.warnings,
        ["unavailable group--failed: member failed"]
    );
    output.warnings[0] = "unavailable group--failed: lens failed".into();
    let header = "Unavailable lenses:\ngroup--failed: lens failed";
    let room = COMPOSITE_EXCERPT_BYTES - header.len() - 2;
    assert!(body.len() > room);
    let body = bounded(&body, COMPOSITE_EXCERPT_BYTES);
    let excerpt = format!("{header}\n\n{}", bounded(&body, room));
    let result = output.result.as_mut().unwrap();
    assert!(result.truncated);
    result.digest = CanonicalV1::file_digest(excerpt.as_bytes()).to_string();
    result.excerpt = excerpt;
    assert!(result.excerpt.contains("member failed"));
    advance(&document, &mut FakeOwner::default(), &mut journal).unwrap();
}

#[test]
fn qualified_findings_survive_model_synthesis() {
    for dispositions in [vec![], vec!["accepted"]] {
        let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version":1,"graph_id":"qualified-findings","nodes":[
            {"id":"lenses","kind":"composite","access":"read_only",
             "composite":{"min_successful":1,"nodes":[
                 {"id":"tests","template":"review.tests","review_lens":"tests","access":"read_only","prompt":"review","route":{"alias":"first"}}
             ],"exports":[{"node":"tests","outputs":["findings"]}]}},
            {"id":"synthesis","kind":"synthesise","access":"read_only","prompt":"synthesise","route":{"alias":"second"},
             "depends_on":["lenses"],"dispositions":dispositions,
             "inputs":[{"node":"lenses","outputs":["findings"]}]}
        ]
    })).unwrap();
        let finding = entry(
            ",\"lens\":\"wrong\",\"evidence\":\"observed failure\",\"proposed_fix\":\"repair branch\"",
        );
        let mut owner = FakeOwner {
            observations: VecDeque::from([
                terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&finding)),
                terminal_with_result(
                    AttemptOutcome::Succeeded,
                    &findings_text(r#"{"id":"F1","disposition":"accepted","reason":"verified"}"#),
                ),
            ]),
            ..Default::default()
        };
        let mut journal = MemoryJournal::default();
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        for id in ["lenses", "synthesis"] {
            let node = run
                .checkpoint
                .nodes
                .iter()
                .find(|node| node.id == id)
                .unwrap();
            let finding = &node.outputs.as_ref().unwrap().findings[0];
            assert!(finding.id.ends_with(":F1"));
            assert_eq!(finding.effective_severity(), "note");
            assert_eq!((finding.file.as_str(), finding.line), ("a.rs", None));
            assert_eq!(finding.extra["lens"], "tests");
            assert_eq!(finding.extra["evidence"], "observed failure");
            assert_eq!(finding.extra["proposed_fix"], "repair branch");
        }
        assert!(owner.prompts[1].contains("\"lens\":\"tests\""));
        assert!(owner.prompts[1].contains("\"evidence\":\"observed failure\""));
        assert!(owner.prompts[1].contains("\"proposed_fix\":\"repair branch\""));
        assert_eq!(
            advance(&document, &mut FakeOwner::default(), &mut journal)
                .unwrap()
                .checkpoint,
            run.checkpoint
        );
    }
}

#[test]
fn qualified_findings_survive_deterministic_review_reduction() {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version":1,"graph_id":"reduced-finding-lenses","nodes":[
            {"id":"panel","kind":"composite","access":"read_only",
             "composite":{"min_successful":1,"reduce_verdict":true,"nodes":[
                 {"id":"tests","template":"review.tests","review_lens":"tests","access":"read_only","prompt":"review","route":{"alias":"first"}},
                 {"id":"security","template":"review.security","review_lens":"security","access":"read_only","prompt":"review","route":{"alias":"second"}}
             ],"exports":[{"node":"tests","outputs":["result","findings"]},{"node":"security","outputs":["result","findings"]}]}}
        ]
    })).unwrap();
    let finding =
        entry(",\"lens\":\"wrong\",\"evidence\":\"observed\",\"proposed_fix\":\"repair\"");
    let result = format!("{}\nVERDICT: FAIL", findings_text(&finding));
    let mut owner = FakeOwner {
        observations: VecDeque::from([
            terminal_with_result(AttemptOutcome::Succeeded, &result),
            terminal_with_result(AttemptOutcome::Succeeded, &result),
        ]),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    let panel = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "panel")
        .unwrap();
    assert!(panel.attempts.is_empty());
    let findings = &panel.outputs.as_ref().unwrap().findings;
    assert_eq!(findings.len(), 2);
    for (finding, lens) in findings.iter().zip(["tests", "security"]) {
        assert_eq!(finding.extra["lens"], lens);
        assert_eq!(finding.extra["evidence"], "observed");
        assert_eq!(finding.extra["proposed_fix"], "repair");
    }
    assert_eq!(
        advance(&document, &mut FakeOwner::default(), &mut journal)
            .unwrap()
            .checkpoint,
        run.checkpoint
    );
}

#[test]
fn partial_composite_below_its_minimum_fails_with_the_recorded_reason() {
    let (document, mut owner) = partial_review(2);
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    let lenses = run
        .checkpoint
        .nodes
        .iter()
        .find(|n| n.id == "lenses")
        .unwrap();
    assert_eq!(lenses.state, NodeState::Failed);
    assert_eq!(
        lenses.advance_error_code.as_deref(),
        Some("coverage_below_minimum")
    );
    let reason = lenses.advance_error.as_deref().unwrap();
    assert!(
        reason.contains("1 of 3 members succeeded, minimum 2"),
        "{reason}"
    );
    assert!(
        reason.contains("lens-b") && reason.contains("lens-c"),
        "{reason}"
    );
    assert_eq!(owner.prompts.len(), 3, "verify must not run");
    assert!(!run.checkpoint.successful());
    assert_eq!(run.outcome, Some(GraphOutcome::Failed));
    let replay = advance(&document, &mut FakeOwner::default(), &mut journal).unwrap();
    assert_eq!(replay.checkpoint, run.checkpoint);
}

#[test]
fn composite_executes_nested_children_and_exports_only_declared_outputs() {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version":1,"graph_id":"composite-test","nodes":[{
            "id":"swarm","kind":"composite","access":"read_only",
            "composite":{"nodes":[{
                "id":"nested","kind":"composite","access":"read_only",
                "composite":{"nodes":[{
                    "id":"scout","access":"read_only","prompt":"scout", "route":{"alias":"junior"}
                }],"exports":[{"node":"scout","outputs":["result"]}]}
            }],"exports":[{"node":"nested","outputs":["result"]}]}
        }, {
            "id":"consume","access":"read_only","prompt":"consume", "route":{"alias":"senior"},
            "depends_on":["swarm"],"inputs":[{"node":"swarm","outputs":["result"]}]
        }]
    }))
    .unwrap();
    let mut owner = FakeOwner {
        observations: [
            terminal_with_result(AttemptOutcome::Succeeded, "evidence"),
            terminal_with_result(AttemptOutcome::Succeeded, "done"),
        ]
        .into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(run.checkpoint.successful());
    assert_eq!(owner.calls.len(), 2);
    assert_eq!(owner.calls[0].0.node_id, "swarm--nested--scout");
    let swarm = run
        .checkpoint
        .nodes
        .iter()
        .find(|n| n.id == "swarm")
        .unwrap();
    assert_eq!(swarm.state, NodeState::Succeeded);
    assert!(swarm.attempts.is_empty());
    assert_eq!(
        swarm
            .outputs
            .as_ref()
            .unwrap()
            .result
            .as_ref()
            .unwrap()
            .excerpt,
        "evidence"
    );
    assert_eq!(swarm.outputs.as_ref().unwrap().verdict, None);
    assert!(owner.prompts[1].contains("evidence"));
    let replay = advance(&document, &mut FakeOwner::default(), &mut journal).unwrap();
    assert_eq!(run.checkpoint, replay.checkpoint);
}
