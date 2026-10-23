//! Protected instruction snapshots and post-quiescence quarantine.
use crate::git_evidence::{
    FileSnapshot, Git, GitEvidenceError, IndexFingerprint, MAX_DISCLOSURE_BYTES,
    MAX_DISCLOSURE_FILES, TrackedFingerprint, canonical_paths, read_git, read_head,
    read_index_fingerprint, read_optional_file, read_tracked_file, refuse_index_lock,
};
use crate::repo::RepoPath;
use crate::status::{StatusError, StatusKind, parse_porcelain_v2_z};
use crate::utility::GitRead;
use provenant_core::{GitOid, RelativePath};
use provenant_records::{CanonicalV1, FileDigest, PathWire};
use provenant_supervisor::custody::{Recovery, RecoveryState, Scope};
use provenant_supervisor::fs::{FsError, Publication, PublishMode, SafeDir};
use serde::Serialize;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

const EVIDENCE_MANIFEST_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstructionClass {
    OrdinaryInstruction,
    PolicyControl,
}

/// A caller-authorised protected path. The ordinary constructor accepts only
/// paths below `.agents/` and excludes known policy/control names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtectedPath {
    path: RepoPath,
    class: InstructionClass,
}
impl ProtectedPath {
    pub fn ordinary(path: impl AsRef<Path>) -> Result<Self, QuarantineError> {
        let path = RepoPath::new(path).map_err(|_| QuarantineError::InvalidPath)?;
        if !under_agents(path.as_path()) || is_known_control(path.as_path()) {
            return Err(QuarantineError::InvalidPath);
        }
        Ok(Self {
            path,
            class: InstructionClass::OrdinaryInstruction,
        })
    }
    pub fn policy_control(path: impl AsRef<Path>) -> Result<Self, QuarantineError> {
        let path = RepoPath::new(path).map_err(|_| QuarantineError::InvalidPath)?;
        Ok(Self {
            path,
            class: InstructionClass::PolicyControl,
        })
    }
    pub fn path(&self) -> &RepoPath {
        &self.path
    }
    pub fn class(&self) -> InstructionClass {
        self.class
    }
}

impl Git<'_> {
    pub fn snapshot_instructions(
        &self,
        scope: Scope,
        protected: &[ProtectedPath],
    ) -> Result<InstructionSnapshot, QuarantineError> {
        capture_instruction_snapshot(self, scope, protected)
    }

    pub fn quarantine_instructions(
        &self,
        recovery: &Recovery,
        snapshot: &InstructionSnapshot,
        quarantine_dir: &dyn SafeDir,
    ) -> Result<QuarantineOutcome, QuarantineError> {
        quarantine_instruction_changes(self, recovery, snapshot, quarantine_dir)
    }
}

#[derive(Debug, Error)]
pub enum QuarantineError {
    #[error("protected path or Git record is invalid")]
    InvalidPath,
    #[error("protected path is not a tracked ordinary file")]
    NotTracked,
    #[error("quarantine requires the matching, confirmed quiescence receipt")]
    QuiescenceUnconfirmed,
    #[error("protected evidence changed concurrently; no restore was attempted")]
    ConcurrentChange,
    #[error("protected evidence is incomplete or exceeds its bound")]
    Incomplete,
    #[error("protected path requires review; archived evidence was retained")]
    ReviewRequired,
    #[error("quarantine evidence publication is visible but not durably confirmed")]
    DurabilityUnconfirmed,
    #[error("evidence manifest exceeds its bound")]
    ManifestTooLarge,
    #[error("Git evidence failed: {0}")]
    Git(#[from] GitEvidenceError),
    #[error("filesystem operation failed: {0}")]
    Filesystem(#[from] FsError),
    #[error("quarantine manifest serialization failed")]
    Serialize(#[from] serde_json::Error),
    #[error("Git status record was malformed: {0}")]
    Status(#[from] StatusError),
}

#[derive(Clone, Debug)]
struct TreeRecord {
    path: RepoPath,
    mode: String,
}

#[derive(Debug)]
struct InitialFile {
    protected: ProtectedPath,
    bytes: Vec<u8>,
    fingerprint: TrackedFingerprint,
}

/// Opaque start-of-attempt facts. It is built from held repository handles and
/// cannot be deserialized into an authority-bearing snapshot.
#[derive(Debug)]
pub struct InstructionSnapshot {
    scope: Scope,
    head: GitOid,
    index: IndexFingerprint,
    index_records: Vec<u8>,
    files: Vec<InitialFile>,
}
impl InstructionSnapshot {
    pub fn head(&self) -> GitOid {
        self.head
    }
    pub fn scope(&self) -> Scope {
        self.scope
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct ArchivedPath {
    path: PathWire,
    class: InstructionClass,
    baseline: Option<TrackedFingerprint>,
    current: Option<TrackedFingerprint>,
    current_unreadable: bool,
    before_blob: Option<String>,
    after_blob: Option<String>,
    // Reserved before mutation; retained in place to preserve open-writer edits.
    retained_blob: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct ArchivedUnaccountedPath {
    path: PathWire,
    status: StatusKind,
    current: Option<TrackedFingerprint>,
    current_blob: Option<String>,
    unreadable: bool,
}

struct UnaccountedSnapshot {
    path: RepoPath,
    status: StatusKind,
    current: Option<FileSnapshot>,
    unreadable: bool,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct EvidenceManifest {
    schema_version: u64,
    scope: Scope,
    kind: &'static str,
    start_head: GitOid,
    observed_head: GitOid,
    start_index: IndexFingerprint,
    observed_index: IndexFingerprint,
    status_digest: FileDigest,
    commit_diff_digest: FileDigest,
    index_diff_digest: FileDigest,
    worktree_diff_digest: FileDigest,
    diff_digest: FileDigest,
    paths: Vec<ArchivedPath>,
    unaccounted_paths: Vec<ArchivedUnaccountedPath>,
    review_required: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuarantineOutcome {
    Restored {
        paths: Vec<PathWire>,
        evidence_digest: FileDigest,
    },
    ReviewRequired {
        evidence_digest: FileDigest,
    },
}

/// Captures initial protected Git and disk state before launch. Callers provide
/// the trusted path scope and their explicit ordinary/control classification.
fn capture_instruction_snapshot(
    git: &Git<'_>,
    scope: Scope,
    protected: &[ProtectedPath],
) -> Result<InstructionSnapshot, QuarantineError> {
    let executor = git.executor;
    let repo = git.repo;
    let runtime = git.runtime;
    let budget = git.budget;
    let cancel = git.cancel;
    if protected.is_empty() || protected.len() > MAX_DISCLOSURE_FILES {
        return Err(QuarantineError::Incomplete);
    }
    let mut protected = protected.to_vec();
    protected.sort_by(|left, right| path_bytes(&left.path).cmp(path_bytes(&right.path)));
    if protected.windows(2).any(|pair| {
        pair.first()
            .zip(pair.get(1))
            .is_some_and(|(left, right)| path_bytes(&left.path) == path_bytes(&right.path))
    }) {
        return Err(QuarantineError::InvalidPath);
    }
    let head = read_head(executor, repo, runtime, budget, cancel)?;
    refuse_index_lock(repo.private_dir())?;
    let index = read_index_fingerprint(repo.private_dir())?;
    let mut tree = Vec::new();
    for item in &protected {
        let tree_raw = read_git(
            executor,
            repo,
            runtime,
            budget,
            cancel,
            GitRead::TreeEntries {
                oid: head,
                prefix: item.path.clone(),
            },
        )?
        .stdout
        .bytes;
        tree.extend(parse_tree_entries(&tree_raw)?);
        if tree.len() > MAX_DISCLOSURE_FILES {
            return Err(QuarantineError::Incomplete);
        }
    }
    let paths = protected
        .iter()
        .map(|item| item.path.clone())
        .collect::<Vec<_>>();
    let index_records = read_git(
        executor,
        repo,
        runtime,
        budget,
        cancel,
        GitRead::IndexEntries {
            paths: paths.clone(),
        },
    )?
    .stdout
    .bytes;
    let index_by_path = parse_index_entries(&index_records)?;
    let mut total = 0_u64;
    let mut files = Vec::with_capacity(protected.len());
    for path in protected {
        let tree_entry = tree
            .iter()
            .find(|entry| path_bytes(&entry.path) == path_bytes(&path.path))
            .ok_or(QuarantineError::NotTracked)?;
        if !matches!(tree_entry.mode.as_str(), "100644" | "100755") {
            return Err(QuarantineError::NotTracked);
        }
        let index_matches = index_by_path
            .iter()
            .filter(|entry| entry.path.as_slice() == path_bytes(&path.path))
            .collect::<Vec<_>>();
        if index_matches.len() != 1 {
            return Err(QuarantineError::NotTracked);
        }
        let index_entry = index_matches.first().ok_or(QuarantineError::NotTracked)?;
        if index_entry.stage != 0 || !matches!(index_entry.mode, 0o100644 | 0o100755) {
            return Err(QuarantineError::NotTracked);
        }
        let file = read_tracked_file(repo.root_dir(), &path.path, MAX_DISCLOSURE_BYTES)?;
        if file.fingerprint.device != Some(repo.root_identity().device)
            || file.fingerprint.links != Some(1)
        {
            return Err(QuarantineError::NotTracked);
        }
        total = total
            .checked_add(file.bytes.len() as u64)
            .ok_or(QuarantineError::Incomplete)?;
        if total > MAX_DISCLOSURE_BYTES {
            return Err(QuarantineError::Incomplete);
        }
        files.push(InitialFile {
            protected: path,
            bytes: file.bytes,
            fingerprint: file.fingerprint,
        });
    }
    let index_after = read_index_fingerprint(repo.private_dir())?;
    refuse_index_lock(repo.private_dir())?;
    let head_after = read_head(executor, repo, runtime, budget, cancel)?;
    repo.revalidate()
        .map_err(|_| QuarantineError::ConcurrentChange)?;
    if head != head_after || index != index_after {
        return Err(QuarantineError::ConcurrentChange);
    }
    Ok(InstructionSnapshot {
        scope,
        head,
        index,
        index_records,
        files,
    })
}

/// Archive status, pinned HEAD/index/worktree patches and tracked blobs before
/// restoring any attributable ordinary instruction file. The opaque Recovery
/// borrow holds supervisor's writer exclusion for the whole operation.
fn quarantine_instruction_changes(
    git: &Git<'_>,
    recovery: &Recovery,
    snapshot: &InstructionSnapshot,
    quarantine_dir: &dyn SafeDir,
) -> Result<QuarantineOutcome, QuarantineError> {
    let executor = git.executor;
    let repo = git.repo;
    let runtime = git.runtime;
    let budget = git.budget;
    let cancel = git.cancel;
    if recovery.scope() != snapshot.scope || recovery.observe() != RecoveryState::Absent {
        return Err(QuarantineError::QuiescenceUnconfirmed);
    }
    repo.revalidate()
        .map_err(|_| QuarantineError::ConcurrentChange)?;
    refuse_index_lock(repo.private_dir())?;
    let index_before = read_index_fingerprint(repo.private_dir())?;
    let head_before = read_head(executor, repo, runtime, budget, cancel)?;
    let status = read_git(executor, repo, runtime, budget, cancel, GitRead::Status)?;
    let status_paths = parse_porcelain_v2_z(&status.stdout.bytes)?;
    let selected = snapshot
        .files
        .iter()
        .map(|file| file.protected.path.clone())
        .collect::<Vec<_>>();
    let selected_names = selected
        .iter()
        .map(|path| path_bytes(path).to_vec())
        .collect::<Vec<_>>();
    let mut evidence_paths = selected.clone();
    for record in &status_paths {
        for raw_path in std::iter::once(&record.path).chain(record.original_path.iter()) {
            if is_protected_evidence_path(raw_path) {
                let path = RepoPath::new(PathBuf::from(OsString::from_vec(raw_path.clone())))
                    .map_err(|_| QuarantineError::InvalidPath)?;
                evidence_paths.push(path);
            }
        }
    }
    let evidence_paths = canonical_paths(&evidence_paths)?;
    if evidence_paths.len() > MAX_DISCLOSURE_FILES {
        return Err(QuarantineError::Incomplete);
    }
    let commit_diff = read_git(
        executor,
        repo,
        runtime,
        budget,
        cancel,
        GitRead::Diff {
            base: snapshot.head,
            head: head_before,
            paths: evidence_paths.clone(),
        },
    )?
    .stdout
    .bytes;
    let index_diff = read_git(
        executor,
        repo,
        runtime,
        budget,
        cancel,
        GitRead::DiffIndex {
            paths: evidence_paths.clone(),
        },
    )?
    .stdout
    .bytes;
    let worktree_diff = read_git(
        executor,
        repo,
        runtime,
        budget,
        cancel,
        GitRead::DiffWorktree {
            paths: evidence_paths.clone(),
        },
    )?
    .stdout
    .bytes;
    let mut review = head_before != snapshot.head || index_before != snapshot.index;
    let mut current = Vec::with_capacity(snapshot.files.len());
    let mut current_unreadable = Vec::with_capacity(snapshot.files.len());
    let mut total = 0_u64;
    for file in &snapshot.files {
        let observed =
            read_optional_file(repo.root_dir(), &file.protected.path, MAX_DISCLOSURE_BYTES);
        match observed {
            Ok(observed) => {
                if let Some(observed) = &observed {
                    total = total
                        .checked_add(observed.bytes.len() as u64)
                        .ok_or(QuarantineError::Incomplete)?;
                }
                let status_changed = status_paths.iter().any(|record| {
                    record.path.as_slice() == path_bytes(&file.protected.path)
                        && record.kind != StatusKind::Ignored
                });
                let content_changed = observed.as_ref().map(|file| file.fingerprint.digest)
                    != Some(file.fingerprint.digest);
                let fingerprint_changed =
                    observed.as_ref().map(|file| &file.fingerprint) != Some(&file.fingerprint);
                if file.protected.class == InstructionClass::PolicyControl
                    && (status_changed || content_changed || fingerprint_changed)
                {
                    review = true;
                }
                if file.protected.class == InstructionClass::OrdinaryInstruction
                    && fingerprint_changed
                    && !content_changed
                {
                    review = true;
                }
                current.push(observed);
                current_unreadable.push(false);
            }
            Err(_) => {
                review = true;
                current.push(None);
                current_unreadable.push(true);
            }
        }
    }
    if total > MAX_DISCLOSURE_BYTES {
        return Err(QuarantineError::Incomplete);
    }
    let mut unaccounted_snapshots = Vec::new();
    for record in &status_paths {
        for raw_path in std::iter::once(&record.path).chain(record.original_path.iter()) {
            if !is_protected_evidence_path(raw_path) {
                continue;
            }
            let selected_path = selected_names
                .iter()
                .any(|path| path.as_slice() == raw_path.as_slice());
            if !selected_path {
                review = true;
                let path = RepoPath::new(PathBuf::from(OsString::from_vec(raw_path.clone())))
                    .map_err(|_| QuarantineError::InvalidPath)?;
                let (observed, unreadable) =
                    match read_optional_file(repo.root_dir(), &path, MAX_DISCLOSURE_BYTES) {
                        Ok(observed) => (observed, false),
                        Err(_) => (None, true),
                    };
                if let Some(observed) = &observed {
                    total = total
                        .checked_add(observed.bytes.len() as u64)
                        .ok_or(QuarantineError::Incomplete)?;
                    if total > MAX_DISCLOSURE_BYTES {
                        return Err(QuarantineError::Incomplete);
                    }
                }
                if unreadable {
                    review = true;
                }
                unaccounted_snapshots.push(UnaccountedSnapshot {
                    path,
                    status: record.kind,
                    current: observed,
                    unreadable,
                });
            }
        }
        if matches!(record.kind, StatusKind::Conflict | StatusKind::RenameOrCopy) {
            review = true;
        }
    }
    let index_records_after = read_git(
        executor,
        repo,
        runtime,
        budget,
        cancel,
        GitRead::IndexEntries {
            paths: selected.clone(),
        },
    )?
    .stdout
    .bytes;
    let index_after_observation = read_index_fingerprint(repo.private_dir())?;
    refuse_index_lock(repo.private_dir())?;
    let head_after_observation = read_head(executor, repo, runtime, budget, cancel)?;
    let status_after = read_git(executor, repo, runtime, budget, cancel, GitRead::Status)?;
    if index_before != index_after_observation
        || head_before != head_after_observation
        || status.stdout.bytes != status_after.stdout.bytes
    {
        return Err(QuarantineError::ConcurrentChange);
    }
    if index_records_after != snapshot.index_records {
        review = true;
    }
    repo.revalidate()
        .map_err(|_| QuarantineError::ConcurrentChange)?;

    let mut manifest_paths = Vec::new();
    let mut manifest_unaccounted = Vec::new();
    let mut archive_bytes = status.stdout.bytes.len() as u64;
    archive_bytes = archive_bytes
        .checked_add(commit_diff.len() as u64)
        .and_then(|value| value.checked_add(index_diff.len() as u64))
        .and_then(|value| value.checked_add(worktree_diff.len() as u64))
        .ok_or(QuarantineError::Incomplete)?;
    put(
        quarantine_dir,
        OsStr::new("git-status.z"),
        &status.stdout.bytes,
    )?;
    put(
        quarantine_dir,
        OsStr::new("protected.head.patch"),
        &commit_diff,
    )?;
    put(
        quarantine_dir,
        OsStr::new("protected.index.patch"),
        &index_diff,
    )?;
    put(
        quarantine_dir,
        OsStr::new("protected.disk.patch"),
        &worktree_diff,
    )?;
    quarantine_dir.mkdir(OsStr::new("blobs"), 0o700)?;
    let blobs = quarantine_dir.open_verified_directory(OsStr::new("blobs"))?;
    for (index, file) in snapshot.files.iter().enumerate() {
        let prefix = format!("{index:08}");
        let before_name = format!("{prefix}.before");
        let after_name = format!("{prefix}.after");
        archive_bytes = archive_bytes
            .checked_add(file.bytes.len() as u64)
            .ok_or(QuarantineError::Incomplete)?;
        put(blobs.as_ref(), OsStr::new(&before_name), &file.bytes)?;
        let observed = current.get(index).and_then(Option::as_ref);
        if let Some(observed) = observed {
            archive_bytes = archive_bytes
                .checked_add(observed.bytes.len() as u64)
                .ok_or(QuarantineError::Incomplete)?;
            put(blobs.as_ref(), OsStr::new(&after_name), &observed.bytes)?;
        }
        let relative = RelativePath::parse(file.protected.path.as_path())
            .map_err(|_| QuarantineError::InvalidPath)?;
        manifest_paths.push(ArchivedPath {
            path: PathWire::from_relative(&relative).map_err(|_| QuarantineError::InvalidPath)?,
            class: file.protected.class,
            baseline: Some(file.fingerprint.clone()),
            current: observed.map(|observed| observed.fingerprint.clone()),
            current_unreadable: current_unreadable.get(index).copied().unwrap_or(true),
            before_blob: Some(before_name),
            after_blob: observed.map(|_| after_name),
            retained_blob: if file.protected.class == InstructionClass::OrdinaryInstruction
                && observed.is_some()
            {
                Some(retained_name(index))
            } else {
                None
            },
        });
        let changed = observed.map(|item| item.fingerprint.digest) != Some(file.fingerprint.digest);
        if file.protected.class == InstructionClass::PolicyControl && changed {
            review = true;
        }
        if file.protected.class == InstructionClass::OrdinaryInstruction && changed {
            let has_simple_status = status_paths.iter().any(|record| {
                record.path.as_slice() == path_bytes(&file.protected.path)
                    && record.kind == StatusKind::Changed
            });
            if !has_simple_status {
                review = true;
            }
        }
    }
    for (index, item) in unaccounted_snapshots.iter().enumerate() {
        let current_blob = if let Some(current) = item.current.as_ref() {
            let blob_name = format!("unaccounted-{index:08}.current");
            archive_bytes = archive_bytes
                .checked_add(current.bytes.len() as u64)
                .ok_or(QuarantineError::Incomplete)?;
            put(blobs.as_ref(), OsStr::new(&blob_name), &current.bytes)?;
            Some(blob_name)
        } else {
            None
        };
        let relative =
            RelativePath::parse(item.path.as_path()).map_err(|_| QuarantineError::InvalidPath)?;
        manifest_unaccounted.push(ArchivedUnaccountedPath {
            path: PathWire::from_relative(&relative).map_err(|_| QuarantineError::InvalidPath)?,
            status: item.status,
            current: item
                .current
                .as_ref()
                .map(|current| current.fingerprint.clone()),
            current_blob,
            unreadable: item.unreadable,
        });
    }
    if archive_bytes > MAX_DISCLOSURE_BYTES.saturating_mul(3) {
        return Err(QuarantineError::Incomplete);
    }
    let digest_diff = digest_parts(&commit_diff, &index_diff, &worktree_diff);
    let evidence = EvidenceManifest {
        schema_version: 1,
        scope: snapshot.scope,
        kind: "instruction_quarantine_evidence",
        start_head: snapshot.head,
        observed_head: head_before,
        start_index: snapshot.index,
        observed_index: index_before,
        status_digest: CanonicalV1::file_digest(&status.stdout.bytes),
        commit_diff_digest: CanonicalV1::file_digest(&commit_diff),
        index_diff_digest: CanonicalV1::file_digest(&index_diff),
        worktree_diff_digest: CanonicalV1::file_digest(&worktree_diff),
        diff_digest: digest_diff,
        paths: manifest_paths,
        unaccounted_paths: manifest_unaccounted,
        review_required: review,
    };
    let evidence_bytes = serde_json::to_vec(&evidence)?;
    if evidence_bytes.len() > EVIDENCE_MANIFEST_LIMIT {
        return Err(QuarantineError::ManifestTooLarge);
    }
    put(quarantine_dir, OsStr::new("evidence.json"), &evidence_bytes)?;
    let evidence_digest = CanonicalV1::file_digest(&evidence_bytes);
    if recovery.observe() != RecoveryState::Absent {
        return Err(QuarantineError::QuiescenceUnconfirmed);
    }
    if review {
        let result = QuarantineOutcome::ReviewRequired { evidence_digest };
        write_result(quarantine_dir, snapshot.scope, evidence_digest, &result)?;
        return Ok(result);
    }

    let mut restored_paths = Vec::new();
    for (index, file) in snapshot.files.iter().enumerate() {
        if file.protected.class != InstructionClass::OrdinaryInstruction {
            continue;
        }
        let observed = current.get(index).and_then(Option::as_ref);
        let current_digest = observed.map(|item| item.fingerprint.digest);
        if current_digest == Some(file.fingerprint.digest) {
            continue;
        }
        if recovery.observe() != RecoveryState::Absent {
            return Err(QuarantineError::QuiescenceUnconfirmed);
        }
        let latest =
            read_optional_file(repo.root_dir(), &file.protected.path, MAX_DISCLOSURE_BYTES)?;
        if latest.as_ref().map(|item| &item.fingerprint) != observed.map(|item| &item.fingerprint) {
            return Err(QuarantineError::ConcurrentChange);
        }
        restore_file(
            repo.root_dir(),
            &file.protected.path,
            &file.bytes,
            latest.as_ref(),
            quarantine_dir,
            OsStr::new(&retained_name(index)),
        )?;
        let restored =
            read_tracked_file(repo.root_dir(), &file.protected.path, MAX_DISCLOSURE_BYTES)?;
        if restored.fingerprint.digest != file.fingerprint.digest {
            return Err(QuarantineError::ConcurrentChange);
        }
        restored_paths.push(
            PathWire::from_relative(
                &RelativePath::parse(file.protected.path.as_path())
                    .map_err(|_| QuarantineError::InvalidPath)?,
            )
            .map_err(|_| QuarantineError::InvalidPath)?,
        );
    }
    if recovery.observe() != RecoveryState::Absent {
        return Err(QuarantineError::QuiescenceUnconfirmed);
    }
    repo.revalidate()
        .map_err(|_| QuarantineError::ConcurrentChange)?;
    let outcome = QuarantineOutcome::Restored {
        paths: restored_paths,
        evidence_digest,
    };
    write_result(quarantine_dir, snapshot.scope, evidence_digest, &outcome)?;
    Ok(outcome)
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct QuarantineResultRecord {
    schema_version: u64,
    scope: Scope,
    kind: &'static str,
    evidence_digest: FileDigest,
    restored: Vec<PathWire>,
    review_required: bool,
}
fn write_result(
    directory: &dyn SafeDir,
    scope: Scope,
    evidence_digest: FileDigest,
    result: &QuarantineOutcome,
) -> Result<(), QuarantineError> {
    let (restored, review_required) = match result {
        QuarantineOutcome::Restored { paths, .. } => (paths.clone(), false),
        QuarantineOutcome::ReviewRequired { .. } => (Vec::new(), true),
    };
    let bytes = serde_json::to_vec(&QuarantineResultRecord {
        schema_version: 1,
        scope,
        kind: "instruction_quarantine_result",
        evidence_digest,
        restored,
        review_required,
    })?;
    put(directory, OsStr::new("quarantine-result.json"), &bytes)
}

fn put(directory: &dyn SafeDir, name: &OsStr, bytes: &[u8]) -> Result<(), QuarantineError> {
    match directory.atomic_write(name, ".quarantine-", bytes, PublishMode::NoReplace)? {
        Publication::Durable => Ok(()),
        Publication::VisibleDurabilityUnconfirmed { .. } => {
            Err(QuarantineError::DurabilityUnconfirmed)
        }
    }
}

/// Preserve the old inode in quarantine before publishing a baseline. No
/// operation replaces a concurrently created destination. The retained name is
/// recorded before mutation and never unlinked: an open writer can still use it.
fn retained_name(index: usize) -> String {
    format!("{index:08}.retained")
}

fn restore_file(
    root: &dyn SafeDir,
    path: &RepoPath,
    bytes: &[u8],
    observed: Option<&FileSnapshot>,
    archive: &dyn SafeDir,
    retained: &OsStr,
) -> Result<(), QuarantineError> {
    let (parent, leaf) = open_parent(root, path)?;
    if let Some(observed) = observed {
        parent.rename_noreplace_to(&leaf, archive, retained)?;
        let source_sync = parent.sync();
        let destination_sync = archive.sync();
        source_sync?;
        destination_sync?;
        let retained_path =
            RepoPath::new(Path::new(retained)).map_err(|_| QuarantineError::InvalidPath)?;
        let saved = read_tracked_file(archive, &retained_path, MAX_DISCLOSURE_BYTES)?;
        let mut saved_fingerprint = saved.fingerprint;
        saved_fingerprint.path = observed.fingerprint.path.clone();
        if saved_fingerprint != observed.fingerprint {
            // A concurrent writer's inode is restored only into a vacant name.
            let _ = archive.rename_noreplace_to(retained, &*parent, &leaf);
            let source_sync = parent.sync();
            let destination_sync = archive.sync();
            source_sync?;
            destination_sync?;
            return Err(QuarantineError::ConcurrentChange);
        }
    }
    match parent.atomic_write(
        &leaf,
        ".instruction-restore-",
        bytes,
        PublishMode::NoReplace,
    )? {
        Publication::Durable => Ok(()),
        Publication::VisibleDurabilityUnconfirmed { .. } => {
            Err(QuarantineError::DurabilityUnconfirmed)
        }
    }
}

enum HeldDir<'a> {
    Borrowed(&'a dyn SafeDir),
    Owned(Box<dyn SafeDir>),
}
impl<'a> std::ops::Deref for HeldDir<'a> {
    type Target = dyn SafeDir + 'a;
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Borrowed(directory) => *directory,
            Self::Owned(directory) => directory.as_ref(),
        }
    }
}
fn open_parent<'a>(
    root: &'a dyn SafeDir,
    path: &RepoPath,
) -> Result<(HeldDir<'a>, OsString), QuarantineError> {
    let mut parts = Vec::new();
    for component in path.as_path().components() {
        if let Component::Normal(name) = component {
            parts.push(name.to_os_string());
        } else {
            return Err(QuarantineError::InvalidPath);
        }
    }
    if parts.is_empty() {
        return Err(QuarantineError::InvalidPath);
    }
    let leaf = parts.pop().ok_or(QuarantineError::InvalidPath)?;
    let mut parent = HeldDir::Borrowed(root);
    for component in parts {
        let next = parent.open_verified_directory(&component)?;
        parent = HeldDir::Owned(next);
    }
    Ok((parent, leaf))
}

fn parse_tree_entries(raw: &[u8]) -> Result<Vec<TreeRecord>, QuarantineError> {
    if !raw.is_empty() && !raw.ends_with(&[0]) {
        return Err(QuarantineError::Incomplete);
    }
    let mut records = Vec::new();
    for record in raw
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let (metadata, path) = split_once(record, b'\t').ok_or(QuarantineError::Incomplete)?;
        let fields = metadata.split(|byte| *byte == b' ').collect::<Vec<_>>();
        if fields.len() != 3 || path.is_empty() {
            return Err(QuarantineError::Incomplete);
        }
        let mode = std::str::from_utf8(fields.first().ok_or(QuarantineError::Incomplete)?)
            .map_err(|_| QuarantineError::Incomplete)?
            .to_owned();
        let object_type = fields.get(1).ok_or(QuarantineError::Incomplete)?;
        if *object_type != b"blob" {
            continue;
        }
        let oid_text = std::str::from_utf8(fields.get(2).ok_or(QuarantineError::Incomplete)?)
            .map_err(|_| QuarantineError::Incomplete)?;
        GitOid::parse(oid_text).map_err(|_| QuarantineError::Incomplete)?;
        let path = RepoPath::new(PathBuf::from(OsString::from_vec(path.to_vec())))
            .map_err(|_| QuarantineError::InvalidPath)?;
        records.push(TreeRecord { path, mode });
        if records.len() > MAX_DISCLOSURE_FILES {
            return Err(QuarantineError::Incomplete);
        }
    }
    Ok(records)
}

struct IndexRecord {
    path: Vec<u8>,
    mode: u32,
    stage: u8,
}

fn parse_index_entries(raw: &[u8]) -> Result<Vec<IndexRecord>, QuarantineError> {
    if !raw.is_empty() && !raw.ends_with(&[0]) {
        return Err(QuarantineError::Incomplete);
    }
    let mut entries = Vec::new();
    for record in raw
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let (metadata, path) = split_once(record, b'\t').ok_or(QuarantineError::Incomplete)?;
        if path.is_empty() {
            return Err(QuarantineError::Incomplete);
        }
        let fields = metadata.split(|byte| *byte == b' ').collect::<Vec<_>>();
        if fields.len() != 3 {
            return Err(QuarantineError::Incomplete);
        }
        let mode_text = std::str::from_utf8(fields.first().ok_or(QuarantineError::Incomplete)?)
            .map_err(|_| QuarantineError::Incomplete)?;
        let mode = u32::from_str_radix(mode_text, 8).map_err(|_| QuarantineError::Incomplete)?;
        let oid = std::str::from_utf8(fields.get(1).ok_or(QuarantineError::Incomplete)?)
            .map_err(|_| QuarantineError::Incomplete)?;
        GitOid::parse(oid).map_err(|_| QuarantineError::Incomplete)?;
        let stage_text = std::str::from_utf8(fields.get(2).ok_or(QuarantineError::Incomplete)?)
            .map_err(|_| QuarantineError::Incomplete)?;
        let stage = stage_text
            .parse::<u8>()
            .map_err(|_| QuarantineError::Incomplete)?;
        if stage > 3 {
            return Err(QuarantineError::Incomplete);
        }
        let path_bytes = path.to_vec();
        entries.push(IndexRecord {
            path: path_bytes,
            mode,
            stage,
        });
        if entries.len() > MAX_DISCLOSURE_FILES {
            return Err(QuarantineError::Incomplete);
        }
    }
    Ok(entries)
}

fn split_once(bytes: &[u8], separator: u8) -> Option<(&[u8], &[u8])> {
    let index = bytes.iter().position(|byte| *byte == separator)?;
    Some((bytes.get(..index)?, bytes.get(index.saturating_add(1)..)?))
}

fn digest_parts(head: &[u8], index: &[u8], disk: &[u8]) -> FileDigest {
    let mut bytes = Vec::with_capacity(
        head.len()
            .saturating_add(index.len())
            .saturating_add(disk.len())
            .saturating_add(32),
    );
    for (label, value) in [
        (b"head".as_slice(), head),
        (b"index".as_slice(), index),
        (b"disk".as_slice(), disk),
    ] {
        bytes.extend_from_slice(&(label.len() as u64).to_le_bytes());
        bytes.extend_from_slice(label);
        bytes.extend_from_slice(&(value.len() as u64).to_le_bytes());
        bytes.extend_from_slice(value);
    }
    CanonicalV1::file_digest(&bytes)
}

fn path_bytes(path: &RepoPath) -> &[u8] {
    path.as_os_str().as_bytes()
}
fn under_agents(path: &Path) -> bool {
    path.strip_prefix(".agents")
        .is_ok_and(|relative| !relative.as_os_str().is_empty())
}
fn under_agents_bytes(path: &[u8]) -> bool {
    path.starts_with(b".agents/")
}
fn is_protected_evidence_path(path: &[u8]) -> bool {
    if under_agents_bytes(path) {
        return true;
    }
    is_known_control(Path::new(OsStr::from_bytes(path)))
}
fn is_known_control(path: &Path) -> bool {
    path.components().any(|component| {
        let Component::Normal(name) = component else {
            return true;
        };
        let bytes = name.as_bytes();
        bytes.eq_ignore_ascii_case(b"AGENTS.md")
            || bytes.eq_ignore_ascii_case(b"HARNESS.md")
            || bytes.eq_ignore_ascii_case(b".git")
            || bytes.eq_ignore_ascii_case(b"git")
            || bytes.eq_ignore_ascii_case(b"config")
            || bytes.eq_ignore_ascii_case(b"hooks")
            || bytes
                .get(..7)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"config."))
    })
}

#[cfg(test)]
mod tests {
    use super::{InstructionClass, ProtectedPath, parse_index_entries, parse_tree_entries};
    use std::error::Error;

    #[test]
    fn ordinary_instruction_class_requires_agents_path_and_rejects_control_names()
    -> Result<(), Box<dyn Error>> {
        assert_eq!(
            ProtectedPath::ordinary(".agents/notes/rules.md")?.class(),
            InstructionClass::OrdinaryInstruction
        );
        assert!(ProtectedPath::ordinary(".agents/AGENTS.md").is_err());
        assert!(ProtectedPath::ordinary(".agents/config.yaml").is_err());
        assert!(ProtectedPath::ordinary("skills/example/SKILL.md").is_err());
        assert_eq!(
            ProtectedPath::policy_control(".agents/AGENTS.md")?.class(),
            InstructionClass::PolicyControl
        );
        assert_eq!(
            ProtectedPath::policy_control("AGENTS.md")?.class(),
            InstructionClass::PolicyControl
        );
        assert!(ProtectedPath::policy_control("../AGENTS.md").is_err());
        Ok(())
    }

    #[test]
    fn tree_and_index_records_preserve_nul_delimited_path_bytes() -> Result<(), Box<dyn Error>> {
        let oid = b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let mut tree = b"100644 blob ".to_vec();
        tree.extend_from_slice(oid);
        tree.extend_from_slice(b"\t.agents/a b\xff.md\0");
        let parsed = parse_tree_entries(&tree)?;
        assert_eq!(parsed.len(), 1);
        let mut index = b"100644 ".to_vec();
        index.extend_from_slice(oid);
        index.extend_from_slice(b" 0\t.agents/a b\xff.md\0");
        let parsed_index = parse_index_entries(&index)?;
        assert_eq!(parsed_index.len(), 1);
        assert_eq!(
            parsed_index.first().map(|row| row.path.as_slice()),
            Some(&b".agents/a b\xff.md"[..])
        );
        let conflict = [b"100644 ".as_slice(), oid, b" 2\t.agents/a b\xff.md\0"].concat();
        assert_eq!(
            parse_index_entries(&conflict)?.first().map(|row| row.stage),
            Some(2)
        );
        Ok(())
    }
}
#[cfg(test)]
mod restore_race_tests {
    use super::*;
    use provenant_supervisor::fs::{LocalFs, SafeFs};
    use std::error::Error;
    use std::io::Write;

    #[test]
    fn restored_baseline_preserves_future_writes_to_displaced_inode() -> Result<(), Box<dyn Error>>
    {
        let temp = tempfile::tempdir()?;
        let root_path = temp.path().join("root");
        let archive_path = temp.path().join("archive");
        std::fs::create_dir(&root_path)?;
        std::fs::create_dir(&archive_path)?;
        std::fs::write(root_path.join("notes"), b"provider edit")?;
        let root = LocalFs.open_anchor(&root_path)?;
        let archive = LocalFs.open_anchor(&archive_path)?;
        let path = RepoPath::new("notes")?;
        let observed = read_tracked_file(root.as_ref(), &path, 1024)?;
        let mut concurrent_writer = std::fs::OpenOptions::new()
            .append(true)
            .open(root_path.join("notes"))?;
        restore_file(
            root.as_ref(),
            &path,
            b"baseline",
            Some(&observed),
            archive.as_ref(),
            OsStr::new("retained"),
        )?;
        concurrent_writer.write_all(b" later edit")?;
        assert_eq!(std::fs::read(root_path.join("notes"))?, b"baseline");
        assert_eq!(
            std::fs::read(archive_path.join("retained"))?,
            b"provider edit later edit"
        );
        Ok(())
    }

    #[test]
    fn raced_replacement_is_retained_and_rolled_back_without_overwrite()
    -> Result<(), Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        let root_path = temp.path().join("root");
        let archive_path = temp.path().join("archive");
        std::fs::create_dir(&root_path)?;
        std::fs::create_dir(&archive_path)?;
        std::fs::write(root_path.join("notes"), b"observed")?;
        let root = LocalFs.open_anchor(&root_path)?;
        let archive = LocalFs.open_anchor(&archive_path)?;
        let path = RepoPath::new("notes")?;
        let observed = read_tracked_file(root.as_ref(), &path, 1024)?;
        std::fs::write(root_path.join("replacement"), b"concurrent")?;
        std::fs::rename(root_path.join("replacement"), root_path.join("notes"))?;
        assert!(matches!(
            restore_file(
                root.as_ref(),
                &path,
                b"baseline",
                Some(&observed),
                archive.as_ref(),
                OsStr::new("retained")
            ),
            Err(QuarantineError::ConcurrentChange)
        ));
        assert_eq!(std::fs::read(root_path.join("notes"))?, b"concurrent");
        std::fs::write(archive_path.join("occupied"), b"old archive")?;
        let now = read_tracked_file(root.as_ref(), &path, 1024)?;
        assert!(
            restore_file(
                root.as_ref(),
                &path,
                b"baseline",
                Some(&now),
                archive.as_ref(),
                OsStr::new("occupied")
            )
            .is_err()
        );
        assert_eq!(std::fs::read(root_path.join("notes"))?, b"concurrent");
        assert_eq!(
            std::fs::read(archive_path.join("occupied"))?,
            b"old archive"
        );
        Ok(())
    }
}

#[cfg(test)]
mod custody_integration_tests {
    use crate::git_evidence::{Git, GitEvidenceError};
    use crate::quarantine::{InstructionClass, ProtectedPath, QuarantineOutcome};
    use crate::repo::RepoPath;
    use crate::utility::tests::Fixture;
    use provenant_core::{AttemptId, CancelToken, Clock, RunId, WaitBudget};
    use provenant_supervisor::clock::SystemClock;
    use provenant_supervisor::custody::{Recovery, RecoveryState, Scope, WriterBinding};
    use provenant_supervisor::fs::Directory;
    use std::error::Error;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn seed_protected(fixture: &Fixture) -> Result<(), Box<dyn Error>> {
        fs::create_dir(fixture.root.join(".agents"))?;
        fs::write(fixture.root.join(".agents/a.md"), b"baseline ordinary\n")?;
        fs::write(fixture.root.join("AGENTS.md"), b"baseline policy\n")?;
        fs::write(fixture.root.join("unrelated.txt"), b"baseline unrelated\n")?;
        fixture.run(&["add", "--", ".agents/a.md", "AGENTS.md", "unrelated.txt"])?;
        fixture.run(&["commit", "-m", "protected baseline"])?;
        Ok(())
    }

    fn scope() -> Scope {
        Scope {
            run: RunId::from_bytes([0x51; 16]),
            attempt: AttemptId::from_bytes([0xA2; 16]),
            generation: 1,
        }
    }

    fn acquire_absent_recovery(
        fixture: &Fixture,
        scope: Scope,
        base: provenant_core::GitOid,
    ) -> Result<Recovery, Box<dyn Error>> {
        let state_path = fixture._temp.path().join("writer-state");
        fs::create_dir(&state_path)?;
        fs::set_permissions(&state_path, fs::Permissions::from_mode(0o700))?;
        let state = Directory::open_anchor(&state_path)?;
        let worktree = Directory::open_anchor(&fixture.root)?;
        let git_dir = Directory::open_anchor(fixture.repo.private_git_dir())?;
        let binding =
            WriterBinding::from_verified(&worktree, &git_dir, "refs/heads/evidence-fixture", base)?;
        let clock = SystemClock::new();
        let budget = WaitBudget::cli(clock.mono_now(), 1)?;
        let recovery = provenant_supervisor::fixture::acquire_writer_with_binding(
            state,
            scope,
            binding,
            budget,
            &CancelToken::new(),
            &clock,
        )?
        .into_recovery()?;
        if recovery.observe() != RecoveryState::Absent {
            return Err("never-launched writer was not classified absent".into());
        }
        Ok(recovery)
    }

    fn archive_dir(fixture: &Fixture, name: &str) -> Result<Directory, Box<dyn Error>> {
        let path = fixture._temp.path().join(name);
        fs::create_dir(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        Ok(Directory::open_anchor(&path)?)
    }

    fn protected() -> Result<Vec<ProtectedPath>, Box<dyn Error>> {
        Ok(vec![
            ProtectedPath::ordinary(".agents/a.md")?,
            ProtectedPath::policy_control("AGENTS.md")?,
        ])
    }

    #[test]
    fn quarantine_archives_then_restores_only_attributable_ordinary_edit()
    -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        seed_protected(&fixture)?;
        let base = fixture.head()?;
        let token = CancelToken::new();
        let budget = fixture.budget()?;
        let git = Git::new(
            &fixture.executor,
            &fixture.repo,
            &fixture.runtime,
            budget,
            &token,
        );
        let scope = scope();
        let snapshot = git.snapshot_instructions(scope, &protected()?)?;
        let recovery = acquire_absent_recovery(&fixture, scope, base)?;

        fs::write(
            fixture.root.join(".agents/a.md"),
            b"attempt ordinary edit\n",
        )?;
        fs::write(
            fixture.root.join("unrelated.txt"),
            b"unrelated edit retained\n",
        )?;
        let archive = archive_dir(&fixture, "ordinary-archive")?;
        let outcome = git.quarantine_instructions(&recovery, &snapshot, &archive)?;

        match outcome {
            QuarantineOutcome::Restored { paths, .. } => assert_eq!(paths.len(), 1),
            QuarantineOutcome::ReviewRequired { .. } => {
                return Err("ordinary disk edit unexpectedly required review".into());
            }
        }
        assert_eq!(
            fs::read(fixture.root.join(".agents/a.md"))?,
            b"baseline ordinary\n"
        );
        assert_eq!(
            fs::read(fixture.root.join("unrelated.txt"))?,
            b"unrelated edit retained\n"
        );
        let archive_path = fixture._temp.path().join("ordinary-archive");
        assert!(archive_path.join("evidence.json").is_file());
        assert!(archive_path.join("quarantine-result.json").is_file());
        assert_eq!(
            fs::read(archive_path.join("blobs/00000000.before"))?,
            b"baseline ordinary\n"
        );
        assert_eq!(
            fs::read(archive_path.join("blobs/00000000.after"))?,
            b"attempt ordinary edit\n"
        );
        Ok(())
    }

    #[test]
    fn staged_ordinary_instruction_edit_is_archived_and_retained_for_review()
    -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        seed_protected(&fixture)?;
        let base = fixture.head()?;
        let token = CancelToken::new();
        let git = Git::new(
            &fixture.executor,
            &fixture.repo,
            &fixture.runtime,
            fixture.budget()?,
            &token,
        );
        let scope = scope();
        let snapshot = git.snapshot_instructions(scope, &protected()?)?;
        let recovery = acquire_absent_recovery(&fixture, scope, base)?;
        fs::write(fixture.root.join(".agents/a.md"), b"staged ordinary edit\n")?;
        fixture.run(&["add", "--", ".agents/a.md"])?;
        let archive = archive_dir(&fixture, "staged-archive")?;

        let outcome = git.quarantine_instructions(&recovery, &snapshot, &archive)?;

        assert!(matches!(outcome, QuarantineOutcome::ReviewRequired { .. }));
        assert_eq!(
            fs::read(fixture.root.join(".agents/a.md"))?,
            b"staged ordinary edit\n"
        );
        let evidence = fs::read(fixture._temp.path().join("staged-archive/evidence.json"))?;
        assert!(
            evidence
                .windows(b"\"review_required\":true".len())
                .any(|window| window == b"\"review_required\":true")
        );
        assert!(
            fixture
                ._temp
                .path()
                .join("staged-archive/protected.index.patch")
                .is_file()
        );
        Ok(())
    }

    #[test]
    fn policy_control_edit_is_archived_and_retained_for_review() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        seed_protected(&fixture)?;
        let base = fixture.head()?;
        let token = CancelToken::new();
        let git = Git::new(
            &fixture.executor,
            &fixture.repo,
            &fixture.runtime,
            fixture.budget()?,
            &token,
        );
        let scope = scope();
        let snapshot = git.snapshot_instructions(scope, &protected()?)?;
        let recovery = acquire_absent_recovery(&fixture, scope, base)?;
        fs::write(fixture.root.join("AGENTS.md"), b"changed policy\n")?;
        let archive = archive_dir(&fixture, "policy-archive")?;

        let outcome = git.quarantine_instructions(&recovery, &snapshot, &archive)?;

        assert!(matches!(outcome, QuarantineOutcome::ReviewRequired { .. }));
        assert_eq!(
            fs::read(fixture.root.join("AGENTS.md"))?,
            b"changed policy\n"
        );
        assert!(
            fixture
                ._temp
                .path()
                .join("policy-archive/evidence.json")
                .is_file()
        );
        assert!(
            fixture
                ._temp
                .path()
                .join("policy-archive/quarantine-result.json")
                .is_file()
        );
        Ok(())
    }

    #[test]
    fn archive_failure_prevents_ordinary_instruction_restoration() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        seed_protected(&fixture)?;
        let base = fixture.head()?;
        let token = CancelToken::new();
        let git = Git::new(
            &fixture.executor,
            &fixture.repo,
            &fixture.runtime,
            fixture.budget()?,
            &token,
        );
        let scope = scope();
        let snapshot = git.snapshot_instructions(scope, &protected()?)?;
        let recovery = acquire_absent_recovery(&fixture, scope, base)?;
        fs::write(
            fixture.root.join(".agents/a.md"),
            b"must remain on archive failure\n",
        )?;
        let archive = archive_dir(&fixture, "failed-archive")?;
        fs::write(
            fixture._temp.path().join("failed-archive/git-status.z"),
            b"pre-existing collision",
        )?;

        assert!(
            git.quarantine_instructions(&recovery, &snapshot, &archive)
                .is_err()
        );
        assert_eq!(
            fs::read(fixture.root.join(".agents/a.md"))?,
            b"must remain on archive failure\n"
        );
        Ok(())
    }

    #[test]
    fn selected_untracked_path_is_refused_and_tip_movement_is_not_verified()
    -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        seed_protected(&fixture)?;
        let base = fixture.head()?;
        let token = CancelToken::new();
        let git = Git::new(
            &fixture.executor,
            &fixture.repo,
            &fixture.runtime,
            fixture.budget()?,
            &token,
        );
        fs::write(fixture.root.join(".agents/untracked.md"), b"untracked\n")?;
        let untracked = RepoPath::new(".agents/untracked.md")?;
        assert!(matches!(
            git.evidence(base, &[untracked]),
            Err(GitEvidenceError::UntrackedOrUnsupportedPath)
                | Err(GitEvidenceError::SelectedUntracked)
        ));

        fs::write(fixture.root.join("later.txt"), b"advance the tip\n")?;
        fixture.run(&["add", "--", "later.txt"])?;
        fixture.run(&["commit", "-m", "advance fixture tip"])?;
        assert!(matches!(
            git.verify_tip(base),
            Err(GitEvidenceError::ConcurrentChange)
        ));
        Ok(())
    }

    #[test]
    fn ordinary_and_control_classification_is_explicit_in_the_real_snapshot()
    -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        seed_protected(&fixture)?;
        let base = fixture.head()?;
        let token = CancelToken::new();
        let git = Git::new(
            &fixture.executor,
            &fixture.repo,
            &fixture.runtime,
            fixture.budget()?,
            &token,
        );
        let snapshot = git.snapshot_instructions(scope(), &protected()?)?;
        assert_eq!(snapshot.scope().generation, 1);
        assert_eq!(snapshot.head(), base);
        assert_eq!(
            ProtectedPath::ordinary(".agents/a.md")?.class(),
            InstructionClass::OrdinaryInstruction
        );
        assert_eq!(
            ProtectedPath::policy_control("AGENTS.md")?.class(),
            InstructionClass::PolicyControl
        );
        Ok(())
    }
}
