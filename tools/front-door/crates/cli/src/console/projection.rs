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

use provenant_core::ProjectId;
use provenant_records::CanonicalV1;
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

/// One board lane, including the owner operation metadata and census coverage.
pub fn board_lane(
    lane_id: &Value,
    chair: &Value,
    operation: &Value,
    attempt: &Value,
    wait: &Value,
    attention: bool,
    known: &[&str],
    missing: &[&str],
    observation: &str,
) -> Value {
    json!({"lane_id":lane_id,"chair":chair,"metadata":{"operation":operation,"attempt":attempt},
        "wait":wait,"attention":attention,"usage_coverage":{"known":known,"missing":missing},
        "observation_coverage":observation})
}

/// One tree node with all derived display fields kept beside the source record.
pub fn tree_node(
    lane_id: &Value,
    depth: u64,
    store_parent_attempt: &Value,
    store_path: Option<&str>,
    usage: &Value,
    parents: &Value,
    operation: &Value,
    attempt: &Value,
    children: &[Value],
) -> Value {
    let mut node = json!({"lane_id":lane_id,"depth":depth,
        "store_parent_attempt":store_parent_attempt,"usage":usage,"parents":parents,
        "metadata":{"operation":operation,"attempt":attempt},"children":children,
        "status":attempt["state"],"title":attempt["title"]});
    if let Some(path) = store_path {
        node["store_path"] = json!(path);
    }
    node
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

/// `console detail` data when the target is not present locally.
pub fn detail_missing(lane_id: &Value, nested: &str) -> Value {
    json!({
        "schema_version": 1,
        "lane_id": lane_id,
        "status": "missing",
        "coverage": {"nested": nested, "peer": "not_searched"},
    })
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

#[cfg(test)]
mod tests {
    use super::*;

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
}

/// `output` data for one artifact byte range. The attempt identity and state
/// are already projected by the run owner; this assembles the response.
#[allow(clippy::too_many_arguments)]
pub fn output(
    attempt_id: &str,
    state: &str,
    reason: &Value,
    settlement_reason: &Value,
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
    json!({
        "attempt_id": attempt_id,
        "state": state,
        "reason": reason,
        "settlement_reason": settlement_reason,
        "part": part,
        "offset": offset,
        "next_offset": next_offset,
        "length": length,
        "eof": eof,
        "clipped": clipped,
        "complete": complete,
        "digest": digest,
        "bytes": bytes,
    })
}
