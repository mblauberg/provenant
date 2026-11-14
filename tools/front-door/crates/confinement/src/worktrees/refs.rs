//! Host-side ref ownership ledger. One record per (endpoint, full ref) sits in
//! the project's host state, which every cooperating Provenant writer for the
//! repository shares, so ownership outlives the worktree that made the ref.
//!
//! A record says whether Provenant created the ref or adopted one that already
//! existed. Each creation or adoption starts a new generation, so a ref that is
//! deleted and created again, even at the same commit, never inherits the
//! earlier proof. A ref without a record is unobserved; consumers retain it.
use super::*;
use provenant_core::{OperationId, ProjectId, WallTime};
use provenant_records::custody::{RefEndpoint, RefOrigin, RefOwnership};

const LEDGER: &str = "refs";
const LEDGER_LOCK: &str = "refs.lock";
const GENERATION_SEQUENCE: &str = "last-generation";
const LEDGER_LOCK_SECONDS: u64 = 5;
const MAX_REMOTE: usize = 256;
const MAX_PUSH_URL: usize = 4096;
/// Bound on records a listing reads; far above any one repository's lanes.
const MAX_LEDGER_RECORDS: usize = 65_536;
const MAX_LEDGER_ENTRIES: usize = MAX_LEDGER_RECORDS + 2;

/// The durable ownership record for one ref generation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefRecord {
    pub schema_version: u64,
    /// Project binding of the repository's Git common directory.
    pub repository: ProjectId,
    pub endpoint: RefEndpoint,
    /// Full ref name, such as `refs/heads/lane/x`.
    pub reference: String,
    /// Repository-wide generation token; it is never reused after forgetting.
    pub generation: u64,
    pub origin: RefOrigin,
    /// The ref's commit when this generation was recorded.
    pub tip: GitOid,
    pub recorded_at: WallTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<AttemptId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<OperationId>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerationSequence {
    schema_version: u64,
    repository: ProjectId,
    last_generation: u64,
}

impl RefRecord {
    /// The compact link stored beside a worktree owner.
    pub fn ownership(&self) -> RefOwnership {
        RefOwnership {
            repository: self.repository,
            endpoint: self.endpoint.clone(),
            reference: self.reference.clone(),
            generation: self.generation,
            origin: self.origin,
        }
    }
}

/// One observation to record. `origin` says what the caller saw: `Created`
/// only when its own atomic create of an absent ref succeeded.
#[derive(Clone, Debug)]
pub struct RefObservation {
    pub endpoint: RefEndpoint,
    pub reference: GitRefName,
    pub origin: RefOrigin,
    pub tip: GitOid,
    pub recorded_at: WallTime,
    pub attempt_id: Option<AttemptId>,
    pub operation_id: Option<OperationId>,
}

pub struct RefLedger {
    directory: Directory,
    repository: ProjectId,
}

impl RefLedger {
    /// The ledger under `<state>/worktrees/<project>/refs`, shared with the
    /// worktree registry of the same project.
    pub fn open(state_root: &Path, repository: ProjectId) -> Result<Self, WorktreeError> {
        let state = Directory::open_anchor(state_root)?;
        let registry = private_child(&state, OsStr::new("worktrees"))?;
        let project = private_child(&registry, OsStr::new(&repository.to_string()))?;
        Self::within(&project, repository)
    }

    pub fn for_context(context: &WorktreeContext<'_>) -> Result<Self, WorktreeError> {
        Self::within(context.project_state, context.binding.project_id())
    }

    fn within(project_state: &Directory, repository: ProjectId) -> Result<Self, WorktreeError> {
        Ok(Self {
            directory: private_child(project_state, OsStr::new(LEDGER))?,
            repository,
        })
    }

    fn generation_high_water(&self, current: Option<&RefRecord>) -> Result<u64, WorktreeError> {
        let bytes = match read_bounded(
            &self.directory,
            OsStr::new(GENERATION_SEQUENCE),
            MAX_STATE_RECORD,
        ) {
            Ok(bytes) => bytes,
            Err(error) if error.is_not_found() => {
                let names = provenant_os::fs::entry_names(&self.directory, MAX_LEDGER_ENTRIES)?;
                if names.iter().any(|name| {
                    name.as_bytes().ends_with(b".json") && !name.as_bytes().starts_with(b".")
                }) {
                    return Err(WorktreeError::InvalidOutput);
                }
                return Ok(0);
            }
            Err(error) => return Err(error.into()),
        };
        let sequence: GenerationSequence =
            serde_json::from_slice(&bytes).map_err(|_| WorktreeError::InvalidOutput)?;
        if sequence.schema_version != 1
            || sequence.repository != self.repository
            || sequence.last_generation == 0
            || current.is_some_and(|record| record.generation > sequence.last_generation)
        {
            return Err(WorktreeError::InvalidOutput);
        }
        Ok(sequence.last_generation)
    }

    fn write_generation_high_water(&self, last_generation: u64) -> Result<(), WorktreeError> {
        let sequence = GenerationSequence {
            schema_version: 1,
            repository: self.repository,
            last_generation,
        };
        self.directory
            .atomic_write(
                OsStr::new(GENERATION_SEQUENCE),
                ".ref-generation-",
                &serde_json::to_vec(&sequence).map_err(|_| WorktreeError::InvalidOutput)?,
                PublishMode::Replace,
            )?
            .require_durable()
            .map_err(|_| WorktreeError::RegistrationUnconfirmed)?;
        Ok(())
    }

    pub fn repository(&self) -> ProjectId {
        self.repository
    }

    /// The current generation for a ref, if one has been observed.
    pub fn load(
        &self,
        endpoint: &RefEndpoint,
        reference: &GitRefName,
    ) -> Result<Option<RefRecord>, WorktreeError> {
        let name = record_name(endpoint, reference)?;
        let bytes = match read_bounded(&self.directory, OsStr::new(&name), MAX_STATE_RECORD) {
            Ok(bytes) => bytes,
            Err(error) if error.is_not_found() => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let record: RefRecord =
            serde_json::from_slice(&bytes).map_err(|_| WorktreeError::InvalidOutput)?;
        if record.repository != self.repository
            || &record.endpoint != endpoint
            || record.reference != reference.as_str()
        {
            return Err(WorktreeError::InvalidOutput);
        }
        validate_record(&record)?;
        Ok(Some(record))
    }

    /// Record a creation or adoption and return the resulting generation.
    ///
    /// A fresh creation or adoption starts a generation, even when the ref
    /// still has the same tip. Only a replay carrying the same stable owner
    /// identity and tip reuses its generation.
    pub fn record(
        &self,
        observation: RefObservation,
        clock: &dyn Clock,
        cancel: &CancelToken,
    ) -> Result<RefRecord, WorktreeError> {
        validate_endpoint(&observation.endpoint)?;
        if !observation.reference.as_str().starts_with("refs/") {
            return Err(WorktreeError::InvalidRequest);
        }
        let _lock = self.lock(clock, cancel)?;
        let current = self.load(&observation.endpoint, &observation.reference)?;
        let last_generation = self.generation_high_water(current.as_ref())?;
        if let Some(current) = current.as_ref()
            && current.tip == observation.tip
            && (observation.attempt_id.is_some() || observation.operation_id.is_some())
            && current.attempt_id == observation.attempt_id
            && current.operation_id == observation.operation_id
        {
            return Ok(current.clone());
        }
        let generation = last_generation
            .checked_add(1)
            .ok_or(WorktreeError::InvalidRequest)?;
        // Publish the high-water mark first. A failed record write may leave a
        // gap, but forgetting a RefRecord can never make its token reusable.
        self.write_generation_high_water(generation)?;
        let record = RefRecord {
            schema_version: 1,
            repository: self.repository,
            endpoint: observation.endpoint,
            reference: observation.reference.as_str().to_owned(),
            generation,
            origin: observation.origin,
            tip: observation.tip,
            recorded_at: observation.recorded_at,
            attempt_id: observation.attempt_id,
            operation_id: observation.operation_id,
        };
        let name = record_name(&record.endpoint, &observation.reference)?;
        self.directory
            .atomic_write(
                OsStr::new(&name),
                ".ref-",
                &serde_json::to_vec(&record).map_err(|_| WorktreeError::InvalidOutput)?,
                PublishMode::Replace,
            )?
            .require_durable()
            .map_err(|_| WorktreeError::RegistrationUnconfirmed)?;
        Ok(record)
    }
}

impl RefLedger {
    fn lock(&self, clock: &dyn Clock, cancel: &CancelToken) -> Result<LockGuard, WorktreeError> {
        let budget = WaitBudget::cli(clock.mono_now(), LEDGER_LOCK_SECONDS)
            .map_err(|_| WorktreeError::InvalidRequest)?;
        self.directory
            .lock(
                OsStr::new(LEDGER_LOCK),
                LockMode::Exclusive,
                budget,
                cancel,
                clock,
            )
            .map_err(|error| match error {
                FsError::LockTimeout { .. } => WorktreeError::RegistryLockTimeout,
                other => other.into(),
            })
    }

    /// Every current record, in name order. A record that does not parse or
    /// belongs elsewhere is skipped, so the ref it names stays unobserved and
    /// is retained. The listing is bounded; a larger ledger is an error.
    pub fn list(&self) -> Result<Vec<RefRecord>, WorktreeError> {
        let mut names = provenant_os::fs::entry_names(&self.directory, MAX_LEDGER_ENTRIES)?;
        names.sort();
        let mut records = Vec::new();
        for name in names {
            if !name.as_bytes().ends_with(b".json") || name.as_bytes().starts_with(b".") {
                continue;
            }
            let Ok(bytes) = read_bounded(&self.directory, &name, MAX_STATE_RECORD) else {
                continue;
            };
            let Ok(record) = serde_json::from_slice::<RefRecord>(&bytes) else {
                continue;
            };
            let Ok(reference) = GitRefName::new(&record.reference) else {
                continue;
            };
            if record.repository != self.repository
                || validate_record(&record).is_err()
                || record_name(&record.endpoint, &reference).ok().as_deref() != name.to_str()
            {
                continue;
            }
            records.push(record);
        }
        Ok(records)
    }

    /// Drop a record once its ref is gone, only while it is still the
    /// generation the caller acted on. Returns whether it was removed.
    pub fn forget(
        &self,
        record: &RefRecord,
        clock: &dyn Clock,
        cancel: &CancelToken,
    ) -> Result<bool, WorktreeError> {
        let reference =
            GitRefName::new(&record.reference).map_err(|_| WorktreeError::InvalidRequest)?;
        let _lock = self.lock(clock, cancel)?;
        if self.load(&record.endpoint, &reference)?.as_ref() != Some(record) {
            return Ok(false);
        }
        self.generation_high_water(Some(record))?;
        let name = record_name(&record.endpoint, &reference)?;
        self.directory.unlink(OsStr::new(&name))?;
        self.directory.sync()?;
        Ok(true)
    }
}

fn private_child(parent: &Directory, name: &OsStr) -> Result<Directory, WorktreeError> {
    match parent.mkdir(name, 0o700) {
        Ok(()) => parent.sync()?,
        Err(error) if error.is_exists() => {}
        Err(error) => return Err(error.into()),
    }
    Ok(parent.open_verified_directory(name)?)
}

fn record_name(endpoint: &RefEndpoint, reference: &GitRefName) -> Result<String, WorktreeError> {
    let key = serde_json::to_vec(&(endpoint, reference.as_str()))
        .map_err(|_| WorktreeError::InvalidRequest)?;
    Ok(format!("{}.json", hex(&Sha256::digest(key))))
}

fn validate_endpoint(endpoint: &RefEndpoint) -> Result<(), WorktreeError> {
    match endpoint {
        RefEndpoint::Local => Ok(()),
        RefEndpoint::Remote { remote, push_url } => {
            let bad = |text: &str, limit| {
                text.is_empty() || text.len() > limit || text.chars().any(char::is_control)
            };
            if bad(remote, MAX_REMOTE) || bad(push_url, MAX_PUSH_URL) {
                Err(WorktreeError::InvalidRequest)
            } else {
                Ok(())
            }
        }
    }
}

fn validate_record(record: &RefRecord) -> Result<(), WorktreeError> {
    validate_endpoint(&record.endpoint).map_err(|_| WorktreeError::InvalidOutput)?;
    if record.schema_version != 1
        || record.generation == 0
        || GitRefName::new(&record.reference).is_err()
        || !record.reference.starts_with("refs/")
    {
        return Err(WorktreeError::InvalidOutput);
    }
    Ok(())
}

/// Record a local branch the lifecycle just created or reused. Failure leaves
/// the ref unobserved, which retains it, so it is reported rather than fatal.
pub(super) fn record_local_branch(
    context: &WorktreeContext<'_>,
    branch: &GitRefName,
    tip: GitOid,
    created: bool,
    cancel: &CancelToken,
) -> Result<RefRecord, WorktreeError> {
    let recorded_at = context
        .clock
        .wall_now()
        .map_err(|_| WorktreeError::InvalidRequest)?;
    RefLedger::for_context(context)?.record(
        RefObservation {
            endpoint: RefEndpoint::Local,
            reference: branch.clone(),
            origin: if created {
                RefOrigin::Created
            } else {
                RefOrigin::Adopted
            },
            tip,
            recorded_at,
            attempt_id: None,
            operation_id: None,
        },
        context.clock,
        cancel,
    )
}
