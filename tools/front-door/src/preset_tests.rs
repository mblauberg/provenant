use super::preset::{Layers, expand};
use serde_json::json;

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
    let expanded = expand(&layers, "roles", "the change", &[]).unwrap();
    let leaves = &expanded.document["nodes"];
    assert_eq!(leaves[0]["route"], json!({"alias":"junior"}));
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
    assert_eq!(scouts[0]["route"]["alias"], "junior");
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
        "implement-with-lens",
        "review-council",
        "big-batch",
    ] {
        let document = bundled_fixture(name);
        let config = provenant_fabric_runtime::config::GraphConfig::default();
        let limits = document
            .limits
            .as_ref()
            .unwrap_or(&Default::default())
            .resolve(&config)
            .unwrap();
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

fn bundled_fixture(name: &str) -> provenant_fabric_runtime::graph::GraphDocument {
    let overrides = match name {
        "implement-with-lens" | "review-council" => vec!["parameters.worktree='/repo/lane'".into()],
        "big-batch" => vec![
            "parameters.lane_one_worktree='/repo/one'".into(),
            "parameters.lane_two_worktree='/repo/two'".into(),
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

#[derive(Default)]
struct PresetOwner {
    launched: Vec<String>,
    integration_pins: usize,
}

impl provenant_fabric_runtime::graph::AttemptOwner for PresetOwner {
    type Error = std::convert::Infallible;

    fn actor(&self) -> String {
        "preset-test-chair".into()
    }

    fn integration_head(&mut self) -> Result<Option<String>, Self::Error> {
        self.integration_pins += 1;
        Ok(Some("0123456789abcdef0123456789abcdef01234567".into()))
    }

    fn observe_or_start(
        &mut self,
        key: &provenant_fabric_runtime::graph::AttemptKey,
        node: provenant_fabric_runtime::graph::NodeInput<'_>,
        _: &provenant_fabric_runtime::graph::GraphRoute,
        access: provenant_fabric_runtime::graph::AccessMode,
        _: Option<&str>,
        _: provenant_fabric_runtime::graph::Replay<'_>,
    ) -> Result<provenant_fabric_runtime::graph::AttemptObservation, Self::Error> {
        use provenant_fabric_runtime::graph::{AccessMode, AttemptObservation, AttemptOutcome};
        self.launched.push(key.node_id.clone());
        let mut result =
            provenant_records::result_capture::ResultCapture::new(node.required_verdict);
        result.push(b"Evidence checked.\nVERDICT: PASS\n");
        Ok(AttemptObservation::Terminal {
            attempt_ref: Some(format!("fixture/{}", key.node_id)),
            outcome: AttemptOutcome::Succeeded,
            launched: true,
            result: Some(result.finish()),
            result_warning: None,
            result_path: None,
            branch: (access == AccessMode::WorktreeWrite).then(|| format!("lane/{}", key.node_id)),
            commit: if access == AccessMode::WorktreeWrite {
                Some(format!("{:040x}", self.launched.len()))
            } else {
                node.source_commit.map(str::to_owned)
            },
        })
    }
}

#[test]
fn bundled_presets_complete_through_the_real_scheduler_and_replay() {
    use provenant_fabric_runtime::graph::{GraphStatus, NodeState, advance_with_budget};
    for name in [
        "scout-swarm",
        "implement-with-lens",
        "review-council",
        "big-batch",
    ] {
        let document = bundled_fixture(name);
        let config = provenant_fabric_runtime::config::GraphConfig::default();
        let limits = document
            .limits
            .as_ref()
            .unwrap_or(&Default::default())
            .resolve(&config)
            .unwrap();
        let mut owner = PresetOwner::default();
        let mut journal = PresetJournal::default();
        let run = advance_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &limits,
            &config.limit_ceiling,
            &config.budget,
        )
        .unwrap_or_else(|error| panic!("{name}: {error}"));
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
            assert_eq!(owner.integration_pins, 1);
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
        let replay = advance_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &limits,
            &config.limit_ceiling,
            &config.budget,
        )
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
        "implement-with-lens",
        "approved brief",
        &["parameters.worktree='/repo/writer'".into()],
    )
    .unwrap();
    let nodes = &expanded.document["nodes"];
    assert_eq!(nodes[0]["worktree"], "/repo/writer");
    assert!(nodes[1].get("worktree").is_none());
    assert_eq!(nodes[1]["read_roots"], json!(["/repo/writer"]));
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
            {"id":"task", "access":"read_only", "prompt":"brief", "route":{"alias":"senior"}, "depends_on":[]}
        ])
    );
    assert_eq!(
        nodes("chain"),
        json!([
            {"id":"first", "access":"read_only", "prompt":"brief", "route":{"alias":"senior"}, "depends_on":[]},
            {"id":"second", "access":"read_only", "prompt":"brief", "route":{"alias":"senior"}, "depends_on":["first"], "inputs":[{"node":"first","outputs":["result"]}]}
        ])
    );
    assert_eq!(
        nodes("fan-out"),
        json!([
            {"id":"branch-1", "access":"read_only", "prompt":"brief", "route":{"alias":"senior"}, "depends_on":[]},
            {"id":"branch-2", "access":"read_only", "prompt":"brief", "route":{"alias":"senior"}, "depends_on":[]}
        ])
    );
    assert_eq!(
        nodes("fan-out-synthesise"),
        json!([
            {"id":"branch-1", "access":"read_only", "prompt":"brief", "route":{"alias":"senior"}, "depends_on":[]},
            {"id":"branch-2", "access":"read_only", "prompt":"brief", "route":{"alias":"senior"}, "depends_on":[]},
            {"id":"synthesis", "kind":"synthesise", "access":"read_only", "prompt":"brief", "route":{"alias":"senior"}, "depends_on":["branch-1","branch-2"], "inputs":[{"node":"branch-1","outputs":["result"]},{"node":"branch-2","outputs":["result"]}]}
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
    assert_eq!(result.document["nodes"][0]["route"]["alias"], "fast");
    assert_eq!(result.origins["parameters.route"], "--set");
    layers
        .insert(
            "repo",
            "chain",
            "[slots.first]\npreset = 'single'\n[slots.first.parameters]\nroute = 'utility'\n",
        )
        .unwrap();
    let result = expand(&layers, "chain", "brief", &[]).unwrap();
    assert_eq!(result.document["nodes"][0]["id"], "first-task");
    assert_eq!(result.document["nodes"][0]["route"]["alias"], "utility");
    let result = expand(
        &layers,
        "chain",
        "brief",
        &["slots.first.parameters.route='fast'".into()],
    )
    .unwrap();
    assert_eq!(result.document["nodes"][0]["route"]["alias"], "fast");
    assert_eq!(result.origins["expanded.first.parameters.route"], "--set");
    assert!(
        expand(&layers, "single", "brief", &["parameters.nope=1".into()])
            .unwrap_err()
            .contains("unknown")
    );
    assert!(
        expand(
            &layers,
            "chain",
            "brief",
            &["slots.nope.preset=\"single\"".into()]
        )
        .unwrap_err()
        .contains("unknown")
    );
    layers
        .insert("repo", "single", "[slots.task]\npreset = 'single'\n")
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
        .insert("repo", "chain", "[slots.first]\npreset = 'fan-out'\n")
        .unwrap();
    let graph = expand(&layers, "chain", "brief", &[]).unwrap().document;
    assert_eq!(
        graph["nodes"][2]["depends_on"],
        json!(["first-branch-1", "first-branch-2"])
    );
    layers
        .insert(
            "repo",
            "fan-out-synthesise",
            "[slots.synthesis]\npreset = 'single'\n",
        )
        .unwrap();
    let synthesis = expand(&layers, "fan-out-synthesise", "brief", &[]).unwrap();
    assert_eq!(synthesis.document["nodes"][2]["id"], "synthesis-task");
    assert_eq!(
        synthesis.document["nodes"][2]["depends_on"],
        json!(["branch-1", "branch-2"])
    );
    let mut large = Layers::empty();
    large
        .insert(
            "repo",
            "outer",
            "extends='fan-out'\n[parameters]\nwidth=32\n[slots.branch]\npreset='inner'\n",
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
    for i in 0..8 {
        let slot = if i < 7 {
            format!("[slots.branch]\npreset='p{}'\n", i + 1)
        } else {
            String::new()
        };
        deep.insert(
            "repo",
            &format!("p{i}"),
            &format!("extends='fan-out'\n[parameters]\nwidth=1\n{slot}"),
        )
        .unwrap();
    }
    assert!(
        expand(&deep, "p0", "brief", &[])
            .unwrap_err()
            .contains("64 bytes")
    );
    let mut deep = Layers::empty();
    for i in 0..9 {
        let slot = if i < 8 {
            format!("[slots.task]\npreset='p{}'\n", i + 1)
        } else {
            String::new()
        };
        deep.insert(
            "repo",
            &format!("p{i}"),
            &format!("extends='single'\n{slot}"),
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
        error.contains("parameters.route") && error.contains("slots.task.preset"),
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
    std::fs::write(repo.join("single.toml"), "[parameters]\nunknown=1\n")?;
    let layers = Layers::load(&user, &repo)?;
    let error = expand(&layers, "single", "", &[]).unwrap_err();
    assert!(
        error.contains("single")
            && error.contains("parameters.unknown")
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
        "[slots.first]\npreset = 'single'\n[slots.second]\npreset = 'single'\n",
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
