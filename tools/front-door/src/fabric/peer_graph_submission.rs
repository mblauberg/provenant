#[allow(unused_imports)]
use super::*;

#[test]
fn graph_owner_submits_sibling_families_as_a_preference_and_alias_retry_exclusions()
-> Result<(), Box<dyn std::error::Error>> {
    if host_test(
        "fabric::peer_tests::peer_graph_submission::graph_owner_submits_sibling_families_as_a_preference_and_alias_retry_exclusions",
    )? {
        return Ok(());
    }
    use provenant_fabric_runtime::graph::{self, AttemptOwner};
    use provenant_records::run::RequestedRoute;
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
        mcp_project: None,
        workers: &FreeWorkers,
    };
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    let mut bound = BoundGraphOwner { writer_launch_budget: None, admission_timeout: None, launch_warnings: Vec::new(), repo_facts: Default::default(),
        factory: &factory,
        integration_base: None,
        integration_branch: None,
        network: None, capability_overrides: Default::default(),
        exclude: Vec::new(),
        budget,
        cancel: &cancel,
        prices: None,
        tick: false,
    };
    let actor = bound.actor();
    let owner = BinaryRunOwner {
        factory: &factory,
        allow_remote: true,
        actor: None,
        chair: None,
    };
    let store = owner.store(&cancel)?;
    let call = owner.call(&cancel);
    let upstream = provenant_run::Resume {
        prior: fixture.settled.attempt_id, expected_generation: 0,
        operation_id: "family-composition-writer".parse()?,
        run_id: RunId::from_bytes([71; 16]), attempt_id: AttemptId::from_bytes([72; 16]),
        prompt: "writer".into(), created_at: factory.clock.wall_now()?,
    };
    fixture.store.resume_fresh(&upstream, "fixture", &call)?;
    let record = fixture.store.record(upstream.attempt_id, &call)?;
    let head = fixture.store.record(fixture.settled.attempt_id, &call)?.head.ok_or("head")?;
    publish_routed(&fixture.rig, &fixture.store, &record, head, Outcome::Succeeded, "codex/gpt-6-luna", &call)?;
    let writer = format!("{}/{}", factory.context.identity.host, upstream.attempt_id);
    let alias: graph::GraphRoute = serde_json::from_value(serde_json::json!({"alias":"junior"}))?;
    // Submit one lens attempt through the production owner and return the
    // route and knobs it recorded.
    let mut submit = |node_id: &str,
                      route_index: u16,
                      peers: &[String],
                      prior: &[String],
                      require: bool, distinct: &[String]|
     -> Result<(RequestedRoute, Vec<String>), Box<dyn std::error::Error>> {
        let key: graph::AttemptKey = serde_json::from_value(serde_json::json!({
            "graph_id":"sibling-lenses", "node_id":node_id, "route_index":route_index
        }))?;
        bound.observe_or_start(&key, graph::NodeInput {
        attachments: &[], capabilities: None, capability_sources: None, attempt: None, require_skill_location: false, settlement_retries: None, review_base: None,
            prior_attempts: prior, family_peers: peers, context: graph_test_context(&key), read_roots: &[],
            distinct_from_attempts: distinct, require_distinct: require, source_commit: None,
            title: "lens", prompt: "review", required_verdict: None, deadline_seconds: None,
            inputs_dir: None,
        }, &alias, graph::AccessMode::ReadOnly, None, graph::Replay {
            delivery: None, branch: None, fast_forward_only: false, stored_ref: None, launch_actor: &actor,
        })?;
        let operation = graph_operation("graph-attempt-operation", &key)?;
        let operation = store.operation(&operation, &owner.call(&cancel))?.ok_or("lens operation missing")?;
        let request = store.request(operation.run_id, &owner.call(&cancel))?;
        let task = request.tasks.first().ok_or("task missing")?;
        Ok((task.route.clone(), task.profile.overrides.clone()))
    };
    let review = |excluded: &[&str]| RequestedRoute::Review {
        extra: Default::default(),
        name: "junior".into(),
        effort: None,
        excluded_families: excluded.iter().map(|family| (*family).to_owned()).collect(),
        distinct_families: true,
        preferred_families: vec![],
        require_distinct: false,
    };
    // The first sibling has no peers yet and takes the plain alias.
    let (route, _) = submit("one", 0, &[], &[], false, &[])?;
    assert_eq!(route, RequestedRoute::Named { extra: Default::default(), name: "junior".into(), effort: None });
    // Later siblings prefer other families. A preference never refuses, so
    // with one family available every lens still runs.
    let peers = ["openai".to_owned(), "anthropic".to_owned()];
    assert_eq!(submit("two", 0, &peers[..1], &[], false, &[])?.0, review(&["openai"]));
    assert_eq!(submit("three", 0, &peers, &[], false, &[])?.0, review(&["openai", "anthropic"]));
    // A required writer separation with no writer family to enforce still
    // leaves sibling families as a preference.
    assert_eq!(submit("four", 0, &peers[..1], &[], true, &[])?.0, review(&["openai"]));
    let accepted = submit("five", 0, &peers[1..], &[], true, std::slice::from_ref(&writer))?.0;
    assert_eq!(accepted, RequestedRoute::Review {
        extra: Default::default(), name: "junior".into(), effort: None,
        excluded_families: vec!["openai".into()], distinct_families: true,
        preferred_families: vec!["anthropic".into()], require_distinct: true,
    });
    // An accepted operation without a checkpoint reference retains its
    // original route even if the newly observed peers have changed.
    assert_eq!(submit("five", 0, &[], &[], true, &[writer])?.0, accepted);
    // An alias retry excludes the model ids the runtime resolved.
    let (route, knobs) = submit("two", 1, &peers[..1], &["codex/first".to_owned()], false, &[])?;
    assert_eq!(route, review(&["openai"]));
    assert_eq!(knobs, [r#"routing.graph_exclude=["codex/first"]"#]);
    Ok(())
}

#[test]
fn graph_submission_refuses_a_push_to_main_before_admission()
-> Result<(), Box<dyn std::error::Error>> {
    if host_test(
        "fabric::peer_tests::peer_graph_submission::graph_submission_refuses_a_push_to_main_before_admission",
    )? {
        return Ok(());
    }
    use provenant_confinement::worktrees::{self, WorktreeCreate};
    use provenant_fabric_runtime::graph::FileJournal;
    let fixture = SettlementFixture::resumed()?;
    let cancel = CancelToken::new();
    let info = worktrees::with_context(
        &fixture.roots,
        &fixture.rig.home,
        &fixture.rig.project,
        &SystemClock,
        &cancel,
        |context| {
            worktrees::create(
                &worktrees::WorktreeContext {
                    workers: &FreeWorkers,
                    ..*context
                },
                WorktreeCreate {
                    branch: "main".into(),
                    base: Some("HEAD".into()),
                },
                &cancel,
            )
        },
    )?;
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
        mcp_project: None,
        workers: &FreeWorkers,
    };
    let document = fixture.rig.project.join("protected-push-graph.json");
    std::fs::write(
        &document,
        serde_json::to_vec(&serde_json::json!({
            "schema_version":1, "graph_id":"protected-push",
            "nodes":[{"id":"one","prompt":"reply OK","access":"worktree_write",
                "worktree":info.path,"delivery":"push",
                "route":{"adapter":"fixture","model":"fixture/model"}}]
        }))?,
    )?;
    let result = factory.graph(
        GraphInput::Run {
            extra: Default::default(),
            document,
        },
        WaitBudget::cli(factory.clock.mono_now(), 30)?,
        &cancel,
    );
    assert!(
        FileJournal::read_checkpoint(
            &fixture.roots.state,
            fixture.settled.project_id,
            "protected-push"
        )?
        .is_none(),
        "protected push was admitted: {result:?}"
    );
    assert!(
        matches!(result, Err(FacadeError::ConfinementRefused { .. })),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn graph_submission_refuses_a_worktree_on_its_integration_branch_before_admission()
-> Result<(), Box<dyn std::error::Error>> {
    if host_test(
        "fabric::peer_tests::peer_graph_submission::graph_submission_refuses_a_worktree_on_its_integration_branch_before_admission",
    )? {
        return Ok(());
    }
    let fixture = SettlementFixture::resumed()?;
    let (result, admitted) = integrate_graph(&fixture, "integrate-own-branch", "resumed-lane")?;
    assert!(!admitted, "graph was admitted: {result:?}");
    let Err(FacadeError::DispatchRefused { reason, .. }) = result else {
        return Err(format!("expected a dispatch refusal: {result:?}").into());
    };
    assert!(
        reason.contains("is on the integration branch resumed-lane")
            && reason.contains("git worktree add -b batch/work PATH BASE"),
        "{reason}"
    );
    Ok(())
}

#[test]
fn graph_submission_admits_a_worktree_on_a_separate_branch()
-> Result<(), Box<dyn std::error::Error>> {
    if host_test("fabric::peer_tests::peer_graph_submission::graph_submission_admits_a_worktree_on_a_separate_branch")?
    {
        return Ok(());
    }
    let fixture = SettlementFixture::resumed()?;
    let (result, admitted) = integrate_graph(&fixture, "integrate-separate", "batch/graph")?;
    assert!(admitted, "graph was refused: {result:?}");
    assert!(result.is_ok(), "{result:?}");
    Ok(())
}

#[test]
fn bare_graph_miss_names_the_scratch_root_and_searches_no_legacy_root()
-> Result<(), Box<dyn std::error::Error>> {
    if host_test(
        "fabric::peer_tests::peer_graph_submission::bare_graph_miss_names_the_scratch_root_and_searches_no_legacy_root",
    )? {
        return Ok(());
    }
    let fixture = SettlementFixture::resumed()?;
    // A readable document under the legacy root must not be found by a bare name.
    let legacy = fixture.rig.project.join(".agent-run/graphs");
    std::fs::create_dir_all(&legacy)?;
    std::fs::write(legacy.join("legacy-plan.json"), b"not a graph")?;
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
        mcp_project: None,
        workers: &FreeWorkers,
    };
    let result = factory.graph(
        GraphInput::Run {
            extra: Default::default(),
            document: "legacy-plan.json".into(),
        },
        WaitBudget::cli(factory.clock.mono_now(), 30)?,
        &CancelToken::new(),
    );
    match result {
        Err(FacadeError::Io { operation, .. }) => {
            assert!(
                operation.starts_with("read project-local graph document")
                    && operation.contains(".provenant/scratch/graphs")
                    && operation.contains("explicit path"),
                "{operation}"
            );
        }
        other => panic!("bare-name miss must not read a legacy document: {other:?}"),
    }
    Ok(())
}

#[test]
fn graph_submission_replays_an_accepted_push_after_protection_changes()
-> Result<(), Box<dyn std::error::Error>> {
    if host_test(
        "fabric::peer_tests::peer_graph_submission::graph_submission_replays_an_accepted_push_after_protection_changes",
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
        mcp_project: None,
        workers: &FreeWorkers,
    };
    let document = fixture.rig.project.join("replay-push-graph.json");
    std::fs::write(
        &document,
        serde_json::to_vec(&serde_json::json!({
            "schema_version":1, "graph_id":"replay-push", "template_library":{},
            "nodes":[{"id":"one","prompt":"reply OK","access":"worktree_write",
                "worktree":fixture.worktree,"delivery":"push",
                "route":{"adapter":"fixture","model":"fixture/model"}}]
        }))?,
    )?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    let run = factory.graph(
        GraphInput::Run {
            extra: Default::default(),
            document: document.clone(),
        },
        budget,
        &cancel,
    )?;
    assert_eq!(
        run.checkpoint.nodes[0].state,
        provenant_fabric_runtime::graph::NodeState::Failed
    );
    assert!(!run.checkpoint.nodes[0].attempts[0].launched);
    let git = provenant_os::git::git_executable().ok_or("git")?;
    for args in [
        vec![
            "remote",
            "add",
            "origin",
            fixture.rig.project.to_str().ok_or("project")?,
        ],
        vec![
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/resumed-lane",
        ],
    ] {
        let output = fixture.rig.execute(&git, &args, None)?;
        assert_eq!(crate::worktree_tests::support::code(&output), 0);
    }
    for input in [
        GraphInput::Run {
            extra: Default::default(),
            document: document.clone(),
        },
        GraphInput::Resume {
            extra: Default::default(),
            document,
            extend: vec![],
        },
    ] {
        let replay = factory.graph(input, budget, &cancel)?;
        assert_eq!(replay.checkpoint, run.checkpoint);
    }
    Ok(())
}

#[test]
fn graph_submission_pins_the_integration_base_across_replay()
-> Result<(), Box<dyn std::error::Error>> {
    if host_test(
        "fabric::peer_tests::peer_graph_submission::graph_submission_pins_the_integration_base_across_replay",
    )? {
        return Ok(());
    }
    use provenant_fabric_runtime::graph::FileJournal;
    let fixture = SettlementFixture::resumed()?;
    let git = provenant_os::git::git_executable().ok_or("git")?;
    let output = fixture
        .rig
        .execute(&git, &["branch", "integration"], None)?;
    assert_eq!(crate::worktree_tests::support::code(&output), 0);
    for args in [
        vec!["remote", "add", "origin", fixture.rig.project.to_str().ok_or("path")?],
        vec!["update-ref", "refs/heads/main", "HEAD"],
        vec!["update-ref", "refs/remotes/origin/main", "HEAD"],
    ] {
        let output = fixture.rig.execute(&git, &args, None)?;
        assert_eq!(crate::worktree_tests::support::code(&output), 0);
    }
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
        mcp_project: None,
        workers: &FreeWorkers,
    };
    let document = fixture.rig.project.join("base-graph.json");
    std::fs::write(
        &document,
        serde_json::to_vec(&serde_json::json!({
            "schema_version":2, "graph_id":"pinned-base", "template_library":{}, "integration_branch":"integration", "review_base":"origin/main", "delivery":"pr",
            "nodes":[{"id":"one","prompt":"reply OK","access":"read_only","route":{"adapter":"fixture","model":"fixture/model"}}]
        }))?,
    )?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    // The fixture adapter is unavailable, so no provider launches. The
    // first graph checkpoint must already contain its integration base.
    let run = factory.graph(
        GraphInput::Run {
            extra: Default::default(),
            document: document.clone(),
        },
        budget,
        &cancel,
    )?;
    assert_eq!(
        run.checkpoint.nodes[0].state,
        provenant_fabric_runtime::graph::NodeState::Failed
    );
    assert!(!run.checkpoint.nodes[0].attempts[0].launched);
    let owner = BinaryRunOwner {
        factory: &factory,
        allow_remote: true,
        actor: None,
        chair: None,
    };
    let store = owner.store(&cancel)?;
    let operation = graph_operation(
        "graph-attempt-operation",
        &run.checkpoint.nodes[0].attempts[0].key,
    )?;
    let operation = store
        .operation(&operation, &owner.call(&cancel))?
        .ok_or("graph run not admitted")?;
    let index_call = provenant_fabric_runtime::chairs::Request {
        clock: &factory.clock,
        cancel: &cancel,
    };
    let binding = provenant_fabric_runtime::chairs::ChairIndex::open(
        &factory.context.roots.state,
        &index_call,
    )?
    .read(
        factory.context.identity.project,
        operation.run_id,
        &index_call,
    )?
    .ok_or("graph index missing")?;
    assert!(binding.chair.is_none(), "graph created a chair association");
    let base = {
        let journal = FileJournal::open(
            &fixture.roots.state,
            fixture.settled.project_id,
            "pinned-base",
            budget,
            &cancel,
            &factory.clock,
        )?;
        journal.integration_base().ok_or("graph base missing")?
    };
    assert_eq!(
        Some(base),
        fixture
            .store
            .record(
                fixture.settled.attempt_id,
                &provenant_run::Request {
                    clock: &factory.clock,
                    cancel: &cancel,
                }
            )?
            .head
    );
    let output = fixture.rig.execute(&git, &["branch", "old-review", &base.to_string()], None)?;
    assert_eq!(crate::worktree_tests::support::code(&output), 0);
    std::fs::write(fixture.rig.project.join("guide.md"), "small change\n")?;
    let output = fixture.rig.execute(&git, &["add", "guide.md"], None)?;
    assert_eq!(crate::worktree_tests::support::code(&output), 0);
    for args in [
        vec![
            "commit",
            "--allow-empty",
            "-qm",
            "Unrelated primary checkout",
        ],
        vec!["branch", "-f", "integration", "HEAD"],
        vec!["update-ref", "refs/heads/main", "HEAD"],
        vec!["update-ref", "refs/remotes/origin/main", "HEAD"],
    ] {
        let output = fixture.rig.execute(&git, &args, None)?;
        assert_eq!(crate::worktree_tests::support::code(&output), 0);
    }
    let replay = factory.graph(
        GraphInput::Resume {
            extra: Default::default(),
            document,
            extend: vec![],
        },
        budget,
        &cancel,
    )?;
    assert_eq!(replay.checkpoint, run.checkpoint);
    let journal = FileJournal::open(
        &fixture.roots.state,
        fixture.settled.project_id,
        "pinned-base",
        budget,
        &cancel,
        &factory.clock,
    )?;
    assert_eq!(journal.integration_base(), Some(base));
    let execution = FileJournal::execution(&fixture.roots.state, fixture.settled.project_id, "pinned-base")?.ok_or("execution missing")?;
    assert_eq!(execution.review_base, Some(base));
    drop(journal);
    let head_output = fixture.rig.execute(&git, &["rev-parse", "HEAD"], None)?;
    let head = String::from_utf8(head_output.stdout.bytes)?.trim().to_owned();
    // A later lens admitted after the reference moved still reads the pinned
    // review base, independently of the integration base.
    use provenant_fabric_runtime::graph::{self, AttemptOwner};
    let key: graph::AttemptKey = serde_json::from_value(serde_json::json!({
        "graph_id":"pinned-base", "node_id":"later-lens", "route_index":0
    }))?;
    let route = serde_json::from_value(serde_json::json!({"adapter":"fixture", "model":"fixture/model"}))?;
    let mut bound = BoundGraphOwner { writer_launch_budget: None, admission_timeout: None, launch_warnings: Vec::new(), repo_facts: Default::default(),
        factory: &factory, integration_base: None, integration_branch: None,
        network: None, capability_overrides: Default::default(), exclude: Vec::new(), budget, cancel: &cancel, prices: None, tick: false,
    };
    let actor = bound.actor();
    bound.observe_or_start(&key, graph::NodeInput {
        attachments: &[], capabilities: None, capability_sources: None, attempt: None, require_skill_location: false, settlement_retries: None, review_base: None,
        prior_attempts: &[], family_peers: &[], context: graph_test_context(&key), read_roots: &[],
        distinct_from_attempts: &[], require_distinct: false, source_commit: Some(&head),
        title: "later lens", prompt: "review", required_verdict: None, deadline_seconds: None,
        inputs_dir: None,
    }, &route, graph::AccessMode::ReadOnly, None, graph::Replay {
        delivery: None, branch: None, fast_forward_only: false, stored_ref: None, launch_actor: &actor,
    })?;
    let operation = graph_operation("graph-attempt-operation", &key)?;
    let operation = store.operation(&operation, &owner.call(&cancel))?.ok_or("later lens operation missing")?;
    let request = store.request(operation.run_id, &owner.call(&cancel))?;
    let base_text = base.to_string();
    assert_eq!(request.tasks[0].base.as_deref(), Some(base_text.as_str()));
    assert_ne!(base_text, head);
    // A different integration base does not displace the pinned review base.
    bound.integration_base = Some(BoundGraphBases {
        integration: Some(provenant_core::GitOid::parse(&head)?),
        review: Some(base),
    });
    let second: graph::AttemptKey = serde_json::from_value(serde_json::json!({
        "graph_id":"pinned-base", "node_id":"second-lens", "route_index":0
    }))?;
    let actor = bound.actor();
    bound.observe_or_start(&second, graph::NodeInput {
        attachments: &[], capabilities: None, capability_sources: None, attempt: None, require_skill_location: false, settlement_retries: None, review_base: None,
        prior_attempts: &[], family_peers: &[], context: graph_test_context(&second), read_roots: &[],
        distinct_from_attempts: &[], require_distinct: false, source_commit: Some(&head),
        title: "second lens", prompt: "review", required_verdict: None, deadline_seconds: None,
        inputs_dir: None,
    }, &route, graph::AccessMode::ReadOnly, None, graph::Replay {
        delivery: None, branch: None, fast_forward_only: false, stored_ref: None, launch_actor: &actor,
    })?;
    let operation = graph_operation("graph-attempt-operation", &second)?;
    let operation = store.operation(&operation, &owner.call(&cancel))?.ok_or("second lens operation missing")?;
    let request = store.request(operation.run_id, &owner.call(&cancel))?;
    assert_eq!(request.tasks[0].base.as_deref(), Some(base_text.as_str()));
    bound.integration_base = Some(BoundGraphBases { integration: None, review: Some(base) });
    for (id, reference, expected) in [("review-base", "old-review", base_text.as_str()), ("empty-review-base", "", head.as_str())] {
        let path = fixture.rig.project.join(format!("{id}.json"));
        std::fs::write(&path, serde_json::to_vec(&serde_json::json!({
            "schema_version":2, "graph_id":id, "template_library":{}, "review_base":reference,
            "nodes":[{"id":"lens","prompt":"review","access":"read_only", "source":{"type":"commit","commit":head}, "route":{"adapter":"fixture","model":"fixture/model"}}]
        }))?)?;
        let result = factory.graph(GraphInput::Run { extra: Default::default(), document: path }, budget, &cancel)?;
        let operation = graph_operation("graph-attempt-operation", &result.checkpoint.nodes[0].attempts[0].key)?;
        let operation = store.operation(&operation, &owner.call(&cancel))?.ok_or("operation missing")?;
        let request = store.request(operation.run_id, &owner.call(&cancel))?;
        assert_eq!(request.tasks[0].base.as_deref(), Some(expected));
        assert_eq!(request.tasks[0].head.as_deref(), Some(head.as_str()));
    }
    Ok(())
}

#[test]
fn detached_writer_reports_branch_requirement() -> Result<(), Box<dyn std::error::Error>> {
    if host_test("fabric::peer_tests::peer_graph_submission::detached_writer_reports_branch_requirement")? { return Ok(()); }
    let fixture = SettlementFixture::resumed()?;
    let git = provenant_os::git::git_executable().ok_or("git")?;
    let output = fixture.rig.execute(&git, &["-C", fixture.worktree.to_str().ok_or("path")?, "checkout", "--detach"], None)?;
    assert_eq!(crate::worktree_tests::support::code(&output), 0);
    let factory = Factory { context: Context::open(fixture.roots.clone(), &fixture.rig.home, &fixture.rig.project)?, project_attribution: false, clock: SystemClock, backoff: ThreadPause, executor: SystemExecutor::new(1)?, home: fixture.rig.home.clone(), cwd: fixture.rig.project.clone(), assets: fixture.rig.project.clone(), host_launch: None, mcp_project: None, workers: &FreeWorkers };
    let document = fixture.rig.project.join("detached-writer.json");
    std::fs::write(&document, serde_json::to_vec(&serde_json::json!({"schema_version":1,"graph_id":"detached-writer", "template_library":{},"nodes":[{"id":"writer","prompt":"write","access":"worktree_write","worktree":fixture.worktree,"route":{"adapter":"fixture","model":"fixture/model"}}]}))?)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    let run = factory.graph(GraphInput::Run { extra: Default::default(), document }, budget, &cancel)?;
    let node = &run.checkpoint.nodes[0];
    assert!(node.advance_error.as_deref().is_some_and(|error| error.contains("writer needs a branch")), "{node:#?}");
    let detached_factory = Factory { context: Context::open(fixture.roots.clone(), &fixture.rig.home, &fixture.worktree)?, project_attribution: false, clock: SystemClock, backoff: ThreadPause, executor: SystemExecutor::new(1)?, home: fixture.rig.home.clone(), cwd: fixture.worktree.clone(), assets: fixture.rig.project.clone(), host_launch: None, mcp_project: None, workers: &FreeWorkers };
    let head = fixture.rig.execute(&git, &["-C", fixture.worktree.to_str().ok_or("path")?, "rev-parse", "HEAD"], None)?;
    let head = String::from_utf8(head.stdout.bytes)?.trim().to_owned();
    let document = fixture.worktree.join("detached-review.json");
    std::fs::write(&document, serde_json::to_vec(&serde_json::json!({"schema_version":1,"graph_id":"detached-review", "template_library":{},"nodes":[{"id":"review","prompt":"review","access":"read_only","source":{"type":"commit","commit":head},"route":{"adapter":"fixture","model":"fixture/model"}}]}))?)?;
    let budget = WaitBudget::cli(detached_factory.clock.mono_now(), 30)?;
    let run = detached_factory.graph(GraphInput::Run { extra: Default::default(), document }, budget, &cancel)?;
    let node = &run.checkpoint.nodes[0];
    assert!(node.advance_error.is_none(), "{node:#?}");
    let key = &node.attempts.first().ok_or("review attempt missing")?.key;
    let operation = graph_operation("graph-attempt-operation", key)?;
    let owner = BinaryRunOwner { factory: &detached_factory, allow_remote: true, actor: None, chair: None };
    assert!(owner.store(&cancel)?.operation(&operation, &owner.call(&cancel))?.is_some(), "review was not submitted");
    Ok(())
}

#[test]
fn retained_lane_settlement_collects_only_its_output_and_cache()
-> Result<(), Box<dyn std::error::Error>> {
    if host_test(
        "fabric::peer_tests::peer_graph_submission::retained_lane_settlement_collects_only_its_output_and_cache",
    )? {
        return Ok(());
    }
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let fixture = SettlementFixture::resumed()?;
    std::fs::write(
        fixture.rig.project.join(".git/info/exclude"),
        "build/\nsaved/\n",
    )?;
    std::fs::write(
        fixture.rig.state.join("config.toml"),
        format!(
            "schema_version=1\n[gc.projects.\"{}\"]\nregenerable=['build','saved']\nkeep=['saved']\nforeign_worktrees='merged-clean'\n",
            fixture.settled.project_id
        ),
    )?;
    std::fs::create_dir(fixture.worktree.join("build"))?;
    std::fs::write(fixture.worktree.join("build/artifact"), "regenerable")?;
    std::fs::create_dir(fixture.worktree.join("saved"))?;
    std::fs::write(fixture.worktree.join("saved/keep"), "keep")?;
    let other = fixture.rig.project.join(".worktrees/foreign");
    let git = provenant_os::git::git_executable().ok_or("git")?;
    let output = fixture.rig.execute(
        &git,
        &[
            "worktree",
            "add",
            "-b",
            "foreign",
            other.to_str().ok_or("path")?,
        ],
        None,
    )?;
    assert_eq!(crate::worktree_tests::support::code(&output), 0);
    std::fs::create_dir(other.join("build"))?;
    std::fs::write(other.join("build/artifact"), "foreign")?;
    let cache_root = fixture.rig.home.join("Library/Caches/provenant-lane-cargo");
    // Use valid cache ownership markers, including an unrelated orphan.
    let cache = cache_root.join("a".repeat(64));
    let orphan = cache_root.join("b".repeat(64));
    let identity = std::fs::metadata(&fixture.worktree)?;
    for (path, owner) in [
        (&cache, fixture.worktree.clone()),
        (&orphan, fixture.rig.root.join("missing")),
    ] {
        std::fs::create_dir_all(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        let mut marker = b"provenant-lane-cargo-owner-v2\n".to_vec();
        marker.extend_from_slice(&identity.dev().to_le_bytes());
        marker.extend_from_slice(&identity.ino().to_le_bytes());
        marker.extend_from_slice(owner.as_os_str().as_encoded_bytes());
        std::fs::write(path.join(".provenant-worktree-owner"), marker)?;
        std::fs::write(path.join("artifact"), "cached")?;
    }
    let delivery = provenant_run::WriterDeliveryReceipt {
        extra: Default::default(),
        schema_version: 1,
        remote: "origin".into(),
        branch: "resumed-lane".into(),
        head: None,
        review_acceptance: None,
        state: provenant_run::WriterDeliveryState::NotAttempted {
            extra: Default::default(),
        },
        integration: None,
    };
    let collection = crate::worktrees::collection_lock(&fixture.roots)?;
    settle_owned_worktree(
        &fixture.settled,
        &fixture.roots,
        &FreeWorkers,
        Some(&delivery),
    )?;
    assert!(fixture.worktree.join("build/artifact").exists());
    assert!(cache.exists());
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let call = provenant_run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let busy = fixture
        .store
        .worktree_settlement(fixture.settled.attempt_id, &call)?
        .ok_or("missing busy settlement receipt")?;
    assert_eq!(busy["state"], "retained", "{busy}");
    assert_eq!(busy["reason"], crate::worktrees::COLLECTION_BUSY);
    drop(collection);
    let state = provenant_os::fs::Directory::open_anchor(
        &fixture
            .rig
            .state
            .join("worktrees")
            .join(fixture.settled.project_id.to_string())
            .join(provenant_confinement::worktrees::state_dir_name(
                &fixture.worktree,
            )),
    )?;
    let writer = state.lock(
        std::ffi::OsStr::new("writer.lock"),
        provenant_os::fs::LockMode::Exclusive,
        WaitBudget::nonblocking(SystemClock.mono_now()),
        &CancelToken::new(),
        &SystemClock,
    )?;
    settle_owned_worktree(
        &fixture.settled,
        &fixture.roots,
        &FreeWorkers,
        Some(&delivery),
    )?;
    assert!(fixture.worktree.join("build/artifact").exists());
    assert!(cache.exists());
    drop(writer);
    for _ in 0..2 {
        settle_owned_worktree(
            &fixture.settled,
            &fixture.roots,
            &FreeWorkers,
            Some(&delivery),
        )?;
        assert!(fixture.worktree.exists());
        assert!(!fixture.worktree.join("build/artifact").exists());
        assert!(!cache.exists(), "settled lane cache survived");
        assert_eq!(
            std::fs::read_to_string(fixture.worktree.join("saved/keep"))?,
            "keep"
        );
        assert_eq!(
            std::fs::read_to_string(other.join("build/artifact"))?,
            "foreign"
        );
        assert!(orphan.exists(), "unrelated cache collected");
    }
    Ok(())
}

#[test]
fn explicit_writer_refreshes_tracking_ref_while_collection_is_busy()
-> Result<(), Box<dyn std::error::Error>> {
    if host_test(
        "fabric::peer_tests::peer_graph_submission::explicit_writer_refreshes_tracking_ref_while_collection_is_busy",
    )? {
        return Ok(());
    }
    let fixture = SettlementFixture::resumed()?;
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let owner_path = worktrees::with_context(
        &fixture.roots,
        &fixture.rig.home,
        &fixture.rig.project,
        &clock,
        &cancel,
        |context| {
            let tree = provenant_confinement::verify_worktree(context.repo, &fixture.worktree)?;
            Ok(tree.repo().private_git_dir().join("provenant-owner.json"))
        },
    )?;
    std::fs::remove_file(owner_path)?;
    let git = provenant_os::git::git_executable().ok_or("git")?;
    let remote = fixture.rig.root.join("remote.git");
    for args in [
        vec!["init", "--bare", remote.to_str().ok_or("remote path")?],
        vec![
            "remote",
            "add",
            "origin",
            remote.to_str().ok_or("remote path")?,
        ],
        vec![
            "push",
            "origin",
            "refs/heads/resumed-lane:refs/heads/resumed-lane",
        ],
        vec!["update-ref", "-d", "refs/remotes/origin/resumed-lane"],
    ] {
        let output = fixture.rig.execute(&git, &args, None)?;
        assert_eq!(
            crate::worktree_tests::support::code(&output),
            0,
            "{}",
            String::from_utf8_lossy(&output.stderr.bytes)
        );
    }
    let collection = crate::worktrees::collection_lock(&fixture.roots)?;
    settle_owned_worktree(&fixture.settled, &fixture.roots, &FreeWorkers, None)?;
    let output = fixture.rig.execute(
        &git,
        &["rev-parse", "--verify", "refs/remotes/origin/resumed-lane"],
        None,
    )?;
    assert_eq!(crate::worktree_tests::support::code(&output), 0);
    drop(collection);
    Ok(())
}
