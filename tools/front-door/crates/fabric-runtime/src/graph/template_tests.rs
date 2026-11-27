use super::template::*;
use super::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn library() -> TemplateLibrary {
    TemplateLibrary::bundled()
}

fn document(nodes: Vec<GraphNode>) -> GraphDocument {
    GraphDocument {
        queue_ci_wait: None,
        queue: None,
        review: None,
        review_policy: None,
        subject: None,
        exclude: Vec::new(),
        network: None,
        capability: Vec::new(),
        no_capability: Vec::new(),
        provenance: None,

        review_base: None,
        context: None,
        replay_digest: None,
        extra: Default::default(),
        limits: None,
        integration_branch: Some("batch/fixture".into()),
        delivery: None,
        decision_session: None,
        submission_patch: None,
        template_library: Default::default(),
        template_layers: Default::default(),
        template_resolvers: Default::default(),
        peer_origin_work_key: None,
        review_tier_override: None,
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

fn compiled_batch(raw: Value) -> GraphDocument {
    let library = library();
    let resolvers = BTreeMap::from([
        ("review-panel".into(), vec!["correctness".into()]),
        ("review-frame".into(), vec!["false".into()]),
        ("review-verify".into(), vec!["false".into()]),
        ("review-synthesis".into(), vec!["false".into()]),
        ("review-verifiers".into(), vec!["1".into()]),
        ("review-advice-correctness".into(), vec![String::new()]),
        (BATCH_MAX_PARALLEL_DEFAULT.into(), vec!["4".into()]),
    ]);
    let limits = super::plans::generous();
    let context = super::patch::PatchContext {
        catalogue: None,
        library: &library,
        resolvers: &resolvers,
        journal: None,
        limits: &limits,
        worktree_layout: provenant_config::PortTable::builtin().worktree_layout,
    };
    let patch = serde_json::from_value(json!({"schema_version": 1, "nodes": [raw]})).unwrap();
    super::patch::compile_submission("batch-test", &patch, &context, Some("batch/integration"))
        .unwrap()
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
        assert_eq!(library.layer(expected), Some(Layer::Default));
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
fn bundled_leaf_fanout_prompts_contain_only_their_item() {
    let library = library();
    let compiler = Compiler::new(&library);
    let items = ["unique-area-alpha", "unique-area-beta"];
    for name in library.names() {
        let template = library.resolve(name).unwrap();
        if template.is_composite() {
            continue;
        }
        for brief in [None, Some("Inspect the requested area")] {
            let mut node = json!({"id":"fanout", "use":name, "over":items});
            if let Some(brief) = brief {
                node["prompt"] = json!(brief);
            }
            let compiled = compiler
                .compile(&node)
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            for (index, child) in compiled.composite.unwrap().nodes.iter().enumerate() {
                assert!(child.prompt.contains(items[index]), "{name}");
                assert!(!child.prompt.contains(items[1 - index]), "{name}");
            }
        }
    }
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
    let prose = Compiler::new(&library)
        .compile(
            &json!({"id": "loose", "use": "task", "prompt": "Describe ${item} and ${unknown}"}),
        )
        .unwrap();
    assert!(prose.prompt.contains("${item}"));
    assert!(prose.prompt.contains("${unknown}"));
}

#[test]
fn review_panel_binds_per_lens_resolver_values() {
    let library = library();
    let resolvers = BTreeMap::from([
        ("review-frame".to_owned(), vec!["true".to_owned()]),
        ("review-verify".to_owned(), vec!["true".to_owned()]),
        ("review-synthesis".to_owned(), vec!["true".to_owned()]),
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
    let shape = panel.nodes[0].composite.as_ref().unwrap();
    let group = shape.nodes.iter().find(|node| node.id == "lenses").unwrap();
    let lenses = &group.composite.as_ref().unwrap().nodes;
    assert!(!lenses[0].prompt.contains("mutation guidance"));
    assert!(lenses[1].prompt.contains("mutation guidance"));
    assert_eq!(group.depends_on, ["frame"]);
    let verify = shape.nodes.iter().find(|node| node.id == "verify").unwrap();
    assert!(verify.after_settled.iter().any(|id| id == "lenses"));
    let synthesis = shape
        .nodes
        .iter()
        .find(|node| node.id == "synthesis")
        .unwrap();
    assert!(synthesis.after_settled.iter().any(|id| id == "verify"));
    for frame in [0, 1] {
        for verifier in [0, 1] {
            for synthesis in [0, 1] {
                let mut selected = resolvers.clone();
                selected.extend(when::review_stages(
                    frame == 1,
                    verifier == 1,
                    synthesis == 1,
                ));
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
                        expanded.nodes.iter().any(|node| {
                            node.id.ends_with(&format!("--{stage}"))
                                && node.when.iter().all(WhenTerm::holds)
                        }),
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
    let shape = panel.nodes[0].composite.as_ref().unwrap();
    let verifier = shape.nodes.iter().find(|node| node.id == "verify").unwrap();
    let verifiers = &verifier.composite.as_ref().unwrap().nodes;
    for id in &two["review-verifiers"] {
        assert!(
            verifiers.iter().any(
                |node| &node.id == id && node.review_behaviour == Some(ReviewBehaviour::Verify)
            )
        );
    }
    let mut single = resolvers.clone();
    single.extend(when::review_stages(false, false, false));
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
        .filter(|node| node.composite.is_none() && node.when.iter().all(WhenTerm::holds))
        .collect();
    assert_eq!(leaves.len(), 1);
    assert!(
        matches!(&leaves[0].source, Some(SourceBinding::Commit { commit }) if commit == "0123456789abcdef0123456789abcdef01234567")
    );
    let missing = BTreeMap::from([
        ("review-frame".to_owned(), vec!["false".to_owned()]),
        ("review-verify".to_owned(), vec!["false".to_owned()]),
        ("review-synthesis".to_owned(), vec!["false".to_owned()]),
        ("review-verifiers".to_owned(), vec!["1".to_owned()]),
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
            Layer::Project,
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
            Layer::Host,
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
            Layer::Host,
            "outer",
            r#"
nodes = [
  { id = "a", use = "inner", with = { area = "crates/run" } },
  { id = "b", use = "task", after = ["a"], prompt = "Act on the synthesis" },
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
            Layer::Launch,
            "loop-a",
            "nodes = [{ id = \"n\", use = \"loop-b\" }]",
        )
        .unwrap();
    library
        .insert_toml(
            Layer::Launch,
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

    let explicitly_bound = Compiler::new(&bundled)
        .compile(&json!({
            "id":"fix", "use":"repair",
            "repeat":{"until":"accepted", "max_rounds":2,
                "previous_bindings":{"synthesis":"initial-synthesis"},
                "retry_on":["accepted"], "selected_publication":"publication",
                "initial_publication":"initial-publish"},
            "with":{"worktree":"/fixture/writer", "write_scope":["src"],
                "lenses":["security"]}
        }))
        .unwrap();
    assert_eq!(
        explicitly_bound
            .loop_body
            .as_ref()
            .unwrap()
            .previous_bindings["synthesis"],
        "initial-synthesis"
    );
    let spec = explicitly_bound.loop_body.as_ref().unwrap();
    assert_eq!(spec.retry_on, ["accepted"]);
    assert_eq!(spec.selected_publication.as_deref(), Some("publication"));
    assert_eq!(spec.initial_publication.as_deref(), Some("initial-publish"));
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
    assert_eq!(node.template.as_deref(), Some("review.security"));
    assert!(library.resolve("review.missing").is_err());

    library
        .insert_toml(Layer::Host, "task", "brief = \"user\"\ntier = \"junior\"")
        .unwrap();
    library
        .insert_toml(
            Layer::Project,
            "task",
            "brief = \"repository\"\ntier = \"senior\"",
        )
        .unwrap();
    assert_eq!(library.layer("task"), Some(Layer::Project));
    let node = Compiler::new(&library)
        .compile(&json!({"id": "t", "use": "task", "tier": "principal"}))
        .unwrap();
    assert_eq!(node.prompt, "repository");
    assert_eq!(node.route.unwrap().alias.as_deref(), Some("principal"));
}

#[test]
fn unknown_template_flavour_names_the_template_and_known_flavours() {
    // Spec: docs/specs/cli.md#template-name-errors
    let library = library();
    let error = library.resolve("review.reliability").unwrap_err();
    let known = library
        .names()
        .filter(|name| name.starts_with("review."))
        .collect::<Vec<_>>();
    assert_eq!(
        error,
        format!(
            "unknown template review.reliability; known review flavours: {}",
            known.join(", ")
        )
    );
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

fn deliver_resolvers(panel: bool) -> BTreeMap<String, Vec<String>> {
    let mut resolvers = BTreeMap::from([
        ("review-frame".to_owned(), vec!["false".to_owned()]),
        ("review-verify".to_owned(), vec!["false".to_owned()]),
        ("review-synthesis".to_owned(), vec!["false".to_owned()]),
        ("review-verifiers".to_owned(), vec!["1".to_owned()]),
    ]);
    if panel {
        resolvers.insert(
            "review-panel".to_owned(),
            vec!["correctness".to_owned(), "tests".to_owned()],
        );
        resolvers.insert("review-advice-correctness".to_owned(), vec![String::new()]);
        resolvers.insert("review-advice-tests".to_owned(), vec![String::new()]);
    }
    resolvers
}

fn compiled_delivery(with: Value, panel: bool) -> GraphDocument {
    let library = library();
    let resolvers = deliver_resolvers(panel);
    let compiler = Compiler {
        library: &library,
        resolvers: &resolvers,
    };
    let mut raw = json!({"id":"delivery", "use":"deliver",
        "with":{"worktree":"/fixture/writer", "write_scope":["src"]}});
    for (key, value) in with.as_object().unwrap() {
        raw["with"][key] = value.clone();
    }
    let mut doc = document(vec![compiler.compile(&raw).unwrap()]);
    doc.schema_version = 2;
    doc
}

/// Expand a delivery graph without the execution admission gate, which stays
/// closed until the execution slices wire delivery policy.
fn expanded_delivery(
    document: &GraphDocument,
    limits: &crate::config::GraphLimits,
) -> GraphDocument {
    super::loops::expand(document, &[], limits).unwrap()
}

fn find_compiled_node<'a>(node: &'a GraphNode, id: &str) -> Option<&'a GraphNode> {
    if node.id == id {
        return Some(node);
    }
    node.composite
        .as_ref()
        .and_then(|spec| {
            spec.nodes
                .iter()
                .find_map(|child| find_compiled_node(child, id))
        })
        .or_else(|| {
            node.loop_body.as_ref().and_then(|spec| {
                spec.body
                    .iter()
                    .find_map(|child| find_compiled_node(child, id))
            })
        })
}

/// The default delivery shape is one writer and one single reviewer, plus the
/// ordered deterministic stages. Panel mode replaces that single reviewer with
/// one panel, not a second panel.
#[test]
fn delivery_composites_compile_both_review_shapes() {
    for (panel, shape, template) in [(false, "single", "review"), (true, "panel", "review-panel")] {
        let doc = compiled_delivery(
            json!({"review_shape": shape, "review_template": template, "plan": false}),
            panel,
        );
        let limits = limits();
        // Delivery policy stays un-admittable until the execution slices wire
        // it (AdmissionError::DeliveryQueueUnsupported); compilation and
        // expansion are the template contract this slice owns.
        let expanded = expanded_delivery(&doc, &limits);
        let order: Vec<_> = [
            "implement",
            "hygiene",
            "publish",
            "conflict",
            "ci",
            "review",
            "outcome",
            "land",
        ]
        .iter()
        .map(|suffix| format!("delivery--{suffix}"))
        .collect();
        for id in &order {
            let present = if id.ends_with("--review") && panel {
                expanded
                    .nodes
                    .iter()
                    .any(|node| node.id.starts_with("delivery--review--"))
            } else {
                expanded.nodes.iter().any(|node| &node.id == id)
            };
            assert!(present, "missing {id} for {shape}");
        }
        let review = find_compiled_node(&doc.nodes[0], "review").unwrap();
        assert_eq!(
            review.delivery_review.as_ref().unwrap().shape,
            serde_json::from_value::<provenant_records::graph::ReviewShape>(json!(shape)).unwrap()
        );
        if panel {
            assert!(review.composite.is_some());
            assert!(
                expanded
                    .nodes
                    .iter()
                    .any(|node| node.id.contains("delivery--review--selected--lenses--"))
            );
        } else {
            assert!(review.composite.is_none());
            assert_eq!(review.review_behaviour, Some(ReviewBehaviour::Lens));
            assert!(
                expanded
                    .nodes
                    .iter()
                    .any(|node| node.id == "delivery--review"
                        && node.review_behaviour == Some(ReviewBehaviour::Lens))
            );
        }
        // No pre-CI findings panel and no second post-CI panel.
        assert!(
            !expanded
                .nodes
                .iter()
                .any(|node| node.id.contains("initial-review"))
        );
    }
}

#[test]
fn delivery_review_waits_on_the_single_ci_outcome_boundary() {
    let doc = compiled_delivery(json!({"plan": false}), false);
    let expanded = expanded_delivery(&doc, &limits());
    let ci = expanded
        .nodes
        .iter()
        .find(|node| node.id == "delivery--ci")
        .unwrap();
    let spec = ci.composite.as_ref().unwrap();
    assert_eq!(spec.min_successful, Some(1));
    assert!(!spec.reduce_verdict);
    assert_eq!(spec.exports.len(), 2);
    for expected in ["delivery--ci--check", "delivery--ci--ci-repair"] {
        assert!(spec.exports.iter().any(|input| input.node == expected));
    }

    let check = expanded
        .nodes
        .iter()
        .find(|node| node.id == "delivery--ci--check")
        .unwrap();
    assert_eq!(check.on_fail.as_deref(), Some("delivery--ci--ci-repair"));
    let review = expanded
        .nodes
        .iter()
        .find(|node| node.id == "delivery--review")
        .unwrap();
    assert_eq!(review.depends_on.len(), 1);
    assert_eq!(review.depends_on[0], "delivery--ci");
}

/// Sourced settings resolve over the declared layers and seal the winning
/// value with its source; undeclared layers are ignored with a warning.
#[test]
fn delivery_queue_knobs_resolve_and_seal_with_sources() {
    use provenant_records::{Layer, Source, Sourced};
    let sourced = |value: usize, layer: Layer, origin: &str| Sourced {
        value,
        source: Source {
            layer,
            origin: Some(origin.into()),
        },
    };
    let shipped = QueueKnobs::default();
    let defaults = BTreeMap::from([
        (QUEUE_MAX_PARALLEL_KEY, shipped.max_parallel.value),
        (QUEUE_CI_SLOTS_KEY, shipped.ci_slots.value),
        (QUEUE_PREP_RETRIES_KEY, shipped.prep_retries.value),
    ]);
    let overrides = BTreeMap::from([(
        QUEUE_MAX_PARALLEL_KEY,
        vec![
            sourced(4, Layer::Default, "compiled"),
            sourced(7, Layer::Host, "~/.config/provenant/config.toml"),
            sourced(9, Layer::Project, ".provenant/config.toml"),
        ],
    )]);
    let sealed = seal_settings(&defaults, &overrides);
    let max_parallel = &sealed[QUEUE_MAX_PARALLEL_KEY];
    assert_eq!(max_parallel.value, 9);
    assert_eq!(max_parallel.source.layer, Layer::Project);
    assert_eq!(
        max_parallel.source.origin.as_deref(),
        Some(".provenant/config.toml")
    );
    // Absent keys keep their shipped value and source.
    assert_eq!(sealed[QUEUE_CI_SLOTS_KEY].value, 1);
    assert_eq!(sealed[QUEUE_CI_SLOTS_KEY].source.layer, Layer::Default);
    assert_eq!(sealed[QUEUE_PREP_RETRIES_KEY].value, 2);
    let launch = seal_settings(
        &defaults,
        &BTreeMap::from([(
            QUEUE_CI_SLOTS_KEY,
            vec![sourced(3, Layer::Launch, "--ci-slots")],
        )]),
    );
    assert_eq!(launch[QUEUE_CI_SLOTS_KEY].value, 3);
    assert_eq!(launch[QUEUE_CI_SLOTS_KEY].source.layer, Layer::Launch);
    assert_eq!(
        launch[QUEUE_CI_SLOTS_KEY].source.origin.as_deref(),
        Some("--ci-slots")
    );
    // Zero CI slots disables the cap; zero max_parallel is refused.
    let knobs = QueueKnobs {
        max_parallel: max_parallel.clone(),
        ci_slots: launch[QUEUE_CI_SLOTS_KEY].clone(),
        prep_retries: sealed[QUEUE_PREP_RETRIES_KEY].clone(),
    };
    assert!(knobs.validate().is_ok());
    let mut invalid = knobs.clone();
    invalid.ci_slots.value = 0;
    assert!(invalid.validate().is_ok());
    invalid.max_parallel.value = 0;
    assert!(invalid.validate().is_err());
}

/// Every shipped default is sealed, including settings added without overrides.
#[test]
fn seal_settings_includes_every_shipped_default() {
    use provenant_records::Layer;
    let defaults = BTreeMap::from([("first", 1), ("new_setting", 2)]);
    let sealed = seal_settings(&defaults, &BTreeMap::new());
    assert_eq!(sealed.len(), defaults.len());
    assert_eq!(sealed["first"].value, 1);
    assert_eq!(sealed["new_setting"].value, 2);
    assert_eq!(sealed["new_setting"].source.layer, Layer::Default);
}

/// Graph setting origins use the shared provenance layer vocabulary, not a
/// local setting-origin enum. The wire spelling is the layer's snake_case name.
/// Spec: docs/specs/agent-native.md#33-error-and-warning-contract
#[test]
fn graph_setting_sources_use_the_provenance_layer() {
    use provenant_records::Layer;
    let timeout: DecideTimeout =
        serde_json::from_value(json!({"seconds": 30, "source": "host"})).unwrap();
    assert_eq!(timeout.source, Layer::Host);
    assert_eq!(
        serde_json::to_value(timeout).unwrap(),
        json!({"seconds": 30, "source": "host"})
    );
    let policy: CiFailedChecksPolicy = serde_json::from_value(json!({
        "max_bytes": 150, "max_bytes_source": "template",
        "max_checks": 2, "max_checks_source": "default"
    }))
    .unwrap();
    assert_eq!(policy.max_bytes_source, Layer::Template);
    assert_eq!(policy.max_checks_source, Layer::Default);
    assert_eq!(
        serde_json::to_value(Layer::Detected).unwrap(),
        json!("detected")
    );
}

/// The optional scope is a read-only planner whose result joins the writer
/// brief only when `plan` is true.
#[test]
fn delivery_optional_scope_propagates_into_the_writer() {
    let limits = limits();
    let planned = compiled_delivery(json!({"plan": true}), false);
    let expanded = expanded_delivery(&planned, &limits);
    let writer = expanded
        .nodes
        .iter()
        .find(|node| {
            node.access == AccessMode::WorktreeWrite
                && node.delivery == Some(provenant_records::run::DeliveryMode::Pr)
                && node.continues.is_none()
        })
        .unwrap();
    assert!(
        writer
            .inputs
            .iter()
            .any(|input| input.node.ends_with("--plan"))
    );
    let unplanned = compiled_delivery(json!({"plan": false}), false);
    let expanded = expanded_delivery(&unplanned, &limits);
    assert!(!expanded.nodes.iter().any(|node| node.id.contains("--plan")));
}

/// The final-rung brief widens scope within authority. Repair briefs carry the
/// failing head, a bounded excerpt and the sweep-the-class instruction.
#[test]
fn delivery_policy_briefs_carry_rung_and_repair_inputs() {
    let doc = compiled_delivery(
        json!({"plan": false, "failing_head": "deadbeef", "failure_excerpt": "noise\nfatal: real failure"}),
        false,
    );
    let limits = limits();
    let expanded = expanded_delivery(&doc, &limits);
    let writer = expanded
        .nodes
        .iter()
        .find(|node| node.id == "delivery--implement")
        .unwrap();
    assert!(writer.prompt.contains(FINAL_RUNG_SCOPE_BRIEF));
    let fix = expanded
        .nodes
        .iter()
        .find(|node| {
            node.id.contains("ci-repair") && node.prompt.contains(REPAIR_SWEEP_INSTRUCTION)
        })
        .unwrap();
    assert!(fix.prompt.contains("deadbeef"));
    assert!(fix.prompt.contains("fatal: real failure"));
    let widened = widen_final_rung_brief("base");
    assert_eq!(widened, format!("base\n\n{FINAL_RUNG_SCOPE_BRIEF}"));
    let repair = repair_brief("head", "line one\nline two");
    assert!(repair.contains(REPAIR_SWEEP_INSTRUCTION));
    let long = repair_brief("head", &"x".repeat(9000));
    assert!(long.chars().count() < 5000);
    assert!(long.contains('…'));
}

/// A sealed old plan has no delivery fields and replays byte-for-byte: the new
/// contract fields stay absent, so a restart cannot silently add policy.
#[test]
fn sealed_old_delivery_plan_replays_unchanged() {
    let sealed = json!({
        "schema_version": 1,
        "graph_id": "legacy",
        "nodes": [{
            "id": "task", "access": "read_only", "prompt": "work",
            "route": {"alias": "senior"}, "depends_on": []
        }]
    });
    let decoded: GraphDocument = serde_json::from_value(sealed.clone()).unwrap();
    let wire = serde_json::to_value(&decoded).unwrap();
    assert_eq!(wire, sealed);
    assert!(wire.get("queue").is_none());
    assert!(wire["nodes"][0].get("escalate").is_none());
    assert!(wire["nodes"][0].get("delivery_review").is_none());
    assert!(wire["nodes"][0].get("loop").is_none());
}

/// The old bounded `batch` repair shape keeps working: the slice only replaces
/// the `deliver` default, not unrelated composites.
#[test]
fn batch_repair_shape_is_untouched_by_the_deliver_change() {
    let raw = json!({"id":"batch", "use":"batch",
        "with":{"worktree":"/fixture/writer", "write_scope":["src"], "max_repair_cycles":2}});
    let doc = compiled_batch(raw);
    let limits = crate::config::GraphLimits::default();
    doc.admit(&limits).unwrap();
    let expanded = expanded_delivery(&doc, &limits);
    let repair = expanded
        .nodes
        .iter()
        .find(|node| node.id.ends_with("repair--cycles"))
        .unwrap();
    assert_eq!(repair.loop_body.as_ref().unwrap().max_rounds, 2);
}

#[test]
fn review_cycle_limit_is_configurable_with_no_upper_ceiling() {
    let bundled = library();
    let error = Compiler::new(&bundled)
        .compile(&json!({"id":"repair", "use":"review-repair",
        "with":{"worktree":"/fixture/writer", "write_scope":["src"], "max_repair_cycles":0}}))
        .unwrap_err()
        .to_string();
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
                "id":"review", "use":name, "prompt":"Inspect this", "require_findings":false,
                "required_verdict":"anything", "role":"task"
            }))
            .unwrap();
        assert!(node.role.is_none());
        assert_eq!(
            node.review_behaviour
                .map(|value| serde_json::to_value(value).unwrap()),
            Some(serde_json::Value::from(behaviour))
        );
        assert!(node.require_findings);
        assert_eq!(node.required_verdict.as_deref(), Some("VERDICT: PASS"));
        if name != "review.tests" {
            assert!(node.review_lens.is_none());
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
        on_stop = { stopped_early = "chair" }
    "#,
    )
    .unwrap();
    templates = templates
        .with(
            Layer::Project,
            &BTreeMap::from([("custom-writer".into(), custom)]),
        )
        .unwrap();
    let compiled = Compiler::new(&templates)
        .compile(&json!({"id":"work", "use":"custom-writer", "worktree":".worktrees/work"}))
        .unwrap();
    assert_eq!(
        compiled
            .on_stop
            .get(provenant_records::StopCause::StoppedEarly),
        Some(provenant_records::NextStep::Chair)
    );
    assert!(
        Compiler::new(&templates)
            .compile(
                &json!({"id":"work", "use":"custom-writer", "on_stop":{"stopped_early":"later"}})
            )
            .is_err()
    );
}

#[test]
fn template_node_fields_are_checked_and_extensions_are_explicit() {
    let mut library = library();
    library
        .insert_toml(Layer::Launch, "typo", "[node]\nmisspelt_field = true")
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
            Layer::Launch,
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
            .insert(Layer::Launch, &format!("copy-{name}"), copy)
            .unwrap();
    }
    let resolvers = BTreeMap::from([
        ("review-frame".into(), vec!["false".into()]),
        ("review-verify".into(), vec!["false".into()]),
        ("review-synthesis".into(), vec!["false".into()]),
        ("review-verifiers".into(), vec!["1".into()]),
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
// Spec: docs/specs/fabric.md#fix-round-invariants-and-split-verdict-adjudication
fn split_verdict_yields_one_adjudication_node_and_no_new_round() {
    let panels = ["panel".to_owned()];
    let patch = super::repair::split_verdict_patch("item", "panel", &panels, None, 2, "principal");
    assert_eq!(
        patch.nodes.len(),
        1,
        "a split adds one adjudication node, never a second panel or round"
    );
    let compiled = Compiler::new(&library()).compile(&patch.nodes[0]).unwrap();
    assert_eq!(
        compiled.kind,
        Some(NodeKind::Adjudicate),
        "identify the adjudication node by kind"
    );
    assert_eq!(compiled.access, AccessMode::ReadOnly);
    assert_eq!(compiled.depends_on, ["panel"]);
    assert_eq!(compiled.loop_body, None, "no round is added");
    assert_eq!(
        compiled.required_verdict, None,
        "an upheld finding must be able to return VERDICT: FAIL for repair"
    );
    assert_eq!(
        patch.idempotency_key,
        super::repair::split_verdict_patch("item", "another-panel", &panels, None, 3, "principal",)
            .idempotency_key,
        "split detection is idempotent per item and head"
    );
}

#[test]
fn every_bundled_template_compiles_with_required_bindings() {
    let library = library();
    let resolvers = BTreeMap::from([
        ("review-frame".into(), vec!["true".into()]),
        ("review-verify".into(), vec!["true".into()]),
        ("review-synthesis".into(), vec!["true".into()]),
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
                    other => panic!("no required binding fixture for {name}.{key}: {other:?}"),
                };
                bindings.insert(key.clone(), value);
            }
        }
        // Every bundled asset must be usable through the public compiler. Prompt
        // text and expansion shape may evolve; fan-out item isolation and other
        // behavioural contracts are covered by the focused invariant tests.
        compiler
            .compile(&json!({"id":"stage", "use":name, "with":bindings}))
            .unwrap_or_else(|error| panic!("{name}: {error}"));
    }
}

#[test]
// Spec: docs/specs/fabric.md#bundled-template-declarations — the bundled smoke
// template declares six nodes: read, write, two reviews and two route probes.
fn smoke_template_compiles_the_read_review_write_retry_and_steer_nodes() {
    let library = library();
    let node = Compiler::new(&library)
        .compile(&json!({"id":"smoke", "use":"smoke"}))
        .unwrap();
    let composite = node.composite.unwrap();
    // With no writer the fixture review, which compares the fixture change
    // against its base, is the graph's result.
    assert_eq!(composite.exports.len(), 1);
    assert_eq!(composite.exports[0].node, "review-fixture");
    let children = composite.nodes;
    let by_id = |id: &str| children.iter().find(|node| node.id == id).unwrap();
    // The writer runs before review so the review's node source is the smoke
    // writer's scratch commit, never the caller checkout's commit.
    assert_eq!(
        children
            .iter()
            .map(|node| node.id.as_str())
            .collect::<Vec<_>>(),
        [
            "read",
            "write",
            "review-write",
            "review-fixture",
            "retry-probe",
            "steer-probe"
        ]
    );
    assert_eq!(by_id("read").access, AccessMode::ReadOnly);
    assert_eq!(by_id("write").access, AccessMode::WorktreeWrite);
    assert_eq!(by_id("review-write").access, AccessMode::ReadOnly);
    assert_eq!(by_id("review-fixture").access, AccessMode::ReadOnly);
    assert_eq!(by_id("retry-probe").access, AccessMode::ReadOnly);
    assert_eq!(by_id("steer-probe").access, AccessMode::ReadOnly);

    let node = Compiler::new(&library)
        .compile(&json!({
            "id":"smoke", "use":"smoke",
            "with":{"worktree":"/fixture/writer", "write":true}
        }))
        .unwrap();
    assert_eq!(node.composite.unwrap().exports[0].node, "review-write");
}

#[test]
// Spec: docs/specs/fabric.md#bundled-template-declarations — every smoke node
// routes on a tier that resolves to at least one free model, so verification
// never depends on a paid account or an unavailable route.
fn smoke_nodes_resolve_on_a_free_capable_tier() {
    let library = library();
    let compiled = Compiler::new(&library)
        .compile(&json!({"id":"smoke", "use":"smoke"}))
        .unwrap();
    let catalogue = provenant_routing::Catalogue::bundled().unwrap();
    let config = provenant_routing::RoutingConfig::default();
    let context = provenant_routing::RoutingContext {
        policy_account_hint: None,
        selection_evidence: None,
        choice_context: None,
        catalogue: &catalogue,
        config: &config,
        registry: provenant_providers::REGISTRY,
        availability: &[],
        quota: &[],
        capacity_cooldowns: &[],
        accounts: &[],
        quota_policies: &[],
        readiness: None,
        access: &[],
        now: provenant_core::WallTime::from_unix_seconds(100).unwrap(),
        host: "smoke-route-test",
    };
    // A fan-out node (the review panel) is a boundary: its tier lives on the
    // member leaves it expands to, so walk down to every leaf that runs.
    fn leaves(nodes: Vec<GraphNode>) -> Vec<GraphNode> {
        nodes
            .into_iter()
            .flat_map(|node| match node.composite.clone() {
                Some(composite) => leaves(composite.nodes),
                None => vec![node],
            })
            .collect()
    }
    let leaves = leaves(compiled.composite.unwrap().nodes);
    assert!(
        leaves.iter().any(|node| node.review_lens.is_some()),
        "the review panel's lens leaves are part of the smoke graph"
    );
    for node in leaves {
        // A template leaf's `tier` lowers to the node's route alias.
        let tier = node
            .route
            .as_ref()
            .and_then(|route| route.alias.as_deref())
            .unwrap_or_else(|| panic!("smoke node {} declares no tier", node.id));
        let listed = provenant_routing::list(
            &provenant_routing::RouteListQuery {
                valid: false,
                adapter: None,
                pool: Some(tier.to_owned()),
            },
            &context,
        )
        .unwrap_or_else(|error| panic!("smoke node {} tier {tier}: {error}", node.id));
        let members = listed
            .pools
            .get(tier)
            .unwrap_or_else(|| panic!("smoke node {} tier {tier} is not a pool", node.id));
        assert!(
            members.iter().any(|member| member.free),
            "smoke node {} on tier {tier} has no free route: {members:?}",
            node.id
        );
    }
}

#[test]
// Spec: docs/specs/fabric.md#sources-and-read-roots — the write-review is pinned
// to the writer's commit, not to the base the fixture change started from.
fn smoke_review_covers_only_the_writer_commit() -> Result<(), String> {
    let library = library();
    let diff_base = "1111111111111111111111111111111111111111";
    let fixture_base = "2222222222222222222222222222222222222222";
    let compiled = Compiler::new(&library)
        .compile(&json!({
            "id":"smoke", "use":"smoke",
            "with":{
                "diff_base":diff_base,
                "fixture_base":fixture_base,
                "write":true
            }
        }))
        .map_err(|error| error.to_string())?;
    let composite = compiled.composite.unwrap();
    let review = composite
        .nodes
        .iter()
        .find(|node| node.id == "review-write")
        .unwrap();
    assert_eq!(
        review.source,
        Some(SourceBinding::Node {
            node: "write".into()
        })
    );
    assert!(
        review.depends_on.iter().any(|id| id == "write"),
        "the write-review must depend on the writer: {:?}",
        review.depends_on
    );
    let lenses = &review.composite.as_ref().unwrap().nodes;
    assert_eq!(lenses.len(), 2);
    assert!(
        lenses
            .iter()
            .all(|lens| lens.review_base.as_deref() == Some(diff_base)),
        "a write-review lens was not pinned to diff_base: {:?}",
        lenses
            .iter()
            .map(|lens| lens.review_base.clone())
            .collect::<Vec<_>>()
    );
    Ok(())
}

#[test]
// Spec: docs/specs/fabric.md#sources-and-read-roots — smoke carries no staged
// fixture. The read node is plain prose, and each review takes its range from
// the launch-supplied base for its own subject.
fn smoke_template_reads_no_staged_fixture() -> Result<(), String> {
    let library = library();
    let diff_base = "1111111111111111111111111111111111111111";
    let fixture_base = "2222222222222222222222222222222222222222";

    fn collect<'a>(nodes: &'a [GraphNode], out: &mut Vec<&'a GraphNode>) {
        for node in nodes {
            out.push(node);
            if let Some(composite) = &node.composite {
                collect(&composite.nodes, out);
            }
            if let Some(loop_body) = &node.loop_body {
                collect(&loop_body.body, out);
            }
        }
    }
    fn bases(node: &GraphNode, out: &mut Vec<String>) {
        if let Some(base) = &node.review_base {
            out.push(base.clone());
        }
        if let Some(composite) = &node.composite {
            for child in &composite.nodes {
                bases(child, out);
            }
        }
    }

    for smoke_write in [true, false] {
        let compiled = Compiler::new(&library)
            .compile(&json!({
                "id":"smoke", "use":"smoke",
                "with":{
                    "diff_base":diff_base,
                    "fixture_base":fixture_base,
                    "write":smoke_write
                }
            }))
            .map_err(|error| error.to_string())?;
        let composite = compiled.composite.unwrap();
        let mut all = Vec::new();
        collect(&composite.nodes, &mut all);
        assert!(
            all.iter()
                .all(|node| node.fixture.is_none() && node.fixture_bytes.is_none()),
            "smoke staged a fixture with write={smoke_write}"
        );
        let by_id = |id: &str| all.iter().find(|node| node.id == id).unwrap();
        let read = by_id("read");
        assert_eq!(read.access, AccessMode::ReadOnly);
        assert!(read.fixture.is_none(), "the read node stages a fixture");

        for (id, expected) in [
            ("review-write", diff_base),
            ("review-fixture", fixture_base),
        ] {
            let mut found = Vec::new();
            bases(by_id(id), &mut found);
            assert!(!found.is_empty(), "{id} carries no review_base");
            assert!(
                found.iter().all(|base| base == expected),
                "{id} carried a wrong review_base with write={smoke_write}: {found:?}"
            );
        }
    }
    Ok(())
}

#[test]
fn malformed_review_repair_behaviour_reports_an_error() {
    let mut library = library();
    library
        .insert_toml(
            Layer::Launch,
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
                Layer::Project,
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
            Layer::Project,
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
    assert_eq!(node.review_lens.as_deref(), Some("local"));
    library
        .insert_toml(
            Layer::Project,
            "review.local.explicit",
            r#"
[node]
review_lens = "explicit"
"#,
        )
        .unwrap();
    let node = Compiler::new(&library)
        .compile(&json!({"id":"stage", "use":"review.local.explicit"}))
        .unwrap();
    assert_eq!(node.extra["custom"], true);
    assert_eq!(node.review_lens.as_deref(), Some("explicit"));
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
        library.insert(Layer::Project, "local", template).unwrap();
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
                Layer::Launch,
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
            Layer::Launch,
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
            Layer::Launch,
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
            Layer::Launch,
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
            Layer::Launch,
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
        .compile(&serde_json::json!({"id": "d", "use": "deliver"}))
        .unwrap_err();
    let CompileError::Parameter(refusal) = error else {
        panic!("expected a parameter refusal: {error}");
    };
    let text = refusal.to_string();
    assert!(text.contains("worktree"), "{text}");
    assert!(text.contains("example:"), "{text}");
    let example = refusal.example().expect("example");
    assert!(example.contains("worktree"), "{example}");
}

#[test]
fn set_values_parse_by_declared_kind_and_a_wrong_kind_names_the_declaration() {
    let library = library();
    let mut checked: Vec<ParameterKind> = Vec::new();
    let names: Vec<_> = library.names().map(str::to_owned).collect();
    for name in &names {
        let template = library.resolve(name).unwrap();
        for (key, parameter) in &template.parameters {
            let (valid, wrong): (&str, &str) = match parameter.kind {
                ParameterKind::String => ("a plain word", ""),
                ParameterKind::Integer => ("7", "true"),
                ParameterKind::Number => ("1.5", "true"),
                ParameterKind::Boolean => ("true", "7"),
                ParameterKind::List => ("[1, 2]", "7"),
                ParameterKind::Table => ("{ a = 1 }", "7"),
            };
            if !checked.contains(&parameter.kind) {
                checked.push(parameter.kind);
            }
            let bound = template
                .type_binding(name, key, json!(valid))
                .unwrap_or_else(|_| {
                    panic!("{name}.{key}: valid {} text refused", parameter.kind.name())
                });
            let fits = match parameter.kind {
                ParameterKind::String => bound.is_string(),
                ParameterKind::Integer => bound.is_i64(),
                ParameterKind::Number => bound.is_number(),
                ParameterKind::Boolean => bound.is_boolean(),
                ParameterKind::List => bound.is_array(),
                ParameterKind::Table => bound.is_object(),
            };
            assert!(fits, "{name}.{key}: {bound}");
            if parameter.kind == ParameterKind::String {
                assert_eq!(bound, json!(valid), "a string is taken as written");
                continue;
            }
            let refusal = template
                .type_binding(name, key, json!(wrong))
                .expect_err("a value of another kind must be refused");
            match refusal.fault {
                ParameterFault::Shape {
                    key: named,
                    expected,
                } => {
                    assert_eq!(&named, key);
                    assert!(expected.contains(parameter.kind.name()), "{expected}");
                }
                other => panic!("{name}.{key}: {other:?}"),
            }
        }
    }
    assert!(checked.contains(&ParameterKind::String), "{checked:?}");
}

#[test]
fn undeclared_set_keys_and_typed_values_pass_through_to_the_compiler() {
    let library = library();
    let template = library.resolve("task").unwrap();
    assert_eq!(
        template
            .type_binding("task", "not-declared", json!("x"))
            .unwrap(),
        json!("x")
    );
    assert_eq!(
        template.type_binding("task", "brief", json!(7)).unwrap(),
        json!(7)
    );
}

#[test]
fn graph_names_are_valid_ids_within_the_bound() {
    for ok in [
        "a",
        "review",
        "fix-2",
        "a1-b2",
        &"a".repeat(MAX_GRAPH_NAME_BYTES),
    ] {
        assert!(fold_graph_name(ok).is_ok(), "{ok}");
    }
    // Capitals fold to lower case instead of being refused.
    assert_eq!(fold_graph_name("Fix-2").unwrap(), "fix-2");
    for bad in [
        "",
        "1a",
        "-a",
        "a-",
        "a--b",
        "a_b",
        "a.b",
        &"a".repeat(MAX_GRAPH_NAME_BYTES + 1),
    ] {
        assert!(fold_graph_name(bad).is_err(), "{bad}");
    }
    let names: Vec<_> = library().names().map(str::to_owned).collect();
    for template in names {
        let base = default_graph_base(&template);
        assert!(fold_graph_name(&format!("{base}-1")).is_ok(), "{template}");
    }
}

/// Replays a series of launches against an in-memory journal of ids and digests.
fn launch(
    journal: &mut Vec<(String, String)>,
    base: &str,
    digest: &str,
    named: bool,
) -> Result<(String, bool), String> {
    let existing: Vec<String> = journal.iter().map(|(id, _)| id.clone()).collect();
    let stored = journal.clone();
    match decide_graph_name(base, digest, named, &existing, |id| {
        stored
            .iter()
            .find(|(known, _)| known == id)
            .map(|(_, digest)| digest.clone())
    })? {
        NameDecision::Replay(id) => Ok((id, true)),
        NameDecision::Create { id, .. } => {
            journal.push((id.clone(), digest.to_owned()));
            Ok((id, false))
        }
    }
}

#[test]
fn a_name_replays_its_digest_and_takes_the_next_suffix_for_a_new_one() {
    let mut journal = Vec::new();
    assert_eq!(
        launch(&mut journal, "fix", "d1", true).unwrap(),
        ("fix".into(), false)
    );
    // The same input returns the same graph and starts nothing.
    assert_eq!(
        launch(&mut journal, "fix", "d1", true).unwrap(),
        ("fix".into(), true)
    );
    assert_eq!(journal.len(), 1);
    // Changed input takes the next free suffix and leaves the first alone.
    assert_eq!(
        launch(&mut journal, "fix", "d2", true).unwrap(),
        ("fix-2".into(), false)
    );
    assert_eq!(journal[0], ("fix".into(), "d1".into()));
    // Repeating either input finds its own graph.
    assert_eq!(
        launch(&mut journal, "fix", "d2", true).unwrap(),
        ("fix-2".into(), true)
    );
    assert_eq!(
        launch(&mut journal, "fix", "d1", true).unwrap(),
        ("fix".into(), true)
    );
    assert_eq!(
        launch(&mut journal, "fix", "d3", true).unwrap(),
        ("fix-3".into(), false)
    );
    assert_eq!(journal.len(), 3);
    // A graph whose name only looks like a family member is not one.
    let mut other = vec![("fix-it".to_owned(), "d1".to_owned())];
    assert_eq!(
        launch(&mut other, "fix", "d1", true).unwrap(),
        ("fix".into(), false)
    );
}

#[test]
fn an_unnamed_launch_takes_a_unique_valid_handle_and_never_replays() {
    let mut journal = Vec::new();
    let base = default_graph_base("review.adversarial");
    assert_eq!(base, "review-adversarial");
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..5 {
        let (id, replayed) = launch(&mut journal, &base, "same", false).unwrap();
        assert!(!replayed);
        assert!(fold_graph_name(&id).is_ok(), "{id}");
        assert!(seen.insert(id));
    }
    assert!(seen.contains("review-adversarial-1"));
}

#[test]
fn a_name_without_room_for_its_suffix_is_refused_in_one_line() {
    let base = "a".repeat(MAX_GRAPH_NAME_BYTES);
    let mut journal = Vec::new();
    assert_eq!(launch(&mut journal, &base, "d1", true).unwrap().0, base);
    let error = launch(&mut journal, &base, "d2", true).unwrap_err();
    assert!(!error.contains('\n') && error.contains("--name"), "{error}");
    assert_eq!(journal.len(), 1);
}

#[test]
fn graph_node_accepts_prompt_and_refuses_removed_brief_spelling() {
    let prompt = serde_json::from_value::<GraphNode>(json!({
        "id":"work", "access":"read_only", "prompt":"Do the work"
    }))
    .unwrap();
    assert_eq!(prompt.prompt, "Do the work");

    let alias = serde_json::from_value::<GraphNode>(json!({
        "id":"work", "access":"read_only", "brief":"Do the work"
    }));
    assert!(alias.is_err());
}

#[test]
fn batch_items_accept_structured_tables_and_bare_strings() {
    let directory = tempfile::tempdir().unwrap();
    let brief_file = directory.path().join("brief.txt");
    std::fs::write(&brief_file, "file brief").unwrap();
    let raw = json!({
        "id": "batch-run",
        "use": "batch",
        "with": {
            "worktree": "/fixture/writer",
            "items": [
                "bare-slice",
                {
                    "id": "table-slice",
                    "brief": "custom table brief",
                    "write_scope": ["src/custom.rs"]
                },
                {"id": "file-slice", "brief_file": brief_file}
            ]
        }
    });
    let doc = compiled_batch(raw);
    let limits = crate::config::GraphLimits::default();
    doc.admit(&limits).unwrap();
    let expanded = expanded_delivery(&doc, &limits);

    let bare = expanded
        .nodes
        .iter()
        .find(|n| n.id.contains("bare-slice"))
        .unwrap();
    assert!(bare.prompt.contains("Implement slice bare-slice"));

    let table = expanded
        .nodes
        .iter()
        .find(|n| n.id.contains("table-slice"))
        .unwrap();
    assert!(table.prompt.contains("custom table brief"));
    assert_eq!(table.write_scope, vec!["src/custom.rs"]);
    assert_ne!(bare.worktree, table.worktree);
    assert!(bare.provision_worktree && table.provision_worktree);
    assert!(
        expanded
            .nodes
            .iter()
            .any(|node| node.id.contains("file-slice") && node.prompt.contains("file brief"))
    );
}

#[test]
fn batch_per_item_routing_route_vs_model() {
    let raw = json!({
        "id": "batch-run",
        "use": "batch",
        "with": {
            "worktree": "/fixture/writer",
            "items": [
                {
                    "id": "slice-route",
                    "route": "senior"
                },
                {
                    "id": "slice-model",
                    "model": "anthropic/claude-3-5-sonnet"
                }
            ]
        }
    });
    let doc = compiled_batch(raw);
    let limits = crate::config::GraphLimits::default();
    doc.admit(&limits).unwrap();
    let expanded = expanded_delivery(&doc, &limits);

    let route_node = expanded
        .nodes
        .iter()
        .find(|n| n.id.contains("slice-route"))
        .unwrap();
    assert_eq!(
        route_node.route.as_ref().unwrap().alias.as_deref(),
        Some("senior")
    );
    assert_eq!(route_node.route.as_ref().unwrap().tier, None);

    let model_node = expanded
        .nodes
        .iter()
        .find(|n| n.id.contains("slice-model"))
        .unwrap();
    assert_eq!(
        model_node.route.as_ref().unwrap().model.as_deref(),
        Some("anthropic/claude-3-5-sonnet")
    );
    assert_eq!(
        model_node.route.as_ref().unwrap().adapter.as_deref(),
        Some("anthropic")
    );
    assert_eq!(model_node.route.as_ref().unwrap().tier, None);
}

#[test]
fn batch_items_mutual_exclusion_validation() {
    let bundled = library();
    let resolvers = std::collections::BTreeMap::new();
    let compiler = Compiler {
        library: &bundled,
        resolvers: &resolvers,
    };

    let raw_brief_conflict = json!({
        "id": "batch-run",
        "use": "batch",
        "with": {
            "worktree": "/fixture/writer",
            "items": [
                {
                    "id": "slice-1",
                    "brief": "do something",
                    "brief_file": "brief.txt"
                }
            ]
        }
    });
    let err = compiler
        .compile(&raw_brief_conflict)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("cannot specify both `brief` and `brief_file`"),
        "{err}"
    );

    let raw_route_conflict = json!({
        "id": "batch-run",
        "use": "batch",
        "with": {
            "worktree": "/fixture/writer",
            "items": [
                {
                    "id": "slice-2",
                    "route": "senior",
                    "model": "openai/gpt-4o"
                }
            ]
        }
    });
    let err2 = compiler
        .compile(&raw_route_conflict)
        .unwrap_err()
        .to_string();
    assert!(
        err2.contains("cannot specify both `route` and `model`"),
        "{err2}"
    );
}

#[test]
fn batch_items_after_ordering_and_cycle_detection() {
    let bundled = library();
    let resolvers = std::collections::BTreeMap::new();
    let compiler = Compiler {
        library: &bundled,
        resolvers: &resolvers,
    };

    let named = json!({
        "id": "batch-run", "use": "batch", "with": {
            "worktree": "/fixture/writer",
            "items": [{"id": "slice A"}, {"id": "next", "after": ["slice A"]}]
        }
    });
    let named_doc = compiled_batch(named);
    let expanded = named_doc
        .expanded(&crate::config::GraphLimits::default())
        .unwrap();
    let next = expanded
        .nodes
        .iter()
        .find(|node| node.id.ends_with("--next"))
        .unwrap();
    assert!(next.depends_on.iter().any(|id| id.ends_with("--slice-A")));
    assert!(matches!(
        next.source.as_ref(),
        Some(SourceBinding::Integration)
    ));

    let unknown_after = json!({
        "id": "batch-run",
        "use": "batch",
        "with": {
            "worktree": "/fixture/writer",
            "items": [
                {
                    "id": "slice-a",
                    "after": ["unknown-slice"]
                }
            ]
        }
    });
    let err = compiler.compile(&unknown_after).unwrap_err().to_string();
    assert!(err.contains("unknown-slice"), "{err}");

    let cycle = json!({
        "id": "batch-run",
        "use": "batch",
        "with": {
            "worktree": "/fixture/writer",
            "items": [
                {
                    "id": "slice-a",
                    "after": ["slice-b"]
                },
                {
                    "id": "slice-b",
                    "after": ["slice-a"]
                }
            ]
        }
    });
    let err_cycle = compiler.compile(&cycle).unwrap_err().to_string();
    assert!(err_cycle.contains("cycle"), "{err_cycle}");
    assert!(
        err_cycle.contains("slice-a") && err_cycle.contains("slice-b"),
        "{err_cycle}"
    );
}

/// Spec: docs/specs/state.md#every-setting-reports-its-layer
#[test]
fn template_precedence_ranks_each_declared_layer_once() {
    assert_eq!(
        super::template::TEMPLATE_PRECEDENCE.0,
        &[Layer::Default, Layer::Host, Layer::Project, Layer::Launch]
    );
}

#[test]
fn delivery_contracts_round_trip_and_admit_execution() {
    let raw = json!({"id":"repair", "kind":"loop", "access":"read_only",
        "loop":{"body":[
            {"id":"writer", "access":"worktree_write", "worktree":"/fixture/writer",
                "prompt":"repair", "route":{"alias":"junior"}},
            {"id":"done", "kind":"gate", "access":"read_only",
                "depends_on":["writer"], "condition":{"type":"budget"}}
        ], "until":"done", "max_rounds":2},
        "escalate":{"ladder":["junior","principal"],"max_ci_rounds":3},
        "delivery_review":{"shape":"panel","accept_below":"off"}});
    let compiled = Compiler::new(&library()).compile(&raw).unwrap();
    let wire = serde_json::to_value(&compiled).unwrap();
    assert_eq!(wire["escalate"], raw["escalate"]);
    assert_eq!(wire["delivery_review"], raw["delivery_review"]);
    let decoded: GraphNode = serde_json::from_value(wire).unwrap();
    assert_eq!(decoded, compiled);
    let mut graph = document(vec![decoded]);
    graph.schema_version = 2;
    graph.queue = Some(QueueKnobs::default());
    let wire = serde_json::to_value(&graph).unwrap();
    assert_eq!(wire["queue"]["ci_slots"]["value"], 1);
    assert_eq!(wire["queue"]["ci_slots"]["layer"], "default");
    assert_eq!(
        serde_json::from_value::<GraphDocument>(wire).unwrap(),
        graph
    );
    assert!(graph.admit(&limits()).is_ok());
}

#[test]
fn compiler_lowers_and_admits_a_queue_delivery_composite() {
    let composite = Compiler::new(&library())
        .compile(&serde_json::from_str::<serde_json::Value>(r#"{
            "id": "delivery",
            "kind": "composite",
            "access": "read_only",
            "composite": {
                "nodes": [
                    {"id":"writer", "access":"worktree_write", "worktree":"/fixture/writer",
                        "prompt":"write the change", "route":{"alias":"senior"}},
                    {"id":"hygiene", "kind":"hygiene", "access":"worktree_write",
                        "worktree":"/fixture/writer", "depends_on":["writer"],
                        "source":{"type":"node", "node":"writer"}},
                    {"id":"publish", "kind":"publish", "access":"worktree_write",
                        "worktree":"/fixture/writer", "depends_on":["hygiene"],
                        "source":{"type":"node", "node":"hygiene"}},
                    {"id":"conflict", "kind":"gate", "access":"read_only",
                        "depends_on":["publish"], "on_fail":"rebase",
                        "condition":{"type":"conflict", "target":{"node":"publish"}}},
                    {"id":"rebase", "kind":"loop", "access":"read_only",
                        "depends_on":["conflict", "publish", "hygiene"],
                        "loop":{"max_rounds":2, "until":"accepted",
                            "retry_on":["verified_clean", "fresh_conflict", "fresh_ci", "accepted"],
                            "selected_publication":"refresh", "initial_publication":"publish",
                            "previous_bindings":{"refresh":"publish", "verified_clean":"hygiene"},
                            "body":[
                                {"id":"fix", "access":"worktree_write", "worktree":"/fixture/writer",
                                    "prompt":"merge the base into the retained branch", "route":{"alias":"senior"},
                                    "source":{"type":"node", "node":"previous.verified_clean"},
                                    "inputs":[{"node":"previous.refresh", "outputs":["branch", "commit"]}]},
                                {"id":"verified_clean", "kind":"hygiene", "access":"worktree_write",
                                    "worktree":"/fixture/writer", "depends_on":["fix"],
                                    "source":{"type":"node", "node":"fix"}},
                                {"id":"refresh", "kind":"publish", "access":"worktree_write",
                                    "worktree":"/fixture/writer", "depends_on":["verified_clean"],
                                    "source":{"type":"node", "node":"verified_clean"}},
                                {"id":"fresh_conflict", "kind":"gate", "access":"read_only",
                                    "depends_on":["refresh"],
                                    "condition":{"type":"conflict", "target":{"node":"refresh"}}},
                                {"id":"fresh_ci", "kind":"gate", "access":"read_only",
                                    "depends_on":["fresh_conflict"],
                                    "condition":{"type":"ci", "target":{"node":"refresh"}}},
                                {"id":"fresh_review", "access":"read_only",
                                    "prompt":"review the recovery publication",
                                    "source":{"type":"head", "node":"refresh"},
                                    "depends_on":["refresh", "fresh_conflict", "fresh_ci"],
                                    "route":{"alias":"senior"}},
                                {"id":"accepted", "kind":"gate", "access":"read_only",
                                    "depends_on":["fresh_review", "fresh_ci"],
                                    "inputs":[{"node":"fresh_review", "outputs":["verdict"]}],
                                    "condition":{"type":"verdict", "expected":"VERDICT: PASS", "threshold":"all"}}
                            ]}},
                    {"id":"ci", "kind":"gate", "access":"read_only",
                        "depends_on":["conflict", "publish"], "on_fail":"repair",
                        "condition":{"type":"ci", "target":{"node":"publish"}}},
                    {"id":"repair", "kind":"loop", "access":"read_only",
                        "depends_on":["ci", "publish", "hygiene"],
                        "inputs":[{"node":"ci", "outputs":["findings", "commit"]}],
                        "loop":{"max_rounds":2, "until":"accepted",
                            "retry_on":["verified_clean", "fresh_conflict", "fresh_ci", "accepted"],
                            "selected_publication":"refresh", "initial_publication":"publish",
                            "previous_bindings":{"refresh":"publish", "verified_clean":"hygiene"},
                            "body":[
                                {"id":"fix", "access":"worktree_write", "worktree":"/fixture/writer",
                                    "prompt":"Repair the failed CI head. Sweep the whole defect class across related tests, help text and documentation that asserts the changed behaviour.",
                                    "route":{"alias":"senior"},
                                    "source":{"type":"node", "node":"previous.verified_clean"},
                                    "inputs":[{"node":"previous.refresh", "outputs":["branch", "commit"]}]},
                                {"id":"verified_clean", "kind":"hygiene", "access":"worktree_write",
                                    "worktree":"/fixture/writer", "depends_on":["fix"],
                                    "source":{"type":"node", "node":"fix"}},
                                {"id":"refresh", "kind":"publish", "access":"worktree_write",
                                    "worktree":"/fixture/writer", "depends_on":["verified_clean"],
                                    "source":{"type":"node", "node":"verified_clean"}},
                                {"id":"fresh_conflict", "kind":"gate", "access":"read_only",
                                    "depends_on":["refresh"],
                                    "condition":{"type":"conflict", "target":{"node":"refresh"}}},
                                {"id":"fresh_ci", "kind":"gate", "access":"read_only",
                                    "depends_on":["fresh_conflict"],
                                    "condition":{"type":"ci", "target":{"node":"refresh"}}},
                                {"id":"fresh_review", "access":"read_only",
                                    "prompt":"review the repaired publication",
                                    "source":{"type":"head", "node":"refresh"},
                                    "depends_on":["refresh", "fresh_conflict", "fresh_ci"],
                                    "route":{"alias":"senior"}},
                                {"id":"accepted", "kind":"gate", "access":"read_only",
                                    "depends_on":["fresh_review", "fresh_ci"],
                                    "inputs":[{"node":"fresh_review", "outputs":["verdict"]}],
                                    "condition":{"type":"verdict", "expected":"VERDICT: PASS", "threshold":"all"}}
                            ]}},
                    {"id":"review", "kind":"composite", "access":"read_only",
                        "depends_on":["repair","publish"], "source":{"type":"head", "node":"publish"},
                        "delivery_review":{"shape":"single", "accept_below":"off"},
                        "composite":{"nodes":[
                            {"id":"lens", "access":"read_only", "prompt":"review the published head",
                                "route":{"alias":"senior"}, "require_findings":true,
                                "required_verdict":"VERDICT: PASS"}
                        ], "exports":[{"node":"lens", "outputs":["result", "findings", "verdict"]}]}},
                    {"id":"accept", "kind":"gate", "access":"read_only",
                        "depends_on":["review"],
                        "inputs":[{"node":"review", "outputs":["verdict"]}],
                        "condition":{"type":"verdict", "expected":"VERDICT: PASS", "threshold":"all"}}
                ],
                "exports":[{"node":"accept", "outputs":["verdict"]}]
            }
        }"#).unwrap())
        .unwrap();
    let mut graph = document(vec![composite]);
    graph.schema_version = 2;
    graph.integration_branch = Some("delivery/item".into());
    graph.queue = Some(QueueKnobs::default());
    let limits = crate::config::GraphLimits::default();
    graph.admit(&limits).unwrap();

    let expanded = graph.expanded(&limits).unwrap();
    let writer = expanded
        .nodes
        .iter()
        .find(|node| node.id == "delivery--writer")
        .unwrap();
    let hygiene = expanded
        .nodes
        .iter()
        .find(|node| node.id == "delivery--hygiene")
        .unwrap();
    let publish = expanded
        .nodes
        .iter()
        .find(|node| node.id == "delivery--publish")
        .unwrap();
    assert_eq!(writer.access, AccessMode::WorktreeWrite);
    assert_eq!(hygiene.kind, Some(NodeKind::Hygiene));
    assert!(matches!(
        &hygiene.source,
        Some(SourceBinding::Node { node }) if node == "delivery--writer"
    ));
    assert_eq!(publish.kind, Some(NodeKind::Publish));
    assert!(matches!(
        &publish.source,
        Some(SourceBinding::Node { node }) if node == "delivery--hygiene"
    ));
    let conflict = expanded
        .nodes
        .iter()
        .find(|node| node.id == "delivery--conflict")
        .unwrap();
    assert!(matches!(
        &conflict.condition,
        Some(GateCondition::Conflict {
            target: CiTarget::Node { node, .. },
            ..
        }) if node == "delivery--publish"
    ));
    assert_eq!(conflict.on_fail.as_deref(), Some("delivery--rebase"));
    let rebase = expanded
        .nodes
        .iter()
        .find(|node| node.id == "delivery--rebase")
        .unwrap();
    assert_eq!(rebase.kind, Some(NodeKind::Loop));
    assert_eq!(
        rebase
            .loop_body
            .as_ref()
            .unwrap()
            .selected_publication
            .as_deref(),
        Some("refresh")
    );
    let rebase_writer = expanded
        .nodes
        .iter()
        .find(|node| node.id == "delivery--rebase--r1--fix")
        .unwrap();
    assert_eq!(rebase_writer.worktree.as_deref(), Some("/fixture/writer"));
    assert!(matches!(
        &rebase_writer.source,
        Some(SourceBinding::Node { node }) if node == "delivery--hygiene"
    ));
    assert!(rebase_writer.inputs.iter().any(|input| {
        input.node == "delivery--publish"
            && input.outputs.contains(&OutputKind::Branch)
            && input.outputs.contains(&OutputKind::Commit)
    }));
    let ci = expanded
        .nodes
        .iter()
        .find(|node| node.id == "delivery--ci")
        .unwrap();
    assert!(matches!(
        &ci.condition,
        Some(GateCondition::Ci {
            target: CiTarget::Node { node, .. },
            ..
        }) if node == "delivery--publish"
    ));
    assert!(
        expanded
            .nodes
            .iter()
            .any(|node| { node.id == "delivery--review" && node.delivery_review.is_some() })
    );
    assert!(expanded.nodes.iter().any(|node| {
        node.id == "delivery--review--lens"
            && matches!(
                &node.source,
                Some(SourceBinding::Head { node }) if node == "delivery--publish"
            )
    }));
    let repair = expanded
        .nodes
        .iter()
        .find(|node| node.id == "delivery--repair")
        .unwrap();
    assert_eq!(repair.kind, Some(NodeKind::Loop));
    let repair_writer = expanded
        .nodes
        .iter()
        .find(|node| node.id == "delivery--repair--r1--fix")
        .unwrap();
    assert!(matches!(
        &repair_writer.source,
        Some(SourceBinding::Node { node }) if node == "delivery--hygiene"
    ));
    assert!(repair_writer.inputs.iter().any(|input| {
        input.node == "delivery--publish"
            && input.outputs.contains(&OutputKind::Branch)
            && input.outputs.contains(&OutputKind::Commit)
    }));
    assert!(repair.inputs.iter().any(|input| {
        input.node == "delivery--ci"
            && input.outputs.len() == 2
            && input.outputs.contains(&OutputKind::Findings)
            && input.outputs.contains(&OutputKind::Commit)
    }));
    assert!(repair_writer.prompt.contains("whole defect class"));
}

#[test]
fn old_plan_round_trips_without_new_contract_fields() {
    let graph = document(vec![
        Compiler::new(&library())
            .compile(&json!({
                "id":"task", "access":"read_only", "prompt":"work", "route":{"alias":"senior"}
            }))
            .unwrap(),
    ]);
    let wire = serde_json::to_value(&graph).unwrap();
    assert!(wire.get("queue").is_none());
    assert!(wire["nodes"][0].get("escalate").is_none());
    assert!(wire["nodes"][0].get("delivery_review").is_none());
    let decoded: GraphDocument = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), wire);
}

#[test]
fn delivery_loop_must_be_bounded() {
    let node = Compiler::new(&library())
        .compile(&json!({
            "id":"repair", "kind":"loop", "access":"read_only",
            "loop":{"body":[],"until":"gate","max_rounds":0,
                    "outcome_edge":{"outcome":"exhausted","node":"park"}}
        }))
        .unwrap();
    let mut graph = document(vec![node]);
    graph.schema_version = 2;
    assert!(
        matches!(graph.admit(&limits()), Err(AdmissionError::Plan(reason)) if reason.contains("max_rounds"))
    );
}

#[test]
fn bounded_loop_budget_covers_all_rounds_and_admission_rejects_unbounded_cycles() {
    let loop_node = Compiler::new(&library())
        .compile(&json!({
            "id":"repair", "kind":"loop", "access":"read_only",
            "loop":{"body":[
                {"id":"writer", "access":"worktree_write", "worktree":"/fixture/writer",
                    "prompt":"repair",
                    "write_scope":["src"], "route":{"alias":"junior"}},
                {"id":"ci", "kind":"gate", "access":"read_only",
                    "depends_on":["writer"], "condition":{"type":"budget"}}
            ], "until":"ci", "max_rounds":2}
        }))
        .unwrap();
    let mut graph = document(vec![loop_node]);
    graph.schema_version = 2;
    graph.queue = Some(QueueKnobs::default());
    let expanded = graph.admit_expanded(&limits()).unwrap();
    assert_eq!(
        expanded.nodes.len(),
        5,
        "two body nodes per repair round plus the loop boundary"
    );

    let mut unbounded = document(vec![
        serde_json::from_value(json!({
            "id":"first", "access":"read_only", "prompt":"first",
            "route":{"alias":"junior"}, "depends_on":["second"]
        }))
        .unwrap(),
        serde_json::from_value(json!({
            "id":"second", "access":"read_only", "prompt":"second",
            "route":{"alias":"junior"}, "depends_on":["first"]
        }))
        .unwrap(),
    ]);
    unbounded.schema_version = 2;
    assert!(unbounded.admit(&limits()).is_err());
}

#[test]
fn delivery_settings_require_v2_and_validate_overrides() {
    let mut graph = document(vec![
        Compiler::new(&library())
            .compile(&json!({
                "id":"task", "access":"read_only", "prompt":"work",
                "escalate":{"ladder":[], "max_ci_rounds":3}
            }))
            .unwrap(),
    ]);
    assert!(matches!(
        graph.admit(&limits()),
        Err(AdmissionError::Schema(_))
    ));
    graph.schema_version = 2;
    assert!(matches!(
        graph.admit(&limits()),
        Err(AdmissionError::Plan(_))
    ));
    let mut queue = QueueKnobs::default();
    queue.ci_slots.value = 0;
    assert!(queue.validate().is_ok());
    queue.max_parallel.value = 0;
    assert!(queue.validate().is_err());
    assert_eq!(
        DeliveryReviewSpec::default().shape,
        provenant_records::graph::ReviewShape::Single
    );
    assert_eq!(
        DeliveryReviewSpec::default().accept_below,
        AcceptBelow::Blocking
    );
}
