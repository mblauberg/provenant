//! Settled run records for telemetry tests, without a provider or obsolete CLI.
use crate::support::{Result, TestRig};
use provenant_core::{
    ActorId, AttemptId, CancelToken, Clock, Digest, OperationId, RootInputs, RunId, TaskId,
};
use provenant_os::clock::SystemClock;
use provenant_records::CanonicalV1;
use provenant_run::{
    CanonicalRequest, ExecutionContext, NewAttempt, PreparedRun, Request, RunStore,
};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;

pub fn seed(rig: &TestRig, operation: &str, contexts: &[Value]) -> Result<RunId> {
    std::fs::create_dir_all(&rig.state)?;
    std::fs::set_permissions(&rig.state, std::fs::Permissions::from_mode(0o700))?;
    let mut inputs = RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let project_id = provenant_config::bind_project(
        &roots,
        provenant_config::ProjectLocation::new(std::fs::canonicalize(rig.project.join(".git"))?)?,
    )?
    .project_id();
    let context = ExecutionContext {
        state_root: roots.state,
        project_id,
        actor_id: ActorId::from_bytes([1; 16]),
        read_prompts: false,
    };
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let store = RunStore::open(&context, &call)?;
    let run_id = RunId::from_bytes(provenant_os::fs::entropy_128()?);
    let prompt = "private fixture prompt";
    let mut tasks = Vec::new();
    let mut attempts = Vec::new();
    for work in contexts {
        let task_id = TaskId::from_bytes(provenant_os::fs::entropy_128()?);
        tasks.push(json!({"task_id":task_id,"prompt_digest":CanonicalV1::file_digest(prompt.as_bytes()),
            "context":work,"route":{"kind":"model","provider":"codex","model":"gpt-6-luna","effort":null},
            "fallback":[],"authority_digest":Digest::from_bytes([5;32]),"provider_timeout_seconds":30,"delivery_id":null}));
        attempts.push(NewAttempt {
            task_id,
            attempt_id: AttemptId::from_bytes(provenant_os::fs::entropy_128()?),
            prompt: prompt.into(),
        });
    }
    let request: CanonicalRequest = serde_json::from_value(
        json!({"schema_version":1,"tasks":tasks,"concurrency":1,"parent":null}),
    )?;
    store.create(
        &PreparedRun {
            run_id,
            operation_id: OperationId::parse(operation)?,
            request,
            attempts,
            created_at: clock.wall_now()?,
        },
        &call,
    )?;
    store.refuse_queued(run_id, "fixture provider unavailable", &call)?;
    Ok(run_id)
}
