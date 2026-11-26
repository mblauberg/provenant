//! Pure assembly of console response data from bounded, already-loaded values.
//!
//! The owner commands load state and pass the loaded values here; the fixture
//! generator builds one synthetic set of the same values and passes it here too.
//! Every field of a console response is therefore written in exactly one place:
//! a wire change changes both the command and the committed fixtures, and
//! `scripts/check-swift-fixtures` fails until both are updated.
//!
//! These functions never touch the filesystem, the clock or a process. Values
//! that the owner reads (timestamps, snapshots, host identities, coverage
//! decisions) arrive as arguments so the caller keeps that decision visible.

use provenant_core::{AttemptId, ProjectId};
use provenant_records::CanonicalV1;
use provenant_records::wire::LanePresence;
use provenant_run::AttemptState;
use provenant_workflow::session::{SummaryLine, SummarySource};
use serde::Serialize;
use serde_json::{Value, json};

/// One overview lane from an already projected attempt.
pub fn overview_lane(
    project_id: &Value,
    project_label: &(impl Serialize + ?Sized),
    key: &Value,
    task_label: &(impl Serialize + ?Sized),
    attempt: &Value,
    queued_at: &Value,
    attention: bool,
) -> Value {
    json!({
        "project_id": project_id, "project_label": project_label, "key": key,
        "attempt_id": attempt["attempt_id"], "task_label": task_label,
        "state": attempt["state"], "reason": attempt["reason"],
        "presence": attempt["presence"], "queued_at": queued_at,
        "cause": attempt["cause"], "retry_of": attempt["retry_of"],
        "superseded_by": attempt["superseded_by"], "attention": attention,
    })
}

/// One joined needs-you request after the inbox and project owners resolved it.
pub fn overview_needs_you(
    id: &Value,
    title: &str,
    chair_session_id: &Value,
    project_id: &Value,
    project_label: &Value,
) -> Value {
    json!({"id":id,"title":title,"chair_session_id":chair_session_id,
        "project_id":project_id,"project_label":project_label})
}

/// Whether a projected overview lane counts as running or needs attention.
/// Presence is authoritative when observed; durable state is the fallback
/// only when no presence observation exists.
pub fn overview_activity(presence: Option<LanePresence>, state: &AttemptState) -> (bool, bool) {
    let active = match presence {
        Some(LanePresence::Live) => true,
        Some(LanePresence::Unattended | LanePresence::Unknown | LanePresence::Settling) => false,
        None => !matches!(state, AttemptState::Terminal { .. }),
    };
    let attention = matches!(
        presence,
        Some(LanePresence::Unattended | LanePresence::Unknown | LanePresence::Settling)
    );
    (active, attention)
}

/// Whether a board attempt needs attention from its outcome or observation.
pub fn board_attention(state: &AttemptState, presence: Option<LanePresence>) -> bool {
    matches!(state, AttemptState::Terminal { outcome, .. }
        if !matches!(outcome, provenant_run::Outcome::Succeeded | provenant_run::Outcome::Cancelled))
        || matches!(
            presence,
            Some(LanePresence::Unattended | LanePresence::Unknown | LanePresence::Settling)
        )
}

/// Inputs for one board lane, including owner metadata and census coverage.
pub struct BoardLaneInput<'a> {
    pub lane_id: &'a Value,
    pub chair: &'a Value,
    pub operation: &'a Value,
    pub attempt: &'a Value,
    pub wait: &'a Value,
    pub attention: bool,
    pub known: &'a [&'a str],
    pub missing: &'a [&'a str],
    pub observation: &'a str,
}

/// Identity for a board row assembled from the observed host, key and attempt.
pub fn board_lane_id(host: &Value, key: &Value, attempt_id: &Value) -> Value {
    json!({"host":host,"key":key,"attempt_id":attempt_id})
}

/// One board lane, including the owner operation metadata and census coverage.
pub fn board_lane(input: BoardLaneInput<'_>) -> Value {
    let BoardLaneInput {
        lane_id,
        chair,
        operation,
        attempt,
        wait,
        attention,
        known,
        missing,
        observation,
    } = input;
    json!({"lane_id":lane_id,"chair":chair,"metadata":{"operation":operation,"attempt":attempt},
        "wait":wait,"attention":attention,"usage_coverage":{"known":known,"missing":missing},
        "observation_coverage":observation})
}

/// Inputs for one tree node with derived display fields beside the source record.
pub struct TreeNodeInput<'a> {
    pub lane_id: &'a Value,
    pub depth: u64,
    pub store_parent_attempt: &'a Value,
    pub store_path: Option<&'a str>,
    pub usage: &'a Value,
    pub parents: &'a Value,
    pub operation: &'a Value,
    pub attempt: &'a Value,
    pub children: &'a [Value],
}

/// One tree node with all derived display fields kept beside the source record.
pub fn tree_node(input: TreeNodeInput<'_>) -> Value {
    let TreeNodeInput {
        lane_id,
        depth,
        store_parent_attempt,
        store_path,
        usage,
        parents,
        operation,
        attempt,
        children,
    } = input;
    let mut node = json!({"lane_id":lane_id,"depth":depth,
        "store_parent_attempt":store_parent_attempt,"usage":usage,"parents":parents,
        "metadata":{"operation":operation,"attempt":attempt},"children":children,
        "status":attempt["state"],"title":attempt["title"]});
    if let Some(path) = store_path {
        node["store_path"] = json!(path);
    }
    node
}

/// Parent identities projected from the operation and attempt records.
pub fn tree_parents(operation: &Value, attempt: &Value) -> Value {
    tree_parent_values(&operation["parent_attempt"], &attempt["retry_of"])
}

/// Parent identities with the dispatch and continuation sources made explicit.
pub fn tree_parent_values(dispatch: &impl Serialize, continuation: &impl Serialize) -> Value {
    json!({"dispatch":dispatch,"continuation":continuation})
}

/// One bounded child-store failure, with optional underlying detail.
pub fn tree_unfetched_child<P: Serialize, A: Serialize>(
    project_id: &P,
    parent_attempt: &A,
    reason: &str,
    detail: Option<&str>,
) -> Value {
    let mut item = json!({"project_id":project_id,"parent_attempt":parent_attempt,"reason":reason});
    if let Some(detail) = detail {
        item["detail"] = json!(detail);
    }
    item
}

/// Conflicting observations for one attempt identity.
pub fn tree_identity_conflict(
    project_id: &impl Serialize,
    attempt_id: &impl Serialize,
    reason: &str,
    stored: &impl Serialize,
    observed: &impl Serialize,
    stored_store: &str,
    observed_store: &str,
) -> Value {
    json!({"project_id":project_id,"attempt_id":attempt_id,"reason":reason,
        "values":{"stored":stored,"observed":observed},
        "store":{"stored":stored_store,"observed":observed_store}})
}

/// One parent/child edge in the tree projection.
pub fn tree_edge(
    project_id: &Value,
    kind: &str,
    parent: &str,
    child: &str,
    parent_status: &str,
) -> Value {
    json!({"project_id":project_id,"kind":kind,"parent_attempt":parent,
        "child_attempt":child,"parent_status":parent_status})
}

/// Project parent edges and whether each parent is present in the bounded tree.
pub fn tree_edges(nodes: &[Value]) -> Option<Vec<Value>> {
    let ids: std::collections::BTreeSet<_> = nodes
        .iter()
        .map(|node| {
            Some((
                node["lane_id"]["project_id"].as_str()?.to_owned(),
                node["lane_id"]["attempt_id"].as_str()?.to_owned(),
            ))
        })
        .collect::<Option<_>>()?;
    let mut edges = Vec::new();
    for node in nodes {
        let project_id = node["lane_id"]["project_id"].as_str()?;
        let child = node["lane_id"]["attempt_id"].as_str()?;
        for kind in ["dispatch", "continuation"] {
            if let Some(parent) = node["parents"][kind].as_str() {
                edges.push(tree_edge(
                    &node["lane_id"]["project_id"],
                    kind,
                    parent,
                    child,
                    if ids.contains(&(project_id.to_owned(), parent.to_owned())) {
                        "present"
                    } else {
                        "missing"
                    },
                ));
            }
        }
    }
    Some(edges)
}

/// Child links projected from the same tree edges exposed at top level.
pub fn tree_children(edges: &[Value], project_id: &Value, parent: &Value) -> Vec<Value> {
    edges
        .iter()
        .filter(|edge| {
            edge.get("project_id") == Some(project_id) && edge.get("parent_attempt") == Some(parent)
        })
        .map(|edge| json!({"kind":edge["kind"],"attempt_id":edge["child_attempt"]}))
        .collect()
}

/// Apply the display fields derived from tree edges and hide optional detail.
pub fn tree_finalize_nodes(
    nodes: &mut [Value],
    edges: &[Value],
    include_store_path: bool,
    detail: bool,
) -> Option<()> {
    for node in nodes {
        let children = tree_children(
            edges,
            &node["lane_id"]["project_id"],
            &node["lane_id"]["attempt_id"],
        );
        let status = node["metadata"]["attempt"]["state"].clone();
        let title = node["metadata"]["attempt"]["title"].clone();
        if !include_store_path {
            node.as_object_mut()?.remove("store_path");
        }
        node["children"] = json!(children);
        node["status"] = status;
        node["title"] = title;
        if !detail {
            node.as_object_mut()?.remove("metadata");
        }
    }
    Some(())
}

/// Aggregate token totals from the unique projected nodes.
pub fn tree_usage_totals(nodes: &[Value]) -> Option<Value> {
    let mut totals: std::collections::BTreeMap<&str, (u64, u64)> =
        ["input", "cache_read", "cache_write", "output", "reasoning"]
            .into_iter()
            .map(|key| (key, (0, 0)))
            .collect();
    for node in nodes {
        for (name, count) in [
            ("input", node["usage"]["tokens"]["input"].as_u64()),
            ("cache_read", node["usage"]["tokens"]["cache_read"].as_u64()),
            (
                "cache_write",
                node["usage"]["tokens"]["cache_write"].as_u64(),
            ),
            ("output", node["usage"]["tokens"]["output"].as_u64()),
            ("reasoning", node["usage"]["tokens"]["reasoning"].as_u64()),
        ] {
            let (sum, missing) = totals.get_mut(name)?;
            if let Some(count) = count {
                *sum = sum.checked_add(count)?;
            } else {
                *missing = missing.checked_add(1)?;
            }
        }
    }
    let mut result = serde_json::Map::new();
    for (key, (known_sum, missing)) in totals {
        result.insert(
            key.to_owned(),
            json!({"known_sum":known_sum,"missing":missing}),
        );
    }
    Some(Value::Object(result))
}

#[cfg(test)]
#[test]
fn tree_finalization_and_usage_totals_follow_projected_nodes_and_edges() {
    let mut nodes = vec![
        json!({
            "lane_id":{"project_id":"project-1","attempt_id":"parent"},
            "parents":{"dispatch":null,"continuation":null},
            "store_path":"/private/store",
            "metadata":{"operation":{"kind":"dispatch"},"attempt":{"state":"succeeded","title":"parent task"}},
            "usage":{"tokens":{"input":3,"cache_read":null,"cache_write":null,"output":null,"reasoning":null}}
        }),
        json!({
            "lane_id":{"project_id":"project-1","attempt_id":"child"},
            "parents":{"dispatch":"parent","continuation":null},
            "store_path":"/private/store",
            "metadata":{"operation":{"kind":"dispatch"},"attempt":{"state":"failed","title":"child task"}},
            "usage":{"tokens":{"input":null,"cache_read":null,"cache_write":null,"output":2,"reasoning":null}}
        }),
    ];
    let edges = tree_edges(&nodes).expect("tree edges");

    tree_finalize_nodes(&mut nodes, &edges, false, false).expect("tree nodes finalize");
    assert_eq!(nodes[0]["children"][0]["attempt_id"], "child");
    assert_eq!(nodes[0]["status"], "succeeded");
    assert_eq!(nodes[0]["title"], "parent task");
    assert!(nodes[0].get("metadata").is_none());
    assert!(nodes[0].get("store_path").is_none());

    let totals = tree_usage_totals(&nodes).expect("tree usage totals");
    assert_eq!(totals["input"], json!({"known_sum":3,"missing":1}));
    assert_eq!(totals["output"], json!({"known_sum":2,"missing":1}));
    assert_eq!(totals["reasoning"], json!({"known_sum":0,"missing":2}));

    let tree = json!({
        "nodes":nodes,
        "edges":edges,
        "totals":{"attempts":2},
        "coverage":{"nested_runs":"complete"}
    });
    let subtree = tree_selected_subtree(&tree, "parent", 1).expect("selected subtree");
    assert_eq!(subtree["nodes"].as_array().map(Vec::len), Some(1));
    assert_eq!(subtree["nodes"][0]["depth"], 0);
    assert_eq!(subtree["nodes"][0]["children"], json!([]));
    assert_eq!(subtree["totals"], json!({"attempts":1}));
    assert_eq!(
        subtree["subtree"],
        json!({"root":"parent","status":"found","depth_limited":true})
    );
    assert_eq!(subtree["coverage"]["nested_runs"], "partial");
}

/// Restrict tree output to a selected node and its bounded descendants.
pub fn tree_selected_subtree(tree: &Value, root: &str, max_depth: u64) -> Option<Value> {
    let nodes = tree.get("nodes")?.as_array()?;
    let edges = tree.get("edges")?.as_array()?;
    let roots: Vec<_> = nodes
        .iter()
        .filter(|node| node["lane_id"]["attempt_id"] == root)
        .map(|node| node["lane_id"]["project_id"].as_str().map(str::to_owned))
        .collect::<Option<_>>()?;
    let mut selected = std::collections::BTreeMap::new();
    for project in roots {
        selected.insert((project, root.to_owned()), 0u64);
    }
    let mut cut = false;
    loop {
        let mut changed = false;
        for edge in edges {
            let project = edge["project_id"].as_str()?;
            let parent = edge["parent_attempt"].as_str()?;
            let child = edge["child_attempt"].as_str()?;
            if let Some(depth) = selected
                .get(&(project.to_owned(), parent.to_owned()))
                .copied()
            {
                let key = (project.to_owned(), child.to_owned());
                if let std::collections::btree_map::Entry::Vacant(entry) = selected.entry(key) {
                    if depth + 1 >= max_depth {
                        cut = true;
                    } else {
                        entry.insert(depth + 1);
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    let missing = selected.is_empty();
    let selected_node = |node: &Value| {
        selected.contains_key(&(
            node["lane_id"]["project_id"]
                .as_str()
                .unwrap_or("")
                .to_owned(),
            node["lane_id"]["attempt_id"]
                .as_str()
                .unwrap_or("")
                .to_owned(),
        ))
    };
    let kept_edges: Vec<_> = edges
        .iter()
        .filter(|edge| {
            let project = edge["project_id"].as_str().unwrap_or("").to_owned();
            selected.contains_key(&(
                project.clone(),
                edge["parent_attempt"].as_str().unwrap_or("").to_owned(),
            )) && selected.contains_key(&(
                project,
                edge["child_attempt"].as_str().unwrap_or("").to_owned(),
            ))
        })
        .cloned()
        .collect();
    let mut kept = Vec::new();
    for node in nodes.iter().filter(|node| selected_node(node)) {
        let project = node["lane_id"]["project_id"].as_str()?;
        let attempt = node["lane_id"]["attempt_id"].as_str()?;
        let depth = *selected.get(&(project.to_owned(), attempt.to_owned()))?;
        kept.push(tree_subtree_node(node, depth, &kept_edges));
    }

    let mut result = tree.clone();
    result["nodes"] = json!(kept);
    result["edges"] = json!(kept_edges);
    result["totals"] = json!({"attempts":kept.len()});
    result["subtree"] =
        json!({"root":root,"status":if missing { "missing" } else { "found" },"depth_limited":cut});
    if missing || cut {
        result["coverage"]["nested_runs"] = json!("partial");
    }
    Some(result)
}

/// Project one selected node with its relative depth and retained children.
fn tree_subtree_node(node: &Value, depth: u64, edges: &[Value]) -> Value {
    let mut selected = node.clone();
    let project = selected["lane_id"]["project_id"].clone();
    let attempt = selected["lane_id"]["attempt_id"].clone();
    selected["depth"] = json!(depth);
    selected["children"] = json!(tree_children(edges, &project, &attempt));
    selected
}

/// Totals are a projection of observed tree records, not a fixture constant.
pub fn tree_totals(attempts: usize, duplicates: u64, usage: &Value) -> Value {
    json!({"attempts":attempts,"duplicate_observations":duplicates,"usage":usage})
}

/// A search match from one bounded artifact scan.
pub fn search_match(lane_id: &Value, part: &str, snippet: &str, artifact_bytes: u64) -> Value {
    json!({"lane_id":lane_id,"part":part,"snippet":snippet,"artifact_bytes":artifact_bytes})
}

/// Detail lane identity, including project ownership.
pub fn lane_id(host: &Value, project_id: &Value, key: &Value, attempt_id: &Value) -> Value {
    json!({"host":host,"project_id":project_id,"key":key,"attempt_id":attempt_id})
}

/// Detail artifact inventory entry after the owner has checked filesystem presence.
pub fn detail_artifact(
    name: &str,
    bytes: u64,
    digest: &Value,
    complete: bool,
    redacted: bool,
    present: bool,
) -> Value {
    json!({"name":name,"bytes":bytes,"digest":digest,"complete":complete,"redacted":redacted,"present":present})
}

/// Detail timing values, including terminal finish time when available.
pub fn detail_timings(
    queued: &Value,
    started: &Value,
    provider_ms: &Value,
    finished: &Value,
) -> Value {
    json!({"queued_at":queued,"started_at":started,"provider_ms":provider_ms,"finished_at":finished})
}

/// Usage data and bucket coverage from the same effective attempt usage.
pub fn detail_usage(
    usage: &provenant_run::UsageRecord,
) -> Result<(Value, Vec<&'static str>, Vec<&'static str>), serde_json::Error> {
    let tokens = &usage.tokens;
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
            known.push(name);
        } else {
            missing.push(name);
        }
    }
    Ok((serde_json::to_value(usage)?, known, missing))
}

/// Attach result-only owner evidence to the common output projection.
pub fn output_result_evidence(
    data: &mut Value,
    writer_base: &Value,
    writer_head: &Value,
    residual_tree: &Value,
    publication: &Value,
    changed_files: &Value,
    changed_files_total: &Value,
) {
    data["writer_base"] = writer_base.clone();
    data["writer_head"] = writer_head.clone();
    data["residual_tree"] = residual_tree.clone();
    data["publication"] = publication.clone();
    data["changed_files"] = changed_files.clone();
    data["changed_files_total"] = changed_files_total.clone();
}

/// Attach the lane owner row and its typed result claims to an output page.
/// The local command and fixture generator both pass an already projected
/// lane, so state, evidence and future fields stay aligned.
pub fn output_lane_fields(
    data: &mut Value,
    lane: &provenant_records::wire::LaneRow,
    attempt: AttemptId,
) {
    let row = json!(lane);
    if let (Some(data), Some(row)) = (data.as_object_mut(), row.as_object()) {
        data.remove("settlement_reason");
        if let Some(grants) = data.get_mut("grants").and_then(Value::as_object_mut) {
            for field in ["write_grants", "read_only_writable"] {
                if let Some(value) = row.get(field) {
                    grants.insert(field.into(), value.clone());
                }
            }
            if let Some(route) = row.get("applied_route") {
                for field in ["user_skills", "lane_access"] {
                    grants.insert(
                        field.into(),
                        route.get(field).cloned().unwrap_or(Value::Null),
                    );
                }
            }
        }
        data.extend(row.clone());
    }
    data["result_projection"] =
        serde_json::to_value(output_result_projection(data, attempt)).unwrap_or(Value::Null);
}

/// Result evidence from the output owner, with provider claims kept separate.
pub fn output_result_projection(
    data: &Value,
    attempt: AttemptId,
) -> provenant_records::lane_contract::ResultProjection {
    use provenant_records::lane_contract::{
        Coverage, GitFacts, HostFacts, LaneClaims, Observed, ResultProjection,
    };

    let commands = provenant_run::observed_commands();
    let git_facts = GitFacts {
        base: data["writer_base"].as_str().map(str::to_owned),
        head: data["writer_head"]
            .as_str()
            .or_else(|| data["head"].as_str())
            .map(str::to_owned),
        commits: data["stop_work"]["ahead"]
            .as_u64()
            .into_iter()
            .map(|ahead| format!("{ahead} commit(s)"))
            .collect(),
        diffstat: data["changed_files_total"]
            .as_u64()
            .map(|total| format!("{total} changed file(s)")),
        shared_worktree: false,
        extra: Default::default(),
    };
    let git_complete = git_facts.base.is_some() || git_facts.head.is_some();
    let git = Observed {
        source: "host-observation".into(),
        completeness: if git_complete {
            Coverage::Partial
        } else {
            Coverage::Unknown
        },
        value: Some(git_facts),
        extra: Default::default(),
    };
    let ci: Observed<Value> = Observed {
        source: "unavailable".into(),
        completeness: Coverage::Unknown,
        value: None,
        extra: Default::default(),
    };
    let scope: Observed<provenant_records::handoff::WriteScopeReport> = Observed {
        source: "unavailable".into(),
        completeness: Coverage::Unknown,
        value: None,
        extra: Default::default(),
    };
    let handoff = serde_json::from_value::<provenant_records::handoff::Handoff>(
        data["handoff"]["Ok"].clone(),
    )
    .ok();
    let verify = handoff
        .as_ref()
        .map(|handoff| handoff.verify.clone())
        .or_else(|| {
            let verify = &data["selected"]["verify"];
            verify
                .is_array()
                .then(|| serde_json::from_value(verify.clone()).ok())
                .flatten()
        });
    ResultProjection::new(
        provenant_records::RecordHeader::V1,
        attempt,
        HostFacts {
            commands,
            git,
            ci,
            scope,
            extra: Default::default(),
        },
        LaneClaims {
            handoff,
            verify,
            extra: Default::default(),
        },
    )
}

/// The output envelope's next action from its final projected state and the
/// actual command target. This is shared by local, peer and fixture paths.
pub fn output_next_step(
    data: &Value,
    target: Option<&crate::Target>,
) -> Option<provenant_records::command_result::NextStep> {
    use provenant_records::command_result::NextStep;
    let target = target?;
    let state = data["state"].as_str().unwrap_or_default();
    if data["eof"] == json!(false) {
        let offset = data["next_offset"].as_u64()?;
        let part = data["part"].as_str().unwrap_or("result");
        Some(NextStep::from_args(
            &[
                "provenant",
                "output",
                &target.to_string(),
                "--part",
                part,
                "--offset",
                &offset.to_string(),
            ],
            "read the next page of this output",
        ))
    } else if matches!(state, "queued" | "starting" | "running" | "settling") {
        let handle = target.to_string();
        Some(NextStep::from_args(
            &["provenant", "wait", &handle],
            "wait for this work to settle",
        ))
    } else {
        None
    }
}

/// Reviewer receipt object and human text from one settled typed attempt.
/// Both the output command and generated fixtures use this projection so a
/// receipt tail exercises the same wire shape the command actually returns.
pub fn output_receipt(
    record: &provenant_records::run::AttemptRecord,
    writer: bool,
    catalogue: Option<&provenant_routing::Catalogue>,
) -> Result<(Value, String), provenant_run::RunError> {
    let route = record.applied_route.as_ref();
    let adapter = route
        .and_then(|route| route.provider.as_deref())
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown");
    let model = route
        .and_then(|route| route.model.as_deref())
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown");
    let qualified_model = route
        .and_then(|route| output_receipt_model(route.provider.as_deref(), route.model.as_deref()));
    let (effort, effort_source) = output_receipt_effort(record);
    let family = output_receipt_family(catalogue, qualified_model.as_deref());
    let resolved_from = output_receipt_resolution(record);
    let reviewer = provenant_run::fact(record.clone())?.reviewer;
    let reviewer_model = record
        .observed_route
        .as_ref()
        .and_then(|route| output_receipt_model(route.provider.as_deref(), route.model.as_deref()));
    let reviewer_family = output_receipt_family(catalogue, reviewer_model.as_deref());
    let provenance = reviewer
        .as_ref()
        .map_or("unknown", |reviewer| match reviewer.provenance {
            provenant_records::ReviewerProvenance::Observed => "observed",
            provenant_records::ReviewerProvenance::ChairAttested => "chair_attested",
            provenant_records::ReviewerProvenance::Unknown => "unknown",
        });
    let actor = reviewer.as_ref().map_or_else(
        || "unknown".into(),
        |reviewer| reviewer.actor_id.to_string(),
    );
    let reviewer_model = reviewer
        .as_ref()
        .and_then(|reviewer| reviewer.model.as_deref())
        .unwrap_or("unknown");
    let reviewer_family = if reviewer.is_some() {
        reviewer_family
    } else {
        "unknown".into()
    };
    let role = if writer { "writer" } else { "reviewer" };
    let label = if writer { "Writer" } else { "Reviewer" };
    let qualified_model = qualified_model.as_deref().unwrap_or("unknown");
    let line = format!(
        "{label}: {qualified_model}@{effort} ({family}; resolved from {resolved_from}; effort source {effort_source}), attempt {} {}",
        record.key, record.attempt_id
    );
    let attribution = format!(
        "Reviewer provenance: {provenance}, actor {actor}, model {reviewer_model}, family {reviewer_family}"
    );
    let data = json!({
        "key": record.key,
        "attempt_id": record.attempt_id,
        "adapter": adapter,
        "model": model,
        "effort": effort,
        "effort_source": effort_source,
        "family": family,
        "role": role,
        "resolved_from": resolved_from,
        "receipt": line,
        "reviewer": {
            "provenance": provenance,
            "actor_id": actor,
            "model": reviewer_model,
            "family": reviewer_family
        },
        "reviewer_provenance": attribution
    });
    let text = format!("{line}\n{attribution}\n");
    Ok((data, text))
}

fn output_receipt_model(adapter: Option<&str>, model: Option<&str>) -> Option<String> {
    let adapter = adapter.filter(|value| !value.is_empty())?;
    let model = model.filter(|value| !value.is_empty())?;
    Some(if model.starts_with(&format!("{adapter}/")) {
        model.to_owned()
    } else {
        format!("{adapter}/{model}")
    })
}

fn output_receipt_family(
    catalogue: Option<&provenant_routing::Catalogue>,
    model: Option<&str>,
) -> String {
    catalogue
        .and_then(|catalogue| {
            provenant_routing::ModelId::parse(model?)
                .ok()
                .and_then(|id| catalogue.model(&id).and_then(|entry| entry.family.clone()))
        })
        .unwrap_or_else(|| "unknown".into())
}

fn output_receipt_effort(record: &provenant_records::run::AttemptRecord) -> (String, String) {
    let route = record.applied_route.as_ref();
    if let Some(effort) = route
        .and_then(|route| route.effort.as_deref())
        .filter(|value| !value.is_empty())
    {
        return (effort.to_owned(), "applied_route.effort".into());
    }
    if let Some(warning) = route.and_then(|route| route.effort_warning.as_ref()) {
        let source = format!("effort_warning:{}", warning.source);
        if let Some(applied) = warning.applied.as_deref().filter(|value| !value.is_empty()) {
            return (applied.to_owned(), source);
        }
        return ("none".into(), source);
    }
    ("unknown".into(), "unknown".into())
}

fn output_receipt_resolution(record: &provenant_records::run::AttemptRecord) -> String {
    let Some(route) = record.applied_route.as_ref() else {
        return "unknown".into();
    };
    let Some(applied) = output_receipt_model(route.provider.as_deref(), route.model.as_deref())
    else {
        return "unknown".into();
    };
    if let Some(pool) = &route.pool_selection {
        return if pool.pick == applied {
            format!("pool {}", pool.pool)
        } else {
            "unknown".into()
        };
    }
    match route.source.as_deref() {
        Some("pin") => return "explicit".into(),
        Some("pool") => return "pool".into(),
        Some("named_route") => {
            return match &record.requested_route {
                provenant_records::run::RequestedRoute::Named { name, .. }
                | provenant_records::run::RequestedRoute::Review { name, .. } => {
                    format!("route {name}")
                }
                _ => "route".into(),
            };
        }
        Some("fallback") => return "fallback".into(),
        Some("continuation") => return "continuation".into(),
        _ => {}
    }
    let provenant_records::run::RequestedRoute::Model {
        provider, model, ..
    } = &record.requested_route
    else {
        return "unknown".into();
    };
    let Some(requested) = output_receipt_model(Some(provider), Some(model)) else {
        return "unknown".into();
    };
    if applied == requested {
        "explicit".into()
    } else {
        "unknown".into()
    }
}

/// Optional fields added by the output owner after reading an artifact. Keeping
/// their wire shape here lets the fixture generator exercise the same branches.
#[derive(Default)]
pub struct OutputExtras<'a> {
    pub budget_usage: Option<&'a Value>,
    pub stop_work: Option<&'a Value>,
    pub payload_evicted: bool,
    pub planner_fallback: Option<&'a str>,
    /// Outer `None` omits the field; `Some(None)` preserves a reported null.
    pub provider_error: Option<Option<&'a str>>,
}

/// Add the optional output metadata present for one loaded attempt state.
pub fn output_extras(data: &mut Value, extras: OutputExtras<'_>) {
    if let Some(budget_usage) = extras.budget_usage {
        data["budget_usage"] = budget_usage.clone();
    }
    if let Some(stop_work) = extras.stop_work {
        data["stop_work"] = stop_work.clone();
    }
    if extras.payload_evicted {
        data["payload"] = json!("evicted");
    }
    if let Some(reason) = extras.planner_fallback {
        data["planner_fallback"] = json!({"stage":"static", "reason":reason});
    }
    if let Some(provider_error) = extras.provider_error {
        data["provider_error"] = json!(provider_error);
    }
}

#[derive(Clone, Copy)]
pub enum OutputTail {
    Bytes(u64),
    Lines(u64),
}

/// Select the human text for a receipt tail using the same byte boundary as
/// the output command. Receipt metadata and its displayed tail stay aligned.
pub fn output_tail_text(text: &str, tail: OutputTail) -> String {
    let bytes = text.as_bytes();
    let start = match tail {
        OutputTail::Bytes(max_bytes) => crate::run::tail_start_bytes(bytes, max_bytes),
        OutputTail::Lines(lines) => crate::run::tail_start_lines(bytes, lines),
    };
    text[start..].to_owned()
}

pub struct OutputWrite<'a> {
    pub path: &'a Value,
    pub bytes: usize,
    pub final_result: bool,
}

#[derive(Default)]
pub struct OutputRenderFields<'a> {
    pub headed: bool,
    pub tail: Option<OutputTail>,
    pub tail_clamped: bool,
    pub tail_text: Option<&'a str>,
    pub written: Option<OutputWrite<'a>>,
}

pub struct OutputProgress {
    pub state: String,
    pub pending: bool,
}

/// Project fields added by the final output rendering stage.
pub fn output_render_fields(
    data: &mut Value,
    fields: OutputRenderFields<'_>,
) -> Option<OutputProgress> {
    let progress = if fields.headed
        && let Some(state) = data["state"].as_str()
        && matches!(state, "queued" | "starting" | "running" | "settling")
    {
        let state = state.to_owned();
        let pending = data["length"].as_u64().unwrap_or(0) == 0;
        data["status"] = json!(state);
        data["result"] = json!(if pending { "pending" } else { "partial" });
        Some(OutputProgress { state, pending })
    } else {
        None
    };

    if let Some(tail) = fields.tail {
        match tail {
            OutputTail::Bytes(bytes) => data["tail"] = json!(bytes),
            OutputTail::Lines(lines) => data["tail_lines"] = json!(lines),
        }
        data["tail_clamped"] = json!(fields.tail_clamped);
    }
    if let Some(text) = fields.tail_text {
        data["tail_text"] = json!(text);
    }
    if let Some(written) = fields.written {
        data["to"] = written.path.clone();
        data["written"] = json!(written.bytes);
        if written.final_result {
            data["bytes"] = json!(
                provenant_fabric_runtime::coordination::facade::OutputBytes::from_bytes(Vec::new(),)
            );
        }
    }
    progress
}

/// `console projects` data from the registered local projects.
pub fn projects<T: serde::Serialize>(registered: &[T]) -> Value {
    json!({
        "schema_version": 2,
        "projects": registered,
        "coverage": {"scope": "local"},
    })
}

/// `console overview`. `running` and `attention` are the counts the owner
/// derived while projecting lanes; the counts are hidden when the owner could
/// not read the corresponding store, so callers pass the coverage strings too.
pub fn overview(
    needs_you: &[Value],
    lanes: &[Value],
    running: usize,
    attention: usize,
    inbox: &str,
    runs: &str,
) -> Value {
    let known = |coverage: &str| coverage != "unavailable" && coverage != "incompatible";
    json!({
        "schema_version": 2,
        "needs_you": needs_you,
        "lanes": lanes,
        "counts": {
            "needs_you": known(inbox).then_some(needs_you.len()),
            "running": known(runs).then_some(running),
            "attention": known(runs).then_some(attention),
        },
        // Overview reads only this host's stores; it never traverses nested runs.
        "coverage": {
            "scope": "local", "inbox": inbox, "runs": runs, "nested_runs": "unavailable"
        },
    })
}

/// The `console sessions` display-line precedence: the chair's structured
/// `now` wins, then the line derived from the checkpoint, then unavailable.
pub fn session_line(structured: Option<&str>, derived: Option<SummaryLine>) -> (Value, Value) {
    let (line, source) = match structured.filter(|now| !now.trim().is_empty()) {
        Some(now) => (json!(now), SummarySource::Structured),
        None => match derived {
            Some(line) => (json!(line.text), line.source),
            None => (Value::Null, SummarySource::Unavailable),
        },
    };
    (line, serde_json::to_value(source).unwrap_or(Value::Null))
}

/// One `console sessions` row. The display line is the structured `now` when
/// it is present; otherwise the line derived from the checkpoint markdown.
pub fn session_row(
    record: &Value,
    consistency: &Value,
    summary_line: &Value,
    summary_source: &Value,
) -> Value {
    json!({
        "record": record,
        "consistency": consistency,
        "role": "chair",
        "evidence": "checkpoint_save",
        "summary_line": summary_line,
        "summary_source": summary_source,
    })
}

/// `console sessions` data: a bounded page of rows over one scan snapshot.
pub fn sessions(
    rows: &[Value],
    snapshot: &str,
    next_cursor: Option<&str>,
    truncated: bool,
    scan_truncated: bool,
) -> Value {
    json!({
        "schema_version": 2,
        "snapshot": snapshot,
        "sessions": rows,
        "next_cursor": next_cursor,
        "truncated": truncated,
        "scan_truncated": scan_truncated,
        "coverage": {"scope": "local"},
    })
}

/// Stable snapshot identity for a session catalogue page, shared by the owner
/// and fixture generator so cursor semantics are part of the fixture contract.
pub fn session_snapshot<T: Serialize>(
    selected: Option<ProjectId>,
    session: Option<&str>,
    limit: u64,
    scan_truncated: bool,
    rows: &[T],
) -> Result<String, String> {
    CanonicalV1::record_digest(
        "console-sessions",
        1,
        &(selected, session, limit, scan_truncated, rows),
    )
    .map(|digest| digest.to_string())
    .map_err(|error| error.to_string())
}

/// `console changes`: compare the current owner generations with an optional
/// baseline. The caller parses and validates `--since`; this only compares.
pub fn changes(current: &Value, previous: Option<&Value>) -> Value {
    let mut changed = serde_json::Map::new();
    for name in ["inbox", "board", "runs", "chair", "quota", "configuration"] {
        let now = current[name].as_u64();
        let before = previous.and_then(|value| value[name].as_u64());
        let same_epoch = current["epoch"].as_u64().is_some()
            && previous.is_some_and(|value| current["epoch"] == value["epoch"]);
        let comparison = match (now, before) {
            (Some(now), Some(before)) if same_epoch && now >= before => json!(now > before),
            _ => Value::Null,
        };
        changed.insert(name.to_owned(), comparison);
    }
    json!({"schema_version": 2, "generations": current, "changed": changed})
}

/// `console board` data from one bounded page of projected lanes.
#[allow(clippy::too_many_arguments)]
pub fn board(
    snapshot: &str,
    high_water: u64,
    generation: &str,
    observed_at: &Value,
    filters: &Value,
    lanes: &[Value],
    next_cursor: Option<&str>,
    host: &Value,
    runs: &str,
    chairs: &str,
    observations: &str,
) -> Value {
    json!({
        "schema_version": 3,
        "snapshot": snapshot,
        "high_water": high_water,
        "generation": generation,
        "observed_at": observed_at,
        "filters": filters,
        "lanes": lanes,
        "next_cursor": next_cursor,
        "truncated": next_cursor.is_some(),
        "coverage": {
            "scope": "local", "host": host, "runs": runs, "chairs": chairs,
            "observations": observations, "nested_runs": "unavailable"
        },
    })
}

/// Stable cursor binding shared with the board owner and fixture generator.
pub fn board_snapshot(high_water: u64, filters: &Value) -> Result<String, String> {
    CanonicalV1::record_digest("console-board", 2, &(high_water, filters))
        .map(|digest| digest.to_string())
        .map_err(|error| error.to_string())
}

/// Filter identity embedded in a board response and used to bind its cursor.
pub fn board_filters(
    state_root: &Value,
    host: &Value,
    project_id: &Value,
    all: bool,
    limit: u64,
) -> Value {
    json!({"state_root":state_root,"host":host,"project_id":project_id,"all":all,"limit":limit})
}

/// `console tree` data from a bounded snapshot of projected nodes and edges.
#[allow(clippy::too_many_arguments)]
pub fn tree(
    nodes: &[Value],
    edges: &[Value],
    totals: &Value,
    conflicts: &[Value],
    truncated: bool,
    children_absent: u64,
    unfetched: &[Value],
    host: &Value,
    root_coverage: &str,
    partial: bool,
    max_depth: u64,
    max_nodes: u64,
) -> Value {
    json!({
        "schema_version": 1,
        "nodes": nodes,
        "edges": edges,
        "totals": totals,
        "identity_conflicts": conflicts,
        "truncated": truncated,
        "children_absent": children_absent,
        "unfetched_children": unfetched,
        "coverage": {
            "scope": "local", "host": host, "runs": root_coverage,
            "nested_runs": if partial { "partial" } else { "complete" }
        },
        "limits": {"depth": max_depth, "nodes": max_nodes},
    })
}

/// `console detail` data for one attempt.
#[allow(clippy::too_many_arguments)]
pub fn detail(
    lane_id: &Value,
    status: &str,
    requested_route: &Value,
    resolved_route: &Value,
    applied_route: &Value,
    observed_route: &Value,
    usage: &Value,
    usage_known: &[&str],
    usage_missing: &[&str],
    trace: &Value,
    timings: &Value,
    artifacts: &[Value],
    delivery: &Value,
    linked_inbox_requests: &[Value],
    nested: &str,
    inbox: &str,
) -> Value {
    json!({
        "schema_version": 1,
        "lane_id": lane_id,
        "status": status,
        "requested_route": requested_route,
        "resolved_route": resolved_route,
        "applied_route": applied_route,
        "observed_route": observed_route,
        "usage": usage,
        "usage_coverage": {"known": usage_known, "missing": usage_missing},
        "trace": trace,
        "timings": timings,
        "artifacts": artifacts,
        "delivery": delivery,
        "linked_inbox_requests": linked_inbox_requests,
        "coverage": {
            "nested": nested, "peer": "not_searched", "linked_inbox_requests": inbox
        },
    })
}

/// Lane identity before the command has resolved a project or work key.
pub fn detail_lookup_lane_id(host: &Value, attempt_id: &Value) -> Value {
    json!({"host":host,"attempt_id":attempt_id})
}

/// `console detail` data when the target is not present locally.
pub fn detail_missing(lane_id: &Value, nested: &str) -> Value {
    json!({
        "schema_version": 1,
        "lane_id": lane_id,
        "status": "missing",
        "coverage": {"nested": nested, "peer": "not_searched"},
    })
}

/// `console detail` data when its local record or tree lookup is unavailable.
/// Coverage is absent when the command did not attempt a nested tree search.
pub fn detail_unavailable(
    host: &Value,
    attempt_id: &Value,
    reason: &str,
    nested_coverage: Option<&str>,
) -> Value {
    let mut data = json!({
        "schema_version": 1,
        "lane_id": detail_lookup_lane_id(host, attempt_id),
        "status": "unavailable",
        "reason": reason,
    });
    if let Some(nested) = nested_coverage {
        data["coverage"] = json!({"nested": nested, "peer": "not_searched"});
    }
    data
}

/// One artifact-read failure in a bounded `console search` scan.
pub fn search_artifact_error(attempt_id: &Value, reason: &str) -> Value {
    json!({"attempt_id":attempt_id,"reason":reason})
}

/// `console search` data from one bounded page of matches.
#[allow(clippy::too_many_arguments)]
pub fn search(
    snapshot: &str,
    query: &str,
    part: &str,
    matches: &[Value],
    next_cursor: Option<&str>,
    truncated: bool,
    scanned: usize,
    scan_bytes: u64,
    artifact_errors: &[Value],
    file_limit: usize,
    per_artifact_bytes: u64,
    total_bytes: u64,
) -> Value {
    json!({
        "schema_version": 1,
        "snapshot": snapshot,
        "query": query,
        "part": part,
        "matches": matches,
        "next_cursor": next_cursor,
        "truncated": truncated,
        "coverage": {
            "scope": "local", "peers": "not_searched", "files_scanned": scanned,
            "bytes_scanned": scan_bytes, "file_limit": file_limit,
            "per_artifact_bytes": per_artifact_bytes,
            "total_bytes": total_bytes,
            "artifact_errors": artifact_errors,
            "restart": "If the cursor is invalid or stale, restart with the same query and filters without --cursor."
        },
    })
}

/// Stable identity for a search scan, shared with cursor validation.
pub fn search_snapshot(
    query: &str,
    part: &str,
    session: Option<&str>,
    rows: &[(ProjectId, String, u64)],
    scanned: usize,
    scan_bytes: u64,
    truncated: bool,
) -> Result<String, String> {
    CanonicalV1::record_digest(
        "console-search",
        1,
        &(query, part, session, rows, scanned, scan_bytes, truncated),
    )
    .map(|digest| digest.to_string())
    .map_err(|error| error.to_string())
}

/// `output` data for one artifact byte range. The attempt identity and state
/// are already projected by the run owner; this assembles the response.
#[allow(clippy::too_many_arguments)]
pub fn output(
    attempt_id: &str,
    state: &str,
    reason: Option<Value>,
    settlement_reason: Option<Value>,
    part: &str,
    offset: u64,
    next_offset: u64,
    length: Option<u64>,
    eof: bool,
    clipped: Option<bool>,
    complete: Option<bool>,
    digest: &Value,
    bytes: &Value,
) -> Value {
    let mut data = json!({
        "attempt_id": attempt_id,
        "state": state,
        "part": part,
        "offset": offset,
        "next_offset": next_offset,
        "length": length,
        "eof": eof,
        "clipped": clipped,
        "complete": complete,
        "digest": digest,
        "bytes": bytes,
    });
    if let Some(reason) = reason {
        data["reason"] = reason;
    }
    if let Some(settlement_reason) = settlement_reason {
        data["settlement_reason"] = settlement_reason;
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overview_activity_preserves_unsettled_fallback_and_excludes_terminal_outcomes() {
        let state =
            |value| serde_json::from_value::<AttemptState>(value).expect("synthetic attempt state");
        for value in [
            json!({"state":"queued"}),
            json!({"state":"starting"}),
            json!({"state":"running"}),
            json!({"state":"settling", "publication_pending":true}),
        ] {
            assert!(overview_activity(None, &state(value)).0);
        }
        for outcome in ["succeeded", "failed", "timed_out", "cancelled"] {
            let terminal = state(json!({
                "state":"terminal", "outcome":outcome, "exit_code":1,
                "finished_at":"2026-11-02T02:00:00.000000Z", "reason":null
            }));
            assert_eq!(overview_activity(None, &terminal), (false, false));
        }
        let running = state(json!({"state":"running"}));
        assert_eq!(
            overview_activity(Some(LanePresence::Live), &running),
            (true, false)
        );
        assert_eq!(
            overview_activity(Some(LanePresence::Unknown), &running),
            (false, true)
        );
    }

    #[test]
    fn board_lane_projection_keeps_owner_fields_and_coverage_together() {
        let lane_id = board_lane_id(
            &Value::Null,
            &json!({"graph":"fixture"}),
            &json!("attempt-1"),
        );
        let lane = board_lane(BoardLaneInput {
            lane_id: &lane_id,
            chair: &json!({"session_id":"session-1"}),
            operation: &json!({"name":"review"}),
            attempt: &json!({"state":"running"}),
            wait: &Value::Null,
            attention: true,
            known: &["input"],
            missing: &["output"],
            observation: "complete",
        });

        assert_eq!(lane["lane_id"]["attempt_id"], "attempt-1");
        assert_eq!(lane["lane_id"]["key"]["graph"], "fixture");
        assert_eq!(lane["metadata"]["operation"]["name"], "review");
        assert_eq!(lane["metadata"]["attempt"]["state"], "running");
        assert_eq!(lane["usage_coverage"]["known"][0], "input");
        assert_eq!(lane["usage_coverage"]["missing"][0], "output");
        assert_eq!(lane["observation_coverage"], "complete");
    }

    #[test]
    fn overview_lane_accepts_borrowed_labels_and_preserves_missing_title() {
        let lane = overview_lane(
            &json!("01010101010101010101010101010101"),
            "provenant",
            &json!({"graph":"fixture","node":"task","attempt":1}),
            &Option::<String>::None,
            &json!({"attempt_id":"fixture","state":"queued"}),
            &Value::Null,
            false,
        );

        assert_eq!(lane["task_label"], Value::Null);
    }

    #[test]
    fn detail_usage_coverage_projects_the_effective_token_buckets() {
        let usage = provenant_run::UsageRecord {
            tokens: provenant_records::usage::Tokens {
                input: Some(12),
                ..Default::default()
            },
            ..Default::default()
        };

        let (value, known, missing) = detail_usage(&usage).expect("typed usage serializes");

        assert_eq!(value["tokens"]["input"], 12);
        assert_eq!(known, ["input"]);
        assert_eq!(
            missing,
            ["cache_read", "cache_write", "output", "reasoning"]
        );
    }

    #[test]
    fn detail_unavailable_omits_unattempted_coverage_and_keeps_error_coverage() {
        let remote = detail_unavailable(
            &json!("remote-host"),
            &json!("attempt-1"),
            "remote_detail_requires_host_read",
            None,
        );
        assert_eq!(
            remote,
            json!({
                "schema_version":1,
                "lane_id":{"host":"remote-host","attempt_id":"attempt-1"},
                "status":"unavailable",
                "reason":"remote_detail_requires_host_read"
            })
        );

        let tree_error = detail_unavailable(
            &Value::Null,
            &json!("attempt-2"),
            "tree read failed",
            Some("unavailable"),
        );
        assert_eq!(
            tree_error["coverage"],
            json!({"nested":"unavailable","peer":"not_searched"})
        );

        let missing = detail_missing(
            &detail_lookup_lane_id(&Value::Null, &json!("attempt-3")),
            "bounded_tree_search",
        );
        assert_eq!(
            missing,
            json!({
                "schema_version":1,
                "lane_id":{"host":null,"attempt_id":"attempt-3"},
                "status":"missing",
                "coverage":{"nested":"bounded_tree_search","peer":"not_searched"}
            })
        );
    }

    #[test]
    fn search_artifact_error_projection_keeps_attempt_and_reason() {
        assert_eq!(
            search_artifact_error(&json!("attempt-1"), "fixture read error"),
            json!({"attempt_id":"attempt-1","reason":"fixture read error"})
        );
    }

    #[test]
    fn output_extras_preserve_optional_fields_and_null_provider_error() {
        let budget = json!({"unit":"fresh_tokens","counted":742});
        let stop_work = json!({"trigger":"explicit_cancellation","push":"not_granted"});
        let mut data = json!({"attempt_id":"attempt-1"});

        output_extras(
            &mut data,
            OutputExtras {
                budget_usage: Some(&budget),
                stop_work: Some(&stop_work),
                payload_evicted: true,
                planner_fallback: Some("planner route failed"),
                provider_error: Some(None),
            },
        );

        assert_eq!(
            data,
            json!({
                "attempt_id":"attempt-1",
                "budget_usage":{"unit":"fresh_tokens","counted":742},
                "stop_work":{"trigger":"explicit_cancellation","push":"not_granted"},
                "payload":"evicted",
                "planner_fallback":{"stage":"static","reason":"planner route failed"},
                "provider_error":null
            })
        );

        let mut absent = json!({"attempt_id":"attempt-2"});
        output_extras(&mut absent, OutputExtras::default());
        assert_eq!(absent, json!({"attempt_id":"attempt-2"}));
    }
}
