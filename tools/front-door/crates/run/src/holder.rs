//! The attempt holder: Run's admission and terminal transaction, composed into
//! the same-binary guardian. It runs while the guardian still holds custody.
use crate::{artifacts::read_sealed, launch::Providers, *};
use provenant_core::{
    ActorId, AttemptId, CancelToken, Clock, ProjectId, RootInputs, RunId, WaitBudget,
};
use provenant_supervisor::{
    clock::SystemClock,
    custody::{self, CustodyError, ExitObservation},
    exec::Termination,
    fs::{Directory, LocalFs},
    machine::StopReason,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
    pub git: PathBuf,
    pub codex_home: Option<PathBuf>,
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
}

enum Completion<'a> {
    Live {
        custody: &'a custody::Recovery,
        delivery: Option<&'a crate::HostDeliveryContext>,
        /// Request digest the host sealed at launch.
        sealed: Option<&'a provenant_core::Digest>,
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
}
impl HolderContext {
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
                Err(RunError::Contention | RunError::Sqlite(_) | RunError::Io(_)) => {
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
            .resolve(&LocalFs)
            .map_err(|_| RunError::UnsafePath)?
            .state;
        if state_root.as_path() != context.roots.state_root {
            return Err(RunError::UnsafePath);
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
fn custody_error(error: RunError) -> CustodyError {
    CustodyError::Io(std::io::Error::other(error.to_string()))
}
impl provenant_supervisor::Holder for RunHolder<'_> {
    fn admit(&self, context: &[u8], attempt: &Directory) -> custody::Result<()> {
        let (context, store) = HolderContext::open(context).map_err(custody_error)?;
        store
            .admit(
                context.attempt_id,
                attempt,
                self.resources,
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
                },
                self.providers,
                &call(&SystemClock, &cancel).map_err(custody_error)?,
            )
            .map_err(custody_error)?;
        crate::writer::settle_launch_staging(attempt).map_err(custody_error)?;
        custody::publish_release(attempt, exit.scope)
    }
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
            if matches!(record.state, AttemptState::Terminal { .. }) {
                match directory.metadata_at(std::ffi::OsStr::new("reservation.json"), false) {
                    Err(error) if error.is_not_found() => return Ok(record),
                    Err(error) => return Err(error.into()),
                    Ok(_) => {}
                }
            }
            Some(
                custody::acquire_recovery(directory, call.budget, call.cancel, call.clock)
                    .map_err(|e| RunError::Io(std::io::Error::other(e.to_string())))?,
            )
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
        crate::writer::settle_launch_staging(attempt)?;
        if let Some(recovery) = recovery {
            custody::publish_release(attempt, exit.scope)
                .map_err(|e| RunError::Io(std::io::Error::other(e.to_string())))?;
            let commit = recovery
                .verify_settlement(attempt)
                .map_err(|e| RunError::Io(std::io::Error::other(e.to_string())))?;
            let mut scratch = provenant_supervisor::scratch::Scratch::new(attempt.try_clone()?)?;
            recovery
                .settle(commit, &mut scratch)
                .map_err(|e| RunError::Io(std::io::Error::other(e.to_string())))?;
        }
        Ok(record)
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
                providers
                    .decode(
                        route,
                        &stdout,
                        exit,
                        &self
                            .context
                            .state_root
                            .as_path()
                            .join("runs")
                            .join(record.run_id.to_string())
                            .join("attempts")
                            .join(id.to_string())
                            .join("provider-state"),
                    )
                    .map_err(RunError::ProviderEvidence)?
            }
            (None, false) => return Err(RunError::Validation("running attempt lacks a route")),
        };
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
        if let Some(writer) = &task.writer {
            let check = crate::prepare_writer(writer, &self.writer_registry, record.project_id)
                .and_then(|writer| {
                    writer.terminal(
                        self.context.state_root.as_path(),
                        attempt,
                        recovery.ok_or(RunError::Validation("writer terminal custody missing"))?,
                        task.deliver
                            .as_ref()
                            .map_or("origin", |deliver| deliver.remote.as_str()),
                    )
                });
            match check {
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
        if decoded.outcome == Outcome::Succeeded {
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
            crate::delivery::deliver_once(host, writer, deliver, attempt)?;
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
        // A result sealed without intent predates a crash: keep its bytes.
        if result_orphan {
            return self.recover_result_only(id, &publication, exit, call);
        }
        self.publish_terminal(id, record.revision, &publication, exit, call)
    }
}
