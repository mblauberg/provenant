use super::*;
use provenant_os::{clock::SystemClock, fs::SystemPathResolver};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;

#[test]
fn fragments_cannot_inject_persisted_topology_through_extra_fields() {
    for field in ["composite_members", "after_expansion"] {
        let mut child = slice("injected", &[]);
        child[field] = json!(["plan--missing"]);
        let output = result(vec![child]);
        let run = advance(
            &planner(1),
            &mut fixture_owner(&[&output]),
            &mut MemoryJournal::default(),
        )
        .unwrap();
        assert!(run.checkpoint.expansions.is_empty());
        assert_eq!(
            run.checkpoint.pause_cause(),
            Some(PauseCause::InvalidPlanExpansion)
        );
        assert!(
            run.checkpoint
                .pause_reason()
                .unwrap()
                .contains("invalid fragment")
        );
    }
}

#[test]
fn planner_fragment_writers_use_the_declared_worktree_template() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version": 1,
        "graph_id": "planned",
        "nodes": []
    }))
    .unwrap();
    let checkpoint: GraphCheckpoint = serde_json::from_value(json!({
        "schema_version": 1,
        "graph_id": "planned",
        "plan_digest": "fixture",
        "nodes": []
    }))
    .unwrap();
    let local_nodes: Vec<GraphNode> = serde_json::from_value(json!([{
        "id": "writer",
        "prompt": "Write the checked slice",
        "write_scope": ["src"],
        "acceptance_checks": ["Verify the slice"],
        "access": "worktree_write",
        "route": {"alias": "senior"}
    }]))
    .unwrap();
    let mut ports = provenant_config::PortTable::builtin();
    ports.worktree_layout.roots = vec!["trees".into()];
    ports.worktree_layout.default = "trees/<branch>".into();

    let flattened = plans::flatten_fragment(
        local_nodes,
        None,
        None,
        &document,
        &checkpoint,
        &ports.worktree_layout,
    )
    .unwrap();

    assert_eq!(
        flattened[0].worktree.as_deref(),
        Some("trees/planned-writer")
    );
    assert_eq!(
        flattened[0].extra.get("provision_worktree"),
        Some(&serde_json::Value::Bool(true))
    );
}

#[test]
fn admission_planner_fragment_capability() {
    let fragment = json!({
        "schema_version": 1,
        "nodes": [{
            "id": "child", "access": "read_only", "prompt": "Do a slice",
            "acceptance_checks": ["Check it"], "route": {"alias": "senior"},
            "family_group": "group"
        }]
    });
    let output = format!("```fragment\n{}\n```", fragment);
    let mut owner = fixture_owner(&[&output]);
    let run = advance(&planner(4), &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert_eq!(
        run.checkpoint.pause_cause(),
        Some(PauseCause::InvalidPlanExpansion)
    );
    assert!(
        run.checkpoint
            .pause_reason()
            .unwrap()
            .contains("invalid fragment"),
        "{:?}",
        run.checkpoint.pause_reason()
    );
    assert!(run.checkpoint.expansions.is_empty());
}

#[test]
fn planner_fragment_inherits_skill_requirement() {
    let mut doc = planner(2);
    doc.nodes[0].require_skill_location = true;
    let output = result(vec![slice("child", &[])]);
    let mut owner = fixture_owner(&[&output, "done", "done"]);
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.checkpoint.expansions.len(), 1);
    assert!(
        run.checkpoint.expansions[0]
            .nodes
            .iter()
            .any(|node| node.require_skill_location)
    );
    assert!(
        owner
            .skill_requirements
            .iter()
            .take(2)
            .all(|required| *required)
    );
}

#[test]
fn raw_expansion_rejects_each_injected_topology_field() {
    let doc = planner(2);
    let output = result(vec![slice("child", &[])]);
    let mut owner = fixture_owner(&[&output]);
    owner
        .observations
        .push_back(AttemptObservation::Pending { attempt_ref: None });
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    let wire = serde_json::to_value(&run.checkpoint).unwrap();
    for field in ["composite_members", "after_expansion"] {
        let mut tampered = wire.clone();
        let node = &mut tampered["expansions"][0]["nodes"][0];
        let id = node["id"].as_str().unwrap().to_owned();
        node[field] = json!(["plan--injected"]);
        let mut journal = MemoryJournal(Some(serde_json::from_value(tampered).unwrap()));
        let error = advance(&doc, &mut fixture_owner(&[]), &mut journal).unwrap_err();
        assert_eq!(error.code(), "bad_checkpoint");
        assert!(error.to_string().contains(&id), "{error}");
    }
}

#[test]
fn planned_fan_out_survives_checkpoint_json_and_mixed_settlement() {
    for fallback in [false, true] {
        let mut doc = planner(8);
        let leaves: Vec<_> = (0..4)
            .map(|i| {
                json!({
                    "id": format!("lens{i}"), "access":"read_only", "prompt":"Review",
                    "route":{"alias":"senior"}, "acceptance_checks":["Review"]
                })
            })
            .collect();
        let exports: Vec<_> = (0..4)
            .map(|i| json!({"node":format!("lens{i}"),"outputs":["result"]}))
            .collect();
        let planner_result = if fallback {
            doc.nodes[0].plan.as_mut().unwrap().fallback = Some(
                serde_json::from_value(json!({
                    "nodes":leaves, "exports":exports, "min_successful":1
                }))
                .unwrap(),
            );
            "invalid fragment".to_owned()
        } else {
            doc.nodes[0].plan.as_mut().unwrap().templates = vec![
                serde_json::from_value(json!({
                    "name":"lens", "nodes":[leaves[0]], "exports":[exports[0]]
                }))
                .unwrap(),
            ];
            format!(
                "```fragment\n{}\n```",
                json!({"schema_version":1,"nodes":[],"patterns":
                    (0..4).map(|i| json!({"id":format!("lens{i}"),"pattern":"lens"})).collect::<Vec<_>>()
                })
            )
        };
        let mut owner = fixture_owner(&[&planner_result]);
        owner
            .observations
            .extend((0..4).map(|_| AttemptObservation::Pending { attempt_ref: None }));
        let mut journal = MemoryJournal::default();
        let pending = advance(&doc, &mut owner, &mut journal).unwrap();
        assert_eq!(pending.status, GraphStatus::Pending);
        assert_eq!(pending.checkpoint.expansions.len(), 1);
        assert_eq!(
            pending
                .checkpoint
                .nodes
                .iter()
                .find(|n| n.id == "after")
                .unwrap()
                .state,
            NodeState::Pending
        );
        let wire = serde_json::to_value(&pending.checkpoint).unwrap();
        {
            let nodes = wire["expansions"][0]["nodes"].as_array().unwrap();
            let boundary = nodes
                .iter()
                .position(|node| node.get("composite").is_some())
                .unwrap();
            let member = nodes
                .iter()
                .position(|node| node.get("composite").is_none())
                .unwrap();
            for (index, field, value) in [
                (boundary, "composite_members", json!(["plan--tampered"])),
                (boundary, "after_expansion", json!(["plan--tampered"])),
                (boundary, "composite_members", json!([])),
                (boundary, "after_expansion", json!([])),
                (member, "composite_members", json!(["plan--tampered"])),
                (member, "after_expansion", json!(["plan--tampered"])),
            ] {
                let mut tampered = wire.clone();
                let node = &mut tampered["expansions"][0]["nodes"][index];
                let id = node["id"].as_str().unwrap().to_owned();
                node[field] = value;
                journal.0 = Some(serde_json::from_value(tampered).unwrap());
                let error = advance(&doc, &mut fixture_owner(&[]), &mut journal).unwrap_err();
                assert_eq!(error.code(), "bad_checkpoint");
                assert!(error.to_string().contains(&id), "{error}");
            }
            let mut missing = wire.clone();
            let node = &mut missing["expansions"][0]["nodes"][boundary];
            let id = node["id"].as_str().unwrap().to_owned();
            node.as_object_mut().unwrap().remove("composite_members");
            journal.0 = Some(serde_json::from_value(missing).unwrap());
            let error = advance(&doc, &mut fixture_owner(&[]), &mut journal).unwrap_err();
            assert_eq!(error.code(), "bad_checkpoint");
            assert!(error.to_string().contains(&id), "{error}");
        }
        journal.0 = Some(serde_json::from_value(wire).unwrap());
        let valid = journal.0.clone().unwrap();
        let boundary = valid
            .nodes
            .iter()
            .position(|n| !n.children.is_empty())
            .unwrap();
        journal.0.as_mut().unwrap().nodes[boundary].children.clear();
        let error = advance(&doc, &mut fixture_owner(&[]), &mut journal).unwrap_err();
        assert_eq!(error.code(), "bad_checkpoint");
        assert!(
            error.to_string().contains(&valid.nodes[boundary].id),
            "{error}"
        );
        journal.0 = Some(valid);
        let mut owner = FakeOwner {
            observations: [
                terminal_with_result(AttemptOutcome::Succeeded, "review passed"),
                terminal(AttemptOutcome::OtherFailure),
                terminal(AttemptOutcome::UsageLimited),
                terminal(AttemptOutcome::UsageLimited),
                // A usage limit hands the slice to another agent until the
                // graph-wide restart budget is spent.
                terminal(AttemptOutcome::UsageLimited),
                terminal(AttemptOutcome::UsageLimited),
            ]
            .into(),
            ..Default::default()
        };
        let run = advance(&doc, &mut owner, &mut journal).unwrap();
        assert_eq!(run.status, GraphStatus::Complete, "fallback={fallback}");
        assert!(run.checkpoint.nodes.iter().all(|n| n.state.settled()));
        assert_eq!(
            run.checkpoint
                .nodes
                .iter()
                .find(|n| n.id == "after")
                .unwrap()
                .state,
            NodeState::Blocked
        );
        assert!(
            run.checkpoint
                .nodes
                .iter()
                .any(|n| n.advance_error_code.as_deref() == Some("usage_limited"))
        );
        journal.0 =
            Some(serde_json::from_slice(&serde_json::to_vec(&run.checkpoint).unwrap()).unwrap());
        assert_eq!(
            advance(&doc, &mut fixture_owner(&[]), &mut journal)
                .unwrap()
                .checkpoint,
            run.checkpoint
        );
    }
}

fn planner(max_nodes: usize) -> GraphDocument {
    serde_json::from_value(json!({
        "schema_version":1, "graph_id":"planned", "nodes":[
            {"id":"plan", "kind":"task", "access":"read_only", "prompt":"Slice the work",
             "route":{"alias":"senior"}, "plan":{"kinds":["task", "gate"], "max_nodes":max_nodes}},
            {"id":"after", "access":"read_only", "prompt":"Check all slices", "depends_on":["plan"], "route":{"alias":"senior"}}
        ]
    })).unwrap()
}

fn slice(id: &str, scope: &[&str]) -> Value {
    json!({"id":id, "prompt":"Do a slice", "write_scope":scope,
           "acceptance_checks":["Verify the slice"], "access":"read_only"})
}

fn result(nodes: Vec<Value>) -> String {
    format!(
        "```fragment\n{}\n```",
        json!({"schema_version":1,"nodes":nodes})
    )
}

fn pattern_definition() -> Value {
    json!({
        "name": "generator",
        "parameters": {
            "label": {"type": "string", "default": "the slice"},
            "scope": {"type": "list", "default": ["src"]},
            "worktree": {"type": "string", "default": ".worktrees/impl"},
            "retries": {"type": "integer", "default": 1, "min": 0, "max": 2}
        },
        "nodes": [
            {"id": "write", "prompt": "Do ${label}", "access": "worktree_write",
             "worktree": "${worktree}", "write_scope": "${scope}",
             "acceptance_checks": ["Verify the slice"]},
            {"id": "review", "prompt": "Review the work", "access": "read_only",
             "depends_on": ["write"], "write_scope": ["src"],
             "acceptance_checks": ["Verify the review"]}
        ],
        "exports": [{"node": "review", "outputs": ["result", "findings"]}]
    })
}

fn pattern_planner(max_nodes: usize) -> GraphDocument {
    serde_json::from_value(json!({
        "schema_version":1, "graph_id":"planned-patterns", "nodes":[
            {"id":"plan", "kind":"task", "access":"read_only", "prompt":"Slice the work",
             "route":{"alias":"senior"}, "plan":{"kinds":["task"], "max_nodes":max_nodes,
             "patterns":[pattern_definition()]}},
            {"id":"after", "access":"read_only", "prompt":"Check all slices",
             "depends_on":["plan"], "route":{"alias":"senior"}}
        ]
    }))
    .unwrap()
}

#[test]
fn failed_planner_runs_static_fallback_and_records_reason() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1, "graph_id":"planned-fallback", "nodes":[
            {"id":"before", "access":"read_only", "prompt":"Prepare inputs", "route":{"alias":"junior"}},
            {"id":"plan", "kind":"task", "access":"read_only", "prompt":"Plan", "require_skill_location":true,
             "route":{"alias":"senior"}, "depends_on":["before"],
             "inputs":[{"node":"before", "outputs":["result"]}],
             "plan":{"kinds":["task"], "max_nodes":2,
             "wire_exports":true,
             "fallback_depends_on":["before"],
             "fallback_inputs":[{"node":"before", "outputs":["result"]}],
             "fallback":{"nodes":[
                 {"id":"static", "access":"read_only", "prompt":"Static stage", "route":{"alias":"junior"}}
             ], "exports":[{"node":"static", "outputs":["result"]}]}}},
            {"id":"after", "access":"read_only", "prompt":"Consume stage", "route":{"alias":"senior"},
             "depends_on":["plan"], "inputs":[{"node":"plan", "outputs":["result"]}]}
        ]
    })).unwrap();
    let mut owner = FakeOwner {
        observations: [
            terminal_with_result(AttemptOutcome::Succeeded, "prepared result"),
            terminal_with_result(
                AttemptOutcome::TaskFailed,
                "TOKEN_SENTINEL /private/source \u{1b}",
            ),
            terminal_with_result(AttemptOutcome::Succeeded, "static result"),
            terminal_with_result(AttemptOutcome::Succeeded, "done"),
        ]
        .into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(run.checkpoint.successful());
    assert_eq!(owner.calls.len(), 4);
    assert!(owner.calls[2].0.node_id.contains("plan--fallback--static"));
    assert_eq!(owner.skill_requirements, [false, true, true, false]);
    assert!(owner.prompts[2].contains("prepared result"));
    assert!(owner.prompts[3].contains("static result"));
    let plan = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "plan")
        .unwrap();
    assert_eq!(plan.state, NodeState::Succeeded);
    assert_eq!(
        serde_json::to_value(plan).unwrap()["outputs"]["plan"]["fallback_reason"],
        "planner attempt failed: TaskFailed"
    );
    let summary = plan.deterministic_summary().unwrap();
    assert!(!summary.contains("TOKEN_SENTINEL"));
    assert!(!summary.contains("/private/source"));
    assert!(!summary.contains('\u{1b}'));
    assert_eq!(
        summary,
        include_str!("../fixtures/plan-fallback-output.txt")
    );
}

#[test]
fn invalid_successful_planner_fragment_runs_static_fallback_and_records_reason() {
    let document = two_step_fallback_document();
    let mut owner = fixture_owner(&[
        "```fragment\n{not json}\n```",
        "first static result",
        "second static result",
        "done",
    ]);
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(
        run.status,
        GraphStatus::Complete,
        "{:?}",
        run.checkpoint.pause_reason()
    );
    assert!(run.checkpoint.successful());
    assert_eq!(owner.calls.len(), 4);
    assert!(owner.calls[1].0.node_id.contains("plan--fallback--first"));
    let plan = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "plan")
        .unwrap();
    let reason = &plan
        .outputs
        .as_ref()
        .unwrap()
        .plan
        .as_ref()
        .unwrap()
        .fallback_reason;
    assert!(
        reason.contains("planner fragment invalid: invalid fragment"),
        "{reason}"
    );
    assert!(plan.deterministic_summary().unwrap().contains(reason));
}

#[test]
fn planner_fragment_accepts_string_shorthand_inputs_as_result_inputs() {
    let mut second = slice("second", &["src"]);
    second["depends_on"] = json!(["first"]);
    second["inputs"] = json!(["first"]);
    let output = result(vec![slice("first", &["src"]), second]);
    let mut owner = fixture_owner(&[&output, "first result", "second result", "done"]);
    let run = advance(&planner(2), &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(
        run.status,
        GraphStatus::Complete,
        "{:?}",
        run.checkpoint.pause_reason()
    );
    let second = run.checkpoint.expansions[0]
        .nodes
        .iter()
        .find(|node| node.id == "plan--second")
        .unwrap();
    assert_eq!(second.inputs[0].node, "plan--first");
    assert_eq!(second.inputs[0].outputs, vec![OutputKind::Result]);
}

fn two_step_fallback_document() -> GraphDocument {
    serde_json::from_value(json!({
        "schema_version":1, "graph_id":"planned-fallback-restart", "nodes":[
            {"id":"plan", "kind":"task", "access":"read_only", "prompt":"Plan",
             "route":{"alias":"senior"},
             "plan":{"kinds":["task"], "max_nodes":2, "wire_exports":true,
             "fallback":{"nodes":[
                 {"id":"first", "access":"read_only", "prompt":"First step", "route":{"alias":"junior"}},
                 {"id":"second", "access":"read_only", "prompt":"Second step", "route":{"alias":"junior"},
                  "depends_on":["first"], "inputs":[{"node":"first", "outputs":["result"]}]}
             ], "exports":[{"node":"second", "outputs":["result"]}]}}},
            {"id":"after", "access":"read_only", "prompt":"Consume stage", "route":{"alias":"senior"},
             "depends_on":["plan"], "inputs":[{"node":"plan", "outputs":["result"]}]}
        ]
    })).unwrap()
}

#[test]
fn restart_after_fallback_expansion_does_not_duplicate_work_or_admit_dependants_early() {
    let document = two_step_fallback_document();
    let mut journal = MemoryJournal::default();
    let mut first_owner = FakeOwner {
        observations: [
            terminal_with_result(AttemptOutcome::TaskFailed, "planner rejected the brief"),
            AttemptObservation::Pending { attempt_ref: None },
        ]
        .into(),
        ..Default::default()
    };
    let interrupted = advance(&document, &mut first_owner, &mut journal).unwrap();
    assert_eq!(interrupted.status, GraphStatus::Pending);
    assert_eq!(interrupted.checkpoint.expansions.len(), 1);
    let ids: Vec<_> = first_owner
        .calls
        .iter()
        .map(|call| call.0.node_id.as_str())
        .collect();
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert!(ids[1].ends_with("--fallback--first"), "{ids:?}");
    let state = |run: &GraphRun, suffix: &str| {
        run.checkpoint
            .nodes
            .iter()
            .find(|node| node.id.ends_with(suffix))
            .map(|node| node.state)
    };
    assert_eq!(
        state(&interrupted, "--fallback--second"),
        Some(NodeState::Pending)
    );
    assert_eq!(state(&interrupted, "after"), Some(NodeState::Pending));

    let mut second_owner = FakeOwner {
        observations: [
            terminal_with_result(AttemptOutcome::Succeeded, "first result"),
            terminal_with_result(AttemptOutcome::Succeeded, "second result"),
            terminal_with_result(AttemptOutcome::Succeeded, "done"),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(&document, &mut second_owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(run.checkpoint.successful());
    assert_eq!(run.checkpoint.expansions.len(), 1);
    let ids: Vec<_> = second_owner
        .calls
        .iter()
        .map(|call| call.0.node_id.as_str())
        .collect();
    assert_eq!(ids.len(), 3, "{ids:?}");
    assert!(!ids.contains(&"plan"), "{ids:?}");
    assert!(ids[0].ends_with("--fallback--first"), "{ids:?}");
    assert!(ids[1].ends_with("--fallback--second"), "{ids:?}");
    assert_eq!(ids[2], "after");
    assert!(second_owner.prompts[1].contains("first result"));
    assert!(second_owner.prompts[2].contains("second result"));
}

#[test]
fn restart_after_invalid_fragment_fallback_reuses_the_persisted_expansion() {
    let document = two_step_fallback_document();
    let mut journal = MemoryJournal::default();
    let mut first_owner = FakeOwner {
        observations: [
            terminal_with_result(AttemptOutcome::Succeeded, "```fragment\n{not json}\n```"),
            AttemptObservation::Pending { attempt_ref: None },
        ]
        .into(),
        ..Default::default()
    };
    let interrupted = advance(&document, &mut first_owner, &mut journal).unwrap();
    assert_eq!(interrupted.status, GraphStatus::Pending);
    assert_eq!(interrupted.checkpoint.expansions.len(), 1);
    assert!(
        interrupted
            .checkpoint
            .nodes
            .iter()
            .find(|node| node.id == "plan")
            .unwrap()
            .outputs
            .as_ref()
            .unwrap()
            .plan
            .as_ref()
            .unwrap()
            .fallback_reason
            .contains("invalid fragment")
    );

    let mut second_owner = FakeOwner {
        observations: [
            terminal_with_result(AttemptOutcome::Succeeded, "first result"),
            terminal_with_result(AttemptOutcome::Succeeded, "second result"),
            terminal_with_result(AttemptOutcome::Succeeded, "done"),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(&document, &mut second_owner, &mut journal).unwrap();
    assert_eq!(
        run.status,
        GraphStatus::Complete,
        "{:?}",
        run.checkpoint.pause_reason()
    );
    assert!(run.checkpoint.successful());
    assert_eq!(run.checkpoint.expansions.len(), 1);
    let ids: Vec<_> = second_owner
        .calls
        .iter()
        .map(|call| call.0.node_id.as_str())
        .collect();
    assert_eq!(
        ids,
        ["plan--fallback--first", "plan--fallback--second", "after"]
    );
}

#[test]
fn restart_rejects_a_fallback_expansion_that_differs_from_the_spec() {
    let document = two_step_fallback_document();
    let mut journal = MemoryJournal::default();
    let mut owner = FakeOwner {
        observations: [
            terminal_with_result(AttemptOutcome::TaskFailed, "planner rejected the brief"),
            AttemptObservation::Pending { attempt_ref: None },
        ]
        .into(),
        ..Default::default()
    };
    advance(&document, &mut owner, &mut journal).unwrap();
    let mut checkpoint = journal.0.clone().unwrap();
    checkpoint.expansions[0].nodes[0].prompt = "Tampered step".into();
    journal.0 = Some(checkpoint);
    let result = advance(&document, &mut fixture_owner(&[]), &mut journal);
    assert!(
        matches!(result, Err(GraphError::BadCheckpoint { .. })),
        "{:?}",
        result.map(|run| run.status)
    );
}

#[test]
fn fallback_expansion_over_a_structural_limit_pauses_with_a_reason() {
    let document = two_step_fallback_document();
    let limits = crate::config::GraphLimits {
        nodes: 3,
        ..Default::default()
    };
    let mut owner = fixture_owner(&["planner rejected the brief"]);
    let mut owner = {
        owner.observations = [terminal_with_result(
            AttemptOutcome::TaskFailed,
            "planner rejected the brief",
        )]
        .into();
        owner
    };
    let run = advance_with_budget(
        &document,
        &mut owner,
        &mut MemoryJournal::default(),
        &limits,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert_eq!(
        run.checkpoint.pause_cause(),
        Some(PauseCause::InvalidPlanExpansion)
    );
    let reason = run.checkpoint.pause_reason().unwrap();
    assert!(
        reason.contains("plan fallback expansion paused"),
        "{reason}"
    );
    assert!(run.checkpoint.expansions.is_empty());
    assert_eq!(owner.calls.len(), 1);
}

#[test]
fn missing_fragment_fallback_retries_after_raising_the_structural_limit() {
    let document = two_step_fallback_document();
    let limits = crate::config::GraphLimits {
        nodes: 3,
        ..Default::default()
    };
    let mut owner = FakeOwner {
        observations: [terminal_with_result(
            AttemptOutcome::Succeeded,
            "planner returned no fragment block",
        )]
        .into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let paused = advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &limits,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(paused.status, GraphStatus::Paused);
    let reason = paused.checkpoint.pause_reason().unwrap();
    assert!(
        reason.contains("planner fragment invalid: fragment block missing"),
        "{reason}"
    );
    assert!(
        reason.contains("fallback expansion paused: limits.nodes=3"),
        "{reason}"
    );
    assert!(reason.find("fragment block missing") < reason.find("fallback expansion paused"));
    assert!(
        paused
            .checkpoint
            .nodes
            .iter()
            .find(|node| node.id == "plan")
            .unwrap()
            .fragment
            .is_none()
    );
    assert!(paused.checkpoint.expansions.is_empty());
    let plan = paused
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "plan")
        .unwrap();
    assert!(matches!(
        &plan.expansion_cause,
        Some(NodeDecisionCause::PlanExpansionPaused {
            fragment_error: Some(PlanFailureFact::MissingBlock),
            ..
        })
    ));
    assert!(
        !plan
            .expansion_pause
            .as_deref()
            .unwrap()
            .contains("planner-fragment-retry-context")
    );
    let decoded: GraphCheckpoint =
        serde_json::from_slice(&serde_json::to_vec(&paused.checkpoint).unwrap()).unwrap();
    assert_eq!(decoded, paused.checkpoint);
    let mut forged = paused.checkpoint.clone();
    forged
        .nodes
        .iter_mut()
        .find(|node| node.id == "plan")
        .unwrap()
        .expansion_cause = None;
    let mut forged_journal = MemoryJournal(Some(forged));
    assert!(matches!(
        advance_with_budget(
            &document,
            &mut fixture_owner(&[]),
            &mut forged_journal,
            &limits,
            &Default::default()
        ),
        Err(GraphError::BadCheckpoint { .. })
    ));

    let mut owner = fixture_owner(&["first result", "second result", "done"]);
    let resumed = resume_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &Default::default(),
        &Default::default(),
        &[],
    )
    .unwrap();
    assert_eq!(
        resumed.status,
        GraphStatus::Complete,
        "{:?}",
        resumed.checkpoint.pause_reason()
    );
    assert!(resumed.checkpoint.successful());
    assert_eq!(resumed.checkpoint.expansions.len(), 1);
    let plan = resumed
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "plan")
        .unwrap();
    assert!(
        plan.outputs
            .as_ref()
            .unwrap()
            .plan
            .as_ref()
            .unwrap()
            .fallback_reason
            .contains("fragment block missing")
    );
    let ids: Vec<_> = owner
        .calls
        .iter()
        .map(|call| call.0.node_id.as_str())
        .collect();
    assert_eq!(
        ids,
        ["plan--fallback--first", "plan--fallback--second", "after"]
    );
}

#[test]
fn ordinary_advance_clears_pause_after_admitting_retried_fallback() {
    let document = two_step_fallback_document();
    let limits = crate::config::GraphLimits {
        nodes: 3,
        ..Default::default()
    };
    let mut owner = FakeOwner {
        observations: [terminal_with_result(
            AttemptOutcome::Succeeded,
            "planner returned no fragment block",
        )]
        .into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let paused = advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &limits,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(paused.status, GraphStatus::Paused);

    let mut owner = fixture_owner(&["first result", "second result", "done"]);
    let advanced = advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &Default::default(),
        &Default::default(),
    )
    .unwrap();
    assert_eq!(
        advanced.status,
        GraphStatus::Complete,
        "{:?}",
        advanced.checkpoint.pause_reason()
    );
    assert!(advanced.checkpoint.successful());
    assert_eq!(advanced.checkpoint.expansions.len(), 1);
}

#[test]
fn planner_prelaunch_failure_runs_static_fallback_without_an_attempt() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1, "graph_id":"planned-fallback-prelaunch", "nodes":[
            {"id":"before", "access":"read_only", "prompt":"Prepare", "route":{"alias":"junior"}},
            {"id":"plan", "kind":"task", "access":"read_only", "prompt":"Plan",
             "route":{"alias":"senior"}, "depends_on":["before"],
             "inputs":[{"node":"before", "outputs":["result"]}],
             "plan":{"kinds":["task"], "max_nodes":2,
             "wire_exports":true,
             "fallback":{"nodes":[
                 {"id":"static", "access":"read_only", "prompt":"Static stage", "route":{"alias":"junior"}}
             ], "exports":[{"node":"static", "outputs":["result"]}]}}},
            {"id":"after", "access":"read_only", "prompt":"Consume", "route":{"alias":"senior"},
             "depends_on":["plan"], "inputs":[{"node":"plan", "outputs":["result"]}]}
        ]
    })).unwrap();
    let mut document = document;
    document.nodes[1].prompt = "x".repeat(100);
    let mut owner = fixture_owner(&[&"u".repeat(3000), "static result", "done"]);
    let limits = crate::config::GraphLimits {
        node_prompt_bytes: 2000,
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
    assert_eq!(
        run.status,
        GraphStatus::Complete,
        "{:?}",
        run.checkpoint.pause_reason()
    );
    assert_eq!(owner.calls.len(), 3, "{:?}", run.checkpoint.nodes);
    assert!(owner.calls[1].0.node_id.contains("plan--fallback--static"));
    assert_eq!(owner.calls[2].0.node_id, "after");
    assert!(owner.prompts[2].contains("static result"));
    let plan = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "plan")
        .unwrap();
    assert!(plan.attempts.is_empty());
    assert_eq!(plan.state, NodeState::Succeeded);
    assert_eq!(
        plan.outputs
            .as_ref()
            .unwrap()
            .plan
            .as_ref()
            .unwrap()
            .fallback_reason,
        "planner attempt failed (unknown failure): node plan: selected upstream outputs do not fit; increase node_prompt_bytes and restart the graph"
    );
}

fn pattern_result(instance: Value) -> String {
    format!(
        "```fragment\n{}\n```",
        json!({"schema_version":1,"nodes":[],"patterns":[instance]})
    )
}

fn by_id(mut nodes: Vec<GraphNode>) -> Vec<GraphNode> {
    nodes.sort_by(|left, right| left.id.cmp(&right.id));
    nodes
}

fn fixture_owner(results: &[&str]) -> FakeOwner {
    FakeOwner {
        observations: results
            .iter()
            .map(|text| terminal_with_result(AttemptOutcome::Succeeded, text))
            .collect(),
        ..Default::default()
    }
}

const STOP: &str = "```handoff\n{\"schema_version\":2,\"status\":\"stopped_early\",\"remaining\":[],\"decisions\":[],\"verify\":[],\"note\":\"First half committed; finish the second half and run checks.\"}\n```";

#[test]
fn fenced_handoff_examples_do_not_control_continuations() {
    for fence in ["````", "~~~~"] {
        for padding in [0, 28_000] {
            let example = format!("{fence}markdown\n{}\n{STOP}\n{fence}", "x".repeat(padding));
            for genuine in [false, true] {
                let output = if genuine {
                    format!("{example}\n{STOP}")
                } else {
                    example.clone()
                };
                let mut doc = writer_graph();
                doc.nodes[0].required_verdict = None;
                let mut owner = fixture_owner(&[&output, "VERDICT: PASS", "checked"]);
                let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
                assert_eq!(run.status, GraphStatus::Complete);
                assert_eq!(run.checkpoint.expansions.len(), usize::from(genuine));
                // Only a genuine handoff reaches the in-place restart's brief.
                assert_eq!(
                    owner
                        .prompts
                        .get(1)
                        .is_some_and(|prompt| prompt.contains("First half committed")),
                    genuine
                );
            }
        }
    }
}

#[test]
fn refused_planner_fragment_retries_after_file_journal_reopen() {
    let directory = crate::test_support::tempdir().unwrap();
    let base = std::fs::canonicalize(directory.path()).unwrap();
    let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("checkout"));
    inputs.state_dir = Some(base.join("state"));
    std::fs::create_dir(base.join("state")).unwrap();
    std::fs::set_permissions(base.join("state"), std::fs::Permissions::from_mode(0o700)).unwrap();
    let root = StateRoot::resolve(&inputs, &SystemPathResolver).unwrap();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let project = ProjectId::from_bytes([7; 16]);
    let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
    for (field, cap) in [
        ("nodes", 3),
        ("edges", 3),
        ("fan_out_width", 2),
        ("expansion_depth", 1),
    ] {
        let mut doc = planner(2);
        doc.graph_id = format!("planned-{field}");
        let limits = serde_json::from_value(json!({field:cap})).unwrap();
        let mut first = slice("a", &["src"]);
        first["prompt"] = json!("detail ".repeat(700));
        let output = result(vec![first, slice("b", &["docs"])]);
        let mut owner = fixture_owner(&[&output]);
        {
            let mut journal =
                FileJournal::open(&root, project, &doc.graph_id, budget, &cancel, &clock).unwrap();
            journal
                .bind(GraphExecution {
                    extra: Default::default(),
                    bases: None,
                    directory: base.clone(),
                    document: doc.clone(),
                })
                .unwrap();
            let run =
                advance_with_budget(&doc, &mut owner, &mut journal, &limits, &Default::default())
                    .unwrap();
            assert_eq!(run.status, GraphStatus::Paused);
            assert!(
                run.checkpoint.nodes[0]
                    .outputs
                    .as_ref()
                    .unwrap()
                    .result
                    .as_ref()
                    .unwrap()
                    .truncated
            );
            assert!(run.checkpoint.nodes[0].fragment.as_ref().unwrap().len() > 4096);
        }
        owner.observations.extend(
            ["a", "b", "checked"].map(|text| terminal_with_result(AttemptOutcome::Succeeded, text)),
        );
        let mut journal =
            FileJournal::open(&root, project, &doc.graph_id, budget, &cancel, &clock).unwrap();
        let limits = serde_json::from_value(json!({field:cap * 2})).unwrap();
        let run = resume_with_budget(
            &doc,
            &mut owner,
            &mut journal,
            &limits,
            &Default::default(),
            &[],
        )
        .unwrap();
        assert_eq!(
            run.status,
            GraphStatus::Complete,
            "{field}: {:?}",
            run.checkpoint.pause_reason()
        );
        assert!(run.checkpoint.successful());
        assert_eq!(run.checkpoint.expansions.len(), 1);
        assert_eq!(owner.calls.len(), 4);
        assert_eq!(
            advance_with_budget(&doc, &mut owner, &mut journal, &limits, &Default::default())
                .unwrap()
                .checkpoint,
            run.checkpoint
        );
    }
}

#[test]
fn malformed_optional_handoff_warns_and_still_delivers() {
    for output in [
        "```handoff\n{}\n```".to_owned(),
        format!("{STOP}\n{STOP}"),
        STOP.replace("stopped_early", "TOKEN_SENTINEL/private"),
        STOP.trim_end_matches("\n```").to_owned(),
        format!(
            "```handoff\n{}\n```",
            json!({"schema_version":1,"status":"stopped_early","note":"x".repeat(4097)})
        ),
    ] {
        let mut doc = writer_graph();
        doc.nodes[0].required_verdict = None;
        doc.delivery = Some(provenant_records::run::DeliveryMode::Pr);
        doc.integration_branch = Some("integration".into());
        let mut owner = fixture_owner(&[&output, "checked"]);
        let mut journal = MemoryJournal::default();
        let run = advance(&doc, &mut owner, &mut journal).unwrap();
        assert_eq!(run.status, GraphStatus::Complete);
        assert!(run.checkpoint.successful());
        assert!(run.checkpoint.expansions.is_empty());
        let warnings = &run.checkpoint.nodes[0].outputs.as_ref().unwrap().warnings;
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("ignored optional handoff"))
        );
        assert!(warnings.iter().all(|warning| !warning.contains("TOKEN_SENTINEL") && !warning.contains("/private")));
        assert_eq!(owner.deliveries, 1);
        assert_eq!(
            advance(&doc, &mut owner, &mut journal).unwrap().checkpoint,
            run.checkpoint
        );
    }
}

#[test]
fn planner_prompt_uses_wire_kinds_and_required_field_names() {
    let document = planner(2);
    let node = &document.nodes[0];
    let prompt = plans::prompt(node);
    for kind in [
        NodeKind::Composite,
        NodeKind::Task,
        NodeKind::Synthesise,
        NodeKind::Gate,
        NodeKind::Loop,
        NodeKind::Decide,
        NodeKind::Land,
    ] {
        let wire = serde_json::to_value(kind).unwrap();
        assert!(prompt.contains(wire.as_str().unwrap()), "{prompt}");
    }
    for field in [
        "id",
        "goal",
        "access",
        "acceptance_checks",
        "depends_on",
        "worktree",
        "write_scope",
        "inputs",
    ] {
        assert!(prompt.contains(field), "missing {field}: {prompt}");
    }
    assert!(prompt.contains(r#"{"node":"id","outputs":["result"]}"#));
}

#[test]
fn planner_prompt_names_only_the_nodes_allow_listed_patterns() {
    let mut doc = pattern_planner(4);
    let parameters = &mut doc.nodes[0].plan.as_mut().unwrap().templates[0].parameters;
    parameters.get_mut("label").unwrap().default = None;
    parameters.get_mut("retries").unwrap().allowed = vec![json!(1), json!(2)];
    let prompt = plans::prompt(&doc.nodes[0]);
    assert!(prompt.contains("generator"));
    assert!(!prompt.contains("not-allow-listed-pattern"));
}

#[test]
fn planner_treats_write_scope_as_advisory_metadata() {
    let doc = planner(1);
    let reader =
        json!({"id":"read","prompt":"Read","access":"read_only","acceptance_checks":["check"]});
    let mut owner = fixture_owner(&[&result(vec![reader]), "read", "checked"]);
    let mut journal = MemoryJournal::default();
    let run = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(
        run.checkpoint.expansions.len(),
        1,
        "{:?}",
        run.checkpoint.pause_reason()
    );
    assert_eq!(run.status, GraphStatus::Complete);

    for scope in [None, Some(json!(["../outside"]))] {
        let mut writer = json!({"id":"write","prompt":"Write","access":"worktree_write","worktree":".worktrees/write","acceptance_checks":["check"]});
        if let Some(scope) = scope {
            writer["write_scope"] = scope;
        }
        let mut owner = fixture_owner(&[&result(vec![writer]), "writer", "checked"]);
        let mut journal = MemoryJournal::default();
        let run = advance(&doc, &mut owner, &mut journal).unwrap();
        assert_eq!(
            run.status,
            GraphStatus::Complete,
            "{:?}",
            run.checkpoint.pause_reason()
        );
        assert_eq!(run.checkpoint.expansions.len(), 1);
    }
}

#[test]
fn oversized_nested_fragment_id_names_the_source_and_child() {
    let mut doc = planner(1);
    doc.nodes[0].id = "p".repeat(100);
    doc.nodes.truncate(1);
    let name = "c".repeat(99);
    let run = advance(
        &doc,
        &mut fixture_owner(&[&result(vec![slice(&name, &["src"])])]),
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert_eq!(
        run.checkpoint.pause_cause(),
        Some(PauseCause::InvalidPlanExpansion)
    );
    assert!(run.checkpoint.expansions.is_empty());
}

#[test]
fn plan_rejects_invalid_and_over_bound_fragments_without_admitting_consumers() {
    let mut cyclic = slice("cycle", &["src"]);
    cyclic["depends_on"] = json!(["cycle"]);
    let mut undeclared = slice("unexpected", &["src"]);
    undeclared["kind"] = json!("synthesise");
    let mut no_checks = slice("unchecked", &["src"]);
    no_checks["acceptance_checks"] = json!([]);
    for (output, reason) in [
        (
            result(vec![slice("a", &["src"]), slice("b", &["docs"])]),
            "1..1 nodes",
        ),
        ("```fragment\n{not json}\n```".into(), "invalid fragment"),
        ("plain prose".into(), "fragment block missing"),
        (result(vec![cyclic]), "dependency"),
        (result(vec![undeclared]), "not declared"),
        (result(vec![no_checks]), "acceptance_checks"),
    ] {
        let mut owner = fixture_owner(&[&output]);
        let mut journal = MemoryJournal::default();
        let run = advance(&planner(1), &mut owner, &mut journal).unwrap();
        assert_eq!(run.status, GraphStatus::Paused, "{output}");
        assert_eq!(
            run.checkpoint.pause_cause(),
            Some(PauseCause::InvalidPlanExpansion),
            "{reason}"
        );
        assert!(run.checkpoint.expansions.is_empty());
        assert_eq!(owner.calls.len(), 1);
        assert_eq!(
            advance(&planner(1), &mut owner, &mut journal)
                .unwrap()
                .status,
            GraphStatus::Paused
        );
        assert_eq!(
            resume(&planner(1), &mut owner, &mut journal)
                .unwrap()
                .status,
            GraphStatus::Paused
        );
    }
}

#[test]
fn plan_validates_host_limits_before_saving() {
    let output = result(vec![slice("a", &["src"]), slice("b", &["docs"])]);
    for (field, cap) in [
        ("nodes", 3),
        ("edges", 3),
        ("fan_out_width", 2),
        ("expansion_depth", 1),
    ] {
        let mut doc = planner(2);
        doc.limits = Some(serde_json::from_value(json!({field: 999})).unwrap());
        let limits = serde_json::from_value(json!({field:cap})).unwrap();
        let mut owner = fixture_owner(&[&output]);
        let mut journal = MemoryJournal::default();
        let run = advance_with_budget(&doc, &mut owner, &mut journal, &limits, &Default::default())
            .unwrap();
        assert_eq!(run.status, GraphStatus::Paused);
        assert_eq!(
            run.checkpoint.pause_cause(),
            Some(PauseCause::InvalidPlanExpansion)
        );
        assert!(run.checkpoint.expansions.is_empty());
        assert_eq!(owner.calls.len(), 1);
    }
}

#[test]
fn allowed_pattern_instantiation_records_parameters_and_matches_a_static_composite() {
    let instance = json!({"id":"impl", "pattern":"generator",
        "parameters":{"label":"the slice", "scope":["src/a"]}});
    let mut owner = fixture_owner(&[
        &pattern_result(instance),
        "implemented",
        "reviewed",
        "checked",
    ]);
    let run = advance(
        &pattern_planner(4),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert_eq!(
        run.status,
        GraphStatus::Complete,
        "{:?}",
        run.checkpoint.pause_reason()
    );
    assert!(run.checkpoint.successful());

    let expansion = &run.checkpoint.expansions[0];
    assert_eq!(expansion.uses.len(), 1);
    let recorded = &expansion.uses[0];
    assert_eq!(recorded.id, "plan--impl");
    assert_eq!(recorded.name, "generator");
    assert_eq!(recorded.parameters["label"], "the slice");
    assert_eq!(recorded.parameters["scope"], json!(["src/a"]));
    assert_eq!(recorded.parameters["retries"], 1);
    let writer = expansion
        .nodes
        .iter()
        .find(|node| node.id == "plan--impl--write")
        .unwrap();
    assert_eq!(
        recorded.parameters["worktree"].as_str(),
        writer.worktree.as_deref()
    );
    assert_eq!(
        recorded.nodes,
        vec!["plan--impl--review", "plan--impl--write"]
    );

    // Composite templates retain their authored briefs and structured checks.
    let write_prompt = "Do the slice";
    let review_prompt = "Review the work";
    let mut static_doc = pattern_planner(4);
    static_doc.nodes.push(
        serde_json::from_value(json!({
            "id":"plan--impl", "kind":"composite", "access":"read_only", "depends_on":["plan"],
            "composite":{
                "nodes":[
                    {"id":"write", "prompt":write_prompt, "access":"worktree_write",
                     "worktree":".worktrees/impl", "write_scope":["src/a"],
                     "acceptance_checks":["Verify the slice"], "route":{"alias":"senior"}},
                    {"id":"review", "prompt":review_prompt, "access":"read_only",
                     "depends_on":["write"], "write_scope":["src"], "acceptance_checks":["Verify the review"],
                     "route":{"alias":"senior"}}
                ],
                "exports":[{"node":"review","outputs":["result","findings"]}]
            }
        }))
        .unwrap(),
    );
    let expanded = composites::expand(&static_doc, &plans::generous()).unwrap();
    let expected: Vec<_> = expanded
        .nodes
        .into_iter()
        .filter(|node| node.id.starts_with("plan--impl"))
        .map(|mut node| {
            // The planner admitted the fragment.
            patch::stamp_admitter(&mut node, "plan");
            node
        })
        .collect();
    assert_eq!(by_id(expansion.nodes.clone()), by_id(expected));
}

#[test]
fn plan_patterns_refuse_unlisted_invalid_and_widening_instances() {
    let cases = [
        (json!({"id":"impl", "pattern":"other"}), "allow-list"),
        (
            json!({"id":"impl", "pattern":"generator", "parameters":{"label":7}}),
            "label",
        ),
        (
            json!({"id":"impl", "pattern":"generator", "parameters":{"retries":9}}),
            "retries",
        ),
        (
            json!({"id":"impl", "pattern":"generator", "access":"worktree_write"}),
            "access",
        ),
        (
            json!({"id":"impl", "pattern":"generator", "read_roots":["/elsewhere"]}),
            "read_roots",
        ),
    ];
    for (instance, reason) in cases {
        let mut owner = fixture_owner(&[&pattern_result(instance)]);
        let run = advance(
            &pattern_planner(4),
            &mut owner,
            &mut MemoryJournal::default(),
        )
        .unwrap();
        assert_eq!(run.status, GraphStatus::Paused, "{reason}");
        assert_eq!(
            run.checkpoint.pause_cause(),
            Some(PauseCause::InvalidPlanExpansion),
            "{reason}"
        );
        assert!(run.checkpoint.expansions.is_empty());
        assert_eq!(owner.calls.len(), 1);
    }
}

#[test]
fn captured_fragment_text_does_not_become_a_pause_diagnostic() {
    let doc = pattern_planner(4);
    let instance = json!({"id":"impl", "pattern":"TOKEN_SENTINEL/private/host\u{1b}"});
    let mut owner = fixture_owner(&[&pattern_result(instance)]);
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    let reason = run.checkpoint.pause_reason().unwrap();
    assert!(reason.contains("invalid fragment"));
    assert!(!reason.contains("TOKEN_SENTINEL"));
    assert!(!reason.contains("/private/host"));
    assert!(!reason.contains('\u{1b}'));
    let node = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "plan")
        .unwrap();
    let cause = serde_json::to_string(&node.expansion_cause).unwrap();
    assert!(!cause.contains("TOKEN_SENTINEL"));
    assert!(!cause.contains("/private/host"));

    let document = two_step_fallback_document();
    let limits = crate::config::GraphLimits {
        nodes: 3,
        ..Default::default()
    };
    let mut owner = fixture_owner(&[&pattern_result(
        json!({"id":"impl", "pattern":"TOKEN_SENTINEL/private/host"}),
    )]);
    let paused = advance_with_budget(
        &document,
        &mut owner,
        &mut MemoryJournal::default(),
        &limits,
        &Default::default(),
    )
    .unwrap();
    let node = paused
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "plan")
        .unwrap();
    assert!(matches!(
        node.expansion_cause,
        Some(NodeDecisionCause::PlanExpansionPaused {
            fragment_error: Some(PlanFailureFact::InvalidFragment),
            ..
        })
    ));
    for text in [
        paused.checkpoint.pause_reason().unwrap(),
        node.expansion_pause.as_deref().unwrap(),
    ] {
        assert!(!text.contains("TOKEN_SENTINEL"));
        assert!(!text.contains("/private/host"));
    }
}

#[test]
fn refused_pattern_instantiation_raises_a_pause_decision() {
    let mut doc = pattern_planner(4);
    doc.decision_session = Some("chair-session".into());
    let instance = json!({"id":"impl", "pattern":"other"});
    let mut owner = fixture_owner(&[&pattern_result(instance)]);
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    let ask = &run.checkpoint.asks[0];
    assert_eq!((ask.ask_id.as_str(), ask.kind), ("pause-1", AskKind::Pause));
    assert_eq!(
        run.checkpoint.pause_cause(),
        Some(PauseCause::InvalidPlanExpansion)
    );
    assert!(run.checkpoint.expansions.is_empty());
}

#[test]
fn pattern_expansion_counts_against_the_graph_node_limit() {
    let instance = json!({"id":"impl", "pattern":"generator"});
    let limits = crate::config::GraphLimits {
        nodes: 3,
        ..Default::default()
    };
    let mut owner = fixture_owner(&[&pattern_result(instance)]);
    let run = advance_with_budget(
        &pattern_planner(4),
        &mut owner,
        &mut MemoryJournal::default(),
        &limits,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert_eq!(
        run.checkpoint.pause_cause(),
        Some(PauseCause::InvalidPlanExpansion)
    );
    assert!(run.checkpoint.expansions.is_empty());
}

fn assert_pattern_parameter_error(
    case: &str,
    doc: &GraphDocument,
    parameters: Value,
    reason: &str,
) {
    // The persisted pause reason is sanitised; check the specific validation
    // failure at the binding boundary before exercising the planner refusal.
    let error = doc.nodes[0].plan.as_ref().unwrap().templates[0]
        .bind("generator", &parameters)
        .unwrap_err()
        .to_string();
    assert!(error.contains(reason), "{case}: {error}");
    let mut owner = fixture_owner(&[&pattern_result(
        json!({"id":"impl", "pattern":"generator", "parameters":parameters}),
    )]);
    let run = advance(doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Paused, "{case}");
    assert_eq!(
        run.checkpoint.pause_cause(),
        Some(PauseCause::InvalidPlanExpansion),
        "{case}"
    );
    assert!(
        run.checkpoint
            .pause_reason()
            .unwrap_or_default()
            .contains("invalid fragment"),
        "{case}: {:?}",
        run.checkpoint.pause_reason()
    );
    assert!(run.checkpoint.expansions.is_empty(), "{case}");
}

#[test]
fn pattern_parameter_errors_are_refused() {
    let mut missing_required = pattern_planner(4);
    missing_required.nodes[0].plan.as_mut().unwrap().templates[0]
        .parameters
        .get_mut("label")
        .unwrap()
        .default = None;
    let mut enum_violation = pattern_planner(4);
    enum_violation.nodes[0].plan.as_mut().unwrap().templates[0]
        .parameters
        .get_mut("retries")
        .unwrap()
        .allowed = vec![json!(1)];

    for (case, doc, parameters, reason) in [
        (
            "missing required parameter",
            missing_required,
            json!({}),
            "requires parameter label",
        ),
        (
            "unknown parameter",
            pattern_planner(4),
            json!({"unknown":1}),
            "no parameter unknown",
        ),
        (
            "non-table parameters",
            pattern_planner(4),
            json!([]),
            "parameters must be a table",
        ),
        (
            "enum violation",
            enum_violation,
            json!({"retries":2}),
            "one of",
        ),
    ] {
        assert_pattern_parameter_error(case, &doc, parameters, reason);
    }
}

#[test]
fn pattern_template_authority_and_types_are_validated() {
    for (field, value) in [
        ("access", json!("${label}")),
        ("role", json!("${label}")),
        ("kind", json!("${label}")),
    ] {
        let mut doc = pattern_planner(4);
        doc.nodes[0].plan.as_mut().unwrap().templates[0].nodes[0][field] = value;
        assert!(plans::validate_spec(&doc.nodes[0]).is_err(), "{field}");
    }
}

#[test]
fn nested_pattern_cannot_parameterise_authority() {
    for nested in [
        json!({"id":"inner", "access":"${label}", "prompt":"review"}),
        json!({"id":"inner", "access":"read_only", "role":"${label}", "prompt":"review"}),
        json!({"id":"inner", "access":"read_only", "read_roots":["${label}"], "prompt":"review"}),
    ] {
        let mut doc = pattern_planner(4);
        let pattern = &mut doc.nodes[0].plan.as_mut().unwrap().templates[0];
        pattern.nodes[1] = json!({"id":"review", "kind":"composite", "access":"read_only",
            "composite":{"nodes":[nested],"exports":[{"node":"inner","outputs":["result"]}]}});
        assert!(plans::validate_spec(&doc.nodes[0]).is_err());
    }
}

#[test]
fn nested_pattern_writer_keeps_its_scope_requirement() {
    let mut doc = pattern_planner(4);
    let pattern = &mut doc.nodes[0].plan.as_mut().unwrap().templates[0];
    pattern.nodes[1] = json!({"id":"review", "kind":"composite", "access":"read_only",
        "depends_on":["write"], "composite":{"nodes":[{"id":"inner", "access":"worktree_write", "prompt":"write", "worktree":"/tmp/w", "write_scope":[], "acceptance_checks":["check"], "route":{"alias":"senior"}}],"exports":[{"node":"inner","outputs":["result"]}]}});
    let instance = json!({"id":"impl","pattern":"generator"});
    let mut owner = fixture_owner(&[&pattern_result(instance)]);
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(
        run.checkpoint.pause_cause(),
        Some(PauseCause::InvalidPlanExpansion)
    );
}

#[test]
fn pattern_required_sample_does_not_violate_bounds_at_admission() {
    let mut doc = pattern_planner(4);
    let parameters = &mut doc.nodes[0].plan.as_mut().unwrap().templates[0].parameters;
    let retries = parameters.get_mut("retries").unwrap();
    retries.default = None;
    retries.min = Some(json!(5));
    retries.max = Some(json!(10));
    assert!(plans::validate_spec(&doc.nodes[0]).is_ok());
}

#[test]
fn pattern_expansion_respects_plan_max_nodes_boundary() {
    let output = pattern_result(json!({"id":"impl", "pattern":"generator"}));
    let mut owner = fixture_owner(&[&output, "implemented", "reviewed", "checked"]);
    let run = advance(
        &pattern_planner(1),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert_eq!(
        run.status,
        GraphStatus::Complete,
        "{:?}",
        run.checkpoint.pause_reason()
    );
    assert_eq!(run.checkpoint.expansions[0].nodes.len(), 3);

    let output = pattern_result_all(vec![
        json!({"id":"a", "pattern":"generator"}),
        json!({"id":"b", "pattern":"generator"}),
    ]);
    let run = advance(
        &pattern_planner(1),
        &mut fixture_owner(&[&output]),
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert_eq!(
        run.checkpoint.pause_cause(),
        Some(PauseCause::InvalidPlanExpansion)
    );
    assert!(run.checkpoint.expansions.is_empty());

    let output = pattern_result(json!({"id":"impl", "pattern":"generator"}));
    let limits = crate::config::GraphLimits {
        nodes: 4,
        ..Default::default()
    };
    let run = advance_with_budget(
        &pattern_planner(1),
        &mut fixture_owner(&[&output]),
        &mut MemoryJournal::default(),
        &limits,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert_eq!(
        run.checkpoint.pause_cause(),
        Some(PauseCause::InvalidPlanExpansion)
    );
    assert!(
        run.checkpoint
            .pause_reason()
            .unwrap()
            .contains("limits.nodes")
    );
    assert!(run.checkpoint.expansions.is_empty());
}

#[test]
fn planner_integrate_delivery_needs_the_documents_integration_branch() {
    let mut writer = slice("w", &["src"]);
    writer["access"] = json!("worktree_write");
    writer["worktree"] = json!(".worktrees/w");
    writer["delivery"] = json!("integrate:origin/release/1.3");
    let output = result(vec![writer]);
    let run = advance(
        &planner(1),
        &mut fixture_owner(&[&output]),
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert_eq!(
        run.checkpoint.pause_cause(),
        Some(PauseCause::InvalidPlanExpansion)
    );
    assert!(run.checkpoint.expansions.is_empty());

    // With one, the destination comes from the document, never the planner.
    let mut doc = planner(1);
    doc.integration_branch = Some("integration".into());
    let mut owner = fixture_owner(&[&output]);
    owner.observations.push_back(AttemptObservation::Pending {
        attempt_ref: Some("w".into()),
    });
    advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(
        owner.delivery_modes[1],
        Some(provenant_records::run::DeliveryMode::Integrate(
            "integration".into()
        ))
    );
}

#[test]
fn plan_disjoint_writers_are_admitted_while_a_peer_is_running() {
    let mut doc = planner(2);
    doc.integration_branch = Some("integration".into());
    let nodes = ["a", "b"]
        .into_iter()
        .map(|id| {
            let mut node = slice(id, &[&format!("src/{id}")]);
            node["access"] = json!("worktree_write");
            node["worktree"] = json!(format!(".worktrees/{id}"));
            node
        })
        .collect();
    let output = result(nodes);
    let mut owner = fixture_owner(&[&output]);
    owner.observations.extend([
        AttemptObservation::Pending {
            attempt_ref: Some("a".into()),
        },
        AttemptObservation::Pending {
            attempt_ref: Some("b".into()),
        },
    ]);
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Pending);
    assert_eq!(owner.calls.len(), 3);
    assert_ne!(owner.permissions[1].1, owner.permissions[2].1);
    assert_eq!(
        run.checkpoint
            .nodes
            .iter()
            .filter(|n| n.state == NodeState::Running)
            .count(),
        2
    );
}

fn writer_graph() -> GraphDocument {
    let mut writer = node("write", pool_route("senior"));
    writer.access = AccessMode::WorktreeWrite;
    writer.worktree = Some(".worktrees/writer".into());
    writer.required_verdict = Some("VERDICT: PASS".into());
    let mut after = node("after", route("fixture"));
    after.depends_on = vec!["write".into()];
    graph(vec![writer, after])
}

#[test]
fn honest_stop_restarts_in_place_on_the_same_checkout_with_handoff_and_verdict_contract() {
    let mut doc = writer_graph();
    doc.nodes[1].inputs = vec![GraphInput {
        required_outputs: Vec::new(),
        extra: Default::default(),
        node: "write".into(),
        outputs: vec![OutputKind::Result],
    }];
    let mut owner = fixture_owner(&[STOP, "VERDICT: PASS", "checked"]);
    let mut journal = MemoryJournal::default();
    let run = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert_eq!(run.checkpoint.expansions.len(), 1);
    assert_eq!(owner.permissions[0], owner.permissions[1]);
    assert_eq!(owner.calls[1].0.node_id, "write");
    assert_eq!(owner.calls[2].0.node_id, "after");
    assert!(owner.prompts[2].contains("write result:"));
    assert!(owner.prompts[1].contains("First half committed"));
    assert_eq!(run.checkpoint.remaining.as_ref().unwrap().continuations, 1);
    assert_eq!(
        advance(&doc, &mut owner, &mut journal).unwrap().checkpoint,
        run.checkpoint
    );
    assert_eq!(owner.calls.len(), 3);
}

#[test]
fn continuation_budget_pauses_then_extends_without_duplicating_prior_work() {
    let doc = writer_graph();
    let mut limits = crate::config::GraphLimits {
        continuations: 1,
        ..Default::default()
    };
    let mut owner = fixture_owner(&[STOP, STOP]);
    let mut journal = MemoryJournal::default();
    let run =
        advance_with_budget(&doc, &mut owner, &mut journal, &limits, &Default::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert!(!run.checkpoint.successful());
    assert_eq!(run.checkpoint.expansions.len(), 1);
    assert_eq!(run.checkpoint.remaining.as_ref().unwrap().continuations, 0);
    assert_eq!(
        run.checkpoint.pause_cause(),
        Some(PauseCause::InvalidPlanExpansion)
    );
    assert_eq!(owner.calls.len(), 2);
    assert_eq!(
        advance_with_budget(&doc, &mut owner, &mut journal, &limits, &Default::default())
            .unwrap()
            .status,
        GraphStatus::Paused
    );
    owner.observations.extend(
        ["VERDICT: PASS", "checked"]
            .map(|text| terminal_with_result(AttemptOutcome::Succeeded, text)),
    );
    limits.continuations = 2;
    let resumed = resume_with_budget(
        &doc,
        &mut owner,
        &mut journal,
        &limits,
        &Default::default(),
        &[],
    )
    .unwrap();
    assert_eq!(resumed.status, GraphStatus::Complete);
    assert_eq!(resumed.checkpoint.expansions.len(), 2);
    assert_eq!(owner.calls.len(), 4);
    limits.continuations = 0;
    let mut owner = super::FakeOwner {
        observations: [terminal_with_result(AttemptOutcome::Succeeded, STOP)].into(),
        ..Default::default()
    };
    let disabled = advance_with_budget(
        &doc,
        &mut owner,
        &mut MemoryJournal::default(),
        &limits,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(disabled.status, GraphStatus::Paused);
    assert!(disabled.checkpoint.expansions.is_empty());
    assert_eq!(owner.calls.len(), 1);
}

#[test]
fn honest_stop_is_structured_and_continuation_keeps_the_completion_contract() {
    let doc = writer_graph();
    let mut owner = fixture_owner(&[STOP, "unfinished"]);
    let mut journal = MemoryJournal::default();
    let run = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(!run.checkpoint.successful());
    assert_eq!(run.checkpoint.expansions.len(), 1);
    assert_eq!(owner.calls.len(), 2);
    let mut prose = fixture_owner(&["I stopped early"]);
    let run = advance(&doc, &mut prose, &mut MemoryJournal::default()).unwrap();
    assert!(!run.checkpoint.successful());
    assert!(run.checkpoint.expansions.is_empty());
}

#[test]
fn plan_inside_loop_keeps_round_membership_after_checkpoint_serialisation() {
    let doc: GraphDocument = serde_json::from_value(json!({
        "schema_version":1,"graph_id":"loop-plan","nodes":[
            {"id":"iteration","kind":"loop","access":"read_only","loop":{
                "max_rounds":1,"until":"check","body":[
                    {"id":"plan","kind":"task","prompt":"Plan","access":"read_only","route":{"alias":"senior"},"plan":{"kinds":["task"],"max_nodes":1}},
                    {"id":"check","kind":"gate","access":"read_only","depends_on":["plan"],"condition":{"type":"budget"}}
                ]
            }}
        ]
    })).unwrap();
    let output = result(vec![slice("slice", &["src"])]);
    let mut owner = fixture_owner(&[&output, "done"]);
    let mut journal = MemoryJournal::default();
    let run = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(run.checkpoint.successful());
    assert!(
        run.checkpoint
            .nodes
            .iter()
            .find(|n| n.id.ends_with("--slice"))
            .unwrap()
            .round
            .is_some()
    );
    journal.0 =
        Some(serde_json::from_slice(&serde_json::to_vec(&run.checkpoint).unwrap()).unwrap());
    assert_eq!(
        advance(&doc, &mut owner, &mut journal).unwrap().checkpoint,
        run.checkpoint
    );
}

#[test]
fn planner_cannot_add_separate_writer_checkouts_to_one_loop() {
    let mut doc = planner(2);
    let plan = serde_json::to_value(&doc.nodes[0]).unwrap();
    doc.nodes = serde_json::from_value(json!([{
        "id":"iteration","kind":"loop","access":"read_only","loop":{
            "max_rounds":1,"until":"check","body":[plan,
                {"id":"check","kind":"gate","access":"read_only","depends_on":["plan"],"condition":{"type":"budget"}}
            ]
        }
    }])).unwrap();
    doc.integration_branch = Some("integration".into());
    let nodes = ["a", "b"]
        .into_iter()
        .map(|id| {
            let mut child = slice(id, &[&format!("src/{id}")]);
            child["access"] = json!("worktree_write");
            child["worktree"] = json!(format!(".worktrees/{id}"));
            child
        })
        .collect();
    let run = advance(
        &doc,
        &mut fixture_owner(&[&result(nodes)]),
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert_eq!(
        run.checkpoint.pause_cause(),
        Some(PauseCause::InvalidPlanExpansion)
    );
    assert!(run.checkpoint.expansions.is_empty());
}

#[test]
fn nested_plan_rewires_all_consumers_and_keeps_original_digest() {
    let doc = planner(1);
    let mut nested = slice("nested", &["src"]);
    nested["kind"] = json!("task");
    nested["plan"] = json!({"kinds":["task"],"max_nodes":1});
    let output = result(vec![nested]);
    let inner = result(vec![slice("inner", &["src"])]);
    let mut owner = fixture_owner(&[&output, &inner, "done", "checked"]);
    let mut journal = MemoryJournal::default();
    let run = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    let outer = run
        .checkpoint
        .expansions
        .iter()
        .find(|expansion| expansion.source == "plan")
        .unwrap();
    let nested = outer
        .nodes
        .iter()
        .find(|node| node.id == "plan--nested")
        .unwrap();
    assert!(nested.plan.is_some());
    let inner_expansion = run
        .checkpoint
        .expansions
        .iter()
        .find(|expansion| expansion.source == nested.id)
        .unwrap();
    assert!(
        inner_expansion
            .nodes
            .iter()
            .any(|node| node.id == "plan--nested--inner")
    );

    // Each fragment is journalled against its planner and identifies that
    // planner as the admitter of its additions.
    for expansion in &run.checkpoint.expansions {
        let record = expansion.patch.as_ref().unwrap();
        assert_eq!(record.parent.as_deref(), Some(expansion.source.as_str()));
        assert!(record.revision > 0);
        assert!(record.digest.starts_with("sha256:"));
        for node in &expansion.nodes {
            assert_eq!(
                node.attempt
                    .as_ref()
                    .and_then(|spec| spec.admitted_by.as_deref()),
                Some(expansion.source.as_str())
            );
        }
    }

    // A consumer of the planner result waits for its nested additions.
    let launch_position = |id: &str| {
        owner
            .calls
            .iter()
            .position(|(key, _)| key.node_id == id)
            .unwrap()
    };
    assert!(launch_position("after") > launch_position("plan--nested--inner"));
    for id in ["plan", "plan--nested", "plan--nested--inner", "after"] {
        assert!(
            run.checkpoint
                .nodes
                .iter()
                .any(|node| node.id == id && node.state == NodeState::Succeeded)
        );
    }
    let (base, _) = validate_plan(&doc).unwrap();
    assert_eq!(
        run.checkpoint.plan_digest,
        CanonicalV1::record_digest("fabric-graph-plan", 1, &base)
            .unwrap()
            .to_string()
    );
}

#[test]
fn expansion_completes_at_node_budget() {
    let limits = crate::config::GraphLimits {
        nodes: 4,
        ..Default::default()
    };
    let count = limits.nodes / 2;
    let nodes = (0..count)
        .map(|index| {
            let mut node = planner(1).nodes.remove(0);
            node.id = format!("plan{index}");
            node
        })
        .collect();
    let doc = graph(nodes);
    let output = result(vec![slice("child", &["src"])]);
    let mut owner = fixture_owner(&vec![output.as_str(); count]);
    owner
        .observations
        .extend((0..count).map(|_| terminal_with_result(AttemptOutcome::Succeeded, "done")));
    let mut journal = MemoryJournal::default();
    let run =
        advance_with_budget(&doc, &mut owner, &mut journal, &limits, &Default::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert_eq!(run.checkpoint.nodes.len(), limits.nodes);
    assert_eq!(run.checkpoint.expansions.len(), count);
}

#[test]
fn invalid_planner_result_remains_paused_when_every_attempt_has_settled() {
    let doc = graph(vec![planner(1).nodes.remove(0)]);
    let mut owner = fixture_owner(&["not a fragment"]);
    let mut journal = MemoryJournal::default();
    let run = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert!(!run.checkpoint.successful());
    assert!(run.checkpoint.nodes.iter().all(|n| n.state.settled()));
    assert_eq!(
        advance(&doc, &mut owner, &mut journal).unwrap().status,
        GraphStatus::Paused
    );
}

#[test]
fn structured_blocks_survive_real_result_head_truncation() {
    for repetitions in [800, 4000] {
        for (doc, block, completion) in [
            (planner(1), result(vec![slice("slice", &["src"])]), "done"),
            (writer_graph(), STOP.into(), "VERDICT: PASS"),
        ] {
            let output = format!("{}\n{block}", "detail ".repeat(repetitions));
            let mut owner = fixture_owner(&[&output, completion, "checked"]);
            if let Some(AttemptObservation::Terminal {
                result: Some(result),
                ..
            }) = owner.observations.front_mut()
            {
                result.source_truncated = true;
            }
            let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
            assert_eq!(run.status, GraphStatus::Complete);
            assert!(run.checkpoint.successful());
            assert_eq!(owner.calls.len(), 3);
        }
    }
}

#[test]
fn fragment_gate_recovery_is_a_completion_dependency_for_consumers() {
    for expected in ["VERDICT: PASS", "VERDICT: FAIL"] {
        let output = result(vec![
            slice("review", &["src"]),
            json!({"id":"gate","kind":"gate","access":"read_only","depends_on":["review"],"inputs":[{"node":"review","outputs":["verdict"]}],"condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"},"on_fail":"repair"}),
            {
                let mut repair = slice("repair", &["src"]);
                repair["depends_on"] = json!(["gate"]);
                repair
            },
        ]);
        let texts = if expected == "VERDICT: PASS" {
            vec![output.as_str(), expected, "checked"]
        } else {
            vec![output.as_str(), expected, "repaired", "checked"]
        };
        let mut owner = fixture_owner(&texts);
        let mut journal = MemoryJournal::default();
        if expected == "VERDICT: FAIL" {
            owner.observations.truncate(2);
            owner.observations.push_back(AttemptObservation::Pending {
                attempt_ref: Some("repair".into()),
            });
            let pending = advance(&planner(3), &mut owner, &mut journal).unwrap();
            assert_eq!(pending.status, GraphStatus::Pending);
            assert_eq!(
                pending
                    .checkpoint
                    .nodes
                    .iter()
                    .find(|n| n.id == "after")
                    .unwrap()
                    .state,
                NodeState::Pending
            );
            owner.observations.extend(
                ["repaired", "checked"]
                    .map(|text| terminal_with_result(AttemptOutcome::Succeeded, text)),
            );
        }
        let run = advance(&planner(3), &mut owner, &mut journal).unwrap();
        assert_eq!(run.status, GraphStatus::Complete);
        assert!(run.checkpoint.successful());
        assert_eq!(owner.calls.last().unwrap().0.node_id, "after");
    }
}

#[test]
fn an_inactive_recovery_writer_does_not_skip_an_overlapping_required_writer() {
    for (sibling, explicit_dependency) in [(true, false), (false, false), (false, true)] {
        let mut first = planner(4).nodes.remove(0);
        first.id = "first".into();
        let mut second = first.clone();
        second.id = "second".into();
        let mut doc = graph(if sibling {
            vec![first, second]
        } else {
            vec![first]
        });
        doc.integration_branch = Some("integration".into());
        let mut repair = slice("repair", &["src"]);
        repair["access"] = json!("worktree_write");
        repair["worktree"] = json!(".worktrees/a");
        repair["depends_on"] = json!(["gate"]);
        let mut nodes = vec![
            slice("review", &["src"]),
            json!({"id":"gate","kind":"gate","access":"read_only","depends_on":["review"],"inputs":[{"node":"review","outputs":["verdict"]}],"condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"},"on_fail":"repair"}),
            repair,
        ];
        let mut required = slice("required", &["src"]);
        required["access"] = json!("worktree_write");
        required["worktree"] = json!(".worktrees/b");
        if explicit_dependency {
            required["depends_on"] = json!(["repair"]);
        }
        if !sibling {
            nodes.push(required.clone());
        }
        let first = result(nodes);
        let second = result(vec![required]);
        let texts = if sibling {
            vec![first.as_str(), second.as_str(), "VERDICT: PASS", "done"]
        } else {
            vec![first.as_str(), "VERDICT: PASS", "done"]
        };
        let mut owner = fixture_owner(&texts);
        let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
        assert!(run.checkpoint.successful());
        let id = if sibling {
            "second--required"
        } else {
            "first--required"
        };
        assert_eq!(
            run.checkpoint
                .nodes
                .iter()
                .find(|n| n.id == id)
                .unwrap()
                .state,
            if explicit_dependency {
                NodeState::Skipped
            } else {
                NodeState::Succeeded
            }
        );
        assert_eq!(
            owner.calls.len(),
            if sibling {
                4
            } else if explicit_dependency {
                2
            } else {
                3
            }
        );
    }
}

#[test]
fn fragment_cannot_widen_read_roots_source_or_role() {
    let mut wide = slice("wide", &["src"]);
    wide["read_roots"] = json!(["/elsewhere"]);
    let mut roled = slice("roled", &["src"]);
    roled["role"] = json!("admin");
    let mut sourced = slice("sourced", &["src"]);
    sourced["source"] = json!({"type":"integration"});
    for (node, reason) in [(wide, "read root"), (roled, "role"), (sourced, "source")] {
        let mut doc = planner(1);
        doc.nodes[0].read_roots = vec!["/granted".into()];
        let mut owner = fixture_owner(&[&result(vec![node])]);
        let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
        assert_eq!(run.status, GraphStatus::Paused);
        assert_eq!(
            run.checkpoint.pause_cause(),
            Some(PauseCause::InvalidPlanExpansion),
            "{reason}"
        );
        assert!(run.checkpoint.expansions.is_empty());
    }
    let mut same = slice("same", &["src"]);
    same["read_roots"] = json!(["/granted"]);
    let mut doc = planner(1);
    doc.nodes[0].read_roots = vec!["/granted".into()];
    let run = advance(
        &doc,
        &mut fixture_owner(&[&result(vec![same]), "done", "done"]),
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert_eq!(run.checkpoint.expansions.len(), 1);
}

#[test]
fn planner_fragment_cannot_invent_capabilities() {
    let mut requested = slice("requested", &["src"]);
    requested["capabilities"] = json!(["postgres"]);
    let mut doc = planner(1);
    let run = advance(
        &doc,
        &mut fixture_owner(&[&result(vec![requested.clone()])]),
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert_eq!(
        run.checkpoint.pause_cause(),
        Some(PauseCause::InvalidPlanExpansion)
    );
    doc.nodes[0].capabilities = Some(vec!["postgres".into()]);
    let run = advance(
        &doc,
        &mut fixture_owner(&[&result(vec![requested]), "done", "done"]),
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert_eq!(run.checkpoint.expansions.len(), 1);
}

fn two_instances(first: Value, second: Value) -> String {
    format!(
        "```fragment\n{}\n```",
        json!({"schema_version":1,"nodes":[],"patterns":[first, second]})
    )
}

#[test]
fn disjoint_waves_use_worktree_identity_and_ignore_relative_scope_overlap() {
    let mut doc = pattern_planner(8);
    doc.integration_branch = Some("integration".into());
    doc.nodes[0].plan.as_mut().unwrap().disjoint_waves = true;
    let one = json!({"id":"one", "pattern":"generator", "parameters":{"scope":["src/a"], "worktree":".worktrees/one"}});
    let clash = json!({"id":"two", "pattern":"generator", "parameters":{"scope":["src/a/b"], "worktree":".worktrees/two"}});
    let results = [
        "implemented",
        "reviewed",
        "implemented",
        "reviewed",
        "checked",
    ];
    let fragment = two_instances(one.clone(), clash.clone());
    let mut owner = fixture_owner(&[&[fragment.as_str()][..], &results[..]].concat());
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(
        run.status,
        GraphStatus::Complete,
        "{:?}",
        run.checkpoint.pause_reason()
    );
    assert_eq!(run.checkpoint.expansions.len(), 1);

    let mut same_worktree = clash.clone();
    same_worktree["parameters"]["worktree"] = json!(".worktrees/one");
    let mut owner = fixture_owner(&[&two_instances(one.clone(), same_worktree)]);
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert_eq!(
        run.checkpoint.pause_cause(),
        Some(PauseCause::InvalidPlanExpansion)
    );
    assert!(run.checkpoint.expansions.is_empty());
    assert_eq!(owner.calls.len(), 1);

    let disjoint = json!({"id":"two", "pattern":"generator", "parameters":{"scope":["src/b"], "worktree":".worktrees/two"}});
    let mut ordered = clash.clone();
    ordered["parameters"]["worktree"] = json!(".worktrees/one");
    ordered["depends_on"] = json!(["one"]);
    for second in [disjoint, ordered] {
        let results = [
            "implemented",
            "reviewed",
            "implemented",
            "reviewed",
            "checked",
        ];
        let mut owner = fixture_owner(
            &[
                &[two_instances(one.clone(), second).as_str()][..],
                &results[..],
            ]
            .concat(),
        );
        let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
        assert_eq!(
            run.status,
            GraphStatus::Complete,
            "{:?}",
            run.checkpoint.pause_reason()
        );
        assert_eq!(run.checkpoint.expansions[0].uses.len(), 2);
    }

    // Without the wave switch, relative overlap still cannot connect trees.
    let mut plain = pattern_planner(8);
    plain.integration_branch = Some("integration".into());
    plain.nodes[0].plan.as_mut().unwrap().disjoint_waves = false;
    let results = [
        "implemented",
        "reviewed",
        "implemented",
        "reviewed",
        "checked",
    ];
    let mut owner =
        fixture_owner(&[&[two_instances(one, clash).as_str()][..], &results[..]].concat());
    let run = advance(&plain, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(
        run.status,
        GraphStatus::Complete,
        "{:?}",
        run.checkpoint.pause_reason()
    );
}

#[test]
fn planner_adds_lenses_omitted_by_a_path_floor() {
    let mut doc = planner(4);
    let spec = doc.nodes[0].plan.as_mut().unwrap();
    for name in ["lens-tests", "lens-security-privacy"] {
        spec.templates.push(serde_json::from_value(json!({
            "name": name,
            "nodes": [{"id":"lens", "access":"read_only", "prompt":"Review", "route":{"alias":"senior"}, "acceptance_checks":["Review"]}],
            "exports":[{"node":"lens", "outputs":["result"]}]
        })).unwrap());
    }
    spec.extra.insert(
        "required_floor_patterns".into(),
        json!(["lens-security-privacy"]),
    );
    spec.extra
        .insert("floor_classes".into(), json!(["security"]));
    spec.extra.insert(
        "floor_candidate_patterns".into(),
        json!(["lens-tests", "lens-security-privacy"]),
    );
    spec.extra.insert("floor_min_reviewers".into(), json!(2));
    spec.extra.insert("model_max_nodes".into(), json!(3));
    let output = pattern_result(json!({"id":"tests", "pattern":"lens-tests"}));
    let mut owner = fixture_owner(&[&output, "reviewed", "reviewed", "checked"]);
    let mut journal = MemoryJournal::default();
    let run = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(
        run.status,
        GraphStatus::Complete,
        "{:?}",
        run.checkpoint.pause_reason()
    );
    let expansion = &run.checkpoint.expansions[0];
    assert!(
        expansion
            .uses
            .iter()
            .any(|item| item.name == "lens-security-privacy")
    );
    assert!(
        expansion.extra["floor_warnings"]
            .to_string()
            .contains("security")
    );
    let mut tampered = serde_json::to_value(journal.0.as_ref().unwrap()).unwrap();
    tampered["expansions"][0]["patterns"]
        .as_array_mut()
        .unwrap()
        .retain(|pattern| pattern["name"] != "lens-security-privacy");
    let mut bad_journal = MemoryJournal(Some(serde_json::from_value(tampered).unwrap()));
    let error = advance(&doc, &mut fixture_owner(&[]), &mut bad_journal).unwrap_err();
    assert_eq!(error.code(), "bad_checkpoint");
    let mut replay = fixture_owner(&[]);
    let restored = advance(&doc, &mut replay, &mut journal).unwrap();
    assert_eq!(restored.status, GraphStatus::Complete);
    assert!(replay.calls.is_empty());
    let output = pattern_result(json!({"id":"security", "pattern":"lens-security-privacy"}));
    let mut owner = fixture_owner(&[&output, "reviewed", "reviewed", "checked"]);
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert!(
        run.checkpoint.expansions[0]
            .uses
            .iter()
            .any(|item| item.name == "lens-tests")
    );
}

#[test]
fn pattern_routes_may_vary_only_over_a_closed_list() {
    let mut doc = pattern_planner(4);
    {
        let pattern = &mut doc.nodes[0].plan.as_mut().unwrap().templates[0];
        pattern.nodes[0]["route"] = json!({"alias":"${label}"});
    }
    assert!(
        plans::validate_spec(&doc.nodes[0]).is_err(),
        "free text names any route"
    );
    {
        let parameter = doc.nodes[0].plan.as_mut().unwrap().templates[0]
            .parameters
            .get_mut("label")
            .unwrap();
        parameter.default = Some(json!("junior"));
        parameter.allowed = vec![json!("junior"), json!("senior")];
    }
    assert!(plans::validate_spec(&doc.nodes[0]).is_ok());
    let outside = json!({"id":"impl", "pattern":"generator", "parameters":{"label":"principal"}});
    let mut owner = fixture_owner(&[&pattern_result(outside)]);
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    let inside = json!({"id":"impl", "pattern":"generator", "parameters":{"label":"senior"}});
    let mut owner = fixture_owner(&[
        &pattern_result(inside),
        "implemented",
        "reviewed",
        "checked",
    ]);
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(
        run.status,
        GraphStatus::Complete,
        "{:?}",
        run.checkpoint.pause_reason()
    );
}

#[test]
fn pattern_read_roots_may_follow_only_a_closed_list() {
    let mut doc = pattern_planner(4);
    doc.nodes[0].plan.as_mut().unwrap().templates[0].nodes[0]["read_roots"] = json!(["${label}"]);
    assert!(
        plans::validate_spec(&doc.nodes[0]).is_err(),
        "free text names any root"
    );
    {
        let parameter = doc.nodes[0].plan.as_mut().unwrap().templates[0]
            .parameters
            .get_mut("label")
            .unwrap();
        parameter.default = Some(json!("/repo/a"));
        parameter.allowed = vec![json!("/repo/a"), json!("/repo/b")];
    }
    assert!(plans::validate_spec(&doc.nodes[0]).is_ok());
}

#[test]
fn pattern_read_roots_must_be_the_bare_placeholder_over_clean_absolute_values() {
    let closed = |roots: Value, values: &[&str]| {
        let mut doc = pattern_planner(4);
        let pattern = &mut doc.nodes[0].plan.as_mut().unwrap().templates[0];
        pattern.nodes[0]["read_roots"] = roots;
        let parameter = pattern.parameters.get_mut("label").unwrap();
        parameter.default = Some(json!(values[0]));
        parameter.allowed = values.iter().map(|value| json!(value)).collect();
        plans::validate_spec(&doc.nodes[0])
    };
    assert!(closed(json!(["${label}"]), &["/repo/a", "/repo/b"]).is_ok());
    // A suffix lets a parameter climb out of the registered worktree.
    assert!(closed(json!(["${label}/../../outside"]), &["/repo/a"]).is_err());
    assert!(closed(json!(["/repo/${label}"]), &["a"]).is_err());
    // An enum value that is relative or climbs is no read root.
    assert!(closed(json!(["${label}"]), &["/repo/a", "/repo/../../outside"]).is_err());
    assert!(closed(json!(["${label}"]), &["/repo/a", "../outside"]).is_err());
    assert!(closed(json!(["${label}"]), &["/repo/a", ""]).is_err());
}

/// A preset planner with a static fallback refuses an invalid fragment by
/// running the fallback and naming the refusal, never by trusting it.
fn assert_refused_into_fallback(run: &GraphRun) {
    assert_eq!(run.status, GraphStatus::Complete);
    let plan = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "plan")
        .unwrap();
    let reason = &plan
        .outputs
        .as_ref()
        .unwrap()
        .plan
        .as_ref()
        .unwrap()
        .fallback_reason;
    assert!(reason.contains("planner fragment invalid"), "{reason}");
}

fn planned_fixture(name: &str) -> GraphDocument {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("assets/test-fixtures/plans/{name}.plan.json"));
    let text = std::fs::read_to_string(&path).unwrap().replace(
        "\"commit\": \"HEAD\"",
        "\"commit\": \"0123456789abcdef0123456789abcdef01234567\"",
    );
    serde_json::from_str(&text).unwrap()
}

#[test]
fn planned_scout_swarm_expands_scouts_with_a_question_and_a_route() {
    let doc = planned_fixture("scout-swarm");
    let instances = vec![
        json!({"id":"a", "pattern":"scout", "parameters":{"question":"Where is X decided?"}}),
        json!({"id":"b", "pattern":"scout", "parameters":{"question":"Who calls Y?", "route":"senior"}}),
        json!({"id":"web", "pattern":"external-scout", "parameters":{"question":"How do others do Z?"}}),
    ];
    let fragment = pattern_result_all(instances);
    let mut results = vec![fragment.as_str()];
    results.extend(std::iter::repeat_n("done", 12));
    let mut owner = fixture_owner(&results);
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(
        run.status,
        GraphStatus::Complete,
        "{:?}",
        run.checkpoint.pause_reason()
    );
    let expansion = &run.checkpoint.expansions[0];
    let recorded: Vec<_> = expansion.uses.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(recorded, ["scout", "scout", "external-scout"]);
    assert_eq!(expansion.uses[1].parameters["route"], "senior");
    assert_eq!(expansion.uses[0].parameters["route"], "junior");
    // The synthesis reads each scout's evidence as well as the planner's text.
    let expanded =
        plans::expand(&doc, &[], &run.checkpoint.expansions, &plans::generous()).unwrap();
    let synthesis = expanded.nodes.iter().find(|n| n.id == "synthesis").unwrap();
    let inputs: Vec<_> = synthesis.inputs.iter().map(|i| i.node.as_str()).collect();
    assert_eq!(
        inputs,
        [
            "plan",
            "plan--a--scout",
            "plan--b--scout",
            "plan--web--scout"
        ]
    );
    assert!(synthesis.depends_on.contains(&"plan--a--scout".to_owned()));
    // The scouts keep the caller's read roots, which the planner holds too.
    let scout = expanded
        .nodes
        .iter()
        .find(|n| n.id == "plan--a--scout")
        .unwrap();
    assert!(
        scout
            .read_roots
            .iter()
            .any(|root| root.ends_with("/repo/extra")),
        "{:?}",
        scout.read_roots
    );
    // A route outside the closed list is refused, not trusted.
    let outside = pattern_result_all(vec![
        json!({"id":"a", "pattern":"scout", "parameters":{"question":"q", "route":"web"}}),
    ]);
    let mut owner = fixture_owner(&[&outside]);
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_refused_into_fallback(&run);
}

#[test]
fn single_planned_review_records_rationale_before_lens() {
    let doc = planned_fixture("review");
    let fragment = pattern_result(
        json!({"id":"correctness", "pattern":"lens-correctness-spec", "parameters":{"tier":"senior"}}),
    );
    let output = format!("The change needs a correctness review.\n{fragment}");
    let mut results = vec!["framed", output.as_str()];
    results.extend(std::iter::repeat_n("done\nVERDICT: PASS", 12));
    let mut owner = fixture_owner(&results);
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(
        run.status,
        GraphStatus::Complete,
        "{:?}",
        run.checkpoint.pause_reason()
    );
    let planner = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "plan")
        .unwrap();
    assert!(
        planner
            .outputs
            .as_ref()
            .unwrap()
            .result
            .as_ref()
            .unwrap()
            .excerpt
            .contains("The change needs a correctness review.")
    );
    let planner_call = owner
        .calls
        .iter()
        .position(|(key, _)| key.node_id == "plan")
        .unwrap();
    let lens_call = owner
        .calls
        .iter()
        .position(|(key, _)| key.node_id == "plan--correctness--lens")
        .unwrap();
    assert!(planner_call < lens_call);
    assert_eq!(run.checkpoint.expansions[0].uses.len(), 1);
}

#[test]
fn planned_review_expands_only_the_lens_legs_the_catalogue_lists() {
    let doc = planned_fixture("review");
    let leg = |lens: &str, tier: &str| json!({"id":lens, "pattern":format!("lens-{lens}"), "parameters":{"tier":tier}});
    let fragment = pattern_result_all(vec![
        leg("correctness-spec", "senior"),
        leg("tests", "junior"),
    ]);
    let mut results = vec!["framed", fragment.as_str()];
    results.extend(std::iter::repeat_n("done\nVERDICT: PASS", 12));
    let mut owner = fixture_owner(&results);
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(
        run.status,
        GraphStatus::Complete,
        "{:?}",
        run.checkpoint.pause_reason()
    );
    let expansion = &run.checkpoint.expansions[0];
    let recorded: Vec<_> = expansion.uses.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(recorded, ["lens-correctness-spec", "lens-tests"]);
    assert_eq!(expansion.uses[0].parameters["tier"], "senior");
    // The verifier reads the lens findings through the lens legs' exports.
    let expanded =
        plans::expand(&doc, &[], &run.checkpoint.expansions, &plans::generous()).unwrap();
    let verify = expanded.nodes.iter().find(|n| n.id == "verify").unwrap();
    let inputs: Vec<_> = verify.inputs.iter().map(|i| i.node.as_str()).collect();
    assert_eq!(
        inputs,
        ["plan", "plan--correctness-spec--lens", "plan--tests--lens"]
    );
    // The security lens is gated off for this subject, so it cannot be named.
    let gated = pattern_result_all(vec![leg("security-privacy", "junior")]);
    let mut owner = fixture_owner(&["framed", &gated]);
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_refused_into_fallback(&run);
    // A lens the catalogue lists cannot be run at a tier outside its closed list.
    let wild = pattern_result_all(vec![leg("tests", "web")]);
    let mut owner = fixture_owner(&["framed", &wild]);
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_refused_into_fallback(&run);
}

#[test]
fn planned_workers_and_the_no_scout_note_expand_and_feed_their_consumers() {
    for (name, instances, ids, last) in [
        (
            "fan-out-synthesise",
            vec![
                json!({"id":"w1", "pattern":"worker", "parameters":{"question":"What does X do?"}}),
                json!({"id":"w2", "pattern":"worker", "parameters":{"question":"Who owns Y?", "route":"principal"}}),
            ],
            vec!["plan--w1--worker", "plan--w2--worker"],
            "synthesis",
        ),
        (
            "scope-then-run",
            vec![
                json!({"id":"skip", "pattern":"no-scout", "parameters":{"reason":"the brief names the files"}}),
            ],
            vec!["plan--skip--note"],
            "synthesis",
        ),
    ] {
        let doc = planned_fixture(name);
        let fragment = pattern_result_all(instances);
        let mut results = vec![fragment.as_str()];
        results.extend(std::iter::repeat_n("done\nVERDICT: PASS", 20));
        let mut owner = fixture_owner(&results);
        let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
        assert_eq!(
            run.status,
            GraphStatus::Complete,
            "{name}: {:?}",
            run.checkpoint.pause_reason()
        );
        let expanded =
            plans::expand(&doc, &[], &run.checkpoint.expansions, &plans::generous()).unwrap();
        let consumer = expanded.nodes.iter().find(|n| n.id == last).unwrap();
        let inputs: Vec<_> = consumer.inputs.iter().map(|i| i.node.as_str()).collect();
        assert_eq!(&inputs[1..], ids, "{name}");
    }
    // The worker's route is a closed list too.
    let doc = planned_fixture("fan-out");
    let outside = pattern_result_all(vec![
        json!({"id":"w", "pattern":"worker", "parameters":{"question":"q", "route":"web"}}),
    ]);
    let mut owner = fixture_owner(&[&outside]);
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_refused_into_fallback(&run);
}

#[test]
fn a_hand_written_plan_keeps_its_consumers_declared_inputs() {
    let mut doc = pattern_planner(4);
    doc.nodes[1].inputs = vec![GraphInput {
        required_outputs: Vec::new(),
        extra: Default::default(),
        node: "plan".into(),
        outputs: vec![OutputKind::Result],
    }];
    let instance = json!({"id":"impl", "pattern":"generator", "parameters":{}});
    let run_it = |doc: &GraphDocument| {
        let mut owner = fixture_owner(&[&pattern_result(instance.clone()), "a", "b", "c"]);
        let run = advance(doc, &mut owner, &mut MemoryJournal::default()).unwrap();
        assert_eq!(
            run.status,
            GraphStatus::Complete,
            "{:?}",
            run.checkpoint.pause_reason()
        );
        plans::expand(doc, &[], &run.checkpoint.expansions, &plans::generous()).unwrap()
    };
    let expanded = run_it(&doc);
    let after = expanded.nodes.iter().find(|n| n.id == "after").unwrap();
    let inputs: Vec<_> = after.inputs.iter().map(|i| i.node.as_str()).collect();
    assert_eq!(inputs, ["plan"], "no wire_exports, no extra inputs");
    // Declared by the plan node, the same consumer also reads the instance exports.
    doc.nodes[0].plan.as_mut().unwrap().wire_exports = true;
    let wired = run_it(&doc);
    let after = wired.nodes.iter().find(|n| n.id == "after").unwrap();
    assert!(after.inputs.len() > 1);
}

fn pattern_result_all(instances: Vec<Value>) -> String {
    format!(
        "```fragment\n{}\n```",
        json!({"schema_version":1,"nodes":[],"patterns":instances})
    )
}

#[test]
fn planned_big_batch_refuses_overlapping_waves_and_budgets_over_the_lane_deadline() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("assets/test-fixtures/plans/big-batch.plan.json");
    let mut doc: GraphDocument =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap().replace(
            "\"commit\": \"HEAD\"",
            "\"commit\": \"0123456789abcdef0123456789abcdef01234567\"",
        ))
        .unwrap();
    doc.nodes.retain(|node| node.id == "plan");
    doc.nodes[0].depends_on.clear();
    doc.nodes[0].inputs.clear();
    let lane = |id: &str, tree: &str, scope: &str, budget: u64| {
        json!({"id":id, "pattern":"writer-lane", "parameters":{
        "lane":id, "worktree":tree, "scope":[scope], "acceptance":["works"], "budget_seconds":budget}})
    };
    let overlap = pattern_result_all(vec![
        lane("a", "/repo/lane1", "src", 600),
        lane("b", "/repo/lane2", "src/b", 600),
    ]);
    let mut owner = fixture_owner(&[&overlap, "a", "b"]);
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(
        run.checkpoint.expansions.len(),
        1,
        "{:?}",
        run.checkpoint.pause_reason()
    );

    let cases = [
        ("budget", vec![lane("a", "/repo/lane1", "src/a", 3601)]),
        ("worktree", vec![lane("a", "/elsewhere", "src/a", 600)]),
    ];
    for (label, instances) in cases {
        let fragment = format!(
            "```fragment\n{}\n```",
            json!({"schema_version":1,"nodes":[],"patterns":instances})
        );
        let mut owner = fixture_owner(&[&fragment]);
        let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
        assert_eq!(run.status, GraphStatus::Paused, "{label}");
        assert_eq!(
            run.checkpoint.pause_cause(),
            Some(PauseCause::InvalidPlanExpansion),
            "{label}"
        );
        assert!(run.checkpoint.expansions.is_empty(), "{label}");
        assert_eq!(owner.calls.len(), 1, "{label}");
    }
    // Relative scope is not used to connect distinct worktrees.
    let fragment = format!(
        "```fragment\n{}\n```",
        json!({"schema_version":1,"nodes":[],"patterns":[lane("a", "/repo/lane1", "src", 3600), lane("b", "/repo/lane2", "src/b", 60)]})
    );
    let mut owner = fixture_owner(&[&fragment, "a", "b"]);
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(
        run.checkpoint.expansions.len(),
        1,
        "{:?}",
        run.checkpoint.pause_reason()
    );
}

fn running_graph() -> (GraphDocument, GraphCheckpoint) {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1, "graph_id":"live", "nodes":[
            {"id":"a", "access":"read_only", "prompt":"Start", "route":{"alias":"senior"}},
            {"id":"b", "access":"read_only", "prompt":"Then", "depends_on":["a"], "route":{"alias":"senior"}}
        ]
    })).unwrap();
    let mut owner = FakeOwner::default();
    owner.observations.push_back(AttemptObservation::Pending {
        attempt_ref: Some("a".into()),
    });
    let checkpoint = advance(&document, &mut owner, &mut MemoryJournal::default())
        .unwrap()
        .checkpoint;
    (document, checkpoint)
}

fn host_patch(value: Value) -> Patch {
    serde_json::from_value(value).unwrap()
}

fn admit(
    document: &GraphDocument,
    checkpoint: &mut GraphCheckpoint,
    patch: Value,
) -> Result<Admitted, PatchError> {
    admit_with(document, checkpoint, None, patch, &|_| Ok(()))
}

fn admit_with(
    document: &GraphDocument,
    checkpoint: &mut GraphCheckpoint,
    parent: Option<&str>,
    patch: Value,
    scan: &dyn Fn(&str) -> Result<(), String>,
) -> Result<Admitted, PatchError> {
    let library = TemplateLibrary::bundled();
    let resolvers = std::collections::BTreeMap::from([(
        "areas".to_owned(),
        vec!["src".to_owned(), "docs".to_owned()],
    )]);
    let limits = plans::generous();
    let context = PatchContext {
        library: &library,
        resolvers: &resolvers,
        scan,
        journal: None,
        limits: &limits,

        worktree_layout: provenant_config::PortTable::builtin().worktree_layout,
    };
    admit_patch(
        &mut LiveGraph {
            document,
            checkpoint,
        },
        parent,
        &host_patch(patch),
        &context,
    )
}

fn live_ids(document: &GraphDocument, checkpoint: &GraphCheckpoint) -> Vec<String> {
    let expanded = plans::expand(
        document,
        &checkpoint.extensions,
        &checkpoint.expansions,
        &plans::generous(),
    )
    .unwrap();
    expanded.nodes.into_iter().map(|node| node.id).collect()
}

#[test]
fn admit_patch_adds_a_swarm_and_replays_by_idempotency_key() {
    let (document, mut checkpoint) = running_graph();
    let patch = json!({"schema_version":1, "expected_revision":0, "idempotency_key":"map-1",
        "reason":"map before acting",
        "nodes":[{"id":"map", "use":"explore", "over":"@areas", "after":["a"]}]});
    let admitted = admit(&document, &mut checkpoint, patch.clone()).unwrap();
    assert_eq!((admitted.revision, admitted.replayed), (1, false));
    assert!(
        admitted.added.contains(&"map".to_owned()),
        "{:?}",
        admitted.added
    );
    let ids = live_ids(&document, &checkpoint);
    for id in ["map", "map--src", "map--docs"] {
        assert!(ids.contains(&id.to_owned()), "{ids:?}");
    }
    let record = checkpoint.expansions[0].patch.as_ref().unwrap();
    assert_eq!(
        (
            record.revision,
            record.key.as_deref(),
            record.parent.as_deref()
        ),
        (1, Some("map-1"), None)
    );
    assert_eq!(record.reason.as_deref(), Some("map before acting"));
    assert_eq!(checkpoint.expansions[0].uses[0].name, "explore");
    assert!(
        checkpoint
            .nodes
            .iter()
            .any(|node| node.id == "map--src" && node.state == NodeState::Pending)
    );

    let replay = admit(&document, &mut checkpoint, patch).unwrap();
    assert_eq!((replay.revision, replay.replayed), (1, true));
    assert_eq!(checkpoint.expansions.len(), 1);

    let changed = json!({"schema_version":1, "idempotency_key":"map-1",
        "nodes":[{"id":"other", "use":"task", "after":["a"]}]});
    assert_eq!(
        admit(&document, &mut checkpoint, changed).unwrap_err().code,
        "idempotency_conflict"
    );
    let stale = json!({"schema_version":1, "expected_revision":0,
        "nodes":[{"id":"late", "use":"task", "after":["a"]}]});
    assert_eq!(
        admit(&document, &mut checkpoint, stale).unwrap_err().code,
        "stale_revision"
    );
}

#[test]
fn admit_patch_refuses_cycles_conflicts_and_secrets() {
    let (document, mut checkpoint) = running_graph();
    let cycle = json!({"schema_version":1, "nodes":[
        {"id":"x", "use":"task", "after":["y"]}, {"id":"y", "use":"task", "after":["x"]}]});
    assert_eq!(
        admit(&document, &mut checkpoint, cycle).unwrap_err().code,
        "cycle"
    );
    let duplicate = json!({"schema_version":1, "nodes":[{"id":"a", "use":"task"}]});
    assert_eq!(
        admit(&document, &mut checkpoint, duplicate)
            .unwrap_err()
            .code,
        "conflict"
    );
    let both = json!({"schema_version":1, "skip":["b"],
        "replace":[{"id":"b", "access":"read_only", "prompt":"New", "route":{"alias":"senior"}}]});
    assert_eq!(
        admit(&document, &mut checkpoint, both).unwrap_err().code,
        "conflict"
    );
    let secret = json!({"schema_version":1, "nodes":[{"id":"leak", "use":"task", "brief":"use sk-fixture-secret"}]});
    let error = admit_with(&document, &mut checkpoint, None, secret, &|text| {
        if text.contains("sk-fixture-secret") {
            Err("credential-shaped text".into())
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert_eq!(error.code, "secret");
    assert!(
        checkpoint.expansions.is_empty(),
        "refused patches leave no journal entry"
    );
}

#[test]
fn admit_patch_skips_and_replaces_only_unstarted_nodes() {
    let (document, mut checkpoint) = running_graph();
    let started = json!({"schema_version":1, "skip":["a"]});
    assert_eq!(
        admit(&document, &mut checkpoint, started).unwrap_err().code,
        "started"
    );
    let replace = json!({"schema_version":1, "replace":[{"id":"b", "use":"review", "with":{"lens":"tests"}, "after":["a"]}]});
    let admitted = admit(&document, &mut checkpoint, replace).unwrap();
    assert_eq!(admitted.revision, 1);
    let expanded = plans::expand(
        &document,
        &checkpoint.extensions,
        &checkpoint.expansions,
        &plans::generous(),
    )
    .unwrap();
    let b = expanded.nodes.iter().find(|node| node.id == "b").unwrap();
    assert!(b.prompt.contains("tests lens"), "{}", b.prompt);
    assert_eq!(
        checkpoint.expansions[0]
            .patch
            .as_ref()
            .unwrap()
            .replaced
            .len(),
        1
    );

    let skip = json!({"schema_version":1, "expected_revision":1, "skip":["b"]});
    assert_eq!(admit(&document, &mut checkpoint, skip).unwrap().revision, 2);
    assert_eq!(
        checkpoint
            .nodes
            .iter()
            .find(|node| node.id == "b")
            .unwrap()
            .state,
        NodeState::Skipped
    );
    assert_eq!(
        checkpoint.expansions[1].patch.as_ref().unwrap().skipped,
        ["b"]
    );
}

#[test]
fn admit_patch_confines_lane_edits_but_allows_owner_and_planner_edits() {
    let (document, checkpoint) = running_graph();
    for edit in [
        json!({"schema_version":1, "skip":["b"]}),
        json!({"schema_version":1, "replace":[{"id":"b", "use":"task", "brief":"Replacement"}]}),
    ] {
        let mut refused = checkpoint.clone();
        let error = admit_with(
            &document,
            &mut refused,
            Some("a"),
            edit.clone(),
            &|_| Ok(()),
        )
        .unwrap_err();
        assert_eq!(error.code, "edit_scope");
        assert!(error.message.contains("b"));
        assert_eq!(refused, checkpoint);

        let mut owner = checkpoint.clone();
        admit(&document, &mut owner, edit.clone()).unwrap();

        let mut planned = document.clone();
        let library = TemplateLibrary::bundled();
        let resolvers = Default::default();
        planned.nodes[0] = Compiler {
            library: &library,
            resolvers: &resolvers,
        }
        .compile(&json!({"id":"a", "use":"plan"}))
        .unwrap();
        let mut planner = checkpoint.clone();
        admit_with(&planned, &mut planner, Some("a"), edit, &|_| Ok(())).unwrap();
    }
}

#[test]
fn admitted_nodes_name_their_admitter_for_the_sandbox_floor() {
    let admitted_by = |node: &GraphNode| {
        node.attempt
            .as_ref()
            .and_then(|spec| spec.admitted_by.clone())
    };
    // A planner's replace of a node outside its subtree, carrying a forged
    // admitter, is named after the planner; so is a node it adds.
    let (document, checkpoint) = running_graph();
    let library = TemplateLibrary::bundled();
    let resolvers = Default::default();
    let mut planned = document.clone();
    planned.nodes[0] = Compiler {
        library: &library,
        resolvers: &resolvers,
    }
    .compile(&json!({"id":"a", "use":"plan"}))
    .unwrap();
    let mut planner = checkpoint.clone();
    admit_with(&planned, &mut planner, Some("a"), json!({"schema_version":1,
        "nodes":[{"id":"child", "use":"task", "brief":"Child"}],
        "replace":[{"id":"b", "use":"task", "brief":"Replacement", "attempt":{"admitted_by":"elsewhere"}}]}),
        &|_| Ok(())).unwrap();
    let expansion = planner.expansions.last().unwrap();
    assert_eq!(admitted_by(&expansion.nodes[0]).as_deref(), Some("a"));
    assert_eq!(
        admitted_by(&expansion.patch.as_ref().unwrap().replaced[0]).as_deref(),
        Some("a")
    );
    // The host admits on its own authority and names no admitter.
    let mut owner = checkpoint.clone();
    admit(
        &document,
        &mut owner,
        json!({"schema_version":1,
        "replace":[{"id":"b", "use":"task", "brief":"Replacement"}]}),
    )
    .unwrap();
    assert_eq!(
        admitted_by(
            &owner
                .expansions
                .last()
                .unwrap()
                .patch
                .as_ref()
                .unwrap()
                .replaced[0]
        ),
        None
    );
    // Nested members, loop bodies and fallback stages expand later and carry it too.
    let mut nested: GraphNode = serde_json::from_value(json!({
        "id":"c", "access":"read_only", "kind":"composite",
        "composite":{"nodes":[{"id":"m", "access":"read_only", "prompt":"M",
            "loop":{"body":[{"id":"r", "access":"read_only", "prompt":"R"}], "until":"done", "max_rounds":2}}],
            "exports":[]}
    })).unwrap();
    patch::stamp_admitter(&mut nested, "lane");
    let member = &nested.composite.as_ref().unwrap().nodes[0];
    assert_eq!(admitted_by(&nested).as_deref(), Some("lane"));
    assert_eq!(admitted_by(member).as_deref(), Some("lane"));
    assert_eq!(
        admitted_by(&member.loop_body.as_ref().unwrap().body[0]).as_deref(),
        Some("lane")
    );
}

#[test]
fn admit_patch_allows_lane_edits_in_its_subtree() {
    let (document, mut checkpoint) = running_graph();
    admit_with(
        &document,
        &mut checkpoint,
        Some("a"),
        json!({"schema_version":1,"nodes":[{"id":"child", "use":"task", "brief":"Child"}]}),
        &|_| Ok(()),
    )
    .unwrap();
    for edit in [
        json!({"schema_version":1, "skip":["a--child"]}),
        json!({"schema_version":1, "replace":[{"id":"a--child", "use":"task", "brief":"Replacement"}]}),
    ] {
        let mut edited = checkpoint.clone();
        admit_with(&document, &mut edited, Some("a"), edit, &|_| Ok(())).unwrap();
    }
}

#[test]
fn admit_patch_journals_before_commit_and_tags_its_nodes_with_the_parent() {
    let (document, mut checkpoint) = running_graph();
    let library = TemplateLibrary::bundled();
    let resolvers = std::collections::BTreeMap::new();
    let limits = plans::generous();
    let journaled = std::cell::RefCell::new(Vec::new());
    let sink = |record: &PatchRecord| {
        journaled
            .borrow_mut()
            .push((record.revision, record.parent.clone(), record.journaled));
        Ok(())
    };
    let context = PatchContext {
        library: &library,
        resolvers: &resolvers,
        scan: &|_| Ok(()),
        journal: Some(&sink),
        limits: &limits,

        worktree_layout: provenant_config::PortTable::builtin().worktree_layout,
    };
    let patch =
        host_patch(json!({"schema_version":1, "nodes":[{"id":"x", "use":"task", "brief":"Work"}]}));
    admit_patch(
        &mut LiveGraph {
            document: &document,
            checkpoint: &mut checkpoint,
        },
        Some("a"),
        &patch,
        &context,
    )
    .unwrap();
    // The sink saw the record before commit, unmarked, with the parent named.
    assert_eq!(*journaled.borrow(), [(1, Some("a".to_owned()), false)]);
    assert!(checkpoint.expansions[0].patch.as_ref().unwrap().journaled);
    assert_eq!(
        checkpoint.expansions[0].nodes[0].extra.get("parent_node"),
        Some(&json!("a"))
    );

    let failing = |_: &PatchRecord| Err("disk full".to_owned());
    let context = PatchContext {
        library: &library,
        resolvers: &resolvers,
        scan: &|_| Ok(()),
        journal: Some(&failing),
        limits: &limits,

        worktree_layout: provenant_config::PortTable::builtin().worktree_layout,
    };
    let patch =
        host_patch(json!({"schema_version":1, "nodes":[{"id":"y", "use":"task", "brief":"Work"}]}));
    let error = admit_patch(
        &mut LiveGraph {
            document: &document,
            checkpoint: &mut checkpoint,
        },
        None,
        &patch,
        &context,
    )
    .unwrap_err();
    assert_eq!(error.code, "journal");
    assert_eq!(
        checkpoint.expansions.len(),
        1,
        "an unjournaled admission commits nothing"
    );
}

#[test]
fn submit_compiles_a_patch_into_a_new_graph() {
    let library = TemplateLibrary::bundled();
    let resolvers = Default::default();
    let limits = plans::generous();
    let context = PatchContext {
        library: &library,
        resolvers: &resolvers,
        scan: &|_| Ok(()),
        journal: None,
        limits: &limits,

        worktree_layout: provenant_config::PortTable::builtin().worktree_layout,
    };
    let patch = host_patch(json!({"schema_version":1, "templates":{"pair":{"nodes":[
            {"id":"one", "use":"task", "brief":"First"}, {"id":"two", "use":"task", "brief":"Second", "after":["one"]}]}},
        "nodes":[{"id":"map", "use":"explore", "over":["src"]}, {"id":"work", "use":"pair", "after":["map"]}]}));
    let document = patch::submit("submitted", &patch, &context, None).unwrap();
    assert_eq!(document.nodes.len(), 2);
    assert_eq!(document.nodes[1].kind, Some(NodeKind::Composite));
    let cycle = host_patch(json!({"schema_version":1, "nodes":[
        {"id":"x", "use":"task", "after":["y"]}, {"id":"y", "use":"task", "after":["x"]}]}));
    assert_eq!(
        patch::submit("cyclic", &cycle, &context, None)
            .unwrap_err()
            .code,
        "cycle"
    );
    let stale = host_patch(
        json!({"schema_version":1, "expected_revision":3, "nodes":[{"id":"x", "use":"task"}]}),
    );
    assert_eq!(
        patch::submit("stale", &stale, &context, None)
            .unwrap_err()
            .code,
        "stale_revision"
    );
}

#[test]
fn planner_fragments_may_emit_composites_and_loops() {
    let fragment = json!({"schema_version":1,
        "templates":{"attempt":{"nodes":[
            {"id":"work", "use":"task", "brief":"Try the slice", "acceptance_checks":["It works"]},
            {"id":"done", "kind":"gate", "access":"read_only", "after":["work"],
             "inputs":[{"node":"work", "outputs":["verdict"]}],
             "condition":{"type":"verdict", "expected":"VERDICT: PASS", "threshold":"all"}}],
            "exports":[{"node":"done", "outputs":["result"]}]}},
        "nodes":[
            {"id":"map", "use":"explore", "over":["src", "docs"], "acceptance_checks":["Cite paths"]},
            {"id":"try", "use":"attempt", "after":["map"], "repeat":{"until":"done", "max_rounds":2}}]});
    let output = format!("```fragment\n{fragment}\n```");
    let mut owner = fixture_owner(&[&output]);
    let run = advance(&planner(8), &mut owner, &mut MemoryJournal::default()).unwrap();
    assert!(
        run.checkpoint.pause.is_none(),
        "{:?}",
        run.checkpoint.pause_reason()
    );
    let expansion = &run.checkpoint.expansions[0];
    assert!(
        expansion
            .patch
            .as_ref()
            .is_some_and(|record| record.parent.as_deref() == Some("plan"))
    );
    let ids: Vec<_> = run
        .checkpoint
        .nodes
        .iter()
        .map(|node| node.id.as_str())
        .collect();
    for id in [
        "plan--map",
        "plan--map--src",
        "plan--map--docs",
        "plan--try",
        "plan--try--r1--work",
    ] {
        assert!(ids.contains(&id), "{ids:?}");
    }
}

#[test]
fn bundled_plan_admits_a_fragment_patch() {
    let library = TemplateLibrary::bundled();
    let node = template::Compiler::new(&library)
        .compile(&json!({"id":"plan", "use":"plan"}))
        .unwrap();
    let mut document = planner(8);
    document.nodes[0] = node;
    let output = result(vec![slice("slice", &[])]);
    let run = advance(
        &document,
        &mut fixture_owner(&[&output]),
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert!(
        run.checkpoint.pause.is_none(),
        "{:?}",
        run.checkpoint.pause_reason()
    );
    assert!(
        run.checkpoint
            .nodes
            .iter()
            .any(|node| node.id == "plan--slice")
    );
    assert!(run.checkpoint.expansions[0].patch.is_some());
}

#[test]
fn initial_submission_is_a_durable_revision_and_replays_template_evidence() {
    let library = TemplateLibrary::bundled();
    let resolvers = Default::default();
    let compiler = Compiler {
        library: &library,
        resolvers: &resolvers,
    };
    let node = compiler
        .compile(&json!({"id":"work", "use":"task", "brief":"Do the work"}))
        .unwrap();
    let second = compiler
        .compile(&json!({"id":"then", "use":"task", "brief":"Continue", "after":["work"]}))
        .unwrap();
    let mut document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1, "graph_id":"submitted", "nodes":[node, second],
        "provenance":{"parameters":{"brief":"Do the work"}}
    }))
    .unwrap();
    let limits = plans::generous();
    patch::submission(
        &mut document,
        "operation-one",
        &limits,
        provenant_config::PortTable::builtin().worktree_layout,
    )
    .unwrap();
    struct RefuseSave;
    impl GraphJournal for RefuseSave {
        type Error = std::io::Error;
        fn load(&mut self, _: &str) -> Result<Option<GraphCheckpoint>, Self::Error> {
            Ok(None)
        }
        fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error> {
            assert_eq!(patch::revision(checkpoint), 1);
            assert_eq!(checkpoint.expansions[0].uses[0].name, "task");
            Err(std::io::Error::other("injected save failure"))
        }
    }
    let mut unlaunched = FakeOwner::default();
    assert!(advance(&document, &mut unlaunched, &mut RefuseSave).is_err());
    assert!(unlaunched.calls.is_empty());
    let mut owner = FakeOwner::default();
    owner.observations.push_back(AttemptObservation::Pending {
        attempt_ref: Some("work-attempt".into()),
    });
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(patch::revision(&run.checkpoint), 1);
    let expansion = &run.checkpoint.expansions[0];
    assert_eq!(
        expansion.patch.as_ref().unwrap().key.as_deref(),
        Some("operation-one")
    );
    assert_eq!(expansion.uses[0].name, "task");
    assert_eq!(expansion.uses[0].nodes, vec!["work"]);
    assert_eq!(expansion.uses[1].nodes, vec!["then"]);
    assert_eq!(expansion.uses[0].parameters["brief"], "Do the work");
    assert_eq!(run.checkpoint.nodes.len(), 2);
    assert!(journal.0.as_ref().unwrap().expansions[0].patch.is_some());
    let mut checkpoint = run.checkpoint;
    let receipt = admit(
        &document,
        &mut checkpoint,
        document.extra["submission_patch"].clone(),
    )
    .unwrap();
    assert!(receipt.replayed);
    assert_eq!(receipt.revision, 1);
    owner.observations.push_back(AttemptObservation::Pending {
        attempt_ref: Some("work-attempt".into()),
    });
    let replay = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(patch::revision(&replay.checkpoint), 1);
    assert_eq!(replay.checkpoint.nodes.len(), 2);
    assert_eq!(replay.checkpoint.expansions.len(), 1);
    let retained = journal.0.clone();
    let mut changed = document.clone();
    changed.nodes[0].prompt = "Different work under the same operation".into();
    changed.extra.remove("submission_patch");
    patch::submission(
        &mut changed,
        "operation-one",
        &limits,
        provenant_config::PortTable::builtin().worktree_layout,
    )
    .unwrap();
    assert!(advance(&changed, &mut owner, &mut journal).is_err());
    assert_eq!(journal.0, retained);
}

#[test]
fn initial_submission_refuses_changed_snapshot_before_checkpoint_or_launch() {
    let library = TemplateLibrary::bundled();
    let resolvers = Default::default();
    let compiler = Compiler {
        library: &library,
        resolvers: &resolvers,
    };
    let node = compiler
        .compile(&json!({"id":"work", "use":"task", "brief":"Do the work"}))
        .unwrap();
    let mut document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1, "graph_id":"secret-submission", "nodes":[node]
    }))
    .unwrap();
    patch::submission(
        &mut document,
        "operation-two",
        &plans::generous(),
        provenant_config::PortTable::builtin().worktree_layout,
    )
    .unwrap();
    document.extra.get_mut("submission_patch").unwrap()["nodes"][0]["prompt"] =
        json!("changed prompt");
    let mut journal = MemoryJournal::default();
    let mut owner = FakeOwner::default();
    assert!(advance(&document, &mut owner, &mut journal).is_err());
    assert!(journal.0.is_none());
}

#[test]
fn initial_submission_envelope_counts_prompt_references_not_brief_copies() {
    let library = TemplateLibrary::bundled();
    let resolvers = Default::default();
    let compiler = Compiler {
        library: &library,
        resolvers: &resolvers,
    };
    let base = compiler
        .compile(&json!({"id":"work", "use":"task", "brief":"Work"}))
        .unwrap();
    let brief = "work ".repeat(220_000);
    let nodes: Vec<_> = (0..8)
        .map(|index| {
            let mut node = base.clone();
            node.id = format!("work{index}");
            node.prompt = brief.clone();
            node
        })
        .collect();
    let mut document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1, "graph_id":"large-submission", "nodes":nodes,
        "provenance":{"parameters":{"brief":brief}}
    }))
    .unwrap();
    let limits = plans::generous();
    patch::submission(
        &mut document,
        "large-operation",
        &limits,
        provenant_config::PortTable::builtin().worktree_layout,
    )
    .unwrap();
    assert!(canonical_len(&document) > MAX_CHECKPOINT_BYTES as usize);
    let (plan, _) = validate_plan_with_limits(&document, &limits).unwrap();
    assert!(graph_envelope_reason(&document, &[], &[], &plan).is_none());
}

#[test]
fn continuation_fragment_resolves_the_host_review_panel() {
    let mut document = planner(8);
    document.nodes[0].plan.as_mut().unwrap().extra.insert(
        "resolvers".into(),
        json!({"review-panel":["correctness","tests"]}),
    );
    let output = result(vec![json!({
        "id":"delta", "use":"review", "over":"@review-panel",
        "with":{"lens":"${item}"}, "acceptance_checks":["Review the repair delta"]
    })]);
    let passed = "```findings\n{\"schema_version\":1,\"findings\":[]}\n```\nVERDICT: PASS";
    let mut owner = fixture_owner(&[&output, passed, passed, "done"]);
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(run.checkpoint.successful());
    let expanded = &run.checkpoint.expansions[0].nodes;
    assert!(
        expanded
            .iter()
            .any(|node| node.id.ends_with("delta--correctness"))
    );
    assert!(
        expanded
            .iter()
            .any(|node| node.id.ends_with("delta--tests"))
    );
    assert_eq!(
        advance(&document, &mut fixture_owner(&[]), &mut journal)
            .unwrap()
            .checkpoint,
        run.checkpoint
    );
}

#[test]
fn ordinary_reviewer_final_patch_adds_a_follow_up() {
    let library = TemplateLibrary::bundled();
    let mut document = planner(8);
    document.nodes[0] = template::Compiler::new(&library)
        .compile(&json!({"id":"plan", "use":"review"}))
        .unwrap();
    let output = format!(
        "```findings\n{{\"schema_version\":1,\"findings\":[]}}\n```\n```patch\n{}\n```\nVERDICT: PASS",
        json!({"schema_version":1,
        "nodes":[{"id":"follow-up", "use":"task", "brief":"Verify the review finding"}]})
    );
    let run = advance(
        &document,
        &mut fixture_owner(&[&output]),
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert!(
        run.checkpoint.pause.is_none(),
        "{:?}",
        run.checkpoint.pause_reason()
    );
    assert!(
        run.checkpoint
            .nodes
            .iter()
            .any(|node| node.id == "plan--follow-up")
    );
    assert!(run.checkpoint.expansions[0].patch.is_some());
}

#[test]
fn planner_template_defaults_satisfy_acceptance_checks() {
    let output = result(vec![json!({"id":"leaf", "use":"task"})]);
    let run = advance(
        &planner(8),
        &mut fixture_owner(&[&output]),
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert!(
        run.checkpoint.pause.is_none(),
        "{:?}",
        run.checkpoint.pause_reason()
    );
    assert!(
        run.checkpoint
            .nodes
            .iter()
            .any(|node| node.id == "plan--leaf")
    );
}

#[test]
fn planner_uses_snapshotted_repository_flavour_and_review_panel() {
    let mut document = planner(8);
    document.extra.insert(
        "template_library".into(),
        json!({
            "review.repository":{"brief":"Check the repository-specific contract", "tier":"senior"}
        }),
    );
    document.extra.insert(
        "template_resolvers".into(),
        json!({
            "review-panel":["correctness", "tests"]
        }),
    );
    let output = result(vec![
        json!({"id":"repository", "use":"review.repository"}),
        json!({"id":"panel", "use":"review", "over":"@review-panel", "with":{"lens":"${item}"}}),
    ]);
    let run = advance(
        &document,
        &mut fixture_owner(&[&output]),
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert!(
        run.checkpoint.pause.is_none(),
        "{:?}",
        run.checkpoint.pause_reason()
    );
    let nodes = &run.checkpoint.expansions[0].nodes;
    assert!(nodes.iter().any(|node| node.id == "plan--repository"
        && node.prompt.contains("repository-specific contract")));
    for lens in ["correctness", "tests"] {
        let id = format!("plan--panel--{lens}");
        assert!(
            nodes
                .iter()
                .any(|node| node.id == id && node.prompt.contains(lens))
        );
    }
}

#[test]
fn planner_admits_bundled_delivery_composites_without_rewriting_them() {
    let library = TemplateLibrary::bundled();
    let resolvers = std::collections::BTreeMap::from([
        ("review-panel".into(), vec!["correctness".into()]),
        ("review-frame".into(), vec!["false".into()]),
        ("review-verify".into(), vec!["false".into()]),
        ("review-synthesis".into(), vec!["false".into()]),
        ("review-verifiers".into(), vec!["1".into()]),
        ("review-advice-correctness".into(), vec![String::new()]),
    ]);
    let compiler = template::Compiler {
        library: &library,
        resolvers: &resolvers,
    };
    let document = planner(8);
    let plan = validate_flat_plan(&document, &Default::default())
        .unwrap()
        .0;
    let planner = plan.nodes.iter().find(|node| node.id == "plan").unwrap();
    let spec = planner.plan.as_ref().unwrap();
    for name in ["batch", "deliver"] {
        let mut node = compiler
            .compile(&json!({"id":"delivery", "use":name,
            "with":{"worktree":".worktrees/slice", "write_scope":["src"]}}))
            .unwrap();
        let expected = node.clone();
        plans::prepare(&mut node, planner, spec, &document, true).unwrap();
        assert_eq!(
            node, expected,
            "{name} must retain its authored source and delivery"
        );
    }
}

#[test]
fn ordinary_agent_patch_refusal_replays_without_growing_or_disclosing_secrets() {
    let secret = "sk-abcdefghijklmnopqrstuvwx";
    for payload in [
        json!({"schema_version":1, "nodes":[{"id":"child", "use":"task.typo"}]}),
        json!({"schema_version":secret, "nodes":[{"id":"child", "use":"task"}]}),
    ] {
        let mut document = planner(8);
        document.nodes[0].plan = None;
        let output = format!("```patch\n{payload}\n```");
        let mut journal = MemoryJournal::default();
        let run = advance(&document, &mut fixture_owner(&[&output]), &mut journal).unwrap();
        assert_eq!(run.status, GraphStatus::Paused);
        assert!(run.checkpoint.expansions.is_empty());
        let refusal = run.checkpoint.pause_reason().unwrap();
        assert!(!refusal.contains(secret), "{refusal}");
        assert!(run.checkpoint.nodes.iter().all(|node| {
            node.fragment
                .as_deref()
                .is_none_or(|fragment| !fragment.contains(secret))
        }));
        let warnings = &run.checkpoint.nodes[0].outputs.as_ref().unwrap().warnings;
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("patch refused"))
        );
        assert!(warnings.iter().all(|warning| !warning.contains(secret)));
        for _ in 0..3 {
            journal.0 = Some(
                serde_json::from_slice(&serde_json::to_vec(&run.checkpoint).unwrap()).unwrap(),
            );
            let replay = advance(&document, &mut fixture_owner(&[]), &mut journal).unwrap();
            assert_eq!(replay.checkpoint, run.checkpoint);
        }
    }
}

#[test]
fn patch_examples_inside_markdown_fences_do_not_mutate_the_graph() {
    let patch = format!(
        "```patch\n{}\n```",
        json!({"schema_version":1,
        "nodes":[{"id":"child", "use":"task"}]})
    );
    for outer in ["````", "~~~~"] {
        let mut document = planner(8);
        document.nodes.truncate(1);
        document.nodes[0].plan = None;
        let output = format!("Example only:\n{outer}markdown\n{patch}\n{outer}");
        let run = advance(
            &document,
            &mut fixture_owner(&[&output]),
            &mut MemoryJournal::default(),
        )
        .unwrap();
        assert_eq!(run.status, GraphStatus::Complete);
        assert!(run.checkpoint.expansions.is_empty());
        assert_eq!(run.checkpoint.nodes.len(), 1);
    }
}

#[test]
fn ordinary_agent_final_patch_follows_earlier_live_children_once() {
    let (document, mut checkpoint) = running_graph();
    admit_with(
        &document,
        &mut checkpoint,
        Some("a"),
        json!({"schema_version":1,
        "nodes":[{"id":"live-child", "use":"task"}]}),
        &|_| Ok(()),
    )
    .unwrap();
    let expanded = plans::expand(
        &document,
        &checkpoint.extensions,
        &checkpoint.expansions,
        &plans::generous(),
    )
    .unwrap();
    let plan = validate_flat_plan(&expanded, &plans::generous()).unwrap().0;
    let parent = plan.nodes.iter().find(|node| node.id == "a").unwrap();
    checkpoint
        .nodes
        .iter_mut()
        .find(|node| node.id == "a")
        .unwrap()
        .state = NodeState::Succeeded;
    let output = format!(
        "```patch\n{}\n```",
        json!({"schema_version":1,
        "nodes":[{"id":"final-child", "use":"task"}]})
    );
    let (next, _, _) = plans::settle(
        parent,
        &document,
        &expanded,
        &mut checkpoint,
        Some(&capture(&output)),
        &plans::generous(),
        &provenant_config::PortTable::builtin().worktree_layout,
    )
    .expect("final patch must follow the live patch");
    assert_eq!(checkpoint.expansions.len(), 2);
    for id in ["a--live-child", "a--final-child"] {
        assert!(next.nodes.iter().any(|node| node.id == id));
    }
    assert!(
        plans::settle(
            parent,
            &document,
            &next,
            &mut checkpoint,
            Some(&capture(&output)),
            &plans::generous(),
            &provenant_config::PortTable::builtin().worktree_layout,
        )
        .is_none()
    );
    assert_eq!(checkpoint.expansions.len(), 2);
}

#[test]
fn planner_template_overrides_cannot_invent_capabilities() {
    for node in [
        json!({"id":"child", "use":"task", "capabilities":["postgres"]}),
        json!({"id":"child", "use":"task-chain", "composite":{
            "nodes":[{"id":"injected", "prompt":"Do the task", "access":"read_only",
                "route":{"alias":"senior"}, "acceptance_checks":["Verify it"], "capabilities":["postgres"]}],
            "exports":[{"node":"injected", "outputs":["result"]}]
        }}),
    ] {
        let output = result(vec![node]);
        let run = advance(
            &planner(8),
            &mut fixture_owner(&[&output]),
            &mut MemoryJournal::default(),
        )
        .unwrap();
        assert_eq!(run.status, GraphStatus::Paused);
        assert!(run.checkpoint.expansions.is_empty());
        assert!(
            run.checkpoint.pause_reason().unwrap().contains("postgres"),
            "{:?}",
            run.checkpoint.pause_reason()
        );
    }
}

#[test]
fn planner_admits_bundled_batch_and_deliver_through_the_patch_path() {
    let library = TemplateLibrary::bundled();
    let resolvers = std::collections::BTreeMap::from([
        ("review-panel".into(), vec!["correctness".into()]),
        ("review-frame".into(), vec!["false".into()]),
        ("review-verify".into(), vec!["false".into()]),
        ("review-synthesis".into(), vec!["false".into()]),
        ("review-verifiers".into(), vec!["1".into()]),
        ("review-advice-correctness".into(), vec![String::new()]),
    ]);
    let limits = plans::generous();
    let context = PatchContext {
        library: &library,
        resolvers: &resolvers,
        scan: &patch::scan_for_secrets,
        journal: None,
        limits: &limits,

        worktree_layout: provenant_config::PortTable::builtin().worktree_layout,
    };
    for name in ["batch", "deliver"] {
        let mut document = planner(128);
        document.schema_version = 2;
        document.integration_branch = Some("batch/integration".into());
        let mut owner = FakeOwner::default();
        owner
            .observations
            .push_back(AttemptObservation::Pending { attempt_ref: None });
        let mut checkpoint = advance(&document, &mut owner, &mut MemoryJournal::default())
            .unwrap()
            .checkpoint;
        let patch = host_patch(json!({"schema_version":1, "nodes":[{
            "id":"delivery", "use":name,
            "with":{"worktree":".worktrees/slice", "write_scope":["src"]}
        }]}));
        let admitted = admit_patch(
            &mut LiveGraph {
                document: &document,
                checkpoint: &mut checkpoint,
            },
            Some("plan"),
            &patch,
            &context,
        )
        .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(admitted.added.contains(&"plan--delivery".to_owned()));
        let nodes = &checkpoint.expansions[0].nodes;
        assert!(nodes.iter().any(|node| node.source.is_some()));
        assert!(
            nodes
                .iter()
                .any(|node| node.access == AccessMode::WorktreeWrite)
        );
        assert!(nodes.iter().any(|node| node.delivery.is_some()));
    }
}

#[test]
fn raw_submission_preserves_raw_reporting_label() {
    let mut document: GraphDocument = serde_json::from_value(json!({
        "schema_version": 1, "graph_id": "raw-reporting", "nodes": [{
            "id": "custom", "kind": "task", "access":"read_only", "prompt": "Work",
            "route": {"alias": "junior"}
        }]
    }))
    .unwrap();
    patch::submission(
        &mut document,
        "raw-reporting",
        &plans::generous(),
        provenant_config::PortTable::builtin().worktree_layout,
    )
    .unwrap();
    assert_eq!(document.nodes[0].extra["template"], "raw");
    assert_eq!(document.extra["submission_patch"]["nodes"][0]["use"], "raw");
    assert_eq!(
        document.extra["submission_patch"]["nodes"][0]["extensions"]["template"],
        "raw"
    );
}

#[test]
fn planner_writer_without_worktree_is_provisioned_before_launch_and_replayed() {
    let doc = planner(1);
    let mut writer = slice("write", &["src"]);
    writer["access"] = json!("worktree_write");
    let output = result(vec![writer]);
    let mut owner = fixture_owner(&[&output, "written", "checked"]);
    let mut journal = MemoryJournal::default();
    let run = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(
        run.status,
        GraphStatus::Complete,
        "{:?}",
        run.checkpoint.pause_reason()
    );
    assert_eq!(
        owner.provisioned,
        vec![(".worktrees/planned-plan--write".into(), "a".repeat(40))]
    );
    assert_eq!(
        owner.permissions[1].1.as_deref(),
        Some("/fixture/.worktrees/planned-plan--write")
    );
    assert!(relay::pending_provisioning(&run.checkpoint).is_empty());
    let launches = owner.calls.len();
    let replay = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(replay.status, GraphStatus::Complete);
    assert_eq!(owner.calls.len(), launches);
    assert_eq!(owner.provisioned.len(), 1);
}

#[test]
fn planner_writer_provisioning_recovers_fenced_source_after_crash() {
    let doc = planner(1);
    let mut writer = slice("write", &["src"]);
    writer["access"] = json!("worktree_write");
    let output = result(vec![writer]);
    let mut owner = fixture_owner(&[&output, "written", "checked"]);
    owner.provision_fail_once = true;
    let mut journal = MemoryJournal::default();
    assert!(matches!(
        advance(&doc, &mut owner, &mut journal),
        Err(GraphError::Run(_))
    ));
    let saved = journal.0.as_ref().unwrap();
    assert_eq!(
        relay::pending_provisioning(saved),
        vec![(
            "plan--write".into(),
            ".worktrees/planned-plan--write".into(),
            "a".repeat(40)
        )]
    );
    owner.implicit_head = Some("b".repeat(40));
    let recovered = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(recovered.status, GraphStatus::Complete);
    assert_eq!(owner.provisioned.len(), 2);
    assert!(
        owner
            .provisioned
            .iter()
            .all(|(_, source)| source == &"a".repeat(40))
    );
    assert!(relay::pending_provisioning(&recovered.checkpoint).is_empty());
}

#[test]
fn pending_writer_continuation_reopens_and_excludes_its_predecessor_model() {
    let doc = writer_graph();
    let mut owner = FakeOwner {
        observations: [
            terminal_from(STOP, "stopped-writer"),
            AttemptObservation::Pending {
                attempt_ref: Some("fresh-writer".into()),
            },
        ]
        .into(),
        models: [("stopped-writer".into(), "codex/previous".into())].into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(owner.prior_attempts[1], ["codex/previous"]);
    assert_eq!(owner.stored_refs[1], None);
    assert_eq!(owner.calls[1].0.node_id, "write");
    let saved = serde_json::to_vec(&run.checkpoint).unwrap();
    journal.0 = Some(serde_json::from_slice(&saved).unwrap());
    owner.observations.extend([
        terminal_with_result(AttemptOutcome::Succeeded, "VERDICT: PASS"),
        terminal_with_result(AttemptOutcome::Succeeded, "checked"),
    ]);
    let reopened = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(reopened.status, GraphStatus::Complete);
    assert_eq!(owner.prior_attempts[2], ["codex/previous"]);
    assert_eq!(owner.calls[1].0, owner.calls[2].0);
}

#[test]
fn declared_writer_stop_restarts_with_handoff_and_excludes_the_used_pool_model() {
    let doc = writer_graph();
    let mut observation = terminal_from(STOP, "stopped-writer");
    if let AttemptObservation::Terminal { outcome, .. } = &mut observation {
        *outcome = AttemptOutcome::DeclaredStop;
    }
    let mut owner = FakeOwner {
        observations: [
            observation,
            terminal_with_result(AttemptOutcome::Succeeded, "VERDICT: PASS"),
            terminal_with_result(AttemptOutcome::Succeeded, "checked"),
        ]
        .into(),
        models: [("stopped-writer".into(), "codex/previous".into())].into(),
        ..Default::default()
    };
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert_eq!(owner.calls.len(), 3);
    assert_ne!(owner.calls[0].0, owner.calls[1].0);
    assert_eq!(owner.stored_refs[1], None);
    assert_eq!(owner.prior_attempts[1], ["codex/previous"]);
    assert!(owner.prompts[1].contains("First half committed"));
    assert_eq!(owner.permissions[0], owner.permissions[1]);
}
