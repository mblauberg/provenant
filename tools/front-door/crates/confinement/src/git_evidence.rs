//! Pinned, bounded Git evidence obtained only through confinement's closed utility API.
use crate::repo::{RepoPath, VerifiedRepo};
use crate::status::{StatusError, StatusKind, parse_index_entries, parse_porcelain_v2_z};
use crate::utility::{GitRead, UtilityBudget, UtilityError, UtilityRuntime, run_utility};
use provenant_core::CancelToken;
use provenant_core::{GitOid, RelativePath};
use provenant_records::{CanonicalV1, FileDigest, PathWire};
use provenant_supervisor::exec::{ExecOutcome, Executor, Termination};
use provenant_supervisor::fs::{FsError, Metadata, SafeDir};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::ffi::{OsStr, OsString};
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

impl<'a> Git<'a> {
    /// Collect pinned HEAD/base diffs, the raw NUL status stream, and selected file
    /// fingerprints. Any timeout, truncation, index race, HEAD movement, unsafe
    /// path, or content drift refuses to certify complete evidence.
    pub fn evidence(
        &self,
        base: GitOid,
        selected: &[RepoPath],
    ) -> Result<GitEvidence, GitEvidenceError> {
        self.repo
            .revalidate()
            .map_err(|_| GitEvidenceError::RepositoryChanged)?;
        let head = self.head()?;
        let selected_paths = canonical_paths(selected)?;
        if selected_paths.is_empty() || selected_paths.len() > MAX_DISCLOSURE_FILES {
            return Err(GitEvidenceError::TooLarge);
        }
        let index_before = read_index_fingerprint(self.repo.private_dir())?;
        refuse_index_lock(self.repo.private_dir())?;
        self.prove_selected_tracked(&selected_paths)?;
        let mut total = 0_u64;
        let mut before = Vec::with_capacity(selected_paths.len());
        for path in &selected_paths {
            let snapshot = read_optional_file(self.repo.root_dir(), path, MAX_DISCLOSURE_BYTES)?;
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

        let status = self.read(GitRead::Status)?;
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
            matches!(
                record.kind,
                StatusKind::Conflict | StatusKind::RenameOrCopy | StatusKind::Unknown
            ) && (selected_bytes.iter().any(|path| path == &record.path)
                || record
                    .original_path
                    .as_ref()
                    .is_some_and(|original| selected_bytes.iter().any(|path| path == original)))
        }) {
            return Err(GitEvidenceError::Incomplete);
        }

        let commit_diff = self
            .read(GitRead::Diff {
                base,
                head,
                paths: selected_paths.clone(),
            })?
            .stdout
            .bytes;
        let index_diff = self
            .read(GitRead::DiffIndex {
                paths: selected_paths.clone(),
            })?
            .stdout
            .bytes;
        let worktree_diff = self
            .read(GitRead::DiffWorktree {
                paths: selected_paths.clone(),
            })?
            .stdout
            .bytes;

        let mut after = Vec::with_capacity(selected_paths.len());
        for path in &selected_paths {
            let snapshot = read_optional_file(self.repo.root_dir(), path, MAX_DISCLOSURE_BYTES)?;
            after.push(match snapshot {
                Some(file) => file.fingerprint,
                None => absent_fingerprint(path)?,
            });
        }
        if before != after {
            return Err(GitEvidenceError::ConcurrentChange);
        }
        refuse_index_lock(self.repo.private_dir())?;
        let index_after = read_index_fingerprint(self.repo.private_dir())?;
        let head_after = self.head()?;
        let status_after = self.read(GitRead::Status)?;
        if index_before != index_after
            || head != head_after
            || status.stdout.bytes != status_after.stdout.bytes
        {
            return Err(GitEvidenceError::ConcurrentChange);
        }
        self.repo
            .revalidate()
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

    fn prove_selected_tracked(&self, paths: &[RepoPath]) -> Result<(), GitEvidenceError> {
        let index = self.read(GitRead::IndexEntries {
            paths: paths.to_vec(),
        })?;
        if !index_has_stage_zero_regular(&index.stdout.bytes, paths)? {
            return Err(GitEvidenceError::UntrackedOrUnsupportedPath);
        }
        Ok(())
    }

    /// Changed and untracked paths when HEAD is still `base`; empty once the
    /// writer committed. Ignored paths and `.review-snapshots/` never count.
    pub fn uncommitted_since(&self, base: GitOid) -> Result<Vec<Vec<u8>>, GitEvidenceError> {
        if self.head()? != base {
            return Ok(Vec::new());
        }
        // Lists each untracked file, not its directory, whatever
        // `status.showUntrackedFiles` says.
        let status = self.read(GitRead::StatusIncludingIgnored)?;
        Ok(parse_porcelain_v2_z(&status.stdout.bytes)?
            .into_iter()
            .filter(|record| record.kind != StatusKind::Ignored)
            .map(|record| record.path)
            .filter(|path| !path.starts_with(b".review-snapshots/"))
            .collect())
    }

    pub fn verify_tip(&self, expected: GitOid) -> Result<GitOid, GitEvidenceError> {
        self.repo
            .revalidate()
            .map_err(|_| GitEvidenceError::RepositoryChanged)?;
        let actual = self.head()?;
        self.repo
            .revalidate()
            .map_err(|_| GitEvidenceError::RepositoryChanged)?;
        if actual != expected {
            return Err(GitEvidenceError::ConcurrentChange);
        }
        Ok(actual)
    }

    pub(crate) fn head(&self) -> Result<GitOid, GitEvidenceError> {
        let output = self.read(GitRead::RevParseHead)?;
        let bytes = output
            .stdout
            .bytes
            .strip_suffix(b"\n")
            .ok_or(GitEvidenceError::Incomplete)?;
        if bytes.contains(&b'\n') || bytes.contains(&b'\r') {
            return Err(GitEvidenceError::Incomplete);
        }
        GitOid::parse(std::str::from_utf8(bytes).map_err(|_| GitEvidenceError::Incomplete)?)
            .map_err(|_| GitEvidenceError::Incomplete)
    }

    pub(crate) fn read(&self, read: GitRead) -> Result<ExecOutcome, GitEvidenceError> {
        let output = run_utility(
            self.executor,
            read,
            self.repo,
            self.runtime,
            self.budget,
            self.cancel,
        )?;
        if output.termination != Termination::Exit(0) {
            return Err(GitEvidenceError::GitFailed);
        }
        if output.stdout.bytes.len() as u64 > MAX_GIT_RECORD_BYTES {
            return Err(GitEvidenceError::TooLarge);
        }
        Ok(output)
    }
}

fn index_has_stage_zero_regular(raw: &[u8], paths: &[RepoPath]) -> Result<bool, GitEvidenceError> {
    let wanted = paths
        .iter()
        .map(|path| path.as_os_str().as_bytes())
        .collect::<Vec<_>>();
    let mut found = vec![false; wanted.len()];
    for entry in parse_index_entries(raw)? {
        let entry = entry?;
        let Some(position) = wanted.iter().position(|candidate| *candidate == entry.path) else {
            continue;
        };
        if entry.stage != 0 || !matches!(entry.mode, 0o100644 | 0o100755) {
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
    let (bytes, after) =
        crate::stable_read::read_regular(&*parent, &leaf, limit).map_err(map_stable_read_error)?;
    let fingerprint = fingerprint(path, &bytes, after)?;
    Ok(FileSnapshot { bytes, fingerprint })
}

fn map_stable_read_error(error: crate::stable_read::ReadError) -> GitEvidenceError {
    use crate::stable_read::ReadError;
    match error {
        ReadError::Unsupported => GitEvidenceError::UntrackedOrUnsupportedPath,
        ReadError::TooLarge => GitEvidenceError::TooLarge,
        ReadError::Changed => GitEvidenceError::ConcurrentChange,
        ReadError::Filesystem(error) if error.is_not_found() => {
            GitEvidenceError::UntrackedOrUnsupportedPath
        }
        ReadError::Filesystem(error) => GitEvidenceError::Filesystem(error),
        ReadError::Io(_) => GitEvidenceError::Incomplete,
    }
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

/// Like `read_tracked_file`, but a symlink leaf is evidence too: its bytes are
/// the link target, read with readlink and never followed.
pub(crate) fn read_protected_entry(
    root: &dyn SafeDir,
    path: &RepoPath,
    limit: u64,
) -> Result<FileSnapshot, GitEvidenceError> {
    use std::os::unix::ffi::OsStrExt;
    let (parent, leaf) = open_parent(root, path)?;
    let before = parent.metadata_at(&leaf, false)?;
    if before.kind != provenant_supervisor::fs::FileKind::Symlink {
        return read_tracked_file(root, path, limit);
    }
    let target = parent.read_link(&leaf)?;
    let bytes = target.as_os_str().as_bytes().to_vec();
    if bytes.len() as u64 > limit {
        return Err(GitEvidenceError::TooLarge);
    }
    let after = parent.metadata_at(&leaf, false)?;
    if !crate::stable_read::same_metadata(&before, &after) {
        return Err(GitEvidenceError::ConcurrentChange);
    }
    let fingerprint = fingerprint(path, &bytes, after)?;
    Ok(FileSnapshot { bytes, fingerprint })
}

pub(crate) fn read_optional_protected_entry(
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
        Ok(_) => read_protected_entry(root, path, limit).map(Some),
        Err(error) if error.is_not_found() => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn open_parent<'a>(
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

pub(crate) enum HeldDir<'a> {
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

pub(crate) fn read_index_fingerprint(
    directory: &dyn SafeDir,
) -> Result<IndexFingerprint, GitEvidenceError> {
    let (bytes, after) =
        crate::stable_read::read_regular(directory, OsStr::new("index"), MAX_GIT_RECORD_BYTES)
            .map_err(|error| match error {
                crate::stable_read::ReadError::TooLarge => GitEvidenceError::TooLarge,
                crate::stable_read::ReadError::Changed => GitEvidenceError::ConcurrentChange,
                crate::stable_read::ReadError::Filesystem(error) => {
                    GitEvidenceError::Filesystem(error)
                }
                crate::stable_read::ReadError::Unsupported
                | crate::stable_read::ReadError::Io(_) => GitEvidenceError::Incomplete,
            })?;
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
    use super::{Git, digest_diff_parts};
    use crate::test_support::Fixture;
    use provenant_core::CancelToken;
    use provenant_records::CanonicalV1;
    use std::{error::Error, fs};

    #[test]
    fn uncommitted_paths_skip_ignored_and_snapshots_and_need_an_unmoved_head()
    -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let base = fixture.head()?;
        let token = CancelToken::new();
        let git = Git::new(
            &fixture.executor,
            &fixture.repo,
            &fixture.runtime,
            fixture.budget()?,
            &token,
        );
        assert!(git.uncommitted_since(base)?.is_empty());
        fs::write(fixture.root.join(".gitignore"), "ignored.log\n")?;
        fs::write(fixture.root.join("ignored.log"), "noise")?;
        fs::write(fixture.root.join("edit.txt"), "work")?;
        fs::create_dir(fixture.root.join("new"))?;
        fs::write(fixture.root.join("new/file.rs"), "work")?;
        fs::create_dir(fixture.root.join(".review-snapshots"))?;
        fs::write(fixture.root.join(".review-snapshots/1.md"), "issue")?;
        let mut dirty = git.uncommitted_since(base)?;
        dirty.sort();
        assert_eq!(
            dirty,
            [
                b".gitignore".to_vec(),
                b"edit.txt".to_vec(),
                b"new/file.rs".to_vec()
            ]
        );
        // A commit moves HEAD past the base: the lane delivered something.
        fixture.run(&["add", "--", "edit.txt"])?;
        fixture.run(&["commit", "-m", "work"])?;
        assert!(git.uncommitted_since(base)?.is_empty());
        Ok(())
    }

    #[test]
    fn diff_digest_separates_views_and_preserves_empty_views() {
        let first = digest_diff_parts(b"ab", b"c", b"");
        assert_ne!(first, digest_diff_parts(b"a", b"bc", b""));
        assert_ne!(first, CanonicalV1::file_digest(b"abc"));
    }
}
