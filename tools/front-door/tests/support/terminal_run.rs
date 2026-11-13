//! Settled run records for telemetry tests, without a provider or obsolete CLI.
use crate::support::{Result, TestRig};
use provenant_core::{
    ActorId, AttemptId, CancelToken, Clock, Digest, OperationId, RootInputs, RunId, TaskId,
};
use provenant_os::clock::SystemClock;
use provenant_records::{CanonicalV1, WorkKey};
use provenant_run::{
    CanonicalRequest, ExecutionContext, NewAttempt, PreparedRun, Request, RunStore,
};
use serde_json::{Value, json};

pub fn seed(rig: &TestRig, operation: &str, contexts: &[Value]) -> Result<Vec<WorkKey>> {
    provenant_os::fixture::private_state_dir(&rig.state)?;
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
    let mut attempt_ids = Vec::new();
    let mut key_counts = std::collections::BTreeMap::<(String, String), u32>::new();
    for (index, work) in contexts.iter().enumerate() {
        let task_id = TaskId::from_bytes(provenant_os::fs::entropy_128()?);
        let attempt_id = AttemptId::from_bytes(provenant_os::fs::entropy_128()?);
        let mut work_context = work.as_object().cloned().unwrap_or_default();
        let graph = match work_context.get("graph_id") {
            None => fixture_graph(operation),
            Some(value) => value
                .as_str()
                .ok_or("fixture graph_id must be a string")?
                .to_owned(),
        };
        let node = match work_context.get("node_id") {
            None => format!("node-{}", index + 1),
            Some(value) => value
                .as_str()
                .ok_or("fixture node_id must be a string")?
                .to_owned(),
        };
        let number = key_counts.entry((graph.clone(), node.clone())).or_default();
        *number += 1;
        WorkKey::new(project_id, graph.clone(), node.clone(), *number)?;
        work_context.insert("graph_id".into(), Value::String(graph));
        work_context.insert("node_id".into(), Value::String(node));
        work_context.insert("attempt".into(), Value::from(*number));
        tasks.push(json!({"task_id":task_id,"prompt_digest":CanonicalV1::file_digest(prompt.as_bytes()),
            "context":work_context,"route":{"kind":"model","provider":"codex","model":"gpt-6-luna","effort":null},
            "authority_digest":Digest::from_bytes([5;32]),"provider_timeout_seconds":30,"delivery_id":null}));
        attempts.push(NewAttempt {
            task_id,
            attempt_id,
            prompt: prompt.into(),
        });
        attempt_ids.push(attempt_id);
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
    attempt_ids
        .into_iter()
        .map(|id| store.work_key(id, &call))
        .collect::<provenant_run::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn fixture_graph(operation: &str) -> String {
    let safe = operation
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
                byte as char
            } else {
                '-'
            }
        })
        .take(48 - "fixture-".len())
        .collect::<String>();
    if safe.is_empty() {
        "fixture-graph".into()
    } else {
        format!("fixture-{safe}")
    }
}
