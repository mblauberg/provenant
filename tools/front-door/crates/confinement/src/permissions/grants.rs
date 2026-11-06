//! Worktree Git write grants bound into writer authority.

use super::*;
use crate::cache::{CARGO_CACHE_OWNER, CARGO_CACHE_OWNER_PREFIX, verify_cache_metadata};
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
) -> Result<PathGrant, PermissionError> {
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
    let grant = PathGrant {
        path: home.join("Library/Caches/provenant-lane-cargo").join(name),
        scope: PathScope::Subtree,
    };
    validate_noncredential_grant(&grant)
        .map_err(|error| error.with_credential_path(&grant.path, Some(home)))?;
    Ok(grant)
}

/// Prepare only derived writer directories outside the child sandbox. This
/// creates the lane cache and other derived directories. Call while holding writer custody,
/// before collecting `required_anchors`. Shared config and packed refs remain
/// immutable; tracking refs remain host-owned.
pub fn prepare_writer_directories(
    worktree: &VerifiedWorktree,
    selected_ref: &GitRefName,
    authority: &WorktreeWriteAuthority,
    home: &Path,
) -> Result<(), PermissionError> {
    worktree.repo().revalidate()?;
    let grants = WorktreeWriteGrants::derive(worktree, selected_ref);
    if grants.digest(&authority.cargo_cache) != authority.git_grants_digest {
        return Err(PermissionError::GitWriteAuthorityMissing);
    }
    grants.validate()?;
    validate_writer_cache(authority, worktree, home)?;
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
                let mut marker = CARGO_CACHE_OWNER_PREFIX.to_vec();
                let identity = worktree.identity();
                marker.extend_from_slice(&identity.device.to_le_bytes());
                marker.extend_from_slice(&identity.inode.to_le_bytes());
                marker.extend_from_slice(worktree.path().as_os_str().as_encoded_bytes());
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
    Ok(())
}

pub(super) fn validate_writer_cache(
    authority: &WorktreeWriteAuthority,
    worktree: &VerifiedWorktree,
    home: &Path,
) -> Result<(), PermissionError> {
    if authority.cargo_cache != worktree_cargo_cache(home, worktree)? {
        return Err(PermissionError::GitWriteAuthorityMissing);
    }
    Ok(())
}

/// Host Git boundaries bound into writer authority. All remain read-only;
/// the lane owns its complete Git directory under attempt scratch.
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

    pub(super) fn digest(&self, cache: &PathGrant) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"provenant-worktree-git-grants-v13\0");
        digest.update((self.branch.len() as u64).to_be_bytes());
        digest.update(self.branch.as_bytes());
        for grant in self.iter().chain([cache]) {
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

    pub(super) fn validate(&self) -> Result<(), PermissionError> {
        self.worktree.validate()?;
        for grant in self.iter().skip(1) {
            validate_noncredential_grant(grant)
                .map_err(|error| error.with_credential_path(&grant.path, None))?;
        }
        Ok(())
    }
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
pub(super) fn push_worktree_rule(
    source: &mut String,
    grants: &WorktreeWriteGrants,
) -> Result<(), PermissionError> {
    let worktree = sbpl_path(&grants.worktree.path)?;
    let git_dir = sbpl_path(&grants.private_git_dir.path)?;
    let common_dir = sbpl_path(&grants.common_git_dir.path)?;
    source.push_str(&format!(
        "(allow file-read* (subpath \"{git_dir}\") (subpath \"{common_dir}\"))\n"
    ));
    source.push_str(&format!(
        "(allow file-write* (require-all (subpath \"{worktree}\") (require-not (subpath \"{git_dir}\")) (require-not (subpath \"{common_dir}\"))))\n"
    ));
    // Shared metadata stays immutable even when another write root overlaps it.
    // Git writes its index, refs and operation state entirely under scratch.
    for grant in [&grants.common_git_dir, &grants.private_git_dir] {
        push_rule(source, "deny", "file-write*", &grant.path, grant.scope)?;
    }
    Ok(())
}
