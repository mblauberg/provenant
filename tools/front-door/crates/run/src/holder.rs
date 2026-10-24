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
}
impl HolderContext {
    pub fn encode(&self) -> Result<Vec<u8>> {
        crate::db::bytes(self)
    }
    /// Open the store, retrying transient failures for about 30 s so a busy
    /// host does not turn into a refused or stranded attempt.
    fn open(raw: &[u8]) -> Result<(Self, RunStore)> {
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
        let store = RunStore::open(&execution, &call(&SystemClock, &CancelToken::new())?)?;
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
    ) -> custody::Result<()> {
        let (context, store) = HolderContext::open(context).map_err(custody_error)?;
        let cancel = CancelToken::new();
        store
            .finish(
                context.attempt_id,
                attempt,
                exit,
                self.providers,
                &call(&SystemClock, &cancel).map_err(custody_error)?,
            )
            .map_err(custody_error)?;
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
            (_, true) if cancelled => Decoded {
                outcome: Outcome::Cancelled,
                result: String::new(),
                complete: true,
                observed_route: None,
                usage: UsageRecord::default(),
                reason: Some("cancelled before start".into()),
            },
            (_, true) => Decoded {
                outcome: Outcome::Rejected,
                result: String::new(),
                complete: true,
                observed_route: None,
                usage: UsageRecord::default(),
                reason: Some(
                    exit.warnings
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "provider never spawned".into()),
                ),
            },
            (Some(route), false) => {
                let stdout = read_sealed(attempt, "stdout.log", CAPTURE_LIMIT)?.unwrap_or_default();
                providers.decode(route, &stdout, exit)
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
                decoded.reason = Some(reason.into());
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
            result: &decoded.result,
            complete: decoded.complete,
            redacted: false,
            resolved_route: record.resolved_route.clone(),
            applied_route: record.applied_route.clone(),
            observed_route: decoded.observed_route,
            usage: decoded.usage,
        };
        self.publish_terminal(id, record.revision, &publication, exit, call)
    }
}
