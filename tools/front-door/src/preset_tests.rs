use super::preset::{Expansion, Layers, expand};
use provenant_fabric_runtime::read_roots::ReadRoots;
use serde_json::{Value, json};

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
        "generator-evaluator",
        "review-council",
        "big-batch",
        "scope-then-run",
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

fn bundled_fixture(name: &str) -> provenant_fabric_runtime::graph::GraphDocument {
    let overrides = match name {
        "generator-evaluator" | "review-council" => vec!["parameters.worktree='/repo/lane'".into()],
        "scope-then-run" => vec!["parameters.worktree='/repo/lane'".into()],
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

#[derive(Default)]
struct PresetOwner {
    /// Nodes whose result carries `VERDICT: FAIL`.
    failing: Vec<String>,
    launched: Vec<String>,
    /// The brief each launched node received, inputs included.
    prompts: Vec<(String, String)>,
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
        self.prompts
            .push((key.node_id.clone(), node.prompt.to_owned()));
        let mut result =
            provenant_records::result_capture::ResultCapture::new(node.required_verdict);
        result.push(if self.failing.contains(&key.node_id) {
            b"Blocked on the user.\nVERDICT: FAIL\n".as_slice()
        } else {
            format!("Evidence checked for {}.\nVERDICT: PASS\n", key.node_id)
                .leak()
                .as_bytes()
        });
        Ok(AttemptObservation::Terminal {
            attempt_ref: Some(format!("fixture/{}", key.node_id)),
            outcome: AttemptOutcome::Succeeded,
            launched: true,
            result: Some(Box::new(result.finish())),
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
        "generator-evaluator",
        "review-council",
        "big-batch",
        "scope-then-run",
    ] {
        let document = bundled_fixture(name);
        let config = provenant_fabric_runtime::config::GraphConfig::default();
        let limits = config.limits.clone();
        let mut owner = PresetOwner::default();
        let mut journal = PresetJournal::default();
        let run = advance_with_budget(&document, &mut owner, &mut journal, &limits, &config.budget)
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
            // One resolution, then the moved-head check before synthesis and the refactor writer.
            assert_eq!(owner.integration_pins, 3);
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
            "stages = [{ id = 'first', preset = 'single', parameters = { route = 'utility' } }, { id = 'second' }]\n",
        )
        .unwrap();
    let result = expand(&layers, "chain", "brief", &[]).unwrap();
    assert_eq!(result.document["nodes"][0]["id"], "first");
    assert_eq!(
        result.document["nodes"][0]["composite"]["nodes"][0]["route"]["alias"],
        "utility"
    );
    let result = expand(&layers, "chain", "brief", &["first.route='fast'".into()]).unwrap();
    assert_eq!(
        result.document["nodes"][0]["composite"]["nodes"][0]["route"]["alias"],
        "fast"
    );
    assert_eq!(result.origins["first.route"], "--set");
    // A leaf stage takes its own fields by stage id.
    let result = expand(&layers, "chain", "brief", &["second.tier='junior'".into()]).unwrap();
    assert_eq!(result.document["nodes"][1]["route"]["alias"], "junior");
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
/// high risk is six, otherwise two.
fn shapes() -> super::preset::ShapeSelector {
    Box::new(|named, risk| {
        let (name, source, count) = match (named, risk) {
            (Some("standard"), _) => ("standard", "override", 2),
            (_, Some("high")) => ("deep", "by_risk", 6),
            (_, Some("low")) => ("quick", "by_risk", 1),
            _ => ("standard", "default", 2),
        };
        Ok(super::preset::ShapeHint {
            name: name.into(),
            source: source.into(),
            slots: vec!["junior".into(); count],
            named: Vec::new(),
        })
    })
}

fn council(overrides: &[&str], shaped: bool) -> Result<serde_json::Value, String> {
    let mut layers = Layers::empty();
    layers.review = shaped.then(shapes);
    let overrides: Vec<String> = overrides.iter().map(|s| (*s).to_owned()).collect();
    expand(&layers, "review-council", "the change", &overrides).map(|e| e.document)
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
            "lens-design-fit-reuse",
            "lens-simplicity",
            "lens-conventions-readability",
            "lens-tests",
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
    assert_eq!(nodes.len(), 2);
    assert!(nodes.iter().all(|node| node["route"]["alias"] == "junior"));
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
    assert_eq!(nodes[0]["route"]["alias"], "principal");
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
    high.push("council.risk_tier='high'".into());
    let high = expand(&layers, "big-batch", "brief", &high).unwrap();
    assert_ne!(lens_count(&high.document, "council"), standard);
    assert!(high.review_shape.unwrap().contains("deep (by_risk)"));
    // A parent's ambient risk tier reaches a child that declares it.
    let mut layers = Layers::empty();
    layers.review = Some(shapes());
    layers
        .insert(
            "repo",
            "wrap",
            "stages = [{ id = 'council', preset = 'review-council', parameters = { worktree = '/repo/lane' } }]\n[parameters]\nrisk_tier = 'low'\n",
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
    assert_eq!(scout["route"]["alias"], "junior");
    assert_eq!(expanded.substitutions.len(), 1);
    assert_eq!(expanded.substitutions[0]["requested"], "web");
    layers.known_routes = Some(["web".to_owned()].into());
    let expanded = expand(&layers, "scout-swarm", "brief", &[]).unwrap();
    assert_eq!(
        node(&expanded.document, "external")["route"]["alias"],
        "web"
    );
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
    let verify = node(&expanded.document, "verify");
    assert_eq!(verify["family"], "cross_preferred");
    assert_eq!(verify["source"], json!({"type": "node", "node": "subject"}));
    assert_eq!(verify["route"]["alias"], "verifier");
    assert_eq!(verify["depends_on"], json!(["lenses", "subject"]));
    let synthesis = node(&expanded.document, "synthesis");
    assert_eq!(synthesis["depends_on"], json!(["verify", "subject"]));
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
    assert_eq!(
        node(&expanded.document, "verify")["route"]["alias"],
        "senior"
    );
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
        ],
    )
    .unwrap();
    let evaluate = node(&expanded.document, "evaluate");
    assert_eq!(evaluate["kind"], "synthesise");
    assert_eq!(evaluate["route"]["alias"], "principal");
    assert_eq!(evaluate["source"]["node"], "implement");
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
    let text = expanded.document.to_string();
    for lane in ["lane-a", "lane-b", "lane-c"] {
        assert!(
            text.contains(&format!("\"id\":\"{lane}\"")) || text.contains(lane),
            "{lane}"
        );
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
            .contains("needs a default")
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
    assert_eq!(result.document["nodes"][2]["route"]["alias"], "principal");
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
        result.document["nodes"][1]["composite"]["nodes"][0]["route"]["alias"],
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
            "stages = [{ id = 'council', preset = 'review-council', parameters = { worktree = '/repo/lane' } }]\n[parameters]\nsubject = { type = 'integration' }\nrisk_tier = 'high'\n",
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
fn cross_family_is_recorded_as_a_preference_not_an_enforced_constraint() {
    let expanded = expand(
        &Layers::empty(),
        "review-council",
        "the change",
        &["parameters.worktree='/repo/lane'".into()],
    )
    .unwrap();
    let verify = node(&expanded.document, "verify");
    assert_eq!(verify["family"], "cross_preferred");
    assert!(verify.get("distinct_from").is_none());
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
}
