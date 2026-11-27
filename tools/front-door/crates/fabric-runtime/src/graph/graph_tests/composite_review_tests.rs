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
                    Some(text) => terminal_with_result(GraphAttemptOutcome::Succeeded, text),
                    None => terminal(GraphAttemptOutcome::AdapterUnavailable),
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
                composite.advance_code.as_deref(),
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
        observations: [terminal(GraphAttemptOutcome::AdapterUnavailable)].into(),
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
        composite.advance_code.as_deref(),
        Some("coverage_below_minimum")
    );
    assert!(composite.outputs.is_none());
}

// Spec: docs/specs/fabric.md#fix-round-invariants-and-split-verdict-adjudication
#[test]
fn distinct_member_verdicts_run_one_adjudicator_and_supply_the_gate_verdict() {
    let answer = |verdict: &str| {
        format!("```findings\n{{\"schema_version\":1,\"findings\":[]}}\n```\n{verdict}\n")
    };
    let lens = |id: &str| {
        serde_json::json!({
            "id":id, "access":"read_only", "review_lens":id,
            "review_behaviour":"lens", "require_findings":true,
            "prompt":"review", "route":{"alias":"fixture"}
        })
    };
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "graph_id": "split-adjudication", "nodes": [
            {"id":"panel", "kind":"composite", "access":"read_only",
             "composite":{"min_successful":2,"reduce_verdict":true,
                "nodes":[lens("lens-changes"),lens("lens-approve")],
                "exports":[
                    {"node":"lens-changes","outputs":["result","findings","verdict"]},
                    {"node":"lens-approve","outputs":["result","findings","verdict"]}
                ]}},
            {"id":"gate", "kind":"gate", "access":"read_only", "depends_on":["panel"],
             "inputs":[{"node":"panel","outputs":["verdict"]}],
             "condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"}}
        ]
    }))
    .unwrap();
    for (first, case) in [
        ("VERDICT: FAIL", "FAIL beside approval"),
        (
            "VERDICT: INCONCLUSIVE",
            "INCONCLUSIVE beside approval also requires adjudication",
        ),
    ] {
        let mut owner = FakeOwner {
            observations: [
                terminal_with_result(GraphAttemptOutcome::Succeeded, &answer(first)),
                terminal_with_result(GraphAttemptOutcome::Succeeded, &answer("VERDICT: PASS")),
                terminal_with_result(GraphAttemptOutcome::Succeeded, &answer("VERDICT: PASS")),
            ]
            .into(),
            ..Default::default()
        };
        let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();

        let adjudicators: Vec<_> = run
            .checkpoint
            .nodes
            .iter()
            .filter(|node| node.id.ends_with("adjudicate"))
            .collect();
        assert_eq!(
            adjudicators.len(),
            1,
            "{case}: a distinct member verdict set is resolved by exactly one adjudicator"
        );
        assert_eq!(adjudicators[0].state, NodeState::Succeeded);
        assert_eq!(
            adjudicators[0]
                .outputs
                .as_ref()
                .and_then(|outputs| outputs.verdict.as_deref()),
            Some("VERDICT: PASS")
        );
        assert_eq!(
            owner.calls.len(),
            3,
            "only the two lenses and one adjudicator run"
        );
        assert!(
            owner
                .prompts
                .last()
                .is_some_and(|prompt| prompt.contains("Resolve the split once"))
        );
        let gate = run
            .checkpoint
            .nodes
            .iter()
            .find(|node| node.id == "gate")
            .unwrap();
        assert!(
            gate.gate.as_ref().is_some_and(|gate| gate.passed),
            "the gate uses the adjudicator verdict"
        );
    }
}

#[test]
fn adjudicator_is_not_started_when_panel_artifacts_are_capped() {
    let answer = |verdict: &str| {
        format!("```findings\n{{\"schema_version\":1,\"findings\":[]}}\n```\n{verdict}\n")
    };
    let lens = |id: &str| {
        serde_json::json!({
            "id":id, "access":"read_only", "review_lens":id,
            "review_behaviour":"lens", "require_findings":true,
            "prompt":"review", "route":{"alias":"fixture"}
        })
    };
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "graph_id": "capped-split-adjudication", "nodes": [
            {"id":"panel", "kind":"composite", "access":"read_only",
             "composite":{"min_successful":2,"reduce_verdict":true,
                "nodes":[lens("lens-changes"),lens("lens-approve")],
                "exports":[
                    {"node":"lens-changes","outputs":["result","findings","verdict"]},
                    {"node":"lens-approve","outputs":["result","findings","verdict"]}
                ]}},
            {"id":"gate", "kind":"gate", "access":"read_only", "depends_on":["panel"],
             "inputs":[{"node":"panel","outputs":["verdict"]}],
             "condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"}}
        ]
    }))
    .unwrap();
    let mut owner = FakeOwner {
        observations: [
            terminal_from(
                &answer("VERDICT: FAIL"),
                "host-a/lens-changes",
            ),
            terminal_from(
                &answer("VERDICT: PASS"),
                "host-a/lens-approve",
            ),
            terminal_from(
                &answer("VERDICT: PASS"),
                "host-a/adjudicate",
            ),
        ]
        .into(),
        materialise: Some(cap_materialised_inputs),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    let adjudicator = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "panel--adjudicate")
        .unwrap();
    assert_eq!(adjudicator.state, NodeState::Failed);
    assert_eq!(
        adjudicator.advance_code.as_deref(),
        Some("review_adjudication_evidence_unavailable")
    );
    assert_eq!(owner.calls.len(), 2);
    assert!(owner
        .calls
        .iter()
        .all(|(key, _)| !key.node_id.ends_with("adjudicate")));
    assert!(owner.file_requests.iter().flatten().any(|request| {
        request.attempt_ref.is_some() && request.label.starts_with("panel--")
    }));
}

#[test]
fn unaccepted_replayed_adjudicator_refuses_capped_panel_artifacts() {
    let source = |id: &str| {
        let mut source = node(id, route("fixture"));
        source.require_findings = true;
        source
    };
    let mut adjudicator = node("adjudicate", route("fixture"));
    adjudicator.kind = Some(NodeKind::Adjudicate);
    adjudicator.require_findings = true;
    adjudicator.inputs = ["panel-a", "panel-b"]
        .into_iter()
        .map(|input| GraphInput {
            extra: Default::default(),
            required_outputs: vec![OutputKind::Findings, OutputKind::Verdict],
            node: input.into(),
            outputs: vec![OutputKind::Findings, OutputKind::Verdict],
        })
        .collect();
    let mut document = graph(vec![source("panel-a"), source("panel-b"), adjudicator]);
    document.schema_version = 2;
    let limits = crate::config::GraphLimits::default();
    let admitted = candidate::admit_candidate_with_review_policy(
        &document,
        None,
        &[],
        &limits,
        &Default::default(),
        None,
        &ReviewPolicy::default(),
    )
    .unwrap();
    let (plan, positions) = validate_flat_plan(admitted.expanded(), &limits).unwrap();
    let digest = plan_digest(&plan).unwrap().to_string();
    let mut checkpoint = transitions::initial_checkpoint(
        &document,
        &plan,
        digest,
        None,
        None,
        None,
    );
    checkpoint.review_policy = Some(ReviewPolicy::default());
    for (index, (id, verdict)) in [
        ("panel-a", "VERDICT: FAIL"),
        ("panel-b", "VERDICT: PASS"),
    ]
    .into_iter()
    .enumerate()
    {
        let node_index = positions[id];
        let key = AttemptKey {
            extra: Default::default(),
            retry_generation: None,
            previous_attempt_ref: None,
            graph_id: document.graph_id.clone(),
            node_id: id.into(),
            route_index: 0,
            attempt: 1,
        };
        transitions::begin_attempt(
            node_index,
            &plan,
            &mut checkpoint,
            &positions,
            &key,
            &plan.nodes[node_index].routes[0],
            transitions::LaunchFacts {
                input_cap: limits.node_prompt_bytes,
                placement_host_id: None,
                launch_actor: "fixture".into(),
                input_cap_source: provenant_records::Layer::Detected,
                timing: None,
            },
        );
        let attempt = checkpoint.nodes[node_index].attempts.last_mut().unwrap();
        attempt.state = AttemptState::Terminal;
        attempt.attempt_ref = Some(format!("host-a/panel-{index}"));
        attempt.outcome = Some(GraphAttemptOutcome::Succeeded);
        checkpoint.nodes[node_index].state = NodeState::Succeeded;
        checkpoint.nodes[node_index].outputs = Some(NodeOutputs {
            result: Some(format!("complete panel result {verdict}")),
            findings: Vec::new(),
            findings_available: Some(true),
            verdict: Some(verdict.into()),
            ..Default::default()
        });
    }
    let adjudicator_index = positions["adjudicate"];
    let key = AttemptKey {
        extra: Default::default(),
        retry_generation: None,
        previous_attempt_ref: None,
        graph_id: document.graph_id.clone(),
        node_id: "adjudicate".into(),
        route_index: 0,
        attempt: 1,
    };
    transitions::begin_attempt(
        adjudicator_index,
        &plan,
        &mut checkpoint,
        &positions,
        &key,
        &plan.nodes[adjudicator_index].routes[0],
        transitions::LaunchFacts {
            input_cap: limits.node_prompt_bytes,
            placement_host_id: None,
            launch_actor: "fixture".into(),
            input_cap_source: provenant_records::Layer::Detected,
            timing: None,
        },
    );
    let checkpoint: GraphCheckpoint =
        serde_json::from_value(serde_json::to_value(checkpoint).unwrap()).unwrap();
    let mut journal = MemoryJournal(Some(checkpoint));
    let mut owner = FakeOwner {
        materialise: Some(cap_materialised_inputs),
        ..Default::default()
    };

    let replay = advance(&document, &mut owner, &mut journal);
    assert!(matches!(replay, Err(GraphError::Refused(reason)) if reason.contains("split-panel evidence")));
    assert!(owner.calls.is_empty(), "the unaccepted adjudicator must not launch");
    assert!(owner.file_requests[0].iter().all(|request| request.attempt_ref.is_some()));
}

// Spec: docs/specs/fabric.md#fix-round-invariants-and-split-verdict-adjudication
#[test]
fn each_split_panel_uses_its_own_adjudicator_on_its_pinned_head() {
    let answer = |verdict: &str| format!(
        "```findings\n{{\"schema_version\":1,\"findings\":[]}}\n```\nVERDICT: {verdict}\n"
    );
    let panel = |id: &str, dependency: Option<&str>, head: &str| {
        let lens = |name: &str| serde_json::json!({
            "id": name, "access": "read_only", "review_lens": name,
            "review_behaviour": "lens", "require_findings": true,
            "prompt": "review", "route": {"alias": "fixture"},
            "source": {"type": "commit", "commit": head}
        });
        serde_json::json!({
            "id": id, "kind": "composite", "access": "read_only",
            "depends_on": dependency.into_iter().collect::<Vec<_>>(),
            "composite": {"min_successful": 2, "reduce_verdict": true,
                "nodes": [lens("changes"), lens("approve")],
                "exports": [
                    {"node": "changes", "outputs": ["result", "findings", "verdict"]},
                    {"node": "approve", "outputs": ["result", "findings", "verdict"]}
                ]}
        })
    };
    let gate = |id: &str, source: &str, expected: &str| serde_json::json!({
        "id": id, "kind": "gate", "access": "read_only", "depends_on": [source],
        "inputs": [{"node": source, "outputs": ["verdict"]}],
        "condition": {"type": "verdict", "expected": expected, "threshold": "all"}
    });
    let first_head = "a".repeat(40);
    let second_head = "b".repeat(40);
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "graph_id": "two-splits", "nodes": [
            panel("first", None, &first_head), gate("first-gate", "first", "VERDICT: PASS"),
            panel("second", Some("first-gate"), &second_head),
            gate("second-gate", "second", "VERDICT: FAIL")
        ]
    })).unwrap();
    let mut owner = FakeOwner {
        observations: ["FAIL", "PASS", "PASS", "FAIL", "PASS", "FAIL"]
            .into_iter().map(|verdict| terminal_with_result(GraphAttemptOutcome::Succeeded, &answer(verdict))).collect(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    for (panel, gate_id, head, verdict) in [
        ("first", "first-gate", &first_head, "VERDICT: PASS"),
        ("second", "second-gate", &second_head, "VERDICT: FAIL"),
    ] {
        let adjudicator = run.checkpoint.nodes.iter().find(|node| node.id == format!("{panel}--adjudicate")).unwrap();
        assert_eq!(adjudicator.state, NodeState::Succeeded);
        assert_eq!(adjudicator.source_commit.as_deref(), Some(head.as_str()));
        assert_eq!(adjudicator.outputs.as_ref().and_then(|outputs| outputs.verdict.as_deref()), Some(verdict));
        let gate = run.checkpoint.nodes.iter().find(|node| node.id == gate_id).unwrap();
        assert!(gate.gate.as_ref().is_some_and(|gate| gate.passed));
    }
    assert_eq!(owner.calls.len(), 6);
}

// Spec: docs/specs/agent-native.md#3-7-review-convergence
#[test]
fn separate_review_chains_on_the_same_head_get_separate_adjudicators() {
    let answer = |verdict: &str| format!(
        "```findings\n{{\"schema_version\":1,\"findings\":[]}}\n```\nVERDICT: {verdict}\n"
    );
    let head = "c".repeat(40);
    let panel = |id: &str, dependency: Option<&str>| {
        let lens = |name: &str| serde_json::json!({
            "id": name, "access": "read_only", "review_lens": name,
            "review_behaviour": "lens", "require_findings": true,
            "prompt": "review", "route": {"alias": "fixture",
                "review_policy": {"adjudicator_route": "principal"}},
            "source": {"type": "commit", "commit": head}
        });
        serde_json::json!({
            "id": id, "kind": "composite", "access": "read_only",
            "depends_on": dependency.into_iter().collect::<Vec<_>>(),
            "composite": {"min_successful": 2, "reduce_verdict": true,
                "nodes": [lens("changes"), lens("approve")],
                "exports": [
                    {"node": "changes", "outputs": ["result", "findings", "verdict"]},
                    {"node": "approve", "outputs": ["result", "findings", "verdict"]}
                ]}
        })
    };
    let gate = |id: &str, source: &str| serde_json::json!({
        "id": id, "kind": "gate", "access": "read_only", "depends_on": [source],
        "inputs": [{"node": source, "outputs": ["verdict"]}],
        "condition": {"type": "verdict", "expected": "VERDICT: PASS", "threshold": "all"}
    });
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "graph_id": "two-review-chains-same-head",
        "review_policy": {"adjudicator_route": "fixture"}, "nodes": [
            panel("first", None), gate("first-gate", "first"),
            panel("second", Some("first-gate")), gate("second-gate", "second")
        ]
    })).unwrap();
    let mut owner = FakeOwner {
        observations: ["FAIL", "PASS", "PASS", "FAIL", "PASS", "FAIL"]
            .into_iter()
            .map(|verdict| terminal_with_result(GraphAttemptOutcome::Succeeded, &answer(verdict)))
            .collect(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    let adjudicators: Vec<_> = run
        .checkpoint
        .nodes
        .iter()
        .filter(|node| node.id.ends_with("adjudicate"))
        .collect();
    assert_eq!(adjudicators.len(), 2, "the key is (review chain, pinned head)");
    for (panel, verdict) in [("first", "VERDICT: PASS"), ("second", "VERDICT: FAIL")] {
        let adjudicator = run
            .checkpoint
            .nodes
            .iter()
            .find(|node| node.id == format!("{panel}--adjudicate"))
            .unwrap();
        assert_eq!(adjudicator.source_commit.as_deref(), Some(head.as_str()));
        assert_eq!(
            adjudicator
                .attempts
                .last()
                .and_then(|attempt| attempt.route.alias.as_deref()),
            Some("principal"),
            "route-scoped adjudicator setting reaches the launched node"
        );
        assert_eq!(
            adjudicator
                .outputs
                .as_ref()
                .and_then(|outputs| outputs.verdict.as_deref()),
            Some(verdict)
        );
    }
    assert_eq!(owner.calls.len(), 6, "four panel lenses and two adjudicators run");
    for gate_id in ["first-gate", "second-gate"] {
        let gate = run.checkpoint.nodes.iter().find(|node| node.id == gate_id).unwrap();
        assert!(gate.gate.as_ref().is_some_and(|gate| gate.passed), "{gate_id}");
    }
}

// Spec: docs/specs/agent-native.md#3-7-review-convergence
#[test]
fn same_chain_panels_on_one_head_share_complete_adjudication_inputs() {
    let answer = |verdict: &str| format!(
        "```findings\n{{\"schema_version\":1,\"findings\":[]}}\n```\nVERDICT: {verdict}\n"
    );
    let head = "d".repeat(40);
    let panel = |id: &str| {
        let lens = |name: &str| serde_json::json!({
            "id": name, "access": "read_only", "review_lens": name,
            "review_behaviour": "lens", "require_findings": true,
            "prompt": "review", "route": {"alias": "fixture"},
            "source": {"type": "commit", "commit": head}
        });
        serde_json::json!({
            "id": id, "kind": "composite", "access": "read_only",
            "composite": {"min_successful": 2, "reduce_verdict": true,
                "nodes": [lens("changes"), lens("approve")],
                "exports": [
                    {"node": "changes", "outputs": ["result", "findings", "verdict"]},
                    {"node": "approve", "outputs": ["result", "findings", "verdict"]}
                ]}
        })
    };
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "graph_id": "same-chain-same-head", "nodes": [{
            "id": "review-chain", "kind": "loop", "access": "read_only",
            "loop": {"until": "done", "max_rounds": 1, "body": [
                panel("first"), panel("second"),
                {"id":"done", "kind":"gate", "access":"read_only",
                 "depends_on":["first","second"],
                 "inputs":[
                    {"node":"first","outputs":["verdict"]},
                    {"node":"second","outputs":["verdict"]}
                 ],
                 "condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"}}
            ]}
        }]
    })).unwrap();
    let mut owner = FakeOwner {
        observations: ["FAIL", "PASS", "FAIL", "PASS", "PASS"]
            .into_iter()
            .map(|verdict| terminal_with_result(GraphAttemptOutcome::Succeeded, &answer(verdict)))
            .collect(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    let adjudicators: Vec<_> = run
        .checkpoint
        .nodes
        .iter()
        .filter(|node| node.id.ends_with("adjudicate"))
        .collect();
    assert_eq!(adjudicators.len(), 1);
    assert_eq!(adjudicators[0].source_commit.as_deref(), Some(head.as_str()));
    let adjudicator = run
        .checkpoint
        .expansions
        .iter()
        .flat_map(|expansion| &expansion.nodes)
        .find(|node| node.kind == Some(NodeKind::Adjudicate))
        .unwrap();
    assert_eq!(adjudicator.inputs.len(), 2, "both split panels are adjudicated together");
    assert_eq!(owner.calls.len(), 5, "four panel lenses and one adjudicator run");
    let gate = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "review-chain--r1--done")
        .unwrap();
    assert!(gate.gate.as_ref().is_some_and(|gate| gate.passed));
}

// Spec: docs/specs/agent-native.md#3-7-review-convergence
#[test]
fn later_same_head_split_continues_one_adjudicator_and_replays_its_union() {
    let answer = |verdict: &str| format!(
        "```findings\n{{\"schema_version\":1,\"findings\":[]}}\n```\nVERDICT: {verdict}\n"
    );
    let prior_findings: Vec<_> = (0..10)
        .map(|index| {
            serde_json::json!({
                "id": format!("ADJ-F{index}"),
                "severity": "high",
                "file": "src/prior.rs",
                "line": index + 1,
                "summary": format!("prior disposition snapshot {index} {}", "x".repeat(900)),
                "status": "resolved",
                "disposition": "accepted",
                "evidence": "The prior adjudicator reviewed this source finding",
                "proposed_fix": "Retain the recorded disposition"
            })
        })
        .collect();
    let prior_adjudication = format!("```findings\n{}\n```\nVERDICT: PASS\n", serde_json::json!({"schema_version": 1, "findings": prior_findings}));
    let head = "e".repeat(40);
    let panel = |id: &str, depends_on: &[&str]| {
        let lens = |name: &str| serde_json::json!({
            "id": name, "access": "read_only", "review_lens": name,
            "review_behaviour": "lens", "require_findings": true,
            "prompt": "review", "route": {"alias": "fixture"},
            "source": {"type": "commit", "commit": head}
        });
        serde_json::json!({
            "id": id, "kind": "composite", "access": "read_only",
            "depends_on": depends_on,
            "composite": {"min_successful": 2, "reduce_verdict": true,
                "nodes": [lens("changes"), lens("approve")],
                "exports": [
                    {"node": "changes", "outputs": ["result", "findings", "verdict"]},
                    {"node": "approve", "outputs": ["result", "findings", "verdict"]}
                ]}
        })
    };
    let gate = |id: &str, panels: &[&str]| serde_json::json!({
        "id": id, "kind": "gate", "access": "read_only", "depends_on": panels,
        "inputs": panels.iter().map(|panel| serde_json::json!({
            "node": panel, "outputs": ["verdict"]
        })).collect::<Vec<_>>(),
        "condition": {"type": "verdict", "expected": "VERDICT: PASS", "threshold": "all"}
    });
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "graph_id": "sequential-same-item-head-adjudication",
        "nodes": [{
            "id": "review-chain", "kind": "loop", "access": "read_only",
            "loop": {"until": "second-gate", "max_rounds": 1, "body": [
                panel("first", &[]), gate("first-gate", &["first"]),
                panel("second", &["first-gate"]), gate("second-gate", &["first", "second"])
            ]}
        }]
    })).unwrap();
    let mut observations = ["FAIL", "PASS", "PASS", "FAIL", "PASS"]
        .into_iter()
        .enumerate()
        .map(|(index, verdict)| {
            if index == 2 {
                terminal_with_result(GraphAttemptOutcome::Succeeded, &prior_adjudication)
            } else {
                terminal_with_result(GraphAttemptOutcome::Succeeded, &answer(verdict))
            }
        })
        .collect::<VecDeque<_>>();
    observations.push_back(AttemptObservation::Pending {
        attempt_ref: Some("run/adjudicator-continuation".into()),
    });
    let mut journal = MemoryJournal::default();
    let mut owner = FakeOwner {
        observations,
        ..Default::default()
    };

    let _first = advance(&document, &mut owner, &mut journal);
    let checkpoint = journal.0.clone().expect("pending continuation was journalled");
    let adjudicators: Vec<_> = checkpoint
        .nodes
        .iter()
        .filter(|node| node.id.ends_with("--adjudicate"))
        .collect();
    assert_eq!(adjudicators.len(), 1, "continuation keeps the stable node ID");
    let pending = adjudicators[0];
    assert_eq!(pending.state, NodeState::Running);
    assert!(pending.outputs.is_none(), "the superseded verdict is cleared");
    assert!(pending.review_settlement.is_none());
    assert_eq!(pending.previous_attempts.len(), 1);
    assert_eq!(pending.review_adjudication_receipts.len(), 1);
    assert_eq!(pending.review_adjudication_receipts[0].verdict, "VERDICT: PASS");
    assert_eq!(
        pending.review_adjudication_receipts[0].attempt_ref.as_deref(),
        None,
        "receipt snapshots remain usable when the provider supplied no immutable reference"
    );
    assert_eq!(
        pending.review_adjudication_receipts[0]
            .prior_outputs
            .full_artifact_findings
            .as_ref()
            .unwrap()
            .len(),
        10
    );
    assert_eq!(
        pending.review_adjudication_receipts[0]
            .prior_outputs
            .full_artifact_findings
            .as_ref()
            .unwrap()[0]
            .disposition
            .as_deref(),
        Some("accepted")
    );
    assert!(pending.review_adjudication_receipts[0]
        .panels
        .iter()
        .any(|panel| panel.ends_with("--first")));
    assert!(pending.review_adjudication_receipts[0]
        .panels
        .iter()
        .all(|panel| !panel.ends_with("--second")));
    assert!(pending.review_adjudication_receipts[0]
        .added_panels
        .iter()
        .any(|panel| panel.ends_with("--second")));
    assert_eq!(pending.attempts.len(), 1);
    assert_eq!(pending.attempts[0].state, AttemptState::Running);
    let later_gate = checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "review-chain--r1--second-gate")
        .unwrap();
    assert_eq!(later_gate.state, NodeState::Pending);
    assert!(later_gate.gate.is_none(), "the later gate waits for fresh evidence");

    let limits = crate::config::GraphLimits::default();
    let expanded = plans::expand(
        &document,
        &checkpoint.extensions,
        &checkpoint.expansions,
        &limits,
    )
    .unwrap();
    let effective_adjudicator = expanded
        .nodes
        .iter()
        .find(|node| node.id == pending.id)
        .unwrap();
    assert_eq!(effective_adjudicator.inputs.len(), 2);

    let directory_root = tempfile::tempdir().unwrap();
    let directory = provenant_os::fs::Directory::open_anchor(directory_root.path()).unwrap();
    let mut stored = serde_json::to_value(&checkpoint).unwrap();
    prompt_store::externalise(&directory, "adjudication-receipt-test", &mut stored).unwrap();
    assert!(stored["stored_text"]
        .as_object()
        .unwrap()
        .keys()
        .any(|path| path.contains("prior_outputs/full_artifact_findings")));
    let stored_bytes = serde_json::to_vec(&stored).unwrap();
    let restored = prompt_store::rehydrate(
        &directory,
        "adjudication-receipt-test",
        &stored_bytes,
    )
    .unwrap();
    let reopened: GraphCheckpoint = serde_json::from_slice(&restored).unwrap();
    let reopened_receipt = reopened
        .nodes
        .iter()
        .find(|node| node.id == pending.id)
        .unwrap()
        .review_adjudication_receipts[0]
        .clone();
    assert_eq!(reopened_receipt.attempt_ref, None);
    assert_eq!(
        reopened_receipt
            .prior_outputs
            .full_artifact_findings
            .as_ref()
            .unwrap()[0]
            .disposition
            .as_deref(),
        Some("accepted")
    );
    let mut replay_journal = MemoryJournal(Some(reopened));
    let mut replay_owner = FakeOwner {
        observations: [terminal_with_result(
            GraphAttemptOutcome::Succeeded,
            &answer("INCONCLUSIVE"),
        )]
        .into(),
        materialise: Some(fully_materialise_inputs),
        ..Default::default()
    };
    let replay = advance(&document, &mut replay_owner, &mut replay_journal).unwrap();
    assert_eq!(replay_owner.calls.len(), 1, "replay resumes the same attempt only");
    assert_eq!(replay_owner.calls[0].0.node_id, pending.id);
    assert_eq!(replay_owner.calls[0].0.retry_generation, Some(1));
    let replay_inputs = &replay_owner.file_requests[0];
    assert!(replay_inputs.iter().any(|request| request.label.contains("--first--")));
    assert!(replay_inputs.iter().any(|request| request.label.contains("--second--")));
    let adjudicator = replay
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == pending.id)
        .unwrap();
    assert_eq!(adjudicator.previous_attempts.len(), 1);
    assert_eq!(adjudicator.review_adjudication_receipts.len(), 1);
    assert_eq!(adjudicator.review_adjudication_receipts[0].attempt_ref, None);
    assert_eq!(
        adjudicator.review_adjudication_receipts[0]
            .prior_outputs
            .full_artifact_findings
            .as_ref()
            .unwrap()[0]
            .disposition
            .as_deref(),
        Some("accepted")
    );
    assert_eq!(adjudicator.attempts.len(), 1);
    assert_eq!(
        adjudicator
            .outputs
            .as_ref()
            .and_then(|outputs| outputs.verdict.as_deref()),
        Some("VERDICT: INCONCLUSIVE")
    );
    let gate = replay
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "review-chain--r1--second-gate")
        .unwrap();
    assert!(gate.gate.as_ref().is_some_and(|gate| !gate.passed));
}

// Spec: docs/specs/agent-native.md#37-review-convergence
#[test]
fn adjudicator_empty_pass_cannot_omit_blocking_input_findings() {
    let lens = |id: &str| serde_json::json!({
        "id": id, "access": "read_only", "review_lens": id,
        "review_behaviour": "lens", "require_findings": true,
        "prompt": "review", "route": {"alias": "fixture"}
    });
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "graph_id": "adjudicator-requires-full-dispositions",
        "nodes": [
            {"id":"panel", "kind":"composite", "access":"read_only",
             "composite":{"reduce_verdict":true,"nodes":[lens("first"),lens("second")],
                "exports":[
                    {"node":"first","outputs":["findings","verdict"]},
                    {"node":"second","outputs":["findings","verdict"]}
                ]}},
            {"id":"gate","kind":"gate","access":"read_only","depends_on":["panel"],
             "inputs":[{"node":"panel","outputs":["verdict"]}],
             "condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"}}
        ]
    }))
    .unwrap();
    let evidence = serde_json::json!([{
        "id":"F1", "severity":"blocking", "file":"src/lib.rs", "line":7,
        "summary":"blocking input finding", "status":"open",
        "evidence":"The input reaches the failing path", "proposed_fix":"Handle the input",
        "triage":{"decision":"confirmed","reason":"Reproduced","worth_fixing":true,
            "root_cause":"Unchecked input","blocking_class":"acceptance","shared_flaw":false}
    }]);
    let answer = |verdict: &str, findings: serde_json::Value| {
        format!("```findings\n{}\n```\nVERDICT: {verdict}\n", serde_json::json!({"schema_version":1,"findings":findings}))
    };
    let mut owner = FakeOwner {
        observations: VecDeque::from([
            terminal_with_result(GraphAttemptOutcome::Succeeded, &answer("FAIL", evidence.clone())),
            terminal_with_result(GraphAttemptOutcome::Succeeded, &answer("PASS", evidence)),
            terminal_with_result(GraphAttemptOutcome::Succeeded, &answer("PASS", serde_json::json!([]))),
        ]),
        materialise: Some(fully_materialise_inputs),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    let adjudicator = run
        .checkpoint
        .nodes
        .iter()
        .find(|progress| progress.id.ends_with("--adjudicate"))
        .expect("split panel must launch an adjudicator");
    assert_eq!(adjudicator.state, NodeState::Failed);
    assert_eq!(
        adjudicator.advance_code.as_deref(),
        Some("review_adjudication_incomplete")
    );
    assert!(run
        .checkpoint
        .nodes
        .iter()
        .find(|progress| progress.id == "gate")
        .and_then(|progress| progress.gate.as_ref())
        .is_none_or(|gate| !gate.passed));
}

// Spec: docs/specs/agent-native.md#37-review-convergence
#[test]
fn partial_split_panel_does_not_launch_an_adjudicator() {
    let lens = |id: &str| serde_json::json!({
        "id": id, "access": "read_only", "review_lens": id,
        "review_behaviour": "lens", "require_findings": true,
        "prompt": "review", "route": {"alias": "fixture"}
    });
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "graph_id": "partial-split-panel-is-not-adjudicable",
        "nodes": [
            {"id":"panel", "kind":"composite", "access":"read_only",
             "composite":{"min_successful":2,"reduce_verdict":true,
                "nodes":[lens("first"),lens("second"),lens("unavailable")],
                "exports":[
                    {"node":"first","outputs":["findings","verdict"]},
                    {"node":"second","outputs":["findings","verdict"]},
                    {"node":"unavailable","outputs":["findings","verdict"]}
                ]}},
            {"id":"gate","kind":"gate","access":"read_only","depends_on":["panel"],
             "inputs":[{"node":"panel","outputs":["verdict"]}],
             "condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"}}
        ]
    }))
    .unwrap();
    let answer = |verdict: &str| format!(
        "```findings\n{{\"schema_version\":1,\"findings\":[]}}\n```\nVERDICT: {verdict}\n"
    );
    let mut owner = FakeOwner {
        observations: VecDeque::from([
            terminal_with_result(GraphAttemptOutcome::Succeeded, &answer("FAIL")),
            terminal_with_result(GraphAttemptOutcome::Succeeded, &answer("PASS")),
            terminal(GraphAttemptOutcome::UsageLimited),
        ]),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    let panel = run
        .checkpoint
        .nodes
        .iter()
        .find(|progress| progress.id == "panel")
        .unwrap();
    let panel_outputs = panel.outputs.as_ref().unwrap();
    assert!(panel_outputs.partial);
    assert!(panel_outputs
        .warnings
        .iter()
        .any(|warning| warning.starts_with("review_split_verdict:")));
    assert!(!run.checkpoint.expansions.iter().any(|expansion| {
        expansion
            .nodes
            .iter()
            .any(|node| node.kind == Some(NodeKind::Adjudicate))
    }));
    assert!(run
        .checkpoint
        .nodes
        .iter()
        .find(|progress| progress.id == "gate")
        .and_then(|progress| progress.gate.as_ref())
        .is_none_or(|gate| !gate.passed));
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
                    terminal(GraphAttemptOutcome::DeclaredStop)
                } else {
                    terminal_with_result(GraphAttemptOutcome::Succeeded, clean)
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
    assert_eq!(panel.advance_code, None);
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
            terminal(GraphAttemptOutcome::DeclaredStop),
            terminal_with_result(GraphAttemptOutcome::Succeeded, clean),
            terminal(GraphAttemptOutcome::DeclaredStop),
            terminal_with_result(GraphAttemptOutcome::Succeeded, clean),
            terminal_with_result(GraphAttemptOutcome::Succeeded, clean),
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
                    terminal(GraphAttemptOutcome::DeclaredStop)
                } else {
                    terminal_with_result(GraphAttemptOutcome::Succeeded, clean)
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
        panel.advance_code.as_deref(),
        Some("coverage_below_minimum")
    );
    assert!(
        panel
            .advance_note
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
        terminal_with_result(GraphAttemptOutcome::Succeeded, &malformed),
        terminal_with_result(GraphAttemptOutcome::Succeeded, &findings_text("")),
        terminal(GraphAttemptOutcome::AdapterUnavailable),
        terminal_with_result(GraphAttemptOutcome::Succeeded, "verified"),
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
    assert_eq!(outputs.findings[1].severity, "high");
    assert_eq!(outputs.findings[0].status, "open");
    assert_eq!(outputs.findings[2].id, "F3");
    assert_eq!(outputs.findings[2].severity, "high");
    assert!(
        outputs
            .warnings
            .iter()
            .any(|warning| warning.contains("unrecognised value")),
        "{:?}",
        outputs.warnings
    );
    assert!(
        outputs.findings.iter().all(|finding| {
            finding.effective_severity() == finding.severity && finding.trigger().is_none()
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
        .zip(["blocking", "high", "high"])
    {
        assert_eq!(finding.severity, severity);
        assert_eq!(finding.effective_severity(), finding.severity);
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
        owner.observations[1] = terminal_with_result(GraphAttemptOutcome::Succeeded, &findings_text(""));
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
            terminal(GraphAttemptOutcome::OtherFailure),
            terminal_with_result(GraphAttemptOutcome::Succeeded, "b"),
            terminal_with_result(GraphAttemptOutcome::Succeeded, "c"),
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
        owner.observations[1] = terminal_with_result(GraphAttemptOutcome::Succeeded, &findings_text(""));
        owner.observations[2] = terminal(GraphAttemptOutcome::OtherFailure);
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
        terminal_with_result(GraphAttemptOutcome::DeclaredStop, &fail),
        terminal_with_result(GraphAttemptOutcome::Succeeded, &findings_text("")),
        terminal(GraphAttemptOutcome::AdapterUnavailable),
        terminal_with_result(GraphAttemptOutcome::Succeeded, "evaluated"),
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
        observations: VecDeque::from([terminal_with_result(GraphAttemptOutcome::Succeeded, &result)]),
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
        // A stop with nothing new is followed once, then returns to the chair:
        // no round accepts the node.
        let mut owner = FakeOwner {
            observations: VecDeque::from([
                terminal_with_result(GraphAttemptOutcome::DeclaredStop, &text),
                terminal_with_result(GraphAttemptOutcome::DeclaredStop, &text),
            ]),
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
    owner.observations[1] = terminal_with_result(GraphAttemptOutcome::Succeeded, &findings_text(""));
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
            .advance_note
            .as_deref()
            .unwrap()
            .contains("lenses (Failed)")
    );
}

#[test]
fn replay_attributes_an_unexplained_blocked_node_without_overwriting_other_reasons() {
    let (document, mut owner) = partial_review(3);
    owner.observations[1] = terminal_with_result(GraphAttemptOutcome::Succeeded, &findings_text(""));
    let mut journal = MemoryJournal::default();
    advance(&document, &mut owner, &mut journal).unwrap();
    let checkpoint = journal.0.as_mut().unwrap();
    let verify = checkpoint
        .nodes
        .iter_mut()
        .find(|n| n.id == "verify")
        .unwrap();
    assert_eq!(verify.state, NodeState::Blocked);
    verify.advance_note = None;
    verify.advance_code = None;

    let replay = advance(&document, &mut FakeOwner::default(), &mut journal).unwrap();
    let verify = replay
        .checkpoint
        .nodes
        .iter()
        .find(|n| n.id == "verify")
        .unwrap();
    assert_eq!(
        verify.advance_code.as_deref(),
        Some("upstream_blocked")
    );
    assert_eq!(
        verify.advance_note.as_deref(),
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
    verify.advance_note = Some("existing reason".into());
    verify.advance_code = Some("other_reason".into());
    let replay = advance(&document, &mut FakeOwner::default(), &mut journal).unwrap();
    let verify = replay
        .checkpoint
        .nodes
        .iter()
        .find(|n| n.id == "verify")
        .unwrap();
    assert_eq!(verify.advance_note.as_deref(), Some("existing reason"));
    assert_eq!(verify.advance_code.as_deref(), Some("other_reason"));
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
        node("lens-b").advance_code.as_deref(),
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
        Some(GraphAttemptOutcome::AdapterUnavailable)
    );
    assert!(excerpt.contains(&format!(
        "{}: {}",
        unavailable.id,
        unavailable.advance_note.as_deref().unwrap()
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
fn minimum_successful_composite_joins_exported_gate_recovery_alternatives() {
    for (
        graph_id,
        verdict,
        review_outcome,
        recovery_outcome,
        extra_non_export,
        check_state,
        recovery_state,
        composite_state,
        outcome,
    ) in [
        (
            "ci-pass",
            "VERDICT: PASS",
            GraphAttemptOutcome::Succeeded,
            GraphAttemptOutcome::Succeeded,
            false,
            NodeState::Succeeded,
            NodeState::Skipped,
            NodeState::Succeeded,
            GraphOutcome::Succeeded,
        ),
        (
            "ci-repaired",
            "VERDICT: FAIL",
            GraphAttemptOutcome::Succeeded,
            GraphAttemptOutcome::Succeeded,
            false,
            NodeState::Failed,
            NodeState::Succeeded,
            NodeState::Succeeded,
            GraphOutcome::Succeeded,
        ),
        (
            "ci-exhausted",
            "VERDICT: FAIL",
            GraphAttemptOutcome::Succeeded,
            GraphAttemptOutcome::TaskFailed,
            false,
            NodeState::Failed,
            NodeState::Failed,
            NodeState::Failed,
            GraphOutcome::Failed,
        ),
        (
            "ci-blocked-non-export",
            "VERDICT: PASS",
            GraphAttemptOutcome::Succeeded,
            GraphAttemptOutcome::Succeeded,
            true,
            NodeState::Succeeded,
            NodeState::Skipped,
            NodeState::Blocked,
            GraphOutcome::Failed,
        ),
        (
            "ci-blocked-export",
            "VERDICT: PASS",
            GraphAttemptOutcome::TaskFailed,
            GraphAttemptOutcome::Succeeded,
            false,
            NodeState::Blocked,
            NodeState::Blocked,
            NodeState::Blocked,
            GraphOutcome::Failed,
        ),
    ] {
        let mut members = vec![
            serde_json::json!({
                "id":"review", "access":"read_only", "prompt":"review",
                "route":{"alias":"fixture"}
            }),
            serde_json::json!({
                "id":"check", "kind":"gate", "access":"read_only",
                "depends_on":["review"], "on_fail":"repair",
                "inputs":[{"node":"review", "outputs":["verdict"]}],
                "condition":{"type":"verdict", "expected":"VERDICT: PASS", "threshold":"all"}
            }),
            serde_json::json!({
                "id":"repair", "access":"read_only", "prompt":"repair",
                "route":{"alias":"fixture"}, "depends_on":["check"]
            }),
        ];
        if extra_non_export {
            members.push(serde_json::json!({
                "id":"follow-up", "access":"read_only", "prompt":"follow up",
                "route":{"alias":"fixture"}, "depends_on":["repair"]
            }));
        }
        let document: GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version":2, "graph_id":graph_id, "nodes":[{
                "id":"ci-outcome", "kind":"composite", "access":"read_only",
                "composite":{
                    "min_successful":1,
                    "nodes":members,
                    "exports":[
                        {"node":"check", "outputs":["result"]},
                        {"node":"repair", "outputs":["result"]}
                    ]
                }
            }]
        }))
        .unwrap();
        let mut owner = FakeOwner {
            observations: [
                terminal_with_result(review_outcome, verdict),
                terminal(recovery_outcome),
            ]
            .into(),
            ..Default::default()
        };
        let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
        let progress = |id: &str| {
            run.checkpoint
                .nodes
                .iter()
                .find(|node| node.id == id || node.id == format!("ci-outcome--{id}"))
                .unwrap()
        };
        assert_eq!(progress("check").state, check_state, "{graph_id}");
        assert_eq!(progress("repair").state, recovery_state, "{graph_id}");
        assert_eq!(progress("ci-outcome").state, composite_state, "{graph_id}");
        assert_eq!(run.outcome, Some(outcome), "{graph_id}");
    }
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
                terminal_with_result(GraphAttemptOutcome::Succeeded, &findings_text(&finding)),
                terminal_with_result(
                    GraphAttemptOutcome::Succeeded,
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
            assert_eq!(finding.effective_severity(), finding.severity);
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
            terminal_with_result(GraphAttemptOutcome::Succeeded, &result),
            terminal_with_result(GraphAttemptOutcome::Succeeded, &result),
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
        lenses.advance_code.as_deref(),
        Some("coverage_below_minimum")
    );
    let reason = lenses.advance_note.as_deref().unwrap();
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
            terminal_with_result(GraphAttemptOutcome::Succeeded, "evidence"),
            terminal_with_result(GraphAttemptOutcome::Succeeded, "done"),
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
