//! Scheduled host work. Deadlines live on disk; this process owns no timers.
use provenant_cli::Globals;
use provenant_confinement::worktrees::WorkerObserver;
use provenant_core::{Clock, DurationMicros, WaitBudget, WallTime};
use provenant_os::{
    clock::SystemClock,
    exec::{Executor, SystemExecutor},
    fs::{Directory, LockGuard, LockMode, PublishMode},
};
use provenant_records::{CommandResult, Outcome};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::Duration,
};

mod steward;

const DAY_SECONDS: u64 = 86_400;
/// An unfinished scheduled collection retries after this delay, then after
/// each factor-fold longer one, and never later than the daily cadence.
const GC_RETRY_BASE_SECONDS: u64 = 15 * 60;
const GC_RETRY_FACTOR: u64 = 4;
const GC_RESULT: &str = "gc-result.json";
const MAX_SEED_DISCOVERY_ERRORS: usize = 16;
const MAX_SEED_ERROR_BYTES: usize = 1024;
const MAX_SEED_PROJECT_PATH_BYTES: usize = 1024;
const MAX_BUSY_SEED_PROJECTS: usize = 512;
const MAX_SEED_PROJECT_RECORDS: usize = 512;
const MAX_SEED_RESULT_BYTES: u64 = 2 * 1024 * 1024;
const SEED_WORK_RETRY_MICROS: i64 = 60_000_000;

fn is_zero<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

/// The daily collection deadline. It advances only when a collection
/// completes, so a failed or interrupted one stays due.
#[derive(Default, Serialize, Deserialize)]
struct Cadence {
    #[serde(flatten)]
    extra: provenant_records::ExtraFields,
    next_gc_at: Option<WallTime>,
}

/// A task's last outcome, written by its detached worker or its scheduler.
#[derive(Serialize, Deserialize)]
struct TaskResult {
    #[serde(flatten)]
    extra: provenant_records::ExtraFields,
    state: String,
    at: WallTime,
    error: Option<String>,
    /// Consecutive attempts that did not complete. It selects the retry delay
    /// after a failure and restarts at zero when a collection completes.
    #[serde(default, skip_serializing_if = "is_zero")]
    failures: u32,
}

const SEED_QUEUE: &str = "seed-queue.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", content = "project_keys", rename_all = "snake_case")]
enum SeedScope {
    Full,
    Busy(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SeedRequest {
    scope: SeedScope,
}

impl SeedRequest {
    fn full() -> Self {
        Self {
            scope: SeedScope::Full,
        }
    }

    fn busy(project_keys: Vec<String>) -> Self {
        Self {
            scope: SeedScope::Busy(project_keys),
        }
    }

    fn project_keys(&self) -> &[String] {
        match &self.scope {
            SeedScope::Full => &[],
            SeedScope::Busy(project_keys) => project_keys,
        }
    }
}

/// Durable admission and crash-recovery state for the single seed worker.
#[derive(Debug, Default, Serialize, Deserialize)]
struct SeedQueue {
    #[serde(flatten)]
    extra: provenant_records::ExtraFields,
    pending: Option<SeedRequest>,
    active: Option<SeedRequest>,
    retry: Option<SeedRequest>,
    pending_at: Option<WallTime>,
    launch_at: Option<WallTime>,
    active_at: Option<WallTime>,
    retry_at: Option<WallTime>,
}

fn publish<T: Serialize>(
    directory: &Directory,
    file: &str,
    purpose: &str,
    value: &T,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    directory
        .atomic_write(file.as_ref(), purpose, &bytes, PublishMode::Replace)
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn write_result(
    directory: &Directory,
    file: &str,
    purpose: &str,
    at: WallTime,
    state: &str,
    error: Option<String>,
) -> Result<(), String> {
    write_task_result(directory, file, purpose, at, state, error, 0)
}

fn write_task_result(
    directory: &Directory,
    file: &str,
    purpose: &str,
    at: WallTime,
    state: &str,
    error: Option<String>,
    failures: u32,
) -> Result<(), String> {
    let result = TaskResult {
        extra: Default::default(),
        state: state.into(),
        at,
        error,
        failures,
    };
    publish(directory, file, purpose, &result)
}

fn write_gc_result(
    directory: &Directory,
    at: WallTime,
    state: &str,
    error: Option<String>,
    failures: u32,
) -> Result<(), String> {
    write_task_result(directory, GC_RESULT, "tick-gc", at, state, error, failures)
}

fn read_gc_result(directory: &Directory) -> Result<Option<TaskResult>, String> {
    match directory.read_bounded(GC_RESULT.as_ref(), 64 * 1024) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| error.to_string()),
        Err(error) if error.is_not_found() => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

/// What one check of the scheduled collection found, for the tick's report.
#[derive(Debug, PartialEq, Eq)]
struct GcReport {
    state: &'static str,
    /// Context for an operator, such as the age and next retry of an old failure.
    note: Option<String>,
}

impl GcReport {
    fn state(state: &'static str) -> Self {
        Self { state, note: None }
    }
}

/// Seconds to wait after the given number of consecutive unfinished attempts:
/// 15 minutes, then 1 hour, 4 hours and 16 hours, capped at the daily cadence.
fn gc_retry_delay_seconds(failures: u32) -> u64 {
    let mut delay = GC_RETRY_BASE_SECONDS;
    for _ in 1..failures {
        delay = delay.saturating_mul(GC_RETRY_FACTOR);
        if delay >= DAY_SECONDS {
            return DAY_SECONDS;
        }
    }
    delay.min(DAY_SECONDS)
}

fn gc_retry_at(result: &TaskResult) -> Result<WallTime, String> {
    result
        .at
        .checked_add(
            DurationMicros::from_seconds(gc_retry_delay_seconds(result.failures))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())
}

fn describe_span(micros: i64) -> String {
    let seconds = micros.max(0) / 1_000_000;
    if seconds < 60 {
        "under a minute".to_owned()
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else if seconds < DAY_SECONDS as i64 {
        format!("{}h{:02}m", seconds / 3_600, seconds % 3_600 / 60)
    } else {
        format!("{}d{:02}h", seconds / 86_400, seconds % 86_400 / 3_600)
    }
}

/// A recorded failure that is waiting for its retry. It is old news, not a
/// fresh failure: the tick reports its age and the next attempt and does not
/// fail.
fn gc_retry_pending(result: &TaskResult, retry_at: WallTime, now: WallTime) -> GcReport {
    GcReport {
        state: "retry_pending",
        note: Some(format!(
            "the last scheduled GC failed {} ago ({}); attempt {} retries in {}",
            describe_span(now.as_micros().saturating_sub(result.at.as_micros())),
            result.error.as_deref().unwrap_or("no saved cause"),
            result.failures.max(1).saturating_add(1),
            describe_span(retry_at.as_micros().saturating_sub(now.as_micros())),
        )),
    }
}

enum GcUnfinished {
    /// A worker holds its lock, or has been launched too recently to judge.
    Live(&'static str),
    /// No worker holds the lock: the failure was just recorded.
    Interrupted(String),
    /// The worker finished between the snapshot and the lock probe.
    Settled(TaskResult),
}

/// A result that still reads queued or running is trusted only while the
/// worker lock is held; a free lock proves the worker is gone.
fn settle_unfinished_gc(
    directory: &Directory,
    now: WallTime,
    result: &TaskResult,
) -> Result<GcUnfinished, String> {
    let live = if result.state == "running" {
        "running"
    } else {
        "queued"
    };
    if result.state == "queued"
        && now.as_micros().saturating_sub(result.at.as_micros()) < SEED_WORK_RETRY_MICROS
    {
        return Ok(GcUnfinished::Live(live));
    }
    let _worker = match directory.try_lock("gc-worker.lock".as_ref(), LockMode::Exclusive) {
        Ok(lock) => lock,
        Err(provenant_os::fs::FsError::LockTimeout { .. }) => return Ok(GcUnfinished::Live(live)),
        Err(error) => return Err(error.to_string()),
    };
    // Re-read the atomic result under custody: the worker may have completed
    // after our snapshot.
    let Some(result) = read_gc_result(directory)? else {
        return Err("scheduled GC result disappeared".to_owned());
    };
    if !matches!(result.state.as_str(), "running" | "queued") {
        return Ok(GcUnfinished::Settled(result));
    }
    let error = "scheduled GC interrupted; next: provenant gc --apply".to_owned();
    write_gc_result(
        directory,
        now,
        "failed",
        Some(error.clone()),
        result.failures.saturating_add(1),
    )?;
    Ok(GcUnfinished::Interrupted(error))
}

/// The cadence lock is held by the caller. The daily deadline advances only
/// when a worker completes (`advance_gc_cadence`), so a failed or interrupted
/// collection stays due and is retried with a bounded backoff from its recorded
/// failure count. A spawn error is such a failure. A collection that is queued
/// or running is never launched twice. Only a failure found now is an error;
/// an older one waiting for its retry is reported with its age.
fn queue_gc(
    directory: &Directory,
    now: WallTime,
    spawn: impl FnOnce() -> Result<(), String>,
) -> Result<GcReport, String> {
    let cadence = read_cadence(directory).map_err(|error| error.to_string())?;
    let mut failures = 0;
    let mut retrying = false;
    if let Some(mut result) = read_gc_result(directory)? {
        if matches!(result.state.as_str(), "running" | "queued") {
            match settle_unfinished_gc(directory, now, &result)? {
                GcUnfinished::Live(state) => return Ok(GcReport::state(state)),
                GcUnfinished::Interrupted(error) => return Err(error),
                GcUnfinished::Settled(settled) => result = settled,
            }
        }
        if result.state == "failed" {
            let retry_at = gc_retry_at(&result)?;
            if now < retry_at {
                return Ok(gc_retry_pending(&result, retry_at, now));
            }
            failures = result.failures;
            retrying = true;
        }
    }
    if !retrying && !gc_due(&cadence, now) {
        return Ok(GcReport::state("not_due"));
    }
    write_gc_result(directory, now, "queued", None, failures)?;
    if let Err(error) = spawn() {
        write_gc_result(
            directory,
            now,
            "failed",
            Some(error.clone()),
            failures.saturating_add(1),
        )?;
        return Err(error);
    }
    Ok(GcReport::state("queued"))
}

/// Move the daily deadline a day past a completed collection.
fn advance_gc_cadence(directory: &Directory, completed_at: WallTime) -> Result<(), String> {
    let _lock = bounded_lock(directory, "cadence.lock")?;
    let mut cadence = read_cadence(directory).map_err(|error| error.to_string())?;
    cadence.next_gc_at = Some(
        completed_at
            .checked_add(
                DurationMicros::from_seconds(DAY_SECONDS).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?,
    );
    publish(directory, "cadence.json", "tick-cadence", &cadence)
}

/// Detached daily collection uses the existing collector and its custody. Its
/// durable result replaces discarded detached-process stdout and stderr.
pub(crate) fn gc_worker(globals: &Globals, workers: &dyn WorkerObserver) -> u8 {
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        if provenant_cli::fabric::in_lane() {
            return Err("tick GC requires host custody".into());
        }
        let resolved = crate::roots::resolve(
            globals,
            crate::roots::Workspace::Home,
            crate::roots::Product::Required,
        )?;
        let directory = provenant_config::private_directory(
            &resolved.roots.state.as_path().join("tick"),
            true,
        )?;
        let clock = SystemClock;
        let _worker = match directory.try_lock("gc-worker.lock".as_ref(), LockMode::Exclusive) {
            Ok(lock) => lock,
            Err(provenant_os::fs::FsError::LockTimeout { .. }) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let failures = read_gc_result(&directory)
            .ok()
            .flatten()
            .map_or(0, |result| result.failures);
        write_gc_result(&directory, clock.wall_now()?, "running", None, failures)?;
        let collected = (|| -> Result<(), String> {
            let config = provenant_cli::config_reads::read_for(&resolved.roots, "tick")
                .map_err(|error| error.to_string())?;
            crate::worktrees::scheduled_gc(
                globals,
                config
                    .gc
                    .idle_seconds()
                    .map_err(|error| error.to_string())?,
                workers,
            )
        })()
        .and_then(|()| {
            advance_gc_cadence(
                &directory,
                clock.wall_now().map_err(|error| error.to_string())?,
            )
        });
        match &collected {
            Ok(()) => write_gc_result(&directory, clock.wall_now()?, "complete", None, 0)?,
            Err(error) => write_gc_result(
                &directory,
                clock.wall_now()?,
                "failed",
                Some(error.clone()),
                failures.saturating_add(1),
            )?,
        }
        // The collection fetched every project's default branch, so any seed
        // that is now behind can be rebuilt. This only queues a detached
        // worker; a failure to queue is recorded for the tick and never fails
        // the collection.
        let _ = queue_seed(
            &directory,
            clock.wall_now()?,
            Some(SeedRequest::full()),
            || spawn_seed_worker(resolved.roots.state.as_path(), &resolved.home),
        );
        collected.map_err(Into::into)
    })();
    match result {
        Ok(()) => 0,
        Err(error) => crate::fail(
            "internal tick-gc",
            Outcome::Error,
            "tick_gc",
            &error.to_string(),
            globals,
        ),
    }
}

const SEED_RESULT: &str = "seed-result.json";

fn bounded_lock(directory: &Directory, name: &str) -> Result<LockGuard, String> {
    let clock = SystemClock;
    let budget = WaitBudget::up_to(
        clock.mono_now(),
        DurationMicros::from_seconds(5).map_err(|error| error.to_string())?,
    );
    directory
        .lock(
            name.as_ref(),
            LockMode::Exclusive,
            budget,
            &provenant_core::CancelToken::new(),
            &clock,
        )
        .map_err(|error| error.to_string())
}

fn seed_queue_lock(directory: &Directory) -> Result<LockGuard, String> {
    bounded_lock(directory, "seed-queue.lock")
}

fn read_seed_queue(directory: &Directory) -> Result<SeedQueue, String> {
    match directory.read_bounded(SEED_QUEUE.as_ref(), 256 * 1024) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| error.to_string()),
        Err(error) if error.is_not_found() => Ok(SeedQueue::default()),
        Err(error) => Err(error.to_string()),
    }
}

fn write_seed_queue(directory: &Directory, queue: &SeedQueue) -> Result<(), String> {
    let bytes = serde_json::to_vec(queue).map_err(|error| error.to_string())?;
    directory
        .atomic_write(
            SEED_QUEUE.as_ref(),
            "tick-seed-queue",
            &bytes,
            PublishMode::Replace,
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn merge_seed_requests(left: Option<SeedRequest>, right: SeedRequest) -> SeedRequest {
    let Some(left) = left else {
        return right;
    };
    // Full work reevaluates every project, so it dominates any busy retry.
    let (SeedScope::Busy(left_keys), SeedScope::Busy(right_keys)) = (left.scope, right.scope)
    else {
        return SeedRequest::full();
    };
    let keys: BTreeSet<String> = left_keys.into_iter().chain(right_keys).collect();
    SeedRequest::busy(keys.into_iter().take(MAX_BUSY_SEED_PROJECTS).collect())
}

impl SeedQueue {
    fn enqueue(&mut self, request: SeedRequest, now: WallTime) {
        if matches!(&request.scope, SeedScope::Full) {
            self.retry = None;
            self.retry_at = None;
        }
        self.pending = Some(merge_seed_requests(self.pending.take(), request));
        self.pending_at = Some(now);
    }

    fn promote_due_retry(&mut self, now: WallTime) {
        if self.retry_at.is_some_and(|at| {
            now.as_micros().saturating_sub(at.as_micros()) >= SEED_WORK_RETRY_MICROS
        }) {
            if let Some(retry) = self.retry.take() {
                self.pending = Some(merge_seed_requests(self.pending.take(), retry));
                self.pending_at = Some(now);
            }
            self.retry_at = None;
        }
    }
}

/// Admit work under a short queue lock. Full requests dominate busy retries;
/// an active worker drains pending work before releasing its single-flight lock.
fn queue_seed(
    directory: &Directory,
    now: WallTime,
    request: Option<SeedRequest>,
    spawn: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let _queue_lock = seed_queue_lock(directory)?;
    let mut queue = read_seed_queue(directory)?;
    if let Some(request) = request {
        queue.enqueue(request, now);
    }

    // A due retry is promoted only after a full request has had a chance to
    // clear it. Its explicit scope survives queueing and detached startup.
    queue.promote_due_retry(now);
    let worker_lock = match directory.try_lock("seed-worker.lock".as_ref(), LockMode::Exclusive) {
        Ok(lock) => Some(lock),
        Err(provenant_os::fs::FsError::LockTimeout { .. }) => None,
        Err(error) => return Err(error.to_string()),
    };

    if let Some(worker_lock) = worker_lock {
        // The OS lock proves any recorded active request belongs to a dead
        // worker. Replay it idempotently, while respecting a queued Full pass.
        if let Some(active) = queue.active.take() {
            queue.pending = Some(merge_seed_requests(queue.pending.take(), active));
            queue.pending_at = Some(now);
            queue.launch_at = None;
            queue.active_at = None;
        }
        if queue.pending.is_none() {
            write_seed_queue(directory, &queue)?;
            drop(worker_lock);
            return Ok(());
        }
        if queue.launch_at.is_some_and(|at| {
            now.as_micros().saturating_sub(at.as_micros()) < SEED_WORK_RETRY_MICROS
        }) {
            write_seed_queue(directory, &queue)?;
            drop(worker_lock);
            return Ok(());
        }
        queue.pending_at.get_or_insert(now);
        queue.launch_at = Some(now);
        write_seed_queue(directory, &queue)?;
        write_seed_progress(directory, now, "queued", None)?;
        // Keep the queue lock while releasing the probe lock and spawning:
        // the child waits for the queue lock before claiming the request.
        drop(worker_lock);
        if let Err(error) = spawn() {
            write_seed_progress(directory, now, "failed", Some(error.clone()))?;
            queue.pending_at = Some(now);
            queue.launch_at = None;
            write_seed_queue(directory, &queue)?;
            return Err(error);
        }
        return Ok(());
    }

    write_seed_queue(directory, &queue)?;
    Ok(())
}

fn spawn_seed_worker(state: &std::path::Path, home: &std::path::Path) -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let argv = [
        executable.into_os_string(),
        "--state-dir".into(),
        state.as_os_str().to_owned(),
        "internal".into(),
        "tick-seed".into(),
    ];
    provenant_os::exec::spawn_detached(&argv, home).map_err(|error| error.to_string())
}

fn truncate_bytes(message: String, limit: usize) -> String {
    if message.len() <= limit {
        return message;
    }
    let mut end = limit;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &message[..end])
}

/// Find delayed busy retries and abandoned pending or active jobs.
fn seed_retry_due(directory: &Directory, now: WallTime) -> bool {
    read_seed_queue(directory).is_ok_and(|queue| {
        [
            queue.retry_at,
            queue.pending_at,
            queue.launch_at,
            queue.active_at,
        ]
        .into_iter()
        .flatten()
        .any(|at| now.as_micros().saturating_sub(at.as_micros()) >= SEED_WORK_RETRY_MICROS)
    })
}

/// What one project's refresh came to. Only a failure fails the pass; a skip
/// names a repair the project's owner can make and leaves every other project's
/// seed alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum SeedProjectOutcome {
    Built,
    Current,
    Busy,
    /// No selected Cargo manifest: nothing to seed.
    NotCargo,
    Skipped {
        reason: crate::target_seed_build::SkipReason,
        detail: String,
        repair: String,
    },
    Failed {
        error: String,
    },
}

impl SeedProjectOutcome {
    fn skipped(skip: &crate::target_seed_build::SeedSkip) -> Self {
        Self::Skipped {
            reason: skip.reason,
            detail: truncate_seed_error(skip.to_string()),
            repair: skip.reason.repair().to_owned(),
        }
    }

    fn failed(message: String) -> Self {
        Self::Failed {
            error: truncate_seed_error(message),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SeedProjectRecord {
    project: String,
    project_key: String,
    #[serde(flatten)]
    outcome: SeedProjectOutcome,
}

impl SeedProjectRecord {
    fn new(project: &SeedProject, outcome: SeedProjectOutcome) -> Self {
        Self {
            project: truncate_bytes(
                project.root.display().to_string(),
                MAX_SEED_PROJECT_PATH_BYTES,
            ),
            project_key: project.project_key.clone(),
            outcome,
        }
    }
}

/// The last seed pass: its state and every registered project's own outcome.
/// A scoped retry replaces only its projects' records, so one project's result
/// never hides another's.
#[derive(Serialize, Deserialize)]
struct SeedResult {
    #[serde(flatten)]
    extra: provenant_records::ExtraFields,
    state: String,
    at: WallTime,
    /// A cause that belongs to no project, such as an unreadable registry.
    error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    projects: Vec<SeedProjectRecord>,
    /// Records beyond `MAX_SEED_PROJECT_RECORDS` that were not kept.
    #[serde(default, skip_serializing_if = "is_zero")]
    omitted_projects: usize,
}

impl SeedResult {
    fn new(
        at: WallTime,
        state: &str,
        error: Option<String>,
        mut projects: Vec<SeedProjectRecord>,
    ) -> Self {
        let omitted_projects = projects.len().saturating_sub(MAX_SEED_PROJECT_RECORDS);
        projects.truncate(MAX_SEED_PROJECT_RECORDS);
        Self {
            extra: Default::default(),
            state: state.into(),
            at,
            error,
            projects,
            omitted_projects,
        }
    }

    /// A finished pass is failed when anything failed, else busy while any
    /// project's build lock was held, else complete. A skip is none of these.
    fn finished(at: WallTime, error: Option<String>, projects: Vec<SeedProjectRecord>) -> Self {
        let any = |wanted: fn(&SeedProjectOutcome) -> bool| {
            projects.iter().any(|record| wanted(&record.outcome))
        };
        let state = if error.is_some()
            || any(|outcome| matches!(outcome, SeedProjectOutcome::Failed { .. }))
        {
            "failed"
        } else if any(|outcome| matches!(outcome, SeedProjectOutcome::Busy)) {
            "busy"
        } else {
            "complete"
        };
        Self::new(at, state, error, projects)
    }
}

fn read_seed_result(directory: &Directory) -> Result<Option<SeedResult>, String> {
    match directory.read_bounded(SEED_RESULT.as_ref(), MAX_SEED_RESULT_BYTES) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| error.to_string()),
        Err(error) if error.is_not_found() => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn write_seed_result(directory: &Directory, result: &SeedResult) -> Result<(), String> {
    publish(directory, SEED_RESULT, "tick-seed", result)
}

/// Record that work is queued, running or could not start. The projects' last
/// outcomes stay visible while it happens.
fn write_seed_progress(
    directory: &Directory,
    at: WallTime,
    state: &str,
    error: Option<String>,
) -> Result<(), String> {
    let projects = read_seed_result(directory)
        .ok()
        .flatten()
        .map(|result| result.projects)
        .unwrap_or_default();
    write_seed_result(directory, &SeedResult::new(at, state, error, projects))
}

#[derive(Debug, Default)]
struct SeedOutcome {
    busy_project_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SeedProject {
    root: PathBuf,
    project_key: String,
}

#[derive(Debug, Default)]
struct SeedDiscoveryError {
    project_key: String,
    project: String,
    message: String,
}

impl SeedDiscoveryError {
    fn into_record(self) -> SeedProjectRecord {
        SeedProjectRecord {
            project: truncate_bytes(self.project, MAX_SEED_PROJECT_PATH_BYTES),
            project_key: self.project_key,
            outcome: SeedProjectOutcome::Failed {
                error: self.message,
            },
        }
    }
}

#[derive(Debug, Default)]
struct SeedDiscovery {
    projects: Vec<SeedProject>,
    errors: Vec<SeedDiscoveryError>,
    omitted_errors: usize,
}

impl SeedDiscovery {
    #[cfg(test)]
    #[allow(dead_code)] // The worktree driver includes this module without libtest.
    fn from_paths(paths: Vec<PathBuf>) -> Self {
        Self {
            projects: paths
                .into_iter()
                .map(|root| SeedProject {
                    project_key: provenant_run::target_seed::project_key(&root),
                    root,
                })
                .collect(),
            ..Self::default()
        }
    }

    fn record_error(&mut self, project_key: String, project: String, message: String) {
        if self.errors.len() < MAX_SEED_DISCOVERY_ERRORS {
            let message = truncate_seed_error(message);
            self.errors.push(SeedDiscoveryError {
                project_key,
                project,
                message,
            });
        } else {
            self.omitted_errors += 1;
        }
    }
}

fn truncate_seed_error(message: String) -> String {
    truncate_bytes(message, MAX_SEED_ERROR_BYTES)
}

/// Refresh each project in turn and record each one's own outcome. A busy build
/// is a no-op, an unresolved verify base is a skip, and no project's failure
/// stops the others.
fn refresh_projects(
    projects: &[SeedProject],
    host_state: &std::path::Path,
    refresh: impl Fn(
        &std::path::Path,
    ) -> Result<crate::target_seed_build::Refresh, Box<dyn std::error::Error>>,
) -> Vec<SeedProjectRecord> {
    use crate::target_seed_build::Refresh;
    projects
        .iter()
        .map(|project| {
            let outcome = match provenant_run::target_seed::cargo_build_directory(
                &project.root,
                host_state,
            ) {
                Ok(None) => SeedProjectOutcome::NotCargo,
                Err(error) => SeedProjectOutcome::failed(error.to_string()),
                Ok(Some(_)) => match refresh(&project.root) {
                    Ok(Refresh::Built(_)) => SeedProjectOutcome::Built,
                    Ok(Refresh::NotCargo) => SeedProjectOutcome::NotCargo,
                    Ok(Refresh::Current) => SeedProjectOutcome::Current,
                    Ok(Refresh::Busy) => SeedProjectOutcome::Busy,
                    Err(error) => match crate::target_seed_build::skip_of(error.as_ref()) {
                        Some(skip) => SeedProjectOutcome::skipped(skip),
                        None => SeedProjectOutcome::failed(error.to_string()),
                    },
                },
            };
            SeedProjectRecord::new(project, outcome)
        })
        .collect()
}

/// Run one single-flight pass under `seed-worker.lock`. A second worker, or a
/// disabled seed, returns without touching the result file.
#[cfg(test)]
#[allow(dead_code)] // The worktree driver includes this module without libtest.
fn seed_pass(
    directory: &Directory,
    host_state: &Path,
    clock: &dyn Clock,
    enabled: bool,
    request: &SeedRequest,
    projects: impl FnOnce() -> Result<SeedDiscovery, String>,
    refresh: impl Fn(
        &std::path::Path,
    ) -> Result<crate::target_seed_build::Refresh, Box<dyn std::error::Error>>,
) -> Result<(), String> {
    let _worker = match directory.try_lock("seed-worker.lock".as_ref(), LockMode::Exclusive) {
        Ok(lock) => lock,
        Err(provenant_os::fs::FsError::LockTimeout { .. }) => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    seed_pass_locked(
        directory, host_state, clock, enabled, request, projects, refresh,
    )
    .map(|_| ())
}

fn seed_pass_locked(
    directory: &Directory,
    host_state: &Path,
    clock: &dyn Clock,
    enabled: bool,
    request: &SeedRequest,
    projects: impl FnOnce() -> Result<SeedDiscovery, String>,
    refresh: impl Fn(
        &std::path::Path,
    ) -> Result<crate::target_seed_build::Refresh, Box<dyn std::error::Error>>,
) -> Result<SeedOutcome, String> {
    let at = || clock.wall_now().map_err(|error| error.to_string());
    let retry_project_keys = request.project_keys();
    if !enabled {
        write_seed_result(
            directory,
            &SeedResult::new(at()?, "disabled", None, Vec::new()),
        )?;
        return Ok(SeedOutcome::default());
    }
    let prior = read_seed_result(directory)
        .ok()
        .flatten()
        .map(|result| result.projects)
        .unwrap_or_default();
    write_seed_progress(directory, at()?, "running", None)?;
    let (records, error, busy_project_keys) = match projects() {
        Ok(mut discovery) => {
            if !retry_project_keys.is_empty() {
                discovery
                    .projects
                    .retain(|project| retry_project_keys.contains(&project.project_key));
                discovery
                    .errors
                    .retain(|error| retry_project_keys.contains(&error.project_key));
            }
            let mut fresh = refresh_projects(&discovery.projects, host_state, refresh);
            fresh.extend(
                discovery
                    .errors
                    .into_iter()
                    .map(SeedDiscoveryError::into_record),
            );
            let error = (discovery.omitted_errors > 0).then(|| {
                format!(
                    "{} additional registered project layout error(s) omitted",
                    discovery.omitted_errors
                )
            });
            let busy_project_keys = fresh
                .iter()
                .filter(|record| matches!(record.outcome, SeedProjectOutcome::Busy))
                .map(|record| record.project_key.clone())
                .take(MAX_BUSY_SEED_PROJECTS)
                .collect();
            // A scoped retry answers only for its own projects; the others
            // keep the outcome the last full pass recorded.
            let mut records: Vec<SeedProjectRecord> = prior
                .into_iter()
                .filter(|record| {
                    !retry_project_keys.is_empty()
                        && !retry_project_keys.contains(&record.project_key)
                })
                .collect();
            records.extend(fresh);
            (records, error, busy_project_keys)
        }
        Err(error) => (prior, Some(error), Vec::new()),
    };
    write_seed_result(directory, &SeedResult::finished(at()?, error, records))?;
    Ok(SeedOutcome { busy_project_keys })
}

fn claim_seed_job(
    directory: &Directory,
    now: WallTime,
) -> Result<Option<(LockGuard, SeedRequest)>, String> {
    let _queue_lock = seed_queue_lock(directory)?;
    let worker_lock = match directory.try_lock("seed-worker.lock".as_ref(), LockMode::Exclusive) {
        Ok(lock) => lock,
        Err(provenant_os::fs::FsError::LockTimeout { .. }) => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let mut queue = read_seed_queue(directory)?;
    if let Some(active) = queue.active.take() {
        // Acquiring the worker lock proves an earlier active record is stale.
        queue.pending = Some(merge_seed_requests(queue.pending.take(), active));
        queue.pending_at = Some(now);
        queue.active_at = None;
    }
    queue.promote_due_retry(now);
    let Some(request) = queue.pending.take() else {
        queue.launch_at = None;
        write_seed_queue(directory, &queue)?;
        drop(worker_lock);
        return Ok(None);
    };
    queue.pending_at = None;
    queue.launch_at = None;
    if matches!(&request.scope, SeedScope::Full) {
        queue.retry = None;
        queue.retry_at = None;
    }
    queue.active = Some(request.clone());
    queue.active_at = Some(now);
    write_seed_queue(directory, &queue)?;
    write_seed_progress(directory, now, "running", None)?;
    drop(_queue_lock);
    Ok(Some((worker_lock, request)))
}

/// Publish completion and either continue with admitted work or release the
/// worker lock while still holding the queue lock, so no producer can slip in
/// between the final pending check and worker exit.
fn finish_seed_job(
    directory: &Directory,
    now: WallTime,
    worker_lock: &mut Option<LockGuard>,
    outcome: SeedOutcome,
) -> Result<Option<SeedRequest>, String> {
    let queue_lock = seed_queue_lock(directory)?;
    finish_seed_job_with_lock(directory, queue_lock, now, worker_lock, outcome)
}

fn finish_seed_job_with_lock(
    directory: &Directory,
    _queue_lock: LockGuard,
    now: WallTime,
    worker_lock: &mut Option<LockGuard>,
    outcome: SeedOutcome,
) -> Result<Option<SeedRequest>, String> {
    let mut queue = read_seed_queue(directory)?;
    queue.active = None;
    queue.active_at = None;
    queue.launch_at = None;
    if outcome.busy_project_keys.is_empty() {
        queue.retry = None;
        queue.retry_at = None;
    } else {
        let retry = SeedRequest::busy(outcome.busy_project_keys);
        match queue.pending.take() {
            Some(pending) if matches!(&pending.scope, SeedScope::Full) => {
                // The full pass covers this busy result.
                queue.pending = Some(pending);
                queue.pending_at.get_or_insert(now);
                queue.retry = None;
                queue.retry_at = None;
            }
            Some(pending) => {
                queue.pending = Some(merge_seed_requests(Some(pending), retry));
                queue.pending_at.get_or_insert(now);
                queue.retry = None;
                queue.retry_at = None;
            }
            None => {
                queue.retry = Some(retry);
                queue.retry_at = Some(now);
            }
        }
    }

    if let Some(next) = queue.pending.take() {
        queue.pending_at = None;
        if matches!(&next.scope, SeedScope::Full) {
            queue.retry = None;
            queue.retry_at = None;
        }
        queue.active = Some(next.clone());
        queue.active_at = Some(now);
        write_seed_queue(directory, &queue)?;
        write_seed_progress(directory, now, "running", None)?;
        Ok(Some(next))
    } else {
        write_seed_queue(directory, &queue)?;
        // Keep this order: a waiting producer can only acquire the queue lock
        // after the single-flight lock has become available.
        drop(worker_lock.take());
        Ok(None)
    }
}

/// Resolve registered projects through the utility's layout owner, which
/// classifies local bare provenance; a bare common directory has no project.
fn registered_seed_projects(
    bindings: &[provenant_config::ProjectBinding],
    retry_project_keys: &[String],
    executor: &dyn Executor,
    utility: &provenant_confinement::UtilityRuntime,
    cancel: &provenant_core::CancelToken,
) -> SeedDiscovery {
    let mut discovery = SeedDiscovery::default();
    let mut seen = BTreeSet::new();
    for binding in bindings {
        let common = binding.common_dir();
        let project_key = provenant_run::target_seed::project_key(common);
        // Busy retries are scoped to the projects that actually had a seed
        // lock. Do not re-inventory other registrations, which may include a
        // separate project whose earlier discovery/build failed.
        if !retry_project_keys.is_empty() && !retry_project_keys.contains(&project_key) {
            continue;
        }
        let layout = match provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))
            .and_then(|budget| {
                utility.project_layout_for_binding(executor, binding, budget, cancel)
            }) {
            Ok(layout) => layout,
            Err(error) => {
                discovery.record_error(
                    project_key,
                    common.display().to_string(),
                    format!(
                        "cannot verify registered Git directory {}: {error}",
                        common.display()
                    ),
                );
                continue;
            }
        };
        // A bare common directory owns host Git state, not a Cargo project.
        if let Some(project) = layout.project()
            && seen.insert(project.to_path_buf())
        {
            discovery.projects.push(SeedProject {
                root: project.to_path_buf(),
                project_key,
            });
        }
    }
    discovery
}

/// Detached cargo-seed refresh queued by the daily collector. The build can
/// run for hours, so it never runs inline in the tick; its durable result
/// replaces discarded detached-process output.
pub(crate) fn seed_worker(globals: &Globals) -> u8 {
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        if provenant_cli::fabric::in_lane() {
            return Err("tick seed refresh requires host custody".into());
        }
        let resolved = crate::roots::resolve(
            globals,
            crate::roots::Workspace::Home,
            crate::roots::Product::Required,
        )?;
        let directory = provenant_config::private_directory(
            &resolved.roots.state.as_path().join("tick"),
            true,
        )?;
        let config = provenant_cli::config_reads::read_for(&resolved.roots, "tick")?;
        let state = resolved.roots.state.as_path();
        let Some((worker, mut request)) = claim_seed_job(&directory, SystemClock.wall_now()?)?
        else {
            return Ok(());
        };
        let mut worker_lock = Some(worker);
        loop {
            let outcome = seed_pass_locked(
                &directory,
                state,
                &SystemClock,
                config.domains.fabric.lanes.cargo_seed.enabled,
                &request,
                || {
                    let projects = provenant_config::registered_gc_projects(&resolved.roots)
                        .map_err(|error| error.to_string())?;
                    let git = provenant_os::git::git_executable()
                        .ok_or_else(|| "Git executable unavailable".to_owned())?;
                    let executor = SystemExecutor::new(1).map_err(|error| error.to_string())?;
                    let utility = provenant_confinement::UtilityRuntime::read_only(&git, state)
                        .map_err(|error| error.to_string())?;
                    Ok(registered_seed_projects(
                        &projects,
                        request.project_keys(),
                        &executor,
                        &utility,
                        &provenant_core::CancelToken::new(),
                    ))
                },
                |project| crate::target_seed_build::refresh(state, project),
            )?;
            let now = SystemClock.wall_now()?;
            match finish_seed_job(&directory, now, &mut worker_lock, outcome)? {
                Some(next) => request = next,
                None => break,
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => 0,
        Err(error) => crate::fail(
            "internal tick-seed",
            Outcome::Error,
            "tick_seed",
            &error.to_string(),
            globals,
        ),
    }
}

fn seed_result_state(state: &str) -> &'static str {
    match state {
        "queued" => "queued",
        "running" => "running",
        "complete" => "complete",
        "busy" => "busy",
        "disabled" => "disabled",
        _ => "last_failed",
    }
}

/// Project persisted errors onto a small, safe set of operator-facing causes.
/// The stored detail can include command arguments, stderr, or repository paths.
fn safe_seed_failure_diagnostic(error: Option<&str>) -> &'static str {
    let Some(error) = error.filter(|error| !error.is_empty()) else {
        return "The seed refresh failed without a saved cause. Run `provenant worktree seed-build` in the affected repository for current details.";
    };
    if error.contains("shared Cargo target seed is unavailable while") {
        "A Rust or Cargo override prevents reliable shared-seed reuse. Review host overrides, then run `provenant worktree seed-build` in the affected repository."
    } else if error.contains("Cargo configuration cannot be represented")
        || error.contains("cargo_config_")
    {
        "Cargo configuration selects a build setting the shared seed cannot represent. Review the host and repository Cargo config, then run `provenant worktree seed-build` in the affected repository."
    } else if error.contains("cannot verify registered Git directory")
        || error.contains("Git executable unavailable")
        || error.contains("git unavailable")
    {
        "A registered Git repository could not be verified. Check its local Git or worktree state, then rerun `provenant tick`."
    } else if error.contains("verify base") || error.contains("verify-base") {
        "The configured verify base could not be read or lacks the selected Cargo workspace. Refresh or correct that ref, then run `provenant worktree seed-build` in the affected repository."
    } else if error.contains("command failed:") || error.contains("running seed command:") {
        "A Cargo or Git command failed while refreshing the seed. Run `provenant worktree seed-build` in the affected repository for local details."
    } else {
        "A registered repository's seed could not be refreshed or verified. Run `provenant worktree seed-build` in the affected repository for local details."
    }
}

/// The tick's one-line account of the last seed pass. A finished pass reports
/// how many projects ended each way, so a skipped or failed project never hides
/// the ones that were seeded.
fn seed_summary(result: &SeedResult) -> String {
    let word = seed_result_state(&result.state);
    if matches!(word, "queued" | "running" | "disabled") || result.projects.is_empty() {
        return word.to_owned();
    }
    let (mut ok, mut skipped, mut busy, mut failed) = (0usize, 0usize, 0usize, 0usize);
    for record in &result.projects {
        match record.outcome {
            SeedProjectOutcome::Built | SeedProjectOutcome::Current => ok += 1,
            SeedProjectOutcome::Skipped { .. } => skipped += 1,
            SeedProjectOutcome::Busy => busy += 1,
            SeedProjectOutcome::Failed { .. } => failed += 1,
            SeedProjectOutcome::NotCargo => {}
        }
    }
    failed += usize::from(result.error.is_some());
    let parts: Vec<String> = [
        ("ok", ok, true),
        ("skipped", skipped, false),
        ("busy", busy, false),
        ("failed", failed, false),
    ]
    .into_iter()
    .filter(|(_, count, always)| *count > 0 || (*always && ok + skipped + busy + failed == 0))
    .map(|(name, count, _)| format!("{name}:{count}"))
    .collect();
    parts.join(" ")
}

fn push_distinct(notes: &mut Vec<String>, note: String) {
    if !notes.contains(&note) {
        notes.push(note);
    }
}

/// What an operator does next, from the typed outcomes and the safe projection
/// of each failure. Project paths and command output stay in the result file.
fn seed_diagnostic(result: &SeedResult) -> Option<String> {
    let mut notes = Vec::new();
    let failures = result
        .projects
        .iter()
        .filter_map(|record| match &record.outcome {
            SeedProjectOutcome::Failed { error } => Some(error.as_str()),
            _ => None,
        })
        .chain(result.error.as_deref());
    for failure in failures {
        push_distinct(
            &mut notes,
            safe_seed_failure_diagnostic(Some(failure)).to_owned(),
        );
    }
    if result.state == "failed" && notes.is_empty() {
        push_distinct(&mut notes, safe_seed_failure_diagnostic(None).to_owned());
    }
    let mut skipped = std::collections::BTreeMap::new();
    for record in &result.projects {
        if let SeedProjectOutcome::Skipped { reason, .. } = &record.outcome {
            *skipped.entry(*reason).or_insert(0usize) += 1;
        }
    }
    for (reason, count) in skipped {
        push_distinct(
            &mut notes,
            format!(
                "{count} project{} skipped because {}. {}",
                if count == 1 { "" } else { "s" },
                reason.cause(),
                reason.repair()
            ),
        );
    }
    (!notes.is_empty()).then(|| notes.join(" "))
}

const SEED_RESULT_UNREADABLE: &str = "The local seed result could not be read. Run `provenant worktree seed-build` in the affected repository for current details.";

fn target_seed_task(directory: &Directory) -> Task {
    match read_seed_result(directory) {
        Ok(Some(result)) => Task::reported(
            "target_seed",
            seed_summary(&result),
            seed_diagnostic(&result),
        ),
        Ok(None) => Task::idle("target_seed", "idle"),
        Err(_) => Task::idle_with_diagnostic("target_seed", "last_failed", SEED_RESULT_UNREADABLE),
    }
}

#[derive(Default, Serialize)]
pub(crate) struct Graphs {
    pub advanced: usize,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Serialize)]
struct Task {
    task: &'static str,
    state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diagnostic: Option<String>,
}

impl Task {
    fn run(task: &'static str, action: impl FnOnce() -> Result<&'static str, String>) -> Self {
        match action() {
            Ok(state) => Self::idle(task, state),
            Err(error) => Self::failed(task, error),
        }
    }
    fn failed(task: &'static str, error: String) -> Self {
        Self {
            task,
            state: "failed".into(),
            error: Some(error),
            diagnostic: None,
        }
    }
    /// A check whose success carries a note. Only a failure found by this
    /// check is an error; an old one the report merely mentions is not.
    fn gc(action: impl FnOnce() -> Result<GcReport, String>) -> Self {
        match action() {
            Ok(report) => Self::reported("gc", report.state, report.note),
            Err(error) => Self::failed("gc", error),
        }
    }
    fn reported(task: &'static str, state: impl Into<String>, diagnostic: Option<String>) -> Self {
        Self {
            task,
            state: state.into(),
            error: None,
            diagnostic,
        }
    }
    fn idle(task: &'static str, state: &'static str) -> Self {
        Self::reported(task, state, None)
    }
    fn idle_with_diagnostic(task: &'static str, state: &'static str, diagnostic: &str) -> Self {
        Self::reported(task, state, Some(diagnostic.to_owned()))
    }
}

#[derive(Serialize)]
struct Tick {
    graphs: Graphs,
    tasks: Vec<Task>,
}

fn gc_due(cadence: &Cadence, now: WallTime) -> bool {
    cadence.next_gc_at.is_none_or(|deadline| now >= deadline)
}

fn read_cadence(directory: &Directory) -> Result<Cadence, Box<dyn std::error::Error>> {
    match directory.read_bounded("cadence.json".as_ref(), 64 * 1024) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.is_not_found() => Ok(Cadence::default()),
        Err(error) => Err(error.into()),
    }
}

/// Recovery is independent of graph journals: even an orphaned attempt must
/// eventually settle. Share one host census across all registered projects.
fn reap_projects(resolved: &crate::roots::Resolved) -> Result<&'static str, String> {
    use provenant_supervisor::census::{MAX_CENSUS_ENTRIES, SystemProcessObserver};
    let census = SystemProcessObserver.census(MAX_CENSUS_ENTRIES).ok();
    let clock = SystemClock;
    let cancel = provenant_core::CancelToken::new();
    let call = provenant_run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let mut errors = Vec::new();
    for binding in
        provenant_config::registered_projects(&resolved.roots).map_err(|e| e.to_string())?
    {
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            let context = provenant_cli::fabric::Context::open_for_project(
                resolved.roots.clone(),
                &resolved.home,
                &binding,
            )?;
            let execution = provenant_run::ExecutionContext {
                state_root: resolved.roots.state.clone(),
                project_id: binding.project_id(),
                actor_id: context.identity.inbox.actor,
                read_prompts: false,
            };
            if let Some(store) = provenant_run::RunStore::open_disk_read_only(&execution, &call)? {
                let store = store.with_writer_registry(resolved.roots.state.as_path());
                reap_before_index(
                    || -> Result<(), Box<dyn std::error::Error>> {
                        match &census {
                            Some(census) => {
                                store.reap_with_census(&call, census)?;
                            }
                            None => {
                                store.reap_without_census(&call)?;
                            }
                        }
                        Ok(())
                    },
                    || {
                        provenant_run::RunStore::open(&execution, &call)?
                            .requeue_unindexed_observations(&call)?;
                        crate::event_delivery::ingest_lifecycle(
                            &resolved.roots,
                            binding.project_id(),
                            context.identity.host,
                            &store,
                        )?;
                        Ok(())
                    },
                )?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            errors.push(format!("{}: {error}", binding.project_id()));
        }
    }
    if errors.is_empty() {
        Ok("queued")
    } else {
        Err(errors.join("; "))
    }
}

// Indexing is retryable and must never prevent orphan recovery.
fn reap_before_index<E>(
    reap: impl FnOnce() -> Result<(), E>,
    index: impl FnOnce() -> Result<(), E>,
) -> Result<(), E> {
    reap()?;
    index()
}

fn tick(
    globals: &Globals,
    workers: &'static dyn WorkerObserver,
) -> Result<Tick, Box<dyn std::error::Error>> {
    // Scheduled host work must never inherit a lane's authority.
    if provenant_cli::fabric::in_lane() {
        return Err("tick requires host custody".into());
    }
    let resolved = crate::roots::resolve(
        globals,
        crate::roots::Workspace::Home,
        crate::roots::Product::Required,
    )?;
    let config = provenant_cli::config_reads::read_for(&resolved.roots, "tick")?;
    let clock = SystemClock;
    let now = clock.wall_now()?;
    let settlement = Task::run("settlement", || reap_projects(&resolved));
    let graphs =
        crate::fabric::tick_graphs(&resolved, now, workers).unwrap_or_else(|error| Graphs {
            advanced: 0,
            errors: vec![error.to_string()],
            warnings: Vec::new(),
        });
    // Task errors are isolated: a failed graph or collection must not prevent
    // later hooks from running. GC cadence is serialised across manual ticks
    // and launchd invocations, separately from each graph's journal custody.
    let gc = if !config.gc.schedule {
        Task::idle("gc", "disabled")
    } else {
        Task::gc(|| -> Result<GcReport, String> {
            let directory = provenant_config::private_directory(
                &resolved.roots.state.as_path().join("tick"),
                true,
            )
            .map_err(|error| error.to_string())?;
            let _lock = match directory.try_lock("cadence.lock".as_ref(), LockMode::Exclusive) {
                Ok(lock) => lock,
                Err(provenant_os::fs::FsError::LockTimeout { .. }) => {
                    return Ok(GcReport::state("busy; next tick retries"));
                }
                Err(error) => return Err(error.to_string()),
            };
            queue_gc(&directory, now, || {
                let executable = std::env::current_exe().map_err(|error| error.to_string())?;
                let argv = [
                    executable.into_os_string(),
                    "--state-dir".into(),
                    resolved.roots.state.as_path().as_os_str().to_owned(),
                    "internal".into(),
                    "tick-gc".into(),
                ];
                provenant_os::exec::spawn_detached(&argv, &resolved.home)
                    .map_err(|error| error.to_string())
            })
        })
    };
    // Seed retries are independently admitted from durable queue state; the
    // remaining notice and quota tasks stay explicit until their owners wire them.
    let target_seed = match provenant_config::private_directory(
        &resolved.roots.state.as_path().join("tick"),
        true,
    ) {
        Ok(directory) => {
            // Busy retries and abandoned detached jobs are re-admitted here.
            // The queue protocol retains their scope and coalesces a full pass.
            if seed_retry_due(&directory, now) {
                match queue_seed(&directory, now, None, || {
                    spawn_seed_worker(resolved.roots.state.as_path(), &resolved.home)
                }) {
                    Err(error) => Task::idle_with_diagnostic(
                        "target_seed",
                        "last_failed",
                        safe_seed_failure_diagnostic(Some(&error)),
                    ),
                    Ok(()) => target_seed_task(&directory),
                }
            } else {
                target_seed_task(&directory)
            }
        }
        Err(_) => Task::idle_with_diagnostic(
            "target_seed",
            "last_failed",
            "The local tick state directory is unavailable. Check host state permissions, then rerun `provenant tick`.",
        ),
    };
    let tasks = vec![
        gc,
        target_seed,
        steward::run(&config.domains.steward, &resolved, workers),
        settlement,
        Task::run("forge_events", || {
            crate::event_delivery::tick(&resolved)
                .map(|()| "observed")
                .map_err(|error| error.to_string())
        }),
        Task::idle("notice_batch_flush", "unwired"),
        Task::idle("quota_refresh", "unwired"),
    ];
    Ok(Tick { graphs, tasks })
}

pub(crate) fn run(globals: &Globals, workers: &'static dyn WorkerObserver) -> u8 {
    match tick(globals, workers) {
        Ok(data) => {
            let failed = !data.graphs.errors.is_empty()
                || data.tasks.iter().any(|task| task.error.is_some());
            let mut human = format!(
                "tick: graphs={} errors={} gc={} seed={} steward={} settlement={}\n",
                data.graphs.advanced,
                data.graphs.errors.len(),
                data.tasks[0].state,
                data.tasks[1].state,
                data.tasks[2].state,
                data.tasks[3].state
            );
            for warning in &data.graphs.warnings {
                human.push_str(&format!("warning: {warning}\n"));
            }
            let rows = data
                .graphs
                .errors
                .iter()
                .cloned()
                .chain(
                    data.graphs
                        .warnings
                        .iter()
                        .map(|warning| format!("warning: {warning}")),
                )
                .chain(data.tasks.iter().filter_map(|task| {
                    task.error
                        .as_ref()
                        .map(|error| format!("{}: {error}", task.task))
                }))
                .chain(data.tasks.iter().filter_map(|task| {
                    task.diagnostic
                        .as_ref()
                        .map(|diagnostic| format!("note: {}: {diagnostic}", task.task))
                }))
                .collect();
            let summary = provenant_cli::Summary::new(
                human.trim(),
                rows,
                failed.then(|| "provenant tick --json".into()),
            );
            crate::emit_summarised(
                &CommandResult::new(
                    "tick",
                    if failed { Outcome::Error } else { Outcome::Ok },
                    data,
                ),
                globals,
                &human,
                false,
                Some(&summary),
            )
        }
        Err(error) => crate::fail("tick", Outcome::Error, "tick", &error.to_string(), globals),
    }
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)] // The worktree driver includes this module without libtest.
    use super::*;

    #[test]
    fn reaping_proceeds_when_indexing_fails() {
        let reaped = std::cell::Cell::new(false);
        let result = reap_before_index(
            || {
                reaped.set(true);
                Ok(())
            },
            || Err("index unavailable"),
        );
        assert!(result.is_err());
        assert!(reaped.get());
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    /// Spec: docs/specs/run-git.md#project-layout-ownership
    #[test]
    fn registered_seed_projects_resolve_separate_gitdirs_and_skip_bare_repositories() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let home = root.join("home");
        let source = root.join("source");
        let state = root.join("state");
        for path in [&home, &source, &state] {
            std::fs::create_dir(path).unwrap();
        }
        std::fs::set_permissions(
            &state,
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        )
        .unwrap();
        let mut inputs = provenant_core::RootInputs::new(home, source);
        inputs.state_dir = Some(state.clone());
        let roots = inputs
            .resolve(&provenant_os::fs::SystemPathResolver)
            .unwrap();
        let primary = root.join("primary");
        std::fs::create_dir(&primary).unwrap();
        provenant_os::fixture::run_git(&primary, &["init", "-q"]).unwrap();
        provenant_os::fixture::run_git(&primary, &["commit", "--allow-empty", "-qm", "base"])
            .unwrap();

        let separate = root.join("separate");
        let separate_git = root.join("separate.git");
        std::fs::create_dir(&separate).unwrap();
        provenant_os::fixture::run_git(
            &separate,
            &[
                "init",
                "-q",
                "--separate-git-dir",
                separate_git.to_str().unwrap(),
            ],
        )
        .unwrap();
        provenant_os::fixture::run_git(&separate, &["commit", "--allow-empty", "-qm", "base"])
            .unwrap();

        let bare = root.join("host-only.git");
        provenant_os::fixture::run_git(&root, &["init", "--bare", "-q", bare.to_str().unwrap()])
            .unwrap();
        let invalid = root.join("invalid-git-dir");
        std::fs::create_dir(&invalid).unwrap();

        let git = provenant_os::git::git_executable().unwrap();
        let executor = SystemExecutor::new(4).unwrap();
        let utility = provenant_confinement::UtilityRuntime::read_only(&git, &state).unwrap();
        let budget = provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5)).unwrap();
        let cancel = provenant_core::CancelToken::new();
        let primary_binding = provenant_config::bind_project(
            &roots,
            utility
                .project_location(&executor, &primary, budget, &cancel)
                .unwrap()
                .location,
        )
        .unwrap();
        let separate_binding = provenant_config::bind_project(
            &roots,
            utility
                .project_location(&executor, &separate, budget, &cancel)
                .unwrap()
                .location,
        )
        .unwrap();
        let invalid_binding = provenant_config::bind_project(
            &roots,
            provenant_config::ProjectLocation::new(&invalid).unwrap(),
        )
        .unwrap();
        let bare_binding = provenant_config::bind_project(
            &roots,
            provenant_config::ProjectLocation::new(&bare).unwrap(),
        )
        .unwrap();
        let primary_common = primary.join(".git");
        let stale_primary_hint = provenant_config::bind_project(
            &roots,
            provenant_config::ProjectLocation::from_verified_primary_checkout(
                &primary_common,
                &invalid,
            )
            .unwrap(),
        )
        .unwrap();
        let primary_layout = provenant_confinement::worktrees::project_layout_for_binding(
            &executor,
            &stale_primary_hint,
            &git,
            &provenant_confinement::worktrees::project_layout_git_environment(),
            Duration::from_secs(5),
            &provenant_core::CancelToken::new(),
        )
        .unwrap();
        assert_eq!(primary_layout.project(), Some(primary.as_path()));
        let discovery = registered_seed_projects(
            &[
                primary_binding.clone(),
                invalid_binding.clone(),
                separate_binding.clone(),
                bare_binding,
            ],
            &[],
            &executor,
            &utility,
            &provenant_core::CancelToken::new(),
        );
        assert_eq!(
            discovery
                .projects
                .iter()
                .map(|project| project.root.as_path())
                .collect::<Vec<_>>(),
            vec![primary.as_path(), separate.as_path()]
        );
        assert_eq!(discovery.errors.len(), 1);
        assert!(
            discovery.errors[0]
                .message
                .contains(&invalid.display().to_string())
        );

        let primary_key = provenant_run::target_seed::project_key(&primary_common);
        let retry = registered_seed_projects(
            &[primary_binding, invalid_binding, separate_binding],
            std::slice::from_ref(&primary_key),
            &executor,
            &utility,
            &provenant_core::CancelToken::new(),
        );
        assert_eq!(retry.projects.len(), 1);
        assert_eq!(retry.projects[0].project_key, primary_key);
        assert!(
            retry.errors.is_empty(),
            "unrelated failures are not retried"
        );
    }

    #[allow(dead_code)] // The worktree driver includes this module without libtest.
    fn after(time: WallTime, seconds: u64) -> WallTime {
        time.checked_add(DurationMicros::from_seconds(seconds).unwrap())
            .unwrap()
    }

    #[allow(dead_code)]
    fn gc_directory(temp: &tempfile::TempDir) -> Directory {
        provenant_config::private_directory(temp.path(), true).unwrap()
    }

    /// Spec: docs/specs/state.md#retention-keeps-evidence
    #[test]
    fn the_daily_gc_deadline_advances_only_when_a_collection_completes() {
        let temp = provenant_os::fixture::tempdir(|root| tempfile::tempdir_in(root)).unwrap();
        let directory = gc_directory(&temp);
        let now = WallTime::from_micros(1_000_000).unwrap();
        assert!(gc_due(&read_cadence(&directory).unwrap(), now));
        assert_eq!(
            queue_gc(&directory, now, || Ok(())).unwrap(),
            GcReport::state("queued")
        );
        assert!(
            gc_due(&read_cadence(&directory).unwrap(), now),
            "launching a worker is not completing a collection"
        );

        write_gc_result(&directory, now, "running", None, 0).unwrap();
        let worker = directory
            .try_lock("gc-worker.lock".as_ref(), LockMode::Exclusive)
            .unwrap();
        assert_eq!(
            queue_gc(&directory, now, || Err("duplicate launch".into()))
                .unwrap()
                .state,
            "running",
            "a running collection is never launched twice"
        );
        drop(worker);

        write_gc_result(&directory, now, "complete", None, 0).unwrap();
        advance_gc_cadence(&directory, now).unwrap();
        let deadline = after(now, DAY_SECONDS);
        let reopened = Directory::open_anchor(temp.path()).unwrap();
        let restored = read_cadence(&reopened).unwrap();
        assert!(!gc_due(&restored, now), "the deadline survives a restart");
        assert!(gc_due(&restored, deadline));
        assert_eq!(
            queue_gc(&reopened, now, || Err("not due".into()))
                .unwrap()
                .state,
            "not_due"
        );
        assert_eq!(
            queue_gc(&reopened, deadline, || Ok(())).unwrap().state,
            "queued"
        );
    }

    /// Spec: docs/specs/state.md#retention-keeps-evidence
    #[test]
    fn an_interrupted_gc_retries_with_a_bounded_backoff_instead_of_waiting_a_day() {
        let temp = provenant_os::fixture::tempdir(|root| tempfile::tempdir_in(root)).unwrap();
        let directory = gc_directory(&temp);
        let launched = WallTime::from_micros(10_000_000_000).unwrap();
        queue_gc(&directory, launched, || Ok(())).unwrap();

        // A worker that has not yet taken its lock gets a grace period.
        assert_eq!(
            queue_gc(&directory, after(launched, 30), || Err("early".into()))
                .unwrap()
                .state,
            "queued"
        );

        // No worker holds the lock: the result is a lie and the first retry is
        // fifteen minutes away. Only this finding is an error.
        let found = after(launched, 61);
        let error = queue_gc(&directory, found, || Err("not yet".into())).unwrap_err();
        assert!(error.contains("interrupted"), "{error}");
        let saved = read_gc_result(&directory).unwrap().unwrap();
        assert_eq!((saved.state.as_str(), saved.failures), ("failed", 1));
        let cadence = read_cadence(&directory).unwrap();
        assert!(gc_due(&cadence, found), "an interruption keeps the day due");

        let early = after(found, 14 * 60);
        let report = queue_gc(&directory, early, || Err("too early".into())).unwrap();
        assert_eq!(report.state, "retry_pending");
        let note = report.note.unwrap();
        assert!(note.contains("14m ago"), "{note}");
        assert!(note.contains("retries in 1m"), "{note}");

        let retry = after(found, 15 * 60);
        let mut launches = 0;
        assert_eq!(
            queue_gc(&directory, retry, || {
                launches += 1;
                Ok(())
            })
            .unwrap()
            .state,
            "queued"
        );
        assert_eq!(launches, 1);
        assert_eq!(
            read_gc_result(&directory).unwrap().unwrap().failures,
            1,
            "a retry carries its failures so the next one backs off"
        );

        // Interrupted again: the second delay is an hour.
        let second = after(retry, 61);
        assert!(
            queue_gc(&directory, second, || Err("not yet".into()))
                .unwrap_err()
                .contains("interrupted")
        );
        assert_eq!(read_gc_result(&directory).unwrap().unwrap().failures, 2);
        assert_eq!(
            queue_gc(&directory, after(second, 3_599), || Err("early".into()))
                .unwrap()
                .state,
            "retry_pending"
        );
        assert_eq!(
            queue_gc(&directory, after(second, 3_600), || Ok(()))
                .unwrap()
                .state,
            "queued"
        );
    }

    /// Spec: docs/specs/state.md#retention-keeps-evidence
    #[test]
    fn gc_retry_delays_grow_but_never_pass_the_daily_cadence() {
        let seconds: Vec<u64> = [0, 1, 2, 3, 4, 5, 6, u32::MAX]
            .into_iter()
            .map(gc_retry_delay_seconds)
            .collect();
        assert_eq!(
            seconds,
            [900, 900, 3_600, 14_400, 57_600, 86_400, 86_400, 86_400]
        );
    }

    /// Spec: docs/specs/state.md#retention-keeps-evidence
    #[test]
    fn a_failed_spawn_or_collection_is_retried_and_a_stale_failure_is_not_a_fresh_one() {
        let temp = provenant_os::fixture::tempdir(|root| tempfile::tempdir_in(root)).unwrap();
        let directory = gc_directory(&temp);
        let now = WallTime::from_micros(10_000_000_000).unwrap();
        assert_eq!(
            queue_gc(&directory, now, || Err("spawn failed".into())).unwrap_err(),
            "spawn failed"
        );
        assert!(
            gc_due(&read_cadence(&directory).unwrap(), now),
            "spawn failure must remain retryable"
        );

        // The next tick reports the old failure with its age and next try; it
        // is not an error, so it does not fail every tick.
        let later = after(now, 600);
        let task = Task::gc(|| queue_gc(&directory, later, || Err("too early".into())));
        assert_eq!(task.state, "retry_pending");
        assert!(task.error.is_none(), "{:?}", task.error);
        let note = task.diagnostic.unwrap();
        assert!(note.contains("10m ago"), "{note}");
        assert!(note.contains("spawn failed"), "{note}");
        assert!(note.contains("retries in 5m"), "{note}");

        // A worker that fails its collection backs off the same way.
        let failed = after(now, 3_000);
        write_gc_result(&directory, failed, "failed", Some("busy graph".into()), 3).unwrap();
        let report = queue_gc(&directory, after(failed, 4 * 3_600 - 1), || Ok(())).unwrap();
        assert_eq!(report.state, "retry_pending");
        assert_eq!(
            queue_gc(&directory, after(failed, 4 * 3_600), || Ok(()))
                .unwrap()
                .state,
            "queued"
        );
    }

    /// Spec: docs/specs/state.md (every new persisted field is optional or defaulted)
    #[test]
    fn gc_results_keep_their_shape_until_a_failure_is_counted() {
        let temp = provenant_os::fixture::tempdir(|root| tempfile::tempdir_in(root)).unwrap();
        let directory = gc_directory(&temp);
        let now = WallTime::from_micros(1_000_000).unwrap();
        write_gc_result(&directory, now, "complete", None, 0).unwrap();
        let bytes = directory.read_bounded(GC_RESULT.as_ref(), 4096).unwrap();
        assert!(!String::from_utf8(bytes).unwrap().contains("failures"));
        write_gc_result(&directory, now, "failed", Some("x".into()), 2).unwrap();
        let bytes = directory.read_bounded(GC_RESULT.as_ref(), 4096).unwrap();
        assert!(String::from_utf8(bytes).unwrap().contains("\"failures\":2"));

        let old: TaskResult = serde_json::from_slice(
            br#"{"state":"failed","at":"2026-11-18T10:50:19.012844Z","error":"scheduled GC interrupted"}"#,
        )
        .unwrap();
        assert_eq!(old.failures, 0, "an older result counts no failures");
    }

    #[allow(dead_code)] // The worktree driver includes this module without libtest.
    fn seed_clock() -> provenant_os::clock::SystemClock {
        provenant_os::clock::SystemClock
    }

    #[allow(dead_code)]
    fn seed_state(directory: &Directory) -> String {
        target_seed_task(directory).state
    }

    #[allow(dead_code)]
    fn seed_state_of(directory: &Directory) -> (String, Option<String>) {
        let result = read_seed_result(directory).unwrap().unwrap();
        (seed_state(directory), result.error)
    }

    #[allow(dead_code)]
    fn seed_records(directory: &Directory) -> Vec<SeedProjectRecord> {
        read_seed_result(directory).unwrap().unwrap().projects
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn seed_failure_report_projects_a_safe_cause_and_action() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let directory = provenant_config::private_directory(&root, true).unwrap();
        let private = "/private/sensitive/repo: command failed: [\"cargo\", \"--token\", \"do-not-print\"] API_KEY=secret-value";
        write_seed_result(
            &directory,
            &SeedResult::new(
                WallTime::from_micros(1_000_000).unwrap(),
                "failed",
                None,
                vec![SeedProjectRecord {
                    project: "/private/sensitive/repo".into(),
                    project_key: "key".into(),
                    outcome: SeedProjectOutcome::Failed {
                        error: private.into(),
                    },
                }],
            ),
        )
        .unwrap();

        let task = target_seed_task(&directory);
        assert_eq!(task.state, "failed:1");
        assert!(task.error.is_none(), "a seed note must not fail the tick");
        let diagnostic = task.diagnostic.clone().unwrap();
        assert!(diagnostic.contains("Cargo or Git command failed"));
        assert!(diagnostic.contains("provenant worktree seed-build"));

        let rendered = serde_json::to_string(&task).unwrap();
        for private_detail in [
            "/private/sensitive/repo",
            "--token",
            "do-not-print",
            "API_KEY",
            "secret-value",
        ] {
            assert!(!rendered.contains(private_detail));
        }
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn seed_refresh_is_queued_without_running_inline_and_a_spawn_failure_is_recorded() {
        let temp = tempfile::tempdir().unwrap();
        // macOS temp roots sit behind a /var symlink, which private_directory refuses.
        let root = temp.path().canonicalize().unwrap();
        let directory = provenant_config::private_directory(&root, true).unwrap();
        let now = WallTime::from_micros(1_000_000).unwrap();
        assert_eq!(seed_state(&directory), "idle");
        let mut spawned = 0;
        queue_seed(&directory, now, Some(SeedRequest::full()), || {
            spawned += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(spawned, 1);
        assert_eq!(seed_state_of(&directory).0, "queued");
        assert!(matches!(
            read_seed_queue(&directory).unwrap().pending.unwrap().scope,
            SeedScope::Full
        ));
        // Queueing never builds: the refresh closure is only reachable from
        // the detached worker.
        let stale = WallTime::from_micros(now.as_micros() + SEED_WORK_RETRY_MICROS + 1).unwrap();
        let error =
            queue_seed(&directory, stale, None, || Err("no process budget".into())).unwrap_err();
        assert_eq!(error, "no process budget");
        assert_eq!(
            seed_state_of(&directory),
            (
                "last_failed".to_owned(),
                Some("no process budget".to_owned())
            )
        );
        assert!(seed_retry_due(
            &directory,
            WallTime::from_micros(stale.as_micros() + SEED_WORK_RETRY_MICROS).unwrap()
        ));
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn a_due_busy_snapshot_cannot_narrow_a_queued_full_pass() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let directory = provenant_config::private_directory(&root, true).unwrap();
        let now = WallTime::from_micros(1_000_000).unwrap();
        let queue = SeedQueue {
            retry: Some(SeedRequest::busy(vec!["project-a".into()])),
            retry_at: Some(
                WallTime::from_micros(now.as_micros() - SEED_WORK_RETRY_MICROS).unwrap(),
            ),
            ..Default::default()
        };
        write_seed_queue(&directory, &queue).unwrap();

        // The daily full request arrives after a tick has observed the old
        // busy deadline, but before that tick's worker is admitted.
        queue_seed(&directory, now, Some(SeedRequest::full()), || Ok(())).unwrap();
        let running = directory
            .try_lock("seed-worker.lock".as_ref(), LockMode::Exclusive)
            .unwrap();
        queue_seed(&directory, now, None, || {
            panic!("a stale due callback must not launch around an active worker")
        })
        .unwrap();
        drop(running);

        let queue = read_seed_queue(&directory).unwrap();
        assert!(queue.retry.is_none());
        assert_eq!(
            queue.pending,
            Some(SeedRequest::full()),
            "full scope must dominate the stale busy snapshot"
        );
        let (_, request) = claim_seed_job(&directory, now).unwrap().unwrap();
        assert_eq!(request, SeedRequest::full());
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn a_full_request_queued_during_busy_work_supersedes_the_busy_retry() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let directory = provenant_config::private_directory(&root, true).unwrap();
        let now = WallTime::from_micros(1_500_000).unwrap();
        let queue = SeedQueue {
            active: Some(SeedRequest::busy(vec!["project-a".into()])),
            active_at: Some(now),
            ..Default::default()
        };
        write_seed_queue(&directory, &queue).unwrap();
        let mut worker = Some(
            directory
                .try_lock("seed-worker.lock".as_ref(), LockMode::Exclusive)
                .unwrap(),
        );

        queue_seed(&directory, now, Some(SeedRequest::full()), || {
            panic!("the active worker will drain the full request")
        })
        .unwrap();
        let next = finish_seed_job(
            &directory,
            now,
            &mut worker,
            SeedOutcome {
                busy_project_keys: vec!["project-a".into()],
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(next, SeedRequest::full());
        let queue = read_seed_queue(&directory).unwrap();
        assert_eq!(queue.active, Some(SeedRequest::full()));
        assert!(queue.retry.is_none());
        assert!(queue.retry_at.is_none());
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn queued_and_active_seed_jobs_recover_after_detached_worker_crash() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let directory = provenant_config::private_directory(&root, true).unwrap();
        let queued_at = WallTime::from_micros(1_000_000).unwrap();
        let stale =
            WallTime::from_micros(queued_at.as_micros() + SEED_WORK_RETRY_MICROS + 1).unwrap();
        queue_seed(&directory, queued_at, Some(SeedRequest::full()), || Ok(())).unwrap();
        assert!(seed_retry_due(&directory, stale));
        let mut respawns = 0;
        queue_seed(&directory, stale, None, || {
            respawns += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(respawns, 1, "an unclaimed queued job is respawned");
        let (worker, request) = claim_seed_job(&directory, stale).unwrap().unwrap();
        assert_eq!(request, SeedRequest::full());
        drop(worker); // Simulate a crash after claiming the job.

        let active_stale =
            WallTime::from_micros(stale.as_micros() + SEED_WORK_RETRY_MICROS + 1).unwrap();
        assert!(seed_retry_due(&directory, active_stale));
        queue_seed(&directory, active_stale, None, || {
            respawns += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(respawns, 2, "an abandoned active job is replayed");
        let queue = read_seed_queue(&directory).unwrap();
        assert_eq!(queue.active, None);
        assert_eq!(queue.pending, Some(SeedRequest::full()));
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn stale_busy_job_recovery_keeps_its_project_keys() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let directory = provenant_config::private_directory(&root, true).unwrap();
        let started = WallTime::from_micros(2_000_000).unwrap();
        queue_seed(
            &directory,
            started,
            Some(SeedRequest::busy(vec!["project-a".into()])),
            || Ok(()),
        )
        .unwrap();
        let (worker, request) = claim_seed_job(&directory, started).unwrap().unwrap();
        assert_eq!(request, SeedRequest::busy(vec!["project-a".into()]));
        drop(worker); // Simulate a crash after the request became active.

        let stale =
            WallTime::from_micros(started.as_micros() + SEED_WORK_RETRY_MICROS + 1).unwrap();
        assert!(seed_retry_due(&directory, stale));
        queue_seed(&directory, stale, None, || Ok(())).unwrap();
        let queue = read_seed_queue(&directory).unwrap();
        assert_eq!(queue.active, None);
        assert_eq!(
            queue.pending,
            Some(SeedRequest::busy(vec!["project-a".into()]))
        );
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn producer_racing_worker_exit_is_admitted_after_the_final_check() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let directory = provenant_config::private_directory(&root, true).unwrap();
        let now = WallTime::from_micros(3_000_000).unwrap();
        let queue = SeedQueue {
            active: Some(SeedRequest::full()),
            active_at: Some(now),
            ..Default::default()
        };
        write_seed_queue(&directory, &queue).unwrap();
        write_seed_progress(&directory, now, "running", None).unwrap();
        let worker = directory
            .try_lock("seed-worker.lock".as_ref(), LockMode::Exclusive)
            .unwrap();
        let queue_lock = seed_queue_lock(&directory).unwrap();
        let spawned = AtomicBool::new(false);

        let (started_tx, started_rx) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let producer = scope.spawn(|| {
                started_tx.send(()).unwrap();
                queue_seed(&directory, now, Some(SeedRequest::full()), || {
                    spawned.store(true, Ordering::SeqCst);
                    Ok(())
                })
            });
            started_rx.recv().unwrap();
            let mut worker = Some(worker);
            assert!(
                finish_seed_job_with_lock(
                    &directory,
                    queue_lock,
                    now,
                    &mut worker,
                    SeedOutcome::default(),
                )
                .unwrap()
                .is_none()
            );
            producer.join().unwrap().unwrap();
        });

        assert!(spawned.load(Ordering::SeqCst));
        assert_eq!(
            read_seed_queue(&directory).unwrap().pending,
            Some(SeedRequest::full())
        );
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn a_busy_retry_replaces_only_its_projects_and_keeps_every_other_outcome() {
        use crate::target_seed_build::Refresh;
        let temp = tempfile::tempdir().unwrap();
        // macOS temp roots sit behind a /var symlink, which private_directory refuses.
        let root = temp.path().canonicalize().unwrap();
        let directory = provenant_config::private_directory(&root, true).unwrap();
        let project = |name: &str| seed_project(&root, name, true);
        let projects = vec![project("a"), project("b"), project("c"), project("d")];
        let entries = SeedDiscovery::from_paths(projects.clone()).projects;
        let busy_key = entries[0].project_key.clone();
        let wall = seed_clock().wall_now().unwrap();
        queue_seed(&directory, wall, Some(SeedRequest::full()), || Ok(())).unwrap();
        let (worker, request) = claim_seed_job(&directory, wall).unwrap().unwrap();
        let mut worker = Some(worker);
        let outcome = seed_pass_locked(
            &directory,
            &root,
            &seed_clock(),
            true,
            &request,
            || Ok(SeedDiscovery::from_paths(projects.clone())),
            |project| match project.file_name().unwrap().to_str().unwrap() {
                "a" => Ok(Refresh::Busy),
                "b" => Err("origin/main missing".into()),
                "c" => Ok(Refresh::Built("generation".into())),
                _ => Ok(Refresh::Current),
            },
        )
        .unwrap();
        assert_eq!(outcome.busy_project_keys, vec![busy_key.clone()]);
        assert!(
            finish_seed_job(&directory, wall, &mut worker, outcome)
                .unwrap()
                .is_none()
        );
        // One failed project does not hide the two that were seeded or the one
        // that is waiting for its build lock; the tick is not itself failed.
        assert_eq!(seed_state(&directory), "ok:2 busy:1 failed:1");
        let outcomes: Vec<_> = seed_records(&directory)
            .into_iter()
            .map(|record| record.outcome)
            .collect();
        assert_eq!(
            outcomes,
            vec![
                SeedProjectOutcome::Busy,
                SeedProjectOutcome::Failed {
                    error: "origin/main missing".into()
                },
                SeedProjectOutcome::Built,
                SeedProjectOutcome::Current,
            ]
        );
        assert!(target_seed_task(&directory).error.is_none());

        assert!(!seed_retry_due(&directory, wall), "just written: not due");
        let far = WallTime::from_micros(wall.as_micros() + SEED_WORK_RETRY_MICROS + 1).unwrap();
        assert!(seed_retry_due(&directory, far), "busy is retried next tick");
        let retry = read_seed_queue(&directory).unwrap().retry.unwrap();
        assert_eq!(retry.scope, SeedScope::Busy(vec![busy_key.clone()]));
        queue_seed(&directory, far, None, || Ok(())).unwrap();
        let queued = read_seed_queue(&directory).unwrap();
        assert_eq!(
            queued.pending,
            Some(SeedRequest::busy(vec![busy_key.clone()]))
        );
        assert!(queued.retry.is_none());
        let (worker_lock, request) = claim_seed_job(&directory, far).unwrap().unwrap();
        let mut worker_lock = Some(worker_lock);
        let retried = std::cell::RefCell::new(Vec::new());
        let outcome = seed_pass_locked(
            &directory,
            &root,
            &seed_clock(),
            true,
            &request,
            || Ok(SeedDiscovery::from_paths(projects.clone())),
            |project| {
                retried.borrow_mut().push(project.to_owned());
                assert_eq!(project.file_name().unwrap(), "a");
                Ok(Refresh::Current)
            },
        )
        .unwrap();
        assert!(
            finish_seed_job(&directory, far, &mut worker_lock, outcome)
                .unwrap()
                .is_none()
        );
        assert_eq!(*retried.borrow(), vec![projects[0].clone()]);
        assert_eq!(
            seed_state(&directory),
            "ok:3 failed:1",
            "the retry kept the failed project and the seeded ones"
        );
        assert!(
            !seed_retry_due(&directory, far),
            "resolved busy work is not retried"
        );

        seed_pass(
            &directory,
            &root,
            &seed_clock(),
            true,
            &SeedRequest::full(),
            || Ok(SeedDiscovery::from_paths(projects)),
            |_| Ok(Refresh::Current),
        )
        .unwrap();
        assert_eq!(seed_state_of(&directory), ("ok:4".to_owned(), None));
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn seed_pass_skips_non_cargo_projects_and_allows_later_retry_after_failure() {
        use crate::target_seed_build::Refresh;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let directory = provenant_config::private_directory(&root, true).unwrap();
        let bare = seed_project(&root, "bare", false);
        let broken = seed_project(&root, "broken", true);
        let listed = vec![bare.clone(), broken.clone()];
        let called = std::cell::RefCell::new(Vec::new());
        seed_pass(
            &directory,
            &root,
            &seed_clock(),
            true,
            &SeedRequest::full(),
            || Ok(SeedDiscovery::from_paths(listed)),
            |project| {
                called.borrow_mut().push(project.to_owned());
                Err(std::io::Error::other("temporary Git spawn failure").into())
            },
        )
        .unwrap();
        assert_eq!(*called.borrow(), vec![broken.clone()], "bare is skipped");
        let records = seed_records(&directory);
        assert_eq!(records[0].outcome, SeedProjectOutcome::NotCargo);
        assert!(matches!(
            &records[1].outcome,
            SeedProjectOutcome::Failed { error } if error.contains("temporary Git spawn failure")
        ));
        assert_eq!(
            seed_state(&directory),
            "failed:1",
            "a project with nothing to seed is not counted"
        );
        let outcomes = refresh_projects(
            &SeedDiscovery::from_paths(vec![bare]).projects,
            &root,
            |_| panic!("no manifest, no refresh"),
        );
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].outcome, SeedProjectOutcome::NotCargo);
        seed_pass(
            &directory,
            &root,
            &seed_clock(),
            true,
            &SeedRequest::full(),
            || Ok(SeedDiscovery::from_paths(vec![broken])),
            |_| Ok(Refresh::Built("generation".into())),
        )
        .unwrap();
        assert_eq!(seed_state_of(&directory), ("ok:1".to_owned(), None));
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn a_project_whose_verify_base_cannot_be_resolved_is_skipped_without_hiding_another_seed() {
        use crate::target_seed_build::{Refresh, SeedSkip, SkipReason};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let directory = provenant_config::private_directory(&root, true).unwrap();
        let unresolved = seed_project(&root, "unresolved", true);
        let healthy = seed_project(&root, "healthy", true);
        let called = std::cell::RefCell::new(Vec::new());
        seed_pass(
            &directory,
            &root,
            &seed_clock(),
            true,
            &SeedRequest::full(),
            || {
                Ok(SeedDiscovery::from_paths(vec![
                    unresolved.clone(),
                    healthy.clone(),
                ]))
            },
            |project| {
                called.borrow_mut().push(project.to_owned());
                if project == unresolved {
                    Err(SeedSkip::new(
                        SkipReason::VerifyBaseUnresolved,
                        "verify base origin/main does not resolve to a commit in this repository",
                    )
                    .into())
                } else {
                    Ok(Refresh::Built("generation".into()))
                }
            },
        )
        .unwrap();
        assert_eq!(
            *called.borrow(),
            vec![unresolved.clone(), healthy],
            "a skip stops no other project"
        );
        let result = read_seed_result(&directory).unwrap().unwrap();
        assert_eq!(result.state, "complete", "a skip is not a failed pass");
        assert!(result.error.is_none());
        assert!(matches!(
            &result.projects[0].outcome,
            SeedProjectOutcome::Skipped { reason: SkipReason::VerifyBaseUnresolved, repair, .. }
                if repair.contains("seed-build")
        ));
        assert_eq!(result.projects[1].outcome, SeedProjectOutcome::Built);

        let task = target_seed_task(&directory);
        assert_eq!(task.state, "ok:1 skipped:1");
        assert!(task.error.is_none());
        let note = task.diagnostic.unwrap();
        assert!(note.contains("1 project skipped"), "{note}");
        assert!(
            note.contains(SkipReason::VerifyBaseUnresolved.repair()),
            "{note}"
        );
        assert!(
            !note.contains(unresolved.to_str().unwrap()),
            "the note names the repair, not the repository path"
        );
        assert!(!seed_retry_due(
            &directory,
            WallTime::from_micros(i64::MAX / 2).unwrap()
        ));
    }

    /// Spec: docs/specs/state.md (every new persisted field is optional or defaulted)
    #[test]
    fn the_seed_result_records_typed_per_project_outcomes_and_reads_an_older_result() {
        use crate::target_seed_build::SkipReason;
        let record = |key: &str, outcome| SeedProjectRecord {
            project: format!("/repo/{key}"),
            project_key: key.into(),
            outcome,
        };
        let result = SeedResult::finished(
            WallTime::from_micros(1_000_000).unwrap(),
            None,
            vec![
                record("built", SeedProjectOutcome::Built),
                record(
                    "skipped",
                    SeedProjectOutcome::Skipped {
                        reason: SkipReason::VerifyBaseUnset,
                        detail: "verify.base is unset".into(),
                        repair: SkipReason::VerifyBaseUnset.repair().into(),
                    },
                ),
                record(
                    "failed",
                    SeedProjectOutcome::Failed {
                        error: "command failed".into(),
                    },
                ),
            ],
        );
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["state"], "failed");
        assert_eq!(json["projects"][0]["state"], "built");
        assert_eq!(json["projects"][1]["state"], "skipped");
        assert_eq!(json["projects"][1]["reason"], "verify_base_unset");
        assert_eq!(json["projects"][1]["project_key"], "skipped");
        assert_eq!(json["projects"][2]["state"], "failed");
        assert_eq!(seed_summary(&result), "ok:1 skipped:1 failed:1");
        let round_trip: SeedResult = serde_json::from_value(json).unwrap();
        assert_eq!(round_trip.projects, result.projects);

        // A result written before per-project records has none; its state and
        // cause still read, and the tick does not invent counts.
        let older: SeedResult = serde_json::from_slice(
            br#"{"state":"failed","at":"2026-11-17T12:49:25.659684Z","error":"/repo: resolving verify base: worktree or branch was not found"}"#,
        )
        .unwrap();
        assert!(older.projects.is_empty());
        assert_eq!(seed_summary(&older), "last_failed");
        assert!(seed_diagnostic(&older).unwrap().contains("verify base"));
    }

    #[allow(dead_code)] // The worktree driver includes this module without libtest.
    fn seed_project(root: &std::path::Path, name: &str, manifest: bool) -> std::path::PathBuf {
        let project = root.join("projects").join(name);
        let workspace = project.join("arbitrarily/nested/rust");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(project.join(".provenant")).unwrap();
        let mut ports = provenant_config::PortTable::builtin();
        ports.verify.workspace = "arbitrarily/nested/rust".into();
        std::fs::write(
            project.join(provenant_config::PORTS_CONFIG),
            serde_json::to_vec(&ports).unwrap(),
        )
        .unwrap();
        if manifest {
            std::fs::write(workspace.join("Cargo.toml"), "[workspace]\n").unwrap();
        }
        project
    }

    #[test]
    fn seed_pass_is_single_flight_and_respects_a_disabled_seed() {
        use crate::target_seed_build::Refresh;
        let temp = tempfile::tempdir().unwrap();
        // macOS temp roots sit behind a /var symlink, which private_directory refuses.
        let root = temp.path().canonicalize().unwrap();
        let directory = provenant_config::private_directory(&root, true).unwrap();
        let held = directory
            .try_lock("seed-worker.lock".as_ref(), LockMode::Exclusive)
            .unwrap();
        seed_pass(
            &directory,
            &root,
            &seed_clock(),
            true,
            &SeedRequest::full(),
            || panic!("a second worker must not enumerate projects"),
            |_| -> Result<Refresh, Box<dyn std::error::Error>> { panic!("must not refresh") },
        )
        .unwrap();
        assert_eq!(seed_state(&directory), "idle");
        drop(held);
        // A sibling test thread can fork while the lock is released and briefly
        // hold a copy of its descriptor, so retry the acquire for up to 2 s.
        provenant_test_support::wait_for(
            "disabled seed state",
            std::time::Duration::from_secs(2),
            || {
                seed_pass(
                    &directory,
                    &root,
                    &seed_clock(),
                    false,
                    &SeedRequest::full(),
                    || panic!("a disabled seed must not enumerate projects"),
                    |_| -> Result<Refresh, Box<dyn std::error::Error>> {
                        panic!("must not refresh")
                    },
                )
                .unwrap();
                seed_state(&directory)
            },
            |state| *state == "disabled",
        )
        .unwrap();
        assert_eq!(seed_state(&directory), "disabled");
        seed_pass(
            &directory,
            &root,
            &seed_clock(),
            true,
            &SeedRequest::full(),
            || Err("registry unreadable".into()),
            |_| Ok(Refresh::Current),
        )
        .unwrap();
        assert_eq!(
            seed_state_of(&directory),
            (
                "last_failed".to_owned(),
                Some("registry unreadable".to_owned())
            )
        );
    }
}
