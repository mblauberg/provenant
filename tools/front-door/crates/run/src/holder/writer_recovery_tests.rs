use super::*;
use crate::tests::{call, clock, context, input, route};

/// The settled snapshot's stored shape: a handoff result, not the bare
/// provider declaration.
fn seal_progress_handoff(
    attempt: &provenant_os::fs::Directory,
    progress: &serde_json::Value,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let handoff = provenant_records::handoff::Handoff::from_json(&serde_json::to_vec(progress)?)?;
    let stored: provenant_records::handoff::HandoffResult = Ok(Some(handoff));
    crate::artifacts::seal(
        attempt,
        provenant_records::AttemptFile::Handoff.name(),
        &serde_json::to_vec(&stored)?,
        provenant_records::handoff::HANDOFF_BYTES as u64 * 4,
    )?;
    Ok(())
}

#[test]
fn modern_writer_reconcile_preserves_unlaunched_private_evidence()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    use crate::delivery_tests::{PrepareFixture, registered_landed_writer_fixture};
    use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
    use std::ffi::OsString;
    for mode in [
        "no_summary_terminal_modern",
        "no_summary_terminal_other_generation",
    ] {
        let fixture = PrepareFixture::new()?;
        let execution = registered_landed_writer_fixture(&fixture)?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&execution, &call)?;
        let mut run = input();
        run.request.task.writer = Some(fixture.writer.clone());
        store.create(&run, &call)?;
        let id = run.attempt_id;
        let record = store.mark_starting(
            &store.record(id, &call)?,
            &route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let scope = RunStore::scope(&record);
        let attempt = store.attempt_dir(id, false)?;
        let mut launch = crate::prepare_writer(
            &fixture.writer,
            execution.state_root.as_path(),
            execution.project_id,
        )?;
        launch.snapshot(
            execution.state_root.as_path(),
            &attempt,
            scope,
            &provenant_records::policy::Policy::default(),
        )?;
        let candidate = store.temporary_path(id, &call)?.join("candidate-0");
        std::fs::create_dir_all(&candidate)?;
        let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
        crate::objects::prepare_bound(
            &git,
            launch.worktree.repo(),
            &attempt,
            &candidate,
            launch.branch.as_str(),
            launch.base,
        )?;
        // A commit that initially exists only in the writer's private Git.
        let private_git =
            |args: &[&str]| -> std::result::Result<String, Box<dyn std::error::Error>> {
                let argv: Vec<OsString> = std::iter::once(git.as_os_str().to_owned())
                    .chain(args.iter().map(|arg| OsString::from(*arg)))
                    .collect();
                let mut environment: Vec<(OsString, OsString)> = [
                    ("PATH", "/usr/bin:/bin"),
                    ("GIT_CONFIG_NOSYSTEM", "1"),
                    ("GIT_CONFIG_GLOBAL", "/dev/null"),
                    ("GIT_AUTHOR_NAME", "Fixture"),
                    ("GIT_COMMITTER_NAME", "Fixture"),
                    ("GIT_AUTHOR_EMAIL", "fixture@example.invalid"),
                    ("GIT_COMMITTER_EMAIL", "fixture@example.invalid"),
                ]
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect();
                environment.extend([
                    (
                        "GIT_DIR".into(),
                        candidate.join("git-common").into_os_string(),
                    ),
                    (
                        "GIT_COMMON_DIR".into(),
                        candidate.join("git-common").into_os_string(),
                    ),
                    (
                        "GIT_OBJECT_DIRECTORY".into(),
                        candidate.join("git-objects").into_os_string(),
                    ),
                ]);
                let mut options = ExecOptions::new(
                    &argv,
                    &fixture.writer.worktree,
                    std::time::Duration::from_secs(5),
                );
                options.env = Some(&environment);
                let output = SystemExecutor::new(1)?.execute(options)?;
                assert!(
                    output.termination == Termination::Exit(0),
                    "{}",
                    String::from_utf8_lossy(&output.stderr.bytes)
                );
                Ok(String::from_utf8(output.stdout.bytes)?.trim().to_owned())
            };
        let tree = private_git(&["rev-parse", "HEAD^{tree}"])?;
        let head = private_git(&[
            "commit-tree",
            &tree,
            "-p",
            &launch.base.to_string(),
            "-m",
            "private commit",
        ])?;
        private_git(&["update-ref", launch.branch.as_str(), &head])?;
        let (slot, binding) = launch.custody.take().ok_or("writer custody")?;
        let slot_path = execution
            .state_root
            .as_path()
            .join("worktrees")
            .join(execution.project_id.to_string())
            .join(provenant_confinement::worktrees::state_dir_name(
                &fixture.writer.worktree,
            ));
        let lease = provenant_supervisor::fixture::acquire_writer_with_binding(
            slot.try_clone()?,
            scope,
            binding,
            call.lock_budget(),
            &cancel,
            &clock,
        )?;
        drop(provenant_supervisor::fixture::bind_writer_attempt(
            lease, &attempt,
        )?);
        let modern = slot_path.join(id.to_string());
        let mut reservation: serde_json::Value =
            serde_json::from_slice(&std::fs::read(modern.join("reservation.json"))?)?;
        if mode == "no_summary_terminal_other_generation" {
            reservation["scope"]["generation"] = (scope.generation + 1).into();
            let anchor = provenant_os::fs::Directory::open_anchor(&modern)?.metadata()?;
            reservation["reservation_device"] = anchor.device.into();
            reservation["reservation_inode"] = anchor.inode.into();
            std::fs::write(
                modern.join("reservation.json"),
                serde_json::to_vec(&reservation)?,
            )?;
        }
        store.commit_lost(
            scope,
            "launch interrupted: provider never spawned; private evidence retained",
            &call,
        )?;
        let mut spawned = false;
        store.queue_reconcile_with(run.attempt_id, &call, |argv, _| {
            assert_eq!(argv[1..3], ["internal", "settle"]);
            spawned = true;
            Ok(())
        })?;
        assert!(spawned, "{mode}: reconcile must queue recovery");
        let queued = store.attempt_dir(run.attempt_id, false)?;
        assert!(
            crate::artifacts::read_sealed(&queued, "settlement-queued.json", RECORD_LIMIT)?
                .is_some(),
            "{mode}: reconcile must queue recovery"
        );
        if mode == "no_summary_terminal_other_generation" {
            continue;
        }
        let found = store.reconcile_with_observer(
            run.attempt_id,
            &crate::dispatch_tests::Fake::default(),
            &call,
            &provenant_supervisor::fixture::RecordedBootObserver,
        )?;
        assert!(
            matches!(
                found.as_slice(),
                [(
                    _,
                    crate::Reconciled::Finished
                        | crate::Reconciled::Withdrawn
                        | crate::Reconciled::WriterLeaseReleased { .. }
                )]
            ),
            "{mode}: {found:?}"
        );
        let terminal = store.record(id, &call)?;
        assert!(
            matches!(terminal.state, AttemptState::Terminal { .. }),
            "{mode}: {:?}",
            terminal.state
        );
        assert!(
            matches!(terminal.state, AttemptState::Terminal { outcome: Outcome::Lost, reason: Some(ref reason), .. } if reason.contains("never spawned")),
            "{:?}",
            terminal.state
        );
        assert!(store.writer_fences()?.is_empty());
        assert!(store.writer_fence_listing()?.issues.is_empty());
        assert!(!slot_path.join("reservation.json").exists());
        assert!(modern.join("settlement.json").is_file());
        assert_eq!(
            private_git(&["rev-parse", "HEAD"])?,
            head,
            "unlaunched private head retained"
        );
        store.gc_provider_state(
            &Directory::open_anchor(execution.state_root.as_path())?,
            7,
            clock
                .wall_now()?
                .checked_add(provenant_core::DurationMicros::from_seconds(2 * 86400)?)?,
            false,
            &call,
            &crate::RetentionScan::default(),
        )?;
        assert_eq!(
            private_git(&["rev-parse", "HEAD"])?,
            head,
            "private head retained after GC eligibility"
        );
    }
    Ok(())
}

#[test]
fn settlement_finish_cleans_writer_pins_with_recovery_held()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    settlement_finish_case(false)
}

#[test]
fn settlement_finish_preserves_private_staged_snapshot_through_payload_gc()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    settlement_finish_case(true)
}

fn settlement_finish_case(
    private_staged: bool,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    use crate::delivery_tests::{PrepareFixture, registered_landed_writer_fixture};
    use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
    use std::{
        ffi::{OsStr, OsString},
        time::Duration,
    };

    let case = if private_staged {
        "private_staged"
    } else {
        "clean"
    };
    let fixture = PrepareFixture::new()?;
    let base_contents = "base contents\n";
    let staged_contents = "private staged snapshot\n";
    if private_staged {
        std::fs::write(fixture.writer.worktree.join("base.txt"), base_contents)?;
        fixture.lane_git(&["add", "base.txt"])?;
        fixture.commit(&fixture.writer.worktree, "add base file")?;
    }
    let execution = registered_landed_writer_fixture(&fixture)?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&execution, &call)?;
    let mut run = input();
    run.request.task.writer = Some(fixture.writer.clone());
    store.create(&run, &call)?;
    let id = run.attempt_id;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let record = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let scope = RunStore::scope(&record);
    let attempt = store.attempt_dir(id, false)?;
    let mut launch = crate::prepare_writer(
        &fixture.writer,
        execution.state_root.as_path(),
        execution.project_id,
    )?;
    launch.snapshot(
        execution.state_root.as_path(),
        &attempt,
        scope,
        &provenant_records::policy::Policy::default(),
    )?;
    let candidate = store.temporary_path(id, &call)?.join("candidate-0");
    std::fs::create_dir_all(&candidate)?;
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    crate::objects::prepare_bound(
        &git,
        launch.worktree.repo(),
        &attempt,
        &candidate,
        launch.branch.as_str(),
        launch.base,
    )?;
    let private_git = |args: &[&str]| -> std::result::Result<String, Box<dyn std::error::Error>> {
        let argv: Vec<OsString> = std::iter::once(git.as_os_str().to_owned())
            .chain(args.iter().map(|arg| OsString::from(*arg)))
            .collect();
        let mut environment: Vec<(OsString, OsString)> = [
            ("PATH", "/usr/bin:/bin"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ]
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect();
        environment.extend([
            (
                "GIT_DIR".into(),
                candidate.join("git-common").into_os_string(),
            ),
            (
                "GIT_COMMON_DIR".into(),
                candidate.join("git-common").into_os_string(),
            ),
            (
                "GIT_OBJECT_DIRECTORY".into(),
                candidate.join("git-objects").into_os_string(),
            ),
        ]);
        let mut options = ExecOptions::new(&argv, &fixture.writer.worktree, Duration::from_secs(5));
        options.env = Some(&environment);
        let output = SystemExecutor::new(1)?.execute(options)?;
        assert!(
            output.termination == Termination::Exit(0),
            "{case}: {}",
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        Ok(String::from_utf8(output.stdout.bytes)?.trim().to_owned())
    };
    let staged_file = fixture.writer.worktree.join("base.txt");
    if private_staged {
        std::fs::write(&staged_file, staged_contents)?;
        let staged_blob = private_git(&["hash-object", "-w", "base.txt"])?;
        let cacheinfo = format!("100644,{staged_blob},base.txt");
        private_git(&["update-index", "--add", "--cacheinfo", &cacheinfo])?;
        std::fs::write(&staged_file, base_contents)?;
        assert_eq!(
            private_git(&["show", "HEAD:base.txt"])?,
            base_contents.trim()
        );
        assert_eq!(private_git(&["show", ":base.txt"])?, staged_contents.trim());
    }
    let native_status = || -> std::result::Result<Vec<u8>, Box<dyn std::error::Error>> {
        let argv: Vec<OsString> = [
            git.as_os_str().to_owned(),
            "-C".into(),
            fixture.writer.worktree.as_os_str().to_owned(),
            "status".into(),
            "--porcelain".into(),
            "-z".into(),
        ]
        .into();
        let mut options = ExecOptions::new(&argv, &fixture.writer.project, Duration::from_secs(5));
        options.env = Some(&fixture.environment);
        let output = SystemExecutor::new(1)?.execute(options)?;
        assert!(
            output.termination == Termination::Exit(0),
            "{case}: {}",
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        Ok(output.stdout.bytes)
    };
    assert!(
        native_status()?.is_empty(),
        "{case}: native Git state must be clean"
    );

    let pin = crate::objects::import_pin(scope.attempt, launch.base);
    fixture.git(&["update-ref", &pin, &launch.base.to_string()])?;
    let (directory, binding) = launch.custody.take().ok_or("writer custody")?;
    let lease = provenant_supervisor::fixture::acquire_writer_with_binding(
        directory,
        scope,
        binding,
        call.lock_budget(),
        &cancel,
        &clock,
    )?;
    drop(provenant_supervisor::fixture::bind_writer_attempt(
        lease, &attempt,
    )?);
    let mut observed = crate::tests::exit();
    observed.scope = scope;
    crate::dispatch_tests::seal_exit(&store, &observed)?;
    crate::artifacts::seal(
        &attempt,
        "launch.json",
        &crate::db::bytes(
            &serde_json::json!({"schema_version":1,"scope":scope,"requester":execution.actor_id}),
        )?,
        RECORD_LIMIT,
    )?;
    let finished = store.finish(
        id,
        &attempt,
        &observed,
        &crate::dispatch_tests::Fake::default(),
        &call,
    )?;
    let receipts = finished.settlement_receipts.unwrap();
    assert_eq!(receipts.git_import, EffectDisposition::Done, "{case}");
    assert_eq!(
        receipts.cleanup_custody_release,
        EffectDisposition::Done,
        "{case}"
    );
    assert!(crate::objects::resolve_ref(&git, launch.worktree.repo(), &pin).is_err());
    let journal: serde_json::Value = crate::db::decode(
        &attempt.read_stable(OsStr::new("settlement-progress.json"), RECORD_LIMIT)?,
    )?;
    assert_eq!(journal["phase"], "complete", "{case}");
    let db = rusqlite::Connection::open(execution.state_root.as_path().join("run.sqlite3"))?;
    let retained: bool = db.query_row(
        "SELECT tmp_retained FROM attempts WHERE id=?1",
        [id.to_string()],
        |row| row.get(0),
    )?;
    assert!(candidate.exists(), "{case}: private candidate was cleaned");
    if private_staged {
        assert!(!retained, "staged private Git is not cleanup debt");
        let retained_index =
            crate::artifacts::read_sealed(&attempt, "retained-index.json", RECORD_LIMIT)?
                .ok_or("staged index marker")?;
        let retained_index: serde_json::Value = serde_json::from_slice(&retained_index)?;
        assert_eq!(retained_index["paths"], serde_json::json!(["base.txt"]));
        assert_eq!(private_git(&["show", ":base.txt"])?, staged_contents.trim());
        assert_eq!(std::fs::read_to_string(&staged_file)?, base_contents);
        // Pressure collection measures evictable provider state, while
        // private Git lives in tmp. Give the collector actual payload to
        // evict unless the retained-index marker pins the whole attempt.
        let payload = attempt.current_path()?.join("provider-state/candidate-0");
        std::fs::create_dir_all(&payload)?;
        let probe = vec![7_u8; 262_144];
        std::fs::write(payload.join("blob"), &probe)?;
        let collection_time = clock
            .wall_now()?
            .checked_add(provenant_core::DurationMicros::from_seconds(2 * 86_400)?)?;
        let collection = store.gc_payloads(
            &Directory::open_anchor(execution.state_root.as_path())?,
            0,
            collection_time,
            false,
            true,
            None,
            &call,
            &crate::retention::RetentionScan::default(),
        )?;
        assert!(collection.errors.is_empty(), "{collection:?}");
        assert!(collection.evicted.is_empty(), "{collection:?}");
        assert!(
            collection.pinned_bytes > 0,
            "private Git must stay pinned: {collection:?}"
        );
        assert!(candidate.is_dir(), "payload collection removed private Git");
        assert_eq!(std::fs::read(payload.join("blob"))?, probe);
        assert_eq!(private_git(&["show", ":base.txt"])?, staged_contents.trim());
        assert_eq!(std::fs::read_to_string(&staged_file)?, base_contents);
    } else {
        assert!(
            retained,
            "unfinished clean work retains scratch after release"
        );
    }
    assert!(
        native_status()?.is_empty(),
        "{case}: native Git state must stay clean"
    );
    Ok(())
}
#[test]
fn clean_settlement_retry_accepts_deleted_private_view_with_retirement_receipt()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    use crate::delivery_tests::{PrepareFixture, registered_landed_writer_fixture};

    let fixture = PrepareFixture::new()?;
    let execution = registered_landed_writer_fixture(&fixture)?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&execution, &call)?;
    let mut run = input();
    run.request.task.writer = Some(fixture.writer.clone());
    store.create(&run, &call)?;
    let id = run.attempt_id;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let record = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let scope = RunStore::scope(&record);
    let attempt = store.attempt_dir(id, false)?;
    let mut launch = crate::prepare_writer(
        &fixture.writer,
        execution.state_root.as_path(),
        execution.project_id,
    )?;
    launch.snapshot(
        execution.state_root.as_path(),
        &attempt,
        scope,
        &provenant_records::policy::Policy::default(),
    )?;
    let candidate = store.temporary_path(id, &call)?.join("candidate-0");
    std::fs::create_dir_all(&candidate)?;
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    crate::objects::prepare_bound(
        &git,
        launch.worktree.repo(),
        &attempt,
        &candidate,
        launch.branch.as_str(),
        launch.base,
    )?;
    assert!(launch.private_staged_paths(&attempt)?.is_empty());
    let (directory, binding) = launch.custody.take().ok_or("writer custody")?;
    let slot = directory.try_clone()?;
    let recovery_directory = directory.try_clone()?;
    let pin = crate::objects::import_pin(scope.attempt, launch.base);
    fixture.git(&["update-ref", &pin, &launch.base.to_string()])?;
    let lease = provenant_supervisor::fixture::acquire_writer_with_binding(
        directory,
        scope,
        binding,
        call.lock_budget(),
        &cancel,
        &clock,
    )?;
    drop(provenant_supervisor::fixture::bind_writer_attempt(
        lease, &attempt,
    )?);
    provenant_supervisor::custody::register_attempt_index(
        &slot,
        scope,
        &attempt,
        "candidate-0",
        launch.branch.as_str(),
    )?;
    let mut recovery = store
        .recover_writer_exit(recovery_directory.try_clone()?, scope, &call)?
        .ok_or("prepared writer recovery")?;
    recovery.prepare_settlement()?;
    let mut observed = crate::tests::exit();
    observed.scope = scope;
    crate::dispatch_tests::seal_exit(&store, &observed)?;
    crate::artifacts::seal(
        &attempt,
        "launch.json",
        &crate::db::bytes(
            &serde_json::json!({"schema_version":1,"scope":scope,"requester":execution.actor_id}),
        )?,
        RECORD_LIMIT,
    )?;
    let finished = store.drive_settlement(
        id,
        &attempt,
        &observed,
        Completion::Recovered(Some(&recovery)),
        &crate::dispatch_tests::Fake::default(),
        &call,
    )?;
    assert!(finished.release_permitted);
    assert_eq!(
        finished.record.settlement_receipts.unwrap().git_import,
        EffectDisposition::Done
    );

    // Match the durable prefix of settle_recovered, then model a crash after
    // private scratch removal but before the still-live reservation is cleared.
    crate::writer::settle_launch_staging(&attempt)?;
    provenant_supervisor::custody::publish_release(&attempt, scope)?;
    crate::settlement::complete(&attempt, scope)?;
    let commit = recovery
        .verify_settlement(&attempt)
        .map_err(|e| format!("verify clean release: {e}"))?;
    recovery
        .retire_attempt_index(&commit)
        .map_err(|e| format!("retire clean index: {e}"))?;
    let mut scratch = provenant_os::scratch::Scratch::new(attempt.try_clone()?)?;
    scratch.settle()?;
    // `temporary_path` creates the directory it resolves, so use the pure
    // path helper to model removal and to assert absence (#3947).
    let temporary = crate::plan::scratch_root(execution.state_root.as_path(), id).join("tmp");
    std::fs::remove_dir_all(&temporary)?;
    assert!(!candidate.exists());
    assert!(provenant_supervisor::custody::attempt_index_retired(
        &slot, scope
    )?);
    let reservation = slot
        .current_path()?
        .join(id.to_string())
        .join("reservation.json");
    assert!(
        reservation.is_file(),
        "reservation remains the removal fence"
    );
    assert!(!store.writer_fences()?.is_empty());
    drop(scratch);
    drop(commit);
    drop(recovery);

    let mut retry = store
        .recover_writer_exit(recovery_directory, scope, &call)?
        .ok_or("reservation must remain recoverable")?;
    retry
        .prepare_settlement()
        .map_err(|e| format!("prepare retry: {e}"))?;
    store
        .settle_recovered(&attempt, retry)
        .map_err(|e| format!("settle retry: {e}"))?;
    assert!(store.writer_fences()?.is_empty());
    assert!(!reservation.exists());
    assert!(
        slot.current_path()?
            .join(id.to_string())
            .join("settlement.json")
            .is_file()
    );
    assert!(provenant_supervisor::custody::attempt_index_retired(
        &slot, scope
    )?);
    assert!(!temporary.exists());
    assert!(store.custody_released(&store.record(id, &call)?, &call)?);
    Ok(())
}

#[test]
fn budget_warning_records_the_steer_refusal_for_lane_providers()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    use provenant_records::control::{
        ControlKind, ControlReceipt, ControlState, ControlSupport, LaunchKind,
    };
    use provenant_supervisor::Holder;
    for provider in ["claude", "codex", "command", "unknown"] {
        let temp = tempfile::tempdir()?;
        let base = std::fs::canonicalize(temp.path())?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        let execution = context(temp.path())?;
        let store = RunStore::open(&execution, &call)?;
        let run = input();
        store.create(&run, &call)?;
        let id = run.attempt_id;
        let applied = RouteRecord {
            provider: Some(provider.into()),
            ..route()
        };
        store.mark_starting(
            &store.record(id, &call)?,
            &applied,
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let attempt = store.attempt_dir(id, false)?;
        let context = HolderContext {
            schema_version: 1,
            roots: RootSeed {
                home: base.join("home"),
                checkout: base.join("checkout"),
                state_root: execution.state_root.as_path().to_path_buf(),
            },
            project_id: execution.project_id,
            actor_id: execution.actor_id,
            attempt_id: id,
            host_delivery: None,
            launch: None,
            sealed: None,
        };
        let holder = RunHolder {
            providers: &crate::dispatch_tests::Fake::default(),
            resources: &crate::tests::HostFacts::new(100),
            clock: &clock,
        };
        holder.budget_warning(&context.encode()?, 450_000, 500_000)?;
        let warning: serde_json::Value = serde_json::from_slice(
            &read_sealed(&attempt, "budget-warning.json", RECORD_LIMIT)?.ok_or("budget warning")?,
        )?;
        assert_eq!(warning["tokens"], 450_000);
        assert_eq!(warning["budget"], 500_000);
        let receipt: ControlReceipt = serde_json::from_value(warning["receipt"].clone())?;
        assert_eq!(receipt.control, ControlKind::Steer);
        assert_eq!(receipt.target, id.to_string());
        assert_eq!(receipt.launch, LaunchKind::Lane);
        assert_eq!(receipt.state, ControlState::Unsupported);
        let support = provenant_records::ADAPTER_SPECS
            .iter()
            .find(|spec| spec.name == provider)
            .map_or(ControlSupport::Unsupported, |spec| {
                spec.launch.control.for_launch(LaunchKind::Lane)
            });
        assert_eq!(receipt.support, support);
        assert!(!receipt.support.delivers());
        assert_eq!(
            receipt.reason,
            "token-budget warning is recorded locally and is not delivered to the live provider"
        );
        assert_eq!(receipt.transport, None);
        assert_eq!(receipt.message_id, None);
    }
    Ok(())
}

// Spec: docs/specs/fabric.md#session-ready-steering
#[test]
fn stopped_writer_preserves_work_and_arbitrates_steer_continuation()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    use crate::delivery_tests::{PrepareFixture, registered_landed_writer_fixture};
    use provenant_records::wire::lane::{LaneProjectionFacts, LaneRow};
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Marker {
        None,
        AdmissionSteer,
        AdmissionCancel,
        DeliveredSteer,
        ExplicitCancel,
        ExplicitAfterSteer,
        DeliveredSteerUnavailable,
        DeliveredSteerDescendant,
        DeliveredSteerAdopted,
        Deadline,
        // The outcome commits on the outcome-only pass; the worktree turns dirty before the quiet pass delivers.
        LateDirty,
    }
    use Marker::{
        DeliveredSteer, DeliveredSteerAdopted, DeliveredSteerDescendant, DeliveredSteerUnavailable,
        ExplicitAfterSteer, ExplicitCancel,
    };
    for (changed, dirty, blocked, shared, budget, marker) in [
        (false, false, false, false, false, Marker::AdmissionSteer),
        (false, false, false, false, false, Marker::AdmissionCancel),
        (false, false, false, false, false, Marker::None),
        (true, false, false, false, false, Marker::None),
        (false, true, false, false, false, Marker::None),
        (false, false, true, false, false, Marker::None),
        (true, false, false, true, false, Marker::None),
        (true, true, false, true, false, ExplicitCancel),
        (true, true, false, true, false, Marker::Deadline),
        // Unchanged and unshared: the worktree is still at the base, so the late
        // edit is uncommitted work (a writer that already committed is not).
        (false, false, false, false, false, Marker::LateDirty),
        (true, false, false, true, false, DeliveredSteer),
        (true, false, false, true, false, DeliveredSteerUnavailable),
        (true, false, false, true, false, DeliveredSteerDescendant),
        (true, false, false, true, false, DeliveredSteerAdopted),
        (true, false, false, true, false, ExplicitCancel),
        (true, false, false, true, false, ExplicitAfterSteer),
        (true, false, false, false, true, Marker::None),
        (false, true, false, false, true, Marker::None),
    ] {
        let steered = matches!(
            marker,
            DeliveredSteer
                | ExplicitAfterSteer
                | DeliveredSteerUnavailable
                | DeliveredSteerDescendant
                | DeliveredSteerAdopted
        );
        let explicit = matches!(marker, ExplicitCancel | ExplicitAfterSteer);
        let late = marker == Marker::LateDirty;
        let plain = matches!(marker, Marker::None | Marker::LateDirty);
        let fixture = PrepareFixture::new()?;
        let execution = registered_landed_writer_fixture(&fixture)?;
        let (mut clock, cancel) = (clock(), CancelToken::new());
        let mut call = call(&clock, &cancel);
        let store = RunStore::open(&execution, &call)?;
        let mut run = input();
        run.request.task.writer = Some(fixture.writer.clone());
        if marker == Marker::Deadline {
            run.request
                .task
                .profile
                .knobs
                .insert("lanes.publish".into(), serde_json::Value::Bool(false));
        }
        let source = String::from_utf8(provenant_os::fixture::run_git(
            &fixture.writer.worktree,
            &["rev-parse", "HEAD"],
        )?)?
        .trim()
        .to_owned();
        run.request.task.head = Some(source.clone());
        if late {
            run.request.task.deliver = Some(fixture.request.clone());
        }
        store.create(&run, &call)?;
        let id = run.attempt_id;
        let limits = provenant_records::LaneLimits::default();
        store.mark_starting_with_priority(
            &store.record(id, &call)?,
            &route(),
            &[],
            &Default::default(),
            None,
            None,
            &limits,
            &call,
        )?;
        if matches!(marker, Marker::AdmissionSteer | Marker::AdmissionCancel) {
            let record = store.record(id, &call)?;
            let scope = RunStore::scope(&record);
            let attempt = store.attempt_dir(id, false)?;
            crate::artifacts::seal(
                &attempt,
                "stop-intent.json",
                &crate::db::bytes(&serde_json::json!({
                    "schema_version": 1,
                    "scope": scope,
                    "requester": execution.actor_id,
                    "request_id": format!("{}.{}.stop", scope.attempt, scope.generation),
                    "reason": if marker == Marker::AdmissionSteer {
                        StopReason::Steered
                    } else {
                        StopReason::ExplicitCancellation
                    }
                }))?,
                RECORD_LIMIT,
            )?;
            let admitted = store.admit(
                id,
                &attempt,
                &crate::tests::HostFacts::new(100),
                &clock,
                &cancel,
            );
            if marker == Marker::AdmissionCancel {
                assert!(matches!(admitted, Err(RunError::Cancelled)));
                assert_eq!(store.record(id, &call)?.state, record.state);
            } else {
                assert!(matches!(admitted?.state, AttemptState::Running { .. }));
            }
            continue;
        }
        let record = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let scope = RunStore::scope(&record);
        let attempt = store.attempt_dir(id, false)?;
        let mut launch = crate::prepare_writer(
            &fixture.writer,
            execution.state_root.as_path(),
            execution.project_id,
        )?;
        if marker == DeliveredSteerDescendant {
            let mut owner = provenant_confinement::worktrees::load_owner(&launch.worktree)?
                .ok_or("writer owner")?;
            owner.origin = provenant_confinement::worktrees::WorktreeOrigin::Lane;
            owner.attempt_id = Some(id);
            owner.operation_id = Some(run.operation_id.clone());
            provenant_confinement::worktrees::save_owner(&launch.worktree, &owner)?;
        }
        launch.snapshot(
            execution.state_root.as_path(),
            &attempt,
            scope,
            &provenant_records::policy::Policy::default(),
        )?;
        let candidate = store.temporary_path(id, &call)?.join("candidate-0");
        std::fs::create_dir_all(&candidate)?;
        let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
        crate::objects::prepare_bound(
            &git,
            launch.worktree.repo(),
            &attempt,
            &candidate,
            launch.branch.as_str(),
            launch.base,
        )?;
        let pin = crate::objects::import_pin(scope.attempt, launch.base);
        fixture.git(&["update-ref", &pin, &launch.base.to_string()])?;
        let (directory, binding) = launch.custody.take().ok_or("writer custody")?;
        let mut lease = provenant_supervisor::custody::acquire_writer(
            directory,
            scope,
            binding,
            call.lock_budget(),
            &cancel,
            &clock,
        )?;
        if matches!(
            marker,
            DeliveredSteer
                | DeliveredSteerUnavailable
                | DeliveredSteerDescendant
                | DeliveredSteerAdopted
                | ExplicitCancel
                | ExplicitAfterSteer
                | Marker::Deadline
        ) {
            use provenant_supervisor::census::{ProcessObservation, SystemProcessObserver};
            let mut provider = match SystemProcessObserver.observe(i64::from(std::process::id())) {
                ProcessObservation::Present(fact) if !fact.zombie => fact.identity,
                observation => {
                    return Err(format!("fixture process unavailable: {observation:?}").into());
                }
            };
            let capture = provenant_os::capture::CaptureSourceIdentity {
                attempt: id.to_string(),
                generation: scope.generation,
                boot: provider.boot.clone(),
                pid: provider.pid,
                native_start: provider.native_start,
            };
            if marker == Marker::Deadline {
                crate::artifacts::seal(
                    &attempt,
                    "stop-intent.json",
                    &crate::db::bytes(&serde_json::json!({
                        "schema_version": 1,
                        "scope": scope,
                        "requester": execution.actor_id,
                        "request_id": format!("{}.{}.stop", scope.attempt, scope.generation),
                        "reason": StopReason::WallDeadline,
                    }))?,
                    RECORD_LIMIT,
                )?;
            } else {
                assert!(matches!(
                    store.steer(
                        id,
                        "writer-marker-arbitration".parse()?,
                        "Continue from the committed writer head.",
                        &call,
                    )?,
                    crate::SteerReceipt::Queued { .. }
                ));
            }
            if marker == ExplicitCancel {
                store.cancel(id, "explicit stop instead of steer", &call)?;
            }
            // Session readiness uses the actual steer path, which writes
            // custody's versioned StopIntent rather than an exit flag.
            assert!(store.record_provider_session_ready(
                id,
                &route(),
                "writer-session",
                &capture,
                &call
            )?);
            if steered {
                // A steer never interrupts a running attempt. Model the
                // steered stop as an independent custody request so the
                // settlement arbitration cases below keep their coverage.
                provenant_supervisor::custody::request_stop(
                    attempt.try_clone()?,
                    scope,
                    execution.actor_id,
                    StopReason::Steered,
                )?;
            }
            if marker == DeliveredSteerUnavailable {
                // The delivered stop remains durable while readiness can no
                // longer prove that the prior provider session is resumable.
                store.db.execute(
                    "DELETE FROM provider_session_ready WHERE attempt=?1",
                    [id.to_string()],
                )?;
            }
            let saved: provenant_supervisor::custody::StopIntent = crate::db::decode(
                &read_sealed(&attempt, "stop-intent.json", RECORD_LIMIT)?.ok_or("cancel marker")?,
            )?;
            assert_eq!(
                saved.reason,
                match marker {
                    Marker::Deadline => StopReason::WallDeadline,
                    _ if steered => StopReason::Steered,
                    _ => StopReason::ExplicitCancellation,
                }
            );
            // Keep the live provider binding for session readiness, but
            // model a guardian identity whose PID has since been reused by
            // this fixture process. Its different native start proves that
            // guardian absent without assuming an arbitrary PID is free.
            let guardian = provenant_supervisor::census::ProcessIdentity {
                boot: provider.boot.clone(),
                pid: provider.pid,
                native_start: provider.native_start.wrapping_sub(1),
                group_id: provider.pid,
                session_id: provider.pid,
            };
            provider.group_id = provider.pid;
            provider.session_id = guardian.session_id;
            provenant_supervisor::fixture::record_spawned_provider(
                &mut lease, &attempt, guardian, provider,
            )?;
        } else {
            lease = provenant_supervisor::fixture::bind_writer_attempt(lease, &attempt)?;
        }
        drop(lease);
        let mut shared_head = None;
        if changed {
            let objects = candidate.join("git-objects");
            let private_git = |args: &[std::ffi::OsString]| {
                crate::objects::execute_bounded(
                    &git,
                    &candidate.join("git-common"),
                    &fixture.writer.worktree,
                    args,
                    None,
                    crate::objects::Execution {
                        object_directory: Some(&objects),
                        ..Default::default()
                    },
                )
            };
            let head = private_git(&[
                "-c".into(),
                "user.name=Fixture".into(),
                "-c".into(),
                "user.email=fixture@example.invalid".into(),
                "commit-tree".into(),
                format!("{}^{{tree}}", launch.base).into(),
                "-p".into(),
                launch.base.to_string().into(),
                "-m".into(),
                "Partial work".into(),
            ])?;
            let head = std::str::from_utf8(&head)?.trim();
            if shared {
                // A native writer edits files in the shared worktree and commits
                // them with ordinary Git, which lands in its private history.
                std::fs::write(
                    fixture.writer.worktree.join("shared-work.txt"),
                    "committed native work",
                )?;
                let native = provenant_os::git::resolve_git_executable(None)
                    .ok_or("native Git unavailable")?;
                let native_private = |args: &[&str]| {
                    crate::delivery_tests::private_git(&fixture, &candidate, &native, args)
                };
                native_private(&["add", "shared-work.txt"])?;
                native_private(&["commit", "-m", "Shared commit"])?;
                shared_head = Some(crate::objects::private_tip(
                    &crate::objects::bound_source(&attempt)?.ok_or("private source")?,
                    launch.branch.as_str(),
                )?);
            } else {
                private_git(&[
                    "update-ref".into(),
                    launch.branch.as_str().into(),
                    head.into(),
                ])?;
            }
        }
        if dirty {
            std::fs::write(
                fixture.writer.worktree.join("unfinished.txt"),
                "retained partial edit",
            )?;
        }
        let mut observed = crate::tests::exit();
        observed.scope = scope;
        if budget {
            observed.stop_reason = Some(StopReason::TokenBudget);
            observed.termination = Some(Termination::Exit(1));
        } else if marker == Marker::Deadline {
            observed.stop_reason = Some(StopReason::WallDeadline);
            observed.termination = Some(Termination::Exit(1));
        } else if steered {
            observed.stop_reason = Some(StopReason::Steered);
            observed.cancel_delivered_before_exit = Some(true);
        }
        if marker == ExplicitCancel {
            observed.stop_reason = Some(StopReason::ExplicitCancellation);
            observed.cancel_delivered_before_exit = Some(true);
        }
        let deferred_outcome = (changed && shared || late) && !dirty && !budget && plain;
        let mut first_exit = observed.clone();
        first_exit.unresolved_cleanup = deferred_outcome;
        crate::dispatch_tests::seal_exit(&store, &first_exit)?;
        if marker == ExplicitAfterSteer {
            assert_eq!(
                store.cancel(id, "explicit stop after delivered steer", &call)?,
                crate::CancelReceipt::Requested
            );
        }
        let mut fake =
            crate::dispatch_tests::Fake::with_observed(Some(route()), Some(Outcome::Succeeded));
        if !plain {
            fake.session = Some("writer-session".into());
        }
        if budget {
            fake.outcome = Some(Outcome::Failed);
            fake.complete = Some(true);
        }
        if marker == Marker::Deadline {
            // A host deadline kill is not a provider success.
            fake.outcome = Some(Outcome::Failed);
        }
        let note = if changed {
            "No commit was made; acceptance remains unfinished"
        } else {
            "Acceptance remains unfinished"
        };
        let status = if budget || late {
            "done"
        } else if blocked {
            "blocked"
        } else {
            "stopped_early"
        };
        fake.answer = Some(format!(
            "```handoff\n{{\"schema_version\":2,\"status\":\"{status}\",\"note\":\"{note}\",\"remaining\":[\"Finish work\"],\"decisions\":[],\"verify\":[]}}\n```"
        ));
        if late {
            fake.answer = Some("```handoff\n{\"schema_version\":2,\"status\":\"done\",\"note\":\"Delivered\",\"remaining\":[],\"decisions\":[],\"verify\":[]}\n```".into());
        }
        if budget {
            fake.answer = Some("```handoff\n{\"schema_version\":2,\"status\":\"done\",\"note\":\"Delivered\",\"remaining\":[],\"decisions\":[],\"verify\":[]}\n```".into());
        }
        if deferred_outcome {
            let holder_context = HolderContext {
                schema_version: 1,
                roots: RootSeed {
                    home: fixture.writer.project.join("home"),
                    checkout: fixture.writer.project.clone(),
                    state_root: execution.state_root.as_path().to_path_buf(),
                },
                project_id: execution.project_id,
                actor_id: execution.actor_id,
                attempt_id: id,
                host_delivery: None,
                launch: None,
                sealed: None,
            };
            let early = store.drive_settlement(
                id,
                &attempt,
                &first_exit,
                Completion::Outcome {
                    holder_context: &holder_context,
                },
                &fake,
                &call,
            )?;
            assert!(!store.import_release_permitted(&early.record, &call)?);
            assert!(read_sealed(&attempt, "writer-terminal.json", RECORD_LIMIT)?.is_none());
            if late {
                assert!(matches!(
                    early.record.state,
                    AttemptState::Terminal {
                        outcome: Outcome::Succeeded,
                        ..
                    }
                ));
                std::fs::write(
                    fixture.writer.worktree.join("late-dirty.txt"),
                    "edited after the outcome committed",
                )?;
            }
            let mut resolved = serde_json::to_value(&observed)?;
            resolved
                .as_object_mut()
                .ok_or("exit")?
                .remove("publication");
            resolved["schema_version"] = 1.into();
            attempt
                .atomic_write(
                    std::ffi::OsStr::new("process-exit.json"),
                    ".test-",
                    &serde_json::to_vec(&resolved)?,
                    provenant_os::fs::PublishMode::Replace,
                )?
                .require_durable()?;
        }
        let retry_summary = if marker == ExplicitCancel && dirty {
            attempt.mkdir(std::ffi::OsStr::new("terminal-effects.json"), 0o700)?;
            let failed = store.finish(id, &attempt, &observed, &fake, &call);
            assert!(failed.is_err(), "injected post-import persistence failure");
            let raw = read_sealed(&attempt, "writer-terminal.json", RECORD_LIMIT)?
                .ok_or("writer terminal sealed before fault")?;
            let saved: crate::writer::WriterTerminal = crate::db::decode(&raw)?;
            fixture.git(&[
                "update-ref",
                "refs/remotes/origin/lane",
                &saved.head.to_string(),
            ])?;
            attempt.rmdir(std::ffi::OsStr::new("terminal-effects.json"))?;
            // The failed driver armed the settlement retry backoff; replay after it.
            let Some(crate::settlement::Refusal::RetryDeadline(deadline)) =
                crate::settlement::refusal(&attempt, scope, &clock)?
            else {
                return Err("failed settlement did not arm its retry backoff".into());
            };
            let now = u64::try_from(clock.wall_now()?.unix_millis())?;
            clock.advance(provenant_core::DurationMicros::from_micros(
                deadline.saturating_sub(now) * 1000,
            ))?;
            call = crate::tests::call(&clock, &cancel);
            Some(raw)
        } else {
            None
        };
        let finished = store.finish(id, &attempt, &observed, &fake, &call)?;
        if matches!(marker, DeliveredSteerDescendant | DeliveredSteerAdopted) {
            // The writer commits in its own worktree after the stop was
            // sealed: the successor pin is now a strict ancestor of HEAD.
            std::fs::write(
                fixture.writer.worktree.join("post-steer.txt"),
                "continued after the seal",
            )?;
            let native = provenant_os::git::resolve_git_executable(None).ok_or("native Git")?;
            crate::delivery_tests::worktree_git(&fixture, &native, &["add", "post-steer.txt"])?;
            crate::delivery_tests::worktree_git(
                &fixture,
                &native,
                &["commit", "-m", "Post-seal writer commit"],
            )?;
        }
        if let Some(summary) = retry_summary {
            assert_eq!(
                read_sealed(&attempt, "writer-terminal.json", RECORD_LIMIT)?,
                Some(summary),
                "replay keeps the sealed pre-push/ahead summary"
            );
        }
        if late {
            // The committed outcome stands; the dirty worktree is not delivered.
            assert!(store.import_release_permitted(&finished, &call)?);
            assert!(matches!(
                store.record(id, &call)?.state,
                AttemptState::Terminal {
                    outcome: Outcome::Succeeded,
                    ..
                }
            ));
            let delivery = store
                .record(id, &call)?
                .settlement_receipts
                .ok_or("settlement receipts")?
                .delivery;
            assert!(
                matches!(
                    &delivery,
                    EffectDisposition::Failed { reason, retryable: false }
                        if reason.contains("late-dirty.txt")
                ),
                "{delivery:?}"
            );
            let remote = String::from_utf8(provenant_os::fixture::run_git(
                &fixture.writer.worktree,
                &["ls-remote", "origin", "refs/heads/lane"],
            )?)?;
            assert!(remote.trim().is_empty(), "dirty writer pushed: {remote}");
            assert_eq!(
                std::fs::read_to_string(fixture.writer.worktree.join("late-dirty.txt"))?,
                "edited after the outcome committed"
            );
            continue;
        }
        if deferred_outcome {
            assert!(store.import_release_permitted(&finished, &call)?);
            assert!(store.custody_released(&finished, &call)?);
            let receipt = WriterDeliveryReceipt {
                extra: Default::default(),
                schema_version: 1,
                remote: "origin".into(),
                branch: "lane".into(),
                head: finished.head,
                review_acceptance: None,
                integration: None,
                state: WriterDeliveryState::Delivered {
                    extra: Default::default(),
                    pull_request: None,
                },
            };
            crate::artifacts::seal(
                &attempt,
                "delivery.json",
                &crate::db::bytes(&receipt)?,
                RECORD_LIMIT,
            )?;
            assert_eq!(
                store.record(id, &call)?.delivery,
                Some(receipt),
                "receipt follows the imported head overlay"
            );
        }
        if let Some(head) = shared_head {
            assert_eq!(finished.head, Some(head));
            let terminal: crate::writer::WriterTerminal = crate::db::decode(
                &read_sealed(&attempt, "writer-terminal.json", RECORD_LIMIT)?
                    .ok_or("writer terminal")?,
            )?;
            assert_eq!(terminal.head, head);
            if matches!(
                marker,
                ExplicitCancel | ExplicitAfterSteer | Marker::Deadline
            ) {
                let summary = terminal
                    .stop_work
                    .as_ref()
                    .ok_or("cancel stop-work summary")?;
                assert_eq!(summary.dirty_paths, Some(u64::from(dirty)));
                assert_eq!(
                    summary.push,
                    if marker == Marker::Deadline {
                        provenant_records::run::StopWorkPush::NotGranted
                    } else {
                        provenant_records::run::StopWorkPush::NotAttempted
                    }
                );
                if marker == Marker::Deadline {
                    let remote = String::from_utf8(provenant_os::fixture::run_git(
                        &fixture.writer.worktree,
                        &["ls-remote", "origin", "refs/heads/lane"],
                    )?)?;
                    assert!(
                        remote.trim().is_empty(),
                        "publish opt-out pushed a ref: {remote}"
                    );
                }
                assert_eq!(
                    summary.trigger,
                    if marker == Marker::Deadline {
                        provenant_records::run::StopWorkTrigger::WallDeadline
                    } else {
                        provenant_records::run::StopWorkTrigger::ExplicitCancellation
                    }
                );
            }
            let status = provenant_os::fixture::run_git(
                &fixture.writer.worktree,
                &["status", "--porcelain", "--untracked-files=normal"],
            )?;
            assert_eq!(
                status.is_empty(),
                !dirty || budget,
                "deadline and explicit cancel leave dirty files untouched; only budget keeps its existing commit"
            );
        }
        if marker == Marker::Deadline {
            assert!(
                matches!(
                    finished.state,
                    AttemptState::Terminal {
                        outcome: Outcome::TimedOut,
                        ..
                    }
                ),
                "{:?}",
                finished.state
            );
            continue;
        }
        if !plain {
            assert!(matches!(
                finished.state,
                AttemptState::Terminal {
                    outcome: Outcome::Cancelled,
                    cause,
                    ..
                } if cause == steered.then_some(CauseKind::Steered)
            ));
            let (child, warnings) =
                provenant_records::warning::capture(|| store.prepare_steer_continuation(id, &call));
            if marker == DeliveredSteerUnavailable {
                assert!(matches!(&child, Err(RunError::SteerSessionUnavailable)));
                let child = store
                    .prepare_fresh_steer_continuation(id, &call)?
                    .ok_or("fresh steer fallback did not admit successor")?;
                let task = store.request(child, &call)?.task;
                assert_eq!(task.head, shared_head.map(|head| head.to_string()));
                assert_eq!(task.source_head.as_deref(), Some(source.as_str()));
                assert_eq!(task.writer.as_ref(), Some(&fixture.writer));
                let prompt = String::from_utf8(
                    store.artifact_bytes(&store.record(child, &call)?.prompt, RECORD_LIMIT)?,
                )?;
                for retained in [
                    "Accepted brief:\nexact prompt",
                    "Previous handoff:",
                    "remains unfinished",
                    "Queued steering instructions:\nContinue from the committed writer head.",
                ] {
                    assert!(
                        prompt.contains(retained),
                        "fresh prompt lost {retained:?}: {prompt}"
                    );
                }
                assert_eq!(
                    store.record(child, &call)?.continuation.as_deref(),
                    Some("fresh")
                );
                assert_eq!(store.record(id, &call)?.superseded_by, Some(child));
                continue;
            }
            if marker == DeliveredSteerAdopted {
                assert!(matches!(child, Err(RunError::WriterLineage(_))));
                assert_eq!(store.record(id, &call)?.superseded_by, None);
                continue;
            }
            let child = child?;
            if explicit {
                assert_eq!(
                    child, None,
                    "explicit stop must veto continuation: {marker:?}"
                );
                assert_eq!(store.record(id, &call)?.superseded_by, None);
            } else {
                let child = child.ok_or("delivered steer did not admit successor")?;
                let task = store.request(child, &call)?.task;
                // The writer's own post-seal commit is a descendant of the
                // sealed head. The successor must adopt it, keep the source
                // pin and warn with both ids; any other head stays refused.
                let current: provenant_core::GitOid =
                    crate::delivery_tests::worktree_git(&fixture, &git, &["rev-parse", "HEAD"])?
                        .parse()?;
                if marker == DeliveredSteerDescendant {
                    let sealed = shared_head.ok_or("sealed head")?;
                    assert_ne!(current, sealed);
                    assert_eq!(task.head, Some(current.to_string()));
                    let warning = warnings
                        .iter()
                        .find(|warning| {
                            warning.code.as_ref()
                                == provenant_records::warning::BRANCH_BEHIND_BASE.as_str()
                        })
                        .ok_or("no re-pin warning")?;
                    assert!(warning.message.contains(sealed.to_string().as_str()));
                    assert!(warning.message.contains(current.to_string().as_str()));
                    let prompt = String::from_utf8(
                        store.artifact_bytes(&store.record(child, &call)?.prompt, RECORD_LIMIT)?,
                    )?;
                    assert!(prompt.contains(current.to_string().as_str()), "{prompt}");
                } else {
                    assert_eq!(task.head, shared_head.map(|head| head.to_string()));
                }
                assert_eq!(task.source_head.as_deref(), Some(source.as_str()));
                assert_eq!(task.writer.as_ref(), Some(&fixture.writer));
                let successor = crate::prepare_writer(
                    task.writer.as_ref().ok_or("successor writer")?,
                    &store.writer_registry,
                    execution.project_id,
                )?;
                successor.require_continuation_source(
                    &git,
                    source.parse()?,
                    current,
                    &provenant_records::policy::Policy::default(),
                )?;
                store.check_launch_writer_lineage(
                    &store.record(child, &call)?,
                    &task,
                    &successor.worktree,
                    &call,
                )?;
                assert_eq!(store.record(id, &call)?.superseded_by, Some(child));
                assert_eq!(store.prepare_steer_continuation(id, &call)?, Some(child));
            }
            continue;
        }
        if blocked {
            // A blocked writer seals one typed stop.
            let AttemptState::Terminal { cause, stop, .. } = &finished.state else {
                panic!("not terminal")
            };
            assert_eq!(*cause, Some(CauseKind::Blocked));
            let stop = stop.as_ref().ok_or("stop")?;
            assert_eq!(stop.cause(), provenant_records::StopCause::Blocked);
            assert_eq!(stop.note(), Some(note));
            continue;
        }
        assert_eq!(finished.superseded_by, None);
        if budget {
            let AttemptState::Terminal { stop, outcome, .. } = &finished.state else {
                unreachable!()
            };
            let stop = stop.as_ref().ok_or("stop")?;
            assert_eq!(
                stop.handoff().unwrap().status,
                provenant_records::handoff::HandoffStatus::Done
            );
            assert_eq!(
                stop.cause().source(),
                provenant_records::run::CauseSource::Host
            );
            if !dirty {
                assert_eq!(*outcome, Outcome::Succeeded);
                assert_eq!(
                    finished.settlement_class,
                    Some(provenant_records::run::SettlementClass::CompletedBeforeBudget)
                );
                continue;
            }
        }

        if dirty {
            assert!(matches!(
                finished.state,
                AttemptState::Terminal {
                    outcome: Outcome::Uncommitted,
                    ..
                }
            ));
            assert_eq!(
                std::fs::read_to_string(fixture.writer.worktree.join("unfinished.txt"))?,
                "retained partial edit"
            );
            assert_eq!(
                store.request(run.attempt_id, &call)?.task.writer.as_ref(),
                Some(&fixture.writer)
            );
            continue;
        }
        assert_eq!(
            finished.settlement_class,
            Some(provenant_records::run::SettlementClass::DeclaredStop)
        );
        let AttemptState::Terminal {
            cause,
            outcome,
            reason,
            ..
        } = &finished.state
        else {
            panic!("not terminal")
        };
        assert_eq!(*outcome, Outcome::ReviewRequired);
        assert_eq!(*cause, (!changed).then_some(CauseKind::StoppedNoWork));
        let row = LaneRow::project(&finished, LaneProjectionFacts::default());
        assert_eq!(row.cause, (!changed).then_some(CauseKind::StoppedNoWork));
        assert!(
            !reason
                .as_deref()
                .unwrap()
                .contains("handoff disagrees with the branch"),
            "{reason:?}"
        );
        if !changed {
            assert!(reason.as_deref().unwrap().contains("no new commit"));
            assert_eq!(
                LaneRow::state_and_label(&finished.state).1.as_deref(),
                Some(CauseKind::StoppedNoWork.reason())
            );
            assert_eq!(
                row.reason.as_deref(),
                Some(CauseKind::StoppedNoWork.detail())
            );
            assert_eq!(
                row.settlement.unwrap().reason.as_deref(),
                Some(CauseKind::StoppedNoWork.detail())
            );
        }
    }
    Ok(())
}

#[test]
fn empty_committed_writer_requires_review_and_controls_fail()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let test = provenant_os::fixture::exact_test_path(
        module_path!(),
        "empty_committed_writer_requires_review_and_controls_fail",
    );
    if provenant_os::fixture::reexec_with_clean_git(&test)? {
        return Ok(());
    }
    use crate::delivery_tests::{PrepareFixture, registered_landed_writer_fixture};
    use provenant_records::wire::lane::{LaneProjectionFacts, LaneRow};
    for (writer, outcome, answer) in [
        (true, Outcome::Succeeded, ""),
        (false, Outcome::Succeeded, ""),
        (true, Outcome::Failed, ""),
        (true, Outcome::Succeeded, "VERDICT: PASS"),
        (true, Outcome::Succeeded, "VERDICT: FAIL"),
        (true, Outcome::Succeeded, "VERDICT: INCONCLUSIVE"),
    ] {
        let fixture = PrepareFixture::new()?;
        let execution = registered_landed_writer_fixture(&fixture)?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&execution, &call)?;
        let mut run = input();
        if writer {
            run.request.task.writer = Some(fixture.writer.clone());
        }
        store.create(&run, &call)?;
        let id = run.attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let record = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let scope = RunStore::scope(&record);
        let attempt = store.attempt_dir(id, false)?;
        let mut launch = crate::prepare_writer(
            &fixture.writer,
            execution.state_root.as_path(),
            execution.project_id,
        )?;
        launch.snapshot(
            execution.state_root.as_path(),
            &attempt,
            scope,
            &provenant_records::policy::Policy::default(),
        )?;
        let candidate = store.temporary_path(id, &call)?.join("candidate-0");
        std::fs::create_dir_all(&candidate)?;
        let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
        crate::objects::prepare_bound(
            &git,
            launch.worktree.repo(),
            &attempt,
            &candidate,
            launch.branch.as_str(),
            launch.base,
        )?;
        let pin = crate::objects::import_pin(scope.attempt, launch.base);
        fixture.git(&["update-ref", &pin, &launch.base.to_string()])?;
        let (directory, binding) = launch.custody.take().ok_or("writer custody")?;
        let lease = provenant_supervisor::fixture::acquire_writer_with_binding(
            directory,
            scope,
            binding,
            call.lock_budget(),
            &cancel,
            &clock,
        )?;
        drop(provenant_supervisor::fixture::bind_writer_attempt(
            lease, &attempt,
        )?);
        let committed_head = {
            let objects = candidate.join("git-objects");
            let shared_objects = launch.worktree.repo().common_git_dir().join("objects");
            let private_git = |args: &[std::ffi::OsString]| {
                crate::objects::execute_bounded(
                    &git,
                    &candidate.join("git-common"),
                    &fixture.writer.worktree,
                    args,
                    None,
                    crate::objects::Execution {
                        object_directory: Some(&objects),
                        alternate_directory: Some(&shared_objects),
                        ..Default::default()
                    },
                )
            };
            let head = private_git(&[
                "-c".into(),
                "user.name=Fixture".into(),
                "-c".into(),
                "user.email=fixture@example.invalid".into(),
                "commit-tree".into(),
                format!("{}^{{tree}}", launch.base).into(),
                "-p".into(),
                launch.base.to_string().into(),
                "-m".into(),
                "Partial work".into(),
            ])?;
            let head = std::str::from_utf8(&head)?.trim();
            private_git(&[
                "update-ref".into(),
                launch.branch.as_str().into(),
                head.into(),
            ])?;
            head.to_owned()
        };
        let mut observed = crate::tests::exit();
        observed.scope = scope;
        crate::dispatch_tests::seal_exit(&store, &observed)?;
        let mut fake = crate::dispatch_tests::Fake::with_observed(Some(route()), Some(outcome));
        fake.answer = Some(answer.to_owned());
        let finished = store.finish(id, &attempt, &observed, &fake, &call)?;
        let AttemptState::Terminal {
            outcome: actual,
            reason,
            ..
        } = &finished.state
        else {
            panic!("not terminal")
        };
        if writer && outcome == Outcome::Succeeded {
            assert_eq!(*actual, Outcome::ReviewRequired, "{reason:?}");
            if answer.is_empty() {
                assert!(reason.as_deref().unwrap().contains(EMPTY_WRITER_REASON));
                let row = LaneRow::project(&finished, LaneProjectionFacts::default());
                assert!(row.reason.as_deref().unwrap().contains(EMPTY_WRITER_REASON));
                assert_eq!(
                    read_sealed(&attempt, "result.md", RESULT_LIMIT)?.as_deref(),
                    Some(EMPTY_WRITER_RESULT.as_bytes())
                );
            } else {
                assert!(reason.as_deref().unwrap().contains(NO_HANDOFF_REASON));
                assert_eq!(
                    finished.settlement_class,
                    Some(provenant_records::run::SettlementClass::HandoffMissing)
                );
                assert_eq!(
                    finished.signals,
                    [provenant_records::run::SettlementSignal::HandoffMissing]
                );
                let AttemptState::Terminal { stop, .. } = &finished.state else {
                    panic!("attempt must be terminal");
                };
                let stop = stop.as_ref().ok_or("stop")?;
                assert_eq!(stop.cause(), provenant_records::StopCause::HandoffMissing);
                assert_eq!(
                    stop.next_step(
                        &Default::default(),
                        provenant_records::StopFacts {
                            custody_released: true,
                            ..Default::default()
                        }
                    ),
                    provenant_records::NextStep::Chair
                );
                assert_eq!(
                    finished
                        .settlement_class
                        .unwrap()
                        .release(Some(stop.cause())),
                    provenant_records::run::DependantRelease::Hold
                );
                assert_eq!(
                    read_sealed(&attempt, "result.md", RESULT_LIMIT)?.as_deref(),
                    Some(answer.as_bytes())
                );
            }
            fixture.git(&[
                "merge-base",
                "--is-ancestor",
                &committed_head,
                launch.branch.as_str(),
            ])?;
            assert_ne!(committed_head, launch.base.to_string());
        } else {
            assert_eq!(*actual, Outcome::Failed);
        }
    }
    Ok(())
}

/// A writer that exits without a handoff and without a commit keeps its
/// uncommitted paths in the terminal reason: the empty-final-message cause
/// must not overwrite them (#3341). The branch head and the dirty file are
/// preserved for the chair, so the graph stays continuable in both the
/// empty and the non-empty final-message cases. A commit would move the
/// head and end the census, so this case needs a writer that never committed.
#[test]
fn no_handoff_writer_exit_keeps_dirty_paths_head_and_continuation()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    use crate::delivery_tests::{PrepareFixture, registered_landed_writer_fixture};
    use provenant_records::run::{SettlementClass, SettlementSignal};

    const DIRTY: &str = "left-uncommitted.txt";
    for (answer, empty) in [("", true), ("Work remains in the renderer.", false)] {
        let fixture = PrepareFixture::new()?;
        let execution = registered_landed_writer_fixture(&fixture)?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&execution, &call)?;
        let mut run = input();
        run.request.task.writer = Some(fixture.writer.clone());
        store.create(&run, &call)?;
        let id = run.attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let record = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let scope = RunStore::scope(&record);
        let attempt = store.attempt_dir(id, false)?;
        let mut launch = crate::prepare_writer(
            &fixture.writer,
            execution.state_root.as_path(),
            execution.project_id,
        )?;
        launch.snapshot(
            execution.state_root.as_path(),
            &attempt,
            scope,
            &provenant_records::policy::Policy::default(),
        )?;
        let candidate = store.temporary_path(id, &call)?.join("candidate-0");
        std::fs::create_dir_all(&candidate)?;
        let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
        crate::objects::prepare_bound(
            &git,
            launch.worktree.repo(),
            &attempt,
            &candidate,
            launch.branch.as_str(),
            launch.base,
        )?;
        let pin = format!("refs/provenant/import/{}", launch.base);
        fixture.git(&["update-ref", &pin, &launch.base.to_string()])?;
        let (directory, binding) = launch.custody.take().ok_or("writer custody")?;
        let lease = provenant_supervisor::fixture::acquire_writer_with_binding(
            directory,
            scope,
            binding,
            call.lock_budget(),
            &cancel,
            &clock,
        )?;
        drop(provenant_supervisor::fixture::bind_writer_attempt(
            lease, &attempt,
        )?);
        // The writer exited mid-build with an edit it never committed and
        // no commit: the branch head stays at its base for the chair.
        std::fs::write(fixture.writer.worktree.join(DIRTY), "uncommitted edit\n")?;

        let mut observed = crate::tests::exit();
        observed.scope = scope;
        crate::dispatch_tests::seal_exit(&store, &observed)?;
        let mut fake =
            crate::dispatch_tests::Fake::with_observed(Some(route()), Some(Outcome::Succeeded));
        fake.answer = Some(answer.to_owned());
        let finished = store.finish(id, &attempt, &observed, &fake, &call)?;

        let AttemptState::Terminal {
            outcome,
            reason,
            stop,
            ..
        } = &finished.state
        else {
            return Err("not terminal".into());
        };
        // Uncommitted work is never delivered or reported as a success; the
        // missing handoff or empty message does not hide that outcome.
        assert_eq!(*outcome, Outcome::Uncommitted, "{reason:?}");
        let reason = reason.as_deref().ok_or("terminal reason")?;
        assert!(
            reason.contains(DIRTY),
            "uncommitted path lost from reason: {reason}"
        );
        assert!(
            reason.contains("uncommitted"),
            "uncommitted cause lost from reason: {reason}"
        );
        let terminal: crate::writer::WriterTerminal = crate::db::decode(
            &read_sealed(&attempt, "writer-terminal.json", RECORD_LIMIT)?
                .ok_or("writer terminal not sealed")?,
        )?;
        assert_eq!(terminal.head, launch.base, "recorded head");
        assert_eq!(
            finished.signals,
            [
                SettlementSignal::HandoffMissing,
                SettlementSignal::UncommittedWork
            ]
        );
        let stop = stop.as_ref().ok_or("typed stop")?;
        let next_after = |retries| {
            stop.next_step(
                &Default::default(),
                provenant_records::StopFacts {
                    retries,
                    custody_released: true,
                },
            )
        };
        let next = next_after(0);
        if empty {
            assert!(reason.contains(EMPTY_WRITER_REASON), "{reason}");
            assert_eq!(
                finished.settlement_class,
                Some(SettlementClass::EmptyResult)
            );
            assert_eq!(stop.cause(), provenant_records::StopCause::EmptyResult);
            // run.md "What follows a stop": `empty_result` allows one fresh
            // same-route attempt, then moves to a new route.
            assert_eq!(
                next,
                provenant_records::NextStep::SameRoute {
                    continuation: provenant_records::Continuation::Fresh
                },
                "{reason}"
            );
            assert_eq!(next_after(1), provenant_records::NextStep::NewRoute);
        } else {
            assert!(
                reason.contains("handoff_missing"),
                "missing handoff not named: {reason}"
            );
            assert_eq!(
                finished.settlement_class,
                Some(SettlementClass::HandoffMissing)
            );
            assert_eq!(stop.cause(), provenant_records::StopCause::HandoffMissing);
            assert_eq!(next, provenant_records::NextStep::Chair, "{reason}");
        }
        assert_eq!(
            std::fs::read_to_string(fixture.writer.worktree.join(DIRTY))?,
            "uncommitted edit\n",
            "dirty work must be preserved"
        );
    }
    Ok(())
}

#[test]
fn writer_token_budget_stop_commits_uncommitted_work_and_publishes_branch()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    for case in [
        BudgetStopCase::Commit,
        BudgetStopCase::Native { dirty: false },
        BudgetStopCase::Native { dirty: true },
    ] {
        budget_stop_case(case)?;
    }
    Ok(())
}

#[test]
fn writer_token_budget_commit_failure_preserves_timeout()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    budget_stop_case(BudgetStopCase::CommitFailure)
}

#[test]
fn writer_token_budget_commit_requires_absent_custody()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    budget_stop_case(BudgetStopCase::CustodyPresent)
}

#[test]
fn writer_token_budget_commit_ignores_lane_configured_clean_filters()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    budget_stop_case(BudgetStopCase::HostileFilter)
}

#[test]
fn writer_token_budget_commit_refuses_symlinked_object_fanout()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    budget_stop_case(BudgetStopCase::SymlinkObjects)
}

#[test]
fn writer_token_budget_commit_leaves_a_cherry_pick_uncommitted()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    budget_stop_case(BudgetStopCase::InProgress("cherry-pick"))
}

#[test]
fn writer_token_budget_commit_leaves_a_rebase_uncommitted()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    budget_stop_case(BudgetStopCase::InProgress("rebase"))
}

#[test]
fn writer_token_budget_commit_leaves_a_revert_uncommitted()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    budget_stop_case(BudgetStopCase::InProgress("revert"))
}

#[test]
fn writer_token_budget_commit_leaves_an_apply_rebase_uncommitted()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    budget_stop_case(BudgetStopCase::InProgress("rebase-apply"))
}

/// A writer the host stopped at its context handoff threshold, with an
/// edit left uncommitted and a progress handoff recorded, settles as a
/// declared stop whose result carries that handoff. The graph restarts
/// such a stop fresh on the same worktree with the handoff in its brief
/// (`no_work`); the host commits nothing and the edit stays in place.
#[test]
fn writer_context_handoff_stop_settles_as_a_declared_stop_with_its_handoff()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    use crate::delivery_tests::{PrepareFixture, registered_landed_writer_fixture};
    let fixture = PrepareFixture::new()?;
    let execution = registered_landed_writer_fixture(&fixture)?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&execution, &call)?;
    let mut run = input();
    run.request.task.writer = Some(fixture.writer.clone());
    run.request.task.profile.write_scope = vec!["src".into()];
    store.create(&run, &call)?;
    let id = run.attempt_id;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let record = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let scope = RunStore::scope(&record);
    let attempt = store.attempt_dir(id, false)?;
    let mut launch = crate::prepare_writer(
        &fixture.writer,
        execution.state_root.as_path(),
        execution.project_id,
    )?;
    launch.snapshot(
        execution.state_root.as_path(),
        &attempt,
        scope,
        &provenant_records::policy::Policy::default(),
    )?;
    let candidate = store.temporary_path(id, &call)?.join("candidate-0");
    std::fs::create_dir_all(&candidate)?;
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    crate::objects::prepare_bound(
        &git,
        launch.worktree.repo(),
        &attempt,
        &candidate,
        launch.branch.as_str(),
        launch.base,
    )?;
    let pin = crate::objects::import_pin(scope.attempt, launch.base);
    fixture.git(&["update-ref", &pin, &launch.base.to_string()])?;
    let (directory, binding) = launch.custody.take().ok_or("writer custody")?;
    let lease = provenant_supervisor::fixture::acquire_writer_with_binding(
        directory.try_clone()?,
        scope,
        binding,
        call.lock_budget(),
        &cancel,
        &clock,
    )?;
    drop(provenant_supervisor::fixture::bind_writer_attempt(
        lease, &attempt,
    )?);
    std::fs::write(
        launch.worktree.repo().root_path().join("context-work.txt"),
        "uncommitted work before the context handoff\n",
    )?;
    // The lane's latest progress handoff, as settlement snapshots it.
    let progress = serde_json::json!({
        "schema_version": 2,
        "status": "stopped_early",
        "note": "Parser done; renderer next",
        "remaining": ["Finish renderer"],
        "decisions": ["Keep parser API"],
        "verify": [],
    });
    seal_progress_handoff(&attempt, &progress)?;
    let mut observed = crate::tests::exit();
    observed.scope = scope;
    observed.stop_reason = Some(StopReason::ContextHandoff);
    crate::dispatch_tests::seal_exit(&store, &observed)?;
    crate::artifacts::seal(
        &attempt,
        "launch.json",
        &crate::db::bytes(
            &serde_json::json!({"schema_version":1,"scope":scope,"requester":execution.actor_id}),
        )?,
        RECORD_LIMIT,
    )?;
    // A stopped provider leaves no final answer.
    let fake = crate::dispatch_tests::Fake::with_observed(Some(route()), Some(Outcome::Failed));
    let finished = store.finish(id, &attempt, &observed, &fake, &call)?;
    let AttemptState::Terminal {
        cause,
        outcome,
        reason,
        ..
    } = &finished.state
    else {
        panic!("attempt must be terminal");
    };
    assert_eq!(*outcome, Outcome::ReviewRequired, "reason: {reason:?}");
    assert_eq!(*cause, Some(CauseKind::ContextHandoff));
    // Only a declared stop: the owner maps this to the graph's restart.
    assert_eq!(
        finished.signals,
        vec![provenant_records::run::SettlementSignal::DeclaredStop]
    );
    let handoff = store.handoff(&finished)??.ok_or("sealed handoff")?;
    assert_eq!(handoff.note, "Parser done; renderer next");
    assert_eq!(handoff.remaining, vec!["Finish renderer".to_owned()]);
    assert!(handoff.unfinished());
    let Some(provenant_records::handoff::WriteScopeReport::Full {
        code,
        strength,
        out_of_scope_paths: paths,
        ..
    }) = handoff.write_scope.as_ref()
    else {
        return Err("typed full scope report".into());
    };
    assert_eq!(code, "WRITE_SCOPE");
    assert_eq!(strength, provenant_records::policy::Strength::Warn);
    assert!(
        paths
            .iter()
            .any(|path| path.as_path_text() == Some("context-work.txt"))
    );
    // Nothing was committed for the lane; its edit stays for the restart.
    assert!(read_sealed(&attempt, "budget-commit.json", RECORD_LIMIT)?.is_none());
    assert_eq!(
        std::fs::read_to_string(fixture.writer.worktree.join("context-work.txt"))?,
        "uncommitted work before the context handoff\n"
    );
    Ok(())
}

#[test]
fn strict_write_scope_keeps_outside_budget_edits_uncommitted()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    budget_stop_case(BudgetStopCase::StrictScope)
}

/// A Claude writer whose launch-time token copy was lost mid-run (the
/// host renewed the account login) settles as a declared stop on its
/// latest handoff; its uncommitted edit stays, and the reason stays the
/// lane-token one, not a login.
#[test]
fn writer_that_lost_its_lane_token_settles_on_its_handoff()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    for host_outcome in [Outcome::AuthRequired, Outcome::Lost, Outcome::Unsettled] {
        use crate::delivery_tests::{PrepareFixture, registered_landed_writer_fixture};
        let fixture = PrepareFixture::new()?;
        let execution = registered_landed_writer_fixture(&fixture)?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&execution, &call)?;
        let mut run = input();
        run.request.task.writer = Some(fixture.writer.clone());
        store.create(&run, &call)?;
        let id = run.attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let record = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let scope = RunStore::scope(&record);
        let attempt = store.attempt_dir(id, false)?;
        let mut launch = crate::prepare_writer(
            &fixture.writer,
            execution.state_root.as_path(),
            execution.project_id,
        )?;
        launch.snapshot(
            execution.state_root.as_path(),
            &attempt,
            scope,
            &provenant_records::policy::Policy::default(),
        )?;
        let candidate = store.temporary_path(id, &call)?.join("candidate-0");
        std::fs::create_dir_all(&candidate)?;
        let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
        crate::objects::prepare_bound(
            &git,
            launch.worktree.repo(),
            &attempt,
            &candidate,
            launch.branch.as_str(),
            launch.base,
        )?;
        let pin = crate::objects::import_pin(scope.attempt, launch.base);
        fixture.git(&["update-ref", &pin, &launch.base.to_string()])?;
        let (directory, binding) = launch.custody.take().ok_or("writer custody")?;
        let lease = provenant_supervisor::fixture::acquire_writer_with_binding(
            directory.try_clone()?,
            scope,
            binding,
            call.lock_budget(),
            &cancel,
            &clock,
        )?;
        drop(provenant_supervisor::fixture::bind_writer_attempt(
            lease, &attempt,
        )?);
        std::fs::write(
            launch.worktree.repo().root_path().join("token-work.txt"),
            "uncommitted work before the token was lost\n",
        )?;
        let progress = serde_json::json!({
            "schema_version": 2,
            "status": "stopped_early",
            "note": "Parser done; renderer next",
            "remaining": ["Finish renderer"],
            "decisions": [],
            "verify": [],
        });
        seal_progress_handoff(&attempt, &progress)?;
        let mut observed = crate::tests::exit();
        observed.scope = scope;
        crate::dispatch_tests::seal_exit(&store, &observed)?;
        crate::artifacts::seal(
            &attempt,
            "launch.json",
            &crate::db::bytes(
                &serde_json::json!({"schema_version":1,"scope":scope,"requester":execution.actor_id}),
            )?,
            RECORD_LIMIT,
        )?;
        // Exercise the host's uncertain-cancellation transition: an
        // unsolicited provider cancellation becomes Unsettled with its
        // required custody diagnostic, rather than an invalid bare outcome.
        let provider_outcome = if host_outcome == Outcome::Unsettled {
            Outcome::Cancelled
        } else {
            host_outcome
        };
        let mut fake =
            crate::dispatch_tests::Fake::with_observed(Some(route()), Some(provider_outcome));
        fake.cause = Some(CauseKind::LaneTokenLost);
        let finished = store.finish(id, &attempt, &observed, &fake, &call)?;
        let AttemptState::Terminal {
            cause,
            stop,
            outcome,
            reason,
            ..
        } = &finished.state
        else {
            panic!("attempt must be terminal");
        };
        let expected_outcome = if host_outcome == Outcome::AuthRequired {
            Outcome::ReviewRequired
        } else {
            host_outcome
        };
        assert_eq!(*outcome, expected_outcome, "reason: {reason:?}");
        if host_outcome == Outcome::AuthRequired {
            assert_eq!(*cause, Some(CauseKind::LaneTokenLost));
        }
        let stop = stop.as_ref().ok_or("stop")?;
        let expected_stop = match host_outcome {
            Outcome::Lost => provenant_records::StopCause::Lost,
            Outcome::Unsettled => provenant_records::StopCause::Unsettled,
            _ => provenant_records::StopCause::LaneTokenLost,
        };
        assert_eq!(stop.cause(), expected_stop);
        let handoff = stop.handoff().ok_or("sealed stop handoff")?;
        assert_eq!(handoff.note, "Parser done; renderer next");
        assert_eq!(
            std::fs::read_to_string(fixture.writer.worktree.join("token-work.txt"))?,
            "uncommitted work before the token was lost\n"
        );
    }
    Ok(())
}

#[test]
fn a_context_handoff_keeps_an_unfinished_handoff_and_never_reports_done() {
    use provenant_records::handoff::{Handoff, HandoffStatus};
    let missing = context_handoff(None);
    assert_eq!(missing.status, HandoffStatus::StoppedEarly);
    assert!(missing.note.contains("context handoff threshold"));
    let done = Handoff {
        status: HandoffStatus::Done,
        note: "claimed done".into(),
        ..missing.clone()
    };
    let kept = context_handoff(Some(done));
    assert_eq!(kept.status, HandoffStatus::StoppedEarly);
    assert_eq!(kept.note, "claimed done");
}

#[test]
fn budget_wrap_up_resumes_once_with_remaining_grace_and_natural_completion_wins()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    budget_stop_case(BudgetStopCase::WrapUp)?;
    budget_stop_case(BudgetStopCase::Natural)?;
    budget_stop_case(BudgetStopCase::Clean)?;
    budget_stop_case(BudgetStopCase::WrapUpRefused)?;
    budget_stop_case(BudgetStopCase::WrapUpBlocked)?;
    budget_stop_case(BudgetStopCase::NativeStaged)
}

#[derive(Clone, Copy)]
enum BudgetStopCase {
    /// An operation Git left in progress.
    InProgress(&'static str),
    Commit,
    WrapUp,
    WrapUpRefused,
    WrapUpBlocked,
    NativeStaged,
    Natural,
    Clean,
    Native {
        dirty: bool,
    },
    CommitFailure,
    CustodyPresent,
    HostileFilter,
    StrictScope,
    SymlinkObjects,
}

fn budget_stop_case(case: BudgetStopCase) -> std::result::Result<(), Box<dyn std::error::Error>> {
    use crate::delivery_tests::{PrepareFixture, registered_landed_writer_fixture};
    let fixture = PrepareFixture::new()?;
    let execution = registered_landed_writer_fixture(&fixture)?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&execution, &call)?;
    let mut run = input();
    run.request.task.writer = Some(fixture.writer.clone());
    if matches!(case, BudgetStopCase::HostileFilter) {
        run.request.task.profile.write_scope = vec![".".into()];
    }
    if matches!(case, BudgetStopCase::StrictScope) {
        run.request.task.profile.write_scope = vec!["src".into()];
        run.request.task.profile.policy_overrides.insert(
            "WRITE_SCOPE".into(),
            provenant_records::policy::Strength::Refuse,
        );
    }
    store.create(&run, &call)?;
    let id = run.attempt_id;
    let limits = if matches!(
        case,
        BudgetStopCase::WrapUp
            | BudgetStopCase::WrapUpRefused
            | BudgetStopCase::WrapUpBlocked
            | BudgetStopCase::Natural
            | BudgetStopCase::Clean
    ) {
        provenant_records::LaneLimits {
            token_budget: Some(provenant_records::Limit::new(
                1_000_000,
                provenant_records::Layer::Default,
                provenant_records::LimitStrength::Default,
            )),
            budget_policy: Some(provenant_records::BudgetPolicy::default()),
            ..Default::default()
        }
    } else {
        Default::default()
    };
    let mut applied_route = route();
    if limits.budget_policy.is_some() {
        applied_route.provider = Some("codex".into());
    }
    store.mark_starting_with_priority(
        &store.record(id, &call)?,
        &applied_route,
        &[],
        &Default::default(),
        None,
        None,
        &limits,
        &call,
    )?;
    let record = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let scope = RunStore::scope(&record);
    let attempt = store.attempt_dir(id, false)?;
    let mut launch = crate::prepare_writer(
        &fixture.writer,
        execution.state_root.as_path(),
        execution.project_id,
    )?;
    launch.snapshot(
        execution.state_root.as_path(),
        &attempt,
        scope,
        &provenant_records::policy::Policy::default(),
    )?;
    let candidate = store.temporary_path(id, &call)?.join("candidate-0");
    std::fs::create_dir_all(&candidate)?;
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    if matches!(case, BudgetStopCase::NativeStaged) {
        let branch = launch
            .branch
            .as_str()
            .strip_prefix("refs/heads/")
            .ok_or("local branch")?;
        fixture.lane_git(&["push", "--set-upstream", "origin", branch])?;
    }
    crate::objects::prepare_bound(
        &git,
        launch.worktree.repo(),
        &attempt,
        &candidate,
        launch.branch.as_str(),
        launch.base,
    )?;
    let pin = crate::objects::import_pin(scope.attempt, launch.base);
    fixture.git(&["update-ref", &pin, &launch.base.to_string()])?;
    let (directory, binding) = launch.custody.take().ok_or("writer custody")?;
    let lease = provenant_supervisor::fixture::acquire_writer_with_binding(
        directory.try_clone()?,
        scope,
        binding,
        call.lock_budget(),
        &cancel,
        &clock,
    )?;
    drop(provenant_supervisor::fixture::bind_writer_attempt(
        lease, &attempt,
    )?);

    // Leave an uncommitted file in the lane's worktree.
    std::fs::write(
        launch.worktree.repo().root_path().join("budget-work.txt"),
        "uncommitted work before budget stop\n",
    )?;

    if matches!(case, BudgetStopCase::NativeStaged) {
        // Match the launcher's private upstream configuration so a false
        // clean status cannot be hidden by a missing-upstream error.
        let source = crate::objects::bound_source(&attempt)?.ok_or("private source")?;
        let refs = source.refs.current_path()?;
        for (key, value) in [
            (
                "remote.origin.fetch".to_owned(),
                "+refs/heads/*:refs/remotes/origin/*".to_owned(),
            ),
            (
                format!(
                    "branch.{}.remote",
                    launch
                        .branch
                        .as_str()
                        .strip_prefix("refs/heads/")
                        .ok_or("local branch")?
                ),
                "origin".to_owned(),
            ),
            (
                format!(
                    "branch.{}.merge",
                    launch
                        .branch
                        .as_str()
                        .strip_prefix("refs/heads/")
                        .ok_or("local branch")?
                ),
                launch.branch.as_str().to_owned(),
            ),
        ] {
            crate::objects::command(
                &git,
                &refs,
                launch.worktree.repo().root_path(),
                &["config".into(), key.into(), value.into()],
                None,
            )?;
        }
        let objects = source.objects.current_path()?;
        let alternate = launch.worktree.repo().common_git_dir().join("objects");
        crate::objects::execute_bounded(
            &git,
            &refs,
            launch.worktree.repo().root_path(),
            &[
                "update-ref".into(),
                format!(
                    "refs/remotes/origin/{}",
                    launch
                        .branch
                        .as_str()
                        .strip_prefix("refs/heads/")
                        .ok_or("local branch")?
                )
                .into(),
                launch.base.to_string().into(),
            ],
            None,
            crate::objects::Execution {
                object_directory: Some(&objects),
                alternate_directory: Some(&alternate),
                ..Default::default()
            },
        )?;
        // Stage in the attempt's private index: it, not the host's native
        // index, is the writer's staging area.
        crate::delivery_tests::private_git(
            &fixture,
            &candidate,
            &git,
            &["add", "budget-work.txt"],
        )?;
        std::fs::remove_file(fixture.writer.worktree.join("budget-work.txt"))?;
        assert!(
            !launch.budget_clean_point(&attempt)?,
            "native staged edits are dirty even before HEAD advances"
        );
        return Ok(());
    }

    let native_head = if let Some(dirty) = match case {
        BudgetStopCase::Native { dirty } => Some(dirty),
        BudgetStopCase::Clean => Some(false),
        _ => None,
    } {
        // The lane commits in its private Git, as a native session does.
        let native =
            provenant_os::git::resolve_git_executable(None).ok_or("native Git unavailable")?;
        let private =
            |args: &[&str]| crate::delivery_tests::private_git(&fixture, &candidate, &native, args);
        private(&["add", "budget-work.txt"])?;
        private(&["commit", "-m", "Native lane work"])?;
        let head = crate::objects::private_tip(
            &crate::objects::bound_source(&attempt)?.ok_or("private source")?,
            launch.branch.as_str(),
        )?;
        if !dirty {
            // The clean-point contract uses locally recorded push evidence.
            // A real local bare remote exercises native Git's tracking ref.
            let branch = launch
                .branch
                .as_str()
                .strip_prefix("refs/heads/")
                .ok_or("local branch")?;
            private(&["push", "--set-upstream", "origin", branch])?;
        }
        if dirty {
            std::fs::write(
                fixture.writer.worktree.join("budget-work.txt"),
                "additional work before budget stop\n",
            )?;
        }
        Some(head)
    } else {
        None
    };

    if native_head.is_some() {
        if matches!(
            case,
            BudgetStopCase::Clean | BudgetStopCase::Native { dirty: false }
        ) {
            assert!(launch.budget_clean_point(&attempt)?);
        } else {
            assert!(!launch.budget_clean_point(&attempt)?);
        }
    }

    let filter_sentinel = fixture
        .writer
        .project
        .parent()
        .ok_or("fixture root")?
        .join("filter-ran");
    if matches!(case, BudgetStopCase::HostileFilter) {
        let config = candidate.join("git-common/config");
        let mut bytes = std::fs::read_to_string(&config)?;
        bytes.push_str(&format!(
            "\n[filter \"poison\"]\n\tclean = touch {}; printf altered\n\tprocess = touch {}; exit 1\n\trequired = true\n",
            filter_sentinel.display(), filter_sentinel.display(),
        ));
        std::fs::write(config, bytes)?;
        std::fs::write(
            fixture.writer.worktree.join(".gitattributes"),
            "budget-work.txt filter=poison\n",
        )?;
    }
    if matches!(case, BudgetStopCase::StrictScope) {
        let recovery =
            custody::acquire_recovery(directory, scope, call.lock_budget(), &cancel, &clock)?;
        assert_eq!(recovery.observe(), custody::RecoveryState::Absent);
        let mut settlement =
            crate::settlement::Guard::claim(&attempt, scope, &clock)?.ok_or("settlement claim")?;
        let result = launch.terminal(
            execution.state_root.as_path(),
            &attempt,
            &recovery,
            &mut settlement,
            crate::writer::TerminalPolicy {
                enforcement: &store.resume_policy(id, &call)?,
                history: provenant_config::LaneHistory::default(),
                budget_stop: true,
                preservation_stop: None,
                work: None,
            },
        );
        assert!(result.is_err());
        let report: serde_json::Value = serde_json::from_slice(
            &read_sealed(&attempt, "policy-write-scope.json", RECORD_LIMIT)?
                .ok_or_else(|| format!("write scope report missing after {result:?}"))?,
        )?;
        assert_eq!(report["code"], "WRITE_SCOPE");
        assert_eq!(report["strength"], "refuse");
        // The strict scope comes from the request's sealed dispatch
        // override, not a project rule.
        assert_eq!(report["source"], "dispatch");
        assert!(
            !report["out_of_scope_paths"]
                .as_array()
                .ok_or("paths")?
                .is_empty()
        );
        assert_eq!(
            crate::objects::resolve_ref(&git, launch.worktree.repo(), launch.branch.as_str())?,
            launch.base
        );
        let source = crate::objects::bound_source(&attempt)?.ok_or("private source")?;
        let private_tip = crate::objects::private_tip(&source, launch.branch.as_str())?;
        // Scoped budget commits must not capture edits outside the claim.
        // Strict scope still refuses settlement and preserves those edits.
        assert_eq!(private_tip, launch.base);
        let disposition: serde_json::Value = serde_json::from_slice(
            &read_sealed(&attempt, "budget-commit.json", RECORD_LIMIT)?
                .ok_or("budget commit disposition")?,
        )?;
        assert!(disposition.get("commit").is_none());
        assert!(
            disposition["note"]
                .as_str()
                .is_some_and(|note| note.contains("outside declared paths"))
        );
        assert!(
            report["out_of_scope_paths"]
                .to_string()
                .contains("budget-work.txt")
        );
        assert_eq!(
            std::fs::read_to_string(fixture.writer.worktree.join("budget-work.txt"))?,
            "uncommitted work before budget stop\n"
        );
        return Ok(());
    }
    if matches!(case, BudgetStopCase::SymlinkObjects) {
        let outside = fixture
            .writer
            .project
            .parent()
            .ok_or("fixture root")?
            .join("outside-objects");
        std::fs::create_dir(&outside)?;
        // Any new blob or tree object must encounter a poisoned fanout.
        for byte in 0..=255u8 {
            std::os::unix::fs::symlink(
                &outside,
                candidate.join("git-objects").join(format!("{byte:02x}")),
            )?;
        }
        let recovery =
            custody::acquire_recovery(directory, scope, call.lock_budget(), &cancel, &clock)?;
        assert_eq!(recovery.observe(), custody::RecoveryState::Absent);
        let mut settlement =
            crate::settlement::Guard::claim(&attempt, scope, &clock)?.ok_or("settlement claim")?;
        let private_index = std::fs::read(candidate.join("git-common/index"))?;
        let source = crate::objects::bound_source(&attempt)?.ok_or("private source")?;
        let private_head = crate::objects::private_tip(&source, launch.branch.as_str())?;
        let result = launch.terminal(
            execution.state_root.as_path(),
            &attempt,
            &recovery,
            &mut settlement,
            crate::writer::TerminalPolicy {
                enforcement: &store.resume_policy(id, &call)?,
                history: provenant_config::LaneHistory::default(),
                budget_stop: true,
                preservation_stop: None,
                work: None,
            },
        );
        assert!(result.is_err(), "unsafe object store must remain unsettled");
        let disposition: serde_json::Value = serde_json::from_slice(
            &read_sealed(&attempt, "budget-commit.json", RECORD_LIMIT)?
                .ok_or("budget commit disposition")?,
        )?;
        let note = disposition["note"].as_str().ok_or("commit note")?;
        assert!(
            note.contains("budget commit failed:") && note.contains("symlink"),
            "{note}"
        );
        assert_eq!(std::fs::read_dir(&outside)?.count(), 0);
        assert_eq!(
            std::fs::read(candidate.join("git-common/index"))?,
            private_index
        );
        assert_eq!(
            crate::objects::private_tip(&source, launch.branch.as_str())?,
            private_head
        );
        assert_eq!(
            crate::objects::resolve_ref(&git, launch.worktree.repo(), launch.branch.as_str())?,
            launch.base
        );
        return Ok(());
    }
    if matches!(case, BudgetStopCase::CustodyPresent) {
        use provenant_supervisor::census::{ProcessObservation, current_process};
        let ProcessObservation::Present(mut guardian) = current_process() else {
            return Err("current process must be observable".into());
        };
        // Model a reservation whose guardian identity cannot be proved
        // quiescent. Native group/session mismatch makes it unknown.
        guardian.identity.group_id = guardian.identity.pid;
        guardian.identity.session_id = guardian.identity.pid;
        let path = directory
            .current_path()?
            .join(id.to_string())
            .join("reservation.json");
        let mut reservation: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        reservation["launch_state"] = "spawned".into();
        reservation["unresolved_cleanup"] = true.into();
        reservation["boot"] = guardian.identity.boot.clone().into();
        reservation["guardian"] = serde_json::to_value(&guardian.identity)?;
        reservation["provider"] = serde_json::to_value(&guardian.identity)?;
        std::fs::write(&path, serde_json::to_vec(&reservation)?)?;
        let recovery =
            custody::acquire_recovery(directory, scope, call.lock_budget(), &cancel, &clock)?;
        assert_ne!(recovery.observe(), custody::RecoveryState::Absent);
        let mut settlement =
            crate::settlement::Guard::claim(&attempt, scope, &clock)?.ok_or("settlement claim")?;
        let private_index = std::fs::read(candidate.join("git-common/index"))?;
        let private_head = crate::objects::private_tip(
            &crate::objects::bound_source(&attempt)?.ok_or("private source")?,
            launch.branch.as_str(),
        )?;
        let result = launch.terminal(
            execution.state_root.as_path(),
            &attempt,
            &recovery,
            &mut settlement,
            crate::writer::TerminalPolicy {
                enforcement: &store.resume_policy(id, &call)?,
                history: provenant_config::LaneHistory::default(),
                budget_stop: true,
                preservation_stop: None,
                work: None,
            },
        );
        assert!(matches!(
            result,
            Err(RunError::Validation(
                "writer object import requires absent custody"
            ))
        ));
        assert_eq!(
            std::fs::read(candidate.join("git-common/index"))?,
            private_index
        );
        assert_eq!(
            crate::objects::private_tip(
                &crate::objects::bound_source(&attempt)?.ok_or("private source")?,
                launch.branch.as_str()
            )?,
            private_head
        );
        assert_eq!(
            crate::objects::resolve_ref(&git, launch.worktree.repo(), launch.branch.as_str())?,
            launch.base
        );
        assert!(read_sealed(&attempt, "budget-commit.json", RECORD_LIMIT)?.is_none());
        return Ok(());
    }
    if matches!(case, BudgetStopCase::CommitFailure) {
        std::fs::write(
            candidate.join("git-common/index.lock"),
            b"existing Git lock",
        )?;
    }
    if let BudgetStopCase::InProgress(operation) = case {
        let git_dir = candidate.join("git-common");
        match operation {
            "rebase" => std::fs::create_dir(git_dir.join("rebase-merge"))?,
            "rebase-apply" => std::fs::create_dir(git_dir.join("rebase-apply"))?,
            operation => std::fs::write(
                git_dir.join(if operation == "revert" {
                    "REVERT_HEAD"
                } else {
                    "CHERRY_PICK_HEAD"
                }),
                format!("{}\n", launch.base),
            )?,
        }
    }
    let mut observed = crate::tests::exit();
    observed.scope = scope;
    observed.stop_reason = Some(StopReason::TokenBudget);
    if matches!(case, BudgetStopCase::Clean) {
        crate::artifacts::seal(
            &attempt,
            provenant_records::AttemptFile::BudgetCleanPoint.name(),
            b"{\"schema_version\":1,\"clean\":true}",
            RECORD_LIMIT,
        )?;
    }
    crate::dispatch_tests::seal_exit(&store, &observed)?;
    crate::artifacts::seal(
        &attempt,
        "launch.json",
        &crate::db::bytes(
            &serde_json::json!({"schema_version":1,"scope":scope,"requester":execution.actor_id}),
        )?,
        RECORD_LIMIT,
    )?;
    let mut fake = crate::dispatch_tests::Fake::with_observed(
        Some(applied_route.clone()),
        Some(if matches!(case, BudgetStopCase::Natural) {
            Outcome::Succeeded
        } else {
            Outcome::TimedOut
        }),
    );
    if matches!(case, BudgetStopCase::WrapUpBlocked) {
        let mut blocked = make_host_handoff(None, "Owner decision required before continuing.");
        blocked.status = provenant_records::handoff::HandoffStatus::Blocked;
        fake.answer = Some(blocked.fenced());
    }
    if matches!(case, BudgetStopCase::Natural) {
        fake.answer = Some("Completed the accepted work and pushed its branch.".into());
    }
    if matches!(
        case,
        BudgetStopCase::WrapUp
            | BudgetStopCase::WrapUpRefused
            | BudgetStopCase::WrapUpBlocked
            | BudgetStopCase::Natural
            | BudgetStopCase::Clean
    ) {
        fake.codex_usage = Some(UsageRecord {
            per_attempt: Some(true),
            tokens: Tokens {
                input: Some(1_000_001),
                ..Tokens::default()
            },
            ..Default::default()
        });
    }
    if matches!(case, BudgetStopCase::WrapUpRefused) {
        crate::artifacts::seal(&attempt, "provider-state-expired.json", b"{}", RECORD_LIMIT)?;
    }
    let finished = store.finish(id, &attempt, &observed, &fake, &call)?;
    if matches!(
        case,
        BudgetStopCase::WrapUp
            | BudgetStopCase::WrapUpRefused
            | BudgetStopCase::WrapUpBlocked
            | BudgetStopCase::Natural
            | BudgetStopCase::Clean
    ) {
        assert!(read_sealed(&attempt, "budget-commit.json", RECORD_LIMIT)?.is_none());
        assert!(
            std::fs::read_to_string(fixture.writer.worktree.join("budget-work.txt"))?
                .contains("uncommitted work")
        );
        if matches!(case, BudgetStopCase::Natural | BudgetStopCase::Clean) {
            assert!(!matches!(
                finished.state,
                AttemptState::Terminal {
                    outcome: Outcome::TimedOut,
                    ..
                }
            ));
            assert!(store.prepare_budget_wrap_up(id, &call)?.is_none());
        } else if matches!(case, BudgetStopCase::WrapUpBlocked) {
            assert!(store.prepare_budget_wrap_up(id, &call)?.is_none());
            assert!(finished.superseded_by.is_none());
            let AttemptState::Terminal {
                terminal_note,
                stop,
                ..
            } = &finished.state
            else {
                panic!("not terminal")
            };
            let stop = stop.as_ref().ok_or("stop")?;
            assert_eq!(stop.cause(), provenant_records::StopCause::Blocked);
            assert_eq!(
                terminal_note.as_deref(),
                Some("Owner decision required before continuing.")
            );
            assert_eq!(
                stop.handoff().ok_or("blocked handoff")?.status,
                provenant_records::handoff::HandoffStatus::Blocked
            );
        } else if matches!(case, BudgetStopCase::WrapUpRefused) {
            assert!(store.prepare_budget_wrap_up(id, &call)?.is_none());
            assert!(
                read_sealed(
                    &attempt,
                    provenant_records::AttemptFile::BudgetWrapUpRefused.name(),
                    RECORD_LIMIT
                )?
                .is_some()
            );
            assert!(store.custody_released(&store.record(id, &call)?, &call)?);
        } else {
            assert!(matches!(
                finished.state,
                AttemptState::Terminal {
                    outcome: Outcome::ReviewRequired,
                    ..
                }
            ));
            let successor = store
                .prepare_budget_wrap_up(id, &call)?
                .ok_or("wrap-up successor")?;
            assert_eq!(store.prepare_budget_wrap_up(id, &call)?, Some(successor));
            let request = store.request(successor, &call)?;
            assert_eq!(request.parent, Some(id));
            assert!(
                request.fresh_continuation.is_none(),
                "wrap-up must resume the provider session"
            );
            let task = &request.task;
            let (budget, policy) = crate::writer_budget_limits(
                &provenant_confinement::run_config::RunConfig::default(),
                "p",
                &task.profile,
                task.context.as_ref(),
                None,
            );
            assert_eq!(budget.ok_or("remaining grace")?.value, 99_999);
            assert_eq!(policy.action, provenant_records::BudgetAction::Stop);
            assert!(store.record(id, &call)?.superseded_by.is_some());
        }
        return Ok(());
    }
    let AttemptState::Terminal {
        cause,
        outcome,
        reason,
        ..
    } = finished.state
    else {
        panic!("attempt must be terminal");
    };
    assert_eq!(outcome, Outcome::TimedOut, "reason: {reason:?}");
    assert_eq!(cause, Some(CauseKind::TokenBudget));
    let reason_text = reason.expect("terminal attempt has reason");
    assert!(
        reason_text.contains("token budget reached: provider stopped after passing"),
        "reason: {reason_text}"
    );
    if let BudgetStopCase::InProgress(operation) = case {
        let operation = if operation == "rebase-apply" {
            "rebase"
        } else {
            operation
        };
        assert!(
            reason_text.contains(&format!(
                "budget commit skipped: in-progress {operation} left uncommitted"
            )),
            "reason: {reason_text}"
        );
        assert_eq!(
            crate::objects::resolve_ref(&git, launch.worktree.repo(), launch.branch.as_str())?,
            launch.base
        );
        assert_eq!(
            std::fs::read_to_string(fixture.writer.worktree.join("budget-work.txt"))?,
            "uncommitted work before budget stop\n"
        );
        return Ok(());
    }
    if matches!(case, BudgetStopCase::CommitFailure) {
        assert!(
            reason_text.contains("budget commit failed:"),
            "reason: {reason_text}"
        );
        assert!(
            !reason_text.contains("work committed on its branch"),
            "reason: {reason_text}"
        );
        assert_eq!(
            crate::objects::resolve_ref(&git, launch.worktree.repo(), launch.branch.as_str())?,
            launch.base
        );
        assert_eq!(
            std::fs::read_to_string(fixture.writer.worktree.join("budget-work.txt"))?,
            "uncommitted work before budget stop\n"
        );
        let disposition: serde_json::Value = serde_json::from_slice(
            &read_sealed(&attempt, "budget-commit.json", RECORD_LIMIT)?
                .ok_or("budget commit disposition")?,
        )?;
        assert!(
            disposition["note"]
                .as_str()
                .ok_or("commit note")?
                .contains("budget commit failed:")
        );
        return Ok(());
    }
    if let Some(head) = native_head {
        let published =
            crate::objects::resolve_ref(&git, launch.worktree.repo(), launch.branch.as_str())?;
        fixture.lane_git(&[
            "merge-base",
            "--is-ancestor",
            &head.to_string(),
            &published.to_string(),
        ])?;
        assert_eq!(finished.head, Some(published));
        assert!(
            provenant_os::fixture::run_git(&fixture.writer.worktree, &["status", "--porcelain"])?
                .is_empty()
        );
        if matches!(case, BudgetStopCase::Native { dirty: false }) {
            assert_eq!(
                published, head,
                "clean native work must not create a snapshot"
            );
            let disposition: serde_json::Value = serde_json::from_slice(
                &read_sealed(&attempt, "budget-commit.json", RECORD_LIMIT)?
                    .ok_or("budget disposition")?,
            )?;
            assert!(disposition.get("commit").is_none());
            return Ok(());
        }
    }
    assert!(
        reason_text.contains("work committed on its branch"),
        "reason: {reason_text}"
    );
    // The newly committed file is now published on the lane branch.
    let published_tip =
        crate::objects::resolve_ref(&git, launch.worktree.repo(), launch.branch.as_str())?;
    assert_ne!(published_tip, launch.base);
    if matches!(case, BudgetStopCase::HostileFilter) {
        assert!(
            !filter_sentinel.exists(),
            "lane-configured filter executed on the host"
        );
        let blob = crate::objects::command(
            &git,
            launch.worktree.repo().common_git_dir(),
            launch.worktree.repo().root_path(),
            &[
                "show".into(),
                format!("{published_tip}:budget-work.txt").into(),
            ],
            None,
        )?;
        assert_eq!(blob, b"uncommitted work before budget stop\n");
    }

    let log = crate::objects::command(
        &git,
        launch.worktree.repo().common_git_dir(),
        launch.worktree.repo().root_path(),
        &[
            "log".into(),
            "-1".into(),
            "--pretty=%B".into(),
            published_tip.to_string().into(),
        ],
        None,
    )?;
    let log = String::from_utf8_lossy(&log);
    assert!(
        log.starts_with("wip: stopped on budget (") && log.contains(" files)\n"),
        "{log}"
    );
    assert!(log.contains("\nbudget-work.txt"), "{log}");
    let disposition: serde_json::Value = serde_json::from_slice(
        &read_sealed(&attempt, "budget-commit.json", RECORD_LIMIT)?.ok_or("budget commit")?,
    )?;
    assert_eq!(
        disposition["commit"].as_str(),
        Some(&published_tip.to_string()[..crate::writer::SHORT_SHA])
    );
    assert!(
        disposition["paths"]
            .as_array()
            .is_some_and(|paths| paths.iter().any(|path| path == "budget-work.txt"))
    );
    let placeholder = budget_stop_result(&disposition, "work committed on its branch");
    assert!(placeholder.contains("budget-work.txt"), "{placeholder}");
    Ok(())
}

#[test]
fn settlement_import_cancellation_preserves_provider_exit_ordering()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    for delivered in [None, Some(false)] {
        use crate::delivery_tests::{PrepareFixture, registered_landed_writer_fixture};
        use std::ffi::OsStr;
        let fixture = PrepareFixture::new()?;
        let execution = registered_landed_writer_fixture(&fixture)?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&execution, &call)?;
        let mut run = input();
        run.request.task.writer = Some(fixture.writer.clone());
        store.create(&run, &call)?;
        let id = run.attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let record = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let scope = RunStore::scope(&record);
        let attempt = store.attempt_dir(id, false)?;
        let mut launch = crate::prepare_writer(
            &fixture.writer,
            execution.state_root.as_path(),
            execution.project_id,
        )?;
        launch.snapshot(
            execution.state_root.as_path(),
            &attempt,
            scope,
            &provenant_records::policy::Policy::default(),
        )?;
        let candidate = store.temporary_path(id, &call)?.join("candidate-0");
        std::fs::create_dir_all(&candidate)?;
        let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
        crate::objects::prepare_bound(
            &git,
            launch.worktree.repo(),
            &attempt,
            &candidate,
            launch.branch.as_str(),
            launch.base,
        )?;
        let missing = "a".repeat(40);
        std::fs::write(
            candidate.join("git-common").join(launch.branch.as_str()),
            format!("{missing}\n"),
        )?;
        let (directory, binding) = launch.custody.take().ok_or("writer custody")?;
        let lease = provenant_supervisor::fixture::acquire_writer_with_binding(
            directory,
            scope,
            binding,
            call.lock_budget(),
            &cancel,
            &clock,
        )?;
        let mut recovery =
            provenant_supervisor::fixture::bind_writer_attempt(lease, &attempt)?.into_recovery()?;
        recovery.prepare_settlement()?;
        let mut observed = crate::tests::exit();
        observed.scope = scope;
        crate::dispatch_tests::seal_exit(&store, &observed)?;

        observed.cancel_delivered_before_exit = delivered;
        // Replace the fixture's exit before validation; ordering remains a fact.
        attempt.unlink(OsStr::new("process-exit.json"))?;
        crate::dispatch_tests::seal_exit(&store, &observed)?;
        crate::artifacts::seal(
            &attempt,
            "stop-intent.json",
            &crate::db::bytes(&serde_json::json!({
                "schema_version": 1, "scope": scope, "requester": execution.actor_id,
                "request_id": "import-cancel", "reason": StopReason::ExplicitCancellation
            }))?,
            RECORD_LIMIT,
        )?;
        let record = store.drive_settlement(
            id,
            &attempt,
            &observed,
            Completion::Recovered(Some(&recovery)),
            &crate::dispatch_tests::Fake::default(),
            &call,
        )?;
        assert!(
            matches!(record.record.state, AttemptState::Terminal {outcome: Outcome::Cancelled, reason: Some(ref reason), ..} if reason == "cancelled during Git import")
        );
        assert_eq!(
            record.record.terminal_basis,
            Some(TerminalBasis::ImportCancellation)
        );
        assert_eq!(
            record.record.cancellation_disposition,
            Some(CancellationDisposition::ImportEffective)
        );
        assert_eq!(observed.cancel_delivered_before_exit, delivered);
    }
    Ok(())
}

#[test]
fn never_spawned_exit_survives_cancel_during_import()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    for (stop_reason, expected, cancel_first) in [
        (None, Outcome::Rejected, true),
        (Some(StopReason::WallDeadline), Outcome::TimedOut, false),
    ] {
        use crate::delivery_tests::{PrepareFixture, registered_landed_writer_fixture};
        let fixture = PrepareFixture::new()?;
        let execution = registered_landed_writer_fixture(&fixture)?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&execution, &call)?;
        let mut run = input();
        run.request.task.writer = Some(fixture.writer.clone());
        store.create(&run, &call)?;
        let id = run.attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let record = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let scope = RunStore::scope(&record);
        let attempt = store.attempt_dir(id, false)?;
        let mut launch = crate::prepare_writer(
            &fixture.writer,
            execution.state_root.as_path(),
            execution.project_id,
        )?;
        launch.snapshot(
            execution.state_root.as_path(),
            &attempt,
            scope,
            &provenant_records::policy::Policy::default(),
        )?;
        let candidate = store.temporary_path(id, &call)?.join("candidate-0");
        std::fs::create_dir_all(&candidate)?;
        let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
        crate::objects::prepare_bound(
            &git,
            launch.worktree.repo(),
            &attempt,
            &candidate,
            launch.branch.as_str(),
            launch.base,
        )?;
        let missing = "a".repeat(40);
        std::fs::write(
            candidate.join("git-common").join(launch.branch.as_str()),
            format!("{missing}\n"),
        )?;
        let (directory, binding) = launch.custody.take().ok_or("writer custody")?;
        let lease = provenant_supervisor::fixture::acquire_writer_with_binding(
            directory,
            scope,
            binding,
            call.lock_budget(),
            &cancel,
            &clock,
        )?;
        drop(provenant_supervisor::fixture::bind_writer_attempt(
            lease, &attempt,
        )?);
        let mut observed = crate::tests::exit();
        observed.scope = scope;
        observed.never_spawned = true;
        observed.termination = None;
        observed.settlement_evidence = custody::SettlementEvidence::NeverSpawned;
        observed.stop_reason = stop_reason;
        crate::dispatch_tests::seal_exit(&store, &observed)?;
        crate::artifacts::seal(
            &attempt,
            "launch.json",
            &crate::db::bytes(&serde_json::json!({
                "schema_version": 1, "scope": scope, "requester": execution.actor_id
            }))?,
            RECORD_LIMIT,
        )?;
        if cancel_first {
            assert!(matches!(
                store.cancel(id, "stop", &call)?,
                crate::CancelReceipt::Requested | crate::CancelReceipt::AlreadyRequested
            ));
        } else {
            crate::artifacts::seal(
                &attempt,
                "stop-intent.json",
                &crate::db::bytes(&serde_json::json!({
                    "schema_version": 1, "scope": scope, "requester": execution.actor_id,
                    "request_id": "import-cancel", "reason": StopReason::ExplicitCancellation
                }))?,
                RECORD_LIMIT,
            )?;
        }
        let found = store.reconcile_with_observer(
            run.attempt_id,
            &crate::dispatch_tests::Fake::default(),
            &call,
            &provenant_supervisor::census::SystemProcessObserver,
        )?;
        assert!(
            matches!(
                found.as_slice(),
                [
                    (attempt, crate::Reconciled::WriterLeaseReleased { locks })
                ] if *attempt == id && !locks.is_empty()
            ),
            "{found:?}"
        );
        let record = store.record(id, &call)?;
        assert!(
            matches!(record.state, AttemptState::Terminal { outcome, .. } if outcome == expected)
        );
        assert_eq!(record.terminal_basis, Some(TerminalBasis::ProcessExit));
        assert_eq!(record.provider_launched, Some(false));
        assert!(read_sealed(&attempt, "import-cancelled.json", RECORD_LIMIT)?.is_none());
    }
    Ok(())
}

#[test]
fn settlement_import_fault_seals_retains_fence_and_resumes_without_provider()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    settlement_fault_modes(&["normal", "orphan", "manifest_conflict", "archived_fault"])
}

#[test]
fn reconcile_settles_failed_import_once_and_keeps_evidence()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    for mode in ["reconcile_failed", "reconcile_new", "reconcile_reboot"] {
        settlement_fault_modes(&[mode])
            .map_err(|error| std::io::Error::other(format!("{mode}: {error}")))?;
    }
    Ok(())
}

#[test]
fn retained_import_excludes_writer_after_lock_release()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    settlement_fault_modes(&["release_boundary"])
}

#[test]
fn retained_graph_writer_import_completes_while_recovery_holds_writer_lock()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    settlement_fault_modes(&["held_import_success"])
}

#[test]
fn writer_import_succeeds_after_a_concurrent_gc_pass()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    settlement_fault_modes(&["gc_during_import"])
}

#[test]
fn settlement_publish_advanced_branch_clears_fence()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    settlement_fault_modes(&["publish_advanced"])
}

#[test]
fn settlement_publish_diverged_branch_reports_error()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    settlement_fault_modes(&["publish_diverged"])
}

fn settlement_fault_modes(modes: &[&str]) -> std::result::Result<(), Box<dyn std::error::Error>> {
    for &mode in modes {
        use crate::delivery_tests::{PrepareFixture, registered_landed_writer_fixture};
        use std::ffi::OsStr;
        let fixture = PrepareFixture::new()?;
        let execution = registered_landed_writer_fixture(&fixture)?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&execution, &call)?;
        let mut run = input();
        run.request.task.writer = Some(fixture.writer.clone());
        if mode == "publish_diverged" {
            run.request
                .task
                .profile
                .knobs
                .insert(provenant_config::REBASE.into(), serde_json::json!("refuse"));
        }
        store.create(&run, &call)?;
        let id = run.attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let record = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let scope = RunStore::scope(&record);
        let attempt = store.attempt_dir(id, false)?;
        let mut launch = crate::prepare_writer(
            &fixture.writer,
            execution.state_root.as_path(),
            execution.project_id,
        )?;
        launch.snapshot(
            execution.state_root.as_path(),
            &attempt,
            scope,
            &provenant_records::policy::Policy::default(),
        )?;
        let candidate = store.temporary_path(id, &call)?.join("candidate-0");
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&candidate)?;
        // Fixture commands must bypass the active lane's private Git binding.
        let git =
            provenant_os::git::resolve_git_executable(None).ok_or("native Git unavailable")?;
        crate::objects::prepare_bound(
            &git,
            launch.worktree.repo(),
            &attempt,
            &candidate,
            launch.branch.as_str(),
            launch.base,
        )?;
        let retained_commit = if mode.starts_with("reconcile_")
            || matches!(mode, "held_import_success" | "gc_during_import")
        {
            // Make a real commit that exists only in the lane's private objects.
            let tree = crate::objects::command(
                &git,
                launch.worktree.repo().common_git_dir(),
                launch.worktree.path(),
                &[
                    "rev-parse".into(),
                    format!("{}^{{tree}}", launch.base).into(),
                ],
                None,
            )?;
            let tree = std::str::from_utf8(&tree)?.trim();
            let output = provenant_os::fixture::git_command_at(&git)
                .arg("--git-dir")
                .arg(candidate.join("git-common"))
                .args([
                    "commit-tree",
                    tree,
                    "-p",
                    &launch.base.to_string(),
                    "-m",
                    "Retained lane commit",
                ])
                .current_dir(launch.worktree.path())
                .env("GIT_OBJECT_DIRECTORY", candidate.join("git-objects"))
                .env(
                    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
                    launch.worktree.repo().common_git_dir().join("objects"),
                )
                .output()?;
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let raw = output.stdout;
            let head = provenant_core::GitOid::parse(std::str::from_utf8(&raw)?.trim())?;
            if mode == "held_import_success" {
                let commit = format!("{head}^{{commit}}");
                let private = provenant_os::fixture::git_command_at(&git)
                    .arg("--git-dir")
                    .arg(candidate.join("git-common"))
                    .args(["cat-file", "-e", &commit])
                    .env("GIT_OBJECT_DIRECTORY", candidate.join("git-objects"))
                    .env(
                        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
                        launch.worktree.repo().common_git_dir().join("objects"),
                    )
                    .output()?;
                assert!(
                    private.status.success(),
                    "commit is absent from private Git source: {}",
                    String::from_utf8_lossy(&private.stderr)
                );
                let host = provenant_os::fixture::git_command_at(&git)
                    .arg("--git-dir")
                    .arg(launch.worktree.repo().common_git_dir())
                    .args(["cat-file", "-e", &commit])
                    .output()?;
                assert!(
                    !host.status.success(),
                    "retained commit was already visible in host Git"
                );
            }
            let branch_ref = candidate.join("git-common").join(launch.branch.as_str());
            std::fs::write(&branch_ref, format!("{head}\n")).map_err(|error| {
                std::io::Error::other(format!(
                    "write retained private branch ref {}: {error}",
                    branch_ref.display()
                ))
            })?;
            let oid = head.to_string();
            let path = candidate
                .join("git-objects")
                .join(&oid[..2])
                .join(&oid[2..]);
            // The fixture Git executable must honour the candidate's
            // GIT_OBJECT_DIRECTORY for this private source snapshot.
            let bytes = std::fs::read(&path).map_err(|error| {
                std::io::Error::other(format!(
                    "read retained private commit object {}: {error}",
                    path.display()
                ))
            })?;
            Some((head, path, bytes))
        } else {
            None
        };
        let mut published_head = None;
        let mut advanced_tip = None;
        if mode.starts_with("publish_") {
            fixture.commit(&fixture.writer.worktree, "Lane head")?;
            let head =
                crate::objects::resolve_ref(&git, launch.worktree.repo(), launch.branch.as_str())?;
            published_head = Some(head);
            std::fs::write(
                candidate.join("git-common/refs/heads/lane"),
                format!("{head}\n"),
            )?;
            if mode == "publish_advanced" {
                // The host branch already holds the lane tip and later work.
                std::fs::write(
                    fixture.writer.worktree.join("later.txt"),
                    "later lane contents\n",
                )?;
                fixture.lane_git(&["add", "later.txt"])?;
                fixture.commit(&fixture.writer.worktree, "Later lane")?;
                advanced_tip = Some(crate::objects::resolve_ref(
                    &git,
                    launch.worktree.repo(),
                    launch.branch.as_str(),
                )?);
            } else {
                fixture.lane_git(&["reset", "--hard", &launch.base.to_string()])?;
                fixture.commit(&fixture.writer.worktree, "Competing branch")?;
            }
        }
        let (directory, binding) = launch.custody.take().ok_or("writer custody")?;
        let lock_directory = directory.try_clone()?;
        let lease = provenant_supervisor::fixture::acquire_writer_with_binding(
            directory,
            scope,
            binding,
            call.lock_budget(),
            &cancel,
            &clock,
        )?;
        let mut recovery =
            provenant_supervisor::fixture::bind_writer_attempt(lease, &attempt)?.into_recovery()?;
        recovery.prepare_settlement()?;
        if mode == "held_import_success" {
            store.record_worktree_settlement(
                id,
                &serde_json::json!({
                    "schema_version":1,
                    "state":"retained",
                    "worktree":fixture.writer.worktree,
                    "reason":"graph writer worktree retained for continuation, later rounds and explicit delivery",
                }),
                &call,
            )?;
        }
        let mut observed = crate::tests::exit();
        observed.scope = scope;
        crate::dispatch_tests::seal_exit(&store, &observed)?;

        let providers = crate::dispatch_tests::Fake::default();
        crate::artifacts::seal(
            &attempt,
            "launch.json",
            &crate::db::bytes(&serde_json::json!({
                "schema_version":1,"scope":scope,"requester":execution.actor_id
            }))?,
            RECORD_LIMIT,
        )?;
        if mode == "orphan" {
            store.retain_import_fence(&record)?;
            crate::artifacts::seal(
                &attempt,
                "terminal-effects.json",
                &crate::db::bytes(&crate::store::TerminalEffects {
                    schema_version: 1,
                    scope,
                    effects: vec![(
                        SettlementEffect::GitImport,
                        EffectDisposition::Failed {
                            reason: "pre-crash import failure".into(),
                            retryable: true,
                        },
                    )],
                })?,
                RECORD_LIMIT,
            )?;
            crate::artifacts::seal(&attempt, "result.md", b"orphan result", RESULT_LIMIT)?;
        }
        if matches!(mode, "manifest_conflict" | "archived_fault") {
            let archive = crate::artifacts::subdir(&attempt, OsStr::new("quarantine"), true)?;
            crate::artifacts::seal(
                &archive,
                "manifest.json",
                b"conflicting writer manifest",
                RECORD_LIMIT,
            )?;
        }
        if mode == "reconcile_new" {
            drop(recovery);
            crate::writer::FAIL_IMPORT.with(|fault| fault.set(true));
            let found = store.reconcile_with_observer(
                run.attempt_id,
                &providers,
                &call,
                &provenant_supervisor::census::SystemProcessObserver,
            );
            crate::writer::FAIL_IMPORT.with(|fault| fault.set(false));
            assert!(found?.iter().any(|(attempt, state)| {
                *attempt == id
                    && matches!(
                        state,
                        crate::Reconciled::Finished | crate::Reconciled::WriterLeaseReleased { .. }
                    )
            }));
            assert!(
                matches!(store.record(id, &call)?.settlement_receipts.unwrap().git_import,
                EffectDisposition::Failed { retryable: false, ref reason }
                if reason.contains("injected Git import failure"))
            );
            assert!(store.writer_fences()?.is_empty());
            let (_, path, bytes) = retained_commit.as_ref().ok_or("private commit")?;
            assert_eq!(
                &std::fs::read(path).map_err(|error| {
                    std::io::Error::other(format!("retained private commit after GC: {error}"))
                })?,
                bytes
            );
            assert_eq!(
                crate::objects::resolve_ref(&git, launch.worktree.repo(), launch.branch.as_str())?,
                launch.base
            );
            assert!(
                store
                    .reconcile_with_observer(
                        run.attempt_id,
                        &providers,
                        &call,
                        &provenant_supervisor::census::SystemProcessObserver
                    )?
                    .is_empty()
            );
            continue;
        }
        crate::writer::FAIL_IMPORT.with(|fault| {
            fault.set(matches!(
                mode,
                "normal"
                    | "release_boundary"
                    | "archived_fault"
                    | "reconcile_failed"
                    | "reconcile_reboot"
                    | "gc_during_import"
            ))
        });
        let first = store.drive_settlement(
            id,
            &attempt,
            &observed,
            Completion::Recovered(Some(&recovery)),
            &providers,
            &call,
        );
        crate::writer::FAIL_IMPORT.with(|fault| fault.set(false));
        let first = first.map_err(|error| {
            std::io::Error::other(format!(
                "drive held-Recovery settlement import and effects: {error}"
            ))
        })?;
        if mode == "publish_advanced" {
            // A host branch that already contains the lane tip needs no
            // forced publication: the import completes and the fence clears.
            assert!(first.release_permitted);
            assert_eq!(
                first
                    .record
                    .settlement_receipts
                    .as_ref()
                    .ok_or("settlement receipts")?
                    .git_import,
                EffectDisposition::Done
            );
            store.settle_recovered(&attempt, recovery)?;
            assert_eq!(
                crate::objects::resolve_ref(&git, launch.worktree.repo(), launch.branch.as_str())?,
                advanced_tip.ok_or("advanced tip")?
            );
            assert!(
                crate::objects::resolve_ref(
                    &git,
                    launch.worktree.repo(),
                    &crate::objects::import_pin(id, published_head.ok_or("lane head")?)
                )
                .is_err()
            );
            let journal: serde_json::Value = crate::db::decode(
                &attempt.read_stable(OsStr::new("settlement-progress.json"), RECORD_LIMIT)?,
            )?;
            assert_eq!(journal["phase"], "complete");
            assert!(journal["blocking_reason"].is_null());
            assert_eq!(
                std::fs::read_to_string(fixture.writer.worktree.join("later.txt"))?,
                "later lane contents\n"
            );
            assert!(store.writer_fences()?.is_empty());
            continue;
        }
        if mode == "publish_diverged" {
            // A forked lane cannot publish on any retry. It is refused at
            // once, its custody is released and its commits stay in the
            // attempt's private Git for the host to recover.
            assert!(
                first.release_permitted,
                "{:?}",
                first.record.settlement_receipts
            );
            assert!(matches!(
                first
                    .record
                    .settlement_receipts
                    .as_ref()
                    .ok_or("settlement receipts")?
                    .git_import,
                EffectDisposition::Failed { retryable: false, ref reason }
                    if reason.contains("diverged from the host branch")
            ));
            let refusal = object_import_refused(&attempt, scope)?.ok_or("refusal evidence")?;
            assert!(refusal.contains("lane history diverged"), "{refusal}");
            assert!(
                crate::objects::publish_branch(
                    &git,
                    launch.worktree.repo(),
                    crate::objects::BranchPublication {
                        branch: launch.branch.as_str(),
                        base: launch.base,
                        launch_ref: None,
                        head: published_head.ok_or("lane head")?,
                        attempt: id,
                        history: provenant_config::LaneHistory::resolve(&run.request.task.profile,),
                    },
                    |_| Ok(()),
                )
                .is_err()
            );
            store.settle_recovered(&attempt, recovery)?;
            assert!(store.writer_fences()?.is_empty());
            assert!(candidate.join("git-common/HEAD").is_file());
            continue;
        }
        if mode == "held_import_success" {
            assert!(
                first.release_permitted,
                "{:?}",
                first.record.settlement_receipts
            );
            assert!(matches!(
                &first
                    .record
                    .settlement_receipts
                    .as_ref()
                    .ok_or("settlement receipts")?
                    .git_import,
                EffectDisposition::Done
            ));
            assert_eq!(
                store
                    .worktree_settlement(id, &call)?
                    .ok_or("retained receipt")?["state"],
                "retained"
            );
            assert!(recovery.holds_writer_lock(&lock_directory)?);
            assert!(
                lock_directory
                    .try_lock(
                        OsStr::new("writer.lock"),
                        provenant_os::fs::LockMode::Exclusive,
                    )
                    .is_err()
            );
            let (head, _, _) = retained_commit.as_ref().ok_or("retained commit")?;
            assert_eq!(
                crate::objects::resolve_ref(&git, launch.worktree.repo(), launch.branch.as_str())?,
                *head
            );
            store
                .settle_recovered(&attempt, recovery)
                .map_err(|error| {
                    std::io::Error::other(format!(
                        "settle recovered writer after successful import: {error}"
                    ))
                })?;
            assert!(store.writer_fences()?.is_empty());
            continue;
        }
        if mode == "manifest_conflict" {
            assert!(matches!(
                first.record.settlement_receipts.unwrap().manifest_conflict,
                EffectDisposition::Failed {
                    retryable: true,
                    ..
                }
            ));
            let archive = crate::artifacts::subdir(&attempt, OsStr::new("quarantine"), false)?;
            let path = execution
                .state_root
                .as_path()
                .join(format!("attempts/{id}/quarantine"));
            drop(recovery);
            drop(store);
            let store = RunStore::open(&execution, &call)?;
            store.reconcile_terminal_effects(&store.record(id, &call)?, &call)?;
            assert_eq!(
                read_sealed(&archive, "manifest.json", RECORD_LIMIT)?.unwrap(),
                b"conflicting writer manifest"
            );
            assert!(matches!(
                store
                    .record(id, &call)?
                    .settlement_receipts
                    .unwrap()
                    .manifest_conflict,
                EffectDisposition::Failed {
                    retryable: true,
                    ..
                }
            ));
            std::fs::rename(
                path.join("manifest.json"),
                path.join("manifest.conflict.json"),
            )?;
            store.reconcile_terminal_effects(&store.record(id, &call)?, &call)?;
            assert!(matches!(
                store
                    .record(id, &call)?
                    .settlement_receipts
                    .unwrap()
                    .manifest_conflict,
                EffectDisposition::Done
            ));
            let found = store.reconcile_with_observer(
                run.attempt_id,
                &providers,
                &call,
                &provenant_supervisor::census::SystemProcessObserver,
            )?;
            assert!(
                !found
                    .iter()
                    .any(|(_, state)| matches!(state, crate::Reconciled::Uncertain { .. })),
                "{found:?}"
            );
            assert!(
                store.writer_fences()?.is_empty(),
                "{:?}",
                store.writer_fences()?
            );
            continue;
        }
        if mode == "orphan" {
            assert!(first.release_permitted);
            assert_eq!(
                first
                    .record
                    .settlement_receipts
                    .as_ref()
                    .unwrap()
                    .git_import,
                EffectDisposition::Done
            );
            store.settle_recovered(&attempt, recovery)?;
            assert!(store.writer_fences()?.is_empty());
            continue;
        }
        assert!(!first.release_permitted);
        assert!(matches!(
            first
                .record
                .settlement_receipts
                .as_ref()
                .unwrap()
                .git_import,
            EffectDisposition::Failed {
                retryable: true,
                ..
            }
        ));
        let decision = read_sealed(&attempt, "terminal-pending.json", RECORD_LIMIT)?
            .ok_or("sealed decision")?;
        assert!(!store.writer_fences()?.is_empty());
        if mode == "reconcile_failed" {
            // A held custody lock must leave the receipt retryable.
            store.reconcile_with_observer(
                run.attempt_id,
                &providers,
                &call,
                &provenant_supervisor::census::SystemProcessObserver,
            )?;
            assert_eq!(
                store.record(id, &call)?.settlement_receipts,
                first.record.settlement_receipts
            );
        }
        assert!(
            store.settle_recovered(&attempt, recovery).is_err(),
            "unresolved import cannot release"
        );
        assert!(candidate.join("git-common/HEAD").is_file());
        let mut competing = crate::prepare_writer(
            &fixture.writer,
            execution.state_root.as_path(),
            execution.project_id,
        )?;
        let (directory, binding) = competing.custody.take().ok_or("competing custody")?;
        let other = provenant_records::custody::Scope {
            attempt: provenant_core::AttemptId::from_bytes([99; 16]),
            ..scope
        };
        assert!(
            store
                .acquire_writer_fenced(
                    directory,
                    other,
                    binding,
                    &|directory, scope, binding| {
                        provenant_supervisor::fixture::acquire_writer_with_binding(
                            directory,
                            scope,
                            binding,
                            provenant_core::WaitBudget::nonblocking(clock.mono_now()),
                            &cancel,
                            &clock,
                        )
                    },
                    &call
                )
                .is_err(),
            "retained reservation excludes competing writer"
        );
        if matches!(mode, "reconcile_failed" | "reconcile_reboot") {
            struct Reboot;
            impl provenant_supervisor::census::ProcessObserver for Reboot {
                fn boot_identity(
                    &self,
                ) -> std::result::Result<String, provenant_supervisor::census::CensusError>
                {
                    Ok("darwin:11111111-1111-1111-1111-111111111111".into())
                }
                fn observe(&self, _: i64) -> provenant_supervisor::census::ProcessObservation {
                    provenant_supervisor::census::ProcessObservation::Absent
                }
                fn census(
                    &self,
                    _: usize,
                ) -> std::result::Result<
                    provenant_supervisor::census::Census,
                    provenant_supervisor::census::CensusError,
                > {
                    // Reboot proof needs only the boot identity, but holderless
                    // settlement now also takes a bounded census to reap
                    // sandbox-marked escapees. This observer's world has no
                    // live processes, so answer with an empty census.
                    provenant_supervisor::census::Census::from_observations(
                        [],
                        [],
                        [],
                        true,
                        false,
                        16,
                    )
                }
            }
            let observer: &dyn provenant_supervisor::census::ProcessObserver =
                if mode == "reconcile_reboot" {
                    // A real bound reservation survives the failed import;
                    // only its boot changes, as across a machine restart.
                    let reservation = store
                        .writer_fences()?
                        .into_iter()
                        .find(|fence| fence.scope == scope)
                        .ok_or("retained fence")?
                        .reservation;
                    let mut value: serde_json::Value =
                        serde_json::from_slice(&std::fs::read(&reservation)?)?;
                    value["boot"] = "darwin:00000000-0000-0000-0000-000000000000".into();
                    std::fs::write(&reservation, serde_json::to_vec(&value)?)?;
                    &Reboot
                } else {
                    &provenant_supervisor::census::SystemProcessObserver
                };
            let sealed = read_sealed(&attempt, "terminal-pending.json", RECORD_LIMIT)?;
            let result = read_sealed(&attempt, "result.md", RESULT_LIMIT)?;
            let retained = std::fs::read(candidate.join("git-common/HEAD"))?;
            drop(store);
            let store = RunStore::open(&execution, &call)?;
            let name = OsStr::new("settlement-progress.json");
            let mut journal: serde_json::Value =
                crate::db::decode(&attempt.read_stable(name, RECORD_LIMIT)?)?;
            journal["retry_after_ms"] = 0.into();
            attempt
                .atomic_write(
                    name,
                    ".retry-fixture-",
                    &crate::db::bytes(&journal)?,
                    provenant_os::fs::PublishMode::Replace,
                )?
                .require_durable()?;
            // Settle while the public queuing pass is still running. Its
            // earlier error snapshot must disappear from the final report.
            let found = store.queue_reconcile_with(run.attempt_id, &call, |_, _| {
                let found = store
                    .reconcile_with_observer(run.attempt_id, &providers, &call, observer)
                    .expect("recovery");
                assert!(
                    found.iter().any(|(attempt, state)| {
                        *attempt == id
                            && matches!(
                                state,
                                crate::Reconciled::Finished
                                    | crate::Reconciled::WriterLeaseReleased { .. }
                            )
                    }),
                    "{found:?}"
                );
                Ok(())
            })?;
            assert!(found.is_empty(), "{found:?}");
            if mode == "reconcile_reboot" {
                let cleanup = crate::controls::REBOOT_TEMPORARY_CLEANUP
                    .with(|result| result.borrow_mut().take());
                assert_eq!(cleanup, Some(Ok(None)));
            }
            assert!(
                matches!(store.record(id, &call)?.settlement_receipts.unwrap().git_import,
                EffectDisposition::Failed { retryable: false, ref reason }
                if reason.contains("injected Git import failure"))
            );
            assert!(store.writer_fences()?.is_empty());
            assert_eq!(
                read_sealed(&attempt, "terminal-pending.json", RECORD_LIMIT)?,
                sealed
            );
            assert_eq!(
                std::fs::read(candidate.join("git-common/HEAD")).map_err(|error| {
                    std::io::Error::other(format!(
                        "retained candidate HEAD after reconcile: {error}"
                    ))
                })?,
                retained
            );
            assert_eq!(read_sealed(&attempt, "result.md", RESULT_LIMIT)?, result);
            let (head, path, bytes) = retained_commit.as_ref().ok_or("private commit")?;
            let recovery_info =
                retained_git(execution.state_root.as_path(), &store.record(id, &call)?)?;
            assert_eq!(recovery_info.head, Some(*head));
            assert_eq!(recovery_info.objects, Some(candidate.join("git-objects")));
            // Age well past ordinary collection; refused-import evidence stays.
            store.gc_provider_state(
                &store.root,
                1,
                clock
                    .wall_now()?
                    .checked_add(provenant_core::DurationMicros::from_seconds(90 * 86_400)?)?,
                false,
                &call,
                &crate::RetentionScan::default(),
            )?;
            assert_eq!(
                &std::fs::read(path).map_err(|error| {
                    std::io::Error::other(format!("retained private commit after GC: {error}"))
                })?,
                bytes
            );
            assert_eq!(
                crate::objects::resolve_ref(&git, launch.worktree.repo(), launch.branch.as_str())?,
                launch.base
            );
            assert_eq!(
                crate::objects::resolve_ref(
                    &git,
                    launch.worktree.repo(),
                    &format!("refs/provenant/lane-base/{id}")
                )?,
                launch.base
            );

            assert!(
                store
                    .reconcile_with_observer(
                        run.attempt_id,
                        &providers,
                        &call,
                        &provenant_supervisor::census::SystemProcessObserver,
                    )?
                    .is_empty()
            );
            continue;
        }
        if mode == "archived_fault" {
            // A retryable import failure keeps its fence even though a
            // manifest already exists; clear it so the retry can seal.
            let archive = crate::artifacts::subdir(&attempt, OsStr::new("quarantine"), false)?;
            archive.unlink(OsStr::new("manifest.json"))?;
        }
        drop(store);
        let store = RunStore::open(&execution, &call)?;
        let journal_name = OsStr::new("settlement-progress.json");
        let mut journal: serde_json::Value =
            crate::db::decode(&attempt.read_stable(journal_name, RECORD_LIMIT)?)?;
        journal["retry_after_ms"] = 0.into();
        attempt
            .atomic_write(
                journal_name,
                ".retry-fixture-",
                &crate::db::bytes(&journal)?,
                provenant_os::fs::PublishMode::Replace,
            )?
            .require_durable()?;
        if mode == "release_boundary" {
            let directory = store.writer_custody_directory(&first.record, &fixture.writer)?;
            let mut recovery = store
                .recover_writer_exit(directory, scope, &call)?
                .ok_or("recovery")?;
            recovery.prepare_settlement()?;
            let finished = store.drive_settlement(
                id,
                &attempt,
                &observed,
                Completion::Recovered(Some(&recovery)),
                &providers,
                &call,
            )?;
            assert!(finished.release_permitted);
            assert!(store.retained_import_fence(scope)?);
            drop(recovery);
            // The kernel lock is now free, but the retained import still
            // protects this worktree until reconciliation completes.
            let mut competing = crate::prepare_writer(
                &fixture.writer,
                execution.state_root.as_path(),
                execution.project_id,
            )?;
            let (directory, binding) = competing.custody.take().ok_or("competing custody")?;
            let other = provenant_records::custody::Scope {
                attempt: provenant_core::AttemptId::from_bytes([99; 16]),
                ..scope
            };
            let refusal = match store.acquire_writer_fenced(
                directory,
                other,
                binding,
                &|directory, scope, binding| {
                    provenant_supervisor::fixture::acquire_writer_with_binding(
                        directory,
                        scope,
                        binding,
                        provenant_core::WaitBudget::nonblocking(clock.mono_now()),
                        &cancel,
                        &clock,
                    )
                },
                &call,
            ) {
                Err(error) => error,
                Ok(_) => panic!("retained import must exclude a competing writer"),
            };
            assert!(matches!(
                refusal,
                crate::writer::WriterCustodyError::Admission(
                    custody::CustodyError::ImportRetained { fence }
                ) if fence.scope == scope
            ));
            let found = store.reconcile_with_observer(
                run.attempt_id,
                &providers,
                &call,
                &provenant_supervisor::census::SystemProcessObserver,
            )?;
            assert!(
                !found
                    .iter()
                    .any(|(_, state)| matches!(state, crate::Reconciled::Uncertain { .. })),
                "{found:?}"
            );
        } else if mode == "gc_during_import" {
            // A concurrent GC pass while the settlement driver owns the
            // retry must not take the private objects the import reads.
            let root = Directory::open_anchor(execution.state_root.as_path())?;
            let finished = |_: &AttemptRecord, _: &CanonicalRequest| ScratchOwner::Finished;
            let policy = ScratchPolicy {
                owner: Some(&finished),
                ..ScratchPolicy::new(0)
            };
            let collection_time = clock
                .wall_now()?
                .checked_add(provenant_core::DurationMicros::from_seconds(90 * 86_400)?)?;
            // A free lease between settlement attempts is not completion.
            let idle = store.gc_temporary(
                &root,
                &policy,
                collection_time,
                false,
                &call,
                &RetentionScan::default(),
            )?;
            assert!(idle.attempt_tmp.is_empty(), "{idle:?}");
            assert!(candidate.join("git-objects").is_dir());
            let driver = crate::settlement::Guard::claim(&attempt, scope, &clock)?
                .ok_or("settlement driver claim")?;
            let gc = store.gc_temporary(
                &root,
                &policy,
                collection_time,
                false,
                &call,
                &RetentionScan::default(),
            )?;
            assert!(
                gc.attempt_tmp.is_empty(),
                "GC collected scratch under a live settlement driver: {gc:?}"
            );
            assert!(
                candidate.join("git-objects").is_dir(),
                "private objects vanished before import"
            );
            drop(driver);
            let journal_name = OsStr::new("settlement-progress.json");
            let mut journal: serde_json::Value =
                crate::db::decode(&attempt.read_stable(journal_name, RECORD_LIMIT)?)?;
            journal["retry_after_ms"] = 0.into();
            attempt
                .atomic_write(
                    journal_name,
                    ".gc-retry-fixture-",
                    &crate::db::bytes(&journal)?,
                    provenant_os::fs::PublishMode::Replace,
                )?
                .require_durable()?;
            store.finish(id, &attempt, &observed, &providers, &call)?;
            let (head, _, _) = retained_commit.as_ref().ok_or("private commit")?;
            assert_eq!(
                crate::objects::resolve_ref(&git, launch.worktree.repo(), launch.branch.as_str())?,
                *head
            );
        } else {
            store.finish(id, &attempt, &observed, &providers, &call)?;
        }
        assert_eq!(
            providers.decoded.get(),
            1,
            "recovery never reruns decoding or the provider"
        );
        assert_eq!(
            read_sealed(&attempt, "terminal-pending.json", RECORD_LIMIT)?.unwrap(),
            decision
        );
        let completed = store.record(id, &call)?;
        assert!(matches!(
            completed.settlement_receipts.unwrap().git_import,
            EffectDisposition::Done
        ));
        assert!(store.writer_fences()?.is_empty());
    }
    Ok(())
}

#[test]
fn settlement_failed_import_becomes_terminal_after_three_attempts_and_releases_custody()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    use crate::delivery_tests::{PrepareFixture, registered_landed_writer_fixture};
    use std::ffi::OsStr;
    let fixture = PrepareFixture::new()?;
    let execution = registered_landed_writer_fixture(&fixture)?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&execution, &call)?;
    let mut run = input();
    run.request.task.writer = Some(fixture.writer.clone());
    store.create(&run, &call)?;
    let id = run.attempt_id;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let record = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let scope = RunStore::scope(&record);
    let attempt = store.attempt_dir(id, false)?;
    let mut launch = crate::prepare_writer(
        &fixture.writer,
        execution.state_root.as_path(),
        execution.project_id,
    )?;
    launch.snapshot(
        execution.state_root.as_path(),
        &attempt,
        scope,
        &provenant_records::policy::Policy::default(),
    )?;
    let candidate = store.temporary_path(id, &call)?.join("candidate-0");
    std::fs::create_dir_all(&candidate)?;
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    crate::objects::prepare_bound(
        &git,
        launch.worktree.repo(),
        &attempt,
        &candidate,
        launch.branch.as_str(),
        launch.base,
    )?;
    let missing = "a".repeat(40);
    std::fs::write(
        candidate.join("git-common").join(launch.branch.as_str()),
        format!("{missing}\n"),
    )?;
    let (directory, binding) = launch.custody.take().ok_or("writer custody")?;
    let lease = provenant_supervisor::fixture::acquire_writer_with_binding(
        directory,
        scope,
        binding,
        call.lock_budget(),
        &cancel,
        &clock,
    )?;
    let mut recovery =
        provenant_supervisor::fixture::bind_writer_attempt(lease, &attempt)?.into_recovery()?;
    recovery.prepare_settlement()?;
    let mut observed = crate::tests::exit();
    observed.scope = scope;
    crate::dispatch_tests::seal_exit(&store, &observed)?;
    for index in 0..3 {
        if index != 0 {
            let retained = store.drive_settlement(
                id,
                &attempt,
                &observed,
                Completion::Recovered(Some(&recovery)),
                &crate::dispatch_tests::Fake::default(),
                &call,
            )?;
            assert!(!retained.release_permitted);
            let journal_name = OsStr::new("settlement-progress.json");
            let mut journal: serde_json::Value =
                crate::db::decode(&attempt.read_stable(journal_name, RECORD_LIMIT)?)?;
            assert_eq!(journal["attempts"], index);
            assert!(journal["owner"].is_null());
            // Advance the persisted deadline instead of making the fixture
            // sleep through the production five- and thirty-second delays.
            journal["retry_after_ms"] = serde_json::json!(0);
            attempt
                .atomic_write(
                    journal_name,
                    ".fixture-backoff-",
                    &crate::db::bytes(&journal)?,
                    provenant_os::fs::PublishMode::Replace,
                )?
                .require_durable()?;
        }
        let result = store.drive_settlement(
            id,
            &attempt,
            &observed,
            Completion::Recovered(Some(&recovery)),
            &crate::dispatch_tests::Fake::default(),
            &call,
        );
        if index < 2 {
            let active: i64 = store.db.query_row(
                "SELECT COUNT(*) FROM admission WHERE attempt=?1",
                [id.to_string()],
                |row| row.get(0),
            )?;
            assert_eq!(active, 0);
            assert!(!store.writer_fences()?.is_empty());
            let finished = result?;
            assert!(!finished.release_permitted);
            assert!(matches!(
                finished.record.state,
                AttemptState::Terminal {
                    outcome: Outcome::Failed,
                    ..
                }
            ));
            assert!(matches!(
                finished.record.settlement_receipts.unwrap().git_import,
                EffectDisposition::Failed {
                    retryable: true,
                    ..
                }
            ));
        } else {
            object_import_refused(&attempt, scope)?.ok_or("refusal evidence")?;
            let finished = result?;
            assert!(finished.release_permitted);
            assert!(matches!(
                finished.record.state,
                AttemptState::Terminal {
                    outcome: Outcome::Failed,
                    ..
                }
            ));
        }
        let journal: serde_json::Value = crate::db::decode(
            &attempt.read_stable(OsStr::new("settlement-progress.json"), RECORD_LIMIT)?,
        )?;
        assert_eq!(journal["attempts"], index + 1);
    }
    let active: i64 = store.db.query_row(
        "SELECT COUNT(*) FROM admission WHERE attempt=?1",
        [id.to_string()],
        |row| row.get(0),
    )?;
    assert_eq!(active, 0);
    store.settle_recovered(&attempt, recovery)?;
    assert!(store.writer_fences()?.is_empty());
    let terminal = store.record(id, &call)?;
    assert!(store.custody_released(&terminal, &call)?);
    assert!(
        candidate.join("git-common/HEAD").is_file(),
        "refused private Git remains recoverable"
    );
    assert!(
        attempt
            .read_stable(OsStr::new("release.json"), RECORD_LIMIT)?
            .len()
            > 10
    );
    Ok(())
}

#[test]
fn writer_recovery_accepts_only_matching_terminal_absence()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let custody_dir = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    store.create(&run, &call)?;
    let id = run.attempt_id;
    let starting = store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let scope = RunStore::scope(&starting);
    let directory = || Directory::open_anchor(custody_dir.path());
    assert!(
        store
            .recover_writer_exit(directory()?, scope, &call)
            .is_err(),
        "missing custody cannot settle a nonterminal attempt"
    );
    drop(custody::acquire_reader(
        directory()?,
        scope,
        call.lock_budget(),
        call.cancel,
        call.clock,
    )?);
    let recovery = store
        .recover_writer_exit(directory()?, scope, &call)?
        .ok_or("prepared reservation must remain recoverable")?;
    recovery.withdraw_prepared()?;
    store.withdraw(
        id,
        AttemptState::Starting {
            extra: Default::default(),
        },
        Outcome::Cancelled,
        "fixture cancellation",
        clock.wall_now()?,
        &call,
    )?;
    // The caller holds a starting snapshot while the guardian has committed
    // cancellation and removed the reservation.
    assert_eq!(
        starting.state,
        AttemptState::Starting {
            extra: Default::default()
        }
    );
    assert!(
        store
            .recover_writer_exit(directory()?, scope, &call)?
            .is_none()
    );
    let mut changed = scope;
    changed.generation += 1;
    assert!(
        store
            .recover_writer_exit(directory()?, changed, &call)
            .is_err(),
        "another generation's terminal state is not settlement"
    );
    std::fs::write(
        custody_dir
            .path()
            .join(scope.attempt.to_string())
            .join("reservation.json"),
        b"{",
    )?;
    assert!(
        store
            .recover_writer_exit(directory()?, scope, &call)
            .is_err(),
        "terminal state must not hide corrupt retained custody"
    );
    Ok(())
}
