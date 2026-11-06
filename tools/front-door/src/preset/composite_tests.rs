#![allow(
    clippy::disallowed_methods,
    reason = "Git fixture setup in isolated repositories"
)]
use super::*;

fn resolvers() -> BTreeMap<String, Vec<String>> {
    BTreeMap::from([
        ("review-topology".into(), vec!["f0-v0-s0".into()]),
        ("review-verifiers".into(), vec!["1".into()]),
        (
            "review-panel".into(),
            vec!["correctness".into(), "tests".into()],
        ),
        ("review-advice-correctness".into(), vec![String::new()]),
        ("review-advice-tests".into(), vec![String::new()]),
        ("review-advice-security".into(), vec![String::new()]),
        ("review-advice-contract".into(), vec![String::new()]),
        ("review-advice-design".into(), vec![String::new()]),
        ("review-advice-adversarial".into(), vec![String::new()]),
    ])
}

fn review_leaves(value: &Value) -> Vec<&Value> {
    match value {
        Value::Object(fields)
            if fields.get("review_lens").is_some_and(Value::is_string)
                && !fields.contains_key("composite") =>
        {
            vec![value]
        }
        Value::Object(fields) => fields.values().flat_map(review_leaves).collect(),
        Value::Array(values) => values.iter().flat_map(review_leaves).collect(),
        _ => Vec::new(),
    }
}

#[test]
fn composite_expansion_uses_the_selected_template_layers() {
    use provenant_fabric_runtime::graph::{Layer, Template, TemplateLibrary};
    let mut library = TemplateLibrary::bundled();
    let template = |brief: &str| Template {
        brief: Some(brief.into()),
        tier: Some("senior".into()),
        ..Default::default()
    };
    library
        .insert(Layer::User, "custom", template("user brief"))
        .unwrap();
    let mut layers = Layers::empty();
    layers.trusted_templates = Some(library.clone());
    library
        .insert(Layer::Repository, "custom", template("repo brief"))
        .unwrap();
    layers.templates = Some(library);
    let expand = || expand_composite(&layers, "custom", &json!({}), &[], &resolvers()).unwrap();
    assert_eq!(expand().document["nodes"][0]["prompt"], "repo brief");
    layers.exclude_repo.set(true);
    assert_eq!(expand().document["nodes"][0]["prompt"], "user brief");
}

#[test]
fn composite_catalogue_maps_every_bundled_preset() {
    let layers = Layers::empty();
    let bundled = layers.entries.keys().collect::<BTreeSet<_>>();
    let mapped = PRESET_TEMPLATES
        .iter()
        .map(|(name, _)| name.to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        bundled
            .into_iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        mapped
    );
    for (name, _) in PRESET_TEMPLATES {
        let parameters = match *name {
            "big-batch" => json!({"worktree":"/repo/lane", "write_scope":["src"]}),
            "deliver" | "generator-evaluator" | "scope-then-run" => {
                json!({"worktree":"/repo/lane", "write_scope":["src"]})
            }
            _ => json!({}),
        };
        let expansion = expand_composite(&layers, name, &parameters, &[], &resolvers())
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(expansion.document["nodes"][0]["kind"], "composite");
    }
}

#[test]
fn composite_swarms_bind_named_items_and_min_ok() {
    let layers = Layers::empty();
    for template in ["explore-swarm", "research-swarm"] {
        let expansion = expand_composite(
            &layers,
            template,
            &json!({"items":["src/graph", "src/run"], "min_ok":2}),
            &[],
            &resolvers(),
        )
        .unwrap();
        let group = &expansion.document["nodes"][0]["composite"]["nodes"][0]["composite"];
        assert_eq!(group["min_successful"], 2);
        assert_eq!(group["nodes"][0]["id"], "src-graph");
        assert!(
            group["nodes"][1]["prompt"]
                .as_str()
                .unwrap()
                .contains("src/run")
        );
    }
}

#[test]
fn composite_batch_keeps_each_lanes_route_and_binding() {
    let layers = Layers::empty();
    let lanes = vec![
        CompositeLane {
            name: "core".into(),
            worktree: "/repo/core".into(),
            scope: vec!["src/core".into()],
            tier: Some("junior".into()),
            brief: None,
        },
        CompositeLane {
            name: "api".into(),
            worktree: "/repo/api".into(),
            scope: vec!["src/api".into()],
            tier: Some("principal".into()),
            brief: Some("Implement the API".into()),
        },
    ];
    let expansion = expand_composite(
        &layers,
        "big-batch",
        &json!({"integration_branch":"batch/feature", "worktree":"/repo/integration", "write_scope":["src/core", "src/api"]}),
        &lanes,
        &resolvers(),
    )
    .unwrap();
    assert_eq!(expansion.document["integration_branch"], "batch/feature");
    let members = &expansion.document["nodes"][0]["composite"]["nodes"][0]["composite"]["nodes"];
    assert_eq!(members[0]["route"]["alias"], "junior");
    assert_eq!(members[1]["route"]["alias"], "principal");
    assert_eq!(members[1]["worktree"], "/repo/api");
    assert_eq!(members[1]["prompt"], "Implement the API");
    assert_eq!(members[0]["delivery"], "integrate:batch/feature");
    assert_batch_repair_binding(&expansion.document);
}

#[test]
fn composite_submission_refuses_secrets_and_unresolved_review_panel() {
    let layers = Layers::empty();
    assert!(expand_composite(&layers, "review", &json!({}), &[], &BTreeMap::new()).is_err());
    let secret = format!("ghp_{}", "a1".repeat(18));
    assert!(
        expand_composite(
            &layers,
            "deliver",
            &json!({"brief":secret,"worktree":"/repo/lane"}),
            &[],
            &resolvers()
        )
        .is_err()
    );
}

#[test]
fn composite_integration_branch_is_created_once_without_checkout() {
    use std::process::Command;
    let temp = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let output = Command::new(provenant_os::git::git_executable().unwrap())
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(args)
            .current_dir(temp.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    git(&["init", "-b", "main"]);
    git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.org",
        "commit",
        "--allow-empty",
        "-m",
        "initial",
    ]);
    ensure_integration_branch(temp.path(), "batch/feature").unwrap();
    let initial = git(&["rev-parse", "batch/feature"]);
    git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.org",
        "commit",
        "--allow-empty",
        "-m",
        "next",
    ]);
    ensure_integration_branch(temp.path(), "batch/feature").unwrap();
    assert_eq!(git(&["rev-parse", "batch/feature"]), initial);
    assert_eq!(git(&["branch", "--show-current"]), "main");
    assert!(ensure_integration_branch(temp.path(), "../bad").is_err());
}

#[test]
fn composite_review_of_one_is_one_attempt_and_multi_panel_requires_every_lens() {
    let layers = Layers::empty();
    let mut one = resolvers();
    one.insert("review-panel".into(), vec!["correctness".into()]);
    let expansion = expand_composite(&layers, "review", &json!({}), &[], &one).unwrap();
    let nodes = review_leaves(&expansion.document);
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0]["kind"], Value::Null);
    assert_eq!(nodes[0]["required_verdict"], "VERDICT: PASS");
    let multi = expand_composite(&layers, "review", &json!({}), &[], &resolvers()).unwrap();
    let lenses = &multi.document["nodes"][0]["composite"]["nodes"][0]["composite"]["nodes"][0]["composite"]
        ["nodes"][0];
    assert_eq!(lenses["id"], "lenses");
    assert!(lenses["composite"].get("min_successful").is_none());
    assert_eq!(review_leaves(&multi.document).len(), 2);
}

#[test]
fn composite_task_keeps_the_supplied_brief() {
    let expansion = expand_composite(
        &Layers::empty(),
        "single",
        &json!({"brief":"Map the graph kernel"}),
        &[],
        &resolvers(),
    )
    .unwrap();
    assert_eq!(
        expansion.document["nodes"][0]["composite"]["nodes"][0]["prompt"],
        "Map the graph kernel"
    );
}

#[test]
fn composite_review_override_keeps_its_additional_checks() {
    let mut library = graph::TemplateLibrary::bundled();
    library
        .insert_toml(
            graph::Layer::Repository,
            "review-panel",
            r#"
        nodes = [
          { id = "lenses", use = "review", over = ["correctness"] },
          { id = "security", use = "verify", after = ["lenses"] },
        ]
    "#,
        )
        .unwrap();
    let panel = Compiler::new(&library)
        .compile(&json!({"id":"review", "use":"review-panel"}))
        .unwrap();
    let children = &panel.composite.unwrap().nodes;
    assert_eq!(children.len(), 2);
    assert_eq!(children[1].id, "security");
}

#[test]
fn production_selection_uses_composites_parameters_and_host_panel_routes() {
    let mut layers = Layers::empty();
    layers.review = Some(Box::new(|query| {
        assert_eq!(query.risk, Some("substantial"));
        Ok(super::super::panel::ShapeHint {
            name: "host-panel".into(),
            source: "fixture".into(),
            default_lenses: vec!["correctness".into(), "tests".into()],
            slots: vec!["principal".into(), "junior".into()],
            ..Default::default()
        })
    }));
    let params = vec![("risk_tier".into(), json!("substantial"))];
    let review = expand_for_run(&layers, "review", "Review this brief", &params, &[]).unwrap();
    let lenses = review_leaves(&review.document);
    assert_eq!(lenses[0]["id"], "correctness");
    assert_eq!(lenses[0]["route"]["alias"], "principal");
    assert_eq!(lenses[1]["route"]["alias"], "junior");
    assert!(
        lenses[0]["prompt"]
            .as_str()
            .unwrap()
            .contains("Review this brief")
    );
    let mut params = params;
    params.push(("worktree".into(), json!("/repo/integration")));
    params.push(("write_scope".into(), json!(["src/core", "src/api"])));
    params.push((
        "lanes".into(),
        json!([
            {"name":"core", "worktree":"/repo/core", "scope":["src/core"], "tier":"junior"},
            {"name":"api", "worktree":"/repo/api", "scope":["src/api"], "tier":"principal"}
        ]),
    ));
    let batch = expand_for_run(
        &layers,
        "big-batch",
        "Implement this brief",
        &params,
        &["parameters.integration_branch=\"batch/production\"".into()],
    )
    .unwrap();
    assert_eq!(batch.document["integration_branch"], "batch/production");
    let members = &batch.document["nodes"][0]["composite"]["nodes"][0]["composite"]["nodes"];
    assert_eq!(members[0]["worktree"], "/repo/core");
    assert_eq!(members[0]["route"]["alias"], "junior");
    let panel = review_leaves(&batch.document["nodes"][0]["composite"]["nodes"][1]);
    assert_eq!(panel[0]["route"]["alias"], "principal");
    assert_eq!(panel[1]["route"]["alias"], "junior");
    assert_batch_repair_binding(&batch.document);
}

fn assert_batch_repair_binding(document: &Value) {
    let repair = &document["nodes"][0]["composite"]["nodes"][2];
    assert_eq!(repair["id"], "repair");
    let fix = &repair["composite"]["nodes"][0]["loop"]["body"][0];
    assert_eq!(fix["id"], "fix");
    assert_eq!(fix["worktree"], "/repo/integration");
    assert_eq!(fix["write_scope"], json!(["src/core", "src/api"]));
}

#[test]
fn production_plain_run_is_one_task_node() {
    let expansion = expand_for_run(&Layers::empty(), "single", "Do the task", &[], &[]).unwrap();
    assert_eq!(expansion.document["nodes"][0]["prompt"], "Do the task");
    assert!(expansion.document["nodes"][0].get("composite").is_none());
    assert_eq!(expansion.document["provenance"]["template"], "task");
}

#[test]
fn production_plain_run_keeps_route_access_and_attempt_controls() {
    let request = dispatch_fixture(
        [
            "run",
            "--prompt-file",
            "brief.txt",
            "--model",
            "codex/gpt-5.4",
            "--effort",
            "high",
            "--account",
            "work",
            "--mode",
            "worktree-write",
            "--worktree",
            "/repo/writer",
            "--write-root",
            "/repo/build",
            "--provider-timeout",
            "120",
            "--build-heavy",
            "--title",
            "Task title",
        ]
        .into_iter()
        .map(Into::into),
    );
    let mut expansion =
        expand_for_run(&Layers::empty(), "single", "Do the task", &[], &[]).unwrap();
    super::super::bind_plain_run(&mut expansion, &request).unwrap();
    let node = &expansion.document["nodes"][0];
    assert_eq!(node["route"]["model"], "codex/gpt-5.4");
    assert_eq!(node["route"]["effort"], "high");
    assert_eq!(node["access"], "worktree_write");
    assert_eq!(node["worktree"], "/repo/writer");
    assert_eq!(node["deadline_seconds"], 120);
    assert_eq!(node["attempt"]["account"], "work");
    assert_eq!(node["attempt"]["write_roots"][0], "/repo/build");
    assert_eq!(node["attempt"]["build_heavy"], true);
}

#[test]
fn production_graph_identity_tracks_operation_not_brief() {
    let request = |key: &str| {
        dispatch_fixture(
            ["run", "--prompt-file", "brief", "--operation-id", key]
                .into_iter()
                .map(Into::into),
        )
    };
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().canonicalize().unwrap();
    let policy = ReadRoots::new(vec![directory.clone()], &[]);
    let bind = |brief: &str, key: &str| {
        let mut expansion = expand_for_run(&Layers::empty(), "single", brief, &[], &[]).unwrap();
        super::super::bind_run_context(
            &mut expansion,
            &request(key),
            &directory,
            &policy,
            &[],
            "explicit",
            None,
        )
        .unwrap();
        expansion.document
    };
    let first = bind("first brief", "test-first");
    assert_eq!(
        first["graph_id"],
        bind("changed brief", "test-first")["graph_id"]
    );
    assert_ne!(
        first["graph_id"],
        bind("first brief", "test-second")["graph_id"]
    );
    assert_eq!(first["provenance"]["operation_id"], "test-first");
}

#[test]
fn production_selection_refuses_unknown_explicit_parameters() {
    assert!(
        expand_for_run(
            &Layers::empty(),
            "single",
            "brief",
            &[],
            &["parameters.breif='typo'".into()]
        )
        .is_err()
    );
}

#[test]
fn production_writer_leaf_binds_before_admission_and_keeps_template_attempt() {
    let mut layers = Layers::empty();
    layers.default_worktree = Some(PathBuf::from("/repo/writer"));
    let mut library = graph::TemplateLibrary::bundled();
    library
        .insert_toml(
            graph::Layer::Repository,
            "custom-writer",
            r#"
brief = "Write"
tier = "senior"
access = "worktree_write"
[node.attempt]
account = "template-account"
build_heavy = true
"#,
        )
        .unwrap();
    layers.templates = Some(library);
    let mut expansion = expand_for_run(
        &layers,
        "custom-writer",
        "Approved brief",
        &[],
        &["parameters.write_scope=['src/api']".into()],
    )
    .unwrap();
    let request = dispatch_fixture(
        ["run", "--use", "custom-writer", "--prompt-file", "brief"]
            .into_iter()
            .map(Into::into),
    );
    super::super::bind_plain_run(&mut expansion, &request).unwrap();
    let node = &expansion.document["nodes"][0];
    assert_eq!(node["worktree"], "/repo/writer");
    assert_eq!(node["write_scope"], json!(["src/api"]));
    assert_eq!(node["attempt"]["account"], "template-account");
    assert_eq!(node["attempt"]["build_heavy"], true);
}

fn dispatch_fixture(
    args: impl IntoIterator<Item = std::ffi::OsString>,
) -> Box<provenant_cli::Dispatch> {
    #[derive(clap::Parser)]
    struct Fixture {
        #[command(flatten)]
        request: provenant_cli::Dispatch,
    }
    Box::new(<Fixture as clap::Parser>::parse_from(args).request)
}
