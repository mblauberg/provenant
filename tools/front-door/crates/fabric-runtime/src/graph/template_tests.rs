use super::template::*;
use super::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn library() -> TemplateLibrary {
    TemplateLibrary::bundled()
}

fn document(nodes: Vec<GraphNode>) -> GraphDocument {
    GraphDocument {
        review_base: None,
        context: None,
        extra: Default::default(),
        limits: None,
        integration_branch: Some("batch/fixture".into()),
        delivery: None,
        decision_session: None,
        schema_version: 1,
        graph_id: "templates".into(),
        nodes,
    }
}

fn limits() -> crate::config::GraphLimits {
    crate::config::GraphLimits {
        expansion_depth: 16,
        ..Default::default()
    }
}

fn expanded_ids(nodes: Vec<GraphNode>) -> Vec<String> {
    let document = document(nodes);
    document.admit(&limits()).unwrap();
    let mut ids: Vec<_> = document
        .expanded(&limits())
        .unwrap()
        .nodes
        .into_iter()
        .map(|node| node.id)
        .collect();
    ids.sort();
    ids
}

#[test]
fn every_bundled_template_parses_and_resolves() {
    let library = library();
    let names: Vec<_> = library.names().map(str::to_owned).collect();
    for name in &names {
        library.resolve(name).unwrap();
    }
    for expected in [
        "task",
        "plan",
        "explore",
        "research",
        "design",
        "implement",
        "implement.fix",
        "implement.plan",
        "implement.issue",
        "review",
        "review.security",
        "review.correctness",
        "review.tests",
        "review.contract",
        "review.design",
        "review.adversarial",
        "verify",
        "synthesise",
        "repair",
    ] {
        assert!(
            names.iter().any(|name| name == expected),
            "{expected} missing"
        );
        library.resolve(expected).unwrap();
        assert_eq!(library.layer(expected), Some(Layer::Bundled));
    }
}

#[test]
fn swarm_over_named_areas_is_one_node() {
    let bundled = library();
    let compiler = Compiler::new(&bundled);
    let swarm = compiler
        .compile(&json!({"id": "map", "use": "explore", "over": ["crates/run", "crates/fabric-runtime"]}))
        .unwrap();
    assert_eq!(swarm.kind, Some(NodeKind::Composite));
    let members = &swarm.composite.as_ref().unwrap().nodes;
    assert_eq!(
        members
            .iter()
            .map(|node| node.id.as_str())
            .collect::<Vec<_>>(),
        ["crates-run", "crates-fabric-runtime"]
    );
    assert!(members[0].prompt.contains("Map crates/run and cite"));
    assert!(
        members[1]
            .prompt
            .contains("Map crates/fabric-runtime and cite")
    );
    assert_eq!(
        members[0].route.as_ref().unwrap().alias.as_deref(),
        Some("junior")
    );
    assert_eq!(
        swarm.composite.as_ref().unwrap().exports.len(),
        2,
        "every copy exports its result"
    );
    assert_eq!(
        expanded_ids(vec![swarm]),
        ["map", "map--crates-fabric-runtime", "map--crates-run"]
    );
}

#[test]
fn over_binds_item_in_overrides_and_resolves_named_lists() {
    let library = library();
    let resolvers = BTreeMap::from([(
        "review-panel".to_owned(),
        vec!["security".to_owned(), "tests".to_owned()],
    )]);
    let compiler = Compiler {
        library: &library,
        resolvers: &resolvers,
    };
    let panel = compiler
        .compile(
            &json!({"id": "panel", "use": "review", "over": "@review-panel",
                         "with": {"lens": "${item}"}, "min_ok": 1}),
        )
        .unwrap();
    let composite = panel.composite.unwrap();
    assert_eq!(composite.min_successful, Some(1));
    assert!(composite.nodes[0].prompt.contains("security lens"));
    assert!(composite.nodes[1].prompt.contains("tests lens"));
    assert!(composite.nodes.iter().all(|node| node.require_findings));
    let error = Compiler::new(&library)
        .compile(&json!({"id": "panel", "use": "review", "over": "@missing"}))
        .unwrap_err();
    assert!(error.contains("resolver @missing"), "{error}");
    let error = Compiler::new(&library)
        .compile(&json!({"id": "loose", "use": "task", "brief": "Do ${item}"}))
        .unwrap_err();
    assert!(error.contains("only inside `over`"), "{error}");
}

#[test]
fn composite_template_from_a_repository_layer_nests_a_swarm() {
    let mut library = library();
    library
        .insert_toml(
            Layer::Repository,
            "map-and-plan",
            r#"
nodes = [
  { id = "map", use = "explore", over = ["crates/run", "crates/fabric-runtime"] },
  { id = "slices", use = "plan", after = ["map"] },
]
"#,
        )
        .unwrap();
    let node = Compiler::new(&library)
        .compile(&json!({"id": "mp", "use": "map-and-plan"}))
        .unwrap();
    let members = &node.composite.as_ref().unwrap().nodes;
    assert_eq!(members[0].kind, Some(NodeKind::Composite));
    assert_eq!(members[1].kind, Some(NodeKind::Plan));
    assert_eq!(members[1].depends_on, ["map"]);
    // Only the planner is a sink, so it alone exports.
    let exports = &node.composite.as_ref().unwrap().exports;
    assert_eq!(exports.len(), 1);
    assert_eq!(exports[0].node, "slices");
    assert_eq!(
        expanded_ids(vec![node]),
        [
            "mp",
            "mp--map",
            "mp--map--crates-fabric-runtime",
            "mp--map--crates-run",
            "mp--slices"
        ]
    );
}

#[test]
fn nested_composite_templates_compile_and_detect_recursion() {
    let mut library = library();
    library
        .insert_toml(
            Layer::User,
            "inner",
            r#"
nodes = [
  { id = "x", use = "explore", with = { item = "${area}" } },
  { id = "y", use = "synthesise", after = ["x"], inputs = [{ node = "x", outputs = ["result"] }] },
]
[parameters.area]
type = "string"
"#,
        )
        .unwrap();
    library
        .insert_toml(
            Layer::User,
            "outer",
            r#"
nodes = [
  { id = "a", use = "inner", with = { area = "crates/run" } },
  { id = "b", use = "task", after = ["a"], brief = "Act on the synthesis" },
]
"#,
        )
        .unwrap();
    let node = Compiler::new(&library)
        .compile(&json!({"id": "o", "use": "outer"}))
        .unwrap();
    let inner = &node.composite.as_ref().unwrap().nodes[0];
    let x = &inner.composite.as_ref().unwrap().nodes[0];
    assert!(x.prompt.contains("Map crates/run"));
    assert_eq!(
        expanded_ids(vec![node]),
        ["o", "o--a", "o--a--x", "o--a--y", "o--b"]
    );
    library
        .insert_toml(
            Layer::Inline,
            "loop-a",
            "nodes = [{ id = \"n\", use = \"loop-b\" }]",
        )
        .unwrap();
    library
        .insert_toml(
            Layer::Inline,
            "loop-b",
            "nodes = [{ id = \"n\", use = \"loop-a\" }]",
        )
        .unwrap();
    let error = Compiler::new(&library)
        .compile(&json!({"id": "r", "use": "loop-a"}))
        .unwrap_err();
    assert!(error.contains("uses itself"), "{error}");
}

#[test]
fn repeat_lowers_a_composite_template_to_a_loop() {
    let bundled = library();
    let node = Compiler::new(&bundled)
        .compile(
            &json!({"id": "fix", "use": "repair", "repeat": {"until": "accepted", "max_rounds": 2},
                         "with": {"worktree": "/fixture/writer", "write_scope": ["src"], "lens": "security"}}),
        )
        .unwrap();
    assert_eq!(node.kind, Some(NodeKind::Loop));
    let body = &node.loop_body.as_ref().unwrap().body;
    assert_eq!(
        body.iter().map(|node| node.id.as_str()).collect::<Vec<_>>(),
        ["fix", "check", "accepted"]
    );
    assert_eq!(body[0].access, AccessMode::WorktreeWrite);
    assert_eq!(body[0].write_scope, ["src"]);
    assert_eq!(body[0].inputs[0].node, "previous.check");
    assert!(body[1].prompt.contains("security lens"));
    assert!(matches!(&body[1].source, Some(SourceBinding::Node { node }) if node == "fix"));
    let ids = expanded_ids(vec![node]);
    assert!(ids.contains(&"fix--r2--accepted".to_owned()), "{ids:?}");
}

#[test]
fn flavours_and_layers_resolve_in_order() {
    let mut library = library();
    let security = library.resolve("review.security").unwrap();
    assert_eq!(security.parameters["lens"].default, Some(json!("security")));
    assert_eq!(security.tier.as_deref(), Some("senior"));
    let node = Compiler::new(&library)
        .compile(&json!({"id": "sec", "use": "review.security"}))
        .unwrap();
    assert!(node.prompt.contains("security lens"));
    assert_eq!(node.extra.get("template"), Some(&json!("review.security")));
    assert!(library.resolve("review.missing").is_err());

    library
        .insert_toml(Layer::User, "task", "brief = \"user\"\ntier = \"junior\"")
        .unwrap();
    library
        .insert_toml(
            Layer::Repository,
            "task",
            "brief = \"repository\"\ntier = \"senior\"",
        )
        .unwrap();
    assert_eq!(library.layer("task"), Some(Layer::Repository));
    let node = Compiler::new(&library)
        .compile(&json!({"id": "t", "use": "task", "tier": "principal"}))
        .unwrap();
    assert_eq!(node.prompt, "repository");
    assert_eq!(node.route.unwrap().alias.as_deref(), Some("principal"));
}

#[test]
fn raw_nodes_pass_through_unchanged() {
    let bundled = library();
    let raw = json!({"id": "lane", "access": "read_only", "prompt": "Uses ${literal} text",
                     "route": {"alias": "senior"},
                     "composite": null});
    let lowered = Compiler::new(&bundled).lower(&raw).unwrap();
    assert_eq!(lowered, raw);
    let composite = json!({"id": "c", "kind": "composite", "access": "read_only",
        "composite": {"nodes": [{"id": "x", "use": "task"}], "exports": [{"node": "x", "outputs": ["result"]}]}});
    let lowered = Compiler::new(&bundled).lower(&composite).unwrap();
    assert_eq!(
        lowered.pointer("/composite/nodes/0/route/alias"),
        Some(&json!("senior"))
    );
}

#[test]
fn templates_refuse_undeclared_parameters_and_authority_by_parameter() {
    assert!(
        Template::from_toml("brief = \"Do ${missing}\"")
            .unwrap_err()
            .contains("undeclared parameter missing")
    );
    let widening: Template = serde_json::from_value(json!({
        "name": "wide", "parameters": {"mode": {"type": "string"}},
        "nodes": [{"id": "n", "access": "${mode}", "prompt": "x"}],
    }))
    .unwrap();
    assert!(widening.check_authority().is_err());
    let value: Value = serde_json::to_value(&widening).unwrap();
    assert_eq!(value["name"], "wide");
}
