use super::*;
use provenant_core::{
    ActorId, AttemptId, CancelToken, Clock as _, Digest, MonoTime, OperationId, ProjectId,
    RootInputs, RunId, TaskId, TestClock, WallTime,
};
use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
use provenant_records::CanonicalV1;
use provenant_supervisor::custody::{
    CaptureTotals, ExitObservation, PublicationState, Scope, SettlementEvidence,
};
use std::{ffi::OsString, path::Path, time::Duration};
type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

#[test]
fn command_identity_is_verified_without_manufacturing_model_evidence() {
    let route = RouteRecord {
        provider: Some("command".into()),
        ..Default::default()
    };
    assert!(crate::reader::verified_route(
        Some(&route),
        Some(&route),
        Some(&route)
    ));
    for provider in ["codex", "unknown"] {
        let route = RouteRecord {
            provider: Some(provider.into()),
            ..Default::default()
        };
        assert!(!crate::reader::verified_route(
            Some(&route),
            Some(&route),
            Some(&route)
        ));
    }
    assert!(!crate::reader::verified_route(
        Some(&route),
        Some(&route),
        None
    ));
    let mut invented = route.clone();
    invented.model = Some("placeholder".into());
    assert!(!crate::reader::verified_route(
        Some(&route),
        Some(&invented),
        Some(&route)
    ));
    let mut effort = route.clone();
    effort.effort = Some("high".into());
    assert!(!crate::reader::verified_route(
        Some(&route),
        Some(&effort),
        Some(&route)
    ));
}

#[test]
fn unsafe_path_reports_each_check_and_relative_path() {
    for (check, reason) in [
        (PathCheck::Mode, "mode"),
        (PathCheck::Owner, "owner"),
        (PathCheck::Symlink, "symlink"),
        (PathCheck::Kind, "kind"),
        (PathCheck::Links, "link count"),
        (PathCheck::Identity, "identity mismatch"),
        (PathCheck::Containment, "outside the root"),
        (PathCheck::Missing, "missing"),
    ] {
        let error = RunError::unsafe_path("runs/example/process.json", check);
        assert_eq!(
            error.to_string(),
            format!(
                "run-store path runs/example/process.json is not private or contained: {reason}"
            )
        );
    }
}

#[test]
fn unsafe_state_root_is_named_relative_to_itself() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    drop(RunStore::open(&context, &call)?);
    std::fs::set_permissions(
        context.state_root.as_path(),
        std::fs::Permissions::from_mode(0o755),
    )?;
    let error = match RunStore::open_read_only(&context, &call) {
        Err(error) => error,
        Ok(_) => panic!("unsafe state root was accepted"),
    };
    assert_eq!(
        error.to_string(),
        "run-store path . is not private or contained: mode"
    );
    let error = match RunStore::open(&context, &call) {
        Err(error) => error,
        Ok(_) => panic!("unsafe state root was accepted for writing"),
    };
    assert_eq!(
        error.to_string(),
        "run-store path . is not private or contained: mode"
    );
    Ok(())
}
#[test]
fn mutation_record_reads_sync_the_checkpoint_while_views_do_not() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let input = input();
    let id = input.attempts[0].attempt_id;
    let run = store.create(&input, &call)?;
    let record = store.record(id, &call)?;
    let attempt = store.attempt_dir(run.run_id, id, false)?;
    let checkpoint = serde_json::json!({
        "schema_version": 1,
        "scope": RunStore::scope(&record),
    });
    crate::artifacts::seal(
        &attempt,
        "process.json",
        &serde_json::to_vec(&checkpoint)?,
        RECORD_LIMIT,
    )?;
    crate::artifacts::seal(&attempt, "stderr.log", b"view output", CAPTURE_LIMIT)?;

    // Mutation consumers read the checkpoint durably.
    crate::artifacts::sync_probe::reset();
    store.record(id, &call)?;
    assert!(crate::artifacts::sync_probe::count() >= 1);

    // Lane listings are a view: same checkpoint, no durable flush.
    crate::artifacts::sync_probe::reset();
    let page = store.lanes(
        &LaneQuery {
            targets: Vec::new(),
            all: true,
            limit: 10,
            after: 0,
            newest_first: false,
            owner: None,
            unseen_after: None,
        },
        &call,
    )?;
    assert_eq!(page.lanes.len(), 1);
    assert_eq!(crate::artifacts::sync_probe::count(), 0);

    // Output paging is a view too.
    crate::artifacts::sync_probe::reset();
    let chunk = store.read_output(
        id,
        ArtifactPart::Stderr,
        ByteRange::default(),
        &context,
        &call,
    )?;
    assert_eq!(chunk.bytes, b"view output");
    assert_eq!(crate::artifacts::sync_probe::count(), 0);
    Ok(())
}

pub(super) fn context(
    base: &Path,
) -> std::result::Result<ExecutionContext, Box<dyn std::error::Error>> {
    let base = std::fs::canonicalize(base)?;
    std::fs::create_dir_all(base.join("home"))?;
    std::fs::create_dir_all(base.join("checkout"))?;
    let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
    inputs.state_dir = Some(base.join("state"));
    Ok(ExecutionContext {
        state_root: inputs.resolve(&provenant_os::fs::SystemPathResolver)?.state,
        project_id: ProjectId::from_bytes([1; 16]),
        actor_id: ActorId::from_bytes([2; 16]),
        read_prompts: true,
    })
}
pub(super) fn clock() -> TestClock {
    TestClock::new(
        WallTime::from_micros(100).unwrap(),
        MonoTime::from_micros(0),
    )
}
pub(super) fn call<'a>(clock: &'a TestClock, cancel: &'a CancelToken) -> Request<'a> {
    Request { clock, cancel }
}
/// The provenance of a refused artifact. Any other outcome fails the test.
pub(super) fn artifact_provenance<T>(result: Result<T>) -> ArtifactProvenance {
    match result {
        Err(RunError::ArtifactMismatch(provenance)) => provenance,
        other => panic!("expected an artifact mismatch, got {:?}", other.err()),
    }
}
/// The 12-digit prefix a digest comparison reports, built without the code under test.
pub(super) fn digest_prefix(digest: impl std::fmt::Display) -> ArtifactValue {
    let text = digest.to_string();
    ArtifactValue::Digest(text.trim_start_matches("sha256:")[..12].to_string())
}
pub(super) fn input() -> PreparedRun {
    let task = TaskId::from_bytes([3; 16]);
    PreparedRun {
        run_id: RunId::from_bytes([4; 16]),
        operation_id: OperationId::parse("run-test").unwrap(),
        created_at: WallTime::from_micros(100).unwrap(),
        request: CanonicalRequest {
            extra: Default::default(),
            child_attempt_id_version: None,
            chair: None,
            fresh_continuation: None,
            schema_version: 1,
            tasks: vec![TaskRequest {
                context: None,
                extra: Default::default(),
                build_heavy: false,
                title: None,
                base: None,
                head: None,
                base_ref: None,
                writer: None,
                profile: HarnessProfile::default(),
                task_id: task,
                prompt_digest: CanonicalV1::file_digest(b"exact prompt"),
                route: RequestedRoute::Model {
                    extra: Default::default(),
                    provider: "fixture".into(),
                    model: "fixture/model".into(),
                    effort: Some("high".into()),
                },
                account: None,
                fallback: vec![],
                authority_digest: Digest::from_bytes([5; 32]),
                provider_timeout_seconds: 3600,
                delivery_id: None,
                deliver: None,
                access: Default::default(),
            }],
            concurrency: 1,
            parent: None,
            lane_depth: Some(1),
            parent_attempt: None,
        },
        attempts: vec![NewAttempt {
            task_id: task,
            attempt_id: AttemptId::from_bytes([6; 16]),
            prompt: "exact prompt".into(),
        }],
    }
}
pub(super) fn route() -> RouteRecord {
    RouteRecord {
        provider: Some("fixture".into()),
        model: Some("fixture/model".into()),
        effort: Some("high".into()),
        account_label: None,
        ..Default::default()
    }
}
pub(super) fn publication() -> TerminalPublication<'static> {
    TerminalPublication {
        outcome: Outcome::Succeeded,
        finished_at: WallTime::from_micros(200).unwrap(),
        started_at: Some(WallTime::from_micros(110).unwrap()),
        provider_ms: Some(1),
        reason: None,
        result: "sealed result ☀",
        complete: true,
        redacted: false,
        resolved_route: Some(route()),
        applied_route: Some(route()),
        observed_route: Some(route()),
        usage: UsageRecord::default(),
        session: None,
        partial_signals: Vec::new(),
    }
}
pub(super) fn exit() -> ExitObservation {
    ExitObservation {
        publication: PublicationState::Durable,
        scope: Scope {
            run: input().run_id,
            attempt: input().attempts[0].attempt_id,
            generation: 0,
        },
        termination: Some(Termination::Exit(0)),
        never_spawned: false,
        stop_reason: None,
        delivered_signals: vec![],
        reaped: vec![],
        omitted_reaped: 0,
        spared: vec![],
        omitted_spared: 0,
        warnings: vec![],
        omitted_warnings: 0,
        // Production guardians have no live frame decoder; delivery must not need one.
        terminal_observed: false,
        unresolved_cleanup: false,
        unresolved_pids: vec![],
        settlement_evidence: SettlementEvidence::DarwinCensusAndCustodyLease,
        stdout: CaptureTotals::default(),
        stderr: CaptureTotals::default(),
    }
}
#[test]
fn a_long_step_between_transactions_does_not_expire_later_store_writes() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let store = RunStore::open(&context, &call)?;
    // A provider launch or a bounded wait runs far past the allowance.
    clock.wait_until(MonoTime::from_micros(60_000_000))?;
    let input = input();
    store.create(&input, &call)?;
    assert!(store.operation(&input.operation_id, &call)?.is_some());
    Ok(())
}

#[test]
fn overview_reads_committed_wal_and_bounds_rows() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let writer = RunStore::open(&context, &call)?;
    let reader = RunStore::open_read_only(&context, &call)?.ok_or("missing reader")?;
    let mut first = input();
    first.request.tasks[0].title = Some("Review changes".into());
    writer.create(&first, &call)?;
    let batch = reader.overview_attempts(1, 1_048_576, &call)?;
    assert!(!batch.truncated);
    assert_eq!(batch.attempts.len(), 1);
    assert_eq!(batch.attempts[0].title.as_deref(), Some("Review changes"));
    assert_eq!(batch.attempts[0].state, OverviewState::Queued);
    assert_eq!(batch.attempts[0].project_id, context.project_id);
    let empty = reader.overview_attempts(0, 1_048_576, &call)?;
    assert!(empty.attempts.is_empty());
    assert!(empty.truncated);
    Ok(())
}

#[test]
fn overview_bounds_aggregate_record_bytes_before_decoding() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let writer = RunStore::open(&context, &call)?;
    for n in 1..=3 {
        let mut value = input();
        value.run_id = RunId::from_bytes([n; 16]);
        value.operation_id = OperationId::parse(&format!("run-{n}"))?;
        value.request.tasks[0].task_id = TaskId::from_bytes([n; 16]);
        value.attempts[0].task_id = value.request.tasks[0].task_id;
        value.attempts[0].attempt_id = AttemptId::from_bytes([n; 16]);
        value.request.tasks[0].title = Some("x".repeat(200));
        writer.create(&value, &call)?;
    }
    let mut statement = writer
        .db
        .prepare("SELECT length(record) FROM attempts ORDER BY sequence DESC")?;
    let lengths = statement
        .query_map([], |row| row.get::<_, u32>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let reader = RunStore::open_read_only(&context, &call)?.ok_or("missing reader")?;
    let batch = reader.overview_attempts(3, (lengths[0] + lengths[1] - 1) as usize, &call)?;
    assert_eq!(batch.attempts.len(), 1);
    assert_eq!(batch.bytes_read, lengths[0] as usize);
    assert!(batch.truncated);

    // Invalid JSON exposes any attempt to decode a row beyond the budget.
    writer.db.execute(
        "UPDATE attempts SET record=?1 WHERE sequence=(SELECT max(sequence) FROM attempts)",
        [vec![b'x'; 4096]],
    )?;
    let batch = reader.overview_attempts(3, 100, &call)?;
    assert!(batch.attempts.is_empty());
    assert_eq!(batch.bytes_read, 0);
    assert!(batch.truncated);
    Ok(())
}

#[test]
fn read_only_data_version_detects_a_new_queued_attempt() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let writer = RunStore::open(&context, &call)?;
    let reader = RunStore::open_disk_read_only(&context, &call)?.ok_or("missing reader")?;
    let before = reader.data_version(&call)?;
    assert_eq!(reader.data_version(&call)?, before);
    writer.create(&input(), &call)?;
    let after = reader.data_version(&call)?;
    assert_ne!(after, before);
    assert_eq!(reader.data_version(&call)?, after);
    Ok(())
}

#[test]
fn protected_review_retains_candidate_and_paths_without_delivery() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let mut input = input();
    input.request.tasks[0].writer = Some(WriterRequest {
        extra: Default::default(),
        project: temp.path().to_path_buf(),
        worktree: temp.path().join("lane"),
    });
    input.request.tasks[0].deliver = Some(WriterDeliveryRequest {
        extra: Default::default(),
        mode: Default::default(),
        remote: "origin".into(),
        branch: "published/lane".into(),
        open_pr: false,
    });
    store.create(&input, &call)?;
    seal_exit_fixture(&store, &input)?;
    let id = input.attempts[0].attempt_id;
    let head = provenant_core::GitOid::parse(&"a".repeat(40))?;
    let path = provenant_records::PathWire::from_relative(&provenant_core::RelativePath::parse(
        ".github/workflows/ci.yml",
    )?)?;
    let attempt = store.attempt_dir(input.run_id, id, false)?;
    artifacts::seal(
        &attempt,
        "writer-terminal.json",
        &db::bytes(&writer::WriterTerminal {
            schema_version: 1,
            head,
            protected_paths: vec![path.clone()],
        })?,
        RECORD_LIMIT,
    )?;
    let mut publication = publication();
    publication.outcome = Outcome::ReviewRequired;
    publication.reason = Some("protected changes require review".into());
    let mut incomplete = publication.clone();
    incomplete.complete = false;
    assert!(matches!(
        store.publish_terminal(id, 1, &incomplete, &exit(), &call),
        Err(RunError::Validation(_))
    ));
    let record = store.publish_terminal(id, 1, &publication, &exit(), &call)?;
    assert_eq!(record.head, Some(head));
    assert_eq!(record.protected_paths, vec![path]);
    assert!(record.delivery.is_none());
    assert!(matches!(
        record.state,
        AttemptState::Terminal {
            outcome: Outcome::ReviewRequired,
            ..
        }
    ));
    assert_eq!(store.record(id, &call)?, record);
    assert_eq!(
        serde_json::to_value(&record)?["state"]["outcome"],
        "review_required"
    );
    assert_eq!(
        store.attempt(id, &call)?.terminal_state,
        provenant_records::AttemptOutcome::ReviewRequired
    );
    Ok(())
}

#[test]
fn requested_delivery_that_never_ran_is_recorded_not_attempted() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let mut input = input();
    input.request.tasks[0].writer = Some(WriterRequest {
        extra: Default::default(),
        project: temp.path().to_path_buf(),
        worktree: temp.path().join("lane"),
    });
    input.request.tasks[0].deliver = Some(WriterDeliveryRequest {
        extra: Default::default(),
        mode: Default::default(),
        remote: "origin".into(),
        branch: "published/lane".into(),
        open_pr: false,
    });
    store.create(&input, &call)?;
    seal_exit_fixture(&store, &input)?;
    let id = input.attempts[0].attempt_id;
    let record = store.publish_terminal(id, 1, &publication(), &exit(), &call)?;
    let receipt = record.delivery.ok_or("delivery state missing")?;
    assert_eq!(
        receipt.state,
        WriterDeliveryState::NotAttempted {
            extra: Default::default()
        }
    );
    assert_eq!((receipt.remote.as_str(), receipt.head), ("origin", None));
    Ok(())
}
#[test]
fn database_initialisation_lock_preserves_wait_errors() -> TestResult {
    use provenant_os::fs::LockMode;
    use std::ffi::OsStr;

    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let store = RunStore::open(&context, &call(&clock, &cancel))?;
    let _held = store
        .root
        .try_lock(OsStr::new("run.sqlite3.lock"), LockMode::Exclusive)?;
    let request = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let contended =
        provenant_sqlite::with_busy_timeout(std::time::Duration::from_millis(50), || {
            matches!(
                db::open_database(context.state_root.as_path(), "run.sqlite3", &request),
                Err(RunError::Contention)
            )
        });
    cancel.cancel();
    let cancelled = matches!(
        db::open_database(context.state_root.as_path(), "run.sqlite3", &request),
        Err(RunError::Cancelled)
    );
    assert_eq!((contended, cancelled), (true, true));
    Ok(())
}

#[test]
fn reopening_a_current_store_does_not_wait_for_a_writer() -> TestResult {
    use provenant_os::clock::SystemClock;

    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let request = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let writer = RunStore::open(&context, &request)?;
    writer.db.execute_batch("BEGIN IMMEDIATE")?;
    let reader = RunStore::open(&context, &request)?;
    assert!(!reader.has_runs(&request)?);
    writer.db.execute_batch("ROLLBACK")?;
    Ok(())
}

#[test]
fn concurrent_first_opens_create_a_ready_store() -> TestResult {
    // Repeated fresh directories exercise both file creation and WAL setup.
    for _ in 0..16 {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let barrier = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            let workers = (0..8)
                .map(|_| {
                    scope.spawn(|| -> std::result::Result<(), String> {
                        let clock = clock();
                        let cancel = CancelToken::new();
                        let call = call(&clock, &cancel);
                        barrier.wait();
                        let store = RunStore::open(&context, &call)
                            .map_err(|error| format!("open: {error:?}"))?;
                        assert!(!store.has_runs(&call).map_err(|error| error.to_string())?);
                        Ok(())
                    })
                })
                .collect::<Vec<_>>();
            for worker in workers {
                worker.join().map_err(|_| "open worker panicked")??;
            }
            Ok::<_, Box<dyn std::error::Error>>(())
        })?;
    }
    Ok(())
}

#[test]
fn opens_refuse_unsafe_database_companions() -> TestResult {
    use provenant_os::fs::Directory;
    use std::os::unix::fs::{PermissionsExt, symlink};

    for suffix in ["", "-wal", "-shm", "-journal"] {
        for kind in ["symlink", "directory", "public"] {
            let temp = tempfile::tempdir()?;
            let context = context(temp.path())?;
            let clock = clock();
            let cancel = CancelToken::new();
            let request = call(&clock, &cancel);
            drop(RunStore::open(&context, &request)?);
            let root = context.state_root.as_path();
            let directory = Directory::open_anchor(root)?;
            let target = root.join("target");
            directory.create_new(std::ffi::OsStr::new("target"))?;
            let path = root.join(format!("run.sqlite3{suffix}"));
            if suffix.is_empty() {
                std::fs::remove_file(&path)?;
            }
            let created = match kind {
                "symlink" => symlink(&target, &path),
                "directory" => std::fs::create_dir(&path),
                _ => {
                    std::fs::write(&path, [])?;
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
                }
            };
            created.map_err(|error| format!("{suffix}/{kind}: {error}"))?;
            assert!(
                matches!(
                    RunStore::open(&context, &request),
                    Err(RunError::UnsafePath { .. })
                ),
                "{suffix}/{kind}"
            );
            assert!(
                matches!(
                    RunStore::open_read_only(&context, &request),
                    Err(RunError::UnsafePath { .. })
                ),
                "read-only {suffix}/{kind}"
            );
        }
    }
    Ok(())
}

#[test]
fn read_only_store_reports_whether_it_holds_a_run() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let writer = RunStore::open(&context, &call)?;
    let reader = RunStore::open_read_only(&context, &call)?.ok_or("initialised store missing")?;
    assert!(!reader.has_runs(&call)?);
    writer.create(&input(), &call)?;
    assert!(reader.has_runs(&call)?);
    Ok(())
}

#[test]
fn historical_task_class_preserves_request_verification_and_lane_listing() -> TestResult {
    for task_class in [serde_json::Value::Null, serde_json::json!("mechanical")] {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context, &call)?;
        let mut input = input();
        let mut sealed = serde_json::to_value(&input.request)?;
        sealed["tasks"][0]["profile"] = serde_json::json!({
            "name": "", "knobs": {}, "overrides": [], "task_class": task_class
        });
        let digest = CanonicalV1::record_digest("run-request", 1, &sealed)?;
        input.request = serde_json::from_value(sealed.clone())?;
        store.create(&input, &call)?;
        assert_eq!(store.run(input.run_id, &call)?.request_digest, digest);
        assert_eq!(
            serde_json::to_value(store.request(input.run_id, &call)?)?,
            sealed
        );
        let page = store.lanes(
            &LaneQuery {
                targets: Vec::new(),
                all: true,
                limit: 10,
                after: 0,
                newest_first: false,
                owner: None,
                unseen_after: None,
            },
            &call,
        )?;
        assert_eq!(page.lanes.len(), 1);
        assert_eq!(
            serde_json::to_value(&page.lanes[0].profile)?,
            sealed["tasks"][0]["profile"]
        );
    }
    Ok(())
}

#[test]
fn missing_output_is_not_found_only_after_terminal() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let input = input();
    let id = input.attempts[0].attempt_id;
    store.create(&input, &call)?;
    for part in [ArtifactPart::Result, ArtifactPart::Stderr] {
        assert!(matches!(
            store.read_output(id, part, ByteRange::default(), &context, &call),
            Err(RunError::Nonterminal)
        ));
    }
    store.withdraw(
        id,
        AttemptState::Queued {
            extra: Default::default(),
        },
        Outcome::Rejected,
        "fixture rejection",
        WallTime::from_micros(200)?,
        &call,
    )?;
    assert!(matches!(
        store.read_output(
            id,
            ArtifactPart::Stderr,
            ByteRange::default(),
            &context,
            &call
        ),
        Err(RunError::NotFound)
    ));
    assert!(
        store
            .read_output(
                id,
                ArtifactPart::Result,
                ByteRange::default(),
                &context,
                &call
            )?
            .bytes
            .is_empty()
    );
    Ok(())
}

#[test]
fn streamed_result_is_verified_and_redacted_across_chunks() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let input = input();
    let id = input.attempts[0].attempt_id;
    store.create(&input, &call)?;
    seal_exit_fixture(&store, &input)?;
    let secret = format!("ghp_{}", "a".repeat(36));
    let text = format!("{} {}\n{}", "x".repeat(8189), secret, "z".repeat(1000));
    let mut done = publication();
    done.result = &text;
    store.publish_terminal(id, 1, &done, &exit(), &call)?;
    let result = store.stream_result(id, &call, None)?;
    assert_eq!(
        result.digest,
        CanonicalV1::file_digest(text.as_bytes()).to_string()
    );
    assert!(result.source_truncated);
    assert!(!result.tail.contains(&secret));
    assert!(result.tail.contains("[REDACTED]"));
    let path = context
        .state_root
        .as_path()
        .join(format!("runs/{}/attempts/{id}/result.md", input.run_id));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
        let expected =
            std::path::PathBuf::from(format!("runs/{}/attempts/{id}/result.md", input.run_id));
        assert!(
            matches!(store.stream_result(id, &call, None), Err(RunError::UnsafePath { path, check: PathCheck::Mode }) if path == expected)
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::write(&path, vec![b'x'; text.len() + 1])?;
    assert_eq!(
        artifact_provenance(store.stream_result(id, &call, None)),
        ArtifactProvenance {
            check: ArtifactCheck::RecordedLength,
            artifact: "result.md".into(),
            expected: Some(ArtifactValue::Length(text.len() as u64)),
            actual: Some(ArtifactValue::Length(text.len() as u64 + 1)),
        }
    );
    std::fs::write(path, vec![b'x'; text.len()])?;
    assert_eq!(
        artifact_provenance(store.stream_result(id, &call, None)),
        ArtifactProvenance {
            check: ArtifactCheck::RecordedDigest,
            artifact: "result.md".into(),
            expected: Some(digest_prefix(CanonicalV1::file_digest(text.as_bytes()))),
            actual: Some(digest_prefix(CanonicalV1::file_digest(&vec![
                b'x';
                text.len()
            ]))),
        }
    );
    Ok(())
}

#[test]
fn appended_after_eof_names_the_recorded_and_final_lengths() -> TestResult {
    use std::io::Write as _;
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let input = input();
    let id = input.attempts[0].attempt_id;
    store.create(&input, &call)?;
    seal_exit_fixture(&store, &input)?;
    let text = "sealed result";
    let mut done = publication();
    done.result = text;
    store.publish_terminal(id, 1, &done, &exit(), &call)?;
    let path = context
        .state_root
        .as_path()
        .join(format!("runs/{}/attempts/{id}/result.md", input.run_id));
    // Grow the file through a second handle between the streamed EOF and the
    // final metadata check, so the streamed length and digest still match the
    // record and only the final unchanged check can refuse.
    let provenance = artifact_provenance(store.stream_result_observing(id, &call, None, || {
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .and_then(|mut handle| handle.write_all(b"appended"))
            .unwrap();
    }));
    assert_eq!(
        provenance,
        ArtifactProvenance {
            check: ArtifactCheck::Stream,
            artifact: "result.md".into(),
            expected: Some(ArtifactValue::Length(text.len() as u64)),
            actual: Some(ArtifactValue::Length(text.len() as u64 + 8)),
        }
    );
    let message = provenance.to_string();
    assert!(message.contains(text.len().to_string().as_str()));
    assert!(message.contains((text.len() as u64 + 8).to_string().as_str()));
    Ok(())
}

#[test]
fn tampered_request_names_the_request_digest_that_failed() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let input = input();
    store.create(&input, &call)?;
    let stored = store.run(input.run_id, &call)?.request_digest;
    let path = context
        .state_root
        .as_path()
        .join(format!("runs/{}/request.json", input.run_id));
    let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    value["concurrency"] = 99.into();
    let recomputed = serde_json::from_value::<CanonicalRequest>(value.clone())?.digest()?;
    std::fs::write(&path, serde_json::to_vec(&value)?)?;
    assert_eq!(
        artifact_provenance(store.request(input.run_id, &call)),
        ArtifactProvenance {
            check: ArtifactCheck::RequestDigest,
            artifact: "request.json".into(),
            expected: Some(digest_prefix(stored)),
            actual: Some(digest_prefix(recomputed)),
        }
    );
    Ok(())
}

#[test]
fn streamed_handoff_redacts_decoded_strings_before_graph_continuation() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let input = input();
    let id = input.attempts[0].attempt_id;
    store.create(&input, &call)?;
    seal_exit_fixture(&store, &input)?;
    let secret = "sk-abcdefghijklmnopqrstuvwx";
    let handoff = serde_json::json!({"schema_version":2,"status":"stopped_early","note":format!("Continue.\n{secret}"),"remaining":[format!("\t{secret}")],"decisions":[],"verify":[],"future":{"token":format!("\n{secret}")}});
    let text = format!("{}\n```handoff\n{handoff}\n```", "x".repeat(28_000));
    let mut done = publication();
    done.result = &text;
    store.publish_terminal(id, 1, &done, &exit(), &call)?;
    let result = store.stream_result(id, &call, None)?;
    let handoff = result
        .handoff
        .map_err(std::io::Error::other)?
        .ok_or("streamed handoff")?;
    assert!(!handoff.fenced().contains(secret));
    assert_eq!(handoff.note, "Continue.\n[REDACTED]");
    assert_eq!(handoff.remaining, ["\t[REDACTED]"]);
    assert_eq!(handoff.extra["future"]["token"], "\n[REDACTED]");
    Ok(())
}

#[test]
fn streamed_verdict_evidence_validates_complete_large_artifacts() -> TestResult {
    for duplicate in [false, true] {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context, &call)?;
        let input = input();
        let id = input.attempts[0].attempt_id;
        store.create(&input, &call)?;
        seal_exit_fixture(&store, &input)?;
        let text = format!(
            "{}\n{}{}\nVERDICT: PASS",
            "x".repeat(5000),
            if duplicate { "VERDICT: FAIL\n" } else { "" },
            "y".repeat(20000)
        );
        let mut done = publication();
        done.result = &text;
        store.publish_terminal(id, 1, &done, &exit(), &call)?;
        let captured = store.stream_result(id, &call, None)?;
        assert!(captured.source_truncated);
        assert!(!captured.excerpt.contains("VERDICT: FAIL"));
        assert!(!captured.tail.contains("VERDICT: FAIL"));
        assert_eq!(
            captured.verdict.as_deref(),
            if duplicate {
                None
            } else {
                Some("VERDICT: PASS")
            }
        );
        assert_eq!(captured.verdict_invalid, duplicate);
    }
    Ok(())
}

#[test]
fn transcript_part_reads_the_redacted_tail_of_the_provider_transcript() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let input = input();
    let id = input.attempts[0].attempt_id;
    let run = store.create(&input, &call)?;
    seal_exit_fixture(&store, &input)?;
    let mut done = publication();
    done.session = Some("sess-1848".into());
    store.publish_terminal(id, 1, &done, &exit(), &call)?;
    let attempt = context
        .state_root
        .as_path()
        .join(format!("runs/{}/attempts/{id}", run.run_id));
    let dir = attempt.join("provider-state/candidate-0/claude-config/projects/key");
    std::fs::create_dir_all(&dir)?;
    let secret = format!("ghp_{}", "a".repeat(36));
    std::fs::write(
        dir.join("sess-1848.jsonl"),
        format!("{{\"n\":1}}\n{{\"token\":\"{secret}\"}}\n"),
    )?;
    let chunk = store.read_output(
        id,
        ArtifactPart::Transcript,
        ByteRange::default(),
        &context,
        &call,
    )?;
    let text = String::from_utf8(chunk.bytes)?;
    assert!(text.contains("\"n\":1"));
    assert!(!text.contains(&secret));
    let fact = store.attempt(id, &call)?;
    let transcript = fact.transcript.ok_or("missing transcript locator")?;
    assert_eq!(transcript.adapter, "fixture");
    assert_eq!(transcript.session, "sess-1848");
    Ok(())
}

#[test]
fn created_attempt_and_event_carry_the_host_and_submitting_parent() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let host = provenant_core::HostId::from_bytes([8; 16]);
    let store = RunStore::open(&context, &call)?.with_host(host);
    let parent = AttemptId::from_bytes([9; 16]);
    let mut input = input();
    input.request.parent_attempt = Some(parent);
    store.create(&input, &call)?;
    let id = input.attempts[0].attempt_id;
    let record = store.record(id, &call)?;
    assert_eq!(record.trace.host, Some(host));
    assert_eq!(record.trace.parent, Some(parent));
    let events = store.events(0, 64, Some(id), &call)?;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].trace, record.trace);
    Ok(())
}

#[test]
fn create_publish_and_read_exact_bounded_facts() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let mut input = input();
    let work = provenant_records::run::WorkContext {
        work_item: Some("issue-42".into()),
        role: Some("reviewer".into()),
        ..Default::default()
    };
    input.request.tasks[0].context = Some(work.clone());
    // A schema-v1 request without a profile must keep its sealed digest.
    let mut unstamped = serde_json::to_value(&input.request)?;
    for task in unstamped["tasks"].as_array_mut().ok_or("tasks")? {
        task.as_object_mut().ok_or("task")?.remove("profile");
    }
    let decoded: CanonicalRequest = serde_json::from_value(unstamped.clone())?;
    assert_eq!(
        decoded.digest()?,
        CanonicalV1::record_digest("run-request", 1, &unstamped)?
    );
    let id = input.attempts[0].attempt_id;
    let run = store.create(&input, &call)?;
    assert_eq!(admission_count(&store)?, 1);
    seal_exit_fixture(&store, &input)?;
    assert_eq!(run.closure, Closure::Open);
    assert!(matches!(
        store.attempt(id, &call),
        Err(RunError::Nonterminal)
    ));
    assert_eq!(store.create(&input, &call)?.run_id, run.run_id);
    let mut changed = self::input();
    changed.request.concurrency = 2;
    assert!(matches!(
        store.create(&changed, &call),
        Err(RunError::OperationConflict)
    ));
    assert_eq!(
        store
            .read_output(
                id,
                ArtifactPart::Prompt,
                ByteRange::default(),
                &context,
                &call
            )?
            .bytes,
        b"exact prompt"
    );
    let mut wrong_route = publication();
    wrong_route
        .resolved_route
        .as_mut()
        .ok_or("missing fixture route")?
        .model = Some("different/model".into());
    assert!(matches!(
        store.publish_terminal(id, 1, &wrong_route, &exit(), &call),
        Err(RunError::Validation(_))
    ));
    let record = store.publish_terminal(id, 1, &publication(), &exit(), &call)?;
    assert_eq!(admission_count(&store)?, 0);
    assert_eq!(record.provider_launched, Some(true));
    assert_eq!(
        store.publish_terminal(id, 1, &publication(), &exit(), &call)?,
        record
    );
    assert_eq!(store.run(run.run_id, &call)?.closure, Closure::Closed);
    assert_eq!(
        store.operation(&input.operation_id, &call)?.unwrap().state,
        OperationState::Terminal
    );
    let output = store.read_output(
        id,
        ArtifactPart::Result,
        ByteRange {
            offset: 7,
            max_bytes: 6,
        },
        &context,
        &call,
    )?;
    assert_eq!(output.bytes, b"result");
    assert!(output.clipped);
    assert!(!output.eof);
    assert_eq!(output.next_offset, 13);
    let all = store.read_output(
        id,
        ArtifactPart::Result,
        ByteRange::default(),
        &context,
        &call,
    )?;
    assert!(all.eof && all.complete);
    assert_eq!(all.bytes, publication().result.as_bytes());
    let fact = store.attempt(id, &call)?;
    assert_eq!(
        store.request(run.run_id, &call)?.tasks[0].context,
        Some(work.clone())
    );
    assert_eq!(fact.context, Some(work));
    assert_eq!(fact.output_refs[0].digest, all.digest);
    assert_eq!(fact.provider_ms, Some(1));
    let reviewer = fact
        .reviewer
        .as_ref()
        .expect("reviewer from observed route");
    assert_eq!(
        reviewer.provenance,
        provenant_records::ReviewerProvenance::Observed
    );
    // The adapter name is not a model family, and the submitter is not the reviewer.
    assert_eq!(reviewer.family, None);
    assert_ne!(reviewer.actor_id, context.actor_id);
    assert_eq!(fact.verified_owner, Some(context.actor_id));
    assert_eq!(
        store
            .attempts(AttemptFilter::default(), &call)?
            .collect::<Result<Vec<_>>>()?,
        vec![fact]
    );
    let events = store.events(0, 64, None, &call)?;
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].revision, record.revision);
    // The edge supplied no host, so only that field stays unknown.
    let trace = provenant_records::Trace {
        extra: Default::default(),
        host: None,
        project: Some(context.project_id),
        run: Some(run.run_id),
        attempt: Some(id),
        operation: Some(input.operation_id.clone()),
        actor: Some(context.actor_id),
        parent: None,
    };
    assert_eq!(record.trace, trace);
    assert_eq!(store.attempt(id, &call)?.trace, trace);
    assert!(events.iter().all(|event| event.trace == trace));
    let mut replacement = publication();
    replacement.result = "different";
    assert!(matches!(
        store.publish_terminal(id, 1, &replacement, &exit(), &call),
        Err(RunError::Conflict)
    ));
    let mut denied = context.clone();
    denied.read_prompts = false;
    assert!(matches!(
        store.read_output(
            id,
            ArtifactPart::Prompt,
            ByteRange::default(),
            &denied,
            &call
        ),
        Err(RunError::Refused)
    ));
    let mut foreign = context.clone();
    foreign.project_id = ProjectId::from_bytes([9; 16]);
    assert!(matches!(
        RunStore::open(&foreign, &call)?.attempt(id, &call),
        Err(RunError::NotFound)
    ));
    drop(store);
    assert_eq!(
        RunStore::open(&context, &call)?
            .attempts(AttemptFilter::default(), &call)?
            .count(),
        1
    );
    Ok(())
}

#[test]
fn lanes_read_last_output_time_from_supervisor_checkpoint() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let input = input();
    store.create(&input, &call)?;
    let expected = WallTime::parse("2026-10-25T00:00:00.000000Z")?;
    let directory = store.attempt_dir(input.run_id, input.attempts[0].attempt_id, true)?;
    artifacts::seal(
        &directory,
        "process.json",
        &serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "scope": {"run": input.run_id, "attempt": input.attempts[0].attempt_id, "generation": 0},
            "last_output_at": expected,
            "priority": 15,
            "publication_pending": false
        }))?,
        RECORD_LIMIT,
    )?;
    assert_eq!(
        store
            .record(input.attempts[0].attempt_id, &call)?
            .last_output_at,
        Some(expected)
    );
    assert_eq!(
        store.record(input.attempts[0].attempt_id, &call)?.priority,
        Some(15)
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn read_only_open_works_without_directory_write_access_or_creating_files() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let writer = RunStore::open(&context, &call)?;

    let state = context.state_root.as_path();
    let entries_before = std::fs::read_dir(state)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<std::collections::BTreeSet<_>>>()?;
    let database = state.join("run.sqlite3");
    std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o400))?;
    std::fs::set_permissions(state, std::fs::Permissions::from_mode(0o500))?;

    let store = RunStore::open_read_only(&context, &call)?.ok_or("initialised store missing")?;
    assert_eq!(store.attempts(AttemptFilter::default(), &call)?.count(), 0);
    drop(store);

    std::fs::set_permissions(state, std::fs::Permissions::from_mode(0o700))?;
    std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o600))?;
    let entries_after = std::fs::read_dir(state)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<std::collections::BTreeSet<_>>>()?;
    assert_eq!(entries_after, entries_before);
    drop(writer);
    Ok(())
}

#[test]
fn read_only_open_treats_missing_store_and_schema_as_empty() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);

    assert!(RunStore::open_read_only(&context, &call)?.is_none());
    assert!(!context.state_root.as_path().exists());

    std::fs::create_dir_all(context.state_root.as_path())?;
    std::fs::set_permissions(
        context.state_root.as_path(),
        std::fs::Permissions::from_mode(0o700),
    )?;
    let database = context.state_root.as_path().join("run.sqlite3");
    std::fs::write(&database, [])?;
    std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o600))?;
    assert!(RunStore::open_read_only(&context, &call)?.is_none());
    assert_eq!(std::fs::read_dir(context.state_root.as_path())?.count(), 1);
    Ok(())
}
#[test]
fn oversize_is_refused_before_publication_or_read() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let input = input();
    let id = input.attempts[0].attempt_id;
    store.create(&input, &call)?;
    seal_exit_fixture(&store, &input)?;
    assert!(matches!(
        store.read_output(
            id,
            ArtifactPart::Prompt,
            ByteRange {
                offset: 0,
                max_bytes: OUTPUT_LIMIT + 1
            },
            &context,
            &call
        ),
        Err(RunError::TooLarge { .. })
    ));
    let huge = "x".repeat(RESULT_LIMIT as usize + 1);
    let mut publication = publication();
    publication.result = &huge;
    assert!(matches!(
        store.publish_terminal(id, 1, &publication, &exit(), &call),
        Err(RunError::TooLarge { .. })
    ));
    assert!(matches!(
        store.attempt(id, &call),
        Err(RunError::Nonterminal)
    ));
    publication.result = "complete";
    store.publish_terminal(id, 1, &publication, &exit(), &call)?;
    let path = context
        .state_root
        .as_path()
        .join(format!("runs/{}/attempts/{id}/result.md", input.run_id));
    std::fs::write(path, &huge)?;
    assert!(matches!(
        store.read_output(
            id,
            ArtifactPart::Result,
            ByteRange::default(),
            &context,
            &call
        ),
        Err(RunError::Filesystem(
            provenant_os::fs::FsError::TooLarge { .. }
        ))
    ));
    Ok(())
}
#[test]
fn contained_artifact_refuses_symlink_escape_for_reads_and_writes() -> TestResult {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let input = input();
    let id = input.attempts[0].attempt_id;
    store.create(&input, &call)?;
    seal_exit_fixture(&store, &input)?;
    let attempt = context
        .state_root
        .as_path()
        .join(format!("runs/{}/attempts/{id}", input.run_id));
    let outside = temp.path().join("outside");
    std::fs::create_dir(&outside)?;
    std::fs::write(outside.join("prompt.txt"), "credential sentinel")?;
    std::fs::rename(&attempt, attempt.with_extension("held"))?;
    symlink(&outside, &attempt)?;
    assert!(
        store
            .read_output(
                id,
                ArtifactPart::Prompt,
                ByteRange::default(),
                &context,
                &call
            )
            .is_err()
    );
    assert!(
        store
            .publish_terminal(id, 1, &publication(), &exit(), &call)
            .is_err()
    );
    assert!(!outside.join("result.md").exists());
    assert_eq!(
        std::fs::read(outside.join("prompt.txt"))?,
        b"credential sentinel"
    );
    Ok(())
}
pub(crate) fn crash_at(phase: &str) {
    let wanted = std::env::var("PROVENANT_RUN_TEST_CRASH").unwrap_or_default();
    if wanted.strip_prefix("withdraw-").unwrap_or(&wanted) == phase {
        std::process::exit(73);
    }
}
#[test]
fn publication_crash_worker() -> TestResult {
    let Ok(path) = std::env::var("PROVENANT_RUN_TEST_ROOT") else {
        return Ok(());
    };
    let context = context(Path::new(&path))?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let input = input();
    store.create(&input, &call)?;
    assert_eq!(admission_count(&store)?, 1);
    let crash = std::env::var("PROVENANT_RUN_TEST_CRASH").unwrap_or_default();
    if crash.starts_with("withdraw-") {
        store.withdraw(
            input.attempts[0].attempt_id,
            AttemptState::Queued {
                extra: Default::default(),
            },
            Outcome::Cancelled,
            "cancelled before the crash",
            WallTime::from_micros(150)?,
            &call,
        )?;
        return Err("crash phase was not reached".into());
    }
    seal_exit_fixture(&store, &input)?;
    store.publish_terminal(
        input.attempts[0].attempt_id,
        1,
        &publication(),
        &exit(),
        &call,
    )?;
    Err("crash phase was not reached".into())
}
#[test]
fn publication_crashes_preserve_committed_records_and_allow_retry() -> TestResult {
    let executor = SystemExecutor::new(1)?;
    for phase in [
        "reserved",
        "result",
        "sealed",
        "transaction",
        "committed",
        "withdraw-sealed",
    ] {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let argv: Vec<OsString> = vec![
            std::env::current_exe()?.into_os_string(),
            "--exact".into(),
            "tests::publication_crash_worker".into(),
        ];
        let env: Vec<(OsString, OsString)> = [
            ("HOME", temp.path().join("home").into_os_string()),
            (
                "PROVENANT_STATE_DIR",
                context.state_root.as_path().as_os_str().to_owned(),
            ),
            (
                "XDG_CONFIG_HOME",
                temp.path().join("config").into_os_string(),
            ),
            (
                "CLAUDE_CONFIG_DIR",
                temp.path().join("claude").into_os_string(),
            ),
            ("CODEX_HOME", temp.path().join("codex").into_os_string()),
            (
                "PROVENANT_RUN_TEST_ROOT",
                temp.path().as_os_str().to_owned(),
            ),
            ("PROVENANT_RUN_TEST_CRASH", phase.into()),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v))
        .collect();
        let mut options = ExecOptions::new(&argv, temp.path(), Duration::from_secs(10));
        options.env = Some(&env);
        options.stdout = StreamPolicy::Capture { limit: 8192 };
        options.stderr = StreamPolicy::Capture { limit: 8192 };
        let output = executor.execute(options)?;
        assert_eq!(
            output.termination,
            Termination::Exit(73),
            "{}",
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        let clock = clock();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context, &call)?;
        let input = input();
        let id = input.attempts[0].attempt_id;
        assert_eq!(
            store.record(id, &call)?.prompt.digest,
            input.request.tasks[0].prompt_digest
        );
        if phase != "committed" {
            assert!(matches!(
                store.attempt(id, &call),
                Err(RunError::Nonterminal)
            ));
        } else {
            assert!(store.attempt(id, &call).is_ok());
        }
        if phase == "reserved" {
            // Queued rows exist, but request.json is unsealed: not launchable.
            assert!(store.queued_runs(&call)?.is_empty());
        }
        // A replay mints fresh IDs; it must land on the reserved ones.
        let mut replay = self::input();
        replay.run_id = RunId::from_bytes([9; 16]);
        replay.attempts[0].attempt_id = AttemptId::from_bytes([10; 16]);
        assert_eq!(store.create(&replay, &call)?.run_id, input.run_id);
        if phase == "reserved" {
            assert_eq!(store.queued_runs(&call)?, vec![input.run_id]);
        }
        seal_exit_fixture(&store, &input)?;
        if matches!(phase, "sealed" | "transaction") {
            assert_eq!(admission_count(&store)?, 1);
            let mut recomputed = publication();
            recomputed.result = "different text after restart";
            assert!(matches!(
                store.publish_terminal(id, 1, &recomputed, &exit(), &call),
                Err(RunError::TerminalRecoveryRequired)
            ));
            store.recover_terminal(id, &exit(), &call)?;
            assert_eq!(admission_count(&store)?, 0);
        } else if phase == "withdraw-sealed" {
            // The retry renders a new time and reason; the sealed intent wins.
            let record = store
                .withdraw(
                    id,
                    AttemptState::Queued {
                        extra: Default::default(),
                    },
                    Outcome::Cancelled,
                    "retried",
                    WallTime::from_micros(900)?,
                    &call,
                )?
                .ok_or("withdrawal was already terminal")?;
            let sealed = WallTime::from_micros(150)?;
            assert!(matches!(
                record.state,
                AttemptState::Terminal { finished_at, .. } if finished_at == sealed
            ));
            assert_eq!(admission_count(&store)?, 0);
        } else if phase == "result" {
            assert_eq!(admission_count(&store)?, 1);
            let mut recomputed = publication();
            recomputed.result = "different text after restart";
            store.recover_result_only(id, &recomputed, &exit(), &call)?;
            assert_eq!(admission_count(&store)?, 0);
        } else {
            store.publish_terminal(id, 1, &publication(), &exit(), &call)?;
        }
        assert_eq!(store.events(0, 64, None, &call)?.len(), 2);
        assert!(
            store
                .read_output(
                    id,
                    ArtifactPart::Result,
                    ByteRange::default(),
                    &context,
                    &call
                )?
                .complete
        );
    }
    Ok(())
}

#[test]
fn result_orphan_redaction_flag_describes_the_preserved_bytes() -> TestResult {
    for (text, redacted) in [
        ("clean orphan", true),
        ("ghp_abcdefghijklmnopqrstuvwxyz0123456789", false),
    ] {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context, &call)?;
        let input = input();
        let id = input.attempts[0].attempt_id;
        store.create(&input, &call)?;
        seal_exit_fixture(&store, &input)?;
        let dir = store.attempt_dir(input.run_id, id, false)?;
        super::artifacts::seal(&dir, "result.md", text.as_bytes(), RESULT_LIMIT)?;
        let mut facts = publication();
        facts.redacted = true;
        let record = store.recover_result_only(id, &facts, &exit(), &call)?;
        let artifact = record.result.ok_or("result")?;
        assert_eq!(artifact.redacted, redacted);
        assert_eq!(artifact.digest, CanonicalV1::file_digest(text.as_bytes()));
        assert_eq!(
            super::artifacts::read_sealed(&dir, "result.md", RESULT_LIMIT)?.as_deref(),
            Some(text.as_bytes())
        );
    }
    Ok(())
}

#[test]
fn result_only_orphan_is_not_poisoned_by_different_recomputed_text() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let input = input();
    let id = input.attempts[0].attempt_id;
    store.create(&input, &call)?;
    seal_exit_fixture(&store, &input)?;
    super::artifacts::seal(
        &store.attempt_dir(input.run_id, id, false)?,
        "result.md",
        publication().result.as_bytes(),
        RESULT_LIMIT,
    )?;
    let mut recomputed = publication();
    recomputed.result = "different text after restart";
    assert_eq!(
        artifact_provenance(store.publish_terminal(id, 1, &recomputed, &exit(), &call)),
        ArtifactProvenance {
            check: ArtifactCheck::SealConflict,
            artifact: "result.md".into(),
            expected: Some(digest_prefix(CanonicalV1::file_digest(
                publication().result.as_bytes()
            ))),
            actual: Some(digest_prefix(CanonicalV1::file_digest(
                recomputed.result.as_bytes()
            ))),
        }
    );
    store.recover_result_only(id, &recomputed, &exit(), &call)?;
    assert_eq!(store.run(input.run_id, &call)?.closure, Closure::Closed);
    assert_eq!(
        store
            .read_output(
                id,
                ArtifactPart::Result,
                ByteRange::default(),
                &context,
                &call
            )?
            .bytes,
        publication().result.as_bytes()
    );
    Ok(())
}

#[test]
fn full_database_retains_admission_and_recovers_original_terminal_intent() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let input = input();
    let id = input.attempts[0].attempt_id;
    store.create(&input, &call)?;
    assert_eq!(admission_count(&store)?, 1);
    seal_exit_fixture(&store, &input)?;
    let pages: i64 = store
        .db
        .pragma_query_value(None, "page_count", |row| row.get(0))?;
    store.db.pragma_update(None, "max_page_count", pages)?;
    let mut publication = publication();
    publication.reason = Some("x".repeat(128 * 1024));
    assert!(matches!(
        store.publish_terminal(id, 1, &publication, &exit(), &call),
        Err(RunError::Sqlite(rusqlite::Error::SqliteFailure(code, _)))
            if code.code == rusqlite::ErrorCode::DiskFull
    ));
    assert_eq!(admission_count(&store)?, 1);
    assert!(matches!(
        store.attempt(id, &call),
        Err(RunError::Nonterminal)
    ));
    assert_eq!(store.events(0, 64, None, &call)?.len(), 1);
    assert_eq!(store.run(input.run_id, &call)?.closure, Closure::Open);
    store.db.pragma_update(None, "max_page_count", 10000)?;
    store.recover_terminal(id, &exit(), &call)?;
    assert_eq!(admission_count(&store)?, 0);
    assert_eq!(store.events(0, 64, None, &call)?.len(), 2);
    assert_eq!(store.run(input.run_id, &call)?.closure, Closure::Closed);
    assert_eq!(
        store
            .read_output(
                id,
                ArtifactPart::Result,
                ByteRange::default(),
                &context,
                &call
            )?
            .bytes,
        publication.result.as_bytes()
    );
    Ok(())
}

fn seal_exit_fixture(store: &RunStore, input: &PreparedRun) -> TestResult {
    seal_observation(store, input, &exit())
}
pub(super) fn seal_observation(
    store: &RunStore,
    input: &PreparedRun,
    observation: &ExitObservation,
) -> TestResult {
    let mut value = serde_json::to_value(observation)?;
    value
        .as_object_mut()
        .ok_or("exit fixture")?
        .remove("publication");
    value
        .as_object_mut()
        .ok_or("exit fixture")?
        .insert("schema_version".into(), 1.into());
    let dir = store.attempt_dir(input.run_id, input.attempts[0].attempt_id, false)?;
    super::artifacts::seal(
        &dir,
        "process-exit.json",
        &serde_json::to_vec(&value)?,
        RECORD_LIMIT,
    )?;
    Ok(())
}

fn admission_count(store: &RunStore) -> std::result::Result<i64, rusqlite::Error> {
    store
        .db
        .query_row("SELECT COUNT(*) FROM admission", [], |row| row.get(0))
}

#[test]
fn prelaunch_refusal_publishes_durable_never_launched_evidence() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let input = input();
    let id = input.attempts[0].attempt_id;
    store.create(&input, &call)?;
    let mut observation = exit();
    observation.never_spawned = true;
    observation.termination = None;
    seal_observation(&store, &input, &observation)?;
    let mut publication = publication();
    publication.outcome = Outcome::QuotaLimited;
    publication.started_at = None;
    publication.provider_ms = None;
    let record = store.publish_terminal(id, 1, &publication, &observation, &call)?;
    assert_eq!(record.provider_launched, Some(false));
    assert_eq!(store.record(id, &call)?.provider_launched, Some(false));
    Ok(())
}

#[test]
fn the_store_initialises_at_version_six_without_spacing() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let version: i64 =
        store
            .db
            .query_row("SELECT user_version FROM pragma_user_version", [], |r| {
                r.get(0)
            })?;
    assert_eq!(version, 6);
    let spacing: i64 = store.db.query_row(
        "SELECT COUNT(*) FROM sqlite_schema WHERE name='spacing'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(spacing, 0);
    Ok(())
}

#[test]
fn temporary_tracking_upgrade_preserves_version_one_cancel_binding() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    // Shape the store exactly as version 1 left it.
    RunStore::open(&context, &call)?
        .db
        .execute_batch(
            "DROP INDEX reaction_pending; DROP TABLE reactions; DROP INDEX retained_temporary; ALTER TABLE attempts DROP COLUMN tmp_retained; DROP TABLE cancellations; CREATE TABLE spacing (id INTEGER PRIMARY KEY CHECK(id=1), last_start INTEGER NOT NULL) STRICT; PRAGMA user_version=1;",
        )?;
    let store = RunStore::open(&context, &call)?;
    let op = OperationId::parse("stop")?;
    store.bind_cancel(&op, "attempt", &call)?;
    assert!(matches!(
        store.bind_cancel(&op, "other", &call),
        Err(RunError::OperationConflict)
    ));
    Ok(())
}

#[test]
fn temporary_tracking_upgrade_drops_version_two_spacing() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    // Shape the store exactly as version 2 left it, with the unused table back.
    RunStore::open(&context, &call)?.db.execute_batch(
        "DROP INDEX reaction_pending; DROP TABLE reactions; DROP INDEX retained_temporary; ALTER TABLE attempts DROP COLUMN tmp_retained; CREATE TABLE spacing (id INTEGER PRIMARY KEY CHECK(id=1), last_start INTEGER NOT NULL) STRICT; PRAGMA user_version=2;",
    )?;
    let store = RunStore::open(&context, &call)?;
    let remaining: i64 = store.db.query_row(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name='spacing'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(remaining, 0);
    let version: i64 =
        store
            .db
            .query_row("SELECT user_version FROM pragma_user_version", [], |r| {
                r.get(0)
            })?;
    assert_eq!(version, 6);
    Ok(())
}

#[test]
fn the_store_refuses_a_version_newer_than_six() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    RunStore::open(&context, &call)?
        .db
        .execute_batch("PRAGMA user_version=7;")?;
    assert!(matches!(
        RunStore::open(&context, &call),
        Err(RunError::Version(7))
    ));
    Ok(())
}

#[test]
fn version_five_lets_one_run_continue_several_targets() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let input = input();
    let store = RunStore::open(&context, &call)?;
    store.create(&input, &call)?;
    let first = input.attempts[0].attempt_id.to_string();
    let run = input.run_id.to_string();
    // Shape the table as version 4 left it, with one row to carry over.
    store.db.execute_batch(&format!(
        "DROP INDEX reaction_pending; DROP TABLE reactions; DROP INDEX continuation_runs; DROP TABLE continuations; CREATE TABLE continuations (target TEXT PRIMARY KEY REFERENCES attempts(id), run TEXT NOT NULL UNIQUE REFERENCES runs(id)) STRICT; INSERT INTO continuations VALUES ('{first}','{run}'); PRAGMA user_version=4;"
    ))?;
    drop(store);
    let store = RunStore::open(&context, &call)?;
    let kept: String = store.db.query_row(
        "SELECT run FROM continuations WHERE target=?1",
        [&first],
        |r| r.get(0),
    )?;
    assert_eq!(kept, run);
    // A second attempt in the same run can now be continued as well.
    let mut second = store.record(input.attempts[0].attempt_id, &call)?;
    second.attempt_id = AttemptId::from_bytes([42; 16]);
    second.attempt_number = 2;
    store.db.execute(
        "INSERT INTO attempts(id,run,project,task,number,revision,terminal,record) VALUES (?1,?2,?3,?4,2,1,0,?5)",
        rusqlite::params![
            second.attempt_id.to_string(),
            run,
            second.project_id.to_string(),
            second.task_id.to_string(),
            crate::db::bytes(&second)?
        ],
    )?;
    store.db.execute(
        "INSERT INTO continuations(target,run) VALUES (?1,?2)",
        rusqlite::params![second.attempt_id.to_string(), run],
    )?;
    let rows: i64 = store.db.query_row(
        "SELECT COUNT(*) FROM continuations WHERE run=?1",
        [&run],
        |r| r.get(0),
    )?;
    assert_eq!(rows, 2);
    Ok(())
}

#[test]
fn output_redacts_before_slicing_and_keeps_artifact_binding() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let input = input();
    let id = input.attempts[0].attempt_id;
    store.create(&input, &call)?;
    seal_exit_fixture(&store, &input)?;
    let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let raw = format!("prefix {secret} suffix");
    let mut publication = publication();
    publication.result = &raw;
    store.publish_terminal(id, 1, &publication, &exit(), &call)?;
    let output = store.read_output(
        id,
        ArtifactPart::Result,
        ByteRange::default(),
        &context,
        &call,
    )?;
    assert_eq!(output.bytes, b"prefix [REDACTED] suffix");
    assert_eq!(output.digest, CanonicalV1::file_digest(&output.bytes));
    let fragment = store.read_output(
        id,
        ArtifactPart::Result,
        ByteRange {
            offset: 9,
            max_bytes: 4,
        },
        &context,
        &call,
    )?;
    assert_eq!(fragment.bytes, b"EDAC");
    assert_eq!(
        store.record(id, &call)?.result.unwrap().digest,
        CanonicalV1::file_digest(raw.as_bytes())
    );
    Ok(())
}

#[test]
fn lane_selection_is_owned_and_skips_seen_completions() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let input = input();
    store.create(&input, &call)?;
    let id = input.attempts[0].attempt_id;
    let query = |owner: Option<ActorId>, unseen_after: Option<u64>| LaneQuery {
        targets: vec![],
        all: unseen_after.is_some(),
        limit: 100,
        after: 0,
        newest_first: false,
        owner,
        unseen_after,
    };
    let count = |q: &LaneQuery| store.lanes(q, &call).map(|p| p.lanes.len());
    // Another actor's chair sees none of this actor's active work.
    assert_eq!(count(&query(Some(context.actor_id), None))?, 1);
    assert_eq!(count(&query(Some(ActorId::from_bytes([9; 16])), None))?, 0);
    assert_eq!(count(&query(None, None))?, 1);
    // A completed lane is selected until its event is acknowledged.
    seal_exit_fixture(&store, &input)?;
    store.publish_terminal(id, 1, &publication(), &exit(), &call)?;
    let last = store.last_event_sequence(id, &call)?;
    assert!(last > 0);
    assert_eq!(count(&query(Some(context.actor_id), Some(0)))?, 1);
    assert_eq!(
        store.seen_cursor("default", true, &call)?,
        last,
        "a fresh cursor starts at the log end"
    );
    assert_eq!(store.seen_cursor("default", false, &call)?, 0);
    store.ack_seen("default", last, &call)?;
    store.ack_seen("default", 0, &call)?;
    let seen = store.seen_cursor("default", true, &call)?;
    assert_eq!(seen, last, "the cursor never moves back");
    assert_eq!(count(&query(Some(context.actor_id), Some(seen)))?, 0);
    Ok(())
}

#[test]
fn work_context_bounds_are_enforced() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    for value in ["x".repeat(257), "  ".into(), "bad\nvalue".into()] {
        let mut request = input();
        request.request.tasks[0].context = Some(provenant_records::run::WorkContext {
            work_item: Some(value),
            ..Default::default()
        });
        assert!(matches!(
            store.create(&request, &call),
            Err(RunError::Validation("work context"))
        ));
    }
    let mut request = input();
    request.request.tasks[0].context = Some(provenant_records::run::WorkContext {
        work_item: Some("x".repeat(256)),
        ..Default::default()
    });
    store.create(&request, &call)?;
    Ok(())
}

#[test]
fn version_six_upgrades_from_five_and_keeps_existing_attempts() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let input = input();
    let store = RunStore::open(&context, &call)?;
    store.create(&input, &call)?;
    // Shape the store exactly as version 5 left it.
    store.db.execute_batch(
        "DROP INDEX reaction_pending; DROP TABLE reactions; PRAGMA user_version=5;",
    )?;
    drop(store);
    let store = RunStore::open(&context, &call)?;
    let version: i64 =
        store
            .db
            .query_row("SELECT user_version FROM pragma_user_version", [], |r| {
                r.get(0)
            })?;
    assert_eq!(version, 6);
    // History is not backfilled: nothing is owed for attempts that predate it.
    assert!(store.pending_reactions(input.run_id, &call)?.is_empty());
    store.record(input.attempts[0].attempt_id, &call)?;
    Ok(())
}

fn settle_one(
    store: &RunStore,
    call: &Request<'_>,
) -> std::result::Result<PreparedRun, Box<dyn std::error::Error>> {
    let input = input();
    let id = input.attempts[0].attempt_id;
    store.create(&input, call)?;
    let mut observation = exit();
    observation.never_spawned = true;
    observation.termination = None;
    seal_observation(store, &input, &observation)?;
    let mut publication = publication();
    publication.outcome = Outcome::QuotaLimited;
    publication.started_at = None;
    publication.provider_ms = None;
    store.publish_terminal(id, 1, &publication, &observation, call)?;
    Ok(input)
}

#[test]
fn a_terminal_commit_owes_one_frozen_reaction_in_the_same_transaction() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let input = settle_one(&store, &call)?;
    let id = input.attempts[0].attempt_id;
    assert_eq!(store.pending_reactions(input.run_id, &call)?, [id]);
    let owed = store.reaction(id, &call)?.ok_or("row")?;
    assert!(
        owed.notice
            .starts_with(&format!("settled {id} quota_limited"))
    );
    assert!(owed.notice.len() <= crate::reactions::NOTICE_BYTES);
    assert!(owed.graph && !owed.resolved);
    // A replayed commit neither adds a second row nor rewrites the first.
    let again = store.reaction(id, &call)?.ok_or("row")?;
    assert_eq!(owed, again);
    Ok(())
}

#[test]
fn recovery_drains_what_a_crashed_guardian_left_and_a_failed_step_retries() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let input = settle_one(&store, &call)?;
    // The guardian dies after the commit, before any reaction.
    drop(store);
    let recovery = RunStore::open(&context, &call)?;
    assert_eq!(recovery.pending_reactions(input.run_id, &call)?.len(), 1);
    let calls = std::cell::RefCell::new((0, 0));
    let mut step = |reaction: &mut Reaction,
                    _save: &mut crate::reactions::Saver<'_>|
     -> std::result::Result<(), Box<dyn std::error::Error>> {
        // The graph advances from the frozen trusted inputs.
        assert!(reaction.graph);
        if !reaction.graph_done {
            calls.borrow_mut().0 += 1;
            reaction.graph_done = true;
        }
        if !reaction.notice_done {
            calls.borrow_mut().1 += 1;
            if calls.borrow().1 == 1 {
                return Err("inbox busy".into());
            }
            reaction.notice_done = true;
        }
        reaction.log_done = true;
        Ok(())
    };
    let failed = recovery.drain_reactions(input.run_id, &clock, &cancel, &mut step);
    assert_eq!(failed.unwrap_err().to_string(), "inbox busy");
    // Progress is kept and the row is still owed.
    assert_eq!(recovery.pending_reactions(input.run_id, &call)?.len(), 1);
    recovery.drain_reactions(input.run_id, &clock, &cancel, &mut step)?;
    assert!(recovery.pending_reactions(input.run_id, &call)?.is_empty());
    recovery.drain_reactions(input.run_id, &clock, &cancel, &mut step)?;
    // The graph ran once, the failed notice ran twice, and nothing ran after.
    assert_eq!(*calls.borrow(), (1, 2));
    Ok(())
}

#[test]
fn two_concurrent_drains_react_once() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let input = settle_one(&store, &call)?;
    let runs = std::cell::Cell::new(0);
    let mut finish = |reaction: &mut Reaction, _: &mut crate::reactions::Saver<'_>| {
        runs.set(runs.get() + 1);
        reaction.graph_done = true;
        reaction.notice_done = true;
        reaction.log_done = true;
        Ok(())
    };
    // One drain is mid-flight and holds the run's lock; a second, from another
    // process's store, defers to it instead of repeating its work.
    let held = store.reaction_lock(input.run_id)?.ok_or("lock")?;
    let other = RunStore::open(&context, &call)?;
    other.drain_reactions(input.run_id, &clock, &cancel, &mut finish)?;
    assert_eq!(runs.get(), 0);
    drop(held);
    other.drain_reactions(input.run_id, &clock, &cancel, &mut finish)?;
    store.drain_reactions(input.run_id, &clock, &cancel, &mut finish)?;
    assert_eq!(runs.get(), 1);
    assert!(store.pending_reactions(input.run_id, &call)?.is_empty());
    Ok(())
}

#[test]
fn done_rows_are_pruned_and_pending_rows_are_kept() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let input = settle_one(&store, &call)?;
    let id = input.attempts[0].attempt_id;
    store.prune_reactions(i64::MAX, &call)?;
    assert!(store.reaction(id, &call)?.is_some());
    store.drain_reactions(input.run_id, &clock, &cancel, &mut |reaction, _| {
        reaction.graph_done = true;
        reaction.notice_done = true;
        reaction.log_done = true;
        Ok(())
    })?;
    store.prune_reactions(i64::MAX, &call)?;
    assert!(store.reaction(id, &call)?.is_none());
    Ok(())
}

/// A clock whose monotonic reading a test moves while a step runs.
struct SkewClock {
    inner: TestClock,
    skew: std::sync::atomic::AtomicU64,
}
impl provenant_core::Clock for SkewClock {
    fn wall_now(&self) -> std::result::Result<WallTime, provenant_core::TimeError> {
        self.inner.wall_now()
    }
    fn mono_now(&self) -> MonoTime {
        MonoTime::from_micros(
            self.inner.mono_now().as_micros() + self.skew.load(std::sync::atomic::Ordering::SeqCst),
        )
    }
    fn wait_until(&self, deadline: MonoTime) -> std::result::Result<(), provenant_core::TimeError> {
        self.inner.wait_until(deadline)
    }
}

#[test]
fn progress_saves_with_a_fresh_budget_after_a_long_step() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let (base, cancel) = (clock(), CancelToken::new());
    let call = call(&base, &cancel);
    let store = RunStore::open(&context, &call)?;
    let input = settle_one(&store, &call)?;
    let id = input.attempts[0].attempt_id;
    let skewed = SkewClock {
        inner: clock(),
        skew: Default::default(),
    };
    store.drain_reactions(input.run_id, &skewed, &cancel, &mut |reaction, save| {
        // A graph advance can run for longer than one call budget.
        skewed
            .skew
            .store(120_000_000, std::sync::atomic::Ordering::SeqCst);
        reaction.graph_done = true;
        save(reaction)?;
        reaction.notice_done = true;
        reaction.log_done = true;
        Ok(())
    })?;
    assert!(store.pending_reactions(input.run_id, &call)?.is_empty());
    assert!(store.reaction(id, &call)?.ok_or("row")?.graph_done);
    Ok(())
}

#[test]
fn a_step_boundary_save_survives_a_failure_after_it() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let input = settle_one(&store, &call)?;
    let id = input.attempts[0].attempt_id;
    let failed = store.drain_reactions(input.run_id, &clock, &cancel, &mut |reaction, save| {
        reaction.resolved = true;
        save(reaction)?;
        // The step dies before it records anything else.
        Err("crash".into())
    });
    assert!(failed.is_err());
    let kept = store.reaction(id, &call)?.ok_or("row")?;
    assert!(kept.resolved && kept.pending());
    Ok(())
}

#[test]
fn a_closed_run_with_pending_reactions_is_open_for_reconcile() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let input = settle_one(&store, &call)?;
    assert_eq!(store.open_runs(10, &call)?, vec![input.run_id]);
    store.drain_reactions(input.run_id, &clock, &cancel, &mut |reaction, _| {
        reaction.notice_done = true;
        reaction.log_done = true;
        reaction.graph_done = true;
        Ok(())
    })?;
    assert!(store.open_runs(10, &call)?.is_empty());
    Ok(())
}

#[test]
fn a_forged_row_cannot_carry_launch_authority() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let input = settle_one(&store, &call)?;
    let id = input.attempts[0].attempt_id;
    // A lane rewrites the row to name its own executables and config root.
    let mut forged = serde_json::to_value(store.reaction(id, &call)?.ok_or("row")?)?;
    forged["graph"] = serde_json::json!({
        "configuration_root": "/tmp/evil",
        "cwd": "/tmp/evil",
        "executables": {"codex": "/tmp/evil/codex"},
        "git": "/tmp/evil/git",
        "codex_home": null,
    });
    store.db.execute(
        "UPDATE reactions SET payload=?1 WHERE attempt=?2",
        rusqlite::params![crate::db::bytes(&forged)?, id.to_string()],
    )?;
    // The row has no place for host authority, so it is refused, not obeyed.
    assert!(store.reaction(id, &call).is_err());
    let mut ran = false;
    let result = store.drain_reactions(input.run_id, &clock, &cancel, &mut |_, _| {
        ran = true;
        Ok(())
    });
    assert!(result.is_err() && !ran);
    Ok(())
}
