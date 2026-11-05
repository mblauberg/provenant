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
        let run = advance(&planner(1), &mut fixture_owner(&[&output]), &mut MemoryJournal::default()).unwrap();
        assert!(run.checkpoint.expansions.is_empty());
        assert_eq!(run.checkpoint.pause_code.as_deref(), Some("invalid_plan_expansion"));
        assert!(run.checkpoint.pause_reason.as_deref().unwrap().contains("computed topology fields are reserved"));
    }
}

#[test]
fn admission_planner_fragment_capability() {
    let fragment = json!({
        "schema_version": 1,
        "nodes": [{
            "id": "child", "access": "read_only", "goal": "Do a slice",
            "acceptance_checks": ["Check it"], "route": {"alias": "senior"},
            "family_group": "group"
        }]
    });
    let output = format!("```fragment\n{}\n```", fragment);
    let mut owner = fixture_owner(&[&output]);
    let run = advance(&planner(4), &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert_eq!(run.checkpoint.pause_code.as_deref(), Some("invalid_plan_expansion"));
    assert!(run.checkpoint.pause_reason.as_deref().unwrap().contains("schema version"), "{:?}", run.checkpoint.pause_reason);
    assert!(run.checkpoint.expansions.is_empty());
}

#[test]
fn raw_expansion_rejects_each_injected_topology_field() {
    let doc = planner(2);
    let output = result(vec![slice("child", &[])]);
    let mut owner = fixture_owner(&[&output]);
    owner.observations.push_back(AttemptObservation::Pending { attempt_ref: None });
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
    for (fallback, legacy) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut doc = planner(8);
        let leaves: Vec<_> = (0..4).map(|i| json!({
            "id": format!("lens{i}"), "access":"read_only", "prompt":"Review",
            "route":{"alias":"senior"}, "acceptance_checks":["Review"]
        })).collect();
        let exports: Vec<_> = (0..4).map(|i| json!({"node":format!("lens{i}"),"outputs":["result"]})).collect();
        let planner_result = if fallback {
            doc.nodes[0].plan.as_mut().unwrap().fallback = Some(serde_json::from_value(json!({
                "nodes":leaves, "exports":exports, "min_successful":1
            })).unwrap());
            "invalid fragment".to_owned()
        } else {
            doc.nodes[0].plan.as_mut().unwrap().patterns = vec![serde_json::from_value(json!({
                "name":"lens", "nodes":[leaves[0]], "exports":[exports[0]]
            })).unwrap()];
            format!("```fragment\n{}\n```", json!({"schema_version":1,"nodes":[],"patterns":
                (0..4).map(|i| json!({"id":format!("lens{i}"),"pattern":"lens"})).collect::<Vec<_>>()
            }))
        };
        let mut owner = fixture_owner(&[&planner_result]);
        owner.observations.extend((0..4).map(|_| AttemptObservation::Pending { attempt_ref: None }));
        let mut journal = MemoryJournal::default();
        let pending = advance(&doc, &mut owner, &mut journal).unwrap();
        assert_eq!(pending.status, GraphStatus::Pending);
        assert_eq!(pending.checkpoint.expansions.len(), 1);
        assert_eq!(pending.checkpoint.nodes.iter().find(|n| n.id == "after").unwrap().state, NodeState::Pending);
        let mut wire = serde_json::to_value(&pending.checkpoint).unwrap();
        if !legacy {
            let nodes = wire["expansions"][0]["nodes"].as_array().unwrap();
            let boundary = nodes.iter().position(|node| node.get("composite").is_some()).unwrap();
            let member = nodes.iter().position(|node| node.get("composite").is_none()).unwrap();
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
        }
        if legacy {
            for node in wire["expansions"][0]["nodes"].as_array_mut().unwrap() {
                node.as_object_mut().unwrap().remove("composite_members");
                node.as_object_mut().unwrap().remove("after_expansion");
            }
        }
        journal.0 = Some(serde_json::from_value(wire).unwrap());
        let valid = journal.0.clone().unwrap();
        let boundary = valid.nodes.iter().position(|n| !n.children.is_empty()).unwrap();
        journal.0.as_mut().unwrap().nodes[boundary].children.clear();
        let error = advance(&doc, &mut fixture_owner(&[]), &mut journal).unwrap_err();
        assert_eq!(error.code(), "bad_checkpoint");
        assert!(error.to_string().contains(&valid.nodes[boundary].id), "{error}");
        assert!(error.to_string().contains("composite children must match expanded plan membership"), "{error}");
        journal.0 = Some(valid);
        let mut owner = FakeOwner { observations: [
            terminal_with_result(AttemptOutcome::Succeeded, "review passed"),
            terminal(AttemptOutcome::OtherFailure),
            terminal(AttemptOutcome::UsageLimited),
            terminal(AttemptOutcome::UsageLimited),
        ].into(), ..Default::default() };
        let run = if legacy { resume(&doc, &mut owner, &mut journal) }
            else { advance(&doc, &mut owner, &mut journal) }.unwrap();
        assert_eq!(run.status, GraphStatus::Complete, "fallback={fallback}, legacy={legacy}");
        assert!(run.checkpoint.nodes.iter().all(|n| n.state.settled()));
        assert_eq!(run.checkpoint.nodes.iter().find(|n| n.id == "after").unwrap().state, NodeState::Blocked);
        assert!(run.checkpoint.nodes.iter().any(|n| n.advance_error_code.as_deref() == Some("usage_limited")));
        journal.0 = Some(serde_json::from_slice(&serde_json::to_vec(&run.checkpoint).unwrap()).unwrap());
        assert_eq!(advance(&doc, &mut fixture_owner(&[]), &mut journal).unwrap().checkpoint, run.checkpoint);
    }
}

fn planner(max_nodes: usize) -> GraphDocument {
    serde_json::from_value(json!({
        "schema_version":1, "graph_id":"planned", "nodes":[
            {"id":"plan", "kind":"plan", "access":"read_only", "prompt":"Slice the work",
             "route":{"alias":"senior"}, "plan":{"kinds":["task", "plan", "gate"], "max_nodes":max_nodes}},
            {"id":"after", "access":"read_only", "prompt":"Check all slices", "depends_on":["plan"], "route":{"alias":"senior"}}
        ]
    })).unwrap()
}

fn slice(id: &str, scope: &[&str]) -> Value {
    json!({"id":id, "goal":"Do a slice", "write_scope":scope,
           "acceptance_checks":["Verify the slice"], "access":"read_only"})
}

fn result(nodes: Vec<Value>) -> String {
    format!("```fragment\n{}\n```", json!({"schema_version":1,"nodes":nodes}))
}

fn pattern_definition() -> Value {
    json!({
        "name": "generator",
        "parameters": {
            "label": {"type": "string", "default": "the slice"},
            "scope": {"type": "list", "default": ["src"]},
            "retries": {"type": "integer", "default": 1, "min": 0, "max": 2}
        },
        "nodes": [
            {"id": "write", "goal": "Do ${label}", "access": "worktree_write",
             "worktree": ".worktrees/impl", "write_scope": "${scope}",
             "acceptance_checks": ["Verify the slice"]},
            {"id": "review", "goal": "Review the work", "access": "read_only",
             "depends_on": ["write"], "write_scope": ["src"],
             "acceptance_checks": ["Verify the review"]}
        ],
        "exports": [{"node": "review", "outputs": ["result", "findings"]}]
    })
}

fn pattern_planner(max_nodes: usize) -> GraphDocument {
    serde_json::from_value(json!({
        "schema_version":1, "graph_id":"planned-patterns", "nodes":[
            {"id":"plan", "kind":"plan", "access":"read_only", "prompt":"Slice the work",
             "route":{"alias":"senior"}, "plan":{"kinds":["task"], "max_nodes":max_nodes,
             "patterns":[pattern_definition()]}},
            {"id":"after", "access":"read_only", "prompt":"Check all slices",
             "depends_on":["plan"], "route":{"alias":"senior"}}
        ]
    })).unwrap()
}

#[test]
fn failed_planner_runs_static_fallback_and_records_reason() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1, "graph_id":"planned-fallback", "nodes":[
            {"id":"before", "access":"read_only", "prompt":"Prepare inputs", "route":{"alias":"junior"}},
            {"id":"plan", "kind":"plan", "access":"read_only", "prompt":"Plan",
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
            terminal_with_result(AttemptOutcome::TaskFailed, "planner rejected the brief"),
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
    assert!(owner.prompts[2].contains("prepared result"));
    assert!(owner.prompts[3].contains("static result"));
    let plan = run.checkpoint.nodes.iter().find(|node| node.id == "plan").unwrap();
    assert_eq!(plan.state, NodeState::Succeeded);
    assert_eq!(
        serde_json::to_value(plan).unwrap()["outputs"]["plan"]["fallback_reason"],
        "planner attempt failed (TaskFailed): planner rejected the brief"
    );
    let summary = plan.deterministic_summary().unwrap();
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
    assert_eq!(run.status, GraphStatus::Complete, "{:?}", run.checkpoint.pause_reason);
    assert!(run.checkpoint.successful());
    assert_eq!(owner.calls.len(), 4);
    assert!(owner.calls[1].0.node_id.contains("plan--fallback--first"));
    let plan = run.checkpoint.nodes.iter().find(|node| node.id == "plan").unwrap();
    let reason = &plan.outputs.as_ref().unwrap().plan.as_ref().unwrap().fallback_reason;
    assert!(reason.contains("planner fragment invalid: invalid fragment"), "{reason}");
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
    assert_eq!(run.status, GraphStatus::Complete, "{:?}", run.checkpoint.pause_reason);
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
            {"id":"plan", "kind":"plan", "access":"read_only", "prompt":"Plan",
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
    let ids: Vec<_> = first_owner.calls.iter().map(|call| call.0.node_id.as_str()).collect();
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert!(ids[1].ends_with("--fallback--first"), "{ids:?}");
    let state = |run: &GraphRun, suffix: &str| {
        run.checkpoint.nodes.iter().find(|node| node.id.ends_with(suffix)).map(|node| node.state)
    };
    assert_eq!(state(&interrupted, "--fallback--second"), Some(NodeState::Pending));
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
    let ids: Vec<_> = second_owner.calls.iter().map(|call| call.0.node_id.as_str()).collect();
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
    assert!(interrupted.checkpoint.nodes.iter().find(|node| node.id == "plan").unwrap()
        .outputs.as_ref().unwrap().plan.as_ref().unwrap().fallback_reason.contains("invalid fragment"));

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
    assert_eq!(run.status, GraphStatus::Complete, "{:?}", run.checkpoint.pause_reason);
    assert!(run.checkpoint.successful());
    assert_eq!(run.checkpoint.expansions.len(), 1);
    let ids: Vec<_> = second_owner.calls.iter().map(|call| call.0.node_id.as_str()).collect();
    assert_eq!(ids, ["plan--fallback--first", "plan--fallback--second", "after"]);
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
    assert!(matches!(result, Err(GraphError::BadCheckpoint { .. })), "{:?}", result.map(|run| run.status));
}

#[test]
fn fallback_expansion_over_a_structural_limit_pauses_with_a_reason() {
    let document = two_step_fallback_document();
    let limits = crate::config::GraphLimits { nodes: 3, ..Default::default() };
    let mut owner = fixture_owner(&["planner rejected the brief"]);
    let mut owner = {
        owner.observations =
            [terminal_with_result(AttemptOutcome::TaskFailed, "planner rejected the brief")].into();
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
    assert_eq!(run.checkpoint.pause_code.as_deref(), Some("invalid_plan_expansion"));
    let reason = run.checkpoint.pause_reason.as_deref().unwrap();
    assert!(reason.contains("plan fallback expansion paused"), "{reason}");
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
    let reason = paused.checkpoint.pause_reason.as_deref().unwrap();
    assert!(reason.contains("planner fragment invalid: fragment block missing"), "{reason}");
    assert!(reason.contains("fallback expansion paused: limits.nodes=3"), "{reason}");
    assert!(reason.find("fragment block missing") < reason.find("fallback expansion paused"));
    assert!(paused
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "plan")
        .unwrap()
        .fragment
        .is_none());
    assert!(paused.checkpoint.expansions.is_empty());

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
    assert_eq!(resumed.status, GraphStatus::Complete, "{:?}", resumed.checkpoint.pause_reason);
    assert!(resumed.checkpoint.successful());
    assert_eq!(resumed.checkpoint.expansions.len(), 1);
    let plan = resumed
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "plan")
        .unwrap();
    assert!(plan
        .outputs
        .as_ref()
        .unwrap()
        .plan
        .as_ref()
        .unwrap()
        .fallback_reason
        .contains("fragment block missing"));
    let ids: Vec<_> = owner.calls.iter().map(|call| call.0.node_id.as_str()).collect();
    assert_eq!(ids, ["plan--fallback--first", "plan--fallback--second", "after"]);
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
    assert_eq!(advanced.status, GraphStatus::Complete, "{:?}", advanced.checkpoint.pause_reason);
    assert!(advanced.checkpoint.successful());
    assert_eq!(advanced.checkpoint.expansions.len(), 1);
}

#[test]
fn planner_prelaunch_failure_runs_static_fallback_without_an_attempt() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1, "graph_id":"planned-fallback-prelaunch", "nodes":[
            {"id":"before", "access":"read_only", "prompt":"Prepare", "route":{"alias":"junior"}},
            {"id":"plan", "kind":"plan", "access":"read_only", "prompt":"Plan",
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
    assert_eq!(run.status, GraphStatus::Complete, "{:?}", run.checkpoint.pause_reason);
    assert_eq!(owner.calls.len(), 3, "{:?}", run.checkpoint.nodes);
    assert!(owner.calls[1].0.node_id.contains("plan--fallback--static"));
    assert_eq!(owner.calls[2].0.node_id, "after");
    assert!(owner.prompts[2].contains("static result"));
    let plan = run.checkpoint.nodes.iter().find(|node| node.id == "plan").unwrap();
    assert!(plan.attempts.is_empty());
    assert_eq!(plan.state, NodeState::Succeeded);
    assert_eq!(
        plan.outputs.as_ref().unwrap().plan.as_ref().unwrap().fallback_reason,
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
    FakeOwner { observations: results.iter().map(|text| terminal_with_result(AttemptOutcome::Succeeded, text)).collect(), ..Default::default() }
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
                assert_eq!(run.checkpoint.nodes[0].handoff.is_some(), genuine);
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
    for (field, cap) in [("nodes", 3), ("edges", 3), ("fan_out_width", 2), ("expansion_depth", 1)] {
        let mut doc = planner(2);
        doc.graph_id = format!("planned-{field}");
        let limits = serde_json::from_value(json!({field:cap})).unwrap();
        let mut first = slice("a", &["src"]);
        first["goal"] = json!("detail ".repeat(700));
        let output = result(vec![first, slice("b", &["docs"])]);
        let mut owner = fixture_owner(&[&output]);
        {
            let mut journal = FileJournal::open(&root, project, &doc.graph_id, budget, &cancel, &clock).unwrap();
            journal.bind(GraphExecution {review_base: None, extra: Default::default(),  integration_base: None, directory: base.clone(), document: doc.clone() }).unwrap();
            let run = advance_with_budget(&doc, &mut owner, &mut journal, &limits, &Default::default()).unwrap();
            assert_eq!(run.status, GraphStatus::Paused);
            assert!(run.checkpoint.nodes[0].outputs.as_ref().unwrap().result.as_ref().unwrap().truncated);
            assert!(run.checkpoint.nodes[0].fragment.as_ref().unwrap().len() > 4096);
        }
        owner.observations.extend(["a", "b", "checked"].map(|text| terminal_with_result(AttemptOutcome::Succeeded, text)));
        let mut journal = FileJournal::open(&root, project, &doc.graph_id, budget, &cancel, &clock).unwrap();
        let limits = serde_json::from_value(json!({field:cap * 2})).unwrap();
        let run = resume_with_budget(&doc, &mut owner, &mut journal, &limits, &Default::default(), &[]).unwrap();
        assert_eq!(run.status, GraphStatus::Complete, "{field}: {:?}", run.checkpoint.pause_reason);
        assert!(run.checkpoint.successful());
        assert_eq!(run.checkpoint.expansions.len(), 1);
        assert_eq!(owner.calls.len(), 4);
        assert_eq!(advance_with_budget(&doc, &mut owner, &mut journal, &limits, &Default::default()).unwrap().checkpoint, run.checkpoint);
    }
}

#[test]
fn malformed_optional_handoff_warns_and_still_delivers() {
    for output in ["```handoff\n{}\n```".to_owned(), format!("{STOP}\n{STOP}"), STOP.replace("stopped_early", "invalid"), STOP.trim_end_matches("\n```").to_owned(), format!("```handoff\n{}\n```", json!({"schema_version":1,"status":"stopped_early","note":"x".repeat(4097)}))] {
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
        assert!(run.checkpoint.nodes[0].outputs.as_ref().unwrap().warnings.iter().any(|w| w.contains("ignored optional handoff")));
        assert_eq!(owner.deliveries, 1);
        assert_eq!(advance(&doc, &mut owner, &mut journal).unwrap().checkpoint, run.checkpoint);
    }
}

#[test]
fn original_writers_fence_overlapping_and_unspecified_scopes() {
    for scope in [vec!["src".into()], Vec::new(), vec!["./src".into()], vec!["src/*".into()]] {
        let mut first = writer_graph().nodes.remove(0);
        first.required_verdict = None;
        first.write_scope = scope;
        let mut second = first.clone(); second.id = "second".into();
        second.worktree = Some(".worktrees/second".into()); second.write_scope = vec!["src/a".into()];
        let doc = graph(vec![first, second]);
        let mut owner = FakeOwner::default();
        owner.observations.push_back(AttemptObservation::Pending { attempt_ref: Some("first".into()) });
        let mut journal = MemoryJournal::default();
        let run = advance(&doc, &mut owner, &mut journal).unwrap();
        assert_eq!(owner.calls.len(), 1);
        assert_eq!(run.checkpoint.nodes[1].state, NodeState::Pending);
        owner.observations.extend(["done", "done"].map(|text| terminal_with_result(AttemptOutcome::Succeeded, text)));
        assert!(advance(&doc, &mut owner, &mut journal).unwrap().checkpoint.successful());
    }
}

#[test]
fn skipped_writer_does_not_connect_separate_checkouts_during_expansion() {
    let mut doc = planner(1);
    doc.integration_branch = Some("integration".into());
    for (id, scope) in [("skipped", "src/a"), ("active", "src/b")] {
        let mut writer = writer_graph().nodes.remove(0);
        writer.id = id.into(); writer.worktree = Some(format!(".worktrees/{id}"));
        writer.write_scope = vec![scope.into()]; writer.required_verdict = None;
        doc.nodes.push(writer);
    }
    let (plan, _) = validate_plan(&doc).unwrap();
    let mut owner = fixture_owner(&["invalid"]);
    let mut journal = MemoryJournal::default();
    let mut checkpoint = advance(&planner(1), &mut owner, &mut journal).unwrap().checkpoint;
    loops::reconcile(&mut checkpoint, &plan);
    checkpoint.nodes.iter_mut().find(|n| n.id == "skipped").unwrap().state = NodeState::Skipped;
    let planner = plan.nodes.iter().find(|n| n.id == "plan").unwrap();
    let mut child = slice("required", &["src"]); child["access"] = json!("worktree_write"); child["worktree"] = json!(".worktrees/new");
    assert!(plans::settle(planner, &doc, &doc, &mut checkpoint, Some(&capture(&result(vec![child]))), &Default::default()).is_some(), "{:?}", checkpoint.pause_reason);
    assert_eq!(checkpoint.expansions[0].nodes[0].worktree.as_deref(), Some(".worktrees/active"));
}

#[test]
fn planner_prompt_uses_wire_kinds_and_required_field_names() {
    let prompt = plans::prompt(&planner(2).nodes[0]);
    assert!(prompt.contains(r#"["task","plan","gate"]"#));
    assert!(prompt.contains("`worktree`"));
    assert!(prompt.contains("`inputs`"));
    assert!(prompt.contains("`acceptance_checks`"));
    assert!(prompt.contains(r#"{"node":"id","outputs":["result"]}"#));
    assert!(prompt.contains(r#""id"` (which selects `result`)"#));
    assert!(prompt.contains("Do not emit presets or review shapes"));
}

#[test]
fn planner_prompt_names_only_the_nodes_allow_listed_patterns() {
    let mut doc = pattern_planner(4);
    let parameters = &mut doc.nodes[0].plan.as_mut().unwrap().patterns[0].parameters;
    parameters.get_mut("label").unwrap().default = None;
    parameters.get_mut("retries").unwrap().allowed = vec![json!(1), json!(2)];
    let prompt = plans::prompt(&doc.nodes[0]);
    assert!(prompt.contains("generator"));
    assert!(!prompt.contains("not-allow-listed-pattern"));
}

#[test]
fn planner_requires_write_scope_only_for_writers() {
    let doc = planner(1);
    let reader = json!({"id":"read","goal":"Read","access":"read_only","acceptance_checks":["check"]});
    let mut owner = fixture_owner(&[&result(vec![reader]), "read", "checked"]);
    let mut journal = MemoryJournal::default();
    let run = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(run.checkpoint.expansions.len(), 1, "{:?}", run.checkpoint.pause_reason);
    assert_eq!(run.status, GraphStatus::Complete);

    let writer = json!({"id":"write","goal":"Write","access":"worktree_write","worktree":".worktrees/write","acceptance_checks":["check"]});
    let mut owner = fixture_owner(&[&result(vec![writer])]);
    let mut journal = MemoryJournal::default();
    let run = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert_eq!(run.checkpoint.pause_code.as_deref(), Some("invalid_plan_expansion"));
    assert!(run.checkpoint.expansions.is_empty());
}

#[test]
fn continuation_rejects_original_pr_delivery_before_integration_rewrite() {
    let mut doc = writer_graph();
    doc.integration_branch = Some("integration".into());
    doc.nodes[0].delivery = Some(provenant_records::run::DeliveryMode::Pr);
    let run = advance(&doc, &mut fixture_owner(&[STOP]), &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert_eq!(run.checkpoint.pause_code.as_deref(), Some("invalid_plan_expansion"));
    assert!(run.checkpoint.expansions.is_empty());
}

#[test]
fn oversized_nested_fragment_id_names_the_source_and_child() {
    let mut doc = planner(1);
    doc.nodes[0].id = "p".repeat(40);
    doc.nodes.truncate(1);
    let name = "c".repeat(23);
    let run = advance(&doc, &mut fixture_owner(&[&result(vec![slice(&name, &["src"])])]), &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.checkpoint.pause_code.as_deref(), Some("invalid_plan_expansion"));
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
        (result(vec![slice("a", &["src"]), slice("b", &["docs"])]), "1..1 nodes"),
        ("```fragment\n{not json}\n```".into(), "invalid fragment"),
        ("plain prose".into(), "fragment block missing"),
        (result(vec![cyclic]), "dependency"),
        (result(vec![undeclared]), "not declared"),
        (result(vec![no_checks]), "acceptance_checks"),
        (result(vec![slice("escape", &["../other"])]), "write_scope"),
    ] {
        let mut owner = fixture_owner(&[&output]);
        let mut journal = MemoryJournal::default();
        let run = advance(&planner(1), &mut owner, &mut journal).unwrap();
        assert_eq!(run.status, GraphStatus::Paused, "{output}");
        assert_eq!(run.checkpoint.pause_code.as_deref(), Some("invalid_plan_expansion"), "{reason}");
        assert!(run.checkpoint.expansions.is_empty());
        assert_eq!(owner.calls.len(), 1);
        assert_eq!(advance(&planner(1), &mut owner, &mut journal).unwrap().status, GraphStatus::Paused);
        assert_eq!(resume(&planner(1), &mut owner, &mut journal).unwrap().status, GraphStatus::Paused);
    }
}

#[test]
fn plan_validates_host_limits_before_saving() {
    let output = result(vec![slice("a", &["src"]), slice("b", &["docs"])]);
    for (field, cap) in [("nodes", 3), ("edges", 3), ("fan_out_width", 2), ("expansion_depth", 1)] {
        let mut doc = planner(2);
        doc.limits = Some(serde_json::from_value(json!({field: 999})).unwrap());
        let limits = serde_json::from_value(json!({field:cap})).unwrap();
        let mut owner = fixture_owner(&[&output]);
        let mut journal = MemoryJournal::default();
        let run = advance_with_budget(&doc, &mut owner, &mut journal, &limits, &Default::default()).unwrap();
        assert_eq!(run.status, GraphStatus::Paused);
        assert_eq!(run.checkpoint.pause_code.as_deref(), Some("invalid_plan_expansion"));
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
    let run = advance(&pattern_planner(4), &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Complete, "{:?}", run.checkpoint.pause_reason);
    assert!(run.checkpoint.successful());

    let expansion = &run.checkpoint.expansions[0];
    assert_eq!(expansion.patterns.len(), 1);
    let recorded = &expansion.patterns[0];
    assert_eq!(recorded.id, "plan--impl");
    assert_eq!(recorded.name, "generator");
    assert_eq!(
        recorded.parameters,
        json!({"label":"the slice", "scope":["src/a"], "retries":1})
    );
    assert_eq!(
        recorded.nodes,
        vec!["plan--impl--review", "plan--impl--write"]
    );

    let write_prompt = "Do the slice\n\nWrite scope: src/a\nAcceptance checks:\nVerify the slice";
    let review_prompt =
        "Review the work\n\nWrite scope: src\nAcceptance checks:\nVerify the review";
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
        let run = advance(&pattern_planner(4), &mut owner, &mut MemoryJournal::default()).unwrap();
        assert_eq!(run.status, GraphStatus::Paused, "{reason}");
        assert_eq!(run.checkpoint.pause_code.as_deref(), Some("invalid_plan_expansion"), "{reason}");
        assert!(run.checkpoint.expansions.is_empty());
        assert_eq!(owner.calls.len(), 1);
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
    assert_eq!(run.checkpoint.pause_code.as_deref(), Some("invalid_plan_expansion"));
    assert!(run.checkpoint.expansions.is_empty());
}

#[test]
fn pattern_expansion_counts_against_the_graph_node_limit() {
    let instance = json!({"id":"impl", "pattern":"generator"});
    let limits = crate::config::GraphLimits { nodes: 3, ..Default::default() };
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
    assert_eq!(run.checkpoint.pause_code.as_deref(), Some("invalid_plan_expansion"));
    assert!(run.checkpoint.expansions.is_empty());
}

fn assert_pattern_parameter_error(
    case: &str,
    doc: &GraphDocument,
    parameters: Value,
    reason: &str,
) {
    let mut owner = fixture_owner(&[&pattern_result(json!({"id":"impl", "pattern":"generator", "parameters":parameters}))]);
    let run = advance(doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Paused, "{case}");
    assert_eq!(run.checkpoint.pause_code.as_deref(), Some("invalid_plan_expansion"), "{case}");
    assert!(run.checkpoint.pause_reason.as_deref().unwrap_or_default().contains(reason), "{case}: {:?}", run.checkpoint.pause_reason);
    assert!(run.checkpoint.expansions.is_empty(), "{case}");
}

#[test]
fn pattern_parameter_errors_are_refused() {
    let mut missing_required = pattern_planner(4);
    missing_required.nodes[0].plan.as_mut().unwrap().patterns[0].parameters.get_mut("label").unwrap().default = None;
    let mut enum_violation = pattern_planner(4);
    enum_violation.nodes[0].plan.as_mut().unwrap().patterns[0].parameters.get_mut("retries").unwrap().allowed = vec![json!(1)];

    for (case, doc, parameters, reason) in [
        ("missing required parameter", missing_required, json!({}), "requires parameter label"),
        ("unknown parameter", pattern_planner(4), json!({"unknown":1}), "no parameter unknown"),
        ("non-table parameters", pattern_planner(4), json!([]), "parameters must be a table"),
        ("enum violation", enum_violation, json!({"retries":2}), "must be one of"),
    ] {
        assert_pattern_parameter_error(case, &doc, parameters, reason);
    }
}

#[test]
fn pattern_template_authority_and_types_are_validated() {
    for (field, value) in [("access", json!("${label}")), ("role", json!("${label}")), ("write_scope", json!("${label}"))] {
        let mut doc = pattern_planner(4);
        doc.nodes[0].plan.as_mut().unwrap().patterns[0].nodes[0][field] = value;
        assert!(plans::validate_spec(&doc.nodes[0]).is_err(), "{field}");
    }
}

#[test]
fn pattern_required_sample_does_not_violate_bounds_at_admission() {
    let mut doc = pattern_planner(4);
    let parameters = &mut doc.nodes[0].plan.as_mut().unwrap().patterns[0].parameters;
    let retries = parameters.get_mut("retries").unwrap();
    retries.default = None;
    retries.min = Some(json!(5));
    retries.max = Some(json!(10));
    assert!(plans::validate_spec(&doc.nodes[0]).is_ok());
}

#[test]
fn pattern_children_obey_plan_node_and_kind_limits() {
    let instance = json!({"id":"impl", "pattern":"generator"});
    let mut doc = pattern_planner(4);
    doc.nodes[0].plan.as_mut().unwrap().kinds = vec![NodeKind::Gate];
    let mut owner = fixture_owner(&[&pattern_result(instance)]);
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.checkpoint.pause_code.as_deref(), Some("invalid_plan_expansion"));
}

#[test]
fn pattern_expansion_respects_plan_max_nodes_boundary() {
    let output = pattern_result(json!({"id":"impl", "pattern":"generator"}));
    let mut owner = fixture_owner(&[&output, "implemented", "reviewed", "checked"]);
    let run = advance(&pattern_planner(1), &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Complete, "{:?}", run.checkpoint.pause_reason);
    assert_eq!(run.checkpoint.expansions[0].nodes.len(), 3);

    let output = pattern_result_all(vec![json!({"id":"a", "pattern":"generator"}), json!({"id":"b", "pattern":"generator"})]);
    let run = advance(&pattern_planner(1), &mut fixture_owner(&[&output]), &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert_eq!(run.checkpoint.pause_code.as_deref(), Some("invalid_plan_expansion"));
    assert!(run.checkpoint.expansions.is_empty());

    let output = pattern_result(json!({"id":"impl", "pattern":"generator"}));
    let limits = crate::config::GraphLimits { nodes: 4, ..Default::default() };
    let run = advance_with_budget(&pattern_planner(1), &mut fixture_owner(&[&output]), &mut MemoryJournal::default(), &limits, &Default::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert_eq!(run.checkpoint.pause_code.as_deref(), Some("invalid_plan_expansion"));
    assert!(run.checkpoint.pause_reason.as_deref().unwrap().contains("limits.nodes"));
    assert!(run.checkpoint.expansions.is_empty());
}

#[test]
fn plan_overlapping_writers_share_one_worktree_integrate_and_deliver_once() {
    let mut doc = planner(3);
    doc.integration_branch = Some("integration".into());
    doc.delivery = Some(provenant_records::run::DeliveryMode::Pr);
    let nodes = [("a", vec!["src/a"]), ("b", vec!["src/b"]), ("both", vec!["src"])].into_iter().map(|(id, scope)| {
        let mut node = slice(id, &scope);
        node["access"] = json!("worktree_write");
        node["worktree"] = json!(format!(".worktrees/{id}"));
        node
    }).collect();
    let output = result(nodes);
    let mut owner = fixture_owner(&[&output]);
    owner.observations.push_back(AttemptObservation::Pending { attempt_ref: Some("a".into()) });
    let mut journal = MemoryJournal::default();
    let partial = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(partial.checkpoint.nodes.iter().filter(|n| n.state == NodeState::Running).count(), 1);
    assert_eq!(owner.calls.len(), 2);
    assert_eq!(owner.deliveries, 0);
    journal.0 = Some(serde_json::from_slice(&serde_json::to_vec(&partial.checkpoint).unwrap()).unwrap());
    owner.observations.extend(["a done", "b done", "both done", "checked"].map(|text| terminal_with_result(AttemptOutcome::Succeeded, text)));
    let run = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(run.checkpoint.successful());
    assert_eq!(owner.deliveries, 1);
    assert!(owner.permissions[1..5].iter().all(|p| *p == (AccessMode::WorktreeWrite, Some(".worktrees/a".into()))));
    assert!(owner.delivery_modes[1..5].iter().all(|mode| *mode == Some(provenant_records::run::DeliveryMode::Integrate("integration".into()))));
    let encoded = serde_json::to_vec(&run.checkpoint).unwrap();
    journal.0 = Some(serde_json::from_slice(&encoded).unwrap());
    let replay = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(run.checkpoint, replay.checkpoint);
    assert_eq!(owner.deliveries, 1);
}

#[test]
fn planner_integrate_delivery_needs_the_documents_integration_branch() {
    let mut writer = slice("w", &["src"]);
    writer["access"] = json!("worktree_write");
    writer["worktree"] = json!(".worktrees/w");
    writer["delivery"] = json!("integrate:origin/release/1.3");
    let output = result(vec![writer]);
    let run = advance(&planner(1), &mut fixture_owner(&[&output]), &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert_eq!(run.checkpoint.pause_code.as_deref(), Some("invalid_plan_expansion"));
    assert!(run.checkpoint.expansions.is_empty());

    // With one, the destination comes from the document, never the planner.
    let mut doc = planner(1);
    doc.integration_branch = Some("integration".into());
    let mut owner = fixture_owner(&[&output]);
    owner.observations.push_back(AttemptObservation::Pending { attempt_ref: Some("w".into()) });
    advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(owner.delivery_modes[1], Some(provenant_records::run::DeliveryMode::Integrate("integration".into())));
}

#[test]
fn plan_disjoint_writers_are_admitted_while_a_peer_is_running() {
    let mut doc = planner(2);
    doc.integration_branch = Some("integration".into());
    let nodes = ["a", "b"].into_iter().map(|id| {
        let mut node = slice(id, &[&format!("src/{id}")]);
        node["access"] = json!("worktree_write");
        node["worktree"] = json!(format!(".worktrees/{id}"));
        node
    }).collect();
    let output = result(nodes);
    let mut owner = fixture_owner(&[&output]);
    owner.observations.extend([AttemptObservation::Pending { attempt_ref: Some("a".into()) }, AttemptObservation::Pending { attempt_ref: Some("b".into()) }]);
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Pending);
    assert_eq!(owner.calls.len(), 3);
    assert_ne!(owner.permissions[1].1, owner.permissions[2].1);
    assert_eq!(run.checkpoint.nodes.iter().filter(|n| n.state == NodeState::Running).count(), 2);
}

fn writer_graph() -> GraphDocument {
    let mut writer = node("write", route("fixture"));
    writer.access = AccessMode::WorktreeWrite;
    writer.worktree = Some(".worktrees/writer".into());
    writer.required_verdict = Some("VERDICT: PASS".into());
    let mut after = node("after", route("fixture"));
    after.depends_on = vec!["write".into()];
    graph(vec![writer, after])
}

#[test]
fn honest_stop_continues_once_on_same_checkout_with_handoff_and_verdict_contract() {
    let mut doc = writer_graph();
    doc.nodes[1].inputs = vec![GraphInput {extra: Default::default(),  node: "write".into(), outputs: vec![OutputKind::Result] }];
    let mut owner = fixture_owner(&[STOP, "VERDICT: PASS", "checked"]);
    let mut journal = MemoryJournal::default();
    let run = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert_eq!(run.checkpoint.expansions.len(), 1);
    assert_eq!(owner.permissions[0], owner.permissions[1]);
    assert_eq!(owner.calls[1].0.node_id, "write--continue");
    assert_eq!(owner.calls[2].0.node_id, "after");
    assert!(owner.prompts[2].contains("write--continue result:"));
    assert!(owner.prompts[1].contains("First half committed"));
    assert!(run.checkpoint.nodes[0].handoff.as_ref().unwrap().contains("First half committed"));
    assert_eq!(run.checkpoint.remaining.as_ref().unwrap().continuations, 1);
    assert_eq!(advance(&doc, &mut owner, &mut journal).unwrap().checkpoint, run.checkpoint);
    assert_eq!(owner.calls.len(), 3);
}

#[test]
fn continuation_budget_pauses_then_extends_without_duplicating_prior_work() {
    let doc = writer_graph();
    let mut limits = crate::config::GraphLimits { continuations: 1, ..Default::default() };
    let mut owner = fixture_owner(&[STOP, STOP]);
    let mut journal = MemoryJournal::default();
    let run = advance_with_budget(&doc, &mut owner, &mut journal, &limits, &Default::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert!(!run.checkpoint.successful());
    assert_eq!(run.checkpoint.expansions.len(), 1);
    assert_eq!(run.checkpoint.remaining.as_ref().unwrap().continuations, 0);
    assert_eq!(run.checkpoint.pause_code.as_deref(), Some("invalid_plan_expansion"));
    assert_eq!(owner.calls.len(), 2);
    assert_eq!(advance_with_budget(&doc, &mut owner, &mut journal, &limits, &Default::default()).unwrap().status, GraphStatus::Paused);
    owner.observations.extend(["VERDICT: PASS", "checked"].map(|text| terminal_with_result(AttemptOutcome::Succeeded, text)));
    limits.continuations = 2;
    let resumed = resume_with_budget(&doc, &mut owner, &mut journal, &limits, &Default::default(), &[]).unwrap();
    assert_eq!(resumed.status, GraphStatus::Complete);
    assert_eq!(resumed.checkpoint.expansions.len(), 2);
    assert_eq!(owner.calls.len(), 4);
    limits.continuations = 0;
    let mut owner = super::FakeOwner { observations: [terminal_with_result(AttemptOutcome::Succeeded, STOP)].into(), ..Default::default() };
    let disabled = advance_with_budget(&doc, &mut owner, &mut MemoryJournal::default(), &limits, &Default::default()).unwrap();
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
                    {"id":"plan","kind":"plan","prompt":"Plan","access":"read_only","route":{"alias":"senior"},"plan":{"kinds":["task"],"max_nodes":1}},
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
    assert!(run.checkpoint.nodes.iter().find(|n| n.id.ends_with("--slice")).unwrap().round.is_some());
    journal.0 = Some(serde_json::from_slice(&serde_json::to_vec(&run.checkpoint).unwrap()).unwrap());
    assert_eq!(advance(&doc, &mut owner, &mut journal).unwrap().checkpoint, run.checkpoint);
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
    let nodes = ["a", "b"].into_iter().map(|id| {
        let mut child = slice(id, &[&format!("src/{id}")]);
        child["access"] = json!("worktree_write");
        child["worktree"] = json!(format!(".worktrees/{id}"));
        child
    }).collect();
    let run = advance(&doc, &mut fixture_owner(&[&result(nodes)]), &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert_eq!(run.checkpoint.pause_code.as_deref(), Some("invalid_plan_expansion"));
    assert!(run.checkpoint.expansions.is_empty());
}

#[test]
fn nested_plan_rewires_all_consumers_and_keeps_original_digest() {
    let doc = planner(1);
    let mut nested = slice("nested", &["src"]);
    nested["kind"] = json!("plan");
    nested["plan"] = json!({"kinds":["task"],"max_nodes":1});
    let output = result(vec![nested]);
    let inner = result(vec![slice("inner", &["src"])]);
    let mut owner = fixture_owner(&[&output, &inner, "done", "checked"]);
    let mut journal = MemoryJournal::default();
    let run = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert_eq!(owner.calls[2].0.node_id, "plan--nested--inner");
    assert_eq!(owner.calls[3].0.node_id, "after");
    assert_eq!(run.checkpoint.expansions.len(), 2);
    let recursive: GraphCheckpoint = serde_json::from_str(include_str!("nested-plan-recursive.json")).unwrap();
    assert_eq!(run.checkpoint, recursive);
    let (base, _) = validate_plan(&doc).unwrap();
    assert_eq!(run.checkpoint.plan_digest, CanonicalV1::record_digest("fabric-graph-plan", 1, &base).unwrap().to_string());
}

#[test]
fn expansion_completes_at_node_budget() {
    let limits = crate::config::GraphLimits {
        nodes: 4,
        ..Default::default()
    };
    let count = limits.nodes / 2;
    let nodes = (0..count).map(|index| {
        let mut node = planner(1).nodes.remove(0);
        node.id = format!("plan{index}");
        node
    }).collect();
    let doc = graph(nodes);
    let output = result(vec![slice("child", &["src"])]);
    let mut owner = fixture_owner(&vec![output.as_str(); count]);
    owner.observations.extend((0..count).map(|_| terminal_with_result(AttemptOutcome::Succeeded, "done")));
    let mut journal = MemoryJournal::default();
    let run = advance_with_budget(&doc, &mut owner, &mut journal, &limits, &Default::default()).unwrap();
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
    assert_eq!(advance(&doc, &mut owner, &mut journal).unwrap().status, GraphStatus::Paused);
}

#[test]
fn structured_blocks_survive_real_result_head_truncation() {
    for repetitions in [800, 4000] {
    for (doc, block, completion) in [(planner(1), result(vec![slice("slice", &["src"])]), "done"), (writer_graph(), STOP.into(), "VERDICT: PASS")] {
        let output = format!("{}\n{block}", "detail ".repeat(repetitions));
        let mut owner = fixture_owner(&[&output, completion, "checked"]);
        if let Some(AttemptObservation::Terminal { result: Some(result), .. }) = owner.observations.front_mut() { result.source_truncated = true; }
        let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
        assert_eq!(run.status, GraphStatus::Complete);
        assert!(run.checkpoint.successful());
        assert_eq!(owner.calls.len(), 3);
    }
    }
}

#[test]
fn sibling_planners_share_overlapping_writer_checkout_and_wait() {
    let mut first = planner(1).nodes.remove(0); first.id = "first".into();
    let mut second = first.clone(); second.id = "second".into();
    let mut doc = graph(vec![first, second]); doc.integration_branch = Some("integration".into());
    let outputs: Vec<_> = ["a", "b"].into_iter().map(|id| {
        let mut child = slice(id, &["src"]); child["access"] = json!("worktree_write"); child["worktree"] = json!(format!(".worktrees/{id}")); result(vec![child])
    }).collect();
    let mut owner = fixture_owner(&[&outputs[0], &outputs[1]]);
    owner.observations.extend([AttemptObservation::Pending { attempt_ref: Some("a".into()) }, AttemptObservation::Pending { attempt_ref: Some("b".into()) }]);
    let mut journal = MemoryJournal::default();
    let run = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(owner.calls.len(), 3);
    assert_eq!(run.checkpoint.nodes.iter().filter(|n| n.state == NodeState::Running).count(), 1);
    assert_eq!(run.checkpoint.expansions[1].nodes[0].worktree.as_deref(), Some(".worktrees/a"));
    owner.observations.clear(); owner.observations.extend([terminal_with_result(AttemptOutcome::Succeeded, "done"), terminal_with_result(AttemptOutcome::Succeeded, "done")]);
    assert!(advance(&doc, &mut owner, &mut journal).unwrap().checkpoint.successful());
    assert_eq!(owner.permissions.last().unwrap().1.as_deref(), Some(".worktrees/a"));
}

#[test]
fn fragment_gate_recovery_is_a_completion_dependency_for_consumers() {
    for expected in ["VERDICT: PASS", "VERDICT: FAIL"] {
        let output = result(vec![slice("review", &["src"]), json!({"id":"gate","kind":"gate","access":"read_only","depends_on":["review"],"inputs":[{"node":"review","outputs":["verdict"]}],"condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"},"on_fail":"repair"}), {
            let mut repair = slice("repair", &["src"]); repair["depends_on"] = json!(["gate"]); repair
        }]);
        let texts = if expected == "VERDICT: PASS" { vec![output.as_str(), expected, "checked"] } else { vec![output.as_str(), expected, "repaired", "checked"] };
        let mut owner = fixture_owner(&texts);
        let mut journal = MemoryJournal::default();
        if expected == "VERDICT: FAIL" {
            owner.observations.truncate(2);
            owner.observations.push_back(AttemptObservation::Pending { attempt_ref: Some("repair".into()) });
            let pending = advance(&planner(3), &mut owner, &mut journal).unwrap();
            assert_eq!(pending.status, GraphStatus::Pending);
            assert_eq!(pending.checkpoint.nodes.iter().find(|n| n.id == "after").unwrap().state, NodeState::Pending);
            owner.observations.extend(["repaired", "checked"].map(|text| terminal_with_result(AttemptOutcome::Succeeded, text)));
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
        let mut first = planner(4).nodes.remove(0); first.id = "first".into();
        let mut second = first.clone(); second.id = "second".into();
        let mut doc = graph(if sibling { vec![first, second] } else { vec![first] }); doc.integration_branch = Some("integration".into());
        let mut repair = slice("repair", &["src"]); repair["access"] = json!("worktree_write"); repair["worktree"] = json!(".worktrees/a"); repair["depends_on"] = json!(["gate"]);
        let mut nodes = vec![slice("review", &["src"]), json!({"id":"gate","kind":"gate","access":"read_only","depends_on":["review"],"inputs":[{"node":"review","outputs":["verdict"]}],"condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"},"on_fail":"repair"}), repair];
        let mut required = slice("required", &["src"]); required["access"] = json!("worktree_write"); required["worktree"] = json!(".worktrees/b");
        if explicit_dependency { required["depends_on"] = json!(["repair"]); }
        if !sibling { nodes.push(required.clone()); }
        let first = result(nodes);
        let second = result(vec![required]);
        let texts = if sibling { vec![first.as_str(), second.as_str(), "VERDICT: PASS", "done"] } else { vec![first.as_str(), "VERDICT: PASS", "done"] };
        let mut owner = fixture_owner(&texts);
        let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
        assert!(run.checkpoint.successful());
        let id = if sibling { "second--required" } else { "first--required" };
        assert_eq!(run.checkpoint.nodes.iter().find(|n| n.id == id).unwrap().state, if explicit_dependency { NodeState::Skipped } else { NodeState::Succeeded });
        assert_eq!(owner.calls.len(), if sibling { 4 } else if explicit_dependency { 2 } else { 3 });
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
        assert_eq!(run.checkpoint.pause_code.as_deref(), Some("invalid_plan_expansion"), "{reason}");
        assert!(run.checkpoint.expansions.is_empty());
    }
    let mut same = slice("same", &["src"]);
    same["read_roots"] = json!(["/granted"]);
    let mut doc = planner(1);
    doc.nodes[0].read_roots = vec!["/granted".into()];
    let run = advance(&doc, &mut fixture_owner(&[&result(vec![same]), "done", "done"]), &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.checkpoint.expansions.len(), 1);
}

fn two_instances(first: Value, second: Value) -> String {
    format!(
        "```fragment\n{}\n```",
        json!({"schema_version":1,"nodes":[],"patterns":[first, second]})
    )
}

#[test]
fn disjoint_waves_refuse_overlapping_scopes_unless_a_dependency_orders_them() {
    let mut doc = pattern_planner(8);
    doc.integration_branch = Some("integration".into());
    doc.nodes[0].plan.as_mut().unwrap().disjoint_waves = true;
    let one = json!({"id":"one", "pattern":"generator", "parameters":{"scope":["src/a"]}});
    let clash = json!({"id":"two", "pattern":"generator", "parameters":{"scope":["src/a/b"]}});
    let mut owner = fixture_owner(&[&two_instances(one.clone(), clash.clone())]);
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert_eq!(run.checkpoint.pause_code.as_deref(), Some("invalid_plan_expansion"));
    assert!(run.checkpoint.expansions.is_empty());
    assert_eq!(owner.calls.len(), 1);

    let disjoint = json!({"id":"two", "pattern":"generator", "parameters":{"scope":["src/b"]}});
    let mut ordered = clash.clone();
    ordered["depends_on"] = json!(["one"]);
    for second in [disjoint, ordered] {
        let results = ["implemented", "reviewed", "implemented", "reviewed", "checked"];
        let mut owner = fixture_owner(&[&[two_instances(one.clone(), second).as_str()][..], &results[..]].concat());
        let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
        assert_eq!(run.status, GraphStatus::Complete, "{:?}", run.checkpoint.pause_reason);
        assert_eq!(run.checkpoint.expansions[0].patterns.len(), 2);
    }

    // Without the switch, overlapping instances keep sharing one checkout.
    let mut plain = pattern_planner(8);
    plain.integration_branch = Some("integration".into());
    plain.nodes[0].plan.as_mut().unwrap().disjoint_waves = false;
    let results = ["implemented", "reviewed", "implemented", "reviewed", "checked"];
    let mut owner = fixture_owner(&[&[two_instances(one, clash).as_str()][..], &results[..]].concat());
    let run = advance(&plain, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Complete, "{:?}", run.checkpoint.pause_reason);
}

#[test]
fn patterns_only_plans_refuse_raw_fragment_nodes() {
    let mut doc = pattern_planner(8);
    doc.nodes[0].plan.as_mut().unwrap().patterns_only = true;
    let raw = format!(
        "```fragment\n{}\n```",
        json!({"schema_version":1,"nodes":[slice("a", &["src"])],"patterns":[]})
    );
    let mut owner = fixture_owner(&[&raw]);
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert_eq!(run.checkpoint.pause_code.as_deref(), Some("invalid_plan_expansion"));
}

#[test]
fn pattern_routes_may_vary_only_over_a_closed_list() {
    let mut doc = pattern_planner(4);
    {
        let pattern = &mut doc.nodes[0].plan.as_mut().unwrap().patterns[0];
        pattern.nodes[0]["route"] = json!({"alias":"${label}"});
    }
    assert!(plans::validate_spec(&doc.nodes[0]).is_err(), "free text names any route");
    {
        let parameter = doc.nodes[0].plan.as_mut().unwrap().patterns[0]
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
    let mut owner = fixture_owner(&[&pattern_result(inside), "implemented", "reviewed", "checked"]);
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Complete, "{:?}", run.checkpoint.pause_reason);
}

#[test]
fn pattern_read_roots_may_follow_only_a_closed_list() {
    let mut doc = pattern_planner(4);
    doc.nodes[0].plan.as_mut().unwrap().patterns[0].nodes[0]["read_roots"] = json!(["${label}"]);
    assert!(plans::validate_spec(&doc.nodes[0]).is_err(), "free text names any root");
    {
        let parameter = doc.nodes[0].plan.as_mut().unwrap().patterns[0]
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
        let pattern = &mut doc.nodes[0].plan.as_mut().unwrap().patterns[0];
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

#[test]
fn planned_big_batch_expands_to_its_static_composite_and_records_the_patterns() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/presets/fixtures/big-batch.plan.json");
    // The CLI pins `HEAD` to a commit before a graph runs; do the same here.
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replace("\"commit\": \"HEAD\"", "\"commit\": \"0123456789abcdef0123456789abcdef01234567\"");
    let mut doc: GraphDocument = serde_json::from_str(&text).unwrap();
    // Run the planner node alone, ahead of one consumer.
    doc.nodes.retain(|node| node.id == "plan");
    doc.nodes[0].depends_on.clear();
    doc.nodes[0].inputs.clear();
    doc.nodes.push(serde_json::from_value(json!({
        "id":"after", "access":"read_only", "prompt":"Check all slices",
        "depends_on":["plan"], "route":{"alias":"senior"}
    })).unwrap());

    let lane = |id: &str, tree: &str, scope: &str, extra: Value| {
        let mut instance = json!({"id":id, "pattern":"writer-lane", "parameters":{
            "lane":id, "worktree":tree, "scope":[scope], "acceptance":[format!("{id} works")],
            "route":"junior", "budget_seconds":1800}});
        instance.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        instance
    };
    let instances = vec![
        lane("a", "/repo/lane1", "src/a", json!({})),
        lane("b", "/repo/lane2", "src/b", json!({})),
        json!({"id":"barrier", "pattern":"wave-barrier", "parameters":{"wave":"one"},
               "depends_on":["a","b"],
               "inputs":[{"node":"a","outputs":["commit"]},{"node":"b","outputs":["commit"]}]}),
        lane("c", "/repo/lane1", "src/a", json!({"depends_on":["barrier"]})),
    ];
    let fragment = format!("```fragment\n{}\n```", json!({"schema_version":1,"nodes":[],"patterns":instances}));
    // Every lane runs a writer, two lens legs and an evaluator, then the barrier and consumer.
    let mut results = vec![fragment.as_str()];
    results.extend(std::iter::repeat_n("done\nVERDICT: PASS", 40));
    let mut owner = fixture_owner(&results);
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Complete, "{:?}", run.checkpoint.pause_reason);
    let expansion = &run.checkpoint.expansions[0];
    let recorded: Vec<_> = expansion.patterns.iter().map(|p| (p.id.as_str(), p.name.as_str())).collect();
    assert_eq!(recorded, [("plan--a", "writer-lane"), ("plan--b", "writer-lane"), ("plan--barrier", "wave-barrier"), ("plan--c", "writer-lane")]);
    assert_eq!(expansion.patterns[0].parameters["budget_seconds"], 1800);
    assert_eq!(expansion.patterns[0].parameters["route"], "junior");
    assert_eq!(expansion.patterns[2].parameters, json!({"wave":"one"}));

    // Each planned lane is the static lane written independently: the golden
    // expansion of the static preset, flattened. Same roles and digests, access,
    // family separation, dispositions, source and dependency edges.
    let golden: Value = serde_json::from_str(
        &std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../assets/presets/fixtures/big-batch.expansion.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let lane = &golden["nodes"].as_array().unwrap().iter().find(|n| n["id"] == "lanes").unwrap()["composite"]["nodes"][0];
    let mut static_nodes: Vec<Value> = Vec::new();
    for child in lane["composite"]["nodes"].as_array().unwrap() {
        match child.get("composite") {
            // Legs inherit the lens composite's source, which flat nodes carry themselves.
            Some(inner) => static_nodes.extend(inner["nodes"].as_array().unwrap().iter().cloned().map(|mut leg| {
                leg["source"] = child["source"].clone();
                leg
            })),
            None => static_nodes.push(child.clone()),
        }
    }
    let project = |nodes: &[Value], prefix: &str| -> Vec<Value> {
        let mut rows: Vec<Value> = nodes
            .iter()
            .map(|node| {
                json!({
                    "id": node["id"].as_str().unwrap().trim_start_matches(prefix),
                    "role": node["role"],
                    "role_digest": node["role_digest"],
                    "access": node["access"],
                    "kind": node.get("kind").filter(|kind| *kind != "composite"),
                    "distinct_from": node.get("distinct_from"),
                    "dispositions": node.get("dispositions"),
                    "source": node["source"]["type"],
                })
            })
            .collect();
        rows.sort_by_key(|row| row["id"].as_str().unwrap().to_owned());
        rows
    };
    let planned: Vec<Value> = expansion.nodes.iter().filter(|n| n.id.starts_with("plan--a--")).map(|n| serde_json::to_value(n).unwrap()).collect();
    assert_eq!(planned.len(), 4);
    assert_eq!(project(&planned, "plan--a--"), project(&static_nodes, ""));
    // The evaluator waits for the lens legs and the writer, as the static one does.
    let evaluate = planned.iter().find(|n| n["id"] == "plan--a--evaluate").unwrap();
    assert_eq!(evaluate["depends_on"], json!(["plan--a--lens-correctness-spec", "plan--a--lens-simplicity", "plan--a--implement"]));
    // A lane's exports carry the evaluator's verdict for the barrier and later waves.
    assert!(expansion.patterns[0].nodes.iter().any(|id| id == "plan--a--evaluate"));
}

/// A preset planner with a static fallback refuses an invalid fragment by
/// running the fallback and naming the refusal, never by trusting it.
fn assert_refused_into_fallback(run: &GraphRun) {
    assert_eq!(run.status, GraphStatus::Complete);
    let plan = run.checkpoint.nodes.iter().find(|node| node.id == "plan").unwrap();
    let reason = &plan.outputs.as_ref().unwrap().plan.as_ref().unwrap().fallback_reason;
    assert!(reason.contains("planner fragment invalid"), "{reason}");
}

fn planned_fixture(name: &str) -> GraphDocument {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../assets/presets/fixtures/{name}.plan.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replace("\"commit\": \"HEAD\"", "\"commit\": \"0123456789abcdef0123456789abcdef01234567\"");
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
    assert_eq!(run.status, GraphStatus::Complete, "{:?}", run.checkpoint.pause_reason);
    let expansion = &run.checkpoint.expansions[0];
    let recorded: Vec<_> = expansion.patterns.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(recorded, ["scout", "scout", "external-scout"]);
    assert_eq!(expansion.patterns[1].parameters["route"], "senior");
    assert_eq!(expansion.patterns[0].parameters["route"], "junior");
    // The synthesis reads each scout's evidence as well as the planner's text.
    let expanded = plans::expand(&doc, &[], &run.checkpoint.expansions, &plans::generous()).unwrap();
    let synthesis = expanded.nodes.iter().find(|n| n.id == "synthesis").unwrap();
    let inputs: Vec<_> = synthesis.inputs.iter().map(|i| i.node.as_str()).collect();
    assert_eq!(inputs, ["plan", "plan--a--scout", "plan--b--scout", "plan--web--scout"]);
    assert!(synthesis.depends_on.contains(&"plan--a--scout".to_owned()));
    // The scouts keep the caller's read roots, which the planner holds too.
    let scout = expanded.nodes.iter().find(|n| n.id == "plan--a--scout").unwrap();
    assert!(scout.read_roots.iter().any(|root| root.ends_with("/repo/extra")), "{:?}", scout.read_roots);
    // A route outside the closed list is refused, not trusted.
    let outside = pattern_result_all(vec![
        json!({"id":"a", "pattern":"scout", "parameters":{"question":"q", "route":"web"}}),
    ]);
    let mut owner = fixture_owner(&[&outside]);
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_refused_into_fallback(&run);
}

#[test]
fn planned_review_council_expands_only_the_lens_legs_the_catalogue_lists() {
    let doc = planned_fixture("review-council");
    let leg = |lens: &str, tier: &str| {
        json!({"id":lens, "pattern":format!("lens-{lens}"), "parameters":{"tier":tier}})
    };
    let fragment = pattern_result_all(vec![
        leg("correctness-spec", "senior"),
        leg("tests", "junior"),
    ]);
    let mut results = vec!["framed", fragment.as_str()];
    results.extend(std::iter::repeat_n("done\nVERDICT: PASS", 12));
    let mut owner = fixture_owner(&results);
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Complete, "{:?}", run.checkpoint.pause_reason);
    let expansion = &run.checkpoint.expansions[0];
    let recorded: Vec<_> = expansion.patterns.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(recorded, ["lens-correctness-spec", "lens-tests"]);
    assert_eq!(expansion.patterns[0].parameters["tier"], "senior");
    // The verifier reads the lens findings through the lens legs' exports.
    let expanded = plans::expand(&doc, &[], &run.checkpoint.expansions, &plans::generous()).unwrap();
    let verify = expanded.nodes.iter().find(|n| n.id == "verify").unwrap();
    let inputs: Vec<_> = verify.inputs.iter().map(|i| i.node.as_str()).collect();
    assert_eq!(inputs, ["plan", "plan--correctness-spec--lens", "plan--tests--lens"]);
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
            vec![json!({"id":"skip", "pattern":"no-scout", "parameters":{"reason":"the brief names the files"}})],
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
        assert_eq!(run.status, GraphStatus::Complete, "{name}: {:?}", run.checkpoint.pause_reason);
        let expanded = plans::expand(&doc, &[], &run.checkpoint.expansions, &plans::generous()).unwrap();
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
        extra: Default::default(),
        node: "plan".into(),
        outputs: vec![OutputKind::Result],
    }];
    let instance = json!({"id":"impl", "pattern":"generator", "parameters":{}});
    let run_it = |doc: &GraphDocument| {
        let mut owner = fixture_owner(&[&pattern_result(instance.clone()), "a", "b", "c"]);
        let run = advance(doc, &mut owner, &mut MemoryJournal::default()).unwrap();
        assert_eq!(run.status, GraphStatus::Complete, "{:?}", run.checkpoint.pause_reason);
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
        .join("../../assets/presets/fixtures/big-batch.plan.json");
    let mut doc: GraphDocument = serde_json::from_str(
        &std::fs::read_to_string(&path)
            .unwrap()
            .replace("\"commit\": \"HEAD\"", "\"commit\": \"0123456789abcdef0123456789abcdef01234567\""),
    )
    .unwrap();
    doc.nodes.retain(|node| node.id == "plan");
    doc.nodes[0].depends_on.clear();
    doc.nodes[0].inputs.clear();
    let lane = |id: &str, tree: &str, scope: &str, budget: u64| json!({"id":id, "pattern":"writer-lane", "parameters":{
        "lane":id, "worktree":tree, "scope":[scope], "acceptance":["works"], "budget_seconds":budget}});
    let cases = [
        ("overlap", vec![lane("a", "/repo/lane1", "src", 600), lane("b", "/repo/lane2", "src/b", 600)]),
        ("budget", vec![lane("a", "/repo/lane1", "src/a", 3601)]),
        ("worktree", vec![lane("a", "/elsewhere", "src/a", 600)]),
    ];
    for (label, instances) in cases {
        let fragment = format!("```fragment\n{}\n```", json!({"schema_version":1,"nodes":[],"patterns":instances}));
        let mut owner = fixture_owner(&[&fragment]);
        let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
        assert_eq!(run.status, GraphStatus::Paused, "{label}");
        assert_eq!(run.checkpoint.pause_code.as_deref(), Some("invalid_plan_expansion"), "{label}");
        assert!(run.checkpoint.expansions.is_empty(), "{label}");
        assert_eq!(owner.calls.len(), 1, "{label}");
    }
    // The same two lanes are admitted once they are ordered or disjoint.
    let fragment = format!("```fragment\n{}\n```", json!({"schema_version":1,"nodes":[],"patterns":[lane("a", "/repo/lane1", "src/a", 3600), lane("b", "/repo/lane2", "src/b", 60)]}));
    let mut owner = fixture_owner(&[&fragment, "a", "b"]);
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.checkpoint.expansions.len(), 1, "{:?}", run.checkpoint.pause_reason);
}
