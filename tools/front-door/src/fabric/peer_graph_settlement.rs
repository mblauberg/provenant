#[allow(unused_imports)]
use super::*;

#[test]
fn graph_review_launch_submits_pinned_source_and_base() -> Result<(), Box<dyn std::error::Error>> {
    if host_test(
        "fabric::peer_tests::peer_graph_settlement::graph_review_launch_submits_pinned_source_and_base",
    )? {
        return Ok(());
    }
    use provenant_fabric_runtime::graph::{self, AttemptOwner};
    let rig = crate::worktree_tests::repository()?;
    let git = |args: &[&str]| crate::worktree_tests::git(&rig, args);
    assert!(
        git(&[
            "for-each-ref",
            "--format=%(refname)",
            "refs/remotes/origin/HEAD"
        ])?
        .is_empty()
    );
    let base = git(&["rev-parse", "HEAD"])?;
    std::fs::write(rig.project.join("lane-change"), "change")?;
    git(&["add", "lane-change"])?;
    git(&["commit", "-qm", "lane"])?;
    let head = git(&["rev-parse", "HEAD"])?;
    git(&["checkout", "-q", "--detach", &base])?;
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
        workers: &FreeWorkers,
    };
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    let mut graph_owner = BoundGraphOwner {
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
    let actor = graph_owner.actor();
    let route =
        serde_json::from_value(serde_json::json!({"adapter":"fixture", "model":"fixture/model"}))?;
    let owner = BinaryRunOwner {
        factory: &factory,
        allow_remote: true,
        actor: None,
        chair: None,
    };
    let store = owner.store(&cancel)?;
    let call = owner.call(&cancel);
    for (name, review_base, expected_base) in [
        ("lens", Some(base.as_str()), &base),
        ("evaluate", Some(base.as_str()), &base),
        ("equal", Some(head.as_str()), &head),
        ("default-no-origin-head", None, &head),
    ] {
        let key: graph::AttemptKey = serde_json::from_value(
            serde_json::json!({"graph_id":"review-pins", "node_id":name, "route_index":0, "attempt": 1}),
        )?;
        let observation = graph_owner.observe_or_start(
            &key,
            graph::NodeInput {
                review_launch: false,
                attachments: &[],
                capabilities: None,
                capability_sources: None,
                attempt: None,
                require_skill_location: false,
                settlement_retries: Some(1),
                prior_attempts: &[],
                family_peers: &[],
                context: graph_test_context(&key),
                read_roots: &[],
                distinct_from_attempts: &[],
                require_distinct: false,
                source_commit: Some(&head),
                review_base,
                title: name,
                prompt: "review",
                required_verdict: None,
                deadline_seconds: None,
                inputs_dir: None,
            },
            &route,
            graph::AccessMode::ReadOnly,
            None,
            graph::Replay {
                delivery: None,
                branch: None,
                fast_forward_only: false,
                stored_ref: None,
                launch_actor: &actor,
            },
        )?;
        let operation = store
            .operation(&graph_operation("graph-attempt-operation", &key)?, &call)?
            .ok_or("missing operation")?;
        let request = store.request(
            store.operation_attempt(&operation.operation_id, &call)?,
            &call,
        )?;
        assert_eq!(request.task.profile.settlement_retry_limit, Some(1));
        assert_eq!(request.task.base.as_deref(), Some(expected_base.as_str()));
        assert_eq!(request.task.head.as_deref(), Some(head.as_str()));
        let reference = match observation {
            graph::AttemptObservation::Pending { attempt_ref }
            | graph::AttemptObservation::Unknown { attempt_ref }
            | graph::AttemptObservation::Terminal { attempt_ref, .. } => {
                attempt_ref.ok_or("missing attempt reference")?
            }
        };
        let range = graph_owner.review_range(&reference, review_base.is_some())?;
        if review_base.is_none() {
            assert_eq!(range, graph::ReviewRange::NoBase);
        } else {
            assert_eq!(
                range,
                graph::ReviewRange::Resolved {
                    base: expected_base.clone(),
                    head: head.clone()
                }
            );
        }
    }
    Ok(())
}

#[test]
fn child_launch_refuses_credential_and_state_roots_at_the_host()
-> Result<(), Box<dyn std::error::Error>> {
    if host_test(
        "fabric::peer_tests::peer_graph_settlement::child_launch_refuses_credential_and_state_roots_at_the_host",
    )? {
        return Ok(());
    }
    use provenant_fabric_runtime::graph::{self, AttemptOwner};
    let rig = crate::worktree_tests::repository()?;
    let credential = rig.home.join(".ssh");
    std::fs::create_dir_all(&credential)?;
    std::fs::create_dir_all(&rig.state)?;
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
        workers: &FreeWorkers,
    };
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
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
    let actor = owner.actor();
    let route =
        serde_json::from_value(serde_json::json!({"adapter":"fixture", "model":"fixture/model"}))?;
    let run_owner = BinaryRunOwner {
        factory: &factory,
        allow_remote: false,
        actor: None,
        chair: None,
    };
    let call = run_owner.call(&cancel);
    let store = run_owner.store(&cancel)?;
    for (name, root) in [("credential", credential), ("state", rig.state.clone())] {
        // The lane forwards the request; only the host decides access at launch.
        let child = factory.child_request(&serde_json::json!({
            "op":"run", "document":{"use":"task", "brief":"Read requested root", "read_roots":[root]}
        }))?;
        assert_eq!(
            child.launch.ok_or("missing child launch")?.node["read_roots"],
            serde_json::json!([root])
        );
        for write in [false, true] {
            let key: graph::AttemptKey = serde_json::from_value(serde_json::json!({
                "graph_id":"child-roots", "node_id":format!("parent--{name}-{write}"), "route_index":0, "attempt": 1
            }))?;
            let roots = vec![root.to_string_lossy().into_owned()];
            let attempt = graph::AttemptSpec {
                write_roots: if write { roots.clone() } else { Vec::new() },
                ..Default::default()
            };
            let result = owner.observe_or_start(
                &key,
                graph::NodeInput {
                    review_launch: false,
                    attachments: &[],
                    capabilities: None,
                    capability_sources: None,
                    attempt: Some(&attempt),
                    require_skill_location: false,
                    settlement_retries: None,
                    prior_attempts: &[],
                    family_peers: &[],
                    context: graph_test_context(&key),
                    read_roots: if write { &[] } else { &roots },
                    distinct_from_attempts: &[],
                    require_distinct: false,
                    source_commit: None,
                    review_base: None,
                    title: "child",
                    prompt: "Read requested root",
                    required_verdict: None,
                    deadline_seconds: None,
                    inputs_dir: None,
                },
                &route,
                graph::AccessMode::ReadOnly,
                None,
                graph::Replay {
                    delivery: None,
                    branch: None,
                    fast_forward_only: false,
                    stored_ref: None,
                    launch_actor: &actor,
                },
            );
            assert!(
                matches!(result, Err(FacadeError::DispatchRefused { .. })),
                "{name}, write={write}: {result:?}"
            );
            let refusal = store
                .operation(&graph_operation("graph-attempt-operation", &key)?, &call)?
                .ok_or("refusal operation missing")?;
            assert_eq!(refusal.state, provenant_run::OperationState::Refused);
        }
    }
    Ok(())
}

#[test]
fn pinned_review_range_survives_moved_ref_and_another_checkout()
-> Result<(), Box<dyn std::error::Error>> {
    if host_test(
        "fabric::peer_tests::peer_graph_settlement::pinned_review_range_survives_moved_ref_and_another_checkout",
    )? {
        return Ok(());
    }
    use provenant_fabric_runtime::graph::{self, AttemptOwner};
    let rig = crate::worktree_tests::repository()?;
    let git = |args: &[&str]| crate::worktree_tests::git(&rig, args);
    let base = git(&["rev-parse", "HEAD"])?;
    git(&["branch", "review-pin", &base])?;
    std::fs::write(rig.project.join("source-change"), "source")?;
    git(&["add", "source-change"])?;
    git(&["commit", "-qm", "original source"])?;
    let original = git(&["rev-parse", "HEAD"])?;
    std::fs::write(rig.project.join("predecessor-change"), "predecessor")?;
    git(&["add", "predecessor-change"])?;
    git(&["commit", "-qm", "predecessor"])?;
    let head = git(&["rev-parse", "HEAD"])?;
    assert_ne!(base, original);
    assert_ne!(original, head);
    let other = rig.root.join("other-checkout");
    git(&[
        "worktree",
        "add",
        "--detach",
        other.to_str().ok_or("other path")?,
        &base,
    ])?;
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
        workers: &FreeWorkers,
    };
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    let mut owner = BoundGraphOwner {
        writer_launch_budget: None,
        admission_timeout: None,
        launch_warnings: Vec::new(),
        repo_facts: Default::default(),
        factory: &factory,
        integration_base: None,
        capability_overrides: (Vec::new(), Vec::new()),
        integration_branch: None,
        network: None,
        exclude: Vec::new(),
        budget,
        cancel: &cancel,
        prices: None,
        tick: false,
    };
    let pinned = owner
        .resolve_review_base("review-pin", Some(rig.project.to_str().ok_or("project")?))?
        .ok_or("base")?;
    assert_eq!(pinned, base);
    let actor = owner.actor();
    let key: graph::AttemptKey = serde_json::from_value(
        serde_json::json!({"graph_id":"pinned-range","node_id":"review","route_index":0, "attempt": 1}),
    )?;
    let route =
        serde_json::from_value(serde_json::json!({"adapter":"fixture","model":"fixture/model"}))?;
    let source_roots = vec![rig.project.to_str().ok_or("source path")?.to_owned()];
    let write_root = rig.root.join("extra-write");
    std::fs::create_dir(&write_root)?;
    let attempt_spec = graph::AttemptSpec {
        build_heavy: true,
        write_roots: vec![write_root.to_string_lossy().into_owned()],
        ..Default::default()
    };
    let node = || graph::NodeInput {
        review_launch: false,
        attachments: &[],
        capabilities: None,
        capability_sources: None,
        attempt: Some(&attempt_spec),
        require_skill_location: false,
        settlement_retries: None,
        prior_attempts: &[],
        family_peers: &[],
        context: graph_test_context(&key),
        read_roots: &source_roots,
        distinct_from_attempts: &[],
        require_distinct: false,
        source_commit: Some(&head),
        review_base: Some(&pinned),
        title: "review",
        prompt: "review",
        required_verdict: None,
        deadline_seconds: None,
        inputs_dir: None,
    };
    let first = owner
        .observe_or_start(
            &key,
            node(),
            &route,
            graph::AccessMode::ReadOnly,
            Some(rig.project.to_str().ok_or("source path")?),
            graph::Replay {
                delivery: None,
                branch: None,
                fast_forward_only: false,
                stored_ref: None,
                launch_actor: &actor,
            },
        )
        .map_err(|error| format!("first launch: {error:?}"))?;
    let reference = match first {
        graph::AttemptObservation::Pending { attempt_ref }
        | graph::AttemptObservation::Unknown { attempt_ref }
        | graph::AttemptObservation::Terminal { attempt_ref, .. } => {
            attempt_ref.ok_or("attempt reference")?
        }
    };
    git(&["branch", "-f", "review-pin", &head])?;
    git(&["commit", "--allow-empty", "-qm", "move source checkout"])?;
    assert_ne!(git(&["rev-parse", "HEAD"])?, head);
    let reattached = owner
        .observe_or_start(
            &key,
            node(),
            &route,
            graph::AccessMode::ReadOnly,
            Some(rig.project.to_str().ok_or("source path")?),
            graph::Replay {
                delivery: None,
                branch: None,
                fast_forward_only: false,
                stored_ref: None,
                launch_actor: &actor,
            },
        )
        .map_err(|error| format!("operation replay: {error:?}"))?;
    let replayed = match reattached {
        graph::AttemptObservation::Pending { attempt_ref }
        | graph::AttemptObservation::Unknown { attempt_ref }
        | graph::AttemptObservation::Terminal { attempt_ref, .. } => {
            attempt_ref.ok_or("replay reference")?
        }
    };
    assert_eq!(replayed, reference);
    let other_factory = factory.for_graph_directory(&other)?;
    let mut replay_owner = BoundGraphOwner {
        writer_launch_budget: None,
        admission_timeout: None,
        launch_warnings: Vec::new(),
        repo_facts: Default::default(),
        factory: &other_factory,
        integration_base: None,
        capability_overrides: (Vec::new(), Vec::new()),
        integration_branch: None,
        network: None,
        exclude: Vec::new(),
        budget,
        cancel: &cancel,
        prices: None,
        tick: false,
    };
    replay_owner.observe_or_start(
        &key,
        node(),
        &route,
        graph::AccessMode::ReadOnly,
        Some(rig.project.to_str().ok_or("source path")?),
        graph::Replay {
            delivery: None,
            branch: None,
            fast_forward_only: false,
            stored_ref: Some(&reference),
            launch_actor: &actor,
        },
    )?;
    let run_owner = BinaryRunOwner {
        factory: &other_factory,
        allow_remote: true,
        actor: None,
        chair: None,
    };
    let call = run_owner.call(&cancel);
    let store = run_owner.store(&cancel)?;
    let operation = store
        .operation(&graph_operation("graph-attempt-operation", &key)?, &call)?
        .ok_or("operation")?;
    let request = store.request(
        store.operation_attempt(&operation.operation_id, &call)?,
        &call,
    )?;
    assert_eq!(request.task.base.as_deref(), Some(base.as_str()));
    assert_eq!(request.task.head.as_deref(), Some(head.as_str()));
    assert_eq!(
        request.task.access.source_root.as_deref(),
        Some(rig.project.as_path())
    );
    assert!(request.task.access.read_roots.is_empty());
    assert!(request.task.build_heavy);
    assert_eq!(request.task.access.write_roots, vec![write_root]);
    let scratch = rig.root.join("review-evidence");
    std::fs::create_dir(&scratch)?;
    let directory = provenant_os::fs::Directory::open_anchor(&scratch)?;
    let git_path = provenant_os::git::git_executable().ok_or("git")?;
    provenant_run::GitContext::capture_at(&git_path, &other, &base, &head)?
        .write_to(&directory, &scratch)?;
    let stat = std::fs::read_to_string(scratch.join("git-diff.stat"))?;
    assert!(stat.contains("source-change"), "{stat}");
    assert!(stat.contains("predecessor-change"), "{stat}");
    Ok(())
}

#[test]
fn implicit_writer_binding_uses_verified_worktree_head() -> Result<(), Box<dyn std::error::Error>> {
    if host_test(
        "fabric::peer_tests::peer_graph_settlement::implicit_writer_binding_uses_verified_worktree_head",
    )? {
        return Ok(());
    }
    use provenant_fabric_runtime::graph::AttemptOwner;
    let rig = crate::worktree_tests::repository()?;
    let git = |args: &[&str]| crate::worktree_tests::git(&rig, args);
    let initial = git(&["rev-parse", "HEAD"])?;
    let lane = rig.root.join("lane");
    git(&[
        "worktree",
        "add",
        "-qb",
        "lane",
        lane.to_str().ok_or("lane path")?,
    ])?;
    git(&["commit", "--allow-empty", "-qm", "move invoking checkout"])?;
    assert_ne!(git(&["rev-parse", "HEAD"])?, initial);
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
        workers: &FreeWorkers,
    };
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    let mut owner = BoundGraphOwner {
        writer_launch_budget: None,
        admission_timeout: None,
        launch_warnings: Vec::new(),
        repo_facts: Default::default(),
        factory: &factory,
        integration_base: None,
        capability_overrides: (Vec::new(), Vec::new()),
        integration_branch: None,
        network: None,
        exclude: Vec::new(),
        budget,
        cancel: &cancel,
        prices: None,
        tick: false,
    };
    let binding = owner
        .bind_writer_worktree(lane.to_str().ok_or("lane path")?)?
        .ok_or("binding")?;
    assert_eq!(
        binding.worktree,
        std::fs::canonicalize(&lane)?.to_string_lossy()
    );
    assert_eq!(binding.head, initial);
    let metadata = provenant_os::fs::Directory::open_anchor(&lane)?.metadata()?;
    assert_eq!(
        binding.extra["source_directory_identity"]["device"],
        metadata.device
    );
    assert_eq!(
        binding.extra["source_directory_identity"]["inode"],
        metadata.inode
    );
    git(&[
        "-C",
        lane.to_str().ok_or("lane path")?,
        "checkout",
        "--detach",
        "-q",
    ])?;
    assert!(
        owner
            .bind_writer_worktree(lane.to_str().ok_or("lane path")?)
            .is_err()
    );
    Ok(())
}

#[test]
fn integrating_graph_writer_refuses_a_source_outside_the_pinned_base()
-> Result<(), Box<dyn std::error::Error>> {
    if host_test(
        "fabric::peer_tests::peer_graph_settlement::integrating_graph_writer_refuses_a_source_outside_the_pinned_base",
    )? {
        return Ok(());
    }
    use provenant_fabric_runtime::graph::{self, AttemptOwner};
    let rig = crate::worktree_tests::repository()?;
    let git = |args: &[&str]| crate::worktree_tests::git(&rig, args);
    let base = git(&["rev-parse", "HEAD"])?;
    let tree = git(&["rev-parse", "HEAD^{tree}"])?;
    let unrelated = git(&["commit-tree", &tree, "-m", "unrelated"])?;
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
        workers: &FreeWorkers,
    };
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    let mut owner = BoundGraphOwner {
        writer_launch_budget: None,
        admission_timeout: None,
        launch_warnings: Vec::new(),
        repo_facts: Default::default(),
        factory: &factory,
        capability_overrides: (Vec::new(), Vec::new()),
        integration_base: Some(BoundGraphBases {
            integration: Some(base.parse()?),
            review: None,
        }),
        integration_branch: Some("batch".into()),
        network: None,
        exclude: Vec::new(),
        budget,
        cancel: &cancel,
        prices: None,
        tick: false,
    };
    let actor = owner.actor();
    let key: graph::AttemptKey = serde_json::from_value(
        serde_json::json!({"graph_id":"batch", "node_id":"lane-outside", "route_index":0, "attempt": 1}),
    )?;
    let route =
        serde_json::from_value(serde_json::json!({"adapter":"fixture", "model":"fixture/model"}))?;
    let delivery = provenant_records::run::DeliveryMode::Integrate("batch".into());
    let error = owner
        .observe_or_start(
            &key,
            graph::NodeInput {
                review_launch: false,
                attachments: &[],
                capabilities: None,
                capability_sources: None,
                attempt: None,
                require_skill_location: false,
                settlement_retries: None,
                prior_attempts: &[],
                family_peers: &[],
                context: graph_test_context(&key),
                read_roots: &[],
                distinct_from_attempts: &[],
                require_distinct: false,
                source_commit: Some(&unrelated),
                review_base: None,
                title: "lane",
                prompt: "write",
                required_verdict: None,
                deadline_seconds: None,
                inputs_dir: None,
            },
            &route,
            graph::AccessMode::WorktreeWrite,
            None,
            graph::Replay {
                delivery: Some(&delivery),
                branch: None,
                fast_forward_only: false,
                stored_ref: None,
                launch_actor: &actor,
            },
        )
        .err()
        .ok_or("unrelated lane was accepted")?;
    assert!(error.to_string().contains("lane-outside"));
    assert!(error.to_string().contains(&base));
    let binary = BinaryRunOwner {
        factory: &factory,
        allow_remote: true,
        actor: None,
        chair: None,
    };
    let refusal = binary
        .store(&cancel)?
        .operation(
            &graph_operation("graph-attempt-operation", &key)?,
            &binary.call(&cancel),
        )?
        .ok_or("refusal operation missing")?;
    assert_eq!(refusal.state, provenant_run::OperationState::Refused);
    Ok(())
}

#[test]
fn run_list_is_compact_and_pages_newest_first() -> Result<(), Box<dyn std::error::Error>> {
    if host_test(
        "fabric::peer_tests::peer_graph_settlement::run_list_is_compact_and_pages_newest_first",
    )? {
        return Ok(());
    }
    let fixture = SettlementFixture::resumed()?;
    let factory = Factory {
        context: Context::open(
            fixture.roots.clone(),
            &fixture.rig.home,
            &fixture.rig.project,
        )?,
        project_attribution: false,
        clock: SystemClock,
        backoff: ThreadPause,
        executor: SystemExecutor::new(1)?,
        home: fixture.rig.home.clone(),
        cwd: fixture.rig.project.clone(),
        assets: fixture.rig.project.clone(),
        host_launch: None,
        workers: &FreeWorkers,
    };
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    let mut owner = BinaryRunOwner {
        factory: &factory,
        allow_remote: false,
        actor: None,
        chair: None,
    };
    let first = owner.run(
        RunInput::List {
            extra: Default::default(),
            after: None,
            limit: 1,
        },
        budget,
        &cancel,
    )?;
    let wire = serde_json::to_value(&first)?;
    let row = &wire["page"]["items"][0];
    assert!(
        row["attempt_id"] == fixture.settled.attempt_id.to_string()
            && row.get("usage").is_none()
            && row.get("prompt").is_none()
            && row.get("trace").is_none(),
        "expected newest compact row, got {row}"
    );
    let RunResult::List { page, .. } = first else {
        return Err("list result".into());
    };
    assert!(page.truncated);
    let cursor = page.next_after.ok_or("next cursor")?;
    let second = owner.run(
        RunInput::List {
            extra: Default::default(),
            after: Some(cursor),
            limit: 1,
        },
        budget,
        &cancel,
    )?;
    let RunResult::List { page, .. } = second else {
        return Err("list result".into());
    };
    assert!(!page.truncated);
    assert!(page.next_after.is_none());
    let second = serde_json::to_value(&page.items[0])?;
    assert_ne!(second["attempt_id"], row["attempt_id"]);
    assert_eq!(second["state"], "cancelled");
    assert_eq!(
        second["reason"],
        provenant_records::run::Outcome::Cancelled.diagnostic_reason()
    );
    assert!(!second["reason"].as_str().unwrap().contains("fixture"));
    assert!(second["finished_at"].is_string());
    let status = owner.run(
        RunInput::Status {
            extra: Default::default(),
            target: AttemptTarget {
                extra: Default::default(),
                attempt: fixture.settled.attempt_id,
                host: None,
            },
        },
        budget,
        &cancel,
    )?;
    let RunResult::Status { mut attempt, .. } = status else {
        return Err("status result".into());
    };
    let status = serde_json::to_value(RunResult::Status {
        extra: Default::default(),
        attempt: attempt.clone(),
    })?;
    assert!(status["attempt"].get("usage").is_some());
    assert!(status["attempt"].get("prompt").is_some());
    // Pool routing carries an audit receipt in full records. It must not
    // enlarge the compact route returned by list.
    attempt.applied_route = Some(serde_json::from_value(serde_json::json!({
        "provider":"fixture", "model":"fixture/model", "effort":"high",
        "account_label":"fixture", "account_key":"private-key", "source":"pool",
        "pool_selection": {
            "pool":"senior", "candidates":[], "roll":0, "total_weight":1,
            "pick":"fixture/model"
        }
    }))?);
    let summary = provenant_fabric_runtime::runs::AttemptSummary::from(attempt);
    let summary = serde_json::to_value(summary)?;
    assert_eq!(
        summary["applied_route"],
        serde_json::json!({
            "provider":"fixture", "model":"fixture/model", "effort":"high",
            "account_label":"fixture"
        })
    );
    Ok(())
}

#[test]
fn graph_hint_requires_a_matching_attempt() -> Result<(), Box<dyn std::error::Error>> {
    if host_test(
        "fabric::peer_tests::peer_graph_settlement::graph_hint_requires_a_matching_attempt",
    )? {
        return Ok(());
    }
    use provenant_fabric_runtime::graph::{FileJournal, GraphJournal};
    let fixture = SettlementFixture::resumed()?;
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let call = provenant_run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let mut record = fixture.store.record(fixture.settled.attempt_id, &call)?;
    let document = serde_json::from_value(serde_json::json!({
        "schema_version":1,"graph_id":"hint-test","nodes":[{"id":"one","prompt":"work","access":"read_only","route":{"adapter":"fixture","model":"fixture/model"}}]
    }))?;
    let checkpoint = serde_json::from_value(serde_json::json!({
        "schema_version":1,"graph_id":"hint-test","plan_digest":"digest","nodes":[{
            "id":"one","worktree":null,"state":"running","attempts":[{
                "key":{"graph_id":"hint-test","node_id":"one","route_index":0, "attempt": 1},
                "route":{"adapter":"fixture","model":"fixture/model"},"state":"starting",
                "launched":true,"launch_actor":"fixture", "attempt_ref":format!("host/{}",record.attempt_id)
            }]
        }]
    }))?;
    let mut journal = FileJournal::open(
        &fixture.roots.state,
        fixture.settled.project_id,
        "hint-test",
        call.lock_budget(),
        &cancel,
        &clock,
    )?;
    journal.bind(provenant_fabric_runtime::graph::GraphExecution {
        extra: Default::default(),
        integration_base: None,
        review_base: None,
        directory: fixture.rig.project.clone(),
        document,
    })?;
    journal.save(&checkpoint)?;
    drop(journal);
    let operation = fixture
        .store
        .attempt_operation(record.attempt_id, &call)?
        .operation_id;
    assert!(
        matching_graph_hint(
            &fixture.roots.state,
            fixture.settled.project_id,
            &record,
            &operation
        )
        .is_none()
    );
    record.context = Some(provenant_records::run::WorkContext {
        graph_id: Some("unknown".into()),
        ..Default::default()
    });
    assert!(
        matching_graph_hint(
            &fixture.roots.state,
            fixture.settled.project_id,
            &record,
            &operation
        )
        .is_none()
    );
    record.context.as_mut().ok_or("context")?.graph_id = Some("hint-test".into());
    assert_eq!(
        matching_graph_hint(
            &fixture.roots.state,
            fixture.settled.project_id,
            &record,
            &operation
        )
        .as_deref(),
        Some("hint-test")
    );
    record.attempt_id = AttemptId::from_bytes([99; 16]);
    assert!(
        matching_graph_hint(
            &fixture.roots.state,
            fixture.settled.project_id,
            &record,
            &operation
        )
        .is_none()
    );
    Ok(())
}

#[test]
fn settlement_advances_only_matching_graph_and_retains_writer()
-> Result<(), Box<dyn std::error::Error>> {
    if host_test(
        "fabric::peer_tests::peer_graph_settlement::settlement_advances_only_matching_graph_and_retains_writer",
    )? {
        return Ok(());
    }
    use provenant_fabric_runtime::graph;
    struct Seed {
        reference: String,
        actor: String,
    }
    impl graph::AttemptOwner for Seed {
        type Error = std::convert::Infallible;
        fn worktree_layout(&self) -> provenant_config::WorktreeLayoutPort {
            provenant_config::PortTable::builtin().worktree_layout
        }
        fn actor(&self) -> String {
            self.actor.clone()
        }
        fn observe_or_start(
            &mut self,
            _: &graph::AttemptKey,
            _: graph::NodeInput<'_>,
            _: &graph::GraphRoute,
            _: graph::AccessMode,
            _: Option<&str>,
            _: graph::Replay<'_>,
        ) -> Result<graph::AttemptObservation, Self::Error> {
            Ok(graph::AttemptObservation::Pending {
                attempt_ref: Some(self.reference.clone()),
            })
        }
    }
    // The attempt's work key names matching/one; settlement reaches only the
    // graph that key names.
    let mut fixture = SettlementFixture::for_node("matching", "one")?;
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let identity = Context::open(
        fixture.roots.clone(),
        &fixture.rig.home,
        &fixture.rig.project,
    )?
    .identity;
    let budget = WaitBudget::cli(clock.mono_now(), 30)?;
    for id in ["matching", "unrelated"] {
        let document: graph::GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version":1,"graph_id":id,"nodes":[{"id":"one","prompt":"reply OK",
            "access":"worktree_write","worktree":fixture.worktree,"route":{"adapter":"fixture","model":"fixture/model"}}]
        }))?;
        let mut journal = graph::FileJournal::open(
            &fixture.roots.state,
            fixture.settled.project_id,
            id,
            budget,
            &cancel,
            &clock,
        )?;
        journal.bind(graph::GraphExecution {
            review_base: None,
            extra: Default::default(),
            integration_base: None,
            directory: fixture.rig.project.clone(),
            document: document.clone(),
        })?;
        let attempt = if id == "matching" {
            fixture.settled.attempt_id
        } else {
            AttemptId::from_bytes([99; 16])
        };
        let mut seed = Seed {
            reference: format!("{}/{attempt}", identity.host),
            actor: identity.inbox.actor.to_string(),
        };
        graph::advance_with_budget(
            &document,
            &mut seed,
            &mut journal,
            &Default::default(),
            &Default::default(),
        )?;
    }
    fixture.settled.launch = Some(provenant_run::HostLaunchContext {
        graph_directory: None,
        configuration_root: fixture.rig.state.clone(),
        cwd: fixture.rig.project.clone(),
        executables: Default::default(),
        executable_search_path: Vec::new(),
        git: provenant_os::git::git_executable().ok_or("git")?,
        default_account_directories: Default::default(),
        credential_roots: Default::default(),
        github_roots: Default::default(),
        provider_path_environment: Default::default(),
        cargo_build_jobs: None,
        fixture_state_write: false,
    });
    advance_graphs_after_settlement(&fixture.settled, &FreeWorkers)?;
    let matched = graph::FileJournal::read_checkpoint(
        &fixture.roots.state,
        fixture.settled.project_id,
        "matching",
    )?
    .ok_or("matching")?;
    let other = graph::FileJournal::read_checkpoint(
        &fixture.roots.state,
        fixture.settled.project_id,
        "unrelated",
    )?
    .ok_or("unrelated")?;
    assert_eq!(matched.nodes[0].state, graph::NodeState::Succeeded);
    assert_eq!(other.nodes[0].state, graph::NodeState::Running);
    assert!(fixture.worktree.exists());
    let call = provenant_run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let receipt = fixture
        .store
        .worktree_settlement(fixture.settled.attempt_id, &call)?
        .ok_or("receipt")?;
    assert_eq!(receipt["state"], "retained");
    Ok(())
}

#[test]
fn settlement_and_tick_read_no_terminal_graphs_at_scale() -> Result<(), Box<dyn std::error::Error>>
{
    if host_test(
        "fabric::peer_tests::peer_graph_settlement::settlement_and_tick_read_no_terminal_graphs_at_scale",
    )? {
        return Ok(());
    }
    use provenant_fabric_runtime::graph;
    struct Seed {
        reference: String,
        actor: String,
        at: provenant_core::WallTime,
    }
    impl graph::AttemptOwner for Seed {
        type Error = std::convert::Infallible;
        fn worktree_layout(&self) -> provenant_config::WorktreeLayoutPort {
            provenant_config::PortTable::builtin().worktree_layout
        }
        fn actor(&self) -> String {
            self.actor.clone()
        }
        fn wall_now(&self) -> Option<provenant_core::WallTime> {
            Some(self.at)
        }
        fn observe_or_start(
            &mut self,
            _: &graph::AttemptKey,
            _: graph::NodeInput<'_>,
            _: &graph::GraphRoute,
            _: graph::AccessMode,
            _: Option<&str>,
            _: graph::Replay<'_>,
        ) -> Result<graph::AttemptObservation, Self::Error> {
            Ok(graph::AttemptObservation::Pending {
                attempt_ref: Some(self.reference.clone()),
            })
        }
    }
    let mut fixture = SettlementFixture::for_node("owner", "one")?;
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let identity = Context::open(
        fixture.roots.clone(),
        &fixture.rig.home,
        &fixture.rig.project,
    )?
    .identity;
    let budget = WaitBudget::cli(clock.mono_now(), 30)?;
    let now = clock.wall_now()?;
    let mut seed = Seed {
        reference: format!("{}/{}", identity.host, fixture.settled.attempt_id),
        actor: identity.inbox.actor.to_string(),
        at: now,
    };
    for number in 0..20 {
        let id = if number == 0 {
            "owner".to_owned()
        } else {
            format!("pending-{number:02}")
        };
        let nodes = if number == 0 {
            serde_json::json!([{"id":"one","prompt":"reply OK","access":"worktree_write",
            "worktree":fixture.worktree,"route":{"adapter":"fixture","model":"fixture/model"}}])
        } else {
            serde_json::json!([{"id":"choose","kind":"decide","access":"read_only","decide":{"on_unanswered":"block_dependants",
            "question":"Ship?","options":[{"id":"ship","label":"Ship"},{"id":"hold","label":"Hold"}],
            "default":"hold","timeout_seconds":3600}}])
        };
        let document: graph::GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version":1,"graph_id":id,"decision_session":"chair-session","nodes":nodes
        }))?;
        let mut journal = graph::FileJournal::open(
            &fixture.roots.state,
            fixture.settled.project_id,
            &id,
            budget,
            &cancel,
            &clock,
        )?;
        journal.bind(graph::GraphExecution {
            review_base: None,
            extra: Default::default(),
            integration_base: None,
            directory: fixture.rig.project.clone(),
            document: document.clone(),
        })?;
        graph::advance_with_budget(
            &document,
            &mut seed,
            &mut journal,
            &Default::default(),
            &Default::default(),
        )?;
    }
    let mut terminal = graph::FileJournal::read_checkpoint(
        &fixture.roots.state,
        fixture.settled.project_id,
        "owner",
    )?
    .ok_or("owner")?;
    let mut execution =
        graph::FileJournal::execution(&fixture.roots.state, fixture.settled.project_id, "owner")?
            .ok_or("execution")?;
    terminal.nodes[0].state = graph::NodeState::Succeeded;
    terminal.nodes[0].attempts.clear();
    let directory = fixture
        .roots
        .state
        .as_path()
        .join("fabric/graphs")
        .join(fixture.settled.project_id.to_string());
    for number in 0..5000 {
        let id = format!("terminal-{number:04}");
        terminal.graph_id = id.clone();
        execution.document.graph_id = id.clone();
        let digest = provenant_records::CanonicalV1::record_digest(
            "fabric-graph-checkpoint",
            2,
            &(&terminal, &execution),
        )?
        .to_string();
        let envelope = serde_json::json!({"schema_version":1,"digest":digest,"checkpoint":terminal,"execution":execution});
        std::fs::write(
            directory.join(format!("graph-{id}.json")),
            serde_json::to_vec(&envelope)?,
        )?;
    }
    assert_eq!(
        graph::FileJournal::pending_graph_ids(&fixture.roots.state, fixture.settled.project_id)?
            .len(),
        20
    );
    graph::take_checkpoint_read_ids();
    settle_owned_worktree(&fixture.settled, &fixture.roots, &FreeWorkers, None)?;
    let reads = graph::take_checkpoint_read_ids();
    assert!(!reads.is_empty());
    assert!(reads.iter().all(|id| id == "owner"), "{reads:?}");
    fixture.settled.launch = Some(provenant_run::HostLaunchContext {
        graph_directory: None,
        configuration_root: fixture.rig.state.clone(),
        cwd: fixture.rig.project.clone(),
        executables: Default::default(),
        executable_search_path: Vec::new(),
        git: provenant_os::git::git_executable().ok_or("git")?,
        default_account_directories: Default::default(),
        credential_roots: Default::default(),
        github_roots: Default::default(),
        provider_path_environment: Default::default(),
        cargo_build_jobs: None,
        fixture_state_write: false,
    });
    advance_graphs_after_settlement(&fixture.settled, &FreeWorkers)?;
    let reads = graph::take_checkpoint_read_ids();
    assert!(!reads.is_empty());
    assert!(reads.iter().all(|id| id == "owner"), "{reads:?}");
    let resolved = crate::roots::Resolved {
        assets: fixture.rig.project.clone(),
        home: fixture.rig.home.clone(),
        project: fixture.rig.project.clone(),
        roots: fixture.roots.clone(),
        claude_config: None,
        codex_home: None,
    };
    let report = tick_graphs(&resolved, clock.wall_now()?, &FreeWorkers)?;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let reads = graph::take_checkpoint_read_ids();
    assert!(!reads.is_empty());
    assert!(
        reads.iter().all(|id| id.starts_with("pending-")),
        "{reads:?}"
    );
    Ok(())
}

#[test]
fn an_attempt_keyed_to_another_graph_is_refused_not_adopted()
-> Result<(), Box<dyn std::error::Error>> {
    if host_test(
        "fabric::peer_tests::peer_graph_settlement::an_attempt_keyed_to_another_graph_is_refused_not_adopted",
    )? {
        return Ok(());
    }
    // An attempt's work key is its graph identity (#3078). A graph node
    // that references an attempt admitted under another graph's key is
    // corrupt state: neither settlement nor tick may adopt that result.
    use provenant_fabric_runtime::graph;
    struct Seed {
        reference: String,
        actor: String,
    }
    impl graph::AttemptOwner for Seed {
        type Error = std::convert::Infallible;
        fn worktree_layout(&self) -> provenant_config::WorktreeLayoutPort {
            provenant_config::PortTable::builtin().worktree_layout
        }
        fn actor(&self) -> String {
            self.actor.clone()
        }
        fn observe_or_start(
            &mut self,
            _: &graph::AttemptKey,
            _: graph::NodeInput<'_>,
            _: &graph::GraphRoute,
            _: graph::AccessMode,
            _: Option<&str>,
            _: graph::Replay<'_>,
        ) -> Result<graph::AttemptObservation, Self::Error> {
            Ok(graph::AttemptObservation::Pending {
                attempt_ref: Some(self.reference.clone()),
            })
        }
    }
    let mut fixture = SettlementFixture::for_node("elsewhere", "one")?;
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let identity = Context::open(
        fixture.roots.clone(),
        &fixture.rig.home,
        &fixture.rig.project,
    )?
    .identity;
    let budget = WaitBudget::cli(clock.mono_now(), 30)?;
    let document: graph::GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version":1,"graph_id":"owner","nodes":[{"id":"one","prompt":"reply OK",
        "access":"worktree_write","worktree":fixture.worktree,"route":{"adapter":"fixture","model":"fixture/model"}}]
    }))?;
    let mut journal = graph::FileJournal::open(
        &fixture.roots.state,
        fixture.settled.project_id,
        "owner",
        budget,
        &cancel,
        &clock,
    )?;
    journal.bind(graph::GraphExecution {
        review_base: None,
        extra: Default::default(),
        integration_base: None,
        directory: fixture.rig.project.clone(),
        document: document.clone(),
    })?;
    let mut seed = Seed {
        reference: format!("{}/{}", identity.host, fixture.settled.attempt_id),
        actor: identity.inbox.actor.to_string(),
    };
    graph::advance_with_budget(
        &document,
        &mut seed,
        &mut journal,
        &Default::default(),
        &Default::default(),
    )?;
    drop(journal);
    fixture.settled.launch = Some(provenant_run::HostLaunchContext {
        graph_directory: None,
        configuration_root: fixture.rig.state.clone(),
        cwd: fixture.rig.project.clone(),
        executables: Default::default(),
        executable_search_path: Vec::new(),
        git: provenant_os::git::git_executable().ok_or("git")?,
        default_account_directories: Default::default(),
        credential_roots: Default::default(),
        github_roots: Default::default(),
        provider_path_environment: Default::default(),
        cargo_build_jobs: None,
        fixture_state_write: false,
    });
    advance_graphs_after_settlement(&fixture.settled, &FreeWorkers)?;
    let resolved = crate::roots::Resolved {
        assets: fixture.rig.project.clone(),
        home: fixture.rig.home.clone(),
        project: fixture.rig.project.clone(),
        roots: fixture.roots.clone(),
        claude_config: None,
        codex_home: None,
    };
    let report = tick_graphs(&resolved, clock.wall_now()?, &FreeWorkers)?;
    assert!(
        report
            .errors
            .iter()
            .any(|error| error.contains("/owner:") && error.contains("corrupt")),
        "{:?}",
        report.errors
    );
    let after = graph::FileJournal::read_checkpoint(
        &fixture.roots.state,
        fixture.settled.project_id,
        "owner",
    )?
    .ok_or("owner")?;
    assert_ne!(after.nodes[0].state, graph::NodeState::Succeeded);
    assert!(fixture.worktree.exists());
    Ok(())
}
