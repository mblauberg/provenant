//! Dispatch, admission, holder and control behaviour at Run's owner boundary.
//! Fake providers and resources stand in for adapters and the host.
use super::tests::{call, clock, context, exit, input, route};
use super::*;
use provenant_confinement::permissions::{PathGrant, PathScope};
use provenant_core::{AttemptId, CancelToken, RunId, TaskId, WallTime};
use provenant_records::CanonicalV1;
use provenant_supervisor::{
    custody::ExitObservation, exec::Termination, fs::Directory, sys::resources::MemorySample,
};
use std::{
    cell::{Cell, RefCell},
    os::unix::fs::DirBuilderExt,
};
type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

fn hold_child_fixture(
    store: &RunStore,
    directory: Directory,
    scope: provenant_supervisor::custody::Scope,
    requester: provenant_core::ActorId,
) -> TestResult {
    let custody = directory.try_lock(
        std::ffi::OsStr::new("custody.lock"),
        provenant_supervisor::fs::LockMode::Exclusive,
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
            false,
            "network under a read-only parent",
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
    store.queue_ticket(&store.db, &replacement, 1, 0)?;
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
    let mut swapped = validated.clone();
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
        provenant_supervisor::fs::PublishMode::Replace,
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
fn host_launches_only_the_validated_child_request() -> TestResult {
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

    // The lane swaps its sealed request and the recorded digest together.
    let mut swapped = validated.clone();
    swapped.tasks[0].access.network = Some(NetworkAccess::Open);
    store.run_dir(record.run_id, false)?.atomic_write(
        std::ffi::OsStr::new("request.json"),
        ".swap-",
        &crate::db::bytes(&swapped)?,
        provenant_supervisor::fs::PublishMode::Replace,
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
        &swapped,
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
    Ok(())
}

/// Prepares a plan; no guardian ever runs it.
struct Ready;
impl Providers for Ready {
    fn prepare(
        &self,
        _: &TaskRequest,
        _: &str,
        _: &LaunchPaths,
        _: Option<&AttemptRecord>,
    ) -> std::result::Result<PreparedLaunch, String> {
        Ok(PreparedLaunch {
            argv: vec!["fixture".into()],
            environment: Vec::new(),
            stdin: Vec::new(),
            secret_input: None,
            cwd: "/".into(),
            route: route(),
            skipped_routes: Vec::new(),
            observed_head: None,
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

#[test]
fn oversized_write_grants_reject_without_stalling_launch() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let prepared = input();
    store.create(&prepared, &call)?;
    let id = prepared.attempts[0].attempt_id;
    let provider = Confined {
        writer: false,
        tool_writes: (0..128)
            .map(|i| PathGrant {
                path: format!("/fixture/tools/{i}/{}", "x".repeat(3000)).into(),
                scope: PathScope::Subtree,
            })
            .collect(),
    };
    assert!(matches!(
        store.start(prepared.run_id, &provider, &|_, _| unreachable!("oversized grants must be refused before handoff"), &call)?.as_slice(),
        [Started::Refused { attempt, kind: RefusalKind::Preparation, .. }] if *attempt == id
    ));
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
        &run.request,
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
            &run.request,
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
    use provenant_supervisor::custody::{PublicationReservation, Scope};
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
            let replacement =
                directory.try_lock(lock, provenant_supervisor::fs::LockMode::Exclusive)?;
            assert!(!identity.same_identity(&replacement.metadata()?));
        } else if state == "symlink" {
            directory.try_lock(
                OsStr::new("other.lock"),
                provenant_supervisor::fs::LockMode::Exclusive,
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
        provenant_supervisor::fs::LockMode::Exclusive,
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
        &run.request,
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
        provenant_supervisor::fs::LockMode::Exclusive,
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
        provenant_supervisor::fs::LockMode::Exclusive,
    )?;
    let fake = Fake::default();
    let started = store.start_child(
        run.run_id,
        &run.request,
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
struct Fake {
    refuse: bool,
    observed: Option<RouteRecord>,
    decoded: Cell<usize>,
    prepared: Cell<usize>,
    resumed: Cell<Option<AttemptId>>,
    prompt: RefCell<String>,
}
impl Providers for Fake {
    fn prepare(
        &self,
        _: &TaskRequest,
        prompt: &str,
        paths: &LaunchPaths,
        resume: Option<&AttemptRecord>,
    ) -> std::result::Result<PreparedLaunch, String> {
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
    let now = WallTime::from_micros(100)?;

    store.set_admission_policy(AdmissionPolicy {
        startup_spacing_ms: 0,
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
        .claim(first, now, &call)?
        .ok_or("first not admitted")?;
    assert_eq!(running.state, AttemptState::Running);
    assert_eq!(running.started_at, Some(now));
    assert!(
        store.claim(second, now, &call)?.is_none(),
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
    assert!(store.claim(id, now, &call)?.is_none(), "20 s host spacing");

    let attempt = store.attempt_dir(spaced.run_id, id, false)?;
    store.set_admission_policy(AdmissionPolicy {
        admission_seconds: 0,
        ..AdmissionPolicy::default()
    });
    let mut expired = input();
    expired.run_id = provenant_core::RunId::from_bytes([13; 16]);
    expired.operation_id = provenant_core::OperationId::parse("expired")?;
    expired.attempts[0].attempt_id = AttemptId::from_bytes([14; 16]);
    store.create(&expired, &call)?;
    let id = expired.attempts[0].attempt_id;
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
    Ok(())
}

#[test]
fn child_stores_share_the_host_ledger_and_recovery_frees_a_lease() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let now = WallTime::from_micros(100)?;
    let base = std::fs::canonicalize(temp.path())?;
    let host = base.join("host");
    let mut stores = Vec::new();
    for name in ["first", "second"] {
        std::fs::create_dir(base.join(name))?;
        let mut store =
            RunStore::open(&context(&base.join(name))?, &call)?.with_writer_registry(&host);
        store.set_admission_policy(AdmissionPolicy {
            startup_spacing_ms: 0,
            ..AdmissionPolicy::default()
        });
        stores.push(store);
    }
    // Each store fills half the project's host-wide limit with its own run.
    let half = PROJECT_ACTIVE / 2;
    let batches = [batch(1, half), batch(2, half)];
    for (store, run) in stores.iter().zip(&batches) {
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
                .claim(attempt.attempt_id, now, &call)?
                .ok_or("within the host limit")?;
        }
    }
    // Counted per store, the second store would admit a ninth attempt.
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
        stores[1].claim(waiting, now, &call)?.is_none(),
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
        .claim(waiting, now, &call)?
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

/// Stages a host-owned input the way provider preparation does, over a file
/// the host cannot vouch for.
struct Unreadable;
impl Providers for Unreadable {
    fn prepare(
        &self,
        _: &TaskRequest,
        _: &str,
        paths: &LaunchPaths,
        _: Option<&AttemptRecord>,
    ) -> std::result::Result<PreparedLaunch, String> {
        use std::os::unix::fs::PermissionsExt;
        let file = paths.scratch.join("git-log.txt");
        std::fs::write(&file, b"context").map_err(|error| error.to_string())?;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644))
            .map_err(|error| error.to_string())?;
        let scratch = Directory::open_anchor(&paths.scratch).map_err(|error| error.to_string())?;
        artifacts::seal(&scratch, "git-log.txt", b"context", RECORD_LIMIT)
            .map_err(|error| error.to_string())?;
        unreachable!("an unreadable input must stop preparation")
    }
    fn decode(
        &self,
        _: &RouteRecord,
        _: &[u8],
        _: &ExitObservation,
        _: &std::path::Path,
    ) -> std::result::Result<Decoded, String> {
        unreachable!("a refused attempt never runs")
    }
}

#[test]
fn unreadable_staged_input_ends_the_attempt_rejected_not_succeeded() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    store.create(&run, &call)?;
    let started = store.start(run.run_id, &Unreadable, &|_, _| unreachable!(), &call)?;
    assert!(matches!(
        &started[..],
        [Started::Refused { reason, kind: RefusalKind::Preparation, .. }]
            if reason.contains("not private")
    ));
    let id = run.attempts[0].attempt_id;
    assert_eq!(terminal(&store, id, &call)?, Outcome::Rejected);
    assert!(store.attempt(id, &call)?.started_at.is_none());
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
        ..AdmissionPolicy::default()
    });
    let batch = two_tasks(2);
    store.create(&batch, &call)?;
    let now = WallTime::from_micros(100)?;
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
        store.claim(id, now, &call)?.ok_or("not admitted")?;
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
    let system = provenant_supervisor::clock::SystemClock::new();
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
fn resume_continues_a_terminal_attempt_once_with_its_route() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let mut run = input();
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
    let next = AttemptId::from_bytes([21; 16]);
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
    let system = provenant_supervisor::clock::SystemClock::new();
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
    use provenant_supervisor::fs::{FsError, LockMode};
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
        .collect::<provenant_supervisor::fs::Result<Vec<_>>>()?;

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
        provenant_supervisor::fs::LockMode::Exclusive,
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
                if *attempt == id && reason.contains("chair recovery required"))
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
        [(attempt, Reconciled::Uncertain { reason })]
        if *attempt == id && reason.contains("writer reservation retained"))
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
fn reconcile_finishes_a_durable_exit_and_withdraws_a_launch_that_never_spawned() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let mut store = RunStore::open(&context(temp.path())?, &call)?;
    store.set_admission_policy(AdmissionPolicy {
        startup_spacing_ms: 0,
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
        .claim(exited, WallTime::from_micros(100)?, &call)?
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
        .claim(id, WallTime::from_micros(100)?, &call)?
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
            git_grants_digest: None,
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
    assert_eq!(store.record(first, &call)?.git_grants_digest, None);
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
