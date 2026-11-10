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
        project_id: String => string();
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
        usage: Value => any();
        delivery_id: Option<String> => nullable(string());
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
        git_import_failure: Value => object(&[("reason", string()), ("head", nullable(string())), ("retained_objects", nullable(super::schema::worktree_path()))], &[]);
        write_grants: Value => nullable(array(grant_schema()));
        terminal_cause: crate::run::TerminalCause => choices(&["preparation_refused", "source_view_refused", "account_login_required", "route_resolution_refused", "access_policy_refused", "disk_floor", "disk_observation_unavailable", "stored_prompt_unreadable", "write_grants_refused", "worktree_busy", "handoff_refused", "retained_writer_import", "unsupported_platform", "unknown_pacing_account", "unregistered_account", "delivery_prepare_credential_helper", "spawn_failure", "process_crash", "token_budget", "settlement_failure", "provider_early_stop", "free_pool_quota", "command_unavailable", "host_native_unconfigured", "identity_unobserved", "stopped_no_work", "context_handoff"]);
        host: String => string();
        title: String => string();
        requested_priority: i64 => integer();
        provider_seconds: f64 => nullable(number());
        handoff: Value => any();
        provider_error: String => string();
        cloud: Value => open_object();
        cloud_state: String => string();
        payload: String => choices(&["evicted"]);
        capabilities: Value => array(object(&[("name", string()), ("source", string())], &[]));
    }
}

pub(super) fn grant_schema() -> Value {
    let mut grant = open_object();
    grant["properties"] = serde_json::json!({"path": nullable(super::schema::worktree_path())});
    grant["required"] = serde_json::json!(["path"]);
    grant
}

impl LaneRow {
    /// Project the full safe cause, remedy and note for structured results.
    pub fn state_and_reason(state: &crate::run::AttemptState) -> (String, Option<String>) {
        Self::state_diagnostic(state, true)
    }

    /// Project a short label for human status rows, without per-lane notes.
    pub fn state_and_label(state: &crate::run::AttemptState) -> (String, Option<String>) {
        Self::state_diagnostic(state, false)
    }

    fn state_diagnostic(
        state: &crate::run::AttemptState,
        detail: bool,
    ) -> (String, Option<String>) {
        use crate::run::AttemptState;
        match state {
            AttemptState::Terminal {
                outcome,
                reason,
                extra,
                ..
            } => (
                serde_json::to_value(outcome)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_default(),
                extra
                    .get("terminal_cause")
                    .and_then(|value| {
                        serde_json::from_value::<crate::run::TerminalCause>(value.clone()).ok()
                    })
                    .map(|cause| {
                        if detail {
                            cause.detail()
                        } else {
                            cause.reason()
                        }
                        .to_owned()
                    })
                    .or_else(|| {
                        (reason.is_some() || *outcome != crate::run::Outcome::Succeeded)
                            .then(|| outcome.diagnostic_reason().to_owned())
                    })
                    // The note is Provenant's own fixed-form text (a retry
                    // decision or a configured executable), never provider output.
                    .map(
                        |safe| match extra.get("terminal_note").and_then(Value::as_str) {
                            Some(note) if detail && !note.is_empty() => {
                                format!("{safe}; {}", crate::diagnostic::safe_text(note))
                            }
                            _ => safe,
                        },
                    ),
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
    pub git_import_head: Option<String>,
    pub retained_git_objects: Option<crate::StructuredPathWire>,
    /// Retention evicted the attempt's bulk payload; its records remain.
    pub payload_evicted: bool,
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
        let (state, mut terminal_reason) = Self::state_and_reason(&lane.state);
        let cloud_state = lane
            .cloud_observation
            .0
            .map(|cloud| cloud.state().to_owned());
        let mut state = cloud_state.clone().unwrap_or(state);
        // Settlement reasons are closed, locally recorded facts, unlike the
        // terminal state's free text, which can contain provider output.
        if let Some(reason) = lane.settlement_reason
            && matches!(
                lane.state,
                AttemptState::Terminal {
                    outcome: crate::run::Outcome::Succeeded
                        | crate::run::Outcome::ReviewRequired
                        | crate::run::Outcome::Uncommitted,
                    ..
                }
            )
        {
            use crate::run::SettlementReason;
            let detail = match reason {
                SettlementReason::HandoffMissing => match &lane.state {
                    AttemptState::Terminal { extra, .. }
                        if extra.get("terminal_cause")
                            == Some(&json!(crate::run::TerminalCause::EmptyWriterResult)) =>
                    {
                        Some(crate::run::TerminalCause::EmptyWriterResult.detail())
                    }
                    _ => Some("the lane finished without a handoff; inspect its result"),
                },
                SettlementReason::DeclaredStop => match &lane.state {
                    AttemptState::Terminal { extra, .. }
                        if extra.get("terminal_cause")
                            == Some(&json!(crate::run::TerminalCause::StoppedNoWork)) =>
                    {
                        Some(crate::run::TerminalCause::StoppedNoWork.detail())
                    }
                    _ => Some("the lane stopped with work remaining; inspect its result"),
                },
                SettlementReason::ProviderStop => {
                    Some("the provider stopped early; inspect its result and resume if needed")
                }
                SettlementReason::UncommittedWork => {
                    Some("the writer has uncommitted changes; preserve or commit the edits")
                }
                _ => None,
            };
            if let Some(detail) = detail {
                terminal_reason = Some(detail.to_owned());
            }
        }
        if matches!(
            lane.state,
            AttemptState::Terminal {
                outcome: crate::run::Outcome::Lost | crate::run::Outcome::Unsettled,
                ..
            }
        ) {
            terminal_reason = Some(format!(
                "the lane outcome is uncertain; inspect status for operation {} before retrying",
                lane.operation_id
            ));
        }
        let git_import_failure =
            lane.settlement_receipts
                .as_ref()
                .and_then(|receipts| match &receipts.git_import {
                    crate::run::EffectDisposition::Failed {
                        reason,
                        retryable: false,
                    } if matches!(lane.state, AttemptState::Terminal { .. }) => Some(reason),
                    _ => None,
                });
        if let Some(reason) = git_import_failure {
            state = "failed".into();
            let mut detail = format!(
                "lane Git import failed: {}; the lane's commits may not have reached its branch",
                crate::diagnostic::safe_text(reason)
            );
            if let Some(head) = &facts.git_import_head {
                detail.push_str(&format!("; head commit {head}"));
            }
            if let Some(objects) = facts
                .retained_git_objects
                .as_ref()
                .and_then(|path| path.to_path_buf().ok())
            {
                detail.push_str(&format!(
                    "; retained objects at {}",
                    crate::diagnostic::safe_text(&objects.display().to_string())
                ));
            }
            terminal_reason = Some(detail);
        }
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
            project_id: lane.project_id.to_string(),
            run_id: lane.run_id.to_string(),
            task_id: lane.task_id.to_string(),
            attempt_id: lane.attempt_id.to_string(),
            attempt_number: lane.attempt_number,
            retry_of: lane.retry_of.map(|id| id.to_string()),
            superseded_by: facts
                .superseded_by
                .or(lane.superseded_by)
                .map(|id| id.to_string()),
            git_import_failure: git_import_failure.map(|reason| {
                json!({
                    "reason": crate::diagnostic::safe_text(reason),
                    "head": facts.git_import_head,
                    "retained_objects": facts.retained_git_objects,
                })
            }),
            cause: if git_import_failure.is_some() {
                Some(crate::run::SettlementReason::Failed)
            } else {
                lane.settlement_reason
            },
            terminal_cause: if git_import_failure.is_some() {
                Some(crate::run::TerminalCause::SettlementFailure)
            } else {
                match &lane.state {
                    AttemptState::Terminal { extra, .. } => extra
                        .get("terminal_cause")
                        .and_then(|value| serde_json::from_value(value.clone()).ok()),
                    _ => None,
                }
            },
            state,
            exit_code,
            reason,
            settlement,
            lane_class: facts.lane_class,
            waiting_for,
            requested_route: json!(lane.requested_route),
            host: facts.host,
            applied_route: project_route(lane.applied_route.as_ref()),
            skipped_routes: Value::Array(
                lane.skipped_routes
                    .iter()
                    .map(|route| {
                        let diagnostic = route.diagnostic();
                        json!({
                            "route": crate::diagnostic::safe_text(&route.route),
                            "kind": route.kind,
                            "cause": route.cause,
                            "code": diagnostic.code,
                            "category": diagnostic.category,
                            "detail": diagnostic.detail,
                            "next": diagnostic.next,
                        })
                    })
                    .collect(),
            ),
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
            usage: json!(lane.usage),
            delivery_id: lane.delivery_id.map(|id| id.to_string()),
            write_grants: Some(json!(write_grants)),
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
            cloud: lane.cloud_view.0.clone(),
            cloud_state,
            capabilities: None,
            payload: facts.payload_evicted.then(|| "evicted".to_owned()),
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

#[cfg(test)]
mod terminal_cause_tests {
    use super::*;
    use crate::run::{AttemptState, Outcome, TerminalCause};

    #[test]
    fn terminal_reasons_use_typed_causes_without_provider_text() {
        for cause in [
            TerminalCause::PreparationRefused,
            TerminalCause::SourceViewRefused,
            TerminalCause::AccountLoginRequired,
            TerminalCause::RouteResolutionRefused,
            TerminalCause::AccessPolicyRefused,
            TerminalCause::DiskFloor,
            TerminalCause::DiskObservationUnavailable,
            TerminalCause::StoredPromptUnreadable,
            TerminalCause::WriteGrantsRefused,
            TerminalCause::WorktreeBusy,
            TerminalCause::HandoffRefused,
            TerminalCause::RetainedWriterImport,
            TerminalCause::UnsupportedPlatform,
            TerminalCause::UnknownPacingAccount,
            TerminalCause::UnregisteredAccount,
            TerminalCause::DeliveryPrepareCredentialHelper,
            TerminalCause::SpawnFailure,
            TerminalCause::ProcessCrash,
            TerminalCause::TokenBudget,
            TerminalCause::SettlementFailure,
            TerminalCause::ProviderEarlyStop,
            TerminalCause::FreePoolQuota,
            TerminalCause::CommandUnavailable,
            TerminalCause::HostNativeUnconfigured,
            TerminalCause::IdentityUnobserved,
            TerminalCause::StoppedNoWork,
            TerminalCause::ContextHandoff,
        ] {
            let mut extra = crate::ExtraFields::default();
            extra.insert(
                "terminal_cause".into(),
                serde_json::to_value(cause).unwrap(),
            );
            extra.insert(
                "terminal_note".into(),
                Value::String("fixture scoped note".into()),
            );
            let state = AttemptState::Terminal {
                extra,
                outcome: Outcome::Failed,
                exit_code: Some(1),
                finished_at: provenant_core::WallTime::from_micros(1).unwrap(),
                reason: Some("secret provider text".into()),
            };
            let (_, reason) = LaneRow::state_and_reason(&state);
            let reason = reason.unwrap();
            assert_eq!(reason, format!("{}; fixture scoped note", cause.detail()));
            assert!(!reason.contains("secret"));
            let (_, label) = LaneRow::state_and_label(&state);
            assert_eq!(label.as_deref(), Some(cause.reason()));
            let label = label.unwrap();
            assert!(label.len() <= 60, "{cause:?}: {label}");
            assert!(label.contains("provenant output ID"), "{label}");
            assert!(!label.contains("fixture scoped note"), "{label}");
            assert!(!label.contains("secret"));
        }
        assert!(TerminalCause::FreePoolQuota.detail().contains("paid route"));
    }
}
