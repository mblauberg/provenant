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

// The host registry is protected from lanes. Thirty lock timeouts permit an
// hour of contention at the normal lock budget.
const VERIFICATION_RETRY_LIMIT: i64 = 3;
const BUSY_RETRY_LIMIT: i64 = 30;

fn settlement_retries(
    host: &std::path::Path,
    scope: custody::Scope,
    call: &Request<'_>,
    increment: Option<bool>,
) -> Result<(i64, i64)> {
    // The settlement guard serialises live and recovered settlers, including
    // while slow object verification temporarily releases writer.lock.
    use rusqlite::{OptionalExtension, params};
    let (_, db, _initialisation) =
        crate::db::open_database(host, "object-settlement.sqlite3", call)?;
    crate::db::immediate(&db, call, |tx| {
        tx.execute_batch("CREATE TABLE IF NOT EXISTS retries (run TEXT NOT NULL, attempt TEXT NOT NULL, generation TEXT NOT NULL, deadlines INTEGER NOT NULL, busy INTEGER NOT NULL, PRIMARY KEY(run,attempt,generation)) STRICT;")?;
        let key = (
            scope.run.to_string(),
            scope.attempt.to_string(),
            scope.generation.to_string(),
        );
        if let Some(deadline) = increment {
            tx.execute(
                "INSERT INTO retries VALUES (?1,?2,?3,?4,?5) ON CONFLICT(run,attempt,generation) DO UPDATE SET deadlines=deadlines+?4, busy=busy+?5",
                params![key.0, key.1, key.2, i64::from(deadline), i64::from(!deadline)],
            )?;
        }
        Ok(tx
            .query_row(
                "SELECT deadlines,busy FROM retries WHERE run=?1 AND attempt=?2 AND generation=?3",
                params![key.0, key.1, key.2],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .unwrap_or((0, 0)))
    })
}

fn clear_settlement_retries(
    host: &std::path::Path,
    scope: custody::Scope,
    call: &Request<'_>,
) -> Result<()> {
    let (_, db, _initialisation) =
        crate::db::open_database(host, "object-settlement.sqlite3", call)?;
    crate::db::immediate(&db, call, |tx| {
        tx.execute_batch("CREATE TABLE IF NOT EXISTS retries (run TEXT NOT NULL, attempt TEXT NOT NULL, generation TEXT NOT NULL, deadlines INTEGER NOT NULL, busy INTEGER NOT NULL, PRIMARY KEY(run,attempt,generation)) STRICT;")?;
        tx.execute(
            "DELETE FROM retries WHERE run=?1 AND attempt=?2",
            rusqlite::params![scope.run.to_string(), scope.attempt.to_string()],
        )?;
        Ok(())
    })
}

fn terminal_request<'a>(request: &Request<'a>) -> Result<Request<'a>> {
    Ok(Request {
        clock: request.clock,
        cancel: request.cancel,
        budget: WaitBudget::cli(request.clock.mono_now(), 30)
            .map_err(|_| RunError::Validation("terminal call budget"))?,
    })
}

fn retry_limit(counts: (i64, i64)) -> Option<RunError> {
    if counts.0 >= VERIFICATION_RETRY_LIMIT {
        Some(RunError::ObjectImportLimit(
            ObjectImportLimitReason::VerificationRetries,
        ))
    } else if counts.1 >= BUSY_RETRY_LIMIT {
        Some(RunError::ObjectImportLimit(
            ObjectImportLimitReason::StoreBusyRetries,
        ))
    } else {
        None
    }
}

fn seal_retry_refusal(attempt: &Directory, scope: custody::Scope) -> Result<()> {
    let archive = crate::artifacts::subdir(attempt, std::ffi::OsStr::new("quarantine"), true)?;
    #[derive(Serialize)]
    struct Manifest {
        schema_version: u64,
        scope: custody::Scope,
        kind: &'static str,
        disposition: &'static str,
    }
    crate::artifacts::seal(
        &archive,
        "manifest.json",
        &crate::db::bytes(&Manifest {
            schema_version: 1,
            scope,
            kind: "quarantine_manifest",
            disposition: "object_import_limit",
        })?,
        RECORD_LIMIT,
    )
}

/// Inputs that re-resolve the submitting edge's state root in the guardian,
/// whose environment is cleared.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootSeed {
    pub home: PathBuf,
    pub checkout: PathBuf,
    pub state_root: PathBuf,
}
/// Host-only launch inputs, passed on the custody pipe rather than through
/// lane-writable state. Provider credentials are resolved afresh on the host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
/// Opaque to supervisor; travels on the handoff pipe only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
        let mut delay = std::time::Duration::from_millis(100);
        for _ in 0..10 {
            match Self::open_once(raw) {
                Err(
                    RunError::Contention
                    | RunError::Sqlite(_)
                    | RunError::Io(_)
                    | RunError::OperationIo { .. },
                ) => {
                    std::thread::park_timeout(delay);
                    delay = (delay * 2).min(std::time::Duration::from_secs(5));
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
    Ok(Request {
        clock,
        budget: WaitBudget::cli(clock.mono_now(), 30)
            .map_err(|_| RunError::Validation("call budget"))?,
        cancel,
    })
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
}
fn custody_error(error: RunError) -> CustodyError {
    match error {
        RunError::HostClock => CustodyError::AdmissionClock,
        RunError::BuildSettings => CustodyError::AdmissionBuildSettings,
        RunError::GuardianIdentityUnavailable => CustodyError::AdmissionGuardianIdentity,
        RunError::DiskFloor { free, floor } => CustodyError::AdmissionDisk { free, floor },
        RunError::MemoryPressure { level } => CustodyError::AdmissionMemoryPressure { level },
        RunError::BuildSlots { active, limit } => {
            CustodyError::AdmissionBuildSlots { active, limit }
        }
        other => CustodyError::Io(std::io::Error::other(other.to_string())),
    }
}

/// A resource refusal seals the failed attempt without certifying its graph.
/// Its writer reservation and private objects remain available for recovery.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ObjectImportRefusal {
    schema_version: u64,
    scope: custody::Scope,
    kind: String,
    worktree: PathBuf,
    reason: String,
    #[serde(default)]
    release_custody: bool,
}

pub(crate) fn object_import_refused(
    attempt: &Directory,
    scope: custody::Scope,
) -> Result<Option<String>> {
    Ok(import_refusal(attempt, scope)?.map(|refusal| refusal.reason))
}

fn import_retains_custody(attempt: &Directory, scope: custody::Scope) -> Result<bool> {
    Ok(import_refusal(attempt, scope)?.is_some_and(|refusal| !refusal.release_custody))
}

fn import_refusal(
    attempt: &Directory,
    scope: custody::Scope,
) -> Result<Option<ObjectImportRefusal>> {
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
    Ok(Some(refusal))
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
        if !import_retains_custody(attempt, exit.scope).map_err(custody_error)? {
            crate::writer::settle_launch_staging(attempt).map_err(custody_error)?;
        }
        release_and_reclaim(attempt, exit.scope, recovery)
    }
}

pub(crate) fn release_and_reclaim(
    attempt: &Directory,
    scope: custody::Scope,
    recovery: &custody::Recovery,
) -> custody::Result<()> {
    if import_retains_custody(attempt, scope).map_err(custody_error)? {
        return Ok(());
    }
    custody::publish_release(attempt, scope)?;
    if let Ok(_commit) = recovery.verify_settlement(attempt) {
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
        let recovery = if let Some(writer) = &task.writer {
            let directory = self.writer_custody_directory(&record, writer)?;
            self.recover_writer_exit(directory, Self::scope(&record), call)?
        } else {
            None
        };
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
        } else if !import_retains_custody(attempt, exit.scope)? {
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
        match custody::acquire_recovery(directory, call.budget, call.cancel, call.clock) {
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
        if import_retains_custody(attempt, recovery.scope())? {
            return Ok(());
        }
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
        let settlement = completion
            .custody()
            .map(|recovery| recovery.try_settlement_guard())
            .transpose()
            .map_err(|_| RunError::Contention)?;
        let record = self.finish_in_custody_inner(
            id,
            attempt,
            exit,
            completion,
            settlement.as_ref(),
            providers,
            call,
        )?;
        if matches!(record.state, AttemptState::Terminal { .. }) {
            // Also runs on terminal replay, repairing a crash between publishing
            // the terminal transaction and pruning the host-owned retry rows.
            clear_settlement_retries(&self.writer_registry, exit.scope, &terminal_request(call)?)?;
        }
        Ok(record)
    }

    // Keep the settlement guard explicit beside the custody-bearing completion.
    #[allow(clippy::too_many_arguments)]
    fn finish_in_custody_inner(
        &self,
        id: AttemptId,
        attempt: &Directory,
        exit: &ExitObservation,
        completion: Completion<'_>,
        settlement: Option<&custody::SettlementGuard>,
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
        let finished_at = call
            .clock
            .wall_now()
            .map_err(|_| RunError::Validation("wall clock"))?;
        let cancelled = read_sealed(attempt, "cancel.json", RECORD_LIMIT)?.is_some();
        let mut decoded = match (&record.applied_route, exit.never_spawned) {
            // Cancelled before spawn; keep the guardian's own reason too.
            (_, true) if cancelled => Decoded {
                outcome: Outcome::Cancelled,
                result: String::new(),
                complete: true,
                observed_route: None,
                usage: UsageRecord::default(),
                session: None,
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
                reason: Some(
                    exit.warnings
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "provider never spawned".into()),
                ),
            },
            (Some(route), false) => {
                let stdout = read_sealed(attempt, "stdout.log", CAPTURE_LIMIT)?.unwrap_or_default();
                let provider_state = self
                    .context
                    .state_root
                    .as_path()
                    .join("runs")
                    .join(record.run_id.to_string())
                    .join("attempts")
                    .join(id.to_string())
                    .join("provider-state");
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
        let mut review_required = false;
        let mut legacy_policy_note = None;
        if let Some(writer) = &task.writer {
            if crate::objects::legacy_policy(attempt)? {
                legacy_policy_note = Some(crate::objects::legacy_policy_diagnostic(&format!(
                    "Run {} Attempt {id}",
                    record.run_id
                )));
            }
            let refusal = object_import_refused(attempt, exit.scope)?;
            let refused = refusal.is_some();
            let prior_limit = if refused {
                None
            } else {
                retry_limit(settlement_retries(
                    &self.writer_registry,
                    exit.scope,
                    call,
                    None,
                )?)
            };
            let check = if let Some(reason) = refusal {
                decoded.outcome = Outcome::Failed;
                decoded.reason = Some(reason);
                Ok(false)
            } else if let Some(error) = prior_limit {
                Err(error)
            } else {
                crate::prepare_writer(writer, &self.writer_registry, record.project_id).and_then(
                    |writer| {
                        writer.terminal(
                            self.context.state_root.as_path(),
                            attempt,
                            recovery
                                .ok_or(RunError::Validation("writer terminal custody missing"))?,
                            settlement
                                .ok_or(RunError::Validation("writer settlement guard missing"))?,
                            task.deliver
                                .as_ref()
                                .map_or("origin", |deliver| deliver.remote.as_str()),
                            task.base.as_deref(),
                        )
                    },
                )
            };
            // Object work has its own real-time deadlines. It must not consume
            // the caller's database budget and prevent recording success or a
            // retry once that work finishes.
            let terminal_call = terminal_request(call)?;
            let call = &terminal_call;
            let check = match check {
                Err(error @ RunError::ObjectStoreBusy) => {
                    match retry_limit(settlement_retries(
                        &self.writer_registry,
                        exit.scope,
                        call,
                        Some(false),
                    )?) {
                        Some(limit) => Err(limit),
                        None => return Err(error),
                    }
                }
                Err(error @ RunError::ObjectVerificationRetry(_)) => {
                    match retry_limit(settlement_retries(
                        &self.writer_registry,
                        exit.scope,
                        call,
                        Some(true),
                    )?) {
                        Some(limit) => Err(limit),
                        None => return Err(error),
                    }
                }
                other => other,
            };
            match check {
                Err(error @ RunError::ObjectImportLimit(limit)) => {
                    let release_custody = matches!(
                        limit,
                        ObjectImportLimitReason::VerificationRetries
                            | ObjectImportLimitReason::StoreBusyRetries
                            | ObjectImportLimitReason::StagedBytes
                    );
                    if release_custody {
                        seal_retry_refusal(attempt, exit.scope)?;
                    }
                    let reason = format!(
                        "writer object import refused for {}: {error}{}",
                        writer.worktree.display(),
                        if release_custody {
                            ""
                        } else {
                            "; private objects and writer custody retained for recovery"
                        }
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
                            release_custody,
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
                                launch.uncommitted(
                                    self.context.state_root.as_path(),
                                    recovery.ok_or(RunError::Validation(
                                        "writer terminal custody missing",
                                    ))?,
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
                    Some(detail) if reason == "provider identity was not observed" => {
                        format!("{reason}: {detail}")
                    }
                    _ => reason.into(),
                });
            }
        }
        if decoded.outcome == Outcome::Succeeded && review_required {
            decoded.outcome = Outcome::ReviewRequired;
            decoded.reason = Some("protected changes require review".into());
        }
        // Recovered/result-only publication never starts external delivery.
        // A succeeded outcome already proves the provider's terminal event in
        // the sealed stdout, so no live frame observation is required.
        let result_orphan = read_sealed(attempt, "result.md", RESULT_LIMIT)?.is_some();
        if !result_orphan
            && decoded.outcome == Outcome::Succeeded
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
            // Delivery can refresh and verify another object graph. Preserve
            // the reservation and settlement guard while letting cancellation
            // and other custody observers take writer.lock during this work.
            recovery
                .ok_or(RunError::Validation("delivery custody missing"))?
                .without_writer_lease(
                    settlement.ok_or(RunError::Validation("delivery settlement guard missing"))?,
                    || {
                        crate::delivery::deliver_once(
                            host,
                            writer,
                            deliver,
                            attempt,
                            &format!("Run {}\nAttempt {id}", record.run_id),
                        )
                    },
                )
                .map_err(|error| RunError::Io(std::io::Error::other(error.to_string())))??;
        }
        // Terminal state is also published as a run event. Record this host
        // warning there without changing validation of the provider's result.
        if let Some(note) = legacy_policy_note {
            decoded.reason = Some(match decoded.reason.take() {
                Some(reason) => format!("{reason}; {note}"),
                None => note,
            });
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
            result: &decoded.result,
            complete: decoded.complete,
            redacted: false,
            resolved_route: record.resolved_route.clone(),
            applied_route: record.applied_route.clone(),
            observed_route: decoded.observed_route,
            usage: decoded.usage,
            session: decoded.session,
        };
        let terminal_call = terminal_request(call)?;
        let call = &terminal_call;
        // A result sealed without intent predates a crash: keep its bytes.
        if result_orphan {
            return self.recover_result_only(id, &publication, exit, call);
        }
        self.publish_terminal(id, record.revision, &publication, exit, call)
    }
}

#[cfg(test)]
mod host_clock_tests {
    use super::*;

    #[test]
    fn admission_failures_reach_guardian_as_typed_causes() {
        assert!(matches!(
            custody_error(RunError::BuildSettings),
            CustodyError::AdmissionBuildSettings
        ));
        assert!(matches!(
            custody_error(RunError::GuardianIdentityUnavailable),
            CustodyError::AdmissionGuardianIdentity
        ));
    }

    #[test]
    fn an_unreadable_host_clock_reaches_the_guardian_as_itself() {
        assert!(matches!(
            custody_error(RunError::HostClock),
            CustodyError::AdmissionClock
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
            custody_error(RunError::DiskFloor {
                free: 99,
                floor: 100
            }),
            CustodyError::AdmissionDisk {
                free: 99,
                floor: 100
            }
        ));
    }
}

#[cfg(test)]
mod writer_recovery_tests {
    use super::*;
    use crate::tests::{call, clock, context, input, route};

    struct CertificateClock {
        inner: provenant_core::TestClock,
        certificate: std::cell::RefCell<Option<PathBuf>>,
    }
    impl Clock for CertificateClock {
        fn wall_now(
            &self,
        ) -> std::result::Result<provenant_core::WallTime, provenant_core::TimeError> {
            self.inner.wall_now()
        }
        fn mono_now(&self) -> provenant_core::MonoTime {
            let elapsed = if self
                .certificate
                .borrow()
                .as_ref()
                .is_some_and(|path| path.exists())
            {
                31_000_000
            } else {
                0
            };
            provenant_core::MonoTime::from_micros(self.inner.mono_now().as_micros() + elapsed)
        }
        fn wait_until(
            &self,
            deadline: provenant_core::MonoTime,
        ) -> std::result::Result<(), provenant_core::TimeError> {
            self.inner.wait_until(deadline)
        }
        fn boot_identity(&self) -> std::result::Result<String, provenant_core::TimeError> {
            self.inner.boot_identity()
        }
        fn host_monotonic_micros(&self) -> std::result::Result<u64, provenant_core::TimeError> {
            self.inner.host_monotonic_micros()
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum ImportCase {
        PackLimit,
        LockWait,
        Deadline,
        BusyCap,
        StagedBytes,
        ExpiredCall,
        LegacyPolicy,
        ConcurrentCancel,
    }

    #[test]
    fn settlement_retry_counts_persist_and_are_scoped_to_attempt()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let host = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let run = input();
        let scope = custody::Scope {
            run: run.run_id,
            attempt: run.attempts[0].attempt_id,
            generation: 1,
        };
        assert_eq!(
            settlement_retries(host.path(), scope, &call, Some(true))?,
            (1, 0)
        );
        // Each invocation opens the database again, as a restarted holder does.
        assert_eq!(settlement_retries(host.path(), scope, &call, None)?, (1, 0));
        let next = custody::Scope {
            attempt: AttemptId::from_bytes([4; 16]),
            ..scope
        };
        assert_ne!(next.attempt, scope.attempt);
        assert_eq!(settlement_retries(host.path(), next, &call, None)?, (0, 0));
        Ok(())
    }

    #[test]
    fn object_import_limit_is_terminal_and_retains_private_evidence_and_custody()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        object_import_refusal_fixture(ImportCase::PackLimit)
    }

    #[test]
    fn object_store_lock_timeout_leaves_settlement_nonterminal_and_can_be_retried()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        object_import_refusal_fixture(ImportCase::LockWait)
    }

    #[test]
    fn verification_deadline_cap_seals_and_releases_custody()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        object_import_refusal_fixture(ImportCase::Deadline)
    }

    #[test]
    fn busy_cap_is_separate_and_releases_custody()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        object_import_refusal_fixture(ImportCase::BusyCap)
    }

    #[test]
    fn staged_bytes_limit_through_holder_seals_and_releases_custody()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        object_import_refusal_fixture(ImportCase::StagedBytes)
    }

    #[test]
    fn verification_can_outlive_reconcile_budget_and_still_publish_terminal()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        object_import_refusal_fixture(ImportCase::ExpiredCall)
    }

    #[test]
    fn concurrent_cancel_and_reconcile_progress_while_verification_is_paused()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        object_import_refusal_fixture(ImportCase::ConcurrentCancel)
    }

    #[test]
    fn legacy_git_policy_warning_names_run_and_attempt_in_the_terminal_event()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        object_import_refusal_fixture(ImportCase::LegacyPolicy)
    }

    fn object_import_refusal_fixture(
        case: ImportCase,
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        use crate::delivery_tests::{PrepareFixture, registered_landed_writer_fixture};
        use provenant_os::fs::LockMode;
        use std::ffi::OsStr;
        let lock_wait = matches!(case, ImportCase::LockWait | ImportCase::BusyCap);

        let fixture = PrepareFixture::new()?;
        let execution = registered_landed_writer_fixture(&fixture)?;
        let clock = CertificateClock {
            inner: clock(),
            certificate: std::cell::RefCell::new(None),
        };
        let cancel = CancelToken::new();
        let call = Request {
            clock: &clock,
            budget: WaitBudget::cli(clock.mono_now(), 30)?,
            cancel: &cancel,
        };
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
        let temporary = store.temporary_path(id, &call)?;
        let candidate = temporary.join("candidate-0");
        if case == ImportCase::ExpiredCall {
            clock.certificate.replace(Some(
                temporary
                    .parent()
                    .ok_or("attempt path")?
                    .join("writer-terminal.json"),
            ));
        }
        std::fs::create_dir_all(&candidate)?;
        let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
        if case != ImportCase::LegacyPolicy {
            crate::objects::prepare_bound(
                &git,
                launch.worktree.repo(),
                &attempt,
                &candidate,
                launch.branch.as_str(),
                launch.base,
            )?;
        } else {
            std::fs::create_dir_all(candidate.join("git-objects"))?;
        }
        let packs = candidate.join("git-objects/pack");
        std::fs::create_dir(&packs)?;
        if case == ImportCase::StagedBytes {
            let name = launch.base.to_string();
            let fanout = candidate.join("git-objects").join(&name[..2]);
            std::fs::create_dir(&fanout)?;
            std::fs::copy(
                launch
                    .worktree
                    .repo()
                    .common_git_dir()
                    .join("objects")
                    .join(&name[..2])
                    .join(&name[2..]),
                fanout.join(&name[2..]),
            )?;
            crate::objects::TEST_STAGED_HASH_BUDGET.set(Some(std::time::Duration::ZERO));
        }
        for number in 0..if case == ImportCase::PackLimit { 65 } else { 0 } {
            std::fs::write(packs.join(format!("pack-{number:040x}.pack")), b"untrusted")?;
        }
        std::fs::write(candidate.join("recovery.lock"), b"private evidence")?;
        let (custody_directory, binding) = launch.custody.take().ok_or("writer custody")?;
        let lease = provenant_supervisor::fixture::acquire_writer_with_binding(
            custody_directory,
            scope,
            binding,
            call.budget,
            &cancel,
            &clock,
        )?;
        let recovery =
            provenant_supervisor::fixture::bind_writer_attempt(lease, &attempt)?.into_recovery()?;
        let mut observed = crate::tests::exit();
        observed.scope = scope;
        crate::dispatch_tests::seal_exit(&store, &observed)?;
        let shared_lock = if lock_wait {
            let common = launch.worktree.repo().common_dir();
            common.create_new(OsStr::new("provenant-objects.lock"))?;
            Some(common.try_lock_existing(OsStr::new("provenant-objects.lock"), LockMode::Shared)?)
        } else {
            None
        };
        if lock_wait {
            crate::objects::TEST_LOCK_BUDGET.set(Some(std::time::Duration::from_millis(100)));
        }
        if case == ImportCase::Deadline {
            crate::objects::TEST_VERIFICATION_BUDGET.set(Some(std::time::Duration::ZERO));
        }
        if case == ImportCase::ConcurrentCancel {
            let execution = execution.clone();
            let writer = fixture.writer.clone();
            let observed = observed.clone();
            let slot = store.writer_custody_directory(&record, &writer)?;
            crate::objects::TEST_VERIFICATION_HOOK.replace(Some(Box::new(move || {
                // The verifier is paused until both calls complete. No timing
                // race can accidentally let them run after verification ends.
                let (send, receive) = std::sync::mpsc::channel();
                let worker = std::thread::spawn(move || {
                    let result = (|| -> std::result::Result<(), String> {
                        let clock = SystemClock::new();
                        let cancel = CancelToken::new();
                        let call = Request {
                            clock: &clock, cancel: &cancel,
                            budget: WaitBudget::cli(clock.mono_now(), 2).map_err(|e| e.to_string())?,
                        };
                        let lease = slot.try_lock_existing(OsStr::new("writer.lock"), LockMode::Exclusive)
                            .map_err(|e| format!("verification still holds writer.lock: {e}"))?;
                        drop(lease);
                        let store = RunStore::open(&execution, &call).map_err(|e| e.to_string())?;
                        let receipt = store.cancel(id, "concurrent cancel", &call).map_err(|e| e.to_string())?;
                        assert!(!matches!(&receipt, CancelReceipt::Unobservable { reason } if reason.contains("budget") || reason.contains("contended")));
                        let attempt = store.attempt_dir(scope.run, id, false).map_err(|e| e.to_string())?;
                        assert!(matches!(store.finish(id, &attempt, &observed,
                            &crate::dispatch_tests::Fake::default(), &call), Err(RunError::Contention)));
                        Ok(())
                    })();
                    let _ = send.send(result);
                });
                receive.recv_timeout(std::time::Duration::from_secs(1))
                    .expect("cancel/reconcile blocked behind paused verification")
                    .expect("concurrent custody calls");
                worker.join().expect("concurrent custody worker");
            })));
        }
        let first = store.finish_in_custody(
            id,
            &attempt,
            &observed,
            Completion::Recovered(Some(&recovery)),
            &crate::dispatch_tests::Fake::default(),
            &call,
        );
        crate::objects::TEST_LOCK_BUDGET.set(None);
        crate::objects::TEST_VERIFICATION_BUDGET.set(None);
        crate::objects::TEST_STAGED_HASH_BUDGET.set(None);
        if case == ImportCase::ConcurrentCancel {
            assert!(matches!(first?.state, AttemptState::Terminal { .. }));
            store.settle_recovered(&attempt, recovery)?;
            assert!(store.writer_fence_listing()?.reservations.is_empty());
            return Ok(());
        }
        if case == ImportCase::ExpiredCall {
            let terminal = first?;
            assert!(matches!(terminal.state, AttemptState::Terminal { .. }));
            assert_eq!(call.budget.remaining(clock.mono_now()).as_micros(), 0);
            assert!(matches!(store.record(id, &call), Err(RunError::Contention)));
            assert_eq!(store.record(id, &terminal_request(&call)?)?, terminal);
            store.settle_recovered(&attempt, recovery)?;
            assert!(store.writer_fence_listing()?.reservations.is_empty());
            return Ok(());
        }
        if matches!(case, ImportCase::Deadline | ImportCase::BusyCap) {
            let expected = if case == ImportCase::Deadline {
                VERIFICATION_RETRY_LIMIT
            } else {
                BUSY_RETRY_LIMIT
            };
            assert!(first.is_err());
            for index in 1..expected {
                if case == ImportCase::Deadline {
                    crate::objects::TEST_VERIFICATION_BUDGET.set(Some(std::time::Duration::ZERO));
                } else {
                    crate::objects::TEST_LOCK_BUDGET.set(Some(std::time::Duration::from_millis(1)));
                }
                let result = store.finish_in_custody(
                    id,
                    &attempt,
                    &observed,
                    Completion::Recovered(Some(&recovery)),
                    &crate::dispatch_tests::Fake::default(),
                    &call,
                );
                crate::objects::TEST_VERIFICATION_BUDGET.set(None);
                crate::objects::TEST_LOCK_BUDGET.set(None);
                if index + 1 == expected {
                    assert!(matches!(
                        result?.state,
                        AttemptState::Terminal {
                            outcome: Outcome::Failed,
                            ..
                        }
                    ));
                } else {
                    assert!(result.is_err());
                }
            }
            let counts = settlement_retries(&store.writer_registry, scope, &call, None)?;
            assert_eq!(counts, (0, 0), "terminal publication prunes retry rows");
            let reason =
                object_import_refused(&attempt, scope)?.ok_or("sealed cap refusal missing")?;
            assert!(reason.contains(if case == ImportCase::Deadline {
                "verification retry cap"
            } else {
                "lock retry cap"
            }));
            let failed = store.record(id, &call)?;
            settlement_retries(&store.writer_registry, scope, &call, Some(true))?;
            assert_eq!(
                store.finish_in_custody(
                    id,
                    &attempt,
                    &observed,
                    Completion::Recovered(Some(&recovery)),
                    &crate::dispatch_tests::Fake::default(),
                    &call
                )?,
                failed
            );
            assert_eq!(
                settlement_retries(&store.writer_registry, scope, &call, None)?,
                (0, 0)
            );
            drop(shared_lock);
            store.settle_recovered(&attempt, recovery)?;
            assert!(read_sealed(&attempt, "release.json", RECORD_LIMIT)?.is_some());
            assert!(store.writer_fence_listing()?.reservations.is_empty());
            return Ok(());
        }
        if case == ImportCase::StagedBytes {
            let failed = first?;
            assert!(
                matches!(&failed.state, AttemptState::Terminal { outcome: Outcome::Failed, reason: Some(reason), .. } if reason.contains("staged object hashing"))
            );
            assert!(object_import_refused(&attempt, scope)?.is_some());
            assert!(!import_retains_custody(&attempt, scope)?);
            assert!(
                read_sealed(
                    &attempt.open_verified_directory(OsStr::new("quarantine"))?,
                    "manifest.json",
                    RECORD_LIMIT
                )?
                .is_some()
            );
            release_and_reclaim(&attempt, scope, &recovery)?;
            store.settle_recovered(&attempt, recovery)?;
            assert!(read_sealed(&attempt, "release.json", RECORD_LIMIT)?.is_some());
            assert!(store.writer_fence_listing()?.reservations.is_empty());
            assert!(
                !candidate.exists(),
                "released staged-byte refusal reclaims scratch"
            );
            return Ok(());
        }
        if lock_wait {
            assert!(matches!(first, Err(RunError::ObjectStoreBusy)));
            assert!(!matches!(
                store.record(id, &call)?.state,
                AttemptState::Terminal { .. }
            ));
            assert!(object_import_refused(&attempt, scope)?.is_none());
            assert_eq!(
                settlement_retries(&store.writer_registry, scope, &call, None)?,
                (0, 1)
            );
            assert!(read_sealed(&attempt, "terminal-pending.json", RECORD_LIMIT)?.is_none());
            assert!(read_sealed(&attempt, "writer-terminal.json", RECORD_LIMIT)?.is_none());
            assert!(read_sealed(&attempt, "release.json", RECORD_LIMIT)?.is_none());
            assert!(candidate.exists());
            assert_eq!(store.writer_fence_listing()?.reservations.len(), 1);
            drop(shared_lock);
            let retried = store.finish_in_custody(
                id,
                &attempt,
                &observed,
                Completion::Recovered(Some(&recovery)),
                &crate::dispatch_tests::Fake::default(),
                &call,
            )?;
            assert!(matches!(retried.state, AttemptState::Terminal { .. }));
            assert!(read_sealed(&attempt, "writer-terminal.json", RECORD_LIMIT)?.is_some());
            assert!(object_import_refused(&attempt, scope)?.is_none());
            assert_eq!(
                settlement_retries(&store.writer_registry, scope, &call, None)?,
                (0, 0)
            );
            return Ok(());
        }
        let failed = first?;
        if case == ImportCase::LegacyPolicy {
            let events = store.events(0, 64, Some(id), &call)?;
            assert!(events.iter().any(|event| matches!(&event.change, AttemptState::Terminal { reason: Some(reason), .. }
                if reason.contains("legacy pre-v11 Git grants policy") && reason.contains(&run.run_id.to_string()) && reason.contains(&id.to_string())
            )));
            assert!(
                matches!(&failed.state, AttemptState::Terminal { reason: Some(reason), .. } if reason.contains("legacy pre-v11"))
            );
            return Ok(());
        }
        assert!(matches!(
            &failed.state,
            AttemptState::Terminal { outcome: Outcome::Failed, reason: Some(reason), .. }
                if reason.contains("pack") && reason.contains("64")
        ));
        assert!(object_import_refused(&attempt, scope)?.is_some());
        assert!(
            object_import_refused(
                &attempt,
                custody::Scope {
                    generation: scope.generation + 1,
                    ..scope
                }
            )
            .is_err()
        );
        let archive = attempt.open_verified_directory(OsStr::new("quarantine"))?;
        assert!(read_sealed(&archive, "manifest.json", RECORD_LIMIT)?.is_none());
        assert!(read_sealed(&attempt, "writer-terminal.json", RECORD_LIMIT)?.is_none());
        release_and_reclaim(&attempt, scope, &recovery)?;
        store.settle_recovered(&attempt, recovery)?;
        assert_eq!(
            std::fs::read(candidate.join("recovery.lock"))?,
            b"private evidence"
        );
        assert_eq!(std::fs::read_dir(&packs)?.count(), 65);
        assert!(read_sealed(&attempt, "release.json", RECORD_LIMIT)?.is_none());
        assert_eq!(store.writer_fence_listing()?.reservations.len(), 1);
        // Retrying a completed refusal preserves its outcome and the fence.
        assert_eq!(
            store.finish(
                id,
                &attempt,
                &observed,
                &crate::dispatch_tests::Fake::default(),
                &call
            )?,
            failed
        );
        assert!(candidate.exists());
        assert_eq!(store.writer_fence_listing()?.reservations.len(), 1);
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
            call.budget,
            call.cancel,
            call.clock,
        )?);
        let recovery = store
            .recover_writer_exit(directory()?, scope, &call)?
            .ok_or("prepared reservation must remain recoverable")?;
        recovery.withdraw_prepared()?;
        store.withdraw(
            id,
            AttemptState::Starting,
            Outcome::Cancelled,
            "fixture cancellation",
            clock.wall_now()?,
            &call,
        )?;
        // The caller holds a starting snapshot while the guardian has committed
        // cancellation and removed the reservation.
        assert_eq!(starting.state, AttemptState::Starting);
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
        std::fs::write(custody_dir.path().join("reservation.json"), b"{")?;
        assert!(
            store
                .recover_writer_exit(directory()?, scope, &call)
                .is_err(),
            "terminal state must not hide corrupt retained custody"
        );
        Ok(())
    }
}
