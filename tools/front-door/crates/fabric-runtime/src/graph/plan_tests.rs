use super::*;
use serde_json::{Value, json};

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

fn fixture_owner(results: &[&str]) -> FakeOwner {
    FakeOwner { observations: results.iter().map(|text| terminal_with_result(AttemptOutcome::Succeeded, text)).collect(), ..Default::default() }
}

const STOP: &str = "```handoff\n{\"schema_version\":1,\"status\":\"stopped_early\",\"note\":\"First half committed; finish the second half and run checks.\"}\n```";

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
        assert!(run.checkpoint.pause_reason.as_ref().unwrap().contains(reason), "{:?}", run.checkpoint.pause_reason);
        assert!(run.checkpoint.expansions.is_empty());
        assert_eq!(owner.calls.len(), 1);
        assert_eq!(advance(&planner(1), &mut owner, &mut journal).unwrap().status, GraphStatus::Paused);
        assert_eq!(resume(&planner(1), &mut owner, &mut journal).unwrap().status, GraphStatus::Paused);
    }
}

#[test]
fn plan_validates_effective_limits_and_ceiling_before_saving() {
    let output = result(vec![slice("a", &["src"]), slice("b", &["docs"])]);
    for (field, cap) in [("nodes", 3), ("edges", 3), ("fan_out_width", 2), ("expansion_depth", 1)] {
        let mut doc = planner(2);
        doc.limits = Some(serde_json::from_value(json!({field: 999})).unwrap());
        let ceiling: crate::config::GraphLimits = serde_json::from_value(json!({field:cap})).unwrap();
        let config = crate::config::GraphConfig { limits: ceiling.clone(), limit_ceiling: ceiling, ..Default::default() };
        let limits = doc.limits.as_ref().unwrap().resolve(&config).unwrap();
        let mut owner = fixture_owner(&[&output]);
        let mut journal = MemoryJournal::default();
        let run = advance_with_budget(&doc, &mut owner, &mut journal, &limits, &config.limit_ceiling, &Default::default()).unwrap();
        assert_eq!(run.status, GraphStatus::Paused);
        assert!(run.checkpoint.pause_reason.as_ref().unwrap().contains(field));
        assert!(run.checkpoint.expansions.is_empty());
        assert_eq!(owner.calls.len(), 1);
    }
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
    doc.nodes[1].inputs = vec![GraphInput { node: "write".into(), outputs: vec![OutputKind::Result] }];
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
    let ceiling = crate::config::GraphLimits::default();
    let mut owner = fixture_owner(&[STOP, STOP]);
    let mut journal = MemoryJournal::default();
    let run = advance_with_budget(&doc, &mut owner, &mut journal, &limits, &ceiling, &Default::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert!(!run.checkpoint.successful());
    assert_eq!(run.checkpoint.expansions.len(), 1);
    assert_eq!(run.checkpoint.remaining.as_ref().unwrap().continuations, 0);
    assert!(run.checkpoint.pause_reason.unwrap().contains("limits.continuations=1"));
    assert_eq!(owner.calls.len(), 2);
    assert_eq!(advance_with_budget(&doc, &mut owner, &mut journal, &limits, &ceiling, &Default::default()).unwrap().status, GraphStatus::Paused);
    owner.observations.extend(["VERDICT: PASS", "checked"].map(|text| terminal_with_result(AttemptOutcome::Succeeded, text)));
    let resumed = resume_with_budget(&doc, &mut owner, &mut journal, &limits, &ceiling, &Default::default(), &["limits.continuations=2".into()]).unwrap();
    assert_eq!(resumed.status, GraphStatus::Complete);
    assert_eq!(resumed.checkpoint.expansions.len(), 2);
    assert_eq!(owner.calls.len(), 4);
    limits.continuations = 0;
    let mut owner = super::FakeOwner { observations: [terminal_with_result(AttemptOutcome::Succeeded, STOP)].into(), ..Default::default() };
    let disabled = advance_with_budget(&doc, &mut owner, &mut MemoryJournal::default(), &limits, &ceiling, &Default::default()).unwrap();
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
    let (base, _) = validate_plan(&doc).unwrap();
    assert_eq!(run.checkpoint.plan_digest, CanonicalV1::record_digest("fabric-graph-plan", 1, &base).unwrap().to_string());
}

#[test]
fn invalid_planner_result_and_handoff_remain_paused_when_every_attempt_has_settled() {
    let doc = graph(vec![planner(1).nodes.remove(0)]);
    let mut owner = fixture_owner(&["not a fragment"]);
    let mut journal = MemoryJournal::default();
    let run = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert!(!run.checkpoint.successful());
    assert!(run.checkpoint.nodes.iter().all(|n| n.state.settled()));
    assert_eq!(advance(&doc, &mut owner, &mut journal).unwrap().status, GraphStatus::Paused);
    let mut doc = writer_graph();
    doc.nodes[0].required_verdict = None;
    let mut owner = fixture_owner(&["```handoff\n{}\n```"]);
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert!(run.checkpoint.pause_reason.unwrap().contains("invalid handoff"));
    assert_eq!(owner.calls.len(), 1);
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
