//! Held path anchors for verifying grants at compile time.

use super::*;
use crate::repo::{GitRefName, VerifiedWorktree};
#[cfg(target_os = "macos")]
use provenant_os::fs::Metadata;
use provenant_os::fs::{Directory, FileKind};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub(super) fn anchor_at(path: &Path, stage: &'static str) -> PermissionError {
    PermissionError::AnchorMismatchAt {
        path: path.to_owned(),
        stage,
    }
}

pub struct AnchoredDirectory {
    pub(super) path: PathBuf,
    #[cfg(target_os = "macos")]
    pub(super) directory: Directory,
    #[cfg(target_os = "macos")]
    identity: Metadata,
}

impl AnchoredDirectory {
    /// `path` must be the canonical absolute spelling resolved by the caller's
    /// verified-path logic. Operations use only the held Directory thereafter.
    pub fn open_canonical(path: impl AsRef<Path>) -> Result<Self, PermissionError> {
        let path = path.as_ref();
        validate_absolute(path)?;
        let canonical =
            provenant_os::fs::canonicalize(path).map_err(PermissionError::Filesystem)?;
        if canonical.as_path() != path {
            return Err(PermissionError::InvalidPath(path.to_owned()));
        }
        let directory = Directory::open_anchor(path).map_err(PermissionError::Filesystem)?;
        let identity = directory.metadata().map_err(PermissionError::Filesystem)?;
        if identity.kind != FileKind::Directory {
            return Err(anchor_at(path, "path is not a directory"));
        }
        Ok(Self {
            path: path.to_owned(),
            #[cfg(target_os = "macos")]
            directory,
            #[cfg(target_os = "macos")]
            identity,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(target_os = "macos")]
    pub(super) fn verify(&self) -> Result<(), PermissionError> {
        let canonical =
            provenant_os::fs::canonicalize(&self.path).map_err(PermissionError::Filesystem)?;
        if canonical.as_path() != self.path {
            return Err(anchor_at(&self.path, "path no longer resolves to itself"));
        }
        let named = Directory::open_anchor(&self.path).map_err(PermissionError::Filesystem)?;
        let named_identity = named.metadata().map_err(PermissionError::Filesystem)?;
        let held_identity = self
            .directory
            .metadata()
            .map_err(PermissionError::Filesystem)?;
        if named_identity.kind != FileKind::Directory
            || held_identity.kind != FileKind::Directory
            || !named_identity.same_identity(&self.identity)
            || !held_identity.same_identity(&self.identity)
        {
            return Err(anchor_at(
                &self.path,
                "directory was replaced after it was held",
            ));
        }
        Ok(())
    }
}

pub struct Anchors<'a> {
    /// Canonical user home resolved by the host, independent of plan records.
    pub home: &'a Path,
    pub directories: &'a [AnchoredDirectory],

    /// Linked tree and selected branch derive Git paths at the authority edge.
    pub worktree: Option<(&'a VerifiedWorktree, &'a GitRefName)>,
}

#[cfg(target_os = "macos")]
impl Anchors<'_> {
    pub(super) fn verify_all(&self) -> Result<(), PermissionError> {
        for anchor in self.directories {
            anchor.verify()?;
        }
        Ok(())
    }

    pub(super) fn verify_discovery_grant(&self, grant: &PathGrant) -> Result<(), PermissionError> {
        // Discovery grants list only the literal directory, never its children.
        // Resolve the entry without following links through its verified parent.
        let mismatch = |stage| PermissionError::AnchorMismatchAt {
            path: grant.path.clone(),
            stage,
        };
        if grant.scope != PathScope::Exact {
            return Err(PermissionError::InvalidPath(grant.path.clone()));
        }
        let parent = grant
            .path
            .parent()
            .ok_or_else(|| mismatch("discovery parent"))?;
        let name = grant
            .path
            .file_name()
            .ok_or_else(|| mismatch("discovery entry"))?;
        let anchor = self
            .directories
            .iter()
            .find(|anchor| anchor.path == parent)
            .ok_or_else(|| mismatch("discovery parent anchor lookup"))?;
        anchor.verify()?;
        match anchor.directory.metadata_at(name, false) {
            Ok(metadata) if metadata.kind == FileKind::Directory => Ok(()),
            Ok(_) => Err(mismatch("discovery entry type check")),
            Err(error) if error.is_not_found() => Ok(()),
            Err(error) => Err(PermissionError::Filesystem(error)),
        }
    }

    pub(super) fn verify_grant_held(&self, grant: &PathGrant) -> Result<(), PermissionError> {
        match grant.scope {
            PathScope::Subtree => {
                let anchor = self
                    .directories
                    .iter()
                    .find(|anchor| anchor.path == grant.path)
                    .ok_or_else(|| anchor_at(&grant.path, "directory anchor not held"))?;
                anchor.verify()
            }
            PathScope::Exact => {
                self.exact_grant_metadata(&grant.path)?;
                Ok(())
            }
        }
    }

    /// Read grants may also name a directory exactly: the profile renders it as
    /// a literal, which lists entry names without reading any child. A link
    /// still fails, because the entry is resolved without following it.
    pub(super) fn verify_read_held(&self, grant: &PathGrant) -> Result<(), PermissionError> {
        if grant.scope == PathScope::Subtree {
            return self.verify_grant_held(grant);
        }
        let Some((anchor, name)) = self.exact_parent(&grant.path)? else {
            return Ok(());
        };
        match anchor.directory.metadata_at(name, false) {
            Ok(metadata) if matches!(metadata.kind, FileKind::Regular | FileKind::Directory) => {
                Ok(())
            }
            Ok(_) => Err(anchor_at(
                &grant.path,
                "path is not a regular file or directory",
            )),
            Err(error) if error.is_not_found() => Ok(()),
            Err(error) => Err(PermissionError::Filesystem(error)),
        }
    }

    pub(super) fn verify_executable(&self, grant: &PathGrant) -> Result<(), PermissionError> {
        if grant.scope != PathScope::Exact {
            return Err(PermissionError::InvalidPath(grant.path.clone()));
        }
        match self.exact_grant_metadata(&grant.path)? {
            Some(_) => Ok(()),
            None => Err(anchor_at(&grant.path, "executable is missing")),
        }
    }

    pub(super) fn verify_single_link_grant(
        &self,
        grant: &PathGrant,
    ) -> Result<(), PermissionError> {
        if grant.scope != PathScope::Exact {
            return Err(PermissionError::InvalidPath(grant.path.clone()));
        }
        match self.exact_grant_metadata(&grant.path)? {
            Some(metadata) if metadata.links == 1 => Ok(()),
            Some(_) => Err(anchor_at(&grant.path, "file has other hard links")),
            None => Ok(()),
        }
    }

    /// The verified parent anchor and entry name of an exact grant, or `None`
    /// when the parent does not exist and the grant therefore grants nothing.
    fn exact_parent<'p>(
        &self,
        path: &'p Path,
    ) -> Result<Option<(&AnchoredDirectory, &'p std::ffi::OsStr)>, PermissionError> {
        let parent = path
            .parent()
            .ok_or_else(|| PermissionError::InvalidPath(path.to_owned()))?;
        let name = path
            .file_name()
            .ok_or_else(|| PermissionError::InvalidPath(path.to_owned()))?;
        let Some(anchor) = self.directories.iter().find(|anchor| anchor.path == parent) else {
            // A grant on a file in a directory that does not exist grants
            // nothing: only a held, present parent can be written through.
            return match std::fs::symlink_metadata(parent) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                _ => Err(anchor_at(parent, "parent directory anchor not held")),
            };
        };
        anchor.verify()?;
        Ok(Some((anchor, name)))
    }

    fn exact_grant_metadata(&self, path: &Path) -> Result<Option<Metadata>, PermissionError> {
        let Some((anchor, name)) = self.exact_parent(path)? else {
            return Ok(None);
        };
        match anchor.directory.metadata_at(name, false) {
            Ok(metadata) if metadata.kind == FileKind::Regular => Ok(Some(metadata)),
            Ok(_) => Err(anchor_at(path, "path is not a regular file")),
            Err(error) if error.is_not_found() => Ok(None),
            Err(error) => Err(PermissionError::Filesystem(error)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Enforcement {
    MacOsSandboxExec,
}

/// A compiled launch profile. Discard and recompile it after any writable root
/// changes; callers hold writer exclusion from compilation through handoff.
pub struct ConfinedLaunch {
    pub enforcement: Enforcement,
    pub profile: Vec<u8>,
    pub profile_digest: [u8; 32],
    pub native_controls: Vec<String>,
}

impl ConfinedLaunch {
    /// The only way to turn a provider argv into a runnable command: the
    /// compiled profile wraps it, so no caller can launch the CLI unconfined.
    ///
    /// The profile also allows looking up the attempt's own Mach name, which
    /// grants nothing. The sandbox is inherited and cannot be dropped, so the
    /// supervisor uses that allowance to find processes that left the
    /// provider's process tree.
    pub fn command(
        &self,
        argv: &[OsString],
        attempt: &provenant_core::AttemptId,
    ) -> Result<Vec<OsString>, PermissionError> {
        let profile =
            std::str::from_utf8(&self.profile).map_err(|_| PermissionError::InvalidFixture)?;
        let profile = format!(
            "{profile}(allow mach-lookup (global-name \"{}\"))\n",
            attempt.sandbox_lookup_name()
        );
        let mut command = Vec::with_capacity(argv.len() + 3);
        command.extend(["/usr/bin/sandbox-exec", "-p", &profile].map(OsString::from));
        command.extend(argv.iter().cloned());
        Ok(command)
    }
}

/// Directories a launcher must hold as anchors before `compile`: each subtree
/// grant itself and the parent of each exact grant. Writer Git paths are
/// derived from the verified worktree, exactly as the compiler derives them.
pub fn required_anchors(
    plan: &PlanPermissions,
    profile: &ProviderProfile,
    worktree: Option<(&VerifiedWorktree, &GitRefName)>,
) -> BTreeSet<PathBuf> {
    let spec = &profile.spec;
    let selected = plan.selected_authentication.as_ref().and_then(|mechanism| {
        spec.authentication
            .iter()
            .find(|auth| &auth.mechanism == mechanism)
    });
    let mut grants: Vec<&PathGrant> = plan
        .read_roots
        .iter()
        .chain(&plan.protected_roots)
        .chain(&plan.secret_roots)
        .chain(std::iter::once(&plan.scratch))
        .chain(&plan.private_state)
        .chain(&spec.process_executables)
        .chain(&spec.runtime_reads)
        .chain(spec.fabric_socket.iter())
        .chain(
            spec.discovery_reads
                .iter()
                .filter(|grant| super::validation::valid_discovery_read(grant)),
        )
        .chain(&spec.state_writes)
        .chain(&spec.tool_exec_roots)
        .chain(
            selected
                .into_iter()
                .flat_map(|auth| auth.roots.iter().chain(&auth.in_place_writes)),
        )
        .collect();
    if plan.include_continuation {
        grants.extend(&spec.continuation_reads);
    }
    if let Some(read_only) = &plan.read_only {
        grants.extend(read_only.git_reads.iter().chain(&read_only.writes));
    }
    let derived = worktree
        .map(|(verified, selected_ref)| WorktreeWriteGrants::derive(verified, selected_ref));
    if let Some(derived) = &derived {
        grants.extend(derived.iter());
    }
    if let AccessMode::WorktreeWrite(authority) = &plan.mode {
        grants.push(&authority.cargo_cache);
    }
    grants
        .into_iter()
        .filter_map(|grant| match grant.scope {
            PathScope::Subtree => Some(grant.path.clone()),
            // A file in a directory that does not exist yet needs no anchor.
            PathScope::Exact => grant
                .path
                .parent()
                .filter(|parent| std::fs::symlink_metadata(parent).is_ok())
                .map(Path::to_owned),
        })
        .collect()
}
