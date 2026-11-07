//! SBPL compilation and rendering.

#[cfg(target_os = "macos")]
use super::validation::filtered_native_controls;
use super::*;
#[cfg(target_os = "macos")]
use crate::cache::verify_cache_metadata;
#[cfg(target_os = "macos")]
use sha2::{Digest, Sha256};
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

#[cfg(any(target_os = "macos", feature = "snapshot"))]
pub(super) const DYLD_BOOTSTRAP_RULES: &str = r#"
(deny file-link)
(allow file-read* (subpath "/System/Library"))
(allow file-read* (subpath "/usr/bin"))
(allow file-read* (subpath "/usr/lib"))
(allow file-read* (subpath "/usr/libexec"))
(allow file-read* (subpath "/bin"))
(allow file-read* file-test-existence file-map-executable
  (subpath "/System/Volumes/Preboot/Cryptexes/App/System")
  (subpath "/System/Volumes/Preboot/Cryptexes/Incoming/OS")
  (subpath "/System/Volumes/Preboot/Cryptexes/OS")
  (subpath "/System/Volumes/Preboot/Cryptexes/Rosetta")
  (subpath "/System/Cryptexes/App")
  (subpath "/System/Cryptexes/OS")
  (subpath "/System/Cryptexes/Rosetta"))
(allow file-read* file-test-existence
  (path-ancestors "/System/Volumes/Preboot/Cryptexes/App/System")
  (path-ancestors "/System/Volumes/Preboot/Cryptexes/Incoming/OS")
  (path-ancestors "/System/Volumes/Preboot/Cryptexes/OS")
  (path-ancestors "/System/Volumes/Preboot/Cryptexes/Rosetta")
  (path-ancestors "/System/Cryptexes/App")
  (path-ancestors "/System/Cryptexes/OS")
  (path-ancestors "/System/Cryptexes/Rosetta"))
(allow file-read* file-test-existence (literal "/"))
(allow file-map-executable
  (subpath "/Library/Apple/System/Library/Frameworks")
  (subpath "/Library/Apple/System/Library/PrivateFrameworks")
  (subpath "/Library/Apple/usr/lib")
  (subpath "/System/Library/Extensions")
  (subpath "/System/Library/Frameworks")
  (subpath "/System/Library/PrivateFrameworks")
  (subpath "/System/Library/SubFrameworks")
  (subpath "/System/iOSSupport/System/Library/Frameworks")
  (subpath "/System/iOSSupport/System/Library/PrivateFrameworks")
  (subpath "/System/iOSSupport/System/Library/SubFrameworks")
  (subpath "/System/iOSSupport/usr/lib")
  (subpath "/usr/lib"))
(allow syscall-unix
  (syscall-number SYS___mac_syscall)
  (syscall-number SYS_getfsstat SYS_getfsstat64)
  (syscall-number SYS_map_with_linking_np)
  (syscall-number SYS_open SYS_openat)
  (syscall-number SYS_fstatat SYS_fstatat64)
  (syscall-number SYS_dup))
(allow system-fcntl
  (fcntl-command F_ADDFILESIGS_RETURN F_CHECK_LV F_GETPATH))
(with-filter (mac-policy-name "Sandbox")
  (allow system-mac-syscall (mac-syscall-number 2)))
"#;
/// Operating-system runtime grants shared with the backend probe:
/// kernel and preference reads, TLS trust, DNS, time zone data, process
/// information, and signals within the same sandbox. It names no user path; explicit
/// secret-root denials still override the metadata grant.
#[cfg(any(target_os = "macos", feature = "snapshot"))]
pub(super) const RUNTIME_BASELINE_RULES: &str = r#"(allow sysctl-read)
(deny sysctl-read (sysctl-name-prefix "kern.procargs"))
(deny mach-lookup (global-name-prefix "dev.provenant.attempt."))
(allow file-read-metadata)
(allow ipc-posix-shm-read* (ipc-posix-name-prefix "apple.cfprefs."))
(deny signal)
(allow signal (target same-sandbox))
(allow process-info*)
(deny mach-priv-task-port mach-task-read)
(allow mach-priv-task-port mach-task-read (target same-sandbox))
(deny process-info-setcontrol process-info-dirtycontrol)
(allow process-info-setcontrol process-info-dirtycontrol (target same-sandbox))
(allow mach-lookup
  (global-name "com.apple.cfprefsd.agent")
  (global-name "com.apple.cfprefsd.daemon")
  (global-name "com.apple.SecurityServer")
  (global-name "com.apple.trustd")
  (global-name "com.apple.trustd.agent")
  (global-name "com.apple.ocspd")
  (global-name "com.apple.SystemConfiguration.configd")
  (global-name "com.apple.SystemConfiguration.DNSConfiguration")
  (global-name "com.apple.system.opendirectoryd.libinfo")
  (global-name "com.apple.networkd"))
(allow network-outbound (remote unix-socket (path-literal "/private/var/run/mDNSResponder")))
(allow file-read* file-write-data (literal "/dev/null"))
(allow file-read-metadata file-write-data (literal "/private/var/tmp"))
(allow file-read* file-write* (regex #"^/private/var/tmp/sh-thd[^/]*$"))
(allow file-read* (literal "/dev/urandom") (literal "/dev/random") (literal "/dev/zero"))
(allow file-read* (literal "/dev/fd"))
(allow file-read* (subpath "/usr/share") (subpath "/private/var/db/timezone") (literal "/private/etc/localtime"))
"#;

/// Localhost services a build or test run talks to: any loopback TCP port,
/// its own listeners, and a local Postgres server's Unix socket.
#[cfg(any(target_os = "macos", feature = "snapshot"))]
pub(super) const LOCAL_SERVICE_RULES: &str = r##"(allow network-bind (local ip "localhost:*"))
(allow network-inbound (local ip "localhost:*"))
(allow network-outbound (remote ip "localhost:*"))
(allow network-outbound (remote unix-socket (path-regex #"^/private/tmp/\.s\.PGSQL\.[0-9]+$")))
"##;
/// `openpty` through the multiplexer. The kernel issues the pty extension only
/// for a terminal this sandbox allocates, so the user's own `ttys` stay denied.
#[cfg(any(target_os = "macos", feature = "snapshot"))]
pub(super) const PSEUDO_TTY_RULES: &str = r##"(allow pseudo-tty)
(allow file-read* file-write* file-ioctl (literal "/dev/ptmx"))
(allow file-read* file-write* file-ioctl (require-all (regex #"^/dev/ttys[0-9]+$") (extension "com.apple.sandbox.pty")))
"##;
/// Default profile-byte warning threshold for renderer fixtures.
#[cfg(any(test, feature = "snapshot"))]
pub(super) const PROFILE_LIMIT: usize = 512 * 1024;

#[cfg(all(target_os = "macos", any(test, feature = "snapshot")))]
pub(super) fn compile_sbpl(
    plan: &PlanPermissions,
    profile: &ProviderProfile,
    anchors: &Anchors<'_>,
) -> Result<ConfinedLaunch, PermissionError> {
    compile_sbpl_impl(plan, profile, anchors, false, ProfileLimits::default())
}

#[cfg(target_os = "macos")]
pub(super) fn compile_sbpl_with_diagnostic(
    plan: &PlanPermissions,
    profile: &ProviderProfile,
    anchors: &Anchors<'_>,
    limits: ProfileLimits,
) -> Result<ConfinedLaunch, PermissionError> {
    // Enforcing launches require the complete host contract. The pure snapshot
    // seam may still render standalone fixtures with the shared baseline.
    if plan.policy.is_none() {
        return Err(PermissionError::PermissionNotRecorded);
    }
    compile_sbpl_impl(plan, profile, anchors, true, limits)
}

#[cfg(target_os = "macos")]
fn compile_sbpl_impl(
    plan: &PlanPermissions,
    profile: &ProviderProfile,
    anchors: &Anchors<'_>,
    diagnostic: bool,
    limits: ProfileLimits,
) -> Result<ConfinedLaunch, PermissionError> {
    anchors.verify_all()?;
    if let Some(policy) = &plan.policy {
        policy.mandatory().revalidate_paths()?;
    }
    if plan.nested_confinement {
        return Err(PermissionError::NestedConfinementUnsupported);
    }
    // Capacity is advisory; path and authority validation still runs in full.
    let plan_count = plan
        .read_roots
        .len()
        .saturating_add(plan.protected_roots.len())
        .saturating_add(plan.secret_roots.len())
        .saturating_add(plan.write_roots.len());
    let spec = &profile.spec;
    let profile_count = [
        spec.process_executables.len(),
        spec.runtime_reads.len(),
        spec.discovery_reads.len(),
        spec.authentication.len(),
        spec.continuation_reads.len(),
        spec.state_writes.len(),
        spec.tool_exec_roots.len(),
        spec.tool_writes.len(),
        spec.provider_api.len(),
        spec.native_controls.len(),
        spec.authentication
            .iter()
            .map(|auth| auth.roots.len().max(auth.in_place_writes.len()))
            .max()
            .unwrap_or(0),
    ]
    .into_iter()
    .max()
    .unwrap_or(0);
    report_capacity_warning(capacity_warning(
        "domains.run.grant_limit",
        plan_count.max(profile_count),
        limits.grant_limit,
        limits.grant_origin,
    ));
    validate_plan_grant_limit(plan, usize::MAX)?;
    validate_profile_spec_with_home(&profile.spec, anchors.home, usize::MAX)?;
    for grant in &plan.read_roots {
        validate_read_exec_grant(grant)
            .map_err(|error| error.with_credential_path(&grant.path, Some(anchors.home)))?;
    }
    for grant in &plan.protected_roots {
        validate_noncredential_grant(grant)
            .map_err(|error| error.with_credential_path(&grant.path, Some(anchors.home)))?;
    }
    let worktree_grants = match &plan.mode {
        AccessMode::ReadOnly => None,
        AccessMode::WorktreeWrite(authority) => {
            Some(verify_worktree_authority(authority, anchors)?)
        }
    };
    if let Some(read_only) = &plan.read_only {
        if !matches!(plan.mode, AccessMode::ReadOnly) {
            return Err(PermissionError::InvalidReadOnlyPolicy);
        }
        read_only.revalidate()?;
        for grant in &read_only.writes {
            validate_noncredential_grant(grant)
                .map_err(|error| error.with_credential_path(&grant.path, Some(anchors.home)))?;
            if plan
                .protected_roots
                .iter()
                .chain(&plan.secret_roots)
                .chain(&read_only.git_reads)
                .any(|p| grant.overlaps(p))
            {
                return Err(PermissionError::ProtectedOverlap);
            }
            anchors.verify_grant_held(grant)?;
        }
    }
    if let Some(policy) = &plan.policy {
        for grant in &plan.write_roots {
            policy.mandatory().validate_extra_write(grant)?;
            let canonical = resolve_boundary_path(&grant.path)?;
            policy.mandatory().validate_extra_write(&PathGrant {
                path: canonical,
                scope: grant.scope,
            })?;
        }
    }
    let grants = GrantSet::build(plan, profile, anchors.home, worktree_grants)?;
    let read_grants = &grants.reads;
    let discovery = || {
        profile
            .spec
            .discovery_reads
            .iter()
            .filter(|grant| super::validation::valid_discovery_read(grant))
    };
    let all_reads = || read_grants.iter().chain(discovery());
    let credential_writes = &grants.credential_writes;
    let worktree_grants = grants.worktree.as_ref();

    let shared_writes: Vec<&PathGrant> = plan
        .write_roots
        .iter()
        .chain(&profile.spec.tool_writes)
        .collect();
    // The path checks above know only conventional homes; the host's own home
    // is broad and its nested credential roots stay hidden wherever it lives.
    if let Some(grant) = all_reads()
        .chain(shared_writes.iter().copied())
        .chain(&profile.spec.tool_exec_roots)
        .chain(&profile.spec.state_writes)
        .find(|grant| encloses_home_credentials(grant, anchors.home))
    {
        return Err(if diagnostic {
            PermissionError::credential_root_at(&grant.path, Some(anchors.home))
        } else {
            PermissionError::CredentialRoot
        });
    }
    for grant in read_grants {
        anchors.verify_read_held(grant)?;
    }
    for grant in discovery() {
        anchors.verify_discovery_grant(grant)?;
    }
    for executable in &profile.spec.process_executables {
        anchors.verify_executable(executable)?;
    }
    for grant in credential_writes {
        anchors.verify_single_link_grant(grant)?;
    }
    verify_state_writes(plan, profile, anchors)?;
    if let AccessMode::WorktreeWrite(authority) = &plan.mode {
        verify_state_write(plan, &authority.cargo_cache, anchors)?;
        let cache = anchors
            .directories
            .iter()
            .find(|anchor| anchor.path == authority.cargo_cache.path)
            .ok_or_else(|| anchor_at(&authority.cargo_cache.path, "lane cache anchor not held"))?;
        verify_cache_metadata(
            &cache
                .directory
                .metadata()
                .map_err(PermissionError::Filesystem)?,
        )?;
    }
    if plan.scratch.scope != PathScope::Subtree {
        return Err(PermissionError::InvalidPath(plan.scratch.path.clone()));
    }
    validate_noncredential_grant(&plan.scratch)
        .map_err(|error| error.with_credential_path(&plan.scratch.path, Some(anchors.home)))?;
    anchors.verify_grant_held(&plan.scratch)?;
    let mut scratch_exclusions: Vec<&PathGrant> = all_reads()
        .chain(&plan.protected_roots)
        .chain(&plan.secret_roots)
        .chain(&profile.spec.continuation_reads)
        .chain(&profile.spec.state_writes)
        .chain(
            profile
                .spec
                .authentication
                .iter()
                .flat_map(|authentication| &authentication.roots),
        )
        .collect();
    if let Some(read_only) = &plan.read_only {
        scratch_exclusions.extend(read_only.git_reads.iter().chain(&read_only.writes));
    }
    scratch_exclusions.extend(&shared_writes);
    if let Some(grants) = &worktree_grants {
        scratch_exclusions.extend([
            &grants.worktree,
            &grants.private_git_dir,
            &grants.common_git_dir,
        ]);
    }
    if let Some((verified, _)) = anchors.worktree {
        for path in [
            verified.path(),
            verified.repo().private_git_dir(),
            verified.repo().common_git_dir(),
        ] {
            if std::iter::once(&plan.scratch)
                .chain(&plan.private_state)
                .any(|private| path_overlaps(&private.path, path))
            {
                return Err(PermissionError::ProtectedOverlap);
            }
        }
    }
    verify_private_scratch(&plan.scratch, &scratch_exclusions, anchors)?;
    if let Some(state) = &plan.private_state {
        validate_noncredential_grant(state)
            .map_err(|error| error.with_credential_path(&state.path, Some(anchors.home)))?;
        if state.overlaps(&plan.scratch) {
            return Err(PermissionError::ProtectedOverlap);
        }
        verify_private_scratch(state, &scratch_exclusions, anchors)?;
    }
    verify_shared_writes(plan, &shared_writes, anchors)?;

    for secret in &plan.secret_roots {
        secret.validate()?;
        anchors.verify_grant_held(secret)?;
    }
    for read in all_reads() {
        if plan.secret_roots.iter().any(|secret| read.overlaps(secret)) {
            return Err(PermissionError::ProtectedOverlap);
        }
    }
    if plan
        .protected_roots
        .iter()
        .chain(&plan.secret_roots)
        .any(|protected| plan.scratch.overlaps(protected))
    {
        return Err(PermissionError::ProtectedOverlap);
    }
    let worktree_protected_overlap = worktree_grants.as_ref().is_some_and(|grants| {
        plan.protected_roots
            .iter()
            .chain(&plan.secret_roots)
            .any(|protected| {
                grants.worktree.path.starts_with(&protected.path)
                    && grants.worktree.path != protected.path
            })
    });
    if worktree_protected_overlap {
        return Err(PermissionError::ProtectedOverlap);
    }

    let mut profile_bytes = render_with_limit(&grants, usize::MAX)?.into_bytes();
    report_capacity_warning(capacity_warning(
        "domains.run.profile_limit_bytes",
        profile_bytes.len(),
        limits.profile_limit_bytes,
        limits.profile_origin,
    ));
    let profile_digest = Sha256::digest(&profile_bytes).into();
    // SBPL grants pathnames, so rebind them to their held identities after
    // rendering, immediately before returning the launch profile.
    if let AccessMode::WorktreeWrite(authority) = &plan.mode {
        verify_worktree_authority(authority, anchors)?;
    }
    verify_private_scratch(&plan.scratch, &scratch_exclusions, anchors)?;
    if let Some(state) = &plan.private_state {
        verify_private_scratch(state, &scratch_exclusions, anchors)?;
    }
    verify_state_writes(plan, profile, anchors)?;
    if let Some(read_only) = &plan.read_only {
        read_only.revalidate()?;
        for grant in &read_only.writes {
            anchors.verify_grant_held(grant)?;
        }
    }
    if let AccessMode::WorktreeWrite(authority) = &plan.mode {
        verify_state_write(plan, &authority.cargo_cache, anchors)?;
        let cache = anchors
            .directories
            .iter()
            .find(|anchor| anchor.path == authority.cargo_cache.path)
            .ok_or_else(|| anchor_at(&authority.cargo_cache.path, "lane cache anchor not held"))?;
        verify_cache_metadata(
            &cache
                .directory
                .metadata()
                .map_err(PermissionError::Filesystem)?,
        )?;
    }
    anchors.verify_all()?;
    for grant in discovery() {
        anchors.verify_discovery_grant(grant)?;
    }
    if let Some(policy) = &plan.policy {
        policy.mandatory().revalidate_paths()?;
    }
    // Avoid retaining a second mutable representation of the profile.
    profile_bytes.shrink_to_fit();
    let native_controls = filtered_native_controls(&profile.spec.native_controls);
    Ok(ConfinedLaunch {
        enforcement: Enforcement::MacOsSandboxExec,
        profile: profile_bytes,
        profile_digest,
        native_controls,
    })
}

/// All path grants and inputs used to render one launch profile.
///
/// Build this once from the launch inputs, then use the same set for
/// validation, rendering and the post-render checks.
#[cfg(any(target_os = "macos", feature = "snapshot"))]
pub(crate) struct GrantSet<'a> {
    plan: &'a PlanPermissions,
    profile: &'a ProviderProfile,
    home: &'a Path,
    pub(super) reads: Vec<PathGrant>,
    credential_writes: Vec<PathGrant>,
    worktree: Option<WorktreeWriteGrants>,
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
impl<'a> GrantSet<'a> {
    pub(crate) fn build(
        plan: &'a PlanPermissions,
        profile: &'a ProviderProfile,
        home: &'a Path,
        worktree: Option<WorktreeWriteGrants>,
    ) -> Result<Self, PermissionError> {
        let mut reads = plan.read_roots.clone();
        if let Some(read_only) = &plan.read_only {
            reads.extend(read_only.git_reads.iter().cloned());
        }
        if let Some(grants) = worktree.as_ref() {
            reads.push(grants.worktree.clone());
        }
        reads.extend(plan.protected_roots.iter().cloned());
        reads.extend(profile.spec.runtime_reads.iter().cloned());
        reads.extend(profile.spec.process_executables.iter().cloned());
        let mut credential_writes = Vec::new();
        if let Some(mechanism) = &plan.selected_authentication {
            let selected = profile
                .spec
                .authentication
                .iter()
                .find(|item| &item.mechanism == mechanism)
                .ok_or(PermissionError::PermissionNotRecorded)?;
            reads.extend(selected.roots.iter().cloned());
            credential_writes.extend(selected.in_place_writes.iter().cloned());
        }
        if plan.include_continuation {
            reads.extend(profile.spec.continuation_reads.iter().cloned());
        }
        Ok(Self {
            plan,
            profile,
            home,
            reads,
            credential_writes,
            worktree,
        })
    }
}

/// Render a built grant set without filesystem or environment access.
#[cfg(any(test, feature = "snapshot"))]
pub(crate) fn render(grants: &GrantSet<'_>) -> Result<String, PermissionError> {
    render_with_limit(grants, PROFILE_LIMIT)
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
pub(super) fn render_with_limit(
    grants: &GrantSet<'_>,
    limit: usize,
) -> Result<String, PermissionError> {
    let source = String::from_utf8(render_profile_with_limit(
        grants.plan,
        grants.profile,
        &grants.reads,
        &grants.credential_writes,
        grants.worktree.as_ref(),
        grants.home,
        limit,
    )?)
    .map_err(|_| PermissionError::NonUtf8Path)?;
    Ok(source)
}

/// Provider state writes stay outside every secret, protected and Git root.
#[cfg(target_os = "macos")]
pub(super) fn verify_state_writes(
    plan: &PlanPermissions,
    profile: &ProviderProfile,
    anchors: &Anchors<'_>,
) -> Result<(), PermissionError> {
    for grant in &profile.spec.state_writes {
        verify_state_write(plan, grant, anchors)?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub(super) fn verify_state_write(
    plan: &PlanPermissions,
    grant: &PathGrant,
    anchors: &Anchors<'_>,
) -> Result<(), PermissionError> {
    grant.validate()?;
    if plan
        .secret_roots
        .iter()
        .chain(&plan.protected_roots)
        .chain(std::iter::once(&plan.scratch))
        .chain(&plan.private_state)
        .any(|root| grant.overlaps(root))
    {
        return Err(PermissionError::ProtectedOverlap);
    }
    if let Some((verified, _)) = anchors.worktree {
        for path in [
            verified.path(),
            verified.repo().private_git_dir(),
            verified.repo().common_git_dir(),
        ] {
            if path_overlaps(&grant.path, path) {
                return Err(PermissionError::ProtectedOverlap);
            }
        }
    }
    match grant.scope {
        PathScope::Subtree => anchors.verify_grant_held(grant)?,
        PathScope::Exact => anchors.verify_single_link_grant(grant)?,
    }
    Ok(())
}

/// Shared tool stores and dispatch write roots are not inspected beyond this:
/// they can be large (a pnpm store), and the child cannot add hard links.
/// They still stay clear of every secret, protected, private and Git root.
#[cfg(target_os = "macos")]
pub(super) fn verify_shared_writes(
    plan: &PlanPermissions,
    writes: &[&PathGrant],
    anchors: &Anchors<'_>,
) -> Result<(), PermissionError> {
    for grant in writes {
        validate_noncredential_grant(grant)
            .map_err(|error| error.with_credential_path(&grant.path, Some(anchors.home)))?;
        if plan
            .secret_roots
            .iter()
            .chain(&plan.protected_roots)
            .chain(std::iter::once(&plan.scratch))
            .chain(&plan.private_state)
            .chain(plan.read_only.iter().flat_map(|grants| &grants.git_reads))
            .any(|root| grant.overlaps(root))
            || mandatory_for_plan(plan, anchors.home, None)?
                .credentials()
                .iter()
                .any(|root| grant.overlaps(root))
        {
            return Err(PermissionError::ProtectedOverlap);
        }
        if let Some((verified, _)) = anchors.worktree {
            for path in [
                verified.repo().private_git_dir(),
                verified.repo().common_git_dir(),
            ] {
                if path_overlaps(&grant.path, path) {
                    return Err(PermissionError::ProtectedOverlap);
                }
            }
        }
    }
    Ok(())
}

/// GitHub CLI configuration roots that networked read-only lanes may read.
#[cfg(all(test, any(target_os = "macos", feature = "snapshot")))]
pub(super) fn gh_config_paths(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".config/gh"),
        home.join("Library/Application Support/GitHub CLI"),
    ]
}

#[cfg(target_os = "macos")]
pub(super) fn verify_private_scratch(
    scratch: &PathGrant,
    exclusions: &[&PathGrant],
    anchors: &Anchors<'_>,
) -> Result<(), PermissionError> {
    if exclusions.iter().any(|root| scratch.overlaps(root)) {
        return Err(PermissionError::ProtectedOverlap);
    }
    let anchor = anchors
        .directories
        .iter()
        .find(|anchor| anchor.path == scratch.path)
        .ok_or_else(|| anchor_at(&scratch.path, "scratch anchor not held"))?;
    anchor.verify()?;
    let metadata = anchor
        .directory
        .metadata()
        .map_err(PermissionError::Filesystem)?;
    if scratch.scope != PathScope::Subtree
        || metadata.uid != provenant_os::fs::effective_uid()
        || metadata.mode & 0o077 != 0
    {
        return Err(anchor_at(
            &scratch.path,
            "directory is not private to this user (owner or group/other mode bits)",
        ));
    }
    Ok(())
}

#[cfg(all(test, any(target_os = "macos", feature = "snapshot")))]
pub(super) fn render_profile(
    plan: &PlanPermissions,
    profile: &ProviderProfile,
    reads: &[PathGrant],
    credential_writes: &[PathGrant],
    worktree_grants: Option<&WorktreeWriteGrants>,
    home: &Path,
) -> Result<Vec<u8>, PermissionError> {
    render_profile_with_limit(
        plan,
        profile,
        reads,
        credential_writes,
        worktree_grants,
        home,
        PROFILE_LIMIT,
    )
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
fn render_profile_with_limit(
    plan: &PlanPermissions,
    profile: &ProviderProfile,
    reads: &[PathGrant],
    credential_writes: &[PathGrant],
    worktree_grants: Option<&WorktreeWriteGrants>,
    home: &Path,
    limit: usize,
) -> Result<Vec<u8>, PermissionError> {
    let mut source = String::from("(version 1)\n(allow default)\n");
    let network = plan.policy.as_ref().map_or(plan.network, |policy| {
        policy.optional().network_for(plan.network)
    });
    // Restrict the default before adding typed online endpoint/relay grants.
    if network != NetworkAccess::Open {
        source.push_str("(deny network*)\n");
    }
    // Apple dyld bootstrap rules are shared with the backend probe to keep
    // launch prerequisites and provider profiles aligned.
    source.push_str(DYLD_BOOTSTRAP_RULES);
    source.push_str(RUNTIME_BASELINE_RULES);
    let writer = matches!(plan.mode, AccessMode::WorktreeWrite(_));
    if profile.spec.allow_database_ipc {
        source.push_str("(allow ipc-sysv-shm)\n(allow ipc-sysv-sem)\n");
    }
    for grant in reads {
        push_rule(&mut source, "allow", "file-read*", &grant.path, grant.scope)?;
    }
    let mut omitted_discovery = false;
    for grant in &profile.spec.discovery_reads {
        if super::validation::valid_discovery_read(grant) {
            push_rule(&mut source, "allow", "file-read*", &grant.path, grant.scope)?;
        } else {
            omitted_discovery = true;
        }
    }
    if omitted_discovery {
        use std::io::Write;
        let _ = writeln!(
            std::io::stderr().lock(),
            "warning: invalid discovery paths were omitted"
        );
    }
    if writer {
        // Shell tools may execute any readable program, including a binary
        // produced in the worktree. File grants and the later credential and
        // keychain denials still apply to every descendant.
        source.push_str("(allow process-exec)\n(allow process-fork)\n");
        source.push_str("(allow file-read* (subpath \"/dev/fd\"))\n");
    } else {
        for executable in &profile.spec.process_executables {
            push_rule(
                &mut source,
                "allow",
                "process-exec",
                &executable.path,
                PathScope::Exact,
            )?;
        }
        for root in &profile.spec.tool_exec_roots {
            push_rule(&mut source, "allow", "process-exec", &root.path, root.scope)?;
        }
        if profile.spec.allow_process_fork {
            source.push_str("(allow process-fork)\n");
        }
    }
    if writer || profile.spec.allow_pseudo_tty {
        source.push_str(PSEUDO_TTY_RULES);
    }
    // A file-shaped authentication grant alone never implies keychain IPC.
    // The selected fixture-backed Agy profile names its one required service.
    let mandatory_keychains = mandatory_for_plan(plan, home, worktree_grants)?;
    let keychain = !profile.spec.deny_keychain_access
        && !profile.spec.deny_credential_access
        && selected_authentication(plan, profile).is_some_and(|selected| {
            selected.keychain == Some(KeychainAccess::SecurityServer)
                && !selected.roots.iter().any(|root| {
                    mandatory_keychains
                        .non_exempt()
                        .iter()
                        .any(|boundary| root.overlaps(boundary))
                })
                && selected.roots.iter().any(|root| {
                    root.scope == PathScope::Exact
                        && root.path.ends_with("Library/Keychains/login.keychain-db")
                        && !root.path.starts_with(home.join("Library/Keychains"))
                })
        });
    source.push_str(concat!(
        "(deny mach-lookup (global-name \"com.apple.securityd\") (global-name \"com.apple.securityd.xpc\"))\n",
        "(deny mach-lookup (global-name-prefix \"com.apple.securityd\") (global-name-prefix \"com.apple.security.cloudkeychain\") (global-name-prefix \"com.apple.keychainsharing\"))\n",
    ));
    // TLS root lookup on macOS needs this one service: denying it stops every
    // provider reaching its API. Keychain files keep their own denials.
    if profile.spec.deny_keychain_access || profile.spec.deny_credential_access {
        source.push_str("(deny mach-lookup (global-name-prefix \"com.apple.SecurityServer\"))\n");
        source.push_str("(deny mach-lookup (global-name \"com.apple.SecurityServer\"))\n");
    } else {
        source.push_str("(deny mach-lookup (require-all (global-name-prefix \"com.apple.SecurityServer\") (require-not (global-name \"com.apple.SecurityServer\"))))\n");
    }
    if !keychain {
        source.push_str("(deny process-exec (literal \"/usr/bin/security\"))\n");
    }
    for grant in &profile.spec.state_writes {
        for operation in ["file-read*", "file-write*"] {
            push_rule(&mut source, "allow", operation, &grant.path, grant.scope)?;
        }
    }
    for grant in credential_writes {
        push_rule(
            &mut source,
            "allow",
            "file-write-data",
            &grant.path,
            PathScope::Exact,
        )?;
    }
    for grant in plan.write_roots.iter().chain(&profile.spec.tool_writes) {
        for operation in ["file-read*", "file-write*"] {
            push_rule(&mut source, "allow", operation, &grant.path, grant.scope)?;
        }
    }
    for private in std::iter::once(&plan.scratch).chain(&plan.private_state) {
        for operation in ["file-read*", "file-write*"] {
            push_rule(
                &mut source,
                "allow",
                operation,
                &private.path,
                PathScope::Subtree,
            )?;
        }
    }
    // Package managers link inside projects and stores; the host reads
    // provider state back, so no link may be planted there.
    for grant in profile.spec.state_writes.iter().chain(&plan.private_state) {
        source.push_str(&format!(
            "(deny file-write-create (require-all (vnode-type SYMLINK) (subpath \"{}\")))\n",
            sbpl_path(&grant.path)?
        ));
    }

    // The lane-private object store must not redirect Git writes through links.
    if plan.read_only.is_some() || matches!(plan.mode, AccessMode::WorktreeWrite(_)) {
        source.push_str(&format!(
            "(deny file-write-create (require-all (vnode-type SYMLINK) (subpath \"{}\")))\n",
            sbpl_path(&plan.scratch.path.join("git-objects"))?
        ));
    }
    if let Some(read_only) = &plan.read_only {
        for grant in &read_only.writes {
            for operation in ["file-read*", "file-write*"] {
                push_rule(&mut source, "allow", operation, &grant.path, grant.scope)?;
            }
            // Restrict this exception, preserving separate provider auth grants.
            source.push_str(&format!(
                "(deny file-write* (require-all (subpath \"{}\") (regex #\"{}\")))\n",
                sbpl_path(&grant.path)?,
                crate::plan::read_only::exclusion_regex()
            ));
        }
        for git in &read_only.git_reads {
            push_rule(&mut source, "deny", "file-write*", &git.path, git.scope)?;
        }
    }
    if let AccessMode::WorktreeWrite(authority) = &plan.mode {
        push_worktree_rule(
            &mut source,
            worktree_grants.ok_or(PermissionError::GitWriteAuthorityMissing)?,
        )?;
        for operation in ["file-read*", "file-write*"] {
            push_rule(
                &mut source,
                "allow",
                operation,
                &authority.cargo_cache.path,
                PathScope::Subtree,
            )?;
        }
    }

    // Writers' fixtures bind and connect to Unix-domain sockets in their own
    // scratch; the path is evaluated after the `TMPDIR` alias resolves.
    if matches!(plan.mode, AccessMode::WorktreeWrite(_)) {
        let scratch = sbpl_path(&plan.scratch.path)?;
        for operation in ["network-bind", "network-inbound"] {
            source.push_str(&format!(
                "(allow {operation} (local unix-socket (subpath \"{scratch}\")))\n"
            ));
        }
        source.push_str(&format!(
            "(allow network-outbound (remote unix-socket (subpath \"{scratch}\")))\n"
        ));
    }
    if let Some(socket) = &profile.spec.fabric_socket {
        push_rule(
            &mut source,
            "allow",
            "file-read*",
            &socket.path,
            PathScope::Exact,
        )?;
        source.push_str(&format!(
            "(allow network-outbound (remote unix-socket (path-literal \"{}\")))\n",
            sbpl_path(&socket.path)?
        ));
    }
    for endpoint in &profile.spec.provider_api {
        if super::validation::validate_endpoint(endpoint).is_ok() {
            push_network_endpoint(&mut source, endpoint)?;
        } else {
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning: invalid provider network endpoint was omitted"
            );
        }
    }
    if profile.spec.loopback_server {
        source.push_str(concat!(
            "(allow network-bind (local ip \"localhost:*\"))\n",
            "(allow network-inbound (local ip \"localhost:*\"))\n",
            "(allow network-outbound (remote ip \"localhost:*\"))\n",
        ));
    }
    if matches!(network, NetworkAccess::Local | NetworkAccess::Open) {
        source.push_str(LOCAL_SERVICE_RULES);
    }
    if network == NetworkAccess::Open {
        source.push_str("(allow network-outbound (remote ip \"*:*\"))\n");
    }
    if profile.spec.deny_network {
        source.push_str("(deny network*)\n");
    }

    push_boundaries(
        &mut source,
        plan,
        profile,
        reads,
        credential_writes,
        worktree_grants,
        home,
        network,
    )?;
    report_capacity_warning(capacity_warning(
        "domains.run.profile_limit_bytes",
        source.len(),
        limit,
        provenant_records::diagnostic::SettingSource::ShippedDefault,
    ));
    Ok(source.into_bytes())
}

/// Resolve the shared baseline even for standalone renderer fixtures. Real
/// launches carry the complete host-derived inventory in `plan.policy`.
#[cfg(any(target_os = "macos", feature = "snapshot"))]
fn mandatory_for_plan(
    plan: &PlanPermissions,
    home: &Path,
    worktree: Option<&WorktreeWriteGrants>,
) -> Result<MandatoryRoots, PermissionError> {
    if let Some(policy) = &plan.policy {
        return Ok(policy.mandatory().clone());
    }
    let state = home.join(".local/share/provenant/v1");
    let missing_source = home.join(".provenant-unbound-source");
    let source = worktree
        .map(|grants| grants.worktree.path.as_path())
        .or_else(|| plan.read_only.as_ref().map(|grants| grants.root.as_path()))
        .or_else(|| plan.read_roots.first().map(|root| root.path.as_path()))
        .unwrap_or(&missing_source);
    MandatoryRoots::new(home, &state, source)
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
fn selected_authentication<'a>(
    plan: &PlanPermissions,
    profile: &'a ProviderProfile,
) -> Option<&'a AuthenticationProfile> {
    plan.selected_authentication.as_ref().and_then(|mechanism| {
        profile
            .spec
            .authentication
            .iter()
            .find(|auth| &auth.mechanism == mechanism)
    })
}

/// Encode a denial with only typed exceptions removed from its path predicate.
/// An ordinary later allow cannot reopen a sibling beneath the boundary.
#[cfg(any(target_os = "macos", feature = "snapshot"))]
fn push_denial(
    source: &mut String,
    operation: &str,
    root: &PathGrant,
    exceptions: &[PathGrant],
) -> Result<(), PermissionError> {
    let exceptions: Vec<_> = exceptions
        .iter()
        .filter(|grant| {
            grant.path.starts_with(&root.path)
                && (root.scope == PathScope::Subtree || grant.path == root.path)
        })
        .collect();
    if exceptions.is_empty() {
        return push_rule(source, "deny", operation, &root.path, root.scope);
    }
    let filter = |scope| match scope {
        PathScope::Exact => "literal",
        PathScope::Subtree => "subpath",
    };
    source.push_str(&format!(
        "(deny {operation} (require-all ({} \"{}\")",
        filter(root.scope),
        sbpl_path(&root.path)?
    ));
    for exception in exceptions {
        source.push_str(&format!(
            " (require-not ({} \"{}\"))",
            filter(exception.scope),
            sbpl_path(&exception.path)?
        ));
    }
    source.push_str("))\n");
    Ok(())
}

/// Unix socket IPC need not issue a file-read check. Keep host services
/// private under the same state and credential pathname boundary.
#[cfg(any(target_os = "macos", feature = "snapshot"))]
fn push_socket_denial(
    source: &mut String,
    root: &PathGrant,
    exceptions: &[PathGrant],
) -> Result<(), PermissionError> {
    let filter = |scope| match scope {
        PathScope::Exact => "path-literal",
        PathScope::Subtree => "subpath",
    };
    source.push_str(&format!(
        "(deny network-outbound (require-all (remote unix-socket ({} \"{}\"))",
        filter(root.scope),
        sbpl_path(&root.path)?
    ));
    for exception in exceptions
        .iter()
        .filter(|grant| grant.path.starts_with(&root.path))
    {
        source.push_str(&format!(
            " (require-not (remote unix-socket ({} \"{}\")))",
            filter(exception.scope),
            sbpl_path(&exception.path)?
        ));
    }
    source.push_str("))\n");
    Ok(())
}

/// Rename and unlink are checked on the affected vnode. Protect every ancestor
/// without denying ordinary creation of unrelated siblings in those parents.
#[cfg(any(target_os = "macos", feature = "snapshot"))]
fn push_ancestor_guards(
    source: &mut String,
    root: &PathGrant,
    ancestors: &mut std::collections::BTreeSet<std::path::PathBuf>,
) -> Result<(), PermissionError> {
    for ancestor in root.path.ancestors() {
        if !ancestors.insert(ancestor.to_owned()) {
            continue;
        }
        push_rule(
            source,
            "deny",
            "file-write-unlink",
            ancestor,
            PathScope::Exact,
        )?;
    }
    Ok(())
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
#[allow(clippy::too_many_arguments)]
fn push_boundaries(
    source: &mut String,
    plan: &PlanPermissions,
    profile: &ProviderProfile,
    reads: &[PathGrant],
    credential_writes: &[PathGrant],
    worktree: Option<&WorktreeWriteGrants>,
    home: &Path,
    network: NetworkAccess,
) -> Result<(), PermissionError> {
    let mut ancestors = std::collections::BTreeSet::new();
    let mut mandatory = mandatory_for_plan(plan, home, worktree)?;
    // Every declared authentication family is private, selected or otherwise.
    for root in profile
        .spec
        .authentication
        .iter()
        .flat_map(|auth| &auth.roots)
    {
        mandatory.add_credentials(root.clone())?;
    }
    let selected = selected_authentication(plan, profile);
    let gh = network == NetworkAccess::Open
        && !profile.spec.deny_network
        && !profile.spec.deny_credential_access;
    let mut credential_reads = Vec::new();
    if !profile.spec.deny_credential_access {
        if let Some(selected) = selected {
            credential_reads.extend(
                selected
                    .roots
                    .iter()
                    .filter(|root| !root.path.starts_with(home.join("Library/Keychains")))
                    .cloned(),
            );
        }
        if gh {
            credential_reads.extend(mandatory.github().iter().cloned());
        }
        credential_reads.extend(mandatory.credential_alias_reads(&credential_reads));
    }
    // A package grant passes the existing standalone validator; it never opens
    // the credential/config/session siblings in the enclosing Codex home.
    let packages: Vec<_> = reads
        .iter()
        .chain(&profile.spec.tool_exec_roots)
        .filter(|_| !profile.spec.deny_credential_access)
        .filter(|grant| {
            is_codex_standalone_package(&grant.path) && validate_read_exec_grant(grant).is_ok()
        })
        .cloned()
        .collect();
    credential_reads.extend(packages.iter().cloned());
    if gh {
        for root in mandatory.github() {
            push_rule(source, "allow", "file-read*", &root.path, root.scope)?;
        }
    }
    let empty = Vec::new();
    for root in mandatory.credentials() {
        // This separate exact store stays hidden even from Claude itself.
        let exceptions = if root.path == home.join(".claude.json") {
            &empty
        } else {
            &credential_reads
        };
        push_denial(source, "file-read*", root, exceptions)?;
        push_denial(source, "file-clone", root, exceptions)?;
        push_denial(source, "process-exec", root, &packages)?;
        push_denial(source, "file-map-executable", root, &packages)?;
        if profile.spec.deny_credential_access
            || credential_writes.is_empty()
            || root.path == home.join(".claude.json")
        {
            push_denial(source, "file-write*", root, &[])?;
        } else {
            push_denial(source, "file-write*", root, credential_writes)?;
            // A refresh can modify bytes only. Creating, replacing, renaming,
            // changing ownership/mode or relinking the credential stays denied.
            push_rule(
                source,
                "deny",
                "file-write-create file-write-unlink file-write-mode file-write-owner file-write-flags file-write-acl file-write-xattr file-write-setugid file-write-mount file-write-umount",
                &root.path,
                root.scope,
            )?;
        }
        push_socket_denial(source, root, &[])?;
        push_ancestor_guards(source, root, &mut ancestors)?;
    }
    // Real-home keychains, preservation archives and Claude's MCP-secret file
    // admit no authentication exception, including through canonical aliases.
    for root in mandatory.non_exempt() {
        for operation in [
            "file-read*",
            "file-clone",
            "file-write*",
            "process-exec",
            "file-map-executable",
        ] {
            push_rule(source, "deny", operation, &root.path, root.scope)?;
        }
        push_socket_denial(source, root, &[])?;
    }
    for root in mandatory.github() {
        push_rule(source, "deny", "file-write*", &root.path, root.scope)?;
    }
    let private: Vec<_> = std::iter::once(&plan.scratch)
        .chain(&plan.private_state)
        .cloned()
        .collect();
    // Authentication and gh retain their typed read exceptions when the host
    // stores an identity below S. No credential root may open S or its parent.
    for grant in &credential_reads {
        if std::iter::once(mandatory.state())
            .chain(mandatory.state_aliases())
            .any(|root| root.path.starts_with(&grant.path))
        {
            return Err(PermissionError::ProtectedOverlap);
        }
    }
    let refresh: Vec<_> = credential_writes
        .iter()
        .filter(|_| !profile.spec.deny_credential_access)
        .filter(|grant| {
            !mandatory
                .non_exempt()
                .iter()
                .any(|root| grant.overlaps(root))
        })
        .cloned()
        .collect();
    let mut state_writes = private.clone();
    state_writes.extend(refresh);
    let mut state_reads = private.clone();
    state_reads.extend(credential_reads.iter().cloned());
    state_reads.extend(mandatory.state_reads().iter().cloned());
    if let Some(socket) = &profile.spec.fabric_socket {
        state_reads.push(socket.clone());
    }
    for root in std::iter::once(mandatory.state()).chain(mandatory.state_aliases()) {
        push_denial(source, "file-read*", root, &state_reads)?;
        push_denial(source, "file-clone", root, &state_reads)?;
        let mut state_exec = private.clone();
        state_exec.extend(mandatory.state_exec().iter().cloned());
        state_exec.extend(packages.iter().cloned());
        push_denial(source, "process-exec", root, &state_exec)?;
        push_denial(source, "file-map-executable", root, &state_exec)?;
        push_denial(source, "file-write*", root, &state_writes)?;
        push_denial(
            source,
            "file-write-create file-write-unlink file-write-mode file-write-owner file-write-flags file-write-acl file-write-xattr file-write-setugid file-write-mount file-write-umount",
            root,
            &private,
        )?;
        let mut sockets = private.clone();
        sockets.extend(profile.spec.fabric_socket.iter().cloned());
        push_socket_denial(source, root, &sockets)?;
        push_ancestor_guards(source, root, &mut ancestors)?;
    }
    // Private roots remain anchored even when outside the selected state root.
    for root in &private {
        push_ancestor_guards(source, root, &mut ancestors)?;
    }
    let readonly = matches!(plan.mode, AccessMode::ReadOnly);
    if readonly {
        let exceptions = plan
            .read_only
            .as_ref()
            .map_or(&[][..], |grants| grants.writes.as_slice());
        let source_root = plan.read_only.as_ref().map(|grants| PathGrant {
            path: grants.root.clone(),
            scope: PathScope::Subtree,
        });
        for root in std::iter::once(mandatory.source())
            .chain(mandatory.source_aliases())
            .chain(source_root.as_ref())
        {
            push_denial(source, "file-write*", root, exceptions)?;
            push_ancestor_guards(source, root, &mut ancestors)?;
        }
    }
    let writer_exception: Vec<_> = worktree
        .map(|grants| grants.worktree.clone())
        .into_iter()
        .collect();
    for root in mandatory.protected().iter().chain(&plan.protected_roots) {
        let exceptions = if mandatory.protected().contains(root) {
            &writer_exception[..]
        } else {
            &[]
        };
        push_denial(source, "file-write*", root, exceptions)?;
        push_ancestor_guards(source, root, &mut ancestors)?;
    }
    let marker = PathGrant {
        path: mandatory.source().path.join(".git"),
        scope: PathScope::Subtree,
    };
    let writer_marker = worktree.map(|grants| PathGrant {
        path: grants.worktree.path.join(".git"),
        scope: PathScope::Subtree,
    });
    let host_git = worktree
        .into_iter()
        .flat_map(|grants| [&grants.private_git_dir, &grants.common_git_dir]);
    let read_git = plan.read_only.iter().flat_map(|grants| &grants.git_reads);
    for root in mandatory
        .git()
        .iter()
        .chain(host_git)
        .chain(read_git)
        .chain([&marker])
        .chain(writer_marker.as_ref())
    {
        push_rule(source, "deny", "file-write*", &root.path, root.scope)?;
        push_ancestor_guards(source, root, &mut ancestors)?;
    }
    for root in &plan.secret_roots {
        for operation in [
            "file-read*",
            "file-clone",
            "file-write*",
            "process-exec",
            "file-map-executable",
        ] {
            push_rule(source, "deny", operation, &root.path, root.scope)?;
        }
        push_socket_denial(source, root, &[])?;
        push_ancestor_guards(source, root, &mut ancestors)?;
    }
    // These immutable selected inputs include staged user skills, independent
    // of where the host placed their credential/config parents.
    for root in mandatory.state_reads() {
        push_rule(source, "deny", "file-write*", &root.path, root.scope)?;
        push_ancestor_guards(source, root, &mut ancestors)?;
    }
    // Host restrictions are final and admit no exceptions, including refresh,
    // prompt, package and GitHub CLI grants.
    if let Some(policy) = &plan.policy {
        for root in &policy.optional().deny_read {
            for operation in [
                "file-read*",
                "file-clone",
                "process-exec",
                "file-map-executable",
            ] {
                push_rule(source, "deny", operation, &root.path, root.scope)?;
            }
            push_ancestor_guards(source, root, &mut ancestors)?;
        }
        for root in &policy.optional().deny_write {
            push_rule(source, "deny", "file-write*", &root.path, root.scope)?;
            push_ancestor_guards(source, root, &mut ancestors)?;
        }
    }
    // Ordinary read grants cannot expose another terminal's input stream.
    source.push_str(
        r#"(deny file-read* file-write* file-ioctl
  (require-all (regex #"^/dev/ttys[0-9]+$")
    (require-not (extension "com.apple.sandbox.pty"))))
"#,
    );
    Ok(())
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
pub(super) fn push_network_endpoint(
    source: &mut String,
    endpoint: &NetworkEndpoint,
) -> Result<(), PermissionError> {
    let port = u16::try_from(endpoint.port).map_err(|_| PermissionError::InvalidFixture)?;
    source.push_str(&format!(
        "(allow network-outbound (remote tcp \"{}:{port}\"))\n",
        endpoint.address
    ));
    Ok(())
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
pub(super) fn push_rule(
    source: &mut String,
    effect: &str,
    operation: &str,
    path: &Path,
    scope: PathScope,
) -> Result<(), PermissionError> {
    let path = sbpl_path(path)?;
    let filter = match scope {
        PathScope::Exact => "literal",
        PathScope::Subtree => "subpath",
    };
    source.push_str(&format!("({effect} {operation} ({filter} \"{path}\"))\n"));
    Ok(())
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
pub(super) fn sbpl_path(path: &Path) -> Result<String, PermissionError> {
    let text = path.to_str().ok_or(PermissionError::NonUtf8Path)?;
    if !profile_path_representable(path) {
        return Err(PermissionError::InvalidPath(path.to_owned()));
    }
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            value if value.is_control() => {
                return Err(PermissionError::InvalidPath(path.to_owned()));
            }
            value => escaped.push(value),
        }
    }
    Ok(escaped)
}
