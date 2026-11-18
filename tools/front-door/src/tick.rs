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
const MAX_SEED_DISCOVERY_ERRORS: usize = 16;
const MAX_SEED_ERROR_BYTES: usize = 1024;
const MAX_BUSY_SEED_PROJECTS: usize = 512;
const MAX_SEED_RESULT_ERROR_BYTES: usize = 16 * 1024;
const SEED_WORK_RETRY_MICROS: i64 = 60_000_000;

#[derive(Default, Serialize, Deserialize)]
struct Cadence {
    #[serde(flatten)]
    extra: provenant_records::ExtraFields,
    next_gc_at: Option<WallTime>,
}

#[derive(Serialize, Deserialize)]
struct GcResult {
    #[serde(flatten)]
    extra: provenant_records::ExtraFields,
    state: String,
    at: WallTime,
    error: Option<String>,
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
    prior_error: Option<String>,
}

impl SeedRequest {
    fn full() -> Self {
        Self {
            scope: SeedScope::Full,
            prior_error: None,
        }
    }

    fn busy(project_keys: Vec<String>, prior_error: Option<String>) -> Self {
        Self {
            scope: SeedScope::Busy(project_keys),
            prior_error,
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

fn write_gc_result(
    directory: &Directory,
    at: WallTime,
    state: &str,
    error: Option<String>,
) -> Result<(), String> {
    write_result(directory, "gc-result.json", "tick-gc", at, state, error)
}

fn write_result(
    directory: &Directory,
    file: &str,
    purpose: &str,
    at: WallTime,
    state: &str,
    error: Option<String>,
) -> Result<(), String> {
    let result = GcResult {
        extra: Default::default(),
        state: state.into(),
        at,
        error,
    };
    let bytes = serde_json::to_vec(&result).map_err(|error| error.to_string())?;
    directory
        .atomic_write(file.as_ref(), purpose, &bytes, PublishMode::Replace)
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// The cadence lock is held by the caller. Spawn errors retain a due deadline;
/// completion belongs to the collector's one-shot worker, never the scheduler.
fn queue_gc(
    directory: &Directory,
    now: WallTime,
    spawn: impl FnOnce() -> Result<(), String>,
) -> Result<&'static str, String> {
    let mut cadence = read_cadence(directory).map_err(|error| error.to_string())?;
    if !gc_due(&cadence, now) {
        return match directory.read_bounded("gc-result.json".as_ref(), 64 * 1024) {
            Ok(bytes) => {
                let mut result: GcResult =
                    serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
                if result.state == "running"
                    || result.state == "queued"
                        && now.as_micros().saturating_sub(result.at.as_micros())
                            >= SEED_WORK_RETRY_MICROS
                {
                    let _worker = match directory
                        .try_lock("gc-worker.lock".as_ref(), LockMode::Exclusive)
                    {
                        Ok(lock) => lock,
                        Err(provenant_os::fs::FsError::LockTimeout { .. }) => return Ok("running"),
                        Err(error) => return Err(error.to_string()),
                    };
                    // The worker may have completed between the snapshot and
                    // our lock probe. Re-read its atomic result under custody.
                    let bytes = directory
                        .read_bounded("gc-result.json".as_ref(), 64 * 1024)
                        .map_err(|error| error.to_string())?;
                    result = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
                    if matches!(result.state.as_str(), "running" | "queued") {
                        let error =
                            "scheduled GC interrupted; next: provenant gc --apply".to_owned();
                        write_gc_result(directory, now, "failed", Some(error.clone()))?;
                        return Err(error);
                    }
                }
                let state = match result.state.as_str() {
                    "running" => "running",
                    "queued" => "queued",
                    _ => "not_due",
                };
                result.error.map_or(Ok(state), Err)
            }
            Err(error) if error.is_not_found() => Ok("not_due"),
            Err(error) => Err(error.to_string()),
        };
    }
    write_gc_result(directory, now, "queued", None)?;
    if let Err(error) = spawn() {
        write_gc_result(directory, now, "failed", Some(error.clone()))?;
        return Err(error);
    }
    cadence.next_gc_at = Some(
        now.checked_add(
            DurationMicros::from_seconds(DAY_SECONDS).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?,
    );
    let bytes = serde_json::to_vec(&cadence).map_err(|error| error.to_string())?;
    directory
        .atomic_write(
            "cadence.json".as_ref(),
            "tick-cadence",
            &bytes,
            PublishMode::Replace,
        )
        .map_err(|error| error.to_string())?;
    Ok("queued")
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
        write_gc_result(&directory, clock.wall_now()?, "running", None)?;
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
        })();
        write_gc_result(
            &directory,
            clock.wall_now()?,
            if collected.is_ok() {
                "complete"
            } else {
                "failed"
            },
            collected.as_ref().err().cloned(),
        )?;
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

fn seed_queue_lock(directory: &Directory) -> Result<LockGuard, String> {
    let clock = SystemClock;
    let budget = WaitBudget::up_to(
        clock.mono_now(),
        DurationMicros::from_seconds(5).map_err(|error| error.to_string())?,
    );
    directory
        .lock(
            "seed-queue.lock".as_ref(),
            LockMode::Exclusive,
            budget,
            &provenant_core::CancelToken::new(),
            &clock,
        )
        .map_err(|error| error.to_string())
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
    if matches!(&left.scope, SeedScope::Full) || matches!(&right.scope, SeedScope::Full) {
        // Full work reevaluates every project, so stale retry errors must not
        // survive as if the full pass had failed.
        return SeedRequest::full();
    }
    let (SeedScope::Busy(left_keys), SeedScope::Busy(right_keys)) = (left.scope, right.scope)
    else {
        unreachable!("full scopes returned above");
    };
    let mut keys = BTreeSet::new();
    keys.extend(left_keys);
    keys.extend(right_keys);
    let omitted_keys = keys.len().saturating_sub(MAX_BUSY_SEED_PROJECTS);
    let mut project_keys = Vec::new();
    for key in keys {
        if project_keys.len() == MAX_BUSY_SEED_PROJECTS {
            break;
        }
        project_keys.push(key);
    }
    let mut prior_error = match (left.prior_error, right.prior_error) {
        (Some(left), Some(right)) if left == right => Some(left),
        (Some(left), Some(right)) => Some(join_seed_errors(Some(&left), &right)),
        (Some(error), None) | (None, Some(error)) => Some(error),
        (None, None) => None,
    };
    if omitted_keys > 0 {
        prior_error = Some(join_seed_errors(
            prior_error.as_deref(),
            "more than 512 busy seed projects are pending; excess projects wait for the next full pass",
        ));
    }
    SeedRequest::busy(project_keys, prior_error)
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
        let Some(pending) = queue.pending.as_ref() else {
            write_seed_queue(directory, &queue)?;
            drop(worker_lock);
            return Ok(());
        };
        if queue.launch_at.is_some_and(|at| {
            now.as_micros().saturating_sub(at.as_micros()) < SEED_WORK_RETRY_MICROS
        }) {
            write_seed_queue(directory, &queue)?;
            drop(worker_lock);
            return Ok(());
        }
        let error = pending.prior_error.clone();
        queue.pending_at.get_or_insert(now);
        queue.launch_at = Some(now);
        write_seed_queue(directory, &queue)?;
        write_result(
            directory,
            SEED_RESULT,
            "tick-seed",
            now,
            "queued",
            error.clone(),
        )?;
        // Keep the queue lock while releasing the probe lock and spawning:
        // the child waits for the queue lock before claiming the request.
        drop(worker_lock);
        if let Err(spawn_error) = spawn() {
            let error = join_seed_errors(error.as_deref(), &spawn_error);
            write_result(
                directory,
                SEED_RESULT,
                "tick-seed",
                now,
                "failed",
                Some(error.clone()),
            )?;
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

fn join_seed_errors(previous: Option<&str>, next: &str) -> String {
    truncate_bytes(
        match previous.filter(|error| !error.is_empty()) {
            Some(previous) => format!("{previous}; {next}"),
            None => next.to_owned(),
        },
        MAX_SEED_RESULT_ERROR_BYTES,
    )
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

/// What one pass over the registered projects did.
#[derive(Debug, Default, PartialEq, Eq)]
struct SeedPass {
    built: usize,
    current: usize,
    busy: usize,
    /// Projects with no selected Cargo manifest, which have nothing to seed.
    skipped: usize,
    errors: Vec<String>,
    busy_project_keys: Vec<String>,
}

#[derive(Debug)]
struct SeedOutcome {
    error: Option<String>,
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
    message: String,
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

    fn record_error(&mut self, project_key: String, message: String) {
        if self.errors.len() < MAX_SEED_DISCOVERY_ERRORS {
            let message = truncate_seed_error(message);
            self.errors.push(SeedDiscoveryError {
                project_key,
                message,
            });
        } else {
            self.omitted_errors += 1;
        }
    }
}

fn truncate_seed_error(message: String) -> String {
    if message.len() <= MAX_SEED_ERROR_BYTES {
        return message;
    }
    let mut end = MAX_SEED_ERROR_BYTES;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &message[..end])
}

/// Refresh each project in turn. A busy build is a no-op and one project's
/// failure never stops the others.
fn refresh_projects(
    projects: &[SeedProject],
    host_state: &std::path::Path,
    refresh: impl Fn(
        &std::path::Path,
    ) -> Result<crate::target_seed_build::Refresh, Box<dyn std::error::Error>>,
) -> SeedPass {
    use crate::target_seed_build::Refresh;
    let mut pass = SeedPass::default();
    for project in projects {
        match provenant_run::target_seed::cargo_build_directory(&project.root, host_state) {
            Ok(Some(_)) => {}
            Ok(None) => {
                pass.skipped += 1;
                continue;
            }
            Err(error) => {
                pass.errors
                    .push(format!("{}: {error}", project.root.display()));
                continue;
            }
        }
        match refresh(&project.root) {
            Ok(Refresh::Built(_)) => pass.built += 1,
            Ok(Refresh::NotCargo) => pass.skipped += 1,
            Ok(Refresh::Current) => pass.current += 1,
            Ok(Refresh::Busy) => {
                pass.busy += 1;
                if pass.busy_project_keys.len() < MAX_BUSY_SEED_PROJECTS {
                    pass.busy_project_keys.push(project.project_key.clone());
                } else if pass.busy == MAX_BUSY_SEED_PROJECTS + 1 {
                    pass.errors.push(format!(
                        "more than {MAX_BUSY_SEED_PROJECTS} project seeds are busy; excess projects wait for the next daily refresh"
                    ));
                }
            }
            Err(error) => pass
                .errors
                .push(format!("{}: {error}", project.root.display())),
        }
    }
    pass
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
    let prior_error = request.prior_error.clone();
    if !enabled {
        write_result(directory, SEED_RESULT, "tick-seed", at()?, "disabled", None)?;
        return Ok(SeedOutcome {
            error: None,
            busy_project_keys: Vec::new(),
        });
    }
    write_result(
        directory,
        SEED_RESULT,
        "tick-seed",
        at()?,
        "running",
        prior_error.clone(),
    )?;
    let outcome = projects().map(|mut discovery| {
        if !retry_project_keys.is_empty() {
            discovery
                .projects
                .retain(|project| retry_project_keys.contains(&project.project_key));
            discovery
                .errors
                .retain(|error| retry_project_keys.contains(&error.project_key));
        }
        let mut pass = refresh_projects(&discovery.projects, host_state, refresh);
        pass.errors
            .extend(discovery.errors.into_iter().map(|error| error.message));
        if discovery.omitted_errors > 0 {
            pass.errors.push(format!(
                "{} additional registered project layout error(s) omitted",
                discovery.omitted_errors
            ));
        }
        pass
    });
    let (state, error, busy_project_keys) = match outcome {
        Ok(pass) => {
            let mut error = prior_error;
            for message in &pass.errors {
                error = Some(join_seed_errors(error.as_deref(), message));
            }
            let has_failure = error.is_some();
            let state = if has_failure {
                "failed"
            } else if pass.busy > 0 {
                "busy"
            } else {
                "complete"
            };
            (state, error, pass.busy_project_keys)
        }
        Err(error) => (
            "failed",
            Some(join_seed_errors(prior_error.as_deref(), &error)),
            Vec::new(),
        ),
    };
    write_result(
        directory,
        SEED_RESULT,
        "tick-seed",
        at()?,
        state,
        error.clone(),
    )?;
    Ok(SeedOutcome {
        error,
        busy_project_keys,
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
    write_result(
        directory,
        SEED_RESULT,
        "tick-seed",
        now,
        "running",
        request.prior_error.clone(),
    )?;
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
        let retry = SeedRequest::busy(outcome.busy_project_keys, outcome.error);
        match queue.pending.take() {
            Some(pending) if matches!(&pending.scope, SeedScope::Full) => {
                // The full pass covers this busy result and supersedes its
                // errors, which will be recomputed by that pass.
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
        write_result(
            directory,
            SEED_RESULT,
            "tick-seed",
            now,
            "running",
            next.prior_error.clone(),
        )?;
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

/// The last seed pass, for the tick's report. Failures stay visible; full
/// collection retries them daily, while scoped Busy work retries on the tick.
#[cfg(test)]
fn seed_state(directory: &Directory) -> &'static str {
    match directory.read_bounded(SEED_RESULT.as_ref(), 64 * 1024) {
        Ok(bytes) => match serde_json::from_slice::<GcResult>(&bytes) {
            Ok(result) => seed_result_state(&result.state),
            Err(_) => "last_failed",
        },
        Err(error) if error.is_not_found() => "idle",
        Err(_) => "last_failed",
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

fn target_seed_task(directory: &Directory) -> Task {
    match directory.read_bounded(SEED_RESULT.as_ref(), 64 * 1024) {
        Ok(bytes) => match serde_json::from_slice::<GcResult>(&bytes) {
            Ok(result) => {
                let state = seed_result_state(&result.state);
                if state == "last_failed" {
                    Task::idle_with_diagnostic(
                        "target_seed",
                        state,
                        safe_seed_failure_diagnostic(result.error.as_deref()),
                    )
                } else {
                    Task::idle("target_seed", state)
                }
            }
            Err(_) => Task::idle_with_diagnostic(
                "target_seed",
                "last_failed",
                "The local seed result could not be read. Run `provenant worktree seed-build` in the affected repository for current details.",
            ),
        },
        Err(error) if error.is_not_found() => Task::idle("target_seed", "idle"),
        Err(_) => Task::idle_with_diagnostic(
            "target_seed",
            "last_failed",
            "The local seed result could not be read. Run `provenant worktree seed-build` in the affected repository for current details.",
        ),
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
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diagnostic: Option<&'static str>,
}

impl Task {
    fn run(task: &'static str, action: impl FnOnce() -> Result<&'static str, String>) -> Self {
        match action() {
            Ok(state) => Self {
                task,
                state,
                error: None,
                diagnostic: None,
            },
            Err(error) => Self {
                task,
                state: "failed",
                error: Some(error),
                diagnostic: None,
            },
        }
    }
    fn idle(task: &'static str, state: &'static str) -> Self {
        Self {
            task,
            state,
            error: None,
            diagnostic: None,
        }
    }
    fn idle_with_diagnostic(
        task: &'static str,
        state: &'static str,
        diagnostic: &'static str,
    ) -> Self {
        Self {
            task,
            state,
            error: None,
            diagnostic: Some(diagnostic),
        }
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
        Task::run("gc", || -> Result<&'static str, String> {
            let directory = provenant_config::private_directory(
                &resolved.roots.state.as_path().join("tick"),
                true,
            )
            .map_err(|error| error.to_string())?;
            let _lock = match directory.try_lock("cadence.lock".as_ref(), LockMode::Exclusive) {
                Ok(lock) => lock,
                Err(provenant_os::fs::FsError::LockTimeout { .. }) => {
                    return Ok("busy; next tick retries");
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

    #[test]
    fn daily_cadence_survives_restart_and_fires_at_the_deadline() {
        let temp = provenant_os::fixture::tempdir(|root| tempfile::tempdir_in(root)).unwrap();
        let directory = provenant_config::private_directory(temp.path(), true).unwrap();
        let now = WallTime::from_micros(1_000_000).unwrap();
        let cadence = read_cadence(&directory).unwrap();
        assert!(gc_due(&cadence, now));
        assert!(queue_gc(&directory, now, || Err("spawn failed".into())).is_err());
        assert!(
            gc_due(&read_cadence(&directory).unwrap(), now),
            "spawn failure must remain retryable"
        );
        assert_eq!(queue_gc(&directory, now, || Ok(())).unwrap(), "queued");
        let deadline = now
            .checked_add(DurationMicros::from_seconds(DAY_SECONDS).unwrap())
            .unwrap();
        let reopened = Directory::open_anchor(temp.path()).unwrap();
        let restored = read_cadence(&reopened).unwrap();
        assert!(!gc_due(&restored, now));
        assert!(gc_due(&restored, deadline));
        assert_eq!(
            queue_gc(&reopened, now, || Err("duplicate launch".into())).unwrap(),
            "queued"
        );
        write_gc_result(&reopened, now, "running", None).unwrap();
        let worker = reopened
            .try_lock("gc-worker.lock".as_ref(), LockMode::Exclusive)
            .unwrap();
        assert_eq!(
            queue_gc(&reopened, now, || Err("duplicate launch".into())).unwrap(),
            "running"
        );
        drop(worker);
        assert!(
            queue_gc(&reopened, now, || Err("duplicate launch".into()))
                .unwrap_err()
                .contains("interrupted")
        );
        write_gc_result(&reopened, now, "failed", Some("collection failed".into())).unwrap();
        assert_eq!(
            queue_gc(&reopened, now, || Err("duplicate launch".into())).unwrap_err(),
            "collection failed"
        );
        assert_eq!(queue_gc(&reopened, deadline, || Ok(())).unwrap(), "queued");
    }

    #[allow(dead_code)] // The worktree driver includes this module without libtest.
    fn seed_clock() -> provenant_os::clock::SystemClock {
        provenant_os::clock::SystemClock
    }

    #[allow(dead_code)]
    fn seed_state_of(directory: &Directory) -> (&'static str, Option<String>) {
        let bytes = directory
            .read_bounded(SEED_RESULT.as_ref(), 64 * 1024)
            .unwrap();
        let result: GcResult = serde_json::from_slice(&bytes).unwrap();
        (seed_state(directory), result.error)
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn seed_failure_report_projects_a_safe_cause_and_action() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let directory = provenant_config::private_directory(&root, true).unwrap();
        write_result(
            &directory,
            SEED_RESULT,
            "tick-seed",
            WallTime::from_micros(1_000_000).unwrap(),
            "failed",
            Some(
                "/private/sensitive/repo: command failed: [\"cargo\", \"--token\", \"do-not-print\"] API_KEY=secret-value".into(),
            ),
        )
        .unwrap();

        let task = target_seed_task(&directory);
        assert_eq!(task.state, "last_failed");
        assert!(task.error.is_none(), "a seed note must not fail the tick");
        let diagnostic = task.diagnostic.unwrap();
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
            ("last_failed", Some("no process budget".into()))
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
            retry: Some(SeedRequest::busy(
                vec!["project-a".into()],
                Some("project-b failed previously".into()),
            )),
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
            "full scope must dominate the stale busy snapshot and clear its error"
        );
        let (_, request) = claim_seed_job(&directory, now).unwrap().unwrap();
        assert_eq!(request, SeedRequest::full());
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn a_full_request_queued_during_busy_work_clears_the_busy_error_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let directory = provenant_config::private_directory(&root, true).unwrap();
        let now = WallTime::from_micros(1_500_000).unwrap();
        let queue = SeedQueue {
            active: Some(SeedRequest::busy(
                vec!["project-a".into()],
                Some("project-b failed".into()),
            )),
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
                error: Some("project-b failed".into()),
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
    fn stale_busy_job_recovery_keeps_its_project_keys_and_prior_error() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let directory = provenant_config::private_directory(&root, true).unwrap();
        let started = WallTime::from_micros(2_000_000).unwrap();
        queue_seed(
            &directory,
            started,
            Some(SeedRequest::busy(
                vec!["project-a".into()],
                Some("project-b failed".into()),
            )),
            || Ok(()),
        )
        .unwrap();
        let (worker, request) = claim_seed_job(&directory, started).unwrap().unwrap();
        assert_eq!(
            request,
            SeedRequest::busy(vec!["project-a".into()], Some("project-b failed".into()))
        );
        drop(worker); // Simulate a crash after the request became active.

        let stale =
            WallTime::from_micros(started.as_micros() + SEED_WORK_RETRY_MICROS + 1).unwrap();
        assert!(seed_retry_due(&directory, stale));
        queue_seed(&directory, stale, None, || Ok(())).unwrap();
        let queue = read_seed_queue(&directory).unwrap();
        assert_eq!(queue.active, None);
        assert_eq!(
            queue.pending,
            Some(SeedRequest::busy(
                vec!["project-a".into()],
                Some("project-b failed".into())
            ))
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
        write_result(&directory, SEED_RESULT, "tick-seed", now, "running", None).unwrap();
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
                    SeedOutcome {
                        error: None,
                        busy_project_keys: Vec::new(),
                    },
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
    fn seed_retry_retries_only_busy_projects_and_preserves_other_failures() {
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
        assert!(
            finish_seed_job(&directory, wall, &mut worker, outcome)
                .unwrap()
                .is_none()
        );
        // The failure is logged in the result and visible to the tick, which
        // is not itself failed: the later projects still ran.
        let (state, error) = seed_state_of(&directory);
        assert_eq!(state, "last_failed");
        assert!(
            error
                .as_deref()
                .unwrap()
                .contains(&format!("{}: origin/main missing", projects[1].display()))
        );
        let pass = refresh_projects(&entries, &root, |project| {
            match project.file_name().unwrap().to_str().unwrap() {
                "a" => Ok(Refresh::Busy),
                "b" => Err("origin/main missing".into()),
                "c" => Ok(Refresh::Built("generation".into())),
                _ => Ok(Refresh::Current),
            }
        });
        assert_eq!(
            (
                pass.built,
                pass.current,
                pass.busy,
                pass.errors.len(),
                pass.busy_project_keys
            ),
            (1, 1, 1, 1, vec![busy_key.clone()])
        );
        assert!(!seed_retry_due(&directory, wall), "just written: not due");
        let far = WallTime::from_micros(wall.as_micros() + SEED_WORK_RETRY_MICROS + 1).unwrap();
        assert!(seed_retry_due(&directory, far), "busy is retried next tick");
        let retry = read_seed_queue(&directory).unwrap().retry.unwrap();
        assert_eq!(retry.scope, SeedScope::Busy(vec![busy_key.clone()]));
        let previous_error = retry.prior_error.unwrap();
        assert!(previous_error.contains("origin/main missing"));
        queue_seed(&directory, far, None, || Ok(())).unwrap();
        let queued = read_seed_queue(&directory).unwrap();
        let request = queued.pending.unwrap();
        assert_eq!(request.scope, SeedScope::Busy(vec![busy_key.clone()]));
        assert_eq!(request.prior_error, Some(previous_error.clone()));
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
        let (state, error) = seed_state_of(&directory);
        assert_eq!(state, "last_failed", "the failed project remains visible");
        assert!(error.unwrap().contains("origin/main missing"));
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
        assert_eq!(seed_state_of(&directory), ("complete", None));
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
        let (state, note) = seed_state_of(&directory);
        assert_eq!(state, "last_failed");
        assert!(note.unwrap().contains("temporary Git spawn failure"));
        let pass = refresh_projects(
            &SeedDiscovery::from_paths(vec![bare]).projects,
            &root,
            |_| panic!("no manifest, no refresh"),
        );
        assert_eq!(
            pass,
            SeedPass {
                skipped: 1,
                ..Default::default()
            }
        );
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
        assert_eq!(seed_state_of(&directory), ("complete", None));
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
            ("last_failed", Some("registry unreadable".into()))
        );
    }
}
