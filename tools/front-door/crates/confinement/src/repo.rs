//! Verified Git repository and linked-worktree registration anchors.
//!
//! Repository paths are retained as pathname values only for Executor's child
//! cwd. Every use reopens the path and compares it with the held
//! descriptor identity. Git metadata is read through the held `Directory`s.
use provenant_os::fs::{Directory, FileKind, FsError, Metadata, canonicalize, effective_uid};
use sha2::{Digest as _, Sha256};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

const GIT_MARKER: &str = ".git";
const GITFILE_LIMIT: u64 = 4096;
const ALTERNATES_LIMIT: u64 = 1024 * 1024;
const CONFIG_LIMIT: u64 = 1024 * 1024;

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
    #[error("repository has not passed its open-time Git probes")]
    Unprobed,
    #[error(transparent)]
    Fs(#[from] FsError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RepoObjectFormat {
    Sha1,
    Sha256,
}

/// Eagerly verified Git properties cached on an opened repository. Utility
/// operations reuse this snapshot instead of probing mutable Git config again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RepoProbeData {
    pub object_format: RepoObjectFormat,
    pub attribute_source: String,
    /// Config overrides that disable repository-configured programs.
    pub config_overrides: Vec<(String, String)>,
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

/// User-selected Git revision syntax, passed only as one `rev-parse` argument
/// after `--end-of-options` and resolved to a full commit OID before use.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GitRevision(String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[error("Git revision must be bounded printable text and cannot start with `-`")]
pub struct GitRevisionError;

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

    /// A remote-tracking ref named `remote/branch`, such as `origin/main`.
    pub fn remote_tracking(value: impl AsRef<str>) -> Result<Self, GitRefError> {
        let value = value.as_ref();
        if !valid_branch(value) || !value.contains('/') {
            return Err(GitRefError);
        }
        Ok(Self(format!("refs/remotes/{value}")))
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

impl GitRevision {
    pub fn new(value: impl AsRef<str>) -> Result<Self, GitRevisionError> {
        let value = value.as_ref();
        if value.is_empty()
            || value.len() > 1024
            || value.starts_with('-')
            || !value.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(GitRevisionError);
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Git's own rules for a branch name (`git check-ref-format --branch`), so
/// anything Git accepts is accepted here. Names reach paths only through
/// escaped path grants and reach Git only as single argv entries.
fn valid_branch(branch: &str) -> bool {
    !(branch.is_empty()
        || branch == "@"
        || branch == "HEAD"
        || branch.starts_with('-')
        || branch.ends_with('/')
        || branch.ends_with('.')
        || branch.contains("..")
        || branch.contains("@{")
        || branch
            .chars()
            .any(|character| character.is_ascii_control() || " ~^:?*[\\".contains(character))
        || branch
            .split('/')
            .any(|part| part.is_empty() || part.starts_with('.') || part.ends_with(".lock")))
}

/// Held, verified repository and Git common/private directories.
pub struct VerifiedRepo {
    root_path: PathBuf,
    private_path: PathBuf,
    common_path: PathBuf,
    root: Directory,
    private: Directory,
    common: Directory,
    root_identity: Metadata,
    private_identity: Metadata,
    common_identity: Metadata,
    marker_identity: Metadata,
    marker_bytes: Option<Vec<u8>>,
    commondir_bytes: Option<Vec<u8>>,
    gitdir_backlink_bytes: Option<Vec<u8>>,
    probe_data: Option<RepoProbeData>,
    config_digest: Option<[u8; 32]>,
    native_alternates: bool,
}

impl VerifiedRepo {
    fn open_structure(path: impl AsRef<Path>, native_alternates: bool) -> Result<Self, RepoError> {
        let (root_path, root) = open_canonical_anchor(path.as_ref(), false)?;
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
                let bytes = read_bounded(&root, OsStr::new(GIT_MARKER), GITFILE_LIMIT)?;
                let after_marker = root.metadata_at(OsStr::new(GIT_MARKER), false)?;
                if !marker_identity.same_identity(&after_marker) {
                    return Err(RepoError::IdentityChanged);
                }
                let gitdir =
                    parse_git_path(&bytes, b"gitdir: ").ok_or(RepoError::InvalidGitMarker)?;
                let (private_path, private) =
                    open_canonical_anchor(&resolve_git_path(&root_path, &gitdir)?, false)?;
                (private_path, private, Some(bytes))
            }
            FileKind::Symlink | FileKind::Other => return Err(RepoError::InvalidGitMarker),
        };
        let private_identity = private.metadata()?;
        verify_git_directory(private_identity, root_identity.uid)?;

        let commondir_bytes =
            read_optional_bounded(&private, OsStr::new("commondir"), GITFILE_LIMIT)?;
        let common_candidate = if let Some(bytes) = commondir_bytes.as_deref() {
            let common = parse_git_path(bytes, b"").ok_or(RepoError::InvalidGitDirectory)?;
            resolve_git_path(&private_path, &common)?
        } else {
            private_path.clone()
        };
        let (common_path, common) = open_canonical_anchor(&common_candidate, false)?;
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
            read_optional_bounded(&private, OsStr::new("gitdir"), GITFILE_LIMIT)?;
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
            let target_parent = Directory::open_anchor(&target_parent_path)?;
            let target_identity = target_parent.metadata_at(target_name, false)?;
            if target_identity.kind != FileKind::Regular
                || !target_identity.same_identity(&marker_identity)
                || marker_bytes.is_none()
            {
                return Err(RepoError::InvalidRegistration);
            }
        }

        if !native_alternates {
            reject_alternates(&private)?;
            if !private_identity.same_identity(&common_identity) {
                reject_alternates(&common)?;
            }
        }

        Ok(Self {
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
            probe_data: None,
            config_digest: None,
            native_alternates,
        })
    }

    /// Open and structurally verify the repository, then perform the supplied
    /// fixed Git safety probes before exposing it to callers.
    pub(crate) fn open<E>(
        path: impl AsRef<Path>,
        probe: impl FnOnce(&VerifiedRepo) -> Result<RepoProbeData, E>,
    ) -> Result<Self, E>
    where
        E: From<RepoError>,
    {
        Self::open_mode(path, false, probe)
    }

    pub(crate) fn open_mode<E>(
        path: impl AsRef<Path>,
        native_alternates: bool,
        probe: impl FnOnce(&VerifiedRepo) -> Result<RepoProbeData, E>,
    ) -> Result<Self, E>
    where
        E: From<RepoError>,
    {
        let mut repo = Self::open_structure(path, native_alternates).map_err(E::from)?;
        let config_before = repo.configuration_digest().map_err(E::from)?;
        repo.probe_data = Some(probe(&repo)?);
        let config_after = repo.configuration_digest().map_err(E::from)?;
        if config_before != config_after {
            return Err(E::from(RepoError::IdentityChanged));
        }
        repo.config_digest = Some(config_after);
        Ok(repo)
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

    pub fn root_dir(&self) -> &Directory {
        &self.root
    }

    pub fn private_dir(&self) -> &Directory {
        &self.private
    }

    pub fn common_dir(&self) -> &Directory {
        &self.common
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

    pub(crate) fn probe_data(&self) -> Option<&RepoProbeData> {
        self.probe_data.as_ref()
    }

    fn configuration_digest(&self) -> Result<[u8; 32], RepoError> {
        let mut digest = Sha256::new();
        for (marker, directory) in [(b'P', &self.private), (b'C', &self.common)] {
            for name in [OsStr::new("config"), OsStr::new("config.worktree")] {
                digest.update([marker]);
                digest.update(name.as_bytes());
                match read_optional_bounded(directory, name, CONFIG_LIMIT)? {
                    Some(bytes) => {
                        digest.update([1]);
                        digest.update((bytes.len() as u64).to_be_bytes());
                        digest.update(bytes);
                    }
                    None => digest.update([0]),
                }
            }
        }
        Ok(digest.finalize().into())
    }

    pub fn revalidate(&self) -> Result<(), RepoError> {
        let current = Self::open_structure(&self.root_path, self.native_alternates)?;
        let Some(config_digest) = self.config_digest else {
            return Err(RepoError::Unprobed);
        };
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
            || config_digest != current.configuration_digest()?
        {
            return Err(RepoError::IdentityChanged);
        }
        Ok(())
    }
}

/// Read-only Git evidence capability. Native alternates belong to Git's object
/// resolution contract; this wrapper cannot be passed to any mutation service.
pub struct ReadOnlyRepo(pub(crate) VerifiedRepo);
impl ReadOnlyRepo {
    pub fn common_git_dir(&self) -> &Path {
        self.0.common_git_dir()
    }
    pub fn root_path(&self) -> &Path {
        self.0.root_path()
    }
    pub fn root_dir(&self) -> &Directory {
        self.0.root_dir()
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
    let mut worktree = VerifiedRepo::open_structure(path, false)?;
    if repo.probe_data.is_none() || repo.config_digest.is_none() {
        return Err(RepoError::Unprobed);
    }
    if !worktree
        .common_identity
        .same_identity(&repo.common_identity)
    {
        return Err(RepoError::InvalidRegistration);
    }
    if worktree
        .private_identity
        .same_identity(&worktree.common_identity)
    {
        return Err(RepoError::InvalidRegistration);
    }
    worktree.probe_data = repo.probe_data.clone();
    worktree.config_digest = Some(worktree.configuration_digest()?);
    worktree.revalidate()?;
    Ok(VerifiedWorktree { repo: worktree })
}

fn verify_git_directory(metadata: Metadata, owner: u32) -> Result<(), RepoError> {
    if metadata.kind != FileKind::Directory || metadata.uid != owner || metadata.mode & 0o022 != 0 {
        return Err(RepoError::InvalidGitDirectory);
    }
    Ok(())
}

fn reject_alternates(common: &Directory) -> Result<(), RepoError> {
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
    match read_optional_bounded(&info, OsStr::new("alternates"), ALTERNATES_LIMIT)? {
        Some(bytes) if bytes.iter().any(|byte| !byte.is_ascii_whitespace()) => {
            Err(RepoError::AlternateObjects)
        }
        _ => Ok(()),
    }
}

fn read_optional_bounded(
    directory: &Directory,
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

fn read_bounded(directory: &Directory, name: &OsStr, limit: u64) -> Result<Vec<u8>, RepoError> {
    crate::stable_read::read_regular(directory, name, limit)
        .map(|(bytes, _)| bytes)
        .map_err(|error| match error {
            crate::stable_read::ReadError::Unsupported => RepoError::InvalidGitDirectory,
            crate::stable_read::ReadError::TooLarge => RepoError::MetadataTooLarge,
            crate::stable_read::ReadError::Changed => RepoError::IdentityChanged,
            crate::stable_read::ReadError::Filesystem(error) => RepoError::Fs(error),
            crate::stable_read::ReadError::Io(source) => RepoError::Fs(FsError::Io {
                operation: "read Git metadata",
                path: PathBuf::from(name),
                source,
            }),
        })
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
    normalise_absolute(&joined, true)
}

fn normalise_absolute(path: &Path, allow_parent: bool) -> Result<PathBuf, RepoError> {
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
    path: &Path,
    allow_parent: bool,
) -> Result<(PathBuf, Directory), RepoError> {
    let candidate = normalise_absolute(path, allow_parent)?;
    let initial = Directory::open_anchor(&candidate)?;
    let before = initial.metadata()?;
    if before.kind != FileKind::Directory {
        return Err(RepoError::InvalidRoot);
    }
    let canonical = canonicalize(&candidate)?;
    let held = Directory::open_anchor(&canonical)?;
    let after = held.metadata()?;
    if !before.same_identity(&after) {
        return Err(RepoError::IdentityChanged);
    }
    Ok((canonical, held))
}

#[cfg(test)]
mod tests {
    use super::{
        GitRefName, GitRevision, GitRevisionError, RepoError, RepoObjectFormat, RepoProbeData,
        VerifiedRepo, verify_worktree,
    };
    use std::fs;

    #[test]
    fn branch_names_follow_git_ref_format() {
        for name in [
            "feat/a+b",
            "fix#12",
            "user@x",
            "caf\u{e9}",
            "a\"b",
            "x/y.z",
            "v1.0",
        ] {
            assert!(GitRefName::branch(name).is_ok(), "{name}");
        }
        for name in [
            "", "@", "HEAD", "-x", "/x", "x/", "x//y", "x.", ".x", "x/.y", "a..b", "a@{b", "a b",
            "a~b", "a^b", "a:b", "a?b", "a*b", "a[b", "a\\b", "a\u{7f}b", "a\nb", "x.lock",
            "x.lock/y",
        ] {
            assert!(GitRefName::branch(name).is_err(), "{name:?}");
        }
    }

    #[test]
    fn verify_worktree_refuses_the_primary_checkout() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();
        let repo = VerifiedRepo::open(temp.path(), |_| {
            Ok::<_, RepoError>(RepoProbeData {
                object_format: RepoObjectFormat::Sha1,
                attribute_source: String::new(),
                config_overrides: Vec::new(),
            })
        })
        .unwrap();

        assert!(matches!(
            verify_worktree(&repo, temp.path()),
            Err(RepoError::InvalidRegistration)
        ));
    }

    #[test]
    fn git_revision_accepts_frozen_base_refs_and_refuses_option_injection() {
        assert_eq!(
            GitRevision::new("origin/main").unwrap().as_str(),
            "origin/main"
        );
        assert_eq!(GitRevision::new("HEAD~2").unwrap().as_str(), "HEAD~2");
        assert!(matches!(
            GitRevision::new("--upload-pack=evil"),
            Err(GitRevisionError)
        ));
        assert!(matches!(
            GitRevision::new("origin/main\nHEAD"),
            Err(GitRevisionError)
        ));
    }

    #[test]
    fn repo_revalidation_detects_config_changes_without_a_second_git_probe() {
        let temp = tempfile::tempdir().unwrap();
        let git = temp.path().join(".git");
        fs::create_dir(&git).unwrap();
        fs::write(git.join("config"), b"[core]\nrepositoryformatversion = 0\n").unwrap();
        let repo = VerifiedRepo::open(temp.path(), |_| {
            Ok::<_, RepoError>(RepoProbeData {
                object_format: RepoObjectFormat::Sha1,
                attribute_source: String::new(),
                config_overrides: Vec::new(),
            })
        })
        .unwrap();
        fs::write(
            git.join("config"),
            b"[filter.poison]\nclean = touch /tmp/pwned\n",
        )
        .unwrap();

        assert!(matches!(repo.revalidate(), Err(RepoError::IdentityChanged)));
    }
}
