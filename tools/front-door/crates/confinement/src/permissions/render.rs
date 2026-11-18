//! SBPL compilation and rendering.

#[cfg(target_os = "macos")]
use super::validation::filtered_native_controls_with_policy;
use super::*;
#[cfg(target_os = "macos")]
use crate::cache::verify_cache_metadata;
use provenant_records::policy::{Effect, Facts, Policy};
#[cfg(target_os = "macos")]
use sha2::{Digest, Sha256};
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

#[cfg(any(target_os = "macos", feature = "snapshot"))]
pub(super) const DYLD_BOOTSTRAP_RULES: &str = r#"
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
(deny process-info-pidinfo)
(allow process-info-pidinfo (target same-sandbox))
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
#[cfg(all(target_os = "macos", any(test, feature = "snapshot")))]
pub(super) fn compile_sbpl(
    plan: &PlanPermissions,
    profile: &ProviderProfile,
    anchors: &Anchors<'_>,
) -> Result<ConfinedLaunch, PermissionError> {
    compile_sbpl_impl(
        plan,
        profile,
        anchors,
        ProfileLimits::default(),
        &Policy::default(),
    )
}

#[cfg(target_os = "macos")]
pub(super) fn compile_sbpl_with_diagnostic(
    plan: &PlanPermissions,
    profile: &ProviderProfile,
    anchors: &Anchors<'_>,
    limits: ProfileLimits,
    policy: &Policy,
) -> Result<ConfinedLaunch, PermissionError> {
    // Enforcing launches require the complete host contract. The pure snapshot
    // seam may still render standalone fixtures with the shared baseline.
    if plan.policy.is_none() {
        let error = PermissionError::PermissionNotRecorded;
        return Err(Policy::default().boundary_error(
            "RENDER_COMPILE_SBPL_AUTHORITY_3",
            Facts::violation("the enforcing launch is missing its complete permission snapshot"),
            error,
        ));
    }
    compile_sbpl_impl(plan, profile, anchors, limits, policy)
}

#[cfg(target_os = "macos")]
pub(super) fn compile_sbpl_impl(
    plan: &PlanPermissions,
    profile: &ProviderProfile,
    anchors: &Anchors<'_>,
    limits: ProfileLimits,
    policy: &Policy,
) -> Result<ConfinedLaunch, PermissionError> {
    compile_sbpl_inner(plan, profile, anchors, limits, policy).map_err(compile_boundary_error)
}

#[cfg(target_os = "macos")]
fn compile_boundary_error(error: PermissionError) -> PermissionError {
    use provenant_records::diagnostic::BoundaryClass;
    let code = match error.boundary_class() {
        BoundaryClass::Credentials => Some("RENDER_COMPILE_SBPL_CREDENTIALS"),
        BoundaryClass::LaneGit => Some("RENDER_COMPILE_SBPL_GIT_ISOLATION"),
        BoundaryClass::Authority => Some(match &error {
            PermissionError::NestedConfinementUnsupported => "RENDER_COMPILE_SBPL_AUTHORITY",
            PermissionError::PermissionNotRecorded => "RENDER_COMPILE_SBPL_AUTHORITY_3",
            _ => "RENDER_COMPILE_SBPL_AUTHORITY_2",
        }),
        BoundaryClass::Integrity | BoundaryClass::None => None,
    };
    match code {
        Some(code) => {
            let detail = error.to_string();
            Policy::default().boundary_error(code, Facts::violation(detail), error)
        }
        None => error,
    }
}

#[cfg(target_os = "macos")]
fn compile_authority_error(error: PermissionError) -> PermissionError {
    let detail = error.to_string();
    Policy::default().boundary_error(
        "RENDER_COMPILE_SBPL_AUTHORITY_2",
        Facts::violation(detail),
        error,
    )
}

#[cfg(target_os = "macos")]
fn compile_sbpl_inner(
    plan: &PlanPermissions,
    profile: &ProviderProfile,
    anchors: &Anchors<'_>,
    limits: ProfileLimits,
    policy: &Policy,
) -> Result<ConfinedLaunch, PermissionError> {
    anchors.verify_all().map_err(compile_authority_error)?;
    if let Some(policy) = &plan.policy {
        policy
            .mandatory()
            .revalidate_paths()
            .map_err(compile_authority_error)?;
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
    let mandatory = mandatory_for_plan(plan, anchors.home, None)?;
    if let Some(warning) = check_capacity_policy(
        policy,
        "GRANT_COUNT",
        GRANT_LIMIT_SETTING_KEY,
        plan_count,
        limits.grant_limit,
        limits.grant_origin,
        PermissionError::GrantLimitExceeded {
            family: "plan root grant count",
            limit: limits.grant_limit,
        },
    )? {
        report_policy_message(warning);
    }
    if let Some(warning) = check_capacity_policy(
        policy,
        "PROFILE_GRANT_COUNT",
        GRANT_LIMIT_SETTING_KEY,
        profile_count,
        limits.grant_limit,
        limits.grant_origin,
        PermissionError::GrantLimitExceeded {
            family: "provider profile grant count",
            limit: limits.grant_limit,
        },
    )? {
        report_policy_message(warning);
    }
    // Admission uses inventory aliases too, so runtime authority cannot reopen
    // an external directory target discovered underneath a store.
    let mut credentials = mandatory.credential_roots().clone();
    for auth in &profile.spec.authentication {
        for grant in &auth.roots {
            credentials.add(
                &grant.path,
                match grant.scope {
                    PathScope::Exact => provenant_core::CredentialScope::Exact,
                    PathScope::Subtree => provenant_core::CredentialScope::Subtree,
                },
                format!("provider authentication: {}", auth.mechanism),
                provenant_core::CredentialKind::Store,
            );
        }
    }
    validate_profile_spec_with_home(&profile.spec, anchors.home, &credentials, usize::MAX)?;
    for grant in &plan.read_roots {
        validate_read_exec_grant(grant, &credentials)
            .map_err(|error| error.with_credential_path(&grant.path, Some(anchors.home)))?;
    }
    // Every lexical credential check runs before any filesystem anchor check,
    // so a refused root names the declared store rather than a missing anchor.
    for grant in plan.protected_roots.iter().chain(&plan.write_roots) {
        validate_noncredential_grant(grant, &credentials)
            .map_err(|error| error.with_credential_path(&grant.path, Some(anchors.home)))?;
    }
    let worktree_grants = match &plan.mode {
        AccessMode::ReadOnly => None,
        AccessMode::WorktreeWrite(authority) => {
            Some(verify_worktree_authority(authority, anchors, &credentials)?)
        }
    };
    if let Some(read_only) = &plan.read_only {
        if !matches!(plan.mode, AccessMode::ReadOnly) {
            return Err(PermissionError::InvalidReadOnlyPolicy);
        }
        read_only.revalidate().map_err(compile_authority_error)?;
        for grant in &read_only.writes {
            validate_noncredential_grant(grant, &credentials)
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
            anchors
                .verify_grant_held(grant)
                .map_err(compile_authority_error)?;
        }
    }
    for grant in &plan.write_roots {
        mandatory.validate_extra_write(grant)?;
        let canonical = resolve_boundary_path(&grant.path)?;
        mandatory.validate_extra_write(&PathGrant {
            path: canonical,
            scope: grant.scope,
        })?;
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
    for grant in read_grants {
        anchors
            .verify_read_held(grant)
            .map_err(compile_authority_error)?;
    }
    for grant in discovery() {
        verify_discovery_grant(anchors, grant)?;
    }
    for executable in &profile.spec.process_executables {
        anchors
            .verify_executable(executable)
            .map_err(compile_authority_error)?;
    }
    for grant in credential_writes {
        anchors
            .verify_single_link_grant(grant)
            .map_err(compile_authority_error)?;
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
    validate_noncredential_grant(&plan.scratch, &credentials)
        .map_err(|error| error.with_credential_path(&plan.scratch.path, Some(anchors.home)))?;
    anchors
        .verify_grant_held(&plan.scratch)
        .map_err(compile_authority_error)?;
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
        validate_noncredential_grant(state, &credentials)
            .map_err(|error| error.with_credential_path(&state.path, Some(anchors.home)))?;
        if state.overlaps(&plan.scratch) {
            policy.warn_or(
                "PLAN_STATE_SCRATCH_OVERLAP",
                Facts::violation("private state overlaps scratch"),
                |_| PermissionError::ProtectedOverlap,
            )?;
        }
        verify_private_scratch(state, &scratch_exclusions, anchors)?;
    }
    verify_shared_writes(plan, &shared_writes, anchors)?;

    for secret in &plan.secret_roots {
        secret.validate()?;
        anchors
            .verify_grant_held(secret)
            .map_err(compile_authority_error)?;
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

    let mut profile_bytes = render_with_policy(&grants, policy)?.into_bytes();
    if let Some(warning) = check_capacity_policy(
        policy,
        "PROFILE_BYTES",
        "domains.run.profile_limit_bytes",
        profile_bytes.len(),
        limits.profile_limit_bytes,
        limits.profile_origin,
        PermissionError::ProfileTooLarge {
            bytes: profile_bytes.len(),
            limit: limits.profile_limit_bytes,
        },
    )? {
        report_policy_message(warning);
    }
    let profile_digest = Sha256::digest(&profile_bytes).into();
    // SBPL grants pathnames, so rebind them to their held identities after
    // rendering, immediately before returning the launch profile.
    if let AccessMode::WorktreeWrite(authority) = &plan.mode {
        verify_worktree_authority(authority, anchors, &credentials)?;
    }
    verify_private_scratch(&plan.scratch, &scratch_exclusions, anchors)?;
    if let Some(state) = &plan.private_state {
        verify_private_scratch(state, &scratch_exclusions, anchors)?;
    }
    verify_state_writes(plan, profile, anchors)?;
    if let Some(read_only) = &plan.read_only {
        read_only.revalidate().map_err(compile_authority_error)?;
        for grant in &read_only.writes {
            anchors
                .verify_grant_held(grant)
                .map_err(compile_authority_error)?;
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
    anchors.verify_all().map_err(compile_authority_error)?;
    for grant in discovery() {
        verify_discovery_grant(anchors, grant)?;
    }
    if let Some(policy) = &plan.policy {
        policy
            .mandatory()
            .revalidate_paths()
            .map_err(compile_authority_error)?;
    }
    // Avoid retaining a second mutable representation of the profile.
    profile_bytes.shrink_to_fit();
    let native_controls =
        filtered_native_controls_with_policy(&profile.spec.native_controls, policy);
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
        let confinement = profile.spec.confinement.resolve_for(plan);
        let keychain_root = home.join("Library/Keychains");
        if let Some(mechanism) = &plan.selected_authentication {
            let selected = profile
                .spec
                .authentication
                .iter()
                .find(|item| &item.mechanism == mechanism)
                .ok_or(PermissionError::PermissionNotRecorded)?;
            if confinement.credentials_permitted()
                && (confinement.keychain_permitted() || selected.keychain.is_none())
            {
                reads.extend(
                    selected
                        .roots
                        .iter()
                        .filter(|root| {
                            confinement.keychain_permitted()
                                || !root.path.starts_with(&keychain_root)
                        })
                        .cloned(),
                );
                credential_writes.extend(
                    selected
                        .in_place_writes
                        .iter()
                        .filter(|root| {
                            confinement.keychain_permitted()
                                || !root.path.starts_with(&keychain_root)
                        })
                        .cloned(),
                );
            }
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
    render_with_policy(grants, &Policy::default())
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
pub(super) fn render_with_policy(
    grants: &GrantSet<'_>,
    policy: &Policy,
) -> Result<String, PermissionError> {
    let source = String::from_utf8(render_profile_with_policy(
        grants.plan,
        grants.profile,
        &grants.reads,
        &grants.credential_writes,
        grants.worktree.as_ref(),
        grants.home,
        policy,
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
        validate_noncredential_grant(grant, &plan.credential_roots)
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
                .any(|root| root.overlaps(grant))
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
    render_profile_with_policy(
        plan,
        profile,
        reads,
        credential_writes,
        worktree_grants,
        home,
        &Policy::default(),
    )
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
fn render_profile_with_policy(
    plan: &PlanPermissions,
    profile: &ProviderProfile,
    reads: &[PathGrant],
    credential_writes: &[PathGrant],
    worktree_grants: Option<&WorktreeWriteGrants>,
    home: &Path,
    policy: &Policy,
) -> Result<Vec<u8>, PermissionError> {
    let mut source = String::from("(version 1)\n(allow default)\n");
    let confinement = profile.spec.confinement.resolve_for(plan);
    let network = confinement.network();
    // Restrict the default before adding typed online endpoint/relay grants.
    if network != NetworkPolicy::Open {
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
    let mut omitted_discovery = 0;
    for grant in &profile.spec.discovery_reads {
        if super::validation::valid_discovery_read(grant) {
            push_rule(&mut source, "allow", "file-read*", &grant.path, grant.scope)?;
        } else {
            omitted_discovery += 1;
        }
    }
    if omitted_discovery > 0 {
        report_policy_warning(
            policy,
            "RENDER_RENDER_PROFILE_WITH_LIMIT_NONE_2",
            format!("omitted {omitted_discovery} invalid discovery-only reads"),
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
    // Keychain clients need both their executable and securityd IPC. A file
    // grant alone does not select the native keychain mechanism.
    let mandatory_keychains = mandatory_for_plan(plan, home, worktree_grants)?;
    let keychain = selected_keychain(plan, profile, home, &mandatory_keychains).is_some();
    if !keychain {
        source.push_str(concat!(
            "(deny mach-lookup (global-name \"com.apple.securityd\") (global-name \"com.apple.securityd.xpc\"))\n",
            "(deny mach-lookup (global-name-prefix \"com.apple.securityd\") (global-name-prefix \"com.apple.security.cloudkeychain\") (global-name-prefix \"com.apple.keychainsharing\"))\n",
        ));
    }
    // TLS root lookup on macOS needs this one service: denying it stops every
    // provider reaching its API. Keychain files keep their own denials.
    if !confinement.keychain_permitted() {
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
    let mut omitted_endpoints = 0;
    for endpoint in &profile.spec.provider_api {
        if super::validation::validate_endpoint(endpoint).is_ok() {
            push_network_endpoint(&mut source, endpoint)?;
        } else {
            omitted_endpoints += 1;
        }
    }
    if omitted_endpoints > 0 {
        report_policy_warning(
            policy,
            "RENDER_RENDER_PROFILE_WITH_LIMIT_NONE",
            format!("omitted {omitted_endpoints} malformed provider network endpoints"),
        );
    }
    if profile.spec.loopback_server {
        source.push_str(concat!(
            "(allow network-bind (local ip \"localhost:*\"))\n",
            "(allow network-inbound (local ip \"localhost:*\"))\n",
            "(allow network-outbound (remote ip \"localhost:*\"))\n",
        ));
    }
    if matches!(network, NetworkPolicy::Local | NetworkPolicy::Open) {
        source.push_str(LOCAL_SERVICE_RULES);
    }
    if network == NetworkPolicy::Open {
        source.push_str("(allow network-outbound (remote ip \"*:*\"))\n");
    }
    if network == NetworkPolicy::Offline {
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
    Ok(source.into_bytes())
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
#[allow(clippy::panic)]
fn report_policy_warning(policy: &Policy, code: &str, detail: String) {
    match policy.check(code, Facts::violation(detail)) {
        Effect::Proceed => {}
        Effect::Warn(message) => report_policy_message(message),
        Effect::Refuse(_) | Effect::Stop(_) => {
            panic!("optional-filter rules are warn-or-off decisions")
        }
    }
}

#[cfg(target_os = "macos")]
fn verify_discovery_grant(anchors: &Anchors<'_>, grant: &PathGrant) -> Result<(), PermissionError> {
    anchors.verify_discovery_grant(grant).map_err(|error| {
        let detail = error.to_string();
        Policy::default().boundary_error(
            "VALIDATION_VALIDATE_PROFILE_SPEC_IMPL_CREDENTIALS_2",
            Facts::violation(detail),
            error,
        )
    })
}

/// Resolve the shared baseline even for standalone renderer fixtures. Real
/// launches carry the complete host-derived inventory in `plan.policy`.
#[cfg(any(target_os = "macos", feature = "snapshot"))]
fn mandatory_for_plan(
    plan: &PlanPermissions,
    home: &Path,
    worktree: Option<&WorktreeWriteGrants>,
) -> Result<MandatoryRoots, PermissionError> {
    let mut mandatory = if let Some(policy) = &plan.policy {
        policy.mandatory().clone()
    } else {
        let state = home.join(provenant_core::DEFAULT_STATE_SUFFIX);
        let missing_source = home.join(".provenant-unbound-source");
        let source = worktree
            .map(|grants| grants.worktree.path.as_path())
            .or_else(|| plan.read_only.as_ref().map(|grants| grants.root.as_path()))
            .or_else(|| plan.read_roots.first().map(|root| root.path.as_path()))
            .unwrap_or(&missing_source);
        MandatoryRoots::new(home, &state, source)?
    };
    for root in plan.credential_roots.entries() {
        mandatory.add_declared_credential(
            PathGrant {
                path: root.path.clone(),
                scope: match root.scope {
                    provenant_core::CredentialScope::Exact => PathScope::Exact,
                    provenant_core::CredentialScope::Subtree => PathScope::Subtree,
                },
            },
            root.source.clone(),
            root.kind.clone(),
        )?;
    }
    let mut lane_roots = mandatory.credential_roots().lane_roots().to_vec();
    for root in plan.credential_roots.lane_roots() {
        if !lane_roots.contains(root) {
            lane_roots.push(root.clone());
        }
    }
    mandatory.set_credential_roots(
        mandatory
            .credential_roots()
            .clone()
            .with_lane_roots(lane_roots),
    )?;

    Ok(mandatory)
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
fn selected_keychain<'a>(
    plan: &PlanPermissions,
    profile: &'a ProviderProfile,
    home: &Path,
    mandatory: &MandatoryRoots,
) -> Option<&'a PathGrant> {
    if !profile
        .spec
        .confinement
        .resolve_for(plan)
        .keychain_permitted()
    {
        return None;
    }
    let auth = selected_authentication(plan, profile)?;
    let [root] = auth.roots.as_slice() else {
        return None;
    };
    (auth.keychain == Some(KeychainAccess::SecurityServer)
        && root.scope == PathScope::Exact
        && root.path.ends_with("Library/Keychains/login.keychain-db")
        && !mandatory.non_exempt().iter().any(|boundary| {
            root.overlaps(boundary)
                && !(boundary.path == home.join("Library/Keychains")
                    && root.path == home.join("Library/Keychains/login.keychain-db"))
        }))
    .then_some(root)
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
    root: impl Into<DenialRoot>,
    exceptions: &[PathGrant],
) -> Result<(), PermissionError> {
    let root = root.into();
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
    // An exception that covers the whole root leaves nothing to deny.
    if exceptions.iter().any(|grant| {
        grant.path == root.path
            && (grant.scope == PathScope::Subtree || root.scope == PathScope::Exact)
    }) {
        return Ok(());
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

/// Admitted files still need metadata on their ancestors during exec and
/// dyld startup. Exempt only those directory vnodes from the broad read deny,
/// then deny their contents and extended attributes separately. No late allow
/// may reopen a credential, and sibling paths keep the original denial.
#[cfg(any(target_os = "macos", feature = "snapshot"))]
fn push_read_denial(
    source: &mut String,
    root: &DenialRoot,
    exceptions: &[PathGrant],
) -> Result<(), PermissionError> {
    let mut ancestors = std::collections::BTreeSet::new();
    if root.scope == PathScope::Subtree {
        for grant in exceptions {
            for ancestor in grant.path.ancestors().skip(1) {
                if ancestor.starts_with(&root.path)
                    && !exceptions.iter().any(|read| {
                        read.path == ancestor
                            || (read.scope == PathScope::Subtree
                                && ancestor.starts_with(&read.path))
                    })
                {
                    ancestors.insert(ancestor.to_owned());
                }
            }
        }
    }
    let mut metadata = exceptions.to_vec();
    metadata.extend(ancestors.iter().map(|path| PathGrant {
        path: path.clone(),
        scope: PathScope::Exact,
    }));
    push_denial(source, "file-read*", root, &metadata)?;
    for ancestor in ancestors {
        push_rule(
            source,
            "deny",
            "file-read-data file-read-xattr",
            &ancestor,
            PathScope::Exact,
        )?;
    }
    Ok(())
}

/// Unix socket IPC need not issue a file-read check. Keep host services
/// private under the same state and credential pathname boundary.
#[cfg(any(target_os = "macos", feature = "snapshot"))]
fn push_socket_denial(
    source: &mut String,
    root: impl Into<DenialRoot>,
    exceptions: &[PathGrant],
) -> Result<(), PermissionError> {
    let root = root.into();
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
    root: impl Into<DenialRoot>,
    ancestors: &mut std::collections::BTreeSet<std::path::PathBuf>,
) -> Result<(), PermissionError> {
    let root = root.into();
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
    network: NetworkPolicy,
) -> Result<(), PermissionError> {
    let mut ancestors = std::collections::BTreeSet::new();
    let mut mandatory = mandatory_for_plan(plan, home, worktree)?;
    let confinement = profile.spec.confinement.resolve_for(plan);

    // Every declared authentication family is private, selected or otherwise.
    for root in profile
        .spec
        .authentication
        .iter()
        .flat_map(|auth| &auth.roots)
    {
        mandatory.add_credentials(root.clone())?;
    }
    let runtime: Vec<_> = plan
        .read_roots
        .iter()
        .chain(&profile.spec.process_executables)
        .chain(&profile.spec.runtime_reads)
        .chain(&profile.spec.tool_exec_roots)
        .filter(|grant| validate_read_exec_grant(grant, mandatory.credential_roots()).is_ok())
        .cloned()
        .collect();
    let keychain = selected_keychain(plan, profile, home, &mandatory).cloned();
    let selected = selected_authentication(plan, profile);
    let gh = network == NetworkPolicy::Open && confinement.github_permitted();
    let mut credential_reads = Vec::new();
    if confinement.credentials_permitted() {
        if let Some(selected) =
            selected.filter(|auth| confinement.keychain_permitted() || auth.keychain.is_none())
        {
            credential_reads.extend(
                selected
                    .roots
                    .iter()
                    .filter(|root| {
                        !root.path.starts_with(home.join("Library/Keychains"))
                            || keychain.as_ref() == Some(*root)
                    })
                    .cloned(),
            );
        }
        if gh {
            credential_reads.extend(mandatory.github().iter().cloned());
        }
    }
    // A shared store is a protection with lane carve-outs, never a broad
    // authentication grant. Discard broad reads overlapping it before checking
    // enclosing grants against state; exact selected authentication stays usable.
    credential_reads.retain(|grant| {
        grant.scope == PathScope::Exact
            || !mandatory
                .credentials()
                .iter()
                .any(|root| !mandatory.lane_exceptions(root).is_empty() && root.overlaps(grant))
    });
    // Only retained exact authentication endpoints can authorise an alias.
    // Ambient GitHub configuration reads confer no authority over link targets.
    credential_reads.extend(mandatory.credential_alias_reads(&credential_reads));
    // A package grant passes the existing standalone validator; it never opens
    // the credential/config/session siblings in the enclosing Codex home.
    let packages: Vec<_> = reads
        .iter()
        .chain(&profile.spec.tool_exec_roots)
        .filter(|grant| {
            is_codex_standalone_package(&grant.path, mandatory.credential_roots())
                && validate_read_exec_grant(grant, mandatory.credential_roots()).is_ok()
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
        let lane_exceptions = mandatory.lane_exceptions(root);
        // This separate exact store stays hidden even from Claude itself.
        let exceptions = if root.path == home.join(".claude.json") {
            &empty
        } else {
            &credential_reads
        };
        // A shared store must not be reopened by a broad authentication or
        // GitHub configuration grant. Exact selected credentials still work.
        let reads: Vec<_> = exceptions
            .iter()
            .filter(|grant| lane_exceptions.is_empty() || grant.scope == PathScope::Exact)
            .chain(&lane_exceptions)
            .cloned()
            .collect();
        let runtime_exceptions: Vec<_> = runtime
            .iter()
            .filter(|grant| {
                mandatory
                    .credential_roots()
                    .entries()
                    .iter()
                    .any(|declaration| {
                        declaration.path == root.path
                            && super::validation::SandboxReadAdmission::new(
                                mandatory.credential_roots(),
                            )
                            .permits_carve_out(declaration, grant)
                    })
            })
            .cloned()
            .collect();
        let reads: Vec<_> = reads
            .into_iter()
            .chain(runtime_exceptions.iter().cloned())
            .collect();
        let exec: Vec<_> = packages
            .iter()
            .chain(&lane_exceptions)
            .chain(&runtime_exceptions)
            .cloned()
            .collect();
        push_read_denial(source, root, &reads)?;
        push_denial(source, "file-clone", root, &reads)?;
        push_denial(source, "process-exec", root, &exec)?;
        push_denial(source, "file-map-executable", root, &exec)?;
        push_rule(source, "deny", "file-link", &root.path, root.scope)?;
        if !confinement.credentials_permitted()
            || credential_writes.is_empty()
            || root.path == home.join(".claude.json")
        {
            push_denial(source, "file-write*", root, &lane_exceptions)?;
        } else {
            let writes: Vec<_> = credential_writes
                .iter()
                .filter(|grant| lane_exceptions.is_empty() || grant.scope == PathScope::Exact)
                .chain(&lane_exceptions)
                .cloned()
                .collect();
            push_denial(source, "file-write*", root, &writes)?;
            // A refresh can modify bytes only. Creating, replacing, renaming,
            // changing ownership/mode or relinking the credential stays denied.
            push_denial(
                source,
                "file-write-create file-write-unlink file-write-mode file-write-owner file-write-flags file-write-acl file-write-xattr file-write-setugid file-write-mount file-write-umount",
                root,
                &lane_exceptions,
            )?;
        }
        push_socket_denial(source, root, &lane_exceptions)?;
        push_ancestor_guards(source, root, &mut ancestors)?;
    }
    // Only the typed native keychain mechanism can read the exact login
    // keychain. Other mandatory stores and aliases retain their denials.
    for root in mandatory.non_exempt() {
        let lanes = mandatory.lane_exceptions(&root.clone().into());
        for operation in [
            "file-read*",
            "file-clone",
            "file-link",
            "file-write*",
            "process-exec",
            "file-map-executable",
        ] {
            let mut exceptions = lanes.clone();
            if operation == "file-read*" && root.path == home.join("Library/Keychains") {
                exceptions.extend(keychain.iter().cloned());
            }
            push_denial(source, operation, root, &exceptions)?;
        }
        push_socket_denial(source, root, &lanes)?;
    }
    for root in mandatory.github() {
        let lanes = mandatory.lane_exceptions(&root.clone().into());
        push_denial(source, "file-write*", root, &lanes)?;
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
        .filter(|_| confinement.credentials_permitted())
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
    state_reads.extend(profile.spec.fabric_state_reads.iter().cloned());
    for root in std::iter::once(mandatory.state()).chain(mandatory.state_aliases()) {
        push_read_denial(source, &root.clone().into(), &state_reads)?;
        push_denial(source, "file-clone", root, &state_reads)?;
        push_denial(source, "file-link", root, &private)?;
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
            push_rule(source, "deny", "file-link", &root.path, root.scope)?;
            push_ancestor_guards(source, root, &mut ancestors)?;
        }
    }
    // A lane's own checkout can be a protected worktree: its writer grant, the
    // host owner's writer roots inside other checkouts and a read-only lane's
    // configured project writes stay usable. The rest of each checkout stays
    // immutable.
    let writer_exception: Vec<_> = worktree
        .map(|grants| grants.worktree.clone())
        .into_iter()
        .chain(
            worktree
                .is_some()
                .then(|| mandatory.host_writes().iter().cloned())
                .into_iter()
                .flatten(),
        )
        .chain(
            plan.read_only
                .iter()
                .flat_map(|grants| grants.writes.iter().cloned()),
        )
        .collect();
    for root in mandatory.protected().iter().chain(&plan.protected_roots) {
        let exceptions = if mandatory.protected().contains(root) {
            &writer_exception[..]
        } else {
            &[]
        };
        push_denial(source, "file-write*", root, exceptions)?;
        push_rule(source, "deny", "file-link", &root.path, root.scope)?;
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
        push_rule(source, "deny", "file-link", &root.path, root.scope)?;
        push_ancestor_guards(source, root, &mut ancestors)?;
    }
    for root in &plan.secret_roots {
        for operation in [
            "file-read*",
            "file-clone",
            "file-link",
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
        push_rule(source, "deny", "file-link", &root.path, root.scope)?;
        push_ancestor_guards(source, root, &mut ancestors)?;
    }
    // Host restrictions are final and admit no exceptions, including refresh,
    // prompt, package and GitHub CLI grants.
    for root in confinement.deny_read() {
        for operation in [
            "file-read*",
            "file-clone",
            "file-link",
            "process-exec",
            "file-map-executable",
        ] {
            push_rule(source, "deny", operation, &root.path, root.scope)?;
        }
        push_ancestor_guards(source, root, &mut ancestors)?;
    }
    for root in confinement.deny_write() {
        push_rule(source, "deny", "file-write*", &root.path, root.scope)?;
        push_rule(source, "deny", "file-link", &root.path, root.scope)?;
        push_ancestor_guards(source, root, &mut ancestors)?;
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
    let error = |error| {
        let detail = format!("required path cannot be represented in SBPL: {path:?}: {error}");
        Policy::default().boundary_error(
            "RENDER_SBPL_PATH_AUTHORITY",
            Facts::violation(detail),
            error,
        )
    };
    let text = path
        .to_str()
        .ok_or(PermissionError::NonUtf8Path)
        .map_err(error)?;
    if !profile_path_representable(path) {
        return Err(error(PermissionError::InvalidPath(path.to_owned())));
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
                return Err(error(PermissionError::InvalidPath(path.to_owned())));
            }
            value => escaped.push(value),
        }
    }
    Ok(escaped)
}
