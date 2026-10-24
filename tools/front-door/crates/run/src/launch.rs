//! Supervisor handoff for accepted attempts. The provider seam is minimal and
//! shaped for the providers crate's launch specification: the composition edge
//! resolves routes, applies fallback and builds the exact invocation.
use crate::{artifacts::read_sealed, db::decode, *};
use provenant_core::{AttemptId, OperationId, RunId};
use provenant_supervisor::custody::{
    self, CapturePlan, ExitObservation, OwnedLaunch, ProviderCommand, PublicationReservation, Scope,
};
use rusqlite::params;
use std::{ffi::OsString, path::PathBuf};

/// An explicit dispatch argument wins over the effective profile default.
pub fn concurrency(explicit: Option<u64>, profile: &HarnessProfile) -> u64 {
    explicit.unwrap_or_else(|| profile.knob_u64("lanes.max_concurrency").unwrap_or(8))
}

pub fn capture_plan(task: &TaskRequest) -> CapturePlan {
    CapturePlan {
        wall_ms: task.provider_timeout_seconds.saturating_mul(1000),
        silence_ms: task
            .profile
            .knob_u64("lanes.silence_seconds")
            .unwrap_or(300)
            .saturating_mul(1000),
        ..CapturePlan::default()
    }
}

/// One prepared provider invocation. Secret inputs stay in memory and travel
/// on the handoff pipe, then a dedicated read pipe into the provider.
pub struct PreparedLaunch {
    pub argv: Vec<OsString>,
    pub environment: Vec<(OsString, OsString)>,
    pub stdin: Vec<u8>,
    pub secret_input: Option<provenant_supervisor::exec::SecretInput>,
    pub cwd: PathBuf,
    /// Exact provider/model/effort the invocation applies.
    pub route: RouteRecord,
    pub confinement: Option<crate::LaunchConfinement>,
}
/// Provider-decoded facts from a completed capture.
pub struct Decoded {
    pub outcome: Outcome,
    pub result: String,
    pub complete: bool,
    pub observed_route: Option<RouteRecord>,
    pub usage: UsageRecord,
    pub reason: Option<String>,
}
/// Run-owned per-attempt paths. Allocated once, shared by explicit fallback candidates.
pub struct LaunchPaths {
    pub attempt_id: AttemptId,
    pub operation_id: OperationId,
    pub task_index: u64,
    pub lane_depth: u64,
    pub run_id: RunId,
    pub deadline_unix_ms: u64,
    pub scratch: PathBuf,
    pub provider_state: PathBuf,
    /// Private temporary directory for a read-only lane, never a shared system
    /// one. Terminal commit and withdrawal remove it best-effort.
    pub temporary: PathBuf,
}
pub trait Providers {
    /// Prepare one task, trying its fallback routes in order. An error is a
    /// task-local refusal reason; siblings still launch. `resume` is the prior
    /// terminal attempt of a continuation, whose exact session must be used:
    /// without one, refuse rather than start a fresh session.
    fn prepare(
        &self,
        task: &TaskRequest,
        prompt: &str,
        paths: &LaunchPaths,
        resume: Option<&AttemptRecord>,
    ) -> std::result::Result<PreparedLaunch, String>;
    /// Decode the sealed stdout capture of a provider that exited.
    fn decode(
        &self,
        route: &RouteRecord,
        stdout: &[u8],
        exit: &ExitObservation,
        provider_state: &std::path::Path,
    ) -> std::result::Result<Decoded, String>;
}
enum HandOff {
    /// Failed before anything was sent to a guardian.
    Local(String),
    Busy,
    /// The guardian may have received the launch.
    Uncertain(String),
}
#[derive(Debug)]
pub enum Started {
    Launched(AttemptId),
    Refused {
        attempt: AttemptId,
        reason: String,
        kind: RefusalKind,
    },
    /// Handoff did not confirm; the attempt stays fenced for `reconcile`.
    Unknown {
        attempt: AttemptId,
        error: String,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefusalKind {
    Preparation,
    WorktreeBusy,
}

impl RunStore {
    pub fn request(&self, run: RunId, call: &Request<'_>) -> Result<CanonicalRequest> {
        let raw = read_sealed(&self.run_dir(run, false)?, "request.json", RECORD_LIMIT)?
            .ok_or(RunError::NotFound)?;
        let request: CanonicalRequest = decode(&raw)?;
        if request.digest()? != self.run(run, call)?.request_digest {
            return Err(RunError::ArtifactMismatch);
        }
        Ok(request)
    }
    /// Hand each queued attempt of `run` to its own guardian, which admits it
    /// and later commits its terminal record. `context` builds the opaque
    /// holder reference for one attempt.
    pub fn start(
        &self,
        run: RunId,
        providers: &dyn Providers,
        context: &dyn Fn(&AttemptRecord) -> Result<Vec<u8>>,
        call: &Request<'_>,
    ) -> Result<Vec<Started>> {
        self.start_inner(
            run,
            providers,
            context,
            &|directory, scope, binding| {
                custody::acquire_writer(
                    directory,
                    scope,
                    binding,
                    provenant_core::WaitBudget::nonblocking(call.clock.mono_now()),
                    call.cancel,
                    call.clock,
                )
            },
            call,
        )
    }
    /// Native observation seam for integration fixtures. The ordinary start
    /// path always uses supervisor's system observer and nonblocking lease.
    #[cfg(feature = "fixture")]
    pub fn start_with_writer_lease(
        &self,
        run: RunId,
        providers: &dyn Providers,
        context: &dyn Fn(&AttemptRecord) -> Result<Vec<u8>>,
        acquire_writer: &dyn Fn(
            provenant_supervisor::fs::Directory,
            Scope,
            custody::WriterBinding,
        ) -> custody::Result<custody::CustodyLease>,
        call: &Request<'_>,
    ) -> Result<Vec<Started>> {
        self.start_inner(run, providers, context, acquire_writer, call)
    }
    fn start_inner(
        &self,
        run: RunId,
        providers: &dyn Providers,
        context: &dyn Fn(&AttemptRecord) -> Result<Vec<u8>>,
        acquire_writer: &dyn Fn(
            provenant_supervisor::fs::Directory,
            Scope,
            custody::WriterBinding,
        ) -> custody::Result<custody::CustodyLease>,
        call: &Request<'_>,
    ) -> Result<Vec<Started>> {
        let request = self.request(run, call)?;
        let resume = request
            .parent
            .map(|prior| self.record(prior, call))
            .transpose()?;
        let ids: Vec<String> = call.retry(|| {
            let mut query = self.db.prepare(
                "SELECT id FROM attempts WHERE run=?1 AND project=?2 AND terminal=0 ORDER BY sequence",
            )?;
            let rows = query.query_map(
                params![run.to_string(), self.context.project_id.to_string()],
                |r| r.get(0),
            )?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })?;
        let mut started = Vec::new();
        for id in ids {
            let id = AttemptId::parse(&id).map_err(|_| RunError::Validation("attempt index ID"))?;
            let record = self.record(id, call)?;
            if record.state != AttemptState::Queued {
                continue;
            }
            let (task_index, task) = request
                .tasks
                .iter()
                .enumerate()
                .find(|(_, task)| task.task_id == record.task_id)
                .ok_or(RunError::Validation("attempt task missing from request"))?;
            let task_index = u64::try_from(task_index)
                .map_err(|_| RunError::Validation("attempt task index"))?;
            let prompt = self.artifact_bytes(run, &record.prompt, RECORD_LIMIT)?;
            let prompt = String::from_utf8(prompt)
                .map_err(|_| RunError::Validation("prompt is not UTF-8"))?;
            let directory = self
                .context
                .state_root
                .as_path()
                .join(format!("runs/{run}/attempts/{id}"));
            let prompt = format!(
                "Durable output for this attempt: {result} (your final answer); {exit}, {stdout}, {stderr} (harness evidence). These are the only durable outputs. Put your report and evidence in your final answer; the harness writes these files. Empty attempt scratch is deleted at settlement; unknown contents and provider history remain retained. Files left in scratch are not deliverables.\n\n{prompt}",
                result = directory.join("result.md").display(),
                exit = directory.join("process-exit.json").display(),
                stdout = directory.join("stdout.log").display(),
                stderr = directory.join("stderr.log").display(),
            );
            let now = || {
                call.clock
                    .wall_now()
                    .map_err(|_| RunError::Validation("wall clock"))
            };
            let attempt = self.attempt_dir(record.run_id, id, false)?;
            crate::artifacts::subdir(&attempt, std::ffi::OsStr::new("scratch"), true)?;
            crate::artifacts::subdir(&attempt, std::ffi::OsStr::new("provider-state"), true)?;
            crate::artifacts::subdir(&attempt, std::ffi::OsStr::new("tmp"), true)?;
            let base = self
                .context
                .state_root
                .as_path()
                .join("runs")
                .join(record.run_id.to_string())
                .join("attempts")
                .join(id.to_string());
            let paths = LaunchPaths {
                attempt_id: id,
                operation_id: record.operation_id.clone(),
                task_index,
                lane_depth: request.lane_depth.unwrap_or(1),
                run_id: record.run_id,
                deadline_unix_ms: u64::try_from(now()?.as_micros() / 1000)
                    .unwrap_or(0)
                    .saturating_add(task.provider_timeout_seconds.saturating_mul(1000)),
                scratch: base.join("scratch"),
                provider_state: base.join("provider-state"),
                temporary: base.join("tmp"),
            };
            let mut plan = match providers.prepare(task, &prompt, &paths, resume.as_ref()) {
                Ok(plan) => plan,
                Err(reason) => {
                    crate::writer::settle_launch_staging(&attempt)?;
                    self.withdraw(
                        id,
                        AttemptState::Queued,
                        Outcome::Rejected,
                        &reason,
                        now()?,
                        call,
                    )?;
                    started.push(Started::Refused {
                        attempt: id,
                        reason,
                        kind: RefusalKind::Preparation,
                    });
                    continue;
                }
            };
            set_lane_environment(&mut plan.environment, request.lane_depth.unwrap_or(1), id);
            // Record the exact project-relative exceptions of the plan that launches.
            let writable = plan
                .confinement
                .as_ref()
                .and_then(|c| c.plan.permissions().read_only.as_ref())
                .map(|g| g.writable_paths())
                .unwrap_or_default();
            let record = match self.mark_starting(id, &plan.route, writable, call) {
                Ok(record) => record,
                // A concurrent start already took this attempt.
                Err(RunError::Conflict) => continue,
                Err(error) => return Err(error),
            };
            started.push(
                match self.hand_off(task, plan, context, acquire_writer, &record, call) {
                    Ok(()) => Started::Launched(id),
                    // Nothing reached a guardian: the attempt ends here.
                    Err(error @ (HandOff::Local(_) | HandOff::Busy)) => {
                        let (reason, kind) = match error {
                            HandOff::Busy => ("worktree is busy".into(), RefusalKind::WorktreeBusy),
                            HandOff::Local(error) => (
                                format!("launch preparation failed: {error}"),
                                RefusalKind::Preparation,
                            ),
                            _ => unreachable!(),
                        };
                        self.withdraw(
                            id,
                            AttemptState::Starting,
                            Outcome::Rejected,
                            &reason,
                            now()?,
                            call,
                        )?;
                        Started::Refused {
                            attempt: id,
                            reason,
                            kind,
                        }
                    }
                    Err(HandOff::Uncertain(error)) => {
                        self.launch_unknown(run, call)?;
                        Started::Unknown { attempt: id, error }
                    }
                },
            );
        }
        Ok(started)
    }
    fn hand_off(
        &self,
        task: &TaskRequest,
        mut plan: PreparedLaunch,
        context: &dyn Fn(&AttemptRecord) -> Result<Vec<u8>>,
        acquire_writer: &dyn Fn(
            provenant_supervisor::fs::Directory,
            Scope,
            custody::WriterBinding,
        ) -> custody::Result<custody::CustodyLease>,
        record: &AttemptRecord,
        call: &Request<'_>,
    ) -> std::result::Result<(), HandOff> {
        let local = |error: &dyn std::fmt::Display| HandOff::Local(error.to_string());
        let id = record.attempt_id;
        let scope = Scope {
            run: record.run_id,
            attempt: id,
            generation: record.generation,
        };
        let directory = || {
            self.attempt_dir(record.run_id, id, false)
                .map_err(|e| local(&e))
        };
        let mut holder_context = context(record).map_err(|e| local(&e))?;
        let reservation =
            PublicationReservation::prepare(directory()?, call.budget, call.cancel, call.clock)
                .map_err(|e| local(&e))?;
        let writer = plan.confinement.as_mut().and_then(|c| c.writer.as_mut());
        if task.writer.as_ref().map(|w| w.worktree.as_path())
            != writer.as_ref().map(|w| w.worktree.path())
        {
            return Err(local(&"writer confinement does not match the task"));
        }
        let mut lease = match writer {
            Some(writer) => {
                let (directory, binding) = writer
                    .custody
                    .take()
                    .ok_or_else(|| local(&"writer custody missing"))?;
                acquire_writer(directory, scope, binding)
            }
            None => {
                custody::acquire_reader(directory()?, scope, call.budget, call.cancel, call.clock)
            }
        }
        .map_err(|e| {
            if matches!(e, custody::CustodyError::Busy) {
                HandOff::Busy
            } else {
                local(&e)
            }
        })?;
        // Reconcile may have withdrawn this attempt before the lease existed.
        match self.record(id, call) {
            Ok(now) if now.revision == record.revision => {}
            Ok(_) => {
                lease
                    .withdraw_unlaunched()
                    .map_err(|e| HandOff::Uncertain(e.to_string()))?;
                return Err(local(&"attempt changed before handoff"));
            }
            Err(error) => {
                lease
                    .withdraw_unlaunched()
                    .map_err(|e| HandOff::Uncertain(e.to_string()))?;
                return Err(local(&error));
            }
        }
        if let Some(deliver) = &task.deliver {
            let prepare = || -> Result<Vec<u8>> {
                let mut holder: HolderContext = decode(&holder_context)?;
                let writer = task
                    .writer
                    .as_ref()
                    .ok_or(RunError::Validation("delivery requires a writer"))?;
                let host = holder
                    .host_delivery
                    .take()
                    .ok_or(RunError::Validation("delivery lacks host context"))?;
                holder.host_delivery = Some(host.prepare(writer, deliver)?);
                holder.encode()
            };
            match prepare() {
                Ok(context) => holder_context = context,
                Err(error) => {
                    lease
                        .withdraw_unlaunched()
                        .map_err(|e| HandOff::Uncertain(e.to_string()))?;
                    return Err(local(&error));
                }
            }
        }
        if let Some(confinement) = &plan.confinement {
            if let Some(writer) = &confinement.writer
                && let Err(error) =
                    writer.snapshot(self.context.state_root.as_path(), &directory()?, scope)
            {
                lease
                    .withdraw_unlaunched()
                    .map_err(|e| HandOff::Uncertain(e.to_string()))?;
                return Err(local(&error));
            }
            match confinement.command(&plan.argv, call.cancel) {
                Ok(argv) => plan.argv = argv,
                Err(error) => {
                    lease
                        .withdraw_unlaunched()
                        .map_err(|e| HandOff::Uncertain(e.to_string()))?;
                    return Err(local(&error));
                }
            }
        }
        // Route records name models as `adapter/model`; prefix only a
        // provider-native one (which may itself contain `/`).
        let provider = plan.route.provider.as_deref().unwrap_or_default();
        let model = plan.route.model.as_deref().unwrap_or_default();
        let route = match model.strip_prefix(provider) {
            Some(rest) if rest.starts_with('/') => model.to_owned(),
            _ => format!("{provider}/{model}"),
        };
        let command = ProviderCommand {
            resolved_route: route,
            permission_plan_digest: task.authority_digest,
            task: task.task_id,
            requester: self.context.actor_id,
            protected_commands: vec![],
            config_environment: vec![],
            argv: plan.argv,
            environment: plan.environment,
            cwd: plan.cwd,
            stdin: plan.stdin,
            secret_input: plan.secret_input,
            holder_context,
        };
        let capture = capture_plan(task);
        custody::launch(
            OwnedLaunch {
                command,
                lease: &mut lease,
                handshake: call.budget,
                cancel: call.cancel,
                clock: call.clock,
            },
            capture,
            reservation,
        )
        .map_err(|failure| HandOff::Uncertain(failure.error.to_string()))?;
        Ok(())
    }
    fn launch_unknown(&self, run: RunId, call: &Request<'_>) -> Result<()> {
        self.write(call, |tx| {
            let raw: Vec<u8> = tx.query_row(
                "SELECT record FROM operations WHERE run=?1 AND project=?2",
                params![run.to_string(), self.context.project_id.to_string()],
                |r| r.get(0),
            )?;
            let mut op: OperationRecord = decode(&raw)?;
            if op.state == OperationState::Terminal {
                return Ok(());
            }
            op.state = OperationState::LaunchUnknown;
            tx.execute(
                "UPDATE operations SET record=?1 WHERE run=?2",
                params![crate::db::bytes(&op)?, run.to_string()],
            )?;
            Ok(())
        })
    }
}

fn set_lane_environment(
    environment: &mut Vec<(OsString, OsString)>,
    depth: u64,
    parent: AttemptId,
) {
    set_environment_value(environment, LANE_DEPTH_ENV, depth.to_string());
    set_environment_value(environment, PARENT_ATTEMPT_ENV, parent.to_string());
}

fn set_environment_value(environment: &mut Vec<(OsString, OsString)>, name: &str, value: String) {
    let name = OsString::from(name);
    environment.retain(|(existing, _)| existing != &name);
    environment.push((name, OsString::from(value)));
}

#[cfg(test)]
mod lane_environment_tests {
    use super::*;

    #[test]
    fn lane_environment_overrides_adapter_values() {
        let parent = AttemptId::from_bytes([6; 16]);
        let mut environment = vec![
            (OsString::from(LANE_DEPTH_ENV), OsString::from("99")),
            (OsString::from("OTHER"), OsString::from("kept")),
        ];
        set_lane_environment(&mut environment, 2, parent);
        assert_eq!(
            environment,
            vec![
                (OsString::from("OTHER"), OsString::from("kept")),
                (OsString::from(LANE_DEPTH_ENV), OsString::from("2")),
                (
                    OsString::from(PARENT_ATTEMPT_ENV),
                    OsString::from(parent.to_string())
                ),
            ]
        );
    }
}
