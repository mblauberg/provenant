//! Bounded host-local composition of indexed run stores.
use super::Error;
use provenant_core::{
    ActorId, AttemptId, CancelToken, Clock, ProjectId, RootInputs, Roots, WaitBudget,
};
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
    detail: bool,
) -> Result<Value, Error> {
    if !(1..=16).contains(&max_depth) || !(1..=100).contains(&max_nodes) {
        return Err(provenant_run::RunError::Validation("tree bounds").into());
    }
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let host = match provenant_config::lookup_host(roots) {
        Err(provenant_config::ConfigError::ProjectBindingFilesystem(error))
            if error.is_not_found() =>
        {
            None
        }
        found => found?,
    };
    let mut stores: VecDeque<_> = VecDeque::from([(
        roots.state.clone(),
        0u64,
        None::<(ProjectId, AttemptId)>,
        false,
    )]);
    let mut nodes = Vec::new();
    let mut seen = BTreeMap::new();
    let mut duplicate_count = 0u64;
    let mut conflicts = Vec::new();
    let mut examined = 0u64;
    let mut unfetched = Vec::new();
    let mut children_absent = 0u64;
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
    while let Some((state, depth, owner, expected)) = stores.pop_front() {
        let call = provenant_run::Request {
            clock: &clock,
            budget: WaitBudget::cli(clock.mono_now(), 5).map_err(|_| Error::Encode)?,
            cancel: &cancel,
        };
        let (owner_project, owner_attempt) = owner
            .map_or((selected, None), |(project, attempt)| {
                (Some(project), Some(attempt))
            });
        if examined == max_nodes {
            truncated = true;
            unfetched.push(json!({"project_id":owner_project,"parent_attempt":owner_attempt,"reason":"node_limit"}));
            continue;
        }
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
                    unfetched.push(json!({"project_id":owner_project,"parent_attempt":owner_attempt,"reason":"missing_store"}));
                }
                continue;
            }
            Err(error) if owner.is_some() => {
                unfetched.push(json!({"project_id":owner_project,"parent_attempt":owner_attempt,"reason":"unavailable","detail":error.to_string()}));
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        if depth == 0 {
            root_coverage = "complete";
        }
        let store_path = state.as_path().to_string_lossy().into_owned();
        let mut after = 0;
        let mut through = None;
        loop {
            let remaining = max_nodes.saturating_sub(examined);
            if remaining == 0 {
                truncated = true;
                unfetched.push(json!({"project_id":owner_project,"parent_attempt":owner_attempt,"reason":"node_limit"}));
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
                    unfetched.push(json!({"project_id":owner_project,"parent_attempt":owner_attempt,"reason":"unavailable","detail":error.to_string()}));
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
                let parents = (lane.run.parent_attempt, lane.run.parent, attempt.retry_of);
                if let Some((existing, existing_parents, existing_store)) = seen.get(&key) {
                    duplicate_count += 1;
                    if *existing != identity {
                        conflicts.push(json!({"project_id":attempt.project_id,"attempt_id":attempt.attempt_id,"reason":"conflicting_identity","values":{"stored":{"run_id":existing.0,"task_id":existing.1},"observed":{"run_id":identity.0,"task_id":identity.1}},"store":{"stored":existing_store,"observed":store_path.clone()}}));
                    }
                    if *existing_parents != parents {
                        conflicts.push(json!({"project_id":attempt.project_id,"attempt_id":attempt.attempt_id,"reason":"conflicting_parents","values":{"stored":{"dispatch":existing_parents.0,"continuation":existing_parents.1,"retry":existing_parents.2},"observed":{"dispatch":parents.0,"continuation":parents.1,"retry":parents.2}},"store":{"stored":existing_store,"observed":store_path.clone()}}));
                    }
                    let duplicate_child = state
                        .as_path()
                        .join("children")
                        .join(attempt.attempt_id.to_string());
                    match std::fs::symlink_metadata(duplicate_child) {
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        _ => unfetched.push(json!({"project_id":attempt.project_id,"parent_attempt":attempt.attempt_id,"reason":"unavailable","detail":"duplicate observation child store not traversed"})),
                    }
                    continue;
                }
                seen.insert(key, (identity, parents, store_path.clone()));
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
                let expected = !lane.run.children.is_empty();
                match std::fs::symlink_metadata(&child_path) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        if expected {
                            unfetched.push(json!({"project_id":attempt.project_id,"parent_attempt":attempt.attempt_id,"reason":"missing_store"}));
                        } else { children_absent += 1; }
                    }
                    Err(_) => unfetched.push(json!({"project_id":attempt.project_id,"parent_attempt":attempt.attempt_id,"reason":"unavailable"})),
                    Ok(_) if depth + 1 >= max_depth => {
                        truncated = true;
                        unfetched.push(json!({"project_id":attempt.project_id,"parent_attempt":attempt.attempt_id,"reason":"depth_limit"}));
                    }
                    Ok(_) => match inputs.resolve(&provenant_os::fs::SystemPathResolver) {
                        Ok(child) if child.state.as_path() == child_path => {
                            stores.push_back((child.state, depth + 1, Some((attempt.project_id, attempt.attempt_id)), expected))
                        }
                        _ => unfetched.push(
                            json!({"project_id":attempt.project_id,"parent_attempt":attempt.attempt_id,"reason":"unavailable"}),
                        ),
                    },
                }
                nodes.push(json!({"lane_id":{"host":host,"project_id":attempt.project_id,"run_id":attempt.run_id,"task_id":attempt.task_id,"attempt_id":attempt.attempt_id},"depth":depth,"store_parent_attempt":owner_attempt,"metadata":lane}));
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
                edges.push(json!({"project_id":node["lane_id"]["project_id"],"kind":kind,"parent_attempt":parent,"child_attempt":node["lane_id"]["attempt_id"],"parent_status":if ids.contains(&(node["lane_id"]["project_id"].as_str().unwrap_or(""), parent)) { "present" } else { "missing" }}));
            }
        }
        if let Some(parent) = node["metadata"]["attempt"]["retry_of"].as_str() {
            edges.push(json!({"project_id":node["lane_id"]["project_id"],"kind":"retry","parent_attempt":parent,"child_attempt":node["lane_id"]["attempt_id"],"parent_status":if ids.contains(&(node["lane_id"]["project_id"].as_str().unwrap_or(""), parent)) { "present" } else { "missing" }}));
        }
    }
    let partial = truncated
        || !unfetched.is_empty()
        || !conflicts.is_empty()
        || edges.iter().any(|edge| edge["parent_status"] == "missing");
    for node in &mut nodes {
        let children: Vec<_> = edges
            .iter()
            .filter(|edge| {
                edge["project_id"] == node["lane_id"]["project_id"]
                    && edge["parent_attempt"] == node["lane_id"]["attempt_id"]
            })
            .map(|edge| json!({"kind":edge["kind"],"attempt_id":edge["child_attempt"]}))
            .collect();
        let metadata = node["metadata"].clone();
        node["children"] = json!(children);
        node["status"] = metadata["attempt"]["state"].clone();
        node["title"] = metadata["attempt"]["title"].clone();
        node["usage"] = metadata["attempt"]["usage"].clone();
        node["parents"] = json!({"dispatch":metadata["run"]["parent_attempt"],"continuation":metadata["run"]["parent"],"retry":metadata["attempt"]["retry_of"]});
        if !detail {
            node.as_object_mut()
                .ok_or(Error::Encode)?
                .remove("metadata");
        }
    }
    let totals: BTreeMap<_, _> = totals
        .into_iter()
        .map(|(key, (known, missing))| (key, json!({"known_sum":known,"missing":missing})))
        .collect();
    Ok(
        json!({"schema_version":1,"nodes":nodes,"edges":edges,"totals":{"attempts":seen.len(),"duplicate_observations":duplicate_count,"usage":totals},"identity_conflicts":conflicts,"truncated":truncated,"children_absent":children_absent,"unfetched_children":unfetched,"coverage":{"scope":"local","host":host,"runs":root_coverage,"nested_runs":if partial { "partial" } else { "complete" }},"limits":{"depth":max_depth,"nodes":max_nodes}}),
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
        assert_eq!(read(&roots, None, 3, 100, false)?["nodes"], json!([]));
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
            |run: u8, attempt: u8, request: CanonicalRequest| -> Result<PreparedRun, Box<dyn std::error::Error>> {
                Ok(PreparedRun {
                    run_id: RunId::from_bytes([run; 16]),
                    operation_id: OperationId::parse(&format!("tree-{run}-{attempt}"))?,
                    created_at: clock.wall_now()?,
                    request,
                    attempts: vec![NewAttempt {
                        task_id: task,
                        attempt_id: AttemptId::from_bytes([attempt; 16]),
                        prompt: "body".into(),
                    }],
                })
            };
        store.create(&prepared(1, 1, request.clone())?, &call)?;
        let leaf = read(&roots, None, 1, 100, false)?;
        assert_eq!(leaf["coverage"]["nested_runs"], "complete");
        assert_eq!(leaf["children_absent"], 1);
        assert_eq!(leaf["truncated"], false);
        let project2 = ProjectId::from_bytes([7; 16]);
        let other_context = ExecutionContext {
            project_id: project2,
            ..context.clone()
        };
        let other_store = RunStore::open(&other_context, &call)?;
        other_store.create(&prepared(4, 4, request.clone())?, &call)?;
        let child_path = roots
            .state
            .as_path()
            .join("children")
            .join(AttemptId::from_bytes([1; 16]).to_string());
        inputs.state_dir = Some(child_path);
        let child = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let child_state = child.state.clone();
        let child_store = RunStore::open(
            &ExecutionContext {
                state_root: child.state,
                ..context.clone()
            },
            &call,
        )?;
        let other_child = RunStore::open(
            &ExecutionContext {
                state_root: child_state,
                ..other_context.clone()
            },
            &call,
        )?;
        let mut duplicate = request.clone();
        duplicate.parent_attempt = Some(AttemptId::from_bytes([9; 16]));
        request.parent_attempt = Some(AttemptId::from_bytes([1; 16]));
        request.lane_depth = Some(2);
        child_store.create(&prepared(2, 2, request.clone())?, &call)?;
        request.parent = Some(AttemptId::from_bytes([2; 16]));
        request.parent_attempt = Some(AttemptId::from_bytes([9; 16]));
        child_store.create(&prepared(3, 3, request.clone())?, &call)?;
        let missing = read(&roots, None, 3, 100, false)?;
        assert_eq!(missing["identity_conflicts"], json!([]));
        assert_eq!(missing["unfetched_children"], json!([]));
        assert_eq!(missing["coverage"]["nested_runs"], "partial");
        other_child.create(&prepared(4, 4, duplicate)?, &call)?;
        let child4_path = roots
            .state
            .as_path()
            .join("children")
            .join(AttemptId::from_bytes([4; 16]).to_string());
        inputs.state_dir = Some(child4_path.clone());
        let child4 = RunStore::open(
            &ExecutionContext {
                state_root: inputs.resolve(&provenant_os::fs::SystemPathResolver)?.state,
                ..other_context
            },
            &call,
        )?;
        let mut other_request = request.clone();
        other_request.parent = None;
        other_request.parent_attempt = Some(AttemptId::from_bytes([4; 16]));
        child4.create(&prepared(2, 2, other_request)?, &call)?;
        let tree = read(&roots, None, 3, 100, false)?;
        assert_eq!(tree["totals"]["attempts"], 5);
        assert_eq!(
            tree["nodes"]
                .as_array()
                .ok_or("nodes")?
                .iter()
                .filter(
                    |node| node["lane_id"]["attempt_id"] == json!(AttemptId::from_bytes([2; 16]))
                )
                .map(|node| node["lane_id"]["project_id"].as_str().unwrap_or(""))
                .collect::<BTreeSet<_>>()
                .len(),
            2
        );
        assert_eq!(tree["totals"]["duplicate_observations"], 1);
        assert_eq!(tree["coverage"]["nested_runs"], "partial");
        assert_eq!(
            tree["identity_conflicts"][0]["reason"],
            "conflicting_parents"
        );
        let exact = read(&roots, None, 3, 6, false)?;
        assert_eq!(exact["totals"]["attempts"], 5);
        assert_eq!(exact["truncated"], false);
        assert_eq!(exact["unfetched_children"], json!([]));
        assert_eq!(exact["totals"]["duplicate_observations"], 1);
        assert_eq!(tree["totals"]["usage"]["reasoning"]["missing"], 5);
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
        assert!(
            tree["edges"]
                .as_array()
                .ok_or("edges")?
                .iter()
                .any(|e| e["kind"] == "retry"
                    && e["parent_status"] == "present"
                    && e["project_id"] == json!(project))
        );
        assert!(
            tree["nodes"]
                .as_array()
                .ok_or("nodes")?
                .iter()
                .all(|n| n.get("metadata").is_none())
        );
        assert!(
            tree["nodes"]
                .as_array()
                .ok_or("nodes")?
                .iter()
                .any(|n| n["children"].as_array().is_some_and(|c| !c.is_empty()))
        );
        let detailed = read(&roots, None, 3, 100, true)?;
        assert!(detailed["nodes"][0]["metadata"]["run"].is_object());
        assert!(serde_json::to_vec(&tree)?.len() < serde_json::to_vec(&detailed)?.len());
        let mut pair = request.clone();
        pair.parent = None;
        pair.parent_attempt = Some(AttemptId::from_bytes([9; 16]));
        pair.lane_depth = Some(3);
        child4.create(&prepared(6, 6, pair.clone())?, &call)?;
        other_child.create(&prepared(7, 6, pair)?, &call)?;
        let identified = read(&roots, None, 3, 100, false)?;
        let conflicting = identified["identity_conflicts"]
            .as_array()
            .ok_or("conflicts")?
            .iter()
            .find(|entry| entry["attempt_id"] == json!(AttemptId::from_bytes([6; 16])))
            .ok_or("conflicting_identity")?;
        assert_eq!(conflicting["reason"], "conflicting_identity");
        assert_eq!(
            conflicting["values"]["stored"]["run_id"],
            json!(RunId::from_bytes([6; 16]))
        );
        assert_eq!(
            conflicting["values"]["observed"]["run_id"],
            json!(RunId::from_bytes([7; 16]))
        );
        assert_eq!(
            conflicting["store"]["stored"],
            child4_path.to_string_lossy().as_ref()
        );
        assert_eq!(
            conflicting["store"]["observed"],
            child_state.to_string_lossy().as_ref()
        );
        assert!(
            !identified["identity_conflicts"]
                .as_array()
                .ok_or("conflicts")?
                .iter()
                .any(|entry| entry["attempt_id"] == json!(AttemptId::from_bytes([6; 16]))
                    && entry["reason"] == "conflicting_parents")
        );
        let selected = read(&roots, Some(project2), 3, 100, false)?;
        assert_eq!(selected["totals"]["attempts"], 3);
        assert!(
            selected["nodes"]
                .as_array()
                .ok_or("nodes")?
                .iter()
                .all(|n| n["lane_id"]["project_id"] == json!(project2))
        );
        let limited = read(&roots, None, 1, 100, false)?;
        assert_eq!(limited["truncated"], true);
        assert_eq!(limited["unfetched_children"][0]["reason"], "depth_limit");
        let limited = read(&roots, None, 3, 1, false)?;
        assert!(limited["truncated"].as_bool().unwrap_or(false));
        assert_eq!(
            limited["unfetched_children"]
                .as_array()
                .ok_or("unfetched")?
                .iter()
                .filter(|entry| entry["reason"] == "node_limit")
                .count(),
            2
        );
        assert!(tree["children_absent"].as_u64().unwrap_or(0) > 0);
        std::fs::create_dir_all(
            inputs
                .state_dir
                .as_ref()
                .ok_or("child state")?
                .parent()
                .ok_or("children")?
                .join(AttemptId::from_bytes([1; 16]).to_string())
                .join("children")
                .join(AttemptId::from_bytes([4; 16]).to_string()),
        )?;
        let duplicate_gap = read(&roots, None, 3, 100, false)?;
        assert!(
            duplicate_gap["unfetched_children"]
                .as_array()
                .ok_or("unfetched")?
                .iter()
                .any(|entry| entry["detail"] == "duplicate observation child store not traversed")
        );
        std::fs::create_dir_all(
            roots
                .state
                .as_path()
                .join("children")
                .join(AttemptId::from_bytes([1; 16]).to_string())
                .join("children")
                .join(AttemptId::from_bytes([2; 16]).to_string()),
        )?;
        let empty_child = read(&roots, None, 3, 100, false)?;
        assert!(
            empty_child["unfetched_children"]
                .as_array()
                .ok_or("unfetched")?
                .iter()
                .any(|entry| entry["reason"] == "missing_store"
                    && entry["project_id"] == json!(project)
                    && entry["parent_attempt"] == json!(AttemptId::from_bytes([1; 16])))
        );
        let db = rusqlite::Connection::open(child4_path.join("run.sqlite3"))?;
        let raw: Vec<u8> = db.query_row(
            "SELECT record FROM runs WHERE id=?1",
            [RunId::from_bytes([2; 16]).to_string()],
            |row| row.get(0),
        )?;
        let mut record: Value = serde_json::from_slice(&raw)?;
        record["children"] = json!([RunId::from_bytes([42; 16])]);
        db.execute(
            "UPDATE runs SET record=?1 WHERE id=?2",
            rusqlite::params![
                serde_json::to_vec(&record)?,
                RunId::from_bytes([2; 16]).to_string()
            ],
        )?;
        drop(db);
        let missing_store = read(&roots, None, 3, 100, false)?;
        assert!(
            missing_store["unfetched_children"]
                .as_array()
                .ok_or("unfetched")?
                .iter()
                .any(|entry| entry["reason"] == "missing_store"
                    && entry["project_id"] == json!(project2))
        );
        let bad = child4_path
            .join("children")
            .join(AttemptId::from_bytes([2; 16]).to_string());
        std::fs::create_dir_all(child4_path.join("children"))?;
        std::os::unix::fs::symlink(base.join("outside"), bad)?;
        let unavailable = read(&roots, None, 3, 100, false)?;
        assert!(
            unavailable["unfetched_children"]
                .as_array()
                .ok_or("unfetched")?
                .iter()
                .any(|entry| entry["reason"] == "unavailable"
                    && entry["project_id"] == json!(project2))
        );
        Ok(())
    }
}
