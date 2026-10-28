//! Bounded host-local composition of indexed run stores.
use super::Error;
use provenant_core::{ActorId, CancelToken, Clock, ProjectId, RootInputs, Roots, WaitBudget};
use provenant_os::clock::SystemClock;
use provenant_run::{ExecutionContext, RunStore};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[allow(clippy::disallowed_methods)] // The request owns a bounded real-time budget.
pub(super) fn read(
    roots: &Roots,
    selected: Option<ProjectId>,
    max_depth: u64,
    max_nodes: u64,
) -> Result<Value, Error> {
    if !(1..=16).contains(&max_depth) || !(1..=100).contains(&max_nodes) {
        return Err(provenant_run::RunError::Validation("tree bounds").into());
    }
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = provenant_run::Request {
        clock: &clock,
        budget: WaitBudget::cli(clock.mono_now(), 5).map_err(|_| Error::Encode)?,
        cancel: &cancel,
    };
    let host = match provenant_config::lookup_host(roots) {
        Err(provenant_config::ConfigError::ProjectBindingFilesystem(error))
            if error.is_not_found() =>
        {
            None
        }
        found => found?,
    };
    let mut stores = VecDeque::from([(roots.state.clone(), 0u64, None)]);
    let mut nodes = Vec::new();
    let mut seen = BTreeMap::new();
    let mut duplicate_count = 0u64;
    let mut conflicts = Vec::new();
    let mut examined = 0u64;
    let mut unfetched = Vec::new();
    let mut root_coverage = "empty";
    let mut truncated = false;
    let mut totals: BTreeMap<&str, (u64, u64)> = [
        "uncached_input",
        "cache_read",
        "cache_write",
        "output_excluding_reasoning",
        "reasoning",
    ]
    .into_iter()
    .map(|key| (key, (0, 0)))
    .collect();
    while let Some((state, depth, owner)) = stores.pop_front() {
        let context = ExecutionContext {
            state_root: state.clone(),
            project_id: selected.unwrap_or(ProjectId::from_bytes([0; 16])),
            actor_id: ActorId::from_bytes([0; 16]),
            read_prompts: false,
        };
        let store = match RunStore::open_read_only(&context, &call) {
            Ok(Some(store)) => store,
            Ok(None) => {
                if owner.is_some() {
                    unfetched.push(json!({"parent_attempt":owner,"reason":"missing_store"}));
                }
                continue;
            }
            Err(error) if owner.is_some() => {
                unfetched.push(json!({"parent_attempt":owner,"reason":"unavailable","detail":error.to_string()}));
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        if depth == 0 {
            root_coverage = "complete";
        }
        let mut after = 0;
        let mut through = None;
        loop {
            let remaining = max_nodes.saturating_sub(examined);
            if remaining == 0 {
                truncated = true;
                unfetched.push(json!({"parent_attempt":owner,"reason":"node_limit"}));
                break;
            }
            let page = match store.board_page(
                selected,
                true,
                remaining.min(100),
                after,
                through,
                &call,
            ) {
                Ok(page) => page,
                Err(error) if owner.is_some() => {
                    unfetched.push(json!({"parent_attempt":owner,"reason":"unavailable","detail":error.to_string()}));
                    break;
                }
                Err(error) => return Err(error.into()),
            };
            through = Some(page.high_water);
            for lane in page.lanes {
                examined += 1;
                let attempt = &lane.attempt;
                let key = (attempt.project_id, attempt.attempt_id);
                let identity = (attempt.run_id, attempt.task_id);
                if let Some(existing) = seen.get(&key) {
                    duplicate_count += 1;
                    if *existing != identity {
                        conflicts.push(json!({"project_id":attempt.project_id,"attempt_id":attempt.attempt_id,"reason":"conflicting_identity"}));
                    }
                    continue;
                }
                seen.insert(key, identity);
                let usage = serde_json::to_value(&attempt.usage).map_err(|_| Error::Encode)?;
                for (name, (sum, missing)) in &mut totals {
                    if let Some(value) = usage[*name].as_u64() {
                        *sum = sum.checked_add(value).ok_or(Error::Encode)?;
                    } else {
                        *missing += 1;
                    }
                }
                let child_path = state
                    .as_path()
                    .join("children")
                    .join(attempt.attempt_id.to_string());
                let mut inputs = RootInputs::new("/".into(), "/__provenant_tree_excluded__".into());
                inputs.state_dir = Some(child_path.clone());
                if depth + 1 >= max_depth {
                    truncated = true;
                    unfetched
                        .push(json!({"parent_attempt":attempt.attempt_id,"reason":"depth_limit"}));
                } else {
                    match inputs.resolve(&provenant_os::fs::SystemPathResolver) {
                        Ok(child) if child.state.as_path() == child_path => {
                            stores.push_back((child.state, depth + 1, Some(attempt.attempt_id)))
                        }
                        _ => unfetched.push(
                            json!({"parent_attempt":attempt.attempt_id,"reason":"unavailable"}),
                        ),
                    }
                }
                nodes.push(json!({"lane_id":{"host":host,"project_id":attempt.project_id,"run_id":attempt.run_id,"task_id":attempt.task_id,"attempt_id":attempt.attempt_id},"depth":depth,"store_parent_attempt":owner,"metadata":lane}));
            }
            match page.next {
                Some(next) => after = next,
                None => break,
            }
        }
    }
    let ids: BTreeSet<_> = nodes
        .iter()
        .filter_map(|n| {
            Some((
                n["lane_id"]["project_id"].as_str()?,
                n["lane_id"]["attempt_id"].as_str()?,
            ))
        })
        .collect();
    let mut edges = Vec::new();
    for node in &nodes {
        let run = &node["metadata"]["run"];
        for (field, kind) in [("parent_attempt", "dispatch"), ("parent", "continuation")] {
            if let Some(parent) = run[field].as_str() {
                edges.push(json!({"kind":kind,"parent_attempt":parent,"child_attempt":node["lane_id"]["attempt_id"],"parent_status":if ids.contains(&(node["lane_id"]["project_id"].as_str().unwrap_or(""), parent)) { "present" } else { "missing" }}));
            }
        }
        if let Some(parent) = node["metadata"]["attempt"]["retry_of"].as_str() {
            edges.push(json!({"kind":"retry","parent_attempt":parent,"child_attempt":node["lane_id"]["attempt_id"],"parent_status":if ids.contains(&(node["lane_id"]["project_id"].as_str().unwrap_or(""), parent)) { "present" } else { "missing" }}));
        }
    }
    let totals: BTreeMap<_, _> = totals
        .into_iter()
        .map(|(key, (known, missing))| (key, json!({"known_sum":known,"missing":missing})))
        .collect();
    Ok(
        json!({"schema_version":1,"nodes":nodes,"edges":edges,"totals":{"attempts":seen.len(),"duplicate_observations":duplicate_count,"usage":totals},"identity_conflicts":conflicts,"truncated":truncated,"unfetched_children":unfetched,"coverage":{"scope":"local","host":host,"runs":root_coverage,"nested_runs":if unfetched.is_empty() && conflicts.is_empty() { "complete" } else { "partial" }},"limits":{"depth":max_depth,"nodes":max_nodes}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::{AttemptId, Digest, OperationId, RunId, TaskId};
    use provenant_records::CanonicalV1;
    use provenant_run::{CanonicalRequest, NewAttempt, PreparedRun};

    #[test]
    fn nested_tree_once() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let base = temp.path().canonicalize()?;
        std::fs::create_dir(base.join("home"))?;
        std::fs::create_dir(base.join("checkout"))?;
        let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(base.join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        assert_eq!(read(&roots, None, 3, 100)?["nodes"], json!([]));
        assert!(!roots.state.as_path().exists());
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = provenant_run::Request {
            clock: &clock,
            budget: WaitBudget::cli(clock.mono_now(), 30)?,
            cancel: &cancel,
        };
        let project = ProjectId::from_bytes([1; 16]);
        let context = ExecutionContext {
            state_root: roots.state.clone(),
            project_id: project,
            actor_id: ActorId::from_bytes([2; 16]),
            read_prompts: false,
        };
        let store = RunStore::open(&context, &call)?;
        let task = TaskId::from_bytes([3; 16]);
        let mut request: CanonicalRequest = serde_json::from_value(
            json!({"schema_version":1,"tasks":[{"task_id":task,"prompt_digest":CanonicalV1::file_digest(b"body"),"route":{"kind":"model","provider":"fixture","model":"fixture/model","effort":"high"},"fallback":[],"authority_digest":Digest::from_bytes([5;32]),"provider_timeout_seconds":30,"delivery_id":null}],"concurrency":1,"parent":null,"lane_depth":1}),
        )?;
        let prepared =
            |n: u8, request: CanonicalRequest| -> Result<PreparedRun, Box<dyn std::error::Error>> {
                Ok(PreparedRun {
                    run_id: RunId::from_bytes([n; 16]),
                    operation_id: OperationId::parse(&format!("tree-{n}"))?,
                    created_at: clock.wall_now()?,
                    request,
                    attempts: vec![NewAttempt {
                        task_id: task,
                        attempt_id: AttemptId::from_bytes([n; 16]),
                        prompt: "body".into(),
                    }],
                })
            };
        store.create(&prepared(1, request.clone())?, &call)?;
        let child_path = roots
            .state
            .as_path()
            .join("children")
            .join(AttemptId::from_bytes([1; 16]).to_string());
        inputs.state_dir = Some(child_path);
        let child = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let child_store = RunStore::open(
            &ExecutionContext {
                state_root: child.state,
                ..context
            },
            &call,
        )?;
        child_store.create(&prepared(1, request.clone())?, &call)?;
        request.parent_attempt = Some(AttemptId::from_bytes([1; 16]));
        request.lane_depth = Some(2);
        child_store.create(&prepared(2, request.clone())?, &call)?;
        request.parent = Some(AttemptId::from_bytes([2; 16]));
        request.parent_attempt = Some(AttemptId::from_bytes([9; 16]));
        child_store.create(&prepared(3, request)?, &call)?;
        let tree = read(&roots, None, 3, 100)?;
        assert_eq!(tree["totals"]["attempts"], 3);
        assert_eq!(tree["totals"]["duplicate_observations"], 1);
        assert_eq!(tree["totals"]["usage"]["reasoning"]["missing"], 3);
        assert!(
            tree["edges"]
                .as_array()
                .ok_or("edges")?
                .iter()
                .any(|e| e["kind"] == "dispatch" && e["parent_status"] == "present")
        );
        assert!(
            tree["edges"]
                .as_array()
                .ok_or("edges")?
                .iter()
                .any(|e| e["kind"] == "dispatch" && e["parent_status"] == "missing")
        );
        assert!(
            tree["edges"]
                .as_array()
                .ok_or("edges")?
                .iter()
                .any(|e| e["kind"] == "continuation" && e["parent_status"] == "present")
        );
        let limited = read(&roots, None, 1, 100)?;
        assert_eq!(limited["truncated"], true);
        assert_eq!(limited["unfetched_children"][0]["reason"], "depth_limit");
        let limited = read(&roots, None, 3, 1)?;
        assert!(limited["truncated"].as_bool().unwrap_or(false));
        assert!(
            limited["unfetched_children"]
                .as_array()
                .ok_or("unfetched")?
                .iter()
                .any(|entry| entry["reason"] == "node_limit")
        );
        assert!(
            tree["unfetched_children"]
                .as_array()
                .ok_or("unfetched")?
                .iter()
                .any(|entry| entry["reason"] == "missing_store")
        );
        Ok(())
    }
}
