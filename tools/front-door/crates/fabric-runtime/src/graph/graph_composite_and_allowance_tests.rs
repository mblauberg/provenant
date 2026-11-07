use super::*;

#[test]
fn advisory_composite_failure_preserves_cause_and_launches_dependant() {
    for outcome in [AttemptOutcome::ReviewRequired, AttemptOutcome::TaskFailed] {
        let document: GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version":1, "graph_id":"advisory", "nodes":[
                {"id":"scout", "kind":"composite", "access":"read_only", "advisory":true,
                 "composite":{"nodes":[{"id":"synthesis","access":"read_only","prompt":"research","route":{"alias":"fixture"}}],
                              "exports":[{"node":"synthesis","outputs":["result"]}]}},
                {"id":"lane", "access":"read_only", "prompt":"write", "route":{"alias":"fixture"},
                 "depends_on":["scout"], "inputs":[{"node":"scout","outputs":["result"]}]}
            ]
        })).unwrap();
        let mut owner = FakeOwner {
            observations: VecDeque::from([terminal(outcome), terminal(AttemptOutcome::Succeeded)]),
            ..Default::default()
        };
        let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
        let scout = run
            .checkpoint
            .nodes
            .iter()
            .find(|n| n.id == "scout--synthesis")
            .unwrap();
        assert_eq!(scout.attempts[0].outcome, Some(outcome));
        assert_eq!(
            run.checkpoint
                .nodes
                .iter()
                .find(|n| n.id == "lane")
                .unwrap()
                .state,
            NodeState::Succeeded
        );
        assert!(owner.prompts.last().unwrap().contains("scout: unavailable"));
    }
}

#[test]
fn cumulative_allowance_counts_graph_attempts_and_survives_observation_replay() {
    let mut lane = node("lane", route("first"));
    lane.route = None;
    lane.routes = Some(vec![route("first"), route("second"), route("third")]);
    let document = graph(vec![lane]);
    let mut owner = FakeOwner {
        retry_limit: Some(1),
        observations: [AttemptObservation::Pending {
            attempt_ref: Some("first".into()),
        }]
        .into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(owner.retry_budgets, vec![Some(1)]);
    owner.retry_limit = Some(9); // Host changes cannot renew an admitted node.
    let mut refused = terminal(AttemptOutcome::UsageLimited);
    if let AttemptObservation::Terminal { attempt_ref, .. } = &mut refused {
        *attempt_ref = Some("first".into());
    }
    owner.observations.push_back(refused);
    owner
        .observations
        .push_back(terminal(AttemptOutcome::UsageLimited));
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.checkpoint.nodes[0].state, NodeState::Failed);
    assert_eq!(run.checkpoint.nodes[0].attempts.len(), 2);
    assert_eq!(run.checkpoint.nodes[0].extra["retry"]["retries"], 1);
    assert_eq!(owner.calls.len(), 3); // One observation replay and one allowed route switch.
    assert_eq!(owner.retry_budgets, vec![Some(1), Some(1), Some(0)]);
    assert_eq!(owner.calls[0].0, owner.calls[1].0);
    advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(owner.calls.len(), 3);
}

#[test]
fn preparation_fallback_does_not_consume_settlement_allowance() {
    use provenant_records::run::PreparationFailure;
    let mut lane = node("lane", route("first"));
    lane.route = None;
    lane.routes = Some(vec![route("first"), route("second"), route("third")]);
    let mut owner = FakeOwner {
        retry_limit: Some(0),
        fallback_evidence: [
            ("first".into(), PreparationFailure::UsageLimited),
            ("second".into(), PreparationFailure::UsageLimited),
        ]
        .into(),
        observations: [
            preparation_refusal("first", AttemptOutcome::UsageLimited),
            preparation_refusal("second", AttemptOutcome::UsageLimited),
            terminal(AttemptOutcome::Succeeded),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![lane]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert!(run.checkpoint.successful());
    assert_eq!(owner.retry_budgets, vec![Some(0), Some(0), Some(0)]);
}

#[test]
fn writer_quota_continuation_succeeds_and_lens_diff_base_stays_node_source() {
    let node_source = "0".repeat(40);
    let head_2 = "2".repeat(40);

    let mut write = node("writer", route("first"));
    write.access = AccessMode::WorktreeWrite;
    write.worktree = Some(".worktrees/write".into());
    write.source = Some(SourceBinding::Commit {
        commit: node_source.clone(),
    });

    let mut lens = node("lens", route("first"));
    lens.access = AccessMode::ReadOnly;
    lens.depends_on = vec!["writer".into()];
    lens.source = Some(SourceBinding::Commit {
        commit: node_source.clone(),
    });

    // Writer predecessor committed and hit quota; its continuation succeeded with head_2.
    let writer_done = AttemptObservation::Terminal {
        attempt_ref: Some("host/writer-cont".into()),
        outcome: AttemptOutcome::Succeeded,
        launched: true,
        result: None,
        result_warning: None,
        result_path: None,
        branch: Some("writer".into()),
        commit: Some(head_2.clone()),
    };
    let lens_done = terminal(AttemptOutcome::Succeeded);

    let mut owner = FakeOwner {
        observations: [writer_done, lens_done].into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&graph(vec![write, lens]), &mut owner, &mut journal).unwrap();

    assert!(run.checkpoint.successful());
    assert_eq!(run.checkpoint.nodes[0].state, NodeState::Succeeded);
    assert_eq!(
        run.checkpoint.nodes[0]
            .outputs
            .as_ref()
            .and_then(|o| o.commit.as_ref()),
        Some(&head_2)
    );
    // Writer launched pinned to node_source.
    assert_eq!(owner.source_commits[0], Some(node_source.clone()));
    // Lens diff base stays the original node_source, not advancing to writer's intermediate/final head.
    assert_eq!(owner.source_commits[1], Some(node_source));
}

#[test]
fn composite_truncates_over_cap_findings_and_reaches_downstream() {
    for long in [false, true] {
        let document: GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version":1,"graph_id":"over-cap","nodes":[
                {"id":"reviews","kind":"composite","access":"read_only","composite":{
                    "nodes":[{"id":"a","access":"read_only","prompt":"review","route":{"alias":"senior"}},
                             {"id":"b","access":"read_only","prompt":"review","route":{"alias":"senior"}}],
                    "exports":[{"node":"a","outputs":["result","findings"]},{"node":"b","outputs":["result","findings"]}]
                }},
                {"id":"verify","access":"read_only","prompt":"verify","route":{"alias":"senior"},
                 "depends_on":["reviews"],"inputs":[{"node":"reviews","outputs":["result","findings"]}]}
            ]
        })).unwrap();
        let count = if long { 8 } else { 20 };
        let entries: Vec<_> = (0..count).map(|i| serde_json::json!({"id":format!("F{i}"),"severity":"high","file":"a", "summary":if long {"s".repeat(800)} else {"s".into()},"status":"open"})).collect();
        let text = format!(
            "```findings\n{}\n```",
            serde_json::json!({"schema_version":1,"findings":entries})
        );
        let observation = |path: &str| {
            let mut result = terminal_with_result(AttemptOutcome::Succeeded, &text);
            if let AttemptObservation::Terminal {
                result_path,
                attempt_ref,
                ..
            } = &mut result
            {
                *result_path = Some(path.into());
                *attempt_ref = Some(format!("fixture/{}", path.rsplit('/').next().unwrap()));
            }
            result
        };
        let mut owner = FakeOwner {
            observations: [
                observation("/results/a"),
                observation("/results/b"),
                terminal(AttemptOutcome::Succeeded),
            ]
            .into(),
            ..Default::default()
        };
        let mut journal = MemoryJournal::default();
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        let composite = run
            .checkpoint
            .nodes
            .iter()
            .find(|n| n.id == "reviews")
            .unwrap();
        assert_eq!(composite.state, NodeState::Succeeded);
        assert!(matches!(
            composite.truncation_cause,
            Some(NodeDecisionCause::CompositeFindingsTruncated { .. })
        ));
        let decoded: GraphCheckpoint =
            serde_json::from_slice(&serde_json::to_vec(&run.checkpoint).unwrap()).unwrap();
        assert_eq!(decoded, run.checkpoint);
        let mut forged = run.checkpoint.clone();
        forged
            .nodes
            .iter_mut()
            .find(|node| node.id == "reviews")
            .unwrap()
            .truncation_cause = None;
        let mut forged_journal = MemoryJournal(Some(forged));
        assert!(matches!(
            advance(&document, &mut FakeOwner::default(), &mut forged_journal),
            Err(GraphError::BadCheckpoint { .. })
        ));
        let outputs = composite.outputs.as_ref().unwrap();
        assert_eq!(outputs.findings_available, Some(false));
        assert!(outputs.findings.len() < 2 * count);
        assert!(outputs.findings.len() <= MAX_FINDINGS);
        assert!(canonical_len(&outputs.findings) <= FINDINGS_BYTES);
        assert!(
            outputs.warnings[0]
                .contains(&format!("{} dropped", 2 * count - outputs.findings.len()))
        );
        assert!(
            outputs.warnings[0].contains("reviews--a")
                && outputs.warnings[0].contains("reviews--b")
                && !outputs.warnings[0].contains("/results/")
                && outputs.warnings[0].contains("fixture/a")
                && outputs.warnings[0].contains("fixture/b")
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
        assert!(owner.prompts[2].contains("composite findings truncated"));
        if !long {
            let (plan, positions) = validate_plan(&document).unwrap();
            let mut spoofed = run.checkpoint.clone();
            let source = &mut spoofed.nodes[positions["reviews"]];
            source.truncation_cause = None;
            source.outputs.as_mut().unwrap().warnings =
                vec!["composite findings truncated: TOKEN_SPOOF".into()];
            let (prompt, _) = render_input(
                &plan.nodes[positions["verify"]],
                &plan,
                &spoofed,
                &positions,
                usize::MAX,
            );
            assert!(!prompt.contains("TOKEN_SPOOF"));
            let mut checkpoint = run.checkpoint.clone();
            checkpoint.nodes[positions["reviews--a"]]
                .outputs
                .as_mut()
                .unwrap()
                .result = None;
            let requests = upstream_results(
                &plan.nodes[positions["verify"]],
                &plan,
                &checkpoint,
                &positions,
            );
            let gate = requests
                .iter()
                .find(|request| request.label == "reviews--a")
                .unwrap();
            assert_eq!(gate.inline_findings.as_ref().unwrap().len(), count);
            assert_eq!(gate.finding_prefix.as_deref(), Some("reviews--a:"));
        }
        assert_eq!(
            advance(&document, &mut FakeOwner::default(), &mut journal)
                .unwrap()
                .checkpoint,
            run.checkpoint
        );
    }
}

#[test]
fn nested_composite_propagates_its_members_truncation_notice() {
    let doc: GraphDocument = serde_json::from_value(serde_json::json!({"schema_version":1,"graph_id":"nested-cap","nodes":[
        {"id":"outer","kind":"composite","access":"read_only","composite":{"nodes":[
            {"id":"inner","kind":"composite","access":"read_only","composite":{"nodes":[
                {"id":"a","access":"read_only","prompt":"review","route":{"alias":"senior"}},
                {"id":"b","access":"read_only","prompt":"review","route":{"alias":"senior"}}
            ],"exports":[{"node":"a","outputs":["result","findings"]},{"node":"b","outputs":["result","findings"]}]}}
        ],"exports":[{"node":"inner","outputs":["findings"]}]}},
        {"id":"next","access":"read_only","prompt":"use findings","route":{"alias":"senior"},"depends_on":["outer"],"inputs":[{"node":"outer","outputs":["findings"]}]}
    ]})).unwrap();
    let entries: Vec<_> = (0..20).map(|i| serde_json::json!({"id":format!("F{i}"),"severity":"high","file":"a","summary":"s","status":"open"})).collect();
    let text = format!(
        "```findings\n{}\n```",
        serde_json::json!({"schema_version":1,"findings":entries})
    );
    let mut owner = FakeOwner {
        observations: [
            terminal_with_result(AttemptOutcome::Succeeded, &text),
            terminal_with_result(AttemptOutcome::Succeeded, &text),
            terminal(AttemptOutcome::Succeeded),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    let outer = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "outer")
        .unwrap();
    assert_eq!(outer.state, NodeState::Succeeded);
    assert!(
        outer
            .outputs
            .as_ref()
            .unwrap()
            .warnings
            .iter()
            .any(|warning| warning.contains("8 dropped"))
    );
    assert!(owner.prompts[2].contains("8 dropped"));
    let (plan, positions) = validate_plan(&doc).unwrap();
    let requests = upstream_results(
        &plan.nodes[positions["next"]],
        &plan,
        &run.checkpoint,
        &positions,
    );
    let member = requests
        .iter()
        .find(|request| request.label == "outer--inner--a")
        .unwrap();
    assert_eq!(
        member.finding_prefix.as_deref(),
        Some("outer--inner:outer--inner--a:")
    );
    let ids = truncated_finding_ids(
        &plan.nodes[positions["next"]],
        &plan,
        &run.checkpoint,
        &positions,
    );
    assert_eq!(ids.len(), 40);
    assert!(ids.contains(&(
        "outer--inner:outer--inner--a:F0".into(),
        "outer--inner--a:0".into()
    )));
    assert!(ids.contains(&(
        "outer--inner:outer--inner--b:F19".into(),
        "outer--inner--b:19".into()
    )));
}

// Small: a qualified ID that cannot fit must fail the composite, including on replay.
#[test]
fn composite_rejects_overlong_qualified_finding_id() {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version":1,"graph_id":"long-id","nodes":[
            {"id":"reviews","kind":"composite","access":"read_only","composite":{
                "nodes":[{"id":"a","access":"read_only","prompt":"review","route":{"alias":"senior"}}],
                "exports":[{"node":"a","outputs":["findings"]}]
            }},
            {"id":"synthesis","kind":"synthesise","access":"read_only","prompt":"dispose","route":{"alias":"senior"},
             "depends_on":["reviews"],"inputs":[{"node":"reviews","outputs":["findings"]}],"dispositions":["rejected"]}
        ]
    })).unwrap();
    let text = findings_text(&entry("").replace("F1", &"x".repeat(1024)));
    let mut owner = FakeOwner {
        observations: [terminal_with_result(AttemptOutcome::Succeeded, &text)].into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    let composite = run
        .checkpoint
        .nodes
        .iter()
        .find(|n| n.id == "reviews")
        .unwrap();
    assert_eq!(composite.state, NodeState::Failed);
    assert_eq!(
        composite.advance_error_code.as_deref(),
        Some("composite_findings_exceed_cap")
    );
    assert_ne!(
        run.checkpoint
            .nodes
            .iter()
            .find(|n| n.id == "synthesis")
            .unwrap()
            .state,
        NodeState::Succeeded
    );
    assert_eq!(
        advance(&document, &mut FakeOwner::default(), &mut journal)
            .unwrap()
            .checkpoint,
        run.checkpoint
    );
}

// Small: inherited descriptions can exceed the ordinary findings budget.
#[test]
fn identity_dispositions_preserve_large_inherited_fields() {
    let mut synthesis = node("synthesis", route("first"));
    synthesis.dispositions = DISPOSITIONS.map(String::from).to_vec();
    synthesis.depends_on = vec!["review".into()];
    synthesis.inputs = vec![GraphInput {
        required_outputs: Vec::new(),
        extra: Default::default(),
        node: "review".into(),
        outputs: vec![OutputKind::Findings],
    }];
    let long = "x".repeat(900);
    let entries: Vec<_> = (0..3).map(|i| serde_json::json!({"id":format!("F{i}"),"severity":"high","file":long,"summary":long,"status":long})).collect();
    let review = format!(
        "```findings\n{}\n```\nVERDICT: PASS",
        serde_json::json!({"schema_version":1,"findings":entries})
    );
    let decisions = (0..3)
        .map(|i| {
            format!(
                r#"{{"id":"F{i}","disposition":"rejected","reason":"{}"}}"#,
                "r".repeat(700)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let mut owner = FakeOwner {
        observations: [
            terminal_with_result(AttemptOutcome::Succeeded, &review),
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&decisions)),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![node("review", route("first")), synthesis]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    let result = &run.checkpoint.nodes[1];
    assert_eq!(
        result.state,
        NodeState::Succeeded,
        "{:?}",
        result.advance_error
    );
    let findings = &result.outputs.as_ref().unwrap().findings;
    assert!(canonical_len(findings) > FINDINGS_BYTES);
    assert_eq!(findings[0].summary, long);
}

// Small: directly selected sources with the same ID remain distinct.
#[test]
fn direct_reviewers_with_duplicate_ids_require_distinct_dispositions() {
    let mut synthesis = node("synthesis", route("first"));
    synthesis.dispositions = DISPOSITIONS.map(String::from).to_vec();
    synthesis.depends_on = vec!["a".into(), "b".into()];
    synthesis.inputs = ["a", "b"]
        .into_iter()
        .map(|id| GraphInput {
            required_outputs: Vec::new(),
            extra: Default::default(),
            node: id.into(),
            outputs: vec![OutputKind::Findings],
        })
        .collect();
    for (decisions, expected) in [
        (
            r#"{"id":"F1","disposition":"rejected","reason":"no"}"#,
            NodeState::Failed,
        ),
        (
            r#"{"id":"a:F1","disposition":"rejected","reason":"no"},{"id":"b:F1","disposition":"rejected","reason":"no"}"#,
            NodeState::Succeeded,
        ),
    ] {
        let mut owner = FakeOwner {
            observations: [
                terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&entry(""))),
                terminal_with_result(
                    AttemptOutcome::Succeeded,
                    &findings_text(
                        &entry("").replace("\"summary\":\"s\"", "\"summary\":\"other\""),
                    ),
                ),
                terminal_with_result(AttemptOutcome::Succeeded, &findings_text(decisions)),
            ]
            .into(),
            ..Default::default()
        };
        let run = advance(
            &graph(vec![
                node("a", route("first")),
                node("b", route("first")),
                synthesis.clone(),
            ]),
            &mut owner,
            &mut MemoryJournal::default(),
        )
        .unwrap();
        let result = &run.checkpoint.nodes[2];
        assert!(owner.prompts[2].contains("a:F1"));
        assert!(owner.prompts[2].contains("b:F1"));
        assert_eq!(result.state, expected, "{:?}", result.advance_error);
        if expected == NodeState::Succeeded {
            let findings = &result.outputs.as_ref().unwrap().findings;
            assert_eq!(
                (&findings[0].summary, &findings[1].summary),
                (&"s".to_owned(), &"other".to_owned())
            );
        } else {
            assert!(
                result
                    .advance_error
                    .as_deref()
                    .unwrap()
                    .contains("ambiguous")
            );
        }
    }
}

#[test]
fn equal_review_range_cannot_pass_a_reduced_panel_gate() {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "graph_id": "empty-quick-review", "nodes": [
            {"id":"lenses", "kind":"composite", "access":"read_only",
             "composite":{"reduce_verdict":true,"min_successful":1,
                "nodes":[{"id":"lens","role":"lens.correctness-spec","access":"read_only",
                          "prompt":"review","route":{"alias":"fixture"}}],
                "exports":[{"node":"lens","outputs":["result"]}]}},
            {"id":"accept","kind":"gate","access":"read_only","depends_on":["lenses"],
             "inputs":[{"node":"lenses","outputs":["verdict"]}],
             "condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"}}
        ]
    }))
    .unwrap();
    let mut owner = FakeOwner {
        equal_range: true,
        observations: [terminal_from("VERDICT: PASS", "host/one")].into(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    let panel = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "lenses")
        .unwrap();
    assert_eq!(
        panel
            .outputs
            .as_ref()
            .and_then(|output| output.verdict.as_deref()),
        Some("VERDICT: INCONCLUSIVE")
    );
    let gate = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "accept")
        .unwrap();
    assert!(!gate.gate.as_ref().unwrap().passed);
}

#[test]
fn equal_review_range_warns_on_review_nodes() {
    let mut review = node("review", route("review"));
    review.role = Some("lens.correctness-spec".into());
    let mut synthesis = node("synthesis", route("review"));
    synthesis.role = Some("review.synthesis".into());
    synthesis.depends_on = vec!["review".into()];
    let mut other = node("other", route("other"));
    other.role = Some("scout".into());
    other.depends_on = vec!["synthesis".into()];
    let mut gate = gate_node(GateCondition::Verdict {
        extra: Default::default(),
        expected: "VERDICT: PASS".into(),
        threshold: VerdictThreshold::All,
    });
    gate.depends_on = vec!["synthesis".into()];
    gate.inputs = vec![GraphInput {
        extra: Default::default(),
        node: "synthesis".into(),
        outputs: vec![OutputKind::Verdict],
        required_outputs: Vec::new(),
    }];
    let pass = "```findings\n{\"schema_version\":1,\"findings\":[]}\n```\nVERDICT: PASS";
    let mut owner = FakeOwner {
        equal_range: true,
        observations: [
            terminal_from(pass, "host/one"),
            terminal_from(pass, "host/two"),
            terminal_from(pass, "host/three"),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![review, synthesis, other, gate]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    for progress in &run.checkpoint.nodes[..2] {
        let outputs = progress.outputs.as_ref().unwrap();
        assert_eq!(outputs.verdict.as_deref(), Some("VERDICT: INCONCLUSIVE"));
        assert!(
            outputs
                .warnings
                .iter()
                .any(|warning| warning.contains(&format!(
                    "base {}, head {}",
                    "1".repeat(40),
                    "1".repeat(40)
                )))
        );
    }
    assert!(
        run.checkpoint.nodes[2]
            .outputs
            .as_ref()
            .unwrap()
            .warnings
            .is_empty()
    );
    assert!(!run.checkpoint.nodes[3].gate.as_ref().unwrap().passed);

    let mut review = node("review", route("review"));
    review.role = Some("lens.correctness-spec".into());
    let mut owner = FakeOwner {
        no_review_base: true,
        observations: [terminal_from(pass, "host/one")].into(),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![review]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    let outputs = run.checkpoint.nodes[0].outputs.as_ref().unwrap();
    assert_eq!(outputs.verdict.as_deref(), Some("VERDICT: PASS"));
    assert!(
        outputs
            .warnings
            .iter()
            .any(|warning| warning.contains("no base was resolved"))
    );
    assert!(
        !outputs
            .warnings
            .iter()
            .any(|warning| warning.contains("equal_base_and_head"))
    );

    let mut review = node("review", route("review"));
    review.role = Some("lens.correctness-spec".into());
    let malformed =
        findings_text(r#"{"id":"F1","severity":1,"file":1,"summary":1,"status":1,"line":-1}"#);
    let mut owner = FakeOwner {
        equal_range: true,
        observations: [terminal_from(&malformed, "host/one")].into(),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![review]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    let outputs = run.checkpoint.nodes[0].outputs.as_ref().unwrap();
    assert!(
        outputs
            .warnings
            .iter()
            .any(|warning| warning.contains("equal_base_and_head"))
    );
}
