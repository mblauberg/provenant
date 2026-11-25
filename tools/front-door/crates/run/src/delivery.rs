//! Host-only delivery. Credential environment travels on the private handoff
//! pipe, never in provider environment, launch records or the delivery receipt.
use crate::{
    DeliveryCause, DeliveryPrepareRefusal, DeliveryPrepareStep, HelperSafety, PathCheck, Result,
    RunError, WriterDeliveryReceipt, WriterDeliveryRequest, WriterDeliveryStage as Stage,
    WriterDeliveryState, WriterIntegration, WriterRequest,
};
use provenant_confinement::repo::GitRefName;
use provenant_core::GitOid;
use provenant_os::git_config::BranchField;
use provenant_os::{
    exec::{ExecOptions, StreamPolicy, SystemExecutor, Termination},
    fs::Directory,
};
use serde::{Deserialize, Serialize};
use std::{
    ffi::{OsStr, OsString},
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    time::Duration,
};

mod attempt;
/// An advisory that keeps delivery usable rather than refusing it.
/// Spec: docs/specs/cli.md#warnings-are-typed-values
mod origin_refresh;
pub(crate) use attempt::deliver_once;

#[cfg(test)]
pub(super) use origin_refresh::coalesced_origin_refresh_with_budget;
pub(super) use origin_refresh::{OriginRefreshRequest, coalesced_origin_refresh};

fn delivery_advisory(message: impl Into<String>) {
    use provenant_records::warning::{DELIVERY_REFRESH_FAILED, Warning, warn};
    warn(Warning::new(DELIVERY_REFRESH_FAILED, message.into()));
}

/// Build a forge context from explicit values for test fixtures, never for
/// production. Production resolves the context once through [`resolve_forge`]
/// and passes it down; a checkout without a remote has no context rather than
/// an empty remote name.
/// Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
#[cfg(any(test, feature = "fixture"))]
#[doc(hidden)]
pub fn forge_for(
    remote: &str,
    default_branch: &str,
    protected: &[&str],
) -> provenant_config::ForgeContext {
    use provenant_records::provenance::{Layer, Source, Sourced};
    assert!(
        !remote.trim().is_empty(),
        "a fixture forge context needs a non-empty remote name"
    );
    let source = Source {
        layer: Layer::Detected,
        origin: Some("explicit fixture values".to_owned()),
    };
    let mut names = vec![default_branch.to_owned()];
    names.extend(protected.iter().map(|name| (*name).to_owned()));
    names.sort();
    names.dedup();
    provenant_config::ForgeContext {
        remote: Sourced {
            value: remote.to_owned(),
            source: source.clone(),
        },
        default_branch: Sourced {
            value: default_branch.to_owned(),
            source: source.clone(),
        },
        protected: Sourced {
            value: names,
            source: source.clone(),
        },
        policy: Sourced {
            value: Default::default(),
            source,
        },
    }
}

/// Resolve the one forge context for a delivery-related command. Owners call
/// this once and pass the result down.
/// Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
pub fn resolve_forge(
    state: &Path,
    project: &Path,
    ports: &provenant_config::PortTable,
) -> Result<provenant_config::ForgeContext> {
    let invalid = |error: String| RunError::Input {
        kind: crate::ErrorKind::Invalid,
        message: error,
    };
    let policy = provenant_config::operation_policy(state, project, ports);
    let config = provenant_config::ForgeConfig::new(ports, (*policy).clone());
    let git =
        provenant_config::ForgeGit::new(project).map_err(|error| invalid(error.to_string()))?;
    provenant_config::ForgeContext::resolve(&config, &git)
        .map_err(|error| invalid(error.to_string()))
}

/// The one forge context for a command, resolved when a delivery check first
/// needs it. A command that delivers nothing, such as a read-only graph, never
/// asks for a remote, so a checkout without one is not refused.
/// Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
pub struct ForgeOnce<'a> {
    state: PathBuf,
    project: PathBuf,
    ports: &'a provenant_config::PortTable,
    context: std::cell::OnceCell<provenant_config::ForgeContext>,
}

impl<'a> ForgeOnce<'a> {
    pub fn new(state: &Path, project: &Path, ports: &'a provenant_config::PortTable) -> Self {
        Self {
            state: state.to_path_buf(),
            project: project.to_path_buf(),
            ports,
            context: std::cell::OnceCell::new(),
        }
    }

    /// The resolved context; the first call resolves it and later calls
    /// reuse it.
    pub fn get(&self) -> Result<&provenant_config::ForgeContext> {
        if let Some(context) = self.context.get() {
            return Ok(context);
        }
        let context = resolve_forge(&self.state, &self.project, self.ports)?;
        Ok(self.context.get_or_init(|| context))
    }
}

/// Launch delivery fields a test compares across stop-publish preparation:
/// git, gh, the launch binding digest, the stop binding digest and the
/// environment.
#[cfg(test)]
pub(crate) type LaunchStateForTest = (
    Option<PathBuf>,
    Option<PathBuf>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Vec<(Vec<u8>, Vec<u8>)>,
);

/// Transport-only context. Debug deliberately omits all environment values.
#[derive(Serialize, Deserialize)]
pub struct HostDeliveryContext {
    environment: Vec<(Vec<u8>, Vec<u8>)>,
    #[serde(default)]
    lane_access: Option<provenant_records::LaneAccessReceipt>,
    #[serde(default)]
    lane_sandboxed: Option<bool>,
    #[serde(default)]
    trusted_worktree: Option<PathBuf>,
    /// The host-owned repository a host lane's worktree was linked from: the
    /// primary checkout, or a bare/relocated common Git directory whose config
    /// is host-owned rather than another lane's writable area.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    host_project: Option<PathBuf>,
    /// Consulted while preparing delivery on the host, never by the guardian.
    /// Serialising the compiled rule table would overflow the handoff's holder
    /// context limit and turn a launched lane into an unconfirmed handoff.
    #[serde(skip)]
    policy: provenant_records::policy::Policy,
    tools: Option<HostTools>,
    bound: Option<BoundDelivery>,
    /// Separate launch-bound destination for stop preservation. It never shares
    /// the task's explicit delivery target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stop_publish_bound: Option<BoundDelivery>,
    /// Tools and environment that stop publication prepared for itself. The
    /// launch delivery fields above stay exactly as the launch prepared them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stop_publish_host: Option<StopPublishHost>,
    #[serde(skip)]
    lane_path: Option<FilteredLanePath>,
    /// Where the lane can write: its worktree and the project's lane areas.
    /// Host transport refuses configuration and programs located there.
    #[serde(default)]
    lane_roots: Vec<PathBuf>,
    #[serde(skip)]
    cancellation: Option<provenant_core::CancelToken>,
    #[serde(skip)]
    facts: provenant_confinement::worktrees::RepoFacts,
    /// Host state root whose port declarations name lane worktree roots. When
    /// unset, the captured PROVENANT_STATE_DIR or the default root is used.
    #[serde(skip)]
    host_state: Option<PathBuf>,
    /// The one forge context resolved by the owner for this command. When it is
    /// absent (older sealed holders), preparation resolves one from the host or
    /// project port table and falls back to the request's remote.
    /// Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
    #[serde(skip)]
    forge: Option<provenant_config::ForgeContext>,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BoundDelivery {
    /// Lane head when delivery was prepared, before the writer ran.
    base_head: GitOid,
    push_url: String,
    lane_branch: String,
    pull_request_repo: Option<String>,
    git_dir: PathBuf,
    /// Host repository authority, independent of lane-writable commondir.
    common_git_dir: PathBuf,
    host_cwd: PathBuf,
    /// Destination sealed at launch; a later request edit cannot redirect it.
    destination: WriterDeliveryRequest,
}
const BINDING_FILE: &str = "delivery-binding.json";
const STOP_PUBLISH_BINDING_FILE: &str = "stop-publish-binding.json";
pub(crate) const HEAD_MISMATCH: &str = "worktree head differs from sealed delivery head";
const BINDING_MISMATCH: &str = "delivery destination differs from the launch binding";
const BINDING_MISSING: &str = "delivery launch binding was not sealed";
const ORIGIN_REFRESH_LIMIT: u64 = 4096;
/// Bounded stderr tail kept in a delivery command cause. Redaction is off by
/// default by owner decision, so the text is passed through, not masked.
const DELIVERY_STDERR_LIMIT: usize = 1024;
pub(super) const ORIGIN_REFRESH_PRECOMPOSE_SETTING: &str = "core.precomposeunicode=true";
pub(super) const PREPARE_REASON_LIMIT: usize = 1024;
pub(super) const PREPARE_REASON_TRUNCATION_MARKER: &str = "...";
pub(super) const PR_BODY_LIMIT: usize = 60_000;
pub(super) const PR_BODY_TRUNCATION_NOTE: &str = "\n\n[Commit summary truncated.]\n";
pub(super) const HOST_GIT_HARDENING_CONFIG_ARGS: [&str; 4] = [
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "core.fsmonitor=false",
];

/// Recovery cannot repair these; retrying them cannot change the answer.
pub(crate) fn recovery_refused(error: &RunError) -> bool {
    matches!(
        error,
        RunError::Validation(HEAD_MISMATCH | BINDING_MISMATCH | BINDING_MISSING)
    )
}

/// The destination the host bound before the lane ran.
#[derive(Serialize, Deserialize)]
struct LaunchBinding {
    #[serde(flatten)]
    extra: provenant_records::ExtraFields,
    schema_version: u64,
    scope: provenant_records::custody::Scope,
    push_url: String,
    lane_branch: String,
    pull_request_repo: Option<String>,
    destination: WriterDeliveryRequest,
}
impl LaunchBinding {
    fn new(scope: provenant_records::custody::Scope, bound: &BoundDelivery) -> Self {
        Self {
            extra: Default::default(),
            schema_version: 1,
            scope,
            push_url: provenant_records::custody::endpoint_identity(&bound.push_url),
            lane_branch: bound.lane_branch.clone(),
            pull_request_repo: bound.pull_request_repo.clone(),
            destination: bound.destination.clone(),
        }
    }
}
impl PartialEq for LaunchBinding {
    fn eq(&self, other: &Self) -> bool {
        self.schema_version == other.schema_version
            && self.scope == other.scope
            && self.push_url == other.push_url
            && self.lane_branch == other.lane_branch
            && self.pull_request_repo == other.pull_request_repo
            && self.destination == other.destination
    }
}
impl Eq for LaunchBinding {}
/// Host state prepared only for stop publication.
#[derive(Clone, Serialize, Deserialize)]
struct StopPublishHost {
    environment: Vec<(Vec<u8>, Vec<u8>)>,
    tools: Option<HostTools>,
    #[serde(default)]
    lane_roots: Vec<PathBuf>,
    #[serde(skip)]
    lane_path: Option<FilteredLanePath>,
}

/// Exact host receipt for one graph PR publication.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphPublicationReceipt {
    pub branch: String,
    pub head: GitOid,
    pub pull_request: String,
}

/// Authenticated target details for updating an adopted pull request in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphAdoptionTarget<'a> {
    pub repository: &'a str,
    pub pull_request: u64,
    pub pull_request_url: &'a str,
    pub branch: &'a str,
    pub base_repository: &'a str,
    pub base_branch: &'a str,
}

struct AdoptedPullRequestObservation {
    url: String,
    head: GitOid,
}
impl PartialEq for StopPublishHost {
    fn eq(&self, other: &Self) -> bool {
        self.environment == other.environment
            && self.tools == other.tools
            && self.lane_roots == other.lane_roots
    }
}
impl Eq for StopPublishHost {}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct HostTools {
    git: PathBuf,
    gh: Option<PathBuf>,
}
#[derive(Serialize, Deserialize)]
struct PickupReceipt {
    #[serde(flatten)]
    extra: provenant_records::ExtraFields,
    schema_version: u64,
    request: WriterDeliveryRequest,
    receipt: WriterDeliveryReceipt,
}

fn publish_pickup(
    attempt: &Directory,
    request: &WriterDeliveryRequest,
    receipt: &WriterDeliveryReceipt,
) -> Result<()> {
    let extra = match attempt.read_stable(OsStr::new("delivery-pickup.json"), crate::RECORD_LIMIT) {
        Ok(raw) => crate::db::decode::<PickupReceipt>(&raw)?.extra,
        Err(error) if error.is_not_found() => Default::default(),
        Err(error) => return Err(error.into()),
    };
    attempt
        .atomic_write(
            OsStr::new("delivery-pickup.json"),
            ".pickup-",
            &crate::db::bytes(&PickupReceipt {
                extra,
                schema_version: 1,
                request: request.clone(),
                receipt: receipt.clone(),
            })?,
            provenant_os::fs::PublishMode::Replace,
        )?
        .require_durable()
        .map_err(RunError::from)
}
/// One owner for the lane PATH value and its automatic executable lookup.
/// Both the search directories and resolved symlink targets stay outside the
/// selected project and worktree.
#[derive(Clone)]
pub struct FilteredLanePath {
    path: OsString,
    directories: Vec<PathBuf>,
    excluded_roots: Vec<PathBuf>,
}

impl FilteredLanePath {
    /// Capture the retained requester PATH, or the host's captured search path
    /// when the request predates PATH retention. Ambient PATH is read only when
    /// no host handoff exists.
    pub fn for_requester(
        host: Option<&crate::HostLaunchContext>,
        project: &Path,
        worktree: &Path,
    ) -> std::io::Result<Self> {
        Self::for_requester_excluding_roots(host, &[project, worktree])
    }

    /// Capture the requester PATH and remove every project, executor, and
    /// worktree root that must not supply automatic tools to the lane.
    pub fn for_requester_excluding_roots(
        host: Option<&crate::HostLaunchContext>,
        excluded_roots: &[&Path],
    ) -> std::io::Result<Self> {
        let path = match host {
            Some(host) => match &host.lane_path {
                Some(path) => OsString::from_vec(path.clone()),
                None => std::env::join_paths(&host.executable_search_path)
                    .map_err(std::io::Error::other)?,
            },
            None => std::env::var_os("PATH").unwrap_or_default(),
        };
        Self::from_path_excluding_roots(&path, excluded_roots)
    }

    /// Filter and retain a PATH value for a host-owned delivery operation.
    pub fn from_path(path: &OsStr, project: &Path, worktree: &Path) -> std::io::Result<Self> {
        Self::from_path_excluding_roots(path, &[project, worktree])
    }

    /// Filter a PATH value against every verified root owned by the request.
    pub fn from_path_excluding_roots(path: &OsStr, roots: &[&Path]) -> std::io::Result<Self> {
        let mut excluded_roots = Vec::with_capacity(roots.len());
        for root in roots {
            let root = std::fs::canonicalize(root)?;
            if !excluded_roots.contains(&root) {
                excluded_roots.push(root);
            }
        }
        let mut directories = Vec::new();
        for directory in std::env::split_paths(path).filter(|path| path.is_absolute()) {
            let Ok(directory) = std::fs::canonicalize(directory) else {
                continue;
            };
            if excluded_roots
                .iter()
                .any(|root| directory.starts_with(root))
                || directories.contains(&directory)
            {
                continue;
            }
            directories.push(directory);
        }
        let path = std::env::join_paths(&directories).map_err(std::io::Error::other)?;
        Ok(Self {
            path,
            directories,
            excluded_roots,
        })
    }

    pub fn path(&self) -> &OsStr {
        &self.path
    }

    /// Find a bare executable name on the filtered PATH. The returned path is
    /// the original PATH candidate, preserving safe command symlinks.
    pub fn resolve(&self, name: &str) -> std::result::Result<PathBuf, String> {
        let name_path = Path::new(name);
        if name.is_empty()
            || name.contains('/')
            || name_path.components().count() != 1
            || !matches!(
                name_path.components().next(),
                Some(std::path::Component::Normal(_))
            )
        {
            return Err("automatic executable lookup requires a bare name".into());
        }
        for directory in &self.directories {
            let candidate = directory.join(name);
            if !candidate.is_file() || !provenant_os::sys::is_executable(&candidate) {
                continue;
            }
            let Ok(target) = std::fs::canonicalize(&candidate) else {
                continue;
            };
            if self
                .excluded_roots
                .iter()
                .any(|root| target.starts_with(root))
                || !target.is_file()
                || !provenant_os::sys::is_executable(&target)
            {
                continue;
            }
            return Ok(candidate);
        }
        Err(format!("{name} executable was not found on filtered PATH"))
    }
}

// Runtime cancellation is host-local, absent from handoffs and equality.
impl Clone for HostDeliveryContext {
    fn clone(&self) -> Self {
        Self {
            environment: self.environment.clone(),
            lane_access: self.lane_access.clone(),
            lane_sandboxed: self.lane_sandboxed,
            trusted_worktree: self.trusted_worktree.clone(),
            host_project: self.host_project.clone(),
            policy: self.policy.clone(),
            tools: self.tools.clone(),
            bound: self.bound.clone(),
            stop_publish_bound: self.stop_publish_bound.clone(),
            stop_publish_host: self.stop_publish_host.clone(),
            lane_path: self.lane_path.clone(),
            lane_roots: self.lane_roots.clone(),
            cancellation: self.cancellation.clone(),
            facts: self.facts.clone(),
            host_state: self.host_state.clone(),
            forge: self.forge.clone(),
        }
    }
}
impl PartialEq for HostDeliveryContext {
    fn eq(&self, other: &Self) -> bool {
        self.environment == other.environment
            && self.lane_access == other.lane_access
            && self.lane_sandboxed == other.lane_sandboxed
            && self.trusted_worktree == other.trusted_worktree
            && self.host_project == other.host_project
            && self.policy == other.policy
            && self.tools == other.tools
            && self.bound == other.bound
            && self.stop_publish_bound == other.stop_publish_bound
            && self.stop_publish_host == other.stop_publish_host
    }
}
impl Eq for HostDeliveryContext {}

impl std::fmt::Debug for HostDeliveryContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HostDeliveryContext([private handoff])")
    }
}

/// How a host command is run: config commands retry lock contention and keep
/// their failure's leaf cause; ordinary commands do neither.
#[derive(Clone, Copy)]
struct CommandMode {
    retry_config_lock: bool,
    preserve_reason: bool,
}

impl CommandMode {
    const ORDINARY: Self = Self {
        retry_config_lock: false,
        preserve_reason: false,
    };
    const CONFIG_READ: Self = Self {
        retry_config_lock: false,
        preserve_reason: true,
    };
    const CONFIG_WRITE: Self = Self {
        retry_config_lock: true,
        preserve_reason: true,
    };
}

impl HostDeliveryContext {
    /// Rebuild the delivery binding after a restart. The destination sealed at
    /// launch and the sealed head must both still hold; a mismatch is final.
    pub(crate) fn prepare_recovery(
        self,
        writer: &WriterRequest,
        request: &WriterDeliveryRequest,
        attempt: &Directory,
        scope: provenant_records::custody::Scope,
        head: GitOid,
        base: GitOid,
    ) -> Result<Self> {
        let mut host = self.prepare(writer, request)?;
        host.verify_launch_binding(attempt, scope)?;
        if host.head(writer)? != head {
            return Err(RunError::Validation(HEAD_MISMATCH));
        }
        host.bound
            .as_mut()
            .ok_or(RunError::Validation("delivery binding missing"))?
            .base_head = base;
        Ok(host)
    }
    /// Bind the lane's own branch for best-effort stop preservation, separate
    /// from any user-selected delivery destination.
    pub(crate) fn has_stop_publish(&self) -> bool {
        self.stop_publish_bound.is_some()
    }
    pub(crate) fn prepare_stop_publish(
        mut self,
        writer: &WriterRequest,
        request: &WriterDeliveryRequest,
    ) -> Result<Self> {
        if request.mode != crate::DeliveryMode::Push || request.open_pr {
            return Err(RunError::Validation(
                "stop publishing requires a branch push",
            ));
        }
        // Preparation runs on a copy: only its sealed binding and the tools it
        // validated are kept, in stop-only fields. The launch binding, host
        // tools and environment stay as the launch prepared them. The launch's
        // own prepare has just refreshed the remote, so a second all-branch
        // fetch here would run outside the origin-refresh lock and could lose
        // a tracking-ref transaction against an unrelated forced ref update,
        // dropping stop publication. Bind from that same refreshed state.
        let prepared = self.clone().prepare_with_fetch(writer, request, false)?;
        if prepared.bound.is_none() {
            return Err(RunError::Validation("stop publish binding missing"));
        }
        self.attach_stop_publish(prepared);
        Ok(self)
    }
    #[cfg(test)]
    pub(crate) fn launch_state_for_test(&self) -> LaunchStateForTest {
        let scope = provenant_records::custody::Scope {
            attempt: provenant_core::AttemptId::from_bytes([0x54; 16]),
            generation: 1,
        };
        (
            self.tools.as_ref().map(|tools| tools.git.clone()),
            self.tools.as_ref().and_then(|tools| tools.gh.clone()),
            self.bound
                .as_ref()
                .and_then(|bound| crate::db::bytes(&LaunchBinding::new(scope, bound)).ok()),
            self.stop_publish_bound
                .as_ref()
                .and_then(|bound| crate::db::bytes(&LaunchBinding::new(scope, bound)).ok()),
            self.environment.clone(),
        )
    }
    /// Bind recorded destinations directly for the sealed-name boundary tests.
    /// Production never uses this: its bindings come from `prepare`.
    #[cfg(test)]
    pub(crate) fn with_bound_remotes_for_test(
        mut self,
        delivery_remote: &str,
        stop_publish_remote: Option<&str>,
    ) -> Self {
        let bound = |remote: &str, mode: crate::DeliveryMode| BoundDelivery {
            base_head: GitOid::parse(&"1".repeat(40)).expect("a valid stand-in head"),
            push_url: "https://example.invalid/lane.git".to_owned(),
            lane_branch: "lane".to_owned(),
            pull_request_repo: None,
            git_dir: PathBuf::from("/nonexistent-sealed/.git"),
            common_git_dir: PathBuf::from("/nonexistent-sealed/.git"),
            host_cwd: PathBuf::from("/nonexistent-sealed"),
            destination: WriterDeliveryRequest {
                extra: Default::default(),
                mode,
                remote: remote.to_owned(),
                branch: "lane".to_owned(),
                open_pr: false,
                fast_forward_only: false,
            },
        };
        self.bound = Some(bound(
            delivery_remote,
            if delivery_remote.is_empty() {
                crate::DeliveryMode::Integrate("main".into())
            } else {
                crate::DeliveryMode::Push
            },
        ));
        self.stop_publish_bound =
            stop_publish_remote.map(|remote| bound(remote, crate::DeliveryMode::Push));
        self
    }
    fn attach_stop_publish(&mut self, prepared: Self) {
        self.stop_publish_bound = prepared.bound;
        self.stop_publish_host = Some(StopPublishHost {
            environment: prepared.environment,
            tools: prepared.tools,
            lane_roots: prepared.lane_roots,
            lane_path: prepared.lane_path,
        });
    }
    /// Rebuild stop-publish transport from its immutable launch binding.
    pub(crate) fn prepare_stop_publish_recovery(
        mut self,
        writer: &WriterRequest,
        request: &WriterDeliveryRequest,
        attempt: &Directory,
        scope: provenant_records::custody::Scope,
        head: GitOid,
        base: GitOid,
    ) -> Result<Self> {
        let mut prepared = self.clone().prepare(writer, request)?;
        let bound = prepared
            .bound
            .as_ref()
            .ok_or(RunError::Validation("stop publish binding missing"))?;
        prepared.verify_binding(attempt, scope, bound, STOP_PUBLISH_BINDING_FILE)?;
        if prepared.head(writer)? != head {
            return Err(RunError::Validation(HEAD_MISMATCH));
        }
        prepared
            .bound
            .as_mut()
            .ok_or(RunError::Validation("stop publish binding missing"))?
            .base_head = base;
        self.attach_stop_publish(prepared);
        Ok(self)
    }
    pub(crate) fn push_stopped_head(
        &self,
        writer: &WriterRequest,
        attempt: &Directory,
        scope: provenant_records::custody::Scope,
        head: GitOid,
    ) -> Result<()> {
        self.verify_stop_publish_binding(attempt, scope)?;
        let bound = self
            .stop_publish_bound
            .as_ref()
            .ok_or(RunError::Validation("stop publish binding missing"))?;
        let stop = self
            .stop_publish_host
            .as_ref()
            .ok_or(RunError::Validation("stop publish tools missing"))?;
        // Push with the stop-prepared tools and environment, not the launch's.
        let mut host = self.clone();
        host.environment = stop.environment.clone();
        host.tools = stop.tools.clone();
        host.lane_roots = stop.lane_roots.clone();
        host.lane_path = stop.lane_path.clone();
        if bound.destination.mode != crate::DeliveryMode::Push
            || bound.destination.open_pr
            || bound.destination.branch != bound.lane_branch
            || host.head(writer)? != head
        {
            return Err(RunError::Validation(HEAD_MISMATCH));
        }
        host.bound = Some(bound.clone());
        host.push_with_clean(writer, &bound.destination, head, "", false)
            .map(|_| ())
    }

    /// Seal the destination bound at launch, replacing an earlier launch's.
    pub(crate) fn seal_launch_binding(
        &self,
        attempt: &Directory,
        scope: provenant_records::custody::Scope,
    ) -> Result<()> {
        let bound = self
            .bound
            .as_ref()
            .ok_or(RunError::Validation("delivery binding missing"))?;
        self.seal_binding(attempt, scope, bound, BINDING_FILE)
    }
    pub(crate) fn seal_stop_publish_binding(
        &self,
        attempt: &Directory,
        scope: provenant_records::custody::Scope,
    ) -> Result<()> {
        let bound = self
            .stop_publish_bound
            .as_ref()
            .ok_or(RunError::Validation("stop publish binding missing"))?;
        self.seal_binding(attempt, scope, bound, STOP_PUBLISH_BINDING_FILE)
    }
    fn seal_binding(
        &self,
        attempt: &Directory,
        scope: provenant_records::custody::Scope,
        bound: &BoundDelivery,
        file: &str,
    ) -> Result<()> {
        let mut binding = LaunchBinding::new(scope, bound);
        if let Some(raw) = crate::artifacts::read_sealed(attempt, file, crate::RECORD_LIMIT)? {
            let previous: LaunchBinding = crate::db::decode(&raw)?;
            if previous != binding {
                return Err(RunError::Validation(BINDING_MISMATCH));
            }
            binding.extra = previous.extra;
        }
        attempt
            .atomic_write(
                OsStr::new(file),
                ".binding-",
                &crate::db::bytes(&binding)?,
                provenant_os::fs::PublishMode::Replace,
            )?
            .require_durable()
            .map_err(RunError::from)
    }
    fn verify_launch_binding(
        &self,
        attempt: &Directory,
        scope: provenant_records::custody::Scope,
    ) -> Result<()> {
        let bound = self
            .bound
            .as_ref()
            .ok_or(RunError::Validation("delivery binding missing"))?;
        self.verify_binding(attempt, scope, bound, BINDING_FILE)
    }
    fn verify_stop_publish_binding(
        &self,
        attempt: &Directory,
        scope: provenant_records::custody::Scope,
    ) -> Result<()> {
        let bound = self
            .stop_publish_bound
            .as_ref()
            .ok_or(RunError::Validation("stop publish binding missing"))?;
        self.verify_binding(attempt, scope, bound, STOP_PUBLISH_BINDING_FILE)
    }
    fn verify_binding(
        &self,
        attempt: &Directory,
        scope: provenant_records::custody::Scope,
        bound: &BoundDelivery,
        file: &str,
    ) -> Result<()> {
        let sealed = crate::artifacts::read_sealed(attempt, file, crate::RECORD_LIMIT)?
            .ok_or(RunError::Validation(BINDING_MISSING))?;
        let sealed: LaunchBinding = crate::db::decode(&sealed)?;
        if sealed != LaunchBinding::new(scope, bound) {
            return Err(RunError::Validation(BINDING_MISMATCH));
        }
        Ok(())
    }
    pub(crate) fn cancellable(&self, cancel: &provenant_core::CancelToken) -> Self {
        let mut context = self.clone();
        context.cancellation = Some(cancel.clone());
        context
    }
    /// Read host port declarations from this state root.
    pub fn with_host_state(mut self, state: &Path) -> Self {
        self.host_state = Some(state.to_path_buf());
        self
    }
    /// Reuse the owner's already-resolved forge context rather than resolving a
    /// second one for this command.
    /// Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
    pub fn with_forge(mut self, forge: &provenant_config::ForgeContext) -> Self {
        self.forge = Some(forge.clone());
        self
    }
    /// A validated sealed remote: a malformed record refuses, naming its value.
    /// An empty recorded remote is the deliberate local-integration sentinel,
    /// not a corrupt name: it maps to the same absence as an unbound remote.
    fn sealed_remote(name: Option<&str>) -> Result<Option<provenant_config::RemoteName>> {
        name.filter(|name| !name.is_empty())
            .map(|name| {
                provenant_config::RemoteName::new(name)
                    .map_err(|_| RunError::InvalidSealedRemote(name.to_owned()))
            })
            .transpose()
    }
    /// The remote named by the sealed launch destination.
    pub(crate) fn delivery_remote(&self) -> Result<Option<provenant_config::RemoteName>> {
        Self::sealed_remote(self.bound.as_ref().map(|b| b.destination.remote.as_str()))
    }
    /// The remote named by the sealed stop-publication destination.
    pub(crate) fn stop_publish_remote(&self) -> Result<Option<provenant_config::RemoteName>> {
        Self::sealed_remote(
            self.stop_publish_bound
                .as_ref()
                .map(|b| b.destination.remote.as_str()),
        )
    }
    /// The explicit state root, else the captured PROVENANT_STATE_DIR, else
    /// the default root under the captured HOME.
    fn host_state(&self) -> Option<PathBuf> {
        let captured = |name: &str| {
            self.environment
                .iter()
                .find(|(key, _)| key.as_slice() == name.as_bytes())
                .map(|(_, value)| PathBuf::from(OsString::from_vec(value.clone())))
                .filter(|path| path.is_absolute())
        };
        self.host_state
            .clone()
            .or_else(|| captured(provenant_core::STATE_DIRECTORY_ENV))
            .or_else(|| {
                captured("HOME").map(|home| home.join(provenant_core::DEFAULT_STATE_SUFFIX))
            })
    }
    fn publication_projector(
        &self,
    ) -> std::result::Result<
        provenant_confinement::publication::Projector,
        provenant_confinement::publication::ProjectionError,
    > {
        provenant_confinement::publication::Projector::from_environment(
            self.environment.iter().map(|(key, value)| {
                (
                    OsString::from_vec(key.clone()),
                    OsString::from_vec(value.clone()),
                )
            }),
        )
    }

    fn forge_command_with_input(
        &self,
        cwd: &Path,
        args: &[&str],
        stage: Stage,
        input: &[u8],
    ) -> Result<String> {
        use provenant_config::{ForgeOperation, ForgePort, PortTable};
        let state = self.host_state().ok_or_else(|| {
            prepare_error(DeliveryPrepareStep::ForgePort, "host state root unknown")
        })?;
        let project = self
            .bound
            .as_ref()
            .map_or(cwd, |bound| bound.host_cwd.as_path());
        let table = PortTable::read_for(Some(project), &state)
            .map_err(|error| prepare_error(DeliveryPrepareStep::ForgePort, error.to_string()))?;
        let mut forge = ForgePort::new(
            table.forge,
            "forge",
            project,
            &state,
            std::sync::Arc::new(self.policy.clone()),
        )
        .map_err(|error| prepare_error(DeliveryPrepareStep::ForgePort, error.to_string()))?;
        provenant_confinement::publication::install_forge(
            &mut forge,
            project.to_owned(),
            self.publication_projector(),
        );
        if let Some(cancel) = &self.cancellation {
            forge.set_cancel(cancel.clone());
        }
        let arguments: Vec<OsString> = args.iter().map(OsString::from).collect();
        let operation = ForgeOperation::Cli {
            arguments,
            input: Some(input.to_vec()),
        };
        let timeout = Duration::from_secs(30);
        let truncated = std::cell::Cell::new(false);
        // The port discards its execution on a non-success disposition, so keep
        // the command's own exit and bounded stderr at this seam.
        let command_failure = std::cell::Cell::new(None::<(Termination, bool, String)>);
        let forge_limit = || RunError::DeliveryLimit {
            stage,
            tool: "gh",
            command: git_subcommand(args),
            limit: stdout_limit(false, args),
        };
        let response = forge
            .request_with(
                operation,
                timeout,
                |forge_args, forge_input, forge_environment, budget| {
                    let tools = self
                        .tools
                        .as_ref()
                        .ok_or_else(|| std::io::Error::other("host tools unavailable"))?;
                    let executable = tools.gh.as_deref().ok_or_else(|| {
                        std::io::Error::other("the builtin forge executable was not sealed during delivery preparation")
                    })?;
        let argv: Vec<OsString> = std::iter::once(executable.as_os_str().to_owned())
                        .chain(forge_args.iter().skip(1).cloned())
                        .collect();
                    let mut env: Vec<_> = self
                        .environment
                        .iter()
                        .map(|(key, value)| {
                            (
                                OsString::from_vec(key.clone()),
                                OsString::from_vec(value.clone()),
                            )
                        })
                        .collect();
                    env.extend(forge_environment.iter().cloned());
                    if let Some(bound) = &self.bound {
                        env.push(("GIT_DIR".into(), bound.git_dir.as_os_str().to_owned()));
                        env.push((
                            "GIT_WORK_TREE".into(),
                            bound.host_cwd.as_os_str().to_owned(),
                        ));
                    }
                    let mut options = ExecOptions::new(&argv, project, budget);
                    options.cancel = self.cancellation.as_ref();
                    options.process_group = true;
                    options.env = Some(&env);
                    options.stdin = Some(forge_input.unwrap_or(&[]));
                    options.stdout = StreamPolicy::Capture {
                        limit: stdout_limit(false, args),
                    };
                    // The fatal line is last, so every stage keeps a bounded
                    // stderr tail as its cause, preparation included.
                    options.stderr = StreamPolicy::Tail {
                        limit: DELIVERY_STDERR_LIMIT * 4,
                    };
                    let executor = SystemExecutor::new(1).map_err(std::io::Error::other)?;
                    let fallback = provenant_core::CancelToken::new();
                    let outcome = self
                        .facts
                        .with_executor::<_, Box<dyn std::error::Error + Send + Sync>>(
                            &executor,
                            self.cancellation.as_ref().unwrap_or(&fallback),
                            |executor| executor.execute(options).map_err(Into::into),
                        )
                        .map_err(std::io::Error::other)?;
                    truncated.set(outcome.stdout.truncated);
                    let stderr = provenant_core::bounded_tail(
                        &String::from_utf8_lossy(&outcome.stderr.bytes),
                        DELIVERY_STDERR_LIMIT,
                    );
                    command_failure.set(Some((outcome.termination, outcome.timed_out, stderr)));
                    Ok(outcome)
                },
            )
            .map_err(|error| {
                // The port reports an over-bound capture as an unknown
                // disposition; the bound itself is the typed refusal.
                if truncated.get() {
                    return forge_limit();
                }
                let cause = failure_cause(command_failure.take(), error);
                if stage == Stage::Prepare {
                    prepare_error(DeliveryPrepareStep::ForgePort, cause)
                } else {
                    RunError::delivery(stage, cause)
                }
            })?;
        if response.execution.disposition != provenant_config::PortCommandDisposition::Succeeded {
            return Err(if stage == Stage::Prepare {
                prepare_error(
                    DeliveryPrepareStep::ForgePort,
                    format!(
                        "forge command returned {:?}",
                        response.execution.disposition
                    ),
                )
            } else {
                RunError::delivery(
                    stage,
                    response
                        .execution
                        .outcome
                        .as_ref()
                        .map(command_cause)
                        .unwrap_or_else(|| {
                            DeliveryCause::parse(format!(
                                "forge command returned {:?}",
                                response.execution.disposition
                            ))
                        }),
                )
            });
        }
        String::from_utf8(response.body)
            .map(|value| value.trim().to_owned())
            .map_err(|error| RunError::delivery(stage, DeliveryCause::parse(error)))
    }

    /// Share the command-scoped repository fact limits with host delivery.
    pub fn with_repo_facts(mut self, facts: &provenant_confinement::worktrees::RepoFacts) -> Self {
        self.facts = facts.clone();
        self
    }
    /// Integration authority never includes the default or primary checkout ref.
    pub fn validate_integration_target(
        project: &Path,
        target: &str,
        forge: &provenant_config::ForgeContext,
    ) -> Result<()> {
        let branch = target
            .strip_prefix(&format!("{}/", forge.remote.value))
            .unwrap_or(target);
        Self::check_delivery_branch(project, branch, forge)
    }
    /// The same protected-target rule without reading host state, for
    /// `run --explain` and other read-only admission previews.
    pub fn validate_integration_target_read_only(
        project: &Path,
        target: &str,
        forge: &provenant_config::ForgeContext,
    ) -> Result<()> {
        let branch = target
            .strip_prefix(&format!("{}/", forge.remote.value))
            .unwrap_or(target);
        let writer = WriterRequest {
            extra: Default::default(),
            custody_project: None,
            project: project.into(),
            worktree: project.into(),
        };
        let mut host = Self::capture();
        host.host_environment_without_host_state(&writer)?;
        host.tools = Some(HostTools {
            git: host.executable("git", &writer)?,
            gh: None,
        });
        host.refuse_protected_target(&writer, branch, Stage::Prepare, Some(forge))
    }
    /// A publication ref is distinct from a PR's base, which may be protected.
    pub fn check_delivery_branch(
        project: &Path,
        target: &str,
        forge: &provenant_config::ForgeContext,
    ) -> Result<()> {
        Self::capture().check_delivery_branch_with_facts(project, target, forge)
    }

    pub fn check_delivery_branch_with_facts(
        &self,
        project: &Path,
        target: &str,
        forge: &provenant_config::ForgeContext,
    ) -> Result<()> {
        let writer = WriterRequest {
            extra: Default::default(),
            custody_project: None,
            project: project.into(),
            worktree: project.into(),
        };
        let mut host = self.clone();
        host.host_environment(&writer)?;
        host.tools = Some(HostTools {
            git: host.executable("git", &writer)?,
            gh: None,
        });
        host.refuse_protected_target(&writer, target, Stage::Prepare, Some(forge))
    }

    fn refuse_protected_target(
        &self,
        writer: &WriterRequest,
        target: &str,
        stage: Stage,
        forge: Option<&provenant_config::ForgeContext>,
    ) -> Result<()> {
        if !self
            .command(
                &writer.worktree,
                "git",
                &["symbolic-ref", "--quiet", &format!("refs/heads/{target}")],
                stage,
            )?
            .is_empty()
        {
            return Err(prepare_error(
                DeliveryPrepareStep::IntegrationTarget,
                "symbolic branch targets are refused",
            ));
        }
        // The one resolved context already carries the default branch, the
        // declared protected set and any forge-reported protection. Reading
        // `refs/remotes/<remote>/HEAD` here rebuilt that decision from a second
        // source and failed outright when the remote was absent (a local
        // integration). A compiled-in `main`/`master` fallback could retire a
        // differently named default, so an unknown default protects nothing.
        // Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
        let protected_default = forge.is_some_and(|forge| forge.is_protected(target));
        let trees = self.command(
            &writer.worktree,
            "git",
            &["worktree", "list", "--porcelain"],
            stage,
        )?;
        let primary_branch = trees
            .split("\n\n")
            .next()
            .unwrap_or_default()
            .lines()
            .find_map(|line| line.strip_prefix("branch refs/heads/"));
        if protected_default || primary_branch == Some(target) {
            return Err(RunError::ProtectedDeliveryBranch);
        }
        Ok(())
    }

    fn refuse_remote_symbolic_target(
        &self,
        cwd: &Path,
        remote: &str,
        target: &str,
        stage: Stage,
    ) -> Result<()> {
        let refs = self.command(
            cwd,
            "git",
            &[
                "ls-remote",
                "--symref",
                "--",
                remote,
                &format!("refs/heads/{target}"),
            ],
            stage,
        )?;
        if refs.lines().any(|line| line.starts_with("ref: ")) {
            return Err(delivery_refusal(stage, "remote target is a symbolic ref"));
        }
        Ok(())
    }
    /// Final graph delivery publishes its integration ref, with a body built
    /// from settled node receipts. No checkout is switched or edited.
    pub fn deliver_graph_pr(
        self,
        project: &Path,
        branch: &str,
        base: GitOid,
        body: &str,
        forge: &provenant_config::ForgeContext,
    ) -> Result<String> {
        self.deliver_graph_pr_inner(project, branch, base, None, None, None, body, forge)
            .map(|receipt| receipt.pull_request)
    }

    /// Publish an exact writer head on one stable graph-owned branch. The
    /// writer ref and expected old remote value are checked before the push;
    /// replay reconciles an already-pushed head and its existing PR.
    #[allow(clippy::too_many_arguments)]
    pub fn deliver_graph_pr_at_head(
        self,
        project: &Path,
        writer_branch: &str,
        base: GitOid,
        expected_head: GitOid,
        publication_identity: &str,
        expected_remote_head: Option<GitOid>,
        body: &str,
        forge: &provenant_config::ForgeContext,
    ) -> Result<GraphPublicationReceipt> {
        self.deliver_graph_pr_inner(
            project,
            writer_branch,
            base,
            Some(expected_head),
            Some(publication_identity),
            expected_remote_head,
            body,
            forge,
        )
    }

    /// Update the authenticated adopted PR's existing head branch. The
    /// persisted old head is an exact remote lease; replay accepts only that
    /// old head or the already-pushed intended head and never creates a PR.
    pub fn update_adopted_graph_pr_at_head(
        self,
        project: &Path,
        expected_head: GitOid,
        expected_remote_head: GitOid,
        target: &GraphAdoptionTarget<'_>,
        forge: &provenant_config::ForgeContext,
    ) -> Result<GraphPublicationReceipt> {
        let branch = GitRefName::branch(target.branch)
            .map_err(|_| RunError::Validation("adopted PR branch"))?;
        if target.pull_request == 0
            || target.repository.trim().is_empty()
            || target.base_repository.trim().is_empty()
            || !target
                .repository
                .eq_ignore_ascii_case(target.base_repository)
            || target.base_branch.trim().is_empty()
            || !target.pull_request_url.starts_with("https://")
            || target.pull_request_url.chars().any(char::is_whitespace)
            || target.pull_request_url.chars().any(char::is_control)
        {
            return Err(delivery_refusal(
                Stage::Prepare,
                "adopted pull request identity is invalid",
            ));
        }
        let request = Self::destination(crate::DeliveryMode::Pr, branch.short_name(), forge)?
            .ok_or(RunError::Validation("adopted PR destination"))?;
        let writer = WriterRequest {
            extra: Default::default(),
            custody_project: None,
            project: project.into(),
            worktree: project.into(),
        };
        let host = self.prepare_with_fetch(&writer, &request, false)?;
        let bound = host.bound.as_ref().ok_or_else(|| {
            delivery_refusal(Stage::Prepare, "delivery launch binding was not sealed")
        })?;
        let repo = bound.pull_request_repo.as_deref().ok_or_else(|| {
            delivery_refusal(Stage::PullRequest, "pull request repository was not bound")
        })?;
        let bound_repository = repo.split_once('/').map(|(_, path)| path);
        let expected_url = format!("https://{repo}/pull/{}", target.pull_request);
        if bound_repository.is_none_or(|path| !path.eq_ignore_ascii_case(target.repository))
            || !expected_url.eq_ignore_ascii_case(target.pull_request_url)
        {
            return Err(delivery_refusal(
                Stage::PullRequest,
                "adopted pull request repository differs from the bound repository",
            ));
        }
        host.verify_objects(&writer, expected_remote_head, Stage::Head)?;
        host.verify_objects(&writer, expected_head, Stage::Head)?;
        host.refuse_protected_target(&writer, branch.short_name(), Stage::Push, Some(forge))?;
        host.refuse_remote_symbolic_target(
            project,
            &bound.push_url,
            branch.short_name(),
            Stage::Push,
        )?;

        let observed =
            host.adopted_pull_request(project, target, &[expected_remote_head, expected_head])?;
        if observed.head != expected_remote_head && observed.head != expected_head {
            return Err(delivery_refusal(
                Stage::Verify,
                "adopted pull request moved from the persisted expected head",
            ));
        }
        host.publish_graph_ref(
            project,
            &bound.push_url,
            branch.short_name(),
            expected_head,
            Some(expected_remote_head),
        )?;
        let remote = host.command(
            project,
            "git",
            &[
                "ls-remote",
                "--exit-code",
                "--",
                &bound.push_url,
                &format!("refs/heads/{}", branch.short_name()),
            ],
            Stage::Verify,
        )?;
        if remote.split_whitespace().next() != Some(expected_head.to_string().as_str()) {
            return Err(delivery_refusal(
                Stage::Verify,
                "adopted PR branch differs from the intended head",
            ));
        }
        let observed = host.adopted_pull_request(project, target, &[expected_head])?;
        if observed.head != expected_head {
            return Err(delivery_refusal(
                Stage::Verify,
                "adopted pull request did not retain the intended head",
            ));
        }
        Ok(GraphPublicationReceipt {
            branch: branch.short_name().to_owned(),
            head: expected_head,
            pull_request: observed.url,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn deliver_graph_pr_inner(
        self,
        project: &Path,
        branch: &str,
        base: GitOid,
        expected_head: Option<GitOid>,
        publication_identity: Option<&str>,
        expected_remote_head: Option<GitOid>,
        body: &str,
        forge: &provenant_config::ForgeContext,
    ) -> Result<GraphPublicationReceipt> {
        let qualified = format!("{}/", forge.remote.value);
        let target = branch.strip_prefix(&qualified).unwrap_or(branch);
        self.check_delivery_branch_with_facts(project, target, forge)?;
        let local = !branch.starts_with(&qualified);
        let source = if let Some(branch) = branch.strip_prefix(&qualified) {
            format!("refs/remotes/{}/{branch}", forge.remote.value)
        } else {
            format!("refs/heads/{branch}")
        };
        let branch = branch.strip_prefix(&qualified).unwrap_or(branch);
        let request = Self::destination(crate::DeliveryMode::Pr, branch, forge)?
            .ok_or(RunError::Validation("graph PR destination"))?;
        let writer = WriterRequest {
            extra: Default::default(),
            custody_project: None,
            project: project.into(),
            worktree: project.into(),
        };
        let host = self.prepare_with_fetch(&writer, &request, false)?;
        let bound = host.bound.as_ref().ok_or_else(|| {
            delivery_refusal(Stage::Prepare, "delivery launch binding was not sealed")
        })?;
        let head = if local {
            host.command(
                project,
                "git",
                &["rev-parse", "--verify", &source],
                Stage::Head,
            )?
        } else {
            host.refresh_target_and_resolve(&writer, branch, Stage::Head, &forge.remote_name()?)?
        };
        let head = GitOid::parse(&head)
            .map_err(|error| RunError::delivery(Stage::Head, DeliveryCause::parse(error)))?;
        if expected_head.is_some_and(|expected| expected != head) {
            return Err(delivery_refusal(
                Stage::Head,
                "graph publication source differs from the sealed hygiene head",
            ));
        }
        host.verify_objects(&writer, head, Stage::Head)?;
        host.verify_objects(&writer, base, Stage::Head)?;
        // A local integration ref may belong to a human. Publish a separate
        // graph-owned ref; never push that existing local branch by name.
        let publication = if let Some(identity) = publication_identity {
            Some(host.graph_publication_branch_for(project, identity)?)
        } else if local {
            Some(host.graph_publication_branch(project, head)?)
        } else {
            None
        };
        let branch = publication.as_deref().unwrap_or(branch);
        host.refuse_protected_target(&writer, branch, Stage::Push, Some(forge))?;
        let reference = format!("refs/heads/{branch}");
        if publication_identity.is_some() {
            host.publish_graph_ref(project, &bound.push_url, branch, head, expected_remote_head)?;
        } else if local {
            host.refuse_remote_symbolic_target(project, &bound.push_url, branch, Stage::Push)?;
            host.command(
                project,
                "git",
                &[
                    "push",
                    "--no-follow-tags",
                    "--recurse-submodules=no",
                    "--",
                    &bound.push_url,
                    &format!("{head}:{reference}"),
                ],
                Stage::Push,
            )?;
        }
        let remote = host.command(
            project,
            "git",
            &[
                "ls-remote",
                "--exit-code",
                "--",
                &bound.push_url,
                &reference,
            ],
            Stage::Verify,
        )?;
        if remote.split_whitespace().next() != Some(head.to_string().as_str()) {
            return Err(delivery_refusal(
                Stage::Verify,
                "verified remote ref differs from the published head",
            ));
        }
        let repo = bound.pull_request_repo.as_deref().ok_or_else(|| {
            delivery_refusal(Stage::PullRequest, "pull request repository was not bound")
        })?;
        let base_branch = host.pr_base(project, repo)?;
        let url = if let Some(url) = host.existing_pr(project, repo, branch, head, &base_branch)? {
            url
        } else {
            let commits = host.commit_range_body(&writer, &format!("{base}..{head}"), body)?;
            match host.command_with_input(
                project,
                "gh",
                &[
                    "pr",
                    "create",
                    "--repo",
                    repo,
                    "--head",
                    branch,
                    "--base",
                    &base_branch,
                    "--title",
                    &format!("Graph delivery {branch}"),
                    "--body-file",
                    "-",
                ],
                Stage::PullRequest,
                commits.as_bytes(),
            ) {
                Ok(url) => url,
                Err(create_error) => {
                    match host.existing_pr(project, repo, branch, head, &base_branch) {
                        Ok(Some(url)) => url,
                        Ok(None) | Err(_) => return Err(create_error),
                    }
                }
            }
        };
        if !url.starts_with("https://") || url.lines().count() != 1 {
            return Err(delivery_refusal(
                Stage::PullRequest,
                "gh did not return a single https pull request URL",
            ));
        }
        Ok(GraphPublicationReceipt {
            branch: branch.to_owned(),
            head,
            pull_request: url,
        })
    }

    pub(super) fn graph_publication_branch(&self, project: &Path, head: GitOid) -> Result<String> {
        use provenant_core::{CancelToken, Clock, WaitBudget};
        use provenant_os::{clock::SystemClock, fs::LockMode};
        let stage = Stage::Push;
        let common = self.common_git_dir(project, stage)?;
        let directory = Directory::open_anchor(&common)?;
        let clock = SystemClock;
        let cancel = CancelToken::new();
        let _lock = directory.lock(
            OsStr::new("provenant-graph-publication.lock"),
            LockMode::Exclusive,
            WaitBudget::cli(clock.mono_now(), 60)
                .map_err(|error| RunError::delivery(stage, DeliveryCause::parse(error)))?,
            &cancel,
            &clock,
        )?;
        let branch = format!("provenant/graph/{head}");
        let reference = format!("refs/heads/{branch}");
        let marker = format!("provenant-graph-publication-{head}.json");
        if !self
            .command(
                project,
                "git",
                &["symbolic-ref", "--quiet", &reference],
                stage,
            )?
            .is_empty()
        {
            return Err(delivery_refusal(stage, "publication ref is a symbolic ref"));
        }
        if let Some(raw) = crate::artifacts::read_sealed(&directory, &marker, crate::RECORD_LIMIT)?
            && raw != branch.as_bytes()
        {
            return Err(delivery_refusal(
                stage,
                "publication marker was written for a different head",
            ));
        }
        // Recover an unmarked publication or a marked ref deleted after a PR.
        // Both updates compare the old value, so a different head refuses.
        if self
            .command(
                project,
                "git",
                &[
                    "update-ref",
                    "--no-deref",
                    &reference,
                    &head.to_string(),
                    "",
                ],
                stage,
            )
            .is_err()
        {
            self.command(
                project,
                "git",
                &[
                    "update-ref",
                    "--no-deref",
                    &reference,
                    &head.to_string(),
                    &head.to_string(),
                ],
                stage,
            )?;
        }
        crate::artifacts::seal(&directory, &marker, branch.as_bytes(), crate::RECORD_LIMIT)?;
        Ok(branch)
    }

    pub(super) fn graph_publication_branch_for(
        &self,
        project: &Path,
        identity: &str,
    ) -> Result<String> {
        use provenant_core::{CancelToken, Clock, WaitBudget};
        use provenant_os::{clock::SystemClock, fs::LockMode};
        use sha2::{Digest, Sha256};

        if identity.trim().is_empty() {
            return Err(delivery_refusal(
                Stage::Prepare,
                "stable graph publication identity is empty",
            ));
        }
        let common = self.common_git_dir(project, Stage::Prepare)?;
        let directory = Directory::open_anchor(&common)?;
        let clock = SystemClock;
        let cancel = CancelToken::new();
        let _lock = directory.lock(
            OsStr::new("provenant-graph-publication.lock"),
            LockMode::Exclusive,
            WaitBudget::cli(clock.mono_now(), 60)
                .map_err(|error| RunError::delivery(Stage::Prepare, DeliveryCause::parse(error)))?,
            &cancel,
            &clock,
        )?;
        let digest = provenant_core::hex(&Sha256::digest(identity.as_bytes()));
        let branch = format!("provenant/graph/{digest}");
        let marker = format!("provenant-graph-owner-{digest}.json");
        if let Some(raw) = crate::artifacts::read_sealed(&directory, &marker, crate::RECORD_LIMIT)?
            && raw != identity.as_bytes()
        {
            return Err(delivery_refusal(
                Stage::Prepare,
                "stable graph publication identity marker differs",
            ));
        }
        crate::artifacts::seal(
            &directory,
            &marker,
            identity.as_bytes(),
            crate::RECORD_LIMIT,
        )?;
        Ok(branch)
    }

    fn publish_graph_ref(
        &self,
        project: &Path,
        push_url: &str,
        branch: &str,
        head: GitOid,
        expected_remote_head: Option<GitOid>,
    ) -> Result<()> {
        let stage = Stage::Push;
        self.refuse_remote_symbolic_target(project, push_url, branch, stage)?;
        let reference = format!("refs/heads/{branch}");
        let read_remote = || -> Result<Option<GitOid>> {
            let raw = self.command(
                project,
                "git",
                &["ls-remote", "--", push_url, &reference],
                Stage::Verify,
            )?;
            let mut refs = raw.lines().filter(|line| !line.trim().is_empty());
            let Some(line) = refs.next() else {
                return Ok(None);
            };
            if refs.next().is_some() {
                return Err(delivery_refusal(
                    Stage::Verify,
                    "graph publication ref resolved to multiple remote values",
                ));
            }
            let oid = line
                .split_whitespace()
                .next()
                .ok_or_else(|| delivery_refusal(Stage::Verify, "remote ref has no object id"))?;
            GitOid::parse(oid)
                .map(Some)
                .map_err(|error| RunError::delivery(Stage::Verify, DeliveryCause::parse(error)))
        };

        let current = read_remote()?;
        if current == Some(head) {
            return Ok(());
        }
        if current != expected_remote_head {
            return Err(delivery_refusal(
                Stage::Verify,
                "graph publication ref differs from the persisted expected remote head",
            ));
        }
        let expected = expected_remote_head.map_or_else(String::new, |oid| oid.to_string());
        let push = self.command(
            project,
            "git",
            &[
                "push",
                "--no-follow-tags",
                "--recurse-submodules=no",
                &format!("--force-with-lease={reference}:{expected}"),
                "--",
                push_url,
                &format!("{head}:{reference}"),
            ],
            stage,
        );
        if let Err(error) = push {
            if read_remote()? == Some(head) {
                return Ok(());
            }
            return Err(error);
        }
        if read_remote()? != Some(head) {
            return Err(delivery_refusal(
                Stage::Verify,
                "verified graph publication ref differs from the intended head",
            ));
        }
        Ok(())
    }
    /// Resolve the concise mode into the existing explicit destination contract.
    /// Integration names are local unless qualified as `<remote>/<branch>`.
    pub fn destination(
        mode: crate::DeliveryMode,
        branch: &str,
        forge: &provenant_config::ForgeContext,
    ) -> Result<Option<WriterDeliveryRequest>> {
        Self::destination_for_remote(mode, branch, &forge.remote.value)
    }

    /// Resolve a destination from an already-known remote name. Callers that
    /// carry the resolved remote but have no full context (a sealed request, a
    /// stop-publication recovery) use this instead of rebuilding a context.
    /// An absent remote is represented by the caller, never invented here.
    pub fn destination_for_remote(
        mode: crate::DeliveryMode,
        branch: &str,
        remote: &str,
    ) -> Result<Option<WriterDeliveryRequest>> {
        let qualified = format!("{remote}/");
        let (remote, destination, open_pr) = match &mode {
            crate::DeliveryMode::None => return Ok(None),
            crate::DeliveryMode::Push => (remote.to_owned(), branch.to_owned(), false),
            crate::DeliveryMode::Pr => (remote.to_owned(), branch.to_owned(), true),
            crate::DeliveryMode::Integrate(target) => match target.strip_prefix(&qualified) {
                Some(branch) => (remote.to_owned(), branch.to_owned(), false),
                None => (String::new(), target.clone(), false),
            },
        };
        let request = WriterDeliveryRequest {
            extra: Default::default(),
            mode,
            remote,
            branch: destination,
            open_pr,
            fast_forward_only: false,
        };
        validate(&request)?;
        Ok(Some(request))
    }
    pub(crate) fn new(environment: Vec<(OsString, OsString)>) -> Self {
        Self {
            environment: environment
                .into_iter()
                .map(|(k, v)| (k.into_vec(), v.into_vec()))
                .collect(),
            lane_access: None,
            lane_sandboxed: None,
            trusted_worktree: None,
            host_project: None,
            policy: Default::default(),
            bound: None,
            stop_publish_bound: None,
            stop_publish_host: None,
            tools: None,
            lane_path: None,
            lane_roots: Vec::new(),
            cancellation: None,
            facts: Default::default(),
            host_state: None,
            forge: None,
        }
    }
    pub(crate) fn with_lane_access(
        mut self,
        access: Option<provenant_records::LaneAccessReceipt>,
        sandboxed: Option<bool>,
    ) -> Self {
        self.lane_access = access;
        self.lane_sandboxed = sandboxed;
        self
    }
    pub(crate) fn with_policy(mut self, policy: provenant_records::policy::Policy) -> Self {
        self.policy = policy;
        self
    }
    fn isolated(&self) -> bool {
        self.lane_access
            .as_ref()
            .is_some_and(|receipt| receipt.access.is_isolated())
    }
    fn transport_warning(&self, detail: &str) {
        let source = self
            .lane_access
            .as_ref()
            .map_or("default", |receipt| receipt.source.as_str());
        let facts = provenant_records::policy::Facts::violation(format!(
            "{detail} (lanes.access=host, source: {source})"
        ));
        if let Some(message) = self
            .policy
            .check("DELIVERY_HOST_TRANSPORT_NONE", facts)
            .message()
        {
            use provenant_records::warning::{DELIVERY_TRANSPORT_FALLBACK, Warning, warn};
            // Spec: docs/specs/cli.md#warnings-are-typed-values
            warn(
                Warning::new(DELIVERY_TRANSPORT_FALLBACK, message)
                    .subject("DELIVERY_HOST_TRANSPORT_NONE"),
            );
        }
    }
    /// Preserve normal host credential/config discovery without Git's ambient
    /// repository/index overrides or any provider-specific environment.
    pub fn capture() -> Self {
        const KEYS: &[&str] = &[
            "HOME",
            "PATH",
            "XDG_CONFIG_HOME",
            "GH_CONFIG_DIR",
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "SSH_AUTH_SOCK",
            "SSH_AGENT_PID",
            "SSH_ASKPASS",
            "SSH_ASKPASS_REQUIRE",
            "DISPLAY",
            "GIT_ASKPASS",
            "GIT_SSH",
            "GIT_SSH_COMMAND",
            "GIT_SSH_VARIANT",
            "GIT_TERMINAL_PROMPT",
            provenant_core::consumer_env::GIT,
            provenant_core::STATE_DIRECTORY_ENV,
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_SYSTEM",
            "GIT_CONFIG_NOSYSTEM",
            "GCM_INTERACTIVE",
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
            "GIT_SSL_CAINFO",
            "GIT_SSL_CAPATH",
            "HTTPS_PROXY",
            "HTTP_PROXY",
            "ALL_PROXY",
            "NO_PROXY",
            "https_proxy",
            "http_proxy",
            "all_proxy",
            "no_proxy",
            "TMPDIR",
            "LANG",
            "LC_ALL",
        ];
        Self::new(
            std::env::vars_os()
                .filter(|(key, _)| KEYS.iter().any(|k| key == OsStr::new(k)))
                .collect(),
        )
    }
    /// Under writer custody, before the provider launches. Bind the single
    /// push destination and refresh shared refs with the host's Git authority.
    pub(crate) fn prepare(
        self,
        writer: &WriterRequest,
        request: &WriterDeliveryRequest,
    ) -> Result<Self> {
        self.prepare_with_fetch(writer, request, true)
    }

    /// Resolve the forge context for a preparation guard when the owner did not
    /// pass one: sealed holders from before that was carried resolve it from
    /// the host/project port table. A checkout that cannot resolve a remote or
    /// default branch keeps working, reporting no protected names while the
    /// primary-checkout rule still applies.
    /// Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
    fn resolve_preparation_forge(
        &self,
        writer: &WriterRequest,
    ) -> Option<provenant_config::ForgeContext> {
        let state = self.host_state()?;
        let ports = provenant_config::PortTable::read_for(Some(&writer.project), &state).ok()?;
        resolve_forge(&state, &writer.project, &ports).ok()
    }

    fn prepare_with_fetch(
        mut self,
        writer: &WriterRequest,
        request: &WriterDeliveryRequest,
        fetch: bool,
    ) -> Result<Self> {
        validate(request)?;
        self.host_environment(writer)?;
        let stage = Stage::Prepare;
        let git = self.executable("git", writer)?;
        let gh = if request.open_pr {
            let state = self.host_state().ok_or_else(|| {
                prepare_error(DeliveryPrepareStep::ForgePort, "host state root unknown")
            })?;
            let ports = provenant_config::PortTable::read_for(Some(&writer.project), &state)
                .map_err(|error| prepare_error(DeliveryPrepareStep::ForgePort, error))?;
            provenant_config::dispatch_port_backend(
                &ports.forge.backend,
                || self.executable("gh", writer).map(Some),
                |_| Ok(None),
                || Ok(None),
            )?
        } else {
            None
        };
        self.tools = Some(HostTools { git, gh });
        let common_git_dir = self.trusted_common_git_dir(writer, stage)?;
        self.set_host_project(&common_git_dir);
        // Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
        if self.forge.is_none() {
            let resolved = self.resolve_preparation_forge(writer);
            self.forge = resolved;
        }
        self.refuse_protected_target(writer, &request.branch, stage, self.forge.as_ref())?;
        if matches!(request.mode, crate::DeliveryMode::Integrate(_)) && request.remote.is_empty() {
            let branch = self.command(
                &writer.worktree,
                "git",
                &["symbolic-ref", "--quiet", "--short", "HEAD"],
                stage,
            )?;
            let git_dir = self.command(
                &writer.worktree,
                "git",
                &["rev-parse", "--absolute-git-dir"],
                stage,
            )?;
            let base_head = self.head(writer)?;
            self.bound = Some(BoundDelivery {
                base_head,
                push_url: String::new(),
                lane_branch: branch,
                pull_request_repo: None,
                git_dir: std::fs::canonicalize(git_dir)
                    .map_err(|error| prepare_error(DeliveryPrepareStep::GitDir, error))?,
                common_git_dir,
                host_cwd: writer.project.clone(),
                destination: request.clone(),
            });
            return Ok(self);
        }
        let urls = self.command(
            &writer.worktree,
            "git",
            &["remote", "get-url", "--push", "--all", &request.remote],
            stage,
        )?;
        let mut urls = urls.lines();
        let url = urls
            .next()
            .filter(|v| !v.is_empty() && !v.starts_with('-'))
            .ok_or_else(|| prepare_error(DeliveryPrepareStep::RemoteUrl, "no usable push URL"))?;
        if urls.next().is_some() {
            return Err(prepare_error(
                DeliveryPrepareStep::RemoteUrl,
                "multiple push URLs",
            ));
        }
        // Embedded HTTP credentials would become process arguments below.
        // Host credential helpers, gh config and the SSH agent remain available.
        if let Some((scheme, rest)) = url.split_once("://")
            && ((scheme != "ssh"
                && rest
                    .split('/')
                    .next()
                    .is_some_and(|authority| authority.contains('@')))
                || rest.contains(['?', '#'])
                || scheme == "ssh"
                    && rest
                        .split('@')
                        .next()
                        .is_some_and(|user| user.contains(':')))
        {
            if self.isolated() {
                return Err(prepare_error(
                    DeliveryPrepareStep::RemoteUrl,
                    "embedded credentials, query or fragment refused",
                ));
            }
            self.transport_warning("delivery uses a remote URL with userinfo, query or fragment");
        }
        let branch = self.command(
            &writer.worktree,
            "git",
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
            stage,
        )?;
        GitRefName::branch(&branch)
            .map_err(|error| prepare_error(DeliveryPrepareStep::Head, error))?;
        let git_dir = self.command(
            &writer.worktree,
            "git",
            &["rev-parse", "--absolute-git-dir"],
            stage,
        )?;
        let push_url = if !url.contains(':') && !Path::new(url).is_absolute() {
            std::fs::canonicalize(writer.worktree.join(url))
                .map_err(|error| prepare_error(DeliveryPrepareStep::RemoteUrl, error))?
                .to_str()
                .ok_or_else(|| prepare_error(DeliveryPrepareStep::RemoteUrl, "path is not UTF-8"))?
                .to_owned()
        } else {
            url.to_owned()
        };
        let base = self.command(
            &writer.worktree,
            "git",
            &["rev-parse", "--verify", "HEAD"],
            stage,
        )?;
        self.bound = Some(BoundDelivery {
            base_head: GitOid::parse(&base)
                .map_err(|error| prepare_error(DeliveryPrepareStep::Head, error))?,
            push_url,
            lane_branch: branch,
            pull_request_repo: request
                .open_pr
                .then(|| pull_request_repo(url))
                .transpose()
                .map_err(|error| prepare_error(DeliveryPrepareStep::RemoteUrl, error))?,
            git_dir: std::fs::canonicalize(git_dir)
                .map_err(|error| prepare_error(DeliveryPrepareStep::GitDir, error))?,
            common_git_dir,
            host_cwd: writer.project.clone(),
            destination: request.clone(),
        });
        self.validate_transport_helpers(writer)?;
        if fetch && !matches!(request.mode, crate::DeliveryMode::Integrate(_)) {
            // The shared lock coalesces fetches and tolerates forced ref updates.
            let common = self.common_git_dir(&writer.project, stage)?;
            let common = Directory::open_anchor(&common)?;
            let requested = OriginRefreshRequest::capture()?;
            let cancel = provenant_core::CancelToken::new();
            coalesced_origin_refresh(
                &common,
                &requested,
                self.cancellation.as_ref().unwrap_or(&cancel),
                || self.refresh_remote(writer, &[], &request.remote),
            )?;
        }
        Ok(self)
    }
    /// Refresh the forge remote under writer custody with delivery's host
    /// credential and executable checks, constraining the refspec and helpers.
    /// `target` is the private Git directory and the one remote to refresh.
    pub(crate) fn refresh_origin(
        writer: &WriterRequest,
        target: (&Path, Option<provenant_config::RemoteName>),
        host_state: &Path,
        cancel: &provenant_core::CancelToken,
        facts: &provenant_confinement::worktrees::RepoFacts,
        access: (Option<&provenant_records::LaneAccessReceipt>, Option<bool>),
        policy: &provenant_records::policy::Policy,
    ) -> Result<()> {
        let (git_dir, remote) = target;
        // A checkout with no remote has no tracking branch to refresh.
        let Some(remote) = remote else {
            return Ok(());
        };
        let mut host = Self::capture()
            .with_lane_access(access.0.cloned(), access.1)
            .with_policy(policy.clone())
            .with_repo_facts(facts)
            .with_host_state(host_state);
        host.cancellation = Some(cancel.clone());
        host.refresh_origin_with(writer, git_dir, remote.as_str())
    }
    pub(super) fn refresh_origin_with(
        self,
        writer: &WriterRequest,
        git_dir: &Path,
        remote: &str,
    ) -> Result<()> {
        let requested = OriginRefreshRequest::capture()?;
        let mut host = self;
        host.host_environment(writer)?;
        host.environment
            .retain(|(key, _)| key != b"GIT_TERMINAL_PROMPT");
        host.environment
            .push((b"GIT_TERMINAL_PROMPT".to_vec(), b"0".to_vec()));
        host.tools = Some(HostTools {
            git: host.executable("git", writer)?,
            gh: None,
        });
        let git_dir = git_dir
            .to_str()
            .ok_or_else(|| prepare_error(DeliveryPrepareStep::GitDir, "path is not UTF-8"))?;
        let worktree = writer
            .worktree
            .to_str()
            .ok_or_else(|| prepare_error(DeliveryPrepareStep::Worktree, "path is not UTF-8"))?;
        let remotes = host.command(
            &writer.worktree,
            "git",
            &["--git-dir", git_dir, "--work-tree", worktree, "remote"],
            Stage::Prepare,
        )?;
        if !remotes.lines().any(|line| line == remote) {
            return Ok(());
        }
        // A refresh completed since this request began also satisfies it,
        // including one which started during transport validation. The fetch
        // lock is independent of the short upstream-config lock.
        let common = host.common_git_dir(&writer.project, Stage::Prepare)?;
        host.set_host_project(&common);
        host.validate_transport_helpers(writer)?;
        let common = Directory::open_anchor(&common)?;
        let cancel = provenant_core::CancelToken::new();
        coalesced_origin_refresh(
            &common,
            &requested,
            host.cancellation.as_ref().unwrap_or(&cancel),
            || {
                host.refresh_remote(
                    writer,
                    &["--git-dir", git_dir, "--work-tree", worktree],
                    remote,
                )
            },
        )
    }

    /// Fetch every advertised branch of `remote` into `refs/remotes/<remote>/`,
    /// leaving out any whose tracking ref could alias another one. `prefix`
    /// holds the repository selection arguments, if the caller has no bound
    /// repository.
    fn refresh_remote(&self, writer: &WriterRequest, prefix: &[&str], remote: &str) -> Result<()> {
        let tracking = format!("refs/remotes/{remote}/");
        let uploadpack = format!("remote.{remote}.uploadpack=git-upload-pack");
        let with = |tail: &[&str]| -> Vec<String> {
            prefix
                .iter()
                .chain(tail)
                .map(|arg| (*arg).to_owned())
                .collect()
        };
        let run = |tail: &[&str]| -> Result<String> {
            let args = with(tail);
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            self.command(&writer.worktree, "git", &args, Stage::Prepare)
        };
        let advertised = run(&[
            "-c",
            &uploadpack,
            "ls-remote",
            "--symref",
            "--",
            remote,
            "HEAD",
            "refs/heads/*",
        ])?;
        let mut default = None;
        let mut heads = Vec::new();
        for line in advertised.lines() {
            let Some((value, name)) = line.split_once('\t') else {
                continue;
            };
            if name == "HEAD" {
                if let Some(branch) = value.strip_prefix("ref: refs/heads/") {
                    default = Some(branch.to_owned());
                }
            } else if let Some(branch) = name.strip_prefix("refs/heads/")
                && GitOid::parse(value).is_ok()
            {
                heads.push(branch);
            }
        }
        // Names that alias on a case-insensitive filesystem would share one
        // tracking ref. `HEAD` is the symref `<remote>/HEAD`, so a branch
        // named `head` aliases it there, and so does any local tracking symref:
        // a fetch writes through a symref to the ref it points at. Leave such
        // names out. Only a colliding default is fatal.
        let mut reserved = vec!["HEAD".to_owned()];
        let mut existing = Vec::new();
        for line in run(&["for-each-ref", "--format=%(refname)%09%(symref)", &tracking])?.lines() {
            if let Some((name, symref)) = line.split_once('\t')
                && let Some(short) = name.strip_prefix(&tracking)
            {
                if !symref.is_empty() {
                    reserved.push(short.to_owned());
                }
                existing.push(short.to_owned());
            }
        }
        // Spellings that differ only in Unicode normalisation share one
        // tracking ref where Git precomposes names or the filesystem folds
        // them, so a sibling's commit would land behind the default (or
        // behind `<remote>/HEAD`). Which spelling survives is not ours to
        // choose, so refuse outright instead of leaving a name out.
        let advertised: Vec<&str> = default
            .iter()
            .map(String::as_str)
            .chain(heads.iter().copied())
            .collect();
        let on_disk: Vec<&str> = existing.iter().map(String::as_str).collect();
        if let Some((name, other)) = provenant_core::find_normalisation_alias(&advertised, &on_disk)
        {
            return Err(RunError::OriginNormalisationCollision(prepare_reason(
                format!(
                    "branch names {} and {} are equivalent under Unicode NFC normalisation; refusing to refresh {remote} until one spelling is gone",
                    spelled(name),
                    spelled(other),
                ),
            )));
        }
        let mut refspecs = Vec::new();
        for name in &heads {
            let collides = heads
                .iter()
                .any(|other| provenant_core::refs_collide(name, other))
                || reserved
                    .iter()
                    .any(|held| *name == held || provenant_core::refs_collide(name, held));
            if collides && default.as_deref() == Some(*name) {
                return Err(RunError::OriginDefaultCollision);
            }
            if collides || GitRefName::branch(name).is_err() {
                delivery_advisory(format!(
                    "not refreshing {remote} branch {name:?}; its name collides or is unsupported, and an existing tracking ref may be stale"
                ));
                continue;
            }
            refspecs.push(format!("+refs/heads/{name}:{tracking}{name}"));
        }
        if refspecs.is_empty() {
            return Ok(());
        }
        let mut tail = vec![
            "-c",
            &uploadpack,
            "-c",
            "core.logAllRefUpdates=false",
            // Git precomposes decomposed Unicode in ref names itself, so
            // NFC and NFD spellings of one branch land on one tracking ref
            // (which `refs_collide` then sees) without a normalisation
            // dependency here.
            "-c",
            ORIGIN_REFRESH_PRECOMPOSE_SETTING,
            "fetch",
            "--no-auto-maintenance",
            "--no-tags",
            "--no-recurse-submodules",
            "--refmap=",
            "--",
            remote,
        ];
        tail.extend(refspecs.iter().map(String::as_str));
        run(&tail)?;
        Ok(())
    }

    /// Refresh and use the remote-advertised OID under Git's serialised ref transaction.
    fn refresh_target_and_resolve(
        &self,
        writer: &WriterRequest,
        branch: &str,
        stage: Stage,
        remote: &provenant_config::RemoteName,
    ) -> Result<String> {
        let branch_ref = GitRefName::branch(branch)
            .map_err(|error| RunError::delivery(stage, DeliveryCause::parse(error)))?;
        let common = self.common_git_dir(&writer.project, stage)?;
        crate::writer::verify_writer_git_paths(&common, &branch_ref, Some(remote))?;
        self.validate_transport_helpers(writer)?;
        let source = format!("refs/heads/{branch}");
        // The URL is transport-only; the remote name owns the tracking ref and
        // its hardened config key, since a URL is not a valid ref namespace.
        let push_url = &self
            .bound
            .as_ref()
            .ok_or_else(|| delivery_refusal(stage, "delivery launch binding was not sealed"))?
            .push_url;
        let query = || {
            self.command(
                &writer.worktree,
                "git",
                &[
                    "-c",
                    &format!("remote.{remote}.uploadpack=git-upload-pack"),
                    "ls-remote",
                    "--exit-code",
                    "--",
                    push_url,
                    &source,
                ],
                stage,
            )
        };
        let advertised = query()?;
        let (oid, reference) = advertised
            .split_once('\t')
            .ok_or_else(|| delivery_refusal(stage, "advertised ref is malformed"))?;
        if reference != source || GitOid::parse(oid).is_err() {
            return Err(delivery_refusal(
                stage,
                "advertised ref does not match the requested ref",
            ));
        }
        let tracking = format!("refs/remotes/{remote}/{branch}");
        self.refuse_symref_destination(&writer.worktree, &tracking, stage)?;
        let refspec = format!("+{source}:{tracking}");
        self.command(
            &writer.worktree,
            "git",
            &[
                "-c",
                &format!("remote.{remote}.uploadpack=git-upload-pack"),
                "-c",
                "core.logAllRefUpdates=false",
                "fetch",
                "--no-auto-maintenance",
                "--no-tags",
                "--no-recurse-submodules",
                "--refmap=",
                "--",
                push_url,
                &refspec,
            ],
            stage,
        )?;
        if query()? != advertised {
            return Err(delivery_refusal(
                stage,
                "remote ref changed during delivery",
            ));
        }
        self.verify_objects(
            writer,
            GitOid::parse(oid)
                .map_err(|error| RunError::delivery(stage, DeliveryCause::parse(error)))?,
            stage,
        )?;
        Ok(oid.to_owned())
    }

    fn validate_transport_helpers(&self, writer: &WriterRequest) -> Result<()> {
        let stage = Stage::Prepare;
        // Free-form shell helpers can select writer code. Support normal
        // executable helpers and gh's standard helper, otherwise refuse early.
        let context = provenant_os::git_config::ConfigContext {
            operation: "read transport helper settings",
            file: writer.worktree.join(".git/config"),
            key: None,
        };
        let helpers = provenant_os::git_config::read(
            context,
            provenant_os::git_config::Read::GetRegexp(
                "^(credential(\\..*)?\\.helper|core\\.sshcommand)$",
            ),
            |args| {
                self.command_for_config(
                    &writer.worktree,
                    "git",
                    &git_config_args(args, stage)?,
                    stage,
                    b"",
                )
            },
        )
        .map_err(|error| RunError::DeliveryTransport {
            stage,
            kind: crate::TransportRefusal::Unreadable,
            reason: transport_config_cause(error),
        })?;
        for line in helpers.lines() {
            let (key, value) = line.split_once(' ').unwrap_or((line, ""));
            if value.trim().is_empty() {
                continue;
            }
            if let Err(error) = self.validate_transport_helper(writer, key, value) {
                if self.isolated() {
                    return Err(error);
                }
                self.transport_warning("delivery uses a host-configured transport command");
            }
        }
        Ok(())
    }

    fn validate_transport_helper(
        &self,
        writer: &WriterRequest,
        key: &str,
        value: &str,
    ) -> Result<()> {
        if key.eq_ignore_ascii_case("core.sshcommand") {
            return Err(credential_helper_refusal(
                HelperSafety::UnsupportedSshCommand,
                "delivery does not support core.sshCommand; use host SSH config",
            ));
        }
        if let Some(command) = value.strip_prefix('!') {
            let parts: Vec<_> = command.split_whitespace().collect();
            if parts.len() != 3
                || parts[1..] != ["auth", "git-credential"]
                || (parts[0] != "gh" && !Path::new(parts[0]).is_absolute())
                || !parts[0]
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/_-.=~:@".contains(&b))
            {
                return Err(credential_helper_refusal(
                    HelperSafety::OpaqueShellCommand,
                    "delivery refuses an opaque shell credential helper",
                ));
            }
            if Path::new(parts[0]).is_absolute() {
                credential_helper(Path::new(parts[0]), writer)?;
            } else {
                self.executable("gh", writer)
                    .map_err(|error| credential_helper_refusal(HelperSafety::Unavailable, error))?;
            }
        } else {
            let parts: Vec<_> = value.split_whitespace().collect();
            if !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b" /_-.=~:@".contains(&b))
            {
                return Err(credential_helper_refusal(
                    HelperSafety::NonPlainExecutable,
                    "delivery credential helper needs a plain executable",
                ));
            }
            if Path::new(parts[0]).is_absolute() {
                credential_helper(Path::new(parts[0]), writer)?;
            } else if parts[0].contains('/') {
                return Err(credential_helper_refusal(
                    HelperSafety::LaneLocalPath,
                    "delivery credential helper must resolve outside the lane",
                ));
            }
        }
        Ok(())
    }
    fn host_environment(&mut self, writer: &WriterRequest) -> Result<()> {
        self.host_environment_with(writer, true)
    }
    fn host_environment_without_host_state(&mut self, writer: &WriterRequest) -> Result<()> {
        self.host_environment_with(writer, false)
    }
    fn host_environment_with(
        &mut self,
        writer: &WriterRequest,
        use_host_state: bool,
    ) -> Result<()> {
        // A host lane reads the host's own Git configuration and uses ordinary
        // transport. Its worktree and the primary checkout it was linked from
        // are its own, not another lane's writable area, whether or not the
        // lane is sandboxed; sandboxing still confines the lane's writes.
        self.trusted_worktree = (!self.isolated()).then(|| writer.worktree.clone());
        self.lane_roots = if use_host_state {
            let host_state = self.host_state().ok_or_else(|| {
                prepare_error(DeliveryPrepareStep::LaneRoots, "host state root unknown")
            })?;
            provenant_confinement::transport::LaneRoots::project(&writer.project, &host_state)
                .map_err(|error| prepare_error(DeliveryPrepareStep::LaneRoots, error))?
                .into_paths()
        } else {
            // Read-only previews have no validated host state, so project-only
            // port declarations decide the lane roots.
            provenant_confinement::transport::LaneRoots::project_without_host_state(&writer.project)
                .map_err(|error| prepare_error(DeliveryPrepareStep::LaneRoots, error))?
                .into_paths()
        };
        if writer.worktree != writer.project {
            self.lane_roots.push(writer.worktree.clone());
        }
        if let Some(run_state) = std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV) {
            self.lane_roots.push(PathBuf::from(run_state));
        }
        let captured_path = self
            .environment
            .iter()
            .find(|(key, _)| key == b"PATH")
            .map(|(_, value)| OsString::from_vec(value.clone()))
            .unwrap_or_default();
        let lane_path =
            FilteredLanePath::from_path(&captured_path, &writer.project, &writer.worktree)
                .map_err(|error| prepare_error(DeliveryPrepareStep::Environment, error))?;
        let path_bytes = lane_path.path().as_bytes().to_vec();
        if let Some((_, value)) = self.environment.iter_mut().find(|(key, _)| key == b"PATH") {
            *value = path_bytes;
        } else {
            self.environment.push((b"PATH".to_vec(), path_bytes));
        }
        self.lane_path = Some(lane_path);
        let isolated = self.isolated();
        let mut ssh_command_warning = false;
        let mut own_config_warning = false;
        let trusted_worktree = self.trusted_worktree.clone();
        for (key, value) in &mut self.environment {
            let name = std::str::from_utf8(key).unwrap_or_default();
            if name == "GIT_SSH_COMMAND" && !value.is_empty() {
                if isolated {
                    return Err(prepare_error(
                        DeliveryPrepareStep::Environment,
                        "GIT_SSH_COMMAND is not supported; use host SSH config",
                    ));
                }
                ssh_command_warning = true;
            }
            let path = OsString::from_vec(value.clone());
            if name != "PATH"
                && matches!(
                    name,
                    "HOME"
                        | "XDG_CONFIG_HOME"
                        | "GH_CONFIG_DIR"
                        | "GIT_CONFIG_GLOBAL"
                        | "GIT_CONFIG_SYSTEM"
                        | "GIT_SSH"
                        | "GIT_ASKPASS"
                        | "SSH_ASKPASS"
                )
                && !value.is_empty()
            {
                if matches!(name, "GIT_CONFIG_GLOBAL" | "GIT_CONFIG_SYSTEM")
                    && trusted_worktree
                        .as_ref()
                        .is_some_and(|root| Path::new(&path).starts_with(root))
                {
                    // The transport check verifies the resolved config and any
                    // includes against every other lane before remote use.
                    own_config_warning = true;
                } else {
                    *value = host_path(Path::new(&path), writer)
                        .map_err(|error| {
                            prepare_error(
                                DeliveryPrepareStep::Environment,
                                format!("{name}: {error}"),
                            )
                        })?
                        .into_os_string()
                        .into_vec();
                }
            }
        }
        if ssh_command_warning {
            self.transport_warning("delivery uses host GIT_SSH_COMMAND");
        }
        if own_config_warning {
            self.transport_warning("delivery uses the unsandboxed lane's Git configuration");
        }
        Ok(())
    }
    fn executable(&self, tool: &str, writer: &WriterRequest) -> Result<PathBuf> {
        if tool == "git" {
            let override_path = self
                .environment
                .iter()
                .find(|(key, _)| key == provenant_core::consumer_env::GIT.as_bytes())
                .map(|(_, value)| OsString::from_vec(value.clone()));
            let path =
                provenant_os::git::resolve_git_executable(override_path.as_deref().map(Path::new))
                    .ok_or(RunError::GitUnavailable)?;
            return host_path(&path, writer)
                .map_err(|error| prepare_error(DeliveryPrepareStep::GitExecutable, error));
        }
        let candidate = self
            .lane_path
            .as_ref()
            .ok_or_else(|| prepare_error(DeliveryPrepareStep::GhExecutable, "PATH is unavailable"))?
            .resolve(tool)
            .map_err(|error| {
                prepare_error(
                    DeliveryPrepareStep::GhExecutable,
                    format!("{tool} is unavailable on filtered PATH: {error}"),
                )
            })?;
        host_path(&candidate, writer)
            .map_err(|error| prepare_error(DeliveryPrepareStep::GhExecutable, error))?;
        Ok(candidate)
    }
    pub(super) fn command(
        &self,
        cwd: &Path,
        tool: &str,
        args: &[&str],
        stage: Stage,
    ) -> Result<String> {
        self.command_with_input(cwd, tool, args, stage, b"")
    }
    /// A config mutation can lose a lock race with a sibling settlement, so run
    /// it with the shared lock retry; ordinary host commands run once.
    pub(super) fn command_with_config_lock_retry(
        &self,
        cwd: &Path,
        tool: &str,
        args: &[&str],
        stage: Stage,
    ) -> Result<String> {
        self.command_with_input_uncached(cwd, tool, args, stage, b"", CommandMode::CONFIG_WRITE)
    }
    /// A named config read keeps its failure's leaf cause so the refusal names
    /// what actually failed; ordinary commands report only the stage.
    fn command_for_config(
        &self,
        cwd: &Path,
        tool: &str,
        args: &[&str],
        stage: Stage,
        input: &[u8],
    ) -> Result<String> {
        self.command_with_input_uncached(cwd, tool, args, stage, input, CommandMode::CONFIG_READ)
    }
    fn command_with_input(
        &self,
        cwd: &Path,
        tool: &str,
        args: &[&str],
        stage: Stage,
        input: &[u8],
    ) -> Result<String> {
        // Delivery may run after another worktree checks out the destination.
        // Reuse listings only for preparation; mutation checks need fresh facts.
        if stage == Stage::Prepare && tool == "git" && git_subcommand(args) == "worktree list" {
            let bytes = self.facts.cached_worktree_listing(cwd, || {
                self.command_with_input_uncached(
                    cwd,
                    tool,
                    args,
                    stage,
                    input,
                    CommandMode::ORDINARY,
                )
                .map(String::into_bytes)
            })?;
            return String::from_utf8(bytes).map_err(|_| RunError::DeliveryFact {
                stage,
                source: provenant_confinement::worktrees::WorktreeError::InvalidOutput,
            });
        }
        self.command_with_input_uncached(cwd, tool, args, stage, input, CommandMode::ORDINARY)
    }

    fn command_with_input_uncached(
        &self,
        cwd: &Path,
        tool: &str,
        args: &[&str],
        stage: Stage,
        input: &[u8],
        mode: CommandMode,
    ) -> Result<String> {
        if tool == "gh" {
            return self.forge_command_with_input(cwd, args, stage, input);
        }
        let failure = |cause: DeliveryCause| {
            if mode.preserve_reason {
                // A named config read needs the leaf cause for its refusal,
                // not just the stage that an ordinary command reports.
                RunError::ConfigCommandFailed {
                    reason: cause.to_string(),
                }
            } else if stage == Stage::Prepare {
                let step = match args.first().copied() {
                    Some("remote") => DeliveryPrepareStep::RemoteUrl,
                    Some("symbolic-ref") => DeliveryPrepareStep::Head,
                    Some("rev-parse") if args.contains(&"--absolute-git-dir") => {
                        DeliveryPrepareStep::GitDir
                    }
                    Some("rev-parse") => DeliveryPrepareStep::Head,
                    Some("config") => DeliveryPrepareStep::CredentialHelper,
                    Some("worktree") => DeliveryPrepareStep::Worktree,
                    _ if args.contains(&"fetch") => DeliveryPrepareStep::Fetch,
                    _ => DeliveryPrepareStep::GitExecutable,
                };
                prepare_error(step, cause)
            } else {
                RunError::delivery(stage, cause)
            }
        };
        let mut environment: Vec<_> = self
            .environment
            .iter()
            .map(|(k, v)| (OsString::from_vec(k.clone()), OsString::from_vec(v.clone())))
            .collect();
        let cwd = if let Some(bound) = &self.bound {
            environment.push(("GIT_DIR".into(), bound.git_dir.as_os_str().to_owned()));
            environment.push((
                "GIT_WORK_TREE".into(),
                bound.host_cwd.as_os_str().to_owned(),
            ));
            bound.host_cwd.as_path()
        } else {
            cwd
        };
        let tools = self
            .tools
            .as_ref()
            .ok_or_else(|| failure(DeliveryCause::parse("host tools are unavailable")))?;
        let executable = match tool {
            "git" => &tools.git,
            _ => {
                return Err(failure(DeliveryCause::parse(format!(
                    "{tool} is unavailable"
                ))));
            }
        };
        let git_options: &[&str] = if tool == "git" {
            &HOST_GIT_HARDENING_CONFIG_ARGS
        } else {
            &[]
        };
        // Advertisement and fetch share one hardened transport: the same
        // protocol limits everywhere, and a refusal to run when the effective
        // configuration or a transport program lives where a lane can write.
        let network = tool == "git" && reaches_remote(args);
        if network || (!self.isolated() && tool == "git" && git_subcommand(args) == "push") {
            self.check_transport(cwd, &environment, stage)?;
        }
        let transport = if network {
            provenant_confinement::transport::config_args()
        } else {
            Vec::new()
        };
        // Use a one-command Git configuration entry to keep URL userinfo out
        // of argv without re-reading a mutable named remote after binding.
        let secret_url = args.iter().copied().find(|url| {
            url.split_once("://").is_some_and(|(_, rest)| {
                rest.split('/')
                    .next()
                    .is_some_and(|authority| authority.contains('@'))
            })
        });
        let alias = if let Some(url) = secret_url {
            let unique = provenant_os::fs::temporary_name()
                .map_err(|error| failure(DeliveryCause::parse(error)))?;
            let alias = format!("provenant-bound-{}", unique.to_string_lossy());
            let count = environment
                .iter()
                .find(|(name, _)| name == "GIT_CONFIG_COUNT")
                .map(|(_, value)| value.to_string_lossy().parse::<usize>())
                .transpose()
                .map_err(|error| failure(DeliveryCause::parse(error)))?
                .unwrap_or(0);
            environment.retain(|(name, _)| name != "GIT_CONFIG_COUNT");
            environment.push(("GIT_CONFIG_COUNT".into(), (count + 1).to_string().into()));
            environment.push((
                format!("GIT_CONFIG_KEY_{count}").into(),
                format!("remote.{alias}.url").into(),
            ));
            environment.push((format!("GIT_CONFIG_VALUE_{count}").into(), url.into()));
            Some(alias)
        } else {
            None
        };
        let argv: Vec<OsString> = std::iter::once(executable.as_os_str().to_owned())
            .chain(git_options.iter().map(OsString::from))
            .chain(transport)
            .chain(args.iter().map(|arg| {
                if secret_url == Some(*arg) {
                    OsString::from(alias.as_deref().unwrap_or_default())
                } else {
                    OsString::from(arg)
                }
            }))
            .collect();
        let executor =
            SystemExecutor::new(1).map_err(|error| failure(DeliveryCause::parse(error)))?;
        let commit_log = tool == "git" && args.first() == Some(&"log");
        let limit = if tool == "git" && provenant_os::git::fact_capture_limit(args, 0) != 0 {
            host_output_limit(HostOutputClass::FactCapture)
        } else {
            stdout_limit(commit_log, args)
        };
        // A config mutation on the shared `.git/config` can lose a lock race to
        // a sibling settlement. Capture a bounded stderr tail so that collision
        // is detectable, and give every attempt a share of one 30s deadline.
        let make_options = |timeout: Duration| {
            let mut options = ExecOptions::new(&argv, cwd, timeout);
            options.cancel = self.cancellation.as_ref();
            options.process_group = true;
            options.env = Some(&environment);
            options.stdin = Some(input);
            options.stdout = StreamPolicy::Capture { limit };
            options.stop_on_output_limit = commit_log;
            // The fatal line is last, so every stage keeps a bounded stderr
            // tail as its cause, preparation included. Redaction is off by
            // default by owner decision.
            options.stderr = StreamPolicy::Tail {
                limit: DELIVERY_STDERR_LIMIT * 4,
            };
            options
        };
        let cancel = provenant_core::CancelToken::new();
        let attempt_cancel = self.cancellation.as_ref().unwrap_or(&cancel);
        let output = if mode.retry_config_lock {
            self.facts
                .with_executor(&executor, attempt_cancel, |executor| {
                    provenant_confinement::utility::execute_with_git_lock_retry(
                        "Git config mutation",
                        Duration::from_secs(30),
                        attempt_cancel,
                        |timeout| {
                            executor
                                .execute(make_options(timeout))
                                .map_err(provenant_confinement::utility::UtilityError::from)
                        },
                    )
                    .map_err(|error| {
                        RunError::Writer(provenant_confinement::worktrees::WorktreeError::Utility(
                            error,
                        ))
                    })
                })
                .map_err(|error: RunError| match error {
                    RunError::Writer(source) => RunError::DeliveryFact { stage, source },
                    other => other,
                })?
        } else {
            let options = make_options(Duration::from_secs(30));
            self.facts
                .with_executor(&executor, attempt_cancel, |executor| {
                    executor.execute(options).map_err(|error| {
                        failure(DeliveryCause::parse(format!(
                            "{}: {error}",
                            executable.display()
                        )))
                    })
                })
                .map_err(|error: RunError| match error {
                    RunError::Writer(provenant_confinement::worktrees::WorktreeError::Utility(
                        provenant_confinement::utility::UtilityError::Truncated {
                            stream: "stdout",
                        },
                    )) if tool == "git" && git_subcommand(args) == "worktree list" => {
                        RunError::DeliveryLimit {
                            stage,
                            tool: "git",
                            command: "worktree list".into(),
                            limit: self.facts.inventory_limit(),
                        }
                    }
                    RunError::Writer(source) => RunError::DeliveryFact { stage, source },
                    other => other,
                })?
        };
        // `git config --get-regexp` exits 1 with no output when nothing
        // matches, which is an absent setting rather than a failed read.
        let absent_helpers = args.first() == Some(&"config")
            && args.contains(&"--get-regexp")
            && output.termination == Termination::Exit(1)
            && output.stdout.bytes.is_empty();
        let absent_default = args.starts_with(&["symbolic-ref", "--quiet"])
            && output.termination == Termination::Exit(1)
            && output.stdout.bytes.is_empty();
        let bounded_log = commit_log && output.stdout.truncated && output.overflow_stopped;
        if output.stdout.truncated && !commit_log {
            return Err(RunError::DeliveryLimit {
                stage,
                tool: if tool == "gh" { "gh" } else { "git" },
                command: git_subcommand(args),
                limit,
            });
        }
        if (output.termination != Termination::Exit(0)
            && !absent_helpers
            && !absent_default
            && !bounded_log)
            || output.timed_out
            || output.stdout.read_failed
            || output.drain_truncated
        {
            if tool == "git"
                && args.first() == Some(&"merge-tree")
                && output.termination == Termination::Exit(1)
            {
                let stdout_str = String::from_utf8_lossy(&output.stdout.bytes);
                let conflicting_files: Vec<String> = stdout_str
                    .lines()
                    .skip(1)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                return Err(RunError::MergeConflict { conflicting_files });
            }
            return Err(failure(command_cause(&output)));
        }
        if commit_log && output.stdout.truncated {
            // A truncated log may end in the middle of a credential, including
            // one spanning lines. No prefix of it is safe to publish.
            return Ok(PR_BODY_TRUNCATION_NOTE.trim().to_owned());
        }
        String::from_utf8(output.stdout.bytes)
            .map(|v| v.trim().into())
            .map_err(|error| failure(DeliveryCause::parse(error)))
    }
    /// A fetch writes through a symref to the ref it points at, which would
    /// move that ref instead. Never fetch into one.
    pub(super) fn refuse_symref_destination(
        &self,
        cwd: &Path,
        reference: &str,
        stage: Stage,
    ) -> Result<()> {
        let target = self.command(cwd, "git", &["symbolic-ref", "--quiet", reference], stage)?;
        if target.is_empty() {
            return Ok(());
        }
        if stage == Stage::Prepare {
            return Err(prepare_error(
                DeliveryPrepareStep::Fetch,
                "destination is a symbolic ref",
            ));
        }
        Err(delivery_refusal(stage, "destination is a symbolic ref"))
    }

    /// Refuse an advertisement or fetch whose effective configuration is read
    /// from a lane-writable file, or names a transport program there.
    fn check_transport(
        &self,
        cwd: &Path,
        environment: &[(OsString, OsString)],
        stage: Stage,
    ) -> Result<()> {
        use provenant_confinement::transport;
        let refuse = |error: transport::TransportError| RunError::DeliveryTransport {
            stage,
            kind: (&error).into(),
            reason: prepare_reason(error),
        };
        let base = self
            .bound
            .as_ref()
            .map_or(cwd, |bound| bound.host_cwd.as_path());
        let mut protected = self.lane_roots.clone();
        if let Some(worktree) = &self.trusted_worktree {
            let tools = self
                .tools
                .as_ref()
                .ok_or_else(|| delivery_refusal(stage, "host tools are unavailable"))?;
            let executor = SystemExecutor::new(1)
                .map_err(|error| RunError::delivery(stage, DeliveryCause::parse(error)))?;
            let cancel = provenant_core::CancelToken::new();
            let own = std::fs::canonicalize(worktree).ok();
            let others = self
                .facts
                .discover_worktree_paths(
                    base,
                    &tools.git,
                    environment,
                    &executor,
                    Duration::from_secs(30),
                    self.cancellation.as_ref().unwrap_or(&cancel),
                )
                .map_err(|source| RunError::DeliveryFact { stage, source })?;
            protected.extend(others.into_iter().filter(|path| {
                path != worktree && (own.is_none() || std::fs::canonicalize(path).ok() != own)
            }));
        }
        let mut roots = transport::LaneRoots::new(protected);
        // A host lane reads the host's own Git configuration. Its own worktree
        // (also the Git process's base) and the primary checkout it was linked
        // from are host-owned; other lanes' worktrees stay protected above.
        if let Some(worktree) = &self.trusted_worktree {
            roots = roots
                .except_owned_worktree(worktree)
                .except_owned_worktree(base);
            if let Some(project) = &self.host_project {
                roots = roots.except_owned_worktree(project);
            }
        }
        let ctx =
            transport::ConfigContext::new("check delivery transport configuration", base, None);
        transport::check_environment(environment, base, &roots, &ctx).map_err(refuse)?;
        let context = provenant_os::git_config::ConfigContext {
            operation: "read effective Git configuration",
            file: base.to_owned(),
            key: None,
        };
        let listing = provenant_os::git_config::read(
            context.clone(),
            provenant_os::git_config::Read::Listing,
            |args| self.command_for_config(cwd, "git", &git_config_args(args, stage)?, stage, b""),
        )
        .map_err(|error| refuse(transport::unreadable(&ctx, error)))?;
        let entries = transport::parse_listing(listing.as_bytes(), &ctx).map_err(refuse)?;
        transport::check_listing(&entries, base, &roots, &ctx).map_err(refuse)?;
        // Git rereads configuration for the command itself, so refuse any
        // include directive that names a lane area, read or not: a lane could
        // otherwise create the target between this check and the command.
        let directives = provenant_os::git_config::read(
            provenant_os::git_config::ConfigContext {
                operation: "read configuration include directives",
                file: base.to_owned(),
                key: Some("include.path"),
            },
            provenant_os::git_config::Read::Directives,
            |args| self.command_for_config(cwd, "git", &git_config_args(args, stage)?, stage, b""),
        )
        .map_err(|error| refuse(transport::unreadable(&ctx, error)))?;
        let top = transport::parse_listing(directives.as_bytes(), &ctx).map_err(refuse)?;
        let home = environment
            .iter()
            .find(|(name, _)| name == "HOME")
            .map_or(base, |(_, home)| Path::new(home));
        let mut read = |file: &Path| -> std::result::Result<
            Option<Vec<transport::ConfigEntry>>,
            transport::TransportError,
        > {
            let ctx = transport::ConfigContext::new(
                "read included config file",
                "<included config>",
                Some("include.path"),
            );
            match std::fs::metadata(file) {
                Ok(metadata) if !metadata.is_file() => return Ok(None),
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(transport::unreadable(&ctx, error)),
            }
            file.to_str()
                .ok_or_else(|| transport::unreadable(&ctx, "included config path is not UTF-8"))?;
            let listing = provenant_os::git_config::read(
                provenant_os::git_config::ConfigContext {
                    operation: "read included config file",
                    file: PathBuf::from("<included config>"),
                    key: Some("include.path"),
                },
                provenant_os::git_config::Read::File(file),
                |args| {
                    self.command_for_config(cwd, "git", &git_config_args(args, stage)?, stage, b"")
                },
            )
            .map_err(|error| transport::unreadable(&ctx, error))?;
            transport::parse_listing(listing.as_bytes(), &ctx).map(Some)
        };
        transport::check_includes(&top, base, home, &roots, &mut read, &ctx).map_err(refuse)
    }
    pub(super) fn head(&self, writer: &WriterRequest) -> Result<GitOid> {
        let head = self.command(
            &writer.worktree,
            "git",
            &["rev-parse", "--verify", "HEAD"],
            Stage::Head,
        )?;
        let head = GitOid::parse(&head)
            .map_err(|error| RunError::delivery(Stage::Head, DeliveryCause::parse(error)))?;
        self.verify_objects(writer, head, Stage::Head)?;
        Ok(head)
    }
    fn verify_objects(&self, writer: &WriterRequest, head: GitOid, stage: Stage) -> Result<()> {
        let git = &self
            .tools
            .as_ref()
            .ok_or_else(|| delivery_refusal(stage, "host tools are unavailable"))?
            .git;
        let common = self.common_git_dir(&writer.project, stage)?;
        crate::objects::require_commit(
            git,
            &common,
            &writer.project,
            head,
            self.bound.as_ref().map(|bound| bound.base_head),
            self.cancellation.as_ref(),
        )
        .map_err(|error| RunError::delivery(stage, DeliveryCause::parse(error)))
    }
    fn trusted_common_git_dir(&self, writer: &WriterRequest, stage: Stage) -> Result<PathBuf> {
        self.discover_common_git_dir(&writer.project, stage)
    }

    /// A host lane reads the host-owned Git configuration; that is not another
    /// lane's worktree. A regular checkout keeps its metadata in a `.git`
    /// directory, so the host-owned project is its parent. A bare repository
    /// (or a relocated common directory) has no such parent checkout: the
    /// common directory itself holds the host's config and the lanes'
    /// worktrees, and Git's own inventory reports it as a worktree. Exempt it
    /// too, while `LaneRoots::contains` still protects other lanes' worktrees
    /// beneath it.
    fn set_host_project(&mut self, common_git_dir: &Path) {
        if self.isolated() {
            return;
        }
        self.host_project = if common_git_dir.file_name() == Some(std::ffi::OsStr::new(".git")) {
            common_git_dir.parent().map(Path::to_path_buf)
        } else {
            Some(common_git_dir.to_path_buf())
        };
    }

    pub(super) fn common_git_dir(&self, project: &Path, stage: Stage) -> Result<PathBuf> {
        match self.bound.as_ref() {
            Some(bound) => Ok(bound.common_git_dir.clone()),
            None => self.discover_common_git_dir(project, stage),
        }
    }

    fn discover_common_git_dir(&self, project: &Path, stage: Stage) -> Result<PathBuf> {
        // Use the validated host executable and checkout; never resolve through
        // lane-writable Git metadata after delivery has been bound.
        let failure = |cause: DeliveryCause| {
            if stage == Stage::Prepare {
                prepare_error(DeliveryPrepareStep::Head, cause)
            } else {
                RunError::delivery(stage, cause)
            }
        };
        let git = &self
            .tools
            .as_ref()
            .ok_or_else(|| failure(DeliveryCause::parse("host tools are unavailable")))?
            .git;
        let executor =
            SystemExecutor::new(1).map_err(|error| failure(DeliveryCause::parse(error)))?;
        let cancel = provenant_core::CancelToken::new();
        self.facts
            .with_executor(
                &executor,
                self.cancellation.as_ref().unwrap_or(&cancel),
                |executor| {
                    provenant_os::git::repository_path_with_cancel(
                        executor,
                        git,
                        project,
                        provenant_os::git::RepositoryPath::CommonDir,
                        self.cancellation.as_ref(),
                    )
                    .map_err(|error| failure(DeliveryCause::parse(error)))
                },
            )
            .map_err(|error: RunError| match error {
                RunError::Writer(source) => RunError::DeliveryFact { stage, source },
                other => other,
            })
    }
    fn clean(&self, writer: &WriterRequest) -> Result<()> {
        let path = writer.worktree.to_str().ok_or_else(|| {
            RunError::unsafe_path(writer.worktree.clone(), PathCheck::Containment)
        })?;
        let status = self.command(
            &writer.worktree,
            "git",
            &[
                "--work-tree",
                path,
                "status",
                "--porcelain=v1",
                "--untracked-files=all",
            ],
            Stage::Head,
        )?;
        if !status.is_empty() {
            return Err(delivery_refusal(
                Stage::Head,
                "writer worktree is not clean",
            ));
        }
        Ok(())
    }

    pub(super) fn commit_body(
        &self,
        writer: &WriterRequest,
        head: GitOid,
        identity: &str,
    ) -> Result<String> {
        let bound = self.bound.as_ref().ok_or_else(|| {
            delivery_refusal(Stage::Head, "delivery launch binding was not sealed")
        })?;
        self.verify_objects(writer, head, Stage::Head)?;
        self.verify_objects(writer, bound.base_head, Stage::Head)?;
        let range = if bound.base_head == head {
            head.to_string()
        } else {
            format!("{}..{head}", bound.base_head)
        };
        self.commit_range_body(writer, &range, identity)
    }

    fn commit_range_body(
        &self,
        writer: &WriterRequest,
        range: &str,
        identity: &str,
    ) -> Result<String> {
        let commits = self.command(
            &writer.worktree,
            "git",
            &[
                "log",
                "--no-show-signature",
                "--max-count=100",
                "--format=%s%n%b",
                range,
                "--",
            ],
            Stage::Head,
        )?;
        let body = format!("{identity}\n\n{commits}");
        let projection = self
            .publication_projector()
            .and_then(|projector| {
                projector.project(
                    provenant_confinement::publication::Destination::PullRequest,
                    body.as_bytes(),
                )
            })
            .map_err(|error| RunError::delivery(Stage::PullRequest, DeliveryCause::parse(error)))?;
        let body = String::from_utf8(projection.into_bytes())
            .map_err(|error| RunError::delivery(Stage::PullRequest, DeliveryCause::parse(error)))?;
        // Projection precedes truncation so an exact credential cannot be cut
        // into an unrecognisable prefix and then escape the final boundary.
        Ok(cap_pr_body(body))
    }

    /// Prepare objects without changing a checkout, then publish optimistically
    /// with an expected-old ref or explicit remote lease. A competing update
    /// refuses publication and can be retried through explicit host pickup.
    pub(super) fn integrate(
        &self,
        writer: &WriterRequest,
        request: &WriterDeliveryRequest,
        head: GitOid,
        forge: Option<&provenant_config::ForgeContext>,
    ) -> Result<WriterIntegration> {
        let stage = Stage::Integrate;
        self.refuse_protected_target(writer, &request.branch, stage, forge)
            .map_err(|error| delivery_stage_error(error, stage))?;
        let bound = self
            .bound
            .as_ref()
            .ok_or_else(|| delivery_refusal(stage, "delivery launch binding was not sealed"))?;
        self.refuse_protected_target(writer, &request.branch, stage, forge)
            .map_err(|error| delivery_stage_error(error, stage))?;
        self.verify_objects(writer, head, stage)?;
        self.clean(writer)?;
        if self.head(writer)? != head {
            return Err(delivery_refusal(
                Stage::Head,
                "writer head moved during delivery",
            ));
        }
        let destination = format!("refs/heads/{}", request.branch);
        let remote = !request.remote.is_empty();
        let resolved_remote = forge.is_some_and(|forge| request.remote == forge.remote.value);
        let remote_name =
            Self::sealed_remote((!request.remote.is_empty()).then_some(request.remote.as_str()))?;
        let target = if remote {
            let tracking = format!("refs/remotes/{}/{}", request.remote, request.branch);
            if !resolved_remote {
                self.refuse_symref_destination(&writer.worktree, &tracking, Stage::Fetch)?;
                self.command(
                    &writer.worktree,
                    "git",
                    &[
                        "fetch",
                        "--no-tags",
                        "--no-recurse-submodules",
                        "--",
                        &bound.push_url,
                        &format!("+{destination}:{tracking}"),
                    ],
                    stage,
                )?;
            }
            tracking
        } else {
            destination.clone()
        };
        let expected = if let Some(remote) = remote_name.as_ref().filter(|_| resolved_remote) {
            self.refresh_target_and_resolve(writer, &request.branch, stage, remote)?
        } else {
            self.command(
                &writer.worktree,
                "git",
                &["rev-parse", "--verify", &target],
                stage,
            )?
        };
        let expected_oid = GitOid::parse(&expected)
            .map_err(|error| RunError::delivery(stage, DeliveryCause::parse(error)))?;
        self.verify_objects(writer, expected_oid, stage)?;
        let integration = |integrated| WriterIntegration {
            extra: Default::default(),
            remote: bound.destination.remote.clone(),
            push_url: remote
                .then(|| provenant_records::custody::endpoint_identity(&bound.push_url)),
            target: destination.clone(),
            previous: Some(expected_oid),
            integrated,
        };
        let target_ref = GitRefName::new(&destination)
            .map_err(|error| RunError::delivery(stage, DeliveryCause::parse(error)))?;
        if self.landed(writer, head, target_ref, expected_oid, stage)? {
            return Ok(integration(expected_oid));
        }
        let checkout = if !remote {
            let trees = self.command(
                &writer.worktree,
                "git",
                &["worktree", "list", "--porcelain", "-z"],
                stage,
            )?;
            trees.split("\0\0").find_map(|record| {
                record
                    .split('\0')
                    .any(|line| line == format!("branch {destination}"))
                    .then(|| {
                        record
                            .split('\0')
                            .find_map(|line| line.strip_prefix("worktree "))
                    })
                    .flatten()
                    .map(std::path::PathBuf::from)
            })
        } else {
            None
        };
        if checkout.is_some() {
            // An index lock only excludes Git, not the parent's editor or
            // provider process. Never read-tree into a checkout we do not own:
            // even a clean preflight can race an overlapping file write.
            // Keep the child's sealed commit available for explicit pickup.
            return Err(RunError::IntegrationParentBusy { child_head: head });
        }
        let candidate = if self
            .command(
                &writer.worktree,
                "git",
                &["merge-base", "--is-ancestor", &expected, &head.to_string()],
                stage,
            )
            .is_ok()
        {
            head
        } else if request.fast_forward_only {
            // A loop round never merges: the target moved since the round began.
            return Err(RunError::IntegrationHeadMoved);
        } else {
            // A configured merge driver may execute arbitrary repository code.
            let config = provenant_os::git_config::merge_drivers(
                writer.worktree.join(".git/config"),
                |args| {
                    self.command_for_config(
                        &writer.worktree,
                        "git",
                        &git_config_args(args, stage)?,
                        stage,
                        b"",
                    )
                },
            )
            .map_err(|error| delivery_config_read_error(stage, error))?;
            if !config.is_empty() {
                return Err(delivery_refusal(
                    stage,
                    "configured merge drivers are not allowed",
                ));
            }
            let tree = self
                .command(
                    &writer.worktree,
                    "git",
                    &[
                        "merge-tree",
                        "--write-tree",
                        "--name-only",
                        "--no-messages",
                        &expected,
                        &head.to_string(),
                    ],
                    stage,
                )
                .map_err(|error| match error {
                    RunError::MergeConflict { conflicting_files } => {
                        RunError::IntegrationConflict {
                            child_head: head,
                            conflicting_files,
                        }
                    }
                    RunError::Delivery {
                        stage: Stage::Integrate,
                        ..
                    } => RunError::IntegrationConflict {
                        child_head: head,
                        conflicting_files: Vec::new(),
                    },
                    error => error,
                })?;
            let tree = GitOid::parse(tree.lines().next().unwrap_or_default())
                .map_err(|error| RunError::delivery(stage, DeliveryCause::parse(error)))?;
            let commit = self.command(
                &writer.worktree,
                "git",
                &[
                    "-c",
                    "user.name=Provenant",
                    "-c",
                    "user.email=provenant@localhost",
                    "commit-tree",
                    &tree.to_string(),
                    "-p",
                    &expected,
                    "-p",
                    &head.to_string(),
                    "-m",
                    &format!("Merge lane into {}", request.branch),
                ],
                stage,
            )?;
            GitOid::parse(&commit)
                .map_err(|error| RunError::delivery(stage, DeliveryCause::parse(error)))?
        };
        self.refuse_protected_target(writer, &request.branch, stage, forge)
            .map_err(|error| delivery_stage_error(error, stage))?;
        if remote {
            self.refuse_remote_symbolic_target(
                &writer.worktree,
                &bound.push_url,
                &request.branch,
                stage,
            )?;
        }
        if remote {
            self.command(
                &writer.worktree,
                "git",
                &[
                    "push",
                    "--no-follow-tags",
                    "--recurse-submodules=no",
                    &format!("--force-with-lease={destination}:{expected_oid}"),
                    "--",
                    &bound.push_url,
                    &format!("{candidate}:{destination}"),
                ],
                stage,
            )?;
            let verified = if let Some(remote) = remote_name.as_ref().filter(|_| resolved_remote) {
                self.refresh_target_and_resolve(writer, &request.branch, Stage::Verify, remote)?
            } else {
                self.refuse_symref_destination(&writer.worktree, &target, Stage::Verify)?;
                self.command(
                    &writer.worktree,
                    "git",
                    &[
                        "fetch",
                        "--no-tags",
                        "--no-recurse-submodules",
                        "--",
                        &bound.push_url,
                        &format!("+{destination}:{target}"),
                    ],
                    Stage::Verify,
                )?;
                self.command(
                    &writer.worktree,
                    "git",
                    &["rev-parse", "--verify", &target],
                    Stage::Verify,
                )?
            };
            if verified != candidate.to_string() {
                return Err(delivery_refusal(
                    Stage::Verify,
                    "verified ref differs from the integrated commit",
                ));
            }
        } else {
            self.command(
                &writer.worktree,
                "git",
                &[
                    "update-ref",
                    "--no-deref",
                    &destination,
                    &candidate.to_string(),
                    &expected,
                ],
                stage,
            )?;
        }
        if self.head(writer)? != head {
            return Err(delivery_refusal(
                Stage::Head,
                "writer head moved during delivery",
            ));
        }
        Ok(integration(candidate))
    }
    /// Whether `head` has landed on the target tip just read, through the
    /// shared landed check over the host's object store.
    fn landed(
        &self,
        writer: &WriterRequest,
        head: GitOid,
        reference: GitRefName,
        tip: GitOid,
        stage: Stage,
    ) -> Result<bool> {
        let graph = HostGraph {
            host: self,
            cwd: &writer.worktree,
            stage,
        };
        let target = provenant_confinement::worktrees::LandingTarget {
            reference: Some(reference),
            tip,
        };
        provenant_confinement::worktrees::landed(&graph, head, &target, &[])
            .map(|landing| landing.is_landed())
            .map_err(|error| RunError::delivery(stage, DeliveryCause::parse(error)))
    }
    // Bind the same default branch for lookup and creation. A failed lookup
    // is ambiguous and must never be treated as permission to create a PR.
    fn pr_base(&self, cwd: &Path, repo: &str) -> Result<String> {
        let raw = self
            .command(
                cwd,
                "gh",
                &["repo", "view", repo, "--json", "defaultBranchRef"],
                Stage::PullRequest,
            )
            .map_err(pr_lookup_error)?;
        let value: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|error| RunError::delivery(Stage::PullRequest, DeliveryCause::parse(error)))?;
        value["defaultBranchRef"]["name"]
            .as_str()
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                delivery_refusal(Stage::PullRequest, "gh did not return a default branch")
            })
    }
    fn existing_pr(
        &self,
        cwd: &Path,
        repo: &str,
        branch: &str,
        head: GitOid,
        base: &str,
    ) -> Result<Option<String>> {
        let raw = self
            .command(
                cwd,
                "gh",
                &[
                    "pr",
                    "list",
                    "--repo",
                    repo,
                    "--head",
                    branch,
                    "--state",
                    "all",
                    "--limit",
                    "1000",
                    "--json",
                    "url,state,headRefOid,baseRefName",
                ],
                Stage::PullRequest,
            )
            .map_err(pr_lookup_error)?;
        let values: Vec<serde_json::Value> = serde_json::from_str(&raw)
            .map_err(|error| RunError::delivery(Stage::PullRequest, DeliveryCause::parse(error)))?;
        if values.is_empty() {
            return Ok(None);
        }
        // Multiple results are ambiguous: do not choose one or create another.
        if values.len() != 1 {
            return Err(pr_reconciliation(
                "multiple pull requests match the branch",
                "inspect the branch's pull requests before retrying",
            ));
        }
        let value = &values[0];
        if value["headRefOid"].as_str() != Some(head.to_string().as_str()) {
            return Err(delivery_refusal(
                Stage::Verify,
                "pull request head differs from the delivered commit",
            ));
        }
        let url = value["url"]
            .as_str()
            .filter(|url| {
                url.starts_with("https://")
                    && !url.chars().any(char::is_whitespace)
                    && !url.chars().any(char::is_control)
            })
            .ok_or_else(|| {
                delivery_refusal(Stage::PullRequest, "pull request has no usable URL")
            })?;
        if value["baseRefName"].as_str() != Some(base) {
            return Err(pr_reconciliation(
                "pull request base differs from the expected base",
                &format!("inspect {url} and its base before retrying"),
            ));
        }
        if !matches!(value["state"].as_str(), Some("OPEN" | "MERGED")) {
            return Err(pr_reconciliation(
                "pull request is not open or merged",
                &format!("inspect {url} before retrying"),
            ));
        }
        Ok(Some(url.to_owned()))
    }

    fn adopted_pull_request(
        &self,
        cwd: &Path,
        target: &GraphAdoptionTarget<'_>,
        allowed_heads: &[GitOid],
    ) -> Result<AdoptedPullRequestObservation> {
        let number = target.pull_request.to_string();
        let raw = self
            .command(
                cwd,
                "gh",
                &[
                    "pr",
                    "view",
                    &number,
                    "--repo",
                    target.repository,
                    "--json",
                    "url,state,headRefName,headRefOid,headRepository,baseRefName,baseRepository",
                ],
                Stage::PullRequest,
            )
            .map_err(pr_lookup_error)?;
        let value: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|error| RunError::delivery(Stage::PullRequest, DeliveryCause::parse(error)))?;
        let url = value["url"]
            .as_str()
            .filter(|url| {
                url.starts_with("https://")
                    && !url.chars().any(char::is_whitespace)
                    && !url.chars().any(char::is_control)
            })
            .ok_or_else(|| {
                delivery_refusal(Stage::PullRequest, "pull request has no usable URL")
            })?;
        let head_ref = value["headRefName"].as_str();
        let head = value["headRefOid"]
            .as_str()
            .ok_or_else(|| delivery_refusal(Stage::Verify, "pull request has no head commit"))?
            .parse::<GitOid>()
            .map_err(|error| RunError::delivery(Stage::Verify, DeliveryCause::parse(error)))?;
        let head_repository = value["headRepository"]["nameWithOwner"].as_str();
        let base_repository = value["baseRepository"]["nameWithOwner"].as_str();
        if url != target.pull_request_url
            || value["state"].as_str() != Some("OPEN")
            || head_ref != Some(target.branch)
            || !allowed_heads.contains(&head)
            || value["baseRefName"].as_str() != Some(target.base_branch)
            || head_repository.is_none_or(|repo| !repo.eq_ignore_ascii_case(target.repository))
            || base_repository.is_none_or(|repo| !repo.eq_ignore_ascii_case(target.base_repository))
        {
            return Err(delivery_refusal(
                Stage::Verify,
                "live pull request differs from the authenticated adoption target",
            ));
        }
        Ok(AdoptedPullRequestObservation {
            url: url.to_owned(),
            head,
        })
    }

    pub(super) fn push(
        &self,
        writer: &WriterRequest,
        request: &WriterDeliveryRequest,
        head: GitOid,
        body: &str,
    ) -> Result<Published> {
        self.push_with_clean(writer, request, head, body, true)
    }
    fn push_with_clean(
        &self,
        writer: &WriterRequest,
        request: &WriterDeliveryRequest,
        head: GitOid,
        body: &str,
        require_clean: bool,
    ) -> Result<Published> {
        if self.head(writer)? != head {
            return Err(delivery_refusal(
                Stage::Head,
                "writer head moved during delivery",
            ));
        }
        // The destination was built from the owner's forge context and carries
        // its remote; reuse that context rather than resolving a second one
        // that could disagree with the one already sealed at launch. A holder
        // deserialized without the context resolves one from the host/project
        // port table; an unresolvable one protects only the primary checkout.
        // Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
        let forge = match self.forge.as_ref() {
            Some(forge) => Some(forge.clone()),
            None => self.resolve_preparation_forge(writer),
        };
        self.refuse_protected_target(writer, &request.branch, Stage::Push, forge.as_ref())
            .map_err(|error| match error {
                RunError::DeliveryLimit { .. } | RunError::DeliveryFact { .. } => error,
                other => RunError::delivery(Stage::Prepare, DeliveryCause::parse(other)),
            })?;
        if require_clean {
            self.clean(writer)?;
        }
        if matches!(request.mode, crate::DeliveryMode::Integrate(_)) {
            return Ok(Published {
                pull_request: None,
                integration: Some(self.integrate(writer, request, head, forge.as_ref())?),
            });
        }
        let bound = self.bound.as_ref().ok_or_else(|| {
            delivery_refusal(Stage::Prepare, "delivery launch binding was not sealed")
        })?;
        let cwd = &writer.worktree;
        let branch = self.command(
            cwd,
            "git",
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
            Stage::Head,
        )?;
        if branch != bound.lane_branch {
            return Err(delivery_refusal(
                Stage::Head,
                "writer branch changed after preparation",
            ));
        }
        let destination = format!("refs/heads/{}", request.branch);
        self.refuse_remote_symbolic_target(cwd, &bound.push_url, &request.branch, Stage::Push)?;
        // A previous push may have succeeded before its receipt was saved.
        let advertised = self.command(
            cwd,
            "git",
            &["ls-remote", "--", &bound.push_url, &destination],
            Stage::Push,
        )?;
        let previous =
            if advertised.is_empty() {
                None
            } else {
                let mut lines = advertised.lines();
                let (oid, reference) = lines
                    .next()
                    .and_then(|line| line.split_once('\t'))
                    .ok_or_else(|| delivery_refusal(Stage::Push, "advertised ref is malformed"))?;
                if reference != destination || lines.next().is_some() {
                    return Err(delivery_refusal(
                        Stage::Push,
                        "unexpected advertisement for the destination ref",
                    ));
                }
                Some(GitOid::parse(oid).map_err(|error| {
                    RunError::delivery(Stage::Push, DeliveryCause::parse(error))
                })?)
            };
        let already_pushed = previous == Some(head);
        if !already_pushed {
            let refspec = format!("{head}:{destination}");
            self.command(
                cwd,
                "git",
                &[
                    "push",
                    "--no-follow-tags",
                    "--recurse-submodules=no",
                    "--",
                    &bound.push_url,
                    &refspec,
                ],
                Stage::Push,
            )?;
        }
        let tracking = format!("refs/remotes/{}/{}", request.remote, request.branch);
        let fetchspec = format!("+{destination}:{tracking}");
        self.refuse_symref_destination(cwd, &tracking, Stage::Fetch)?;
        self.command(
            cwd,
            "git",
            &[
                "fetch",
                "--no-tags",
                "--no-recurse-submodules",
                "--",
                &bound.push_url,
                &fetchspec,
            ],
            Stage::Fetch,
        )?;
        let pushed = self.command(
            cwd,
            "git",
            &["rev-parse", "--verify", &tracking],
            Stage::Verify,
        )?;
        if pushed != head.to_string() || self.head(writer)? != head {
            return Err(delivery_refusal(
                Stage::Verify,
                "verified ref differs from the pushed head",
            ));
        }
        // A retried push that finds its own head already there cannot tell
        // whether it created the ref, so it records the head it found.
        let integration = Some(WriterIntegration {
            extra: Default::default(),
            remote: bound.destination.remote.clone(),
            push_url: Some(provenant_records::custody::endpoint_identity(
                &bound.push_url,
            )),
            target: destination.clone(),
            previous,
            integrated: head,
        });
        for (field, value) in [
            (BranchField::Remote, request.remote.as_str()),
            (BranchField::Merge, destination.as_str()),
            (BranchField::PushRemote, request.remote.as_str()),
        ] {
            let context = provenant_os::git_config::ConfigContext::new(
                "set branch upstream configuration",
                cwd.join(".git/config"),
                Some("branch tracking key"),
            );
            provenant_os::git_config::write(
                context,
                provenant_os::git_config::ConfigMutation::SetBranchTracking {
                    branch: bound.lane_branch.as_str().to_owned(),
                    field,
                    value: value.to_owned(),
                    replace: false,
                },
                |args| {
                    self.command_with_config_lock_retry(
                        cwd,
                        "git",
                        &git_config_args(args, Stage::Upstream)?,
                        Stage::Upstream,
                    )
                },
            )
            .map_err(|error| delivery_config_read_error(Stage::Upstream, error))?;
        }
        // Read the upstream back from config: `@{upstream}` and `status -sb`
        // only resolve when the remote's fetch refspec covers the branch, and a
        // narrowed clone (for example main only) is legitimate.
        for (key, value) in [
            ("remote", request.remote.as_str()),
            ("merge", destination.as_str()),
            ("pushRemote", request.remote.as_str()),
        ] {
            let config_key = format!("branch.{}.{key}", bound.lane_branch);
            let stored = provenant_os::git_config::read(
                provenant_os::git_config::ConfigContext {
                    operation: "verify upstream configuration",
                    file: cwd.to_owned(),
                    key: None,
                },
                provenant_os::git_config::Read::GetLocal(&config_key),
                |args| {
                    self.command_for_config(
                        cwd,
                        "git",
                        &git_config_args(args, Stage::Verify)?,
                        Stage::Verify,
                        b"",
                    )
                },
            )
            .map_err(|error| delivery_config_read_error(Stage::Verify, error))?;
            if stored != value {
                return Err(delivery_refusal(
                    Stage::Verify,
                    "verified upstream configuration differs from the request",
                ));
            }
        }
        if request.open_pr {
            let repo = bound.pull_request_repo.as_deref().ok_or_else(|| {
                delivery_refusal(Stage::PullRequest, "pull request repository was not bound")
            })?;
            let base_branch = self.pr_base(cwd, repo)?;
            if let Some(url) = self.existing_pr(cwd, repo, &request.branch, head, &base_branch)? {
                return Ok(Published {
                    pull_request: Some(url),
                    integration,
                });
            }
            let url = self.command_with_input(
                cwd,
                "gh",
                &[
                    "pr",
                    "create",
                    "--repo",
                    bound.pull_request_repo.as_deref().ok_or_else(|| {
                        delivery_refusal(
                            Stage::PullRequest,
                            "pull request repository was not bound",
                        )
                    })?,
                    "--head",
                    &request.branch,
                    "--base",
                    &base_branch,
                    "--title",
                    &format!("Lane delivery {}", &head.to_string()[..12]),
                    "--body-file",
                    "-",
                ],
                Stage::PullRequest,
                body.as_bytes(),
            )?;
            if !url.starts_with("https://") || url.lines().count() != 1 {
                return Err(delivery_refusal(
                    Stage::PullRequest,
                    "gh did not return a single https pull request URL",
                ));
            }
            Ok(Published {
                pull_request: Some(url),
                integration,
            })
        } else {
            Ok(Published {
                pull_request: None,
                integration,
            })
        }
    }
}

/// What a delivered push or integration published.
#[derive(Debug)]
pub(crate) struct Published {
    pub(crate) pull_request: Option<String>,
    pub(crate) integration: Option<WriterIntegration>,
}

/// The host's object store, read with delivery's own Git authority, for the
/// shared landed check. A commit `rev-list` cannot read is missing. The
/// parent listing stays small whatever the commit message's size.
struct HostGraph<'a> {
    host: &'a HostDeliveryContext,
    cwd: &'a Path,
    stage: Stage,
}

impl provenant_confinement::worktrees::CommitGraph for HostGraph<'_> {
    fn parents(
        &self,
        commit: GitOid,
    ) -> std::result::Result<Option<Vec<GitOid>>, provenant_confinement::worktrees::WorktreeError>
    {
        let commit = format!("{commit}^{{commit}}");
        let Ok(listing) = self.host.command(
            self.cwd,
            "git",
            &["rev-list", "--parents", "--max-count=1", &commit, "--"],
            self.stage,
        ) else {
            return Ok(None);
        };
        let mut oids = listing.split_ascii_whitespace().map(GitOid::parse);
        match oids.next() {
            Some(Ok(_)) => oids
                .collect::<std::result::Result<Vec<_>, _>>()
                .map(Some)
                .map_err(|_| provenant_confinement::worktrees::WorktreeError::InvalidOutput),
            _ => Err(provenant_confinement::worktrees::WorktreeError::InvalidOutput),
        }
    }

    fn contains(
        &self,
        ancestor: GitOid,
        tip: GitOid,
    ) -> std::result::Result<bool, provenant_confinement::worktrees::WorktreeError> {
        Ok(self
            .host
            .command(
                self.cwd,
                "git",
                &[
                    "merge-base",
                    "--is-ancestor",
                    &ancestor.to_string(),
                    &tip.to_string(),
                ],
                self.stage,
            )
            .is_ok())
    }
}

pub(super) fn cap_pr_body(mut body: String) -> String {
    if body.len() > PR_BODY_LIMIT - PR_BODY_TRUNCATION_NOTE.len() {
        body.truncate(body.floor_char_boundary(PR_BODY_LIMIT - PR_BODY_TRUNCATION_NOTE.len()));
        body.push_str(PR_BODY_TRUNCATION_NOTE);
    }
    body
}

/// A name with its non-ASCII characters escaped, so two spellings of one
/// name read differently.
fn spelled(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            out.push_str(&c.escape_unicode().to_string());
        }
    }
    format!("\"{name}\" ({out})")
}

/// The typed leaf cause of a failed host command: its exit or signal and a
/// bounded, single-line tail of stderr. `bounded_line` does not redact, which
/// matches the owner's default-off redaction decision.
fn command_cause(outcome: &provenant_os::exec::ExecOutcome) -> DeliveryCause {
    DeliveryCause::Command {
        termination: outcome.termination,
        timed_out: outcome.timed_out,
        stderr: provenant_core::bounded_tail(
            &String::from_utf8_lossy(&outcome.stderr.bytes),
            DELIVERY_STDERR_LIMIT,
        ),
    }
}

/// The cause of a failed forge command port call. The command's own exit or
/// signal is the cause only when the command failed; a command that exited 0
/// and then returned something the port could not use keeps the port's error.
fn failure_cause(
    command: Option<(Termination, bool, String)>,
    error: impl std::fmt::Display,
) -> DeliveryCause {
    match command {
        Some((termination, timed_out, stderr))
            if timed_out || termination != Termination::Exit(0) =>
        {
            DeliveryCause::Command {
                termination,
                timed_out,
                stderr,
            }
        }
        _ => DeliveryCause::parse(error),
    }
}

/// A delivery failure at `stage` with a descriptive cause, for refusals that
/// have no underlying command, I/O or parse error to carry.
fn delivery_refusal(stage: Stage, detail: impl std::fmt::Display) -> RunError {
    RunError::delivery(stage, DeliveryCause::parse(detail))
}

pub(super) fn prepare_error(step: DeliveryPrepareStep, detail: impl std::fmt::Display) -> RunError {
    RunError::DeliveryPrepare {
        step,
        refusal: None,
        reason: prepare_reason(detail),
    }
}

fn credential_helper_refusal(safety: HelperSafety, detail: impl std::fmt::Display) -> RunError {
    RunError::DeliveryPrepare {
        step: DeliveryPrepareStep::CredentialHelper,
        refusal: Some(DeliveryPrepareRefusal::CredentialHelper(safety)),
        reason: prepare_reason(detail),
    }
}

fn pr_reconciliation(reason: &str, next: &str) -> RunError {
    RunError::DeliveryReconciliation {
        reason: reason.into(),
        next: next.into(),
    }
}

fn pr_lookup_error(error: RunError) -> RunError {
    match error {
        RunError::Delivery {
            stage: Stage::PullRequest,
            cause,
        } => pr_reconciliation(
            &format!("pull request lookup failed: {cause}"),
            "inspect the repository and branch, then retry delivery",
        ),
        other => other,
    }
}

/// Output classes used by host-side repository and forge capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HostOutputClass {
    FactCapture,
    RefListing,
    CommitLog,
    OriginConfiguration,
    Ordinary,
}

/// One declaration for every bounded host output class.
pub(super) const HOST_COMMAND_OUTPUT_LIMITS: [(HostOutputClass, usize); 5] = [
    (
        HostOutputClass::FactCapture,
        provenant_os::exec::DEFAULT_CAPTURE_LIMIT,
    ),
    (
        HostOutputClass::RefListing,
        provenant_os::exec::DEFAULT_CAPTURE_LIMIT,
    ),
    (HostOutputClass::CommitLog, 56_000),
    (HostOutputClass::OriginConfiguration, 1024 * 1024),
    (HostOutputClass::Ordinary, 16 * 1024),
];

pub(super) fn host_output_limit(class: HostOutputClass) -> usize {
    HOST_COMMAND_OUTPUT_LIMITS
        .iter()
        .find_map(|(declared, limit)| (*declared == class).then_some(*limit))
        // Every class is declared (tested); an undeclared one gets the ordinary limit.
        .unwrap_or(16 * 1024)
}

pub(super) fn host_output_class(commit_log: bool, args: &[&str]) -> HostOutputClass {
    if commit_log {
        HostOutputClass::CommitLog
    } else if args.contains(&"--show-origin") {
        // A configuration listing with origins can exceed an ordinary reply.
        HostOutputClass::OriginConfiguration
    } else if matches!(git_subcommand(args).as_str(), "for-each-ref" | "ls-remote") {
        HostOutputClass::RefListing
    } else {
        HostOutputClass::Ordinary
    }
}

/// The stdout limit for non-fact host Git and forge calls. Ref listings grow
/// with the repository; other calls keep their own bounds.
pub(super) fn stdout_limit(commit_log: bool, args: &[&str]) -> usize {
    host_output_limit(host_output_class(commit_log, args))
}

/// The failed receipt state for a delivery error. A staged error keeps its
/// stage; a host limit or prepare refusal also keeps its redacted reason under
/// `reason`. Any other error is
/// a failed push.
pub(crate) fn failed_state(error: &RunError) -> WriterDeliveryState {
    let (stage, reason) = match error {
        RunError::Delivery { stage, cause } => (*stage, Some(prepare_reason(cause))),
        RunError::DeliveryReconciliation { .. } => {
            (Stage::PullRequest, Some(prepare_reason(error)))
        }
        RunError::IntegrationHeadMoved => (Stage::Integrate, None),
        RunError::IntegrationParentBusy { .. } | RunError::IntegrationConflict { .. } => {
            (Stage::Integrate, Some(prepare_reason(error)))
        }
        RunError::DeliveryLimit { stage, .. } | RunError::DeliveryFact { stage, .. } => {
            (*stage, Some(prepare_reason(error)))
        }
        RunError::DeliveryConfigRead { stage, cause } => (*stage, Some(prepare_reason(cause))),
        RunError::DeliveryTransport { stage, reason, .. } => (*stage, Some(reason.clone())),
        RunError::DeliveryPrepare { .. } => (Stage::Prepare, Some(prepare_reason(error))),
        _ => (Stage::Push, None),
    };
    WriterDeliveryState::Failed {
        extra: Default::default(),
        stage,
        reason,
    }
}

/// The subcommand words of a host call, for diagnostics. Global options and
/// their values (`-c <name=value>`, `-C <path>`, `--git-dir <path>`,
/// `--work-tree <path>`) are skipped, as are `--option=value` forms.
pub(super) fn git_subcommand(args: &[&str]) -> String {
    let mut words = Vec::new();
    let mut iter = args.iter().copied();
    while let Some(word) = iter.next() {
        match word {
            "-c" | "-C" | "--git-dir" | "--work-tree" | "--namespace" | "--exec-path" => {
                iter.next();
            }
            _ if word.starts_with('-') => {}
            _ => words.push(word),
        }
    }
    let mut words = words.into_iter();
    let verb = words.next().unwrap_or("git");
    match (verb, words.next()) {
        ("worktree", Some(sub)) => format!("worktree {sub}"),
        _ => verb.to_owned(),
    }
}

fn delivery_config_read_error(
    stage: Stage,
    error: provenant_os::git_config::ConfigError<RunError>,
) -> RunError {
    RunError::DeliveryConfigRead {
        stage,
        cause: Box::new(error),
    }
}

fn delivery_stage_error(error: RunError, stage: Stage) -> RunError {
    match error {
        RunError::DeliveryLimit {
            tool,
            command,
            limit,
            ..
        } => RunError::DeliveryLimit {
            stage,
            tool,
            command,
            limit,
        },
        RunError::DeliveryFact { source, .. } => RunError::DeliveryFact { stage, source },
        RunError::DeliveryConfigRead { cause, .. } => RunError::DeliveryConfigRead { stage, cause },
        RunError::Delivery { cause, .. } => RunError::delivery(stage, cause),
        other => RunError::delivery(stage, DeliveryCause::parse(other)),
    }
}

fn git_config_args(args: &[OsString], _stage: Stage) -> Result<Vec<&str>> {
    args.iter()
        .map(|arg| {
            arg.to_str().ok_or_else(|| RunError::ConfigCommandFailed {
                reason: "a Git config argument is not valid UTF-8".into(),
            })
        })
        .collect()
}

fn transport_config_cause(error: provenant_os::git_config::ConfigError<RunError>) -> String {
    // The config error's `Display` names the operation, file, key and the single
    // bounded leaf cause. Redact that one formatted string at this edge.
    prepare_reason(error)
}

pub(crate) fn prepare_reason(detail: impl std::fmt::Display) -> String {
    let detail = detail.to_string();
    let reason = detail.split_whitespace().collect::<Vec<_>>().join(" ");
    if reason.len() <= PREPARE_REASON_LIMIT {
        return reason;
    }
    // Keep the start, which names the step, and the end, where a command's
    // failure is: a reason cut at the start alone loses the fatal line.
    let head = reason.floor_char_boundary(PREPARE_REASON_LIMIT / 4);
    let tail =
        reason.ceil_char_boundary(reason.len() - (PREPARE_REASON_LIMIT - PREPARE_REASON_LIMIT / 4));
    format!(
        "{}{PREPARE_REASON_TRUNCATION_MARKER}{}",
        &reason[..head],
        &reason[tail..]
    )
}

fn credential_helper(path: &Path, writer: &WriterRequest) -> Result<()> {
    let helper = match host_path(path, writer) {
        Ok(helper) => helper,
        Err(error @ HostPathError::InsideWorktree) => {
            return Err(credential_helper_refusal(
                HelperSafety::LaneLocalPath,
                format!("credential helper: {error}"),
            ));
        }
        Err(error @ HostPathError::NotFound) => {
            return Err(credential_helper_refusal(
                HelperSafety::Unavailable,
                format!("credential helper: {error}"),
            ));
        }
    };
    if !helper.is_file() || !provenant_os::sys::is_executable(&helper) {
        return Err(credential_helper_refusal(
            HelperSafety::Unavailable,
            "configured credential helper is unavailable",
        ));
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum HostPathError {
    #[error("not found")]
    NotFound,
    #[error("inside the writer worktree")]
    InsideWorktree,
}

fn host_path(path: &Path, writer: &WriterRequest) -> std::result::Result<PathBuf, HostPathError> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        writer.project.join(path)
    };
    let canonical = match std::fs::canonicalize(&path) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or(HostPathError::NotFound)?;
            std::fs::canonicalize(parent)
                .map_err(|_| HostPathError::NotFound)?
                .join(path.file_name().ok_or(HostPathError::NotFound)?)
        }
        Err(_) => return Err(HostPathError::NotFound),
    };
    if canonical.starts_with(&writer.worktree) {
        return Err(HostPathError::InsideWorktree);
    }
    Ok(canonical)
}

/// Whether the Git command talks to a remote to advertise or fetch refs. The
/// subcommand is the first argument that is not an option or an option value.
fn reaches_remote(args: &[&str]) -> bool {
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match *arg {
            "-c" | "-C" | "--git-dir" | "--work-tree" => {
                args.next();
            }
            option if option.starts_with('-') => {}
            subcommand => return matches!(subcommand, "fetch" | "ls-remote"),
        }
    }
    false
}

fn pull_request_repo(url: &str) -> Result<String> {
    let (host, path) = if let Some((scheme, rest)) = url.split_once("://") {
        if !matches!(scheme, "https" | "ssh") {
            return Err(delivery_refusal(
                Stage::Prepare,
                "unsupported push URL scheme",
            ));
        }
        let (authority, path) = rest
            .split_once('/')
            .ok_or_else(|| delivery_refusal(Stage::Prepare, "push URL has no path"))?;
        (authority.rsplit('@').next().unwrap_or_default(), path)
    } else {
        let (authority, path) = url
            .split_once(':')
            .ok_or_else(|| delivery_refusal(Stage::Prepare, "push URL has no authority"))?;
        (authority.rsplit('@').next().unwrap_or_default(), path)
    };
    let path = path
        .split(['?', '#'])
        .next()
        .unwrap_or(path)
        .trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    if host.is_empty()
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
        || path.split('/').count() != 2
        || path.split('/').any(|p| {
            p.is_empty()
                || p == "."
                || p == ".."
                || !p
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
    {
        return Err(delivery_refusal(
            Stage::Prepare,
            "push URL is not a valid repository URL",
        ));
    }
    Ok(format!("{host}/{path}"))
}

pub(crate) fn validate(request: &WriterDeliveryRequest) -> Result<()> {
    if matches!(request.mode, crate::DeliveryMode::None) {
        return Err(RunError::Validation(
            "none delivery must omit the destination",
        ));
    }
    if matches!(request.mode, crate::DeliveryMode::Integrate(_)) && request.remote.is_empty() {
        GitRefName::branch(&request.branch)
            .map_err(|_| RunError::Validation("integration branch"))?;
        if request.open_pr {
            return Err(RunError::Validation(
                "integration cannot also open a lane PR",
            ));
        }
        return Ok(());
    }
    if request.remote.is_empty()
        || request.remote.len() > 64
        || request.remote.starts_with('-')
        || !request
            .remote
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err(RunError::Validation(
            "delivery needs a configured remote name",
        ));
    }
    GitRefName::branch(&request.branch).map_err(|_| RunError::Validation("delivery branch"))?;
    Ok(())
}

impl crate::RunStore {
    /// Record the remote branch a delivered push created, or found already
    /// there, in the repository's shared ref ledger. The endpoint is the one
    /// the receipt read back from the bound delivery. Integration only
    /// advances a target that already existed, so it never claims one. A
    /// failed record leaves the ref unobserved, which retains it.
    pub(crate) fn record_delivered_ref(
        &self,
        record: &crate::AttemptRecord,
        request: &WriterDeliveryRequest,
        receipt: &WriterDeliveryReceipt,
        call: &crate::Request<'_>,
    ) {
        use provenant_confinement::worktrees::{RefLedger, RefObservation, WorktreeError};
        use provenant_records::custody::{RefEndpoint, RefOrigin};
        if matches!(request.mode, crate::DeliveryMode::Integrate(_))
            || !matches!(receipt.state, WriterDeliveryState::Delivered { .. })
        {
            return;
        }
        let Some(integration) = receipt.integration.as_ref() else {
            return;
        };
        let Some(push_url) = integration.push_url.as_ref() else {
            return;
        };
        let operation_id = match self.attempt_operation(record.attempt_id, call) {
            Ok(operation) => Some(operation.operation_id),
            Err(error) => {
                delivery_advisory(format!(
                    "could not record ownership of delivered ref {}: {error}; it is retained as unobserved",
                    integration.target
                ));
                return;
            }
        };
        let result = (|| -> std::result::Result<(), WorktreeError> {
            let reference =
                GitRefName::new(&integration.target).map_err(|_| WorktreeError::InvalidRequest)?;
            let recorded_at = call
                .clock
                .wall_now()
                .map_err(|_| WorktreeError::InvalidRequest)?;
            RefLedger::open(&self.writer_registry, record.project_id)?.record(
                RefObservation {
                    endpoint: RefEndpoint::Remote {
                        remote: integration.remote.clone(),
                        push_url: push_url.clone(),
                    },
                    reference,
                    origin: if integration.previous.is_none() {
                        RefOrigin::Created
                    } else {
                        RefOrigin::Adopted
                    },
                    tip: integration.integrated,
                    recorded_at,
                    attempt_id: Some(record.attempt_id),
                    operation_id,
                },
                call.clock,
                call.cancel,
            )?;
            Ok(())
        })();
        if let Err(error) = result {
            delivery_advisory(format!(
                "could not record ownership of delivered ref {}: {error}; it is retained as unobserved",
                integration.target
            ));
        }
    }
    pub fn delivered(
        &self,
        id: provenant_core::AttemptId,
        call: &crate::Request<'_>,
    ) -> Result<Option<(WriterDeliveryRequest, WriterDeliveryReceipt)>> {
        let record = self.record(id, call)?;
        let attempt = self.attempt_dir(id, false)?;
        match attempt.read_stable(OsStr::new("delivery-pickup.json"), crate::RECORD_LIMIT) {
            Ok(raw) => {
                let pickup: PickupReceipt = crate::db::decode(&raw)?;
                return Ok(Some((pickup.request, pickup.receipt)));
            }
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
        let Some(receipt) = record
            .delivery
            .filter(|receipt| matches!(receipt.state, WriterDeliveryState::Delivered { .. }))
        else {
            return Ok(None);
        };
        let request = self
            .request(record.attempt_id, call)?
            .task
            .deliver
            .ok_or(RunError::Validation("delivered destination missing"))?;
        Ok(Some((request, receipt)))
    }
    pub fn worktree_settlement(
        &self,
        id: provenant_core::AttemptId,
        call: &crate::Request<'_>,
    ) -> Result<Option<serde_json::Value>> {
        self.record(id, call)?;
        let attempt = self.attempt_dir(id, false)?;
        match attempt.read_stable(OsStr::new("worktree-settlement.json"), crate::RECORD_LIMIT) {
            Ok(raw) => Ok(Some(crate::db::decode_receipt(&raw)?)),
            Err(error) if error.is_not_found() => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    /// Host lifecycle receipt, separate from the immutable terminal decision.
    pub fn record_worktree_settlement(
        &self,
        id: provenant_core::AttemptId,
        receipt: &serde_json::Value,
        call: &crate::Request<'_>,
    ) -> Result<()> {
        self.record_worktree_settlement_if(id, receipt, |_| true, call)
            .map(|_| ())
    }
    /// Write `receipt` only when `admit` accepts the prior receipt, read under
    /// the same lock as the write. A `pruned` receipt is never replaced.
    /// Returns whether the receipt was written.
    pub fn record_worktree_settlement_if(
        &self,
        id: provenant_core::AttemptId,
        receipt: &serde_json::Value,
        admit: impl FnOnce(Option<&serde_json::Value>) -> bool,
        call: &crate::Request<'_>,
    ) -> Result<bool> {
        self.record(id, call)?;
        let attempt = self.attempt_dir(id, false)?;
        let _lock = attempt.lock(
            OsStr::new("worktree-settlement.lock"),
            provenant_os::fs::LockMode::Exclusive,
            call.lock_budget(),
            call.cancel,
            call.clock,
        )?;
        let prior: Option<serde_json::Value> = match attempt
            .read_stable(OsStr::new("worktree-settlement.json"), crate::RECORD_LIMIT)
        {
            Ok(raw) => Some(crate::db::decode_receipt(&raw)?),
            Err(error) if error.is_not_found() => None,
            Err(error) => return Err(error.into()),
        };
        if prior
            .as_ref()
            .is_some_and(|prior| prior["state"] == "pruned")
            || !admit(prior.as_ref())
        {
            return Ok(false);
        }
        attempt
            .atomic_write(
                OsStr::new("worktree-settlement.json"),
                ".settlement-",
                &crate::db::bytes(receipt)?,
                provenant_os::fs::PublishMode::Replace,
            )?
            .require_durable()
            .map_err(RunError::from)?;
        Ok(true)
    }
    /// Explicit host pickup of a settled writer. Its immutable terminal record
    /// stays intact; the keyed sealed receipt records this later delivery.
    pub fn deliver(
        &self,
        id: provenant_core::AttemptId,
        override_request: Option<WriterDeliveryRequest>,
        host: HostDeliveryContext,
        accept_review: bool,
        call: &crate::Request<'_>,
    ) -> Result<WriterDeliveryReceipt> {
        let record = self.record(id, call)?;
        let review_acceptance = match record.state {
            crate::AttemptState::Terminal {
                outcome: crate::Outcome::Succeeded,
                ..
            } => None,
            crate::AttemptState::Terminal {
                outcome: crate::Outcome::ReviewRequired,
                ..
            } if accept_review => Some(crate::WriterReviewAcceptance {
                extra: Default::default(),
                actor: self.context.actor_id,
                protected_paths: record.protected_paths.clone(),
            }),
            crate::AttemptState::Terminal {
                outcome: crate::Outcome::ReviewRequired,
                ..
            } => {
                return Err(RunError::ReviewAcceptanceRequired);
            }
            _ => {
                return Err(RunError::Validation(
                    "delivery needs a succeeded settled writer",
                ));
            }
        };
        let task = self.request(record.attempt_id, call)?.task;
        let writer = task
            .writer
            .ok_or(RunError::Validation("delivery needs a writer"))?;
        let request = override_request
            .or(task.deliver.clone())
            .ok_or(RunError::Validation(
                "choose --deliver for a lane without recorded delivery",
            ))?;
        validate(&request)?;
        let head = record
            .head
            .ok_or(RunError::Validation("receipt has no settled head"))?;
        let attempt = self.attempt_dir(id, false)?;
        let key = provenant_core::hex(
            provenant_records::CanonicalV1::file_digest(
                &serde_json::to_vec(&(head, &request))
                    .map_err(|_| RunError::Validation("delivery key"))?,
            )
            .bytes(),
        );
        let receipt_name = format!("delivery-complete-{key}.json");
        if let Some(raw) =
            crate::artifacts::read_sealed(&attempt, &receipt_name, crate::RECORD_LIMIT)?
        {
            let receipt: WriterDeliveryReceipt = crate::db::decode(&raw)?;
            if matches!(receipt.state, WriterDeliveryState::Delivered { .. }) {
                // Recover a crash between sealing success and publishing
                // the mutable pickup pointer used by graph observation.
                publish_pickup(&attempt, &request, &receipt)?;
                return Ok(receipt);
            }
        }
        if let Some(receipt) = record.delivery.as_ref()
            && task.deliver.as_ref() == Some(&request)
            && receipt.remote == request.remote
            && receipt.branch == request.branch
            && matches!(receipt.state, WriterDeliveryState::Delivered { .. })
            && (!request.open_pr
                || matches!(
                    receipt.state,
                    WriterDeliveryState::Delivered {
                        pull_request: Some(_),
                        ..
                    }
                ))
        {
            return Ok(receipt.clone());
        }
        if !self.custody_released(&record, call)? {
            return Err(RunError::Validation("writer custody has not settled"));
        }
        let mut launch = crate::prepare_writer_with_facts(
            &writer,
            &self.writer_registry,
            record.project_id,
            &self.repo_facts,
        )?;
        let state = launch
            .custody
            .take()
            .ok_or_else(|| RunError::unsafe_path(writer.worktree.clone(), PathCheck::Missing))?
            .0;
        let _writer = provenant_supervisor::custody::lock_writer_exclusion(
            &state,
            call.lock_budget(),
            call.cancel,
            call.clock,
        )
        .map_err(|error| match error {
            provenant_supervisor::custody::CustodyError::Filesystem(error) => {
                RunError::Filesystem(error)
            }
            other => RunError::Io(std::io::Error::other(other.to_string())),
        })?;
        let host = host
            .with_repo_facts(self.repo_facts())
            .with_host_state(&self.writer_registry)
            .prepare(&writer, &request)?;
        host.clean(&writer)?;
        if host.head(&writer)? != head {
            return Err(RunError::Validation(
                "worktree head differs from the settled receipt",
            ));
        }
        let result = host
            .commit_body(
                &writer,
                head,
                &format!("Run {}\nAttempt {id}", record.attempt_id),
            )
            .and_then(|body| host.push(&writer, &request, head, &body));
        let (state, integration) = match result {
            Ok(published) => (
                WriterDeliveryState::Delivered {
                    extra: Default::default(),
                    pull_request: published.pull_request,
                },
                published.integration,
            ),
            Err(error) => (failed_state(&error), None),
        };
        let receipt = WriterDeliveryReceipt {
            extra: Default::default(),
            schema_version: 1,
            remote: request.remote.clone(),
            branch: request.branch.clone(),
            head: Some(head),
            review_acceptance,
            state,
            integration,
        };
        // Failed effects may be explicitly retried; successful effects replay.
        if matches!(receipt.state, WriterDeliveryState::Delivered { .. }) {
            crate::artifacts::seal(
                &attempt,
                &receipt_name,
                &crate::db::bytes(&receipt)?,
                crate::RECORD_LIMIT,
            )?;
            publish_pickup(&attempt, &request, &receipt)?;
            self.record_delivered_ref(&record, &request, &receipt, call);
            let _ = self.record_effect(
                id,
                record.generation,
                crate::SettlementEffect::Delivery,
                crate::EffectDisposition::Done,
                call,
            );
        }
        Ok(receipt)
    }
}

#[cfg(test)]
#[path = "delivery_inline_tests.rs"]
mod delivery_inline_tests;
