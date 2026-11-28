use super::EdgeError;
use provenant_config::DomainConfig;
use provenant_core::{CancelToken, Clock, HostId, Roots, WaitBudget};
use provenant_fabric_runtime::{
    config::FabricConfig,
    coordination::facade::FacadeError,
    graph::{
        REVIEW_ACCEPT_BELOW_KEY, REVIEW_ADJUDICATOR_ROUTE_KEY, REVIEW_EVIDENCE_RETRIES_KEY,
        REVIEW_LATER_ROUNDS_KEY, REVIEW_LENS_RETRY_KEY, ReviewPolicy,
    },
    hosts::{
        PeerAction, PeerError, PeerRequest, PeerResult, PeerTransport, ProjectLocator,
        SshTransport, discover_ssh_executable,
        mirror::{
            self, PEER_PROJECT_UNLOCATABLE, PEER_SOURCE_SNAPSHOT, PROJECTION_KEY, ProjectionMode,
        },
    },
    identity::{CrossHostProjectKey, Identity, IdentityError},
};
use provenant_os::clock::SystemClock;
use provenant_os::{
    exec::{Executor, SystemExecutor},
    fs::{Directory, PublishMode},
};
use provenant_records::{
    AbsolutePath, CommandResult, Outcome, Source, Sourced,
    command_result::SettingRef,
    warning::{Warning, WarningCode, warn},
};
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
    /// Review settings resolved from host/project config and sealed on new graphs.
    pub review_policy: ReviewPolicy,
    pub local_observation: provenant_fabric_runtime::coordination::ObservationSettings,
    /// Project ports resolved once from the discovered project root.
    pub port_table: provenant_config::PortTable,
    /// Verified owner for project-scoped worktree allocation.
    pub project_root: PathBuf,
    /// Origin captured with the effective graph configuration.
    pub graph_prompt_limit_source: provenant_records::Layer,
    pub graph_decide_timeout_source: provenant_records::Layer,
    /// Captured once from the host-set lane environment, never from arguments.
    pub in_lane: bool,
    /// `domains.fabric.project_projection` with its resolved layer and origin.
    pub projection: Sourced<ProjectionMode>,
    pub(super) executor: SystemExecutor,
    pub(super) home: PathBuf,
    pub(super) cwd: PathBuf,
}

fn review_policy(
    settings: &provenant_fabric_runtime::config::ReviewSettings,
    knobs: &std::collections::BTreeMap<String, Sourced<serde_json::Value>>,
    origins: &std::collections::BTreeMap<String, provenant_config::Origin>,
) -> ReviewPolicy {
    fn effective<T>(
        fallback: T,
        key: &str,
        knobs: &std::collections::BTreeMap<String, Sourced<serde_json::Value>>,
        origins: &std::collections::BTreeMap<String, provenant_config::Origin>,
    ) -> Sourced<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let candidate = knobs.get(key);
        let override_value = candidate
            .filter(|candidate| candidate.source.layer != provenant_records::Layer::Default)
            .and_then(|candidate| serde_json::from_value(candidate.value.clone()).ok());
        let has_override = override_value.is_some();
        let value = override_value.unwrap_or(fallback);
        let source = if has_override {
            candidate.map(|candidate| candidate.source.clone())
        } else {
            None
        }
        .unwrap_or_else(|| crate::config_reads::origin_source(origins.get(key), key));
        Sourced { value, source }
    }
    ReviewPolicy {
        later_rounds: effective(
            settings.later_rounds,
            REVIEW_LATER_ROUNDS_KEY,
            knobs,
            origins,
        ),
        adjudicator_route: effective(
            settings.adjudicator_route.clone(),
            REVIEW_ADJUDICATOR_ROUTE_KEY,
            knobs,
            origins,
        ),
        accept_below: effective(
            settings.accept_below,
            REVIEW_ACCEPT_BELOW_KEY,
            knobs,
            origins,
        ),
        evidence_retries: effective(
            settings.evidence_retries,
            REVIEW_EVIDENCE_RETRIES_KEY,
            knobs,
            origins,
        ),
        lens_retry: effective(settings.lens_retry, REVIEW_LENS_RETRY_KEY, knobs, origins),
    }
}

fn project_review_policy(
    mut policy: ReviewPolicy,
    state: &Path,
    project: Option<&Path>,
) -> ReviewPolicy {
    let (layers, _) = provenant_config::overlays::read_layers(state, project, "review");
    for layer in layers {
        if layer.path == state.join("config.toml") {
            continue;
        }
        let Ok(document) = layer.text.parse::<toml_edit::DocumentMut>() else {
            continue;
        };
        let Some(review_table) = document
            .get("domains")
            .and_then(toml_edit::Item::as_table)
            .and_then(|domains| domains.get("review"))
            .and_then(toml_edit::Item::as_table)
            .cloned()
        else {
            continue;
        };
        let Ok(settings) = toml_edit::de::from_document::<
            provenant_fabric_runtime::config::ReviewSettings,
        >(toml_edit::DocumentMut::from(review_table.clone())) else {
            continue;
        };
        if !settings.validate().is_empty() {
            continue;
        }
        let source = |field: &str| Source {
            layer: layer.source,
            origin: Some(format!("{}#domains.review.{field}", layer.path.display())),
        };
        macro_rules! apply_field {
            ($name:literal, $field:ident) => {
                if review_table.get($name).is_some() {
                    policy.$field = Sourced {
                        value: settings.$field,
                        source: source($name),
                    };
                }
            };
        }
        apply_field!("later_rounds", later_rounds);
        apply_field!("adjudicator_route", adjudicator_route);
        apply_field!("accept_below", accept_below);
        apply_field!("evidence_retries", evidence_retries);
        apply_field!("lens_retry", lens_retry);
    }
    policy
}
pub(super) fn canonical(path: &Path) -> Result<PathBuf, EdgeError> {
    Ok(provenant_os::fs::canonicalize(path)?)
}

mod lane_project;

fn has_git_common_layout(path: &Path) -> bool {
    path.file_name() == Some(OsStr::new(".git"))
        || (path.join("HEAD").is_file() && path.join("objects").is_dir())
}

fn registered_project_roots_for_repository(
    binding: &provenant_config::ProjectBinding,
    repo: &provenant_confinement::repo::VerifiedRepo,
    executor: &dyn Executor,
    utility: &provenant_confinement::UtilityRuntime,
    cancel: &CancelToken,
) -> Result<(PathBuf, Option<PathBuf>), EdgeError> {
    if !has_git_common_layout(binding.common_dir()) {
        let root = canonical(binding.common_dir())?;
        return Ok((root.clone(), Some(root)));
    }
    let layout = utility.project_layout_for_bound_repository(
        executor,
        repo,
        binding,
        provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))?,
        cancel,
    )?;
    Ok((
        layout.root().to_path_buf(),
        layout.project().map(Path::to_path_buf),
    ))
}

fn registered_project_roots_from_binding(
    binding: &provenant_config::ProjectBinding,
    executor: &dyn Executor,
    utility: &provenant_confinement::UtilityRuntime,
    cancel: &CancelToken,
) -> Result<(PathBuf, Option<PathBuf>), EdgeError> {
    if !has_git_common_layout(binding.common_dir()) {
        let root = canonical(binding.common_dir())?;
        return Ok((root.clone(), Some(root)));
    }
    let layout = utility.project_layout_for_binding(
        executor,
        binding,
        provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))?,
        cancel,
    )?;
    Ok((
        layout.root().to_path_buf(),
        layout.project().map(Path::to_path_buf),
    ))
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
    std::env::var_os(provenant_core::consumer_env::PARENT_ATTEMPT_ID).is_some()
        || std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV).is_some()
        || std::env::var_os(provenant_core::consumer_env::LANE_DEPTH)
            .is_some_and(|depth| depth != "0")
}

/// An explicit, harness-neutral chair session for the chair log. Without it,
/// settlement finds the session the chair saved (`chair_session_for`).
pub fn current_session() -> Option<String> {
    std::env::var(provenant_core::consumer_env::SESSION_ID)
        .ok()
        .filter(|value| !value.is_empty())
}

/// Observe the invoking harness rather than the short-lived command process.
pub fn chair_process() -> Option<provenant_records::custody::HolderProcess> {
    use provenant_supervisor::census::{
        ProcessObservation, ProcessObserver, SystemProcessObserver, current_process,
    };
    let ProcessObservation::Present(command) = current_process() else {
        return None;
    };
    let ProcessObservation::Present(parent) =
        SystemProcessObserver.observe_topology(command.parent_pid)
    else {
        return None;
    };
    Some(provenant_records::custody::HolderProcess {
        boot: parent.identity.boot,
        pid: parent.identity.pid,
        native_start: parent.identity.native_start,
    })
}
pub fn current_chair(context: &Context) -> Result<provenant_records::custody::Chair, EdgeError> {
    let inbox = &context.identity.inbox;
    let actor = inbox.actor;
    Ok(provenant_records::custody::Chair {
        extra: Default::default(),
        host: inbox.host,
        actor,
        session: current_session().or_else(|| {
            provenant_workflow::session::chair_session_for(
                &context.roots.state,
                context.identity.project,
                actor,
            )
            .map(|s| s.as_str().to_owned())
        }),
        process: chair_process(),
    })
}

/// A host chair session with its source explanation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostChairSession {
    pub session: String,
    pub source: String,
}

/// Rank the configured peer names close to `name` with the shared ranker.
fn close_host_names<'a>(name: &str, names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    provenant_core::close_matches(name, names)
        .into_iter()
        .map(str::to_owned)
        .collect()
}

impl Context {
    /// The host chair session when known, and the source it came from.
    pub fn host_chair_session(&self) -> Option<HostChairSession> {
        if let Some(session) = current_session() {
            return Some(HostChairSession {
                session,
                source: format!(
                    "environment variable {}",
                    provenant_core::consumer_env::SESSION_ID
                ),
            });
        }
        let actor = self.identity.inbox.actor;
        if let Some(session) = provenant_workflow::session::chair_session_for(
            &self.roots.state,
            self.identity.project,
            actor,
        ) {
            return Some(HostChairSession {
                session: session.as_str().to_owned(),
                source: format!("saved chair session for actor {actor}"),
            });
        }
        None
    }
}

/// Record a lifecycle acquisition without turning advisory custody into admission.
pub fn acquire_custody(
    context: &Context,
    holder: &provenant_records::custody::Holder,
    subjects: &[provenant_records::custody::Subject],
) {
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = provenant_fabric_store::Request {
        clock: &clock,
        cancel: &cancel,
    };
    match provenant_fabric_runtime::custody::acquire(
        &context.roots.state,
        context.identity.project,
        holder,
        subjects,
        provenant_records::custody::Origin::Provenant,
        &call,
    ) {
        Ok(changes) => provenant_fabric_runtime::custody::warn(&changes),
        Err(error) => provenant_fabric_runtime::custody::warning(format_args!(
            "custody acquisition pending ({error}); use provenant adopt KIND:ID"
        )),
    }
}
/// Observe a checkout identity without creating state during previews.
pub fn inspect_checkout_subject(
    roots: &Roots,
    path: &Path,
) -> Result<Option<provenant_records::custody::Subject>, EdgeError> {
    let repo = custody_checkout(roots, path)?;
    match provenant_confinement::worktrees::worktree_subject(&repo) {
        Ok(subject) => Ok(Some(subject)),
        Err(provenant_confinement::worktrees::WorktreeError::Filesystem(error))
            if error.is_not_found() =>
        {
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

/// Verify the nested creator receipt and capture the exact checkout incarnation
/// through held Git handles. The launch seals this subject through occupancy.
pub fn provisioned_checkout_subject(
    roots: &Roots,
    path: &Path,
    owner: &provenant_core::Digest,
) -> Result<provenant_records::custody::Subject, EdgeError> {
    let repo = custody_checkout(roots, path)?;
    let receipt = repo
        .private_dir()
        .read_stable(OsStr::new("provenant-provision-owner"), 256)?;
    if receipt != owner.to_string().as_bytes() {
        return Err(EdgeError::Invalid);
    }
    let subject = provenant_confinement::worktrees::worktree_subject(&repo)?;
    repo.revalidate_identity()
        .map_err(provenant_confinement::worktrees::WorktreeError::from)?;
    Ok(subject)
}

/// Acquire through the verified Git boundary shared by creation and launch.
pub fn checkout_subject(
    roots: &Roots,
    path: &Path,
) -> Result<provenant_records::custody::Subject, EdgeError> {
    let repo = custody_checkout(roots, path)?;
    Ok(provenant_confinement::worktrees::acquire_worktree_subject(
        &repo,
    )?)
}

fn custody_checkout(
    roots: &Roots,
    path: &Path,
) -> Result<provenant_confinement::repo::VerifiedRepo, EdgeError> {
    use provenant_confinement::{UtilityBudget, UtilityRuntime};
    let git = provenant_os::git::git_executable().ok_or(EdgeError::GitUnavailable)?;
    let runtime = UtilityRuntime::read_only(git, roots.state.as_path())?;
    let executor = SystemExecutor::new(1)?;
    let cancel = CancelToken::new();
    let budget = UtilityBudget::bounded(Duration::from_secs(5))?;
    Ok(runtime.open_repo(&executor, path, budget, &cancel)?)
}

pub fn checkout_branch(roots: &Roots, path: &Path) -> Result<Option<String>, EdgeError> {
    use provenant_confinement::repo::GitRevision;
    use provenant_confinement::{GitRead, UtilityBudget, UtilityRuntime};
    let git = provenant_os::git::git_executable().ok_or(EdgeError::GitUnavailable)?;
    let runtime = UtilityRuntime::read_only(git, roots.state.as_path())?;
    let executor = SystemExecutor::new(1)?;
    let cancel = CancelToken::new();
    let budget = UtilityBudget::bounded(Duration::from_secs(5))?;
    let repo = runtime.open_read_repo(&executor, path, budget, &cancel)?;
    let result = provenant_confinement::run_read_utility(
        &executor,
        GitRead::SymbolicRevision {
            revision: GitRevision::new("HEAD").map_err(|_| EdgeError::Invalid)?,
        },
        &repo,
        &runtime,
        budget,
        &cancel,
    )?;
    if !matches!(result.termination, provenant_os::exec::Termination::Exit(0)) {
        return Ok(None);
    }
    let reference = String::from_utf8(result.stdout.bytes).map_err(|_| EdgeError::Invalid)?;
    Ok(reference
        .trim()
        .strip_prefix("refs/heads/")
        .map(str::to_owned))
}

fn decide_timeout_source(
    origins: &std::collections::BTreeMap<String, provenant_config::Origin>,
) -> provenant_records::Layer {
    use provenant_records::Layer;
    match origins.get("domains.fabric.graph.decide_timeout_seconds") {
        Some(provenant_config::Origin::User { .. }) => Layer::Host,
        Some(provenant_config::Origin::Compiled { .. }) | None => Layer::Default,
    }
}

fn prompt_limit_source(
    origins: &std::collections::BTreeMap<String, provenant_config::Origin>,
) -> provenant_records::Layer {
    use provenant_records::Layer;
    match origins.get("domains.fabric.graph.limits.node_prompt_bytes") {
        Some(provenant_config::Origin::Compiled { .. }) => Layer::Default,
        Some(provenant_config::Origin::User { .. }) => Layer::Host,
        None => Layer::Detected,
    }
}

/// Read the projection setting once, keeping its layer and origin so a
/// mirrored read and an unlocatable writer can both name the setting that
/// selected the behaviour.
fn projection_from_knobs(
    knobs: &std::collections::BTreeMap<String, Sourced<serde_json::Value>>,
) -> Sourced<ProjectionMode> {
    knobs
        .get(PROJECTION_KEY)
        .map(|knob| Sourced {
            value: knob
                .value
                .as_str()
                .map(ProjectionMode::parse)
                .unwrap_or(ProjectionMode::Auto),
            source: knob.source.clone(),
        })
        .unwrap_or_else(|| Sourced {
            value: ProjectionMode::Auto,
            source: Source {
                layer: provenant_records::Layer::Default,
                origin: None,
            },
        })
}

impl Context {
    /// Directory selected for this project context, which may be a linked checkout.
    pub fn selected_project(&self) -> &Path {
        &self.cwd
    }

    pub fn open(roots: Roots, home: &Path, cwd: &Path) -> Result<Self, EdgeError> {
        if in_lane() {
            return Self::open_in_lane(roots, home, cwd, lane_project::LaneHost::Bind);
        }
        // Validate configuration before allocating identities or opening SQLite.
        let resolved = crate::config_reads::read_for(&roots, "fabric context")?;
        let graph_prompt_limit_source = prompt_limit_source(&resolved.origins);
        let graph_decide_timeout_source = decide_timeout_source(&resolved.origins);
        let local_observation = crate::wait::observation_settings(&resolved);
        let review_policy_base =
            review_policy(&resolved.domains.review, &resolved.knobs, &resolved.origins);
        let config = resolved.domains.fabric;
        private_dir(roots.state.as_path(), roots.state.as_path())?;
        let utility_home = roots.state.as_path().join("tmp").join("git-home");
        private_dir(roots.state.as_path(), &utility_home)?;
        let git = provenant_os::git::git_executable().ok_or(EdgeError::GitUnavailable)?;
        let runtime =
            provenant_confinement::UtilityRuntime::new(&git, &utility_home, roots.state.as_path())?
                .with_lane_roots(provenant_confinement::utility::lane_state_roots(
                    roots.state.as_path(),
                ));
        let executor = SystemExecutor::new(1)?;
        let budget = provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))?;
        let discovered = runtime.project_location(&executor, cwd, budget, &CancelToken::new())?;
        let is_git = discovered.repository.is_some();
        let common_dir = discovered.location.common_dir().to_path_buf();
        let binding = provenant_config::bind_project(&roots, discovered.location.clone())?;
        let (project_root, project_config_root) = if is_git {
            let repository = discovered.repository.as_ref().ok_or(EdgeError::Invalid)?;
            registered_project_roots_for_repository(
                &binding,
                repository,
                &executor,
                &runtime,
                &CancelToken::new(),
            )?
        } else {
            (common_dir.clone(), Some(common_dir.clone()))
        };
        let review_policy = project_review_policy(
            review_policy_base,
            roots.state.as_path(),
            Some(&project_root),
        );
        let port_table = provenant_config::PortTable::read_for(
            project_config_root.as_deref(),
            roots.state.as_path(),
        )?;
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
            std::env::var_os(provenant_core::consumer_env::AGENT_LABEL).as_deref(),
            active_harness_client(),
            key,
        )?;
        Ok(Self {
            inspection_only: false,
            roots,
            identity,
            config,
            review_policy,
            projection: projection_from_knobs(&resolved.knobs),
            local_observation,
            port_table,
            project_root,
            graph_prompt_limit_source,
            graph_decide_timeout_source,
            in_lane: false,
            executor,
            home: home.to_path_buf(),
            cwd: cwd.to_path_buf(),
        })
    }
    /// Resolve existing identity and configuration without creating state.
    pub fn open_read_only(roots: Roots, home: &Path, cwd: &Path) -> Result<Self, EdgeError> {
        if in_lane() {
            return Self::open_in_lane(roots, home, cwd, lane_project::LaneHost::Lookup);
        }
        let resolved = crate::config_reads::read_for(&roots, "fabric context")?;
        let graph_prompt_limit_source = prompt_limit_source(&resolved.origins);
        let graph_decide_timeout_source = decide_timeout_source(&resolved.origins);
        let local_observation = crate::wait::observation_settings(&resolved);
        let review_policy_base =
            review_policy(&resolved.domains.review, &resolved.knobs, &resolved.origins);
        let config = resolved.domains.fabric;
        let git = provenant_os::git::git_executable().ok_or(EdgeError::GitUnavailable)?;
        let runtime = provenant_confinement::UtilityRuntime::read_only(git, roots.state.as_path())?;
        let executor = SystemExecutor::new(1)?;
        let budget = provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))?;
        let discovered = runtime.project_location(&executor, cwd, budget, &CancelToken::new())?;
        let is_git = discovered.repository.is_some();
        let common_dir = discovered.location.common_dir().to_path_buf();
        let binding = provenant_config::lookup_project(&roots, &discovered.location)?
            .ok_or(EdgeError::Invalid)?;
        let (project_root, project_config_root) = if is_git {
            let repository = discovered.repository.as_ref().ok_or(EdgeError::Invalid)?;
            registered_project_roots_for_repository(
                &binding,
                repository,
                &executor,
                &runtime,
                &CancelToken::new(),
            )?
        } else {
            (common_dir.clone(), Some(common_dir.clone()))
        };
        let review_policy = project_review_policy(
            review_policy_base,
            roots.state.as_path(),
            Some(&project_root),
        );
        let port_table = provenant_config::PortTable::read_for(
            project_config_root.as_deref(),
            roots.state.as_path(),
        )?;
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
            std::env::var_os(provenant_core::consumer_env::AGENT_LABEL).as_deref(),
            active_harness_client(),
            key,
        )?;
        Ok(Self {
            inspection_only: false,
            roots,
            identity,
            config,
            review_policy,
            projection: projection_from_knobs(&resolved.knobs),
            local_observation,
            port_table,
            project_root,
            graph_prompt_limit_source,
            graph_decide_timeout_source,
            in_lane: in_lane(),
            executor,
            home: home.to_path_buf(),
            cwd: cwd.to_path_buf(),
        })
    }
    /// Inspect templates and compile explanations before this host has an identity.
    /// The placeholder identity is never persisted or used to launch work.
    pub fn open_inspection(roots: Roots, home: &Path, cwd: &Path) -> Result<Self, EdgeError> {
        let resolved = crate::config_reads::read_for(&roots, "fabric context")?;
        let graph_prompt_limit_source = prompt_limit_source(&resolved.origins);
        let graph_decide_timeout_source = decide_timeout_source(&resolved.origins);
        let local_observation = crate::wait::observation_settings(&resolved);
        let review_policy_base =
            review_policy(&resolved.domains.review, &resolved.knobs, &resolved.origins);
        let config = resolved.domains.fabric;
        let git = provenant_os::git::git_executable().ok_or(EdgeError::GitUnavailable)?;
        let runtime = provenant_confinement::UtilityRuntime::read_only(git, roots.state.as_path())?;
        let executor = SystemExecutor::new(1)?;
        let budget = provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))?;
        let discovered = runtime.project_location(&executor, cwd, budget, &CancelToken::new())?;
        let state_present = roots.state.as_path().try_exists()?;
        let binding = if state_present {
            provenant_config::lookup_project(&roots, &discovered.location)?
        } else {
            None
        };
        let (project_root, project_config_root) = match (&binding, &discovered.repository) {
            (Some(binding), Some(repo)) => registered_project_roots_for_repository(
                binding,
                repo,
                &executor,
                &runtime,
                &CancelToken::new(),
            )?,
            (None, Some(repo)) => {
                let read_repo = runtime.open_read_repo(
                    &executor,
                    repo.root_path(),
                    budget,
                    &CancelToken::new(),
                )?;
                let layout = provenant_confinement::worktrees::project_layout_for_checkout(
                    &executor,
                    &read_repo,
                    &runtime,
                    budget,
                    &CancelToken::new(),
                )?;
                (
                    layout.root().to_path_buf(),
                    layout.project().map(Path::to_path_buf),
                )
            }
            (Some(binding), None) if has_git_common_layout(binding.common_dir()) => {
                registered_project_roots_from_binding(
                    binding,
                    &executor,
                    &runtime,
                    &CancelToken::new(),
                )?
            }
            (Some(binding), None) => {
                let root = binding.common_dir().to_path_buf();
                (root.clone(), Some(root))
            }
            (None, None) => {
                let root = discovered.location.common_dir().to_path_buf();
                (root.clone(), Some(root))
            }
        };
        let review_policy = project_review_policy(
            review_policy_base,
            roots.state.as_path(),
            Some(&project_root),
        );
        let port_table = provenant_config::PortTable::read_for(
            project_config_root.as_deref(),
            roots.state.as_path(),
        )?;
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
        let client = active_harness_client();
        let identity = Identity {
            project,
            host,
            inbox: provenant_fabric_runtime::identity::inbox_address(
                host,
                &config,
                std::env::var_os(provenant_core::consumer_env::AGENT_LABEL).as_deref(),
                client,
            )?,
            client: client.unwrap_or(&config.client_label).to_owned(),
            cwd: AbsolutePath::new(cwd).map_err(|_| EdgeError::Invalid)?,
            common_dir: AbsolutePath::new(common_dir).map_err(|_| EdgeError::Invalid)?,
            federation: None,
        };
        Ok(Self {
            inspection_only: true,
            roots,
            identity,
            config,
            review_policy,
            projection: projection_from_knobs(&resolved.knobs),
            local_observation,
            port_table,
            project_root,
            graph_prompt_limit_source,
            graph_decide_timeout_source,
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
        let git = provenant_os::git::git_executable().ok_or(EdgeError::GitUnavailable)?;
        let executor = SystemExecutor::new(1)?;
        let runtime =
            provenant_confinement::UtilityRuntime::read_only(&git, roots.state.as_path())?
                .with_lane_roots(provenant_confinement::utility::lane_state_roots(
                    roots.state.as_path(),
                ));
        let (project_root, project_config_root) = registered_project_roots_from_binding(
            binding,
            &executor,
            &runtime,
            &CancelToken::new(),
        )?;
        // Git metadata has no checkout when inventory proves a bare repository.
        // Keep its context host-only instead of inventing one from the path.
        let cwd = project_root.clone();
        let port_table = provenant_config::PortTable::read_for(
            project_config_root.as_deref(),
            roots.state.as_path(),
        )?;
        let resolved = crate::config_reads::read_for(&roots, "fabric context")?;
        let graph_prompt_limit_source = prompt_limit_source(&resolved.origins);
        let graph_decide_timeout_source = decide_timeout_source(&resolved.origins);
        let local_observation = crate::wait::observation_settings(&resolved);
        let review_policy = project_review_policy(
            review_policy(&resolved.domains.review, &resolved.knobs, &resolved.origins),
            roots.state.as_path(),
            Some(&project_root),
        );
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
            &cwd,
            &config,
            std::env::var_os(provenant_core::consumer_env::AGENT_LABEL).as_deref(),
            active_harness_client(),
            key,
        )?;
        Ok(Self {
            inspection_only: false,
            roots,
            identity,
            config,
            review_policy,
            projection: projection_from_knobs(&resolved.knobs),
            local_observation,
            port_table,
            project_root,
            graph_prompt_limit_source,
            graph_decide_timeout_source,
            in_lane: in_lane(),
            executor,
            home: home.to_path_buf(),
            cwd,
        })
    }

    /// Resolve the stable allocation owner for an existing project binding.
    /// A linked checkout is never promoted to project owner by selection.
    pub fn project_root_for_registered_project(
        roots: &Roots,
        binding: &provenant_config::ProjectBinding,
    ) -> Result<PathBuf, EdgeError> {
        let location = provenant_config::ProjectLocation::new(binding.common_dir().to_path_buf())?;
        let registered =
            provenant_config::lookup_project(roots, &location)?.ok_or(EdgeError::Invalid)?;
        if registered.project_id() != binding.project_id() {
            return Err(EdgeError::Invalid);
        }
        let git = provenant_os::git::git_executable().ok_or(EdgeError::GitUnavailable)?;
        let executor = SystemExecutor::new(1)?;
        let runtime =
            provenant_confinement::UtilityRuntime::read_only(&git, roots.state.as_path())?
                .with_lane_roots(provenant_confinement::utility::lane_state_roots(
                    roots.state.as_path(),
                ));
        Ok(registered_project_roots_from_binding(
            binding,
            &executor,
            &runtime,
            &CancelToken::new(),
        )?
        .0)
    }

    /// Resolve the allocation root and verified metadata boundary for a
    /// peer locator before a missing project binding can be written.
    pub fn workspace_for_common_location(
        roots: &Roots,
        common: &Path,
        binding: Option<&provenant_config::ProjectBinding>,
    ) -> Result<(PathBuf, Option<PathBuf>), EdgeError> {
        let canonical_common = canonical(common)?;
        if canonical_common != common {
            return Err(EdgeError::Invalid);
        }
        if let Some(binding) = binding {
            if binding.common_dir() != common {
                return Err(EdgeError::Invalid);
            }
            let location = provenant_config::ProjectLocation::new(common.to_path_buf())?;
            let registered =
                provenant_config::lookup_project(roots, &location)?.ok_or(EdgeError::Invalid)?;
            if registered.project_id() != binding.project_id() {
                return Err(EdgeError::Invalid);
            }
            let root = Self::project_root_for_registered_project(roots, binding)?;
            let metadata = has_git_common_layout(common).then(|| common.to_path_buf());
            return Ok((root, metadata));
        }
        if !has_git_common_layout(common) {
            return Ok((canonical_common, None));
        }
        let git = provenant_os::git::git_executable().ok_or(EdgeError::GitUnavailable)?;
        let executor = SystemExecutor::new(1)?;
        let runtime =
            provenant_confinement::UtilityRuntime::read_only(&git, roots.state.as_path())?
                .with_lane_roots(provenant_confinement::utility::lane_state_roots(
                    roots.state.as_path(),
                ));
        let layout = runtime.project_layout_for_common(
            &executor,
            common,
            provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))?,
            &CancelToken::new(),
        )?;
        Ok((layout.root().to_path_buf(), Some(common.to_path_buf())))
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
        let attempt = std::env::var(provenant_core::consumer_env::PARENT_ATTEMPT_ID)
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
            std::env::var_os(provenant_core::consumer_env::AGENT_LABEL).as_deref(),
            harness_client(client).map(|harness| harness.spec().identity.name),
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
    /// The configured peer labels and host IDs close to `name`, ranked by the
    /// one shared name ranker. Labels and IDs are the only names a host
    /// refusal may show; no secret or path ever enters the universe.
    pub fn host_candidates(&self, name: &str) -> Vec<String> {
        let mut universe: Vec<String> = self.config.peers.keys().cloned().collect();
        universe.extend(self.config.peers.values().map(|peer| peer.host.to_string()));
        close_host_names(name, universe.iter().map(String::as_str))
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

    /// Resolve a project for a peer call when it is not home-relative. Writers
    /// always need a locatable project; only readers may cross through a
    /// mirrored snapshot under `auto` or `mirror`.
    fn project_for_peer(
        &self,
        host: HostId,
        action: &PeerAction,
        peer: &str,
        cancel: &CancelToken,
    ) -> Result<ProjectLocator, PeerFailure> {
        if !self
            .projection
            .value
            .allows_mirror(is_reader_action(action))
        {
            return Err(unlocatable(&self.projection, host, peer));
        }
        let git = provenant_os::git::git_executable()
            .ok_or_else(|| mirror_failure(&self.projection, "Git executable is unavailable"))?;
        let snapshot = mirror::mirror_snapshot(
            &self.executor,
            mirror::MirrorRequest {
                git_executable: &git,
                worktree: &self.cwd,
                scratch: &self.roots.state.as_path().join("tmp").join("mirror"),
                state: self.roots.state.as_path(),
                origin_host: &self.identity.host.to_string(),
                project_id: &self.identity.project.to_string(),
                cancel,
            },
        )
        .map_err(|error| mirror_failure(&self.projection, &error.to_string()))?;
        warn_mirror(&self.projection, &snapshot, peer);
        // The peer binds the bundle advertised at the snapshot commit instead
        // of locating a live checkout. The key is derived from that commit, so
        // two snapshots of the same tree at different times are distinct.
        let key_text = format!(
            "mirror:{}:{}:{}",
            self.identity.host, snapshot.project_id, snapshot.commit
        );
        let key: CrossHostProjectKey = serde_json::from_value(serde_json::Value::String(key_text))
            .map_err(|_| mirror_failure(&self.projection, "mirror key could not be encoded"))?;
        Ok(ProjectLocator {
            extra: Default::default(),
            key,
            common_dir: format!("mirrors/{}", snapshot.project_id),
        })
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
        let project = match ProjectLocator::new(&self.identity, &self.home) {
            Ok(project) => project,
            Err(_) => self.project_for_peer(host, &action, &peer.destination, cancel)?,
        };
        // Graph operation replay is scoped to the actor that first launched
        // the attempt, even when another agent later resumes its graph.
        // Keep that namespace under the authenticated sending host rather
        // than adopting a caller-supplied actor ID on the receiving host.
        let graph_actor = match &action {
            PeerAction::Submit { graph_context, .. } => Some(&graph_context.launch_actor),
            PeerAction::Receipt { graph_context, .. } => {
                graph_context.as_ref().map(|context| &context.launch_actor)
            }
            PeerAction::CancelSubmission { graph_context, .. } => Some(&graph_context.launch_actor),
            PeerAction::Output {
                audience:
                    provenant_fabric_runtime::hosts::OutputAudience::GraphExecution {
                        launch_actor, ..
                    },
                ..
            } => Some(launch_actor),
            PeerAction::Inbox {
                input:
                    provenant_fabric_runtime::inbox::InboxInput::Steer {
                        graph_context: Some(context),
                        ..
                    },
                ..
            } => Some(&context.launch_actor),
            _ => None,
        };
        let agent = graph_actor
            .map(|actor| format!("graph-{actor}"))
            .unwrap_or_else(|| self.identity.inbox.agent.clone());
        let request = PeerRequest::new(host, self.identity.host, agent, project, action);
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
                request.agent.clone(),
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

/// Format-2 health always reports the supported run operations.
fn check_submit_health(peer: &str, result: PeerResult) -> Result<(), PeerFailure> {
    let PeerResult::Health { data, .. } = result else {
        return Ok(());
    };
    if !data.run_capabilities.submit {
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
    explicit.or_else(|| {
        let present = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
        let detected: Vec<_> = provenant_records::ADAPTER_SPECS
            .iter()
            .filter(|spec| {
                spec.launch.client_env_markers.is_some_and(|markers| {
                    markers
                        .equals_one
                        .is_some_and(|name| std::env::var(name).is_ok_and(|value| value == "1"))
                        || markers.presence.iter().any(|name| present(name))
                })
            })
            .collect();
        match detected.as_slice() {
            [spec] => Some(spec.id),
            _ => None,
        }
    })
}
fn active_harness_client() -> Option<&'static str> {
    harness_client(None).map(|harness| harness.spec().identity.name)
}

/// A peer call failure: the stable error plus, for transport failures, the
/// peer and cause. The detail is message text only; the wire codes are unchanged.
#[derive(Debug)]
pub struct PeerFailure {
    pub error: Box<FacadeError>,
    pub detail: Option<String>,
}
impl PeerFailure {
    fn new(error: FacadeError, detail: impl Into<String>) -> Self {
        Self {
            error: Box::new(error),
            detail: Some(detail.into()),
        }
    }
}
impl From<FacadeError> for PeerFailure {
    fn from(error: FacadeError) -> Self {
        Self {
            error: Box::new(error),
            detail: None,
        }
    }
}
/// Read-only peer actions may be served from a mirrored snapshot. Everything
/// that mutates state needs a locatable project on the peer.
fn is_reader_action(action: &PeerAction) -> bool {
    use provenant_fabric_runtime::hosts::HostReadInput;
    match action {
        PeerAction::HostRead {
            input: HostReadInput::Events {
                consumer: Some(_), ..
            },
            ..
        } => false,
        PeerAction::HostRead {
            input: HostReadInput::EventsAck { .. },
            ..
        } => false,
        PeerAction::HostRead { .. } => true,
        PeerAction::Health { .. }
        | PeerAction::Doctor { .. }
        | PeerAction::ConsoleBoard { .. }
        | PeerAction::ConsoleTree { .. }
        | PeerAction::ConsoleDetail { .. }
        | PeerAction::ConsoleInbox { .. }
        | PeerAction::RunStatuses { .. }
        | PeerAction::Output { .. }
        | PeerAction::RunEvents { .. }
        | PeerAction::Lanes { .. }
        | PeerAction::LaneWaits { .. } => true,
        _ => false,
    }
}

fn setting_ref(projection: &Sourced<ProjectionMode>) -> SettingRef {
    SettingRef {
        key: PROJECTION_KEY.to_owned(),
        value: projection.value.as_str().to_owned(),
        layer: projection.source.layer,
        origin: projection
            .source
            .origin
            .clone()
            .unwrap_or_else(|| "shipped default".to_owned()),
    }
}

/// A writer whose project cannot be located: an operation error that names the
/// setting and the way out, never a policy refusal.
fn unlocatable(projection: &Sourced<ProjectionMode>, host: HostId, peer: &str) -> PeerFailure {
    let setting = setting_ref(projection);
    let detail = format!(
        "{} = {} ({}); project is outside the peer's home on host {host}",
        setting.key, setting.value, setting.origin
    );
    PeerFailure::new(
        FacadeError::RunKind {
            run_code: PEER_PROJECT_UNLOCATABLE.to_owned(),
            next: "run without --host".to_owned(),
            detail: Some(detail),
            resource: None,
        },
        format!("{peer}: project is outside the peer's home"),
    )
}

/// A snapshot could not be produced or verified. The transfer is retryable,
/// so it stays an operation error rather than a refusal.
fn mirror_failure(projection: &Sourced<ProjectionMode>, detail: &str) -> PeerFailure {
    let setting = setting_ref(projection);
    PeerFailure::new(
        FacadeError::RunKind {
            run_code: "PEER_MIRROR_FAILED".to_owned(),
            next: "retry the read, or run without --host".to_owned(),
            detail: Some(format!("{}: {detail}", setting.key)),
            resource: None,
        },
        detail.to_owned(),
    )
}

/// Tell a reader that it is seeing a snapshot, not live edits.
fn warn_mirror(
    projection: &Sourced<ProjectionMode>,
    snapshot: &mirror::MirrorSnapshot,
    peer: &str,
) {
    warn(
        Warning::new(
            WarningCode(PEER_SOURCE_SNAPSHOT),
            format!(
                "peer {peer} reads snapshot {} of project {}; live edits are not visible",
                snapshot.commit, snapshot.project_id
            ),
            projection.source.clone(),
        )
        .setting(setting_ref(projection))
        .repair(format!(
            "rerun this read after a writer commits to {}",
            snapshot.project_id
        )),
    );
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
        FacadeError::UnknownHost => (Outcome::Invalid, ErrorKind::UnknownHost),
        FacadeError::WorktreeNotApplicable => (Outcome::Invalid, ErrorKind::WorktreeNotApplicable),
        FacadeError::NotFound => (Outcome::Invalid, ErrorKind::NotFound),
        FacadeError::Conflict | FacadeError::GenerationConflict { .. } => {
            (Outcome::Refused, ErrorKind::Conflict)
        }
        FacadeError::NotTerminal => (Outcome::Refused, ErrorKind::Nonterminal),
        FacadeError::OperationConflict => (Outcome::Refused, ErrorKind::OperationConflict),
        FacadeError::Busy => (Outcome::Refused, ErrorKind::QueueFull),
        FacadeError::ConfinementRefused { .. }
        | FacadeError::GraphRelayRefused { .. }
        | FacadeError::RecordedRefusal { .. } => (Outcome::Refused, ErrorKind::Refused),
        // Custody is a lease: `admit_writer` only raises WriterBusy when the
        // owning lane set `lanes.worktree.sharing = refuse`. Shared and warn
        // modes proceed, so a refusal here always names that setting's owner.
        FacadeError::WriterBusy { .. } => (Outcome::Refused, ErrorKind::WriterBusy),
        FacadeError::CustodyRefused { .. } => (Outcome::Refused, ErrorKind::Refused),
        FacadeError::WriterLeaseHeld { .. } => (Outcome::Refused, ErrorKind::WriterLeaseHeld),
        FacadeError::WriterLockBusy { .. } => (Outcome::Refused, ErrorKind::WriterLockBusy),
        FacadeError::WriterUnknown => (Outcome::Refused, ErrorKind::WriterUnknown),
        FacadeError::CredentialRoot { .. } => (Outcome::Refused, ErrorKind::CredentialRoot),
        FacadeError::DispatchRefused { .. } => (Outcome::Refused, ErrorKind::Refused),
        FacadeError::GitMutationFailed { .. }
        | FacadeError::DispatchUnobserved { .. }
        | FacadeError::HostDeliveryStage { .. }
        | FacadeError::HostDeliveryLimit { .. } => (Outcome::Error, ErrorKind::Io),
        FacadeError::WorktreeMissing { .. } => (Outcome::Invalid, ErrorKind::WorktreeMissing),
        FacadeError::WorktreeUnregistered { .. } => {
            (Outcome::Refused, ErrorKind::WorktreeUnregistered)
        }
        FacadeError::BaseUnreachable | FacadeError::BaseFetchFailed { .. } => {
            (Outcome::Refused, ErrorKind::BaseUnreachable)
        }
        FacadeError::BranchExists => (Outcome::Refused, ErrorKind::BranchExists),
        FacadeError::LaneDepth => (Outcome::Refused, ErrorKind::LaneDepth),
        FacadeError::LaneCoordinationRefused => {
            (Outcome::Refused, ErrorKind::LaneCoordinationRefused)
        }
        FacadeError::OwnerUnavailable => (Outcome::Refused, ErrorKind::OwnerUnavailable),
        FacadeError::ProviderStateExpired => (Outcome::Refused, ErrorKind::ProviderStateExpired),
        FacadeError::Cancelled => (Outcome::Error, ErrorKind::Cancelled),
        FacadeError::StateCorrupt | FacadeError::StateCorruptDetail { .. } => {
            (Outcome::Error, ErrorKind::StateCorrupt)
        }
        FacadeError::UpgradeRefused => (Outcome::Refused, ErrorKind::UpgradeRefused),
        FacadeError::UpgradeRequired { .. } => (Outcome::Refused, ErrorKind::UpgradeRequired),
        FacadeError::Timeout => (Outcome::Error, ErrorKind::Timeout),
        FacadeError::RunKind { run_code, .. } => {
            let kind = ErrorKind::ALL
                .iter()
                .copied()
                .find(|kind| kind.code() == run_code)
                .unwrap_or(ErrorKind::Refused);
            let outcome = match kind.category() {
                "invalid" => Outcome::Invalid,
                "refused" => Outcome::Refused,
                _ => Outcome::Error,
            };
            (outcome, kind)
        }
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
        error: Box::new(FacadeError::from(error)),
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
#[path = "context_tests.rs"]
mod tests;
