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

/// Git environment for project-layout inventory and saved-hint verification.
/// Keep host paths, global config, lazy fetch and replace refs out of layout evidence.
pub fn project_layout_git_environment() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    [
        ("HOME", "/nonexistent"),
        ("XDG_CONFIG_HOME", "/nonexistent"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_SYSTEM", "/dev/null"),
        ("GIT_NO_REPLACE_OBJECTS", "1"),
        ("GIT_NO_LAZY_FETCH", "1"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("LC_ALL", "C"),
    ]
    .map(|(key, value)| (key.into(), value.into()))
    .into()
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

/// Resolve a registered project using Git inventory first and its saved
/// checkout hint only when Git cannot identify a conventional primary.
/// The hint is re-proved from the checkout with Git and its common-directory
/// identity before it can own project configuration or managed worktree paths.
pub fn project_layout_for_binding(
    executor: &dyn Executor,
    binding: &provenant_config::ProjectBinding,
    git: &Path,
    environment: &[(std::ffi::OsString, std::ffi::OsString)],
    inventory_timeout: Duration,
    cancel: &CancelToken,
) -> Result<ProjectLayout, WorktreeError> {
    match project_layout_for_common(
        executor,
        binding.common_dir(),
        git,
        environment,
        inventory_timeout,
        cancel,
    ) {
        Ok(layout) => return Ok(layout),
        // A valid inventory with no discoverable primary is the one case a
        // saved checkout hint can resolve. Malformed output and Git failures
        // remain fail-closed.
        Err(WorktreeError::InvalidRequest) => {}
        Err(error) => return Err(error),
    }

    let Some(checkout_hint) = binding.primary_checkout_hint() else {
        return Err(WorktreeError::ProjectLayoutUnresolved);
    };
    let common = canonicalize(binding.common_dir())?;
    let checkout =
        canonicalize(checkout_hint).map_err(|_| WorktreeError::ProjectLayoutUnresolved)?;
    let git_common = provenant_os::git::repository_path_with_cancel(
        executor,
        git,
        &checkout,
        provenant_os::git::RepositoryPath::CommonDir,
        Some(cancel),
    )
    .map_err(|_| WorktreeError::ProjectLayoutUnresolved)?;
    let git_root = provenant_os::git::repository_path_with_cancel(
        executor,
        git,
        &checkout,
        provenant_os::git::RepositoryPath::Toplevel,
        Some(cancel),
    )
    .map_err(|_| WorktreeError::ProjectLayoutUnresolved)?;
    if canonicalize(&git_common)? != common || canonicalize(&git_root)? != checkout {
        return Err(WorktreeError::ProjectLayoutUnresolved);
    }

    let common_identity = Directory::open_anchor(&common)?.metadata()?;
    let repo = VerifiedRepo::open_structure_only(&checkout)
        .map_err(|_| WorktreeError::ProjectLayoutUnresolved)?;
    if repo.common_git_dir() != common
        || repo.private_git_dir() != common
        || !repo.common_identity().same_identity(&common_identity)
    {
        return Err(WorktreeError::ProjectLayoutUnresolved);
    }
    ProjectLayout::from_repo(&repo, true)
}

/// Resolve a primary project owner from an already verified selected checkout.
/// Git inventory proves primary ownership; a selected linked checkout remains
/// only the request's selected root when no primary can be established.
pub fn project_layout_for_checkout(
    executor: &dyn Executor,
    repo: &crate::repo::ReadOnlyRepo,
    utility: &UtilityRuntime,
    budget: UtilityBudget,
    cancel: &CancelToken,
) -> Result<ProjectLayout, WorktreeError> {
    let inventory =
        RepoFacts::default().read_only_inventory(repo, executor, utility, budget, cancel)?;
    let entries = parse_worktree_list(&inventory)?;
    match verified_primary_from_inventory(repo.common_git_dir(), repo.0.common_identity(), &entries)
    {
        Ok(Some(primary)) => ProjectLayout::from_repo(&primary, true),
        Ok(None) => {
            ProjectLayout::from_common_identity(repo.common_git_dir(), repo.0.common_identity())
        }
        Err(WorktreeError::InvalidRequest) if repo.private_git_dir() == repo.common_git_dir() => {
            ProjectLayout::from_repo(&repo.0, true)
        }
        Err(WorktreeError::InvalidRequest) => {
            ProjectLayout::from_common_identity(repo.common_git_dir(), repo.0.common_identity())
        }
        Err(error) => Err(error),
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
