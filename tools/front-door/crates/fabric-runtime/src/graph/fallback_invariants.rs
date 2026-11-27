//! Settlement and launch selection must agree on preparation route switches.
use super::*;
use serde_json::json;

#[test]
fn settlement_and_selection_share_the_stop_rule_and_custody_guards() {
    use provenant_records::run::PreparationFailure;
    let limits = crate::config::GraphLimits::default();
    for (outcome, failure) in [
        (
            GraphAttemptOutcome::AuthRequired,
            PreparationFailure::AuthRequired,
        ),
        (
            GraphAttemptOutcome::UsageLimited,
            PreparationFailure::UsageLimited,
        ),
        (
            GraphAttemptOutcome::AdapterUnavailable,
            PreparationFailure::AdapterUnavailable,
        ),
    ] {
        for held in [false, true] {
            let mut raw = json!({"id":"n", "prompt":"work", "access":"read_only",
                "routes":[{"adapter":"a","model":"a/m"},{"adapter":"b","model":"b/m"}]});
            if held {
                raw["on_stop"] = json!({"auth_required":"chair", "usage_limited":"chair", "adapter_unavailable":"chair"});
            }
            let document: GraphDocument = serde_json::from_value(json!({
                "schema_version":1, "graph_id":"g", "nodes":[raw]
            }))
            .unwrap();
            let (plan, _) = validate_plan(&document).unwrap();
            let node = &plan.nodes[0];
            for local in [false, true] {
                for released in [false, true] {
                    let evidence = PreparationFallbackEvidence {
                        failure,
                        handoff: if local {
                            provenant_routing::FallbackHandoff::Local
                        } else {
                            provenant_routing::FallbackHandoff::UncertainOrLaunched
                        },
                        custody_released: released,
                    };
                    let settlement = next_step::settlement_fallback_decision(
                        node,
                        0,
                        outcome,
                        None,
                        false,
                        FallbackEvidence {
                            preparation: Some(evidence),
                            unreadable: false,
                            review_limit_reached: false,
                            cause: None,
                            skipped_accounts: &[],
                        },
                    );
                    let progress: NodeProgress = serde_json::from_value(json!({
                        "id":"n", "state":"running", "worktree":null, "attempts":[{
                            "launched":false, "key":{"graph_id":"g","node_id":"n","route_index":0, "attempt": 1},
                            "route":{"adapter":"a","model":"a/m"}, "state":"terminal",
                            "attempt_ref":"attempt-1", "outcome":outcome, "launch_actor":"chair"
                        }]
                    }))
                    .unwrap();
                    let facts = next_attempt::NextAttemptFacts {
                        launch_accepted: None,
                        fallback: Some(next_attempt::PreparationObservation {
                            evidence: Some(evidence),
                            unreadable: false,
                            cause: None,
                        }),
                    };
                    let selected = next_attempt::next_attempt_transition(
                        node, &progress, &limits, None, None, &facts,
                    );
                    let advances = matches!(
                        selected,
                        next_attempt::NextAttemptDecision::Ready(NextAttempt::Candidate {
                            route_index: 1,
                            ..
                        })
                    );
                    assert_eq!(advances, local && released && !held);
                    assert_eq!(advances, matches!(settlement, SettlementFallback::Continue));
                }
            }
        }
    }
}

#[test]
fn sealed_chair_only_preparation_stop_never_advances_to_another_route() {
    use provenant_records::StopCause;
    use provenant_records::run::{CauseKind, PreparationFailure};

    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1, "graph_id":"g", "nodes":[{
            "id":"n", "prompt":"work", "access":"read_only",
            "routes":[{"adapter":"a","model":"a/m"},{"adapter":"b","model":"b/m"}]
        }]
    }))
    .unwrap();
    let (plan, _) = validate_plan(&document).unwrap();
    let decision = next_step::settlement_fallback_decision(
        &plan.nodes[0],
        0,
        GraphAttemptOutcome::PreparationRefused(CauseKind::UnsupportedPlatform),
        Some(StopCause::UnsupportedPlatform),
        false,
        FallbackEvidence {
            preparation: Some(PreparationFallbackEvidence {
                failure: PreparationFailure::AdapterUnavailable,
                handoff: provenant_routing::FallbackHandoff::Local,
                custody_released: true,
            }),
            unreadable: false,
            review_limit_reached: false,
            cause: None,
            skipped_accounts: &[],
        },
    );
    assert!(matches!(
        decision,
        SettlementFallback::Fail {
            code: Some("preparation_refused"),
            message: Some(ref message),
        } if message == CauseKind::UnsupportedPlatform.detail()
    ));
}

/// A cause the work can recover on its own route keeps its own identity: a
/// pinned read-only node never reports a lost lane token (or any other
/// same-route cause) as an authentication failure or a pinned-route dead end.
/// Derived from the stop table, so a new same-route cause joins the rule.
#[test]
fn a_recoverable_stop_keeps_its_code_and_never_reports_auth_required() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1, "graph_id":"g", "nodes":[{
            "id":"n", "prompt":"work", "access":"read_only",
            "routes":[{"adapter":"a","model":"a/m","account":"acct"}]
        }]
    }))
    .unwrap();
    let (plan, _) = validate_plan(&document).unwrap();
    let node = &plan.nodes[0];
    let recoverable = provenant_records::StopCause::ALL
        .iter()
        .copied()
        .filter(|cause| cause.continues_on_same_route())
        .collect::<Vec<_>>();
    assert!(!recoverable.is_empty(), "the rule needs at least one cause");
    for cause in recoverable {
        // Even when the provider labelled the stop `auth_required`, the sealed
        // cause wins and the node retries on its route instead of failing.
        let decision = next_step::settlement_fallback_decision(
            node,
            0,
            GraphAttemptOutcome::AuthRequired,
            Some(cause),
            true,
            FallbackEvidence {
                preparation: None,
                unreadable: false,
                review_limit_reached: false,
                cause: None,
                skipped_accounts: &[],
            },
        );
        assert_eq!(decision, SettlementFallback::ContinueSameRoute, "{cause}");
        assert_ne!(cause.advance_code(), "auth_required", "{cause}");
    }
    // A route-specific cause still reports the pinned-route dead end: the
    // guard was narrowed, not removed.
    let pinned = next_step::settlement_fallback_decision(
        node,
        0,
        GraphAttemptOutcome::AuthRequired,
        Some(provenant_records::StopCause::AuthRequired),
        true,
        FallbackEvidence {
            preparation: None,
            unreadable: false,
            review_limit_reached: false,
            cause: None,
            skipped_accounts: &[],
        },
    );
    assert!(matches!(
        pinned,
        SettlementFallback::Fail {
            code: Some("fallback_route_pinned"),
            ..
        }
    ));
}

/// An explicit `on_stop` suppresses the same-route continuation, so the node
/// fails; it must still fail under the cause it sealed, not under the label the
/// provider reported. A lost lane token is not an authentication failure.
#[test]
fn explicit_on_stop_keeps_the_sealed_cause_code() {
    use provenant_records::StopCause;

    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1, "graph_id":"g", "nodes":[{
            "id":"n", "prompt":"work", "access":"read_only",
            "routes":[{"adapter":"a","model":"a/m"}],
            "on_stop":{"lane_token_lost":"chair"}
        }]
    }))
    .unwrap();
    let (plan, _) = validate_plan(&document).unwrap();
    let decision = next_step::settlement_fallback_decision(
        &plan.nodes[0],
        0,
        GraphAttemptOutcome::AuthRequired,
        Some(StopCause::LaneTokenLost),
        true,
        FallbackEvidence {
            preparation: None,
            unreadable: false,
            review_limit_reached: false,
            cause: None,
            skipped_accounts: &[],
        },
    );
    assert_eq!(
        decision,
        SettlementFallback::Fail {
            code: Some(StopCause::LaneTokenLost.advance_code()),
            message: Some("The lane requires authentication.".into()),
        }
    );
    assert_ne!(
        StopCause::LaneTokenLost.advance_code(),
        "auth_required",
        "the regression would be invisible if the cause shared the provider label"
    );
}
