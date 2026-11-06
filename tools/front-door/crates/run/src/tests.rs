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

/// Synthetic admission facts, including a process table independent of the host.
pub(super) struct HostFacts {
    pub(super) available: u64,
    pub(super) total: u64,
    pub(super) process_used: u64,
    pub(super) disk: u64,
    pub(super) process_limit: u64,
    pub(super) identity: Option<provenant_supervisor::census::ProcessIdentity>,
    observations: std::cell::Cell<usize>,
}
impl HostFacts {
    pub(super) fn new(available: u64) -> Self {
        Self {
            available,
            total: 100,
            process_used: 1,
            disk: u64::MAX,
            process_limit: 100,
            identity: Some(Self::identity()),
            observations: std::cell::Cell::new(0),
        }
    }
    pub(super) fn observations(&self) -> usize {
        self.observations.get()
    }
    fn identity() -> provenant_supervisor::census::ProcessIdentity {
        provenant_supervisor::census::ProcessIdentity {
            boot: "test-boot".into(),
            pid: 42,
            native_start: 1,
            group_id: 42,
            session_id: 42,
        }
    }
}
impl Resources for HostFacts {
    fn memory(&self) -> std::io::Result<provenant_os::sys::resources::MemorySample> {
        Ok(provenant_os::sys::resources::MemorySample {
            available_bytes: self.available,
            total_bytes: self.total,
        })
    }
    fn disk(&self, _: &provenant_os::fs::Directory) -> std::io::Result<u64> {
        Ok(self.disk)
    }
    fn process_budget(&self) -> std::io::Result<provenant_os::sys::resources::ProcessBudget> {
        Ok(provenant_os::sys::resources::ProcessBudget {
            used: self.process_used,
            limit: self.process_limit,
        })
    }
    fn guardian_identity(&self) -> Option<provenant_supervisor::census::ProcessIdentity> {
        self.identity.clone()
    }
    fn observe_process(&self, pid: i64) -> provenant_supervisor::census::ProcessObservation {
        use provenant_supervisor::census::{ProcessFact, ProcessObservation};
        self.observations.set(self.observations.get() + 1);
        if let Some(identity) = self
            .identity
            .as_ref()
            .filter(|identity| pid == identity.pid)
        {
            ProcessObservation::Present(ProcessFact {
                identity: identity.clone(),
                parent_pid: 1,
                zombie: false,
                cpu_micros: None,
                command: None,
            })
        } else {
            ProcessObservation::Absent
        }
    }
}

#[test]
fn host_facts_can_override_resource_and_process_observations() {
    let mut facts = HostFacts::new(23);
    facts.total = 80;
    facts.process_used = 7;
    facts.process_limit = 9;
    facts.identity = None;
    let memory = facts.memory().expect("fixture memory");
    assert_eq!((memory.available_bytes, memory.total_bytes), (23, 80));
    let budget = facts.process_budget().expect("fixture budget");
    assert_eq!((budget.used, budget.limit), (7, 9));
    assert!(facts.guardian_identity().is_none());
    assert!(matches!(
        facts.observe_process(42),
        provenant_supervisor::census::ProcessObservation::Absent
    ));
}

#[test]
fn cloud_storage_is_attempt_scoped_and_locked() -> TestResult {
    use provenant_os::fs::{LockMode, PublishMode};
    use std::ffi::OsStr;

    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let input = input();
    store.create(&input, &call)?;
    let id = input.attempts[0].attempt_id;
    let attempt = store.attempt_dir(input.run_id, id, false)?;
    assert!(store.with_cloud_storage(id, false, &call, |_| ()).is_err());
    assert!(
        attempt
            .metadata_at(OsStr::new("cloud.lock"), false)
            .unwrap_err()
            .is_not_found()
    );
    store.with_cloud_storage(id, true, &call, |cloud| {
        assert!(matches!(
            attempt.try_lock(OsStr::new("cloud.lock"), LockMode::Exclusive),
            Err(provenant_os::fs::FsError::LockTimeout { .. })
        ));
        cloud
            .atomic_write(
                OsStr::new("cloud-job.json"),
                ".tmp-",
                b"job",
                PublishMode::NoReplace,
            )
            .unwrap()
            .require_durable()
            .unwrap();
    })?;
    let saved = store.with_cloud_storage(id, false, &call, |cloud| {
        cloud.read_stable(OsStr::new("cloud-job.json"), 1024)
    })??;
    assert_eq!(saved, b"job");
    assert!(
        store
            .with_cloud_storage(AttemptId::from_bytes([9; 16]), false, &call, |_| ())
            .is_err()
    );
    Ok(())
}

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
    let mut messages = std::collections::BTreeSet::new();
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
        assert!(
            matches!(&error, RunError::UnsafePath { path, check: actual }
            if path == std::path::Path::new("runs/example/process.json") && *actual == check)
        );
        let message = error.to_string();
        assert!(message.contains("runs/example/process.json"), "{message}");
        assert!(message.contains(reason), "{message}");
        assert!(messages.insert(message));
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
                cloud: None,
                context: None,
                extra: Default::default(),
                build_heavy: false,
                title: None,
                base: None,
                head: None,
                source_head: None,
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

#[test]
fn refused_operation_is_durable_and_replay_checks_request_identity() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let input = input();
    let diagnostic = provenant_records::diagnostic::Diagnostic::new("QUEUE_FULL")
        .category("refused")
        .detail("admission queue is full");
    let store = RunStore::open(&context, &call)?;
    let saved = store.record_refusal(&input, diagnostic.clone(), Some("lane".into()), &call)?;
    assert_eq!(saved.refused_at, Some(input.created_at));
    drop(store);
    let store = RunStore::open(&context, &call)?;
    let replayed = store
        .operation(&input.operation_id, &call)?
        .ok_or("missing refusal")?;
    assert_eq!(replayed.refusal, Some(diagnostic.clone()));
    assert_eq!(replayed.worktree.as_deref(), Some("lane"));
    assert_eq!(store.recent_refusals(10, &call)?, vec![replayed.clone()]);
    let mut other_context = context.clone();
    other_context.actor_id = ActorId::from_bytes([9; 16]);
    let other = RunStore::open(&other_context, &call)?;
    assert!(other.recent_refusals(10, &call)?.is_empty());
    assert!(store.recent_refusals(0, &call).is_err());
    let duplicate = store.record_refusal(
        &input,
        provenant_records::diagnostic::Diagnostic::new("OTHER"),
        Some("other".into()),
        &call,
    )?;
    assert_eq!(
        duplicate, replayed,
        "a replay cannot replace the first refusal"
    );
    assert!(
        matches!(store.create(&input, &call), Err(RunError::RecordedRefusal(saved)) if *saved == diagnostic)
    );
    let mut changed = input;
    changed.request.concurrency = 2;
    assert!(matches!(
        store.create(&changed, &call),
        Err(RunError::OperationConflict)
    ));
    assert!(matches!(
        store.record_refusal(&changed, diagnostic, None, &call),
        Err(RunError::OperationConflict)
    ));
    Ok(())
}

#[test]
fn refusal_notice_obligation_survives_restart_and_completes_only_for_same_facts() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let mut input = input();
    input.request.chair = Some(ChairBinding {
        extra: Default::default(),
        host: provenant_core::HostId::from_bytes([1; 16]),
        actor: ActorId::from_bytes([2; 16]),
        session: None,
    });
    let store = RunStore::open(&context, &call)?;
    let record = store.record_refusal(
        &input,
        provenant_records::diagnostic::Diagnostic::new("REFUSED"),
        None,
        &call,
    )?;
    assert_eq!(
        store.pending_refusal_notices(10, &call)?,
        vec![record.clone()]
    );
    let mut changed = record.clone();
    changed.refusal = Some(provenant_records::diagnostic::Diagnostic::new("OTHER"));
    assert!(matches!(
        store.complete_refusal_notice(&changed, &call),
        Err(RunError::Conflict)
    ));
    drop(store);
    let store = RunStore::open(&context, &call)?;
    assert_eq!(
        store.pending_refusal_notices(10, &call)?,
        vec![record.clone()]
    );
    store.complete_refusal_notice(&record, &call)?;
    store.complete_refusal_notice(&record, &call)?;
    assert!(store.pending_refusal_notices(10, &call)?.is_empty());
    assert_eq!(
        store
            .operation(&input.operation_id, &call)?
            .ok_or("missing")?
            .extra["refusal_notice_done"],
        true
    );
    for n in 10_u8..91 {
        input.operation_id = OperationId::parse(&format!("notice-{n}"))?;
        input.run_id = RunId::from_bytes([n; 16]);
        store.record_refusal(
            &input,
            provenant_records::diagnostic::Diagnostic::new("REFUSED"),
            None,
            &call,
        )?;
    }
    // Stored records are BLOBs: SQLite JSON queries must cast these exact bytes.
    let storage: String =
        store
            .db
            .query_row("SELECT typeof(record) FROM operations LIMIT 1", [], |row| {
                row.get(0)
            })?;
    assert_eq!(storage, "blob");
    assert_eq!(store.recent_refusals(100, &call)?.len(), 82);
    let page = store.pending_refusal_notices_after(80, 0, &call)?;
    assert_eq!(page.len(), 80);
    let after = page.last().ok_or("empty page")?.0;
    let tail = store.pending_refusal_notices_after(80, after, &call)?;
    assert_eq!(
        tail.len(),
        1,
        "failed notices in the first page cannot hide the tail"
    );
    assert_eq!(tail[0].1.operation_id.as_str(), "notice-90");
    let first = store.pending_refusal_notices(80, &call)?;
    assert_eq!(first.len(), 80);
    for pending in &first {
        store.complete_refusal_notice(pending, &call)?;
    }
    assert_eq!(store.pending_refusal_notices(80, &call)?.len(), 1);
    Ok(())
}
#[test]
fn corrupt_refusal_notices_do_not_hide_valid_notices_after_a_full_page() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let mut input = input();
    input.request.chair = Some(ChairBinding {
        extra: Default::default(),
        host: provenant_core::HostId::from_bytes([1; 16]),
        actor: ActorId::from_bytes([2; 16]),
        session: None,
    });
    let mut expected = Vec::new();
    for n in 0_u8..164 {
        input.operation_id = OperationId::parse(&format!("corrupt-notice-{n}"))?;
        input.run_id = RunId::from_bytes([n; 16]);
        let record = store.record_refusal(
            &input,
            provenant_records::diagnostic::Diagnostic::new("REFUSED"),
            None,
            &call,
        )?;
        if n >= 81 {
            expected.push(record);
            continue;
        }
        let mut value = serde_json::to_value(&record)?;
        let mut operation = input.operation_id.to_string();
        let raw = if n == 0 {
            b"{malformed JSON".to_vec()
        } else {
            match n % 3 {
                0 => {
                    value["schema_version"] = serde_json::json!(999);
                    serde_json::to_vec(&value)?
                }
                1 => {
                    operation = format!("invalid id {n}");
                    serde_json::to_vec(&value)?
                }
                _ => {
                    value["operation_id"] = serde_json::json!("another-operation");
                    serde_json::to_vec(&value)?
                }
            }
        };
        store.db.execute(
            "UPDATE operations SET operation=?1,record=?2 WHERE operation=?3",
            rusqlite::params![operation, raw, input.operation_id.as_str()],
        )?;
    }
    let first = store.pending_refusal_notices_after(80, 0, &call)?;
    assert_eq!(first.len(), 80);
    assert_eq!(
        first
            .iter()
            .map(|(_, record)| record.clone())
            .collect::<Vec<_>>(),
        expected[..80]
    );
    let tail = store.pending_refusal_notices_after(80, first[79].0, &call)?;
    assert_eq!(
        tail.into_iter()
            .map(|(_, record)| record)
            .collect::<Vec<_>>(),
        expected[80..]
    );
    for record in &expected {
        store.complete_refusal_notice(record, &call)?;
    }
    assert!(store.pending_refusal_notices(80, &call)?.is_empty());
    Ok(())
}

#[test]
fn recorded_refusal_freezes_the_notice_setting() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let mut input = input();
    input.request.tasks[0]
        .profile
        .knobs
        .insert("chair.refusal_notice".into(), serde_json::json!("off"));
    let record = store.record_refusal(
        &input,
        provenant_records::diagnostic::Diagnostic::new("REFUSED"),
        None,
        &call,
    )?;
    assert_eq!(record.extra["refusal_notice"], "off");
    assert_eq!(store.recent_refusals(1, &call)?, vec![record]);
    Ok(())
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
        terminal_cause: None,
        terminal_note: None,
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
        cancel_delivered_before_exit: None,
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
        launch_refusal: None,
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
        fast_forward_only: false,
    });
    store.create(&input, &call)?;
    seal_exit_fixture(&store, &input)?;
    let id = input.attempts[0].attempt_id;
    let head = provenant_core::GitOid::parse(&"a".repeat(40))?;
    let attempt = store.attempt_dir(input.run_id, id, false)?;
    artifacts::seal(
        &attempt,
        "writer-terminal.json",
        &db::bytes(&writer::WriterTerminal {
            schema_version: 1,
            head,
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
    assert!(record.protected_paths.is_empty());
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
fn record_read_is_one_snapshot_across_a_concurrent_terminal_publication() -> TestResult {
    let (new_clock, new_call, new_input) = (clock, call, input);
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let reader = RunStore::open(&context, &call)?;
    let prepared = input();
    reader.create(&prepared, &call)?;
    let id = prepared.attempts[0].attempt_id;
    // Another connection settles the attempt after the reader has taken the
    // pre-terminal attempt row but before it looks for the settlement receipt.
    let publisher = context.clone();
    crate::reader::AFTER_ATTEMPT_ROW.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let clock = new_clock();
            let cancel = CancelToken::new();
            let publish_call = new_call(&clock, &cancel);
            let store = RunStore::open(&publisher, &publish_call).expect("publisher store");
            seal_exit_fixture(&store, &new_input()).expect("exit fixture");
            store
                .publish_terminal(id, 1, &publication(), &exit(), &publish_call)
                .expect("terminal publication");
        }));
    });
    let torn = reader.record(id, &call);
    crate::reader::AFTER_ATTEMPT_ROW.with(|hook| hook.borrow_mut().take());
    let seen = torn?;
    assert!(
        !matches!(seen.state, AttemptState::Terminal { .. }),
        "the snapshot predates the publication: {:?}",
        seen.state
    );
    assert!(seen.settlement_receipts.is_none());
    let settled = reader.record(id, &call)?;
    assert!(matches!(settled.state, AttemptState::Terminal { .. }));
    assert!(settled.settlement_receipts.is_some());
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
        fast_forward_only: false,
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
    let raw = crate::db::bytes(&value)?;
    let recomputed = CanonicalV1::record_digest("run-request", 1, &value)?;
    std::fs::write(&path, raw)?;
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
fn older_request_bytes_are_verified_before_decoding() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let input = input();
    store.create(&input, &call)?;

    let mut value = serde_json::to_value(&input.request)?;
    let task = value["tasks"][0].as_object_mut().unwrap();
    task.insert(
        "profile".into(),
        serde_json::to_value(&input.request.tasks[0].profile)?,
    );
    assert_eq!(task.len(), 9);
    let mut raw = crate::db::bytes(&value)?;
    let digest = CanonicalV1::record_digest("run-request", 1, &value)?;
    let path = context
        .state_root
        .as_path()
        .join(format!("runs/{}/request.json", input.run_id));
    std::fs::write(&path, &raw)?;
    let mut record = store.run(input.run_id, &call)?;
    record.request_digest = digest;
    store.db.execute(
        "UPDATE runs SET record=?1 WHERE id=?2",
        rusqlite::params![crate::db::bytes(&record)?, input.run_id.to_string()],
    )?;
    let decoded = store.request(input.run_id, &call)?;
    assert_eq!(decoded.tasks[0].profile, input.request.tasks[0].profile);
    assert_ne!(decoded.digest()?, digest);

    raw[0] ^= 1;
    std::fs::write(&path, raw)?;
    assert_eq!(
        artifact_provenance(store.request(input.run_id, &call)).check,
        ArtifactCheck::RequestDigest
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

#[test]
fn invalid_or_stale_process_checkpoint_has_unknown_silence() -> TestResult {
    for checkpoint in [
        serde_json::json!({
            "schema_version": 1,
            "scope": {"run": RunId::from_bytes([4; 16]), "attempt": AttemptId::from_bytes([6; 16]), "generation": 9},
            "silent": true,
            "silent_for_ms": 1_800_000
        }),
        serde_json::json!({"schema_version": 1, "scope": "invalid", "silent": true}),
        serde_json::json!({
            "schema_version": 1,
            "scope": {"run": RunId::from_bytes([4; 16]), "attempt": AttemptId::from_bytes([6; 16]), "generation": 0},
            "observed_at": WallTime::from_micros(1_000_000)?,
            "silent": true,
            "silent_for_ms": 1_800_000
        }),
    ] {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let clock = TestClock::new(WallTime::from_micros(10_000_000)?, MonoTime::from_micros(0));
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context, &call)?;
        let input = input();
        store.create(&input, &call)?;
        let directory = store.attempt_dir(input.run_id, input.attempts[0].attempt_id, true)?;
        artifacts::seal(
            &directory,
            "process.json",
            &serde_json::to_vec(&checkpoint)?,
            RECORD_LIMIT,
        )?;
        let id = input.attempts[0].attempt_id;
        assert!(
            store
                .record_observed(id, &call)?
                .provider_silent_for_ms
                .0
                .is_none()
        );
        // Recovery and mutation reads still refuse a checkpoint for another scope.
        if checkpoint["observed_at"].is_null() {
            assert!(store.record(id, &call).is_err());
        }
    }
    Ok(())
}

#[test]
fn process_checkpoint_freshness_allows_one_second_of_clock_skew() -> TestResult {
    let now = WallTime::from_micros(10_000_000)?;
    assert!(crate::reader::process_observation_fresh(
        WallTime::from_micros(11_000_000)?,
        now
    ));
    assert!(!crate::reader::process_observation_fresh(
        WallTime::from_micros(11_000_001)?,
        now
    ));
    assert!(crate::reader::process_observation_fresh(
        WallTime::from_micros(5_000_000)?,
        now
    ));
    assert!(!crate::reader::process_observation_fresh(
        WallTime::from_micros(4_999_999)?,
        now
    ));
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
fn oversized_prompt_retains_code_and_projects_field_limit() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let mut request = input();
    request.attempts[0].prompt = format!(
        "TOKEN_SENTINEL{}",
        "é".repeat((RECORD_LIMIT / 2 + 1) as usize)
    );
    request.request.tasks[0].prompt_digest =
        CanonicalV1::file_digest(request.attempts[0].prompt.as_bytes());
    let Err(error) = store.create(&request, &call) else {
        return Err("oversized prompt accepted".into());
    };
    assert_eq!(
        error.to_string(),
        format!("read or record exceeds byte limit {RECORD_LIMIT}")
    );
    let projection = error.diagnostic(provenant_records::diagnostic::Destination::Local);
    assert_eq!(projection.code, "INVALID");
    assert!(
        projection
            .detail
            .as_deref()
            .unwrap()
            .contains("tasks[0].prompt")
    );
    assert!(
        projection
            .detail
            .as_deref()
            .unwrap()
            .contains(&RECORD_LIMIT.to_string())
    );
    assert!(!serde_json::to_string(&projection)?.contains("TOKEN_SENTINEL"));
    assert!(
        RunError::TooLarge {
            limit: RECORD_LIMIT
        }
        .diagnostic(provenant_records::diagnostic::Destination::Local)
        .detail
        .is_none()
    );
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
    if wanted
        .strip_prefix("withdraw-auth-")
        .or_else(|| wanted.strip_prefix("withdraw-"))
        .unwrap_or(&wanted)
        == phase
    {
        std::process::exit(73);
    }
}
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
            if crash.starts_with("withdraw-auth-") {
                Outcome::AuthRequired
            } else {
                Outcome::Cancelled
            },
            "cancelled before the crash",
            WallTime::from_micros(150)?,
            &call,
        )?;
        return Err("crash phase was not reached".into());
    }
    seal_exit_fixture(&store, &input)?;
    let attempt = store.attempt_dir(input.run_id, input.attempts[0].attempt_id, false)?;
    crate::artifacts::seal(
        &attempt,
        "terminal-effects.json",
        &crate::db::bytes(&crate::store::TerminalEffects {
            schema_version: 1,
            scope: RunStore::scope(&store.record(input.attempts[0].attempt_id, &call)?),
            effects: vec![(
                SettlementEffect::ManifestConflict,
                EffectDisposition::Failed {
                    reason: "crash-retained conflict".into(),
                    retryable: true,
                },
            )],
        })?,
        RECORD_LIMIT,
    )?;
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
    if std::env::var_os("PROVENANT_RUN_TEST_ROOT").is_some() {
        return publication_crash_worker();
    }

    let executor = SystemExecutor::new(1)?;
    for phase in [
        "reserved",
        "result",
        "sealed",
        "transaction",
        "committed",
        "withdraw-sealed",
        "withdraw-auth-sealed",
    ] {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let argv: Vec<OsString> = vec![
            std::env::current_exe()?.into_os_string(),
            "--exact".into(),
            "tests::publication_crashes_preserve_committed_records_and_allow_retry".into(),
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
            assert!(store.queued_runs(&call, QUEUED_RUNS_PAGE_SIZE)?.is_empty());
        }
        // A replay mints fresh IDs; it must land on the reserved ones.
        let mut replay = self::input();
        replay.run_id = RunId::from_bytes([9; 16]);
        replay.attempts[0].attempt_id = AttemptId::from_bytes([10; 16]);
        assert_eq!(store.create(&replay, &call)?.run_id, input.run_id);
        if phase == "reserved" {
            assert_eq!(
                store.queued_runs(&call, QUEUED_RUNS_PAGE_SIZE)?,
                vec![input.run_id]
            );
        }
        if !phase.starts_with("withdraw-") {
            seal_exit_fixture(&store, &input)?;
        }
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
        } else if phase.starts_with("withdraw-") {
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
            assert!(record.exit_evidence.is_none());
            assert_eq!(record.provider_launched, Some(false));
            assert!(
                matches!(record.state, AttemptState::Terminal { outcome, .. }
                if outcome == if phase == "withdraw-auth-sealed" { Outcome::AuthRequired } else { Outcome::Cancelled })
            );
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
        if phase != "reserved" && !phase.starts_with("withdraw-") {
            assert!(
                matches!(store.record(id, &call)?.settlement_receipts.unwrap().manifest_conflict,
                EffectDisposition::Failed { reason, retryable: true } if reason == "crash-retained conflict")
            );
        }
        assert!(store.record(id, &call)?.terminal_evidence.is_some());
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

#[test]
fn receipt_cancellation_requires_ordered_delivery() -> TestResult {
    for delivered in [None, Some(false), Some(true)] {
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
        observation.cancel_delivered_before_exit = delivered;
        seal_observation(&store, &input, &observation)?;
        let mut publication = publication();
        publication.outcome = Outcome::Cancelled;
        let result = store.publish_terminal(id, 1, &publication, &observation, &call);
        if delivered == Some(true) {
            let record = result?;
            assert_eq!(
                record.terminal_evidence,
                Some(TerminalEvidence::EffectiveCancellation)
            );
        } else {
            assert!(result.is_err());
            assert!(matches!(
                store.record(id, &call)?.state,
                AttemptState::Queued { .. }
            ));
        }
    }
    Ok(())
}

#[test]
fn publication_deferred_effects_survive_commit_and_replay() -> TestResult {
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
    let attempt = store.attempt_dir(input.run_id, id, false)?;
    let effects = serde_json::json!({
        "schema_version": 1,
        "scope": RunStore::scope(&store.record(id, &call)?),
        "effects": [["manifest_conflict", {"state":"failed", "reason":"conflicting evidence", "retryable":true}]]
    });
    crate::artifacts::seal(
        &attempt,
        "terminal-effects.json",
        &serde_json::to_vec(&effects)?,
        RECORD_LIMIT,
    )?;
    store.publish_terminal(id, 1, &publication(), &exit(), &call)?;
    drop(store);
    let store = RunStore::open(&context, &call)?;
    let record = store.replay_terminal(id, &call)?;
    assert!(
        matches!(record.settlement_receipts.unwrap().manifest_conflict,
        EffectDisposition::Failed { reason, retryable: true } if reason == "conflicting evidence")
    );
    Ok(())
}

#[test]
fn publication_lock_timeout_retains_exact_sealed_decision() -> TestResult {
    provenant_sqlite::with_busy_timeout(std::time::Duration::from_millis(50), || -> TestResult {
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
        let run = store.run_dir(input.run_id, false)?;
        let lock = run.try_lock(
            std::ffi::OsStr::new("publication.lock"),
            provenant_os::fs::LockMode::Exclusive,
        )?;
        assert!(
            store
                .publish_terminal(id, 1, &publication(), &exit(), &call)
                .is_err()
        );
        let attempt = store.attempt_dir(input.run_id, id, false)?;
        let sealed =
            crate::artifacts::read_sealed(&attempt, "terminal-pending.json", RECORD_LIMIT)?
                .ok_or("decision not sealed")?;
        assert!(matches!(
            store.record(id, &call)?.state,
            AttemptState::Queued { .. }
        ));
        drop(lock);
        drop(store);
        let store = RunStore::open(&context, &call)?;
        let recovered = store.recover_terminal(id, &exit(), &call)?;
        assert_eq!(
            crate::artifacts::read_sealed(&attempt, "terminal-pending.json", RECORD_LIMIT)?
                .unwrap(),
            sealed
        );
        let pending: AttemptRecord = crate::db::decode_attempt(&sealed)?;
        assert_eq!(recovered.state, pending.state);
        assert_eq!(recovered.result, pending.result);
        assert_eq!(store.events(0, 64, None, &call)?.len(), 2);
        Ok(())
    })
}

#[test]
fn publication_postcommit_effect_failure_still_cleans_temporary() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let mut input = input();
    input.request.tasks[0].writer = Some(WriterRequest {
        extra: Default::default(),
        project: temp.path().into(),
        worktree: temp.path().into(),
    });
    let id = input.attempts[0].attempt_id;
    store.create(&input, &call)?;
    seal_exit_fixture(&store, &input)?;
    let temporary = store.temporary_path(id, &call)?;
    std::fs::write(temporary.join("payload"), b"temporary")?;
    crate::store::FAIL_EFFECT_WRITE.with(|fault| fault.set(true));
    let published = store.publish_terminal(id, 1, &publication(), &exit(), &call);
    crate::store::FAIL_EFFECT_WRITE.with(|fault| fault.set(false));
    assert!(matches!(published?.state, AttemptState::Terminal { .. }));
    assert!(!temporary.exists());
    assert_eq!(admission_count(&store)?, 0);
    Ok(())
}

#[test]
fn publication_manifest_recovery_preserves_conflict_until_intent_is_established() -> TestResult {
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
    let attempt = store.attempt_dir(input.run_id, id, false)?;
    let archive = crate::artifacts::subdir(&attempt, std::ffi::OsStr::new("quarantine"), true)?;
    let scope = RunStore::scope(&store.record(id, &call)?);
    let intended = crate::db::bytes(
        &serde_json::json!({"schema_version":1,"scope":scope,"kind":"quarantine_manifest","disposition":"read_only"}),
    )?;
    crate::artifacts::seal(
        &archive,
        "manifest.json",
        b"conflicting evidence",
        RECORD_LIMIT,
    )?;
    assert!(
        crate::holder::seal_manifest(&archive, "manifest.json", &intended, RECORD_LIMIT).is_err()
    );
    crate::artifacts::seal(
        &attempt,
        "terminal-effects.json",
        &crate::db::bytes(&crate::store::TerminalEffects {
            schema_version: 1,
            scope,
            effects: vec![(
                SettlementEffect::ManifestConflict,
                EffectDisposition::Failed {
                    reason: "conflict".into(),
                    retryable: true,
                },
            )],
        })?,
        RECORD_LIMIT,
    )?;
    store.publish_terminal(id, 1, &publication(), &exit(), &call)?;
    drop(store);
    let store = RunStore::open(&context, &call)?;
    assert!(
        store
            .pending_effect_attempts(input.run_id, &call)?
            .contains(&id)
    );
    store.reconcile_terminal_effects(&store.record(id, &call)?, &call)?;
    assert_eq!(
        crate::artifacts::read_sealed(&archive, "manifest.json", RECORD_LIMIT)?.unwrap(),
        b"conflicting evidence"
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
    // Model explicit host resolution while preserving the conflicting evidence.
    let path = context
        .state_root
        .as_path()
        .join(format!("runs/{}/attempts/{id}/quarantine", input.run_id));
    std::fs::rename(
        path.join("manifest.json"),
        path.join("manifest.conflict.json"),
    )?;
    store.reconcile_terminal_effects(&store.record(id, &call)?, &call)?;
    assert_eq!(
        crate::artifacts::read_sealed(&archive, "manifest.json", RECORD_LIMIT)?.unwrap(),
        intended
    );
    assert!(matches!(
        store
            .record(id, &call)?
            .settlement_receipts
            .unwrap()
            .manifest_conflict,
        EffectDisposition::Done
    ));
    assert!(
        !store
            .pending_effect_attempts(input.run_id, &call)?
            .contains(&id)
    );
    Ok(())
}

#[test]
fn receipt_prelaunch_cancel_and_deadline_have_distinct_evidence() -> TestResult {
    use provenant_supervisor::machine::StopReason;
    for prelaunch in [true, false] {
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
        observation.never_spawned = prelaunch;
        observation.termination = if prelaunch {
            None
        } else {
            Some(Termination::Exit(1))
        };
        observation.stop_reason = Some(if prelaunch {
            StopReason::ExplicitCancellation
        } else {
            StopReason::WallDeadline
        });
        observation.cancel_delivered_before_exit = Some(false);
        seal_observation(&store, &input, &observation)?;
        let dir = store.attempt_dir(input.run_id, id, false)?;
        crate::artifacts::seal(
            &dir,
            "cancel.json",
            &serde_json::to_vec(
                &serde_json::json!({"schema_version":1,"scope":observation.scope,"requester":context.actor_id,"request_id":"stop-test","reason":observation.stop_reason}),
            )?,
            RECORD_LIMIT,
        )?;
        let mut publication = publication();
        publication.outcome = if prelaunch {
            Outcome::Cancelled
        } else {
            Outcome::TimedOut
        };
        publication.started_at = if prelaunch {
            None
        } else {
            publication.started_at
        };
        let record = store.publish_terminal(id, 1, &publication, &observation, &call)?;
        assert_eq!(
            record.cancellation_disposition,
            if prelaunch {
                Some(CancellationDisposition::Effective)
            } else {
                None
            }
        );
    }
    Ok(())
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
fn the_store_initialises_at_version_nine_without_spacing() -> TestResult {
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
    assert_eq!(version, 9);
    let spacing: i64 = store.db.query_row(
        "SELECT COUNT(*) FROM sqlite_schema WHERE name='spacing'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(spacing, 0);
    Ok(())
}

#[test]
fn a_version_seven_store_upgrades_on_open_and_can_admit() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    // Shape the store exactly as version 7 left it.
    RunStore::open(&context, &call)?
        .db
        .execute_batch("DROP TABLE admission_windows; PRAGMA user_version=7;")?;
    let store = RunStore::open(&context, &call)?;
    let version: i64 =
        store
            .db
            .query_row("SELECT user_version FROM pragma_user_version", [], |r| {
                r.get(0)
            })?;
    assert_eq!(version, 9);
    let operation = OperationId::parse("admit-after-upgrade")?;
    assert!(store.admission_window(&operation, &call)?.is_none());
    let deadline = WallTime::from_micros(1_000_000)?;
    store.save_admission_window(
        &operation,
        crate::AdmissionWindow {
            deadline,
            wake_at: None,
        },
        &call,
    )?;
    assert!(store.admission_window(&operation, &call)?.is_some());
    let backups = std::fs::read_dir(temp.path().join("state"))?
        .filter_map(std::result::Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("run.sqlite3.bak-")
        })
        .count();
    assert_eq!(backups, 1, "the upgrade keeps one pre-migration copy");
    Ok(())
}

#[test]
fn a_store_failure_omits_source_text_at_every_destination() {
    use provenant_records::diagnostic::Destination;
    let error = RunError::Sqlite(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(1),
        Some("no such table: TOKEN_SENTINEL /private/host\u{1b}".to_owned()),
    ));
    for destination in [
        Destination::Local,
        Destination::Remote {
            repository: None,
            home: None,
        },
    ] {
        let diagnostic = error.diagnostic(destination);
        assert_eq!(diagnostic.code, "IO");
        assert!(diagnostic.detail.is_none());
        assert!(diagnostic.next.is_some());
        let wire = serde_json::to_string(&diagnostic).unwrap();
        assert!(!wire.contains("TOKEN_SENTINEL"));
        assert!(!wire.contains("/private/host"));
        assert!(!wire.chars().any(char::is_control));
    }
}

#[test]
fn the_store_refuses_a_version_newer_than_nine() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    RunStore::open(&context, &call)?
        .db
        .execute_batch("PRAGMA user_version=10;")?;
    assert!(matches!(
        RunStore::open(&context, &call),
        Err(RunError::Version(10))
    ));
    let state = context.state_root.as_path();
    let version: i64 = provenant_sqlite::open_connection(&state.join("run.sqlite3"), true)?
        .query_row("SELECT user_version FROM pragma_user_version", [], |r| {
            r.get(0)
        })?;
    assert_eq!(version, 10, "a refused store stays as it was");
    assert!(
        !std::fs::read_dir(state)?
            .filter_map(std::result::Result::ok)
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("run.sqlite3.bak-")),
        "a refused store takes no backup"
    );
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
        "DROP TABLE settlement_receipts; DROP INDEX reaction_pending; DROP TABLE reactions; DROP INDEX continuation_runs; DROP TABLE continuations; CREATE TABLE continuations (target TEXT PRIMARY KEY REFERENCES attempts(id), run TEXT NOT NULL UNIQUE REFERENCES runs(id)) STRICT; INSERT INTO continuations VALUES ('{first}','{run}'); PRAGMA user_version=4;"
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
fn version_seven_is_snapshotted_before_admission_windows_are_added() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    store
        .db
        .execute_batch("DROP TABLE admission_windows; PRAGMA user_version=7;")?;
    drop(store);

    let store = RunStore::open(&context, &call)?;
    let version: i64 = store
        .db
        .pragma_query_value(None, "user_version", |row| row.get(0))?;
    assert_eq!(version, 9);
    let backup = context.state_root.as_path().join(format!(
        "run.sqlite3.bak-{}",
        call.clock.wall_now()?.as_micros()
    ));
    let backup = provenant_sqlite::open_connection(&backup, true)?;
    let version: i64 = backup.pragma_query_value(None, "user_version", |row| row.get(0))?;
    assert_eq!(version, 7);
    let tables: i64 = backup.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE name='admission_windows'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(tables, 0);
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

#[test]
fn receipt_lost_recovery_converges_on_a_sealed_non_lost_decision() -> TestResult {
    provenant_sqlite::with_busy_timeout(std::time::Duration::from_millis(30), || -> TestResult {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context, &call)?;
        let input = input();
        let id = input.attempts[0].attempt_id;
        store.create(&input, &call)?;
        let blocker = rusqlite::Connection::open(context.state_root.as_path().join("run.sqlite3"))?;
        blocker.execute_batch("BEGIN IMMEDIATE")?;
        crate::dispatch_tests::seal_exit(&store, &exit())?;
        assert!(
            store
                .publish_terminal(id, 1, &publication(), &exit(), &call)
                .is_err()
        );
        blocker.execute_batch("ROLLBACK")?;
        let conflicting = TerminalPublication {
            terminal_cause: None,
            terminal_note: None,
            result: "conflicting result",
            ..publication()
        };
        assert!(
            store
                .publish_terminal(id, 1, &conflicting, &exit(), &call)
                .is_err()
        );
        let attempt = store.attempt_dir(input.run_id, id, false)?;
        let intent =
            attempt.read_stable(std::ffi::OsStr::new("terminal-pending.json"), RECORD_LIMIT)?;
        assert!(!intent.is_empty());
        let scope = RunStore::scope(&store.record(id, &call)?);
        store.commit_lost(scope, "holder lost", &call)?;
        assert!(matches!(
            store.record(id, &call)?.state,
            AttemptState::Terminal {
                outcome: Outcome::Succeeded,
                ..
            }
        ));
        let rows: i64 = blocker.query_row(
            "SELECT count(*) FROM events WHERE attempt=?1 AND revision=2",
            [id.to_string()],
            |row| row.get(0),
        )?;
        assert_eq!(rows, 1);
        Ok(())
    })
}

#[test]
fn receipt_busy_then_replay_converges_and_retains_conflict() -> TestResult {
    provenant_sqlite::with_busy_timeout(std::time::Duration::from_millis(30), || -> TestResult {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context, &call)?;
        let input = input();
        let id = input.attempts[0].attempt_id;
        store.create(&input, &call)?;
        let blocker = rusqlite::Connection::open(context.state_root.as_path().join("run.sqlite3"))?;
        blocker.execute_batch("BEGIN IMMEDIATE")?;
        crate::dispatch_tests::seal_exit(&store, &exit())?;
        assert!(
            store
                .publish_terminal(id, 1, &publication(), &exit(), &call)
                .is_err()
        );
        let path = context
            .state_root
            .as_path()
            .join("runs")
            .join(input.run_id.to_string())
            .join("attempts")
            .join(id.to_string());
        let original = std::fs::read(path.join("terminal-pending.json"))?;
        blocker.execute_batch("ROLLBACK")?;
        let conflicting = TerminalPublication {
            terminal_cause: None,
            terminal_note: None,
            result: "conflicting result",
            ..publication()
        };
        assert!(
            store
                .publish_terminal(id, 1, &conflicting, &exit(), &call)
                .is_err()
        );
        assert_eq!(std::fs::read(path.join("terminal-pending.json"))?, original);
        let conflicts: Vec<_> = std::fs::read_dir(&path)?
            .filter_map(std::result::Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("terminal-pending.conflict-")
            })
            .collect();
        assert_eq!(conflicts.len(), 1);
        let conflict = std::fs::read(conflicts[0].path())?;
        let digest = provenant_core::hex(CanonicalV1::file_digest(&conflict).bytes());
        assert_eq!(
            conflicts[0].file_name().to_string_lossy(),
            format!("terminal-pending.conflict-{digest}.json")
        );
        let first = store.replay_terminal(id, &call)?;
        let second = store.replay_terminal(id, &call)?;
        assert_eq!(first.state, second.state);
        assert!(matches!(
            first.state,
            AttemptState::Terminal {
                outcome: Outcome::Succeeded,
                ..
            }
        ));
        assert_eq!(
            first.result.unwrap().digest,
            CanonicalV1::file_digest(publication().result.as_bytes())
        );
        let rows: i64 = blocker.query_row(
            "SELECT count(*) FROM events WHERE attempt=?1 AND revision=2",
            [id.to_string()],
            |row| row.get(0),
        )?;
        assert_eq!(rows, 1);
        Ok(())
    })
}

#[test]
fn pressure_collection_uses_configured_logs_age() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let input = settle_one(&store, &call)?;
    let id = input.attempts[0].attempt_id;
    store.drain_reactions(input.run_id, &clock, &cancel, &mut |reaction, _| {
        reaction.graph_done = true;
        reaction.notice_done = true;
        reaction.log_done = true;
        Ok(())
    })?;
    store
        .db
        .execute("UPDATE reactions SET settled_at=100", [])?;
    let directory = provenant_os::fs::Directory::open_anchor(context.state_root.as_path())?;
    let now = provenant_core::WallTime::from_micros((100 + 10 * 86_400) * 1_000_000)?;
    store.gc_temporary(&directory, 7, 20, now, false, &call)?;
    assert!(store.reaction(id, &call)?.is_some());
    store.gc_temporary(&directory, 7, 5, now, false, &call)?;
    assert!(store.reaction(id, &call)?.is_none());
    Ok(())
}

#[test]
fn historical_run_stores_upgrade_with_attempts_and_backups() -> TestResult {
    for version in [1, 2, 5] {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        let input = input();
        let store = RunStore::open(&context, &call)?;
        store.create(&input, &call)?;
        store.db.execute_batch(
            "DROP TABLE settlement_receipts; DROP INDEX reaction_pending; DROP TABLE reactions;",
        )?;
        if version < 5 {
            store.db.execute_batch("DROP INDEX retained_temporary; ALTER TABLE attempts DROP COLUMN tmp_retained; CREATE TABLE spacing (id INTEGER PRIMARY KEY CHECK(id=1), last_start INTEGER NOT NULL) STRICT;")?;
        }
        if version == 1 {
            store.db.execute_batch("DROP TABLE cancellations;")?;
        }
        store.db.pragma_update(None, "user_version", version)?;
        drop(store);
        let upgraded = RunStore::open(&context, &call)?;
        let backup = context.state_root.as_path().join(format!(
            "run.sqlite3.bak-{}",
            call.clock.wall_now()?.as_micros()
        ));
        let backup = provenant_sqlite::open_connection(&backup, true)?;
        assert_eq!(
            backup.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))?,
            version
        );
        assert_eq!(
            backup.query_row("SELECT count(*) FROM attempts", [], |r| r.get::<_, i64>(0))?,
            1
        );
        upgraded.record(input.attempts[0].attempt_id, &call)?;
        if version == 1 {
            let op = OperationId::parse("stop")?;
            upgraded.bind_cancel(&op, "attempt", &call)?;
            assert!(matches!(
                upgraded.bind_cancel(&op, "other", &call),
                Err(RunError::OperationConflict)
            ));
        }
    }
    Ok(())
}

#[test]
fn version_eight_upgrade_preserves_terminal_attempt_references() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let input = input();
    let id = input.attempts[0].attempt_id;
    let store = RunStore::open(&context, &call)?;
    store.create(&input, &call)?;
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
    let before = store.record(id, &call)?;
    let event_count: i64 = store.db.query_row(
        "SELECT count(*) FROM events WHERE attempt=?1",
        [id.to_string()],
        |row| row.get(0),
    )?;
    assert!(event_count > 0);
    store.db.pragma_update(None, "user_version", 8)?;
    drop(store);
    let upgraded = RunStore::open(&context, &call)?;
    assert_eq!(upgraded.record(id, &call)?, before);
    assert_eq!(
        upgraded.db.query_row(
            "SELECT count(*) FROM events WHERE attempt=?1",
            [id.to_string()],
            |row| row.get::<_, i64>(0)
        )?,
        event_count
    );
    assert_eq!(
        upgraded
            .db
            .pragma_query_value(None, "foreign_keys", |row| row.get::<_, i64>(0))?,
        1
    );
    assert_eq!(
        upgraded
            .db
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, i64>(0)
            })?,
        0
    );
    assert!(
        upgraded
            .db
            .execute(
                "UPDATE attempts SET record=record WHERE id=?1",
                [id.to_string()]
            )
            .is_err()
    );
    assert!(
        upgraded
            .db
            .execute("DELETE FROM attempts WHERE id=?1", [id.to_string()])
            .is_err()
    );
    Ok(())
}
