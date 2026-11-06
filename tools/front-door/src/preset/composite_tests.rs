#![allow(
    clippy::disallowed_methods,
    reason = "Git fixture setup in isolated repositories"
)]
use super::*;

fn resolvers() -> BTreeMap<String, Vec<String>> {
    BTreeMap::from([(
        "review-panel".into(),
        vec!["correctness".into(), "tests".into()],
    )])
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
        &json!({"integration_branch":"batch/feature"}),
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
        let output = Command::new("git")
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
    let one = BTreeMap::from([("review-panel".into(), vec!["correctness".into()])]);
    let expansion = expand_composite(&layers, "review", &json!({}), &[], &one).unwrap();
    let nodes = expansion.document["nodes"][0]["composite"]["nodes"]
        .as_array()
        .unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0]["kind"], Value::Null);
    assert_eq!(nodes[0]["required_verdict"], "VERDICT: PASS");
    let multi = expand_composite(&layers, "review", &json!({}), &[], &resolvers()).unwrap();
    assert!(
        multi.document["nodes"][0]["composite"]["nodes"][0]["composite"]
            .get("min_successful")
            .is_none()
    );
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
