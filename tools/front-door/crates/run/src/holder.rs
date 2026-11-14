//! The attempt holder: Run's admission and terminal transaction, composed into
//! the same-binary guardian. It runs while the guardian still holds custody.
use crate::{artifacts::read_sealed, launch::Providers, *};
use provenant_confinement::permissions::PathGrant;
use provenant_core::{ActorId, AttemptId, CancelToken, Clock, ProjectId, RootInputs};
use provenant_os::{clock::SystemClock, exec::Termination, fs::Directory};
use provenant_supervisor::{
    custody::{self, CustodyError, ExitObservation},
    machine::StopReason,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn codex_attempt_usage(
    usage: &mut UsageRecord,
    session: Option<&str>,
    prior: Option<(Option<&str>, &UsageRecord)>,
    baseline_unknown: bool,
) {
    let total = usage.clone();
    let baseline = match (baseline_unknown, prior) {
        (true, _) => None,
        (false, None) => Some(UsageRecord::default()),
        (false, Some((previous_session, _)))
            if session.is_some() && previous_session.is_some() && previous_session != session =>
        {
            Some(UsageRecord::default())
        }
        (false, Some((previous_session, previous_usage)))
            if session.is_some() && previous_session == session =>
        {
            previous_usage
                .extra
                .get("session_total")
                .and_then(|v| serde_json::from_value::<UsageRecord>(v.clone()).ok())
        }
        (false, Some(_)) => None,
    };
    let delta = |current: Option<u64>, previous: Option<u64>| match baseline.as_ref() {
        None => None,
        Some(_)
            if prior.is_none()
                || prior.is_some_and(|(previous_session, _)| {
                    session.is_some() && previous_session.is_some() && previous_session != session
                }) =>
        {
            current
        }
        Some(_) => current.zip(previous).and_then(|(a, b)| a.checked_sub(b)),
    };
    usage.uncached_input = delta(
        total.uncached_input,
        baseline.as_ref().and_then(|b| b.uncached_input),
    );
    usage.cache_read = delta(
        total.cache_read,
        baseline.as_ref().and_then(|b| b.cache_read),
    );
    usage.cache_write = delta(
        total.cache_write,
        baseline.as_ref().and_then(|b| b.cache_write),
    );
    usage.cache_write_1h = delta(
        total.cache_write_1h,
        baseline.as_ref().and_then(|b| b.cache_write_1h),
    );
    usage.output_excluding_reasoning = delta(
        total.output_excluding_reasoning,
        baseline.as_ref().and_then(|b| b.output_excluding_reasoning),
    );
    usage.reasoning = delta(total.reasoning, baseline.as_ref().and_then(|b| b.reasoning));
    usage
        .extra
        .insert("per_attempt".into(), serde_json::Value::Bool(true));
    if let Ok(value) = serde_json::to_value(total) {
        usage.extra.insert("session_total".into(), value);
    }
}

#[cfg(test)]
mod codex_usage_tests {
    use super::*;

    #[test]
    fn codex_session_deltas_and_unknown_baseline() {
        let mut first = UsageRecord {
            uncached_input: Some(20),
            cache_read: Some(80),
            output_excluding_reasoning: Some(7),
            ..Default::default()
        };
        codex_attempt_usage(&mut first, Some("t"), None, false);
        assert_eq!(first.uncached_input, Some(20));
        assert_eq!(first.extra["session_total"]["cache_read"], 80);
        let mut second = UsageRecord {
            uncached_input: Some(50),
            cache_read: Some(100),
            output_excluding_reasoning: Some(12),
            ..Default::default()
        };
        codex_attempt_usage(&mut second, Some("t"), Some((Some("t"), &first)), false);
        assert_eq!(second.uncached_input, Some(30));
        assert_eq!(second.cache_read, Some(20));
        assert_eq!(second.output_excluding_reasoning, Some(5));
        assert_eq!(second.extra["session_total"]["uncached_input"], 50);
        let mut unknown = second.clone();
        codex_attempt_usage(
            &mut unknown,
            Some("t"),
            Some((Some("t"), &UsageRecord::default())),
            false,
        );
        assert_eq!(unknown.uncached_input, None);
    }
}

fn account_directories(
    specs: &[provenant_records::AdapterSpec],
    lookup: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> std::collections::BTreeMap<String, PathBuf> {
    specs
        .iter()
        .filter_map(|spec| {
            let (environment, _) = spec.default_account_directory?;
            lookup(environment).map(|path| (spec.name.to_owned(), PathBuf::from(path)))
        })
        .collect()
}

/// Inputs that re-resolve the submitting edge's state root in the guardian,
/// whose environment is cleared.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootSeed {
    pub home: PathBuf,
    pub checkout: PathBuf,
    pub state_root: PathBuf,
}
/// Host-only launch inputs, passed on the custody pipe rather than through
/// lane-writable state. Provider credentials are resolved afresh on the host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostLaunchContext {
    pub configuration_root: PathBuf,
    pub cwd: PathBuf,
    /// Host-owned graph origin, independent of the provider's selected checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_directory: Option<PathBuf>,
    pub executables: std::collections::BTreeMap<String, PathBuf>,
    /// Original host search directories for providers unavailable during handoff.
    #[serde(default)]
    pub executable_search_path: Vec<PathBuf>,
    pub git: PathBuf,
    /// Trusted environment overrides for adapter default account directories.
    #[serde(default)]
    pub default_account_directories: std::collections::BTreeMap<String, PathBuf>,
    /// Host-resolved relocated credential stores, inherited unchanged by children.
    /// Required on the handoff pipe: an older host must not silently omit the
    /// inventory needed by the ambient renderer.
    pub credential_roots: Vec<PathGrant>,
    pub github_roots: Vec<PathBuf>,
    /// Trusted path overrides needed to select the same host authentication
    /// mechanism after the guardian clears its process environment.
    pub provider_path_environment: std::collections::BTreeMap<String, std::ffi::OsString>,
    #[serde(default)]
    pub cargo_build_jobs: Option<String>,
    /// Synthetic fake-provider evidence; absent from installed builds.
    #[cfg(feature = "fixture")]
    #[serde(default)]
    pub fixture_state_write: bool,
}
impl HostLaunchContext {
    pub fn provider_path_environment() -> std::collections::BTreeMap<String, std::ffi::OsString> {
        [
            "CODEX_HOME",
            "CLAUDE_CONFIG_DIR",
            "GH_CONFIG_DIR",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
        ]
        .into_iter()
        .filter_map(|name| std::env::var_os(name).map(|value| (name.into(), value)))
        .collect()
    }
    pub fn github_roots() -> Vec<PathBuf> {
        provenant_confinement::permissions::host_github_roots(|name| std::env::var_os(name))
    }
    pub fn credential_roots() -> Vec<PathGrant> {
        provenant_confinement::permissions::host_credential_roots(|name| std::env::var_os(name))
    }
    pub fn default_account_directories() -> std::collections::BTreeMap<String, PathBuf> {
        account_directories(provenant_records::ADAPTER_SPECS, |name| {
            std::env::var_os(name)
        })
    }
    /// The inputs a host derives for itself when no guardian handed it any:
    /// the caller's trusted roots and git, with search path and tool homes
    /// from this process's environment and no pinned executables.
    pub fn from_environment(
        configuration_root: &std::path::Path,
        cwd: &std::path::Path,
        git: PathBuf,
    ) -> Self {
        Self {
            configuration_root: configuration_root.into(),
            cwd: cwd.into(),
            graph_directory: None,
            executables: Default::default(),
            executable_search_path: std::env::var_os("PATH")
                .map(|path| {
                    std::env::split_paths(&path)
                        .filter(|path| path.is_absolute())
                        .collect()
                })
                .unwrap_or_default(),
            git,
            default_account_directories: Self::default_account_directories(),
            credential_roots: Self::credential_roots(),
            github_roots: Self::github_roots(),
            provider_path_environment: Self::provider_path_environment(),
            cargo_build_jobs: std::env::var("CARGO_BUILD_JOBS").ok(),
            #[cfg(feature = "fixture")]
            fixture_state_write: false,
        }
    }
}
/// Opaque to supervisor; travels on the handoff pipe only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HolderContext {
    pub schema_version: u64,
    pub roots: RootSeed,
    pub project_id: ProjectId,
    pub actor_id: ActorId,
    pub attempt_id: AttemptId,
    /// Private handoff only, never copied to the attempt or provider environment.
    #[serde(default)]
    pub host_delivery: Option<crate::HostDeliveryContext>,
    pub launch: Option<HostLaunchContext>,
    /// The request and task the host launched this attempt from. Its child
    /// launcher bounds children by these, since the store holding them can be
    /// a parent lane's, which that lane may rewrite.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sealed: Option<SealedTask>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedTask {
    pub request_digest: provenant_core::Digest,
    pub effective_network: NetworkAccess,
    pub effective_database_ipc: bool,
}
impl SealedTask {
    /// Restore host-sealed launch policy before validating the next generation.
    pub fn apply_access(&self, task: &mut TaskRequest) {
        task.access.network = Some(self.effective_network);
        task.access.database_ipc = self.effective_database_ipc;
    }
}

enum Completion<'a> {
    Live {
        custody: &'a custody::Recovery,
        delivery: Option<&'a crate::HostDeliveryContext>,
        /// Request digest the host sealed at launch.
        sealed: Option<&'a provenant_core::Digest>,
        holder_context: &'a HolderContext,
    },
    Recovered(Option<&'a custody::Recovery>),
}

#[derive(Debug)]
struct Finished {
    record: AttemptRecord,
    release_permitted: bool,
}
impl Completion<'_> {
    fn custody(&self) -> Option<&custody::Recovery> {
        match self {
            Self::Live { custody, .. } => Some(custody),
            Self::Recovered(custody) => *custody,
        }
    }
    fn holder_context(&self) -> Option<&HolderContext> {
        match self {
            Self::Live { holder_context, .. } => Some(holder_context),
            Self::Recovered(_) => None,
        }
    }
}
impl HolderContext {
    /// Configuration and output roots sealed by the launching host.
    pub fn disk_roots(&self) -> (&std::path::Path, &std::path::Path) {
        self.launch.as_ref().map_or(
            (
                self.roots.state_root.as_path(),
                self.roots.checkout.as_path(),
            ),
            |launch| (launch.configuration_root.as_path(), launch.cwd.as_path()),
        )
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        crate::db::bytes(self)
    }
    /// Open the store, retrying transient failures for about 30 s so a busy
    /// host does not turn into a refused or stranded attempt.
    pub fn open(raw: &[u8]) -> Result<(Self, RunStore)> {
        Self::open_with_clock(raw, &SystemClock)
    }
    fn open_with_clock(raw: &[u8], clock: &dyn Clock) -> Result<(Self, RunStore)> {
        use provenant_core::retry::{Backoff, RetryCause, is_transient};
        const BACKOFF: Backoff = Backoff::new(
            std::time::Duration::from_millis(100),
            2,
            std::time::Duration::from_secs(5),
        );
        for attempt in 0..10 {
            match Self::open_once(raw, clock) {
                Err(error) => {
                    let cause = match error {
                        RunError::Contention => RetryCause::Contention,
                        RunError::Sqlite(_) | RunError::Io(_) | RunError::OperationIo { .. } => {
                            RetryCause::Transport
                        }
                        _ => RetryCause::Permanent,
                    };
                    if !is_transient(cause) {
                        return Err(error);
                    }
                    let micros = u64::try_from(BACKOFF.delay(attempt).as_micros())
                        .map_err(|_| RunError::Admission(AdmissionError::Clock))?;
                    let deadline = clock
                        .mono_now()
                        .checked_add(provenant_core::DurationMicros::from_micros(micros))
                        .map_err(|_| RunError::Admission(AdmissionError::Clock))?;
                    clock
                        .wait_until(deadline)
                        .map_err(|_| RunError::Admission(AdmissionError::Clock))?;
                }
                other => return other,
            }
        }
        Self::open_once(raw, clock)
    }
    fn open_once(raw: &[u8], clock: &dyn Clock) -> Result<(Self, RunStore)> {
        let context: Self = crate::db::decode(raw)?;
        let mut inputs =
            RootInputs::new(context.roots.home.clone(), context.roots.checkout.clone());
        inputs.state_dir = Some(context.roots.state_root.clone());
        let state_root = inputs
            .resolve(&provenant_os::fs::SystemPathResolver)
            .map_err(|_| {
                RunError::unsafe_path(context.roots.state_root.as_path(), PathCheck::Containment)
            })?
            .state;
        if state_root.as_path() != context.roots.state_root {
            return Err(RunError::unsafe_path(
                context.roots.state_root.as_path(),
                PathCheck::Containment,
            ));
        }
        let execution = ExecutionContext {
            state_root,
            project_id: context.project_id,
            actor_id: context.actor_id,
            read_prompts: false,
        };
        let registry = context
            .launch
            .as_ref()
            .map_or(context.roots.state_root.as_path(), |host| {
                host.configuration_root.as_path()
            });
        let mut store = RunStore::open(&execution, &call(clock, &CancelToken::new())?)?
            .with_writer_registry(registry);
        if let Some(sealed) = &context.sealed {
            store = store.with_sealed_parent_network(sealed.effective_network);
        }
        Ok((context, store))
    }
}
fn call<'a>(clock: &'a dyn Clock, cancel: &'a CancelToken) -> Result<Request<'a>> {
    Ok(Request { clock, cancel })
}

pub struct RunHolder<'a> {
    pub providers: &'a dyn Providers,
    pub resources: &'a dyn Resources,
    pub clock: &'a dyn Clock,
}
/// Bind the host's sealed output roots to admission's resource observations.
struct LaneResources<'a> {
    resources: &'a dyn Resources,
    context: &'a HolderContext,
}
impl Resources for LaneResources<'_> {
    fn observe_process(&self, pid: i64) -> provenant_supervisor::census::ProcessObservation {
        self.resources.observe_process(pid)
    }
    fn process_budget(&self) -> std::io::Result<provenant_os::sys::resources::ProcessBudget> {
        self.resources.process_budget()
    }
    fn pressure(&self) -> std::io::Result<Option<provenant_os::sys::resources::MemoryPressure>> {
        self.resources.pressure()
    }
    fn guardian_identity(&self) -> Option<provenant_supervisor::census::ProcessIdentity> {
        self.resources.guardian_identity()
    }
    fn memory(&self) -> std::io::Result<provenant_os::sys::resources::MemorySample> {
        self.resources.memory()
    }
    fn disk(&self, attempt: &Directory) -> std::io::Result<u64> {
        self.resources.lane_disk(attempt, self.context)
    }
    fn reclaim_disk(&self, attempt: &Directory) -> bool {
        self.resources.reclaim_lane_disk(attempt, self.context)
    }
}
fn custody_error(error: RunError) -> CustodyError {
    match error {
        RunError::Admission(error) => CustodyError::Admission(error),
        other => CustodyError::Io(std::io::Error::other(other.to_string())),
    }
}

/// A resource refusal seals the failed attempt without certifying its graph.
/// Its writer reservation and private objects remain available for recovery.
#[derive(Serialize, Deserialize)]
struct ObjectImportRefusal {
    schema_version: u64,
    scope: custody::Scope,
    kind: String,
    worktree: PathBuf,
    reason: String,
}

/// Seal the refusal that keeps the private Git for host recovery.
fn seal_import_refusal(
    attempt: &Directory,
    scope: custody::Scope,
    worktree: &std::path::Path,
    reason: &str,
) -> Result<()> {
    crate::artifacts::seal(
        attempt,
        "object-import-refused.json",
        &crate::db::bytes(&ObjectImportRefusal {
            schema_version: 1,
            scope,
            kind: "object_import_refusal".into(),
            worktree: worktree.into(),
            reason: reason.chars().take(4096).collect(),
        })?,
        RECORD_LIMIT,
    )
}

pub(crate) fn object_import_refused(
    attempt: &Directory,
    scope: custody::Scope,
) -> Result<Option<String>> {
    let Some(raw) = read_sealed(attempt, "object-import-refused.json", RECORD_LIMIT)? else {
        return Ok(None);
    };
    let refusal: ObjectImportRefusal = crate::db::decode(&raw)?;
    if refusal.schema_version != 1
        || refusal.scope != scope
        || refusal.kind != "object_import_refusal"
        || refusal.reason.is_empty()
    {
        return Err(RunError::Validation(
            "object import refusal evidence mismatch",
        ));
    }
    Ok(Some(refusal.reason))
}

/// Recovery coordinates for a lane whose Git import failed. The objects stay
/// private: their presence is not a certificate that they can be published.
pub struct RetainedGit {
    pub head: Option<provenant_core::GitOid>,
    pub objects: Option<PathBuf>,
}

/// Return the base pinned by writer settlement, when its evidence is available.
pub fn writer_base(
    root: &std::path::Path,
    record: &AttemptRecord,
) -> Result<Option<provenant_core::GitOid>> {
    let attempt = Directory::open_anchor(&root.join(format!("attempts/{}", record.attempt_id)))?;
    crate::settlement::pinned_base(&attempt, RunStore::scope(record))
}

/// Read retained recovery coordinates without attempting Git import or cleanup.
pub fn retained_git(root: &std::path::Path, record: &AttemptRecord) -> Result<RetainedGit> {
    let attempt = Directory::open_anchor(&root.join(format!("attempts/{}", record.attempt_id)))?;
    let head = crate::settlement::pinned_head(&attempt, RunStore::scope(record))
        .ok()
        .flatten()
        .or(record.head);
    let objects = crate::objects::bound_source(&attempt)
        .ok()
        .flatten()
        .and_then(|source| source.objects.current_path().ok());
    Ok(RetainedGit { head, objects })
}

impl provenant_supervisor::Holder for RunHolder<'_> {
    fn admit(&self, context: &[u8], attempt: &Directory) -> custody::Result<()> {
        let (context, store) =
            HolderContext::open_with_clock(context, self.clock).map_err(custody_error)?;
        store
            .admit(
                context.attempt_id,
                attempt,
                &LaneResources {
                    resources: self.resources,
                    context: &context,
                },
                self.clock,
                &CancelToken::new(),
            )
            .map(drop)
            .map_err(custody_error)
    }
    fn provider_exited(&self, raw: &[u8], _reason: Option<StopReason>) -> custody::Result<()> {
        let (context, store) =
            HolderContext::open_with_clock(raw, self.clock).map_err(custody_error)?;
        let cancel = CancelToken::new();
        store
            .write(&call(self.clock, &cancel).map_err(custody_error)?, |db| {
                db.execute(
                    "DELETE FROM admission WHERE attempt=?1",
                    [context.attempt_id.to_string()],
                )?;
                Ok(())
            })
            .map_err(custody_error)
    }
    fn budget_warning(&self, raw: &[u8], tokens: u64, budget: u64) -> custody::Result<()> {
        let (context, store) =
            HolderContext::open_with_clock(raw, self.clock).map_err(custody_error)?;
        let cancel = CancelToken::new();
        let record = store
            .record(
                context.attempt_id,
                &call(self.clock, &cancel).map_err(custody_error)?,
            )
            .map_err(custody_error)?;
        let attempt = store
            .attempt_dir(context.attempt_id, false)
            .map_err(custody_error)?;
        let provider = record
            .applied_route
            .as_ref()
            .and_then(|r| r.provider.as_deref());
        record_budget_warning(&attempt, context.attempt_id, provider, tokens, budget)
            .map_err(custody_error)
    }
    fn budget_clean_point(&self, raw: &[u8]) -> custody::Result<bool> {
        let (context, store) =
            HolderContext::open_with_clock(raw, self.clock).map_err(custody_error)?;
        let cancel = CancelToken::new();
        let call = call(self.clock, &cancel).map_err(custody_error)?;
        let record = store
            .record(context.attempt_id, &call)
            .map_err(custody_error)?;
        let request = store
            .request(record.attempt_id, &call)
            .map_err(custody_error)?;
        let Some(writer) = request.task.writer.as_ref() else {
            return Ok(false);
        };
        let attempt = store
            .attempt_dir(record.attempt_id, false)
            .map_err(custody_error)?;
        let clean = crate::prepare_writer_with_facts(
            writer,
            &store.writer_registry,
            record.project_id,
            &store.repo_facts,
        )
        .and_then(|writer| writer.budget_clean_point(&attempt))
        .unwrap_or(false);
        if clean {
            crate::artifacts::seal(
                &attempt,
                provenant_records::AttemptFile::BudgetCleanPoint.name(),
                b"{\"schema_version\":1,\"clean\":true}",
                RECORD_LIMIT,
            )
            .map_err(custody_error)?;
        }
        Ok(clean)
    }
    fn budget_crossed(&self, raw: &[u8], tokens: u64, budget: u64) -> custody::Result<()> {
        let (context, store) =
            HolderContext::open_with_clock(raw, self.clock).map_err(custody_error)?;
        let attempt = store
            .attempt_dir(context.attempt_id, false)
            .map_err(custody_error)?;
        crate::artifacts::seal(
            &attempt,
            provenant_records::AttemptFile::BudgetOverrun.name(),
            &crate::db::bytes(
                &serde_json::json!({"schema_version":1,"tokens":tokens,"budget":budget}),
            )
            .map_err(custody_error)?,
            RECORD_LIMIT,
        )
        .map_err(custody_error)
    }
    fn terminal(
        &self,
        context: &[u8],
        attempt: &Directory,
        exit: &ExitObservation,
        recovery: &custody::Recovery,
    ) -> custody::Result<()> {
        let (context, store) =
            HolderContext::open_with_clock(context, self.clock).map_err(custody_error)?;
        let cancel = CancelToken::new();
        let finished = store
            .finish_in_custody(
                context.attempt_id,
                attempt,
                exit,
                Completion::Live {
                    custody: recovery,
                    delivery: context.host_delivery.as_ref(),
                    sealed: context.sealed.as_ref().map(|sealed| &sealed.request_digest),
                    holder_context: &context,
                },
                self.providers,
                &call(self.clock, &cancel).map_err(custody_error)?,
            )
            .map_err(custody_error)?;
        if !finished.release_permitted {
            return Err(custody_error(RunError::Contention));
        }
        if object_import_refused(attempt, exit.scope)
            .map_err(custody_error)?
            .is_none()
        {
            crate::writer::settle_launch_staging(attempt).map_err(custody_error)?;
        }
        release_and_reclaim(attempt, exit.scope, recovery)?;
        // Build output is surveyed by admission and GC, outside settlement.
        Ok(())
    }
}

// Use the same capability matrix as the existing steer path. No current
// lane adapter accepts a live steer; never claim that a queued warning reached it.
fn record_budget_warning(
    attempt: &Directory,
    id: AttemptId,
    provider: Option<&str>,
    tokens: u64,
    budget: u64,
) -> Result<()> {
    use provenant_records::control::*;
    let support = provenant_records::ADAPTER_SPECS
        .iter()
        .find(|spec| Some(spec.name) == provider)
        .map_or(ControlSupport::Unsupported, |spec| {
            spec.launch.control.for_launch(LaunchKind::Lane)
        });
    let receipt = ControlReceipt {
        extra: Default::default(),
        operation_id: provenant_core::OperationId::parse("token-budget-warning")
            .map_err(|_| RunError::Validation("budget warning operation"))?,
        control: ControlKind::Steer,
        target: id.to_string(),
        launch: LaunchKind::Lane,
        support,
        transport: None,
        state: ControlState::Unsupported,
        reason: support.reason().into(),
        message_id: None,
    };
    crate::artifacts::seal(
        attempt,
        "budget-warning.json",
        &crate::db::bytes(&serde_json::json!({
            "schema_version": 1, "tokens": tokens, "budget": budget,
            "text": format!("Token budget warning: {tokens} of {budget} fresh tokens used. Commit your work before the budget stop."),
            "receipt": receipt,
        }))?,
        RECORD_LIMIT,
    )
}

pub(crate) fn release_and_reclaim(
    attempt: &Directory,
    scope: custody::Scope,
    _recovery: &custody::Recovery,
) -> custody::Result<()> {
    custody::publish_release(attempt, scope)?;
    crate::settlement::complete(attempt, scope).map_err(custody_error)?;
    // Retention GC reclaims terminal temporary payloads under writer exclusion.
    Ok(())
}

impl RunStore {
    pub(crate) fn finish_recovered_terminal(
        &self,
        id: AttemptId,
        attempt: &Directory,
        exit: &ExitObservation,
        mut recovery: custody::Recovery,
        providers: &dyn Providers,
        call: &Request<'_>,
    ) -> Result<bool> {
        recovery
            .prepare_settlement()
            .map_err(|error| RunError::Io(std::io::Error::other(error.to_string())))?;
        self.settle_failed_import(&self.record(id, call)?, attempt, &recovery, call)?;
        let finished = self.finish_in_custody(
            id,
            attempt,
            exit,
            Completion::Recovered(Some(&recovery)),
            providers,
            call,
        )?;
        if finished.release_permitted {
            self.settle_recovered(attempt, recovery)?;
        }
        Ok(finished.release_permitted)
    }
    /// Commit the terminal record for an exited attempt from supervisor's
    /// durable exit and the sealed stdout capture. Idempotent.
    pub fn finish(
        &self,
        id: AttemptId,
        attempt: &Directory,
        exit: &ExitObservation,
        providers: &dyn Providers,
        call: &Request<'_>,
    ) -> Result<AttemptRecord> {
        let record = self.record(id, call)?;
        let request = self.request(record.attempt_id, call)?;
        let task = request.task;
        let mut recovery = if let Some(writer) = &task.writer {
            let directory = self.writer_custody_directory(&record, writer)?;
            self.recover_writer_exit(directory, Self::scope(&record), call)?
        } else {
            None
        };
        if let Some(recovery) = &mut recovery {
            recovery
                .prepare_settlement()
                .map_err(|error| RunError::Io(std::io::Error::other(error.to_string())))?;
        }
        let finished = self.finish_in_custody(
            id,
            attempt,
            exit,
            Completion::Recovered(recovery.as_ref()),
            providers,
            call,
        )?;
        if !finished.release_permitted {
            return Ok(finished.record);
        }
        if let Some(recovery) = recovery {
            self.settle_recovered(attempt, recovery)?;
        } else if object_import_refused(attempt, exit.scope)?.is_none() {
            crate::writer::settle_launch_staging(attempt)?;
        }
        Ok(finished.record)
    }
    fn recover_writer_exit(
        &self,
        directory: Directory,
        scope: custody::Scope,
        call: &Request<'_>,
    ) -> Result<Option<custody::Recovery>> {
        match custody::acquire_recovery(
            directory,
            scope,
            call.lock_budget(),
            call.cancel,
            call.clock,
        ) {
            Ok(recovery) => Ok(Some(recovery)),
            Err(error) => {
                if let CustodyError::Filesystem(filesystem) = &error
                    && filesystem.is_not_found()
                {
                    // The guardian can commit and release custody after finish
                    // reads the attempt but before recovery acquires the lock.
                    let current = self.record(scope.attempt, call)?;
                    if Self::scope(&current) == scope
                        && matches!(current.state, AttemptState::Terminal { .. })
                    {
                        return Ok(None);
                    }
                }
                Err(RunError::Io(std::io::Error::other(error.to_string())))
            }
        }
    }
    /// Release a landed worktree's retained writer reservation only after its
    /// canonical terminal transaction and recorded writer binding are verified.
    /// This never stops a process or settles a nonterminal attempt.
    pub fn settle_landed_writer(
        &self,
        recovery: custody::Recovery,
        call: &Request<'_>,
    ) -> Result<()> {
        let scope = recovery.scope();
        let record = self.record(scope.attempt, call)?;
        if record.project_id != self.context.project_id || Self::scope(&record) != scope {
            return Err(RunError::Validation("land writer custody scope mismatch"));
        }
        if !matches!(record.state, AttemptState::Terminal { .. }) {
            return Err(RunError::Nonterminal);
        }
        let request = self.request(record.attempt_id, call)?;
        let writer = request
            .task
            .writer
            .as_ref()
            .ok_or(RunError::Validation("land writer request missing"))?;
        let launch = crate::prepare_writer_with_facts(
            writer,
            &self.writer_registry,
            record.project_id,
            &self.repo_facts,
        )?;
        recovery
            .writer_base(
                launch.worktree.repo().root_dir(),
                launch.worktree.repo().private_dir(),
                launch.branch.as_str(),
            )
            .map_err(|error| RunError::Io(std::io::Error::other(error.to_string())))?;
        if recovery.observe() != custody::RecoveryState::Absent {
            return Err(RunError::Validation("land writer custody is not absent"));
        }
        let attempt = self.attempt_dir(record.attempt_id, false)?;
        self.settle_recovered(&attempt, recovery)
    }

    pub(crate) fn settle_recovered(
        &self,
        attempt: &Directory,
        recovery: custody::Recovery,
    ) -> Result<()> {
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel)?;
        let record = self.record(recovery.scope().attempt, &call)?;
        if !self.import_release_permitted(&record, &call)? {
            return Err(RunError::Validation("writer import remains unresolved"));
        }
        if object_import_refused(attempt, recovery.scope())?.is_some() {
            custody::publish_release(attempt, recovery.scope())
                .map_err(|error| RunError::Io(std::io::Error::other(error)))?;
            let commit = recovery
                .verify_settlement(attempt)
                .map_err(|error| RunError::Io(std::io::Error::other(error)))?;
            crate::settlement::complete(attempt, recovery.scope())?;
            let id = recovery.scope().attempt;
            recovery
                .release_preserving_private(commit)
                .map_err(|error| RunError::Io(std::io::Error::other(error)))?;
            self.record_effect(
                id,
                record.generation,
                SettlementEffect::CleanupCustodyRelease,
                EffectDisposition::Done,
                &call,
            )?;
            // The preserved private Git is evidence, not temporary cleanup debt.
            return self.set_temporary_retained(id, false, &call);
        }
        crate::settlement::complete(attempt, recovery.scope())?;
        crate::writer::settle_launch_staging(attempt)?;
        custody::publish_release(attempt, recovery.scope())
            .map_err(|e| RunError::Io(std::io::Error::other(e.to_string())))?;
        let commit = recovery
            .verify_settlement(attempt)
            .map_err(|e| RunError::Io(std::io::Error::other(e.to_string())))?;
        let mut scratch = provenant_os::scratch::Scratch::new(attempt.try_clone()?)?;
        recovery
            .settle(commit, &mut scratch)
            .map_err(|e| RunError::Io(std::io::Error::other(e.to_string())))
    }
    fn finish_effects(
        &self,
        record: AttemptRecord,
        attempt: &Directory,
        completion: &Completion<'_>,
        call: &Request<'_>,
    ) -> Result<Finished> {
        if !self.import_release_permitted(&record, call)? {
            let Some(recovery) = completion.custody() else {
                return Ok(Finished {
                    record,
                    release_permitted: false,
                });
            };
            let mut driver = match crate::settlement::Guard::claim(
                attempt,
                Self::scope(&record),
                call.clock,
            ) {
                Ok(Some(driver)) => driver,
                Ok(None) => {
                    return Ok(Finished {
                        record,
                        release_permitted: false,
                    });
                }
                Err(error @ RunError::ObjectImportLimit(_)) => {
                    let reason = format!(
                        "settlement exhausted three attempts; private Git retained for host recovery: {error}"
                    );
                    self.refuse_pending_import(&record, attempt, &reason, call)?;
                    return Ok(Finished {
                        record: self.record(record.attempt_id, call)?,
                        release_permitted: true,
                    });
                }
                Err(error) => return Err(error),
            };
            let request = self.request(record.attempt_id, call)?;
            let task = request.task;
            let writer = task
                .writer
                .as_ref()
                .ok_or(RunError::Validation("pending import lacks writer"))?;
            let cancel = driver.cancellation();
            let budget_stop = record
                .extra
                .get("terminal_cause")
                .and_then(|value| serde_json::from_value::<TerminalCause>(value.clone()).ok())
                == Some(TerminalCause::TokenBudget);
            let imported = crate::prepare_writer_with_facts(
                writer,
                &self.writer_registry,
                record.project_id,
                &self.repo_facts,
            )
            .and_then(|launch| {
                crate::settlement::with_cancellation(attempt, Self::scope(&record), &cancel, || {
                    launch.terminal(
                        self.context.state_root.as_path(),
                        attempt,
                        recovery,
                        &mut driver,
                        budget_stop,
                    )
                })
            });
            if let Err(error) = &imported {
                driver.record_failure(&error.to_string())?;
            }
            match imported {
                Ok(_) => {
                    self.record_effect(
                        record.attempt_id,
                        record.generation,
                        SettlementEffect::GitImport,
                        EffectDisposition::Done,
                        call,
                    )?;
                }
                Err(error) if driver.exhausted() => {
                    let reason = format!(
                        "settlement exhausted three attempts; private Git retained for host recovery: {error}"
                    );
                    self.refuse_pending_import(&record, attempt, &reason, call)?;
                }
                // The same precondition refusal the first attempt makes: no
                // retry can change it without host recovery.
                Err(error @ (RunError::Writer(_) | RunError::WriterProjectMismatch { .. })) => {
                    let reason = format!(
                        "writer preconditions refused; private Git retained for host recovery: {error}"
                    );
                    self.refuse_pending_import(&record, attempt, &reason, call)?;
                }
                Err(error) => {
                    self.record_effect(
                        record.attempt_id,
                        record.generation,
                        SettlementEffect::GitImport,
                        EffectDisposition::Failed {
                            reason: error.to_string().chars().take(1024).collect(),
                            retryable: true,
                        },
                        call,
                    )?;
                    return Ok(Finished {
                        record: self.record(record.attempt_id, call)?,
                        release_permitted: false,
                    });
                }
            }
        }
        self.retry_terminal_effects(&record, attempt, completion, call)?;
        self.settle_cleanup(
            &self.record(record.attempt_id, call)?,
            completion.custody(),
            call,
        );
        self.prepare_budget_wrap_up(record.attempt_id, call)?;
        Ok(Finished {
            record: self.record(record.attempt_id, call)?,
            release_permitted: true,
        })
    }

    /// Admit exactly one same-session wrap-up before releasing the predecessor.
    /// The ordinary settlement reaction launches this queued run after custody
    /// drops; recovery replays the same operation rather than creating another.
    pub fn prepare_budget_wrap_up(
        &self,
        id: AttemptId,
        call: &Request<'_>,
    ) -> Result<Option<AttemptId>> {
        let record = self.record(id, call)?;
        let is_budget = matches!(&record.state, AttemptState::Terminal { outcome: Outcome::ReviewRequired, extra, .. }
            if extra.get("terminal_cause").and_then(serde_json::Value::as_str) == Some("token_budget"));
        if !is_budget || !soft_budget_wrap_up(&record, &record.usage) {
            return Ok(None);
        }
        let request = self.request(record.attempt_id, call)?;
        let task = request.task;
        if task.writer.is_none() || task.profile.extra.contains_key("budget_wrap_up") {
            return Ok(None);
        }
        let budget = record
            .limits
            .as_ref()
            .and_then(|limits| limits.token_budget.as_ref())
            .ok_or(RunError::Validation("wrap-up budget missing"))?;
        let policy = record
            .limits
            .as_ref()
            .and_then(|limits| limits.budget_policy)
            .unwrap_or_default();
        let remaining = budget_ceiling(budget.value, policy.grace)
            .saturating_sub(crate::token_budget::used(&record.usage).unwrap_or(budget.value));
        if remaining == 0 {
            return Ok(None);
        }
        let digest = provenant_records::CanonicalV1::record_digest("budget-wrap-up", 1, &id)?;
        let mut attempt_bytes = [0; 16];
        attempt_bytes.copy_from_slice(&digest.as_bytes()[16..]);
        let attempt = self.attempt_dir(id, false)?;
        if read_sealed(
            &attempt,
            provenant_records::AttemptFile::BudgetWrapUpRefused.name(),
            RECORD_LIMIT,
        )?
        .is_some()
        {
            return Ok(None);
        }
        let operation_id = provenant_core::OperationId::parse(&format!("budget-wrap-up-{id}"))
            .map_err(|_| RunError::Validation("wrap-up operation"))?;
        let refused = |reason: &str| -> Result<Option<AttemptId>> {
            crate::artifacts::seal(
                &attempt,
                provenant_records::AttemptFile::BudgetWrapUpRefused.name(),
                &crate::db::bytes(&serde_json::json!({"schema_version":1,"reason":reason}))?,
                RECORD_LIMIT,
            )?;
            Ok(None)
        };
        if self
            .operation(&operation_id, call)?
            .is_some_and(|operation| operation.state == OperationState::Refused)
        {
            return refused(
                "budget wrap-up admission was refused; the lane remains stopped for the chair",
            );
        }
        let input = crate::Resume {
            prior: id,
            expected_generation: 0,
            operation_id,
            attempt_id: AttemptId::from_bytes(attempt_bytes),
            prompt: format!(
                "The writer token target has been crossed. You have {remaining} fresh tokens left in the grace allowance. Finish at a clean point now: commit your work, push your branch, and return the final handoff. Continue this same task and provider session. This is the only budget wrap-up continuation; the remaining allowance is a hard stop."
            ),
            created_at: match record.state {
                AttemptState::Terminal { finished_at, .. } => finished_at,
                _ => return Ok(None),
            },
            extend: Vec::new(),
        };
        match self.resume_budget_wrap_up(&input, remaining, call) {
            Ok(run) => Ok(Some(run.attempt_id)),
            Err(
                error @ (RunError::RecordedRefusal(_)
                | RunError::ContinuationRefused { .. }
                | RunError::ProviderStateExpired
                | RunError::WriterLineage(_)
                | RunError::GenerationConflict { .. }),
            ) => refused(&error.to_string()),
            Err(error) => Err(error),
        }
    }

    /// Reconcile a failed import as a final failure once custody proves that
    /// the guardian is gone. Never rewrite its sealed terminal decision.
    pub(crate) fn settle_failed_import(
        &self,
        record: &AttemptRecord,
        attempt: &Directory,
        recovery: &custody::Recovery,
        call: &Request<'_>,
    ) -> Result<()> {
        let Some(receipts) = &record.settlement_receipts else {
            return Ok(());
        };
        let EffectDisposition::Failed { reason, retryable } = &receipts.git_import else {
            return Ok(());
        };
        if !retryable && object_import_refused(attempt, Self::scope(record))?.is_none() {
            return Ok(());
        }
        if recovery.scope() != Self::scope(record)
            || recovery.observe() != custody::RecoveryState::Absent
        {
            return Err(RunError::Validation(
                "failed import requires absent writer custody",
            ));
        }
        // Exclude an active settlement driver even if it has dropped custody.
        let _driver = attempt.try_lock(
            std::ffi::OsStr::new("settlement-driver.lock"),
            provenant_os::fs::LockMode::Exclusive,
        )?;
        self.refuse_pending_import(record, attempt, reason, call)?;
        // Refusal finishes automatic settlement. Keep any distinct manifest or
        // delivery failure as a final receipt, rather than retrying it forever.
        let manifest = match &receipts.manifest_conflict {
            EffectDisposition::Pending => EffectDisposition::Done,
            EffectDisposition::Failed { reason, .. } => EffectDisposition::Failed {
                reason: reason.clone(),
                retryable: false,
            },
            disposition => disposition.clone(),
        };
        self.record_effect(
            record.attempt_id,
            record.generation,
            SettlementEffect::ManifestConflict,
            manifest,
            call,
        )?;
        let delivery = match &receipts.delivery {
            EffectDisposition::Pending => EffectDisposition::Failed {
                reason: "delivery withheld because lane Git import failed".into(),
                retryable: false,
            },
            EffectDisposition::Failed { reason, .. } => EffectDisposition::Failed {
                reason: reason.clone(),
                retryable: false,
            },
            EffectDisposition::Cancelled { reason, .. } => EffectDisposition::Cancelled {
                reason: reason.clone(),
                retryable: false,
            },
            disposition => disposition.clone(),
        };
        self.record_effect(
            record.attempt_id,
            record.generation,
            SettlementEffect::Delivery,
            delivery,
            call,
        )?;
        Ok(())
    }

    /// Settlement could not publish the lane's commits: leave the bound
    /// worktree at the branch the host holds, and name where the work is.
    fn unpublish_worktree(
        &self,
        attempt: &Directory,
        writer: &crate::WriterRequest,
        project: provenant_core::ProjectId,
    ) -> String {
        crate::prepare_writer_with_facts(writer, &self.writer_registry, project, &self.repo_facts)
            .map(|launch| launch.unpublish(attempt))
            .unwrap_or_else(|error| format!("worktree files not restored: {error}"))
    }

    fn refuse_pending_import(
        &self,
        record: &AttemptRecord,
        attempt: &Directory,
        reason: &str,
        call: &Request<'_>,
    ) -> Result<()> {
        let request = self.request(record.attempt_id, call)?;
        let writer = request
            .task
            .writer
            .as_ref()
            .ok_or(RunError::Validation("pending import lacks writer"))?;
        let reason = match object_import_refused(attempt, Self::scope(record))? {
            Some(reason) => reason,
            None => {
                let note = self.unpublish_worktree(attempt, writer, record.project_id);
                if note.is_empty() {
                    reason.to_owned()
                } else {
                    format!("{reason}; {note}")
                }
            }
        };
        seal_import_refusal(attempt, Self::scope(record), &writer.worktree, &reason)?;
        self.record_effect(
            record.attempt_id,
            record.generation,
            SettlementEffect::GitImport,
            EffectDisposition::Failed {
                reason: reason.chars().take(1024).collect(),
                retryable: false,
            },
            call,
        )?;
        let archive = crate::artifacts::subdir(attempt, std::ffi::OsStr::new("quarantine"), true)?;
        if read_sealed(&archive, "manifest.json", RECORD_LIMIT)?.is_none() {
            seal_manifest(
                &archive,
                "manifest.json",
                &crate::db::bytes(&serde_json::json!({
                    "schema_version":1,"scope":Self::scope(record),"kind":"quarantine_manifest","disposition":"import_refused"
                }))?,
                RECORD_LIMIT,
            )?;
        }
        Ok(())
    }

    fn retry_terminal_effects(
        &self,
        record: &AttemptRecord,
        attempt: &Directory,
        completion: &Completion<'_>,
        call: &Request<'_>,
    ) -> Result<()> {
        // Another driver is already settling these effects; it is not a failure.
        let _effects = match attempt.try_lock(
            std::ffi::OsStr::new("terminal-effects.lock"),
            provenant_os::fs::LockMode::Exclusive,
        ) {
            Ok(lock) => lock,
            Err(provenant_os::fs::FsError::LockTimeout { .. }) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let current = self.record(record.attempt_id, call)?;
        let Some(receipts) = current.settlement_receipts else {
            return Ok(());
        };
        if matches!(
            receipts.manifest_conflict,
            EffectDisposition::Pending
                | EffectDisposition::Failed {
                    retryable: true,
                    ..
                }
        ) {
            let manifest = (|| -> Result<()> {
                let archive =
                    crate::artifacts::subdir(attempt, std::ffi::OsStr::new("quarantine"), true)?;
                let intended = read_sealed(&archive, "manifest-intended.json", RECORD_LIMIT)?
                    .ok_or(RunError::Validation(
                        "manifest disposition remains unresolved",
                    ))?;
                let value: serde_json::Value = crate::db::decode(&intended)?;
                if value["scope"]
                    != serde_json::to_value(Self::scope(record))
                        .map_err(|_| RunError::Validation("manifest scope"))?
                {
                    return Err(RunError::Conflict);
                }
                crate::artifacts::seal(&archive, "manifest.json", &intended, RECORD_LIMIT)
            })();
            let disposition = match manifest {
                Ok(()) => EffectDisposition::Done,
                Err(error) => EffectDisposition::Failed {
                    reason: error.to_string().chars().take(1024).collect(),
                    retryable: true,
                },
            };
            self.record_effect(
                record.attempt_id,
                record.generation,
                SettlementEffect::ManifestConflict,
                disposition,
                call,
            )?;
        }
        if !matches!(
            receipts.delivery,
            EffectDisposition::Done
                | EffectDisposition::Failed {
                    retryable: false,
                    ..
                }
        ) {
            if !Self::delivery_outcome(record) {
                self.record_effect(
                    record.attempt_id,
                    record.generation,
                    SettlementEffect::Delivery,
                    EffectDisposition::Done,
                    call,
                )?;
                return Ok(());
            }
            let request = self.request(record.attempt_id, call)?;
            let task = request.task;
            if let (Some(writer), Some(delivery)) = (&task.writer, &task.deliver) {
                if self
                    .delivered(record.attempt_id, call)?
                    .is_some_and(|(request, receipt)| {
                        request == *delivery
                            && receipt.head == record.head
                            && matches!(receipt.state, WriterDeliveryState::Delivered { .. })
                    })
                {
                    self.record_effect(
                        record.attempt_id,
                        record.generation,
                        SettlementEffect::Delivery,
                        EffectDisposition::Done,
                        call,
                    )?;
                    return Ok(());
                }
                // Without custody, a later writer may own the worktree. Wait
                // for its writer lock rather than deliver beneath it.
                let slot = if completion.custody().is_none() {
                    self.writer_custody_directory(record, writer).ok()
                } else {
                    None
                };
                let observation = slot
                    .as_ref()
                    .map(provenant_supervisor::custody::writer_admission_guard)
                    .transpose()
                    .map_err(|error| RunError::Io(std::io::Error::other(error.to_string())))?;
                let _writer = match slot.as_ref().map(|slot| {
                    slot.try_lock(
                        std::ffi::OsStr::new("writer.lock"),
                        provenant_os::fs::LockMode::Exclusive,
                    )
                }) {
                    Some(Ok(lock)) => Some(lock),
                    Some(Err(provenant_os::fs::FsError::LockTimeout { .. })) => return Ok(()),
                    Some(Err(error)) => return Err(error.into()),
                    None => None,
                };
                drop(observation);
                let cancel = CancelToken::new();
                let host = match completion {
                    Completion::Live {
                        delivery: Some(host),
                        ..
                    } => Ok((*host).clone()),
                    _ => {
                        let host = crate::HostDeliveryContext::capture()
                            .with_repo_facts(self.repo_facts());
                        let evidence = ["delivery.json", "delivery-cancelled.json"]
                            .iter()
                            .try_fold(false, |found, name| -> Result<bool> {
                                Ok(found || read_sealed(attempt, name, RECORD_LIMIT)?.is_some())
                            })?;
                        if evidence {
                            Ok(host)
                        } else {
                            record
                                .head
                                .ok_or(RunError::Validation("sealed delivery head missing"))
                                .and_then(|head| {
                                    let base = crate::settlement::pinned_base(
                                        attempt,
                                        Self::scope(record),
                                    )?
                                    .ok_or(RunError::Validation("sealed delivery base missing"))?;
                                    host.prepare_recovery(
                                        writer,
                                        delivery,
                                        attempt,
                                        Self::scope(record),
                                        head,
                                        base,
                                    )
                                })
                        }
                    }
                };
                let result = host.and_then(|host| {
                    crate::settlement::with_cancellation(
                        attempt,
                        Self::scope(record),
                        &cancel,
                        || {
                            crate::delivery::deliver_once(
                                &host.with_repo_facts(self.repo_facts()).cancellable(&cancel),
                                writer,
                                delivery,
                                attempt,
                                &format!(
                                    "Run {}\nAttempt {}",
                                    record.attempt_id, record.attempt_id
                                ),
                            )
                        },
                    )
                });
                if let Ok(receipt) = result.as_ref() {
                    self.record_delivered_ref(record, delivery, receipt, call);
                }
                let disposition = match result {
                    Ok(receipt)
                        if matches!(
                            receipt.state,
                            WriterDeliveryState::Delivered { .. }
                                | WriterDeliveryState::NoNewCommits { .. }
                        ) =>
                    {
                        EffectDisposition::Done
                    }
                    Ok(_)
                        if read_sealed(attempt, "delivery-cancelled.json", RECORD_LIMIT)?
                            .is_some() =>
                    {
                        self.record_cancellation(
                            record.attempt_id,
                            record.generation,
                            CancellationDisposition::DeliveryEffective,
                            call,
                        )?;
                        EffectDisposition::Cancelled {
                            reason: delivery_cancellation_reason(
                                read_sealed(attempt, "cancel.json", RECORD_LIMIT)?.as_deref(),
                            ),
                            retryable: true,
                        }
                    }
                    // A sealed result is definite; automatic recovery will not
                    // repeat it, and explicit pickup remains available.
                    Ok(WriterDeliveryReceipt {
                        state: WriterDeliveryState::Failed { stage, .. },
                        ..
                    }) if stage != WriterDeliveryStage::Interrupted => EffectDisposition::Failed {
                        reason: format!("delivery failed at the {stage:?} stage"),
                        retryable: false,
                    },
                    // A start marker without a result: the remote effect is unknown.
                    Ok(_) => EffectDisposition::Failed {
                        reason: "delivery outcome is uncertain; explicit host pickup required"
                            .into(),
                        retryable: false,
                    },
                    Err(error) => EffectDisposition::Failed {
                        reason: error.to_string().chars().take(1024).collect(),
                        retryable: !crate::delivery::recovery_refused(&error),
                    },
                };
                self.record_effect(
                    record.attempt_id,
                    record.generation,
                    SettlementEffect::Delivery,
                    disposition,
                    call,
                )?;
            } else {
                self.record_effect(
                    record.attempt_id,
                    record.generation,
                    SettlementEffect::Delivery,
                    EffectDisposition::Done,
                    call,
                )?;
            }
        }
        Ok(())
    }

    pub(crate) fn reconcile_terminal_effects(
        &self,
        record: &AttemptRecord,
        call: &Request<'_>,
    ) -> Result<()> {
        let attempt = self.attempt_dir(record.attempt_id, false)?;
        self.retry_terminal_effects(record, &attempt, &Completion::Recovered(None), call)
    }

    fn finish_in_custody(
        &self,
        id: AttemptId,
        attempt: &Directory,
        exit: &ExitObservation,
        completion: Completion<'_>,
        providers: &dyn Providers,
        call: &Request<'_>,
    ) -> Result<Finished> {
        let recovery = completion.custody();
        let record = self.record(id, call)?;
        if exit.scope != Self::scope(&record)
            || recovery.is_some_and(|recovery| recovery.scope() != exit.scope)
        {
            return Err(RunError::Validation("terminal custody scope mismatch"));
        }
        if matches!(record.state, AttemptState::Terminal { .. }) {
            return self.finish_effects(record, attempt, &completion, call);
        }
        // Upgraded guardians may not have called provider_exited. Capacity is
        // independent of writer custody and is free as soon as exit is known.
        self.write(call, |tx| {
            tx.execute("DELETE FROM admission WHERE attempt=?1", [id.to_string()])?;
            Ok(())
        })?;
        if let Some(raw) = read_sealed(attempt, "terminal-pending.json", TERMINAL_RECORD_LIMIT)? {
            let pending = crate::db::decode_attempt(&raw)?;
            let record = if pending.exit_evidence.is_some() {
                self.recover_terminal(id, exit, call)
            } else {
                self.replay_terminal(id, call)
            }?;
            return self.finish_effects(record, attempt, &completion, call);
        }
        let mut settlement = match crate::settlement::Guard::claim(attempt, exit.scope, call.clock)
        {
            Ok(Some(driver)) => Some(driver),
            Ok(None) => return Err(RunError::Contention),
            Err(RunError::ObjectImportLimit(_)) => None,
            Err(error) => return Err(error),
        };
        let finished_at = call
            .clock
            .wall_now()
            .map_err(|_| RunError::Validation("wall clock"))?;
        // Seal these dispositions alongside the intent; the terminal transaction
        // and its replay create the receipts together with the decision.
        let mut deferred: Vec<(SettlementEffect, EffectDisposition)> = Vec::new();
        let mut import_failed = false;
        let mut import_stopped = false;
        // A late request alone cannot invalidate a verified clean exit. If the
        // current guardian could not establish ordering, its receipt stays unknown.
        let mut cancelled = if exit.never_spawned {
            exit.stop_reason == Some(StopReason::ExplicitCancellation)
        } else {
            exit.cancel_delivered_before_exit == Some(true)
        };
        let provider_state = self
            .context
            .state_root
            .as_path()
            .join("attempts")
            .join(id.to_string())
            .join("provider-state");
        let mut decoded = match (&record.applied_route, exit.never_spawned) {
            // Cancelled before spawn; keep the guardian's own reason too.
            (_, true) if cancelled => Decoded {
                terminal_cause: None,
                outcome: Outcome::Cancelled,
                result: String::new(),
                complete: true,
                observed_route: None,
                usage: UsageRecord::default(),
                session: None,
                partial_signals: Vec::new(),
                turns: Vec::new(),
                reason: Some(format!(
                    "cancelled before start: {}",
                    exit.warnings
                        .first()
                        .map_or("provider never spawned", String::as_str)
                )),
            },
            (_, true) => Decoded {
                terminal_cause: Some(TerminalCause::SpawnFailure),
                outcome: Outcome::Rejected,
                result: String::new(),
                complete: true,
                observed_route: None,
                usage: UsageRecord::default(),
                session: None,
                partial_signals: Vec::new(),
                turns: Vec::new(),
                reason: Some(
                    exit.warnings
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "provider never spawned".into()),
                ),
            },
            (Some(route), false) => {
                let stdout = read_sealed(attempt, "stdout.log", CAPTURE_LIMIT)?.unwrap_or_default();
                let stderr = read_sealed(attempt, "stderr.log", CAPTURE_LIMIT)
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                providers
                    .decode_captures(
                        route,
                        &stdout,
                        &stderr,
                        exit,
                        &provider_state,
                        completion.holder_context(),
                    )
                    .map_err(RunError::ProviderEvidence)?
            }
            (None, false) => return Err(RunError::Validation("running attempt lacks a route")),
        };
        if record
            .applied_route
            .as_ref()
            .and_then(|r| r.provider.as_deref())
            == Some("codex")
        {
            let prior_id = self.request(record.attempt_id, call).map(|request| {
                if request.fresh_continuation.is_none() {
                    request.parent
                } else {
                    None
                }
            });
            let prior = prior_id.and_then(|id| id.map(|id| self.record(id, call)).transpose());
            let baseline_unknown = prior.is_err();
            let prior = prior.ok().flatten();
            codex_attempt_usage(
                &mut decoded.usage,
                decoded.session.as_deref(),
                prior.as_ref().map(|p| (p.session.as_deref(), &p.usage)),
                baseline_unknown,
            );
        }
        if !cancelled && matches!(exit.termination, Some(Termination::Signal(_))) {
            decoded.terminal_cause = Some(TerminalCause::ProcessCrash);
        }
        // Provider-owned session transcripts record tool output, so a printed
        // credential can survive there after capture and result filtering.
        // Scrub before publication so a retry never starts from new plaintext.
        let session_redaction_failed =
            provenant_confinement::redact::scrub_session_history(&provider_state).is_err();
        let mut handoff = self.capture_handoff(attempt, &record, &decoded)?;
        // Failed headless tools can leave only stderr. Keep the recognised
        // provider diagnostic alongside any partial answer before sealing it.
        if decoded.outcome == Outcome::Failed
            && let Some(route) = &record.applied_route
            && let Some(note) = providers.failure_diagnostic(
                route,
                // A diagnostic is best effort: an unreadable stderr must not stop sealing.
                &read_sealed(attempt, "stderr.log", CAPTURE_LIMIT)
                    .ok()
                    .flatten()
                    .unwrap_or_default(),
            )
        {
            if !decoded.result.is_empty() {
                decoded.result.push_str("\n\n");
            }
            decoded.result.push_str(&note);
            decoded.result.push('\n');
            // The provider's note is captured output, not an approved safe fact.
            // Keep it in the explicit result, but do not project it as a reason.
            const FAILURE_NOTE: &str =
                "provider reported a failure; inspect the captured output for details";
            decoded.reason = Some(match decoded.reason.take() {
                Some(reason) => format!("{reason}; {FAILURE_NOTE}"),
                None => FAILURE_NOTE.into(),
            });
        }
        // A provider can report a tool denial even when the provider itself
        // succeeds. Captures and the decoded answer are evidence, not proof
        // of which sandbox rule or filesystem permission caused the error.
        let denial = path_denial_hint(decoded.result.as_bytes()).or_else(|| {
            ["stdout.log", "stderr.log"].iter().find_map(|name| {
                read_sealed(attempt, name, CAPTURE_LIMIT)
                    .ok()
                    .flatten()
                    .and_then(|capture| path_denial_hint(&capture))
            })
        });
        let clean_budget = exit.stop_reason == Some(StopReason::TokenBudget)
            && read_sealed(
                attempt,
                provenant_records::AttemptFile::BudgetCleanPoint.name(),
                RECORD_LIMIT,
            )?
            .is_some();
        if clean_budget && !decoded.complete {
            // Use the lane's recorded outcome, including a done progress
            // handoff. With no completed report, ordinary review is required;
            // a clean boundary must not invent success or impose a timeout.
            let done = handoff.as_ref().is_some_and(|handoff| {
                handoff.status == provenant_records::handoff::HandoffStatus::Done
            });
            decoded.outcome = if done {
                Outcome::Succeeded
            } else {
                Outcome::ReviewRequired
            };
            decoded.terminal_cause = None;
            if decoded.result.trim().is_empty()
                && let Some(handoff) = &handoff
            {
                decoded.result = handoff.fenced();
            }
            if decoded.result.trim().is_empty() {
                decoded.result = "Token target reached at a clean pushed point; inspect the retained work before accepting completion.".into();
            }
            // The host has sealed a complete review report even when the
            // interrupted provider did not finish its own report.
            decoded.complete = true;
            decoded.reason = Some("token target reached at a clean pushed point; retained the lane's handoff without a budget snapshot".into());
        }
        // Valid natural completion wins over a late stop request.
        if decoded.outcome != Outcome::Succeeded
            && !clean_budget
            && !(exit.stop_reason == Some(StopReason::TokenBudget) && decoded.complete)
        {
            match exit.stop_reason {
                Some(
                    StopReason::WallDeadline | StopReason::TokenBudget | StopReason::ContextHandoff,
                ) => decoded.outcome = Outcome::TimedOut,
                Some(StopReason::ExplicitCancellation) if cancelled => {
                    decoded.outcome = Outcome::Cancelled
                }
                _ => {}
            }
        }
        // Recovery could not prove a leftover process was the attempt's, so it
        // was left running. Name it; a clean success keeps its outcome.
        if exit.settlement_evidence == custody::SettlementEvidence::LeftoverNotProven {
            let pids: Vec<String> = exit.unresolved_pids.iter().map(i64::to_string).collect();
            let note = format!(
                "leftover process not proven to belong to this attempt and left running: pid {}",
                pids.join(", ")
            );
            if decoded.outcome != Outcome::Succeeded {
                decoded.outcome = Outcome::Lost;
            }
            decoded.reason = Some(match decoded.reason.take() {
                Some(reason) => format!("{reason}; {note}"),
                None => note,
            });
        }
        let (request, digest) = self.request_with_digest(record.attempt_id, call)?;
        // The run store is writable by a parent lane, so a live terminal must
        // see the request the host launched before it acts on its writer or
        // delivery fields.
        if let Completion::Live {
            sealed: Some(sealed),
            ..
        } = &completion
            && digest != **sealed
        {
            return Err(RunError::Validation("request changed after launch"));
        }
        let task = request.task;
        // A stop at the runtime deadline is a typed timeout, whatever partial
        // output the provider left, and the lane stays resumable.
        let typed_deadline = decoded.outcome == Outcome::TimedOut
            && exit.stop_reason == Some(StopReason::WallDeadline);
        if typed_deadline {
            let limit = format!(
                "runtime deadline exceeded: provider stopped after {}s",
                task.provider_timeout_seconds
            );
            decoded.reason = Some(match decoded.reason.take() {
                Some(detail) if detail != "provider output is partial" => {
                    format!("{limit}; {detail}")
                }
                _ => limit,
            });
        }
        // A writer that passed its token budget was stopped by the host: say
        // so, with the budget and where it came from. Settlement records
        // whether its work could be committed.
        if decoded.outcome == Outcome::TimedOut && exit.stop_reason == Some(StopReason::TokenBudget)
        {
            decoded.terminal_cause = Some(TerminalCause::TokenBudget);
            let budget = record.limits.as_ref().and_then(|l| l.token_budget.as_ref());
            let limit = format!(
                "token budget reached: provider stopped after passing {} tokens (source: {})",
                budget.map_or(0, |budget| budget.value),
                budget.map_or("unrecorded", |budget| budget.source.as_str())
            );
            decoded.reason = Some(match decoded.reason.take() {
                Some(detail) if detail != "provider output is partial" => {
                    format!("{limit}; {detail}")
                }
                _ => limit,
            });
        }
        if decoded.outcome == Outcome::TimedOut
            && exit.stop_reason == Some(StopReason::TokenBudget)
            && soft_budget_wrap_up(&record, &decoded.usage)
        {
            decoded.outcome = Outcome::ReviewRequired;
            let stopped = stopped_handoff(
                handoff.take(),
                "Token target crossed; a same-session budget wrap-up is pending.",
            );
            let blocked = stopped.status == provenant_records::handoff::HandoffStatus::Blocked;
            if blocked {
                decoded.terminal_cause = Some(TerminalCause::Blocked);
            }
            decoded.result = stopped.fenced();
            decoded.complete = true;
            handoff = Some(stopped);
            decoded.reason = Some(if blocked {
                "token budget crossed; the blocked handoff requires an owner decision before continuing".into()
            } else {
                "token budget crossed; continuing this provider session once to commit, push and report within the remaining grace".into()
            });
        }
        if let Some(bytes) = read_sealed(
            attempt,
            provenant_records::AttemptFile::BudgetOverrun.name(),
            RECORD_LIMIT,
        )? {
            let overrun: serde_json::Value = crate::db::decode(&bytes)?;
            let note = format!(
                "token budget crossed: {} fresh tokens, target {}",
                overrun["tokens"], overrun["budget"]
            );
            decoded.reason = Some(match decoded.reason.take() {
                Some(reason) => format!("{reason}; {note}"),
                None => note,
            });
        }
        if let Some(budget) = record
            .limits
            .as_ref()
            .and_then(|limits| limits.token_budget.as_ref())
            .filter(|budget| !budget.enforced())
        {
            let note = format!(
                "token budget of {} is not enforced: usage is not reported while the provider runs",
                budget.value
            );
            decoded.reason = Some(match decoded.reason.take() {
                Some(existing) => format!("{existing}; {note}"),
                None => note,
            });
        }
        if let Some(bytes) = read_sealed(attempt, "budget-warning.json", RECORD_LIMIT)? {
            let warning: serde_json::Value = crate::db::decode(&bytes)?;
            let note = format!(
                "90% budget warning not delivered: {}",
                warning["receipt"]["reason"]
                    .as_str()
                    .unwrap_or("control unsupported")
            );
            decoded.reason = Some(match decoded.reason.take() {
                Some(reason) => format!("{reason}; {note}"),
                None => note,
            });
        }
        if cancelled {
            decoded.outcome = Outcome::Cancelled;
        }
        if let Some(writer) = &task.writer {
            let refusal = object_import_refused(attempt, exit.scope)?;
            let refused = refusal.is_some();
            let check = if cancelled {
                Ok(())
            } else if settlement.is_none() {
                Err(RunError::ObjectImportLimit(
                    "settlement exhausted three attempts; private Git retained for host recovery",
                ))
            } else if let Some(reason) = refusal {
                decoded.outcome = Outcome::Failed;
                decoded.reason = Some(reason);
                Ok(())
            } else {
                crate::prepare_writer_with_facts(
                    writer,
                    &self.writer_registry,
                    record.project_id,
                    &self.repo_facts,
                )
                .and_then(|writer| {
                    let driver = settlement.as_mut().ok_or(RunError::Contention)?;
                    let cancel = driver.cancellation();
                    crate::settlement::with_cancellation(attempt, exit.scope, &cancel, || {
                        writer.terminal(
                            self.context.state_root.as_path(),
                            attempt,
                            recovery
                                .ok_or(RunError::Validation("writer terminal custody missing"))?,
                            driver,
                            exit.stop_reason == Some(StopReason::TokenBudget)
                                && !clean_budget
                                && decoded.outcome == Outcome::TimedOut
                                && !soft_budget_wrap_up(&record, &decoded.usage),
                        )
                    })
                })
            };
            if let Some(bytes) = read_sealed(attempt, "budget-commit.json", RECORD_LIMIT)? {
                let result: serde_json::Value = crate::db::decode(&bytes)?;
                let note = result["note"]
                    .as_str()
                    .ok_or(RunError::Validation("budget commit note missing"))?;
                decoded.reason = Some(match decoded.reason.take() {
                    Some(reason) => format!("{reason}; {note}"),
                    None => note.to_owned(),
                });
                if decoded.result.trim().is_empty() {
                    decoded.result = budget_stop_result(&result, note);
                    decoded.turns.clear();
                }
            }
            if let Err(error) = &check
                && let Some(driver) = &mut settlement
            {
                driver.record_failure(&error.to_string())?;
            }
            let last_failure = check.as_ref().err().map(ToString::to_string);
            match crate::artifacts::subdir(attempt, std::ffi::OsStr::new("quarantine"), false) {
                Ok(archive) => {
                    let saved = read_sealed(&archive, "manifest.json", RECORD_LIMIT)?;
                    let intended = read_sealed(&archive, "manifest-intended.json", RECORD_LIMIT)?;
                    if saved
                        .as_ref()
                        .zip(intended.as_ref())
                        .is_some_and(|(saved, intended)| saved != intended)
                    {
                        deferred.push((
                            SettlementEffect::ManifestConflict,
                            EffectDisposition::Failed {
                                reason: "manifest conflicts with sealed intended disposition"
                                    .into(),
                                retryable: true,
                            },
                        ));
                    }
                }
                Err(RunError::Filesystem(error)) if error.is_not_found() => {}
                Err(error) => return Err(error),
            }
            // A conflicting manifest is itself the import failure: retention
            // cannot help, and the conflict is a retryable effect of its own.
            let manifest_conflicted = deferred
                .iter()
                .any(|(effect, _)| *effect == SettlementEffect::ManifestConflict);
            let import_stop_requested = read_sealed(attempt, "cancel.json", RECORD_LIMIT)?
                .and_then(|raw| crate::db::decode::<crate::store::CancellationEvidence>(&raw).ok())
                .is_some_and(|request| {
                    request.requests_stop(
                        exit.scope,
                        record.verified_owner.unwrap_or(self.context.actor_id),
                    )
                });
            let check = match check {
                Err(error) if crate::writer::worktree_removed(writer) || manifest_conflicted => {
                    Err(error)
                }
                Err(RunError::Writer(_) | RunError::WriterProjectMismatch { .. }) => {
                    Err(RunError::ObjectImportLimit(
                        "writer preconditions refused; private Git retained for host recovery",
                    ))
                }
                // An exhausted import cannot progress, so a request to stop is
                // the only way to settle it other than a refusal.
                Err(error)
                    if import_stop_requested
                        || !matches!(error, RunError::ObjectImportLimit(_)) =>
                {
                    if cancelled || import_stop_requested {
                        import_stopped = true;
                        // A stop can end import, but cannot rewrite a sealed
                        // prelaunch refusal or deadline as a cancellation.
                        if !exit.never_spawned || cancelled {
                            cancelled = true;
                            crate::artifacts::seal(
                                attempt,
                                "import-cancelled.json",
                                &crate::db::bytes(&exit.scope)?,
                                RECORD_LIMIT,
                            )?;
                            decoded.outcome = Outcome::Cancelled;
                            decoded.reason = Some("cancelled during Git import".into());
                        }
                        Ok(())
                    } else if settlement
                        .as_ref()
                        .is_some_and(crate::settlement::Driver::exhausted)
                    {
                        Err(RunError::ObjectImportLimit(
                            "settlement failed after three attempts; private Git retained for host recovery",
                        ))
                    } else {
                        import_failed = true;
                        self.retain_import_fence(&record)?;
                        if matches!(
                            decoded.outcome,
                            Outcome::Succeeded | Outcome::ReviewRequired | Outcome::Uncommitted
                        ) {
                            decoded.outcome = Outcome::Failed;
                            decoded.terminal_cause = Some(TerminalCause::SettlementFailure);
                        }
                        let restored = self.unpublish_worktree(attempt, writer, record.project_id);
                        let restored = if restored.is_empty() {
                            String::new()
                        } else {
                            format!("; {restored}")
                        };
                        let reason = match decoded.reason.take() {
                            Some(reason) => {
                                format!(
                                    "{reason}; writer import remains unresolved: {error}{restored}"
                                )
                            }
                            None => {
                                format!("writer import remains unresolved: {error}{restored}")
                            }
                        };
                        decoded.reason = Some(reason.chars().take(4096).collect());
                        deferred.push((
                            SettlementEffect::GitImport,
                            EffectDisposition::Failed {
                                reason: error.to_string().chars().take(1024).collect(),
                                retryable: true,
                            },
                        ));
                        Ok(())
                    }
                }
                other => other,
            };
            match check {
                Err(error @ RunError::ObjectImportLimit(_)) => {
                    let reason = format!(
                        "writer terminal check failed for {}: {error}; {}; private Git retained for host recovery; live writer custody released",
                        writer.worktree.display(),
                        last_failure.as_deref().unwrap_or("settlement retry limit")
                    );
                    let note = self.unpublish_worktree(attempt, writer, record.project_id);
                    let reason = if note.is_empty() {
                        reason
                    } else {
                        format!("{reason}; {note}")
                    };
                    seal_import_refusal(attempt, exit.scope, &writer.worktree, &reason)?;
                    decoded.outcome = Outcome::Failed;
                    decoded.terminal_cause = Some(TerminalCause::SettlementFailure);
                    decoded.reason = Some(reason);
                }
                _ if refused => {}
                _ if crate::writer::worktree_removed(writer) => {
                    // Custody is proven absent at settlement; the work went
                    // with the directory, so no archive can be taken.
                    if decoded.outcome == Outcome::Succeeded {
                        decoded.outcome = Outcome::Lost;
                    }
                    let note = format!(
                        "worktree removed after the attempt lost custody: {}",
                        writer.worktree.display()
                    );
                    decoded.reason = Some(match decoded.reason.take() {
                        Some(reason) => format!("{reason}; {note}"),
                        None => note,
                    });
                    let archive = crate::artifacts::subdir(
                        attempt,
                        std::ffi::OsStr::new("quarantine"),
                        true,
                    )?;
                    #[derive(Serialize)]
                    struct RemovedManifest {
                        schema_version: u64,
                        scope: custody::Scope,
                        kind: &'static str,
                        disposition: &'static str,
                    }
                    note_manifest(
                        &mut deferred,
                        seal_manifest(
                            &archive,
                            "manifest.json",
                            &crate::db::bytes(&RemovedManifest {
                                schema_version: 1,
                                scope: exit.scope,
                                kind: "quarantine_manifest",
                                disposition: "worktree_removed",
                            })?,
                            RECORD_LIMIT,
                        ),
                    );
                }
                Ok(_) if decoded.outcome == Outcome::Succeeded => {
                    // Edits with no commit are not a success, and nothing is delivered.
                    let left = crate::prepare_writer_with_facts(
                        writer,
                        &self.writer_registry,
                        record.project_id,
                        &self.repo_facts,
                    )
                    .and_then(|launch| {
                        let cancel = settlement
                            .as_ref()
                            .map_or_else(CancelToken::new, |driver| driver.cancellation());
                        crate::settlement::with_cancellation(attempt, exit.scope, &cancel, || {
                            launch.uncommitted(
                                self.context.state_root.as_path(),
                                recovery.ok_or(RunError::Validation(
                                    "writer terminal custody missing",
                                ))?,
                                &cancel,
                            )
                        })
                    });
                    let note = match left {
                        Ok(None) => None,
                        Ok(Some(reason)) => {
                            decoded.outcome = Outcome::Uncommitted;
                            Some(reason)
                        }
                        Err(error) => {
                            decoded.outcome = Outcome::Failed;
                            Some(format!(
                                "writer terminal check failed for {}: {error}",
                                writer.worktree.display()
                            ))
                        }
                    };
                    if let Some(note) = note {
                        decoded.reason = Some(match decoded.reason.take() {
                            Some(reason) => format!("{reason}; {note}"),
                            None => note,
                        });
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    // An incomplete manifest remains retryable.
                    let archive = crate::artifacts::subdir(
                        attempt,
                        std::ffi::OsStr::new("quarantine"),
                        false,
                    )?;
                    if read_sealed(&archive, "manifest.json", RECORD_LIMIT)?.is_none() {
                        return Err(error);
                    }
                    decoded.outcome = Outcome::Failed;
                    decoded.reason = Some(format!(
                        "writer terminal check failed for {}: {error}",
                        writer.worktree.display()
                    ));
                }
            }
        } else {
            let archive =
                crate::artifacts::subdir(attempt, std::ffi::OsStr::new("quarantine"), true)?;
            #[derive(Serialize)]
            struct ReadOnlyManifest {
                schema_version: u64,
                scope: custody::Scope,
                kind: &'static str,
                disposition: &'static str,
            }
            note_manifest(
                &mut deferred,
                seal_manifest(
                    &archive,
                    "manifest.json",
                    &crate::db::bytes(&ReadOnlyManifest {
                        schema_version: 1,
                        scope: exit.scope,
                        kind: "quarantine_manifest",
                        disposition: "read_only",
                    })?,
                    RECORD_LIMIT,
                ),
            );
        }
        if cancelled
            || import_stopped
            || decoded.outcome == Outcome::Cancelled
            || object_import_refused(attempt, exit.scope)?.is_some()
        {
            let archive =
                crate::artifacts::subdir(attempt, std::ffi::OsStr::new("quarantine"), true)?;
            if read_sealed(&archive, "manifest.json", RECORD_LIMIT)?.is_none() {
                note_manifest(
                    &mut deferred,
                    seal_manifest(
                        &archive,
                        "manifest.json",
                        &crate::db::bytes(&serde_json::json!({
                            "schema_version": 1, "scope": exit.scope, "kind": "quarantine_manifest",
                            "disposition": if decoded.outcome == Outcome::Cancelled || import_stopped { "cancelled" } else { "import_refused" }
                        }))?,
                        RECORD_LIMIT,
                    ),
                );
            }
        }
        // Each durable phase gets a fresh short database budget. An import
        // can outlive the request which started it without exhausting Record.
        let record_call = Request {
            clock: call.clock,
            cancel: call.cancel,
        };
        let call = &record_call;
        let cancellation = read_sealed(attempt, "cancel.json", RECORD_LIMIT)?;
        if cancelled {
            decoded.outcome = Outcome::Cancelled;
            // A typed runtime deadline stays the reason when cancellation also
            // applies; a scope-only cancellation marker carries no user cause.
            if let Some(reason) = cancellation_reason(cancellation.as_deref())
                && !typed_deadline
                && !crate::store::import_cancelled(
                    attempt,
                    exit.scope,
                    record.verified_owner.unwrap_or(self.context.actor_id),
                )?
            {
                decoded.reason = Some(format!("cancelled: {reason}"));
            }
        }
        // A writer the host stopped at its context handoff threshold settles as
        // a declared stop carrying its latest progress handoff, so the graph's
        // one restart (`no_work`) continues it fresh on the same worktree. Its
        // commits and uncommitted edits stay where they are.
        if decoded.outcome == Outcome::TimedOut
            && exit.stop_reason == Some(StopReason::ContextHandoff)
            && task.writer.is_some()
            && !cancelled
        {
            // The result's handoff fence is what the graph continues from;
            // the sealed snapshot, if any, stays as the lane wrote it.
            let stopped = context_handoff(handoff.take());
            decoded.result = format!(
                "Stopped at the context handoff threshold (lanes.context_handoff_tokens); a fresh attempt continues from this handoff on the same worktree.\n\n{}",
                stopped.fenced()
            );
            decoded.turns.clear();
            decoded.complete = true;
            decoded.outcome = Outcome::ReviewRequired;
            decoded.terminal_cause = Some(TerminalCause::ContextHandoff);
            decoded.reason = Some(TerminalCause::ContextHandoff.detail().into());
            handoff = Some(stopped);
        }
        // A writer whose launch-time token copy was lost settles the same way:
        // a declared stop on its latest handoff, continued on a new token. The
        // successor's launch re-probes the account, so a login that really
        // lapsed refuses it there as `account_login_required`.
        if decoded.outcome == Outcome::AuthRequired
            && decoded.terminal_cause == Some(TerminalCause::LaneTokenLost)
            && task.writer.is_some()
            && !cancelled
        {
            let stopped = stopped_handoff(
                handoff.take(),
                "The lane's access token was lost before it recorded a progress handoff; inspect the branch and worktree.",
            );
            decoded.result = format!(
                "Stopped because this lane's access token was revoked or expired; a fresh attempt continues from this handoff on a new token.\n\n{}",
                stopped.fenced()
            );
            decoded.turns.clear();
            decoded.complete = true;
            decoded.outcome = Outcome::ReviewRequired;
            handoff = Some(stopped);
        }
        let answer_limit = RESULT_LIMIT as usize;
        if decoded.result.len() > answer_limit {
            let mut end = answer_limit;
            while !decoded.result.is_char_boundary(end) {
                end -= 1;
            }
            decoded.result.truncate(end);
            decoded.complete = false;
        }
        if matches!(decoded.outcome, Outcome::Succeeded | Outcome::Uncommitted) {
            let refusal = if exit.termination != Some(Termination::Exit(0)) {
                Some("provider exited unsuccessfully")
            } else if !decoded.complete {
                Some("provider result is empty or incomplete")
            } else if !crate::reader::verified_route(
                record.resolved_route.as_ref(),
                record.applied_route.as_ref(),
                decoded.observed_route.as_ref(),
            ) {
                Some("provider identity was not observed")
            } else {
                None
            };
            if let Some(reason) = refusal {
                decoded.outcome = Outcome::Failed;
                if reason == "provider identity was not observed" {
                    decoded.terminal_cause = Some(TerminalCause::IdentityUnobserved);
                }
                // Keep the decoder's typed identity evidence error, if any.
                decoded.reason = Some(match decoded.reason.take() {
                    detail if reason == "provider identity was not observed" => {
                        format!(
                            "{reason}: requested model {}; observed model {}",
                            record
                                .resolved_route
                                .as_ref()
                                .and_then(|route| route.model.as_deref())
                                .unwrap_or("unknown"),
                            decoded
                                .observed_route
                                .as_ref()
                                .and_then(|route| route.model.as_deref())
                                .unwrap_or("unknown"),
                        ) + &detail.map(|value| format!("; {value}")).unwrap_or_default()
                    }
                    _ => reason.into(),
                });
            }
        }
        let mut terminal_note = if exit.never_spawned && !cancelled {
            exit.launch_refusal.map(|fact| {
                let diagnostic = fact.project(provenant_records::diagnostic::Diagnostic::new(
                    ErrorKind::Refused.code(),
                ));
                match (diagnostic.detail, diagnostic.next) {
                    (Some(detail), Some(next)) => format!("{detail}; {next}"),
                    (Some(detail), None) => detail,
                    (None, Some(next)) => next,
                    (None, None) => String::new(),
                }
            })
        } else {
            None
        };
        if decoded.result.is_empty()
            && matches!(decoded.outcome, Outcome::Succeeded | Outcome::Uncommitted)
        {
            if task.writer.is_some() {
                // A writer's work is its commit. A provider that exited cleanly
                // with no final message needs review; it has not failed.
                decoded.result = EMPTY_WRITER_RESULT.into();
                decoded.turns.clear();
                if decoded.outcome == Outcome::Succeeded {
                    decoded.outcome = Outcome::ReviewRequired;
                }
                decoded.reason = Some(EMPTY_WRITER_REASON.into());
                decoded.terminal_cause = Some(TerminalCause::EmptyWriterResult);
            } else {
                decoded.outcome = Outcome::Failed;
                decoded.reason = Some("provider result is empty or incomplete".into());
            }
        }
        // Decoding can reveal credential markers hidden by JSON escapes in
        // the already-filtered capture. Seal only a filtered rendering.
        // A filter failure withholds the result instead of blocking settlement
        // forever: the lane fails with a fixed placeholder and stays terminal.
        let (mut result, result_redaction_failed) =
            match provenant_confinement::redact::known_shapes(decoded.result.as_bytes()) {
                Ok(safe) => match String::from_utf8(safe) {
                    Ok(text) => (text, false),
                    Err(_) => (UNREDACTABLE_RESULT.to_owned(), true),
                },
                Err(_) => (UNREDACTABLE_RESULT.to_owned(), true),
            };
        if task.writer.is_some()
            && task.deliver.is_none()
            && task.profile.knobs.get("lanes.receipts") == Some(&serde_json::json!("none"))
        {
            decoded.complete &= result.len() + UNDELIVERED_NOTICE.len() <= RESULT_LIMIT as usize;
            result = undelivered_writer_result(&result);
        }
        let redaction_failed = result_redaction_failed || session_redaction_failed;
        if redaction_failed {
            // A redaction failure fails a lane that would otherwise succeed. A
            // cancelled, timed-out or lost lane keeps its outcome, and every
            // reason it already has survives with the redaction failure after it.
            if matches!(
                decoded.outcome,
                Outcome::Succeeded | Outcome::ReviewRequired | Outcome::Uncommitted
            ) {
                decoded.outcome = Outcome::Failed;
            }
            decoded.reason = Some(match decoded.reason.take() {
                Some(reason) => format!("{reason}; {REDACTION_FAILED_REASON}"),
                None => REDACTION_FAILED_REASON.into(),
            });
        }
        // Delivery keys off the outcome before a declared stop, so partial work
        // is still delivered. The stop changes only the settled state.
        let succeeded_before_stop = decoded.outcome == Outcome::Succeeded;
        // A declared stop requires review even when the provider succeeded.
        let mut partial = std::mem::take(&mut decoded.partial_signals);
        // A read-only lane has no changes to prove, so its complete answer is
        // the proof. Only a writer must also report its work in a handoff.
        if matches!(
            decoded.outcome,
            Outcome::Succeeded | Outcome::ReviewRequired | Outcome::Uncommitted
        ) && task.writer.is_some()
            && !matches!(&task.route, RequestedRoute::Adapter { .. })
            && decoded.handoff().ok().flatten().is_none()
            && !reports_verdict(&decoded.result)
        {
            partial.push(PartialSignal::HandoffMissing);
            // A lane that reports nothing has not passed. It needs review,
            // and the reason names the cause.
            if decoded.outcome == Outcome::Succeeded {
                decoded.outcome = Outcome::ReviewRequired;
                decoded.reason = Some(NO_HANDOFF_REASON.into());
            }
        }
        if matches!(
            decoded.outcome,
            Outcome::Cancelled | Outcome::ReviewRequired
        ) && handoff
            .as_ref()
            .is_some_and(|handoff| !handoff.unfinished())
        {
            let note = "handoff declared done but settlement was not successful";
            decoded.reason = Some(match decoded.reason.take() {
                Some(reason) => format!("{reason}; {note}"),
                None => note.into(),
            });
        }
        if matches!(
            decoded.outcome,
            Outcome::Succeeded | Outcome::ReviewRequired | Outcome::Uncommitted
        ) {
            let mut notes = Vec::new();
            if let Some(handoff) = handoff.as_ref().filter(|handoff| handoff.unfinished()) {
                partial.push(PartialSignal::DeclaredStop);
                if decoded.outcome == Outcome::Succeeded {
                    decoded.outcome = Outcome::ReviewRequired;
                }
                notes.push(format!(
                    "lane stopped early: {}",
                    bounded_note(&handoff.note)
                ));
            }
            if decoded.outcome == Outcome::Uncommitted {
                partial.push(PartialSignal::UncommittedWork);
            }
            partial.sort();
            partial.dedup();
            if !partial.is_empty() {
                let signals: Vec<_> = partial.iter().map(|signal| signal.as_str()).collect();
                notes.insert(0, format!("partial finish ({})", signals.join(", ")));
                let note = notes.join(": ");
                decoded.reason = Some(match decoded.reason.take() {
                    Some(reason) => format!("{reason}; {note}"),
                    None => note,
                });
            }
        } else if handoff.as_ref().is_some_and(|handoff| handoff.unfinished()) {
            partial.push(PartialSignal::DeclaredStop);
            partial.sort();
            partial.dedup();
        } else {
            partial.clear();
        }
        // A blocked writer settles with its own cause and its note. A failure
        // or another cause already recorded keeps its place.
        if task.writer.is_some()
            && (decoded.terminal_cause.is_none()
                || decoded.terminal_cause == Some(TerminalCause::Blocked))
            && matches!(
                decoded.outcome,
                Outcome::Succeeded | Outcome::ReviewRequired | Outcome::Uncommitted
            )
            && let Some(blocked) = handoff.as_ref().filter(|handoff| {
                handoff.status == provenant_records::handoff::HandoffStatus::Blocked
            })
        {
            decoded.terminal_cause = Some(TerminalCause::Blocked);
            terminal_note = Some(bounded_note(&blocked.note));
        }
        // No-work is a host Git fact, never inferred from provider prose or a
        // missing head. Keep failures and uncommitted work distinct.
        if task.writer.is_some()
            && decoded.outcome == Outcome::ReviewRequired
            && partial.as_slice() == [PartialSignal::DeclaredStop]
            && decoded.terminal_cause.is_none()
            && settlement.as_ref().is_some_and(|driver| {
                driver
                    .base()
                    .zip(
                        read_sealed(attempt, "writer-terminal.json", RECORD_LIMIT)
                            .ok()
                            .flatten()
                            .and_then(|raw| {
                                crate::db::decode::<crate::writer::WriterTerminal>(&raw).ok()
                            })
                            .map(|terminal| terminal.head),
                    )
                    .is_some_and(|(base, head)| base == head)
            })
        {
            decoded.terminal_cause = Some(TerminalCause::StoppedNoWork);
            decoded.reason = Some(format!(
                "{}; {}",
                TerminalCause::StoppedNoWork.detail(),
                decoded.reason.take().unwrap_or_default()
            ));
        }
        // A denial is a diagnostic, not part of the provider's answer: the
        // handoff, findings and verdict blocks must stay as the provider wrote them.
        if let Some(denial) = denial {
            let note = denial.diagnostic();
            decoded.reason = Some(match decoded.reason.take() {
                Some(reason) => format!("{reason}; {note}"),
                None => note.into(),
            });
        }
        // External delivery runs only from the committed decision below.
        let result_orphan = read_sealed(attempt, "result.md", RESULT_LIMIT)?.is_some();
        let delivery_applicable = succeeded_before_stop
            && !redaction_failed
            && !cancelled
            && exit.stop_reason.is_none()
            && !exit.unresolved_cleanup
            && exit.publication == custody::PublicationState::Durable
            && task.writer.is_some()
            && task.deliver.is_some();
        deferred.push((
            SettlementEffect::Delivery,
            if delivery_applicable {
                EffectDisposition::Pending
            } else {
                EffectDisposition::Done
            },
        ));
        if !import_failed {
            let disposition = match object_import_refused(attempt, exit.scope)? {
                Some(reason) => EffectDisposition::Failed {
                    reason: reason.chars().take(1024).collect(),
                    retryable: false,
                },
                None => EffectDisposition::Done,
            };
            deferred.push((SettlementEffect::GitImport, disposition));
        }
        if !deferred
            .iter()
            .any(|(effect, _)| *effect == SettlementEffect::ManifestConflict)
        {
            deferred.push((
                SettlementEffect::ManifestConflict,
                if import_failed {
                    EffectDisposition::Pending
                } else {
                    EffectDisposition::Done
                },
            ));
        }
        if !cancelled && decoded.outcome == Outcome::Cancelled {
            decoded.outcome = Outcome::Unsettled;
            decoded.reason = Some("cancellation delivery before provider exit is unknown".into());
        } else if cancelled {
            decoded.outcome = Outcome::Cancelled;
        }
        if matches!(
            decoded.outcome,
            Outcome::Cancelled | Outcome::Lost | Outcome::Unsettled
        ) {
            decoded.terminal_cause = None;
        }
        let decoded_reason = decoded.reason.take().map(bounded_reason);
        let stop = provenant_records::Stop::sealed(
            decoded.terminal_cause,
            decoded.outcome,
            handoff.clone(),
        )
        .map_err(|_| RunError::Validation("stop could not be sealed"))?;
        let publication = TerminalPublication {
            stop,
            terminal_cause: decoded.terminal_cause,
            terminal_note,
            outcome: decoded.outcome,
            finished_at,
            started_at: record.started_at,
            provider_ms: record.started_at.and_then(|started| {
                u64::try_from(finished_at.as_micros().saturating_sub(started.as_micros()) / 1000)
                    .ok()
            }),
            reason: decoded_reason,
            result: &result,
            complete: decoded.complete,
            redacted: true,
            resolved_route: record.resolved_route.clone(),
            applied_route: record.applied_route.clone(),
            observed_route: decoded.observed_route,
            usage: decoded.usage,
            session: decoded.session,
            partial_signals: partial,
        };
        if let Some(raw) = read_sealed(attempt, "terminal-effects.json", RECORD_LIMIT)? {
            let saved: crate::store::TerminalEffects = crate::db::decode(&raw)?;
            if saved.schema_version != 1 || saved.scope != exit.scope {
                return Err(RunError::Conflict);
            }
            // An effects/result orphan predates the decision. Keep its initial
            // receipts; post-commit driving reconciles them with durable effects.
        } else {
            crate::artifacts::seal(
                attempt,
                "terminal-effects.json",
                &crate::db::bytes(&crate::store::TerminalEffects {
                    schema_version: 1,
                    scope: exit.scope,
                    effects: deferred,
                })?,
                RECORD_LIMIT,
            )?;
        }
        // A result sealed without intent predates a crash: keep its bytes.
        if !import_failed && let Some(driver) = &mut settlement {
            driver.advance(crate::settlement::Phase::Record)?;
        }
        let terminal = if result_orphan {
            self.recover_result_only(id, &publication, exit, call)?
        } else {
            self.publish_terminal(id, record.revision, &publication, exit, call)?
        };
        if !import_failed {
            let disposition = match object_import_refused(attempt, exit.scope)? {
                Some(reason) => EffectDisposition::Failed {
                    reason: reason.chars().take(1024).collect(),
                    retryable: false,
                },
                None => EffectDisposition::Done,
            };
            self.record_effect(
                id,
                terminal.generation,
                SettlementEffect::GitImport,
                disposition,
                call,
            )?;
        }
        if import_failed {
            return Ok(Finished {
                record: terminal,
                release_permitted: false,
            });
        }
        let finished = self.finish_effects(terminal, attempt, &completion, call)?;
        if let Some(driver) = &mut settlement {
            driver.advance(crate::settlement::Phase::Release)?;
        }
        Ok(finished)
    }
}

/// A manifest that cannot be sealed (for instance a conflicting one already
/// exists, which stays untouched) is a retryable effect, not a reason to
/// withhold the terminal decision.
fn note_manifest(deferred: &mut Vec<(SettlementEffect, EffectDisposition)>, sealed: Result<()>) {
    if let Err(error) = sealed {
        deferred.push((
            SettlementEffect::ManifestConflict,
            EffectDisposition::Failed {
                reason: error.to_string().chars().take(1024).collect(),
                retryable: true,
            },
        ));
    }
}

pub(crate) fn seal_manifest(
    archive: &Directory,
    name: &str,
    bytes: &[u8],
    limit: u64,
) -> Result<()> {
    crate::artifacts::seal(archive, "manifest-intended.json", bytes, limit)?;
    crate::artifacts::seal(archive, name, bytes, limit)
}

/// The cause shown for a lane that ended without a final handoff.
/// Placeholder result for a writer whose provider sent no final message.
/// The result of a writer stopped on its token budget that sent no final
/// message: what the host committed, and that the remainder is unknown.
/// The handoff a context handoff stop continues from: the lane's latest
/// unfinished progress handoff, or a stopped-early one naming the gap. A
/// progress handoff that claims `done` keeps its content but not that status,
/// because the host stopped the lane before its final answer.
fn context_handoff(
    progress: Option<provenant_records::handoff::Handoff>,
) -> provenant_records::handoff::Handoff {
    stopped_handoff(
        progress,
        "Stopped at the context handoff threshold before the lane recorded a progress handoff; inspect the branch and worktree.",
    )
}

/// The lane's latest unfinished progress handoff, or a stopped-early one
/// carrying `missing` when it recorded none.
fn stopped_handoff(
    progress: Option<provenant_records::handoff::Handoff>,
    missing: &str,
) -> provenant_records::handoff::Handoff {
    use provenant_records::handoff::{Handoff, HandoffStatus};
    match progress {
        Some(handoff) if handoff.unfinished() => handoff,
        progress => {
            let mut handoff = progress.unwrap_or_else(|| Handoff {
                schema_version: 2,
                status: HandoffStatus::StoppedEarly,
                note: missing.into(),
                remaining: vec!["unknown: inspect the branch and worktree".into()],
                decisions: Vec::new(),
                verify: Vec::new(),
                spec_concerns: Vec::new(),
                extra: Default::default(),
            });
            handoff.status = HandoffStatus::StoppedEarly;
            handoff
        }
    }
}

fn budget_stop_result(record: &serde_json::Value, note: &str) -> String {
    let mut text = String::from("Stopped on budget.\n");
    if let Some(commit) = record["commit"].as_str() {
        let paths: Vec<String> = record["paths"]
            .as_array()
            .map(|paths| {
                paths
                    .iter()
                    .filter_map(|path| path.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let files = record["files"].as_u64().unwrap_or(paths.len() as u64) as usize;
        text.push_str(&format!(
            "Leftover work was committed as {commit} ({files} files).\nChanged paths:\n"
        ));
        for line in crate::writer::path_list(&paths, files) {
            text.push_str(&format!("- {line}\n"));
        }
    } else {
        text.push_str(&format!("{note}.\n"));
    }
    text.push_str("What remains is unknown; inspect the branch.\n\n");
    let handoff = serde_json::json!({
        "schema_version": 2,
        "status": "stopped_early",
        "note": "Stopped on budget with no final message from the writer; the host recorded the branch state only.",
        "remaining": ["unknown: inspect branch"],
        "decisions": [],
        "verify": [],
    });
    text.push_str(&format!("```handoff\n{handoff}\n```"));
    text
}

const UNDELIVERED_NOTICE: &str = "No delivery was requested; this work has not been delivered.\n\n";

fn undelivered_writer_result(result: &str) -> String {
    let mut end = result
        .len()
        .min(RESULT_LIMIT as usize - UNDELIVERED_NOTICE.len());
    while !result.is_char_boundary(end) {
        end -= 1;
    }
    format!("{UNDELIVERED_NOTICE}{}", &result[..end])
}

pub const EMPTY_WRITER_RESULT: &str = "[the provider sent no final message]";
pub const EMPTY_WRITER_REASON: &str =
    "the provider ended with an empty final message; its commit is kept for review";
pub const NO_HANDOFF_REASON: &str = "no handoff: the lane ended without reporting results";

/// A final verdict reports a result even without a handoff.
fn reports_verdict(result: &str) -> bool {
    let mut capture = provenant_records::result_capture::ResultCapture::new(None);
    capture.push(result.as_bytes());
    capture.finish().verdict.is_some()
}
/// Stored instead of the provider's answer when settlement cannot filter it.
const UNREDACTABLE_RESULT: &str = "[result withheld: redaction failed]";
const REDACTION_FAILED_REASON: &str = "redaction failed; result withheld";

/// Captured text can suggest a denial, but cannot establish its source or
/// supply a path safe for a remote diagnostic. The operation is a closed fact;
/// the path stays in the captured output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ObservedAccessDenial {
    Create,
    Remove,
    Read,
    Write,
    Other,
}

impl ObservedAccessDenial {
    fn of(line: &str) -> Self {
        if line.contains("deny(1) file-write-create") {
            Self::Create
        } else if line.contains("deny(1) file-write-unlink") {
            Self::Remove
        } else if line.contains("deny(1) file-write") {
            Self::Write
        } else if line.contains("deny(1) file-read") {
            Self::Read
        } else if line.contains("cannot create") || line.starts_with("touch:") {
            Self::Create
        } else if line.contains("cannot remove") || line.starts_with("rm:") {
            Self::Remove
        } else if line.contains("cannot open") || line.starts_with("cat:") {
            Self::Read
        } else {
            Self::Other
        }
    }

    fn diagnostic(self) -> &'static str {
        match self {
            Self::Create => {
                "A file create was denied; check the declared access grant and filesystem permissions, then see the path in the captured output"
            }
            Self::Remove => {
                "A file remove was denied; check the declared access grant and filesystem permissions, then see the path in the captured output"
            }
            Self::Read => {
                "A file read was denied; check the declared access grant and filesystem permissions, then see the path in the captured output"
            }
            Self::Write => {
                "A file write was denied; check the declared access grant and filesystem permissions, then see the path in the captured output"
            }
            Self::Other => {
                "A path access denial was observed; check the declared access grant and filesystem permissions, then see the path in the captured output"
            }
        }
    }
}

fn path_denial_hint(stderr: &[u8]) -> Option<ObservedAccessDenial> {
    let text = String::from_utf8_lossy(stderr);
    text.lines()
        .find(|line| {
            line.contains('/')
                && (line.contains("Permission denied")
                    || line.contains("Operation not permitted")
                    || line.contains("deny(1) file-"))
        })
        .map(ObservedAccessDenial::of)
}

/// Terminal evidence refuses a longer reason, so every published reason fits.
const REASON_LIMIT: usize = 4096;

fn bounded_reason(mut reason: String) -> String {
    if reason.len() > REASON_LIMIT {
        let mut end = REASON_LIMIT;
        while !reason.is_char_boundary(end) {
            end -= 1;
        }
        reason.truncate(end);
    }
    reason
}

/// A lane's own words in a one-line reason: bounded and on one line.
fn bounded_note(note: &str) -> String {
    provenant_confinement::redact::bounded_line(note, 240)
}

#[cfg(test)]
mod budget_result_tests {
    use super::*;

    #[test]
    fn a_budget_stop_placeholder_names_the_commit_and_caps_the_paths() {
        let paths: Vec<String> = (0..20).map(|n| format!("src/f{n}.rs")).collect();
        let record = serde_json::json!({"commit": "abcdef123", "files": 25, "paths": paths});
        let text = budget_stop_result(&record, "work committed on its branch");
        assert!(text.contains("Stopped on budget"), "{text}");
        assert!(text.contains("abcdef123 (25 files)"), "{text}");
        assert!(text.contains("- src/f19.rs") && !text.contains("src/f20.rs"));
        assert!(text.contains("- and 5 more"), "{text}");
        assert!(text.contains("inspect the branch"), "{text}");
        let handoff = provenant_records::handoff::Handoff::from_result(&text)
            .unwrap()
            .expect("a handoff");
        assert_eq!(
            handoff.status,
            provenant_records::handoff::HandoffStatus::StoppedEarly
        );
        assert_eq!(handoff.remaining, ["unknown: inspect branch"]);
    }
}

#[cfg(test)]
mod host_clock_tests {
    use super::*;

    #[test]
    fn credential_environment_seals_an_exact_ssh_agent_socket() {
        use provenant_confinement::permissions::{PathScope, host_credential_roots};
        let roots = host_credential_roots(|name| match name {
            "SSH_AUTH_SOCK" => Some("/private/tmp/fixture-agent/socket".into()),
            "XDG_CONFIG_HOME" => Some("/fixture/config".into()),
            _ => None,
        });
        assert!(roots.contains(&PathGrant {
            path: "/private/tmp/fixture-agent/socket".into(),
            scope: PathScope::Exact
        }));
        assert!(roots.contains(&PathGrant {
            path: "/fixture/config/opencode".into(),
            scope: PathScope::Subtree
        }));
        assert!(
            !roots
                .iter()
                .any(|root| root.path == std::path::Path::new("/private/tmp/fixture-agent"))
        );
        assert!(
            host_credential_roots(
                |name| (name == "SSH_AUTH_SOCK").then(|| "relative/socket".into())
            )
            .is_empty()
        );
    }

    #[test]
    fn host_launch_decode_requires_explicit_boundary_inventory()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let context: HostLaunchContext = serde_json::from_value(serde_json::json!({
            "configuration_root": "/fixture/state",
            "cwd": "/fixture/source",
            "executables": {},
            "git": "/fixture/git",
            "credential_roots": [],
            "github_roots": [],
            "provider_path_environment": {}
        }))?;
        assert!(context.credential_roots.is_empty());
        assert!(context.github_roots.is_empty());
        assert!(context.provider_path_environment.is_empty());
        let encoded = serde_json::to_value(&context)?;
        for field in [
            "credential_roots",
            "github_roots",
            "provider_path_environment",
        ] {
            let mut incomplete = encoded.clone();
            incomplete
                .as_object_mut()
                .ok_or("host context object")?
                .remove(field);
            assert!(
                serde_json::from_value::<HostLaunchContext>(incomplete).is_err(),
                "missing {field} accepted"
            );
        }
        let retained: HostLaunchContext = serde_json::from_value(encoded)?;
        assert_eq!(retained, context);
        Ok(())
    }

    #[test]
    fn account_directory_overrides_follow_registry_entries() {
        let mut fixture = provenant_records::ADAPTER_SPECS[0];
        fixture.name = "fixture";
        fixture.default_account_directory = Some(("FIXTURE_HOME", ".fixture"));
        let actual = account_directories(&[fixture], |key| {
            (key == "FIXTURE_HOME").then(|| "/fixture/home".into())
        });
        assert_eq!(actual.get("fixture"), Some(&PathBuf::from("/fixture/home")));
        assert!(account_directories(&[fixture], |_| None).is_empty());
    }

    #[test]
    fn a_published_reason_fits_terminal_evidence_on_a_character_boundary() {
        let long = "é".repeat(REASON_LIMIT);
        let bounded = bounded_reason(long.clone());
        assert!(bounded.len() <= REASON_LIMIT);
        assert!(long.starts_with(&bounded));
        assert_eq!(bounded_reason("short".into()), "short");
    }

    #[test]
    fn path_denial_hints_do_not_project_captured_text() {
        assert_eq!(path_denial_hint(b"Permission denied"), None);
        assert_eq!(path_denial_hint(b"/fixture/path: unrelated error"), None);
        let cause =
            path_denial_hint(b"/private/host: Operation not permitted TOKEN_SENTINEL \x1b[31m")
                .expect("path denial");
        let hint = cause.diagnostic();
        assert!(hint.contains("access grant"));
        assert!(!hint.contains("/private/host"));
        assert!(!hint.contains("TOKEN_SENTINEL"));
        assert!(!hint.contains('\u{1b}'));
    }

    #[test]
    fn path_denial_hints_name_the_operation() {
        for (line, expected, phrase) in [
            (
                &b"touch: /fixture/repo/file: Operation not permitted"[..],
                ObservedAccessDenial::Create,
                "file create",
            ),
            (
                b"cat: /fixture/secret: Permission denied",
                ObservedAccessDenial::Read,
                "file read",
            ),
            (
                b"deny(1) file-write-create /fixture/repo/new",
                ObservedAccessDenial::Create,
                "file create",
            ),
            (
                b"deny(1) file-write-data /fixture/repo/new",
                ObservedAccessDenial::Write,
                "file write",
            ),
        ] {
            let cause = path_denial_hint(line).expect("path denial");
            assert_eq!(cause, expected);
            assert!(cause.diagnostic().contains(phrase));
            assert!(!cause.diagnostic().contains("/fixture"));
        }
    }

    #[test]
    fn only_a_reportable_verdict_counts_as_a_lane_reporting() {
        assert!(reports_verdict("Reviewed the change.\n\nVERDICT: PASS"));
        assert!(reports_verdict(
            "Reviewed the change.\nVERDICT: FAIL\n```handoff\n{\"schema_version\":2,\"status\":\"done\"}\n```"
        ));
        // Exactly one verdict line outside fenced blocks counts, wherever it sits.
        assert!(reports_verdict(
            "Reviewed the change.\nVERDICT: PASS\nOne more note."
        ));
        assert!(reports_verdict(
            "```text\nVERDICT: FAIL\n```\nVERDICT: INCONCLUSIVE"
        ));
        // Several verdict lines, a malformed token or none do not.
        assert!(!reports_verdict("VERDICT: PASS\nVERDICT: FAIL"));
        assert!(!reports_verdict("Reviewed.\nVERDICT: MAYBE soon"));
        assert!(!reports_verdict("Reviewed.\nVERDICT: maybe"));
        assert!(!reports_verdict("Reviewed the change with no verdict."));
    }

    #[test]
    fn admission_failures_reach_guardian_as_typed_causes() {
        assert!(matches!(
            custody_error(RunError::Admission(AdmissionError::BuildSettings)),
            CustodyError::Admission(AdmissionError::BuildSettings)
        ));
        assert!(matches!(
            custody_error(RunError::Admission(AdmissionError::GuardianIdentity)),
            CustodyError::Admission(AdmissionError::GuardianIdentity)
        ));
    }

    #[test]
    fn an_unreadable_host_clock_reaches_the_guardian_as_itself() {
        assert!(matches!(
            custody_error(RunError::Admission(AdmissionError::Clock)),
            CustodyError::Admission(AdmissionError::Clock)
        ));
        assert!(matches!(
            custody_error(RunError::Validation("wall clock")),
            CustodyError::Io(_)
        ));
    }
}

#[cfg(test)]
mod disk_admission_tests {
    use super::*;
    #[test]
    fn guardian_admission_uses_injected_host_facts_and_clock()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use provenant_supervisor::Holder;

        for available in [1, 100] {
            let temp = tempfile::tempdir()?;
            let base = std::fs::canonicalize(temp.path())?;
            let clock = crate::tests::clock();
            let cancel = CancelToken::new();
            let call = crate::tests::call(&clock, &cancel);
            let execution = crate::tests::context(temp.path())?;
            let mut store = RunStore::open(&execution, &call)?;
            store.set_admission_policy(crate::AdmissionPolicy {
                memory_floor_basis_points: 100,
                ..Default::default()
            });
            let run = crate::tests::input();
            store.create(&run, &call)?;
            let id = run.attempt_id;
            store.mark_starting(
                &store.record(id, &call)?,
                &crate::tests::route(),
                &[],
                &Default::default(),
                None,
                &call,
            )?;
            let attempt = store.attempt_dir(id, false)?;
            let context = HolderContext {
                schema_version: 1,
                roots: RootSeed {
                    home: base.join("home"),
                    checkout: base.join("checkout"),
                    state_root: execution.state_root.as_path().to_path_buf(),
                },
                project_id: execution.project_id,
                actor_id: execution.actor_id,
                attempt_id: id,
                host_delivery: None,
                launch: None,
                sealed: None,
            };
            let holder = RunHolder {
                providers: &crate::dispatch_tests::Fake::default(),
                resources: &crate::tests::HostFacts::new(available),
                clock: &clock,
            };
            holder.admit(&context.encode()?, &attempt)?;
            let admitted = store.record(id, &call)?;
            assert!(matches!(admitted.state, AttemptState::Running { .. }));
            assert_eq!(admitted.started_at, Some(clock.wall_now()?));
        }
        Ok(())
    }

    #[test]
    fn admission_samples_the_sealed_working_directory_and_cache()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let attempt = Directory::open_anchor(temp.path())?;
        let blocker = temp.path().join("file");
        std::fs::write(&blocker, "")?;
        let mut context = HolderContext {
            schema_version: 1,
            roots: RootSeed {
                home: temp.path().into(),
                checkout: temp.path().into(),
                state_root: temp.path().into(),
            },
            project_id: provenant_core::ProjectId::from_bytes([1; 16]),
            actor_id: provenant_core::ActorId::from_bytes([2; 16]),
            attempt_id: AttemptId::from_bytes([4; 16]),
            host_delivery: None,
            launch: Some(HostLaunchContext {
                graph_directory: None,
                configuration_root: temp.path().into(),
                cwd: temp.path().into(),
                executables: Default::default(),
                executable_search_path: Vec::new(),
                git: "/usr/bin/git".into(),
                default_account_directories: Default::default(),
                credential_roots: Default::default(),
                github_roots: Default::default(),
                provider_path_environment: Default::default(),
                cargo_build_jobs: None,
                #[cfg(feature = "fixture")]
                fixture_state_write: false,
            }),
            sealed: None,
        };
        struct Limited;
        impl Resources for Limited {
            fn guardian_identity(&self) -> Option<provenant_supervisor::census::ProcessIdentity> {
                crate::tests::HostFacts::new(100).guardian_identity()
            }
            fn observe_process(
                &self,
                pid: i64,
            ) -> provenant_supervisor::census::ProcessObservation {
                crate::tests::HostFacts::new(100).observe_process(pid)
            }
            fn memory(&self) -> std::io::Result<provenant_os::sys::resources::MemorySample> {
                Ok(provenant_os::sys::resources::MemorySample {
                    available_bytes: 100,
                    total_bytes: 100,
                })
            }
            fn disk(&self, _: &Directory) -> std::io::Result<u64> {
                Ok(u64::MAX)
            }
            fn process_budget(
                &self,
            ) -> std::io::Result<provenant_os::sys::resources::ProcessBudget> {
                Ok(provenant_os::sys::resources::ProcessBudget {
                    used: 950,
                    limit: 1000,
                })
            }
        }
        let budget = LaneResources {
            resources: &Limited,
            context: &context,
        }
        .process_budget()?;
        assert_eq!(budget.used, 950);
        assert!(!budget.has_room());
        let available = |context: &HolderContext| {
            LaneResources {
                resources: &SystemResources,
                context,
            }
            .disk(&attempt)
        };
        assert!(available(&context).is_ok(), "a missing cache is observable");
        if let Some(launch) = &mut context.launch {
            launch.cwd = blocker.join("child");
        }
        assert!(
            available(&context).is_err(),
            "admission must observe the sealed cwd rather than only the attempt volume"
        );
        if let Some(launch) = &mut context.launch {
            launch.cwd = temp.path().into();
        }
        context.roots.home = blocker.join("child");
        assert!(
            available(&context).is_err(),
            "admission must observe the cache volume"
        );
        Ok(())
    }

    #[test]
    fn disk_refusal_reaches_the_guardian_as_safe_numeric_evidence() {
        assert!(matches!(
            custody_error(RunError::Admission(AdmissionError::Disk {
                free: 99,
                floor: 100,
                origin: provenant_records::diagnostic::SettingSource::Configuration,
            })),
            CustodyError::Admission(AdmissionError::Disk {
                free: 99,
                floor: 100,
                origin: provenant_records::diagnostic::SettingSource::Configuration,
            })
        ));
    }
}

#[cfg(test)]
mod writer_recovery_tests {
    use super::*;
    use crate::tests::{call, clock, context, input, route};

    #[test]
    fn modern_writer_reconcile_preserves_unlaunched_private_evidence()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use crate::delivery_tests::{PrepareFixture, registered_landed_writer_fixture};
        use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
        use std::ffi::OsString;
        for mode in [
            "no_summary_terminal_modern",
            "no_summary_terminal_other_generation",
        ] {
            let fixture = PrepareFixture::new()?;
            let execution = registered_landed_writer_fixture(&fixture)?;
            let (clock, cancel) = (clock(), CancelToken::new());
            let call = call(&clock, &cancel);
            let store = RunStore::open(&execution, &call)?;
            let mut run = input();
            run.request.task.writer = Some(fixture.writer.clone());
            store.create(&run, &call)?;
            let id = run.attempt_id;
            let record = store.mark_starting(
                &store.record(id, &call)?,
                &route(),
                &[],
                &Default::default(),
                None,
                &call,
            )?;
            let scope = RunStore::scope(&record);
            let attempt = store.attempt_dir(id, false)?;
            let mut launch = crate::prepare_writer(
                &fixture.writer,
                execution.state_root.as_path(),
                execution.project_id,
            )?;
            launch.snapshot(execution.state_root.as_path(), &attempt, scope)?;
            let candidate = store.temporary_path(id, &call)?.join("candidate-0");
            std::fs::create_dir_all(&candidate)?;
            let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
            crate::objects::prepare_bound(
                &git,
                launch.worktree.repo(),
                &attempt,
                &candidate,
                launch.branch.as_str(),
                launch.base,
            )?;
            // A commit that initially exists only in the writer's private Git.
            let private_git =
                |args: &[&str]| -> std::result::Result<String, Box<dyn std::error::Error>> {
                    let argv: Vec<OsString> = std::iter::once(git.as_os_str().to_owned())
                        .chain(args.iter().map(|arg| OsString::from(*arg)))
                        .collect();
                    let mut environment: Vec<(OsString, OsString)> = [
                        ("PATH", "/usr/bin:/bin"),
                        ("GIT_CONFIG_NOSYSTEM", "1"),
                        ("GIT_CONFIG_GLOBAL", "/dev/null"),
                        ("GIT_AUTHOR_NAME", "Fixture"),
                        ("GIT_COMMITTER_NAME", "Fixture"),
                        ("GIT_AUTHOR_EMAIL", "fixture@example.invalid"),
                        ("GIT_COMMITTER_EMAIL", "fixture@example.invalid"),
                    ]
                    .into_iter()
                    .map(|(key, value)| (key.into(), value.into()))
                    .collect();
                    environment.extend([
                        (
                            "GIT_DIR".into(),
                            candidate.join("git-common").into_os_string(),
                        ),
                        (
                            "GIT_COMMON_DIR".into(),
                            candidate.join("git-common").into_os_string(),
                        ),
                        (
                            "GIT_OBJECT_DIRECTORY".into(),
                            candidate.join("git-objects").into_os_string(),
                        ),
                    ]);
                    let mut options = ExecOptions::new(
                        &argv,
                        &fixture.writer.worktree,
                        std::time::Duration::from_secs(5),
                    );
                    options.env = Some(&environment);
                    let output = SystemExecutor::new(1)?.execute(options)?;
                    assert!(
                        output.termination == Termination::Exit(0),
                        "{}",
                        String::from_utf8_lossy(&output.stderr.bytes)
                    );
                    Ok(String::from_utf8(output.stdout.bytes)?.trim().to_owned())
                };
            let tree = private_git(&["rev-parse", "HEAD^{tree}"])?;
            let head = private_git(&[
                "commit-tree",
                &tree,
                "-p",
                &launch.base.to_string(),
                "-m",
                "private commit",
            ])?;
            private_git(&["update-ref", launch.branch.as_str(), &head])?;
            let (slot, binding) = launch.custody.take().ok_or("writer custody")?;
            let slot_path = execution
                .state_root
                .as_path()
                .join("worktrees")
                .join(execution.project_id.to_string())
                .join(provenant_confinement::worktrees::state_dir_name(
                    &fixture.writer.worktree,
                ));
            let lease = provenant_supervisor::fixture::acquire_writer_with_binding(
                slot.try_clone()?,
                scope,
                binding,
                call.lock_budget(),
                &cancel,
                &clock,
            )?;
            drop(provenant_supervisor::fixture::bind_writer_attempt(
                lease, &attempt,
            )?);
            let modern = slot_path.join(id.to_string());
            let mut reservation: serde_json::Value =
                serde_json::from_slice(&std::fs::read(modern.join("reservation.json"))?)?;
            if mode == "no_summary_terminal_other_generation" {
                reservation["scope"]["generation"] = (scope.generation + 1).into();
                let anchor = provenant_os::fs::Directory::open_anchor(&modern)?.metadata()?;
                reservation["reservation_device"] = anchor.device.into();
                reservation["reservation_inode"] = anchor.inode.into();
                std::fs::write(
                    modern.join("reservation.json"),
                    serde_json::to_vec(&reservation)?,
                )?;
            }
            store.commit_lost(
                scope,
                "launch interrupted: provider never spawned; private evidence retained",
                &call,
            )?;
            let mut spawned = false;
            store.queue_reconcile_with(run.attempt_id, &call, |argv, _| {
                assert_eq!(argv[1..3], ["internal", "settle"]);
                spawned = true;
                Ok(())
            })?;
            assert!(spawned, "{mode}: reconcile must queue recovery");
            let queued = store.attempt_dir(run.attempt_id, false)?;
            assert!(
                crate::artifacts::read_sealed(&queued, "settlement-queued.json", RECORD_LIMIT)?
                    .is_some(),
                "{mode}: reconcile must queue recovery"
            );
            if mode == "no_summary_terminal_other_generation" {
                continue;
            }
            let found = store.reconcile_with_observer(
                run.attempt_id,
                &crate::dispatch_tests::Fake::default(),
                &call,
                &provenant_supervisor::fixture::RecordedBootObserver,
            )?;
            assert!(
                matches!(
                    found.as_slice(),
                    [(
                        _,
                        crate::Reconciled::Finished | crate::Reconciled::Withdrawn
                    )]
                ),
                "{mode}: {found:?}"
            );
            let terminal = store.record(id, &call)?;
            assert!(
                matches!(terminal.state, AttemptState::Terminal { .. }),
                "{mode}: {:?}",
                terminal.state
            );
            assert!(
                matches!(terminal.state, AttemptState::Terminal { outcome: Outcome::Lost, reason: Some(ref reason), .. } if reason.contains("never spawned")),
                "{:?}",
                terminal.state
            );
            assert!(store.writer_fences()?.is_empty());
            assert!(store.writer_fence_listing()?.issues.is_empty());
            assert!(!slot_path.join("reservation.json").exists());
            assert!(modern.join("settlement.json").is_file());
            assert_eq!(
                private_git(&["rev-parse", "HEAD"])?,
                head,
                "unlaunched private head retained"
            );
            store.gc_provider_state(
                &Directory::open_anchor(execution.state_root.as_path())?,
                7,
                clock
                    .wall_now()?
                    .checked_add(provenant_core::DurationMicros::from_seconds(2 * 86400)?)?,
                false,
                &call,
            )?;
            assert_eq!(
                private_git(&["rev-parse", "HEAD"])?,
                head,
                "private head retained after GC eligibility"
            );
        }
        Ok(())
    }

    #[test]
    fn settlement_finish_cleans_writer_pins_with_recovery_held()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use crate::delivery_tests::{PrepareFixture, registered_landed_writer_fixture};
        let fixture = PrepareFixture::new()?;
        let execution = registered_landed_writer_fixture(&fixture)?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&execution, &call)?;
        let mut run = input();
        run.request.task.writer = Some(fixture.writer.clone());
        store.create(&run, &call)?;
        let id = run.attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let record = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let scope = RunStore::scope(&record);
        let attempt = store.attempt_dir(id, false)?;
        let mut launch = crate::prepare_writer(
            &fixture.writer,
            execution.state_root.as_path(),
            execution.project_id,
        )?;
        launch.snapshot(execution.state_root.as_path(), &attempt, scope)?;
        let candidate = store.temporary_path(id, &call)?.join("candidate-0");
        std::fs::create_dir_all(&candidate)?;
        let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
        crate::objects::prepare_bound(
            &git,
            launch.worktree.repo(),
            &attempt,
            &candidate,
            launch.branch.as_str(),
            launch.base,
        )?;
        let pin = format!("refs/provenant/import/{}", launch.base);
        fixture.git(&["update-ref", &pin, &launch.base.to_string()])?;
        let (directory, binding) = launch.custody.take().ok_or("writer custody")?;
        let lease = provenant_supervisor::fixture::acquire_writer_with_binding(
            directory,
            scope,
            binding,
            call.lock_budget(),
            &cancel,
            &clock,
        )?;
        drop(provenant_supervisor::fixture::bind_writer_attempt(
            lease, &attempt,
        )?);
        let mut observed = crate::tests::exit();
        observed.scope = scope;
        crate::dispatch_tests::seal_exit(&store, &observed)?;
        crate::artifacts::seal(
            &attempt,
            "launch.json",
            &crate::db::bytes(
                &serde_json::json!({"schema_version":1,"scope":scope,"requester":execution.actor_id}),
            )?,
            RECORD_LIMIT,
        )?;
        let finished = store.finish(
            id,
            &attempt,
            &observed,
            &crate::dispatch_tests::Fake::default(),
            &call,
        )?;
        assert_eq!(
            finished
                .settlement_receipts
                .unwrap()
                .cleanup_custody_release,
            EffectDisposition::Done
        );
        assert!(crate::objects::resolve_ref(&git, launch.worktree.repo(), &pin).is_err());
        let db = rusqlite::Connection::open(execution.state_root.as_path().join("run.sqlite3"))?;
        let retained: bool = db.query_row(
            "SELECT tmp_retained FROM attempts WHERE id=?1",
            [id.to_string()],
            |row| row.get(0),
        )?;
        assert!(!retained);
        Ok(())
    }

    #[test]
    fn budget_warning_records_the_steer_refusal_for_lane_providers()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use provenant_records::control::{
            ControlKind, ControlReceipt, ControlState, ControlSupport, LaunchKind,
        };
        use provenant_supervisor::Holder;
        for provider in ["claude", "codex", "command", "unknown"] {
            let temp = tempfile::tempdir()?;
            let base = std::fs::canonicalize(temp.path())?;
            let clock = clock();
            let cancel = CancelToken::new();
            let call = call(&clock, &cancel);
            let execution = context(temp.path())?;
            let store = RunStore::open(&execution, &call)?;
            let run = input();
            store.create(&run, &call)?;
            let id = run.attempt_id;
            let applied = RouteRecord {
                provider: Some(provider.into()),
                ..route()
            };
            store.mark_starting(
                &store.record(id, &call)?,
                &applied,
                &[],
                &Default::default(),
                None,
                &call,
            )?;
            let attempt = store.attempt_dir(id, false)?;
            let context = HolderContext {
                schema_version: 1,
                roots: RootSeed {
                    home: base.join("home"),
                    checkout: base.join("checkout"),
                    state_root: execution.state_root.as_path().to_path_buf(),
                },
                project_id: execution.project_id,
                actor_id: execution.actor_id,
                attempt_id: id,
                host_delivery: None,
                launch: None,
                sealed: None,
            };
            let holder = RunHolder {
                providers: &crate::dispatch_tests::Fake::default(),
                resources: &crate::tests::HostFacts::new(100),
                clock: &clock,
            };
            holder.budget_warning(&context.encode()?, 450_000, 500_000)?;
            let warning: serde_json::Value = serde_json::from_slice(
                &read_sealed(&attempt, "budget-warning.json", RECORD_LIMIT)?
                    .ok_or("budget warning")?,
            )?;
            assert_eq!(warning["tokens"], 450_000);
            assert_eq!(warning["budget"], 500_000);
            let receipt: ControlReceipt = serde_json::from_value(warning["receipt"].clone())?;
            assert_eq!(receipt.control, ControlKind::Steer);
            assert_eq!(receipt.target, id.to_string());
            assert_eq!(receipt.launch, LaunchKind::Lane);
            assert_eq!(receipt.state, ControlState::Unsupported);
            let support = provenant_records::ADAPTER_SPECS
                .iter()
                .find(|spec| spec.name == provider)
                .map_or(ControlSupport::Unsupported, |spec| {
                    spec.launch.control.for_launch(LaunchKind::Lane)
                });
            assert_eq!(receipt.support, support);
            assert!(!receipt.support.delivers());
            assert_eq!(receipt.transport, None);
            assert_eq!(receipt.message_id, None);
        }
        Ok(())
    }

    #[test]
    fn declared_stop_without_a_commit_reports_no_work_in_outcome_and_status()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use crate::delivery_tests::{PrepareFixture, registered_landed_writer_fixture};
        use provenant_records::wire::lane::{LaneProjectionFacts, LaneRow};
        for (changed, dirty, blocked, shared) in [
            (false, false, false, false),
            (true, false, false, false),
            (false, true, false, false),
            (false, false, true, false),
            (true, false, false, true),
        ] {
            let fixture = PrepareFixture::new()?;
            let execution = registered_landed_writer_fixture(&fixture)?;
            let (clock, cancel) = (clock(), CancelToken::new());
            let call = call(&clock, &cancel);
            let store = RunStore::open(&execution, &call)?;
            let mut run = input();
            run.request.task.writer = Some(fixture.writer.clone());
            store.create(&run, &call)?;
            let id = run.attempt_id;
            let limits = provenant_records::LaneLimits::default();
            store.mark_starting_with_priority(
                &store.record(id, &call)?,
                &route(),
                &[],
                &Default::default(),
                None,
                None,
                &limits,
                &call,
            )?;
            let record = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
            let scope = RunStore::scope(&record);
            let attempt = store.attempt_dir(id, false)?;
            let mut launch = crate::prepare_writer(
                &fixture.writer,
                execution.state_root.as_path(),
                execution.project_id,
            )?;
            launch.snapshot(execution.state_root.as_path(), &attempt, scope)?;
            let candidate = store.temporary_path(id, &call)?.join("candidate-0");
            std::fs::create_dir_all(&candidate)?;
            let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
            crate::objects::prepare_bound(
                &git,
                launch.worktree.repo(),
                &attempt,
                &candidate,
                launch.branch.as_str(),
                launch.base,
            )?;
            let pin = format!("refs/provenant/import/{}", launch.base);
            fixture.git(&["update-ref", &pin, &launch.base.to_string()])?;
            let (directory, binding) = launch.custody.take().ok_or("writer custody")?;
            let lease = provenant_supervisor::fixture::acquire_writer_with_binding(
                directory,
                scope,
                binding,
                call.lock_budget(),
                &cancel,
                &clock,
            )?;
            drop(provenant_supervisor::fixture::bind_writer_attempt(
                lease, &attempt,
            )?);
            let mut shared_head = None;
            if changed {
                let objects = candidate.join("git-objects");
                let private_git = |args: &[std::ffi::OsString]| {
                    crate::objects::execute_bounded(
                        &git,
                        &candidate.join("git-common"),
                        &fixture.writer.worktree,
                        args,
                        None,
                        crate::objects::Execution {
                            object_directory: Some(&objects),
                            ..Default::default()
                        },
                    )
                };
                let head = private_git(&[
                    "-c".into(),
                    "user.name=Fixture".into(),
                    "-c".into(),
                    "user.email=fixture@example.invalid".into(),
                    "commit-tree".into(),
                    format!("{}^{{tree}}", launch.base).into(),
                    "-p".into(),
                    launch.base.to_string().into(),
                    "-m".into(),
                    "Partial work".into(),
                ])?;
                let head = std::str::from_utf8(&head)?.trim();
                if shared {
                    // A native writer may commit file changes through the shared
                    // worktree while the private candidate remains at the base.
                    std::fs::write(
                        fixture.writer.worktree.join("shared-work.txt"),
                        "committed native work",
                    )?;
                    fixture.lane_git(&["add", "shared-work.txt"])?;
                    fixture.lane_git(&["commit", "-m", "Shared commit"])?;
                    shared_head = Some(
                        std::str::from_utf8(&provenant_os::fixture::run_git(
                            &fixture.writer.worktree,
                            &["rev-parse", "HEAD"],
                        )?)?
                        .trim()
                        .parse::<provenant_core::GitOid>()?,
                    );
                } else {
                    private_git(&[
                        "update-ref".into(),
                        launch.branch.as_str().into(),
                        head.into(),
                    ])?;
                }
            }
            if dirty {
                std::fs::write(
                    fixture.writer.worktree.join("unfinished.txt"),
                    "retained partial edit",
                )?;
            }
            let mut observed = crate::tests::exit();
            observed.scope = scope;
            crate::dispatch_tests::seal_exit(&store, &observed)?;
            let mut fake =
                crate::dispatch_tests::Fake::with_observed(Some(route()), Some(Outcome::Succeeded));
            let note = if changed {
                "No commit was made; acceptance remains unfinished"
            } else {
                "Acceptance remains unfinished"
            };
            let status = if blocked { "blocked" } else { "stopped_early" };
            fake.answer = Some(format!(
                "```handoff\n{{\"schema_version\":2,\"status\":\"{status}\",\"note\":\"{note}\",\"remaining\":[\"Finish work\"],\"decisions\":[],\"verify\":[]}}\n```"
            ));
            let finished = store.finish(id, &attempt, &observed, &fake, &call)?;
            if let Some(head) = shared_head {
                assert_eq!(finished.head, Some(head));
                let terminal: crate::writer::WriterTerminal = crate::db::decode(
                    &read_sealed(&attempt, "writer-terminal.json", RECORD_LIMIT)?
                        .ok_or("writer terminal")?,
                )?;
                assert_eq!(terminal.head, head);
                assert!(
                    provenant_os::fixture::run_git(
                        &fixture.writer.worktree,
                        &["status", "--porcelain", "--untracked-files=normal"],
                    )?
                    .is_empty(),
                    "settlement must preserve the native commit's clean index"
                );
            }
            if blocked {
                // A blocked writer seals one typed stop.
                let AttemptState::Terminal { extra, .. } = &finished.state else {
                    panic!("not terminal")
                };
                assert_eq!(
                    extra.get("terminal_cause"),
                    Some(&serde_json::json!(TerminalCause::Blocked))
                );
                let stop: provenant_records::Stop =
                    serde_json::from_value(extra.get("stop").cloned().ok_or("stop")?)?;
                assert_eq!(stop.cause(), provenant_records::StopCause::Blocked);
                assert_eq!(stop.note(), Some(note));
                continue;
            }
            assert_eq!(finished.superseded_by, None);
            if dirty {
                assert!(matches!(
                    finished.state,
                    AttemptState::Terminal {
                        outcome: Outcome::Uncommitted,
                        ..
                    }
                ));
                assert_eq!(
                    std::fs::read_to_string(fixture.writer.worktree.join("unfinished.txt"))?,
                    "retained partial edit"
                );
                assert_eq!(
                    store.request(run.attempt_id, &call)?.task.writer.as_ref(),
                    Some(&fixture.writer)
                );
                continue;
            }
            assert_eq!(
                finished.settlement_reason,
                Some(provenant_records::run::SettlementReason::DeclaredStop)
            );
            let AttemptState::Terminal {
                extra,
                outcome,
                reason,
                ..
            } = &finished.state
            else {
                panic!("not terminal")
            };
            assert_eq!(*outcome, Outcome::ReviewRequired);
            assert_eq!(
                extra.get("terminal_cause"),
                (!changed)
                    .then(|| serde_json::json!(TerminalCause::StoppedNoWork))
                    .as_ref()
            );
            let row = LaneRow::project(&finished, LaneProjectionFacts::default());
            assert_eq!(
                row.terminal_cause,
                (!changed).then_some(TerminalCause::StoppedNoWork)
            );
            assert!(
                !reason
                    .as_deref()
                    .unwrap()
                    .contains("handoff disagrees with the branch"),
                "{reason:?}"
            );
            if !changed {
                assert!(reason.as_deref().unwrap().contains("no new commit"));
                assert_eq!(
                    LaneRow::state_and_label(&finished.state).1.as_deref(),
                    Some(TerminalCause::StoppedNoWork.reason())
                );
                assert_eq!(
                    row.reason.as_deref(),
                    Some(TerminalCause::StoppedNoWork.detail())
                );
                assert_eq!(
                    row.settlement.unwrap().reason.as_deref(),
                    Some(TerminalCause::StoppedNoWork.detail())
                );
            }
        }
        Ok(())
    }

    #[test]
    fn empty_committed_writer_requires_review_and_controls_fail()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use crate::delivery_tests::{PrepareFixture, registered_landed_writer_fixture};
        use provenant_records::wire::lane::{LaneProjectionFacts, LaneRow};
        for (writer, outcome) in [
            (true, Outcome::Succeeded),
            (false, Outcome::Succeeded),
            (true, Outcome::Failed),
        ] {
            let fixture = PrepareFixture::new()?;
            let execution = registered_landed_writer_fixture(&fixture)?;
            let (clock, cancel) = (clock(), CancelToken::new());
            let call = call(&clock, &cancel);
            let store = RunStore::open(&execution, &call)?;
            let mut run = input();
            if writer {
                run.request.task.writer = Some(fixture.writer.clone());
            }
            store.create(&run, &call)?;
            let id = run.attempt_id;
            store.mark_starting(
                &store.record(id, &call)?,
                &route(),
                &[],
                &Default::default(),
                None,
                &call,
            )?;
            let record = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
            let scope = RunStore::scope(&record);
            let attempt = store.attempt_dir(id, false)?;
            let mut launch = crate::prepare_writer(
                &fixture.writer,
                execution.state_root.as_path(),
                execution.project_id,
            )?;
            launch.snapshot(execution.state_root.as_path(), &attempt, scope)?;
            let candidate = store.temporary_path(id, &call)?.join("candidate-0");
            std::fs::create_dir_all(&candidate)?;
            let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
            crate::objects::prepare_bound(
                &git,
                launch.worktree.repo(),
                &attempt,
                &candidate,
                launch.branch.as_str(),
                launch.base,
            )?;
            let pin = format!("refs/provenant/import/{}", launch.base);
            fixture.git(&["update-ref", &pin, &launch.base.to_string()])?;
            let (directory, binding) = launch.custody.take().ok_or("writer custody")?;
            let lease = provenant_supervisor::fixture::acquire_writer_with_binding(
                directory,
                scope,
                binding,
                call.lock_budget(),
                &cancel,
                &clock,
            )?;
            drop(provenant_supervisor::fixture::bind_writer_attempt(
                lease, &attempt,
            )?);
            let committed_head = {
                let objects = candidate.join("git-objects");
                let shared_objects = launch.worktree.repo().common_git_dir().join("objects");
                let private_git = |args: &[std::ffi::OsString]| {
                    crate::objects::execute_bounded(
                        &git,
                        &candidate.join("git-common"),
                        &fixture.writer.worktree,
                        args,
                        None,
                        crate::objects::Execution {
                            object_directory: Some(&objects),
                            alternate_directory: Some(&shared_objects),
                            ..Default::default()
                        },
                    )
                };
                let head = private_git(&[
                    "-c".into(),
                    "user.name=Fixture".into(),
                    "-c".into(),
                    "user.email=fixture@example.invalid".into(),
                    "commit-tree".into(),
                    format!("{}^{{tree}}", launch.base).into(),
                    "-p".into(),
                    launch.base.to_string().into(),
                    "-m".into(),
                    "Partial work".into(),
                ])?;
                let head = std::str::from_utf8(&head)?.trim();
                private_git(&[
                    "update-ref".into(),
                    launch.branch.as_str().into(),
                    head.into(),
                ])?;
                head.to_owned()
            };
            let mut observed = crate::tests::exit();
            observed.scope = scope;
            crate::dispatch_tests::seal_exit(&store, &observed)?;
            let mut fake = crate::dispatch_tests::Fake::with_observed(Some(route()), Some(outcome));
            fake.answer = Some(String::new());
            let finished = store.finish(id, &attempt, &observed, &fake, &call)?;
            let AttemptState::Terminal {
                outcome: actual,
                reason,
                ..
            } = &finished.state
            else {
                panic!("not terminal")
            };
            if writer && outcome == Outcome::Succeeded {
                assert_eq!(*actual, Outcome::ReviewRequired, "{reason:?}");
                assert!(reason.as_deref().unwrap().contains(EMPTY_WRITER_REASON));
                let row = LaneRow::project(&finished, LaneProjectionFacts::default());
                assert!(row.reason.as_deref().unwrap().contains(EMPTY_WRITER_REASON));
                assert_eq!(
                    read_sealed(&attempt, "result.md", RESULT_LIMIT)?.as_deref(),
                    Some(EMPTY_WRITER_RESULT.as_bytes())
                );
                fixture.git(&[
                    "merge-base",
                    "--is-ancestor",
                    &committed_head,
                    launch.branch.as_str(),
                ])?;
                assert_ne!(committed_head, launch.base.to_string());
            } else {
                assert_eq!(*actual, Outcome::Failed);
            }
        }
        Ok(())
    }

    #[test]
    fn writer_token_budget_stop_commits_uncommitted_work_and_publishes_branch()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        for case in [
            BudgetStopCase::Commit,
            BudgetStopCase::Native { dirty: false },
            BudgetStopCase::Native { dirty: true },
        ] {
            budget_stop_case(case)?;
        }
        Ok(())
    }

    #[test]
    fn writer_token_budget_commit_failure_preserves_timeout()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        budget_stop_case(BudgetStopCase::CommitFailure)
    }

    #[test]
    fn writer_token_budget_commit_requires_absent_custody()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        budget_stop_case(BudgetStopCase::CustodyPresent)
    }

    #[test]
    fn writer_token_budget_commit_ignores_lane_configured_clean_filters()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        budget_stop_case(BudgetStopCase::HostileFilter)
    }

    #[test]
    fn writer_token_budget_commit_refuses_symlinked_object_fanout()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        budget_stop_case(BudgetStopCase::SymlinkObjects)
    }

    #[test]
    fn writer_token_budget_commit_leaves_a_cherry_pick_uncommitted()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        budget_stop_case(BudgetStopCase::InProgress("cherry-pick"))
    }

    #[test]
    fn writer_token_budget_commit_leaves_a_rebase_uncommitted()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        budget_stop_case(BudgetStopCase::InProgress("rebase"))
    }

    #[test]
    fn writer_token_budget_commit_leaves_a_revert_uncommitted()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        budget_stop_case(BudgetStopCase::InProgress("revert"))
    }

    #[test]
    fn writer_token_budget_commit_leaves_an_apply_rebase_uncommitted()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        budget_stop_case(BudgetStopCase::InProgress("rebase-apply"))
    }

    /// A writer the host stopped at its context handoff threshold, with an
    /// edit left uncommitted and a progress handoff recorded, settles as a
    /// declared stop whose result carries that handoff. The graph restarts
    /// such a stop fresh on the same worktree with the handoff in its brief
    /// (`no_work`); the host commits nothing and the edit stays in place.
    #[test]
    fn writer_context_handoff_stop_settles_as_a_declared_stop_with_its_handoff()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use crate::delivery_tests::{PrepareFixture, registered_landed_writer_fixture};
        let fixture = PrepareFixture::new()?;
        let execution = registered_landed_writer_fixture(&fixture)?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&execution, &call)?;
        let mut run = input();
        run.request.task.writer = Some(fixture.writer.clone());
        store.create(&run, &call)?;
        let id = run.attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let record = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let scope = RunStore::scope(&record);
        let attempt = store.attempt_dir(id, false)?;
        let mut launch = crate::prepare_writer(
            &fixture.writer,
            execution.state_root.as_path(),
            execution.project_id,
        )?;
        launch.snapshot(execution.state_root.as_path(), &attempt, scope)?;
        let candidate = store.temporary_path(id, &call)?.join("candidate-0");
        std::fs::create_dir_all(&candidate)?;
        let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
        crate::objects::prepare_bound(
            &git,
            launch.worktree.repo(),
            &attempt,
            &candidate,
            launch.branch.as_str(),
            launch.base,
        )?;
        let pin = format!("refs/provenant/import/{}", launch.base);
        fixture.git(&["update-ref", &pin, &launch.base.to_string()])?;
        let (directory, binding) = launch.custody.take().ok_or("writer custody")?;
        let lease = provenant_supervisor::fixture::acquire_writer_with_binding(
            directory.try_clone()?,
            scope,
            binding,
            call.lock_budget(),
            &cancel,
            &clock,
        )?;
        drop(provenant_supervisor::fixture::bind_writer_attempt(
            lease, &attempt,
        )?);
        std::fs::write(
            launch.worktree.repo().root_path().join("context-work.txt"),
            "uncommitted work before the context handoff\n",
        )?;
        // The lane's latest progress handoff, as settlement snapshots it.
        let progress = serde_json::json!({
            "schema_version": 2,
            "status": "stopped_early",
            "note": "Parser done; renderer next",
            "remaining": ["Finish renderer"],
            "decisions": ["Keep parser API"],
            "verify": [],
        });
        crate::artifacts::seal(
            &attempt,
            provenant_records::AttemptFile::Handoff.name(),
            &serde_json::to_vec(&progress)?,
            provenant_records::handoff::HANDOFF_BYTES as u64,
        )?;
        let mut observed = crate::tests::exit();
        observed.scope = scope;
        observed.stop_reason = Some(StopReason::ContextHandoff);
        crate::dispatch_tests::seal_exit(&store, &observed)?;
        crate::artifacts::seal(
            &attempt,
            "launch.json",
            &crate::db::bytes(
                &serde_json::json!({"schema_version":1,"scope":scope,"requester":execution.actor_id}),
            )?,
            RECORD_LIMIT,
        )?;
        // A stopped provider leaves no final answer.
        let fake = crate::dispatch_tests::Fake::with_observed(Some(route()), Some(Outcome::Failed));
        let finished = store.finish(id, &attempt, &observed, &fake, &call)?;
        let AttemptState::Terminal {
            extra,
            outcome,
            reason,
            ..
        } = &finished.state
        else {
            panic!("attempt must be terminal");
        };
        assert_eq!(*outcome, Outcome::ReviewRequired, "reason: {reason:?}");
        assert_eq!(
            extra.get("terminal_cause"),
            Some(&serde_json::json!(TerminalCause::ContextHandoff))
        );
        // Only a declared stop: the owner maps this to the graph's restart.
        assert_eq!(
            finished.partial_signals,
            vec![provenant_records::run::PartialSignal::DeclaredStop]
        );
        let result =
            String::from_utf8(read_sealed(&attempt, "result.md", RESULT_LIMIT)?.ok_or("result")?)?;
        let handoff =
            provenant_records::handoff::Handoff::from_result(&result)?.ok_or("result handoff")?;
        assert_eq!(handoff.note, "Parser done; renderer next");
        assert_eq!(handoff.remaining, vec!["Finish renderer".to_owned()]);
        assert!(handoff.unfinished());
        // Nothing was committed for the lane; its edit stays for the restart.
        assert!(read_sealed(&attempt, "budget-commit.json", RECORD_LIMIT)?.is_none());
        assert_eq!(
            std::fs::read_to_string(fixture.writer.worktree.join("context-work.txt"))?,
            "uncommitted work before the context handoff\n"
        );
        Ok(())
    }

    /// A Claude writer whose launch-time token copy was lost mid-run (the
    /// host renewed the account login) settles as a declared stop on its
    /// latest handoff; its uncommitted edit stays, and the reason stays the
    /// lane-token one, not a login.
    #[test]
    fn writer_that_lost_its_lane_token_settles_on_its_handoff()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        for host_outcome in [Outcome::AuthRequired, Outcome::Lost, Outcome::Unsettled] {
            use crate::delivery_tests::{PrepareFixture, registered_landed_writer_fixture};
            let fixture = PrepareFixture::new()?;
            let execution = registered_landed_writer_fixture(&fixture)?;
            let (clock, cancel) = (clock(), CancelToken::new());
            let call = call(&clock, &cancel);
            let store = RunStore::open(&execution, &call)?;
            let mut run = input();
            run.request.task.writer = Some(fixture.writer.clone());
            store.create(&run, &call)?;
            let id = run.attempt_id;
            store.mark_starting(
                &store.record(id, &call)?,
                &route(),
                &[],
                &Default::default(),
                None,
                &call,
            )?;
            let record = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
            let scope = RunStore::scope(&record);
            let attempt = store.attempt_dir(id, false)?;
            let mut launch = crate::prepare_writer(
                &fixture.writer,
                execution.state_root.as_path(),
                execution.project_id,
            )?;
            launch.snapshot(execution.state_root.as_path(), &attempt, scope)?;
            let candidate = store.temporary_path(id, &call)?.join("candidate-0");
            std::fs::create_dir_all(&candidate)?;
            let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
            crate::objects::prepare_bound(
                &git,
                launch.worktree.repo(),
                &attempt,
                &candidate,
                launch.branch.as_str(),
                launch.base,
            )?;
            let pin = format!("refs/provenant/import/{}", launch.base);
            fixture.git(&["update-ref", &pin, &launch.base.to_string()])?;
            let (directory, binding) = launch.custody.take().ok_or("writer custody")?;
            let lease = provenant_supervisor::fixture::acquire_writer_with_binding(
                directory.try_clone()?,
                scope,
                binding,
                call.lock_budget(),
                &cancel,
                &clock,
            )?;
            drop(provenant_supervisor::fixture::bind_writer_attempt(
                lease, &attempt,
            )?);
            std::fs::write(
                launch.worktree.repo().root_path().join("token-work.txt"),
                "uncommitted work before the token was lost\n",
            )?;
            let progress = serde_json::json!({
                "schema_version": 2,
                "status": "stopped_early",
                "note": "Parser done; renderer next",
                "remaining": ["Finish renderer"],
                "decisions": [],
                "verify": [],
            });
            crate::artifacts::seal(
                &attempt,
                provenant_records::AttemptFile::Handoff.name(),
                &serde_json::to_vec(&progress)?,
                provenant_records::handoff::HANDOFF_BYTES as u64,
            )?;
            let mut observed = crate::tests::exit();
            observed.scope = scope;
            crate::dispatch_tests::seal_exit(&store, &observed)?;
            crate::artifacts::seal(
                &attempt,
                "launch.json",
                &crate::db::bytes(
                    &serde_json::json!({"schema_version":1,"scope":scope,"requester":execution.actor_id}),
                )?,
                RECORD_LIMIT,
            )?;
            // Exercise the host's uncertain-cancellation transition: an
            // unsolicited provider cancellation becomes Unsettled with its
            // required custody diagnostic, rather than an invalid bare outcome.
            let provider_outcome = if host_outcome == Outcome::Unsettled {
                Outcome::Cancelled
            } else {
                host_outcome
            };
            let mut fake =
                crate::dispatch_tests::Fake::with_observed(Some(route()), Some(provider_outcome));
            fake.terminal_cause = Some(TerminalCause::LaneTokenLost);
            let finished = store.finish(id, &attempt, &observed, &fake, &call)?;
            let AttemptState::Terminal {
                extra,
                outcome,
                reason,
                ..
            } = &finished.state
            else {
                panic!("attempt must be terminal");
            };
            let expected_outcome = if host_outcome == Outcome::AuthRequired {
                Outcome::ReviewRequired
            } else {
                host_outcome
            };
            assert_eq!(*outcome, expected_outcome, "reason: {reason:?}");
            if host_outcome == Outcome::AuthRequired {
                assert_eq!(
                    extra.get("terminal_cause"),
                    Some(&serde_json::json!(TerminalCause::LaneTokenLost))
                );
            }
            let stop: provenant_records::Stop = serde_json::from_value(extra["stop"].clone())?;
            let expected_stop = match host_outcome {
                Outcome::Lost => provenant_records::StopCause::Lost,
                Outcome::Unsettled => provenant_records::StopCause::Unsettled,
                _ => provenant_records::StopCause::LaneTokenLost,
            };
            assert_eq!(stop.cause(), expected_stop);
            let handoff = stop.handoff().ok_or("sealed stop handoff")?;
            assert_eq!(handoff.note, "Parser done; renderer next");
            if host_outcome == Outcome::AuthRequired {
                let result = String::from_utf8(
                    read_sealed(&attempt, "result.md", RESULT_LIMIT)?.ok_or("result")?,
                )?;
                let rendered = provenant_records::handoff::Handoff::from_result(&result)?
                    .ok_or("result handoff")?;
                assert_eq!(rendered.note, handoff.note);
            }
            assert_eq!(
                std::fs::read_to_string(fixture.writer.worktree.join("token-work.txt"))?,
                "uncommitted work before the token was lost\n"
            );
        }
        Ok(())
    }

    #[test]
    fn a_context_handoff_keeps_an_unfinished_handoff_and_never_reports_done() {
        use provenant_records::handoff::{Handoff, HandoffStatus};
        let missing = context_handoff(None);
        assert_eq!(missing.status, HandoffStatus::StoppedEarly);
        assert!(missing.note.contains("context handoff threshold"));
        let done = Handoff {
            status: HandoffStatus::Done,
            note: "claimed done".into(),
            ..missing.clone()
        };
        let kept = context_handoff(Some(done));
        assert_eq!(kept.status, HandoffStatus::StoppedEarly);
        assert_eq!(kept.note, "claimed done");
    }

    #[test]
    fn budget_wrap_up_resumes_once_with_remaining_grace_and_natural_completion_wins()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        budget_stop_case(BudgetStopCase::WrapUp)?;
        budget_stop_case(BudgetStopCase::Natural)?;
        budget_stop_case(BudgetStopCase::Clean)?;
        budget_stop_case(BudgetStopCase::WrapUpRefused)?;
        budget_stop_case(BudgetStopCase::WrapUpBlocked)?;
        budget_stop_case(BudgetStopCase::NativeStaged)
    }

    #[derive(Clone, Copy)]
    enum BudgetStopCase {
        /// An operation Git left in progress.
        InProgress(&'static str),
        Commit,
        WrapUp,
        WrapUpRefused,
        WrapUpBlocked,
        NativeStaged,
        Natural,
        Clean,
        Native {
            dirty: bool,
        },
        CommitFailure,
        CustodyPresent,
        HostileFilter,
        SymlinkObjects,
    }

    fn budget_stop_case(
        case: BudgetStopCase,
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        use crate::delivery_tests::{PrepareFixture, registered_landed_writer_fixture};
        let fixture = PrepareFixture::new()?;
        let execution = registered_landed_writer_fixture(&fixture)?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&execution, &call)?;
        let mut run = input();
        run.request.task.writer = Some(fixture.writer.clone());
        store.create(&run, &call)?;
        let id = run.attempt_id;
        let limits = if matches!(
            case,
            BudgetStopCase::WrapUp
                | BudgetStopCase::WrapUpRefused
                | BudgetStopCase::WrapUpBlocked
                | BudgetStopCase::Natural
                | BudgetStopCase::Clean
        ) {
            provenant_records::LaneLimits {
                token_budget: Some(provenant_records::Limit::new(
                    1_000_000,
                    provenant_records::LimitSource::Default,
                    provenant_records::LimitStrength::Default,
                )),
                budget_policy: Some(provenant_records::BudgetPolicy::default()),
                ..Default::default()
            }
        } else {
            Default::default()
        };
        let mut applied_route = route();
        if limits.budget_policy.is_some() {
            applied_route.provider = Some("codex".into());
        }
        store.mark_starting_with_priority(
            &store.record(id, &call)?,
            &applied_route,
            &[],
            &Default::default(),
            None,
            None,
            &limits,
            &call,
        )?;
        let record = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let scope = RunStore::scope(&record);
        let attempt = store.attempt_dir(id, false)?;
        let mut launch = crate::prepare_writer(
            &fixture.writer,
            execution.state_root.as_path(),
            execution.project_id,
        )?;
        launch.snapshot(execution.state_root.as_path(), &attempt, scope)?;
        let candidate = store.temporary_path(id, &call)?.join("candidate-0");
        std::fs::create_dir_all(&candidate)?;
        let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
        if matches!(case, BudgetStopCase::NativeStaged) {
            let branch = launch
                .branch
                .as_str()
                .strip_prefix("refs/heads/")
                .ok_or("local branch")?;
            fixture.lane_git(&["push", "--set-upstream", "origin", branch])?;
        }
        crate::objects::prepare_bound(
            &git,
            launch.worktree.repo(),
            &attempt,
            &candidate,
            launch.branch.as_str(),
            launch.base,
        )?;
        let pin = format!("refs/provenant/import/{}", launch.base);
        fixture.git(&["update-ref", &pin, &launch.base.to_string()])?;
        let (directory, binding) = launch.custody.take().ok_or("writer custody")?;
        let lease = provenant_supervisor::fixture::acquire_writer_with_binding(
            directory.try_clone()?,
            scope,
            binding,
            call.lock_budget(),
            &cancel,
            &clock,
        )?;
        drop(provenant_supervisor::fixture::bind_writer_attempt(
            lease, &attempt,
        )?);

        // Leave an uncommitted file in the lane's worktree.
        std::fs::write(
            launch.worktree.repo().root_path().join("budget-work.txt"),
            "uncommitted work before budget stop\n",
        )?;

        if matches!(case, BudgetStopCase::NativeStaged) {
            // Match the launcher's private upstream configuration so a false
            // clean status cannot be hidden by a missing-upstream error.
            let source = crate::objects::bound_source(&attempt)?.ok_or("private source")?;
            let refs = source.refs.current_path()?;
            for (key, value) in [
                (
                    "remote.origin.fetch".to_owned(),
                    "+refs/heads/*:refs/remotes/origin/*".to_owned(),
                ),
                (
                    format!(
                        "branch.{}.remote",
                        launch
                            .branch
                            .as_str()
                            .strip_prefix("refs/heads/")
                            .ok_or("local branch")?
                    ),
                    "origin".to_owned(),
                ),
                (
                    format!(
                        "branch.{}.merge",
                        launch
                            .branch
                            .as_str()
                            .strip_prefix("refs/heads/")
                            .ok_or("local branch")?
                    ),
                    launch.branch.as_str().to_owned(),
                ),
            ] {
                crate::objects::command(
                    &git,
                    &refs,
                    launch.worktree.repo().root_path(),
                    &["config".into(), key.into(), value.into()],
                    None,
                )?;
            }
            let objects = source.objects.current_path()?;
            let alternate = launch.worktree.repo().common_git_dir().join("objects");
            crate::objects::execute_bounded(
                &git,
                &refs,
                launch.worktree.repo().root_path(),
                &[
                    "update-ref".into(),
                    format!(
                        "refs/remotes/origin/{}",
                        launch
                            .branch
                            .as_str()
                            .strip_prefix("refs/heads/")
                            .ok_or("local branch")?
                    )
                    .into(),
                    launch.base.to_string().into(),
                ],
                None,
                crate::objects::Execution {
                    object_directory: Some(&objects),
                    alternate_directory: Some(&alternate),
                    ..Default::default()
                },
            )?;
            fixture.lane_git(&["add", "budget-work.txt"])?;
            std::fs::remove_file(fixture.writer.worktree.join("budget-work.txt"))?;
            assert!(
                !launch.budget_clean_point(&attempt)?,
                "native staged edits are dirty even before HEAD advances"
            );
            return Ok(());
        }

        let native_head = if let Some(dirty) = match case {
            BudgetStopCase::Native { dirty } => Some(dirty),
            BudgetStopCase::Clean => Some(false),
            _ => None,
        } {
            fixture.lane_git(&["add", "budget-work.txt"])?;
            fixture.lane_git(&["commit", "-m", "Native lane work"])?;
            let head =
                crate::objects::resolve_ref(&git, launch.worktree.repo(), launch.branch.as_str())?;
            if !dirty {
                // The clean-point contract uses locally recorded push evidence.
                // A real local bare remote exercises native Git's tracking ref.
                let branch = launch
                    .branch
                    .as_str()
                    .strip_prefix("refs/heads/")
                    .ok_or("local branch")?;
                fixture.lane_git(&["push", "--set-upstream", "origin", branch])?;
            }
            if dirty {
                std::fs::write(
                    fixture.writer.worktree.join("budget-work.txt"),
                    "additional work before budget stop\n",
                )?;
            }
            Some(head)
        } else {
            None
        };

        if native_head.is_some() {
            if matches!(
                case,
                BudgetStopCase::Clean | BudgetStopCase::Native { dirty: false }
            ) {
                assert!(launch.budget_clean_point(&attempt)?);
            } else {
                assert!(!launch.budget_clean_point(&attempt)?);
            }
        }

        let filter_sentinel = fixture
            .writer
            .project
            .parent()
            .ok_or("fixture root")?
            .join("filter-ran");
        if matches!(case, BudgetStopCase::HostileFilter) {
            let config = candidate.join("git-common/config");
            let mut bytes = std::fs::read_to_string(&config)?;
            bytes.push_str(&format!(
                "\n[filter \"poison\"]\n\tclean = touch {}; printf altered\n\tprocess = touch {}; exit 1\n\trequired = true\n",
                filter_sentinel.display(), filter_sentinel.display(),
            ));
            std::fs::write(config, bytes)?;
            std::fs::write(
                fixture.writer.worktree.join(".gitattributes"),
                "budget-work.txt filter=poison\n",
            )?;
        }
        if matches!(case, BudgetStopCase::SymlinkObjects) {
            let outside = fixture
                .writer
                .project
                .parent()
                .ok_or("fixture root")?
                .join("outside-objects");
            std::fs::create_dir(&outside)?;
            // Any new blob or tree object must encounter a poisoned fanout.
            for byte in 0..=255u8 {
                std::os::unix::fs::symlink(
                    &outside,
                    candidate.join("git-objects").join(format!("{byte:02x}")),
                )?;
            }
            let recovery =
                custody::acquire_recovery(directory, scope, call.lock_budget(), &cancel, &clock)?;
            assert_eq!(recovery.observe(), custody::RecoveryState::Absent);
            let mut settlement = crate::settlement::Guard::claim(&attempt, scope, &clock)?
                .ok_or("settlement claim")?;
            let private_index = std::fs::read(candidate.join("git-common/index"))?;
            let source = crate::objects::bound_source(&attempt)?.ok_or("private source")?;
            let private_head = crate::objects::private_tip(&source, launch.branch.as_str())?;
            let result = launch.terminal(
                execution.state_root.as_path(),
                &attempt,
                &recovery,
                &mut settlement,
                true,
            );
            assert!(result.is_err(), "unsafe object store must remain unsettled");
            let disposition: serde_json::Value = serde_json::from_slice(
                &read_sealed(&attempt, "budget-commit.json", RECORD_LIMIT)?
                    .ok_or("budget commit disposition")?,
            )?;
            let note = disposition["note"].as_str().ok_or("commit note")?;
            assert!(
                note.contains("budget commit failed:") && note.contains("symlink"),
                "{note}"
            );
            assert_eq!(std::fs::read_dir(&outside)?.count(), 0);
            assert_eq!(
                std::fs::read(candidate.join("git-common/index"))?,
                private_index
            );
            assert_eq!(
                crate::objects::private_tip(&source, launch.branch.as_str())?,
                private_head
            );
            assert_eq!(
                crate::objects::resolve_ref(&git, launch.worktree.repo(), launch.branch.as_str())?,
                launch.base
            );
            return Ok(());
        }
        if matches!(case, BudgetStopCase::CustodyPresent) {
            use provenant_supervisor::census::{ProcessObservation, current_process};
            let ProcessObservation::Present(mut guardian) = current_process() else {
                return Err("current process must be observable".into());
            };
            // Model a reservation whose guardian identity cannot be proved
            // quiescent. Native group/session mismatch makes it unknown.
            guardian.identity.group_id = guardian.identity.pid;
            guardian.identity.session_id = guardian.identity.pid;
            let path = directory
                .current_path()?
                .join(id.to_string())
                .join("reservation.json");
            let mut reservation: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path)?)?;
            reservation["launch_state"] = "spawned".into();
            reservation["unresolved_cleanup"] = true.into();
            reservation["boot"] = guardian.identity.boot.clone().into();
            reservation["guardian"] = serde_json::to_value(&guardian.identity)?;
            reservation["provider"] = serde_json::to_value(&guardian.identity)?;
            std::fs::write(&path, serde_json::to_vec(&reservation)?)?;
            let recovery =
                custody::acquire_recovery(directory, scope, call.lock_budget(), &cancel, &clock)?;
            assert_ne!(recovery.observe(), custody::RecoveryState::Absent);
            let mut settlement = crate::settlement::Guard::claim(&attempt, scope, &clock)?
                .ok_or("settlement claim")?;
            let private_index = std::fs::read(candidate.join("git-common/index"))?;
            let private_head = crate::objects::private_tip(
                &crate::objects::bound_source(&attempt)?.ok_or("private source")?,
                launch.branch.as_str(),
            )?;
            let result = launch.terminal(
                execution.state_root.as_path(),
                &attempt,
                &recovery,
                &mut settlement,
                true,
            );
            assert!(matches!(
                result,
                Err(RunError::Validation(
                    "writer object import requires absent custody"
                ))
            ));
            assert_eq!(
                std::fs::read(candidate.join("git-common/index"))?,
                private_index
            );
            assert_eq!(
                crate::objects::private_tip(
                    &crate::objects::bound_source(&attempt)?.ok_or("private source")?,
                    launch.branch.as_str()
                )?,
                private_head
            );
            assert_eq!(
                crate::objects::resolve_ref(&git, launch.worktree.repo(), launch.branch.as_str())?,
                launch.base
            );
            assert!(read_sealed(&attempt, "budget-commit.json", RECORD_LIMIT)?.is_none());
            return Ok(());
        }
        if matches!(case, BudgetStopCase::CommitFailure) {
            std::fs::write(
                candidate.join("git-common/index.lock"),
                b"existing Git lock",
            )?;
        }
        if let BudgetStopCase::InProgress(operation) = case {
            let git_dir = candidate.join("git-common");
            match operation {
                "rebase" => std::fs::create_dir(git_dir.join("rebase-merge"))?,
                "rebase-apply" => std::fs::create_dir(git_dir.join("rebase-apply"))?,
                operation => std::fs::write(
                    git_dir.join(if operation == "revert" {
                        "REVERT_HEAD"
                    } else {
                        "CHERRY_PICK_HEAD"
                    }),
                    format!("{}\n", launch.base),
                )?,
            }
        }
        let mut observed = crate::tests::exit();
        observed.scope = scope;
        observed.stop_reason = Some(StopReason::TokenBudget);
        if matches!(case, BudgetStopCase::Clean) {
            crate::artifacts::seal(
                &attempt,
                provenant_records::AttemptFile::BudgetCleanPoint.name(),
                b"{\"schema_version\":1,\"clean\":true}",
                RECORD_LIMIT,
            )?;
        }
        crate::dispatch_tests::seal_exit(&store, &observed)?;
        crate::artifacts::seal(
            &attempt,
            "launch.json",
            &crate::db::bytes(
                &serde_json::json!({"schema_version":1,"scope":scope,"requester":execution.actor_id}),
            )?,
            RECORD_LIMIT,
        )?;
        let mut fake = crate::dispatch_tests::Fake::with_observed(
            Some(applied_route.clone()),
            Some(if matches!(case, BudgetStopCase::Natural) {
                Outcome::Succeeded
            } else {
                Outcome::TimedOut
            }),
        );
        if matches!(case, BudgetStopCase::WrapUpBlocked) {
            let mut blocked = stopped_handoff(None, "Owner decision required before continuing.");
            blocked.status = provenant_records::handoff::HandoffStatus::Blocked;
            fake.answer = Some(blocked.fenced());
        }
        if matches!(case, BudgetStopCase::Natural) {
            fake.answer = Some("Completed the accepted work and pushed its branch.".into());
        }
        if matches!(
            case,
            BudgetStopCase::WrapUp
                | BudgetStopCase::WrapUpRefused
                | BudgetStopCase::WrapUpBlocked
                | BudgetStopCase::Natural
                | BudgetStopCase::Clean
        ) {
            fake.codex_usage = Some(UsageRecord {
                extra: [("per_attempt".into(), true.into())].into(),
                uncached_input: Some(1_000_001),
                ..Default::default()
            });
        }
        if matches!(case, BudgetStopCase::WrapUpRefused) {
            crate::artifacts::seal(&attempt, "provider-state-expired.json", b"{}", RECORD_LIMIT)?;
        }
        let finished = store.finish(id, &attempt, &observed, &fake, &call)?;
        if matches!(
            case,
            BudgetStopCase::WrapUp
                | BudgetStopCase::WrapUpRefused
                | BudgetStopCase::WrapUpBlocked
                | BudgetStopCase::Natural
                | BudgetStopCase::Clean
        ) {
            assert!(read_sealed(&attempt, "budget-commit.json", RECORD_LIMIT)?.is_none());
            assert!(
                std::fs::read_to_string(fixture.writer.worktree.join("budget-work.txt"))?
                    .contains("uncommitted work")
            );
            if matches!(case, BudgetStopCase::Natural | BudgetStopCase::Clean) {
                assert!(!matches!(
                    finished.state,
                    AttemptState::Terminal {
                        outcome: Outcome::TimedOut,
                        ..
                    }
                ));
                assert!(store.prepare_budget_wrap_up(id, &call)?.is_none());
            } else if matches!(case, BudgetStopCase::WrapUpBlocked) {
                assert!(store.prepare_budget_wrap_up(id, &call)?.is_none());
                assert!(finished.superseded_by.is_none());
                let AttemptState::Terminal { extra, .. } = &finished.state else {
                    panic!("not terminal")
                };
                let stop: provenant_records::Stop =
                    serde_json::from_value(extra.get("stop").cloned().ok_or("stop")?)?;
                assert_eq!(stop.cause(), provenant_records::StopCause::Blocked);
                assert_eq!(
                    extra
                        .get("terminal_note")
                        .and_then(serde_json::Value::as_str),
                    Some("Owner decision required before continuing.")
                );
                assert_eq!(
                    stop.handoff().ok_or("blocked handoff")?.status,
                    provenant_records::handoff::HandoffStatus::Blocked
                );
            } else if matches!(case, BudgetStopCase::WrapUpRefused) {
                assert!(store.prepare_budget_wrap_up(id, &call)?.is_none());
                assert!(
                    read_sealed(
                        &attempt,
                        provenant_records::AttemptFile::BudgetWrapUpRefused.name(),
                        RECORD_LIMIT
                    )?
                    .is_some()
                );
                assert!(store.custody_released(&store.record(id, &call)?, &call)?);
            } else {
                assert!(matches!(
                    finished.state,
                    AttemptState::Terminal {
                        outcome: Outcome::ReviewRequired,
                        ..
                    }
                ));
                let successor = store
                    .prepare_budget_wrap_up(id, &call)?
                    .ok_or("wrap-up successor")?;
                assert_eq!(store.prepare_budget_wrap_up(id, &call)?, Some(successor));
                let request = store.request(successor, &call)?;
                assert_eq!(request.parent, Some(id));
                assert!(
                    request.fresh_continuation.is_none(),
                    "wrap-up must resume the provider session"
                );
                let task = &request.task;
                let (budget, policy) = crate::writer_budget_limits(
                    &provenant_confinement::run_config::RunConfig::default(),
                    "p",
                    &task.profile,
                    task.context.as_ref(),
                    None,
                );
                assert_eq!(budget.ok_or("remaining grace")?.value, 99_999);
                assert_eq!(policy.action, provenant_records::BudgetAction::Stop);
                assert!(store.record(id, &call)?.superseded_by.is_some());
            }
            return Ok(());
        }
        let AttemptState::Terminal {
            extra,
            outcome,
            reason,
            ..
        } = finished.state
        else {
            panic!("attempt must be terminal");
        };
        assert_eq!(outcome, Outcome::TimedOut, "reason: {reason:?}");
        assert_eq!(
            extra.get("terminal_cause"),
            Some(&serde_json::json!(TerminalCause::TokenBudget))
        );
        let reason_text = reason.expect("terminal attempt has reason");
        assert!(
            reason_text.contains("token budget reached: provider stopped after passing"),
            "reason: {reason_text}"
        );
        if let BudgetStopCase::InProgress(operation) = case {
            let operation = if operation == "rebase-apply" {
                "rebase"
            } else {
                operation
            };
            assert!(
                reason_text.contains(&format!(
                    "budget commit skipped: in-progress {operation} left uncommitted"
                )),
                "reason: {reason_text}"
            );
            assert_eq!(
                crate::objects::resolve_ref(&git, launch.worktree.repo(), launch.branch.as_str())?,
                launch.base
            );
            assert_eq!(
                std::fs::read_to_string(fixture.writer.worktree.join("budget-work.txt"))?,
                "uncommitted work before budget stop\n"
            );
            return Ok(());
        }
        if matches!(case, BudgetStopCase::CommitFailure) {
            assert!(
                reason_text.contains("budget commit failed:"),
                "reason: {reason_text}"
            );
            assert!(
                !reason_text.contains("work committed on its branch"),
                "reason: {reason_text}"
            );
            assert_eq!(
                crate::objects::resolve_ref(&git, launch.worktree.repo(), launch.branch.as_str())?,
                launch.base
            );
            assert_eq!(
                std::fs::read_to_string(fixture.writer.worktree.join("budget-work.txt"))?,
                "uncommitted work before budget stop\n"
            );
            let disposition: serde_json::Value = serde_json::from_slice(
                &read_sealed(&attempt, "budget-commit.json", RECORD_LIMIT)?
                    .ok_or("budget commit disposition")?,
            )?;
            assert!(
                disposition["note"]
                    .as_str()
                    .ok_or("commit note")?
                    .contains("budget commit failed:")
            );
            return Ok(());
        }
        if let Some(head) = native_head {
            let published =
                crate::objects::resolve_ref(&git, launch.worktree.repo(), launch.branch.as_str())?;
            fixture.lane_git(&[
                "merge-base",
                "--is-ancestor",
                &head.to_string(),
                &published.to_string(),
            ])?;
            assert_eq!(finished.head, Some(published));
            assert!(
                provenant_os::fixture::run_git(
                    &fixture.writer.worktree,
                    &["status", "--porcelain"]
                )?
                .is_empty()
            );
            if matches!(case, BudgetStopCase::Native { dirty: false }) {
                assert_eq!(
                    published, head,
                    "clean native work must not create a snapshot"
                );
                let disposition: serde_json::Value = serde_json::from_slice(
                    &read_sealed(&attempt, "budget-commit.json", RECORD_LIMIT)?
                        .ok_or("budget disposition")?,
                )?;
                assert!(disposition.get("commit").is_none());
                return Ok(());
            }
        }
        assert!(
            reason_text.contains("work committed on its branch"),
            "reason: {reason_text}"
        );
        // The newly committed file is now published on the lane branch.
        let published_tip =
            crate::objects::resolve_ref(&git, launch.worktree.repo(), launch.branch.as_str())?;
        assert_ne!(published_tip, launch.base);
        if matches!(case, BudgetStopCase::HostileFilter) {
            assert!(
                !filter_sentinel.exists(),
                "lane-configured filter executed on the host"
            );
            let blob = crate::objects::command(
                &git,
                launch.worktree.repo().common_git_dir(),
                launch.worktree.repo().root_path(),
                &[
                    "show".into(),
                    format!("{published_tip}:budget-work.txt").into(),
                ],
                None,
            )?;
            assert_eq!(blob, b"uncommitted work before budget stop\n");
        }

        let log = crate::objects::command(
            &git,
            launch.worktree.repo().common_git_dir(),
            launch.worktree.repo().root_path(),
            &[
                "log".into(),
                "-1".into(),
                "--pretty=%B".into(),
                published_tip.to_string().into(),
            ],
            None,
        )?;
        let log = String::from_utf8_lossy(&log);
        assert!(
            log.starts_with("wip: stopped on budget (") && log.contains(" files)\n"),
            "{log}"
        );
        assert!(log.contains("\nbudget-work.txt"), "{log}");
        let disposition: serde_json::Value = serde_json::from_slice(
            &read_sealed(&attempt, "budget-commit.json", RECORD_LIMIT)?.ok_or("budget commit")?,
        )?;
        assert_eq!(
            disposition["commit"].as_str(),
            Some(&published_tip.to_string()[..crate::writer::SHORT_SHA])
        );
        assert!(
            disposition["paths"]
                .as_array()
                .is_some_and(|paths| paths.iter().any(|path| path == "budget-work.txt"))
        );
        let placeholder = budget_stop_result(&disposition, "work committed on its branch");
        assert!(placeholder.contains("budget-work.txt"), "{placeholder}");
        Ok(())
    }

    #[test]
    fn settlement_import_cancellation_preserves_provider_exit_ordering()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        for delivered in [None, Some(false)] {
            use crate::delivery_tests::{PrepareFixture, registered_landed_writer_fixture};
            use std::ffi::OsStr;
            let fixture = PrepareFixture::new()?;
            let execution = registered_landed_writer_fixture(&fixture)?;
            let (clock, cancel) = (clock(), CancelToken::new());
            let call = call(&clock, &cancel);
            let store = RunStore::open(&execution, &call)?;
            let mut run = input();
            run.request.task.writer = Some(fixture.writer.clone());
            store.create(&run, &call)?;
            let id = run.attempt_id;
            store.mark_starting(
                &store.record(id, &call)?,
                &route(),
                &[],
                &Default::default(),
                None,
                &call,
            )?;
            let record = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
            let scope = RunStore::scope(&record);
            let attempt = store.attempt_dir(id, false)?;
            let mut launch = crate::prepare_writer(
                &fixture.writer,
                execution.state_root.as_path(),
                execution.project_id,
            )?;
            launch.snapshot(execution.state_root.as_path(), &attempt, scope)?;
            let candidate = store.temporary_path(id, &call)?.join("candidate-0");
            std::fs::create_dir_all(&candidate)?;
            let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
            crate::objects::prepare_bound(
                &git,
                launch.worktree.repo(),
                &attempt,
                &candidate,
                launch.branch.as_str(),
                launch.base,
            )?;
            let missing = "a".repeat(40);
            std::fs::write(
                candidate.join("git-common").join(launch.branch.as_str()),
                format!("{missing}\n"),
            )?;
            let (directory, binding) = launch.custody.take().ok_or("writer custody")?;
            let lease = provenant_supervisor::fixture::acquire_writer_with_binding(
                directory,
                scope,
                binding,
                call.lock_budget(),
                &cancel,
                &clock,
            )?;
            let mut recovery = provenant_supervisor::fixture::bind_writer_attempt(lease, &attempt)?
                .into_recovery()?;
            recovery.prepare_settlement()?;
            let mut observed = crate::tests::exit();
            observed.scope = scope;
            crate::dispatch_tests::seal_exit(&store, &observed)?;

            observed.cancel_delivered_before_exit = delivered;
            // Replace the fixture's exit before validation; ordering remains a fact.
            attempt.unlink(OsStr::new("process-exit.json"))?;
            crate::dispatch_tests::seal_exit(&store, &observed)?;
            crate::artifacts::seal(
                &attempt,
                "cancel.json",
                &crate::db::bytes(&serde_json::json!({
                    "schema_version": 1, "scope": scope, "requester": execution.actor_id,
                    "request_id": "import-cancel", "reason": StopReason::ExplicitCancellation
                }))?,
                RECORD_LIMIT,
            )?;
            let record = store.finish_in_custody(
                id,
                &attempt,
                &observed,
                Completion::Recovered(Some(&recovery)),
                &crate::dispatch_tests::Fake::default(),
                &call,
            )?;
            assert!(
                matches!(record.record.state, AttemptState::Terminal {outcome: Outcome::Cancelled, reason: Some(ref reason), ..} if reason == "cancelled during Git import")
            );
            assert_eq!(
                record.record.terminal_evidence,
                Some(TerminalEvidence::ImportCancellation)
            );
            assert_eq!(
                record.record.cancellation_disposition,
                Some(CancellationDisposition::ImportEffective)
            );
            assert_eq!(observed.cancel_delivered_before_exit, delivered);
        }
        Ok(())
    }

    #[test]
    fn never_spawned_exit_survives_cancel_during_import()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        for (stop_reason, expected, cancel_first) in [
            (None, Outcome::Rejected, true),
            (Some(StopReason::WallDeadline), Outcome::TimedOut, false),
        ] {
            use crate::delivery_tests::{PrepareFixture, registered_landed_writer_fixture};
            let fixture = PrepareFixture::new()?;
            let execution = registered_landed_writer_fixture(&fixture)?;
            let (clock, cancel) = (clock(), CancelToken::new());
            let call = call(&clock, &cancel);
            let store = RunStore::open(&execution, &call)?;
            let mut run = input();
            run.request.task.writer = Some(fixture.writer.clone());
            store.create(&run, &call)?;
            let id = run.attempt_id;
            store.mark_starting(
                &store.record(id, &call)?,
                &route(),
                &[],
                &Default::default(),
                None,
                &call,
            )?;
            let record = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
            let scope = RunStore::scope(&record);
            let attempt = store.attempt_dir(id, false)?;
            let mut launch = crate::prepare_writer(
                &fixture.writer,
                execution.state_root.as_path(),
                execution.project_id,
            )?;
            launch.snapshot(execution.state_root.as_path(), &attempt, scope)?;
            let candidate = store.temporary_path(id, &call)?.join("candidate-0");
            std::fs::create_dir_all(&candidate)?;
            let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
            crate::objects::prepare_bound(
                &git,
                launch.worktree.repo(),
                &attempt,
                &candidate,
                launch.branch.as_str(),
                launch.base,
            )?;
            let missing = "a".repeat(40);
            std::fs::write(
                candidate.join("git-common").join(launch.branch.as_str()),
                format!("{missing}\n"),
            )?;
            let (directory, binding) = launch.custody.take().ok_or("writer custody")?;
            let lease = provenant_supervisor::fixture::acquire_writer_with_binding(
                directory,
                scope,
                binding,
                call.lock_budget(),
                &cancel,
                &clock,
            )?;
            drop(provenant_supervisor::fixture::bind_writer_attempt(
                lease, &attempt,
            )?);
            let mut observed = crate::tests::exit();
            observed.scope = scope;
            observed.never_spawned = true;
            observed.termination = None;
            observed.settlement_evidence = custody::SettlementEvidence::NeverSpawned;
            observed.stop_reason = stop_reason;
            crate::dispatch_tests::seal_exit(&store, &observed)?;
            crate::artifacts::seal(
                &attempt,
                "launch.json",
                &crate::db::bytes(&serde_json::json!({
                    "schema_version": 1, "scope": scope, "requester": execution.actor_id
                }))?,
                RECORD_LIMIT,
            )?;
            if cancel_first {
                assert!(matches!(
                    store.cancel(id, "stop", &call)?,
                    crate::CancelReceipt::Requested | crate::CancelReceipt::AlreadyRequested
                ));
            } else {
                crate::artifacts::seal(
                    &attempt,
                    "cancel.json",
                    &crate::db::bytes(&serde_json::json!({
                        "schema_version": 1, "scope": scope, "requester": execution.actor_id,
                        "request_id": "import-cancel", "reason": StopReason::ExplicitCancellation
                    }))?,
                    RECORD_LIMIT,
                )?;
            }
            let found = store.reconcile_with_observer(
                run.attempt_id,
                &crate::dispatch_tests::Fake::default(),
                &call,
                &provenant_supervisor::census::SystemProcessObserver,
            )?;
            assert_eq!(found, vec![(id, crate::Reconciled::Finished)]);
            let record = store.record(id, &call)?;
            assert!(
                matches!(record.state, AttemptState::Terminal { outcome, .. } if outcome == expected)
            );
            assert_eq!(
                record.terminal_evidence,
                Some(TerminalEvidence::ProcessExit)
            );
            assert_eq!(record.provider_launched, Some(false));
            assert!(read_sealed(&attempt, "import-cancelled.json", RECORD_LIMIT)?.is_none());
        }
        Ok(())
    }

    #[test]
    fn settlement_import_fault_seals_retains_fence_and_resumes_without_provider()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        settlement_fault_modes(&["normal", "orphan", "manifest_conflict", "archived_fault"])
    }

    #[test]
    fn reconcile_settles_failed_import_once_and_keeps_evidence()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        settlement_fault_modes(&["reconcile_failed", "reconcile_new", "reconcile_reboot"])
    }

    #[test]
    fn retained_import_excludes_writer_after_lock_release()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        settlement_fault_modes(&["release_boundary"])
    }

    #[test]
    fn settlement_publish_advanced_branch_clears_fence()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        settlement_fault_modes(&["publish_advanced"])
    }

    #[test]
    fn settlement_publish_diverged_branch_reports_error()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        settlement_fault_modes(&["publish_diverged"])
    }

    fn settlement_fault_modes(
        modes: &[&str],
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        for &mode in modes {
            use crate::delivery_tests::{PrepareFixture, registered_landed_writer_fixture};
            use std::ffi::OsStr;
            let fixture = PrepareFixture::new()?;
            let execution = registered_landed_writer_fixture(&fixture)?;
            let (clock, cancel) = (clock(), CancelToken::new());
            let call = call(&clock, &cancel);
            let store = RunStore::open(&execution, &call)?;
            let mut run = input();
            run.request.task.writer = Some(fixture.writer.clone());
            store.create(&run, &call)?;
            let id = run.attempt_id;
            store.mark_starting(
                &store.record(id, &call)?,
                &route(),
                &[],
                &Default::default(),
                None,
                &call,
            )?;
            let record = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
            let scope = RunStore::scope(&record);
            let attempt = store.attempt_dir(id, false)?;
            let mut launch = crate::prepare_writer(
                &fixture.writer,
                execution.state_root.as_path(),
                execution.project_id,
            )?;
            launch.snapshot(execution.state_root.as_path(), &attempt, scope)?;
            let candidate = store.temporary_path(id, &call)?.join("candidate-0");
            std::fs::create_dir_all(&candidate)?;
            let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
            crate::objects::prepare_bound(
                &git,
                launch.worktree.repo(),
                &attempt,
                &candidate,
                launch.branch.as_str(),
                launch.base,
            )?;
            let retained_commit = if mode.starts_with("reconcile_") {
                // Make a real commit that exists only in the lane's private objects.
                let tree = crate::objects::command(
                    &git,
                    launch.worktree.repo().common_git_dir(),
                    launch.worktree.path(),
                    &[
                        "rev-parse".into(),
                        format!("{}^{{tree}}", launch.base).into(),
                    ],
                    None,
                )?;
                let tree = std::str::from_utf8(&tree)?.trim();
                let output = provenant_os::fixture::git_command_at(&git)
                    .arg("--git-dir")
                    .arg(candidate.join("git-common"))
                    .args([
                        "commit-tree",
                        tree,
                        "-p",
                        &launch.base.to_string(),
                        "-m",
                        "Retained lane commit",
                    ])
                    .current_dir(launch.worktree.path())
                    .env("GIT_OBJECT_DIRECTORY", candidate.join("git-objects"))
                    .env(
                        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
                        launch.worktree.repo().common_git_dir().join("objects"),
                    )
                    .output()?;
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let raw = output.stdout;
                let head = provenant_core::GitOid::parse(std::str::from_utf8(&raw)?.trim())?;
                std::fs::write(
                    candidate.join("git-common").join(launch.branch.as_str()),
                    format!("{head}\n"),
                )?;
                let oid = head.to_string();
                let path = candidate
                    .join("git-objects")
                    .join(&oid[..2])
                    .join(&oid[2..]);
                Some((head, path.clone(), std::fs::read(path)?))
            } else {
                None
            };
            let mut published_head = None;
            if mode.starts_with("publish_") {
                fixture.commit(&fixture.writer.worktree, "Lane head")?;
                let head = crate::objects::resolve_ref(
                    &git,
                    launch.worktree.repo(),
                    launch.branch.as_str(),
                )?;
                published_head = Some(head);
                std::fs::write(
                    candidate.join("git-common/refs/heads/lane"),
                    format!("{head}\n"),
                )?;
                fixture.lane_git(&["reset", "--hard", &launch.base.to_string()])?;
                fixture.commit(&fixture.writer.worktree, "Competing branch")?;
            }
            let (directory, binding) = launch.custody.take().ok_or("writer custody")?;
            let lease = provenant_supervisor::fixture::acquire_writer_with_binding(
                directory,
                scope,
                binding,
                call.lock_budget(),
                &cancel,
                &clock,
            )?;
            let mut recovery = provenant_supervisor::fixture::bind_writer_attempt(lease, &attempt)?
                .into_recovery()?;
            recovery.prepare_settlement()?;
            let mut observed = crate::tests::exit();
            observed.scope = scope;
            crate::dispatch_tests::seal_exit(&store, &observed)?;

            let providers = crate::dispatch_tests::Fake::default();
            crate::artifacts::seal(
                &attempt,
                "launch.json",
                &crate::db::bytes(&serde_json::json!({
                    "schema_version":1,"scope":scope,"requester":execution.actor_id
                }))?,
                RECORD_LIMIT,
            )?;
            if mode == "orphan" {
                store.retain_import_fence(&record)?;
                crate::artifacts::seal(
                    &attempt,
                    "terminal-effects.json",
                    &crate::db::bytes(&crate::store::TerminalEffects {
                        schema_version: 1,
                        scope,
                        effects: vec![(
                            SettlementEffect::GitImport,
                            EffectDisposition::Failed {
                                reason: "pre-crash import failure".into(),
                                retryable: true,
                            },
                        )],
                    })?,
                    RECORD_LIMIT,
                )?;
                crate::artifacts::seal(&attempt, "result.md", b"orphan result", RESULT_LIMIT)?;
            }
            if matches!(mode, "manifest_conflict" | "archived_fault") {
                let archive = crate::artifacts::subdir(&attempt, OsStr::new("quarantine"), true)?;
                crate::artifacts::seal(
                    &archive,
                    "manifest.json",
                    b"conflicting writer manifest",
                    RECORD_LIMIT,
                )?;
            }
            if mode == "reconcile_new" {
                drop(recovery);
                crate::writer::FAIL_IMPORT.with(|fault| fault.set(true));
                let found = store.reconcile_with_observer(
                    run.attempt_id,
                    &providers,
                    &call,
                    &provenant_supervisor::census::SystemProcessObserver,
                );
                crate::writer::FAIL_IMPORT.with(|fault| fault.set(false));
                assert!(found?.contains(&(id, crate::Reconciled::Finished)));
                assert!(
                    matches!(store.record(id, &call)?.settlement_receipts.unwrap().git_import,
                    EffectDisposition::Failed { retryable: false, ref reason }
                    if reason.contains("injected Git import failure"))
                );
                assert!(store.writer_fences()?.is_empty());
                let (_, path, bytes) = retained_commit.as_ref().ok_or("private commit")?;
                assert_eq!(&std::fs::read(path)?, bytes);
                assert_eq!(
                    crate::objects::resolve_ref(
                        &git,
                        launch.worktree.repo(),
                        launch.branch.as_str()
                    )?,
                    launch.base
                );
                assert!(
                    store
                        .reconcile_with_observer(
                            run.attempt_id,
                            &providers,
                            &call,
                            &provenant_supervisor::census::SystemProcessObserver
                        )?
                        .is_empty()
                );
                continue;
            }
            crate::writer::FAIL_IMPORT.with(|fault| {
                fault.set(matches!(
                    mode,
                    "normal"
                        | "release_boundary"
                        | "archived_fault"
                        | "reconcile_failed"
                        | "reconcile_reboot"
                ))
            });
            let first = store.finish_in_custody(
                id,
                &attempt,
                &observed,
                Completion::Recovered(Some(&recovery)),
                &providers,
                &call,
            );
            crate::writer::FAIL_IMPORT.with(|fault| fault.set(false));
            let first = first?;
            if mode == "manifest_conflict" {
                assert!(matches!(
                    first.record.settlement_receipts.unwrap().manifest_conflict,
                    EffectDisposition::Failed {
                        retryable: true,
                        ..
                    }
                ));
                let archive = crate::artifacts::subdir(&attempt, OsStr::new("quarantine"), false)?;
                let path = execution
                    .state_root
                    .as_path()
                    .join(format!("attempts/{id}/quarantine"));
                drop(recovery);
                drop(store);
                let store = RunStore::open(&execution, &call)?;
                store.reconcile_terminal_effects(&store.record(id, &call)?, &call)?;
                assert_eq!(
                    read_sealed(&archive, "manifest.json", RECORD_LIMIT)?.unwrap(),
                    b"conflicting writer manifest"
                );
                assert!(matches!(
                    store
                        .record(id, &call)?
                        .settlement_receipts
                        .unwrap()
                        .manifest_conflict,
                    EffectDisposition::Failed {
                        retryable: true,
                        ..
                    }
                ));
                std::fs::rename(
                    path.join("manifest.json"),
                    path.join("manifest.conflict.json"),
                )?;
                store.reconcile_terminal_effects(&store.record(id, &call)?, &call)?;
                assert!(matches!(
                    store
                        .record(id, &call)?
                        .settlement_receipts
                        .unwrap()
                        .manifest_conflict,
                    EffectDisposition::Done
                ));
                let found = store.reconcile_with_observer(
                    run.attempt_id,
                    &providers,
                    &call,
                    &provenant_supervisor::census::SystemProcessObserver,
                )?;
                assert!(
                    !found
                        .iter()
                        .any(|(_, state)| matches!(state, crate::Reconciled::Uncertain { .. })),
                    "{found:?}"
                );
                assert!(
                    store.writer_fences()?.is_empty(),
                    "{:?}",
                    store.writer_fences()?
                );
                continue;
            }
            if mode == "orphan" {
                assert!(first.release_permitted);
                assert_eq!(
                    first
                        .record
                        .settlement_receipts
                        .as_ref()
                        .unwrap()
                        .git_import,
                    EffectDisposition::Done
                );
                store.settle_recovered(&attempt, recovery)?;
                assert!(store.writer_fences()?.is_empty());
                continue;
            }
            assert!(!first.release_permitted);
            assert!(matches!(
                first
                    .record
                    .settlement_receipts
                    .as_ref()
                    .unwrap()
                    .git_import,
                EffectDisposition::Failed {
                    retryable: true,
                    ..
                }
            ));
            let decision = read_sealed(&attempt, "terminal-pending.json", RECORD_LIMIT)?
                .ok_or("sealed decision")?;
            assert!(!store.writer_fences()?.is_empty());
            if mode == "reconcile_failed" {
                // A held custody lock must leave the receipt retryable.
                store.reconcile_with_observer(
                    run.attempt_id,
                    &providers,
                    &call,
                    &provenant_supervisor::census::SystemProcessObserver,
                )?;
                assert_eq!(
                    store.record(id, &call)?.settlement_receipts,
                    first.record.settlement_receipts
                );
            }
            assert!(
                store.settle_recovered(&attempt, recovery).is_err(),
                "unresolved import cannot release"
            );
            assert!(candidate.join("git-common/HEAD").is_file());
            let mut competing = crate::prepare_writer(
                &fixture.writer,
                execution.state_root.as_path(),
                execution.project_id,
            )?;
            let (directory, binding) = competing.custody.take().ok_or("competing custody")?;
            let other = custody::Scope {
                attempt: provenant_core::AttemptId::from_bytes([99; 16]),
                ..scope
            };
            assert!(
                store
                    .acquire_writer_fenced(
                        directory,
                        other,
                        binding,
                        &|directory, scope, binding| {
                            provenant_supervisor::fixture::acquire_writer_with_binding(
                                directory,
                                scope,
                                binding,
                                provenant_core::WaitBudget::nonblocking(clock.mono_now()),
                                &cancel,
                                &clock,
                            )
                        },
                        &call
                    )
                    .is_err(),
                "retained reservation excludes competing writer"
            );
            if matches!(mode, "reconcile_failed" | "reconcile_reboot") {
                struct Reboot;
                impl provenant_supervisor::census::ProcessObserver for Reboot {
                    fn boot_identity(
                        &self,
                    ) -> std::result::Result<String, provenant_supervisor::census::CensusError>
                    {
                        Ok("darwin:11111111-1111-1111-1111-111111111111".into())
                    }
                    fn observe(&self, _: i64) -> provenant_supervisor::census::ProcessObservation {
                        provenant_supervisor::census::ProcessObservation::Absent
                    }
                    fn census(
                        &self,
                        _: usize,
                    ) -> std::result::Result<
                        provenant_supervisor::census::Census,
                        provenant_supervisor::census::CensusError,
                    > {
                        unreachable!("reboot proof needs only the boot identity")
                    }
                }
                let observer: &dyn provenant_supervisor::census::ProcessObserver =
                    if mode == "reconcile_reboot" {
                        // A real bound reservation survives the failed import;
                        // only its boot changes, as across a machine restart.
                        let reservation = store
                            .writer_fences()?
                            .into_iter()
                            .find(|fence| fence.scope == scope)
                            .ok_or("retained fence")?
                            .reservation;
                        let mut value: serde_json::Value =
                            serde_json::from_slice(&std::fs::read(&reservation)?)?;
                        value["boot"] = "darwin:00000000-0000-0000-0000-000000000000".into();
                        std::fs::write(&reservation, serde_json::to_vec(&value)?)?;
                        &Reboot
                    } else {
                        &provenant_supervisor::census::SystemProcessObserver
                    };
                let sealed = read_sealed(&attempt, "terminal-pending.json", RECORD_LIMIT)?;
                let result = read_sealed(&attempt, "result.md", RESULT_LIMIT)?;
                let retained = std::fs::read(candidate.join("git-common/HEAD"))?;
                drop(store);
                let store = RunStore::open(&execution, &call)?;
                let name = OsStr::new("settlement-progress.json");
                let mut journal: serde_json::Value =
                    crate::db::decode(&attempt.read_stable(name, RECORD_LIMIT)?)?;
                journal["retry_after_ms"] = 0.into();
                attempt
                    .atomic_write(
                        name,
                        ".retry-fixture-",
                        &crate::db::bytes(&journal)?,
                        provenant_os::fs::PublishMode::Replace,
                    )?
                    .require_durable()?;
                // Settle while the public queuing pass is still running. Its
                // earlier error snapshot must disappear from the final report.
                let found = store.queue_reconcile_with(run.attempt_id, &call, |_, _| {
                    let found = store
                        .reconcile_with_observer(run.attempt_id, &providers, &call, observer)
                        .expect("recovery");
                    assert!(
                        found.contains(&(id, crate::Reconciled::Finished)),
                        "{found:?}"
                    );
                    Ok(())
                })?;
                assert!(found.is_empty(), "{found:?}");
                if mode == "reconcile_reboot" {
                    let cleanup = crate::controls::REBOOT_TEMPORARY_CLEANUP
                        .with(|result| result.borrow_mut().take());
                    assert_eq!(cleanup, Some(Ok(None)));
                }
                assert!(
                    matches!(store.record(id, &call)?.settlement_receipts.unwrap().git_import,
                    EffectDisposition::Failed { retryable: false, ref reason }
                    if reason.contains("injected Git import failure"))
                );
                assert!(store.writer_fences()?.is_empty());
                assert_eq!(
                    read_sealed(&attempt, "terminal-pending.json", RECORD_LIMIT)?,
                    sealed
                );
                assert_eq!(std::fs::read(candidate.join("git-common/HEAD"))?, retained);
                assert_eq!(read_sealed(&attempt, "result.md", RESULT_LIMIT)?, result);
                let (head, path, bytes) = retained_commit.as_ref().ok_or("private commit")?;
                let recovery_info =
                    retained_git(execution.state_root.as_path(), &store.record(id, &call)?)?;
                assert_eq!(recovery_info.head, Some(*head));
                assert_eq!(recovery_info.objects, Some(candidate.join("git-objects")));
                // Age well past ordinary collection; refused-import evidence stays.
                store.gc_provider_state(
                    &store.root,
                    1,
                    clock
                        .wall_now()?
                        .checked_add(provenant_core::DurationMicros::from_seconds(90 * 86_400)?)?,
                    false,
                    &call,
                )?;
                assert_eq!(&std::fs::read(path)?, bytes);
                assert_eq!(
                    crate::objects::resolve_ref(
                        &git,
                        launch.worktree.repo(),
                        launch.branch.as_str()
                    )?,
                    launch.base
                );
                assert_eq!(
                    crate::objects::resolve_ref(
                        &git,
                        launch.worktree.repo(),
                        &format!("refs/provenant/lane-base/{id}")
                    )?,
                    launch.base
                );

                assert!(
                    store
                        .reconcile_with_observer(
                            run.attempt_id,
                            &providers,
                            &call,
                            &provenant_supervisor::census::SystemProcessObserver,
                        )?
                        .is_empty()
                );
                continue;
            }
            let mut advanced_tip = None;
            if let Some(head) = published_head {
                let journal: serde_json::Value = crate::db::decode(
                    &attempt.read_stable(OsStr::new("settlement-progress.json"), RECORD_LIMIT)?,
                )?;
                assert_eq!(journal["phase"], "publish");
                assert_eq!(
                    journal["blocking_reason"],
                    "invalid run record: lane Git import failed"
                );
                let queued = store.queue_reconcile_with(run.attempt_id, &call, |_, _| Ok(()))?;
                let json = serde_json::to_value(&queued)?;
                assert!(
                    json.to_string().contains("lane Git import failed"),
                    "{json}"
                );
                if mode == "publish_diverged" {
                    assert!(!store.writer_fences()?.is_empty());
                    assert!(
                        crate::objects::publish_branch(
                            &git,
                            launch.worktree.repo(),
                            launch.branch.as_str(),
                            launch.base,
                            head
                        )
                        .is_err()
                    );
                    // Reproduce an attempt retained before errors were journalled.
                    let name = OsStr::new("settlement-progress.json");
                    let mut journal: serde_json::Value =
                        crate::db::decode(&attempt.read_stable(name, RECORD_LIMIT)?)?;
                    journal["blocking_reason"] = serde_json::Value::Null;
                    journal["retry_after_ms"] = 0.into();
                    attempt
                        .atomic_write(
                            name,
                            ".retry-fixture-",
                            &crate::db::bytes(&journal)?,
                            provenant_os::fs::PublishMode::Replace,
                        )?
                        .require_durable()?;
                    store.finish(id, &attempt, &observed, &providers, &call)?;
                    let found = store.reconcile_with_observer(
                        run.attempt_id,
                        &providers,
                        &call,
                        &provenant_supervisor::census::SystemProcessObserver,
                    )?;
                    assert!(
                        found.contains(&(id, crate::Reconciled::Finished)),
                        "{found:?}"
                    );
                    assert!(
                        matches!(store.record(id, &call)?.settlement_receipts.unwrap().git_import,
                        EffectDisposition::Failed { retryable: false, ref reason }
                        if reason.contains("lane Git import failed"))
                    );
                    assert!(store.writer_fences()?.is_empty());
                    assert_eq!(
                        read_sealed(&attempt, "terminal-pending.json", RECORD_LIMIT)?.unwrap(),
                        decision
                    );
                    assert!(candidate.join("git-common/HEAD").is_file());
                    assert!(
                        store
                            .reconcile_with_observer(
                                run.attempt_id,
                                &providers,
                                &call,
                                &provenant_supervisor::census::SystemProcessObserver
                            )?
                            .is_empty()
                    );
                    continue;
                }
                fixture.lane_git(&["reset", "--hard", &head.to_string()])?;
                std::fs::write(
                    fixture.writer.worktree.join("later.txt"),
                    "later lane contents\n",
                )?;
                fixture.lane_git(&["add", "later.txt"])?;
                fixture.commit(&fixture.writer.worktree, "Later lane")?;
                advanced_tip = Some(crate::objects::resolve_ref(
                    &git,
                    launch.worktree.repo(),
                    launch.branch.as_str(),
                )?);
            }
            if mode == "archived_fault" {
                // A retryable import failure keeps its fence even though a
                // manifest already exists; clear it so the retry can seal.
                let archive = crate::artifacts::subdir(&attempt, OsStr::new("quarantine"), false)?;
                archive.unlink(OsStr::new("manifest.json"))?;
            }
            drop(store);
            let store = RunStore::open(&execution, &call)?;
            let journal_name = OsStr::new("settlement-progress.json");
            let mut journal: serde_json::Value =
                crate::db::decode(&attempt.read_stable(journal_name, RECORD_LIMIT)?)?;
            journal["retry_after_ms"] = 0.into();
            attempt
                .atomic_write(
                    journal_name,
                    ".retry-fixture-",
                    &crate::db::bytes(&journal)?,
                    provenant_os::fs::PublishMode::Replace,
                )?
                .require_durable()?;
            if mode == "release_boundary" {
                let directory = store.writer_custody_directory(&first.record, &fixture.writer)?;
                let mut recovery = store
                    .recover_writer_exit(directory, scope, &call)?
                    .ok_or("recovery")?;
                recovery.prepare_settlement()?;
                let finished = store.finish_in_custody(
                    id,
                    &attempt,
                    &observed,
                    Completion::Recovered(Some(&recovery)),
                    &providers,
                    &call,
                )?;
                assert!(finished.release_permitted);
                assert!(store.retained_import_fence(scope)?);
                drop(recovery);
                // The kernel lock is now free, but the retained import still
                // protects this worktree until reconciliation completes.
                let mut competing = crate::prepare_writer(
                    &fixture.writer,
                    execution.state_root.as_path(),
                    execution.project_id,
                )?;
                let (directory, binding) = competing.custody.take().ok_or("competing custody")?;
                let other = custody::Scope {
                    attempt: provenant_core::AttemptId::from_bytes([99; 16]),
                    ..scope
                };
                let refusal = match store.acquire_writer_fenced(
                    directory,
                    other,
                    binding,
                    &|directory, scope, binding| {
                        provenant_supervisor::fixture::acquire_writer_with_binding(
                            directory,
                            scope,
                            binding,
                            provenant_core::WaitBudget::nonblocking(clock.mono_now()),
                            &cancel,
                            &clock,
                        )
                    },
                    &call,
                ) {
                    Err(error) => error,
                    Ok(_) => panic!("retained import must exclude a competing writer"),
                };
                assert!(matches!(
                    refusal,
                    crate::writer::WriterCustodyError::Admission(
                        custody::CustodyError::ImportRetained { attempt }
                    ) if attempt == scope.attempt
                ));
                let found = store.reconcile_with_observer(
                    run.attempt_id,
                    &providers,
                    &call,
                    &provenant_supervisor::census::SystemProcessObserver,
                )?;
                assert!(
                    !found
                        .iter()
                        .any(|(_, state)| matches!(state, crate::Reconciled::Uncertain { .. })),
                    "{found:?}"
                );
            } else {
                store.finish(id, &attempt, &observed, &providers, &call)?;
            }
            assert_eq!(
                providers.decoded.get(),
                1,
                "recovery never reruns decoding or the provider"
            );
            assert_eq!(
                read_sealed(&attempt, "terminal-pending.json", RECORD_LIMIT)?.unwrap(),
                decision
            );
            let completed = store.record(id, &call)?;
            assert!(matches!(
                completed.settlement_receipts.unwrap().git_import,
                EffectDisposition::Done
            ));
            if let Some(tip) = advanced_tip {
                assert_eq!(
                    crate::objects::resolve_ref(
                        &git,
                        launch.worktree.repo(),
                        launch.branch.as_str()
                    )?,
                    tip
                );
                assert!(
                    crate::objects::resolve_ref(
                        &git,
                        launch.worktree.repo(),
                        &format!("refs/provenant/import/{}", published_head.unwrap())
                    )
                    .is_err()
                );
                let journal: serde_json::Value = crate::db::decode(
                    &attempt.read_stable(OsStr::new("settlement-progress.json"), RECORD_LIMIT)?,
                )?;
                assert_eq!(
                    std::fs::read_to_string(fixture.writer.worktree.join("later.txt"))?,
                    "later lane contents\n"
                );
                assert_eq!(journal["phase"], "complete");
                assert!(journal["blocking_reason"].is_null());
            }
            assert!(store.writer_fences()?.is_empty());
        }
        Ok(())
    }

    #[test]
    fn settlement_failed_import_becomes_terminal_after_three_attempts_and_releases_custody()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use crate::delivery_tests::{PrepareFixture, registered_landed_writer_fixture};
        use std::ffi::OsStr;
        let fixture = PrepareFixture::new()?;
        let execution = registered_landed_writer_fixture(&fixture)?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&execution, &call)?;
        let mut run = input();
        run.request.task.writer = Some(fixture.writer.clone());
        store.create(&run, &call)?;
        let id = run.attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let record = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let scope = RunStore::scope(&record);
        let attempt = store.attempt_dir(id, false)?;
        let mut launch = crate::prepare_writer(
            &fixture.writer,
            execution.state_root.as_path(),
            execution.project_id,
        )?;
        launch.snapshot(execution.state_root.as_path(), &attempt, scope)?;
        let candidate = store.temporary_path(id, &call)?.join("candidate-0");
        std::fs::create_dir_all(&candidate)?;
        let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
        crate::objects::prepare_bound(
            &git,
            launch.worktree.repo(),
            &attempt,
            &candidate,
            launch.branch.as_str(),
            launch.base,
        )?;
        let missing = "a".repeat(40);
        std::fs::write(
            candidate.join("git-common").join(launch.branch.as_str()),
            format!("{missing}\n"),
        )?;
        let (directory, binding) = launch.custody.take().ok_or("writer custody")?;
        let lease = provenant_supervisor::fixture::acquire_writer_with_binding(
            directory,
            scope,
            binding,
            call.lock_budget(),
            &cancel,
            &clock,
        )?;
        let mut recovery =
            provenant_supervisor::fixture::bind_writer_attempt(lease, &attempt)?.into_recovery()?;
        recovery.prepare_settlement()?;
        let mut observed = crate::tests::exit();
        observed.scope = scope;
        crate::dispatch_tests::seal_exit(&store, &observed)?;
        for index in 0..3 {
            if index != 0 {
                let retained = store.finish_in_custody(
                    id,
                    &attempt,
                    &observed,
                    Completion::Recovered(Some(&recovery)),
                    &crate::dispatch_tests::Fake::default(),
                    &call,
                )?;
                assert!(!retained.release_permitted);
                let journal_name = OsStr::new("settlement-progress.json");
                let mut journal: serde_json::Value =
                    crate::db::decode(&attempt.read_stable(journal_name, RECORD_LIMIT)?)?;
                assert_eq!(journal["attempts"], index);
                assert!(journal["owner"].is_null());
                // Advance the persisted deadline instead of making the fixture
                // sleep through the production five- and thirty-second delays.
                journal["retry_after_ms"] = serde_json::json!(0);
                attempt
                    .atomic_write(
                        journal_name,
                        ".fixture-backoff-",
                        &crate::db::bytes(&journal)?,
                        provenant_os::fs::PublishMode::Replace,
                    )?
                    .require_durable()?;
            }
            let result = store.finish_in_custody(
                id,
                &attempt,
                &observed,
                Completion::Recovered(Some(&recovery)),
                &crate::dispatch_tests::Fake::default(),
                &call,
            );
            if index < 2 {
                let active: i64 = store.db.query_row(
                    "SELECT COUNT(*) FROM admission WHERE attempt=?1",
                    [id.to_string()],
                    |row| row.get(0),
                )?;
                assert_eq!(active, 0);
                assert!(!store.writer_fences()?.is_empty());
                let finished = result?;
                assert!(!finished.release_permitted);
                assert!(matches!(
                    finished.record.state,
                    AttemptState::Terminal {
                        outcome: Outcome::Failed,
                        ..
                    }
                ));
                assert!(matches!(
                    finished.record.settlement_receipts.unwrap().git_import,
                    EffectDisposition::Failed {
                        retryable: true,
                        ..
                    }
                ));
            } else {
                object_import_refused(&attempt, scope)?.ok_or("refusal evidence")?;
                let finished = result?;
                assert!(finished.release_permitted);
                assert!(matches!(
                    finished.record.state,
                    AttemptState::Terminal {
                        outcome: Outcome::Failed,
                        ..
                    }
                ));
            }
            let journal: serde_json::Value = crate::db::decode(
                &attempt.read_stable(OsStr::new("settlement-progress.json"), RECORD_LIMIT)?,
            )?;
            assert_eq!(journal["attempts"], index + 1);
        }
        let active: i64 = store.db.query_row(
            "SELECT COUNT(*) FROM admission WHERE attempt=?1",
            [id.to_string()],
            |row| row.get(0),
        )?;
        assert_eq!(active, 0);
        store.settle_recovered(&attempt, recovery)?;
        assert!(store.writer_fences()?.is_empty());
        let terminal = store.record(id, &call)?;
        assert!(store.custody_released(&terminal, &call)?);
        assert!(
            candidate.join("git-common/HEAD").is_file(),
            "refused private Git remains recoverable"
        );
        assert!(
            attempt
                .read_stable(OsStr::new("release.json"), RECORD_LIMIT)?
                .len()
                > 10
        );
        Ok(())
    }

    #[test]
    fn writer_recovery_accepts_only_matching_terminal_absence()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let custody_dir = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let run = input();
        store.create(&run, &call)?;
        let id = run.attempt_id;
        let starting = store.mark_starting(
            &store.record(id, &call)?,
            &route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let scope = RunStore::scope(&starting);
        let directory = || Directory::open_anchor(custody_dir.path());
        assert!(
            store
                .recover_writer_exit(directory()?, scope, &call)
                .is_err(),
            "missing custody cannot settle a nonterminal attempt"
        );
        drop(custody::acquire_reader(
            directory()?,
            scope,
            call.lock_budget(),
            call.cancel,
            call.clock,
        )?);
        let recovery = store
            .recover_writer_exit(directory()?, scope, &call)?
            .ok_or("prepared reservation must remain recoverable")?;
        recovery.withdraw_prepared()?;
        store.withdraw(
            id,
            AttemptState::Starting {
                extra: Default::default(),
            },
            Outcome::Cancelled,
            "fixture cancellation",
            clock.wall_now()?,
            &call,
        )?;
        // The caller holds a starting snapshot while the guardian has committed
        // cancellation and removed the reservation.
        assert_eq!(
            starting.state,
            AttemptState::Starting {
                extra: Default::default()
            }
        );
        assert!(
            store
                .recover_writer_exit(directory()?, scope, &call)?
                .is_none()
        );
        let mut changed = scope;
        changed.generation += 1;
        assert!(
            store
                .recover_writer_exit(directory()?, changed, &call)
                .is_err(),
            "another generation's terminal state is not settlement"
        );
        std::fs::write(
            custody_dir
                .path()
                .join(scope.attempt.to_string())
                .join("reservation.json"),
            b"{",
        )?;
        assert!(
            store
                .recover_writer_exit(directory()?, scope, &call)
                .is_err(),
            "terminal state must not hide corrupt retained custody"
        );
        Ok(())
    }
}

fn cancellation_reason(bytes: Option<&[u8]>) -> Option<String> {
    bytes.and_then(|bytes| {
        serde_json::from_slice::<serde_json::Value>(bytes)
            .ok()?
            .get("reason")?
            .as_str()
            .map(str::to_owned)
    })
}

fn delivery_cancellation_reason(bytes: Option<&[u8]>) -> String {
    match cancellation_reason(bytes) {
        Some(cancellation) => format!("cancelled during delivery: {cancellation}"),
        None => "cancelled during delivery".into(),
    }
}

#[cfg(test)]
mod friction_tests {
    #[test]
    fn cancellation_reason_uses_request_reason_and_invents_none() {
        assert_eq!(
            super::cancellation_reason(Some(br#"{"reason":"explicit_cancellation"}"#)),
            Some("explicit_cancellation".into())
        );
        assert_eq!(super::cancellation_reason(Some(b"{}")), None);
        assert_eq!(super::cancellation_reason(Some(br#"{"scope":{}}"#)), None);
    }

    #[test]
    fn delivery_cancellation_reason_keeps_its_whole_cause() {
        assert_eq!(
            super::delivery_cancellation_reason(Some(br#"{"reason":"explicit_cancellation"}"#)),
            "cancelled during delivery: explicit_cancellation"
        );
        assert_eq!(
            super::delivery_cancellation_reason(Some(br#"{"scope":{}}"#)),
            "cancelled during delivery"
        );
    }
}

fn soft_budget_wrap_up(record: &AttemptRecord, usage: &UsageRecord) -> bool {
    let Some(limits) = &record.limits else {
        return false;
    };
    let Some(budget) = &limits.token_budget else {
        return false;
    };
    let policy = limits.budget_policy.unwrap_or_default();
    policy.action == provenant_records::BudgetAction::WrapUp
        && crate::token_budget::used(usage).is_some_and(|used| {
            used > budget.value && used < budget_ceiling(budget.value, policy.grace)
        })
}

fn budget_ceiling(budget: u64, grace: provenant_records::BudgetGrace) -> u64 {
    let extra = u128::from(budget) * u128::from(grace.millionths()) / 1_000_000;
    budget.saturating_add(u64::try_from(extra).unwrap_or(u64::MAX))
}
