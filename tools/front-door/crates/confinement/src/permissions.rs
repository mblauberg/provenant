//! Typed permission plans and the fail-closed macOS SBPL backend.
//!
//! Provider-owned profile data bounds every requested permission.
//! Linux deliberately exposes an unavailable backend.
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::io::Read;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
#[cfg(target_os = "macos")]
use std::time::Duration;

use provenant_core::CancelToken;
use provenant_supervisor::exec::Executor;
#[cfg(target_os = "macos")]
use provenant_supervisor::exec::{ExecOptions, ExecOutcome, StreamPolicy, Termination};
#[cfg(target_os = "macos")]
use provenant_supervisor::fs::Metadata;
#[cfg(target_os = "macos")]
use provenant_supervisor::fs::SafeDir;
use provenant_supervisor::fs::{FileKind, PublishMode, SafeFs};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::repo::{GitRefName, VerifiedWorktree};
pub use provenant_records::run::NetworkAccess;

const MAX_PATH_BYTES: usize = 4096;
const CARGO_CACHE_OWNER: &str = ".provenant-worktree-owner";
const CARGO_CACHE_OWNER_PREFIX: &[u8] = b"provenant-lane-cargo-owner-v2\n";
const LEGACY_CARGO_CACHE_OWNER_PREFIX: &[u8] = b"provenant-lane-cargo-owner-v1\n";
const MAX_CACHE_SWEEP_DEPTH: usize = 32;
#[cfg(any(target_os = "macos", test))]
const MAX_PATH_GRANTS: usize = 128;
#[cfg(any(target_os = "macos", test))]
const MAX_NETWORK_GRANTS: usize = 128;
#[cfg(target_os = "macos")]
const MAX_GIT_HEAD_BYTES: u64 = 1024;
#[cfg(target_os = "macos")]
const MAX_WRITABLE_TREE_ENTRIES: usize = 100_000;
#[cfg(target_os = "macos")]
const MAX_WRITABLE_TREE_DEPTH: usize = 128;
#[cfg(target_os = "macos")]
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
#[cfg(target_os = "macos")]
const DYLD_BOOTSTRAP_RULES: &str = r#"
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
/// Fixed operating-system runtime every provider CLI needs under deny-default:
/// kernel and preference reads, TLS trust, DNS and time zone data. It names no
/// user path; explicit secret-root denials still override the metadata grant.
#[cfg(target_os = "macos")]
const RUNTIME_BASELINE_RULES: &str = r#"(allow sysctl-read)
(allow file-read-metadata)
(allow ipc-posix-shm-read* (ipc-posix-name-prefix "apple.cfprefs."))
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
#[cfg(target_os = "macos")]
const PROBE_PROCESS_RULES: &str = "(allow process-exec (literal \"/usr/bin/true\"))\n(allow process-exec (literal \"/bin/cat\"))\n";
/// Localhost services a build or test run talks to: any loopback TCP port,
/// its own listeners, and a local Postgres server's Unix socket.
#[cfg(target_os = "macos")]
const LOCAL_SERVICE_RULES: &str = r##"(allow network-bind (local ip "localhost:*"))
(allow network-inbound (local ip "localhost:*"))
(allow network-outbound (remote ip "localhost:*"))
(allow network-outbound (remote unix-socket (path-regex #"^/private/tmp/\.s\.PGSQL\.[0-9]+$")))
"##;
#[cfg(target_os = "macos")]
const PROFILE_LIMIT: usize = 64 * 1024;
#[cfg(target_os = "macos")]
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(target_os = "macos")]
const PROBE_CAPTURE_LIMIT: usize = 1024;
#[cfg(target_os = "macos")]
const CLEANUP_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    MacOsSbpl,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AvailabilityReason {
    UnsupportedPlatform,
    SandboxExecutableMissing,
    SandboxExecutableUntrusted,
    ProbeFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    Available { backend: BackendKind },
    Unavailable { reason: AvailabilityReason },
}

#[derive(Debug, Error)]
pub enum PermissionError {
    #[error("invalid read-only project write policy")]
    InvalidReadOnlyPolicy,
    #[error("path is not an absolute, normalised path")]
    InvalidPath,
    #[error("permission root is too broad")]
    BroadRoot,
    #[error("credential root requires a selected fixture-backed mechanism")]
    CredentialRoot,
    #[error("permission root overlaps a protected path")]
    ProtectedOverlap,
    #[error("required held path anchor is missing or changed")]
    AnchorMismatch,
    #[error("writable tree contains a hard-linked regular file")]
    HardLinkedWritableFile,
    #[error("path is not valid UTF-8 for the SBPL source format")]
    NonUtf8Path,
    #[error("profile or fixture record is malformed")]
    InvalidFixture,
    #[error("requested permission is not present in the provider profile")]
    PermissionNotRecorded,
    #[error("provider process execution policy is missing")]
    ProcessPolicyMissing,
    #[error("worktree Git writes need exact verified Git authority paths")]
    GitWriteAuthorityMissing,
    #[error("nested confinement is unsupported by this backend")]
    NestedConfinementUnsupported,
    #[error("compiled profile exceeds its byte bound")]
    ProfileTooLarge,
    #[error("sandbox backend is unavailable on this platform")]
    BackendUnavailable,
    #[error("sandbox backend probe reported unavailable: {0:?}")]
    BackendNotAvailable(AvailabilityReason),
    #[error("sandbox probe was cancelled")]
    Cancelled,
    #[error("safe filesystem operation failed")]
    Filesystem(#[source] provenant_supervisor::fs::FsError),
    #[error("sandbox probe execution failed")]
    Execution(#[source] provenant_supervisor::exec::ExecError),
    #[error("verified repository authority could not be revalidated")]
    Repository(#[from] crate::repo::RepoError),
    #[error("bounded Git HEAD read failed")]
    HeadRead(#[source] std::io::Error),
    #[error("lane cache sweep could not complete: {0}")]
    CacheSweepIo(#[source] std::io::Error),
    #[error("lane cache contents exceed the sweep depth bound")]
    CacheSweepDepth,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathScope {
    Exact,
    Subtree,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathGrant {
    pub path: PathBuf,
    pub scope: PathScope,
}

impl PathGrant {
    pub fn validate(&self) -> Result<(), PermissionError> {
        validate_absolute(&self.path)?;
        if is_broad_root(&self.path) {
            return Err(PermissionError::BroadRoot);
        }
        Ok(())
    }

    pub fn overlaps(&self, other: &Self) -> bool {
        path_overlaps(&self.path, &other.path)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkEndpoint {
    pub address: String,
    pub port: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthenticationProfile {
    pub mechanism: String,
    pub roots: Vec<PathGrant>,
    /// Exact credential files the CLI rewrites in place on token refresh.
    /// Each must also be a read root; no create, rename or unlink is granted.
    #[serde(default)]
    pub in_place_writes: Vec<PathGrant>,
}

/// Provider policy is declarative data. There are no provider-name branches in
/// the compiler; only the selected authentication mechanism is included.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderProfileSpec {
    /// Exact provider/runtime executables required by this adapter.
    pub process_executables: Vec<PathGrant>,
    /// Forking is broad process authority and requires an explicit profile grant.
    pub allow_process_fork: bool,
    /// Deny keychain IPC and its CLI even when a tool can execute system programs.
    pub deny_keychain_access: bool,
    pub runtime_reads: Vec<PathGrant>,
    /// Literal directory discovery; never the contents of provider config.
    pub discovery_reads: Vec<PathGrant>,
    pub authentication: Vec<AuthenticationProfile>,
    pub continuation_reads: Vec<PathGrant>,
    /// Provider-owned state the CLI must write outside scratch, such as one
    /// session transcript. It never overlaps an authentication root.
    pub state_writes: Vec<PathGrant>,
    /// Directories whose executables the provider's own tools may run, for
    /// providers whose file reading goes through a shell. Exec only: reading
    /// them needs the runtime baseline or another grant.
    pub tool_exec_roots: Vec<PathGrant>,
    /// Shared toolchain stores and caches the provider's tools write, such as
    /// the Cargo registry and the pnpm store. Read and write, never executable
    /// toolchain directories and never a credential root.
    pub tool_writes: Vec<PathGrant>,
    pub provider_api: Vec<NetworkEndpoint>,
    /// The CLI talks to its own private server over loopback TCP: bind,
    /// accept and connect on `localhost` only.
    pub loopback_server: bool,
    pub native_controls: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "authority", rename_all = "snake_case")]
pub enum AccessMode {
    ReadOnly,
    WorktreeWrite(Box<WorktreeWriteAuthority>),
}

/// A digest binds the plan to Git grants and its one host-derived build cache.
/// Neither the selected ref nor concrete Git paths enter the record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorktreeWriteAuthority {
    pub git_grants_digest: [u8; 32],
    cargo_cache: PathGrant,
}

impl WorktreeWriteAuthority {
    /// Exact lane-only cache already bound into this writer authority.
    pub fn cargo_cache(&self) -> &PathGrant {
        &self.cargo_cache
    }

    pub fn for_worktree(
        worktree: &VerifiedWorktree,
        selected_ref: &GitRefName,
        home: &Path,
    ) -> Result<Self, PermissionError> {
        worktree.repo().revalidate()?;
        let grants = WorktreeWriteGrants::derive(worktree, selected_ref);
        grants.validate()?;
        let cargo_cache = worktree_cargo_cache(home, worktree)?;
        Ok(Self {
            git_grants_digest: grants.digest(&cargo_cache),
            cargo_cache,
        })
    }
}

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
    let name: String = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let grant = PathGrant {
        path: home.join("Library/Caches/provenant-lane-cargo").join(name),
        scope: PathScope::Subtree,
    };
    validate_noncredential_grant(&grant)?;
    Ok(grant)
}

/// Remove only marked lane caches whose recorded worktree incarnation is absent.
/// A sweep can race a lane preparing its cache only if the lane forged its own marker.
pub fn prune_orphaned_worktree_cargo_caches(home: &Path) -> Result<usize, PermissionError> {
    use provenant_supervisor::fs::LocalFs;
    use std::io;

    validate_absolute(home)?;
    let cache_root = home.join("Library/Caches/provenant-lane-cargo");
    match std::fs::symlink_metadata(&cache_root) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Ok(0),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(PermissionError::CacheSweepIo(error)),
    }
    let root = LocalFs
        .open_anchor(&cache_root)
        .map_err(PermissionError::Filesystem)?;
    let mut removed = 0;
    let mut failures = Vec::new();
    for entry in root.entries().map_err(PermissionError::Filesystem)? {
        let name = match entry {
            Ok(name) => name,
            Err(error) => {
                failures.push(format!("unreadable lane cache entry: {error}"));
                continue;
            }
        };
        match prune_worktree_cache(root.as_ref(), &name) {
            Ok(true) => removed += 1,
            Ok(false) => {}
            Err(error) => failures.push(format!("{:?}: {error}", name)),
        }
    }
    if !failures.is_empty() {
        return Err(PermissionError::CacheSweepIo(std::io::Error::other(
            failures.join("; "),
        )));
    }
    Ok(removed)
}

fn prune_worktree_cache(
    root: &dyn provenant_supervisor::fs::SafeDir,
    name: &OsStr,
) -> Result<bool, PermissionError> {
    use std::io;

    let Some(digest) = name.to_str() else {
        return Ok(false);
    };
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(false);
    }
    let metadata = match root.metadata_at(name, false) {
        Ok(metadata) if metadata.kind == FileKind::Directory => metadata,
        Ok(_) => return Ok(false),
        Err(error) if error.is_not_found() => return Ok(false),
        Err(error) => return Err(PermissionError::Filesystem(error)),
    };
    if metadata.uid != provenant_supervisor::fs::effective_uid() || metadata.mode & 0o7777 != 0o700
    {
        return Ok(false);
    }
    let cache = match root.open_verified_directory(name) {
        Ok(cache) => cache,
        Err(error) if error.is_not_found() => return Ok(false),
        Err(error) => return Err(PermissionError::Filesystem(error)),
    };
    let marker_name = OsStr::new(CARGO_CACHE_OWNER);
    let marker_meta = match cache.metadata_at(marker_name, false) {
        Ok(metadata)
            if metadata.kind == FileKind::Regular
                && metadata.links == 1
                && metadata.uid == provenant_supervisor::fs::effective_uid() =>
        {
            metadata
        }
        Ok(_) => return Ok(false),
        Err(error) if error.is_not_found() => return Ok(false),
        Err(error) => return Err(PermissionError::Filesystem(error)),
    };
    if marker_meta.size > 8192 {
        return Ok(false);
    }
    let mut marker = Vec::with_capacity(marker_meta.size as usize);
    let file = match cache.open_read(marker_name) {
        Ok(file) => file,
        Err(error) if error.is_not_found() => return Ok(false),
        Err(error) => return Err(PermissionError::Filesystem(error)),
    };
    if let Err(error) = file.take(8193).read_to_end(&mut marker) {
        return Err(PermissionError::CacheSweepIo(error));
    }
    if marker.len() > 8192 {
        return Ok(false);
    }
    if marker.starts_with(LEGACY_CARGO_CACHE_OWNER_PREFIX) {
        return Ok(false);
    }
    if !marker.starts_with(CARGO_CACHE_OWNER_PREFIX) {
        return Ok(false);
    }
    let owner_bytes = &marker[CARGO_CACHE_OWNER_PREFIX.len()..];
    // Legacy markers have no inode identity, so retain them rather than guess.
    if owner_bytes.len() <= 16 {
        return Ok(false);
    }
    let (identity, path_bytes) = owner_bytes.split_at(16);
    let recorded_device = u64::from_le_bytes(
        identity[..8]
            .try_into()
            .map_err(|_| PermissionError::InvalidFixture)?,
    );
    let recorded_inode = u64::from_le_bytes(
        identity[8..]
            .try_into()
            .map_err(|_| PermissionError::InvalidFixture)?,
    );
    if path_bytes.is_empty() {
        return Ok(false);
    }
    let owner = PathBuf::from(OsString::from_vec(path_bytes.to_vec()));
    if !owner.is_absolute()
        || owner
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Ok(false);
    }
    match std::fs::symlink_metadata(&owner) {
        Ok(metadata) if metadata.dev() == recorded_device && metadata.ino() == recorded_inode => {
            return Ok(false);
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(PermissionError::CacheSweepIo(error)),
    }
    remove_cache_contents(cache.as_ref())?;
    drop(cache);
    root.rmdir(name).map_err(PermissionError::Filesystem)?;
    Ok(true)
}

fn remove_cache_contents(
    directory: &dyn provenant_supervisor::fs::SafeDir,
) -> Result<(), PermissionError> {
    remove_cache_contents_at(directory, 0)
}

fn remove_cache_contents_at(
    directory: &dyn provenant_supervisor::fs::SafeDir,
    depth: usize,
) -> Result<(), PermissionError> {
    if depth >= MAX_CACHE_SWEEP_DEPTH {
        return Err(PermissionError::CacheSweepDepth);
    }
    for entry in directory.entries().map_err(PermissionError::Filesystem)? {
        let name = entry.map_err(PermissionError::Filesystem)?;
        let metadata = directory
            .metadata_at(&name, false)
            .map_err(PermissionError::Filesystem)?;
        if metadata.kind == FileKind::Directory {
            let child = directory
                .open_verified_directory(&name)
                .map_err(PermissionError::Filesystem)?;
            remove_cache_contents_at(child.as_ref(), depth + 1)?;
            drop(child);
            directory
                .rmdir(&name)
                .map_err(PermissionError::Filesystem)?;
        } else {
            directory
                .unlink(&name)
                .map_err(PermissionError::Filesystem)?;
        }
    }
    Ok(())
}

fn verify_cache_metadata(
    metadata: &provenant_supervisor::fs::Metadata,
) -> Result<(), PermissionError> {
    if metadata.kind != FileKind::Directory
        || metadata.uid != provenant_supervisor::fs::effective_uid()
        || metadata.mode & 0o7777 != 0o700
    {
        return Err(PermissionError::AnchorMismatch);
    }
    Ok(())
}

/// Prepare only derived writer directories outside the child sandbox. This
/// also creates missing nested branch/ref-log parents without granting the
/// child authority to create sibling refs. Call while holding writer custody,
/// before collecting `required_anchors`. Shared config and packed refs remain
/// immutable: the host fetches before launch and sets upstream after exit.
pub fn prepare_writer_directories(
    safe_fs: &dyn SafeFs,
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
        let mut directory = safe_fs
            .open_anchor(Path::new("/"))
            .map_err(PermissionError::Filesystem)?;
        let mut traversed = PathBuf::from("/");
        for component in path.components() {
            let Component::Normal(name) = component else {
                continue;
            };
            directory = match directory.open_directory(name, false) {
                Ok(child) => child,
                Err(error) if error.is_not_found() => {
                    directory
                        .mkdir(name, 0o700)
                        .map_err(PermissionError::Filesystem)?;
                    directory
                        .open_directory(name, false)
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
                        0o600,
                        PublishMode::Replace,
                    )
                    .map_err(PermissionError::Filesystem)?;
            }
        }
    }
    Ok(())
}

fn validate_writer_cache(
    authority: &WorktreeWriteAuthority,
    worktree: &VerifiedWorktree,
    home: &Path,
) -> Result<(), PermissionError> {
    if authority.cargo_cache != worktree_cargo_cache(home, worktree)? {
        return Err(PermissionError::GitWriteAuthorityMissing);
    }
    Ok(())
}

/// A pathspec `git commit` holds its temporary index as
/// `next-index-<pid>.lock` and unlinks it; the pid is not known in advance.
const NEXT_INDEX_SAMPLE: &str = "next-index-0.lock";

struct WorktreeWriteGrants {
    worktree: PathGrant,
    private_git_dir: PathGrant,
    common_git_dir: PathGrant,
    index: [PathGrant; 2],
    /// Sample member of the pathspec-commit temporary index family
    /// (`next-index-<digits>.lock`) in the private git directory.
    next_index: PathGrant,
    /// Git reads this shared exclude file during status and build fingerprinting.
    common_exclude: PathGrant,
    /// Deleting any ref, even an unpacked pseudoref such as AUTO_MERGE, takes
    /// this lock. `packed-refs` itself stays read-only, so the lock can never be
    /// committed over it.
    packed_refs_lock: PathGrant,
    head: [PathGrant; 14],
    objects: PathGrant,
    selected_ref: [PathGrant; 4],
}

impl WorktreeWriteGrants {
    fn derive(worktree: &VerifiedWorktree, selected_ref: &GitRefName) -> Self {
        let repo = worktree.repo();
        let private = repo.private_git_dir();
        let common = repo.common_git_dir();
        let exact = |path: PathBuf| PathGrant {
            path,
            scope: PathScope::Exact,
        };
        let subtree = |path: PathBuf| PathGrant {
            path,
            scope: PathScope::Subtree,
        };
        let branch = selected_ref.short_name();
        Self {
            worktree: subtree(worktree.path().to_owned()),
            private_git_dir: subtree(private.to_owned()),
            common_git_dir: subtree(common.to_owned()),
            index: ["index", "index.lock"].map(|name| exact(private.join(name))),
            next_index: exact(private.join(NEXT_INDEX_SAMPLE)),
            common_exclude: exact(common.join("info/exclude")),
            packed_refs_lock: exact(common.join("packed-refs.lock")),
            // `git commit` writes its message buffer and clears AUTO_MERGE here;
            // a local `git merge` records its state and ORIG_HEAD here too.
            head: [
                "HEAD",
                "HEAD.lock",
                "logs/HEAD",
                "logs/HEAD.lock",
                "COMMIT_EDITMSG",
                "AUTO_MERGE",
                "AUTO_MERGE.lock",
                "MERGE_HEAD",
                "MERGE_MSG",
                "MERGE_MODE",
                "ORIG_HEAD",
                "ORIG_HEAD.lock",
                // Rerere records conflicted paths here when <common>/rr-cache exists.
                "MERGE_RR",
                "MERGE_RR.lock",
            ]
            .map(|name| exact(private.join(name))),
            objects: subtree(common.join("objects")),
            selected_ref: [
                format!("refs/heads/{branch}"),
                format!("refs/heads/{branch}.lock"),
                format!("logs/refs/heads/{branch}"),
                format!("logs/refs/heads/{branch}.lock"),
            ]
            .map(|relative| exact(common.join(relative))),
        }
    }

    fn iter(&self) -> impl Iterator<Item = &PathGrant> {
        std::iter::once(&self.worktree)
            .chain([&self.private_git_dir, &self.common_git_dir])
            .chain(self.index.iter())
            .chain(std::iter::once(&self.next_index))
            .chain([&self.common_exclude, &self.packed_refs_lock])
            .chain(self.head.iter())
            .chain(std::iter::once(&self.objects))
            .chain(self.selected_ref.iter())
    }

    fn digest(&self, cache: &PathGrant) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"provenant-worktree-git-grants-v7\0");
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

    fn validate(&self) -> Result<(), PermissionError> {
        self.worktree.validate()?;
        for grant in self.iter().skip(1) {
            validate_noncredential_grant(grant)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanPermissions {
    pub read_only: Option<crate::plan::ReadOnlyGrants>,
    pub mode: AccessMode,
    pub scratch: PathGrant,
    /// Attempt-private retained provider state, separate from disposable scratch.
    pub private_state: Option<PathGrant>,
    pub read_roots: Vec<PathGrant>,
    /// Readable control files that remain immutable during the attempt.
    pub protected_roots: Vec<PathGrant>,
    /// Sensitive roots that must not be read or written by the provider.
    pub secret_roots: Vec<PathGrant>,
    pub selected_authentication: Option<String>,
    pub include_continuation: bool,
    /// Extra directories the dispatch made writable, such as a project lock
    /// directory beside the repository. Read and write, outside Git metadata.
    pub write_roots: Vec<PathGrant>,
    pub network: NetworkAccess,
    pub nested_confinement: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureOutcome {
    Allowed,
    Denied,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureOperation {
    DeclaredRead,
    UndeclaredRead,
    UndeclaredWrite,
    RuntimeRead,
    AuthenticationRead,
    ContinuationRead,
    ProtectedInstructionRead,
    SecretRead,
    WorktreeWrite,
    GitIndexWrite,
    GitHeadWrite,
    GitObjectsWrite,
    SelectedRefWrite,
    GitConfigWrite,
    OtherRefWrite,
    ProtectedWrite,
    ProviderEndpoint,
    OtherNetwork,
    DeclaredProcessExec,
    UndeclaredProcessExec,
    ProcessFork,
    StateWrite,
    ShellExec,
    KeychainTool,
    KeychainRead,
    LoopbackServer,
}

/// Typed provider policy consumed by the permission compiler. Providers own
/// the launch policy; observations are retained for recorder tooling only.
pub struct ProviderProfile {
    pub spec: ProviderProfileSpec,
    pub observations: BTreeMap<FixtureOperation, FixtureOutcome>,
}

pub struct AnchoredDirectory {
    path: PathBuf,
    #[cfg(target_os = "macos")]
    directory: Box<dyn SafeDir>,
    #[cfg(target_os = "macos")]
    identity: Metadata,
}

impl AnchoredDirectory {
    /// `path` must be the canonical absolute spelling resolved by the caller's
    /// verified-path logic. Operations use only the held SafeDir thereafter.
    pub fn open_canonical(
        safe_fs: &dyn SafeFs,
        path: impl AsRef<Path>,
    ) -> Result<Self, PermissionError> {
        let path = path.as_ref();
        validate_absolute(path)?;
        let canonical =
            provenant_supervisor::fs::canonicalize(path).map_err(PermissionError::Filesystem)?;
        if canonical.as_path() != path {
            return Err(PermissionError::InvalidPath);
        }
        let directory = safe_fs
            .open_anchor(path)
            .map_err(PermissionError::Filesystem)?;
        let identity = directory.metadata().map_err(PermissionError::Filesystem)?;
        if identity.kind != FileKind::Directory {
            return Err(PermissionError::AnchorMismatch);
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
    fn verify(&self, safe_fs: &dyn SafeFs) -> Result<(), PermissionError> {
        let canonical = provenant_supervisor::fs::canonicalize(&self.path)
            .map_err(PermissionError::Filesystem)?;
        if canonical.as_path() != self.path {
            return Err(PermissionError::AnchorMismatch);
        }
        let named = safe_fs
            .open_anchor(&self.path)
            .map_err(PermissionError::Filesystem)?;
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
            return Err(PermissionError::AnchorMismatch);
        }
        Ok(())
    }
}

pub struct Anchors<'a> {
    /// Canonical user home resolved by the host, independent of plan records.
    pub home: &'a Path,
    pub directories: &'a [AnchoredDirectory],
    pub safe_fs: &'a dyn SafeFs,
    /// Linked tree and selected branch derive Git paths at the authority edge.
    pub worktree: Option<(&'a VerifiedWorktree, &'a GitRefName)>,
}

#[cfg(target_os = "macos")]
impl Anchors<'_> {
    fn verify_all(&self) -> Result<(), PermissionError> {
        for anchor in self.directories {
            anchor.verify(self.safe_fs)?;
        }
        Ok(())
    }

    fn verify_grant_held(&self, grant: &PathGrant) -> Result<(), PermissionError> {
        match grant.scope {
            PathScope::Subtree => {
                let anchor = self
                    .directories
                    .iter()
                    .find(|anchor| anchor.path == grant.path)
                    .ok_or(PermissionError::AnchorMismatch)?;
                anchor.verify(self.safe_fs)
            }
            PathScope::Exact => {
                self.exact_grant_metadata(&grant.path)?;
                Ok(())
            }
        }
    }

    fn verify_executable(&self, grant: &PathGrant) -> Result<(), PermissionError> {
        if grant.scope != PathScope::Exact {
            return Err(PermissionError::InvalidPath);
        }
        match self.exact_grant_metadata(&grant.path)? {
            Some(_) => Ok(()),
            None => Err(PermissionError::AnchorMismatch),
        }
    }

    fn verify_single_link_grant(&self, grant: &PathGrant) -> Result<(), PermissionError> {
        if grant.scope != PathScope::Exact {
            return Err(PermissionError::InvalidPath);
        }
        match self.exact_grant_metadata(&grant.path)? {
            Some(metadata) if metadata.links == 1 => Ok(()),
            Some(_) => Err(PermissionError::AnchorMismatch),
            None => Ok(()),
        }
    }

    fn exact_grant_metadata(&self, path: &Path) -> Result<Option<Metadata>, PermissionError> {
        let parent = path.parent().ok_or(PermissionError::InvalidPath)?;
        let name = path.file_name().ok_or(PermissionError::InvalidPath)?;
        let anchor = self
            .directories
            .iter()
            .find(|anchor| anchor.path == parent)
            .ok_or(PermissionError::AnchorMismatch)?;
        anchor.verify(self.safe_fs)?;
        match anchor.directory.metadata_at(name, false) {
            Ok(metadata) if metadata.kind == FileKind::Regular => Ok(Some(metadata)),
            Ok(_) => Err(PermissionError::AnchorMismatch),
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
        .chain(&spec.discovery_reads)
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
            PathScope::Exact => grant.path.parent().map(Path::to_owned),
        })
        .collect()
}

pub struct BackendContext<'a> {
    pub safe_fs: &'a dyn SafeFs,
    pub executor: &'a dyn Executor,
    pub cancel: &'a CancelToken,
    #[cfg(target_os = "macos")]
    pub env: &'a [(OsString, OsString)],
}

pub trait ConfinementBackend: Send + Sync {
    fn probe(&self, context: &BackendContext<'_>) -> Result<Availability, PermissionError>;

    /// Compile immediately before launch while holding writer exclusion over
    /// every writable root through the child handoff. The pathname sandbox
    /// denies child-created hard links but cannot stop an external writer from
    /// adding an alias after this census.
    fn compile(
        &self,
        plan: &PlanPermissions,
        profile: &ProviderProfile,
        anchors: &Anchors<'_>,
    ) -> Result<ConfinedLaunch, PermissionError>;
}

/// Returns a backend bound to a successful OS probe. On macOS the returned
/// backend's private readiness value is immutable, so `compile` cannot be
/// called through an unprobed backend instance.
pub fn system_backend(
    context: &BackendContext<'_>,
) -> Result<Box<dyn ConfinementBackend>, PermissionError> {
    #[cfg(target_os = "macos")]
    {
        match probe_sandbox(context)? {
            Availability::Available {
                backend: BackendKind::MacOsSbpl,
            } => Ok(Box::new(MacOsSandboxBackend { ready: true })),
            Availability::Unavailable { reason } => {
                Err(PermissionError::BackendNotAvailable(reason))
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = context;
        Err(PermissionError::BackendNotAvailable(
            AvailabilityReason::UnsupportedPlatform,
        ))
    }
}

#[cfg(target_os = "macos")]
struct MacOsSandboxBackend {
    ready: bool,
}

#[cfg(target_os = "macos")]
impl ConfinementBackend for MacOsSandboxBackend {
    fn probe(&self, context: &BackendContext<'_>) -> Result<Availability, PermissionError> {
        if !self.ready {
            return Err(PermissionError::BackendUnavailable);
        }
        probe_sandbox(context)
    }

    fn compile(
        &self,
        plan: &PlanPermissions,
        profile: &ProviderProfile,
        anchors: &Anchors<'_>,
    ) -> Result<ConfinedLaunch, PermissionError> {
        if !self.ready {
            return Err(PermissionError::BackendUnavailable);
        }
        compile_sbpl(plan, profile, anchors)
    }
}

#[cfg(target_os = "macos")]
fn probe_sandbox(context: &BackendContext<'_>) -> Result<Availability, PermissionError> {
    probe_sandbox_with_diagnostic(context).map(|(availability, _)| availability)
}

#[cfg(target_os = "macos")]
fn describe_sandbox_probe(phase: &str, outcome: &ExecOutcome) -> String {
    format!(
        "{phase}: termination={:?}, timed_out={}, drain_truncated={}, stdout_read_failed={}, stderr_read_failed={}, stdout_bytes={}, stderr={:?}",
        outcome.termination,
        outcome.timed_out,
        outcome.drain_truncated,
        outcome.stdout.read_failed,
        outcome.stderr.read_failed,
        outcome.stdout.bytes.len(),
        String::from_utf8_lossy(&outcome.stderr.bytes),
    )
}

#[cfg(target_os = "macos")]
fn probe_exec_options<'a>(
    argv: &'a [OsString],
    context: &'a BackendContext<'_>,
) -> ExecOptions<'a> {
    let mut options = ExecOptions::new(argv, Path::new("/"), PROBE_TIMEOUT);
    options.env = Some(context.env);
    options.cancel = Some(context.cancel);
    options.stdout = StreamPolicy::Capture {
        limit: PROBE_CAPTURE_LIMIT,
    };
    options.stderr = StreamPolicy::Capture {
        limit: PROBE_CAPTURE_LIMIT,
    };
    options.terminate_grace = CLEANUP_TIMEOUT;
    options.reap_timeout = CLEANUP_TIMEOUT;
    options
}

#[cfg(target_os = "macos")]
fn probe_sandbox_with_diagnostic(
    context: &BackendContext<'_>,
) -> Result<(Availability, Option<String>), PermissionError> {
    if context.cancel.is_cancelled() {
        return Err(PermissionError::Cancelled);
    }
    let executable = match context.safe_fs.open_source(Path::new(SANDBOX_EXEC)) {
        Ok(executable) => executable,
        Err(error) if error.is_not_found() => {
            return Ok((
                Availability::Unavailable {
                    reason: AvailabilityReason::SandboxExecutableMissing,
                },
                None,
            ));
        }
        Err(error) => return Err(PermissionError::Filesystem(error)),
    };
    let metadata = executable.metadata().map_err(PermissionError::Filesystem)?;
    if metadata.kind != FileKind::Regular
        || metadata.uid != 0
        || metadata.mode & 0o111 == 0
        || metadata.mode & 0o022 != 0
    {
        return Ok((
            Availability::Unavailable {
                reason: AvailabilityReason::SandboxExecutableUntrusted,
            },
            None,
        ));
    }

    let mut probe_profile = String::from("(version 1)\n(deny default)\n");
    probe_profile.push_str(DYLD_BOOTSTRAP_RULES);
    probe_profile.push_str(PROBE_PROCESS_RULES);
    let argv = [
        OsString::from(SANDBOX_EXEC),
        OsString::from("-p"),
        OsString::from(probe_profile.as_str()),
        OsString::from("/usr/bin/true"),
    ];
    let options = probe_exec_options(&argv, context);
    match context.executor.execute(options) {
        Ok(outcome) if successful_probe(&outcome) => (),
        Err(provenant_supervisor::exec::ExecError::Cancelled) => {
            return Err(PermissionError::Cancelled);
        }
        Err(error) => return Err(PermissionError::Execution(error)),
        Ok(outcome) => {
            return Ok((
                Availability::Unavailable {
                    reason: AvailabilityReason::ProbeFailed,
                },
                Some(describe_sandbox_probe(
                    "sandbox-exec allowed-command probe",
                    &outcome,
                )),
            ));
        }
    };

    let denied_argv = [
        OsString::from(SANDBOX_EXEC),
        OsString::from("-p"),
        OsString::from(probe_profile.as_str()),
        OsString::from("/bin/cat"),
        OsString::from("/etc/passwd"),
    ];
    let denied_options = probe_exec_options(&denied_argv, context);
    match context.executor.execute(denied_options) {
        Ok(outcome) if denied_probe(&outcome) => Ok((
            Availability::Available {
                backend: BackendKind::MacOsSbpl,
            },
            None,
        )),
        Err(provenant_supervisor::exec::ExecError::Cancelled) => Err(PermissionError::Cancelled),
        Err(error) => Err(PermissionError::Execution(error)),
        Ok(outcome) => Ok((
            Availability::Unavailable {
                reason: AvailabilityReason::ProbeFailed,
            },
            Some(describe_sandbox_probe(
                "sandbox-exec denied-command probe",
                &outcome,
            )),
        )),
    }
}

#[cfg(target_os = "macos")]
fn successful_probe(outcome: &ExecOutcome) -> bool {
    matches!(outcome.termination, Termination::Exit(0))
        && !outcome.timed_out
        && !outcome.drain_truncated
        && !outcome.stdout.read_failed
        && !outcome.stderr.read_failed
}

#[cfg(target_os = "macos")]
fn denied_probe(outcome: &ExecOutcome) -> bool {
    matches!(outcome.termination, Termination::Exit(code) if code != 0)
        && String::from_utf8_lossy(&outcome.stderr.bytes).contains("Operation not permitted")
        && !outcome.timed_out
        && !outcome.cancelled
        && !outcome.overflow_stopped
        && !outcome.drain_truncated
        && !outcome.stdout.read_failed
        && !outcome.stderr.read_failed
}

#[cfg(target_os = "macos")]
fn compile_sbpl(
    plan: &PlanPermissions,
    profile: &ProviderProfile,
    anchors: &Anchors<'_>,
) -> Result<ConfinedLaunch, PermissionError> {
    anchors.verify_all()?;
    if plan.nested_confinement {
        return Err(PermissionError::NestedConfinementUnsupported);
    }
    if plan.read_roots.len() > MAX_PATH_GRANTS
        || plan.protected_roots.len() > MAX_PATH_GRANTS
        || plan.secret_roots.len() > MAX_PATH_GRANTS
        || plan.write_roots.len() > MAX_PATH_GRANTS
    {
        return Err(PermissionError::ProfileTooLarge);
    }
    validate_profile_spec(&profile.spec)?;
    for grant in &plan.read_roots {
        validate_read_exec_grant(grant)?;
    }
    for grant in &plan.protected_roots {
        validate_noncredential_grant(grant)?;
    }
    let worktree_grants = match &plan.mode {
        AccessMode::ReadOnly => None,
        AccessMode::WorktreeWrite(authority) => {
            Some(verify_worktree_authority(authority, anchors)?)
        }
    };
    if let Some(grants) = &worktree_grants {
        verify_writable_tree(&grants.worktree.path, anchors)?;
        verify_writable_tree(&grants.objects.path, anchors)?;
    }
    let mut read_grants = plan.read_roots.clone();
    if let Some(read_only) = &plan.read_only {
        if !matches!(plan.mode, AccessMode::ReadOnly) {
            return Err(PermissionError::InvalidReadOnlyPolicy);
        }
        read_only.revalidate(anchors.safe_fs)?;
        read_grants.extend(read_only.git_reads.iter().cloned());
        for grant in &read_only.writes {
            validate_noncredential_grant(grant)?;
            if plan
                .protected_roots
                .iter()
                .chain(&plan.secret_roots)
                .chain(&read_only.git_reads)
                .any(|p| grant.overlaps(p))
            {
                return Err(PermissionError::ProtectedOverlap);
            }
            verify_writable_tree(&grant.path, anchors)?;
        }
    }
    if let Some(grants) = &worktree_grants {
        read_grants.push(grants.worktree.clone());
    }
    read_grants.extend(plan.protected_roots.iter().cloned());
    read_grants.extend(profile.spec.runtime_reads.iter().cloned());
    read_grants.extend(profile.spec.discovery_reads.iter().cloned());
    read_grants.extend(profile.spec.process_executables.iter().cloned());
    let mut credential_writes = Vec::new();
    if let Some(mechanism) = &plan.selected_authentication {
        let selected = profile
            .spec
            .authentication
            .iter()
            .find(|item| &item.mechanism == mechanism)
            .ok_or(PermissionError::PermissionNotRecorded)?;
        for grant in &selected.roots {
            read_grants.push(grant.clone());
        }
        credential_writes.extend(selected.in_place_writes.iter().cloned());
    }
    if plan.include_continuation {
        read_grants.extend(profile.spec.continuation_reads.iter().cloned());
    }

    let shared_writes: Vec<&PathGrant> = plan
        .write_roots
        .iter()
        .chain(&profile.spec.tool_writes)
        .collect();
    for grant in &read_grants {
        anchors.verify_grant_held(grant)?;
    }
    for executable in &profile.spec.process_executables {
        anchors.verify_executable(executable)?;
    }
    for grant in &credential_writes {
        anchors.verify_single_link_grant(grant)?;
    }
    verify_state_writes(plan, profile, anchors)?;
    if let AccessMode::WorktreeWrite(authority) = &plan.mode {
        verify_state_write(plan, &authority.cargo_cache, anchors)?;
        let cache = anchors
            .directories
            .iter()
            .find(|anchor| anchor.path == authority.cargo_cache.path)
            .ok_or(PermissionError::AnchorMismatch)?;
        verify_cache_metadata(
            &cache
                .directory
                .metadata()
                .map_err(PermissionError::Filesystem)?,
        )?;
    }
    if plan.scratch.scope != PathScope::Subtree {
        return Err(PermissionError::InvalidPath);
    }
    validate_noncredential_grant(&plan.scratch)?;
    anchors.verify_grant_held(&plan.scratch)?;
    let mut scratch_exclusions: Vec<&PathGrant> = read_grants
        .iter()
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
        validate_noncredential_grant(state)?;
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
    for read in &read_grants {
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

    let mut profile_bytes = render_profile(
        plan,
        profile,
        &read_grants,
        &credential_writes,
        worktree_grants.as_ref(),
        anchors.home,
    )?;
    if profile_bytes.len() > PROFILE_LIMIT {
        return Err(PermissionError::ProfileTooLarge);
    }
    let profile_digest = Sha256::digest(&profile_bytes).into();
    // SBPL grants pathnames, so rebind them to their held identities after
    // rendering, immediately before returning the launch profile.
    if let AccessMode::WorktreeWrite(authority) = &plan.mode {
        verify_worktree_authority(authority, anchors)?;
    }
    if let Some(grants) = &worktree_grants {
        verify_writable_tree(&grants.worktree.path, anchors)?;
        verify_writable_tree(&grants.objects.path, anchors)?;
    }
    verify_private_scratch(&plan.scratch, &scratch_exclusions, anchors)?;
    if let Some(state) = &plan.private_state {
        verify_private_scratch(state, &scratch_exclusions, anchors)?;
    }
    verify_state_writes(plan, profile, anchors)?;
    if let Some(read_only) = &plan.read_only {
        read_only.revalidate(anchors.safe_fs)?;
        for grant in &read_only.writes {
            verify_writable_tree(&grant.path, anchors)?;
        }
    }
    if let AccessMode::WorktreeWrite(authority) = &plan.mode {
        verify_state_write(plan, &authority.cargo_cache, anchors)?;
        let cache = anchors
            .directories
            .iter()
            .find(|anchor| anchor.path == authority.cargo_cache.path)
            .ok_or(PermissionError::AnchorMismatch)?;
        verify_cache_metadata(
            &cache
                .directory
                .metadata()
                .map_err(PermissionError::Filesystem)?,
        )?;
    }
    anchors.verify_all()?;
    // Avoid retaining a second mutable representation of the profile.
    profile_bytes.shrink_to_fit();
    Ok(ConfinedLaunch {
        enforcement: Enforcement::MacOsSandboxExec,
        profile: profile_bytes,
        profile_digest,
        native_controls: profile.spec.native_controls.clone(),
    })
}

/// Provider state writes stay outside every secret, protected and Git root, and
/// carry the same hard-link census as other writable trees.
#[cfg(target_os = "macos")]
fn verify_state_writes(
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
fn verify_state_write(
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
        PathScope::Subtree => verify_writable_tree(&grant.path, anchors)?,
        PathScope::Exact => anchors.verify_single_link_grant(grant)?,
    }
    Ok(())
}

/// Shared tool stores and dispatch write roots are deliberately not censused:
/// they can be large (a pnpm store), and the child cannot add hard links.
/// They still stay clear of every secret, protected, private and Git root.
#[cfg(target_os = "macos")]
fn verify_shared_writes(
    plan: &PlanPermissions,
    writes: &[&PathGrant],
    anchors: &Anchors<'_>,
) -> Result<(), PermissionError> {
    for grant in writes {
        validate_noncredential_grant(grant)?;
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

/// Credential and preservation paths no lane may read or write, whatever
/// else its profile grants. Rendered last so they override every allow.
#[cfg(target_os = "macos")]
fn hard_denied(home: &Path) -> [(PathBuf, PathScope); 8] {
    [
        (".ssh", PathScope::Subtree),
        (".aws", PathScope::Subtree),
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
fn verify_private_scratch(
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
        .ok_or(PermissionError::AnchorMismatch)?;
    anchor.verify(anchors.safe_fs)?;
    let metadata = anchor
        .directory
        .metadata()
        .map_err(PermissionError::Filesystem)?;
    if scratch.scope != PathScope::Subtree
        || metadata.uid != provenant_supervisor::fs::effective_uid()
        || metadata.mode & 0o077 != 0
    {
        return Err(PermissionError::AnchorMismatch);
    }
    verify_writable_tree(&scratch.path, anchors)?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn verify_writable_tree(path: &Path, anchors: &Anchors<'_>) -> Result<(), PermissionError> {
    let root = anchors
        .directories
        .iter()
        .find(|anchor| anchor.path == path)
        .ok_or(PermissionError::AnchorMismatch)?;
    root.verify(anchors.safe_fs)?;
    let identity = root
        .directory
        .metadata()
        .map_err(PermissionError::Filesystem)?;
    if identity.kind != FileKind::Directory {
        return Err(PermissionError::AnchorMismatch);
    }
    let mut entries = 0;
    inspect_writable_directory(root.directory.as_ref(), 0, &mut entries)?;
    let after = root
        .directory
        .metadata()
        .map_err(PermissionError::Filesystem)?;
    root.verify(anchors.safe_fs)?;
    if !crate::stable_read::same_metadata(&identity, &after) || after.kind != FileKind::Directory {
        return Err(PermissionError::AnchorMismatch);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn inspect_writable_directory(
    directory: &dyn SafeDir,
    depth: usize,
    count: &mut usize,
) -> Result<(), PermissionError> {
    if depth > MAX_WRITABLE_TREE_DEPTH {
        return Err(PermissionError::ProfileTooLarge);
    }
    for name in directory.entries().map_err(PermissionError::Filesystem)? {
        let name = name.map_err(PermissionError::Filesystem)?;
        *count += 1;
        if *count > MAX_WRITABLE_TREE_ENTRIES {
            return Err(PermissionError::ProfileTooLarge);
        }
        let before = directory
            .metadata_at(&name, false)
            .map_err(PermissionError::Filesystem)?;
        if before.kind == FileKind::Regular && before.links != 1 {
            return Err(PermissionError::HardLinkedWritableFile);
        }
        if before.kind == FileKind::Directory {
            let child = directory
                .open_verified_directory(&name)
                .map_err(PermissionError::Filesystem)?;
            let opened = child.metadata().map_err(PermissionError::Filesystem)?;
            if !crate::stable_read::same_metadata(&before, &opened) {
                return Err(PermissionError::AnchorMismatch);
            }
            inspect_writable_directory(child.as_ref(), depth + 1, count)?;
            let after = directory
                .metadata_at(&name, false)
                .map_err(PermissionError::Filesystem)?;
            if after.kind != FileKind::Directory
                || !crate::stable_read::same_metadata(&opened, &after)
            {
                return Err(PermissionError::AnchorMismatch);
            }
        } else {
            let after = directory
                .metadata_at(&name, false)
                .map_err(PermissionError::Filesystem)?;
            if !crate::stable_read::same_metadata(&before, &after) {
                return Err(PermissionError::AnchorMismatch);
            }
            if after.kind == FileKind::Regular && after.links != 1 {
                return Err(PermissionError::HardLinkedWritableFile);
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn render_profile(
    plan: &PlanPermissions,
    profile: &ProviderProfile,
    reads: &[PathGrant],
    credential_writes: &[PathGrant],
    worktree_grants: Option<&WorktreeWriteGrants>,
    home: &Path,
) -> Result<Vec<u8>, PermissionError> {
    let mut source = String::from("(version 1)\n(deny default)\n");
    // Apple dyld bootstrap rules are shared with the backend probe to keep
    // launch prerequisites and provider profiles aligned.
    source.push_str(DYLD_BOOTSTRAP_RULES);
    source.push_str(RUNTIME_BASELINE_RULES);
    for grant in reads {
        push_rule(&mut source, "allow", "file-read*", &grant.path, grant.scope)?;
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
        push_rule(
            &mut source,
            "allow",
            "process-exec",
            &root.path,
            PathScope::Subtree,
        )?;
    }
    if profile.spec.allow_process_fork {
        source.push_str("(allow process-fork)\n");
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

    for endpoint in &profile.spec.provider_api {
        push_network_endpoint(&mut source, endpoint)?;
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
        push_rule(&mut source, "deny", "file-read*", &path, scope)?;
        push_rule(&mut source, "deny", "file-write*", &path, scope)?;
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
    if source.len() > PROFILE_LIMIT {
        return Err(PermissionError::ProfileTooLarge);
    }
    Ok(source.into_bytes())
}

#[cfg(target_os = "macos")]
fn push_worktree_rule(
    source: &mut String,
    grants: &WorktreeWriteGrants,
) -> Result<(), PermissionError> {
    let worktree = sbpl_path(&grants.worktree.path)?;
    let git_dir = sbpl_path(&grants.private_git_dir.path)?;
    let common_dir = sbpl_path(&grants.common_git_dir.path)?;
    // Git in the lane reads its repository as read-only lanes do (#1061);
    // writes stay limited to the exact grants below.
    source.push_str(&format!(
        "(allow file-read* (subpath \"{git_dir}\") (subpath \"{common_dir}\"))\n"
    ));
    push_rule(
        source,
        "allow",
        "file-read*",
        &grants.common_exclude.path,
        PathScope::Exact,
    )?;
    source.push_str(&format!(
        "(allow file-write* (require-all (subpath \"{worktree}\") (require-not (subpath \"{git_dir}\")) (require-not (subpath \"{common_dir}\"))))\n"
    ));
    for grant in grants
        .index
        .iter()
        .chain(std::iter::once(&grants.packed_refs_lock))
        .chain(&grants.head)
        .chain(&grants.selected_ref)
    {
        push_rule(source, "allow", "file-write*", &grant.path, grant.scope)?;
    }
    // Anchored to the private git directory: one path component, digits only.
    let private = sbpl_regex_path(&grants.private_git_dir.path)?;
    source.push_str(&format!(
        "(allow file-write* (regex #\"^{private}/next-index-[0-9]+\\.lock$\"))\n"
    ));
    let path = sbpl_path(&grants.objects.path)?;
    source.push_str(&format!(
        "(allow file-write* (require-all (subpath \"{path}\") (require-not (subpath \"{path}/info\"))))\n"
    ));
    Ok(())
}

#[cfg(target_os = "macos")]
fn verify_worktree_authority(
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
    Ok(grants)
}

#[cfg(target_os = "macos")]
fn verify_selected_head(worktree: &VerifiedWorktree, branch: &str) -> Result<(), PermissionError> {
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
    if !opened.same_identity(&before) {
        return Err(PermissionError::AnchorMismatch);
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
        return Err(PermissionError::AnchorMismatch);
    }
    let expected = format!("ref: refs/heads/{branch}\n");
    if bytes.as_slice() != expected.as_bytes() {
        return Err(PermissionError::GitWriteAuthorityMissing);
    }
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
fn validate_profile_spec(spec: &ProviderProfileSpec) -> Result<(), PermissionError> {
    if spec.process_executables.len() > MAX_PATH_GRANTS
        || spec.runtime_reads.len() > MAX_PATH_GRANTS
        || spec.discovery_reads.len() > MAX_PATH_GRANTS
        || spec.authentication.len() > 16
        || spec.continuation_reads.len() > MAX_PATH_GRANTS
        || spec.state_writes.len() > MAX_PATH_GRANTS
        || spec.tool_exec_roots.len() > MAX_PATH_GRANTS
        || spec.tool_writes.len() > MAX_PATH_GRANTS
        || spec.provider_api.len() > MAX_NETWORK_GRANTS
        || spec.native_controls.len() > 64
    {
        return Err(PermissionError::InvalidFixture);
    }
    if spec.process_executables.is_empty() {
        return Err(PermissionError::ProcessPolicyMissing);
    }
    for executable in &spec.process_executables {
        validate_read_exec_grant(executable)?;
        if executable.scope != PathScope::Exact {
            return Err(PermissionError::InvalidPath);
        }
    }
    for grant in &spec.runtime_reads {
        validate_read_exec_grant(grant)?;
    }
    for grant in &spec.discovery_reads {
        grant.validate()?;
        if grant.scope != PathScope::Exact
            || !matches!(
                grant.path.file_name().and_then(OsStr::to_str),
                Some(".agents" | ".claude" | ".codex")
            )
        {
            return Err(PermissionError::InvalidPath);
        }
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
        if auth.roots.len() > MAX_PATH_GRANTS || auth.in_place_writes.len() > MAX_PATH_GRANTS {
            return Err(PermissionError::InvalidFixture);
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
        validate_noncredential_grant(grant)?;
    }
    for root in &spec.tool_exec_roots {
        validate_read_exec_grant(root)?;
        if root.scope != PathScope::Subtree {
            return Err(PermissionError::InvalidPath);
        }
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
            return Err(PermissionError::CredentialRoot);
        }
    }
    for grant in &spec.tool_writes {
        validate_noncredential_grant(grant)?;
    }
    for endpoint in &spec.provider_api {
        validate_endpoint(endpoint)?;
    }
    if spec.native_controls.iter().any(|control| {
        control.is_empty()
            || control.len() > 128
            || control
                .bytes()
                .any(|byte| !(byte.is_ascii_alphanumeric() || b"._-".contains(&byte)))
    }) {
        return Err(PermissionError::InvalidFixture);
    }
    Ok(())
}

pub(crate) fn validate_noncredential_grant(grant: &PathGrant) -> Result<(), PermissionError> {
    grant.validate()?;
    if is_credential_root(&grant.path) {
        return Err(PermissionError::CredentialRoot);
    }
    Ok(())
}

/// Validates a grant that confers only read and exec. It is the one place the
/// Codex standalone package exception applies: writes, scratch, protected
/// roots and private state keep the strict credential-root check.
pub(crate) fn validate_read_exec_grant(grant: &PathGrant) -> Result<(), PermissionError> {
    grant.validate()?;
    if is_codex_standalone_package(&grant.path) {
        return Ok(());
    }
    validate_noncredential_grant(grant)
}

/// Exactly `<home>/.codex/packages/standalone/releases/<release>` or a path
/// beneath it, where `<home>` is `/Users/<name>` or `/home/<name>`. Neither
/// `~/.codex`, `packages`, `standalone`, `releases` nor any sibling (auth,
/// config, sessions, state, logs) matches, and nothing credential-named may
/// appear below the release directory.
fn is_codex_standalone_package(path: &Path) -> bool {
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

fn validate_absolute(path: &Path) -> Result<(), PermissionError> {
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
        return Err(PermissionError::InvalidPath);
    }
    Ok(())
}

fn is_broad_root(path: &Path) -> bool {
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

fn is_credential_root(path: &Path) -> bool {
    let components: Vec<&OsStr> = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value),
            _ => None,
        })
        .collect();
    let single = [
        ".ssh", ".aws", ".azure", ".gnupg", ".kube", ".secrets", ".codex", ".claude", ".cursor",
        ".gemini", "Monero",
    ];
    if components
        .iter()
        .any(|component| single.iter().any(|name| *component == OsStr::new(name)))
    {
        return true;
    }
    components.windows(2).any(|pair| {
        (pair[0] == OsStr::new(".config")
            && (pair[1] == OsStr::new("gh") || pair[1] == OsStr::new("gcloud")))
            || (pair[0] == OsStr::new("Library") && pair[1] == OsStr::new("Keychains"))
            || (pair[0] == OsStr::new("Archives") && pair[1] == OsStr::new("git-preservation"))
    }) || components.windows(3).any(|triple| {
        triple[0] == OsStr::new("Library")
            && triple[1] == OsStr::new("Application Support")
            && triple[2] == OsStr::new("GitHub CLI")
    })
}

fn path_overlaps(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

#[cfg(any(target_os = "macos", test))]
fn validate_endpoint(endpoint: &NetworkEndpoint) -> Result<(), PermissionError> {
    if endpoint.port == 0
        || endpoint.port > u64::from(u16::MAX)
        || !matches!(endpoint.address.as_str(), "*" | "localhost")
    {
        return Err(PermissionError::InvalidFixture);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn push_network_endpoint(
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

#[cfg(target_os = "macos")]
fn push_rule(
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

/// Escapes a path for a raw SBPL `#"..."` regex literal.
#[cfg(target_os = "macos")]
fn sbpl_regex_path(path: &Path) -> Result<String, PermissionError> {
    let text = path.to_str().ok_or(PermissionError::NonUtf8Path)?;
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '"' => return Err(PermissionError::InvalidPath),
            value if value.is_control() => return Err(PermissionError::InvalidPath),
            '\\' | '.' | '^' | '$' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' => {
                escaped.push('\\');
                escaped.push(character);
            }
            value => escaped.push(value),
        }
    }
    Ok(escaped)
}

#[cfg(target_os = "macos")]
fn sbpl_path(path: &Path) -> Result<String, PermissionError> {
    let text = path.to_str().ok_or(PermissionError::NonUtf8Path)?;
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            value if value.is_control() => return Err(PermissionError::InvalidPath),
            value => escaped.push(value),
        }
    }
    Ok(escaped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_supervisor::exec::SystemExecutor;
    use provenant_supervisor::fs::LocalFs;

    #[test]
    fn launch_command_allows_only_the_attempts_own_mach_name() -> Result<(), PermissionError> {
        let launch = ConfinedLaunch {
            enforcement: Enforcement::MacOsSandboxExec,
            profile: b"(version 1)\n(deny default)\n".to_vec(),
            profile_digest: [0; 32],
            native_controls: Vec::new(),
        };
        let attempt = provenant_core::AttemptId::from_bytes([7; 16]);
        let command = launch.command(&["tool".into()], &attempt)?;
        assert_eq!(command[3], "tool");
        let profile = command[2].to_string_lossy();
        assert!(profile.starts_with("(version 1)\n(deny default)\n"));
        assert!(profile.ends_with(&format!(
            "(allow mach-lookup (global-name \"dev.provenant.attempt.{attempt}\"))\n"
        )));
        Ok(())
    }

    #[test]
    fn codex_standalone_package_is_read_exec_only() {
        let grant = |path: &str| PathGrant {
            path: PathBuf::from(path),
            scope: PathScope::Subtree,
        };
        let release =
            "/Users/alice/.codex/packages/standalone/releases/0.163.0-aarch64-apple-darwin";
        for path in [release, &format!("{release}/bin/codex")] {
            assert!(validate_read_exec_grant(&grant(path)).is_ok(), "{path}");
            assert!(
                matches!(
                    validate_noncredential_grant(&grant(path)),
                    Err(PermissionError::CredentialRoot)
                ),
                "writes stay strict: {path}"
            );
        }
        for path in [
            "/Users/alice/.codex",
            "/Users/alice/.codex/auth.json",
            "/Users/alice/.codex/config.toml",
            "/Users/alice/.codex/sessions",
            "/Users/alice/.codex/packages",
            "/Users/alice/.codex/packages/standalone",
            "/Users/alice/.codex/packages/standalone/releases",
            "/Users/alice/.codex/packages/standalone/current",
            "/Users/alice/.codex/packages/other/releases/1",
            "/Users/alice/.codex/packages/standalone/releases/1/.codex/auth.json",
            "/Users/alice/.claude/packages/standalone/releases/1",
            "/tmp/x/.codex/packages/standalone/releases/1",
        ] {
            assert!(
                matches!(
                    validate_read_exec_grant(&grant(path)),
                    Err(PermissionError::CredentialRoot | PermissionError::BroadRoot)
                ),
                "{path}"
            );
        }
    }

    #[test]
    fn broad_home_tmp_and_credential_roots_are_refused() {
        for path in [
            "/",
            "/Users/alice",
            "/home/alice",
            "/tmp",
            "/private",
            "/private/tmp",
            "/private/var",
            "/private/var/tmp",
            "/var",
            "/var/tmp",
        ] {
            let grant = PathGrant {
                path: PathBuf::from(path),
                scope: PathScope::Subtree,
            };
            assert!(grant.validate().is_err(), "{path}");
        }
        for path in [
            "/Users/alice/.ssh",
            "/Users/alice/.config/gh",
            "/Users/alice/.kube",
            "/Users/alice/.secrets/tokens",
            "/Users/alice/Monero",
            "/Users/alice/Archives/git-preservation/repo",
        ] {
            let grant = PathGrant {
                path: PathBuf::from(path),
                scope: PathScope::Subtree,
            };
            assert!(
                matches!(
                    validate_noncredential_grant(&grant),
                    Err(PermissionError::CredentialRoot)
                ),
                "{path}"
            );
            // A shared tool store under a credential root is refused too.
            let spec = ProviderProfileSpec {
                process_executables: vec![PathGrant {
                    path: PathBuf::from("/usr/bin/true"),
                    scope: PathScope::Exact,
                }],
                tool_writes: vec![grant],
                ..ProviderProfileSpec::default()
            };
            assert!(validate_profile_spec(&spec).is_err(), "{path}");
        }
        for path in [
            "/private/tmp/scoped",
            "/private/var/tmp/scoped",
            "/var/tmp/scoped",
        ] {
            let grant = PathGrant {
                path: PathBuf::from(path),
                scope: PathScope::Subtree,
            };
            assert!(grant.validate().is_ok(), "{path}");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn anchored_directory_rejects_noncanonical_tmp_aliases() {
        let temp = tempfile::tempdir_in("/tmp").expect("temporary anchor");
        let canonical = provenant_supervisor::fs::canonicalize(temp.path())
            .expect("canonical temporary anchor");
        assert_ne!(temp.path(), canonical);
        assert!(matches!(
            AnchoredDirectory::open_canonical(&LocalFs, temp.path()),
            Err(PermissionError::InvalidPath)
        ));
        assert!(AnchoredDirectory::open_canonical(&LocalFs, canonical).is_ok());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn anchored_directory_detects_replacement_at_its_sbpl_path() {
        let temp = tempfile::tempdir_in("/tmp").expect("temporary anchor root");
        let original = temp.path().join("root");
        std::fs::create_dir(&original).expect("anchor directory");
        let path =
            provenant_supervisor::fs::canonicalize(&original).expect("canonical anchor path");
        let safe_fs = LocalFs;
        let anchor = AnchoredDirectory::open_canonical(&safe_fs, &path).expect("open anchor");
        let moved = temp.path().join("moved");
        std::fs::rename(&original, &moved).expect("move held anchor");
        std::fs::create_dir(&original).expect("replacement directory");
        let directories = [anchor];
        let anchors = Anchors {
            home: Path::new("/fixture/home"),
            directories: &directories,
            safe_fs: &safe_fs,
            worktree: None,
        };
        assert!(matches!(
            anchors.verify_all(),
            Err(PermissionError::AnchorMismatch)
        ));
    }

    #[test]
    fn provider_profile_without_a_declared_executable_is_refused() {
        let spec = ProviderProfileSpec::default();
        assert!(matches!(
            validate_profile_spec(&spec),
            Err(PermissionError::ProcessPolicyMissing)
        ));
    }

    #[test]
    fn credential_writes_stay_inside_auth_roots_and_state_stays_outside() {
        let grant = |path: &str, scope| PathGrant {
            path: path.into(),
            scope,
        };
        let spec = |in_place: PathGrant, state: PathGrant| ProviderProfileSpec {
            process_executables: vec![grant("/opt/provider/bin/cli", PathScope::Exact)],
            authentication: vec![AuthenticationProfile {
                mechanism: "login".into(),
                roots: vec![grant(
                    "/Users/fixture/.provider/auth.json",
                    PathScope::Exact,
                )],
                in_place_writes: vec![in_place],
            }],
            state_writes: vec![state],
            ..ProviderProfileSpec::default()
        };
        let auth = grant("/Users/fixture/.provider/auth.json", PathScope::Exact);
        let transcript = grant("/Users/fixture/.provider/projects/p", PathScope::Subtree);
        assert!(validate_profile_spec(&spec(auth.clone(), transcript.clone())).is_ok());
        for in_place in [
            grant("/Users/fixture/.provider/other.json", PathScope::Exact),
            grant("/Users/fixture/.provider", PathScope::Subtree),
        ] {
            assert!(matches!(
                validate_profile_spec(&spec(in_place, transcript.clone())),
                Err(PermissionError::InvalidFixture)
            ));
        }
        assert!(matches!(
            validate_profile_spec(&spec(
                auth,
                grant("/Users/fixture/.provider", PathScope::Subtree)
            )),
            Err(PermissionError::CredentialRoot)
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn read_only_profile_reads_verified_git_and_only_writes_project_exceptions()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::plan::{
            CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest, ReadOnlyPolicy,
        };
        use crate::utility::{GitMutation, execute_git_mutation};
        use provenant_core::WorkScope;
        use std::os::unix::fs::PermissionsExt;
        let f = crate::test_support::Fixture::new()?;
        let branch = GitRefName::branch("read-only-profile")?;
        let linked = f._temp.path().join("linked");
        let outcome = execute_git_mutation(
            &f.executor,
            &f.repo,
            &f.runtime,
            GitMutation::WorktreeAdd {
                branch,
                base: Some(f.head()?),
                path: linked.clone(),
                create_branch: true,
            },
            f.budget()?,
            &CancelToken::new(),
        )?;
        assert_eq!(outcome.termination, Termination::Exit(0));
        let repo =
            f.runtime
                .open_read_repo(&f.executor, &linked, f.budget()?, &CancelToken::new())?;
        let root = repo.root_path();
        std::fs::create_dir_all(root.join(".agent-run/locks"))?;
        let scratch = provenant_supervisor::fs::canonicalize(f._temp.path())?.join("private-tmp");
        std::fs::create_dir(&scratch)?;
        std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(0o700))?;
        let policy: ReadOnlyPolicy =
            serde_json::from_str(r#"{"readOnlyWritable":[".agent-run/locks"]}"#)?;
        let binding = f.binding()?;
        let plan = ConfinedPlan::new(
            PlanRequest {
                scope: WorkScope::Project(binding.project_id()),
                permissions: PlanPermissions {
                    read_only: None,
                    mode: AccessMode::ReadOnly,
                    scratch: PathGrant {
                        path: scratch.clone(),
                        scope: PathScope::Subtree,
                    },
                    private_state: None,
                    read_roots: vec![],
                    protected_roots: vec![],
                    secret_roots: vec![],
                    selected_authentication: None,
                    include_continuation: false,
                    write_roots: vec![],
                    network: NetworkAccess::Provider,
                    nested_confinement: false,
                },
                codex: CodexPolicy::Clean,
                extensions: ExtensionPolicy::Deny,
                agy_state: None,
            }
            .with_read_only_repo(&repo, &binding, &policy)?,
        )?;
        use FixtureOperation as O;
        use FixtureOutcome::{Allowed, Denied};
        let home = provenant_supervisor::fs::canonicalize(&f.home)?;
        for directory in [
            home.join(".cargo/bin"),
            home.join(".cargo/registry"),
            home.join(".rustup/toolchains"),
            scratch.join("target"),
        ] {
            std::fs::create_dir_all(directory)?;
        }
        std::fs::write(
            home.join(".rustup/settings.toml"),
            "default_toolchain = 'fixture'",
        )?;
        std::fs::write(home.join(".cargo/credentials.toml"), "fixture-secret")?;
        let subtree = |path| PathGrant {
            path,
            scope: PathScope::Subtree,
        };
        let profile = ProviderProfile {
            spec: ProviderProfileSpec {
                process_executables: vec![PathGrant {
                    path: "/usr/bin/true".into(),
                    scope: PathScope::Exact,
                }],
                runtime_reads: vec![
                    subtree(home.join(".cargo/bin")),
                    subtree(home.join(".cargo/registry")),
                    subtree(home.join(".rustup/toolchains")),
                    PathGrant {
                        path: home.join(".rustup/settings.toml"),
                        scope: PathScope::Exact,
                    },
                ],
                tool_exec_roots: vec![
                    subtree(home.join(".cargo/bin")),
                    subtree(home.join(".rustup/toolchains")),
                    subtree(scratch.join("target")),
                ],
                ..Default::default()
            },
            observations: BTreeMap::from([
                (O::DeclaredRead, Allowed),
                (O::UndeclaredRead, Denied),
                (O::UndeclaredWrite, Denied),
                (O::SecretRead, Denied),
                (O::ProtectedWrite, Denied),
                (O::OtherNetwork, Denied),
                (O::DeclaredProcessExec, Allowed),
                (O::UndeclaredProcessExec, Denied),
                (O::ProcessFork, Denied),
                (O::RuntimeRead, Allowed),
            ]),
        };
        let paths = required_anchors(plan.permissions(), &profile, None);
        let directories = paths
            .iter()
            .map(|p| AnchoredDirectory::open_canonical(&LocalFs, p))
            .collect::<Result<Vec<_>, _>>()?;
        let anchors = Anchors {
            home: &home,
            directories: &directories,
            safe_fs: &LocalFs,
            worktree: None,
        };
        let compiled =
            MacOsSandboxBackend { ready: true }.compile(plan.permissions(), &profile, &anchors)?;
        let source = String::from_utf8(compiled.profile)?;
        let git = binding.common_dir();
        assert!(repo.0.private_git_dir().starts_with(git));
        assert!(source.contains(&format!(
            "(allow file-read* (subpath \"{}\"))",
            git.display()
        )));
        assert!(source.contains(&format!(
            "(deny file-write* (subpath \"{}\"))",
            git.display()
        )));
        assert!(!source.contains(&format!(
            "(allow file-write* (subpath \"{}\"))",
            git.display()
        )));
        assert!(source.contains(&format!(
            "(allow file-write* (subpath \"{}\"))",
            root.join(".agent-run/locks").display()
        )));
        for tool in [home.join(".cargo/bin"), home.join(".rustup/toolchains")] {
            assert!(source.contains(&format!(
                "(allow file-read* (subpath \"{}\"))",
                tool.display()
            )));
            assert!(source.contains(&format!(
                "(allow process-exec (subpath \"{}\"))",
                tool.display()
            )));
        }
        // Cargo credentials appear only as hard denies.
        assert!(
            source
                .lines()
                .filter(|line| line.contains("credentials"))
                .all(|line| line.starts_with("(deny "))
        );
        assert!(!source.contains(&format!(
            "(allow file-read* (subpath \"{}\"))",
            home.join(".cargo").display()
        )));
        assert!(source.contains(&format!(
            "(deny file-write* (require-all (subpath \"{}\") (regex #",
            root.join(".agent-run/locks").display()
        )));
        assert!(!source.contains("(deny file-write* (regex #"));
        assert!(source.contains(&crate::plan::read_only::exclusion_regex()));
        // A component swapped for a symlink after resolution cannot be compiled.
        std::fs::rename(root.join(".agent-run"), root.join("moved"))?;
        std::os::unix::fs::symlink(root.join("moved"), root.join(".agent-run"))?;
        assert!(
            MacOsSandboxBackend { ready: true }
                .compile(plan.permissions(), &profile, &anchors)
                .is_err()
        );
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn denied_probe_requires_an_operating_system_refusal() {
        let mut outcome = ExecOutcome {
            termination: Termination::Exit(44),
            stdout: Default::default(),
            stderr: provenant_supervisor::exec::CapturedStream {
                bytes: b"security: The specified item could not be found in the keychain.".to_vec(),
                ..Default::default()
            },
            timed_out: false,
            drain_truncated: false,
            overflow_stopped: false,
            cancelled: false,
            elapsed_micros: 0,
        };
        assert!(!denied_probe(&outcome));
        outcome.stderr.bytes =
            b"sandbox-exec: execvp() of '/usr/bin/security' failed: Operation not permitted"
                .to_vec();
        assert!(denied_probe(&outcome));
        outcome.termination = Termination::Exit(0);
        assert!(!denied_probe(&outcome));
        outcome.termination = Termination::Exit(1);
        outcome.cancelled = true;
        assert!(!denied_probe(&outcome));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn serialised_digest_cannot_authorise_without_a_verified_worktree() {
        let authority = WorktreeWriteAuthority {
            git_grants_digest: [0; 32],
            cargo_cache: PathGrant {
                path: "/fixture/home/Library/Caches/provenant-lane-cargo/fixture".into(),
                scope: PathScope::Subtree,
            },
        };
        let safe_fs = LocalFs;
        let directories = [];
        let anchors = Anchors {
            home: Path::new("/fixture/home"),
            directories: &directories,
            safe_fs: &safe_fs,
            worktree: None,
        };
        assert!(matches!(
            verify_worktree_authority(&authority, &anchors),
            Err(PermissionError::GitWriteAuthorityMissing)
        ));
    }

    #[test]
    fn writer_cache_is_private_and_bound_to_repository_and_worktree_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::repo::verify_worktree;
        use std::os::unix::fs::PermissionsExt;

        let fixture = crate::test_support::Fixture::new()?;
        let other = crate::test_support::Fixture::new()?;
        let selected_ref = GitRefName::branch("cache-test")?;
        let add = |fixture: &crate::test_support::Fixture, path: &Path, branch: &str| {
            fixture.run(&[
                "worktree",
                "add",
                "-b",
                branch,
                path.to_str().ok_or("path")?,
            ])?;
            Ok::<_, Box<dyn std::error::Error>>(verify_worktree(&fixture.repo, path)?)
        };
        let path = fixture._temp.path().join("package");
        let linked = add(&fixture, &path, "cache-test")?;
        let home = provenant_supervisor::fs::canonicalize(&fixture.home)?;
        let authority = WorktreeWriteAuthority::for_worktree(&linked, &selected_ref, &home)?;
        let cache = &authority.cargo_cache.path;
        assert_eq!(
            cache.parent(),
            Some(home.join("Library/Caches/provenant-lane-cargo").as_path())
        );
        assert_eq!(cache.file_name().ok_or("cache name")?.len(), 64);
        assert_eq!(
            authority,
            WorktreeWriteAuthority::for_worktree(&linked, &selected_ref, &home)?
        );

        let other_linked = add(&other, &other._temp.path().join("package"), "cache-test")?;
        assert_ne!(
            *cache,
            WorktreeWriteAuthority::for_worktree(&other_linked, &selected_ref, &home)?
                .cargo_cache
                .path
        );
        let sibling_path = fixture._temp.path().join("nested/package");
        std::fs::create_dir(sibling_path.parent().ok_or("parent")?)?;
        let sibling = add(&fixture, &sibling_path, "sibling")?;
        assert_ne!(
            *cache,
            WorktreeWriteAuthority::for_worktree(&sibling, &GitRefName::branch("sibling")?, &home)?
                .cargo_cache
                .path
        );

        prepare_writer_directories(&LocalFs, &linked, &selected_ref, &authority, &home)?;
        prepare_writer_directories(&LocalFs, &linked, &selected_ref, &authority, &home)?;
        let mut metadata = LocalFs.open_anchor(cache)?.metadata()?;
        verify_cache_metadata(&metadata)?;
        metadata.uid = metadata.uid.wrapping_add(1);
        assert!(matches!(
            verify_cache_metadata(&metadata),
            Err(PermissionError::AnchorMismatch)
        ));
        for mode in [0o755, 0o770, 0o1700] {
            std::fs::set_permissions(cache, std::fs::Permissions::from_mode(mode))?;
            assert!(
                matches!(
                    prepare_writer_directories(&LocalFs, &linked, &selected_ref, &authority, &home),
                    Err(PermissionError::AnchorMismatch)
                ),
                "mode {mode:o}"
            );
        }
        std::fs::set_permissions(cache, std::fs::Permissions::from_mode(0o700))?;
        let namespace = cache.parent().ok_or("namespace")?;
        std::fs::set_permissions(namespace, std::fs::Permissions::from_mode(0o755))?;
        assert!(matches!(
            prepare_writer_directories(&LocalFs, &linked, &selected_ref, &authority, &home),
            Err(PermissionError::AnchorMismatch)
        ));
        std::fs::set_permissions(namespace, std::fs::Permissions::from_mode(0o700))?;

        let moved = fixture._temp.path().join("retired");
        fixture.run(&[
            "worktree",
            "move",
            path.to_str().ok_or("path")?,
            moved.to_str().ok_or("path")?,
        ])?;
        let recreated = add(&fixture, &path, "recreated")?;
        assert_ne!(
            *cache,
            WorktreeWriteAuthority::for_worktree(
                &recreated,
                &GitRefName::branch("recreated")?,
                &home
            )?
            .cargo_cache
            .path
        );
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn linked_worktree_grants_derive_from_verified_git_paths_and_ref()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::repo::verify_worktree;
        use crate::utility::{GitMutation, execute_git_mutation};

        let fixture = crate::test_support::Fixture::new()?;
        let head = fixture.head()?;
        let selected_ref = GitRefName::branch("s3/authority")?;
        let worktree_path = fixture._temp.path().join("linked");
        let added = execute_git_mutation(
            &fixture.executor,
            &fixture.repo,
            &fixture.runtime,
            GitMutation::WorktreeAdd {
                branch: selected_ref.clone(),
                base: Some(head),
                path: worktree_path.clone(),
                create_branch: true,
            },
            fixture.budget()?,
            &CancelToken::new(),
        )?;
        assert_eq!(added.termination, Termination::Exit(0));
        let linked = verify_worktree(&fixture.repo, &worktree_path)?;
        let private = linked.repo().private_git_dir().to_owned();
        let common = linked.repo().common_git_dir().to_owned();
        assert_ne!(private, common);

        let authority = WorktreeWriteAuthority::for_worktree(
            &linked,
            &selected_ref,
            &provenant_supervisor::fs::canonicalize(&fixture.home)?,
        )?;
        fixture.run(&["pack-refs", "--all"])?;
        let missing_parent = common.join("refs/heads/s3");
        if missing_parent.exists() {
            std::fs::remove_dir(&missing_parent)?;
        }
        assert!(!missing_parent.exists());

        prepare_writer_directories(
            &LocalFs,
            &linked,
            &selected_ref,
            &authority,
            &provenant_supervisor::fs::canonicalize(&fixture.home)?,
        )?;
        let grants = WorktreeWriteGrants::derive(&linked, &selected_ref);
        assert_eq!(
            authority.git_grants_digest,
            grants.digest(&authority.cargo_cache)
        );
        assert_eq!(grants.objects.path, common.join("objects"));
        assert_eq!(grants.common_exclude.path, common.join("info/exclude"));
        assert!(
            grants
                .selected_ref
                .iter()
                .all(|grant| grant.path.starts_with(&common))
        );

        let common_heads = common.join("refs/heads/s3");
        let common_logs = common.join("logs/refs/heads/s3");
        let private_logs = private.join("logs");
        for directory in [&common_heads, &common_logs, &private_logs] {
            std::fs::create_dir_all(directory)?;
        }
        let anchor_paths = [
            linked.path().to_owned(),
            private.clone(),
            common.clone(),
            common.join("info"),
            common.join("objects"),
            authority.cargo_cache.path.clone(),
            private_logs.clone(),
            common_heads,
            common_logs,
        ];
        let safe_fs = LocalFs;
        let directories = anchor_paths
            .iter()
            .map(|path| AnchoredDirectory::open_canonical(&safe_fs, path))
            .collect::<Result<Vec<_>, _>>()?;
        let anchors = Anchors {
            home: &provenant_supervisor::fs::canonicalize(&fixture.home)?,
            directories: &directories,
            safe_fs: &safe_fs,
            worktree: Some((&linked, &selected_ref)),
        };
        assert!(missing_parent.is_dir());
        let wrong_home = Anchors {
            home: Path::new("/different/home"),
            ..anchors
        };
        assert!(matches!(
            verify_worktree_authority(&authority, &wrong_home),
            Err(PermissionError::GitWriteAuthorityMissing)
        ));
        let verified_grants = verify_worktree_authority(&authority, &anchors)?;
        let mut source = String::new();
        push_worktree_rule(&mut source, &verified_grants)?;
        assert!(source.contains("(require-not (subpath "));
        assert!(source.contains(&format!("{}/objects/info", common.display())));
        // `git commit` needs its message buffer, only in the private directory.
        assert!(source.contains(&format!("\"{}\"", private.join("COMMIT_EDITMSG").display())));
        assert!(!source.contains(&format!("\"{}\"", common.join("COMMIT_EDITMSG").display())));
        // A local `git merge` records its state only in the private directory.
        for name in [
            "MERGE_HEAD",
            "MERGE_MSG",
            "MERGE_MODE",
            "MERGE_RR",
            "ORIG_HEAD",
            "ORIG_HEAD.lock",
        ] {
            assert!(source.contains(&format!("\"{}\"", private.join(name).display())));
            assert!(!source.contains(&format!("\"{}\"", common.join(name).display())));
        }
        // Pathspec commits write `next-index-<pid>.lock`, only in the private directory.
        let private_pattern = sbpl_regex_path(&private)?;
        assert!(source.contains(&format!(
            "(regex #\"^{private_pattern}/next-index-[0-9]+\\.lock$\")"
        )));
        assert!(!source.contains(&format!("^{}/next-index", sbpl_regex_path(&common)?)));
        // Ref deletion takes the packed-refs lock; packed-refs itself stays read-only.
        assert!(source.contains(&format!(
            "\"{}\"",
            common.join("packed-refs.lock").display()
        )));
        for relative in [
            "config",
            "config.lock",
            "packed-refs",
            "refs/heads/main",
            "refs/remotes/origin",
            "refs/remotes/upstream",
        ] {
            assert!(
                !source.contains(&format!("\"{}\"", common.join(relative).display())),
                "{relative}"
            );
        }

        let other_ref = GitRefName::branch("s3/other")?;
        let mismatched = Anchors {
            home: &provenant_supervisor::fs::canonicalize(&fixture.home)?,
            directories: &directories,
            safe_fs: &safe_fs,
            worktree: Some((&linked, &other_ref)),
        };
        assert!(matches!(
            verify_worktree_authority(&authority, &mismatched),
            Err(PermissionError::GitWriteAuthorityMissing)
        ));

        let private_head_log = private_logs.join("HEAD");
        let hardlink_alias = fixture._temp.path().join("hardlinked-private-log");
        std::fs::write(&private_head_log, b"ref: refs/heads/s3/authority\n")?;
        std::fs::hard_link(&private_head_log, &hardlink_alias)?;
        assert!(matches!(
            verify_worktree_authority(&authority, &anchors),
            Err(PermissionError::AnchorMismatch)
        ));
        std::fs::remove_file(&hardlink_alias)?;
        std::fs::remove_file(&private_head_log)?;

        let mut tampered = authority.clone();
        tampered.git_grants_digest[0] ^= 1;
        assert!(matches!(
            verify_worktree_authority(&tampered, &anchors),
            Err(PermissionError::GitWriteAuthorityMissing)
        ));

        let original_head = std::fs::read(private.join("HEAD"))?;
        std::fs::write(private.join("HEAD"), b"ref: refs/heads/s3/other\n")?;
        assert!(matches!(
            verify_worktree_authority(
                &WorktreeWriteAuthority::for_worktree(
                    &linked,
                    &selected_ref,
                    &provenant_supervisor::fs::canonicalize(&fixture.home)?
                )?,
                &anchors
            ),
            Err(PermissionError::GitWriteAuthorityMissing)
        ));
        std::fs::write(private.join("HEAD"), original_head)?;
        verify_worktree_authority(&authority, &anchors)?;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn scratch_cannot_promote_git_or_read_authority_to_writes()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        let root = provenant_supervisor::fs::canonicalize(temp.path())?;
        let fs = LocalFs;
        let scratch_path = root.join("scratch");
        std::fs::create_dir(&scratch_path)?;
        std::fs::set_permissions(&scratch_path, std::fs::Permissions::from_mode(0o700))?;
        let scratch = PathGrant {
            path: scratch_path.clone(),
            scope: PathScope::Subtree,
        };
        let directories = [AnchoredDirectory::open_canonical(&fs, &scratch_path)?];
        let anchors = Anchors {
            home: Path::new("/fixture/home"),
            directories: &directories,
            safe_fs: &fs,
            worktree: None,
        };
        verify_private_scratch(&scratch, &[], &anchors)?;
        let common = PathGrant {
            path: root.clone(),
            scope: PathScope::Subtree,
        };
        assert!(matches!(
            verify_private_scratch(&scratch, &[&common], &anchors),
            Err(PermissionError::ProtectedOverlap)
        ));
        let read_root = PathGrant {
            path: scratch_path.join("read-only"),
            scope: PathScope::Subtree,
        };
        assert!(matches!(
            verify_private_scratch(&scratch, &[&read_root], &anchors),
            Err(PermissionError::ProtectedOverlap)
        ));
        std::fs::set_permissions(&scratch_path, std::fs::Permissions::from_mode(0o755))?;
        assert!(matches!(
            verify_private_scratch(&scratch, &[], &anchors),
            Err(PermissionError::AnchorMismatch)
        ));
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_sandbox_blocks_path_escape_probes() -> Result<(), Box<dyn std::error::Error>> {
        use crate::repo::{GitRefName, verify_worktree};
        use crate::utility::{GitMutation, execute_git_mutation};
        use std::os::unix::fs::symlink;

        let fixture = crate::test_support::Fixture::new()?;
        let root = provenant_supervisor::fs::canonicalize(fixture._temp.path())?;
        let home = root.join("home");
        let state = root.join("state");
        let xdg = root.join("xdg");
        let provider_state = root.join("provider-state");
        for path in [&home, &state, &xdg, &provider_state] {
            std::fs::create_dir_all(path)?;
        }
        let keychain = home.join("Library/Keychains/login.keychain-db");
        std::fs::create_dir_all(keychain.parent().ok_or("keychain parent")?)?;
        std::fs::write(&keychain, b"isolated fake credential")?;
        let head = fixture.head()?;
        let selected_ref = GitRefName::branch("s3/native")?;
        let worktree_path = root.join("linked");
        let added = execute_git_mutation(
            &fixture.executor,
            &fixture.repo,
            &fixture.runtime,
            GitMutation::WorktreeAdd {
                branch: selected_ref.clone(),
                base: Some(head),
                path: worktree_path.clone(),
                create_branch: true,
            },
            fixture.budget()?,
            &CancelToken::new(),
        )?;
        assert_eq!(added.termination, Termination::Exit(0));
        let linked = verify_worktree(&fixture.repo, &worktree_path)?;
        let authority = WorktreeWriteAuthority::for_worktree(
            &linked,
            &selected_ref,
            &provenant_supervisor::fs::canonicalize(&fixture.home)?,
        )?;
        prepare_writer_directories(
            &LocalFs,
            &linked,
            &selected_ref,
            &authority,
            &provenant_supervisor::fs::canonicalize(&fixture.home)?,
        )?;
        let grants = WorktreeWriteGrants::derive(&linked, &selected_ref);

        let scratch = root.join("scratch");
        std::fs::create_dir(&scratch)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(0o700))?;
        }
        let protected_parent = root.join("protected-parent");
        let protected = protected_parent.join("protected");
        std::fs::create_dir_all(&protected)?;
        let protected_file = protected.join("secret");
        std::fs::write(&protected_file, b"protected")?;
        let nested_git = linked.path().join("sub/.git");
        std::fs::create_dir_all(&nested_git)?;
        std::fs::write(nested_git.join("config"), b"nested")?;
        let inside = linked.path().join("inside");
        std::fs::write(&inside, b"")?;
        let symlink_path = linked.path().join("symlink");
        symlink(&protected_file, &symlink_path)?;
        // Shared tool stores and dispatch write roots are writable; Cargo
        // credentials stay hidden inside an otherwise readable Cargo home.
        let cargo = provenant_supervisor::fs::canonicalize(&fixture.home)?.join(".cargo");
        std::fs::create_dir_all(cargo.join("registry"))?;
        std::fs::create_dir_all(cargo.join("bin"))?;
        let cargo_credentials = cargo.join("credentials.toml");
        std::fs::write(&cargo_credentials, b"token")?;
        let project_locks = root.join("agent-run");
        std::fs::create_dir(&project_locks)?;
        let private = linked.repo().private_git_dir().to_owned();
        let common = linked.repo().common_git_dir().to_owned();
        let private_logs = private.join("logs");
        let common_heads = common.join("refs/heads/s3");
        let common_logs = common.join("logs/refs/heads/s3");
        for directory in [&private_logs, &common_heads, &common_logs] {
            std::fs::create_dir_all(directory)?;
        }
        let executables = [
            provenant_supervisor::fs::canonicalize(Path::new("/usr/bin/touch"))?,
            provenant_supervisor::fs::canonicalize(Path::new("/bin/mv"))?,
            provenant_supervisor::fs::canonicalize(Path::new("/bin/ln"))?,
            provenant_supervisor::fs::canonicalize(Path::new("/bin/sh"))?,
            provenant_supervisor::fs::canonicalize(Path::new("/bin/cat"))?,
            provenant_supervisor::fs::canonicalize(Path::new("/usr/sbin/sysctl"))?,
        ];

        let profile = ProviderProfile {
            spec: ProviderProfileSpec {
                state_writes: vec![PathGrant {
                    path: provider_state.clone(),
                    scope: PathScope::Subtree,
                }],
                deny_keychain_access: true,
                allow_process_fork: true,
                runtime_reads: vec![PathGrant {
                    path: cargo.clone(),
                    scope: PathScope::Subtree,
                }],
                tool_writes: vec![PathGrant {
                    path: cargo.join("registry"),
                    scope: PathScope::Subtree,
                }],
                tool_exec_roots: ["/bin", "/usr/bin", "/usr/libexec"]
                    .map(|path| PathGrant {
                        path: path.into(),
                        scope: PathScope::Subtree,
                    })
                    .to_vec(),
                process_executables: executables
                    .iter()
                    .cloned()
                    .map(|path| PathGrant {
                        path,
                        scope: PathScope::Exact,
                    })
                    .collect(),
                ..ProviderProfileSpec::default()
            },
            observations: [
                (FixtureOperation::DeclaredRead, FixtureOutcome::Allowed),
                (FixtureOperation::UndeclaredRead, FixtureOutcome::Denied),
                (FixtureOperation::UndeclaredWrite, FixtureOutcome::Denied),
                (FixtureOperation::SecretRead, FixtureOutcome::Denied),
                (FixtureOperation::ProtectedWrite, FixtureOutcome::Denied),
                (FixtureOperation::OtherNetwork, FixtureOutcome::Denied),
                (
                    FixtureOperation::DeclaredProcessExec,
                    FixtureOutcome::Allowed,
                ),
                (
                    FixtureOperation::UndeclaredProcessExec,
                    FixtureOutcome::Denied,
                ),
                (FixtureOperation::ProcessFork, FixtureOutcome::Allowed),
                (FixtureOperation::StateWrite, FixtureOutcome::Allowed),
                (FixtureOperation::RuntimeRead, FixtureOutcome::Allowed),
                (FixtureOperation::ShellExec, FixtureOutcome::Allowed),
                (FixtureOperation::KeychainTool, FixtureOutcome::Denied),
                (FixtureOperation::KeychainRead, FixtureOutcome::Denied),
                (
                    FixtureOperation::ProtectedInstructionRead,
                    FixtureOutcome::Allowed,
                ),
                (FixtureOperation::WorktreeWrite, FixtureOutcome::Allowed),
                (FixtureOperation::GitIndexWrite, FixtureOutcome::Allowed),
                (FixtureOperation::GitHeadWrite, FixtureOutcome::Allowed),
                (FixtureOperation::GitObjectsWrite, FixtureOutcome::Allowed),
                (FixtureOperation::SelectedRefWrite, FixtureOutcome::Allowed),
                (FixtureOperation::GitConfigWrite, FixtureOutcome::Denied),
                (FixtureOperation::OtherRefWrite, FixtureOutcome::Denied),
            ]
            .into(),
        };
        let cache = authority.cargo_cache.path.clone();
        let plan = PlanPermissions {
            read_only: None,
            private_state: None,
            mode: AccessMode::WorktreeWrite(Box::new(authority)),
            scratch: PathGrant {
                path: scratch.clone(),
                scope: PathScope::Subtree,
            },
            read_roots: Vec::new(),
            protected_roots: [protected.clone(), nested_git.clone()]
                .map(|path| PathGrant {
                    path,
                    scope: PathScope::Subtree,
                })
                .to_vec(),
            secret_roots: Vec::new(),
            selected_authentication: None,
            include_continuation: false,
            write_roots: vec![PathGrant {
                path: project_locks.clone(),
                scope: PathScope::Subtree,
            }],
            network: NetworkAccess::Provider,
            nested_confinement: false,
        };
        let safe_fs = LocalFs;
        let mut anchor_paths = vec![
            root.clone(),
            cargo.clone(),
            scratch.clone(),
            cache.clone(),
            provider_state.clone(),
            protected.clone(),
            nested_git.clone(),
            linked.path().to_owned(),
            private.clone(),
            common.clone(),
            common.join("info"),
            grants.objects.path.clone(),
            private_logs,
            common_heads,
            common_logs,
        ];
        let executable_directories = executables
            .iter()
            .filter_map(|path| path.parent().map(Path::to_owned))
            .collect::<BTreeSet<_>>();
        anchor_paths.extend(executable_directories);
        let directories = anchor_paths
            .iter()
            .map(|path| AnchoredDirectory::open_canonical(&safe_fs, path))
            .collect::<Result<Vec<_>, _>>()?;
        let anchors = Anchors {
            home: &provenant_supervisor::fs::canonicalize(&fixture.home)?,
            directories: &directories,
            safe_fs: &safe_fs,
            worktree: Some((&linked, &selected_ref)),
        };
        let backend = MacOsSandboxBackend { ready: true };
        let hardlink_path = linked.path().join("hardlink");
        for writable_path in [&grants.worktree.path, &grants.objects.path, &scratch] {
            let alias = writable_path.join("hardlink-probe");
            std::fs::hard_link(&protected_file, &alias)?;
            assert!(matches!(
                backend.compile(&plan, &profile, &anchors),
                Err(PermissionError::HardLinkedWritableFile)
            ));
            std::fs::remove_file(alias)?;
        }
        let compiled = backend.compile(&plan, &profile, &anchors)?;
        let sbpl = String::from_utf8(compiled.profile)?;
        assert!(sbpl.contains("(deny file-link)"));
        assert!(sbpl.contains("(allow sysctl-read)"));
        let executor = SystemExecutor::new(2)?;
        let cancel = CancelToken::new();
        let env = [
            ("HOME".into(), home.into_os_string()),
            ("PROVENANT_STATE_DIR".into(), state.into_os_string()),
            ("XDG_CONFIG_HOME".into(), xdg.into_os_string()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
        ];
        let (baseline, baseline_diagnostic) = probe_sandbox_with_diagnostic(&BackendContext {
            safe_fs: &LocalFs,
            executor: &fixture.executor,
            cancel: &CancelToken::new(),
            env: &env,
        })?;
        if !matches!(baseline, Availability::Available { .. }) {
            let diagnostic = baseline_diagnostic
                .as_deref()
                .unwrap_or("no command outcome was captured");
            assert_ne!(
                std::env::var("PROVENANT_CONFINEMENT_REQUIRED").as_deref(),
                Ok("1"),
                "the required native sandbox baseline is unavailable: {baseline:?}; {diagnostic}"
            );
            return Ok(());
        }
        let run = |program: &Path, args: &[&Path]| {
            let mut argv = vec![
                OsString::from(SANDBOX_EXEC),
                OsString::from("-p"),
                OsString::from(&sbpl),
                OsString::from(program.as_os_str()),
            ];
            argv.extend(args.iter().map(|path| path.as_os_str().to_owned()));
            let mut options = ExecOptions::new(&argv, Path::new("/"), Duration::from_secs(3));
            options.env = Some(&env);
            options.cancel = Some(&cancel);
            options.stdout = StreamPolicy::Capture { limit: 1024 };
            options.stderr = StreamPolicy::Capture { limit: 1024 };
            options.terminate_grace = CLEANUP_TIMEOUT;
            options.reap_timeout = CLEANUP_TIMEOUT;
            executor.execute(options).expect("bounded sandbox child")
        };

        let shell_outcome = run(&executables[3], &[Path::new("-c"), Path::new("printf ok")]);
        assert!(
            successful_probe(&shell_outcome),
            "shell: {:?}; {}",
            shell_outcome.termination,
            String::from_utf8_lossy(&shell_outcome.stderr.bytes)
        );
        let memory_outcome = run(&executables[5], &[Path::new("-n"), Path::new("hw.memsize")]);
        assert!(
            successful_probe(&memory_outcome)
                && String::from_utf8_lossy(&memory_outcome.stdout.bytes)
                    .trim()
                    .parse::<u64>()
                    .is_ok_and(|bytes| bytes > 0),
            "hw.memsize sysctl-read failed under the compiled provider profile: {:?}; {}",
            memory_outcome.termination,
            String::from_utf8_lossy(&memory_outcome.stderr.bytes)
        );
        assert!(successful_probe(&run(
            &executables[0],
            &[&provider_state.join("transcript")]
        )));
        let cache_outcome = run(&executables[0], &[&cache.join("artifact")]);
        assert!(
            successful_probe(&cache_outcome),
            "writer cache: {:?}",
            cache_outcome.termination
        );
        let inside_outcome = run(&executables[0], &[&inside]);
        assert!(
            successful_probe(&inside_outcome),
            "worktree write failed: {:?}; stderr={}",
            inside_outcome.termination,
            String::from_utf8_lossy(&inside_outcome.stderr.bytes)
        );
        // Package managers link into node_modules; a link grants nothing,
        // since the sandbox checks the resolved path.
        let new_link = linked.path().join("new-link");
        for (name, outcome) in [
            (
                "tool store write",
                run(&executables[0], &[&cargo.join("registry/crate")]),
            ),
            (
                "write root",
                run(&executables[0], &[&project_locks.join("lock")]),
            ),
            (
                "new symlink in worktree",
                run(
                    &executables[2],
                    &[Path::new("-s"), &protected_file, &new_link],
                ),
            ),
        ] {
            assert!(
                successful_probe(&outcome),
                "{name}: {:?}",
                outcome.termination
            );
        }
        let mut probes = vec![
            (
                "cargo credentials read",
                run(&executables[4], &[&cargo_credentials]),
            ),
            (
                "cargo bin write",
                run(&executables[0], &[&cargo.join("bin/cargo")]),
            ),
            (
                "write through new symlink",
                run(&executables[0], &[&new_link]),
            ),
            (
                "keychain tool",
                run(Path::new("/usr/bin/security"), &[Path::new("help")]),
            ),
            ("keychain read", run(&executables[4], &[&keychain])),
            (
                "sibling cache",
                run(&executables[0], &[&cache.with_file_name("other-lane")]),
            ),
            ("protected write", run(&executables[0], &[&protected_file])),
            ("symlink write", run(&executables[0], &[&symlink_path])),
            (
                "new hardlink",
                run(&executables[2], &[&protected_file, &hardlink_path]),
            ),
            (
                "nested git write",
                run(&executables[0], &[&nested_git.join("config")]),
            ),
        ];
        let moved_protected_parent = root.join("protected-parent-moved");
        probes.push((
            "protected ancestor rename",
            run(
                &executables[1],
                &[&protected_parent, &moved_protected_parent],
            ),
        ));
        let moved_worktree = root.join("linked-moved");
        probes.push((
            "worktree root rename",
            run(&executables[1], &[&worktree_path, &moved_worktree]),
        ));
        let roots_remained = protected_parent.exists() && worktree_path.exists();

        // Simulate a root swap in the gap between compiling the pathname
        // policy and starting its child: the authorised worktree spelling now
        // resolves to the protected directory.
        std::fs::rename(&worktree_path, &moved_worktree)?;
        symlink(&protected, &worktree_path)?;
        let swapped_probe = worktree_path.join("root-swap-probe");
        probes.push((
            "worktree root swap",
            run(&executables[0], &[&swapped_probe]),
        ));
        let root_swap_did_not_write = !protected.join("root-swap-probe").exists();
        std::fs::remove_file(&worktree_path)?;
        std::fs::rename(&moved_worktree, &worktree_path)?;
        assert!(
            roots_remained
                && root_swap_did_not_write
                && !hardlink_path.exists()
                && probes.iter().all(|(_, outcome)| denied_probe(outcome)),
            "native sandbox probes failed: {:?}; roots_remained={roots_remained}; root_swap_did_not_write={root_swap_did_not_write}; hardlink_absent={}",
            probes
                .iter()
                .filter(|(_, outcome)| !denied_probe(outcome))
                .map(|(name, outcome)| (*name, outcome.termination))
                .collect::<Vec<_>>(),
            !hardlink_path.exists(),
        );
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sbpl_process_authority_is_exact_and_fork_requires_profile_data() {
        let executable = PathGrant {
            path: PathBuf::from("/usr/bin/true"),
            scope: PathScope::Exact,
        };
        let mut spec = ProviderProfileSpec {
            process_executables: vec![executable],
            allow_process_fork: true,
            ..ProviderProfileSpec::default()
        };
        let profile = ProviderProfile {
            spec: spec.clone(),
            observations: BTreeMap::new(),
        };
        let plan = PlanPermissions {
            read_only: None,
            private_state: None,
            mode: AccessMode::ReadOnly,
            scratch: PathGrant {
                path: PathBuf::from("/tmp/attempt-scratch"),
                scope: PathScope::Subtree,
            },
            read_roots: Vec::new(),
            protected_roots: Vec::new(),
            secret_roots: Vec::new(),
            selected_authentication: None,
            include_continuation: false,
            write_roots: vec![],
            network: NetworkAccess::Provider,
            nested_confinement: false,
        };
        let rendered = String::from_utf8(
            render_profile(&plan, &profile, &[], &[], None, Path::new("/Users/fixture"))
                .expect("SBPL"),
        )
        .expect("UTF-8 SBPL");
        assert!(rendered.contains("(allow process-exec (literal \"/usr/bin/true\"))"));
        assert!(rendered.contains("(allow process-fork)"));
        assert!(!rendered.contains("localhost"));

        spec.allow_process_fork = false;
        spec.loopback_server = true;
        let profile = ProviderProfile {
            spec,
            observations: BTreeMap::new(),
        };
        let rendered = String::from_utf8(
            render_profile(&plan, &profile, &[], &[], None, Path::new("/Users/fixture"))
                .expect("SBPL"),
        )
        .expect("UTF-8 SBPL");
        assert!(!rendered.contains("(allow process-fork)"));
        // A private server binds and is reached on loopback only.
        assert!(rendered.contains("(allow network-bind (local ip \"localhost:*\"))"));
        assert!(rendered.contains("(allow network-outbound (remote ip \"localhost:*\"))"));
        assert!(!rendered.contains("\"*:*\""));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn network_levels_shared_writes_and_hard_denies_render_in_order() {
        let grant = |path: &str| PathGrant {
            path: PathBuf::from(path),
            scope: PathScope::Subtree,
        };
        let profile = ProviderProfile {
            spec: ProviderProfileSpec {
                process_executables: vec![PathGrant {
                    path: PathBuf::from("/usr/bin/true"),
                    scope: PathScope::Exact,
                }],
                tool_writes: vec![grant("/Users/fixture/.cargo/registry")],
                ..ProviderProfileSpec::default()
            },
            observations: BTreeMap::new(),
        };
        let mut plan = PlanPermissions {
            read_only: None,
            private_state: None,
            mode: AccessMode::ReadOnly,
            scratch: grant("/tmp/attempt-scratch"),
            read_roots: Vec::new(),
            protected_roots: Vec::new(),
            secret_roots: Vec::new(),
            selected_authentication: None,
            include_continuation: false,
            write_roots: vec![grant("/work/repo/.agent-run")],
            network: NetworkAccess::Provider,
            nested_confinement: false,
        };
        let render = |plan: &PlanPermissions| {
            let bytes = render_profile(plan, &profile, &[], &[], None, Path::new("/Users/fixture"))
                .expect("SBPL");
            String::from_utf8(bytes).expect("UTF-8 SBPL")
        };
        let provider = render(&plan);
        assert!(!provider.contains("localhost"));
        assert!(!provider.contains("PGSQL"));
        assert!(!provider.contains("\"*:*\""));
        let last_allow = ["/Users/fixture/.cargo/registry", "/work/repo/.agent-run"]
            .map(|path| {
                provider
                    .find(&format!("(allow file-write* (subpath \"{path}\"))"))
                    .expect(path)
            })
            .into_iter()
            .max()
            .expect("allow rules");
        for denied in [
            "(deny file-read* (subpath \"/Users/fixture/.ssh\"))",
            "(deny file-write* (subpath \"/Users/fixture/.kube\"))",
            "(deny file-read* (subpath \"/Users/fixture/Monero\"))",
            "(deny file-write* (subpath \"/Users/fixture/Archives/git-preservation\"))",
            "(deny file-read* (literal \"/Users/fixture/.cargo/credentials.toml\"))",
        ] {
            assert!(
                provider.find(denied).expect(denied) > last_allow,
                "{denied}"
            );
        }
        plan.network = NetworkAccess::Local;
        let local = render(&plan);
        assert!(local.contains("(allow network-outbound (remote ip \"localhost:*\"))"));
        assert!(local.contains("(allow network-bind (local ip \"localhost:*\"))"));
        assert!(local.contains("\\.s\\.PGSQL\\."));
        assert!(!local.contains("\"*:*\""));
        plan.network = NetworkAccess::Open;
        let open = render(&plan);
        assert!(open.contains("(allow network-outbound (remote ip \"*:*\"))"));
        assert!(open.contains("PGSQL"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_sandbox_probe_runs_through_executor_with_isolated_environment() {
        let temp = tempfile::tempdir_in("/tmp").expect("temporary isolated root");
        let root =
            provenant_supervisor::fs::canonicalize(temp.path()).expect("canonical isolated root");
        let home = root.join("home");
        let state = root.join("state");
        let xdg = root.join("xdg");
        let claude = root.join("claude");
        let codex = root.join("codex");
        for directory in [&home, &state, &xdg, &claude, &codex] {
            std::fs::create_dir_all(directory).expect("isolated process directory");
        }
        let env = [
            ("HOME".into(), home.into_os_string()),
            ("PROVENANT_STATE_DIR".into(), state.into_os_string()),
            ("XDG_CONFIG_HOME".into(), xdg.into_os_string()),
            (
                "GIT_CONFIG_GLOBAL".into(),
                root.join("gitconfig").into_os_string(),
            ),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("CLAUDE_CONFIG_DIR".into(), claude.into_os_string()),
            ("CODEX_HOME".into(), codex.into_os_string()),
        ];
        let safe_fs = LocalFs;
        let executor = SystemExecutor::new(2).expect("bounded utility executor");
        let cancel = CancelToken::new();
        let context = BackendContext {
            safe_fs: &safe_fs,
            executor: &executor,
            cancel: &cancel,
            env: &env,
        };
        match system_backend(&context) {
            Ok(_) | Err(PermissionError::BackendNotAvailable(_)) => {}
            Err(error) => panic!("sandbox probe did not fail closed: {error}"),
        }
    }

    #[test]
    fn linux_has_no_confinement_backend() {
        #[cfg(target_os = "linux")]
        {
            let safe_fs = LocalFs;
            let executor = SystemExecutor::new(1).expect("bounded executor");
            let cancel = CancelToken::new();
            let context = BackendContext {
                safe_fs: &safe_fs,
                executor: &executor,
                cancel: &cancel,
            };
            assert_eq!(
                system_backend(&context)
                    .err()
                    .map(|error| error.to_string()),
                Some(
                    PermissionError::BackendNotAvailable(AvailabilityReason::UnsupportedPlatform)
                        .to_string()
                )
            );
        }
    }

    #[test]
    fn network_endpoints_accept_only_sbpl_hosts_and_checked_ports() {
        for address in ["*", "localhost"] {
            assert!(
                validate_endpoint(&NetworkEndpoint {
                    address: address.into(),
                    port: 443,
                })
                .is_ok()
            );
        }
        for address in ["127.0.0.1", "::1", "example.test"] {
            assert!(
                validate_endpoint(&NetworkEndpoint {
                    address: address.into(),
                    port: 443,
                })
                .is_err()
            );
        }
        for port in [0, u64::from(u16::MAX) + 1] {
            assert!(
                validate_endpoint(&NetworkEndpoint {
                    address: "localhost".into(),
                    port,
                })
                .is_err()
            );
        }
    }

    #[test]
    fn lane_cache_sweep_removes_only_marked_caches_for_missing_worktrees()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let temporary = tempfile::tempdir()?;
        let home = temporary.path();
        let cache_root = home.join("Library/Caches/provenant-lane-cargo");
        std::fs::create_dir_all(&cache_root)?;
        let orphan = cache_root.join("a".repeat(64));
        let live = cache_root.join("b".repeat(64));
        let unknown = cache_root.join("c".repeat(64));
        let legacy = cache_root.join("d".repeat(64));
        for cache in [&orphan, &live, &unknown, &legacy] {
            std::fs::create_dir(cache)?;
            std::fs::set_permissions(cache, std::fs::Permissions::from_mode(0o700))?;
        }
        let existing_worktree = home.join("worktree");
        std::fs::create_dir(&existing_worktree)?;
        let old_worktree = home.join("removed-worktree");
        std::fs::create_dir(&old_worktree)?;
        let old_identity = std::fs::metadata(&old_worktree)?;
        std::fs::rename(&old_worktree, home.join("previous-worktree"))?;
        std::fs::create_dir(&old_worktree)?;
        for (cache, owner, identity) in [
            (&orphan, old_worktree, old_identity),
            (
                &live,
                existing_worktree.clone(),
                std::fs::metadata(&existing_worktree)?,
            ),
        ] {
            let mut marker = CARGO_CACHE_OWNER_PREFIX.to_vec();
            marker.extend_from_slice(&identity.dev().to_le_bytes());
            marker.extend_from_slice(&identity.ino().to_le_bytes());
            marker.extend_from_slice(owner.as_os_str().as_encoded_bytes());
            std::fs::write(cache.join(CARGO_CACHE_OWNER), marker)?;
        }
        let legacy_owner = home.join("legacy-worktree");
        std::fs::write(
            legacy.join(CARGO_CACHE_OWNER),
            [
                LEGACY_CARGO_CACHE_OWNER_PREFIX,
                legacy_owner.as_os_str().as_encoded_bytes(),
            ]
            .concat(),
        )?;
        std::fs::create_dir(orphan.join("release"))?;
        std::fs::write(orphan.join("release/provenant"), b"lane artifact")?;

        assert_eq!(prune_orphaned_worktree_cargo_caches(home)?, 1);
        assert!(!orphan.exists());
        assert!(live.is_dir());
        assert!(unknown.is_dir());
        assert!(legacy.is_dir());
        Ok(())
    }

    #[test]
    fn lane_cache_sweep_skips_deep_cache_and_continues() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir()?;
        let home = temporary.path();
        let cache_root = home.join("Library/Caches/provenant-lane-cargo");
        std::fs::create_dir_all(&cache_root)?;
        let deep = cache_root.join("a".repeat(64));
        let ordinary = cache_root.join("b".repeat(64));
        for cache in [&deep, &ordinary] {
            std::fs::create_dir(cache)?;
            std::fs::set_permissions(cache, std::fs::Permissions::from_mode(0o700))?;
        }
        let missing_owner = home.join("missing-worktree");
        let mut marker = CARGO_CACHE_OWNER_PREFIX.to_vec();
        marker.extend_from_slice(&1_u64.to_le_bytes());
        marker.extend_from_slice(&2_u64.to_le_bytes());
        marker.extend_from_slice(missing_owner.as_os_str().as_encoded_bytes());
        for cache in [&deep, &ordinary] {
            std::fs::write(cache.join(CARGO_CACHE_OWNER), &marker)?;
        }
        let mut nested = deep.clone();
        for _ in 0..MAX_CACHE_SWEEP_DEPTH + 1 {
            nested.push("nested");
            std::fs::create_dir(&nested)?;
        }

        let error = prune_orphaned_worktree_cargo_caches(home)
            .expect_err("deep cache is skipped and reported after the sweep");
        assert!(error.to_string().contains(&"a".repeat(64)));
        assert!(deep.is_dir());
        assert!(!ordinary.exists());
        Ok(())
    }
}
