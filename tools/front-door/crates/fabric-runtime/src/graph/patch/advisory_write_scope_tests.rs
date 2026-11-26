use super::*;
use serde_json::json;

fn writer(id: &str, worktree: &str, scopes: &[&str]) -> GraphNode {
    serde_json::from_value(json!({
        "id": id,
        "access": "worktree_write",
        "worktree": worktree,
        "write_scope": scopes,
        "prompt": "write",
        "route": {"alias": "senior"}
    }))
    .expect("valid writer fixture")
}

// Spec: docs/specs/run-launch.md#admission-and-custody
#[test]
fn overlapping_shared_worktree_scopes_are_admitted_and_warning_is_persisted() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version": 1,
        "graph_id": "shared-worktree-claims",
        "nodes": [{
            "id": "first",
            "access": "worktree_write",
            "worktree": "/repo/shared",
            "write_scope": ["src/lib"],
            "prompt": "first writer",
            "route": {"alias": "senior"}
        }]
    }))
    .expect("valid initial graph");
    let limits = plans::generous();
    document.admit(&limits).expect("initial graph is valid");
    let expanded =
        super::super::plans::expand(&document, &[], &[], &limits).expect("initial graph expands");
    let (plan, _) =
        super::super::validate_flat_plan(&expanded, &limits).expect("initial graph validates");
    let digest = super::super::journal::plan_digest(&plan)
        .expect("initial graph digest")
        .to_string();
    let mut checkpoint =
        super::super::transitions::initial_checkpoint(&document, &plan, digest, None, None, None);
    let library = TemplateLibrary::bundled();
    let resolvers = BTreeMap::new();
    let patch = Patch {
        schema_version: PATCH_SCHEMA_VERSION,
        expected_revision: Some(0),
        nodes: vec![json!({
            "id": "second",
            "access": "worktree_write",
            "worktree": "/repo/shared",
            "write_scope": ["src/lib/module.rs"],
            "prompt": "second writer",
            "route": {"alias": "senior"}
        })],
        ..Default::default()
    };

    let admission = admit_patch(
        &mut LiveGraph {
            document: &document,
            checkpoint: &mut checkpoint,
        },
        None,
        &patch,
        &PatchContext {
            catalogue: None,
            library: &library,
            resolvers: &resolvers,
            journal: None,
            limits: &limits,
            worktree_layout: provenant_config::PortTable::builtin().worktree_layout,
        },
    )
    .expect("overlapping advisory claims remain admissible");

    assert_eq!(admission.added, ["second"]);
    assert!(
        checkpoint
            .nodes
            .iter()
            .any(|node| { node.id == "first" && node.state == NodeState::Pending })
    );
    assert!(
        checkpoint
            .nodes
            .iter()
            .any(|node| { node.id == "second" && node.state == NodeState::Pending })
    );
    let warnings = &checkpoint
        .expansions
        .last()
        .expect("admission persisted")
        .floor_warnings;
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("first"));
    assert!(warnings[0].contains("second"));
}

// Spec: docs/specs/run-launch.md#admission-and-custody
#[test]
fn overlapping_static_submission_scopes_are_admitted_and_warning_is_durable() {
    let mut document: GraphDocument = serde_json::from_value(json!({
        "schema_version": 1,
        "graph_id": "static-shared-worktree-claims",
        "integration_branch": "batch/static-shared-worktree-claims",
        "nodes": [
            {
                "id": "first",
                "access": "worktree_write",
                "worktree": "/repo/shared",
                "write_scope": ["src/lib"],
                "prompt": "first writer",
                "route": {"alias": "senior"}
            },
            {
                "id": "second",
                "access": "worktree_write",
                "worktree": "/repo/shared",
                "write_scope": ["src/lib/module.rs"],
                "prompt": "second writer",
                "route": {"alias": "senior"}
            }
        ]
    }))
    .expect("valid static graph");
    let limits = plans::generous();
    super::submission(
        &mut document,
        "static-scope-overlap",
        &limits,
        provenant_config::PortTable::builtin().worktree_layout,
    )
    .expect("overlapping advisory claims do not refuse submission");
    let expanded =
        super::super::plans::expand(&document, &[], &[], &limits).expect("static graph expands");
    let (plan, _) =
        super::super::validate_flat_plan(&expanded, &limits).expect("static graph validates");
    let digest = super::super::journal::plan_digest(&plan)
        .expect("static graph digest")
        .to_string();
    let mut checkpoint =
        super::super::transitions::initial_checkpoint(&document, &plan, digest, None, None, None);
    let library = TemplateLibrary::bundled();
    let resolvers = BTreeMap::new();
    let submission = document
        .submission_patch
        .as_ref()
        .expect("submission patch was retained")
        .clone();
    let admitted = admit_patch(
        &mut LiveGraph {
            document: &document,
            checkpoint: &mut checkpoint,
        },
        None,
        &submission,
        &PatchContext {
            catalogue: None,
            library: &library,
            resolvers: &resolvers,
            journal: None,
            limits: &limits,
            worktree_layout: provenant_config::PortTable::builtin().worktree_layout,
        },
    )
    .expect("initial static admission succeeds");

    assert_eq!(
        admitted.added,
        vec!["first".to_owned(), "second".to_owned()]
    );
    assert!(
        checkpoint
            .nodes
            .iter()
            .all(|node| node.state == NodeState::Pending)
    );
    let persisted: GraphCheckpoint =
        serde_json::from_value(serde_json::to_value(&checkpoint).expect("checkpoint serialises"))
            .expect("checkpoint reloads");
    let warnings = &persisted
        .expansions
        .last()
        .expect("initial expansion persisted")
        .floor_warnings;
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("first"));
    assert!(warnings[0].contains("second"));
}

// Spec: docs/specs/run-launch.md#admission-and-custody
#[test]
fn disjoint_or_absent_claims_do_not_warn() {
    let changed = BTreeSet::from(["second".to_owned()]);
    let disjoint = vec![
        writer("first", ".worktrees/shared", &["src/lib"]),
        writer("second", ".worktrees/shared", &["src/library"]),
    ];
    assert!(advisory_write_scope_warnings(&disjoint, &changed).is_empty());

    let isolated = vec![
        writer("first", ".worktrees/one", &["src/lib"]),
        writer("second", ".worktrees/two", &["src/lib/module.rs"]),
    ];
    assert!(advisory_write_scope_warnings(&isolated, &changed).is_empty());

    let absent = vec![
        writer("first", ".worktrees/shared", &["src/lib"]),
        writer("second", ".worktrees/shared", &[]),
    ];
    assert!(advisory_write_scope_warnings(&absent, &changed).is_empty());
}

// Spec: docs/specs/run-launch.md#admission-and-custody
#[test]
fn glob_claims_are_compared_conservatively() {
    let nodes = vec![
        writer("first", ".worktrees/shared", &["src/*.rs"]),
        writer("second", ".worktrees/shared", &["docs/readme.md"]),
    ];
    let changed = BTreeSet::from(["second".to_owned()]);
    assert_eq!(advisory_write_scope_warnings(&nodes, &changed).len(), 1);
}

#[test]
fn lexical_scope_paths_preserve_leading_parent_components() {
    assert_eq!(
        lexical_path(Path::new("../../src/lib")),
        PathBuf::from("../../src/lib")
    );
    assert_eq!(
        lexical_path(Path::new("a/../../src/lib")),
        PathBuf::from("../src/lib")
    );
}

#[test]
fn mixed_scope_paths_resolve_against_an_absolute_worktree() {
    assert!(scopes_overlap(
        Path::new("/repo/shared"),
        "src/lib",
        "/repo/shared/src/lib/module.rs"
    ));
}
