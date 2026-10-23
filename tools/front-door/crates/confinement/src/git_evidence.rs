//! Pinned, bounded Git evidence obtained only through confinement's closed utility API.
use crate::repo::{RepoPath, VerifiedRepo};
use crate::status::{StatusError, StatusKind, parse_porcelain_v2_z};
use crate::utility::{
    GitRead, UtilityBudget, UtilityError, UtilityRead, UtilityRuntime, run_utility,
};
use provenant_core::CancelToken;
use provenant_core::{GitOid, RelativePath};
use provenant_records::{CanonicalV1, FileDigest, PathWire};
use provenant_supervisor::exec::{ExecOutcome, Executor, Termination};
use provenant_supervisor::fs::{FileKind, FsError, Metadata, SafeDir};
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use std::ffi::{OsStr, OsString};
use std::io::{Read, Seek, SeekFrom};
use std::ops::Deref;
use std::os::unix::ffi::OsStrExt;
use std::path::Component;
use thiserror::Error;

pub const MAX_DISCLOSURE_FILES: usize = 10_000;
pub const MAX_DISCLOSURE_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_GIT_RECORD_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum GitEvidenceError {
    #[error("Git repository identity changed")]
    RepositoryChanged,
    #[error("Git evidence is incomplete or malformed")]
    Incomplete,
    #[error("Git selected an untracked path")]
    SelectedUntracked,
    #[error("Git selected a path that is not an ordinary tracked file")]
    UntrackedOrUnsupportedPath,
    #[error("Git evidence changed while it was being collected")]
    ConcurrentChange,
    #[error("Git command failed")]
    GitFailed,
    #[error("Git path is invalid")]
    InvalidPath,
    #[error("Git evidence exceeds its byte or file bound")]
    TooLarge,
    #[error("Git or filesystem operation was refused: {0}")]
    Utility(#[from] UtilityError),
    #[error("filesystem operation was refused: {0}")]
    Filesystem(#[from] FsError),
    #[error("status record was malformed: {0}")]
    Status(#[from] StatusError),
    #[error("path record could not be encoded")]
    PathWire,
}

/// A byte-preserving fingerprint of a tracked working-tree file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TrackedFingerprint {
    pub path: PathWire,
    pub present: bool,
    pub digest: Option<FileDigest>,
    pub device: Option<u64>,
    pub inode: Option<u64>,
    pub links: Option<u64>,
    pub size: Option<u64>,
    pub modified_ns: Option<i64>,
    pub mode: Option<u32>,
}

/// Raw external Git results plus the facts that pin their interpretation.
/// Raw streams stay bounded in memory until the caller durably archives them.
#[derive(Debug)]
pub struct GitEvidence {
    pub base: GitOid,
    pub head: GitOid,
    pub status_z: Vec<u8>,
    pub selected_paths: Vec<RepoPath>,
    pub commit_diff: Vec<u8>,
    pub index_diff: Vec<u8>,
    pub worktree_diff: Vec<u8>,
    pub before: Vec<TrackedFingerprint>,
    pub after: Vec<TrackedFingerprint>,
    pub diff_digest: FileDigest,
}

/// Per-operation Git capability. It binds the trusted executor, verified
/// repository, pinned utility runtime, finite budget, and cancellation token so
/// policy services never rebuild this authority from path strings.
pub struct Git<'a> {
    pub(crate) executor: &'a dyn Executor,
    pub(crate) repo: &'a VerifiedRepo,
    pub(crate) runtime: &'a UtilityRuntime,
    pub(crate) budget: UtilityBudget,
    pub(crate) cancel: &'a CancelToken,
}

impl<'a> Git<'a> {
    pub fn new(
        executor: &'a dyn Executor,
        repo: &'a VerifiedRepo,
        runtime: &'a UtilityRuntime,
        budget: UtilityBudget,
        cancel: &'a CancelToken,
    ) -> Self {
        Self {
            executor,
            repo,
            runtime,
            budget,
            cancel,
        }
    }

    pub fn evidence(
        &self,
        base: GitOid,
        selected: &[RepoPath],
    ) -> Result<GitEvidence, GitEvidenceError> {
        collect_git_evidence(
            self.executor,
            self.repo,
            self.runtime,
            self.budget,
            self.cancel,
            base,
            selected,
        )
    }

    pub fn verify_tip(&self, expected: GitOid) -> Result<GitOid, GitEvidenceError> {
        verify_tip(
            self.executor,
            self.repo,
            self.runtime,
            self.budget,
            self.cancel,
            expected,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IndexFingerprint {
    pub device: u64,
    pub inode: u64,
    pub links: u64,
    pub size: u64,
    pub modified_ns: i64,
    pub digest: FileDigest,
}

#[derive(Debug)]
pub(crate) struct FileSnapshot {
    pub bytes: Vec<u8>,
    pub fingerprint: TrackedFingerprint,
}

/// Collect pinned HEAD/base diffs, the raw NUL status stream, and selected file
/// fingerprints. Any timeout, truncation, index race, HEAD movement, unsafe
/// path, or content drift refuses to certify complete evidence.
fn collect_git_evidence(
    executor: &dyn Executor,
    repo: &VerifiedRepo,
    runtime: &UtilityRuntime,
    budget: UtilityBudget,
    cancel: &CancelToken,
    base: GitOid,
    selected: &[RepoPath],
) -> Result<GitEvidence, GitEvidenceError> {
    repo.revalidate()
        .map_err(|_| GitEvidenceError::RepositoryChanged)?;
    let head = read_head(executor, repo, runtime, budget, cancel)?;
    let selected_paths = canonical_paths(selected)?;
    if selected_paths.is_empty() || selected_paths.len() > MAX_DISCLOSURE_FILES {
        return Err(GitEvidenceError::TooLarge);
    }
    let index_before = read_index_fingerprint(repo.private_dir())?;
    refuse_index_lock(repo.private_dir())?;
    prove_selected_tracked(executor, repo, runtime, budget, cancel, &selected_paths)?;
    let mut total = 0_u64;
    let mut before = Vec::with_capacity(selected_paths.len());
    for path in &selected_paths {
        let snapshot = read_optional_file(repo.root_dir(), path, MAX_DISCLOSURE_BYTES)?;
        let bytes_len = snapshot.as_ref().map_or(0, |file| file.bytes.len() as u64);
        total = total
            .checked_add(bytes_len)
            .ok_or(GitEvidenceError::TooLarge)?;
        if total > MAX_DISCLOSURE_BYTES {
            return Err(GitEvidenceError::TooLarge);
        }
        before.push(match snapshot {
            Some(file) => file.fingerprint,
            None => absent_fingerprint(path)?,
        });
    }

    let status = read_git(executor, repo, runtime, budget, cancel, GitRead::Status)?;
    let status_paths = parse_porcelain_v2_z(&status.stdout.bytes)?;
    let selected_bytes = selected_paths
        .iter()
        .map(|path| path.as_os_str().as_bytes().to_vec())
        .collect::<Vec<_>>();
    if status_paths.iter().any(|record| {
        record.kind == StatusKind::Untracked
            && selected_bytes.iter().any(|path| path == &record.path)
    }) {
        return Err(GitEvidenceError::SelectedUntracked);
    }
    if status_paths.iter().any(|record| {
        matches!(record.kind, StatusKind::Conflict | StatusKind::RenameOrCopy)
            && (selected_bytes.iter().any(|path| path == &record.path)
                || record
                    .original_path
                    .as_ref()
                    .is_some_and(|original| selected_bytes.iter().any(|path| path == original)))
    }) {
        return Err(GitEvidenceError::Incomplete);
    }

    let commit_diff = read_git(
        executor,
        repo,
        runtime,
        budget,
        cancel,
        GitRead::Diff {
            base,
            head,
            paths: selected_paths.clone(),
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
            paths: selected_paths.clone(),
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
            paths: selected_paths.clone(),
        },
    )?
    .stdout
    .bytes;

    let mut after = Vec::with_capacity(selected_paths.len());
    for path in &selected_paths {
        let snapshot = read_optional_file(repo.root_dir(), path, MAX_DISCLOSURE_BYTES)?;
        after.push(match snapshot {
            Some(file) => file.fingerprint,
            None => absent_fingerprint(path)?,
        });
    }
    if before != after {
        return Err(GitEvidenceError::ConcurrentChange);
    }
    refuse_index_lock(repo.private_dir())?;
    let index_after = read_index_fingerprint(repo.private_dir())?;
    let head_after = read_head(executor, repo, runtime, budget, cancel)?;
    let status_after = read_git(executor, repo, runtime, budget, cancel, GitRead::Status)?;
    if index_before != index_after
        || head != head_after
        || status.stdout.bytes != status_after.stdout.bytes
    {
        return Err(GitEvidenceError::ConcurrentChange);
    }
    repo.revalidate()
        .map_err(|_| GitEvidenceError::RepositoryChanged)?;
    let diff_digest = digest_diff_parts(&commit_diff, &index_diff, &worktree_diff);
    Ok(GitEvidence {
        base,
        head,
        status_z: status.stdout.bytes,
        selected_paths,
        commit_diff,
        index_diff,
        worktree_diff,
        before,
        after,
        diff_digest,
    })
}

fn prove_selected_tracked(
    executor: &dyn Executor,
    repo: &VerifiedRepo,
    runtime: &UtilityRuntime,
    budget: UtilityBudget,
    cancel: &CancelToken,
    paths: &[RepoPath],
) -> Result<(), GitEvidenceError> {
    let index = read_git(
        executor,
        repo,
        runtime,
        budget,
        cancel,
        GitRead::IndexEntries {
            paths: paths.to_vec(),
        },
    )?;
    if !index_has_stage_zero_regular(&index.stdout.bytes, paths)? {
        return Err(GitEvidenceError::UntrackedOrUnsupportedPath);
    }
    Ok(())
}

fn index_has_stage_zero_regular(raw: &[u8], paths: &[RepoPath]) -> Result<bool, GitEvidenceError> {
    if !raw.is_empty() && !raw.ends_with(&[0]) {
        return Err(GitEvidenceError::Incomplete);
    }
    let wanted = paths
        .iter()
        .map(|path| path.as_os_str().as_bytes())
        .collect::<Vec<_>>();
    let mut found = vec![false; wanted.len()];
    for entry in raw
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let (metadata, path) = split_tab(entry).ok_or(GitEvidenceError::Incomplete)?;
        let fields = metadata.split(|byte| *byte == b' ').collect::<Vec<_>>();
        if fields.len() != 3 || path.is_empty() {
            return Err(GitEvidenceError::Incomplete);
        }
        let mode = fields.first().ok_or(GitEvidenceError::Incomplete)?;
        let oid = std::str::from_utf8(fields.get(1).ok_or(GitEvidenceError::Incomplete)?)
            .map_err(|_| GitEvidenceError::Incomplete)?;
        GitOid::parse(oid).map_err(|_| GitEvidenceError::Incomplete)?;
        let stage = fields.get(2).ok_or(GitEvidenceError::Incomplete)?;
        let Some(position) = wanted.iter().position(|candidate| *candidate == path) else {
            continue;
        };
        if *stage != b"0" || (*mode != b"100644" && *mode != b"100755") {
            return Ok(false);
        }
        let Some(slot) = found.get_mut(position) else {
            return Err(GitEvidenceError::Incomplete);
        };
        if *slot {
            return Ok(false);
        }
        *slot = true;
    }
    Ok(found.into_iter().all(|value| value))
}

/// Verify a protected tip at the repository boundary. The result is the exact
/// SHA read from Git; this grants no authority to merge or release it.
fn verify_tip(
    executor: &dyn Executor,
    repo: &VerifiedRepo,
    runtime: &UtilityRuntime,
    budget: UtilityBudget,
    cancel: &CancelToken,
    expected: GitOid,
) -> Result<GitOid, GitEvidenceError> {
    repo.revalidate()
        .map_err(|_| GitEvidenceError::RepositoryChanged)?;
    let actual = read_head(executor, repo, runtime, budget, cancel)?;
    repo.revalidate()
        .map_err(|_| GitEvidenceError::RepositoryChanged)?;
    if actual != expected {
        return Err(GitEvidenceError::ConcurrentChange);
    }
    Ok(actual)
}

pub(crate) fn read_head(
    executor: &dyn Executor,
    repo: &VerifiedRepo,
    runtime: &UtilityRuntime,
    budget: UtilityBudget,
    cancel: &CancelToken,
) -> Result<GitOid, GitEvidenceError> {
    let output = read_git(
        executor,
        repo,
        runtime,
        budget,
        cancel,
        GitRead::RevParseHead,
    )?;
    let bytes = output
        .stdout
        .bytes
        .strip_suffix(b"\n")
        .ok_or(GitEvidenceError::Incomplete)?;
    if bytes.contains(&b'\n') || bytes.contains(&b'\r') {
        return Err(GitEvidenceError::Incomplete);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| GitEvidenceError::Incomplete)?;
    GitOid::parse(text).map_err(|_| GitEvidenceError::Incomplete)
}

pub(crate) fn read_git(
    executor: &dyn Executor,
    repo: &VerifiedRepo,
    runtime: &UtilityRuntime,
    budget: UtilityBudget,
    cancel: &CancelToken,
    read: GitRead,
) -> Result<ExecOutcome, GitEvidenceError> {
    let output = run_utility(
        executor,
        UtilityRead::Git(read),
        repo,
        runtime,
        budget,
        cancel,
    )?;
    ensure_complete(&output)?;
    if output.termination != Termination::Exit(0) {
        return Err(GitEvidenceError::GitFailed);
    }
    if output.stdout.bytes.len() as u64 > MAX_GIT_RECORD_BYTES {
        return Err(GitEvidenceError::TooLarge);
    }
    Ok(output)
}

fn ensure_complete(output: &ExecOutcome) -> Result<(), GitEvidenceError> {
    let out = &output.stdout;
    let err = &output.stderr;
    if output.timed_out
        || output.drain_truncated
        || output.overflow_stopped
        || output.cancelled
        || out.truncated
        || err.truncated
        || out.sink_failed
        || err.sink_failed
        || out.sink_lost_bytes != 0
        || err.sink_lost_bytes != 0
        || out.oversized_frames != 0
        || err.oversized_frames != 0
        || out.decoder_lost_frames != 0
        || err.decoder_lost_frames != 0
        || out.read_failed
        || err.read_failed
        || out.filter_failed
        || err.filter_failed
    {
        return Err(GitEvidenceError::Incomplete);
    }
    Ok(())
}

pub(crate) fn canonical_paths(paths: &[RepoPath]) -> Result<Vec<RepoPath>, GitEvidenceError> {
    let mut result = paths.to_vec();
    result.sort_by(|left, right| {
        left.as_os_str()
            .as_bytes()
            .cmp(right.as_os_str().as_bytes())
    });
    result.dedup_by(|left, right| left.as_os_str().as_bytes() == right.as_os_str().as_bytes());
    for path in &result {
        if path.as_os_str().is_empty()
            || path.as_os_str().as_bytes().contains(&0)
            || path
                .as_path()
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(GitEvidenceError::InvalidPath);
        }
    }
    Ok(result)
}

fn fingerprint(
    path: &RepoPath,
    bytes: &[u8],
    metadata: Metadata,
) -> Result<TrackedFingerprint, GitEvidenceError> {
    let path = wire_path(path)?;
    Ok(TrackedFingerprint {
        path,
        present: true,
        digest: Some(CanonicalV1::file_digest(bytes)),
        device: Some(metadata.device),
        inode: Some(metadata.inode),
        links: Some(metadata.links),
        size: Some(metadata.size),
        modified_ns: Some(
            i64::try_from(metadata.modified_ns).map_err(|_| GitEvidenceError::Incomplete)?,
        ),
        mode: Some(metadata.mode),
    })
}

fn wire_path(path: &RepoPath) -> Result<PathWire, GitEvidenceError> {
    let relative =
        RelativePath::parse(path.as_path()).map_err(|_| GitEvidenceError::InvalidPath)?;
    PathWire::from_relative(&relative).map_err(|_| GitEvidenceError::PathWire)
}

fn absent_fingerprint(path: &RepoPath) -> Result<TrackedFingerprint, GitEvidenceError> {
    Ok(TrackedFingerprint {
        path: wire_path(path)?,
        present: false,
        digest: None,
        device: None,
        inode: None,
        links: None,
        size: None,
        modified_ns: None,
        mode: None,
    })
}

/// Reads a single tracked leaf through held SafeDir handles. Symlinks, hard
/// links, special files, path drift and oversized files cannot become evidence.
pub(crate) fn read_tracked_file(
    root: &dyn SafeDir,
    path: &RepoPath,
    limit: u64,
) -> Result<FileSnapshot, GitEvidenceError> {
    let (parent, leaf) = open_parent(root, path)?;
    let entry = match parent.metadata_at(&leaf, false) {
        Ok(metadata) => metadata,
        Err(error) if error.is_not_found() => {
            return Err(GitEvidenceError::UntrackedOrUnsupportedPath);
        }
        Err(error) => return Err(error.into()),
    };
    if entry.kind != FileKind::Regular || entry.links != 1 || entry.size > limit {
        return Err(if entry.size > limit {
            GitEvidenceError::TooLarge
        } else {
            GitEvidenceError::UntrackedOrUnsupportedPath
        });
    }
    let mut file = parent.open_read(&leaf)?;
    let before = file.metadata()?;
    if before.kind != FileKind::Regular || before.links != 1 || !entry.same_identity(&before) {
        return Err(GitEvidenceError::ConcurrentChange);
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|_| GitEvidenceError::Incomplete)?;
    let mut bytes = Vec::new();
    (&mut *file)
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| GitEvidenceError::Incomplete)?;
    if bytes.len() as u64 > limit {
        return Err(GitEvidenceError::TooLarge);
    }
    let after = file.metadata()?;
    if !same_fingerprint(&before, &after) || after.size != bytes.len() as u64 {
        return Err(GitEvidenceError::ConcurrentChange);
    }
    let fingerprint = fingerprint(path, &bytes, after)?;
    Ok(FileSnapshot { bytes, fingerprint })
}

pub(crate) fn read_optional_file(
    root: &dyn SafeDir,
    path: &RepoPath,
    limit: u64,
) -> Result<Option<FileSnapshot>, GitEvidenceError> {
    let (parent, leaf) = match open_parent(root, path) {
        Ok(parent) => parent,
        Err(GitEvidenceError::Filesystem(error)) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error),
    };
    match parent.metadata_at(&leaf, false) {
        Ok(_) => read_tracked_file(root, path, limit).map(Some),
        Err(error) if error.is_not_found() => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn open_parent<'a>(
    root: &'a dyn SafeDir,
    path: &RepoPath,
) -> Result<(HeldDir<'a>, OsString), GitEvidenceError> {
    let mut normals = Vec::new();
    for component in path.as_path().components() {
        match component {
            Component::Normal(name) => normals.push(name.to_os_string()),
            _ => return Err(GitEvidenceError::InvalidPath),
        }
    }
    if normals.is_empty() {
        return Err(GitEvidenceError::InvalidPath);
    }
    let leaf = normals.pop().ok_or(GitEvidenceError::InvalidPath)?;
    let mut parent = HeldDir::Borrowed(root);
    for name in normals {
        let child = parent.open_verified_directory(&name)?;
        parent = HeldDir::Owned(child);
    }
    Ok((parent, leaf))
}

enum HeldDir<'a> {
    Borrowed(&'a dyn SafeDir),
    Owned(Box<dyn SafeDir>),
}
impl<'a> Deref for HeldDir<'a> {
    type Target = dyn SafeDir + 'a;
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Borrowed(directory) => *directory,
            Self::Owned(directory) => directory.as_ref(),
        }
    }
}

fn split_tab(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let separator = bytes.iter().position(|byte| *byte == b'\t')?;
    Some((
        bytes.get(..separator)?,
        bytes.get(separator.saturating_add(1)..)?,
    ))
}

pub(crate) fn read_index_fingerprint(
    directory: &dyn SafeDir,
) -> Result<IndexFingerprint, GitEvidenceError> {
    let mut file = directory.open_read(OsStr::new("index"))?;
    let before = file.metadata()?;
    if before.kind != FileKind::Regular || before.links != 1 || before.size > MAX_GIT_RECORD_BYTES {
        return Err(GitEvidenceError::Incomplete);
    }
    let mut bytes = Vec::new();
    (&mut *file)
        .take(MAX_GIT_RECORD_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| GitEvidenceError::Incomplete)?;
    let after = file.metadata()?;
    if bytes.len() as u64 > MAX_GIT_RECORD_BYTES
        || after.size != bytes.len() as u64
        || !same_fingerprint(&before, &after)
    {
        return Err(GitEvidenceError::ConcurrentChange);
    }
    Ok(IndexFingerprint {
        device: after.device,
        inode: after.inode,
        links: after.links,
        size: after.size,
        modified_ns: i64::try_from(after.modified_ns).map_err(|_| GitEvidenceError::Incomplete)?,
        digest: CanonicalV1::file_digest(&bytes),
    })
}

pub(crate) fn refuse_index_lock(directory: &dyn SafeDir) -> Result<(), GitEvidenceError> {
    match directory.metadata_at(OsStr::new("index.lock"), false) {
        Ok(_) => Err(GitEvidenceError::ConcurrentChange),
        Err(error) if error.is_not_found() => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn same_fingerprint(left: &Metadata, right: &Metadata) -> bool {
    left.kind == right.kind
        && left.device == right.device
        && left.inode == right.inode
        && left.links == right.links
        && left.size == right.size
        && left.modified_ns == right.modified_ns
        && left.mode == right.mode
        && left.uid == right.uid
}

fn digest_diff_parts(commit: &[u8], index: &[u8], worktree: &[u8]) -> FileDigest {
    let mut digest = Sha256::new();
    digest.update(b"provenant-git-evidence-v1\0");
    for (label, bytes) in [
        (b"commit".as_slice(), commit),
        (b"index".as_slice(), index),
        (b"worktree".as_slice(), worktree),
    ] {
        digest.update((label.len() as u64).to_le_bytes());
        digest.update(label);
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    let bytes: [u8; 32] = digest.finalize().into();
    FileDigest::from_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::digest_diff_parts;
    use provenant_records::CanonicalV1;

    #[test]
    fn diff_digest_separates_views_and_preserves_empty_views() {
        let first = digest_diff_parts(b"ab", b"c", b"");
        assert_ne!(first, digest_diff_parts(b"a", b"bc", b""));
        assert_ne!(first, CanonicalV1::file_digest(b"abc"));
    }
}
