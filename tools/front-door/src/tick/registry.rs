//! Registry contract for independently budgeted scheduled tasks.
//!
//! Every task carries its own wall budget. The runner samples one clock for a
//! whole pass, caps each worker at its own deadline, and holds a single-flight
//! lease until the worker actually returns, including past an overrun.

use super::TaskResult;
use provenant_core::{Clock, Deadline, DurationMicros, WallTime};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

/// The sampled time and budget one task worker may spend.
#[derive(Clone, Copy)]
pub(super) struct TaskContext {
    pub wall: WallTime,
    pub deadline: Deadline,
}

/// A tick task owns its worker execution and returns the same durable result
/// shape used by the existing scheduler.
pub(super) trait TickTask: Send + Sync {
    fn name(&self) -> &'static str;
    fn enabled(&self) -> bool {
        true
    }
    fn budget(&self) -> DurationMicros;
    fn run(&self, ctx: TaskContext) -> TaskResult;
}

/// A named task whose body is a scoped closure. The lifetime stays scoped so a
/// task can borrow resolved roots and config without leaking them into state.
pub(super) struct TaskDef {
    name: &'static str,
    enabled: bool,
    budget: DurationMicros,
    body: Box<dyn Fn(TaskContext) -> TaskResult + Send + Sync + 'static>,
}

impl TaskDef {
    pub(super) fn new(
        name: &'static str,
        enabled: bool,
        budget: DurationMicros,
        body: impl Fn(TaskContext) -> TaskResult + Send + Sync + 'static,
    ) -> Self {
        Self {
            name,
            enabled,
            budget,
            body: Box::new(body),
        }
    }
}

impl TickTask for TaskDef {
    fn name(&self) -> &'static str {
        self.name
    }
    fn enabled(&self) -> bool {
        self.enabled
    }
    fn budget(&self) -> DurationMicros {
        self.budget
    }
    fn run(&self, ctx: TaskContext) -> TaskResult {
        (self.body)(ctx)
    }
}

/// One task's durable outcome, paired with its name for the report.
pub(super) struct TaskOutcome {
    pub name: &'static str,
    pub result: TaskResult,
}

/// Host-wide task locks remain held by the actual worker until it exits.
#[derive(Clone)]
pub(super) struct TaskLeases {
    root: PathBuf,
}

impl TaskLeases {
    pub(super) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn try_hold(&self, name: &'static str) -> Result<provenant_os::fs::LockGuard, String> {
        let directory = provenant_config::private_directory(&self.root.join("tick"), true)
            .map_err(|error| error.to_string())?;
        directory
            .try_lock(
                format!("{name}.lock").as_ref(),
                provenant_os::fs::LockMode::Exclusive,
            )
            .map_err(|error| error.to_string())
    }
}

/// A fairness cursor. Keys are `project/subject`; a `Busy` or `OverBudget`
/// visit is retried first on the next pass, and every visit moves `last`
/// forward so a pass never loops on one subject forever.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Cursor {
    #[serde(default)]
    retry: Vec<String>,
    #[serde(default)]
    last: Option<String>,
}

impl Cursor {
    /// Retry-front first, then the remaining due subjects in due order.
    pub(crate) fn order(&self, due: &[String]) -> Vec<String> {
        let mut ordered: Vec<String> = self
            .retry
            .iter()
            .filter(|key| due.contains(*key))
            .cloned()
            .collect();
        let start = self
            .last
            .as_ref()
            .and_then(|last| due.iter().position(|key| key == last))
            .map_or(0, |index| (index + 1) % due.len());
        for key in due.iter().cycle().skip(start).take(due.len()) {
            if !ordered.contains(key) {
                ordered.push(key.clone());
            }
        }
        ordered
    }

    /// Advance past a visited subject. Only `Busy` and `OverBudget` leave a
    /// retry marker; every other outcome is released.
    pub(crate) fn advance(&mut self, key: &str, retry: bool) {
        self.retry.retain(|entry| entry.as_str() != key);
        if retry {
            self.retry.push(key.to_owned());
        }
        self.last = Some(key.to_owned());
    }
}

fn reserve_slot(active: &AtomicUsize, slots: usize) -> bool {
    let mut current = active.load(Ordering::SeqCst);
    while current < slots {
        match active.compare_exchange_weak(current, current + 1, Ordering::SeqCst, Ordering::SeqCst)
        {
            Ok(_) => return true,
            Err(observed) => current = observed,
        }
    }
    false
}

/// Run enabled tasks concurrently under `concurrency`, each under its own
/// deadline. Results come back in declaration order regardless of completion
/// order, so `tick --json` stays stable.
pub(super) fn run_tasks<V: TickTask + 'static>(
    tasks: Vec<V>,
    concurrency: usize,
    leases: &TaskLeases,
    clock: &impl Clock,
    wall: WallTime,
    holder: &str,
) -> Result<Vec<TaskOutcome>, String> {
    let count = tasks.len();
    let queue = Arc::new(Mutex::new(tasks.into_iter().enumerate()));
    let outcomes = Arc::new(Mutex::new(Vec::with_capacity(count)));
    let lock_failed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let slots = concurrency.max(1).min(count.max(1));
    let active = Arc::new(AtomicUsize::new(0));
    std::thread::scope(|scope| {
        for _ in 0..slots {
            let queue = Arc::clone(&queue);
            let outcomes = Arc::clone(&outcomes);
            let active = Arc::clone(&active);
            let lock_failed = Arc::clone(&lock_failed);
            scope.spawn(move || {
                loop {
                    let Ok(mut queue) = queue.lock() else {
                        lock_failed.store(true, Ordering::SeqCst);
                        break;
                    };
                    let next = queue.next();
                    drop(queue);
                    let Some((index, task)) = next else {
                        break;
                    };
                    let name = task.name();
                    let result = if !task.enabled() {
                        TaskResult::disabled(wall)
                    } else {
                        let now = clock.mono_now();
                        let deadline = Deadline::after(now, task.budget())
                            .unwrap_or_else(|_| Deadline::at(now));
                        match leases.try_hold(name) {
                            Err(error) => TaskResult::busy(wall, &format!("{holder}: {error}")),
                            Ok(guard) if !reserve_slot(&active, slots) => {
                                drop(guard);
                                TaskResult::busy(
                                    wall,
                                    "tick concurrency occupied by overrunning workers",
                                )
                            }
                            Ok(guard) => {
                                let worker_active = Arc::clone(&active);
                                let (send, receive) = mpsc::sync_channel(1);
                                std::thread::spawn(move || {
                                    let _guard = guard;
                                    let result =
                                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                                            || task.run(TaskContext { wall, deadline }),
                                        ));
                                    worker_active.fetch_sub(1, Ordering::SeqCst);
                                    if let Ok(result) = result {
                                        let _ = send.send(result);
                                    }
                                });
                                match receive.recv_timeout(Duration::from_micros(
                                    deadline.remaining(clock.mono_now()).as_micros(),
                                )) {
                                    Ok(result) => result,
                                    Err(mpsc::RecvTimeoutError::Timeout) => {
                                        TaskResult::over_budget(wall, name)
                                    }
                                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                                        TaskResult::busy(wall, "worker failed")
                                    }
                                }
                            }
                        }
                    };
                    match outcomes.lock() {
                        Ok(mut outcomes) => outcomes.push((index, TaskOutcome { name, result })),
                        Err(_) => {
                            lock_failed.store(true, Ordering::SeqCst);
                            break;
                        }
                    }
                }
            });
        }
    });
    if lock_failed.load(Ordering::SeqCst) {
        return Err("tick dispatch lock poisoned".into());
    }
    let mut outcomes = Arc::try_unwrap(outcomes)
        .map_err(|_| "tick dispatchers still hold outcomes")?
        .into_inner()
        .map_err(|_| "tick outcomes lock poisoned")?;
    outcomes.sort_by_key(|(index, _)| *index);
    Ok(outcomes.into_iter().map(|(_, outcome)| outcome).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrun_reports_before_worker_exits_and_retains_lease() {
        let temp = tempfile::tempdir().unwrap();
        let leases = TaskLeases::new(temp.path().to_owned());
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release = Mutex::new(release_rx);
        let calls = Arc::new(AtomicUsize::new(0));
        let worker_calls = Arc::clone(&calls);
        let task = TaskDef::new(
            "graphs",
            true,
            DurationMicros::from_micros(50_000),
            move |_| {
                worker_calls.fetch_add(1, Ordering::SeqCst);
                started_tx.send(()).unwrap();
                release.lock().unwrap().recv().unwrap();
                TaskResult::sample(crate::tick::test_wall())
            },
        );
        let clock = provenant_os::clock::SystemClock;
        let outcomes = run_tasks(
            vec![task],
            1,
            &leases,
            &clock,
            crate::tick::test_wall(),
            "first",
        )
        .unwrap();
        started_rx.recv().unwrap();
        assert_eq!(outcomes[0].result.state, "over_budget");
        let second = TaskDef::new("graphs", true, DurationMicros::from_micros(50_000), |_| {
            panic!("lease must exclude the second tick")
        });
        let outcomes = run_tasks(
            vec![second],
            1,
            &leases,
            &clock,
            crate::tick::test_wall(),
            "second",
        )
        .unwrap();
        assert_eq!(outcomes[0].result.state, "busy");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        release_tx.send(()).unwrap();
    }

    #[test]
    fn concurrent_ticks_share_the_host_lease() {
        let temp = tempfile::tempdir().unwrap();
        let leases = TaskLeases::new(temp.path().to_owned());
        let first_leases = leases.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release = Mutex::new(release_rx);
        let first = std::thread::spawn(move || {
            let task = TaskDef::new(
                "graphs",
                true,
                DurationMicros::from_seconds(5).unwrap(),
                move |_| {
                    started_tx.send(()).unwrap();
                    release.lock().unwrap().recv().unwrap();
                    TaskResult::sample(crate::tick::test_wall())
                },
            );
            run_tasks(
                vec![task],
                1,
                &first_leases,
                &provenant_os::clock::SystemClock,
                crate::tick::test_wall(),
                "first",
            )
            .unwrap()
        });
        started_rx.recv().unwrap();
        let task = TaskDef::new(
            "graphs",
            true,
            DurationMicros::from_seconds(5).unwrap(),
            |_| panic!("already leased"),
        );
        let second = run_tasks(
            vec![task],
            1,
            &leases,
            &provenant_os::clock::SystemClock,
            crate::tick::test_wall(),
            "second",
        )
        .unwrap();
        assert_eq!(second[0].result.state, "busy");
        release_tx.send(()).unwrap();
        assert_eq!(first.join().unwrap()[0].result.state, "observed");
    }

    #[test]
    fn cursor_rotates_and_prioritises_retry() {
        let due: Vec<String> = (0..5).map(|i| i.to_string()).collect();
        let mut cursor = Cursor::default();
        let mut visited = Vec::new();
        for _ in 0..3 {
            for key in cursor.order(&due).into_iter().take(2) {
                cursor.advance(&key, false);
                visited.push(key);
            }
        }
        assert_eq!(&visited[..5], &due);
        cursor.advance("2", true);
        assert_eq!(cursor.order(&due)[0], "2");
    }
    #[test]
    fn task_summary_contains_every_registry_entry() {
        let names = ["settlement", "graphs", "forge_events", "gc", "steward"];
        let rows: Vec<super::super::Task> = names
            .iter()
            .map(|name| super::super::Task {
                task: name,
                state: "observed".into(),
                error: None,
                diagnostic: None,
            })
            .collect();
        let summary = super::super::task_summary(&rows);
        for name in names {
            assert!(summary.contains(&format!("{name}=observed")));
        }
    }
}
