use super::preset::{Expansion, Layers, expand, find_prompt, request_bindings};
use provenant_fabric_runtime::read_roots::ReadRoots;
use serde_json::{Value, json};

#[test]
fn big_batch_keeps_seven_untyped_lanes_at_every_risk_tier() {
    for risk in ["routine", "substantial", "crucial", "terminal"] {
        let mut layers = Layers::empty();
        layers.review = Some(shapes());
        let lanes: Vec<_> = (1..=7).map(|n| json!({"name": format!("lane{n}"), "worktree": format!("/repo/lane{n}"), "scope": ["src"], "spec": "#1"})).collect();
        let expanded = super::preset::expand_with(
            &layers,
            "big-batch",
            "brief",
            &[
                ("lanes".into(), json!(lanes)),
                ("risk_tier".into(), json!(risk)),
            ],
            &[],
        )
        .unwrap();
        let nodes = expanded.document["nodes"].as_array().unwrap();
        let group = nodes.iter().find(|node| node["id"] == "lanes").unwrap();
        assert_eq!(
            group["composite"]["nodes"].as_array().unwrap().len(),
            7,
            "{risk}"
        );
    }
}

#[test]
fn live_writer_cwd_refusal_suggests_running_from_the_main_checkout() {
    let error = provenant_confinement::worktrees::WorktreeError::LiveProcess(
        provenant_confinement::worktrees::ObservedProcess {
            pid: 42,
            command: Some("zsh".into()),
            kind: provenant_confinement::worktrees::ProcessEvidenceKind::WorkingDirectory,
            cwd: None,
        },
    );
    assert!(
        error
            .to_string()
            .contains("run from the main checkout or another directory")
    );
}

#[test]
fn nested_role_index_belongs_to_each_child_count_copy() {
    let mut layers = Layers::empty();
    layers
        .insert(
            "test",
            "indexed-child",
            r#"
stages = [{ id = "q", role = "scout", count = 3 }]
"#,
        )
        .unwrap();
    layers.insert("test", "indexed-parent", r#"
stages = [{ id = "child", preset = "indexed-child", roles = { scout = { prompt = "Question ${index} for ${topic}", title = "Q${index}" } } }]
[parameters]
topic = "compiler"
"#).unwrap();
    let expanded = expand(&layers, "indexed-parent", "brief", &[]).unwrap();
    let children = expanded.document["nodes"][0]["composite"]["nodes"]
        .as_array()
        .unwrap();
    for (position, child) in children.iter().enumerate() {
        let index = position + 1;
        assert!(
            child["prompt"]
                .as_str()
                .unwrap()
                .contains(&format!("Question {index} for compiler"))
        );
        assert_eq!(child["title"], format!("Q{index}"));
    }
}

#[test]
fn node_titles_default_to_node_ids() {
    let mut layers = Layers::empty();
    layers
        .insert("test", "questions", "stages = [{id='q2', role='scout'}]")
        .unwrap();
    let expanded = expand(&layers, "questions", "brief", &[]).unwrap();
    assert_eq!(expanded.document["nodes"][0]["title"], "q2");
}

#[test]
fn injected_read_roots_are_optional_for_child_interfaces() {
    let mut layers = Layers::empty();
    layers
        .insert(
            "test",
            "optional-context",
            r#"
stages = [{id="child", preset="single", parameters={read_roots=["/repo/other"]}}]
"#,
        )
        .unwrap();
    assert!(expand(&layers, "optional-context", "brief", &[]).is_ok());
    assert!(
        expand(
            &layers,
            "optional-context",
            "brief",
            &["child.read_roots=['/repo/extra']".into()]
        )
        .is_ok()
    );
    let error = expand(
        &layers,
        "optional-context",
        "brief",
        &["child.typo=1".into()],
    )
    .unwrap_err();
    assert!(error.contains("typo"), "{error}");
}

#[test]
fn required_typed_parameters_name_the_missing_value_and_accept_overrides() {
    let mut layers = Layers::empty();
    layers
        .insert(
            "test",
            "required",
            "stages=[{id='task', prompt='${topic}'}]\n[parameters]\ntopic={type='string'}",
        )
        .unwrap();
    let error = expand(&layers, "required", "brief", &[]).unwrap_err();
    assert!(
        error.contains("parameters.topic") && error.contains("required parameter"),
        "{error}"
    );
    let expanded = expand(
        &layers,
        "required",
        "brief",
        &["parameters.topic='compiler'".into()],
    )
    .unwrap();
    assert_eq!(expanded.document["nodes"][0]["prompt"], "compiler");
    assert!(
        expand(
            &layers,
            "required",
            "brief",
            &["parameters.topic=42".into()]
        )
        .unwrap_err()
        .contains("must be string")
    );
}

#[test]
fn omitted_lenses_are_recorded_in_explain_provenance() {
    let mut layers = Layers::empty();
    layers.review = Some(shapes());
    let expanded = expand(
        &layers,
        "review-council",
        "brief",
        &["parameters.risk_tier='routine'".into()],
    )
    .unwrap();
    assert!(!expanded.warnings.is_empty());
    assert!(
        expanded
            .warnings
            .iter()
            .all(|warning| warning.contains("omitted lens"))
    );
    assert_eq!(
        expanded.document["provenance"]["warnings"],
        json!(expanded.warnings)
    );
}

#[test]
fn scope_preset_refuses_tree_write_broadening() {
    let layers = Layers::empty();
    let expanded = expand(&layers, "scope", "brief", &[]).unwrap();
    assert_eq!(expanded.document["nodes"][0]["access"], "read_only");
    for override_ in [
        "roles.scope.access='worktree_write'",
        "scope.access='full'",
        "scope.write_scope=['.']",
        "scope.kind='land'",
        "scope.delivery='pr'",
    ] {
        let error = expand(&layers, "scope", "brief", &[override_.into()]).unwrap_err();
        assert!(
            error.contains("scope preset refuses"),
            "{override_}: {error}"
        );
    }
    // The same floor applies when scope is composed as a child.
    let mut layers = layers;
    layers
        .insert(
            "test",
            "unsafe-scope",
            "stages=[{id='child',preset='scope',roles={scope={access='full'}}}]",
        )
        .unwrap();
    assert!(
        expand(&layers, "unsafe-scope", "brief", &[])
            .unwrap_err()
            .contains("scope preset refuses")
    );
    layers
        .insert(
            "test",
            "inherited-scope",
            "extends='scope'\n[roles.scope]\naccess='full'",
        )
        .unwrap();
    assert!(
        expand(&layers, "inherited-scope", "brief", &[])
            .unwrap_err()
            .contains("scope preset refuses")
    );
    layers
        .insert(
            "test",
            "custom-floor",
            "stages=[{id='reader',access='read_only',read_only_floor=true}]",
        )
        .unwrap();
    layers.insert("test", "custom-floor-extended", "extends='custom-floor'\nstages=[{id='writer',access='worktree_write',worktree='/repo/lane',write_scope=['src']}]").unwrap();
    assert!(
        expand(&layers, "custom-floor-extended", "brief", &[])
            .unwrap_err()
            .contains("write-broadening")
    );
    layers
        .insert("test", "scope-delivery", "extends='scope'\ndelivery='pr'")
        .unwrap();
    assert!(
        expand(&layers, "scope-delivery", "brief", &[])
            .unwrap_err()
            .contains("scope preset refuses")
    );
    layers.insert("test", "scope-before-write", "stages=[{id='scope',preset='scope'},{id='writer',access='worktree_write',worktree='/repo/lane',write_scope=['src']}]").unwrap();
    assert!(expand(&layers, "scope-before-write", "brief", &[]).is_ok());
}

#[test]
fn preset_role_titles_keep_user_and_inherited_overrides() {
    let mut layers = Layers::empty();
    layers
        .roles
        .add("user", "[roles.scout]\ntitle='User question ${index}'")
        .unwrap();
    layers
        .insert(
            "test",
            "user-title",
            "stages=[{id='q2',role='scout.codebase'}]\n[parameters]\nread_roots=[]",
        )
        .unwrap();
    let expanded = expand(&layers, "user-title", "brief", &[]).unwrap();
    assert_eq!(expanded.document["nodes"][0]["title"], "User question 1");
}

/// A node's primary route: read-only aliases repeat as `routes`, others keep `route`.
fn route(node: &serde_json::Value) -> &serde_json::Value {
    node.get("route").unwrap_or_else(|| &node["routes"][0])
}

fn nodes(name: &str) -> serde_json::Value {
    expand(&Layers::empty(), name, "brief", &[])
        .unwrap()
        .document["nodes"]
        .clone()
}

#[test]
fn roles_lower_distinct_leaf_jobs_and_tiers() {
    let mut layers = Layers::empty();
    layers
        .insert(
            "repo",
            "roles",
            r#"
stages = [{ id = "map", role = "scout" }, { id = "fix", role = "implementer" }]
[parameters]
worktree = "/repo/lane"
[roles.scout]
tier = "junior"
fallback_tier = "junior"
title = "Map the call sites"
prompt = "Find call sites for ${prompt}"
read_roots = ["/repo/other"]
[roles.implementer]
tier = "senior"
effort = "high"
access = "worktree_write"
worktree = "${worktree}"
write_scope = ["src"]
acceptance_checks = ["targeted tests"]
prompt = "Implement ${prompt}"
"#,
        )
        .unwrap();
    layers.known_routes = Some(["senior".to_owned()].into());
    let expanded = expand(&layers, "roles", "the change", &[]).unwrap();
    let leaves = &expanded.document["nodes"];
    let exact = expand(
        &layers,
        "roles",
        "the change",
        &["map.tier='codex/gpt-6@high'".into()],
    )
    .unwrap();
    assert_eq!(
        *route(&exact.document["nodes"][0]),
        json!({"adapter":"codex","model":"gpt-6","effort":"high"})
    );
    assert_eq!(*route(&leaves[0]), json!({"alias":"junior"}));
    assert_eq!(leaves[0]["title"], "Map the call sites");
    assert_eq!(leaves[0]["prompt"], "Find call sites for the change");
    assert_eq!(leaves[0]["access"], "read_only");
    assert_eq!(leaves[0]["read_roots"], json!(["/repo/other"]));
    assert_eq!(
        leaves[1]["route"],
        json!({"alias":"senior","effort":"high"})
    );
    assert_eq!(leaves[1]["worktree"], "/repo/lane");
    assert_eq!(leaves[1]["write_scope"], json!(["src"]));
    assert_eq!(leaves[1]["acceptance_checks"], json!(["targeted tests"]));
    assert_eq!(leaves[1]["depends_on"], json!(["map"]));
    assert!(
        leaves[1]["prompt"]
            .as_str()
            .unwrap()
            .contains("Write scope (relative literal paths):\n- src")
    );
    assert!(
        leaves[1]["prompt"]
            .as_str()
            .unwrap()
            .contains("Acceptance checks:\n- targeted tests")
    );
}

#[test]
fn expanded_preset_retains_pattern_attribution() {
    let expansion = expand(&Layers::empty(), "fan-out", "brief", &[]).unwrap();
    assert_eq!(expansion.document["context"]["preset"], "fan-out");
    assert_eq!(expansion.document["context"]["pattern"], "fan-out");
}

#[test]
fn stages_keep_nested_presets_and_heterogeneous_parallel_entries_grouped() {
    let mut layers = Layers::empty();
    layers
        .insert(
            "repo",
            "staged",
            r#"
stages = [
  { id = "scouts", parallel = [{ role = "scout", count = 2 }, { preset = "single" }] },
  { id = "summary", role = "summary" },
]
exports = [{ node = "summary", outputs = ["result", "findings"] }]
[roles.scout]
tier = "junior"
prompt = "Scope ${index}: ${prompt}"
[roles.summary]
tier = "senior"
kind = "synthesise"
"#,
        )
        .unwrap();
    let expanded = expand(&layers, "staged", "map", &[]).unwrap();
    let nodes = &expanded.document["nodes"];
    assert_eq!(nodes.as_array().unwrap().len(), 2);
    assert_eq!(nodes[0]["kind"], "composite");
    let scouts = &nodes[0]["composite"]["nodes"];
    assert_eq!(scouts.as_array().unwrap().len(), 3);
    assert_eq!(route(&scouts[0])["alias"], "junior");
    assert_eq!(scouts[0]["prompt"], "Scope 1: map");
    assert_eq!(scouts[1]["prompt"], "Scope 2: map");
    assert_eq!(scouts[2]["kind"], "composite");
    assert_eq!(nodes[1]["depends_on"], json!(["scouts"]));
    assert_eq!(
        nodes[1]["inputs"],
        json!([{"node":"scouts","outputs":["result"]}])
    );
    let _: provenant_fabric_runtime::graph::GraphDocument =
        serde_json::from_value(expanded.document).unwrap();
}

#[test]
fn raw_node_kinds_and_control_role_profiles_keep_their_configuration() {
    use provenant_fabric_runtime::graph::{GraphDocument, NodeKind};
    let cases = [
        (
            "raw-loop",
            r#"
stages = [{ id = "repair", kind = "loop", loop = { max_rounds = 1, until = "accept", body = [{ id = "accept", kind = "gate", access = "read_only", condition = { type = "budget" } }] } }]
"#,
            NodeKind::Loop,
        ),
        (
            "role-loop",
            r#"
stages = [{ id = "repair", role = "repair-loop" }]
[roles.repair-loop]
kind = "loop"
loop = { max_rounds = 1, until = "accept", body = [{ id = "accept", kind = "gate", access = "read_only", condition = { type = "budget" } }] }
"#,
            NodeKind::Loop,
        ),
        (
            "role-gate",
            r#"
stages = [{ id = "accept", role = "budget-gate" }]
[roles.budget-gate]
kind = "gate"
condition = { type = "budget" }
"#,
            NodeKind::Gate,
        ),
        (
            "role-plan",
            r#"
stages = [{ id = "plan", role = "planner" }]
[roles.planner]
kind = "plan"
tier = "senior"
prompt = "Plan the approved slice"
plan = { kinds = ["task"], max_nodes = 2 }
"#,
            NodeKind::Plan,
        ),
        (
            "role-composite",
            r#"
stages = [{ id = "group", role = "grouped-check" }]
[roles.grouped-check]
kind = "composite"
composite = { nodes = [{ id = "check", access = "read_only", prompt = "Check the approved slice", route = { alias = "senior" } }], exports = [{ node = "check", outputs = ["result"] }] }
"#,
            NodeKind::Composite,
        ),
    ];
    for (name, text, kind) in cases {
        let mut layers = Layers::empty();
        layers.insert("repo", name, text).unwrap();
        let expanded = expand(&layers, name, "approved brief", &[])
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        let document: GraphDocument = serde_json::from_value(expanded.document).unwrap();
        assert_eq!(document.nodes[0].kind, Some(kind), "{name}");
        document
            .inspect(&Default::default())
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        match kind {
            NodeKind::Loop => {
                let spec = document.nodes[0].loop_body.as_ref().unwrap();
                assert_eq!(spec.max_rounds, 1);
                assert_eq!(spec.until, "accept");
                assert_eq!(spec.body[0].kind, Some(NodeKind::Gate));
            }
            NodeKind::Gate => assert!(document.nodes[0].condition.is_some()),
            NodeKind::Plan => assert_eq!(document.nodes[0].plan.as_ref().unwrap().max_nodes, 2),
            NodeKind::Composite => {
                assert_eq!(document.nodes[0].composite.as_ref().unwrap().nodes.len(), 1)
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn bundled_role_presets_have_bounded_nested_graphs() {
    for name in [
        "scout-swarm",
        "generator-evaluator",
        "review-council",
        "big-batch",
        "scope-then-run",
        "deliver",
    ] {
        let document = bundled_fixture(name);
        let config = provenant_fabric_runtime::config::GraphConfig::default();
        let limits = config.limits.clone();
        let inspection = document
            .inspect(&limits)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(
            inspection.limit_reason.is_none(),
            "{name}: {:?}",
            inspection.limit_reason
        );
        assert!(inspection.nodes <= 512);
    }
}

#[test]
fn require_findings_nodes_instruct_a_findings_block() {
    fn walk(nodes: &[Value], preset: &str) {
        for node in nodes {
            if node["require_findings"] == true {
                let typed: provenant_fabric_runtime::graph::GraphNode =
                    serde_json::from_value(node.clone()).unwrap();
                let rendered = provenant_fabric_runtime::graph::rendered_prompt(&typed);
                let prompt = rendered.as_str();
                assert!(
                    prompt.contains("```findings"),
                    "{preset}: {} lacks findings fence",
                    node["id"]
                );
                assert!(
                    prompt.contains("{\"schema_version\":1,\"findings\":[]}"),
                    "{preset}: {} lacks empty findings form",
                    node["id"]
                );
            }
            for key in ["composite", "loop"] {
                for child_key in ["nodes", "body"] {
                    if let Some(children) = node
                        .pointer(&format!("/{key}/{child_key}"))
                        .and_then(Value::as_array)
                    {
                        walk(children, preset);
                    }
                }
            }
            if let Some(nodes) = node
                .pointer("/plan/fallback/nodes")
                .and_then(Value::as_array)
            {
                walk(nodes, preset);
            }
            for pattern in node
                .pointer("/plan/patterns")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(nodes) = pattern.get("nodes").and_then(Value::as_array) {
                    walk(nodes, preset);
                }
            }
        }
    }
    for name in [
        "scout-swarm",
        "generator-evaluator",
        "review-council",
        "big-batch",
        "scope-then-run",
        "deliver",
    ] {
        let expanded = expand(&Layers::empty(), name, "approved brief", &[]).unwrap();
        walk(expanded.document["nodes"].as_array().unwrap(), name);
    }
}

#[test]
fn explain_pending_checks_follow_the_document() {
    let cases = [
        (
            json!({"nodes":[{"id":"read","access":"read_only","prompt":"x"}]}),
            vec!["route_readiness"],
        ),
        (
            json!({"delivery":"integrate:main","nodes":[{"id":"write","access":"worktree_write","worktree":"/repo/w","prompt":"x"}]}),
            vec!["delivery_target", "route_readiness", "worktree"],
        ),
        (
            json!({"nodes":[{"id":"upstream","access":"read_only","prompt":"x"},{"id":"downstream","access":"read_only","prompt":"y","source":{"type":"node","node":"upstream"},"inputs":[{"node":"upstream","outputs":["result"]}]}]}),
            vec!["route_readiness", "source"],
        ),
        (
            json!({"nodes":[{"id":"plan","kind":"plan","access":"read_only","prompt":"x"}]}),
            vec!["planner_authority", "route_readiness"],
        ),
    ];
    for (document, expected) in cases {
        let checks = super::preset::pending_checks(&document);
        let ids: Vec<_> = checks
            .iter()
            .filter_map(|check| check["id"].as_str())
            .collect();
        assert_eq!(ids, expected);
        assert!(checks.iter().all(|check| {
            check["reason"]
                .as_str()
                .is_some_and(|reason| reason.lines().count() == 1)
        }));
    }
}

#[test]
fn admission_preset_version_follows_document() {
    for name in [
        "scout-swarm",
        "generator-evaluator",
        "review-council",
        "big-batch",
        "scope-then-run",
        "deliver",
    ] {
        let document = bundled_fixture(name);
        let raw: Value = serde_json::to_value(&document).unwrap();
        document
            .admit(&provenant_fabric_runtime::config::GraphConfig::default().limits)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let raw_document: provenant_fabric_runtime::graph::GraphDocument =
            serde_json::from_value(raw).unwrap();
        assert_eq!(
            document.admit(&Default::default()),
            raw_document.admit(&Default::default()),
            "{name}"
        );
        assert_eq!(document.schema_version, 1, "{name}");
    }
    let mut review = expand(
        &real_selector("/nonexistent"),
        "deliver",
        "approved brief",
        &[
            "parameters.worktree='/repo/w'".into(),
            "parameters.base='aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'".into(),
        ],
    )
    .unwrap();
    pin_fixture_sources(&mut review.document);
    let review_doc: provenant_fabric_runtime::graph::GraphDocument =
        serde_json::from_value(review.document.clone()).unwrap();
    assert_eq!(review_doc.schema_version, 2);
    review_doc
        .admit(&provenant_fabric_runtime::config::GraphConfig::default().limits)
        .unwrap();
    let raw: provenant_fabric_runtime::graph::GraphDocument =
        serde_json::from_value(review.document).unwrap();
    assert_eq!(
        review_doc.admit(&Default::default()),
        raw.admit(&Default::default())
    );
    let raw_v2: provenant_fabric_runtime::graph::GraphDocument = serde_json::from_value(json!({
        "schema_version":2,"graph_id":"raw-v2","nodes":[{"id":"task","access":"read_only","prompt":"x"}]
    })).unwrap();
    assert_eq!(raw_v2.schema_version, 2);
    assert_eq!(
        provenant_fabric_runtime::graph::minimum_schema_version(&raw_v2),
        1
    );
}

#[test]
fn implementer_and_scope_briefs_carry_the_authority_envelope() {
    let mut expanded = expand(
        &Layers::empty(),
        "generator-evaluator",
        "approved brief",
        &["parameters.worktree='/repo/lane'".into()],
    )
    .unwrap();
    pin_fixture_sources(&mut expanded.document);
    let brief = expanded.document["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["id"] == "implement")
        .unwrap()["prompt"]
        .as_str()
        .unwrap()
        .to_owned();
    for needle in [
        "approved brief",
        "Spec (acceptance criteria)",
        "Authority envelope",
        "reversible",
        "DECISIONS",
        "NEEDS-USER",
    ] {
        assert!(brief.contains(needle), "{needle}: {brief}");
    }
}

fn bundled_fixture(name: &str) -> provenant_fabric_runtime::graph::GraphDocument {
    let overrides = match name {
        "generator-evaluator" | "review-council" => vec!["parameters.worktree='/repo/lane'".into()],
        "scope-then-run" | "deliver" => vec!["parameters.worktree='/repo/lane'".into()],
        "big-batch" => vec![
            "parameters.lanes=[{name='one',worktree='/repo/one',scope=['.'],spec=''},{name='two',worktree='/repo/two',scope=['.'],spec=''}]".into(),
            "parameters.batch_worktree='/repo/batch'".into(),
        ],
        _ => Vec::new(),
    };
    let mut expanded = expand(&Layers::empty(), name, "approved brief", &overrides).unwrap();
    pin_fixture_sources(&mut expanded.document);
    serde_json::from_value(expanded.document).unwrap()
}

#[derive(Default)]
struct PresetJournal(Option<provenant_fabric_runtime::graph::GraphCheckpoint>);

impl provenant_fabric_runtime::graph::GraphJournal for PresetJournal {
    type Error = std::convert::Infallible;

    fn load(
        &mut self,
        _: &str,
    ) -> Result<Option<provenant_fabric_runtime::graph::GraphCheckpoint>, Self::Error> {
        Ok(self.0.clone())
    }

    fn save(
        &mut self,
        checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
    ) -> Result<(), Self::Error> {
        self.0 = Some(checkpoint.clone());
        Ok(())
    }
}

/// The fake provider runs in a separate process and reads the files named in
/// its brief, as a real provider must do when a composite export is truncated.
#[test]
fn fake_council_provider_process() {
    let Some(request) = std::env::var_os("PROVENANT_TEST_COUNCIL_REQUEST") else {
        return;
    };
    let request: Value = serde_json::from_slice(&std::fs::read(request).unwrap()).unwrap();
    let id = request["id"].as_str().unwrap();
    let prompt = request["prompt"].as_str().unwrap();
    let text = if id.starts_with("lenses--") {
        let findings: Vec<_> = (0..10).map(|i| json!({"id":format!("F{i}"),"severity":"high","file":"a.rs","summary":"issue","status":"open"})).collect();
        format!(
            "```findings\n{}\n```\nVERDICT: PASS\n",
            json!({"schema_version":1,"findings":findings})
        )
    } else if ["verify", "synthesis"].contains(&id) {
        assert!(prompt.contains("composite findings truncated:"));
        let mut text = String::new();
        let mut structured = Vec::new();
        let mut omitted = false;
        let mut changed = false;
        for file in request["files"].as_array().unwrap() {
            let lens = file["label"].as_str().unwrap();
            let path = file["path"].as_str().unwrap();
            let source = prompt
                .lines()
                .filter_map(|line| {
                    line.strip_prefix("composite findings truncated: full member result: ")
                        .and_then(|source| serde_json::from_str::<Value>(source).ok())
                })
                .find(|source| source["label"] == lens)
                .unwrap();
            let named_path = source["path"].as_str().unwrap();
            assert_eq!(named_path, path);
            let full = std::fs::read_to_string(named_path).unwrap();
            let block = full
                .split_once("```findings\n")
                .unwrap()
                .1
                .split_once("```")
                .unwrap()
                .0;
            let findings: Value = serde_json::from_str(block).unwrap();
            for (index, finding) in findings["findings"].as_array().unwrap().iter().enumerate() {
                let qualified = format!("{lens}:{}", finding["id"].as_str().unwrap());
                text.push_str(&format!("{qualified}: rejected, evidence refutes it\n"));
                if id == request["omit_stage"].as_str().unwrap_or("") && !omitted {
                    omitted = true;
                } else {
                    let status = if id == "verify" {
                        "falsified"
                    } else if request["mismatch_proof"] == true && !changed {
                        changed = true;
                        "accepted"
                    } else {
                        "rejected"
                    };
                    let proof = if request["reference_proof"] == true {
                        json!({"member":lens,"index":index,"disposition":status,"reason":"evidence refutes it"})
                    } else {
                        json!({"id":qualified,"disposition":status,"reason":"evidence refutes it"})
                    };
                    text.push_str(&format!("DISPOSITION {proof}\n"));
                }
                if structured.len() < 32 {
                    let mut item = json!({"id":qualified,"severity":"high","file":"a.rs","summary":"issue","status":"open","disposition":"rejected","reason":"evidence refutes it","verification":"falsified"});
                    if id == "verify" {
                        item["propagates"] = json!(format!("lenses:{qualified}"));
                    }
                    structured.push(item);
                }
            }
        }
        text.push_str(&format!(
            "```findings\n{}\n```\nVERDICT: PASS\n",
            json!({"schema_version":1,"findings":structured})
        ));
        text
    } else {
        "framed\nVERDICT: PASS\n".into()
    };
    std::fs::write(request["output"].as_str().unwrap(), text).unwrap();
}

#[derive(Default)]
struct PresetOwner {
    council_findings: bool,
    /// Nodes whose result carries `VERDICT: FAIL`.
    failing: Vec<String>,
    many_findings: bool,
    omit_overflow_proof: bool,
    omit_verifier_proof: bool,
    mismatch_proof: bool,
    reference_proof: bool,
    input_dir: Option<tempfile::TempDir>,
    captured_results: std::collections::BTreeMap<String, String>,
    launched: Vec<String>,
    /// The brief each launched node received, inputs included.
    prompts: Vec<(String, String)>,
    contexts: Vec<provenant_records::run::WorkContext>,
    integration_pins: usize,
    /// Integration head once a writer has integrated; the pin until then.
    integration: Option<String>,
}

impl provenant_fabric_runtime::graph::AttemptOwner for PresetOwner {
    type Error = std::convert::Infallible;

    fn actor(&self) -> String {
        "preset-test-chair".into()
    }

    fn integration_head(&mut self) -> Result<Option<String>, Self::Error> {
        self.integration_pins += 1;
        Ok(Some(self.integration.clone().unwrap_or_else(|| {
            "0123456789abcdef0123456789abcdef01234567".into()
        })))
    }

    fn materialise_inputs(
        &mut self,
        key: &provenant_fabric_runtime::graph::AttemptKey,
        requests: &[provenant_fabric_runtime::graph::InputRequest],
        _: usize,
    ) -> Option<provenant_fabric_runtime::graph::MaterialisedInputs> {
        use provenant_fabric_runtime::graph::{MaterialisedInput, MaterialisedInputs};
        let dir = self.input_dir.as_ref()?.path().join(&key.node_id);
        std::fs::create_dir_all(&dir).unwrap();
        let entries = requests
            .iter()
            .map(|request| {
                let text = &self.captured_results[request.attempt_ref.as_ref().unwrap()];
                let path = dir.join(&request.label);
                std::fs::write(&path, text).unwrap();
                MaterialisedInput::File {
                    label: request.label.clone(),
                    path,
                    bytes: text.len() as u64,
                    omitted: 0,
                }
            })
            .collect();
        Some(MaterialisedInputs {
            index: None,
            directory: Some(dir),
            entries,
        })
    }

    fn observe_or_start(
        &mut self,
        key: &provenant_fabric_runtime::graph::AttemptKey,
        node: provenant_fabric_runtime::graph::NodeInput<'_>,
        _: &provenant_fabric_runtime::graph::GraphRoute,
        access: provenant_fabric_runtime::graph::AccessMode,
        _: Option<&str>,
        replay: provenant_fabric_runtime::graph::Replay<'_>,
    ) -> Result<provenant_fabric_runtime::graph::AttemptObservation, Self::Error> {
        use provenant_fabric_runtime::graph::{AccessMode, AttemptObservation, AttemptOutcome};
        self.launched.push(key.node_id.clone());
        self.prompts
            .push((key.node_id.clone(), node.prompt.to_owned()));
        self.contexts.push(node.context.clone());
        let mut result =
            provenant_records::result_capture::ResultCapture::new(node.required_verdict);
        let text = if self.many_findings {
            let directory = self.input_dir.as_ref().unwrap().path();
            let request_path = directory.join(format!("request-{}.json", key.node_id));
            let output_path = directory.join(format!("output-{}.txt", key.node_id));
            let files: Vec<_> = self
                .launched
                .iter()
                .filter(|id| id.starts_with("lenses--") && *id != &key.node_id)
                .map(|id| json!({"label": id, "path": directory.join(&key.node_id).join(id)}))
                .collect();
            std::fs::write(&request_path, serde_json::to_vec(&json!({"id":key.node_id,"prompt":node.prompt,"files":files,"output":output_path,"reference_proof":self.reference_proof,"mismatch_proof":self.mismatch_proof,"omit_stage":if self.omit_verifier_proof { "verify" } else if self.omit_overflow_proof { "synthesis" } else { "" }})).unwrap()).unwrap();
            #[allow(
                clippy::disallowed_methods,
                reason = "the test exercises a separate fake-provider process"
            )]
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "preset_tests::fake_council_provider_process"])
                .env("PROVENANT_TEST_COUNCIL_REQUEST", &request_path)
                .output()
                .unwrap();
            assert!(
                child.status.success(),
                "{}",
                String::from_utf8_lossy(&child.stdout)
            );
            std::fs::read_to_string(output_path).unwrap()
        } else if self.failing.contains(&key.node_id) {
            "Blocked on the user.\nVERDICT: FAIL\n".into()
        } else if self.council_findings && key.node_id == "verify" {
            "```findings\n{\"schema_version\":1,\"findings\":[{\"id\":\"F1\",\"severity\":\"high\",\"file\":\"a.rs\",\"summary\":\"fixture finding\",\"status\":\"open\"}]}\n```\nVERDICT: PASS\n".into()
        } else if self.council_findings && key.node_id == "synthesis" {
            "```findings\n{\"schema_version\":1,\"findings\":[{\"id\":\"F1\",\"disposition\":\"rejected\",\"reason\":\"falsified\"}]}\n```\nVERDICT: PASS\n".into()
        } else {
            format!(
                "Evidence checked for {}.\n```findings\n{{\"schema_version\":1,\"findings\":[]}}\n```\nVERDICT: PASS\n",
                key.node_id
            )
        };
        result.push(text.as_bytes());
        self.captured_results
            .insert(format!("fixture/{}", key.node_id), text);
        let commit = if access == AccessMode::WorktreeWrite {
            Some(format!("{:040x}", self.launched.len()))
        } else {
            node.source_commit.map(str::to_owned)
        };
        if access == AccessMode::WorktreeWrite
            && matches!(
                replay.delivery,
                Some(provenant_records::run::DeliveryMode::Integrate(_))
            )
        {
            self.integration = commit.clone();
        }
        Ok(AttemptObservation::Terminal {
            attempt_ref: Some(format!("fixture/{}", key.node_id)),
            outcome: AttemptOutcome::Succeeded,
            launched: true,
            result: Some(Box::new(result.finish())),
            result_warning: None,
            result_path: self
                .many_findings
                .then(|| format!("/results/{}", key.node_id)),
            branch: (access == AccessMode::WorktreeWrite).then(|| format!("lane/{}", key.node_id)),
            commit,
        })
    }
}

#[test]
fn graph_attempt_input_carries_preset_selection_context() {
    use provenant_fabric_runtime::graph::advance_with_budget;
    let mut document = bundled_fixture("single");
    document.context = Some(provenant_records::run::WorkContext {
        preset: Some("single".into()),
        selection_source: Some("readiness".into()),
        ..Default::default()
    });
    let config = provenant_fabric_runtime::config::GraphConfig::default();
    let mut owner = PresetOwner::default();
    let mut journal = PresetJournal::default();
    advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &config.limits,
        &config.budget,
    )
    .unwrap();
    let context = &owner.contexts[0];
    assert_eq!(context.preset.as_deref(), Some("single"));
    assert_eq!(context.selection_source.as_deref(), Some("readiness"));
    assert_eq!(
        context.graph_id.as_deref(),
        Some(document.graph_id.as_str())
    );
    assert_eq!(context.node_id.as_deref(), Some(owner.launched[0].as_str()));
}

#[test]
fn preset_wait_retries_an_injected_scratch_failure_after_graph_admission() {
    let mut scratch = super::fabric::PreparedScratch::default();
    let mut attempts = 0;
    let result = loop {
        attempts += 1;
        match scratch.get_or_prepare(|| {
            if attempts == 1 {
                Err(super::fabric::ScratchPreparationError(
                    provenant_confinement::worktrees::WorktreeError::Filesystem(
                        provenant_os::fs::FsError::LockTimeout {
                            path: ".git/info/exclude".into(),
                            timeout: std::time::Duration::from_millis(100),
                        },
                    ),
                ))
            } else {
                Ok(".provenant/scratch".into())
            }
        }) {
            Err(error) if super::fabric::retry_scratch_poll(&error, true) => {
                continue;
            }
            result => break result,
        }
    };
    assert_eq!(attempts, 2);
    assert_eq!(result.unwrap(), std::path::Path::new(".provenant/scratch"));

    let scratch_error = super::fabric::ScratchPreparationError(
        provenant_confinement::worktrees::WorktreeError::Filesystem(
            provenant_os::fs::FsError::LockTimeout {
                path: ".git/info/exclude".into(),
                timeout: std::time::Duration::from_millis(100),
            },
        ),
    );
    assert!(!super::fabric::retry_scratch_poll(&scratch_error, false));
    let permanent_error = super::fabric::ScratchPreparationError(
        provenant_confinement::worktrees::WorktreeError::Filesystem(
            provenant_os::fs::FsError::Symlink {
                path: ".provenant/scratch".into(),
                source: std::io::Error::other("symlink"),
            },
        ),
    );
    assert!(!super::fabric::retry_scratch_poll(&permanent_error, true));

    let exhausted =
        super::fabric::scratch_prepare_error_message(&scratch_error, Some("admitted-graph"), true);
    assert!(
        exhausted
            .as_deref()
            .is_some_and(|message| message.contains("admitted-graph")
                && message.contains(".provenant/scratch")
                && message.contains("lock acquisition timed out"))
    );
    assert_eq!(
        super::fabric::scratch_prepare_error_message(
            &permanent_error,
            Some("admitted-graph"),
            true,
        ),
        None
    );
}

#[test]
fn preset_wait_prepares_scratch_once_across_polls() {
    let mut scratch = super::fabric::PreparedScratch::default();
    let mut calls = 0;
    for _ in 0..5 {
        let path = scratch
            .get_or_prepare(|| {
                calls += 1;
                Ok(".provenant/scratch".into())
            })
            .unwrap();
        assert_eq!(path, std::path::Path::new(".provenant/scratch"));
    }
    assert_eq!(calls, 1);
}

#[test]
fn review_council_continues_when_lens_findings_exceed_the_cap() {
    use provenant_fabric_runtime::graph::{GraphStatus, NodeState, advance_with_budget};
    let document = bundled_fixture("review-council");
    let config = provenant_fabric_runtime::config::GraphConfig::default();
    let mut owner = PresetOwner {
        many_findings: true,
        input_dir: Some(tempfile::tempdir().unwrap()),
        ..Default::default()
    };
    let run = advance_with_budget(
        &document,
        &mut owner,
        &mut PresetJournal::default(),
        &config.limits,
        &config.budget,
    )
    .unwrap();
    assert_eq!(
        run.status,
        GraphStatus::Complete,
        "{:?}",
        run.checkpoint.nodes
    );
    for id in ["verify", "synthesis"] {
        assert_eq!(
            run.checkpoint
                .nodes
                .iter()
                .find(|n| n.id == id)
                .unwrap()
                .state,
            NodeState::Succeeded
        );
        let output = run
            .checkpoint
            .nodes
            .iter()
            .find(|node| node.id == id)
            .unwrap()
            .outputs
            .as_ref()
            .unwrap();
        if id == "verify" {
            assert_eq!(output.findings.len(), 32);
        } else {
            // The structured decisions cover 32 verifier findings. Of those, 28
            // also appear in the capped lens export; settlement retains both copies.
            // The overflow proof covers the remaining lens findings.
            let lenses = run
                .checkpoint
                .nodes
                .iter()
                .find(|n| n.id == "lenses")
                .unwrap();
            let originals = &lenses.outputs.as_ref().unwrap().findings;
            let verifier = run
                .checkpoint
                .nodes
                .iter()
                .find(|n| n.id == "verify")
                .unwrap();
            let copies = &verifier.outputs.as_ref().unwrap().findings;
            assert_eq!(originals.len(), 32);
            assert_eq!(copies.len(), 32);
            assert_eq!(output.findings.len(), 60);
            assert!(
                output
                    .findings
                    .iter()
                    .all(|f| f.disposition.as_deref() == Some("rejected"))
            );
        }
        let prompt = &owner.prompts.iter().find(|(node, _)| node == id).unwrap().1;
        assert!(
            prompt.contains("composite findings truncated"),
            "{id}: {prompt}"
        );
        assert!(
            prompt.contains(
                "composite findings truncated: full member result: {\"label\":\"lenses--"
            ),
            "{id}: {prompt}"
        );
        assert!(!prompt.contains("/results/lenses--"), "{id}: {prompt}");
        let report = &owner.captured_results[&format!("fixture/{id}")];
        for lens in owner
            .launched
            .iter()
            .filter(|name| name.starts_with("lenses--"))
        {
            for i in 0..10 {
                assert!(report.contains(&format!("{lens}:F{i}: rejected")));
            }
        }
    }
}

#[test]
fn review_council_accepts_compact_overflow_dispositions() {
    use provenant_fabric_runtime::graph::{GraphStatus, advance_with_budget};
    let document = bundled_fixture("review-council");
    let config = provenant_fabric_runtime::config::GraphConfig::default();
    let mut owner = PresetOwner {
        many_findings: true,
        reference_proof: true,
        input_dir: Some(tempfile::tempdir().unwrap()),
        ..Default::default()
    };
    let run = advance_with_budget(
        &document,
        &mut owner,
        &mut PresetJournal::default(),
        &config.limits,
        &config.budget,
    )
    .unwrap();
    assert_eq!(
        run.status,
        GraphStatus::Complete,
        "{:?}",
        run.checkpoint.nodes
    );
}

#[test]
fn review_council_refuses_a_missing_overflow_disposition() {
    use provenant_fabric_runtime::graph::{NodeState, advance_with_budget};
    let document = bundled_fixture("review-council");
    let config = provenant_fabric_runtime::config::GraphConfig::default();
    let mut owner = PresetOwner {
        many_findings: true,
        omit_overflow_proof: true,
        input_dir: Some(tempfile::tempdir().unwrap()),
        ..Default::default()
    };
    let run = advance_with_budget(
        &document,
        &mut owner,
        &mut PresetJournal::default(),
        &config.limits,
        &config.budget,
    )
    .unwrap();
    let synthesis = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "synthesis")
        .unwrap();
    assert_eq!(synthesis.state, NodeState::Failed);
    assert_eq!(
        synthesis.advance_error_code.as_deref(),
        Some("disposition_overflow_incomplete")
    );
}

#[test]
fn review_council_refuses_a_missing_overflow_verification() {
    use provenant_fabric_runtime::graph::{NodeState, advance_with_budget};
    let document = bundled_fixture("review-council");
    let config = provenant_fabric_runtime::config::GraphConfig::default();
    let mut owner = PresetOwner {
        many_findings: true,
        omit_verifier_proof: true,
        input_dir: Some(tempfile::tempdir().unwrap()),
        ..Default::default()
    };
    let run = advance_with_budget(
        &document,
        &mut owner,
        &mut PresetJournal::default(),
        &config.limits,
        &config.budget,
    )
    .unwrap();
    let verify = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "verify")
        .unwrap();
    assert_eq!(verify.state, NodeState::Failed);
    assert_eq!(
        verify.advance_error_code.as_deref(),
        Some("disposition_overflow_incomplete")
    );
    assert_eq!(
        run.checkpoint
            .nodes
            .iter()
            .find(|node| node.id == "synthesis")
            .unwrap()
            .state,
        NodeState::Blocked
    );
}

#[test]
fn review_council_refuses_conflicting_overflow_dispositions() {
    use provenant_fabric_runtime::graph::{NodeState, advance_with_budget};
    let document = bundled_fixture("review-council");
    let config = provenant_fabric_runtime::config::GraphConfig::default();
    let mut owner = PresetOwner {
        many_findings: true,
        mismatch_proof: true,
        input_dir: Some(tempfile::tempdir().unwrap()),
        ..Default::default()
    };
    let run = advance_with_budget(
        &document,
        &mut owner,
        &mut PresetJournal::default(),
        &config.limits,
        &config.budget,
    )
    .unwrap();
    let synthesis = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "synthesis")
        .unwrap();
    assert_eq!(synthesis.state, NodeState::Failed);
    assert_eq!(
        synthesis.advance_error_code.as_deref(),
        Some("disposition_overflow_inconsistent")
    );
}

#[test]
fn bundled_presets_complete_through_the_real_scheduler_and_replay() {
    use provenant_fabric_runtime::graph::{GraphStatus, NodeState, advance_with_budget};
    for name in [
        "scout-swarm",
        "generator-evaluator",
        "review-council",
        "big-batch",
        "scope-then-run",
    ] {
        let document = bundled_fixture(name);
        let config = provenant_fabric_runtime::config::GraphConfig::default();
        let limits = config.limits.clone();
        let mut owner = PresetOwner {
            council_findings: name == "review-council",
            ..Default::default()
        };
        let mut journal = PresetJournal::default();
        let run = advance_with_budget(&document, &mut owner, &mut journal, &limits, &config.budget)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        if name == "review-council" {
            let synthesis = run
                .checkpoint
                .nodes
                .iter()
                .find(|node| node.id == "synthesis")
                .unwrap();
            assert_eq!(
                synthesis.outputs.as_ref().unwrap().findings[0].summary,
                "fixture finding"
            );
        }
        assert_eq!(
            run.status,
            GraphStatus::Complete,
            "{name}: {:?}",
            run.checkpoint.pause_reason
        );
        assert!(
            run.checkpoint
                .nodes
                .iter()
                .all(|node| matches!(node.state, NodeState::Succeeded | NodeState::Skipped)),
            "{name}: {:?}",
            run.checkpoint.nodes
        );
        assert!(
            run.checkpoint
                .nodes
                .iter()
                .filter(|node| !node.children.is_empty())
                .all(|node| node.attempts.is_empty())
        );
        if name == "big-batch" {
            // One resolution, then the moved-head check before synthesis and the
            // refactor writer, and the delivered-head check before the repairer.
            assert_eq!(owner.integration_pins, 4);
            let fix = run
                .checkpoint
                .nodes
                .iter()
                .find(|node| node.id == "repair--r1--fix")
                .unwrap();
            let review = run
                .checkpoint
                .nodes
                .iter()
                .find(|node| node.id == "repair--r1--review--subject")
                .unwrap();
            assert_eq!(review.source_commit, fix.outputs.as_ref().unwrap().commit);
            let council = run
                .checkpoint
                .nodes
                .iter()
                .find(|node| node.id == "council")
                .unwrap();
            let refactor = run
                .checkpoint
                .nodes
                .iter()
                .find(|node| node.id == "refactor")
                .unwrap();
            assert_eq!(
                refactor.source_commit,
                council.outputs.as_ref().unwrap().commit
            );
        }
        let attempts = owner.launched.clone();
        let replay =
            advance_with_budget(&document, &mut owner, &mut journal, &limits, &config.budget)
                .unwrap();
        assert_eq!(replay.checkpoint, run.checkpoint);
        assert_eq!(owner.launched, attempts);
    }
}

fn pin_fixture_sources(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            if map.get("type").is_some_and(|kind| kind == "commit")
                && map.get("commit").is_some_and(|commit| commit == "HEAD")
            {
                map.insert(
                    "commit".into(),
                    json!("0123456789abcdef0123456789abcdef01234567"),
                );
            }
            for value in map.values_mut() {
                pin_fixture_sources(value);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                pin_fixture_sources(value);
            }
        }
        _ => {}
    }
}

#[test]
fn read_only_worktree_bindings_lower_to_read_roots() {
    let expanded = expand(
        &Layers::empty(),
        "generator-evaluator",
        "approved brief",
        &["parameters.worktree='/repo/writer'".into()],
    )
    .unwrap();
    let nodes = &expanded.document["nodes"];
    assert_eq!(nodes[0]["worktree"], "/repo/writer");
    let lens = &nodes[1]["composite"]["nodes"][0];
    assert!(lens.get("worktree").is_none());
    assert_eq!(lens["read_roots"], json!(["/repo/writer"]));
    assert_eq!(
        nodes[1]["source"],
        json!({"type":"node","node":"implement"})
    );
}

#[test]
fn bundled_expansions_match_golden_graphs() {
    assert_eq!(
        nodes("single"),
        json!([
            {"id":"task", "title":"task", "access":"read_only", "prompt":"brief", "routes":[{"alias":"senior"},{"alias":"senior"}], "depends_on":[]}
        ])
    );
    assert_eq!(
        nodes("chain"),
        json!([
            {"id":"first", "title":"first", "access":"read_only", "prompt":"brief", "routes":[{"alias":"senior"},{"alias":"senior"}], "depends_on":[]},
            {"id":"second", "title":"second", "access":"read_only", "prompt":"brief", "routes":[{"alias":"senior"},{"alias":"senior"}], "depends_on":["first"], "inputs":[{"node":"first","outputs":["result"]}]}
        ])
    );
    assert_eq!(
        nodes("fan-out"),
        json!([
            {"id":"branch-1", "title":"branch-1", "access":"read_only", "prompt":"brief", "routes":[{"alias":"senior"},{"alias":"senior"}], "depends_on":[]},
            {"id":"branch-2", "title":"branch-2", "access":"read_only", "prompt":"brief", "routes":[{"alias":"senior"},{"alias":"senior"}], "depends_on":[]}
        ])
    );
    assert_eq!(
        nodes("fan-out-synthesise"),
        json!([
            {"id":"branch-1", "title":"branch-1", "access":"read_only", "prompt":"brief", "routes":[{"alias":"senior"},{"alias":"senior"}], "depends_on":[]},
            {"id":"branch-2", "title":"branch-2", "access":"read_only", "prompt":"brief", "routes":[{"alias":"senior"},{"alias":"senior"}], "depends_on":[]},
            {"id":"synthesis", "title":"synthesis", "kind":"synthesise", "access":"read_only", "prompt":"brief", "routes":[{"alias":"senior"},{"alias":"senior"}], "depends_on":["branch-1","branch-2"], "inputs":[{"node":"branch-1","outputs":["result"]},{"node":"branch-2","outputs":["result"]}]}
        ])
    );
    for name in ["single", "chain", "fan-out", "fan-out-synthesise"] {
        let first = expand(&Layers::empty(), name, "brief", &[]).unwrap();
        let second = expand(&Layers::empty(), name, "brief", &[]).unwrap();
        assert_eq!(first.document, second.document);
        let _: provenant_fabric_runtime::graph::GraphDocument =
            serde_json::from_value(first.document).unwrap();
    }
}

/// Compare a bundled expansion with its checked-in graph document.
/// `UPDATE_GOLDEN=1` rewrites the fixture.
fn expansion_golden(name: &str) {
    let expanded = expand(&Layers::empty(), name, "brief", &[]).unwrap();
    let mut actual = serde_json::to_string_pretty(&expanded.document).unwrap();
    actual.push('\n');
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("assets/presets/fixtures")
        .join(format!("{name}.expansion.json"));
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, &actual).unwrap();
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        actual, expected,
        "{name}: rerun with UPDATE_GOLDEN=1 to refresh"
    );
}

#[test]
fn composite_bundled_expansions_match_golden_graphs() {
    for name in [
        "scout-swarm",
        "generator-evaluator",
        "review-council",
        "big-batch",
        "scope-then-run",
        "deliver",
    ] {
        expansion_golden(name);
    }
}

#[test]
fn layering_composition_and_refusals() {
    let mut layers = Layers::empty();
    layers
        .insert("user", "single", "[parameters]\nroute = 'utility'\n")
        .unwrap();
    assert_eq!(
        layers.resolve("single").unwrap().1["parameters.route"],
        "user"
    );
    layers
        .insert("repo", "single", "[parameters]\nroute = 'senior'\n")
        .unwrap();
    assert_eq!(
        layers.resolve("single").unwrap().1["parameters.route"],
        "repo"
    );
    let result = expand(
        &layers,
        "single",
        "brief",
        &["parameters.route=\"fast\"".into()],
    )
    .unwrap();
    assert_eq!(route(&result.document["nodes"][0])["alias"], "fast");
    assert_eq!(result.origins["parameters.route"], "--set");
    layers
        .insert(
            "repo",
            "chain",
            "stages = [{ id = 'first', preset = 'single', parameters = { route = 'utility' } }, { id = 'second' }]\n",
        )
        .unwrap();
    let result = expand(&layers, "chain", "brief", &[]).unwrap();
    assert_eq!(result.document["nodes"][0]["id"], "first");
    assert_eq!(
        route(&result.document["nodes"][0]["composite"]["nodes"][0])["alias"],
        "utility"
    );
    let result = expand(&layers, "chain", "brief", &["first.route='fast'".into()]).unwrap();
    assert_eq!(
        route(&result.document["nodes"][0]["composite"]["nodes"][0])["alias"],
        "fast"
    );
    assert_eq!(result.origins["first.route"], "--set");
    // A leaf stage takes its own fields by stage id.
    let result = expand(&layers, "chain", "brief", &["second.tier='junior'".into()]).unwrap();
    assert_eq!(route(&result.document["nodes"][1])["alias"], "junior");
    assert!(
        expand(&layers, "single", "brief", &["parameters.nope=1".into()])
            .unwrap_err()
            .contains("unknown")
    );
    assert!(
        expand(&layers, "chain", "brief", &["nope.route='fast'".into()])
            .unwrap_err()
            .contains("unknown")
    );
    assert!(
        expand(&layers, "chain", "brief", &["first.nope=1".into()])
            .unwrap_err()
            .contains("unknown")
    );
    layers
        .insert(
            "repo",
            "single",
            "stages = [{ id = 'task', preset = 'single' }]\n",
        )
        .unwrap();
    assert!(
        expand(&layers, "single", "brief", &[])
            .unwrap_err()
            .contains("cycle")
    );
}

#[test]
fn named_presets_inherit_and_cycles_are_refused() {
    let mut layers = Layers::empty();
    layers
        .insert(
            "user",
            "mine",
            "extends = 'fan-out'\n[parameters]\nwidth = 3\n",
        )
        .unwrap();
    let expanded = expand(&layers, "mine", "brief", &[]).unwrap();
    assert_eq!(expanded.document["nodes"].as_array().unwrap().len(), 3);
    assert_eq!(expanded.origins["parameters.width"], "user");
    layers
        .insert("repo", "mine", "extends = 'other'\n")
        .unwrap();
    layers
        .insert("repo", "other", "extends = 'mine'\n")
        .unwrap();
    assert!(layers.resolve("mine").unwrap_err().contains("cycle"));
    let mut deep = Layers::empty();
    for index in 0..9 {
        let parent = if index == 0 {
            "single".to_owned()
        } else {
            format!("level-{}", index - 1)
        };
        deep.insert(
            "repo",
            &format!("level-{index}"),
            &format!("extends = '{parent}'\n"),
        )
        .unwrap();
    }
    assert!(deep.resolve("level-8").unwrap_err().contains("depth"));
}

#[test]
fn graph_publication_is_private_and_idempotent() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir()?;
    let graphs = temp.path().join("graphs");
    std::fs::create_dir(&graphs)?;
    std::fs::set_permissions(&graphs, std::fs::Permissions::from_mode(0o700))?;
    let graph = expand(&Layers::empty(), "single", "brief", &[])?.document;
    let first = super::fabric::publish_preset_graph(temp.path(), &graph)?;
    let second = super::fabric::publish_preset_graph(temp.path(), &graph)?;
    assert_eq!(first, second);
    assert_eq!(std::fs::read(&first)?, serde_json::to_vec(&graph)?);
    assert_eq!(std::fs::metadata(&first)?.permissions().mode() & 0o077, 0);
    std::fs::write(&first, b"different")?;
    assert!(super::fabric::publish_preset_graph(temp.path(), &graph).is_err());
    std::fs::write(&first, serde_json::to_vec(&graph)?)?;
    std::fs::set_permissions(&first, std::fs::Permissions::from_mode(0o644))?;
    assert!(super::fabric::publish_preset_graph(temp.path(), &graph).is_err());
    Ok(())
}

#[test]
fn graph_publication_repairs_owned_loose_directory() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir()?;
    let graphs = temp.path().join("graphs");
    std::fs::create_dir(&graphs)?;
    std::fs::set_permissions(&graphs, std::fs::Permissions::from_mode(0o755))?;
    let graph = expand(&Layers::empty(), "single", "brief", &[])?.document;
    let published = super::fabric::publish_preset_graph(temp.path(), &graph)?;
    assert_eq!(
        std::fs::metadata(&graphs)?.permissions().mode() & 0o777,
        0o700
    );
    assert!(published.starts_with(&graphs));
    assert_eq!(std::fs::read(&published)?, serde_json::to_vec(&graph)?);
    Ok(())
}

#[test]
fn preset_limits_and_composition_wiring() {
    let mut layers = Layers::empty();
    for width in [0, 33] {
        assert!(
            expand(
                &layers,
                "fan-out",
                "brief",
                &[format!("parameters.width={width}")]
            )
            .unwrap_err()
            .contains("width")
        );
    }
    layers
        .insert(
            "repo",
            "chain",
            "stages = [{ id = 'first', preset = 'fan-out' }, { id = 'second' }]\n",
        )
        .unwrap();
    let graph = expand(&layers, "chain", "brief", &[]).unwrap().document;
    assert_eq!(graph["nodes"][1]["depends_on"], json!(["first"]));
    assert_eq!(
        graph["nodes"][0]["composite"]["exports"],
        json!([
            {"node":"branch-1","outputs":["result","findings"]},
            {"node":"branch-2","outputs":["result","findings"]}
        ])
    );
    let mut large = Layers::empty();
    large
        .insert(
            "repo",
            "outer",
            "extends='fan-out'\nstages=[{ id = 'branch', count = '${width}', preset = 'inner' }]\n[parameters]\nwidth=32\n",
        )
        .unwrap();
    large
        .insert(
            "repo",
            "inner",
            "extends='fan-out'\n[parameters]\nwidth=32\n",
        )
        .unwrap();
    assert!(
        expand(&large, "outer", "brief", &[])
            .unwrap_err()
            .contains("512 nodes")
    );
    let mut deep = Layers::empty();
    for i in 0..9 {
        let stages = if i < 8 {
            format!("stages = [{{ id = 'task', preset = 'p{}' }}]\n", i + 1)
        } else {
            String::new()
        };
        deep.insert(
            "repo",
            &format!("p{i}"),
            &format!("extends='single'\n{stages}"),
        )
        .unwrap();
    }
    assert!(
        expand(&deep, "p0", "brief", &[])
            .unwrap_err()
            .contains("depth")
    );
    let long = "x".repeat(1_048_576);
    assert!(
        expand(
            &Layers::empty(),
            "fan-out",
            &long,
            &["parameters.width=9".into()]
        )
        .unwrap_err()
        .contains("8 MiB")
    );
}

#[test]
fn invalid_paths_and_toml_values_offer_help() {
    let layers = Layers::empty();
    let error = expand(&layers, "single", "brief", &["parameters.unknown=1".into()]).unwrap_err();
    assert!(
        error.contains("parameters.route") && error.contains("task.FIELD"),
        "{error}"
    );
    let error = expand(
        &layers,
        "single",
        "brief",
        &["parameters.route=utility".into()],
    )
    .unwrap_err();
    assert!(error.contains("parameters.route=\"utility\""), "{error}");
    assert!(expand(&layers, "single", "brief", &["parameters.route=[".into()]).is_err());
    let error = expand(
        &layers,
        "generator-evaluator",
        "brief",
        &["parameters.subject={type='commit', commit='abc'}".into()],
    )
    .unwrap_err();
    assert!(
        error.contains("parameters.subject.commit=\"<sha>\""),
        "{error}"
    );
}

#[test]
fn malformed_files_are_isolated_and_name_the_path() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let user = temp.path().join("user");
    let repo = temp.path().join("repo");
    std::fs::create_dir(&user)?;
    std::fs::create_dir(&repo)?;
    let bad = repo.join("broken.toml");
    std::fs::write(&bad, "[bad")?;
    std::fs::write(repo.join("single.toml"), "[parameters]\nroute='utility'\n")?;
    let layers = Layers::load(&user, &repo)?;
    assert!(expand(&layers, "single", "brief", &[]).is_ok());
    assert_eq!(
        expand(&layers, "single", "brief", &[])?.repo_files,
        vec![repo.join("single.toml").display().to_string()]
    );
    let listing = serde_json::to_string(&layers.listing())?;
    assert!(listing.contains("broken.toml"));
    assert!(
        layers
            .resolve("broken")
            .unwrap_err()
            .contains(&bad.to_string_lossy().to_string())
    );
    std::fs::write(
        repo.join("single.toml"),
        "[parameters.extra]\ntype='string'\n",
    )?;
    let layers = Layers::load(&user, &repo)?;
    let error = expand(&layers, "single", "", &[]).unwrap_err();
    assert!(
        error.contains("single")
            && error.contains("parameters.extra")
            && error.contains("single.toml"),
        "{error}"
    );
    Ok(())
}

#[test]
fn repository_notice_tracks_resolved_layers_in_order() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let user = temp.path().join("user");
    let repo = temp.path().join("repo");
    std::fs::create_dir(&user)?;
    std::fs::create_dir(&repo)?;
    std::fs::write(
        repo.join("base.toml"),
        "extends = 'single'\n[parameters]\nroute = 'utility'\n",
    )?;
    std::fs::write(
        repo.join("middle.toml"),
        "extends = 'base'\n[parameters]\nroute = 'senior'\n",
    )?;
    std::fs::write(repo.join("top.toml"), "extends = 'middle'\n")?;
    std::fs::write(user.join("top.toml"), "[parameters]\nroute = 'utility'\n")?;
    let layers = Layers::load(&user, &repo)?;
    let files = expand(
        &layers,
        "top",
        "brief",
        &["parameters.route='senior'".into()],
    )?
    .repo_files;
    assert_eq!(
        files,
        ["base", "middle", "top"]
            .map(|name| repo.join(format!("{name}.toml")).display().to_string())
    );
    Ok(())
}

#[test]
fn repository_notice_deduplicates_nested_presets() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo)?;
    std::fs::write(
        repo.join("chain.toml"),
        "stages = [{ id = 'first', preset = 'single' }, { id = 'second', preset = 'single' }]\n",
    )?;
    std::fs::write(
        repo.join("single.toml"),
        "[parameters]\nroute = 'utility'\n",
    )?;
    let layers = Layers::load(&temp.path().join("user"), &repo)?;
    assert_eq!(
        expand(&layers, "chain", "brief", &[])?.repo_files,
        ["chain", "single"].map(|name| repo.join(format!("{name}.toml")).display().to_string())
    );
    Ok(())
}

#[test]
fn read_roots_are_confined_to_the_project_or_the_host_allow_list()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let base = std::fs::canonicalize(temp.path())?;
    let project = base.join("project");
    let sibling = base.join("sibling");
    let shared = base.join("shared");
    for dir in [&project, &sibling, &shared, &project.join("src")] {
        std::fs::create_dir_all(dir)?;
    }
    std::os::unix::fs::symlink(&sibling, project.join("escape"))?;
    let strict = ReadRoots::new(vec![project.clone()], &[]);
    assert_eq!(strict.confine(&project.join("src"))?, project.join("src"));
    for outside in [
        sibling.clone(),
        project.join("../sibling"),
        project.join("escape"),
    ] {
        let error = strict.confine(&outside).unwrap_err();
        assert!(error.contains("allowed_read_roots"), "{error}");
    }
    let allowed = ReadRoots::new(
        vec![project.clone()],
        &[shared.to_string_lossy().into_owned()],
    );
    assert_eq!(allowed.confine(&shared)?, shared);
    assert!(allowed.confine(&sibling).is_err());
    Ok(())
}

/// Stand-in for the routing review configuration: low risk is one junior,
/// high risk is eight, otherwise five.
fn shapes() -> super::preset::ShapeSelector {
    Box::new(|query| {
        let (name, source, count) = match (query.shape, query.risk, query.kind) {
            (_, _, Some("integration")) => ("deep", "by_kind", 8),
            (Some("standard"), _, _) => ("standard", "override", 5),
            (_, Some("crucial"), _) => ("deep", "by_risk", 8),
            (_, Some("routine"), _) => ("quick", "by_risk", 1),
            _ => ("standard", "default", 5),
        };
        Ok(super::preset::ShapeHint {
            name: name.into(),
            source: source.into(),
            slots: vec!["junior".into(); count],
            plan: provenant_routing::ReviewConfig::default().lens_plan(query.kind, &[], false),
            ..Default::default()
        })
    })
}

fn council(overrides: &[&str], shaped: bool) -> Result<serde_json::Value, String> {
    let mut layers = Layers::empty();
    layers.review = shaped.then(shapes);
    let overrides: Vec<String> = overrides.iter().map(|s| (*s).to_owned()).collect();
    expand(&layers, "review-council", "the change", &overrides).map(|e| e.document)
}

#[test]
fn tests_lens_includes_mutation_testing_only_for_crucial_or_terminal_risk() {
    for (risk, expected) in [
        ("", false),
        ("routine", false),
        ("substantial", false),
        ("crucial", true),
        ("terminal", true),
    ] {
        let risk_override = format!("parameters.risk_tier='{risk}'");
        let document = council(
            &[
                "parameters.worktree='/repo/lane'",
                "parameters.kind='integration'",
                "parameters.lenses=[{name='tests',tier='',when=''},{name='correctness-spec',tier='',when=''}]",
                risk_override.as_str(),
            ],
            true,
        )
        .unwrap();
        let prompt = document["nodes"][1]["composite"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["id"] == "lens-tests")
            .unwrap()["prompt"]
            .as_str()
            .unwrap();
        assert_eq!(
            prompt.contains("Diff-scoped mutation evidence"),
            expected,
            "risk tier {risk}"
        );
        assert_eq!(
            prompt.contains("git diff --relative=<workspace> <base>...<PR head>"),
            expected,
            "mutation command at risk tier {risk}"
        );
        if expected {
            assert!(prompt.contains("never gates delivery"), "risk tier {risk}");
        }
        let correctness_prompt = document["nodes"][1]["composite"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["id"] == "lens-correctness-spec")
            .unwrap()["prompt"]
            .as_str()
            .unwrap();
        assert!(
            !correctness_prompt.contains("Diff-scoped mutation evidence"),
            "correctness lens at risk tier {risk}"
        );
    }
}

#[test]
fn preset_explain_big_batch_shows_targeted_checks_policy_and_origin() {
    let layers = Layers::empty();
    let explained = layers.explain("big-batch").unwrap();
    for role_name in [
        "write.implement",
        "repairer",
        "refactor-principal",
        "review.verify",
    ] {
        let prompt = explained["prompts"][role_name].as_str().expect(role_name);
        // Each prescribed limit is asserted on its own, so dropping any one
        // of them from the bundled policy fails here, not only in goldens.
        let flat = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
        for clause in [
            "Local check policy: run the tests you add or change",
            "the named tests covering changed functions, with a filter",
            "cargo nextest run -p CRATE -E 'test(NAME)'",
            "Run fmt, clippy for touched crates and generators for touched generated outputs",
            "Never run a whole package, `--workspace` or the full suite locally",
            "since CI runs those on the PR",
            "reviewers reproduce one finding with one targeted test",
        ] {
            assert!(
                flat.contains(clause),
                "role {role_name} prompt missing policy clause {clause:?}: {prompt}"
            );
        }
        assert!(
            prompt.contains("scripts/select-affected-crate-tests"),
            "role {role_name} missing affected tests script: {prompt}"
        );
        assert!(
            prompt.contains("scripts/find-changed-tests"),
            "role {role_name} missing find changed tests script: {prompt}"
        );
        assert_eq!(
            explained["local_check_policy"]["origin"], "bundled",
            "role {role_name} policy origin"
        );
    }
}

#[test]
fn preset_explain_big_batch_project_override_replaces_policy_text() {
    let mut layers = Layers::empty();
    layers
        .roles
        .add(
            ".provenant/roles.toml",
            "local_check_policy = 'Custom project policy: run targeted tests only.'\n",
        )
        .unwrap();
    let explained = layers.explain("big-batch").unwrap();
    for role_name in [
        "write.implement",
        "repairer",
        "refactor-principal",
        "review.verify",
    ] {
        let prompt = explained["prompts"][role_name].as_str().expect(role_name);
        assert!(
            prompt.contains("Custom project policy: run targeted tests only."),
            "role {role_name} missing overridden policy: {prompt}"
        );
        assert!(!prompt.contains("Local check policy: run the tests you add or change"));
        assert_eq!(
            explained["local_check_policy"]["origin"], ".provenant/roles.toml",
            "role {role_name} policy origin"
        );
    }
}

#[test]
fn preset_explain_big_batch_parameter_override_replaces_policy_text() {
    let layers = Layers::empty();
    let expanded = expand(
        &layers,
        "big-batch",
        "brief",
        &["parameters.local_check_policy='Preset param override: run targeted.'".into()],
    )
    .unwrap();
    assert_eq!(expanded.origins["parameters.local_check_policy"], "--set");
    for role_name in [
        "write.implement",
        "repairer",
        "refactor-principal",
        "review.verify",
        "lens.tests",
    ] {
        let prompt = find_prompt(&expanded.document["nodes"], role_name).expect(role_name);
        assert!(
            prompt.contains("Preset param override: run targeted."),
            "role {role_name} missing parameter overridden policy: {prompt}"
        );
        assert!(!prompt.contains("Local check policy: run the tests you add or change"));
    }
}

#[test]
fn preset_local_check_policy_parameter_propagates_through_compositions() {
    let layers = Layers::empty();
    for (preset, roles) in [
        (
            "generator-evaluator",
            vec!["write.implement", "lens.correctness-spec"],
        ),
        (
            "deliver",
            vec!["write.implement", "repairer", "review.verify", "lens.tests"],
        ),
        (
            "scope-then-run",
            vec!["write.implement", "lens.correctness-spec"],
        ),
        ("review-council", vec!["review.verify", "lens.tests"]),
    ] {
        let expanded = expand(
            &layers,
            preset,
            "brief",
            &["parameters.local_check_policy='Parameter policy only.'".into()],
        )
        .unwrap();
        assert_eq!(expanded.origins["parameters.local_check_policy"], "--set");
        for role in roles {
            let prompt = find_prompt(&expanded.document["nodes"], role).expect(role);
            assert!(
                prompt.contains("Parameter policy only."),
                "{preset}: {role}"
            );
            assert!(!prompt.contains("Local check policy: run the tests you add or change"));
        }
    }
}

#[test]
fn preset_local_check_policy_empty_parameter_uses_layered_roles() {
    let mut layers = Layers::empty();
    layers
        .roles
        .add("user/roles.toml", "local_check_policy = 'User policy.'")
        .unwrap();
    layers
        .roles
        .add(
            ".provenant/roles.toml",
            "local_check_policy = 'Project policy.'",
        )
        .unwrap();
    let expanded = expand(
        &layers,
        "big-batch",
        "brief",
        &["parameters.local_check_policy=''".into()],
    )
    .unwrap();
    assert_eq!(
        expanded.origins["parameters.local_check_policy"],
        ".provenant/roles.toml"
    );
    for role in [
        "write.implement",
        "repairer",
        "refactor-principal",
        "review.verify",
        "lens.tests",
    ] {
        let prompt = find_prompt(&expanded.document["nodes"], role).expect(role);
        assert!(prompt.contains("Project policy."), "{role}");
        assert!(!prompt.contains("User policy."));
    }
}

#[test]
fn preset_local_check_policy_nonempty_default_wins_over_roles_library() {
    let mut layers = Layers::empty();
    layers
        .roles
        .add(
            ".provenant/roles.toml",
            "local_check_policy = 'Project policy.'",
        )
        .unwrap();
    layers
        .insert(
            "user/presets/big-batch.toml",
            "big-batch",
            r#"
[parameters.local_check_policy]
default = "Preset default policy."
"#,
        )
        .unwrap();
    let explained = layers.explain("big-batch").unwrap();
    assert_eq!(
        explained["local_check_policy"]["origin"],
        "user/presets/big-batch.toml"
    );
    assert_eq!(
        explained["local_check_policy"]["prompt"],
        "Preset default policy."
    );
    for role in [
        "write.implement",
        "repairer",
        "refactor-principal",
        "review.verify",
    ] {
        assert!(
            explained["prompts"][role]
                .as_str()
                .unwrap()
                .contains("Preset default policy.")
        );
    }
}

fn lens_ids(document: &serde_json::Value) -> Vec<String> {
    document["nodes"][1]["composite"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn review_council_fans_out_one_lens_role_over_the_lens_list() {
    let document = council(&["parameters.worktree='/repo/lane'"], false).unwrap();
    assert_eq!(
        lens_ids(&document),
        [
            "lens-correctness-spec",
            "lens-tests",
            "lens-design-fit-reuse",
            "lens-simplicity",
            "lens-reliability",
            "lens-conventions-readability",
            "lens-refactor-cleanup",
            "lens-flexibility",
        ]
    );
    let lens = &document["nodes"][1]["composite"]["nodes"][0];
    assert_eq!(lens["distinct_from"], "writer");
    let prompt = lens["prompt"].as_str().unwrap();
    assert!(prompt.contains("read access to the whole repository"));
    assert!(prompt.contains("Lens: correctness-spec"));
    assert!(
        document["nodes"][1]["composite"]["nodes"][1]
            .get("distinct_from")
            .is_none()
    );
}

#[test]
fn lens_contract_rejects_two_verdict_lines() {
    let document = council(&["parameters.worktree='/repo/lane'"], false).unwrap();
    let lenses = document["nodes"][1]["composite"]["nodes"]
        .as_array()
        .unwrap();
    for lens in lenses {
        let prompt = lens["prompt"].as_str().unwrap();
        assert!(prompt.contains("```findings"), "{}: {prompt}", lens["id"]);
        assert!(
            prompt.contains("schema_version 1"),
            "{}: {prompt}",
            lens["id"]
        );
        assert!(
            prompt.contains("{\"schema_version\":1,\"findings\":[]}"),
            "{}: {prompt}",
            lens["id"]
        );
        assert!(
            prompt.contains(
                "\"id\":\"F1\",\"severity\":\"high\",\"file\":\"src/lib.rs\",\"line\":12"
            ),
            "{}: {prompt}",
            lens["id"]
        );
        assert!(prompt.contains("at most 32"), "{}: {prompt}", lens["id"]);
        assert!(
            prompt.contains("before the verdict line"),
            "{}: {prompt}",
            lens["id"]
        );
        assert!(
            prompt.contains("exactly one verdict line"),
            "{}: {prompt}",
            lens["id"]
        );
    }
    let mut capture = provenant_records::result_capture::ResultCapture::new(None);
    capture.push(b"VERDICT: changes abc123\nVERDICT: FAIL\n");
    let captured = capture.finish();
    assert!(captured.verdict.is_none());
    assert!(captured.verdict_invalid);
}

#[test]
fn integration_review_expands_all_base_lenses_and_resolves_roles() {
    let document = council(
        &[
            "parameters.worktree='/repo/lane'",
            "parameters.kind='integration'",
        ],
        true,
    )
    .unwrap();
    let nodes = document["nodes"][1]["composite"]["nodes"]
        .as_array()
        .unwrap();
    assert_eq!(nodes.len(), 8);
    for lens in ["refactor-cleanup", "reliability", "flexibility"] {
        let node = nodes
            .iter()
            .find(|node| node["id"] == format!("lens-{lens}"))
            .unwrap();
        assert!(
            node["prompt"]
                .as_str()
                .unwrap()
                .contains(&format!("Lens: {lens}"))
        );
        assert!(
            node["prompt"]
                .as_str()
                .unwrap()
                .contains("Canonical questions")
        );
    }
}

#[test]
fn docs_panel_keeps_risk_floor_with_readability_lens() {
    let document = council(
        &[
            "parameters.worktree='/repo/lane'",
            "parameters.kind='docs'",
            "parameters.risk_tier='crucial'",
        ],
        true,
    )
    .unwrap();
    assert_eq!(lens_ids(&document), ["lens-conventions-readability"]);
}

#[test]
fn refactor_panel_adds_kind_lenses_outside_the_slot_cap() {
    let document = council(
        &[
            "parameters.worktree='/repo/lane'",
            "parameters.kind='refactor'",
        ],
        true,
    )
    .unwrap();
    assert_eq!(
        lens_ids(&document),
        [
            "lens-correctness-spec",
            "lens-tests",
            "lens-design-fit-reuse",
            "lens-simplicity",
            "lens-reliability",
            "lens-refactor-cleanup"
        ]
    );
}

#[test]
fn a_caller_supplied_lens_list_keeps_its_order() {
    let document = council(
        &[
            "parameters.worktree='/repo/lane'",
            "parameters.kind='refactor'",
            "parameters.lenses=[{name='simplicity',tier='',when=''},{name='tests',tier='',when=''},{name='correctness-spec',tier='',when=''}]",
        ],
        true,
    )
    .unwrap();
    assert_eq!(
        lens_ids(&document),
        ["lens-simplicity", "lens-tests", "lens-correctness-spec"]
    );
}

#[test]
fn a_path_added_lens_survives_its_risk_tag_condition_without_duplicating() {
    let lenses = "parameters.lenses=[{name='correctness-spec',tier='',when=''},{name='tests',tier='',when='security'}]";
    for tags in ["[]", "['security']", "['other']"] {
        let mut layers = Layers::empty();
        layers.review = Some(Box::new(|_| {
            Ok(super::preset::ShapeHint {
                name: "quick".into(),
                source: "test".into(),
                slots: vec!["junior".into()],
                plan: provenant_routing::LensPlan {
                    add: vec!["tests".into()],
                    change_add: vec!["tests".into()],
                    ..Default::default()
                },
                ..Default::default()
            })
        }));
        let overrides = [
            "parameters.worktree='/repo/lane'".to_owned(),
            lenses.to_owned(),
            format!("parameters.risk_tags={tags}"),
        ];
        let document = expand(&layers, "review-council", "the change", &overrides)
            .unwrap()
            .document;
        assert_eq!(
            lens_ids(&document),
            ["lens-correctness-spec", "lens-tests"],
            "risk_tags={tags}"
        );
    }
}

#[test]
fn the_council_lens_triggers_match_the_shared_risk_tag_table() {
    let text = include_str!("../assets/presets/review-council.toml");
    for (tag, lens) in provenant_routing::RISK_TAG_LENSES {
        assert!(
            text.contains(&format!(
                "{{ name = \"{lens}\", tier = \"\", when = \"{tag}\" }}"
            )),
            "{lens} must trigger on {tag}"
        );
    }
}

#[test]
fn triggered_lenses_follow_the_risk_tags() {
    let document = council(
        &[
            "parameters.worktree='/repo/lane'",
            "parameters.risk_tags=['security','terminal']",
        ],
        false,
    )
    .unwrap();
    let ids = lens_ids(&document);
    assert!(ids.contains(&"lens-security-privacy".to_owned()));
    assert!(ids.contains(&"lens-adversarial".to_owned()));
    assert!(!ids.contains(&"lens-concurrency-data-errors".to_owned()));
}

#[test]
fn terminal_risk_tier_carries_the_terminal_tag() {
    let document = council(
        &[
            "parameters.worktree='/repo/lane'",
            "parameters.risk_tier='terminal'",
        ],
        false,
    )
    .unwrap();
    let ids = lens_ids(&document);
    assert!(ids.contains(&"lens-adversarial".to_owned()));
    assert!(!ids.contains(&"lens-security-privacy".to_owned()));
    let document = council(
        &[
            "parameters.worktree='/repo/lane'",
            "parameters.risk_tier='crucial'",
        ],
        false,
    )
    .unwrap();
    assert!(!lens_ids(&document).contains(&"lens-adversarial".to_owned()));
}

#[test]
fn lens_tiers_come_from_the_review_shape_unless_overridden() {
    let document = council(
        &[
            "parameters.worktree='/repo/lane'",
            "parameters.review_shape='standard'",
        ],
        true,
    )
    .unwrap();
    let nodes = document["nodes"][1]["composite"]["nodes"]
        .as_array()
        .unwrap();
    assert_eq!(nodes.len(), 5);
    assert!(nodes.iter().all(|node| route(node)["alias"] == "junior"));
    let document = council(
        &[
            "parameters.worktree='/repo/lane'",
            "parameters.review_shape='standard'",
            "parameters.lenses=[{name='tests',tier='principal',prompt_file='code-review/lenses/tests',distinct_from='',when=''}]",
        ],
        true,
    )
    .unwrap();
    let nodes = document["nodes"][1]["composite"]["nodes"]
        .as_array()
        .unwrap();
    assert_eq!(route(&nodes[0])["alias"], "principal");
    for (tier, expected) in [
        (
            "'codex/gpt-6@high'",
            json!({"adapter":"codex","model":"gpt-6","effort":"high"}),
        ),
        (
            "{alias='senior',effort='high'}",
            json!({"alias":"senior","effort":"high"}),
        ),
    ] {
        let lenses = format!("parameters.lenses=[{{name='tests',tier={tier},when=''}}]");
        let document = council(&["parameters.worktree='/repo/lane'", &lenses], true).unwrap();
        assert_eq!(
            *route(&document["nodes"][1]["composite"]["nodes"][0]),
            expected
        );
    }
}

fn lens_count(document: &serde_json::Value, council: &str) -> usize {
    let council = document["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["id"] == council)
        .unwrap();
    council["composite"]["nodes"][1]["composite"]["nodes"]
        .as_array()
        .unwrap()
        .len()
}

#[test]
fn each_review_stage_selects_its_shape_from_its_own_parameters() {
    let mut layers = Layers::empty();
    layers.review = Some(shapes());
    let batch = [
        "parameters.batch_worktree='/repo/batch'".to_owned(),
        "parameters.lanes=[{name='one',worktree='/repo/one',scope=['.'],spec=''}]".to_owned(),
    ];
    let plain = expand(&layers, "big-batch", "brief", &batch).unwrap();
    let standard = lens_count(&plain.document, "council");
    // A stage-level risk tier reaches that stage's shape selection.
    let mut high = batch.to_vec();
    high.push("council.risk_tier='crucial'".into());
    let high = expand(&layers, "big-batch", "brief", &high).unwrap();
    assert_eq!(standard, 8);
    assert_eq!(lens_count(&high.document, "council"), standard);
    assert!(high.review_shape.unwrap().contains("deep (by_kind)"));
    // A parent's ambient risk tier reaches a child that declares it.
    let mut layers = Layers::empty();
    layers.review = Some(shapes());
    layers
        .insert(
            "repo",
            "wrap",
            "stages = [{ id = 'council', preset = 'review-council', parameters = { worktree = '/repo/lane' } }]\n[parameters]\nrisk_tier = 'routine'\n",
        )
        .unwrap();
    let low = expand(&layers, "wrap", "brief", &[]).unwrap();
    assert_eq!(low.review_shape.as_deref(), Some("quick (by_risk)"));
}

#[test]
fn nested_set_paths_keep_the_rest_of_a_children_table_default() {
    let mut layers = Layers::empty();
    layers
        .insert(
            "repo",
            "wrap",
            "stages = [{ id = 'child', preset = 'generator-evaluator', parameters = { worktree = '/repo/lane' } }]\n",
        )
        .unwrap();
    let result = expand(
        &layers,
        "wrap",
        "brief",
        &["child.subject.commit='refs/heads/x'".into()],
    )
    .unwrap();
    let source = &result.document["nodes"][0]["composite"]["nodes"][0]["source"];
    assert_eq!(source["type"], "commit");
    assert_eq!(source["commit"], "refs/heads/x");
    let _: provenant_fabric_runtime::graph::GraphDocument =
        serde_json::from_value(result.document).unwrap();
}

#[test]
fn malformed_parameter_schemas_are_refused() {
    for (schema, expect) in [
        (
            "{ type = 'integer', default = 3, max = '2' }",
            "max must be a number",
        ),
        (
            "{ type = 'integer', default = 3, min = true }",
            "min must be a number",
        ),
        (
            "{ type = 'integer', default = 3, min = 5, max = 2 }",
            "min must not exceed max",
        ),
        (
            "{ type = 'string', default = 'a', enum = 'a' }",
            "enum must be a nonempty list",
        ),
        (
            "{ type = 'string', default = 'a', enum = [] }",
            "enum must be a nonempty list",
        ),
        ("{ type = 'integr', default = 'a' }", "type must be one of"),
        ("{ type = 3, default = 'a' }", "type must be one of"),
        (
            "{ type = 'boolean', default = true, max = 1 }",
            "apply to numbers",
        ),
    ] {
        let mut layers = Layers::empty();
        layers
            .insert(
                "repo",
                "bad",
                &format!("extends = 'single'\n[parameters]\nextra = {schema}\n"),
            )
            .unwrap();
        let error = expand(&layers, "bad", "brief", &[]).unwrap_err();
        assert!(
            error.contains("parameters.extra") && error.contains(expect),
            "{schema}: {error}"
        );
    }
}

#[test]
fn child_stage_errors_name_the_stage_path_and_origin() {
    let mut layers = Layers::empty();
    layers
        .insert(
            "repo",
            "wrap",
            "stages = [{ id = 'child', preset = 'fan-out' }]\n",
        )
        .unwrap();
    let error = expand(&layers, "wrap", "brief", &["child.width=0".into()]).unwrap_err();
    assert!(
        error.contains("child.width") && error.contains("(--set)"),
        "{error}"
    );
    layers
        .insert(
            "repo",
            "wrap2",
            "stages = [{ id = 'child', preset = 'fan-out', parameters = { width = 99 } }]\n",
        )
        .unwrap();
    let error = expand(&layers, "wrap2", "brief", &[]).unwrap_err();
    assert!(
        error.contains("child.width") && error.contains("(entry)"),
        "{error}"
    );
    let result = expand(&layers, "wrap", "brief", &["child.width=3".into()]).unwrap();
    assert_eq!(result.origins["expanded.child.parameters.width"], "--set");
}

fn council_with_roles(roles: &str) -> Result<Expansion, String> {
    let mut layers = Layers::empty();
    layers.roles.add("repo roles.toml", roles)?;
    expand(
        &layers,
        "review-council",
        "the change",
        &[
            "parameters.worktree='/repo/lane'".into(),
            "parameters.lenses=[{name='x',tier='senior',when=''}]".into(),
        ],
    )
}

#[test]
fn unknown_prompt_files_and_foreign_distinct_from_values_are_refused() {
    let error = council_with_roles(
        "[roles.\"lens.x\"]\nextends = 'lens'\nprompt_file = 'code-review/lenses/nope'\n",
    )
    .unwrap_err();
    assert!(error.contains("unknown reference"), "{error}");
    let error =
        council_with_roles("[roles.\"lens.x\"]\nextends = 'lens'\ndistinct_from = 'reviewer'\n")
            .unwrap_err();
    assert!(error.contains("distinct_from"), "{error}");
}

fn node<'a>(document: &'a Value, id: &str) -> &'a Value {
    fn find<'a>(nodes: &'a [Value], id: &str) -> Option<&'a Value> {
        for node in nodes {
            if node["id"] == id {
                return Some(node);
            }
            for path in ["/composite/nodes", "/loop/body"] {
                if let Some(found) = node
                    .pointer(path)
                    .and_then(Value::as_array)
                    .and_then(|children| find(children, id))
                {
                    return Some(found);
                }
            }
        }
        None
    }
    find(document["nodes"].as_array().unwrap(), id).unwrap_or_else(|| panic!("no node {id}"))
}

#[test]
fn a_repository_role_extends_a_library_role_and_reports_its_origin() {
    let expanded = council_with_roles(
        "[roles.\"lens.x\"]\nextends = 'lens.tests'\ncard = 'House lens for x.'\n",
    )
    .unwrap();
    let lens = node(&expanded.document, "lens-x");
    assert_eq!(lens["role"], "lens.x");
    let prompt = lens["prompt"].as_str().unwrap();
    assert!(prompt.starts_with("House lens for x."), "{prompt}");
    // The parent's references carry over.
    assert!(prompt.contains("# Reviewer context"));
    assert_eq!(expanded.origins["roles.lens.x"], "repo roles.toml");
    assert_eq!(expanded.origins["roles.review.synthesis"], "bundled");
    assert_eq!(
        expanded.roles["lens.x"]["extends"],
        json!(["lens.x", "lens.tests", "lens"])
    );
}

#[test]
fn an_unknown_dotted_role_falls_back_to_its_family_and_an_unknown_role_is_refused() {
    let expanded = council_with_roles("").unwrap();
    // `lens.x` is not in the library, so it uses the generic `lens` role.
    assert_eq!(node(&expanded.document, "lens-x")["role"], "lens.x");
    let mut layers = Layers::empty();
    layers
        .insert("test", "bad", "stages = [{ id = 'a', role = 'nope' }]\n")
        .unwrap();
    let error = expand(&layers, "bad", "brief", &[]).unwrap_err();
    assert!(error.contains("unknown preset role: nope"), "{error}");
}

fn skill_root(files: &[(&str, &str)]) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "provenant-skill-root-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    for (path, text) in files {
        let path = root.join("skills").join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    root
}

#[test]
fn editing_a_skill_reference_changes_the_recorded_digest() {
    let root = skill_root(&[
        ("scope/references/researcher.md", "Look outward.\n"),
        ("scope/references/scout-areas.md", "Areas.\n"),
    ]);
    let roles = "[roles.\"scout.external\"]\ncard = 'x'\n";
    let digest = |text: &str| {
        std::fs::write(root.join("skills/scope/references/researcher.md"), text).unwrap();
        let mut layers = Layers::empty();
        layers.roles.skill_root = Some(root.clone());
        layers.roles.add("repo", roles).unwrap();
        let expanded = expand(&layers, "scout-swarm", "brief", &[]).unwrap();
        let used = expanded.roles["scout.external"].clone();
        let reference = used["references"][0].clone();
        assert_eq!(reference["ref"], "skill:scope/references/researcher.md");
        (
            reference["digest"].clone(),
            used["digest"].clone(),
            expanded,
        )
    };
    let (first, role_first, _) = digest("Look outward.\n");
    let (second, role_second, expanded) = digest("Look outward, then inward.\n");
    assert_ne!(first, second);
    assert_ne!(role_first, role_second);
    // The disk text, not the compiled copy, reaches the brief.
    assert!(
        node(&expanded.document, "external")["prompt"]
            .as_str()
            .unwrap()
            .contains("then inward")
    );
}

#[test]
fn unknown_and_out_of_root_references_are_refused() {
    let root = skill_root(&[("scope/references/real.md", "real\n")]);
    std::fs::write(root.join("outside.md"), "secret\n").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        root.join("outside.md"),
        root.join("skills/scope/references/link.md"),
    )
    .unwrap();
    let mut library = crate::preset_roles::RoleLibrary::bundled();
    library.skill_root = Some(root);
    assert!(library.reference("skill:scope/references/real.md").is_ok());
    for bad in [
        "skill:scope/references/missing.md",
        "skill:scope/../../outside.md",
        "skill:/etc/passwd",
        "skill:scope",
        "scope/references/real.md",
        "skill:scope/references/link.md",
    ] {
        assert!(library.reference(bad).is_err(), "{bad}");
    }
}

#[test]
fn the_external_scout_falls_back_and_records_a_substitution_without_a_web_route() {
    let mut layers = Layers::empty();
    layers.known_routes = Some(["senior".to_owned()].into());
    let expanded = expand(&layers, "scout-swarm", "brief", &[]).unwrap();
    let scout = node(&expanded.document, "external");
    assert_eq!(route(scout)["alias"], "junior");
    assert_eq!(expanded.substitutions.len(), 1);
    assert_eq!(expanded.substitutions[0]["requested"], "web");
    layers.known_routes = Some(["web".to_owned()].into());
    let expanded = expand(&layers, "scout-swarm", "brief", &[]).unwrap();
    assert_eq!(route(node(&expanded.document, "external"))["alias"], "web");
    assert!(expanded.substitutions.is_empty());
    let prompt = node(&expanded.document, "external")["prompt"]
        .as_str()
        .unwrap();
    assert!(prompt.contains("URL"));
}

#[test]
fn opting_out_of_external_scouts_is_an_explicit_recorded_parameter() {
    let layers = Layers::empty();
    let default = expand(&layers, "scout-swarm", "brief", &[]).unwrap();
    assert_eq!(default.origins.get("parameters.external"), None);
    let opted = expand(
        &layers,
        "scout-swarm",
        "brief",
        &["parameters.external=0".into()],
    )
    .unwrap();
    assert_eq!(opted.origins["parameters.external"], "--set");
    assert!(!opted.document.to_string().contains("\"id\":\"external\""));
    // The synthesis still follows the codebase scouts.
    assert!(opted.document.to_string().contains("scouts"));
}

#[test]
fn review_council_verifies_between_the_lenses_and_the_synthesis() {
    let expanded = expand(
        &Layers::empty(),
        "review-council",
        "the change",
        &["parameters.worktree='/repo/lane'".into()],
    )
    .unwrap();
    let ids: Vec<&str> = expanded.document["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["subject", "lenses", "verify", "synthesis"]);
    assert_eq!(
        node(&expanded.document, "lenses")["composite"]["min_successful"],
        1
    );
    assert!(
        node(&expanded.document, "lenses")["composite"]["nodes"][0]["require_findings"] == true
    );
    let verify = node(&expanded.document, "verify");
    assert_eq!(verify["distinct_from"], "dependencies");
    assert_eq!(verify["source"], json!({"type": "node", "node": "subject"}));
    assert_eq!(route(verify)["alias"], "verifier");
    assert_eq!(verify["depends_on"], json!(["lenses", "subject"]));
    let synthesis = node(&expanded.document, "synthesis");
    assert_eq!(
        synthesis["depends_on"],
        json!(["verify", "lenses", "subject"])
    );
    assert_eq!(
        synthesis["inputs"],
        json!([
            {"node": "verify", "outputs": ["result", "findings"]},
            {"node": "lenses", "outputs": ["result", "findings"]},
            {"node": "subject", "outputs": ["findings"]}
        ])
    );
    assert!(
        verify["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|input| input["node"] == "lenses")
    );
    assert!(
        synthesis["prompt"]
            .as_str()
            .unwrap()
            .contains("disposition")
    );
    let skipped = expand(
        &Layers::empty(),
        "review-council",
        "the change",
        &[
            "parameters.worktree='/repo/lane'".into(),
            "parameters.verifiers=0".into(),
        ],
    )
    .unwrap();
    let synthesis = node(&skipped.document, "synthesis");
    assert_eq!(synthesis["depends_on"], json!(["lenses", "subject"]));
    assert!(
        synthesis["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|input| input["node"] != "verify")
    );
    assert!(!skipped.document.to_string().contains("\"id\":\"verify\""));
}

#[test]
fn the_verifier_route_falls_back_to_senior_when_undefined() {
    let mut layers = Layers::empty();
    layers.known_routes = Some(["senior".to_owned()].into());
    let expanded = expand(
        &layers,
        "review-council",
        "the change",
        &["parameters.worktree='/repo/lane'".into()],
    )
    .unwrap();
    assert_eq!(route(node(&expanded.document, "verify"))["alias"], "senior");
    assert_eq!(expanded.substitutions[0]["node"], "verify");
}

#[test]
fn generator_evaluator_ends_with_an_evaluator_over_the_lens_list() {
    let expanded = expand(
        &Layers::empty(),
        "generator-evaluator",
        "the slice",
        &[
            "parameters.worktree='/repo/lane'".into(),
            "parameters.evaluator='principal'".into(),
            "parameters.base='main'".into(),
        ],
    )
    .unwrap();
    assert_eq!(
        node(&expanded.document, "lens")["composite"]["min_successful"],
        1
    );
    let override_expanded = expand(
        &Layers::empty(),
        "generator-evaluator",
        "the slice",
        &[
            "parameters.worktree='/repo/lane'".into(),
            "parameters.min_lenses=2".into(),
        ],
    )
    .unwrap();
    assert_eq!(
        node(&override_expanded.document, "lens")["composite"]["min_successful"],
        2
    );
    let evaluate = node(&expanded.document, "evaluate");
    assert_eq!(evaluate["kind"], "synthesise");
    assert_eq!(
        evaluate["inputs"][0],
        json!({"node":"lens", "outputs":["findings"]})
    );
    assert_eq!(route(evaluate)["alias"], "principal");
    assert_eq!(evaluate["source"]["node"], "implement");
    assert_eq!(evaluate["review_base"], "main");
    let mut fixture = expanded.document.clone();
    pin_fixture_sources(&mut fixture);
    let document: provenant_fabric_runtime::graph::GraphDocument =
        serde_json::from_value(fixture).unwrap();
    let graph = document
        .expanded(&provenant_fabric_runtime::config::GraphLimits::default())
        .unwrap();
    let lenses: Vec<_> = graph
        .nodes
        .iter()
        .filter(|node| {
            node.role
                .as_deref()
                .is_some_and(|role| role.starts_with("lens."))
        })
        .collect();
    assert!(!lenses.is_empty());
    for lens in lenses {
        assert_eq!(lens.extra.get("review_base"), Some(&json!("main")));
        assert!(
            matches!(&lens.source, Some(provenant_fabric_runtime::graph::SourceBinding::Node { node }) if node == "implement")
        );
    }
    assert!(
        evaluate["prompt"]
            .as_str()
            .unwrap()
            .contains("accepted, rejected or deferred")
    );
}

#[test]
fn roles_view_lists_the_library_and_one_role() {
    let layers = Layers::empty();
    let all = layers.roles_view(None).unwrap();
    let names: Vec<&str> = all["roles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|role| role["name"].as_str().unwrap())
        .collect();
    for expected in [
        "plan.scope",
        "plan.slices",
        "scout.codebase",
        "scout.external",
        "lens.tests",
        "review.verify",
    ] {
        assert!(names.contains(&expected), "{expected}");
    }
    let one = layers.roles_view(Some("plan.scope")).unwrap();
    assert_eq!(one["extends"], json!(["plan"]));
    assert_eq!(one["origin"], "bundled");
    assert!(
        one["references"][0]["digest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    let tests = layers.roles_view(Some("lens.tests")).unwrap();
    let refs: Vec<_> = tests["references"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|reference| reference["ref"].as_str())
        .collect();
    assert!(refs.contains(&"skill:test-design/references/judgement.md"));
    assert!(refs.contains(&"skill:test-design/references/boundaries.md"));
}

#[test]
fn big_batch_fans_out_one_lane_per_list_entry() {
    let lanes = "parameters.lanes=[{name='a',worktree='/repo/a',scope=['x'],spec='#1'},{name='b',worktree='/repo/b',scope=['y'],spec='#2'},{name='c',worktree='/repo/c',scope=['z'],spec='#3'}]";
    let expanded = expand(
        &Layers::empty(),
        "big-batch",
        "the batch",
        &[
            lanes.into(),
            "parameters.batch_worktree='/repo/batch'".into(),
        ],
    )
    .unwrap();
    let nodes = expanded.document["nodes"].as_array().unwrap();
    let group = nodes.iter().find(|node| node["id"] == "lanes").unwrap();
    let lanes = group["composite"]["nodes"].as_array().unwrap();
    assert_eq!(lanes.len(), 3);
    for lane in ["lane-a", "lane-b", "lane-c"] {
        assert!(lanes.iter().any(|node| node["id"] == lane), "{lane}");
    }
}

const CATALOGUE: [&str; 9] = [
    "single",
    "chain",
    "fan-out",
    "fan-out-synthesise",
    "review-council",
    "generator-evaluator",
    "scout-swarm",
    "big-batch",
    "scope-then-run",
];

#[test]
fn typed_parameters_validate_and_name_the_parameter_and_origin() {
    let mut layers = Layers::empty();
    for (set, expect) in [
        ("parameters.width=0", "must be 1 to 32"),
        ("parameters.width=33", "must be 1 to 32"),
        ("parameters.width='two'", "must be integer"),
    ] {
        let error = expand(&layers, "fan-out", "brief", &[set.into()]).unwrap_err();
        assert!(
            error.contains("parameters.width") && error.contains("--set") && error.contains(expect),
            "{error}"
        );
    }
    layers
        .insert(
            "user",
            "tight",
            "extends = 'fan-out'\n[parameters.route]\nenum = ['senior', 'junior']\n[parameters]\nwidth = 3\n",
        )
        .unwrap();
    assert_eq!(
        expand(&layers, "tight", "brief", &[]).unwrap().document["nodes"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let error = expand(
        &layers,
        "tight",
        "brief",
        &["parameters.route='fast'".into()],
    )
    .unwrap_err();
    assert!(
        error.contains("parameters.route") && error.contains("one of"),
        "{error}"
    );
    let error = expand(&layers, "tight", "brief", &["parameters.width=40".into()]).unwrap_err();
    assert!(error.contains("--set"), "{error}");
    layers
        .insert(
            "user",
            "bad",
            "extends = 'single'\n[parameters.extra]\ntype = 'string'\n",
        )
        .unwrap();
    assert!(
        expand(&layers, "bad", "brief", &[])
            .unwrap_err()
            .contains("required parameter is missing")
    );
}

#[test]
fn a_bundled_override_may_add_parameters() {
    let mut layers = Layers::empty();
    layers
        .insert(
            "repo",
            "fan-out",
            "[parameters]\nwidth = 3\nextra = 'kept'\n",
        )
        .unwrap();
    let result = expand(&layers, "fan-out", "brief", &[]).unwrap();
    assert_eq!(result.document["nodes"].as_array().unwrap().len(), 3);
    assert_eq!(result.origins["parameters.extra"], "repo");
}

#[test]
fn stage_ids_address_set_paths_on_leaves_and_presets() {
    let layers = Layers::empty();
    let result = expand(
        &layers,
        "fan-out-synthesise",
        "brief",
        &["synthesis.tier='principal'".into()],
    )
    .unwrap();
    assert_eq!(route(&result.document["nodes"][2])["alias"], "principal");
    // Default ids address unnamed stages by position.
    let mut layers = Layers::empty();
    layers
        .insert(
            "repo",
            "pair",
            "stages = [{ preset = 'single' }, { preset = 'single' }]\n",
        )
        .unwrap();
    let result = expand(&layers, "pair", "brief", &["s2.route='fast'".into()]).unwrap();
    assert_eq!(
        route(&result.document["nodes"][1]["composite"]["nodes"][0])["alias"],
        "fast"
    );
    let error = expand(&layers, "pair", "brief", &["s1.id='x'".into()]).unwrap_err();
    assert!(error.contains("cannot be overridden"), "{error}");
    // Nested parameter paths reach into table values.
    let result = expand(
        &Layers::empty(),
        "generator-evaluator",
        "brief",
        &["parameters.subject.commit='refs/heads/x'".into()],
    )
    .unwrap();
    assert_eq!(
        result.document["nodes"][0]["source"]["commit"],
        "refs/heads/x"
    );
}

#[test]
fn ambient_subject_and_risk_tier_flow_to_children_that_declare_them() {
    let mut layers = Layers::empty();
    layers
        .insert(
            "repo",
            "wrap",
            "stages = [{ id = 'council', preset = 'review-council', parameters = { worktree = '/repo/lane' } }]\n[parameters]\nsubject = { type = 'integration' }\nrisk_tier = 'crucial'\n",
        )
        .unwrap();
    let result = expand(&layers, "wrap", "brief", &[]).unwrap();
    let subject = &result.document["nodes"][0]["composite"]["nodes"][0];
    assert_eq!(subject["source"], json!({"type":"integration"}));
    // An explicit entry value wins over the ambient one.
    layers
        .insert(
            "repo",
            "wrap",
            "stages = [{ id = 'council', preset = 'review-council', parameters = { worktree = '/repo/lane', subject = { type = 'commit', commit = 'HEAD' } } }]\n",
        )
        .unwrap();
    let result = expand(&layers, "wrap", "brief", &[]).unwrap();
    assert_eq!(
        result.document["nodes"][0]["composite"]["nodes"][0]["source"]["type"],
        "commit"
    );
}

#[test]
fn every_catalogue_preset_has_a_pattern_and_an_explanation() {
    let layers = Layers::empty();
    for name in CATALOGUE {
        let explained = layers
            .explain(name)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let pattern = &explained["pattern"];
        assert!(
            ["default", "recommended", "experimental"]
                .contains(&pattern["level"].as_str().unwrap_or_default()),
            "{name}: {pattern}"
        );
        for key in ["summary", "cost", "select_when"] {
            assert!(
                pattern[key].as_str().is_some_and(|v| !v.is_empty()),
                "{name}.{key}"
            );
        }
        for parameter in explained["parameters"].as_array().unwrap() {
            if name != "big-batch" && parameter["name"] != "lenses" {
                assert!(parameter["doc"].is_string(), "{name}: {parameter}");
            }
        }
        assert!(explained["next"].as_array().is_some_and(|n| !n.is_empty()));
    }
    let explained = layers.explain("fan-out").unwrap();
    assert_eq!(
        explained["parameters"],
        json!([
            {"name":"distinct_families","type":"boolean","default":false,"doc":"Prefer different model families across the branches. A preference only: every branch still runs.","origin":"bundled"},
            {"name":"route","type":"string","default":"senior","doc":"Routing pool for every branch.","origin":"bundled"},
            {"name":"width","type":"integer","default":2,"min":1,"max":32,"doc":"Number of parallel branches.","origin":"bundled"}
        ])
    );
    assert_eq!(
        explained["stages"],
        json!([{"id":"branch","count":"${width}"}])
    );
    let listing = layers.listing();
    let single = listing
        .iter()
        .find(|entry| entry["name"] == "single")
        .unwrap();
    assert_eq!(single["level"], "default");
    assert!(layers.explain("nope").is_err());
}

#[test]
fn unknown_pattern_fields_and_levels_are_refused() {
    let mut layers = Layers::empty();
    layers
        .insert("repo", "single", "[pattern]\nlevel = 'famous'\n")
        .unwrap();
    assert!(
        expand(&layers, "single", "brief", &[])
            .unwrap_err()
            .contains("pattern.level")
    );
    let mut layers = Layers::empty();
    layers
        .insert("repo", "single", "[pattern]\nweather = 'fine'\n")
        .unwrap();
    assert!(
        expand(&layers, "single", "brief", &[])
            .unwrap_err()
            .contains("unknown field")
    );
}

#[test]
fn a_blocked_scope_launches_no_writer() {
    use provenant_fabric_runtime::graph::advance_with_budget;
    let document = bundled_fixture("scope-then-run");
    let config = provenant_fabric_runtime::config::GraphConfig::default();
    let mut owner = PresetOwner {
        failing: vec!["scope".into()],
        ..Default::default()
    };
    let mut journal = PresetJournal::default();
    let _ = advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &config.limits,
        &config.budget,
    );
    assert!(
        owner.launched.iter().any(|id| id == "scope"),
        "{:?}",
        owner.launched
    );
    assert!(
        owner.launched.iter().all(|id| !id.starts_with("run")),
        "{:?}",
        owner.launched
    );
}

#[test]
fn the_writer_receives_the_scoped_criteria_once_the_scope_is_ready() {
    use provenant_fabric_runtime::graph::advance_with_budget;
    let document = bundled_fixture("scope-then-run");
    let config = provenant_fabric_runtime::config::GraphConfig::default();
    let mut owner = PresetOwner::default();
    let mut journal = PresetJournal::default();
    advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &config.limits,
        &config.budget,
    )
    .unwrap();
    let writer = owner
        .prompts
        .iter()
        .find(|(id, _)| id.starts_with("run--implement"))
        .unwrap_or_else(|| panic!("{:?}", owner.launched));
    assert!(
        writer.1.contains("Evidence checked for scope."),
        "{}",
        writer.1
    );
}

#[test]
fn set_paths_on_a_preset_stage_apply_in_command_line_order() {
    let mut layers = Layers::empty();
    layers
        .insert(
            "repo",
            "wrap",
            "stages = [{ id = 'child', preset = 'fan-out' }]\n",
        )
        .unwrap();
    let count = |sets: &[&str]| {
        let sets: Vec<String> = sets.iter().map(|s| (*s).to_owned()).collect();
        expand(&layers, "wrap", "brief", &sets).unwrap().document["nodes"][0]["composite"]["nodes"]
            .as_array()
            .unwrap()
            .len()
    };
    assert_eq!(count(&["child.width.default=1", "child.width=3"]), 3);
    assert_eq!(count(&["child.width=3", "child.width.default=1"]), 1);
}

#[test]
fn nested_diagnostics_qualify_only_the_path_and_keep_real_origins() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    std::fs::write(
        repo.join("inner.toml"),
        "extends = 'single'\n[pattern]\nlevel = 'famous'\n[parameters.route]\nenum = ['a']\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("middle.toml"),
        "stages = [{ id = 'leaf', preset = 'inner' }]\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("outer.toml"),
        "stages = [{ id = 'child', preset = 'middle' }]\n",
    )
    .unwrap();
    let layers = Layers::load(&temp.path().join("user"), &repo).unwrap();
    let error = expand(&layers, "outer", "brief", &[]).unwrap_err();
    let file = repo.join("inner.toml").display().to_string();
    assert!(
        error.contains("child.leaf.pattern.level") && error.contains(&format!("({file})")),
        "{error}"
    );
    std::fs::write(
        repo.join("inner.toml"),
        "extends = 'single'\n[parameters.route]\nenum = ['a']\ndefault = 'b'\n",
    )
    .unwrap();
    let layers = Layers::load(&temp.path().join("user"), &repo).unwrap();
    let error = expand(&layers, "outer", "brief", &[]).unwrap_err();
    assert!(
        error.contains("child.leaf.route") && error.contains(&format!("({file})")),
        "{error}"
    );
}

#[test]
fn composite_dependencies_and_inputs_take_parameters() {
    let mut layers = Layers::empty();
    layers
        .insert(
            "repo",
            "after",
            "stages = [{ id = 'scope' }, { id = 'run', preset = 'single', depends_on = ['${upstream}'], inputs = [{ node = '${upstream}', outputs = ['result'] }] }]\n[parameters]\nupstream = 'scope'\n",
        )
        .unwrap();
    let result = expand(&layers, "after", "brief", &[]).unwrap();
    let run = &result.document["nodes"][1];
    assert_eq!(run["depends_on"], json!(["scope"]));
    assert_eq!(run["inputs"][0]["node"], "scope");
    let _: provenant_fabric_runtime::graph::GraphDocument =
        serde_json::from_value(result.document).unwrap();
}

#[test]
fn bundled_presets_fit_the_default_host_depth_limit() {
    // The documents' own `limits` are ignored by the host, so check against
    // the default host limits only.
    let limits = provenant_fabric_runtime::config::GraphConfig::default().limits;
    for name in ["big-batch", "scope-then-run", "review-council"] {
        let document = bundled_fixture(name);
        let inspection = document.inspect(&limits).unwrap();
        assert_eq!(inspection.limit_reason, None, "{name}");
    }
}

#[test]
fn cross_family_prefers_by_default_and_a_project_may_require_it() {
    let expanded = expand(
        &Layers::empty(),
        "review-council",
        "the change",
        &["parameters.worktree='/repo/lane'".into()],
    )
    .unwrap();
    let verify = node(&expanded.document, "verify");
    assert_eq!(verify["distinct_from"], "dependencies");
    assert!(verify.get("distinct_strength").is_none());
    assert!(verify.get("family").is_none());
    let required =
        council_with_roles("[roles.\"review.verify\"]\nfamily = 'cross_required'\n").unwrap();
    let verify = node(&required.document, "verify");
    assert_eq!(verify["distinct_from"], "dependencies");
    assert_eq!(verify["distinct_strength"], "require");
    let strict =
        council_with_roles("[roles.\"review.verify\"]\nfamily_strength = 'require'\n").unwrap();
    assert_eq!(
        node(&strict.document, "verify")["distinct_strength"],
        "require"
    );
    let error =
        council_with_roles("[roles.\"review.verify\"]\nfamily_strength = 'maybe'\n").unwrap_err();
    assert!(error.contains("family_strength"), "{error}");
}

#[test]
fn role_digest_covers_the_whole_effective_profile() {
    let digest = |roles: &str| {
        let mut layers = Layers::empty();
        layers.roles.add("repo", roles).unwrap();
        let expanded = expand(&layers, "scout-swarm", "brief", &[]).unwrap();
        expanded.roles["scout.codebase"]["digest"].clone()
    };
    let base = digest("");
    for change in [
        "prompt = 'different'",
        "tier = 'senior'",
        "access = 'worktree_write'",
        "kind = 'synthesise'",
        "outputs = ['result', 'commit']",
        "references = [{ ref = 'skill:scope/references/scout-areas.md', when = 'always', inline = false }]",
    ] {
        let changed = digest(&format!("[roles.\"scout.codebase\"]\n{change}\n"));
        assert_ne!(base, changed, "{change}");
    }
}

#[test]
fn embedded_references_are_inlined_when_there_is_no_skills_root() {
    let expanded = expand(
        &Layers::empty(),
        "scope-then-run",
        "brief",
        &["parameters.worktree='/repo/lane'".into()],
    )
    .unwrap();
    let scope = node(&expanded.document, "scope");
    let prompt = scope["prompt"].as_str().unwrap();
    // architect.md and critic.md are listed, not inline, in the role.
    assert!(prompt.contains("# Architect"), "{prompt}");
    assert!(prompt.contains("# Critic"), "{prompt}");
}

#[test]
fn substitutions_and_origins_travel_in_the_retained_graph_document() {
    let mut layers = Layers::empty();
    layers.known_routes = Some(["senior".to_owned()].into());
    let expanded = expand(
        &layers,
        "scout-swarm",
        "brief",
        &["parameters.external=2".into()],
    )
    .unwrap();
    let provenance = &expanded.document["provenance"];
    assert_eq!(provenance["substitutions"][0]["requested"], "web");
    assert_eq!(provenance["origins"]["parameters.external"], "--set");
    assert!(provenance["roles"]["scout.external"]["digest"].is_string());
    // It survives a typed round trip, as retention does.
    let typed: provenant_fabric_runtime::graph::GraphDocument =
        serde_json::from_value(expanded.document.clone()).unwrap();
    let again = serde_json::to_value(&typed).unwrap();
    assert_eq!(again["provenance"], *provenance);
}

#[test]
fn synthesis_selects_findings_from_implicit_and_explicit_dependencies() {
    let mut layers = Layers::empty();
    layers.insert("test", "mixed-dependencies", r#"
stages = [
  { id = "first", prompt = "First" },
  { id = "second", prompt = "Second" },
  { id = "synthesis", prompt = "Synthesise", depends_on = ["first"], dispositions = ["accepted", "rejected"] },
]
"#).unwrap();
    let expanded = expand(&layers, "mixed-dependencies", "brief", &[]).unwrap();
    let synthesis = node(&expanded.document, "synthesis");
    assert_eq!(synthesis["depends_on"], json!(["second", "first"]));
    assert_eq!(
        synthesis["inputs"],
        json!([
            {"node": "second", "outputs": ["result", "findings"]},
            {"node": "first", "outputs": ["result", "findings"]}
        ])
    );
    layers.insert("test", "custom-inputs", r#"
stages = [
  { id = "first", prompt = "First" },
  { id = "second", prompt = "Second" },
  { id = "synthesis", prompt = "Synthesise", depends_on = ["first"], dispositions = ["accepted"], inputs = [{ node = "first", outputs = ["result", "findings"] }, { node = "second", outputs = ["result"] }] },
]
"#).unwrap();
    let expanded = expand(&layers, "custom-inputs", "brief", &[]).unwrap();
    assert_eq!(
        node(&expanded.document, "synthesis")["inputs"],
        json!([
            {"node": "first", "outputs": ["result", "findings"]},
            {"node": "second", "outputs": ["result", "findings"]}
        ])
    );

    layers.insert("test", "missing-input", r#"
stages = [
  { id = "first", prompt = "First" },
  { id = "second", prompt = "Second" },
  { id = "synthesis", prompt = "Synthesise", depends_on = ["first"], dispositions = ["accepted"], inputs = [{ node = "first", outputs = ["result"] }] },
]
"#).unwrap();
    let expanded = expand(&layers, "missing-input", "brief", &[]).unwrap();
    assert_eq!(
        node(&expanded.document, "synthesis")["inputs"],
        json!([
            {"node": "first", "outputs": ["result", "findings"]},
            {"node": "second", "outputs": ["findings"]}
        ])
    );
}

#[test]
fn synthesis_declares_and_requests_a_disposition_for_every_finding() {
    let expanded = expand(
        &Layers::empty(),
        "review-council",
        "the change",
        &["parameters.worktree='/repo/lane'".into()],
    )
    .unwrap();
    let synthesis = node(&expanded.document, "synthesis");
    assert_eq!(
        synthesis["dispositions"],
        json!(["accepted", "rejected", "deferred"])
    );
    let prompt = synthesis["prompt"].as_str().unwrap();
    assert!(prompt.contains("findings block") && prompt.contains("\"disposition\""));
    assert!(prompt.contains("canonical array form") && prompt.contains("\"findings\":[{"));
}

fn appending(overrides: &[&str]) -> Result<Expansion, String> {
    let mut layers = Layers::empty();
    layers
        .insert(
            "repo",
            "appending",
            r#"
stages = [{ id = "map", role = "scout" }]
[parameters]
tags = { type = "list", default = ["a"] }
plain = ["x"]
width = { type = "integer", default = 2 }
[roles.scout]
acceptance_checks = "${tags}"
prompt = "Map ${prompt}"
"#,
        )
        .unwrap();
    let overrides: Vec<String> = overrides.iter().map(|item| (*item).to_owned()).collect();
    expand(&layers, "appending", "brief", &overrides)
}

#[test]
fn set_append_extends_list_parameters_and_refuses_scalars() {
    let appended = appending(&[
        "parameters.tags+=['b']",
        "parameters.tags+='c'",
        "parameters.plain+=['y']",
    ])
    .unwrap();
    assert_eq!(
        appended.document["nodes"][0]["acceptance_checks"],
        json!(["a", "b", "c"])
    );
    assert_eq!(appended.origins["parameters.tags"], "--set");
    let error = appending(&["parameters.width+=1"]).err().unwrap();
    assert!(error.contains("parameters.width"), "{error}");
    assert!(error.contains("not a list"), "{error}");
}

#[test]
fn export_selecting_an_output_its_node_cannot_supply_names_both_nodes() {
    let mut layers = Layers::empty();
    layers
        .insert(
            "repo",
            "typed",
            r#"
stages = [{ id = "look", role = "scout" }]
exports = [{ node = "look", outputs = ["result", "commit"] }]
[roles.scout]
prompt = "Look at ${prompt}"
"#,
        )
        .unwrap();
    layers
        .insert(
            "repo",
            "outer",
            "stages = [{ id = \"inner\", preset = \"typed\" }]",
        )
        .unwrap();
    let expansion = expand(&layers, "outer", "brief", &[]).unwrap();
    let document: provenant_fabric_runtime::graph::GraphDocument =
        serde_json::from_value(expansion.document).unwrap();
    let error = document.admit(&Default::default()).unwrap_err();
    assert!(error.contains("look"), "{error}");
    assert!(error.contains("inner"), "{error}");
    assert!(error.contains("commit"), "{error}");
}

fn selection(stages: &str) -> Result<Expansion, String> {
    let mut layers = Layers::empty();
    layers
        .insert(
            "repo",
            "sel",
            &format!("stages = {stages}\n[roles.scout]\nprompt = \"Look at ${{prompt}}\""),
        )
        .unwrap();
    let expansion = expand(&layers, "sel", "brief", &[])?;
    let document: provenant_fabric_runtime::graph::GraphDocument =
        serde_json::from_value(expansion.document.clone()).map_err(|error| error.to_string())?;
    document.admit(&Default::default())?;
    Ok(expansion)
}

#[test]
fn composite_children_inherit_the_pinned_source_for_commit() {
    let pinned = r#"[{ id = "grp", source = { type = "commit", commit = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" },
        stages = [{ id = "look", role = "scout" }],
        exports = [{ node = "look", outputs = ["commit"] }] }]"#;
    selection(pinned).unwrap();
    let error = selection(&pinned.replace(
        "source = { type = \"commit\", commit = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\" },",
        "",
    ))
    .err()
    .unwrap();
    assert!(error.contains("commit from grp--look"), "{error}");
}

#[test]
fn selections_resolve_against_enclosing_scopes_and_branch_needs_a_writer() {
    let error = selection(
        r#"[{ id = "scope", role = "scout" },
        { id = "run", stages = [{ id = "x", role = "scout", inputs = [{ node = "scope", outputs = ["commit"] }] }] }]"#,
    )
    .err()
    .unwrap();
    assert!(
        error.contains("node run--x selects commit from scope"),
        "{error}"
    );
    let error = selection(
        r#"[{ id = "grp", source = { type = "commit", commit = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" },
        stages = [{ id = "look", role = "scout" }],
        exports = [{ node = "look", outputs = ["branch"] }] }]"#,
    )
    .err()
    .unwrap();
    assert!(error.contains("branch from grp--look"), "{error}");
}

#[test]
fn loops_supply_no_commit_or_branch() {
    let error = selection(
        r#"[{ id = "again", loop = { stages = [{ id = "look", role = "scout" }, { id = "accept", kind = "gate", condition = { type = "budget" } }], until = "accept", max_rounds = 2 } },
        { id = "after", role = "scout", inputs = [{ node = "again", outputs = ["commit"] }] }]"#,
    )
    .err()
    .unwrap();
    assert!(error.contains("commit from again"), "{error}");
}

#[test]
fn previous_round_selections_resolve_against_the_loop_body() {
    let error = selection(
        r#"[{ id = "again", loop = { stages = [{ id = "look", role = "scout" },
        { id = "fix", role = "scout", inputs = [{ node = "previous.look", outputs = ["commit"] }] }, { id = "accept", kind = "gate", condition = { type = "budget" } }], until = "accept", max_rounds = 2 } }]"#,
    )
    .err()
    .unwrap();
    assert!(error.contains("commit from again--r1--look"), "{error}");
    assert!(error.contains("again--r2--fix"), "{error}");
}

mod planner_front_end {
    use super::super::preset::{Layers, expand_with, find_prompt, planner};
    use super::{route, shapes};
    use provenant_fabric_runtime::config::{PlanAuto, PlanMode};
    use serde_json::{Value, json};

    fn layers(mode: PlanMode, unavailable: Option<&str>) -> Layers {
        let mut layers = Layers::empty();
        layers.review = Some(shapes());
        layers.plan = Some(planner::PlanContext {
            mode,
            route: "principal".into(),
            lane_deadline_seconds: 3600,
            max_nodes: 24,
            auto: PlanAuto::default(),
            unavailable: unavailable.map(str::to_owned),
            from_cli: true,
            paths: Vec::new(),
            worktrees: Vec::new(),
        });
        layers
    }

    fn run(
        layers: &Layers,
        name: &str,
        brief: &str,
        params: Vec<(String, Value)>,
    ) -> super::super::preset::Expansion {
        expand_with(layers, name, brief, &params, &[]).unwrap()
    }

    fn worktrees() -> Vec<(String, Value)> {
        vec![
            ("worktrees".into(), json!(["/repo/lane1", "/repo/lane2"])),
            ("batch_worktree".into(), json!("/repo/batch")),
        ]
    }

    /// The same preset without any plan context: the baseline that `off` must equal.
    fn plain(name: &str, params: Vec<(String, Value)>) -> super::super::preset::Expansion {
        let mut layers = Layers::empty();
        layers.review = Some(shapes());
        expand_with(&layers, name, "brief", &params, &[]).unwrap()
    }

    fn ids(expansion: &super::super::preset::Expansion) -> Vec<String> {
        expansion.document["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|node| node["id"].as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn plan_on_swaps_the_lanes_for_a_principal_planner_with_the_catalogue() {
        let expansion = run(
            &layers(PlanMode::On, None),
            "big-batch",
            "brief",
            worktrees(),
        );
        assert_eq!(ids(&expansion)[..3], ["scout", "plan", "council"]);
        let plan = &expansion.document["nodes"][1];
        assert_eq!(plan["kind"], "plan");
        assert_eq!(plan["role"], "plan.shape");
        assert_eq!(plan["access"], "read_only");
        assert_eq!(route(plan)["alias"], "principal");
        assert_eq!(plan["depends_on"], json!(["scout"]));
        assert_eq!(plan["plan"]["patterns_only"], true);
        assert_eq!(plan["plan"]["disjoint_waves"], true);
        let names: Vec<_> = plan["plan"]["patterns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pattern| pattern["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["writer-lane", "wave-barrier"]);
        let budget = &plan["plan"]["patterns"][0]["parameters"]["budget_seconds"];
        assert_eq!(
            (budget["default"].clone(), budget["max"].clone()),
            (json!(3600), json!(3600))
        );
        assert_eq!(
            plan["plan"]["patterns"][0]["parameters"]["worktree"]["enum"],
            json!(["/repo/lane1", "/repo/lane2"])
        );
        assert_eq!(
            expansion.document["nodes"][2]["depends_on"],
            json!(["plan"])
        );
        let decision = expansion.plan.as_ref().unwrap();
        assert_eq!(decision.line(), "plan: on -> on (planning requested)");
        assert_eq!(
            expansion.document["provenance"]["plan"]["route"],
            "principal"
        );
        // The planned graph is admissible: the runtime validates every pattern.
        let document: provenant_fabric_runtime::graph::GraphDocument =
            serde_json::from_value(expansion.document.clone()).unwrap();
        document.inspect(&Default::default()).unwrap();
    }

    #[test]
    fn plan_off_and_no_context_leave_the_static_graph_untouched() {
        let plain = plain("big-batch", Vec::new());
        let off = run(
            &layers(PlanMode::Off, None),
            "big-batch",
            "brief",
            Vec::new(),
        );
        assert_eq!(plain.document["nodes"], off.document["nodes"]);
        assert_eq!(
            off.plan.as_ref().unwrap().line(),
            "plan: off -> off (planning turned off)"
        );
        assert!(plain.plan.is_none());
        // A preset with no catalogue is never planned under auto, and refuses on.
        let single = run(&layers(PlanMode::Auto, None), "single", "brief", Vec::new());
        assert!(single.plan.is_none());
        let error =
            expand_with(&layers(PlanMode::On, None), "single", "brief", &[], &[]).unwrap_err();
        assert!(error.contains("declares no planner patterns"), "{error}");
        assert!(error.contains("single"), "{error}");
        assert!(error.contains("--plan auto or --plan off"), "{error}");
        // A config default of `on` is not an explicit request: the run goes
        // ahead on the static graph with the reason recorded.
        let mut configured = layers(PlanMode::On, None);
        configured.plan.as_mut().unwrap().from_cli = false;
        let quiet = run(&configured, "single", "brief", Vec::new());
        let decision = quiet.plan.as_ref().unwrap();
        assert!(!decision.applied);
        assert_eq!(decision.reason, "preset declares no planner patterns");
        assert_eq!(
            decision.line(),
            "plan: on -> off (preset declares no planner patterns)"
        );
        assert!(!ids(&quiet).contains(&"plan".to_owned()));
    }

    fn pattern_names(expansion: &super::super::preset::Expansion) -> Vec<String> {
        let plan = expansion.document["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["id"] == "plan")
            .unwrap();
        plan["plan"]["patterns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pattern| pattern["name"].as_str().unwrap().to_owned())
            .collect()
    }

    fn pattern<'a>(expansion: &'a super::super::preset::Expansion, name: &str) -> &'a Value {
        expansion.document["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["id"] == "plan")
            .and_then(|plan| plan["plan"]["patterns"].as_array())
            .and_then(|patterns| patterns.iter().find(|pattern| pattern["name"] == name))
            .unwrap()
    }

    #[test]
    fn research_presets_offer_the_planner_scouts_and_workers_with_a_question_and_a_route() {
        let on = layers(PlanMode::On, None);
        let swarm = run(&on, "scout-swarm", "brief", Vec::new());
        assert_eq!(ids(&swarm), ["plan", "synthesis"]);
        assert_eq!(pattern_names(&swarm), ["scout", "external-scout"]);
        let scout = pattern(&swarm, "scout");
        assert_eq!(scout["nodes"][0]["role"], "scout.codebase");
        assert_eq!(scout["parameters"]["route"]["default"], "junior");
        assert_eq!(
            scout["parameters"]["route"]["enum"],
            json!(["junior", "senior", "principal"])
        );
        assert_eq!(scout["parameters"]["question"]["type"], "string");
        assert!(
            scout["nodes"][0]["prompt"]
                .as_str()
                .unwrap()
                .contains("${question}")
        );
        assert_eq!(
            pattern(&swarm, "external-scout")["nodes"][0]["role"],
            "scout.external"
        );
        for name in ["fan-out", "fan-out-synthesise"] {
            let fan = run(&on, name, "brief", Vec::new());
            assert_eq!(pattern_names(&fan), ["worker"], "{name}");
            let worker = pattern(&fan, "worker");
            assert_eq!(worker["parameters"]["route"]["default"], "senior");
            assert_eq!(route(&worker["nodes"][0])["alias"], "${route}");
        }
        let synth = run(&on, "fan-out-synthesise", "brief", Vec::new());
        assert_eq!(ids(&synth), ["plan", "synthesis"]);
        // An explicit width is the caller's choice, so auto leaves the graph alone.
        let given = run(
            &layers(PlanMode::Auto, None),
            "scout-swarm",
            "brief",
            vec![("width".into(), json!(3))],
        );
        assert_eq!(
            given.plan.as_ref().unwrap().reason,
            "width given explicitly"
        );
        assert!(!given.plan.as_ref().unwrap().applied);
    }

    #[test]
    fn external_zero_removes_the_external_scout_from_the_catalogue() {
        let on = layers(PlanMode::On, None);
        for name in ["scout-swarm", "scope-then-run"] {
            let with = run(&on, name, "brief", Vec::new());
            assert!(pattern_names(&with).contains(&"external-scout".to_owned()));
            let without = run(&on, name, "brief", vec![("external".into(), json!(0))]);
            assert!(
                !pattern_names(&without).contains(&"external-scout".to_owned()),
                "{name}: {:?}",
                pattern_names(&without)
            );
            assert!(pattern_names(&without).contains(&"scout".to_owned()));
        }
    }

    #[test]
    fn shared_scout_and_worker_patterns_are_declared_identically() {
        let on = layers(PlanMode::On, None);
        let swarm = run(&on, "scout-swarm", "brief", Vec::new());
        let scoped = run(&on, "scope-then-run", "brief", Vec::new());
        for name in ["scout", "external-scout"] {
            assert_eq!(pattern(&swarm, name), pattern(&scoped, name), "{name}");
        }
        let fan = run(&on, "fan-out", "brief", Vec::new());
        let synthesised = run(&on, "fan-out-synthesise", "brief", Vec::new());
        assert_eq!(pattern(&fan, "worker"), pattern(&synthesised, "worker"));
    }

    #[test]
    fn planned_scouts_keep_the_callers_read_roots() {
        let on = layers(PlanMode::On, None);
        for name in ["scout-swarm", "scope-then-run"] {
            let extra = vec![("read_roots".to_owned(), json!(["/repo/extra"]))];
            let expansion = run(&on, name, "brief", extra);
            let roots = &pattern(&expansion, "scout")["nodes"][0]["read_roots"];
            assert!(
                roots.as_array().unwrap().contains(&json!("/repo/extra")),
                "{name}: {roots}"
            );
            // A fragment cannot widen its planner, so the planner holds them too.
            let planner = expansion.document["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|node| node["id"] == "plan")
                .unwrap();
            assert_eq!(planner["read_roots"], json!(["/repo/extra"]), "{name}");
        }
    }

    #[test]
    fn scope_then_run_lets_the_planner_decide_whether_to_scout() {
        let expansion = run(
            &layers(PlanMode::On, None),
            "scope-then-run",
            "brief",
            Vec::new(),
        );
        assert_eq!(
            ids(&expansion),
            ["plan", "synthesis", "scope", "ready", "run"]
        );
        assert_eq!(
            pattern_names(&expansion),
            ["scout", "external-scout", "no-scout"]
        );
        let none = pattern(&expansion, "no-scout");
        assert_eq!(none["parameters"]["reason"]["type"], "string");
        assert_eq!(none["exports"][0]["node"], "note");
        // The scouts' evidence is reconciled before scoping, as the static
        // scout stage's synthesis does.
        let nodes = expansion.document["nodes"].as_array().unwrap();
        assert_eq!(nodes[1]["depends_on"], json!(["plan"]));
        assert_eq!(nodes[1]["role"], "plan.research");
        assert_eq!(nodes[2]["depends_on"], json!(["synthesis"]));
    }

    fn lens_names(expansion: &super::super::preset::Expansion) -> Vec<String> {
        pattern_names(expansion)
            .iter()
            .map(|name| name.strip_prefix("lens-").unwrap().to_owned())
            .collect()
    }

    fn static_lenses(params: Vec<(String, Value)>) -> Vec<String> {
        let plain = plain("review-council", params);
        plain.document["nodes"][1]["composite"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|node| {
                node["id"]
                    .as_str()
                    .unwrap()
                    .strip_prefix("lens-")
                    .unwrap()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn review_council_plans_only_lenses_the_panel_allows() {
        let on = layers(PlanMode::On, None);
        let base = vec![("worktree".to_owned(), json!("/repo/lane"))];
        for tags in [json!([]), json!(["security"])] {
            let mut params = base.clone();
            params.push(("risk_tags".into(), tags.clone()));
            let planned = run(&on, "review-council", "brief", params.clone());
            // The catalogue is exactly the lens set the static stage would run.
            assert_eq!(lens_names(&planned), static_lenses(params), "{tags}");
            assert_eq!(ids(&planned), ["subject", "plan", "verify", "synthesis"]);
        }
        // A gated lens is offered only when its risk tag is present.
        let mut quiet = base.clone();
        quiet.push(("risk_tags".into(), json!([])));
        assert!(
            !lens_names(&run(&on, "review-council", "brief", quiet))
                .contains(&"security-privacy".to_owned())
        );
        let mut tagged = base.clone();
        tagged.push(("risk_tags".into(), json!(["security"])));
        let planned = run(&on, "review-council", "brief", tagged);
        assert!(lens_names(&planned).contains(&"security-privacy".to_owned()));
        // Each lens leg pins the commit itself and offers its panel tier.
        let leg = pattern(&planned, "lens-correctness-spec");
        assert_eq!(leg["nodes"][0]["role"], "lens.correctness-spec");
        assert_eq!(leg["nodes"][0]["source"]["type"], "commit");
        assert_eq!(route(&leg["nodes"][0])["alias"], "${tier}");
        assert_eq!(leg["parameters"]["tier"]["default"], "junior");
        assert!(
            leg["parameters"]["tier"]["enum"]
                .as_array()
                .unwrap()
                .contains(&json!("principal"))
        );
    }

    #[test]
    fn review_council_with_a_non_commit_subject_keeps_its_static_lenses() {
        let params = vec![
            ("worktree".to_owned(), json!("/repo/lane")),
            (
                "subject".to_owned(),
                json!({"type": "node", "node": "write"}),
            ),
        ];
        let expansion = run(
            &layers(PlanMode::On, None),
            "review-council",
            "brief",
            params,
        );
        let decision = expansion.plan.as_ref().unwrap();
        assert!(!decision.applied);
        assert!(
            decision.reason.contains("node subject cannot be pinned"),
            "{}",
            decision.reason
        );
        assert!(!ids(&expansion).contains(&"plan".to_owned()));
    }

    #[test]
    fn review_council_auto_plans_only_a_large_subject() {
        let sized = |size: Option<&'static str>| -> Layers {
            let mut layers = layers(PlanMode::Auto, None);
            layers.review = Some(Box::new(move |query| {
                let mut hint = shapes()(query)?;
                hint.size = size.map(str::to_owned);
                Ok(hint)
            }));
            layers
        };
        let params = || vec![("worktree".to_owned(), json!("/repo/lane"))];
        let large = run(&sized(Some("large")), "review-council", "brief", params());
        let decision = large.plan.as_ref().unwrap();
        assert!(decision.applied, "{}", decision.reason);
        assert_eq!(decision.reason, "review subject is large");
        assert!(ids(&large).contains(&"plan".to_owned()));
        let small = run(&sized(Some("small")), "review-council", "brief", params());
        let decision = small.plan.as_ref().unwrap();
        assert!(!decision.applied);
        assert_eq!(decision.reason, "review subject is not large (size=small)");
        assert!(!ids(&small).contains(&"plan".to_owned()));
        // A long brief does not override the panel's size.
        let long = (1..=12)
            .map(|n| format!("- [ ] criterion {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let brief = format!("Review #1 #2 #3 #4 #5 #6\n## Acceptance\n{long}");
        let small_long = run(&sized(Some("small")), "review-council", &brief, params());
        let decision = small_long.plan.as_ref().unwrap();
        assert!(!decision.applied, "{}", decision.reason);
        assert_eq!(decision.reason, "review subject is not large (size=small)");
        let unknown = run(&sized(None), "review-council", "brief", params());
        assert!(
            unknown
                .plan
                .as_ref()
                .unwrap()
                .reason
                .contains("size=unknown")
        );
    }

    #[test]
    fn plan_on_warns_when_it_discards_explicit_lanes() {
        let lanes = vec![(
            "lanes".into(),
            json!([{"name":"one","worktree":"/w","scope":["src"],"spec":""}]),
        )];
        let expansion = run(
            &layers(PlanMode::On, None),
            "big-batch",
            "brief",
            lanes.clone(),
        );
        assert!(ids(&expansion).contains(&"plan".to_owned()));
        assert!(
            expansion
                .warnings
                .iter()
                .any(|warning| warning.contains("--plan on discards the explicit lanes")),
            "{:?}",
            expansion.warnings
        );
        // Without explicit lanes there is nothing to discard.
        let quiet = run(
            &layers(PlanMode::On, None),
            "big-batch",
            "brief",
            Vec::new(),
        );
        assert!(
            quiet
                .warnings
                .iter()
                .all(|warning| !warning.contains("discards"))
        );
        // Auto never overrides explicit lanes, so it never warns.
        let auto = run(&layers(PlanMode::Auto, None), "big-batch", "brief", lanes);
        assert!(
            auto.warnings
                .iter()
                .all(|warning| !warning.contains("discards"))
        );
    }

    #[test]
    fn worktrees_default_to_the_projects_linked_worktrees_unless_listed() {
        let mut project = layers(PlanMode::On, None);
        project.plan.as_mut().unwrap().worktrees = vec!["/repo/w1".into(), "/repo/w2".into()];
        let defaulted = run(&project, "big-batch", "brief", Vec::new());
        let enumerated = |expansion: &super::super::preset::Expansion| {
            expansion.document["nodes"][1]["plan"]["patterns"][0]["parameters"]["worktree"]["enum"]
                .clone()
        };
        assert_eq!(enumerated(&defaulted), json!(["/repo/w1", "/repo/w2"]));
        // A chair-supplied list wins over the discovered one.
        let listed = run(&project, "big-batch", "brief", worktrees());
        assert_eq!(enumerated(&listed), json!(["/repo/lane1", "/repo/lane2"]));
    }

    #[test]
    fn linked_worktrees_come_from_git_without_the_main_checkout() {
        let temp = tempfile::tempdir().unwrap();
        let main = temp.path().join("main");
        let linked = temp.path().join("linked");
        std::fs::create_dir(&main).unwrap();
        let git = |dir: &std::path::Path, args: &[&str]| {
            let output = provenant_os::fixture::git_command()
                .current_dir(dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .output()
                .unwrap();
            assert!(output.status.success(), "{args:?}: {output:?}");
        };
        git(&main, &["init", "-q", "-b", "main"]);
        git(&main, &["commit", "-q", "--allow-empty", "-m", "root"]);
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "lane",
                linked.to_str().unwrap(),
            ],
        );
        let found = super::super::preset::linked_worktrees(&main);
        let expected = std::fs::canonicalize(&linked)
            .unwrap()
            .display()
            .to_string();
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(
            std::fs::canonicalize(&found[0])
                .unwrap()
                .display()
                .to_string(),
            expected
        );
        // Outside a repository the project is only its own directory: no lanes.
        let bare = temp.path().join("bare");
        std::fs::create_dir(&bare).unwrap();
        assert!(super::super::preset::linked_worktrees(&bare).is_empty());
        // The discovered list is what the planner is offered by default.
        let mut project = layers(PlanMode::On, None);
        project.plan.as_mut().unwrap().worktrees = found.clone();
        let expansion = run(&project, "big-batch", "brief", Vec::new());
        assert_eq!(
            expansion.document["nodes"][1]["plan"]["patterns"][0]["parameters"]["worktree"]["enum"],
            json!(found)
        );
    }

    #[test]
    fn any_override_beyond_the_bundled_default_counts_as_explicit() {
        let lane = "[{name='one', worktree='/w', scope=['src'], spec=''}]";
        let mut layers = layers(PlanMode::Auto, None);
        layers
            .insert(
                "repo",
                "mine",
                &format!("extends='big-batch'\n[parameters]\nlanes={lane}\n"),
            )
            .unwrap();
        let expansion = run(&layers, "mine", &acceptance(7), Vec::new());
        let decision = expansion.plan.as_ref().unwrap();
        assert!(!decision.applied, "{}", decision.line());
        assert_eq!(decision.reason, "lanes given explicitly");
        // The bundled default alone is not an override.
        let bundled = run(&layers, "big-batch", &acceptance(2), Vec::new());
        assert!(bundled.plan.as_ref().unwrap().applied);
    }

    #[test]
    fn an_extending_preset_missing_the_shaped_stage_degrades_never_errors() {
        let mut layers = layers(PlanMode::On, None);
        layers
            .insert(
                "user",
                "narrow",
                "extends='big-batch'\nstages=[{ id = 'only', preset = 'single' }]\n",
            )
            .unwrap();
        let expansion = run(&layers, "narrow", "brief", Vec::new());
        let decision = expansion.plan.as_ref().unwrap();
        assert!(!decision.applied);
        let why = decision.degraded.as_deref().unwrap();
        assert!(why.contains("names no top-level stage: lanes"), "{why}");
        assert!(decision.reason.contains("deterministic defaults"));
        assert!(!ids(&expansion).contains(&"plan".to_owned()));
        assert!(
            expansion
                .warnings
                .iter()
                .any(|warning| warning.contains("deterministic defaults"))
        );
    }

    fn planned_error(extends: &str, plan: &str) -> String {
        let mut layers = layers(PlanMode::On, None);
        layers
            .insert("user", "custom", &format!("extends='{extends}'\n{plan}\n"))
            .unwrap();
        expand_with(&layers, "custom", "brief", &[], &[]).unwrap_err()
    }

    #[test]
    fn catalogue_gates_and_trailing_stages_are_validated() {
        let pattern = |gate: &str| {
            format!(
                "[[plan.patterns]]\nname='w'\nenabled_by={gate}\nexports=[{{node='n',outputs=['result']}}]\nnodes=[{{id='n',brief='b',acceptance_checks=['c']}}]\n[plan.patterns.parameters]\nq={{type='string'}}\n"
            )
        };
        let unknown = planned_error(
            "fan-out",
            &format!("[plan]\nreplaces='branch'\n{}", pattern("'nope'")),
        );
        assert!(
            unknown.contains("enabled_by names no parameter: nope"),
            "{unknown}"
        );
        let wrong = planned_error(
            "fan-out",
            &format!("[plan]\nreplaces='branch'\n{}", pattern("3")),
        );
        assert!(
            wrong.contains("enabled_by must name a parameter"),
            "{wrong}"
        );
        let list = planned_error("scope-then-run", "[plan]\nafter='synthesis'");
        assert!(list.contains("plan.after must be a list"), "{list}");
        let nameless = planned_error("scope-then-run", "[plan]\nafter=[{ role='plan.research' }]");
        assert!(
            nameless.contains("plan.after stages need an id"),
            "{nameless}"
        );
        let repeat = planned_error(
            "scope-then-run",
            "[plan]\nafter=[{ id='scope', role='plan.research' }]",
        );
        assert!(repeat.contains("repeats a stage id"), "{repeat}");
    }

    fn acceptance(count: usize) -> String {
        let items: String = (1..=count).map(|n| format!("{n}. item {n}\n")).collect();
        format!("Do the work.\n\n## Acceptance\n{items}\n## Verify\n- cargo test\n")
    }

    #[test]
    fn auto_decides_on_and_off_with_a_reason() {
        let explicit_lanes = vec![(
            "lanes".into(),
            json!([{"name":"one","worktree":"/w","scope":["src"],"spec":""}]),
        )];
        type Case = (
            &'static str,
            String,
            Vec<(String, Value)>,
            bool,
            &'static str,
        );
        let cases: Vec<Case> = vec![
            (
                "large spec",
                acceptance(7),
                explicit_lanes.clone(),
                false,
                "lanes given explicitly",
            ),
            (
                "lanes unset, long spec",
                acceptance(7),
                Vec::new(),
                true,
                "7 acceptance items, more than 6",
            ),
            (
                "lanes unset, short spec",
                acceptance(3),
                Vec::new(),
                true,
                "lanes not set",
            ),
            (
                "several crates",
                "Touch crates/a/x.rs, crates/b/y.rs and crates/c/z.rs".into(),
                Vec::new(),
                true,
                "write scope spans 3 crates, more than 2",
            ),
            (
                "batch of issues",
                "Land #101 and #102 together".into(),
                Vec::new(),
                true,
                "2 issues in one batch",
            ),
        ];
        for (label, brief, params, on, reason) in cases {
            let expansion = run(&layers(PlanMode::Auto, None), "big-batch", &brief, params);
            let decision = expansion.plan.as_ref().unwrap();
            assert_eq!(decision.applied, on, "{label}: {}", decision.line());
            assert!(
                decision.reason.contains(reason),
                "{label}: {}",
                decision.line()
            );
            assert_eq!(ids(&expansion).contains(&"plan".to_owned()), on, "{label}");
        }
        let small = run(
            &layers(PlanMode::Auto, None),
            "big-batch",
            &acceptance(2),
            explicit_lanes,
        );
        assert!(
            small
                .plan
                .as_ref()
                .unwrap()
                .line()
                .starts_with("plan: auto -> off (lanes given explicitly")
        );
        // Thresholds come from the configuration.
        let mut tight = layers(PlanMode::Auto, None);
        tight.plan.as_mut().unwrap().auto.acceptance_items = 2;
        let expansion = run(&tight, "big-batch", &acceptance(3), vec![]);
        assert!(
            expansion
                .plan
                .as_ref()
                .unwrap()
                .reason
                .contains("3 acceptance items, more than 2")
        );
    }

    #[test]
    fn subject_counters_read_briefs() {
        assert_eq!(
            planner::acceptance_items("## Acceptance\n- a\n- b\n## Out\n- c"),
            2
        );
        assert_eq!(
            planner::acceptance_items("Acceptance:\n1. a\n2) b\n\n3. c"),
            3
        );
        assert_eq!(planner::acceptance_items("- a\n- b"), 0);
        assert_eq!(planner::issues("Closes #12, refs #12 and #13; C#"), 2);
        assert_eq!(planner::issues("no issues &#39;"), 0);
        // Hex colours, identifiers and over-long numbers are not issues.
        assert_eq!(planner::issues("colour #1a2b3c and #fff and #1234567"), 0);
        assert_eq!(planner::issues("#abc123 #12ab #99 #123456"), 2);
        assert_eq!(planner::issues("see (#7) and #8."), 2);
        // Nested sub-bullets belong to their parent item.
        assert_eq!(
            planner::acceptance_items(
                "## Acceptance\n- a\n  - detail\n    - more\n- b\n\t- tab detail\n1. c\n   1. nested"
            ),
            3
        );
    }

    #[test]
    fn unavailable_principal_route_degrades_to_the_static_defaults_with_a_reason() {
        let degraded = run(
            &layers(
                PlanMode::On,
                Some("route principal is not defined in the routing configuration"),
            ),
            "big-batch",
            "brief",
            Vec::new(),
        );
        let plain = plain("big-batch", Vec::new());
        assert_eq!(degraded.document["nodes"], plain.document["nodes"]);
        let decision = degraded.plan.as_ref().unwrap();
        assert!(!decision.applied);
        assert!(
            decision
                .line()
                .starts_with("plan: on -> off (principal route unavailable"),
            "{}",
            decision.line()
        );
        assert!(
            decision
                .degraded
                .as_deref()
                .unwrap()
                .contains("not defined")
        );
        assert!(
            degraded
                .warnings
                .iter()
                .any(|warning| warning.contains("deterministic defaults"))
        );
        assert_eq!(
            degraded.document["provenance"]["plan"]["degraded"],
            "route principal is not defined in the routing configuration"
        );
        // Never a weaker planner: no node stands in for the planner.
        assert!(!ids(&degraded).contains(&"plan".to_owned()));
    }

    /// A planned writer lane is the static `generator-evaluator` lane, written
    /// independently: expand that preset on its own and compare node by node.
    #[test]
    fn a_planned_lane_keeps_the_static_lanes_review_steps() {
        let planned = run(
            &layers(PlanMode::On, None),
            "big-batch",
            "brief",
            worktrees(),
        );
        let catalogue = &planned.document["nodes"][1]["plan"]["patterns"][0];
        assert_eq!(catalogue["name"], "writer-lane");
        // The default lens roster and the evaluator, with no review shape
        // selecting a roster; shape-selected rosters are a follow-up.
        let unplanned = Layers::empty();
        let lane = expand_with(
            &unplanned,
            "generator-evaluator",
            "brief",
            &[
                ("worktree".into(), json!("${worktree}")),
                ("scope".into(), json!(["${scope}"])),
                ("delivery".into(), json!("integrate:batch/graph")),
            ],
            &[],
        )
        .unwrap();
        let mut expected = Vec::new();
        for node in lane.document["nodes"].as_array().unwrap() {
            match node.get("composite") {
                Some(inner) => {
                    for leg in inner["nodes"].as_array().unwrap() {
                        let mut leg = leg.clone();
                        leg["source"] = node["source"].clone();
                        expected.push(leg);
                    }
                }
                None => expected.push(node.clone()),
            }
        }
        let project = |node: &Value| {
            json!({
                "id": node["id"],
                "role": node["role"],
                "role_digest": node["role_digest"],
                "access": node["access"],
                "kind": node.get("kind"),
                "route": node["route"]["alias"],
                "distinct_from": node.get("distinct_from"),
                "dispositions": node.get("dispositions"),
                "source": node.get("source").map(|source| source["type"].clone()),
                "read_roots": node.get("read_roots"),
                "prompt": node["prompt"],
            })
        };
        let by_id = |nodes: &[Value]| {
            let mut rows: Vec<Value> = nodes.iter().map(project).collect();
            rows.sort_by_key(|row| row["id"].as_str().unwrap().to_owned());
            rows
        };
        let mut actual: Vec<Value> = catalogue["nodes"].as_array().unwrap().clone();
        // The writer's route is the pattern's `route` parameter and its brief is
        // the lane's own; the review steps match the static ones exactly.
        for node in &mut actual {
            if node["id"] == "implement" {
                node["route"] = json!({"alias": "senior"});
            }
        }
        let (actual, expected) = (by_id(&actual), by_id(&expected));
        assert_eq!(actual.len(), 4);
        for (planned, static_lane) in actual.iter().zip(&expected) {
            if planned["id"] == "implement" {
                for key in ["role", "role_digest", "access", "source"] {
                    assert_eq!(planned[key], static_lane[key], "implement {key}");
                }
            } else {
                assert_eq!(planned, static_lane);
            }
        }
        let ids: Vec<_> = actual
            .iter()
            .map(|row| row["id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            [
                "evaluate",
                "implement",
                "lens-correctness-spec",
                "lens-simplicity"
            ]
        );
    }

    /// The planned document is a checked-in fixture the runtime plan tests run.
    /// A planner node whose pattern reads `root` and may take any of `values`.
    fn reading(root: &str, values: &[String]) -> Value {
        json!({"id": "plan", "plan": {"patterns": [{
            "name": "lane",
            "parameters": {"tree": {"type": "string", "enum": values}},
            "nodes": [{"id": "lens", "read_roots": [root]}]
        }]}})
    }

    #[test]
    fn a_pattern_read_root_is_confined_to_the_projects_worktrees() {
        use provenant_fabric_runtime::read_roots::ReadRoots;
        let temp = tempfile::tempdir().unwrap();
        let inside = temp.path().join("inside");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let policy = ReadRoots::new(vec![inside.clone()], &[]);
        let text = |path: &std::path::Path| path.display().to_string();
        let confine = |root: &str, values: &[String]| {
            super::super::preset::confine_pattern_roots(&mut reading(root, values), &policy)
        };
        assert!(confine("${tree}", &[text(&inside)]).is_ok());
        // A suffix, a prefix or a second placeholder is not a closed-list root.
        let tree = [text(&inside)];
        assert!(confine("${tree}/../outside", &tree).is_err());
        assert!(confine("/x/${tree}", &tree).is_err());
        assert!(confine("${tree}${tree}", &tree).is_err());
        // Every value the planner may pick is confined, not only the first.
        assert!(confine("${tree}", &[text(&inside), text(&outside)]).is_err());
        let climbing = inside.join("..").join("outside");
        assert!(confine("${tree}", &[text(&inside), text(&climbing)]).is_err());
        assert!(confine("${tree}", &["inside".into()]).is_err(), "relative");
        assert!(confine("${tree}", &[]).is_err(), "an open list");
    }

    #[test]
    fn a_pattern_read_root_that_is_a_link_is_pinned_to_its_canonical_target() {
        use provenant_fabric_runtime::read_roots::ReadRoots;
        let temp = tempfile::tempdir().unwrap();
        let inside = temp.path().join("inside");
        let target = inside.join("lane");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let policy = ReadRoots::new(vec![inside.clone()], &[]);
        let given = link.display().to_string();
        let mut node = reading("${tree}", std::slice::from_ref(&given));
        node["prompt"] = json!(format!("Pick from {given}, then {given}."));
        super::super::preset::confine_pattern_roots(&mut node, &policy).unwrap();
        let canonical = std::fs::canonicalize(&target)
            .unwrap()
            .display()
            .to_string();
        let values = &node["plan"]["patterns"][0]["parameters"]["tree"]["enum"];
        assert_eq!(values, &json!([canonical]));
        assert_eq!(
            node["prompt"],
            json!(format!("Pick from {canonical}, then {canonical}."))
        );
        // Retargeting the link after admission moves nothing the graph holds.
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert_eq!(
            &node["plan"]["patterns"][0]["parameters"]["tree"]["enum"],
            values
        );
    }

    #[test]
    fn planned_review_lenses_receive_file_spec() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("spec.md"), "planned acceptance criteria").unwrap();
        let mut layers = layers(PlanMode::On, None);
        layers.project_dir = Some(temp.path().to_path_buf());
        let expansion = run(
            &layers,
            "review-council",
            "brief",
            vec![("spec".into(), json!("spec.md"))],
        );
        let brief = find_prompt(&expansion.document["nodes"], "lens.correctness-spec").unwrap();
        assert!(brief.contains("planned acceptance criteria"), "{brief}");
    }

    #[test]
    fn planned_research_and_review_match_their_golden_graphs() {
        let council = vec![("worktree".to_owned(), json!("/repo/lane"))];
        let roots = vec![("read_roots".to_owned(), json!(["/repo/extra"]))];
        let scoped = vec![("worktree".to_owned(), json!("/repo/lane"))];
        for (name, params) in [
            ("scout-swarm", roots),
            ("review-council", council),
            ("fan-out", Vec::new()),
            ("fan-out-synthesise", Vec::new()),
            ("scope-then-run", scoped),
        ] {
            let expansion = run(&layers(PlanMode::On, None), name, "brief", params);
            let mut actual = serde_json::to_string_pretty(&expansion.document).unwrap();
            actual.push('\n');
            let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join(format!("assets/presets/fixtures/{name}.plan.json"));
            if std::env::var_os("UPDATE_GOLDEN").is_some() {
                std::fs::write(&path, &actual).unwrap();
            }
            let expected = std::fs::read_to_string(&path).unwrap_or_default();
            assert_eq!(actual, expected, "{name}: rerun with UPDATE_GOLDEN=1");
        }
    }

    #[test]
    fn planned_prompts_use_the_effective_local_check_policy() {
        for preset in ["big-batch", "review-council", "scope-then-run"] {
            for (project_policy, parameter, expected) in [
                (
                    None,
                    "",
                    "Local check policy: run the tests you add or change",
                ),
                (Some("Project policy."), "", "Project policy."),
                (
                    Some("Project policy."),
                    "Parameter policy.",
                    "Parameter policy.",
                ),
            ] {
                let mut layers = layers(PlanMode::On, None);
                if let Some(policy) = project_policy {
                    layers
                        .roles
                        .add(
                            ".provenant/roles.toml",
                            &format!("local_check_policy = '{policy}'"),
                        )
                        .unwrap();
                }
                let mut params = worktrees();
                if preset != "big-batch" {
                    params = vec![("worktree".into(), json!("/repo/lane"))];
                }
                params.push(("local_check_policy".into(), json!(parameter)));
                let expansion = run(&layers, preset, "brief", params);
                let role = if preset == "review-council" {
                    "lens.correctness-spec"
                } else {
                    "write.implement"
                };
                let prompt = find_prompt(&expansion.document["nodes"], role).expect(role);
                assert!(prompt.contains(expected), "{preset}: {role}: {prompt}");
            }
        }
    }

    fn review_prompts(value: &Value) -> Vec<String> {
        let mut prompts = Vec::new();
        match value {
            Value::Object(map) => {
                if map.get("role").and_then(Value::as_str).is_some_and(|role| {
                    role.starts_with("lens.")
                        || role.starts_with("review.")
                        || role == "plan.review"
                }) {
                    prompts.push(map["prompt"].as_str().unwrap().to_owned());
                }
                // Static fallback lanes retain their own explicit lane specs.
                for (key, child) in map {
                    if key != "fallback" {
                        prompts.extend(review_prompts(child));
                    }
                }
            }
            Value::Array(items) => {
                for child in items {
                    prompts.extend(review_prompts(child));
                }
            }
            _ => {}
        }
        prompts
    }

    #[test]
    fn planned_big_batch_reviews_share_one_bounded_batch_spec() {
        let temp = tempfile::tempdir().unwrap();
        let batch = temp.path().join("batch");
        let writer = temp.path().join("writer");
        for root in [temp.path(), &batch, &writer] {
            std::fs::create_dir_all(root.join(".agent-run")).unwrap();
        }
        std::fs::write(writer.join(".agent-run/spec.md"), "lane-only criteria").unwrap();
        std::fs::write(temp.path().join(".agent-run/spec.md"), "project criteria").unwrap();
        let path = batch.join(".agent-run/spec.md");
        let content = format!("batch criteria {}", "x".repeat(20_000));
        std::fs::write(&path, &content).unwrap();
        let digest = "d5ee5caf2a1e61a61df03de27472700e6d3944682ee99c8112243eb36972e54d";
        let mut layers = layers(PlanMode::On, None);
        layers.project_dir = Some(temp.path().to_owned());
        let mut baseline = None;
        // An open choice and many choices carry exactly the same review text.
        for count in [0, 1, 64] {
            let expansion = run(
                &layers,
                "big-batch",
                "brief",
                vec![
                    ("worktrees".into(), json!(vec![&writer; count])),
                    ("batch_worktree".into(), json!(batch)),
                    ("spec".into(), json!(".agent-run/spec.md")),
                ],
            );
            assert!(!expansion.warnings.iter().any(|w| w.contains("spec file")));
            let prompts = review_prompts(&expansion.document["nodes"]);
            assert!(
                prompts.len() > 3,
                "include lane, council and repair reviews"
            );
            for prompt in &prompts {
                let (_, context) = prompt
                    .split_once("\n\nSpec file: ")
                    .unwrap_or_else(|| panic!("missing spec: {prompt}"));
                assert!(context.starts_with(&format!(
                    "{} (sha256:{digest}; excerpt up to 16384 bytes)\n",
                    path.display()
                )));
                let (_, excerpt) = context.split_once('\n').unwrap();
                assert_eq!(excerpt, &content[..16_384]);
                assert!(!prompt.contains("lane-only criteria"));
                assert!(!prompt.contains("project criteria"));
            }
            if let Some(baseline) = &baseline {
                assert_eq!(&prompts, baseline, "review text must not grow with lanes");
            } else {
                baseline = Some(prompts);
            }
            let document: provenant_fabric_runtime::graph::GraphDocument =
                serde_json::from_value(expansion.document).unwrap();
            // An open worktree list is not runtime-admissible until confined.
            if count > 0 {
                document.inspect(&Default::default()).unwrap();
            }
        }
        std::fs::remove_file(&path).unwrap();
        let fallback = run(
            &layers,
            "big-batch",
            "brief",
            vec![
                ("batch_worktree".into(), json!(batch)),
                ("spec".into(), json!(".agent-run/spec.md")),
            ],
        );
        for prompt in review_prompts(&fallback.document["nodes"]) {
            assert!(prompt.contains("project criteria"));
            assert!(prompt.contains(&temp.path().join(".agent-run/spec.md").display().to_string()));
        }
    }

    #[test]
    fn planned_big_batch_lane_only_spec_warns_even_with_open_worktree_choices() {
        let temp = tempfile::tempdir().unwrap();
        let writer = temp.path().join("writer");
        std::fs::create_dir_all(writer.join(".agent-run")).unwrap();
        std::fs::write(writer.join(".agent-run/spec.md"), "lane-only criteria").unwrap();
        let mut layers = layers(PlanMode::On, None);
        layers.project_dir = Some(temp.path().to_owned());
        for choices in [json!([]), json!([writer])] {
            let expansion = run(
                &layers,
                "big-batch",
                "brief",
                vec![
                    ("worktrees".into(), choices),
                    ("batch_worktree".into(), json!(temp.path().join("batch"))),
                    ("spec".into(), json!(".agent-run/spec.md")),
                ],
            );
            assert!(expansion.warnings.iter().any(|w| {
                w.contains("spec file '.agent-run/spec.md'")
                    && w.contains("pass an issue-number spec instead")
            }));
            assert_eq!(
                expansion.document["provenance"]["warnings"],
                json!(expansion.warnings)
            );
            let prompts = review_prompts(&expansion.document["nodes"]);
            assert!(prompts.len() > 3);
            for prompt in prompts {
                assert!(!prompt.contains("Spec file:"));
                assert!(!prompt.contains("lane-only criteria"));
            }
        }
    }

    #[test]
    fn planned_big_batch_matches_its_golden_graph() {
        let expansion = run(
            &layers(PlanMode::On, None),
            "big-batch",
            "brief",
            worktrees(),
        );
        let mut actual = serde_json::to_string_pretty(&expansion.document).unwrap();
        actual.push('\n');
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets/presets/fixtures/big-batch.plan.json");
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::write(&path, &actual).unwrap();
        }
        let expected = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(actual, expected, "rerun with UPDATE_GOLDEN=1 to refresh");
    }
}

fn real_selector(directory: &str) -> Layers {
    let mut layers = Layers::empty();
    layers.review = Some(super::preset::review_selector(
        provenant_routing::ReviewConfig::default(),
        std::path::PathBuf::from(directory),
        None,
    ));
    layers
}

fn review_lens_count(document: &serde_json::Value) -> usize {
    let review = document["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["id"] == "review")
        .unwrap();
    review["composite"]["nodes"][1]["composite"]["nodes"]
        .as_array()
        .unwrap()
        .len()
}

#[test]
fn deliver_forwards_size_and_kind_into_the_review_panel() {
    let layers = real_selector("/nonexistent");
    let panel = |extra: &[&str]| {
        let mut overrides = vec!["parameters.worktree='/repo/w'".to_owned()];
        overrides.extend(extra.iter().map(|s| (*s).to_owned()));
        expand(&layers, "deliver", "brief", &overrides).unwrap()
    };
    assert_eq!(
        review_lens_count(&panel(&["parameters.size='small'"]).document),
        1
    );
    assert_eq!(
        review_lens_count(&panel(&["parameters.size='medium'"]).document),
        5
    );
    assert_eq!(
        review_lens_count(
            &panel(&["parameters.size='medium'", "parameters.kind='refactor'"]).document
        ),
        6
    );
    assert_eq!(
        review_lens_count(&panel(&["parameters.size='large'"]).document),
        8
    );
}

/// A review of a head or node subject with no size or shape cannot be measured
/// until the subject is pinned, so expansion emits one deferred node for the
/// host to resolve and measures nothing itself.
#[test]
fn a_head_subject_review_defers_its_size_to_the_pinned_commit() {
    let layers = real_selector("/nonexistent");
    let expanded = expand(
        &layers,
        "deliver",
        "brief",
        &[
            "parameters.worktree='/repo/w'".into(),
            "parameters.base='main'".into(),
        ],
    )
    .unwrap();
    let document = &expanded.document;
    assert_eq!(document["schema_version"], 2);
    assert_eq!(document["review_base"], "main");
    // No expansion-time git read: nothing to degrade, nothing to warn about.
    assert!(expanded.warnings.is_empty(), "{:?}", expanded.warnings);
    let review = &document["nodes"][2]["composite"]["nodes"];
    let lenses = &review[1];
    assert_eq!(lenses["id"], "lenses");
    assert_eq!(lenses["kind"], "plan");
    assert_eq!(lenses["review"]["worktree"], "/repo/w");
    assert_eq!(lenses["review"]["max_nodes"], 12);
    assert_eq!(lenses["depends_on"], json!(["subject"]));
    assert_eq!(lenses["read_roots"], json!(["/repo/w"]));
    assert!(lenses.get("composite").is_none());
    let templates = lenses["lens_templates"].as_object().unwrap();
    assert!(templates.contains_key("correctness-spec") && templates.contains_key("adversarial"));
    let template = &templates["tests"];
    assert_eq!(template["access"], "read_only");
    assert_eq!(template["role"], "lens.tests");
    for stripped in ["route", "routes", "depends_on", "inputs", "read_roots"] {
        assert!(template.get(stripped).is_none(), "{stripped}");
    }
    // The writer keeps one route; read-only members repeat their alias.
    assert!(document["nodes"][0].get("routes").is_none());
    assert_eq!(document["nodes"][0]["route"]["alias"], "senior");
    assert_eq!(review[0]["routes"].as_array().unwrap().len(), 2);
    let verify = &review[2];
    assert_eq!(verify["id"], "verify");
    let document: provenant_fabric_runtime::graph::GraphDocument =
        serde_json::from_value(document.clone()).unwrap();
    assert_eq!(document.review_base.as_deref(), Some("main"));
}

#[test]
fn explicit_size_shape_and_commit_subjects_keep_the_static_v1_panel() {
    let layers = real_selector("/nonexistent");
    let deliver = |extra: &str| {
        expand(
            &layers,
            "deliver",
            "brief",
            &["parameters.worktree='/repo/w'".into(), extra.into()],
        )
        .unwrap()
        .document
    };
    for extra in ["parameters.size='small'", "parameters.review_shape='quick'"] {
        let document = deliver(extra);
        assert_eq!(document["schema_version"], 1, "{extra}");
        assert!(document.get("review_base").is_none(), "{extra}");
        assert!(document["nodes"][2]["composite"]["nodes"][1]["composite"].is_object());
    }
    // The default council reviews a commit, which is already pinned.
    let document = expand(
        &layers,
        "review-council",
        "brief",
        &["parameters.worktree='/repo/w'".into()],
    )
    .unwrap()
    .document;
    assert_eq!(document["schema_version"], 1);
    assert!(document["nodes"][1]["composite"].is_object());
    // A preset that never reviews stays on v1 with no review fields.
    for name in ["single", "chain", "fan-out", "scout-swarm"] {
        let document = expand(&Layers::empty(), name, "brief", &[])
            .unwrap()
            .document;
        assert_eq!(document["schema_version"], 1, "{name}");
    }
}

#[test]
fn reviews_that_disagree_on_the_base_are_refused() {
    let mut layers = real_selector("/nonexistent");
    layers
        .insert(
            "repo",
            "two-reviews",
            r#"
stages = [
  { id = "a", role = "writer" },
  { id = "first", preset = "review-council", depends_on = ["a"], parameters = { subject = { type = "head", node = "a" }, worktree = "/repo/w", base = "main" } },
  { id = "second", preset = "review-council", depends_on = ["a"], parameters = { subject = { type = "head", node = "a" }, worktree = "/repo/w", base = "release" } },
]
[parameters]
worktree = { type = "string", default = "/repo/w" }
[roles.writer]
tier = "senior"
access = "worktree_write"
worktree = "${worktree}"
write_scope = ["src"]
"#,
        )
        .unwrap();
    let error = expand(&layers, "two-reviews", "brief", &[]).unwrap_err();
    assert!(error.contains("disagree on the base"), "{error}");
}

#[test]
fn read_only_aliases_repeat_while_exact_models_and_writers_stay_single() {
    let tiers = |bad: bool| {
        let mut layers = Layers::empty();
        layers
            .insert(
                "repo",
                "tiers",
                &format!(
                    r#"
stages = [
  {{ id = "look", role = "scout" }},
  {{ id = "pin", role = "scout-exact" }},
  {{ id = "edit", role = "writer" }},
  {{ id = "table", role = "scout-table" }},
]
[parameters]
worktree = "/repo/lane"
[roles.scout]
tier = "junior"
[roles.scout-exact]
tier = "codex/gpt-6@high"
[roles.scout-table]
tier = {{ {} }}
[roles.writer]
tier = "senior"
access = "worktree_write"
worktree = "${{worktree}}"
write_scope = ["src"]
"#,
                    if bad {
                        "effort = \"high\""
                    } else {
                        "alias = \"senior\", effort = \"high\""
                    }
                ),
            )
            .unwrap();
        layers.known_routes = Some(["senior".to_owned()].into());
        expand(&layers, "tiers", "brief", &[])
    };
    let error = tiers(true).unwrap_err();
    assert!(
        error.contains("route table needs an alias, or an adapter and a model"),
        "{error}"
    );
    let nodes = tiers(false).unwrap().document["nodes"].clone();
    assert_eq!(
        nodes[0]["routes"],
        json!([{"alias":"junior"}, {"alias":"junior"}])
    );
    assert_eq!(
        nodes[1]["route"],
        json!({"adapter":"codex","model":"gpt-6","effort":"high"})
    );
    assert!(nodes[1].get("routes").is_none());
    assert_eq!(nodes[2]["route"], json!({"alias":"senior"}));
    assert!(nodes[2].get("routes").is_none());
    // A kept route table repeats like any other read-only alias.
    assert_eq!(
        nodes[3]["routes"],
        json!([{"alias":"senior","effort":"high"}, {"alias":"senior","effort":"high"}])
    );
}

#[test]
fn distinct_families_labels_the_siblings_of_a_fan_out() {
    let family = |extra: &[&str]| {
        let overrides: Vec<String> = extra.iter().map(|s| (*s).to_owned()).collect();
        expand(&Layers::empty(), "fan-out", "brief", &overrides)
            .unwrap()
            .document
    };
    let off = family(&[]);
    assert_eq!(off["schema_version"], 1);
    assert!(off["nodes"][0].get("family_group").is_none());
    let on = family(&["parameters.distinct_families=true", "parameters.width=3"]);
    assert_eq!(on["schema_version"], 2);
    let groups: Vec<_> = on["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["family_group"].clone())
        .collect();
    assert_eq!(groups.len(), 3);
    assert!(groups[0].is_string() && groups.iter().all(|group| *group == groups[0]));
    // A single branch has no sibling to separate from.
    let single = family(&["parameters.distinct_families=true", "parameters.width=1"]);
    assert!(single["nodes"][0].get("family_group").is_none());
}

#[test]
fn an_unknown_size_override_is_refused() {
    let layers = real_selector("/nonexistent");
    let error = expand(
        &layers,
        "deliver",
        "brief",
        &[
            "parameters.worktree='/repo/w'".into(),
            "parameters.size='huge'".into(),
        ],
    )
    .unwrap_err();
    assert!(error.contains("unknown review size"), "{error}");
}

#[test]
fn big_batch_without_scout_matches_golden() {
    let expanded = super::preset::expand_with(
        &Layers::empty(),
        "big-batch",
        "brief",
        &[("scout".into(), json!(0))],
        &[],
    )
    .unwrap();
    let nodes = expanded.document["nodes"].as_array().unwrap();
    assert!(!nodes.iter().any(|node| node["id"] == "scout"));
    let mut actual = serde_json::to_string_pretty(&expanded.document).unwrap();
    actual.push('\n');
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("assets/presets/fixtures/big-batch.no-scout.json");
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, &actual).unwrap();
    }
    assert_eq!(actual, std::fs::read_to_string(path).unwrap_or_default());
}

#[test]
fn compact_preset_show_lists_review_council_parameter_docs() {
    let layers = Layers::empty();
    let (values, origins) = layers.resolve("review-council").unwrap();
    let data = json!({"name": "review-council", "values": values, "origins": origins});
    let shown = super::preset::human_for_test(&data, false);
    for (name, value) in data["values"]["parameters"].as_object().unwrap() {
        assert!(shown.contains(name), "missing {name}");
        if let Some(doc) = value.get("doc").and_then(Value::as_str) {
            let first = doc.find(". ").map_or(doc, |end| &doc[..=end]);
            assert!(shown.contains(first), "missing {name}: {first}");
        }
    }
    assert!(!shown.contains("Origins\n"));
    assert!(super::preset::human_for_test(&data, true).contains("Origins\n"));
}

#[test]
fn compact_preset_explain_lists_parameter_names() {
    let data = json!({"name": "example", "parameters": [{"name": "base"}, {"name": "risk"}]});
    assert!(super::preset::human_for_test(&data, false).contains("Parameters: base, risk"));
}

#[test]
fn preset_show_and_explain_json_match_snapshots() {
    use provenant_records::{CommandResult, Outcome};
    let layers = Layers::empty();
    let (values, mut origins) = layers.resolve("review-council").unwrap();
    origins.extend(expand(&layers, "review-council", "", &[]).unwrap().origins);
    let show = CommandResult::new(
        "preset",
        Outcome::Ok,
        json!({"name": "review-council", "values": values, "origins": origins}),
    );
    let explain = CommandResult::new(
        "preset",
        Outcome::Ok,
        layers.explain("review-council").unwrap(),
    );
    // `UPDATE_GOLDEN=1` rewrites the fixtures.
    let golden = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    for (actual, name) in [
        (
            serde_json::to_value(show).unwrap(),
            "preset-review-council-show.json",
        ),
        (
            serde_json::to_value(explain).unwrap(),
            "preset-review-council-explain.json",
        ),
    ] {
        let path = golden.join(name);
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            let mut text = serde_json::to_string_pretty(&actual).unwrap();
            text.push('\n');
            std::fs::write(&path, text).unwrap();
        }
        let expected = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            actual,
            serde_json::from_str::<Value>(&expected).unwrap_or_default(),
            "{name}: rerun with UPDATE_GOLDEN=1 to refresh"
        );
    }
}

#[test]
fn preset_human_views_preserve_detail_and_type_plain_tables() {
    let data = json!({"name":"example", "values":{"parameters":{
        "schema":{"type":"integer", "default":1, "min":0, "doc":"First.\nSecond."},
        "subject":{"type":"commit", "commit":"HEAD"}
    }}, "origins":{}});
    let compact = super::preset::human_for_test(&data, false);
    assert!(compact.contains("schema  integer  1  First."));
    assert!(!compact.contains("Second."));
    assert!(compact.contains("subject  table"));
    let full = super::preset::human_for_test(&data, true);
    assert!(full.contains("\"min\":0"));
    assert!(full.contains("Second."));
    let explanation = json!({"name":"example", "pattern":{"summary":"Summary", "cost":"Cost", "select_when":"Condition"}, "parameters":[{"name":"base"}]});
    let compact = super::preset::human_for_test(&explanation, false);
    for value in ["Summary", "Cost", "Condition", "base"] {
        assert!(compact.contains(value));
    }
}

/// A council over a pinned commit expands statically, yet it still records the
/// requested base so the host submits a real diff rather than a self-diff.
#[test]
fn a_static_commit_council_records_the_requested_base() {
    let layers = real_selector("/nonexistent");
    let document = expand(
        &layers,
        "review-council",
        "brief",
        &[
            "parameters.worktree='/repo/w'".into(),
            "parameters.base='origin/main'".into(),
        ],
    )
    .unwrap()
    .document;
    assert!(document["nodes"][1]["composite"].is_object());
    assert_eq!(document["review_base"], "origin/main");
    // The runtime refuses a v1 document that carries a review base.
    assert_eq!(document["schema_version"], 2);
}

/// Every document a preset emits must pass the admission checks the runtime
/// applies on submit, not just deserialize: a document that carried a review
/// base under schema 1 expanded and explained cleanly yet was refused as
/// "invalid input" by every `run --preset review-council` with a base.
#[test]
fn every_bundled_preset_document_passes_graph_admission() {
    let layers = real_selector("/nonexistent");
    let limits = provenant_fabric_runtime::config::GraphLimits::default();
    let shapes: [&[&str]; 5] = [
        &[],
        &["parameters.base='main'"],
        &["parameters.base='main'", "parameters.review_shape='quick'"],
        &["parameters.base='main'", "parameters.size='small'"],
        &["parameters.review_shape='quick'"],
    ];
    let mut checked = 0;
    for name in [
        "single",
        "chain",
        "fan-out",
        "scout-swarm",
        "generator-evaluator",
        "review-council",
        "scope-then-run",
        "deliver",
        "big-batch",
    ] {
        for extra in shapes {
            let mut overrides: Vec<String> = match name {
                "generator-evaluator" | "review-council" | "scope-then-run" | "deliver" => {
                    vec!["parameters.worktree='/repo/lane'".into()]
                }
                "big-batch" => vec![
                    "parameters.lanes=[{name='one',worktree='/repo/one',scope=['.'],spec=''},{name='two',worktree='/repo/two',scope=['.'],spec=''}]".into(),
                    "parameters.batch_worktree='/repo/batch'".into(),
                ],
                _ => Vec::new(),
            };
            overrides.extend(extra.iter().map(|entry| (*entry).to_owned()));
            let mut expanded = match expand(&layers, name, "approved brief", &overrides) {
                Ok(expanded) => expanded,
                // A preset without a review stage declares no review parameters.
                Err(error) if error.contains("unknown parameter") => continue,
                Err(error) => panic!("{name} {extra:?}: {error}"),
            };
            pin_fixture_sources(&mut expanded.document);
            let document: provenant_fabric_runtime::graph::GraphDocument =
                serde_json::from_value(expanded.document)
                    .unwrap_or_else(|error| panic!("{name} {extra:?}: {error}"));
            document
                .validate(&limits)
                .unwrap_or_else(|error| panic!("{name} {extra:?}: {error}"));
            checked += 1;
        }
    }
    assert!(checked >= 20, "{checked} documents checked");
}

/// The lenses of a generator-evaluator judge what the writer commits, so the
/// panel is resolved after the writer runs even though the preset's own subject
/// is a commit.
#[test]
fn a_generator_review_defers_its_size_until_the_writer_has_run() {
    let layers = real_selector("/nonexistent");
    let document = expand(
        &layers,
        "generator-evaluator",
        "brief",
        &["parameters.worktree='/repo/lane'".into()],
    )
    .unwrap()
    .document;
    assert_eq!(document["schema_version"], 2);
    let lens = document["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["id"] == "lens")
        .unwrap();
    assert_eq!(lens["kind"], "plan");
    assert_eq!(lens["source"]["type"], "node");
    assert_eq!(lens["source"]["node"], "implement");
    assert!(lens["lens_templates"].is_object());
    let sized = expand(
        &layers,
        "generator-evaluator",
        "brief",
        &[
            "parameters.worktree='/repo/lane'".into(),
            "parameters.size='small'".into(),
        ],
    )
    .unwrap()
    .document;
    assert_eq!(sized["schema_version"], 1);
}

#[test]
fn preset_file_spec_reaches_review_nodes_and_warns_when_missing() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let writer = temp.path().join("writer");
    std::fs::create_dir_all(project.join(".agent-run")).unwrap();
    std::fs::create_dir_all(writer.join(".agent-run")).unwrap();
    std::fs::write(project.join(".agent-run/spec.md"), "wrong project spec").unwrap();
    std::fs::write(
        writer.join(".agent-run/spec.md"),
        "writer acceptance criteria",
    )
    .unwrap();
    let mut layers = Layers::empty();
    layers.project_dir = Some(project.clone());
    for preset in ["generator-evaluator", "review-council"] {
        let expanded = super::preset::expand_with(
            &layers,
            preset,
            "brief",
            &[
                ("worktree".into(), json!(writer)),
                ("spec".into(), json!(".agent-run/spec.md")),
                ("spec_max_bytes".into(), json!(16)),
            ],
            &[],
        )
        .unwrap();
        let document: provenant_fabric_runtime::graph::GraphDocument =
            serde_json::from_value(expanded.document.clone()).unwrap();
        let _ = document;
        fn check_reviews(value: &serde_json::Value, expected_path: &str) -> usize {
            let mut count = 0;
            match value {
                serde_json::Value::Object(map) => {
                    if let Some(role) = map.get("role").and_then(serde_json::Value::as_str)
                        && (role.starts_with("lens.")
                            || role.starts_with("review.")
                            || role == "plan.review")
                    {
                        let brief = map["prompt"].as_str().unwrap();
                        assert!(brief.contains("writer acceptanc"), "{role}: {brief}");
                        assert!(
                            brief.contains(&format!("Spec file: {expected_path} (sha256:")),
                            "{role}"
                        );
                        assert!(brief.contains(
                            "2e4b7d401613f23b5002dfe69d8e2b02606c5982fc2a53941d2ea8b0aa05e4d0"
                        ));
                        assert!(!brief.contains("wrong project spec"));
                        count += 1;
                    }
                    for child in map.values() {
                        count += check_reviews(child, expected_path);
                    }
                }
                serde_json::Value::Array(items) => {
                    for child in items {
                        count += check_reviews(child, expected_path);
                    }
                }
                _ => {}
            }
            count
        }
        assert!(
            check_reviews(
                &expanded.document["nodes"],
                &writer.join(".agent-run/spec.md").display().to_string()
            ) >= 3
        );
        let missing = super::preset::expand_with(
            &layers,
            preset,
            "brief",
            &[
                ("worktree".into(), json!(writer)),
                ("spec".into(), json!(".agent-run/missing.md")),
            ],
            &[],
        )
        .unwrap();
        assert!(
            missing
                .warnings
                .iter()
                .any(|w| w.contains("spec file '.agent-run/missing.md'"))
        );
        assert_eq!(
            missing.document["provenance"]["warnings"],
            json!(missing.warnings)
        );
        let issue = expand(
            &layers,
            preset,
            "brief",
            &["parameters.spec='#2785'".into()],
        )
        .unwrap();
        assert!(!issue.warnings.iter().any(|w| w.contains("spec file")));
    }
    std::fs::remove_file(writer.join(".agent-run/spec.md")).unwrap();
    let fallback = super::preset::expand_with(
        &layers,
        "generator-evaluator",
        "brief",
        &[
            ("worktree".into(), json!(writer)),
            ("spec".into(), json!(".agent-run/spec.md")),
        ],
        &[],
    )
    .unwrap();
    let brief = find_prompt(&fallback.document["nodes"], "lens.correctness-spec").unwrap();
    assert!(brief.contains("wrong project spec"));
    assert!(brief.contains(&project.join(".agent-run/spec.md").display().to_string()));
}

#[cfg(unix)]
#[test]
fn preset_file_spec_falls_back_after_read_error_and_names_all_candidates() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let writer = temp.path().join("writer");
    std::fs::create_dir_all(project.join(".agent-run")).unwrap();
    // Opening a directory succeeds on Unix, but reading it as a file fails.
    std::fs::create_dir_all(writer.join(".agent-run/spec.md")).unwrap();
    let project_spec = project.join(".agent-run/spec.md");
    std::fs::write(&project_spec, "project acceptance criteria").unwrap();
    let mut layers = Layers::empty();
    layers.project_dir = Some(project.clone());
    let expand_spec = || {
        super::preset::expand_with(
            &layers,
            "generator-evaluator",
            "brief",
            &[
                ("worktree".into(), json!(writer)),
                ("spec".into(), json!(".agent-run/spec.md")),
            ],
            &[],
        )
        .unwrap()
    };
    let expanded = expand_spec();
    assert!(
        find_prompt(&expanded.document["nodes"], "lens.correctness-spec")
            .unwrap()
            .contains("project acceptance criteria")
    );
    assert!(!expanded.warnings.iter().any(|w| w.contains("spec file")));
    std::fs::remove_file(&project_spec).unwrap();
    let missing = expand_spec();
    let warning = missing
        .warnings
        .iter()
        .find(|w| w.contains("spec file"))
        .unwrap();
    assert!(
        warning.contains(&writer.join(".agent-run/spec.md").display().to_string()),
        "{warning}"
    );
    assert!(
        warning.contains(&project_spec.display().to_string()),
        "{warning}"
    );
}

#[test]
fn preset_file_spec_resolves_relative_writer_from_launch_and_falls_back_to_main() {
    let temp = tempfile::tempdir().unwrap();
    let main = temp.path().join("main");
    let linked = temp.path().join("linked");
    let writer = temp.path().join("writer");
    std::fs::create_dir(&main).unwrap();
    let git = |args: &[&str]| {
        let output = provenant_os::fixture::git_command()
            .current_dir(&main)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["commit", "-q", "--allow-empty", "-m", "root"]);
    git(&[
        "worktree",
        "add",
        "-q",
        "-b",
        "linked",
        linked.to_str().unwrap(),
    ]);
    let launch = linked.join("subdir");
    std::fs::create_dir_all(&launch).unwrap();
    std::fs::create_dir_all(writer.join(".agent-run")).unwrap();
    std::fs::create_dir_all(main.join(".agent-run")).unwrap();
    std::fs::write(writer.join(".agent-run/spec.md"), "writer criteria").unwrap();
    std::fs::write(main.join(".agent-run/spec.md"), "main criteria").unwrap();
    git(&["add", "-f", ".agent-run/spec.md"]);
    git(&["commit", "-q", "-m", "main spec"]);
    let mut layers = Layers::empty();
    layers.project_dir = Some(linked.clone());
    layers.launch_dir = Some(launch);
    let expand_spec = || {
        super::preset::expand_with(
            &layers,
            "generator-evaluator",
            "brief",
            &[
                ("worktree".into(), json!("../../writer")),
                ("spec".into(), json!(".agent-run/spec.md")),
            ],
            &[],
        )
        .unwrap()
    };
    let expanded = expand_spec();
    assert!(
        find_prompt(&expanded.document["nodes"], "lens.correctness-spec")
            .unwrap()
            .contains("writer criteria")
    );
    std::fs::remove_file(writer.join(".agent-run/spec.md")).unwrap();
    let expanded = expand_spec();
    assert!(
        find_prompt(&expanded.document["nodes"], "lens.correctness-spec")
            .unwrap()
            .contains("main criteria")
    );
    std::fs::remove_file(main.join(".agent-run/spec.md")).unwrap();
    let missing = expand_spec();
    let warning = missing
        .warnings
        .iter()
        .find(|w| w.contains("spec file"))
        .unwrap();
    for candidate in [
        linked.join("subdir/../../writer/.agent-run/spec.md"),
        linked.join(".agent-run/spec.md"),
        super::preset::project_worktrees(&linked)[0].join(".agent-run/spec.md"),
    ] {
        assert!(
            warning.contains(&candidate.display().to_string()),
            "{warning}"
        );
    }
}

#[test]
fn file_spec_expansion_golden() {
    let mut layers = Layers::empty();
    layers.project_dir = Some(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    let mut expanded = expand(
        &layers,
        "generator-evaluator",
        "brief",
        &["parameters.spec='assets/presets/fixtures/review-spec.md'".into()],
    )
    .unwrap();
    expanded.document["graph_id"] = json!("<graph-id>");
    let mut actual = serde_json::to_string_pretty(&expanded.document)
        .unwrap()
        .replace(env!("CARGO_MANIFEST_DIR"), "<project>");
    actual.push('\n');
    let path = layers
        .project_dir
        .unwrap()
        .join("assets/presets/fixtures/generator-evaluator-file-spec.expansion.json");
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, &actual).unwrap();
    }
    assert_eq!(actual, std::fs::read_to_string(path).unwrap_or_default());
}

#[test]
fn retry_allowances_survive_preset_and_node_expansion() {
    let mut layers = Layers::empty();
    layers
        .insert(
            "repo",
            "retry-test",
            "retry={retries=3}\nstages=[{id='read',tier='junior',retry={retries=1}}]\n",
        )
        .unwrap();
    let result = expand(&layers, "retry-test", "brief", &[]).unwrap();
    assert_eq!(result.document["retry"]["retries"], 3);
    assert_eq!(result.document["nodes"][0]["retry"]["retries"], 1);
    assert!(
        serde_json::from_value::<provenant_fabric_runtime::graph::GraphDocument>(result.document)
            .is_ok()
    );
}

#[test]
fn nested_presets_retain_the_nearest_retry_default() {
    let mut layers = Layers::empty();
    layers.insert("repo", "child-retry", "retry={retries=0}\nstages=[{id='read',tier='junior',fallback_stage={tier='junior'}}]\n").unwrap();
    layers.insert("repo", "wrapper-retry", "retry={retries=4}\nstages=[{id='nested',preset='child-retry'},{id='outer',tier='junior'}]\n").unwrap();
    let result = expand(&layers, "wrapper-retry", "brief", &[]).unwrap();
    let nodes = result.document["nodes"].as_array().unwrap();
    assert_eq!(
        nodes[0]["composite"]["nodes"][0]["preset_retry"]["retries"],
        0
    );
    assert_eq!(
        nodes[0]["composite"]["nodes"][0]["fallback_stage"]["nodes"][0]["preset_retry"]["retries"],
        0
    );
    assert_eq!(nodes[1]["preset_retry"]["retries"], 4);
}

#[test]
fn five_and_seven_lane_big_batches_fit_the_checkpoint_envelope() {
    let layers = real_selector("/nonexistent");
    for n in [5, 7] {
        let lanes = (0..n)
            .map(|i| format!("{{name='l{i}',worktree='/repo/w{i}',scope=['p{i}'],spec=''}}"))
            .collect::<Vec<_>>()
            .join(",");
        let mut expanded = expand(
            &layers,
            "big-batch",
            "approved brief",
            &[
                format!("parameters.lanes=[{lanes}]"),
                "parameters.batch_worktree='/repo/batch'".into(),
                "parameters.risk_tier='crucial'".into(),
            ],
        )
        .unwrap();
        pin_fixture_sources(&mut expanded.document);
        let document: provenant_fabric_runtime::graph::GraphDocument =
            serde_json::from_value(expanded.document).unwrap();
        assert!(
            document
                .nodes
                .iter()
                .find(|node| node.id == "lanes")
                .unwrap()
                .plan
                .is_none()
        );
        document.admit(&Default::default()).unwrap();
    }
}

#[test]
fn request_bindings_follow_declarations_and_warn_when_missing() {
    let mut layers = Layers::empty();
    layers.insert("test", "custom-bindings", "stages = []\n[parameters]\npriority = { type = 'string', default = '', bind = 'risk_tier' }\ncategory = { type = 'string', default = '', bind = 'kind' }\n").unwrap();
    let (values, warnings) =
        request_bindings(&layers, "custom-bindings", Some("crucial"), Some("docs")).unwrap();
    assert!(warnings.is_empty());
    assert!(values.contains(&("priority".into(), json!("crucial"))));
    assert!(values.contains(&("category".into(), json!("docs"))));
    let (values, warnings) =
        request_bindings(&layers, "single", Some("routine"), Some("docs")).unwrap();
    assert!(values.is_empty());
    assert!(warnings.iter().any(|w| w.contains("--risk-tier")));
    assert!(warnings.iter().any(|w| w.contains("--task-kind")));
}
