//! Typed permission plan, profile and error vocabulary.

use super::*;
use crate::repo::{GitRefName, VerifiedWorktree};
use provenant_os::fs::FsError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

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
    #[error("credential root guard refused {path}: {guard}")]
    CredentialRootAt { path: String, guard: &'static str },
    #[error("credential root refused for {access} grant at {}", .path.display())]
    CredentialGrant { path: PathBuf, access: &'static str },
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
        "compiled profile is {bytes} bytes, over the configured {limit}-byte limit; reduce granted roots or raise domains.run.profile_limit_bytes (the OS may impose its own argv limit)"
    )]
    ProfileTooLarge { bytes: usize, limit: usize },
    #[error(
        "{family} exceeds its effective limit of {limit}; raise domains.run.grant_limit to allow more"
    )]
    GrantLimitExceeded { family: &'static str, limit: usize },
    #[error("{source} (source: {origin})")]
    ConfigOrigin {
        #[source]
        source: Box<PermissionError>,
        origin: &'static str,
    },
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
    #[error("verified repository authority could not be revalidated: {kind}: {0}", kind = .0.variant())]
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

impl PermissionError {
    /// Classify only a boundary that the typed error itself establishes.
    /// Ambiguous guards stay unclassified until their caller supplies context.
    pub fn boundary_class(&self) -> provenant_records::diagnostic::BoundaryClass {
        use provenant_records::diagnostic::BoundaryClass as B;
        match self {
            Self::ConfigOrigin { source, .. } => source.boundary_class(),
            Self::CredentialRoot | Self::CredentialRootAt { .. } | Self::CredentialGrant { .. } => {
                B::Credentials
            }
            Self::GitControlHardLinked(_)
            | Self::GitWriteAuthorityMissing
            | Self::Repository(_) => B::LaneGit,
            // A required path that cannot be encoded cannot be enforced in
            // SBPL. Optional discoveries are filtered before compilation.
            Self::AnchorMismatch
            | Self::AnchorMismatchAt { .. }
            | Self::InvalidFixture
            | Self::PermissionNotRecorded
            | Self::ProtectedOverlap
            | Self::InvalidReadOnlyPolicy
            | Self::ProcessPolicyMissing
            | Self::NonUtf8Path
            | Self::BackendUnavailable
            | Self::BackendNotAvailable(_)
            | Self::NestedConfinementUnsupported => B::Authority,
            _ => B::None,
        }
    }

    /// Add the refused path to a credential guard error.
    pub fn with_credential_path(self, path: &Path, home: Option<&Path>) -> Self {
        if matches!(self, Self::CredentialRoot) {
            let env_home = std::env::var_os("HOME").map(PathBuf::from);
            Self::credential_root_at(path, home.or(env_home.as_deref()))
        } else {
            self
        }
    }

    /// Preserve the requested path and identify the guard in user-facing refusals.
    pub fn credential_root_at(path: &Path, home: Option<&Path>) -> Self {
        let path = home
            .and_then(|home| path.strip_prefix(home).ok())
            .map(|relative| {
                if relative.as_os_str().is_empty() {
                    "~".to_owned()
                } else {
                    format!("~/{}", relative.display())
                }
            })
            .unwrap_or_else(|| path.display().to_string());
        Self::CredentialRootAt {
            path,
            guard: "credential root",
        }
    }
}

#[cfg(test)]
mod repository_error_tests {
    use super::PermissionError;
    use crate::repo::RepoError;

    #[test]
    fn boundary_classification_uses_the_typed_guard_not_a_generic_code() {
        use provenant_records::diagnostic::BoundaryClass as B;
        for (error, expected) in [
            (PermissionError::CredentialRoot, B::Credentials),
            (
                PermissionError::ConfigOrigin {
                    source: Box::new(PermissionError::CredentialRoot),
                    origin: "fixture",
                },
                B::Credentials,
            ),
            (PermissionError::GitWriteAuthorityMissing, B::LaneGit),
            (PermissionError::AnchorMismatch, B::Authority),
            (PermissionError::NonUtf8Path, B::Authority),
            (
                PermissionError::ProfileTooLarge { bytes: 2, limit: 1 },
                B::None,
            ),
            (PermissionError::BackendUnavailable, B::Authority),
            (
                PermissionError::BackendNotAvailable(super::AvailabilityReason::ProbeFailed),
                B::Authority,
            ),
        ] {
            assert_eq!(error.boundary_class(), expected, "{error:?}");
        }
    }

    #[test]
    fn repository_refusal_names_the_underlying_reason() {
        let message = PermissionError::from(RepoError::IdentityChanged).to_string();
        assert!(message.contains("IdentityChanged"));
    }
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
    pub(super) cargo_cache: PathGrant,
    pub(super) publishing: bool,
    /// Older records remain readable, but cannot launch without a verified binding.
    #[serde(default)]
    pub(super) common_git_dir: Option<PathBuf>,
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

/// List explicit write allowances in rendering order. Baseline `/dev/null` and
/// here-document access are omitted, as are concrete worktree/Git paths; writer
/// records carry the existing Git grants digest instead. This list grants no authority.
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
