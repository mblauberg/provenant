//! Typed permission plans and the fail-closed macOS SBPL backend.
//!
//! A provider profile is usable only after its checked fixture records the
//! relevant allow and deny observations. The current tree has no sandbox
//! operation recordings, so callers cannot construct a supported real-provider
//! profile yet. Linux deliberately exposes an unavailable backend.
use std::collections::BTreeMap;
#[cfg(target_os = "macos")]
use std::collections::BTreeSet;
use std::ffi::OsStr;
#[cfg(target_os = "macos")]
use std::ffi::OsString;
use std::io::Read;
use std::net::IpAddr;
use std::path::{Component, Path, PathBuf};
#[cfg(target_os = "macos")]
use std::time::Duration;

use provenant_core::CancelToken;
use provenant_supervisor::exec::Executor;
#[cfg(target_os = "macos")]
use provenant_supervisor::exec::{ExecOptions, ExecOutcome, StreamPolicy, Termination};
#[cfg(target_os = "macos")]
use provenant_supervisor::fs::Metadata;
use provenant_supervisor::fs::{FileKind, SafeDir, SafeFs};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

const FIXTURE_LIMIT: u64 = 1024 * 1024;
const MAX_PATH_BYTES: usize = 4096;
const MAX_PATH_GRANTS: usize = 128;
const MAX_NETWORK_GRANTS: usize = 128;
#[cfg(target_os = "macos")]
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
#[cfg(target_os = "macos")]
const PROBE_PROFILE: &str = "(version 1)\n(deny default)\n(allow process-exec)\n(allow file-read* (subpath \"/System/Library\"))\n(allow file-read* (subpath \"/usr/bin\"))\n(allow file-read* (subpath \"/usr/lib\"))\n(allow file-read* (subpath \"/usr/libexec\"))\n(allow file-read* (subpath \"/bin\"))\n";
#[cfg(target_os = "macos")]
const PROFILE_LIMIT: usize = 64 * 1024;
#[cfg(target_os = "macos")]
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(target_os = "macos")]
const CLEANUP_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    MacOsSbpl,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostPlatform {
    MacOs,
    Linux,
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
    #[error("permission fixture is missing required allow/deny observations")]
    FixtureInsufficient,
    #[error("permission fixture does not match the requested provider profile")]
    FixtureMismatch,
    #[error("no recorded permission fixture is available for this profile")]
    MissingFixture,
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
    pub address: IpAddr,
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
    pub profile_id: String,
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
    WorktreeWrite(WorktreeWriteAuthority),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitWriteAuthority {
    pub private_git_dir: PathGrant,
    pub index: Vec<PathGrant>,
    pub head: Vec<PathGrant>,
    pub objects: Vec<PathGrant>,
    pub selected_ref: Vec<PathGrant>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorktreeWriteAuthority {
    pub worktree: PathGrant,
    pub git: GitWriteAuthority,
    pub selected_branch: String,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureObservation {
    pub operation: FixtureOperation,
    pub outcome: FixtureOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionFixture {
    pub schema_version: u64,
    pub profile_id: String,
    pub platform: HostPlatform,
    pub backend: BackendKind,
    pub observations: Vec<FixtureObservation>,
}

/// An immutable profile carries proof that its fixture bytes came from a
/// bounded SafeDir read. Its fields are private to prevent unverified profiles
/// from being assembled by ordinary callers.
pub struct ProviderProfile {
    spec: ProviderProfileSpec,
    fixture_digest: [u8; 32],
    observations: BTreeMap<FixtureOperation, FixtureOutcome>,
}

impl ProviderProfile {
    pub fn load(
        safe_fs: &dyn SafeFs,
        fixture_root: &Path,
        fixture_path: &Path,
        spec: ProviderProfileSpec,
    ) -> Result<Self, PermissionError> {
        validate_profile_spec(&spec)?;
        let root = safe_fs
            .open_anchor(fixture_root)
            .map_err(PermissionError::Filesystem)?;
        let bytes = read_relative(root.as_ref(), fixture_path)?;
        let fixture: PermissionFixture =
            serde_json::from_slice(&bytes).map_err(|_| PermissionError::InvalidFixture)?;
        if fixture.schema_version != 1
            || fixture.profile_id != spec.profile_id
            || fixture.platform != HostPlatform::MacOs
            || fixture.backend != BackendKind::MacOsSbpl
        {
            return Err(PermissionError::FixtureMismatch);
        }
        let mut observations = BTreeMap::new();
        for observation in fixture.observations {
            if observations
                .insert(observation.operation, observation.outcome)
                .is_some()
            {
                return Err(PermissionError::InvalidFixture);
            }
        }
        require_observation(
            &observations,
            FixtureOperation::UndeclaredRead,
            FixtureOutcome::Denied,
        )?;
        require_observation(
            &observations,
            FixtureOperation::UndeclaredWrite,
            FixtureOutcome::Denied,
        )?;
        require_observation(
            &observations,
            FixtureOperation::SecretRead,
            FixtureOutcome::Denied,
        )?;
        require_observation(
            &observations,
            FixtureOperation::ProtectedWrite,
            FixtureOutcome::Denied,
        )?;
        require_observation(
            &observations,
            FixtureOperation::OtherNetwork,
            FixtureOutcome::Denied,
        )?;
        require_observation(
            &observations,
            FixtureOperation::DeclaredProcessExec,
            FixtureOutcome::Allowed,
        )?;
        require_observation(
            &observations,
            FixtureOperation::UndeclaredProcessExec,
            FixtureOutcome::Denied,
        )?;
        require_observation(
            &observations,
            FixtureOperation::ProcessFork,
            if spec.allow_process_fork {
                FixtureOutcome::Allowed
            } else {
                FixtureOutcome::Denied
            },
        )?;
        Ok(Self {
            spec,
            fixture_digest: Sha256::digest(&bytes).into(),
            observations,
        })
    }

    pub fn profile_id(&self) -> &str {
        &self.spec.profile_id
    }

    pub fn fixture_digest(&self) -> [u8; 32] {
        self.fixture_digest
    }
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
    fn verify(&self) -> Result<(), PermissionError> {
        let current = self
            .directory
            .metadata()
            .map_err(PermissionError::Filesystem)?;
        if current.kind != FileKind::Directory || !current.same_identity(&self.identity) {
            return Err(PermissionError::AnchorMismatch);
        }
        Ok(())
    }
}

pub struct Anchors<'a> {
    pub directories: &'a [AnchoredDirectory],
}

#[cfg(target_os = "macos")]
impl Anchors<'_> {
    fn verify_grant(&self, grant: &PathGrant) -> Result<(), PermissionError> {
        grant.validate()?;
        match grant.scope {
            PathScope::Subtree => {
                let anchor = self
                    .directories
                    .iter()
                    .find(|anchor| anchor.path == grant.path)
                    .ok_or(PermissionError::AnchorMismatch)?;
                anchor.verify()
            }
            PathScope::Exact => {
                let parent = grant.path.parent().ok_or(PermissionError::InvalidPath)?;
                let name = grant.path.file_name().ok_or(PermissionError::InvalidPath)?;
                let anchor = self
                    .directories
                    .iter()
                    .find(|anchor| anchor.path == parent)
                    .ok_or(PermissionError::AnchorMismatch)?;
                anchor.verify()?;
                match anchor.directory.metadata_at(name, false) {
                    Ok(metadata) if metadata.kind == FileKind::Regular => {}
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
        self.verify_grant(grant)?;
        let parent = grant.path.parent().ok_or(PermissionError::InvalidPath)?;
        let name = grant.path.file_name().ok_or(PermissionError::InvalidPath)?;
        let anchor = self
            .directories
            .iter()
            .find(|anchor| anchor.path == parent)
            .ok_or(PermissionError::AnchorMismatch)?;
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
    if context.cancel.is_cancelled() {
        return Err(PermissionError::Cancelled);
    }
    let executable = match context.safe_fs.open_source(Path::new(SANDBOX_EXEC)) {
        Ok(executable) => executable,
        Err(error) if error.is_not_found() => {
            return Ok(Availability::Unavailable {
                reason: AvailabilityReason::SandboxExecutableMissing,
            });
        }
        Err(error) => return Err(PermissionError::Filesystem(error)),
    };
    let metadata = executable.metadata().map_err(PermissionError::Filesystem)?;
    if metadata.kind != FileKind::Regular
        || metadata.uid != 0
        || metadata.mode & 0o111 == 0
        || metadata.mode & 0o022 != 0
    {
        return Ok(Availability::Unavailable {
            reason: AvailabilityReason::SandboxExecutableUntrusted,
        });
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
    options.stdout = StreamPolicy::Discard;
    options.stderr = StreamPolicy::Discard;
    options.terminate_grace = CLEANUP_TIMEOUT;
    options.reap_timeout = CLEANUP_TIMEOUT;
    match context.executor.execute(options) {
        Ok(outcome) if successful_probe(&outcome) => (),
        Err(provenant_supervisor::exec::ExecError::Cancelled) => {
            return Err(PermissionError::Cancelled);
        }
        Err(error) => return Err(PermissionError::Execution(error)),
        Ok(_) => {
            return Ok(Availability::Unavailable {
                reason: AvailabilityReason::ProbeFailed,
            });
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
    denied_options.stdout = StreamPolicy::Discard;
    denied_options.stderr = StreamPolicy::Discard;
    denied_options.terminate_grace = CLEANUP_TIMEOUT;
    denied_options.reap_timeout = CLEANUP_TIMEOUT;
    match context.executor.execute(denied_options) {
        Ok(outcome) if denied_probe(&outcome) => Ok(Availability::Available {
            backend: BackendKind::MacOsSbpl,
        }),
        Err(provenant_supervisor::exec::ExecError::Cancelled) => Err(PermissionError::Cancelled),
        Err(error) => Err(PermissionError::Execution(error)),
        Ok(_) => Ok(Availability::Unavailable {
            reason: AvailabilityReason::ProbeFailed,
        }),
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
    if profile.spec.process_executables.is_empty() {
        return Err(PermissionError::ProcessPolicyMissing);
    }

    let mut read_grants = plan.read_roots.clone();
    read_grants.extend(plan.protected_roots.iter().cloned());
    read_grants.extend(profile.spec.runtime_reads.iter().cloned());
    read_grants.extend(profile.spec.process_executables.iter().cloned());
    let mut selected_authentication_roots = Vec::new();
    if let Some(mechanism) = &plan.selected_authentication {
        let selected = profile
            .spec
            .authentication
            .iter()
            .find(|item| &item.mechanism == mechanism)
            .ok_or(PermissionError::PermissionNotRecorded)?;
        for grant in &selected.roots {
            grant.validate()?;
            selected_authentication_roots.push(grant.clone());
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
        if selected_authentication_roots.contains(grant) {
            grant.validate()?;
        } else {
            validate_noncredential_grant(grant)?;
        }
        anchors.verify_grant(grant)?;
    }
    for executable in &profile.spec.process_executables {
        anchors.verify_executable(executable)?;
    }
    plan.scratch.validate()?;
    if is_credential_root(&plan.scratch.path) {
        return Err(PermissionError::CredentialRoot);
    }
    if plan.scratch.scope != PathScope::Subtree {
        return Err(PermissionError::InvalidPath);
    }
    anchors.verify_grant(&plan.scratch)?;
    for endpoint in &profile.spec.provider_api {
        validate_endpoint(endpoint)?;
    }
    for grant in plan.network.iter().chain(&profile.spec.network) {
        validate_network_grant(grant)?;
    }
    for grant in &plan.network {
        if !profile.spec.network.contains(grant) {
            return Err(PermissionError::PermissionNotRecorded);
        }
        verify_network_anchor(grant, anchors)?;
    }
    for capability in &profile.spec.capabilities {
        if selected_capabilities.contains(&capability.capability) {
            for root in &capability.unix_socket_roots {
                anchors.verify_grant(root)?;
            }
        }
    }

    for secret in &plan.secret_roots {
        secret.validate()?;
        anchors.verify_grant(secret)?;
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
    if let AccessMode::WorktreeWrite(authority) = &plan.mode {
        verify_worktree_authority(authority, anchors)?;
        if plan
            .protected_roots
            .iter()
            .chain(&plan.secret_roots)
            .any(|protected| {
                authority.worktree.path.starts_with(&protected.path)
                    && authority.worktree.path != protected.path
            })
        {
            return Err(PermissionError::ProtectedOverlap);
        }
    }

    let mut profile_bytes = render_profile(plan, profile, &read_grants)?;
    if profile_bytes.len() > PROFILE_LIMIT {
        return Err(PermissionError::ProfileTooLarge);
    }
    let profile_digest = Sha256::digest(&profile_bytes).into();
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
fn render_profile(
    plan: &PlanPermissions,
    profile: &ProviderProfile,
    reads: &[PathGrant],
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
        "file-write*",
        &plan.scratch.path,
        PathScope::Subtree,
    )?;

    if let AccessMode::WorktreeWrite(authority) = &plan.mode {
        validate_git_authority(authority)?;
        push_worktree_rule(&mut source, authority)?;
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
    if let AccessMode::WorktreeWrite(authority) = &plan.mode {
        let git_marker = authority.worktree.path.join(".git");
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
    authority: &WorktreeWriteAuthority,
) -> Result<(), PermissionError> {
    let worktree = sbpl_path(&authority.worktree.path)?;
    let git_dir = sbpl_path(&authority.git.private_git_dir.path)?;
    source.push_str(&format!(
        "(allow file-write* (require-not (subpath \"{git_dir}\")) (subpath \"{worktree}\"))\n"
    ));
    for grant in authority
        .git
        .index
        .iter()
        .chain(&authority.git.head)
        .chain(&authority.git.selected_ref)
    {
        push_rule(source, "allow", "file-write*", &grant.path, grant.scope)?;
    }
    for grant in &authority.git.objects {
        let path = sbpl_path(&grant.path)?;
        source.push_str(&format!(
            "(allow file-write* (require-not (subpath \"{path}/info\")) (subpath \"{path}\"))\n"
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn validate_git_authority(authority: &WorktreeWriteAuthority) -> Result<(), PermissionError> {
    authority.worktree.validate()?;
    authority.git.private_git_dir.validate()?;
    if authority.worktree.scope != PathScope::Subtree
        || authority.git.private_git_dir.scope != PathScope::Subtree
        || path_overlaps(
            &authority.worktree.path,
            &authority.git.private_git_dir.path,
        ) && authority.git.private_git_dir.path != authority.worktree.path.join(".git")
        || authority.git.index.is_empty()
        || authority.git.head.is_empty()
        || authority.git.objects.len() != 1
        || authority.git.selected_ref.len() != 4
        || !valid_branch(&authority.selected_branch)
    {
        return Err(PermissionError::GitWriteAuthorityMissing);
    }
    for grant in authority
        .git
        .index
        .iter()
        .chain(&authority.git.head)
        .chain(&authority.git.objects)
        .chain(&authority.git.selected_ref)
    {
        grant.validate()?;
        if !grant.path.starts_with(&authority.git.private_git_dir.path) {
            return Err(PermissionError::GitWriteAuthorityMissing);
        }
    }
    let index_paths =
        ["index", "index.lock"].map(|name| authority.git.private_git_dir.path.join(name));
    if !exact_grant_paths(&authority.git.index, &index_paths) {
        return Err(PermissionError::GitWriteAuthorityMissing);
    }
    let head_paths = ["HEAD", "HEAD.lock", "logs/HEAD", "logs/HEAD.lock"]
        .map(|name| authority.git.private_git_dir.path.join(name));
    if !exact_grant_paths(&authority.git.head, &head_paths) {
        return Err(PermissionError::GitWriteAuthorityMissing);
    }
    for grant in &authority.git.objects {
        if grant.scope != PathScope::Subtree
            || grant.path != authority.git.private_git_dir.path.join("objects")
        {
            return Err(PermissionError::GitWriteAuthorityMissing);
        }
    }
    let branch = &authority.selected_branch;
    let selected_paths = [
        format!("refs/heads/{branch}"),
        format!("refs/heads/{branch}.lock"),
        format!("logs/refs/heads/{branch}"),
        format!("logs/refs/heads/{branch}.lock"),
    ]
    .map(|relative| authority.git.private_git_dir.path.join(relative));
    if !exact_grant_paths(&authority.git.selected_ref, &selected_paths) {
        return Err(PermissionError::GitWriteAuthorityMissing);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn exact_grant_paths<const N: usize>(grants: &[PathGrant], paths: &[PathBuf; N]) -> bool {
    grants.len() == N
        && grants
            .iter()
            .all(|grant| grant.scope == PathScope::Exact && paths.contains(&grant.path))
        && paths
            .iter()
            .all(|path| grants.iter().any(|grant| &grant.path == path))
}

#[cfg(target_os = "macos")]
fn valid_branch(branch: &str) -> bool {
    !branch.is_empty()
        && branch.len() <= 255
        && !branch.starts_with('/')
        && !branch.ends_with('/')
        && !branch.ends_with('.')
        && !branch.contains("..")
        && !branch.contains("@{")
        && branch.split('/').all(|part| {
            !part.is_empty()
                && !part.starts_with('.')
                && !part.ends_with(".lock")
                && !part.ends_with('.')
                && part != "@"
                && !part.bytes().any(|byte| {
                    byte.is_ascii_control()
                        || byte.is_ascii_whitespace()
                        || b"~^:?*[]\\".contains(&byte)
                })
        })
}

#[cfg(target_os = "macos")]
fn verify_worktree_authority(
    authority: &WorktreeWriteAuthority,
    anchors: &Anchors<'_>,
) -> Result<(), PermissionError> {
    validate_git_authority(authority)?;
    anchors.verify_grant(&authority.worktree)?;
    anchors.verify_grant(&authority.git.private_git_dir)?;
    for grant in authority
        .git
        .index
        .iter()
        .chain(&authority.git.head)
        .chain(&authority.git.objects)
        .chain(&authority.git.selected_ref)
    {
        anchors.verify_grant(grant)?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn verify_requested_evidence(
    plan: &PlanPermissions,
    profile: &ProviderProfile,
) -> Result<(), PermissionError> {
    for (operation, outcome) in [
        (FixtureOperation::DeclaredRead, FixtureOutcome::Allowed),
        (FixtureOperation::UndeclaredRead, FixtureOutcome::Denied),
        (FixtureOperation::UndeclaredWrite, FixtureOutcome::Denied),
        (FixtureOperation::SecretRead, FixtureOutcome::Denied),
        (FixtureOperation::ProtectedWrite, FixtureOutcome::Denied),
        (FixtureOperation::OtherNetwork, FixtureOutcome::Denied),
    ] {
        require_observation(&profile.observations, operation, outcome)?;
    }
    if !profile.spec.runtime_reads.is_empty() {
        require_observation(
            &profile.observations,
            FixtureOperation::RuntimeRead,
            FixtureOutcome::Allowed,
        )?;
    }
    if plan.selected_authentication.is_some() {
        require_observation(
            &profile.observations,
            FixtureOperation::AuthenticationRead,
            FixtureOutcome::Allowed,
        )?;
    }
    if plan.include_continuation {
        require_observation(
            &profile.observations,
            FixtureOperation::ContinuationRead,
            FixtureOutcome::Allowed,
        )?;
    }
    if !plan.protected_roots.is_empty() {
        require_observation(
            &profile.observations,
            FixtureOperation::ProtectedInstructionRead,
            FixtureOutcome::Allowed,
        )?;
    }
    if !plan.secret_roots.is_empty() {
        require_observation(
            &profile.observations,
            FixtureOperation::SecretRead,
            FixtureOutcome::Denied,
        )?;
    }
    if !plan.network.is_empty() {
        require_observation(
            &profile.observations,
            FixtureOperation::ProviderEndpoint,
            FixtureOutcome::Allowed,
        )?;
    }
    if !profile.spec.provider_api.is_empty() {
        require_observation(
            &profile.observations,
            FixtureOperation::ProviderEndpoint,
            FixtureOutcome::Allowed,
        )?;
    }
    for capability in &plan.capabilities {
        let operation = match capability {
            Capability::BrowserIpc => FixtureOperation::BrowserIpc,
            Capability::PostgresIpc => FixtureOperation::PostgresIpc,
        };
        require_observation(&profile.observations, operation, FixtureOutcome::Allowed)?;
    }
    if matches!(plan.mode, AccessMode::WorktreeWrite(_)) {
        for operation in [
            FixtureOperation::WorktreeWrite,
            FixtureOperation::GitIndexWrite,
            FixtureOperation::GitHeadWrite,
            FixtureOperation::GitObjectsWrite,
            FixtureOperation::SelectedRefWrite,
        ] {
            require_observation(&profile.observations, operation, FixtureOutcome::Allowed)?;
        }
        for operation in [
            FixtureOperation::GitConfigWrite,
            FixtureOperation::OtherRefWrite,
            FixtureOperation::ProtectedWrite,
        ] {
            require_observation(&profile.observations, operation, FixtureOutcome::Denied)?;
        }
    }
    Ok(())
}

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

fn validate_profile_spec(spec: &ProviderProfileSpec) -> Result<(), PermissionError> {
    if spec.profile_id.is_empty()
        || spec.profile_id.len() > 128
        || spec
            .profile_id
            .bytes()
            .any(|byte| !(byte.is_ascii_alphanumeric() || b"._-".contains(&byte)))
        || spec.process_executables.len() > MAX_PATH_GRANTS
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

fn validate_noncredential_grant(grant: &PathGrant) -> Result<(), PermissionError> {
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
        || (first == "private" && normal.get(1).is_some_and(|value| *value == "tmp"))
        || (first == "var" && normal.get(1).is_some_and(|value| *value == "tmp"))
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

fn validate_endpoint(endpoint: &NetworkEndpoint) -> Result<(), PermissionError> {
    if endpoint.port == 0 || endpoint.port > u64::from(u16::MAX) {
        return Err(PermissionError::InvalidFixture);
    }
    Ok(())
}

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
        NetworkGrant::UnixSocket { root } => anchors.verify_grant(root),
    }
}

#[cfg(target_os = "macos")]
fn push_network_endpoint(
    source: &mut String,
    endpoint: &NetworkEndpoint,
) -> Result<(), PermissionError> {
    validate_endpoint(endpoint)?;
    let port = u16::try_from(endpoint.port).map_err(|_| PermissionError::InvalidFixture)?;
    let address = match endpoint.address {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => format!("[{ip}]"),
    };
    source.push_str(&format!(
        "(allow network-outbound (remote tcp \"{address}:{port}\"))\n"
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

fn read_relative(root: &dyn SafeDir, path: &Path) -> Result<Vec<u8>, PermissionError> {
    let relative =
        provenant_core::RelativePath::parse(path).map_err(|_| PermissionError::InvalidFixture)?;
    let parts: Vec<&OsStr> = relative
        .as_path()
        .components()
        .filter_map(|part| match part {
            Component::Normal(value) => Some(value),
            _ => None,
        })
        .collect();
    if parts.is_empty() || parts.len() > 32 || path.as_os_str().as_encoded_bytes().len() > 4096 {
        return Err(PermissionError::InvalidFixture);
    }
    let mut parents: Vec<Box<dyn SafeDir>> = Vec::with_capacity(parts.len().saturating_sub(1));
    for part in parts.iter().take(parts.len().saturating_sub(1)) {
        let current = parents.last().map_or(root, |directory| directory.as_ref());
        let next = current
            .open_verified_directory(part)
            .map_err(PermissionError::Filesystem)?;
        parents.push(next);
    }
    let leaf = parts.last().ok_or(PermissionError::InvalidFixture)?;
    let current = parents.last().map_or(root, |directory| directory.as_ref());
    let mut file = current
        .open_read(leaf)
        .map_err(PermissionError::Filesystem)?;
    let before = file.metadata().map_err(PermissionError::Filesystem)?;
    if before.kind != FileKind::Regular || before.size > FIXTURE_LIMIT {
        return Err(PermissionError::InvalidFixture);
    }
    let capacity = usize::try_from(before.size).map_err(|_| PermissionError::InvalidFixture)?;
    let mut bytes = Vec::with_capacity(capacity);
    (&mut file)
        .take(FIXTURE_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| PermissionError::InvalidFixture)?;
    let after = file.metadata().map_err(PermissionError::Filesystem)?;
    if u64::try_from(bytes.len()).map_err(|_| PermissionError::InvalidFixture)? != before.size
        || after.size != before.size
        || !after.same_identity(&before)
    {
        return Err(PermissionError::InvalidFixture);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_supervisor::exec::SystemExecutor;
    use provenant_supervisor::fs::LocalFs;

    #[test]
    fn broad_home_tmp_and_credential_roots_are_refused() {
        for path in ["/", "/Users/alice", "/home/alice", "/tmp", "/private/tmp"] {
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

    #[test]
    fn provider_profile_without_a_declared_executable_is_refused() {
        let spec = ProviderProfileSpec {
            profile_id: "test-profile".into(),
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

    #[test]
    fn fixture_allowing_secret_reads_does_not_construct_a_provider_profile() {
        let temp = tempfile::tempdir_in("/tmp").expect("temporary fixture root");
        let temporary_fixture_root = temp.path().join("fixtures");
        std::fs::create_dir(&temporary_fixture_root).expect("fixture directory");
        let fixture = serde_json::json!({
            "schema_version": 1,
            "profile_id": "test-profile",
            "platform": "mac_os",
            "backend": "mac_os_sbpl",
            "observations": [
                {"operation":"undeclared_read","outcome":"denied"},
                {"operation":"undeclared_write","outcome":"denied"},
                {"operation":"secret_read","outcome":"allowed"},
                {"operation":"protected_write","outcome":"denied"},
                {"operation":"other_network","outcome":"denied"},
                {"operation":"declared_process_exec","outcome":"allowed"},
                {"operation":"undeclared_process_exec","outcome":"denied"},
                {"operation":"process_fork","outcome":"denied"}
            ]
        });
        std::fs::write(
            temporary_fixture_root.join("permission.json"),
            serde_json::to_vec(&fixture).expect("fixture JSON"),
        )
        .expect("fixture write");
        let fixture_root = provenant_supervisor::fs::canonicalize(&temporary_fixture_root)
            .expect("canonical temporary fixture root");
        let safe_fs = LocalFs;
        let spec = ProviderProfileSpec {
            profile_id: "test-profile".into(),
            process_executables: vec![PathGrant {
                path: PathBuf::from("/usr/bin/true"),
                scope: PathScope::Exact,
            }],
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
            ProviderProfile::load(&safe_fs, &fixture_root, Path::new("permission.json"), spec),
            Err(PermissionError::FixtureInsufficient)
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sbpl_process_authority_is_exact_and_fork_requires_profile_data() {
        let executable = PathGrant {
            path: PathBuf::from("/usr/bin/true"),
            scope: PathScope::Exact,
        };
        let mut spec = ProviderProfileSpec {
            profile_id: "recorded-test-profile".into(),
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
            fixture_digest: [0; 32],
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
        let rendered = String::from_utf8(render_profile(&plan, &profile, &[]).expect("SBPL"))
            .expect("UTF-8 SBPL");
        assert!(rendered.contains("(allow process-exec (literal \"/usr/bin/true\"))"));
        assert!(rendered.contains("(allow process-fork)"));

        spec.allow_process_fork = false;
        let profile = ProviderProfile {
            spec,
            fixture_digest: [0; 32],
            observations: BTreeMap::new(),
        };
        let rendered = String::from_utf8(render_profile(&plan, &profile, &[]).expect("SBPL"))
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
    fn network_ports_use_checked_u64_record_values() {
        let valid = NetworkEndpoint {
            address: IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            port: 443,
        };
        assert!(validate_endpoint(&valid).is_ok());
        for port in [0, u64::from(u16::MAX) + 1] {
            assert!(
                validate_endpoint(&NetworkEndpoint {
                    address: valid.address,
                    port,
                })
                .is_err()
            );
        }
    }
}
