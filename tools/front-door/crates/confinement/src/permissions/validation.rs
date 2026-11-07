//! Path, credential and worktree authority validation.

use super::*;
#[cfg(target_os = "macos")]
use crate::repo::VerifiedWorktree;
#[cfg(target_os = "macos")]
use provenant_os::fs::{Directory, FileKind};
use std::ffi::OsStr;
#[cfg(target_os = "macos")]
use std::io::Read;
use std::path::{Component, Path, PathBuf};

pub(super) const MAX_PATH_BYTES: usize = 4096;
#[cfg(test)]
pub(super) const MAX_PATH_GRANTS: usize = 128;
#[cfg(target_os = "macos")]
pub(super) const MAX_GIT_HEAD_BYTES: u64 = 1024;

#[cfg(any(target_os = "macos", test))]
pub(super) fn validate_plan_grant_limit(
    plan: &PlanPermissions,
    limit: usize,
) -> Result<(), PermissionError> {
    for (family, count) in [
        (
            "total root grant count",
            plan.read_roots
                .len()
                .saturating_add(plan.protected_roots.len())
                .saturating_add(plan.secret_roots.len())
                .saturating_add(plan.write_roots.len()),
        ),
        ("read root grant count", plan.read_roots.len()),
        ("protected root grant count", plan.protected_roots.len()),
        ("secret root grant count", plan.secret_roots.len()),
        ("write root grant count", plan.write_roots.len()),
    ] {
        if count > limit {
            return Err(PermissionError::GrantLimitExceeded { family, limit });
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub(super) fn verify_worktree_authority(
    authority: &WorktreeWriteAuthority,
    anchors: &Anchors<'_>,
) -> Result<WorktreeWriteGrants, PermissionError> {
    let (verified, selected_ref) = anchors
        .worktree
        .ok_or(PermissionError::GitWriteAuthorityMissing)?;
    verified.repo().revalidate()?;
    if verified.repo().private_git_dir() == verified.repo().common_git_dir() {
        return Err(PermissionError::GitWriteAuthorityMissing);
    }
    let grants = WorktreeWriteGrants::derive(verified, selected_ref);
    grants.validate()?;
    validate_writer_cache(authority, verified, anchors.home)?;
    if authority.common_git_dir() != Some(grants.common_git_dir.path.as_path()) {
        return Err(PermissionError::GitWriteAuthorityMissing);
    }
    if grants.digest(&authority.cargo_cache) != authority.git_grants_digest {
        return Err(PermissionError::GitWriteAuthorityMissing);
    }
    verify_selected_head(verified, selected_ref.short_name())?;
    for grant in grants.iter() {
        match grant.scope {
            PathScope::Subtree => anchors.verify_grant_held(grant)?,
            PathScope::Exact => anchors.verify_single_link_grant(grant)?,
        }
    }
    verify_git_control_links(verified.repo())?;
    Ok(grants)
}

/// Git config, hooks, `info/`, packed refs and the linked index are read-only
/// to the lane by path, so each must have one name: a second name inside the worktree would be writable.
/// A lane cannot make one (`deny file-link`), and a host tool does not link
/// into Git metadata, so only the few control files are inspected, not the
/// worktree.
#[cfg(target_os = "macos")]
pub(super) fn verify_git_control_links(
    repo: &crate::repo::VerifiedRepo,
) -> Result<(), PermissionError> {
    const ENTRIES_PER_DIRECTORY: usize = 512;
    let check = |directory: &Directory, parent: &Path, name: &OsStr| match directory
        .metadata_at(name, false)
    {
        Ok(metadata) if metadata.kind == FileKind::Regular && metadata.links > 1 => {
            Err(PermissionError::GitControlHardLinked(parent.join(name)))
        }
        Ok(_) => Ok(()),
        Err(error) if error.is_not_found() => Ok(()),
        Err(error) => Err(PermissionError::Filesystem(error)),
    };
    for name in ["config", "packed-refs"] {
        check(repo.common_dir(), repo.common_git_dir(), OsStr::new(name))?;
    }
    for name in ["config.worktree", "index"] {
        check(repo.private_dir(), repo.private_git_dir(), OsStr::new(name))?;
    }
    for name in ["hooks", "info"] {
        let parent = repo.common_git_dir().join(name);
        let directory = match repo.common_dir().open_verified_directory(OsStr::new(name)) {
            Ok(directory) => directory,
            Err(error) if error.is_not_found() => continue,
            Err(error) => return Err(PermissionError::Filesystem(error)),
        };
        for (seen, entry) in directory
            .entries()
            .map_err(PermissionError::Filesystem)?
            .enumerate()
        {
            if seen == ENTRIES_PER_DIRECTORY {
                return Err(PermissionError::GitControlEntriesExceeded {
                    limit: ENTRIES_PER_DIRECTORY,
                    path: parent,
                });
            }
            check(
                &directory,
                &parent,
                &entry.map_err(PermissionError::Filesystem)?,
            )?;
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub(super) fn verify_selected_head(
    worktree: &VerifiedWorktree,
    branch: &str,
) -> Result<(), PermissionError> {
    let directory = worktree.repo().private_dir();
    let name = OsStr::new("HEAD");
    let before = directory
        .metadata_at(name, false)
        .map_err(PermissionError::Filesystem)?;
    if before.kind != FileKind::Regular || before.links != 1 || before.size > MAX_GIT_HEAD_BYTES {
        return Err(PermissionError::GitWriteAuthorityMissing);
    }
    let mut file = directory
        .open_read(name)
        .map_err(PermissionError::Filesystem)?;
    let opened = file.metadata().map_err(PermissionError::Filesystem)?;
    let changed = || {
        anchor_at(
            &worktree.repo().private_git_dir().join("HEAD"),
            "HEAD changed while it was read",
        )
    };
    if !opened.same_identity(&before) {
        return Err(changed());
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_GIT_HEAD_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(PermissionError::HeadRead)?;
    let after = file.metadata().map_err(PermissionError::Filesystem)?;
    let named_after = directory
        .metadata_at(name, false)
        .map_err(PermissionError::Filesystem)?;
    let byte_count =
        u64::try_from(bytes.len()).map_err(|_| PermissionError::GitWriteAuthorityMissing)?;
    if !after.same_identity(&opened)
        || !named_after.same_identity(&opened)
        || after.size != opened.size
        || byte_count != opened.size
        || byte_count > MAX_GIT_HEAD_BYTES
    {
        return Err(changed());
    }
    let expected = format!("ref: refs/heads/{branch}\n");
    if bytes.as_slice() != expected.as_bytes() {
        return Err(PermissionError::GitWriteAuthorityMissing);
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn validate_profile_spec(spec: &ProviderProfileSpec) -> Result<(), PermissionError> {
    validate_profile_spec_impl(spec, None, MAX_PATH_GRANTS)
}

#[cfg(any(target_os = "macos", test))]
pub(super) fn validate_profile_spec_with_home(
    spec: &ProviderProfileSpec,
    home: &Path,
    grant_limit: usize,
) -> Result<(), PermissionError> {
    validate_profile_spec_impl(spec, Some(home), grant_limit)
}

#[cfg(any(target_os = "macos", test))]
fn validate_profile_spec_impl(
    spec: &ProviderProfileSpec,
    home: Option<&Path>,
    grant_limit: usize,
) -> Result<(), PermissionError> {
    for (family, count, limit) in [
        (
            "process executables",
            spec.process_executables.len(),
            grant_limit,
        ),
        ("runtime reads", spec.runtime_reads.len(), grant_limit),
        (
            "discovery reads",
            spec.discovery_reads
                .iter()
                .filter(|grant| valid_discovery_read(grant))
                .count(),
            grant_limit,
        ),
        (
            "authentication profiles",
            spec.authentication.len(),
            grant_limit.max(16),
        ),
        (
            "continuation reads",
            spec.continuation_reads.len(),
            grant_limit,
        ),
        ("state writes", spec.state_writes.len(), grant_limit),
        (
            "tool execution roots",
            spec.tool_exec_roots.len(),
            grant_limit,
        ),
        ("tool writes", spec.tool_writes.len(), grant_limit),
        ("provider endpoints", spec.provider_api.len(), grant_limit),
        (
            "native controls",
            spec.native_controls.len(),
            grant_limit.max(64),
        ),
    ] {
        if count > limit {
            return Err(PermissionError::GrantLimitExceeded { family, limit });
        }
    }
    if spec.process_executables.is_empty() {
        return Err(PermissionError::ProcessPolicyMissing);
    }
    for executable in &spec.process_executables {
        validate_read_exec_grant(executable).map_err(|error| {
            if home.is_some() {
                error.with_credential_path(&executable.path, home)
            } else {
                error
            }
        })?;
        if executable.scope != PathScope::Exact {
            return Err(PermissionError::InvalidPath(executable.path.clone()));
        }
    }
    if let Some(socket) = &spec.fabric_socket {
        validate_noncredential_grant(socket).map_err(|error| {
            if home.is_some() {
                error.with_credential_path(&socket.path, home)
            } else {
                error
            }
        })?;
        if socket.scope != PathScope::Exact {
            return Err(PermissionError::InvalidPath(socket.path.clone()));
        }
    }
    for grant in &spec.runtime_reads {
        validate_read_exec_grant(grant).map_err(|error| {
            if home.is_some() {
                error.with_credential_path(&grant.path, home)
            } else {
                error
            }
        })?;
    }
    for auth in &spec.authentication {
        if auth.mechanism.is_empty()
            || auth.mechanism.len() > 128
            || auth
                .mechanism
                .bytes()
                .any(|byte| !(byte.is_ascii_alphanumeric() || b"._-".contains(&byte)))
            || auth.roots.is_empty()
        {
            return Err(PermissionError::InvalidFixture);
        }
        for grant in &auth.roots {
            grant.validate()?;
            if is_broad_root(&grant.path) {
                return Err(PermissionError::BroadRoot);
            }
        }
        for (family, count) in [
            ("authentication roots", auth.roots.len()),
            ("authentication writes", auth.in_place_writes.len()),
        ] {
            if count > grant_limit {
                return Err(PermissionError::GrantLimitExceeded {
                    family,
                    limit: grant_limit,
                });
            }
        }
        for write in &auth.in_place_writes {
            write.validate()?;
            if write.scope != PathScope::Exact
                || !auth.roots.iter().any(|root| match root.scope {
                    PathScope::Exact => root.path == write.path,
                    PathScope::Subtree => write.path.starts_with(&root.path),
                })
            {
                return Err(PermissionError::InvalidFixture);
            }
        }
    }
    for grant in &spec.continuation_reads {
        validate_noncredential_grant(grant).map_err(|error| {
            if home.is_some() {
                error.with_credential_path(&grant.path, home)
            } else {
                error
            }
        })?;
    }
    for root in &spec.tool_exec_roots {
        validate_read_exec_grant(root).map_err(|error| {
            if home.is_some() {
                error.with_credential_path(&root.path, home)
            } else {
                error
            }
        })?;
    }
    // State may sit under a provider's home (Claude keeps transcripts beside
    // its configuration), so only the authentication roots themselves are off
    // limits, together with the ordinary path and breadth checks.
    for grant in &spec.state_writes {
        grant.validate()?;
        if spec
            .authentication
            .iter()
            .flat_map(|auth| auth.roots.iter().chain(&auth.in_place_writes))
            .any(|root| grant.overlaps(root))
        {
            return Err(if home.is_some() {
                PermissionError::credential_root_at(&grant.path, home)
            } else {
                PermissionError::CredentialRoot
            });
        }
    }
    for grant in &spec.tool_writes {
        validate_noncredential_grant(grant).map_err(|error| {
            if home.is_some() {
                error.with_credential_path(&grant.path, home)
            } else {
                error
            }
        })?;
    }
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
pub(super) fn valid_native_control(control: &str) -> bool {
    !control.is_empty()
        && control.len() <= 128
        && control
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

pub(super) fn valid_discovery_read(grant: &PathGrant) -> bool {
    grant.validate().is_ok()
        && grant.scope == PathScope::Exact
        && profile_path_representable(&grant.path)
        && matches!(
            grant.path.file_name().and_then(OsStr::to_str),
            Some(".agents" | ".claude" | ".codex")
        )
}

pub(crate) fn validate_noncredential_grant(grant: &PathGrant) -> Result<(), PermissionError> {
    grant.validate()?;
    if refuses_credential_grant(grant, false) {
        return Err(PermissionError::CredentialRoot);
    }
    Ok(())
}

/// Whether a lane root at `path` must be refused because it is credential
/// named or encloses a nested credential root. Read and exec roots allow the
/// Codex standalone package, which is credential named but holds no
/// credentials. `home` is the host's canonical home, wherever it lives; a root
/// that holds it or its nested credential roots is refused too. Entry checks
/// and the compiler share this predicate so they cannot drift.
pub fn refuses_lane_root(path: &Path, read: bool, home: Option<&Path>) -> bool {
    let grant = PathGrant {
        path: path.to_owned(),
        scope: PathScope::Subtree,
    };
    refuses_credential_grant(&grant, read)
        || home.is_some_and(|home| encloses_home_credentials(&grant, home))
}

/// The grant-level form of [`refuses_lane_root`], honouring an exact grant's
/// narrower reach.
pub(super) fn refuses_credential_grant(grant: &PathGrant, read: bool) -> bool {
    if read && is_codex_standalone_package(&grant.path) && !encloses_credential_root(grant) {
        return false;
    }
    is_credential_root(&grant.path) || encloses_credential_root(grant)
}

/// A subtree of `/Users/<name>` or `/home/<name>` that holds a nested
/// credential root (`~/.config`, `~/Library`, `~/Archives`) would expose it.
/// The home itself is a broad root; [`refuses_lane_root`] and `compile_sbpl`
/// repeat this check against the host's actual home, wherever it lives.
pub(super) fn encloses_credential_root(grant: &PathGrant) -> bool {
    grant.scope == PathScope::Subtree && encloses_credential_root_path(&grant.path)
}

pub(super) fn encloses_credential_root_path(path: &Path) -> bool {
    let mut normal = path.components().filter_map(|component| match component {
        Component::Normal(value) => Some(value),
        _ => None,
    });
    match (normal.next(), normal.next()) {
        (Some(root), Some(user)) if root == "Users" || root == "home" => {
            encloses_home_credentials_path(path, &Path::new("/").join(root).join(user))
        }
        _ => false,
    }
}

/// A subtree that holds `home` itself or one of its nested credential roots.
pub(super) fn encloses_home_credentials(grant: &PathGrant, home: &Path) -> bool {
    grant.scope == PathScope::Subtree && encloses_home_credentials_path(&grant.path, home)
}

pub(super) fn encloses_home_credentials_path(path: &Path, home: &Path) -> bool {
    home.starts_with(path)
        || [
            ".config",
            "Library",
            "Library/Application Support",
            "Archives",
        ]
        .iter()
        .any(|nested| path == home.join(nested))
}

/// Validates a grant that confers only read and exec. It is the one place the
/// Codex standalone package exception applies: writes, scratch, protected
/// roots and private state keep the strict credential-root check.
pub(crate) fn validate_read_exec_grant(grant: &PathGrant) -> Result<(), PermissionError> {
    grant.validate()?;
    if refuses_credential_grant(grant, true) {
        return Err(PermissionError::CredentialRoot);
    }
    Ok(())
}

/// Exactly `<home>/.codex/packages/standalone/releases/<release>` or a path
/// beneath it, where `<home>` is `/Users/<name>` or `/home/<name>`. Neither
/// `~/.codex`, `packages`, `standalone`, `releases` nor any sibling (auth,
/// config, sessions, state, logs) matches, and nothing credential-named may
/// appear below the release directory.
pub fn is_codex_standalone_package(path: &Path) -> bool {
    let normal: Vec<&OsStr> = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value),
            _ => None,
        })
        .collect();
    let [
        root,
        _user,
        codex,
        packages,
        standalone,
        releases,
        _release,
        below @ ..,
    ] = normal.as_slice()
    else {
        return false;
    };
    (*root == OsStr::new("Users") || *root == OsStr::new("home"))
        && *codex == OsStr::new(".codex")
        && *packages == OsStr::new("packages")
        && *standalone == OsStr::new("standalone")
        && *releases == OsStr::new("releases")
        && !is_credential_root(&below.iter().collect::<PathBuf>())
}

/// Whether a path can be encoded losslessly in the sandbox profile language.
pub fn profile_path_representable(path: &Path) -> bool {
    path.to_str().is_some_and(|text| {
        text.chars()
            .all(|character| !character.is_control() || matches!(character, '\n' | '\r' | '\t'))
    })
}

pub(crate) fn validate_absolute(path: &Path) -> Result<(), PermissionError> {
    let bytes = path.as_os_str().as_encoded_bytes();
    if !path.is_absolute()
        || bytes.contains(&0)
        || bytes.len() > MAX_PATH_BYTES
        || bytes.len() <= 1
        || bytes.ends_with(b"/")
        || bytes
            .split(|byte| *byte == b'/')
            .skip(1)
            .any(|part| part.is_empty() || part == b"." || part == b"..")
        || path.components().any(|part| {
            matches!(
                part,
                Component::CurDir | Component::ParentDir | Component::Prefix(_)
            )
        })
    {
        return Err(PermissionError::InvalidPath(path.to_owned()));
    }
    Ok(())
}

pub(super) fn is_broad_root(path: &Path) -> bool {
    let normal: Vec<&OsStr> = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value),
            _ => None,
        })
        .collect();
    if normal.is_empty() {
        return true;
    }
    let first = normal[0].to_string_lossy();
    (first == "Users" || first == "home") && normal.len() <= 2
        || (first == "tmp" && normal.len() == 1)
        || (first == "private"
            && (normal.len() == 1
                || (normal.len() == 2 && normal.get(1).is_some_and(|value| *value == "tmp"))
                || (normal.len() == 2 && normal.get(1).is_some_and(|value| *value == "var"))
                || (normal.len() == 3
                    && normal.get(1).is_some_and(|value| *value == "var")
                    && normal.get(2).is_some_and(|value| *value == "tmp"))))
        || (first == "var"
            && (normal.len() == 1
                || (normal.len() == 2 && normal.get(1).is_some_and(|value| *value == "tmp"))))
}

pub(super) const CREDENTIAL_DIRECTORIES: &[&str] = &[
    ".ssh", ".aws", ".azure", ".gnupg", ".kube", ".secrets", ".codex", ".claude", ".cursor",
    ".gemini", "Monero",
];
pub(super) const NESTED_CREDENTIAL_DIRECTORIES: &[&str] = &[
    ".config/gh",
    ".config/gcloud",
    "Library/Keychains",
    "Archives/git-preservation",
    "Library/Application Support/GitHub CLI",
];

// Additional provider stores can contain credentials alongside their state.
// Keep these command-only denials out of model profile validation.
pub(super) const PROVIDER_CREDENTIAL_DIRECTORIES: &[&str] = &[
    ".config/cursor",
    ".config/opencode",
    ".local/share/opencode",
];

/// Whether a path lies within a known credential store.
pub fn is_credential_root(path: &Path) -> bool {
    let components: Vec<&OsStr> = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value),
            _ => None,
        })
        .collect();
    if components.iter().any(|component| {
        CREDENTIAL_DIRECTORIES
            .iter()
            .any(|name| *component == OsStr::new(name))
    }) {
        return true;
    }
    NESTED_CREDENTIAL_DIRECTORIES.iter().any(|relative| {
        let parts: Vec<_> = Path::new(relative)
            .components()
            .map(|part| part.as_os_str())
            .collect();
        components
            .windows(parts.len())
            .any(|window| window == parts)
    })
}

pub(super) fn path_overlaps(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

#[cfg(any(target_os = "macos", test, feature = "snapshot"))]
pub(super) fn validate_endpoint(endpoint: &NetworkEndpoint) -> Result<(), PermissionError> {
    if endpoint.port == 0
        || endpoint.port > u64::from(u16::MAX)
        || !matches!(endpoint.address.as_str(), "*" | "localhost")
    {
        return Err(PermissionError::InvalidFixture);
    }
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
pub(super) fn filtered_native_controls(controls: &[String]) -> Vec<String> {
    let filtered: Vec<_> = controls
        .iter()
        .filter(|control| valid_native_control(control))
        .cloned()
        .collect();
    if filtered.len() != controls.len() {
        use std::io::Write;
        let _ = writeln!(
            std::io::stderr().lock(),
            "warning: invalid native control labels were omitted"
        );
    }
    filtered
}
