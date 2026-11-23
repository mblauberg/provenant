include!("interface_implement_invariants.rs");
#[test]
fn tier_override_preserves_leaf_family_constraints() {
    use provenant_fabric_runtime::graph::GraphNode;
    let override_route = super::RouteChoice::Alias("senior".into());
    for fields in [
        json!({"route":{"alias":"junior", "required_family":"openai"}}),
        json!({"routes":[{"alias":"junior", "required_family":"openai"},{"alias":"principal", "required_family":"openai"}]}),
    ] {
        let mut value = json!({"id":"read", "access":"read_only", "prompt":"review"});
        value
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        let mut node: GraphNode = serde_json::from_value(value).unwrap();
        super::override_tier(&mut node, &override_route);
        let route = node.route.unwrap();
        assert_eq!(route.alias.as_deref(), Some("senior"));
        assert_eq!(route.required_family.as_deref(), Some("openai"));
    }
}

#[test]
fn a_role_map_sets_each_node_to_its_role_tier_or_keeps_the_default() {
    use provenant_fabric_runtime::graph::GraphNode;
    let node = |fields: Value| -> GraphNode {
        let mut value = json!({"id":"n", "access":"read_only", "prompt":"p",
                "route":{"alias":"declared"}});
        value
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        serde_json::from_value(value).unwrap()
    };
    let cases = [
        ("writer", node(json!({"access":"worktree_write"}))),
        ("review", node(json!({"review_behaviour":"lens"}))),
        ("synthesis", node(json!({"kind":"synthesise"}))),
        ("task", node(json!({}))),
    ];
    let choice = super::RouteChoice::parse(&json!("writer=junior, review=senior")).unwrap();
    for (role, mut node) in cases {
        assert_eq!(super::route_role(&node), role);
        super::override_tier(&mut node, &choice);
        let expected = choice.tier_for(role).unwrap_or("declared");
        assert_eq!(
            node.route.unwrap().alias.as_deref(),
            Some(expected),
            "{role}"
        );
    }
    assert_eq!(
        choice,
        super::RouteChoice::parse(&json!({"writer":"junior","review":"senior"})).unwrap()
    );
    assert_eq!(
        super::RouteChoice::parse(&json!("senior")).unwrap(),
        super::RouteChoice::Alias("senior".into())
    );
}

#[test]
fn an_unknown_route_role_is_refused_listing_the_valid_roles() {
    for bad in ["wrter=junior", "writer=junior,=senior", "writer=", "="] {
        let error = super::RouteChoice::parse(&json!(bad)).unwrap_err();
        if let FacadeError::DispatchRefused { reason, .. } = &error {
            assert!(
                super::ROUTE_ROLES.iter().all(|role| reason.contains(role)),
                "{reason}"
            );
        }
    }
    assert!(super::RouteChoice::parse(&json!(7)).is_err());
}

#[test]
fn attempt_settings_typed_as_text_take_the_first_reading_the_schema_accepts() {
    for (field, text, expected) in [
        ("build_heavy", "true", json!(true)),
        ("provider_timeout", "42", json!(42)),
        ("host", "'other'", json!("other")),
        ("host", "workshop", json!("workshop")),
        ("host", "1", json!("1")),
    ] {
        assert_eq!(
            super::type_attempt_field(field, json!(text)).unwrap(),
            expected,
            "{field}={text}"
        );
    }
    assert_eq!(
        super::type_attempt_field("build_heavy", json!(true)).unwrap(),
        json!(true)
    );
    assert!(super::type_attempt_field("provider_timeout", json!("soon")).is_err());
}

#[test]
fn decode_errors_preserve_paths_and_parser_text() -> Result<()> {
    use provenant_fabric_runtime::graph::GraphDocument;
    let node = |field: &str, value: Value| {
        let mut node = json!({"id":"one","access":"read_only","prompt":"p","route":{"adapter":"mock","model":"m"}});
        match field {
            "role" => node["role"] = value,
            other => node["attempt"] = json!({ other: value }),
        }
        json!({"schema_version":1,"graph_id":"g","nodes":[node]})
    };
    for (field, path) in [
        ("directory", "nodes[0].attempt.directory"),
        ("new_worktree", "nodes[0].attempt.new_worktree"),
        ("role", "nodes[0].role"),
    ] {
        let error = super::decode_input::<GraphDocument>(node(field, json!(7)), "")
            .expect_err("decode must fail");
        let text = error.diagnostic().detail.unwrap_or_default();
        assert!(text.contains(path), "{text}");
        assert!(text.contains("invalid type"), "{text}");
    }
    let secret = "ghp_TOKEN_SENTINEL";
    let mut doc = node("role", json!("fine"));
    doc["nodes"][0]["access"] = json!(secret);
    let error = super::decode_input::<GraphDocument>(doc, "").expect_err("decode must fail");
    let wire = serde_json::to_string(&error).unwrap();
    assert!(wire.contains(secret), "{wire}");
    assert!(wire.contains("nodes[0].access"), "{wire}");
    // Caller-selected map keys remain visible in the field path.
    for key in ["foo.secret", "k[0]secret", "userkey"] {
        #[derive(Debug, serde::Deserialize)]
        #[allow(dead_code, reason = "decoded only to provoke a typed map failure")]
        struct Doc {
            nodes: Vec<Node>,
        }
        #[derive(Debug, serde::Deserialize)]
        #[allow(dead_code, reason = "decoded only to provoke a typed map failure")]
        struct Node {
            attempt: Attempt,
        }
        #[derive(Debug, serde::Deserialize)]
        #[allow(dead_code, reason = "decoded only to provoke a typed map failure")]
        struct Attempt {
            extra: BTreeMap<String, u8>,
        }
        let doc = json!({"nodes":[{"attempt":{"extra":{key: "not a number"}}}]});
        let error = super::decode_input::<Doc>(doc, "").expect_err("decode must fail");
        let wire = serde_json::to_string(&error).unwrap();
        assert!(wire.contains(key), "{wire}");
        let back: FacadeError = serde_json::from_str(&wire).unwrap();
        assert_eq!(back, error);
        assert_eq!(serde_json::to_string(&back).unwrap(), wire);
        let text = back.diagnostic().detail.unwrap_or_default();
        assert!(text.contains("nodes[0].attempt.extra"), "{text}");
    }
    Ok(())
}

#[test]
fn relay_refusals_preserve_named_code_and_detail() -> Result<()> {
    for (code, detail) in [
        ("parent_settled", "parent has already settled"),
        ("child_depth", "child exceeds limits.child_depth=2"),
    ] {
        let error = super::relay_error(ServiceError::Cli {
            code: code.into(),
            message: detail.into(),
            data: Value::Null,
        });
        assert_eq!(error.code(), code);
        assert!(matches!(
            &error,
            FacadeError::GraphRelayRefused {
                detail: Some(InvalidInputDetail::GraphReason(_)),
                ..
            }
        ));
        let diagnostic = error.diagnostic();
        assert_eq!(diagnostic.code, code);
        assert!(
            diagnostic
                .detail
                .as_deref()
                .is_some_and(|text| text.contains(detail))
        );
        assert!(serde_json::to_string(&error)?.contains(detail));
        let restored: FacadeError = serde_json::from_value(serde_json::to_value(&error)?)?;
        assert_eq!(restored.code(), code);
        assert_eq!(restored.diagnostic(), diagnostic);
        let structured = ServiceError::Facade(error).structured_error();
        assert_eq!(structured["code"], code);
        assert!(
            structured["message"]
                .as_str()
                .unwrap_or_default()
                .contains(detail)
        );
    }
    Ok(())
}

#[test]
fn relay_upgrade_refusal_retains_safe_drain_guidance() -> Result<()> {
    let error = super::relay_error(ServiceError::Cli {
        code: "UPGRADE_DRAIN_REQUIRED".into(),
        message: "PROMPT_SENTINEL private source text".into(),
        data: json!({"private": "PROMPT_SENTINEL"}),
    });
    let wire = serde_json::to_value(&error)?;
    assert!(!serde_json::to_string(&wire)?.contains("PROMPT_SENTINEL"));
    let restored: FacadeError = serde_json::from_value(wire)?;
    assert_eq!(restored, error);
    let diagnostic = restored.diagnostic();
    assert_eq!(diagnostic.code, "UPGRADE_DRAIN_REQUIRED");
    assert_eq!(
        diagnostic.detail.as_deref(),
        Some(
            "Finish or cancel old pending graphs and drain live lanes with the previous release before admitting new work."
        )
    );
    assert!(
        diagnostic
            .next
            .as_deref()
            .ok_or("drain repair")?
            .contains("--yes cannot bypass")
    );
    let structured = ServiceError::Facade(restored).structured_error();
    assert_eq!(structured["code"], "UPGRADE_DRAIN_REQUIRED");
    assert!(
        structured["message"]
            .as_str()
            .ok_or("drain message")?
            .contains("previous release")
    );
    assert!(!serde_json::to_string(&structured)?.contains("PROMPT_SENTINEL"));
    Ok(())
}

use super::*;
use crate::worktree_tests::support::{Result, TestRig, git};

struct Workers;
impl WorkerObserver for Workers {
    fn observe_roots(
        &self,
        _: &[PathBuf],
    ) -> BTreeMap<PathBuf, provenant_confinement::worktrees::Occupancy> {
        BTreeMap::new()
    }
}

fn bind_fixture_project(roots: &provenant_core::Roots, path: &Path) -> Result {
    let project = std::fs::canonicalize(path)?;
    let common_dir = std::fs::canonicalize(project.join(".git"))?;
    let location =
        provenant_config::ProjectLocation::from_verified_primary_checkout(common_dir, &project)?;
    provenant_config::bind_project(roots, location)?;
    Ok(())
}

/// Declare the one forge context a delivery preview needs in a local-only
/// fixture. The batch resolves it lazily and never guesses a remote or
/// default branch, so a graph that checks a delivery target must declare
/// both. Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
fn declare_forge(rig: &TestRig, default_branch: &str) -> Result {
    std::fs::create_dir_all(rig.project.join(".provenant"))?;
    std::fs::write(
        rig.project.join(provenant_config::PORTS_CONFIG),
        serde_json::to_vec(&json!({
            "schema_version": provenant_config::PORTS_SCHEMA_VERSION,
            "git": {"remote":"origin", "default_branch":default_branch, "protected":[]},
        }))?,
    )?;
    Ok(())
}

pub(super) fn factory(rig: &TestRig) -> Result<Factory> {
    let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
    // Library unit tests inherit the lane environment, where Context::open
    // requires a project binding instead of creating one as a CLI would.
    bind_fixture_project(&roots, &rig.project)?;
    Ok(Factory {
        context: Context::open(roots, &rig.home, &rig.project)?,
        project_attribution: false,
        clock: SystemClock,
        backoff: ThreadPause,
        executor: SystemExecutor::new(1)?,
        home: rig.home.clone(),
        cwd: rig.project.clone(),
        assets: rig.project.clone(),
        host_launch: None,
        workers: &Workers,
    })
}

/// Spec: docs/specs/cli.md#ci-observations -- a fake forge PR
/// with a failing check binds the head branch and base and carries the
/// failure into the launch brief.
#[test]
fn implement_pr_repair_binds_branch_base_and_carries_failing_checks() -> Result {
    let rig = TestRig::new()?;
    let factory = factory(&rig)?;
    let input = json!({
        "op": "run",
        "template": "implement",
        "brief": "Repair the pull request",
        "pr": 7,
    });
    let repaired = factory.apply_pr_repair_with(input, |number| {
        assert_eq!(number, 7);
        Ok(super::PrRepairFact {
            branch: "fix/ci".into(),
            base: "main".into(),
            failures: "CI failures for pull request #7:\n- test: assertion failed at src/lib.rs:42"
                .into(),
        })
    })?;
    assert_eq!(repaired["branch"].as_str(), Some("fix/ci"));
    assert_eq!(repaired["base"].as_str(), Some("main"));
    let brief = repaired["brief"].as_str().ok_or("brief")?;
    assert!(brief.contains("Repair the pull request"));
    assert!(brief.contains("test: assertion failed at src/lib.rs:42"));
    assert!(repaired.get("pr").is_none(), "the PR pin was consumed");
    // An explicit branch or base wins over the forge answer.
    let pinned = json!({
        "op": "run", "template": "implement", "brief": "Repair",
        "pr": 7, "branch": "chosen", "base": "refs/remotes/origin/main",
    });
    let repaired = factory.apply_pr_repair_with(pinned, |_| {
        Ok(super::PrRepairFact {
            branch: "fix/ci".into(),
            base: "main".into(),
            failures: String::new(),
        })
    })?;
    assert_eq!(repaired["branch"].as_str(), Some("chosen"));
    assert_eq!(repaired["base"].as_str(), Some("refs/remotes/origin/main"));
    assert_eq!(repaired["brief"].as_str(), Some("Repair"));
    // A review template keeps its own subject path; the repair is inert.
    let review = json!({"op": "run", "template": "review", "pr": 7});
    let unchanged = factory.apply_pr_repair_with(review.clone(), |_| {
        panic!("a review template must not resolve a PR repair")
    })?;
    assert_eq!(unchanged, review);
    Ok(())
}

// Spec: docs/specs/supervisor.md#native-environment-and-sandbox-tool-stores
#[test]
fn graph_advance_uses_requester_path_after_submission_and_retry() -> Result {
    if super::super::peer_tests::host_test(
        "fabric::interface::tests::graph_advance_uses_requester_path_after_submission_and_retry",
    )? {
        return Ok(());
    }
    use std::os::unix::ffi::OsStrExt;
    let rig = TestRig::new()?;
    let mut factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
    let mut fixture: Value = serde_json::from_str(include_str!(
        "../../crates/fabric-runtime/src/fixtures/graph-checkpoint.json"
    ))?;
    let mut pending = fixture["checkpoint"]["nodes"][0].clone();
    pending["id"] = json!("review");
    pending["state"] = json!("pending");
    pending["attempts"] = json!([]);
    fixture["checkpoint"]["nodes"]
        .as_array_mut()
        .ok_or("checkpoint nodes")?
        .push(pending);
    let mut pending = fixture["execution"]["document"]["nodes"][0].clone();
    pending["id"] = json!("review");
    pending["prompt"] = json!("work for review");
    fixture["execution"]["document"]["nodes"]
        .as_array_mut()
        .ok_or("document nodes")?
        .push(pending);
    let mut checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint =
        serde_json::from_value(fixture["checkpoint"].clone())?;
    let document: GraphDocument = serde_json::from_value(fixture["execution"]["document"].clone())?;
    let p = rig.home.join("requester-p");
    let q = rig.home.join("requester-q");
    std::fs::create_dir_all(&p)?;
    std::fs::create_dir_all(&q)?;
    let p = std::fs::canonicalize(p)?.as_os_str().as_bytes().to_vec();
    let q_directory = std::fs::canonicalize(q)?;
    let q = q_directory.as_os_str().as_bytes().to_vec();
    let mut journal = FileJournal::open(
        &factory.context.roots.state,
        factory.context.identity.project,
        &document.graph_id,
        budget,
        &cancel,
        &factory.clock,
    )?;
    journal.bind(provenant_fabric_runtime::graph::GraphExecution {
        peer_origin_context: None,
        peer_submission_digest: None,
        requester_path: Some(p.clone()),
        extra: Default::default(),
        bases: None,
        directory: factory.cwd.clone(),
        document: document.clone(),
    })?;
    journal.prepare_fixture_checkpoint(&mut checkpoint)?;
    journal.save(&checkpoint)?;
    drop(journal);
    assert!(
        factory
            .graph_target(&document.graph_id)?
            .is_some_and(|target| target.node.is_none())
    );
    let mut supervisor = super::super::settlement::recovery_launch(
        factory.context.roots.state.as_path(),
        &factory.cwd,
    )?;
    supervisor.lane_path = Some(b"/usr/bin".to_vec());
    super::super::run_owner::restore_graph_lane_path(
        &factory,
        &document.graph_id,
        &mut supervisor,
    )?;
    assert_eq!(supervisor.lane_path.as_deref(), Some(p.as_slice()));

    let mut requester = supervisor.clone();
    requester.lane_path = None;
    requester.executable_search_path = vec![q_directory];
    factory.host_launch = Some(requester.clone());
    // The subsequent resume may refuse for unrelated fixture reasons; the
    // retry request must have persisted its PATH before that advance.
    let _ = factory.graph_document(
        json!({"op":"retry", "id":document.graph_id}),
        budget,
        &cancel,
    );
    supervisor.lane_path = Some(b"/usr/bin".to_vec());
    super::super::run_owner::restore_graph_lane_path(
        &factory,
        &document.graph_id,
        &mut supervisor,
    )?;
    assert_eq!(supervisor.lane_path.as_deref(), Some(q.as_slice()));

    let before = FileJournal::execution(
        &factory.context.roots.state,
        factory.context.identity.project,
        &document.graph_id,
    )?
    .ok_or("bound graph execution")?;
    let before_checkpoint = FileJournal::read_checkpoint(
        &factory.context.roots.state,
        factory.context.identity.project,
        &document.graph_id,
    )?;
    let mut unavailable_git = requester.clone();
    unavailable_git.git = rig.root.join("missing-git");
    factory.host_launch = Some(unavailable_git);
    for request in [
        json!({"op":"retry", "id":document.graph_id}),
        json!({"op":"retry", "id":document.graph_id, "node":"review"}),
        json!({"op":"retry", "id":document.graph_id, "node":"review", "brief":"repair review"}),
        json!({"op":"retry", "id":document.graph_id, "extend":["budget.per_graph.tokens=30"]}),
    ] {
        let error = factory
            .graph_document(request.clone(), budget, &cancel)
            .expect_err("PATH capture failure must refuse retry");
        assert!(
            matches!(
                error,
                FacadeError::Io { ref operation, .. } if operation == "capture requester PATH"
            ),
            "unexpected retry error for {request}: {error:?}"
        );
        let after = FileJournal::execution(
            &factory.context.roots.state,
            factory.context.identity.project,
            &document.graph_id,
        )?
        .ok_or("bound graph execution after refused retry")?;
        assert_eq!(after, before);
        assert_eq!(
            FileJournal::read_checkpoint(
                &factory.context.roots.state,
                factory.context.identity.project,
                &document.graph_id,
            )?,
            before_checkpoint
        );
    }

    requester.lane_path = Some(Vec::new());
    factory.host_launch = Some(requester);
    assert!(factory.requester_lane_path()?.is_empty());
    Ok(())
}

#[test]
fn admission_refusals_preserve_publication() -> Result {
    if super::super::peer_tests::host_test(
        "fabric::interface::tests::admission_refusals_preserve_publication",
    )? {
        return Ok(());
    }
    let rig = TestRig::new()?;
    let factory = factory(&rig)?;
    let project = factory.context.identity.project;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
    let fixture: Value = serde_json::from_str(include_str!(
        "../../crates/fabric-runtime/src/fixtures/graph-checkpoint.json"
    ))?;
    let mut checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint =
        serde_json::from_value(fixture["checkpoint"].clone())?;
    let mut execution_value = fixture["execution"]["document"].clone();
    execution_value["schema_version"] =
        json!(crate::worktree_tests::support::GRAPH_DOCUMENT_SCHEMA_VERSION);
    let execution: GraphDocument = serde_json::from_value(execution_value)?;
    let directory: PathBuf = factory
        .context
        .roots
        .state
        .as_path()
        .join("fabric/graphs")
        .join(project.to_string());
    let mut journal = FileJournal::open(
        &factory.context.roots.state,
        project,
        &execution.graph_id,
        budget,
        &cancel,
        &factory.clock,
    )?;
    journal.bind(provenant_fabric_runtime::graph::GraphExecution {
        peer_origin_context: None,
        peer_submission_digest: None,
        requester_path: None,
        extra: Default::default(),
        bases: None,
        directory: PathBuf::from("/legacy/project"),
        document: execution,
    })?;
    journal.prepare_fixture_checkpoint(&mut checkpoint)?;
    journal.save(&checkpoint)?;
    drop(journal);
    let path = directory.join("graph-g-test.json");
    let before = std::fs::read(&path)?;

    let marker = "REFUSAL_MARKER_3391";
    let secret = format!("sk-abcdefghijklmnopqrstuvwx {marker}");
    let _scope = provenant_redact::publication_scope([secret.as_bytes().to_vec()])?;
    let mut keyed = json!({"id":"reader", "prompt":"safe", "route":{"alias":"senior"}});
    keyed[marker] = json!(secret);
    // Spec: docs/specs/fabric.md#graph-admission-and-refusal-causes
    for (path_name, request) in [
        (
            "launch",
            json!({"op":"run", "document":{
                "schema_version":99, "graph_id":"refused-launch",
                "context":{marker:secret},
                "nodes":[{"id":"inspect", "prompt":"safe", "access":"read_only", "route":{"alias":"senior"}}]
            }}),
        ),
        (
            "edit",
            json!({"op":"edit", "id":"g-test", "patch":{"schema_version":99, "replace":[keyed]}}),
        ),
        (
            "extension",
            json!({"op":"retry", "id":"g-test", "extend":[format!("per_graph.tokens={secret}")]}),
        ),
        (
            "limit",
            json!({"op":"retry", "id":"g-test", "extend":[format!("limits.nodes={secret}")]}),
        ),
        (
            "input_count",
            json!({"op":"retry", "id":"g-test", "extend":vec!["loop.none.rounds=2";65]}),
        ),
        (
            "input_bytes",
            json!({"op":"retry", "id":"g-test", "extend":[format!("loop.{}.rounds=2", "x".repeat(257))]}),
        ),
    ] {
        let error = factory
            .graph_document(request, budget, &cancel)
            .expect_err(path_name);
        if path_name == "extension" {
            assert!(
                error.to_string().contains("per_graph.tokens"),
                "unexpected extension refusal cause: {error}"
            );
        }
        if path_name == "limit" {
            assert!(error.to_string().contains("limits.nodes"));
        }
        // Check the complete envelope, including the requester's PATH, byte for byte.
        assert_eq!(std::fs::read(&path)?, before, "{path_name}");
        assert!(
            FileJournal::read_checkpoint(&factory.context.roots.state, project, "refused-launch")?
                .is_none()
        );
    }
    let error = factory
        .graph_document(
            json!({"op":"retry","id":"g-test/reader","node":"other","brief":"Replacement"}),
            budget,
            &cancel,
        )
        .expect_err("conflicting retry targets must be refused");
    assert!(matches!(error, FacadeError::InvalidInput { .. }));
    assert_eq!(std::fs::read(&path)?, before);
    assert_eq!(
        FileJournal::read_checkpoint(&factory.context.roots.state, project, "g-test")?,
        Some(checkpoint)
    );
    Ok(())
}

// Spec: docs/specs/fabric.md#graph-admission-and-refusal-causes
#[test]
fn tightened_retry_limit_preserves_the_checkpoint_envelope() -> Result {
    if super::super::peer_tests::host_test(
        "fabric::interface::tests::tightened_retry_limit_preserves_the_checkpoint_envelope",
    )? {
        return Ok(());
    }
    let rig = TestRig::new()?;
    let mut factory = factory(&rig)?;
    let project = factory.context.identity.project;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
    let fixture: Value = serde_json::from_str(include_str!(
        "../../crates/fabric-runtime/src/fixtures/graph-checkpoint.json"
    ))?;
    let mut checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint =
        serde_json::from_value(fixture["checkpoint"].clone())?;
    let mut document_value = fixture["execution"]["document"].clone();
    document_value["schema_version"] =
        json!(crate::worktree_tests::support::GRAPH_DOCUMENT_SCHEMA_VERSION);
    let document: GraphDocument = serde_json::from_value(document_value)?;
    let mut journal = FileJournal::open(
        &factory.context.roots.state,
        project,
        &document.graph_id,
        budget,
        &cancel,
        &factory.clock,
    )?;
    journal.bind(provenant_fabric_runtime::graph::GraphExecution {
        peer_origin_context: None,
        peer_submission_digest: None,
        requester_path: None,
        extra: Default::default(),
        bases: None,
        directory: factory.cwd.clone(),
        document: document.clone(),
    })?;
    journal.prepare_fixture_checkpoint(&mut checkpoint)?;
    journal.save(&checkpoint)?;
    drop(journal);
    let path = factory
        .context
        .roots
        .state
        .as_path()
        .join("fabric/graphs")
        .join(project.to_string())
        .join("graph-g-test.json");
    let before = std::fs::read(&path)?;
    // A candidate that passed under the old policy must be checked again
    // after the host tightens its limit, before publishing the retry.
    let request = json!({"op":"retry", "id":"g-test/reader", "brief":"new work"});
    let mut staged = checkpoint.clone();
    let mut target = factory
        .graph_target("g-test/reader")?
        .ok_or("retry target")?;
    factory.prepare_retry(&mut target, &request, &document, &mut staged)?;
    provenant_fabric_runtime::graph::admit_candidate(
        &document,
        Some(&staged),
        &[],
        &factory.context.config.graph.limits,
        &factory.context.config.graph.budget,
        None,
    )?;
    factory.context.config.graph.limits.node_prompt_bytes = 1;
    let error = factory
        .graph_document(
            json!({"op":"retry", "id":"g-test/reader", "brief":"new work"}),
            budget,
            &cancel,
        )
        .expect_err("tightened limit must refuse");
    assert!(
        error.to_string().contains("node_prompt_bytes"),
        "unexpected tightened-limit refusal cause: {error}"
    );
    assert_eq!(std::fs::read(path)?, before);
    Ok(())
}

#[test]
fn stop_reports_pending_acknowledgement_without_the_graph_lock() -> Result {
    if super::super::peer_tests::host_test(
        "fabric::interface::tests::stop_reports_pending_acknowledgement_without_the_graph_lock",
    )? {
        return Ok(());
    }
    let rig = TestRig::new()?;
    let factory = factory(&rig)?;
    let project = factory.context.identity.project;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
    let fixture: Value = serde_json::from_str(include_str!(
        "../../crates/fabric-runtime/src/fixtures/graph-checkpoint.json"
    ))?;
    let mut checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint =
        serde_json::from_value(fixture["checkpoint"].clone())?;
    checkpoint.nodes[0].state = provenant_fabric_runtime::graph::NodeState::Running;
    checkpoint.nodes[0].attempts[0].state = provenant_fabric_runtime::graph::AttemptState::Running;
    checkpoint.nodes[0].attempts[0].outcome = None;
    let execution: GraphDocument =
        serde_json::from_value(fixture["execution"]["document"].clone())?;
    let mut journal = FileJournal::open(
        &factory.context.roots.state,
        project,
        &execution.graph_id,
        budget,
        &cancel,
        &factory.clock,
    )?;
    journal.bind(provenant_fabric_runtime::graph::GraphExecution {
        peer_origin_context: None,
        peer_submission_digest: None,
        requester_path: None,
        extra: Default::default(),
        bases: None,
        directory: PathBuf::from("/legacy/project"),
        document: execution,
    })?;
    journal.prepare_fixture_checkpoint(&mut checkpoint)?;
    journal.save(&checkpoint)?;

    let receipt = factory.graph_document(
        json!({"op":"stop","id":"g-test","wait_seconds":0}),
        budget,
        &cancel,
    )?;
    assert_eq!(receipt["cancellation_requested"]["acknowledged"], false);
    assert_eq!(
        receipt["cancellation_requested"]["pending_acknowledgement"],
        true
    );
    assert_eq!(receipt["timed_out"], true);
    assert_eq!(
        FileJournal::read_checkpoint(&factory.context.roots.state, project, "g-test")?
            .ok_or("checkpoint missing")?,
        checkpoint
    );
    Ok(())
}

#[test]
fn child_unknown_parameter_keeps_the_compiler_refusal() -> Result {
    let rig = TestRig::new()?;
    let factory = factory(&rig)?;
    let error = factory
        .child_request(&json!({
            "op":"run", "brief":"Inspect", "set":{"bogus":true}
        }))
        .expect_err("task has no bogus parameter");
    let FacadeError::InvalidInput {
        detail: Some(InvalidInputDetail::Parameter(refusal)),
    } = &error
    else {
        return Err("missing parameter refusal".into());
    };
    assert_eq!(refusal.template, "task");
    assert!(
        matches!(&refusal.fault, provenant_fabric_runtime::graph::template::ParameterFault::Unknown { key: Some(key), accepted, .. } if key == "bogus" && accepted.is_empty()),
        "{refusal:?}"
    );
    let diagnostic = error.diagnostic();
    assert_eq!(diagnostic.code, "invalid_input");
    let detail = diagnostic.detail.as_deref().unwrap_or_default();
    assert!(
        detail.contains("template task has no parameter bogus"),
        "{detail}"
    );
    assert!(
        detail.contains("template task takes no parameters"),
        "{detail}"
    );
    let restored: FacadeError = serde_json::from_value(serde_json::to_value(&error)?)?;
    assert_eq!(restored.diagnostic(), diagnostic);
    Ok(())
}

#[test]
fn branch_control_uses_the_project_worktree_layout() -> Result {
    let project = tempfile::tempdir()?;
    std::fs::create_dir(project.path().join(".provenant"))?;
    let mut ports = provenant_config::PortTable::builtin();
    ports.worktree_layout.roots = vec![
        std::path::PathBuf::from(".claude/worktrees"),
        std::path::PathBuf::from(".worktrees"),
    ];
    ports.worktree_layout.default = std::path::PathBuf::from(".claude/worktrees/<branch>");
    std::fs::write(
        project.path().join(provenant_config::PORTS_CONFIG),
        serde_json::to_vec(&ports)?,
    )?;

    let ports = provenant_config::PortTable::read_for(Some(project.path()), project.path())?;
    let mut node = json!({"use":"task"});
    bind_launch_controls(
        &json!({"branch":"feature/topic"}),
        &mut node,
        project.path(),
        &ports,
    )?;
    assert_eq!(
        node["worktree"],
        json!(
            project
                .path()
                .join(ports.managed_writer_path("feature/topic")?)
        ),
    );
    Ok(())
}

#[test]
fn branch_without_base_uses_the_remote_default_branch() -> Result {
    let mut node = json!({"use":"task"});
    bind_launch_controls(
        &json!({"branch":"topic"}),
        &mut node,
        Path::new("/repo"),
        &provenant_config::PortTable::builtin(),
    )?;
    assert_eq!(node["worktree"], "/repo/.worktrees/topic");
    assert_eq!(
        node["attempt"]["new_worktree"]["base"],
        "refs/remotes/origin/HEAD"
    );
    assert_eq!(node["attempt"]["new_worktree"]["default_base"], true);
    let mut explicit = json!({"use":"task"});
    bind_launch_controls(
        &json!({"branch":"topic", "base":"refs/remotes/origin/HEAD"}),
        &mut explicit,
        Path::new("/repo"),
        &provenant_config::PortTable::builtin(),
    )?;
    assert!(
        explicit["attempt"]["new_worktree"]
            .get("default_base")
            .is_none()
    );
    assert!(
        bind_launch_controls(
            &json!({"base":"HEAD"}),
            &mut node,
            Path::new("/repo"),
            &provenant_config::PortTable::builtin()
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn a_named_child_replays_by_name_and_input() -> Result {
    let rig = TestRig::new()?;
    let factory = factory(&rig)?;
    let input = json!({"op":"run", "brief":"Inspect", "name":"inspect-parser"});
    let first = factory.child_request(&input)?;
    let replay = factory.child_request(&input)?;
    assert_eq!(first.id, "inspect-parser");
    assert_eq!(first.id, replay.id);
    assert_eq!(first.idempotency_key, replay.idempotency_key);
    assert_eq!(first.launch, replay.launch);
    let changed = factory
        .child_request(&json!({"op":"run", "brief":"Inspect more", "name":"inspect-parser"}))?;
    assert_ne!(first.idempotency_key, changed.idempotency_key);
    // Unnamed children stay distinct.
    let unnamed = json!({"op":"run", "brief":"Inspect"});
    assert_ne!(
        factory.child_request(&unnamed)?.idempotency_key,
        factory.child_request(&unnamed)?.idempotency_key
    );
    assert!(
        factory
            .child_request(&json!({"op":"run", "name":"Invalid Name"}))
            .is_err()
    );
    Ok(())
}

// Spec: docs/specs/routing.md#pinned-review-selection
#[test]
fn review_fallback_warns_range_and_keeps_empty_context_free() -> Result {
    if super::super::peer_tests::host_test(
        "fabric::interface::tests::review_fallback_warns_range_and_keeps_empty_context_free",
    )? {
        return Ok(());
    }
    let rig = crate::worktree_tests::repository()?;
    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    let input = json!({"op":"run","template":"review.correctness","brief":"Review","explain":true});
    let response = factory.graph_document(input.clone(), budget, &cancel)?;
    assert!(response["document"]["review_base"].is_null());
    assert_eq!(response["document"]["review_subject_missing"], true);
    let child = factory
        .child_request(&input)?
        .launch
        .ok_or("missing child")?
        .node;
    assert!(child["review_base"].is_null());
    assert!(child["source"].is_null());
    assert!(
        child["prompt"]
            .as_str()
            .ok_or("prompt")?
            .contains("empty range")
    );
    let base = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
    crate::worktree_tests::git(&rig, &["commit", "--allow-empty", "-m", "tip"])?;
    let head = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
    crate::worktree_tests::git(&rig, &["update-ref", "refs/remotes/origin/main", &head])?;
    crate::worktree_tests::git(
        &rig,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    )?;
    let response = factory.graph_document(input.clone(), budget, &cancel)?;
    assert_eq!(response["document"]["review_base"], base);
    assert_eq!(response["document"]["nodes"][0]["source"]["commit"], head);
    assert!(
        response["warnings"]
            .as_array()
            .ok_or("warnings")?
            .iter()
            .any(|warning| {
                warning.as_str().is_some_and(|warning| {
                    warning.contains(&format!("{base}...{head}"))
                        && warning.contains("first parent")
                })
            })
    );
    let child = factory
        .child_request(&input)?
        .launch
        .ok_or("missing child")?
        .node;
    assert_eq!(child["review_base"], base);
    assert_eq!(child["source"]["commit"], head);
    Ok(())
}

// Spec: docs/specs/routing.md#pinned-review-selection
#[test]
fn review_pr_pins_merge_branch_from_any_launch_checkout() -> Result {
    if super::super::peer_tests::host_test(
        "fabric::interface::tests::review_pr_pins_merge_branch_from_any_launch_checkout",
    )? {
        return Ok(());
    }
    let rig = crate::worktree_tests::repository()?;
    let git = |args: &[&str]| crate::worktree_tests::git(&rig, args);
    git(&["checkout", "-B", "main"])?;
    git(&["checkout", "-b", "feature"])?;
    std::fs::write(rig.project.join("feature.txt"), "feature\n")?;
    git(&["add", "feature.txt"])?;
    git(&["commit", "-m", "feature"])?;
    git(&["checkout", "main"])?;
    std::fs::write(rig.project.join("merged.txt"), "merged main\n")?;
    git(&["add", "merged.txt"])?;
    git(&["commit", "-m", "main change"])?;
    git(&["checkout", "feature"])?;
    git(&["merge", "--no-ff", "main", "-m", "merge main"])?;
    let head = git(&["rev-parse", "HEAD"])?;
    git(&["checkout", "main"])?;
    std::fs::write(rig.project.join("later.txt"), "later main\n")?;
    git(&["add", "later.txt"])?;
    git(&["commit", "-m", "later main"])?;
    let base = git(&["rev-parse", "HEAD"])?;
    let metadata = rig.root.join("pull.json");
    std::fs::write(
        &metadata,
        serde_json::to_vec(
            &json!({"baseRefOid":base,"baseRefName":"main","headRefOid":head,"url":"https://github.com/owner/repo/pull/7"}),
        )?,
    )?;
    // The PR subject is read through the one CI observation transport, so
    // the command port must answer its repository and pull paths. A body is
    // a JSON string inside the typed port response; prepare those bytes in
    // the fixture rather than escaping them in a shell script.
    // Spec: docs/specs/cli.md#ci-observations
    let repository_response = rig.root.join("repository-response.json");
    std::fs::write(
        &repository_response,
        serde_json::to_vec(&json!({
            "status":200, "headers":{},
            "body": json!({"full_name":"owner/repo"}).to_string(),
        }))?,
    )?;
    let pull_response = rig.root.join("pull-response.json");
    std::fs::write(
        &pull_response,
        serde_json::to_vec(&json!({
            "status":200, "headers":{},
            "body": json!({
                "base":{"ref":"main","sha":base},
                "head":{"ref":"feature","sha":head},
                "html_url":"https://github.com/owner/repo/pull/7",
            }).to_string(),
        }))?,
    )?;
    let observation_script = concat!(
        "case \"$PROVENANT_PORT_REQUEST_PATH\" in\n",
        "  'repos/{owner}/{repo}') /bin/cat \"$1\" ;;\n",
        "  'repos/owner/repo/pulls/7') /bin/cat \"$2\" ;;\n",
        "  *) exit 71 ;;\n",
        "esac\n",
    );
    std::fs::write(
        rig.state.join("ports.json"),
        serde_json::to_vec(&json!({
            "schema_version":provenant_config::PORTS_SCHEMA_VERSION,
            "forge":{"source":"host","backend":{"kind":"command","command":{
                "argv":["/bin/sh","-c","test \"$2\" = pr && test \"$3\" = view && test \"$4\" = 7 && /bin/cat \"$1\"","fixture",metadata],
                "exit_codes":{"success":[0],"unknown":[75],"other":"refused"}
            }}},
            "ci_observation":{"source":"host","backend":{"kind":"command","command":{
                "argv":["/bin/sh","-c",observation_script,"fixture",repository_response,pull_response],
                "exit_codes":{"success":[0],"unknown":[75],"other":"refused"}
            }}}
        }))?,
    )?;
    for checkout in ["main", "feature"] {
        git(&["checkout", checkout])?;
        let factory = factory(&rig)?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        for template in ["review.correctness", "review-panel"] {
            let request =
                json!({"op":"run","template":template,"brief":"Review","pr":7,"explain":true});
            let response = factory.graph_document(request.clone(), budget, &cancel)?;
            assert_eq!(response["document"]["review_base"], base);
            let frame = factory.relay_document(&request, budget, &cancel)?;
            assert_eq!(frame["op"], "run");
            assert_eq!(frame["input"]["pr"], 7);
            let git_path = provenant_os::git::git_executable().ok_or("Git unavailable")?;
            let holder = provenant_run::HolderContext {
                schema_version: 1,
                roots: provenant_run::RootSeed {
                    home: rig.home.clone(),
                    checkout: rig.project.clone(),
                    state_root: rig.state.clone(),
                },
                project_id: factory.context.identity.project,
                actor_id: factory.context.identity.inbox.actor,
                attempt_id: provenant_core::AttemptId::from_bytes(provenant_os::fs::entropy_128()?),
                host_delivery: None,
                sealed: None,
                launch: Some(provenant_run::HostLaunchContext::from_environment(
                    &rig.state,
                    &rig.project,
                    git_path.clone(),
                )),
            };
            let prepared = super::super::read_lane_graph(
                &holder,
                json!({"op":"prepare_child","input":frame["input"]}),
                &cancel,
                std::time::Duration::from_secs(30),
                &Workers,
            )
            .map_err(|error| format!("host preparation: {error:?}"))?;
            assert_eq!(prepared["launch"]["node"]["review_base"], base);
            if checkout == "main" && template == "review.correctness" {
                let repeated = super::super::read_lane_graph(
                    &holder,
                    json!({"op":"prepare_child","input":frame["input"]}),
                    &cancel,
                    std::time::Duration::from_secs(30),
                    &Workers,
                )
                .map_err(|error| format!("host replay preparation: {error:?}"))?;
                assert_eq!(prepared["id"], repeated["id"]);
                assert_eq!(prepared["idempotency_key"], repeated["idempotency_key"]);
                let distinct = factory.relay_document(&request, budget, &cancel)?;
                assert_ne!(
                    frame["input"]["idempotency_key"],
                    distinct["input"]["idempotency_key"]
                );
                for control in ["name", "idempotency_key"] {
                    let mut named = request.clone();
                    named[control] = json!("stable-review");
                    let first = factory.relay_document(&named, budget, &cancel)?;
                    let retry = factory.relay_document(&named, budget, &cancel)?;
                    assert_eq!(first["request_id"], retry["request_id"]);
                    named["wait_seconds"] = json!(5);
                    let changed_wait = factory.relay_document(&named, budget, &cancel)?;
                    assert_eq!(first["request_id"], changed_wait["request_id"]);
                }
                // The launch receipt names the commit pair the review is pinned to.
                let mut launch = request.clone();
                launch["explain"] = json!(false);
                launch["name"] = json!("review-receipt-pin");
                let receipt = factory.graph_document(launch, budget, &cancel)?;
                assert_eq!(receipt["review_subject"]["base"], base);
                assert_eq!(receipt["review_subject"]["head"], head);
            }
            let document: GraphDocument = serde_json::from_value(response["document"].clone())?;
            let expanded = document.expanded(&factory.context.config.graph.limits)?;
            for node in expanded
                .nodes
                .iter()
                .filter(|node| node.review_behaviour.is_some())
            {
                assert_eq!(serde_json::to_value(&node.source)?["commit"], head);
            }
            let child = factory.child_request(&request)?;
            let another = factory.child_request(&request)?;
            assert_ne!(child.id, another.id);
            assert_ne!(child.idempotency_key, another.idempotency_key);
            let child_node = child.launch.ok_or("missing launch")?.node;
            assert_eq!(child_node["review_base"], base);
            let git_path = provenant_os::git::git_executable().ok_or("Git unavailable")?;
            let context =
                provenant_run::review_diff_context_at(&git_path, &rig.project, &base, &head)?;
            assert_eq!(context.paths, ["feature.txt"]);
        }
        // `--pr` against a template with no review range is advisory
        // (issue #3551): it warns with its source and proceeds with the
        // host's default review shape instead of refusing the submission.
        {
            let mut advisory = json!({"template":"task","pr":7});
            advisory["op"] = json!("run");
            advisory["explain"] = json!(true);
            let response = factory.graph_document(advisory, budget, &cancel)?;
            assert!(
                response["warnings"]
                    .as_array()
                    .ok_or("warnings")?
                    .iter()
                    .any(|warning| {
                        warning.as_str().is_some_and(|warning| {
                            warning.contains("--pr was ignored")
                                && warning.contains("source: template review range")
                        })
                    }),
                "warnings: {}",
                response["warnings"]
            );
        }
        for invalid in [
            json!({"template":"review","pr":0}),
            json!({"template":"review","pr":7,"set":{"subject":head,"review_base":base}}),
        ] {
            let mut invalid = invalid;
            invalid["op"] = json!("run");
            invalid["explain"] = json!(true);
            assert!(
                factory
                    .graph_document(invalid.clone(), budget, &cancel)
                    .is_err()
            );
            assert!(factory.child_request(&invalid).is_err());
        }
    }
    Ok(())
}

#[test]
fn review_ref_binds_tip_until_launch() -> Result {
    if super::super::peer_tests::host_test(
        "fabric::interface::tests::review_ref_binds_tip_until_launch",
    )? {
        return Ok(());
    }
    let rig = crate::worktree_tests::repository()?;
    let base = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
    crate::worktree_tests::git(&rig, &["checkout", "-b", "review-subject"])?;
    crate::worktree_tests::git(&rig, &["commit", "--allow-empty", "-m", "subject"])?;
    let head = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
    crate::worktree_tests::git(&rig, &["checkout", "-b", "review-target", &base])?;
    crate::worktree_tests::git(&rig, &["commit", "--allow-empty", "-m", "target advanced"])?;
    let tip = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    let input = json!({"op":"run","name":"pinned-ref","template":"review","brief":"Review","set":{"subject":head,"review_base":"review-target"},"explain":true});
    let result = factory.graph_document(input.clone(), budget, &cancel)?;
    let child = factory.child_request(&input)?;
    assert_eq!(result["document"]["review_base"], tip);
    crate::worktree_tests::git(
        &rig,
        &["commit", "--allow-empty", "-m", "target advanced again"],
    )?;
    let advanced = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
    let changed = factory.graph_document(input.clone(), budget, &cancel)?;
    let changed_child = factory.child_request(&input)?;
    assert_eq!(changed["document"]["review_base"], advanced);
    assert_ne!(
        result["document"]["replay_digest"],
        changed["document"]["replay_digest"]
    );
    assert_ne!(child.idempotency_key, changed_child.idempotency_key);
    assert_eq!(child.launch.ok_or("child")?.node["review_base"], tip);
    Ok(())
}

// Spec: docs/specs/routing.md#pinned-review-selection
#[test]
fn pinned_review_expansion_applies_path_floors_and_keeps_docs_quick() -> Result {
    let rig = crate::worktree_tests::repository()?;
    let base = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
    let workflow = rig.project.join(".github/workflows/release.yml");
    std::fs::create_dir_all(workflow.parent().ok_or("workflow parent")?)?;
    std::fs::write(&workflow, "name: Release\n")?;
    let credentials = rig.project.join("credentials/identity.json");
    std::fs::create_dir_all(credentials.parent().ok_or("credentials parent")?)?;
    std::fs::write(&credentials, "{}\n")?;
    crate::worktree_tests::git(
        &rig,
        &[
            "add",
            ".github/workflows/release.yml",
            "credentials/identity.json",
        ],
    )?;
    crate::worktree_tests::git(&rig, &["commit", "-qm", "touch protected review paths"])?;
    let protected_head = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;

    crate::worktree_tests::git(&rig, &["checkout", "-q", "-b", "docs-review", &base])?;
    let documentation = rig.project.join("docs/guide.md");
    std::fs::create_dir_all(documentation.parent().ok_or("documentation parent")?)?;
    std::fs::write(&documentation, "Review selection guide\n")?;
    crate::worktree_tests::git(&rig, &["add", "docs/guide.md"])?;
    crate::worktree_tests::git(&rig, &["commit", "-qm", "touch docs only"])?;
    let docs_head = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;

    let factory = factory(&rig)?;
    // Protected paths resolve to the `crucial` shape; derive the panel size
    // from the resolved defaults rather than a literal lens count.
    let review = provenant_routing::RoutingConfig::default().review;
    let crucial_reviewers: usize = review.shapes[&review.select.by_risk["crucial"]]
        .reviewers
        .iter()
        .map(|group| group.count)
        .sum();
    let cancel = CancelToken::new();
    let caller_brief = "REVIEW_PANEL_CALLER_BRIEF_SENTINEL";
    let explain = |subject: &str, review_base: &str| -> Result<GraphDocument> {
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let result = factory.graph_document(
            json!({
                "op":"run", "template":"review-panel", "brief":caller_brief,
                "explain":true,
                "set":{"subject":subject,"review_base":review_base}
            }),
            budget,
            &cancel,
        )?;
        Ok(serde_json::from_value(result["document"].clone())?)
    };

    let protected = explain(&protected_head, &base)?;
    assert_eq!(protected.review_base.as_deref(), Some(base.as_str()));
    let protected_lenses = &protected.template_resolvers["review-panel"];
    assert!(
        protected_lenses.len() >= crucial_reviewers,
        "{protected_lenses:?}"
    );
    for lens in ["correctness-spec", "reliability", "security-privacy"] {
        assert!(
            protected_lenses.iter().any(|selected| selected == lens),
            "{protected_lenses:?}"
        );
    }
    assert_eq!(
        protected.template_resolvers["review-frame"],
        vec!["true".to_owned()]
    );
    assert_eq!(
        protected.template_resolvers["review-verify"],
        vec!["true".to_owned()]
    );
    assert_eq!(
        protected.template_resolvers["review-synthesis"],
        vec!["true".to_owned()]
    );
    let protected_expanded = protected.expanded(&factory.context.config.graph.limits)?;
    let review_leaves: Vec<_> = protected_expanded
        .nodes
        .iter()
        .filter(|node| node.review_lens.is_some())
        .collect();
    assert!(
        review_leaves.len() >= crucial_reviewers,
        "{} review leaves",
        review_leaves.len()
    );
    assert!(
        review_leaves
            .iter()
            .all(|node| node.prompt.contains(caller_brief))
    );
    for stage in ["frame", "verify", "synthesis"] {
        let node = protected_expanded
            .nodes
            .iter()
            .find(|node| node.id.contains(&format!("--{stage}")))
            .ok_or_else(|| format!("full review panel has no {stage} stage"))?;
        assert!(node.prompt.contains(caller_brief), "{stage} prompt");
    }
    assert!(
        protected_expanded
            .nodes
            .iter()
            .filter(|node| node.review_behaviour.is_some())
            .all(|node| matches!(
                node.source.as_ref(),
                Some(provenant_fabric_runtime::graph::SourceBinding::Commit { commit })
                    if commit == &protected_head
            ))
    );

    let docs = explain(&docs_head, &base)?;
    assert_eq!(docs.template_resolvers["review-panel"].len(), 1);
    assert_eq!(
        docs.template_resolvers["review-frame"],
        vec!["false".to_owned()]
    );
    assert_eq!(
        docs.template_resolvers["review-verify"],
        vec!["false".to_owned()]
    );
    assert_eq!(
        docs.template_resolvers["review-synthesis"],
        vec!["false".to_owned()]
    );

    let tree = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD^{tree}"])?;
    let disconnected_base =
        crate::worktree_tests::git(&rig, &["commit-tree", &tree, "-m", "disconnected base"])?;
    let disconnected_head =
        crate::worktree_tests::git(&rig, &["commit-tree", &tree, "-m", "disconnected head"])?;
    let incomplete = explain(&disconnected_head, &disconnected_base)?;
    let incomplete_lenses = &incomplete.template_resolvers["review-panel"];
    assert!(
        incomplete_lenses.len() >= crucial_reviewers,
        "{incomplete_lenses:?}"
    );
    for lens in ["correctness-spec", "reliability", "security-privacy"] {
        assert!(
            incomplete_lenses.iter().any(|selected| selected == lens),
            "{incomplete_lenses:?}"
        );
    }
    assert!(
        incomplete.template_resolvers["review-advice-correctness-spec"][0]
            .contains("configured unreadable-path floors were applied")
    );
    Ok(())
}

#[test]
fn empty_review_range_is_refused_before_launch() -> Result {
    let rig = crate::worktree_tests::repository()?;
    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    let error = factory.graph_document(json!({"op":"run","template":"review","brief":"Review","set":{"subject":"HEAD","review_base":"HEAD"}}), budget, &cancel).unwrap_err();
    assert!(
        matches!(error, FacadeError::InvalidInput { detail: Some(InvalidInputDetail::Field(ref message)) } if message.contains("review_empty_range"))
    );
    Ok(())
}

fn replay_digest_fixture(name: &str) -> Result<Option<(TestRig, Factory)>> {
    if super::super::peer_tests::host_test(name)? {
        return Ok(None);
    }
    // `Factory::template_library` honours the caller's XDG directory. Run
    // this fixture with an empty user template root so unrelated templates
    // installed on the test host cannot invalidate inline task compilation.
    const FIXTURE: &str = "PROVENANT_TEST_REPLAY_DIGEST_CONFIG_HOME";
    if let Some(config_home) = std::env::var_os(FIXTURE) {
        let config_home = std::path::PathBuf::from(config_home);
        std::fs::create_dir_all(config_home.join("provenant/templates"))?;
    } else {
        use provenant_os::exec::{
            ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination,
        };
        use std::time::Duration;

        let temp = tempfile::tempdir()?;
        let config_home = temp.path().join("config");
        std::fs::create_dir_all(config_home.join("provenant/templates"))?;
        let mut environment: Vec<_> = std::env::vars_os()
            .filter(|(key, _)| key != "XDG_CONFIG_HOME" && key != FIXTURE)
            .collect();
        environment.push((FIXTURE.into(), config_home.as_os_str().to_owned()));
        environment.push(("XDG_CONFIG_HOME".into(), config_home.as_os_str().to_owned()));
        let argv = [
            std::env::current_exe()?.into_os_string(),
            "--exact".into(),
            name.into(),
            "--nocapture".into(),
        ];
        let cwd = std::env::current_dir()?;
        let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(60));
        options.env = Some(&environment);
        options.stdout = StreamPolicy::Capture { limit: 65_536 };
        options.stderr = StreamPolicy::Capture { limit: 65_536 };
        let output = SystemExecutor::new(1)?.execute(options)?;
        assert_eq!(
            output.termination,
            Termination::Exit(0),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout.bytes),
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        assert!(!output.timed_out, "{output:?}");
        super::super::peer_tests::assert_one_test_ran(name, &output.stdout.bytes);
        return Ok(None);
    }
    let rig = crate::worktree_tests::repository()?;
    let factory = factory(&rig)?;
    Ok(Some((rig, factory)))
}

#[test]
fn replay_digest_tracks_launch_input_and_source_commit() -> Result {
    let Some((_rig, factory)) = replay_digest_fixture(
        "fabric::interface::tests::replay_digest_tracks_launch_input_and_source_commit",
    )?
    else {
        return Ok(());
    };
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    let explain = |factory: &Factory, input: Value| -> Result<String> {
        let label = input.to_string();
        let response = factory
            .graph_document(input, budget, &cancel)
            .map_err(|error| format!("{label}: {error:?}"))?;
        Ok(response["document"]["replay_digest"]
            .as_str()
            .ok_or("replay digest")?
            .to_owned())
    };
    let task = json!({"op":"run","brief":"same brief","explain":true});
    let task_digest = explain(&factory, task.clone())?;
    assert_eq!(task_digest, explain(&factory, task.clone())?);
    // The name is a handle: it never changes the digest.
    let mut named = task.clone();
    named["name"] = json!("handle");
    assert_eq!(task_digest, explain(&factory, named)?);
    let inline =
        json!({"op":"run","document":{"brief":"inline task","tier":"senior"},"explain":true});
    assert_eq!(
        explain(&factory, inline.clone())?,
        explain(&factory, inline)?
    );
    // The digest includes launch policy, even when the brief and the
    // source commit are unchanged.
    let controlled = json!({
        "op":"run", "explain":true,
        "document":{"brief":"Inspect launch policy",
            "tier":"senior", "access":"read_only"},
        "attempt":{"host":"local", "confidential":true, "provider_timeout":42}
    });
    let original = explain(&factory, controlled.clone())?;
    assert_eq!(original, explain(&factory, controlled.clone())?);
    for (field, value) in [
        ("host", json!("localhost")),
        ("confidential", json!(false)),
        ("provider_timeout", json!(43)),
    ] {
        let mut changed = controlled.clone();
        changed["attempt"][field] = value;
        let expected: provenant_fabric_runtime::graph::AttemptSpec =
            serde_json::from_value(changed["attempt"].clone())?;
        let response = factory.graph_document(changed.clone(), budget, &cancel)?;
        let document: GraphDocument = serde_json::from_value(response["document"].clone())?;
        assert_ne!(
            response["document"]["replay_digest"]
                .as_str()
                .ok_or("replay digest")?,
            original,
            "attempt.{field}"
        );
        let expanded = document.expanded(&factory.context.config.graph.limits)?;
        assert_eq!(
            expanded.nodes[0].attempt.as_ref(),
            Some(&expected),
            "attempt.{field}"
        );
    }
    Ok(())
}

#[test]
fn replay_digest_tracks_review_subject() -> Result {
    let Some((rig, factory)) =
        replay_digest_fixture("fabric::interface::tests::replay_digest_tracks_review_subject")?
    else {
        return Ok(());
    };
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    let review = factory.graph_document(
        json!({"op":"run","template":"review","brief":"Review","explain":true}),
        budget,
        &cancel,
    )?;
    assert_eq!(
        review["document"]["nodes"][0]["source"],
        json!({
            "type":"commit", "commit":crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?
        })
    );
    assert_eq!(review["document"]["review_subject_missing"], true);
    assert!(
        review["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| {
                warning.as_str().is_some_and(|text| {
                    text.contains("No review subject was supplied")
                        && text.contains("...")
                        && text.contains("from ")
                })
            })
    );
    assert!(
        review["document"]["nodes"][0]["prompt"]
            .as_str()
            .unwrap()
            .contains("No review subject was supplied")
    );
    let base = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
    crate::worktree_tests::git(&rig, &["commit", "--allow-empty", "-m", "review subject"])?;
    let head = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
    let pinned = factory.graph_document(
            json!({"op":"run","template":"review","brief":"Review","set":{"subject":head,"review_base":base},"explain":true}), budget, &cancel,
        )?;
    assert_eq!(pinned["document"]["review_base"], base);
    assert_eq!(pinned["document"]["nodes"][0]["source"]["commit"], head);
    let error = factory.graph_document(json!({"op":"run","template":"review","set":{"subject":"missing-ref","review_base":base},"explain":true}), budget, &cancel).unwrap_err();
    assert!(
        matches!(error, FacadeError::InvalidInput { detail: Some(InvalidInputDetail::UnresolvableBase(ref name)) } if name == "missing-ref")
    );
    Ok(())
}

fn replay_digest_for_template(test_name: &str, template: &str) -> Result {
    let Some((rig, factory)) = replay_digest_fixture(test_name)? else {
        return Ok(());
    };
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    let explain = |input: Value| -> Result<String> {
        let label = input.to_string();
        let response = factory
            .graph_document(input, budget, &cancel)
            .map_err(|error| format!("{label}: {error:?}"))?;
        Ok(response["document"]["replay_digest"]
            .as_str()
            .ok_or("replay digest")?
            .to_owned())
    };
    let mut input = json!({"op":"run","template":template,"brief":"same brief","explain":true});
    if template == "deliver" {
        input["set"] = json!({"worktree":rig.project,"write_scope":["src"]});
    }
    let first = explain(input.clone())?;
    assert_eq!(first, explain(input.clone())?);
    crate::worktree_tests::git(&rig, &["commit", "--allow-empty", "-m", "new subject"])?;
    let after_commit = explain(input.clone())?;
    assert_ne!(first, after_commit);
    input["brief"] = json!("changed brief");
    assert_ne!(after_commit, explain(input)?);
    Ok(())
}

#[test]
fn replay_digest_tracks_review_source_commit() -> Result {
    replay_digest_for_template(
        "fabric::interface::tests::replay_digest_tracks_review_source_commit",
        "review",
    )
}

#[test]
fn replay_digest_tracks_security_review_source_commit() -> Result {
    replay_digest_for_template(
        "fabric::interface::tests::replay_digest_tracks_security_review_source_commit",
        "review.security",
    )
}

#[test]
fn replay_digest_tracks_delivery_source_commit() -> Result {
    replay_digest_for_template(
        "fabric::interface::tests::replay_digest_tracks_delivery_source_commit",
        "deliver",
    )
}

#[test]
fn smoke_graph_explains_writer_gating_for_default_remote_and_local() -> Result {
    let rig = TestRig::new()?;
    let remote_host = provenant_core::HostId::from_bytes([2; 16]);
    let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
    let mut context = Context::open_inspection(roots, &rig.home, &rig.project)?;
    context.in_lane = false;
    context.config.peers.insert(
        "peer-a".into(),
        provenant_fabric_runtime::config::Peer {
            host: remote_host,
            destination: "fixture".into(),
            remote_executable: None,
            connect_timeout_seconds: 10,
        },
    );
    let factory = Factory {
        context,
        project_attribution: false,
        clock: SystemClock,
        backoff: ThreadPause,
        executor: SystemExecutor::new(1)?,
        home: rig.home.clone(),
        cwd: rig.project.clone(),
        assets: rig.project.clone(),
        host_launch: None,
        workers: &Workers,
    };
    let budget = WaitBudget::cli(factory.clock.mono_now(), 5)
        .map_err(|error| format!("wait budget: {error:?}"))?;
    let cancel = CancelToken::new();

    let preview = factory
        .graph_document(
            json!({"op":"run", "template":"smoke", "explain":true}),
            budget,
            &cancel,
        )
        .map_err(|error| format!("default smoke preview: {error:?}"))?;
    let preview_remote = factory.graph_document(
            json!({"op":"run", "template":"smoke", "host":"peer-a", "set":{"worktree":rig.project}, "explain":true}),
            budget,
            &cancel,
        ).map_err(|error| format!("remote smoke preview: {error:?}"))?;
    let preview_local = factory
        .graph_document(
            json!({"op":"run", "template":"smoke", "set":{"worktree":rig.project}, "explain":true}),
            budget,
            &cancel,
        )
        .map_err(|error| format!("local smoke preview: {error:?}"))?;
    let skipped_writer = |preview: &Value| {
        preview["skipped"].as_array().is_some_and(|rows| {
            rows.iter().any(|row| {
                row["node"]
                    .as_str()
                    .is_some_and(|id| id.ends_with("task--write"))
            })
        })
    };
    assert!(skipped_writer(&preview));
    assert!(preview["skipped"].as_array().unwrap().iter().any(|row| {
        row["node"]
            .as_str()
            .is_some_and(|id| id.ends_with("task--write"))
            && row["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("smoke-write"))
    }));
    assert!(skipped_writer(&preview_remote));
    assert!(
        !serde_json::to_string(&preview_remote["document"])?
            .contains(&rig.project.to_string_lossy().to_string())
    );
    assert!(!skipped_writer(&preview_local));
    assert!(
        serde_json::to_string(&preview_local["document"])?
            .contains(&rig.project.to_string_lossy().to_string())
    );

    Ok(())
}

#[test]
// Regression #3744: a launch prepared for a cross-host peer must survive
// the peer-side graph-document decode exactly as a remote launch does.
fn cross_host_launch_document_round_trips_through_peer_decode() -> Result {
    let rig = TestRig::new()?;
    let remote_host = provenant_core::HostId::from_bytes([2; 16]);
    let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
    let mut context = Context::open_inspection(roots, &rig.home, &rig.project)?;
    context.in_lane = false;
    context.config.peers.insert(
        "workshop".into(),
        provenant_fabric_runtime::config::Peer {
            host: remote_host,
            destination: "workshop".into(),
            remote_executable: None,
            connect_timeout_seconds: 10,
        },
    );
    let factory = Factory {
        context,
        project_attribution: false,
        clock: SystemClock,
        backoff: ThreadPause,
        executor: SystemExecutor::new(1)?,
        home: rig.home.clone(),
        cwd: rig.project.clone(),
        assets: rig.project.clone(),
        host_launch: None,
        workers: &Workers,
    };
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    let cancel = CancelToken::new();
    // A file-backed brief makes the launch carry this host's absolute
    // policy-input path, exactly as `--brief-file` does on the sender.
    let brief = rig.root.join("sender/private-brief.md");
    std::fs::create_dir_all(brief.parent().unwrap())?;
    std::fs::write(&brief, "Inspect the parser")?;
    let launch = factory.graph_document(
        json!({
            "op":"run", "template":"explore", "brief":"Inspect the parser",
            "host":"workshop", "explain":true,
            "set":{
                "attempt.policy_inputs":[brief.clone()],
                "attempt.confidential":true
            }
        }),
        budget,
        &cancel,
    )?;
    let mut sender: provenant_fabric_runtime::graph::AttemptSpec =
        serde_json::from_value(launch["document"]["nodes"][0]["attempt"].clone())?;
    assert_eq!(sender.policy_inputs, vec![brief.clone()]);
    provenant_cli::run::seal_launch_policy(
        &mut sender,
        &rig.project,
        "Inspect the parser",
        &Default::default(),
        None,
        None,
    )
    .map_err(|error| std::io::Error::other(error.message))?;
    assert!(sender.confidential);
    assert!(sender.launch_policy.is_some());
    // The peer embeds the peer-bound projection, not the sender attempt.
    let peer_attempt = sender.peer_bound();
    let mut peer_document = launch["document"].clone();
    peer_document["nodes"][0]["attempt"] = serde_json::to_value(&peer_attempt)?;
    let peer_text = serde_json::to_string(&peer_document)?;
    let sender_path = brief.to_string_lossy().into_owned();
    assert!(
        !peer_text.contains(&sender_path),
        "sender path {sender_path} reached the peer document"
    );
    assert!(peer_attempt.policy_inputs.is_empty());
    assert!(peer_attempt.directory.is_none());
    assert_eq!(peer_attempt.launch_policy, sender.launch_policy);
    let bytes = serde_json::to_vec(&peer_document)?;
    let decoded = decode_graph_document(&bytes)
        .map_err(|error| format!("peer decode of cross-host launch: {error:?}"))?;
    assert_eq!(serde_json::to_value(&decoded)?, peer_document);
    assert_eq!(
        decoded.nodes[0]
            .attempt
            .as_ref()
            .and_then(|attempt| attempt.launch_policy.clone()),
        sender.launch_policy
    );
    // Mirror what the host does next: publish the peer-bound document and
    // launch it.
    let scratch = factory
        .prepare_scratch(&cancel)
        .map_err(|error| format!("{error:?}"))?;
    let path = publish_graph_document(&scratch, &peer_document)
        .map_err(|error| format!("publish: {error}"))?;
    let run = factory
        .graph(
            GraphInput::Run {
                extra: Default::default(),
                document: path,
            },
            budget,
            &cancel,
        )
        .map_err(|error| format!("cross-host launch: {error:?}"))?;
    assert_eq!(run.checkpoint.graph_id, decoded.graph_id);
    Ok(())
}

#[test]
// Spec: docs/specs/routing.md#pinned-review-selection
fn reviews_of_the_same_subject_attach_to_the_completed_attempt() -> Result {
    use provenant_fabric_runtime::graph::{self, AttemptOwner, GraphExecution};
    struct Reviewer(usize);
    impl AttemptOwner for Reviewer {
        type Error = std::convert::Infallible;
        fn worktree_layout(&self) -> provenant_config::WorktreeLayoutPort {
            provenant_config::PortTable::builtin().worktree_layout
        }
        fn actor(&self) -> String {
            "review-fixture".into()
        }
        fn observe_or_start(
            &mut self,
            _: &graph::AttemptKey,
            _: graph::NodeInput<'_>,
            _: &graph::GraphRoute,
            _: graph::AccessMode,
            _: Option<&str>,
            _: graph::Replay<'_>,
        ) -> std::result::Result<graph::AttemptObservation, Self::Error> {
            self.0 += 1;
            Ok(graph::AttemptObservation::Terminal {
                attempt_ref: Some("fixture/review".into()),
                outcome: graph::AttemptOutcome::Succeeded,
                launched: true,
                result: None,
                result_warning: None,
                result_path: None,
                branch: None,
                commit: None,
                settlement_reason: Some(provenant_records::run::SettlementReason::Completed),
                stop: None,
            })
        }
    }
    let rig = crate::worktree_tests::repository()?;
    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    // Only a named launch replays (run.md: without a name the host
    // assigns the next TEMPLATE-K and never replays).
    let input = json!({"op":"run", "template":"review", "brief":"Review the parser",
            "route":"senior", "name":"parser-review"});
    let mut preview = input.clone();
    preview["explain"] = json!(true);
    let document: GraphDocument = serde_json::from_value(
        factory.graph_document(preview, budget, &cancel)?["document"].clone(),
    )?;
    assert!(
        document.nodes[0]
            .prompt
            .contains("No review subject was supplied")
    );
    let mut reviewer = Reviewer(0);
    let first = {
        let mut journal = FileJournal::open(
            &factory.context.roots.state,
            factory.context.identity.project,
            &document.graph_id,
            budget,
            &cancel,
            &factory.clock,
        )?;
        journal.bind(GraphExecution {
            peer_origin_context: None,
            peer_submission_digest: None,
            requester_path: None,
            extra: Default::default(),
            bases: None,
            directory: factory.cwd.clone(),
            document: document.clone(),
        })?;
        graph::advance_with_budget(
            &document,
            &mut reviewer,
            &mut journal,
            &factory.context.config.graph.limits,
            &factory.context.config.graph.budget,
        )?
    };
    assert_eq!(first.status, GraphStatus::Complete);
    assert_eq!(reviewer.0, 1);
    // Submit through the real front door, with and without a wait. A
    // content-identical named review must reuse the terminal journal
    // rather than entering the host's provider-launch path again.
    for wait in [None, Some(1)] {
        let mut repeated = input.clone();
        if let Some(seconds) = wait {
            repeated["wait_seconds"] = json!(seconds);
        }
        let attached = factory.graph_document(repeated, budget, &cancel)?;
        assert_eq!(attached["status"], "complete");
        assert_eq!(attached["timed_out"], false);
        let checkpoint: graph::GraphCheckpoint =
            serde_json::from_value(attached["checkpoint"].clone())?;
        assert_eq!(checkpoint, first.checkpoint);
        let attempts: Vec<_> = checkpoint
            .nodes
            .iter()
            .flat_map(|node| &node.attempts)
            .collect();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].attempt_ref.as_deref(), Some("fixture/review"));
    }
    assert_eq!(
        FileJournal::graph_ids(
            &factory.context.roots.state,
            factory.context.identity.project,
        )?,
        [document.graph_id]
    );
    Ok(())
}

#[test]
fn a_named_launch_with_the_same_digest_returns_its_graph_without_launching() -> Result {
    let rig = crate::worktree_tests::repository()?;
    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    let mut document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1, "graph_id":"recover-run", "nodes":[{
            "id":"worker", "access":"read_only", "prompt":"Original task",
            "route":{"adapter":"fixture", "model":"fixture/model"}
        }]
    }))?;
    document.set_replay_digest("identical-launch-input");
    // Both submissions chose the same unbound name. Admission is real;
    // provider execution is deferred at the process boundary.
    let barrier = std::sync::Barrier::new(2);
    let launch = || {
        barrier.wait();
        provenant_cli::run::with_deferred_provider_launch(|| {
            factory.launch_submission(document.clone(), &json!({}), budget, &cancel, None)
        })
    };
    let results = std::thread::scope(|scope| {
        let left = scope.spawn(launch);
        let right = scope.spawn(launch);
        [left.join().unwrap(), right.join().unwrap()]
    });
    let results = [
        results[0].as_ref().map_err(|error| error.to_string())?,
        results[1].as_ref().map_err(|error| error.to_string())?,
    ];
    assert_eq!(
        results
            .iter()
            .filter(|result| result["replayed"] == false)
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| result["replayed"] == true)
            .count(),
        1
    );
    let saved = FileJournal::read_checkpoint(
        &factory.context.roots.state,
        factory.context.identity.project,
        "recover-run",
    )?
    .ok_or("checkpoint")?;
    assert_eq!(saved.nodes[0].attempts.len(), 1);
    let replay =
        factory.launch_submission(document, &json!({"wait_seconds":30}), budget, &cancel, None)?;
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["timed_out"], false);
    let after = FileJournal::read_checkpoint(
        &factory.context.roots.state,
        factory.context.identity.project,
        "recover-run",
    )?
    .ok_or("replayed checkpoint")?;
    assert_eq!(after, saved);
    Ok(())
}

#[test]
fn attempt_settings_merge_before_graph_and_child_template_binding() -> Result {
    let rig = TestRig::new()?;
    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
    let input = json!({
        "op":"run", "explain":true, "host":"local", "account":"flag-account",
        "document":{"use":"task", "attempt":{"build_heavy":true, "provider_timeout":10}},
        "set":{
            "attempt":{"host":"overridden-peer", "account":"set-account", "confidential":true},
            "attempt.provider_timeout":42
        }
    });
    let response = factory.graph_document(input.clone(), budget, &cancel)?;
    let document: GraphDocument = serde_json::from_value(response["document"].clone())?;
    let child = factory.child_request(&input)?;
    let child = child.launch.ok_or("child launch missing")?;
    let child: provenant_fabric_runtime::graph::GraphNode = serde_json::from_value(child.node)?;
    for node in [&document.nodes[0], &child] {
        let attempt = node.attempt.as_ref().ok_or("attempt missing")?;
        assert_eq!(attempt.host.as_deref(), Some("local"));
        assert_eq!(attempt.account.as_deref(), Some("flag-account"));
        assert!(attempt.build_heavy);
        assert!(attempt.confidential);
        assert_eq!(attempt.provider_timeout, Some(42));
    }
    // A host supplied only through --set must survive parameter extraction too.
    for settings in [
        json!({"attempt.host":"local"}),
        json!({"attempt":{"host":"local"}}),
    ] {
        let input = json!({"op":"run", "brief":"Inspect", "set":settings, "explain":true});
        let response = factory.graph_document(input.clone(), budget, &cancel)?;
        assert_eq!(response["document"]["nodes"][0]["attempt"]["host"], "local");
        let child = factory
            .child_request(&input)?
            .launch
            .ok_or("child launch missing")?;
        assert_eq!(child.node["attempt"]["host"], "local");
    }
    Ok(())
}

#[test]
fn graph_and_child_launch_refuse_invalid_attempt_settings_before_publication() -> Result {
    let rig = TestRig::new()?;
    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
    for settings in [
        json!({"attempt":[]}),
        json!({"attempt.host":7}),
        json!({"attempt.host":""}),
        json!({"attempt.account":[]}),
        json!({"attempt.build_heavy":1}),
        // `--set` text is typed by the field (cli.md), so "true" binds;
        // text no field reading accepts is still refused.
        json!({"attempt.confidential":"maybe"}),
        json!({"attempt.provider_timeout":0}),
        json!({"attempt.provider_timeout":-1}),
        json!({"attempt.provider_timeout":1.5}),
    ] {
        let input = json!({"op":"run", "brief":"Inspect", "set":settings, "explain":true});
        assert!(
            matches!(
                factory.graph_document(input.clone(), budget, &cancel),
                Err(FacadeError::InvalidInput { .. })
            ),
            "graph accepted {settings}"
        );
        assert!(
            matches!(
                factory.child_request(&input),
                Err(FacadeError::InvalidInput { .. })
            ),
            "child accepted {settings}"
        );
    }
    assert!(!rig.project.join(".provenant/scratch/graphs").exists());
    assert!(
        FileJournal::graph_ids(
            &factory.context.roots.state,
            factory.context.identity.project
        )?
        .is_empty()
    );
    Ok(())
}

#[test]
fn composite_graph_and_child_apply_route_and_attempt_controls_to_every_leaf() -> Result {
    fn assert_leaves(
        node: &provenant_fabric_runtime::graph::GraphNode,
        leaves: &mut usize,
    ) -> Result {
        if let Some(composite) = &node.composite {
            for child in &composite.nodes {
                assert_leaves(child, leaves)?;
            }
            return Ok(());
        }
        *leaves += 1;
        let route = node.route.as_ref().ok_or("leaf route missing")?;
        assert_eq!(route.adapter.as_deref(), Some("fixture"));
        assert_eq!(route.model.as_deref(), Some("fixture/model"));
        assert_eq!(route.effort.as_deref(), Some("high"));
        assert_eq!(route.account.as_deref(), Some("flag-account"));
        assert!(node.routes.is_none());
        let attempt = node.attempt.as_ref().ok_or("leaf attempt missing")?;
        assert_eq!(attempt.host.as_deref(), Some("local"));
        assert_eq!(attempt.account.as_deref(), Some("flag-account"));
        assert!(attempt.build_heavy);
        assert!(attempt.confidential);
        assert_eq!(attempt.provider_timeout, Some(42));
        Ok(())
    }
    let rig = TestRig::new()?;
    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
    let input = json!({
        "op":"run", "explain":true,
        "model":"fixture/model", "effort":"high", "account":"flag-account",
        "set":{"attempt.host":"local", "attempt.build_heavy":true,
            "attempt.confidential":true, "attempt.provider_timeout":42},
        "document":{"nodes":[
            {"id":"first", "use":"task", "prompt":"Inspect first", "tier":"junior"},
            {"id":"second", "use":"task", "prompt":"Inspect second", "tier":"senior"}
        ]}
    });
    let response = factory.graph_document(input.clone(), budget, &cancel)?;
    let document: GraphDocument = serde_json::from_value(response["document"].clone())?;
    let mut graph_leaves = 0;
    for node in &document.nodes {
        assert_leaves(node, &mut graph_leaves)?;
    }
    assert_eq!(graph_leaves, 2);
    let child = factory
        .child_request(&input)?
        .launch
        .ok_or("child launch missing")?;
    let child: provenant_fabric_runtime::graph::GraphNode = serde_json::from_value(child.node)?;
    let mut child_leaves = 0;
    assert_leaves(&child, &mut child_leaves)?;
    assert_eq!(child_leaves, graph_leaves);
    Ok(())
}

#[test]
fn plain_task_launch_controls_bind_writer_and_route_before_admission() -> Result {
    let rig = TestRig::new()?;
    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
    let response = factory.graph_document(
        json!({
            "op":"run", "brief":"Fix the parser", "route":"senior",
            "worktree":rig.project, "write_scope":["src"], "explain":true
        }),
        budget,
        &cancel,
    )?;
    let document: GraphDocument = serde_json::from_value(response["document"].clone())?;
    assert_eq!(document.nodes.len(), 1);
    let node = &document.nodes[0];
    assert_eq!(
        node.access,
        provenant_fabric_runtime::graph::AccessMode::WorktreeWrite
    );
    assert_eq!(node.worktree.as_deref(), rig.project.to_str());
    assert_eq!(node.write_scope, vec!["src"]);
    assert_eq!(
        node.route.as_ref().and_then(|route| route.alias.as_deref()),
        Some("senior")
    );
    Ok(())
}

#[test]
fn writer_branch_explanation_is_one_admitted_node_without_git_effects() -> Result {
    let rig = TestRig::new()?;
    let factory = factory(&rig)?;
    let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
    let cancel = CancelToken::new();
    let response = factory.graph_document(
        json!({
            "op":"run", "brief":"Fix the parser", "branch":"fix/parser",
            "base":"HEAD", "explain":true
        }),
        budget,
        &cancel,
    )?;
    let document: GraphDocument = serde_json::from_value(response["document"].clone())?;
    assert_eq!(document.nodes.len(), 1);
    let node = &document.nodes[0];
    assert_eq!(
        node.worktree.as_deref(),
        Some(
            rig.project
                .join(".worktrees/fix-parser")
                .to_str()
                .ok_or("path")?
        )
    );
    let target = node
        .attempt
        .as_ref()
        .and_then(|spec| spec.new_worktree.as_ref())
        .ok_or("new worktree missing")?;
    assert_eq!(target.branch, "fix/parser");
    assert_eq!(target.base, "HEAD");
    assert!(!rig.project.join(".worktrees/fix-parser").exists());
    assert!(
        factory
            .graph_document(
                json!({"op":"run","template":"deliver","worktree":rig.project,"explain":true}),
                budget,
                &cancel
            )
            .is_err()
    );
    Ok(())
}

#[test]
fn graph_template_errors_reach_the_agent_with_safe_diagnostics() -> Result {
    let rig = TestRig::new()?;
    // Inspection does not require a previously registered project, including
    // when the test process itself runs inside a lane.
    let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
    let mut context = Context::open_inspection(roots, &rig.home, &rig.project)?;
    context.in_lane = false;
    let factory = Factory {
        context,
        project_attribution: false,
        clock: SystemClock,
        backoff: ThreadPause,
        executor: SystemExecutor::new(1)?,
        home: rig.home.clone(),
        cwd: rig.project.clone(),
        assets: rig.project.clone(),
        host_launch: None,
        workers: &Workers,
    };
    let directory = rig.project.join(".provenant/templates");
    std::fs::create_dir_all(&directory)?;
    let path = directory.join("review.typo.toml");
    std::fs::write(&path, "accesz = \"read_only\"\n")?;
    let error = factory.template_library(None).unwrap_err();
    let structured =
        provenant_fabric_runtime::relay::ServiceError::Facade(error.clone()).structured_error();
    assert_eq!(structured["code"], error.diagnostic().code);
    assert!(
        structured["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("templates")),
        "{structured}"
    );
    assert_eq!(
        structured["next"].as_str(),
        error.diagnostic().next.as_deref()
    );
    let diagnostic = error.diagnostic();
    let text = diagnostic.detail.ok_or("agent diagnostic")?;
    assert!(
        text.contains("templates") && text.contains("validated"),
        "{text}"
    );
    assert!(!text.contains("accesz"), "{text}");
    let wire = serde_json::to_value(&error)?;
    assert!(
        wire["detail"]
            .as_str()
            .ok_or("wire detail")?
            .contains("templates")
    );
    std::fs::remove_file(path)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
    let error = factory
        .graph_document(
            json!({"op":"run", "template":"task.typo", "explain":true}),
            budget,
            &cancel,
        )
        .unwrap_err();
    assert!(
        error
            .diagnostic()
            .detail
            .ok_or("template refusal")?
            // docs/specs/cli.md#template-name-errors (#3350): the refusal names the
            // supplied template; unclassified graph input keeps the generic text.
            .contains("unknown template task.typo")
    );
    let error = factory
        .graph_document(
            json!({"op":"run", "document":{"graph_id":"cycle", "nodes":[
            {"id":"a", "use":"task", "after":["b"]},
            {"id":"b", "use":"task", "after":["a"]}
        ]}, "explain":true}),
            budget,
            &cancel,
        )
        .unwrap_err();
    // A compiled cycle is an admission failure, so it reads the same safe
    // plan refusal as the raw document it compiles to.
    assert!(
        error
            .diagnostic()
            .detail
            .ok_or("patch refusal")?
            .contains("node a: dependency cycle")
    );
    let error = factory
        .graph_document(
            json!({"op":"run", "template":"implement", "brief":"Fix it", "explain":true}),
            budget,
            &cancel,
        )
        .unwrap_err();
    let diagnostic = error.diagnostic();
    assert!(diagnostic.detail.as_deref().is_some_and(|detail| {
        detail.starts_with("node ") && detail.contains("writer worktree")
    }));
    assert!(diagnostic.next.as_deref().is_some_and(|next| {
        next.contains("--worktree PATH") && next.contains("--branch B --base REF")
    }));
    let error = factory
        .graph_document(
            json!({"op":"run", "brief":"Fix it", "access":"worktree_write", "explain":true}),
            budget,
            &cancel,
        )
        .unwrap_err();
    assert!(
        error
            .diagnostic()
            .next
            .as_deref()
            .is_some_and(|next| next.contains("--worktree PATH"))
    );
    Ok(())
}

#[test]
fn writer_controls_preserve_new_branch_and_refuse_conflicting_access() -> Result {
    let root = Path::new("/fixture/project");
    let mut node = json!({"id":"task","use":"task"});
    bind_launch_controls(
        &json!({"branch":"fix/parser","base":"origin/main"}),
        &mut node,
        root,
        &provenant_config::PortTable::builtin(),
    )?;
    assert_eq!(node["worktree"], "/fixture/project/.worktrees/fix-parser");
    let mut explicit = json!({"id":"task","use":"task"});
    bind_launch_controls(
        &json!({"worktree":".worktrees/fix-parser"}),
        &mut explicit,
        Path::new("/fixture/project"),
        &provenant_config::PortTable::builtin(),
    )?;
    assert_eq!(
        node["worktree"].as_str().ok_or("branch path")?,
        explicit["worktree"].as_str().ok_or("explicit worktree")?
    );
    let mut relative = json!({"id":"task","use":"task"});
    bind_launch_controls(
        &json!({"worktree":"writer"}),
        &mut relative,
        root,
        &provenant_config::PortTable::builtin(),
    )?;
    // A relative writer path is project-root-relative from every checkout.
    assert_eq!(relative["worktree"], "/fixture/project/writer");
    assert_eq!(
        node["attempt"]["new_worktree"],
        json!({"branch":"fix/parser","base":"origin/main"})
    );
    assert_eq!(node["access"], "worktree_write");
    for input in [
        json!({"worktree":".","access":"read_only"}),
        json!({"base":"HEAD"}),
        json!({"branch":"fix/parser","base":"HEAD","worktree":"."}),
        json!({"access":"unknown"}),
        json!({"write_scope":[1]}),
    ] {
        assert!(
            bind_launch_controls(
                &input,
                &mut json!({"use":"task"}),
                root,
                &provenant_config::PortTable::builtin()
            )
            .is_err()
        );
    }
    Ok(())
}

#[test]
fn plain_read_only_task_pins_fetched_upstream_and_local_source_is_honoured() -> Result {
    let rig = TestRig::new()?;
    let remote = rig.root.join("remote.git");
    let remote = remote.to_str().ok_or("remote path")?;
    git(&rig, &["init", "-q", "--bare", remote])?;
    git(
        &rig,
        &["-C", remote, "symbolic-ref", "HEAD", "refs/heads/master"],
    )?;
    let commit = |message: &str| {
        git(
            &rig,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-q",
                "-m",
                message,
            ],
        )
    };
    std::fs::write(rig.project.join("file"), "one\n")?;
    git(&rig, &["add", "file"])?;
    commit("one")?;
    let stale = git(&rig, &["rev-parse", "HEAD"])?;
    git(&rig, &["branch", "-M", "master"])?;
    std::fs::write(rig.project.join("file"), "two\n")?;
    git(&rig, &["add", "file"])?;
    commit("two")?;
    let default = git(&rig, &["rev-parse", "HEAD"])?;
    git(&rig, &["remote", "add", "origin", remote])?;
    git(&rig, &["push", "-q", "origin", "master"])?;
    git(&rig, &["remote", "set-head", "origin", "master"])?;
    git(&rig, &["reset", "-q", "--hard", &stale])?;

    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
    let submit = |source: Option<&str>| {
        let mut input =
            json!({"op":"run","template":"task","brief":"Review the tree","explain":true});
        if let Some(source) = source {
            input["source"] = json!(source);
        }
        factory.graph_document(input, budget, &cancel)
    };

    let pinned = submit(None)?;
    let node = &pinned["document"]["nodes"][0];
    assert_eq!(node["source"]["commit"], json!(default), "{node}");
    let warnings = pinned["warnings"].as_array().ok_or("warnings")?;
    assert!(
        warnings.iter().any(|warning| warning
            .as_str()
            .is_some_and(|text| text.contains("origin") && text.contains(&default))),
        "the launch must name the source ref it used: {warnings:?}"
    );

    // An explicit local source keeps the stale checkout as the subject.
    let local = submit(Some("local"))?;
    let node = &local["document"]["nodes"][0];
    assert!(node["source"].is_null(), "{node}");
    let local_warnings = local["warnings"].as_array().ok_or("local warnings")?;
    assert!(
        !local_warnings
            .iter()
            .any(|warning| warning.as_str().is_some_and(|text| text.contains(&default))),
        "a local source must not report an upstream pin: {local_warnings:?}"
    );
    Ok(())
}

/// Regression #3870: a writer launch never takes the read-only default
/// source, however it names its checkout. A stale primary checkout must not
/// warn, pin the launch, or supply the identity commit.
/// Spec: docs/specs/run-launch.md#inputs-and-resources
#[test]
fn writer_launch_never_takes_the_read_only_default_source() -> Result {
    let rig = TestRig::new()?;
    let remote = rig.root.join("remote.git");
    let remote = remote.to_str().ok_or("remote path")?;
    git(&rig, &["init", "-q", "--bare", remote])?;
    git(
        &rig,
        &["-C", remote, "symbolic-ref", "HEAD", "refs/heads/master"],
    )?;
    let commit = |message: &str| {
        git(
            &rig,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-q",
                "-m",
                message,
            ],
        )
    };
    std::fs::write(rig.project.join("file"), "one\n")?;
    git(&rig, &["add", "file"])?;
    commit("one")?;
    let stale = git(&rig, &["rev-parse", "HEAD"])?;
    git(&rig, &["branch", "-M", "master"])?;
    std::fs::write(rig.project.join("file"), "two\n")?;
    git(&rig, &["add", "file"])?;
    commit("two")?;
    let upstream = git(&rig, &["rev-parse", "HEAD"])?;
    git(&rig, &["remote", "add", "origin", remote])?;
    git(&rig, &["push", "-q", "origin", "master"])?;
    git(&rig, &["remote", "set-head", "origin", "master"])?;
    git(&rig, &["reset", "-q", "--hard", &stale])?;

    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
    // A writer template and a read-only template bound to a writer
    // worktree both resolve to a writer graph.
    for template in ["implement", "task"] {
        let mut input = json!({
            "op":"run","template":template,"brief":"Fix the parser","explain":true,
            "branch":"fix/writer","base":upstream
        });
        if template == "implement" {
            input["set"] = json!({"plan": false});
        }
        let launch = factory
            .graph_document(input, budget, &cancel)
            .map_err(|error| {
                format!("{template} writer launch from a stale checkout: {error:?}")
            })?;
        assert!(
            launch["read_source"]["commit"].is_null(),
            "{template} writer launch must not pin a read source: {launch}"
        );
        assert!(
            !launch["warnings"].as_array().is_some_and(|warnings| {
                warnings.iter().any(|warning| {
                    warning
                        .as_str()
                        .is_some_and(|text| text.contains("read-only lane will read"))
                })
            }),
            "{template} writer launch must not warn about a read-only source: {launch}"
        );
        assert!(
            launch["document"]["nodes"][0]["source"].is_null(),
            "{template} writer launch must keep its own base: {launch}"
        );
    }
    Ok(())
}

/// Regression #3870: a writer's launch identity starts from its base, so
/// advancing the caller's checkout does not make it a different launch.
#[test]
fn writer_launch_identity_follows_its_base_not_the_checkout_head() -> Result {
    let rig = TestRig::new()?;
    git(&rig, &["config", "user.name", "Test"])?;
    git(&rig, &["config", "user.email", "test@example.invalid"])?;
    std::fs::write(rig.project.join("file"), "one\n")?;
    git(&rig, &["add", "file"])?;
    git(&rig, &["commit", "-q", "-m", "one"])?;
    let base = git(&rig, &["rev-parse", "HEAD"])?;

    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
    let digest = || -> Result<String> {
        let launch = factory.graph_document(
            json!({
                "op":"run","template":"implement","brief":"Fix the parser","explain":true,
                "branch":"fix/writer","base":base,"set":{"plan":false}
            }),
            budget,
            &cancel,
        )?;
        Ok(launch["document"]["replay_digest"]
            .as_str()
            .ok_or("replay digest")?
            .to_owned())
    };
    let first = digest()?;
    std::fs::write(rig.project.join("file"), "two\n")?;
    git(&rig, &["add", "file"])?;
    git(&rig, &["commit", "-q", "-m", "two"])?;
    assert_eq!(
        first,
        digest()?,
        "the writer's identity must follow its base, not the checkout head"
    );
    Ok(())
}

/// A writer hidden under a composite still makes the launch a writer; a
/// writer whose condition does not hold leaves it a reader. Fixtures come
/// from the bundled templates' own lowering, so the roles under test are
/// the ones the compiler produces.
#[test]
fn writer_roles_follow_the_lowered_graph_and_its_conditions() {
    let library = TemplateLibrary::bundled();
    let resolvers = BTreeMap::new();
    let lower = |node: Value| -> Value {
        provenant_fabric_runtime::graph::template::Compiler {
            library: &library,
            resolvers: &resolvers,
        }
        .lower(&node)
        .expect("bundled template lowers")
    };
    // A bundled writer template lowers to a worktree writer.
    assert!(lowered_writes_worktree(&lower(json!({"use":"implement"}))));
    // The bundled smoke template carries a writer under `smoke-write`,
    // which defaults to false: the same launch stays read-only until the
    // condition holds.
    assert!(!lowered_writes_worktree(&lower(json!({"use":"smoke"}))));
    assert!(lowered_writes_worktree(&lower(json!({
        "use":"smoke",
        "with":{"smoke-write":true, "worktree":"/tmp/writer"}
    }))));
}

/// Spec: docs/specs/run-launch.md#inputs-and-resources
#[test]
fn read_source_choices_pin_the_commit_and_name_their_layer() -> Result {
    let rig = TestRig::new()?;
    let remote = rig.root.join("remote.git");
    let remote = remote.to_str().ok_or("remote path")?;
    git(&rig, &["init", "-q", "--bare", remote])?;
    git(
        &rig,
        &["-C", remote, "symbolic-ref", "HEAD", "refs/heads/master"],
    )?;
    git(&rig, &["config", "user.name", "Test"])?;
    git(&rig, &["config", "user.email", "test@example.invalid"])?;
    std::fs::write(rig.project.join("file"), "one\n")?;
    git(&rig, &["add", "file"])?;
    git(&rig, &["commit", "-q", "-m", "one"])?;
    let stale = git(&rig, &["rev-parse", "HEAD"])?;
    git(&rig, &["branch", "-M", "master"])?;
    std::fs::write(rig.project.join("file"), "two\n")?;
    git(&rig, &["add", "file"])?;
    git(&rig, &["commit", "-q", "-m", "two"])?;
    let default = git(&rig, &["rev-parse", "HEAD"])?;
    git(&rig, &["remote", "add", "origin", remote])?;
    git(&rig, &["push", "-q", "origin", "master"])?;
    git(&rig, &["remote", "set-head", "origin", "master"])?;
    git(&rig, &["reset", "-q", "--hard", &stale])?;

    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
    let submit = |knob: Option<&str>, source: Option<&str>| -> Result<Value> {
        let mut config = "schema_version=1\n[gc]\nfree_floor='1B'\n".to_owned();
        if let Some(knob) = knob {
            config.push_str(&format!("[knobs]\nlanes.read_source='{knob}'\n"));
        }
        std::fs::write(rig.state.join("config.toml"), config)?;
        let mut input =
            json!({"op":"run","template":"task","brief":"Review the tree","explain":true});
        if let Some(source) = source {
            input["source"] = json!(source);
        }
        Ok(factory.graph_document(input, budget, &cancel)?)
    };

    let pinned = submit(Some("default_branch"), None)?;
    assert_eq!(
        pinned["document"]["nodes"][0]["source"]["commit"],
        json!(default),
        "{pinned}"
    );
    assert_eq!(pinned["read_source"]["source"], json!("host"));
    assert_eq!(pinned["read_source"]["commit"], json!(default));
    assert!(
        pinned["warnings"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row
                .as_str()
                .is_some_and(|text| text.contains("lanes.read_source=default_branch")))),
        "the warning must name the sourced setting: {}",
        pinned["warnings"]
    );

    let checkout = submit(Some("checkout"), None)?;
    assert!(checkout["document"]["nodes"][0]["source"].is_null());
    assert!(checkout["read_source"]["commit"].is_null());

    let explicit = submit(Some("refs/remotes/origin/master"), None)?;
    assert_eq!(
        explicit["document"]["nodes"][0]["source"]["commit"],
        json!(default),
        "{explicit}"
    );
    assert_eq!(
        explicit["read_source"]["value"],
        json!("refs/remotes/origin/master")
    );

    let flag = submit(None, Some("upstream"))?;
    assert_eq!(
        flag["document"]["nodes"][0]["source"]["commit"],
        json!(default)
    );
    assert_eq!(flag["read_source"]["source"], json!("launch"));
    let flag = submit(None, Some("local"))?;
    assert!(flag["document"]["nodes"][0]["source"].is_null());
    Ok(())
}

/// An explicit `default_branch` pins the fetched default, not the current
/// branch's tracking upstream: a feature checkout must not read its own tip.
/// Spec: docs/specs/run-launch.md#inputs-and-resources
#[test]
fn default_branch_pins_the_forge_default_not_the_feature_upstream() -> Result {
    let rig = TestRig::new()?;
    let remote = rig.root.join("remote.git");
    let remote = remote.to_str().ok_or("remote path")?;
    git(&rig, &["init", "-q", "--bare", remote])?;
    git(
        &rig,
        &["-C", remote, "symbolic-ref", "HEAD", "refs/heads/main"],
    )?;
    git(&rig, &["config", "user.name", "Test"])?;
    git(&rig, &["config", "user.email", "test@example.invalid"])?;
    std::fs::write(rig.project.join("file"), "one\n")?;
    git(&rig, &["add", "file"])?;
    git(&rig, &["commit", "-q", "-m", "one"])?;
    git(&rig, &["branch", "-M", "main"])?;
    git(&rig, &["remote", "add", "origin", remote])?;
    git(&rig, &["push", "-q", "-u", "origin", "main"])?;
    git(&rig, &["remote", "set-head", "origin", "main"])?;
    let default = git(&rig, &["rev-parse", "HEAD"])?;
    // The feature branch tracks origin/feature, a non-default upstream.
    git(&rig, &["checkout", "-q", "-b", "feature"])?;
    std::fs::write(rig.project.join("file"), "two\n")?;
    git(&rig, &["add", "file"])?;
    git(&rig, &["commit", "-q", "-m", "two"])?;
    git(&rig, &["push", "-q", "-u", "origin", "feature"])?;
    let feature = git(&rig, &["rev-parse", "HEAD"])?;
    assert_ne!(feature, default, "the fixture needs two distinct tips");

    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
    let submit = |knob: Option<&str>, source: Option<&str>| -> Result<Value> {
        let mut config = "schema_version=1\n[gc]\nfree_floor='1B'\n".to_owned();
        if let Some(knob) = knob {
            config.push_str(&format!("[knobs]\nlanes.read_source='{knob}'\n"));
        }
        std::fs::write(rig.state.join("config.toml"), config)?;
        let mut input =
            json!({"op":"run","template":"task","brief":"Review the tree","explain":true});
        if let Some(source) = source {
            input["source"] = json!(source);
        }
        Ok(factory.graph_document(input, budget, &cancel)?)
    };

    for chosen in [
        submit(Some("default_branch"), None)?,
        submit(None, Some("upstream"))?,
    ] {
        let commit = chosen["document"]["nodes"][0]["source"]["commit"].clone();
        assert_eq!(
            commit,
            json!(default),
            "explicit default_branch must pin the fetched default: {chosen}"
        );
        assert_ne!(
            commit,
            json!(feature),
            "the tracking upstream is not the default branch: {chosen}"
        );
    }
    // With no source setting the advisory pin keeps this level checkout.
    let automatic = submit(None, None)?;
    assert!(
        automatic["document"]["nodes"][0]["source"].is_null(),
        "{automatic}"
    );
    Ok(())
}

/// Spec: docs/specs/run-launch.md#inputs-and-resources
#[test]
fn read_source_fetch_first_refreshes_the_ref_before_resolving() -> Result {
    let rig = TestRig::new()?;
    let remote = rig.root.join("remote.git");
    let remote = remote.to_str().ok_or("remote path")?;
    git(&rig, &["init", "-q", "--bare", remote])?;
    git(
        &rig,
        &["-C", remote, "symbolic-ref", "HEAD", "refs/heads/main"],
    )?;
    git(&rig, &["config", "user.name", "Test"])?;
    git(&rig, &["config", "user.email", "test@example.invalid"])?;
    std::fs::write(rig.project.join("file"), "one\n")?;
    git(&rig, &["add", "file"])?;
    git(&rig, &["commit", "-q", "-m", "one"])?;
    let first = git(&rig, &["rev-parse", "HEAD"])?;
    git(&rig, &["branch", "-M", "main"])?;
    git(&rig, &["remote", "add", "origin", remote])?;
    git(&rig, &["push", "-q", "origin", "main"])?;
    git(&rig, &["remote", "set-head", "origin", "main"])?;
    // A separate clone publishes a newer tip the checkout has not fetched.
    let seed = rig.root.join("seed");
    git(
        &rig,
        &["clone", "-q", remote, seed.to_str().ok_or("seed path")?],
    )?;
    let seed = seed.to_str().ok_or("seed path")?;
    git(&rig, &["-C", seed, "config", "user.name", "Test"])?;
    git(
        &rig,
        &["-C", seed, "config", "user.email", "test@example.invalid"],
    )?;
    std::fs::write(rig.root.join("seed").join("file"), "two\n")?;
    git(&rig, &["-C", seed, "add", "file"])?;
    git(&rig, &["-C", seed, "commit", "-q", "-m", "two"])?;
    let second = git(&rig, &["-C", seed, "rev-parse", "HEAD"])?;
    git(&rig, &["-C", seed, "push", "-q", "origin", "main"])?;

    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
    let submit = |fetch_first: bool| -> Result<Value> {
        let config = format!(
            "schema_version=1\n[gc]\nfree_floor='1B'\n[knobs]\nlanes.read_source='refs/remotes/origin/main'\nlanes.read_source_fetch_first={fetch_first}\n"
        );
        std::fs::write(rig.state.join("config.toml"), config)?;
        Ok(factory.graph_document(
            json!({"op":"run","template":"task","brief":"Review the tree","explain":true}),
            budget,
            &cancel,
        )?)
    };

    let last_fetched = submit(false)?;
    assert_eq!(
        last_fetched["document"]["nodes"][0]["source"]["commit"],
        json!(first),
        "{last_fetched}"
    );
    let fetched = submit(true)?;
    assert_eq!(
        fetched["document"]["nodes"][0]["source"]["commit"],
        json!(second),
        "fetch-first must pin the newly fetched tip: {fetched}"
    );
    assert_eq!(fetched["read_source"]["fetch_first"], json!(true));
    Ok(())
}

/// Spec: docs/specs/run-launch.md#inputs-and-resources
#[test]
fn plan_node_of_implement_plan_reads_the_launch_base() -> Result {
    let rig = TestRig::new()?;
    git(&rig, &["config", "user.name", "Test"])?;
    git(&rig, &["config", "user.email", "test@example.invalid"])?;
    std::fs::write(rig.project.join("file"), "one\n")?;
    git(&rig, &["add", "file"])?;
    git(&rig, &["commit", "-q", "-m", "one"])?;
    let base = git(&rig, &["rev-parse", "HEAD"])?;
    std::fs::write(rig.project.join("file"), "two\n")?;
    git(&rig, &["add", "file"])?;
    git(&rig, &["commit", "-q", "-m", "two"])?;

    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
    let planned = factory.graph_document(
        json!({
            "op":"run","template":"implement","brief":"Fix it","explain":true,
            "branch":"fix/base","base":base,"set":{"plan":true}
        }),
        budget,
        &cancel,
    )?;
    let plan = &planned["document"]["nodes"][0]["composite"]["nodes"][0];
    assert_eq!(plan["template"], json!("plan"), "{planned}");
    assert_eq!(plan["source"]["commit"], json!(base), "{planned}");
    Ok(())
}

#[test]
fn writer_template_with_target_defaults_to_writer_access() -> Result {
    let rig = TestRig::new()?;
    let factory = factory(&rig)?;
    for target in [
        json!({"worktree":".worktrees/fix"}),
        json!({"branch":"fix","base":"origin/main"}),
    ] {
        let mut input =
            json!({"op":"run","template":"implement","brief":"Fix it","set":{"plan":false}});
        input
            .as_object_mut()
            .unwrap()
            .extend(target.as_object().unwrap().clone());
        let request = factory.child_request(&input)?;
        assert!(request.writer);
        let node = &request
            .launch
            .as_ref()
            .ok_or("missing compiled launch")?
            .node;
        assert_eq!(node["access"], "worktree_write");
    }

    let mut explicit_read = json!({"op":"run","template":"implement","brief":"Fix it","set":{"plan":false},"access":"read_only","worktree":".worktrees/fix"});
    assert!(factory.child_request(&explicit_read).is_err());
    explicit_read["access"] = json!("worktree_write");
    let explicit_write = factory.child_request(&explicit_read)?;
    assert!(explicit_write.writer);
    let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
    let cancel = CancelToken::new();
    let explicit_read = factory.graph_document(
            json!({"op":"run","template":"implement","brief":"Inspect","access":"read_only","explain":true}),
            budget,
            &cancel,
        )?;
    assert_eq!(explicit_read["document"]["nodes"][0]["access"], "read_only");
    Ok(())
}

#[test]
fn eight_lane_composite_with_a_file_brief_fits_the_checkpoint_limit() -> Result {
    use provenant_fabric_runtime::graph::{self, AttemptOwner, GraphExecution, GraphJournal};
    #[derive(Default)]
    struct Snapshot(Option<graph::GraphCheckpoint>);
    impl GraphJournal for Snapshot {
        type Error = std::convert::Infallible;
        fn load(
            &mut self,
            _: &str,
        ) -> std::result::Result<Option<graph::GraphCheckpoint>, Self::Error> {
            Ok(self.0.clone())
        }
        fn save(
            &mut self,
            checkpoint: &graph::GraphCheckpoint,
        ) -> std::result::Result<(), Self::Error> {
            self.0 = Some(checkpoint.clone());
            Ok(())
        }
    }
    struct Waiting;
    impl AttemptOwner for Waiting {
        type Error = std::convert::Infallible;
        fn worktree_layout(&self) -> provenant_config::WorktreeLayoutPort {
            provenant_config::PortTable::builtin().worktree_layout
        }
        fn actor(&self) -> String {
            "checkpoint-fixture".into()
        }
        fn observe_or_start(
            &mut self,
            key: &graph::AttemptKey,
            _: graph::NodeInput<'_>,
            _: &graph::GraphRoute,
            _: graph::AccessMode,
            _: Option<&str>,
            _: graph::Replay<'_>,
        ) -> std::result::Result<graph::AttemptObservation, Self::Error> {
            Ok(graph::AttemptObservation::Pending {
                attempt_ref: Some(format!("fixture/{}", key.node_id)),
            })
        }
    }
    let rig = TestRig::new()?;
    let mut factory = factory(&rig)?;
    // A large file brief used by eight leaves used to be repeated in every
    // checkpoint projection. Exercise the bundled replacement for that
    // workload, including submission metadata and sealed input manifests.
    let path = rig.root.join("acceptance.txt");
    // Each control byte occupies six bytes in an inline JSON string. The
    // same real checkpoint limit is crossed with much less ingress text
    // to normalise, hash and retain in the prompt store.
    std::fs::write(&path, "\u{0001}".repeat(90_000))?;
    let brief = std::fs::read_to_string(&path)?;
    let limits = &mut factory.context.config.graph.limits;
    limits.node_prompt_bytes = 1_048_576;
    limits.graph_prompt_bytes = 8_388_608;
    limits.concurrency = Some(8);
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 60)?;
    let response = factory.graph_document(
        json!({
            "op":"run", "template":"task-fan-out", "brief":brief,
            "set":{"items":(0..8).map(|n|format!("slice-{n}")).collect::<Vec<_>>()},
            "explain":true,
        }),
        budget,
        &cancel,
    )?;
    let document: GraphDocument = serde_json::from_value(response["document"].clone())?;
    let execution = GraphExecution {
        peer_origin_context: None,
        peer_submission_digest: None,
        requester_path: None,
        extra: Default::default(),
        bases: None,
        directory: factory.cwd.clone(),
        document: document.clone(),
    };
    // The recovery tests cover publication at each transition. Here the
    // property is the size and round trip of the fully populated snapshot;
    // publish it once instead of fsyncing the same large brief per start.
    let first = graph::advance_with_budget(
        &document,
        &mut Waiting,
        &mut Snapshot::default(),
        &factory.context.config.graph.limits,
        &factory.context.config.graph.budget,
    )?;
    {
        let mut journal = FileJournal::open(
            &factory.context.roots.state,
            factory.context.identity.project,
            &document.graph_id,
            budget,
            &cancel,
            &factory.clock,
        )?;
        journal.bind(execution.clone())?;
        journal.save(&first.checkpoint)?;
    }
    assert_eq!(first.status, GraphStatus::Pending);
    assert_eq!(
        first
            .checkpoint
            .nodes
            .iter()
            .filter(|node| !node.attempts.is_empty())
            .count(),
        8
    );
    let inline_bytes =
        serde_json::to_vec(&json!({"execution":execution,"checkpoint":first.checkpoint}))?.len();
    let checkpoint_path = rig
        .state
        .join("fabric/graphs")
        .join(factory.context.identity.project.to_string())
        .join(format!("graph-{}.json", document.graph_id));
    let stored = std::fs::read(checkpoint_path)?;
    let limit = 16_777_216;
    assert!(
        inline_bytes > limit,
        "fixture must exceed the former inline limit: {inline_bytes}"
    );
    assert!(stored.len() < limit, "checkpoint is {} bytes", stored.len());
    let envelope: Value = serde_json::from_slice(&stored)?;
    assert!(
        !envelope["stored_text"]
            .as_object()
            .ok_or("prompt references")?
            .is_empty()
    );
    // Short node titles remain readable; the full file brief belongs only
    // in the prompt store, including its copies in submission metadata.
    // Inspect decoded strings: a raw-byte absence check would always pass
    // for escaped briefs, including those with ordinary newlines.
    let mut values = vec![&envelope];
    while let Some(value) = values.pop() {
        match value {
            Value::String(text) if text.len() >= brief.len() => {
                assert!(!text.contains(&brief));
            }
            Value::Array(items) => values.extend(items),
            Value::Object(fields) => values.extend(fields.values()),
            _ => {}
        }
    }
    assert_eq!(
        FileJournal::read_checkpoint(
            &factory.context.roots.state,
            factory.context.identity.project,
            &document.graph_id,
        )?,
        Some(first.checkpoint)
    );
    assert_eq!(
        FileJournal::execution(
            &factory.context.roots.state,
            factory.context.identity.project,
            &document.graph_id,
        )?,
        Some(execution)
    );
    Ok(())
}

#[test]
fn submission_snapshots_repo_templates_and_task_over_has_distinct_briefs() -> Result {
    let rig = TestRig::new()?;
    let factory = factory(&rig)?;
    let directory = rig.project.join(".provenant/templates");
    std::fs::create_dir_all(&directory)?;
    std::fs::write(
        directory.join("review.local.toml"),
        "brief = \"Review local invariants\"\n",
    )?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
    let response = factory.graph_document(
        json!({"op":"run", "template":"task", "over":["alpha","beta"], "explain":true}),
        budget,
        &cancel,
    )?;
    let document: GraphDocument = serde_json::from_value(response["document"].clone())?;
    assert_eq!(
        document.template_library["review.local"].brief.as_deref(),
        Some("Review local invariants")
    );
    assert!(document.template_resolvers.contains_key("review-panel"));
    let expanded = document.expanded(&factory.context.config.graph.limits)?;
    let prompts: Vec<_> = expanded
        .nodes
        .iter()
        .filter(|node| node.composite.is_none())
        .map(|node| node.prompt.as_str())
        .collect();
    assert_eq!(prompts.len(), 2);
    assert!(prompts[0].contains("alpha"));
    assert!(prompts[1].contains("beta"));
    assert_ne!(prompts[0], prompts[1]);
    Ok(())
}

#[test]
fn detached_run_is_a_lane_child_control() -> Result {
    let rig = TestRig::new()?;
    let mut factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    let input = json!({"op":"run","brief":"Inspect","detached":true,"explain":true});
    assert!(
        factory
            .graph_document(input.clone(), budget, &cancel)
            .is_err()
    );
    factory.context.in_lane = true;
    let preview = factory.graph_document(input, budget, &cancel)?;
    assert_eq!(preview["detached"], true);
    Ok(())
}

#[test]
fn graph_operations_refuse_operation_ids_and_name_the_replacement() -> Result {
    let rig = TestRig::new()?;
    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    let input = json!({"op":"run","brief":"Inspect","explain":true,"operation_id":"retired"});
    let error = factory.graph_document(input, budget, &cancel).unwrap_err();
    assert_eq!(
        error,
        FacadeError::invalid(InvalidInputDetail::OperationIdRetired)
    );
    assert!(error.to_string().contains("use `name`"), "{error}");
    Ok(())
}

#[test]
fn child_launch_preserves_an_unregistered_selected_project_directory() -> Result {
    let rig = TestRig::new()?;
    let mut factory = factory(&rig)?;
    let scratch = rig.root.join("scratch-repository");
    std::fs::create_dir_all(&scratch)?;
    let output = provenant_os::fixture::git_command()
        .arg("init")
        .arg("-q")
        .arg(&scratch)
        .output()?;
    assert!(output.status.success());
    factory.cwd = scratch.clone();
    factory.context = Context::open_inspection(factory.context.roots.clone(), &rig.home, &scratch)?;
    let request = factory
        .child_request(&json!({"op":"run","brief":"Read the selected scratch repository"}))?;
    assert_eq!(
        request.launch.as_ref().ok_or("child launch")?.node["attempt"]["directory"],
        json!(scratch)
    );
    assert!(!request.writer);
    Ok(())
}

#[test]
fn child_launch_retains_writer_access_and_has_a_stable_logical_identity() -> Result {
    let rig = TestRig::new()?;
    let factory = factory(&rig)?;
    let input = json!({"op":"run", "template":"implement", "brief":"Fix the parser", "set":{"plan":false}, "idempotency_key":"parser-launch"});
    let first = factory.child_request(&input)?;
    let unkeyed =
        json!({"op":"run", "template":"implement", "brief":"Fix the parser", "set":{"plan":false}});
    assert_ne!(
        factory.child_request(&unkeyed)?.id,
        factory.child_request(&unkeyed)?.id
    );
    let operation = json!({"op":"run","document":{"use":"task","prompt":"Nested operation","idempotency_key":"child-replay"}});
    assert_eq!(
        factory.child_request(&operation)?,
        factory.child_request(&operation)?
    );
    assert_eq!(
        factory.child_request(&operation)?.idempotency_key,
        "child-replay"
    );
    assert!(first.writer);
    assert_eq!(
        first.launch.as_ref().unwrap().node["access"],
        "worktree_write"
    );
    let mut retry = input.clone();
    retry["explain"] = true.into();
    retry["wait_seconds"] = 30.into();
    assert_eq!(factory.child_request(&retry)?, first);
    assert_ne!(
        factory
            .child_request(
                &json!({"op":"run", "template":"implement", "brief":"Fix another parser"})
            )?
            .id,
        first.id
    );
    let bound = factory.child_request(&json!({"op":"run", "document":{"use":"implement", "with":{"plan":false}, "prompt":"Bound writer", "worktree":".worktrees/existing"}}))?;
    let controlled = factory.child_request(&json!({"op":"run", "template":"implement",
            "brief":"Bound controls", "branch":"fix/child", "base":"HEAD",
            "write_scope":["src"], "detached":true}))?;
    // The launch controls belong to the writer leaf, whether it is direct
    // or nested below an explicitly requested planner.
    fn writer_leaf(node: &Value) -> Option<&Value> {
        match node.pointer("/composite/nodes").and_then(Value::as_array) {
            Some(children) => children.iter().find_map(writer_leaf),
            None => (node["access"] == "worktree_write").then_some(node),
        }
    }
    let node = writer_leaf(&controlled.launch.as_ref().unwrap().node).ok_or("writer leaf")?;
    let ports = provenant_config::PortTable::read_for(Some(&rig.project), &rig.state)?;
    assert_eq!(
        node["worktree"],
        json!(rig.project.join(ports.managed_writer_path("fix/child")?)),
    );
    assert_eq!(
        node["attempt"]["new_worktree"],
        json!({"branch":"fix/child", "base":"HEAD"})
    );
    assert_eq!(node["write_scope"], json!(["src"]));
    assert!(controlled.has_writers());
    assert!(controlled.detached);
    let nested = json!({"op":"run", "document":{"use":"implement", "with":{"plan":false}, "prompt":"Nested replay",
            "idempotency_key":"nested-child", "detached":true}});
    let nested_first = factory.child_request(&nested)?;
    assert_eq!(factory.child_request(&nested)?, nested_first);
    assert!(nested_first.detached);
    let mut top = nested.clone();
    top["idempotency_key"] = json!("top-child");
    top["detached"] = json!(false);
    let top = factory.child_request(&top)?;
    assert_eq!(top.idempotency_key, "top-child");
    assert_ne!(top.id, nested_first.id);
    assert!(!top.detached);
    assert!(bound.writer);
    assert!(!bound.needs_worktree_provisioning());
    assert_eq!(
        bound.launch.as_ref().unwrap().node["worktree"],
        ".worktrees/existing"
    );
    Ok(())
}

#[test]
fn authored_documents_validate_schema_version_before_compilation() -> Result {
    let rig = TestRig::new()?;
    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
    for document in [
        json!({"schema_version":3, "use":"task"}),
        json!({"schema_version":3, "graph_id":"invalid-version", "nodes":[]}),
        json!({"schema_version":3, "graph_id":"invalid-version", "templates":{}, "nodes":[{"id":"task", "use":"task"}]}),
    ] {
        let input = json!({"op":"run", "document":document, "explain":true});
        let error = factory.child_request(&input).unwrap_err();
        assert!(error.to_string().contains("schema_version"), "{error}");
        let error = factory.graph_document(input, budget, &cancel).unwrap_err();
        assert!(error.to_string().contains("schema_version"), "{error}");
    }
    Ok(())
}

#[test]
fn child_document_refuses_unapplied_controls_by_field_name() -> Result {
    let rig = TestRig::new()?;
    let factory = factory(&rig)?;
    for field in [
        "network",
        "capability",
        "no_capability",
        "integration_branch",
        "limits",
        "delivery",
        "review_base",
        "decision_session",
        "retry",
        "exclude",
    ] {
        for mut document in [
            json!({"use":"task", "prompt":"Inspect"}),
            json!({"graph_id":"child", "schema_version":2, "nodes":[]}),
        ] {
            document[field] = json!(null);
            let error = factory
                .child_request(&json!({"op":"run", "document":document}))
                .unwrap_err();
            assert!(error.to_string().contains(field), "{field}: {error}");
        }
    }
    Ok(())
}

#[test]
fn child_launch_compiles_over_set_and_inline_documents() -> Result {
    let rig = TestRig::new()?;
    let factory = factory(&rig)?;
    let request = factory.child_request(&json!({
        "op":"run", "document":{
            "parameters":{"subject":{"type":"string", "default":"default"}},
            "nodes":[{"id":"one", "use":"task", "prompt":"Inspect ${subject} ${item}"}]
        }, "set":{"subject":"code"}, "over":["a","b"], "route":"junior"
    }))?;
    assert!(!request.writer);
    let node = &request.launch.as_ref().unwrap().node;
    fn prompts(node: &Value, found: &mut Vec<String>) {
        if let Some(nodes) = node.pointer("/composite/nodes").and_then(Value::as_array) {
            for child in nodes {
                prompts(child, found);
            }
        } else if let Some(prompt) = node["prompt"].as_str() {
            found.push(prompt.into());
            assert_eq!(node["route"]["alias"], "junior");
        }
    }
    let mut found = Vec::new();
    prompts(node, &mut found);
    assert_eq!(found, ["Inspect code a", "Inspect code b"]);
    assert!(
        request
            .launch
            .as_ref()
            .unwrap()
            .templates
            .contains_key("lane-document")
    );
    let request = factory.child_request(&json!({
        "op":"run", "document":{"graph_id":"child-doc", "nodes":[
            {"id":"inspect", "use":"task", "prompt":"Document child"}
        ]}
    }))?;
    assert_eq!(
        request.launch.as_ref().unwrap().node["composite"]["nodes"][0]["prompt"],
        "Document child"
    );
    let request = factory.child_request(
        &json!({"op":"run", "template":"implement", "brief":"Fix ${item}", "over":["a","b"]}),
    )?;
    assert!(!request.writer);
    let children = request.launch.as_ref().unwrap().node["composite"]["nodes"]
        .as_array()
        .unwrap();
    assert_eq!(children.len(), 2);
    assert!(
        children
            .iter()
            .all(|node| node["access"] == "worktree_write")
    );
    Ok(())
}

fn exercise_child_launch(mut input: Value, count: usize, disjoint: bool) -> Result {
    use provenant_fabric_runtime::graph::{GraphCheckpoint, GraphExecution, relay::RelayOp};
    let rig = TestRig::new()?;
    crate::worktree_tests::git(&rig, &["commit", "--allow-empty", "-m", "source"])?;
    let mut source = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
    let branch = crate::worktree_tests::git(&rig, &["symbolic-ref", "--short", "HEAD"])?;
    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    let foreign_checkout = input["worktree"] == "$foreign";
    let explicit_checkout = foreign_checkout || input["worktree"] == "$registered";
    if foreign_checkout {
        let selected = rig.root.join("foreign-project");
        std::fs::create_dir_all(&selected)?;
        for args in [
            vec!["init", "-q"],
            vec!["commit", "--allow-empty", "-qm", "foreign source"],
        ] {
            let result = provenant_os::fixture::git_command()
                .current_dir(&selected)
                .args(args)
                .output()?;
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
        let result = provenant_os::fixture::git_command()
            .current_dir(&selected)
            .args(["rev-parse", "HEAD"])
            .output()?;
        assert!(result.status.success());
        source = String::from_utf8(result.stdout)?.trim().to_owned();
        bind_fixture_project(&factory.context.roots, &selected)?;
        let selected_context = Context::open(factory.context.roots.clone(), &rig.home, &selected)?;
        provenant_cli::fabric::graph_relay::provision_child(
            &selected_context,
            ".worktrees/explicit-child",
            &source,
            WaitBudget::cli(factory.clock.mono_now(), 30)?,
            &cancel,
        )?;
        input["worktree"] = json!(selected.join(".worktrees/explicit-child"));
    } else if explicit_checkout {
        provenant_cli::fabric::graph_relay::provision_child(
            &factory.context,
            ".worktrees/explicit-child",
            &source,
            WaitBudget::cli(factory.clock.mono_now(), 30)?,
            &cancel,
        )?;
        input["worktree"] = json!(rig.project.join(".worktrees/explicit-child"));
    }
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":2, "graph_id":"g-child-e2e", "template_library":{}, "integration_branch":if explicit_checkout { None } else { Some(&branch) },
        "nodes":[{"id":"parent", "access":"read_only", "prompt":"Go", "route":{"alias":"senior"}}]
    }))?;
    let reference = format!("{}/caller", factory.context.identity.host);
    let bases = if explicit_checkout {
        None
    } else {
        Some(
            provenant_fabric_runtime::graph::GraphBaseBinding::new(
                None,
                Some(provenant_core::GitOid::parse(&source)?),
            )
            .ok_or("integration base")?,
        )
    };
    let mut checkpoint: GraphCheckpoint = serde_json::from_value(json!({
        "schema_version":2, "graph_id":"g-child-e2e", "plan_digest":"fixture",
        "nodes":[{"id":"parent", "state":"running", "attempts":[{
            "launched":true, "key":{"graph_id":"g-child-e2e", "node_id":"parent", "route_index":0, "attempt": 1},
            "route":{"alias":"senior"}, "state":"running", "attempt_ref":reference, "outcome":null, "launch_actor":"fixture"
        }]}]
    }))?;
    checkpoint.bases = bases;
    {
        let mut journal = FileJournal::open(
            &factory.context.roots.state,
            factory.context.identity.project,
            &document.graph_id,
            budget,
            &cancel,
            &factory.clock,
        )?;
        journal.bind(GraphExecution {
            peer_origin_context: None,
            peer_submission_digest: None,
            requester_path: None,
            extra: Default::default(),
            bases,
            directory: rig.project.clone(),
            document: document.clone(),
        })?;
        journal.prepare_fixture_checkpoint(&mut checkpoint)?;
        journal.save(&checkpoint)?;
    }
    crate::worktree_tests::git(&rig, &["commit", "--allow-empty", "-m", "host head moved"])?;
    let context = Context::open(factory.context.roots.clone(), &rig.home, &rig.project)?;
    // Retain TMPDIR's short alias: the canonical attempt path can
    // exceed Unix SUN_LEN even though the owned scratch is writable.
    let temporary = std::env::temp_dir();
    let relative = rig.root.strip_prefix(std::fs::canonicalize(&temporary)?)?;
    let socket = temporary.join(relative).join("r/s");
    let _relay = provenant_fabric_runtime::relay::HostRelay::start_with_graph(
        socket.clone(),
        Default::default(),
        |_, _, _| Err(ServiceError::FabricOwnerUnavailable),
        move |arguments, cancel: &CancelToken, _| {
            let clock = SystemClock;
            let budget = WaitBudget::cli(clock.mono_now(), 30)
                .map_err(|_| ServiceError::FabricOwnerUnavailable)?;
            provenant_cli::fabric::graph_relay::serve(
                &context, "caller", arguments, budget, cancel, &clock,
            )
        },
    )?;
    {
        let request = factory.child_request(&input)?;
        let frame = serde_json::to_value(RelayOp::Add(request))?;
        let receipt = provenant_fabric_runtime::relay::graph(
            &socket,
            frame.clone(),
            &cancel,
            std::time::Duration::from_secs(30),
        )?;
        let replay = provenant_fabric_runtime::relay::graph(
            &socket,
            frame,
            &cancel,
            std::time::Duration::from_secs(30),
        )?;
        assert_eq!(replay["replayed"], true);
        assert_eq!(replay["node"], receipt["node"]);
        let checkpoint = FileJournal::read_checkpoint(
            &factory.context.roots.state,
            factory.context.identity.project,
            &document.graph_id,
        )?
        .ok_or("missing checkpoint")?;
        assert_eq!(checkpoint.expansions.len(), 1);
        let prefix = receipt["node"].as_str().ok_or("missing child")?;
        let writers: Vec<_> = checkpoint.expansions[0]
            .nodes
            .iter()
            .filter(|node| {
                node.access == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite
                    && (node.id == prefix || node.id.starts_with(&format!("{prefix}--")))
            })
            .collect();
        assert_eq!(writers.len(), count);
        let mut paths = std::collections::BTreeSet::new();
        for writer in writers {
            let path = writer.worktree.as_ref().ok_or("missing writer worktree")?;
            let unique = paths.insert(path.clone());
            if disjoint {
                assert!(unique);
            }
            assert_eq!(
                crate::worktree_tests::git(&rig, &["-C", path, "rev-parse", "HEAD"])?,
                source
            );
            let mut owner = BoundGraphOwner {
                writer_launch_budget: None,
                admission_timeout: None,
                launch_warnings: Vec::new(),
                repo_facts: Default::default(),
                factory: &factory,
                integration_base: None,
                integration_branch: None,
                network: None,
                capability_overrides: Default::default(),
                exclude: Vec::new(),
                budget,
                cancel: &cancel,
                prices: None,
                tick: false,
            };
            let bound = provenant_fabric_runtime::graph::AttemptOwner::bind_writer_worktree(
                &mut owner, path,
            )?
            .ok_or("writer binding")?;
            assert_eq!(bound.head, source);
            if foreign_checkout {
                let state = factory.context.roots.state.as_path();
                let request =
                    provenant_run::resolve_writer(state, Path::new(path), Path::new(path))?;
                let selected = request.custody_project.ok_or("resolved custody project")?;
                assert_ne!(selected, factory.context.identity.project);
                let name = provenant_confinement::worktrees::state_dir_name(Path::new(path));
                assert!(
                    state
                        .join("worktrees")
                        .join(selected.to_string())
                        .join(&name)
                        .is_dir()
                );
                assert!(
                    !state
                        .join("worktrees")
                        .join(factory.context.identity.project.to_string())
                        .join(&name)
                        .exists()
                );
                assert!(
                    state
                        .join("writer-projects")
                        .join(factory.context.identity.project.to_string())
                        .join(selected.to_string())
                        .is_dir()
                );
                let mut forged = request;
                forged.custody_project = Some(factory.context.identity.project);
                assert!(
                    provenant_run::prepare_writer(&forged, state, factory.context.identity.project)
                        .is_err()
                );
            }

            assert_eq!(
                writer.delivery,
                (!explicit_checkout)
                    .then(|| { provenant_records::run::DeliveryMode::Integrate(branch.clone()) })
            );
            assert_eq!(
                writer.source,
                (!explicit_checkout).then(|| {
                    provenant_fabric_runtime::graph::SourceBinding::Commit {
                        commit: source.clone(),
                    }
                })
            );
        }
    }
    Ok(())
}

#[test]
fn explicit_writer_children_use_their_selected_project_custody() -> Result {
    for worktree in ["$foreign", "$registered"] {
        exercise_child_launch(
            json!({"op":"run", "brief":"Write from a read-only parent", "access":"worktree_write", "worktree":worktree}),
            1,
            false,
        )?;
    }
    Ok(())
}

#[test]
fn child_launch_crosses_the_lane_transport_and_provisions_writer_fanout() -> Result {
    for (input, count, disjoint) in [
        (
            json!({"op":"run", "template":"implement", "brief":"Fix the parser"}),
            1,
            false,
        ),
        (
            json!({"op":"run", "template":"implement", "brief":"Fix ${item}", "over":["one","two"]}),
            2,
            false,
        ),
        (
            json!({"op":"run", "document":{
                "parameters":{"subject":{"type":"string"}},
                "nodes":[{"id":"write", "use":"implement", "prompt":"Fix ${subject}"}]
            }, "set":{"subject":"inline"}}),
            1,
            false,
        ),
        (
            json!({"op":"run", "document":{"use":"implement", "prompt":"Fix ${item}", "write_scope":["${item}"]},
                "over":["three","four"]}),
            2,
            true,
        ),
    ] {
        exercise_child_launch(input, count, disjoint)?;
    }
    Ok(())
}

#[test]
fn child_launch_retains_agent_roots_after_template_parameters_are_bound() -> Result {
    let rig = TestRig::new()?;
    let factory = factory(&rig)?;
    let escaping = json!({"op":"run", "document":{
            "templates":{"escaped":{
                "brief":"Inspect", "tier":"senior", "access":"read_only",
                "parameters":{"root":{"type":"string"}},
                "node":{"read_roots":["${root}"]}
            }}, "use":"escaped"
        }, "set":{"root":rig.home}});
    let request = factory.child_request(&escaping)?;
    let launch = request.launch.ok_or("missing child launch")?;
    assert_eq!(launch.node["read_roots"], json!([rig.home]));
    Ok(())
}

#[test]
fn explanation_and_preparation_share_worktree_admission() -> Result {
    use provenant_cli::run::{SourceAdmissionMode, admit_source_root};
    let rig = crate::worktree_tests::repository()?;
    let locations = [
        rig.project.join(".worktrees/source"),
        rig.project.join(".claude/worktrees/source"),
        rig.root.join("external-source"),
        rig.home.join(".claude/worktrees/credential-source"),
    ];
    for (index, path) in locations.iter().enumerate() {
        crate::worktree_tests::git(
            &rig,
            &[
                "worktree",
                "add",
                "-b",
                &format!("source-{index}"),
                path.to_str().ok_or("fixture path")?,
                "HEAD",
            ],
        )?;
        let mut factory = factory(&rig)?;
        factory.cwd = std::fs::canonicalize(path)?;
        let preparation = admit_source_root(
            &factory.cwd,
            &rig.state,
            &factory.cwd,
            SourceAdmissionMode::Live,
        );
        let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
        let preview = factory.graph_document(
            json!({"op":"run","brief":"Inspect source","explain":true}),
            budget,
            &CancelToken::new(),
        )?;
        let rows = preview["source_admission"]
            .as_array()
            .ok_or("admission rows")?;
        assert!(!rows.is_empty());
        let admission = preparation?;
        assert!(rows.iter().all(|row| row["status"] == "pending_route"
            && row["credential_admission"] == "pending_route"));
        assert!(rows.iter().all(|row| row["path"] == json!(admission.path)));
        assert_eq!(preview["status"], "pending_route");
        assert_eq!(preview["credential_admission"], "pending_route");
        if path.starts_with(rig.project.join(".claude"))
            || path.starts_with(rig.home.join(".claude"))
        {
            assert!(!admission.warnings.is_empty());
            assert!(
                rows.iter()
                    .all(|row| row["warnings"].as_array().is_some_and(|warnings| {
                        warnings.iter().any(|warning| {
                            warning["source"]
                                .as_str()
                                .is_some_and(|source| !source.is_empty())
                        })
                    }))
            );
        }
        assert!(!rig.state.join("writer-git-home").exists());
    }
    Ok(())
}

#[test]
fn graph_run_explanation_compiles_brief_tier_over_and_parameters() -> Result {
    let rig = TestRig::new()?;
    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
    let response = factory.graph_document(json!({"op":"run","brief":"Inspect ${item}","route":"junior","over":["one","two"],"explain":true}),budget,&cancel)?;
    let document: GraphDocument = serde_json::from_value(response["document"].clone())?;
    let expanded = document.expanded(&factory.context.config.graph.limits)?;
    let leaves: Vec<_> = expanded
        .nodes
        .iter()
        .filter(|node| node.composite.is_none())
        .collect();
    assert_eq!(leaves.len(), 2);
    assert!(leaves.iter().all(|node| {
        node.route.as_ref().and_then(|route| route.alias.as_deref()) == Some("junior")
    }));
    let prompts: Vec<_> = leaves
        .iter()
        .map(|node| Some(node.prompt.as_str()))
        .collect();
    assert_eq!(prompts, vec![Some("Inspect one"), Some("Inspect two")]);
    let response = factory.graph_document(json!({"op":"run","document":{"use":"task","prompt":"Inspect ${item}"},"over":["one","two"],"explain":true}), budget, &cancel)?;
    let doc: GraphDocument = serde_json::from_value(response["document"].clone())?;
    let prompts: Vec<_> = doc
        .expanded(&factory.context.config.graph.limits)?
        .nodes
        .into_iter()
        .filter(|node| node.composite.is_none())
        .map(|node| node.prompt)
        .collect();
    assert_eq!(prompts, vec!["Inspect one", "Inspect two"]);
    let response = factory.graph_document(json!({"op":"run","document":{"parameters":{"subject":{"type":"string"}},"nodes":[{"id":"one","use":"task","prompt":"Inspect ${subject} ${item}"}]},"set":{"subject":"code"},"over":["a","b"],"explain":true}), budget, &cancel)?;
    let doc: GraphDocument = serde_json::from_value(response["document"].clone())?;
    let prompts: Vec<_> = doc
        .expanded(&factory.context.config.graph.limits)?
        .nodes
        .into_iter()
        .filter(|node| node.composite.is_none())
        .map(|node| node.prompt)
        .collect();
    assert_eq!(prompts, vec!["Inspect code a", "Inspect code b"]);
    let response = factory.graph_document(
            json!({"op":"run","template":"task-once","brief":"Parameter brief","route":"junior","set":{},"explain":true}),
            budget,
            &cancel,
        )?;
    let document: GraphDocument = serde_json::from_value(response["document"].clone())?;
    assert!(
        document
            .expanded(&factory.context.config.graph.limits)?
            .nodes
            .iter()
            .any(|node| Some(node.prompt.as_str()) == Some("Parameter brief"))
    );
    assert!(
        factory
            .graph_document(json!({"op":"run","document":"graph.json"}), budget, &cancel)
            .is_err()
    );
    Ok(())
}

#[test]
fn graph_context_secret_is_retained_in_authorised_preview() -> Result {
    // Spec: docs/specs/fabric.md#checkpoint-envelope
    let rig = TestRig::new()?;
    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
    let secret = "sk-abcdefghijklmnopqrstuvwx";
    let context = json!({(secret):"retained note"});
    let response = factory.graph_document(
        json!({
            "op":"run", "explain":true,
            "document":{"graph_id":"metadata-secret", "context":context,
                "nodes":[{"id":"inspect", "use":"task", "prompt":"Inspect code"}]}
        }),
        budget,
        &cancel,
    )?;
    assert_eq!(response["document"]["context"], context);
    assert!(
        FileJournal::graph_ids(
            &factory.context.roots.state,
            factory.context.identity.project
        )?
        .is_empty()
    );
    Ok(())
}

#[test]
fn generated_graph_id_preserves_document_binding_and_settings() -> Result {
    let rig = TestRig::new()?;
    declare_forge(&rig, "main")?;
    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
    let settings = json!({
        "integration_branch":"batch/document",
        "limits":{"nodes":128},
        "delivery":"none",
        "review_base":"1111111111111111111111111111111111111111",
        "context":{"risk_tier":"substantial", "task_kind":"implementation"},
        "decision_session":"chair-session",
        "review":{"risk_tier":"substantial"},
        "retry":{"retries":1},
        "exclude":["codex/gpt-6-luna"],
        "network":"off",
        "capability":["postgres"],
        "no_capability":[]
    });
    let mut source = settings.clone();
    source["nodes"] = json!([{
        "id":"inspect", "use":"task", "source":{"type":"integration"}
    }]);
    let response = factory.graph_document(
        json!({"op":"run", "document":source, "explain":true}),
        budget,
        &cancel,
    )?;
    let compiled = &response["document"];
    assert!(compiled["graph_id"].as_str().is_some_and(|id| {
        provenant_fabric_runtime::graph::template::fold_graph_name(id).is_ok()
    }));
    // Compare declarations after decoding: empty default fields may be omitted on the wire.
    let document: GraphDocument = serde_json::from_value(compiled.clone())?;
    let mut expected = compiled.clone();
    expected
        .as_object_mut()
        .ok_or("compiled document")?
        .extend(settings.as_object().ok_or("settings object")?.clone());
    let expected: GraphDocument = serde_json::from_value(expected)?;
    assert_eq!(document, expected);
    for invalid_branch in [
        json!(7),
        json!({"branch":"batch/document"}),
        json!(["batch/document"]),
    ] {
        let error = factory.graph_document(json!({
                "op":"run", "explain":true,
                "document":{"integration_branch":invalid_branch,"nodes":[{"id":"task","use":"task"}]}
            }), budget, &cancel).err().ok_or("invalid branch was accepted")?;
        assert_eq!(
            error,
            FacadeError::invalid(InvalidInputDetail::Field("integration_branch".into()))
        );
    }
    assert!(
        document
            .expanded(&factory.context.config.graph.limits)?
            .nodes
            .iter()
            .any(|node| {
                matches!(
                    node.source,
                    Some(provenant_fabric_runtime::graph::SourceBinding::Integration)
                )
            })
    );
    Ok(())
}

#[test]
fn explicit_batch_node_and_cli_branch_parameters_override_document_binding() -> Result {
    let rig = TestRig::new()?;
    declare_forge(&rig, "main")?;
    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
    for (node_parameters, parameters, expected) in [
        (json!({}), json!({}), "batch/document"),
        (
            json!({"integration_branch":"batch/node"}),
            json!({}),
            "batch/node",
        ),
        (
            json!({"integration_branch":"batch/node"}),
            json!({"integration_branch":"batch/cli"}),
            "batch/cli",
        ),
    ] {
        let mut bindings = json!({"worktree":rig.project,"write_scope":["."]});
        bindings.as_object_mut().ok_or("bindings object")?.extend(
            node_parameters
                .as_object()
                .ok_or("parameters object")?
                .clone(),
        );
        let response = factory.graph_document(
            json!({
                "op":"run", "explain":true, "set":parameters,
                "document":{
                    "use":"batch", "integration_branch":"batch/document",
                    "with":bindings
                }
            }),
            budget,
            &cancel,
        )?;
        assert_eq!(response["document"]["integration_branch"], expected);
        let document: GraphDocument = serde_json::from_value(response["document"].clone())?;
        assert!(
            document
                .expanded(&factory.context.config.graph.limits)?
                .nodes
                .iter()
                .any(|node| {
                    node.delivery
                        == Some(provenant_records::run::DeliveryMode::Integrate(
                            expected.into(),
                        ))
                })
        );
    }
    Ok(())
}

#[test]
fn writer_sourced_template_review_selects_shape_and_pins_every_stage() -> Result {
    use provenant_fabric_runtime::graph::{self, AttemptOwner};
    #[derive(Default)]
    struct Journal(Option<graph::GraphCheckpoint>);
    impl graph::GraphJournal for Journal {
        type Error = std::convert::Infallible;
        fn load(
            &mut self,
            _: &str,
        ) -> std::result::Result<Option<graph::GraphCheckpoint>, Self::Error> {
            Ok(self.0.clone())
        }
        fn save(
            &mut self,
            checkpoint: &graph::GraphCheckpoint,
        ) -> std::result::Result<(), Self::Error> {
            self.0 = Some(checkpoint.clone());
            Ok(())
        }
    }
    struct Owner {
        source: String,
        commit: String,
        reviewed: BTreeMap<String, Option<String>>,
    }
    impl AttemptOwner for Owner {
        type Error = std::convert::Infallible;
        fn worktree_layout(&self) -> provenant_config::WorktreeLayoutPort {
            provenant_config::PortTable::builtin().worktree_layout
        }
        fn actor(&self) -> String {
            "template-review-fixture".into()
        }
        fn bind_writer_worktree(
            &mut self,
            worktree: &str,
        ) -> std::result::Result<Option<graph::WriterWorktreeBinding>, Self::Error> {
            Ok(Some(graph::WriterWorktreeBinding {
                extra: Default::default(),
                shared_worktree: None,
                source_directory_identity: None,
                worktree: worktree.into(),
                head: self.source.clone(),
            }))
        }
        fn observe_or_start(
            &mut self,
            key: &graph::AttemptKey,
            node: graph::NodeInput<'_>,
            _: &graph::GraphRoute,
            access: graph::AccessMode,
            _: Option<&str>,
            _: graph::Replay<'_>,
        ) -> std::result::Result<graph::AttemptObservation, Self::Error> {
            let writer = key.node_id == "writer";
            if !writer {
                assert_eq!(access, graph::AccessMode::ReadOnly);
                self.reviewed
                    .insert(key.node_id.clone(), node.source_commit.map(str::to_owned));
            }
            let mut result =
                provenant_records::result_capture::ResultCapture::new(node.required_verdict);
            result
                .push(b"```findings\n{\"schema_version\":1,\"findings\":[]}\n```\nVERDICT: PASS\n");
            Ok(graph::AttemptObservation::Terminal {
                attempt_ref: Some(format!("fixture/{}", key.node_id)),
                outcome: graph::AttemptOutcome::Succeeded,
                launched: true,
                result: Some(Box::new(result.finish())),
                result_warning: None,
                result_path: None,
                branch: writer.then(|| "slice".into()),
                commit: writer.then(|| self.commit.clone()),
                settlement_reason: Some(provenant_records::run::SettlementReason::Completed),
                stop: None,
            })
        }
    }
    let rig = crate::worktree_tests::repository()?;
    let original = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
    crate::worktree_tests::git(&rig, &["commit", "--allow-empty", "-m", "writer subject"])?;
    let writer_commit = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
    // Push delivery must target a writer checkout on its own branch, not the
    // primary checkout's protected default.
    let writer_tree = rig.root.join("writer-subject-worktree");
    crate::worktree_tests::git(
        &rig,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "writer-subject",
            writer_tree.to_str().ok_or("writer worktree")?,
        ],
    )?;
    declare_forge(&rig, "main")?;
    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    for (risk, full_panel) in [("routine", false), ("substantial", true)] {
        let input = json!({
            "op":"run", "explain":true,
            "document":{
                "schema_version":2, "graph_id":format!("writer-review-{risk}"),
                "review_base":original, "context":{"risk_tier":risk},
                "nodes":[
                    {"id":"writer", "use":"implement", "with":{"plan":false}, "worktree":writer_tree, "write_scope":["."], "delivery":"push"},
                    {"id":"review", "use":"review-panel", "after":["writer"], "source":{"type":"head","node":"writer"}}
                ]
            }
        });
        let response = factory
            .graph_document(input, budget, &cancel)
            .map_err(|error| format!("{risk} public template submission: {error:?}"))?;
        let document: GraphDocument = serde_json::from_value(response["document"].clone())?;
        let expanded = document.expanded(&factory.context.config.graph.limits)?;
        let review_stages: std::collections::BTreeSet<String> = expanded
            .nodes
            .iter()
            .filter(|node| node.id.starts_with("review--") && node.composite.is_none())
            // A stage whose `when` does not hold stays as a skipped row and never launches.
            .filter(|node| node.when.iter().all(graph::WhenTerm::holds))
            .map(|node| node.id.clone())
            .collect();
        for stage in ["frame", "verify", "synthesis"] {
            assert_eq!(
                review_stages
                    .iter()
                    .any(|id| id.contains(&format!("--{stage}"))),
                full_panel,
                "{risk}: {stage}"
            );
        }
        if !full_panel {
            assert_eq!(review_stages.len(), 1);
        }
        // A moving checkout cannot change the immutable commit supplied by
        // the completed writer to the review template and all its stages.
        crate::worktree_tests::git(&rig, &["commit", "--allow-empty", "-m", "later head"])?;
        assert_ne!(
            crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?,
            writer_commit
        );
        let mut owner = Owner {
            source: original.clone(),
            commit: writer_commit.clone(),
            reviewed: BTreeMap::new(),
        };
        let run = graph::advance_with_budget(
            &document,
            &mut owner,
            &mut Journal::default(),
            &factory.context.config.graph.limits,
            &factory.context.config.graph.budget,
        )?;
        assert_eq!(run.status, graph::GraphStatus::Complete);
        assert!(run.checkpoint.successful(), "{risk}: {:?}", run.checkpoint);
        assert_eq!(
            owner
                .reviewed
                .keys()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>(),
            review_stages
        );
        for (id, source) in owner.reviewed {
            assert_eq!(
                source.as_deref(),
                Some(writer_commit.as_str()),
                "{risk}: {id}"
            );
            assert_eq!(
                run.checkpoint
                    .nodes
                    .iter()
                    .find(|node| node.id == id)
                    .ok_or("review checkpoint")?
                    .source_commit
                    .as_deref(),
                Some(writer_commit.as_str())
            );
        }
    }
    Ok(())
}

#[test]
fn graph_edit_is_revisioned_replayable_and_journalled() -> Result {
    let rig = TestRig::new()?;
    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
    let document: GraphDocument = serde_json::from_value(
        factory.graph_document(
            json!({"op":"run","brief":"Initial","explain":true}),
            budget,
            &cancel,
        )?["document"]
            .clone(),
    )?;
    let id = document.graph_id.clone();
    let mut checkpoint = serde_json::from_value(
        json!({"schema_version":2,"graph_id":id,"plan_digest":"test","nodes":[{"id":"task","worktree":null,"state":"running","attempts":[]}]}),
    )?;
    let mut journal = FileJournal::open(
        &factory.context.roots.state,
        factory.context.identity.project,
        &id,
        budget,
        &cancel,
        &factory.clock,
    )?;
    journal.bind(provenant_fabric_runtime::graph::GraphExecution {
        peer_origin_context: None,
        peer_submission_digest: None,
        requester_path: None,
        extra: Default::default(),
        bases: None,
        directory: factory.cwd.clone(),
        document,
    })?;
    journal.prepare_fixture_checkpoint(&mut checkpoint)?;
    journal.save(&checkpoint)?;
    drop(journal);
    let edit = json!({"op":"edit","id":format!("{id}/task"),"patch":{"schema_version":1,"expected_revision":0,"idempotency_key":"add-one","nodes":[{"id":"second","use":"task","prompt":"Next"}]}});
    let admitted = factory.graph_document(edit.clone(), budget, &cancel)?;
    assert_eq!(admitted["revision"], 1);
    let replayed = factory.graph_document(edit, budget, &cancel)?;
    assert_eq!(replayed["replayed"], true);
    assert!(factory.graph_document(json!({"op":"edit","id":id,"patch":{"schema_version":1,"expected_revision":0,"nodes":[{"id":"stale","use":"task"}]}}),budget,&cancel).is_err());
    checkpoint = FileJournal::read_status_checkpoint(
        &factory.context.roots.state,
        factory.context.identity.project,
        &id,
    )?
    .ok_or("checkpoint missing")?;
    assert_eq!(
        checkpoint
            .nodes
            .iter()
            .filter(|node| node.id == "second")
            .count(),
        1
    );
    let events =
        factory.graph_document(json!({"op":"output","id":id,"trace":true}), budget, &cancel)?;
    assert!(
        events["events"]
            .as_array()
            .ok_or("events missing")?
            .iter()
            .any(|event| event["kind"] == "patch")
    );
    Ok(())
}
#[test]
fn graph_stats_counts_each_attempt_once_without_composite_totals() -> Result {
    let rig = TestRig::new()?;
    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
    let document: GraphDocument = serde_json::from_value(
        factory.graph_document(
            json!({"op":"run","brief":"Initial","explain":true}),
            budget,
            &cancel,
        )?["document"]
            .clone(),
    )?;
    let id = document.graph_id.clone();
    let attempt = json!({"key":{"graph_id":id,"node_id":"task","route_index":0, "attempt": 1},"launched":true,"route":{"alias":"senior"},"state":"terminal","attempt_ref":"host/attempt","launch_actor":"actor","usage":{"tokens":100,"cost_micro_usd":25,"wall_time_seconds":3,"cache_read_tokens":60,"input_tokens":80,"queue_time_ms":1500,"outcome":"timed_out","tier":"principal","template":"implement.fix"}});
    let mut current_attempt = attempt.clone();
    current_attempt["key"]["attempt"] = json!(3);
    let mut checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint = serde_json::from_value(
        json!({"schema_version":2,"graph_id":id,"plan_digest":"test","nodes":[
            {"id":"group","worktree":null,"state":"succeeded","attempts":[],"children":["task"],"usage":{"tokens":100}},
            {"id":"task","worktree":null,"state":"succeeded","attempts":[current_attempt],"previous_attempts":[attempt]}
        ]}),
    )?;
    checkpoint.nodes[1]
        .previous_attempts
        .push(serde_json::from_value(json!({
            "key":{"graph_id":id,"node_id":"task","route_index":1, "attempt": 2},
            "launched":false,"route":{"alias":"senior"},"state":"terminal",
            "attempt_ref":null,"outcome":"adapter_unavailable","launch_actor":"actor","settled":1
        }))?);
    let mut journal = FileJournal::open(
        &factory.context.roots.state,
        factory.context.identity.project,
        &id,
        budget,
        &cancel,
        &factory.clock,
    )?;
    journal.bind(provenant_fabric_runtime::graph::GraphExecution {
        peer_origin_context: None,
        peer_submission_digest: None,
        requester_path: None,
        extra: Default::default(),
        bases: None,
        directory: factory.cwd.clone(),
        document,
    })?;
    let mut reserved = checkpoint.clone();
    reserved.nodes[1].attempts[0].attempt_ref = None;
    reserved.nodes[1].attempts[0].state = provenant_fabric_runtime::graph::AttemptState::Starting;
    reserved.nodes[1].attempts[0].outcome = None;
    reserved.nodes[1].attempts[0].usage = None;
    reserved.nodes[1].previous_attempts.clear();
    journal.prepare_fixture_checkpoint(&mut reserved)?;
    journal.save(&reserved)?;
    journal.prepare_fixture_checkpoint(&mut checkpoint)?;
    journal.save(&checkpoint)?;
    drop(journal);
    let stats = factory.graph_document(
        json!({"op":"status","id":id,"stats":true,"by":"tier"}),
        budget,
        &cancel,
    )?;
    let stats_group = stats["groups"]
        .as_array()
        .ok_or("stats groups")?
        .iter()
        .find(|group| group["name"] == json!("principal"))
        .ok_or("expected stats group")?;
    assert_eq!(stats_group["attempts"], 1);
    assert_eq!(stats_group["tokens"], 100);
    assert_eq!(stats_group["cost_micro_usd"], 25);
    assert_eq!(stats_group["wall_time_seconds"], 3);
    assert_eq!(stats_group["name"], "principal");
    assert_eq!(stats_group["cache_read_share"], 0.75);
    assert_eq!(stats_group["queue_time_ms"], 1500);
    assert_eq!(stats_group["outcomes"]["timed_out"], 1);
    let refused = stats["groups"]
        .as_array()
        .ok_or("stats groups")?
        .iter()
        .find(|group| group["name"] == "senior")
        .ok_or("refused attempt group")?;
    assert_eq!(refused["attempts"], 1);
    assert_eq!(refused["outcomes"]["adapter_unavailable"], 1);

    let nodes = factory.graph_document(
        json!({"op":"status","id":id,"stats":true,"by":"node"}),
        budget,
        &cancel,
    )?;
    let nodes_group = nodes["groups"]
        .as_array()
        .ok_or("stats groups")?
        .iter()
        .find(|group| group["name"] == json!(format!("{id}/task")))
        .ok_or("expected stats group")?;
    assert_eq!(nodes_group["name"], format!("{id}/task"));
    assert_eq!(nodes_group["tokens"], 100);

    let before = FileJournal::read_status_checkpoint(
        &factory.context.roots.state,
        factory.context.identity.project,
        &id,
    )?;
    for extend in [json!(true), json!(["budget.tokens=1000"])] {
        assert!(
            factory
                .graph_document(
                    json!({"op":"retry","id":format!("{id}/task"),"extend":extend}),
                    budget,
                    &cancel
                )
                .is_err()
        );
        assert_eq!(
            FileJournal::read_status_checkpoint(
                &factory.context.roots.state,
                factory.context.identity.project,
                &id
            )?,
            before
        );
    }
    assert_eq!(
        FileJournal::collect_terminal(
            &factory.context.roots.state,
            factory.context.identity.project,
            i128::MAX,
            &provenant_core::SweepControl {
                cancel: cancel.clone(),
                ..Default::default()
            },
            &factory.clock
        )?
        .removed,
        1
    );
    let retained = factory.graph_document(
        json!({"op":"status","id":id,"stats":true,"by":"tier"}),
        budget,
        &cancel,
    )?;
    let retained_group = retained["groups"]
        .as_array()
        .ok_or("stats groups")?
        .iter()
        .find(|group| group["name"] == json!("principal"))
        .ok_or("expected stats group")?;
    assert_eq!(retained_group["attempts"], 1);
    assert_eq!(retained_group["tokens"], 100);
    let refused = retained["groups"]
        .as_array()
        .ok_or("retained groups")?
        .iter()
        .find(|group| group["name"] == "senior")
        .ok_or("retained refusal")?;
    assert_eq!(refused["attempts"], 1);
    assert_eq!(refused["outcomes"]["adapter_unavailable"], 1);

    let all = factory.graph_document(
        json!({"op":"status","stats":true,"by":"template"}),
        budget,
        &cancel,
    )?;
    let all_group = all["groups"]
        .as_array()
        .ok_or("stats groups")?
        .iter()
        .find(|group| group["name"] == json!("implement.fix"))
        .ok_or("expected stats group")?;
    assert_eq!(all_group["attempts"], 1);
    assert_eq!(all_group["name"], "implement.fix");
    assert_eq!(all_group["cache_read_share"], 0.75);
    assert_eq!(all_group["queue_time_ms"], 1500);
    assert_eq!(all_group["outcomes"]["timed_out"], 1);
    Ok(())
}
#[test]
fn review_explain_applies_family_floor_to_launched_legs_and_reports_exclusions() -> Result {
    let rig = crate::worktree_tests::repository()?;
    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    let family = provenant_records::AdapterSpec::get("codex")
        .ok_or("adapter")?
        .family;
    let input = json!({
        "op": "run", "explain": true,
        "document": {
            "use": "review", "prompt": "Review required family coverage",
            "review": {
                "shape": "standard", "minimum_family_counts": {(family): 2},
                "lenses": [
                    {"name": "correctness", "tier": "senior"},
                    {"name": "tests", "tier": "senior"}
                ]
            }
        }
    });
    let response = factory.graph_document(input.clone(), budget, &cancel)?;
    let document: GraphDocument = serde_json::from_value(response["document"].clone())?;
    let record: provenant_routing::PanelResolution =
        serde_json::from_str(&document.template_resolvers["review-family-resolution"][0])?;
    assert_eq!(
        record
            .slots
            .iter()
            .filter(|slot| slot.satisfied_family.as_deref() == Some(family))
            .count(),
        2
    );
    let expanded = document.expanded(&factory.context.config.graph.limits)?;
    let constrained = expanded
        .nodes
        .iter()
        .filter(|node| {
            let routes: Vec<_> = node
                .routes
                .as_ref()
                .map(|routes| routes.iter().collect())
                .unwrap_or_else(|| node.route.iter().collect());
            !routes.is_empty()
                && routes
                    .iter()
                    .all(|route| route.required_family.as_deref() == Some(family))
        })
        .count();
    assert_eq!(
        constrained, 2,
        "the actual review legs must retain their required family"
    );
    let mut child = input.clone();
    child["route"] = json!("missing-family-pool");
    child["document"]["review"]["diversity"] = json!("strict");
    let error = factory.child_request(&child).unwrap_err();
    let FacadeError::DispatchRefused { reason, .. } = error else {
        return Err(format!("expected child tier refusal, received {error:?}").into());
    };
    assert!(
        reason.contains("unknown named route `missing-family-pool`"),
        "{reason}"
    );
    let mut advisory = input.clone();
    advisory["document"]["exclude"] = json!(["codex"]);
    let response = factory.graph_document(advisory, budget, &cancel)?;
    let document: GraphDocument = serde_json::from_value(response["document"].clone())?;
    let record: provenant_routing::PanelResolution =
        serde_json::from_str(&document.template_resolvers["review-family-resolution"][0])?;
    assert!(
        record
            .slots
            .iter()
            .all(|slot| slot.satisfied_family.is_none())
    );
    let [warning] = record.warnings.as_slice() else {
        return Err(format!("expected one warning, received {:?}", record.warnings).into());
    };
    assert!(
        warning.contains(family) && warning.contains("count 2"),
        "{warning}"
    );
    assert!(
        warning.contains("excluded by") && warning.contains("codex"),
        "{warning}"
    );
    let mut refused = input;
    refused["document"]["exclude"] = json!(["codex"]);
    refused["document"]["review"]["diversity"] = json!("strict");
    let error = factory
        .graph_document(refused, budget, &cancel)
        .unwrap_err();
    let FacadeError::DispatchRefused { reason, .. } = error else {
        return Err(format!("expected family refusal, received {error:?}").into());
    };
    assert!(reason.contains(family), "{reason}");
    assert!(reason.contains("count 2"), "{reason}");
    assert!(
        reason.contains("excluded by") && reason.contains("codex"),
        "{reason}"
    );
    Ok(())
}

#[test]
fn review_child_refuses_unknown_tier_under_advisory_default() -> Result {
    let rig = crate::worktree_tests::repository()?;
    let factory = factory(&rig)?;
    let family = provenant_records::AdapterSpec::get("codex")
        .ok_or("adapter")?
        .family;
    // No `diversity` key: the advisory default must refuse the tier too.
    let child = json!({
        "op": "run", "route": "missing-family-pool",
        "document": {
            "use": "review", "prompt": "Review required family coverage",
            "review": {
                "shape": "standard", "minimum_family_counts": {(family): 2},
                "lenses": [{"name": "correctness", "tier": "senior"}]
            }
        }
    });
    let error = factory.child_request(&child).unwrap_err();
    let FacadeError::DispatchRefused { reason, .. } = error else {
        return Err(format!("expected unknown tier refusal, received {error:?}").into());
    };
    assert!(
        reason.contains("unknown named route `missing-family-pool`"),
        "{reason}"
    );
    assert!(reason.contains("valid tiers: "), "{reason}");
    assert!(!reason.contains("not both"), "{reason}");
    Ok(())
}
