use super::*;
use provenant_os::{clock::SystemClock, fs::SystemPathResolver};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;

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
            journal.bind(GraphExecution {extra: Default::default(),  integration_base: None, directory: base.clone(), document: doc.clone() }).unwrap();
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
    assert!(prompt.contains("Do not emit presets or review shapes"));
}

#[test]
fn planner_prompt_names_only_the_nodes_allow_listed_patterns() {
    let prompt = plans::prompt(&pattern_planner(4).nodes[0]);
    assert!(prompt.contains("Allowed patterns: generator"));
    assert!(prompt.contains("label:string"));
    assert!(prompt.contains("\"patterns\""));
    assert!(prompt.contains("Do not emit other presets or review shapes"));
}

#[test]
fn continuation_rejects_original_pr_delivery_before_integration_rewrite() {
    let mut doc = writer_graph();
    doc.integration_branch = Some("integration".into());
    doc.nodes[0].delivery = Some(provenant_records::run::DeliveryMode::Pr);
    let run = advance(&doc, &mut fixture_owner(&[STOP]), &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert!(run.checkpoint.pause_reason.unwrap().contains("requires integrate or no delivery"));
    assert!(run.checkpoint.expansions.is_empty());
}

#[test]
fn oversized_nested_fragment_id_names_the_source_and_child() {
    let mut doc = planner(1);
    doc.nodes[0].id = "p".repeat(40);
    doc.nodes.truncate(1);
    let name = "c".repeat(23);
    let run = advance(&doc, &mut fixture_owner(&[&result(vec![slice(&name, &["src"])])]), &mut MemoryJournal::default()).unwrap();
    let reason = run.checkpoint.pause_reason.unwrap();
    assert!(reason.contains(&name) && reason.contains("64 bytes"), "{reason}");
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
        assert!(run.checkpoint.pause_reason.as_ref().unwrap().contains(reason), "{:?}", run.checkpoint.pause_reason);
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
        assert!(run.checkpoint.pause_reason.as_ref().unwrap().contains(field));
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
        assert!(
            run.checkpoint
                .pause_reason
                .as_ref()
                .unwrap()
                .contains(reason),
            "{:?}",
            run.checkpoint.pause_reason
        );
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
    assert!(ask.body.contains("allow-list"));
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
    assert!(
        run.checkpoint.pause_reason.as_ref().unwrap().contains("limits.nodes"),
        "{:?}",
        run.checkpoint.pause_reason
    );
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
    assert!(run.checkpoint.pause_reason.unwrap().contains("names no integration_branch"));
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
    assert!(run.checkpoint.pause_reason.unwrap().contains("limits.continuations=1"));
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
    assert!(run.checkpoint.pause_reason.unwrap().contains("loop writers must share one worktree"));
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
fn expansion_at_node_budget_reads_journal_once() {
    #[derive(Default)]
    struct CountingJournal { inner: MemoryJournal, reads: usize, saves: usize }
    impl GraphJournal for CountingJournal {
        type Error = std::convert::Infallible;
        fn load(&mut self, id: &str) -> Result<Option<GraphCheckpoint>, Self::Error> {
            self.reads += 1;
            self.inner.load(id)
        }
        fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error> {
            self.saves += 1;
            self.inner.save(checkpoint)
        }
    }
    let limits = crate::config::GraphLimits::default();
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
    let mut journal = CountingJournal::default();
    let run = advance_with_budget(&doc, &mut owner, &mut journal, &limits, &Default::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert_eq!(run.checkpoint.nodes.len(), limits.nodes);
    assert_eq!(run.checkpoint.expansions.len(), count);
    assert_eq!(journal.reads, 1);
    assert_eq!(owner.expansion_validations, count);
    assert_eq!(journal.saves, 1 + count * 4);
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
        assert!(run.checkpoint.pause_reason.as_ref().unwrap().contains(reason), "{:?}", run.checkpoint.pause_reason);
        assert!(run.checkpoint.expansions.is_empty());
    }
    let mut same = slice("same", &["src"]);
    same["read_roots"] = json!(["/granted"]);
    let mut doc = planner(1);
    doc.nodes[0].read_roots = vec!["/granted".into()];
    let run = advance(&doc, &mut fixture_owner(&[&result(vec![same]), "done", "done"]), &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.checkpoint.expansions.len(), 1);
}
