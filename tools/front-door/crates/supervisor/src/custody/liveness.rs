//! Kernel-backed guardian death evidence, separate from provider cleanup.
use super::*;

const LOCK: &str = "guardian.lock";
const RECORD: &str = "guardian-lock.json";

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct Identity {
    device: u64,
    inode: u64,
}
impl Identity {
    fn of(metadata: Metadata) -> Self {
        Self {
            device: metadata.device,
            inode: metadata.inode,
        }
    }
    fn matches(self, metadata: Metadata) -> bool {
        self.device == metadata.device && self.inode == metadata.inode
    }
    fn matches_lock(self, metadata: Metadata) -> bool {
        self.matches(metadata) && metadata.links == 1
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct GuardianLock {
    schema_version: u64,
    scope: Scope,
    guardian_process: Option<ProcessIdentity>,
    attempt: Identity,
    guardian: Identity,
    writer: Identity,
    custody: Identity,
}
impl GuardianLock {
    fn names_match(&self, recovery: &Recovery, attempt: &Directory) -> Result<bool> {
        Ok(self.attempt.matches(attempt.metadata()?)
            && self
                .guardian
                .matches_lock(recovery.directory.metadata_at(OsStr::new(LOCK), false)?)
            && self
                .custody
                .matches_lock(attempt.metadata_at(OsStr::new("custody.lock"), false)?)
            && self.writer.matches_lock(
                recovery
                    .lock_directory
                    .metadata_at(OsStr::new("writer.lock"), false)?,
            ))
    }
}

/// Prepare the original lock identities before HandingOff or guardian spawn.
/// Until this is durable the launch remains Prepared and has no native child.
pub(crate) fn prepare_guardian_lifetime(
    evidence: &Directory,
    attempt: &Directory,
    scope: Scope,
    writer: &LockGuard,
    custody: &LockGuard,
) -> Result<()> {
    let guardian = evidence.try_lock(OsStr::new(LOCK), LockMode::Exclusive)?;
    let record = GuardianLock {
        schema_version: 1,
        scope,
        guardian_process: None,
        attempt: Identity::of(attempt.metadata()?),
        guardian: Identity::of(guardian.metadata()?),
        writer: Identity::of(writer.metadata()?),
        custody: Identity::of(custody.metadata()?),
    };
    write_record(evidence, RECORD, &record, PublishMode::NoReplace)?.require_durable()?;
    Ok(())
}

/// Acquire the prepared lock before acknowledging custody or spawning a provider.
/// Directory's owner locks are CLOEXEC; no provider inherits this description.
pub(crate) fn hold_guardian_lifetime(
    evidence: &Directory,
    attempt: &Directory,
    scope: Scope,
    guardian_process: &ProcessIdentity,
    writer: &LockGuard,
    custody: &LockGuard,
) -> Result<LockGuard> {
    guardian_process
        .validate()
        .map_err(|_| CustodyError::InvalidRecord)?;
    if guardian_process.native_start == 0
        || guardian_process.group_id != guardian_process.pid
        || guardian_process.session_id != guardian_process.pid
    {
        return Err(CustodyError::InvalidRecord);
    }
    let guardian = evidence.try_lock_existing(OsStr::new(LOCK), LockMode::Exclusive)?;
    let mut record: GuardianLock = read_record(evidence, RECORD, RECORD_LIMIT)?;
    if record.schema_version != 1
        || record.scope != scope
        || record.guardian_process.is_some()
        || !record.attempt.matches(attempt.metadata()?)
        || !record.guardian.matches_lock(guardian.metadata()?)
        || !record.writer.matches_lock(writer.metadata()?)
        || !record.custody.matches_lock(custody.metadata()?)
    {
        return Err(CustodyError::Fenced);
    }
    record.guardian_process = Some(guardian_process.clone());
    write_record(evidence, RECORD, &record, PublishMode::Replace)?.require_durable()?;
    Ok(guardian)
}

#[derive(Debug)]
pub(crate) struct LossLocks {
    record: GuardianLock,
    attempt: Directory,
    _guardian: LockGuard,
    pub(crate) custody: std::sync::Arc<LockGuard>,
}
impl LossLocks {
    pub(crate) fn validate(&self, recovery: &Recovery) -> Result<()> {
        if !self.record.names_match(recovery, &self.attempt)? {
            return Err(CustodyError::Fenced);
        }
        Ok(())
    }
}

/// Same-boot loss capability. All three original lock inodes remain held while
/// Run commits its canonical Lost outcome; dropping this retains the fence.
pub struct GuardianLossRecovery {
    recovery: Recovery,
    attempt: Directory,
    record: GuardianLock,
    _guardian: LockGuard,
    _custody: LockGuard,
}
impl GuardianLossRecovery {
    pub fn scope(&self) -> Scope {
        self.recovery.scope()
    }
    /// Retain writer exclusion for the shared settlement driver. Native proof
    /// covers cleanup even when the guardian died before republishing its exit.
    pub fn into_recovery(mut self) -> Result<Recovery> {
        if !self.record.names_match(&self.recovery, &self.attempt)? {
            return Err(CustodyError::Fenced);
        }
        self.recovery.guardian_quiescent = true;
        self.recovery.loss_locks = Some(LossLocks {
            record: self.record,
            attempt: self.attempt,
            _guardian: self._guardian,
            custody: std::sync::Arc::new(self._custody),
        });
        Ok(self.recovery)
    }
    /// Call only after the trusted Run owner's terminal transaction commits.
    /// No attempt artefacts, private Git or worktree contents are removed.
    pub fn release(self) -> Result<()> {
        if !self.record.names_match(&self.recovery, &self.attempt)? {
            return Err(CustodyError::Fenced);
        }
        clear_reservation(&self.recovery.directory, self.scope())
    }
}

impl Recovery {
    /// Prove same-boot loss with the original free guardian and custody locks,
    /// the held writer lease, and a fresh complete process/worktree census.
    /// The trusted host's callback must return true only for positive worktree
    /// non-occupancy (or a read-only attempt with no worktree). Unknown is false.
    /// It runs after all locks are acquired, never before exclusion.
    pub fn after_guardian_loss(
        self,
        attempt: &Directory,
        observer: &dyn ProcessObserver,
        worktree_is_free: impl FnOnce() -> Result<bool>,
    ) -> Result<Option<GuardianLossRecovery>> {
        validate_anchor(&self.directory, &self.reservation)?;
        let record: GuardianLock = match read_record(&self.directory, RECORD, RECORD_LIMIT) {
            Ok(record) => record,
            Err(CustodyError::Filesystem(error)) if error.is_not_found() => return Ok(None),
            Err(error) => return Err(error),
        };
        let metadata = attempt.metadata()?;
        if record.schema_version != 1
            || record.scope != self.scope()
            || record.guardian_process.as_ref().is_some_and(|guardian| {
                guardian.validate().is_err()
                    || guardian.native_start == 0
                    || guardian.group_id != guardian.pid
                    || guardian.session_id != guardian.pid
                    || guardian.boot != self.reservation.boot
            })
            || self
                .reservation
                .guardian
                .as_ref()
                .is_some_and(|guardian| record.guardian_process.as_ref() != Some(guardian))
            || (self.reservation.guardian.is_none()
                && self.reservation.launch_state != LaunchState::HandingOff)
            || self.reservation.attempt_device != Some(metadata.device)
            || self.reservation.attempt_inode != Some(metadata.inode)
            || !record.attempt.matches(metadata)
            || !self
                .lease
                .as_ref()
                .map(|lease| lease.metadata())
                .transpose()?
                .is_some_and(|metadata| record.writer.matches_lock(metadata))
            || observer.boot_identity().ok().as_ref() != Some(&self.reservation.boot)
        {
            return Ok(None);
        }
        let guardian = match self
            .directory
            .try_lock_existing(OsStr::new(LOCK), LockMode::Exclusive)
        {
            Ok(lock) => lock,
            Err(error) if error.is_not_found() || matches!(error, FsError::LockTimeout { .. }) => {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        let custody = match attempt
            .try_lock_existing(OsStr::new("custody.lock"), LockMode::Exclusive)
        {
            Ok(lock) => lock,
            Err(error) if error.is_not_found() || matches!(error, FsError::LockTimeout { .. }) => {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        if !record.guardian.matches_lock(guardian.metadata()?)
            || !record.custody.matches_lock(custody.metadata()?)
        {
            return Ok(None);
        }
        let census = match observer.census(crate::census::MAX_CENSUS_ENTRIES) {
            Ok(census) if census.is_complete() => census,
            _ => return Ok(None),
        };
        // Check live roots and every retained descendant as well as their
        // sessions/groups. PID reuse supplies no death evidence: the lock and
        // the worktree census establish quiescence independently of PID values.
        let identities: Vec<_> = record
            .guardian_process
            .iter()
            .chain(self.reservation.provider.iter())
            .chain(self.reservation.descendants.iter())
            .collect();
        // A complete census lists every live process, so a recorded pid that is
        // not listed is absent; only a pid the census names as undescribable is
        // unknown (`Census::process` also reports unlisted pids as unknown).
        if identities
            .iter()
            .any(|identity| census.unknown_pids().contains_key(&identity.pid))
            || census.facts().any(|fact| {
                !fact.zombie
                    && identities.iter().any(|identity| {
                        fact.identity == **identity
                            || fact.identity.session_id == identity.session_id
                            || fact.identity.group_id == identity.group_id
                    })
            })
            || !worktree_is_free()?
        {
            return Ok(None);
        }
        // An unlinked/replaced name cannot prove exclusion even while its old
        // descriptor remains locked. Revalidate after the external census.
        if !record.names_match(&self, attempt)? {
            return Ok(None);
        }
        Ok(Some(GuardianLossRecovery {
            recovery: self,
            attempt: attempt.try_clone()?,
            record,
            _guardian: guardian,
            _custody: custody,
        }))
    }
}
