//! Verified mapping from Git common directories to stable project layouts.
use super::inventory::{GitWorktree, read_bounded};
use super::*;
use provenant_core::CancelToken;
use provenant_os::fs::{Directory, FileKind};
use std::ffi::OsStr;
use std::path::PathBuf;
use std::time::Duration;

/// The path owner for project-scoped configuration and managed worktrees.
/// A bare common directory has no project checkout, so its layout is host-only.
pub struct ProjectLayout {
    root: PathBuf,
    project: Option<PathBuf>,
    directory: Directory,
}

impl ProjectLayout {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn project(&self) -> Option<&Path> {
        self.project.as_deref()
    }

    pub fn directory(&self) -> &Directory {
        &self.directory
    }

    pub(super) fn from_repo(repo: &VerifiedRepo, project: bool) -> Result<Self, WorktreeError> {
        let root = if project {
            repo.root_path().to_path_buf()
        } else {
            repo.common_git_dir().to_path_buf()
        };
        let expected = if project {
            repo.root_identity()
        } else {
            repo.common_identity()
        };
        Self::from_identity(root, project, expected)
    }

    fn from_identity(
        root: PathBuf,
        project: bool,
        expected: provenant_os::fs::Metadata,
    ) -> Result<Self, WorktreeError> {
        let directory = Directory::open_anchor(&root)?;
        if !expected.same_identity(&directory.metadata()?) {
            return Err(WorktreeError::IdentityChanged);
        }
        Ok(Self {
            project: project.then(|| root.clone()),
            root,
            directory,
        })
    }

    pub(super) fn from_common_identity(
        common: &Path,
        identity: provenant_os::fs::Metadata,
    ) -> Result<Self, WorktreeError> {
        Self::from_identity(common.to_path_buf(), false, identity)
    }

    pub(crate) fn for_verified_repo(repo: &VerifiedRepo) -> Result<Self, WorktreeError> {
        Self::from_repo(repo, repo.private_git_dir() == repo.common_git_dir())
    }
}

/// Resolve a registered common Git directory to the project-layout owner using
/// its verified Git inventory. A bare common directory has no project owner.
pub fn project_layout_for_common(
    executor: &dyn Executor,
    common: &Path,
    git: &Path,
    environment: &[(std::ffi::OsString, std::ffi::OsString)],
    inventory_timeout: Duration,
    cancel: &CancelToken,
) -> Result<ProjectLayout, WorktreeError> {
    let common = provenant_os::fs::canonicalize(common)?;
    let directory = Directory::open_anchor(&common)?;
    let identity = directory.metadata()?;
    let inventory = RepoFacts::default().discover_worktree_inventory(
        &common,
        git,
        environment,
        executor,
        inventory_timeout,
        cancel,
    )?;
    let entries = parse_worktree_list(&inventory)?;
    match verified_primary_from_inventory(&common, identity, &entries)? {
        Some(repo) => ProjectLayout::from_repo(&repo, true),
        None => ProjectLayout::from_common_identity(&common, identity),
    }
}

pub(super) fn verified_primary_from_inventory(
    common: &Path,
    common_identity: provenant_os::fs::Metadata,
    entries: &[GitWorktree],
) -> Result<Option<VerifiedRepo>, WorktreeError> {
    let mut primary = None;
    let mut bare_common_registered = false;
    for entry in entries {
        if entry.bare {
            bare_common_registered |=
                canonical_inventory_path(&entry.path).as_deref() == Some(common);
            continue;
        }
        let Some(root) = canonical_inventory_path(&entry.path) else {
            continue;
        };
        if checkout_private_dir(&root)?.as_deref() != Some(common) {
            continue;
        }
        let repo = VerifiedRepo::open_structure_only(&root)?;
        if repo.common_git_dir() != common
            || repo.private_git_dir() != common
            || !repo.common_identity().same_identity(&common_identity)
        {
            return Err(WorktreeError::IdentityChanged);
        }
        if primary.replace(repo).is_some() {
            return Err(WorktreeError::InvalidOutput);
        }
    }
    if primary.is_some() || bare_common_registered {
        Ok(primary)
    } else {
        Err(WorktreeError::InvalidRequest)
    }
}

pub(super) fn canonical_inventory_path(path: &Path) -> Option<PathBuf> {
    provenant_os::fs::canonicalize(path).ok()
}

/// Return a checkout's private Git directory only when its `.git` marker is a
/// real directory or a regular Git file. A linked checkout points below the
/// common directory; only the primary's marker points directly at common.
fn checkout_private_dir(path: &Path) -> Result<Option<PathBuf>, WorktreeError> {
    let root = match Directory::open_anchor(path) {
        Ok(root) => root,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let marker = OsStr::new(".git");
    let metadata = match root.metadata_at(marker, false) {
        Ok(metadata) => metadata,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let target = match metadata.kind {
        FileKind::Directory => path.join(marker),
        FileKind::Regular => {
            let bytes = read_bounded(&root, marker, 4096)?;
            let target = crate::repo::parse_git_path(&bytes, b"gitdir: ")
                .ok_or(WorktreeError::InvalidOutput)?;
            crate::repo::resolve_git_path(path, &target)?
        }
        FileKind::Symlink | FileKind::Other => return Ok(None),
    };
    match provenant_os::fs::canonicalize(&target) {
        Ok(path) => Ok(Some(path)),
        Err(error) if error.is_not_found() => Ok(None),
        Err(error) => Err(error.into()),
    }
}
