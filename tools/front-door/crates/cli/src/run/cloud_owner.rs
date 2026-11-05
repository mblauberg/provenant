//! Preparation of cloud verification identity at the run-owner boundary.
use provenant_confinement::run_config::CloudTarget;
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
    if job.job_id != id.to_string() {
        return Err("cloud job identity does not match the attempt".into());
    }
    provenant_run::Service::scan(task).map_err(|error| error.to_string())?;
    super::cloud_store::AttemptCloudStore::with_attempt(store, id, true, call, |cloud_store| {
        provenant_providers::cloud::submit(provider, cloud_store, job, task)
    })
    .map_err(|error| error.to_string())?
    .map_err(|error| format!("{error:?}"))
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
    super::cloud_store::AttemptCloudStore::with_attempt(store, id, false, call, |cloud_store| {
        let mut job = cloud_store
            .load()
            .map_err(|error| error.to_string())?
            .ok_or("missing cloud job")?;
        provenant_providers::cloud::reconcile(provider, cloud_store, &mut job)
            .map_err(|error| format!("{error:?}"))?;
        Ok(job)
    })
    .map_err(|error| error.to_string())?
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
    super::cloud_store::AttemptCloudStore::with_attempt(store, id, false, call, |cloud_store| {
        let mut job = cloud_store
            .load()
            .map_err(|error| error.to_string())?
            .ok_or("missing cloud job")?;
        provenant_providers::cloud::cancel(provider, cloud_store, &mut job)
            .map_err(|error| format!("{error:?}"))?;
        Ok(job)
    })
    .map_err(|error| error.to_string())?
}
