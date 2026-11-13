//! Bounded host-local composition of indexed run stores.
use super::Error;
use provenant_core::{ActorId, AttemptId, CancelToken, Clock, ProjectId, Roots};
use provenant_os::clock::SystemClock;
use provenant_run::{ExecutionContext, RunStore};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub(super) fn read(
    roots: &Roots,
    selected: Option<ProjectId>,
    max_depth: u64,
    max_nodes: u64,
    detail: bool,
) -> Result<Value, Error> {
    read_inner(roots, selected, max_depth, max_nodes, detail, false)
}

pub(super) fn read_inner(
    roots: &Roots,
    selected: Option<ProjectId>,
    max_depth: u64,
    max_nodes: u64,
    detail: bool,
    include_store_path: bool,
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
    let mut stores: VecDeque<_> =
        VecDeque::from([(roots.state.clone(), 0u64, None::<(ProjectId, AttemptId)>)]);
    let mut nodes = Vec::new();
    #[allow(clippy::type_complexity)]
    let mut seen: BTreeMap<
        (ProjectId, AttemptId),
        (
            provenant_records::WorkKey,
            (Option<AttemptId>, Option<AttemptId>, Option<AttemptId>),
            String,
        ),
    > = BTreeMap::new();
    let mut duplicate_count = 0u64;
    let mut conflicts = Vec::new();
    let mut examined = 0u64;
    let mut unfetched = Vec::new();
    let children_absent = 0u64;
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
        let call = provenant_run::Request {
            clock: &clock,
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
                let work_key = attempt.key.clone().ok_or(Error::Encode)?;
                let parents = (lane.run.parent_attempt, lane.run.parent, attempt.retry_of);
                if let Some((existing, existing_parents, existing_store)) = seen.get(&key) {
                    duplicate_count += 1;
                    if *existing != work_key {
                        conflicts.push(json!({"project_id":attempt.project_id,"attempt_id":attempt.attempt_id,"reason":"conflicting_work_key","values":{"stored":existing,"observed":work_key},"store":{"stored":existing_store,"observed":store_path.clone()}}));
                    }
                    if *existing_parents != parents {
                        conflicts.push(json!({"project_id":attempt.project_id,"attempt_id":attempt.attempt_id,"reason":"conflicting_parents","values":{"stored":{"dispatch":existing_parents.0,"continuation":existing_parents.1,"retry":existing_parents.2},"observed":{"dispatch":parents.0,"continuation":parents.1,"retry":parents.2}},"store":{"stored":existing_store,"observed":store_path.clone()}}));
                    }
                    continue;
                }
                seen.insert(key, (work_key.clone(), parents, store_path.clone()));
                let usage =
                    serde_json::to_value(attempt.effective_usage()).map_err(|_| Error::Encode)?;
                for (name, (sum, missing)) in &mut totals {
                    if let Some(value) = usage[*name].as_u64() {
                        *sum = sum.checked_add(value).ok_or(Error::Encode)?;
                    } else {
                        *missing += 1;
                    }
                }
                let projected = crate::run::project_lane(
                    attempt,
                    &[],
                    state.as_path(),
                    clock.wall_now().ok(),
                    false,
                );
                nodes.push(json!({"lane_id":{"host":host,"project_id":attempt.project_id,"key":work_key,"attempt_id":projected.attempt_id},"depth":depth,"store_parent_attempt":owner_attempt,"store_path":store_path,"usage":usage,"metadata":{"run":lane.run,"attempt":projected}}));
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
        if !include_store_path {
            node.as_object_mut()
                .ok_or(Error::Encode)?
                .remove("store_path");
        }
        node["children"] = json!(children);
        node["status"] = metadata["attempt"]["state"].clone();
        node["title"] = metadata["attempt"]["title"].clone();
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
    use provenant_core::{Digest, OperationId, RootInputs, RunId, TaskId};
    use provenant_records::CanonicalV1;
    use provenant_run::{CanonicalRequest, NewAttempt, PreparedRun};

    #[test]
    fn old_child_stores_are_not_read() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let base = temp.path().canonicalize()?;
        std::fs::create_dir(base.join("home"))?;
        std::fs::create_dir(base.join("checkout"))?;
        let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(base.join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        std::fs::create_dir_all(roots.state.as_path().join("children/old"))?;
        std::fs::set_permissions(
            roots.state.as_path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )?;
        std::fs::write(
            roots.state.as_path().join("children/old/run.sqlite3"),
            b"invalid database",
        )?;
        let tree = read(&roots, None, 3, 100, false)?;
        assert_eq!(tree["nodes"], json!([]));
        assert_eq!(tree["unfetched_children"], json!([]));
        Ok(())
    }
    #[test]
    fn host_tree_preserves_lineage_detail_and_usage() -> Result<(), Box<dyn std::error::Error>> {
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
            json!({"schema_version":1,"tasks":[{"task_id":task,"prompt_digest":CanonicalV1::file_digest(b"body"),"route":{"kind":"model","provider":"fixture","model":"fixture/model","effort":"high"},"authority_digest":Digest::from_bytes([5;32]),"provider_timeout_seconds":30,"delivery_id":null}],"concurrency":1,"parent":null,"lane_depth":1}),
        )?;
        let prepared = |run: u8,
                        attempt: u8,
                        request: CanonicalRequest|
         -> Result<PreparedRun, Box<dyn std::error::Error>> {
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
        request.parent_attempt = Some(AttemptId::from_bytes([1; 16]));
        store.create(&prepared(2, 2, request.clone())?, &call)?;
        request.parent = Some(AttemptId::from_bytes([2; 16]));
        request.parent_attempt = Some(AttemptId::from_bytes([9; 16]));
        store.create(&prepared(3, 3, request.clone())?, &call)?;
        let other_project = ProjectId::from_bytes([7; 16]);
        let other_store = RunStore::open(
            &ExecutionContext {
                project_id: other_project,
                ..context
            },
            &call,
        )?;
        request.parent = None;
        request.parent_attempt = None;
        other_store.create(&prepared(4, 4, request)?, &call)?;
        // Simulate a pre-fix Codex row containing only a running session total.
        let mut legacy = store.record(AttemptId::from_bytes([1; 16]), &call)?;
        legacy.applied_route = Some(provenant_records::run::RouteRecord {
            provider: Some("codex".into()),
            ..Default::default()
        });
        legacy.usage.uncached_input = Some(42);
        let db = rusqlite::Connection::open(roots.state.as_path().join("run.sqlite3"))?;
        db.execute(
            "UPDATE attempts SET record=?1 WHERE id=?2",
            rusqlite::params![serde_json::to_vec(&legacy)?, legacy.attempt_id.to_string()],
        )?;
        let tree = read(&roots, None, 3, 100, false)?;
        assert!(tree["nodes"][0]["lane_id"]["key"].is_object());
        assert!(tree["nodes"][0]["lane_id"].get("run_id").is_none());
        assert!(tree["nodes"][0]["lane_id"].get("task_id").is_none());
        assert_eq!(tree["totals"]["usage"]["uncached_input"]["missing"], 4);
        assert_eq!(tree["totals"]["attempts"], 4);
        assert_eq!(tree["totals"]["usage"]["reasoning"]["missing"], 4);
        assert_eq!(tree["identity_conflicts"], json!([]));
        assert_eq!(tree["unfetched_children"], json!([]));
        assert_eq!(tree["truncated"], false);
        let edges = tree["edges"].as_array().ok_or("edges")?;
        for (kind, status) in [
            ("dispatch", "present"),
            ("dispatch", "missing"),
            ("continuation", "present"),
            ("retry", "present"),
        ] {
            assert!(
                edges
                    .iter()
                    .any(|edge| edge["kind"] == kind && edge["parent_status"] == status),
                "{kind} {status}"
            );
        }
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
        let detail =
            super::super::detail::read(&roots, &AttemptId::from_bytes([2; 16]).to_string())?;
        assert_eq!(detail["status"], "found");
        assert_eq!(detail["artifacts"][0]["bytes"], 4);
        assert_eq!(detail["artifacts"][0]["present"], true);
        assert_eq!(detail["usage"]["reasoning"], Value::Null);
        let selected = read(&roots, Some(other_project), 3, 100, false)?;
        assert_eq!(selected["totals"]["attempts"], 1);
        assert!(
            selected["nodes"]
                .as_array()
                .ok_or("nodes")?
                .iter()
                .all(|n| n["lane_id"]["project_id"] == json!(other_project))
        );
        let limited = read(&roots, None, 3, 1, false)?;
        assert_eq!(limited["truncated"], true);
        Ok(())
    }
}
