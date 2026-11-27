//! One bounded entry point for advancing a graph or a queue item.
//!
//! [`wake_subject`] runs the five fixed steps of one wake: reconcile the
//! subject's queue controls, claim the subject under the deadline, take the
//! journal lock, re-read the live revision under that lock and finally run the
//! caller's bounded advance. The caller owns host context and supplies the
//! steps through [`WakeSource`], so the same coordinator is used by readers,
//! the tick scheduler and deterministic tests.
//!
//! [`wake`] is the lane-side entry. It relays the wake to the host that owns
//! the subject's custody, as a read-class frame with no mutation authority.

use provenant_core::{Clock, Deadline};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub(crate) use provenant_records::progress::WakeSubjectRef;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WakeReason {
    Tick,
    Wait,
    Result,
    InlineRun,
    Status,
    Output,
    PullRequest,
    Lane,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WakeOutcome {
    Advanced { transitions: usize },
    Busy { holder: String },
    Idle,
    Skipped { code: String },
    OverBudget { cursor: String },
}

impl WakeOutcome {
    /// The relay wire shape. A wake reports an outcome only; it never carries
    /// a resume, retry or landing grant.
    pub(crate) fn to_value(&self) -> Value {
        match self {
            Self::Advanced { transitions } => {
                json!({"outcome": "advanced", "transitions": transitions})
            }
            Self::Busy { holder } => json!({"outcome": "busy", "holder": holder}),
            Self::Idle => json!({"outcome": "idle"}),
            Self::Skipped { code } => json!({"outcome": "skipped", "code": code}),
            Self::OverBudget { cursor } => {
                json!({"outcome": "over_budget", "cursor": cursor})
            }
        }
    }

    /// Read a host's wake outcome back from the relay frame. An unknown shape
    /// is a skip, never an authority to advance locally.
    pub(crate) fn from_value(value: &Value) -> Self {
        match value["outcome"].as_str() {
            Some("advanced") => Self::Advanced {
                transitions: value["transitions"].as_u64().unwrap_or(0) as usize,
            },
            Some("busy") => Self::Busy {
                holder: value["holder"].as_str().unwrap_or("unknown").to_owned(),
            },
            Some("idle") => Self::Idle,
            Some("over_budget") => Self::OverBudget {
                cursor: value["cursor"].as_str().unwrap_or("").to_owned(),
            },
            _ => Self::Skipped {
                code: value["code"]
                    .as_str()
                    .unwrap_or("WAKE_OUTCOME_UNREADABLE")
                    .to_owned(),
            },
        }
    }
}

/// The claim a caller reports after reconciling, selecting and taking the
/// subject's journal lock. `revision` is the pre-lock checkpoint revision;
/// the coordinator compares it with a fresh read while the lock is held.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum WakeClaim {
    Ready {
        revision: u64,
    },
    Busy {
        holder: String,
    },
    #[default]
    Idle,
    Skipped {
        code: String,
    },
}

/// The caller-owned steps of one wake. Keeping them as a seam lets the
/// scheduler, the readers and the tests drive the same coordinator.
pub(crate) trait WakeSource {
    /// Step 1: reconcile queue controls scoped to the subject's queue.
    fn reconcile(
        &mut self,
        subject: &WakeSubjectRef,
        reason: WakeReason,
    ) -> Result<(), WakeOutcome>;
    /// Steps 2-3: select/claim the subject and take the journal lock with
    /// `try_lock`. A held lock reports `Busy` at once.
    fn claim(&mut self, subject: &WakeSubjectRef, deadline: Deadline) -> WakeClaim;
    /// Step 4: read the live revision while the lock is still held.
    fn live_revision(&self, subject: &WakeSubjectRef) -> u64;
    /// Step 5: advance under the checkpoint's sealed execution binding with
    /// `budget = deadline.remaining()`.
    fn advance(
        &mut self,
        subject: &WakeSubjectRef,
        reason: WakeReason,
        deadline: Deadline,
    ) -> WakeOutcome;
}

/// Run one custody-checked, deadline-bounded advance in the fixed order. A
/// subject that moved between the pre-lock read and the locked read is
/// abandoned and retried on the next pass; no advance is attempted twice.
pub(crate) fn wake_subject(
    subject: WakeSubjectRef,
    reason: WakeReason,
    deadline: Deadline,
    source: &mut impl WakeSource,
) -> WakeOutcome {
    if let Err(outcome) = source.reconcile(&subject, reason) {
        return outcome;
    }
    let locked = match source.claim(&subject, deadline) {
        WakeClaim::Ready { revision } => revision,
        WakeClaim::Busy { holder } => return WakeOutcome::Busy { holder },
        WakeClaim::Idle => return WakeOutcome::Idle,
        WakeClaim::Skipped { code } => return WakeOutcome::Skipped { code },
    };
    if source.live_revision(&subject) != locked {
        return WakeOutcome::Skipped {
            code: "STALE_REVISION".into(),
        };
    }
    source.advance(&subject, reason, deadline)
}

/// Relay a wake for the graph identified by an initial host status reading.
pub(crate) fn wake_lane_reading(
    value: &Value,
    reason: WakeReason,
    within: std::time::Duration,
) -> WakeOutcome {
    let Some(project) = value["wake_project"]
        .as_str()
        .and_then(|value| value.parse().ok())
    else {
        return WakeOutcome::Skipped {
            code: "WAKE_PROJECT_UNAVAILABLE".into(),
        };
    };
    let Some(id) = value["rows"]
        .as_array()
        .and_then(|rows| rows.iter().find_map(|row| row["graph_id"].as_str()))
    else {
        return WakeOutcome::Idle;
    };
    let now = provenant_os::clock::SystemClock::new().mono_now();
    let duration = provenant_core::DurationMicros::from_micros(
        u64::try_from(within.as_micros()).unwrap_or(u64::MAX),
    );
    let deadline = Deadline::after(now, duration).unwrap_or(Deadline::at(now));
    wake(
        WakeSubjectRef::Graph {
            project,
            id: id.to_owned(),
        },
        reason,
        deadline,
    )
}

/// Lane-side entry. A lane does not own custody of the subject's host state,
/// so it sends a read-class wake frame to the host and maps the reply. On the
/// host, readers drive [`wake_subject`] directly with their own context.
#[allow(
    clippy::disallowed_methods,
    reason = "wake transport uses the caller's remaining real-time budget"
)]
pub(crate) fn wake(subject: WakeSubjectRef, reason: WakeReason, deadline: Deadline) -> WakeOutcome {
    // The wire frame carries only the subject: the host classifies the wake
    // from the reader that received it, so the reason is metadata here.
    let _ = reason;
    if !provenant_cli::fabric::in_lane() {
        return WakeOutcome::Skipped {
            code: "WAKE_HOST_READER_OWNS_ADVANCE".into(),
        };
    }
    let now = provenant_os::clock::SystemClock::new().mono_now();
    let within = std::time::Duration::from_micros(
        deadline.remaining(now).as_micros().min(u64::MAX as u128) as u64,
    );
    let cancel = provenant_core::CancelToken::new();
    match provenant_cli::fabric::graph_relay::wake_from_lane(subject, &cancel, within) {
        Ok(value) => WakeOutcome::from_value(&value),
        Err(error) => WakeOutcome::Skipped {
            code: error.code().to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::{DurationMicros, MonoTime, ProjectId};
    use std::cell::RefCell;

    fn deadline() -> Deadline {
        Deadline::after(
            MonoTime::from_micros(1_000),
            DurationMicros::from_micros(10_000),
        )
        .unwrap_or_else(|_| Deadline::at(MonoTime::from_micros(11_000)))
    }

    fn graph_subject() -> WakeSubjectRef {
        WakeSubjectRef::Graph {
            project: ProjectId::from_bytes([1; 16]),
            id: "graph-one".into(),
        }
    }

    #[derive(Default)]
    struct Fake {
        steps: RefCell<Vec<&'static str>>,
        claim: WakeClaim,
        moved: bool,
        outcome: Option<WakeOutcome>,
        advances: RefCell<usize>,
    }

    impl WakeSource for Fake {
        fn reconcile(&mut self, _s: &WakeSubjectRef, _r: WakeReason) -> Result<(), WakeOutcome> {
            self.steps.borrow_mut().push("reconcile");
            Ok(())
        }
        fn claim(&mut self, _s: &WakeSubjectRef, _d: Deadline) -> WakeClaim {
            self.steps.borrow_mut().push("claim");
            self.claim.clone()
        }
        fn live_revision(&self, _s: &WakeSubjectRef) -> u64 {
            self.steps.borrow_mut().push("revision");
            if self.moved { 2 } else { 1 }
        }
        fn advance(&mut self, _s: &WakeSubjectRef, _r: WakeReason, _d: Deadline) -> WakeOutcome {
            self.steps.borrow_mut().push("advance");
            *self.advances.borrow_mut() += 1;
            self.outcome
                .clone()
                .unwrap_or(WakeOutcome::Advanced { transitions: 1 })
        }
    }

    // Census the production call chain and use one counting source for all
    // reasons. The source assertions fail when an entry point loses its route.
    #[test]
    fn every_reader_and_tick_reaches_the_common_wake() {
        fn body<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
            let (_, tail) = source.split_once(start).expect("entry point exists");
            tail.split_once(end).expect("entry point end exists").0
        }
        let unified = include_str!("unified.rs");
        let wait = include_str!("unified/graph_wait.rs");
        let observation = include_str!("interface/observation.rs");
        let interface = include_str!("interface.rs");
        let tick = include_str!("graph_execution.rs");
        for (source, start, end, call) in [
            (
                wait,
                "fn read_status_rows_with_recovery(",
                "fn missing_graph_context(",
                "wake_graph_for_reader",
            ),
            (
                wait,
                "fn wake_pr_item(",
                "fn reader_wake_budget(",
                "wake_in_host",
            ),
            (
                unified,
                "fn unified_graph_result(",
                "fn finding_severity_counts(",
                "read_status_rows_with_recovery",
            ),
            (
                unified,
                "fn unified_graph_command(",
                "fn graph_listing(",
                "wake_graph_for_reader",
            ),
            (
                unified,
                "fn unified_graph_output(",
                "fn host_graph_output(",
                "wake_graph_for_reader",
            ),
            (
                observation,
                "fn observe_remote_graph_attempts(",
                "fn graph_run_has_remote_pending(",
                "wake_graph_for_reader",
            ),
            (
                interface,
                "fn wait_document(",
                "fn graph_trace(",
                "wake_graph_for_reader",
            ),
            (
                tick,
                "fn tick_graphs(",
                "fn graph_wake_due(",
                "wake_in_host",
            ),
            (
                wait,
                "fn wake_in_host(",
                "fn read_lane_graph(",
                "wake_subject",
            ),
        ] {
            assert!(
                body(source, start, end).contains(call),
                "{start} must route through {call}"
            );
        }
        let mut source = Fake {
            claim: WakeClaim::Ready { revision: 1 },
            ..Default::default()
        };
        for reason in [
            WakeReason::Wait,
            WakeReason::PullRequest,
            WakeReason::Result,
            WakeReason::InlineRun,
            WakeReason::Status,
            WakeReason::Output,
            WakeReason::Tick,
        ] {
            assert_eq!(
                wake_subject(graph_subject(), reason, deadline(), &mut source),
                WakeOutcome::Advanced { transitions: 1 }
            );
        }
        assert_eq!(*source.advances.borrow(), 7);
    }

    #[test]
    fn wake_rechecks_revision_after_lock() {
        let mut fake = Fake {
            claim: WakeClaim::Ready { revision: 1 },
            moved: true,
            ..Default::default()
        };
        let outcome = wake_subject(graph_subject(), WakeReason::Wait, deadline(), &mut fake);
        assert_eq!(
            outcome,
            WakeOutcome::Skipped {
                code: "STALE_REVISION".into()
            }
        );
        assert_eq!(*fake.advances.borrow(), 0);
        assert_eq!(fake.steps.into_inner(), ["reconcile", "claim", "revision"]);
    }

    #[test]
    fn concurrent_revision_change_is_rejected_under_the_claimed_lock() {
        use std::sync::{
            Barrier, Mutex, MutexGuard,
            atomic::{AtomicU64, Ordering},
        };
        struct RacingSource<'a> {
            lock: &'a Mutex<()>,
            revision: &'a AtomicU64,
            read: &'a Barrier,
            written: &'a Barrier,
            guard: Option<MutexGuard<'a, ()>>,
            advanced: bool,
        }
        impl WakeSource for RacingSource<'_> {
            fn reconcile(&mut self, _: &WakeSubjectRef, _: WakeReason) -> Result<(), WakeOutcome> {
                Ok(())
            }
            fn claim(&mut self, _: &WakeSubjectRef, _: Deadline) -> WakeClaim {
                let revision = self.revision.load(Ordering::SeqCst);
                self.read.wait();
                self.written.wait();
                self.guard = Some(self.lock.lock().unwrap());
                WakeClaim::Ready { revision }
            }
            fn live_revision(&self, _: &WakeSubjectRef) -> u64 {
                assert!(self.guard.is_some());
                assert!(self.lock.try_lock().is_err());
                self.revision.load(Ordering::SeqCst)
            }
            fn advance(&mut self, _: &WakeSubjectRef, _: WakeReason, _: Deadline) -> WakeOutcome {
                self.advanced = true;
                WakeOutcome::Advanced { transitions: 1 }
            }
        }
        let lock = Mutex::new(());
        let revision = AtomicU64::new(1);
        let read = Barrier::new(2);
        let written = Barrier::new(2);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                read.wait();
                {
                    let _guard = lock.lock().unwrap();
                    revision.store(2, Ordering::SeqCst);
                }
                written.wait();
            });
            let mut source = RacingSource {
                lock: &lock,
                revision: &revision,
                read: &read,
                written: &written,
                guard: None,
                advanced: false,
            };
            assert_eq!(
                wake_subject(graph_subject(), WakeReason::Wait, deadline(), &mut source),
                WakeOutcome::Skipped {
                    code: "STALE_REVISION".into()
                }
            );
            assert!(!source.advanced);
        });
    }

    #[test]
    fn wake_advances_when_revision_holds_under_lock() {
        let mut fake = Fake {
            claim: WakeClaim::Ready { revision: 1 },
            outcome: Some(WakeOutcome::Advanced { transitions: 3 }),
            ..Default::default()
        };
        let outcome = wake_subject(graph_subject(), WakeReason::Wait, deadline(), &mut fake);
        assert_eq!(outcome, WakeOutcome::Advanced { transitions: 3 });
        assert_eq!(
            fake.steps.into_inner(),
            ["reconcile", "claim", "revision", "advance"]
        );
    }

    #[test]
    fn busy_and_idle_claims_never_advance() {
        let mut busy = Fake {
            claim: WakeClaim::Busy {
                holder: "lane-2".into(),
            },
            ..Default::default()
        };
        assert_eq!(
            wake_subject(graph_subject(), WakeReason::Tick, deadline(), &mut busy),
            WakeOutcome::Busy {
                holder: "lane-2".into()
            }
        );
        assert_eq!(*busy.advances.borrow(), 0);

        let mut idle = Fake {
            claim: WakeClaim::Idle,
            ..Default::default()
        };
        assert_eq!(
            wake_subject(graph_subject(), WakeReason::Status, deadline(), &mut idle),
            WakeOutcome::Idle
        );
        assert_eq!(*idle.advances.borrow(), 0);
    }

    #[test]
    fn two_waiters_and_tick_make_one_transition() {
        // The first claimant holds the single-flight lease; every other
        // caller sees Busy and does not advance the same subject.
        let claimed = std::cell::Cell::new(false);
        struct Lease<'a>(&'a std::cell::Cell<bool>);
        impl WakeSource for Lease<'_> {
            fn reconcile(
                &mut self,
                _s: &WakeSubjectRef,
                _r: WakeReason,
            ) -> Result<(), WakeOutcome> {
                Ok(())
            }
            fn claim(&mut self, _s: &WakeSubjectRef, _d: Deadline) -> WakeClaim {
                if self.0.replace(true) {
                    WakeClaim::Busy {
                        holder: "waiter-1".into(),
                    }
                } else {
                    WakeClaim::Ready { revision: 1 }
                }
            }
            fn live_revision(&self, _s: &WakeSubjectRef) -> u64 {
                1
            }
            fn advance(
                &mut self,
                _s: &WakeSubjectRef,
                _r: WakeReason,
                _d: Deadline,
            ) -> WakeOutcome {
                WakeOutcome::Advanced { transitions: 1 }
            }
        }
        let first = wake_subject(
            graph_subject(),
            WakeReason::Wait,
            deadline(),
            &mut Lease(&claimed),
        );
        let second = wake_subject(
            graph_subject(),
            WakeReason::Wait,
            deadline(),
            &mut Lease(&claimed),
        );
        let tick = wake_subject(
            graph_subject(),
            WakeReason::Tick,
            deadline(),
            &mut Lease(&claimed),
        );
        assert_eq!(first, WakeOutcome::Advanced { transitions: 1 });
        assert_eq!(
            second,
            WakeOutcome::Busy {
                holder: "waiter-1".into()
            }
        );
        assert_eq!(
            tick,
            WakeOutcome::Busy {
                holder: "waiter-1".into()
            }
        );
    }

    #[test]
    fn wait_pr_advances_adopted_item_before_graph_exists() {
        // A queue subject's claim materialises its graph; the advance sees the
        // freshly created subject and reports one transition.
        let created = RefCell::new(false);
        struct Adopt<'a>(&'a RefCell<bool>);
        impl WakeSource for Adopt<'_> {
            fn reconcile(
                &mut self,
                _s: &WakeSubjectRef,
                _r: WakeReason,
            ) -> Result<(), WakeOutcome> {
                Ok(())
            }
            fn claim(&mut self, s: &WakeSubjectRef, _d: Deadline) -> WakeClaim {
                assert!(matches!(s, WakeSubjectRef::QueueItem { .. }));
                *self.0.borrow_mut() = true;
                WakeClaim::Ready { revision: 1 }
            }
            fn live_revision(&self, _s: &WakeSubjectRef) -> u64 {
                1
            }
            fn advance(
                &mut self,
                _s: &WakeSubjectRef,
                _r: WakeReason,
                _d: Deadline,
            ) -> WakeOutcome {
                assert!(*self.0.borrow());
                WakeOutcome::Advanced { transitions: 1 }
            }
        }
        let outcome = wake_subject(
            WakeSubjectRef::QueueItem {
                project: ProjectId::from_bytes([1; 16]),
                queue: "deliver".into(),
                item: "issue-42".into(),
            },
            WakeReason::PullRequest,
            deadline(),
            &mut Adopt(&created),
        );
        assert_eq!(outcome, WakeOutcome::Advanced { transitions: 1 });
    }

    #[test]
    fn lane_wake_relays_without_mutation_authority() {
        use provenant_cli::fabric::graph_relay::GraphRelay;
        let wire = GraphRelay::Wake {
            subject: WakeSubjectRef::Graph {
                project: ProjectId::from_bytes([1; 16]),
                id: "graph-one".into(),
            },
        };
        let frame = serde_json::to_value(&wire).unwrap_or(Value::Null);
        assert_eq!(frame["op"], "wake");
        assert_eq!(frame["subject"]["graph"]["id"], "graph-one");
        assert!(frame.get("authority").is_none());
        assert!(frame.get("resume").is_none());
        assert!(frame.get("retry").is_none());
        assert!(frame.get("land").is_none());
    }

    #[test]
    fn host_wake_defers_to_the_reader_that_owns_custody() {
        // Outside a lane there is no ambient host context: a wake must not
        // open state on its own. Readers drive `wake_subject` with their own
        // context; a lane relays through `wake_from_lane`.
        let outcome = wake(graph_subject(), WakeReason::Wait, deadline());
        assert!(matches!(outcome, WakeOutcome::Skipped { .. }));
    }

    #[test]
    fn wake_outcome_round_trips_through_the_relay_wire_shape() {
        for outcome in [
            WakeOutcome::Advanced { transitions: 2 },
            WakeOutcome::Busy {
                holder: "lane-1".into(),
            },
            WakeOutcome::Idle,
            WakeOutcome::Skipped { code: "X".into() },
            WakeOutcome::OverBudget {
                cursor: "g/1".into(),
            },
        ] {
            assert_eq!(WakeOutcome::from_value(&outcome.to_value()), outcome);
        }
    }
}
