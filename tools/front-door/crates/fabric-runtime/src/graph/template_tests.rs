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
        "steward",
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
    let resolvers = BTreeMap::from([
        (
            "review-panel".to_owned(),
            vec!["security".to_owned(), "tests".to_owned()],
        ),
        ("review-advice-security".to_owned(), vec![String::new()]),
        ("review-advice-tests".to_owned(), vec![String::new()]),
    ]);
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
        .unwrap_err()
        .to_string();
    assert!(error.contains("resolver @missing"), "{error}");
    let error = Compiler::new(&library)
        .compile(&json!({"id": "loose", "use": "task", "brief": "Do ${item}"}))
        .unwrap_err()
        .to_string();
    assert!(error.contains("only inside `over`"), "{error}");
}

#[test]
fn review_panel_binds_per_lens_resolver_values() {
    let library = library();
    let resolvers = BTreeMap::from([
        ("review-topology".to_owned(), vec!["f1-v1-s1".to_owned()]),
        ("review-verifiers".to_owned(), vec!["1".to_owned()]),
        (
            "review-panel".to_owned(),
            vec!["security".to_owned(), "tests".to_owned()],
        ),
        ("review-advice-security".to_owned(), vec![String::new()]),
        (
            "review-advice-tests".to_owned(),
            vec!["mutation guidance".to_owned()],
        ),
    ]);
    let compiler = Compiler {
        library: &library,
        resolvers: &resolvers,
    };
    let node = compiler
        .compile(&json!({"id":"panel", "use":"review-panel"}))
        .unwrap();
    document(vec![node.clone()]).admit(&limits()).unwrap();
    let panel = node.composite.unwrap();
    let selected = panel.nodes[0].composite.as_ref().unwrap();
    let shape = selected.nodes[0].composite.as_ref().unwrap();
    let group = shape.nodes.iter().find(|node| node.id == "lenses").unwrap();
    let lenses = &group.composite.as_ref().unwrap().nodes;
    assert!(!lenses[0].prompt.contains("mutation guidance"));
    assert!(lenses[1].prompt.contains("mutation guidance"));
    assert_eq!(group.depends_on, ["frame"]);
    let verify = shape.nodes.iter().find(|node| node.id == "verify").unwrap();
    assert_eq!(verify.depends_on, ["lenses"]);
    let synthesis = shape
        .nodes
        .iter()
        .find(|node| node.id == "synthesis")
        .unwrap();
    assert!(synthesis.depends_on.iter().any(|id| id == "verify"));
    for frame in [0, 1] {
        for verifier in [0, 1] {
            for synthesis in [0, 1] {
                let mut selected = resolvers.clone();
                selected.insert(
                    "review-topology".into(),
                    vec![format!("f{frame}-v{verifier}-s{synthesis}")],
                );
                let node = Compiler {
                    library: &library,
                    resolvers: &selected,
                }
                .compile(&json!({"id":"panel", "use":"review-panel"}))
                .unwrap();
                let document = document(vec![node]);
                document.admit(&limits()).unwrap();
                let expanded = document.expanded(&limits()).unwrap();
                for (stage, expected) in [
                    ("frame", frame),
                    ("verify", verifier),
                    ("synthesis", synthesis),
                ] {
                    assert_eq!(
                        expanded
                            .nodes
                            .iter()
                            .any(|node| node.id.ends_with(&format!("--{stage}"))),
                        expected == 1,
                        "{frame}/{verifier}/{synthesis}: {stage}"
                    );
                }
            }
        }
    }
    let mut two = resolvers.clone();
    two.insert("review-verifiers".into(), vec!["1".into(), "2".into()]);
    let node = Compiler {
        library: &library,
        resolvers: &two,
    }
    .compile(&json!({"id":"panel", "use":"review-panel"}))
    .unwrap();
    let panel = node.composite.unwrap();
    let selected = panel.nodes[0].composite.as_ref().unwrap();
    let shape = selected.nodes[0].composite.as_ref().unwrap();
    let verifier = shape.nodes.iter().find(|node| node.id == "verify").unwrap();
    assert_eq!(verifier.composite.as_ref().unwrap().nodes.len(), 2);
    let mut single = resolvers.clone();
    single.insert("review-topology".into(), vec!["f0-v0-s0".into()]);
    single.insert("review-panel".into(), vec!["tests".into()]);
    let mut node = Compiler {
        library: &library,
        resolvers: &single,
    }
    .compile(&json!({"id":"panel", "use":"review-panel"}))
    .unwrap();
    node.source = Some(SourceBinding::Commit {
        commit: "0123456789abcdef0123456789abcdef01234567".into(),
    });
    let expanded = document(vec![node]).expanded(&limits()).unwrap();
    let leaves: Vec<_> = expanded
        .nodes
        .iter()
        .filter(|node| node.composite.is_none())
        .collect();
    assert_eq!(leaves.len(), 1);
    assert!(
        matches!(&leaves[0].source, Some(SourceBinding::Commit { commit }) if commit == "0123456789abcdef0123456789abcdef01234567")
    );
    let missing = BTreeMap::from([
        ("review-topology".to_owned(), vec!["f0-v0-s0".to_owned()]),
        ("review-panel".to_owned(), vec!["tests".to_owned()]),
    ]);
    assert!(
        Compiler {
            library: &library,
            resolvers: &missing
        }
        .compile(&json!({"id":"panel", "use":"review-panel"}))
        .is_err()
    );
}

#[test]
fn review_panel_resolver_keeps_each_route_and_family_group() {
    let bundled = library();
    let resolvers = BTreeMap::from([
        ("review-panel".to_owned(), vec!["correctness".to_owned(), "security".to_owned()]),
        ("review-panel-routes".to_owned(), vec![
            r#"{"route":{"alias":"senior"},"family_group":"group-0"}"#.to_owned(),
            r#"{"routes":[{"alias":"principal"},{"alias":"principal"}],"family_group":"group-1"}"#.to_owned(),
        ]),
    ]);
    let compiled = Compiler {
        library: &bundled,
        resolvers: &resolvers,
    }
    .compile(&json!({
        "id":"panel", "use":"review", "over":"@review-panel", "with":{"lens":"${item}"}
    }))
    .unwrap();
    let members = &compiled.composite.unwrap().nodes;
    assert_eq!(
        members[0].route.as_ref().unwrap().alias.as_deref(),
        Some("senior")
    );
    assert_eq!(members[0].family_group.as_deref(), Some("group-0"));
    assert_eq!(
        members[1].routes.as_ref().unwrap()[0].alias.as_deref(),
        Some("principal")
    );
    assert_eq!(members[1].family_group.as_deref(), Some("group-1"));
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
    assert_eq!(members[1].kind, Some(NodeKind::Task));
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
        .unwrap_err()
        .to_string();
    assert!(error.contains("uses itself"), "{error}");
}

#[test]
fn repeat_lowers_a_composite_template_to_a_loop() {
    let bundled = library();
    let node = Compiler::new(&bundled)
        .compile(
            &json!({"id": "fix", "use": "repair", "repeat": {"until": "accepted", "max_rounds": 2},
                         "with": {"worktree": "/fixture/writer", "write_scope": ["src"], "lenses": ["security"]}}),
        )
        .unwrap();
    assert_eq!(node.kind, Some(NodeKind::Loop));
    let body = &node.loop_body.as_ref().unwrap().body;
    assert_eq!(
        body.iter().map(|node| node.id.as_str()).collect::<Vec<_>>(),
        ["fix", "check", "synthesis", "accepted"]
    );
    assert_eq!(body[0].access, AccessMode::WorktreeWrite);
    assert_eq!(body[0].write_scope, ["src"]);
    assert_eq!(body[0].inputs[0].node, "previous.synthesis");
    assert!(
        body[1].composite.as_ref().unwrap().nodes[0]
            .prompt
            .contains("security lens")
    );
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

#[test]
fn review_templates_record_the_raising_lens() {
    let bundled = library();
    for name in ["review", "review.tests", "review.security"] {
        let lowered = Compiler::new(&bundled)
            .lower(&json!({"id":"check", "use":name}))
            .unwrap();
        let lens = lowered["review_lens"].as_str().unwrap();
        assert!(!lens.is_empty());
        assert!(lowered["prompt"].as_str().unwrap().contains(lens));
        assert_eq!(lowered["require_findings"], true);
    }
}

#[test]
fn delivery_templates_admit_bounded_review_repairs() {
    let bundled = library();
    let resolvers = BTreeMap::from([
        ("review-topology".to_owned(), vec!["f1-v1-s1".to_owned()]),
        ("review-verifiers".to_owned(), vec!["1".to_owned()]),
        (
            "review-panel".to_owned(),
            vec!["security".to_owned(), "tests".to_owned()],
        ),
        ("review-advice-security".to_owned(), vec![String::new()]),
        ("review-advice-tests".to_owned(), vec![String::new()]),
    ]);
    let compiler = Compiler {
        library: &bundled,
        resolvers: &resolvers,
    };
    for template in ["deliver", "batch"] {
        let node = compiler
            .compile(&json!({"id":"delivery", "use":template,
            "with":{"worktree":"/fixture/writer", "write_scope":["src"], "max_repair_cycles":2}}))
            .unwrap();
        let mut doc = document(vec![node]);
        if template == "batch" {
            doc.integration_branch = Some("batch/integration".into());
        }
        let delivery_limits = crate::config::GraphLimits::default();
        doc.admit(&delivery_limits).unwrap();
        let expanded = doc.expanded(&delivery_limits).unwrap();
        let delta = expanded
            .nodes
            .iter()
            .find(|node| {
                node.id.contains("repair--cycles--r1--check--")
                    && node.extra.contains_key("repair_lens")
            })
            .unwrap();
        assert_eq!(delta.extra["repair_risk_assessment"], true);
        assert!(delta.prompt.contains("top-level risk_tier"));
        let repair = expanded
            .nodes
            .iter()
            .find(|node| node.id.ends_with("repair--cycles"))
            .unwrap();
        let policy = repair.loop_body.as_ref().unwrap();
        assert_eq!(policy.max_rounds, 2);
        assert_eq!(policy.extra["review_repair"], true);
        assert!(repair.on_fail.as_ref().unwrap().ends_with("design"));
        let design = expanded
            .nodes
            .iter()
            .find(|node| node.id.ends_with("repair--design"))
            .unwrap();
        assert_eq!(design.access, AccessMode::ReadOnly);
        assert_eq!(
            design.route.as_ref().unwrap().alias.as_deref(),
            Some("principal")
        );
        let continuation = expanded
            .nodes
            .iter()
            .find(|node| node.id.ends_with("repair--continue"))
            .unwrap();
        let slice = &continuation.plan.as_ref().unwrap().templates[0];
        assert_eq!(slice.name.as_deref(), Some("repair-slice"));
        assert_eq!(slice.node["write_scope"], json!(["src"]));
        assert_eq!(slice.node["worktree"], "/fixture/writer");
        assert_eq!(slice.node["extensions"][TEMPLATE_FIELD], "implement.fix");
        let mut slices = library();
        slices
            .insert(Layer::Inline, "repair-slice", slice.clone())
            .unwrap();
        let compiled = Compiler::new(&slices)
            .compile(&json!({"id":"slice", "use":"repair-slice"}))
            .unwrap();
        assert_eq!(compiled.extra[TEMPLATE_FIELD], "implement.fix");
        if template == "deliver" {
            let accept = expanded
                .nodes
                .iter()
                .find(|node| node.id == "delivery--accept")
                .unwrap();
            assert!(
                matches!(&accept.condition, Some(GateCondition::Verdict { expected, threshold: VerdictThreshold::All, .. }) if expected == "VERDICT: PASS")
            );
            let review = expanded
                .nodes
                .iter()
                .find(|node| node.id == "delivery--review--selected--f1-v1-s1--lenses--security")
                .unwrap();
            assert!(
                matches!(&review.source, Some(SourceBinding::Head { node }) if node == "delivery--implement")
            );
            assert!(
                expanded
                    .nodes
                    .iter()
                    .any(|node| node.id == "delivery--land" && node.land.is_some())
            );
        }
    }
}

#[test]
fn review_cycle_limit_is_configurable_with_no_upper_ceiling() {
    let bundled = library();
    let error = Compiler::new(&bundled).compile(&json!({"id":"repair", "use":"review-repair",
        "with":{"worktree":"/fixture/writer", "write_scope":["src"], "max_repair_cycles":0}})).unwrap_err().to_string();
    assert!(error.contains("at least 1"), "{error}");
    let node = Compiler::new(&bundled)
        .compile(&json!({"id":"repair", "use":"repair",
        "with":{"worktree":"/fixture/writer", "write_scope":["src"]},
        "repeat":{"until":"accepted", "max_rounds":5}}))
        .unwrap();
    assert_eq!(node.loop_body.as_ref().unwrap().max_rounds, 5);
    let node = Compiler::new(&bundled).compile(&json!({"id":"repair", "use":"review-repair",
        "with":{"worktree":"/fixture/writer", "write_scope":["src"], "max_repair_cycles":5}}));
    assert!(node.is_ok(), "{:?}", node.err());
}

#[test]
fn review_behaviour_survives_brief_and_output_overrides() {
    let bundled = library();
    for (name, behaviour) in [
        ("review.tests", "lens"),
        ("review.verify", "verify"),
        ("review.synthesis", "synthesis"),
    ] {
        let node = Compiler::new(&bundled)
            .compile(&json!({
                "id":"review", "use":name, "brief":"Inspect this", "require_findings":false,
                "required_verdict":"anything", "role":"task"
            }))
            .unwrap();
        assert!(node.role.is_none());
        assert_eq!(node.extra["review_behaviour"], behaviour);
        assert!(node.require_findings);
        assert_eq!(node.required_verdict.as_deref(), Some("VERDICT: PASS"));
        if name != "review.tests" {
            assert!(!node.extra.contains_key("review_lens"));
        }
        if name == "review.synthesis" {
            assert_eq!(node.kind, Some(NodeKind::Synthesise));
            assert_eq!(node.dispositions, ["accepted", "rejected", "deferred"]);
        }
    }
    assert!(Template::from_toml("review = 'unknown'").is_err());
}

#[test]
fn writer_template_can_disable_no_work_continuation() {
    let mut templates = library();
    let custom = Template::from_toml(
        r#"
        brief = "Finish the accepted brief"
        tier = "junior"
        access = "worktree_write"
        [node]
        auto_continue_no_work = false
    "#,
    )
    .unwrap();
    templates = templates
        .with(
            Layer::Repository,
            &BTreeMap::from([("custom-writer".into(), custom)]),
        )
        .unwrap();
    let compiled = Compiler::new(&templates)
        .compile(&json!({"id":"work", "use":"custom-writer", "worktree":".worktrees/work"}))
        .unwrap();
    assert_eq!(compiled.auto_continue_no_work, Some(false));
    assert!(
        Compiler::new(&templates)
            .compile(&json!({"id":"work", "use":"custom-writer", "auto_continue_no_work":"false"}))
            .is_err()
    );
}

#[test]
fn template_node_fields_are_checked_and_extensions_are_explicit() {
    let mut library = library();
    library
        .insert_toml(Layer::Inline, "typo", "[node]\nmisspelt_field = true")
        .unwrap();
    let error = Compiler::new(&library)
        .compile(&json!({"id":"stage", "use":"typo"}))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("stage") && error.contains("misspelt_field"),
        "{error}"
    );
    library
        .insert_toml(
            Layer::Inline,
            "extension",
            "[node.extensions]\ncustom = true",
        )
        .unwrap();
    assert_eq!(
        Compiler::new(&library)
            .compile(&json!({"id":"stage", "use":"extension"}))
            .unwrap()
            .extra["custom"],
        true
    );
}

#[test]
fn renamed_composites_keep_their_behaviour_and_bindings() {
    let mut library = library();
    for name in ["review-panel", "repair", "review-repair"] {
        let copy = library.resolve(name).unwrap();
        library
            .insert(Layer::Inline, &format!("copy-{name}"), copy)
            .unwrap();
    }
    let resolvers = BTreeMap::from([
        ("review-topology".into(), vec!["f0-v0-s0".into()]),
        ("review-panel".into(), vec!["tests".into()]),
        ("review-advice-tests".into(), vec![String::new()]),
    ]);
    let compiler = Compiler {
        library: &library,
        resolvers: &resolvers,
    };
    for (name, extra) in [
        ("review-panel", json!({"repair_panel":true})),
        (
            "repair",
            json!({"with":{"worktree":"/fixture", "write_scope":["src"], "integration_branch":"batch/x"}, "repeat":{"until":"accepted", "max_rounds":2}}),
        ),
        (
            "review-repair",
            json!({"with":{"worktree":"/fixture", "write_scope":["src"]}}),
        ),
    ] {
        let mut original = json!({"id":"stage", "use":name});
        original
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let mut renamed = original.clone();
        renamed["use"] = format!("copy-{name}").into();
        assert_eq!(
            compiler.compile(&original).unwrap(),
            compiler.compile(&renamed).unwrap(),
            "{name}"
        );
    }
}

#[test]
fn bundled_template_expansions_match_golden() {
    let library = library();
    let resolvers = BTreeMap::from([
        ("review-topology".into(), vec!["f1-v1-s1".into()]),
        ("review-verifiers".into(), vec!["1".into()]),
        (
            "review-panel".into(),
            vec!["correctness".into(), "tests".into()],
        ),
        ("review-advice-correctness".into(), vec![String::new()]),
        ("review-advice-tests".into(), vec![String::new()]),
    ]);
    let compiler = Compiler {
        library: &library,
        resolvers: &resolvers,
    };
    // Compare against the assets, not just the successfully loaded catalogue.
    let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/templates");
    let mut names = std::collections::BTreeSet::new();
    for entry in std::fs::read_dir(assets).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_stem().unwrap().to_str().unwrap();
        if name == "composites" {
            let composites: BTreeMap<String, Template> =
                toml_edit::de::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            names.extend(composites.into_keys());
        } else {
            names.insert(name.to_owned());
        }
    }
    assert_eq!(names, library.names().map(str::to_owned).collect());
    let mut expansions = BTreeMap::new();
    for name in library.names() {
        let template = library.resolve(name).unwrap();
        let mut bindings = serde_json::Map::new();
        for (key, parameter) in &template.parameters {
            if key == "write_scope" {
                bindings.insert(key.clone(), json!(["src"]));
            } else if key == "worktree" {
                bindings.insert(key.clone(), json!("/fixture/writer"));
            } else if parameter.default.is_none() {
                let value = match parameter.kind {
                    ParameterKind::String => json!("/fixture/writer"),
                    ParameterKind::List => json!(["src"]),
                    other => panic!("no golden fixture for {name}.{key}: {other:?}"),
                };
                bindings.insert(key.clone(), value);
            }
        }
        let node = compiler
            .compile(&json!({"id":"stage", "use":name, "with":bindings}))
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        expansions.insert(name, node);
    }
    let actual = serde_json::to_string_pretty(&expansions).unwrap() + "\n";
    if let Ok(path) = std::env::var("PROVENANT_GOLDEN_OUTPUT") {
        std::fs::write(path, &actual).unwrap();
    }
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&actual).unwrap(),
        serde_json::from_str::<serde_json::Value>(include_str!(
            "../../assets/template-expansions.json"
        ))
        .unwrap()
    );
}

#[test]
fn malformed_review_repair_behaviour_reports_an_error() {
    let mut library = library();
    library
        .insert_toml(
            Layer::Inline,
            "broken",
            r#"
composite_behaviour = "review_repair"
nodes = [{ id = "first", use = "task" }]
"#,
        )
        .unwrap();
    let error = Compiler::new(&library)
        .compile(&json!({"id":"stage", "use":"broken"}))
        .unwrap_err()
        .to_string();
    assert!(error.contains("review_repair behaviour"), "{error}");
}

#[test]
fn node_overrides_reject_unknown_fields() {
    let library = library();
    let error = Compiler::new(&library)
        .compile(&json!({"id":"stage", "use":"task", "deadine_seconds":30}))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("stage") && error.contains("deadine_seconds"),
        "{error}"
    );
    let node = Compiler::new(&library)
        .compile(&json!({"id":"stage", "use":"task", "deadline_seconds":30,
            "extensions":{"custom":true}}))
        .unwrap();
    assert_eq!(node.deadline_seconds, Some(30));
    assert_eq!(node.extra["custom"], true);
}

#[test]
fn composite_members_reject_unknown_fields() {
    for member in [
        json!({"id":"work", "use":"task", "deadine_seconds":30}),
        json!({"id":"work", "access":"read_only", "deadine_seconds":30}),
        json!({"id":"group", "access":"read_only", "composite":{"nodes":[
            {"id":"work", "access":"read_only", "deadine_seconds":30}
        ]}}),
        json!({"id":"group", "access":"read_only", "loop":{"body":[
            {"id":"work", "access":"read_only", "deadine_seconds":30}
        ], "until":"work", "max_rounds":1}}),
    ] {
        let mut library = library();
        library
            .insert(
                Layer::Repository,
                "local",
                serde_json::from_value(json!({
                    "nodes":[member]
                }))
                .unwrap(),
            )
            .unwrap();
        let error = Compiler::new(&library)
            .compile(&json!({"id":"stage", "use":"local"}))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("work") && error.contains("deadine_seconds"),
            "{error}"
        );
    }
}

#[test]
fn review_flavour_merges_extension_entries() {
    let mut library = library();
    library
        .insert_toml(
            Layer::Repository,
            "review.local",
            r#"
[parameters.lens]
type = "string"
default = "local"
[node.extensions]
custom = true
"#,
        )
        .unwrap();
    let node = Compiler::new(&library)
        .compile(&json!({"id":"stage", "use":"review.local"}))
        .unwrap();
    assert_eq!(node.extra["custom"], true);
    assert_eq!(node.extra["review_lens"], "local");
    library
        .insert_toml(
            Layer::Repository,
            "review.local.explicit",
            r#"
[node.extensions]
review_lens = "explicit"
"#,
        )
        .unwrap();
    let node = Compiler::new(&library)
        .compile(&json!({"id":"stage", "use":"review.local.explicit"}))
        .unwrap();
    assert_eq!(node.extra["custom"], true);
    assert_eq!(node.extra["review_lens"], "explicit");
}

#[test]
fn extensions_cannot_supply_compiled_node_fields() {
    for field in [
        "read_roots",
        "access",
        "source",
        "route",
        "deadline_seconds",
        "use",
    ] {
        let template: Template = serde_json::from_value(json!({
            "parameters":{"root":{"type":"string"}},
            "node":{"extensions":{field:["${root}"]}}
        }))
        .unwrap();
        let error = template.check_authority().unwrap_err();
        assert!(
            error.contains(field) && error.contains("extensions"),
            "{error}"
        );
        let mut library = library();
        library
            .insert(Layer::Repository, "local", template)
            .unwrap();
        let error = Compiler::new(&library)
            .compile(&json!({"id":"stage", "use":"local", "with":{"root":"/any/root"}}))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(field) && error.contains("extensions"),
            "{error}"
        );
    }
    let mut template: Template = serde_json::from_value(json!({
        "parameters":{"root":{"type":"string"}},
        "nodes":[{"id":"work", "use":"task", "extensions":{"read_roots":["${root}"]}}]
    }))
    .unwrap();
    assert!(
        template
            .check_authority()
            .unwrap_err()
            .contains("read_roots")
    );
    template.nodes[0] = json!({"id":"work", "use":"task", "extensions":{"custom":"${root}"}});
    template.check_authority().unwrap();
}

#[test]
fn leaf_node_tables_reject_instance_only_syntax() {
    for field in ["use", "with", "over", "repeat", "min_ok", "reduce_verdict"] {
        let mut library = library();
        library
            .insert(
                Layer::Inline,
                "invalid",
                serde_json::from_value(json!({
                    "node":{field:"ignored"}
                }))
                .unwrap(),
            )
            .unwrap();
        let error = Compiler::new(&library)
            .compile(&json!({"id":"stage", "use":"invalid"}))
            .unwrap_err()
            .to_string();
        assert!(error.contains("stage") && error.contains(field), "{error}");
    }
}

#[test]
fn parameterised_composite_members_reject_unknown_fields() {
    let mut library = library();
    library
        .insert(
            Layer::Inline,
            "local",
            serde_json::from_value(json!({
                "parameters":{"members":{"type":"list"}},
                "node":{"kind":"composite", "composite":{"nodes":"${members}"}}
            }))
            .unwrap(),
        )
        .unwrap();
    let error = Compiler::new(&library)
        .compile(&json!({"id":"stage", "use":"local", "with":{"members":[
            {"id":"work", "access":"read_only", "deadine_seconds":30}
        ]}}))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("work") && error.contains("deadine_seconds"),
        "{error}"
    );
}

#[test]
fn shorthand_document_controls_are_not_node_overrides() {
    let library = TemplateLibrary::bundled();
    let compiler = Compiler::new(&library);
    let mut node = json!({"id":"task", "use":"task", "limits":{"nodes":10}, "templates":{}});
    separate_document_fields(&mut node).unwrap();
    assert!(compiler.compile(&node).is_ok());
    node["unknown_node_field"] = json!(true);
    assert!(
        compiler
            .compile(&node)
            .unwrap_err()
            .to_string()
            .contains("unknown field unknown_node_field")
    );
}

#[test]
fn shorthand_schema_version_must_be_a_supported_integer() {
    for version in [
        json!(0),
        json!(3),
        json!(-1),
        json!(1.5),
        json!("1"),
        json!(null),
        json!(true),
    ] {
        let mut node = json!({"use":"task", "schema_version":version});
        let error = separate_document_fields(&mut node).unwrap_err();
        assert!(error.contains("schema_version"), "{error}");
        assert_eq!(node["schema_version"], version);
    }
    for version in [1, 2] {
        let mut node = json!({"use":"task", "schema_version":version});
        separate_document_fields(&mut node).unwrap();
        assert!(node.get("schema_version").is_none());
    }
}

// Small: the bundled catalogue compiles the proposal-mode contract.
#[test]
fn steward_compiles_with_junior_route_and_proposal_limits() {
    let library = library();
    let node = Compiler::new(&library)
        .compile(&json!({"id":"sweep", "use":"steward"}))
        .unwrap();
    let compiled = serde_json::to_value(&node).unwrap();
    assert_eq!(compiled["route"]["alias"], "junior");
    assert_eq!(node.access, AccessMode::ReadOnly);
    for limit in [
        "is:issue",
        "search_type=hybrid",
        "Never close an",
        "only permitted tracker write",
        "newer issue",
        "High-confidence parent links and thin bodies",
        "run output only",
        "<!-- provenant-steward:duplicate-of=",
        "paginate",
    ] {
        assert!(
            node.prompt.contains(limit),
            "missing proposal limit: {limit}"
        );
    }
}

fn parameter_library() -> TemplateLibrary {
    let mut library = library();
    library
        .insert_toml(
            Layer::Inline,
            "gate",
            r#"
brief = "Gate ${subject} at ${retries} tries"
tier = "junior"
[parameters.subject]
type = "string"
[parameters.review_base]
type = "string"
default = "main"
[parameters.retries]
type = "integer"
min = 1
max = 5
default = 2
"#,
        )
        .unwrap();
    library
        .insert_toml(
            Layer::Inline,
            "gate-pair",
            r#"
nodes = [
  { id = "check", use = "level", with = { level = "${mode}" } },
]
[parameters.mode]
type = "string"
"#,
        )
        .unwrap();
    library
        .insert_toml(
            Layer::Inline,
            "level",
            r#"
brief = "Check at ${level}"
tier = "junior"
[parameters.level]
type = "string"
enum = ["low", "high"]
"#,
        )
        .unwrap();
    library
}

#[test]
fn unknown_parameter_names_the_key_and_suggests_the_nearest_declared_name() {
    let library = parameter_library();
    let compiler = Compiler::new(&library);
    let refusal = compiler
        .compile(&json!({"id":"gate", "use":"gate", "with":{"subject.commit":"abc"}}))
        .unwrap_err()
        .to_string();
    assert!(
        refusal.starts_with("node gate: template gate has no parameter subject.commit"),
        "{refusal}"
    );
    assert!(
        refusal.contains("accepted parameters: retries, review_base, subject"),
        "{refusal}"
    );
    assert!(refusal.ends_with("did you mean subject?"), "{refusal}");

    let refusal = compiler
        .compile(&json!({"id":"gate", "use":"gate", "with":{"nonsense_key":"x"}}))
        .unwrap_err()
        .to_string();
    assert!(
        refusal.contains("has no parameter nonsense_key"),
        "{refusal}"
    );
    assert!(!refusal.contains("did you mean"), "{refusal}");

    // A key that is not an identifier is not echoed, so a pasted secret stays out.
    let refusal = compiler
        .compile(&json!({"id":"gate", "use":"gate", "with":{"sk ant secret":"x"}}))
        .unwrap_err()
        .to_string();
    assert!(!refusal.contains("sk ant secret"), "{refusal}");
    assert!(refusal.contains("has no such parameter"), "{refusal}");
}

#[test]
fn wrong_shape_names_the_key_and_the_declared_shape_without_the_value() {
    let library = parameter_library();
    let compiler = Compiler::new(&library);
    let refusal = compiler
        .compile(&json!({"id":"gate", "use":"gate", "with":{"subject":"x", "retries":9}}))
        .unwrap_err();
    assert_eq!(
        refusal.to_string(),
        "node gate: template gate parameter retries expects an integer from 1 to 5"
    );
    let refusal = compiler
        .compile(&json!({
            "id":"gate", "use":"gate",
            "with":{"subject":"x", "retries":"SECRET-VALUE-7"}
        }))
        .unwrap_err()
        .to_string();
    assert!(refusal.contains("retries expects an integer"), "{refusal}");
    assert!(!refusal.contains("SECRET-VALUE-7"), "{refusal}");
}

#[test]
fn failing_node_inside_a_composite_is_named() {
    let library = parameter_library();
    let refusal = Compiler::new(&library)
        .compile(&json!({"id":"pair", "use":"gate-pair", "with":{"mode":"medium"}}))
        .unwrap_err();
    let CompileError::Parameter(ParameterRefusal {
        node,
        template,
        fault,
        ..
    }) = &refusal
    else {
        panic!("expected a parameter refusal, got {refusal}");
    };
    assert_eq!(node.as_deref(), Some("check"));
    assert_eq!(template, "level");
    assert!(matches!(fault, ParameterFault::Shape { key, .. } if key == "level"));
    assert_eq!(
        refusal.to_string(),
        "node check: template level parameter level expects a string, one of [\"low\",\"high\"]"
    );
}

#[test]
fn template_launch_missing_parameters_names_them() {
    let bundled = TemplateLibrary::bundled();
    let compiler = Compiler::new(&bundled);
    let error = compiler
        .compile(
            &serde_json::json!({"id": "d", "use": "deliver", "with": {"worktree":"/some/path"}}),
        )
        .unwrap_err();
    let CompileError::Parameter(refusal) = error else {
        panic!("expected a parameter refusal: {error}");
    };
    let text = refusal.to_string();
    assert!(text.contains("write_scope"), "{text}");
    assert!(text.contains("example:"), "{text}");
    let example = refusal.example().expect("example");
    assert!(example.contains("worktree"), "{example}");
    assert!(example.contains("write_scope"), "{example}");
}
