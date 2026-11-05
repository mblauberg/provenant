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
