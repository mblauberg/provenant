#[test]
fn graph_review_uses_merge_base_of_advanced_named_ref() -> Result<(), Box<dyn std::error::Error>> {
    if super::super::peer_tests::host_test(
        "fabric::owner::journal_error_tests::graph_review_uses_merge_base_of_advanced_named_ref",
    )? {
        return Ok(());
    }
    for scenario in ["node-base", "graph-base", "behaviour", "parent-missing"] {
        review_launch_fixture(scenario)?;
    }
    Ok(())
}

#[test]
fn empty_graph_review_range_does_not_launch_a_lens() -> Result<(), Box<dyn std::error::Error>> {
    if super::super::peer_tests::host_test(
        "fabric::owner::journal_error_tests::empty_graph_review_range_does_not_launch_a_lens",
    )? {
        return Ok(());
    }
    for scenario in ["same-head", "ancestor", "no-base"] {
        review_launch_fixture(scenario)?;
    }
    Ok(())
}

#[test]
fn repaired_subject_review_launches_with_integration_base() -> Result<(), Box<dyn std::error::Error>>
{
    if super::super::peer_tests::host_test(
        "fabric::owner::journal_error_tests::repaired_subject_review_launches_with_integration_base",
    )? {
        return Ok(());
    }
    review_launch_fixture("repair")
}

#[test]
fn repair_loop_composite_review_uses_merge_base_and_refuses_empty_range()
-> Result<(), Box<dyn std::error::Error>> {
    if super::super::peer_tests::host_test(
        "fabric::owner::journal_error_tests::repair_loop_composite_review_uses_merge_base_and_refuses_empty_range",
    )? {
        return Ok(());
    }
    for scenario in ["expanded", "expanded-empty"] {
        review_launch_fixture(scenario)?;
    }
    Ok(())
}

#[allow(dead_code)] // The custom worktree driver includes this module without libtest.
fn review_launch_fixture(scenario: &str) -> Result<(), Box<dyn std::error::Error>> {
    use super::*;
    use provenant_fabric_runtime::graph::{self, GraphJournal};
    struct Free;
    impl WorkerObserver for Free {
        fn observe_roots(
            &self,
            roots: &[PathBuf],
        ) -> std::collections::BTreeMap<PathBuf, provenant_confinement::worktrees::Occupancy>
        {
            roots
                .iter()
                .cloned()
                .map(|root| (root, provenant_confinement::worktrees::Occupancy::Available))
                .collect()
        }
    }
    let rig = crate::worktree_tests::repository()?;
    let fork = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
    crate::worktree_tests::git(&rig, &["checkout", "-b", "review-subject"])?;
    crate::worktree_tests::git(&rig, &["commit", "--allow-empty", "-m", "subject"])?;
    if scenario == "repair" {
        crate::worktree_tests::git(&rig, &["commit", "--allow-empty", "-m", "repair subject"])?;
    }
    let subject = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
    let target_start = if matches!(scenario, "ancestor" | "expanded-empty") {
        &subject
    } else {
        &fork
    };
    crate::worktree_tests::git(&rig, &["checkout", "-b", "review-target", target_start])?;
    crate::worktree_tests::git(&rig, &["commit", "--allow-empty", "-m", "advanced target"])?;
    let tip = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
    let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
    let factory = Factory {
        context: Context::open(roots, &rig.home, &rig.project)?,
        project_attribution: false,
        clock: SystemClock,
        backoff: ThreadPause,
        executor: SystemExecutor::new(1)?,
        home: rig.home.clone(),
        cwd: rig.project.clone(),
        assets: rig.project.clone(),
        host_launch: None,
        workers: &Free,
    };
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    let mut document = serde_json::json!({
        "schema_version":2,"graph_id":format!("review-launch-{scenario}"),
        "nodes":[{"id":"review","role":"lens.correctness-spec","prompt":"Review subject",
            "access":"read_only","route":{"alias":"senior"},"attempt":{"preset":"light"},
            "source":{"type":"commit","commit":subject}}]
    });
    if scenario == "parent-missing" {
        document["subject"] = serde_json::json!({
            "head": subject,
            "origin": {"kind":"local_ref","name":"HEAD"},
            "pinned_at": "2000-01-01T00:00:00.000000Z"
        });
    }
    let graph_base = scenario == "graph-base" || scenario == "behaviour";
    if graph_base {
        document["review_base"] = serde_json::json!("review-target");
    } else if !matches!(scenario, "repair" | "no-base") {
        document["nodes"][0]["review_base"] = serde_json::json!(if scenario == "same-head" {
            subject.as_str()
        } else {
            "review-target"
        });
    }
    if scenario == "behaviour" {
        document["nodes"][0]
            .as_object_mut()
            .ok_or("node")?
            .remove("role");
        document["nodes"][0]["review_behaviour"] = serde_json::json!("lens");
    }
    if scenario.starts_with("expanded") {
        let review = document["nodes"][0].clone();
        document["nodes"] = serde_json::json!([{
            "id":"repair", "kind":"loop", "access":"read_only",
            "loop":{"review_repair":true,"max_rounds":1,"until":"accepted","body":[
                {"id":"panel","kind":"composite","access":"read_only",
                    "composite":{"nodes":[review],
                        "exports":[{"node":"review","outputs":["verdict"]}]}},
                {"id":"accepted","kind":"gate","access":"read_only",
                    "depends_on":["panel"],"inputs":[{"node":"panel","outputs":["verdict"]}],
                    "condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"}}
            ]}
        }]);
    }
    let document: graph::GraphDocument = serde_json::from_value(document)?;
    let integration = (scenario == "repair").then(|| fork.parse()).transpose()?;
    let bound = GraphBaseBinding::new(graph_base.then(|| tip.parse()).transpose()?, integration);
    let mut journal = graph::FileJournal::open(
        &factory.context.roots.state,
        factory.context.identity.project,
        &document.graph_id,
        budget,
        &cancel,
        &factory.clock,
    )?;
    journal.bind(graph::GraphExecution {
        peer_origin_context: None,
        peer_submission_digest: None,
        requester_path: None,
        bases: bound,
        directory: factory.cwd.clone(),
        extra: Default::default(),
        document: document.clone(),
    })?;
    let mut owner = BoundGraphOwner {
        writer_launch_budget: None,
        admission_timeout: None,
        factory: &factory,
        repo_facts: Default::default(),
        launch_warnings: Vec::new(),
        integration_base: bound,
        integration_branch: None,
        network: None,
        capability_overrides: Default::default(),
        exclude: Vec::new(),
        budget,
        cancel: &cancel,
        prices: None,
        tick: false,
    };
    let result = provenant_cli::run::with_deferred_provider_launch(|| {
        graph::advance_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &factory.context.config.graph.limits,
            &factory.context.config.graph.budget,
        )
    });
    let checkpoint = journal.load(&document.graph_id)?.ok_or("checkpoint")?;
    let review = checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "review" || node.id == "repair--r1--panel--review")
        .ok_or("review checkpoint")?;
    if matches!(
        scenario,
        "same-head" | "ancestor" | "no-base" | "expanded-empty"
    ) {
        // Equal bound inputs fail in the scheduler; derived empty ranges are
        // invalid-input refusals from the launch owner.
        if scenario == "same-head" {
            assert_eq!(
                review.advance_code.as_deref(),
                Some("review_empty_range"),
                "{scenario}: {result:?} {checkpoint:?}"
            );
        } else {
            assert_eq!(
                review.advance_code.as_deref(),
                Some("launch_refused"),
                "{scenario}: {result:?} {checkpoint:?}"
            );
            assert_eq!(
                review.advance_owner_code.as_deref(),
                Some("invalid_input"),
                "{scenario}: {result:?} {checkpoint:?}"
            );
        }
        assert!(
            review
                .attempts
                .iter()
                .all(|attempt| attempt.attempt_ref.is_none()),
            "{scenario}: lens launched"
        );
        return Ok(());
    }
    let run = result?;
    let reference = review
        .attempts
        .iter()
        .find_map(|attempt| attempt.attempt_ref.as_deref())
        .ok_or_else(|| format!("no sealed review: {run:?}"))?;
    let (_, id) = reference.split_once('/').ok_or("attempt reference")?;
    let binary = BinaryRunOwner {
        factory: &factory,
        allow_remote: false,
        actor: None,
        chair: None,
    };
    let store = binary.store(&cancel)?;
    let call = binary.call(&cancel);
    let record = store.record(id.parse()?, &call)?;
    let request = store.request(record.attempt_id, &call)?;
    let task = &request.task;
    assert_eq!(task.base_ref.as_deref(), Some(fork.as_str()), "{scenario}");
    assert_eq!(task.head.as_deref(), Some(subject.as_str()), "{scenario}");
    // The checkpoint keeps the bound ref tip, never the derived comparison base.
    if !graph_base && scenario != "repair" {
        assert_eq!(review.review_base.as_deref(), Some(tip.as_str()));
    }
    Ok(())
}

#[test]
fn child_directory_preserves_scratch_defers_credentials_and_refuses_state()
-> Result<(), Box<dyn std::error::Error>> {
    use super::*;
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let state = root.path().join("state");
    let scratch = root.path().join("scratch");
    let credentials = home.join(".codex");
    for path in [&home, &state, &scratch, &credentials] {
        std::fs::create_dir_all(path)?;
    }
    let mut grants = provenant_records::run::LaneAccessRequest::default();
    bind_child_directory(
        &mut grants,
        scratch.to_str().ok_or("scratch")?,
        &state,
        None,
    )?;
    assert_eq!(
        grants.source_root,
        Some(provenant_os::fs::canonicalize(&scratch)?)
    );
    assert!(grants.source_live);
    assert!(
        bind_child_directory(&mut grants, state.to_str().ok_or("root")?, &state, None).is_err()
    );
    bind_child_directory(
        &mut grants,
        credentials.to_str().ok_or("credential root")?,
        &state,
        None,
    )?;
    assert_eq!(
        grants.source_root,
        Some(provenant_os::fs::canonicalize(&credentials)?)
    );
    Ok(())
}

#[test]
fn pinned_reader_uses_project_when_cwd_contains_credentials()
-> Result<(), Box<dyn std::error::Error>> {
    use super::*;
    use provenant_fabric_runtime::graph::{self, AccessMode};
    let rig = crate::worktree_tests::repository()?;
    std::fs::create_dir_all(rig.home.join(".ssh"))?;
    let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
    let factory = Factory {
        context: Context::open(roots, &rig.home, &rig.project)?,
        project_attribution: false,
        clock: SystemClock,
        backoff: ThreadPause,
        executor: SystemExecutor::new(1)?,
        home: rig.home.clone(),
        cwd: rig.home.clone(),
        assets: rig.project.clone(),
        host_launch: None,
        workers: &provenant_confinement::worktrees::SystemWorkerObserver,
    };
    let cancel = CancelToken::new();
    let owner = BoundGraphOwner {
        writer_launch_budget: None,
        admission_timeout: None,
        factory: &factory,
        repo_facts: Default::default(),
        launch_warnings: Vec::new(),
        integration_base: None,
        integration_branch: None,
        network: None,
        capability_overrides: Default::default(),
        exclude: Vec::new(),
        budget: WaitBudget::cli(factory.clock.mono_now(), 30)?,
        cancel: &cancel,
        prices: None,
        tick: false,
    };
    let key = graph::AttemptKey {
        extra: Default::default(),
        retry_generation: None,
        previous_attempt_ref: None,
        graph_id: "reader-project".into(),
        node_id: "read".into(),
        route_index: 0,
        attempt: 1,
    };
    let node = graph::NodeInput {
        writer_dirty_preflight: false,
        review_launch: false,
        require_skill_location: false,
        settlement_retries: None,
        prior_attempts: &[],
        family_peers: &[],
        context: Default::default(),
        read_roots: &[],
        attachments: &[],
        worktree: None,
        fixture: None,
        fixture_bytes: None,
        capabilities: None,
        capability_sources: None,
        attempt: None,
        distinct_from_attempts: &[],
        require_distinct: false,
        source_commit: None,
        review_base: None,
        title: "reader",
        prompt: "read",
        required_verdict: None,
        deadline_seconds: None,
        inputs_dir: None,
    };
    let grants = owner.launch_grants(
        &node,
        &key,
        AccessMode::ReadOnly,
        None,
        false,
        None,
        Some("HEAD"),
    )?;
    assert_eq!(
        grants.source_root,
        Some(provenant_os::fs::canonicalize(&rig.project)?)
    );
    Ok(())
}

#[test]
fn provisioned_writer_fast_forward_preserves_launch_budget()
-> Result<(), Box<dyn std::error::Error>> {
    use super::*;
    if super::super::peer_tests::host_test(
        "fabric::owner::journal_error_tests::provisioned_writer_fast_forward_preserves_launch_budget",
    )? {
        return Ok(());
    }
    use provenant_fabric_runtime::graph::{AccessMode, AttemptOwner};
    let rig = crate::worktree_tests::repository()?;
    crate::worktree_tests::git(&rig, &["commit", "--allow-empty", "-m", "base"])?;
    let source = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
    let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
    let mut factory = Factory {
        context: Context::open(roots, &rig.home, &rig.project)?,
        project_attribution: false,
        clock: SystemClock,
        backoff: ThreadPause,
        executor: SystemExecutor::new(1)?,
        home: rig.home.clone(),
        cwd: rig.project.clone(),
        assets: rig.project.clone(),
        host_launch: None,
        workers: &provenant_confinement::worktrees::SystemWorkerObserver,
    };
    factory.context.config.graph.writer_provision_seconds = 12;
    let cancel = CancelToken::new();
    let settlement = WaitBudget::cli(factory.clock.mono_now(), 1)?;
    let mut owner = BoundGraphOwner {
        writer_launch_budget: None,
        admission_timeout: Some(settlement),
        factory: &factory,
        repo_facts: Default::default(),
        launch_warnings: Vec::new(),
        integration_base: None,
        integration_branch: None,
        network: None,
        capability_overrides: Default::default(),
        exclude: Vec::new(),
        budget: settlement,
        cancel: &cancel,
        prices: None,
        tick: false,
    };
    let deadline = owner
        .begin_launch(AccessMode::WorktreeWrite, None)?
        .ok_or("deadline")?;
    // Writer admission must derive its wait from writer_provision_seconds,
    // not from the reader/settlement allowance passed in above.
    let remaining = deadline
        .as_micros()
        .saturating_sub(factory.clock.wall_now()?.as_micros());
    assert!(
        remaining > 10_000_000,
        "writer launch wait must outlast the 1 s reader budget: {remaining}us"
    );
    owner.provision_writer_worktree(".worktrees/later", "later", source.trim(), None)?;
    let budget = owner.writer_launch_budget.ok_or("budget")?;
    owner.fast_forward_writer(
        &rig.project,
        &rig.project.join(".worktrees/later"),
        source.trim().parse()?,
        false,
    )?;
    assert_eq!(
        owner.writer_launch_budget.ok_or("budget")?.deadline(),
        budget.deadline()
    );
    assert_eq!(
        owner.begin_launch(AccessMode::WorktreeWrite, Some(deadline))?,
        Some(deadline)
    );
    assert_eq!(
        crate::worktree_tests::git(&rig, &["-C", ".worktrees/later", "rev-parse", "HEAD"])?.trim(),
        source.trim()
    );
    Ok(())
}

#[test]
fn graph_refusal_replays_before_preparation_and_rejects_changed_intent()
-> Result<(), Box<dyn std::error::Error>> {
    use super::*;
    if super::super::peer_tests::host_test(
        "fabric::owner::journal_error_tests::graph_refusal_replays_before_preparation_and_rejects_changed_intent",
    )? {
        return Ok(());
    }
    use provenant_fabric_runtime::graph::{self, AttemptOwner};
    let rig = crate::worktree_tests::repository()?;
    let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
    let factory = Factory {
        context: Context::open(roots, &rig.home, &rig.project)?,
        project_attribution: false,
        clock: SystemClock,
        backoff: ThreadPause,
        executor: SystemExecutor::new(1)?,
        home: rig.home.clone(),
        cwd: rig.project.clone(),
        assets: rig.project.clone(),
        host_launch: None,
        workers: &provenant_confinement::worktrees::SystemWorkerObserver,
    };
    let cancel = CancelToken::new();
    let mut owner = BoundGraphOwner {
        writer_launch_budget: None,
        admission_timeout: None,
        factory: &factory,
        repo_facts: Default::default(),
        launch_warnings: Vec::new(),
        integration_base: None,
        integration_branch: None,
        network: None,
        capability_overrides: Default::default(),
        exclude: Vec::new(),
        budget: WaitBudget::cli(factory.clock.mono_now(), 30)?,
        cancel: &cancel,
        prices: None,
        tick: false,
    };
    let key = graph::AttemptKey {
        extra: Default::default(),
        retry_generation: None,
        previous_attempt_ref: None,
        graph_id: "preflight-replay".into(),
        node_id: "writer".into(),
        route_index: 0,
        attempt: 1,
    };
    let node = |prompt| graph::NodeInput {
        writer_dirty_preflight: false,
        review_launch: false,
        require_skill_location: false,
        settlement_retries: Some(1),
        prior_attempts: &[],
        family_peers: &[],
        context: provenant_records::run::WorkContext {
            graph_id: Some(key.graph_id.clone()),
            node_id: Some(key.node_id.clone()),
            attempt: Some(1),
            ..Default::default()
        },
        read_roots: &[],
        attachments: &[],
        worktree: None,
        fixture: None,
        fixture_bytes: None,
        capabilities: None,
        capability_sources: None,
        attempt: None,
        distinct_from_attempts: &[],
        require_distinct: false,
        source_commit: None,
        review_base: None,
        title: "writer",
        prompt,
        required_verdict: None,
        deadline_seconds: None,
        inputs_dir: None,
    };
    let route: graph::GraphRoute = serde_json::from_value(serde_json::json!({"alias":"senior"}))?;
    let actor = owner.actor();
    let replay = graph::Replay {
        delivery: None,
        branch: None,
        fast_forward_only: false,
        stored_ref: None,
        launch_actor: &actor,
    };
    let access = graph::AccessMode::WorktreeWrite;
    let scope = GraphRefusalScope::new(
        &owner,
        &key,
        &node("PROMPT_SENTINEL"),
        &route,
        access,
        Some("/HOST_PATH_SENTINEL/missing"),
        replay,
    )?;
    let original = FacadeError::DispatchRefused {
        fact: None,
        disk: None,
        reason: "/HOST_PATH_SENTINEL uncommitted edits".into(),
    };
    owner.retain_graph_refusal(
        &scope,
        &original,
        provenant_records::diagnostic::Diagnostic::new("WRITER_SCOPE")
            .detail("the writer worktree has uncommitted edits")
            .next("preserve the edits"),
    )?;
    let store = owner.refusal_store(&scope)?;
    let call = provenant_run::Request {
        clock: &factory.clock,
        cancel: &cancel,
    };
    let first = store.operation(&scope.operation, &call)?.ok_or("refusal")?;
    let serialised = serde_json::to_string(&first)?;
    assert!(!serialised.contains("HOST_PATH_SENTINEL"));
    assert!(!serialised.contains("PROMPT_SENTINEL"));
    assert!(serialised.contains("the writer worktree has uncommitted edits"));
    // This worktree cannot be prepared. The stored dispatch refusal must
    // win without consulting the filesystem or looking for an admitted run.
    let repeated = owner
        .observe_or_start(
            &key,
            node("PROMPT_SENTINEL"),
            &route,
            access,
            Some("/HOST_PATH_SENTINEL/missing"),
            replay,
        )
        .unwrap_err();
    assert_eq!(repeated.code(), original.code());
    assert!(
        repeated
            .to_string()
            .contains("the writer worktree has uncommitted edits")
    );
    assert_eq!(
        store.operation(&scope.operation, &call)?.ok_or("refusal")?,
        first
    );
    let changed = owner
        .observe_or_start(
            &key,
            node("changed"),
            &route,
            access,
            Some("/HOST_PATH_SENTINEL/missing"),
            replay,
        )
        .unwrap_err();
    assert_eq!(changed.code(), "operation_conflict");
    let missing_key = graph::AttemptKey {
        route_index: 2,
        attempt: 3,
        ..key.clone()
    };
    let mut missing = first.clone();
    missing.operation_id = graph_operation("graph-attempt-operation", &missing_key)?;
    missing.origin_intent_digest = None;
    store.insert_refusal(missing, &call)?;
    let missing = owner
        .observe_or_start(
            &missing_key,
            node("PROMPT_SENTINEL"),
            &route,
            access,
            Some("/HOST_PATH_SENTINEL/missing"),
            replay,
        )
        .unwrap_err();
    assert_eq!(missing.code(), "state_corrupt");
    // Exercise RunService's saved preflight result too. Cursor accepts
    // model and effort at this boundary, so an unknown Cursor model reaches
    // route resolution instead. The command adapter declares no model
    // support, which must be refused before a provider launch.
    let service_key = graph::AttemptKey {
        route_index: 1,
        attempt: 2,
        ..key.clone()
    };
    let unsupported: graph::GraphRoute = serde_json::from_value(serde_json::json!({
        "adapter":"command", "model":"command/m", "effort":"high"
    }))?;
    // A distinct attempt of the node takes the next work-key number.
    let service_node = |prompt| {
        let mut input = node(prompt);
        input.context.attempt = Some(2);
        input
    };
    let service_scope = GraphRefusalScope::new(
        &owner,
        &service_key,
        &service_node("service preflight"),
        &unsupported,
        graph::AccessMode::ReadOnly,
        None,
        replay,
    )?;
    let service_refusal = owner
        .observe_or_start(
            &service_key,
            service_node("service preflight"),
            &unsupported,
            graph::AccessMode::ReadOnly,
            None,
            replay,
        )
        .unwrap_err();
    assert!(
        matches!(&service_refusal, FacadeError::RunKind { .. }),
        "{service_refusal:?}"
    );
    let operation = graph_operation("graph-attempt-operation", &service_key)?;
    let saved = store
        .operation(&operation, &call)?
        .ok_or("service refusal")?;
    assert_eq!(saved.state, provenant_run::OperationState::Refused);
    assert_eq!(saved.preflight_digest_version, Some(1));
    assert_eq!(saved.origin_intent_digest, Some(service_scope.digest));
    let repeated = owner
        .observe_or_start(
            &service_key,
            service_node("service preflight"),
            &unsupported,
            graph::AccessMode::ReadOnly,
            None,
            replay,
        )
        .unwrap_err();
    assert_eq!(repeated.code(), service_refusal.code());
    assert_eq!(repeated.to_string(), service_refusal.to_string());
    let changed = owner
        .observe_or_start(
            &service_key,
            service_node("changed service input"),
            &unsupported,
            graph::AccessMode::ReadOnly,
            None,
            replay,
        )
        .unwrap_err();
    assert_eq!(changed.code(), "operation_conflict");
    let attempt: graph::GraphAttempt = serde_json::from_value(serde_json::json!({
        "launched":false,"key":key,"route":route,"state":"starting","attempt_ref":null,
        "outcome":null,"launch_actor":actor,"settled":0
    }))?;
    assert!(!owner.launch_accepted(&attempt)?);
    assert!(owner.launch_refused_before_acceptance(&attempt)?);
    assert!(owner.cancelled_attempt_settled(&attempt)?);
    assert!(!store.has_runs(&call)?);
    Ok(())
}

#[test]
fn repair_scope_comparison_detects_new_paths() {
    let review = provenant_routing::ReviewConfig::default();
    let original = vec!["src/a.rs".to_owned()];
    assert!(!super::repair_scope_grew(&review, &original, &original));
    assert!(super::repair_scope_grew(
        &review,
        &original,
        &["src/a.rs".into(), "src/b.rs".into()]
    ));
    assert!(super::repair_scope_grew(
        &review,
        &original,
        &["src/b.rs".into()]
    ));
    assert!(!super::repair_scope_grew(&review, &original, &[]));
}

#[test]
fn repair_detects_risk_inside_already_reviewed_paths() {
    let review = provenant_routing::ReviewConfig::default();
    let paths = vec!["src/credentials.rs".to_owned()];
    assert!(!super::repair_scope_grew(&review, &paths, &paths));
    assert!(super::repair_touches_risk(&review, &paths));
    assert!(!super::repair_touches_risk(&review, &[]));
    assert!(!super::repair_touches_risk(&review, &["README.md".into()]));
}

#[test]
fn graph_journal_future_context_requires_upgrade() {
    let source = provenant_records::DecodeError::UnsupportedVersion {
        found: 2,
        line: 1,
        column: 1,
    }
    .with_context("/fixture/checkpoint.json", "GraphRecord");
    let error = provenant_fabric_runtime::graph::JournalError::Decode(source);
    assert!(matches!(
        super::graph_journal_error(error),
        super::FacadeError::UpgradeRequired { version: 2, .. }
    ));
}

#[test]
fn unreadable_graph_journal_preserves_the_cause() {
    use super::{FacadeError, graph_journal_error};
    use provenant_fabric_runtime::graph::JournalError;
    for error in [
        JournalError::BadEnvelope,
        JournalError::BadDigest,
        JournalError::InvalidTopology,
    ] {
        let reason = error.to_string();
        assert_eq!(
            graph_journal_error(error),
            FacadeError::DispatchRefused {
                fact: None,
                disk: None,
                reason
            },
        );
    }
    assert_eq!(
        graph_journal_error(JournalError::GraphMismatch),
        FacadeError::StateCorrupt
    );
    assert_eq!(
        graph_journal_error(JournalError::NameReserved),
        FacadeError::Conflict
    );
}
