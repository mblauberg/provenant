//! Registry contract for independently budgeted scheduled tasks.
//!
//! Every task carries its own wall budget. The runner samples one clock for a
//! whole pass, caps each worker at its own deadline, and holds a single-flight
//! lease until the worker actually returns, including past an overrun.

use super::TaskResult;
use provenant_core::{Clock, Deadline, DurationMicros, MonoTime, WallTime};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// The sampled time and budget one task worker may spend.
#[derive(Clone, Copy)]
pub(super) struct TaskContext {
    pub now: MonoTime,
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
pub(super) struct TaskDef<'a> {
    name: &'static str,
    enabled: bool,
    budget: DurationMicros,
    body: Box<dyn Fn(TaskContext) -> TaskResult + Send + Sync + 'a>,
}

impl<'a> TaskDef<'a> {
    pub(super) fn new(
        name: &'static str,
        enabled: bool,
        budget: DurationMicros,
        body: impl Fn(TaskContext) -> TaskResult + Send + Sync + 'a,
    ) -> Self {
        Self {
            name,
            enabled,
            budget,
            body: Box::new(body),
        }
    }
}

impl TickTask for TaskDef<'_> {
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

/// In-process single-flight leases. A guard is held from dispatch until the
/// worker returns, including past a deadline overrun, so a second tick sees
/// `busy` instead of starting the same task twice.
#[derive(Clone, Default)]
pub(super) struct TaskLeases {
    held: Arc<Mutex<BTreeMap<&'static str, String>>>,
}

impl TaskLeases {
    pub(super) fn try_hold(&self, name: &'static str, holder: String) -> Option<LeaseGuard> {
        let mut held = self.held.lock().ok()?;
        if held.contains_key(name) {
            return None;
        }
        held.insert(name, holder);
        Some(LeaseGuard {
            held: Arc::clone(&self.held),
            name,
        })
    }

    #[cfg(test)]
    pub(super) fn holder(&self, name: &'static str) -> Option<String> {
        self.held.lock().ok()?.get(name).cloned()
    }
}

/// Released when the worker returns, never when its deadline expires.
pub(super) struct LeaseGuard {
    held: Arc<Mutex<BTreeMap<&'static str, String>>>,
    name: &'static str,
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        if let Ok(mut held) = self.held.lock() {
            held.remove(&self.name);
        }
    }
}

/// A fairness cursor. Keys are `project/subject`; a `Busy` or `OverBudget`
/// visit is retried first on the next pass, and every visit moves `last`
/// forward so a pass never loops on one subject forever.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Cursor {
    #[serde(default)]
    retry: Vec<String>,
    #[serde(default)]
    last: Option<String>,
}

impl Cursor {
    /// Retry-front first, then the remaining due subjects in due order.
    pub(super) fn order(&self, due: &[String]) -> Vec<String> {
        let mut ordered: Vec<String> = self
            .retry
            .iter()
            .filter(|key| due.contains(*key))
            .cloned()
            .collect();
        for key in due {
            if !ordered.contains(key) {
                ordered.push(key.clone());
            }
        }
        ordered
    }

    /// Advance past a visited subject. Only `Busy` and `OverBudget` leave a
    /// retry marker; every other outcome is released.
    pub(super) fn advance(&mut self, key: &str, retry: bool) {
        self.retry.retain(|entry| entry.as_str() != key);
        if retry {
            self.retry.push(key.to_owned());
        }
        self.last = Some(key.to_owned());
    }

    #[cfg(test)]
    pub(super) fn last(&self) -> Option<&str> {
        self.last.as_deref()
    }
}

/// Run enabled tasks concurrently under `concurrency`, each under its own
/// deadline. Results come back in declaration order regardless of completion
/// order, so `tick --json` stays stable.
pub(super) fn run_tasks<V: TickTask>(
    tasks: Vec<V>,
    concurrency: usize,
    leases: &TaskLeases,
    clock: &impl Clock,
    wall: WallTime,
    holder: &str,
) -> Vec<TaskOutcome> {
    let count = tasks.len();
    let queue = Arc::new(Mutex::new(tasks.into_iter().enumerate()));
    let outcomes = Arc::new(Mutex::new(Vec::with_capacity(count)));
    let slots = concurrency.max(1).min(count.max(1));
    std::thread::scope(|scope| {
        for _ in 0..slots {
            let queue = Arc::clone(&queue);
            let outcomes = Arc::clone(&outcomes);
            scope.spawn(move || {
                loop {
                    let next = { queue.lock().ok().and_then(|mut queue| queue.next()) };
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
                        match leases.try_hold(name, holder.to_owned()) {
                            None => TaskResult::busy(wall, holder),
                            Some(_guard) => {
                                let result = task.run(TaskContext {
                                    now,
                                    wall,
                                    deadline,
                                });
                                let ended = clock.mono_now();
                                if result.state != "busy" && deadline.is_expired(ended) {
                                    TaskResult::over_budget(wall, name)
                                } else {
                                    result
                                }
                            }
                        }
                    };
                    outcomes
                        .lock()
                        .unwrap()
                        .push((index, TaskOutcome { name, result }));
                }
            });
        }
    });
    let mut outcomes = Arc::try_unwrap(outcomes)
        .ok()
        .expect("all workers joined")
        .into_inner()
        .expect("outcomes lock intact");
    outcomes.sort_by_key(|(index, _)| *index);
    outcomes.into_iter().map(|(_, outcome)| outcome).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn one_second() -> DurationMicros {
        DurationMicros::from_seconds(1).unwrap()
    }

    #[test]
    fn tasks_run_concurrently_up_to_the_cap() {
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let tasks: Vec<TaskDef<'_>> = ["graphs", "forge_events", "settlement"]
            .into_iter()
            .map(|name| {
                let active = Arc::clone(&active);
                let peak = Arc::clone(&peak);
                TaskDef::new(name, true, one_second(), move |_ctx| {
                    let live = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(live, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(30));
                    active.fetch_sub(1, Ordering::SeqCst);
                    TaskResult::sample(crate::tick::test_wall())
                })
            })
            .collect();
        let clock = provenant_os::clock::SystemClock;
        let outcomes = run_tasks(
            tasks,
            2,
            &TaskLeases::default(),
            &clock,
            crate::tick::test_wall(),
            "tick-test",
        );
        assert_eq!(outcomes.len(), 3);
        assert!(peak.load(Ordering::SeqCst) >= 2, "workers did not overlap");
    }

    #[test]
    fn slow_graph_slow_forge_page_and_second_tick_overlap() {
        let leases = TaskLeases::default();
        let first_leases = leases.clone();
        let both_started = Arc::new(std::sync::Barrier::new(3));
        let release = Arc::new(std::sync::Barrier::new(3));
        let graph_started = Arc::clone(&both_started);
        let graph_release = Arc::clone(&release);
        let forge_started = Arc::clone(&both_started);
        let forge_release = Arc::clone(&release);
        let tasks: Vec<TaskDef<'_>> = vec![
            TaskDef::new("graphs", true, one_second(), move |_ctx| {
                graph_started.wait();
                graph_release.wait();
                TaskResult::sample(crate::tick::test_wall())
            }),
            TaskDef::new("forge_events", true, one_second(), move |_ctx| {
                forge_started.wait();
                forge_release.wait();
                TaskResult::sample(crate::tick::test_wall())
            }),
        ];
        let clock = provenant_os::clock::SystemClock;
        let first = std::thread::spawn(move || {
            run_tasks(
                tasks,
                2,
                &first_leases,
                &clock,
                crate::tick::test_wall(),
                "tick-a",
            )
        });
        // Both workers are in flight at the same barrier: a slow graph and a
        // slow forge page overlap instead of serialising.
        both_started.wait();
        let second = run_tasks(
            vec![
                TaskDef::new("graphs", true, one_second(), |_ctx| {
                    panic!("second tick must not re-run a held task")
                }),
                TaskDef::new("forge_events", true, one_second(), |_ctx| {
                    panic!("second tick must not re-run a held task")
                }),
            ],
            2,
            &leases,
            &clock,
            crate::tick::test_wall(),
            "tick-b",
        );
        assert!(second.iter().all(|outcome| outcome.result.state == "busy"));
        release.wait();
        let first = first.join().unwrap();
        assert!(
            first
                .iter()
                .all(|outcome| outcome.result.state == "observed")
        );
    }

    #[test]
    fn overrun_task_holds_lease_until_worker_ends() {
        let leases = TaskLeases::default();
        let started = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        let held = leases.clone();
        let started_thread = Arc::clone(&started);
        let release_thread = Arc::clone(&release);
        let worker = std::thread::spawn(move || {
            let _guard = held
                .try_hold("graphs", "tick-a".into())
                .expect("first tick takes the lease");
            started_thread.wait();
            release_thread.wait();
        });
        started.wait();
        assert!(leases.try_hold("graphs", "tick-b".into()).is_none());
        assert_eq!(leases.holder("graphs").as_deref(), Some("tick-a"));
        let over = TaskResult::over_budget(crate::tick::test_wall(), "graphs");
        assert_eq!(over.state, "over_budget");
        release.wait();
        worker.join().unwrap();
        assert!(leases.try_hold("graphs", "tick-b".into()).is_some());
    }

    #[test]
    fn cursor_advances_past_busy_and_failed() {
        let mut cursor = Cursor::default();
        let due: Vec<String> = ["p/a", "p/b", "p/c"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(cursor.order(&due), due);
        cursor.advance("p/a", true); // Busy
        cursor.advance("p/b", false); // Advanced
        cursor.advance("p/c", true); // OverBudget
        assert_eq!(cursor.last(), Some("p/c"));
        assert_eq!(cursor.order(&due), vec!["p/a", "p/c", "p/b"]);
        // A completed subject releases its retry marker.
        cursor.advance("p/a", false);
        assert_eq!(cursor.order(&due), vec!["p/c", "p/a", "p/b"]);
    }

    #[test]
    fn disabled_tasks_are_not_dispatched() {
        let leases = TaskLeases::default();
        let tasks = vec![TaskDef::new("gc", false, one_second(), |_ctx| {
            panic!("disabled task must not run")
        })];
        let clock = provenant_os::clock::SystemClock;
        let outcomes = run_tasks(tasks, 4, &leases, &clock, crate::tick::test_wall(), "t");
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].result.state, "disabled");
        assert!(leases.holder("gc").is_none());
    }
}
