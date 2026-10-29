//! Durable settlement progress. Kernel locks protect only journal updates.
use crate::{RECORD_LIMIT, Result, RunError, artifacts::read_sealed};
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
const CLAIM_BACKOFF_MS: [u64; 3] = [5_000, 30_000, 120_000];

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

fn now_ms() -> Result<u64> {
    provenant_os::clock::SystemClock::new()
        .wall_now()
        .ok()
        .and_then(|time| u64::try_from(time.as_micros() / 1000).ok())
        .ok_or(RunError::HostClock)
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
    CLAIM_BACKOFF_MS[attempts.saturating_sub(1).min(2) as usize]
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
    pub(crate) fn claim(attempt: &Directory, scope: Scope) -> Result<Option<Driver>> {
        Self::claim_at(attempt, scope, now_ms()?)
    }

    pub(crate) fn claim_at(
        attempt: &Directory,
        scope: Scope,
        now_ms: u64,
    ) -> Result<Option<Driver>> {
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
            _ => return Err(RunError::GuardianIdentityUnavailable),
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
        }))
    }
}

/// A native process owns progress; this value holds no kernel lock.
pub(crate) struct Driver {
    attempt: Directory,
    journal: Journal,
    owner: ProcessIdentity,
    cancel: CancelToken,
    import_started: bool,
}

impl Driver {
    pub(crate) fn cancellation(&self) -> CancelToken {
        self.cancel.clone()
    }
    pub(crate) fn phase(&self) -> Phase {
        self.journal.phase
    }
    pub(crate) fn attempts(&self) -> u32 {
        self.journal.attempts
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

impl Drop for Driver {
    fn drop(&mut self) {
        let Ok(_lock) = journal_lock(&self.attempt) else {
            return;
        };
        let Ok(mut journal) = read(&self.attempt, self.journal.scope) else {
            return;
        };
        if journal.owner.as_ref() == Some(&self.owner) {
            journal.owner = None;
            if let Ok(now) = now_ms() {
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

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

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
        let temp = tempfile::tempdir()?;
        let attempt = Directory::open_anchor(temp.path())?;
        let head = GitOid::parse(&"a".repeat(40))?;
        let base = GitOid::parse(&"b".repeat(40))?;
        let mut first = Guard::claim(&attempt, scope())?.expect("initial claim");
        first.set_head(head)?;
        first.set_base(base)?;
        first.advance(Phase::Record)?;
        drop(first);
        let mut resumed =
            Guard::claim_at(&attempt, scope(), read(&attempt, scope())?.retry_after_ms)?
                .expect("resumed claim");
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
        let temp = tempfile::tempdir()?;
        let attempt = Directory::open_anchor(temp.path())?;
        let first = Guard::claim(&attempt, scope())?.expect("initial claim");
        assert!(Guard::claim(&attempt, scope())?.is_none());
        let mut crashed = read(&attempt, scope())?;
        let owner = crashed.owner.as_mut().expect("owner");
        owner.native_start = owner.native_start.wrapping_add(1);
        write(&attempt, &crashed)?;
        drop(first);
        let resumed = Guard::claim_at(&attempt, scope(), crashed.retry_after_ms)?
            .expect("stale native identity recovered");
        assert_eq!(resumed.attempts(), 0);
        Ok(())
    }

    #[test]
    fn claims_before_import_do_not_consume_the_import_retry_budget() -> TestResult {
        let temp = tempfile::tempdir()?;
        let attempt = Directory::open_anchor(temp.path())?;
        let driver = Guard::claim(&attempt, scope())?.expect("initial claim");
        assert_eq!(driver.attempts(), 0, "claiming has not reached import");
        Ok(())
    }

    #[test]
    fn interrupted_settlement_has_one_durable_retry_cap() -> TestResult {
        let temp = tempfile::tempdir()?;
        let attempt = Directory::open_anchor(temp.path())?;
        for expected in 1..=LIMIT {
            let now = read(&attempt, scope())?.retry_after_ms.max(now_ms()?);
            let mut driver = Guard::claim_at(&attempt, scope(), now)?.expect("claim within cap");
            driver.begin_import()?;
            driver.begin_import()?;
            assert_eq!(driver.attempts(), expected);
            drop(driver);
        }
        assert!(matches!(
            Guard::claim(&attempt, scope()),
            Err(RunError::ObjectImportLimit(
                "settlement exhausted three attempts; private Git retained for host recovery"
            ))
        ));
        Ok(())
    }

    #[test]
    fn retry_claims_wait_without_spending_import_attempts() -> TestResult {
        let temp = tempfile::tempdir()?;
        let attempt = Directory::open_anchor(temp.path())?;
        for expected in 1..=LIMIT {
            let now = read(&attempt, scope())?.retry_after_ms.max(now_ms()?);
            let mut driver = Guard::claim_at(&attempt, scope(), now)?.expect("eligible claim");
            driver.begin_import()?;
            let dropped_at = now_ms()?;
            drop(driver);
            let journal = read(&attempt, scope())?;
            if expected < LIMIT {
                assert!(Guard::claim_at(&attempt, scope(), journal.retry_after_ms - 1)?.is_none());
                assert_eq!(read(&attempt, scope())?.attempts, expected);
            }
            assert!(journal.retry_after_ms >= dropped_at + backoff_ms(expected));
        }
        Ok(())
    }

    #[test]
    fn unknown_owner_expires_after_ten_minutes() -> TestResult {
        use provenant_supervisor::census::ObservationIssue;
        let temp = tempfile::tempdir()?;
        let attempt = Directory::open_anchor(temp.path())?;
        let driver = Guard::claim(&attempt, scope())?.expect("claim");
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
        let temp = tempfile::tempdir()?;
        let attempt = Directory::open_anchor(temp.path())?;
        let mut driver = Guard::claim(&attempt, scope())?.expect("claim");
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

impl crate::RunStore {
    pub(crate) fn queue_reconcile(
        &self,
        run: provenant_core::RunId,
        call: &crate::Request<'_>,
    ) -> Result<Vec<(provenant_core::AttemptId, crate::Reconciled)>> {
        self.queue_reconcile_with(run, call, provenant_os::exec::spawn_detached)
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
        self.run(run, call)?;
        let attempts = self.pending_attempts(run, call)?;
        let now = now_ms()?;
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
            .map(|raw| crate::db::decode::<QueuedWorker>(&raw))
            .transpose()?
            .is_some_and(|request| request.run == run && now < request.expires_ms);
        if needs_worker && spawn_lock.is_some() && worker_lock.is_some() && !pending {
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
                        schema_version: 1,
                        run,
                        expires_ms: now.saturating_add(UNKNOWN_OWNER_LIMIT_MS),
                    })?,
                    PublishMode::Replace,
                )?
                .require_durable()?;
            // The marker bridges spawning and the worker taking its kernel lock.
            // A crashed launch expires after ten minutes.
            drop(worker_lock);
            if let Err(error) = spawn(&argv, &checkout) {
                queued.unlink(OsStr::new("settlement-queued.json"))?;
                return Err(RunError::Io(std::io::Error::other(error)));
            }
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
                            let phase = self
                                .attempt_dir(run, attempt.attempt_id, false)
                                .and_then(|directory| {
                                    read(&directory, crate::RunStore::scope(&attempt))
                                })
                                .map(|journal| format!("{:?}", journal.phase).to_ascii_lowercase())
                                .unwrap_or_else(|_| "provider_exit".into());
                            crate::Reconciled::Settling { phase }
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
    expires_ms: u64,
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
    for delay_ms in std::iter::once(0).chain(CLAIM_BACKOFF_MS) {
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
