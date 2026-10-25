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
use provenant_records::{AbsolutePath, CommandResult, Outcome};
use provenant_supervisor::{
    exec::{Executor, SystemExecutor},
    fs::{Directory, LocalFs, PublishMode},
};
use serde::Serialize;
use std::{
    ffi::OsStr,
    io,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Duration,
};

pub struct Context {
    pub roots: Roots,
    pub identity: Identity,
    pub config: FabricConfig,
    pub(super) executor: SystemExecutor,
    pub(super) home: PathBuf,
    pub(super) cwd: PathBuf,
}
pub(super) fn canonical(path: &Path) -> Result<PathBuf, EdgeError> {
    Ok(provenant_supervisor::fs::canonicalize(path)?)
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
    if dir.metadata()?.mode & 0o077 != 0 {
        return Err(EdgeError::Invalid);
    }
    for part in path
        .strip_prefix(root)
        .map_err(|_| EdgeError::Invalid)?
        .components()
    {
        if let Component::Normal(name) = part {
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
            if dir.metadata()?.mode & 0o077 != 0 {
                return Err(EdgeError::Invalid);
            }
        } else {
            return Err(EdgeError::Invalid);
        }
    }
    Ok(dir)
}
impl Context {
    pub fn open(roots: Roots, home: &Path, cwd: &Path) -> Result<Self, EdgeError> {
        // Validate configuration before allocating identities or opening SQLite.
        let config =
            provenant_config::read_with_domains(&roots, &LocalFs, &crate::domains::SECTIONS)?
                .domains
                .fabric;
        private_dir(roots.state.as_path(), roots.state.as_path())?;
        let utility_root = std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|| roots.state.as_path().to_owned());
        let utility_home = utility_root.join("tmp/git-home");
        private_dir(roots.state.as_path(), &utility_home)?;
        let lane = std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV).is_some()
            || std::env::var_os(provenant_run::LANE_DEPTH_ENV).is_some();
        let git = provenant_supervisor::git::git_executable().ok_or(EdgeError::GitUnavailable)?;
        let runtime = provenant_confinement::UtilityRuntime::new(
            Arc::new(LocalFs),
            canonical(&git)?,
            &utility_home,
        )?;
        let executor = SystemExecutor::new(1)?;
        let budget = provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))?;
        let discovered = runtime.project_location(&executor, cwd, budget, &CancelToken::new())?;
        let binding = if lane {
            provenant_config::lookup_project(&roots, &discovered.location, &LocalFs)?
                .ok_or(EdgeError::Invalid)?
        } else {
            provenant_config::bind_project(&roots, discovered.location, &LocalFs)?
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
        let host = provenant_config::bind_host(&roots, &LocalFs)?;
        let identity = Identity::bind(
            &binding,
            host,
            cwd,
            &config,
            std::env::var_os("PROVENANT_AGENT_LABEL").as_deref(),
            key,
        )?;
        Ok(Self {
            roots,
            identity,
            config,
            executor,
            home: home.to_path_buf(),
            cwd: cwd.to_path_buf(),
        })
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
        let request = PeerRequest {
            version: 1,
            target_host: host,
            source_host: self.identity.host,
            agent: self.identity.inbox.agent.clone(),
            project,
            action,
        };
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
        match transport
            .exchange(peer, &request, budget, cancel)
            .map_err(|error| peer_failure(&peer.destination, &error))?
            .result
        {
            PeerResult::Error { code, operation_id } => {
                Err(FacadeError::from_code(&code, operation_id).into())
            }
            value => Ok(value),
        }
    }
    pub fn lease(&self, lease: Lease) -> Result<u64, EdgeError> {
        if lease.lease_seconds > self.config.limits.lease_seconds {
            return Err(EdgeError::Invalid);
        }
        Ok(lease.lease_seconds * 1000)
    }
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
/// its own recovery step.
pub fn facade_outcome(error: &FacadeError) -> (Outcome, &'static str) {
    match error {
        FacadeError::LaunchUnknown { .. } => (Outcome::Error, "LAUNCH_UNKNOWN"),
        FacadeError::HomeUnavailable => (Outcome::Error, "PEER_UNAVAILABLE"),
        FacadeError::PeerVersion => (Outcome::Refused, "PEER_VERSION"),
        FacadeError::InvalidInput => (Outcome::Invalid, "INVALID"),
        FacadeError::NotFound => (Outcome::Invalid, "NOT_FOUND"),
        FacadeError::Conflict => (Outcome::Refused, "CONFLICT"),
        FacadeError::Busy => (Outcome::Refused, "QUEUE_FULL"),
        FacadeError::ConfinementRefused => (Outcome::Refused, "REFUSED"),
        FacadeError::WorktreeMissing => (Outcome::Invalid, "WORKTREE_MISSING"),
        FacadeError::BaseUnreachable => (Outcome::Refused, "BASE_UNREACHABLE"),
        FacadeError::BranchExists => (Outcome::Refused, "BRANCH_EXISTS"),
        FacadeError::LaneDepth => (Outcome::Refused, "LANE_DEPTH"),
        FacadeError::OwnerUnavailable => (Outcome::Refused, "UNSUPPORTED"),
        FacadeError::Cancelled => (Outcome::Error, "CANCELLED"),
        FacadeError::StateCorrupt => (Outcome::Error, "STATE_CORRUPT"),
        _ => (Outcome::Error, "PEER"),
    }
}
/// Map a transport error, naming the peer and, when known, the cause.
pub fn peer_failure(destination: &str, error: &PeerError) -> PeerFailure {
    let operation_id = match error {
        PeerError::Unknown { operation_id } => Some(operation_id.clone()),
        _ => None,
    };
    let detail = match error {
        PeerError::Failed(cause) => Some(format!("{destination}: {cause}")),
        PeerError::Unavailable => Some(format!("{destination}: peer is unavailable")),
        PeerError::Frame => Some(format!("{destination}: invalid peer frame")),
        _ => None,
    };
    PeerFailure {
        error: FacadeError::from_code(error.code(), operation_id),
        detail,
    }
}
/// Publish a result in the selected lane or host result area.
pub fn publish<T: Serialize>(value: &CommandResult<T>, roots: &Roots) -> io::Result<AbsolutePath> {
    fn perform<T: Serialize>(
        value: &CommandResult<T>,
        roots: &Roots,
    ) -> Result<AbsolutePath, EdgeError> {
        let base = std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|| roots.state.as_path().to_owned());
        let path = base.join("tmp/results");
        let dir = private_dir(roots.state.as_path(), &path)?;
        let name = provenant_supervisor::fs::temporary_name()?;
        let bytes = serde_json::to_vec(value)?;
        match dir.atomic_write(&name, ".result-", &bytes, PublishMode::NoReplace)? {
            provenant_supervisor::fs::Publication::Durable => {}
            provenant_supervisor::fs::Publication::VisibleDurabilityUnconfirmed { sync_error } => {
                return Err(sync_error.into());
            }
        }
        AbsolutePath::new(&path.join(OsStr::new(&name))).map_err(|_| EdgeError::Invalid)
    }
    perform(value, roots).map_err(|_| io::Error::other("cannot publish oversized result"))
}
