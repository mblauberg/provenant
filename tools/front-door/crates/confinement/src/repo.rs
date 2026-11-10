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
    #[error(
        "worktree {} belongs to project {}, not the selected project {}",
        worktree.display(),
        project.display(),
        selected.display()
    )]
    ForeignProject {
        worktree: PathBuf,
        project: PathBuf,
        selected: PathBuf,
    },
    #[error("Git common or private directory is unsafe")]
    InvalidGitDirectory,
    #[error("Git configuration normalisation could not execute: {0}")]
    ConfigNormalisationExecution(#[source] provenant_os::exec::ExecError),
    #[error("Git configuration normalisation failed: {0}")]
    ConfigNormalisation(&'static str),
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

impl RepoError {
    /// The error's fixed text, for a variant whose text names no path, value
    /// or source error. Edges may disclose it as the cause of a failure.
    pub fn safe_cause(&self) -> Option<&'static str> {
        Some(match self {
            Self::InvalidPath => "repository path must be absolute and contain no parent traversal",
            Self::InvalidRoot => "repository root is not a private owner-controlled directory",
            Self::InvalidGitMarker => "repository has no valid .git directory or gitfile",
            Self::InvalidRegistration => "Git private directory is not registered to this worktree",
            Self::InvalidGitDirectory => "Git common or private directory is unsafe",
            Self::AlternateObjects => {
                "alternate Git object stores are not accepted for protected history"
            }
            Self::IdentityChanged => "verified repository path or Git metadata changed",
            Self::MetadataTooLarge => "Git metadata file exceeds its bound",
            Self::Unprobed => "repository has not passed its open-time Git probes",
            Self::ForeignProject { .. }
            | Self::ConfigNormalisationExecution(_)
            | Self::ConfigNormalisation(_)
            | Self::Fs(_) => return None,
        })
    }

    pub(crate) fn variant(&self) -> &'static str {
        match self {
            Self::InvalidPath => "InvalidPath",
            Self::InvalidRoot => "InvalidRoot",
            Self::InvalidGitMarker => "InvalidGitMarker",
            Self::InvalidRegistration => "InvalidRegistration",
            Self::ForeignProject { .. } => "ForeignProject",
            Self::InvalidGitDirectory => "InvalidGitDirectory",
            Self::ConfigNormalisationExecution(_) => "ConfigNormalisationExecution",
            Self::ConfigNormalisation(_) => "ConfigNormalisation",
            Self::AlternateObjects => "AlternateObjects",
            Self::IdentityChanged => "IdentityChanged",
            Self::MetadataTooLarge => "MetadataTooLarge",
            Self::Unprobed => "Unprobed",
            Self::Fs(_) => "Fs",
        }
    }
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
    /// Config overrides that neutralise configuration lanes can influence.
    pub config_overrides: Vec<(String, String)>,
    /// Files Git read through `include.path` and `includeIf`, plus every
    /// declared include target, present or not. The configuration digest
    /// covers them, so a later edit makes the handle stale.
    pub config_files: Vec<PathBuf>,
    /// Digest of `config_files` taken when they were read.
    pub config_files_digest: [u8; 32],
    /// A declared include target in a lane-writable location was not read
    /// because its condition (such as `onbranch:`) did not match this HEAD. A
    /// later HEAD change could activate it, so the digest also covers `HEAD`.
    pub config_head_sensitive: bool,
}

#[cfg(test)]
impl RepoProbeData {
    /// Probe data for a repository whose configuration includes no files.
    pub(crate) fn without_includes(
        object_format: RepoObjectFormat,
        attribute_source: String,
        config_overrides: Vec<(String, String)>,
    ) -> Self {
        Self {
            object_format,
            attribute_source,
            config_overrides,
            config_files: Vec::new(),
            config_files_digest: Sha256::digest(Sha256::digest([])).into(),
            config_head_sensitive: false,
        }
    }
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

/// A strictly validated full local branch ref. The name must satisfy Git's own
/// branch-name rules, and Git is never asked to interpret caller-supplied
/// revision syntax.
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
/// escaped path grants and reach Git only as single argv entries. Git accepts a
/// lone `@` (`refs/heads/@` is a valid ref; only the bare ref name `@` is
/// reserved), and rejects `HEAD` and any `@{` sequence.
fn valid_branch(branch: &str) -> bool {
    !(branch.is_empty()
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
    config_digest: Option<ConfigDigest>,
    native_alternates: bool,
}

/// Digest of the repository's own config files, kept apart from the digest of
/// the files they include so a probe can tell which of the two moved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConfigDigest {
    base: [u8; 32],
    pub(crate) included: [u8; 32],
    common_config: Option<[u8; 32]>,
    config_head: Vec<u8>,
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

    /// Open the common directory using structural checks only, without reading
    /// config. Callers can acquire the setup lock before opening a probed repo;
    /// they must compare its common identity with this directory under the lock.
    pub fn open_common_directory(path: impl AsRef<Path>) -> Result<Directory, RepoError> {
        Ok(Self::open_structure(path, false)?.common)
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
        let config_before = repo.configuration_digest(None).map_err(E::from)?;
        let data = probe(&repo)?;
        repo.probe_data = Some(data);
        // The probe records the digest of the included files it read; the
        // repository's own config files must not have changed meanwhile.
        let config_after = repo
            .configuration_digest(Some(&config_before.config_head))
            .map_err(E::from)?;
        let included = repo
            .probe_data
            .as_ref()
            .map(|data| data.config_files_digest);
        if config_before.base != config_after.base
            || config_before.common_config != config_after.common_config
            || included != Some(config_after.included)
        {
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

    fn configuration_digest(
        &self,
        verified_head: Option<&[u8]>,
    ) -> Result<ConfigDigest, RepoError> {
        let mut digest = Sha256::new();
        for (marker, directory) in [(b'P', &self.private), (b'C', &self.common)] {
            for name in [OsStr::new("config"), OsStr::new("config.worktree")] {
                if name == OsStr::new("config")
                    && (marker == b'C'
                        || self.private_identity.same_identity(&self.common_identity))
                {
                    continue;
                }
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
        let (files, head_sensitive) = self.probe_data.as_ref().map_or((&[][..], false), |data| {
            (data.config_files.as_slice(), data.config_head_sensitive)
        });
        let head = match verified_head {
            Some(head) => head.to_vec(),
            // A missing or malformed HEAD is reported by the caller's own HEAD
            // read with its path; here it only means no branch is checked out.
            None => self.config_head(),
        };
        Ok(ConfigDigest {
            config_head: head.clone(),
            base: digest.finalize().into(),
            included: self.included_digest(files, head_sensitive)?,
            common_config: common_config_digest(&self.common, &head)?,
        })
    }

    /// The private `HEAD` bytes that decide which branch's config keys apply.
    /// Empty when the file is missing or unreadable, which keeps every key.
    pub(crate) fn config_head(&self) -> Vec<u8> {
        read_optional_bounded(&self.private, OsStr::new("HEAD"), GITFILE_LIMIT)
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    /// Digest of included config files, for a probe that has just read them.
    pub(crate) fn included_digest(
        &self,
        files: &[PathBuf],
        head_sensitive: bool,
    ) -> Result<[u8; 32], RepoError> {
        let mut digest = Sha256::new();
        digest.update(included_files_digest(files)?);
        if head_sensitive {
            match read_optional_bounded(&self.private, OsStr::new("HEAD"), GITFILE_LIMIT)? {
                Some(bytes) => {
                    digest.update([1]);
                    digest.update(bytes);
                }
                None => digest.update([0]),
            }
        }
        Ok(digest.finalize().into())
    }

    /// Replace the probe data after probing this repository's own effective
    /// configuration, and re-anchor the digest on it.
    pub(crate) fn set_probe_data(&mut self, data: RepoProbeData) -> Result<(), RepoError> {
        self.probe_data = Some(data);
        self.config_digest = Some(self.configuration_digest(None)?);
        self.revalidate()
    }

    pub fn revalidate(&self) -> Result<(), RepoError> {
        let mut current = Self::open_structure(&self.root_path, self.native_alternates)?;
        self.check_identity(&current)?;
        // The included files to digest are the ones this handle was probed on.
        current.probe_data = self.probe_data.clone();
        let Some(config_digest) = &self.config_digest else {
            return Err(RepoError::Unprobed);
        };
        let current_config = current.configuration_digest(Some(&config_digest.config_head))?;
        if config_digest.base != current_config.base
            || config_digest.included != current_config.included
            || config_digest.common_config != current_config.common_config
        {
            return Err(RepoError::IdentityChanged);
        }
        Ok(())
    }

    /// Check the held worktree's structural identity before a potentially slow
    /// setup. This does not validate Git configuration and cannot authorise a
    /// Git operation or replace the full revalidation at the point of use.
    pub fn revalidate_identity(&self) -> Result<(), RepoError> {
        let current = Self::open_structure(&self.root_path, self.native_alternates)?;
        self.check_identity(&current)
    }

    fn check_identity(&self, current: &Self) -> Result<(), RepoError> {
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

/// Read-only Git evidence capability. Native alternates belong to Git's object
/// resolution contract; this wrapper cannot be passed to any mutation service.
pub struct ReadOnlyRepo(pub(crate) VerifiedRepo);
impl ReadOnlyRepo {
    /// Verified metadata directories, for host-side read-only snapshots.
    pub fn common_dir(&self) -> &Directory {
        self.0.common_dir()
    }
    pub fn private_dir(&self) -> &Directory {
        self.0.private_dir()
    }

    /// Recheck the repository identity and configuration before using held paths.
    pub fn revalidate(&self) -> Result<(), RepoError> {
        self.0.revalidate()
    }

    pub fn private_git_dir(&self) -> &Path {
        self.0.private_git_dir()
    }
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

    pub(crate) fn repo_mut(&mut self) -> &mut VerifiedRepo {
        &mut self.repo
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
        // Only the common Git directory names the other project. Its parent
        // is the checkout for a `.git` directory; a separate Git directory is
        // named as it is.
        let common = worktree.common_git_dir();
        let project = common
            .parent()
            .filter(|_| common.file_name() == Some(std::ffi::OsStr::new(".git")))
            .unwrap_or(common)
            .to_path_buf();
        return Err(RepoError::ForeignProject {
            worktree: worktree.root_path().to_path_buf(),
            project,
            selected: repo.root_path().to_path_buf(),
        });
    }
    if worktree
        .private_identity
        .same_identity(&worktree.common_identity)
    {
        return Err(RepoError::InvalidRegistration);
    }
    worktree.probe_data = repo.probe_data.clone();
    worktree.config_digest = Some(worktree.configuration_digest(None)?);
    worktree.revalidate()?;
    Ok(VerifiedWorktree { repo: worktree })
}

/// Hash each file with the path it resolves to, so retargeting a symlink or
/// creating a file that was absent changes the digest. Git follows links in
/// includes, so a link is read through.
fn included_files_digest(files: &[PathBuf]) -> Result<[u8; 32], RepoError> {
    let mut digest = Sha256::new();
    for file in files {
        digest.update(file.as_os_str().as_bytes());
        digest.update([0]);
        let Ok(resolved) = canonicalize(file) else {
            digest.update([0]);
            continue;
        };
        let (Some(parent), Some(name)) = (resolved.parent(), resolved.file_name()) else {
            return Err(RepoError::InvalidGitDirectory);
        };
        let directory = match Directory::open_anchor(parent) {
            Ok(directory) => directory,
            Err(error) if error.is_not_found() => {
                digest.update([0]);
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        digest.update([1]);
        digest.update(resolved.as_os_str().as_bytes());
        match read_optional_bounded(&directory, name, CONFIG_LIMIT)? {
            Some(bytes) => {
                digest.update([1]);
                digest.update((bytes.len() as u64).to_be_bytes());
                digest.update(bytes);
            }
            None => digest.update([0]),
        }
    }
    Ok(digest.finalize().into())
}

/// Other branches' tracking keys (`branch.<name>.remote` and `.merge`) in
/// the common config. Sibling writers set them for their own lanes while this
/// checkout is open, so they cannot change what this checkout's Git reads.
/// A checked-out branch keeps its own keys; a detached HEAD keeps none. A
/// missing or malformed HEAD keeps every branch's keys, so a HEAD race cannot
/// hide a tracking change; the writer's own HEAD read reports the damage with
/// its path.
pub(crate) struct SiblingTrackingKeys<'a> {
    branch: Option<&'a [u8]>,
    keep_all: bool,
}

impl<'a> SiblingTrackingKeys<'a> {
    /// `head` is the raw bytes of the checkout's private `HEAD` file.
    pub(crate) fn for_head(head: &'a [u8]) -> Self {
        let line = head.strip_suffix(b"\n").unwrap_or_default();
        let detached = matches!(line.len(), 40 | 64) && line.iter().all(u8::is_ascii_hexdigit);
        let branch = line.strip_prefix(b"ref: refs/heads/").filter(|branch| {
            std::str::from_utf8(branch).is_ok_and(|name| GitRefName::branch(name).is_ok())
        });
        Self {
            branch,
            keep_all: branch.is_none() && !detached,
        }
    }

    /// Whether `key`, as Git lists it, is another branch's tracking key.
    pub(crate) fn matches(&self, key: &[u8]) -> bool {
        key.strip_prefix(b"branch.")
            .and_then(|rest| {
                rest.strip_suffix(b".remote")
                    .or_else(|| rest.strip_suffix(b".merge"))
            })
            .is_some_and(|name| !self.keep_all && Some(name) != self.branch)
    }
}

// Git parses the bounded snapshot; included files have their own digest.
fn common_config_digest(common: &Directory, head: &[u8]) -> Result<Option<[u8; 32]>, RepoError> {
    let Some(bytes) = read_common_config(common)? else {
        return Ok(None);
    };
    let ignored = SiblingTrackingKeys::for_head(head);
    let git = provenant_os::git::git_executable()
        .ok_or(RepoError::ConfigNormalisation("Git executable unavailable"))?;
    let normalised = COMMON_CONFIG_NORMALISATION
        .normalise(&git, &bytes, || parse_common_config(&git, &bytes))?;
    let mut digest = Sha256::new();
    for entry in normalised
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let key = entry
            .split(|byte| *byte == b'\n')
            .next()
            .unwrap_or_default();
        if !ignored.matches(key) {
            digest.update((entry.len() as u64).to_be_bytes());
            digest.update(entry);
        }
    }
    Ok(Some(digest.finalize().into()))
}

// One bounded successful listing, shared across repeated snapshots and HEADs.
// Parsing runs outside the mutex: a slow miss must not queue unrelated readers.
// Concurrent misses may parse the same snapshot more than once.
static COMMON_CONFIG_NORMALISATION: ConfigNormalisationCache = ConfigNormalisationCache {
    entry: std::sync::Mutex::new(None),
};

#[derive(Default)]
struct ConfigNormalisationCache {
    entry: std::sync::Mutex<Option<NormalisedConfig>>,
}

struct NormalisedConfig {
    git: PathBuf,
    executable: Metadata,
    snapshot: Vec<u8>,
    listing: Vec<u8>,
}

impl ConfigNormalisationCache {
    fn normalise(
        &self,
        git: &Path,
        bytes: &[u8],
        parse: impl FnOnce() -> Result<Vec<u8>, RepoError>,
    ) -> Result<Vec<u8>, RepoError> {
        if bytes.len() > CONFIG_LIMIT as usize {
            return Err(RepoError::MetadataTooLarge);
        }
        let executable = config_parser_metadata(git)?;
        {
            let cached = self
                .entry
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if let Some(entry) = &*cached
                && entry.git == git
                && entry.executable.unchanged(&executable)
                && entry.snapshot == bytes
            {
                return Ok(entry.listing.clone());
            }
        }
        let listing = parse()?;
        if listing.len() > CONFIG_LIMIT as usize * 2 {
            return Err(RepoError::ConfigNormalisation(
                "parser output exceeded its bound",
            ));
        }
        if !executable.unchanged(&config_parser_metadata(git)?) {
            return Err(RepoError::ConfigNormalisation(
                "Git executable changed during parsing",
            ));
        }
        *self
            .entry
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(NormalisedConfig {
            git: git.to_path_buf(),
            executable,
            snapshot: bytes.to_vec(),
            listing: listing.clone(),
        });
        Ok(listing)
    }
}

fn config_parser_metadata(git: &Path) -> Result<Metadata, RepoError> {
    std::fs::metadata(git)
        .map(Metadata::from_std)
        .map_err(|source| {
            RepoError::ConfigNormalisationExecution(provenant_os::exec::ExecError::Io {
                operation: "stat config parser",
                source,
            })
        })
}

fn parse_common_config(git: &Path, bytes: &[u8]) -> Result<Vec<u8>, RepoError> {
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor};
    let argv = [
        git.as_os_str().to_owned(),
        "config".into(),
        "--file".into(),
        "-".into(),
        // Git refuses relative includes from stdin; included files have their
        // own digest, so the snapshot is parsed without following them.
        "--no-includes".into(),
        "--null".into(),
        "--list".into(),
    ];
    // Parsing needs no repository; running outside the worktree keeps Git's
    // discovery from failing on the damaged metadata this check exists to report.
    let mut options = ExecOptions::new(&argv, Path::new("/"), std::time::Duration::from_secs(2));
    options.stdin = Some(bytes);
    options.env = Some(&[]);
    options.stdout = StreamPolicy::Capture {
        limit: CONFIG_LIMIT as usize * 2,
    };
    options.stop_on_output_limit = true;
    normalised_config_output(SystemExecutor::new(1).and_then(|executor| executor.execute(options)))
}

// Operational parser failures say nothing about the directory's ownership,
// permissions or kind. Keep their cause distinct while still refusing the read.
fn normalised_config_output(
    output: Result<provenant_os::exec::ExecOutcome, provenant_os::exec::ExecError>,
) -> Result<Vec<u8>, RepoError> {
    use provenant_os::exec::Termination;
    let output = output.map_err(RepoError::ConfigNormalisationExecution)?;
    let reason = if output.timed_out {
        "parser timed out"
    } else if output.stdout.read_failed {
        "parser output read failed"
    } else if output.stdout.truncated {
        "parser output exceeded its bound"
    } else if output.drain_truncated {
        "parser output drain did not complete"
    } else if output.termination != Termination::Exit(0) {
        "parser exited unsuccessfully"
    } else {
        return Ok(output.stdout.bytes);
    };
    Err(RepoError::ConfigNormalisation(reason))
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
        .map_err(|error| git_read_error(error, name))
}

fn git_read_error(error: crate::stable_read::ReadError, name: &OsStr) -> RepoError {
    match error {
        crate::stable_read::ReadError::Unsupported => RepoError::InvalidGitDirectory,
        crate::stable_read::ReadError::TooLarge => RepoError::MetadataTooLarge,
        crate::stable_read::ReadError::Changed => RepoError::IdentityChanged,
        crate::stable_read::ReadError::Filesystem(error) => RepoError::Fs(error),
        crate::stable_read::ReadError::Io(source) => RepoError::Fs(FsError::Io {
            operation: "read Git metadata",
            path: PathBuf::from(name),
            source,
        }),
    }
}

fn read_common_config(directory: &Directory) -> Result<Option<Vec<u8>>, RepoError> {
    let name = OsStr::new("config");
    // Git replaces this shared file atomically. A rename during a stable read
    // is transient; still compare the eventual bytes against the snapshot.
    for attempt in 0..3 {
        match crate::stable_read::read_regular(directory, name, CONFIG_LIMIT) {
            Ok((bytes, _)) => return Ok(Some(bytes)),
            Err(crate::stable_read::ReadError::Changed) if attempt < 2 => continue,
            Err(crate::stable_read::ReadError::Filesystem(error)) if error.is_not_found() => {
                return Ok(None);
            }
            Err(error) => return Err(git_read_error(error, name)),
        }
    }
    unreachable!("the last stable read always returns")
}

/// A Git path file contains exactly one `gitdir: ` line, or a one-line path in
/// `commondir` / a private `gitdir` backlink. Paths remain raw Unix bytes.
pub(crate) fn parse_git_path(bytes: &[u8], prefix: &[u8]) -> Option<PathBuf> {
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

pub(crate) fn resolve_git_path(base: &Path, target: &Path) -> Result<PathBuf, RepoError> {
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
        SiblingTrackingKeys, VerifiedRepo, verify_worktree,
    };
    use std::fs;

    /// Each row is `git check-ref-format --branch <name>` (git-check-ref-format(1),
    /// "--branch"), observed with Git 2.53: true when it exits 0. `@{` forms
    /// are excluded because `--branch` treats them as shorthand to expand.
    const BRANCH_TABLE: &[(&str, bool)] = &[
        ("feat/a+b", true),
        ("fix#12", true),
        ("user@x", true),
        ("@", true),
        ("@a", true),
        ("a@", true),
        ("a/@", true),
        ("@/a", true),
        ("@@", true),
        ("head", true),
        ("caf\u{e9}", true),
        ("a\"b", true),
        ("a{b", true),
        ("a}b", true),
        ("a.lock.b", true),
        ("lock", true),
        ("x/y.z", true),
        ("v1.0", true),
        ("a/-b", true),
        ("", false),
        ("HEAD", false),
        ("-x", false),
        ("--x", false),
        ("/x", false),
        ("x/", false),
        ("x//y", false),
        (".", false),
        ("..", false),
        (".x", false),
        ("x/.y", false),
        ("x.", false),
        ("a/b.", false),
        ("a..b", false),
        ("a@{b", false),
        ("a b", false),
        ("a~b", false),
        ("a^b", false),
        ("a:b", false),
        ("a?b", false),
        ("a*b", false),
        ("a[b", false),
        ("a\\b", false),
        ("a\u{7f}b", false),
        ("a\nb", false),
        ("a\tb", false),
        ("x.lock", false),
        ("x.lock/y", false),
        ("a/b.lock", false),
        (".lock", false),
        ("a/..", false),
    ];

    #[test]
    fn sibling_tracking_keys_follow_the_checked_out_head() {
        let branch = SiblingTrackingKeys::for_head(b"ref: refs/heads/main\n");
        assert!(branch.matches(b"branch.lane.remote"));
        assert!(branch.matches(b"branch.lane.merge"));
        assert!(!branch.matches(b"branch.main.remote"));
        assert!(!branch.matches(b"remote.origin.url"));
        let detached = format!("{}\n", "a".repeat(40));
        assert!(SiblingTrackingKeys::for_head(detached.as_bytes()).matches(b"branch.main.merge"));
        let malformed = SiblingTrackingKeys::for_head(b"garbage\n");
        assert!(!malformed.matches(b"branch.lane.remote"));
        assert!(!SiblingTrackingKeys::for_head(b"").matches(b"branch.lane.remote"));
    }

    #[test]
    fn branch_names_follow_git_ref_format() {
        for (name, accepted) in BRANCH_TABLE {
            assert_eq!(GitRefName::branch(name).is_ok(), *accepted, "{name:?}");
        }
    }

    /// The table above can drift from Git; ask the installed Git directly.
    #[test]
    fn branch_names_match_the_installed_git() {
        let Some(git) = provenant_os::git::git_executable() else {
            return;
        };
        let temp = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            provenant_os::fixture::git_command_at(&git)
                .current_dir(temp.path())
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .args(args)
                .output()
                .unwrap()
        };
        assert!(run(&["init", "--quiet"]).status.success());
        for (name, accepted) in BRANCH_TABLE {
            let output = run(&["check-ref-format", "--branch", name]);
            assert_eq!(output.status.success(), *accepted, "{name:?}");
        }
    }

    #[test]
    fn verify_worktree_refuses_the_primary_checkout() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();
        fs::write(temp.path().join(".git/HEAD"), b"ref: refs/heads/main\n").unwrap();
        let repo = VerifiedRepo::open(temp.path(), |_| {
            Ok::<_, RepoError>(RepoProbeData::without_includes(
                RepoObjectFormat::Sha1,
                String::new(),
                Vec::new(),
            ))
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
    fn normalisation_reuses_only_the_exact_successful_snapshot_and_executable() {
        use std::cell::Cell;
        let temp = tempfile::tempdir().unwrap();
        let git = temp.path().join("git");
        let other_git = temp.path().join("other-git");
        fs::write(&git, b"parser").unwrap();
        fs::hard_link(&git, &other_git).unwrap();
        let calls = Cell::new(0);
        let parse = || {
            calls.set(calls.get() + 1);
            Ok(b"core.bare\nfalse\0".to_vec())
        };
        let cache = super::ConfigNormalisationCache::default();
        let first = cache.normalise(&git, b"first", parse).unwrap();
        assert_eq!(cache.normalise(&git, b"first", parse).unwrap(), first);
        assert_eq!(calls.get(), 1);
        cache.normalise(&git, b"second", parse).unwrap();
        // A single-entry cache evicts the first snapshot.
        cache.normalise(&git, b"first", parse).unwrap();
        assert_eq!(calls.get(), 3);
        cache.normalise(&other_git, b"first", parse).unwrap();
        assert_eq!(calls.get(), 4);
        fs::remove_file(&other_git).unwrap();
        fs::write(&other_git, b"replacement parser").unwrap();
        cache.normalise(&other_git, b"first", parse).unwrap();
        assert_eq!(calls.get(), 5);
        for _ in 0..2 {
            assert!(matches!(
                cache.normalise(&other_git, b"changed", || {
                    calls.set(calls.get() + 1);
                    Err(RepoError::ConfigNormalisation("parser timed out"))
                }),
                Err(RepoError::ConfigNormalisation(_))
            ));
        }
        assert_eq!(calls.get(), 7);
        // A parser replacement during a miss must not publish under its old identity.
        assert!(matches!(
            cache.normalise(&other_git, b"changed", || {
                fs::rename(&other_git, temp.path().join("old-git")).unwrap();
                fs::write(&other_git, b"new parser").unwrap();
                parse()
            }),
            Err(RepoError::ConfigNormalisation(_))
        ));
        cache.normalise(&other_git, b"changed", parse).unwrap();
        assert_eq!(calls.get(), 9);
        assert!(matches!(
            cache.normalise(&git, &vec![0; super::CONFIG_LIMIT as usize + 1], parse),
            Err(RepoError::MetadataTooLarge)
        ));
        assert_eq!(calls.get(), 9);
        assert!(matches!(
            cache.normalise(&git, b"oversized", || Ok(vec![
                0;
                super::CONFIG_LIMIT as usize
                    * 2
                    + 1
            ])),
            Err(RepoError::ConfigNormalisation(_))
        ));
        cache.normalise(&git, b"oversized", parse).unwrap();
        assert_eq!(calls.get(), 10);
    }

    #[test]
    fn concurrent_cache_misses_do_not_hold_the_cache_lock() {
        use std::sync::mpsc;
        use std::time::Duration;
        let temp = tempfile::tempdir().unwrap();
        let git = temp.path().join("git");
        fs::write(&git, b"parser").unwrap();
        let cache = super::ConfigNormalisationCache::default();
        let (entered, parsing) = mpsc::channel();
        std::thread::scope(|scope| {
            let mut releases = Vec::new();
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let (release, wait) = mpsc::channel();
                    releases.push(release);
                    let entered = entered.clone();
                    let cache = &cache;
                    let git = &git;
                    scope.spawn(move || {
                        cache
                            .normalise(git, b"snapshot", || {
                                entered.send(()).unwrap();
                                wait.recv_timeout(Duration::from_secs(5)).unwrap();
                                Ok(b"core.bare\nfalse\0".to_vec())
                            })
                            .unwrap()
                    })
                })
                .collect();
            // A blocked parser must not keep other misses from entering. Release
            // every worker before asserting, so a regression fails without hanging.
            let all_entered = (0..8).all(|_| parsing.recv_timeout(Duration::from_secs(2)).is_ok());
            for release in releases {
                let _ = release.send(());
            }
            for handle in handles {
                assert_eq!(handle.join().unwrap(), b"core.bare\nfalse\0");
            }
            assert!(all_entered, "cache readers queued behind a parser");
        });
        assert_eq!(
            cache
                .normalise(&git, b"snapshot", || panic!("warm cache miss"))
                .unwrap(),
            b"core.bare\nfalse\0"
        );
    }

    #[test]
    fn warm_normalisation_still_refuses_unsafe_git_metadata() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let git = temp.path().join(".git");
        fs::create_dir(&git).unwrap();
        fs::write(git.join("config"), b"[core]\nrepositoryformatversion = 0\n").unwrap();
        fs::write(git.join("HEAD"), b"ref: refs/heads/main\n").unwrap();
        let repo = VerifiedRepo::open(temp.path(), |_| {
            Ok::<_, RepoError>(RepoProbeData::without_includes(
                RepoObjectFormat::Sha1,
                String::new(),
                Vec::new(),
            ))
        })
        .unwrap();
        fs::hard_link(git.join("config"), git.join("alias")).unwrap();
        assert!(matches!(
            repo.revalidate(),
            Err(RepoError::InvalidGitDirectory)
        ));
        fs::remove_file(git.join("alias")).unwrap();
        fs::rename(git.join("config"), git.join("saved-config")).unwrap();
        std::os::unix::fs::symlink("saved-config", git.join("config")).unwrap();
        assert!(matches!(
            repo.revalidate(),
            Err(RepoError::InvalidGitDirectory)
        ));
        fs::remove_file(git.join("config")).unwrap();
        fs::rename(git.join("saved-config"), git.join("config")).unwrap();
        fs::set_permissions(&git, fs::Permissions::from_mode(0o720)).unwrap();
        assert!(matches!(
            repo.revalidate(),
            Err(RepoError::InvalidGitMarker)
        ));
    }

    #[test]
    fn normalisation_failures_refuse_without_claiming_unsafe_metadata() {
        use provenant_os::exec::{CapturedStream, ExecError, ExecOutcome, Termination};
        let complete = ExecOutcome {
            termination: Termination::Exit(0),
            stdout: CapturedStream {
                bytes: b"core.bare\nfalse\0".to_vec(),
                ..Default::default()
            },
            stderr: CapturedStream::default(),
            timed_out: false,
            drain_truncated: false,
            overflow_stopped: false,
            cancelled: false,
            elapsed_micros: 1,
        };
        assert_eq!(
            super::normalised_config_output(Ok(complete.clone())).unwrap(),
            complete.stdout.bytes
        );
        let mut failures = Vec::new();
        let mut timed_out = complete.clone();
        timed_out.termination = Termination::Signal(15);
        timed_out.timed_out = true;
        failures.push(timed_out);
        let mut read_failed = complete.clone();
        read_failed.stdout.read_failed = true;
        failures.push(read_failed);
        let mut truncated = complete.clone();
        truncated.stdout.truncated = true;
        failures.push(truncated);
        failures.push(ExecOutcome {
            drain_truncated: true,
            ..complete.clone()
        });
        failures.push(ExecOutcome {
            termination: Termination::Exit(128),
            ..complete
        });
        for failure in failures {
            assert!(matches!(
                super::normalised_config_output(Ok(failure)),
                Err(RepoError::ConfigNormalisation(_))
            ));
        }
        assert!(matches!(
            super::normalised_config_output(Err(ExecError::Io {
                operation: "spawn",
                source: std::io::Error::from(std::io::ErrorKind::Interrupted),
            })),
            Err(RepoError::ConfigNormalisationExecution(ExecError::Io {
                operation: "spawn",
                ..
            }))
        ));
    }

    #[test]
    fn git_directory_still_refuses_wrong_owner_writable_modes_and_wrong_kinds() {
        use provenant_os::fs::{FileKind, Metadata};
        let temp = tempfile::tempdir().unwrap();
        let safe = Metadata::from_std(fs::metadata(temp.path()).unwrap());
        assert!(super::verify_git_directory(safe, safe.uid).is_ok());
        for unsafe_metadata in [
            Metadata {
                uid: safe.uid.wrapping_add(1),
                ..safe
            },
            Metadata {
                mode: safe.mode | 0o020,
                ..safe
            },
            Metadata {
                mode: safe.mode | 0o002,
                ..safe
            },
            Metadata {
                kind: FileKind::Regular,
                ..safe
            },
            Metadata {
                kind: FileKind::Symlink,
                ..safe
            },
            Metadata {
                kind: FileKind::Other,
                ..safe
            },
        ] {
            assert!(matches!(
                super::verify_git_directory(unsafe_metadata, safe.uid),
                Err(RepoError::InvalidGitDirectory)
            ));
        }
    }

    #[test]
    fn repo_revalidation_detects_config_changes() {
        let temp = tempfile::tempdir().unwrap();
        let git = temp.path().join(".git");
        fs::create_dir(&git).unwrap();
        fs::write(git.join("config"), b"[core]\nrepositoryformatversion = 0\n").unwrap();
        fs::write(git.join("HEAD"), b"ref: refs/heads/main\n").unwrap();
        let repo = VerifiedRepo::open(temp.path(), |_| {
            Ok::<_, RepoError>(RepoProbeData::without_includes(
                RepoObjectFormat::Sha1,
                String::new(),
                Vec::new(),
            ))
        })
        .unwrap();
        fs::write(
            git.join("config"),
            b"[filter.poison]\nclean = touch /tmp/pwned\n",
        )
        .unwrap();

        assert!(matches!(repo.revalidate(), Err(RepoError::IdentityChanged)));
    }

    #[test]
    fn repo_revalidation_accepts_a_tracked_sibling_worktree() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        fs::create_dir(&root).unwrap();
        let git = |args: &[&str]| {
            let result = provenant_os::fixture::git_command()
                .current_dir(&root)
                .args(args)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["commit", "--allow-empty", "-qm", "fixture"]);
        git(&["remote", "add", "origin", "https://example.invalid/repo"]);
        git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(&[
            "worktree",
            "add",
            "-q",
            "-b",
            "first",
            temp.path().join("first").to_str().unwrap(),
            "HEAD",
        ]);
        let repo = VerifiedRepo::open(&root, |_| {
            Ok::<_, RepoError>(RepoProbeData::without_includes(
                RepoObjectFormat::Sha1,
                String::new(),
                Vec::new(),
            ))
        })
        .unwrap();
        let tree = verify_worktree(&repo, temp.path().join("first")).unwrap();
        git(&[
            "worktree",
            "add",
            "-q",
            "--track",
            "-b",
            "second",
            temp.path().join("second").to_str().unwrap(),
            "origin/main",
        ]);
        assert!(tree.repo().revalidate().is_ok());
        git(&["config", "branch.third.remote", "origin"]);
        assert!(tree.repo().revalidate().is_ok());
        let partial = VerifiedRepo::open(&root, |_| {
            Ok::<_, RepoError>(RepoProbeData::without_includes(
                RepoObjectFormat::Sha1,
                String::new(),
                Vec::new(),
            ))
        })
        .unwrap();
        let partial_tree = verify_worktree(&partial, temp.path().join("first")).unwrap();
        git(&["config", "branch.third.merge", "refs/heads/main"]);
        assert!(partial_tree.repo().revalidate().is_ok());
        assert!(tree.repo().revalidate().is_ok());
        let opened = VerifiedRepo::open(&root, |_| {
            git(&[
                "worktree",
                "add",
                "-q",
                "--track",
                "-b",
                "fourth",
                temp.path().join("fourth").to_str().unwrap(),
                "origin/main",
            ]);
            Ok::<_, RepoError>(RepoProbeData::without_includes(
                RepoObjectFormat::Sha1,
                String::new(),
                Vec::new(),
            ))
        });
        assert!(opened.is_ok());
        let config_path = root.join(".git/config");
        let config = fs::read(&config_path).unwrap();
        // Git decodes quoted subsection names before the comparison.
        let mut escaped = config.clone();
        escaped.extend_from_slice(
            b"[branch \"sibling\\\"quoted\"]\n remote = origin\n merge = refs/heads/main\n",
        );
        fs::write(&config_path, &escaped).unwrap();
        assert!(tree.repo().revalidate().is_ok());
        escaped.extend_from_slice(b"[branch \"first\"]\n remote = elsewhere\n");
        fs::write(&config_path, &escaped).unwrap();
        assert!(tree.repo().revalidate().is_err());
        fs::write(&config_path, b"[broken\n").unwrap();
        assert!(tree.repo().revalidate().is_err());
        fs::write(&config_path, &config).unwrap();
        let mut poisoned = config.clone();
        poisoned.extend_from_slice(b"[core]\n\thooksPath = /tmp/poison\n");
        fs::write(&config_path, poisoned).unwrap();
        assert!(matches!(
            tree.repo().revalidate(),
            Err(RepoError::IdentityChanged)
        ));
        fs::write(&config_path, config).unwrap();
        let marker = temp.path().join("first/.git");
        let marker_bytes = fs::read(&marker).unwrap();
        fs::write(&marker, b"gitdir: /tmp/not-the-original\n").unwrap();
        assert!(tree.repo().revalidate().is_err());
        assert!(tree.repo().revalidate_identity().is_err());
        fs::write(&marker, marker_bytes).unwrap();
        fs::rename(temp.path().join("first"), temp.path().join("moved")).unwrap();
        assert!(tree.repo().revalidate().is_err());
    }
}
