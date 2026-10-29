//! Dispatch, admission, holder and control behaviour at Run's owner boundary.
//! Fake providers and resources stand in for adapters and the host.
use super::tests::{call, clock, context, exit, input, route};
use super::*;
use provenant_confinement::permissions::{PathGrant, PathScope};
use provenant_core::{AttemptId, CancelToken, Clock, MonoTime, RunId, TaskId, TestClock, WallTime};
use provenant_os::{exec::Termination, fs::Directory, sys::resources::MemorySample};
use provenant_records::CanonicalV1;
use provenant_supervisor::custody::ExitObservation;
use std::{
    cell::{Cell, RefCell},
    os::unix::fs::DirBuilderExt,
};
type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

#[test]
fn terminal_dependency_waits_for_custody_release() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let input = input();
    store.create(&input, &call)?;
    let mut record = store.record(input.attempts[0].attempt_id, &call)?;
    record.generation = 1;
    record.state = AttemptState::Terminal {
        outcome: Outcome::Succeeded,
        exit_code: Some(0),
        finished_at: record.queued_at,
        reason: None,
    };
    record.provider_launched = Some(false);
    assert!(
        store.custody_released(&record, &call)?,
        "a preflight refusal needs no custody lock"
    );
    record.provider_launched = Some(true);
    let directory = store.attempt_dir(record.run_id, record.attempt_id, false)?;
    assert!(
        store.custody_released(&record, &call).is_err(),
        "missing custody is not release evidence"
    );
    let custody = directory.try_lock(
        std::ffi::OsStr::new("custody.lock"),
        provenant_os::fs::LockMode::Exclusive,
    )?;
    assert!(!store.custody_released(&record, &call)?);
    drop(custody);
    assert!(store.custody_released(&record, &call)?);
    let worktrees = store
        .writer_registry
        .join("worktrees")
        .join(record.project_id.to_string());
    let broken = worktrees.join("broken");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&broken)?;
    std::fs::write(broken.join("reservation.json"), "{")?;
    assert_eq!(store.writer_fence_listing()?.issues.len(), 1);
    assert!(
        store.custody_released(&record, &call)?,
        "another worktree's unreadable reservation is not this attempt's"
    );
    std::fs::remove_dir_all(&broken)?;
    let writer = worktrees.join("retained");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&writer)?;
    let lease = provenant_supervisor::custody::acquire_reader(
        Directory::open_anchor(&writer)?,
        RunStore::scope(&record),
        call.budget,
        call.cancel,
        call.clock,
    )?;
    drop(lease);
    assert!(
        !store.custody_released(&record, &call)?,
        "a free lock does not clear a retained writer fence"
    );
    record.state = AttemptState::Running;
    assert!(!store.custody_released(&record, &call)?);
    Ok(())
}

#[test]
fn unreadable_reservation_in_the_writers_directory_holds_custody() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let mut input = input();
    let worktree = temp.path().join("lane");
    input.request.tasks[0].writer = Some(WriterRequest {
        project: temp.path().to_path_buf(),
        worktree: worktree.clone(),
    });
    store.create(&input, &call)?;
    let mut record = store.record(input.attempts[0].attempt_id, &call)?;
    record.generation = 1;
    record.state = AttemptState::Terminal {
        outcome: Outcome::Succeeded,
        exit_code: Some(0),
        finished_at: record.queued_at,
        reason: None,
    };
    record.provider_launched = Some(false);
    let worktrees = store
        .writer_registry
        .join("worktrees")
        .join(record.project_id.to_string());
    let unreadable = |name: &std::ffi::OsStr| -> TestResult {
        let directory = worktrees.join(name);
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&directory)?;
        std::fs::write(directory.join("reservation.json"), "{")?;
        Ok(())
    };
    unreadable(std::ffi::OsStr::new("other"))?;
    assert_eq!(store.writer_fence_listing()?.issues.len(), 1);
    assert!(
        store.custody_released(&record, &call)?,
        "another worktree's unreadable reservation is not this attempt's"
    );
    let own = provenant_confinement::worktrees::state_dir_name(&worktree);
    unreadable(&own)?;
    assert_eq!(store.writer_fence_listing()?.issues.len(), 2);
    assert!(
        !store.custody_released(&record, &call)?,
        "this writer's unreadable reservation keeps custody held"
    );
    Ok(())
}

fn hold_child_fixture(
    store: &RunStore,
    directory: Directory,
    scope: provenant_supervisor::custody::Scope,
    requester: provenant_core::ActorId,
) -> TestResult {
    let custody = directory.try_lock(
        std::ffi::OsStr::new("custody.lock"),
        provenant_os::fs::LockMode::Exclusive,
    )?;
    store.hold_child(
        provenant_supervisor::custody::hold(directory, scope, requester),
        custody.metadata()?,
    );
    Ok(())
}

#[test]
fn nested_lane_depth_stops_at_the_configured_maximum() {
    assert!(matches!(next_lane_depth(0, 2), Ok(1)));
    assert!(matches!(next_lane_depth(1, 2), Ok(2)));
    assert!(matches!(
        next_lane_depth(2, 2),
        Err(RunError::DepthExceeded {
            depth: 3,
            max_depth: 2
        })
    ));
}

#[test]
fn host_checks_child_metadata_against_its_parent_record() -> TestResult {
    let parent = AttemptId::from_bytes([9; 16]);
    for (depth, proposed_parent, valid) in [
        (Some(2), Some(parent), true),
        (Some(1), Some(parent), false),
        (None, Some(parent), false),
        (Some(2), Some(AttemptId::from_bytes([8; 16])), false),
    ] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let mut nested = input();
        nested.request.lane_depth = depth;
        nested.request.parent_attempt = proposed_parent;
        let record = store.create(&nested, &call)?;
        assert_eq!(record.parent, None);
        assert_eq!(record.parent_attempt, proposed_parent);
        assert_eq!(store.run(record.run_id, &call)?, record);
        assert_eq!(store.queued_runs(&call)?, vec![record.run_id]);
        assert_eq!(
            store
                .validate_child(
                    record.run_id,
                    parent,
                    &input().request.tasks[0],
                    1,
                    2,
                    &call
                )
                .is_ok(),
            valid
        );
        if !valid {
            store.refuse_queued(record.run_id, "host refused child metadata", &call)?;
            assert!(store.queued_runs(&call)?.is_empty());
            assert!(matches!(
                store.record(nested.attempts[0].attempt_id, &call)?.state,
                AttemptState::Terminal {
                    outcome: Outcome::Rejected,
                    ..
                }
            ));
        }
    }
    Ok(())
}

#[test]
fn host_refuses_child_authority_beyond_its_parent_task() -> TestResult {
    let parent = AttemptId::from_bytes([9; 16]);
    let writer = |worktree: &str| WriterRequest {
        project: "/project".into(),
        worktree: worktree.into(),
    };
    let deliver = |branch: &str, open_pr| WriterDeliveryRequest {
        mode: Default::default(),
        remote: "origin".into(),
        branch: branch.into(),
        open_pr,
    };
    let read_only = input().request.tasks[0].clone();
    let mut writing = read_only.clone();
    writing.writer = Some(writer("/project/.worktrees/parent"));
    let mut delivering = writing.clone();
    delivering.deliver = Some(deliver("feature", true));
    // The host holds no sealed list of worktrees a lane may delegate, so no
    // lane can propose a writer or delivering child, even its own worktree.
    for (parent_task, child_writer, child_delivery, case) in [
        (
            &read_only,
            None,
            None,
            "read-only child of read-only parent",
        ),
        (&writing, None, None, "read-only child of writer parent"),
        (
            &read_only,
            Some(writer("/project/.worktrees/child")),
            None,
            "writer child of read-only parent",
        ),
        (
            &writing,
            Some(writer("/project/.worktrees/parent")),
            None,
            "writer child in its parent's worktree",
        ),
        (
            &writing,
            Some(writer("/project/.worktrees/other")),
            None,
            "writer child in a sibling worktree",
        ),
        (
            &writing,
            Some(WriterRequest {
                project: "/other-project".into(),
                worktree: "/other-project/.worktrees/child".into(),
            }),
            None,
            "writer child in another project",
        ),
        (
            &delivering,
            None,
            Some(deliver("feature", false)),
            "delivery child without a writer",
        ),
        (
            &delivering,
            Some(writer("/project/.worktrees/parent")),
            Some(deliver("feature", false)),
            "delivery child within parent grant",
        ),
    ] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let mut nested = input();
        nested.request.lane_depth = Some(2);
        nested.request.parent_attempt = Some(parent);
        nested.request.tasks[0].writer = child_writer;
        nested.request.tasks[0].deliver = child_delivery;
        let proposes_writer =
            nested.request.tasks[0].writer.is_some() || nested.request.tasks[0].deliver.is_some();
        // create() rejects delivery without a writer, but a lane can bypass
        // it by rewriting its store. Exercise validate_child independently.
        let delivery_only = if nested.request.tasks[0].writer.is_none() {
            nested.request.tasks[0].deliver.take()
        } else {
            None
        };
        let record = store.create(&nested, &call)?;
        if let Some(delivery) = delivery_only {
            nested.request.tasks[0].deliver = Some(delivery);
            rewrite_child_request(&store, record.run_id, &nested.request, &call)?;
        }
        let result = store.validate_child(record.run_id, parent, parent_task, 1, 2, &call);
        if proposes_writer {
            assert!(
                matches!(
                    result,
                    Err(RunError::Validation("a lane cannot propose a writer child"))
                ),
                "{case}: {result:?}"
            );
        } else {
            assert!(result.is_ok(), "{case}: {result:?}");
        }
    }
    Ok(())
}

#[test]
fn host_refuses_child_access_beyond_its_parent_task() -> TestResult {
    let parent = AttemptId::from_bytes([9; 16]);
    let temp = tempfile::tempdir()?;
    let base = std::fs::canonicalize(temp.path())?;
    for name in ["reads/sub", "writes/sub", "outside", "home/.config"] {
        std::fs::create_dir_all(base.join(name))?;
    }
    std::os::unix::fs::symlink(base.join("outside"), base.join("reads/alias"))?;
    let access = |network, reads: &[&str], writes: &[&str]| LaneAccess {
        network,
        read_roots: reads.iter().map(|path| base.join(path)).collect(),
        write_roots: writes.iter().map(|path| base.join(path)).collect(),
    };
    let task = |writer: bool, access: LaneAccess| {
        let mut task = input().request.tasks[0].clone();
        task.writer = writer.then(|| WriterRequest {
            project: "/project".into(),
            worktree: "/project/.worktrees/sibling".into(),
        });
        task.access = access;
        task
    };
    let read_only = task(false, LaneAccess::default());
    let quiet_read_only = task(false, access(Some(NetworkAccess::Provider), &[], &[]));
    let granted = task(
        false,
        access(Some(NetworkAccess::Local), &["reads"], &["writes"]),
    );
    let quiet_writer = task(true, access(Some(NetworkAccess::Provider), &[], &[]));
    for (parent_task, child, valid, case) in [
        (
            &read_only,
            task(false, LaneAccess::default()),
            true,
            "default child",
        ),
        (
            &read_only,
            task(false, access(None, &[], &["writes"])),
            false,
            "write root under a read-only parent",
        ),
        (
            &read_only,
            task(false, access(None, &["home/.config"], &[])),
            false,
            "extra read root under a read-only parent",
        ),
        (
            &read_only,
            task(false, access(Some(NetworkAccess::Open), &[], &[])),
            true,
            "network under a read-only parent",
        ),
        (
            &quiet_read_only,
            task(false, access(Some(NetworkAccess::Open), &[], &[])),
            false,
            "network beyond a quiet read-only parent's network",
        ),
        (
            &granted,
            task(
                false,
                access(
                    Some(NetworkAccess::Local),
                    &["reads/sub", "writes"],
                    &["writes/sub"],
                ),
            ),
            true,
            "child within parent roots",
        ),
        (
            &granted,
            task(false, access(Some(NetworkAccess::Open), &[], &[])),
            false,
            "network beyond parent network",
        ),
        (
            &granted,
            task(false, access(None, &[], &["reads/sub"])),
            false,
            "write root inside a parent read root",
        ),
        (
            &granted,
            task(false, access(None, &["reads/alias"], &[])),
            false,
            "symlinked read root escaping the parent root",
        ),
        (
            &quiet_writer,
            task(false, LaneAccess::default()),
            true,
            "read-only default child of a quiet writer",
        ),
        (
            &quiet_writer,
            task(false, access(Some(NetworkAccess::Local), &[], &[])),
            false,
            "network beyond a writer parent's network",
        ),
    ] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let mut nested = input();
        nested.request.lane_depth = Some(2);
        nested.request.parent_attempt = Some(parent);
        nested.request.tasks[0] = child;
        let record = store.create(&nested, &call)?;
        let result = store.validate_child(record.run_id, parent, parent_task, 1, 2, &call);
        assert_eq!(result.is_ok(), valid, "{case}: {result:?}");
        if valid && case == "read-only default child of a quiet writer" {
            let validated = result.as_ref().unwrap();
            assert_eq!(
                validated
                    .effective_task(&validated.tasks[0])
                    .access
                    .network(false),
                NetworkAccess::Provider,
                "{case}: expected Provider network"
            );
        }
    }
    Ok(())
}

#[test]
fn host_refuses_child_with_extra_attempt_rows() -> TestResult {
    for terminal in [false, true] {
        for forged_task_index in [false, true] {
            let temp = tempfile::tempdir()?;
            let (clock, cancel) = (clock(), CancelToken::new());
            let call = call(&clock, &cancel);
            let store = RunStore::open(&context(temp.path())?, &call)?;
            let parent = AttemptId::from_bytes([9; 16]);
            let mut nested = input();
            nested.request.lane_depth = Some(2);
            nested.request.parent_attempt = Some(parent);
            store.create(&nested, &call)?;
            let mut extra = store.record(nested.attempts[0].attempt_id, &call)?;
            extra.attempt_id = AttemptId::from_bytes([22; 16]);
            extra.attempt_number = 2;
            extra.prompt.path = artifacts::artifact_path(extra.attempt_id, "prompt.txt")?;
            if terminal {
                extra.state = AttemptState::Terminal {
                    outcome: Outcome::Rejected,
                    exit_code: None,
                    finished_at: WallTime::from_micros(150)?,
                    reason: Some("injected terminal row".into()),
                };
            }
            let indexed_task = if forged_task_index {
                TaskId::from_bytes([23; 16])
            } else {
                extra.task_id
            };
            insert_child_attempt(&store, &extra, indexed_task)?;
            assert!(matches!(
                store.validate_child(
                    nested.run_id,
                    parent,
                    &input().request.tasks[0],
                    1,
                    2,
                    &call,
                ),
                Err(RunError::Validation("child task has more than one attempt"))
            ));
        }
    }
    Ok(())
}

#[test]
fn host_refuses_child_attempt_records_outside_the_request() -> TestResult {
    for wrong_run in [false, true] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let parent = AttemptId::from_bytes([9; 16]);
        let mut nested = input();
        nested.request.lane_depth = Some(2);
        nested.request.parent_attempt = Some(parent);
        store.create(&nested, &call)?;
        let mut attempt = store.record(nested.attempts[0].attempt_id, &call)?;
        if wrong_run {
            attempt.run_id = provenant_core::RunId::from_bytes([23; 16]);
        } else {
            attempt.task_id = TaskId::from_bytes([23; 16]);
        }
        // Keep the index columns unchanged: validation must inspect the record.
        store.db.execute(
            "UPDATE attempts SET record=?1 WHERE id=?2",
            rusqlite::params![crate::db::bytes(&attempt)?, attempt.attempt_id.to_string()],
        )?;
        let result = store.validate_child(
            nested.run_id,
            parent,
            &input().request.tasks[0],
            1,
            2,
            &call,
        );
        let expected = if wrong_run {
            "child attempt run does not match"
        } else {
            "child attempt task missing from request"
        };
        assert!(matches!(result, Err(RunError::Validation(reason)) if reason == expected));
    }
    Ok(())
}

fn insert_child_attempt(
    store: &RunStore,
    record: &AttemptRecord,
    indexed_task: TaskId,
) -> TestResult {
    store.db.execute(
        "INSERT INTO attempts(id,run,project,task,number,revision,terminal,record) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        rusqlite::params![
            record.attempt_id.to_string(),
            record.run_id.to_string(),
            record.project_id.to_string(),
            indexed_task.to_string(),
            crate::db::sql_int(record.attempt_number)?,
            crate::db::sql_int(record.revision)?,
            matches!(record.state, AttemptState::Terminal { .. }),
            crate::db::bytes(record)?,
        ],
    )?;
    Ok(())
}

#[test]
fn host_launches_one_attempt_per_child_task() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?
        .with_sealed_requester(provenant_core::ActorId::from_bytes([2; 16]));
    let parent = AttemptId::from_bytes([9; 16]);
    let mut nested = input();
    nested.request.lane_depth = Some(2);
    nested.request.parent_attempt = Some(parent);
    store.create(&nested, &call)?;
    let validated = store.validate_child(
        nested.run_id,
        parent,
        &input().request.tasks[0],
        1,
        2,
        &call,
    )?;
    let mut extra = store.record(nested.attempts[0].attempt_id, &call)?;
    extra.attempt_id = AttemptId::from_bytes([22; 16]);
    extra.attempt_number = 2;
    extra.prompt.path = artifacts::artifact_path(extra.attempt_id, "prompt.txt")?;
    insert_child_attempt(&store, &extra, extra.task_id)?;
    let fake = Fake::default();
    let started = store.start_child(
        nested.run_id,
        &validated,
        &fake,
        &|_, _| unreachable!(),
        &call,
    )?;
    assert_eq!(fake.prepared.get(), 0);
    assert_eq!(started.len(), 2);
    for id in [nested.attempts[0].attempt_id, extra.attempt_id] {
        assert!(started.iter().any(|result| matches!(result,
            Started::Refused { attempt, reason, kind: RefusalKind::Preparation }
            if *attempt == id && reason == "child task has more than one attempt"
        )));
        assert_eq!(terminal(&store, id, &call)?, Outcome::Rejected);
        assert_eq!(store.record(id, &call)?.generation, 0);
    }
    Ok(())
}

#[test]
fn host_refuses_extra_child_attempt_without_touching_held_attempt() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let requester = provenant_core::ActorId::from_bytes([2; 16]);
    let store = RunStore::open(&context(temp.path())?, &call)?.with_sealed_requester(requester);
    let parent = AttemptId::from_bytes([9; 16]);
    let mut nested = input();
    nested.request.lane_depth = Some(2);
    nested.request.parent_attempt = Some(parent);
    store.create(&nested, &call)?;
    let validated = store.validate_child(
        nested.run_id,
        parent,
        &input().request.tasks[0],
        1,
        2,
        &call,
    )?;
    let id = nested.attempts[0].attempt_id;
    let mut extra = store.record(id, &call)?;
    let held = store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    hold_child_fixture(
        &store,
        store.attempt_dir(nested.run_id, id, false)?,
        RunStore::scope(&held),
        requester,
    )?;
    extra.attempt_id = AttemptId::from_bytes([22; 16]);
    extra.attempt_number = 2;
    extra.prompt.path = artifacts::artifact_path(extra.attempt_id, "prompt.txt")?;
    insert_child_attempt(&store, &extra, extra.task_id)?;
    let fake = Fake::default();
    let started = store.start_child(
        nested.run_id,
        &validated,
        &fake,
        &|_, _| unreachable!(),
        &call,
    )?;
    assert!(matches!(started.as_slice(), [Started::Refused {
        attempt, kind: RefusalKind::Preparation, ..
    }] if *attempt == extra.attempt_id));
    assert_eq!(fake.prepared.get(), 0);
    assert_eq!(store.record(id, &call)?, held);
    assert_eq!(
        terminal(&store, extra.attempt_id, &call)?,
        Outcome::Rejected
    );
    assert_eq!(store.held_attempts(), vec![id]);
    assert!(store.launched.borrow()[0].handle.is_some());
    Ok(())
}

#[test]
fn host_remembers_child_task_after_attempt_row_deletion() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?
        .with_sealed_requester(provenant_core::ActorId::from_bytes([2; 16]));
    let parent = AttemptId::from_bytes([9; 16]);
    let mut nested = input();
    nested.request.lane_depth = Some(2);
    nested.request.parent_attempt = Some(parent);
    store.create(&nested, &call)?;
    let validated = store.validate_child(
        nested.run_id,
        parent,
        &input().request.tasks[0],
        1,
        2,
        &call,
    )?;
    let id = nested.attempts[0].attempt_id;
    let mut replacement = store.record(id, &call)?;
    let handed_off = Cell::new(0);
    store.start_child(
        nested.run_id,
        &validated,
        &Ready,
        &|_, _| {
            handed_off.set(handed_off.get() + 1);
            Err(RunError::Validation("fixture observed handoff"))
        },
        &call,
    )?;
    assert_eq!(handed_off.get(), 1);
    assert_eq!(terminal(&store, id, &call)?, Outcome::Rejected);
    // The lane owns this database, including its immutability triggers.
    store
        .db
        .execute_batch("PRAGMA foreign_keys=OFF; DROP TRIGGER immutable_terminal_delete")?;
    store
        .db
        .execute("DELETE FROM attempts WHERE id=?1", [id.to_string()])?;
    replacement.attempt_id = AttemptId::from_bytes([22; 16]);
    replacement.prompt.path = artifacts::artifact_path(replacement.attempt_id, "prompt.txt")?;
    insert_child_attempt(&store, &replacement, replacement.task_id)?;
    // Make the replacement fully launchable, including its prompt and ticket.
    let directory = store.attempt_dir(nested.run_id, replacement.attempt_id, true)?;
    artifacts::seal(&directory, "prompt.txt", b"exact prompt", RECORD_LIMIT)?;
    store.queue_ticket(&store.db, &replacement, LaneClass::ReadOnly, 1, 0, 1)?;
    let revalidated = store.validate_child(
        nested.run_id,
        parent,
        &input().request.tasks[0],
        1,
        2,
        &call,
    );
    let fake = Fake {
        refuse: true,
        ..Default::default()
    };
    let started = store.start_child(
        nested.run_id,
        &validated,
        &fake,
        &|_, _| unreachable!(),
        &call,
    )?;
    assert_eq!(
        fake.prepared.get(),
        0,
        "deleted rows must not reset launch history"
    );
    assert!(
        matches!(started.as_slice(), [Started::Refused { attempt, .. }]
        if *attempt == replacement.attempt_id)
    );
    assert!(matches!(
        revalidated,
        Err(RunError::Validation("child task already launched"))
    ));
    Ok(())
}

/// Mutate lane-owned ingress at the provider preparation boundary.
struct PrepareHook<'a>(&'a dyn Fn() -> TestResult);
impl Providers for PrepareHook<'_> {
    fn prepare(
        &self,
        task: &TaskRequest,
        prompt: &str,
        paths: &LaunchPaths,
        resume: Option<&AttemptRecord>,
    ) -> std::result::Result<PreparedLaunch, String> {
        (self.0)().map_err(|error| error.to_string())?;
        Ready.prepare(task, prompt, paths, resume)
    }
    fn decode(
        &self,
        _: &RouteRecord,
        _: &[u8],
        _: &ExitObservation,
        _: &std::path::Path,
    ) -> std::result::Result<Decoded, String> {
        unreachable!("no guardian runs a provider in these tests")
    }
}

#[test]
fn host_refuses_child_identity_swap_during_preparation() -> TestResult {
    for wrong_run in [false, true] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?
            .with_sealed_requester(provenant_core::ActorId::from_bytes([2; 16]));
        let parent = AttemptId::from_bytes([9; 16]);
        let mut nested = input();
        nested.request.lane_depth = Some(2);
        nested.request.parent_attempt = Some(parent);
        store.create(&nested, &call)?;
        let validated = store.validate_child(
            nested.run_id,
            parent,
            &input().request.tasks[0],
            1,
            2,
            &call,
        )?;
        let mut swapped = store.record(nested.attempts[0].attempt_id, &call)?;
        if wrong_run {
            let mut other = input();
            other.run_id = provenant_core::RunId::from_bytes([24; 16]);
            other.operation_id = provenant_core::OperationId::parse("other-run")?;
            other.attempts[0].attempt_id = AttemptId::from_bytes([25; 16]);
            store.create(&other, &call)?;
            swapped.run_id = other.run_id;
            swapped.operation_id = other.operation_id;
            swapped.attempt_number = 2;
        } else {
            swapped.task_id = TaskId::from_bytes([23; 16]);
            // Keep record(), including its run association check, valid.
            let mut run = store.run(nested.run_id, &call)?;
            run.task_ids.push(swapped.task_id);
            store.db.execute(
                "UPDATE runs SET record=?1 WHERE id=?2",
                rusqlite::params![crate::db::bytes(&run)?, run.run_id.to_string()],
            )?;
        }
        let rewrite = || {
            store.db.execute(
                "UPDATE attempts SET task=?1,run=?2,record=?3,number=?5 WHERE id=?4",
                rusqlite::params![
                    swapped.task_id.to_string(),
                    swapped.run_id.to_string(),
                    crate::db::bytes(&swapped)?,
                    swapped.attempt_id.to_string(),
                    crate::db::sql_int(swapped.attempt_number)?
                ],
            )?;
            // Prove this coherent mutation passes the existing reader checks.
            store.record(swapped.attempt_id, &call)?;
            Ok(())
        };
        let handed_off = Cell::new(0);
        store.start_child(
            nested.run_id,
            &validated,
            &PrepareHook(&rewrite),
            &|_, _| {
                handed_off.set(handed_off.get() + 1);
                Err(RunError::Validation("fixture observed handoff"))
            },
            &call,
        )?;
        assert_eq!(
            handed_off.get(),
            0,
            "identity swap must stop before handoff"
        );
        assert_eq!(
            store.record(swapped.attempt_id, &call)?.state,
            AttemptState::Queued
        );
    }
    Ok(())
}

#[test]
fn host_refuses_invalid_child_rows_added_after_validation_without_aborting_scan() -> TestResult {
    for wrong_run in [false, true] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?
            .with_sealed_requester(provenant_core::ActorId::from_bytes([2; 16]));
        let parent = AttemptId::from_bytes([9; 16]);
        let mut nested = two_tasks(2);
        nested.request.lane_depth = Some(2);
        nested.request.parent_attempt = Some(parent);
        store.create(&nested, &call)?;
        let validated = store.validate_child(
            nested.run_id,
            parent,
            &input().request.tasks[0],
            1,
            2,
            &call,
        )?;
        let mut extra = store.record(nested.attempts[0].attempt_id, &call)?;
        extra.attempt_id = AttemptId::from_bytes([22; 16]);
        extra.task_id = TaskId::from_bytes([23; 16]);
        extra.prompt.path = artifacts::artifact_path(extra.attempt_id, "prompt.txt")?;
        insert_child_attempt(&store, &extra, extra.task_id)?;
        // Encounter the bad row first, before either valid sibling.
        store.db.execute(
            "UPDATE attempts SET sequence=0 WHERE id=?1",
            [extra.attempt_id.to_string()],
        )?;
        if wrong_run {
            extra.run_id = provenant_core::RunId::from_bytes([24; 16]);
            extra.task_id = nested.request.tasks[0].task_id;
            store.db.execute(
                "UPDATE attempts SET record=?1 WHERE id=?2",
                rusqlite::params![crate::db::bytes(&extra)?, extra.attempt_id.to_string()],
            )?;
        } else {
            // An unknown task can still pass record() if the lane also rewrites its run.
            let mut run = store.run(nested.run_id, &call)?;
            run.task_ids.push(extra.task_id);
            store.db.execute(
                "UPDATE runs SET record=?1 WHERE id=?2",
                rusqlite::params![crate::db::bytes(&run)?, run.run_id.to_string()],
            )?;
            store.record(extra.attempt_id, &call)?;
        }
        let fake = Fake {
            refuse: true,
            ..Default::default()
        };
        let started = store.start_child(
            nested.run_id,
            &validated,
            &fake,
            &|_, _| unreachable!(),
            &call,
        )?;
        assert_eq!(fake.prepared.get(), 2);
        assert_eq!(started.len(), 3);
        let expected = if wrong_run {
            "child attempt run does not match"
        } else {
            "child attempt task missing from request"
        };
        assert!(started.iter().any(|result| matches!(result,
            Started::Refused { attempt, reason, kind: RefusalKind::Preparation }
                if *attempt == extra.attempt_id && reason == expected
        )));
        for attempt in &nested.attempts {
            assert_eq!(
                terminal(&store, attempt.attempt_id, &call)?,
                Outcome::Rejected
            );
        }
    }
    Ok(())
}

#[test]
fn host_launches_child_tasks_independently() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?
        .with_sealed_requester(provenant_core::ActorId::from_bytes([2; 16]));
    let parent = AttemptId::from_bytes([9; 16]);
    let mut nested = two_tasks(2);
    nested.request.lane_depth = Some(2);
    nested.request.parent_attempt = Some(parent);
    store.create(&nested, &call)?;
    let validated = store.validate_child(
        nested.run_id,
        parent,
        &input().request.tasks[0],
        1,
        2,
        &call,
    )?;
    let handed_off = RefCell::new(Vec::new());
    let started = store.start_child(
        nested.run_id,
        &validated,
        &Ready,
        &|record, _| {
            handed_off.borrow_mut().push(record.task_id);
            Err(RunError::Validation("fixture observed handoff"))
        },
        &call,
    )?;
    assert_eq!(started.len(), 2);
    assert_eq!(
        *handed_off.borrow(),
        nested
            .request
            .tasks
            .iter()
            .map(|task| task.task_id)
            .collect::<Vec<_>>()
    );
    Ok(())
}

#[test]
fn host_refuses_writer_child_after_request_rewrite() -> TestResult {
    let parent = AttemptId::from_bytes([9; 16]);
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?
        .with_sealed_requester(provenant_core::ActorId::from_bytes([7; 16]));
    let parent_task = input().request.tasks[0].clone();
    let mut nested = input();
    nested.request.lane_depth = Some(2);
    nested.request.parent_attempt = Some(parent);
    let record = store.create(&nested, &call)?;
    let validated = store.validate_child(record.run_id, parent, &parent_task, 1, 2, &call)?;

    // Rewrite both lane-writable copies so the host's validated digest is
    // the remaining boundary against adding writer authority before launch.
    let mut swapped = (*validated).clone();
    swapped.tasks[0].writer = Some(WriterRequest {
        project: "/project".into(),
        worktree: "/project/.worktrees/child".into(),
    });
    rewrite_child_request(&store, record.run_id, &swapped, &call)?;
    let fake = Fake::default();
    assert!(matches!(
        store.start_child(
            record.run_id,
            &validated,
            &fake,
            &|_, _| unreachable!(),
            &call
        ),
        Err(RunError::Validation(
            "child request changed after validation"
        ))
    ));
    assert_eq!(fake.prepared.get(), 0);
    assert_ne!(
        store
            .record(nested.attempts[0].attempt_id, &call)?
            .provider_launched,
        Some(true)
    );
    Ok(())
}

fn rewrite_child_request(
    store: &RunStore,
    run: RunId,
    request: &CanonicalRequest,
    call: &Request<'_>,
) -> TestResult {
    store.run_dir(run, false)?.atomic_write(
        std::ffi::OsStr::new("request.json"),
        ".swap-",
        &crate::db::bytes(request)?,
        provenant_os::fs::PublishMode::Replace,
    )?;
    let mut record = store.run(run, call)?;
    record.request_digest = request.digest()?;
    store.db.execute(
        "UPDATE runs SET record=?1 WHERE id=?2",
        rusqlite::params![crate::db::bytes(&record)?, run.to_string()],
    )?;
    assert_eq!(&store.request(run, call)?, request);
    Ok(())
}

#[test]
fn lane_continuation_respects_sealed_parent_or_provider_fallback() -> TestResult {
    for (bound, expected) in [
        (None, NetworkAccess::Provider),
        (Some(NetworkAccess::Local), NetworkAccess::Local),
        (Some(NetworkAccess::Provider), NetworkAccess::Provider),
    ] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?
            .with_writer_registry(&temp.path().join("host"));
        let store = if let Some(network) = bound {
            store.with_sealed_parent_network(network)
        } else {
            store
        };
        let prior = input();
        store.create(&prior, &call)?;
        let mut proposal = input();
        proposal.run_id = RunId::from_bytes([35; 16]);
        proposal.operation_id = "lane-continuation".parse()?;
        proposal.attempts[0].attempt_id = AttemptId::from_bytes([36; 16]);
        proposal.request.parent = Some(prior.attempts[0].attempt_id);
        proposal.request.tasks[0].access.network = Some(NetworkAccess::Provider);
        let mut narrower = proposal.request.tasks[0].clone();
        narrower.access.network = Some(NetworkAccess::Provider);
        assert_eq!(
            store
                .effective_launch_task(&proposal.request, &narrower, None)
                .access
                .network(false),
            NetworkAccess::Provider
        );
        // A lane-writable request cannot bypass the check by omitting its parent lane.
        proposal.request.parent_attempt = None;
        let run = store.create(&proposal, &call)?;
        let expected_request = proposal.request.clone();
        proposal.request.tasks[0].access.network = Some(NetworkAccess::Open);
        proposal.request.parent = None;
        rewrite_child_request(&store, run.run_id, &proposal.request, &call)?;
        let fake = Fake {
            refuse: true,
            ..Default::default()
        };
        assert!(matches!(
            store.start_checked(
                run.run_id,
                &expected_request,
                &fake,
                &|_, _| unreachable!(),
                &call
            ),
            Err(RunError::Validation(
                "request changed after launch validation"
            ))
        ));
        assert_eq!(fake.prepared.get(), 0);
        store.start(run.run_id, &fake, &|_, _| unreachable!(), &call)?;
        assert_eq!(fake.network.get(), Some(expected));
    }
    Ok(())
}

#[test]
fn host_launches_only_the_validated_child_request() -> TestResult {
    let parent = AttemptId::from_bytes([9; 16]);
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?
        .with_sealed_requester(provenant_core::ActorId::from_bytes([7; 16]));
    let mut parent_task = input().request.tasks[0].clone();
    parent_task.access.network = Some(NetworkAccess::Provider);
    let mut nested = input();
    nested.request.lane_depth = Some(2);
    nested.request.parent_attempt = Some(parent);
    let record = store.create(&nested, &call)?;
    let validated = store.validate_child(record.run_id, parent, &parent_task, 1, 2, &call)?;

    // The lane swaps its sealed request and the recorded digest together.
    let mut swapped = (*validated).clone();
    swapped.tasks[0].access.network = Some(NetworkAccess::Open);
    store.run_dir(record.run_id, false)?.atomic_write(
        std::ffi::OsStr::new("request.json"),
        ".swap-",
        &crate::db::bytes(&swapped)?,
        provenant_os::fs::PublishMode::Replace,
    )?;
    let mut run = store.run(record.run_id, &call)?;
    run.request_digest = swapped.digest()?;
    store.db.execute(
        "UPDATE runs SET record=?1 WHERE id=?2",
        rusqlite::params![crate::db::bytes(&run)?, run.run_id.to_string()],
    )?;
    assert_eq!(store.request(record.run_id, &call)?, swapped);
    let fake = Fake::default();
    assert!(matches!(
        store.start_checked(
            record.run_id,
            &validated,
            &fake,
            &|_, _| unreachable!(),
            &call
        ),
        Err(RunError::Validation(
            "request changed after launch validation"
        ))
    ));
    assert!(matches!(
        store.start_child(
            record.run_id,
            &validated,
            &fake,
            &|_, _| unreachable!(),
            &call
        ),
        Err(RunError::Validation(
            "child request changed after validation"
        ))
    ));
    assert_eq!(fake.prompt.borrow().as_str(), "");

    // A child cannot continue an attempt, whose retained state the host copies.
    let refuse = Fake {
        refuse: true,
        ..Fake::default()
    };
    store.start_child(
        record.run_id,
        &ValidatedChild::fixture(swapped),
        &refuse,
        &|_, _| unreachable!(),
        &call,
    )?;
    let continued = store.resume(
        &Resume {
            prior: nested.attempts[0].attempt_id,
            expected_generation: 0,
            operation_id: provenant_core::OperationId::parse("continue")
                .map_err(|_| RunError::Validation("operation"))?,
            run_id: provenant_core::RunId::from_bytes([20; 16]),
            attempt_id: AttemptId::from_bytes([21; 16]),
            prompt: "answer".into(),
            created_at: WallTime::from_micros(100).map_err(|_| RunError::Validation("time"))?,
        },
        &call,
    )?;
    assert!(matches!(
        store.validate_child(continued.run_id, parent, &parent_task, 1, 2, &call),
        Err(RunError::Validation(
            "a child run cannot continue an attempt"
        ))
    ));
    let fake = Fake {
        refuse: true,
        ..Default::default()
    };
    store.start(continued.run_id, &fake, &|_, _| unreachable!(), &call)?;
    assert_eq!(fake.network.get(), Some(NetworkAccess::Provider));
    Ok(())
}

/// Prepares a plan; no guardian ever runs it.
pub(super) struct Ready;

impl Providers for Ready {
    fn prepare(
        &self,
        _: &TaskRequest,
        _: &str,
        _: &LaunchPaths,
        _: Option<&AttemptRecord>,
    ) -> std::result::Result<PreparedLaunch, String> {
        Ok(PreparedLaunch {
            provider_lease: None,
            argv: vec!["fixture".into()],
            environment: Vec::new(),
            stdin: Vec::new(),
            secret_input: None,
            cwd: "/".into(),
            route: route(),
            skipped_routes: Vec::new(),
            observed_head: None,
            repository: None,
            confinement: None,
        })
    }
    fn decode(
        &self,
        _: &RouteRecord,
        _: &[u8],
        _: &ExitObservation,
        _: &std::path::Path,
    ) -> std::result::Result<Decoded, String> {
        unreachable!("no guardian runs a provider in these tests")
    }
}

struct FirstPriorityFailure(TaskId);
impl Providers for FirstPriorityFailure {
    fn priority(&self, task: &TaskRequest) -> std::result::Result<u8, String> {
        if task.task_id == self.0 {
            Err("priority configuration unavailable".into())
        } else {
            Ok(5)
        }
    }
    fn prepare(
        &self,
        task: &TaskRequest,
        prompt: &str,
        paths: &LaunchPaths,
        resume: Option<&AttemptRecord>,
    ) -> std::result::Result<PreparedLaunch, String> {
        Ready.prepare(task, prompt, paths, resume)
    }
    fn decode(
        &self,
        route: &RouteRecord,
        output: &[u8],
        exit: &ExitObservation,
        path: &std::path::Path,
    ) -> std::result::Result<Decoded, String> {
        Ready.decode(route, output, exit, path)
    }
}

#[test]
fn priority_selection_failure_does_not_stall_siblings() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let prepared = two_tasks(2);
    store.create(&prepared, &call)?;
    let first = prepared.attempts[0].attempt_id;
    let sibling = prepared.attempts[1].attempt_id;
    let held = RefCell::new(Vec::new());
    let holder = |record: &AttemptRecord, _| {
        held.borrow_mut().push(record.attempt_id);
        Err(RunError::Validation("fixture holder"))
    };
    let started = store.start(
        prepared.run_id,
        &FirstPriorityFailure(prepared.attempts[0].task_id),
        &holder,
        &call,
    )?;
    assert!(matches!(started.as_slice(), [
        Started::Refused { attempt, kind: RefusalKind::Preparation, reason },
        Started::Refused { attempt: second, .. },
    ] if *attempt == first && reason.contains("priority configuration unavailable") && *second == sibling));
    assert_eq!(*held.borrow(), [sibling]);
    let index = WorkIndex::open(store.writer_registry(), &call)?;
    let selector = |attempt| WorkSelector {
        project: store.context.project_id,
        run: prepared.run_id,
        task: None,
        attempt: Some(attempt),
        repository: None,
        head: None,
    };
    assert_eq!(
        index.validate(&selector(first), &call)?,
        WorkCoverage::Unindexed
    );
    assert_eq!(
        index.validate(&selector(sibling), &call)?,
        WorkCoverage::Verified
    );
    assert_eq!(terminal(&store, first, &call)?, Outcome::Rejected);
    Ok(())
}

/// Carries a real permission plan through launch, stopping before compilation.
struct Confined {
    writer: bool,
    tool_writes: Vec<PathGrant>,
}
impl Providers for Confined {
    fn prepare(
        &self,
        task: &TaskRequest,
        prompt: &str,
        paths: &LaunchPaths,
        resume: Option<&AttemptRecord>,
    ) -> std::result::Result<PreparedLaunch, String> {
        use provenant_confinement::permissions::{
            AccessMode, NetworkAccess, PlanPermissions, ProviderProfile, ProviderProfileSpec,
            WorktreeWriteAuthority,
        };
        use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
        let grant = |path: &str, scope| PathGrant {
            path: path.into(),
            scope,
        };
        let mode = if self.writer {
            let authority: WorktreeWriteAuthority = serde_json::from_value(serde_json::json!({
                "git_grants_digest": vec![7; 32],
                "publishing": true,
                "cargo_cache": {"path": "/fixture/cache", "scope": "subtree"}
            }))
            .map_err(|e| e.to_string())?;
            AccessMode::WorktreeWrite(Box::new(authority))
        } else {
            AccessMode::ReadOnly
        };
        let plan = ConfinedPlan::new(PlanRequest {
            scope: provenant_core::WorkScope::Project(provenant_core::ProjectId::from_bytes(
                [1; 16],
            )),
            codex: CodexPolicy::Clean,
            extensions: ExtensionPolicy::Deny,
            agy_state: None,
            permissions: PlanPermissions {
                read_only: None,
                mode,
                scratch: grant("/fixture/scratch", PathScope::Subtree),
                private_state: Some(grant("/fixture/private", PathScope::Subtree)),
                read_roots: vec![grant("/fixture/repo", PathScope::Subtree)],
                protected_roots: vec![],
                secret_roots: vec![],
                selected_authentication: None,
                include_continuation: false,
                write_roots: vec![grant("/fixture/shared", PathScope::Subtree)],
                network: NetworkAccess::Provider,
                nested_confinement: false,
            },
        })
        .map_err(|e| e.to_string())?;
        let mut launch = Ready.prepare(task, prompt, paths, resume)?;
        launch.confinement = Some(LaunchConfinement {
            plan,
            profile: ProviderProfile {
                spec: ProviderProfileSpec {
                    state_writes: vec![grant("/fixture/state/session", PathScope::Exact)],
                    tool_writes: self.tool_writes.clone(),
                    ..ProviderProfileSpec::default()
                },
                observations: Default::default(),
            },
            writer: None,
            home: "/fixture/home".into(),
            cargo_registry_seed: false,
            state_root: std::path::PathBuf::from("/fixture/state"),
        });
        Ok(launch)
    }
    fn decode(
        &self,
        _: &RouteRecord,
        _: &[u8],
        _: &ExitObservation,
        _: &std::path::Path,
    ) -> std::result::Result<Decoded, String> {
        unreachable!("no guardian runs a provider in these tests")
    }
}

#[test]
fn launch_records_plan_write_grants_and_git_digest() -> TestResult {
    use provenant_records::run::WriteSource;
    for writer in [false, true] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let context = context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let prepared = input();
        store.create(&prepared, &call)?;
        let id = prepared.attempts[0].attempt_id;
        assert!(matches!(
            store.start(
                prepared.run_id,
                &Confined { writer, tool_writes: vec![] },
                &|_, _| Err(RunError::Validation("fixture holder")),
                &call,
            )?.as_slice(),
            [Started::Refused { attempt, kind: RefusalKind::Preparation, reason }]
                if *attempt == id && reason.contains("fixture holder")
        ));
        drop(store);
        let store = RunStore::open(&context, &call)?;
        let record = store.record(id, &call)?;
        let mut expected = vec![
            (
                WriteSource::StateWrite,
                "/fixture/state/session",
                PathScope::Exact,
            ),
            (
                WriteSource::WriteRoot,
                "/fixture/shared",
                PathScope::Subtree,
            ),
            (WriteSource::Scratch, "/fixture/scratch", PathScope::Subtree),
            (
                WriteSource::PrivateState,
                "/fixture/private",
                PathScope::Subtree,
            ),
        ];
        if writer {
            expected.push((WriteSource::LaneCache, "/fixture/cache", PathScope::Subtree));
        }
        let expected = expected
            .into_iter()
            .map(|(source, path, scope)| {
                Ok(WriteGrantRecord {
                    source,
                    path: provenant_records::AbsolutePath::new(std::path::Path::new(path))?,
                    scope,
                })
            })
            .collect::<std::result::Result<Vec<_>, Box<dyn std::error::Error>>>()?;
        assert_eq!(record.write_grants, expected);
        assert_eq!(
            record.git_grants_digest,
            writer.then(|| provenant_core::Digest::from_bytes([7; 32]))
        );
    }
    Ok(())
}

/// Gives the first task oversized tool grants and its sibling none.
struct FirstOversized {
    first: TaskId,
    oversized: Confined,
    ordinary: Confined,
}
impl Providers for FirstOversized {
    fn prepare(
        &self,
        task: &TaskRequest,
        prompt: &str,
        paths: &LaunchPaths,
        resume: Option<&AttemptRecord>,
    ) -> std::result::Result<PreparedLaunch, String> {
        let provider = if task.task_id == self.first {
            &self.oversized
        } else {
            &self.ordinary
        };
        provider.prepare(task, prompt, paths, resume)
    }
    fn decode(
        &self,
        route: &RouteRecord,
        output: &[u8],
        exit: &ExitObservation,
        path: &std::path::Path,
    ) -> std::result::Result<Decoded, String> {
        self.ordinary.decode(route, output, exit, path)
    }
}

#[test]
fn oversized_write_grants_reject_without_stalling_launch() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let prepared = two_tasks(2);
    store.create(&prepared, &call)?;
    let id = prepared.attempts[0].attempt_id;
    let sibling = prepared.attempts[1].attempt_id;
    let provider = FirstOversized {
        first: prepared.attempts[0].task_id,
        oversized: Confined {
            writer: false,
            tool_writes: (0..128)
                .map(|i| PathGrant {
                    path: format!("/fixture/tools/{i}/{}", "x".repeat(3000)).into(),
                    scope: PathScope::Subtree,
                })
                .collect(),
        },
        ordinary: Confined {
            writer: false,
            tool_writes: vec![],
        },
    };
    let held = RefCell::new(Vec::new());
    let holder = |record: &AttemptRecord, _| {
        held.borrow_mut().push(record.attempt_id);
        Err(RunError::Validation("fixture holder"))
    };
    let started = store.start(prepared.run_id, &provider, &holder, &call)?;
    assert!(matches!(
        started.as_slice(),
        [
            Started::Refused { attempt: first, kind: RefusalKind::Preparation, reason },
            Started::Refused { attempt: second, reason: later, .. },
        ] if *first == id && reason.contains("256 KiB")
            && *second == sibling && later.contains("fixture holder")
    ));
    // The oversized attempt never reached handoff; its sibling did.
    assert_eq!(*held.borrow(), [sibling]);
    drop(store);
    let store = RunStore::open(&context, &call)?;
    let record = store.record(id, &call)?;
    assert!(matches!(
        record.state,
        AttemptState::Terminal {
            outcome: Outcome::Rejected,
            ..
        }
    ));
    assert!(record.write_grants.is_empty());
    let attempt = store.attempt_dir(prepared.run_id, id, false)?;
    assert!(
        attempt
            .metadata_at(std::ffi::OsStr::new("scratch"), false)
            .is_err_and(|e| e.is_not_found())
    );
    assert!(
        store
            .start(prepared.run_id, &provider, &|_, _| unreachable!(), &call)?
            .is_empty()
    );
    Ok(())
}

#[test]
fn child_launches_for_its_parent_requester_not_its_store_record() -> TestResult {
    let parent = AttemptId::from_bytes([9; 16]);
    let sealed = provenant_core::ActorId::from_bytes([7; 16]);
    let forged = provenant_core::ActorId::from_bytes([66; 16]);
    for child in [true, false] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let lane = context(temp.path())?;
        let mut store = RunStore::open(&lane, &call)?;
        let mut prepared = input();
        if child {
            prepared.request.lane_depth = Some(2);
            prepared.request.parent_attempt = Some(parent);
        }
        let record = store.create(&prepared, &call)?;
        // The lane rewrites the actor its child's run record names.
        let mut run = store.run(record.run_id, &call)?;
        run.actor_id = forged;
        store.db.execute(
            "UPDATE runs SET record=?1 WHERE id=?2",
            rusqlite::params![crate::db::bytes(&run)?, run.run_id.to_string()],
        )?;
        let seen = Cell::new(None);
        let holder = |_: &AttemptRecord, requester| {
            seen.set(Some(requester));
            Err(RunError::Validation("holder observed"))
        };
        let started = if child {
            let validated = store.validate_child(
                record.run_id,
                parent,
                &input().request.tasks[0],
                1,
                2,
                &call,
            )?;
            // Only the host's view, opened from the sealed context, launches.
            assert!(matches!(
                store.start_child(record.run_id, &validated, &Ready, &holder, &call),
                Err(RunError::Validation(
                    "child launch lacks a sealed requester"
                ))
            ));
            store = store.with_sealed_requester(sealed);
            store.start_child(record.run_id, &validated, &Ready, &holder, &call)?
        } else {
            store.start(record.run_id, &Ready, &holder, &call)?
        };
        assert!(matches!(started[..], [Started::Refused { .. }]));
        // A top-level store is host-written, so its record stays the source.
        let owner = if child { sealed } else { forged };
        assert_eq!(seen.get(), Some(owner));
        // A started child records the sealed owner. The lane's operation keeps
        // its own actor, so its replay still finds it.
        assert_eq!(store.run(record.run_id, &call)?.actor_id, owner);
        let attempt = store.record(prepared.attempts[0].attempt_id, &call)?;
        let stamped = if child { sealed } else { lane.actor_id };
        assert_eq!(attempt.verified_owner, Some(stamped));
        let operation = store
            .operation(&prepared.operation_id, &call)?
            .ok_or("lane operation")?;
        assert_eq!(operation.actor_id, lane.actor_id);
    }
    Ok(())
}

#[test]
fn host_stops_a_launched_child_from_held_scope_not_its_records() -> TestResult {
    let sealed = provenant_core::ActorId::from_bytes([7; 16]);
    let forged = provenant_core::ActorId::from_bytes([66; 16]);
    // Each rewrite once made the host skip, withdraw or misdirect the stop.
    let forgeries: [fn(&mut AttemptRecord); 3] = [
        |record| {
            record.state = AttemptState::Terminal {
                outcome: Outcome::Rejected,
                exit_code: None,
                finished_at: record.queued_at,
                reason: Some("rejected, says the lane".into()),
            };
            let mut result = record.prompt.clone();
            result.path =
                artifacts::artifact_path(record.attempt_id, "result.md").expect("result path");
            record.result = Some(result);
        },
        |record| record.state = AttemptState::Queued,
        |record| record.generation += 5,
    ];
    for forge in forgeries {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?.with_sealed_requester(sealed);
        let run = input();
        let id = run.attempts[0].attempt_id;
        store.create(&run, &call)?;
        let started = store.mark_starting(
            &store.record(id, &call)?,
            &route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let ticket: Vec<u8> = store.db.query_row(
            "SELECT record FROM admission WHERE attempt=?1",
            [id.to_string()],
            |row| row.get(0),
        )?;
        let ticket: serde_json::Value = crate::db::decode(&ticket)?;
        assert_eq!(ticket["holder"], serde_json::to_value(sealed)?);
        let scope = provenant_supervisor::custody::Scope {
            run: run.run_id,
            attempt: id,
            generation: started.generation,
        };
        // As hand-off holds it just before launching the guardian.
        hold_child_fixture(
            &store,
            store.attempt_dir(run.run_id, id, false)?,
            scope,
            sealed,
        )?;
        // The lane rewrites the record, the launch summary and the stop.
        let mut record = store.record(id, &call)?;
        forge(&mut record);
        store.db.execute(
            "UPDATE attempts SET record=?1, terminal=?2 WHERE id=?3",
            rusqlite::params![
                crate::db::bytes(&record)?,
                matches!(record.state, AttemptState::Terminal { .. }),
                id.to_string()
            ],
        )?;
        assert_eq!(store.record(id, &call)?, record);
        let dir = store.attempt_dir(run.run_id, id, false)?;
        let summary = serde_json::json!({"schema_version": 1, "scope": scope, "requester": forged});
        artifacts::seal(
            &dir,
            "launch.json",
            &serde_json::to_vec(&summary)?,
            RECORD_LIMIT,
        )?;
        let planted = serde_json::json!({
            "schema_version": 1,
            "scope": scope,
            "request_id": format!("{}.{id}.{}.cancel", run.run_id, scope.generation),
            "requester": forged,
            "reason": "explicit_cancellation",
        });
        artifacts::seal(
            &dir,
            "cancel.json",
            &serde_json::to_vec(&planted)?,
            RECORD_LIMIT,
        )?;
        assert_eq!(store.cancel(id, "stop", &call)?, CancelReceipt::Requested);
        let request: serde_json::Value = serde_json::from_slice(&std::fs::read(
            store
                .context
                .state_root
                .as_path()
                .join(format!("runs/{}/attempts/{id}/cancel.json", run.run_id)),
        )?)?;
        assert_eq!(request["requester"], serde_json::to_value(sealed)?);
        assert_eq!(request["scope"], serde_json::to_value(scope)?);
    }
    Ok(())
}

#[test]
fn held_stop_follows_the_handed_off_directory() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let sealed = provenant_core::ActorId::from_bytes([7; 16]);
    let store = RunStore::open(&context(temp.path())?, &call)?.with_sealed_requester(sealed);
    let run = input();
    let id = run.attempts[0].attempt_id;
    store.create(&run, &call)?;
    let started = store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let scope = provenant_supervisor::custody::Scope {
        run: run.run_id,
        attempt: id,
        generation: started.generation,
    };
    hold_child_fixture(
        &store,
        store.attempt_dir(run.run_id, id, false)?,
        scope,
        sealed,
    )?;
    let original = store
        .context
        .state_root
        .as_path()
        .join(format!("runs/{}/attempts/{id}", run.run_id));
    let renamed = original.with_file_name(format!("{id}-renamed"));
    std::fs::rename(&original, &renamed)?;
    std::fs::DirBuilder::new().mode(0o700).create(&original)?;

    assert_eq!(store.cancel(id, "stop", &call)?, CancelReceipt::Requested);
    let request: serde_json::Value =
        serde_json::from_slice(&std::fs::read(renamed.join("cancel.json"))?)?;
    assert_eq!(request["requester"], serde_json::to_value(sealed)?);
    assert_eq!(request["scope"], serde_json::to_value(scope)?);
    assert!(std::fs::read_dir(&original)?.next().is_none());
    Ok(())
}

#[test]
fn host_view_holds_the_scope_it_hands_to_a_guardian() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let sealed = provenant_core::ActorId::from_bytes([7; 16]);
    let store = RunStore::open(&context(temp.path())?, &call)?.with_sealed_requester(sealed);
    let mut run = input();
    // An invalid capture plan makes the launch publish never-spawned
    // evidence instead of starting a guardian.
    run.request.tasks[0]
        .profile
        .knobs
        .insert("lanes.silence_seconds".into(), 0.into());
    store.create(&run, &call)?;
    let id = run.attempts[0].attempt_id;
    assert!(store.held_attempts().is_empty());
    store.start_child(
        run.run_id,
        &ValidatedChild::fixture(run.request.clone()),
        &Ready,
        &|_, _| Ok(Vec::new()),
        &call,
    )?;
    assert_eq!(store.held_attempts(), vec![id]);
    let dir = store.attempt_dir(run.run_id, id, false)?;
    assert!(artifacts::read_sealed(&dir, "process-exit.json", RECORD_LIMIT)?.is_some());
    let scope = store.launched.borrow()[0].scope;
    let original = store
        .context
        .state_root
        .as_path()
        .join(format!("runs/{}/attempts/{id}", run.run_id));
    let renamed = original.with_file_name(format!("{id}-renamed"));
    std::fs::rename(&original, &renamed)?;
    std::fs::DirBuilder::new().mode(0o700).create(&original)?;

    assert_eq!(store.cancel(id, "stop", &call)?, CancelReceipt::Requested);
    let request: serde_json::Value =
        serde_json::from_slice(&std::fs::read(renamed.join("cancel.json"))?)?;
    assert_eq!(request["requester"], serde_json::to_value(sealed)?);
    assert_eq!(request["scope"], serde_json::to_value(scope)?);
    assert!(std::fs::read_dir(&original)?.next().is_none());
    Ok(())
}

#[test]
fn held_children_reclaim_exited_custody_across_sequential_launches() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let sealed = provenant_core::ActorId::from_bytes([7; 16]);
    let mut execution = context(temp.path())?;
    execution.actor_id = sealed;
    let store = RunStore::open(&execution, &call)?.with_sealed_requester(sealed);
    let mut runs = Vec::new();
    let handoffs = Cell::new(0);
    for index in 0..(crate::db::HELD_CHILD_LIMIT * 2 + 1) {
        let mut run = input();
        let byte = u8::try_from(index + 32)?;
        run.run_id = provenant_core::RunId::from_bytes([byte; 16]);
        run.attempts[0].attempt_id = AttemptId::from_bytes([byte; 16]);
        run.operation_id = provenant_core::OperationId::parse(&format!("child-{index}"))?;
        // Exercise real hand-off and reservation release without spawning a
        // provider. Invalid capture publishes never-spawned evidence and drops
        // the same custody guard a guardian would retain until exit.
        run.request.tasks[0]
            .profile
            .knobs
            .insert("lanes.silence_seconds".into(), 0.into());
        store.create(&run, &call)?;
        store.start_child(
            run.run_id,
            &ValidatedChild::fixture(run.request.clone()),
            &Ready,
            &|_, _| {
                handoffs.set(handoffs.get() + 1);
                Ok(Vec::new())
            },
            &call,
        )?;
        assert_eq!(handoffs.get(), index + 1, "child refused at capacity");
        runs.push(run);
    }
    {
        let held = store.launched.borrow();
        assert_eq!(held.len(), runs.len());
        assert!(held[0].handle.is_none());
        assert!(
            held.iter().filter(|held| held.handle.is_some()).count() <= crate::db::HELD_CHILD_LIMIT
        );
    }
    let first = &runs[0];
    assert_eq!(
        store.cancel(first.attempts[0].attempt_id, "stop", &call)?,
        CancelReceipt::AlreadyTerminal
    );
    assert!(
        store
            .stop_children(Default::default(), "stop", &call)
            .is_empty()
    );
    // Reclamation must retain the owner's scope, even if the lane changes
    // its record after the guardian has exited.
    let mut owner = store.run(first.run_id, &call)?;
    owner.actor_id = provenant_core::ActorId::from_bytes([66; 16]);
    store.db.execute(
        "UPDATE runs SET record=?1 WHERE id=?2",
        rusqlite::params![crate::db::bytes(&owner)?, first.run_id.to_string()],
    )?;
    assert!(matches!(
        store.run(first.run_id, &call),
        Err(RunError::Validation("child record names another owner"))
    ));
    Ok(())
}

#[test]
fn held_children_retain_busy_missing_and_recreated_custody_locks() -> TestResult {
    use provenant_supervisor::custody::PublicationReservation;
    use provenant_supervisor::custody::Scope;
    use std::ffi::OsStr;
    for state in ["busy", "missing", "recreated", "symlink", "renamed"] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let sealed = provenant_core::ActorId::from_bytes([7; 16]);
        let store = RunStore::open(&context(temp.path())?, &call)?.with_sealed_requester(sealed);
        let run = input();
        store.create(&run, &call)?;
        let directory = store.attempt_dir(run.run_id, run.attempts[0].attempt_id, false)?;
        let reservation =
            PublicationReservation::prepare(directory.try_clone()?, call.budget, &cancel, &clock)?;
        let identity = reservation.custody_metadata()?;
        for index in 0..crate::db::HELD_CHILD_LIMIT {
            store.hold_child(
                provenant_supervisor::custody::hold(
                    directory.try_clone()?,
                    Scope {
                        run: run.run_id,
                        attempt: AttemptId::from_bytes([u8::try_from(index + 32)?; 16]),
                        generation: 1,
                    },
                    sealed,
                ),
                identity,
            );
        }
        let lock = OsStr::new("custody.lock");
        let path = store.context.state_root.as_path().join(format!(
            "runs/{}/attempts/{}",
            run.run_id, run.attempts[0].attempt_id
        ));
        if state != "busy" && state != "renamed" {
            // The lane is not constrained by the supervisor's refusal to
            // unlink reserved lock names.
            std::fs::remove_file(path.join("custody.lock"))?;
        }
        if state == "recreated" {
            let replacement = directory.try_lock(lock, provenant_os::fs::LockMode::Exclusive)?;
            assert!(!identity.same_identity(&replacement.metadata()?));
        } else if state == "symlink" {
            directory.try_lock(
                OsStr::new("other.lock"),
                provenant_os::fs::LockMode::Exclusive,
            )?;
            std::os::unix::fs::symlink("other.lock", path.join("custody.lock"))?;
        } else if state == "renamed" {
            std::fs::rename(&path, path.with_file_name("renamed-attempt"))?;
            std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        }
        assert!(
            matches!(
                store.check_held_capacity(run.attempts[0].attempt_id),
                Err(RunError::Validation(
                    "held child custody limit reached; earlier children are still running"
                ))
            ),
            "{state}"
        );
        assert!(
            store
                .launched
                .borrow()
                .iter()
                .all(|held| held.handle.is_some()),
            "{state}"
        );
        if state == "missing" {
            assert!(
                directory
                    .metadata_at(lock, false)
                    .is_err_and(|error| error.is_not_found())
            );
        }
        drop(reservation);
        if state == "busy" || state == "renamed" {
            // The unchanged inode becomes releasable only after its owner
            // drops the inherited custody guard.
            store.check_held_capacity(run.attempts[0].attempt_id)?;
            assert!(
                store
                    .launched
                    .borrow()
                    .iter()
                    .all(|held| held.handle.is_none())
            );
        }
    }
    Ok(())
}

#[test]
fn held_children_are_bounded_and_superseded_handles_are_dropped() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let sealed = provenant_core::ActorId::from_bytes([7; 16]);
    let mut execution = context(temp.path())?;
    execution.actor_id = sealed;
    let store = RunStore::open(&execution, &call)?.with_sealed_requester(sealed);
    let run = input();
    let id = run.attempts[0].attempt_id;
    store.create(&run, &call)?;
    let directory = store.attempt_dir(run.run_id, id, false)?;
    for index in 0..crate::db::HELD_CHILD_LIMIT {
        hold_child_fixture(
            &store,
            directory.try_clone()?,
            provenant_supervisor::custody::Scope {
                run: run.run_id,
                attempt: AttemptId::from_bytes([u8::try_from(index + 32)?; 16]),
                generation: 1,
            },
            sealed,
        )?;
    }
    let custody = directory.try_lock(
        std::ffi::OsStr::new("custody.lock"),
        provenant_os::fs::LockMode::Exclusive,
    )?;
    let mut scope = store.launched.borrow()[0].scope;
    // A replacement is allowed even at the cap. Its predecessor retains
    // the scope needed for owner checks, but no longer retains a descriptor.
    store.check_held_capacity(scope.attempt)?;
    scope.generation += 1;
    store.hold_child(
        provenant_supervisor::custody::hold(directory.try_clone()?, scope, sealed),
        custody.metadata()?,
    );
    {
        let held = store.launched.borrow();
        assert_eq!(held[0].scope.generation, 1);
        assert!(held[0].handle.is_none());
        assert_eq!(
            held.iter().filter(|held| held.handle.is_some()).count(),
            crate::db::HELD_CHILD_LIMIT
        );
    }
    assert_eq!(
        store.cancel(scope.attempt, "stop", &call)?,
        CancelReceipt::Requested
    );
    let request: serde_json::Value = serde_json::from_slice(
        &artifacts::read_sealed(&directory, "cancel.json", RECORD_LIMIT)?.ok_or("missing stop")?,
    )?;
    assert_eq!(request["scope"], serde_json::to_value(scope)?);

    let started = store.start_child(
        run.run_id,
        &ValidatedChild::fixture(run.request.clone()),
        &Ready,
        &|_, _| {
            Err(RunError::Validation(
                "holder context must not be built at capacity",
            ))
        },
        &call,
    )?;
    let record = store.record(id, &call)?;
    assert_eq!(record.state, AttemptState::Queued);
    assert_eq!(record.generation, 0);
    assert!(
        !store
            .launched_tasks
            .borrow()
            .contains(&(run.run_id, record.task_id))
    );
    assert!(matches!(started.as_slice(), [Started::Deferred(attempt)] if *attempt == id));
    assert!(!store.held_attempts().contains(&id));
    assert!(artifacts::read_sealed(&directory, "launch.json", RECORD_LIMIT)?.is_none());
    // Superseded scopes still prevent an owner forged in the run record.
    let mut owner = store.run(run.run_id, &call)?;
    owner.actor_id = provenant_core::ActorId::from_bytes([66; 16]);
    store.db.execute(
        "UPDATE runs SET record=?1 WHERE id=?2",
        rusqlite::params![crate::db::bytes(&owner)?, run.run_id.to_string()],
    )?;
    assert!(matches!(
        store.run(run.run_id, &call),
        Err(RunError::Validation("child record names another owner"))
    ));
    Ok(())
}

#[test]
fn deferred_child_launches_once_capacity_frees() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let sealed = provenant_core::ActorId::from_bytes([2; 16]);
    let store = RunStore::open(&context(temp.path())?, &call)?.with_sealed_requester(sealed);
    let parent = AttemptId::from_bytes([9; 16]);
    let mut run = input();
    run.request.lane_depth = Some(2);
    run.request.parent_attempt = Some(parent);
    let id = run.attempts[0].attempt_id;
    store.create(&run, &call)?;
    let validated =
        store.validate_child(run.run_id, parent, &input().request.tasks[0], 1, 2, &call)?;
    let directory = store.attempt_dir(run.run_id, id, false)?;
    for index in 0..crate::db::HELD_CHILD_LIMIT {
        hold_child_fixture(
            &store,
            directory.try_clone()?,
            provenant_supervisor::custody::Scope {
                run: run.run_id,
                attempt: AttemptId::from_bytes([u8::try_from(index + 32)?; 16]),
                generation: 1,
            },
            sealed,
        )?;
    }
    // The fixture handles share one directory. Keep its custody guard busy
    // until simulating the guardian's exit, rather than changing any row.
    let custody = directory.try_lock(
        std::ffi::OsStr::new("custody.lock"),
        provenant_os::fs::LockMode::Exclusive,
    )?;
    let fake = Fake::default();
    for _ in 0..2 {
        let started =
            store.start_child(run.run_id, &validated, &fake, &|_, _| unreachable!(), &call)?;
        assert!(matches!(started.as_slice(), [Started::Deferred(attempt)] if *attempt == id));
        assert_eq!(store.record(id, &call)?.state, AttemptState::Queued);
        assert_eq!(store.record(id, &call)?.generation, 0);
        assert_eq!(store.queued_runs(&call)?, vec![run.run_id]);
    }
    assert_eq!(fake.prepared.get(), 0);
    assert!(
        !store
            .launched_tasks
            .borrow()
            .contains(&(run.run_id, run.request.tasks[0].task_id))
    );
    drop(custody);
    let validated =
        store.validate_child(run.run_id, parent, &input().request.tasks[0], 1, 2, &call)?;
    let started = store.start_child(
        run.run_id,
        &validated,
        &Ready,
        &|_, _| Err(RunError::Validation("fixture observed handoff")),
        &call,
    )?;
    assert!(
        matches!(started.as_slice(), [Started::Refused { attempt, reason, .. }]
        if *attempt == id && reason.contains("fixture observed handoff"))
    );
    assert_eq!(store.record(id, &call)?.generation, 1);
    assert!(
        store
            .launched_tasks
            .borrow()
            .contains(&(run.run_id, run.request.tasks[0].task_id))
    );
    assert!(
        store
            .launched
            .borrow()
            .iter()
            .all(|held| held.handle.is_none())
    );
    Ok(())
}

#[test]
fn deferral_stops_the_scan_after_the_first_attempt() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let sealed = provenant_core::ActorId::from_bytes([2; 16]);
    let store = RunStore::open(&context(temp.path())?, &call)?.with_sealed_requester(sealed);
    let run = two_tasks(2);
    store.create(&run, &call)?;
    let directory = store.attempt_dir(run.run_id, run.attempts[0].attempt_id, false)?;
    for index in 0..crate::db::HELD_CHILD_LIMIT {
        hold_child_fixture(
            &store,
            directory.try_clone()?,
            provenant_supervisor::custody::Scope {
                run: run.run_id,
                attempt: AttemptId::from_bytes([u8::try_from(index + 32)?; 16]),
                generation: 1,
            },
            sealed,
        )?;
    }
    let _custody = directory.try_lock(
        std::ffi::OsStr::new("custody.lock"),
        provenant_os::fs::LockMode::Exclusive,
    )?;
    let fake = Fake::default();
    let started = store.start_child(
        run.run_id,
        &ValidatedChild::fixture(run.request.clone()),
        &fake,
        &|_, _| unreachable!(),
        &call,
    )?;
    assert!(matches!(started.as_slice(), [Started::Deferred(attempt)]
        if *attempt == run.attempts[0].attempt_id));
    assert_eq!(fake.prepared.get(), 0);
    for attempt in &run.attempts {
        let record = store.record(attempt.attempt_id, &call)?;
        assert_eq!(record.state, AttemptState::Queued);
        assert_eq!(record.generation, 0);
        assert!(
            !store
                .launched_tasks
                .borrow()
                .contains(&(run.run_id, record.task_id))
        );
    }
    Ok(())
}

#[test]
fn host_stops_held_children_first_then_accepted_ones_it_never_held() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let sealed = provenant_core::ActorId::from_bytes([7; 16]);
    let store = RunStore::open(&context(temp.path())?, &call)?.with_sealed_requester(sealed);
    let batch = two_tasks(2);
    store.create(&batch, &call)?;
    let [queued, late] = [batch.attempts[0].attempt_id, batch.attempts[1].attempt_id];
    // The parent's scan accepted only the queued child. A start launched
    // the other after that snapshot, so only hand-off holds it.
    let started = store.mark_starting(
        &store.record(late, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let scope = provenant_supervisor::custody::Scope {
        run: batch.run_id,
        attempt: late,
        generation: started.generation,
    };
    hold_child_fixture(
        &store,
        store.attempt_dir(batch.run_id, late, false)?,
        scope,
        sealed,
    )?;
    let accepted = std::collections::BTreeSet::from([(queued, 1)]);
    assert!(store.stop_children(accepted, "stop", &call).is_empty());
    let request: serde_json::Value = serde_json::from_slice(&std::fs::read(
        store
            .context
            .state_root
            .as_path()
            .join(format!("runs/{}/attempts/{late}/cancel.json", batch.run_id)),
    )?)?;
    assert_eq!(request["requester"], serde_json::to_value(sealed)?);
    assert_eq!(request["scope"], serde_json::to_value(scope)?);
    assert_eq!(terminal(&store, queued, &call)?, Outcome::Cancelled);
    Ok(())
}

#[test]
fn host_view_refuses_a_launched_child_renamed_after_start() -> TestResult {
    let sealed = provenant_core::ActorId::from_bytes([7; 16]);
    let forged = provenant_core::ActorId::from_bytes([66; 16]);
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?.with_sealed_requester(sealed);
    let run = input();
    let id = run.attempts[0].attempt_id;
    store.create(&run, &call)?;
    let started = store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    // Before hand-off the host only stamped the owner; nothing is held yet.
    let mut record = store.record(id, &call)?;
    record.verified_owner = Some(forged);
    let rename = |record: &AttemptRecord| {
        store.db.execute(
            "UPDATE attempts SET record=?1 WHERE id=?2",
            rusqlite::params![crate::db::bytes(record)?, id.to_string()],
        )?;
        TestResult::Ok(())
    };
    rename(&record)?;
    assert!(store.record(id, &call).is_ok());
    hold_child_fixture(
        &store,
        store.attempt_dir(run.run_id, id, false)?,
        provenant_supervisor::custody::Scope {
            run: run.run_id,
            attempt: id,
            generation: started.generation,
        },
        sealed,
    )?;
    let renamed = |result: Result<()>| {
        matches!(
            result,
            Err(RunError::Validation("child record names another owner"))
        )
    };
    assert!(renamed(store.record(id, &call).map(drop)));
    // The stamped owner reads again. A renamed run refuses every read,
    // including the terminal fact that carries the owner.
    rename(&started)?;
    assert!(store.record(id, &call).is_ok());
    let mut owner = store.run(run.run_id, &call)?;
    owner.actor_id = forged;
    store.db.execute(
        "UPDATE runs SET record=?1 WHERE id=?2",
        rusqlite::params![crate::db::bytes(&owner)?, run.run_id.to_string()],
    )?;
    assert!(renamed(store.run(run.run_id, &call).map(drop)));
    assert!(renamed(store.record(id, &call).map(drop)));
    assert!(renamed(
        crate::AttemptReader::attempt(&store, id, &call).map(drop)
    ));
    Ok(())
}

struct Host {
    available: u64,
}

#[test]
fn memory_pressure_defers_even_when_available_memory_is_high() -> TestResult {
    use provenant_os::sys::resources::MemoryPressure;
    struct Pressured(MemoryPressure);
    impl Resources for Pressured {
        fn memory(&self) -> std::io::Result<MemorySample> {
            Host { available: 100 }.memory()
        }
        fn pressure(
            &self,
        ) -> std::io::Result<Option<provenant_os::sys::resources::MemoryPressure>> {
            Ok(Some(self.0))
        }
        fn disk(&self, _: &Directory) -> std::io::Result<u64> {
            Ok(u64::MAX)
        }
    }
    for (level, writer) in [
        (MemoryPressure::Warning, true),
        (MemoryPressure::Critical, false),
        (MemoryPressure::Normal, true),
    ] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let mut store = RunStore::open(&context(temp.path())?, &call)?;
        store.set_admission_policy(AdmissionPolicy {
            admission_seconds: 1,
            ..AdmissionPolicy::default()
        });
        let run = lane(54, writer);
        let id = starting(&store, &run, &call)?;
        let attempt = store.attempt_dir(run.run_id, id, false)?;
        if level != MemoryPressure::Normal {
            let waits = store.lane_waits_with_resources(
                &[store.record(id, &call)?],
                &call,
                &Pressured(level),
            );
            assert!(
                waits[0]
                    .waiting_for
                    .contains(&format!("memory pressure {level:?}"))
            );
        }
        let result = store.admit(id, &attempt, &Pressured(level), &clock, &cancel);
        if level == MemoryPressure::Normal {
            assert_eq!(result?.state, AttemptState::Running);
        } else {
            assert!(
                matches!(result, Err(RunError::MemoryPressure { level: actual }) if actual == format!("{level:?}"))
            );
        }
    }
    Ok(())
}

#[test]
fn pressure_errors_fall_back_and_normal_still_obeys_floor() -> TestResult {
    struct Probe {
        available: u64,
        error: bool,
    }
    impl Resources for Probe {
        fn memory(&self) -> std::io::Result<MemorySample> {
            Host {
                available: self.available,
            }
            .memory()
        }
        fn pressure(
            &self,
        ) -> std::io::Result<Option<provenant_os::sys::resources::MemoryPressure>> {
            if self.error {
                Err(std::io::Error::other("unavailable"))
            } else {
                Ok(Some(provenant_os::sys::resources::MemoryPressure::Normal))
            }
        }
        fn disk(&self, _: &Directory) -> std::io::Result<u64> {
            Ok(u64::MAX)
        }
    }
    for (available, error, admitted) in [(100, true, true), (4, false, false)] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let mut store = RunStore::open(&context(temp.path())?, &call)?;
        store.set_admission_policy(AdmissionPolicy {
            admission_seconds: 1,
            ..AdmissionPolicy::default()
        });
        let run = lane(58, true);
        let id = starting(&store, &run, &call)?;
        let attempt = store.attempt_dir(run.run_id, id, false)?;
        let result = store.admit(id, &attempt, &Probe { available, error }, &clock, &cancel);
        assert_eq!(result.is_ok(), admitted);
    }
    Ok(())
}

#[test]
fn invalid_build_settings_refuse_without_waiting() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = lane(69, true);
    let id = starting(&store, &run, &call)?;
    let attempt = store.attempt_dir(run.run_id, id, false)?;
    for assignment in [
        "max_concurrent_builds = 0",
        "cargo_build_jobs = -2",
        "cargo_build_jobs = 'invalid'",
    ] {
        std::fs::write(
            store.writer_registry.join("config.toml"),
            format!("[domains.fabric.lanes]\n{assignment}\n"),
        )?;
        let now = clock.wall_now()?;
        assert!(
            matches!(
                store.admit(id, &attempt, &Host { available: 100 }, &clock, &cancel),
                Err(RunError::BuildSettings)
            ),
            "{assignment}"
        );
        assert_eq!(clock.wall_now()?, now);
    }
    Ok(())
}

#[test]
fn guardian_identity_unknown_retries_and_names_expiry() -> TestResult {
    struct Observation {
        remaining: Cell<u32>,
    }
    impl Resources for Observation {
        fn memory(&self) -> std::io::Result<MemorySample> {
            Host { available: 100 }.memory()
        }
        fn disk(&self, _: &Directory) -> std::io::Result<u64> {
            Ok(u64::MAX)
        }
        fn guardian_identity(&self) -> Option<provenant_supervisor::census::ProcessIdentity> {
            let remaining = self.remaining.get();
            if remaining > 0 {
                self.remaining.set(remaining - 1);
                None
            } else {
                SystemResources.guardian_identity()
            }
        }
    }
    for (failures, admitted) in [(2, true), (100, false)] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let mut store = RunStore::open(&context(temp.path())?, &call)?;
        store.set_admission_policy(AdmissionPolicy {
            startup_spacing_ms: 0,
            admission_seconds: 1,
            ..AdmissionPolicy::default()
        });
        let run = lane(70, true);
        let id = starting(&store, &run, &call)?;
        let attempt = store.attempt_dir(run.run_id, id, false)?;
        let result = store.admit(
            id,
            &attempt,
            &Observation {
                remaining: Cell::new(failures),
            },
            &clock,
            &cancel,
        );
        if admitted {
            assert_eq!(result?.state, AttemptState::Running);
        } else {
            assert!(matches!(result, Err(RunError::GuardianIdentityUnavailable)));
        }
    }
    Ok(())
}

#[test]
fn idle_writers_are_admitted_beyond_the_build_limit() -> TestResult {
    // `max_concurrent_builds` gates builds inside lanes, never writer lanes.
    for limit in [None, Some(1)] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let mut store = RunStore::open(&context(temp.path())?, &call)?;
        store.set_admission_policy(AdmissionPolicy {
            startup_spacing_ms: 0,
            admission_seconds: 1,
            ..AdmissionPolicy::default()
        });
        if let Some(limit) = limit {
            std::fs::write(
                store.writer_registry.join("config.toml"),
                format!("[domains.fabric.lanes]\nmax_concurrent_builds = {limit}\n"),
            )?;
        }
        for seed in 60..65 {
            let run = lane(seed, true);
            let id = starting(&store, &run, &call)?;
            let dir = store.attempt_dir(run.run_id, id, false)?;
            assert_eq!(
                store
                    .admit(id, &dir, &Host { available: 100 }, &clock, &cancel)?
                    .state,
                AttemptState::Running,
                "writer {seed} with limit {limit:?}"
            );
        }
    }
    Ok(())
}

#[test]
fn a_running_lane_reports_a_wait_for_a_build_slot() -> TestResult {
    use provenant_os::build_slots::BuildSlots;
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let mut store = RunStore::open(&context(temp.path())?, &call)?;
    store.set_admission_policy(AdmissionPolicy {
        startup_spacing_ms: 0,
        ..AdmissionPolicy::default()
    });
    let run = lane(60, true);
    let id = starting(&store, &run, &call)?;
    assert!(store.claim(id, &clock, &call)?.is_some());
    let running = store.record(id, &call)?;
    let waits = |store: &RunStore| {
        store.lane_waits_with_resources(&[running.clone()], &call, &Host { available: 100 })
    };
    assert!(
        waits(&store).is_empty(),
        "an idle running lane waits for nothing"
    );
    // The host has total memory 100 bytes, so one build slot. Another lane's
    // build holds it while this lane's cargo waits.
    let state = store.writer_registry.clone();
    let slots = BuildSlots::open(&state)?;
    let held = slots.try_acquire(1)?.ok_or("slot free")?;
    let waiter_cancel = CancelToken::new();
    let attempt = id.to_string();
    std::thread::scope(|scope| -> TestResult {
        let waiter = scope.spawn(|| {
            let slots = BuildSlots::open(&state).map_err(|error| error.to_string())?;
            slots
                .acquire(1, Some(&attempt), &waiter_cancel, &mut |_| {})
                .map(|_| ())
                .map_err(|error| error.to_string())
        });
        let mut seen = Vec::new();
        for _ in 0..200 {
            seen = waits(&store);
            if !seen.is_empty() {
                break;
            }
            provenant_os::sys::wait_pause(std::time::Duration::from_millis(25));
        }
        assert_eq!(seen.len(), 1, "the waiting lane is reported");
        assert_eq!(seen[0].waiting_for, "waiting for build slot: 1 of 1 in use");
        // Releasing the slot ends the wait, and with it the report.
        drop(held);
        waiter.join().map_err(|_| "waiter panicked")??;
        Ok(())
    })?;
    assert!(waits(&store).is_empty());
    Ok(())
}
impl Resources for Host {
    fn memory(&self) -> std::io::Result<MemorySample> {
        Ok(MemorySample {
            available_bytes: self.available,
            total_bytes: 100,
        })
    }
    fn disk(&self, _: &Directory) -> std::io::Result<u64> {
        Ok(u64::MAX)
    }
}
#[derive(Default)]
pub(super) struct Fake {
    network: Cell<Option<NetworkAccess>>,
    refuse: bool,
    policy_task: Option<TaskId>,
    observed: Option<RouteRecord>,
    decoded: Cell<usize>,
    prepared: Cell<usize>,
    resumed: Cell<Option<AttemptId>>,
    prompt: RefCell<String>,
}

#[test]
fn policy_refusal_skips_provider_preparation_and_preserves_sibling_progress() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let batch = two_tasks(2);
    store.create(&batch, &call)?;
    let providers = Fake {
        refuse: true,
        policy_task: Some(batch.request.tasks[0].task_id),
        ..Default::default()
    };
    let started = store.start(batch.run_id, &providers, &|_, _| unreachable!(), &call)?;
    assert_eq!(providers.prepared.get(), 1);
    assert!(matches!(started.as_slice(), [
        Started::Refused { attempt: first, kind: RefusalKind::Policy, reason },
        Started::Refused { attempt: second, kind: RefusalKind::Preparation, .. },
    ] if *first == batch.attempts[0].attempt_id && *second == batch.attempts[1].attempt_id
        && reason.contains("provenant gc")));
    for attempt in &batch.attempts {
        assert_eq!(
            terminal(&store, attempt.attempt_id, &call)?,
            Outcome::Rejected
        );
    }
    Ok(())
}

impl Providers for Fake {
    fn check_policy(&self, task: &TaskRequest) -> std::result::Result<(), String> {
        if self.policy_task == Some(task.task_id) {
            return Err("insufficient disk space; run `provenant gc`".into());
        }
        Ok(())
    }

    fn prepare(
        &self,
        task: &TaskRequest,
        prompt: &str,
        paths: &LaunchPaths,
        resume: Option<&AttemptRecord>,
    ) -> std::result::Result<PreparedLaunch, String> {
        self.network
            .set(Some(task.access.network(task.writer.is_some())));
        self.prepared.set(self.prepared.get() + 1);
        assert!(paths.temporary.ends_with("tmp"));
        assert!(paths.temporary.is_dir());
        self.prompt.replace(prompt.into());
        self.resumed.set(resume.map(|r| r.attempt_id));
        if self.refuse {
            return Err("fixture adapter is disabled".into());
        }
        unreachable!("these tests never hand off to a guardian")
    }
    fn decode(
        &self,
        applied: &RouteRecord,
        stdout: &[u8],
        _: &ExitObservation,
        _: &std::path::Path,
    ) -> std::result::Result<Decoded, String> {
        assert_eq!(applied, &route());
        self.decoded.set(self.decoded.get() + 1);
        Ok(Decoded {
            outcome: Outcome::Succeeded,
            result: String::from_utf8_lossy(stdout).into_owned(),
            complete: true,
            observed_route: self.observed.clone(),
            usage: UsageRecord::default(),
            session: None,
            reason: None,
        })
    }
}
fn two_tasks(concurrency: u64) -> PreparedRun {
    let mut run = input();
    let mut task = run.request.tasks[0].clone();
    task.task_id = TaskId::from_bytes([7; 16]);
    run.request.tasks.push(task.clone());
    run.request.concurrency = concurrency;
    run.attempts.push(NewAttempt {
        task_id: task.task_id,
        attempt_id: AttemptId::from_bytes([8; 16]),
        prompt: "exact prompt".into(),
    });
    run
}
pub(super) fn seal_exit(store: &RunStore, exit: &ExitObservation) -> TestResult {
    let mut value = serde_json::to_value(exit)?;
    let object = value.as_object_mut().ok_or("exit fixture")?;
    object.remove("publication");
    object.insert("schema_version".into(), 1.into());
    let dir = store.attempt_dir(exit.scope.run, exit.scope.attempt, false)?;
    artifacts::seal(
        &dir,
        "process-exit.json",
        &serde_json::to_vec(&value)?,
        RECORD_LIMIT,
    )?;
    Ok(())
}

#[test]
fn skipped_fallback_candidates_are_recorded_on_the_attempt() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    store.create(&run, &call)?;
    let id = run.attempts[0].attempt_id;
    let skipped = vec![SkippedRoute {
        route: "claude".into(),
        kind: SkipKind::AuthRequired,
        detail: "account work: sign in again".into(),
    }];
    store.record_skipped_routes(id, &skipped, &call)?;
    let started = store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    assert_eq!(started.skipped_routes, skipped);
    assert_eq!(store.record(id, &call)?.skipped_routes, skipped);
    Ok(())
}

#[test]
fn admission_holds_run_concurrency_spacing_floors_and_deadline() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let mut store = RunStore::open(&context(temp.path())?, &call)?;
    let now = at_micros(100);

    store.set_admission_policy(AdmissionPolicy {
        startup_spacing_ms: 0,
        read_only_spacing_ms: 0,
        ..AdmissionPolicy::default()
    });
    let serial = two_tasks(1);
    store.create(&serial, &call)?;
    let [first, second] = [serial.attempts[0].attempt_id, serial.attempts[1].attempt_id];
    for id in [first, second] {
        store.mark_starting(
            &store.record(id, &call)?,
            &route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
    }
    let running = store
        .claim(first, &now, &call)?
        .ok_or("first not admitted")?;
    assert_eq!(running.state, AttemptState::Running);
    assert_eq!(running.started_at, Some(now.wall_now()?));
    assert!(
        store.claim(second, &now, &call)?.is_none(),
        "run concurrency 1"
    );

    store.set_admission_policy(AdmissionPolicy::default());
    let mut spaced = two_tasks(2);
    spaced.run_id = provenant_core::RunId::from_bytes([10; 16]);
    spaced.operation_id = provenant_core::OperationId::parse("spaced")?;
    spaced.attempts[0].attempt_id = AttemptId::from_bytes([11; 16]);
    spaced.attempts[1].attempt_id = AttemptId::from_bytes([12; 16]);
    store.create(&spaced, &call)?;
    let id = spaced.attempts[0].attempt_id;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    assert!(
        store.claim(id, &now, &call)?.is_none(),
        "2 s read-only host spacing"
    );

    // A one-second window from the queued time, 100 microseconds, so the
    // starved wait pauses on the virtual clock until the deadline.
    store.set_admission_policy(AdmissionPolicy {
        admission_seconds: 1,
        ..AdmissionPolicy::default()
    });
    let mut expired = input();
    expired.run_id = provenant_core::RunId::from_bytes([13; 16]);
    expired.operation_id = provenant_core::OperationId::parse("expired")?;
    expired.attempts[0].attempt_id = AttemptId::from_bytes([14; 16]);
    store.create(&expired, &call)?;
    let id = expired.attempts[0].attempt_id;
    let attempt = store.attempt_dir(expired.run_id, id, false)?;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let starved = Host { available: 1 };
    assert!(matches!(
        store.admit(id, &attempt, &starved, &clock, &cancel),
        Err(RunError::Refused)
    ));
    // The clock moved only by the waits, and stopped at the deadline.
    assert_eq!(clock.wall_now()?, WallTime::from_micros(1_000_100)?);
    assert_eq!(clock.mono_now(), MonoTime::from_micros(1_000_000));
    Ok(())
}

/// One-task run with its own IDs: a writer when `writer`, else read-only.
fn lane(seed: u8, writer: bool) -> PreparedRun {
    let mut run = batch(seed, 1);
    if writer {
        run.request.tasks[0].writer = Some(WriterRequest {
            project: "/project".into(),
            worktree: format!("/project/.worktrees/lane-{seed}").into(),
        });
    }
    run
}
/// Create and start `run`, returning its only attempt ready to claim.
fn starting(store: &RunStore, run: &PreparedRun, call: &Request<'_>) -> Result<AttemptId> {
    store.create(run, call)?;
    let id = run.attempts[0].attempt_id;
    store.mark_starting(
        &store.record(id, call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        call,
    )?;
    Ok(id)
}
/// A clock stopped at this time on both its wall and its host monotonic
/// coordinate, to start a claim at an exact moment.
fn at_micros(micros: i64) -> TestClock {
    TestClock::new(
        WallTime::from_micros(micros).unwrap(),
        MonoTime::from_micros(u64::try_from(micros).unwrap()),
    )
}
fn at(ms: i64) -> TestClock {
    at_micros(ms * 1000)
}

#[test]
fn read_only_lane_is_admitted_inside_a_writers_spacing_window() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let writer = starting(&store, &lane(20, true), &call)?;
    let reader = starting(&store, &lane(21, false), &call)?;
    assert!(store.claim(writer, &at(10_000), &call)?.is_some());
    assert!(
        store.claim(reader, &at(11_000), &call)?.is_some(),
        "a writer that started 1 s ago does not space a read-only lane"
    );
    Ok(())
}

#[test]
fn writers_stay_20_seconds_apart_and_read_only_starts_do_not_reset_the_clock() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let first = starting(&store, &lane(30, true), &call)?;
    let second = starting(&store, &lane(31, true), &call)?;
    let third = starting(&store, &lane(32, true), &call)?;
    let reader = starting(&store, &lane(33, false), &call)?;
    assert!(store.claim(first, &at(0), &call)?.is_some());
    assert!(
        store.claim(second, &at(19_999), &call)?.is_none(),
        "20 s between writers"
    );
    assert!(store.claim(reader, &at(20_000), &call)?.is_some());
    assert!(
        store.claim(second, &at(20_001), &call)?.is_some(),
        "measured from the last writer, not the read-only start at 20 s"
    );
    assert!(store.claim(third, &at(40_000), &call)?.is_none());
    assert!(store.claim(third, &at(40_001), &call)?.is_some());
    Ok(())
}

#[test]
fn read_only_lanes_are_spaced_2_seconds_apart() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let first = starting(&store, &lane(40, false), &call)?;
    let second = starting(&store, &lane(41, false), &call)?;
    let third = starting(&store, &lane(42, false), &call)?;
    assert!(store.claim(first, &at(0), &call)?.is_some());
    assert!(
        store.claim(second, &at(1_999), &call)?.is_none(),
        "a burst of provider launches is still staggered"
    );
    assert!(store.claim(second, &at(2_000), &call)?.is_some());
    assert!(store.claim(third, &at(3_999), &call)?.is_none());
    assert!(store.claim(third, &at(4_000), &call)?.is_some());
    Ok(())
}

#[test]
fn the_store_accepts_run_concurrency_above_the_task_count_up_to_sixteen() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let mut run = lane(90, false);
    run.request.concurrency = 16;
    store.create(&run, &call)?;
    assert_eq!(store.request(run.run_id, &call)?.concurrency, 16);
    let mut wider = lane(91, false);
    wider.request.concurrency = 17;
    assert!(matches!(
        store.create(&wider, &call),
        Err(RunError::Validation(_))
    ));
    Ok(())
}

#[test]
fn a_nested_lane_reads_class_spacing_from_the_host_ledger_only_with_the_host_root() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let host = context(temp.path())?;
    // A nested lane's store is a child directory with no ledger of its own.
    let mut inputs =
        provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
    inputs.state_dir = Some(host.state_root.as_path().join("children").join("lane"));
    let nested = ExecutionContext {
        state_root: inputs.resolve(&provenant_os::fs::SystemPathResolver)?.state,
        ..host.clone()
    };
    let mut writable =
        RunStore::open(&nested, &call)?.with_writer_registry(host.state_root.as_path());
    writable.set_admission_policy(AdmissionPolicy {
        read_only_spacing_ms: 600_000,
        ..AdmissionPolicy::default()
    });
    let (first, second) = (lane(110, false), lane(111, false));
    let first_id = starting(&writable, &first, &call)?;
    let second_id = starting(&writable, &second, &call)?;
    let attempt = writable.attempt_dir(first.run_id, first_id, false)?;
    writable.admit(
        first_id,
        &attempt,
        &Host { available: 100 },
        &clock,
        &cancel,
    )?;
    let waiting = writable.record(second_id, &call)?;
    let waits = |registry: Option<&std::path::Path>| -> Result<String> {
        let reader = RunStore::open_read_only(&nested, &call)?.ok_or(RunError::NotFound)?;
        let reader = match registry {
            Some(host) => reader.with_writer_registry(host),
            None => reader,
        };
        let waits = reader.lane_waits(std::slice::from_ref(&waiting), &call);
        Ok(waits
            .first()
            .map(|w| w.waiting_for.clone())
            .unwrap_or_default())
    };
    assert!(
        waits(Some(host.state_root.as_path()))?.starts_with("read-only startup spacing: "),
        "the host root shows the class's spacing wait"
    );
    assert!(
        waits(None)?.starts_with("host capacity"),
        "the child directory has no ledger, so the wait goes unseen"
    );
    Ok(())
}

/// A clock a test steps by hand. The wall clock, the host's monotonic clock and
/// the boot identity each move on their own, as they do on a real host; waits
/// move the wall and monotonic clocks together like `TestClock`.
struct Stepped {
    inner: TestClock,
    wall_offset_micros: Cell<i64>,
    host_base_micros: Cell<u64>,
    boot: RefCell<&'static str>,
    host_readable: Cell<bool>,
    boot_readable: Cell<bool>,
}
impl Stepped {
    /// A host that has been up `host_ms`, in boot `"boot-a"`.
    fn up(host_ms: u64) -> Self {
        Self {
            inner: clock(),
            wall_offset_micros: Cell::new(0),
            host_base_micros: Cell::new(host_ms * 1000),
            boot: RefCell::new("boot-a"),
            host_readable: Cell::new(true),
            boot_readable: Cell::new(true),
        }
    }
    /// The wall clock alone steps, as an operator or NTP can make it.
    fn step_wall_ms(&self, ms: i64) {
        self.wall_offset_micros
            .set(self.wall_offset_micros.get() + ms * 1000);
    }
    /// Real time passes: the wall and host clocks both move.
    fn pass_micros(&self, micros: u64) {
        self.host_base_micros
            .set(self.host_base_micros.get() + micros);
        self.wall_offset_micros
            .set(self.wall_offset_micros.get() + i64::try_from(micros).unwrap_or(0));
    }
    fn pass_ms(&self, ms: u64) {
        self.pass_micros(ms * 1000);
    }
    /// The host restarts: a new boot whose clock has run `host_ms`.
    fn reboot(&self, boot: &'static str, host_ms: u64) {
        *self.boot.borrow_mut() = boot;
        self.host_base_micros
            .set(host_ms * 1000 - self.inner.mono_now().as_micros());
    }
}
impl Clock for Stepped {
    fn wall_now(&self) -> std::result::Result<WallTime, provenant_core::TimeError> {
        WallTime::from_micros(self.inner.wall_now()?.as_micros() + self.wall_offset_micros.get())
    }
    fn mono_now(&self) -> MonoTime {
        self.inner.mono_now()
    }
    fn wait_until(&self, deadline: MonoTime) -> std::result::Result<(), provenant_core::TimeError> {
        self.inner.wait_until(deadline)
    }
    fn host_monotonic_micros(&self) -> std::result::Result<u64, provenant_core::TimeError> {
        if !self.host_readable.get() {
            return Err(provenant_core::TimeError::HostClock);
        }
        Ok(self.inner.mono_now().as_micros() + self.host_base_micros.get())
    }
    fn boot_identity(&self) -> std::result::Result<String, provenant_core::TimeError> {
        if !self.boot_readable.get() {
            return Err(provenant_core::TimeError::HostClock);
        }
        Ok((*self.boot.borrow()).to_owned())
    }
}
fn stepped_call<'a>(clock: &'a Stepped, cancel: &'a CancelToken) -> Request<'a> {
    Request {
        clock,
        budget: provenant_core::WaitBudget::cli(MonoTime::from_micros(0), 10).unwrap(),
        cancel,
    }
}
/// Writer lanes ready to claim, each a lane of its own run.
fn writers<const N: usize>(
    store: &RunStore,
    seeds: [u8; N],
    call: &Request<'_>,
) -> Result<[AttemptId; N]> {
    let mut ids = [AttemptId::from_bytes([0; 16]); N];
    for (id, seed) in ids.iter_mut().zip(seeds) {
        *id = starting(store, &lane(seed, true), call)?;
    }
    Ok(ids)
}

#[test]
fn a_wall_clock_step_back_then_forward_cannot_admit_two_writers_within_a_spacing() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (host, cancel) = (Stepped::up(100_000), CancelToken::new());
    let call = stepped_call(&host, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let [first, second] = writers(&store, [80, 81], &call)?;
    assert!(store.claim(first, &host, &call)?.is_some());

    // The wall clock steps back an hour and a claim sees it, then it is
    // corrected forward. Only 5 s of real time have passed since the start.
    host.step_wall_ms(-3_600_000);
    host.pass_ms(5_000);
    assert!(store.claim(second, &host, &call)?.is_none());
    host.step_wall_ms(3_600_000);
    assert!(
        store.claim(second, &host, &call)?.is_none(),
        "5 s after a writer started, whatever the wall clock read between"
    );

    // The hold is one spacing from the start, not from the claim that saw the
    // step: 20 s of real time after the start the writer is admitted, no later.
    host.pass_micros(14_999_999);
    assert!(store.claim(second, &host, &call)?.is_none());
    host.pass_micros(1);
    assert!(store.claim(second, &host, &call)?.is_some());
    Ok(())
}

#[test]
fn a_forward_wall_clock_step_alone_does_not_free_the_class() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (host, cancel) = (Stepped::up(100_000), CancelToken::new());
    let call = stepped_call(&host, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let [first, second] = writers(&store, [82, 83], &call)?;
    assert!(store.claim(first, &host, &call)?.is_some());
    host.step_wall_ms(86_400_000);
    assert!(
        store.claim(second, &host, &call)?.is_none(),
        "a day on the wall clock is no time on the host clock"
    );
    host.pass_micros(19_999_999);
    assert!(store.claim(second, &host, &call)?.is_none());
    host.pass_micros(1);
    assert!(store.claim(second, &host, &call)?.is_some());
    Ok(())
}

#[test]
fn read_only_spacing_holds_on_the_host_clock_and_never_holds_a_writer() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (host, cancel) = (Stepped::up(100_000), CancelToken::new());
    let call = stepped_call(&host, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let first = starting(&store, &lane(85, false), &call)?;
    let second = starting(&store, &lane(86, false), &call)?;
    let [writer] = writers(&store, [87], &call)?;
    assert!(store.claim(first, &host, &call)?.is_some());
    host.step_wall_ms(-60_000);
    host.pass_micros(1_999_999);
    assert!(store.claim(second, &host, &call)?.is_none());
    assert!(
        store.claim(writer, &host, &call)?.is_some(),
        "the read-only start holds no writer"
    );
    host.pass_micros(1);
    assert!(store.claim(second, &host, &call)?.is_some());
    Ok(())
}

#[test]
fn a_new_boot_frees_the_class_and_spacing_runs_on_the_new_boot() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (host, cancel) = (Stepped::up(500_000), CancelToken::new());
    let call = stepped_call(&host, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let [first, second, third] = writers(&store, [88, 89, 90], &call)?;
    assert!(store.claim(first, &host, &call)?.is_some());
    // The host restarted. Its clock is behind the recorded start, which is
    // another boot's reading and says nothing about this one.
    host.reboot("boot-b", 1_000);
    assert!(
        store.claim(second, &host, &call)?.is_some(),
        "a start from an earlier boot does not hold the class"
    );
    host.pass_ms(5_000);
    assert!(
        store.claim(third, &host, &call)?.is_none(),
        "the new boot's start is the one that spaces"
    );
    host.pass_ms(15_000);
    assert!(store.claim(third, &host, &call)?.is_some());
    Ok(())
}

#[test]
fn an_unreadable_host_clock_or_boot_identity_refuses_the_claim() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (host, cancel) = (Stepped::up(100_000), CancelToken::new());
    let call = stepped_call(&host, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let [lane_id] = writers(&store, [91], &call)?;
    let attempt = store.attempt_dir(lane(91, true).run_id, lane_id, false)?;
    for (readable, name) in [
        (&host.host_readable, "monotonic clock"),
        (&host.boot_readable, "boot identity"),
    ] {
        readable.set(false);
        assert!(
            matches!(store.claim(lane_id, &host, &call), Err(RunError::HostClock)),
            "an unreadable {name} refuses, with no wall-clock fallback"
        );
        let admitted = store.admit(lane_id, &attempt, &Host { available: 100 }, &host, &cancel);
        let reason = admitted.err().ok_or("admit must refuse")?.to_string();
        assert!(
            reason.contains("startup spacing cannot be judged"),
            "{name}: {reason}"
        );
        assert_eq!(
            store.record(lane_id, &call)?.state,
            AttemptState::Starting,
            "nothing started"
        );
        readable.set(true);
    }
    assert!(
        store.claim(lane_id, &host, &call)?.is_some(),
        "no lease was taken while the clock was unreadable"
    );
    Ok(())
}

#[test]
fn lane_waits_count_spacing_on_the_host_clock_and_say_nothing_when_it_is_unreadable() -> TestResult
{
    let temp = tempfile::tempdir()?;
    let (host, cancel) = (Stepped::up(100_000), CancelToken::new());
    let call = stepped_call(&host, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let [first, second] = writers(&store, [92, 93], &call)?;
    assert!(store.claim(first, &host, &call)?.is_some());
    let wait = |host: &Stepped| -> Result<String> {
        let records = [store.record(second, &call)?];
        let waits = store.lane_waits(&records, &stepped_call(host, &cancel));
        Ok(waits
            .first()
            .map(|w| w.waiting_for.clone())
            .unwrap_or_default())
    };
    host.pass_ms(5_000);
    host.step_wall_ms(86_400_000);
    assert_eq!(
        wait(&host)?,
        "writer startup spacing: 15.0 s left of 20.0 s",
        "the wall clock stepped a day and is not what the wait counts"
    );
    host.host_readable.set(false);
    assert!(
        wait(&host)?.starts_with("host capacity"),
        "no wait is stated when the host clock cannot be read"
    );
    Ok(())
}

#[test]
fn a_start_ahead_of_the_host_clock_in_the_same_boot_never_frees_the_class() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (host, cancel) = (Stepped::up(100_000), CancelToken::new());
    let call = stepped_call(&host, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let [first, second] = writers(&store, [94, 95], &call)?;
    assert!(store.claim(first, &host, &call)?.is_some());
    let ledger = crate::ledger::open(&store.writer_registry, &call)?;
    ledger.execute(
        "UPDATE spacing SET last_start=last_start+3600000000 WHERE class='writer'",
        [],
    )?;
    host.pass_ms(60_000);
    assert!(
        store.claim(second, &host, &call)?.is_none(),
        "a recorded start the clock has not reached holds, never frees"
    );
    Ok(())
}

/// A ledger as an earlier build left it, under the store's host root. Tables
/// come from `schema`; the file is created private like the real one.
fn old_ledger(store: &RunStore, schema: &str) -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let file = store.writer_registry.join("admission.sqlite3");
    std::fs::create_dir_all(&store.writer_registry)?;
    let old = rusqlite::Connection::open(&file)?;
    old.execute_batch(schema)?;
    drop(old);
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[test]
fn a_version_2_ledger_that_kept_wall_milliseconds_starts_every_class_free() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (host, cancel) = (Stepped::up(100_000), CancelToken::new());
    let call = stepped_call(&host, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    old_ledger(
        &store,
        "CREATE TABLE leases (
          attempt TEXT PRIMARY KEY, project TEXT NOT NULL, store BLOB NOT NULL,
          class TEXT NOT NULL DEFAULT 'writer' CHECK(class IN ('writer','read_only'))
         ) STRICT;
         CREATE INDEX lease_projects ON leases(project);
         CREATE TABLE spacing (
          class TEXT PRIMARY KEY CHECK(class IN ('writer','read_only')),
          last_start INTEGER NOT NULL
         ) STRICT;
         INSERT INTO spacing(class,last_start) VALUES ('writer', 4000000000000);
         PRAGMA user_version=2;",
    )?;
    let [first, second] = writers(&store, [96, 97], &call)?;
    assert!(
        store.claim(first, &host, &call)?.is_some(),
        "wall milliseconds from before boot-keyed spacing hold no class"
    );
    assert!(store.claim(second, &host, &call)?.is_none());
    Ok(())
}

#[test]
fn upgrading_a_version_1_ledger_is_atomic() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (host, cancel) = (Stepped::up(100_000), CancelToken::new());
    let call = stepped_call(&host, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let version_1 = "CREATE TABLE leases (attempt TEXT PRIMARY KEY, project TEXT NOT NULL, store BLOB NOT NULL) STRICT;
         CREATE INDEX lease_projects ON leases(project);";
    // A spacing that cannot be dropped fails the upgrade after it has added
    // the lease class column. A crash at that point must leave version 1 whole.
    old_ledger(
        &store,
        &format!(
            "{version_1}
             CREATE VIEW spacing AS SELECT 1 AS id, 4000000000000 AS last_start;
             PRAGMA user_version=1;"
        ),
    )?;
    let [first, second] = writers(&store, [98, 99], &call)?;
    assert!(store.claim(first, &host, &call).is_err());
    let file = store.writer_registry.join("admission.sqlite3");
    let raw = rusqlite::Connection::open(&file)?;
    let classes: i64 = raw.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('leases') WHERE name='class'",
        [],
        |r| r.get(0),
    )?;
    let version: i64 = raw.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    assert_eq!(
        (classes, version),
        (0, 1),
        "the failed upgrade left no trace"
    );
    raw.execute_batch(
        "DROP VIEW spacing;
         CREATE TABLE spacing (id INTEGER PRIMARY KEY CHECK(id=1), last_start INTEGER NOT NULL) STRICT;
         INSERT INTO spacing(id,last_start) VALUES (1,4000000000000);",
    )?;
    drop(raw);
    assert!(
        store.claim(first, &host, &call)?.is_none(),
        "the upgrade carried the start into the new ledger"
    );
    host.pass_ms(20_500);
    assert!(
        store.claim(first, &host, &call)?.is_some(),
        "a start in the wall future held the class one spacing, and no longer"
    );
    assert!(store.claim(second, &host, &call)?.is_none());
    Ok(())
}

const VERSION_1_LEDGER: &str = "CREATE TABLE leases (attempt TEXT PRIMARY KEY, project TEXT NOT NULL, store BLOB NOT NULL) STRICT;
     CREATE INDEX lease_projects ON leases(project);
     CREATE TABLE spacing (id INTEGER PRIMARY KEY CHECK(id=1), last_start INTEGER NOT NULL) STRICT;";

/// A version 1 ledger whose one start was `ago_ms` before the clock's wall now
/// (negative: in the wall future).
fn version_1_start(store: &RunStore, host: &Stepped, ago_ms: i64) -> TestResult {
    let started = host.wall_now()?.as_micros() / 1000 - ago_ms;
    old_ledger(
        store,
        &format!(
            "{VERSION_1_LEDGER}
             INSERT INTO spacing(id,last_start) VALUES (1,{started});
             PRAGMA user_version=1;"
        ),
    )
}

#[test]
fn a_version_1_start_of_any_age_holds_each_class_one_full_spacing() -> TestResult {
    // Old (a spacing and more), recent, and in the wall future: the upgrade
    // reads no wall clock, so the recorded age changes nothing.
    for (case, ago_ms) in [
        ("old", 3_600_000),
        ("recent", 5_000),
        ("future", -3_600_000),
    ] {
        let temp = tempfile::tempdir()?;
        let (host, cancel) = (Stepped::up(100_000), CancelToken::new());
        let call = stepped_call(&host, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        version_1_start(&store, &host, ago_ms)?;
        let writer = starting(&store, &lane(100, true), &call)?;
        let reader = starting(&store, &lane(101, false), &call)?;
        assert!(
            store.claim(writer, &host, &call)?.is_none(),
            "{case}: the writer is held from the upgrade"
        );
        assert!(
            store.claim(reader, &host, &call)?.is_none(),
            "{case}: the read-only class is held from the upgrade"
        );
        host.pass_ms(1_500);
        assert!(
            store.claim(reader, &host, &call)?.is_none(),
            "{case}: the read-only spacing has not run out"
        );
        host.pass_ms(1_000);
        assert!(
            store.claim(reader, &host, &call)?.is_some(),
            "{case}: the 2 s read-only spacing has run out"
        );
        host.pass_ms(17_000);
        assert!(
            store.claim(writer, &host, &call)?.is_none(),
            "{case}: the writer is held the full 20 s, not less"
        );
        host.pass_ms(1_000);
        assert!(
            store.claim(writer, &host, &call)?.is_some(),
            "{case}: held one spacing, and no longer"
        );
    }
    Ok(())
}

#[test]
fn a_version_1_ledger_with_no_start_holds_nothing() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (host, cancel) = (Stepped::up(100_000), CancelToken::new());
    let call = stepped_call(&host, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    old_ledger(
        &store,
        &format!("{VERSION_1_LEDGER} PRAGMA user_version=1;"),
    )?;
    let writer = starting(&store, &lane(104, true), &call)?;
    let reader = starting(&store, &lane(105, false), &call)?;
    assert!(store.claim(writer, &host, &call)?.is_some());
    assert!(store.claim(reader, &host, &call)?.is_some());
    Ok(())
}

#[test]
fn a_version_1_upgrade_refuses_when_the_host_clock_cannot_be_read() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (host, cancel) = (Stepped::up(100_000), CancelToken::new());
    let call = stepped_call(&host, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    version_1_start(&store, &host, 5_000)?;
    let writer = starting(&store, &lane(103, true), &call)?;
    host.boot_readable.set(false);
    assert!(matches!(
        store.claim(writer, &host, &call),
        Err(RunError::HostClock)
    ));
    let raw = rusqlite::Connection::open(store.writer_registry.join("admission.sqlite3"))?;
    let version: i64 = raw.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let rows: i64 = raw.query_row("SELECT COUNT(*) FROM spacing", [], |r| r.get(0))?;
    assert_eq!(
        (version, rows),
        (1, 1),
        "the refused upgrade dropped nothing"
    );
    host.boot_readable.set(true);
    assert!(store.claim(writer, &host, &call)?.is_none());
    Ok(())
}

/// Probes memory, and while it does another process starts a writer.
struct RivalStarts<'a> {
    store: &'a RunStore,
    rival: AttemptId,
    clock: &'a Stepped,
    call: &'a Request<'a>,
    started: Cell<bool>,
}
impl Resources for RivalStarts<'_> {
    fn memory(&self) -> std::io::Result<MemorySample> {
        if !self.started.replace(true) {
            self.clock.pass_ms(1_000);
            let rival = self.store.claim(self.rival, self.clock, self.call);
            assert!(rival.is_ok_and(|record| record.is_some()), "rival starts");
            self.clock.pass_ms(500);
        }
        Ok(MemorySample {
            available_bytes: 100,
            total_bytes: 100,
        })
    }
    fn disk(&self, _: &Directory) -> std::io::Result<u64> {
        Ok(u64::MAX)
    }
}

#[test]
fn a_delayed_admission_cannot_start_inside_a_newer_start_or_rewind_it() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (Stepped::up(0), CancelToken::new());
    let call = stepped_call(&clock, &cancel);
    let mut store = RunStore::open(&context(temp.path())?, &call)?;
    // Keep the writer slot limit from masking the startup-spacing behaviour.
    std::fs::write(
        store.writer_registry.join("config.toml"),
        "[domains.fabric.lanes]\nmax_concurrent_builds = 2\n",
    )?;
    store.set_admission_policy(AdmissionPolicy {
        admission_seconds: 1,
        ..AdmissionPolicy::default()
    });
    let (delayed, rival) = (lane(70, true), lane(71, true));
    let delayed_id = starting(&store, &delayed, &call)?;
    let rival_id = starting(&store, &rival, &call)?;
    let attempt = store.attempt_dir(delayed.run_id, delayed_id, false)?;
    // `admit` samples the time, then the probe stalls while the rival's
    // writer starts one second later. The delayed lane must see that start.
    let probe = RivalStarts {
        store: &store,
        rival: rival_id,
        clock: &clock,
        call: &call,
        started: Cell::new(false),
    };
    let admission = store.admit(delayed_id, &attempt, &probe, &clock, &cancel);
    assert!(
        matches!(&admission, Err(RunError::Refused)),
        "admission: {admission:?}"
    );
    assert!(probe.started.get());
    assert_eq!(
        store.record(delayed_id, &call)?.state,
        AttemptState::Starting
    );
    // The rival's start, at 1 s, is still the recorded one.
    let since_rival = clock.host_monotonic_micros()? / 1000 - 1_000;
    assert!(since_rival >= 500);
    assert_eq!(
        crate::ledger::spacing_left(&store.writer_registry, LaneClass::Writer, 20_000, &clock),
        Some(20_000 - since_rival)
    );
    Ok(())
}
#[test]
fn memory_floor_refuses_both_lane_classes() -> TestResult {
    // Each case has its own store and ledger, so only the floor can decide it.
    for (seed, writer, available) in [
        (50, true, 1),
        (51, false, 1),
        (52, true, 50),
        (53, false, 50),
    ] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let mut store = RunStore::open(&context(temp.path())?, &call)?;
        store.set_admission_policy(AdmissionPolicy {
            admission_seconds: 1,
            ..AdmissionPolicy::default()
        });
        let run = lane(seed, writer);
        let id = starting(&store, &run, &call)?;
        let attempt = store.attempt_dir(run.run_id, id, false)?;
        let admitted = store.admit(id, &attempt, &Host { available }, &clock, &cancel);
        if available < 5 {
            assert!(
                matches!(admitted, Err(RunError::Refused)),
                "1% memory is under the 5% floor (writer: {writer})"
            );
        } else {
            assert_eq!(admitted?.state, AttemptState::Running, "writer: {writer}");
        }
    }
    Ok(())
}

#[test]
fn project_cap_allows_up_to_32_active_lanes() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    // Four runs of eight keep each run under its own batch concurrency.
    let mut index = 0_i64;
    for seed in 60..64 {
        let run = batch(seed, 8);
        store.create(&run, &call)?;
        for attempt in &run.attempts {
            let id = attempt.attempt_id;
            store.mark_starting(
                &store.record(id, &call)?,
                &route(),
                &[],
                &Default::default(),
                None,
                &call,
            )?;
            // Spaced past both classes' windows so only the cap can refuse.
            index += 1;
            assert!(
                store.claim(id, &at(index * 20_000), &call)?.is_some(),
                "lane {index} is within the 32-lane project cap"
            );
        }
    }
    let over = lane(70, false);
    let id = starting(&store, &over, &call)?;
    assert!(
        store.claim(id, &at(40 * 20_000), &call)?.is_none(),
        "the 33rd lane waits for the project cap"
    );
    Ok(())
}

#[test]
fn batch_concurrency_defaults_by_lane_class_and_an_explicit_value_wins() {
    let profile = |knobs: &[(&str, u64)]| HarnessProfile {
        name: "test".into(),
        knobs: knobs
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect(),
        ..HarnessProfile::default()
    };
    let registry = profile(&[
        ("lanes.max_concurrency", 8),
        ("lanes.max_concurrency_read_only", 16),
    ]);
    assert_eq!(concurrency(None, &registry, false, None), 8, "writer batch");
    assert_eq!(
        concurrency(None, &registry, true, None),
        16,
        "read-only batch"
    );
    assert_eq!(concurrency(Some(3), &registry, false, None), 3);
    assert_eq!(concurrency(Some(3), &registry, true, None), 3);
    let tuned = profile(&[
        ("lanes.max_concurrency", 2),
        ("lanes.max_concurrency_read_only", 5),
    ]);
    assert_eq!(concurrency(None, &tuned, false, None), 2);
    assert_eq!(concurrency(None, &tuned, true, None), 5);
    let bare = HarnessProfile::default();
    assert_eq!(concurrency(None, &bare, false, None), 8);
    assert_eq!(concurrency(None, &bare, true, None), 16);
    assert_eq!(concurrency(None, &registry, true, Some(3)), 3, "bounded");
    assert_eq!(
        concurrency(None, &tuned, true, Some(7)),
        5,
        "a cap, not a floor"
    );
    assert_eq!(
        concurrency(Some(12), &registry, true, Some(3)),
        12,
        "explicit"
    );
}

#[test]
fn sixteen_read_only_lanes_run_in_one_batch_two_seconds_apart() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let mut run = batch(90, 16);
    run.request.concurrency = 16;
    store.create(&run, &call)?;
    for (index, attempt) in run.attempts.iter().enumerate() {
        let id = attempt.attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        assert!(
            store.claim(id, &at(index as i64 * 2_000), &call)?.is_some(),
            "lane {index} of a 16-lane read-only batch"
        );
    }
    Ok(())
}

#[test]
fn lane_waits_name_the_class_and_what_each_unstarted_lane_waits_for() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let first = starting(&store, &lane(100, true), &call)?;
    let second = starting(&store, &lane(101, true), &call)?;
    let reader = starting(&store, &lane(102, false), &call)?;
    let queued = lane(103, false);
    store.create(&queued, &call)?;
    let queued = queued.attempts[0].attempt_id;
    // The test clock reads 0 ms, so the writer start is happening now.
    assert!(store.claim(first, &at(0), &call)?.is_some());
    let records = [first, second, reader, queued]
        .map(|id| store.record(id, &call))
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
    let waits = store.lane_waits(&records, &call);
    let wait = |id| waits.iter().find(|w| w.attempt_id == id);
    assert!(wait(first).is_none(), "a running lane waits for nothing");
    let writer = wait(second).ok_or("second lane waits")?;
    assert_eq!(writer.lane_class, LaneClass::Writer);
    assert_eq!(
        writer.waiting_for,
        "writer startup spacing: 20.0 s left of 20.0 s"
    );
    let reader = wait(reader).ok_or("read-only lane waits")?;
    assert_eq!(reader.lane_class, LaneClass::ReadOnly);
    assert!(
        reader.waiting_for.starts_with("host capacity"),
        "no read-only start yet, so the writer's spacing is not its wait: {}",
        reader.waiting_for
    );
    let queued = wait(queued).ok_or("queued lane waits")?;
    assert_eq!(queued.lane_class, LaneClass::ReadOnly);
    assert!(queued.waiting_for.starts_with("launch"));

    // A serial run reports its own concurrency before any spacing.
    let serial = {
        let mut run = two_tasks(1);
        run.run_id = RunId::from_bytes([110; 16]);
        run.operation_id = provenant_core::OperationId::parse("serial-waits")?;
        run.attempts[0].attempt_id = AttemptId::from_bytes([111; 16]);
        run.attempts[1].attempt_id = AttemptId::from_bytes([112; 16]);
        run
    };
    store.create(&serial, &call)?;
    let [a, b] = [serial.attempts[0].attempt_id, serial.attempts[1].attempt_id];
    for id in [a, b] {
        store.mark_starting(
            &store.record(id, &call)?,
            &route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
    }
    assert!(store.claim(a, &at(0), &call)?.is_some());
    let blocked = store.lane_waits(&[store.record(b, &call)?], &call);
    assert_eq!(
        blocked[0].waiting_for,
        "run concurrency: 1 of 1 lanes of this run are active"
    );
    Ok(())
}

#[test]
fn legacy_tickets_without_a_class_are_treated_as_writers() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    // A ledger from before lane classes, holding one lease and no start, so
    // the upgrade holds no class (the carried start has its own tests).
    old_ledger(
        &store,
        "CREATE TABLE leases (attempt TEXT PRIMARY KEY, project TEXT NOT NULL, store BLOB NOT NULL) STRICT;
         CREATE INDEX lease_projects ON leases(project);
         CREATE TABLE spacing (id INTEGER PRIMARY KEY CHECK(id=1), last_start INTEGER NOT NULL) STRICT;
         INSERT INTO leases(attempt,project,store) VALUES ('legacy','project',x'2f6e6f77686572652f');
         PRAGMA user_version=1;",
    )?;

    let old = starting(&store, &lane(80, true), &call)?;
    // A ticket recorded before classes has no class field; strip it.
    let raw: Vec<u8> = store.db.query_row(
        "SELECT record FROM admission WHERE attempt=?1",
        [old.to_string()],
        |r| r.get(0),
    )?;
    let mut ticket: serde_json::Value = db::decode(&raw)?;
    ticket
        .as_object_mut()
        .ok_or("ticket object")?
        .remove("lane_class");
    store.db.execute(
        "UPDATE admission SET record=?1 WHERE attempt=?2",
        rusqlite::params![db::bytes(&ticket)?, old.to_string()],
    )?;
    let writer = starting(&store, &lane(81, true), &call)?;
    let reader = starting(&store, &lane(82, false), &call)?;

    assert!(store.claim(old, &at(11_000), &call)?.is_some());
    assert!(
        store.claim(writer, &at(11_001), &call)?.is_none(),
        "a ticket with no class is a writer, spaced 20 s from the last writer start"
    );
    assert!(
        store.claim(reader, &at(11_001), &call)?.is_some(),
        "the writer start holds no read-only lane"
    );
    assert!(store.claim(writer, &at(31_000), &call)?.is_some());
    Ok(())
}

#[test]
fn child_stores_share_the_host_ledger_and_recovery_frees_a_lease() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let now = at_micros(100);
    let base = std::fs::canonicalize(temp.path())?;
    let host = base.join("host");
    let mut stores = Vec::new();
    for name in ["first", "second"] {
        std::fs::create_dir(base.join(name))?;
        let mut store =
            RunStore::open(&context(&base.join(name))?, &call)?.with_writer_registry(&host);
        store.set_admission_policy(AdmissionPolicy {
            startup_spacing_ms: 0,
            read_only_spacing_ms: 0,
            ..AdmissionPolicy::default()
        });
        stores.push(store);
    }
    // Each store fills half the project's host-wide limit, in runs that stay
    // inside one batch's concurrency.
    let per_run = PROJECT_ACTIVE / 4;
    let batches = [
        batch(1, per_run),
        batch(4, per_run),
        batch(2, per_run),
        batch(5, per_run),
    ];
    for (store, run) in stores.iter().flat_map(|s| [s, s]).zip(&batches) {
        store.create(run, &call)?;
        for attempt in &run.attempts {
            store.mark_starting(
                &store.record(attempt.attempt_id, &call)?,
                &route(),
                &[],
                &Default::default(),
                None,
                &call,
            )?;
            store
                .claim(attempt.attempt_id, &now, &call)?
                .ok_or("within the host limit")?;
        }
    }
    // Counted per store, the second store would admit another attempt.
    let extra = batch(3, 1);
    let waiting = extra.attempts[0].attempt_id;
    stores[1].create(&extra, &call)?;
    stores[1].mark_starting(
        &stores[1].record(waiting, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    assert!(
        stores[1].claim(waiting, &now, &call)?.is_none(),
        "both child stores count against one project limit"
    );
    // The first attempt's owner dies; recovery commits its terminal from the
    // sealed exit, as reconcile does, and that frees its host lease.
    let dead = batches[0].attempts[0].attempt_id;
    let dir = stores[0].attempt_dir(batches[0].run_id, dead, false)?;
    artifacts::seal(&dir, "stdout.log", b"partial", CAPTURE_LIMIT)?;
    let mut observed = exit();
    observed.scope.run = batches[0].run_id;
    observed.scope.attempt = dead;
    observed.scope.generation = 1;
    observed.termination = Some(Termination::Signal(9));
    seal_exit(&stores[0], &observed)?;
    let fake = Fake {
        refuse: false,
        observed: Some(route()),
        ..Fake::default()
    };
    stores[0].finish(dead, &dir, &observed, &fake, &call)?;
    let admitted = stores[1]
        .claim(waiting, &now, &call)?
        .ok_or("recovered lease not freed")?;
    assert_eq!(admitted.state, AttemptState::Running);
    Ok(())
}
/// A run of `tasks` attempts at full concurrency, with IDs unique to `seed`.
fn batch(seed: u8, tasks: u64) -> PreparedRun {
    let mut run = input();
    let template = run.request.tasks[0].clone();
    run.run_id = provenant_core::RunId::from_bytes([seed; 16]);
    run.operation_id = provenant_core::OperationId::parse(&format!("batch-{seed}")).unwrap();
    run.request.concurrency = tasks.min(8);
    run.request.tasks.clear();
    run.attempts.clear();
    for index in 0..tasks as u8 {
        let mut task = template.clone();
        task.task_id = TaskId::from_bytes([seed, index, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1]);
        run.attempts.push(NewAttempt {
            task_id: task.task_id,
            attempt_id: AttemptId::from_bytes([
                seed, index, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
            ]),
            prompt: "exact prompt".into(),
        });
        run.request.tasks.push(task);
    }
    run
}

#[test]
fn prepare_refusal_is_terminal_without_custody() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    store.create(&run, &call)?;
    let fake = Fake {
        refuse: true,
        observed: None,
        ..Fake::default()
    };
    let started = store.start(run.run_id, &fake, &|_, _| unreachable!(), &call)?;
    assert!(
        matches!(&started[..], [Started::Refused { reason, .. }] if reason.contains("disabled"))
    );
    let prompt = fake.prompt.borrow();
    let path = store.context.state_root.as_path().join(format!(
        "runs/{}/attempts/{}",
        run.run_id, run.attempts[0].attempt_id
    ));
    assert!(prompt.contains(&path.join("result.md").display().to_string()));
    assert!(prompt.contains(&path.join("process-exit.json").display().to_string()));
    assert!(prompt.contains("only durable outputs"));
    assert!(prompt.contains("Files in $TMPDIR are discarded when the attempt ends."));
    assert!(prompt.contains("Provider history is kept."));
    assert!(prompt.ends_with("exact prompt"));
    let fact = store.attempt(run.attempts[0].attempt_id, &call)?;
    assert_eq!(
        fact.terminal_state,
        provenant_records::AttemptOutcome::Rejected
    );
    assert!(fact.started_at.is_none());
    assert_eq!(store.run(run.run_id, &call)?.closure, Closure::Closed);
    assert!(
        store
            .start(run.run_id, &fake, &|_, _| unreachable!(), &call)?
            .is_empty()
    );
    Ok(())
}

/// Starts a one-task run after `damage` has altered its stored prompt, and
/// asserts the attempt ends Rejected before preparation with `cause` recorded.
fn assert_unreadable_prompt_rejected(
    damage: impl FnOnce(&std::path::Path) -> TestResult,
    cause: &str,
) -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let run = input();
    store.create(&run, &call)?;
    let id = run.attempts[0].attempt_id;
    let prompt = context
        .state_root
        .as_path()
        .join(format!("runs/{}/attempts/{id}/prompt.txt", run.run_id));
    damage(&prompt)?;
    let fake = Fake::default();
    let started = store.start(run.run_id, &fake, &|_, _| unreachable!(), &call)?;
    // Restore access so the tempdir can be removed whatever the outcome.
    if prompt.exists() {
        std::fs::set_permissions(&prompt, std::fs::Permissions::from_mode(0o600))?;
    }
    assert!(matches!(
        &started[..],
        [Started::Refused { attempt, reason, kind: RefusalKind::Preparation }]
            if *attempt == id && reason.contains("stored prompt unreadable")
    ));
    assert_eq!(fake.prepared.get(), 0);
    assert_eq!(terminal(&store, id, &call)?, Outcome::Rejected);
    let fact = store.attempt(id, &call)?;
    assert_eq!(
        fact.terminal_state,
        provenant_records::AttemptOutcome::Rejected
    );
    assert!(fact.started_at.is_none());
    assert!(
        fact.reason.as_deref().is_some_and(
            |reason| reason.starts_with("stored prompt unreadable") && reason.contains(cause)
        ),
        "{:?}",
        fact.reason
    );
    assert!(
        store
            .start(run.run_id, &fake, &|_, _| unreachable!(), &call)?
            .is_empty()
    );
    Ok(())
}

#[test]
fn missing_stored_prompt_ends_the_attempt_rejected_with_the_cause() -> TestResult {
    assert_unreadable_prompt_rejected(
        |prompt| {
            std::fs::remove_file(prompt)?;
            Ok(())
        },
        "fstatat failed",
    )
}

#[test]
fn mode_zero_stored_prompt_ends_the_attempt_rejected_with_the_cause() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    assert_unreadable_prompt_rejected(
        |prompt| {
            std::fs::set_permissions(prompt, std::fs::Permissions::from_mode(0o000))?;
            Ok(())
        },
        "openat read failed",
    )
}

#[test]
fn holder_release_reclaims_temporary_after_verified_settlement() -> TestResult {
    holder_release_temporary_fixture(true)
}

#[test]
fn holder_release_retains_temporary_when_verification_fails() -> TestResult {
    holder_release_temporary_fixture(false)
}

fn holder_release_temporary_fixture(valid_binding: bool) -> TestResult {
    let temp = tempfile::tempdir()?;
    let custody_dir = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    store.create(&run, &call)?;
    let id = run.attempts[0].attempt_id;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let attempt = store.attempt_dir(run.run_id, id, false)?;
    let mut observed = exit();
    observed.scope = RunStore::scope(&store.record(id, &call)?);
    seal_exit(&store, &observed)?;
    store.finish(id, &attempt, &observed, &Fake::default(), &call)?;
    let temporary = store.temporary_path(id, &call)?;
    std::fs::create_dir_all(&temporary)?;
    std::fs::write(temporary.join("payload.lock"), "discard")?;
    drop(provenant_supervisor::fixture::acquire_writer(
        Directory::open_anchor(custody_dir.path())?,
        observed.scope,
        call.budget,
        &cancel,
        &clock,
    )?);
    let reservation = custody_dir.path().join("reservation.json");
    let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&reservation)?)?;
    let meta = attempt.metadata()?;
    value["launch_state"] = "never_spawned".into();
    value["unresolved_cleanup"] = false.into();
    value["census_incomplete"] = false.into();
    value["attempt_device"] = meta.device.into();
    value["attempt_inode"] = if valid_binding {
        meta.inode
    } else {
        meta.inode + 1
    }
    .into();
    std::fs::write(reservation, serde_json::to_vec(&value)?)?;
    let recovery = provenant_supervisor::custody::acquire_recovery(
        Directory::open_anchor(custody_dir.path())?,
        call.budget,
        &cancel,
        &clock,
    )?;
    crate::holder::release_and_reclaim(&attempt, observed.scope, &recovery)?;
    assert_eq!(temporary.exists(), !valid_binding);
    assert!(
        attempt
            .metadata_at(std::ffi::OsStr::new("release.json"), false)
            .is_ok()
    );
    Ok(())
}

#[test]
fn holder_commits_decoded_success_only_with_exit_zero_and_observed_identity() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let mut store = RunStore::open(&context(temp.path())?, &call)?;
    store.set_admission_policy(AdmissionPolicy {
        startup_spacing_ms: 0,
        read_only_spacing_ms: 0,
        ..AdmissionPolicy::default()
    });
    let batch = two_tasks(2);
    store.create(&batch, &call)?;
    let now = at_micros(100);
    let fake = Fake {
        refuse: false,
        observed: Some(route()),
        ..Fake::default()
    };
    for (index, code) in [(0, 0), (1, 1)] {
        let id = batch.attempts[index].attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        store.claim(id, &now, &call)?.ok_or("not admitted")?;
        let dir = store.attempt_dir(batch.run_id, id, false)?;
        artifacts::seal(&dir, "stdout.log", b"answer", CAPTURE_LIMIT)?;
        let mut observed = exit();
        observed.scope.attempt = id;
        observed.scope.generation = 1;
        observed.terminal_observed = false;
        observed.termination = Some(Termination::Exit(code));
        seal_exit(&store, &observed)?;
        let record = store.finish(id, &dir, &observed, &fake, &call)?;
        let AttemptState::Terminal { outcome, .. } = record.state else {
            return Err("not terminal".into());
        };
        assert_eq!(
            outcome,
            if code == 0 {
                Outcome::Succeeded
            } else {
                Outcome::Failed
            }
        );
        assert_eq!(store.finish(id, &dir, &observed, &fake, &call)?, record);
        provenant_supervisor::custody::publish_release(&dir, observed.scope)?;
        provenant_supervisor::custody::publish_release(&dir, observed.scope)?;
    }
    assert_eq!(fake.decoded.get(), 2);
    let fact = store.attempt(batch.attempts[0].attempt_id, &call)?;
    assert_eq!(
        fact.output_refs[0].digest,
        CanonicalV1::file_digest(b"answer")
    );
    assert_eq!(store.run(batch.run_id, &call)?.closure, Closure::Closed);
    Ok(())
}

#[test]
fn never_spawned_exit_is_a_refusal_with_its_reason() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    store.create(&run, &call)?;
    let id = run.attempts[0].attempt_id;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let mut observed = exit();
    observed.scope.generation = 1;
    observed.never_spawned = true;
    observed.termination = None;
    observed.terminal_observed = false;
    observed.warnings = vec!["admission refused".into()];
    observed.settlement_evidence = provenant_supervisor::custody::SettlementEvidence::NeverSpawned;
    seal_exit(&store, &observed)?;
    let dir = store.attempt_dir(run.run_id, id, false)?;
    let fake = Fake {
        refuse: false,
        observed: None,
        ..Fake::default()
    };
    store.finish(id, &dir, &observed, &fake, &call)?;
    let fact = store.attempt(id, &call)?;
    assert_eq!(
        fact.terminal_state,
        provenant_records::AttemptOutcome::Rejected
    );
    assert_eq!(fact.reason.as_deref(), Some("admission refused"));
    assert_eq!(fake.decoded.get(), 0);
    Ok(())
}

#[test]
fn private_temporary_directories_are_removed_after_terminal_and_withdrawal() -> TestResult {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = two_tasks(2);
    store.create(&run, &call)?;
    let outside = temp.path().join("outside.txt");
    std::fs::write(&outside, "keep")?;

    let withdrawn = run.attempts[0].attempt_id;
    let withdrawn_tmp = store.temporary_path(withdrawn, &call)?;
    std::fs::write(withdrawn_tmp.join("provider.tmp"), "discard")?;
    symlink(&outside, withdrawn_tmp.join("outside-link"))?;
    store.withdraw(
        withdrawn,
        AttemptState::Queued,
        Outcome::Cancelled,
        "cancelled before start",
        WallTime::from_micros(150)?,
        &call,
    )?;
    assert!(!withdrawn_tmp.exists());

    let terminal = run.attempts[1].attempt_id;
    let terminal_tmp = store.temporary_path(terminal, &call)?;
    std::fs::write(terminal_tmp.join("provider.tmp"), "discard")?;
    symlink(&outside, terminal_tmp.join("outside-link"))?;
    let mut observed = exit();
    observed.scope.attempt = terminal;
    observed.termination = Some(Termination::Exit(1));
    seal_exit(&store, &observed)?;
    let publication = TerminalPublication {
        outcome: Outcome::Failed,
        finished_at: WallTime::from_micros(160)?,
        started_at: Some(WallTime::from_micros(120)?),
        provider_ms: Some(1),
        reason: Some("fixture failure".into()),
        result: "",
        complete: true,
        redacted: false,
        resolved_route: Some(route()),
        applied_route: Some(route()),
        observed_route: Some(route()),
        usage: UsageRecord::default(),
        session: None,
    };
    store.publish_terminal(terminal, 1, &publication, &observed, &call)?;
    assert!(!terminal_tmp.exists());
    assert_eq!(std::fs::read_to_string(outside)?, "keep");
    Ok(())
}

#[test]
fn writer_private_temporary_directories_are_removed_after_terminal_and_withdrawal() -> TestResult {
    for withdrawn in [false, true] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let context = context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let mut run = input();
        run.request.tasks[0].writer = Some(WriterRequest {
            project: temp.path().join("checkout"),
            worktree: temp.path().join("checkout/.worktrees/lane"),
        });
        store.create(&run, &call)?;
        let id = run.attempts[0].attempt_id;
        let attempt = store.attempt_dir(run.run_id, id, false)?;
        let scratch = artifacts::subdir(&attempt, std::ffi::OsStr::new("scratch"), true)?;
        let temporary = store.temporary_path(id, &call)?;
        let candidate = temporary.join("candidate-0");
        std::fs::create_dir(&candidate)?;
        std::fs::write(candidate.join("provider.tmp"), "discard")?;
        if withdrawn {
            store.withdraw(
                id,
                AttemptState::Queued,
                Outcome::Cancelled,
                "cancelled before start",
                WallTime::from_micros(150)?,
                &call,
            )?;
        } else {
            let observed = exit();
            seal_exit(&store, &observed)?;
            store.publish_terminal(id, 1, &super::tests::publication(), &observed, &call)?;
        }
        // The launch/guardian settlement edge removes empty probe staging.
        drop(scratch);
        crate::writer::settle_launch_staging(&attempt)?;
        assert!(matches!(
            store.record(id, &call)?.state,
            AttemptState::Terminal { .. }
        ));
        assert!(
            attempt
                .metadata_at(std::ffi::OsStr::new("scratch"), false)
                .is_err_and(|e| e.is_not_found())
        );
        assert!(!temporary.exists());
    }
    Ok(())
}

fn terminal(store: &RunStore, id: AttemptId, call: &Request<'_>) -> Result<Outcome> {
    match store.record(id, call)?.state {
        AttemptState::Terminal { outcome, .. } => Ok(outcome),
        _ => Err(RunError::Nonterminal),
    }
}

#[test]
fn cancel_ends_queued_work_and_stops_admission_before_spawn() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let batch = two_tasks(2);
    store.create(&batch, &call)?;
    let [queued, starting] = [batch.attempts[0].attempt_id, batch.attempts[1].attempt_id];
    let op = provenant_core::OperationId::parse("stop")?;
    store.bind_cancel(&op, &queued.to_string(), &call)?;
    store.bind_cancel(&op, &queued.to_string(), &call)?;
    assert!(matches!(
        store.bind_cancel(&op, &starting.to_string(), &call),
        Err(RunError::OperationConflict)
    ));

    assert_eq!(
        store.cancel(queued, "stop", &call)?,
        CancelReceipt::Cancelled
    );
    assert_eq!(terminal(&store, queued, &call)?, Outcome::Cancelled);
    assert!(store.attempt(queued, &call)?.started_at.is_none());
    assert_eq!(
        store.cancel(queued, "stop", &call)?,
        CancelReceipt::AlreadyTerminal
    );

    store.mark_starting(
        &store.record(starting, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    // Before the handoff publishes its launch summary there is nothing to
    // stop; cancel retries until its budget ends, then says so.
    let system = provenant_os::clock::SystemClock::new();
    let spent = Request {
        clock: &system,
        budget: provenant_core::WaitBudget::cli(provenant_core::Clock::mono_now(&system), 1)?,
        cancel: &cancel,
    };
    // A held custody lock is a live handoff: cancel leaves it untouched.
    let scope = provenant_supervisor::custody::Scope {
        run: batch.run_id,
        attempt: starting,
        generation: 1,
    };
    let held = provenant_supervisor::fixture::acquire_writer(
        store.attempt_dir(batch.run_id, starting, false)?,
        scope,
        call.budget,
        &cancel,
        &clock,
    )?;
    assert!(matches!(
        store.cancel(starting, "stop", &spent)?,
        CancelReceipt::Unobservable { .. }
    ));
    drop(held);
    // Supervisor's stop writes cancel.json; the waiting guardian sees it.
    let dir = store.attempt_dir(batch.run_id, starting, false)?;
    artifacts::seal(&dir, "cancel.json", b"{}", RECORD_LIMIT)?;
    let host = Host { available: 100 };
    assert!(matches!(
        store.admit(starting, &dir, &host, &clock, &cancel),
        Err(RunError::Cancelled)
    ));
    let mut observed = exit();
    observed.scope.attempt = starting;
    observed.scope.generation = 1;
    observed.never_spawned = true;
    observed.termination = None;
    observed.terminal_observed = false;
    observed.settlement_evidence = provenant_supervisor::custody::SettlementEvidence::NeverSpawned;
    seal_exit(&store, &observed)?;
    store.finish(starting, &dir, &observed, &Fake::default(), &call)?;
    assert_eq!(terminal(&store, starting, &call)?, Outcome::Cancelled);
    assert_eq!(store.run(batch.run_id, &call)?.closure, Closure::Closed);
    Ok(())
}

#[test]
fn rejected_resume_remains_visible_and_addressable() -> TestResult {
    struct RouteMismatch;
    impl Providers for RouteMismatch {
        fn prepare(
            &self,
            _: &TaskRequest,
            _: &str,
            _: &LaunchPaths,
            prior: Option<&AttemptRecord>,
        ) -> std::result::Result<PreparedLaunch, String> {
            assert!(prior.is_some());
            Err("route differs from the prior attempt's provider, model or account".into())
        }
        fn decode(
            &self,
            _: &RouteRecord,
            _: &[u8],
            _: &ExitObservation,
            _: &std::path::Path,
        ) -> std::result::Result<Decoded, String> {
            unreachable!()
        }
    }
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let initial = input();
    store.create(&initial, &call)?;
    let refusal = Fake {
        refuse: true,
        ..Fake::default()
    };
    store.start(initial.run_id, &refusal, &|_, _| unreachable!(), &call)?;
    let resumed = Resume {
        prior: initial.attempts[0].attempt_id,
        expected_generation: 0,
        operation_id: "rejected-resume".parse()?,
        run_id: RunId::from_bytes([20; 16]),
        attempt_id: AttemptId::from_bytes([21; 16]),
        prompt: "answer".into(),
        created_at: WallTime::from_micros(100)?,
    };
    store.resume(&resumed, &call)?;
    store.start(
        resumed.run_id,
        &RouteMismatch,
        &|_, _| unreachable!(),
        &call,
    )?;
    let reader =
        RunStore::open_read_only(&context(temp.path())?, &call)?.ok_or("run store missing")?;
    let lanes = reader.lanes(
        &LaneQuery {
            targets: vec![],
            all: true,
            limit: 10,
            after: 0,
            newest_first: false,
            owner: None,
            unseen_after: None,
        },
        &call,
    )?;
    let rejected = lanes
        .lanes
        .iter()
        .find(|lane| lane.attempt_id == resumed.attempt_id)
        .ok_or("rejected resume missing from lanes")?;
    assert!(matches!(
        rejected.state,
        AttemptState::Terminal {
            outcome: Outcome::Rejected,
            ..
        }
    ));
    assert!(
        matches!(&rejected.state, AttemptState::Terminal { reason: Some(reason), .. } if reason.contains("route differs"))
    );
    let output = Service::new(&reader).output(
        resumed.attempt_id,
        ArtifactPart::Result,
        ByteRange::default(),
        &call,
    )?;
    assert!(output.bytes.is_empty());
    Ok(())
}

#[test]
fn resumed_starting_lane_has_private_directories_before_checkpoint() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let original = input();
    store.create(&original, &call)?;
    let prior = original.attempts[0].attempt_id;
    let fake = Fake {
        refuse: true,
        ..Fake::default()
    };
    store.start(original.run_id, &fake, &|_, _| unreachable!(), &call)?;
    let resumed = Resume {
        prior,
        expected_generation: 0,
        operation_id: provenant_core::OperationId::parse("resume-private")?,
        run_id: RunId::from_bytes([50; 16]),
        attempt_id: AttemptId::from_bytes([51; 16]),
        prompt: "answer".into(),
        created_at: WallTime::from_micros(100)?,
    };
    store.resume(&resumed, &call)?;
    let record = store.record(resumed.attempt_id, &call)?;
    store.mark_starting(&record, &route(), &[], &Default::default(), None, &call)?;
    let directory = store.attempt_dir(resumed.run_id, resumed.attempt_id, false)?;
    assert!(
        matches!(directory.metadata_at("process.json".as_ref(), false), Err(error) if error.is_not_found())
    );
    let reader = RunStore::open_read_only(&context, &call)?.ok_or(RunError::NotFound)?;
    assert_eq!(
        reader.record(resumed.attempt_id, &call)?.state,
        AttemptState::Starting
    );
    let state = context.state_root.as_path();
    for path in [
        state.join("runs"),
        state.join("runs").join(resumed.run_id.to_string()),
        state
            .join("runs")
            .join(resumed.run_id.to_string())
            .join("attempts"),
        state
            .join("runs")
            .join(resumed.run_id.to_string())
            .join("attempts")
            .join(resumed.attempt_id.to_string()),
    ] {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::symlink_metadata(path)?.permissions().mode() & 0o077,
            0
        );
    }
    Ok(())
}

#[test]
fn resume_continues_a_terminal_attempt_once_with_its_route() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let mut run = input();
    run.request.chair = Some(ChairBinding {
        host: provenant_core::HostId::from_bytes([30; 16]),
        actor: provenant_core::ActorId::from_bytes([31; 16]),
    });
    let outer_parent = AttemptId::from_bytes([19; 16]);
    run.request.lane_depth = Some(1);
    run.request.parent_attempt = Some(outer_parent);
    run.request.tasks[0].account = Some("fixture-account".into());
    store.create(&run, &call)?;
    let prior = run.attempts[0].attempt_id;
    let resume = |operation: &str, expected_generation, byte| -> Result<Resume> {
        Ok(Resume {
            prior,
            expected_generation,
            operation_id: provenant_core::OperationId::parse(operation)
                .map_err(|_| RunError::Validation("operation"))?,
            run_id: provenant_core::RunId::from_bytes([byte; 16]),
            attempt_id: AttemptId::from_bytes([byte + 1; 16]),
            prompt: "answer".into(),
            created_at: WallTime::from_micros(100).map_err(|_| RunError::Validation("time"))?,
        })
    };
    assert!(matches!(
        store.resume(&resume("r1", 0, 20)?, &call),
        Err(RunError::Nonterminal)
    ));
    let fake = Fake {
        refuse: true,
        ..Fake::default()
    };
    store.start(run.run_id, &fake, &|_, _| unreachable!(), &call)?;
    assert_eq!(fake.resumed.get(), None);

    let continued = store.resume(&resume("r1", 0, 20)?, &call)?;
    assert_eq!(continued.parent, Some(prior));
    assert_eq!(
        store.request(continued.run_id, &call)?.chair,
        run.request.chair
    );
    assert_eq!(
        store.request(continued.run_id, &call)?.tasks[0]
            .account
            .as_deref(),
        Some("fixture-account")
    );
    assert_eq!(store.resume(&resume("r1", 0, 20)?, &call)?, continued);
    let mut changed = resume("r1", 0, 20)?;
    changed.prompt = "other".into();
    assert!(matches!(
        store.resume(&changed, &call),
        Err(RunError::OperationConflict)
    ));
    for expected in [0, 1] {
        assert!(matches!(
            store.resume(&resume("r2", expected, 30)?, &call),
            Err(RunError::Conflict)
        ));
    }
    let next = store.pending_attempts(continued.run_id, &call)?[0].attempt_id;
    let record = store.record(next, &call)?;
    assert_eq!(record.retry_of, Some(prior));
    assert_eq!(record.attempt_number, 2);
    assert_eq!(
        record.requested_route,
        store.record(prior, &call)?.requested_route
    );
    store.start(continued.run_id, &fake, &|_, _| unreachable!(), &call)?;
    assert_eq!(fake.resumed.get(), Some(prior));
    Ok(())
}

#[test]
fn nested_continuation_uses_host_derived_ids() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let mut child = input();
    child.request.parent_attempt = Some(AttemptId::from_bytes([9; 16]));
    child.request.lane_depth = Some(2);
    store.create(&child, &call)?;
    store.refuse_queued(child.run_id, "completed", &call)?;
    let mut resume = Resume {
        prior: child.attempts[0].attempt_id,
        expected_generation: 0,
        operation_id: provenant_core::OperationId::parse("nested-continuation")?,
        run_id: RunId::from_bytes([30; 16]),
        attempt_id: AttemptId::from_bytes([31; 16]),
        prompt: "answer".into(),
        created_at: clock.wall_now()?,
    };
    let continued = store.resume_fresh(&resume, "fixture", &call)?;
    let request = store.request(continued.run_id, &call)?;
    assert_eq!(request.child_attempt_id_version, Some(1));
    assert_eq!(
        Some(continued.run_id),
        correlation::child_run_id(store.context.project_id, &request, &resume.operation_id)?
    );
    assert_ne!(continued.run_id, resume.run_id);
    let attempt = store.pending_attempts(continued.run_id, &call)?.remove(0);
    assert_eq!(
        Some(attempt.attempt_id),
        correlation::child_attempt_id(
            store.context.project_id,
            continued.run_id,
            &request,
            attempt.task_id
        )?
    );
    resume.run_id = RunId::from_bytes([40; 16]);
    resume.attempt_id = AttemptId::from_bytes([41; 16]);
    assert_eq!(
        store.resume_fresh(&resume, "fixture", &call)?.run_id,
        continued.run_id
    );
    Ok(())
}

#[test]
fn nested_continuation_preflight_does_not_reuse_crashed_staging() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let mut child = input();
    child.request.parent_attempt = Some(AttemptId::from_bytes([9; 16]));
    child.request.lane_depth = Some(2);
    store.create(&child, &call)?;
    store.refuse_queued(child.run_id, "completed", &call)?;
    let resume = Resume {
        prior: child.attempts[0].attempt_id,
        expected_generation: 0,
        operation_id: "crashed-continuation".parse()?,
        run_id: RunId::from_bytes([30; 16]),
        attempt_id: AttemptId::from_bytes([31; 16]),
        prompt: "answer".into(),
        created_at: clock.wall_now()?,
    };
    let mut canonical = child.request.clone();
    canonical.child_attempt_id_version = Some(1);
    canonical.parent = Some(resume.prior);
    canonical.fresh_continuation = Some("fixture".into());
    canonical.tasks[0].prompt_digest = CanonicalV1::file_digest(resume.prompt.as_bytes());
    let expected =
        correlation::child_run_id(store.context.project_id, &canonical, &resume.operation_id)?
            .unwrap();
    let stale = store
        .context
        .state_root
        .as_path()
        .join(format!("runs/{expected}"));
    std::fs::DirBuilder::new().mode(0o700).create(&stale)?;
    std::fs::write(stale.join("retained"), "stale staging")?;
    let (continued, _) = store.resume_fresh_prepared(&resume, "fixture", None, &Ready, &call)?;
    assert_eq!(continued.run_id, expected);
    assert_eq!(
        std::fs::read_to_string(stale.join("retained"))?,
        "stale staging"
    );
    Ok(())
}

#[test]
fn nested_continuation_preserves_legacy_replay_even_before_request_publication() -> TestResult {
    for published in [true, false] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let mut child = input();
        child.request.parent_attempt = Some(AttemptId::from_bytes([9; 16]));
        child.request.lane_depth = Some(2);
        store.create(&child, &call)?;
        store.refuse_queued(child.run_id, "completed", &call)?;
        let prior = child.attempts[0].attempt_id;
        child.run_id = RunId::from_bytes([30; 16]);
        child.operation_id = "legacy-continuation".parse()?;
        child.request.parent = Some(prior);
        child.request.fresh_continuation = Some("fixture".into());
        child.attempts[0].attempt_id = AttemptId::from_bytes([31; 16]);
        store.create(&child, &call)?;
        if !published {
            let mut operation = store.operation(&child.operation_id, &call)?.unwrap();
            operation.state = OperationState::Prepared;
            store.db.execute(
                "UPDATE operations SET record=?1 WHERE operation=?2",
                rusqlite::params![crate::db::bytes(&operation)?, child.operation_id.as_str()],
            )?;
            std::fs::remove_file(
                store
                    .context
                    .state_root
                    .as_path()
                    .join(format!("runs/{}/request.json", child.run_id)),
            )?;
        }
        let resumed = store.resume_fresh(
            &Resume {
                prior,
                expected_generation: 0,
                operation_id: child.operation_id.clone(),
                run_id: RunId::from_bytes([40; 16]),
                attempt_id: AttemptId::from_bytes([41; 16]),
                prompt: child.attempts[0].prompt.clone(),
                created_at: child.created_at,
            },
            "fixture",
            &call,
        )?;
        assert_eq!(resumed.run_id, child.run_id);
        assert_eq!(store.request(resumed.run_id, &call)?, child.request);
    }
    Ok(())
}

#[test]
fn resume_replay_finishes_a_prepared_continuation() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    store.create(&run, &call)?;
    let prior = run.attempts[0].attempt_id;
    let fake = Fake {
        refuse: true,
        ..Fake::default()
    };
    store.start(run.run_id, &fake, &|_, _| unreachable!(), &call)?;
    let resume = |operation: &str, expected_generation, byte| -> Result<Resume> {
        Ok(Resume {
            prior,
            expected_generation,
            operation_id: provenant_core::OperationId::parse(operation)
                .map_err(|_| RunError::Validation("operation"))?,
            run_id: provenant_core::RunId::from_bytes([byte; 16]),
            attempt_id: AttemptId::from_bytes([byte + 1; 16]),
            prompt: "answer".into(),
            created_at: WallTime::from_micros(100).map_err(|_| RunError::Validation("time"))?,
        })
    };
    let original = resume("r1", 0, 20)?;
    let continued = store.resume(&original, &call)?;
    let mut operation = store
        .operation(&original.operation_id, &call)?
        .ok_or(RunError::NotFound)?;
    operation.state = OperationState::Prepared;
    store.db.execute(
        "UPDATE operations SET record=?1 WHERE project=?2 AND actor=?3 AND operation=?4",
        rusqlite::params![
            crate::db::bytes(&operation)?,
            store.context.project_id.to_string(),
            store.context.actor_id.to_string(),
            original.operation_id.as_str(),
        ],
    )?;

    let replayed = store.resume(&resume("r1", 0, 40)?, &call)?;
    assert_eq!(replayed.run_id, provenant_core::RunId::from_bytes([20; 16]));
    assert_eq!(replayed, continued);
    assert_eq!(
        store
            .operation(&original.operation_id, &call)?
            .ok_or(RunError::NotFound)?
            .state,
        OperationState::Accepted
    );
    let mut changed = resume("r1", 0, 40)?;
    changed.prompt = "other".into();
    assert!(matches!(
        store.resume(&changed, &call),
        Err(RunError::OperationConflict)
    ));
    for expected in [0, 1] {
        assert!(matches!(
            store.resume(&resume("r2", expected, 30)?, &call),
            Err(RunError::Conflict)
        ));
    }
    Ok(())
}

#[test]
fn fresh_resume_preserves_lineage_and_authority_without_provider_state() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let mut original = input();
    original.request.chair = Some(ChairBinding {
        host: provenant_core::HostId::from_bytes([32; 16]),
        actor: provenant_core::ActorId::from_bytes([33; 16]),
    });
    store.create(&original, &call)?;
    let fake = Fake {
        refuse: true,
        ..Fake::default()
    };
    store.start(original.run_id, &fake, &|_, _| unreachable!(), &call)?;
    let prior = original.attempts[0].attempt_id;
    let mut resume = Resume {
        prior,
        expected_generation: 0,
        operation_id: "fresh-resume".parse()?,
        run_id: RunId::from_bytes([90; 16]),
        attempt_id: AttemptId::from_bytes([91; 16]),
        prompt: "Original prompt\nPrior result tail\nNew prompt".into(),
        created_at: WallTime::from_micros(100)?,
    };
    let run = store.resume_fresh(&resume, "retained Codex rollout missing", &call)?;
    assert_eq!(
        store.resume_fresh(&resume, "retained Codex rollout missing", &call)?,
        run
    );
    let request = store.request(run.run_id, &call)?;
    assert_eq!(request.parent, Some(prior));
    assert_eq!(request.chair, original.request.chair);
    assert_eq!(
        request.tasks[0].authority_digest,
        original.request.tasks[0].authority_digest
    );
    assert_eq!(request.tasks[0].writer, original.request.tasks[0].writer);
    assert_eq!(request.tasks[0].route, original.request.tasks[0].route);
    assert_eq!(
        request.fresh_continuation.as_deref(),
        Some("retained Codex rollout missing")
    );
    store.start(run.run_id, &fake, &|_, _| unreachable!(), &call)?;
    assert_eq!(fake.resumed.get(), None);
    let record = store.record(resume.attempt_id, &call)?;
    assert_eq!(record.retry_of, Some(prior));
    assert_eq!(record.continuation.as_deref(), Some("fresh"));
    resume.prompt = "different".into();
    assert!(matches!(
        store.resume_fresh(&resume, "retained Codex rollout missing", &call),
        Err(RunError::OperationConflict)
    ));
    Ok(())
}

#[test]
fn cancel_settles_a_prepared_attempt_nobody_holds() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let batch = two_tasks(1);
    store.create(&batch, &call)?;
    let orphan = batch.attempts[0].attempt_id;
    store.mark_starting(
        &store.record(orphan, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let scope = provenant_supervisor::custody::Scope {
        run: batch.run_id,
        attempt: orphan,
        generation: 1,
    };
    drop(provenant_supervisor::fixture::acquire_writer(
        store.attempt_dir(batch.run_id, orphan, false)?,
        scope,
        call.budget,
        &cancel,
        &clock,
    )?);
    let system = provenant_os::clock::SystemClock::new();
    let spent = Request {
        clock: &system,
        budget: provenant_core::WaitBudget::cli(provenant_core::Clock::mono_now(&system), 1)?,
        cancel: &cancel,
    };
    assert_eq!(
        store.cancel(orphan, "stop", &spent)?,
        CancelReceipt::Cancelled
    );
    assert_eq!(terminal(&store, orphan, &call)?, Outcome::Rejected);
    assert_eq!(
        store.cancel(orphan, "stop", &spent)?,
        CancelReceipt::AlreadyTerminal
    );
    Ok(())
}

#[test]
fn reconcile_unreadable_reservation_only_retains_existing_temporary_data() -> TestResult {
    for has_temporary in [false, true] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let context = context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = two_tasks(2);
        store.create(&run, &call)?;
        let terminal = run.attempts[0].attempt_id;
        let sibling = run.attempts[1].attempt_id;
        let temporary = store.temporary_path(terminal, &call)?;
        if has_temporary {
            std::fs::write(temporary.join("keep.lock"), "keep")?;
        }
        store.withdraw(
            terminal,
            AttemptState::Queued,
            Outcome::Cancelled,
            "fixture",
            WallTime::from_micros(150)?,
            &call,
        )?;
        if !has_temporary {
            // A crash after removal can leave the cleanup hint uncleared.
            store.set_temporary_retained(terminal, true, &call)?;
        }
        let custody = context
            .state_root
            .as_path()
            .join("worktrees")
            .join(context.project_id.to_string())
            .join("corrupt");
        std::fs::create_dir_all(&custody)?;
        std::fs::write(custody.join("reservation.json"), b"corrupt")?;
        assert_eq!(store.writer_fence_listing()?.issues.len(), 1);

        let found = store.reconcile(run.run_id, &Fake::default(), &call)?;
        if has_temporary {
            assert!(matches!(found.as_slice(), [
                (id, Reconciled::TemporaryRetained { path, reason }),
                (other, Reconciled::Queued),
            ] if *id == terminal && *other == sibling && *path == temporary
                && reason.contains("writer reservation unreadable")));
            assert!(temporary.join("keep.lock").exists());
        } else {
            assert_eq!(found, vec![(sibling, Reconciled::Queued)]);
        }
    }
    Ok(())
}

#[test]
fn reconcile_backfills_legacy_temporary_without_changing_terminal_records() -> TestResult {
    for path_kind in ["absent", "directory", "unreadable"] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let context = context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = input();
        let id = run.attempts[0].attempt_id;
        store.create(&run, &call)?;
        let temporary = store.temporary_path(id, &call)?;
        let before = store
            .withdraw(
                id,
                AttemptState::Queued,
                Outcome::Cancelled,
                "fixture",
                WallTime::from_micros(150)?,
                &call,
            )?
            .ok_or("not withdrawn")?;
        // Reproduce leaks left by an older binary without a cleanup hint.
        if path_kind == "directory" {
            // Older binaries also created tmp private to the store owner.
            std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700)
                .create(&temporary)?;
            std::fs::write(temporary.join("legacy.lock"), "discard")?;
        } else if path_kind == "unreadable" {
            let attempt = temporary.parent().ok_or("attempt directory")?;
            std::fs::rename(attempt, attempt.with_extension("moved"))?;
            std::os::unix::fs::symlink(attempt.with_extension("moved"), attempt)?;
        }
        store.db.execute_batch("DROP INDEX retained_temporary; ALTER TABLE attempts DROP COLUMN tmp_retained; PRAGMA user_version=3;")?;
        drop(store);

        let store = RunStore::open(&context, &call)?;
        if path_kind == "unreadable" {
            let attempt = temporary.parent().ok_or("attempt directory")?;
            std::fs::remove_file(attempt)?;
            std::fs::rename(attempt.with_extension("moved"), attempt)?;
        }
        assert_eq!(store.record(id, &call)?, before);
        assert!(
            store
                .db
                .execute(
                    "UPDATE attempts SET record=record WHERE id=?1",
                    [id.to_string()]
                )
                .is_err()
        );
        for alias in ["rowid", "oid", "_rowid_"] {
            assert!(
                store
                    .db
                    .execute(
                        &format!("UPDATE attempts SET {alias}={alias}+100 WHERE id=?1"),
                        [id.to_string()]
                    )
                    .is_err()
            );
        }
        assert!(
            store
                .db
                .execute("DELETE FROM attempts WHERE id=?1", [id.to_string()])
                .is_err()
        );
        let expected = if path_kind == "absent" {
            vec![]
        } else {
            vec![run.run_id]
        };
        assert_eq!(store.open_runs(10, &call)?, expected);
        if path_kind == "directory" {
            assert_eq!(
                store.reconcile(run.run_id, &Fake::default(), &call)?,
                vec![(id, Reconciled::TemporaryRemoved)]
            );
            assert!(!temporary.exists());
            assert!(store.open_runs(10, &call)?.is_empty());
        } else if path_kind == "unreadable" {
            // The migration retained the unreadable candidate. Now that its
            // anchor is repaired, absence clears the hint without a result.
            assert!(
                store
                    .reconcile(run.run_id, &Fake::default(), &call)?
                    .is_empty()
            );
        }
        drop(store);
        let store = RunStore::open(&context, &call)?;
        assert_eq!(store.record(id, &call)?, before);
        assert!(store.open_runs(10, &call)?.is_empty());
    }
    Ok(())
}

#[test]
fn reconcile_retries_terminal_temporary_removal() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    let id = run.attempts[0].attempt_id;
    store.create(&run, &call)?;
    let temporary = store.temporary_path(id, &call)?;
    // Settlement protects lock names while custody may still be retained.
    // Reconcile must dispose of these build artefacts once custody clears.
    let incremental = temporary.join("candidate-0/target/debug/incremental/x");
    std::fs::create_dir_all(&incremental)?;
    std::fs::write(incremental.join("s-1.lock"), "discard")?;
    let observed = exit();
    seal_exit(&store, &observed)?;
    store.publish_terminal(id, 1, &super::tests::publication(), &observed, &call)?;
    assert_eq!(terminal(&store, id, &call)?, Outcome::Succeeded);
    assert!(temporary.is_dir());

    assert_eq!(store.open_runs(10, &call)?, vec![run.run_id]);
    assert_eq!(
        store.reconcile(run.run_id, &Fake::default(), &call)?,
        vec![(id, Reconciled::TemporaryRemoved)]
    );
    assert!(!temporary.exists());
    assert!(store.open_runs(10, &call)?.is_empty());
    assert_eq!(terminal(&store, id, &call)?, Outcome::Succeeded);
    Ok(())
}

#[test]
fn reconcile_preserves_protected_locks_outside_temporary() -> TestResult {
    use provenant_os::fs::FsError;
    use provenant_os::fs::LockMode;
    use std::ffi::OsStr;

    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    let id = run.attempts[0].attempt_id;
    store.create(&run, &call)?;
    let temporary = store.temporary_path(id, &call)?;
    std::fs::write(temporary.join("writer.lock"), "disposable payload")?;
    store.withdraw(
        id,
        AttemptState::Queued,
        Outcome::Cancelled,
        "fixture",
        WallTime::from_micros(150)?,
        &call,
    )?;
    let attempt = store.attempt_dir(run.run_id, id, false)?;
    let run_dir = store.run_dir(run.run_id, false)?;
    drop(attempt.try_lock(OsStr::new("writer.lock"), LockMode::Exclusive)?);
    // The run lock may stay held while reconcile removes attempt payload.
    let _held = run_dir.try_lock(OsStr::new("publication.lock"), LockMode::Exclusive)?;
    let locks = [(&attempt, "writer.lock"), (&run_dir, "publication.lock")];
    let before = locks
        .iter()
        .map(|(dir, name)| dir.metadata_at(OsStr::new(name), false))
        .collect::<provenant_os::fs::Result<Vec<_>>>()?;

    assert_eq!(
        store.reconcile(run.run_id, &Fake::default(), &call)?,
        vec![(id, Reconciled::TemporaryRemoved)]
    );
    assert!(!temporary.exists());
    for ((dir, name), identity) in locks.into_iter().zip(before) {
        assert!(matches!(
            dir.unlink(OsStr::new(name)),
            Err(FsError::ReservedLockName { .. })
        ));
        assert!(identity.same_identity(&dir.metadata_at(OsStr::new(name), false)?));
    }
    assert!(matches!(
        run_dir.try_lock(OsStr::new("publication.lock"), LockMode::Exclusive),
        Err(FsError::LockTimeout { .. })
    ));
    Ok(())
}

#[test]
fn reconcile_reports_temporary_removal_failures_and_continues() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = two_tasks(2);
    store.create(&run, &call)?;
    for new in &run.attempts {
        let temporary = store.temporary_path(new.attempt_id, &call)?;
        std::fs::write(temporary.join("payload.lock"), "discard")?;
        store.withdraw(
            new.attempt_id,
            AttemptState::Queued,
            Outcome::Cancelled,
            "fixture",
            WallTime::from_micros(150)?,
            &call,
        )?;
    }
    let retained = run.attempts[0].attempt_id;
    let removed = run.attempts[1].attempt_id;
    let temporary = store.temporary_path(retained, &call)?;
    std::fs::write(temporary.join("payload"), "retain")?;
    let _held = store.attempt_dir(run.run_id, retained, false)?.try_lock(
        std::ffi::OsStr::new("writer.lock"),
        provenant_os::fs::LockMode::Exclusive,
    )?;
    let removable = store.temporary_path(removed, &call)?;
    std::fs::write(removable.join("payload"), "discard")?;

    let found = store.reconcile(run.run_id, &Fake::default(), &call)?;
    assert!(matches!(found.as_slice(), [
        (id, Reconciled::TemporaryRetained { path, reason }),
        (other, Reconciled::TemporaryRemoved),
    ] if *id == retained && *other == removed && *path == temporary
        && reason.contains("writer.lock")));
    assert!(temporary.join("payload").exists());
    assert!(!removable.exists());
    assert_eq!(terminal(&store, retained, &call)?, Outcome::Cancelled);
    Ok(())
}

#[test]
fn reconcile_leaves_nonterminal_and_reserved_temporary_data() -> TestResult {
    for reserved in [false, true] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let run = input();
        let id = run.attempts[0].attempt_id;
        store.create(&run, &call)?;
        let temporary = store.temporary_path(id, &call)?;
        std::fs::write(temporary.join("keep.lock"), "keep")?;
        if reserved {
            store.mark_starting(
                &store.record(id, &call)?,
                &route(),
                &[],
                &Default::default(),
                None,
                &call,
            )?;
            store.withdraw(
                id,
                AttemptState::Starting,
                Outcome::Rejected,
                "fixture",
                WallTime::from_micros(150)?,
                &call,
            )?;
        }
        // Also cover a reservation retained after its owner releases the lock.
        if reserved {
            drop(provenant_supervisor::fixture::acquire_writer(
                store.attempt_dir(run.run_id, id, false)?,
                RunStore::scope(&store.record(id, &call)?),
                call.budget,
                &cancel,
                &clock,
            )?);
        }
        std::fs::write(temporary.join("keep.lock"), "keep")?;
        let found = store.reconcile(run.run_id, &Fake::default(), &call)?;
        if reserved {
            assert!(
                matches!(found.as_slice(), [(attempt, Reconciled::TemporaryRetained { reason, .. })]
                if *attempt == id && reason.contains("reservation retained"))
            );
        } else {
            assert_eq!(found, vec![(id, Reconciled::Queued)]);
        }
        assert_eq!(
            std::fs::read_to_string(temporary.join("keep.lock"))?,
            "keep"
        );
    }
    Ok(())
}

#[test]
fn reconcile_terminal_writer_reservation_without_temporary() -> TestResult {
    for prepared in [false, true] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let context = context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = input();
        let id = run.attempts[0].attempt_id;
        store.create(&run, &call)?;
        let temporary = store.temporary_path(id, &call)?;
        store.mark_starting(
            &store.record(id, &call)?,
            &route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let custody = context
            .state_root
            .as_path()
            .join("worktrees")
            .join(context.project_id.to_string())
            .join("lane");
        std::fs::create_dir_all(&custody)?;
        let held = provenant_supervisor::fixture::acquire_writer(
            Directory::open_anchor(&custody)?,
            RunStore::scope(&store.record(id, &call)?),
            call.budget,
            &cancel,
            &clock,
        )?;
        let _held = if prepared {
            drop(held);
            None
        } else {
            Some(held)
        };
        store.withdraw(
            id,
            AttemptState::Starting,
            Outcome::Rejected,
            "fixture",
            WallTime::from_micros(150)?,
            &call,
        )?;
        assert!(!temporary.exists());
        assert_eq!(store.open_runs(10, &call)?, vec![run.run_id]);
        let found = store.reconcile(run.run_id, &Fake::default(), &call)?;
        if prepared {
            assert_eq!(found, vec![(id, Reconciled::Withdrawn)]);
            assert!(store.writer_fences()?.is_empty());
            assert!(store.open_runs(10, &call)?.is_empty());
        } else {
            assert!(
                matches!(found.as_slice(), [(attempt, Reconciled::Uncertain { reason })]
                if *attempt == id && reason.to_string().contains("chair recovery required"))
            );
            assert_eq!(store.writer_fences()?.len(), 1);
            assert_eq!(store.open_runs(10, &call)?, vec![run.run_id]);
        }
        assert!(!temporary.exists());
    }
    Ok(())
}

#[test]
fn reconcile_leaves_temporary_data_behind_a_writer_reservation() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let mut run = input();
    run.request.tasks[0].writer = Some(WriterRequest {
        project: temp.path().join("checkout"),
        worktree: temp.path().join("checkout/.worktrees/lane"),
    });
    let id = run.attempts[0].attempt_id;
    store.create(&run, &call)?;
    let temporary = store.temporary_path(id, &call)?;
    let candidate = temporary.join("candidate-0");
    std::fs::create_dir(&candidate)?;
    std::fs::write(candidate.join("x.lock"), "keep")?;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let custody = context
        .state_root
        .as_path()
        .join("worktrees")
        .join(context.project_id.to_string())
        .join("lane");
    std::fs::create_dir_all(&custody)?;
    let held = provenant_supervisor::fixture::acquire_writer(
        Directory::open_anchor(&custody)?,
        RunStore::scope(&store.record(id, &call)?),
        call.budget,
        &cancel,
        &clock,
    )?;
    store.withdraw(
        id,
        AttemptState::Starting,
        Outcome::Rejected,
        "fixture",
        WallTime::from_micros(150)?,
        &call,
    )?;
    assert_eq!(std::fs::read_to_string(candidate.join("x.lock"))?, "keep");
    // An unreadable sibling must not hide this attempt's readable reservation.
    let corrupt = custody.with_file_name("corrupt");
    std::fs::create_dir_all(&corrupt)?;
    std::fs::write(corrupt.join("reservation.json"), b"corrupt")?;
    assert_eq!(store.writer_fence_listing()?.issues.len(), 1);
    assert!(
        matches!(store.reconcile(run.run_id, &Fake::default(), &call)?.as_slice(),
        [(attempt, Reconciled::Uncertain { reason: UncertainReason::WriterReservationRetained { .. } })]
        if *attempt == id)
    );
    assert_eq!(std::fs::read_to_string(candidate.join("x.lock"))?, "keep");
    // Reconcile can withdraw the prepared reservation after its lock is released.
    drop(held);
    assert!(matches!(
        store.reconcile(run.run_id, &Fake::default(), &call)?.as_slice(),
        [(attempt, Reconciled::Withdrawn)] if *attempt == id
    ));
    assert!(store.writer_fences()?.is_empty());
    assert!(candidate.join("x.lock").exists());
    // The unreadable sibling remains a separate barrier until removed.
    assert!(matches!(
        store.reconcile(run.run_id, &Fake::default(), &call)?.as_slice(),
        [(attempt, Reconciled::TemporaryRetained { .. })] if *attempt == id
    ));
    assert!(candidate.join("x.lock").exists());
    std::fs::remove_file(corrupt.join("reservation.json"))?;
    assert_eq!(
        store.reconcile(run.run_id, &Fake::default(), &call)?,
        vec![(id, Reconciled::TemporaryRemoved)]
    );
    assert!(!temporary.exists());
    Ok(())
}

#[test]
fn reconcile_temporary_removal_never_follows_symlinks() -> TestResult {
    use std::os::unix::fs::symlink;
    for link_at_root in [false, true] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let run = input();
        let id = run.attempts[0].attempt_id;
        store.create(&run, &call)?;
        let temporary = store.temporary_path(id, &call)?;
        std::fs::write(temporary.join("keep.lock"), "keep")?;
        store.withdraw(
            id,
            AttemptState::Queued,
            Outcome::Cancelled,
            "fixture",
            WallTime::from_micros(150)?,
            &call,
        )?;
        let outside = temp.path().join("outside");
        std::fs::create_dir(&outside)?;
        std::fs::write(outside.join("keep"), "keep")?;
        let attempt = store.attempt_dir(run.run_id, id, false)?;
        if link_at_root {
            std::fs::remove_dir_all(&temporary)?;
            symlink(&outside, &temporary)?;
        } else {
            std::fs::create_dir_all(temporary.join("candidate-0/target"))?;
            symlink(&outside, temporary.join("candidate-0/target/outside.lock"))?;
        }
        assert_eq!(store.open_runs(10, &call)?, vec![run.run_id]);
        assert_eq!(
            store.reconcile(run.run_id, &Fake::default(), &call)?,
            vec![(id, Reconciled::TemporaryRemoved)]
        );
        assert!(std::fs::symlink_metadata(&temporary).is_err());
        assert_eq!(std::fs::read_to_string(outside.join("keep"))?, "keep");
        assert!(artifacts::read_sealed(&attempt, "result.md", RESULT_LIMIT)?.is_some());
        assert!(artifacts::read_sealed(&attempt, "prompt.txt", RECORD_LIMIT)?.is_some());
    }
    Ok(())
}

#[test]
fn reconcile_removes_owner_only_temporary_and_retains_shared_modes() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    for mode in [0o500, 0o750, 0o755] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let run = input();
        let id = run.attempts[0].attempt_id;
        store.create(&run, &call)?;
        let temporary = store.temporary_path(id, &call)?;
        std::fs::write(temporary.join("keep.lock"), "keep")?;
        store.withdraw(
            id,
            AttemptState::Queued,
            Outcome::Cancelled,
            "fixture",
            WallTime::from_micros(150)?,
            &call,
        )?;
        let nested = temporary.join("candidate-0/target");
        std::fs::create_dir_all(&nested)?;
        std::fs::write(nested.join("s-1.lock"), "keep")?;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(mode))?;
        let found = store.reconcile(run.run_id, &Fake::default(), &call)?;
        if mode & 0o077 == 0 {
            assert_eq!(found, vec![(id, Reconciled::TemporaryRemoved)]);
            assert!(std::fs::symlink_metadata(&temporary).is_err());
            continue;
        }
        assert!(
            matches!(found.as_slice(), [(attempt, Reconciled::TemporaryRetained { path, reason })]
            if *attempt == id && *path == temporary && reason.contains("not private")),
            "{found:?}"
        );
        assert_eq!(
            std::fs::metadata(&temporary)?.permissions().mode() & 0o7777,
            mode
        );
        assert_eq!(
            std::fs::read_to_string(temporary.join("keep.lock"))?,
            "keep"
        );
        assert_eq!(std::fs::read_to_string(nested.join("s-1.lock"))?, "keep");
    }
    Ok(())
}

#[test]
fn cancel_queued_work_without_an_attempt_directory() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let run = input();
    store.create(&run, &call)?;
    let id = run.attempts[0].attempt_id;
    let attempt = context
        .state_root
        .as_path()
        .join(format!("runs/{}/attempts/{id}", run.run_id));
    // The queued transaction can survive a crash before directory publication.
    std::fs::remove_dir_all(attempt)?;
    assert_eq!(store.cancel(id, "stop", &call)?, CancelReceipt::Cancelled);
    assert_eq!(terminal(&store, id, &call)?, Outcome::Cancelled);
    Ok(())
}

#[test]
fn reconcile_recovers_a_writer_from_a_previous_boot() -> TestResult {
    recover_rebooted_writer(false, false, true, NEW_BOOT)
}

#[test]
fn reconcile_recovers_writer_when_device_numbers_change_after_reboot() -> TestResult {
    recover_rebooted_writer_inner(false, false, true, NEW_BOOT, false, true)
}

#[test]
fn changed_device_numbers_on_same_boot_keep_writer_custody() -> TestResult {
    recover_rebooted_writer_inner(false, false, true, OLD_BOOT, false, true)
}

#[test]
fn reconcile_settles_a_normal_terminal_attempt_after_reboot() -> TestResult {
    let observer = RebootObserver(NEW_BOOT);
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let run = input();
    store.create(&run, &call)?;
    let id = run.attempts[0].attempt_id;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let attempt = store.attempt_dir(run.run_id, id, false)?;
    let mut observed = exit();
    observed.scope = RunStore::scope(&store.record(id, &call)?);
    seal_exit(&store, &observed)?;
    let record = store.finish(id, &attempt, &observed, &Fake::default(), &call)?;
    let path = context
        .state_root
        .as_path()
        .join(format!("runs/{}/attempts/{id}", run.run_id));
    // Recreate the reservation retained by a crash after the canonical commit.
    drop(provenant_supervisor::fixture::acquire_writer(
        attempt.try_clone()?,
        observed.scope,
        call.budget,
        &cancel,
        &clock,
    )?);
    let reservation = path.join("reservation.json");
    let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&reservation)?)?;
    let meta = attempt.metadata()?;
    value["boot"] = "darwin:00000000-0000-0000-0000-000000000000".into();
    value["guardian"] = serde_json::json!({"boot":value["boot"], "pid":123, "native_start":1, "group_id":123, "session_id":123});
    value["launch_state"] = "spawned".into();
    value["attempt_device"] = meta.device.into();
    value["attempt_inode"] = meta.inode.into();
    std::fs::write(&reservation, serde_json::to_vec(&value)?)?;
    artifacts::subdir(&attempt, std::ffi::OsStr::new("scratch"), true)?;
    // Recreate the private tmp root with production permissions, independent of umask.
    artifacts::subdir(&attempt, std::ffi::OsStr::new("tmp"), true)?;
    let temporary = path.join("tmp/candidate-0");
    std::fs::create_dir_all(&temporary)?;
    std::fs::write(temporary.join("payload.lock"), b"build output")?;
    store.set_temporary_retained(id, true, &call)?;
    let manifest_path = path.join("quarantine/manifest.json");
    let manifest = std::fs::read(&manifest_path)?;
    std::fs::write(&manifest_path, b"invalid manifest")?;
    assert!(
        matches!(store.reconcile_with_observer(run.run_id, &Fake::default(), &call, &observer)?.as_slice(),
        [(attempt, Reconciled::Uncertain { .. })] if *attempt == id)
    );
    assert!(
        reservation.exists(),
        "failed verification retains the reservation"
    );
    assert!(temporary.exists(), "uncertain custody retains tmp");
    assert_eq!(store.record(id, &call)?, record);
    std::fs::write(manifest_path, manifest)?;
    assert_eq!(
        store.reconcile_with_observer(run.run_id, &Fake::default(), &call, &observer)?,
        vec![(id, Reconciled::Finished)]
    );
    let cleanup = controls::REBOOT_TEMPORARY_CLEANUP.with(|result| result.borrow_mut().take());
    assert_eq!(cleanup, Some(Ok(Some(Reconciled::TemporaryRemoved))));
    assert!(
        !path.join("scratch").exists(),
        "normal settlement removes empty launch staging"
    );
    assert!(
        path.join("release.json").exists(),
        "normal settlement verifies release evidence"
    );
    assert!(!reservation.exists());
    assert!(!path.join("tmp").exists(), "released tmp is removed");
    assert_eq!(store.record(id, &call)?, record);
    Ok(())
}

#[test]
fn cancel_recovers_a_writer_from_a_previous_boot() -> TestResult {
    recover_rebooted_writer(true, false, true, NEW_BOOT)
}

#[test]
fn reconcile_retries_reboot_release_after_the_terminal_commit() -> TestResult {
    recover_rebooted_writer(false, true, true, NEW_BOOT)
}

#[test]
fn cancel_retries_reboot_release_after_the_terminal_commit() -> TestResult {
    recover_rebooted_writer(true, true, true, NEW_BOOT)
}

#[test]
fn reconcile_retries_reboot_release_for_a_reader() -> TestResult {
    recover_rebooted_writer(false, true, false, NEW_BOOT)
}

#[test]
fn unreserved_cancel_keeps_unidentified_provider_uncertain() -> TestResult {
    placed_writer_without_reservation(true, 0)
}

#[test]
fn unreserved_reconcile_keeps_unidentified_provider_uncertain() -> TestResult {
    placed_writer_without_reservation(false, 0)
}

#[test]
fn unreserved_live_guardian_keeps_attempt_open() -> TestResult {
    placed_writer_without_reservation(true, 1)?;
    placed_writer_without_reservation(false, 1)
}

#[test]
fn unreserved_live_provider_keeps_attempt_open() -> TestResult {
    placed_writer_without_reservation(true, 3)?;
    placed_writer_without_reservation(false, 3)
}

#[test]
fn unreserved_recorded_process_tree_before_reboot_is_lost() -> TestResult {
    placed_writer_without_reservation(true, 2)?;
    placed_writer_without_reservation(false, 2)
}

fn placed_writer_without_reservation(cancel_run: bool, evidence: u8) -> TestResult {
    placed_writer_with_missing_or_stale_reservation(RecoveryCase {
        cancel_run,
        evidence,
        reservation: ReservationCase::Missing,
        removed: false,
    })
}

#[test]
fn stale_reservation_before_reboot_is_reclaimed() -> TestResult {
    for cancel_run in [true, false] {
        placed_writer_with_missing_or_stale_reservation(RecoveryCase {
            cancel_run,
            evidence: 2,
            reservation: ReservationCase::Stale,
            removed: false,
        })?;
    }
    Ok(())
}

#[test]
fn removed_worktree_before_reboot_is_lost() -> TestResult {
    for cancel_run in [true, false] {
        placed_writer_with_missing_or_stale_reservation(RecoveryCase {
            cancel_run,
            evidence: 2,
            reservation: ReservationCase::Missing,
            removed: true,
        })?;
    }
    recover_rebooted_writer_removed_worktree()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReservationCase {
    Missing,
    Stale,
    Newer,
    DifferentAttempt,
}

struct RecoveryCase {
    cancel_run: bool,
    evidence: u8,
    reservation: ReservationCase,
    removed: bool,
}

#[test]
fn stale_reservation_with_current_boot_process_is_retained() -> TestResult {
    placed_writer_with_missing_or_stale_reservation(RecoveryCase {
        cancel_run: false,
        evidence: 1,
        reservation: ReservationCase::Stale,
        removed: false,
    })
}

#[test]
fn newer_generation_reservation_is_retained() -> TestResult {
    placed_writer_with_missing_or_stale_reservation(RecoveryCase {
        cancel_run: false,
        evidence: 2,
        reservation: ReservationCase::Newer,
        removed: false,
    })
}

#[test]
fn different_attempt_reservation_is_retained() -> TestResult {
    placed_writer_with_missing_or_stale_reservation(RecoveryCase {
        cancel_run: false,
        evidence: 2,
        reservation: ReservationCase::DifferentAttempt,
        removed: false,
    })
}

fn placed_writer_with_missing_or_stale_reservation(case: RecoveryCase) -> TestResult {
    let RecoveryCase {
        cancel_run,
        evidence,
        reservation: reservation_case,
        removed,
    } = case;
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let mut run = input();
    let worktree = temp.path().join("lane");
    std::fs::create_dir(&worktree)?;
    run.request.tasks[0].writer = Some(WriterRequest {
        project: temp.path().to_path_buf(),
        worktree: worktree.clone(),
    });
    store.create(&run, &call)?;
    let id = run.attempts[0].attempt_id;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store
        .claim(id, &at_micros(100), &call)?
        .ok_or("not admitted")?;
    if reservation_case == ReservationCase::Stale {
        let old = store.record(id, &call)?;
        let mut next = old.clone();
        next.generation += 1;
        next.revision += 1;
        store.write(&call, |tx| {
            crate::admission::update_attempt(tx, &old, &next)
        })?;
    }
    let scope = RunStore::scope(&store.record(id, &call)?);
    let attempt = store.attempt_dir(run.run_id, id, false)?;
    let custody = store
        .writer_registry
        .join("worktrees")
        .join(context.project_id.to_string())
        .join(provenant_confinement::worktrees::state_dir_name(&worktree));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&custody)?;
    drop(provenant_supervisor::fixture::acquire_writer(
        Directory::open_anchor(&custody)?,
        scope,
        call.budget,
        &cancel,
        &clock,
    )?);
    let summary =
        serde_json::json!({"schema_version":1, "scope":scope, "requester":context.actor_id});
    artifacts::seal(
        &attempt,
        "launch.json",
        &serde_json::to_vec(&summary)?,
        RECORD_LIMIT,
    )?;
    if evidence != 0 {
        let (guardian, provider) = if evidence == 1 || evidence == 3 {
            let provenant_supervisor::census::ProcessObservation::Present(fact) =
                provenant_supervisor::census::current_process()
            else {
                return Err("current process identity unavailable".into());
            };
            let mut guardian = fact.identity.clone();
            if evidence == 3 {
                guardian.native_start = guardian.native_start.saturating_add(1);
            }
            (guardian, fact.identity)
        } else {
            use provenant_supervisor::census::ProcessIdentity;
            (
                ProcessIdentity {
                    boot: OLD_BOOT.into(),
                    pid: 100,
                    native_start: 1,
                    group_id: 100,
                    session_id: 100,
                },
                ProcessIdentity {
                    boot: OLD_BOOT.into(),
                    pid: 101,
                    native_start: 2,
                    group_id: 101,
                    session_id: 100,
                },
            )
        };
        attempt.atomic_write(
            std::ffi::OsStr::new("process.json"),
            ".checkpoint-",
            &serde_json::to_vec(
                &serde_json::json!({"schema_version": 1, "scope": scope, "guardian": guardian, "provider": provider}),
            )?,
            provenant_os::fs::PublishMode::Replace,
        )?;
    }
    // A launch summary alone does not prove the provider was never spawned.
    let reservation = custody.join("reservation.json");
    if reservation_case != ReservationCase::Missing {
        let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&reservation)?)?;
        match reservation_case {
            ReservationCase::Stale => value["scope"]["generation"] = (scope.generation - 1).into(),
            ReservationCase::Newer => value["scope"]["generation"] = (scope.generation + 1).into(),
            ReservationCase::DifferentAttempt => {
                value["scope"]["attempt"] = serde_json::to_value(AttemptId::from_bytes([9; 16]))?
            }
            ReservationCase::Missing => unreachable!(),
        }
        std::fs::write(&reservation, serde_json::to_vec(&value)?)?;
    } else {
        std::fs::remove_file(&reservation)?;
    }
    if removed {
        std::fs::remove_dir(&worktree)?;
    }
    let system = provenant_supervisor::census::SystemProcessObserver;
    let reboot = RebootObserver(NEW_BOOT);
    let observer: &dyn provenant_supervisor::census::ProcessObserver =
        if evidence == 2 { &reboot } else { &system };
    let should_settle = evidence == 2
        && matches!(
            reservation_case,
            ReservationCase::Missing | ReservationCase::Stale
        );
    if cancel_run {
        assert_eq!(
            store.cancel_with_observer(id, "stop", &call, observer)?,
            if should_settle {
                CancelReceipt::AlreadyTerminal
            } else {
                CancelReceipt::Requested
            }
        );
    } else {
        assert!(
            store
                .reconcile_with_observer(run.run_id, &Fake::default(), &call, observer)?
                .contains(&(
                    id,
                    match evidence {
                        1 => Reconciled::Held,
                        2 if should_settle => Reconciled::Finished,
                        _ => Reconciled::Uncertain {
                            reason: UncertainReason::GuardianNotObservable
                        },
                    }
                ))
        );
    }
    if should_settle {
        assert_eq!(terminal(&store, id, &call)?, Outcome::Lost);
        assert!(!reservation.exists());
        if removed {
            assert!(
                matches!(store.record(id, &call)?.state, AttemptState::Terminal { reason: Some(reason), .. } if reason.contains("worktree"))
            );
        }
    } else {
        assert!(!matches!(
            store.record(id, &call)?.state,
            AttemptState::Terminal { .. }
        ));
        if reservation_case != ReservationCase::Missing {
            assert!(reservation.exists());
        }
    }
    Ok(())
}

const OLD_BOOT: &str = "darwin:00000000-0000-0000-0000-000000000000";
const NEW_BOOT: &str = "darwin:11111111-1111-1111-1111-111111111111";

struct RebootObserver(&'static str);
impl provenant_supervisor::census::ProcessObserver for RebootObserver {
    fn boot_identity(
        &self,
    ) -> std::result::Result<String, provenant_supervisor::census::CensusError> {
        Ok(self.0.into())
    }
    fn observe(&self, _: i64) -> provenant_supervisor::census::ProcessObservation {
        unreachable!("reboot proof needs only the boot identity")
    }
    fn census(
        &self,
        _: usize,
    ) -> std::result::Result<
        provenant_supervisor::census::Census,
        provenant_supervisor::census::CensusError,
    > {
        unreachable!("reboot proof needs only the boot identity")
    }
}

#[test]
fn same_boot_with_free_locks_stays_uncertain() -> TestResult {
    recover_rebooted_writer(false, false, true, OLD_BOOT)
}

fn recover_rebooted_writer(
    cancel_run: bool,
    interrupted: bool,
    writer: bool,
    boot: &'static str,
) -> TestResult {
    recover_rebooted_writer_inner(cancel_run, interrupted, writer, boot, false, false)
}

fn recover_rebooted_writer_removed_worktree() -> TestResult {
    recover_rebooted_writer_inner(false, false, true, NEW_BOOT, true, false)
}

fn recover_rebooted_writer_inner(
    cancel_run: bool,
    interrupted: bool,
    writer: bool,
    boot: &'static str,
    removed: bool,
    changed_device: bool,
) -> TestResult {
    let observer = RebootObserver(boot);
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let mut run = input();
    let worktree = temp.path().join("lane");
    std::fs::create_dir(&worktree)?;
    std::fs::write(worktree.join("unfinished.rs"), "keep my work")?;
    run.request.tasks[0].writer = writer.then(|| WriterRequest {
        project: temp.path().to_path_buf(),
        worktree: worktree.clone(),
    });
    store.create(&run, &call)?;
    let id = run.attempts[0].attempt_id;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store
        .claim(id, &at_micros(100), &call)?
        .ok_or("not admitted")?;
    let before = store.record(id, &call)?;
    let scope = RunStore::scope(&before);
    let attempt = store.attempt_dir(run.run_id, id, false)?;
    let custody = if writer {
        store
            .writer_registry
            .join("worktrees")
            .join(context.project_id.to_string())
            .join(provenant_confinement::worktrees::state_dir_name(&worktree))
    } else {
        context
            .state_root
            .as_path()
            .join(format!("runs/{}/attempts/{id}", run.run_id))
    };
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&custody)?;
    drop(provenant_supervisor::fixture::acquire_writer(
        Directory::open_anchor(&custody)?,
        scope,
        call.budget,
        &cancel,
        &clock,
    )?);
    let reservation = custody.join("reservation.json");
    let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&reservation)?)?;
    let meta = attempt.metadata()?;
    let guardian = serde_json::json!({
        "boot": "darwin:00000000-0000-0000-0000-000000000000",
        "pid": 123, "native_start": 1, "group_id": 123, "session_id": 123,
    });
    value["boot"] = guardian["boot"].clone();
    value["guardian"] = guardian.clone();
    value["launch_state"] = "spawned".into();
    value["attempt_device"] = meta.device.into();
    value["attempt_inode"] = meta.inode.into();
    if changed_device {
        value["reservation_device"] =
            (value["reservation_device"].as_u64().ok_or("device")? + 1).into();
        value["attempt_device"] = (meta.device + 1).into();
    }
    std::fs::write(&reservation, serde_json::to_vec(&value)?)?;
    let summary =
        serde_json::json!({"schema_version":1, "scope":scope, "requester":context.actor_id});
    artifacts::seal(
        &attempt,
        "launch.json",
        &serde_json::to_vec(&summary)?,
        RECORD_LIMIT,
    )?;
    let checkpoint = serde_json::to_vec(
        &serde_json::json!({"schema_version":1, "scope":scope, "guardian":guardian}),
    )?;
    artifacts::seal(&attempt, "process.json", &checkpoint, RECORD_LIMIT)?;
    // Recovery preserves existing captures, results, scratch and provider history.
    let telemetry = [
        ("stdout.log", b"partial output".as_slice()),
        ("stderr.log", b"partial diagnostics".as_slice()),
        ("result.md", b"partial result".as_slice()),
        ("process.json", checkpoint.as_slice()),
    ];
    for (name, bytes) in telemetry {
        if name != "process.json" {
            artifacts::seal(&attempt, name, bytes, CAPTURE_LIMIT)?;
        }
    }
    let temporary = store.temporary_path(id, &call)?;
    std::fs::write(temporary.join("keep"), "scratch")?;
    artifacts::subdir(&attempt, std::ffi::OsStr::new("provider-state"), true)?;
    let history = context
        .state_root
        .as_path()
        .join(format!("runs/{}/attempts/{id}/provider-state", run.run_id));
    std::fs::write(history.join("session"), "history")?;

    if removed {
        std::fs::remove_dir_all(&worktree)?;
    }

    if boot == OLD_BOOT {
        // Both exclusions are free. That alone cannot prove a reboot or exit.
        drop(attempt.try_lock(
            std::ffi::OsStr::new("custody.lock"),
            provenant_os::fs::LockMode::Exclusive,
        )?);
        drop(Directory::open_anchor(&custody)?.try_lock(
            std::ffi::OsStr::new("writer.lock"),
            provenant_os::fs::LockMode::Exclusive,
        )?);
        assert!(
            matches!(store.reconcile_with_observer(run.run_id, &Fake::default(), &call, &observer)?.as_slice(),
            [(attempt, Reconciled::Uncertain { .. })] if *attempt == id)
        );
        assert_eq!(store.record(id, &call)?, before);
        assert!(reservation.exists());
        assert_eq!(std::fs::read_to_string(temporary.join("keep"))?, "scratch");
        return Ok(());
    }
    if interrupted {
        let recovery = provenant_supervisor::custody::acquire_recovery(
            Directory::open_anchor(&custody)?,
            call.budget,
            &cancel,
            &clock,
        )?;
        let proof =
            provenant_supervisor::fixture::after_reboot_with(recovery, &attempt, scope, &observer)?
                .ok_or("reboot not proven")?;
        store.commit_reboot_lost(&proof, &call)?;
        drop(proof);
        assert!(reservation.exists());
        assert_eq!(terminal(&store, id, &call)?, Outcome::Lost);
    }
    if cancel_run && !interrupted {
        let custody_lock = attempt.try_lock(
            std::ffi::OsStr::new("custody.lock"),
            provenant_os::fs::LockMode::Exclusive,
        )?;
        assert_eq!(
            store.cancel_with_observer(id, "stop", &call, &observer)?,
            CancelReceipt::Requested
        );
        assert_eq!(store.record(id, &call)?, before);
        assert!(reservation.exists());
        drop(custody_lock);
    }
    if cancel_run {
        assert_eq!(
            store.cancel_with_observer(id, "stop", &call, &observer)?,
            CancelReceipt::AlreadyTerminal
        );
    } else {
        assert_eq!(
            store.reconcile_with_observer(run.run_id, &Fake::default(), &call, &observer)?,
            vec![(id, Reconciled::Finished)]
        );
    }
    assert_eq!(terminal(&store, id, &call)?, Outcome::Lost);
    if removed {
        assert!(
            matches!(store.record(id, &call)?.state, AttemptState::Terminal { reason: Some(reason), .. } if reason.contains("worktree"))
        );
    }
    let fact = store.attempt(id, &call)?;
    assert_eq!(fact.terminal_state, Outcome::Lost.into());
    assert!(fact.output_refs.is_empty());
    let after = store.record(id, &call)?;
    assert_eq!(after.started_at, before.started_at);
    assert_eq!(after.applied_route, before.applied_route);
    assert_eq!(after.usage, before.usage);
    assert_eq!(after.session, before.session);
    assert_eq!(store.run(run.run_id, &call)?.closure, Closure::Closed);
    assert!(store.writer_fences()?.is_empty());
    assert!(!reservation.exists());
    assert!(
        store
            .reconcile_with_observer(run.run_id, &Fake::default(), &call, &observer)?
            .is_empty()
    );
    // Also retry a crash after fence release but before clearing the marker.
    store.set_temporary_retained(id, true, &call)?;
    assert!(
        store
            .reconcile_with_observer(run.run_id, &Fake::default(), &call, &observer)?
            .is_empty()
    );
    for (name, bytes) in telemetry {
        assert_eq!(
            attempt.read_bounded(std::ffi::OsStr::new(name), CAPTURE_LIMIT)?,
            bytes
        );
    }
    assert_eq!(std::fs::read_to_string(temporary.join("keep"))?, "scratch");
    assert_eq!(std::fs::read_to_string(history.join("session"))?, "history");
    if !removed {
        assert_eq!(
            std::fs::read_to_string(worktree.join("unfinished.rs"))?,
            "keep my work"
        );
    }
    assert!(artifacts::read_sealed(&attempt, "process-exit.json", RECORD_LIMIT)?.is_none());
    assert!(store.open_runs(10, &call)?.is_empty());
    Ok(())
}

#[test]
fn reconcile_finishes_a_durable_exit_and_withdraws_a_launch_that_never_spawned() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let mut store = RunStore::open(&context(temp.path())?, &call)?;
    store.set_admission_policy(AdmissionPolicy {
        startup_spacing_ms: 0,
        read_only_spacing_ms: 0,
        ..AdmissionPolicy::default()
    });
    let batch = two_tasks(2);
    store.create(&batch, &call)?;
    let [exited, orphan] = [batch.attempts[0].attempt_id, batch.attempts[1].attempt_id];

    // The guardian sealed an exit but died before its terminal commit.
    store.mark_starting(
        &store.record(exited, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store
        .claim(exited, &at_micros(100), &call)?
        .ok_or("not admitted")?;
    let dir = store.attempt_dir(batch.run_id, exited, false)?;
    artifacts::seal(&dir, "stdout.log", b"answer", CAPTURE_LIMIT)?;
    let mut observed = exit();
    observed.scope.attempt = exited;
    observed.scope.generation = 1;
    let summary = serde_json::json!({"schema_version": 1, "scope": observed.scope, "requester": provenant_core::ActorId::from_bytes([2; 16])});
    artifacts::seal(
        &dir,
        "launch.json",
        &serde_json::to_vec(&summary)?,
        RECORD_LIMIT,
    )?;
    seal_exit(&store, &observed)?;

    // A handoff whose reservation never reached a guardian.
    store.mark_starting(
        &store.record(orphan, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let scope = provenant_supervisor::custody::Scope {
        run: batch.run_id,
        attempt: orphan,
        generation: 1,
    };
    drop(provenant_supervisor::fixture::acquire_writer(
        store.attempt_dir(batch.run_id, orphan, false)?,
        scope,
        call.budget,
        &cancel,
        &clock,
    )?);

    let fake = Fake {
        observed: Some(route()),
        ..Fake::default()
    };
    let found = store.reconcile(batch.run_id, &fake, &call)?;
    assert_eq!(
        found,
        vec![
            (exited, Reconciled::Finished),
            (orphan, Reconciled::Withdrawn)
        ]
    );
    assert_eq!(terminal(&store, exited, &call)?, Outcome::Succeeded);
    assert!(artifacts::read_sealed(&dir, "release.json", RECORD_LIMIT)?.is_some());
    assert_eq!(terminal(&store, orphan, &call)?, Outcome::Rejected);
    assert!(store.reconcile(batch.run_id, &fake, &call)?.is_empty());
    Ok(())
}
#[test]
fn reconcile_settles_a_writer_attempt_whose_worktree_was_removed() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let mut run = input();
    run.request.tasks[0].writer = Some(WriterRequest {
        project: temp.path().join("checkout"),
        worktree: temp.path().join("removed-lane"),
    });
    store.create(&run, &call)?;
    let id = run.attempts[0].attempt_id;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store
        .claim(id, &at_micros(100), &call)?
        .ok_or("not admitted")?;
    let attempt = store.attempt_dir(run.run_id, id, false)?;
    let scope = provenant_supervisor::custody::Scope {
        run: run.run_id,
        attempt: id,
        generation: 1,
    };
    // The guardian reserved the writer and sealed an exit; its worktree is gone.
    let custody = context
        .state_root
        .as_path()
        .join("worktrees")
        .join(context.project_id.to_string())
        .join("lane");
    std::fs::create_dir_all(&custody)?;
    drop(provenant_supervisor::fixture::acquire_writer(
        Directory::open_anchor(&custody)?,
        scope,
        call.budget,
        &cancel,
        &clock,
    )?);
    let meta = attempt.metadata()?;
    let reservation = custody.join("reservation.json");
    let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&reservation)?)?;
    let fields = value.as_object_mut().ok_or("reservation")?;
    fields.insert("launch_state".into(), "never_spawned".into());
    fields.insert("unresolved_cleanup".into(), false.into());
    fields.insert("census_incomplete".into(), false.into());
    fields.insert("attempt_device".into(), meta.device.into());
    fields.insert("attempt_inode".into(), meta.inode.into());
    std::fs::write(&reservation, serde_json::to_vec(&value)?)?;
    artifacts::seal(&attempt, "stdout.log", b"answer", CAPTURE_LIMIT)?;
    let mut observed = exit();
    observed.scope = scope;
    let summary = serde_json::json!({"schema_version": 1, "scope": scope, "requester": provenant_core::ActorId::from_bytes([2; 16])});
    artifacts::seal(
        &attempt,
        "launch.json",
        &serde_json::to_vec(&summary)?,
        RECORD_LIMIT,
    )?;
    seal_exit(&store, &observed)?;

    let fake = Fake {
        observed: Some(route()),
        ..Fake::default()
    };
    let found = store.reconcile(run.run_id, &fake, &call)?;
    assert_eq!(found, vec![(id, Reconciled::Finished)]);
    assert_eq!(terminal(&store, id, &call)?, Outcome::Lost);
    assert!(store.writer_fences()?.is_empty());
    assert!(store.reconcile(run.run_id, &fake, &call)?.is_empty());
    Ok(())
}
#[test]
fn reconcile_withdraws_a_start_that_died_before_reserving() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    let id = run.attempts[0].attempt_id;
    store.create(&run, &call)?;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let found = store.reconcile(run.run_id, &Fake::default(), &call)?;
    assert_eq!(found, vec![(id, Reconciled::Withdrawn)]);
    assert_eq!(terminal(&store, id, &call)?, Outcome::Rejected);
    Ok(())
}

#[test]
fn read_only_exceptions_are_recorded_and_temporary_paths_are_private_per_attempt() -> TestResult {
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir()?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let prepared = two_tasks(2);
    store.create(&prepared, &call)?;
    let first = prepared.attempts[0].attempt_id;
    let second = prepared.attempts[1].attempt_id;
    let first_tmp = store.temporary_path(first, &call)?;
    let second_tmp = store.temporary_path(second, &call)?;
    assert_ne!(first_tmp, second_tmp);
    assert!(
        first_tmp.starts_with(
            context
                .state_root
                .as_path()
                .join("runs")
                .join(prepared.run_id.to_string())
                .join("attempts")
                .join(first.to_string())
        )
    );
    assert_eq!(
        std::fs::metadata(&first_tmp)?.permissions().mode() & 0o777,
        0o700
    );
    let observed = provenant_core::GitOid::Sha1([7; 20]);
    let write_grants = vec![
        WriteGrantRecord {
            source: WriteSource::WriteRoot,
            path: provenant_records::AbsolutePath::new(
                &context.state_root.as_path().join("shared"),
            )?,
            scope: PathScope::Subtree,
        },
        WriteGrantRecord {
            source: WriteSource::StateWrite,
            path: provenant_records::AbsolutePath::new(
                &context
                    .state_root
                    .as_path()
                    .join(std::ffi::OsString::from_vec(b"session-\xff".to_vec())),
            )?,
            scope: PathScope::Exact,
        },
    ];
    store.mark_starting(
        &store.record(first, &call)?,
        &route(),
        &[".agent-run/locks".into()],
        &admission::AppliedWriteGrants {
            grants: write_grants.clone(),
            git_grants_digest: Some(provenant_core::Digest::from_bytes([7; 32])),
        },
        Some(observed),
        &call,
    )?;
    drop(store);
    let store = RunStore::open(&context, &call)?;
    assert_eq!(
        store.record(first, &call)?.read_only_writable,
        [".agent-run/locks"]
    );
    assert!(store.record(second, &call)?.read_only_writable.is_empty());
    assert_eq!(store.record(first, &call)?.write_grants, write_grants);
    assert!(store.record(second, &call)?.write_grants.is_empty());
    // The host-observed head of a read-only lane persists; unobserved stays absent.
    assert_eq!(store.record(first, &call)?.head, Some(observed));
    assert_eq!(store.record(second, &call)?.head, None);
    // Old v1 records omit the added fields and retain empty defaults.
    let mut record = serde_json::to_value(store.record(first, &call)?)?;
    let fields = record.as_object_mut().ok_or("record object")?;
    fields.remove("read_only_writable");
    fields.remove("write_grants");
    fields.remove("git_grants_digest");
    let old = serde_json::from_value::<AttemptRecord>(record)?;
    assert!(old.read_only_writable.is_empty());
    assert!(old.write_grants.is_empty());
    assert_eq!(old.git_grants_digest, None);
    // Development records containing the retired tracking grant remain readable,
    // but the field is never written back or used as authority.
    let mut legacy = serde_json::to_value(&old)?;
    legacy.as_object_mut().ok_or("record object")?.insert(
        "granted_tracking_ref".into(),
        "refs/remotes/origin/feature/topic".into(),
    );
    let legacy = serde_json::from_value::<AttemptRecord>(legacy)?;
    assert!(
        serde_json::to_value(legacy)?
            .get("granted_tracking_ref")
            .is_none()
    );
    Ok(())
}

#[test]
fn write_grants_are_bounded_at_admission_and_read_and_preserve_git_digest() -> TestResult {
    use std::os::unix::ffi::OsStringExt;

    let temp = tempfile::tempdir()?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let prepared = input();
    store.create(&prepared, &call)?;
    let id = prepared.attempts[0].attempt_id;
    let grant = WriteGrantRecord {
        source: WriteSource::LaneCache,
        path: provenant_records::AbsolutePath::new(std::path::Path::new("/fixture/cache"))?,
        scope: PathScope::Subtree,
    };
    let mut applied = admission::AppliedWriteGrants {
        grants: vec![grant.clone(); provenant_confinement::MAX_WRITE_GRANTS + 1],
        git_grants_digest: Some(provenant_core::Digest::from_bytes([7; 32])),
    };
    let mut record = store.record(id, &call)?;
    assert!(matches!(
        store.mark_starting(&record, &route(), &[], &applied, None, &call),
        Err(RunError::Validation("invalid write grants"))
    ));
    record.write_grants = applied.grants.clone();
    assert!(matches!(
        reader::validate_attempt(&record),
        Err(RunError::Validation("invalid write grants"))
    ));
    applied.grants.pop();
    assert!(reader::valid_write_grants(&applied.grants));
    assert!(reader::write_grants_fit(&applied.grants));
    // Native bytes double in the wire encoding, exceeding the cap even
    // though the raw path is smaller. Old records still decode by count.
    let mut path = b"/fixture/".to_vec();
    path.extend(vec![b'x'; 128 * 1024]);
    path.push(0xff);
    applied.grants = vec![WriteGrantRecord {
        path: provenant_records::AbsolutePath::new(std::path::Path::new(
            &std::ffi::OsString::from_vec(path),
        ))?,
        ..grant.clone()
    }];
    assert!(!reader::write_grants_fit(&applied.grants));
    assert!(matches!(
        store.mark_starting(&record, &route(), &[], &applied, None, &call),
        Err(RunError::Validation("invalid write grants"))
    ));
    record.write_grants = applied.grants.clone();
    let legacy: AttemptRecord = serde_json::from_slice(&serde_json::to_vec(&record)?)?;
    reader::validate_attempt(&legacy)?;
    applied.grants = vec![grant];
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &applied,
        None,
        &call,
    )?;
    drop(store);
    let store = RunStore::open(&context, &call)?;
    assert_eq!(
        store.record(id, &call)?.git_grants_digest,
        applied.git_grants_digest
    );
    // AbsolutePath retains PathWire's contained byte encoding. It cannot
    // deserialise a relative escape into an absolute grant.
    let mut invalid = serde_json::to_value(store.record(id, &call)?)?;
    invalid["write_grants"][0]["path"]["relative_to_root"]["path"] = serde_json::json!("../escape");
    assert!(serde_json::from_value::<AttemptRecord>(invalid).is_err());
    Ok(())
}

#[test]
fn lane_waits_report_only_the_running_lanes_own_disk_pause() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let id = starting(&store, &lane(120, true), &call)?;
    let running = store.claim(id, &at(0), &call)?.ok_or("lane starts")?;
    let other_id = starting(&store, &lane(121, true), &call)?;
    let other = store
        .claim(other_id, &at(20_000), &call)?
        .ok_or("other starts")?;
    let marker = store
        .context
        .state_root
        .as_path()
        .join("runs")
        .join(running.run_id.to_string())
        .join("attempts")
        .join(id.to_string())
        .join("process.json");
    let guardian = match provenant_supervisor::census::SystemProcessObserver
        .observe(i64::from(std::process::id()))
    {
        provenant_supervisor::census::ProcessObservation::Present(fact) => fact.identity,
        _ => return Err("test guardian identity unavailable".into()),
    };
    std::fs::write(
        &marker,
        serde_json::to_vec(&serde_json::json!({
            "disk_paused": true, "guardian": guardian,
        }))?,
    )?;
    let records = [running, other];
    let waits = store.lane_waits(&records, &call);
    assert_eq!(waits.len(), 1);
    assert_eq!(waits[0].attempt_id, id);
    assert_eq!(
        waits[0].waiting_for,
        "disk pressure paused builds below gc.free_floor; run `provenant gc --apply`"
    );
    let exit_path = marker.with_file_name("process-exit.json");
    std::fs::write(&exit_path, br#"{"unresolved_cleanup":true}"#)?;
    assert!(
        store.lane_waits(&records, &call).is_empty(),
        "an exited provider cannot remain disk-paused while its guardian settles cleanup"
    );
    std::fs::remove_file(exit_path)?;
    let mut dead_guardian = guardian;
    dead_guardian.native_start = dead_guardian.native_start.saturating_add(1);
    std::fs::write(
        &marker,
        serde_json::to_vec(&serde_json::json!({
            "disk_paused": true, "guardian": dead_guardian,
        }))?,
    )?;
    assert!(
        store.lane_waits(&records, &call).is_empty(),
        "a dead guardian's checkpoint must not report a current disk pause"
    );
    Ok(())
}

#[test]
fn disk_pause_wait_ignores_false_missing_symlinked_and_oversized_observations() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let id = starting(&store, &lane(120, true), &call)?;
    let running = store.claim(id, &at(0), &call)?.ok_or("lane starts")?;
    let records = [running];
    let marker = store
        .context
        .state_root
        .as_path()
        .join("runs")
        .join(records[0].run_id.to_string())
        .join("attempts")
        .join(id.to_string())
        .join("process.json");
    assert!(store.lane_waits(&records, &call).is_empty());
    std::fs::write(&marker, br#"{"disk_paused":false}"#)?;
    assert!(store.lane_waits(&records, &call).is_empty());
    std::fs::remove_file(&marker)?;
    let elsewhere = temp.path().join("elsewhere.json");
    let body = r#"{"disk_paused":true}"#;
    std::fs::write(&elsewhere, body)?;
    std::os::unix::fs::symlink(&elsewhere, &marker)?;
    assert!(store.lane_waits(&records, &call).is_empty());
    std::fs::remove_file(&marker)?;
    let padding = " ".repeat(usize::try_from(RECORD_LIMIT)? + 1);
    std::fs::write(&marker, format!("{body}{padding}"))?;
    assert!(store.lane_waits(&records, &call).is_empty());
    Ok(())
}

#[test]
fn admission_uses_configured_gc_floor_and_reports_the_fix() -> TestResult {
    struct Disk(u64);
    impl Resources for Disk {
        fn memory(&self) -> std::io::Result<MemorySample> {
            Ok(MemorySample {
                total_bytes: 100,
                available_bytes: 100,
            })
        }
        fn disk(&self, _: &Directory) -> std::io::Result<u64> {
            Ok(self.0)
        }
    }
    for free in [99, 100] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let context = context(temp.path())?;
        let registry = temp.path().canonicalize()?.join("registry");
        std::fs::DirBuilder::new().mode(0o700).create(&registry)?;
        std::fs::write(registry.join("config.toml"), "[gc]\nfree_floor='100B'\n")?;
        let mut store = RunStore::open(&context, &call)?.with_writer_registry(&registry);
        std::fs::write(
            context.state_root.as_path().join("config.toml"),
            "schema_version=1\n[gc]\nfree_floor='1B'\n",
        )?;
        store.set_admission_policy(AdmissionPolicy {
            admission_seconds: 1,
            ..Default::default()
        });
        let run = lane(98, true);
        let id = starting(&store, &run, &call)?;
        let attempt = store.attempt_dir(run.run_id, id, false)?;
        let result = store.admit(id, &attempt, &Disk(free), &clock, &cancel);
        if free == 100 {
            assert_eq!(result?.state, AttemptState::Running);
        } else {
            let error = result.err().ok_or("admitted below floor")?;
            assert!(matches!(
                error,
                RunError::DiskFloor {
                    free: 99,
                    floor: 100
                }
            ));
            let message = error.to_string();
            assert!(message.contains("99 bytes available"), "{message}");
            assert!(message.contains("gc.free_floor 100 bytes"), "{message}");
            assert!(message.contains("provenant gc --apply"), "{message}");
        }
    }
    Ok(())
}

#[test]
fn admission_missing_configuration_uses_the_default_host_floor() -> TestResult {
    struct Disk(u64);
    impl Resources for Disk {
        fn memory(&self) -> std::io::Result<MemorySample> {
            Host { available: 100 }.memory()
        }
        fn disk(&self, _: &Directory) -> std::io::Result<u64> {
            Ok(self.0)
        }
    }
    for below in [true, false] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let context = context(temp.path())?;
        let registry = temp.path().canonicalize()?.join("registry");
        std::fs::DirBuilder::new().mode(0o700).create(&registry)?;
        let config = registry.join("config.toml");
        std::fs::write(&config, "[gc]\nfree_floor='1B'\n")?;
        let mut store = RunStore::open(&context, &call)?.with_writer_registry(&registry);
        store.set_admission_policy(AdmissionPolicy {
            admission_seconds: 1,
            ..Default::default()
        });
        let run = lane(101, true);
        let id = starting(&store, &run, &call)?;
        let attempt = store.attempt_dir(run.run_id, id, false)?;
        std::fs::remove_file(config)?;
        // A nested store's config must not replace the missing host config.
        std::fs::write(context.state_root.as_path().join("config.toml"), "[gc")?;
        let floor = provenant_config::GcSettings::default().free_floor_bytes(&registry)?;
        let free = floor - u64::from(below);
        let result = store.admit(id, &attempt, &Disk(free), &clock, &cancel);
        if below {
            assert!(
                matches!(result, Err(RunError::DiskFloor { free: f, floor: p }) if f == free && p == floor)
            );
        } else {
            assert_eq!(result?.state, AttemptState::Running);
        }
    }
    Ok(())
}

#[test]
fn admission_rereads_gc_floor_after_submission_and_while_waiting() -> TestResult {
    struct Disk<'a> {
        config: &'a std::path::Path,
        samples: Cell<u64>,
    }
    impl Resources for Disk<'_> {
        fn memory(&self) -> std::io::Result<MemorySample> {
            Ok(MemorySample {
                total_bytes: 100,
                available_bytes: 100,
            })
        }
        fn disk(&self, _: &Directory) -> std::io::Result<u64> {
            self.samples.set(self.samples.get() + 1);
            std::fs::write(self.config, "[gc]\nfree_floor='100B'\n")?;
            Ok(100)
        }
    }
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let mut store = RunStore::open(&context, &call)?;
    let config = context.state_root.as_path().join("config.toml");
    std::fs::write(&config, "[gc]\nfree_floor='1B'\n")?;
    store.set_admission_policy(AdmissionPolicy {
        admission_seconds: 1,
        ..Default::default()
    });
    let run = lane(99, true);
    let id = starting(&store, &run, &call)?;
    let attempt = store.attempt_dir(run.run_id, id, false)?;
    std::fs::write(&config, "[gc]\nfree_floor='101B'\n")?;
    let disk = Disk {
        config: &config,
        samples: Cell::new(0),
    };
    assert_eq!(
        store.admit(id, &attempt, &disk, &clock, &cancel)?.state,
        AttemptState::Running
    );
    assert_eq!(
        disk.samples.get(),
        2,
        "the changed floor must hold admission for one observation"
    );
    Ok(())
}

#[test]
fn admission_waits_for_a_readable_current_gc_floor() -> TestResult {
    struct Disk<'a> {
        config: &'a std::path::Path,
        samples: Cell<u64>,
    }
    impl Resources for Disk<'_> {
        fn memory(&self) -> std::io::Result<MemorySample> {
            Ok(MemorySample {
                total_bytes: 100,
                available_bytes: 100,
            })
        }
        fn disk(&self, _: &Directory) -> std::io::Result<u64> {
            self.samples.set(self.samples.get() + 1);
            std::fs::write(self.config, "[gc]\nfree_floor='100B'\n")?;
            Ok(100)
        }
    }
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let mut store = RunStore::open(&context, &call)?;
    let config = context.state_root.as_path().join("config.toml");
    std::fs::write(&config, "[gc]\nfree_floor='1B'\n")?;
    store.set_admission_policy(AdmissionPolicy {
        admission_seconds: 1,
        ..Default::default()
    });
    let run = lane(100, true);
    let id = starting(&store, &run, &call)?;
    let attempt = store.attempt_dir(run.run_id, id, false)?;
    std::fs::write(&config, "[gc]\nfree_floor='")?;
    let disk = Disk {
        config: &config,
        samples: Cell::new(0),
    };
    assert_eq!(
        store.admit(id, &attempt, &disk, &clock, &cancel)?.state,
        AttemptState::Running
    );
    assert_eq!(
        disk.samples.get(),
        2,
        "an unreadable current floor must queue until the next valid observation"
    );
    Ok(())
}
