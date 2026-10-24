use super::*;
use provenant_core::{
    ActorId, AttemptId, CancelToken, Digest, MonoTime, OperationId, ProjectId, RootInputs, RunId,
    TaskId, TestClock, WaitBudget, WallTime,
};
use provenant_records::CanonicalV1;
use provenant_supervisor::{
    custody::{CaptureTotals, ExitObservation, PublicationState, Scope, SettlementEvidence},
    exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination},
    fs::LocalFs,
};
use std::{ffi::OsString, path::Path, time::Duration};
type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;
pub(super) fn context(
    base: &Path,
) -> std::result::Result<ExecutionContext, Box<dyn std::error::Error>> {
    let base = std::fs::canonicalize(base)?;
    std::fs::create_dir_all(base.join("home"))?;
    std::fs::create_dir_all(base.join("checkout"))?;
    let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
    inputs.state_dir = Some(base.join("state"));
    Ok(ExecutionContext {
        state_root: inputs.resolve(&LocalFs)?.state,
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
    Request {
        clock,
        budget: WaitBudget::cli(MonoTime::from_micros(0), 10).unwrap(),
        cancel,
    }
}
pub(super) fn input() -> PreparedRun {
    let task = TaskId::from_bytes([3; 16]);
    PreparedRun {
        run_id: RunId::from_bytes([4; 16]),
        operation_id: OperationId::parse("run-test").unwrap(),
        created_at: WallTime::from_micros(100).unwrap(),
        request: CanonicalRequest {
            schema_version: 1,
            tasks: vec![TaskRequest {
                writer: None,
                profile: HarnessProfile::default(),
                task_id: task,
                prompt_digest: CanonicalV1::file_digest(b"exact prompt"),
                route: RequestedRoute::Model {
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
        terminal_observed: true,
        unresolved_cleanup: false,
        settlement_evidence: SettlementEvidence::LinuxKernelChildren,
        stdout: CaptureTotals::default(),
        stderr: CaptureTotals::default(),
    }
}
#[test]
fn create_publish_and_read_exact_bounded_facts() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let input = input();
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
            provenant_supervisor::fs::FsError::TooLarge { .. }
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
            AttemptState::Queued,
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
        // A replay mints fresh IDs; it must land on the reserved ones.
        let mut replay = self::input();
        replay.run_id = RunId::from_bytes([9; 16]);
        replay.attempts[0].attempt_id = AttemptId::from_bytes([10; 16]);
        assert_eq!(store.create(&replay, &call)?.run_id, input.run_id);
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
                    AttemptState::Queued,
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
    assert!(matches!(
        store.publish_terminal(id, 1, &recomputed, &exit(), &call),
        Err(RunError::ArtifactMismatch)
    ));
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
fn seal_observation(
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
fn a_version_one_store_gains_cancel_binding_on_reopen() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    // Shape the store exactly as version 1 left it.
    RunStore::open(&context, &call)?
        .db
        .execute_batch("DROP TABLE cancellations; PRAGMA user_version=1;")?;
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
