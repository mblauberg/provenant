use super::*;
use provenant_confinement::worktrees::{self, WorktreeCreate, WorktreeOrigin, WorktreeOwner};
use provenant_core::Clock;
use provenant_os::fixture::run_git;
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;

fn create_private_state_root(path: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(path)
}

/// Initialise `project` with one empty commit, through the scrubbed fixture Git.
fn init_base_repository(project: &Path) -> std::io::Result<()> {
    run_git(project, &["init", "-q"])?;
    run_git(project, &["commit", "--allow-empty", "-qm", "Base"])?;
    Ok(())
}

#[test]
// Spec: docs/specs/fabric.md#session-ready-steering
fn stopped_lane_branch_continues_with_handover_warning()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let mut execution = crate::tests::context(temp.path())?;
    let project = temp.path().join("checkout");
    let home = temp.path().join("home");
    let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
    inputs.state_dir = Some(execution.state_root.as_path().into());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    create_private_state_root(roots.state.as_path())?;
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
    let (launched, warnings) = provenant_records::warning::capture(|| {
        store.check_launch_writer_lineage(
            &store.record(child.attempt_id, &call)?,
            &child_task,
            &resumed.worktree,
            &call,
        )
    });
    launched?;
    let handover_warning = warnings
        .iter()
        .find(|w| w.code.as_ref() == provenant_records::policy::WRITER_HANDOVER)
        .ok_or("no WRITER_HANDOVER warning")?;
    assert!(
        handover_warning.message.contains(&prior.to_string()),
        "handover did not name the prior lane: {}",
        handover_warning.message
    );
    assert!(
        handover_warning.message.contains("terminal"),
        "handover did not name the terminal state"
    );
    let fact = crate::writer::read_handover(&store.attempt_dir(child.attempt_id, false)?)?
        .ok_or("no handover fact sealed for the successor")?;
    assert_eq!(fact.from, prior);
    assert_eq!(fact.to, child.attempt_id);
    assert_eq!(fact.head, Some(predecessor));

    // A resumed graph attempt has retry_of as well as a graph predecessor.
    // It must still verify the graph's sealed source rather than taking the
    // manual-resume owner-only shortcut.
    let mut forged = child_task.clone();
    forged.source_head = Some(predecessor.to_string());
    assert!(matches!(
        store.check_launch_writer_lineage(
            &store.record(child.attempt_id, &call)?,
            &forged,
            &resumed.worktree,
            &call,
        ),
        Err(RunError::Validation(_))
    ));

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
    let (checked, warnings) = provenant_records::warning::capture(|| {
        drifted.require_continuation_source(
            &git,
            source,
            predecessor,
            &provenant_records::policy::Policy::default(),
        )
    });
    checked?;
    assert!(
        warnings
            .iter()
            .any(|w| w.code.as_ref() == provenant_records::policy::WRITER_HEAD_MOVED),
        "a moved predecessor head must re-pin with WRITER_HEAD_MOVED"
    );
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
    create_private_state_root(roots.state.as_path())?;
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
            // A foreign lane owner is advisory: ownership alone never refuses.
            assert!(
                store
                    .check_writer_lineage(&record, &launch.worktree, &call)?
                    .is_some()
            );
            let mut owner_context = execution.clone();
            owner_context.project_id = provenant_core::ProjectId::from_bytes([121; 16]);
            let owner_store = RunStore::open(&owner_context, &call)?;
            let mut owner = crate::tests::input();
            owner.attempt_id = AttemptId::from_bytes([122; 16]);
            owner.operation_id = "previous-owner".parse()?;
            owner.attempt_id = AttemptId::from_bytes([120; 16]);
            crate::tests::rekey(&mut owner);
            owner_store.create(&owner, &call)?;
            // A live owner is admitted under the shared-writer rule.
            assert!(
                store
                    .check_writer_lineage(&record, &launch.worktree, &call)?
                    .is_some()
            );
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
            queue_context.graph_template_launch = true;
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
            store.check_launch_writer_lineage(&terminal, &queue_task, &launch.worktree, &call)?;
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
            // The sealed ownership record is advisory now; the worktree owner
            // and the holder's attempt state decide admission.
            assert!(
                store
                    .check_writer_lineage(&record, &launch.worktree, &call)?
                    .is_some()
            );
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
        // Node identity is not a boundary: any lane may continue a lapsed lease.
        store.check_launch_writer_lineage(&graph_record, &other_node, &tree.worktree, &call)?;
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
                    Err(RunError::Validation("graph predecessor writer or source"))
                ),
                "graph predecessor accepted mismatched {field}"
            );
        }
        // A new graph can adopt the retained writer worktree of a finished
        // attempt: its own source and head pins describe the adoptive graph,
        // so they must not be compared with the earlier graph's sealed pins.
        // Writer identity, terminal custody and the lane owner still match
        // (#3987).
        if origin == Some(WorktreeOrigin::Lane) {
            let mut adopted = graph.request.task.clone();
            let context = adopted.context.as_mut().ok_or("graph context")?;
            context.graph_id = Some("adopting-graph".into());
            context.node_id = Some("adopting-node".into());
            adopted.source_head = Some("2".repeat(40));
            adopted.head = Some("3".repeat(40));
            store.check_launch_writer_lineage(&graph_record, &adopted, &tree.worktree, &call)?;
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
        queue_context.graph_template_launch = true;
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
        // Cross-graph adoption is also valid without the queue marker (#3987).
        store.check_launch_writer_lineage(
            &queue_record,
            &unmarked_queue_task,
            &queue_tree.worktree,
            &call,
        )?;
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
        // A foreign lane owner is advisory: it does not refuse a continuation.
        store.check_launch_writer_lineage(
            &graph_record,
            &graph.request.task,
            &tree.worktree,
            &call,
        )?;
        assert!(
            store
                .check_continuation_writer(next.attempt_id, &call)?
                .is_some()
        );
    }
    Ok(())
}

#[test]
fn retry_and_resume_on_lapsed_lease_need_no_flag()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let mut execution = crate::tests::context(temp.path())?;
    let project = temp.path().join("checkout");
    let home = temp.path().join("home");
    let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
    inputs.state_dir = Some(execution.state_root.as_path().into());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    create_private_state_root(roots.state.as_path())?;
    init_base_repository(&project)?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let mut original = crate::tests::input();
    original.request.task.deliver = Some(WriterDeliveryRequest {
        extra: Default::default(),
        mode: DeliveryMode::Pr,
        remote: provenant_records::run::Destination::from_name("origin").expect("fixture remote"),
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
    assert!(
        store
            .check_writer_lineage(&continuation, &launch.worktree, &call)?
            .is_some()
    );
    let mut running = queued.clone();
    running.state = AttemptState::Running {
        extra: Default::default(),
        child_wait: None,
    };
    store.db.execute(
        "UPDATE attempts SET record=?1 WHERE id=?2",
        rusqlite::params![crate::db::bytes(&running)?, prior.to_string()],
    )?;
    assert!(
        store
            .check_writer_lineage(&continuation, &launch.worktree, &call)?
            .is_some()
    );
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
    // No owner means no lease: the tree stays usable as an explicit tree.
    assert!(store.check_continuation_writer(prior, &call)?.is_none());
    worktrees::save_owner(
        &launch.worktree,
        &WorktreeOwner {
            origin: WorktreeOrigin::Adopted,
            ..owner.clone()
        },
    )?;
    assert!(store.check_continuation_writer(prior, &call)?.is_some());
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
        // Another lane's lease on the same path is advisory: a resume or retry
        // proceeds under the shared-writer rule, with no special flag.
        assert!(store.check_writer_lineage(&queued, &tree, &call)?.is_some());
        Ok(())
    })?;
    let next = Resume {
        extend: Vec::new(),
        prior: first.attempt_id,
        operation_id: "new-resume".parse()?,
        attempt_id: AttemptId::from_bytes([51; 16]),
        ..first
    };
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

#[test]
fn handover_keeps_and_reports_uncommitted_files()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let mut execution = crate::tests::context(temp.path())?;
    let project = temp.path().join("checkout");
    let home = temp.path().join("home");
    let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
    inputs.state_dir = Some(execution.state_root.as_path().into());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    create_private_state_root(roots.state.as_path())?;
    init_base_repository(&project)?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let (project_id, info) =
        worktrees::with_context(&roots, &home, &project, &clock, &cancel, |context| {
            let info = worktrees::create(
                context,
                WorktreeCreate {
                    branch: "handover".into(),
                    base: None,
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
    let mut prior = crate::tests::input();
    prior.attempt_id = AttemptId::from_bytes([210; 16]);
    prior.operation_id = "handover-prior".parse()?;
    crate::tests::rekey(&mut prior);
    let prior_id = prior.attempt_id;
    prior.request.task.writer = Some(writer.clone());
    prior.request.task.head = Some(info.head.to_string());
    store.create(&prior, &call)?;
    let launch = crate::prepare_writer(&writer, &store.writer_registry, project_id)?;
    worktrees::save_owner(
        &launch.worktree,
        &WorktreeOwner {
            attempt_id: Some(prior_id),
            operation_id: Some(prior.operation_id.clone()),
            branch: info.branch.clone().ok_or("writer branch missing")?,
            base: info.head.to_string(),
            host: provenant_core::HostId::from_bytes([3; 16]),
            created_at: prior.created_at,
            origin: WorktreeOrigin::Lane,
            label: None,
            reference: None,
        },
    )?;
    launch.snapshot(
        store.context.state_root.as_path(),
        &store.attempt_dir(prior_id, false)?,
        RunStore::scope(&store.record(prior_id, &call)?),
        &provenant_records::policy::Policy::default(),
    )?;
    // Uncommitted predecessor work must survive the handover untouched.
    std::fs::write(info.path.join("retained.txt"), "keep me")?;
    let route = crate::tests::route();
    let starting = store.mark_starting(
        &store.record(prior_id, &call)?,
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
        None,
        0,
        &Default::default(),
        &call,
    )?;
    store.withdraw(
        prior_id,
        starting.state,
        Outcome::Cancelled,
        "fixture",
        clock.wall_now()?,
        &call,
    )?;
    let mut successor = crate::tests::input();
    successor.attempt_id = AttemptId::from_bytes([211; 16]);
    successor.operation_id = "handover-successor".parse()?;
    successor.request.parent = Some(prior_id);
    crate::tests::rekey(&mut successor);
    successor.request.task.writer = Some(writer.clone());
    store.create(&successor, &call)?;
    let record = store.record(successor.attempt_id, &call)?;
    let (checked, warnings) = provenant_records::warning::capture(|| {
        store.check_writer_lineage(&record, &launch.worktree, &call)
    });
    checked?;
    let notice = warnings
        .iter()
        .find(|w| w.code.as_ref() == provenant_records::policy::WRITER_HANDOVER)
        .ok_or("no WRITER_HANDOVER warning")?;
    assert!(
        notice.message.contains("retained.txt"),
        "handover did not list the uncommitted file: {}",
        notice.message
    );
    let fact = crate::writer::read_handover(&store.attempt_dir(successor.attempt_id, false)?)?
        .ok_or("no handover fact")?;
    assert_eq!(fact.uncommitted, ["retained.txt"]);
    assert_eq!(
        std::fs::read_to_string(info.path.join("retained.txt"))?,
        "keep me"
    );
    Ok(())
}

#[test]
fn no_ownership_refusal_strings_remain() {
    for source in [include_str!("controls.rs"), include_str!("writer.rs")] {
        for banned in [
            "belongs to another lane",
            "has no lane owner",
            "continuation owner attempt missing",
        ] {
            assert!(
                !source.contains(banned),
                "ownership refusal string {banned:?} still present"
            );
        }
    }
    assert!(
        !include_str!("controls.rs").contains("WriterLineage"),
        "the deleted RunError::WriterLineage is still referenced"
    );
}
