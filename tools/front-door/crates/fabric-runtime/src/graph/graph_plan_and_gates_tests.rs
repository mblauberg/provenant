use super::*;

#[test]
fn plan_fixture_expands_and_replays_without_replanning() {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "graph_id": "planned", "nodes": [{
            "id": "planner", "kind": "task", "access": "read_only", "prompt": "Plan two slices",
            "plan": {"kinds": ["task"], "max_nodes": 2}, "route": {"alias": "senior"}
        }]
    }))
    .unwrap();
    let fragment = r#"```fragment
{"schema_version":1,"nodes":[
{"id":"first","goal":"First slice","access":"read_only","write_scope":["docs/a"],"acceptance_checks":["read a"]},
{"id":"second","brief":"Second slice","access":"read_only","write_scope":["docs/b"],"acceptance_checks":["read b"],"depends_on":["first"]}
]}
```"#;
    let mut owner = FakeOwner {
        observations: [fragment, "first done", "second done"]
            .into_iter()
            .map(|text| terminal_with_result(AttemptOutcome::Succeeded, text))
            .collect(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(run.checkpoint.successful());
    assert_eq!(run.checkpoint.nodes.len(), 3);
    assert_eq!(owner.calls[1].1.alias.as_deref(), Some("senior"));
    assert!(owner.prompts[1].contains("read a"));
    let replay = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.checkpoint, replay.checkpoint);
    assert_eq!(owner.calls.len(), 3);
}

#[test]
fn plan_fragment_target_is_checked_before_expansion_is_published() {
    // The document names the destination, so the fragment passes the plan
    // check and reaches the owner's target validation.
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "graph_id": "planned", "integration_branch": "target", "nodes": [{
            "id": "planner", "kind": "task", "access": "read_only", "prompt": "Plan",
            "plan": {"kinds": ["task"], "max_nodes": 1}, "route": {"alias": "senior"}
        }]
    }))
    .unwrap();
    let fragment = r#"```fragment
{"schema_version":1,"nodes":[{"id":"writer","goal":"Write","access":"worktree_write","worktree":"lane","write_scope":["src"],"acceptance_checks":["check"],"delivery":"integrate:target"}]}
```"#;
    let mut owner = FakeOwner {
        observations: [terminal_with_result(AttemptOutcome::Succeeded, fragment)].into(),
        refuse_expansion_target: true,
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    assert!(matches!(
        advance(&document, &mut owner, &mut journal),
        Err(GraphError::Run(_))
    ));
    assert!(journal.0.as_ref().unwrap().expansions.is_empty());
    assert_eq!(owner.calls.len(), 1);
    assert_eq!(owner.expansion_validations, 1);
}

#[test]
fn gate_verdict_fixture_is_attempt_less_and_replays() {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "graph_id": "g-test", "nodes": [
            {"id": "review", "prompt": "Review", "access": "read_only",
             "route": {"adapter": "fixture", "model": "fixture/model"}},
            {"id": "gate", "kind": "gate", "access": "read_only", "depends_on": ["review"],
             "inputs": [{"node": "review", "outputs": ["verdict"]}],
             "condition": {"type": "verdict", "expected": "VERDICT: PASS", "threshold": "all"}}
        ]
    }))
    .unwrap();
    let mut owner = FakeOwner {
        observations: VecDeque::from([terminal_with_result(
            AttemptOutcome::Succeeded,
            "VERDICT: PASS",
        )]),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert_eq!(run.checkpoint.nodes[1].state, NodeState::Succeeded);
    assert!(run.checkpoint.nodes[1].attempts.is_empty());
    let replay = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(replay.checkpoint, run.checkpoint);
    assert_eq!(owner.calls.len(), 1);
}

#[test]
fn verdict_gate_matches_review_tokens() {
    let (document, mut owner) = verdict_fixture(VerdictThreshold::All, &[Some("APPROVE")]);
    let mut checkpoint = advance(&document, &mut owner, &mut MemoryJournal::default())
        .unwrap()
        .checkpoint;
    assert!(checkpoint.successful());
    let (mut plan, positions) = validate_plan(&document).unwrap();
    for (expected, actual, passed) in [
        ("VERDICT: PASS", "VERDICT: APPROVE", true),
        ("VERDICT: PASS", "VERDICT: PASS", true),
        ("VERDICT: PASS", "VERDICT: APPROVE_WITH_NITS", false),
        ("APPROVE_WITH_NITS", "VERDICT: APPROVE_WITH_NITS", true),
        ("INCONCLUSIVE", "VERDICT: INCONCLUSIVE", true),
        ("repair", "repair", true),
        ("repair", "REPAIR", false),
        ("LANDED", "LANDED", true),
        ("LANDED", "VERDICT: LANDED", false),
    ] {
        let condition = GateCondition::Verdict {
            extra: Default::default(),
            expected: expected.into(),
            threshold: VerdictThreshold::All,
        };
        checkpoint.nodes[0].outputs.as_mut().unwrap().verdict = Some(actual.into());
        assert_eq!(
            gate_failure(
                &condition,
                &plan.nodes[1],
                &checkpoint,
                &positions,
                &Default::default()
            )
            .is_none(),
            passed,
            "{expected} vs {actual}"
        );
    }
    // Feed a gate's bare PASS output into another verdict gate.
    plan.nodes[1].inputs[0].node = "gate".into();
    let condition = GateCondition::Verdict {
        extra: Default::default(),
        expected: "VERDICT: PASS".into(),
        threshold: VerdictThreshold::All,
    };
    assert!(
        gate_failure(
            &condition,
            &plan.nodes[1],
            &checkpoint,
            &positions,
            &Default::default()
        )
        .is_none()
    );
}

#[test]
fn gate_threshold_fixtures_count_missing_verdicts_and_strict_majorities() {
    for (threshold, verdicts, passed) in [
        (
            VerdictThreshold::All,
            vec![Some("PASS"), Some("PASS"), Some("FAIL")],
            false,
        ),
        (
            VerdictThreshold::Any,
            vec![Some("PASS"), None, Some("FAIL")],
            true,
        ),
        (VerdictThreshold::Any, vec![None, Some("FAIL")], false),
        (
            VerdictThreshold::Majority,
            vec![Some("PASS"), Some("PASS"), None],
            true,
        ),
        (VerdictThreshold::Majority, vec![Some("PASS"), None], false),
        (
            VerdictThreshold::Count(2),
            vec![Some("PASS"), Some("PASS"), None],
            true,
        ),
        (
            VerdictThreshold::Count(3),
            vec![Some("PASS"), Some("PASS"), None],
            false,
        ),
    ] {
        let (document, mut owner) = verdict_fixture(threshold, &verdicts);
        let mut journal = MemoryJournal::default();
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        let gate = run.checkpoint.nodes.last().unwrap();
        assert_eq!(gate.gate.as_ref().unwrap().passed, passed);
        assert_eq!(gate.outputs, Some(gate_outputs(passed)));
        assert!(gate.attempts.is_empty());
        assert_eq!(run.checkpoint.successful(), passed);
        let mut legacy = serde_json::to_value(&run.checkpoint).unwrap();
        for node in legacy["nodes"].as_array_mut().unwrap() {
            if let Some(outputs) = node
                .get_mut("outputs")
                .and_then(serde_json::Value::as_object_mut)
            {
                outputs.remove("findings_available");
            }
        }
        let legacy: GraphCheckpoint = serde_json::from_value(legacy).unwrap();
        journal.0 = Some(legacy.clone());
        assert_eq!(
            advance(&document, &mut owner, &mut journal)
                .unwrap()
                .checkpoint,
            legacy
        );
        journal.0 =
            Some(serde_json::from_value(serde_json::to_value(&run.checkpoint).unwrap()).unwrap());
        assert_eq!(
            advance(&document, &mut owner, &mut journal)
                .unwrap()
                .checkpoint,
            run.checkpoint
        );
    }
}

#[test]
fn gate_budget_fixtures_fail_closed_and_preserve_the_decision_on_resume() {
    for (tokens, partial, limit, passed) in [
        (Some(9), false, 10, true),
        (Some(10), false, 10, false),
        (Some(11), false, 10, false),
        (None, false, 10, false),
        (Some(1), true, 10, false),
    ] {
        let mut gate = gate_node(GateCondition::Budget {
            extra: Default::default(),
        });
        gate.depends_on.push("review".into());
        let document = graph(vec![node("review", route("fixture")), gate]);
        let mut observation = terminal(AttemptOutcome::Succeeded);
        if let AttemptObservation::Terminal { attempt_ref, .. } = &mut observation {
            *attempt_ref = Some("host/attempt".into());
        }
        let mut owner = FakeOwner {
            observations: VecDeque::from([observation]),
            usage: AttemptUsage {
                tokens,
                tokens_partial: partial,
                ..Default::default()
            },
            ..Default::default()
        };
        let budget = crate::config::GraphBudget {
            per_graph: crate::config::BudgetLimit {
                tokens: Some(limit),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut journal = MemoryJournal::default();
        let run = advance_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &Default::default(),
            &budget,
        )
        .unwrap();
        assert_eq!(run.status, GraphStatus::Complete);
        assert_eq!(
            run.checkpoint.nodes[1].gate.as_ref().unwrap().passed,
            passed
        );
        let replay = resume_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &Default::default(),
            &budget,
            &["budget.per_graph.tokens=100".into()],
        )
        .unwrap();
        assert_eq!(replay.checkpoint.nodes, run.checkpoint.nodes);
        assert_eq!(owner.calls.len(), 1);
    }
}

#[test]
fn gate_budget_overrides_warn_only_for_configured_metrics() {
    use crate::config::{BudgetLimit, GraphBudget, UnknownUsage};
    for policy in [UnknownUsage::Warn, UnknownUsage::Pause] {
        for (tokens, limit, passed) in [
            (None, Some(10), false),
            (Some(9), Some(10), true),
            (None, None, true),
        ] {
            let mut gate = gate_node(GateCondition::Budget {
                extra: Default::default(),
            });
            gate.depends_on.push("review".into());
            let document = graph(vec![node("review", route("fixture")), gate]);
            let mut observation = terminal(AttemptOutcome::Succeeded);
            if let AttemptObservation::Terminal { attempt_ref, .. } = &mut observation {
                *attempt_ref = Some("host/attempt".into());
            }
            let mut owner = FakeOwner {
                observations: VecDeque::from([observation]),
                usage: AttemptUsage {
                    tokens,
                    ..Default::default()
                },
                ..Default::default()
            };
            let budget = GraphBudget {
                unknown_usage: policy,
                per_graph: BudgetLimit {
                    tokens: limit,
                    ..Default::default()
                },
                ..Default::default()
            };
            let run = advance_with_budget(
                &document,
                &mut owner,
                &mut MemoryJournal::default(),
                &Default::default(),
                &budget,
            )
            .unwrap();
            assert_eq!(
                run.checkpoint.nodes[1].gate.as_ref().unwrap().passed,
                passed
            );
            assert_eq!(run.status, GraphStatus::Complete);
        }
    }
}

#[test]
fn agent_json_still_requires_explicit_access() {
    for kind in [None, Some(NodeKind::Task), Some(NodeKind::Synthesise)] {
        let mut agent = node("agent", route("fixture"));
        agent.kind = kind;
        let mut wire = serde_json::to_value(graph(vec![agent])).unwrap();
        wire["nodes"][0].as_object_mut().unwrap().remove("access");
        assert_eq!(
            serde_json::from_value::<GraphDocument>(wire)
                .unwrap_err()
                .classify(),
            serde_json::error::Category::Data
        );
    }
}

#[test]
fn gate_skipped_chain_replays_every_save_after_the_gate_decision() {
    #[derive(Default)]
    struct RecordingJournal {
        current: MemoryJournal,
        saves: Vec<GraphCheckpoint>,
    }
    impl GraphJournal for RecordingJournal {
        type Error = Infallible;
        fn load(&mut self, graph_id: &str) -> Result<Option<GraphCheckpoint>, Self::Error> {
            self.current.load(graph_id)
        }
        fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error> {
            self.saves.push(checkpoint.clone());
            self.current.save(checkpoint)
        }
    }
    let mut gate = gate_node(GateCondition::Budget {
        extra: Default::default(),
    });
    gate.on_fail = Some("recovery".into());
    let mut nodes = vec![gate];
    for (id, parent) in [
        ("recovery", "gate"),
        ("child", "recovery"),
        ("grandchild", "child"),
    ] {
        let mut child = node(id, route("fixture"));
        child.depends_on.push(parent.into());
        nodes.push(child);
    }
    let document = graph(nodes);
    let mut journal = RecordingJournal::default();
    let settled = advance(&document, &mut FakeOwner::default(), &mut journal).unwrap();
    assert!(
        settled.checkpoint.nodes[1..]
            .iter()
            .all(|node| node.state == NodeState::Skipped
                && node.attempts.is_empty()
                && node.outputs.is_none())
    );
    let snapshots: Vec<_> = journal
        .saves
        .into_iter()
        .filter(|checkpoint| checkpoint.nodes[0].gate.is_some())
        .collect();
    assert!(!snapshots.is_empty());
    for checkpoint in snapshots {
        let mut replay = MemoryJournal(Some(
            serde_json::from_slice(&serde_json::to_vec(&checkpoint).unwrap()).unwrap(),
        ));
        let mut owner = FakeOwner::default();
        assert_eq!(
            advance(&document, &mut owner, &mut replay).unwrap(),
            settled
        );
        assert!(owner.calls.is_empty());
    }
}

#[test]
fn gate_reachability_handles_two_gates_and_deep_paths() {
    let mut nodes = Vec::new();
    for id in ["first", "second"] {
        let mut gate = gate_node(GateCondition::Budget {
            extra: Default::default(),
        });
        gate.id = id.into();
        gate.on_fail = Some(format!("{id}-recovery"));
        nodes.push(gate);
        for path in ["success", "recovery"] {
            let mut parent = id.to_owned();
            for depth in 0..4 {
                let name = if depth == 0 {
                    format!("{id}-{path}")
                } else {
                    format!("{id}-{path}-{depth}")
                };
                let mut child = node(&name, route("fixture"));
                child.depends_on.push(parent);
                parent = name;
                nodes.push(child);
            }
        }
    }
    let mut join = node("join", route("fixture"));
    join.depends_on = vec!["first-success-3".into(), "second-success-3".into()];
    nodes.push(join);
    let mut document = graph(nodes);
    assert!(validate_plan(&document).is_ok());
    document
        .nodes
        .last_mut()
        .unwrap()
        .depends_on
        .push("first-recovery-3".into());
    document.nodes.reverse();
    assert_eq!(
        validate_plan(&document).unwrap_err(),
        "node requires both gate outcomes"
    );
}

#[test]
fn gate_reachability_accepts_a_limit_sized_graph_and_rejects_lower_limits() {
    let limits = crate::config::GraphLimits::default();
    let mut nodes = Vec::new();
    for index in 0..limits.nodes / 2 {
        let mut gate = gate_node(GateCondition::Budget {
            extra: Default::default(),
        });
        gate.id = format!("gate-{index}");
        let mut recovery = node(&format!("recovery-{index}"), route("fixture"));
        recovery.depends_on.push(gate.id.clone());
        gate.on_fail = Some(recovery.id.clone());
        nodes.extend([gate, recovery]);
    }
    let document = graph(nodes);
    assert_eq!(document.nodes.len(), limits.nodes);
    assert!(validate_plan_with_limits(&document, &limits).is_ok());
    let mut smaller = limits.clone();
    smaller.nodes -= 1;
    assert!(
        validate_plan_with_limits(&document, &smaller)
            .unwrap_err()
            .to_string()
            .contains("limits.nodes")
    );
    smaller = limits;
    smaller.edges = document.nodes.len() / 2 - 1;
    assert!(
        validate_plan_with_limits(&document, &smaller)
            .unwrap_err()
            .to_string()
            .contains("limits.edges")
    );
}

#[test]
fn gate_verdict_rejects_spoofed_multiple_and_malformed_lines() {
    for text in [
        "VERDICT: FAIL\nVERDICT: PASS".to_owned(),
        "VERDICT: PASS\nVERDICT: PASS".to_owned(),
        "VERDICT: PASS\nmore explanation".to_owned(),
        "VERDICT: PASS x".to_owned(),
        "verdict: pass".to_owned(),
        format!("VERDICT: PASS{}", " ".repeat(1024)),
        format!(
            "{}\nVERDICT: FAIL\n{}\nVERDICT: PASS",
            "x".repeat(5000),
            "x".repeat(20000)
        ),
    ] {
        for threshold in [
            VerdictThreshold::All,
            VerdictThreshold::Any,
            VerdictThreshold::Majority,
            VerdictThreshold::Count(1),
        ] {
            let (document, mut owner) =
                verdict_fixture(threshold, &[Some("PASS"), Some("PASS"), Some("PASS")]);
            owner.observations[0] = terminal_with_result(AttemptOutcome::Succeeded, &text);
            let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
            assert_eq!(run.checkpoint.nodes[3].state, NodeState::Failed, "{text:?}");
            assert_eq!(
                run.checkpoint.nodes[0].outputs.as_ref().unwrap().verdict,
                None
            );
        }
    }
}

#[test]
fn gate_on_fail_routes_only_the_active_path_and_delivers_after_recovery() {
    for (passed, recovery_succeeds) in [(true, true), (false, true), (false, false)] {
        let (mut document, mut owner) = verdict_fixture(
            VerdictThreshold::All,
            &[Some(if passed { "PASS" } else { "FAIL" })],
        );
        document.nodes[1].on_fail = Some("recovery".into());
        let mut recovery = node("recovery", route("fixture"));
        recovery.depends_on.push("gate".into());
        let mut success = node("success", route("fixture"));
        success.depends_on.push("gate".into());
        document.nodes.extend([recovery, success]);
        document.integration_branch = Some("integration".into());
        document.delivery = Some(provenant_records::run::DeliveryMode::Pr);
        owner.observations.push_back(terminal(if recovery_succeeds {
            AttemptOutcome::Succeeded
        } else {
            AttemptOutcome::TaskFailed
        }));
        let mut journal = MemoryJournal::default();
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        assert_eq!(
            run.checkpoint.nodes[2].state,
            if passed {
                NodeState::Skipped
            } else if recovery_succeeds {
                NodeState::Succeeded
            } else {
                NodeState::Failed
            }
        );
        assert_eq!(
            run.checkpoint.nodes[3].state,
            if passed {
                NodeState::Succeeded
            } else {
                NodeState::Skipped
            }
        );
        assert_eq!(owner.calls.len(), 2);
        assert_eq!(owner.deliveries, usize::from(recovery_succeeds));
        assert!(
            run.checkpoint.nodes[1]
                .deterministic_summary()
                .unwrap()
                .contains(if passed { "gate PASS" } else { "gate FAIL" })
        );
        assert_eq!(
            advance(&document, &mut owner, &mut journal)
                .unwrap()
                .checkpoint,
            run.checkpoint
        );
    }
}

#[test]
fn gate_checkpoint_rejects_attempts_inconsistent_results_and_forged_skips() {
    let (document, mut owner) = verdict_fixture(VerdictThreshold::All, &[Some("PASS")]);
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    for mutation in 0..5 {
        let mut corrupt = run.checkpoint.clone();
        match mutation {
            0 => corrupt.nodes[1].gate.as_mut().unwrap().passed = false,
            1 => corrupt.nodes[1].attempts = corrupt.nodes[0].attempts.clone(),
            2 => corrupt.nodes[1].state = NodeState::Running,
            3 => corrupt.nodes[0].gate = corrupt.nodes[1].gate.clone(),
            _ => {
                corrupt.nodes[1].state = NodeState::Skipped;
                corrupt.nodes[1].gate = None;
                corrupt.nodes[1].outputs = None;
            }
        }
        journal.0 = Some(corrupt);
        assert!(matches!(
            advance(&document, &mut owner, &mut journal),
            Err(GraphError::BadCheckpoint { .. })
        ));
    }
}

#[test]
fn gate_inactive_path_waits_for_other_parents_before_settling() {
    let (mut document, mut owner) = verdict_fixture(VerdictThreshold::All, &[Some("PASS")]);
    document.nodes[1].on_fail = Some("recovery".into());
    let mut recovery = node("recovery", route("fixture"));
    recovery.depends_on = vec!["gate".into(), "slow".into()];
    document
        .nodes
        .extend([node("slow", route("fixture")), recovery]);
    owner.observations.extend([
        AttemptObservation::Pending { attempt_ref: None },
        terminal(AttemptOutcome::TaskFailed),
    ]);
    let mut journal = MemoryJournal::default();
    let waiting = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(waiting.checkpoint.nodes[3].state, NodeState::Pending);
    let settled = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(settled.checkpoint.nodes[3].state, NodeState::Blocked);
    assert_eq!(
        advance(&document, &mut owner, &mut journal)
            .unwrap()
            .checkpoint,
        settled.checkpoint
    );
}

#[test]
fn gate_only_graph_obeys_structural_limits_before_evaluating() {
    let document = graph(vec![
        gate_node(GateCondition::Budget {
            extra: Default::default(),
        }),
        GraphNode {
            family_group: None,
            id: "second".into(),
            ..gate_node(GateCondition::Budget {
                extra: Default::default(),
            })
        },
    ]);
    let limits = crate::config::GraphLimits {
        nodes: 1,
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance_with_budget(
        &document,
        &mut FakeOwner::default(),
        &mut journal,
        &limits,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert!(run.checkpoint.nodes.iter().all(|node| node.gate.is_none()));
}

#[test]
fn gate_rejects_unreachable_joins_of_mutually_exclusive_paths() {
    for transitive in [false, true] {
        let (mut document, _) = verdict_fixture(VerdictThreshold::All, &[Some("PASS")]);
        document.nodes[1].on_fail = Some("recovery".into());
        let mut recovery = node("recovery", route("fixture"));
        recovery.depends_on.push("gate".into());
        let mut success = node("success", route("fixture"));
        success.depends_on.push("gate".into());
        let mut join = node("join", route("fixture"));
        join.depends_on = vec![
            if transitive { "success" } else { "gate" }.into(),
            "recovery".into(),
        ];
        document.nodes.extend([recovery, success, join]);
        assert_eq!(
            validate_plan(&document).err(),
            Some("node requires both gate outcomes")
        );
    }
}

#[test]
fn gate_validation_rejects_agent_fields_bad_thresholds_and_failure_cycles() {
    let (document, _) = verdict_fixture(VerdictThreshold::All, &[Some("PASS")]);
    for mutation in 0..6 {
        let mut document = document.clone();
        let expected = match mutation {
            0 => {
                document.nodes[1].route = Some(route("fixture"));
                "gate requires a condition and no agent configuration"
            }
            1 => {
                document.nodes[1].condition = Some(GateCondition::Verdict {
                    extra: Default::default(),
                    expected: "PASS".into(),
                    threshold: VerdictThreshold::Count(0),
                });
                "invalid verdict gate"
            }
            2 => {
                document.nodes[1].on_fail = Some("missing".into());
                "on_fail target must depend on its gate"
            }
            3 => {
                document.nodes[1].on_fail = Some("review-0".into());
                document.nodes[0].depends_on.push("gate".into());
                "dependency cycle"
            }
            4 => {
                document.nodes[0].condition = Some(GateCondition::Budget {
                    extra: Default::default(),
                });
                "gate requires a condition and no agent configuration"
            }
            _ => {
                document.nodes[1].inputs.clear();
                "invalid verdict gate"
            }
        };
        assert_eq!(validate_plan(&document).err(), Some(expected));
    }
}
