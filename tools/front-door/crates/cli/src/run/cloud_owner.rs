//! Preparation of cloud verification identity at the run-owner boundary.
use crate::domains::CloudTarget;
use provenant_core::{AttemptId, Clock};
use provenant_providers::cloud::{RemoteJob, RemoteState};

/// Construct a submission identity before persisting the intent. The nonce is
/// advisory correlation evidence and is never used as a credential.
pub fn prepare_job(
    id: AttemptId,
    target_name: &str,
    target: &CloudTarget,
    commit: &str,
    clock: &dyn Clock,
) -> Result<RemoteJob, String> {
    let nonce = provenant_os::fs::entropy_128().map_err(|error| error.to_string())?;
    let nonce: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
    let now = clock
        .wall_now()
        .map_err(|error| error.to_string())?
        .unix_seconds();
    let deadline_unix_seconds = u64::try_from(now)
        .ok()
        .and_then(|now| now.checked_add(target.deadline_seconds))
        .ok_or("invalid cloud deadline")?;
    Ok(RemoteJob {
        job_id: id.to_string(),
        nonce,
        deadline_unix_seconds,
        provider: target.provider.clone(),
        target: target_name.to_owned(),
        environment: target.environment.clone(),
        repository: target.repository.clone(),
        branch: target.branch.clone(),
        remote_id: String::new(),
        commit: commit.to_owned(),
        requested_model: target.model.clone(),
        requested_effort: target.effort.clone(),
        state: RemoteState::Submitting,
        submit_error: None,
        submit_refused: false,
        cloud_transport: Default::default(),
        cloud_command: None,
    })
}

/// Match cloud transport context to the immutable, host-owned task request.
fn validate_intent(
    store: &provenant_run::RunStore,
    id: AttemptId,
    job: &RemoteJob,
    call: &provenant_run::Request<'_>,
) -> Result<(), String> {
    let attempt = store.record(id, call).map_err(|error| error.to_string())?;
    let request = store
        .request(attempt.run_id, call)
        .map_err(|error| error.to_string())?;
    let task = request
        .tasks
        .iter()
        .find(|task| task.task_id == attempt.task_id)
        .ok_or("cloud task is missing")?;
    let placement = task
        .cloud
        .as_ref()
        .ok_or("attempt has no cloud placement")?;
    if job.job_id != id.to_string()
        || task.head.as_deref() != Some(job.commit.as_str())
        || placement.target != job.target
        || placement.provider != job.provider
        || placement.environment != job.environment
        || placement.repository != job.repository
        || placement.branch != job.branch
        || placement.model != job.requested_model
        || placement.effort != job.requested_effort
    {
        return Err("cloud job does not match the durable task request".into());
    }
    Ok(())
}

fn state_digest(job: &RemoteJob) -> Result<String, String> {
    let bytes = serde_json::to_vec(&job.state).map_err(|error| error.to_string())?;
    Ok(provenant_core::hex(
        provenant_records::CanonicalV1::file_digest(&bytes).bytes(),
    ))
}

fn note_state(
    store: &provenant_run::RunStore,
    id: AttemptId,
    job: &RemoteJob,
    call: &provenant_run::Request<'_>,
) -> Result<(), String> {
    let state = serde_json::to_value(&job.state).map_err(|error| error.to_string())?;
    let state = state
        .get("state")
        .and_then(serde_json::Value::as_str)
        .ok_or("missing cloud state")?;
    store
        .record_cloud_observation(id, &state_digest(job)?, state, call)
        .map_err(|error| error.to_string())
}

/// Persist the intent and remote identity under the attempt's cloud lock.
pub fn submit<P: provenant_providers::cloud::CloudProvider>(
    provider: &mut P,
    store: &provenant_run::RunStore,
    id: AttemptId,
    job: RemoteJob,
    task: &str,
    call: &provenant_run::Request<'_>,
) -> Result<RemoteJob, String>
where
    P::Error: std::fmt::Debug,
{
    validate_intent(store, id, &job, call)?;
    for text in [
        task,
        &job.environment,
        &job.repository,
        &job.branch,
        &job.requested_model,
    ] {
        provenant_run::Service::scan(text).map_err(|error| error.to_string())?;
    }
    if let Some(effort) = job.requested_effort.as_deref() {
        provenant_run::Service::scan(effort).map_err(|error| error.to_string())?;
    }
    if job.repository.split_once("://").is_some_and(|(_, rest)| {
        rest.split('/')
            .next()
            .is_some_and(|authority| authority.contains('@'))
    }) {
        return Err("cloud repository URL must not contain credentials".into());
    }
    let job = super::cloud_store::AttemptCloudStore::with_attempt(
        store,
        id,
        true,
        call,
        |cloud_store| -> Result<RemoteJob, String> {
            if let Some(mut existing) = cloud_store.load().map_err(|error| error.to_string())? {
                if existing.job_id != job.job_id
                    || existing.commit != job.commit
                    || existing.target != job.target
                    || existing.provider != job.provider
                    || existing.environment != job.environment
                    || existing.repository != job.repository
                    || existing.branch != job.branch
                    || existing.requested_model != job.requested_model
                    || existing.requested_effort != job.requested_effort
                {
                    return Err("cloud job conflicts with the durable attempt".into());
                }
                if existing.state == RemoteState::Submitting {
                    provenant_providers::cloud::reconcile(provider, cloud_store, &mut existing)
                        .map_err(|error| format!("{error:?}"))?;
                }
                note_state(store, id, &existing, call)?;
                return Ok(existing);
            }
            let submitted = provenant_providers::cloud::submit(provider, cloud_store, job, task)
                .map_err(|error| format!("{error:?}"))?;
            note_state(store, id, &submitted, call)?;
            Ok(submitted)
        },
    )
    .map_err(|error| error.to_string())??;
    Ok(job)
}

/// Observe a durable job after a restart and save any validated result.
pub fn reconcile<P: provenant_providers::cloud::CloudProvider>(
    provider: &mut P,
    store: &provenant_run::RunStore,
    id: AttemptId,
    call: &provenant_run::Request<'_>,
) -> Result<RemoteJob, String>
where
    P::Error: std::fmt::Debug,
{
    let job = super::cloud_store::AttemptCloudStore::with_attempt(
        store,
        id,
        false,
        call,
        |cloud_store| -> Result<RemoteJob, String> {
            let mut job = cloud_store
                .load()
                .map_err(|error| error.to_string())?
                .ok_or("missing cloud job")?;
            validate_intent(store, id, &job, call)?;
            provenant_providers::cloud::reconcile(provider, cloud_store, &mut job)
                .map_err(|error| format!("{error:?}"))?;
            note_state(store, id, &job, call)?;
            Ok(job)
        },
    )
    .map_err(|error| error.to_string())??;
    Ok(job)
}

/// Record cancellation as unconfirmed unless the provider can prove it stopped.
pub fn cancel<P: provenant_providers::cloud::CloudProvider>(
    provider: &mut P,
    store: &provenant_run::RunStore,
    id: AttemptId,
    call: &provenant_run::Request<'_>,
) -> Result<RemoteJob, String>
where
    P::Error: std::fmt::Debug,
{
    let job = super::cloud_store::AttemptCloudStore::with_attempt(
        store,
        id,
        false,
        call,
        |cloud_store| -> Result<RemoteJob, String> {
            let mut job = cloud_store
                .load()
                .map_err(|error| error.to_string())?
                .ok_or("missing cloud job")?;
            validate_intent(store, id, &job, call)?;
            provenant_providers::cloud::cancel(provider, cloud_store, &mut job)
                .map_err(|error| format!("{error:?}"))?;
            note_state(store, id, &job, call)?;
            Ok(job)
        },
    )
    .map_err(|error| error.to_string())??;
    Ok(job)
}

pub fn load(
    store: &provenant_run::RunStore,
    id: AttemptId,
    call: &provenant_run::Request<'_>,
) -> Result<Option<RemoteJob>, String> {
    match super::cloud_store::AttemptCloudStore::with_attempt(
        store,
        id,
        false,
        call,
        |cloud_store| cloud_store.load(),
    ) {
        Ok(result) => {
            let job = result.map_err(|error| error.to_string())?;
            if let Some(job) = &job {
                validate_intent(store, id, job, call)?;
            }
            Ok(job)
        }
        Err(provenant_run::RunError::Filesystem(error)) if error.is_not_found() => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

/// Find a cloud attempt's sealed target without inferring it from local state.
pub fn placement(
    store: &provenant_run::RunStore,
    record: &provenant_run::AttemptRecord,
    call: &provenant_run::Request<'_>,
) -> Result<Option<provenant_records::run::CloudPlacement>, String> {
    let request = store
        .request(record.run_id, call)
        .map_err(|error| error.to_string())?;
    Ok(request
        .tasks
        .iter()
        .find(|task| task.task_id == record.task_id)
        .and_then(|task| task.cloud.clone()))
}

/// Resolve a provider's cloud transport settings from the current host configuration.
pub fn cloud_settings(
    roots: &provenant_core::Roots,
    provider: &str,
) -> Result<provenant_config::ProviderCloudSettings, String> {
    Ok(
        provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)
            .map_err(|error| error.to_string())?
            .providers
            .get(provider)
            .map(|provider| provider.cloud.clone())
            .unwrap_or_default(),
    )
}

/// Settle a legacy submission only after the caller proves its owner terminal.
pub(super) fn settle_saved_submission(
    store: &provenant_run::RunStore,
    id: AttemptId,
    call: &provenant_run::Request<'_>,
) -> Result<RemoteJob, String> {
    use provenant_providers::cloud::CloudJobStore;
    super::cloud_store::AttemptCloudStore::with_attempt(
        store,
        id,
        false,
        call,
        |cloud_store| -> Result<RemoteJob, String> {
            let mut current = cloud_store
                .load()
                .map_err(|e| e.to_string())?
                .ok_or("missing cloud job")?;
            validate_intent(store, id, &current, call)?;
            if current.state == RemoteState::Submitting
                && current.remote_id.trim().is_empty()
                && current.submit_error.is_some()
            {
                let detail = current
                    .submit_error
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or("submission unknown")
                    .to_owned();
                // Only a recorded definite refusal is Failed; anything else may
                // still exist remotely, so it stays unresolved and visible.
                current.state = if current.submit_refused {
                    RemoteState::Failed { detail }
                } else {
                    RemoteState::Unknown {
                        detail: format!("submission outcome unknown: {detail}"),
                    }
                };
                cloud_store.save(&current).map_err(|e| e.to_string())?;
            }
            note_state(store, id, &current, call)?;
            Ok(current)
        },
    )
    .map_err(|e| e.to_string())?
}

/// Reconcile or request cancellation with the installed cloud client.
pub(super) fn observe_installed(
    roots: &provenant_core::Roots,
    store: &provenant_run::RunStore,
    id: AttemptId,
    job: &RemoteJob,
    call: &provenant_run::Request<'_>,
    cwd: &std::path::Path,
    cancel_job: bool,
) -> Result<RemoteJob, String> {
    use provenant_providers::cloud_cli::{ClaudeCloud, CodexCloud, InstalledCloudCli};
    // Old submissions saved errors even when their outcome was uncertain. Only
    // repair them after their owner has settled, never while it can still act.
    if job.state == RemoteState::Submitting
        && job.remote_id.trim().is_empty()
        && job.submit_error.is_some()
    {
        let record = store.record(id, call).map_err(|e| e.to_string())?;
        let closed = store
            .run(record.run_id, call)
            .map_err(|e| e.to_string())?
            .closure
            == provenant_records::run::Closure::Closed;
        let graph_terminal = if !closed
            && let Some(graph) = record.context.as_ref().and_then(|c| c.graph_id.as_deref())
        {
            provenant_fabric_runtime::graph::FileJournal::read_checkpoint(
                &roots.state,
                record.project_id,
                graph,
            )
            .map_err(|e| e.to_string())?
            .is_some_and(|checkpoint| {
                !checkpoint.nodes.is_empty()
                    && checkpoint.nodes.iter().all(|node| {
                        !matches!(
                            node.state,
                            provenant_fabric_runtime::graph::NodeState::Pending
                                | provenant_fabric_runtime::graph::NodeState::Running
                        )
                    })
            })
        } else {
            false
        };
        if closed || graph_terminal {
            return settle_saved_submission(store, id, call);
        }
    }
    let executor = provenant_os::exec::SystemExecutor::new(1).map_err(|error| error.to_string())?;
    let cli = InstalledCloudCli {
        executor: &executor,
        cwd: cwd.to_owned(),
        timeout: std::time::Duration::from_secs(120),
    };
    macro_rules! observe {
        ($provider:expr) => {{
            let mut provider = $provider;
            if cancel_job {
                cancel(&mut provider, store, id, call)
            } else {
                reconcile(&mut provider, store, id, call)
            }
        }};
    }
    match provenant_records::AdapterSpec::get(&job.provider).and_then(|spec| spec.cloud_transport) {
        Some(provenant_records::CloudTransport::Claude) => observe!(ClaudeCloud {
            cli,
            pending_cleanup: None
        }),
        Some(provenant_records::CloudTransport::Codex) => {
            observe!(CodexCloud::new(cli, cloud_settings(roots, &job.provider)?))
        }
        _ => Err(format!(
            "cloud target {} has unsupported provider {}",
            job.target, job.provider
        )),
    }
}

struct ViewCli<'a> {
    inner: provenant_providers::cloud_cli::InstalledCloudCli<'a>,
    deadline: std::time::Instant,
}

#[allow(clippy::disallowed_methods)] // Bound real subprocess time across a provider observation.
impl provenant_providers::cloud_cli::CloudCli for ViewCli<'_> {
    fn run(&mut self, program: &str, args: &[&str], stdin: Option<&str>) -> Result<String, String> {
        self.inner.timeout = self
            .deadline
            .checked_duration_since(std::time::Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or("cloud observation timed out")?;
        provenant_providers::cloud_cli::CloudCli::run(&mut self.inner, program, args, stdin)
    }
}

// Read-only status observation: do not save the result or modify the run store.
#[allow(clippy::disallowed_methods)] // This is the real-time provider boundary.
fn observe_for_view(
    roots: &provenant_core::Roots,
    job: &RemoteJob,
    cwd: &std::path::Path,
    budget_end: std::time::Instant,
) -> Result<RemoteJob, String> {
    use provenant_providers::cloud::{CloudProvider, RemoteState};
    use provenant_providers::cloud_cli::{ClaudeCloud, CodexCloud, InstalledCloudCli};
    fn observe<P: CloudProvider<Error = String>>(
        provider: &mut P,
        saved: &RemoteJob,
    ) -> Result<RemoteJob, String> {
        let mut job = saved.clone();
        if matches!(job.state, RemoteState::Submitting) {
            job.remote_id = provider
                .recover(&job)?
                .filter(|id| !id.trim().is_empty())
                .ok_or("cloud submission not yet found")?;
            job.state = RemoteState::Running;
        }
        let mut state = provider.observe(&job)?;
        if matches!(saved.state, RemoteState::CancellationUnconfirmed)
            && matches!(state, RemoteState::Running | RemoteState::Unknown { .. })
        {
            state = RemoteState::CancellationUnconfirmed;
        }
        if let RemoteState::Finished { result } = &state {
            result
                .clone()
                .check(&job.job_id, &job.nonce, &job.commit, &job.requested_model)
                .map_err(|error| error.to_string())?;
        }
        job.state = state;
        Ok(job)
    }
    let executor = provenant_os::exec::SystemExecutor::new(1).map_err(|error| error.to_string())?;
    let timeout = std::time::Duration::from_secs(3);
    let cli = ViewCli {
        inner: InstalledCloudCli {
            executor: &executor,
            cwd: cwd.to_owned(),
            timeout,
        },
        deadline: budget_end.min(std::time::Instant::now() + timeout),
    };
    match provenant_records::AdapterSpec::get(&job.provider).and_then(|spec| spec.cloud_transport) {
        Some(provenant_records::CloudTransport::Claude) => observe(
            &mut ClaudeCloud {
                cli,
                pending_cleanup: None,
            },
            job,
        ),
        Some(provenant_records::CloudTransport::Codex) => observe(
            &mut CodexCloud::new(cli, cloud_settings(roots, &job.provider)?),
            job,
        ),
        _ => Err(format!("unsupported cloud provider {}", job.provider)),
    }
}

/// User-facing advisory result and model comparison for a durable job.
pub fn view(job: &RemoteJob) -> serde_json::Value {
    let mut value = serde_json::json!({
        "target": job.target, "environment": job.environment, "provider": job.provider,
        "remote_id": job.remote_id, "commit": job.commit,
        "requested_model": job.requested_model, "requested_effort": job.requested_effort, "state": job.state,
    });
    if let Some(error) = &job.submit_error {
        value["submit_error"] = serde_json::json!(error);
    }
    if let Some(diagnostic) = job.environment_diagnostic() {
        value["remedy"] = serde_json::json!(diagnostic.remedy);
    }
    if let RemoteState::Finished { result } = &job.state
        && let Ok(checked) =
            result
                .clone()
                .check(&job.job_id, &job.nonce, &job.commit, &job.requested_model)
    {
        value["passed"] = serde_json::json!(checked.passed);
        value["observed_model_status"] = serde_json::json!(match checked.model {
            provenant_providers::cloud::ModelObservation::Matches => "matches",
            provenant_providers::cloud::ModelObservation::Differs => "differs",
            provenant_providers::cloud::ModelObservation::Unknown => "unknown",
        });
    }
    // Cloud output is untrusted; redact known credential shapes before any view.
    serde_json::to_vec(&value).ok()
        .and_then(|bytes| provenant_confinement::redact::known_shapes(&bytes).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_else(|| serde_json::json!({"state": {"state": "unknown"}, "error": "cloud output could not be redacted"}))
}

/// Decorate read-only lane projections with their saved cloud observation.
// One status or lanes view spends a fixed total on provider calls, however many
// jobs are unfinished. A job past the budget shows its saved state, marked stale.
#[allow(clippy::disallowed_methods)] // This is the real-time provider boundary.
pub(super) fn view_observation_budget() -> std::time::Instant {
    std::time::Instant::now() + std::time::Duration::from_secs(5)
}

pub(super) fn hydrate(
    roots: &provenant_core::Roots,
    store: &provenant_run::RunStore,
    lanes: &mut [provenant_run::AttemptRecord],
    call: &provenant_run::Request<'_>,
) -> Result<(), String> {
    hydrate_within(roots, store, lanes, call, view_observation_budget())
}

/// Hydrate under a budget the caller shares across a view that hydrates in parts.
pub(super) fn hydrate_within(
    roots: &provenant_core::Roots,
    store: &provenant_run::RunStore,
    lanes: &mut [provenant_run::AttemptRecord],
    call: &provenant_run::Request<'_>,
    budget_end: std::time::Instant,
) -> Result<(), String> {
    use provenant_records::run::CloudObservation as State;
    for lane in lanes {
        let Some(placement) = placement(store, lane, call)? else {
            continue;
        };
        let mut job = load(store, lane.attempt_id, call)?;
        let mut stale = false;
        if let Some(saved) = &job
            && !saved.state.is_terminal()
        {
            // A spent budget starts no more providers. A timed-out call costs its
            // termination grace on top, so starting each remaining job would still
            // grow the view with the job count; the overrun stays one call's grace.
            #[allow(clippy::disallowed_methods)] // This is the real-time provider boundary.
            if std::time::Instant::now() >= budget_end {
                stale = true;
            } else {
                match std::env::current_dir()
                    .map_err(|error| error.to_string())
                    .and_then(|cwd| observe_for_view(roots, saved, &cwd, budget_end))
                {
                    Ok(observed) => job = Some(observed),
                    Err(_) => stale = true,
                }
            }
        }
        let mut cloud_view = job.as_ref().map(view).unwrap_or_else(
            || serde_json::json!({"target": placement.target, "state": {"state": "submitting"}}),
        );
        if stale {
            cloud_view["stale"] = serde_json::json!(true);
        }
        cloud_view["attempt_id"] = serde_json::json!(lane.attempt_id);
        lane.cloud_view.0 = Some(cloud_view);
        let observation = match job {
            None => State::Submitting,
            Some(job) => match &job.state {
                RemoteState::Submitting => State::Submitting,
                RemoteState::Running => State::Running,
                RemoteState::Unknown { .. } => State::Unknown,
                RemoteState::SetupFailed { .. } => State::SetupFailed,
                RemoteState::Failed { .. } => State::Failed,
                RemoteState::Cancelled => State::Cancelled,
                RemoteState::CancellationUnconfirmed => State::CancellationUnconfirmed,
                RemoteState::Finished { result } => {
                    let checked = result
                        .clone()
                        .check(&job.job_id, &job.nonce, &job.commit, &job.requested_model)
                        .map_err(|error| error.to_string())?;
                    if checked.passed {
                        State::Passed
                    } else {
                        State::Failed
                    }
                }
            },
        };
        lane.cloud_observation.0 = Some(observation);
    }
    Ok(())
}

/// A finished cloud-only run needs no further observation by a default sweep.
pub(super) fn finished_run(
    store: &provenant_run::RunStore,
    run: provenant_core::RunId,
    call: &provenant_run::Request<'_>,
) -> Result<bool, String> {
    let request = store
        .request(run, call)
        .map_err(|error| error.to_string())?;
    if request.tasks.iter().any(|task| task.cloud.is_none()) {
        return Ok(false);
    }
    let attempts = store
        .pending_attempts(run, call)
        .map_err(|error| error.to_string())?;
    if attempts.is_empty() {
        return Ok(false);
    }
    for attempt in attempts {
        if placement(store, &attempt, call)?.is_none() {
            return Ok(false);
        }
        let Some(job) = load(store, attempt.attempt_id, call)? else {
            return Ok(false);
        };
        if !job.state.is_terminal()
            || attempt
                .extra
                .get("cloud_observation_digest")
                .and_then(serde_json::Value::as_str)
                != Some(state_digest(&job)?.as_str())
        {
            return Ok(false);
        }
    }
    Ok(true)
}
