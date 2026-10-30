//! Explicit controls: cancel, resume and reconcile. None of them guesses at
//! process state; uncertain custody stays fenced and is reported as such.
use crate::{artifacts::read_sealed, launch::Providers, *};
use provenant_core::{AttemptId, OperationId, RunId, WallTime};
use provenant_supervisor::{
    census::{ProcessObserver, SystemProcessObserver},
    custody::{self, CustodyError, Observation, RecoveryState, Scope, StopReceipt},
    machine::StopReason,
};
use rusqlite::params;
use serde::Serialize;

#[cfg(test)]
thread_local! {
    pub(crate) static HOLDERLESS_BEFORE_EXCLUSION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    pub(crate) static REBOOT_TEMPORARY_CLEANUP: std::cell::RefCell<Option<std::result::Result<Option<Reconciled>, String>>> = const { std::cell::RefCell::new(None) };
}

/// What a cancel request achieved. Requested does not prove exit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CancelReceipt {
    /// Queued work ended without reaching a provider.
    Cancelled,
    AlreadyTerminal,
    Requested,
    AlreadyRequested,
    /// No launch summary or custody to address; reconcile first.
    Unobservable {
        reason: String,
    },
}
/// A continuation of one terminal attempt with new input, as a new run.
pub struct Resume {
    pub prior: AttemptId,
    pub expected_generation: u64,
    pub operation_id: OperationId,
    pub run_id: RunId,
    pub attempt_id: AttemptId,
    pub prompt: String,
    pub created_at: WallTime,
}
/// How long past its runtime deadline a live guardian is left to act on its own
/// before `reconcile` asks it to stop.
const DEADLINE_GRACE_SECONDS: u64 = 120;

fn past_runtime_deadline(record: &AttemptRecord, call: &Request<'_>) -> bool {
    call.clock
        .wall_now()
        .is_ok_and(|now| overdue_at(record, now))
}

/// Whether a running attempt is past its runtime deadline plus the grace.
pub fn overdue_at(record: &AttemptRecord, now: WallTime) -> bool {
    let (AttemptState::Running { .. }, Some(deadline)) = (&record.state, record.runtime_deadline)
    else {
        return false;
    };
    provenant_core::DurationMicros::from_seconds(DEADLINE_GRACE_SECONDS)
        .and_then(|grace| deadline.checked_add(grace))
        .is_ok_and(|limit| now >= limit)
}

/// Whether a running attempt's guardian and provider are both gone with no
/// exit recorded, so nothing is left to settle it. Read-only; unreadable
/// state counts as not stale. `root` is the execution state root.
pub fn holder_absent(root: &std::path::Path, record: &AttemptRecord) -> bool {
    if !matches!(record.state, AttemptState::Running { .. }) {
        return false;
    }
    let path = root.join(format!(
        "runs/{}/attempts/{}",
        record.run_id, record.attempt_id
    ));
    let Ok(attempt) = provenant_os::fs::Directory::open_anchor(&path) else {
        return false;
    };
    matches!(
        read_sealed(&attempt, "process-exit.json", RECORD_LIMIT),
        Ok(None)
    ) && custody::holder_gone(&attempt, RunStore::scope(record), &SystemProcessObserver)
}

/// What reconcile found for one attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Reconciled {
    /// Not yet handed off; `start` launches it.
    Queued,
    /// A live guardian still holds custody.
    Held,
    /// A live guardian outlived its attempt's runtime deadline, so a stop at
    /// the deadline was requested of it. It settles the attempt as a timeout.
    DeadlineStop,
    /// The supervisor is resuming durable settlement in the background.
    Settling { phase: String },
    /// A durable exit was committed and released.
    Finished,
    /// Leftover temporary data was removed from a terminal attempt.
    TemporaryRemoved,
    /// The attempt is terminal, but temporary data could not be removed.
    TemporaryRetained {
        path: std::path::PathBuf,
        reason: TemporaryReason,
    },
    /// Launch never reached a guardian; ended as rejected.
    Withdrawn,
    /// Cleanup ended with these descendants possibly alive (for example one
    /// that left the process group); custody stays fenced until they exit.
    Escaped { pids: Vec<i64> },
    /// Custody cannot be proven either way; it stays fenced.
    Uncertain { reason: UncertainReason },
}
impl std::fmt::Display for Reconciled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Queued => f.write_str("queued"),
            Self::Held => f.write_str("held"),
            Self::DeadlineStop => f.write_str("deadline_stop"),
            Self::Settling { phase } => write!(f, "settling ({phase})"),
            Self::Finished => f.write_str("finished"),
            Self::TemporaryRemoved => f.write_str("temporary_removed"),
            Self::TemporaryRetained { path, reason } => {
                write!(f, "temporary_retained {}: {reason}", path.display())
            }
            Self::Withdrawn => f.write_str("withdrawn"),
            Self::Escaped { pids } => write!(
                f,
                "escaped: {}",
                pids.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Uncertain { reason } => write!(f, "uncertain: {reason}"),
        }
    }
}
/// Stable reason for retaining terminal temporary data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TemporaryReasonKind {
    Error,
    ReservationUnreadable,
    ReservationRetained,
    NotPrivate,
}

/// A typed reason that remains a string in reconcile output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct TemporaryReason {
    text: String,
    #[serde(skip)]
    pub kind: TemporaryReasonKind,
}
impl TemporaryReason {
    fn new(kind: TemporaryReasonKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
        }
    }
}
impl From<String> for TemporaryReason {
    fn from(text: String) -> Self {
        Self::new(TemporaryReasonKind::Error, text)
    }
}
impl From<&str> for TemporaryReason {
    fn from(text: &str) -> Self {
        Self::from(text.to_owned())
    }
}
impl std::ops::Deref for TemporaryReason {
    type Target = str;
    fn deref(&self) -> &str {
        &self.text
    }
}
impl std::fmt::Display for TemporaryReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

/// Why custody could not be proven. The text of an underlying error rides
/// along as `error`; the kind is what a caller matches on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UncertainReason {
    /// Reconciling the attempt failed before it could be settled.
    ReconcileFailed { error: String },
    /// Reboot evidence could not be established; the wire kind is unchanged.
    #[serde(rename = "reconcile_failed")]
    RebootProofUnavailable {
        error: String,
        #[serde(skip)]
        cause: RebootProofCause,
    },
    /// Custody could not be reattached, acquired or observed.
    CustodyUnavailable { error: String },
    /// A running attempt has no launch summary.
    NoLaunchSummary,
    /// The launch custody is neither held nor provably absent.
    CustodyNotProvablyAbsent,
    /// A process exit exists although no launch was recorded.
    ExitWithoutLaunchSummary,
    /// The guardian is neither alive nor finished as far as can be seen.
    GuardianNotObservable,
    /// A launch was handed to a writer whose custody is unconfirmed.
    WriterCustodyUnconfirmed { reservation: std::path::PathBuf },
    /// A writer reservation outlived the attempt's terminal commit.
    WriterReservationRetained { reservation: std::path::PathBuf },
}
/// The source of an unavailable reboot proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RebootProofCause {
    Filesystem,
    Other,
}

/// Causes of an attempt lost during custody recovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LostReason {
    Holderless,
    RemovedWorktree,
    StaleReservation,
}
impl LostReason {
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::Holderless => {
                "no live holder: the guardian and provider ended before the attempt settled"
            }
            Self::RemovedWorktree => {
                "machine rebooted before settlement and the writer worktree was removed"
            }
            Self::StaleReservation => {
                "writer reservation was gone or stale and the recorded process tree predates this boot"
            }
        }
    }
}
impl RebootProofCause {
    fn from_error(error: &RunError) -> Self {
        if matches!(error, RunError::Filesystem(_)) {
            Self::Filesystem
        } else {
            Self::Other
        }
    }
}
impl std::fmt::Display for UncertainReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReconcileFailed { error }
            | Self::RebootProofUnavailable { error, .. }
            | Self::CustodyUnavailable { error } => f.write_str(error),
            Self::NoLaunchSummary => f.write_str("running attempt has no launch summary"),
            Self::CustodyNotProvablyAbsent => f.write_str("launch custody is not provably absent"),
            Self::ExitWithoutLaunchSummary => f.write_str("exit exists without a launch summary"),
            Self::GuardianNotObservable => f.write_str("guardian is not observable"),
            Self::WriterCustodyUnconfirmed { reservation } => write!(
                f,
                "writer custody is unconfirmed; reservation retained at {}; chair recovery required",
                reservation.display()
            ),
            Self::WriterReservationRetained { reservation } => write!(
                f,
                "writer reservation retained after terminal commit: {}; chair recovery required",
                reservation.display()
            ),
        }
    }
}

/// How long recovery waits after each of TERM and KILL for an orphan to go.
const ORPHAN_PATIENCE: std::time::Duration = std::time::Duration::from_secs(2);

#[cfg(test)]
mod reboot_tests {
    use super::*;

    #[test]
    fn reader_refusal_published_before_recovery_exclusion_is_not_lost()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let context = crate::tests::context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let id = run.attempts[0].attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let record = store.record(id, &call)?;
        let scope = RunStore::scope(&record);
        let attempt = store.attempt_dir(run.run_id, id, false)?;
        let path = context
            .state_root
            .as_path()
            .join(format!("runs/{}/attempts/{id}", run.run_id));
        drop(custody::acquire_reader(
            attempt.try_clone()?,
            scope,
            call.lock_budget(),
            &cancel,
            &clock,
        )?);
        let reservation = path.join(id.to_string()).join("reservation.json");
        let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&reservation)?)?;
        value["launch_state"] = "spawned".into();
        value["boot"] = "darwin:00000000-0000-0000-0000-000000000000".into();
        value["guardian"] = serde_json::json!({
            "boot": "darwin:00000000-0000-0000-0000-000000000000",
            "pid": 123, "native_start": 1, "group_id": 123, "session_id": 123
        });
        let meta = attempt.metadata()?;
        value["attempt_device"] = meta.device.into();
        value["attempt_inode"] = meta.inode.into();
        std::fs::write(&reservation, serde_json::to_vec(&value)?)?;
        crate::artifacts::seal(&attempt, "launch.json", b"{}", RECORD_LIMIT)?;
        // Simulate the guardian's refusal while it owns writer exclusion, in
        // the precise gap after the first holder check and before acquisition.
        let hook_path = path.clone();
        let hook_reservation = reservation.clone();
        HOLDERLESS_BEFORE_EXCLUSION.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                let dir = provenant_os::fs::Directory::open_anchor(&hook_path).unwrap();
                let _lease = dir
                    .try_lock(
                        std::ffi::OsStr::new("writer.lock"),
                        provenant_os::fs::LockMode::Exclusive,
                    )
                    .unwrap();
                crate::artifacts::seal(&dir, "process-exit.json", b"refusal", RECORD_LIMIT)
                    .unwrap();
                let mut current: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&hook_reservation).unwrap()).unwrap();
                current["launch_state"] = "settling".into();
                current["unresolved_cleanup"] = false.into();
                current["census_incomplete"] = false.into();
                std::fs::write(&hook_reservation, serde_json::to_vec(&current).unwrap()).unwrap();
            }))
        });
        assert_eq!(
            store.settle_holderless(&record, &call, &SystemProcessObserver)?,
            None
        );
        assert_eq!(store.record(id, &call)?, record);
        assert!(reservation.exists());
        let recovery =
            custody::acquire_recovery(attempt, scope, call.lock_budget(), &cancel, &clock)?;
        assert_eq!(recovery.observe(), RecoveryState::Absent);
        Ok(())
    }

    #[test]
    fn reboot_settlement_error_propagates() -> std::result::Result<(), Box<dyn std::error::Error>> {
        use provenant_supervisor::census::{
            Census, CensusError, ProcessObservation, ProcessObserver,
        };
        struct Reboot;
        impl ProcessObserver for Reboot {
            fn boot_identity(&self) -> std::result::Result<String, CensusError> {
                Ok("darwin:11111111-1111-1111-1111-111111111111".into())
            }
            fn observe(&self, _: i64) -> ProcessObservation {
                unreachable!()
            }
            fn census(&self, _: usize) -> std::result::Result<Census, CensusError> {
                unreachable!()
            }
        }
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let context = crate::tests::context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let id = run.attempts[0].attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let record = store.record(id, &call)?;
        let attempt = store.attempt_dir(run.run_id, id, false)?;
        let scope = RunStore::scope(&record);
        attempt.atomic_write(std::ffi::OsStr::new("process.json"), ".checkpoint-", &serde_json::to_vec(&serde_json::json!({
            "schema_version": 1, "scope": scope,
            "guardian": { "boot": "darwin:00000000-0000-0000-0000-000000000000", "pid": 100, "native_start": 1, "group_id": 100, "session_id": 100 },
            "provider": { "boot": "darwin:00000000-0000-0000-0000-000000000000", "pid": 101, "native_start": 2, "group_id": 101, "session_id": 100 }
        }))?, provenant_os::fs::PublishMode::Replace)?;
        let mut next = record.clone();
        next.generation += 1;
        next.revision += 1;
        store.write(&call, |tx| {
            crate::admission::update_attempt(tx, &record, &next)
        })?;
        let result = store.settle_rebooted(&record, &call, &Reboot);
        assert!(
            matches!(
                result,
                Err(RunError::Validation("process observation scope"))
            ),
            "{result:?}"
        );
        Ok(())
    }

    #[test]
    fn unreadable_exit_is_not_reboot_proof() -> std::result::Result<(), Box<dyn std::error::Error>>
    {
        unavailable_reboot_evidence(false)
    }

    #[test]
    fn corrupt_reservation_is_not_reboot_proof()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        unavailable_reboot_evidence(true)
    }

    #[test]
    fn claim_records_the_runtime_deadline_and_a_host_cap_tightens_it()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        for (cap, seconds) in [(0, 3600), (600, 600)] {
            let temp = tempfile::tempdir()?;
            let clock = crate::tests::clock();
            let cancel = provenant_core::CancelToken::new();
            let call = crate::tests::call(&clock, &cancel);
            let context = crate::tests::context(temp.path())?;
            let store = RunStore::open(&context, &call)?;
            let run = crate::tests::input();
            store.create(&run, &call)?;
            let id = run.attempts[0].attempt_id;
            assert_eq!(store.record(id, &call)?.runtime_deadline, None);
            store.mark_starting_with_priority(
                &store.record(id, &call)?,
                &crate::tests::route(),
                &[],
                &Default::default(),
                None,
                None,
                cap,
                &call,
            )?;
            let claimed = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
            let started = claimed.started_at.ok_or("no start")?;
            let expected =
                started.checked_add(provenant_core::DurationMicros::from_seconds(seconds)?)?;
            assert_eq!(claimed.runtime_deadline, Some(expected));
        }
        Ok(())
    }

    #[test]
    fn reap_queues_recovery_for_running_and_cancelled_starting_attempts_only()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let context = crate::tests::context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let id = run.attempts[0].attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let spawned = std::cell::Cell::new(0);
        let spawn = |_: &[std::ffi::OsString], _: &std::path::Path| {
            spawned.set(spawned.get() + 1);
            Ok(())
        };
        assert_eq!(
            store.reap_with(&call, spawn)?,
            0,
            "an uncancelled start waits"
        );
        let attempt = store.attempt_dir(run.run_id, id, false)?;
        let scope = RunStore::scope(&store.record(id, &call)?);
        let request = serde_json::to_vec(&serde_json::json!({"scope": scope}))?;
        crate::artifacts::seal(&attempt, "cancel.json", &request, RECORD_LIMIT)?;
        assert_eq!(
            store.reap_with(&call, spawn)?,
            1,
            "a cancelled start is reaped"
        );
        assert_eq!(spawned.get(), 1);
        store.reconcile_with_observer(
            run.run_id,
            &crate::dispatch_tests::Fake::default(),
            &call,
            &SystemProcessObserver,
        )?;
        assert!(matches!(
            store.record(id, &call)?.state,
            AttemptState::Terminal {
                outcome: Outcome::Cancelled,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn a_running_attempt_with_no_guardian_is_stale_and_reaped()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let context = crate::tests::context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let id = run.attempts[0].attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let record = store.record(id, &call)?;
        assert!(holder_absent(context.state_root.as_path(), &record));
        let spawned = std::cell::Cell::new(0);
        let reaped = store.reap_with(&call, |_, _| {
            spawned.set(spawned.get() + 1);
            Ok(())
        })?;
        assert_eq!((reaped, spawned.get()), (1, 1));
        Ok(())
    }

    #[test]
    fn only_a_running_attempt_past_its_deadline_and_grace_is_overdue()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let mut clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let deadline = provenant_core::Clock::wall_now(&clock)?;
        let mut record: AttemptRecord = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "attempt_id": provenant_core::AttemptId::from_bytes([1; 16]),
            "run_id": provenant_core::RunId::from_bytes([2; 16]),
            "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
            "task_id": provenant_core::TaskId::from_bytes([4; 16]),
            "operation_id": "overdue-test",
            "attempt_number": 1, "revision": 1, "generation": 0,
            "requested_route": {"kind":"model", "provider":"p", "model":"m"},
            "authority_digest": provenant_core::Digest::from_bytes([5; 32]),
            "queued_at": deadline,
            "state": {"state":"running"},
            "usage": {},
            "prompt": {"path":{"encoding":"utf8", "path":"prompt.txt"},
                "digest": provenant_records::CanonicalV1::file_digest(b""), "bytes":0, "complete":true, "redacted":false}
        }))?;
        record.runtime_deadline = Some(deadline);
        let overdue = |clock: &provenant_core::TestClock, record: &AttemptRecord| {
            past_runtime_deadline(record, &crate::tests::call(clock, &cancel))
        };
        assert!(
            !overdue(&clock, &record),
            "at the deadline the guardian acts"
        );
        clock.advance(provenant_core::DurationMicros::from_seconds(
            DEADLINE_GRACE_SECONDS - 1,
        )?)?;
        assert!(!overdue(&clock, &record));
        clock.advance(provenant_core::DurationMicros::from_seconds(1)?)?;
        assert!(overdue(&clock, &record));
        record.runtime_deadline = None;
        assert!(!overdue(&clock, &record), "no recorded deadline, no stop");
        record.runtime_deadline = Some(deadline);
        record.state = AttemptState::Starting {
            extra: Default::default(),
        };
        assert!(!overdue(&clock, &record));
        Ok(())
    }

    fn unavailable_reboot_evidence(
        corrupt_reservation: bool,
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let context = crate::tests::context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let id = run.attempts[0].attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let path = context
            .state_root
            .as_path()
            .join(format!("runs/{}/attempts/{id}", run.run_id));
        let evidence = path.join(id.to_string());
        std::fs::create_dir_all(&evidence)?;
        let reservation = evidence.join("reservation.json");
        let cause = if corrupt_reservation {
            std::fs::write(&reservation, b"corrupt reservation")?;
            RebootProofCause::Other
        } else {
            std::fs::create_dir(path.join("process-exit.json"))?;
            RebootProofCause::Filesystem
        };
        let before = std::fs::read(&reservation).ok();
        assert!(matches!(store.settle_rebooted(
            &store.record(id, &call)?,
            &call,
            &SystemProcessObserver
        )?, Some(Reconciled::Uncertain { reason: UncertainReason::RebootProofUnavailable { cause: found, .. } })
            if found == cause));
        assert_eq!(store.cancel(id, "stop", &call)?, CancelReceipt::Requested);
        let request: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path.join("cancel.json"))?)?;
        assert_eq!(
            request["scope"],
            serde_json::to_value(RunStore::scope(&store.record(id, &call)?))?
        );
        assert!(matches!(store.settle_rebooted(
            &store.record(id, &call)?,
            &call,
            &SystemProcessObserver
        )?, Some(Reconciled::Uncertain { reason: UncertainReason::RebootProofUnavailable { cause: found, .. } })
            if found == cause));
        assert_eq!(std::fs::read(&reservation).ok(), before);
        assert_eq!(
            store.record(id, &call)?.state,
            AttemptState::Running {
                extra: Default::default()
            }
        );
        Ok(())
    }
}

#[cfg(test)]
mod lineage_tests {
    use super::*;
    use provenant_confinement::worktrees::{
        self, WorktreeCreate, WorktreeGcPolicy, WorktreeOrigin, WorktreeOwner,
    };
    use provenant_core::Clock;
    use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
    use std::{ffi::OsString, time::Duration};

    #[test]
    fn resume_preserves_explicit_unowned_and_adopted_worktrees()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let mut execution = crate::tests::context(temp.path())?;
        let project = temp.path().join("checkout");
        let home = temp.path().join("home");
        let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
        inputs.state_dir = Some(execution.state_root.as_path().into());
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        std::fs::create_dir_all(roots.state.as_path())?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            roots.state.as_path(),
            std::fs::Permissions::from_mode(0o700),
        )?;
        let git = provenant_os::git::git_executable().ok_or("git")?;
        for args in [
            vec!["init", "-q"],
            vec![
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--allow-empty",
                "-qm",
                "Base",
            ],
        ] {
            let argv: Vec<OsString> = std::iter::once(git.as_os_str().to_owned())
                .chain(args.into_iter().map(OsString::from))
                .collect();
            let outcome = SystemExecutor::new(1)?.execute(ExecOptions::new(
                &argv,
                &project,
                Duration::from_secs(5),
            ))?;
            assert_eq!(outcome.termination, Termination::Exit(0));
        }
        let clock = provenant_os::clock::SystemClock;
        let cancel = provenant_core::CancelToken::new();
        for (index, origin) in [None, Some(WorktreeOrigin::Adopted)]
            .into_iter()
            .enumerate()
        {
            let mut original = crate::tests::input();
            original.run_id = RunId::from_bytes([60 + index as u8; 16]);
            original.operation_id = format!("explicit-{index}").parse()?;
            original.attempts[0].attempt_id = AttemptId::from_bytes([70 + index as u8; 16]);
            let prior = original.attempts[0].attempt_id;
            let (project_id, info) =
                worktrees::with_context(&roots, &home, &project, &clock, &cancel, |context| {
                    let info = worktrees::create(
                        context,
                        WorktreeCreate {
                            branch: format!("explicit-{index}"),
                            base: None,
                        },
                        &cancel,
                    )?;
                    if let Some(origin) = origin {
                        let tree =
                            provenant_confinement::verify_worktree(context.repo, &info.path)?;
                        worktrees::save_owner(
                            &tree,
                            &WorktreeOwner {
                                attempt_id: None,
                                operation_id: None,
                                branch: info
                                    .branch
                                    .clone()
                                    .ok_or(worktrees::WorktreeError::NotFound)?,
                                base: info.head.to_string(),
                                host: provenant_core::HostId::from_bytes([3; 16]),
                                created_at: original.created_at,
                                origin,
                                label: None,
                            },
                        )?;
                    }
                    Ok((context.binding.project_id(), info))
                })?;
            execution.project_id = project_id;
            let call = Request {
                clock: &clock,
                cancel: &cancel,
            };
            let store = RunStore::open(&execution, &call)?;
            let writer = WriterRequest {
                extra: Default::default(),
                project: project.clone(),
                worktree: info.path.clone(),
            };
            original.request.tasks[0].writer = Some(writer.clone());
            store.create(&original, &call)?;
            let record = store.record(prior, &call)?;
            crate::prepare_writer(&writer, &store.writer_registry, project_id)?.snapshot(
                store.context.state_root.as_path(),
                &store.attempt_dir(record.run_id, prior, false)?,
                RunStore::scope(&record),
            )?;
            std::fs::write(info.path.join("pending.txt"), "keep these edits")?;
            store.withdraw(
                prior,
                AttemptState::Queued {
                    extra: Default::default(),
                },
                Outcome::Cancelled,
                "fixture",
                clock.wall_now()?,
                &call,
            )?;
            let next = Resume {
                prior,
                expected_generation: 0,
                operation_id: format!("explicit-resume-{index}").parse()?,
                run_id: RunId::from_bytes([80 + index as u8; 16]),
                attempt_id: AttemptId::from_bytes([90 + index as u8; 16]),
                prompt: "Continue".into(),
                created_at: clock.wall_now()?,
            };
            let resumed = store.resume_fresh(&next, "no provider session", &call)?;
            assert_eq!(
                store.request(resumed.run_id, &call)?.tasks[0]
                    .writer
                    .as_ref(),
                Some(&writer)
            );
            assert_eq!(
                std::fs::read_to_string(info.path.join("pending.txt"))?,
                "keep these edits"
            );
            let tree = crate::prepare_writer(&writer, &store.writer_registry, project_id)?;
            store.check_writer_lineage(
                &store.record(next.attempt_id, &call)?,
                &tree.worktree,
                &call,
            )?;
            worktrees::save_owner(
                &tree.worktree,
                &WorktreeOwner {
                    attempt_id: Some(AttemptId::from_bytes([100; 16])),
                    operation_id: Some("foreign".parse()?),
                    branch: format!("explicit-{index}"),
                    base: info.head.to_string(),
                    host: provenant_core::HostId::from_bytes([3; 16]),
                    created_at: original.created_at,
                    origin: WorktreeOrigin::Lane,
                    label: None,
                },
            )?;
            assert!(matches!(
                store.check_continuation_writer(next.attempt_id, &call),
                Err(RunError::Validation(
                    "continuation worktree belongs to another lane"
                ))
            ));
        }
        Ok(())
    }

    #[test]
    fn resume_refuses_a_pruned_worktree_reused_by_another_lane()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let mut execution = crate::tests::context(temp.path())?;
        let project = temp.path().join("checkout");
        let home = temp.path().join("home");
        let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
        inputs.state_dir = Some(execution.state_root.as_path().into());
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        std::fs::create_dir_all(roots.state.as_path())?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            roots.state.as_path(),
            std::fs::Permissions::from_mode(0o700),
        )?;
        let git = provenant_os::git::git_executable().ok_or("git")?;
        for args in [
            vec!["init", "-q"],
            vec![
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--allow-empty",
                "-qm",
                "Base",
            ],
        ] {
            let argv: Vec<OsString> = std::iter::once(git.as_os_str().to_owned())
                .chain(args.into_iter().map(OsString::from))
                .collect();
            let outcome = SystemExecutor::new(1)?.execute(ExecOptions::new(
                &argv,
                &project,
                Duration::from_secs(5),
            ))?;
            assert_eq!(outcome.termination, Termination::Exit(0));
        }
        let clock = provenant_os::clock::SystemClock;
        let cancel = provenant_core::CancelToken::new();
        let mut original = crate::tests::input();
        original.request.tasks[0].deliver = Some(WriterDeliveryRequest {
            extra: Default::default(),
            mode: DeliveryMode::Pr,
            remote: "origin".into(),
            branch: "old-lane".into(),
            open_pr: true,
        });
        let prior = original.attempts[0].attempt_id;
        let (project_id, info) =
            worktrees::with_context(&roots, &home, &project, &clock, &cancel, |context| {
                let info = worktrees::create(
                    context,
                    WorktreeCreate {
                        branch: "lane".into(),
                        base: None,
                    },
                    &cancel,
                )?;
                let tree = provenant_confinement::verify_worktree(context.repo, &info.path)?;
                worktrees::save_owner(
                    &tree,
                    &WorktreeOwner {
                        attempt_id: Some(prior),
                        operation_id: Some(original.operation_id.clone()),
                        branch: "lane".into(),
                        base: info.head.to_string(),
                        host: provenant_core::HostId::from_bytes([3; 16]),
                        created_at: original.created_at,
                        origin: WorktreeOrigin::Lane,
                        label: None,
                    },
                )?;
                Ok((context.binding.project_id(), info))
            })?;
        execution.project_id = project_id;
        let call = Request {
            clock: &clock,
            cancel: &cancel,
        };
        let store = RunStore::open(&execution, &call)?;
        original.request.tasks[0].writer = Some(WriterRequest {
            extra: Default::default(),
            project: project.clone(),
            worktree: info.path.clone(),
        });
        store.create(&original, &call)?;
        store.withdraw(
            prior,
            AttemptState::Queued {
                extra: Default::default(),
            },
            Outcome::Cancelled,
            "fixture",
            clock.wall_now()?,
            &call,
        )?;
        let launch = crate::prepare_writer(
            original.request.tasks[0].writer.as_ref().unwrap(),
            &store.writer_registry,
            project_id,
        )?;
        let record = store.record(prior, &call)?;
        launch.snapshot(
            store.context.state_root.as_path(),
            &store.attempt_dir(record.run_id, prior, false)?,
            RunStore::scope(&record),
        )?;
        let owner = worktrees::load_owner(&launch.worktree)?.ok_or("owner")?;
        std::fs::remove_file(
            launch
                .worktree
                .repo()
                .private_git_dir()
                .join("provenant-owner.json"),
        )?;
        assert!(matches!(
            store.check_continuation_writer(prior, &call),
            Err(RunError::Validation(
                "continuation worktree has no lane owner"
            ))
        ));
        worktrees::save_owner(
            &launch.worktree,
            &WorktreeOwner {
                origin: WorktreeOrigin::Adopted,
                ..owner.clone()
            },
        )?;
        assert!(matches!(
            store.check_continuation_writer(prior, &call),
            Err(RunError::Validation(
                "continuation worktree has no lane owner"
            ))
        ));
        worktrees::save_owner(&launch.worktree, &owner)?;
        assert!(store.check_continuation_writer(prior, &call)?.is_some());
        let first = Resume {
            prior,
            expected_generation: 0,
            operation_id: "queued-resume".parse()?,
            run_id: RunId::from_bytes([30; 16]),
            attempt_id: AttemptId::from_bytes([31; 16]),
            prompt: "Continue".into(),
            created_at: clock.wall_now()?,
        };
        store.resume_fresh(&first, "fixture", &call)?;
        store.withdraw(
            first.attempt_id,
            AttemptState::Queued {
                extra: Default::default(),
            },
            Outcome::Cancelled,
            "fixture",
            clock.wall_now()?,
            &call,
        )?;
        worktrees::with_context(&roots, &home, &project, &clock, &cancel, |context| {
            worktrees::prune_owned(
                context,
                &info.path,
                prior,
                info.head,
                info.head,
                &WorktreeGcPolicy {
                    upstream: Some(info.head),
                    terminal: &|_| true,
                    no_live_lanes: &|_| true,
                    regenerable: &[],
                    keep: &[],
                    written_before_ns: i128::MAX,
                    foreign_merged_clean: false,
                    foreign_idle_before_ns: None,
                    dry_run: false,
                },
                &cancel,
            )?;
            let replacement = worktrees::create(
                context,
                WorktreeCreate {
                    branch: "lane".into(),
                    base: None,
                },
                &cancel,
            )?;
            assert_eq!(replacement.path, info.path);
            let tree = provenant_confinement::verify_worktree(context.repo, &replacement.path)?;
            worktrees::save_owner(
                &tree,
                &WorktreeOwner {
                    attempt_id: Some(AttemptId::from_bytes([40; 16])),
                    operation_id: Some("foreign".parse().unwrap()),
                    branch: "lane".into(),
                    base: replacement.head.to_string(),
                    host: provenant_core::HostId::from_bytes([3; 16]),
                    created_at: original.created_at,
                    origin: WorktreeOrigin::Lane,
                    label: None,
                },
            )?;
            let queued = store.record(first.attempt_id, &call).unwrap();
            assert!(matches!(
                store.check_writer_lineage(&queued, &tree, &call),
                Err(RunError::Validation(
                    "continuation worktree belongs to another lane"
                ))
            ));
            Ok(())
        })?;
        let next = Resume {
            prior: first.attempt_id,
            operation_id: "new-resume".parse()?,
            run_id: RunId::from_bytes([50; 16]),
            attempt_id: AttemptId::from_bytes([51; 16]),
            ..first
        };
        assert!(matches!(
            store.resume(&next, &call),
            Err(RunError::Validation(
                "continuation worktree belongs to another lane"
            ))
        ));
        assert!(matches!(
            store.resume_fresh(&next, "fixture", &call),
            Err(RunError::Validation(
                "continuation worktree belongs to another lane"
            ))
        ));
        let replacement =
            worktrees::with_context(&roots, &home, &project, &clock, &cancel, |context| {
                let replacement = worktrees::create(
                    context,
                    WorktreeCreate {
                        branch: "replacement".into(),
                        base: Some(info.head.to_string()),
                    },
                    &cancel,
                )?;
                let tree = provenant_confinement::verify_worktree(context.repo, &replacement.path)?;
                worktrees::save_owner(
                    &tree,
                    &WorktreeOwner {
                        attempt_id: Some(next.attempt_id),
                        operation_id: Some(next.operation_id.clone()),
                        branch: "replacement".into(),
                        base: replacement.head.to_string(),
                        host: provenant_core::HostId::from_bytes([3; 16]),
                        created_at: next.created_at,
                        origin: WorktreeOrigin::Lane,
                        label: None,
                    },
                )?;
                Ok(replacement)
            })?;
        let writer = WriterRequest {
            extra: Default::default(),
            project: project.clone(),
            worktree: replacement.path,
        };
        let accepted =
            store.resume_fresh_in(&next, "original worktree reassigned", &writer, &call)?;
        assert_eq!(
            store.request(accepted.run_id, &call)?.tasks[0]
                .writer
                .as_ref(),
            Some(&writer)
        );
        assert_eq!(
            store.request(accepted.run_id, &call)?.tasks[0]
                .deliver
                .as_ref()
                .unwrap()
                .branch,
            "replacement"
        );
        assert!(info.path.exists(), "foreign lane must retain its checkout");
        Ok(())
    }
}

impl Reconciled {
    fn uncertain(reason: UncertainReason) -> Self {
        Self::Uncertain { reason }
    }
    fn custody_unavailable(error: &dyn std::fmt::Display) -> Self {
        Self::uncertain(UncertainReason::CustodyUnavailable {
            error: error.to_string(),
        })
    }
}

impl RunStore {
    /// A previous boot ended every member of this attempt's process tree.
    /// Keep both exclusions until the canonical commit precedes fence release.
    fn settle_rebooted(
        &self,
        record: &AttemptRecord,
        call: &Request<'_>,
        observer: &dyn ProcessObserver,
    ) -> Result<Option<Reconciled>> {
        if matches!(record.state, AttemptState::Queued { .. }) {
            return Ok(None);
        }
        if Self::needs_lost_settlement(record)
            && let Some(result) = self.settle_unreserved_loss(record, call, observer, true)?
        {
            return Ok(Some(result));
        }
        // Collection failures are uncertain; settlement errors still propagate.
        let collect = || -> Result<_> {
            let attempt = self.attempt_dir(record.run_id, record.attempt_id, false)?;
            if !matches!(record.state, AttemptState::Terminal { .. })
                && read_sealed(&attempt, "process-exit.json", RECORD_LIMIT)?.is_some()
            {
                // A durable exit retains its canonical completion path and facts.
                return Ok(None);
            }
            // Recovery needs the persisted binding, not a fresh Git inspection of
            // the worktree the interrupted writer left behind.
            let request = self.request(record.run_id, call)?;
            let writer = request
                .tasks
                .iter()
                .find(|task| task.task_id == record.task_id)
                .and_then(|task| task.writer.as_ref());
            let (directory, expected_slot) = match writer {
                Some(writer) => {
                    let expected = self
                        .writer_registry
                        .join("worktrees")
                        .join(self.context.project_id.to_string())
                        .join(provenant_confinement::worktrees::state_dir_name(
                            &writer.worktree,
                        ));
                    let readable = self
                        .writer_fences()?
                        .into_iter()
                        .find(|fence| fence.scope == Self::scope(record));
                    let slot = match readable {
                        Some(fence) => fence
                            .custody_directory()
                            .ok_or_else(|| {
                                RunError::unsafe_path(
                                    fence.reservation.clone(),
                                    PathCheck::Containment,
                                )
                            })?
                            .to_path_buf(),
                        None => expected.clone(),
                    };
                    match provenant_os::fs::Directory::open_anchor(&slot) {
                        Ok(directory) => (directory, slot == expected),
                        Err(error) if error.is_not_found() => return Ok(None),
                        Err(error) => return Err(error.into()),
                    }
                }
                None => (attempt.try_clone()?, true),
            };
            let recovery = match custody::acquire_recovery(
                directory.try_clone()?,
                Self::scope(record),
                provenant_core::WaitBudget::nonblocking(call.clock.mono_now()),
                call.cancel,
                call.clock,
            ) {
                Ok(recovery) => recovery,
                Err(CustodyError::Filesystem(error))
                    if error.is_not_found()
                        || matches!(error, provenant_os::fs::FsError::LockTimeout { .. }) =>
                {
                    return Ok(None);
                }
                Err(CustodyError::InvalidRecord) if expected_slot => {
                    #[cfg(test)]
                    let recovered = provenant_supervisor::fixture::acquire_reboot_recovery_with(
                        directory,
                        &attempt,
                        Self::scope(record),
                        provenant_core::WaitBudget::nonblocking(call.clock.mono_now()),
                        call.cancel,
                        call.clock,
                        observer,
                    );
                    #[cfg(not(test))]
                    let recovered = custody::acquire_reboot_recovery(
                        directory,
                        &attempt,
                        Self::scope(record),
                        provenant_core::WaitBudget::nonblocking(call.clock.mono_now()),
                        call.cancel,
                        call.clock,
                    );
                    recovered.map_err(|error| RunError::Io(std::io::Error::other(format!(
                        "reservation cannot be verified after reboot: {error}; retain custody and inspect the reservation and boot identity before chair recovery"
                    ))))?
                }
                Err(error) => return Err(RunError::Io(std::io::Error::other(error.to_string()))),
            };
            #[cfg(test)]
            let proof = provenant_supervisor::fixture::after_reboot_with(
                recovery,
                &attempt,
                Self::scope(record),
                observer,
            );
            #[cfg(not(test))]
            let proof = recovery.after_reboot(&attempt, Self::scope(record));
            let proof =
                proof.map_err(|error| RunError::Io(std::io::Error::other(error.to_string())))?;
            Ok(proof.map(|proof| (attempt, proof)))
        };
        let (attempt, proof) = match collect() {
            Ok(Some(collected)) => collected,
            Ok(None) => return Ok(None),
            Err(error) => {
                return Ok(Some(Reconciled::uncertain(
                    UncertainReason::RebootProofUnavailable {
                        cause: RebootProofCause::from_error(&error),
                        error: format!("reboot proof unavailable: {error}"),
                    },
                )));
            }
        };
        if !Self::needs_lost_settlement(record) {
            self.settle_recovered(&attempt, proof.into_recovery())?;
            // Recheck custody after release before removing lock-named payloads.
            let cleanup = self.reconcile_terminal_temporary(record, call, observer);
            #[cfg(test)]
            REBOOT_TEMPORARY_CLEANUP.with(|result| {
                *result.borrow_mut() = Some(cleanup.as_ref().cloned().map_err(ToString::to_string));
            });
            let _ = cleanup;
        } else {
            // Lost without exit evidence deliberately preserves scratch.
            let request = self.request(record.run_id, call)?;
            let missing_worktree = request
                .tasks
                .iter()
                .find(|task| task.task_id == record.task_id)
                .and_then(|task| task.writer.as_ref())
                .is_some_and(crate::writer::worktree_removed);
            if missing_worktree {
                self.commit_lost(proof.scope(), LostReason::RemovedWorktree.message(), call)?;
            } else {
                self.commit_reboot_lost(&proof, call)?;
            }
            proof
                .release()
                .map_err(|error| RunError::Io(std::io::Error::other(error.to_string())))?;
        }
        if matches!(
            record.state,
            AttemptState::Terminal {
                outcome: Outcome::Lost,
                exit_code: None,
                ..
            }
        ) && record.exit_evidence.is_none()
        {
            self.set_temporary_retained(record.attempt_id, false, call)?;
        }
        Ok(Some(Reconciled::Finished))
    }
    fn needs_lost_settlement(record: &AttemptRecord) -> bool {
        !matches!(record.state, AttemptState::Terminal { .. })
            || (matches!(
                record.state,
                AttemptState::Terminal {
                    outcome: Outcome::Lost,
                    exit_code: None,
                    ..
                }
            ) && record.exit_evidence.is_none())
    }

    /// Writer exclusion for an attempt, taken without waiting: failure means a
    /// guardian (or an inheritor of its lease) still holds custody.
    fn orphan_recovery(
        &self,
        record: &AttemptRecord,
        attempt: provenant_os::fs::Directory,
        call: &Request<'_>,
    ) -> Result<custody::Recovery> {
        let request = self.request(record.run_id, call)?;
        let task = request
            .tasks
            .iter()
            .find(|task| task.task_id == record.task_id)
            .ok_or(RunError::Validation("recovery task missing"))?;
        let directory = match &task.writer {
            Some(writer) => self.writer_custody_directory(record, writer)?,
            None => attempt,
        };
        custody::acquire_recovery(
            directory,
            Self::scope(record),
            provenant_core::WaitBudget::nonblocking(call.clock.mono_now()),
            call.cancel,
            call.clock,
        )
        .map_err(|e| RunError::Io(std::io::Error::other(e.to_string())))
    }
    /// Settle an attempt whose guardian and provider are both gone in this
    /// boot and left no durable exit: nothing remains to import or publish, so
    /// it ends Lost with a reason instead of waiting for a holder that is not
    /// coming. Writer exclusion is taken without waiting, so a live guardian,
    /// or an inheritor of its lease, keeps the attempt open.
    fn settle_holderless(
        &self,
        record: &AttemptRecord,
        call: &Request<'_>,
        observer: &dyn ProcessObserver,
    ) -> Result<Option<Reconciled>> {
        let scope = Self::scope(record);
        let attempt = self.attempt_dir(record.run_id, record.attempt_id, false)?;
        if read_sealed(&attempt, "process-exit.json", RECORD_LIMIT)?.is_some()
            || !custody::holder_gone(&attempt, scope, observer)
        {
            return Ok(None);
        }
        // Name the slot from the recorded binding; a lost writer's repository
        // need not be readable for its custody to be settled.
        let request = self.request(record.run_id, call)?;
        let writer = request
            .tasks
            .iter()
            .find(|task| task.task_id == record.task_id)
            .and_then(|task| task.writer.as_ref());
        let directory = match writer {
            Some(writer) => {
                let slot = self
                    .writer_registry
                    .join("worktrees")
                    .join(self.context.project_id.to_string())
                    .join(provenant_confinement::worktrees::state_dir_name(
                        &writer.worktree,
                    ));
                match provenant_os::fs::Directory::open_anchor(&slot) {
                    Ok(directory) => directory,
                    Err(error) if error.is_not_found() => return Ok(None),
                    Err(error) => return Err(error.into()),
                }
            }
            None => attempt,
        };
        #[cfg(test)]
        HOLDERLESS_BEFORE_EXCLUSION.with(|hook| {
            if let Some(hook) = hook.borrow_mut().take() {
                hook();
            }
        });
        let Ok(recovery) = custody::acquire_recovery(
            directory,
            scope,
            provenant_core::WaitBudget::nonblocking(call.clock.mono_now()),
            call.cancel,
            call.clock,
        ) else {
            return Ok(None);
        };
        // A reader can publish its exit and release exclusion after the first
        // check, so both proofs must still hold under exclusion.
        let attempt = self.attempt_dir(record.run_id, record.attempt_id, false)?;
        if recovery.scope() != scope
            || read_sealed(&attempt, "process-exit.json", RECORD_LIMIT)?.is_some()
            || !custody::holder_gone(&attempt, scope, observer)
            || recovery.observe_with(observer) != RecoveryState::Absent
        {
            return Ok(None);
        }
        self.commit_lost(scope, LostReason::Holderless.message(), call)?;
        recovery
            .release_lost()
            .map_err(|error| RunError::Io(std::io::Error::other(error.to_string())))?;
        self.set_temporary_retained(record.attempt_id, false, call)?;
        Ok(Some(Reconciled::Finished))
    }
    /// Settle an unreserved attempt only with proof its process tree is gone.
    fn settle_unreserved_loss(
        &self,
        record: &AttemptRecord,
        call: &Request<'_>,
        observer: &dyn ProcessObserver,
        proof_errors_uncertain: bool,
    ) -> Result<Option<Reconciled>> {
        let collect = || -> Result<_> {
            let attempt = self.attempt_dir(record.run_id, record.attempt_id, false)?;
            // The slot is named from the worktree path, so it needs no Git access.
            let request = self.request(record.run_id, call)?;
            let writer = request
                .tasks
                .iter()
                .find(|task| task.task_id == record.task_id)
                .and_then(|task| task.writer.as_ref());
            let slot = writer.map(|writer| {
                self.writer_registry
                    .join("worktrees")
                    .join(self.context.project_id.to_string())
                    .join(provenant_confinement::worktrees::state_dir_name(
                        &writer.worktree,
                    ))
            });
            let custody = match &slot {
                Some(slot) => match provenant_os::fs::Directory::open_anchor(slot) {
                    Ok(directory) => directory,
                    Err(error) if error.is_not_found() => return Ok(None),
                    Err(error) => return Err(error.into()),
                },
                None => attempt.try_clone()?,
            };
            let _lease = match custody.try_lock(
                std::ffi::OsStr::new("writer.lock"),
                provenant_os::fs::LockMode::Exclusive,
            ) {
                Ok(lease) => lease,
                Err(provenant_os::fs::FsError::LockTimeout { .. }) => return Ok(None),
                Err(error) => return Err(error.into()),
            };
            let stale = match custody::reservation_present(&custody, Self::scope(record)) {
                Ok(false) => false,
                Ok(true) => {
                    let Some(slot) = &slot else {
                        return Ok(None);
                    };
                    let path = slot
                        .join(record.attempt_id.to_string())
                        .join("reservation.json");
                    let scope = Self::scope(record);
                    if !self.writer_fences()?.iter().any(|fence| {
                        fence.reservation == path
                            && fence.scope.run == scope.run
                            && fence.scope.attempt == scope.attempt
                            && fence.scope.generation < scope.generation
                    }) {
                        return Ok(None);
                    }
                    true
                }
                Err(error) => return Err(error.into()),
            };
            if !stale && matches!(record.state, AttemptState::Terminal { .. }) {
                return Ok(None);
            }
            if read_sealed(&attempt, "process-exit.json", RECORD_LIMIT)?.is_some() {
                return Ok(None);
            }
            if !custody::checkpoint_before_reboot(&attempt, Self::scope(record), observer) {
                return Ok(None);
            }
            let missing_worktree = writer.is_some_and(crate::writer::worktree_removed);
            Ok(Some((custody, _lease, stale, missing_worktree)))
        };
        let (custody, _lease, stale, missing_worktree) = match collect() {
            Ok(Some(proof)) => proof,
            Ok(None) => return Ok(None),
            Err(error) if proof_errors_uncertain => {
                return Ok(Some(Reconciled::uncertain(
                    UncertainReason::RebootProofUnavailable {
                        cause: RebootProofCause::from_error(&error),
                        error: format!("reboot proof unavailable: {error}"),
                    },
                )));
            }
            Err(error) => return Err(error),
        };
        self.commit_lost(
            Self::scope(record),
            if missing_worktree {
                LostReason::RemovedWorktree.message()
            } else {
                LostReason::StaleReservation.message()
            },
            call,
        )?;
        if stale {
            let evidence = custody::attempt_evidence(&custody, Self::scope(record))?;
            evidence.unlink(std::ffi::OsStr::new("reservation.json"))?;
            evidence.sync()?;
        }
        self.set_temporary_retained(record.attempt_id, false, call)?;
        Ok(Some(Reconciled::Finished))
    }
    pub(crate) fn scope(record: &AttemptRecord) -> Scope {
        Scope {
            run: record.run_id,
            attempt: record.attempt_id,
            generation: record.generation,
        }
    }
    /// Bind a cancel operation to its target before any attempt is touched.
    /// The same ID for the same target is a replay; for another target it is
    /// an operation conflict.
    pub fn bind_cancel(
        &self,
        operation: &OperationId,
        target: &str,
        call: &Request<'_>,
    ) -> Result<()> {
        call.check()?;
        let (project, actor) = (
            self.context.project_id.to_string(),
            self.context.actor_id.to_string(),
        );
        let bound: String = {
            self.db.execute(
                "INSERT INTO cancellations VALUES (?1,?2,?3,?4) ON CONFLICT DO NOTHING",
                params![project, actor, operation.as_str(), target],
            )?;
            self.db.query_row(
                "SELECT target FROM cancellations WHERE project=?1 AND actor=?2 AND operation=?3",
                params![project, actor, operation.as_str()],
                |r| r.get(0),
            )?
        };
        if bound != target {
            return Err(RunError::OperationConflict);
        }
        Ok(())
    }
    /// Cancel one attempt. Queued work is withdrawn; started work gets a stop
    /// request. A handoff that has not yet published its launch summary is
    /// retried within the call budget.
    /// Stop a parent's children on its explicit stop, returning each failure.
    /// Every child handed to a guardian stops from the scope held at
    /// hand-off, including one start launched after the parent's scan read
    /// its snapshot. The lane-writable record decides only for `accepted`
    /// children, with the generation the scan expected, that never reached
    /// a guardian.
    pub fn stop_children(
        &self,
        accepted: std::collections::BTreeSet<(AttemptId, u64)>,
        reason: &str,
        call: &Request<'_>,
    ) -> Vec<String> {
        let held = self.held_attempts();
        let unheld = accepted
            .into_iter()
            .filter(|(id, _)| !held.contains(id))
            .map(|(id, generation)| (id, Some(generation)));
        let mut errors = Vec::new();
        for (id, generation) in held.iter().map(|id| (*id, None)).chain(unheld) {
            if let Some(generation) = generation {
                let record = match self.record(id, call) {
                    Ok(record) => record,
                    Err(error) => {
                        errors.push(error.to_string());
                        continue;
                    }
                };
                if matches!(record.state, AttemptState::Terminal { .. }) {
                    continue;
                }
                let still_queued = matches!(record.state, AttemptState::Queued { .. })
                    && record.generation.checked_add(1) == Some(generation);
                if record.generation != generation && !still_queued {
                    errors.push("accepted child generation changed".into());
                    continue;
                }
            }
            match self.cancel(id, reason, call) {
                Ok(CancelReceipt::Unobservable { reason }) => errors.push(reason),
                Err(error) => errors.push(error.to_string()),
                Ok(_) => {}
            }
        }
        errors
    }
    // Cancellation coordinates with a real guardian handoff under a hard cap.
    pub fn cancel(&self, id: AttemptId, reason: &str, call: &Request<'_>) -> Result<CancelReceipt> {
        self.cancel_with_observer(id, reason, call, &SystemProcessObserver)
    }
    pub(crate) fn cancel_with_observer(
        &self,
        id: AttemptId,
        reason: &str,
        call: &Request<'_>,
        _observer: &dyn ProcessObserver,
    ) -> Result<CancelReceipt> {
        // A child this host handed to a guardian stops through the directory,
        // scope and requester held then. The lane can rewrite the child's
        // record and launch summary, so neither decides whether or for whom
        // it stops.
        if let Some(held) = self
            .launched
            .borrow()
            .iter()
            .rev()
            .find(|held| held.scope.attempt == id)
        {
            let Some(handle) = &held.handle else {
                // The latest handle is dropped only after observing guardian
                // exit. Earlier superseded generations cannot be selected.
                return Ok(CancelReceipt::AlreadyTerminal);
            };
            let stopped = custody::stop(handle, StopReason::ExplicitCancellation)
                .map_err(|error| error.to_string());
            return Ok(match stopped {
                Ok(StopReceipt::Requested) => CancelReceipt::Requested,
                Ok(StopReceipt::AlreadyRequested) => CancelReceipt::AlreadyRequested,
                Err(reason) => CancelReceipt::Unobservable { reason },
            });
        }
        loop {
            let record = self.record(id, call)?;
            match record.state {
                AttemptState::Terminal { .. } => return Ok(CancelReceipt::AlreadyTerminal),
                AttemptState::Queued { .. } => {
                    let now = call
                        .clock
                        .wall_now()
                        .map_err(|_| RunError::Validation("wall clock"))?;
                    match self.withdraw(
                        id,
                        AttemptState::Queued {
                            extra: Default::default(),
                        },
                        Outcome::Cancelled,
                        reason,
                        now,
                        call,
                    ) {
                        Ok(Some(_)) => return Ok(CancelReceipt::Cancelled),
                        Ok(None) => return Ok(CancelReceipt::AlreadyTerminal),
                        // A concurrent start took it; it is no longer queued.
                        Err(RunError::Conflict) => continue,
                        Err(error) => return Err(error),
                    }
                }
                _ => {}
            }
            let dir = self.attempt_dir(record.run_id, id, false)?;
            let stopped = match custody::reattach(dir.try_clone()?, Self::scope(&record)) {
                Ok(handle) => custody::stop(&handle, StopReason::ExplicitCancellation),
                Err(CustodyError::Filesystem(error)) if error.is_not_found() => {
                    custody::request_stop(
                        dir,
                        Self::scope(&record),
                        record.verified_owner.unwrap_or(self.context.actor_id),
                        StopReason::ExplicitCancellation,
                    )
                }
                Err(error) => Err(error),
            };
            return Ok(match stopped {
                Ok(StopReceipt::Requested) => CancelReceipt::Requested,
                Ok(StopReceipt::AlreadyRequested) => CancelReceipt::AlreadyRequested,
                Err(error) => CancelReceipt::Unobservable {
                    reason: error.to_string(),
                },
            });
        }
    }

    /// Accept a continuation run. Each terminal attempt can be continued
    /// once; `expected_generation` is 0 before that and 1 after. The route,
    /// access and timeout come from the prior task, never from the caller.
    pub fn resume(&self, input: &Resume, call: &Request<'_>) -> Result<RunRecord> {
        self.resume_mode(input, call, None, None, None)
            .map(|(run, _)| run)
    }
    /// Same lineage, route, authority and writer, without provider continuation.
    pub fn resume_fresh(
        &self,
        input: &Resume,
        reason: &str,
        call: &Request<'_>,
    ) -> Result<RunRecord> {
        self.resume_mode(input, call, Some(reason), None, None)
            .map(|(run, _)| run)
    }
    /// A fresh fallback may replace a vanished or reassigned writer only with
    /// a new worktree owned by this continuation.
    pub fn resume_fresh_in(
        &self,
        input: &Resume,
        reason: &str,
        writer: &WriterRequest,
        call: &Request<'_>,
    ) -> Result<RunRecord> {
        let branch = self.fresh_writer_branch(input, writer)?;
        self.resume_mode(input, call, Some(reason), Some((writer, &branch)), None)
            .map(|(run, _)| run)
    }
    /// Prepare against the canonical continuation request before recording it.
    /// Replays retain their accepted IDs and use the ordinary launch path.
    pub fn resume_fresh_prepared(
        &self,
        input: &Resume,
        reason: &str,
        writer: Option<&WriterRequest>,
        providers: &dyn Providers,
        call: &Request<'_>,
    ) -> Result<(RunRecord, Option<PreparedStart>)> {
        let branch = writer
            .map(|writer| self.fresh_writer_branch(input, writer))
            .transpose()?;
        self.resume_mode(
            input,
            call,
            Some(reason),
            writer.zip(branch.as_deref()),
            Some(providers),
        )
    }
    fn fresh_writer_branch(&self, input: &Resume, writer: &WriterRequest) -> Result<String> {
        let launch = crate::prepare_writer(writer, &self.writer_registry, self.context.project_id)?;
        let owner = provenant_confinement::worktrees::load_owner(&launch.worktree)?.ok_or(
            RunError::Validation("fresh fallback needs an owned worktree"),
        )?;
        if owner.origin != provenant_confinement::worktrees::WorktreeOrigin::Lane
            || owner.operation_id.as_ref() != Some(&input.operation_id)
            || owner.attempt_id != Some(input.attempt_id)
        {
            return Err(RunError::Validation(
                "fresh fallback worktree has another owner",
            ));
        }
        Ok(owner.branch)
    }
    /// Admission checks ownership as well as the path. A later handoff checks
    /// again under custody so prune and reuse cannot redirect a queued resume.
    pub fn check_continuation_writer(
        &self,
        prior: AttemptId,
        call: &Request<'_>,
    ) -> Result<Option<provenant_confinement::worktrees::WorktreeOwner>> {
        let prior = self.record(prior, call)?;
        let request = self.request(prior.run_id, call)?;
        let task = request
            .tasks
            .iter()
            .find(|task| task.task_id == prior.task_id)
            .ok_or(RunError::Validation("prior task missing from request"))?;
        let Some(writer) = &task.writer else {
            return Ok(None);
        };
        let launch = crate::prepare_writer(writer, &self.writer_registry, prior.project_id)?;
        self.check_writer_lineage(&prior, &launch.worktree, call)
    }
    pub(crate) fn check_writer_lineage(
        &self,
        record: &AttemptRecord,
        tree: &provenant_confinement::VerifiedWorktree,
        call: &Request<'_>,
    ) -> Result<Option<provenant_confinement::worktrees::WorktreeOwner>> {
        use provenant_confinement::worktrees::{WorktreeOrigin, load_owner};
        let owner = load_owner(tree)?;
        let lane = owner
            .as_ref()
            .filter(|owner| owner.origin == WorktreeOrigin::Lane);
        let mut current = record.clone();
        let mut seen = std::collections::HashSet::new();
        let mut was_owned = false;
        loop {
            if !seen.insert(current.attempt_id) || seen.len() > 128 {
                return Err(RunError::Validation("continuation parent chain"));
            }
            if lane.is_some_and(|owner| {
                owner.attempt_id == Some(current.attempt_id)
                    || owner.operation_id.as_ref() == Some(&current.operation_id)
            }) {
                return Ok(owner);
            }
            let directory = self.attempt_dir(current.run_id, current.attempt_id, false)?;
            if let Some(raw) = read_sealed(&directory, "writer-owned.json", crate::RECORD_LIMIT)? {
                // Recorded under custody before launch. A later owner removal or
                // adoption cannot turn an owned lane into an explicit tree.
                was_owned |= crate::db::decode::<crate::writer::WriterOwnership>(&raw)?.lane_owned;
            }
            let Some(parent) = current.retry_of else {
                break;
            };
            current = self.record(parent, call)?;
        }
        if lane.is_some() {
            return Err(RunError::Validation(
                "continuation worktree belongs to another lane",
            ));
        }
        if was_owned {
            return Err(RunError::Validation(
                "continuation worktree has no lane owner",
            ));
        }
        // Explicit unowned and adopted trees retain their existing lifecycle.
        Ok(owner)
    }
    fn resume_mode(
        &self,
        input: &Resume,
        call: &Request<'_>,
        fresh: Option<&str>,
        writer: Option<(&WriterRequest, &str)>,
        providers: Option<&dyn Providers>,
    ) -> Result<(RunRecord, Option<PreparedStart>)> {
        let prior = self.record(input.prior, call)?;
        if !matches!(prior.state, AttemptState::Terminal { .. }) {
            return Err(RunError::Nonterminal);
        }
        let operation = self.operation(&input.operation_id, call)?;
        let replay = operation.is_some();
        let previous = match &operation {
            Some(operation) => match self.request(operation.run_id, call) {
                Ok(request) => Some(request),
                Err(RunError::NotFound) if operation.state == OperationState::Prepared => None,
                Err(error) => return Err(error),
            },
            None => None,
        };
        if !replay && writer.is_none() {
            self.check_continuation_writer(input.prior, call)?;
        }
        let _retention = if replay || fresh.is_some() {
            None
        } else {
            Some(self.retain_provider_state(input.prior, call)?)
        };
        let request = self.request(prior.run_id, call)?;
        let task = request
            .tasks
            .iter()
            .find(|t| t.task_id == prior.task_id)
            .ok_or(RunError::Validation("prior task missing from request"))?;
        let mut deliver = task.deliver.clone();
        if let Some((_, branch)) = writer
            && let Some(prior) = &deliver
            && matches!(prior.mode, DeliveryMode::Push | DeliveryMode::Pr)
        {
            let mut destination = HostDeliveryContext::destination(prior.mode.clone(), branch)?
                .ok_or(RunError::Validation("fresh fallback delivery destination"))?;
            destination.remote = prior.remote.clone();
            destination.open_pr |= prior.open_pr;
            deliver = Some(destination);
        }
        let mut prepared = PreparedRun {
            run_id: input.run_id,
            operation_id: input.operation_id.clone(),
            request: CanonicalRequest {
                extra: Default::default(),
                child_attempt_id_version: previous
                    .as_ref()
                    .map_or(request.parent_attempt.map(|_| 1), |request| {
                        request.child_attempt_id_version
                    }),
                chair: request.chair.clone(),
                fresh_continuation: fresh.map(str::to_owned),
                schema_version: 1,
                tasks: vec![TaskRequest {
                    context: None,
                    extra: Default::default(),
                    build_heavy: task.build_heavy,
                    title: None,
                    base: task.base.clone(),
                    head: task.head.clone(),
                    base_ref: task.base_ref.clone(),
                    profile: task.profile.clone(),
                    task_id: prior.task_id,
                    prompt_digest: provenant_records::CanonicalV1::file_digest(
                        input.prompt.as_bytes(),
                    ),
                    route: prior.requested_route.clone(),
                    account: task.account.clone(),
                    fallback: vec![],
                    authority_digest: prior.authority_digest,
                    writer: writer
                        .map(|(writer, _)| writer.clone())
                        .or_else(|| task.writer.clone()),
                    deliver,
                    provider_timeout_seconds: task.provider_timeout_seconds,
                    delivery_id: prior.delivery_id,
                    access: task.access.clone(),
                }],
                concurrency: 1,
                parent: Some(input.prior),
                lane_depth: request.lane_depth,
                parent_attempt: request.parent_attempt,
            },
            attempts: vec![NewAttempt {
                attempt_id: input.attempt_id,
                task_id: prior.task_id,
                prompt: input.prompt.clone(),
            }],
            created_at: input.created_at,
        };
        // Retain an interrupted legacy operation's scheme even if it crashed
        // before publishing its request. Only the exact old digest qualifies.
        if previous.is_none()
            && let Some(operation) = operation
            && operation.request_digest != prepared.request.digest()?
        {
            let mut legacy = prepared.request.clone();
            legacy.child_attempt_id_version = None;
            if legacy.digest()? == operation.request_digest {
                prepared.request = legacy;
            }
        }
        if let Some(run) = crate::correlation::child_run_id(
            self.context.project_id,
            &prepared.request,
            &prepared.operation_id,
        )? {
            prepared.run_id = run;
            prepared.attempts[0].attempt_id = crate::correlation::child_attempt_id(
                self.context.project_id,
                run,
                &prepared.request,
                prior.task_id,
            )?
            .ok_or(RunError::Validation("derived continuation attempt ID"))?;
        }
        // A replay, prepared or accepted, is answered by `create`, which
        // compares the request digest and adopts a prepared operation's IDs.
        if !replay {
            let generation: i64 = {
                self.db.query_row(
                    "SELECT COUNT(*) FROM continuations WHERE target=?1",
                    [input.prior.to_string()],
                    |r| r.get(0),
                )?
            };
            if u64::try_from(generation).ok() != Some(input.expected_generation) {
                return Err(RunError::GenerationConflict {
                    current_generation: u64::try_from(generation)
                        .map_err(|_| RunError::Conflict)?,
                });
            }
        }
        // Derived final paths are deterministic. Preflight in this call's
        // fresh staging instead, so an unrecorded directory left by a crash
        // cannot block retry. Its plan is discarded and launch prepares again.
        let derived = prepared.request.child_attempt_id_version.is_some();
        let mut ready = if !replay && derived {
            if let Some(providers) = providers {
                let staging = PreparedRun {
                    run_id: input.run_id,
                    operation_id: prepared.operation_id.clone(),
                    request: prepared.request.clone(),
                    attempts: vec![NewAttempt {
                        attempt_id: input.attempt_id,
                        task_id: prior.task_id,
                        prompt: input.prompt.clone(),
                    }],
                    created_at: input.created_at,
                };
                drop(self.prepare_fresh_start(&staging, providers, call)?);
            }
            None
        } else if !replay {
            providers
                .map(|providers| self.prepare_fresh_start(&prepared, providers, call))
                .transpose()?
        } else {
            None
        };
        let created = self.create(&prepared, call);
        if let Some(ready) = &mut ready {
            let recorded = match &created {
                Ok(run) => run.run_id == prepared.run_id,
                // Publication may fail after reserving the operation. Keep
                // its staging for recovery, including an unreadable index.
                Err(_) => self
                    .operation(&input.operation_id, call)
                    .map(|operation| operation.is_some_and(|op| op.run_id == prepared.run_id))
                    .unwrap_or(true),
            };
            if recorded {
                self.retain_prepared_start(ready, prepared.run_id);
            }
        }
        let run = created?;
        self.contribution_diagnostic(
            run.run_id,
            self.inherit_contribution(
                input.prior,
                run.run_id,
                writer.map(|(_, branch)| branch),
                call,
            ),
        );
        if run.run_id != prepared.run_id {
            ready = None;
        }
        Ok((run, ready))
    }

    /// Up to `limit` runs needing recovery or terminal temporary cleanup in
    /// this project. Nonterminal work takes priority over temporary cleanup.
    pub fn open_runs(&self, limit: u64, call: &Request<'_>) -> Result<Vec<RunId>> {
        call.check()?;
        let limit = crate::db::sql_int(limit)?;
        let ids: Vec<String> = {
            let mut query = self.db.prepare(
                "SELECT run FROM attempts WHERE project=?1 AND terminal=0 GROUP BY run ORDER BY MIN(sequence) LIMIT ?2",
            )?;
            let rows = query
                .query_map(params![self.context.project_id.to_string(), limit], |r| {
                    r.get(0)
                })?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let mut runs = ids
            .iter()
            .map(|id| RunId::parse(id).map_err(|_| RunError::Validation("run index ID")))
            .collect::<Result<Vec<_>>>()?;
        for fence in self.writer_fences()? {
            if !runs.contains(&fence.scope.run) && runs.len() < limit as usize {
                runs.push(fence.scope.run);
            }
        }
        if runs.len() < limit as usize {
            let mut query = self.db.prepare(
                "SELECT run FROM attempts WHERE project=?1 AND terminal=1 AND tmp_retained=1 GROUP BY run ORDER BY MIN(sequence) LIMIT ?2",
            )?;
            let mut rows = query.query(params![self.context.project_id.to_string(), limit])?;
            while runs.len() < limit as usize {
                let Some(row) = rows.next()? else { break };
                let run = RunId::parse(&row.get::<_, String>(0)?)
                    .map_err(|_| RunError::Validation("run index ID"))?;
                if runs.contains(&run) {
                    continue;
                }
                runs.push(run);
            }
        }
        // A closed run whose reactions are still owed needs a drain too.
        if runs.len() < limit as usize {
            let owed: Vec<String> = {
                let mut query = self.db.prepare(
                    "SELECT run FROM reactions WHERE project=?1 AND pending=1 GROUP BY run ORDER BY MIN(rowid) LIMIT ?2",
                )?;
                let rows = query
                    .query_map(params![self.context.project_id.to_string(), limit], |r| {
                        r.get(0)
                    })?;
                rows.collect::<std::result::Result<_, _>>()?
            };
            for id in owed {
                let run = RunId::parse(&id).map_err(|_| RunError::Validation("run index ID"))?;
                if !runs.contains(&run) && runs.len() < limit as usize {
                    runs.push(run);
                }
            }
        }
        Ok(runs)
    }

    /// Queue recovery for each run of this project that holds a running
    /// attempt, or a starting attempt with a cancel request, so a lane whose
    /// guardian died is settled by the next read instead of waiting for a
    /// `reconcile`. Queuing already leaves a live holder alone and takes no
    /// writer lock. Best effort: a run that cannot be queued is skipped.
    pub fn reap(&self, call: &Request<'_>) -> Result<usize> {
        self.reap_with(call, provenant_os::exec::spawn_detached)
    }
    pub(crate) fn reap_with(
        &self,
        call: &Request<'_>,
        spawn: impl Fn(
            &[std::ffi::OsString],
            &std::path::Path,
        ) -> std::result::Result<(), provenant_os::exec::ExecError>,
    ) -> Result<usize> {
        let runs: Vec<String> = {
            let mut query = self
                .db
                .prepare("SELECT DISTINCT run FROM attempts WHERE project=?1 AND terminal=0")?;
            let rows =
                query.query_map(params![self.context.project_id.to_string()], |r| r.get(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let mut queued = 0;
        for run in runs {
            let Ok(run) = RunId::parse(&run) else {
                continue;
            };
            let ids: Vec<String> = {
                let mut query = self.db.prepare(
                    "SELECT id FROM attempts WHERE run=?1 AND project=?2 AND terminal=0",
                )?;
                let rows = query.query_map(
                    params![run.to_string(), self.context.project_id.to_string()],
                    |row| row.get(0),
                )?;
                rows.collect::<std::result::Result<_, _>>()?
            };
            let reapable = ids
                .iter()
                .filter_map(|id| {
                    AttemptId::parse(id)
                        .ok()
                        .and_then(|id| self.record(id, call).ok())
                })
                .any(|attempt| match attempt.state {
                    AttemptState::Running { .. } => true,
                    AttemptState::Starting { .. } => self
                        .attempt_dir(run, attempt.attempt_id, false)
                        .and_then(|dir| read_sealed(&dir, "cancel.json", RECORD_LIMIT))
                        .is_ok_and(|found| found.is_some()),
                    _ => false,
                });
            if reapable && self.queue_reconcile_with(run, call, &spawn).is_ok() {
                queued += 1;
            }
        }
        Ok(queued)
    }
    /// Check every attempt of `run` against supervisor custody.
    /// A durable exit is committed and released; a launch that provably never
    /// reached a guardian is withdrawn. Terminal temporary data is retried
    /// only after reservations are released; removal failures are reported.
    pub fn reconcile(
        &self,
        run: RunId,
        providers: &dyn Providers,
        call: &Request<'_>,
    ) -> Result<Vec<(AttemptId, Reconciled)>> {
        let _ = providers;
        self.queue_reconcile(run, call)
    }
    /// Execute the supervisor recovery entry with fixture-owned providers.
    /// Installed builds expose only the asynchronous public control.
    #[cfg(feature = "fixture")]
    pub fn reconcile_for_fixture(
        &self,
        run: RunId,
        providers: &dyn Providers,
        call: &Request<'_>,
    ) -> Result<Vec<(AttemptId, Reconciled)>> {
        self.reconcile_with_observer(run, providers, call, &SystemProcessObserver)
    }
    pub(crate) fn reconcile_with_observer(
        &self,
        run: RunId,
        providers: &dyn Providers,
        call: &Request<'_>,
        observer: &dyn ProcessObserver,
    ) -> Result<Vec<(AttemptId, Reconciled)>> {
        let mut ids: Vec<String> = {
            let mut query = self
                .db
                .prepare("SELECT id FROM attempts WHERE run=?1 AND project=?2 AND (terminal=0 OR tmp_retained=1) ORDER BY sequence")?;
            let rows = query.query_map(
                params![run.to_string(), self.context.project_id.to_string()],
                |r| r.get(0),
            )?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let (fences, listing_error) = match self.writer_fences() {
            Ok(fences) => (fences, None),
            Err(error) => {
                for id in self.pending_reactions(run, call)? {
                    let id = id.to_string();
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
                if ids.is_empty() {
                    return Err(error);
                }
                (Vec::new(), Some(error.to_string()))
            }
        };
        for fence in fences {
            if fence.scope.run == run && !ids.contains(&fence.scope.attempt.to_string()) {
                ids.push(fence.scope.attempt.to_string());
            }
        }
        let mut found = Vec::new();
        for id in ids {
            let id = AttemptId::parse(&id).map_err(|_| RunError::Validation("attempt index ID"))?;
            let mut state = self
                .reconcile_attempt(id, providers, call, observer)
                .unwrap_or_else(|error| {
                    Some(Reconciled::uncertain(UncertainReason::ReconcileFailed {
                        error: error.to_string(),
                    }))
                });
            if let Some(error) = &listing_error
                && let Some(
                    uncertain @ (Reconciled::Uncertain { .. }
                    | Reconciled::TemporaryRetained { .. }),
                ) = &state
            {
                state = Some(Reconciled::uncertain(UncertainReason::ReconcileFailed {
                    error: format!("{uncertain}; writer fence listing failed: {error}"),
                }));
            }
            if let Some(state) = state {
                found.push((id, self.track_reconcile_progress(run, id, state, call)));
            }
        }
        Ok(found)
    }
    /// Progress is advisory: a broken attempt must not hide its peers' results.
    fn track_reconcile_progress(
        &self,
        run: RunId,
        id: AttemptId,
        state: Reconciled,
        call: &Request<'_>,
    ) -> Reconciled {
        let progress = (|| {
            let scope = Self::scope(&self.record(id, call)?);
            let directory = self.attempt_dir(run, id, false)?;
            if let Reconciled::Uncertain { reason } = &state {
                crate::settlement::blocked(&directory, scope, &reason.to_string(), call.clock)
            } else {
                crate::settlement::clear_blocked(&directory, scope)
            }
        })();
        if let Err(error) = progress {
            let outcome = match &state {
                Reconciled::Uncertain { reason } => reason.to_string(),
                _ => state.to_string(),
            };
            return Reconciled::uncertain(UncertainReason::ReconcileFailed {
                error: format!("{outcome}; settlement progress update failed: {error}"),
            });
        }
        state
    }
    fn withdraw_prepared_writer(&self, fence: &WriterFence, call: &Request<'_>) -> Result<bool> {
        let directory = fence.custody_directory().ok_or_else(|| {
            RunError::unsafe_path(fence.reservation.clone(), PathCheck::Containment)
        })?;
        let recovery = custody::acquire_recovery(
            provenant_os::fs::Directory::open_anchor(directory)?,
            fence.scope,
            provenant_core::WaitBudget::nonblocking(call.clock.mono_now()),
            call.cancel,
            call.clock,
        );
        Ok(match recovery {
            Ok(recovery) if recovery.scope() == fence.scope => recovery.withdraw_prepared().is_ok(),
            _ => false,
        })
    }
    fn settle_never_spawned_writer(
        &self,
        record: &AttemptRecord,
        fence: &WriterFence,
        call: &Request<'_>,
    ) -> Result<Option<Reconciled>> {
        let scope = Self::scope(record);
        let attempt = self.attempt_dir(record.run_id, record.attempt_id, false)?;
        if fence.scope != scope
            || read_sealed(&attempt, "process-exit.json", RECORD_LIMIT)?.is_some()
        {
            return Ok(None);
        }
        let directory = fence.custody_directory().ok_or_else(|| {
            RunError::unsafe_path(fence.reservation.clone(), PathCheck::Containment)
        })?;
        let recovery = custody::acquire_recovery(
            provenant_os::fs::Directory::open_anchor(directory)?,
            scope,
            provenant_core::WaitBudget::nonblocking(call.clock.mono_now()),
            call.cancel,
            call.clock,
        );
        let Ok(recovery) = recovery else {
            return Ok(None);
        };
        if !recovery
            .never_spawned(&attempt)
            .map_err(|error| RunError::Io(std::io::Error::other(error)))?
        {
            return Ok(None);
        }
        // No canonical exit exists to import the writer's private head.
        // Lost-without-exit retains it through both cleanup and GC. Repeating
        // this transaction also finishes a release interrupted after commit.
        self.commit_lost(
            scope,
            "launch interrupted: provider never spawned; private evidence retained",
            call,
        )?;
        recovery
            .release_lost()
            .map_err(|error| RunError::Io(std::io::Error::other(error)))?;
        self.set_temporary_retained(record.attempt_id, false, call)?;
        Ok(Some(Reconciled::Finished))
    }
    /// Settle an attempt that has no launch summary. Only a reservation or
    /// handoff that provably never spawned is withdrawn; a held custody lock
    /// or a live provider leaves it untouched and uncertain.
    fn settle_unlaunched(
        &self,
        id: AttemptId,
        record: &AttemptRecord,
        call: &Request<'_>,
    ) -> Result<Reconciled> {
        let scope = Self::scope(record);
        let dir = || self.attempt_dir(record.run_id, id, false);
        let cancelled = read_sealed(&dir()?, "cancel.json", RECORD_LIMIT)?
            .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
            .is_some_and(|request| {
                request.get("scope") == serde_json::to_value(scope).ok().as_ref()
            });
        if !matches!(record.state, AttemptState::Starting { .. }) {
            return Ok(Reconciled::uncertain(UncertainReason::NoLaunchSummary));
        }
        if let Some(fence) = self
            .writer_fences()?
            .into_iter()
            .find(|fence| fence.scope == scope)
        {
            if self.withdraw_prepared_writer(&fence, call)? {
                let now = call
                    .clock
                    .wall_now()
                    .map_err(|_| RunError::Validation("wall clock"))?;
                self.withdraw(
                    id,
                    AttemptState::Starting {
                        extra: Default::default(),
                    },
                    if cancelled {
                        Outcome::Cancelled
                    } else {
                        Outcome::Rejected
                    },
                    if cancelled {
                        "cancelled before writer launch"
                    } else {
                        "launch interrupted: writer preparation never reached a guardian"
                    },
                    now,
                    call,
                )?;
                return Ok(Reconciled::Withdrawn);
            }
            if let Some(state) = self.settle_never_spawned_writer(record, &fence, call)? {
                return Ok(state);
            }
            return Ok(Reconciled::uncertain(
                UncertainReason::WriterCustodyUnconfirmed {
                    reservation: fence.reservation.clone(),
                },
            ));
        }
        // The submitter died before reserving: the held lease proves no
        // handoff began and keeps a late reservation out.
        let attempt = dir()?;
        let unreserved =
            custody::prove_unreserved(&attempt, scope, call.lock_budget(), call.cancel, call.clock)
                .map_err(|e| RunError::Io(std::io::Error::other(e.to_string())))?;
        let _recovery = match unreserved {
            Some(lease) => Held::Unreserved(lease),
            None => {
                let recovery = match custody::acquire_recovery(
                    dir()?,
                    scope,
                    call.lock_budget(),
                    call.cancel,
                    call.clock,
                ) {
                    Ok(recovery) => recovery,
                    Err(error) => return Ok(Reconciled::custody_unavailable(&error)),
                };
                if recovery.scope() != scope || recovery.observe() != RecoveryState::Absent {
                    return Ok(Reconciled::uncertain(
                        UncertainReason::CustodyNotProvablyAbsent,
                    ));
                }
                if read_sealed(&attempt, "process-exit.json", RECORD_LIMIT)?.is_some() {
                    return Ok(Reconciled::uncertain(
                        UncertainReason::ExitWithoutLaunchSummary,
                    ));
                }
                Held::Reserved(Box::new(recovery))
            }
        };
        let now = call
            .clock
            .wall_now()
            .map_err(|_| RunError::Validation("wall clock"))?;
        let done = self.withdraw(
            id,
            AttemptState::Starting {
                extra: Default::default(),
            },
            if cancelled {
                Outcome::Cancelled
            } else {
                Outcome::Rejected
            },
            if cancelled {
                "cancelled before launch"
            } else {
                "launch interrupted: never reached a guardian"
            },
            now,
            call,
        )?;
        Ok(match done {
            Some(_) => Reconciled::Withdrawn,
            None => Reconciled::Finished,
        })
    }
    fn reconcile_terminal_temporary(
        &self,
        record: &AttemptRecord,
        call: &Request<'_>,
        observer: &dyn ProcessObserver,
    ) -> Result<Option<Reconciled>> {
        if let Some(state) = self.settle_rebooted(record, call, observer)? {
            return Ok(Some(state));
        }
        let id = record.attempt_id;
        let scope = Self::scope(record);
        let path = self
            .context
            .state_root
            .as_path()
            .join(format!("runs/{}/attempts/{id}/tmp", record.run_id));
        let listing = match self.writer_fence_listing() {
            Ok(listing) => listing,
            Err(error) => {
                return Ok(Some(Reconciled::TemporaryRetained {
                    path,
                    reason: error.to_string().into(),
                }));
            }
        };
        let retained = listing
            .reservations
            .into_iter()
            .find(|fence| fence.scope.run == record.run_id && fence.scope.attempt == id);
        match retained {
            Some(fence)
                if fence.scope == scope
                    && record.provider_launched == Some(false)
                    && record.exit_evidence.is_none()
                    && self.withdraw_prepared_writer(&fence, call)? =>
            {
                // Another retained reservation may name this attempt.
                // Retry temporary cleanup on the next scan.
                return Ok(Some(Reconciled::Withdrawn));
            }
            Some(fence) => {
                if Self::needs_lost_settlement(record)
                    && let Some(state) = self.settle_never_spawned_writer(record, &fence, call)?
                {
                    return Ok(Some(state));
                }
                return Ok(Some(Reconciled::uncertain(
                    UncertainReason::WriterReservationRetained {
                        reservation: fence.reservation.clone(),
                    },
                )));
            }
            None => {}
        }
        if matches!(
            record.state,
            AttemptState::Terminal {
                outcome: Outcome::Lost,
                exit_code: None,
                ..
            }
        ) && record.exit_evidence.is_none()
        {
            // The retry marker covers release, including reader reservations.
            // Once released, preserved scratch must never enter normal cleanup.
            match self
                .attempt_dir(record.run_id, id, false)
                .and_then(|attempt| {
                    let evidence = custody::attempt_evidence(&attempt, scope)?;
                    read_sealed(&evidence, "reservation.json", RECORD_LIMIT)
                }) {
                Ok(Some(_)) => {
                    return Ok(Some(Reconciled::uncertain(
                        UncertainReason::GuardianNotObservable,
                    )));
                }
                Ok(None) => {}
                Err(RunError::Filesystem(error)) if error.is_not_found() => {}
                Err(error) => {
                    return Ok(Some(Reconciled::TemporaryRetained {
                        path,
                        reason: error.to_string().into(),
                    }));
                }
            }
            self.set_temporary_retained(id, false, call)?;
            return Ok(None);
        }
        let attempt = match self
            .attempt_dir(record.run_id, id, false)
            .and_then(|attempt| {
                attempt.metadata_at(std::ffi::OsStr::new("tmp"), false)?;
                Ok(attempt)
            }) {
            Ok(attempt) => attempt,
            Err(RunError::Filesystem(error)) if error.is_not_found() => {
                self.set_temporary_retained(id, false, call)?;
                return Ok(None);
            }
            Err(error) => {
                return Ok(Some(Reconciled::TemporaryRetained {
                    path,
                    reason: error.to_string().into(),
                }));
            }
        };
        // An unreadable reservation cannot establish absence of
        // custody for this attempt, so leave temporary data alone.
        if let Some(issue) = listing.issues.first() {
            return Ok(Some(Reconciled::TemporaryRetained {
                path,
                reason: TemporaryReason::new(
                    TemporaryReasonKind::ReservationUnreadable,
                    format!(
                        "writer reservation unreadable at {}: {}",
                        issue.reservation.display(),
                        issue.reason
                    ),
                ),
            }));
        }
        let remove = || -> Result<()> {
            match attempt.metadata_at(std::ffi::OsStr::new("tmp"), false) {
                Err(error) if error.is_not_found() => return Ok(()),
                Err(error) => return Err(error.into()),
                Ok(_) => {}
            }
            // A read-only guardian reserves the attempt itself. Hold
            // its exclusion lock and refuse any retained reservation.
            let _lease = attempt.try_lock(
                std::ffi::OsStr::new("writer.lock"),
                provenant_os::fs::LockMode::Exclusive,
            )?;
            match custody::reservation_present(&attempt, scope) {
                Ok(true) => return Err(RunError::AttemptReservationRetained),
                Ok(false) => {}
                Err(error) => return Err(error.into()),
            }
            artifacts::remove_unreserved_terminal_temporary(&attempt)?;
            Ok(())
        };
        Ok(Some(match remove() {
            Ok(()) => {
                self.set_temporary_retained(id, false, call)?;
                Reconciled::TemporaryRemoved
            }
            Err(error) => Reconciled::TemporaryRetained {
                path,
                reason: TemporaryReason::new(
                    match error {
                        RunError::AttemptReservationRetained => {
                            TemporaryReasonKind::ReservationRetained
                        }
                        RunError::Filesystem(provenant_os::fs::FsError::NotPrivate { .. }) => {
                            TemporaryReasonKind::NotPrivate
                        }
                        _ => TemporaryReasonKind::Error,
                    },
                    error.to_string(),
                ),
            },
        }))
    }

    fn reconcile_attempt(
        &self,
        id: AttemptId,
        providers: &dyn Providers,
        call: &Request<'_>,
        observer: &dyn ProcessObserver,
    ) -> Result<Option<Reconciled>> {
        let record = self.record(id, call)?;
        if matches!(record.state, AttemptState::Terminal { .. }) {
            return self.reconcile_terminal_temporary(&record, call, observer);
        }
        self.reconcile_live_attempt(&record, providers, call, observer)
            .map(Some)
    }

    fn reconcile_live_attempt(
        &self,
        record: &AttemptRecord,
        providers: &dyn Providers,
        call: &Request<'_>,
        observer: &dyn ProcessObserver,
    ) -> Result<Reconciled> {
        let id = record.attempt_id;
        let scope = Self::scope(record);
        if matches!(record.state, AttemptState::Queued { .. }) {
            return Ok(Reconciled::Queued);
        }
        if let Some(state) = self.settle_rebooted(record, call, observer)? {
            return Ok(state);
        }
        let dir = || self.attempt_dir(record.run_id, id, false);
        match custody::reattach(dir()?, scope) {
            Ok(handle) => {
                let budget = provenant_core::WaitBudget::cli(call.clock.mono_now(), 1)
                    .map_err(|_| RunError::Validation("observation budget"))?;
                match custody::observe(&handle, budget, call.cancel, call.clock) {
                    Ok(Observation::Exited(exit))
                        if exit.unresolved_cleanup && !exit.unresolved_pids.is_empty() =>
                    {
                        let still_named = || {
                            if custody::any_pid_alive(&exit.unresolved_pids) {
                                Reconciled::Escaped {
                                    pids: exit.unresolved_pids.clone(),
                                }
                            } else {
                                // A live guardian republishes the settled exit;
                                // the next pass commits it.
                                Reconciled::Held
                            }
                        };
                        // The writer lock is free only when no guardian remains
                        // to settle this exit, so recovery ends the orphans.
                        let Ok(mut recovery) = self.orphan_recovery(record, dir()?, call) else {
                            return Ok(still_named());
                        };
                        match recovery.end_orphans(&dir()?, ORPHAN_PATIENCE) {
                            Ok(report) if report.survivors.is_empty() => {
                                drop(recovery);
                                // The exit is settled: commit it as any other.
                                self.reconcile_attempt(id, providers, call, observer)
                                    .map(|state| state.unwrap_or(Reconciled::Finished))
                            }
                            Ok(report) => Ok(Reconciled::Escaped {
                                pids: report.survivors.iter().map(|s| s.pid).collect(),
                            }),
                            Err(CustodyError::Fenced) => Ok(still_named()),
                            Err(error) => Ok(Reconciled::custody_unavailable(&error)),
                        }
                    }
                    Ok(Observation::Exited(exit)) => {
                        let attempt = dir()?;
                        self.finish(id, &attempt, &exit, providers, call)?;
                        custody::publish_release(&attempt, scope)
                            .map_err(|e| RunError::Io(std::io::Error::other(e.to_string())))?;
                        Ok(Reconciled::Finished)
                    }
                    Ok(Observation::Live) if past_runtime_deadline(record, call) => {
                        // The guardian should have stopped the provider at the
                        // deadline itself; ask again through its own channel.
                        Ok(match custody::stop(&handle, StopReason::WallDeadline) {
                            Ok(_) => Reconciled::DeadlineStop,
                            Err(error) => Reconciled::custody_unavailable(&error),
                        })
                    }
                    Ok(Observation::Live | Observation::PublicationPending) => Ok(Reconciled::Held),
                    Ok(Observation::Unknown) => {
                        let settled =
                            match self.settle_unreserved_loss(record, call, observer, false)? {
                                Some(state) => Some(state),
                                None => self.settle_holderless(record, call, observer)?,
                            };
                        Ok(settled.unwrap_or_else(|| {
                            Reconciled::uncertain(UncertainReason::GuardianNotObservable)
                        }))
                    }
                    Err(error) => Ok(Reconciled::custody_unavailable(&error)),
                }
            }
            // No launch summary: the handoff never confirmed. Only a
            // reservation that provably never spawned may be withdrawn.
            Err(CustodyError::Filesystem(error)) if error.is_not_found() => {
                self.settle_unlaunched(id, record, call)
            }
            Err(error) => Ok(Reconciled::custody_unavailable(&error)),
        }
    }
}
/// Writer exclusion held across a withdrawal.
#[expect(dead_code, reason = "held only for its lock")]
enum Held {
    Unreserved(custody::Unreserved),
    Reserved(Box<custody::Recovery>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconcile_journal_errors_do_not_hide_other_attempts()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        for failure in ["clear", "blocked", "listing"] {
            let temp = tempfile::tempdir()?;
            let clock = crate::tests::clock();
            let cancel = provenant_core::CancelToken::new();
            let call = crate::tests::call(&clock, &cancel);
            let context = crate::tests::context(temp.path())?;
            let registry = temp.path().join("registry");
            std::fs::create_dir(&registry)?;
            if failure == "listing" {
                std::fs::write(registry.join("worktrees"), b"not a directory")?;
            }
            let store = RunStore::open(&context, &call)?.with_writer_registry(&registry);
            let mut run = crate::tests::input();
            let bad = run.attempts[0].attempt_id;
            let good = AttemptId::from_bytes([7; 16]);
            let mut task = run.request.tasks[0].clone();
            task.task_id = provenant_core::TaskId::from_bytes([8; 16]);
            run.attempts.push(crate::NewAttempt {
                attempt_id: good,
                task_id: task.task_id,
                prompt: run.attempts[0].prompt.clone(),
            });
            run.request.tasks.push(task);
            store.create(&run, &call)?;
            if failure == "blocked" {
                store.mark_starting(
                    &store.record(bad, &call)?,
                    &crate::tests::route(),
                    &[],
                    &Default::default(),
                    None,
                    &call,
                )?;
                crate::artifacts::seal(
                    &store.attempt_dir(run.run_id, bad, false)?,
                    "launch.json",
                    b"{}",
                    RECORD_LIMIT,
                )?;
            }
            // Make journal I/O fail without relying on disk capacity or permissions.
            std::fs::create_dir(context.state_root.as_path().join(format!(
                "runs/{}/attempts/{bad}/settlement-progress.json",
                run.run_id
            )))?;
            let found = store.reconcile_with_observer(
                run.run_id,
                &crate::dispatch_tests::Fake::default(),
                &call,
                &SystemProcessObserver,
            )?;
            assert_eq!(found.len(), 2, "{failure}: {found:?}");
            assert!(
                matches!(
                    &found[0],
                    (id, Reconciled::Uncertain { reason })
                        if *id == bad && reason.to_string().contains("settlement progress")
                ),
                "{failure}: {found:?}"
            );
            assert_eq!(found[1], (good, Reconciled::Queued));
            if failure != "blocked" {
                assert!(matches!(&found[0].1, Reconciled::Uncertain { reason }
                    if reason.to_string().starts_with("queued; settlement progress")));
            }
        }
        Ok(())
    }

    #[test]
    fn reconcile_reports_an_orphan_fence_without_losing_known_attempts()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let context = crate::tests::context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let orphan = AttemptId::from_bytes([7; 16]);
        let slot = context
            .state_root
            .as_path()
            .join("worktrees")
            .join(context.project_id.to_string())
            .join("orphan");
        std::fs::create_dir_all(&slot)?;
        drop(custody::acquire_reader(
            provenant_os::fs::Directory::open_anchor(&slot)?,
            Scope {
                run: run.run_id,
                attempt: orphan,
                generation: 1,
            },
            call.lock_budget(),
            &cancel,
            &clock,
        )?);
        assert!(matches!(
            store.record(orphan, &call),
            Err(RunError::NotFound)
        ));
        store.attempt_dir(run.run_id, orphan, true)?;
        assert_eq!(store.writer_fences()?.len(), 1);
        let found = store.reconcile_with_observer(
            run.run_id,
            &crate::dispatch_tests::Fake::default(),
            &call,
            &SystemProcessObserver,
        )?;
        assert_eq!(found.len(), 2);
        assert_eq!(found[0], (run.attempts[0].attempt_id, Reconciled::Queued));
        assert!(matches!(&found[1], (id, Reconciled::Uncertain { reason })
            if *id == orphan && reason.to_string().contains(&RunError::NotFound.to_string())));
        let queued = store.queue_reconcile_with(run.run_id, &call, |_, _| {
            panic!("queued work and an orphan fence need no recovery worker")
        })?;
        assert_eq!(queued[0], (run.attempts[0].attempt_id, Reconciled::Queued));
        assert!(matches!(&queued[1], (id, Reconciled::Uncertain { .. }) if *id == orphan));
        Ok(())
    }

    #[test]
    fn unreadable_writer_listing_still_judges_known_attempts()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::DirBuilderExt;

        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let registry = std::fs::canonicalize(temp.path())?.join("registry");
        // claim() also uses this as the private admission-ledger root.
        // Its mode must not depend on the test runner's umask.
        std::fs::DirBuilder::new().mode(0o700).create(&registry)?;
        std::fs::write(registry.join("worktrees"), b"not a directory")?;
        let store = RunStore::open(&crate::tests::context(temp.path())?, &call)?
            .with_writer_registry(&registry);
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let found = store.reconcile_with_observer(
            run.run_id,
            &crate::dispatch_tests::Fake::default(),
            &call,
            &SystemProcessObserver,
        )?;
        assert_eq!(found, [(run.attempts[0].attempt_id, Reconciled::Queued)]);
        let id = run.attempts[0].attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let running = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let directory = store.attempt_dir(run.run_id, id, false)?;
        crate::artifacts::seal(&directory, "stdout.log", b"answer", RECORD_LIMIT)?;
        let mut exit = crate::tests::exit();
        exit.scope = RunStore::scope(&running);
        crate::artifacts::seal(
            &directory,
            "launch.json",
            &serde_json::to_vec(&serde_json::json!({
                "schema_version": 1, "scope": exit.scope, "requester": store.context.actor_id,
            }))?,
            RECORD_LIMIT,
        )?;
        crate::dispatch_tests::seal_exit(&store, &exit)?;
        let found = store.reconcile_with_observer(
            run.run_id,
            &crate::dispatch_tests::Fake::default(),
            &call,
            &SystemProcessObserver,
        )?;
        assert_eq!(found, [(id, Reconciled::Finished)]);
        assert!(matches!(
            store.record(id, &call)?.state,
            AttemptState::Terminal { .. }
        ));
        Ok(())
    }

    #[test]
    fn uncertain_reasons_serialise_by_kind_and_keep_their_text() {
        let retained = Reconciled::TemporaryRetained {
            path: "/tmp".into(),
            reason: TemporaryReason::new(TemporaryReasonKind::ReservationUnreadable, "detail"),
        };
        assert_eq!(serde_json::to_value(retained).unwrap()["reason"], "detail");
        assert_eq!(
            serde_json::to_value(UncertainReason::RebootProofUnavailable {
                error: "proof failed".into(),
                cause: RebootProofCause::Filesystem,
            })
            .unwrap(),
            serde_json::json!({ "kind": "reconcile_failed", "error": "proof failed" })
        );
        let reservation = std::path::PathBuf::from("/state/custody/reservation.json");
        let found = Reconciled::uncertain(UncertainReason::WriterCustodyUnconfirmed {
            reservation: reservation.clone(),
        });
        let value = serde_json::to_value(&found).unwrap();
        assert_eq!(value["state"], "uncertain");
        assert_eq!(value["reason"]["kind"], "writer_custody_unconfirmed");
        assert_eq!(
            value["reason"]["reservation"],
            "/state/custody/reservation.json"
        );
        let Reconciled::Uncertain { reason } = found else {
            unreachable!()
        };
        // The display text is the operator's recovery guidance.
        assert!(reason.to_string().contains("chair recovery required"));
        let unit = serde_json::to_value(UncertainReason::GuardianNotObservable).unwrap();
        assert_eq!(
            unit,
            serde_json::json!({ "kind": "guardian_not_observable" })
        );
        let custody = Reconciled::custody_unavailable(&"lock is held");
        assert_eq!(
            custody,
            Reconciled::Uncertain {
                reason: UncertainReason::CustodyUnavailable {
                    error: "lock is held".into()
                }
            }
        );
    }
}
