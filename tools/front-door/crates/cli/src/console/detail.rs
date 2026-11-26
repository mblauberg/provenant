use super::Error;
use provenant_core::{ActorId, AttemptId, CancelToken, ProjectId, Roots};
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
            json!({"schema_version":1,"lane_id":{"host":host,"attempt_id":attempt_id},"status":"unavailable","reason":"remote_detail_requires_host_read"}),
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
    let mut state_path = roots.state.as_path().to_path_buf();
    let mut nested_coverage = "not_searched";
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
                    json!({"schema_version":1,"lane_id":{"host":local_host,"attempt_id":attempt_id},"status":"unavailable","reason":error.to_string()}),
                );
            }
        }
    }
    if found.is_none() {
        nested_coverage = "bounded_tree_search";
        let tree = match super::tree::read_inner(roots, None, 16, 100, true, true) {
            Ok(tree) => tree,
            Err(error) => {
                return Ok(
                    json!({"schema_version":1,"lane_id":{"host":local_host,"attempt_id":attempt_id},"status":"unavailable","reason":error.to_string(),"coverage":{"nested":"unavailable","peer":"not_searched"}}),
                );
            }
        };
        found = tree["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .find_map(|node| {
                (node["lane_id"]["attempt_id"] == json!(attempt_id))
                    .then(|| {
                        let path = node["store_path"].as_str()?;
                        let project_id: ProjectId =
                            node["lane_id"]["project_id"].as_str()?.parse().ok()?;
                        let child = roots
                            .with_state_dir(path.into(), &provenant_os::fs::SystemPathResolver)
                            .ok()?;
                        let context = ExecutionContext {
                            state_root: child.state,
                            project_id,
                            actor_id: ActorId::from_bytes([0; 16]),
                            read_prompts: false,
                        };
                        let store = RunStore::open_read_only(&context, &call).ok()??;
                        let record = store.record(attempt_id, &call).ok()?;
                        state_path = path.into();
                        Some(record)
                    })
                    .flatten()
            });
    }
    let Some(record) = found else {
        return Ok(super::projection::detail_missing(
            &json!({"host": local_host, "attempt_id": attempt_id}),
            "bounded_tree_search",
        ));
    };
    let key = &record.key;
    let effective = record.effective_usage();
    let usage = serde_json::to_value(&effective).map_err(|_| Error::Encode)?;
    let tokens = &effective.tokens;
    let mut known = Vec::new();
    let mut missing = Vec::new();
    for (name, count) in [
        ("input", tokens.input),
        ("cache_read", tokens.cache_read),
        ("cache_write", tokens.cache_write),
        ("output", tokens.output),
        ("reasoning", tokens.reasoning),
    ] {
        if count.is_some() {
            known.push(name)
        } else {
            missing.push(name)
        }
    }
    let artifact = |name: &str, item: Option<&provenant_run::ArtifactRecord>| {
        item.map(|item| {
            let present = item.path.to_relative_path().ok().is_some_and(|path| {
                std::fs::symlink_metadata(state_path.join(path.as_path()))
                    .is_ok_and(|metadata| metadata.is_file())
            });
            json!({"name":name,"bytes":item.bytes,"digest":item.digest,"complete":item.complete,"redacted":item.redacted,"present":present})
        })
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
    let (linked_inbox_requests, inbox_coverage) = super::detail_inbox::read(roots, &record);
    let timing = json!({"queued_at":record.queued_at,"started_at":record.started_at,"provider_ms":record.provider_ms,"finished_at":match &record.state { provenant_run::AttemptState::Terminal { finished_at, .. } => Some(finished_at), _ => None }});
    Ok(super::projection::detail(
        &json!({"host":local_host,"project_id":record.project_id,"key":key,"attempt_id":record.attempt_id}),
        "found",
        &as_value(&record.requested_route)?,
        &as_value(&record.resolved_route)?,
        &as_value(&record.applied_route)?,
        &as_value(&record.observed_route)?,
        &usage,
        &known,
        &missing,
        &as_value(&record.trace)?,
        &timing,
        &artifacts,
        &as_value(&record.delivery)?,
        &linked_inbox_requests,
        nested_coverage,
        inbox_coverage,
    ))
}

fn as_value<T: serde::Serialize>(value: &T) -> Result<Value, Error> {
    serde_json::to_value(value).map_err(|_| Error::Encode)
}
