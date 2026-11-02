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
    let mut doc = pattern_planner(4);
    let parameters = &mut doc.nodes[0].plan.as_mut().unwrap().patterns[0].parameters;
    parameters.get_mut("label").unwrap().default = None;
    parameters.get_mut("retries").unwrap().allowed = vec![json!(1), json!(2)];
    let prompt = plans::prompt(&doc.nodes[0]);
    assert!(prompt.contains("Allowed patterns: generator"));
    assert!(prompt.contains("label:string"));
    assert!(prompt.contains("required"));
    assert!(prompt.contains("default=1"));
    assert!(prompt.contains("enum=[1,2]"));
    assert!(prompt.contains("min=0"));
    assert!(prompt.contains("max=2"));
    assert!(prompt.contains("\"patterns\""));
    assert!(prompt.contains("Do not emit other presets or review shapes"));
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

fn assert_pattern_parameter_error(doc: &GraphDocument, parameters: Value, _reason: &str) {
    let mut owner = fixture_owner(&[&pattern_result(json!({"id":"impl", "pattern":"generator", "parameters":parameters}))]);
    let run = advance(doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert_eq!(run.checkpoint.pause_code.as_deref(), Some("invalid_plan_expansion"));
    assert!(run.checkpoint.expansions.is_empty());
}

#[test]
fn pattern_missing_required_parameter_is_refused() {
    let mut doc = pattern_planner(4);
    doc.nodes[0].plan.as_mut().unwrap().patterns[0].parameters.get_mut("label").unwrap().default = None;
    assert_pattern_parameter_error(&doc, json!({}), "requires parameter label");
}

#[test]
fn pattern_unknown_parameter_is_refused() {
    assert_pattern_parameter_error(&pattern_planner(4), json!({"unknown":1}), "no parameter unknown");
}

#[test]
fn pattern_non_table_parameters_are_refused() {
    assert_pattern_parameter_error(&pattern_planner(4), json!([]), "parameters must be a table");
}

#[test]
fn pattern_enum_violation_is_refused() {
    let mut doc = pattern_planner(4);
    doc.nodes[0].plan.as_mut().unwrap().patterns[0].parameters.get_mut("retries").unwrap().allowed = vec![json!(1)];
    assert_pattern_parameter_error(&doc, json!({"retries":2}), "must be one of");
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
    let mut owner = fixture_owner(&[&pattern_result(instance.clone())]);
    let run = advance(&pattern_planner(2), &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.checkpoint.pause_code.as_deref(), Some("invalid_plan_expansion"));
    let mut doc = pattern_planner(4);
    doc.nodes[0].plan.as_mut().unwrap().kinds = vec![NodeKind::Gate];
    let mut owner = fixture_owner(&[&pattern_result(instance)]);
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.checkpoint.pause_code.as_deref(), Some("invalid_plan_expansion"));
}

#[test]
fn pattern_expansion_respects_plan_max_nodes_boundary() {
    let instance = json!({"id":"impl", "pattern":"generator"});
    let output = pattern_result(instance);
    let mut owner = fixture_owner(&[&output, "implemented", "reviewed", "checked"]);
    let run = advance(&pattern_planner(3), &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Complete, "{:?}", run.checkpoint.pause_reason);
    assert_eq!(run.checkpoint.expansions[0].nodes.len(), 3);
    let mut owner = fixture_owner(&[&output]);
    let run = advance(&pattern_planner(2), &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert_eq!(run.checkpoint.pause_code.as_deref(), Some("invalid_plan_expansion"));
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
    // A route outside the closed list is refused, not trusted.
    let outside = pattern_result_all(vec![
        json!({"id":"a", "pattern":"scout", "parameters":{"question":"q", "route":"web"}}),
    ]);
    let mut owner = fixture_owner(&[&outside]);
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert_eq!(run.checkpoint.pause_code.as_deref(), Some("invalid_plan_expansion"));
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
    // The security lens is gated off for this subject, so it cannot be named.
    let gated = pattern_result_all(vec![leg("security-privacy", "junior")]);
    let mut owner = fixture_owner(&["framed", &gated]);
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert_eq!(run.checkpoint.pause_code.as_deref(), Some("invalid_plan_expansion"));
    // A lens the catalogue lists cannot be run at a tier outside its closed list.
    let wild = pattern_result_all(vec![leg("tests", "web")]);
    let mut owner = fixture_owner(&["framed", &wild]);
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert_eq!(run.checkpoint.pause_code.as_deref(), Some("invalid_plan_expansion"));
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
