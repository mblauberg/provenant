//! The planned shared lane projection for `status` and `lanes`.
use super::schema::{
    any, array, boolean, choices, integer, nullable, number, object, open_object, string, unsigned,
};
use serde::Serialize;
use serde_json::Value;

/// One lane (attempt) in the planned CLI projection. Members the run records own
/// (routes, grants, delivery, continuation, timestamps) stay `Value`: this
/// crate publishes their position and nullability, and the run records stay
/// the single source of their inner shape.
///
/// Optional members (`title`, `requested_priority`, `provider_seconds`, `handoff`, `provider_error`) are
/// omitted when absent; every other member is always present, null when unknown.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LaneRow {
    pub run_id: String,
    pub task_id: String,
    pub attempt_id: String,
    pub attempt_number: u64,
    pub retry_of: Option<String>,
    pub superseded_by: Option<String>,
    pub cause: Option<crate::run::SettlementReason>,
    /// The state name (`queued`, `running`, `terminal`, ...). Settlement owns
    /// the set, so the schema does not enumerate it.
    pub state: String,
    pub exit_code: Option<i64>,
    /// The terminal reason, or the waiting note of a queued lane.
    pub reason: Option<String>,
    /// The settled record's own reason; null until the lane is terminal.
    pub settlement: Option<LaneSettlement>,
    pub lane_class: Option<String>,
    pub waiting_for: Option<String>,
    pub requested_route: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_route: Option<Value>,
    pub applied_route: Value,
    pub skipped_routes: Value,
    pub account: Value,
    pub observed_route: Value,
    pub queued_at: Value,
    pub started_at: Value,
    pub priority: Option<i64>,
    pub finished_at: Value,
    pub last_output_at: Value,
    pub runtime_deadline: Value,
    pub overdue: bool,
    pub stale: bool,
    pub partial_signals: Value,
    pub delivery_id: Option<String>,
    pub write_grants: Value,
    pub read_only_writable: Value,
    pub git_grants_digest: Option<String>,
    pub head: Option<String>,
    pub protected_paths: Value,
    pub delivery: Value,
    pub continuation: Value,
    pub cache_read_share: Option<f64>,
    pub result_path: Value,
    pub transcript: Option<LaneTranscript>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Present only when it differs from `priority`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_priority: Option<i64>,
    /// MCP only: provider time in seconds. The MCP edge replaces the record's
    /// `provider_ms` with `provider_ms / 1000`, so a row carries one of the
    /// two, never both. The CLI emits neither.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handoff: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_error: Option<String>,
}

impl LaneRow {
    /// Project the recorded state without rewriting missing history.
    pub fn state_and_reason(state: &crate::run::AttemptState) -> (String, Option<String>) {
        use crate::run::AttemptState;
        match state {
            AttemptState::Terminal {
                outcome, reason, ..
            } => (
                serde_json::to_value(outcome)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_default(),
                reason.clone(),
            ),
            other => (
                serde_json::to_value(other)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("state")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .unwrap_or_default(),
                None,
            ),
        }
    }
}

/// The settled record's reason.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LaneSettlement {
    pub reason: Option<String>,
}

/// A pointer to the provider's own transcript; never a copy.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LaneTranscript {
    pub adapter: String,
    pub session: String,
    pub path: Option<String>,
}

/// Planned `data` of `lanes` and `status`. Current status emits raw attempt
/// records and a numeric cursor; migration will use this string cursor.
/// Members after `next_cursor` are present only on the
/// command or path that produces them.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LaneList {
    pub lanes: Vec<LaneRow>,
    pub next_cursor: Option<String>,
    /// `lanes` only: live host capacity cooldowns; the quota owner defines
    /// their shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capacity_cooldowns: Option<Vec<Value>>,
    /// `status` only: paths a writer may change unreviewed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_exempt: Option<Value>,
    /// Waits only: whether the awaited condition held.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition_met: Option<bool>,
    /// Waits only: false when a silent provider ended the wait early.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timed_out: Option<bool>,
    /// Waits only: why the wait ended without the condition.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Waits only: already-settled lanes the wait ignored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignored_settled: Option<u64>,
}

/// Schema of one [`LaneRow`].
pub fn lane_row_schema() -> Value {
    let a = any();
    object(
        &[
            ("run_id", string()),
            ("task_id", string()),
            ("attempt_id", string()),
            ("attempt_number", unsigned()),
            ("retry_of", nullable(string())),
            ("superseded_by", nullable(string())),
            (
                "cause",
                nullable(choices(&[
                    "completed",
                    "handoff_missing",
                    "declared_stop",
                    "provider_stop",
                    "uncommitted_work",
                    "pre_launch_refusal",
                    "cancelled",
                    "lost",
                    "unsettled",
                    "failed",
                ])),
            ),
            ("state", string()),
            ("exit_code", nullable(integer())),
            ("reason", nullable(string())),
            (
                "settlement",
                nullable(object(&[("reason", nullable(string()))], &[])),
            ),
            ("lane_class", nullable(string())),
            ("waiting_for", nullable(string())),
            ("requested_route", a.clone()),
            ("applied_route", a.clone()),
            ("skipped_routes", a.clone()),
            ("account", a.clone()),
            ("observed_route", a.clone()),
            ("queued_at", a.clone()),
            ("started_at", a.clone()),
            ("priority", nullable(integer())),
            ("finished_at", a.clone()),
            ("last_output_at", a.clone()),
            ("runtime_deadline", a.clone()),
            ("overdue", boolean()),
            ("stale", boolean()),
            ("partial_signals", a.clone()),
            ("delivery_id", nullable(string())),
            ("write_grants", a.clone()),
            ("read_only_writable", a.clone()),
            ("git_grants_digest", nullable(string())),
            ("head", nullable(string())),
            ("protected_paths", a.clone()),
            ("delivery", a.clone()),
            ("continuation", a.clone()),
            ("cache_read_share", nullable(number())),
            ("result_path", a),
            (
                "transcript",
                nullable(object(
                    &[
                        ("adapter", string()),
                        ("session", string()),
                        ("path", nullable(string())),
                    ],
                    &[],
                )),
            ),
        ],
        &[
            ("resolved_route", any()),
            ("title", string()),
            ("requested_priority", integer()),
            ("provider_seconds", nullable(number())),
            ("handoff", any()),
            ("provider_error", string()),
        ],
    )
}

/// Schema of [`LaneList`].
pub fn lane_list_schema() -> Value {
    object(
        &[
            ("lanes", array(lane_row_schema())),
            ("next_cursor", nullable(string())),
        ],
        &[
            ("capacity_cooldowns", array(open_object())),
            ("review_exempt", any()),
            ("condition_met", boolean()),
            ("timed_out", boolean()),
            ("reason", string()),
            ("ignored_settled", unsigned()),
        ],
    )
}

/// MCP's `provider_seconds`: the record's whole milliseconds as fractional
/// seconds. The single definition of the conversion `fabric-runtime` applies
/// when it encodes an attempt, so the two cannot disagree.
pub fn provider_seconds(provider_ms: Option<u64>) -> Option<f64> {
    provider_ms.map(|millis| millis as f64 / 1000.0)
}
