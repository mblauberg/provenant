use super::{EdgeError, Lease};
use provenant_core::{CancelToken, Clock, HostId, Roots, WaitBudget};
use provenant_fabric_runtime::{
    config::FabricConfig,
    coordination::facade::FacadeError,
    hosts::{
        PeerAction, PeerError, PeerRequest, PeerResult, PeerTransport, ProjectLocator,
        SshTransport, discover_ssh_executable,
    },
    identity::{CrossHostProjectKey, Identity, IdentityError},
};
use provenant_os::{
    exec::{Executor, SystemExecutor},
    fs::{Directory, PublishMode},
};
use provenant_records::{AbsolutePath, CommandResult, Outcome};
use serde::Serialize;
use std::{
    ffi::OsStr,
    io,
    path::{Component, Path, PathBuf},
    time::Duration,
};

pub struct Context {
    /// An in-memory context for observation; mutation services must refuse it.
    pub inspection_only: bool,
    pub roots: Roots,
    pub identity: Identity,
    pub config: FabricConfig,
    /// Origin captured with the effective graph configuration.
    pub graph_prompt_limit_source: provenant_fabric_runtime::graph::InputCapSource,
    /// Captured once from the host-set MCP entry, never from tool arguments.
    pub in_lane: bool,
    pub(super) executor: SystemExecutor,
    pub(super) home: PathBuf,
    pub(super) cwd: PathBuf,
}
pub(super) fn canonical(path: &Path) -> Result<PathBuf, EdgeError> {
    Ok(provenant_os::fs::canonicalize(path)?)
}
/// The provider launch binds TMPDIR to this attempt's private scratch. Require
/// it explicitly: a lane must never fall back to the host's shared temp area.
fn lane_temporary() -> Result<PathBuf, EdgeError> {
    let path = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .ok_or(EdgeError::Invalid)?;
    if !path.is_absolute() {
        return Err(EdgeError::Invalid);
    }
    let path = canonical(&path)?;
    private_dir(&path, &path)?;
    Ok(path)
}

pub(crate) fn private_dir(root: &Path, path: &Path) -> Result<Directory, EdgeError> {
    let mut dir = match Directory::open_anchor(root) {
        Ok(dir) => dir,
        Err(error) if error.is_not_found() => {
            // Only the configured root may be created. Its parent must exist;
            // the trusted initial path permits ancestor aliases such as /var.
            let parent = Directory::open_anchor(root.parent().ok_or(EdgeError::Invalid)?)?;
            let name = root.file_name().ok_or(EdgeError::Invalid)?;
            match parent.mkdir(name, 0o700) {
                Ok(()) => parent.sync()?,
                Err(error) if error.is_exists() => {}
                Err(error) => return Err(error.into()),
            }
            Directory::open_anchor(root)?
        }
        Err(error) => return Err(error.into()),
    };
    let mode = dir.metadata()?.mode & 0o7777;
    if mode & 0o077 != 0 {
        return Err(EdgeError::StateDirectoryPermissions {
            path: root.to_owned(),
            mode,
        });
    }
    let mut current_path = root.to_owned();
    for part in path
        .strip_prefix(root)
        .map_err(|_| EdgeError::Invalid)?
        .components()
    {
        if let Component::Normal(name) = part {
            current_path.push(name);
            dir = match dir.open_verified_directory(name) {
                Ok(child) => child,
                Err(error) if error.is_not_found() => {
                    match dir.mkdir(name, 0o700) {
                        Ok(()) => dir.sync()?,
                        Err(error) if error.is_exists() => {}
                        Err(error) => return Err(error.into()),
                    }
                    dir.open_verified_directory(name)?
                }
                Err(error) => return Err(error.into()),
            };
            let mode = dir.metadata()?.mode & 0o7777;
            if mode & 0o077 != 0 {
                return Err(EdgeError::StateDirectoryPermissions {
                    path: current_path,
                    mode,
                });
            }
        } else {
            return Err(EdgeError::Invalid);
        }
    }
    Ok(dir)
}
/// Whether the host launched this process inside a lane. Every Fabric entry
/// point that can act as an agent checks it before opening the store.
pub fn in_lane() -> bool {
    std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV).is_some()
        || std::env::var_os(provenant_run::LANE_DEPTH_ENV).is_some_and(|depth| depth != "0")
}

/// An explicit, harness-neutral chair session for the chair log. Without it,
/// settlement finds the session the chair saved (`chair_session_for`).
pub fn current_session() -> Option<String> {
    std::env::var("PROVENANT_SESSION_ID")
        .ok()
        .filter(|value| !value.is_empty())
}

fn prompt_limit_source(
    origins: &std::collections::BTreeMap<String, provenant_config::Origin>,
) -> provenant_fabric_runtime::graph::InputCapSource {
    use provenant_fabric_runtime::graph::InputCapSource;
    match origins.get("domains.fabric.graph.limits.node_prompt_bytes") {
        Some(provenant_config::Origin::Compiled { .. }) => InputCapSource::Default,
        Some(provenant_config::Origin::User { .. }) => InputCapSource::Configured,
        None => InputCapSource::EffectiveGraphLimits,
    }
}

impl Context {
    pub fn open(roots: Roots, home: &Path, cwd: &Path) -> Result<Self, EdgeError> {
        // Validate configuration before allocating identities or opening SQLite.
        let resolved = provenant_config::read_with_domains(&roots, &crate::domains::SECTIONS)?;
        let graph_prompt_limit_source = prompt_limit_source(&resolved.origins);
        let config = resolved.domains.fabric;
        private_dir(roots.state.as_path(), roots.state.as_path())?;
        let utility_root = if in_lane() {
            lane_temporary()?
        } else {
            roots.state.as_path().join("tmp")
        };
        let utility_home = utility_root.join("git-home");
        let utility_anchor = if in_lane() {
            utility_root.as_path()
        } else {
            roots.state.as_path()
        };
        private_dir(utility_anchor, &utility_home)?;
        let lane = in_lane();
        let git = provenant_os::git::git_executable().ok_or(EdgeError::GitUnavailable)?;
        let runtime = provenant_confinement::UtilityRuntime::new(&git, &utility_home)?
            .with_lane_roots(provenant_confinement::utility::lane_state_roots(
                roots.state.as_path(),
            ));
        let executor = SystemExecutor::new(1)?;
        let budget = provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))?;
        let discovered = runtime.project_location(&executor, cwd, budget, &CancelToken::new())?;
        let binding = if lane {
            provenant_config::lookup_project(&roots, &discovered.location)?
                .ok_or(EdgeError::Invalid)?
        } else {
            provenant_config::bind_project(&roots, discovered.location)?
        };
        let key = if discovered.repository.is_some() {
            match CrossHostProjectKey::from_home(home, binding.common_dir()) {
                Ok(key) => Some(key),
                Err(IdentityError::OutsideHome) => None,
                Err(error) => return Err(error.into()),
            }
        } else {
            None
        };
        let host = provenant_config::bind_host(&roots)?;
        // Chair paths resolve harness markers separately through inbox_address.
        let identity = Identity::bind(
            &binding,
            host,
            cwd,
            &config,
            std::env::var_os("PROVENANT_AGENT_LABEL").as_deref(),
            None,
            key,
        )?;
        Ok(Self {
            inspection_only: false,
            roots,
            identity,
            config,
            graph_prompt_limit_source,
            in_lane: lane,
            executor,
            home: home.to_path_buf(),
            cwd: cwd.to_path_buf(),
        })
    }
    /// Resolve existing identity and configuration without creating state.
    pub fn open_read_only(roots: Roots, home: &Path, cwd: &Path) -> Result<Self, EdgeError> {
        let resolved = provenant_config::read_with_domains(&roots, &crate::domains::SECTIONS)?;
        let graph_prompt_limit_source = prompt_limit_source(&resolved.origins);
        let config = resolved.domains.fabric;
        let git = provenant_os::git::git_executable().ok_or(EdgeError::GitUnavailable)?;
        let runtime = provenant_confinement::UtilityRuntime::read_only(git)?;
        let executor = SystemExecutor::new(1)?;
        let budget = provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))?;
        let discovered = runtime.project_location(&executor, cwd, budget, &CancelToken::new())?;
        let binding = provenant_config::lookup_project(&roots, &discovered.location)?
            .ok_or(EdgeError::Invalid)?;
        let host = provenant_config::lookup_host(&roots)?.ok_or(EdgeError::Invalid)?;
        let key = if discovered.repository.is_some() {
            match CrossHostProjectKey::from_home(home, binding.common_dir()) {
                Ok(key) => Some(key),
                Err(IdentityError::OutsideHome) => None,
                Err(error) => return Err(error.into()),
            }
        } else {
            None
        };
        let identity = Identity::bind(
            &binding,
            host,
            cwd,
            &config,
            std::env::var_os("PROVENANT_AGENT_LABEL").as_deref(),
            None,
            key,
        )?;
        Ok(Self {
            inspection_only: false,
            roots,
            identity,
            config,
            graph_prompt_limit_source,
            in_lane: in_lane(),
            executor,
            home: home.to_path_buf(),
            cwd: cwd.to_path_buf(),
        })
    }
    /// Inspect templates and compile explanations before this host has an identity.
    /// The placeholder identity is never persisted or used to launch work.
    pub fn open_inspection(roots: Roots, home: &Path, cwd: &Path) -> Result<Self, EdgeError> {
        let resolved = provenant_config::read_with_domains(&roots, &crate::domains::SECTIONS)?;
        let graph_prompt_limit_source = prompt_limit_source(&resolved.origins);
        let config = resolved.domains.fabric;
        let git = provenant_os::git::git_executable().ok_or(EdgeError::GitUnavailable)?;
        let runtime = provenant_confinement::UtilityRuntime::read_only(git)?;
        let executor = SystemExecutor::new(1)?;
        let budget = provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))?;
        let discovered = runtime.project_location(&executor, cwd, budget, &CancelToken::new())?;
        let state_present = roots.state.as_path().try_exists()?;
        let binding = if state_present {
            provenant_config::lookup_project(&roots, &discovered.location)?
        } else {
            None
        };
        let host = if state_present {
            provenant_config::lookup_host(&roots)?
        } else {
            None
        }
        .unwrap_or_else(|| HostId::from_bytes([0; 16]));
        let common_dir = discovered.location.common_dir();
        let digest =
            provenant_records::CanonicalV1::file_digest(common_dir.as_os_str().as_encoded_bytes());
        let mut bytes = [0; 16];
        bytes.copy_from_slice(&digest.bytes()[..16]);
        let project = binding.map_or_else(
            || provenant_core::ProjectId::from_bytes(bytes),
            |binding| binding.project_id(),
        );
        let identity = Identity {
            project,
            host,
            inbox: provenant_fabric_runtime::identity::inbox_address(
                host,
                &config,
                std::env::var_os("PROVENANT_AGENT_LABEL").as_deref(),
                None,
            )?,
            client: config.client_label.clone(),
            cwd: AbsolutePath::new(cwd).map_err(|_| EdgeError::Invalid)?,
            common_dir: AbsolutePath::new(common_dir).map_err(|_| EdgeError::Invalid)?,
            federation: None,
        };
        Ok(Self {
            inspection_only: true,
            roots,
            identity,
            config,
            graph_prompt_limit_source,
            in_lane: in_lane(),
            executor,
            home: home.to_path_buf(),
            cwd: cwd.to_path_buf(),
        })
    }
    /// Context for an already registered project, found without a working
    /// directory inside it. Reads existing identity only and creates no state.
    pub fn open_for_project(
        roots: Roots,
        home: &Path,
        binding: &provenant_config::ProjectBinding,
    ) -> Result<Self, EdgeError> {
        // Its working directory is the project's, as if started there.
        let cwd = binding.common_dir().parent().ok_or(EdgeError::Invalid)?;
        let resolved = provenant_config::read_with_domains(&roots, &crate::domains::SECTIONS)?;
        let graph_prompt_limit_source = prompt_limit_source(&resolved.origins);
        let config = resolved.domains.fabric;
        let host = provenant_config::lookup_host(&roots)?.ok_or(EdgeError::Invalid)?;
        let key = match CrossHostProjectKey::from_home(home, binding.common_dir()) {
            Ok(key) => Some(key),
            Err(IdentityError::OutsideHome) => None,
            Err(error) => return Err(error.into()),
        };
        let identity = Identity::bind(
            binding,
            host,
            cwd,
            &config,
            std::env::var_os("PROVENANT_AGENT_LABEL").as_deref(),
            None,
            key,
        )?;
        Ok(Self {
            inspection_only: false,
            roots,
            identity,
            config,
            graph_prompt_limit_source,
            in_lane: in_lane(),
            executor: SystemExecutor::new(1)?,
            home: home.to_path_buf(),
            cwd: cwd.to_path_buf(),
        })
    }
    /// A configured remote home is the sole coordination owner.
    pub fn remote_home(&self) -> bool {
        self.identity.federation.as_ref().is_some_and(|project| {
            self.config
                .home_hosts
                .get(project.key.as_str())
                .is_some_and(|host| *host != self.identity.host)
        })
    }
    /// This selects a socket, never sender authority; the host binds the actor.
    pub fn lane_relay_socket(&self) -> Result<PathBuf, EdgeError> {
        let attempt = std::env::var("PROVENANT_PARENT_ATTEMPT_ID")
            .ok()
            .and_then(|id| provenant_core::AttemptId::parse(&id).ok())
            .ok_or(EdgeError::Invalid)?;
        Ok(provenant_providers::LaneRuntime::fabric_socket(attempt))
    }

    pub fn inbox_address(
        &self,
        client: Option<provenant_records::Harness>,
    ) -> Result<provenant_fabric_runtime::identity::Address, IdentityError> {
        provenant_fabric_runtime::identity::inbox_address(
            self.identity.host,
            &self.config,
            std::env::var_os("PROVENANT_AGENT_LABEL").as_deref(),
            harness_client(client).map(|harness| harness.spec().name),
        )
    }
    pub fn text(&self, text: &str) -> Result<(), EdgeError> {
        if text.len() as u64 > self.config.limits.body_bytes {
            return Err(EdgeError::Invalid);
        }
        Ok(())
    }
    pub fn texts(&self, text: &str, note: Option<&str>) -> Result<(), EdgeError> {
        self.text(text)?;
        if let Some(note) = note {
            self.text(note)?;
        }
        Ok(())
    }
    pub fn page(&self, limit: u64) -> Result<(), EdgeError> {
        if limit > self.config.limits.page_rows {
            return Err(EdgeError::Invalid);
        }
        Ok(())
    }
    /// Resolve a configured peer by its label or host ID.
    pub fn peer_host(&self, name: &str) -> Option<HostId> {
        if let Some(peer) = self.config.peers.get(name) {
            return Some(peer.host);
        }
        let host = HostId::parse(name).ok()?;
        self.config
            .peers
            .values()
            .any(|peer| peer.host == host)
            .then_some(host)
    }
    /// One bounded request to a configured peer's fixed `fabric peer` entrypoint.
    pub fn peer(
        &self,
        clock: &dyn Clock,
        host: HostId,
        action: PeerAction,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<PeerResult, PeerFailure> {
        self.peer_with(&self.executor, clock, host, action, budget, cancel)
    }
    /// [`Context::peer`] that is over within `within`, SSH termination and
    /// reap included, however the whole-second `budget` rounds.
    pub fn peer_within(
        &self,
        clock: &dyn Clock,
        host: HostId,
        action: PeerAction,
        budget: WaitBudget,
        cancel: &CancelToken,
        within: Duration,
    ) -> Result<PeerResult, PeerFailure> {
        self.peer_bounded(
            &self.executor,
            clock,
            host,
            action,
            budget,
            cancel,
            Some(within),
        )
    }
    /// [`Context::peer`] through a caller-owned executor.
    pub fn peer_with(
        &self,
        executor: &dyn Executor,
        clock: &dyn Clock,
        host: HostId,
        action: PeerAction,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<PeerResult, PeerFailure> {
        self.peer_bounded(executor, clock, host, action, budget, cancel, None)
    }
    #[allow(clippy::too_many_arguments)]
    fn peer_bounded(
        &self,
        executor: &dyn Executor,
        clock: &dyn Clock,
        host: HostId,
        action: PeerAction,
        budget: WaitBudget,
        cancel: &CancelToken,
        within: Option<Duration>,
    ) -> Result<PeerResult, PeerFailure> {
        let peer = self
            .config
            .peers
            .values()
            .find(|p| p.host == host)
            .ok_or_else(|| {
                PeerFailure::new(FacadeError::HomeUnavailable, "peer host is not configured")
            })?;
        let project = ProjectLocator::new(&self.identity, &self.home)
            .map_err(|_| PeerFailure::from(FacadeError::ConfinementRefused))?;
        let request = PeerRequest::new(
            host,
            self.identity.host,
            self.identity.inbox.agent.clone(),
            project,
            action,
        );
        let executable =
            discover_ssh_executable(self.config.ssh_executable.as_deref()).map_err(|_| {
                PeerFailure::new(FacadeError::HomeUnavailable, "no usable ssh executable")
            })?;
        let transport = SshTransport {
            executor,
            clock,
            home: &self.home,
            cwd: &self.cwd,
            executable,
            seconds: self.config.limits.peer_seconds,
        };
        let started = clock.mono_now();
        let exchange = |request: &PeerRequest| {
            let elapsed = Duration::from_micros(
                clock
                    .mono_now()
                    .as_micros()
                    .saturating_sub(started.as_micros()),
            );
            match within {
                Some(limit) => transport.exchange_within(
                    peer,
                    request,
                    budget,
                    cancel,
                    limit.saturating_sub(elapsed),
                ),
                None => transport.exchange(peer, request, budget, cancel),
            }
        };
        let exchanged = if matches!(request.action, PeerAction::Submit { .. }) {
            provenant_fabric_runtime::hosts::encode_peer_request(&request)
                .map_err(|error| peer_failure(&peer.destination, &error))?;
            let probe = PeerRequest::new(
                host,
                self.identity.host,
                self.identity.inbox.agent.clone(),
                request.project.clone(),
                PeerAction::Health {
                    extra: Default::default(),
                },
            );
            submit_after_probe(
                &peer.destination,
                || {
                    let remaining =
                        Duration::from_micros(budget.remaining(clock.mono_now()).as_micros());
                    let remaining = within.map_or(remaining, |limit| {
                        let elapsed = Duration::from_micros(
                            clock
                                .mono_now()
                                .as_micros()
                                .saturating_sub(started.as_micros()),
                        );
                        remaining.min(limit.saturating_sub(elapsed))
                    });
                    transport.exchange_within(
                        peer,
                        &probe,
                        budget,
                        cancel,
                        (remaining / 4).min(Duration::from_millis(250)),
                    )
                },
                || exchange(&request),
            )?
        } else {
            exchange(&request).map_err(|error| peer_failure(&peer.destination, &error))?
        };
        match exchanged.result {
            PeerResult::Error {
                data: provenant_fabric_runtime::hosts::PeerResultError { error, .. },
                ..
            } => {
                // An uncertain launch must name this locally frozen request.
                if let FacadeError::LaunchUnknown {
                    operation_id: returned,
                } = &error
                    && Some(returned) != request.operation().as_ref()
                {
                    return Err(peer_failure(&peer.destination, &PeerError::Frame));
                }
                Err(error.into())
            }
            value => Ok(value),
        }
    }
    pub fn lease(&self, lease: Lease) -> Result<u64, EdgeError> {
        if lease.lease_seconds > self.config.limits.lease_seconds {
            return Err(EdgeError::Invalid);
        }
        Ok(lease.lease_seconds)
    }
}

fn submit_after_probe(
    peer: &str,
    probe: impl FnOnce() -> Result<provenant_fabric_runtime::hosts::PeerResponse, PeerError>,
    submit: impl FnOnce() -> Result<provenant_fabric_runtime::hosts::PeerResponse, PeerError>,
) -> Result<provenant_fabric_runtime::hosts::PeerResponse, PeerFailure> {
    if let Ok(response) = probe() {
        check_submit_health(peer, response.result)?;
    }
    submit().map_err(|error| peer_failure(peer, &error))
}

const MIN_SUBMIT_RELEASE: (u64, u64, u64) = (1, 6, 5);

/// Refuse only capabilities the peer explicitly reports as incompatible.
fn check_submit_health(peer: &str, result: PeerResult) -> Result<(), PeerFailure> {
    let PeerResult::Health { data, .. } = result else {
        return Ok(());
    };
    let version = data.release.as_deref().and_then(|v| {
        let valid_identifiers = |suffix: &str, prerelease: bool| {
            suffix.split('.').all(|part| {
                !part.is_empty()
                    && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    && !(prerelease
                        && part.len() > 1
                        && part.starts_with('0')
                        && part.bytes().all(|b| b.is_ascii_digit()))
            })
        };
        let core = if let Some((core, build)) = v.split_once('+') {
            if !valid_identifiers(build, false) {
                return None;
            }
            core
        } else {
            v
        };
        let core = if let Some((core, prerelease)) = core.split_once('-') {
            if !valid_identifiers(prerelease, true) {
                return None;
            }
            core
        } else {
            core
        };
        let mut parts = core.split('.').map(|part| {
            if part.len() > 1 && part.starts_with('0') {
                return None;
            }
            if !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            part.parse::<u64>().ok()
        });
        let version = (parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(version)
    });
    if version.is_some_and(|version| version < MIN_SUBMIT_RELEASE) {
        return Err(PeerFailure::new(
            FacadeError::PeerVersion,
            format!(
                "{peer}: peer release {} is too old for submission; run `provenant upgrade` on the peer",
                provenant_fabric_runtime::hosts::plain_text(
                    data.release.as_deref().unwrap_or("unknown"),
                    80
                )
            ),
        ));
    }
    if data
        .run_capabilities
        .is_some_and(|capabilities| !capabilities.submit)
    {
        return Err(PeerFailure::new(
            FacadeError::DispatchRefused {
                reason: "peer does not support submission".into(),
                disk: None,
                fact: None,
            },
            format!("{peer}: submission capability is unavailable"),
        ));
    }
    Ok(())
}

/// Configuration directories do not identify the active harness. Conflicting
/// inherited markers require an explicit client rather than guessing a chair.
fn harness_client(
    explicit: Option<provenant_records::Harness>,
) -> Option<provenant_records::Harness> {
    use provenant_records::Harness;
    explicit.or_else(|| {
        let present = |name| std::env::var_os(name).is_some_and(|value| !value.is_empty());
        let codex = present("CODEX_THREAD_ID");
        let claude = std::env::var("CLAUDECODE").is_ok_and(|value| value == "1")
            || present("CLAUDE_CODE_SESSION_ID")
            || present("CLAUDE_SESSION_ID");
        match (codex, claude) {
            (true, false) => Some(Harness::Codex),
            (false, true) => Some(Harness::Claude),
            _ => None,
        }
    })
}
/// A peer call failure: the stable error plus, for transport failures, the
/// peer and cause. The detail is message text only; the wire codes are unchanged.
#[derive(Debug)]
pub struct PeerFailure {
    pub error: FacadeError,
    pub detail: Option<String>,
}
impl PeerFailure {
    fn new(error: FacadeError, detail: impl Into<String>) -> Self {
        Self {
            error,
            detail: Some(detail.into()),
        }
    }
}
impl From<FacadeError> for PeerFailure {
    fn from(error: FacadeError) -> Self {
        Self {
            error,
            detail: None,
        }
    }
}
/// The stable outcome and code for a peer refusal, shared by remote runs and
/// session hand-off. `LaunchUnknown` is handled by each caller, which names
/// its own recovery step. `TIMEOUT` is retryable.
pub fn facade_outcome(error: &FacadeError) -> (Outcome, provenant_run::ErrorKind) {
    use provenant_run::ErrorKind;
    match error {
        FacadeError::LaunchUnknown { .. } => (Outcome::Error, ErrorKind::LaunchUnknown),
        FacadeError::HomeUnavailable => (Outcome::Error, ErrorKind::PeerUnavailable),
        FacadeError::PeerVersion | FacadeError::PeerProtocolMismatch { .. } => {
            (Outcome::Refused, ErrorKind::PeerVersion)
        }
        FacadeError::InvalidInput { .. } => (Outcome::Invalid, ErrorKind::Invalid),
        FacadeError::NotFound => (Outcome::Invalid, ErrorKind::NotFound),
        FacadeError::Conflict | FacadeError::GenerationConflict { .. } => {
            (Outcome::Refused, ErrorKind::Conflict)
        }
        FacadeError::NotTerminal => (Outcome::Refused, ErrorKind::Nonterminal),
        FacadeError::OperationConflict => (Outcome::Refused, ErrorKind::OperationConflict),
        FacadeError::Busy => (Outcome::Refused, ErrorKind::QueueFull),
        FacadeError::ConfinementRefused => (Outcome::Refused, ErrorKind::Refused),
        FacadeError::WriterBusy => (Outcome::Refused, ErrorKind::WriterBusy),
        FacadeError::WriterLockBusy => (Outcome::Refused, ErrorKind::WriterLockBusy),
        FacadeError::WriterUnknown => (Outcome::Refused, ErrorKind::WriterUnknown),
        FacadeError::CredentialRoot { .. } => (Outcome::Refused, ErrorKind::CredentialRoot),
        FacadeError::DispatchRefused { .. } => (Outcome::Refused, ErrorKind::Refused),
        FacadeError::DispatchUnobserved { .. }
        | FacadeError::HostDeliveryStage { .. }
        | FacadeError::HostDeliveryLimit { .. } => (Outcome::Error, ErrorKind::Io),
        FacadeError::WorktreeMissing => (Outcome::Invalid, ErrorKind::WorktreeMissing),
        FacadeError::WorktreeUnregistered { .. } => {
            (Outcome::Refused, ErrorKind::WorktreeUnregistered)
        }
        FacadeError::BaseUnreachable => (Outcome::Refused, ErrorKind::BaseUnreachable),
        FacadeError::BranchExists => (Outcome::Refused, ErrorKind::BranchExists),
        FacadeError::LaneDepth => (Outcome::Refused, ErrorKind::LaneDepth),
        FacadeError::LaneCoordinationRefused => {
            (Outcome::Refused, ErrorKind::LaneCoordinationRefused)
        }
        FacadeError::OwnerUnavailable => (Outcome::Refused, ErrorKind::OwnerUnavailable),
        FacadeError::ProviderStateExpired => (Outcome::Refused, ErrorKind::ProviderStateExpired),
        FacadeError::Cancelled => (Outcome::Error, ErrorKind::Cancelled),
        FacadeError::StateCorrupt => (Outcome::Error, ErrorKind::StateCorrupt),
        FacadeError::UpgradeRefused => (Outcome::Refused, ErrorKind::UpgradeRefused),
        FacadeError::UpgradeRequired { .. } => (Outcome::Refused, ErrorKind::UpgradeRequired),
        FacadeError::Timeout => (Outcome::Error, ErrorKind::Timeout),
        FacadeError::StaleFence
        | FacadeError::ContinuationUnsupported
        | FacadeError::OutputTooLarge
        | FacadeError::PublicationFailed
        | FacadeError::Io { .. } => (Outcome::Error, ErrorKind::Peer),
    }
}
/// Map a transport error, naming the peer and, when known, the cause.
pub fn peer_failure(destination: &str, error: &PeerError) -> PeerFailure {
    let detail = match error {
        PeerError::Failed(cause) => Some(format!("{destination}: {cause}")),
        PeerError::Unavailable => Some(format!("{destination}: peer is unavailable")),
        PeerError::Frame => Some(format!("{destination}: invalid peer frame")),
        PeerError::RequestTooLarge => Some(format!("{destination}: {error}")),
        _ => None,
    };
    PeerFailure {
        error: FacadeError::from(error),
        detail,
    }
}
/// Publish a result in the selected lane or host result area.
pub fn publish<T: Serialize>(value: &CommandResult<T>, roots: &Roots) -> io::Result<AbsolutePath> {
    fn perform<T: Serialize>(
        value: &CommandResult<T>,
        roots: &Roots,
    ) -> Result<AbsolutePath, EdgeError> {
        let base = if in_lane() {
            lane_temporary()?
        } else {
            roots.state.as_path().join("tmp")
        };
        let path = base.join("results");
        let anchor = if in_lane() {
            base.as_path()
        } else {
            roots.state.as_path()
        };
        let dir = private_dir(anchor, &path)?;
        let name = provenant_os::fs::temporary_name()?;
        let bytes = serde_json::to_vec(value)?;
        dir.atomic_write(&name, ".result-", &bytes, PublishMode::NoReplace)?
            .require_durable()?;
        AbsolutePath::new(&path.join(OsStr::new(&name))).map_err(|_| EdgeError::Invalid)
    }
    perform(value, roots).map_err(|_| io::Error::other("cannot publish oversized result"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn lane_publication_uses_private_scratch() -> Result<(), Box<dyn std::error::Error>> {
        use provenant_core::RootInputs;
        const FIXTURE: &str = "PROVENANT_TEST_LANE_PUBLICATION";
        if let Some(base) = std::env::var_os(FIXTURE) {
            let base = std::path::PathBuf::from(base);
            let mut inputs = RootInputs::new(base.join("home"), base.join("assets"));
            inputs.state_dir = Some(base.join("state"));
            let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
            let result = provenant_records::CommandResult::new(
                "fixture",
                provenant_records::Outcome::Ok,
                "result",
            );
            let path = super::publish(&result, &roots)?;
            assert!(
                path.to_path_buf()?
                    .starts_with(base.join("scratch/results"))
            );
            let bytes = std::fs::read(path.to_path_buf()?)?;
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&bytes)?["data"],
                "result"
            );
            assert!(!base.join("state").exists());
            return Ok(());
        }
        let temp = tempfile::tempdir()?;
        let base = temp.path().canonicalize()?;
        for name in ["home", "assets", "scratch"] {
            std::fs::create_dir(base.join(name))?;
            std::fs::set_permissions(
                base.join(name),
                std::os::unix::fs::PermissionsExt::from_mode(0o700),
            )?;
        }
        let output = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "fabric::context::tests::lane_publication_uses_private_scratch",
                "--nocapture",
            ])
            .env(FIXTURE, &base)
            .env(provenant_core::RUN_STATE_DIRECTORY_ENV, base.join("state"))
            .env("TMPDIR", base.join("scratch"))
            .output()?;
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!base.join("state").exists());
        Ok(())
    }

    #[test]
    fn submit_preflight_refuses_old_or_incapable_stub_before_launch()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_core::HostId;
        use provenant_fabric_runtime::{
            hosts::{PROTOCOL, PeerHealth, PeerResponse, PeerResult},
            identity::CrossHostProjectKey,
            runs::RunCapabilities,
        };
        use std::{cell::Cell, path::Path};
        let host = HostId::from_bytes([2; 16]);
        let project =
            CrossHostProjectKey::from_home(Path::new("/home/a"), Path::new("/home/a/repo"))?;
        for (release, submit, code) in [
            (Some("1.6.4"), Some(true), Some("peer_version")),
            (Some("1.6.5"), Some(false), Some("dispatch_refused")),
            (Some("1.6.5"), Some(true), None),
            (Some("1.6.5-rc.1"), Some(true), None),
            (None, None, None),
            (Some("unparseable"), None, None),
            (Some("1.6.4-"), None, None),
        ] {
            let calls = Cell::new(0);
            let stub = || {
                calls.set(calls.get() + 1);
                Ok(PeerResponse {
                    extra: Default::default(),
                    version: 1,
                    protocol: PROTOCOL,
                    host,
                    project: project.clone(),
                    result: PeerResult::Health {
                        extra: Default::default(),
                        data: PeerHealth {
                            extra: Default::default(),
                            host,
                            available: true,
                            execution: submit,
                            release: release.map(str::to_owned),
                            run_capabilities: submit.map(|submit| RunCapabilities {
                                submit,
                                list: true,
                                status: true,
                                wait: true,
                                cancel: true,
                                resume: true,
                                output: true,
                                events: true,
                            }),
                            readiness: None,
                        },
                    },
                })
            };
            let result = super::submit_after_probe("worker", stub, stub);
            if let Some(code) = code {
                let error = result.unwrap_err();
                assert_eq!(error.error.code(), code);
                assert_eq!(calls.get(), 1);
                if code == "peer_version" {
                    assert!(
                        error
                            .detail
                            .unwrap_or_default()
                            .contains("provenant upgrade")
                    );
                }
            } else {
                assert!(result.is_ok());
                assert_eq!(calls.get(), 2);
            }
        }
        Ok(())
    }

    #[test]
    fn prompt_limit_origin_distinguishes_default_from_explicit_same_value()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_fabric_runtime::graph::InputCapSource;
        let temp = tempfile::tempdir()?;
        let mut inputs =
            provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("repo"));
        inputs.state_dir = Some(temp.path().join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let default = provenant_config::load_with_domains::<&[u8], _>(
            &roots,
            None,
            &crate::domains::SECTIONS,
        )?;
        assert_eq!(
            super::prompt_limit_source(&default.origins),
            InputCapSource::Default
        );
        let configured = b"schema_version = 1
[domains.fabric.graph.limits]
node_prompt_bytes = 262144
";
        let resolved = provenant_config::load_with_domains(
            &roots,
            Some(configured.as_slice()),
            &crate::domains::SECTIONS,
        )?;
        assert_eq!(
            resolved.domains.fabric.graph.limits.node_prompt_bytes,
            default.domains.fabric.graph.limits.node_prompt_bytes
        );
        assert_eq!(
            super::prompt_limit_source(&resolved.origins),
            InputCapSource::Configured
        );
        Ok(())
    }
    #[test]
    fn protocol_mismatch_keeps_both_versions_and_the_cli_code() {
        let failure = super::peer_failure(
            "workshop",
            &super::PeerError::ProtocolMismatch {
                peer: "workshop".into(),
                remote: 1,
                expected: provenant_fabric_runtime::hosts::PROTOCOL,
            },
        );
        assert_eq!(failure.error.code(), "peer_version");
        assert_eq!(
            failure.error.to_string(),
            format!(
                "peer workshop speaks protocol 1, this host expects protocol {}; upgrade both",
                provenant_fabric_runtime::hosts::PROTOCOL
            )
        );
        assert_eq!(
            super::facade_outcome(&failure.error).1.code(),
            "PEER_VERSION"
        );
    }
}
