//! Typed permission plans and the fail-closed macOS SBPL backend.
//!
//! Provider-owned profile data bounds every requested permission.
//! Linux deliberately exposes an unavailable backend.
use crate::cache::{CARGO_CACHE_OWNER, CARGO_CACHE_OWNER_PREFIX, verify_cache_metadata};
use crate::repo::{GitRefName, VerifiedWorktree};
use provenant_core::CancelToken;
#[cfg(target_os = "macos")]
use provenant_os::exec::{ExecOptions, ExecOutcome, StreamPolicy, Termination};
#[cfg(target_os = "macos")]
use provenant_os::fs::Metadata;
use provenant_os::{
    exec::Executor,
    fs::{Directory, FileKind, FsError, PublishMode},
};
pub use provenant_records::run::{NetworkAccess, PathScope, WriteSource};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::ffi::OsString;
#[cfg(target_os = "macos")]
use std::io::Read;
use std::path::{Component, Path, PathBuf};
#[cfg(target_os = "macos")]
use std::time::Duration;
use thiserror::Error;

const MAX_PATH_BYTES: usize = 4096;
const MAX_PATH_GRANTS: usize = 128;
/// Bound each of the eight recorded write sources by the profile path limit.
pub const MAX_WRITE_GRANTS: usize = MAX_PATH_GRANTS * 8;
#[cfg(any(target_os = "macos", test))]
const MAX_NETWORK_GRANTS: usize = 128;
#[cfg(target_os = "macos")]
const MAX_GIT_HEAD_BYTES: u64 = 1024;
#[cfg(target_os = "macos")]
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
#[cfg(any(target_os = "macos", feature = "snapshot"))]
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
/// kernel and preference reads, TLS trust, DNS, time zone data, and process
/// information and signals within the same sandbox. It names no user path; explicit
/// secret-root denials still override the metadata grant.
#[cfg(any(target_os = "macos", feature = "snapshot"))]
const RUNTIME_BASELINE_RULES: &str = r#"(allow sysctl-read)
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
#[cfg(target_os = "macos")]
const PROBE_PROCESS_RULES: &str = "(allow process-exec (literal \"/usr/bin/true\"))\n(allow process-exec (literal \"/bin/cat\"))\n";
/// Localhost services a build or test run talks to: any loopback TCP port,
/// its own listeners, and a local Postgres server's Unix socket.
#[cfg(any(target_os = "macos", feature = "snapshot"))]
const LOCAL_SERVICE_RULES: &str = r##"(allow network-bind (local ip "localhost:*"))
(allow network-inbound (local ip "localhost:*"))
(allow network-outbound (remote ip "localhost:*"))
(allow network-outbound (remote unix-socket (path-regex #"^/private/tmp/\.s\.PGSQL\.[0-9]+$")))
"##;
/// `openpty` through the multiplexer. The kernel issues the pty extension only
/// for a terminal this sandbox allocates, so the user's own `ttys` stay denied.
#[cfg(any(target_os = "macos", feature = "snapshot"))]
const PSEUDO_TTY_RULES: &str = r##"(allow pseudo-tty)
(allow file-read* file-write* file-ioctl (literal "/dev/ptmx"))
(allow file-read* file-write* file-ioctl (require-all (regex #"^/dev/ttys[0-9]+$") (extension "com.apple.sandbox.pty")))
"##;
/// Profiles travel in argv: leave headroom below macOS's 1 MiB ARG_MAX for
/// provider arguments and environment.
#[cfg(any(target_os = "macos", feature = "snapshot"))]
const PROFILE_LIMIT: usize = 512 * 1024;
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
    #[error("path is not a permitted absolute, normalised path: {0:?}")]
    InvalidPath(PathBuf),
    #[error("permission root is too broad")]
    BroadRoot,
    #[error("credential root requires a selected fixture-backed mechanism")]
    CredentialRoot,
    #[error("permission root overlaps a protected path")]
    ProtectedOverlap,
    #[error("required held path anchor is missing or changed")]
    AnchorMismatch,
    #[error("held path check failed at {path:?}: {stage}")]
    AnchorMismatchAt { path: PathBuf, stage: &'static str },
    #[error(
        "Git control file has other hard links: {0:?}; a lane could write it through the other name. Find the other name with `find . -samefile {0:?}` from the repository root, remove it, then retry"
    )]
    GitControlHardLinked(PathBuf),
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
    #[error(
        "compiled profile is {bytes} bytes, over the {limit}-byte launch limit (profiles travel in argv); reduce the read, write or executable roots this lane is granted"
    )]
    ProfileTooLarge { bytes: usize, limit: usize },
    #[error("{family} exceeds its bound of {limit}: {path:?}")]
    BoundExceeded {
        family: &'static str,
        limit: usize,
        path: PathBuf,
    },
    #[error("sandbox backend is unavailable on this platform")]
    BackendUnavailable,
    #[error("sandbox backend probe reported unavailable: {0:?}")]
    BackendNotAvailable(AvailabilityReason),
    #[error("sandbox probe was cancelled")]
    Cancelled,
    #[error("safe filesystem operation failed: {0}")]
    Filesystem(#[source] provenant_os::fs::FsError),
    #[error("sandbox probe execution failed")]
    Execution(#[source] provenant_os::exec::ExecError),
    #[error("verified repository authority could not be revalidated")]
    Repository(#[from] crate::repo::RepoError),
    #[error("bounded Git HEAD read failed")]
    HeadRead(#[source] std::io::Error),
    #[error("lane cache sweep could not complete: {0}")]
    CacheSweepIo(#[source] std::io::Error),
    #[error("lane cache {operation} failed at {path:?}: {source}")]
    CacheSweepFilesystem {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: FsError,
    },
    #[error("lane cache contents exceed the sweep depth bound")]
    CacheSweepDepth,
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
    /// Host-global SysV IPC for explicitly declared database lanes only.
    pub allow_database_ipc: bool,
    /// Deny keychain IPC and its CLI even when a tool can execute system programs.
    pub deny_keychain_access: bool,
    /// Deny all known credential stores, including networked GitHub access.
    pub deny_credential_access: bool,
    /// An offline adapter denies even the runtime's DNS socket exception.
    pub deny_network: bool,
    /// The CLI runs its tools' commands on a pseudo-terminal: open `/dev/ptmx`
    /// and only the terminals this sandbox allocates, never another `ttys`.
    pub allow_pseudo_tty: bool,
    pub runtime_reads: Vec<PathGrant>,
    /// Exact attempt-bound host Fabric mutation endpoint.
    pub fabric_socket: Option<PathGrant>,
    /// Literal directory discovery; never the contents of provider config.
    pub discovery_reads: Vec<PathGrant>,
    pub authentication: Vec<AuthenticationProfile>,
    pub continuation_reads: Vec<PathGrant>,
    /// Provider-owned state the CLI must write outside scratch, such as one
    /// session transcript. It never overlaps an authentication root.
    pub state_writes: Vec<PathGrant>,
    /// Paths whose executables the provider's own tools may run, for
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
/// The common directory also binds provider setup to the verified repository.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorktreeWriteAuthority {
    pub git_grants_digest: [u8; 32],
    cargo_cache: PathGrant,
    publishing: bool,
    /// Older records remain readable, but cannot launch without a verified binding.
    #[serde(default)]
    common_git_dir: Option<PathBuf>,
}

impl WorktreeWriteAuthority {
    /// Repository selected by the host, for the lane-private object alternate.
    pub fn common_git_dir(&self) -> Option<&Path> {
        self.common_git_dir.as_deref()
    }

    pub fn set_publishing(&mut self, enabled: bool) {
        self.publishing = enabled;
    }

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
            publishing: true,
            common_git_dir: Some(worktree.repo().common_git_dir().to_path_buf()),
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
    let name = provenant_core::hex(&digest.finalize());
    let grant = PathGrant {
        path: home.join("Library/Caches/provenant-lane-cargo").join(name),
        scope: PathScope::Subtree,
    };
    validate_noncredential_grant(&grant)?;
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
                    directory
                        .mkdir(name, 0o700)
                        .map_err(PermissionError::Filesystem)?;
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

/// Host Git boundaries bound into writer authority. All remain read-only;
/// the lane owns its complete Git directory under attempt scratch.
pub(crate) struct WorktreeWriteGrants {
    worktree: PathGrant,
    private_git_dir: PathGrant,
    common_git_dir: PathGrant,
    branch: String,
}

impl WorktreeWriteGrants {
    fn derive(worktree: &VerifiedWorktree, selected_ref: &GitRefName) -> Self {
        let repo = worktree.repo();
        Self::from_paths(
            worktree.path(),
            repo.private_git_dir(),
            repo.common_git_dir(),
            selected_ref.short_name(),
        )
    }

    fn from_paths(worktree: &Path, private: &Path, common: &Path, branch: &str) -> Self {
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

    fn iter(&self) -> impl Iterator<Item = &PathGrant> {
        [&self.worktree, &self.private_git_dir, &self.common_git_dir].into_iter()
    }

    fn digest(&self, cache: &PathGrant) -> [u8; 32] {
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

impl PlanPermissions {
    /// Publishing credentials require both writer authority and open network.
    pub fn writer_publishing(&self) -> bool {
        matches!(&self.mode, AccessMode::WorktreeWrite(authority) if authority.publishing)
            && self.network == NetworkAccess::Open
    }
}

/// An explicit write allowance, subject to the rendered profile's deny rules.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteGrant {
    pub source: WriteSource,
    pub path: PathBuf,
    pub scope: PathScope,
}

/// List explicit write allowances in rendering order. Baseline `/dev/null`
/// access and concrete worktree/Git paths are omitted; writer records carry
/// the existing Git grants digest instead. This list grants no authority.
pub fn write_grants(plan: &PlanPermissions, profile: &ProviderProfile) -> Vec<WriteGrant> {
    let mut grants = Vec::new();
    let mut push = |source, grant: &PathGrant, scope| {
        grants.push(WriteGrant {
            source,
            path: grant.path.clone(),
            scope,
        });
    };
    for grant in &profile.spec.state_writes {
        push(WriteSource::StateWrite, grant, grant.scope);
    }
    if let Some(selected) = plan.selected_authentication.as_ref().and_then(|mechanism| {
        profile
            .spec
            .authentication
            .iter()
            .find(|auth| &auth.mechanism == mechanism)
    }) {
        for grant in &selected.in_place_writes {
            push(WriteSource::CredentialWrite, grant, PathScope::Exact);
        }
    }
    for grant in &plan.write_roots {
        push(WriteSource::WriteRoot, grant, grant.scope);
    }
    for grant in &profile.spec.tool_writes {
        push(WriteSource::ToolWrite, grant, grant.scope);
    }
    push(WriteSource::Scratch, &plan.scratch, PathScope::Subtree);
    if let Some(grant) = &plan.private_state {
        push(WriteSource::PrivateState, grant, PathScope::Subtree);
    }
    if let Some(read_only) = &plan.read_only {
        for grant in &read_only.writes {
            push(WriteSource::ReadOnlyException, grant, grant.scope);
        }
    }
    if let AccessMode::WorktreeWrite(authority) = &plan.mode {
        push(
            WriteSource::LaneCache,
            &authority.cargo_cache,
            PathScope::Subtree,
        );
    }
    grants
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

fn anchor_at(path: &Path, stage: &'static str) -> PermissionError {
    PermissionError::AnchorMismatchAt {
        path: path.to_owned(),
        stage,
    }
}

pub struct AnchoredDirectory {
    path: PathBuf,
    #[cfg(target_os = "macos")]
    directory: Directory,
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
    fn verify(&self) -> Result<(), PermissionError> {
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
    fn verify_all(&self) -> Result<(), PermissionError> {
        for anchor in self.directories {
            anchor.verify()?;
        }
        Ok(())
    }

    fn verify_discovery_grant(&self, grant: &PathGrant) -> Result<(), PermissionError> {
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

    fn verify_grant_held(&self, grant: &PathGrant) -> Result<(), PermissionError> {
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

    fn verify_executable(&self, grant: &PathGrant) -> Result<(), PermissionError> {
        if grant.scope != PathScope::Exact {
            return Err(PermissionError::InvalidPath(grant.path.clone()));
        }
        match self.exact_grant_metadata(&grant.path)? {
            Some(_) => Ok(()),
            None => Err(anchor_at(&grant.path, "executable is missing")),
        }
    }

    fn verify_single_link_grant(&self, grant: &PathGrant) -> Result<(), PermissionError> {
        if grant.scope != PathScope::Exact {
            return Err(PermissionError::InvalidPath(grant.path.clone()));
        }
        match self.exact_grant_metadata(&grant.path)? {
            Some(metadata) if metadata.links == 1 => Ok(()),
            Some(_) => Err(anchor_at(&grant.path, "file has other hard links")),
            None => Ok(()),
        }
    }

    fn exact_grant_metadata(&self, path: &Path) -> Result<Option<Metadata>, PermissionError> {
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
            // A file in a directory that does not exist yet needs no anchor.
            PathScope::Exact => grant
                .path
                .parent()
                .filter(|parent| std::fs::symlink_metadata(parent).is_ok())
                .map(Path::to_owned),
        })
        .collect()
}

pub struct BackendContext<'a> {
    pub executor: &'a dyn Executor,
    pub cancel: &'a CancelToken,
    #[cfg(target_os = "macos")]
    pub env: &'a [(OsString, OsString)],
}

pub trait ConfinementBackend: Send + Sync {
    fn probe(&self, context: &BackendContext<'_>) -> Result<Availability, PermissionError>;

    /// Compile immediately before launch while holding writer exclusion over
    /// every writable root through the child handoff. The pathname sandbox
    /// denies child-created hard links; compile inspects only the Git control
    /// files for other names, and cannot stop an external writer from adding
    /// an alias to one after that check.
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
    let executable = match provenant_os::fs::FileHandle::open_source(Path::new(SANDBOX_EXEC)) {
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
        Err(provenant_os::exec::ExecError::Cancelled) => {
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
        Err(provenant_os::exec::ExecError::Cancelled) => Err(PermissionError::Cancelled),
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
    for (family, count) in [
        ("read root grant count", plan.read_roots.len()),
        ("protected root grant count", plan.protected_roots.len()),
        ("secret root grant count", plan.secret_roots.len()),
        ("write root grant count", plan.write_roots.len()),
    ] {
        if count > MAX_PATH_GRANTS {
            return Err(PermissionError::BoundExceeded {
                family,
                limit: MAX_PATH_GRANTS,
                path: plan.scratch.path.clone(),
            });
        }
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
    if let Some(read_only) = &plan.read_only {
        if !matches!(plan.mode, AccessMode::ReadOnly) {
            return Err(PermissionError::InvalidReadOnlyPolicy);
        }
        read_only.revalidate()?;
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
            anchors.verify_grant_held(grant)?;
        }
    }
    let grants = GrantSet::build(plan, profile, anchors.home, worktree_grants)?;
    let read_grants = &grants.reads;
    let all_reads = || read_grants.iter().chain(&profile.spec.discovery_reads);
    let credential_writes = &grants.credential_writes;
    let worktree_grants = grants.worktree.as_ref();

    let shared_writes: Vec<&PathGrant> = plan
        .write_roots
        .iter()
        .chain(&profile.spec.tool_writes)
        .collect();
    // The path checks above know only conventional homes; the host's own home
    // is broad and its nested credential roots stay hidden wherever it lives.
    if all_reads()
        .chain(shared_writes.iter().copied())
        .chain(&profile.spec.tool_exec_roots)
        .chain(&profile.spec.state_writes)
        .any(|grant| encloses_home_credentials(grant, anchors.home))
    {
        return Err(PermissionError::CredentialRoot);
    }
    for grant in read_grants {
        anchors.verify_grant_held(grant)?;
    }
    for grant in &profile.spec.discovery_reads {
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
    validate_noncredential_grant(&plan.scratch)?;
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

    let mut profile_bytes = render(&grants)?.into_bytes();
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
    for grant in &profile.spec.discovery_reads {
        anchors.verify_discovery_grant(grant)?;
    }
    // Avoid retaining a second mutable representation of the profile.
    profile_bytes.shrink_to_fit();
    Ok(ConfinedLaunch {
        enforcement: Enforcement::MacOsSandboxExec,
        profile: profile_bytes,
        profile_digest,
        native_controls: profile.spec.native_controls.clone(),
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
    reads: Vec<PathGrant>,
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
#[cfg(any(target_os = "macos", feature = "snapshot"))]
pub(crate) fn render(grants: &GrantSet<'_>) -> Result<String, PermissionError> {
    let source = String::from_utf8(render_profile(
        grants.plan,
        grants.profile,
        &grants.reads,
        &grants.credential_writes,
        grants.worktree.as_ref(),
        grants.home,
    )?)
    .map_err(|_| PermissionError::NonUtf8Path)?;
    Ok(source)
}

/// Provider state writes stay outside every secret, protected and Git root.
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
        PathScope::Subtree => anchors.verify_grant_held(grant)?,
        PathScope::Exact => anchors.verify_single_link_grant(grant)?,
    }
    Ok(())
}

/// Shared tool stores and dispatch write roots are not inspected beyond this:
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

/// GitHub CLI configuration roots that networked read-only lanes may read.
#[cfg(any(target_os = "macos", feature = "snapshot"))]
fn gh_config_paths(home: &Path) -> Vec<PathBuf> {
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
fn hard_denied(home: &Path) -> [(PathBuf, PathScope); 12] {
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

#[cfg(any(target_os = "macos", feature = "snapshot"))]
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
    if profile.spec.allow_database_ipc {
        source.push_str("(allow ipc-sysv-shm)\n(allow ipc-sysv-sem)\n");
    }
    for grant in reads.iter().chain(&profile.spec.discovery_reads) {
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
    if profile.spec.deny_network {
        source.push_str("(deny network*)\n");
    }

    // The gh read comes before the secret-root denies: Seatbelt is
    // last-match-wins, so an overlapping secret root must still hide it.
    let gh_paths = if !profile.spec.deny_credential_access
        && ((matches!(plan.mode, AccessMode::ReadOnly) && plan.network == NetworkAccess::Open)
            || plan.writer_publishing())
    {
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
    if source.len() > PROFILE_LIMIT {
        return Err(PermissionError::ProfileTooLarge {
            bytes: source.len(),
            limit: PROFILE_LIMIT,
        });
    }
    Ok(source.into_bytes())
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
fn push_worktree_rule(
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
fn verify_git_control_links(repo: &crate::repo::VerifiedRepo) -> Result<(), PermissionError> {
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
                return Err(PermissionError::BoundExceeded {
                    family: "Git control directory entry count",
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
            return Err(PermissionError::InvalidPath(executable.path.clone()));
        }
    }
    if let Some(socket) = &spec.fabric_socket {
        validate_noncredential_grant(socket)?;
        if socket.scope != PathScope::Exact {
            return Err(PermissionError::InvalidPath(socket.path.clone()));
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
            return Err(PermissionError::InvalidPath(grant.path.clone()));
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
fn refuses_credential_grant(grant: &PathGrant, read: bool) -> bool {
    if read && is_codex_standalone_package(&grant.path) && !encloses_credential_root(grant) {
        return false;
    }
    is_credential_root(&grant.path) || encloses_credential_root(grant)
}

/// A subtree of `/Users/<name>` or `/home/<name>` that holds a nested
/// credential root (`~/.config`, `~/Library`, `~/Archives`) would expose it.
/// The home itself is a broad root; [`refuses_lane_root`] and `compile_sbpl`
/// repeat this check against the host's actual home, wherever it lives.
fn encloses_credential_root(grant: &PathGrant) -> bool {
    grant.scope == PathScope::Subtree && encloses_credential_root_path(&grant.path)
}

fn encloses_credential_root_path(path: &Path) -> bool {
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
fn encloses_home_credentials(grant: &PathGrant, home: &Path) -> bool {
    grant.scope == PathScope::Subtree && encloses_home_credentials_path(&grant.path, home)
}

fn encloses_home_credentials_path(path: &Path, home: &Path) -> bool {
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

const CREDENTIAL_DIRECTORIES: &[&str] = &[
    ".ssh", ".aws", ".azure", ".gnupg", ".kube", ".secrets", ".codex", ".claude", ".cursor",
    ".gemini", "Monero",
];
const NESTED_CREDENTIAL_DIRECTORIES: &[&str] = &[
    ".config/gh",
    ".config/gcloud",
    "Library/Keychains",
    "Archives/git-preservation",
    "Library/Application Support/GitHub CLI",
];

// Additional provider stores can contain credentials alongside their state.
// Keep these command-only denials out of model profile validation.
#[cfg(any(target_os = "macos", feature = "snapshot"))]
const PROVIDER_CREDENTIAL_DIRECTORIES: &[&str] = &[
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

#[cfg(any(target_os = "macos", feature = "snapshot"))]
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

#[cfg(any(target_os = "macos", feature = "snapshot"))]
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

#[cfg(any(target_os = "macos", feature = "snapshot"))]
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
            value if value.is_control() => {
                return Err(PermissionError::InvalidPath(path.to_owned()));
            }
            value => escaped.push(value),
        }
    }
    Ok(escaped)
}

#[cfg(feature = "snapshot")]
pub mod snapshot;

#[cfg(test)]
mod tests {

    use super::*;
    use provenant_os::exec::SystemExecutor;

    mod ipc;
    mod process_info;
    mod signals;
    mod sysctl;
    mod sysv_audit;

    #[test]
    fn git_control_link_refusal_names_the_file_and_the_recovery() {
        let error = PermissionError::GitControlHardLinked("/repo/.git/config".into());
        let message = error.to_string();
        assert!(message.contains("/repo/.git/config"));
        assert!(message.contains("samefile"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    // CI-required fixture: lane sandboxes may deny hard_link with EPERM.
    fn only_git_control_files_are_checked_for_other_names() -> Result<(), Box<dyn std::error::Error>>
    {
        let fixture = crate::test_support::Fixture::new()?;
        let git_dir = fixture.repo.common_git_dir().to_owned();
        verify_git_control_links(&fixture.repo)?;
        // Build output and dependency trees hold hard links and are not walked.
        let tree = fixture.root.join("target/debug");
        std::fs::create_dir_all(&tree)?;
        std::fs::write(tree.join("object"), b"o")?;
        std::fs::hard_link(tree.join("object"), tree.join("alias"))?;
        verify_git_control_links(&fixture.repo)?;
        std::fs::create_dir_all(git_dir.join("hooks"))?;
        std::fs::create_dir_all(git_dir.join("info"))?;
        std::fs::write(git_dir.join("hooks/pre-commit"), b"#!/bin/sh\n")?;
        std::fs::write(git_dir.join("info/exclude"), b"")?;
        for control in ["config", "hooks/pre-commit", "info/exclude"] {
            let alias = fixture.root.join("alias");
            std::fs::hard_link(git_dir.join(control), &alias)?;
            let error = verify_git_control_links(&fixture.repo).unwrap_err();
            assert!(
                matches!(&error, PermissionError::GitControlHardLinked(path) if path == &git_dir.join(control)),
                "{control}: {error}"
            );
            std::fs::remove_file(alias)?;
            verify_git_control_links(&fixture.repo)?;
        }
        Ok(())
    }

    #[cfg(any(target_os = "macos", feature = "snapshot"))]
    #[test]
    fn profiles_between_the_old_and_new_limits_compile_and_larger_ones_name_their_size()
    -> Result<(), Box<dyn std::error::Error>> {
        let (plan, profile) = write_grants_fixture();
        let mut grants = GrantSet::build(&plan, &profile, Path::new("/fixture/home"), None)?;
        let root = |index: usize, width: usize| PathGrant {
            path: PathBuf::from(format!("/fixture/reads/{index}/{}", "x".repeat(width))),
            scope: PathScope::Subtree,
        };
        // About 130 KiB: over the former 64 KiB cap, inside the launch limit.
        grants.reads = (0..1000).map(|index| root(index, 100)).collect();
        let source = render(&grants)?;
        assert!(source.len() > 64 * 1024 && source.len() <= PROFILE_LIMIT);
        grants.reads = (0..4000).map(|index| root(index, 200)).collect();
        let error = render(&grants).unwrap_err();
        assert!(
            matches!(error, PermissionError::ProfileTooLarge { bytes, limit: PROFILE_LIMIT } if bytes > PROFILE_LIMIT)
        );
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    // CI-required fixture: lane sandboxes may deny hard_link with EPERM.
    fn a_hard_linked_exact_grant_names_its_path() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        std::fs::write(root.join("file"), b"x")?;
        std::fs::hard_link(root.join("file"), root.join("linked"))?;
        let directories = [AnchoredDirectory::open_canonical(&root)?];
        let anchors = Anchors {
            home: &root,
            directories: &directories,
            worktree: None,
        };
        let grant = |name: &str| PathGrant {
            path: root.join(name),
            scope: PathScope::Exact,
        };
        assert!(matches!(
            anchors.verify_single_link_grant(&grant("linked")),
            Err(PermissionError::AnchorMismatchAt { path, stage })
                if path == root.join("linked") && stage == "file has other hard links"
        ));
        anchors.verify_single_link_grant(&grant("absent"))?;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn held_path_failures_name_the_path_and_the_check() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let present = root.join("present");
        std::fs::create_dir(&present)?;
        std::fs::write(present.join("file"), b"x")?;
        let directories = [AnchoredDirectory::open_canonical(&present)?];
        let anchors = Anchors {
            home: &root,
            directories: &directories,
            worktree: None,
        };
        let exact = |path: PathBuf| PathGrant {
            path,
            scope: PathScope::Exact,
        };
        let at = |error: PermissionError| match error {
            PermissionError::AnchorMismatchAt { path, stage } => (path, stage),
            other => panic!("unexpected {other}"),
        };
        assert_eq!(
            at(anchors
                .verify_executable(&exact(present.join("gone")))
                .unwrap_err()),
            (present.join("gone"), "executable is missing")
        );
        assert_eq!(
            at(anchors
                .verify_grant_held(&PathGrant {
                    path: root.clone(),
                    scope: PathScope::Subtree,
                })
                .unwrap_err()),
            (root.clone(), "directory anchor not held")
        );
        // A parent that exists but was never held is an error; one that does
        // not exist grants nothing.
        std::fs::create_dir(root.join("elsewhere"))?;
        assert_eq!(
            at(anchors
                .verify_grant_held(&exact(root.join("elsewhere/file")))
                .unwrap_err()),
            (root.join("elsewhere"), "parent directory anchor not held")
        );
        anchors.verify_grant_held(&exact(root.join("absent/file")))?;
        Ok(())
    }

    #[test]
    fn a_grant_in_a_directory_that_does_not_exist_needs_no_anchor()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let (mut plan, profile) = write_grants_fixture();
        plan.scratch = PathGrant {
            path: root.join("scratch"),
            scope: PathScope::Subtree,
        };
        plan.private_state = None;
        plan.read_only = None;
        let missing = root.join("not-yet/created");
        let mut profile = profile;
        profile.spec.state_writes = vec![
            PathGrant {
                path: missing.join("session"),
                scope: PathScope::Exact,
            },
            PathGrant {
                path: root.join("present-file"),
                scope: PathScope::Exact,
            },
        ];
        let anchors = required_anchors(&plan, &profile, None);
        assert!(!anchors.contains(&missing));
        assert!(anchors.contains(&root));
        std::fs::create_dir_all(&missing)?;
        assert!(required_anchors(&plan, &profile, None).contains(&missing));
        Ok(())
    }

    #[test]
    fn invalid_path_error_names_the_refused_path() {
        for path in [
            "/",
            "relative/path",
            "/opt/homebrew/bin/../Cellar/opencode-v2/2.0.32",
        ] {
            let error = validate_absolute(Path::new(path)).unwrap_err();
            assert_eq!(
                error.to_string(),
                format!("path is not a permitted absolute, normalised path: {path:?}")
            );
        }
    }

    fn write_grants_fixture() -> (PlanPermissions, ProviderProfile) {
        let grant = |path: &str, scope| PathGrant {
            path: path.into(),
            scope,
        };
        let plan = PlanPermissions {
            read_only: Some(crate::plan::ReadOnlyGrants::for_test(
                "/fixture/repo".into(),
                vec![grant("/fixture/repo/locks", PathScope::Subtree)],
            )),
            mode: AccessMode::ReadOnly,
            scratch: grant("/fixture/scratch", PathScope::Subtree),
            private_state: Some(grant("/fixture/private", PathScope::Subtree)),
            read_roots: Vec::new(),
            protected_roots: Vec::new(),
            secret_roots: Vec::new(),
            selected_authentication: Some("selected".into()),
            include_continuation: false,
            write_roots: vec![grant("/fixture/shared", PathScope::Subtree)],
            network: NetworkAccess::Provider,
            nested_confinement: false,
        };
        let profile = ProviderProfile {
            spec: ProviderProfileSpec {
                state_writes: vec![grant("/fixture/state/session", PathScope::Exact)],
                tool_writes: vec![grant("/fixture/tools", PathScope::Subtree)],
                authentication: ["unselected", "selected"]
                    .map(|mechanism| {
                        let credential =
                            grant(&format!("/fixture/auth/{mechanism}"), PathScope::Exact);
                        AuthenticationProfile {
                            mechanism: mechanism.into(),
                            roots: vec![credential.clone()],
                            in_place_writes: vec![credential],
                        }
                    })
                    .into(),
                ..ProviderProfileSpec::default()
            },
            observations: BTreeMap::new(),
        };
        (plan, profile)
    }

    #[test]
    fn write_grants_list_every_write_source() {
        let (mut plan, profile) = write_grants_fixture();
        let grant = |source, path: &str, scope| WriteGrant {
            source,
            path: path.into(),
            scope,
        };
        let mut expected = vec![
            grant(
                WriteSource::StateWrite,
                "/fixture/state/session",
                PathScope::Exact,
            ),
            grant(
                WriteSource::CredentialWrite,
                "/fixture/auth/selected",
                PathScope::Exact,
            ),
            grant(
                WriteSource::WriteRoot,
                "/fixture/shared",
                PathScope::Subtree,
            ),
            grant(WriteSource::ToolWrite, "/fixture/tools", PathScope::Subtree),
            grant(WriteSource::Scratch, "/fixture/scratch", PathScope::Subtree),
            grant(
                WriteSource::PrivateState,
                "/fixture/private",
                PathScope::Subtree,
            ),
        ];
        let mut read_only_expected = expected.clone();
        read_only_expected.push(grant(
            WriteSource::ReadOnlyException,
            "/fixture/repo/locks",
            PathScope::Subtree,
        ));
        assert_eq!(write_grants(&plan, &profile), read_only_expected);

        plan.read_only = None;
        plan.mode = AccessMode::WorktreeWrite(Box::new(WorktreeWriteAuthority {
            git_grants_digest: [7; 32],
            publishing: true,
            common_git_dir: Some("/fixture/git".into()),
            cargo_cache: PathGrant {
                path: "/fixture/cache".into(),
                scope: PathScope::Subtree,
            },
        }));
        expected.push(grant(
            WriteSource::LaneCache,
            "/fixture/cache",
            PathScope::Subtree,
        ));
        assert_eq!(write_grants(&plan, &profile), expected);

        plan.selected_authentication = None;
        assert!(
            !write_grants(&plan, &profile)
                .iter()
                .any(|g| g.source == WriteSource::CredentialWrite)
        );
    }

    #[test]
    fn writer_grants_do_not_include_shared_branch_paths() {
        let common = Path::new("/fixture/git");
        let grants = WorktreeWriteGrants::from_paths(
            Path::new("/fixture/lane"),
            Path::new("/fixture/git/worktrees/lane"),
            common,
            "lane/test",
        );
        for relative in [
            "refs/heads/lane/test",
            "refs/heads/lane/test.lock",
            "logs/refs/heads/lane/test",
            "logs/refs/heads/lane/test.lock",
        ] {
            assert!(
                !grants
                    .iter()
                    .any(|grant| grant.path == common.join(relative))
            );
        }
        let other = WorktreeWriteGrants::from_paths(
            Path::new("/fixture/lane"),
            Path::new("/fixture/git/worktrees/lane"),
            common,
            "lane/other",
        );
        let cache = PathGrant {
            path: "/fixture/cache".into(),
            scope: PathScope::Subtree,
        };
        assert_ne!(grants.digest(&cache), other.digest(&cache));
    }

    #[cfg(target_os = "macos")]
    fn worktree_grants_fixture() -> WorktreeWriteGrants {
        WorktreeWriteGrants::from_paths(
            Path::new("/fixture/repo"),
            Path::new("/fixture/git/private"),
            Path::new("/fixture/git"),
            "fixture",
        )
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn writer_test_grants_stay_within_the_attempt_and_mode()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut plan, profile) = write_grants_fixture();
        let scratch = "/fixture/runs/run/attempts/own/tmp/candidate-0";
        plan.scratch.path = scratch.into();
        let git = worktree_grants_fixture();
        for writer in [false, true] {
            if writer {
                plan.read_only = None;
                plan.mode = AccessMode::WorktreeWrite(Box::new(WorktreeWriteAuthority {
                    git_grants_digest: [7; 32],
                    publishing: true,
                    common_git_dir: Some("/fixture/git".into()),
                    cargo_cache: PathGrant {
                        path: "/fixture/cache".into(),
                        scope: PathScope::Subtree,
                    },
                }));
            }
            let rendered = String::from_utf8(render_profile(
                &plan,
                &profile,
                &[],
                &[],
                writer.then_some(&git),
                Path::new("/fixture/home"),
            )?)?;
            // Every ordinary writer/reviewer profile keeps database IPC denied.
            assert_eq!(
                rendered
                    .lines()
                    .filter(|line| line.starts_with("(allow ipc-"))
                    .collect::<Vec<_>>(),
                ["(allow ipc-posix-shm-read* (ipc-posix-name-prefix \"apple.cfprefs.\"))",]
            );
            let mut database = ProviderProfile {
                spec: profile.spec.clone(),
                observations: BTreeMap::new(),
            };
            database.spec.allow_database_ipc = true;
            let opted_in = String::from_utf8(render_profile(
                &plan,
                &database,
                &[],
                &[],
                writer.then_some(&git),
                Path::new("/fixture/home"),
            )?)?;
            assert!(opted_in.contains("(allow ipc-sysv-shm)\n(allow ipc-sysv-sem)\n"));
            assert_eq!(
                opted_in
                    .replace("(allow ipc-sysv-shm)\n", "")
                    .replace("(allow ipc-sysv-sem)\n", ""),
                rendered
            );
            let exec = format!("(allow process-exec (subpath \"{scratch}\"))");
            assert_eq!(rendered.lines().any(|line| line == exec), writer);
            assert!(rendered.contains(&format!("(allow file-read* (subpath \"{scratch}\"))")));
            for outside in [
                "/fixture/runs",
                "/fixture/runs/run",
                "/fixture/runs/run/attempts/own",
                "/fixture/runs/run/attempts/own/tmp",
                "/fixture/runs/run/attempts/own/tmp/candidate-1",
                "/fixture/runs/run/attempts/other/tmp/candidate-0",
            ] {
                for operation in ["process-exec", "file-read*"] {
                    assert!(
                        !rendered.contains(&format!("(allow {operation} (subpath \"{outside}\"))")),
                        "{outside}"
                    );
                }
            }
            assert!(!rendered.contains("(allow process-exec (subpath \"/fixture/private\"))"));
            let signals: Vec<_> = rendered
                .lines()
                .filter(|line| line.starts_with("(allow signal"))
                .collect();
            assert_eq!(signals, ["(allow signal (target same-sandbox))"]);
            assert!(rendered.contains("(deny sysctl-read (sysctl-name-prefix \"kern.procargs\") (sysctl-name-prefix \"kern.proc.\"))"));
            assert!(
                rendered
                    .contains("(deny process-info*)\n(allow process-info* (target same-sandbox))")
            );
            assert_eq!(
                rendered
                    .lines()
                    .filter(|line| line.starts_with("(allow process-info"))
                    .collect::<Vec<_>>(),
                ["(allow process-info* (target same-sandbox))"]
            );
            assert_eq!(
                rendered.contains("(allow file-read* (subpath \"/dev/fd\"))"),
                writer
            );
            assert!(!rendered.contains("(allow file-read* (subpath \"/dev\"))"));
            assert!(!rendered.contains("(allow file-write* (subpath \"/dev/fd\"))"));
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn write_grants_cover_every_rendered_file_write() -> Result<(), Box<dyn std::error::Error>> {
        let (mut plan, profile) = write_grants_fixture();
        let git = worktree_grants_fixture();
        let subtree = |path: &str| PathGrant {
            path: path.into(),
            scope: PathScope::Subtree,
        };
        for writer in [false, true] {
            if writer {
                plan.read_only = None;
                plan.mode = AccessMode::WorktreeWrite(Box::new(WorktreeWriteAuthority {
                    git_grants_digest: [7; 32],
                    publishing: true,
                    common_git_dir: Some("/fixture/git".into()),
                    cargo_cache: subtree("/fixture/cache"),
                }));
            }
            let credentials = &profile.spec.authentication[1].in_place_writes;
            let rendered = String::from_utf8(render_profile(
                &plan,
                &profile,
                &[],
                credentials,
                writer.then_some(&git),
                Path::new("/fixture/home"),
            )?)?;
            let grants = write_grants(&plan, &profile);
            let mut checked = 0;
            for line in rendered.lines().filter(|line| {
                line.starts_with("(allow ")
                    && (line.contains("file-write*") || line.contains("file-write-data"))
            }) {
                let (scope, tail) = if let Some((_, tail)) = line.split_once("(subpath \"") {
                    (PathScope::Subtree, tail)
                } else if let Some((_, tail)) = line.split_once("(literal \"") {
                    (PathScope::Exact, tail)
                } else {
                    return Err(format!("unrecognised write rule: {line}").into());
                };
                let path = Path::new(tail.split('"').next().ok_or("write rule path")?);
                if path == Path::new("/dev/null") || writer && git.iter().any(|g| g.path == path) {
                    continue;
                }
                assert!(
                    grants.iter().any(|g| g.path == path && g.scope == scope),
                    "unrecorded rule: {line}"
                );
                checked += 1;
            }
            assert_eq!(checked, 7);
        }
        Ok(())
    }

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
    fn exact_tool_exec_grants_validate_without_allowing_credential_paths() {
        let mut spec = ProviderProfileSpec {
            process_executables: vec![PathGrant {
                path: "/bin/sh".into(),
                scope: PathScope::Exact,
            }],
            allow_process_fork: true,
            tool_exec_roots: vec![PathGrant {
                path: "/opt/check/bin/python3".into(),
                scope: PathScope::Exact,
            }],
            ..Default::default()
        };
        assert!(validate_profile_spec(&spec).is_ok());
        for path in [
            "/Users/fixture/.config/gh/token",
            "/Users/fixture/Library/Application Support/GitHub CLI/token",
            "/Users/fixture/Library/Keychains/login.keychain-db",
            "/Users/fixture/.ssh/key",
            "/Users/fixture/.config/gcloud/token",
        ] {
            spec.tool_exec_roots[0].path = path.into();
            assert!(matches!(
                validate_profile_spec(&spec),
                Err(PermissionError::CredentialRoot)
            ));
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

    #[test]
    fn grants_enclosing_a_credential_root_are_refused() {
        let grant = |path: &str, scope| PathGrant {
            path: PathBuf::from(path),
            scope,
        };
        for path in [
            "/Users/alice/.config",
            "/Users/alice/Library",
            "/Users/alice/Library/Application Support",
            "/Users/alice/Archives",
        ] {
            for validate in [validate_read_exec_grant, validate_noncredential_grant] {
                assert!(
                    matches!(
                        validate(&grant(path, PathScope::Subtree)),
                        Err(PermissionError::CredentialRoot)
                    ),
                    "{path}"
                );
            }
            // An exact grant exposes the directory entry, not what it holds.
            assert!(validate_read_exec_grant(&grant(path, PathScope::Exact)).is_ok());
        }
        for path in [
            "/Users/alice/.config/cursor",
            "/Users/alice/.config/opencode",
            "/Users/alice/Library/pnpm",
            "/Users/alice/Library/Caches/pnpm",
            "/Users/alice/project/.config",
            "/Users/alice/project/Archives",
            "/Library/Developer/CommandLineTools",
            "/srv/work/.config",
        ] {
            assert!(
                validate_read_exec_grant(&grant(path, PathScope::Subtree)).is_ok(),
                "{path}"
            );
        }
        // An unconventional home is checked against the host's own path.
        let home = Path::new("/srv/alice");
        for path in [
            "/srv",
            "/srv/alice",
            "/srv/alice/.config",
            "/srv/alice/Library",
        ] {
            assert!(
                encloses_home_credentials(&grant(path, PathScope::Subtree), home),
                "{path}"
            );
        }
        for path in ["/srv/alice/project", "/srv/alice/project/.config"] {
            assert!(
                !encloses_home_credentials(&grant(path, PathScope::Subtree), home),
                "{path}"
            );
        }
    }

    #[test]
    fn lane_root_table_agrees_with_compiler_checks() {
        let standalone =
            "/Users/alice/.codex/packages/standalone/releases/0.163.0-aarch64-apple-darwin";
        let home = Path::new("/fixture/host-home");
        for (path, read_allowed, write_allowed) in [
            ("/Users/alice/.ssh", false, false),
            ("/Users/alice/.config", false, false),
            (standalone, true, false),
            ("/Users/alice/project", true, true),
        ] {
            let grant = PathGrant {
                path: PathBuf::from(path),
                scope: PathScope::Subtree,
            };
            assert_eq!(
                validate_read_exec_grant(&grant).is_ok(),
                read_allowed,
                "read {path}"
            );
            assert_eq!(
                validate_noncredential_grant(&grant).is_ok(),
                write_allowed,
                "write {path}"
            );
            assert_eq!(
                !refuses_lane_root(Path::new(path), true, Some(home)),
                read_allowed,
                "entry read {path}"
            );
            assert_eq!(
                !refuses_lane_root(Path::new(path), false, Some(home)),
                write_allowed,
                "entry write {path}"
            );
        }
        // An injected nonconventional home is refused at entry exactly as the
        // compiler refuses it, without consulting this host's HOME.
        let home = Path::new("/srv/team/alice");
        for (path, read_allowed, write_allowed) in [
            ("/srv/team", false, false),
            ("/srv/team/alice", false, false),
            ("/srv/team/alice/.config", false, false),
            ("/srv/team/alice/project", true, true),
        ] {
            assert_eq!(
                !refuses_lane_root(Path::new(path), true, Some(home)),
                read_allowed,
                "entry read {path}"
            );
            assert_eq!(
                !refuses_lane_root(Path::new(path), false, Some(home)),
                write_allowed,
                "entry write {path}"
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn compiler_refuses_grants_of_or_around_an_unconventional_host_home() {
        // Static grant checks know only /Users and /home; the compiler must
        // still refuse the host's actual home and any root that encloses it.
        let home = Path::new("/srv/team/alice");
        let grant = |path: &Path| PathGrant {
            path: path.to_owned(),
            scope: PathScope::Subtree,
        };
        let profile = ProviderProfile {
            spec: ProviderProfileSpec {
                process_executables: vec![PathGrant {
                    path: "/opt/provider/bin/cli".into(),
                    scope: PathScope::Exact,
                }],
                ..ProviderProfileSpec::default()
            },
            observations: BTreeMap::new(),
        };
        let plan = |read_roots: Vec<PathGrant>, write_roots: Vec<PathGrant>| PlanPermissions {
            read_only: None,
            mode: AccessMode::ReadOnly,
            scratch: grant(Path::new("/srv/scratch")),
            private_state: None,
            read_roots,
            protected_roots: Vec::new(),
            secret_roots: Vec::new(),
            selected_authentication: None,
            include_continuation: false,
            write_roots,
            network: NetworkAccess::Provider,
            nested_confinement: false,
        };
        let anchors = Anchors {
            home,
            directories: &[],

            worktree: None,
        };
        for root in [home, Path::new("/srv/team"), Path::new("/srv")] {
            for plan in [
                plan(vec![grant(root)], Vec::new()),
                plan(Vec::new(), vec![grant(root)]),
            ] {
                assert!(validate_read_exec_grant(&grant(root)).is_ok(), "{root:?}");
                assert!(
                    matches!(
                        compile_sbpl(&plan, &profile, &anchors),
                        Err(PermissionError::CredentialRoot)
                    ),
                    "{root:?}"
                );
            }
        }
        // A project beneath the home passes this check and fails later only
        // because the fixture holds no anchors.
        assert!(matches!(
            compile_sbpl(
                &plan(vec![grant(&home.join("project"))], Vec::new()),
                &profile,
                &anchors
            ),
            Err(PermissionError::AnchorMismatchAt { .. })
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn anchored_directory_rejects_noncanonical_tmp_aliases() {
        let temp = crate::test_support::tempdir().expect("temporary anchor");
        // An explicit alias exercises macOS symlink rejection inside a writable fixture.
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(temp.path(), &alias).expect("temporary anchor alias");
        let canonical = provenant_os::fs::canonicalize(&alias).expect("canonical temporary anchor");
        assert_ne!(alias, canonical);
        assert!(matches!(
            AnchoredDirectory::open_canonical(&alias),
            Err(PermissionError::InvalidPath(path)) if path == alias
        ));
        assert!(AnchoredDirectory::open_canonical(canonical).is_ok());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn anchored_directory_detects_replacement_at_its_sbpl_path() {
        let temp = crate::test_support::tempdir().expect("temporary anchor root");
        let original = temp.path().join("root");
        std::fs::create_dir(&original).expect("anchor directory");
        let path = provenant_os::fs::canonicalize(&original).expect("canonical anchor path");

        let anchor = AnchoredDirectory::open_canonical(&path).expect("open anchor");
        let moved = temp.path().join("moved");
        std::fs::rename(&original, &moved).expect("move held anchor");
        std::fs::create_dir(&original).expect("replacement directory");
        let directories = [anchor];
        let anchors = Anchors {
            home: Path::new("/fixture/home"),
            directories: &directories,
            worktree: None,
        };
        let error = anchors.verify_all().expect_err("replaced anchor accepted");
        assert!(
            matches!(error, PermissionError::AnchorMismatchAt { path: actual, .. } if actual == path)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[allow(
        clippy::print_stderr,
        reason = "Native probe omissions must be visible in test output"
    )]
    fn directory_discovery_keeps_literal_scope_and_refuses_symlinks()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let fixture = crate::test_support::Fixture::new()?;
        let root = provenant_os::fs::canonicalize(fixture._temp.path())?;
        let home = provenant_os::fs::canonicalize(&fixture.home)?;
        let project = root.join("lane");
        fixture.run(&[
            "worktree",
            "add",
            "-b",
            "discovery",
            project.to_str().ok_or("path")?,
        ])?;
        let linked = crate::repo::verify_worktree(&fixture.repo, &project)?;
        let selected_ref = GitRefName::branch("discovery")?;
        let authority = WorktreeWriteAuthority::for_worktree(&linked, &selected_ref, &home)?;
        prepare_writer_directories(&linked, &selected_ref, &authority, &home)?;
        let scratch = root.join("scratch");
        std::fs::create_dir(&scratch)?;
        std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(0o700))?;
        let discovery = [&project, &root]
            .into_iter()
            .flat_map(|parent| {
                [".agents", ".claude", ".codex"].map(|name| PathGrant {
                    path: parent.join(name),
                    scope: PathScope::Exact,
                })
            })
            .collect::<Vec<_>>();
        for grant in &discovery {
            std::fs::create_dir(&grant.path)?;
            std::fs::write(grant.path.join("config.json"), b"private config")?;
        }
        let mut profile = ProviderProfile {
            spec: ProviderProfileSpec {
                process_executables: vec![PathGrant {
                    path: "/bin/cat".into(),
                    scope: PathScope::Exact,
                }],
                discovery_reads: discovery,
                ..ProviderProfileSpec::default()
            },
            observations: BTreeMap::new(),
        };
        let mut plan = PlanPermissions {
            mode: AccessMode::ReadOnly,
            read_only: None,
            read_roots: vec![PathGrant {
                path: project.clone(),
                scope: PathScope::Subtree,
            }],
            protected_roots: Vec::new(),
            secret_roots: Vec::new(),
            scratch: PathGrant {
                path: scratch.clone(),
                scope: PathScope::Subtree,
            },
            private_state: None,
            selected_authentication: None,
            include_continuation: false,
            write_roots: Vec::new(),
            network: NetworkAccess::Provider,
            nested_confinement: false,
        };
        for mode in [
            AccessMode::ReadOnly,
            AccessMode::WorktreeWrite(Box::new(authority)),
        ] {
            plan.mode = mode;
            let worktree = matches!(plan.mode, AccessMode::WorktreeWrite(_))
                .then_some((&linked, &selected_ref));
            let directories = required_anchors(&plan, &profile, worktree)
                .iter()
                .map(AnchoredDirectory::open_canonical)
                .collect::<Result<Vec<_>, _>>()?;
            let anchors = Anchors {
                home: &home,
                directories: &directories,
                worktree,
            };
            let compiled = compile_sbpl(&plan, &profile, &anchors)?;
            let sbpl = std::str::from_utf8(&compiled.profile)?;
            let held_without_parent = directories.iter().filter(|anchor| anchor.path != root);
            let missing_parent = Anchors {
                home: &home,
                directories: &held_without_parent
                    .map(|anchor| AnchoredDirectory::open_canonical(&anchor.path))
                    .collect::<Result<Vec<_>, _>>()?,
                worktree,
            };
            let error = compile_sbpl(&plan, &profile, &missing_parent)
                .err()
                .ok_or("missing discovery parent accepted")?;
            assert!(
                matches!(error, PermissionError::AnchorMismatchAt { path, .. } if path == root.join(".agents"))
            );
            // Only the ancestor's literal directory is readable through discovery.
            for grant in profile
                .spec
                .discovery_reads
                .iter()
                .filter(|grant| grant.path.parent() == Some(root.as_path()))
            {
                let argv = [
                    OsString::from(SANDBOX_EXEC),
                    "-p".into(),
                    sbpl.into(),
                    "/bin/cat".into(),
                    grant.path.join("config.json").into_os_string(),
                ];
                let mut options = ExecOptions::new(&argv, &scratch, Duration::from_secs(2));
                options.stderr = StreamPolicy::Capture { limit: 1024 };
                let outcome = fixture.executor.execute(options)?;
                if String::from_utf8_lossy(&outcome.stderr.bytes)
                    .contains("sandbox_apply: Operation not permitted")
                {
                    eprintln!("native discovery denial probe unavailable under inherited sandbox");
                    assert_ne!(
                        std::env::var("PROVENANT_CONFINEMENT_REQUIRED").as_deref(),
                        Ok("1")
                    );
                    break;
                }
                assert!(denied_probe(&outcome), "{outcome:?}");
                assert!(outcome.stdout.bytes.is_empty());
            }
            // The regular-file path must still reject directories, even when
            // the same literal is also requested for discovery.
            profile
                .spec
                .runtime_reads
                .push(profile.spec.discovery_reads[0].clone());
            assert!(matches!(
                compile_sbpl(&plan, &profile, &anchors),
                Err(PermissionError::AnchorMismatchAt { .. })
            ));
            profile.spec.runtime_reads.pop();
            let path = &profile.spec.discovery_reads[0].path;
            let moved = project.join("moved-config");
            std::fs::rename(path, &moved)?;
            symlink(&moved, path)?;
            let error = compile_sbpl(&plan, &profile, &anchors)
                .err()
                .ok_or("discovery symlink accepted")?;
            assert!(
                matches!(error, PermissionError::AnchorMismatchAt { path: actual, .. } if actual == *path)
            );
            std::fs::remove_file(path)?;
            std::fs::rename(moved, path)?;
        }
        Ok(())
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
        let scratch = provenant_os::fs::canonicalize(f._temp.path())?.join("private-tmp");
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
        let home = provenant_os::fs::canonicalize(&f.home)?;
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
            .map(AnchoredDirectory::open_canonical)
            .collect::<Result<Vec<_>, _>>()?;
        let anchors = Anchors {
            home: &home,
            directories: &directories,

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
        assert!(!source.contains(&format!(
            "(allow process-exec (subpath \"{}\"))",
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
            stderr: provenant_os::exec::CapturedStream {
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
            publishing: true,
            common_git_dir: Some("/fixture/git".into()),
            cargo_cache: PathGrant {
                path: "/fixture/home/Library/Caches/provenant-lane-cargo/fixture".into(),
                scope: PathScope::Subtree,
            },
        };

        let directories = [];
        let anchors = Anchors {
            home: Path::new("/fixture/home"),
            directories: &directories,
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
        let home = provenant_os::fs::canonicalize(&fixture.home)?;
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

        prepare_writer_directories(&linked, &selected_ref, &authority, &home)?;
        prepare_writer_directories(&linked, &selected_ref, &authority, &home)?;
        let mut metadata = Directory::open_anchor(cache)?.metadata()?;
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
                    prepare_writer_directories(&linked, &selected_ref, &authority, &home),
                    Err(PermissionError::AnchorMismatch)
                ),
                "mode {mode:o}"
            );
        }
        std::fs::set_permissions(cache, std::fs::Permissions::from_mode(0o700))?;
        let namespace = cache.parent().ok_or("namespace")?;
        std::fs::set_permissions(namespace, std::fs::Permissions::from_mode(0o755))?;
        assert!(matches!(
            prepare_writer_directories(&linked, &selected_ref, &authority, &home),
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
            &provenant_os::fs::canonicalize(&fixture.home)?,
        )?;
        fixture.run(&["pack-refs", "--all"])?;
        let missing_parent = common.join("refs/heads/s3");
        if missing_parent.exists() {
            std::fs::remove_dir(&missing_parent)?;
        }
        assert!(!missing_parent.exists());

        prepare_writer_directories(
            &linked,
            &selected_ref,
            &authority,
            &provenant_os::fs::canonicalize(&fixture.home)?,
        )?;
        let grants = WorktreeWriteGrants::derive(&linked, &selected_ref);
        assert_eq!(
            authority.git_grants_digest,
            grants.digest(&authority.cargo_cache)
        );
        assert_eq!(grants.common_git_dir.path, common);
        assert_eq!(grants.private_git_dir.path, private);
        assert!(!missing_parent.exists());

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

        let directories = anchor_paths
            .iter()
            .map(AnchoredDirectory::open_canonical)
            .collect::<Result<Vec<_>, _>>()?;
        let anchors = Anchors {
            home: &provenant_os::fs::canonicalize(&fixture.home)?,
            directories: &directories,
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
        for directory in [&common, &private] {
            assert!(source.contains(&format!(
                "(deny file-write* (subpath \"{}\"))",
                directory.display()
            )));
        }
        let git_writes: Vec<_> = source
            .lines()
            .filter(|line| line.starts_with("(allow file-write* (literal "))
            .collect();
        assert!(git_writes.is_empty());
        for relative in [
            "config",
            "config.lock",
            "packed-refs",
            "refs/heads/main",
            "refs/remotes",
            "refs/remotes/origin/s3/authority",
            "refs/remotes/origin/s3/authority.lock",
            "refs/remotes/upstream",
            "logs/refs/remotes",
            "logs/refs/remotes/origin/s3/authority",
            "logs/refs/remotes/origin/s3/authority.lock",
            "hooks",
        ] {
            assert!(
                !source.contains(&format!("\"{}\"", common.join(relative).display())),
                "{relative}"
            );
        }

        let other_ref = GitRefName::branch("s3/other")?;
        let mismatched = Anchors {
            home: &provenant_os::fs::canonicalize(&fixture.home)?,
            directories: &directories,
            worktree: Some((&linked, &other_ref)),
        };
        assert!(matches!(
            verify_worktree_authority(&authority, &mismatched),
            Err(PermissionError::GitWriteAuthorityMissing)
        ));

        let mut tampered = authority.clone();
        tampered.git_grants_digest[0] ^= 1;
        assert!(matches!(
            verify_worktree_authority(&tampered, &anchors),
            Err(PermissionError::GitWriteAuthorityMissing)
        ));

        for common_git_dir in [None, Some(fixture._temp.path().join("other-git"))] {
            let mut tampered = authority.clone();
            tampered.common_git_dir = common_git_dir;
            assert!(matches!(
                verify_worktree_authority(&tampered, &anchors),
                Err(PermissionError::GitWriteAuthorityMissing)
            ));
        }

        let original_head = std::fs::read(private.join("HEAD"))?;
        std::fs::write(private.join("HEAD"), b"ref: refs/heads/s3/other\n")?;
        assert!(matches!(
            verify_worktree_authority(
                &WorktreeWriteAuthority::for_worktree(
                    &linked,
                    &selected_ref,
                    &provenant_os::fs::canonicalize(&fixture.home)?
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
        let root = provenant_os::fs::canonicalize(temp.path())?;

        let scratch_path = root.join("scratch");
        std::fs::create_dir(&scratch_path)?;
        std::fs::set_permissions(&scratch_path, std::fs::Permissions::from_mode(0o700))?;
        let scratch = PathGrant {
            path: scratch_path.clone(),
            scope: PathScope::Subtree,
        };
        let directories = [AnchoredDirectory::open_canonical(&scratch_path)?];
        let anchors = Anchors {
            home: Path::new("/fixture/home"),
            directories: &directories,

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
            Err(PermissionError::AnchorMismatchAt { .. })
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
        let root = provenant_os::fs::canonicalize(fixture._temp.path())?;
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
            &provenant_os::fs::canonicalize(&fixture.home)?,
        )?;
        prepare_writer_directories(
            &linked,
            &selected_ref,
            &authority,
            &provenant_os::fs::canonicalize(&fixture.home)?,
        )?;
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
        let cargo = provenant_os::fs::canonicalize(&fixture.home)?.join(".cargo");
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
        // Probe existing files so a denial cannot be an absent parent.
        for relative in [
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
            "logs/refs/remotes/origin/HEAD",
            "refs/remotes/upstream/HEAD",
            "logs/refs/remotes/upstream/HEAD",
        ] {
            let path = common.join(relative);
            std::fs::create_dir_all(path.parent().ok_or("remote ref parent")?)?;
            std::fs::write(path, b"ref: refs/remotes/origin/main\n")?;
        }
        let executables = [
            provenant_os::fs::canonicalize(Path::new("/usr/bin/touch"))?,
            provenant_os::fs::canonicalize(Path::new("/bin/mv"))?,
            provenant_os::fs::canonicalize(Path::new("/bin/ln"))?,
            provenant_os::fs::canonicalize(Path::new("/bin/sh"))?,
            provenant_os::fs::canonicalize(Path::new("/bin/cat"))?,
            provenant_os::fs::canonicalize(Path::new("/usr/sbin/sysctl"))?,
            provenant_os::fs::canonicalize(Path::new("/usr/bin/perl"))?,
        ];

        let profile = ProviderProfile {
            spec: ProviderProfileSpec {
                state_writes: vec![PathGrant {
                    path: provider_state.clone(),
                    scope: PathScope::Subtree,
                }],
                deny_keychain_access: true,
                allow_process_fork: true,
                allow_database_ipc: true,
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
                (FixtureOperation::GitIndexWrite, FixtureOutcome::Denied),
                (FixtureOperation::GitHeadWrite, FixtureOutcome::Denied),
                (FixtureOperation::GitObjectsWrite, FixtureOutcome::Denied),
                (FixtureOperation::SelectedRefWrite, FixtureOutcome::Denied),
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
            common.join("objects"),
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
            .map(AnchoredDirectory::open_canonical)
            .collect::<Result<Vec<_>, _>>()?;
        let anchors = Anchors {
            home: &provenant_os::fs::canonicalize(&fixture.home)?,
            directories: &directories,
            worktree: Some((&linked, &selected_ref)),
        };
        let executor = SystemExecutor::new(2)?;
        let cancel = CancelToken::new();
        let env = [
            ("HOME".into(), home.into_os_string()),
            ("PROVENANT_STATE_DIR".into(), state.clone().into_os_string()),
            ("XDG_CONFIG_HOME".into(), xdg.into_os_string()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
        ];
        let (baseline, baseline_diagnostic) = probe_sandbox_with_diagnostic(&BackendContext {
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
        let backend = MacOsSandboxBackend { ready: true };
        // Existing hard links in the scratch, the worktree and its ignored
        // build and dependency trees do not refuse the launch: the sandbox
        // denies new links, and the few Git control files are checked directly.
        let alias = scratch.join("hardlink-probe");
        std::fs::hard_link(&protected_file, &alias)?;
        let hardlink_path = linked.path().join("hardlink");
        std::fs::write(linked.path().join(".gitignore"), "target/\nnode_modules/\n")?;
        let aliases = [
            "target/debug/object.o",
            "node_modules/pkg/file",
            "node_modules/pkg/alias",
        ]
        .map(|path| linked.path().join(path));
        for alias in &aliases {
            std::fs::create_dir_all(alias.parent().ok_or("alias parent")?)?;
            std::fs::hard_link(&protected_file, alias)?;
        }
        let compiled = backend.compile(&plan, &profile, &anchors)?;
        let sbpl = String::from_utf8(compiled.profile)?;
        assert!(!sbpl.contains("hardlink-probe"));
        for alias in &aliases {
            assert!(!sbpl.contains(&alias.display().to_string()));
        }
        assert!(!sbpl.contains("file-write-unlink"));
        std::fs::remove_file(alias)?;
        // A second name for a Git control file is the one link that refuses.
        // (Repository revalidation already refuses a second name for `config`.)
        std::fs::create_dir_all(common.join("hooks"))?;
        let hook = common.join("hooks/pre-commit");
        std::fs::write(&hook, b"#!/bin/sh\n")?;
        let hook_alias = linked.path().join("hook-alias");
        std::fs::hard_link(&hook, &hook_alias)?;
        assert!(matches!(
            backend.compile(&plan, &profile, &anchors),
            Err(PermissionError::GitControlHardLinked(path)) if path == hook
        ));
        std::fs::remove_file(hook_alias)?;
        std::fs::remove_file(hook)?;
        assert!(sbpl.contains("(deny file-link)"));
        assert!(sbpl.contains("(allow sysctl-read)"));
        // A writer may run what it builds into a write root.
        assert!(sbpl.contains(&format!(
            "(allow process-exec (subpath \"{}\"))",
            project_locks.display()
        )));
        // A read-only lane over the same grants, for the agent socket probe.
        let read_only_plan = PlanPermissions {
            mode: AccessMode::ReadOnly,
            ..plan.clone()
        };
        let read_only_anchors = Anchors {
            worktree: None,
            ..anchors
        };
        let read_only_sbpl = String::from_utf8(
            backend
                .compile(&read_only_plan, &profile, &read_only_anchors)?
                .profile,
        )?;
        let run_in = |sbpl: &str, program: &Path, args: &[&Path]| {
            let mut argv = vec![
                OsString::from(SANDBOX_EXEC),
                OsString::from("-p"),
                OsString::from(sbpl),
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
        let run = |program: &Path, args: &[&Path]| run_in(&sbpl, program, args);
        // The per-user peer agent relays to fresh `fabric peer` children in
        // the login session, so no lane may connect to its socket. The
        // unconfined connection proves the listener; the system DNS socket
        // proves the confined client runs and can connect where allowed.
        let agent = state.join("peer-agent");
        std::fs::create_dir_all(&agent)?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o700))?;
        }
        let peer_socket = agent.join("peer.sock");
        let _peer_listener = std::os::unix::net::UnixListener::bind(&peer_socket)?;
        std::os::unix::net::UnixStream::connect(&peer_socket)?;
        let connect = |sbpl: &str, socket: &Path| {
            run_in(
                sbpl,
                &executables[6],
                &[
                    Path::new("-MIO::Socket::UNIX"),
                    Path::new("-e"),
                    Path::new("IO::Socket::UNIX->new(Peer => $ARGV[0]) or die \"connect: $!\\n\""),
                    socket,
                ],
            )
        };
        let system_socket = Path::new("/private/var/run/mDNSResponder");
        for (name, sbpl) in [("writer", &sbpl), ("read-only", &read_only_sbpl)] {
            let outcome = connect(sbpl, system_socket);
            assert!(
                successful_probe(&outcome),
                "{name} system socket: {:?}; {}",
                outcome.termination,
                String::from_utf8_lossy(&outcome.stderr.bytes)
            );
            let mut audit = sysv_audit::SysvAudit::new(
                &executor,
                &plan.scratch.path.join(format!("sysv-{name}")),
            )?;
            let script = format!(
                "{}{}",
                sysv_audit::PERL_JOURNAL,
                r#"my ($shm, $sem);
END {
    my @errors;
    push @errors, "shmctl: $!\n" if defined $shm && !shmctl($shm, IPC_RMID, 0);
    push @errors, "semctl: $!\n" if defined $sem && !semctl($sem, 0, IPC_RMID, 0);
    die join("", @errors) if @errors;
}
$shm = shmget(IPC_PRIVATE, 56, IPC_CREAT | 0600);
die "shmget: $!\n" unless defined $shm;
own("m", $shm);
shmwrite($shm, "ok", 0, 2) or die "shmwrite: $!\n";
shmread($shm, my $value, 0, 2) or die "shmread: $!\n";
die "shared memory contents\n" unless $value eq "ok";
$sem = semget(IPC_PRIVATE, 1, IPC_CREAT | 0600);
die "semget: $!\n" unless defined $sem;
own("s", $sem);
semop($sem, pack("s!3", 0, 1, 0)) or die "semop: $!\n";
"#
            );
            let outcome = run_in(
                sbpl,
                &executables[6],
                &[
                    Path::new("-MIPC::SysV=IPC_PRIVATE,IPC_CREAT,IPC_RMID"),
                    Path::new("-e"),
                    Path::new(&script),
                    audit.journal(),
                ],
            );
            audit.finish()?;
            assert!(
                successful_probe(&outcome),
                "{name} SysV IPC: {:?}; {}",
                outcome.termination,
                String::from_utf8_lossy(&outcome.stderr.bytes)
            );
            let outside = root.join("outside-write-roots");
            assert!(denied_probe(&run_in(sbpl, &executables[0], &[&outside])));
            assert!(!outside.exists(), "{name} wrote outside its writable roots");
        }

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
        for name in [
            "index",
            "index.lock",
            "HEAD",
            "HEAD.lock",
            "FETCH_HEAD",
            "COMMIT_EDITMSG",
            "MERGE_HEAD",
            "ORIG_HEAD",
            "next-index-123.lock",
        ] {
            let outcome = run(&executables[0], &[&private.join(name)]);
            assert!(
                denied_probe(&outcome),
                "actual linked metadata {name} must be immutable: {:?}; {}",
                outcome.termination,
                String::from_utf8_lossy(&outcome.stderr.bytes)
            );
        }
        for relative in [
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
            "logs/refs/remotes/origin/HEAD",
        ] {
            assert!(!successful_probe(&run(
                &executables[0],
                &[&common.join(relative)]
            )));
        }
        // Package managers link into node_modules; a link grants nothing,
        // since the sandbox checks the resolved path.
        let new_link = linked.path().join("new-link");
        // A build script built into a declared write root (a shared
        // CARGO_TARGET_DIR) runs; the sandbox checks the exec of the file.
        let build_script = project_locks.join("build-script-main");
        let scratch_script = scratch.join("build-script-main");
        let undeclared_target = root.join("undeclared-target");
        std::fs::create_dir_all(&undeclared_target)?;
        let undeclared_script = undeclared_target.join("build-script-main");
        {
            use std::os::unix::fs::PermissionsExt;
            for script in [&build_script, &scratch_script, &undeclared_script] {
                std::fs::write(script, "#!/bin/sh\nexit 0\n")?;
                std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o755))?;
            }
        }
        for (name, outcome) in [
            (
                "tool store write",
                run(&executables[0], &[&cargo.join("registry/crate")]),
            ),
            (
                "write root",
                run(&executables[0], &[&project_locks.join("lock")]),
            ),
            ("write root exec", run(&build_script, &[])),
            ("attempt tmp exec", run(&scratch_script, &[])),
            (
                "child signal",
                run(
                    &executables[3],
                    &[
                        Path::new("-c"),
                        Path::new(
                            "/bin/sleep 1 & child=$!; kill -TERM \"$child\"; result=$?; wait \"$child\"; exit \"$result\"",
                        ),
                    ],
                ),
            ),
            (
                "descriptor read",
                run(
                    &executables[3],
                    &[
                        Path::new("-c"),
                        Path::new("test \"$(printf ok | /bin/cat /dev/fd/0)\" = ok"),
                    ],
                ),
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
            ("writer peer agent socket", connect(&sbpl, &peer_socket)),
            (
                "read-only peer agent socket",
                connect(&read_only_sbpl, &peer_socket),
            ),
            ("undeclared target exec", run(&undeclared_script, &[])),
            (
                "parent signal probe",
                run(
                    &executables[3],
                    &[Path::new("-c"), Path::new("kill -0 \"$PPID\"")],
                ),
            ),
            (
                "git config write",
                run(&executables[0], &[&common.join("config")]),
            ),
            (
                "packed refs write",
                run(&executables[0], &[&common.join("packed-refs")]),
            ),
            (
                "other branch write",
                run(&executables[0], &[&common.join("refs/heads/main")]),
            ),
            (
                "other remote ref write",
                run(
                    &executables[0],
                    &[&common.join("refs/remotes/upstream/HEAD")],
                ),
            ),
            (
                "other remote reflog write",
                run(
                    &executables[0],
                    &[&common.join("logs/refs/remotes/upstream/HEAD")],
                ),
            ),
            (
                "git hooks write",
                run(&executables[0], &[&common.join("hooks/pre-push")]),
            ),
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
        assert!(!rendered.contains("pseudo-tty") && !rendered.contains("/dev/ttys"));

        spec.allow_process_fork = false;
        spec.loopback_server = true;
        spec.allow_pseudo_tty = true;
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
        // A pseudo-terminal opens the multiplexer and only its own allocations.
        assert!(rendered.contains("(allow pseudo-tty)"));
        assert!(rendered.contains("(literal \"/dev/ptmx\")"));
        assert!(rendered.contains(
            "(require-all (regex #\"^/dev/ttys[0-9]+$\") (extension \"com.apple.sandbox.pty\"))"
        ));
        assert!(!rendered.contains("(subpath \"/dev\")"));
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
        // Read-only lanes never exec from write roots.
        assert!(!provider.contains("(allow process-exec (subpath \"/work/repo/.agent-run\"))"));
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
            "(deny file-read* (subpath \"/Users/fixture/.gnupg\"))",
            "(deny file-read* (subpath \"/Users/fixture/.azure\"))",
            "(deny file-read* (subpath \"/Users/fixture/.config/gh\"))",
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
        for path in [
            "/Users/fixture/.config/gh",
            "/Users/fixture/Library/Application Support/GitHub CLI",
        ] {
            let read = format!("(allow file-read* (subpath \"{path}\"))");
            let deny_read = format!("(deny file-read* (subpath \"{path}\"))");
            let deny_write = format!("(deny file-write* (subpath \"{path}\"))");
            assert!(open.contains(&read), "{path}");
            assert!(!open.contains(&deny_read), "{path}");
            assert!(open.contains(&deny_write), "{path}");
            assert!(!provider.contains(&read), "{path}");
            assert!(!local.contains(&read), "{path}");
            assert!(provider.contains(&deny_read), "{path}");
        }
        let ssh_read = "(deny file-read* (subpath \"/Users/fixture/.ssh\"))";
        assert!(
            open.find(ssh_read).expect("ssh deny")
                > open.rfind("(allow file-read*").expect("allow")
        );

        // A secret root overlapping the gh config must be denied after the allow.
        for secret in [
            "/Users/fixture/.config/gh",
            "/Users/fixture/.config/gh/hosts.yml",
            "/Users/fixture/.config",
        ] {
            plan.secret_roots = vec![grant(secret)];
            let hidden = render(&plan);
            let deny = format!("(deny file-read* (subpath \"{secret}\"))");
            let allow = "(allow file-read* (subpath \"/Users/fixture/.config/gh\"))";
            assert!(hidden.find(&deny).expect(secret) > hidden.find(allow).expect(allow));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_sandbox_probe_runs_through_executor_with_isolated_environment() {
        let temp = crate::test_support::tempdir().expect("temporary isolated root");
        let root = provenant_os::fs::canonicalize(temp.path()).expect("canonical isolated root");
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

        let executor = SystemExecutor::new(2).expect("bounded utility executor");
        let cancel = CancelToken::new();
        let context = BackendContext {
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
            let executor = SystemExecutor::new(1).expect("bounded executor");
            let cancel = CancelToken::new();
            let context = BackendContext {
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
}
