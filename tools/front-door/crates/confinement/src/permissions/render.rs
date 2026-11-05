//! SBPL compilation and rendering.

use super::*;
#[cfg(target_os = "macos")]
use crate::cache::verify_cache_metadata;
#[cfg(target_os = "macos")]
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

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
/// macOS checks SysV segment and semaphore authority only on lookup of an
/// existing object, after creation succeeded. A lane without database IPC could
/// therefore allocate host-global objects that it is then refused permission to
/// remove. Denying the creating system calls, and the legacy multiplexed entry
/// points that reach the same kernel code, stops the allocation itself.
#[cfg(any(target_os = "macos", feature = "snapshot"))]
pub(super) const SYSV_CREATE_DENY_RULES: &str =
    "(deny syscall-unix (syscall-number SYS_shmget SYS_semget SYS_shmsys SYS_semsys))\n";
/// No profile grants SysV message queues, and creation has the same
/// create-but-cannot-remove behaviour, so every lane is refused them.
#[cfg(any(target_os = "macos", feature = "snapshot"))]
pub(super) const SYSV_QUEUE_DENY_RULES: &str =
    "(deny syscall-unix (syscall-number SYS_msgget SYS_msgsys))\n";
/// Fixed operating-system runtime every provider CLI needs under deny-default:
/// kernel and preference reads, TLS trust, DNS, time zone data, and process
/// information and signals within the same sandbox. It names no user path; explicit
/// secret-root denials still override the metadata grant.
#[cfg(any(target_os = "macos", feature = "snapshot"))]
pub(super) const RUNTIME_BASELINE_RULES: &str = r#"(allow sysctl-read)
(deny sysctl-read (sysctl-name-prefix "kern.procargs") (sysctl-name-prefix "kern.proc."))
(allow file-read-metadata)
(allow ipc-posix-shm-read* (ipc-posix-name-prefix "apple.cfprefs."))
(allow signal (target same-sandbox))
(deny process-info*)
(allow process-info* (target same-sandbox))
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
(allow file-read* (literal "/dev/urandom") (literal "/dev/random"))
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
/// Profiles travel in argv: leave headroom below macOS's 1 MiB ARG_MAX for
/// provider arguments and environment.
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
    if plan.nested_confinement {
        return Err(PermissionError::NestedConfinementUnsupported);
    }
    validate_plan_grant_limit(plan, limits.grant_limit)?;
    validate_profile_spec_with_home(&profile.spec, anchors.home, limits.grant_limit)?;
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

    let mut profile_bytes = render_with_limit(&grants, limits.profile_limit_bytes)?.into_bytes();
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

#[cfg(any(target_os = "macos", test))]
pub(super) fn filtered_native_controls(controls: &[String]) -> Vec<String> {
    let filtered: Vec<_> = controls
        .iter()
        .filter(|control| super::validation::valid_native_control(control))
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
            || hard_denied(anchors.home)
                .iter()
                .any(|(path, _)| path_overlaps(&grant.path, path))
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

/// GitHub CLI configuration roots available to explicitly publishing writers.
#[cfg(any(target_os = "macos", feature = "snapshot"))]
pub(super) fn gh_config_paths(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".config/gh"),
        home.join("Library/Application Support/GitHub CLI"),
    ]
}

/// Credential and preservation paths denied to lanes by default. Rendered
/// last so they override every allow, except the scoped gh read below.
/// `Library/Keychains` stays out: Agy reads its login keychain through an
/// exact authentication grant.
#[cfg(any(target_os = "macos", feature = "snapshot"))]
pub(super) fn hard_denied(home: &Path) -> [(PathBuf, PathScope); 12] {
    [
        (".ssh", PathScope::Subtree),
        (".aws", PathScope::Subtree),
        (".azure", PathScope::Subtree),
        (".gnupg", PathScope::Subtree),
        (".config/gh", PathScope::Subtree),
        ("Library/Application Support/GitHub CLI", PathScope::Subtree),
        (".kube", PathScope::Subtree),
        (".secrets", PathScope::Subtree),
        ("Monero", PathScope::Subtree),
        ("Archives/git-preservation", PathScope::Subtree),
        (".cargo/credentials", PathScope::Exact),
        (".cargo/credentials.toml", PathScope::Exact),
    ]
    .map(|(relative, scope)| (home.join(relative), scope))
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

#[cfg(all(test, target_os = "macos"))]
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
    let mut source = String::from("(version 1)\n(deny default)\n");
    // Apple dyld bootstrap rules are shared with the backend probe to keep
    // launch prerequisites and provider profiles aligned.
    source.push_str(DYLD_BOOTSTRAP_RULES);
    source.push_str(RUNTIME_BASELINE_RULES);
    if profile.spec.allow_database_ipc {
        source.push_str("(allow ipc-sysv-shm)\n(allow ipc-sysv-sem)\n");
    } else {
        source.push_str(SYSV_CREATE_DENY_RULES);
    }
    source.push_str(SYSV_QUEUE_DENY_RULES);
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
    // Writers run build scripts in configured write roots and fixture binaries
    // in their own attempt tmp/candidate-N. Scratch already has a read grant.
    if matches!(plan.mode, AccessMode::WorktreeWrite(_)) {
        for root in &plan.write_roots {
            push_rule(&mut source, "allow", "process-exec", &root.path, root.scope)?;
        }
        push_rule(
            &mut source,
            "allow",
            "process-exec",
            &plan.scratch.path,
            PathScope::Subtree,
        )?;
        source.push_str("(allow file-read* (subpath \"/dev/fd\"))\n");
    }
    if profile.spec.allow_process_fork {
        source.push_str("(allow process-fork)\n");
    }
    if profile.spec.allow_pseudo_tty {
        source.push_str(PSEUDO_TTY_RULES);
    }
    if profile.spec.deny_keychain_access {
        source.push_str(concat!(
            "(deny process-exec (literal \"/usr/bin/security\"))\n",
            "(deny mach-lookup (global-name \"com.apple.SecurityServer\") (global-name \"com.apple.securityd\") (global-name \"com.apple.securityd.xpc\"))\n",
        ));
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
    if matches!(plan.network, NetworkAccess::Local | NetworkAccess::Open) {
        source.push_str(LOCAL_SERVICE_RULES);
    }
    if plan.network == NetworkAccess::Open {
        source.push_str("(allow network-outbound (remote ip \"*:*\"))\n");
    }
    if profile.spec.deny_network {
        source.push_str("(deny network*)\n");
    }

    // The gh read comes before the secret-root denies: Seatbelt is
    // last-match-wins, so an overlapping secret root must still hide it.
    let gh_paths = if !profile.spec.deny_credential_access && plan.writer_publishing() {
        gh_config_paths(home)
    } else {
        Vec::new()
    };
    for path in &gh_paths {
        push_rule(&mut source, "allow", "file-read*", path, PathScope::Subtree)?;
    }

    // Protected controls stay readable but immutable; secret roots stay hidden
    // and immutable even if another profile grant overlaps them.
    for grant in &plan.secret_roots {
        push_rule(&mut source, "deny", "file-read*", &grant.path, grant.scope)?;
        push_rule(&mut source, "deny", "file-write*", &grant.path, grant.scope)?;
    }
    for grant in &plan.protected_roots {
        push_rule(&mut source, "deny", "file-write*", &grant.path, grant.scope)?;
    }
    for (path, scope) in hard_denied(home) {
        if !gh_paths.contains(&path) {
            push_rule(&mut source, "deny", "file-read*", &path, scope)?;
        }
        push_rule(&mut source, "deny", "file-write*", &path, scope)?;
    }
    if profile.spec.deny_credential_access {
        for relative in CREDENTIAL_DIRECTORIES
            .iter()
            .chain(NESTED_CREDENTIAL_DIRECTORIES)
            .chain(PROVIDER_CREDENTIAL_DIRECTORIES)
        {
            let path = home.join(relative);
            for operation in ["file-read*", "file-write*", "process-exec"] {
                push_rule(&mut source, "deny", operation, &path, PathScope::Subtree)?;
            }
        }
    }
    if let Some(grants) = worktree_grants {
        let git_marker = grants.worktree.path.join(".git");
        push_rule(
            &mut source,
            "deny",
            "file-write*",
            &git_marker,
            PathScope::Exact,
        )?;
    }
    if source.len() > limit {
        return Err(PermissionError::ProfileTooLarge {
            bytes: source.len(),
            limit,
        });
    }
    Ok(source.into_bytes())
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
