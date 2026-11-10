//! Scheduled host work. Deadlines live on disk; this process owns no timers.
use provenant_cli::Globals;
use provenant_confinement::worktrees::WorkerObserver;
use provenant_core::{CancelToken, Clock, DurationMicros, WaitBudget, WallTime};
use provenant_os::{
    clock::SystemClock,
    fs::{Directory, LockMode, PublishMode},
};
use provenant_records::{CommandResult, Outcome};
use serde::{Deserialize, Serialize};

mod steward;

const DAY_SECONDS: u64 = 86_400;

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

/// Rebuild pending markers and collect old, terminal graph checkpoints using
/// the same cutoff for manual and scheduled collection.
pub(crate) fn collect_graph_checkpoints(
    state: &provenant_core::StateRoot,
    projects: impl IntoIterator<Item = provenant_core::ProjectId>,
    retention_days: u64,
    dry_run: bool,
    clock: &dyn Clock,
) -> Result<usize, String> {
    if dry_run {
        return Ok(0);
    }
    let mut removed = 0usize;
    for project in projects {
        provenant_fabric_runtime::graph::FileJournal::rebuild_pending(state, project)
            .map_err(|error| error.to_string())?;
        let before_ns = i128::from(
            clock
                .wall_now()
                .map_err(|error| error.to_string())?
                .as_micros(),
        ) * 1_000
            - i128::from(retention_days) * 86_400 * 1_000_000_000;
        removed += provenant_fabric_runtime::graph::FileJournal::collect_terminal(
            state,
            project,
            before_ns,
            &CancelToken::new(),
            clock,
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(removed)
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
            let config = provenant_config::read_with_domains(
                &resolved.roots,
                &provenant_cli::domains::SECTIONS,
            )
            .map_err(|error| error.to_string())?;
            collect_graph_checkpoints(
                &resolved.roots.state,
                provenant_config::registered_projects(&resolved.roots)
                    .map_err(|error| error.to_string())?
                    .into_iter()
                    .map(|binding| binding.project_id()),
                config.gc.graph_checkpoint_days,
                false,
                &clock,
            )?;
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
        let _ = queue_seed(&directory, clock.wall_now()?, || {
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
    spawn: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    write_result(directory, SEED_RESULT, "tick-seed", now, "queued", None)?;
    if let Err(error) = spawn() {
        write_result(
            directory,
            SEED_RESULT,
            "tick-seed",
            now,
            "failed",
            Some(error.clone()),
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

/// A pass that found a build already running ends `busy`; the tick retries it
/// once a minute until a pass finds every project current or built.
fn seed_retry_due(directory: &Directory, now: WallTime) -> bool {
    directory
        .read_bounded(SEED_RESULT.as_ref(), 64 * 1024)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<GcResult>(&bytes).ok())
        .is_some_and(|result| {
            result.state == "busy"
                && now.as_micros().saturating_sub(result.at.as_micros()) >= 60_000_000
        })
}

/// What one pass over the registered projects did.
#[derive(Debug, Default, PartialEq, Eq)]
struct SeedPass {
    built: usize,
    current: usize,
    busy: usize,
    /// Projects with no front-door manifest, which have nothing to seed.
    skipped: usize,
    /// Commits whose last build failed, waiting for `origin/main` to move.
    backed_off: Vec<String>,
    errors: Vec<String>,
}

/// What `target_seed_build::build` requires of a checkout.
const SEED_MANIFEST: &str = "tools/front-door/Cargo.lock";

/// Refresh each project in turn. A busy build is a no-op and one project's
/// failure never stops the others.
fn refresh_projects(
    projects: &[std::path::PathBuf],
    refresh: impl Fn(
        &std::path::Path,
    ) -> Result<crate::target_seed_build::Refresh, Box<dyn std::error::Error>>,
) -> SeedPass {
    use crate::target_seed_build::Refresh;
    let mut pass = SeedPass::default();
    for project in projects {
        if !project.join(SEED_MANIFEST).is_file() {
            pass.skipped += 1;
            continue;
        }
        match refresh(project) {
            Ok(Refresh::Built(_)) => pass.built += 1,
            Ok(Refresh::Current) => pass.current += 1,
            Ok(Refresh::BackedOff(commit)) => pass.backed_off.push(commit),
            Ok(Refresh::Busy) => pass.busy += 1,
            Err(error) => pass.errors.push(format!("{}: {error}", project.display())),
        }
    }
    pass
}

fn backed_off_note(commits: &[String]) -> Option<String> {
    let mut commits = commits.to_vec();
    commits.sort();
    commits.dedup();
    (!commits.is_empty()).then(|| {
        format!(
            "commit {} failed; waiting for origin/main to move",
            commits.join(", ")
        )
    })
}

/// Run one single-flight pass under `seed-worker.lock`. A second worker, or a
/// disabled seed, returns without touching the result file.
fn seed_pass(
    directory: &Directory,
    clock: &dyn Clock,
    enabled: bool,
    projects: impl FnOnce() -> Result<Vec<std::path::PathBuf>, String>,
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
    write_result(directory, SEED_RESULT, "tick-seed", at()?, "running", None)?;
    let outcome = projects().map(|projects| refresh_projects(&projects, refresh));
    let (state, error) = match outcome {
        Ok(pass) if !pass.errors.is_empty() => ("failed", Some(pass.errors.join("; "))),
        Ok(pass) if pass.busy > 0 => (
            "busy",
            Some(format!(
                "{} project(s) had a build running; retrying next tick{}",
                pass.busy,
                backed_off_note(&pass.backed_off)
                    .map(|note| format!("; {note}"))
                    .unwrap_or_default()
            )),
        ),
        Ok(pass) if !pass.backed_off.is_empty() => {
            ("backed_off", backed_off_note(&pass.backed_off))
        }
        Ok(_) => ("complete", None),
        Err(error) => ("failed", Some(error)),
    };
    write_result(directory, SEED_RESULT, "tick-seed", at()?, state, error)
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
        let config = provenant_config::read_with_domains(
            &resolved.roots,
            &provenant_cli::domains::SECTIONS,
        )?;
        let state = resolved.roots.state.as_path();
        seed_pass(
            &directory,
            &SystemClock,
            config.domains.fabric.lanes.cargo_seed.enabled,
            || {
                let selection = provenant_config::registered_gc_project_selection(&resolved.roots)
                    .map_err(|error| error.to_string())?;
                Ok(selection
                    .projects
                    .iter()
                    .map(|binding| {
                        let common = binding.common_dir();
                        if common.file_name() == Some(std::ffi::OsStr::new(".git")) {
                            common.parent().unwrap_or(common).to_owned()
                        } else {
                            common.to_owned()
                        }
                    })
                    .collect())
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
                "backed_off" => "backed_off",
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
    let config =
        provenant_config::read_with_domains(&resolved.roots, &provenant_cli::domains::SECTIONS)?;
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
            let cancel = CancelToken::new();
            let budget = WaitBudget::cli(clock.mono_now(), 1).map_err(|error| error.to_string())?;
            let _lock = directory
                .lock(
                    "cadence.lock".as_ref(),
                    LockMode::Exclusive,
                    budget,
                    &cancel,
                    &clock,
                )
                .map_err(|error| error.to_string())?;
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
            if seed_retry_due(&directory, now)
                && queue_seed(&directory, now, || {
                    spawn_seed_worker(resolved.roots.state.as_path(), &resolved.home)
                })
                .is_err()
            {
                Task::idle("target_seed", "last_failed")
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
        queue_seed(&directory, now, || {
            spawned += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(spawned, 1);
        assert_eq!(seed_state_of(&directory).0, "queued");
        // Queueing never builds: the refresh closure is only reachable from
        // the detached worker.
        let error = queue_seed(&directory, now, || Err("no process budget".into())).unwrap_err();
        assert_eq!(error, "no process budget");
        assert_eq!(
            seed_state_of(&directory),
            ("last_failed", Some("no process budget".into()))
        );
    }

    #[test]
    fn seed_pass_treats_busy_as_a_noop_and_isolates_project_failures() {
        use crate::target_seed_build::Refresh;
        let temp = tempfile::tempdir().unwrap();
        // macOS temp roots sit behind a /var symlink, which private_directory refuses.
        let root = temp.path().canonicalize().unwrap();
        let directory = provenant_config::private_directory(&root, true).unwrap();
        let project = |name: &str| seed_project(&root, name, true);
        let projects = vec![project("a"), project("b"), project("c"), project("d")];
        let listed = projects.clone();
        seed_pass(
            &directory,
            &seed_clock(),
            true,
            || Ok(listed),
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
        assert_eq!(
            error.unwrap(),
            format!("{}: origin/main missing", projects[1].display())
        );
        let pass = refresh_projects(&projects, |project| {
            match project.file_name().unwrap().to_str().unwrap() {
                "a" => Ok(Refresh::Busy),
                "b" => Err("origin/main missing".into()),
                "c" => Ok(Refresh::Built("generation".into())),
                _ => Ok(Refresh::Current),
            }
        });
        assert_eq!(
            (pass.built, pass.current, pass.busy, pass.errors.len()),
            (1, 1, 1, 1)
        );
        let clean = vec![projects[0].clone()];
        seed_pass(
            &directory,
            &seed_clock(),
            true,
            || Ok(clean),
            |_| Ok(Refresh::Busy),
        )
        .unwrap();
        let (state, note) = seed_state_of(&directory);
        assert_eq!(state, "busy");
        assert!(note.unwrap().contains("retrying next tick"));
        let wall = seed_clock().wall_now().unwrap();
        assert!(!seed_retry_due(&directory, wall), "just written: not due");
        let far = WallTime::from_micros(wall.as_micros() + 61_000_000).unwrap();
        assert!(seed_retry_due(&directory, far), "busy is retried next tick");
        seed_pass(
            &directory,
            &seed_clock(),
            true,
            || Ok(vec![projects[0].clone()]),
            |_| Ok(Refresh::Current),
        )
        .unwrap();
        assert_eq!(seed_state_of(&directory), ("complete", None));
        assert!(!seed_retry_due(&directory, far), "only busy is retried");
    }

    #[test]
    fn seed_pass_skips_projects_without_a_manifest_and_reports_a_back_off() {
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
            &seed_clock(),
            true,
            || Ok(listed),
            |project| {
                called.borrow_mut().push(project.to_owned());
                Ok(Refresh::BackedOff("abc123".into()))
            },
        )
        .unwrap();
        assert_eq!(*called.borrow(), vec![broken.clone()], "bare is skipped");
        let (state, note) = seed_state_of(&directory);
        assert_eq!(state, "backed_off", "a back-off is not `complete`");
        assert_eq!(
            note.unwrap(),
            "commit abc123 failed; waiting for origin/main to move"
        );
        let pass = refresh_projects(&[bare], |_| panic!("no manifest, no refresh"));
        assert_eq!(
            pass,
            SeedPass {
                skipped: 1,
                ..Default::default()
            }
        );
    }

    #[allow(dead_code)] // The worktree driver includes this module without libtest.
    fn seed_project(root: &std::path::Path, name: &str, manifest: bool) -> std::path::PathBuf {
        let project = root.join("projects").join(name);
        std::fs::create_dir_all(project.join("tools/front-door")).unwrap();
        if manifest {
            std::fs::write(project.join(SEED_MANIFEST), "").unwrap();
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
            &seed_clock(),
            true,
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
                    &seed_clock(),
                    false,
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
            &seed_clock(),
            true,
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
