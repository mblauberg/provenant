//! Cloud transport driven by a graph attempt, including recovery and cancellation.
//! The graph owns scheduling and completion; the attempt store only retains the
//! sealed placement and remote identity. No plain CLI run or wait is started.
use crate::run::cloud_owner;
use provenant_core::AttemptId;
use provenant_fabric_runtime::graph::{AttemptObservation, AttemptOutcome};
use provenant_providers::cloud::{CloudProvider, RemoteJob, RemoteState};

/// Advance the same durable job on every graph tick. An uncertain submission
/// is recovered by its stored identity rather than submitted a second time.
pub fn advance<P: CloudProvider>(
    provider: &mut P,
    store: &provenant_run::RunStore,
    id: AttemptId,
    brief: &str,
    reference: &str,
    call: &provenant_run::Request<'_>,
    cancelling: bool,
) -> Result<Option<AttemptObservation>, String>
where
    P::Error: std::fmt::Debug,
{
    let record = store.record(id, call).map_err(|error| error.to_string())?;
    let Some(placement) = cloud_owner::placement(store, &record, call)? else {
        return Ok(None);
    };
    let job = match cloud_owner::load(store, id, call)? {
        Some(job) if job.state.is_terminal() => job,
        Some(job) if cancelling => {
            // A cancellation request is not evidence that custody ended. Poll
            // unconfirmed requests so a later remote finish can drain the graph.
            let job = if matches!(
                job.state,
                RemoteState::Submitting | RemoteState::CancellationUnconfirmed
            ) {
                cloud_owner::reconcile(provider, store, id, call)?
            } else {
                job
            };
            if job.state.is_terminal() {
                job
            } else {
                cloud_owner::cancel(provider, store, id, call)?
            }
        }
        Some(_) => cloud_owner::reconcile(provider, store, id, call)?,
        None if cancelling => {
            return Ok(Some(terminal(
                reference,
                AttemptOutcome::OtherFailure,
                false,
                None,
            )));
        }
        None => {
            let request = store
                .request(record.run_id, call)
                .map_err(|error| error.to_string())?;
            let task = request
                .tasks
                .iter()
                .find(|task| task.task_id == record.task_id)
                .ok_or("cloud task is missing")?;
            // The graph's prompt is reconstituted from its retained document on
            // replay. Check it against the sealed attempt before submission.
            if provenant_records::CanonicalV1::file_digest(brief.as_bytes()) != task.prompt_digest {
                return Err("cloud brief does not match the durable attempt".into());
            }
            let target = crate::domains::CloudTarget {
                provider: placement.provider,
                environment: placement.environment,
                repository: placement.repository,
                branch: placement.branch,
                model: placement.model,
                effort: placement.effort,
                deadline_seconds: placement.deadline_seconds,
            };
            let job = cloud_owner::prepare_job(
                id,
                &placement.target,
                &target,
                task.head
                    .as_deref()
                    .ok_or("cloud node requires a pinned source commit")?,
                call.clock,
            )?;
            cloud_owner::submit(provider, store, id, job, brief, call)?
        }
    };
    observation(&job, reference).map(Some)
}

fn terminal(
    reference: &str,
    outcome: AttemptOutcome,
    launched: bool,
    result: Option<Box<provenant_records::result_capture::CapturedResult>>,
) -> AttemptObservation {
    AttemptObservation::Terminal {
        attempt_ref: Some(reference.into()),
        outcome,
        launched,
        result,
        result_warning: None,
        result_path: None,
        branch: None,
        commit: None,
    }
}

fn observation(job: &RemoteJob, reference: &str) -> Result<AttemptObservation, String> {
    Ok(match &job.state {
        RemoteState::Submitting
        | RemoteState::Unknown { .. }
        | RemoteState::CancellationUnconfirmed => AttemptObservation::Unknown {
            attempt_ref: Some(reference.into()),
        },
        RemoteState::Running => AttemptObservation::Pending {
            attempt_ref: Some(reference.into()),
        },
        RemoteState::Finished { result } => {
            let checked = result
                .clone()
                .check(&job.job_id, &job.nonce, &job.commit, &job.requested_model)
                .map_err(|error| error.to_string())?;
            let mut capture = provenant_records::result_capture::ResultCapture::new(None);
            let encoded = serde_json::to_vec(result).map_err(|error| error.to_string())?;
            let safe = provenant_confinement::redact::known_shapes(&encoded)
                .map_err(|error| error.to_string())?;
            capture.push(&safe);
            terminal(
                reference,
                if checked.passed {
                    AttemptOutcome::Succeeded
                } else {
                    AttemptOutcome::TaskFailed
                },
                true,
                Some(Box::new(capture.finish())),
            )
        }
        RemoteState::SetupFailed { .. } => {
            terminal(reference, AttemptOutcome::OtherFailure, false, None)
        }
        RemoteState::Cancelled => terminal(reference, AttemptOutcome::OtherFailure, true, None),
    })
}

struct BudgetCli<'a> {
    inner: provenant_providers::cloud_cli::InstalledCloudCli<'a>,
    deadline: std::time::Instant,
}

#[allow(clippy::disallowed_methods)] // Bound all transport calls in this graph tick by one real deadline.
impl provenant_providers::cloud_cli::CloudCli for BudgetCli<'_> {
    fn run(&mut self, program: &str, args: &[&str], stdin: Option<&str>) -> Result<String, String> {
        self.inner.timeout = self
            .deadline
            .checked_duration_since(std::time::Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or("cloud graph tick timed out")?;
        provenant_providers::cloud_cli::CloudCli::run(&mut self.inner, program, args, stdin)
    }
}

/// The production graph edge selects a transport from the sealed placement.
#[allow(clippy::disallowed_methods)] // The installed transport consumes a real-time graph budget.
#[allow(clippy::too_many_arguments)] // One boundary carries the sealed attempt, transport budget and stop state.
pub fn installed(
    store: &provenant_run::RunStore,
    id: AttemptId,
    brief: &str,
    reference: &str,
    call: &provenant_run::Request<'_>,
    cwd: &std::path::Path,
    within: std::time::Duration,
    cancelling: bool,
) -> Result<Option<AttemptObservation>, String> {
    use provenant_providers::cloud_cli::{ClaudeCloud, CodexCloud, InstalledCloudCli};
    let record = store.record(id, call).map_err(|error| error.to_string())?;
    let Some(placement) = cloud_owner::placement(store, &record, call)? else {
        return Ok(None);
    };
    let executor = provenant_os::exec::SystemExecutor::new(1).map_err(|error| error.to_string())?;
    let within = within.min(std::time::Duration::from_secs(120));
    let cli = BudgetCli {
        inner: InstalledCloudCli {
            executor: &executor,
            cwd: cwd.into(),
            timeout: within,
        },
        deadline: std::time::Instant::now() + within,
    };
    match provenant_records::AdapterSpec::get(&placement.provider)
        .and_then(|spec| spec.cloud_transport)
    {
        Some(provenant_records::CloudTransport::Claude) => advance(
            &mut ClaudeCloud {
                cli,
                pending_cleanup: None,
            },
            store,
            id,
            brief,
            reference,
            call,
            cancelling,
        ),
        Some(provenant_records::CloudTransport::Codex) => advance(
            &mut CodexCloud { cli },
            store,
            id,
            brief,
            reference,
            call,
            cancelling,
        ),
        _ => Err(format!(
            "cloud target {} has unsupported provider {}",
            placement.target, placement.provider
        )),
    }
}
