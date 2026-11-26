//! Bounded host-local composition of indexed run stores.
use super::Error;
use provenant_core::{ActorId, AttemptId, CancelToken, Clock, ProjectId, Roots};
use provenant_os::clock::SystemClock;
use provenant_run::{ExecutionContext, RunStore};
use serde_json::Value;
#[cfg(test)]
use serde_json::json;
use std::collections::{BTreeMap, VecDeque};

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
    let census = provenant_supervisor::census::ProcessObserver::census(
        &provenant_supervisor::census::SystemProcessObserver,
        provenant_supervisor::census::MAX_CENSUS_ENTRIES,
    )
    .ok();
    let mut stores: VecDeque<_> =
        VecDeque::from([(roots.state.clone(), 0u64, None::<(ProjectId, AttemptId)>)]);
    let mut nodes = Vec::new();
    #[allow(clippy::type_complexity)]
    let mut seen: BTreeMap<
        (ProjectId, AttemptId),
        (
            provenant_records::WorkKey,
            (Option<AttemptId>, Option<AttemptId>),
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
            unfetched.push(super::projection::tree_unfetched_child(
                &owner_project,
                &owner_attempt,
                "node_limit",
                None,
            ));
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
                    unfetched.push(super::projection::tree_unfetched_child(
                        &owner_project,
                        &owner_attempt,
                        "missing_store",
                        None,
                    ));
                }
                continue;
            }
            Err(error) if owner.is_some() => {
                let detail = error.to_string();
                unfetched.push(super::projection::tree_unfetched_child(
                    &owner_project,
                    &owner_attempt,
                    "unavailable",
                    Some(&detail),
                ));
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
                unfetched.push(super::projection::tree_unfetched_child(
                    &owner_project,
                    &owner_attempt,
                    "node_limit",
                    None,
                ));
                break;
            }
            let page =
                match store.board_page(selected, true, remaining.min(100), after, through, &call) {
                    Ok(page) => page,
                    Err(error) if owner.is_some() => {
                        let detail = error.to_string();
                        unfetched.push(super::projection::tree_unfetched_child(
                            &owner_project,
                            &owner_attempt,
                            "unavailable",
                            Some(&detail),
                        ));
                        break;
                    }
                    Err(error) => return Err(error.into()),
                };
            through = Some(page.high_water);
            for lane in page.lanes {
                examined += 1;
                let attempt = &lane.attempt;
                let key = (attempt.project_id, attempt.attempt_id);
                let work_key = attempt.key.clone();
                // A dispatch parent is sealed on the admitting operation; a
                // continuation is the only source of `retry_of`.
                let parents = (lane.operation.parent_attempt, attempt.retry_of);
                if let Some((existing, existing_parents, existing_store)) = seen.get(&key) {
                    duplicate_count += 1;
                    if *existing != work_key {
                        conflicts.push(super::projection::tree_identity_conflict(
                            &attempt.project_id,
                            &attempt.attempt_id,
                            "conflicting_work_key",
                            existing,
                            &work_key,
                            existing_store,
                            &store_path,
                        ));
                    }
                    if *existing_parents != parents {
                        let stored = super::projection::tree_parent_values(
                            &existing_parents.0,
                            &existing_parents.1,
                        );
                        let observed =
                            super::projection::tree_parent_values(&parents.0, &parents.1);
                        conflicts.push(super::projection::tree_identity_conflict(
                            &attempt.project_id,
                            &attempt.attempt_id,
                            "conflicting_parents",
                            &stored,
                            &observed,
                            existing_store,
                            &store_path,
                        ));
                    }
                    continue;
                }
                seen.insert(key, (work_key.clone(), parents, store_path.clone()));
                let effective = attempt.effective_usage();
                let usage = serde_json::to_value(&effective).map_err(|_| Error::Encode)?;
                let projected = crate::run::project_lane_with_census(
                    attempt,
                    &[],
                    state.as_path(),
                    clock.wall_now().ok(),
                    attempt.superseded_by.is_none(),
                    census.as_ref().ok_or(()),
                );
                let projected = serde_json::to_value(projected).map_err(|_| Error::Encode)?;
                let lane_id = super::projection::lane_id(
                    &serde_json::to_value(host).map_err(|_| Error::Encode)?,
                    &serde_json::to_value(attempt.project_id).map_err(|_| Error::Encode)?,
                    &serde_json::to_value(work_key).map_err(|_| Error::Encode)?,
                    &projected["attempt_id"],
                );
                let operation = serde_json::to_value(&lane.operation).map_err(|_| Error::Encode)?;
                let parents = super::projection::tree_parents(&operation, &projected);
                nodes.push(super::projection::tree_node(
                    super::projection::TreeNodeInput {
                        lane_id: &lane_id,
                        depth,
                        store_parent_attempt: &serde_json::to_value(owner_attempt)
                            .map_err(|_| Error::Encode)?,
                        store_path: include_store_path.then_some(store_path.as_str()),
                        usage: &usage,
                        parents: &parents,
                        operation: &operation,
                        attempt: &projected,
                        children: &[],
                    },
                ));
            }
            match page.next {
                Some(next) => after = next,
                None => break,
            }
        }
    }
    let edges = super::projection::tree_edges(&nodes).ok_or(Error::Encode)?;
    let partial = truncated
        || !unfetched.is_empty()
        || !conflicts.is_empty()
        || edges.iter().any(|edge| edge["parent_status"] == "missing");
    super::projection::tree_finalize_nodes(&mut nodes, &edges, include_store_path, detail)
        .ok_or(Error::Encode)?;
    let usage = super::projection::tree_usage_totals(&nodes).ok_or(Error::Encode)?;
    let totals = super::projection::tree_totals(seen.len(), duplicate_count, &usage);
    Ok(super::projection::tree(
        &nodes,
        &edges,
        &totals,
        &conflicts,
        truncated,
        children_absent,
        &unfetched,
        &serde_json::to_value(host).map_err(|_| Error::Encode)?,
        root_coverage,
        partial,
        max_depth,
        max_nodes,
    ))
}

/// Select descendants across dispatch and continuation edges from a bounded snapshot.
/// Missing roots and depth cuts remain explicit because the snapshot may be partial.
pub(super) fn select_subtree(tree: &mut Value, root: &str, max_depth: u64) -> Result<(), Error> {
    *tree = super::projection::tree_selected_subtree(tree, root, max_depth).ok_or(Error::Encode)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::{Digest, OperationId, RootInputs};
    use provenant_records::CanonicalV1;
    use provenant_run::{CanonicalRequest, PreparedRun};

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
    // Spec: docs/specs/fabric.md#publication-boundary
    #[test]
    fn host_tree_preserves_lineage_detail_and_usage() -> Result<(), Box<dyn std::error::Error>> {
        let _mode = provenant_redact::scoped(provenant_redact::Mode::Secrets);
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
        let mut request: CanonicalRequest = serde_json::from_value(
            json!({"schema_version":1,"task":{"prompt_digest":CanonicalV1::file_digest(b"body"),"route":{"kind":"model","provider":"fixture","model":"fixture/model","effort":"high"},"authority_digest":Digest::from_bytes([5;32]),"provider_timeout_seconds":30,"delivery_id":null},"parent":null,"lane_depth":1}),
        )?;
        let raw_title = "shared-tree-secret token: it; it stays";
        request.task.title = Some(raw_title.into());
        // A launch names its own work key; a continuation takes its parent's.
        let prepared = |attempt: u8,
                        mut request: CanonicalRequest|
         -> Result<PreparedRun, Box<dyn std::error::Error>> {
            request.task.context = Some(provenant_records::run::WorkContext {
                graph_id: Some(format!("lane-{attempt}")),
                node_id: Some("node".into()),
                attempt: Some(1),
                ..Default::default()
            });
            Ok(PreparedRun {
                attempt_id: AttemptId::from_bytes([attempt; 16]),
                operation_id: OperationId::parse(&format!("tree-{attempt}"))?,
                created_at: clock.wall_now()?,
                request,
                prompt: "body".into(),
            })
        };
        store.create(&prepared(1, request.clone())?, &call)?;
        request.parent_attempt = Some(AttemptId::from_bytes([1; 16]));
        store.create(&prepared(2, request.clone())?, &call)?;
        request.parent = Some(AttemptId::from_bytes([2; 16]));
        request.parent_attempt = Some(AttemptId::from_bytes([9; 16]));
        store.create(&prepared(3, request.clone())?, &call)?;
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
        other_store.create(&prepared(4, request)?, &call)?;
        // Historical codex totals are session-wide, so they are not counted
        // against one lane. A stated bucket on a per-attempt lane is counted.
        // Spec: docs/specs/cli.md#nested-tree
        let mut observed = store.record(AttemptId::from_bytes([1; 16]), &call)?;
        observed.applied_route = Some(provenant_records::run::RouteRecord {
            provider: Some("codex".into()),
            ..Default::default()
        });
        observed.usage.tokens.input = Some(42);
        let mut attributed = store.record(AttemptId::from_bytes([2; 16]), &call)?;
        attributed.usage.tokens.input = Some(1_234);
        let db = rusqlite::Connection::open(roots.state.as_path().join("run.sqlite3"))?;
        for record in [&observed, &attributed] {
            db.execute(
                "UPDATE attempts SET record=?1 WHERE id=?2",
                rusqlite::params![serde_json::to_vec(record)?, record.attempt_id.to_string()],
            )?;
        }
        let tree = read(&roots, None, 3, 100, false)?;
        assert!(tree["nodes"][0]["lane_id"]["key"].is_object());
        assert!(tree["nodes"][0]["lane_id"].get("run_id").is_none());
        assert!(tree["nodes"][0]["lane_id"].get("task_id").is_none());
        let nodes = tree["nodes"].as_array().ok_or("nodes")?;
        let codex_id = json!(AttemptId::from_bytes([1; 16]).to_string());
        let attributed_id = json!(AttemptId::from_bytes([2; 16]).to_string());
        let codex_node = nodes
            .iter()
            .find(|node| node["lane_id"]["attempt_id"] == codex_id)
            .ok_or("codex node")?;
        let attributed_node = nodes
            .iter()
            .find(|node| node["lane_id"]["attempt_id"] == attributed_id)
            .ok_or("attributed node")?;
        assert_eq!(codex_node["usage"]["tokens"]["input"], Value::Null);
        assert_eq!(attributed_node["usage"]["tokens"]["input"], json!(1_234));
        assert_eq!(tree["totals"]["usage"]["input"]["known_sum"], 1_234);
        assert_eq!(tree["totals"]["usage"]["input"]["missing"], 3);
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
        // Spec: docs/specs/lane-contracts.md#observation-events
        let mut subtree = tree.clone();
        select_subtree(&mut subtree, &AttemptId::from_bytes([2; 16]).to_string(), 3)?;
        assert_eq!(subtree["subtree"]["status"], "found");
        assert_eq!(subtree["totals"]["attempts"], 2);
        assert!(
            subtree["edges"]
                .as_array()
                .ok_or("edges")?
                .iter()
                .any(|e| e["kind"] == "continuation")
        );
        let mut bounded = tree.clone();
        select_subtree(&mut bounded, &AttemptId::from_bytes([2; 16]).to_string(), 1)?;
        assert_eq!(bounded["subtree"]["depth_limited"], true);
        assert_eq!(bounded["coverage"]["nested_runs"], "partial");
        let mut missing = tree.clone();
        select_subtree(&mut missing, &AttemptId::from_bytes([8; 16]).to_string(), 3)?;
        assert_eq!(missing["subtree"]["status"], "missing");
        assert_eq!(missing["coverage"]["nested_runs"], "partial");
        let detailed = read(&roots, None, 3, 100, true)?;
        assert!(detailed["nodes"][0]["metadata"]["operation"].is_object());
        assert!(serde_json::to_vec(&tree)?.len() < serde_json::to_vec(&detailed)?.len());
        // Exercise the actual compact and detailed producer shapes through transport.
        let projector =
            provenant_confinement::publication::Projector::new([b"shared-tree-secret".to_vec()])?;
        for raw in [&tree, &detailed] {
            let mut expected = raw.clone();
            let mut found = false;
            for node in expected["nodes"].as_array_mut().ok_or("nodes")? {
                if node["title"].as_str() == Some(raw_title) {
                    found = true;
                    let title = "[REDACTED] token: [REDACTED]; it stays";
                    if let Some(metadata) = node.get_mut("metadata") {
                        metadata["attempt"]["title"] = json!(title);
                    }
                }
            }
            assert!(found);
            let response = provenant_fabric_runtime::hosts::PeerResponse {
                extra: Default::default(),
                version: 1,
                protocol: provenant_fabric_runtime::hosts::PROTOCOL,
                host: provenant_core::HostId::from_bytes([8; 16]),
                project: provenant_fabric_runtime::identity::CrossHostProjectKey::from_home(
                    &base.join("home"),
                    &base.join("home/repo/.git"),
                )?,
                result: provenant_fabric_runtime::hosts::PeerResult::Console {
                    data: raw.clone(),
                    extra: Default::default(),
                },
            };
            let bytes =
                provenant_fabric_runtime::hosts::encode_peer_response(response, &projector)?;
            let encoded: Value = serde_json::from_slice(&bytes)?;
            assert_eq!(encoded["result"]["data"], expected);
        }
        assert_eq!(
            store
                .record(AttemptId::from_bytes([1; 16]), &call)?
                .title
                .as_deref(),
            Some(raw_title)
        );
        let detail =
            super::super::detail::read(&roots, &AttemptId::from_bytes([2; 16]).to_string())?;
        assert_eq!(detail["status"], "found");
        assert_eq!(detail["artifacts"][0]["bytes"], 4);
        assert_eq!(detail["artifacts"][0]["present"], true);
        assert_eq!(
            detail["usage"]["tokens"]["input"],
            json!(1_234),
            "the reported bucket reaches console detail"
        );
        assert_eq!(detail["usage"]["tokens"]["reasoning"], Value::Null);
        assert!(
            detail["usage_coverage"]["known"]
                .as_array()
                .ok_or("known buckets")?
                .contains(&json!("input"))
        );
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
