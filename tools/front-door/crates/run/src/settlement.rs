//! Durable settlement progress. Kernel locks protect only journal updates.
use crate::{
    AdmissionError, RECORD_LIMIT, Result, RunError, artifacts::read_sealed, store::StopMarker,
};
use provenant_core::retry::Backoff;
use provenant_core::{ActorId, CancelToken, Clock, DurationMicros, GitOid};
use provenant_os::fs::{Directory, FsError, LockMode, PublishMode};
use provenant_records::custody::Scope;
use provenant_supervisor::census::{ProcessIdentity, ProcessObservation, SystemProcessObserver};
use serde::{Deserialize, Serialize};
use std::{ffi::OsStr, sync::mpsc, time::Duration};

const LIMIT: u32 = 3;
const UNKNOWN_OWNER_LIMIT_MS: u64 = 10 * 60 * 1000;
const CLAIM_BACKOFF: Backoff = Backoff::new(Duration::from_secs(5), 6, Duration::from_secs(120));

/// Permission to preserve committed work under quiescent native custody.
/// This consumes an exact current owner intent, never grants continuation,
/// and never changes the provider's sealed signal ordering or stop cause.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PreservationStop {
    scope: Scope,
    requester: ActorId,
    reason: provenant_supervisor::machine::StopReason,
    fingerprint: provenant_core::Digest,
}
impl PreservationStop {
    pub(crate) fn reason(self) -> provenant_supervisor::machine::StopReason {
        self.reason
    }
    pub(crate) fn from_exit(
        attempt: &Directory,
        exit: &provenant_supervisor::custody::ExitObservation,
        requester: ActorId,
    ) -> Result<Option<Self>> {
        use provenant_supervisor::machine::StopReason;
        if exit.never_spawned {
            return Ok(None);
        }
        let expected = if exit.stop_reason == Some(StopReason::Steered)
            && exit.cancel_delivered_before_exit != Some(true)
        {
            None
        } else {
            exit.stop_reason
        };
        Self::from_marker(attempt, exit.scope, requester, expected)
    }
    pub(crate) fn from_terminal_record(
        attempt: &Directory,
        record: &provenant_records::run::AttemptRecord,
        requester: ActorId,
    ) -> Result<Option<Self>> {
        use provenant_records::stop::StopCause;
        use provenant_supervisor::machine::StopReason;
        if record.provider_launched != Some(true) || record.exit_evidence.is_none() {
            return Ok(None);
        }
        let expected = record.state.stop().and_then(|stop| match stop.cause() {
            StopCause::Steered => Some(StopReason::Steered),
            StopCause::Deadline => Some(StopReason::WallDeadline),
            StopCause::TokenBudget => Some(StopReason::TokenBudget),
            StopCause::ContextHandoff => Some(StopReason::ContextHandoff),
            _ => None,
        });
        Self::from_marker(attempt, crate::RunStore::scope(record), requester, expected)
    }
    fn from_marker(
        attempt: &Directory,
        scope: Scope,
        requester: ActorId,
        expected: Option<provenant_supervisor::machine::StopReason>,
    ) -> Result<Option<Self>> {
        use provenant_supervisor::{
            custody::{StopIntentState, read_stop_intent},
            machine::StopReason,
        };
        let StopIntentState::Requested(request) = read_stop_intent(attempt, scope)
            .map_err(|error| RunError::Io(std::io::Error::other(error)))?
        else {
            return Ok(None);
        };
        if request.requester != requester
            || !(request.reason == StopReason::ExplicitCancellation
                || Some(request.reason) == expected)
        {
            return Ok(None);
        }
        let fingerprint =
            provenant_records::CanonicalV1::record_digest("preservation-stop", 1, &request)?;
        Ok(Some(Self {
            scope,
            requester,
            reason: request.reason,
            fingerprint,
        }))
    }
    pub(crate) fn marker_vetoes(self, attempt: &Directory) -> Result<bool> {
        use provenant_supervisor::custody::{StopIntentState, read_stop_intent};
        let StopIntentState::Requested(request) = read_stop_intent(attempt, self.scope)
            .map_err(|error| RunError::Io(std::io::Error::other(error)))?
        else {
            return Ok(true);
        };
        Ok(request.requester != self.requester
            || request.reason != self.reason
            || provenant_records::CanonicalV1::record_digest("preservation-stop", 1, &request)?
                != self.fingerprint)
    }
}

#[cfg(test)]
thread_local! {
    static TEST_PROCESS_STATE: std::cell::Cell<Option<std::result::Result<provenant_os::pid::ProcessState, std::io::ErrorKind>>> = const { std::cell::Cell::new(None) };
    static TEST_FAIL_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn process_state(
    pid: i64,
) -> std::result::Result<provenant_os::pid::ProcessState, provenant_os::pid::PidError> {
    #[cfg(test)]
    if let Some(state) = TEST_PROCESS_STATE.with(|state| state.get()) {
        return state.map_err(|kind| provenant_os::pid::PidError::Io {
            pid: pid as i32,
            source: std::io::Error::from(kind),
        });
    }
    provenant_os::pid::process_state(pid)
}

pub(crate) use provenant_supervisor::settlement_progress::Phase;
use provenant_supervisor::settlement_progress::{Abandonment, FILE, Journal};

fn read(attempt: &Directory, scope: Scope) -> Result<Journal> {
    let journal = match read_sealed(attempt, FILE, RECORD_LIMIT)? {
        Some(raw) => crate::db::decode::<Journal>(&raw)?,
        None => Journal::new(scope),
    };
    if journal.schema_version != 1 || journal.scope != scope || journal.attempts > LIMIT {
        return Err(RunError::Validation(
            "settlement journal scope or version mismatch",
        ));
    }
    Ok(journal)
}

fn write(attempt: &Directory, journal: &Journal) -> Result<()> {
    #[cfg(test)]
    if TEST_FAIL_WRITE.with(|fail| fail.replace(false)) {
        return Err(RunError::Io(std::io::Error::other(
            "injected settlement write failure",
        )));
    }
    attempt
        .atomic_write(
            OsStr::new(FILE),
            ".settlement-",
            &crate::db::bytes(journal)?,
            PublishMode::Replace,
        )?
        .require_durable()?;
    Ok(())
}

/// Custody can fail before the import driver exists. Retain those failures too,
/// so readers report the cause and cannot repeatedly launch ineffective workers.
pub(crate) fn blocked(
    attempt: &Directory,
    scope: Scope,
    reason: &str,
    clock: &dyn Clock,
) -> Result<()> {
    let _lock = journal_lock(attempt)?;
    let mut journal = read(attempt, scope)?;
    let now = now_ms(clock)?;
    if owner_active(attempt, &journal, now)? {
        return Ok(());
    }
    journal.recovery_failures = journal.recovery_failures.saturating_add(1).min(LIMIT);
    journal.blocking_reason = Some(provenant_core::bounded_line(reason, 4096));
    journal.retry_after_ms = now.saturating_add(backoff_ms(journal.recovery_failures));
    write(attempt, &journal)
}

/// Read-only, scope-checked diagnostic for status. The guardian records this
/// even when no canonical outcome could be committed yet.
pub fn blocking_reason_for(
    root: &std::path::Path,
    record: &provenant_records::run::AttemptRecord,
) -> Option<String> {
    let attempt =
        Directory::open_anchor(&root.join(format!("attempts/{}", record.attempt_id))).ok()?;
    read(&attempt, crate::RunStore::scope(record))
        .ok()?
        .blocking_reason
}

pub(crate) fn clear_blocked(attempt: &Directory, scope: Scope) -> Result<()> {
    let _lock = journal_lock(attempt)?;
    let mut journal = read(attempt, scope)?;
    if journal.blocking_reason.take().is_some() {
        journal.recovery_failures = 0;
        write(attempt, &journal)?;
    }
    Ok(())
}

fn now_ms(clock: &dyn Clock) -> Result<u64> {
    clock
        .wall_now()
        .ok()
        .and_then(|time| u64::try_from(time.unix_millis()).ok())
        .ok_or(RunError::Admission(AdmissionError::Clock))
}

fn alive(owner: &ProcessIdentity, since_ms: u64, now_ms: u64) -> bool {
    observed_alive(
        owner,
        SystemProcessObserver.observe(owner.pid),
        since_ms,
        now_ms,
    )
}

/// Attribute a state only when the recorded identity matches on both sides of the probe.
fn identity_stalled_with(
    clock: &dyn Clock,
    identity: &ProcessIdentity,
    observe: &dyn Fn(i64) -> ProcessObservation,
    probe: &dyn Fn(
        i64,
    ) -> std::result::Result<
        provenant_os::pid::ProcessState,
        provenant_os::pid::PidError,
    >,
) -> bool {
    let matches = || matches!(observe(identity.pid), ProcessObservation::Present(fact) if fact.identity == *identity && !fact.zombie);
    // A healthy process passes through uninterruptible sleep during ordinary
    // disk and network waits. Only a state that persists across the samples,
    // with the same identity throughout, is a stall.
    for sample in 0..STALL_SAMPLES {
        if sample > 0
            && clock
                .mono_now()
                .checked_add(STALL_INTERVAL)
                .and_then(|deadline| clock.wait_until(deadline))
                .is_err()
        {
            return false;
        }
        if !(matches()
            && matches!(
                probe(identity.pid),
                Ok(provenant_os::pid::ProcessState::Uninterruptible)
            )
            && matches())
        {
            return false;
        }
    }
    true
}

/// Consecutive observations that must all find the process uninterruptible.
const STALL_SAMPLES: u32 = 3;
const STALL_INTERVAL: DurationMicros = DurationMicros::from_micros(150_000);

pub(crate) fn host_stalled(attempt: &Directory, scope: Scope) -> bool {
    let clock = provenant_os::clock::SystemClock::new();
    let observer = |pid| SystemProcessObserver.observe(pid);
    let probe = process_state;
    // Probe observations need no durability barrier. Keep the same private-file
    // checks as sealed reads while using the existing bounded stable read.
    let observe = |name: &str| -> Option<Vec<u8>> {
        let name = OsStr::new(name);
        let before = attempt.metadata_at(name, false).ok()?;
        if crate::PathCheck::file(before).is_some() {
            return None;
        }
        let raw = attempt.read_stable(name, RECORD_LIMIT).ok()?;
        let after = attempt.metadata_at(name, false).ok()?;
        (before.unchanged(&after) && crate::PathCheck::file(after).is_none()).then_some(raw)
    };
    if observe(FILE)
        .and_then(|raw| crate::db::decode::<Journal>(&raw).ok())
        .filter(|journal| journal.scope == scope && journal.attempts <= LIMIT)
        .and_then(|journal| journal.owner)
        .is_some_and(|owner| identity_stalled_with(&clock, &owner, &observer, &probe))
    {
        return true;
    }
    let Some(raw) = observe("process.json") else {
        return false;
    };
    #[derive(Deserialize)]
    struct Checkpoint {
        scope: Scope,
        guardian: Option<ProcessIdentity>,
    }
    let Ok(checkpoint) = crate::db::decode::<Checkpoint>(&raw) else {
        return false;
    };
    checkpoint.scope == scope
        && checkpoint
            .guardian
            .as_ref()
            .is_some_and(|guardian| identity_stalled_with(&clock, guardian, &observer, &probe))
}

impl crate::RunStore {
    /// Only recorded identities can be checked; a worker stalled before publishing
    /// its identity retains the existing queued-marker expiry behavior.
    pub(crate) fn supervisor_stalled(
        &self,
        attempt: &Directory,
        scope: Scope,
        call: &crate::Request<'_>,
    ) -> bool {
        if host_stalled(attempt, scope) {
            return true;
        }
        let clock = provenant_os::clock::SystemClock::new();
        let observer = |pid| SystemProcessObserver.observe(pid);
        let probe = process_state;
        let check = |directory: &Directory| {
            provenant_supervisor::custody::reservation_guardian(directory, scope)
                .ok()
                .flatten()
                .is_some_and(|identity| identity_stalled_with(&clock, &identity, &observer, &probe))
        };
        if check(attempt) {
            return true;
        }
        // Only this attempt's own slot can hold its guardian; sibling slots are
        // not inventoried for each displayed row.
        self.attempt_writer_slot(scope.attempt, call)
            .ok()
            .flatten()
            .and_then(|slot| Directory::open_anchor(&slot).ok())
            .is_some_and(|slot| check(&slot))
    }
}

fn observed_alive(
    owner: &ProcessIdentity,
    observation: ProcessObservation,
    since_ms: u64,
    now_ms: u64,
) -> bool {
    match observation {
        ProcessObservation::Present(fact) => fact.identity == *owner && !fact.zombie,
        ProcessObservation::Absent => false,
        ProcessObservation::Unknown(_) => now_ms.saturating_sub(since_ms) < UNKNOWN_OWNER_LIMIT_MS,
    }
}

fn backoff_ms(attempts: u32) -> u64 {
    CLAIM_BACKOFF
        .delay(u64::from(attempts.saturating_sub(1)))
        .as_millis() as u64
}

fn journal_lock(attempt: &Directory) -> Result<provenant_os::fs::LockGuard> {
    let clock = provenant_os::clock::SystemClock::new();
    let budget = provenant_core::WaitBudget::cli(clock.mono_now(), 1)
        .map_err(|_| RunError::Validation("settlement journal budget"))?;
    Ok(attempt.lock(
        OsStr::new("settlement.lock"),
        LockMode::Exclusive,
        budget,
        &CancelToken::new(),
        &clock,
    )?)
}

// A zero-budget flock interrupted by a signal is surfaced as LockTimeout by
// Directory. A few immediate probes distinguish that transient refusal without
// waiting on another owner or spending an import attempt.
fn try_progress_lock(
    attempt: &Directory,
    name: &str,
) -> provenant_os::fs::Result<provenant_os::fs::LockGuard> {
    let mut result = attempt.try_lock(OsStr::new(name), LockMode::Exclusive);
    for _ in 0..2 {
        if !matches!(result, Err(FsError::LockTimeout { .. })) {
            break;
        }
        result = attempt.try_lock(OsStr::new(name), LockMode::Exclusive);
    }
    result
}

pub(crate) struct Guard;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ClaimRefusal {
    DriverLease,
    JournalLock,
    Complete,
    LiveOwner,
    RetryDeadline(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    JournalLock,
    LiveOwner,
    RetryDeadline(u64),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::JournalLock => formatter.write_str("settlement journal contention"),
            Self::LiveOwner => formatter.write_str("settlement driver ownership is live"),
            Self::RetryDeadline(deadline) => {
                write!(formatter, "settlement retry deadline: {deadline}ms")
            }
        }
    }
}

/// Refusal observations use the same locks and deadline as a claim, without
/// changing its retry budget. A free lifetime lease proves a same-process owner
/// has abandoned its driver, even if Drop could not clear the journal.
fn owner_active(attempt: &Directory, journal: &Journal, now: u64) -> Result<bool> {
    match try_progress_lock(attempt, "settlement-driver.lock") {
        Err(FsError::LockTimeout { .. }) => Ok(true),
        Err(error) => Err(error.into()),
        Ok(_lease) => Ok(journal.owner.as_ref().is_some_and(|owner| {
            owner.pid != i64::from(std::process::id()) && alive(owner, journal.owner_since_ms, now)
        })),
    }
}

pub(crate) fn refusal(
    attempt: &Directory,
    scope: Scope,
    clock: &dyn Clock,
) -> Result<Option<Refusal>> {
    let _lock = match try_progress_lock(attempt, "settlement.lock") {
        Ok(lock) => lock,
        Err(FsError::LockTimeout { .. }) => {
            return Ok(Some(Refusal::JournalLock));
        }
        Err(error) => return Err(error.into()),
    };
    let journal = read(attempt, scope)?;
    let now = now_ms(clock)?;
    if owner_active(attempt, &journal, now)? {
        return Ok(Some(Refusal::LiveOwner));
    }
    if now < journal.retry_after_ms {
        return Ok(Some(Refusal::RetryDeadline(journal.retry_after_ms)));
    }
    Ok(None)
}

impl Guard {
    /// A live native owner, including this process, excludes another driver.
    /// A crash leaves its progress and attempt count ready for the next claim.
    pub(crate) fn claim<'a>(
        attempt: &Directory,
        scope: Scope,
        clock: &'a dyn Clock,
    ) -> Result<Option<Driver<'a>>> {
        Ok(Self::claim_observed(attempt, scope, clock)?.0)
    }

    /// Return the refusal observed under the claim's locks, without a second
    /// probe that can race a lock release.
    pub(crate) fn claim_observed<'a>(
        attempt: &Directory,
        scope: Scope,
        clock: &'a dyn Clock,
    ) -> Result<(Option<Driver<'a>>, Option<ClaimRefusal>)> {
        let now_ms = now_ms(clock)?;
        let lease = match try_progress_lock(attempt, "settlement-driver.lock") {
            Ok(lease) => lease,
            Err(FsError::LockTimeout { .. }) => return Ok((None, Some(ClaimRefusal::DriverLease))),
            Err(error) => return Err(error.into()),
        };
        let _lock = match try_progress_lock(attempt, "settlement.lock") {
            Ok(lock) => lock,
            Err(FsError::LockTimeout { .. }) => return Ok((None, Some(ClaimRefusal::JournalLock))),
            Err(error) => return Err(error.into()),
        };
        let mut journal = read(attempt, scope)?;
        if journal.phase == Phase::Complete {
            return Ok((None, Some(ClaimRefusal::Complete)));
        }
        if journal.owner.as_ref().is_some_and(|owner| {
            owner.pid != i64::from(std::process::id())
                && alive(owner, journal.owner_since_ms, now_ms)
        }) {
            return Ok((None, Some(ClaimRefusal::LiveOwner)));
        }
        if journal.attempts == LIMIT && journal.phase == Phase::Import {
            return Err(RunError::ObjectImportLimit(
                "settlement exhausted three attempts; private Git retained for host recovery",
            ));
        }
        if now_ms < journal.retry_after_ms {
            return Ok((
                None,
                Some(ClaimRefusal::RetryDeadline(journal.retry_after_ms)),
            ));
        }
        let owner = match SystemProcessObserver.observe(i64::from(std::process::id())) {
            ProcessObservation::Present(fact) if !fact.zombie => fact.identity,
            _ => return Err(RunError::Admission(AdmissionError::GuardianIdentity)),
        };
        journal.owner = Some(owner.clone());
        journal.owner_since_ms = now_ms;
        journal.retry_after_ms = now_ms.saturating_add(backoff_ms(journal.attempts));
        write(attempt, &journal)?;
        Ok((
            Some(Driver {
                _lease: lease,
                attempt: attempt.try_clone()?,
                journal,
                owner,
                cancel: CancelToken::new(),
                import_started: false,
                clock,
            }),
            None,
        ))
    }
}

/// A kernel lease excludes drivers for their whole lifetime; the separate
/// journal lock covers only short updates.
pub(crate) struct Driver<'a> {
    _lease: provenant_os::fs::LockGuard,
    clock: &'a dyn Clock,
    attempt: Directory,
    journal: Journal,
    owner: ProcessIdentity,
    cancel: CancelToken,
    import_started: bool,
}

impl Driver<'_> {
    pub(crate) fn cancellation(&self) -> CancelToken {
        self.cancel.clone()
    }
    pub(crate) fn phase(&self) -> Phase {
        self.journal.phase
    }
    #[cfg(test)]
    pub(crate) fn attempts(&self) -> u32 {
        self.journal.attempts
    }
    pub(crate) fn exhausted(&self) -> bool {
        self.journal.attempts >= LIMIT
    }
    /// Charge only a driver which has reached the actual object import.
    pub(crate) fn begin_import(&mut self) -> Result<()> {
        if self.import_started || self.journal.phase != Phase::Import {
            return Ok(());
        }
        self.update(|journal| {
            if journal.attempts >= LIMIT {
                return Err(RunError::ObjectImportLimit(
                    "settlement exhausted three attempts; private Git retained for host recovery",
                ));
            }
            journal.attempts += 1;
            Ok(())
        })?;
        self.import_started = true;
        Ok(())
    }
    pub(crate) fn head(&self) -> Option<GitOid> {
        self.journal.head
    }
    pub(crate) fn base(&self) -> Option<GitOid> {
        self.journal.base
    }
    pub(crate) fn set_head(&mut self, head: GitOid) -> Result<()> {
        self.update(|journal| {
            if journal.head.is_some_and(|pinned| pinned != head) {
                return Err(RunError::Validation(
                    "settlement head changed after pinning",
                ));
            }
            journal.head = Some(head);
            Ok(())
        })
    }
    pub(crate) fn set_base(&mut self, base: GitOid) -> Result<()> {
        self.update(|journal| {
            if journal.base.is_some_and(|pinned| pinned != base) {
                return Err(RunError::Validation(
                    "settlement base changed after pinning",
                ));
            }
            journal.base = Some(base);
            Ok(())
        })
    }
    /// Replaying a completed phase leaves progress unchanged.
    pub(crate) fn advance(&mut self, phase: Phase) -> Result<()> {
        self.update(|journal| {
            journal.phase = journal.phase.max(phase);
            journal.blocking_reason = None;
            journal.recovery_failures = 0;
            Ok(())
        })
    }
    pub(crate) fn record_failure(&mut self, reason: &str) -> Result<()> {
        self.update(|journal| {
            journal.blocking_reason =
                Some(provenant_core::bounded_line(reason, 4096));
            Ok(())
        })
    }
    fn update(&mut self, action: impl FnOnce(&mut Journal) -> Result<()>) -> Result<()> {
        let _lock = journal_lock(&self.attempt)?;
        let mut journal = read(&self.attempt, self.journal.scope)?;
        if journal.owner.as_ref() != Some(&self.owner) {
            return Err(RunError::Contention);
        }
        action(&mut journal)?;
        write(&self.attempt, &journal)?;
        self.journal = journal;
        Ok(())
    }
}

impl Drop for Driver<'_> {
    fn drop(&mut self) {
        let Ok(_lock) = journal_lock(&self.attempt) else {
            return;
        };
        let Ok(mut journal) = read(&self.attempt, self.journal.scope) else {
            return;
        };
        if journal.owner.as_ref() == Some(&self.owner) {
            journal.owner = None;
            if let Ok(now) = now_ms(self.clock) {
                journal.retry_after_ms = now.saturating_add(backoff_ms(journal.attempts));
            }
            let _ = write(&self.attempt, &journal);
        }
    }
}

/// Bridge the durable cancellation request to a running Git child's token.
/// Completion wakes the watcher immediately; no watcher outlives the operation.
pub(crate) fn with_cancellation<T>(
    attempt: &Directory,
    scope: Scope,
    cancel: &CancelToken,
    action: impl FnOnce() -> Result<T>,
) -> Result<T> {
    with_cancellation_policy(attempt, scope, cancel, None, action)
}

/// Run writer settlement while accepting only its already delivered Steered
/// request. A replacement explicit cancellation still cancels the import, and
/// so does a marker that disappears.
/// The watcher uses the shared stop-intent API. Ordinary work ignores a valid
/// steer; settlement may consume exactly its delivered stop while preserving
/// committed work. Foreign, replaced, undecodable or unreadable intent vetoes.
pub(crate) fn with_cancellation_policy<T>(
    attempt: &Directory,
    scope: Scope,
    cancel: &CancelToken,
    preservation_stop: Option<PreservationStop>,
    action: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let directory = attempt.try_clone()?;
    let token = cancel.clone();
    let (done, stopped) = mpsc::channel();
    std::thread::scope(|threads| {
        threads.spawn(move || {
            loop {
                let cancelled = match preservation_stop {
                    Some(stop) => stop.marker_vetoes(&directory).unwrap_or(true),
                    None => StopMarker::read(&directory, scope).map_or(true, StopMarker::stops),
                };
                if cancelled {
                    token.cancel();
                    break;
                }
                match stopped.recv_timeout(Duration::from_millis(50)) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
        });
        let result = action();
        let _ = done.send(());
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::AttemptId;
    use provenant_core::{DurationMicros, MonoTime, TestClock, WallTime};

    fn clock() -> TestClock {
        TestClock::new(WallTime::from_micros(0).unwrap(), MonoTime::from_micros(0))
    }

    fn advance_to(clock: &mut TestClock, millis: u64) -> TestResult {
        let current = now_ms(clock)?;
        clock.advance(DurationMicros::from_micros(
            millis.saturating_sub(current) * 1000,
        ))?;
        Ok(())
    }

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    // Spec: docs/specs/run.md#stop-intent
    #[test]
    fn preservation_is_bound_to_the_exact_current_owner_intent() -> TestResult {
        let temp = tempfile::tempdir()?;
        let attempt = Directory::open_anchor(temp.path())?;
        let scope = crate::tests::exit().scope;
        let requester = ActorId::from_bytes([7; 16]);
        provenant_supervisor::custody::request_stop(
            attempt.try_clone()?,
            scope,
            requester,
            provenant_supervisor::machine::StopReason::ExplicitCancellation,
        )?;
        let token =
            PreservationStop::from_marker(&attempt, scope, requester, None)?.ok_or("stop")?;
        assert!(!token.marker_vetoes(&attempt)?);
        let raw = crate::artifacts::read_sealed(&attempt, "stop-intent.json", RECORD_LIMIT)?
            .ok_or("intent")?;
        for foreign_actor in [false, true] {
            let mut replacement: provenant_supervisor::custody::StopIntent =
                crate::db::decode(&raw)?;
            if foreign_actor {
                replacement.requester = ActorId::from_bytes([9; 16]);
            } else {
                replacement.request_id = provenant_core::OperationId::parse("different-operation")?;
            }
            attempt
                .atomic_write(
                    OsStr::new("stop-intent.json"),
                    ".test-",
                    &crate::db::bytes(&replacement)?,
                    PublishMode::Replace,
                )?
                .require_durable()?;
            assert!(
                token.marker_vetoes(&attempt)?,
                "replaced intent cannot reuse preservation authority"
            );
        }
        Ok(())
    }

    // Spec: docs/specs/fabric.md#session-ready-steering
    #[test]
    fn running_git_continues_only_under_its_own_steer_marker() -> TestResult {
        use provenant_supervisor::machine::StopReason;
        let scope = crate::tests::exit().scope;
        let marker = |reason: StopReason| {
            crate::db::bytes(&serde_json::json!({
                "schema_version": 1,
                "scope": scope,
                "requester": ActorId::from_bytes([7; 16]),
                "request_id": format!("{}.{}.stop", scope.attempt, scope.generation),
                "reason": reason,
            }))
        };
        let steered = marker(StopReason::Steered)?;
        for (name, replacement) in [
            ("undecodable", b"{not json".to_vec()),
            ("explicit stop", marker(StopReason::ExplicitCancellation)?),
        ] {
            let temp = tempfile::tempdir()?;
            let attempt = Directory::open_anchor(temp.path())?;
            crate::artifacts::seal(&attempt, "stop-intent.json", &steered, RECORD_LIMIT)?;
            let cancel = CancelToken::new();
            let (continued, cancelled) = with_cancellation(&attempt, scope, &cancel, || {
                // Several watcher polls read this scope's own Steered marker
                // and none of them cancels.
                let continued = provenant_test_support::wait_for(
                    "the steer marker to cancel the operation",
                    Duration::from_millis(250),
                    || cancel.is_cancelled(),
                    |cancelled| *cancelled,
                )
                .is_err();
                attempt
                    .atomic_write(
                        OsStr::new("stop-intent.json"),
                        ".run-",
                        &replacement,
                        PublishMode::Replace,
                    )?
                    .require_durable()
                    .map_err(RunError::Publication)?;
                let cancelled = provenant_test_support::wait_for(
                    "the replaced marker to cancel the operation",
                    Duration::from_secs(5),
                    || cancel.is_cancelled(),
                    |cancelled| *cancelled,
                )
                .is_ok();
                Ok((continued, cancelled))
            })?;
            assert!(
                continued,
                "{name}: the steer marker cancelled the operation"
            );
            assert!(
                cancelled,
                "{name}: the replacement did not cancel the operation"
            );
        }
        Ok(())
    }

    #[test]
    fn stalled_probe_requires_matching_identity_before_and_after() {
        use provenant_supervisor::census::ProcessFact;
        let identity = ProcessIdentity {
            boot: "fixture".into(),
            pid: 42,
            native_start: 1,
            group_id: 42,
            session_id: 42,
        };
        let fact = ProcessFact {
            identity: identity.clone(),
            parent_pid: 1,
            zombie: false,
            cpu_micros: None,
            command: None,
        };
        #[derive(Clone, Copy)]
        enum ProbeCase {
            Persistent,
            Transient,
            Running,
        }
        #[derive(Clone, Copy)]
        enum IdentityCase {
            Stable,
            LostAfterProbe,
        }
        for (probe_case, identity_case, expected) in [
            (ProbeCase::Persistent, IdentityCase::Stable, true),
            (ProbeCase::Transient, IdentityCase::Stable, false),
            (ProbeCase::Running, IdentityCase::Stable, false),
            (ProbeCase::Persistent, IdentityCase::LostAfterProbe, false),
        ] {
            let probe_calls = std::cell::Cell::new(0);
            let probe = |_| {
                let call = probe_calls.get();
                probe_calls.set(call + 1);
                Ok(match probe_case {
                    ProbeCase::Persistent => provenant_os::pid::ProcessState::Uninterruptible,
                    ProbeCase::Transient if call == 0 => {
                        provenant_os::pid::ProcessState::Uninterruptible
                    }
                    ProbeCase::Transient | ProbeCase::Running => {
                        provenant_os::pid::ProcessState::Running
                    }
                })
            };
            let observation_calls = std::cell::Cell::new(0);
            let observe = |_| {
                let call = observation_calls.get();
                observation_calls.set(call + 1);
                if matches!(identity_case, IdentityCase::Stable) || call == 0 {
                    ProcessObservation::Present(fact.clone())
                } else {
                    ProcessObservation::Absent
                }
            };
            assert_eq!(
                identity_stalled_with(&clock(), &identity, &observe, &probe),
                expected
            );
        }
    }

    #[test]
    fn abandon_requires_non_live_presence_free_lease_and_free_worktree() -> TestResult {
        // Spec: docs/specs/supervisor.md#explicit-abandonment
        use provenant_confinement::worktrees::{Occupancy, WorkerObserver};
        use provenant_supervisor::census::{
            Census, CensusError, ProcessFact, ProcessIdentity, ProcessObserver,
        };
        struct Observer(ProcessFact);
        impl ProcessObserver for Observer {
            fn boot_identity(&self) -> std::result::Result<String, CensusError> {
                Ok(self.0.identity.boot.clone())
            }
            fn observe(&self, _: i64) -> ProcessObservation {
                ProcessObservation::Present(self.0.clone())
            }
            fn census(&self, _: usize) -> std::result::Result<Census, CensusError> {
                Census::from_observations([self.0.clone()], [], [], true, false, 64)
            }
        }
        struct Workers(Occupancy);
        impl WorkerObserver for Workers {
            fn observe_roots(
                &self,
                roots: &[std::path::PathBuf],
            ) -> std::collections::BTreeMap<std::path::PathBuf, Occupancy> {
                roots
                    .iter()
                    .cloned()
                    .map(|root| (root, self.0.clone()))
                    .collect()
            }
        }
        for (live, occupancy, held, allowed) in [
            (true, Occupancy::Available, false, false),
            (false, Occupancy::Available, false, true),
            (false, Occupancy::Observed { process: None }, false, false),
            (
                false,
                Occupancy::census_unknown("/fixture".into(), "incomplete census"),
                false,
                false,
            ),
            (false, Occupancy::Available, true, false),
        ] {
            let temp = tempfile::tempdir()?;
            let clock = clock();
            let cancel = CancelToken::new();
            let call = crate::tests::call(&clock, &cancel);
            let context = crate::tests::context(temp.path())?;
            let store = crate::RunStore::open(&context, &call)?;
            let mut run = crate::tests::input();
            run.created_at = clock.wall_now()?;
            run.request.task.writer = Some(crate::WriterRequest {
                extra: Default::default(),
                custody_project: Some(context.project_id),
                project: temp.path().join("checkout"),
                worktree: temp.path().join("checkout/.worktrees/abandon"),
            });
            store.create(&run, &call)?;
            let id = run.attempt_id;
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
            let scope = crate::RunStore::scope(&record);
            let attempt = store.attempt_dir(id, false)?;
            let slot = store
                .attempt_writer_slot(id, &call)?
                .ok_or("missing writer slot")?;
            std::fs::create_dir_all(&slot)?;
            let slot = Directory::open_anchor(&slot)?;
            drop(provenant_supervisor::custody::acquire_reader(
                slot.try_clone()?,
                scope,
                call.lock_budget(),
                &cancel,
                &clock,
            )?);
            let lease = held
                .then(|| slot.try_lock(OsStr::new("writer.lock"), LockMode::Exclusive))
                .transpose()?;
            let observer = Observer(ProcessFact {
                identity: ProcessIdentity {
                    boot: "fixture-current-boot".into(),
                    pid: 123,
                    native_start: 1,
                    group_id: 123,
                    session_id: 123,
                },
                parent_pid: 1,
                zombie: false,
                cpu_micros: None,
                command: None,
            });
            if live {
                crate::artifacts::seal(
                    &attempt,
                    "process.json",
                    &serde_json::to_vec(&serde_json::json!({
                        "schema_version": 1, "scope": scope, "guardian": observer.0.identity,
                        "publication_pending": false, "census_complete": true,
                    }))?,
                    RECORD_LIMIT,
                )?;
            }
            crate::artifacts::seal(&attempt, "retained-evidence", b"keep", RECORD_LIMIT)?;
            let workers = Workers(occupancy.clone());
            let result = store.abandon_settlement_with_observer(
                id,
                "guardian lost",
                &workers,
                &call,
                &observer,
            );
            if allowed {
                assert!(matches!(result?, crate::Reconciled::Abandoned { .. }));
                assert!(matches!(
                    store.record(id, &call)?.state,
                    crate::AttemptState::Terminal {
                        outcome: crate::Outcome::Lost,
                        ..
                    }
                ));
                assert!(!provenant_supervisor::custody::reservation_present(
                    &slot, scope
                )?);
                // Same-boot repetition and a crash after Lost but before completion
                // both reuse the durable explicit intent.
                {
                    let _lock = journal_lock(&attempt)?;
                    let mut journal = read(&attempt, scope)?;
                    journal.phase = Phase::Record;
                    write(&attempt, &journal)?;
                }
                assert!(matches!(
                    store.abandon_settlement_with_observer(
                        id,
                        "guardian lost",
                        &workers,
                        &call,
                        &observer
                    )?,
                    crate::Reconciled::Abandoned { .. }
                ));
                assert!(
                    store
                        .abandon_settlement_with_observer(
                            id,
                            "different",
                            &workers,
                            &call,
                            &observer
                        )
                        .is_err()
                );
            } else {
                let Err(RunError::AbandonBlocked(evidence)) = result else {
                    panic!("expected typed abandon refusal");
                };
                match (&*evidence, live, held) {
                    (AbandonmentRefusal::Live { .. }, true, _)
                    | (AbandonmentRefusal::WriterLease { .. }, _, true) => {}
                    (AbandonmentRefusal::Occupancy { state, .. }, false, false) => {
                        assert_eq!(state, &occupancy)
                    }
                    _ => panic!("wrong refusal evidence: {evidence:?}"),
                }
                assert_eq!(store.record(id, &call)?, record);
                assert!(provenant_supervisor::custody::reservation_present(
                    &slot, scope
                )?);
            }
            assert_eq!(
                read_sealed(&attempt, "retained-evidence", RECORD_LIMIT)?.as_deref(),
                Some(b"keep".as_slice())
            );
            drop(lease);
        }
        Ok(())
    }

    #[test]
    fn stalled_supervisor_is_not_retried_and_is_rechecked() -> TestResult {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let store = crate::RunStore::open(&crate::tests::context(temp.path())?, &call)?;
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let id = run.attempt_id;
        store.queue_reconcile_mode_with_probe(
            run.attempt_id,
            &call,
            |_, _| panic!("queued lane does not need a worker"),
            false,
            &|_, _| panic!("queued lane has no supervisor"),
        )?;
        store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let attempt = store.attempt_dir(id, false)?;
        let scope = crate::RunStore::scope(&store.record(id, &call)?);
        for explicit in [false, true] {
            let found = store.queue_reconcile_mode_with_probe(
                run.attempt_id,
                &call,
                |_, _| panic!("stalled worker must not spawn"),
                explicit,
                &|_, _| true,
            )?;
            assert!(matches!(
                found.as_slice(),
                [(
                    _,
                    crate::Reconciled::Uncertain {
                        reason: crate::UncertainReason::HostStalled
                    }
                )]
            ));
            assert_eq!(read(&attempt, scope)?.recovery_failures, 0);
        }
        store.queue_reconcile_mode_with_probe(
            run.attempt_id,
            &call,
            |_, _| Ok(()),
            false,
            &|_, _| false,
        )?;
        assert!(
            read_sealed(&attempt, "settlement-queued.json", RECORD_LIMIT)?.is_some(),
            "recovery publishes a worker request after the stalled probe clears"
        );
        Ok(())
    }

    #[test]
    fn stalled_checkpoint_reports_lane_and_retains_custody() -> TestResult {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let store = crate::RunStore::open(&crate::tests::context(temp.path())?, &call)?;
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let id = run.attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let lane = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let attempt = store.attempt_dir(id, false)?;
        let scope = crate::RunStore::scope(&lane);
        let lease = provenant_supervisor::custody::acquire_reader(
            attempt.try_clone()?,
            scope,
            provenant_core::WaitBudget::nonblocking(clock.mono_now()),
            &cancel,
            &clock,
        )?;
        let identity = match SystemProcessObserver.observe(i64::from(std::process::id())) {
            ProcessObservation::Present(fact) => fact.identity,
            _ => return Err("missing fixture identity".into()),
        };
        crate::artifacts::seal(
            &attempt,
            "process.json",
            &serde_json::to_vec(
                &serde_json::json!({"schema_version":1, "scope":scope, "publication_pending":false, "guardian":identity}),
            )?,
            RECORD_LIMIT,
        )?;
        TEST_PROCESS_STATE
            .with(|state| state.set(Some(Ok(provenant_os::pid::ProcessState::Uninterruptible))));
        assert!(
            host_stalled(&attempt, scope),
            "checkpoint identifies stalled guardian"
        );
        assert!(matches!(
            lane.state,
            crate::AttemptState::Running { .. } | crate::AttemptState::Starting { .. }
        ));
        let waits = store.lane_waits(std::slice::from_ref(&lane), &call);
        assert_eq!(waits[0].waiting_for, crate::LaneWaitReason::HostStalled);
        let queued = store.queue_reconcile_with(run.attempt_id, &call, |_, _| {
            panic!("stalled worker must not spawn")
        })?;
        assert!(matches!(
            queued.as_slice(),
            [(
                _,
                crate::Reconciled::Uncertain {
                    reason: crate::UncertainReason::HostStalled
                }
            )]
        ));
        let reconciled = store.reconcile_with_observer(
            run.attempt_id,
            &crate::dispatch_tests::Fake::default(),
            &call,
            &SystemProcessObserver,
        )?;
        assert!(matches!(
            reconciled.as_slice(),
            [(
                _,
                crate::Reconciled::Uncertain {
                    reason: crate::UncertainReason::HostStalled
                }
            )]
        ));
        assert!(provenant_supervisor::custody::reservation_present(
            &attempt, scope
        )?);
        assert!(
            attempt
                .try_lock(OsStr::new("writer.lock"), LockMode::Exclusive)
                .is_err()
        );
        assert_eq!(read(&attempt, scope)?.recovery_failures, 0);
        // The guardian can be recorded in custody before its first checkpoint.
        let evidence = provenant_supervisor::custody::attempt_evidence(&attempt, scope)?;
        let mut reservation: serde_json::Value = serde_json::from_slice(
            &read_sealed(&evidence, "reservation.json", RECORD_LIMIT)?
                .ok_or("missing reservation")?,
        )?;
        let mut guardian = identity.clone();
        guardian.group_id = guardian.pid;
        guardian.session_id = guardian.pid;
        let metadata = attempt.metadata()?;
        reservation["guardian"] = serde_json::to_value(&guardian)?;
        reservation["launch_state"] = serde_json::json!("handing_off");
        reservation["attempt_device"] = serde_json::json!(metadata.device);
        reservation["attempt_inode"] = serde_json::json!(metadata.inode);
        evidence
            .atomic_write(
                OsStr::new("reservation.json"),
                ".fixture-",
                &serde_json::to_vec(&reservation)?,
                PublishMode::Replace,
            )?
            .require_durable()?;
        attempt.unlink(OsStr::new("process.json"))?;
        assert_eq!(
            provenant_supervisor::custody::reservation_guardian(&attempt, scope)?,
            Some(guardian)
        );
        assert!(store.supervisor_stalled(&attempt, scope, &call));
        assert!(matches!(
            store
                .reconcile_with_observer(
                    run.attempt_id,
                    &crate::dispatch_tests::Fake::default(),
                    &call,
                    &SystemProcessObserver
                )?
                .as_slice(),
            [(
                _,
                crate::Reconciled::Uncertain {
                    reason: crate::UncertainReason::HostStalled
                }
            )]
        ));
        assert!(provenant_supervisor::custody::reservation_present(
            &attempt, scope
        )?);
        TEST_PROCESS_STATE.with(|state| state.set(Some(Err(std::io::ErrorKind::PermissionDenied))));
        assert!(
            !store.supervisor_stalled(&attempt, scope, &call),
            "an unreadable probe is unknown"
        );
        let mut spawned = false;
        store.queue_reconcile_with(run.attempt_id, &call, |_, _| {
            spawned = true;
            Ok(())
        })?;
        assert!(spawned, "unknown state follows ordinary recovery");
        TEST_PROCESS_STATE.with(|state| state.set(None));
        drop(lease);
        Ok(())
    }

    #[test]
    fn reaction_only_workers_do_not_report_custody_recovery() -> TestResult {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let store = crate::RunStore::open(&crate::tests::context(temp.path())?, &call)?;
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let id = run.attempt_id;
        let record = store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        store.commit_lost(crate::RunStore::scope(&record), "fixture", &call)?;
        store.set_temporary_retained(id, false, &call)?;
        assert!(store.reaction_pending(run.attempt_id, &call)?);
        assert!(store.writer_fences()?.is_empty());
        let mut spawned = false;
        let found = store.queue_reconcile_mode(
            run.attempt_id,
            &call,
            |argv, _| {
                assert_eq!(argv[1..3], ["internal", "settle"]);
                spawned = true;
                Ok(())
            },
            true,
        )?;
        assert!(spawned);
        assert!(found.is_empty());
        assert!(
            read_sealed(
                &store.attempt_dir(run.attempt_id, false)?,
                "settlement-queued.json",
                RECORD_LIMIT
            )?
            .is_some()
        );
        store.drain_reactions(run.attempt_id, &clock, &cancel, &mut |reaction, _| {
            reaction.graph_done = true;
            reaction.notice_done = true;
            reaction.log_done = true;
            reaction.ledger_done = true;
            Ok(())
        })?;
        let registry = temp.path().join("broken-registry");
        std::fs::create_dir(&registry)?;
        std::fs::write(registry.join("worktrees"), b"not a directory")?;
        let store = store.with_writer_registry(&registry);
        assert!(
            store
                .queue_reconcile_with(run.attempt_id, &call, |_, _| {
                    panic!("an unlistable registry with no candidates must return its error")
                })
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn queued_abandonment_preserves_evidence_and_refuses_custody() -> TestResult {
        for fenced in [false, true] {
            let temp = tempfile::tempdir()?;
            let clock = crate::tests::clock();
            let cancel = CancelToken::new();
            let call = crate::tests::call(&clock, &cancel);
            let context = crate::tests::context(temp.path())?;
            let store = crate::RunStore::open(&context, &call)?;
            let run = crate::tests::input();
            store.create(&run, &call)?;
            let id = run.attempt_id;
            let attempt = store.attempt_dir(id, false)?;
            let scope = crate::RunStore::scope(&store.record(id, &call)?);
            crate::artifacts::seal(&attempt, "retained-evidence", b"keep", RECORD_LIMIT)?;
            if fenced {
                let slot = context
                    .state_root
                    .as_path()
                    .join("worktrees")
                    .join(context.project_id.to_string())
                    .join("fixture-slot");
                std::fs::create_dir_all(&slot)?;
                drop(provenant_supervisor::custody::acquire_reader(
                    Directory::open_anchor(&slot)?,
                    Scope {
                        generation: 1,
                        ..scope
                    },
                    call.lock_budget(),
                    &cancel,
                    &clock,
                )?);
            }
            let reason = "unresolvable retained route";
            let result = store.abandon_settlement(
                id,
                reason,
                &provenant_confinement::worktrees::SystemWorkerObserver,
                &call,
            );
            if fenced {
                assert!(result.is_err());
                assert!(matches!(
                    store.record(id, &call)?.state,
                    crate::AttemptState::Queued { .. }
                ));
                assert!(abandoned(&attempt, scope)?.is_none());
            } else {
                assert!(
                    matches!(result?, crate::Reconciled::Abandoned { reason: saved } if saved == reason)
                );
                assert!(matches!(store.record(id, &call)?.state,
                    crate::AttemptState::Terminal { outcome: crate::Outcome::Cancelled, reason: Some(saved), .. } if saved == reason));
                assert!(matches!(
                    store.abandon_settlement(
                        id,
                        reason,
                        &provenant_confinement::worktrees::SystemWorkerObserver,
                        &call
                    )?,
                    crate::Reconciled::Abandoned { .. }
                ));
                assert!(
                    store
                        .abandon_settlement(
                            id,
                            "different reason",
                            &provenant_confinement::worktrees::SystemWorkerObserver,
                            &call
                        )
                        .is_err()
                );
            }
            assert_eq!(
                read_sealed(&attempt, "retained-evidence", RECORD_LIMIT)?.as_deref(),
                Some(b"keep".as_slice())
            );
        }
        Ok(())
    }

    #[test]
    fn terminal_fence_without_retained_data_or_reactions_is_queued() -> TestResult {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let context = crate::tests::context(temp.path())?;
        let store = crate::RunStore::open(&context, &call)?;
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let id = run.attempt_id;
        let record = store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let scope = crate::RunStore::scope(&record);
        store.commit_lost(scope, "fixture", &call)?;
        store.set_temporary_retained(id, false, &call)?;
        store.drain_reactions(run.attempt_id, &clock, &cancel, &mut |reaction, _| {
            reaction.graph_done = true;
            reaction.notice_done = true;
            reaction.log_done = true;
            reaction.ledger_done = true;
            Ok(())
        })?;
        assert!(!store.reaction_pending(run.attempt_id, &call)?);
        let slot = context
            .state_root
            .as_path()
            .join("worktrees")
            .join(context.project_id.to_string())
            .join("fixture-slot");
        std::fs::create_dir_all(&slot)?;
        drop(provenant_supervisor::custody::acquire_reader(
            Directory::open_anchor(&slot)?,
            scope,
            call.lock_budget(),
            &cancel,
            &clock,
        )?);
        assert_eq!(store.writer_fences()?.len(), 1);
        let orphan = provenant_core::AttemptId::from_bytes([9; 16]);
        let orphan_slot = slot.with_file_name("orphan-slot");
        std::fs::create_dir(&orphan_slot)?;
        drop(provenant_supervisor::custody::acquire_reader(
            Directory::open_anchor(&orphan_slot)?,
            Scope {
                attempt: orphan,
                ..scope
            },
            call.lock_budget(),
            &cancel,
            &clock,
        )?);
        let mut spawned = false;
        let found = store.queue_reconcile_mode(
            run.attempt_id,
            &call,
            |_, _| {
                spawned = true;
                Ok(())
            },
            true,
        )?;
        assert!(spawned);
        assert_eq!(found.len(), 1);
        assert!(
            found.iter().any(|(found, state)| *found == id
                && matches!(state, crate::Reconciled::Settling { .. }))
        );
        assert!(found.iter().all(|(found, _)| *found != orphan));
        assert!(
            store
                .writer_fences()?
                .iter()
                .any(|fence| fence.scope.attempt == orphan)
        );
        assert!(
            read_sealed(
                &store.attempt_dir(run.attempt_id, false)?,
                "settlement-queued.json",
                RECORD_LIMIT
            )?
            .is_some()
        );
        Ok(())
    }

    #[test]
    fn retained_terminal_without_fence_is_queued() -> TestResult {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let store = crate::RunStore::open(&crate::tests::context(temp.path())?, &call)?;
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let id = run.attempt_id;
        let record = store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        store.commit_lost(crate::RunStore::scope(&record), "fixture", &call)?;
        store.drain_reactions(run.attempt_id, &clock, &cancel, &mut |reaction, _| {
            reaction.graph_done = true;
            reaction.notice_done = true;
            reaction.log_done = true;
            reaction.ledger_done = true;
            Ok(())
        })?;
        store.set_temporary_retained(id, true, &call)?;
        assert!(run.request.task.writer.is_none());
        assert!(!store.reaction_pending(run.attempt_id, &call)?);
        assert!(store.writer_fences()?.is_empty());
        let mut spawned = false;
        store.queue_reconcile_mode(
            run.attempt_id,
            &call,
            |argv, _| {
                assert_eq!(argv[1..3], ["internal", "settle"]);
                spawned = true;
                Ok(())
            },
            true,
        )?;
        assert!(spawned);
        let queued = store.attempt_dir(run.attempt_id, false)?;
        assert!(read_sealed(&queued, "settlement-queued.json", RECORD_LIMIT)?.is_some());
        Ok(())
    }

    #[test]
    fn pending_terminal_reaction_obeys_registry_failure_limit() -> TestResult {
        for retained in [false, true] {
            let temp = tempfile::tempdir()?;
            let mut clock = crate::tests::clock();
            let cancel = CancelToken::new();
            let registry = temp.path().join("registry");
            std::fs::create_dir(&registry)?;
            std::fs::write(registry.join("worktrees"), b"not a directory")?;
            let store = crate::RunStore::open(
                &crate::tests::context(temp.path())?,
                &crate::tests::call(&clock, &cancel),
            )?
            .with_writer_registry(&registry);
            let run = crate::tests::input();
            let call = crate::tests::call(&clock, &cancel);
            store.create(&run, &call)?;
            let id = run.attempt_id;
            let record = store.mark_starting(
                &store.record(id, &call)?,
                &crate::tests::route(),
                &[],
                &Default::default(),
                None,
                &call,
            )?;
            let scope = crate::RunStore::scope(&record);
            store.commit_lost(scope, "fixture", &call)?;
            store.set_temporary_retained(id, retained, &call)?;
            assert!(store.reaction_pending(run.attempt_id, &call)?);
            let attempt = store.attempt_dir(id, false)?;
            let mut spawned = false;
            store.queue_reconcile_mode(
                run.attempt_id,
                &call,
                |argv, _| {
                    assert_eq!(argv[1..3], ["internal", "settle"]);
                    spawned = true;
                    Ok(())
                },
                true,
            )?;
            assert!(spawned);
            let queued = store.attempt_dir(run.attempt_id, false)?;
            assert!(read_sealed(&queued, "settlement-queued.json", RECORD_LIMIT)?.is_some());
            queued.unlink(OsStr::new("settlement-queued.json"))?;
            for n in 1..=LIMIT {
                let call = crate::tests::call(&clock, &cancel);
                let found = store.reconcile_with_observer(
                    run.attempt_id,
                    &crate::dispatch_tests::Fake::default(),
                    &call,
                    &SystemProcessObserver,
                )?;
                assert!(matches!(
                    found.as_slice(),
                    [(_, crate::Reconciled::Uncertain { .. })]
                ));
                let journal = read(&attempt, scope)?;
                assert_eq!(journal.recovery_failures, n);
                let listed = store.queue_reconcile_with(run.attempt_id, &call, |_, _| {
                    panic!("cooldown must prevent spawn")
                })?;
                assert!(
                    matches!(listed.as_slice(), [(found, crate::Reconciled::Uncertain { .. })] if *found == id)
                );
                advance_to(&mut clock, journal.retry_after_ms)?;
                if n < LIMIT {
                    let call = crate::tests::call(&clock, &cancel);
                    let mut spawned = false;
                    store.queue_reconcile_with(run.attempt_id, &call, |_, _| {
                        spawned = true;
                        Ok(())
                    })?;
                    assert!(spawned);
                    assert!(
                        read_sealed(&queued, "settlement-queued.json", RECORD_LIMIT)?.is_some()
                    );
                    queued.unlink(OsStr::new("settlement-queued.json"))?;
                }
            }
            let call = crate::tests::call(&clock, &cancel);
            let listed = store.queue_reconcile_with(run.attempt_id, &call, |_, _| {
                panic!("failure limit must prevent spawn")
            })?;
            assert!(
                matches!(listed.as_slice(), [(found, crate::Reconciled::Uncertain { .. })] if *found == id)
            );
            assert!(read_sealed(&queued, "settlement-queued.json", RECORD_LIMIT)?.is_none());
            let mut spawned = false;
            store.queue_reconcile_mode(
                run.attempt_id,
                &call,
                |_, _| {
                    spawned = true;
                    Ok(())
                },
                true,
            )?;
            assert!(spawned);
            assert!(read_sealed(&queued, "settlement-queued.json", RECORD_LIMIT)?.is_some());
            assert_eq!(read(&attempt, scope)?.recovery_failures, 0);
            let retried = store.reconcile_with_observer(
                run.attempt_id,
                &crate::dispatch_tests::Fake::default(),
                &call,
                &SystemProcessObserver,
            )?;
            assert!(matches!(
                retried.as_slice(),
                [(_, crate::Reconciled::Uncertain { .. })]
            ));
            assert_eq!(read(&attempt, scope)?.recovery_failures, 1);
        }
        Ok(())
    }

    #[test]
    fn fence_listing_failure_queues_retained_terminal_attempt() -> TestResult {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let registry = temp.path().join("registry");
        std::fs::create_dir(&registry)?;
        std::fs::write(registry.join("worktrees"), b"not a directory")?;
        let store = crate::RunStore::open(&crate::tests::context(temp.path())?, &call)?
            .with_writer_registry(&registry);
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let id = run.attempt_id;
        let record = store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let scope = crate::RunStore::scope(&record);
        store.commit_lost(scope, "fixture", &call)?;
        store.set_temporary_retained(id, true, &call)?;
        let mut spawned = false;
        store.queue_reconcile_mode(
            run.attempt_id,
            &call,
            |_, _| {
                spawned = true;
                Ok(())
            },
            true,
        )?;
        assert!(spawned);
        let found = store.reconcile_with_observer(
            run.attempt_id,
            &crate::dispatch_tests::Fake::default(),
            &call,
            &SystemProcessObserver,
        )?;
        assert!(matches!(
            found.as_slice(),
            [(_, crate::Reconciled::Uncertain { .. })]
        ));
        let journal = read(&store.attempt_dir(id, false)?, scope)?;
        assert_eq!(journal.recovery_failures, 1);
        assert!(journal.blocking_reason.is_some());
        Ok(())
    }

    #[test]
    fn fence_listing_failure_is_reported_and_bounded() -> TestResult {
        let temp = tempfile::tempdir()?;
        let mut clock = clock();
        let cancel = CancelToken::new();
        let registry = temp.path().join("registry");
        std::fs::create_dir(&registry)?;
        std::fs::write(registry.join("worktrees"), b"not a directory")?;
        let store = crate::RunStore::open(
            &crate::tests::context(temp.path())?,
            &crate::tests::call(&clock, &cancel),
        )?
        .with_writer_registry(&registry);
        let run = crate::tests::input();
        let call = crate::tests::call(&clock, &cancel);
        store.create(&run, &call)?;
        let id = run.attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let attempt = store.attempt_dir(id, false)?;
        let scope = crate::RunStore::scope(&store.record(id, &call)?);
        for n in 1..=LIMIT {
            let call = crate::tests::call(&clock, &cancel);
            let found = store.reconcile_with_observer(
                run.attempt_id,
                &crate::dispatch_tests::Fake::default(),
                &call,
                &SystemProcessObserver,
            )?;
            assert!(matches!(
                found.as_slice(),
                [(_, crate::Reconciled::Uncertain { .. })]
            ));
            let journal = read(&attempt, scope)?;
            assert_eq!(journal.recovery_failures, n);
            assert!(journal.blocking_reason.is_some());
            let queued = store.queue_reconcile_with(run.attempt_id, &call, |_, _| {
                panic!("retry must be delayed")
            })?;
            assert!(
                matches!(queued.as_slice(), [(_, crate::Reconciled::Uncertain { reason })] if reason.to_string() == journal.blocking_reason.clone().unwrap())
            );
            advance_to(&mut clock, journal.retry_after_ms)?;
        }
        let call = crate::tests::call(&clock, &cancel);
        store.queue_reconcile_with(run.attempt_id, &call, |_, _| panic!("retry limit"))?;
        let mut spawned = false;
        store.queue_reconcile_mode(
            run.attempt_id,
            &call,
            |_, _| {
                spawned = true;
                Ok(())
            },
            true,
        )?;
        assert!(spawned);
        assert_eq!(read(&attempt, scope)?.recovery_failures, 0);
        Ok(())
    }

    #[test]
    fn detached_process_budget_refusal_is_typed_and_clears_marker() -> TestResult {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let store = crate::RunStore::open(&crate::tests::context(temp.path())?, &call)?;
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let id = run.attempt_id;
        let queued_lane = store.record(id, &call)?;
        store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let result = store.queue_reconcile_with(run.attempt_id, &call, |_, _| {
            Err(provenant_os::exec::ExecError::ProcessBudget {
                used: 950,
                limit: 1000,
            })
        });
        let error = result.unwrap_err();
        assert!(matches!(
            error,
            RunError::Admission(AdmissionError::Processes {
                used: 950,
                limit: 1000
            })
        ));
        assert_eq!(crate::ErrorKind::from(&error), crate::ErrorKind::Processes);
        let waits = store.lane_waits_with_resources(&[queued_lane], &call, &crate::SystemResources);
        assert_eq!(
            waits[0].waiting_for,
            crate::LaneWaitReason::RecoveryWorkerRefused {
                used: 950,
                limit: 1000
            }
        );
        let mut retried = false;
        store.queue_reconcile_with(run.attempt_id, &call, |_, _| {
            retried = true;
            Ok(())
        })?;
        assert!(retried, "failed spawn must remove its queued marker");
        assert!(store.recovery_process_wait(run.attempt_id, &call).is_none());
        Ok(())
    }

    #[test]
    fn preclaim_failures_report_the_cause_bound_background_spawns_and_allow_explicit_retry()
    -> TestResult {
        use crate::{AttemptState, Reconciled, RunStore};
        let temp = tempfile::tempdir()?;
        let mut clock = clock();
        let cancel = CancelToken::new();
        let store = RunStore::open(
            &crate::tests::context(temp.path())?,
            &crate::tests::call(&clock, &cancel),
        )?;
        let run = crate::tests::input();
        let call = crate::tests::call(&clock, &cancel);
        store.create(&run, &call)?;
        let id = run.attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let attempt = store.attempt_dir(id, false)?;
        crate::artifacts::seal(&attempt, "launch.json", b"{}", RECORD_LIMIT)?;
        let scope = RunStore::scope(&store.record(id, &call)?);
        for index in 1..=LIMIT {
            let call = crate::tests::call(&clock, &cancel);
            let found = store.reconcile_with_observer(
                run.attempt_id,
                &crate::dispatch_tests::Fake::default(),
                &call,
                &SystemProcessObserver,
            )?;
            assert!(matches!(
                found.as_slice(),
                [(_, Reconciled::Uncertain { .. })]
            ));
            let journal = read(&attempt, scope)?;
            assert_eq!(journal.attempts, 0, "failure precedes import claim");
            assert_eq!(journal.recovery_failures, index);
            assert!(journal.owner.is_none());
            let queued = store.queue_reconcile_with(run.attempt_id, &call, |_, _| {
                panic!("cooldown must prevent another worker");
            })?;
            assert!(matches!(
                queued.as_slice(),
                [(
                    _,
                    Reconciled::Uncertain {
                        reason: crate::UncertainReason::ReconcileFailed { .. }
                    }
                )]
            ));
            advance_to(&mut clock, journal.retry_after_ms)?;
        }
        let call = crate::tests::call(&clock, &cancel);
        for _ in 0..4 {
            store.queue_reconcile_with(run.attempt_id, &call, |_, _| {
                panic!("exhausted recovery must stay bounded after cooldown");
            })?;
        }
        store.queue_reconcile_mode(run.attempt_id, &call, |_, _| Ok(()), true)?;
        assert!(
            read_sealed(&attempt, "settlement-queued.json", RECORD_LIMIT)?.is_some(),
            "an explicit reconcile publishes another bounded recovery request"
        );
        assert_eq!(read(&attempt, scope)?.recovery_failures, 0);
        attempt.unlink(OsStr::new("launch.json"))?;
        crate::artifacts::seal(
            &attempt,
            "launch.json",
            &serde_json::to_vec(&serde_json::json!({
                "schema_version":1, "scope":scope, "requester": store.context.actor_id
            }))?,
            RECORD_LIMIT,
        )?;
        let ProcessObservation::Present(current) = provenant_supervisor::census::current_process()
        else {
            return Err("current process identity unavailable".into());
        };
        crate::artifacts::seal(
            &attempt,
            "process.json",
            &serde_json::to_vec(&serde_json::json!({
                "schema_version":1, "scope":scope, "publication_pending":false, "guardian":current.identity
            }))?,
            RECORD_LIMIT,
        )?;
        let found = store.reconcile_with_observer(
            run.attempt_id,
            &crate::dispatch_tests::Fake::default(),
            &call,
            &SystemProcessObserver,
        )?;
        assert!(matches!(found.as_slice(), [(_, Reconciled::Held)]));
        assert!(
            read(&attempt, scope)?.blocking_reason.is_none(),
            "a live holder clears an obsolete failure"
        );
        attempt.unlink(OsStr::new("launch.json"))?;
        attempt.unlink(OsStr::new("process.json"))?;
        store.reconcile_with_observer(
            run.attempt_id,
            &crate::dispatch_tests::Fake::default(),
            &call,
            &SystemProcessObserver,
        )?;
        assert!(matches!(
            store.record(id, &call)?.state,
            AttemptState::Terminal { .. }
        ));
        Ok(())
    }

    fn scope() -> Scope {
        Scope {
            attempt: AttemptId::from_bytes([2; 16]),
            generation: 1,
        }
    }

    #[test]
    fn persisted_settlement_accepts_future_fields_and_still_checks_scope_and_version() -> TestResult
    {
        let temp = tempfile::tempdir()?;
        let attempt = Directory::open_anchor(temp.path())?;
        let mut record = serde_json::json!({
            "schema_version": 1,
            "scope": scope(),
            "phase": "import",
            "attempts": 0,
            "head": null,
            "base": null,
            "owner": null,
            "future_progress": {"value": true}
        });
        let persist = |value: &serde_json::Value| -> TestResult {
            attempt
                .atomic_write(
                    OsStr::new(FILE),
                    ".settlement-test-",
                    &serde_json::to_vec(value)?,
                    PublishMode::Replace,
                )?
                .require_durable()?;
            Ok(())
        };
        persist(&record)?;
        assert_eq!(read(&attempt, scope())?.phase, Phase::Import);
        let mut other = scope();
        other.generation += 1;
        assert!(read(&attempt, other).is_err());
        record["schema_version"] = serde_json::json!(2);
        persist(&record)?;
        assert!(read(&attempt, scope()).is_err());
        Ok(())
    }

    #[test]
    fn progress_and_pinned_commit_survive_a_new_driver() -> TestResult {
        let mut clock = clock();
        let temp = tempfile::tempdir()?;
        let attempt = Directory::open_anchor(temp.path())?;
        let head = GitOid::parse(&"a".repeat(40))?;
        let base = GitOid::parse(&"b".repeat(40))?;
        let mut first = Guard::claim(&attempt, scope(), &clock)?.expect("initial claim");
        first.set_head(head)?;
        first.set_base(base)?;
        first.advance(Phase::Record)?;
        drop(first);
        advance_to(&mut clock, read(&attempt, scope())?.retry_after_ms)?;
        let mut resumed = Guard::claim(&attempt, scope(), &clock)?.expect("resumed claim");
        assert_eq!(resumed.phase(), Phase::Record);
        assert_eq!(resumed.head(), Some(head));
        assert_eq!(resumed.base(), Some(base));
        assert_eq!(resumed.attempts(), 0);
        assert!(matches!(
            resumed.set_head(base),
            Err(RunError::Validation(
                "settlement head changed after pinning"
            ))
        ));
        resumed.advance(Phase::Publish)?;
        assert_eq!(resumed.phase(), Phase::Record);
        Ok(())
    }

    #[test]
    fn native_live_owner_excludes_another_driver_and_pid_reuse_resumes() -> TestResult {
        let mut clock = clock();
        let temp = tempfile::tempdir()?;
        let attempt = Directory::open_anchor(temp.path())?;
        let first = Guard::claim(&attempt, scope(), &clock)?.expect("initial claim");
        assert!(Guard::claim(&attempt, scope(), &clock)?.is_none());
        let mut crashed = read(&attempt, scope())?;
        let owner = crashed.owner.as_mut().expect("owner");
        owner.native_start = owner.native_start.wrapping_add(1);
        write(&attempt, &crashed)?;
        drop(first);
        advance_to(&mut clock, crashed.retry_after_ms)?;
        let resumed = Guard::claim(&attempt, scope(), &clock)?.unwrap_or_else(|| {
            panic!(
                "stale native identity not recovered: {:?}; journal={:?}",
                refusal(&attempt, scope(), &clock),
                read(&attempt, scope())
            )
        });
        assert_eq!(resumed.attempts(), 0);
        Ok(())
    }

    #[test]
    fn claims_before_import_do_not_consume_the_import_retry_budget() -> TestResult {
        let clock = clock();
        let temp = tempfile::tempdir()?;
        let attempt = Directory::open_anchor(temp.path())?;
        let driver = Guard::claim(&attempt, scope(), &clock)?.expect("initial claim");
        assert_eq!(driver.attempts(), 0, "claiming has not reached import");
        Ok(())
    }

    #[test]
    fn interrupted_settlement_has_one_durable_retry_cap() -> TestResult {
        let mut clock = clock();
        let temp = tempfile::tempdir()?;
        let attempt = Directory::open_anchor(temp.path())?;
        for expected in 1..=LIMIT {
            let now = read(&attempt, scope())?.retry_after_ms.max(now_ms(&clock)?);
            advance_to(&mut clock, now)?;
            let mut driver = Guard::claim(&attempt, scope(), &clock)?.expect("claim within cap");
            driver.begin_import()?;
            driver.begin_import()?;
            assert_eq!(driver.attempts(), expected);
            drop(driver);
        }
        assert!(matches!(
            Guard::claim(&attempt, scope(), &clock),
            Err(RunError::ObjectImportLimit(
                "settlement exhausted three attempts; private Git retained for host recovery"
            ))
        ));
        Ok(())
    }

    #[test]
    fn retry_claims_wait_without_spending_import_attempts() -> TestResult {
        let mut clock = clock();
        let temp = tempfile::tempdir()?;
        let attempt = Directory::open_anchor(temp.path())?;
        for expected in 1..=LIMIT {
            let now = read(&attempt, scope())?.retry_after_ms.max(now_ms(&clock)?);
            advance_to(&mut clock, now)?;
            let mut driver = Guard::claim(&attempt, scope(), &clock)?.expect("eligible claim");
            driver.begin_import()?;
            let dropped_at = now_ms(&clock)?;
            drop(driver);
            let journal = read(&attempt, scope())?;
            if expected < LIMIT {
                advance_to(&mut clock, journal.retry_after_ms - 1)?;
                assert!(Guard::claim(&attempt, scope(), &clock)?.is_none());
                assert_eq!(read(&attempt, scope())?.attempts, expected);
            }
            assert_eq!(journal.retry_after_ms, dropped_at + backoff_ms(expected));
        }
        Ok(())
    }

    #[test]
    fn settlement_phase_write_failure_preserves_checkpoint_and_lease() -> TestResult {
        let clock = clock();
        let temp = tempfile::tempdir()?;
        let attempt = Directory::open_anchor(temp.path())?;
        let mut driver = Guard::claim(&attempt, scope(), &clock)?.ok_or("initial claim")?;
        let before = std::fs::read(temp.path().join(FILE))?;
        TEST_FAIL_WRITE.with(|fail| fail.set(true));
        assert!(matches!(
            driver.advance(Phase::Publish),
            Err(RunError::Io(_))
        ));
        assert_eq!(driver.phase(), Phase::Import);
        assert_eq!(std::fs::read(temp.path().join(FILE))?, before);
        assert_eq!(read(&attempt, scope())?.phase, Phase::Import);
        assert!(Guard::claim(&attempt, scope(), &clock)?.is_none());
        Ok(())
    }

    #[test]
    fn settlement_reclaims_dropped_owner_after_failed_journal_clear() -> TestResult {
        let clock = clock();
        let temp = tempfile::tempdir()?;
        let attempt = Directory::open_anchor(temp.path())?;
        let driver = Guard::claim(&attempt, scope(), &clock)?.ok_or("initial claim")?;
        assert_eq!(
            refusal(&attempt, scope(), &clock)?,
            Some(Refusal::LiveOwner)
        );
        clock.wait_until(MonoTime::from_micros(5_000_000))?;
        assert!(
            Guard::claim(&attempt, scope(), &clock)?.is_none(),
            "live lease excludes after deadline"
        );
        let lock = try_progress_lock(&attempt, "settlement.lock")?;
        assert_eq!(
            refusal(&attempt, scope(), &clock)?,
            Some(Refusal::JournalLock)
        );
        drop(driver);
        drop(lock);
        assert!(
            read(&attempt, scope())?.owner.is_some(),
            "failed Drop preserved journal owner"
        );
        let reclaimed =
            Guard::claim(&attempt, scope(), &clock)?.ok_or("abandoned same-process owner")?;
        drop(reclaimed);
        assert!(matches!(
            refusal(&attempt, scope(), &clock)?,
            Some(Refusal::RetryDeadline(_))
        ));
        clock.wait_until(MonoTime::from_micros(10_000_000))?;
        assert!(Guard::claim(&attempt, scope(), &clock)?.is_some());
        Ok(())
    }

    #[test]
    fn claim_reports_the_lock_that_refused_reclaim() -> TestResult {
        let clock = clock();
        let temp = tempfile::tempdir()?;
        let attempt = Directory::open_anchor(temp.path())?;
        let driver = Guard::claim(&attempt, scope(), &clock)?.ok_or("initial claim")?;
        drop(driver);
        clock.wait_until(MonoTime::from_micros(120_000_000))?;
        let lock = try_progress_lock(&attempt, "settlement.lock")?;
        let (claim, reason) = Guard::claim_observed(&attempt, scope(), &clock)?;
        assert!(claim.is_none());
        assert_eq!(reason, Some(ClaimRefusal::JournalLock));
        drop(lock);
        assert!(Guard::claim(&attempt, scope(), &clock)?.is_some());
        Ok(())
    }

    #[test]
    fn dropped_driver_uses_injected_clock_for_retry() -> TestResult {
        let clock = clock();
        let temp = tempfile::tempdir()?;
        let attempt = Directory::open_anchor(temp.path())?;
        let driver = Guard::claim(&attempt, scope(), &clock)?.expect("claim");
        clock.wait_until(MonoTime::from_micros(2_000_000))?;
        drop(driver);
        assert_eq!(read(&attempt, scope())?.retry_after_ms, 7_000);
        assert!(Guard::claim(&attempt, scope(), &clock)?.is_none());
        clock.wait_until(MonoTime::from_micros(7_000_000))?;
        assert!(Guard::claim(&attempt, scope(), &clock)?.is_some());
        Ok(())
    }

    #[test]
    fn unknown_owner_expires_after_ten_minutes() -> TestResult {
        use provenant_supervisor::census::ObservationIssue;
        let clock = clock();
        let temp = tempfile::tempdir()?;
        let attempt = Directory::open_anchor(temp.path())?;
        let driver = Guard::claim(&attempt, scope(), &clock)?.expect("claim");
        let unknown = || ProcessObservation::Unknown(ObservationIssue::Malformed);
        assert!(observed_alive(
            &driver.owner,
            unknown(),
            100,
            100 + UNKNOWN_OWNER_LIMIT_MS - 1
        ));
        assert!(!observed_alive(
            &driver.owner,
            unknown(),
            100,
            100 + UNKNOWN_OWNER_LIMIT_MS
        ));
        Ok(())
    }

    #[test]
    fn import_is_accounted_once_and_driver_drop_releases_owner() -> TestResult {
        let clock = clock();
        let temp = tempfile::tempdir()?;
        let attempt = Directory::open_anchor(temp.path())?;
        let mut driver = Guard::claim(&attempt, scope(), &clock)?.expect("claim");
        driver.begin_import()?;
        driver.begin_import()?;
        assert_eq!(driver.attempts(), 1);
        drop(driver);
        assert!(
            read(&attempt, scope())?.owner.is_none(),
            "drop releases durable ownership"
        );
        Ok(())
    }

    #[test]
    fn settlement_records_preserve_unknown_fields_through_bounded_decoding() -> TestResult {
        let attempt = AttemptId::from_bytes([3; 16]);
        let expiry = WallTime::from_micros(100).unwrap();
        let mut journal = serde_json::to_value(Journal {
            schema_version: 1,
            scope: scope(),
            phase: Phase::Import,
            attempts: 0,
            head: None,
            base: None,
            owner: None,
            owner_since_ms: 0,
            retry_after_ms: 0,
            recovery_failures: 0,
            blocking_reason: None,
            abandonment: Some(Abandonment {
                reason: "fixture".into(),
                actor: provenant_core::ActorId::from_bytes([4; 16]),
                at: expiry,
                phase: Phase::Import,
                declare_lost: false,
                extra: Default::default(),
            }),
            extra: Default::default(),
        })?;
        journal["future_journal"] = serde_json::json!({"preserved": true});
        journal["abandonment"]["future_abandonment"] = serde_json::json!([1, 2, 3]);
        let decoded = crate::db::decode::<Journal>(&serde_json::to_vec(&journal)?)?;
        let encoded: serde_json::Value = serde_json::from_slice(&crate::db::bytes(&decoded)?)?;
        assert_eq!(encoded["future_journal"], journal["future_journal"]);
        assert_eq!(
            encoded["abandonment"]["future_abandonment"],
            journal["abandonment"]["future_abandonment"]
        );

        let mut abandonment = serde_json::to_value(QueuedAbandonment {
            scope: scope(),
            reason: "fixture".into(),
            extra: Default::default(),
        })?;
        abandonment["future"] = serde_json::json!("retained");
        let encoded: serde_json::Value =
            serde_json::from_slice(&crate::db::bytes(&crate::db::decode_column::<
                QueuedAbandonment,
            >(&serde_json::to_vec(
                &abandonment,
            )?)?)?)?;
        assert_eq!(encoded["future"], abandonment["future"]);

        let mut queued = serde_json::to_value(QueuedWorker {
            schema_version: 2,
            attempt,
            expires: expiry,
            extra: Default::default(),
        })?;
        queued["future"] = serde_json::json!("retained");
        let queued_bytes = serde_json::to_vec(&queued)?;
        let decoded = QueuedWorker::decode(&queued_bytes, attempt)?;
        let encoded: serde_json::Value = serde_json::from_slice(&crate::db::bytes(&decoded)?)?;
        assert_eq!(encoded["future"], queued["future"]);

        let mut refusal = serde_json::to_value(WorkerProcessRefusal {
            schema_version: 1,
            attempt,
            expires: expiry,
            used: 1,
            limit: 2,
            extra: Default::default(),
        })?;
        refusal["future"] = serde_json::json!("retained");
        let decoded = crate::db::decode::<WorkerProcessRefusal>(&serde_json::to_vec(&refusal)?)?;
        let encoded: serde_json::Value = serde_json::from_slice(&crate::db::bytes(&decoded)?)?;
        assert_eq!(encoded["future"], refusal["future"]);

        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let store = crate::RunStore::open(&crate::tests::context(temp.path())?, &call)?;
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let directory = store.attempt_dir(run.attempt_id, false)?;
        let scope = crate::RunStore::scope(&store.record(run.attempt_id, &call)?);
        let reason = "fixture abandonment";
        let extra = std::collections::BTreeMap::from([(
            "future_abandonment".to_owned(),
            serde_json::json!("retained"),
        )]);
        crate::artifacts::seal(
            &directory,
            "queued-abandonment.json",
            &crate::db::bytes(&QueuedAbandonment {
                scope,
                reason: reason.into(),
                extra,
            })?,
            RECORD_LIMIT,
        )?;
        store.abandon_settlement(
            run.attempt_id,
            reason,
            &provenant_confinement::worktrees::SystemWorkerObserver,
            &call,
        )?;
        let stored: serde_json::Value = serde_json::from_slice(
            &read_sealed(&directory, "queued-abandonment.json", RECORD_LIMIT)?
                .ok_or("queued abandonment marker missing")?,
        )?;
        assert_eq!(stored["future_abandonment"], "retained");
        Ok(())
    }
}

pub(crate) fn pinned_head(attempt: &Directory, scope: Scope) -> Result<Option<GitOid>> {
    read(attempt, scope).map(|journal| journal.head)
}

pub(crate) fn pinned_base(attempt: &Directory, scope: Scope) -> Result<Option<GitOid>> {
    read(attempt, scope).map(|journal| journal.base)
}

pub(crate) fn complete(attempt: &Directory, scope: Scope) -> Result<()> {
    let _lock = journal_lock(attempt)?;
    let mut journal = read(attempt, scope)?;
    journal.phase = Phase::Complete;
    journal.owner = None;
    journal.blocking_reason = None;
    journal.recovery_failures = 0;
    write(attempt, &journal)
}

pub(crate) fn settling(attempt: &Directory, scope: Scope) -> Result<crate::Reconciled> {
    let journal = read(attempt, scope)?;
    Ok(crate::Reconciled::Settling {
        phase: format!("{:?}", journal.phase).to_ascii_lowercase(),
        blocking_reason: journal.blocking_reason,
    })
}

pub(crate) fn abandoned(attempt: &Directory, scope: Scope) -> Result<Option<crate::Reconciled>> {
    let journal = read(attempt, scope)?;
    if journal.phase != Phase::Complete {
        return Ok(None);
    }
    Ok(journal
        .abandonment
        .map(|a| crate::Reconciled::Abandoned { reason: a.reason }))
}

/// A withdrawal may commit before its final abandonment journal is durable.
/// Only its unchanged generation and never-launched terminal row protect evidence.
pub(crate) fn queued_abandonment_pending(
    attempt: &Directory,
    record: &crate::AttemptRecord,
) -> Result<bool> {
    if record.provider_launched != Some(false)
        || !matches!(
            record.state,
            crate::AttemptState::Terminal {
                outcome: crate::Outcome::Cancelled,
                ..
            }
        )
    {
        return Ok(false);
    }
    Ok(
        read_sealed(attempt, "queued-abandonment.json", RECORD_LIMIT)?
            .map(|raw| crate::db::decode_column::<QueuedAbandonment>(&raw))
            .transpose()?
            .is_some_and(|intent| intent.scope == crate::RunStore::scope(record)),
    )
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct QueuedAbandonment {
    scope: Scope,
    reason: String,
    #[serde(default, flatten)]
    extra: provenant_records::ExtraFields,
}

/// Evidence that prevents an explicit loss declaration. Unknown occupancy is
/// retained verbatim so the chair can resolve the particular census blocker.
#[derive(Debug, thiserror::Error)]
pub enum AbandonmentRefusal {
    #[error("attempt {scope:?} presence is live")]
    Live { scope: Scope },
    #[error("writer exclusion is unavailable: {evidence}")]
    WriterLease { evidence: String },
    #[error("worktree {path:?} is {state:?}: {evidence}")]
    Occupancy {
        path: std::path::PathBuf,
        state: provenant_confinement::worktrees::Occupancy,
        evidence: String,
    },
}

fn abandon_refused(evidence: AbandonmentRefusal) -> RunError {
    RunError::AbandonBlocked(Box::new(evidence))
}

impl crate::RunStore {
    fn require_unstarted_abandonment(
        &self,
        record: &crate::AttemptRecord,
        attempt: &Directory,
        call: &crate::Request<'_>,
    ) -> Result<()> {
        let listing = self.writer_fence_listing()?;
        if !listing.issues.is_empty()
            || record.started_at.is_some()
            || record.provider_launched == Some(true)
            || record.exit_evidence.is_some()
            || record.resolved_route.is_some()
            || record.custody_pending == Some(true)
            || listing
                .reservations
                .iter()
                .any(|f| f.scope.attempt == record.attempt_id)
        {
            return Err(RunError::Validation(
                "abandonment requires never-dispatched queued attempt",
            ));
        }
        if provenant_supervisor::custody::reservation_present(attempt, Self::scope(record))? {
            return Err(RunError::Validation(
                "abandonment requires absent attempt custody",
            ));
        }
        for name in ["launch.json", "process.json", "process-exit.json"] {
            if read_sealed(attempt, name, RECORD_LIMIT)?.is_some() {
                return Err(RunError::Validation(
                    "abandonment requires absent process evidence",
                ));
            }
        }
        if let Some(slot) = self.attempt_writer_slot(record.attempt_id, call)? {
            match Directory::open_anchor(&slot) {
                Ok(slot) => {
                    if provenant_supervisor::custody::reservation_present(
                        &slot,
                        Self::scope(record),
                    )? {
                        return Err(RunError::Validation(
                            "abandonment requires absent writer custody",
                        ));
                    }
                }
                Err(error) if error.is_not_found() => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    /// Explicit chair recovery. Native writer exclusion and proven absence are
    /// mandatory; abandonment never removes attempt evidence or private Git.
    pub fn abandon_settlement(
        &self,
        id: provenant_core::AttemptId,
        reason: &str,
        workers: &dyn provenant_confinement::worktrees::WorkerObserver,
        call: &crate::Request<'_>,
    ) -> Result<crate::Reconciled> {
        self.abandon_settlement_with_observer(id, reason, workers, call, &SystemProcessObserver)
    }

    fn abandon_settlement_with_observer(
        &self,
        id: provenant_core::AttemptId,
        reason: &str,
        workers: &dyn provenant_confinement::worktrees::WorkerObserver,
        call: &crate::Request<'_>,
        observer: &dyn provenant_supervisor::census::ProcessObserver,
    ) -> Result<crate::Reconciled> {
        call.check()?;
        if reason.trim().is_empty() || reason.len() > 4096 {
            return Err(RunError::Validation(
                "abandonment reason must contain 1..4096 bytes",
            ));
        }
        let reason = provenant_core::bounded_line(reason, 4096);
        let mut record = self.record(id, call)?;
        let attempt = self.attempt_dir(id, false)?;
        let _driver = try_progress_lock(&attempt, "settlement-driver.lock").map_err(|error| {
            abandon_refused(AbandonmentRefusal::WriterLease {
                evidence: format!("settlement-driver.lock: {error}"),
            })
        })?;
        let queued = matches!(record.state, crate::AttemptState::Queued { .. });
        // A cancelled, never-launched terminal row also permits retry after a
        // crash between withdrawal and the abandonment journal publication.
        let intent = read_sealed(&attempt, "queued-abandonment.json", RECORD_LIMIT)?
            .map(|raw| crate::db::decode_column::<QueuedAbandonment>(&raw))
            .transpose()?;
        let unstarted =
            queued
                || intent.as_ref().is_some_and(|saved| {
                    saved.scope == Self::scope(&record) && saved.reason == reason
                }) && matches!(
                    record.state,
                    crate::AttemptState::Terminal {
                        outcome: crate::Outcome::Cancelled,
                        ..
                    }
                ) && record.provider_launched == Some(false);
        let unfenced = if unstarted {
            self.require_unstarted_abandonment(&record, &attempt, call)?;
            true
        } else {
            false
        };
        if queued {
            let extra = intent
                .as_ref()
                .map(|saved| saved.extra.clone())
                .unwrap_or_default();
            crate::artifacts::seal(
                &attempt,
                "queued-abandonment.json",
                &crate::db::bytes(&QueuedAbandonment {
                    scope: Self::scope(&record),
                    reason: reason.clone(),
                    extra,
                })?,
                RECORD_LIMIT,
            )?;
            record = self
                .withdraw_preserving_evidence(
                    id,
                    crate::store::Withdrawal {
                        expected: record.state.clone(),
                        outcome: crate::Outcome::Cancelled,
                        reason: &reason,
                        finished_at: call
                            .clock
                            .wall_now()
                            .map_err(|_| RunError::Admission(AdmissionError::Clock))?,
                        failure: None,
                        terminal_cause: None,
                        terminal_note: None,
                    },
                    call,
                )?
                .ok_or(RunError::Conflict)?;
        }
        let nonterminal = !matches!(record.state, crate::AttemptState::Terminal { .. });
        let scope = Self::scope(&record);
        let mut journal = read(&attempt, scope)?;
        let declare_lost =
            nonterminal || journal.abandonment.as_ref().is_some_and(|a| a.declare_lost);
        if journal
            .abandonment
            .as_ref()
            .is_some_and(|a| a.reason != reason)
        {
            return Err(RunError::Conflict);
        }
        if declare_lost
            && provenant_supervisor::custody::observe_holder(&attempt, scope, observer)
                == provenant_supervisor::custody::HolderPresence::Live
        {
            return Err(abandon_refused(AbandonmentRefusal::Live { scope }));
        }
        // Exclude a still-starting submitter as well as a handed-off guardian.
        let mut launch_guards = Vec::new();
        if declare_lost {
            for name in ["launcher.lock", "custody.lock"] {
                launch_guards.push(
                    attempt
                        .try_lock(OsStr::new(name), LockMode::Exclusive)
                        .map_err(|error| {
                            abandon_refused(AbandonmentRefusal::WriterLease {
                                evidence: format!("{name}: {error}"),
                            })
                        })?,
                );
            }
        }
        let now = now_ms(call.clock)?;
        if journal.owner.as_ref().is_some_and(|owner| {
            owner.pid != i64::from(std::process::id()) && alive(owner, journal.owner_since_ms, now)
        }) {
            return Err(RunError::Contention);
        }
        let fence = self.writer_fences()?.into_iter().find(|f| f.scope == scope);
        let mut unfenced_lease = None;
        let recovery = if let Some(fence) = fence {
            let slot = fence
                .custody_directory()
                .ok_or(RunError::Validation("writer fence slot missing"))?;
            let recovery = provenant_supervisor::custody::acquire_recovery(
                Directory::open_anchor(slot)?,
                scope,
                provenant_core::WaitBudget::nonblocking(call.clock.mono_now()),
                call.cancel,
                call.clock,
            )
            .map_err(|error| {
                abandon_refused(AbandonmentRefusal::WriterLease {
                    evidence: error.to_string(),
                })
            })?;
            if recovery.scope() != scope {
                return Err(RunError::Conflict);
            }
            if !declare_lost
                && recovery.observe() != provenant_supervisor::custody::RecoveryState::Absent
            {
                return Err(RunError::Validation(
                    "abandonment requires absent writer custody",
                ));
            }
            Some(recovery)
        } else if declare_lost {
            let slot = self.attempt_writer_slot(id, call)?;
            let directory = match slot {
                Some(slot) => {
                    provenant_config::private_directory(&slot, true).map_err(|error| {
                        abandon_refused(AbandonmentRefusal::WriterLease {
                            evidence: error.to_string(),
                        })
                    })?
                }
                None => attempt.try_clone()?,
            };
            unfenced_lease = Some(
                provenant_supervisor::custody::lock_writer_exclusion(
                    &directory,
                    provenant_core::WaitBudget::nonblocking(call.clock.mono_now()),
                    call.cancel,
                    call.clock,
                )
                .map_err(|error| {
                    abandon_refused(AbandonmentRefusal::WriterLease {
                        evidence: error.to_string(),
                    })
                })?,
            );
            None
        } else if journal.abandonment.is_some() || unfenced {
            None
        } else {
            return Err(RunError::Validation("retained writer fence missing"));
        };
        if declare_lost {
            // Re-read presence after acquiring all native exclusion leases.
            if provenant_supervisor::custody::observe_holder(&attempt, scope, observer)
                == provenant_supervisor::custody::HolderPresence::Live
            {
                return Err(abandon_refused(AbandonmentRefusal::Live { scope }));
            }
            let writer = self.request(id, call)?.task.writer;
            let path = writer
                .as_ref()
                .map(|w| w.worktree.clone())
                .unwrap_or(attempt.current_path()?);
            workers.invalidate();
            let (states, details) = workers.observe_roots_with_details(std::slice::from_ref(&path));
            let state = states.get(&path).cloned().unwrap_or_else(|| {
                provenant_confinement::worktrees::Occupancy::census_unknown(
                    path.clone(),
                    "worker census did not return the requested tree",
                )
            });
            if writer.is_none() || !state.is_available() {
                let evidence = if writer.is_none() {
                    "attempt has no recorded confinement worktree".into()
                } else {
                    details.get(&path).cloned().unwrap_or_else(|| match &state {
                        provenant_confinement::worktrees::Occupancy::Unknown {
                            evidence, ..
                        } => evidence.clone(),
                        _ => "worker census did not prove the tree free".into(),
                    })
                };
                return Err(abandon_refused(AbandonmentRefusal::Occupancy {
                    path,
                    state,
                    evidence,
                }));
            }
            let current = self.record(id, call)?;
            if Self::scope(&current) != scope || current.state != record.state {
                return Err(RunError::Conflict);
            }
            // Publish explicit intent before Lost: an interrupted declaration can
            // be retried on the same boot, even after the terminal row commits.
            {
                let _lock = journal_lock(&attempt)?;
                journal = read(&attempt, scope)?;
                if journal.abandonment.is_none() {
                    if journal.phase == Phase::Complete {
                        return Err(RunError::Conflict);
                    }
                    journal.abandonment = Some(Abandonment {
                        reason: reason.clone(),
                        actor: self.context.actor_id,
                        at: call
                            .clock
                            .wall_now()
                            .map_err(|_| RunError::Admission(AdmissionError::Clock))?,
                        phase: journal.phase,
                        declare_lost: true,
                        extra: Default::default(),
                    });
                    write(&attempt, &journal)?;
                }
            }
            self.commit_lost(
                scope,
                &format!("chair abandoned settlement: {reason}"),
                call,
            )?;
            if !matches!(
                self.record(id, call)?.state,
                crate::AttemptState::Terminal {
                    outcome: crate::Outcome::Lost,
                    ..
                }
            ) {
                return Err(RunError::Conflict);
            }
        }
        {
            let _lock = journal_lock(&attempt)?;
            journal = read(&attempt, scope)?;
            if let Some(previous) = &journal.abandonment {
                if previous.reason != reason {
                    return Err(RunError::Conflict);
                }
            } else {
                if journal.phase == Phase::Complete {
                    return Err(RunError::Validation("settlement already complete"));
                }
                journal.abandonment = Some(Abandonment {
                    reason: reason.clone(),
                    actor: self.context.actor_id,
                    at: call
                        .clock
                        .wall_now()
                        .map_err(|_| RunError::Admission(AdmissionError::Clock))?,
                    phase: journal.phase,
                    declare_lost: false,
                    extra: Default::default(),
                });
            }
            journal.phase = Phase::Complete;
            journal.owner = None;
            write(&attempt, &journal)?;
        }
        if let Some(recovery) = recovery {
            recovery
                .release_lost()
                .map_err(|e| RunError::Io(std::io::Error::other(e.to_string())))?;
        }
        drop(unfenced_lease);
        drop(launch_guards);
        abandoned(&attempt, scope)?.ok_or(RunError::Conflict)
    }
}

/// Resume via a detached instance of the same supervisor binary. The CLI call
/// only queues this bounded request; the worker owns import and publication.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeRequest {
    roots: crate::RootSeed,
    project: provenant_core::ProjectId,
    actor: provenant_core::ActorId,
    attempt: provenant_core::AttemptId,
    registry: std::path::PathBuf,
}

impl ResumeRequest {
    pub fn parse(raw: &str) -> Result<Self> {
        serde_json::from_str(raw).map_err(|_| RunError::Validation("settlement resume request"))
    }
    pub fn roots(&self) -> &crate::RootSeed {
        &self.roots
    }
    pub fn project(&self) -> provenant_core::ProjectId {
        self.project
    }
    pub fn actor(&self) -> provenant_core::ActorId {
        self.actor
    }
    pub fn attempt(&self) -> provenant_core::AttemptId {
        self.attempt
    }
}

#[expect(
    clippy::print_stderr,
    reason = "report the run-level registry failure without suppressing judgeable queued rows"
)]
fn report_registry_error(attempt: provenant_core::AttemptId, error: &RunError) {
    eprintln!("warning: writer fence listing failed for {attempt}: {error}");
}

#[derive(Serialize, Deserialize)]
struct WorkerProcessRefusal {
    schema_version: u64,
    attempt: provenant_core::AttemptId,
    expires: provenant_core::WallTime,
    used: u64,
    limit: u64,
    #[serde(default, flatten)]
    extra: provenant_records::ExtraFields,
}

impl crate::RunStore {
    pub(crate) fn recovery_process_wait(
        &self,
        attempt: provenant_core::AttemptId,
        call: &crate::Request<'_>,
    ) -> Option<crate::LaneWaitReason> {
        let directory = self.attempt_dir(attempt, false).ok()?;
        let raw =
            read_sealed(&directory, "settlement-process-refusal.json", RECORD_LIMIT).ok()??;
        let refusal: WorkerProcessRefusal = crate::db::decode(&raw).ok()?;
        if refusal.schema_version != 1
            || refusal.attempt != attempt
            || call.clock.wall_now().ok()? >= refusal.expires
        {
            return None;
        }
        Some(crate::LaneWaitReason::RecoveryWorkerRefused {
            used: refusal.used,
            limit: refusal.limit,
        })
    }
}

impl crate::RunStore {
    pub(crate) fn queue_reconcile(
        &self,
        attempt: provenant_core::AttemptId,
        call: &crate::Request<'_>,
    ) -> Result<Vec<(provenant_core::AttemptId, crate::Reconciled)>> {
        self.queue_reconcile_mode(attempt, call, provenant_os::exec::spawn_detached, true)
    }

    pub(crate) fn queue_reconcile_with(
        &self,
        attempt: provenant_core::AttemptId,
        call: &crate::Request<'_>,
        spawn: impl FnOnce(
            &[std::ffi::OsString],
            &std::path::Path,
        ) -> std::result::Result<(), provenant_os::exec::ExecError>,
    ) -> Result<Vec<(provenant_core::AttemptId, crate::Reconciled)>> {
        self.queue_reconcile_mode(attempt, call, spawn, false)
    }

    fn queue_reconcile_mode(
        &self,
        attempt: provenant_core::AttemptId,
        call: &crate::Request<'_>,
        spawn: impl FnOnce(
            &[std::ffi::OsString],
            &std::path::Path,
        ) -> std::result::Result<(), provenant_os::exec::ExecError>,
        explicit: bool,
    ) -> Result<Vec<(provenant_core::AttemptId, crate::Reconciled)>> {
        self.queue_reconcile_mode_with_probe(attempt, call, spawn, explicit, &|directory, scope| {
            self.supervisor_stalled(directory, scope, call)
        })
    }

    fn queue_reconcile_mode_with_probe(
        &self,
        attempt: provenant_core::AttemptId,
        call: &crate::Request<'_>,
        spawn: impl FnOnce(
            &[std::ffi::OsString],
            &std::path::Path,
        ) -> std::result::Result<(), provenant_os::exec::ExecError>,
        explicit: bool,
        stalled: &dyn Fn(&Directory, Scope) -> bool,
    ) -> Result<Vec<(provenant_core::AttemptId, crate::Reconciled)>> {
        call.check()?;
        // A run is derived from its attempt records, but recovery must inspect
        // those records independently so one corrupt record cannot hide siblings.
        let exists: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM attempts WHERE id=?1 AND project=?2 AND refused=0)",
            rusqlite::params![attempt.to_string(), self.context.project_id.to_string()],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(RunError::NotFound);
        }
        let mut query = self.db.prepare("SELECT id FROM attempts WHERE id=?1 AND project=?2 AND refused=0 AND (terminal=0 OR tmp_retained=1) ORDER BY sequence")?;
        let rows = query.query_map(
            rusqlite::params![attempt.to_string(), self.context.project_id.to_string()],
            |row| row.get::<_, String>(0),
        )?;
        let mut ids = rows
            .map(|id| {
                provenant_core::AttemptId::parse(&id?)
                    .map_err(|_| RunError::Validation("attempt index ID"))
            })
            .collect::<Result<Vec<_>>>()?;
        let listing_error = match self.writer_fences() {
            Ok(fences) => {
                for fence in fences
                    .into_iter()
                    .filter(|fence| fence.scope.attempt == attempt)
                {
                    if !ids.contains(&fence.scope.attempt) {
                        ids.push(fence.scope.attempt);
                    }
                }
                None
            }
            Err(error) => {
                report_registry_error(attempt, &error);
                Some(error)
            }
        };
        for id in self.pending_effect_attempts(attempt, call)? {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        // Reactions need the same retry gates, but are internal work rather
        // than custody recovery rows. Report them only if recovery is uncertain.
        let mut reaction_only = Vec::new();
        if self.reaction_pending(attempt, call)? && !ids.contains(&attempt) {
            ids.push(attempt);
            reaction_only.push(attempt);
        }
        if ids.is_empty()
            && let Some(error) = listing_error
        {
            return Err(error);
        }
        let wall = call
            .clock
            .wall_now()
            .map_err(|_| RunError::Admission(AdmissionError::Clock))?;
        let now = now_ms(call.clock)?;
        let failed = |error: String| crate::Reconciled::Uncertain {
            reason: crate::UncertainReason::ReconcileFailed { error },
        };
        let mut found = Vec::new();
        let mut candidates = Vec::new();
        for id in ids {
            let inspected = crate::controls::retry_reconcile(&self.db, call, || -> Result<_> {
                let record = self.record(id, call)?;
                if record.attempt_id != attempt {
                    return Err(RunError::Validation("writer fence run mismatch"));
                }
                let directory = self.attempt_dir(id, false)?;
                let scope = Self::scope(&record);
                let journal = read(&directory, scope)?;
                if let Some(abandonment) = &journal.abandonment {
                    let state = if journal.phase == Phase::Complete {
                        crate::Reconciled::Abandoned {
                            reason: abandonment.reason.clone(),
                        }
                    } else {
                        crate::Reconciled::Settling {
                            phase: "abandon".into(),
                            blocking_reason: Some("explicit abandonment interrupted; repeat reconcile --abandon with the original reason".into()),
                        }
                    };
                    return Ok((directory, scope, state, false));
                }
                let queued = matches!(record.state, crate::AttemptState::Queued { .. });
                // A supervisor in uninterruptible sleep is never respawned.
                if !queued
                    && !matches!(record.state, crate::AttemptState::Terminal { .. })
                    && stalled(&directory, scope)
                {
                    return Ok((
                        directory,
                        scope,
                        crate::Reconciled::Uncertain {
                            reason: crate::UncertainReason::HostStalled,
                        },
                        false,
                    ));
                }
                let mut eligible = !queued
                    && (explicit
                        || (journal.recovery_failures < LIMIT
                            && !(journal.blocking_reason.is_some()
                                && now < journal.retry_after_ms)))
                    && !owner_active(&directory, &journal, now)?;
                if eligible
                    && let Ok(handle) =
                        provenant_supervisor::custody::reattach(directory.try_clone()?, scope)
                    && matches!(
                        provenant_supervisor::custody::observe(
                            &handle,
                            provenant_core::WaitBudget::nonblocking(call.clock.mono_now()),
                            call.cancel,
                            call.clock,
                        ),
                        Ok(provenant_supervisor::custody::Observation::Live)
                    )
                {
                    eligible = false;
                }
                let state = if queued {
                    crate::Reconciled::Queued
                } else if let Some(error) = journal.blocking_reason {
                    if journal.phase == Phase::Publish {
                        crate::Reconciled::Settling {
                            phase: "publish".into(),
                            blocking_reason: Some(error),
                        }
                    } else {
                        failed(error)
                    }
                } else if let Some(error) = &listing_error {
                    failed(format!("writer fence listing failed: {error}"))
                } else {
                    crate::Reconciled::Settling {
                        phase: refusal(&directory, scope, call.clock)?
                            .map(|reason| reason.to_string())
                            .unwrap_or_else(|| format!("{:?}", journal.phase).to_ascii_lowercase()),
                        blocking_reason: None,
                    }
                };
                Ok((directory, scope, state, eligible))
            });
            match inspected {
                Ok((directory, scope, state, eligible)) => {
                    if eligible {
                        candidates.push((found.len(), directory, scope));
                    }
                    found.push((id, state));
                }
                Err(error) => found.push((id, failed(error.to_string()))),
            }
        }
        let queued = self.attempt_dir(attempt, false)?;
        let spawn_lock =
            match queued.try_lock(OsStr::new("settlement-spawn.lock"), LockMode::Exclusive) {
                Ok(lock) => Some(lock),
                Err(FsError::LockTimeout { .. }) => None,
                Err(error) => return Err(error.into()),
            };
        let worker_lock =
            match queued.try_lock(OsStr::new("settlement-worker.lock"), LockMode::Exclusive) {
                Ok(lock) => Some(lock),
                Err(FsError::LockTimeout { .. }) => None,
                Err(error) => return Err(error.into()),
            };
        let pending = read_sealed(&queued, "settlement-queued.json", RECORD_LIMIT)?
            .map(|raw| QueuedWorker::decode(&raw, attempt))
            .transpose()?
            .is_some_and(|request| wall < request.expires);
        if !candidates.is_empty() && spawn_lock.is_some() && worker_lock.is_some() && !pending {
            let mut needs_worker = false;
            for (index, directory, scope) in candidates {
                let reset = (|| -> Result<()> {
                    if explicit {
                        let _lock = journal_lock(&directory)?;
                        let mut journal = read(&directory, scope)?;
                        // Keep the last reason visible until recovery succeeds.
                        journal.recovery_failures = 0;
                        write(&directory, &journal)?;
                    }
                    Ok(())
                })();
                match reset {
                    Ok(()) => needs_worker = true,
                    Err(error) => found[index].1 = failed(error.to_string()),
                }
            }
            if !needs_worker {
                return Ok(found
                    .into_iter()
                    .filter(|(id, state)| {
                        !reaction_only.contains(id)
                            || matches!(state, crate::Reconciled::Uncertain { .. })
                    })
                    .collect());
            }
            let checkout = std::env::current_dir()?;
            let home = std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .ok_or(RunError::Validation(
                    "HOME unavailable for settlement supervisor",
                ))?;
            let request = ResumeRequest {
                roots: crate::RootSeed {
                    home,
                    checkout: checkout.clone(),
                    state_root: self.context.state_root.as_path().into(),
                },
                project: self.context.project_id,
                actor: self.context.actor_id,
                attempt,
                registry: self.writer_registry.clone(),
            };
            let executable = std::env::current_exe()?;
            let argv = vec![
                executable.into_os_string(),
                "internal".into(),
                "settle".into(),
                serde_json::to_string(&request)
                    .map_err(|_| RunError::Validation("settlement resume request"))?
                    .into(),
            ];
            queued
                .atomic_write(
                    OsStr::new("settlement-queued.json"),
                    ".settlement-",
                    &crate::db::bytes(&QueuedWorker {
                        schema_version: 2,
                        attempt,
                        expires: wall
                            .checked_add(provenant_core::DurationMicros::from_micros(
                                UNKNOWN_OWNER_LIMIT_MS * 1000,
                            ))
                            .map_err(|_| RunError::Admission(AdmissionError::Clock))?,
                        extra: Default::default(),
                    })?,
                    PublishMode::Replace,
                )?
                .require_durable()?;
            // The marker bridges spawning and the worker taking its kernel lock.
            // A crashed launch expires after ten minutes.
            drop(worker_lock);
            if let Err(error) = spawn(&argv, &checkout) {
                queued.unlink(OsStr::new("settlement-queued.json"))?;
                return Err(match error {
                    provenant_os::exec::ExecError::ProcessBudget { used, limit } => {
                        // The caller may discard this best-effort recovery error. Retain
                        // a short-lived observation for queued lanes on the same run.
                        if let Ok(expires) = wall
                            .checked_add(provenant_core::DurationMicros::from_micros(60_000_000))
                            && let Ok(bytes) = crate::db::bytes(&WorkerProcessRefusal {
                                schema_version: 1,
                                attempt,
                                expires,
                                used,
                                limit,
                                extra: Default::default(),
                            })
                        {
                            let _ = queued
                                .atomic_write(
                                    OsStr::new("settlement-process-refusal.json"),
                                    ".settlement-refusal-",
                                    &bytes,
                                    PublishMode::Replace,
                                )
                                .and_then(|publication| publication.require_durable());
                        }
                        RunError::Admission(AdmissionError::Processes { used, limit })
                    }
                    other => RunError::Io(std::io::Error::other(other)),
                });
            }
            // The worker is already running; a stale diagnostic must not turn
            // a successful spawn into a reported launch failure.
            let _ = queued.unlink(OsStr::new("settlement-process-refusal.json"));
        }
        Ok(found
            .into_iter()
            .filter(|(id, state)| {
                (!reaction_only.contains(id)
                    || matches!(state, crate::Reconciled::Uncertain { .. }))
                    && (!matches!(
                        state,
                        crate::Reconciled::Uncertain { .. } | crate::Reconciled::Settling { .. }
                    ) || !self
                        .reconcile_is_settled(attempt, *id, call)
                        .unwrap_or(false))
            })
            .collect())
    }
}

#[derive(Serialize, Deserialize)]
struct QueuedWorker {
    schema_version: u64,
    attempt: provenant_core::AttemptId,
    expires: provenant_core::WallTime,
    #[serde(default, flatten)]
    extra: provenant_records::ExtraFields,
}

impl QueuedWorker {
    fn decode(raw: &[u8], attempt: provenant_core::AttemptId) -> Result<Self> {
        let marker: Self = provenant_records::decode(
            raw,
            provenant_records::Limits {
                max_bytes: RECORD_LIMIT,
                ..Default::default()
            },
        )?;
        if marker.schema_version != 2 || marker.attempt != attempt {
            return Err(RunError::Validation(
                "settlement queue marker run or version mismatch",
            ));
        }
        Ok(marker)
    }
}

/// The internal same-binary supervisor entry. It acquires journal ownership
/// before each attempt; competing resumes return without waiting for Git.
pub fn resume(raw: &str, providers: &dyn crate::Providers) -> Result<()> {
    use provenant_core::RootInputs;
    let request: ResumeRequest =
        serde_json::from_str(raw).map_err(|_| RunError::Validation("settlement resume request"))?;
    let mut roots = RootInputs::new(request.roots.home, request.roots.checkout);
    roots.state_dir = Some(request.roots.state_root);
    let roots = roots
        .resolve(&provenant_os::fs::SystemPathResolver)
        .map_err(|_| RunError::Validation("settlement state root"))?;
    let context = crate::ExecutionContext {
        state_root: roots.state,
        project_id: request.project,
        actor_id: request.actor,
        read_prompts: false,
    };
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = CancelToken::new();
    let call = crate::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let store = crate::RunStore::open(&context, &call)?.with_writer_registry(&request.registry);
    let directory = store.attempt_dir(request.attempt, false)?;
    // A queuing reader can briefly hold this lock while probing for a worker.
    // Wait through that probe instead of abandoning the queued request.
    let _worker = match directory.lock(
        OsStr::new("settlement-worker.lock"),
        LockMode::Exclusive,
        call.lock_budget(),
        call.cancel,
        call.clock,
    ) {
        Ok(lock) => lock,
        Err(FsError::LockTimeout { .. }) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    match directory.unlink(OsStr::new("settlement-queued.json")) {
        Ok(()) => directory.sync()?,
        Err(error) if error.is_not_found() => {}
        Err(error) => return Err(error.into()),
    }
    for delay_ms in std::iter::once(0).chain((1..LIMIT).map(backoff_ms)) {
        if delay_ms != 0 {
            let (_wake, sleep) = mpsc::channel::<()>();
            let _ = sleep.recv_timeout(Duration::from_millis(delay_ms));
        }
        let found = store.reconcile_with_observer(
            request.attempt,
            providers,
            &call,
            &SystemProcessObserver,
        )?;
        if !found
            .iter()
            .any(|(_, state)| matches!(state, crate::Reconciled::Settling {..}) || matches!(state, crate::Reconciled::Uncertain { reason } if !matches!(reason, crate::UncertainReason::HostStalled)))
        {
            break;
        }
    }
    Ok(())
}
