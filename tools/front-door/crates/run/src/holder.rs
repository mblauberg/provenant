//! The attempt holder: Run's admission and terminal transaction, composed into
//! the same-binary guardian. It runs while the guardian still holds custody.
use crate::{artifacts::read_sealed, launch::Providers, *};
use provenant_core::{
    ActorId, AttemptId, CancelToken, Clock, ProjectId, RootInputs, RunId, WaitBudget,
};
use provenant_os::{clock::SystemClock, exec::Termination, fs::Directory};
use provenant_supervisor::{
    custody::{self, CustodyError, ExitObservation},
    machine::StopReason,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
    pub executables: std::collections::BTreeMap<String, PathBuf>,
    /// Original host search directories for providers unavailable during handoff.
    #[serde(default)]
    pub executable_search_path: Vec<PathBuf>,
    pub git: PathBuf,
    pub codex_home: Option<PathBuf>,
    #[serde(default)]
    pub cargo_build_jobs: Option<String>,
    /// Synthetic fake-provider evidence; absent from installed builds.
    #[cfg(feature = "fixture")]
    #[serde(default)]
    pub fixture_state_write: bool,
}
impl HostLaunchContext {
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
            executables: Default::default(),
            executable_search_path: std::env::var_os("PATH")
                .map(|path| {
                    std::env::split_paths(&path)
                        .filter(|path| path.is_absolute())
                        .collect()
                })
                .unwrap_or_default(),
            git,
            codex_home: std::env::var_os("CODEX_HOME").map(PathBuf::from),
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
    pub run_id: RunId,
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
    pub task_id: provenant_core::TaskId,
    #[serde(default = "legacy_network")]
    pub effective_network: NetworkAccess,
}
fn legacy_network() -> NetworkAccess {
    NetworkAccess::Provider
}
impl SealedTask {
    /// Restore host-sealed launch policy before validating the next generation.
    pub fn apply_network(&self, task: &mut TaskRequest) {
        task.access.network = Some(self.effective_network);
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
    // Transient database retries use real pauses between attempts.
    #[allow(clippy::disallowed_methods)]
    pub fn open(raw: &[u8]) -> Result<(Self, RunStore)> {
        use provenant_core::retry::{Backoff, RetryCause, is_transient};
        const BACKOFF: Backoff = Backoff::new(
            std::time::Duration::from_millis(100),
            2,
            std::time::Duration::from_secs(5),
        );
        for attempt in 0..10 {
            match Self::open_once(raw) {
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
                    std::thread::park_timeout(BACKOFF.delay(attempt));
                }
                other => return other,
            }
        }
        Self::open_once(raw)
    }
    fn open_once(raw: &[u8]) -> Result<(Self, RunStore)> {
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
        let store = RunStore::open(&execution, &call(&SystemClock, &CancelToken::new())?)?
            .with_writer_registry(registry);
        Ok((context, store))
    }
}
fn call<'a>(clock: &'a dyn Clock, cancel: &'a CancelToken) -> Result<Request<'a>> {
    Ok(Request { clock, cancel })
}

pub struct RunHolder<'a> {
    pub providers: &'a dyn Providers,
    pub resources: &'a dyn Resources,
}
/// Bind the host's sealed output roots to admission's resource observations.
struct LaneResources<'a> {
    resources: &'a dyn Resources,
    context: &'a HolderContext,
}
impl Resources for LaneResources<'_> {
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

impl provenant_supervisor::Holder for RunHolder<'_> {
    fn provider_lease(
        &self,
        raw: &[u8],
        resolved_route: &str,
    ) -> custody::Result<Option<provenant_os::fs::LockGuard>> {
        let context: HolderContext = crate::db::decode(raw).map_err(custody_error)?;
        let provider = resolved_route
            .split_once('/')
            .map(|(provider, _)| provider)
            .filter(|provider| !provider.is_empty())
            .ok_or(CustodyError::InvalidRecord)?;
        let clock = SystemClock::new();
        let state = context
            .launch
            .as_ref()
            .map_or(context.roots.state_root.as_path(), |launch| {
                launch.configuration_root.as_path()
            });
        Ok(Some(provenant_os::provider_fence::wait(
            state,
            provider,
            provenant_os::fs::LockMode::Shared,
            WaitBudget::cli(clock.mono_now(), 5).map_err(|_| CustodyError::InvalidRecord)?,
            &CancelToken::new(),
            &clock,
        )?))
    }
    fn admit(&self, context: &[u8], attempt: &Directory) -> custody::Result<()> {
        let (context, store) = HolderContext::open(context).map_err(custody_error)?;
        store
            .admit(
                context.attempt_id,
                attempt,
                &LaneResources {
                    resources: self.resources,
                    context: &context,
                },
                &SystemClock,
                &CancelToken::new(),
            )
            .map(drop)
            .map_err(custody_error)
    }
    fn provider_exited(&self, raw: &[u8], _reason: Option<StopReason>) -> custody::Result<()> {
        let (context, store) = HolderContext::open(raw).map_err(custody_error)?;
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        store
            .write(&call(&clock, &cancel).map_err(custody_error)?, |db| {
                db.execute(
                    "DELETE FROM admission WHERE attempt=?1",
                    [context.attempt_id.to_string()],
                )?;
                Ok(())
            })
            .map_err(custody_error)
    }
    fn terminal(
        &self,
        context: &[u8],
        attempt: &Directory,
        exit: &ExitObservation,
        recovery: &custody::Recovery,
    ) -> custody::Result<()> {
        let (context, store) = HolderContext::open(context).map_err(custody_error)?;
        let cancel = CancelToken::new();
        store
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
                &call(&SystemClock, &cancel).map_err(custody_error)?,
            )
            .map_err(custody_error)?;
        if object_import_refused(attempt, exit.scope)
            .map_err(custody_error)?
            .is_none()
        {
            crate::writer::settle_launch_staging(attempt).map_err(custody_error)?;
        }
        release_and_reclaim(attempt, exit.scope, recovery)?;
        // The lane's builds have ended: trim lane build output to its cap and
        // the free-space floor. Best effort: it waits at most the 5 s ledger
        // budget for admission, and only when there is something to remove.
        let (state, _) = context.disk_roots();
        crate::build_dir_gc::reclaim(state, &context.roots.home);
        Ok(())
    }
}

pub(crate) fn release_and_reclaim(
    attempt: &Directory,
    scope: custody::Scope,
    recovery: &custody::Recovery,
) -> custody::Result<()> {
    custody::publish_release(attempt, scope)?;
    crate::settlement::complete(attempt, scope).map_err(custody_error)?;
    if object_import_refused(attempt, scope)
        .map_err(custody_error)?
        .is_none()
        && let Ok(_commit) = recovery.verify_settlement(attempt)
    {
        // Hold the verified custody lock until temporary cleanup finishes.
        let _ = crate::artifacts::remove_unreserved_terminal_temporary(attempt);
    }
    Ok(())
}

impl RunStore {
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
        let request = self.request(record.run_id, call)?;
        let task = request
            .tasks
            .iter()
            .find(|task| task.task_id == record.task_id)
            .ok_or(RunError::Validation("terminal task missing"))?;
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
        let record = self.finish_in_custody(
            id,
            attempt,
            exit,
            Completion::Recovered(recovery.as_ref()),
            providers,
            call,
        )?;
        if let Some(recovery) = recovery {
            self.settle_recovered(attempt, recovery)?;
        } else if object_import_refused(attempt, exit.scope)?.is_none() {
            crate::writer::settle_launch_staging(attempt)?;
        }
        Ok(record)
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
        let request = self.request(record.run_id, call)?;
        let writer = request
            .tasks
            .iter()
            .find(|task| task.task_id == record.task_id)
            .and_then(|task| task.writer.as_ref())
            .ok_or(RunError::Validation("land writer request missing"))?;
        let launch = crate::prepare_writer(writer, &self.writer_registry, record.project_id)?;
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
        let attempt = self.attempt_dir(record.run_id, record.attempt_id, false)?;
        self.settle_recovered(&attempt, recovery)
    }

    pub(crate) fn settle_recovered(
        &self,
        attempt: &Directory,
        recovery: custody::Recovery,
    ) -> Result<()> {
        if object_import_refused(attempt, recovery.scope())?.is_some() {
            custody::publish_release(attempt, recovery.scope())
                .map_err(|error| RunError::Io(std::io::Error::other(error)))?;
            let commit = recovery
                .verify_settlement(attempt)
                .map_err(|error| RunError::Io(std::io::Error::other(error)))?;
            crate::settlement::complete(attempt, recovery.scope())?;
            return recovery
                .release_preserving_private(commit)
                .map_err(|error| RunError::Io(std::io::Error::other(error)));
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
    fn finish_in_custody(
        &self,
        id: AttemptId,
        attempt: &Directory,
        exit: &ExitObservation,
        completion: Completion<'_>,
        providers: &dyn Providers,
        call: &Request<'_>,
    ) -> Result<AttemptRecord> {
        let recovery = completion.custody();
        let record = self.record(id, call)?;
        if exit.scope != Self::scope(&record)
            || recovery.is_some_and(|recovery| recovery.scope() != exit.scope)
        {
            return Err(RunError::Validation("terminal custody scope mismatch"));
        }
        if matches!(record.state, AttemptState::Terminal { .. }) {
            return Ok(record);
        }
        if read_sealed(attempt, "terminal-pending.json", RECORD_LIMIT)?.is_some() {
            return self.recover_terminal(id, exit, call);
        }
        let mut settlement = match crate::settlement::Guard::claim(attempt, exit.scope, call.clock)
        {
            Ok(Some(driver)) => Some(driver),
            Ok(None) => return Err(RunError::Contention),
            Err(RunError::ObjectImportLimit(_)) => None,
            Err(error) => return Err(error),
        };
        // Recovery also frees admission when upgrading an older exited run.
        self.write(call, |db| {
            db.execute("DELETE FROM admission WHERE attempt=?1", [id.to_string()])?;
            Ok(())
        })?;
        let finished_at = call
            .clock
            .wall_now()
            .map_err(|_| RunError::Validation("wall clock"))?;
        let cancelled = read_sealed(attempt, "cancel.json", RECORD_LIMIT)?.is_some();
        let provider_state = self
            .context
            .state_root
            .as_path()
            .join("runs")
            .join(record.run_id.to_string())
            .join("attempts")
            .join(id.to_string())
            .join("provider-state");
        let mut decoded = match (&record.applied_route, exit.never_spawned) {
            // Cancelled before spawn; keep the guardian's own reason too.
            (_, true) if cancelled => Decoded {
                outcome: Outcome::Cancelled,
                result: String::new(),
                complete: true,
                observed_route: None,
                usage: UsageRecord::default(),
                session: None,
                partial_signals: Vec::new(),
                reason: Some(format!(
                    "cancelled before start: {}",
                    exit.warnings
                        .first()
                        .map_or("provider never spawned", String::as_str)
                )),
            },
            (_, true) => Decoded {
                outcome: Outcome::Rejected,
                result: String::new(),
                complete: true,
                observed_route: None,
                usage: UsageRecord::default(),
                session: None,
                partial_signals: Vec::new(),
                reason: Some(
                    exit.warnings
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "provider never spawned".into()),
                ),
            },
            (Some(route), false) => {
                let stdout = read_sealed(attempt, "stdout.log", CAPTURE_LIMIT)?.unwrap_or_default();
                completion
                    .holder_context()
                    .map_or_else(
                        || providers.decode(route, &stdout, exit, &provider_state),
                        |context| {
                            providers.decode_with_context(
                                route,
                                &stdout,
                                exit,
                                &provider_state,
                                context,
                            )
                        },
                    )
                    .map_err(RunError::ProviderEvidence)?
            }
            (None, false) => return Err(RunError::Validation("running attempt lacks a route")),
        };
        // Provider-owned session transcripts record tool output, so a printed
        // credential can survive there after capture and result filtering.
        // Scrub before publication so a retry never starts from new plaintext.
        let session_redaction_failed =
            provenant_confinement::redact::scrub_session_history(&provider_state).is_err();
        let handoff = self.capture_handoff(attempt, &record, &decoded)?;
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
            decoded.reason = Some(match decoded.reason.take() {
                Some(reason) => format!("{reason}; {note}"),
                None => note,
            });
        }
        // Valid natural completion wins over a late stop request.
        if decoded.outcome != Outcome::Succeeded {
            match exit.stop_reason {
                Some(StopReason::WallDeadline) => decoded.outcome = Outcome::TimedOut,
                Some(StopReason::ExplicitCancellation) => decoded.outcome = Outcome::Cancelled,
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
        let request = self.request(record.run_id, call)?;
        // The run store is writable by a parent lane, so a live terminal must
        // see the request the host launched before it acts on its writer or
        // delivery fields.
        if let Completion::Live {
            sealed: Some(sealed),
            ..
        } = &completion
            && request.digest()? != **sealed
        {
            return Err(RunError::Validation("request changed after launch"));
        }
        let task = request
            .tasks
            .iter()
            .find(|task| task.task_id == record.task_id)
            .ok_or(RunError::Validation("terminal task missing from request"))?;
        // A stop at the runtime deadline is a typed timeout, whatever partial
        // output the provider left, and the lane stays resumable.
        if decoded.outcome == Outcome::TimedOut
            && exit.stop_reason == Some(StopReason::WallDeadline)
        {
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
        if cancelled {
            decoded.outcome = Outcome::Cancelled;
        }
        let mut review_required = false;
        if let Some(writer) = &task.writer {
            let refusal = object_import_refused(attempt, exit.scope)?;
            let refused = refusal.is_some();
            let check = if cancelled {
                Ok(false)
            } else if settlement.is_none() {
                Err(RunError::ObjectImportLimit(
                    "settlement exhausted three attempts; private Git retained for host recovery",
                ))
            } else if let Some(reason) = refusal {
                decoded.outcome = Outcome::Failed;
                decoded.reason = Some(reason);
                Ok(false)
            } else {
                crate::prepare_writer(writer, &self.writer_registry, record.project_id).and_then(
                    |writer| {
                        let driver = settlement.as_mut().ok_or(RunError::Contention)?;
                        let cancel = driver.cancellation();
                        crate::settlement::with_cancellation(attempt, exit.scope, &cancel, || {
                            writer.terminal(
                                self.context.state_root.as_path(),
                                attempt,
                                recovery.ok_or(RunError::Validation(
                                    "writer terminal custody missing",
                                ))?,
                                task.deliver
                                    .as_ref()
                                    .map_or("origin", |deliver| deliver.remote.as_str()),
                                task.base.as_deref(),
                                driver,
                            )
                        })
                    },
                )
            };
            let last_failure = check.as_ref().err().map(ToString::to_string);
            let archived = match crate::artifacts::subdir(
                attempt,
                std::ffi::OsStr::new("quarantine"),
                false,
            ) {
                Ok(archive) => read_sealed(&archive, "manifest.json", RECORD_LIMIT)?.is_some(),
                Err(RunError::Filesystem(error)) if error.is_not_found() => false,
                Err(error) => return Err(error),
            };
            let check = match check {
                Err(error) if crate::writer::worktree_removed(writer) || archived => Err(error),
                Err(RunError::Writer(_)) => Err(RunError::ObjectImportLimit(
                    "writer preconditions refused; private Git retained for host recovery",
                )),
                Err(error) if !matches!(error, RunError::ObjectImportLimit(_)) => {
                    if crate::artifacts::read_sealed(attempt, "cancel.json", RECORD_LIMIT)?
                        .is_some()
                    {
                        decoded.outcome = Outcome::Cancelled;
                        decoded.reason = Some("cancelled during Git import".into());
                        Ok(false)
                    } else if settlement
                        .as_ref()
                        .is_some_and(crate::settlement::Driver::exhausted)
                    {
                        Err(RunError::ObjectImportLimit(
                            "settlement failed after three attempts; private Git retained for host recovery",
                        ))
                    } else {
                        return Err(error);
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
                    crate::artifacts::seal(
                        attempt,
                        "object-import-refused.json",
                        &crate::db::bytes(&ObjectImportRefusal {
                            schema_version: 1,
                            scope: exit.scope,
                            kind: "object_import_refusal".into(),
                            worktree: writer.worktree.clone(),
                            reason: reason.clone(),
                        })?,
                        RECORD_LIMIT,
                    )?;
                    decoded.outcome = Outcome::Failed;
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
                    crate::artifacts::seal(
                        &archive,
                        "manifest.json",
                        &crate::db::bytes(&RemovedManifest {
                            schema_version: 1,
                            scope: exit.scope,
                            kind: "quarantine_manifest",
                            disposition: "worktree_removed",
                        })?,
                        RECORD_LIMIT,
                    )?;
                }
                Ok(false) if decoded.outcome == Outcome::Succeeded => {
                    // Edits with no commit are not a success, and nothing is delivered.
                    let left =
                        crate::prepare_writer(writer, &self.writer_registry, record.project_id)
                            .and_then(|launch| {
                                let cancel = settlement
                                    .as_ref()
                                    .map_or_else(CancelToken::new, |driver| driver.cancellation());
                                crate::settlement::with_cancellation(
                                    attempt,
                                    exit.scope,
                                    &cancel,
                                    || {
                                        launch.uncommitted(
                                            self.context.state_root.as_path(),
                                            recovery.ok_or(RunError::Validation(
                                                "writer terminal custody missing",
                                            ))?,
                                            &cancel,
                                        )
                                    },
                                )
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
                Ok(false) => {}
                Ok(true) => {
                    review_required = true;
                }
                Err(error) => {
                    // Incomplete quarantine is retryable, never a terminal
                    // intent that permanently prevents a later archive commit.
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
            crate::artifacts::seal(
                &archive,
                "manifest.json",
                &crate::db::bytes(&ReadOnlyManifest {
                    schema_version: 1,
                    scope: exit.scope,
                    kind: "quarantine_manifest",
                    disposition: "read_only",
                })?,
                RECORD_LIMIT,
            )?;
        }
        if cancelled
            || decoded.outcome == Outcome::Cancelled
            || object_import_refused(attempt, exit.scope)?.is_some()
        {
            let archive =
                crate::artifacts::subdir(attempt, std::ffi::OsStr::new("quarantine"), true)?;
            if read_sealed(&archive, "manifest.json", RECORD_LIMIT)?.is_none() {
                crate::artifacts::seal(
                    &archive,
                    "manifest.json",
                    &crate::db::bytes(&serde_json::json!({
                        "schema_version": 1, "scope": exit.scope, "kind": "quarantine_manifest",
                        "disposition": if decoded.outcome == Outcome::Cancelled { "cancelled" } else { "import_refused" }
                    }))?,
                    RECORD_LIMIT,
                )?;
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
        let cancelled = cancellation.is_some();
        if cancelled {
            decoded.outcome = Outcome::Cancelled;
            // A typed runtime deadline stays the reason when cancellation also
            // applies, and a legacy scope-only record adds no invented cause.
            let typed_deadline = decoded
                .reason
                .as_deref()
                .is_some_and(|reason| reason.starts_with("runtime deadline exceeded"));
            if let Some(reason) = cancellation_reason(cancellation.as_deref())
                && !typed_deadline
            {
                decoded.reason = Some(format!("cancelled: {reason}"));
            }
        }
        if decoded.result.len() as u64 > RESULT_LIMIT {
            let mut end = RESULT_LIMIT as usize;
            while !decoded.result.is_char_boundary(end) {
                end -= 1;
            }
            decoded.result.truncate(end);
            decoded.complete = false;
        }
        if matches!(decoded.outcome, Outcome::Succeeded | Outcome::Uncommitted) {
            let refusal = if exit.termination != Some(Termination::Exit(0)) {
                Some("provider exited unsuccessfully")
            } else if !decoded.complete || decoded.result.is_empty() {
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
        if decoded.outcome == Outcome::Succeeded && review_required {
            decoded.outcome = Outcome::ReviewRequired;
            decoded.reason = Some("protected changes require review".into());
        }
        let frozen_policy_retry = self.staged_policy_retry(&record)?.is_some();
        if frozen_policy_retry
            && matches!(decoded.outcome, Outcome::Succeeded | Outcome::Uncommitted)
        {
            decoded.outcome = if cancelled {
                Outcome::Cancelled
            } else {
                Outcome::Failed
            };
            decoded.reason =
                Some("result-policy recovery decision retained from interrupted settlement".into());
        }
        if !frozen_policy_retry
            && !cancelled
            && exit.stop_reason.is_none()
            && !exit.never_spawned
            && !exit.unresolved_cleanup
        {
            let assessment = providers
                .assess_result(
                    task,
                    &record,
                    &decoded,
                    handoff.as_ref(),
                    completion.holder_context(),
                    self.policy_attempt_number(&record, call)?,
                )
                .map_err(RunError::ProviderEvidence)?;
            if let Some(reason) = assessment.refusal {
                decoded.outcome = Outcome::Failed;
                decoded.reason = Some(reason);
            }
            if let Some(retry) = assessment.retry
                && self.stage_policy_retry(&record, &retry, call)?
                && let Some(note) = assessment.note
            {
                decoded.reason = Some(match decoded.reason.take() {
                    Some(reason) => format!("{reason}; {note}"),
                    None => note,
                });
            }
        }
        // Decoding can reveal credential markers hidden by JSON escapes in
        // the already-filtered capture. Seal only a filtered rendering.
        // A filter failure withholds the result instead of blocking settlement
        // forever: the lane fails with a fixed placeholder and stays terminal.
        let (result, result_redaction_failed) =
            match provenant_confinement::redact::known_shapes(decoded.result.as_bytes()) {
                Ok(safe) => match String::from_utf8(safe) {
                    Ok(text) => (text, false),
                    Err(_) => (UNREDACTABLE_RESULT.to_owned(), true),
                },
                Err(_) => (UNREDACTABLE_RESULT.to_owned(), true),
            };
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
        if matches!(
            decoded.outcome,
            Outcome::Succeeded | Outcome::ReviewRequired | Outcome::Uncommitted
        ) && !matches!(&task.route, RequestedRoute::Adapter { .. })
            && provenant_records::handoff::Handoff::from_result(&decoded.result)
                .ok()
                .flatten()
                .is_none()
        {
            partial.push(PartialSignal::HandoffMissing);
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
        // Recovered/result-only publication never starts external delivery.
        // A succeeded outcome already proves the provider's terminal event in
        // the sealed stdout, so no live frame observation is required.
        let result_orphan = read_sealed(attempt, "result.md", RESULT_LIMIT)?.is_some();
        if !result_orphan
            && succeeded_before_stop
            && !redaction_failed
            && !cancelled
            && exit.stop_reason.is_none()
            && !exit.unresolved_cleanup
            && exit.publication == custody::PublicationState::Durable
            && let Completion::Live {
                delivery: Some(host),
                ..
            } = &completion
            && let (Some(writer), Some(deliver)) = (&task.writer, &task.deliver)
        {
            let cancel = settlement
                .as_ref()
                .map_or_else(CancelToken::new, |driver| driver.cancellation());
            let host = host.cancellable(&cancel);
            crate::settlement::with_cancellation(attempt, exit.scope, &cancel, || {
                crate::delivery::deliver_once(
                    &host,
                    writer,
                    deliver,
                    attempt,
                    &format!("Run {}\nAttempt {id}", record.run_id),
                )
            })?;
            if let Some(cancellation) = read_sealed(attempt, "cancel.json", RECORD_LIMIT)? {
                decoded.outcome = Outcome::Cancelled;
                decoded.reason = Some(delivery_cancellation_reason(
                    Some(&cancellation),
                    handoff
                        .as_ref()
                        .is_some_and(|handoff| !handoff.unfinished()),
                ));
            }
        }
        let publication = TerminalPublication {
            outcome: decoded.outcome,
            finished_at,
            started_at: record.started_at,
            provider_ms: record.started_at.and_then(|started| {
                u64::try_from(finished_at.as_micros().saturating_sub(started.as_micros()) / 1000)
                    .ok()
            }),
            reason: decoded.reason,
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
        // A result sealed without intent predates a crash: keep its bytes.
        if result_orphan {
            return self.recover_result_only(id, &publication, exit, call);
        }
        if let Some(driver) = &mut settlement {
            driver.advance(crate::settlement::Phase::Record)?;
        }
        let terminal = self.publish_terminal(id, record.revision, &publication, exit, call)?;
        if let Some(driver) = &mut settlement {
            driver.advance(crate::settlement::Phase::Release)?;
        }
        Ok(terminal)
    }
}

/// Stored instead of the provider's answer when settlement cannot filter it.
const UNREDACTABLE_RESULT: &str = "[result withheld: redaction failed]";
const REDACTION_FAILED_REASON: &str = "redaction failed; result withheld";

/// A lane's own words in a one-line reason: bounded and on one line.
fn bounded_note(note: &str) -> String {
    provenant_confinement::redact::bounded_line(note, 240)
}

#[cfg(test)]
mod host_clock_tests {
    use super::*;

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
            run_id: RunId::from_bytes([3; 16]),
            attempt_id: AttemptId::from_bytes([4; 16]),
            host_delivery: None,
            launch: Some(HostLaunchContext {
                configuration_root: temp.path().into(),
                cwd: temp.path().into(),
                executables: Default::default(),
                executable_search_path: Vec::new(),
                git: "/usr/bin/git".into(),
                codex_home: None,
                cargo_build_jobs: None,
                #[cfg(feature = "fixture")]
                fixture_state_write: false,
            }),
            sealed: None,
        };
        struct Limited;
        impl Resources for Limited {
            fn memory(&self) -> std::io::Result<provenant_os::sys::resources::MemorySample> {
                SystemResources.memory()
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
                floor: 100
            })),
            CustodyError::Admission(AdmissionError::Disk {
                free: 99,
                floor: 100
            })
        ));
    }
}

#[cfg(test)]
mod writer_recovery_tests {
    use super::*;
    use crate::tests::{call, clock, context, input, route};

    #[test]
    fn legacy_slot_root_writers_reconcile_exited_and_never_spawned_to_terminal()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use crate::delivery_tests::{PrepareFixture, registered_landed_writer_fixture};
        use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
        use std::ffi::{OsStr, OsString};
        for mode in [
            "spawned",
            "never_spawned",
            "no_summary",
            "no_summary_terminal",
        ] {
            let fixture = PrepareFixture::new()?;
            let execution = registered_landed_writer_fixture(&fixture)?;
            let (clock, cancel) = (clock(), CancelToken::new());
            let call = call(&clock, &cancel);
            let store = RunStore::open(&execution, &call)?;
            let mut run = input();
            run.request.tasks[0].writer = Some(fixture.writer.clone());
            store.create(&run, &call)?;
            let id = run.attempts[0].attempt_id;
            let mut record = store.mark_starting(
                &store.record(id, &call)?,
                &route(),
                &[],
                &Default::default(),
                None,
                &call,
            )?;
            if mode == "spawned" {
                record = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
            }
            let scope = RunStore::scope(&record);
            let attempt = store.attempt_dir(run.run_id, id, false)?;
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
                "legacy private commit",
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
            let anchor = slot.metadata()?;
            reservation["reservation_device"] = anchor.device.into();
            reservation["reservation_inode"] = anchor.inode.into();
            if mode == "spawned" {
                reservation["launch_state"] = "spawned".into();
                let identity = serde_json::json!({"boot":reservation["boot"],"pid":999999,"native_start":1,"group_id":999999,"session_id":999999});
                reservation["guardian"] = identity.clone();
                reservation["provider"] = identity;
            }
            std::fs::write(
                slot_path.join("reservation.json"),
                serde_json::to_vec(&reservation)?,
            )?;
            std::fs::rename(
                modern.join("ownership.json"),
                slot_path.join("ownership.json"),
            )?;
            std::fs::remove_dir_all(modern)?;
            if !mode.starts_with("no_summary") {
                crate::artifacts::seal(
                    &attempt,
                    "launch.json",
                    &serde_json::to_vec(&serde_json::json!({
                        "schema_version": 1, "scope": scope, "requester": execution.actor_id
                    }))?,
                    RECORD_LIMIT,
                )?;
                let mut exit = crate::tests::exit();
                exit.scope = scope;
                exit.never_spawned = mode == "never_spawned";
                if exit.never_spawned {
                    exit.warnings.push("legacy provider never spawned".into());
                }
                crate::dispatch_tests::seal_exit(&store, &exit)?;
            }
            if mode == "no_summary_terminal" {
                store.commit_lost(
                    scope,
                    "launch interrupted: provider never spawned; private evidence retained",
                    &call,
                )?;
            }
            let found = store.reconcile_with_observer(
                run.run_id,
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
            if mode == "never_spawned" {
                assert!(
                    matches!(terminal.state, AttemptState::Terminal { outcome: Outcome::Rejected, reason: Some(ref reason), .. } if reason.contains("never spawned")),
                    "{:?}",
                    terminal.state
                );
            }
            if mode.starts_with("no_summary") {
                assert!(
                    matches!(terminal.state, AttemptState::Terminal { outcome: Outcome::Lost, reason: Some(ref reason), .. } if reason.contains("never spawned")),
                    "{:?}",
                    terminal.state
                );
            }
            assert!(store.writer_fences()?.is_empty());
            assert!(store.writer_fence_listing()?.issues.is_empty());
            assert!(!slot_path.join("reservation.json").exists());
            assert!(slot_path.join("settlement.json").is_file());
            if mode.starts_with("no_summary") {
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
            } else {
                assert!(
                    attempt
                        .read_stable(OsStr::new("release.json"), RECORD_LIMIT)?
                        .len()
                        > 10
                );
                let imported = crate::objects::resolve_ref(
                    &git,
                    launch.worktree.repo(),
                    launch.branch.as_str(),
                )?;
                assert_eq!(
                    imported.to_string(),
                    head,
                    "private committed head imported before release"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn failed_import_becomes_terminal_after_three_attempts_and_releases_custody()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use crate::delivery_tests::{PrepareFixture, registered_landed_writer_fixture};
        use std::ffi::OsStr;
        let fixture = PrepareFixture::new()?;
        let execution = registered_landed_writer_fixture(&fixture)?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&execution, &call)?;
        let mut run = input();
        run.request.tasks[0].writer = Some(fixture.writer.clone());
        store.create(&run, &call)?;
        let id = run.attempts[0].attempt_id;
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
        let attempt = store.attempt_dir(run.run_id, id, false)?;
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
                assert!(matches!(
                    store.finish_in_custody(
                        id,
                        &attempt,
                        &observed,
                        Completion::Recovered(Some(&recovery)),
                        &crate::dispatch_tests::Fake::default(),
                        &call,
                    ),
                    Err(RunError::Contention)
                ));
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
                assert!(
                    matches!(
                        result,
                        Err(RunError::OperationIo {
                            operation: "import lane Git objects",
                            ..
                        })
                    ),
                    "{result:?}"
                );
            } else {
                let refusal = object_import_refused(&attempt, scope)?.ok_or("refusal evidence")?;
                assert!(
                    matches!(result?.state, AttemptState::Terminal { outcome: Outcome::Failed, reason: Some(reason), .. } if reason == refusal)
                );
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
        let id = run.attempts[0].attempt_id;
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

fn delivery_cancellation_reason(bytes: Option<&[u8]>, handoff_done: bool) -> String {
    let mut reason = match cancellation_reason(bytes) {
        Some(cancellation) => format!("cancelled during delivery: {cancellation}"),
        None => "cancelled during delivery".into(),
    };
    if handoff_done {
        reason.push_str("; handoff declared done but settlement was not successful");
    }
    reason
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
            super::delivery_cancellation_reason(
                Some(br#"{"reason":"explicit_cancellation"}"#),
                false
            ),
            "cancelled during delivery: explicit_cancellation"
        );
        assert_eq!(
            super::delivery_cancellation_reason(
                Some(br#"{"reason":"explicit_cancellation"}"#),
                true
            ),
            "cancelled during delivery: explicit_cancellation; handoff declared done but settlement was not successful"
        );
        assert_eq!(
            super::delivery_cancellation_reason(Some(br#"{"scope":{}}"#), false),
            "cancelled during delivery"
        );
    }
}
