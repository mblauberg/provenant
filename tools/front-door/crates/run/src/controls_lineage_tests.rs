use super::*;
use provenant_confinement::worktrees::{self, WorktreeCreate, WorktreeOrigin, WorktreeOwner};
use provenant_core::Clock;
use provenant_os::fixture::run_git;
use std::path::Path;

/// Initialise `project` with one empty commit, through the scrubbed fixture Git.
fn init_base_repository(project: &Path) -> std::io::Result<()> {
    run_git(project, &["init", "-q"])?;
    run_git(project, &["commit", "--allow-empty", "-qm", "Base"])?;
    Ok(())
}

#[test]
// Spec: docs/specs/fabric.md#session-ready-steering
fn graph_writer_resume_uses_the_committed_terminal_head_and_original_source()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let mut execution = crate::tests::context(temp.path())?;
    let project = temp.path().join("checkout");
    let home = temp.path().join("home");
    let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
    inputs.state_dir = Some(execution.state_root.as_path().into());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    std::fs::create_dir_all(roots.state.as_path())?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        roots.state.as_path(),
        std::fs::Permissions::from_mode(0o700),
    )?;
    init_base_repository(&project)?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let (project_id, info) =
        worktrees::with_context(&roots, &home, &project, &clock, &cancel, |context| {
            let info = worktrees::create(
                context,
                WorktreeCreate {
                    branch: "steer-writer".into(),
                    base: Some("HEAD".into()),
                },
                &cancel,
            )?;
            Ok((context.binding.project_id(), info))
        })?;
    execution.project_id = project_id;
    let call = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let store = RunStore::open(&execution, &call)?;
    let writer = WriterRequest {
        extra: Default::default(),
        custody_project: Some(project_id),
        project: project.clone(),
        worktree: info.path.clone(),
    };
    let mut original = crate::tests::input();
    original.attempt_id = AttemptId::from_bytes([212; 16]);
    original.operation_id = "steer-committed-writer".parse()?;
    crate::tests::rekey(&mut original);
    let prior = original.attempt_id;
    let source = info.head;
    let source_text = source.to_string();
    original.request.task.writer = Some(writer.clone());
    original.request.task.base = Some(source_text.clone());
    original.request.task.head = Some(source_text.clone());
    original.request.task.source_head = None;
    original.request.task.context = Some(provenant_records::run::WorkContext {
        graph_id: Some("steer-writer-resume".into()),
        node_id: Some("writer".into()),
        attempt: Some(1),
        ..Default::default()
    });
    let route = crate::tests::route();
    store.create(&original, &call)?;
    let initial = crate::prepare_writer(&writer, &store.writer_registry, project_id)?;
    worktrees::save_owner(
        &initial.worktree,
        &WorktreeOwner {
            attempt_id: Some(prior),
            operation_id: Some(original.operation_id.clone()),
            branch: info.branch.clone().ok_or("writer branch missing")?,
            base: source_text.clone(),
            host: provenant_core::HostId::from_bytes([3; 16]),
            created_at: original.created_at,
            origin: WorktreeOrigin::Lane,
            label: None,
            reference: None,
        },
    )?;
    let parent_record = store.record(prior, &call)?;
    initial.snapshot(
        store.context.state_root.as_path(),
        &store.attempt_dir(prior, false)?,
        RunStore::scope(&parent_record),
        &provenant_records::policy::Policy::default(),
    )?;

    run_git(&info.path, &["config", "user.name", "Fixture"])?;
    run_git(
        &info.path,
        &["config", "user.email", "fixture@example.invalid"],
    )?;
    std::fs::write(info.path.join("before.txt"), "committed predecessor work")?;
    run_git(&info.path, &["add", "before.txt"])?;
    run_git(&info.path, &["commit", "-qm", "predecessor work"])?;
    let committed_head = String::from_utf8(run_git(&info.path, &["rev-parse", "HEAD"])?)?
        .trim()
        .to_owned();
    let predecessor = provenant_core::GitOid::parse(&committed_head)?;
    std::fs::write(info.path.join("pending.txt"), "retained uncommitted work")?;

    let starting = store.mark_starting(
        &store.record(prior, &call)?,
        &route,
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let starting = store.record_starting_launch(
        &starting,
        &route,
        None,
        &[],
        &Default::default(),
        Some(predecessor),
        0,
        &Default::default(),
        &call,
    )?;
    store.withdraw(
        prior,
        starting.state,
        Outcome::Cancelled,
        "fixture predecessor settled",
        clock.wall_now()?,
        &call,
    )?;
    let terminal = store.record(prior, &call)?;
    assert_eq!(terminal.head, Some(predecessor));

    let mut context = terminal
        .context
        .clone()
        .ok_or("writer graph context missing")?;
    context.attempt = Some(2);
    context.writer_predecessor = Some(prior);
    let finished_at = match terminal.state {
        AttemptState::Terminal { finished_at, .. } => finished_at,
        _ => return Err("writer predecessor was not terminal".into()),
    };
    let resume = Resume {
        extend: Vec::new(),
        prior,
        expected_generation: 0,
        operation_id: "steer-committed-writer-successor".parse()?,
        attempt_id: AttemptId::from_bytes([213; 16]),
        prompt: "Continue the same writer task".into(),
        created_at: finished_at,
    };
    let child = store.resume_graph(&resume, &context, &call)?;
    let child_task = store.request(child.attempt_id, &call)?.task;
    assert_eq!(
        child_task.source_head.as_deref(),
        Some(source_text.as_str())
    );
    assert_eq!(child_task.head.as_deref(), Some(committed_head.as_str()));
    assert_eq!(child_task.writer.as_ref(), Some(&writer));
    assert_eq!(child_task.route, original.request.task.route);
    assert_eq!(child_task.account, original.request.task.account);
    assert_eq!(
        std::fs::read_to_string(info.path.join("pending.txt"))?,
        "retained uncommitted work"
    );

    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    let resumed = crate::prepare_writer(
        child_task
            .writer
            .as_ref()
            .ok_or("successor writer missing")?,
        &store.writer_registry,
        project_id,
    )?;
    resumed.require_continuation_source(
        &git,
        source,
        predecessor,
        &provenant_records::policy::Policy::default(),
    )?;
    store.check_launch_writer_lineage(
        &store.record(child.attempt_id, &call)?,
        &child_task,
        &resumed.worktree,
        &call,
    )?;

    std::fs::write(
        info.path.join("foreign-drift.txt"),
        "newer after settlement",
    )?;
    run_git(&info.path, &["add", "foreign-drift.txt"])?;
    run_git(&info.path, &["commit", "-qm", "post-settlement drift"])?;
    let drifted = crate::prepare_writer(
        child_task
            .writer
            .as_ref()
            .ok_or("successor writer missing")?,
        &store.writer_registry,
        project_id,
    )?;
    assert!(matches!(
        drifted.require_continuation_source(&git, source, predecessor, &provenant_records::policy::Policy::default()),
        Err(RunError::WriterSourceMismatch { expected, .. }) if expected == predecessor
    ));
    Ok(())
}

#[test]
fn resume_preserves_explicit_worktrees_and_settled_lane_ownership()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let mut execution = crate::tests::context(temp.path())?;
    let project = temp.path().join("checkout");
    let home = temp.path().join("home");
    let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
    inputs.state_dir = Some(execution.state_root.as_path().into());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    std::fs::create_dir_all(roots.state.as_path())?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        roots.state.as_path(),
        std::fs::Permissions::from_mode(0o700),
    )?;
    init_base_repository(&project)?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    for (index, origin) in [
        None,
        Some(WorktreeOrigin::Adopted),
        Some(WorktreeOrigin::Lane),
    ]
    .into_iter()
    .enumerate()
    {
        let mut original = crate::tests::input();
        original.attempt_id = AttemptId::from_bytes([60 + index as u8; 16]);
        original.operation_id = format!("explicit-{index}").parse()?;
        original.attempt_id = AttemptId::from_bytes([70 + index as u8; 16]);
        crate::tests::rekey(&mut original);
        let prior = original.attempt_id;
        let (project_id, info) =
            worktrees::with_context(&roots, &home, &project, &clock, &cancel, |context| {
                let info = worktrees::create(
                    context,
                    WorktreeCreate {
                        branch: format!("explicit-{index}"),
                        base: None,
                    },
                    &cancel,
                )?;
                if let Some(origin) = origin {
                    let tree = provenant_confinement::verify_worktree(context.repo, &info.path)?;
                    worktrees::save_owner(
                        &tree,
                        &WorktreeOwner {
                            attempt_id: (origin == WorktreeOrigin::Lane)
                                .then(|| AttemptId::from_bytes([120; 16])),
                            operation_id: (origin == WorktreeOrigin::Lane)
                                .then(|| "previous-owner".parse().unwrap()),

                            branch: info
                                .branch
                                .clone()
                                .ok_or(worktrees::WorktreeError::NotFound)?,
                            base: info.head.to_string(),
                            host: provenant_core::HostId::from_bytes([3; 16]),
                            created_at: original.created_at,
                            origin,
                            label: None,
                            reference: None,
                        },
                    )?;
                }
                Ok((context.binding.project_id(), info))
            })?;
        execution.project_id = project_id;
        let call = Request {
            clock: &clock,
            cancel: &cancel,
        };
        let store = RunStore::open(&execution, &call)?;
        let writer = WriterRequest {
            extra: Default::default(),
            custody_project: Some(project_id),
            project: project.clone(),
            worktree: info.path.clone(),
        };
        original.request.task.writer = Some(writer.clone());
        original.request.task.head = Some(info.head.to_string());
        original.request.task.context = Some(provenant_records::run::WorkContext {
            graph_id: Some("graph-writer-lineage".into()),
            node_id: Some(format!("writer-{index}")),
            attempt: Some(1),
            ..Default::default()
        });
        store.create(&original, &call)?;
        let record = store.record(prior, &call)?;
        crate::prepare_writer(&writer, &store.writer_registry, project_id)?.snapshot(
            store.context.state_root.as_path(),
            &store.attempt_dir(prior, false)?,
            RunStore::scope(&record),
            &provenant_records::policy::Policy::default(),
        )?;
        let mut prior_terminalized = false;
        if origin == Some(WorktreeOrigin::Lane) {
            let launch = crate::prepare_writer(&writer, &store.writer_registry, project_id)?;
            assert!(matches!(
                store.check_writer_lineage(&record, &launch.worktree, &call),
                Err(RunError::NotFound)
            ));
            let mut owner_context = execution.clone();
            owner_context.project_id = provenant_core::ProjectId::from_bytes([121; 16]);
            let owner_store = RunStore::open(&owner_context, &call)?;
            let mut owner = crate::tests::input();
            owner.attempt_id = AttemptId::from_bytes([122; 16]);
            owner.operation_id = "previous-owner".parse()?;
            owner.attempt_id = AttemptId::from_bytes([120; 16]);
            crate::tests::rekey(&mut owner);
            owner_store.create(&owner, &call)?;
            assert!(matches!(
                store.check_writer_lineage(&record, &launch.worktree, &call),
                Err(RunError::Nonterminal)
            ));
            store.withdraw(
                prior,
                AttemptState::Queued {
                    extra: Default::default(),
                },
                Outcome::Cancelled,
                "fixture prior terminal while retained owner remains active",
                clock.wall_now()?,
                &call,
            )?;
            prior_terminalized = true;
            let terminal = store.record(prior, &call)?;
            let mut queue_task = original.request.task.clone();
            let queue_context = queue_task.context.as_mut().ok_or("queue graph context")?;
            queue_context.graph_id = Some("active-retained-writer-queue".into());
            queue_context.node_id = Some("repair".into());
            queue_context.attempt = Some(1);
            queue_context.writer_predecessor = Some(prior);
            queue_context.delivery_queue_writer_adoption = true;
            queue_task.source_head = queue_task
                .source_head
                .as_ref()
                .or(queue_task.head.as_ref())
                .cloned();
            queue_task.head = terminal
                .head
                .map(|head| head.to_string())
                .or_else(|| queue_task.head.clone());
            assert!(matches!(
                store.check_launch_writer_lineage(&terminal, &queue_task, &launch.worktree, &call),
                Err(RunError::Nonterminal)
            ));
            let owner_id = AttemptId::from_bytes([120; 16]);
            owner_store.withdraw(
                owner_id,
                AttemptState::Queued {
                    extra: Default::default(),
                },
                Outcome::Cancelled,
                "fixture",
                clock.wall_now()?,
                &call,
            )?;
            assert!(store.writer_owner_terminal(owner_id, &call)?);
        }
        if origin.is_none() {
            let launch = crate::prepare_writer(&writer, &store.writer_registry, project_id)?;
            worktrees::save_owner(
                &launch.worktree,
                &WorktreeOwner {
                    attempt_id: None,
                    operation_id: None,
                    branch: format!("explicit-{index}"),
                    base: info.head.to_string(),
                    host: provenant_core::HostId::from_bytes([3; 16]),
                    created_at: original.created_at,
                    origin: WorktreeOrigin::Adopted,
                    label: None,
                    reference: None,
                },
            )?;
            assert!(matches!(
                crate::writer::seal_writer_ownership(
                    &store.attempt_dir(prior, false)?,
                    &launch.worktree
                ),
                Err(RunError::Conflict)
            ));
            std::fs::remove_file(
                launch
                    .worktree
                    .repo()
                    .private_git_dir()
                    .join("provenant-owner.json"),
            )?;
        }
        if origin == Some(WorktreeOrigin::Lane) {
            let attempt = store.attempt_dir(prior, false)?;
            let snapshot = attempt.current_path()?.join("writer-owned.json");
            let unobserved = crate::db::bytes(&crate::writer::WriterOwnership {
                extra: Default::default(),
                schema_version: 1,
                lane_owned: true,
                owner: None,
            })?;
            std::fs::write(&snapshot, &unobserved)?;
            let launch = crate::prepare_writer(&writer, &store.writer_registry, project_id)?;
            assert!(matches!(
                crate::writer::seal_writer_ownership(&attempt, &launch.worktree),
                Err(RunError::Conflict)
            ));
            assert_eq!(std::fs::read(&snapshot)?, unobserved);
            assert!(matches!(
                store.check_writer_lineage(&record, &launch.worktree, &call),
                Err(RunError::WriterLineage(
                    "continuation worktree belongs to another lane"
                ))
            ));
            std::fs::remove_file(snapshot)?;
            crate::writer::seal_writer_ownership(&attempt, &launch.worktree)?;
        }
        std::fs::write(info.path.join("pending.txt"), "keep these edits")?;
        if !prior_terminalized {
            store.withdraw(
                prior,
                AttemptState::Queued {
                    extra: Default::default(),
                },
                Outcome::Cancelled,
                "fixture",
                clock.wall_now()?,
                &call,
            )?;
        }
        let next = Resume {
            extend: Vec::new(),
            prior,
            expected_generation: 0,
            operation_id: format!("explicit-resume-{index}").parse()?,
            attempt_id: AttemptId::from_bytes([90 + index as u8; 16]),
            prompt: "Continue".into(),
            created_at: clock.wall_now()?,
        };
        let resumed = store.resume_fresh(&next, "no provider session", &call)?;
        // A graph retry has a fresh run and no run-policy retry_of. Its
        // sealed predecessor still protects it against ownership changes
        // between admission and the launch-custody check below.
        let mut graph = crate::tests::input();
        graph.attempt_id = AttemptId::from_bytes([180 + index as u8; 16]);
        graph.operation_id = format!("graph-retry-{index}").parse()?;
        graph.attempt_id = AttemptId::from_bytes([190 + index as u8; 16]);
        graph.request.task = original.request.task.clone();
        let graph_task = &mut graph.request.task;
        graph_task.context.as_mut().ok_or("graph context")?.attempt = Some(3);
        graph_task.source_head = graph_task.head.clone();
        graph_task
            .context
            .as_mut()
            .ok_or("graph context")?
            .writer_predecessor = Some(prior);
        store.create(&graph, &call)?;
        let graph_record = store.record(graph.attempt_id, &call)?;
        assert!(graph_record.retry_of.is_none());
        let tree = crate::prepare_writer(&writer, &store.writer_registry, project_id)?;
        store.check_launch_writer_lineage(
            &graph_record,
            &graph.request.task,
            &tree.worktree,
            &call,
        )?;
        let mut other_node = graph.request.task.clone();
        other_node.context.as_mut().ok_or("graph context")?.node_id = Some("unrelated".into());
        assert!(matches!(
            store.check_launch_writer_lineage(&graph_record, &other_node, &tree.worktree, &call),
            Err(RunError::WriterLineage("graph predecessor context"))
        ));
        for field in ["writer", "source", "head"] {
            let mut mismatch = graph.request.task.clone();
            match field {
                "writer" => {
                    mismatch.writer.as_mut().ok_or("writer")?.worktree =
                        project.join(".worktrees/other");
                }
                "source" => mismatch.source_head = Some("0".repeat(40)),
                "head" => mismatch.head = Some("1".repeat(40)),
                _ => unreachable!(),
            }
            assert!(
                matches!(
                    store.check_launch_writer_lineage(
                        &graph_record,
                        &mismatch,
                        &tree.worktree,
                        &call
                    ),
                    Err(RunError::WriterLineage(
                        "graph predecessor writer or source"
                    ))
                ),
                "graph predecessor accepted mismatched {field}"
            );
        }
        let prior_task = store.request(prior, &call)?.task;
        let mut queue_graph = crate::tests::input();
        queue_graph.attempt_id = AttemptId::from_bytes([230 + index as u8; 16]);
        queue_graph.operation_id = format!("queue-adoption-{index}").parse()?;
        queue_graph.request.task = prior_task.clone();
        let queue_context = queue_graph
            .request
            .task
            .context
            .as_mut()
            .ok_or("queue graph context")?;
        queue_context.graph_id = Some(format!("delivery-queue-{index}"));
        queue_context.node_id = Some(format!("repair-{index}"));
        queue_context.attempt = Some(1);
        queue_context.writer_predecessor = Some(prior);
        queue_context.delivery_queue_writer_adoption = true;
        let previous_record = store.record(prior, &call)?;
        queue_graph.request.task.source_head = prior_task
            .source_head
            .as_ref()
            .or(prior_task.head.as_ref())
            .cloned();
        queue_graph.request.task.head = previous_record
            .head
            .map(|head| head.to_string())
            .or_else(|| prior_task.head.clone());
        store.create(&queue_graph, &call)?;
        let queue_record = store.record(queue_graph.attempt_id, &call)?;
        let queue_tree = crate::prepare_writer(&writer, &store.writer_registry, project_id)?;
        store.check_launch_writer_lineage(
            &queue_record,
            &queue_graph.request.task,
            &queue_tree.worktree,
            &call,
        )?;
        let mut unmarked_queue_task = queue_graph.request.task.clone();
        unmarked_queue_task
            .context
            .as_mut()
            .ok_or("queue graph context")?
            .delivery_queue_writer_adoption = false;
        assert!(matches!(
            store.check_launch_writer_lineage(
                &queue_record,
                &unmarked_queue_task,
                &queue_tree.worktree,
                &call
            ),
            Err(RunError::WriterLineage("graph predecessor context"))
        ));
        assert_eq!(
            store
                .request(resumed.attempt_id, &call)?
                .task
                .writer
                .as_ref(),
            Some(&writer)
        );
        assert_eq!(
            std::fs::read_to_string(info.path.join("pending.txt"))?,
            "keep these edits"
        );
        let tree = crate::prepare_writer(&writer, &store.writer_registry, project_id)?;
        store.check_writer_lineage(
            &store.record(next.attempt_id, &call)?,
            &tree.worktree,
            &call,
        )?;
        worktrees::save_owner(
            &tree.worktree,
            &WorktreeOwner {
                attempt_id: Some(AttemptId::from_bytes([100; 16])),
                operation_id: Some("foreign".parse()?),
                branch: format!("explicit-{index}"),
                base: info.head.to_string(),
                host: provenant_core::HostId::from_bytes([3; 16]),
                created_at: original.created_at,
                origin: WorktreeOrigin::Lane,
                label: None,
                reference: None,
            },
        )?;
        assert!(matches!(
            store.check_launch_writer_lineage(
                &graph_record,
                &graph.request.task,
                &tree.worktree,
                &call
            ),
            Err(RunError::WriterLineage(
                "continuation worktree belongs to another lane"
            ))
        ));
        assert!(matches!(
            store.check_continuation_writer(next.attempt_id, &call),
            Err(RunError::WriterLineage(
                "continuation worktree belongs to another lane"
            ))
        ));
    }
    Ok(())
}

#[test]
fn resume_refuses_a_worktree_reused_by_another_lane()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let mut execution = crate::tests::context(temp.path())?;
    let project = temp.path().join("checkout");
    let home = temp.path().join("home");
    let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
    inputs.state_dir = Some(execution.state_root.as_path().into());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    std::fs::create_dir_all(roots.state.as_path())?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        roots.state.as_path(),
        std::fs::Permissions::from_mode(0o700),
    )?;
    init_base_repository(&project)?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let mut original = crate::tests::input();
    original.request.task.deliver = Some(WriterDeliveryRequest {
        extra: Default::default(),
        mode: DeliveryMode::Pr,
        remote: "origin".into(),
        branch: "old-lane".into(),
        open_pr: true,
        fast_forward_only: false,
    });
    let prior = original.attempt_id;
    let (project_id, info) =
        worktrees::with_context(&roots, &home, &project, &clock, &cancel, |context| {
            let info = worktrees::create(
                context,
                WorktreeCreate {
                    branch: "lane".into(),
                    base: None,
                },
                &cancel,
            )?;
            let tree = provenant_confinement::verify_worktree(context.repo, &info.path)?;
            worktrees::save_owner(
                &tree,
                &WorktreeOwner {
                    attempt_id: Some(prior),
                    operation_id: Some(original.operation_id.clone()),
                    branch: "lane".into(),
                    base: info.head.to_string(),
                    host: provenant_core::HostId::from_bytes([3; 16]),
                    created_at: original.created_at,
                    origin: WorktreeOrigin::Lane,
                    label: None,
                    reference: None,
                },
            )?;
            Ok((context.binding.project_id(), info))
        })?;
    execution.project_id = project_id;
    let call = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let store = RunStore::open(&execution, &call)?;
    original.request.task.writer = Some(WriterRequest {
        extra: Default::default(),
        custody_project: Some(project_id),
        project: project.clone(),
        worktree: info.path.clone(),
    });
    store.create(&original, &call)?;
    let launch = crate::prepare_writer(
        original.request.task.writer.as_ref().unwrap(),
        &store.writer_registry,
        project_id,
    )?;
    let mut probe = crate::tests::input();
    probe.request.task.writer = original.request.task.writer.clone();
    probe.attempt_id = AttemptId::from_bytes([70; 16]);
    probe.operation_id = "lineage-probe".parse()?;
    probe.attempt_id = AttemptId::from_bytes([71; 16]);
    crate::tests::rekey(&mut probe);
    store.create(&probe, &call)?;
    let continuation = store.record(probe.attempt_id, &call)?;
    launch.snapshot(
        store.context.state_root.as_path(),
        &store.attempt_dir(continuation.attempt_id, false)?,
        RunStore::scope(&continuation),
        &provenant_records::policy::Policy::default(),
    )?;
    let queued = store.record(prior, &call)?;
    assert!(matches!(
        store.check_writer_lineage(&continuation, &launch.worktree, &call),
        Err(RunError::Nonterminal)
    ));
    let mut running = queued.clone();
    running.state = AttemptState::Running {
        extra: Default::default(),
        child_wait: None,
    };
    store.db.execute(
        "UPDATE attempts SET record=?1 WHERE id=?2",
        rusqlite::params![crate::db::bytes(&running)?, prior.to_string()],
    )?;
    assert!(matches!(
        store.check_writer_lineage(&continuation, &launch.worktree, &call),
        Err(RunError::Nonterminal)
    ));
    store.db.execute(
        "UPDATE attempts SET record=?1 WHERE id=?2",
        rusqlite::params![crate::db::bytes(&queued)?, prior.to_string()],
    )?;
    store.withdraw(
        prior,
        AttemptState::Queued {
            extra: Default::default(),
        },
        Outcome::Cancelled,
        "fixture",
        clock.wall_now()?,
        &call,
    )?;
    assert!(
        store
            .check_writer_lineage(&continuation, &launch.worktree, &call)?
            .is_some()
    );
    let launch = crate::prepare_writer(
        original.request.task.writer.as_ref().unwrap(),
        &store.writer_registry,
        project_id,
    )?;
    let record = store.record(prior, &call)?;
    launch.snapshot(
        store.context.state_root.as_path(),
        &store.attempt_dir(prior, false)?,
        RunStore::scope(&record),
        &provenant_records::policy::Policy::default(),
    )?;
    let owner = worktrees::load_owner(&launch.worktree)?.ok_or("owner")?;
    std::fs::remove_file(
        launch
            .worktree
            .repo()
            .private_git_dir()
            .join("provenant-owner.json"),
    )?;
    assert!(matches!(
        store.check_continuation_writer(prior, &call),
        Err(RunError::WriterLineage(
            "continuation worktree has no lane owner"
        ))
    ));
    worktrees::save_owner(
        &launch.worktree,
        &WorktreeOwner {
            origin: WorktreeOrigin::Adopted,
            ..owner.clone()
        },
    )?;
    assert!(matches!(
        store.check_continuation_writer(prior, &call),
        Err(RunError::WriterLineage(
            "continuation worktree has no lane owner"
        ))
    ));
    worktrees::save_owner(&launch.worktree, &owner)?;
    assert!(store.check_continuation_writer(prior, &call)?.is_some());
    let first = Resume {
        extend: Vec::new(),
        prior,
        expected_generation: 0,
        operation_id: "queued-resume".parse()?,
        attempt_id: AttemptId::from_bytes([31; 16]),
        prompt: "Continue".into(),
        created_at: clock.wall_now()?,
    };
    store.resume_fresh(&first, "fixture", &call)?;
    store.withdraw(
        first.attempt_id,
        AttemptState::Queued {
            extra: Default::default(),
        },
        Outcome::Cancelled,
        "fixture",
        clock.wall_now()?,
        &call,
    )?;
    // Simulate an external replacement. This fixture tests lineage,
    // rather than the host retirement policy used to remove a checkout.
    let path = info.path.to_str().ok_or("fixture worktree path encoding")?;
    run_git(&project, &["worktree", "remove", "--", path])?;
    worktrees::with_context(&roots, &home, &project, &clock, &cancel, |context| {
        let replacement = worktrees::create(
            context,
            WorktreeCreate {
                branch: "lane".into(),
                base: None,
            },
            &cancel,
        )?;
        assert_eq!(replacement.path, info.path);
        let tree = provenant_confinement::verify_worktree(context.repo, &replacement.path)?;
        worktrees::save_owner(
            &tree,
            &WorktreeOwner {
                attempt_id: Some(AttemptId::from_bytes([40; 16])),
                operation_id: Some("foreign".parse().unwrap()),
                branch: "lane".into(),
                base: replacement.head.to_string(),
                host: provenant_core::HostId::from_bytes([3; 16]),
                created_at: original.created_at,
                origin: WorktreeOrigin::Lane,
                label: None,
                reference: None,
            },
        )?;
        let queued = store.record(first.attempt_id, &call).unwrap();
        assert!(matches!(
            store.check_writer_lineage(&queued, &tree, &call),
            Err(RunError::WriterLineage(
                "continuation worktree belongs to another lane"
            ))
        ));
        Ok(())
    })?;
    let next = Resume {
        extend: Vec::new(),
        prior: first.attempt_id,
        operation_id: "new-resume".parse()?,
        attempt_id: AttemptId::from_bytes([51; 16]),
        ..first
    };
    assert!(matches!(
        store.resume(&next, &call),
        Err(RunError::WriterLineage(
            "continuation worktree belongs to another lane"
        ))
    ));
    assert!(matches!(
        store.resume_fresh(&next, "fixture", &call),
        Err(RunError::WriterLineage(
            "continuation worktree belongs to another lane"
        ))
    ));
    let replacement =
        worktrees::with_context(&roots, &home, &project, &clock, &cancel, |context| {
            let replacement = worktrees::create(
                context,
                WorktreeCreate {
                    branch: "replacement".into(),
                    base: Some(info.head.to_string()),
                },
                &cancel,
            )?;
            let tree = provenant_confinement::verify_worktree(context.repo, &replacement.path)?;
            worktrees::save_owner(
                &tree,
                &WorktreeOwner {
                    attempt_id: Some(next.attempt_id),
                    operation_id: Some(next.operation_id.clone()),
                    branch: "replacement".into(),
                    base: replacement.head.to_string(),
                    host: provenant_core::HostId::from_bytes([3; 16]),
                    created_at: next.created_at,
                    origin: WorktreeOrigin::Lane,
                    label: None,
                    reference: None,
                },
            )?;
            Ok(replacement)
        })?;
    let writer = WriterRequest {
        extra: Default::default(),
        custody_project: Some(project_id),
        project: project.clone(),
        worktree: replacement.path,
    };
    let accepted = store.resume_fresh_in(&next, "original worktree reassigned", &writer, &call)?;
    assert_eq!(
        store
            .request(accepted.attempt_id, &call)?
            .task
            .writer
            .as_ref(),
        Some(&writer)
    );
    assert_eq!(
        store
            .request(accepted.attempt_id, &call)?
            .task
            .deliver
            .as_ref()
            .unwrap()
            .branch,
        "replacement"
    );
    assert!(info.path.exists(), "foreign lane must retain its checkout");
    Ok(())
}
