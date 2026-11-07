//! Opt-in proposal sweeps, submitted through the ordinary graph service.
use super::*;
use provenant_cli::steward::Settings;
use provenant_fabric_runtime::graph::{FileJournal, NodeState, template::SubmissionIdentity};
use serde_json::{Value, json};

#[derive(Default, Serialize, Deserialize)]
struct Schedule {
    generation: u64,
    operation: Option<String>,
    submitting: bool,
    next_at: Option<WallTime>,
}

fn save(directory: &Directory, schedule: &Schedule) -> Result<(), String> {
    directory
        .atomic_write(
            "schedule.json".as_ref(),
            "steward-schedule",
            &serde_json::to_vec(schedule).map_err(|error| error.to_string())?,
            PublishMode::Replace,
        )
        .map_err(|error| error.to_string())?
        .require_durable()
        .map_err(|error| error.to_string())
}

/// Hold project custody across observation, recording identity and submission.
/// Retrying an uncertain launch replays its operation, including after a crash.
fn queue(
    directory: &Directory,
    project: &str,
    cadence_seconds: u64,
    clock: &dyn Clock,
    live: impl FnOnce(&str) -> Result<bool, String>,
    launch: impl FnOnce(&str) -> Result<(), String>,
) -> Result<&'static str, String> {
    let _lock = match directory.try_lock("schedule.lock".as_ref(), LockMode::Exclusive) {
        Ok(lock) => lock,
        Err(provenant_os::fs::FsError::LockTimeout { .. }) => return Ok("running"),
        Err(error) => return Err(error.to_string()),
    };
    let now = clock.wall_now().map_err(|error| error.to_string())?;
    let mut schedule: Schedule = match directory.read_bounded("schedule.json".as_ref(), 64 * 1024) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| error.to_string())?,
        Err(error) if error.is_not_found() => Schedule::default(),
        Err(error) => return Err(error.to_string()),
    };
    if let Some(operation) = &schedule.operation
        && live(operation)?
    {
        return Ok("running");
    }
    if schedule.next_at.is_some_and(|deadline| now < deadline) {
        return Ok("not_due");
    }
    // Validate the next deadline before any external effect.
    let next_at = now
        .checked_add(
            DurationMicros::from_seconds(cadence_seconds).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    if !schedule.submitting {
        schedule.generation = schedule
            .generation
            .checked_add(1)
            .ok_or("steward generation exhausted")?;
        schedule.operation = Some(format!("tick-steward-{project}-{}", schedule.generation));
        schedule.submitting = true;
        save(directory, &schedule)?;
    }
    let operation = schedule
        .operation
        .as_deref()
        .ok_or("missing steward operation")?;
    if let Err(error) = launch(operation) {
        write_result(
            directory,
            "result.json",
            "tick-steward",
            now,
            "failed",
            Some(error.clone()),
        )?;
        return Err(error);
    }
    schedule.submitting = false;
    schedule.next_at = Some(next_at);
    save(directory, &schedule)?;
    write_result(
        directory,
        "result.json",
        "tick-steward",
        now,
        "launched",
        None,
    )?;
    Ok("launched")
}

fn task(
    settings: &Settings,
    mut run: impl FnMut(&provenant_cli::steward::Repository) -> Result<&'static str, String>,
) -> Task {
    if settings.repositories.is_empty() {
        return Task::idle("steward", "idle");
    }
    Task::run("steward", || {
        let mut state = "not_due";
        let mut errors = Vec::new();
        for (name, repository) in &settings.repositories {
            match run(repository) {
                Ok("launched") => state = "launched",
                Ok("running") if state != "launched" => state = "running",
                Ok(_) => {}
                Err(error) => errors.push(format!("{name}: {error}")),
            }
        }
        if errors.is_empty() {
            Ok(state)
        } else {
            Err(errors.join("; "))
        }
    })
}

pub(super) fn run(
    settings: &Settings,
    resolved: &crate::roots::Resolved,
    workers: &'static dyn WorkerObserver,
) -> Task {
    task(settings, |repository| {
        let globals = Globals {
            project: Some(repository.directory.clone()),
            state_dir: Some(resolved.roots.state.as_path().to_owned()),
            ..Default::default()
        };
        let factory =
            crate::fabric::factory(&globals, workers).map_err(|error| error.to_string())?;
        let project = factory.context.identity.project;
        let directory = provenant_config::private_directory(
            &resolved
                .roots
                .state
                .as_path()
                .join("tick/steward")
                .join(project.to_string()),
            true,
        )
        .map_err(|error| error.to_string())?;
        queue(
            &directory,
            &project.to_string(),
            repository.cadence_seconds,
            &SystemClock,
            |operation| {
                let id = SubmissionIdentity::Fresh.graph_id(&Value::Null, "", Some(operation))?;
                let checkpoint = FileJournal::read_checkpoint(&resolved.roots.state, project, &id)
                    .map_err(|error| error.to_string())?;
                Ok(checkpoint.is_some_and(|checkpoint| {
                    checkpoint.pause_reason.is_some()
                        || checkpoint.nodes.iter().any(|node| {
                            matches!(node.state, NodeState::Pending | NodeState::Running)
                        })
                }))
            },
            |operation| {
                let clock = SystemClock;
                let budget =
                    WaitBudget::cli(clock.mono_now(), 55).map_err(|error| error.to_string())?;
                let result = factory
                    .graph_document(
                        json!({
                            "op":"run", "use":"steward", "operation_id":operation,
                        }),
                        budget,
                        &CancelToken::new(),
                    )
                    .map_err(|error| error.to_string())?;
                if result["status"] == "paused"
                    || result["outcome"] == "failed"
                    || result["advance_error"].is_string()
                {
                    let reason = result
                        .get("advance_error")
                        .and_then(Value::as_str)
                        .or_else(|| {
                            result
                                .pointer("/checkpoint/pause_reason")
                                .and_then(Value::as_str)
                        })
                        .or_else(|| {
                            result
                                .pointer("/checkpoint/nodes/0/advance_error")
                                .and_then(Value::as_str)
                        })
                        .unwrap_or("steward graph launch failed; inspect graph output");
                    return Err(reason.into());
                }
                Ok(())
            },
        )
    })
}

#[cfg(test)]
#[allow(unused_imports)] // Included by the custom worktree test driver.
mod tests {
    use super::*;
    use provenant_core::{MonoTime, TestClock};
    use std::cell::RefCell;

    // Small: absent config must never even resolve a repository or launch.
    #[test]
    fn absent_section_is_idle() {
        let settings: Settings = toml_edit::de::from_str("").unwrap();
        let result = task(&settings, |_| panic!("disabled steward invoked launcher"));
        assert_eq!(result.state, "idle");
        assert!(result.error.is_none());
    }

    // Medium: durable scheduling, injected clock and launcher; no providers.
    #[test]
    fn due_cadence_and_live_run_are_single_flight_across_restarts() {
        let temp = tempfile::tempdir().unwrap();
        let path = std::fs::canonicalize(temp.path()).unwrap();
        let directory = provenant_config::private_directory(&path, true).unwrap();
        let mut clock = TestClock::new(
            WallTime::from_micros(1_000_000).unwrap(),
            MonoTime::from_micros(0),
        );
        let launches = RefCell::new(Vec::new());
        let launch = |operation: &str| {
            launches.borrow_mut().push(operation.to_owned());
            Ok(())
        };
        assert_eq!(
            queue(&directory, "project", 60, &clock, |_| Ok(false), launch).unwrap(),
            "launched"
        );
        let reopened = Directory::open_anchor(&path).unwrap();
        assert_eq!(
            queue(&reopened, "project", 60, &clock, |_| Ok(false), launch).unwrap(),
            "not_due"
        );
        clock
            .advance(DurationMicros::from_seconds(60).unwrap())
            .unwrap();
        assert_eq!(
            queue(&reopened, "project", 60, &clock, |_| Ok(true), launch).unwrap(),
            "running"
        );
        assert_eq!(launches.borrow().len(), 1);
        let lock = reopened
            .try_lock("schedule.lock".as_ref(), LockMode::Exclusive)
            .unwrap();
        assert_eq!(
            queue(
                &directory,
                "project",
                60,
                &clock,
                |_| panic!("lock held"),
                launch
            )
            .unwrap(),
            "running"
        );
        drop(lock);
        assert_eq!(
            queue(&reopened, "project", 60, &clock, |_| Ok(false), launch).unwrap(),
            "launched"
        );
        let operations = launches.borrow();
        assert_eq!(operations.len(), 2);
        assert_ne!(operations[0], operations[1]);
    }

    // Medium: an uncertain receipt must retain identity and the failure reason.
    #[test]
    fn failed_launch_reports_reason_and_retries_the_same_operation() {
        let temp = tempfile::tempdir().unwrap();
        let path = std::fs::canonicalize(temp.path()).unwrap();
        let directory = provenant_config::private_directory(&path, true).unwrap();
        let clock = TestClock::new(
            WallTime::from_micros(1_000_000).unwrap(),
            MonoTime::from_micros(0),
        );
        let operation = RefCell::new(String::new());
        let result = Task::run("steward", || {
            queue(
                &directory,
                "project",
                60,
                &clock,
                |_| Ok(false),
                |id| {
                    *operation.borrow_mut() = id.into();
                    Err("receipt unavailable".into())
                },
            )
        });
        assert_eq!(result.state, "failed");
        assert_eq!(result.error.as_deref(), Some("receipt unavailable"));
        let saved: GcResult = serde_json::from_slice(
            &directory
                .read_bounded("result.json".as_ref(), 64 * 1024)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(saved.error, result.error);
        let reopened = Directory::open_anchor(&path).unwrap();
        assert_eq!(
            queue(
                &reopened,
                "project",
                60,
                &clock,
                |_| Ok(false),
                |id| {
                    assert_eq!(id, *operation.borrow());
                    Ok(())
                }
            )
            .unwrap(),
            "launched"
        );
        assert!(
            queue(
                &reopened,
                "project",
                60,
                &clock,
                |_| Err("unreadable journal".into()),
                |_| panic!("uncertain liveness")
            )
            .is_err()
        );
    }
}
