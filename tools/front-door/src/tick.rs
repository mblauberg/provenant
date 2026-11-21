//! Scheduled host work. Deadlines live on disk; this process owns no timers.
use crate::worktrees::GcCompletion;
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
    collections::{BTreeMap, BTreeSet},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, UNIX_EPOCH},
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
/// The one repair an operator can act on for a failed prepare command. It never
/// includes the saved command output, which stays in the result file only.
const SEED_PREPARE_REPAIR: &str = "build.prepare failed: fix the command or the repository's generated inputs, then run provenant worktree seed-build in that repository";
/// Bound on an error saved in a task result, so a result never outgrows its read cap.
const MAX_RESULT_ERROR_BYTES: usize = 1024;
/// Bound on a saved cause quoted in a tick note.
const MAX_NOTE_CAUSE_BYTES: usize = 256;
/// A result file is never written within this much of its read cap.
const SEED_RESULT_SLACK_BYTES: u64 = 64 * 1024;
const MAX_GC_RESULT_BYTES: u64 = 64 * 1024;
/// Times a completed collection tries to advance its deadline.
const GC_ADVANCE_ATTEMPTS: u32 = 3;
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
    #[serde(default)]
    retry_attempt: u32,
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
        error: error.map(|error| truncate_bytes(error, MAX_RESULT_ERROR_BYTES)),
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
    match directory.read_bounded(GC_RESULT.as_ref(), MAX_GC_RESULT_BYTES) {
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
    /// When a recorded failure is next retried, while it waits.
    retry_at: Option<WallTime>,
    /// Context for an operator, such as the age and next retry of an old failure.
    note: Option<String>,
}

impl GcReport {
    fn state(state: &'static str) -> Self {
        Self {
            state,
            retry_at: None,
            note: None,
        }
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

/// A result dated after `now` comes from a clock that has since moved back. It
/// cannot be waited out, so it is retried at once; the retry's own result is
/// dated correctly.
fn gc_retry_at(result: &TaskResult, now: WallTime) -> Result<WallTime, String> {
    if result.at > now {
        return Ok(now);
    }
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
        retry_at: Some(retry_at),
        note: Some(format!(
            "the last scheduled GC failed {} ago ({}); attempt {} retries in {}",
            describe_span(now.as_micros().saturating_sub(result.at.as_micros())),
            truncate_bytes(
                result
                    .error
                    .clone()
                    .unwrap_or_else(|| "no saved cause".to_owned()),
                MAX_NOTE_CAUSE_BYTES
            ),
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
    // Only a launch in the last minute is too recent to judge; one dated in the
    // future is a clock that moved back, and no worker is waited on for it.
    if result.state == "queued"
        && (0..SEED_WORK_RETRY_MICROS)
            .contains(&now.as_micros().saturating_sub(result.at.as_micros()))
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
    match read_gc_result(directory) {
        Ok(Some(current)) if !matches!(current.state.as_str(), "running" | "queued") => {
            return Ok(GcUnfinished::Settled(current));
        }
        // Still unfinished, unreadable or gone, with no worker to finish it.
        _ => {}
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
    // A result that cannot be read (corrupt, over its cap, or from a newer
    // writer) must not block collection forever: it is treated as absent and
    // the queued result written below replaces it.
    let (previous, unreadable) = match read_gc_result(directory) {
        Ok(previous) => (previous, None),
        Err(error) => (None, Some(error)),
    };
    if let Some(mut result) = previous {
        if matches!(result.state.as_str(), "running" | "queued") {
            match settle_unfinished_gc(directory, now, &result)? {
                GcUnfinished::Live(state) => return Ok(GcReport::state(state)),
                GcUnfinished::Interrupted(error) => return Err(error),
                GcUnfinished::Settled(settled) => result = settled,
            }
        }
        if result.state == "failed" {
            let retry_at = gc_retry_at(&result, now)?;
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
    Ok(GcReport {
        state: "queued",
        retry_at: None,
        note: unreadable.map(|error| {
            format!(
                "the previous scheduled GC result was unreadable and was replaced ({})",
                truncate_bytes(error, MAX_NOTE_CAUSE_BYTES)
            )
        }),
    })
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

/// What the worker records once the collection has ended, and whether the seed
/// pass should follow. The deadline moves only for a complete or partial
/// collection, and the advance is tried a few times so a busy cadence lock does
/// not turn a finished collection into a failed one; a deadline that still
/// could not be advanced is a note on the completed result. A failure counts
/// toward the retry backoff, and any other outcome resets it. The seed pass
/// follows only a collection that finished its work.
fn settle_gc_run(
    directory: &Directory,
    now: impl Fn() -> Result<WallTime, String>,
    failures: u32,
    collected: Result<GcCompletion, String>,
    mut advance: impl FnMut(WallTime) -> Result<(), String>,
) -> Result<bool, String> {
    match collected {
        Ok(completion) => {
            let (state, cause) = match completion {
                GcCompletion::Complete => ("complete", None),
                GcCompletion::Partial(note) => ("partial", Some(note)),
            };
            let completed_at = now()?;
            let mut advanced = Ok(());
            for _ in 0..GC_ADVANCE_ATTEMPTS {
                advanced = advance(completed_at);
                if advanced.is_ok() {
                    break;
                }
            }
            let note = match advanced {
                Ok(()) => cause,
                Err(error) => Some(format!(
                    "{}the daily deadline was not advanced ({error})",
                    cause.map(|cause| format!("{cause}; ")).unwrap_or_default()
                )),
            };
            write_gc_result(directory, now()?, state, note, 0)?;
            Ok(true)
        }
        Err(error) => {
            write_gc_result(
                directory,
                now()?,
                "failed",
                Some(error),
                failures.saturating_add(1),
            )?;
            Ok(false)
        }
    }
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
        let collected = (|| -> Result<GcCompletion, String> {
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
        })();
        let failed = collected.as_ref().err().cloned();
        let queue_seed_pass = settle_gc_run(
            &directory,
            || clock.wall_now().map_err(|error| error.to_string()),
            failures,
            collected,
            |completed_at| advance_gc_cadence(&directory, completed_at),
        )?;
        // The collection fetched every project's default branch, so any seed
        // that is now behind can be rebuilt. This only queues a detached
        // worker, and only after a collection that finished its work; a
        // failure to queue is recorded for the tick and never fails the
        // collection.
        if queue_seed_pass {
            let _ = queue_seed(
                &directory,
                clock.wall_now()?,
                Some(SeedRequest::full()),
                || spawn_seed_worker(resolved.roots.state.as_path(), &resolved.home),
            );
        }
        // Build-slot GC and prewarm follow a collection that finished its work.
        for warning in schedule_build_hygiene(&resolved) {
            let _ = writeln!(std::io::stderr().lock(), "warning: {warning}");
        }
        failed.map_or(Ok(()), |error| Err(error.into()))
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

/// Delay the first retry by one minute, doubling to a maximum of one hour.
fn retry_delay(attempt: u32) -> i64 {
    SEED_WORK_RETRY_MICROS
        .saturating_mul(1_i64 << attempt.min(6))
        .min(3_600_000_000)
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
            now.as_micros().saturating_sub(at.as_micros())
                >= retry_delay(self.retry_attempt.saturating_sub(1))
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

fn spawn_build_maintain(state: &Path, home: &Path, project: &Path) -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let argv = [
        executable.into_os_string(),
        "--state-dir".into(),
        state.as_os_str().to_owned(),
        "--project".into(),
        project.as_os_str().to_owned(),
        "build".into(),
        "maintain".into(),
    ];
    provenant_os::exec::spawn_detached(&argv, home).map_err(|error| error.to_string())
}

/// Run build-slot GC and queue one detached maintenance job per recently used
/// project, beside the seed pass after a completed collection. Every failure is
/// a warning: host hygiene never fails the tick.
fn schedule_build_hygiene(resolved: &crate::roots::Resolved) -> Vec<String> {
    use provenant_run::build::hygiene;
    let state = resolved.roots.state.as_path();
    let host = match provenant_config::PortTable::read_for(None, state) {
        Ok(table) => table,
        Err(error) => return vec![format!("build hygiene configuration unavailable: {error}")],
    };
    let default = (
        host.build.slot_idle_duration().value,
        host.build.snapshot_retention_duration().value,
    );
    let limit = provenant_config::ResolvedAdmissionClass::resolve("build", state, None, None).slots;
    let mut settings: BTreeMap<String, (Duration, Duration)> = BTreeMap::new();
    let mut projects: Vec<(PathBuf, String)> = Vec::new();
    for binding in provenant_config::registered_gc_projects(&resolved.roots).unwrap_or_default() {
        let Some(path) = binding.primary_checkout_hint().map(Path::to_path_buf) else {
            continue;
        };
        let key = provenant_config::project_key_for(&path)
            .unwrap_or_else(|| provenant_config::project_key(&path));
        let resolved_settings = provenant_config::PortTable::read_for(Some(&path), state)
            .map(|port| {
                (
                    port.build.slot_idle_duration().value,
                    port.build.snapshot_retention_duration().value,
                )
            })
            .unwrap_or(default);
        settings.insert(key.clone(), resolved_settings);
        projects.push((path, key));
    }
    let now_ms = provenant_run::build::ledger::now_unix_ms();
    let now = UNIX_EPOCH + Duration::from_millis(now_ms);
    let report = hygiene::gc_projects(&resolved.home, state, limit, now, |key| {
        settings.get(key).copied().unwrap_or(default)
    });
    let mut warnings = report.warnings;
    if report.rotated_ledgers > 0 {
        let _ = writeln!(
            std::io::stderr().lock(),
            "build ledger rotated; {} rotation(s) expired",
            report.expired_ledgers
        );
    }
    for (path, key) in projects.iter().take(8) {
        if !hygiene::has_recent_job(state, key, now_ms) {
            continue;
        }
        if let Err(error) = spawn_build_maintain(state, &resolved.home, path) {
            warnings.push(format!(
                "cannot schedule build maintenance for {key}: {error}"
            ));
        }
    }
    warnings
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
        queue.retry_at.is_some_and(|at| {
            now.as_micros().saturating_sub(at.as_micros())
                >= retry_delay(queue.retry_attempt.saturating_sub(1))
        }) || [queue.pending_at, queue.launch_at, queue.active_at]
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
    IdentityMismatch,
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
    /// A `build.prepare` command failed. Serialised with the existing `state`
    /// tag as `prepare_failed`; the command and output tail are kept in the
    /// result file but never echoed in an operator diagnostic.
    PrepareFailed {
        command: Vec<String>,
        tail: String,
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

    fn prepare_failed(prepare: &crate::target_seed_build::PrepareFailed) -> Self {
        Self::PrepareFailed {
            command: prepare.argv.clone(),
            tail: truncate_bytes(prepare.tail.clone(), OUTPUT_TAIL_BYTES),
        }
    }

    fn failed(message: String) -> Self {
        Self::Failed {
            error: truncate_seed_error(message),
        }
    }
}

/// Mirrors the builder's 2 KiB bound on a prepare output tail saved in a result.
const OUTPUT_TAIL_BYTES: usize = 2 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SeedProjectRecord {
    project: String,
    project_key: String,
    /// Compiler-selection keys the seed pass removed from its child environment.
    /// Names only; absent for older results and for passes that cleared nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    cleared_environment: Vec<String>,
    #[serde(flatten)]
    outcome: SeedProjectOutcome,
}

impl SeedProjectRecord {
    fn new(
        project: &SeedProject,
        outcome: SeedProjectOutcome,
        cleared_environment: Vec<String>,
    ) -> Self {
        Self {
            project: truncate_bytes(
                project.root.display().to_string(),
                MAX_SEED_PROJECT_PATH_BYTES,
            ),
            project_key: project.project_key.clone(),
            cleared_environment,
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
        // Records are kept in order while they fit the record count and the
        // bytes left under the read cap, so a result is always readable.
        let error = error.map(truncate_seed_error);
        let mut room = MAX_SEED_RESULT_BYTES
            .saturating_sub(SEED_RESULT_SLACK_BYTES)
            .saturating_sub(error.as_ref().map_or(0, |error| error.len() as u64 * 6));
        let keep = projects
            .iter()
            .take(MAX_SEED_PROJECT_RECORDS)
            .take_while(|record| {
                let size =
                    serde_json::to_vec(record).map_or(u64::MAX, |bytes| bytes.len() as u64 + 1);
                let fits = size <= room;
                room = room.saturating_sub(size);
                fits
            })
            .count();
        let omitted_projects = projects.len() - keep;
        projects.truncate(keep);
        Self {
            extra: Default::default(),
            state: state.into(),
            at,
            error,
            projects,
            omitted_projects,
        }
    }

    /// Count records an earlier result had already left out.
    fn with_omitted_before(mut self, before: usize) -> Self {
        self.omitted_projects = self.omitted_projects.saturating_add(before);
        self
    }

    /// A finished pass is failed when anything failed, else busy while any
    /// project's build lock was held, else complete. A skip is none of these.
    fn finished(at: WallTime, error: Option<String>, projects: Vec<SeedProjectRecord>) -> Self {
        let any = |wanted: fn(&SeedProjectOutcome) -> bool| {
            projects.iter().any(|record| wanted(&record.outcome))
        };
        let state = if error.is_some()
            || any(|outcome| {
                matches!(
                    outcome,
                    SeedProjectOutcome::Failed { .. } | SeedProjectOutcome::PrepareFailed { .. }
                )
            }) {
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
    let prior = read_seed_result(directory).ok().flatten();
    let omitted = prior.as_ref().map_or(0, |result| result.omitted_projects);
    let projects = prior.map(|result| result.projects).unwrap_or_default();
    write_seed_result(
        directory,
        &SeedResult::new(at, state, error, projects).with_omitted_before(omitted),
    )
}

#[derive(Debug, Default)]
struct SeedOutcome {
    retry_project_keys: Vec<String>,
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
    fn into_record(self, cleared_environment: &[String]) -> SeedProjectRecord {
        SeedProjectRecord {
            project: truncate_bytes(self.project, MAX_SEED_PROJECT_PATH_BYTES),
            project_key: self.project_key,
            cleared_environment: cleared_environment.to_vec(),
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
    cleared_environment: &[String],
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
                    Ok(Refresh::IdentityMismatch) => SeedProjectOutcome::IdentityMismatch,
                    Err(error) => {
                        if let Some(skip) = crate::target_seed_build::skip_of(error.as_ref()) {
                            SeedProjectOutcome::skipped(skip)
                        } else if let Some(prepare) =
                            crate::target_seed_build::prepare_failed_of(error.as_ref())
                        {
                            SeedProjectOutcome::prepare_failed(prepare)
                        } else {
                            SeedProjectOutcome::failed(error.to_string())
                        }
                    }
                },
            };
            SeedProjectRecord::new(project, outcome, cleared_environment.to_vec())
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
    // The tick worker's own environment is what a seed build clears, so every
    // record this pass produces reports the same set of key names.
    let cleared_environment = provenant_run::target_seed::seed_build_cleared_keys(
        &std::env::vars_os().collect::<Vec<_>>(),
    );
    if !enabled {
        write_seed_result(
            directory,
            &SeedResult::new(at()?, "disabled", None, Vec::new()),
        )?;
        return Ok(SeedOutcome::default());
    }
    let (prior, prior_omitted) = read_seed_result(directory)
        .ok()
        .flatten()
        .map(|result| (result.projects, result.omitted_projects))
        .unwrap_or_default();
    write_seed_progress(directory, at()?, "running", None)?;
    let (records, error, retry_keys) = match projects() {
        Ok(mut discovery) => {
            if !retry_project_keys.is_empty() {
                discovery
                    .projects
                    .retain(|project| retry_project_keys.contains(&project.project_key));
                discovery
                    .errors
                    .retain(|error| retry_project_keys.contains(&error.project_key));
            }
            let mut fresh = refresh_projects(
                &discovery.projects,
                host_state,
                &cleared_environment,
                refresh,
            );
            fresh.extend(
                discovery
                    .errors
                    .into_iter()
                    .map(|error| error.into_record(&cleared_environment)),
            );
            let error = (discovery.omitted_errors > 0).then(|| {
                format!(
                    "{} additional registered project layout error(s) omitted",
                    discovery.omitted_errors
                )
            });
            let retry_keys = fresh
                .iter()
                .filter(|record| {
                    matches!(
                        record.outcome,
                        SeedProjectOutcome::Busy
                            | SeedProjectOutcome::Failed { .. }
                            | SeedProjectOutcome::PrepareFailed { .. }
                    )
                })
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
            (records, error, retry_keys)
        }
        Err(error) => (prior, Some(error), Vec::new()),
    };
    // A full pass answers for every project; a scoped one keeps the records an
    // earlier result had to leave out.
    let omitted_before = if retry_project_keys.is_empty() {
        0
    } else {
        prior_omitted
    };
    write_seed_result(
        directory,
        &SeedResult::finished(at()?, error, records).with_omitted_before(omitted_before),
    )?;
    Ok(SeedOutcome {
        retry_project_keys: retry_keys,
    })
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
    if outcome.retry_project_keys.is_empty() {
        if queue.retry.is_none() {
            queue.retry_attempt = 0;
            queue.retry_at = None;
        }
    } else if !queue
        .pending
        .as_ref()
        .is_some_and(|pending| matches!(pending.scope, SeedScope::Full))
    {
        queue.retry_attempt = queue.retry_attempt.saturating_add(1);
        let retry = SeedRequest::busy(outcome.retry_project_keys);
        queue.retry = Some(merge_seed_requests(queue.retry.take(), retry));
        queue.retry_at.get_or_insert(now);
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

fn request_key_for_project(
    state: &Path,
    project: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
    use provenant_confinement::utility::{UtilityBudget, UtilityRuntime};
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    let executor = SystemExecutor::new(1)?;
    let utility = UtilityRuntime::read_only(&git, state)?;
    let budget = UtilityBudget::bounded(std::time::Duration::from_secs(60))?;
    let repo = utility.open_read_repo(
        &executor,
        project,
        budget,
        &provenant_core::CancelToken::new(),
    )?;
    Ok(provenant_run::target_seed::project_key(
        repo.common_git_dir(),
    ))
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
                |project| {
                    let key = request_key_for_project(state, project)?;
                    let Some(lane_request) = provenant_run::target_seed::read_request(state, &key)?
                    else {
                        return crate::target_seed_build::refresh(state, project);
                    };
                    let outcome =
                        crate::target_seed_build::refresh_requested(state, project, &lane_request);
                    let (name, generation, retry) = match &outcome {
                        Ok(crate::target_seed_build::Refresh::Built(id)) => {
                            ("built", Some(id.clone()), false)
                        }
                        Ok(crate::target_seed_build::Refresh::Current) => ("current", None, false),
                        Ok(crate::target_seed_build::Refresh::Busy) => ("busy", None, true),
                        Ok(crate::target_seed_build::Refresh::NotCargo) => {
                            ("not_cargo", None, false)
                        }
                        Ok(crate::target_seed_build::Refresh::IdentityMismatch) => {
                            ("identity_mismatch", None, false)
                        }
                        Err(_) => ("failed", None, true),
                    };
                    provenant_run::target_seed::acknowledge_request(
                        state,
                        &provenant_run::target_seed::SeedRequestResult {
                            schema: 1,
                            request: lane_request,
                            outcome: name.into(),
                            generation,
                        },
                        retry,
                    )?;
                    outcome
                },
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
    if error.contains("build.prepare") {
        SEED_PREPARE_REPAIR
    } else if error.contains("shared Cargo target seed is unavailable while") {
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
            SeedProjectOutcome::Failed { .. } | SeedProjectOutcome::PrepareFailed { .. } => {
                failed += 1
            }
            SeedProjectOutcome::NotCargo | SeedProjectOutcome::IdentityMismatch => {}
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
    for record in &result.projects {
        if matches!(record.outcome, SeedProjectOutcome::PrepareFailed { .. }) {
            push_distinct(&mut notes, SEED_PREPARE_REPAIR.to_owned());
        }
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
            let queued = read_seed_queue(&directory).unwrap_or_default();
            let incoming =
                provenant_run::target_seed::pending_request_keys(resolved.roots.state.as_path())
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|key| {
                        ![&queued.pending, &queued.active, &queued.retry]
                            .into_iter()
                            .flatten()
                            .any(|scope| {
                                matches!(scope.scope, SeedScope::Full)
                                    || scope.project_keys().contains(key)
                            })
                    })
                    .take(MAX_BUSY_SEED_PROJECTS)
                    .collect::<Vec<_>>();
            if !incoming.is_empty() || seed_retry_due(&directory, now) {
                let incoming = (!incoming.is_empty()).then(|| SeedRequest::busy(incoming));
                match queue_seed(&directory, now, incoming, || {
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
        let saved = |directory: &Directory| {
            let result = read_gc_result(directory).unwrap().unwrap();
            (result.state, result.failures)
        };

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
        assert!(queue_gc(&directory, found, || Err("not yet".into())).is_err());
        assert_eq!(saved(&directory), ("failed".to_owned(), 1));
        assert!(
            gc_due(&read_cadence(&directory).unwrap(), found),
            "an interruption keeps the day due"
        );

        let retry = after(found, 15 * 60);
        let report = queue_gc(&directory, after(found, 14 * 60), || Err("early".into())).unwrap();
        assert_eq!(report.state, "retry_pending");
        assert_eq!(report.retry_at, Some(retry));
        assert_eq!(saved(&directory), ("failed".to_owned(), 1));

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
            saved(&directory),
            ("queued".to_owned(), 1),
            "a retry carries its failures so the next one backs off"
        );

        // Interrupted again: the second delay is an hour.
        let second = after(retry, 61);
        assert!(queue_gc(&directory, second, || Err("not yet".into())).is_err());
        assert_eq!(saved(&directory), ("failed".to_owned(), 2));
        let report = queue_gc(&directory, after(second, 3_599), || Err("early".into())).unwrap();
        assert_eq!(report.state, "retry_pending");
        assert_eq!(report.retry_at, Some(after(second, 3_600)));
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
        let result = read_gc_result(&directory).unwrap().unwrap();
        assert_eq!(
            (
                result.state.as_str(),
                result.failures,
                result.error.as_deref()
            ),
            ("failed", 1, Some("spawn failed"))
        );
        assert!(
            gc_due(&read_cadence(&directory).unwrap(), now),
            "spawn failure must remain retryable"
        );

        // The next tick reports the old failure as waiting for its retry; it is
        // not an error, so it does not fail every tick.
        let task = Task::gc(|| queue_gc(&directory, after(now, 600), || Err("early".into())));
        assert_eq!(task.state, "retry_pending");
        assert!(task.error.is_none(), "{:?}", task.error);
        assert!(task.diagnostic.is_some());

        // A worker that fails its collection backs off the same way.
        let failed = after(now, 3_000);
        write_gc_result(&directory, failed, "failed", Some("busy graph".into()), 3).unwrap();
        let report = queue_gc(&directory, after(failed, 4 * 3_600 - 1), || Ok(())).unwrap();
        assert_eq!(report.state, "retry_pending");
        assert_eq!(report.retry_at, Some(after(failed, 4 * 3_600)));
        assert_eq!(
            queue_gc(&directory, after(failed, 4 * 3_600), || Ok(()))
                .unwrap()
                .state,
            "queued"
        );
        assert_eq!(read_gc_result(&directory).unwrap().unwrap().failures, 3);
    }

    /// Spec: docs/specs/state.md#retention-keeps-evidence
    #[test]
    fn a_future_dated_gc_result_is_stale_and_cannot_block_collection() {
        let temp = provenant_os::fixture::tempdir(|root| tempfile::tempdir_in(root)).unwrap();
        let directory = gc_directory(&temp);
        let now = WallTime::from_micros(10_000_000_000).unwrap();

        // A queued result from a clock that has since moved back gets no grace.
        write_gc_result(&directory, after(now, 3_600), "queued", None, 0).unwrap();
        assert!(queue_gc(&directory, now, || Err("not yet".into())).is_err());
        let result = read_gc_result(&directory).unwrap().unwrap();
        assert_eq!((result.state.as_str(), result.failures), ("failed", 1));
        assert_eq!(result.at, now, "the finding is dated by the current clock");

        // A failure dated in the future is retried at once, not after a delay
        // counted from a time that has not come.
        write_gc_result(&directory, after(now, DAY_SECONDS), "failed", None, 4).unwrap();
        let mut launches = 0;
        let report = queue_gc(&directory, now, || {
            launches += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!((report.state, launches), ("queued", 1));
        let result = read_gc_result(&directory).unwrap().unwrap();
        assert_eq!(
            (result.state.as_str(), result.failures, result.at),
            ("queued", 4, now)
        );
    }

    /// Spec: docs/specs/state.md#retention-keeps-evidence
    #[test]
    fn an_unreadable_gc_result_is_replaced_instead_of_blocking_collection() {
        let temp = provenant_os::fixture::tempdir(|root| tempfile::tempdir_in(root)).unwrap();
        let directory = gc_directory(&temp);
        let now = WallTime::from_micros(10_000_000_000).unwrap();
        let oversized = vec![b' '; MAX_GC_RESULT_BYTES as usize + 1];
        for bytes in [
            b"{not json".to_vec(),
            br#"{"state":5,"at":"x"}"#.to_vec(),
            oversized,
        ] {
            directory
                .atomic_write(GC_RESULT.as_ref(), "test", &bytes, PublishMode::Replace)
                .unwrap();
            assert!(read_gc_result(&directory).is_err());
            let mut launches = 0;
            let report = queue_gc(&directory, now, || {
                launches += 1;
                Ok(())
            })
            .unwrap();
            assert_eq!((report.state, launches), ("queued", 1));
            assert!(report.note.is_some(), "the replacement is reported once");
            let result = read_gc_result(&directory).unwrap().unwrap();
            assert_eq!(result.state, "queued");
            assert_eq!(
                queue_gc(&directory, now, || Err("duplicate".into()))
                    .unwrap()
                    .note,
                None,
                "once replaced, it is not reported again"
            );
        }
    }

    /// Spec: docs/specs/state.md#retention-keeps-evidence
    #[test]
    fn a_saved_gc_cause_is_bounded_and_the_result_keeps_its_shape() {
        let temp = provenant_os::fixture::tempdir(|root| tempfile::tempdir_in(root)).unwrap();
        let directory = gc_directory(&temp);
        let now = WallTime::from_micros(1_000_000).unwrap();
        write_gc_result(&directory, now, "failed", Some("x".repeat(100_000)), 1).unwrap();
        let result = read_gc_result(&directory).unwrap().unwrap();
        assert!(result.error.unwrap().len() <= MAX_RESULT_ERROR_BYTES + '…'.len_utf8());

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

    /// Spec: docs/specs/state.md#retention-keeps-evidence
    #[test]
    fn a_finished_gc_advances_the_deadline_and_seeds_while_a_failed_one_does_neither() {
        let temp = provenant_os::fixture::tempdir(|root| tempfile::tempdir_in(root)).unwrap();
        let directory = gc_directory(&temp);
        let now = WallTime::from_micros(10_000_000_000).unwrap();
        let clock = || Ok(now);
        let settle = |failures: u32, collected: Result<GcCompletion, String>| {
            settle_gc_run(&directory, clock, failures, collected, |at| {
                advance_gc_cadence(&directory, at)
            })
            .unwrap()
        };
        let saved = || {
            let result = read_gc_result(&directory).unwrap().unwrap();
            (result.state, result.failures)
        };

        assert!(
            !settle(0, Err("busy graph".into())),
            "no seed after a failure"
        );
        assert_eq!(saved(), ("failed".to_owned(), 1));
        assert!(!settle(1, Err("busy graph".into())));
        assert_eq!(saved(), ("failed".to_owned(), 2));
        assert!(
            gc_due(&read_cadence(&directory).unwrap(), now),
            "a failure leaves the day due"
        );

        assert!(settle(2, Ok(GcCompletion::Partial("budget".into()))));
        assert_eq!(
            saved(),
            ("partial".to_owned(), 0),
            "partial is not a failure"
        );
        let cadence = read_cadence(&directory).unwrap();
        assert!(
            !gc_due(&cadence, now),
            "a partial sweep advances the cadence"
        );
        assert!(gc_due(&cadence, after(now, DAY_SECONDS)));
        assert_eq!(
            queue_gc(&directory, after(now, 3_600), || Err("not due".into()))
                .unwrap()
                .state,
            "not_due",
            "a partial result is neither pending nor retried"
        );

        write_gc_result(&directory, now, "failed", None, 3).unwrap();
        assert!(settle(3, Ok(GcCompletion::Complete)));
        assert_eq!(
            saved(),
            ("complete".to_owned(), 0),
            "completion resets the count"
        );
    }

    /// Spec: docs/specs/state.md#retention-keeps-evidence
    #[test]
    fn a_completed_gc_is_not_recorded_failed_when_its_deadline_cannot_be_advanced() {
        let temp = provenant_os::fixture::tempdir(|root| tempfile::tempdir_in(root)).unwrap();
        let directory = gc_directory(&temp);
        let now = WallTime::from_micros(10_000_000_000).unwrap();
        let mut attempts = 0;
        let seed = settle_gc_run(
            &directory,
            || Ok(now),
            2,
            Ok(GcCompletion::Complete),
            |_| {
                attempts += 1;
                if attempts < GC_ADVANCE_ATTEMPTS {
                    Err("cadence lock busy".into())
                } else {
                    Ok(())
                }
            },
        )
        .unwrap();
        assert!(seed);
        assert_eq!(attempts, GC_ADVANCE_ATTEMPTS, "the advance is retried");
        let result = read_gc_result(&directory).unwrap().unwrap();
        assert_eq!((result.state.as_str(), result.failures), ("complete", 0));
        assert!(result.error.is_none());

        let seed = settle_gc_run(
            &directory,
            || Ok(now),
            0,
            Ok(GcCompletion::Complete),
            |_| Err("cadence lock busy".into()),
        )
        .unwrap();
        assert!(seed);
        let result = read_gc_result(&directory).unwrap().unwrap();
        assert_eq!((result.state.as_str(), result.failures), ("complete", 0));
        assert!(result.error.is_some(), "the missed advance is noted");
    }

    /// Spec: docs/specs/state.md#typed-host-metadata-in-format-2 (every new persisted field is optional or defaulted)
    #[test]
    fn a_skip_reason_from_a_newer_writer_does_not_make_the_seed_result_unreadable() {
        use crate::target_seed_build::SkipReason;
        let newer = br#"{"state":"complete","at":"2026-11-19T01:00:00.000000Z","error":null,"projects":[
            {"project":"/a","project_key":"a","state":"skipped","reason":"from_a_newer_build","detail":"d","repair":"r"},
            {"project":"/b","project_key":"b","state":"built"}]}"#;
        let result: SeedResult = serde_json::from_slice(newer).unwrap();
        assert!(matches!(
            &result.projects[0].outcome,
            SeedProjectOutcome::Skipped {
                reason: SkipReason::Unrecognised,
                ..
            }
        ));
        assert_eq!(seed_summary(&result), "ok:1 skipped:1");
        assert!(!SkipReason::Unrecognised.repair().is_empty());
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn prepare_failed_records_the_command_and_tail_without_echoing_it() {
        let at = WallTime::from_micros(1_000_000).unwrap();
        let tail = "error: generated module missing\n".repeat(100);
        let record = SeedProjectRecord {
            project: "/repo".into(),
            project_key: "repo".into(),
            cleared_environment: Vec::new(),
            outcome: SeedProjectOutcome::PrepareFailed {
                command: vec!["node".into(), "tools/ensure-generated.mjs".into()],
                tail: tail.clone(),
            },
        };
        let result = SeedResult::finished(at, None, vec![record]);
        assert_eq!(result.state, "failed", "a prepare failure fails the pass");
        assert_eq!(seed_summary(&result), "failed:1");
        let bytes = serde_json::to_vec(&result).unwrap();
        let wire: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(wire["projects"][0]["state"], "prepare_failed");
        assert_eq!(wire["projects"][0]["tail"], tail);
        let decoded: SeedResult = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded.state, "failed");
        assert!(matches!(
            &decoded.projects[0].outcome,
            SeedProjectOutcome::PrepareFailed { command, tail: saved }
                if command.len() == 2
                    && command[0] == "node"
                    && command[1] == "tools/ensure-generated.mjs"
                    && saved == &tail
        ));
        let diagnostic = seed_diagnostic(&decoded).unwrap();
        assert!(diagnostic.contains(SEED_PREPARE_REPAIR));
        assert!(
            !diagnostic.contains("generated module missing"),
            "tail stays in the result"
        );

        // An older result that never had a prepare_failed record still decodes.
        let older =
            br#"{"state":"complete","at":"2026-11-19T01:00:00.000000Z","error":null,"projects":[
            {"project":"/a","project_key":"a","state":"built"}]}"#;
        let older: SeedResult = serde_json::from_slice(older).unwrap();
        assert_eq!(seed_summary(&older), "ok:1");
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn seed_result_stores_only_the_masked_prepare_tail() {
        let _mode = provenant_redact::scoped(provenant_redact::Mode::Full);
        let secret = "seed-result-secret-3391";
        let _scope = provenant_redact::publication_scope([secret.as_bytes().to_vec()]).unwrap();
        let raw_tail = format!("npm ERR! rejected {secret} (Cannot find package 'zod')\n");
        // run_with_tail masks on the capture stream, so the tail tick receives
        // is already masked. Mirror that with the same known-shapes core, then
        // assert the persisted seed-result holds only that masked form.
        let masked_tail =
            String::from_utf8(provenant_redact::known_shapes(raw_tail.as_bytes()).unwrap())
                .unwrap();
        assert!(masked_tail.contains("[REDACTED]"));
        assert!(!masked_tail.contains(secret));

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let directory = provenant_config::private_directory(&root, true).unwrap();
        let project = seed_project(&root, "masked", true);
        seed_pass(
            &directory,
            &root,
            &seed_clock(),
            true,
            &SeedRequest::full(),
            || Ok(SeedDiscovery::from_paths(vec![project.clone()])),
            |_| {
                Err(Box::new(crate::target_seed_build::PrepareFailed {
                    index: 0,
                    argv: vec!["node".into(), "generate.mjs".into()],
                    exit: Some(7),
                    tail: masked_tail.clone(),
                }))
            },
        )
        .unwrap();

        let stored = directory
            .read_bounded(SEED_RESULT.as_ref(), MAX_SEED_RESULT_BYTES)
            .unwrap();
        let stored = String::from_utf8(stored).unwrap();
        let wire: serde_json::Value = serde_json::from_slice(stored.as_bytes()).unwrap();
        assert_eq!(wire["projects"][0]["state"], "prepare_failed");
        assert!(stored.contains("[REDACTED]"), "{stored}");
        assert!(!stored.contains(secret), "{stored}");
        let records = seed_records(&directory);
        assert!(matches!(
            &records[0].outcome,
            SeedProjectOutcome::PrepareFailed { tail, .. } if tail == &masked_tail
        ));
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn a_seed_result_stays_under_its_read_cap_and_keeps_its_omitted_count() {
        let fat = |index: usize| SeedProjectRecord {
            project: format!("/repo/{index}"),
            project_key: format!("{index}"),
            cleared_environment: Vec::new(),
            outcome: SeedProjectOutcome::Failed {
                // Control characters escape to six bytes each in JSON.
                error: "\u{1}".repeat(MAX_SEED_ERROR_BYTES),
            },
        };
        let projects: Vec<_> = (0..MAX_SEED_PROJECT_RECORDS + 10).map(fat).collect();
        let at = WallTime::from_micros(1_000_000).unwrap();
        let result = SeedResult::new(at, "failed", Some("e".repeat(100_000)), projects);
        let bytes = serde_json::to_vec(&result).unwrap();
        assert!(
            (bytes.len() as u64) < MAX_SEED_RESULT_BYTES,
            "{} bytes",
            bytes.len()
        );
        assert!(result.omitted_projects >= 10);
        assert_eq!(
            result.projects.len() + result.omitted_projects,
            MAX_SEED_PROJECT_RECORDS + 10,
            "every dropped record is counted"
        );
        assert!(result.error.unwrap().len() <= MAX_SEED_ERROR_BYTES + '…'.len_utf8());

        // Progress rewrites keep the count of what an earlier pass left out.
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let directory = provenant_config::private_directory(&root, true).unwrap();
        write_seed_result(
            &directory,
            &SeedResult::new(at, "complete", None, vec![fat(0)]).with_omitted_before(7),
        )
        .unwrap();
        write_seed_progress(&directory, at, "running", None).unwrap();
        let progress = read_seed_result(&directory).unwrap().unwrap();
        assert_eq!(progress.omitted_projects, 7);
        assert_eq!(progress.projects.len(), 1);
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
                    cleared_environment: Vec::new(),
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
    fn retry_delay_doubles_and_caps_at_one_hour() {
        let delays: Vec<_> = (0..8).map(super::retry_delay).collect();
        assert_eq!(
            delays,
            vec![60, 120, 240, 480, 960, 1920, 3600, 3600]
                .into_iter()
                .map(|seconds| seconds * 1_000_000)
                .collect::<Vec<_>>()
        );
        assert_eq!(super::retry_delay(u32::MAX), 3_600_000_000);
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
                retry_project_keys: vec!["project-a".into()],
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
    fn unrelated_pending_work_does_not_bypass_retry_backoff() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let directory = provenant_config::private_directory(&root, true).unwrap();
        let now = WallTime::from_micros(1_000_000).unwrap();
        let queue = SeedQueue {
            pending: Some(SeedRequest::busy(vec!["other".into()])),
            ..Default::default()
        };
        write_seed_queue(&directory, &queue).unwrap();
        let mut worker = Some(
            directory
                .try_lock("seed-worker.lock".as_ref(), LockMode::Exclusive)
                .unwrap(),
        );
        let next = finish_seed_job(
            &directory,
            now,
            &mut worker,
            SeedOutcome {
                retry_project_keys: vec!["failed".into()],
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(next, SeedRequest::busy(vec!["other".into()]));
        finish_seed_job(&directory, now, &mut worker, SeedOutcome::default()).unwrap();
        let saved = read_seed_queue(&directory).unwrap();
        assert_eq!(saved.retry, Some(SeedRequest::busy(vec!["failed".into()])));
        assert!(!seed_retry_due(&directory, now));
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn busy_and_failed_retry_with_capped_backoff() {
        use crate::target_seed_build::Refresh;
        let temp = tempfile::tempdir().unwrap();
        // macOS temp roots sit behind a /var symlink, which private_directory refuses.
        let root = temp.path().canonicalize().unwrap();
        let directory = provenant_config::private_directory(&root, true).unwrap();
        let project = |name: &str| seed_project(&root, name, true);
        let projects = vec![project("a"), project("b"), project("c"), project("d")];
        let entries = SeedDiscovery::from_paths(projects.clone()).projects;
        let busy_key = entries[0].project_key.clone();
        let failed_key = entries[1].project_key.clone();
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
        assert_eq!(
            outcome.retry_project_keys,
            vec![busy_key.clone(), failed_key.clone()]
        );
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

        assert_eq!(read_seed_queue(&directory).unwrap().retry_attempt, 1);
        assert_eq!(retry_delay(1), 120_000_000);
        assert!(!seed_retry_due(&directory, wall), "just written: not due");
        let far = WallTime::from_micros(wall.as_micros() + SEED_WORK_RETRY_MICROS + 1).unwrap();
        assert!(
            seed_retry_due(&directory, far),
            "busy and failed projects are retried next tick"
        );
        let retry = read_seed_queue(&directory).unwrap().retry.unwrap();
        assert_eq!(
            retry.scope,
            SeedScope::Busy(vec![busy_key.clone(), failed_key.clone()])
        );
        queue_seed(&directory, far, None, || Ok(())).unwrap();
        let queued = read_seed_queue(&directory).unwrap();
        assert_eq!(
            queued.pending,
            Some(SeedRequest::busy(vec![
                busy_key.clone(),
                failed_key.clone()
            ]))
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
                assert!(matches!(
                    project.file_name().unwrap().to_str(),
                    Some("a" | "b")
                ));
                Ok(Refresh::Current)
            },
        )
        .unwrap();
        assert!(
            finish_seed_job(&directory, far, &mut worker_lock, outcome)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            *retried.borrow(),
            vec![projects[0].clone(), projects[1].clone()]
        );
        assert_eq!(
            seed_state(&directory),
            "ok:4",
            "the retry kept the seeded projects and recovered both failures"
        );
        assert!(
            !seed_retry_due(&directory, far),
            "resolved work is not retried"
        );

        assert_eq!(read_seed_queue(&directory).unwrap().retry_attempt, 0);
        assert_eq!(retry_delay(u32::MAX), 3_600_000_000);
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
            &[],
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
        let much_later = after(seed_clock().wall_now().unwrap(), 10 * DAY_SECONDS);
        assert!(
            !seed_retry_due(&directory, much_later),
            "a skip leaves no work to retry"
        );
    }

    /// Spec: docs/specs/state.md#typed-host-metadata-in-format-2 (every new persisted field is optional or defaulted)
    #[test]
    fn seed_result_records_cleared_environment() {
        let at = WallTime::from_micros(1_000_000).unwrap();
        let result = SeedResult::finished(
            at,
            None,
            vec![
                SeedProjectRecord {
                    project: "/repo/cleared".into(),
                    project_key: "cleared".into(),
                    cleared_environment: vec!["RUSTC_WRAPPER".into(), "RUSTUP_TOOLCHAIN".into()],
                    outcome: SeedProjectOutcome::Built,
                },
                SeedProjectRecord {
                    project: "/repo/plain".into(),
                    project_key: "plain".into(),
                    cleared_environment: Vec::new(),
                    outcome: SeedProjectOutcome::Current,
                },
            ],
        );
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(
            json["projects"][0]["cleared_environment"],
            serde_json::json!(["RUSTC_WRAPPER", "RUSTUP_TOOLCHAIN"])
        );
        assert!(
            json["projects"][1].get("cleared_environment").is_none(),
            "an empty set is omitted rather than written as a host key"
        );
        let round_trip: SeedResult = serde_json::from_value(json).unwrap();
        assert_eq!(round_trip.projects, result.projects);
        assert_eq!(
            round_trip.projects[0].cleared_environment,
            ["RUSTC_WRAPPER", "RUSTUP_TOOLCHAIN"]
        );

        // A result written before the field existed still decodes with no keys.
        let older: SeedResult = serde_json::from_slice(
            br#"{"state":"complete","at":"2026-11-17T12:49:25.659684Z","projects":[{"project":"/repo/old","project_key":"old","state":"built"}]}"#,
        )
        .unwrap();
        assert!(older.projects[0].cleared_environment.is_empty());
    }

    /// Spec: docs/specs/state.md#typed-host-metadata-in-format-2 (every new persisted field is optional or defaulted)
    #[test]
    fn the_seed_result_records_typed_per_project_outcomes_and_reads_an_older_result() {
        use crate::target_seed_build::SkipReason;
        let record = |key: &str, outcome| SeedProjectRecord {
            project: format!("/repo/{key}"),
            project_key: key.into(),
            cleared_environment: Vec::new(),
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
