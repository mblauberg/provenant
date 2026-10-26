//! Protected instruction snapshots and post-quiescence quarantine.
use crate::git_evidence::{
    FileSnapshot, Git, GitEvidenceError, IndexFingerprint, MAX_DISCLOSURE_BYTES,
    MAX_DISCLOSURE_FILES, TrackedFingerprint, canonical_paths, open_parent, read_index_fingerprint,
    read_optional_protected_entry, read_protected_entry, read_tracked_file, refuse_index_lock,
};
use crate::plan::read_only::RESERVED;
use crate::repo::RepoPath;
use crate::status::{
    StatusError, StatusKind, StatusPath, parse_index_entries, parse_porcelain_v2_z,
};
use crate::utility::{GitObjectKind, GitRead};
use provenant_core::{GitOid, RelativePath};
use provenant_records::{CanonicalV1, FileDigest, PathWire};
use provenant_supervisor::custody::{Recovery, RecoveryState, Scope};
use provenant_supervisor::fs::{FileKind, FsError, Metadata, Publication, PublishMode, SafeDir};
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

const EVIDENCE_MANIFEST_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Discover the complete tracked instruction/control scope. New protected
    /// paths are also checked by quarantine, including committed additions.
    pub fn writer_instruction_paths(&self) -> Result<Vec<ProtectedPath>, QuarantineError> {
        let raw = self
            .read(GitRead::IndexEntries { paths: Vec::new() })?
            .stdout
            .bytes;
        let mut paths = Vec::new();
        for entry in parse_index_entries(&raw)? {
            let entry = entry?;
            if !is_protected_evidence_path(entry.path) {
                continue;
            }
            let path = Path::new(OsStr::from_bytes(entry.path));
            paths.push(if under_agents(path) && !is_known_control(path) {
                ProtectedPath::ordinary(path)?
            } else {
                ProtectedPath::policy_control(path)?
            });
        }
        if paths.len() > MAX_DISCLOSURE_FILES {
            return Err(QuarantineError::Incomplete);
        }
        Ok(paths)
    }

    /// Reopen only a prelaunch snapshot in the provider-inaccessible archive.
    /// Scope, base and archive identity are checked against held custody.
    pub fn recover_instruction_snapshot(
        &self,
        recovery: &Recovery,
        base: GitOid,
        archive: &dyn SafeDir,
    ) -> Result<InstructionSnapshot, QuarantineError> {
        let bytes = read_archive(
            archive,
            OsStr::new("snapshot.json"),
            EVIDENCE_MANIFEST_LIMIT as u64,
        )?;
        let record: SnapshotRecord = serde_json::from_slice(&bytes)?;
        let identity = archive.metadata()?;
        validate_archive(self.repo.root_identity().device, &identity)?;
        if record.schema_version != 1
            || record.scope != recovery.scope()
            || record.head != base
            || record.archive_device != identity.device
            || record.archive_inode != identity.inode
            || record.files.len() > MAX_DISCLOSURE_FILES
        {
            return Err(QuarantineError::ArchiveBinding);
        }
        let baseline = archive.open_verified_directory(OsStr::new("baseline"))?;
        let mut files = Vec::new();
        let mut total = 0;
        for (index, file) in record.files.into_iter().enumerate() {
            let path = file
                .fingerprint
                .path
                .to_relative_path()
                .map_err(|_| QuarantineError::InvalidPath)?;
            let protected = match file.class {
                InstructionClass::OrdinaryInstruction => ProtectedPath::ordinary(path.as_path())?,
                InstructionClass::PolicyControl => ProtectedPath::policy_control(path.as_path())?,
            };
            let bytes = read_archive(
                baseline.as_ref(),
                OsStr::new(&format!("{index:08}")),
                MAX_DISCLOSURE_BYTES,
            )?;
            total += bytes.len() as u64;
            if total > MAX_DISCLOSURE_BYTES
                || file.fingerprint.digest != Some(CanonicalV1::file_digest(&bytes))
            {
                return Err(QuarantineError::Incomplete);
            }
            files.push(InitialFile {
                protected,
                bytes,
                fingerprint: file.fingerprint,
            });
        }
        let index_records =
            read_archive(archive, OsStr::new("start-index.z"), MAX_DISCLOSURE_BYTES)?;
        if CanonicalV1::file_digest(&index_records) != record.index_records_digest {
            return Err(QuarantineError::Incomplete);
        }
        Ok(InstructionSnapshot {
            scope: record.scope,
            head: record.head,
            index: record.index,
            index_records,
            files,
            archive_identity: identity,
        })
    }
    pub fn snapshot_instructions(
        &self,
        scope: Scope,
        protected: &[ProtectedPath],
        quarantine_dir: &dyn SafeDir,
    ) -> Result<InstructionSnapshot, QuarantineError> {
        capture_instruction_snapshot(self, scope, protected, quarantine_dir)
    }

    /// Attribute committed protected changes to the lane's delivery diff.
    pub fn quarantine_instructions(
        &self,
        recovery: &Recovery,
        snapshot: &InstructionSnapshot,
        comparison_base: GitOid,
        quarantine_dir: &dyn SafeDir,
    ) -> Result<QuarantineOutcome, QuarantineError> {
        quarantine_instruction_changes(self, recovery, snapshot, comparison_base, quarantine_dir)
    }

    fn protected_changed_paths(
        &self,
        base: GitOid,
        head: GitOid,
    ) -> Result<Vec<RepoPath>, QuarantineError> {
        let raw = self
            .read(GitRead::ChangedPaths { base, head })?
            .stdout
            .bytes;
        let mut paths = Vec::new();
        for path in raw.split(|byte| *byte == 0).filter(|path| !path.is_empty()) {
            if is_protected_evidence_path(path) {
                paths.push(
                    RepoPath::new(Path::new(OsStr::from_bytes(path)))
                        .map_err(|_| QuarantineError::InvalidPath)?,
                );
            }
        }
        canonical_paths(&paths).map_err(QuarantineError::from)
    }

    fn protected_status_paths(&self) -> Result<Vec<RepoPath>, QuarantineError> {
        let status = self.read(GitRead::StatusIncludingIgnored)?;
        let mut paths = Vec::new();
        for record in parse_porcelain_v2_z(&status.stdout.bytes)? {
            for path in std::iter::once(&record.path).chain(record.original_path.iter()) {
                if is_protected_evidence_path(path) {
                    paths.push(
                        RepoPath::new(Path::new(OsStr::from_bytes(path)))
                            .map_err(|_| QuarantineError::InvalidPath)?,
                    );
                }
            }
        }
        canonical_paths(&paths).map_err(QuarantineError::from)
    }
}

#[derive(Debug, Error)]
pub enum QuarantineError {
    #[error("protected path or Git record is invalid")]
    InvalidPath,
    #[error("quarantine must be a private same-device directory bound before launch")]
    ArchiveBinding,
    #[error("protected path is not a tracked ordinary file or symlink")]
    NotTracked,
    #[error("quarantine requires the matching, confirmed quiescence receipt")]
    QuiescenceUnconfirmed,
    #[error("protected evidence changed concurrently; review is required")]
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
    oid: GitOid,
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
    archive_identity: Metadata,
}
impl InstructionSnapshot {
    /// Seal the baseline before handoff. The archive is excluded from provider
    /// grants and retained for terminal recovery; no caller can deserialize this
    /// authority-bearing type.
    pub fn persist(&self, archive: &dyn SafeDir) -> Result<(), QuarantineError> {
        if !archive.metadata()?.same_identity(&self.archive_identity) {
            return Err(QuarantineError::ArchiveBinding);
        }
        archive.mkdir(OsStr::new("baseline"), 0o700)?;
        let baseline = archive.open_verified_directory(OsStr::new("baseline"))?;
        for (index, file) in self.files.iter().enumerate() {
            put(
                baseline.as_ref(),
                OsStr::new(&format!("{index:08}")),
                &file.bytes,
            )?;
        }
        put(archive, OsStr::new("start-index.z"), &self.index_records)?;
        let record = SnapshotRecord {
            schema_version: 1,
            scope: self.scope,
            head: self.head,
            index: self.index,
            index_records_digest: CanonicalV1::file_digest(&self.index_records),
            archive_device: self.archive_identity.device,
            archive_inode: self.archive_identity.inode,
            files: self
                .files
                .iter()
                .map(|file| SnapshotFile {
                    class: file.protected.class,
                    fingerprint: file.fingerprint.clone(),
                })
                .collect(),
        };
        let bytes = serde_json::to_vec(&record)?;
        if bytes.len() > EVIDENCE_MANIFEST_LIMIT {
            return Err(QuarantineError::ManifestTooLarge);
        }
        put(archive, OsStr::new("snapshot.json"), &bytes)
    }
    pub fn head(&self) -> GitOid {
        self.head
    }
    pub fn scope(&self) -> Scope {
        self.scope
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotFile {
    class: InstructionClass,
    fingerprint: TrackedFingerprint,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotRecord {
    schema_version: u64,
    scope: Scope,
    head: GitOid,
    index: IndexFingerprint,
    index_records_digest: FileDigest,
    archive_device: u64,
    archive_inode: u64,
    files: Vec<SnapshotFile>,
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
    comparison_base: GitOid,
    observed_head: GitOid,
    start_index: IndexFingerprint,
    observed_index: IndexFingerprint,
    status_digest: FileDigest,
    commit_diff_digest: FileDigest,
    index_diff_digest: FileDigest,
    worktree_diff_digest: FileDigest,
    paths: Vec<ArchivedPath>,
    unaccounted_paths: Vec<ArchivedUnaccountedPath>,
    review_required: bool,
    review_paths: Vec<PathWire>,
    interrupted_archives: Vec<String>,
    base_is_ancestor: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuarantineOutcome {
    Restored {
        paths: Vec<PathWire>,
        evidence_digest: FileDigest,
    },
    ReviewRequired {
        evidence_digest: FileDigest,
        paths: Vec<PathWire>,
    },
}

/// Captures initial protected Git and disk state before launch. Callers provide
/// the trusted path scope and their explicit ordinary/control classification.
fn capture_instruction_snapshot(
    git: &Git<'_>,
    scope: Scope,
    protected: &[ProtectedPath],
    quarantine_dir: &dyn SafeDir,
) -> Result<InstructionSnapshot, QuarantineError> {
    // Bind restoration custody before any provider may be launched. Cross-device
    // rename has no atomic preservation primitive; refuse rather than copy/unlink.
    let archive_identity = quarantine_dir.metadata()?;
    validate_archive(git.repo.root_identity().device, &archive_identity)?;
    let repo = git.repo;
    if protected.len() > MAX_DISCLOSURE_FILES {
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
    let head = git.head()?;
    refuse_index_lock(repo.private_dir())?;
    let index = read_index_fingerprint(repo.private_dir())?;
    let mut tree = Vec::new();
    for item in &protected {
        let tree_raw = git
            .read(GitRead::TreeEntries {
                oid: head,
                prefix: item.path.clone(),
            })?
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
    let index_records = if paths.is_empty() {
        Vec::new()
    } else {
        git.read(GitRead::IndexEntries {
            paths: paths.clone(),
        })?
        .stdout
        .bytes
    };
    let index_by_path = parse_index_entries(&index_records)?.collect::<Result<Vec<_>, _>>()?;
    if index_by_path.len() > MAX_DISCLOSURE_FILES {
        return Err(QuarantineError::Incomplete);
    }
    let mut total = 0_u64;
    let mut files = Vec::with_capacity(protected.len());
    for path in protected {
        let tree_entry = tree
            .iter()
            .find(|entry| path_bytes(&entry.path) == path_bytes(&path.path))
            .ok_or(QuarantineError::NotTracked)?;
        let tracked_link = match tree_entry.mode.as_str() {
            "100644" | "100755" => false,
            "120000" => true,
            _ => return Err(QuarantineError::NotTracked),
        };
        let index_matches = index_by_path
            .iter()
            .filter(|entry| entry.path == path_bytes(&path.path))
            .collect::<Vec<_>>();
        if index_matches.len() != 1 {
            return Err(QuarantineError::NotTracked);
        }
        let index_entry = index_matches.first().ok_or(QuarantineError::NotTracked)?;
        let index_mode_ok = if tracked_link {
            index_entry.mode == 0o120000
        } else {
            matches!(index_entry.mode, 0o100644 | 0o100755)
        };
        if index_entry.stage != 0 || !index_mode_ok {
            return Err(QuarantineError::NotTracked);
        }
        let file = read_protected_entry(repo.root_dir(), &path.path, MAX_DISCLOSURE_BYTES)?;
        if file.fingerprint.device != Some(repo.root_identity().device)
            || file.fingerprint.links != Some(1)
            || is_link_mode(file.fingerprint.mode) != tracked_link
        {
            return Err(QuarantineError::NotTracked);
        }
        if tracked_link {
            // Evidence is the link target; it must equal the committed blob.
            let blob = git
                .read(GitRead::RawObject {
                    kind: GitObjectKind::Blob,
                    oid: tree_entry.oid,
                })?
                .stdout
                .bytes;
            if blob != file.bytes {
                return Err(QuarantineError::NotTracked);
            }
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
    let head_after = git.head()?;
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
        archive_identity,
    })
}

fn validate_archive(device: u64, metadata: &Metadata) -> Result<(), QuarantineError> {
    if metadata.kind != FileKind::Directory
        || metadata.device != device
        || metadata.uid != provenant_supervisor::fs::effective_uid()
        || metadata.mode & 0o077 != 0
    {
        return Err(QuarantineError::ArchiveBinding);
    }
    Ok(())
}

/// Archive status, pinned HEAD/index/worktree patches and tracked blobs before
/// restoring any attributable ordinary instruction file. The opaque Recovery
/// borrow holds supervisor's writer exclusion for the whole operation.
fn quarantine_instruction_changes(
    git: &Git<'_>,
    recovery: &Recovery,
    snapshot: &InstructionSnapshot,
    comparison_base: GitOid,
    quarantine_dir: &dyn SafeDir,
) -> Result<QuarantineOutcome, QuarantineError> {
    let repo = git.repo;
    let archive_identity = quarantine_dir.metadata()?;
    validate_archive(repo.root_identity().device, &archive_identity)?;
    if !snapshot.archive_identity.same_identity(&archive_identity) {
        return Err(QuarantineError::ArchiveBinding);
    }
    if recovery.scope() != snapshot.scope || recovery.observe() != RecoveryState::Absent {
        return Err(QuarantineError::QuiescenceUnconfirmed);
    }
    match read_archive(
        quarantine_dir,
        OsStr::new("quarantine-result.json"),
        EVIDENCE_MANIFEST_LIMIT as u64,
    ) {
        Ok(bytes) => {
            let result: QuarantineResultRecord = serde_json::from_slice(&bytes)?;
            let evidence = read_archive(
                quarantine_dir,
                OsStr::new("evidence.json"),
                EVIDENCE_MANIFEST_LIMIT as u64,
            )?;
            if result.schema_version != 1
                || result.scope != snapshot.scope
                || result.kind != "instruction_quarantine_result"
                || result.evidence_digest != CanonicalV1::file_digest(&evidence)
            {
                return Err(QuarantineError::Incomplete);
            }
            return Ok(if result.review_required {
                QuarantineOutcome::ReviewRequired {
                    evidence_digest: result.evidence_digest,
                    paths: result.review_paths,
                }
            } else {
                QuarantineOutcome::Restored {
                    paths: result.restored,
                    evidence_digest: result.evidence_digest,
                }
            });
        }
        Err(QuarantineError::Filesystem(error)) if error.is_not_found() => {}
        Err(error) => return Err(error),
    }
    // Evidence is sealed before any restoration. If a prior holder died
    // afterwards, retain the complete archive and require review rather than
    // replaying a partly completed restore or certifying changed fingerprints.
    match read_archive(
        quarantine_dir,
        OsStr::new("evidence.json"),
        EVIDENCE_MANIFEST_LIMIT as u64,
    ) {
        Ok(bytes) => {
            #[derive(Deserialize)]
            struct Header {
                schema_version: u64,
                scope: Scope,
                kind: String,
                start_head: GitOid,
                review_paths: Vec<PathWire>,
            }
            let header: Header = serde_json::from_slice(&bytes)?;
            if header.schema_version != 1
                || header.scope != snapshot.scope
                || header.kind != "instruction_quarantine_evidence"
                || header.start_head != snapshot.head
            {
                return Err(QuarantineError::Incomplete);
            }
            let evidence_digest = CanonicalV1::file_digest(&bytes);
            let result = QuarantineOutcome::ReviewRequired {
                evidence_digest,
                paths: header.review_paths,
            };
            write_result(quarantine_dir, snapshot.scope, evidence_digest, &result)?;
            return Ok(result);
        }
        Err(QuarantineError::Filesystem(error)) if error.is_not_found() => {}
        Err(error) => return Err(error),
    }
    let interrupted_archives = retain_partial_archive(quarantine_dir)?;
    repo.revalidate()
        .map_err(|_| QuarantineError::ConcurrentChange)?;
    refuse_index_lock(repo.private_dir())?;
    let index_before = read_index_fingerprint(repo.private_dir())?;
    let head_before = git.head()?;
    let status = git.read(GitRead::StatusIncludingIgnored)?;
    let mut status_paths = parse_porcelain_v2_z(&status.stdout.bytes)?;
    let own_paths = git.protected_changed_paths(comparison_base, head_before)?;
    let mut review_paths = own_paths.clone();
    review_paths.extend(git.protected_status_paths()?);
    let upstream_paths = git.protected_changed_paths(snapshot.head, comparison_base)?;
    // Only clean paths whose tree matches the delivery baseline are exempt.
    // Staged, disk and conflict changes still require ordinary quarantine.
    let mut clean_upstream_paths = Vec::new();
    let mut upstream_review = false;
    for path in upstream_paths {
        if !own_paths.contains(&path)
            && !status_paths.iter().any(|record| {
                record.path == path_bytes(&path)
                    || record.original_path.as_deref() == Some(path_bytes(&path))
            })
            && matches_tree(git, comparison_base, &path)?
        {
            clean_upstream_paths.push(path);
        } else {
            upstream_review = true;
            review_paths.push(path);
        }
    }
    let upstream_paths = clean_upstream_paths;
    let mut protected_gitlinks = 0;
    for entry in parse_index_entries(
        &git.read(GitRead::IndexEntries { paths: Vec::new() })?
            .stdout
            .bytes,
    )? {
        let entry = entry?;
        if is_protected_evidence_path(entry.path)
            && !upstream_paths
                .iter()
                .any(|path| path_bytes(path) == entry.path)
            && (entry.mode == 0o160000
                || !snapshot
                    .files
                    .iter()
                    .any(|file| path_bytes(&file.protected.path) == entry.path))
            && !status_paths.iter().any(|record| record.path == entry.path)
        {
            protected_gitlinks += 1;
            if protected_gitlinks > MAX_DISCLOSURE_FILES {
                return Err(QuarantineError::Incomplete);
            }
            status_paths.push(StatusPath {
                path: entry.path.to_vec(),
                kind: StatusKind::Unknown,
                original_path: None,
            });
        }
    }
    let selected = snapshot
        .files
        .iter()
        .map(|file| file.protected.path.clone())
        .collect::<Vec<_>>();
    let mut evidence_paths = selected.clone();
    evidence_paths.extend(own_paths.iter().cloned());
    let ancestry = crate::run_utility(
        git.executor,
        GitRead::MergeBaseIsAncestor {
            ancestor: snapshot.head,
            tip: head_before,
        },
        repo,
        git.runtime,
        git.budget,
        git.cancel,
    )
    .map_err(GitEvidenceError::from)?;
    let base_is_ancestor = match ancestry.termination {
        provenant_supervisor::exec::Termination::Exit(0) => true,
        provenant_supervisor::exec::Termination::Exit(1) => false,
        _ => return Err(QuarantineError::Incomplete),
    };
    let mut review = !interrupted_archives.is_empty() || !base_is_ancestor || upstream_review;
    let mut current = Vec::with_capacity(snapshot.files.len());
    let mut current_unreadable = Vec::with_capacity(snapshot.files.len());
    let mut total = 0_u64;
    for file in &snapshot.files {
        let observed = read_optional_protected_entry(
            repo.root_dir(),
            &file.protected.path,
            MAX_DISCLOSURE_BYTES,
        );
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
                if !upstream_paths.contains(&file.protected.path)
                    && file.protected.class == InstructionClass::PolicyControl
                    && (status_changed || content_changed || fingerprint_changed)
                {
                    review = true;
                    review_paths.push(file.protected.path.clone());
                }
                if !upstream_paths.contains(&file.protected.path)
                    && file.protected.class == InstructionClass::OrdinaryInstruction
                    && fingerprint_changed
                    && !content_changed
                {
                    review = true;
                    review_paths.push(file.protected.path.clone());
                }
                current.push(observed);
                current_unreadable.push(false);
            }
            Err(_) => {
                review = true;
                review_paths.push(file.protected.path.clone());
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
            let path = RepoPath::new(PathBuf::from(OsString::from_vec(raw_path.clone())))
                .map_err(|_| QuarantineError::InvalidPath)?;
            if !evidence_paths.contains(&path) {
                if evidence_paths.len() == MAX_DISCLOSURE_FILES {
                    return Err(QuarantineError::Incomplete);
                }
                evidence_paths.push(path.clone());
            }
            let selected_path = selected
                .iter()
                .any(|path| path_bytes(path) == raw_path.as_slice());
            if !selected_path {
                review = true;
                let (observed, unreadable) = match read_optional_protected_entry(
                    repo.root_dir(),
                    &path,
                    MAX_DISCLOSURE_BYTES,
                ) {
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
                unaccounted_snapshots.push(UnaccountedSnapshot {
                    path,
                    status: record.kind,
                    current: observed,
                    unreadable,
                });
            }
        }
        if matches!(
            record.kind,
            StatusKind::Conflict | StatusKind::RenameOrCopy | StatusKind::Unknown
        ) {
            review = true;
        }
    }
    let evidence_paths = canonical_paths(&evidence_paths)?;
    if evidence_paths.len() > MAX_DISCLOSURE_FILES {
        return Err(QuarantineError::Incomplete);
    }
    let (commit_diff, index_diff, worktree_diff) = if evidence_paths.is_empty() {
        (Vec::new(), Vec::new(), Vec::new())
    } else {
        (
            git.read(GitRead::Diff {
                base: comparison_base,
                head: head_before,
                paths: evidence_paths.clone(),
            })?
            .stdout
            .bytes,
            git.read(GitRead::DiffIndex {
                paths: evidence_paths.clone(),
            })?
            .stdout
            .bytes,
            git.read(GitRead::DiffWorktree {
                paths: evidence_paths.clone(),
            })?
            .stdout
            .bytes,
        )
    };
    if !commit_diff.is_empty() {
        review = true;
    }
    let index_after_observation = read_index_fingerprint(repo.private_dir())?;
    refuse_index_lock(repo.private_dir())?;
    let head_after_observation = git.head()?;
    let status_after = git.read(GitRead::StatusIncludingIgnored)?;
    if index_before != index_after_observation
        || head_before != head_after_observation
        || status.stdout.bytes != status_after.stdout.bytes
    {
        return Err(QuarantineError::ConcurrentChange);
    }
    if !index_diff.is_empty() {
        review = true;
    }
    repo.revalidate()
        .map_err(|_| QuarantineError::ConcurrentChange)?;

    let mut manifest_paths = Vec::new();
    let mut manifest_unaccounted = Vec::new();
    let mut archive_bytes = 0;
    for (name, bytes) in [
        ("git-status.z", status.stdout.bytes.as_slice()),
        ("protected.head.patch", &commit_diff),
        ("protected.index.patch", &index_diff),
        ("protected.disk.patch", &worktree_diff),
    ] {
        put_bounded(quarantine_dir, OsStr::new(name), bytes, &mut archive_bytes)?;
    }
    match quarantine_dir.mkdir(OsStr::new("blobs"), 0o700) {
        Ok(()) => {}
        Err(error) if error.is_exists() => {}
        Err(error) => return Err(error.into()),
    }
    let blobs = quarantine_dir.open_verified_directory(OsStr::new("blobs"))?;
    for (index, file) in snapshot.files.iter().enumerate() {
        let prefix = format!("{index:08}");
        let before_name = format!("{prefix}.before");
        let after_name = format!("{prefix}.after");
        put_bounded(
            blobs.as_ref(),
            OsStr::new(&before_name),
            &file.bytes,
            &mut archive_bytes,
        )?;
        let observed = current.get(index).and_then(Option::as_ref);
        if let Some(observed) = observed {
            put_bounded(
                blobs.as_ref(),
                OsStr::new(&after_name),
                &observed.bytes,
                &mut archive_bytes,
            )?;
        }
        manifest_paths.push(ArchivedPath {
            path: wire_path(&file.protected.path)?,
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
        if !upstream_paths.contains(&file.protected.path)
            && file.protected.class == InstructionClass::PolicyControl
            && changed
        {
            review = true;
        }
        if !upstream_paths.contains(&file.protected.path)
            && file.protected.class == InstructionClass::OrdinaryInstruction
            && changed
        {
            let has_simple_status = status_paths.iter().any(|record| {
                record.path.as_slice() == path_bytes(&file.protected.path)
                    && record.kind == StatusKind::Changed
            });
            // Restoring a link would need a symlink write; leave it for review.
            let involves_link = is_link_mode(file.fingerprint.mode)
                || observed.is_some_and(|item| is_link_mode(item.fingerprint.mode));
            if !has_simple_status || involves_link {
                review = true;
                review_paths.push(file.protected.path.clone());
            }
        }
    }
    for (index, item) in unaccounted_snapshots.iter().enumerate() {
        let current_blob = if let Some(current) = item.current.as_ref() {
            let blob_name = format!("unaccounted-{index:08}.current");
            put_bounded(
                blobs.as_ref(),
                OsStr::new(&blob_name),
                &current.bytes,
                &mut archive_bytes,
            )?;
            Some(blob_name)
        } else {
            None
        };
        manifest_unaccounted.push(ArchivedUnaccountedPath {
            path: wire_path(&item.path)?,
            status: item.status,
            current: item
                .current
                .as_ref()
                .map(|current| current.fingerprint.clone()),
            current_blob,
            unreadable: item.unreadable,
        });
    }
    let review_paths: Vec<_> = canonical_paths(&review_paths)?
        .iter()
        .map(wire_path)
        .collect::<Result<_, _>>()?;
    let evidence = EvidenceManifest {
        schema_version: 1,
        scope: snapshot.scope,
        kind: "instruction_quarantine_evidence",
        start_head: snapshot.head,
        comparison_base,
        observed_head: head_before,
        start_index: snapshot.index,
        observed_index: index_before,
        status_digest: CanonicalV1::file_digest(&status.stdout.bytes),
        commit_diff_digest: CanonicalV1::file_digest(&commit_diff),
        index_diff_digest: CanonicalV1::file_digest(&index_diff),
        worktree_diff_digest: CanonicalV1::file_digest(&worktree_diff),
        paths: manifest_paths,
        unaccounted_paths: manifest_unaccounted,
        review_required: review,
        review_paths: review_paths.clone(),
        interrupted_archives,
        base_is_ancestor,
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
        let result = QuarantineOutcome::ReviewRequired {
            evidence_digest,
            paths: review_paths,
        };
        write_result(quarantine_dir, snapshot.scope, evidence_digest, &result)?;
        return Ok(result);
    }

    let mut restored_paths = Vec::new();
    for (index, file) in snapshot.files.iter().enumerate() {
        if file.protected.class != InstructionClass::OrdinaryInstruction
            || upstream_paths.contains(&file.protected.path)
        {
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
        let latest = read_optional_protected_entry(
            repo.root_dir(),
            &file.protected.path,
            MAX_DISCLOSURE_BYTES,
        )?;
        if latest.as_ref().map(|item| &item.fingerprint) != observed.map(|item| &item.fingerprint) {
            return Err(QuarantineError::ConcurrentChange);
        }
        restore_file(
            repo.root_dir(),
            &file.protected.path,
            &file.bytes,
            file.fingerprint.mode.ok_or(QuarantineError::Incomplete)? & 0o777,
            latest.as_ref(),
            quarantine_dir,
            OsStr::new(&retained_name(index)),
        )?;
        let restored =
            read_tracked_file(repo.root_dir(), &file.protected.path, MAX_DISCLOSURE_BYTES)?;
        if restored.fingerprint.digest != file.fingerprint.digest {
            return Err(QuarantineError::ConcurrentChange);
        }
        restored_paths.push(wire_path(&file.protected.path)?);
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

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct QuarantineResultRecord {
    schema_version: u64,
    scope: Scope,
    kind: String,
    evidence_digest: FileDigest,
    restored: Vec<PathWire>,
    review_required: bool,
    review_paths: Vec<PathWire>,
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
    let review_paths = match result {
        QuarantineOutcome::ReviewRequired { paths, .. } => paths.clone(),
        _ => Vec::new(),
    };
    let bytes = serde_json::to_vec(&QuarantineResultRecord {
        schema_version: 1,
        scope,
        kind: "instruction_quarantine_result".into(),
        evidence_digest,
        restored,
        review_required,
        review_paths,
    })?;
    put(directory, OsStr::new("quarantine-result.json"), &bytes)
}

fn read_archive(
    directory: &dyn SafeDir,
    name: &OsStr,
    limit: u64,
) -> Result<Vec<u8>, QuarantineError> {
    use std::io::Read;
    let file = directory.open_read(name)?;
    let metadata = file.metadata()?;
    if metadata.links != 1
        || metadata.mode & 0o077 != 0
        || metadata.uid != provenant_supervisor::fs::effective_uid()
    {
        return Err(QuarantineError::ArchiveBinding);
    }
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| QuarantineError::Incomplete)?;
    if bytes.len() as u64 > limit {
        return Err(QuarantineError::Incomplete);
    }
    Ok(bytes)
}

/// Preserve interrupted observations in their own archive before collecting
/// fresh evidence. Even unrelated status changes must not poison every retry.
fn retain_partial_archive(directory: &dyn SafeDir) -> Result<Vec<String>, QuarantineError> {
    let mut partial = Vec::new();
    let mut interrupted = Vec::new();
    for (count, name) in directory.entries()?.enumerate() {
        if count >= MAX_DISCLOSURE_FILES {
            return Err(QuarantineError::Incomplete);
        }
        let name = name?;
        if let Some(text) = name.to_str()
            && text
                .strip_prefix("interrupted-")
                .is_some_and(|id| provenant_core::AttemptId::parse(id).is_ok())
        {
            directory.open_verified_directory(&name)?;
            interrupted.push(text.to_owned());
        }
        if [
            "git-status.z",
            "protected.head.patch",
            "protected.index.patch",
            "protected.disk.patch",
            "blobs",
        ]
        .iter()
        .any(|item| name == OsStr::new(item))
        {
            partial.push(name);
        }
    }
    if !partial.is_empty() {
        let name = format!(
            "interrupted-{}",
            provenant_core::AttemptId::from_bytes(provenant_supervisor::fs::entropy_128()?)
        );
        directory.mkdir(OsStr::new(&name), 0o700)?;
        directory.sync()?;
        let archive = directory.open_verified_directory(OsStr::new(&name))?;
        for entry in partial {
            directory.rename_noreplace_to(&entry, archive.as_ref(), &entry)?;
            archive.sync()?;
            directory.sync()?;
        }
        interrupted.push(name);
    }
    interrupted.sort();
    Ok(interrupted)
}

fn put(directory: &dyn SafeDir, name: &OsStr, bytes: &[u8]) -> Result<(), QuarantineError> {
    match directory.atomic_write(name, ".quarantine-", bytes, 0o600, PublishMode::NoReplace) {
        Ok(Publication::Durable) => Ok(()),
        Ok(Publication::VisibleDurabilityUnconfirmed { .. }) => {
            Err(QuarantineError::DurabilityUnconfirmed)
        }
        Err(error) if error.is_exists() => {
            if read_archive(directory, name, bytes.len() as u64)? != bytes {
                return Err(QuarantineError::ConcurrentChange);
            }
            directory.open_read(name)?.sync()?;
            directory.sync()?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn put_bounded(
    directory: &dyn SafeDir,
    name: &OsStr,
    bytes: &[u8],
    total: &mut u64,
) -> Result<(), QuarantineError> {
    *total = (*total)
        .checked_add(u64::try_from(bytes.len()).map_err(|_| QuarantineError::Incomplete)?)
        .filter(|size| *size <= MAX_DISCLOSURE_BYTES.saturating_mul(3))
        .ok_or(QuarantineError::Incomplete)?;
    put(directory, name, bytes)
}

/// Preserve the old inode in quarantine before publishing a baseline. No
/// operation replaces a concurrently created destination. The retained name is
/// recorded before mutation and never unlinked: an open writer can still use it.
fn is_link_mode(mode: Option<u32>) -> bool {
    mode.is_some_and(|mode| mode & 0o170_000 == 0o120_000)
}

fn retained_name(index: usize) -> String {
    format!("{index:08}.retained")
}

fn restore_file(
    root: &dyn SafeDir,
    path: &RepoPath,
    bytes: &[u8],
    permissions: u32,
    observed: Option<&FileSnapshot>,
    archive: &dyn SafeDir,
    retained: &OsStr,
) -> Result<(), QuarantineError> {
    let (parent, leaf) = open_parent(root, path).map_err(|error| match error {
        GitEvidenceError::InvalidPath => QuarantineError::InvalidPath,
        GitEvidenceError::Filesystem(error) => QuarantineError::Filesystem(error),
        error => QuarantineError::Git(error),
    })?;
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
        permissions,
        PublishMode::NoReplace,
    )? {
        Publication::Durable => {
            if let Some(observed) = observed {
                let retained_path =
                    RepoPath::new(Path::new(retained)).map_err(|_| QuarantineError::InvalidPath)?;
                let mut retained_fingerprint =
                    read_tracked_file(archive, &retained_path, MAX_DISCLOSURE_BYTES)?.fingerprint;
                retained_fingerprint.path = observed.fingerprint.path.clone();
                if retained_fingerprint != observed.fingerprint {
                    return Err(QuarantineError::ConcurrentChange);
                }
            }
            Ok(())
        }
        Publication::VisibleDurabilityUnconfirmed { .. } => {
            Err(QuarantineError::DurabilityUnconfirmed)
        }
    }
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
        let oid = GitOid::parse(oid_text).map_err(|_| QuarantineError::Incomplete)?;
        let path = RepoPath::new(PathBuf::from(OsString::from_vec(path.to_vec())))
            .map_err(|_| QuarantineError::InvalidPath)?;
        records.push(TreeRecord { path, mode, oid });
        if records.len() > MAX_DISCLOSURE_FILES {
            return Err(QuarantineError::Incomplete);
        }
    }
    Ok(records)
}

fn split_once(bytes: &[u8], separator: u8) -> Option<(&[u8], &[u8])> {
    let index = bytes.iter().position(|byte| *byte == separator)?;
    Some((bytes.get(..index)?, bytes.get(index.saturating_add(1)..)?))
}

fn path_bytes(path: &RepoPath) -> &[u8] {
    path.as_os_str().as_bytes()
}
fn matches_tree(git: &Git<'_>, base: GitOid, path: &RepoPath) -> Result<bool, QuarantineError> {
    let tree = git.read(GitRead::TreeEntries {
        oid: base,
        prefix: path.clone(),
    })?;
    let entries = parse_tree_entries(&tree.stdout.bytes)?;
    let entry = entries.iter().find(|entry| entry.path == *path);
    let observed =
        match read_optional_protected_entry(git.repo.root_dir(), path, MAX_DISCLOSURE_BYTES) {
            Ok(observed) => observed,
            Err(_) => return Ok(false),
        };
    match (entry, observed) {
        (None, None) => Ok(true),
        (Some(entry), Some(observed)) => {
            if observed.fingerprint.device != Some(git.repo.root_identity().device)
                || observed.fingerprint.links != Some(1)
            {
                return Ok(false);
            }
            let mode =
                u32::from_str_radix(&entry.mode, 8).map_err(|_| QuarantineError::Incomplete)?;
            // Git records file type and owner-executable status, not umask.
            let mask = if mode == 0o120000 { 0o170000 } else { 0o170100 };
            if observed.fingerprint.mode.map(|mode| mode & mask) != Some(mode & mask) {
                return Ok(false);
            }
            let blob = git.read(GitRead::RawObject {
                kind: GitObjectKind::Blob,
                oid: entry.oid,
            })?;
            Ok(blob.stdout.bytes == observed.bytes)
        }
        _ => Ok(false),
    }
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
fn wire_path(path: &RepoPath) -> Result<PathWire, QuarantineError> {
    let relative = RelativePath::parse(path.as_path()).map_err(|_| QuarantineError::InvalidPath)?;
    PathWire::from_relative(&relative).map_err(|_| QuarantineError::InvalidPath)
}

fn is_known_control(path: &Path) -> bool {
    // Control filenames apply at any depth, while ordinary source directories
    // called config, git or hooks carry no policy authority.
    path.components().any(|component| {
        let Component::Normal(name) = component else {
            return true;
        };
        [
            b"AGENTS.md".as_slice(),
            b"HARNESS.md",
            b"CLAUDE.md",
            b"GEMINI.md",
            b".git",
            b".gitattributes",
            b".gitmodules",
        ]
        .iter()
        .any(|control| name.as_bytes().eq_ignore_ascii_case(control))
    }) || path.components().next().is_some_and(|first| {
        // The first component is the control root; APFS folds its case. Shares
        // the read-only plan's reserved list so the two cannot drift, while
        // .agents is the instruction tree this quarantine exists to review.
        let root = first.as_os_str().as_bytes();
        RESERVED
            .iter()
            .filter(|reserved| **reserved != ".agents")
            .chain(&[".githooks", "hooks"])
            .any(|reserved| root.eq_ignore_ascii_case(reserved.as_bytes()))
    })
}

#[cfg(test)]
mod tests {
    use super::{InstructionClass, ProtectedPath, parse_tree_entries};
    use crate::status::parse_index_entries;
    use std::error::Error;

    #[test]
    fn provider_control_roots_are_protected_case_insensitively() {
        for path in [
            ".Claude/settings.json",
            ".mcp.json",
            ".VSCode/tasks.json",
            ".GitHooks/pre-commit",
            "Hooks/pre-push",
        ] {
            assert!(
                super::is_known_control(std::path::Path::new(path)),
                "{path}"
            );
        }
        assert!(!super::is_known_control(std::path::Path::new("src/lib.rs")));
        assert!(!super::is_known_control(std::path::Path::new(
            ".agents/skills/a/SKILL.md"
        )));
    }

    #[test]
    fn ordinary_instruction_class_requires_agents_path_and_rejects_control_names()
    -> Result<(), Box<dyn Error>> {
        assert_eq!(
            ProtectedPath::ordinary(".agents/notes/rules.md")?.class(),
            InstructionClass::OrdinaryInstruction
        );
        assert!(ProtectedPath::ordinary(".agents/AGENTS.md").is_err());
        assert!(ProtectedPath::ordinary(".agents/.git/config").is_err());
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
        let parsed_index = parse_index_entries(&index)?.collect::<Result<Vec<_>, _>>()?;
        assert_eq!(parsed_index.len(), 1);
        assert_eq!(
            parsed_index.first().map(|row| row.path),
            Some(&b".agents/a b\xff.md"[..])
        );
        let conflict = [b"100644 ".as_slice(), oid, b" 2\t.agents/a b\xff.md\0"].concat();
        assert_eq!(
            parse_index_entries(&conflict)?
                .next()
                .transpose()?
                .map(|row| row.stage),
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
    fn archive_device_and_privacy_are_checked_before_launch() -> Result<(), Box<dyn Error>> {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700))?;
        let archive = LocalFs.open_anchor(temp.path())?;
        let mut observation = archive.metadata()?;
        validate_archive(observation.device, &observation)?;
        let device = observation.device;
        observation.device = device.wrapping_add(1);
        assert!(matches!(
            validate_archive(device, &observation),
            Err(QuarantineError::ArchiveBinding)
        ));
        observation.device = device;
        observation.mode |= 0o020;
        assert!(matches!(
            validate_archive(device, &observation),
            Err(QuarantineError::ArchiveBinding)
        ));
        Ok(())
    }

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
            0o644,
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
                0o644,
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
                0o644,
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
    use crate::quarantine::{ProtectedPath, QuarantineOutcome};
    use crate::repo::RepoPath;
    use crate::test_support::{
        Fixture, PoisonedNestedRepo, add_poisoned_gitlink, assert_raw_git_runs_poison_filter,
    };
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

    fn seed_embedded_gitlink(fixture: &Fixture) -> Result<PoisonedNestedRepo, Box<dyn Error>> {
        fs::write(
            fixture.root.join(".gitmodules"),
            "[submodule \"embedded\"]\n\tpath = .agents/sub\n\turl = ../embedded\n",
        )?;
        let marker = fixture._temp.path().join("embedded-filter-ran");
        let nested = add_poisoned_gitlink(fixture, &fixture.root, ".agents/sub", &marker)?;
        fixture.run(&["add", "--", ".gitmodules"])?;
        fixture.run(&["commit", "-m", "embedded gitlink"])?;
        Ok(nested)
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

    fn evidence_git<'a>(
        fixture: &'a Fixture,
        token: &'a CancelToken,
    ) -> Result<Git<'a>, Box<dyn Error>> {
        Ok(Git::new(
            &fixture.executor,
            &fixture.repo,
            &fixture.runtime,
            fixture.budget()?,
            token,
        ))
    }

    #[test]
    fn quarantine_attributes_protected_changes_against_delivery_merge_base()
    -> Result<(), Box<dyn Error>> {
        for change in [
            "upstream",
            "upstream_private",
            "edit",
            "delete",
            "hidden_disk",
            "hidden_mode",
        ] {
            let own_change = !change.starts_with("upstream");
            let fixture = Fixture::new()?;
            seed_protected(&fixture)?;
            fixture.run(&["branch", "delivery"])?;
            fixture.run(&["checkout", "-b", "evidence-fixture"])?;
            let base = fixture.head()?;
            let token = CancelToken::new();
            let git = evidence_git(&fixture, &token)?;
            let archive = archive_dir(&fixture, "merge-archive")?;
            let snapshot = git.snapshot_instructions(scope(), &protected()?, &archive)?;
            let recovery = acquire_absent_recovery(&fixture, scope(), base)?;
            fixture.run(&["checkout", "delivery"])?;
            fs::write(fixture.root.join("AGENTS.md"), b"upstream policy\n")?;
            fs::create_dir(fixture.root.join(".github"))?;
            fs::write(fixture.root.join(".github/new.yml"), b"upstream addition\n")?;
            fs::remove_file(fixture.root.join(".agents/a.md"))?;
            fixture.run(&["add", "-A"])?;
            fixture.run(&["commit", "-m", "upstream protected changes"])?;
            let target = fixture.head()?;
            fixture.run(&["checkout", "evidence-fixture"])?;
            fs::write(fixture.root.join("unrelated.txt"), b"writer source\n")?;
            fixture.run(&["add", "unrelated.txt"])?;
            fixture.run(&["commit", "-m", "writer source"])?;
            fixture.run(&["merge", "--no-edit", "delivery"])?;
            if change == "edit" {
                fs::write(fixture.root.join("AGENTS.md"), b"writer policy\n")?;
                fixture.run(&["add", "AGENTS.md"])?;
                fixture.run(&["commit", "-m", "writer policy"])?;
            } else if change == "delete" {
                fixture.run(&["rm", ".github/new.yml"])?;
                fixture.run(&["commit", "-m", "writer deletes upstream protected file"])?;
            } else if change == "hidden_disk" {
                fixture.run(&["update-index", "--assume-unchanged", "AGENTS.md"])?;
                fs::write(fixture.root.join("AGENTS.md"), b"hidden policy\n")?;
            } else if change == "hidden_mode" {
                fixture.run(&["update-index", "--assume-unchanged", "AGENTS.md"])?;
                fs::set_permissions(
                    fixture.root.join("AGENTS.md"),
                    fs::Permissions::from_mode(0o755),
                )?;
            } else if change == "upstream_private" {
                fs::set_permissions(
                    fixture.root.join("AGENTS.md"),
                    fs::Permissions::from_mode(0o600),
                )?;
            }
            let outcome = git
                .quarantine_instructions(&recovery, &snapshot, target, &archive)
                .map_err(|error| format!("{change} quarantine: {error}"))?;
            assert_eq!(
                matches!(outcome, QuarantineOutcome::ReviewRequired { .. }),
                own_change
            );
            if let QuarantineOutcome::ReviewRequired { paths, .. } = &outcome {
                let expected = if change == "delete" {
                    ".github/new.yml"
                } else {
                    "AGENTS.md"
                };
                assert_eq!(
                    paths
                        .iter()
                        .filter_map(|path| path.as_path_text())
                        .collect::<Vec<_>>(),
                    vec![expected]
                );
            }
            assert_eq!(
                git.quarantine_instructions(&recovery, &snapshot, target, &archive)
                    .map_err(|error| format!("{change} quarantine: {error}"))?,
                outcome
            );
            assert_eq!(
                fs::read(fixture.root.join("AGENTS.md"))?,
                if change == "hidden_disk" {
                    b"hidden policy\n".as_slice()
                } else if change == "edit" {
                    b"writer policy\n".as_slice()
                } else {
                    b"upstream policy\n".as_slice()
                }
            );
            assert!(!fixture.root.join(".agents/a.md").exists());
            assert_eq!(
                fixture.root.join(".github/new.yml").exists(),
                change != "delete"
            );
        }
        Ok(())
    }

    #[test]
    fn quarantine_archives_then_restores_only_attributable_ordinary_edit()
    -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        seed_protected(&fixture)?;
        let base = fixture.head()?;
        let token = CancelToken::new();
        let git = evidence_git(&fixture, &token)?;
        let scope = scope();
        let archive = archive_dir(&fixture, "ordinary-archive")?;
        let snapshot =
            git.snapshot_instructions(scope, &git.writer_instruction_paths()?, &archive)?;
        snapshot.persist(&archive)?;
        let recovery = acquire_absent_recovery(&fixture, scope, base)?;
        let snapshot = git.recover_instruction_snapshot(&recovery, base, &archive)?;

        fs::write(
            fixture.root.join(".agents/a.md"),
            b"attempt ordinary edit\n",
        )?;
        fs::write(
            fixture.root.join("unrelated.txt"),
            b"unrelated edit retained\n",
        )?;
        let outcome =
            git.quarantine_instructions(&recovery, &snapshot, snapshot.head(), &archive)?;

        assert_eq!(
            git.quarantine_instructions(&recovery, &snapshot, snapshot.head(), &archive)?,
            outcome
        );
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
        // A crash between sealed evidence and the result cannot replay a
        // partial restore. Recovery retains that archive and requires review.
        archive.unlink(std::ffi::OsStr::new("quarantine-result.json"))?;
        assert!(matches!(
            git.quarantine_instructions(&recovery, &snapshot, snapshot.head(), &archive)?,
            QuarantineOutcome::ReviewRequired { .. }
        ));
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
        let git = evidence_git(&fixture, &token)?;
        let scope = scope();
        let archive = archive_dir(&fixture, "staged-archive")?;
        let snapshot = git.snapshot_instructions(scope, &protected()?, &archive)?;
        let recovery = acquire_absent_recovery(&fixture, scope, base)?;
        fs::write(fixture.root.join(".agents/a.md"), b"staged ordinary edit\n")?;
        fixture.run(&["add", "--", ".agents/a.md"])?;

        let outcome =
            git.quarantine_instructions(&recovery, &snapshot, snapshot.head(), &archive)?;

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
    fn large_index_and_source_config_edits_do_not_force_review() -> Result<(), Box<dyn Error>> {
        use crate::utility::environment;
        use crate::utility::exec_options;
        use provenant_supervisor::exec::{Executor, Termination};
        let fixture = Fixture::new()?;
        seed_protected(&fixture)?;
        let blob = fixture.run(&["rev-parse", "HEAD:unrelated.txt"])?;
        let oid = std::str::from_utf8(&blob.stdout.bytes)?.trim();
        let mut entries = String::new();
        for index in 0..=crate::git_evidence::MAX_DISCLOSURE_FILES {
            use std::fmt::Write;
            writeln!(&mut entries, "100644 {oid}\tbulk/{index:05}.txt")?;
        }
        let argv = [
            fixture.git.as_os_str().to_owned(),
            "update-index".into(),
            "--index-info".into(),
        ];
        let env = environment(&fixture.runtime, &fixture.repo);
        let options = exec_options(
            &argv,
            &fixture.root,
            fixture.budget()?,
            &env,
            Some(entries.as_bytes()),
            None,
        );
        assert_eq!(
            fixture.executor.execute(options)?.termination,
            Termination::Exit(0)
        );
        fs::create_dir_all(fixture.root.join("crates/config"))?;
        fs::write(fixture.root.join("crates/config/source.rs"), b"baseline\n")?;
        fixture.run(&["add", "--", "crates/config/source.rs"])?;
        fixture.run(&["commit", "-m", "large index baseline"])?;
        let base = fixture.head()?;
        let token = CancelToken::new();
        let git = evidence_git(&fixture, &token)?;
        let scope = scope();
        let archive = archive_dir(&fixture, "large-index-archive")?;
        let snapshot = git.snapshot_instructions(scope, &protected()?, &archive)?;
        let recovery = acquire_absent_recovery(&fixture, scope, base)?;
        fs::write(
            fixture.root.join("crates/config/source.rs"),
            b"source edit\n",
        )?;
        fs::write(fixture.root.join(".agents/a.md"), b"ordinary edit\n")?;
        assert!(matches!(
            git.quarantine_instructions(&recovery, &snapshot, snapshot.head(), &archive)?,
            QuarantineOutcome::Restored { .. }
        ));
        assert_eq!(
            fs::read(fixture.root.join(".agents/a.md"))?,
            b"baseline ordinary\n"
        );
        assert_eq!(
            fs::read(fixture.root.join("crates/config/source.rs"))?,
            b"source edit\n"
        );
        Ok(())
    }

    #[test]
    fn policy_control_edit_is_archived_and_retained_for_review() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        seed_protected(&fixture)?;
        let base = fixture.head()?;
        let token = CancelToken::new();
        let git = evidence_git(&fixture, &token)?;
        let scope = scope();
        let archive = archive_dir(&fixture, "policy-archive")?;
        let snapshot = git.snapshot_instructions(scope, &protected()?, &archive)?;
        let recovery = acquire_absent_recovery(&fixture, scope, base)?;
        fs::write(fixture.root.join("AGENTS.md"), b"changed policy\n")?;

        let outcome =
            git.quarantine_instructions(&recovery, &snapshot, snapshot.head(), &archive)?;

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

    fn seed_tracked_link(fixture: &Fixture) -> Result<(), Box<dyn Error>> {
        seed_protected(fixture)?;
        std::os::unix::fs::symlink("AGENTS.md", fixture.root.join("CLAUDE.md"))?;
        fixture.run(&["add", "--", "CLAUDE.md"])?;
        fixture.run(&["commit", "-m", "tracked control link"])?;
        Ok(())
    }

    #[test]
    fn tracked_symlink_control_file_snapshots_its_target_bytes() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        seed_tracked_link(&fixture)?;
        let token = CancelToken::new();
        let git = evidence_git(&fixture, &token)?;
        let archive = archive_dir(&fixture, "link-snapshot")?;
        let protected = [ProtectedPath::policy_control("CLAUDE.md")?];

        let snapshot = git.snapshot_instructions(scope(), &protected, &archive)?;

        let file = snapshot.files.first().ok_or("link was not snapshotted")?;
        assert_eq!(file.bytes, b"AGENTS.md");
        assert!(super::is_link_mode(file.fingerprint.mode));
        Ok(())
    }

    #[test]
    fn retargeted_and_retyped_control_links_are_archived_without_following()
    -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        seed_tracked_link(&fixture)?;
        let base = fixture.head()?;
        let token = CancelToken::new();
        let git = evidence_git(&fixture, &token)?;
        let scope = scope();
        let archive = archive_dir(&fixture, "link-archive")?;
        let protected = [ProtectedPath::policy_control("CLAUDE.md")?];
        let snapshot = git.snapshot_instructions(scope, &protected, &archive)?;
        let recovery = acquire_absent_recovery(&fixture, scope, base)?;
        let link = fixture.root.join("CLAUDE.md");
        fs::remove_file(&link)?;
        std::os::unix::fs::symlink("unrelated.txt", &link)?;

        let outcome =
            git.quarantine_instructions(&recovery, &snapshot, snapshot.head(), &archive)?;

        assert!(matches!(outcome, QuarantineOutcome::ReviewRequired { .. }));
        assert_eq!(fs::read_link(&link)?, std::path::Path::new("unrelated.txt"));
        let blobs = fixture._temp.path().join("link-archive/blobs");
        assert_eq!(fs::read(blobs.join("00000000.before"))?, b"AGENTS.md");
        assert_eq!(fs::read(blobs.join("00000000.after"))?, b"unrelated.txt");

        // Link to regular file with the same bytes is still a retype.
        let archive = archive_dir(&fixture, "retype-archive")?;
        fs::remove_file(&link)?;
        std::os::unix::fs::symlink("AGENTS.md", &link)?;
        let snapshot = git.snapshot_instructions(scope, &protected, &archive)?;
        fs::remove_file(&link)?;
        fs::write(&link, b"AGENTS.md")?;
        let outcome =
            git.quarantine_instructions(&recovery, &snapshot, snapshot.head(), &archive)?;
        assert!(matches!(outcome, QuarantineOutcome::ReviewRequired { .. }));
        Ok(())
    }

    #[test]
    fn interrupted_archive_retains_prior_bytes_and_requires_review() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        seed_protected(&fixture)?;
        let base = fixture.head()?;
        let token = CancelToken::new();
        let git = evidence_git(&fixture, &token)?;
        let scope = scope();
        let archive = archive_dir(&fixture, "failed-archive")?;
        let snapshot = git.snapshot_instructions(scope, &protected()?, &archive)?;
        let recovery = acquire_absent_recovery(&fixture, scope, base)?;
        fs::write(
            fixture.root.join(".agents/a.md"),
            b"must remain on archive failure\n",
        )?;
        fs::write(
            fixture._temp.path().join("failed-archive/git-status.z"),
            b"pre-existing collision",
        )?;

        assert!(matches!(
            git.quarantine_instructions(&recovery, &snapshot, snapshot.head(), &archive)?,
            QuarantineOutcome::ReviewRequired { .. }
        ));
        let manifest: serde_json::Value = serde_json::from_slice(&fs::read(
            fixture._temp.path().join("failed-archive/evidence.json"),
        )?)?;
        let interrupted = manifest["interrupted_archives"][0]
            .as_str()
            .ok_or("interrupted archive missing")?;
        assert_eq!(
            fs::read(
                fixture
                    ._temp
                    .path()
                    .join("failed-archive")
                    .join(interrupted)
                    .join("git-status.z")
            )?,
            b"pre-existing collision"
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
        let git = evidence_git(&fixture, &token)?;
        let selected = RepoPath::new(".agents/a.md")?;
        fs::write(fixture.root.join(".agents/a.md"), b"tracked disk change\n")?;
        let evidence = git.evidence(base, std::slice::from_ref(&selected))?;
        assert_eq!((evidence.base, evidence.head), (base, base));
        assert_eq!(evidence.selected_paths, vec![selected]);
        assert!(!evidence.worktree_diff.is_empty());
        assert_eq!(evidence.before, evidence.after);
        assert_eq!(git.verify_tip(base)?, base);
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
    fn quarantine_sees_ignored_protected_files_and_does_not_read_embedded_repos()
    -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        seed_protected(&fixture)?;
        let nested = seed_embedded_gitlink(&fixture)?;
        assert_raw_git_runs_poison_filter(&fixture, &nested)?;
        let base = fixture.head()?;
        fixture.run(&["config", "submodule.recurse", "true"])?;
        let token = CancelToken::new();
        let repo = fixture.runtime.open_repo(
            &fixture.executor,
            &fixture.root,
            fixture.budget()?,
            &token,
        )?;
        let git = Git::new(
            &fixture.executor,
            &repo,
            &fixture.runtime,
            fixture.budget()?,
            &token,
        );
        let scope = scope();
        let archive = archive_dir(&fixture, "protected-archive")?;
        let snapshot = git.snapshot_instructions(scope, &protected()?, &archive)?;
        let recovery = acquire_absent_recovery(&fixture, scope, base)?;

        fs::write(fixture.root.join(".agents/.gitignore"), "/hidden.md\n")?;
        fs::write(fixture.root.join(".agents/hidden.md"), "protected hidden\n")?;
        nested.arm()?;
        let outcome =
            git.quarantine_instructions(&recovery, &snapshot, snapshot.head(), &archive)?;
        assert!(matches!(outcome, QuarantineOutcome::ReviewRequired { .. }));
        let QuarantineOutcome::ReviewRequired { paths, .. } = &outcome else {
            return Err("ignored protected changes were not retained for review".into());
        };
        assert!(
            paths
                .iter()
                .any(|path| path.as_path_text() == Some(".agents/hidden.md"))
        );
        assert!(
            !nested.marker.exists(),
            "quarantine reads ran a nested clean filter"
        );
        let manifest = fs::read(fixture._temp.path().join("protected-archive/evidence.json"))?;
        assert!(
            manifest
                .windows(b"hidden.md".len())
                .any(|part| part == b"hidden.md")
        );
        assert!(
            manifest
                .windows(b"unknown".len())
                .any(|part| part == b"unknown")
        );
        assert_eq!(
            fs::read(fixture.root.join(".agents/hidden.md"))?,
            b"protected hidden\n"
        );
        Ok(())
    }
}
