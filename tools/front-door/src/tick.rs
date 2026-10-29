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
    let result = GcResult {
        extra: Default::default(),
        state: state.into(),
        at,
        error,
    };
    let bytes = serde_json::to_vec(&result).map_err(|error| error.to_string())?;
    directory
        .atomic_write(
            "gc-result.json".as_ref(),
            "tick-gc",
            &bytes,
            PublishMode::Replace,
        )
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
            let config = provenant_config::read_with_domains(
                &resolved.roots,
                &provenant_cli::domains::SECTIONS,
            )
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

#[derive(Default, Serialize)]
pub(crate) struct Graphs {
    pub advanced: usize,
    pub errors: Vec<String>,
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
    let tasks = vec![
        gc,
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
            let human = format!(
                "tick: graphs={} errors={} gc={}\n",
                data.graphs.advanced,
                data.graphs.errors.len(),
                data.tasks[0].state
            );
            let rows = data
                .graphs
                .errors
                .iter()
                .cloned()
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
    use super::*;

    #[test]
    fn daily_cadence_survives_restart_and_fires_at_the_deadline() {
        let temp = tempfile::tempdir().unwrap();
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
}
