//! Supervisor handoff for accepted attempts. The provider seam is minimal and
//! shaped for the providers crate's launch specification: the composition edge
//! resolves routes, applies fallback and builds the exact invocation.
use crate::{artifacts::read_sealed, db::decode, *};
use provenant_core::{AttemptId, RunId};
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

/// One prepared provider invocation. Secrets travel only in `environment`,
/// in memory and on the handoff pipe.
pub struct PreparedLaunch {
    pub argv: Vec<OsString>,
    pub environment: Vec<(OsString, OsString)>,
    pub stdin: Vec<u8>,
    pub cwd: PathBuf,
    /// Exact provider/model/effort the invocation applies.
    pub route: RouteRecord,
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
pub trait Providers {
    /// Prepare one task, trying its fallback routes in order. An error is a
    /// task-local refusal reason; siblings still launch. `resume` is the prior
    /// terminal attempt of a continuation, whose exact session must be used:
    /// without one, refuse rather than start a fresh session.
    fn prepare(
        &self,
        task: &TaskRequest,
        prompt: &str,
        resume: Option<&AttemptRecord>,
    ) -> std::result::Result<PreparedLaunch, String>;
    /// Decode the sealed stdout capture of a provider that exited.
    fn decode(&self, route: &RouteRecord, stdout: &[u8], exit: &ExitObservation) -> Decoded;
}
enum HandOff {
    /// Failed before anything was sent to a guardian.
    Local(String),
    /// The guardian may have received the launch.
    Uncertain(String),
}
#[derive(Debug)]
pub enum Started {
    Launched(AttemptId),
    Refused {
        attempt: AttemptId,
        reason: String,
    },
    /// Handoff did not confirm; the attempt stays fenced for `reconcile`.
    Unknown {
        attempt: AttemptId,
        error: String,
    },
}

impl RunStore {
    pub(crate) fn request(&self, run: RunId, call: &Request<'_>) -> Result<CanonicalRequest> {
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
            let task = request
                .tasks
                .iter()
                .find(|t| t.task_id == record.task_id)
                .ok_or(RunError::Validation("attempt task missing from request"))?;
            let prompt = self.artifact_bytes(run, &record.prompt, RECORD_LIMIT)?;
            let prompt = String::from_utf8(prompt)
                .map_err(|_| RunError::Validation("prompt is not UTF-8"))?;
            let directory = self
                .context
                .state_root
                .as_path()
                .join(format!("runs/{run}/attempts/{id}"));
            let prompt = format!(
                "Durable output for this attempt: {result} (your final answer); {exit}, {stdout}, {stderr} (harness evidence). These are the only durable outputs. Put your report and evidence in your final answer; the harness writes these files. Attempt tmp/scratch is disposable and deleted after success; files left there are not deliverables. Failed-attempt retention is unchanged.\n\n{prompt}",
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
            let plan = match providers.prepare(task, &prompt, resume.as_ref()) {
                Ok(plan) => plan,
                Err(reason) => {
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
                    });
                    continue;
                }
            };
            let record = match self.mark_starting(id, &plan.route, call) {
                Ok(record) => record,
                // A concurrent start already took this attempt.
                Err(RunError::Conflict) => continue,
                Err(error) => return Err(error),
            };
            started.push(match self.hand_off(task, plan, context, &record, call) {
                Ok(()) => Started::Launched(id),
                // Nothing reached a guardian: the attempt ends here.
                Err(HandOff::Local(error)) => {
                    let reason = format!("launch preparation failed: {error}");
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
                    }
                }
                Err(HandOff::Uncertain(error)) => {
                    self.launch_unknown(run, call)?;
                    Started::Unknown { attempt: id, error }
                }
            });
        }
        Ok(started)
    }
    fn hand_off(
        &self,
        task: &TaskRequest,
        plan: PreparedLaunch,
        context: &dyn Fn(&AttemptRecord) -> Result<Vec<u8>>,
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
        let mut lease =
            custody::acquire_reader(directory()?, scope, call.budget, call.cancel, call.clock)
                .map_err(|e| local(&e))?;
        // Reconcile may have withdrawn this attempt before the lease existed.
        match self.record(id, call) {
            Ok(now) if now.revision == record.revision => {}
            Ok(_) => return Err(local(&"attempt changed before handoff")),
            Err(error) => return Err(local(&error)),
        }
        let reservation =
            PublicationReservation::prepare(directory()?, call.budget, call.cancel, call.clock)
                .map_err(|e| local(&e))?;
        let route = format!(
            "{}/{}",
            plan.route.provider.as_deref().unwrap_or_default(),
            plan.route.model.as_deref().unwrap_or_default()
        );
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
            holder_context: context(record).map_err(|e| local(&e))?,
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
