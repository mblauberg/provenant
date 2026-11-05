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

/// Submit using the installed provider CLI. Provider details stay at this edge.
pub(super) fn submit_installed(
    store: &provenant_run::RunStore,
    id: AttemptId,
    job: RemoteJob,
    task: &str,
    call: &provenant_run::Request<'_>,
    cwd: &std::path::Path,
) -> Result<RemoteJob, String> {
    use provenant_providers::cloud_cli::{ClaudeCloud, CodexCloud, InstalledCloudCli};
    let executor = provenant_os::exec::SystemExecutor::new(1).map_err(|error| error.to_string())?;
    let cli = InstalledCloudCli {
        executor: &executor,
        cwd: cwd.to_owned(),
        timeout: std::time::Duration::from_secs(120),
    };
    match job.provider.as_str() {
        "claude" => submit(
            &mut ClaudeCloud {
                cli,
                pending_cleanup: None,
            },
            store,
            id,
            job,
            task,
            call,
        ),
        "codex" => submit(&mut CodexCloud { cli }, store, id, job, task, call),
        provider => Err(format!(
            "cloud target {} has unsupported provider {provider}",
            job.target
        )),
    }
}

/// Read a cloud record, if submission has written one.
pub(super) fn load(
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
pub(super) fn placement(
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

/// Reconcile or request cancellation with the installed cloud client.
pub(super) fn observe_installed(
    store: &provenant_run::RunStore,
    id: AttemptId,
    job: &RemoteJob,
    call: &provenant_run::Request<'_>,
    cwd: &std::path::Path,
    cancel_job: bool,
) -> Result<RemoteJob, String> {
    use provenant_providers::cloud_cli::{ClaudeCloud, CodexCloud, InstalledCloudCli};
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
    match job.provider.as_str() {
        "claude" => observe!(ClaudeCloud {
            cli,
            pending_cleanup: None
        }),
        "codex" => observe!(CodexCloud { cli }),
        provider => Err(format!(
            "cloud target {} has unsupported provider {provider}",
            job.target
        )),
    }
}

/// User-facing advisory result and model comparison for a durable job.
pub(super) fn view(job: &RemoteJob) -> serde_json::Value {
    let mut value = serde_json::json!({
        "target": job.target, "environment": job.environment, "provider": job.provider,
        "remote_id": job.remote_id, "commit": job.commit,
        "requested_model": job.requested_model, "requested_effort": job.requested_effort, "state": job.state,
    });
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
pub(super) fn hydrate(
    store: &provenant_run::RunStore,
    lanes: &mut [provenant_run::AttemptRecord],
    call: &provenant_run::Request<'_>,
) -> Result<(), String> {
    use provenant_records::run::CloudObservation as State;
    for lane in lanes {
        if placement(store, lane, call)?.is_none() {
            continue;
        }
        let observation = match load(store, lane.attempt_id, call)? {
            None => State::Submitting,
            Some(job) => match &job.state {
                RemoteState::Submitting => State::Submitting,
                RemoteState::Running => State::Running,
                RemoteState::Unknown { .. } => State::Unknown,
                RemoteState::SetupFailed { .. } => State::SetupFailed,
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
