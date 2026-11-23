//! Every enum stored inside an attempt record has a serialised name list that
//! is part of the record contract. A rename without a run-store migration
//! changes stored bytes, so this test pins the current names: the old name must
//! stop decoding, and that failure is the signal to add a migration.
use provenant_records::{
    Layer, RouteExclusionReason, RouteReadiness, SandboxConstraint,
    diagnostic::{
        BoundaryClass, ConfinementGuard, ContinuationCopyLimit, ContinuationField,
        ContinuationIssue, ContinuationRule, LaneLimit, PacingReserveSetting, PacingSettingSource,
        PlanFailure, PortSettingSource, PressureLevel, PressureThreshold, ResourceFact,
    },
    handoff::{BlockedOn, HandoffStatus, SpecConcernStatus, WriteScopeReport},
    limit::{BudgetAction, LimitStrength},
    policy::{Boundary, Source, Strength},
    ports::ExitDisposition,
    route_selection::{
        Capability, ExclusionSource, PacingPolicy, PriorSource, SelectionRule, WeightSource,
        Workload,
    },
    run::{
        AccountSource, AttemptState, CancellationDisposition, CauseSource, ChildWait,
        EffectDisposition, LaneAccess, OriginReceipt, Outcome, PartialSignal, PathScope,
        PreparationFailure, RepositoryAdmissionPhase, RequestedRoute, SandboxFloorStrength,
        SettlementEffect, SettlementReason, SkipKind, StopWorkPush, StopWorkTrigger, TerminalCause,
        TerminalEvidence, UserSkillsMode, WriteGrantLimitOrigin, WriteSource, WriterDeliveryStage,
        WriterDeliveryState,
    },
    stop::StopCause,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn invalid(kind: &str, detail: impl std::fmt::Display) -> Box<dyn std::error::Error> {
    Box::new(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("{kind}: {detail}"),
    ))
}

const GIT_OID: &str = "0123456789abcdef0123456789abcdef01234567";
const NOW: &str = "2026-10-29T00:00:00.000000Z";

fn assert_unit_names<T>(kind: &str, names: &[&str]) -> TestResult
where
    T: DeserializeOwned + Serialize,
{
    for name in names {
        let seed = Value::String((*name).to_owned());
        let decoded: T = serde_json::from_value(seed.clone())
            .map_err(|error| invalid(kind, format!("{name} is not a serialised name: {error}")))?;
        assert_eq!(serde_json::to_value(decoded)?, seed, "{kind}: {name}");
    }
    Ok(())
}

fn assert_tagged<T>(kind: &str, tag: &str, cases: &[(&str, Value)]) -> TestResult
where
    T: DeserializeOwned + Serialize,
{
    for (name, seed) in cases {
        let decoded: T = serde_json::from_value(seed.clone())
            .map_err(|error| invalid(kind, format!("{name} does not decode: {error}")))?;
        let value = serde_json::to_value(decoded)?;
        assert_eq!(
            value.get(tag).and_then(Value::as_str),
            Some(*name),
            "{kind}: {value}"
        );
    }
    Ok(())
}

fn assert_external_names<T>(kind: &str, cases: &[(&str, Value)]) -> TestResult
where
    T: DeserializeOwned + Serialize,
{
    for (name, seed) in cases {
        let decoded: T = serde_json::from_value(seed.clone())
            .map_err(|error| invalid(kind, format!("{name} does not decode: {error}")))?;
        let value = serde_json::to_value(decoded)?;
        assert!(value.get(*name).is_some(), "{kind}: {name}: {value}");
    }
    Ok(())
}

fn assert_all_names<T: Copy + Serialize>(kind: &str, all: &[T], names: &[&str]) -> TestResult {
    let mut actual = Vec::with_capacity(all.len());
    for value in all {
        let value = serde_json::to_value(value)?;
        let Some(name) = value.as_str() else {
            return Err(invalid(kind, "variant does not serialise as a string"));
        };
        actual.push(name.to_owned());
    }
    let expected: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
    assert_eq!(actual, expected, "{kind}");
    Ok(())
}

#[test]
fn retired_enum_names_are_not_silently_accepted() {
    for name in ["git_isolation", "data_integrity"] {
        assert!(
            serde_json::from_value::<Boundary>(json!(name)).is_err(),
            "Boundary still accepts retired {name}"
        );
    }
    assert!(
        serde_json::from_value::<WriteSource>(json!("lane_cache")).is_err(),
        "WriteSource still accepts retired lane_cache"
    );
}

#[test]
fn retired_source_spellings_are_not_silently_accepted() {
    // Spec: docs/specs/state.md#every-setting-reports-its-layer
    for retired in [
        "builtin",
        "route_policy",
        "configuration",
        "shipped_default",
        "knob",
        "restriction",
        "platform",
    ] {
        assert!(
            serde_json::from_value::<Layer>(json!(retired)).is_err(),
            "Layer still accepts retired {retired}"
        );
    }
}

#[test]
fn attempt_record_enum_names_are_a_deliberate_contract() -> TestResult {
    // The policy vocabulary this defect changed.
    assert_unit_names::<Boundary>(
        "Boundary",
        &[
            "credentials",
            "other_work",
            "authority",
            "operation",
            "none",
        ],
    )?;
    assert_unit_names::<Strength>("Strength", &["refuse", "warn", "off"])?;
    assert_unit_names::<Source>(
        "policy::Source",
        &["compiled_default", "host", "project", "dispatch"],
    )?;

    // Grants, limits and terminal state.
    assert_unit_names::<WriteSource>(
        "WriteSource",
        &[
            "write_root",
            "tool_write",
            "state_write",
            "credential_write",
            "scratch",
            "private_state",
            "read_only_exception",
            "git_state",
        ],
    )?;
    assert_unit_names::<PathScope>("PathScope", &["exact", "subtree"])?;
    assert_unit_names::<Layer>(
        "Layer",
        &["default", "host", "project", "template", "tier", "launch"],
    )?;
    assert_unit_names::<LimitStrength>("LimitStrength", &["default", "ceiling", "advisory"])?;
    assert_all_names::<BudgetAction>(
        "BudgetAction",
        &BudgetAction::ALL,
        &["wrap_up", "warn", "stop"],
    )?;
    assert_unit_names::<Outcome>(
        "Outcome",
        &[
            "succeeded",
            "review_required",
            "failed",
            "rejected",
            "cancelled",
            "timed_out",
            "rate_limited",
            "quota_limited",
            "auth_required",
            "model_unavailable",
            "permission_denied",
            "quarantined",
            "lost",
            "unsettled",
            "uncommitted",
            "input_required",
            "unclassified",
        ],
    )?;
    assert_unit_names::<TerminalCause>(
        "TerminalCause",
        &[
            "source_view_refused",
            "project_binding_directory_unsafe",
            "account_login_required",
            "route_resolution_refused",
            "access_policy_refused",
            "disk_floor",
            "disk_observation_unavailable",
            "stored_prompt_unreadable",
            "cloud_placement_removed",
            "write_grants_refused",
            "worktree_busy",
            "handoff_refused",
            "retained_writer_import",
            "unsupported_platform",
            "unknown_pacing_account",
            "unregistered_account",
            "delivery_prepare_credential_helper",
            "spawn_failure",
            "process_crash",
            "token_budget",
            "settlement_failure",
            "provider_early_stop",
            "free_pool_quota",
            "command_unavailable",
            "host_native_unconfigured",
            "identity_mismatch",
            "stopped_no_work",
            "empty_writer_result",
            "context_handoff",
            "blocked",
            "lane_token_lost",
            "steered",
            "unrecorded",
        ],
    )?;
    assert_external_names::<TerminalCause>(
        "TerminalCause",
        &[
            (
                "writer_pin_mismatch",
                json!({"writer_pin_mismatch": {"pinned": GIT_OID, "head": GIT_OID}}),
            ),
            (
                "provider_capture_defect",
                json!({"provider_capture_defect": {"line": 1}}),
            ),
        ],
    )?;
    assert_tagged::<TerminalEvidence>(
        "TerminalEvidence",
        "kind",
        &[
            ("process_exit", json!({"kind": "process_exit"})),
            ("pre_launch_refusal", json!({"kind": "pre_launch_refusal"})),
            (
                "effective_cancellation",
                json!({"kind": "effective_cancellation"}),
            ),
            (
                "import_cancellation",
                json!({"kind": "import_cancellation"}),
            ),
            (
                "unsettled",
                json!({"kind": "unsettled", "reason": "fixture"}),
            ),
            ("lost", json!({"kind": "lost", "reason": "fixture"})),
        ],
    )?;
    assert_unit_names::<PartialSignal>(
        "PartialSignal",
        &[
            "identity_unobserved",
            "handoff_missing",
            "handoff_malformed",
            "declared_stop",
            "provider_stop",
            "uncommitted_work",
            "stray_work",
        ],
    )?;
    assert_unit_names::<PreparationFailure>(
        "PreparationFailure",
        &["auth_required", "usage_limited", "adapter_unavailable"],
    )?;
    assert_unit_names::<CancellationDisposition>(
        "CancellationDisposition",
        &[
            "effective",
            "import_effective",
            "delivery_effective",
            "acknowledged_ineffective",
            "unknown",
        ],
    )?;
    assert_tagged::<EffectDisposition>(
        "EffectDisposition",
        "state",
        &[
            ("pending", json!({"state": "pending"})),
            ("done", json!({"state": "done"})),
            (
                "cancelled",
                json!({"state": "cancelled", "reason": "fixture", "retryable": false}),
            ),
            (
                "failed",
                json!({"state": "failed", "reason": "fixture", "retryable": false}),
            ),
        ],
    )?;
    assert_unit_names::<SettlementEffect>(
        "SettlementEffect",
        &[
            "git_import",
            "manifest_conflict",
            "cleanup_custody_release",
            "delivery",
        ],
    )?;
    assert_all_names::<SettlementReason>(
        "SettlementReason",
        SettlementReason::ALL,
        &[
            "completed",
            "completed_before_budget",
            "handoff_missing",
            "handoff_malformed",
            "empty_result",
            "declared_stop",
            "provider_stop",
            "uncommitted_work",
            "stray_work",
            "pre_launch_refusal",
            "cancelled",
            "steered",
            "lost",
            "unsettled",
            "unclassified",
            "failed",
        ],
    )?;
    assert_unit_names::<CauseSource>("CauseSource", &["provider", "host", "owner", "unavailable"])?;
    assert_unit_names::<ChildWait>("ChildWait", &["children", "admission"])?;

    // Writer receipts and access receipts.
    assert_unit_names::<WriterDeliveryStage>(
        "WriterDeliveryStage",
        &[
            "prepare",
            "head",
            "push",
            "fetch",
            "upstream",
            "verify",
            "pull_request",
            "interrupted",
            "integrate",
        ],
    )?;
    assert_tagged::<WriterDeliveryState>(
        "WriterDeliveryState",
        "state",
        &[
            (
                "delivered",
                json!({"state": "delivered", "pull_request": null}),
            ),
            ("no_new_commits", json!({"state": "no_new_commits"})),
            ("not_attempted", json!({"state": "not_attempted"})),
            ("failed", json!({"state": "failed", "stage": "prepare"})),
        ],
    )?;
    assert_unit_names::<LaneAccess>("LaneAccess", &["host", "isolated"])?;
    assert_unit_names::<Layer>(
        "Layer (lane access)",
        &[
            "default", "host", "project", "tier", "template", "route", "launch",
        ],
    )?;
    assert_unit_names::<UserSkillsMode>("UserSkillsMode", &["discover", "off"])?;
    assert_unit_names::<AccountSource>("AccountSource", &["continuation", "pin", "automatic"])?;
    assert_unit_names::<Layer>("Layer (sandbox)", &["default", "host", "route", "parent"])?;
    assert_unit_names::<SandboxConstraint>("SandboxConstraint", &["restriction", "platform"])?;
    assert_unit_names::<SandboxFloorStrength>("SandboxFloorStrength", &["off", "warn", "strict"])?;
    assert_unit_names::<WriteGrantLimitOrigin>(
        "WriteGrantLimitOrigin",
        &["configuration", "compiled_default"],
    )?;
    assert_unit_names::<RepositoryAdmissionPhase>(
        "RepositoryAdmissionPhase",
        &["acquire_pending", "acquired", "release_pending", "released"],
    )?;
    assert_unit_names::<Layer>(
        "Layer (ports)",
        &["default", "detected", "host", "host_project", "project"],
    )?;
    assert_unit_names::<ExitDisposition>("ExitDisposition", &["refused", "unknown"])?;
    assert_unit_names::<SkipKind>(
        "SkipKind",
        &[
            "unavailable",
            "account",
            "auth_required",
            "setup",
            "refused",
        ],
    )?;
    assert_unit_names::<StopWorkTrigger>(
        "StopWorkTrigger",
        &[
            "wall_deadline",
            "token_budget",
            "explicit_cancellation",
            "steered",
            "context_handoff",
        ],
    )?;
    assert_unit_names::<StopWorkPush>(
        "StopWorkPush",
        &[
            "not_granted",
            "not_attempted",
            "not_needed",
            "pushed",
            "failed",
        ],
    )?;

    // Route receipt.
    assert_tagged::<RequestedRoute>(
        "RequestedRoute",
        "kind",
        &[
            (
                "adapter",
                json!({"kind": "adapter", "provider": "p", "name": "n"}),
            ),
            (
                "model",
                json!({"kind": "model", "provider": "p", "model": "m", "effort": null}),
            ),
            (
                "named",
                json!({"kind": "named", "name": "n", "effort": null}),
            ),
            (
                "review",
                json!({"kind": "review", "name": "n", "effort": null, "excluded_families": [], "distinct_families": false}),
            ),
        ],
    )?;
    assert_tagged::<RouteReadiness>(
        "RouteReadiness",
        "status",
        &[
            (
                "ready",
                json!({"status": "ready", "availability_ref": "a", "expires_unix_seconds": 1}),
            ),
            (
                "unavailable",
                json!({"status": "unavailable", "reason": "unknown_availability"}),
            ),
        ],
    )?;
    assert_tagged::<AttemptState>(
        "AttemptState",
        "state",
        &[
            ("queued", json!({"state": "queued"})),
            ("starting", json!({"state": "starting"})),
            ("running", json!({"state": "running"})),
            (
                "settling",
                json!({"state": "settling", "publication_pending": true}),
            ),
            (
                "terminal",
                json!({"state": "terminal", "outcome": "succeeded", "exit_code": null, "finished_at": NOW, "reason": null}),
            ),
        ],
    )?;
    assert_tagged::<OriginReceipt>(
        "OriginReceipt",
        "kind",
        &[
            ("compiled", json!({"kind": "compiled"})),
            (
                "user",
                json!({"kind": "user", "path": {"encoding": "utf8", "path": "p"}}),
            ),
        ],
    )?;

    // Skip and diagnostic receipts.
    assert_unit_names::<RouteExclusionReason>(
        "RouteExclusionReason",
        &[
            "setup_failed",
            "account_pinned",
            "admission_refused",
            "disabled",
            "missing_executable",
            "provider_updating",
            "unknown_availability",
            "stale_availability",
            "unknown_account",
            "unresolved_account",
            "disclosure_denied",
            "confinement_unavailable",
            "unsupported_effort",
            "exhausted",
            "pacing_reserve",
            "unknown_partial_headroom",
            "auth_required",
            "login_expired",
            "unverified_model",
            "account_mismatch",
            "free_tier_refused",
            "provider_refused",
            "invalid_account_evidence",
        ],
    )?;
    assert_unit_names::<BoundaryClass>(
        "BoundaryClass",
        &["none", "lane_git", "credentials", "integrity", "authority"],
    )?;
    assert_unit_names::<PlanFailure>(
        "PlanFailure",
        &[
            "invalid_roots",
            "unconfined_extensions",
            "protected_overlap",
            "invalid_path",
            "credential_root",
            "anchor_mismatch",
            "broad_root",
            "permission",
            "read_only",
            "source_head",
            "home",
        ],
    )?;
    assert_unit_names::<ConfinementGuard>(
        "ConfinementGuard",
        &[
            "cache_sweep_depth_exceeded",
            "cache_sweep_failed",
            "confinement_filesystem_failed",
            "credential_root_refused",
            "git_control_entries_exceeded",
            "git_control_hard_linked",
            "git_head_read_failed",
            "git_write_authority_missing",
            "grant_limit_exceeded",
            "invalid_permission_path",
            "invalid_plan_roots",
            "invalid_private_home_recipe",
            "invalid_profile_fixture",
            "invalid_read_only_policy",
            "nested_confinement_unsupported",
            "path_anchor_changed",
            "path_not_utf8",
            "permission_not_recorded",
            "permission_root_too_broad",
            "pinned_source_requires_git",
            "private_home_conflict",
            "private_home_durability_unconfirmed",
            "private_home_filesystem_failed",
            "private_home_limit_exceeded",
            "private_home_read_failed",
            "private_home_state_unsupported",
            "process_policy_missing",
            "profile_limit_exceeded",
            "profile_transport_failed",
            "project_binding_directory_unsafe",
            "project_binding_mismatch",
            "project_binding_missing",
            "protected_path_overlap",
            "read_only_preparation_failed",
            "read_only_run_refused",
            "repository_authority_revalidation_failed",
            "sandbox_backend_unavailable",
            "sandbox_executable_missing",
            "sandbox_executable_untrusted",
            "sandbox_execution_failed",
            "sandbox_probe_cancelled",
            "sandbox_probe_failed",
            "source_head_invalid",
            "source_head_missing",
            "source_head_moved",
            "unconfined_extensions",
            "unsafe_private_home_entry",
            "unsupported_sandbox_platform",
        ],
    )?;
    assert_unit_names::<ContinuationRule>(
        "ContinuationRule",
        &["request", "retained_route", "contract", "native_session"],
    )?;
    assert_unit_names::<ContinuationField>(
        "ContinuationField",
        &[
            "applied_route",
            "provider",
            "model",
            "account",
            "effort",
            "session",
            "host",
            "grant_digest",
            "native_home",
            "session_state",
            "rollout",
        ],
    )?;
    assert_unit_names::<ContinuationIssue>(
        "ContinuationIssue",
        &["missing", "invalid", "mismatch", "unavailable"],
    )?;
    assert_tagged::<ResourceFact>(
        "ResourceFact",
        "kind",
        &[
            (
                "disk",
                json!({"kind": "disk", "free": 1, "floor": 2, "origin": "host"}),
            ),
            (
                "argv_prompt_limit",
                json!({"kind": "argv_prompt_limit", "limit": 1, "origin": "default"}),
            ),
            (
                "continuation_copy",
                json!({"kind": "continuation_copy", "limit": 1, "setting": "bytes", "origin": "host", "available_bytes": null}),
            ),
            (
                "write_grant_limit",
                json!({"kind": "write_grant_limit", "limit": 1, "origin": "host"}),
            ),
            (
                "grant_limit",
                json!({"kind": "grant_limit", "limit": 1, "origin": "host"}),
            ),
            (
                "profile_limit",
                json!({"kind": "profile_limit", "bytes": 1, "limit": 2, "origin": "host"}),
            ),
            (
                "dependency_preparation",
                json!({"kind": "dependency_preparation", "configured": true}),
            ),
            (
                "lane_ports",
                json!({"kind": "lane_ports", "blocks": 1, "origin": "configuration"}),
            ),
            (
                "pacing_reserve",
                json!({"kind": "pacing_reserve", "percent": 1, "setting": "account", "origin": "configuration", "account": "a", "window": "w", "setting_path": "p"}),
            ),
            (
                "memory_pressure",
                json!({"kind": "memory_pressure", "level": "warning", "threshold": "critical", "origin": "host"}),
            ),
            (
                "lane_cap",
                json!({"kind": "lane_cap", "active": 1, "limit": 2, "lane_limit": "host_total", "origin": "host"}),
            ),
            (
                "processes",
                json!({"kind": "processes", "used": 1, "limit": 2, "reserved": 0}),
            ),
            (
                "confinement_rule",
                json!({"kind": "confinement_rule", "rule": "peer_project_outside_home", "path": "/fixture/project/.git"}),
            ),
        ],
    )?;

    // Diagnostic limits nested in ResourceFact.
    assert_unit_names::<Layer>("Layer (setting)", &["default", "host"])?;
    assert_unit_names::<PressureLevel>("PressureLevel", &["normal", "warning", "critical"])?;
    assert_unit_names::<PressureThreshold>("PressureThreshold", &["warning", "critical", "never"])?;
    assert_unit_names::<LaneLimit>(
        "LaneLimit",
        &[
            "host_writers",
            "project_writers",
            "host_total",
            "project_total",
        ],
    )?;
    assert_unit_names::<PacingReserveSetting>(
        "PacingReserveSetting",
        &["adapter_five_hour", "adapter_seven_day", "account", "free"],
    )?;
    assert_unit_names::<PacingSettingSource>(
        "PacingSettingSource",
        &["configuration", "shipped_default", "unresolved"],
    )?;
    assert_unit_names::<PortSettingSource>(
        "PortSettingSource",
        &["configuration", "shipped_default", "recorded_plan"],
    )?;
    assert_unit_names::<ContinuationCopyLimit>(
        "ContinuationCopyLimit",
        &["bytes", "entries", "depth"],
    )?;

    // Route-selection receipts.
    assert_unit_names::<SelectionRule>(
        "SelectionRule",
        &[
            "weighted_draw",
            "exploration_draw",
            "model_choice_override",
            "first_zero_weight_ready",
            "first_standby_ready",
            "first_recoverable_unavailable",
            "first_unavailable",
        ],
    )?;
    assert_unit_names::<ExclusionSource>("ExclusionSource", &["flag", "graph", "config"])?;
    assert_unit_names::<WeightSource>("WeightSource", &["config", "catalogue"])?;
    assert_unit_names::<PacingPolicy>("PacingPolicy", &["pace", "spend", "max", "off"])?;
    assert_unit_names::<Capability>("Capability", &["general", "strong", "frontier"])?;
    assert_unit_names::<Workload>(
        "Workload",
        &["general", "read_heavy", "edit_heavy", "review"],
    )?;
    assert_unit_names::<PriorSource>(
        "PriorSource",
        &["config:disabled", "route_outcomes", "no_evidence"],
    )?;

    // A sealed stop carries its cause and the handoff it declared.
    assert_all_names::<StopCause>(
        "StopCause",
        &StopCause::ALL,
        &[
            "stopped_early",
            "blocked",
            "blocked_environment",
            "blocked_dependency",
            "handoff_missing",
            "empty_result",
            "uncommitted",
            "input_required",
            "context_handoff",
            "token_budget",
            "deadline",
            "cancelled",
            "provider_stop",
            "process_crash",
            "auth_required",
            "usage_limited",
            "model_unavailable",
            "adapter_unavailable",
            "permission_denied",
            "identity_mismatch",
            "lane_token_lost",
            "source_view_refused",
            "route_refused",
            "access_policy_refused",
            "disk_floor",
            "disk_unobserved",
            "prompt_unreadable",
            "cloud_placement_removed",
            "write_grants_refused",
            "worktree_busy",
            "launch_handoff_refused",
            "retained_writer_import",
            "unsupported_platform",
            "unknown_pacing_account",
            "unregistered_account",
            "credential_helper_failed",
            "command_unavailable",
            "host_native_unconfigured",
            "spawn_failure",
            "settlement_failed",
            "lost",
            "unsettled",
            "quarantined",
            "handoff_malformed",
            "steered",
            "unclassified",
        ],
    )?;
    assert_unit_names::<HandoffStatus>("HandoffStatus", &["done", "stopped_early", "blocked"])?;
    assert_unit_names::<BlockedOn>(
        "BlockedOn",
        &["owner_decision", "capability", "environment", "dependency"],
    )?;
    assert_unit_names::<SpecConcernStatus>(
        "SpecConcernStatus",
        &["as_written", "diverged", "stopped"],
    )?;
    assert_tagged::<WriteScopeReport>(
        "WriteScopeReport",
        "kind",
        &[
            (
                "full",
                json!({"kind": "full", "schema_version": 1, "code": "c", "base": "b", "head": "h", "strength": "warn", "source": "dispatch", "out_of_scope_paths": []}),
            ),
            (
                "compact",
                json!({"kind": "compact", "code": "c", "strength": "warn", "source": "dispatch", "paths_artifact": "p", "out_of_scope_count": 0, "paths_omitted_from_handoff": true}),
            ),
        ],
    )?;
    Ok(())
}
