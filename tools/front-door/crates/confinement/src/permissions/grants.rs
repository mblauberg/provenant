//! Worktree Git write grants bound into writer authority.

use super::*;
use crate::cache::{CARGO_CACHE_OWNER, CacheOwner, encode_cache_owner, verify_cache_metadata};
use crate::repo::{GitRefName, VerifiedWorktree};
use provenant_os::fs::{Directory, PublishMode};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

/// The lane-only build cache belonging to this verified worktree incarnation.
/// Host-derived canonical paths and held Git identities separate repositories,
/// same-named worktrees and worktrees recreated at an earlier path.
pub fn worktree_cargo_cache(
    home: &Path,
    worktree: &VerifiedWorktree,
    credentials: &provenant_core::CredentialRoots,
) -> Result<PathGrant, PermissionError> {
    let grant = PathGrant {
        path: worktree_cargo_cache_path(home, worktree)?,
        scope: PathScope::Subtree,
    };
    validate_noncredential_grant(&grant, credentials)?;
    Ok(grant)
}

/// Derive the identity-bound cache path used by a verified worktree.
/// Callers that expose it as a permission grant must use
/// [`worktree_cargo_cache`] so declared credentials are checked.
pub fn worktree_cargo_cache_path(
    home: &Path,
    worktree: &VerifiedWorktree,
) -> Result<PathBuf, PermissionError> {
    validate_absolute(home)?;
    worktree.repo().revalidate()?;
    let mut digest = Sha256::new();
    digest.update(b"provenant-lane-cargo-v1");
    for path in [worktree.path(), worktree.repo().common_git_dir()] {
        let bytes = path.as_os_str().as_encoded_bytes();
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    for identity in [
        worktree.identity(),
        worktree.repo().private_identity(),
        worktree.repo().common_identity(),
    ] {
        digest.update(identity.device.to_le_bytes());
        digest.update(identity.inode.to_le_bytes());
    }
    let name = provenant_core::hex(&digest.finalize());
    Ok(home.join("Library/Caches/provenant-lane-cargo").join(name))
}

/// Derive the persistent shared Git state container for this verified
/// worktree incarnation and selected branch. The digest binds both Git
/// directory identities and the custody subject, so path reuse cannot inherit
/// refs from a removed checkout.
pub fn worktree_git_state_path(
    home: &Path,
    worktree: &VerifiedWorktree,
    selected_ref: &GitRefName,
) -> Result<PathBuf, PermissionError> {
    validate_absolute(home)?;
    worktree.repo().revalidate()?;
    let subject = crate::worktrees::worktree_subject(worktree.repo())
        .map_err(|_| PermissionError::GitWriteAuthorityMissing)?;
    let mut digest = Sha256::new();
    digest.update(b"provenant-worktree-git-state-v1\0");
    for value in [
        subject.kind.as_str().as_bytes(),
        subject.id.as_bytes(),
        selected_ref.as_str().as_bytes(),
    ] {
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value);
    }
    for identity in [
        worktree.identity(),
        worktree.repo().private_identity(),
        worktree.repo().common_identity(),
    ] {
        digest.update(identity.device.to_be_bytes());
        digest.update(identity.inode.to_be_bytes());
    }
    let name = provenant_core::hex(&digest.finalize());
    Ok(home
        .join("Library/Application Support/provenant/worktree-git")
        .join(name))
}

/// Host-derived grant for the persistent shared Git state container.
pub fn worktree_git_state(
    home: &Path,
    worktree: &VerifiedWorktree,
    selected_ref: &GitRefName,
    credentials: &provenant_core::CredentialRoots,
) -> Result<PathGrant, PermissionError> {
    let grant = PathGrant {
        path: worktree_git_state_path(home, worktree, selected_ref)?,
        scope: PathScope::Subtree,
    };
    validate_noncredential_grant(&grant, credentials)?;
    validate_noncredential_grant(&worktree_git_state_namespace(&grant)?, credentials)?;
    Ok(grant)
}

/// Only the shared ref/object children are writable. The container itself
/// holds host-owned configuration, initialisation locks and ownership proof.
pub(super) fn worktree_git_state_writes(container: &PathGrant) -> [PathGrant; 2] {
    ["git-common", "git-objects"].map(|name| PathGrant {
        path: container.path.join(name),
        scope: PathScope::Subtree,
    })
}

/// The shared storage namespace contains every worktree's Git state. A writer
/// may mutate only its own two data children beneath this protected root.
pub(super) fn worktree_git_state_namespace(
    container: &PathGrant,
) -> Result<PathGrant, PermissionError> {
    Ok(PathGrant {
        path: container
            .path
            .parent()
            .ok_or(PermissionError::GitWriteAuthorityMissing)?
            .to_owned(),
        scope: PathScope::Subtree,
    })
}

/// Host-maintained Git files stay immutable within the writable data children.
pub(super) fn worktree_git_state_host_files(container: &PathGrant) -> [PathGrant; 6] {
    [
        container.path.join("git-common/config"),
        container.path.join("git-common/config.lock"),
        container.path.join("git-common/config.worktree"),
        container.path.join("git-common/config.worktree.lock"),
        container.path.join("git-objects/info/alternates"),
        container.path.join("git-objects/info/http-alternates"),
    ]
    .map(|path| PathGrant {
        path,
        scope: PathScope::Exact,
    })
}

/// Prepare only host-derived writer directories outside the child sandbox. This
/// creates the lane cache and shared Git-state container and data directories.
/// Call while holding writer custody, before collecting `required_anchors`.
/// The container and Git configuration remain host-owned; shared refs and
/// objects are writable through the two narrowly scoped children.
pub fn prepare_writer_directories(
    worktree: &VerifiedWorktree,
    selected_ref: &GitRefName,
    authority: &WorktreeWriteAuthority,
    home: &Path,
    credentials: &provenant_core::CredentialRoots,
) -> Result<(), PermissionError> {
    worktree.repo().revalidate()?;
    let grants = WorktreeWriteGrants::derive(worktree, selected_ref);
    if grants.digest(&authority.cargo_cache, &authority.git_state) != authority.git_grants_digest {
        return Err(PermissionError::GitWriteAuthorityMissing);
    }
    grants.validate(credentials)?;
    validate_writer_cache(authority, worktree, home, credentials)?;
    validate_writer_git_state(authority, worktree, selected_ref, home, credentials)?;
    let paths: BTreeSet<_> = grants
        .iter()
        .chain([&authority.cargo_cache])
        .filter_map(|grant| match grant.scope {
            PathScope::Subtree => Some(grant.path.as_path()),
            PathScope::Exact => grant.path.parent(),
        })
        .collect();
    for path in paths {
        let mut directory =
            Directory::open_anchor(Path::new("/")).map_err(PermissionError::Filesystem)?;
        let mut traversed = PathBuf::from("/");
        for component in path.components() {
            let Component::Normal(name) = component else {
                continue;
            };
            directory = match directory.open_directory(name) {
                Ok(child) => child,
                Err(error) if error.is_not_found() => {
                    match directory.mkdir(name, 0o700) {
                        Ok(()) => {}
                        Err(error) if error.is_exists() => {}
                        Err(error) => return Err(PermissionError::Filesystem(error)),
                    }
                    directory
                        .open_directory(name)
                        .map_err(PermissionError::Filesystem)?
                }
                Err(error) => return Err(PermissionError::Filesystem(error)),
            };
            traversed.push(name);
            if traversed == authority.cargo_cache.path
                || Some(traversed.as_path()) == authority.cargo_cache.path.parent()
            {
                verify_cache_metadata(&directory.metadata().map_err(PermissionError::Filesystem)?)?;
            }
            if traversed == authority.cargo_cache.path {
                let identity = worktree.identity();
                let marker = encode_cache_owner(&CacheOwner {
                    path: worktree.path().to_path_buf(),
                    identity: (identity.device, identity.inode),
                });
                directory
                    .atomic_write(
                        OsStr::new(CARGO_CACHE_OWNER),
                        ".provenant-owner-",
                        &marker,
                        PublishMode::Replace,
                    )
                    .map_err(PermissionError::Filesystem)?;
            }
        }
    }
    prepare_git_state_directories(&authority.git_state)?;
    Ok(())
}

pub(super) fn validate_writer_cache(
    authority: &WorktreeWriteAuthority,
    worktree: &VerifiedWorktree,
    home: &Path,
    credentials: &provenant_core::CredentialRoots,
) -> Result<(), PermissionError> {
    if authority.cargo_cache != worktree_cargo_cache(home, worktree, credentials)? {
        return Err(PermissionError::GitWriteAuthorityMissing);
    }
    Ok(())
}

pub(super) fn validate_writer_git_state(
    authority: &WorktreeWriteAuthority,
    worktree: &VerifiedWorktree,
    selected_ref: &GitRefName,
    home: &Path,
    credentials: &provenant_core::CredentialRoots,
) -> Result<(), PermissionError> {
    let expected = worktree_git_state(home, worktree, selected_ref, credentials)?;
    if authority.git_state != expected {
        return Err(PermissionError::GitWriteAuthorityMissing);
    }
    Ok(())
}

fn prepare_git_state_directories(container: &PathGrant) -> Result<(), PermissionError> {
    let namespace = worktree_git_state_namespace(container)?;
    let writes = worktree_git_state_writes(container);
    let private_paths = [
        namespace.path.as_path(),
        container.path.as_path(),
        writes[0].path.as_path(),
        writes[1].path.as_path(),
    ];
    for path in private_paths {
        let mut directory =
            Directory::open_anchor(Path::new("/")).map_err(PermissionError::Filesystem)?;
        let mut traversed = PathBuf::from("/");
        for component in path.components() {
            let Component::Normal(name) = component else {
                continue;
            };
            directory = match directory.open_directory(name) {
                Ok(child) => child,
                Err(error) if error.is_not_found() => {
                    match directory.mkdir(name, 0o700) {
                        Ok(()) => directory.sync().map_err(PermissionError::Filesystem)?,
                        Err(error) if error.is_exists() => {}
                        Err(error) => return Err(PermissionError::Filesystem(error)),
                    }
                    directory
                        .open_directory(name)
                        .map_err(PermissionError::Filesystem)?
                }
                Err(error) => return Err(PermissionError::Filesystem(error)),
            };
            traversed.push(name);
            if private_paths.contains(&traversed.as_path()) {
                verify_cache_metadata(&directory.metadata().map_err(PermissionError::Filesystem)?)?;
            }
        }
        directory.sync().map_err(PermissionError::Filesystem)?;
    }
    Ok(())
}

/// Host repository Git boundaries bound into writer authority. They remain
/// read-only; shared mutable refs and objects live in the separate derived
/// Git-state grant, while provider configuration remains attempt-private.
pub(crate) struct WorktreeWriteGrants {
    pub(super) worktree: PathGrant,
    pub(super) private_git_dir: PathGrant,
    pub(super) common_git_dir: PathGrant,
    branch: String,
}

impl WorktreeWriteGrants {
    pub(super) fn derive(worktree: &VerifiedWorktree, selected_ref: &GitRefName) -> Self {
        let repo = worktree.repo();
        Self::from_paths(
            worktree.path(),
            repo.private_git_dir(),
            repo.common_git_dir(),
            selected_ref.short_name(),
        )
    }

    pub(super) fn from_paths(worktree: &Path, private: &Path, common: &Path, branch: &str) -> Self {
        let subtree = |path: &Path| PathGrant {
            path: path.to_owned(),
            scope: PathScope::Subtree,
        };
        Self {
            worktree: subtree(worktree),
            private_git_dir: subtree(private),
            common_git_dir: subtree(common),
            branch: branch.to_owned(),
        }
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &PathGrant> {
        [&self.worktree, &self.private_git_dir, &self.common_git_dir].into_iter()
    }

    pub(super) fn digest(&self, cache: &PathGrant, git_state: &PathGrant) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"provenant-worktree-git-grants-v14\0");
        digest.update((self.branch.len() as u64).to_be_bytes());
        digest.update(self.branch.as_bytes());
        for grant in self.iter().chain([cache, git_state]) {
            let bytes = grant.path.as_os_str().as_encoded_bytes();
            digest.update((bytes.len() as u64).to_be_bytes());
            digest.update(bytes);
            digest.update([match grant.scope {
                PathScope::Exact => 0,
                PathScope::Subtree => 1,
            }]);
        }
        digest.finalize().into()
    }

    pub(super) fn validate(
        &self,
        credentials: &provenant_core::CredentialRoots,
    ) -> Result<(), PermissionError> {
        for grant in self.iter() {
            validate_noncredential_grant(grant, credentials)?;
        }
        Ok(())
    }
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
pub(super) fn push_worktree_rule(
    source: &mut String,
    grants: &WorktreeWriteGrants,
    git_state: &PathGrant,
) -> Result<(), PermissionError> {
    let worktree = sbpl_path(&grants.worktree.path)?;
    let git_dir = sbpl_path(&grants.private_git_dir.path)?;
    let common_dir = sbpl_path(&grants.common_git_dir.path)?;
    let git_state_path = &git_state.path;
    let git_common_path = git_state_path.join("git-common");
    let git_objects_path = git_state_path.join("git-objects");
    let git_common = sbpl_path(&git_common_path)?;
    let git_objects = sbpl_path(&git_objects_path)?;
    source.push_str(&format!(
        "(allow file-read* (subpath \"{git_dir}\") (subpath \"{common_dir}\"))\n"
    ));
    source.push_str(&format!(
        "(allow file-write* (require-all (subpath \"{worktree}\") (require-not (subpath \"{git_dir}\")) (require-not (subpath \"{common_dir}\"))))\n"
    ));
    // The host-owned repository Git directories remain read-only.
    for grant in [&grants.common_git_dir, &grants.private_git_dir] {
        push_rule(source, "deny", "file-write*", &grant.path, grant.scope)?;
    }
    // The container holds host-owned config, init locks and ownership proof.
    // Read exceptions are restricted to this writer's two data children; the
    // namespace-wide boundary is applied for every profile in push_boundaries.
    source.push_str(&format!(
        "(allow file-read* (subpath \"{git_common}\") (subpath \"{git_objects}\"))\n"
    ));
    for grant in worktree_git_state_host_files(git_state) {
        push_rule(source, "deny", "file-write*", &grant.path, grant.scope)?;
    }
    for path in [&git_common_path, &git_objects_path] {
        source.push_str(&format!(
            "(deny file-write-create (require-all (vnode-type SYMLINK) (subpath \"{}\")))\n",
            sbpl_path(path)?
        ));
    }
    Ok(())
}
