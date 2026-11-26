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

use provenant_workflow::session::{SummaryLine, SummarySource};
use serde_json::{Value, json};

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
