//! Durable settlement progress. Kernel locks protect only journal updates.
use crate::{RECORD_LIMIT, Result, RunError, artifacts::read_sealed};
use provenant_core::{CancelToken, GitOid};
use provenant_os::fs::{Directory, FsError, LockMode, PublishMode};
use provenant_supervisor::{
    census::{ProcessIdentity, ProcessObservation, SystemProcessObserver},
    custody::Scope,
};
use serde::{Deserialize, Serialize};
use std::{ffi::OsStr, sync::mpsc, time::Duration};

const FILE: &str = "settlement-progress.json";
const LIMIT: u32 = 3;

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
#[serde(deny_unknown_fields)]
struct Journal {
    schema_version: u64,
    scope: Scope,
    phase: Phase,
    attempts: u32,
    head: Option<GitOid>,
    base: Option<GitOid>,
    owner: Option<ProcessIdentity>,
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

fn alive(owner: &ProcessIdentity) -> bool {
    match SystemProcessObserver.observe(owner.pid) {
        ProcessObservation::Present(fact) => fact.identity == *owner && !fact.zombie,
        ProcessObservation::Absent => false,
        ProcessObservation::Unknown(_) => true,
    }
}

pub(crate) struct Guard;

impl Guard {
    /// A live native owner, including this process, excludes another driver.
    /// A crash leaves its progress and attempt count ready for the next claim.
    pub(crate) fn claim(attempt: &Directory, scope: Scope) -> Result<Option<Driver>> {
        let _lock = match attempt.try_lock(OsStr::new("settlement.lock"), LockMode::Exclusive) {
            Ok(lock) => lock,
            Err(FsError::LockTimeout { .. }) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut journal = read(attempt, scope)?;
        if journal.phase == Phase::Complete || journal.owner.as_ref().is_some_and(alive) {
            return Ok(None);
        }
        if journal.attempts == LIMIT {
            return Err(RunError::ObjectImportLimit(
                "settlement exhausted three attempts; private Git retained for host recovery",
            ));
        }
        let owner = match SystemProcessObserver.observe(i64::from(std::process::id())) {
            ProcessObservation::Present(fact) if !fact.zombie => fact.identity,
            _ => return Err(RunError::GuardianIdentityUnavailable),
        };
        journal.owner = Some(owner.clone());
        journal.attempts += 1;
        write(attempt, &journal)?;
        Ok(Some(Driver {
            attempt: attempt.try_clone()?,
            journal,
            owner,
            cancel: CancelToken::new(),
        }))
    }
}

/// A native process owns progress; this value holds no kernel lock.
pub(crate) struct Driver {
    attempt: Directory,
    journal: Journal,
    owner: ProcessIdentity,
    cancel: CancelToken,
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
        let _lock = self
            .attempt
            .try_lock(OsStr::new("settlement.lock"), LockMode::Exclusive)?;
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
        let Ok(_lock) = self
            .attempt
            .try_lock(OsStr::new("settlement.lock"), LockMode::Exclusive)
        else {
            return;
        };
        let Ok(mut journal) = read(&self.attempt, self.journal.scope) else {
            return;
        };
        if journal.owner.as_ref() == Some(&self.owner) {
            journal.owner = None;
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
        let mut resumed = Guard::claim(&attempt, scope())?.expect("resumed claim");
        assert_eq!(resumed.phase(), Phase::Record);
        assert_eq!(resumed.head(), Some(head));
        assert_eq!(resumed.base(), Some(base));
        assert_eq!(resumed.attempts(), 2);
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
        let resumed = Guard::claim(&attempt, scope())?.expect("stale native identity recovered");
        assert_eq!(resumed.attempts(), 2);
        Ok(())
    }

    #[test]
    fn interrupted_settlement_has_one_durable_retry_cap() -> TestResult {
        let temp = tempfile::tempdir()?;
        let attempt = Directory::open_anchor(temp.path())?;
        for expected in 1..=LIMIT {
            let driver = Guard::claim(&attempt, scope())?.expect("claim within cap");
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
}

pub(crate) fn complete(attempt: &Directory, scope: Scope) -> Result<()> {
    let _lock = attempt.try_lock(OsStr::new("settlement.lock"), LockMode::Exclusive)?;
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
        spawn(&argv, &checkout).map_err(|error| RunError::Io(std::io::Error::other(error)))?;
        Ok(attempts
            .into_iter()
            .map(|attempt| {
                (
                    attempt.attempt_id,
                    match attempt.state {
                        crate::AttemptState::Queued => crate::Reconciled::Queued,
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

/// The internal same-binary supervisor entry. It acquires journal ownership
/// before each attempt; competing resumes return without waiting for Git.
pub fn resume(raw: &str, providers: &dyn crate::Providers) -> Result<()> {
    use provenant_core::{Clock, RootInputs, WaitBudget};
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
        budget: WaitBudget::cli(clock.mono_now(), 30)
            .map_err(|_| RunError::Validation("settlement budget"))?,
        cancel: &cancel,
    };
    let store = crate::RunStore::open(&context, &call)?.with_writer_registry(&request.registry);
    for _ in 0..LIMIT {
        let phase_call = crate::Request {
            clock: &clock,
            cancel: &cancel,
            budget: WaitBudget::cli(clock.mono_now(), 30)
                .map_err(|_| RunError::Validation("settlement phase budget"))?,
        };
        let found = store.reconcile_with_observer(
            request.run,
            providers,
            &phase_call,
            &SystemProcessObserver,
        )?;
        if !found
            .iter()
            .any(|(_, state)| matches!(state, crate::Reconciled::Uncertain { .. }))
        {
            break;
        }
    }
    Ok(())
}
