//! Scheduled host work. Deadlines live on disk; this process owns no timers.
use provenant_cli::Globals;
use provenant_confinement::worktrees::WorkerObserver;
use provenant_core::{Clock, DurationMicros, WaitBudget, WallTime};
use provenant_os::{
    clock::SystemClock,
    exec::{Executor, SystemExecutor},
    fs::{Directory, LockMode, PublishMode},
};
use provenant_records::{CommandResult, Outcome};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};

mod steward;

const DAY_SECONDS: u64 = 86_400;
const MAX_SEED_DISCOVERY_ERRORS: usize = 16;
const MAX_SEED_ERROR_BYTES: usize = 1024;
const MAX_BUSY_SEED_PROJECTS: usize = 512;
const MAX_SEED_RESULT_ERROR_BYTES: usize = 16 * 1024;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    busy_retry_at: Option<WallTime>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    busy_project_keys: Vec<String>,
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
    write_result_with_retry(directory, file, purpose, at, state, error, None, Vec::new())
}

fn write_seed_result(
    directory: &Directory,
    at: WallTime,
    state: &str,
    error: Option<String>,
    busy_retry_at: Option<WallTime>,
    busy_project_keys: Vec<String>,
) -> Result<(), String> {
    write_result_with_retry(
        directory,
        SEED_RESULT,
        "tick-seed",
        at,
        state,
        error,
        busy_retry_at,
        busy_project_keys,
    )
}

fn write_result_with_retry(
    directory: &Directory,
    file: &str,
    purpose: &str,
    at: WallTime,
    state: &str,
    error: Option<String>,
    busy_retry_at: Option<WallTime>,
    busy_project_keys: Vec<String>,
) -> Result<(), String> {
    let result = GcResult {
        extra: Default::default(),
        state: state.into(),
        at,
        error,
        busy_retry_at,
        busy_project_keys,
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
                        && now.as_micros().saturating_sub(result.at.as_micros()) >= 60_000_000
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
            crate::worktrees::scheduled_gc(globals, config.gc.idle_days, workers)
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
        let _ = queue_seed(&directory, clock.wall_now()?, &[], None, || {
            spawn_seed_worker(resolved.roots.state.as_path(), &resolved.home)
        });
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

/// Queue the detached seed worker. Workers are single-flight on
/// `seed-worker.lock`, so a second queue while one runs is a no-op there.
fn queue_seed(
    directory: &Directory,
    now: WallTime,
    retry_project_keys: &[String],
    prior_error: Option<String>,
    spawn: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    write_seed_result(
        directory,
        now,
        "queued",
        prior_error.clone(),
        None,
        retry_project_keys.to_vec(),
    )?;
    if let Err(error) = spawn() {
        let error = join_seed_errors(prior_error.as_deref(), &error);
        write_seed_result(
            directory,
            now,
            "failed",
            Some(error.clone()),
            (!retry_project_keys.is_empty()).then_some(now),
            retry_project_keys.to_vec(),
        )?;
        return Err(error);
    }
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

fn read_seed_retry_context(directory: &Directory) -> (Vec<String>, Option<String>) {
    let result = directory
        .read_bounded(SEED_RESULT.as_ref(), 64 * 1024)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<GcResult>(&bytes).ok());
    result.map_or_else(
        || (Vec::new(), None),
        |result| {
            if result.busy_project_keys.is_empty() {
                (Vec::new(), None)
            } else {
                (result.busy_project_keys, result.error)
            }
        },
    )
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

/// A pass that found a build already running records those projects for a
/// minute retry. Other project failures stay visible for the next daily pass.
fn seed_retry_due(directory: &Directory, now: WallTime) -> bool {
    directory
        .read_bounded(SEED_RESULT.as_ref(), 64 * 1024)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<GcResult>(&bytes).ok())
        .is_some_and(|result| {
            !result.busy_project_keys.is_empty()
                && result
                    .busy_retry_at
                    .is_some_and(|at| now.as_micros().saturating_sub(at.as_micros()) >= 60_000_000)
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
fn seed_pass(
    directory: &Directory,
    host_state: &Path,
    clock: &dyn Clock,
    enabled: bool,
    retry_project_keys: &[String],
    prior_error: Option<String>,
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
    let at = || clock.wall_now().map_err(|error| error.to_string());
    if !enabled {
        return write_result(directory, SEED_RESULT, "tick-seed", at()?, "disabled", None);
    }
    write_seed_result(
        directory,
        at()?,
        "running",
        prior_error.clone(),
        None,
        retry_project_keys.to_vec(),
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
    let (state, error, busy_retry_at, busy_project_keys) = match outcome {
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
            (
                state,
                error,
                if pass.busy > 0 { Some(at()?) } else { None },
                pass.busy_project_keys,
            )
        }
        Err(error) => (
            "failed",
            Some(join_seed_errors(prior_error.as_deref(), &error)),
            None,
            Vec::new(),
        ),
    };
    write_seed_result(
        directory,
        at()?,
        state,
        error,
        busy_retry_at,
        busy_project_keys,
    )
}

fn seed_git_environment() -> Vec<(OsString, OsString)> {
    [
        ("HOME", "/nonexistent"),
        ("XDG_CONFIG_HOME", "/nonexistent"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_SYSTEM", "/dev/null"),
        ("GIT_NO_REPLACE_OBJECTS", "1"),
        ("GIT_NO_LAZY_FETCH", "1"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("LC_ALL", "C"),
    ]
    .map(|(key, value)| (key.into(), value.into()))
    .into()
}

fn registered_seed_projects(
    common_dirs: &[PathBuf],
    retry_project_keys: &[String],
    executor: &dyn Executor,
    git: &Path,
    environment: &[(OsString, OsString)],
    cancel: &provenant_core::CancelToken,
) -> SeedDiscovery {
    let mut discovery = SeedDiscovery::default();
    let mut seen = BTreeSet::new();
    for common in common_dirs {
        let project_key = provenant_run::target_seed::project_key(common);
        // Busy retries are scoped to the projects that actually had a seed
        // lock. Do not re-inventory other registrations, which may include a
        // separate project whose earlier discovery/build failed.
        if !retry_project_keys.is_empty() && !retry_project_keys.contains(&project_key) {
            continue;
        }
        let layout = match provenant_confinement::worktrees::project_layout_for_common(
            executor,
            common,
            git,
            environment,
            Duration::from_secs(5),
            cancel,
        ) {
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
        let (retry_project_keys, prior_error) = read_seed_retry_context(&directory);
        let config = provenant_cli::config_reads::read_for(&resolved.roots, "tick")?;
        let state = resolved.roots.state.as_path();
        seed_pass(
            &directory,
            state,
            &SystemClock,
            config.domains.fabric.lanes.cargo_seed.enabled,
            &retry_project_keys,
            prior_error,
            || {
                let selection = provenant_config::registered_gc_project_selection(&resolved.roots)
                    .map_err(|error| error.to_string())?;
                let common_dirs = selection
                    .projects
                    .iter()
                    .map(|binding| binding.common_dir().to_path_buf())
                    .collect::<Vec<_>>();
                let git = provenant_os::git::git_executable()
                    .ok_or_else(|| "Git executable unavailable".to_owned())?;
                let executor = SystemExecutor::new(1).map_err(|error| error.to_string())?;
                Ok(registered_seed_projects(
                    &common_dirs,
                    &retry_project_keys,
                    &executor,
                    &git,
                    &seed_git_environment(),
                    &provenant_core::CancelToken::new(),
                ))
            },
            |project| crate::target_seed_build::refresh(state, project),
        )?;
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

/// The last seed pass, for the tick's report. A failed pass is visible here
/// but is not a tick failure: the next daily collection retries it.
fn seed_state(directory: &Directory) -> &'static str {
    match directory.read_bounded(SEED_RESULT.as_ref(), 64 * 1024) {
        Ok(bytes) => match serde_json::from_slice::<GcResult>(&bytes) {
            Ok(result) => match result.state.as_str() {
                "queued" => "queued",
                "running" => "running",
                "complete" => "complete",
                "busy" => "busy",
                "disabled" => "disabled",
                _ => "last_failed",
            },
            Err(_) => "last_failed",
        },
        Err(error) if error.is_not_found() => "idle",
        Err(_) => "last_failed",
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
}

impl Task {
    fn run(task: &'static str, action: impl FnOnce() -> Result<&'static str, String>) -> Self {
        match action() {
            Ok(state) => Self {
                task,
                state,
                error: None,
            },
            Err(error) => Self {
                task,
                state: "failed",
                error: Some(error),
            },
        }
    }
    fn idle(task: &'static str, state: &'static str) -> Self {
        Self {
            task,
            state,
            error: None,
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
    // These are the integration seams for #2125 and #2225. They deliberately
    // have no cadence, network probes or policy until their owners wire them.
    let target_seed = match provenant_config::private_directory(
        &resolved.roots.state.as_path().join("tick"),
        true,
    ) {
        Ok(directory) => {
            // A busy pass is retried here; the worker is single-flight, so a
            // retry that finds a build still running costs one short process.
            if seed_retry_due(&directory, now) {
                let (retry_project_keys, prior_error) = read_seed_retry_context(&directory);
                if queue_seed(&directory, now, &retry_project_keys, prior_error, || {
                    spawn_seed_worker(resolved.roots.state.as_path(), &resolved.home)
                })
                .is_err()
                {
                    Task::idle("target_seed", "last_failed")
                } else {
                    Task::idle("target_seed", seed_state(&directory))
                }
            } else {
                Task::idle("target_seed", seed_state(&directory))
            }
        }
        Err(_) => Task::idle("target_seed", "last_failed"),
    };
    let tasks = vec![
        gc,
        target_seed,
        steward::run(&config.domains.steward, &resolved, workers),
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
                "tick: graphs={} errors={} gc={} seed={} steward={}\n",
                data.graphs.advanced,
                data.graphs.errors.len(),
                data.tasks[0].state,
                data.tasks[1].state,
                data.tasks[2].state
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

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn registered_seed_projects_resolve_separate_gitdirs_and_skip_bare_repositories() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
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
        let primary_common = primary.join(".git");
        let discovery = registered_seed_projects(
            &[
                primary_common.clone(),
                invalid.clone(),
                separate_git.clone(),
                bare,
            ],
            &[],
            &executor,
            &git,
            &seed_git_environment(),
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
            &[primary_common, invalid.clone(), separate_git],
            &[primary_key.clone()],
            &executor,
            &git,
            &seed_git_environment(),
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

    #[test]
    fn seed_refresh_is_queued_without_running_inline_and_a_spawn_failure_is_recorded() {
        let temp = tempfile::tempdir().unwrap();
        // macOS temp roots sit behind a /var symlink, which private_directory refuses.
        let root = temp.path().canonicalize().unwrap();
        let directory = provenant_config::private_directory(&root, true).unwrap();
        let now = WallTime::from_micros(1_000_000).unwrap();
        assert_eq!(seed_state(&directory), "idle");
        let mut spawned = 0;
        queue_seed(&directory, now, &[], None, || {
            spawned += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(spawned, 1);
        assert_eq!(seed_state_of(&directory).0, "queued");
        // Queueing never builds: the refresh closure is only reachable from
        // the detached worker.
        let error = queue_seed(&directory, now, &[], None, || {
            Err("no process budget".into())
        })
        .unwrap_err();
        assert_eq!(error, "no process budget");
        assert_eq!(
            seed_state_of(&directory),
            ("last_failed", Some("no process budget".into()))
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
        seed_pass(
            &directory,
            &root,
            &seed_clock(),
            true,
            &[],
            None,
            || Ok(SeedDiscovery::from_paths(projects.clone())),
            |project| match project.file_name().unwrap().to_str().unwrap() {
                "a" => Ok(Refresh::Busy),
                "b" => Err("origin/main missing".into()),
                "c" => Ok(Refresh::Built("generation".into())),
                _ => Ok(Refresh::Current),
            },
        )
        .unwrap();
        // The failure is logged in the result and visible to the tick, which
        // is not itself failed: the later projects still ran.
        let (state, error) = seed_state_of(&directory);
        assert_eq!(state, "last_failed");
        assert!(
            error
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
        let wall = seed_clock().wall_now().unwrap();
        assert!(!seed_retry_due(&directory, wall), "just written: not due");
        let far = WallTime::from_micros(wall.as_micros() + 61_000_000).unwrap();
        assert!(seed_retry_due(&directory, far), "busy is retried next tick");
        let (retry_keys, prior_error) = read_seed_retry_context(&directory);
        assert_eq!(retry_keys, vec![busy_key]);
        assert!(
            prior_error
                .as_deref()
                .is_some_and(|error| error.contains("origin/main missing"))
        );
        let retried = std::cell::RefCell::new(Vec::new());
        seed_pass(
            &directory,
            &root,
            &seed_clock(),
            true,
            &retry_keys,
            prior_error,
            || Ok(SeedDiscovery::from_paths(projects.clone())),
            |project| {
                retried.borrow_mut().push(project.to_owned());
                assert_eq!(project.file_name().unwrap(), "a");
                Ok(Refresh::Current)
            },
        )
        .unwrap();
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
            &[],
            None,
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
            &[],
            None,
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
            &[],
            None,
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
            &[],
            None,
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
                    &[],
                    None,
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
            &[],
            None,
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
