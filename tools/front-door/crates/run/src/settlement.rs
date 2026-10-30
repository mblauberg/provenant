//! Durable settlement progress. Kernel locks protect only journal updates.
use crate::{AdmissionError, RECORD_LIMIT, Result, RunError, artifacts::read_sealed};
use provenant_core::retry::Backoff;
use provenant_core::{CancelToken, Clock, GitOid};
use provenant_os::fs::{Directory, FsError, LockMode, PublishMode};
use provenant_supervisor::{
    census::{ProcessIdentity, ProcessObservation, SystemProcessObserver},
    custody::Scope,
};
use serde::{Deserialize, Serialize};
use std::{ffi::OsStr, sync::mpsc, time::Duration};

const FILE: &str = "settlement-progress.json";
const LIMIT: u32 = 3;
const UNKNOWN_OWNER_LIMIT_MS: u64 = 10 * 60 * 1000;
const CLAIM_BACKOFF: Backoff = Backoff::new(Duration::from_secs(5), 6, Duration::from_secs(120));

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    Import,
    Publish,
    Record,
    Release,
    Complete,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Journal {
    schema_version: u64,
    scope: Scope,
    phase: Phase,
    attempts: u32,
    head: Option<GitOid>,
    base: Option<GitOid>,
    owner: Option<ProcessIdentity>,
    #[serde(default)]
    owner_since_ms: u64,
    #[serde(default)]
    retry_after_ms: u64,
    #[serde(default)]
    recovery_failures: u32,
    #[serde(default)]
    blocking_reason: Option<String>,
}

fn read(attempt: &Directory, scope: Scope) -> Result<Journal> {
    let journal = match read_sealed(attempt, FILE, RECORD_LIMIT)? {
        Some(raw) => crate::db::decode::<Journal>(&raw)?,
        None => Journal {
            schema_version: 1,
            scope,
            phase: Phase::Import,
            attempts: 0,
            head: None,
            base: None,
            owner: None,
            owner_since_ms: 0,
            retry_after_ms: 0,
            recovery_failures: 0,
            blocking_reason: None,
        },
    };
    if journal.schema_version != 1 || journal.scope != scope || journal.attempts > LIMIT {
        return Err(RunError::Validation(
            "settlement journal scope or version mismatch",
        ));
    }
    Ok(journal)
}

fn write(attempt: &Directory, journal: &Journal) -> Result<()> {
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
    if journal
        .owner
        .as_ref()
        .is_some_and(|owner| alive(owner, journal.owner_since_ms, now))
    {
        return Ok(());
    }
    journal.recovery_failures = journal.recovery_failures.saturating_add(1).min(LIMIT);
    journal.blocking_reason = Some(provenant_confinement::redact::bounded_line(reason, 4096));
    journal.retry_after_ms = now.saturating_add(backoff_ms(journal.recovery_failures));
    write(attempt, &journal)
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

pub(crate) struct Guard;

impl Guard {
    /// A live native owner, including this process, excludes another driver.
    /// A crash leaves its progress and attempt count ready for the next claim.
    pub(crate) fn claim<'a>(
        attempt: &Directory,
        scope: Scope,
        clock: &'a dyn Clock,
    ) -> Result<Option<Driver<'a>>> {
        let now_ms = now_ms(clock)?;
        let _lock = match attempt.try_lock(OsStr::new("settlement.lock"), LockMode::Exclusive) {
            Ok(lock) => lock,
            Err(FsError::LockTimeout { .. }) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut journal = read(attempt, scope)?;
        if journal.phase == Phase::Complete
            || journal
                .owner
                .as_ref()
                .is_some_and(|owner| alive(owner, journal.owner_since_ms, now_ms))
        {
            return Ok(None);
        }
        if journal.attempts == LIMIT && journal.phase == Phase::Import {
            return Err(RunError::ObjectImportLimit(
                "settlement exhausted three attempts; private Git retained for host recovery",
            ));
        }
        if now_ms < journal.retry_after_ms {
            return Ok(None);
        }
        let owner = match SystemProcessObserver.observe(i64::from(std::process::id())) {
            ProcessObservation::Present(fact) if !fact.zombie => fact.identity,
            _ => return Err(RunError::Admission(AdmissionError::GuardianIdentity)),
        };
        journal.owner = Some(owner.clone());
        journal.owner_since_ms = now_ms;
        journal.retry_after_ms = now_ms.saturating_add(backoff_ms(journal.attempts));
        write(attempt, &journal)?;
        Ok(Some(Driver {
            attempt: attempt.try_clone()?,
            journal,
            owner,
            cancel: CancelToken::new(),
            import_started: false,
            clock,
        }))
    }
}

/// A native process owns progress; this value holds no kernel lock.
pub(crate) struct Driver<'a> {
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
    #[derive(Deserialize)]
    struct Cancellation {
        scope: Scope,
    }
    let directory = attempt.try_clone()?;
    let token = cancel.clone();
    let (done, stopped) = mpsc::channel();
    std::thread::scope(|threads| {
        threads.spawn(move || {
            loop {
                if let Ok(Some(raw)) = read_sealed(&directory, "cancel.json", RECORD_LIMIT)
                    && let Ok(request) = serde_json::from_slice::<Cancellation>(&raw)
                    && request.scope == scope
                {
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
    use provenant_core::{AttemptId, RunId};
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

    #[test]
    fn detached_process_budget_refusal_is_typed_and_clears_marker() -> TestResult {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let store = crate::RunStore::open(&crate::tests::context(temp.path())?, &call)?;
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let id = run.attempts[0].attempt_id;
        let queued_lane = store.record(id, &call)?;
        store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let result = store.queue_reconcile_with(run.run_id, &call, |_, _| {
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
            "launch: last recovery worker refused: user process count 950 of 1000"
        );
        let mut retried = false;
        store.queue_reconcile_with(run.run_id, &call, |_, _| {
            retried = true;
            Ok(())
        })?;
        assert!(retried, "failed spawn must remove its queued marker");
        assert!(store.recovery_process_wait(run.run_id, &call).is_none());
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
        let id = run.attempts[0].attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let attempt = store.attempt_dir(run.run_id, id, false)?;
        crate::artifacts::seal(&attempt, "launch.json", b"{}", RECORD_LIMIT)?;
        let scope = RunStore::scope(&store.record(id, &call)?);
        for index in 1..=LIMIT {
            let call = crate::tests::call(&clock, &cancel);
            let found = store.reconcile_with_observer(
                run.run_id,
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
            let queued = store.queue_reconcile_with(run.run_id, &call, |_, _| {
                panic!("cooldown must prevent another worker");
            })?;
            assert!(
                matches!(queued.as_slice(), [(_, Reconciled::Uncertain { reason })] if reason.to_string().contains("invalid"))
            );
            advance_to(&mut clock, journal.retry_after_ms)?;
        }
        let call = crate::tests::call(&clock, &cancel);
        for _ in 0..4 {
            store.queue_reconcile_with(run.run_id, &call, |_, _| {
                panic!("exhausted recovery must stay bounded after cooldown");
            })?;
        }
        let mut spawned = false;
        store.queue_reconcile_mode(
            run.run_id,
            &call,
            |_, _| {
                spawned = true;
                Ok(())
            },
            true,
        )?;
        assert!(
            spawned,
            "an explicit reconcile permits another bounded recovery"
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
                "schema_version":1, "scope":scope, "guardian":current.identity
            }))?,
            RECORD_LIMIT,
        )?;
        let found = store.reconcile_with_observer(
            run.run_id,
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
            run.run_id,
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
            run: RunId::from_bytes([1; 16]),
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
        let resumed =
            Guard::claim(&attempt, scope(), &clock)?.expect("stale native identity recovered");
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
    fn import_accounting_and_owner_release_survive_short_journal_contention() -> TestResult {
        let clock = clock();
        let temp = tempfile::tempdir()?;
        let attempt = Directory::open_anchor(temp.path())?;
        let mut driver = Guard::claim(&attempt, scope(), &clock)?.expect("claim");
        let held = attempt.try_lock(OsStr::new("settlement.lock"), LockMode::Exclusive)?;
        std::thread::scope(|threads| {
            threads.spawn(move || {
                let (_wake, waiting) = mpsc::channel::<()>();
                let _ = waiting.recv_timeout(Duration::from_millis(20));
                drop(held);
            });
            driver.begin_import()
        })?;
        assert_eq!(driver.attempts(), 1);
        let held = attempt.try_lock(OsStr::new("settlement.lock"), LockMode::Exclusive)?;
        std::thread::scope(|threads| {
            threads.spawn(move || {
                let (_wake, waiting) = mpsc::channel::<()>();
                let _ = waiting.recv_timeout(Duration::from_millis(20));
                drop(held);
            });
            drop(driver);
        });
        assert!(
            read(&attempt, scope())?.owner.is_none(),
            "drop releases durable ownership"
        );
        Ok(())
    }
}

pub(crate) fn pinned_head(attempt: &Directory, scope: Scope) -> Result<Option<GitOid>> {
    read(attempt, scope).map(|journal| journal.head)
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

/// Resume via a detached instance of the same supervisor binary. The CLI call
/// only queues this bounded request; the worker owns import and publication.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeRequest {
    roots: crate::RootSeed,
    project: provenant_core::ProjectId,
    actor: provenant_core::ActorId,
    run: provenant_core::RunId,
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
    pub fn run(&self) -> provenant_core::RunId {
        self.run
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerProcessRefusal {
    schema_version: u64,
    run: provenant_core::RunId,
    expires: provenant_core::WallTime,
    used: u64,
    limit: u64,
}

impl crate::RunStore {
    pub(crate) fn recovery_process_wait(
        &self,
        run: provenant_core::RunId,
        call: &crate::Request<'_>,
    ) -> Option<String> {
        let directory = self.run_dir(run, false).ok()?;
        let raw =
            read_sealed(&directory, "settlement-process-refusal.json", RECORD_LIMIT).ok()??;
        let refusal: WorkerProcessRefusal = crate::db::decode(&raw).ok()?;
        if refusal.schema_version != 1
            || refusal.run != run
            || call.clock.wall_now().ok()? >= refusal.expires
        {
            return None;
        }
        Some(format!(
            "launch: last recovery worker refused: user process count {} of {}",
            refusal.used, refusal.limit
        ))
    }
}

impl crate::RunStore {
    pub(crate) fn queue_reconcile(
        &self,
        run: provenant_core::RunId,
        call: &crate::Request<'_>,
    ) -> Result<Vec<(provenant_core::AttemptId, crate::Reconciled)>> {
        self.queue_reconcile_mode(run, call, provenant_os::exec::spawn_detached, true)
    }

    pub(crate) fn queue_reconcile_with(
        &self,
        run: provenant_core::RunId,
        call: &crate::Request<'_>,
        spawn: impl FnOnce(
            &[std::ffi::OsString],
            &std::path::Path,
        ) -> std::result::Result<(), provenant_os::exec::ExecError>,
    ) -> Result<Vec<(provenant_core::AttemptId, crate::Reconciled)>> {
        self.queue_reconcile_mode(run, call, spawn, false)
    }

    fn queue_reconcile_mode(
        &self,
        run: provenant_core::RunId,
        call: &crate::Request<'_>,
        spawn: impl FnOnce(
            &[std::ffi::OsString],
            &std::path::Path,
        ) -> std::result::Result<(), provenant_os::exec::ExecError>,
        explicit: bool,
    ) -> Result<Vec<(provenant_core::AttemptId, crate::Reconciled)>> {
        self.run(run, call)?;
        let attempts = self.pending_attempts(run, call)?;
        let wall = call
            .clock
            .wall_now()
            .map_err(|_| RunError::Admission(AdmissionError::Clock))?;
        let now = now_ms(call.clock)?;
        // Owed reactions need a worker even when no attempt is live.
        let mut needs_worker = !self.pending_reactions(run, call)?.is_empty();
        for attempt in &attempts {
            if matches!(
                attempt.state,
                crate::AttemptState::Queued { .. } | crate::AttemptState::Terminal { .. }
            ) {
                continue;
            }
            let directory = self.attempt_dir(run, attempt.attempt_id, false)?;
            let scope = Self::scope(attempt);
            let journal = read(&directory, scope)?;
            if !explicit
                && (journal.recovery_failures >= LIMIT
                    || (journal.blocking_reason.is_some() && now < journal.retry_after_ms))
            {
                continue;
            }
            if journal
                .owner
                .as_ref()
                .is_some_and(|owner| alive(owner, journal.owner_since_ms, now))
            {
                continue;
            }
            if let Ok(handle) = provenant_supervisor::custody::reattach(directory, scope)
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
                continue;
            }
            needs_worker = true;
            break;
        }
        let queued = self.run_dir(run, false)?;
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
            .map(|raw| QueuedWorker::decode(&raw, run))
            .transpose()?
            .is_some_and(|request| wall < request.expires);
        if needs_worker && spawn_lock.is_some() && worker_lock.is_some() && !pending {
            if explicit {
                for attempt in &attempts {
                    let directory = self.attempt_dir(run, attempt.attempt_id, false)?;
                    let _lock = journal_lock(&directory)?;
                    let mut journal = read(&directory, Self::scope(attempt))?;
                    // Keep the last reason visible until recovery succeeds.
                    journal.recovery_failures = 0;
                    write(&directory, &journal)?;
                }
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
                run,
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
                        run,
                        expires: wall
                            .checked_add(provenant_core::DurationMicros::from_micros(
                                UNKNOWN_OWNER_LIMIT_MS * 1000,
                            ))
                            .map_err(|_| RunError::Admission(AdmissionError::Clock))?,
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
                                run,
                                expires,
                                used,
                                limit,
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
        Ok(attempts
            .into_iter()
            .map(|attempt| {
                (
                    attempt.attempt_id,
                    match attempt.state {
                        crate::AttemptState::Queued { .. } => crate::Reconciled::Queued,
                        crate::AttemptState::Terminal { .. } => crate::Reconciled::Finished,
                        _ => {
                            let journal =
                                self.attempt_dir(run, attempt.attempt_id, false).and_then(
                                    |directory| read(&directory, crate::RunStore::scope(&attempt)),
                                );
                            match journal {
                                Ok(Journal {
                                    blocking_reason: Some(error),
                                    ..
                                }) => crate::Reconciled::Uncertain {
                                    reason: crate::UncertainReason::ReconcileFailed { error },
                                },
                                Ok(journal) => crate::Reconciled::Settling {
                                    phase: format!("{:?}", journal.phase).to_ascii_lowercase(),
                                },
                                Err(error) => crate::Reconciled::Uncertain {
                                    reason: crate::UncertainReason::ReconcileFailed {
                                        error: error.to_string(),
                                    },
                                },
                            }
                        }
                    },
                )
            })
            .collect())
    }
}

#[derive(Serialize, Deserialize)]
struct QueuedWorker {
    schema_version: u64,
    run: provenant_core::RunId,
    expires: provenant_core::WallTime,
}

impl QueuedWorker {
    fn decode(raw: &[u8], run: provenant_core::RunId) -> Result<Self> {
        let marker: Self = provenant_records::decode(
            raw,
            provenant_records::Limits {
                max_bytes: RECORD_LIMIT,
                ..Default::default()
            },
        )?;
        if marker.schema_version != 2 || marker.run != run {
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
    let directory = store.run_dir(request.run, false)?;
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
        let found =
            store.reconcile_with_observer(request.run, providers, &call, &SystemProcessObserver)?;
        if !found
            .iter()
            .any(|(_, state)| matches!(state, crate::Reconciled::Uncertain { .. }))
        {
            break;
        }
    }
    Ok(())
}
