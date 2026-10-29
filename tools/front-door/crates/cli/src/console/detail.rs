use super::Error;
use provenant_core::{ActorId, AttemptId, CancelToken, Roots};
use provenant_os::clock::SystemClock;
use provenant_run::{ExecutionContext, RunStore};
use serde_json::{Value, json};

pub(super) fn read(roots: &Roots, id: &str) -> Result<Value, Error> {
    let (host, raw_id) = id
        .split_once('/')
        .map_or((None, id), |(host, id)| (Some(host), id));
    let attempt_id = AttemptId::parse(raw_id).map_err(|_| Error::InvalidSession)?;
    let local_host = match provenant_config::lookup_host(roots) {
        Ok(host) => host,
        Err(provenant_config::ConfigError::ProjectBindingFilesystem(error))
            if error.is_not_found() =>
        {
            None
        }
        Err(error) => return Err(error.into()),
    };
    if host.is_some_and(|host| Some(host) != local_host.map(|h| h.to_string()).as_deref()) {
        return Ok(
            json!({"lane_id":{"host":host,"attempt_id":attempt_id},"status":"unavailable","reason":"remote_detail_requires_host_read"}),
        );
    }
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = provenant_run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let projects = provenant_config::registered_projects(roots)?;
    let mut found = None;
    for project in projects {
        let context = ExecutionContext {
            state_root: roots.state.clone(),
            project_id: project.project_id(),
            actor_id: ActorId::from_bytes([0; 16]),
            read_prompts: false,
        };
        let Some(store) = RunStore::open_read_only(&context, &call)? else {
            continue;
        };
        match store.record(attempt_id, &call) {
            Ok(record) => {
                found = Some(record);
                break;
            }
            Err(provenant_run::RunError::NotFound) => continue,
            Err(error) => {
                return Ok(
                    json!({"lane_id":{"host":local_host,"attempt_id":attempt_id},"status":"unavailable","reason":error.to_string()}),
                );
            }
        }
    }
    if found.is_none() {
        let tree = match super::tree::read(roots, None, 16, 100, true) {
            Ok(tree) => tree,
            Err(error) => {
                return Ok(
                    json!({"lane_id":{"host":local_host,"attempt_id":attempt_id},"status":"unavailable","reason":error.to_string(),"coverage":{"nested":"unavailable","peer":"unknown"}}),
                );
            }
        };
        found = tree["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .find_map(|node| {
                (node["lane_id"]["attempt_id"] == json!(attempt_id))
                    .then(|| serde_json::from_value(node["metadata"]["attempt"].clone()).ok())
                    .flatten()
            });
    }
    let Some(record) = found else {
        return Ok(
            json!({"lane_id":{"host":local_host,"attempt_id":attempt_id},"status":"missing","coverage":{"nested":"bounded_tree_search","peer":"unavailable"}}),
        );
    };
    let usage = serde_json::to_value(&record.usage).map_err(|_| Error::Encode)?;
    let names = [
        "uncached_input",
        "cache_read",
        "cache_write",
        "output_excluding_reasoning",
        "reasoning",
    ];
    let known: Vec<_> = names
        .iter()
        .filter(|name| !usage[**name].is_null())
        .collect();
    let missing: Vec<_> = names
        .iter()
        .filter(|name| usage[**name].is_null())
        .collect();
    let artifact = |name: &str, item: Option<&provenant_run::ArtifactRecord>| {
        item.map(|item| json!({"name":name,"bytes":item.bytes,"digest":item.digest,"complete":item.complete,"redacted":item.redacted}))
    };
    let mut artifacts: Vec<Value> = artifact("prompt", Some(&record.prompt))
        .into_iter()
        .collect();
    artifacts.extend(
        [
            artifact("result", record.result.as_ref()),
            artifact("exit_evidence", record.exit_evidence.as_ref()),
        ]
        .into_iter()
        .flatten(),
    );
    let timing = json!({"queued_at":record.queued_at,"started_at":record.started_at,"provider_ms":record.provider_ms,"finished_at":match &record.state { provenant_run::AttemptState::Terminal { finished_at, .. } => Some(finished_at), _ => None }});
    Ok(
        json!({"schema_version":1,"lane_id":{"host":local_host,"project_id":record.project_id,"run_id":record.run_id,"task_id":record.task_id,"attempt_id":record.attempt_id},"status":"found","requested_route":record.requested_route,"resolved_route":record.resolved_route,"applied_route":record.applied_route,"observed_route":record.observed_route,"usage":usage,"usage_coverage":{"known":known,"missing":missing},"trace":record.trace,"timings":timing,"artifacts":artifacts,"delivery":record.delivery,"linked_inbox_requests":[],"coverage":{"nested":"unknown","peer":"unknown","linked_inbox_requests":"unavailable"}}),
    )
}
