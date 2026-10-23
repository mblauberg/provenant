//! Typed permission plans and the fail-closed macOS SBPL backend.
//!
//! Provider-owned profile data and observations are checked against every
//! requested permission. Linux deliberately exposes an unavailable backend.
use std::collections::BTreeMap;
#[cfg(target_os = "macos")]
use std::collections::BTreeSet;
use std::ffi::OsStr;
#[cfg(target_os = "macos")]
use std::ffi::OsString;
#[cfg(target_os = "macos")]
use std::io::Read;
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
use provenant_supervisor::fs::{FileKind, SafeFs};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::repo::{GitRefName, VerifiedWorktree};

const MAX_PATH_BYTES: usize = 4096;
#[cfg(any(target_os = "macos", test))]
const MAX_PATH_GRANTS: usize = 128;
#[cfg(any(target_os = "macos", test))]
const MAX_NETWORK_GRANTS: usize = 128;
#[cfg(target_os = "macos")]
const MAX_GIT_HEAD_BYTES: u64 = 1024;
#[cfg(target_os = "macos")]
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
#[cfg(target_os = "macos")]
const PROBE_PROFILE: &str = "(version 1)\n(deny default)\n(allow process-exec)\n(allow file-read* (subpath \"/System/Library\"))\n(allow file-read* (subpath \"/System/Volumes/Preboot/Cryptexes/OS/System/Library\"))\n(allow file-read* (subpath \"/usr/bin\"))\n(allow file-read* (subpath \"/usr/lib\"))\n(allow file-read* (subpath \"/usr/libexec\"))\n(allow file-read* (subpath \"/bin\"))\n";
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
    #[error("path is not valid UTF-8 for the SBPL source format")]
    NonUtf8Path,
    #[error("profile or fixture record is malformed")]
    InvalidFixture,
    #[error("provider profile lacks required permission observations")]
    FixtureInsufficient,
    #[error("requested permission is not present in the fixture-backed profile")]
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
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NetworkGrant {
    Tcp { endpoint: NetworkEndpoint },
    UnixSocket { root: PathGrant },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    BrowserIpc,
    PostgresIpc,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityGrant {
    pub capability: Capability,
    #[serde(default)]
    pub mach_services: Vec<String>,
    #[serde(default)]
    pub unix_socket_roots: Vec<PathGrant>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthenticationProfile {
    pub mechanism: String,
    pub roots: Vec<PathGrant>,
}

/// Provider policy is declarative data. There are no provider-name branches in
/// the compiler; only the selected authentication mechanism is included.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderProfileSpec {
    /// Exact provider/runtime executables observed in this profile's recording.
    #[serde(default)]
    pub process_executables: Vec<PathGrant>,
    /// Forking is broad process authority and is enabled only by recorded data.
    #[serde(default)]
    pub allow_process_fork: bool,
    #[serde(default)]
    pub runtime_reads: Vec<PathGrant>,
    #[serde(default)]
    pub authentication: Vec<AuthenticationProfile>,
    #[serde(default)]
    pub continuation_reads: Vec<PathGrant>,
    #[serde(default)]
    pub provider_api: Vec<NetworkEndpoint>,
    #[serde(default)]
    pub network: Vec<NetworkGrant>,
    #[serde(default)]
    pub capabilities: Vec<CapabilityGrant>,
    #[serde(default)]
    pub native_controls: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "authority", rename_all = "snake_case")]
pub enum AccessMode {
    ReadOnly,
    WorktreeWrite(Box<WorktreeWriteAuthority>),
}

/// A digest binds the plan to Git grants derived at the verified worktree edge.
/// Neither the selected ref nor concrete Git paths enter the record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorktreeWriteAuthority {
    pub git_grants_digest: [u8; 32],
}

impl WorktreeWriteAuthority {
    pub fn for_worktree(
        worktree: &VerifiedWorktree,
        selected_ref: &GitRefName,
    ) -> Result<Self, PermissionError> {
        worktree.repo().revalidate()?;
        let grants = WorktreeWriteGrants::derive(worktree, selected_ref);
        grants.validate()?;
        Ok(Self {
            git_grants_digest: grants.digest(),
        })
    }
}

struct WorktreeWriteGrants {
    worktree: PathGrant,
    private_git_dir: PathGrant,
    common_git_dir: PathGrant,
    index: [PathGrant; 2],
    head: [PathGrant; 4],
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
            head: ["HEAD", "HEAD.lock", "logs/HEAD", "logs/HEAD.lock"]
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

    fn digest(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"provenant-worktree-git-grants-v1\0");
        for grant in std::iter::once(&self.worktree)
            .chain([&self.private_git_dir, &self.common_git_dir])
            .chain(self.index.iter())
            .chain(self.head.iter())
            .chain(std::iter::once(&self.objects))
            .chain(self.selected_ref.iter())
        {
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
        for grant in std::iter::once(&self.private_git_dir)
            .chain([&self.common_git_dir])
            .chain(self.index.iter())
            .chain(self.head.iter())
            .chain(std::iter::once(&self.objects))
            .chain(self.selected_ref.iter())
        {
            validate_noncredential_grant(grant)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanPermissions {
    pub mode: AccessMode,
    pub scratch: PathGrant,
    #[serde(default)]
    pub read_roots: Vec<PathGrant>,
    /// Readable control files that remain immutable during the attempt.
    #[serde(default)]
    pub protected_roots: Vec<PathGrant>,
    /// Sensitive roots that must not be read or written by the provider.
    #[serde(default)]
    pub secret_roots: Vec<PathGrant>,
    pub selected_authentication: Option<String>,
    pub include_continuation: bool,
    #[serde(default)]
    pub network: Vec<NetworkGrant>,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
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
    BrowserIpc,
    PostgresIpc,
}

/// Typed provider policy consumed by the permission compiler. Providers own
/// fixture loading and construct this value after their own checks.
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
                let parent = grant.path.parent().ok_or(PermissionError::InvalidPath)?;
                let name = grant.path.file_name().ok_or(PermissionError::InvalidPath)?;
                let anchor = self
                    .directories
                    .iter()
                    .find(|anchor| anchor.path == parent)
                    .ok_or(PermissionError::AnchorMismatch)?;
                anchor.verify(self.safe_fs)?;
                match anchor.directory.metadata_at(name, false) {
                    Ok(metadata) if metadata.kind == FileKind::Regular && metadata.links == 1 => {}
                    Ok(_) => return Err(PermissionError::AnchorMismatch),
                    Err(error) if error.is_not_found() => {}
                    Err(error) => return Err(PermissionError::Filesystem(error)),
                }
                Ok(())
            }
        }
    }

    fn verify_executable(&self, grant: &PathGrant) -> Result<(), PermissionError> {
        if grant.scope != PathScope::Exact {
            return Err(PermissionError::InvalidPath);
        }
        let parent = grant.path.parent().ok_or(PermissionError::InvalidPath)?;
        let name = grant.path.file_name().ok_or(PermissionError::InvalidPath)?;
        let anchor = self
            .directories
            .iter()
            .find(|anchor| anchor.path == parent)
            .ok_or(PermissionError::AnchorMismatch)?;
        anchor.verify(self.safe_fs)?;
        match anchor.directory.metadata_at(name, false) {
            Ok(metadata) if metadata.kind == FileKind::Regular => Ok(()),
            Ok(_) => Err(PermissionError::AnchorMismatch),
            Err(error) if error.is_not_found() => Err(PermissionError::AnchorMismatch),
            Err(error) => Err(PermissionError::Filesystem(error)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Enforcement {
    MacOsSandboxExec,
}

pub struct ConfinedLaunch {
    pub enforcement: Enforcement,
    pub profile: Vec<u8>,
    pub profile_digest: [u8; 32],
    pub native_controls: Vec<String>,
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

    let argv = [
        OsString::from(SANDBOX_EXEC),
        OsString::from("-p"),
        OsString::from(PROBE_PROFILE),
        OsString::from("/usr/bin/true"),
    ];
    let mut options = ExecOptions::new(&argv, Path::new("/"), PROBE_TIMEOUT);
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
        OsString::from(PROBE_PROFILE),
        OsString::from("/bin/cat"),
        OsString::from("/etc/passwd"),
    ];
    let mut denied_options = ExecOptions::new(&denied_argv, Path::new("/"), PROBE_TIMEOUT);
    denied_options.env = Some(context.env);
    denied_options.cancel = Some(context.cancel);
    denied_options.stdout = StreamPolicy::Capture {
        limit: PROBE_CAPTURE_LIMIT,
    };
    denied_options.stderr = StreamPolicy::Capture {
        limit: PROBE_CAPTURE_LIMIT,
    };
    denied_options.terminate_grace = CLEANUP_TIMEOUT;
    denied_options.reap_timeout = CLEANUP_TIMEOUT;
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
        && !outcome.timed_out
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
        || plan.network.len() > MAX_NETWORK_GRANTS
        || plan.capabilities.len() > 16
    {
        return Err(PermissionError::ProfileTooLarge);
    }
    validate_profile_spec(&profile.spec)?;
    verify_requested_evidence(plan, profile)?;
    for grant in plan.read_roots.iter().chain(&plan.protected_roots) {
        validate_noncredential_grant(grant)?;
    }
    let worktree_grants = match &plan.mode {
        AccessMode::ReadOnly => None,
        AccessMode::WorktreeWrite(authority) => {
            Some(verify_worktree_authority(authority, anchors)?)
        }
    };
    let mut read_grants = plan.read_roots.clone();
    if let Some(grants) = &worktree_grants {
        read_grants.push(grants.worktree.clone());
    }
    read_grants.extend(plan.protected_roots.iter().cloned());
    read_grants.extend(profile.spec.runtime_reads.iter().cloned());
    read_grants.extend(profile.spec.process_executables.iter().cloned());
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
    }
    if plan.include_continuation {
        read_grants.extend(profile.spec.continuation_reads.iter().cloned());
    }

    let selected_capabilities: BTreeSet<Capability> = plan.capabilities.iter().copied().collect();
    let profile_capabilities: BTreeSet<Capability> = profile
        .spec
        .capabilities
        .iter()
        .map(|grant| grant.capability)
        .collect();
    if !selected_capabilities.is_subset(&profile_capabilities) {
        return Err(PermissionError::PermissionNotRecorded);
    }
    if !is_network_subset(&plan.network, &profile.spec.network) {
        return Err(PermissionError::PermissionNotRecorded);
    }
    for grant in &read_grants {
        anchors.verify_grant_held(grant)?;
    }
    for executable in &profile.spec.process_executables {
        anchors.verify_executable(executable)?;
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
        .chain(
            profile
                .spec
                .authentication
                .iter()
                .flat_map(|authentication| &authentication.roots),
        )
        .collect();
    scratch_exclusions.extend(plan.network.iter().chain(&profile.spec.network).filter_map(
        |grant| match grant {
            NetworkGrant::UnixSocket { root } => Some(root),
            NetworkGrant::Tcp { .. } => None,
        },
    ));
    scratch_exclusions.extend(
        profile
            .spec
            .capabilities
            .iter()
            .filter(|grant| plan.capabilities.contains(&grant.capability))
            .flat_map(|grant| &grant.unix_socket_roots),
    );
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
            if path_overlaps(&plan.scratch.path, path) {
                return Err(PermissionError::ProtectedOverlap);
            }
        }
    }
    verify_private_scratch(&plan.scratch, &scratch_exclusions, anchors)?;
    for grant in &plan.network {
        verify_network_anchor(grant, anchors)?;
    }
    for capability in &profile.spec.capabilities {
        if selected_capabilities.contains(&capability.capability) {
            for root in &capability.unix_socket_roots {
                anchors.verify_grant_held(root)?;
            }
        }
    }

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

    let mut profile_bytes = render_profile(plan, profile, &read_grants, worktree_grants.as_ref())?;
    if profile_bytes.len() > PROFILE_LIMIT {
        return Err(PermissionError::ProfileTooLarge);
    }
    let profile_digest = Sha256::digest(&profile_bytes).into();
    // SBPL grants pathnames, so rebind them to their held identities after
    // rendering, immediately before returning the launch profile.
    if let AccessMode::WorktreeWrite(authority) = &plan.mode {
        verify_worktree_authority(authority, anchors)?;
    }
    verify_private_scratch(&plan.scratch, &scratch_exclusions, anchors)?;
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
    Ok(())
}

#[cfg(target_os = "macos")]
fn render_profile(
    plan: &PlanPermissions,
    profile: &ProviderProfile,
    reads: &[PathGrant],
    worktree_grants: Option<&WorktreeWriteGrants>,
) -> Result<Vec<u8>, PermissionError> {
    let mut source = String::from("(version 1)\n(deny default)\n");
    // These are operating-system runtime roots; provider state and user paths
    // are granted only from the verified plan/profile data below.
    for path in [
        "/System/Library",
        "/System/Volumes/Preboot/Cryptexes/OS/System/Library",
        "/usr/bin",
        "/usr/lib",
        "/usr/libexec",
        "/bin",
    ] {
        push_rule(
            &mut source,
            "allow",
            "file-read*",
            Path::new(path),
            PathScope::Subtree,
        )?;
    }
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
    if profile.spec.allow_process_fork {
        source.push_str("(allow process-fork)\n");
    }
    push_rule(
        &mut source,
        "allow",
        "file-read*",
        &plan.scratch.path,
        PathScope::Subtree,
    )?;
    push_rule(
        &mut source,
        "allow",
        "file-write*",
        &plan.scratch.path,
        PathScope::Subtree,
    )?;

    if matches!(plan.mode, AccessMode::WorktreeWrite(_)) {
        push_worktree_rule(
            &mut source,
            worktree_grants.ok_or(PermissionError::GitWriteAuthorityMissing)?,
        )?;
    }

    for endpoint in &profile.spec.provider_api {
        push_network_endpoint(&mut source, endpoint)?;
    }
    for grant in &plan.network {
        match grant {
            NetworkGrant::Tcp { endpoint } => {
                push_network_endpoint(&mut source, endpoint)?;
            }
            NetworkGrant::UnixSocket { root } => {
                let path = sbpl_path(&root.path)?;
                let filter = match root.scope {
                    PathScope::Exact => "path-literal",
                    PathScope::Subtree => "subpath",
                };
                source.push_str(&format!(
                    "(allow network-outbound (remote unix-socket ({filter} \"{path}\")))\n"
                ));
            }
        }
    }
    for grant in &profile.spec.capabilities {
        if !plan.capabilities.contains(&grant.capability) {
            continue;
        }
        for service in &grant.mach_services {
            if service.is_empty()
                || service
                    .bytes()
                    .any(|byte| byte.is_ascii_control() || byte == b'"' || byte == b'\\')
            {
                return Err(PermissionError::InvalidPath);
            }
            source.push_str(&format!(
                "(allow mach-lookup (global-name \"{service}\"))\n"
            ));
        }
        for root in &grant.unix_socket_roots {
            let path = sbpl_path(&root.path)?;
            let filter = match root.scope {
                PathScope::Exact => "path-literal",
                PathScope::Subtree => "subpath",
            };
            source.push_str(&format!(
                "(allow network-outbound (remote unix-socket ({filter} \"{path}\")))\n"
            ));
        }
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
    source.push_str(&format!(
        "(allow file-write* (require-all (subpath \"{worktree}\") (require-not (subpath \"{git_dir}\")) (require-not (subpath \"{common_dir}\"))))\n"
    ));
    for grant in grants
        .index
        .iter()
        .chain(&grants.head)
        .chain(&grants.selected_ref)
    {
        push_rule(source, "allow", "file-write*", &grant.path, grant.scope)?;
    }
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
    if grants.digest() != authority.git_grants_digest {
        return Err(PermissionError::GitWriteAuthorityMissing);
    }
    verify_selected_head(verified, selected_ref.short_name())?;
    for grant in std::iter::once(&grants.worktree)
        .chain([&grants.private_git_dir, &grants.common_git_dir])
        .chain(grants.index.iter())
        .chain(grants.head.iter())
        .chain(std::iter::once(&grants.objects))
        .chain(grants.selected_ref.iter())
    {
        anchors.verify_grant_held(grant)?;
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

#[cfg(target_os = "macos")]
fn verify_requested_evidence(
    plan: &PlanPermissions,
    profile: &ProviderProfile,
) -> Result<(), PermissionError> {
    let denied = FixtureOutcome::Denied;
    let allowed = FixtureOutcome::Allowed;
    let mut required = vec![
        (FixtureOperation::DeclaredRead, allowed),
        (FixtureOperation::UndeclaredRead, denied),
        (FixtureOperation::UndeclaredWrite, denied),
        (FixtureOperation::SecretRead, denied),
        (FixtureOperation::ProtectedWrite, denied),
        (FixtureOperation::OtherNetwork, denied),
        (FixtureOperation::DeclaredProcessExec, allowed),
        (FixtureOperation::UndeclaredProcessExec, denied),
        (
            FixtureOperation::ProcessFork,
            if profile.spec.allow_process_fork {
                allowed
            } else {
                denied
            },
        ),
    ];
    if !profile.spec.runtime_reads.is_empty() {
        required.push((FixtureOperation::RuntimeRead, allowed));
    }
    if plan.selected_authentication.is_some() {
        required.push((FixtureOperation::AuthenticationRead, allowed));
    }
    if plan.include_continuation {
        required.push((FixtureOperation::ContinuationRead, allowed));
    }
    if !plan.protected_roots.is_empty() {
        required.push((FixtureOperation::ProtectedInstructionRead, allowed));
    }
    if !plan.network.is_empty() || !profile.spec.provider_api.is_empty() {
        required.push((FixtureOperation::ProviderEndpoint, allowed));
    }
    required.extend(plan.capabilities.iter().map(|capability| {
        let operation = match capability {
            Capability::BrowserIpc => FixtureOperation::BrowserIpc,
            Capability::PostgresIpc => FixtureOperation::PostgresIpc,
        };
        (operation, allowed)
    }));
    if matches!(plan.mode, AccessMode::WorktreeWrite(_)) {
        required.extend([
            (FixtureOperation::WorktreeWrite, allowed),
            (FixtureOperation::GitIndexWrite, allowed),
            (FixtureOperation::GitHeadWrite, allowed),
            (FixtureOperation::GitObjectsWrite, allowed),
            (FixtureOperation::SelectedRefWrite, allowed),
            (FixtureOperation::GitConfigWrite, denied),
            (FixtureOperation::OtherRefWrite, denied),
            (FixtureOperation::ProtectedWrite, denied),
        ]);
    }
    for (operation, outcome) in required {
        require_observation(&profile.observations, operation, outcome)?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn require_observation(
    observations: &BTreeMap<FixtureOperation, FixtureOutcome>,
    operation: FixtureOperation,
    expected: FixtureOutcome,
) -> Result<(), PermissionError> {
    match observations.get(&operation) {
        Some(actual) if *actual == expected => Ok(()),
        _ => Err(PermissionError::FixtureInsufficient),
    }
}

#[cfg(any(target_os = "macos", test))]
fn validate_profile_spec(spec: &ProviderProfileSpec) -> Result<(), PermissionError> {
    if spec.process_executables.len() > MAX_PATH_GRANTS
        || spec.runtime_reads.len() > MAX_PATH_GRANTS
        || spec.authentication.len() > 16
        || spec.continuation_reads.len() > MAX_PATH_GRANTS
        || spec.provider_api.len() > MAX_NETWORK_GRANTS
        || spec.network.len() > MAX_NETWORK_GRANTS
        || spec.capabilities.len() > 16
        || spec.native_controls.len() > 64
    {
        return Err(PermissionError::InvalidFixture);
    }
    if spec.process_executables.is_empty() {
        return Err(PermissionError::ProcessPolicyMissing);
    }
    for executable in &spec.process_executables {
        validate_noncredential_grant(executable)?;
        if executable.scope != PathScope::Exact {
            return Err(PermissionError::InvalidPath);
        }
    }
    for grant in &spec.runtime_reads {
        validate_noncredential_grant(grant)?;
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
        if auth.roots.len() > MAX_PATH_GRANTS {
            return Err(PermissionError::InvalidFixture);
        }
    }
    for grant in &spec.continuation_reads {
        validate_noncredential_grant(grant)?;
    }
    for endpoint in &spec.provider_api {
        validate_endpoint(endpoint)?;
    }
    for grant in &spec.network {
        validate_network_grant(grant)?;
    }
    for capability in &spec.capabilities {
        if capability.mach_services.iter().any(|service| {
            service.is_empty()
                || service.len() > 256
                || service
                    .bytes()
                    .any(|byte| byte.is_ascii_control() || byte == b'"' || byte == b'\\')
        }) || capability.mach_services.len() > 128
            || capability.unix_socket_roots.len() > MAX_PATH_GRANTS
        {
            return Err(PermissionError::InvalidFixture);
        }
        for grant in &capability.unix_socket_roots {
            validate_noncredential_grant(grant)?;
            if grant.scope != PathScope::Subtree {
                return Err(PermissionError::InvalidPath);
            }
        }
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
        ".ssh", ".aws", ".azure", ".gnupg", ".codex", ".claude", ".cursor",
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
    }) || components.windows(3).any(|triple| {
        triple[0] == OsStr::new("Library")
            && triple[1] == OsStr::new("Application Support")
            && triple[2] == OsStr::new("GitHub CLI")
    })
}

fn path_overlaps(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

#[cfg(target_os = "macos")]
fn is_network_subset(requested: &[NetworkGrant], allowed: &[NetworkGrant]) -> bool {
    requested.iter().all(|item| allowed.contains(item))
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

#[cfg(any(target_os = "macos", test))]
fn validate_network_grant(grant: &NetworkGrant) -> Result<(), PermissionError> {
    match grant {
        NetworkGrant::Tcp { endpoint } => validate_endpoint(endpoint),
        NetworkGrant::UnixSocket { root } => {
            validate_noncredential_grant(root)?;
            if root.scope != PathScope::Subtree {
                return Err(PermissionError::InvalidPath);
            }
            Ok(())
        }
    }
}

#[cfg(target_os = "macos")]
fn verify_network_anchor(
    grant: &NetworkGrant,
    anchors: &Anchors<'_>,
) -> Result<(), PermissionError> {
    match grant {
        NetworkGrant::Tcp { .. } => Ok(()),
        NetworkGrant::UnixSocket { root } => anchors.verify_grant_held(root),
    }
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
        for path in ["/Users/alice/.ssh", "/Users/alice/.config/gh"] {
            let grant = PathGrant {
                path: PathBuf::from(path),
                scope: PathScope::Subtree,
            };
            assert!(matches!(
                validate_noncredential_grant(&grant),
                Err(PermissionError::CredentialRoot)
            ));
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
        let spec = ProviderProfileSpec {
            process_executables: Vec::new(),
            allow_process_fork: false,
            runtime_reads: Vec::new(),
            authentication: Vec::new(),
            continuation_reads: Vec::new(),
            provider_api: Vec::new(),
            network: Vec::new(),
            capabilities: Vec::new(),
            native_controls: Vec::new(),
        };
        assert!(matches!(
            validate_profile_spec(&spec),
            Err(PermissionError::ProcessPolicyMissing)
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn serialised_digest_cannot_authorise_without_a_verified_worktree() {
        let authority = WorktreeWriteAuthority {
            git_grants_digest: [0; 32],
        };
        let safe_fs = LocalFs;
        let directories = [];
        let anchors = Anchors {
            directories: &directories,
            safe_fs: &safe_fs,
            worktree: None,
        };
        assert!(matches!(
            verify_worktree_authority(&authority, &anchors),
            Err(PermissionError::GitWriteAuthorityMissing)
        ));
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

        let authority = WorktreeWriteAuthority::for_worktree(&linked, &selected_ref)?;
        let grants = WorktreeWriteGrants::derive(&linked, &selected_ref);
        assert_eq!(authority.git_grants_digest, grants.digest());
        assert_eq!(grants.objects.path, common.join("objects"));
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
            common.join("objects"),
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
            directories: &directories,
            safe_fs: &safe_fs,
            worktree: Some((&linked, &selected_ref)),
        };
        let verified_grants = verify_worktree_authority(&authority, &anchors)?;
        let mut source = String::new();
        push_worktree_rule(&mut source, &verified_grants)?;
        assert!(source.contains("(require-not (subpath "));
        assert!(source.contains(&format!("{}/objects/info", common.display())));

        let other_ref = GitRefName::branch("s3/other")?;
        let mismatched = Anchors {
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
                &WorktreeWriteAuthority::for_worktree(&linked, &selected_ref)?,
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
        for path in [&home, &state, &xdg] {
            std::fs::create_dir_all(path)?;
        }
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
        let authority = WorktreeWriteAuthority::for_worktree(&linked, &selected_ref)?;
        let grants = WorktreeWriteGrants::derive(&linked, &selected_ref);

        let scratch = root.join("scratch");
        std::fs::create_dir(&scratch)?;
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
        let hardlink_path = linked.path().join("hardlink");
        std::fs::hard_link(&protected_file, &hardlink_path)?;

        let profile = ProviderProfile {
            spec: ProviderProfileSpec {
                process_executables: ["/usr/bin/touch", "/usr/bin/mv"]
                    .map(|path| PathGrant {
                        path: PathBuf::from(path),
                        scope: PathScope::Exact,
                    })
                    .to_vec(),
                allow_process_fork: false,
                runtime_reads: Vec::new(),
                authentication: Vec::new(),
                continuation_reads: Vec::new(),
                provider_api: Vec::new(),
                network: Vec::new(),
                capabilities: Vec::new(),
                native_controls: Vec::new(),
            },
            observations: BTreeMap::new(),
        };
        let plan = PlanPermissions {
            mode: AccessMode::WorktreeWrite(Box::new(authority)),
            scratch: PathGrant {
                path: scratch,
                scope: PathScope::Subtree,
            },
            read_roots: vec![PathGrant {
                path: root.clone(),
                scope: PathScope::Subtree,
            }],
            protected_roots: [protected.clone(), nested_git.clone()]
                .map(|path| PathGrant {
                    path,
                    scope: PathScope::Subtree,
                })
                .to_vec(),
            secret_roots: Vec::new(),
            selected_authentication: None,
            include_continuation: false,
            network: Vec::new(),
            capabilities: Vec::new(),
            nested_confinement: false,
        };
        let sbpl = String::from_utf8(render_profile(
            &plan,
            &profile,
            &plan.read_roots,
            Some(&grants),
        )?)?;
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
        let run = |program: &str, args: &[&Path]| {
            let mut argv = vec![
                OsString::from(SANDBOX_EXEC),
                OsString::from("-p"),
                OsString::from(&sbpl),
                OsString::from(program),
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

        let inside_outcome = run("/usr/bin/touch", &[&inside]);
        assert!(
            successful_probe(&inside_outcome),
            "worktree write failed: {:?}; stderr={}",
            inside_outcome.termination,
            String::from_utf8_lossy(&inside_outcome.stderr.bytes)
        );
        assert!(denied_probe(&run("/usr/bin/touch", &[&protected_file])));
        assert!(denied_probe(&run("/usr/bin/touch", &[&symlink_path])));
        assert!(denied_probe(&run("/usr/bin/touch", &[&hardlink_path])));
        assert!(denied_probe(&run(
            "/usr/bin/touch",
            &[&nested_git.join("config")],
        )));
        let moved_protected_parent = root.join("protected-parent-moved");
        assert!(denied_probe(&run(
            "/usr/bin/mv",
            &[&protected_parent, &moved_protected_parent]
        )));
        let moved_worktree = root.join("linked-moved");
        assert!(denied_probe(&run(
            "/usr/bin/mv",
            &[&worktree_path, &moved_worktree]
        )));
        assert!(protected_parent.exists() && worktree_path.exists());

        // Simulate a root swap in the gap between compiling the pathname
        // policy and starting its child: the authorised worktree spelling now
        // resolves to the protected directory.
        std::fs::rename(&worktree_path, &moved_worktree)?;
        symlink(&protected, &worktree_path)?;
        let swapped_probe = worktree_path.join("root-swap-probe");
        assert!(denied_probe(&run("/usr/bin/touch", &[&swapped_probe])));
        assert!(!protected.join("root-swap-probe").exists());
        std::fs::remove_file(&worktree_path)?;
        std::fs::rename(&moved_worktree, &worktree_path)?;
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
            runtime_reads: Vec::new(),
            authentication: Vec::new(),
            continuation_reads: Vec::new(),
            provider_api: Vec::new(),
            network: Vec::new(),
            capabilities: Vec::new(),
            native_controls: Vec::new(),
        };
        let profile = ProviderProfile {
            spec: spec.clone(),
            observations: BTreeMap::new(),
        };
        let plan = PlanPermissions {
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
            network: Vec::new(),
            capabilities: Vec::new(),
            nested_confinement: false,
        };
        let rendered = String::from_utf8(render_profile(&plan, &profile, &[], None).expect("SBPL"))
            .expect("UTF-8 SBPL");
        assert!(rendered.contains("(allow process-exec (literal \"/usr/bin/true\"))"));
        assert!(rendered.contains("(allow process-fork)"));

        spec.allow_process_fork = false;
        let profile = ProviderProfile {
            spec,
            observations: BTreeMap::new(),
        };
        let rendered = String::from_utf8(render_profile(&plan, &profile, &[], None).expect("SBPL"))
            .expect("UTF-8 SBPL");
        assert!(!rendered.contains("(allow process-fork)"));
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
}
