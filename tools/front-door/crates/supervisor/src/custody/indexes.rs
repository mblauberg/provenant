//! Durable custody records for per-attempt private Git indexes.
use super::*;
use provenant_os::fs::FileKind;

const ATTEMPT_INDEX: &str = "attempt-index.json";
const RETIRED_INDEX: &str = "attempt-index-retired.json";
const INDEX_LIMIT: u64 = 64 * 1024 * 1024;
const MAX_ATTEMPTS: usize = 10_000;

/// A verified private Git view retained for a worktree attempt.
pub struct RecordedAttemptIndex {
    pub scope: Scope,
    pub attempt: Directory,
    pub refs: Directory,
    pub objects: Directory,
    pub branch: String,
    pub candidate: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AttemptIndexRecord {
    schema_version: u64,
    scope: Scope,
    attempt_path: PathBuf,
    attempt_device: u64,
    attempt_inode: u64,
    candidate: String,
    refs_device: u64,
    refs_inode: u64,
    objects_device: u64,
    objects_inode: u64,
    branch: String,
    #[serde(default, flatten)]
    extra: ExtraFields,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RetiredAttemptIndex {
    schema_version: u64,
    scope: Scope,
    #[serde(default, flatten)]
    extra: ExtraFields,
}

/// Register the fixed per-attempt Git layout after Run prepares and seals its
/// private Git binding, before a provider starts or while reconciliation runs.
pub fn register_attempt_index(
    slot: &Directory,
    scope: Scope,
    attempt: &Directory,
    candidate: &str,
    branch: &str,
) -> Result<()> {
    validate_candidate(candidate)?;
    validate_branch(branch)?;
    if scope.generation == 0 {
        return Err(CustodyError::InvalidRecord);
    }
    let evidence = attempt_evidence(slot, scope)?;
    let reservation: Reservation = read_record(&evidence, "reservation.json", RECORD_LIMIT)?;
    validate_anchor(&evidence, &reservation)?;
    if reservation.scope != scope {
        return Err(CustodyError::InvalidRecord);
    }
    match &reservation.binding {
        StoredWriterBinding::Worktree {
            branch: reserved_branch,
            ..
        } if reserved_branch == branch => {}
        _ => return Err(CustodyError::InvalidRecord),
    }
    // Failed launches can leave a prepared Git view without ever spawning a
    // provider. Reconciliation must inspect and retain that view too.
    if !matches!(
        reservation.launch_state,
        LaunchState::Prepared
            | LaunchState::HandingOff
            | LaunchState::NeverSpawned
            | LaunchState::Spawned
            | LaunchState::Settling
    ) {
        return Err(CustodyError::Fenced);
    }

    let attempt_path = attempt.current_path()?;
    let attempt_metadata = attempt.metadata()?;
    if !attempt_path.is_absolute()
        || attempt_metadata.kind != FileKind::Directory
        || reservation
            .attempt_device
            .is_some_and(|device| device != attempt_metadata.device)
        || reservation
            .attempt_inode
            .is_some_and(|inode| inode != attempt_metadata.inode)
    {
        return Err(CustodyError::InvalidRecord);
    }
    let (refs, objects) = private_git_directories(attempt, candidate)?;
    let head = refs.read_stable(OsStr::new("HEAD"), 4096)?;
    if head != format!("ref: {branch}\n").as_bytes() {
        return Err(CustodyError::InvalidRecord);
    }
    refs.read_stable(OsStr::new("index"), INDEX_LIMIT)?;
    let refs_metadata = refs.metadata()?;
    let objects_metadata = objects.metadata()?;
    let record = AttemptIndexRecord {
        schema_version: 1,
        scope,
        attempt_path,
        attempt_device: attempt_metadata.device,
        attempt_inode: attempt_metadata.inode,
        candidate: candidate.to_owned(),
        refs_device: refs_metadata.device,
        refs_inode: refs_metadata.inode,
        objects_device: objects_metadata.device,
        objects_inode: objects_metadata.inode,
        branch: branch.to_owned(),
        extra: ExtraFields::default(),
    };

    let retired = read_optional::<RetiredAttemptIndex>(&evidence, RETIRED_INDEX)?;
    if let Some(retired) = &retired
        && (retired.schema_version != 1 || retired.scope != scope)
    {
        return Err(CustodyError::InvalidRecord);
    }
    // Recovery may resume after retirement but before scratch deletion. Re-arm
    // the same view under its still-live reservation before checking it again.
    let registered = match evidence.metadata_at(OsStr::new(ATTEMPT_INDEX), false) {
        Ok(_) => {
            let existing: AttemptIndexRecord = read_record(&evidence, ATTEMPT_INDEX, RECORD_LIMIT)?;
            validate_record_shape(&existing)?;
            if same_record(&existing, &record) {
                Ok(())
            } else {
                Err(CustodyError::InvalidRecord)
            }
        }
        Err(error) if error.is_not_found() => {
            Publication::require_durable(write_record(
                &evidence,
                ATTEMPT_INDEX,
                &record,
                PublishMode::NoReplace,
            )?)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    };
    registered?;
    if retired.is_some() {
        evidence.unlink(OsStr::new(RETIRED_INDEX))?;
        evidence.sync()?;
    }
    Ok(())
}

/// A scoped clean-index receipt explains an already-removed private view on
/// settlement retry; it grants no authority to delete a different view.
pub fn attempt_index_retired(slot: &Directory, scope: Scope) -> Result<bool> {
    let evidence = attempt_evidence(slot, scope)?;
    match read_optional::<RetiredAttemptIndex>(&evidence, RETIRED_INDEX)? {
        None => Ok(false),
        Some(retired) if retired.schema_version == 1 && retired.scope == scope => Ok(true),
        Some(_) => Err(CustodyError::InvalidRecord),
    }
}

/// Open every non-retired private index in the worktree custody slot.
///
/// Records are host-authored and paths are fixed below the verified attempt
/// directory. Every record still present must have matching backing
/// directories. An attempt with no prepared view has no index to inspect.
pub fn recorded_attempt_indexes(slot: &Directory) -> Result<Vec<RecordedAttemptIndex>> {
    let mut records = Vec::new();
    for (count, name) in slot.entries()?.enumerate() {
        if count >= MAX_ATTEMPTS {
            return Err(CustodyError::InvalidRecord);
        }
        let name = name?;
        if slot.metadata_at(&name, false)?.kind != FileKind::Directory {
            continue;
        }
        let evidence = slot.open_verified_directory(&name)?;
        let owner = read_optional::<OwnershipJournal>(&evidence, "ownership.json")?;
        let reservation = read_optional::<Reservation>(&evidence, "reservation.json")?;
        if let Some(reservation) = &reservation {
            validate_anchor(&evidence, reservation)?;
            return Err(CustodyError::Fenced);
        }
        let settlement = read_optional::<SettlementJournal>(&evidence, "settlement.json")?;
        let index = read_optional::<AttemptIndexRecord>(&evidence, ATTEMPT_INDEX)?;
        let retired = read_optional::<RetiredAttemptIndex>(&evidence, RETIRED_INDEX)?;
        let Some(owner) = owner else {
            if index.is_some() || retired.is_some() {
                return Err(CustodyError::InvalidRecord);
            }
            continue;
        };
        if owner.schema_version != 1 || name != OsStr::new(&owner.scope.attempt.to_string()) {
            return Err(CustodyError::InvalidRecord);
        }
        owner.binding.validate()?;
        match &owner.binding {
            StoredWriterBinding::ReadOnly { .. } => {
                if index.is_some() || retired.is_some() {
                    return Err(CustodyError::InvalidRecord);
                }
            }
            StoredWriterBinding::Worktree { branch, .. } => {
                validate_branch(branch)?;
                let Some(settlement) = settlement else {
                    // No reservation and no settlement: the stale-reservation
                    // reconcile (run controls) removes a reservation whose
                    // holder is gone, after settling the attempt as lost, and a
                    // slot that never prepared a view has no index to inspect.
                    // A view without its settlement proof stays fenced.
                    if index.is_some() || retired.is_some() {
                        return Err(CustodyError::Fenced);
                    }
                    continue;
                };
                if settlement.schema_version != 1 || settlement.scope != owner.scope {
                    return Err(CustodyError::InvalidRecord);
                }
                if let Some(retired) = retired {
                    if retired.schema_version != 1 || retired.scope != owner.scope {
                        return Err(CustodyError::InvalidRecord);
                    }
                    if let Some(index) = index {
                        validate_record_shape(&index)?;
                        if index.scope != owner.scope || index.branch != *branch {
                            return Err(CustodyError::InvalidRecord);
                        }
                    }
                    // The host published this marker only after a clean-index
                    // proof under quiescent attempt custody. Private scratch may be gone.
                    continue;
                }
                if let Some(record) = index {
                    records.push(verify_attempt_index(&record, owner.scope, branch)?);
                }
            }
        }
    }
    records.sort_by_key(|record| record.scope.attempt.to_string());
    Ok(records)
}

impl Recovery {
    /// Retire an index only after Run proves there is no prepared view or that
    /// its staged contents are clean. The verified settlement commit and
    /// quiescent recovery keep this attempt fenced through the durable marker.
    pub fn retire_attempt_index(&mut self, commit: &SettlementCommit) -> Result<()> {
        if commit.scope != self.scope() {
            return Err(CustodyError::Fenced);
        }
        self.ensure_writer_lock()?;
        if let Some(proof) = &self.loss_locks {
            proof.validate(self)?;
        }
        let current: Reservation = read_record(&self.directory, "reservation.json", RECORD_LIMIT)?;
        validate_anchor(&self.directory, &current)?;
        if current.scope != self.scope()
            || current.attempt_device != self.reservation.attempt_device
            || current.attempt_inode != self.reservation.attempt_inode
            || !current.binding.same_custody(&self.reservation.binding)
        {
            return Err(CustodyError::Fenced);
        }
        let branch = match &current.binding {
            StoredWriterBinding::Worktree { branch, .. } => branch,
            StoredWriterBinding::ReadOnly { .. } => return Err(CustodyError::InvalidRecord),
        };
        retire_index_record(&self.directory, self.scope(), branch)
    }
}

/// A withdrawn preparation has never had a provider writer. Its seeded index
/// contains no lane work and can be retired before the slot is released.
pub(super) fn retire_unlaunched_index(
    evidence: &Directory,
    reservation: &Reservation,
) -> Result<()> {
    if !matches!(
        reservation.launch_state,
        LaunchState::Prepared | LaunchState::NeverSpawned
    ) || reservation.guardian.is_some()
        || reservation.provider.is_some()
        || !reservation.descendants.is_empty()
    {
        return Err(CustodyError::Fenced);
    }
    if let StoredWriterBinding::Worktree { branch, .. } = &reservation.binding {
        retire_index_record(evidence, reservation.scope, branch)?;
    }
    Ok(())
}

/// Publish the terminal index disposition before scratch cleanup and before
/// the reservation is cleared. A missing private view is accepted only because
/// the Recovery caller established that no view was prepared.
fn retire_index_record(evidence: &Directory, scope: Scope, branch: &str) -> Result<()> {
    match read_optional::<RetiredAttemptIndex>(evidence, RETIRED_INDEX)? {
        Some(retired) if retired.schema_version == 1 && retired.scope == scope => {}
        Some(_) => return Err(CustodyError::InvalidRecord),
        None => {
            if let Some(record) = read_optional::<AttemptIndexRecord>(evidence, ATTEMPT_INDEX)? {
                validate_record_shape(&record)?;
                if record.scope != scope || record.branch != branch {
                    return Err(CustodyError::InvalidRecord);
                }
                let _verified = verify_attempt_index(&record, scope, branch)?;
            }
            let retired = RetiredAttemptIndex {
                schema_version: 1,
                scope,
                extra: ExtraFields::default(),
            };
            Publication::require_durable(write_record(
                evidence,
                RETIRED_INDEX,
                &retired,
                PublishMode::NoReplace,
            )?)?;
        }
    }
    match evidence.unlink(OsStr::new(ATTEMPT_INDEX)) {
        Ok(()) => {}
        Err(error) if error.is_not_found() => {}
        Err(error) => return Err(error.into()),
    }
    evidence.sync()?;
    Ok(())
}

fn verify_attempt_index(
    record: &AttemptIndexRecord,
    scope: Scope,
    branch: &str,
) -> Result<RecordedAttemptIndex> {
    validate_record_shape(record)?;
    if record.scope != scope || record.branch != branch {
        return Err(CustodyError::InvalidRecord);
    }
    let attempt = Directory::open_anchor(&record.attempt_path)?;
    let attempt_metadata = attempt.metadata()?;
    if attempt_metadata.kind != FileKind::Directory
        || attempt_metadata.device != record.attempt_device
        || attempt_metadata.inode != record.attempt_inode
    {
        return Err(CustodyError::InvalidRecord);
    }
    let (refs, objects) = private_git_directories(&attempt, &record.candidate)?;
    let refs_metadata = refs.metadata()?;
    let objects_metadata = objects.metadata()?;
    if refs_metadata.device != record.refs_device
        || refs_metadata.inode != record.refs_inode
        || objects_metadata.device != record.objects_device
        || objects_metadata.inode != record.objects_inode
    {
        return Err(CustodyError::InvalidRecord);
    }
    let head = refs.read_stable(OsStr::new("HEAD"), 4096)?;
    if head != format!("ref: {branch}\n").as_bytes() {
        return Err(CustodyError::InvalidRecord);
    }
    refs.read_stable(OsStr::new("index"), INDEX_LIMIT)?;
    Ok(RecordedAttemptIndex {
        scope,
        attempt,
        refs,
        objects,
        branch: branch.to_owned(),
        candidate: record.candidate.clone(),
    })
}

/// The lane's temporary directory is a sibling root of `attempts`, keyed by
/// attempt id (#3947), never a child of the attempt record directory.
fn temporary_dir(attempt: &Directory) -> Result<Directory> {
    let path = attempt.current_path()?;
    let name = path.file_name().ok_or_else(|| FsError::InvalidComponent {
        name: OsString::from("attempt"),
    })?;
    let state = path
        .parent()
        .and_then(|parent| parent.parent())
        .ok_or_else(|| FsError::InvalidComponent {
            name: OsString::from("state"),
        })?;
    let root = Directory::open_anchor(state)?;
    let scratch = root.open_verified_directory(OsStr::new("attempt-scratch"))?;
    let scratch = scratch.open_verified_directory(name)?;
    Ok(scratch.open_verified_directory(OsStr::new("tmp"))?)
}

fn private_git_directories(attempt: &Directory, candidate: &str) -> Result<(Directory, Directory)> {
    let temporary = temporary_dir(attempt)?;
    let scratch = temporary.open_verified_directory(OsStr::new(candidate))?;
    Ok((
        scratch.open_verified_directory(OsStr::new("git-common"))?,
        scratch.open_verified_directory(OsStr::new("git-objects"))?,
    ))
}

fn validate_record_shape(record: &AttemptIndexRecord) -> Result<()> {
    validate_candidate(&record.candidate)?;
    validate_branch(&record.branch)?;
    if record.schema_version != 1
        || record.scope.generation == 0
        || !record.attempt_path.is_absolute()
        || record.attempt_device == 0
        || record.attempt_inode == 0
        || record.refs_device == 0
        || record.refs_inode == 0
        || record.objects_device == 0
        || record.objects_inode == 0
    {
        return Err(CustodyError::InvalidRecord);
    }
    Ok(())
}

fn same_record(left: &AttemptIndexRecord, right: &AttemptIndexRecord) -> bool {
    left.schema_version == right.schema_version
        && left.scope == right.scope
        && left.attempt_path == right.attempt_path
        && left.attempt_device == right.attempt_device
        && left.attempt_inode == right.attempt_inode
        && left.candidate == right.candidate
        && left.refs_device == right.refs_device
        && left.refs_inode == right.refs_inode
        && left.objects_device == right.objects_device
        && left.objects_inode == right.objects_inode
        && left.branch == right.branch
}

fn validate_candidate(candidate: &str) -> Result<()> {
    let Some(number) = candidate.strip_prefix("candidate-") else {
        return Err(CustodyError::InvalidRecord);
    };
    if number.is_empty() || number.len() > 20 || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(CustodyError::InvalidRecord);
    }
    Ok(())
}

fn validate_branch(branch: &str) -> Result<()> {
    let Some(name) = branch.strip_prefix("refs/heads/") else {
        return Err(CustodyError::InvalidRecord);
    };
    if name.is_empty()
        || name.len() > 1024
        || name.starts_with('/')
        || name.ends_with('/')
        || name.contains("//")
        || name.contains("..")
        || name.contains("@{")
        || name.bytes().any(|byte| {
            byte <= b' ' || matches!(byte, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\' | 0x7f)
        })
        || name.split('/').any(|part| {
            part.is_empty()
                || part.starts_with('.')
                || part.ends_with('.')
                || part.ends_with(".lock")
        })
    {
        return Err(CustodyError::InvalidRecord);
    }
    Ok(())
}

fn read_optional<T: serde::de::DeserializeOwned>(
    directory: &Directory,
    name: &str,
) -> Result<Option<T>> {
    match read_record(directory, name, RECORD_LIMIT) {
        Ok(record) => Ok(Some(record)),
        Err(CustodyError::Filesystem(error)) if error.is_not_found() => Ok(None),
        Err(error) => Err(error),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;
    use std::fs;

    type TestResult<T = ()> = std::result::Result<T, Box<dyn Error>>;

    fn test_scope() -> Scope {
        Scope {
            attempt: AttemptId::from_bytes([39; 16]),
            generation: 1,
        }
    }

    struct Fixture {
        _root: tempfile::TempDir,
        slot: Directory,
        attempt: Directory,
        lease: Option<CustodyLease>,
        branch: &'static str,
    }

    impl Fixture {
        fn new() -> TestResult<Self> {
            let root = tempfile::tempdir()?;
            let slot_path = root.path().join("slot");
            let attempt_path = root.path().join("attempts/attempt");
            let worktree_path = root.path().join("worktree");
            let git_path = root.path().join("git");
            fs::create_dir(&slot_path)?;
            fs::create_dir_all(&attempt_path)?;
            fs::create_dir(&worktree_path)?;
            fs::create_dir(&git_path)?;
            let slot = Directory::open_anchor(&slot_path)?;
            let worktree = Directory::open_anchor(&worktree_path)?;
            let git = Directory::open_anchor(&git_path)?;
            let branch = "refs/heads/lane";
            let binding = WriterBinding::from_verified(
                &worktree,
                &git,
                branch,
                GitOid::parse(&"1".repeat(40))?,
            )?;
            let clock = provenant_os::clock::SystemClock::new();
            let cancel = CancelToken::new();
            let lease = acquire_writer(
                slot.try_clone()?,
                test_scope(),
                binding,
                WaitBudget::cli(clock.mono_now(), 1)?,
                &cancel,
                &clock,
            )?;
            let attempt = Directory::open_anchor(&attempt_path)?;
            let scratch_root = root.path().join("attempt-scratch/attempt");
            fs::create_dir_all(&scratch_root)?;
            let scratch = Directory::open_anchor(&scratch_root)?;
            scratch.mkdir(OsStr::new("tmp"), 0o700)?;
            let temporary = scratch.open_verified_directory(OsStr::new("tmp"))?;
            temporary.mkdir(OsStr::new("candidate-0"), 0o700)?;
            let scratch = temporary.open_verified_directory(OsStr::new("candidate-0"))?;
            scratch.mkdir(OsStr::new("git-common"), 0o700)?;
            let refs = scratch.open_verified_directory(OsStr::new("git-common"))?;
            refs.atomic_write(
                OsStr::new("HEAD"),
                ".head-",
                format!("ref: {branch}\n").as_bytes(),
                PublishMode::NoReplace,
            )?
            .require_durable()?;
            refs.atomic_write(
                OsStr::new("index"),
                ".index-",
                b"index fixture",
                PublishMode::NoReplace,
            )?
            .require_durable()?;
            scratch.mkdir(OsStr::new("git-objects"), 0o700)?;
            Ok(Self {
                _root: root,
                slot,
                attempt,
                lease: Some(lease),
                branch,
            })
        }

        fn register(&self) -> Result<()> {
            register_attempt_index(
                &self.slot,
                test_scope(),
                &self.attempt,
                "candidate-0",
                self.branch,
            )
        }

        fn settle(&mut self) -> Result<()> {
            self.lease
                .take()
                .ok_or(CustodyError::Fenced)?
                .into_recovery()?
                .release_lost()
        }
    }

    #[test]
    fn registered_private_git_directories_are_reopened_and_retired() -> TestResult {
        let mut fixture = Fixture::new()?;
        fixture.register()?;
        assert!(matches!(
            recorded_attempt_indexes(&fixture.slot),
            Err(CustodyError::Fenced)
        ));
        fixture.settle()?;
        let mut indexes = recorded_attempt_indexes(&fixture.slot)?;
        assert_eq!(indexes.len(), 1);
        let index = indexes.remove(0);
        assert_eq!(index.scope, test_scope());
        assert_eq!(index.branch, fixture.branch);
        assert!(
            index
                .refs
                .read_stable(OsStr::new("index"), INDEX_LIMIT)
                .is_ok()
        );
        let evidence = attempt_evidence(&fixture.slot, test_scope())?;
        retire_index_record(&evidence, test_scope(), fixture.branch)?;
        assert!(recorded_attempt_indexes(&fixture.slot)?.is_empty());
        Ok(())
    }

    #[test]
    fn changed_private_git_directory_fences_listing() -> TestResult {
        let mut fixture = Fixture::new()?;
        fixture.register()?;
        fixture.settle()?;
        let attempt = fixture.attempt.current_path()?;
        let candidate = attempt
            .parent()
            .and_then(std::path::Path::parent)
            .ok_or("state root")?
            .join("attempt-scratch/attempt/tmp/candidate-0");
        let refs = candidate.join("git-common");
        fs::rename(&refs, candidate.join("git-common-original"))?;
        fs::create_dir(&refs)?;
        assert!(matches!(
            recorded_attempt_indexes(&fixture.slot),
            Err(CustodyError::InvalidRecord)
        ));
        Ok(())
    }

    #[test]
    fn settled_attempt_without_a_prepared_view_has_no_index_to_inspect() -> TestResult {
        let mut fixture = Fixture::new()?;
        fixture.settle()?;
        assert!(recorded_attempt_indexes(&fixture.slot)?.is_empty());
        Ok(())
    }

    #[test]
    fn recovery_rearms_a_retired_index_before_rechecking_its_contents() -> TestResult {
        let mut fixture = Fixture::new()?;
        fixture.register()?;
        let evidence = attempt_evidence(&fixture.slot, test_scope())?;
        let reservation = evidence.read_stable(OsStr::new("reservation.json"), RECORD_LIMIT)?;
        fixture.settle()?;
        retire_index_record(&evidence, test_scope(), fixture.branch)?;
        assert!(attempt_index_retired(&fixture.slot, test_scope())?);
        evidence
            .atomic_write(
                OsStr::new("reservation.json"),
                ".test-",
                &reservation,
                PublishMode::NoReplace,
            )?
            .require_durable()?;
        fixture.register()?;
        assert!(!attempt_index_retired(&fixture.slot, test_scope())?);
        assert!(
            evidence
                .read_stable(OsStr::new(ATTEMPT_INDEX), RECORD_LIMIT)
                .is_ok()
        );
        Ok(())
    }

    #[test]
    fn no_git_view_can_be_retired_after_settlement() -> TestResult {
        let mut fixture = Fixture::new()?;
        fixture.settle()?;
        let evidence = attempt_evidence(&fixture.slot, test_scope())?;
        retire_index_record(&evidence, test_scope(), fixture.branch)?;
        assert!(recorded_attempt_indexes(&fixture.slot)?.is_empty());
        Ok(())
    }
}
