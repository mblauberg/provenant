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

// Spec: docs/specs/fabric.md#composite-settlement
#[test]
fn soft_panel_settles_and_names_its_stopped_lens() {
    let clean = "```findings\n{\"schema_version\":1,\"findings\":[]}\n```\nVERDICT: PASS\n";
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "graph_id": "soft-nine-lens-review", "nodes": [{
            "id":"lenses", "kind":"composite", "access":"read_only",
            "composite":{"min_successful":3,"reduce_verdict":true,"soft_review_coverage":true,
                "nodes":(0..3).map(|index| serde_json::json!({
                    "id":format!("lens-{}", index), "access":"read_only",
                    "review_lens":format!("lens-{}", index), "prompt":"review",
                    "review_behaviour":"lens", "route":{"alias":"fixture"}
                })).collect::<Vec<_>>(),
                "exports":(0..3).map(|index| serde_json::json!({
                    "node":format!("lens-{}", index), "outputs":["result","findings","verdict"]
                })).collect::<Vec<_>>()}
        }]
    }))
    .unwrap();
    let mut owner = FakeOwner {
        observations: (0..3)
            .map(|index| {
                if index == 1 {
                    terminal(AttemptOutcome::DeclaredStop)
                } else {
                    terminal_with_result(AttemptOutcome::Succeeded, clean)
                }
            })
            .collect(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    let panel = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "lenses")
        .unwrap();
    assert_eq!(panel.state, NodeState::Succeeded);
    let output = panel.outputs.as_ref().unwrap();
    assert_eq!(output.verdict.as_deref(), Some("VERDICT: INCONCLUSIVE"));
    assert!(output.warnings.iter().any(|warning| {
        warning.contains("review_coverage_missing_lenses") && warning.contains("lens-1")
    }));
    let excerpt = &output.result.as_ref().unwrap().excerpt;
    assert!(excerpt.contains("Missing review lenses:"), "{excerpt}");
    assert!(excerpt.contains("lens-1"), "{excerpt}");
    assert!(output.warnings.iter().any(|warning| warning.contains("review_coverage_below_floor")));
    assert_eq!(panel.advance_error_code, None);
    assert_eq!(
        advance(&document, &mut owner, &mut journal)
            .unwrap()
            .checkpoint,
        run.checkpoint
    );
}

// Spec: docs/specs/fabric.md#composite-settlement
#[test]
fn final_panel_settlement_keeps_missing_lens_names_through_synthesis() {
    let clean = "```findings\n{\"schema_version\":1,\"findings\":[]}\n```\nVERDICT: PASS\n";
    let lens = |id: &str| {
        serde_json::json!({
            "id":id, "access":"read_only", "review_lens":id,
            "review_behaviour":"lens", "require_findings":true,
            "required_verdict":"VERDICT: PASS", "prompt":"review",
            "route":{"alias":"fixture"}
        })
    };
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "graph_id": "synthesis-missing-lens", "nodes": [
          {"id":"prior-panel", "kind":"composite", "access":"read_only",
           "composite":{"min_successful":1,"reduce_verdict":true,"soft_review_coverage":true,
             "nodes":[lens("lens-prior")],
             "exports":[{"node":"lens-prior","outputs":["result","findings","verdict"]}]}},
          {
            "id":"panel", "kind":"composite", "access":"read_only",
            "depends_on":["prior-panel"],
            "composite":{"min_successful":1,"reduce_verdict":true,"soft_review_coverage":true,
                "nodes":[
                    {"id":"lenses","kind":"composite","access":"read_only",
                     "composite":{"min_successful":1,"reduce_verdict":true,"soft_review_coverage":true,
                        "hard_review_coverage":true,"required_review_lenses":["lens-security"],
                        "nodes":[lens("lens-correctness"),lens("lens-security")],
                        "exports":[
                            {"node":"lens-correctness","outputs":["result","findings","verdict"]},
                            {"node":"lens-security","outputs":["result","findings","verdict"]}
                        ]}},
                    {"id":"verify","access":"read_only","review_behaviour":"verify",
                     "require_findings":true,"required_verdict":"VERDICT: PASS","prompt":"verify",
                     "after_settled":["lenses"],"inputs":[{"node":"lenses","outputs":["result","findings"]}],
                     "route":{"alias":"fixture"}},
                    {"id":"synthesis","kind":"synthesise","access":"read_only",
                     "review_behaviour":"synthesis","require_findings":true,
                     "required_verdict":"VERDICT: PASS","dispositions":["accepted","rejected","deferred"],
                     "prompt":"synthesis","depends_on":["prior-panel"],"after_settled":["lenses","verify"],
                     "inputs":[{"node":"lenses","outputs":["result","findings"]},{"node":"verify","outputs":["result","findings","verdict"]}],
                     "route":{"alias":"fixture"}}
                ],
                "exports":[{"node":"synthesis","outputs":["result","findings","verdict"]}]}
          }
        ]
    })).unwrap();
    let mut owner = FakeOwner {
        observations: [
            terminal(AttemptOutcome::DeclaredStop),
            terminal_with_result(AttemptOutcome::Succeeded, clean),
            terminal(AttemptOutcome::DeclaredStop),
            terminal_with_result(AttemptOutcome::Succeeded, clean),
            terminal_with_result(AttemptOutcome::Succeeded, clean),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    for stage in ["panel--verify", "panel--synthesis"] {
        assert_eq!(run.checkpoint.nodes.iter().find(|node| node.id == stage).unwrap().state, NodeState::Succeeded, "{stage}");
    }
    let panel = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "panel")
        .unwrap();
    assert_eq!(panel.state, NodeState::Succeeded);
    let output = panel.outputs.as_ref().unwrap();
    assert_eq!(output.verdict.as_deref(), Some("VERDICT: INCONCLUSIVE"));
    assert!(output.warnings.iter().any(|warning| {
        warning.contains("review_coverage_missing_lenses") && warning.contains("lens-security")
    }));
    assert!(
        !output
            .warnings
            .iter()
            .any(|warning| warning.contains("lens-prior"))
    );
    let excerpt = &output.result.as_ref().unwrap().excerpt;
    assert!(excerpt.contains("Missing review lenses:"), "{excerpt}");
    assert!(excerpt.contains("lens-security"), "{excerpt}");
    assert!(!excerpt.contains("lens-prior"), "{excerpt}");
}

// Spec: docs/specs/fabric.md#composite-settlement
#[test]
fn protected_panel_hard_floor_requires_named_lens_even_when_count_is_met() {
    let clean = "```findings\n{\"schema_version\":1,\"findings\":[]}\n```\nVERDICT: PASS\n";
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "graph_id": "protected-review-floor", "nodes": [{
            "id":"lenses", "kind":"composite", "access":"read_only",
            "composite":{"min_successful":1,"reduce_verdict":true,"soft_review_coverage":true,
                "hard_review_coverage":true,"required_review_lenses":["security-privacy"],
                "nodes":(0..2).map(|index| serde_json::json!({
                    "id":if index == 1 {"security-privacy".to_owned()} else {format!("lens-{index}")},
                    "access":"read_only", "review_lens":if index == 1 {"security-privacy".to_owned()} else {format!("lens-{index}")},
                    "prompt":"review", "review_behaviour":"lens", "route":{"alias":"fixture"}
                })).collect::<Vec<_>>(),
                "exports":(0..2).map(|index| serde_json::json!({
                    "node":if index == 1 {"security-privacy".to_owned()} else {format!("lens-{index}")},
                    "outputs":["result","findings","verdict"]
                })).collect::<Vec<_>>()}
        }]
    })).unwrap();
    let mut owner = FakeOwner {
        observations: (0..2)
            .map(|index| {
                if index == 1 {
                    terminal(AttemptOutcome::DeclaredStop)
                } else {
                    terminal_with_result(AttemptOutcome::Succeeded, clean)
                }
            })
            .collect(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    let panel = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "lenses")
        .unwrap();
    assert_eq!(panel.state, NodeState::Failed);
    assert_eq!(
        panel.advance_error_code.as_deref(),
        Some("coverage_below_minimum")
    );
    assert!(
        panel
            .advance_error
            .as_deref()
            .unwrap()
            .contains("security-privacy")
    );
    assert_eq!(
        advance(&document, &mut owner, &mut journal)
            .unwrap()
            .checkpoint,
        run.checkpoint
    );
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
    assert_eq!(outputs.findings[0].severity, "blocking");
    assert_eq!(outputs.findings[1].severity, "blocking");
    assert_eq!(outputs.findings[0].status, "open");
    assert_eq!(outputs.findings[2].id, "F3");
    assert_eq!(outputs.findings[2].severity, "high");
    assert!(
        outputs.findings.iter().all(|finding| {
            finding.effective_severity() == "note" && finding.trigger().is_none()
        })
    );
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
    assert_eq!(composite.findings.len(), 3);
    for (finding, severity) in composite
        .findings
        .iter()
        .zip(["blocking", "blocking", "high"])
    {
        assert_eq!(finding.severity, severity);
        assert_eq!(finding.effective_severity(), "note");
        assert!(finding.trigger().is_none());
        // Typed composite findings above cover severity; the consumer must
        // receive the finding and its warnings regardless of JSON formatting.
        assert!(owner.prompts[3].contains(&finding.file));
    }
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
    let unavailable = node("lens-c");
    assert_eq!(unavailable.state, NodeState::Failed);
    assert_eq!(
        unavailable
            .attempts
            .last()
            .and_then(|attempt| attempt.outcome),
        Some(AttemptOutcome::AdapterUnavailable)
    );
    assert!(excerpt.contains(&format!(
        "{}: {}",
        unavailable.id,
        unavailable.advance_error.as_deref().unwrap()
    )));
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
fn partial_composite_replay_preserves_checkpoint_and_launches_nothing() {
    let (document, mut owner) = partial_review(1);
    let mut journal = MemoryJournal::default();
    let first = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(first.outcome, Some(GraphOutcome::Partial));
    let mut replay_owner = FakeOwner::default();
    let replay = advance(&document, &mut replay_owner, &mut journal).unwrap();
    assert_eq!(replay.checkpoint, first.checkpoint);
    assert!(replay_owner.calls.is_empty());
}

#[test]
fn status_and_replay_agree_on_persisted_partial_composite() {
    let (document, mut owner) = partial_review(1);
    let checkpoint = advance(&document, &mut owner, &mut MemoryJournal::default())
        .unwrap()
        .checkpoint;
    assert_eq!(checkpoint.outcome(), GraphOutcome::Partial);
    use provenant_core::Clock;
    let temp = crate::test_support::tempdir().unwrap();
    let base = std::fs::canonicalize(temp.path()).unwrap();
    let inputs = provenant_test_support::root_inputs(&base).unwrap();
    let root = StateRoot::resolve(&inputs, &provenant_os::fs::SystemPathResolver).unwrap();
    let project = provenant_core::ProjectId::from_bytes([1; 16]);
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
    let mut journal =
        FileJournal::open(&root, project, &document.graph_id, budget, &cancel, &clock).unwrap();
    journal
        .bind(GraphExecution {
            peer_origin_context: None,
            peer_submission_digest: None,
            requester_path: None,
            extra: Default::default(),
            bases: None,
            directory: inputs.checkout.clone(),
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
        GraphOutcome::Partial
    );
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
            assert_eq!(finding.lens.as_deref().unwrap(), "tests");
            assert_eq!(finding.evidence.as_deref().unwrap(), "observed failure");
            assert_eq!(finding.proposed_fix.as_deref().unwrap(), "repair branch");
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
        assert_eq!(finding.lens.as_deref().unwrap(), lens);
        assert_eq!(finding.evidence.as_deref().unwrap(), "observed");
        assert_eq!(finding.proposed_fix.as_deref().unwrap(), "repair");
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
