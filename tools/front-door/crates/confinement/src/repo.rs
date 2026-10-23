//! Verified Git repository and linked-worktree registration anchors.
//!
//! Repository paths are retained as pathname values only for Executor's child
//! cwd. Every use reopens the path with `SafeFs` and compares it with the held
//! descriptor identity. Git metadata is read through the held `SafeDir`s.
use provenant_supervisor::fs::{
    FileKind, FsError, Metadata, SafeDir, SafeFs, canonicalize, effective_uid,
};
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;

const GIT_MARKER: &str = ".git";
const GITFILE_LIMIT: u64 = 4096;
const ALTERNATES_LIMIT: u64 = 1024 * 1024;

#[derive(Debug, Error)]
pub enum RepoError {
    #[error("repository path must be absolute and contain no parent traversal")]
    InvalidPath,
    #[error("repository root is not a private owner-controlled directory")]
    InvalidRoot,
    #[error("repository has no valid .git directory or gitfile")]
    InvalidGitMarker,
    #[error("Git private directory is not registered to this worktree")]
    InvalidRegistration,
    #[error("Git common or private directory is unsafe")]
    InvalidGitDirectory,
    #[error("alternate Git object stores are not accepted for protected history")]
    AlternateObjects,
    #[error("verified repository path or Git metadata changed")]
    IdentityChanged,
    #[error("Git metadata file exceeds its bound")]
    MetadataTooLarge,
    #[error(transparent)]
    Fs(#[from] FsError),
}

/// A repository-relative, byte-preserving path. The value cannot name an
/// absolute path, a parent, the current directory, or an empty component.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RepoPath(PathBuf);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[error("repository path must be nonempty, relative, NUL-free, and traversal-free")]
pub struct RepoPathError;

impl RepoPath {
    pub fn new(path: impl AsRef<Path>) -> Result<Self, RepoPathError> {
        let path = path.as_ref();
        let bytes = path.as_os_str().as_bytes();
        if bytes.is_empty() || bytes.first() == Some(&b'/') || bytes.contains(&0) {
            return Err(RepoPathError);
        }
        if bytes
            .split(|byte| *byte == b'/')
            .any(|part| part.is_empty() || part == b"." || part == b"..")
        {
            return Err(RepoPathError);
        }
        Ok(Self(path.to_path_buf()))
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }

    pub fn as_os_str(&self) -> &OsStr {
        self.0.as_os_str()
    }

    pub(crate) fn append_to(&self, base: &mut OsString) {
        base.push(self.as_os_str());
    }
}

/// A strictly validated full local branch ref. The deliberately narrow ASCII
/// form covers branch names used by the command surface and never asks Git to
/// interpret caller-supplied revision syntax.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GitRefName(String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[error("Git ref must be a valid local branch name")]
pub struct GitRefError;

impl GitRefName {
    pub fn new(value: impl AsRef<str>) -> Result<Self, GitRefError> {
        let value = value.as_ref();
        let Some(branch) = value.strip_prefix("refs/heads/") else {
            return Err(GitRefError);
        };
        if !valid_branch(branch) {
            return Err(GitRefError);
        }
        Ok(Self(value.to_owned()))
    }

    pub fn branch(value: impl AsRef<str>) -> Result<Self, GitRefError> {
        let branch = value.as_ref();
        if !valid_branch(branch) {
            return Err(GitRefError);
        }
        Ok(Self(format!("refs/heads/{branch}")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn short_name(&self) -> &str {
        self.0.strip_prefix("refs/heads/").unwrap_or_default()
    }
}

fn valid_branch(branch: &str) -> bool {
    if branch.is_empty()
        || branch == "@"
        || branch.starts_with('-')
        || branch.starts_with('/')
        || branch.ends_with('/')
        || branch.ends_with('.')
        || branch
            .bytes()
            .any(|byte| !(byte.is_ascii_alphanumeric() || b"._/-".contains(&byte)))
    {
        return false;
    }
    !branch.split('/').any(|part| {
        part.is_empty()
            || part == "."
            || part == ".."
            || part.starts_with('.')
            || part.ends_with(".lock")
    }) && !branch.contains("..")
        && !branch.contains("@{")
}

/// Held, verified repository and Git common/private directories.
pub struct VerifiedRepo {
    fs: Arc<dyn SafeFs>,
    root_path: PathBuf,
    private_path: PathBuf,
    common_path: PathBuf,
    root: Box<dyn SafeDir>,
    private: Box<dyn SafeDir>,
    common: Box<dyn SafeDir>,
    root_identity: Metadata,
    private_identity: Metadata,
    common_identity: Metadata,
    marker_identity: Metadata,
    marker_bytes: Option<Vec<u8>>,
    commondir_bytes: Option<Vec<u8>>,
    gitdir_backlink_bytes: Option<Vec<u8>>,
}

impl VerifiedRepo {
    pub fn open(fs: Arc<dyn SafeFs>, path: impl AsRef<Path>) -> Result<Self, RepoError> {
        let (root_path, root) = open_canonical_anchor(fs.as_ref(), path.as_ref(), false)?;
        let root_identity = root.metadata()?;
        if root_identity.kind != FileKind::Directory
            || root_identity.uid != effective_uid()
            || root_identity.mode & 0o022 != 0
        {
            return Err(RepoError::InvalidRoot);
        }

        let marker_identity = root.metadata_at(OsStr::new(GIT_MARKER), false)?;
        if marker_identity.mode & 0o022 != 0 {
            return Err(RepoError::InvalidGitMarker);
        }
        let (private_path, private, marker_bytes) = match marker_identity.kind {
            FileKind::Directory => {
                let private = root.open_verified_directory(OsStr::new(GIT_MARKER))?;
                if !private.metadata()?.same_identity(&marker_identity) {
                    return Err(RepoError::IdentityChanged);
                }
                (root_path.join(GIT_MARKER), private, None)
            }
            FileKind::Regular => {
                let bytes = read_bounded(root.as_ref(), OsStr::new(GIT_MARKER), GITFILE_LIMIT)?;
                let after_marker = root.metadata_at(OsStr::new(GIT_MARKER), false)?;
                if !marker_identity.same_identity(&after_marker) {
                    return Err(RepoError::IdentityChanged);
                }
                let gitdir =
                    parse_git_path(&bytes, b"gitdir: ").ok_or(RepoError::InvalidGitMarker)?;
                let (private_path, private) = open_canonical_anchor(
                    fs.as_ref(),
                    &resolve_git_path(&root_path, &gitdir)?,
                    false,
                )?;
                (private_path, private, Some(bytes))
            }
            FileKind::Symlink | FileKind::Other => return Err(RepoError::InvalidGitMarker),
        };
        let private_identity = private.metadata()?;
        verify_git_directory(private_identity, root_identity.uid)?;

        let commondir_bytes =
            read_optional_bounded(private.as_ref(), OsStr::new("commondir"), GITFILE_LIMIT)?;
        let common_candidate = if let Some(bytes) = commondir_bytes.as_deref() {
            let common = parse_git_path(bytes, b"").ok_or(RepoError::InvalidGitDirectory)?;
            resolve_git_path(&private_path, &common)?
        } else {
            private_path.clone()
        };
        let (common_path, common) = open_canonical_anchor(fs.as_ref(), &common_candidate, false)?;
        let common_identity = common.metadata()?;
        verify_git_directory(common_identity, root_identity.uid)?;

        // Git registers linked worktrees below <common>/worktrees/<id>. A
        // matching commondir/backlink outside that registry is a fabricated
        // worktree, not proof that Git owns the registration.
        if !private_identity.same_identity(&common_identity) {
            let expected_parent = common_path.join("worktrees");
            if private_path.parent() != Some(expected_parent.as_path()) {
                return Err(RepoError::InvalidRegistration);
            }
            let worktrees = common.open_verified_directory(OsStr::new("worktrees"))?;
            verify_git_directory(worktrees.metadata()?, root_identity.uid)?;
            let private_name = private_path
                .file_name()
                .ok_or(RepoError::InvalidRegistration)?;
            let registered = worktrees.open_verified_directory(private_name)?;
            if !registered.metadata()?.same_identity(&private_identity) {
                return Err(RepoError::InvalidRegistration);
            }
        }

        let gitdir_backlink_bytes =
            read_optional_bounded(private.as_ref(), OsStr::new("gitdir"), GITFILE_LIMIT)?;
        if private_identity.same_identity(&common_identity) {
            if gitdir_backlink_bytes.is_some() {
                return Err(RepoError::InvalidRegistration);
            }
        } else {
            let Some(backlink) = gitdir_backlink_bytes.as_deref() else {
                return Err(RepoError::InvalidRegistration);
            };
            let target = parse_git_path(backlink, b"").ok_or(RepoError::InvalidRegistration)?;
            let target_path = canonicalize(&resolve_git_path(&private_path, &target)?)?;
            let (target_parent_path, target_name) = split_parent(&target_path)?;
            let target_parent = fs.open_anchor(&target_parent_path)?;
            let target_identity = target_parent.metadata_at(target_name, false)?;
            if target_identity.kind != FileKind::Regular
                || !target_identity.same_identity(&marker_identity)
                || marker_bytes.is_none()
            {
                return Err(RepoError::InvalidRegistration);
            }
        }

        reject_alternates(private.as_ref())?;
        if !private_identity.same_identity(&common_identity) {
            reject_alternates(common.as_ref())?;
        }

        Ok(Self {
            fs,
            root_path,
            private_path,
            common_path,
            root,
            private,
            common,
            root_identity,
            private_identity,
            common_identity,
            marker_identity,
            marker_bytes,
            commondir_bytes,
            gitdir_backlink_bytes,
        })
    }

    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    pub fn private_git_dir(&self) -> &Path {
        &self.private_path
    }

    pub fn common_git_dir(&self) -> &Path {
        &self.common_path
    }

    pub fn root_dir(&self) -> &dyn SafeDir {
        self.root.as_ref()
    }

    pub fn private_dir(&self) -> &dyn SafeDir {
        self.private.as_ref()
    }

    pub fn common_dir(&self) -> &dyn SafeDir {
        self.common.as_ref()
    }

    pub fn root_identity(&self) -> Metadata {
        self.root_identity
    }

    pub fn private_identity(&self) -> Metadata {
        self.private_identity
    }

    pub fn common_identity(&self) -> Metadata {
        self.common_identity
    }

    pub fn revalidate(&self) -> Result<(), RepoError> {
        let current = Self::open(self.fs.clone(), &self.root_path)?;
        if !self.root_identity.same_identity(&current.root_identity)
            || !self
                .private_identity
                .same_identity(&current.private_identity)
            || !self.common_identity.same_identity(&current.common_identity)
            || !self.marker_identity.same_identity(&current.marker_identity)
            || self.marker_bytes != current.marker_bytes
            || self.commondir_bytes != current.commondir_bytes
            || self.gitdir_backlink_bytes != current.gitdir_backlink_bytes
            || self.private_path != current.private_path
            || self.common_path != current.common_path
        {
            return Err(RepoError::IdentityChanged);
        }
        Ok(())
    }
}

/// A separately held linked worktree whose gitfile/private-directory backlink
/// proves registration in the same common repository as `repo`.
pub struct VerifiedWorktree {
    repo: VerifiedRepo,
}

impl VerifiedWorktree {
    pub fn repo(&self) -> &VerifiedRepo {
        &self.repo
    }

    pub fn path(&self) -> &Path {
        self.repo.root_path()
    }

    pub fn identity(&self) -> Metadata {
        self.repo.root_identity()
    }
}

pub fn verify_worktree(
    repo: &VerifiedRepo,
    path: impl AsRef<Path>,
) -> Result<VerifiedWorktree, RepoError> {
    repo.revalidate()?;
    let worktree = VerifiedRepo::open(repo.fs.clone(), path)?;
    if !worktree
        .common_identity
        .same_identity(&repo.common_identity)
    {
        return Err(RepoError::InvalidRegistration);
    }
    if worktree
        .private_identity
        .same_identity(&worktree.common_identity)
        && !worktree.root_identity.same_identity(&repo.root_identity)
    {
        return Err(RepoError::InvalidRegistration);
    }
    worktree.revalidate()?;
    Ok(VerifiedWorktree { repo: worktree })
}

fn verify_git_directory(metadata: Metadata, owner: u32) -> Result<(), RepoError> {
    if metadata.kind != FileKind::Directory || metadata.uid != owner || metadata.mode & 0o022 != 0 {
        return Err(RepoError::InvalidGitDirectory);
    }
    Ok(())
}

fn reject_alternates(common: &dyn SafeDir) -> Result<(), RepoError> {
    let objects = match common.open_verified_directory(OsStr::new("objects")) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let info = match objects.open_verified_directory(OsStr::new("info")) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    match read_optional_bounded(info.as_ref(), OsStr::new("alternates"), ALTERNATES_LIMIT)? {
        Some(bytes) if bytes.iter().any(|byte| !byte.is_ascii_whitespace()) => {
            Err(RepoError::AlternateObjects)
        }
        _ => Ok(()),
    }
}

fn read_optional_bounded(
    directory: &dyn SafeDir,
    name: &OsStr,
    limit: u64,
) -> Result<Option<Vec<u8>>, RepoError> {
    match directory.metadata_at(name, false) {
        Ok(metadata) if metadata.kind == FileKind::Regular => {
            read_bounded(directory, name, limit).map(Some)
        }
        Ok(_) => Err(RepoError::InvalidGitDirectory),
        Err(error) if error.is_not_found() => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn read_bounded(directory: &dyn SafeDir, name: &OsStr, limit: u64) -> Result<Vec<u8>, RepoError> {
    let before = directory.metadata_at(name, false)?;
    if before.kind != FileKind::Regular {
        return Err(RepoError::InvalidGitDirectory);
    }
    let mut file = directory.open_read(name)?;
    let opened = file.metadata()?;
    if !before.same_identity(&opened) {
        return Err(RepoError::IdentityChanged);
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| {
            RepoError::Fs(FsError::Io {
                operation: "read Git metadata",
                path: PathBuf::from(name),
                source,
            })
        })?;
    if bytes.len() as u64 > limit {
        return Err(RepoError::MetadataTooLarge);
    }
    let after = directory.metadata_at(name, false)?;
    if !opened.same_identity(&after) {
        return Err(RepoError::IdentityChanged);
    }
    Ok(bytes)
}

/// A Git path file contains exactly one `gitdir: ` line, or a one-line path in
/// `commondir` / a private `gitdir` backlink. Paths remain raw Unix bytes.
fn parse_git_path(bytes: &[u8], prefix: &[u8]) -> Option<PathBuf> {
    let mut line = match bytes.strip_suffix(b"\n") {
        Some(line) => line,
        None => bytes,
    };
    if let Some(without_cr) = line.strip_suffix(b"\r") {
        line = without_cr;
    }
    if line.is_empty() || line.contains(&b'\n') || line.contains(&b'\r') || line.contains(&0) {
        return None;
    }
    let path_bytes = line.strip_prefix(prefix)?;
    if path_bytes.is_empty() {
        return None;
    }
    Some(PathBuf::from(OsString::from_vec(path_bytes.to_vec())))
}

fn resolve_git_path(base: &Path, target: &Path) -> Result<PathBuf, RepoError> {
    let joined = if target.is_absolute() {
        target.to_path_buf()
    } else {
        base.join(target)
    };
    normalize_absolute(&joined, true)
}

fn normalize_absolute(path: &Path, allow_parent: bool) -> Result<PathBuf, RepoError> {
    if !path.is_absolute() || path.as_os_str().as_bytes().contains(&0) {
        return Err(RepoError::InvalidPath);
    }
    if !allow_parent && path.components().any(|part| part == Component::ParentDir) {
        return Err(RepoError::InvalidPath);
    }
    let mut normalized = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir if allow_parent => {
                if !normalized.pop() {
                    return Err(RepoError::InvalidPath);
                }
            }
            Component::ParentDir | Component::Prefix(_) => return Err(RepoError::InvalidPath),
        }
    }
    Ok(normalized)
}

fn split_parent(path: &Path) -> Result<(PathBuf, &OsStr), RepoError> {
    let parent = path.parent().ok_or(RepoError::InvalidRegistration)?;
    let name = path.file_name().ok_or(RepoError::InvalidRegistration)?;
    if name.is_empty() || name == OsStr::new(".") || name == OsStr::new("..") {
        return Err(RepoError::InvalidRegistration);
    }
    Ok((parent.to_path_buf(), name))
}

/// Bind a caller/path-file name to a stable descriptor identity, then retain the
/// canonical name used by Executor for `cwd`. The final component must itself
/// be a directory, never a symlink; intermediate aliases are resolved once.
fn open_canonical_anchor(
    fs: &dyn SafeFs,
    path: &Path,
    allow_parent: bool,
) -> Result<(PathBuf, Box<dyn SafeDir>), RepoError> {
    let candidate = normalize_absolute(path, allow_parent)?;
    let initial = fs.open_anchor(&candidate)?;
    let before = initial.metadata()?;
    if before.kind != FileKind::Directory {
        return Err(RepoError::InvalidRoot);
    }
    let canonical = canonicalize(&candidate)?;
    let held = fs.open_anchor(&canonical)?;
    let after = held.metadata()?;
    if !before.same_identity(&after) {
        return Err(RepoError::IdentityChanged);
    }
    Ok((canonical, held))
}
