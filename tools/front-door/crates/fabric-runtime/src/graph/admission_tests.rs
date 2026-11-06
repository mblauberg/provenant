use super::*;
use serde_json::{Value, json};

fn document(version: u32, nodes: Value) -> GraphDocument {
    serde_json::from_value(json!({
        "schema_version": version,
        "graph_id": "admission-test",
        "nodes": nodes,
    }))
    .unwrap()
}

#[test]
fn admission_refuses_unenforced_required_output_fields() {
    {
        let key = "required";
        let mut selector = json!({"node":"source", "outputs":["findings"]});
        selector[key] = json!(["findings"]);
        let graph = document(
            2,
            json!([
            {"id":"source","prompt":"source","access":"read_only","routes":[{"alias":"senior"}]},
            {"id":"consumer","prompt":"consumer","access":"read_only", "routes":[{"alias":"senior"}], "depends_on":["source"], "inputs":[selector]}
            ]),
        );
        let error = graph
            .admit(&crate::config::GraphLimits::default())
            .unwrap_err();
        assert!(
            error.to_string().contains("required output"),
            "{key}: {error}"
        );
        let round_trip: GraphDocument =
            serde_json::from_value(serde_json::to_value(&graph).unwrap()).unwrap();
        assert_eq!(
            round_trip.nodes[1].inputs[0].extra[key],
            json!(["findings"])
        );
    }
}

#[test]
fn node_review_base_requires_v2_and_a_nonempty_reference() {
    for (version, base, allowed) in [
        (1, json!("main"), false),
        (2, json!("main"), true),
        (2, json!(""), false),
        (2, json!(7), false),
    ] {
        let graph = document(
            version,
            json!([
                {"id":"review","prompt":"review","access":"read_only","route":{"alias":"senior"},"review_base":base}
            ]),
        );
        assert_eq!(
            graph.admit(&crate::config::GraphLimits::default()).is_ok(),
            allowed
        );
    }
}

#[test]
fn required_output_contract_requires_v2_and_selected_supported_kinds() {
    let make = |version, required: Value| {
        document(
            version,
            json!([
                {"id":"source","prompt":"source","access":"read_only","routes":[{"alias":"senior"}]},
                {"id":"consumer","prompt":"consumer","access":"read_only","routes":[{"alias":"senior"}],"depends_on":["source"],"inputs":[{"node":"source","outputs":["findings"],"required_outputs":required}]}
            ]),
        )
    };
    let limits = crate::config::GraphLimits::default();
    assert!(make(1, json!(["findings"])).admit(&limits).is_err());
    assert!(make(2, json!(["findings"])).admit(&limits).is_ok());
    for required in [
        json!(["verdict"]),
        json!(["findings", "findings"]),
        json!(["result"]),
    ] {
        assert!(make(2, required).admit(&limits).is_err());
    }
    let composite = document(
        2,
        json!([
            {"id":"source","kind":"composite","access":"read_only","composite":{
                "nodes":[{"id":"member","prompt":"x","access":"read_only","routes":[{"alias":"senior"}]}],
                "exports":[{"node":"member","outputs":["findings"]}]
            }},
            {"id":"consumer","prompt":"consumer","access":"read_only","routes":[{"alias":"senior"}],"depends_on":["source"],"inputs":[{"node":"source","outputs":["findings"],"required_outputs":["findings"]}]}
        ]),
    );
    assert!(composite.admit(&limits).is_ok());
    let mut unsupported = composite.clone();
    unsupported.nodes[1].inputs[0].outputs = vec![OutputKind::Verdict];
    unsupported.nodes[1].inputs[0].required_outputs = vec![OutputKind::Verdict];
    assert!(
        unsupported
            .admit(&limits)
            .unwrap_err()
            .to_string()
            .contains("cannot yet be verified")
    );
    let planner = document(
        2,
        json!([{
            "id":"plan", "kind":"plan", "access":"read_only", "plan":{
                "kinds":[], "max_nodes":1,
                "patterns":[{"name":"p", "nodes":[], "exports":[{"node":"x","outputs":["findings"],"required_outputs":["findings"]}]}]
            }
        }]),
    );
    assert!(
        validate_output_kinds(&planner)
            .unwrap_err()
            .to_string()
            .contains("planner exports")
    );
    let prior_round = document(
        2,
        json!([{
            "id":"loop", "kind":"loop", "access":"read_only", "loop":{
                "max_rounds":2, "until":"work", "body":[{
                    "id":"work", "prompt":"work", "access":"read_only", "routes":[{"alias":"senior"}],
                    "inputs":[{"node":"previous.work","outputs":["findings"],"required_outputs":["findings"]}]
                }]
            }
        }]),
    );
    assert!(
        prior_round
            .admit(&limits)
            .unwrap_err()
            .to_string()
            .contains("previous-round")
    );
}

#[test]
fn admission_returns_typed_cause_with_original_text() {
    let graph = document(3, json!([{"id":"n","prompt":"x","access":"read_only"}]));
    let error = graph.admit(&Default::default()).unwrap_err();
    assert!(matches!(error, AdmissionError::Schema(_)));
    assert_eq!(error.to_string(), "schema version");
}

#[test]
fn admission_expansion_returns_typed_cause_with_original_text() {
    let graph = document(2, json!([{"id":"loop","kind":"loop","access":"read_only"}]));
    let error = loops::expand(&graph, &[], &Default::default()).unwrap_err();
    assert!(matches!(error, AdmissionError::Composite(_)));
    assert_eq!(error.to_string(), "loop requires a body");
    let error = graph.admit(&Default::default()).unwrap_err();
    assert!(matches!(error, AdmissionError::Composite(_)));
    assert_eq!(error.to_string(), "loop requires a body");
}

#[test]
fn flat_plan_returns_typed_cause_with_original_text() {
    let graph = document(1, json!([{"id":"n","access":"read_only","prompt":"x"}]));
    let error = validate_flat_plan(&graph, &Default::default()).unwrap_err();
    assert!(matches!(error, AdmissionError::Plan(_)));
    assert_eq!(error.to_string(), "route list is empty");
    let error = graph.admit(&Default::default()).unwrap_err();
    assert!(matches!(error, AdmissionError::Plan(_)));
    assert_eq!(error.to_string(), "route list is empty");
}

#[test]
fn admission_schema_matrix() {
    let plain = json!([{"id":"n","prompt":"x","access":"read_only"}]);
    assert!(validate_schema(&document(1, plain.clone())).is_ok());
    assert!(validate_schema(&document(2, plain.clone())).is_ok());
    for version in [0, 3] {
        assert!(validate_schema(&document(version, plain.clone())).is_err());
    }
    for (v2, expected) in [(1, false), (2, true)] {
        let mut review_base = document(v2, plain.clone());
        review_base.review_base = Some("base".into());
        assert_eq!(validate_schema(&review_base).is_ok(), expected);
    }

    let v2_node = json!({"id":"n","prompt":"x","access":"read_only","family_group":"g"});
    assert!(validate_schema(&document(1, json!([v2_node.clone()]))).is_err());
    assert!(validate_schema(&document(2, json!([v2_node]))).is_ok());

    let nested = [
        json!({"id":"c","kind":"composite","access":"read_only","composite":{"nodes":[{"id":"n","prompt":"x","access":"read_only","family_group":"g"}],"exports":[{"node":"n","outputs":["result"]}]}}),
        json!({"id":"l","kind":"loop","access":"read_only","loop":{"body":[{"id":"n","prompt":"x","access":"read_only","family_group":"g"}],"until":"done","max_rounds":1}}),
        json!({"id":"p","kind":"plan","access":"read_only","plan":{"kinds":[],"max_nodes":1,"patterns":[],"fallback":{"nodes":[{"id":"n","prompt":"x","access":"read_only","family_group":"g"}],"exports":[]}}}),
        json!({"id":"p","kind":"plan","access":"read_only","plan":{"kinds":[],"max_nodes":1,"patterns":[{"name":"p","nodes":[{"id":"x","family_group":"g"}],"exports":[]}]} }),
    ];
    for node in nested {
        assert!(validate_schema(&document(1, json!([node.clone()]))).is_err());
        assert!(validate_schema(&document(2, json!([node]))).is_ok());
    }
}

#[test]
fn admission_output_kind_matrix() {
    let kinds = [
        OutputKind::Result,
        OutputKind::Findings,
        OutputKind::Verdict,
        OutputKind::Remaining,
        OutputKind::Branch,
        OutputKind::Commit,
    ];
    let shapes = [
        (json!({"id":"s","access":"read_only"}), 4usize),
        (
            json!({"id":"s","access":"read_only","source":{"type":"commit","commit":"abc"}}),
            5,
        ),
        (
            json!({"id":"s","access":"worktree_write","worktree":"/tmp/w"}),
            6,
        ),
        (
            json!({"id":"s","access":"read_only","kind":"land","land":{"node":"w","tier":"routine"}}),
            6,
        ),
        (
            json!({"id":"s","access":"read_only","kind":"loop","loop":{"body":[],"until":"done","max_rounds":1}}),
            4,
        ),
        (
            json!({"id":"s","access":"read_only","kind":"composite","composite":{"nodes":[{"id":"x","access":"read_only"}],"exports":[{"node":"x","outputs":["commit"]}]}}),
            1,
        ),
    ];
    for (shape, count) in shapes {
        let node: GraphNode = serde_json::from_value(shape).unwrap();
        let supplied = supplied_kinds(&node);
        assert_eq!(supplied.len(), count);
        for kind in kinds {
            assert_eq!(
                supplied.contains(&kind),
                match kind {
                    OutputKind::Result
                    | OutputKind::Findings
                    | OutputKind::Verdict
                    | OutputKind::Remaining => count != 1,
                    OutputKind::Branch => count == 6,
                    OutputKind::Commit => count == 5 || count == 6 || count == 1,
                }
            );
        }
    }

    let inherited = document(
        1,
        json!([{
            "id":"group","access":"read_only","kind":"composite",
            "source":{"type":"commit","commit":"abc"},
            "composite":{"nodes":[{"id":"member","access":"read_only"}],"exports":[{"node":"member","outputs":["commit"]}]}
        }]),
    );
    let expanded = loops::expand(&inherited, &[], &crate::config::GraphLimits::default()).unwrap();
    let member = expanded
        .nodes
        .iter()
        .find(|node| node.id == "group--member")
        .unwrap();
    assert!(supplied_kinds(member).contains(&OutputKind::Commit));

    let selected_commit = document(
        1,
        json!([
            {"id":"source","prompt":"source","access":"read_only"},
            {"id":"consumer","prompt":"consumer","access":"read_only","depends_on":["source"],"inputs":[{"node":"source","outputs":["commit"]}]}
        ]),
    );
    let error = validate_output_kinds(&selected_commit).unwrap_err();
    assert!(matches!(error, AdmissionError::OutputKind { .. }));
    let error = error.to_string();
    for part in ["consumer", "source", "commit", "result", "findings"] {
        assert!(error.contains(part), "{error}");
    }

    let producers = [
        json!({"id":"source","access":"read_only"}),
        json!({"id":"source","access":"read_only","source":{"type":"commit","commit":"abc"}}),
        json!({"id":"source","access":"worktree_write","worktree":"/tmp/w"}),
        json!({"id":"source","access":"read_only","kind":"land","land":{"node":"writer","tier":"routine"}}),
        json!({"id":"source","access":"read_only","kind":"loop","loop":{"body":[],"until":"done","max_rounds":1}}),
        json!({"id":"source","access":"read_only","kind":"composite","composite":{"nodes":[{"id":"x","access":"read_only"}],"exports":[{"node":"x","outputs":["commit"]}]}}),
    ];
    for producer in producers {
        let graph = document(
            1,
            json!([
                producer,
                {"id":"consumer","access":"read_only","depends_on":["source"],"inputs":[{"node":"source","outputs":["commit"]}]}
            ]),
        );
        let expected = graph.nodes[0].access == AccessMode::WorktreeWrite
            || graph.nodes[0].kind == Some(NodeKind::Land)
            || matches!(graph.nodes[0].source, Some(SourceBinding::Commit { .. }))
            || graph.nodes[0].composite.as_ref().is_some_and(|c| {
                c.exports
                    .iter()
                    .any(|e| e.outputs.contains(&OutputKind::Commit))
            });
        assert_eq!(validate_output_kinds(&graph).is_ok(), expected);
    }
}

#[test]
fn admission_composite_exports_are_declared_capabilities() {
    let council = json!({
        "id":"council","kind":"composite","access":"read_only",
        "source":{"type":"commit","commit":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},
        "composite":{"nodes":[{"id":"synthesis","kind":"synthesise","access":"read_only"}],
        "exports":[{"node":"synthesis","outputs":["result","findings","verdict","commit"]}]}
    });
    let document = document(
        1,
        json!([
            council,
            {"id":"writer","access":"read_only","depends_on":["council"],"inputs":[{"node":"council","outputs":["commit"]}]}
        ]),
    );
    let expanded = loops::expand(&document, &[], &crate::config::GraphLimits::default()).unwrap();
    assert!(validate_output_kinds(&expanded).is_ok());
    let mut malformed = document.clone();
    malformed.nodes[0].composite.as_mut().unwrap().exports[0].node = "missing".into();
    assert!(loops::expand(&malformed, &[], &crate::config::GraphLimits::default()).is_err());
    let mut empty = document.clone();
    empty.nodes[0].composite.as_mut().unwrap().exports[0]
        .outputs
        .clear();
    assert!(loops::expand(&empty, &[], &crate::config::GraphLimits::default()).is_err());
    assert!(serde_json::from_value::<GraphDocument>(json!({
        "schema_version":1,"graph_id":"bad-output-kind","nodes":[{"id":"c","kind":"composite","access":"read_only","composite":{"nodes":[{"id":"x","access":"read_only"}],"exports":[{"node":"x","outputs":["unknown"]}]}}]
    })).is_err());
}

#[test]
fn admission_structural_limits_name_the_key() {
    let graph = document(
        1,
        json!([
            {"id":"root","prompt":"root prompt","access":"read_only","routes":[{"alias":"senior"}]},
            {"id":"a","prompt":"child prompt","access":"read_only","depends_on":["root"],"routes":[{"alias":"senior"}]},
            {"id":"b","prompt":"child prompt","access":"read_only","depends_on":["root"],"routes":[{"alias":"senior"}]}
        ]),
    );
    type LimitCase = (&'static str, fn(&mut crate::config::GraphLimits));
    let cases: [LimitCase; 7] = [
        ("nodes", |limits: &mut crate::config::GraphLimits| {
            limits.nodes = 2
        }),
        ("edges", |limits: &mut crate::config::GraphLimits| {
            limits.edges = 1
        }),
        (
            "routes_per_node",
            |limits: &mut crate::config::GraphLimits| limits.routes_per_node = 0,
        ),
        (
            "node_prompt_bytes",
            |limits: &mut crate::config::GraphLimits| limits.node_prompt_bytes = 1,
        ),
        (
            "graph_prompt_bytes",
            |limits: &mut crate::config::GraphLimits| limits.graph_prompt_bytes = 1,
        ),
        (
            "fan_out_width",
            |limits: &mut crate::config::GraphLimits| limits.fan_out_width = 1,
        ),
        (
            "expansion_depth",
            |limits: &mut crate::config::GraphLimits| limits.expansion_depth = 0,
        ),
    ];
    for (key, configure) in cases {
        let mut limits = crate::config::GraphLimits::default();
        configure(&mut limits);
        let refusal = graph.admit(&limits).unwrap_err();
        assert!(
            matches!(refusal, AdmissionError::StructuralLimit(_)),
            "{refusal}"
        );
        let refusal = refusal.to_string();
        assert!(refusal.contains(&format!("limits.{key}")), "{refusal}");
    }
}
