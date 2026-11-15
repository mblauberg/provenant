use super::*;
use serde_json::{Value, json};

fn document(nodes: Value) -> GraphDocument {
    super::model::document_fixture(json!({
        "graph_id": "admission-test",
        "nodes": nodes,
    }))
}

fn versioned_document(version: u32, nodes: Value) -> GraphDocument {
    let mut document = document(nodes);
    document.schema_version = u64::from(version);
    document
}

#[test]
fn admission_refuses_unenforced_required_output_fields() {
    {
        let key = "required";
        let mut selector = json!({"node":"source", "outputs":["findings"]});
        selector[key] = json!(["findings"]);
        let graph = document(json!([
        {"id":"source","prompt":"source","access":"read_only","routes":[{"alias":"senior"}]},
        {"id":"consumer","prompt":"consumer","access":"read_only", "routes":[{"alias":"senior"}], "depends_on":["source"], "inputs":[selector]}
        ]));
        let error = graph
            .admit(&crate::config::GraphLimits::default())
            .unwrap_err();
        assert!(matches!(
            error,
            AdmissionError::OutputKind {
                field: GraphOutputField::Input,
                ..
            }
        ));
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
        (GRAPH_DOCUMENT_SCHEMA_VERSION as u32, json!("main"), true),
        (GRAPH_DOCUMENT_SCHEMA_VERSION as u32, json!(""), false),
        (GRAPH_DOCUMENT_SCHEMA_VERSION as u32, json!(7), false),
    ] {
        let graph = versioned_document(
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
        versioned_document(
            version,
            json!([
                {"id":"source","prompt":"source","access":"read_only","routes":[{"alias":"senior"}]},
                {"id":"consumer","prompt":"consumer","access":"read_only","routes":[{"alias":"senior"}],"depends_on":["source"],"inputs":[{"node":"source","outputs":["findings"],"required_outputs":required}]}
            ]),
        )
    };
    let limits = crate::config::GraphLimits::default();
    assert!(make(1, json!(["findings"])).admit(&limits).is_err());
    let version = GRAPH_DOCUMENT_SCHEMA_VERSION as u32;
    assert!(make(version, json!(["findings"])).admit(&limits).is_ok());
    for required in [
        json!(["verdict"]),
        json!(["findings", "findings"]),
        json!(["result"]),
    ] {
        assert!(make(version, required).admit(&limits).is_err());
    }
    let composite = document(json!([
        {"id":"source","kind":"composite","access":"read_only","composite":{
            "nodes":[{"id":"member","prompt":"x","access":"read_only","routes":[{"alias":"senior"}]}],
            "exports":[{"node":"member","outputs":["findings"]}]
        }},
        {"id":"consumer","prompt":"consumer","access":"read_only","routes":[{"alias":"senior"}],"depends_on":["source"],"inputs":[{"node":"source","outputs":["findings"],"required_outputs":["findings"]}]}
    ]));
    assert!(composite.admit(&limits).is_ok());
    let mut unsupported = composite.clone();
    unsupported.nodes[1].inputs[0].outputs = vec![OutputKind::Verdict];
    unsupported.nodes[1].inputs[0].required_outputs = vec![OutputKind::Verdict];
    assert!(matches!(
        unsupported.admit(&limits).unwrap_err(),
        AdmissionError::OutputKind {
            field: GraphOutputField::Input,
            ..
        }
    ));
    let planner = document(json!([{
        "id":"plan", "kind":"task", "access":"read_only", "plan":{
            "kinds":[], "max_nodes":1,
            "patterns":[{"name":"p", "nodes":[], "exports":[{"node":"x","outputs":["findings"],"required_outputs":["findings"]}]}]
        }
    }]));
    assert!(matches!(
        validate_output_kinds(&planner).unwrap_err(),
        AdmissionError::OutputKind {
            field: GraphOutputField::CompositeExport,
            ..
        }
    ));
    for field in GraphOutputField::ALL {
        let error = match field {
            GraphOutputField::Input => unsupported.admit(&limits).unwrap_err(),
            GraphOutputField::CompositeExport => validate_output_kinds(&planner).unwrap_err(),
        };
        assert!(matches!(
            error,
            AdmissionError::OutputKind { field: actual, .. } if actual == *field
        ));
    }
    let prior_round = document(json!([{
        "id":"loop", "kind":"loop", "access":"read_only", "loop":{
            "max_rounds":2, "until":"work", "body":[{
                "id":"work", "prompt":"work", "access":"read_only", "routes":[{"alias":"senior"}],
                "inputs":[{"node":"previous.work","outputs":["findings"],"required_outputs":["findings"]}]
            }]
        }
    }]));
    assert!(matches!(
        prior_round.admit(&limits).unwrap_err(),
        AdmissionError::OutputKind {
            field: GraphOutputField::Input,
            ..
        }
    ));
}

#[test]
fn admission_refusals_keep_their_typed_causes() {
    let unsupported_schema =
        versioned_document(1, json!([{"id":"n","prompt":"x","access":"read_only"}]));
    let invalid_loop = document(json!([{"id":"loop","kind":"loop","access":"read_only"}]));
    let invalid_plan = document(json!([{"id":"n","access":"read_only","prompt":"x"}]));
    type Case<'a> = (
        &'a str,
        Box<dyn Fn() -> Result<(), AdmissionError> + 'a>,
        fn(&AdmissionError) -> bool,
    );
    let cases: [Case<'_>; 4] = [
        (
            "schema admission",
            Box::new(|| unsupported_schema.admit(&Default::default()).map(|_| ())),
            |error| matches!(error, AdmissionError::UnsupportedVersion { found: 1, supported } if *supported == GRAPH_DOCUMENT_SCHEMA_VERSION),
        ),
        (
            "composite expansion",
            Box::new(|| loops::expand(&invalid_loop, &[], &Default::default()).map(|_| ())),
            |error| matches!(error, AdmissionError::Composite(_)),
        ),
        (
            "composite admission",
            Box::new(|| invalid_loop.admit(&Default::default()).map(|_| ())),
            |error| matches!(error, AdmissionError::Composite(_)),
        ),
        (
            "flat plan",
            Box::new(|| validate_flat_plan(&invalid_plan, &Default::default()).map(|_| ())),
            |error| matches!(error, AdmissionError::Plan(_)),
        ),
    ];
    for (operation, run, has_expected_cause) in cases {
        let error = run().unwrap_err();
        assert!(has_expected_cause(&error), "{operation}: {error:?}");
    }
}

#[test]
fn writer_plan_refusal_names_the_node_and_missing_worktree() {
    let graph = document(json!([{
        "id":"implement",
        "prompt":"Fix the issue",
        "access":"worktree_write",
        "route":{"alias":"senior"}
    }]));
    let error = graph.admit(&Default::default()).unwrap_err();
    assert!(matches!(error, AdmissionError::Plan(_)));
}

#[test]
fn admission_schema_matrix() {
    let plain = json!([{"id":"n","prompt":"x","access":"read_only"}]);
    assert!(validate_schema(&document(plain.clone())).is_ok());
    for version in [0, 1, GRAPH_DOCUMENT_SCHEMA_VERSION as u32 + 1] {
        assert!(validate_schema(&versioned_document(version, plain.clone())).is_err());
    }
    for (version, expected) in [(1, false), (GRAPH_DOCUMENT_SCHEMA_VERSION as u32, true)] {
        let mut review_base = versioned_document(version, plain.clone());
        review_base.review_base = Some("base".into());
        assert_eq!(validate_schema(&review_base).is_ok(), expected);
    }

    let v2_node = json!({"id":"n","prompt":"x","access":"read_only","family_group":"g"});
    assert!(validate_schema(&document(json!([v2_node]))).is_ok());

    let nested = [
        json!({"id":"c","kind":"composite","access":"read_only","composite":{"nodes":[{"id":"n","prompt":"x","access":"read_only","family_group":"g"}],"exports":[{"node":"n","outputs":["result"]}]}}),
        json!({"id":"l","kind":"loop","access":"read_only","loop":{"body":[{"id":"n","prompt":"x","access":"read_only","family_group":"g"}],"until":"done","max_rounds":1}}),
        json!({"id":"p","kind":"task","access":"read_only","plan":{"kinds":[],"max_nodes":1,"patterns":[],"fallback":{"nodes":[{"id":"n","prompt":"x","access":"read_only","family_group":"g"}],"exports":[]}}}),
        json!({"id":"p","kind":"task","access":"read_only","plan":{"kinds":[],"max_nodes":1,"patterns":[{"name":"p","nodes":[{"id":"x","family_group":"g"}],"exports":[]}]} }),
    ];
    for node in nested {
        assert!(validate_schema(&document(json!([node]))).is_ok());
    }
}

#[test]
fn admission_output_kind_matrix() {
    for field in GraphOutputField::ALL {
        match field {
            GraphOutputField::Input | GraphOutputField::CompositeExport => {
                assert!(GraphOutputField::ALL.contains(field));
            }
        }
    }
    assert_eq!(validation::SUPPLIED_KIND_RULES.len(), OutputKind::ALL.len());
    for kind in OutputKind::ALL {
        match kind {
            OutputKind::Result
            | OutputKind::Findings
            | OutputKind::Verdict
            | OutputKind::Remaining
            | OutputKind::Branch
            | OutputKind::Commit => assert!(OutputKind::ALL.contains(kind)),
        }
        let (declared, rule) = validation::SUPPLIED_KIND_RULES
            .iter()
            .find(|(declared, _)| declared == kind)
            .expect("every output kind has one supply rule");
        let expected_rule = match kind {
            OutputKind::Result
            | OutputKind::Findings
            | OutputKind::Verdict
            | OutputKind::Remaining => validation::OutputSupplyRule::Always,
            OutputKind::Branch => validation::OutputSupplyRule::WriterOrLand,
            OutputKind::Commit => validation::OutputSupplyRule::WriterPinnedOrLand,
        };
        assert_eq!(*declared, *kind);
        assert_eq!(*rule, expected_rule);
    }
    for rule in validation::OutputSupplyRule::ALL {
        let used = validation::SUPPLIED_KIND_RULES
            .iter()
            .any(|(_, declared)| declared == rule);
        match rule {
            validation::OutputSupplyRule::Always
            | validation::OutputSupplyRule::WriterOrLand
            | validation::OutputSupplyRule::WriterPinnedOrLand => assert!(used),
        }
    }

    let leaf_shapes = [
        json!({"id":"s","access":"read_only"}),
        json!({"id":"s","access":"read_only","source":{"type":"commit","commit":"abc"}}),
        json!({"id":"s","access":"worktree_write","worktree":"/tmp/w"}),
        json!({"id":"s","access":"read_only","kind":"land","land":{"node":"w","tier":"routine"}}),
        json!({"id":"s","access":"read_only","kind":"loop","loop":{"body":[],"until":"done","max_rounds":1}}),
    ];
    for shape in leaf_shapes {
        let node: GraphNode = serde_json::from_value(shape).unwrap();
        let supplied = supplied_kinds(&node);
        for (kind, rule) in validation::SUPPLIED_KIND_RULES {
            let expected = match rule {
                validation::OutputSupplyRule::Always => true,
                validation::OutputSupplyRule::WriterOrLand => {
                    node.kind != Some(NodeKind::Loop)
                        && (node.access == AccessMode::WorktreeWrite
                            || node.kind == Some(NodeKind::Land))
                }
                validation::OutputSupplyRule::WriterPinnedOrLand => {
                    node.kind != Some(NodeKind::Loop)
                        && (node.access == AccessMode::WorktreeWrite
                            || node.source.is_some()
                            || node.kind == Some(NodeKind::Land))
                }
            };
            assert_eq!(supplied.contains(kind), expected, "{kind:?} in {node:?}");
        }
    }

    let inherited = document(json!([{
        "id":"group","access":"read_only","kind":"composite",
        "source":{"type":"commit","commit":"abc"},
        "composite":{"nodes":[{"id":"member","access":"read_only"}],"exports":[{"node":"member","outputs":["commit"]}]}
    }]));
    let expanded = loops::expand(&inherited, &[], &crate::config::GraphLimits::default()).unwrap();
    let member = expanded
        .nodes
        .iter()
        .find(|node| node.id == "group--member")
        .unwrap();
    assert!(supplied_kinds(member).contains(&OutputKind::Commit));

    let selected_commit = document(json!([
        {"id":"source","prompt":"source","access":"read_only"},
        {"id":"consumer","prompt":"consumer","access":"read_only","depends_on":["source"],"inputs":[{"node":"source","outputs":["commit"]}]}
    ]));
    let error = validate_output_kinds(&selected_commit).unwrap_err();
    assert!(matches!(
        error,
        AdmissionError::OutputKind {
            field: GraphOutputField::Input,
            ..
        }
    ));
}

#[test]
fn admission_composite_exports_are_declared_capabilities() {
    let council = json!({
        "id":"council","kind":"composite","access":"read_only",
        "source":{"type":"commit","commit":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},
        "composite":{"nodes":[{"id":"synthesis","kind":"synthesise","access":"read_only"}],
        "exports":[{"node":"synthesis","outputs":["result","findings","verdict","commit"]}]}
    });
    let document = document(json!([
        council,
        {"id":"writer","access":"read_only","depends_on":["council"],"inputs":[{"node":"council","outputs":["commit"]}]}
    ]));
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
        "schema_version":GRAPH_DOCUMENT_SCHEMA_VERSION,"graph_id":"bad-output-kind","nodes":[{"id":"c","kind":"composite","access":"read_only","composite":{"nodes":[{"id":"x","access":"read_only"}],"exports":[{"node":"x","outputs":["unknown"]}]}}]
    })).is_err());
}

#[test]
fn structural_limit_refusals_preserve_the_declared_key() {
    let graph = document(json!([
        {"id":"root","prompt":"root prompt","access":"read_only","routes":[{"alias":"senior"}]},
        {"id":"a","prompt":"child prompt","access":"read_only","depends_on":["root"],"routes":[{"alias":"senior"}]},
        {"id":"b","prompt":"child prompt","access":"read_only","depends_on":["root"],"routes":[{"alias":"senior"}]}
    ]));
    for key in GraphLimitKey::ALL {
        let configure: fn(&mut crate::config::GraphLimits) = match key {
            GraphLimitKey::Nodes => |limits| limits.nodes = 2,
            GraphLimitKey::Edges => |limits| limits.edges = 1,
            GraphLimitKey::RoutesPerNode => |limits| limits.routes_per_node = 0,
            GraphLimitKey::NodePromptBytes => |limits| limits.node_prompt_bytes = 1,
            GraphLimitKey::GraphPromptBytes => |limits| limits.graph_prompt_bytes = 1,
            GraphLimitKey::FanOutWidth => |limits| limits.fan_out_width = 1,
            GraphLimitKey::ExpansionDepth => |limits| limits.expansion_depth = 0,
        };
        let mut limits = crate::config::GraphLimits::default();
        configure(&mut limits);
        let refusal = graph.admit(&limits).unwrap_err();
        let AdmissionError::StructuralLimit(limit) = refusal else {
            panic!("expected structural limit for {key:?}");
        };
        assert_eq!(limit.key, *key);
    }
}

#[test]
fn cloud_placement_is_refused_at_admission_but_a_persisted_graph_still_replays() {
    for field in ["cloud", "placement"] {
        let graph = document(
            json!([{"id":"cloud","prompt":"p","access":"read_only","routes":[{"alias":"senior"}],
                "attempt":{field: {"target":"t","provider":"claude"}}}]),
        );
        let limits = crate::config::GraphLimits::default();
        assert!(matches!(graph.admit(&limits), Err(AdmissionError::Plan(_))));
        // Replay must plan, so the node reaches launch and blocks with the
        // typed refusal instead of failing every advance.
        assert!(validate_replay_plan(&graph, &limits).is_ok(), "{field}");
    }
}
