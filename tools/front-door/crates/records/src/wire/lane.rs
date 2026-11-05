//! The planned shared lane projection for `status` and `lanes`.
use super::schema::{
    any, array, boolean, choices, integer, nullable, number, object, open_object, string, unsigned,
};
use serde::Serialize;
use serde_json::Value;

// A field and its schema are declared together so generation cannot drift.
macro_rules! lane_row {
    (required { $( $name:ident : $ty:ty => $schema:expr; )* } optional { $( $opt:ident : $oty:ty => $oschema:expr; )* }) => {
        /// One projected lane (attempt).
        #[derive(Debug, Clone, PartialEq, Serialize)]
        pub struct LaneRow {
            $(pub $name: $ty,)*
            $(#[serde(skip_serializing_if = "Option::is_none")] pub $opt: Option<$oty>,)*
        }
        /// Schema generated from the same lane field declarations.
        pub fn lane_row_schema() -> Value {
            object(&[$((stringify!($name), $schema),)*], &[$((stringify!($opt), $oschema),)*])
        }
    }
}

lane_row! {
    required {
        run_id: String => string();
        task_id: String => string();
        attempt_id: String => string();
        attempt_number: u64 => unsigned();
        retry_of: Option<String> => nullable(string());
        superseded_by: Option<String> => nullable(string());
        cause: Option<crate::run::SettlementReason> => nullable(choices(&["completed", "handoff_missing", "declared_stop", "provider_stop", "uncommitted_work", "pre_launch_refusal", "cancelled", "lost", "unsettled", "failed"]));
        state: String => string();
        exit_code: Option<i64> => nullable(integer());
        reason: Option<String> => nullable(string());
        settlement: Option<LaneSettlement> => nullable(object(&[("reason", nullable(string()))], &[]));
        lane_class: Option<String> => nullable(string());
        waiting_for: Option<String> => nullable(string());
        requested_route: Value => any();
        applied_route: Value => any();
        skipped_routes: Value => any();
        account: Value => any();
        observed_route: Value => any();
        queued_at: Value => any();
        started_at: Value => any();
        priority: Option<i64> => nullable(integer());
        finished_at: Value => any();
        last_output_at: Value => any();
        runtime_deadline: Value => any();
        overdue: bool => boolean();
        stale: Option<bool> => nullable(boolean());
        partial_signals: Value => any();
        delivery_id: Option<String> => nullable(string());
        write_grants: Value => array(grant_schema());
        read_only_writable: Value => array(nullable(super::schema::worktree_path()));
        git_grants_digest: Option<String> => nullable(string());
        head: Option<String> => nullable(string());
        protected_paths: Value => array(super::schema::worktree_path());
        delivery: Value => any();
        continuation: Value => any();
        cache_read_share: Option<f64> => nullable(number());
        result_path: Value => nullable(super::schema::worktree_path());
        transcript: Option<LaneTranscript> => nullable(object(&[("adapter", string()), ("session", string()), ("path", nullable(super::schema::worktree_path()))], &[]));
    }
    optional {
        host: String => string();
        resolved_route: Value => any();
        title: String => string();
        requested_priority: i64 => integer();
        provider_seconds: f64 => nullable(number());
        handoff: Value => any();
        provider_error: String => string();
    }
}

pub(super) fn grant_schema() -> Value {
    let mut grant = open_object();
    grant["properties"] = serde_json::json!({"path": nullable(super::schema::worktree_path())});
    grant["required"] = serde_json::json!(["path"]);
    grant
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
    pub path: Option<crate::StructuredPathWire>,
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
