//! Scheduled host work. Deadlines live on disk; this process owns no timers.
use crate::worktrees::GcCompletion;
use provenant_cli::Globals;
use provenant_confinement::worktrees::WorkerObserver;
#[cfg(test)]
use provenant_core::MonoTime;
use provenant_core::{Clock, Deadline, DurationMicros, WaitBudget, WallTime};
use provenant_os::{
    clock::SystemClock,
    fs::{Directory, LockGuard, LockMode, PublishMode},
};
use provenant_records::{CommandResult, Outcome};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, UNIX_EPOCH},
};

mod registry;
mod steward;
pub(crate) use registry::Cursor;
use registry::{TaskDef, TaskLeases};

const DAY_SECONDS: u64 = 86_400;
/// An unfinished scheduled collection retries after this delay, then after
/// each factor-fold longer one, and never later than the daily cadence.
const GC_RETRY_BASE_SECONDS: u64 = 15 * 60;
const GC_RETRY_FACTOR: u64 = 4;
const GC_RESULT: &str = "gc-result.json";
/// Bound on an error saved in a task result, so a result never outgrows its read cap.
const MAX_RESULT_ERROR_BYTES: usize = 1024;
/// Bound on a saved cause quoted in a tick note.
const MAX_NOTE_CAUSE_BYTES: usize = 256;
const MAX_GC_RESULT_BYTES: u64 = 64 * 1024;
/// Times a completed collection tries to advance its deadline.
const GC_ADVANCE_ATTEMPTS: u32 = 3;
/// A queued worker launched within this window is still considered live.
const WORKER_LAUNCH_GRACE_MICROS: i64 = 60_000_000;

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
pub(super) struct TaskResult {
    #[serde(flatten)]
    extra: provenant_records::ExtraFields,
    state: String,
    at: WallTime,
    error: Option<String>,
    /// An operator note that is not a failure, such as a gc budget detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    diagnostic: Option<String>,
    /// Consecutive attempts that did not complete. It selects the retry delay
    /// after a failure and restarts at zero when a collection completes.
    #[serde(default, skip_serializing_if = "is_zero")]
    failures: u32,
}

impl TaskResult {
    fn disabled(at: WallTime) -> Self {
        Self {
            extra: Default::default(),
            state: "disabled".into(),
            at,
            error: None,
            diagnostic: None,
            failures: 0,
        }
    }

    fn busy(at: WallTime, holder: &str) -> Self {
        Self {
            extra: Default::default(),
            state: "busy".into(),
            at,
            error: None,
            diagnostic: Some(format!(
                "TICK_TASK_LEASE_BUSY: single-flight lease unavailable: {holder}"
            )),
            failures: 0,
        }
    }

    fn over_budget(at: WallTime, task: &str) -> Self {
        Self {
            extra: Default::default(),
            state: "over_budget".into(),
            at,
            error: None,
            diagnostic: Some(format!(
                "TICK_TASK_OVERRUN: domains.tick.tasks.{task}.budget_seconds elapsed before the worker ended"
            )),
            failures: 0,
        }
    }

    #[cfg(test)]
    fn sample(at: WallTime) -> Self {
        Self {
            extra: Default::default(),
            state: "observed".into(),
            at,
            error: None,
            diagnostic: None,
            failures: 0,
        }
    }
}

#[cfg(test)]
fn test_wall() -> WallTime {
    WallTime::from_micros(1_700_000_000_000_000)
        .expect("a fixed in-range wall time for task registry tests")
}

pub(crate) fn publish<T: Serialize>(
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
        diagnostic: None,
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
        && (0..WORKER_LAUNCH_GRACE_MICROS)
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

/// What the worker records once the collection has ended. The deadline moves
/// only for a complete or partial collection, and the advance is tried a few
/// times so a busy cadence lock does not turn a finished collection into a
/// failed one; a deadline that still could not be advanced is a note on the
/// completed result. A failure counts toward the retry backoff, and any other
/// outcome resets it.
fn settle_gc_run(
    directory: &Directory,
    now: impl Fn() -> Result<WallTime, String>,
    failures: u32,
    collected: Result<GcCompletion, String>,
    mut advance: impl FnMut(WallTime) -> Result<(), String>,
) -> Result<(), String> {
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
            Ok(())
        }
        Err(error) => {
            write_gc_result(
                directory,
                now()?,
                "failed",
                Some(error),
                failures.saturating_add(1),
            )?;
            Ok(())
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
                Some(&resolved.home),
            )
        })();
        let failed = collected.as_ref().err().cloned();
        settle_gc_run(
            &directory,
            || clock.wall_now().map_err(|error| error.to_string()),
            failures,
            collected,
            |completed_at| advance_gc_cadence(&directory, completed_at),
        )?;
        // Build-slot GC and prewarm follow a collection that finished its work.
        for warning in schedule_build_hygiene(&resolved) {
            provenant_records::warning::warn(
                provenant_records::warning::Warning::new(
                    provenant_records::warning::BUILD_HYGIENE_ADVISORY,
                    warning,
                    provenant_records::warning::detected_source(
                        provenant_records::warning::BUILD_HYGIENE_ADVISORY,
                    ),
                )
                .once_per_change(),
            );
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
            "resolve the reported GC error and rerun `provenant tick`",
            globals,
        ),
    }
}

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
/// project after a completed collection. Every failure is a warning: host
/// hygiene never fails the tick.
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
    let limit = provenant_config::ResolvedAdmissionClass::resolve("build", state, None).slots;
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
    let now_ms = provenant_os::clock::now_unix_millis();
    let now = UNIX_EPOCH + Duration::from_millis(now_ms);
    let report = hygiene::gc_projects(&resolved.home, state, limit, now, |key| {
        settings.get(key).copied().unwrap_or(default)
    });
    let mut warnings = report.warnings;
    if report.rotated_ledgers > 0 {
        provenant_records::warning::warn(
            provenant_records::warning::Warning::new(
                provenant_records::warning::BUILD_LEDGER_ROTATED,
                format!(
                    "build ledger rotated; {} rotation(s) expired",
                    report.expired_ledgers
                ),
                provenant_records::warning::detected_source(
                    provenant_records::warning::BUILD_LEDGER_ROTATED,
                ),
            )
            .once_per_change(),
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

/// Keep the durable result shape while reusing the existing task bodies.
fn result_from_task(task: Task, at: WallTime) -> TaskResult {
    TaskResult {
        extra: Default::default(),
        state: task.state,
        at,
        error: task.error,
        diagnostic: task.diagnostic,
        failures: 0,
    }
}

pub(crate) fn read_cursor(directory: &Directory, task: &str) -> Result<Cursor, String> {
    let name = format!("cursor-{task}.json");
    match directory.read_bounded(name.as_ref(), 64 * 1024) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| error.to_string()),
        Err(error) if error.is_not_found() => Ok(Cursor::default()),
        Err(error) => Err(error.to_string()),
    }
}

/// Recovery is independent of graph journals: even an orphaned attempt must
/// eventually settle. Share one host census across all registered projects.
fn reap_projects(
    resolved: &crate::roots::Resolved,
    deadline: Deadline,
) -> Result<&'static str, String> {
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
        if deadline.is_expired(clock.mono_now()) {
            break;
        }
        // A vanished checkout or common directory has nothing to settle. Report
        // it once with the unregister command rather than failing every tick.
        if crate::project_root::absent(&binding) {
            crate::project_root::warn_vanished(&binding);
            continue;
        }
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
    only: &[String],
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
    let knob = |name: &str| config.knobs.get(name).map(|entry| &entry.value);
    let enabled = |task: &str| {
        knob(&format!("domains.tick.tasks.{task}.enabled"))
            .and_then(|value| value.as_bool())
            .unwrap_or(true)
    };
    let budget = |task: &str, seconds: u64| -> DurationMicros {
        let value = knob(&format!("domains.tick.tasks.{task}.budget_seconds"))
            .and_then(|value| value.as_u64())
            .unwrap_or(seconds);
        DurationMicros::from_seconds(value).unwrap_or_else(|_| {
            DurationMicros::from_seconds(seconds).expect("a valid default budget")
        })
    };
    let concurrency = knob("domains.tick.concurrency")
        .and_then(|value| value.as_u64())
        .unwrap_or(4) as usize;
    let selected = |name: &str| only.is_empty() || only.iter().any(|entry| entry == name);
    let holder = format!("tick:{}", std::process::id());
    let graphs = Arc::new(Mutex::new(Graphs::default()));
    let mut tasks: Vec<TaskDef> = Vec::new();

    if selected("settlement") {
        let resolved = resolved.clone();
        tasks.push(TaskDef::new(
            "settlement",
            enabled("settlement"),
            budget("settlement", 30),
            move |ctx| {
                result_from_task(
                    Task::run("settlement", || reap_projects(&resolved, ctx.deadline)),
                    ctx.wall,
                )
            },
        ));
    }
    if selected("graphs") {
        let graphs_out = Arc::clone(&graphs);
        let resolved = resolved.clone();
        tasks.push(TaskDef::new(
            "graphs",
            enabled("graphs"),
            budget("graphs", 60),
            move |ctx| {
                let report = crate::fabric::tick_graphs(&resolved, ctx.wall, workers, ctx.deadline)
                    .unwrap_or_else(|error| Graphs {
                        advanced: 0,
                        errors: vec![error.to_string()],
                        warnings: Vec::new(),
                    });
                let state = if report.errors.is_empty() {
                    "advanced"
                } else {
                    "failed"
                };
                let error = (!report.errors.is_empty()).then(|| report.errors.join("; "));
                let warnings = report.warnings.len();
                *graphs_out.lock().unwrap() = report;
                let mut result = result_from_task(Task::reported("graphs", state, None), ctx.wall);
                result.error = error;
                if warnings > 0 {
                    result.diagnostic = Some(format!("{warnings} warning(s)"));
                }
                result
            },
        ));
    }
    if selected("forge_events") {
        let resolved = resolved.clone();
        tasks.push(TaskDef::new(
            "forge_events",
            enabled("forge_events"),
            budget("forge_events", 30),
            move |ctx| {
                result_from_task(
                    Task::run("forge_events", || {
                        crate::event_delivery::tick(&resolved, ctx.deadline)
                            .map(|()| "observed")
                            .map_err(|error| error.to_string())
                    }),
                    ctx.wall,
                )
            },
        ));
    }
    if selected("gc") {
        let resolved = resolved.clone();
        // GC cadence is serialised across manual ticks and launchd invocations,
        // separately from each graph's journal custody.
        tasks.push(TaskDef::new(
            "gc",
            enabled("gc") && config.gc.schedule,
            budget("gc", 5),
            move |ctx| {
                result_from_task(
                    Task::gc(|| -> Result<GcReport, String> {
                        if ctx.deadline.is_expired(SystemClock.mono_now()) {
                            return Ok(GcReport::state("over budget; next tick retries"));
                        }
                        let directory = provenant_config::private_directory(
                            &resolved.roots.state.as_path().join("tick"),
                            true,
                        )
                        .map_err(|error| error.to_string())?;
                        let _lock = match directory
                            .try_lock("cadence.lock".as_ref(), LockMode::Exclusive)
                        {
                            Ok(lock) => lock,
                            Err(provenant_os::fs::FsError::LockTimeout { .. }) => {
                                return Ok(GcReport::state("busy; next tick retries"));
                            }
                            Err(error) => return Err(error.to_string()),
                        };
                        queue_gc(&directory, ctx.wall, || {
                            let executable =
                                std::env::current_exe().map_err(|error| error.to_string())?;
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
                    }),
                    ctx.wall,
                )
            },
        ));
    }
    if selected("steward") {
        let resolved = resolved.clone();
        let steward_enabled = enabled("steward");
        let steward_budget = budget("steward", 30);
        let steward_config = config.domains.steward;
        tasks.push(TaskDef::new(
            "steward",
            steward_enabled,
            steward_budget,
            move |ctx| {
                result_from_task(
                    steward::run(&steward_config, &resolved, workers, ctx.deadline),
                    ctx.wall,
                )
            },
        ));
    }

    let outcomes = registry::run_tasks(
        tasks,
        concurrency,
        &TaskLeases::new(resolved.roots.state.as_path().to_owned()),
        &clock,
        now,
        &holder,
    );
    let task_rows: Vec<Task> = outcomes
        .into_iter()
        .map(|outcome| Task {
            task: outcome.name,
            state: outcome.result.state,
            error: outcome.result.error,
            diagnostic: outcome.result.diagnostic,
        })
        .collect();
    let graphs = std::mem::take(&mut *graphs.lock().expect("the graph output lock is intact"));
    Ok(Tick {
        graphs,
        tasks: task_rows,
    })
}

fn task_summary(tasks: &[Task]) -> String {
    tasks
        .iter()
        .map(|task| format!("{}={}", task.task, task.state))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn run_selected(
    globals: &Globals,
    workers: &'static dyn WorkerObserver,
    only: &[String],
) -> u8 {
    match tick(globals, workers, only) {
        Ok(data) => {
            let failed = !data.graphs.errors.is_empty()
                || data.tasks.iter().any(|task| task.error.is_some());
            let states = task_summary(&data.tasks);
            let mut human = format!(
                "tick: graphs_advanced={} errors={} {}\n",
                data.graphs.advanced,
                data.graphs.errors.len(),
                states
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
                        .map(|diagnostic| format!("warning: {}: {diagnostic}", task.task))
                }))
                .collect();
            let summary = provenant_cli::Summary::new(human.trim(), rows);
            let next = failed.then(|| {
                provenant_records::command_result::NextStep::from_command(
                    "provenant tick --json",
                    "inspect the failed scheduled tasks",
                )
            });
            crate::emit_summarised(
                &CommandResult::new(
                    "tick",
                    if failed { Outcome::Error } else { Outcome::Ok },
                    data,
                )
                .with_next(next.into_iter().collect()),
                globals,
                &human,
                false,
                Some(&summary),
            )
        }
        Err(error) => crate::fail(
            "tick",
            Outcome::Error,
            "tick",
            &error.to_string(),
            "correct the reported tick error and rerun `provenant tick`",
            globals,
        ),
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

    // Spec: docs/specs/supervisor.md#tick-driven-settlement
    #[test]
    fn settlement_skips_a_binding_whose_common_directory_is_gone() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let home = root.join("home");
        let project = root.join("project");
        let state = root.join("state");
        for path in [&home, &project, &state] {
            std::fs::create_dir(path).unwrap();
        }
        provenant_os::fixture::private_state_dir(&state).unwrap();
        let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
        inputs.state_dir = Some(state.clone());
        let roots = inputs
            .resolve(&provenant_os::fs::SystemPathResolver)
            .unwrap();
        provenant_os::fixture::run_git(&project, &["init", "-q"]).unwrap();
        provenant_os::fixture::run_git(&project, &["commit", "--allow-empty", "-qm", "base"])
            .unwrap();
        provenant_config::bind_project(
            &roots,
            provenant_config::ProjectLocation::new(project.join(".git")).unwrap(),
        )
        .unwrap();
        std::fs::remove_dir_all(&project).unwrap();
        let resolved = crate::roots::Resolved {
            assets: root.join("assets"),
            home,
            project,
            roots,
            claude_config: None,
            codex_home: None,
        };
        assert_eq!(
            reap_projects(&resolved, Deadline::at(MonoTime::from_micros(u64::MAX))).unwrap(),
            "queued"
        );
    }

    // Spec: docs/specs/supervisor.md#tick-driven-settlement
    #[test]
    fn settlement_warns_once_and_skips_a_project_whose_checkout_vanished() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let home = root.join("home");
        let project = root.join("project");
        let state = root.join("state");
        let common = root.join("common.git");
        for path in [&home, &project, &state] {
            std::fs::create_dir(path).unwrap();
        }
        provenant_os::fixture::private_state_dir(&state).unwrap();
        let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
        inputs.state_dir = Some(state.clone());
        let roots = inputs
            .resolve(&provenant_os::fs::SystemPathResolver)
            .unwrap();
        provenant_os::fixture::run_git(
            &project,
            &["init", "-q", "--separate-git-dir", common.to_str().unwrap()],
        )
        .unwrap();
        provenant_os::fixture::run_git(&project, &["commit", "--allow-empty", "-qm", "base"])
            .unwrap();
        let binding = provenant_config::bind_project(
            &roots,
            provenant_config::ProjectLocation::from_verified_primary_checkout(
                common.canonicalize().unwrap(),
                project.canonicalize().unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        // Delete only the checkout: the separate Git common directory survives.
        std::fs::remove_dir_all(&project).unwrap();
        assert!(binding.common_dir_present());
        assert!(!binding.primary_checkout_present());

        let resolved = crate::roots::Resolved {
            assets: root.join("assets"),
            home,
            project,
            roots,
            claude_config: None,
            codex_home: None,
        };
        let (result, warnings) = provenant_records::warning::capture(|| {
            reap_projects(&resolved, Deadline::at(MonoTime::from_micros(u64::MAX)))
        });
        assert_eq!(
            result.unwrap(),
            "queued",
            "the vanished root is not a task failure"
        );
        let warned = warnings
            .iter()
            .filter(|warning| {
                warning.code.as_ref() == provenant_records::warning::PROJECT_ROOT_VANISHED.0
            })
            .collect::<Vec<_>>();
        assert_eq!(warned.len(), 1, "reported once: {warnings:?}");
        let subject = binding.project_id().to_string();
        assert_eq!(warned[0].subject.as_deref(), Some(subject.as_str()));
        assert!(
            warned[0]
                .next
                .iter()
                .any(|step| step.display_text().contains("provenant gc --apply")),
            "the warning names the unregister command: {:?}",
            warned[0]
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
    fn a_finished_gc_advances_the_deadline_while_a_failed_one_does_neither() {
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

        settle(0, Err("busy graph".into()));
        assert_eq!(saved(), ("failed".to_owned(), 1));
        settle(1, Err("busy graph".into()));
        assert_eq!(saved(), ("failed".to_owned(), 2));
        assert!(
            gc_due(&read_cadence(&directory).unwrap(), now),
            "a failure leaves the day due"
        );

        settle(2, Ok(GcCompletion::Partial("budget".into())));
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
        settle(3, Ok(GcCompletion::Complete));
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
        settle_gc_run(
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
        assert_eq!(attempts, GC_ADVANCE_ATTEMPTS, "the advance is retried");
        let result = read_gc_result(&directory).unwrap().unwrap();
        assert_eq!((result.state.as_str(), result.failures), ("complete", 0));
        assert!(result.error.is_none());

        settle_gc_run(
            &directory,
            || Ok(now),
            0,
            Ok(GcCompletion::Complete),
            |_| Err("cadence lock busy".into()),
        )
        .unwrap();
        let result = read_gc_result(&directory).unwrap().unwrap();
        assert_eq!((result.state.as_str(), result.failures), ("complete", 0));
        assert!(result.error.is_some(), "the missed advance is noted");
    }
}
