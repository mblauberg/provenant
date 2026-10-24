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
    /// task-local refusal reason; siblings still launch.
    fn prepare(
        &self,
        task: &TaskRequest,
        prompt: &str,
    ) -> std::result::Result<PreparedLaunch, String>;
    /// Decode the sealed stdout capture of a provider that exited.
    fn decode(&self, route: &RouteRecord, stdout: &[u8], exit: &ExitObservation) -> Decoded;
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
            started.push(match providers.prepare(task, &prompt) {
                Err(reason) => {
                    let now = call
                        .clock
                        .wall_now()
                        .map_err(|_| RunError::Validation("wall clock"))?;
                    self.refuse(id, &reason, now, call)?;
                    Started::Refused {
                        attempt: id,
                        reason,
                    }
                }
                Ok(plan) => match self.hand_off(task, plan, context, id, call) {
                    Ok(()) => Started::Launched(id),
                    Err(error) => {
                        self.launch_unknown(run, call)?;
                        Started::Unknown {
                            attempt: id,
                            error: error.to_string(),
                        }
                    }
                },
            });
        }
        Ok(started)
    }
    fn hand_off(
        &self,
        task: &TaskRequest,
        plan: PreparedLaunch,
        context: &dyn Fn(&AttemptRecord) -> Result<Vec<u8>>,
        id: AttemptId,
        call: &Request<'_>,
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let record = self.mark_starting(id, &plan.route, call)?;
        let scope = Scope {
            run: record.run_id,
            attempt: id,
            generation: record.generation,
        };
        let mut lease = custody::acquire_reader(
            self.attempt_dir(record.run_id, id, false)?,
            scope,
            call.budget,
            call.cancel,
            call.clock,
        )?;
        let reservation = PublicationReservation::prepare(
            self.attempt_dir(record.run_id, id, false)?,
            call.budget,
            call.cancel,
            call.clock,
        )?;
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
            holder_context: context(&record)?,
        };
        let capture = CapturePlan {
            wall_ms: task.provider_timeout_seconds.saturating_mul(1000),
            ..CapturePlan::default()
        };
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
        .map_err(|failure| failure.error)?;
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
