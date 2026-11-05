//! Shared lane projection for CLI and Fabric observations.
use super::schema::{
    any, array, boolean, choices, integer, nullable, number, object, open_object, string, unsigned,
};
use serde::Serialize;
use serde_json::{Value, json};

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
        overdue: Option<bool> => nullable(boolean());
        stale: Option<bool> => nullable(boolean());
        partial_signals: Value => any();
        delivery_id: Option<String> => nullable(string());
        write_grants: Value => nullable(array(grant_schema()));
        read_only_writable: Value => nullable(array(nullable(super::schema::worktree_path())));
        git_grants_digest: Option<String> => nullable(string());
        head: Option<String> => nullable(string());
        protected_paths: Value => nullable(array(super::schema::worktree_path()));
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

/// Facts observed at the edge; absent observations stay unavailable.
#[derive(Default)]
pub struct LaneProjectionFacts {
    pub superseded_by: Option<provenant_core::AttemptId>,
    pub waiting_for: Option<String>,
    pub lane_class: Option<String>,
    pub overdue: Option<bool>,
    pub stale: Option<bool>,
    pub result_path: Option<crate::StructuredPathWire>,
    pub transcript: Option<LaneTranscript>,
    pub cache_read_share: Option<f64>,
    pub provider_seconds: Option<f64>,
    pub host: Option<String>,
}

fn project_route(route: Option<&crate::run::RouteRecord>) -> Value {
    let mut value = json!(route);
    let Some(skills) = value.get_mut("user_skills") else {
        return value;
    };
    let path = |value: &mut Value| {
        let Some(text) = value.as_str() else {
            return;
        };
        let path = std::path::Path::new(text);
        let projected = if path.is_absolute() {
            crate::StructuredPathWire::from_path(path).ok()
        } else if text.starts_with('~') {
            None
        } else {
            provenant_core::RelativePath::parse(path)
                .ok()
                .and_then(|relative| crate::StructuredPathWire::from_relative(&relative).ok())
        };
        *value = json!(projected);
    };
    if let Some(directory) = skills.get_mut("directory") {
        path(directory);
    }
    if let Some(warnings) = skills.get_mut("warnings").and_then(Value::as_array_mut) {
        for warning in warnings {
            if let Some(value) = warning.get_mut("path") {
                path(value);
            }
        }
    }
    value
}

impl LaneRow {
    /// Project a retained attempt with explicitly supplied observations.
    pub fn project(lane: &crate::run::AttemptRecord, facts: LaneProjectionFacts) -> Self {
        use crate::run::AttemptState;
        let (state, terminal_reason) = Self::state_and_reason(&lane.state);
        let (exit_code, finished_at) = match &lane.state {
            AttemptState::Terminal {
                exit_code,
                finished_at,
                ..
            } => (*exit_code, Some(*finished_at)),
            _ => (None, None),
        };
        let reason = terminal_reason
            .clone()
            .or_else(|| facts.waiting_for.clone());
        let waiting_for = facts.waiting_for.clone();
        let write_grants: Vec<Value> =
            lane.write_grants
                .iter()
                .map(|grant| {
                    let mut value = json!(grant);
                    value["path"] =
                        json!(
                            grant.path.to_path_buf().ok().and_then(|path| {
                                crate::StructuredPathWire::from_path(&path).ok()
                            })
                        );
                    value
                })
                .collect();
        let read_only_writable: Vec<Value> = lane
            .read_only_writable
            .iter()
            .map(|path| {
                let relative = provenant_core::RelativePath::parse(path).ok();
                json!(
                    relative
                        .as_ref()
                        .and_then(|path| { crate::StructuredPathWire::from_relative(path).ok() })
                )
            })
            .collect();
        let protected_paths: Vec<_> = lane
            .protected_paths
            .iter()
            .map(crate::StructuredPathWire::from_wire)
            .collect();
        let mut delivery = json!(lane.delivery);
        if let Some(acceptance) = lane
            .delivery
            .as_ref()
            .and_then(|delivery| delivery.review_acceptance.as_ref())
        {
            delivery["review_acceptance"]["protected_paths"] = json!(
                acceptance
                    .protected_paths
                    .iter()
                    .map(crate::StructuredPathWire::from_wire)
                    .collect::<Vec<_>>()
            );
        }
        let settlement =
            matches!(lane.state, AttemptState::Terminal { .. }).then_some(LaneSettlement {
                reason: terminal_reason,
            });
        Self {
            run_id: lane.run_id.to_string(),
            task_id: lane.task_id.to_string(),
            attempt_id: lane.attempt_id.to_string(),
            attempt_number: lane.attempt_number,
            retry_of: lane.retry_of.map(|id| id.to_string()),
            superseded_by: facts
                .superseded_by
                .or(lane.superseded_by)
                .map(|id| id.to_string()),
            cause: lane.settlement_reason,
            state,
            exit_code,
            reason,
            settlement,
            lane_class: facts.lane_class,
            waiting_for,
            requested_route: json!(lane.requested_route),
            host: facts.host,
            resolved_route: Some(project_route(lane.resolved_route.as_ref())),
            applied_route: project_route(lane.applied_route.as_ref()),
            skipped_routes: json!(lane.skipped_routes),
            account: json!(
                lane.applied_route
                    .as_ref()
                    .and_then(|route| route.account_label.as_deref())
            ),
            observed_route: project_route(lane.observed_route.as_ref()),
            queued_at: json!(lane.queued_at),
            started_at: json!(lane.started_at),
            priority: lane.priority.map(i64::from),
            finished_at: json!(finished_at),
            last_output_at: json!(lane.last_output_at),
            runtime_deadline: json!(lane.runtime_deadline),
            overdue: facts.overdue,
            stale: facts.stale,
            partial_signals: json!(lane.partial_signals),
            delivery_id: lane.delivery_id.map(|id| id.to_string()),
            write_grants: json!(write_grants),
            read_only_writable: json!(read_only_writable),
            git_grants_digest: lane.git_grants_digest.map(|digest| digest.to_string()),
            head: lane.head.map(|head| head.to_string()),
            protected_paths: json!(protected_paths),
            delivery,
            continuation: json!(lane.continuation),
            cache_read_share: facts.cache_read_share,
            result_path: json!(facts.result_path),
            transcript: facts.transcript,
            title: lane.title.clone(),
            requested_priority: lane
                .requested_priority
                .filter(|priority| Some(i32::from(*priority)) != lane.priority)
                .map(i64::from),
            provider_seconds: facts.provider_seconds,
            handoff: None,
            provider_error: None,
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

/// Lane lists and their optional command-specific observations.
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
