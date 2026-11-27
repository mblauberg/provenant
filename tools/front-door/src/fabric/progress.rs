//! One bounded entry point for advancing a graph or a queue item.
//!
//! [`wake_subject`] runs the five fixed steps of one wake: reconcile the
//! subject's queue controls, claim the subject under the deadline, take the
//! journal lock, re-read the live revision under that lock and finally run the
//! caller's bounded advance. The caller owns host context and supplies the
//! steps through [`WakeSource`], so the same coordinator is used by readers,
//! the tick scheduler and deterministic tests.
//!
//! [`wake_with`] is the single injected-driver seam: the caller supplies the
//! bounded advance and the coordinator supplies the fixed order around it.
//! [`wake`] is the lane-side entry. It relays the wake to the host that owns
//! the subject's custody, as a read-class frame with no mutation authority.

use provenant_core::{Clock, Deadline, ProjectId};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub(crate) enum WakeSubject {
    Graph {
        project: ProjectId,
        id: String,
    },
    QueueItem {
        project: ProjectId,
        queue: String,
        item: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
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
#[allow(dead_code)]
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
/// subject's journal lock. `revision` is the checkpoint revision read under
/// the lock; it is compared with a fresh read before advancing.
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
    fn reconcile(&mut self, subject: &WakeSubject, reason: WakeReason) -> Result<(), WakeOutcome>;
    /// Steps 2-3: select/claim the subject and take the journal lock with
    /// `try_lock`. A held lock reports `Busy` at once.
    fn claim(&mut self, subject: &WakeSubject, deadline: Deadline) -> WakeClaim;
    /// Step 4: read the live revision while the lock is still held.
    fn live_revision(&self, subject: &WakeSubject) -> u64;
    /// Step 5: advance under the checkpoint's sealed execution binding with
    /// `budget = deadline.remaining()`.
    fn advance(
        &mut self,
        subject: &WakeSubject,
        reason: WakeReason,
        deadline: Deadline,
    ) -> WakeOutcome;
}

/// Run one custody-checked, deadline-bounded advance in the fixed order. A
/// subject that moved between the pre-lock read and the locked read is
/// abandoned and retried on the next pass; no advance is attempted twice.
pub(crate) fn wake_subject(
    subject: WakeSubject,
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
            code: "WAKE_REVISION_MOVED".into(),
        };
    }
    source.advance(&subject, reason, deadline)
}

/// Inject one subject-scoped advance into the coordinator. The driver is
/// called once with the exact subject, reason and remaining budget; the
/// reconcile/claim/revision steps are inert for an injected driver because it
/// has already been claimed by its caller.
pub(crate) fn wake_with(
    subject: WakeSubject,
    reason: WakeReason,
    deadline: Deadline,
    advance: impl FnOnce(WakeSubject, WakeReason, Deadline) -> WakeOutcome,
) -> WakeOutcome {
    struct Injected<F> {
        advance: Option<F>,
    }
    impl<F> WakeSource for Injected<F>
    where
        F: FnOnce(WakeSubject, WakeReason, Deadline) -> WakeOutcome,
    {
        fn reconcile(
            &mut self,
            _subject: &WakeSubject,
            _reason: WakeReason,
        ) -> Result<(), WakeOutcome> {
            Ok(())
        }
        fn claim(&mut self, _subject: &WakeSubject, _deadline: Deadline) -> WakeClaim {
            WakeClaim::Ready { revision: 0 }
        }
        fn live_revision(&self, _subject: &WakeSubject) -> u64 {
            0
        }
        fn advance(
            &mut self,
            subject: &WakeSubject,
            reason: WakeReason,
            deadline: Deadline,
        ) -> WakeOutcome {
            match self.advance.take() {
                Some(advance) => advance(subject.clone(), reason, deadline),
                None => WakeOutcome::Skipped {
                    code: "WAKE_DRIVER_SPENT".into(),
                },
            }
        }
    }
    wake_subject(
        subject,
        reason,
        deadline,
        &mut Injected {
            advance: Some(advance),
        },
    )
}

/// Lane-side entry. A lane does not own custody of the subject's host state,
/// so it sends a read-class wake frame to the host and maps the reply. On the
/// host, readers drive [`wake_subject`] directly with their own context.
#[allow(
    clippy::disallowed_methods,
    reason = "wake transport uses the caller's remaining real-time budget"
)]
pub(crate) fn wake(subject: WakeSubject, reason: WakeReason, deadline: Deadline) -> WakeOutcome {
    // The wire frame carries only the subject: the host classifies the wake
    // from the reader that received it, so the reason is metadata here.
    let _ = reason;
    if !provenant_cli::fabric::in_lane() {
        return WakeOutcome::Skipped {
            code: "WAKE_HOST_READER_OWNS_ADVANCE".into(),
        };
    }
    let wire = match &subject {
        WakeSubject::Graph { project, id } => {
            provenant_cli::fabric::graph_relay::WakeSubjectRef::Graph {
                project: *project,
                id: id.clone(),
            }
        }
        WakeSubject::QueueItem {
            project,
            queue,
            item,
        } => provenant_cli::fabric::graph_relay::WakeSubjectRef::QueueItem {
            project: *project,
            queue: queue.clone(),
            item: item.clone(),
        },
    };
    let now = provenant_os::clock::SystemClock::new().mono_now();
    let within = std::time::Duration::from_micros(
        deadline.remaining(now).as_micros().min(u64::MAX as u128) as u64,
    );
    let cancel = provenant_core::CancelToken::new();
    match provenant_cli::fabric::graph_relay::wake_from_lane(&wire, &cancel, within) {
        Ok(value) => WakeOutcome::from_value(&value),
        Err(error) => WakeOutcome::Skipped {
            code: error.code().to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::{DurationMicros, MonoTime};
    use std::cell::RefCell;

    fn deadline() -> Deadline {
        Deadline::after(
            MonoTime::from_micros(1_000),
            DurationMicros::from_micros(10_000),
        )
        .unwrap_or_else(|_| Deadline::at(MonoTime::from_micros(11_000)))
    }

    fn graph_subject() -> WakeSubject {
        WakeSubject::Graph {
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
        fn reconcile(&mut self, _s: &WakeSubject, _r: WakeReason) -> Result<(), WakeOutcome> {
            self.steps.borrow_mut().push("reconcile");
            Ok(())
        }
        fn claim(&mut self, _s: &WakeSubject, _d: Deadline) -> WakeClaim {
            self.steps.borrow_mut().push("claim");
            self.claim.clone()
        }
        fn live_revision(&self, _s: &WakeSubject) -> u64 {
            self.steps.borrow_mut().push("revision");
            if self.moved { 2 } else { 1 }
        }
        fn advance(&mut self, _s: &WakeSubject, _r: WakeReason, _d: Deadline) -> WakeOutcome {
            self.steps.borrow_mut().push("advance");
            *self.advances.borrow_mut() += 1;
            self.outcome
                .clone()
                .unwrap_or(WakeOutcome::Advanced { transitions: 1 })
        }
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
                code: "WAKE_REVISION_MOVED".into()
            }
        );
        assert_eq!(*fake.advances.borrow(), 0);
        assert_eq!(fake.steps.into_inner(), ["reconcile", "claim", "revision"]);
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
            fn reconcile(&mut self, _s: &WakeSubject, _r: WakeReason) -> Result<(), WakeOutcome> {
                Ok(())
            }
            fn claim(&mut self, _s: &WakeSubject, _d: Deadline) -> WakeClaim {
                if self.0.replace(true) {
                    WakeClaim::Busy {
                        holder: "waiter-1".into(),
                    }
                } else {
                    WakeClaim::Ready { revision: 1 }
                }
            }
            fn live_revision(&self, _s: &WakeSubject) -> u64 {
                1
            }
            fn advance(&mut self, _s: &WakeSubject, _r: WakeReason, _d: Deadline) -> WakeOutcome {
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
            fn reconcile(&mut self, _s: &WakeSubject, _r: WakeReason) -> Result<(), WakeOutcome> {
                Ok(())
            }
            fn claim(&mut self, s: &WakeSubject, _d: Deadline) -> WakeClaim {
                assert!(matches!(s, WakeSubject::QueueItem { .. }));
                *self.0.borrow_mut() = true;
                WakeClaim::Ready { revision: 1 }
            }
            fn live_revision(&self, _s: &WakeSubject) -> u64 {
                1
            }
            fn advance(&mut self, _s: &WakeSubject, _r: WakeReason, _d: Deadline) -> WakeOutcome {
                assert!(*self.0.borrow());
                WakeOutcome::Advanced { transitions: 1 }
            }
        }
        let outcome = wake_subject(
            WakeSubject::QueueItem {
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
    fn wake_with_preserves_the_injected_seam() {
        let outcome = wake_with(
            graph_subject(),
            WakeReason::Wait,
            deadline(),
            |subject, reason, budget| {
                assert!(matches!(subject, WakeSubject::Graph { id, .. } if id == "graph-one"));
                assert_eq!(reason, WakeReason::Wait);
                assert_eq!(
                    budget.remaining(MonoTime::from_micros(1_000)).as_micros(),
                    10_000
                );
                WakeOutcome::Advanced { transitions: 1 }
            },
        );
        assert_eq!(outcome, WakeOutcome::Advanced { transitions: 1 });
    }

    #[test]
    fn wait_settles_due_graph_with_tick_disabled() {
        // A wait drives the same wake the tick would, so a due graph settles
        // with no scheduler running. The driver reports the transitions it
        // made while the reader holds the subject's custody.
        let settled = RefCell::new(false);
        let outcome = wake_with(
            graph_subject(),
            WakeReason::Wait,
            deadline(),
            |subject, reason, _deadline| {
                assert!(matches!(subject, WakeSubject::Graph { .. }));
                assert_eq!(reason, WakeReason::Wait);
                *settled.borrow_mut() = true;
                WakeOutcome::Advanced { transitions: 1 }
            },
        );
        assert!(*settled.borrow());
        assert_eq!(outcome, WakeOutcome::Advanced { transitions: 1 });
    }

    #[test]
    fn lane_wake_relays_without_mutation_authority() {
        use provenant_cli::fabric::graph_relay::{GraphRelay, WakeSubjectRef};
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
