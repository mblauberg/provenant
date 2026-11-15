//! Settlement and launch selection must agree on preparation route switches.
use super::*;
use serde_json::json;

#[test]
fn settlement_and_selection_share_the_stop_rule_and_custody_guards() {
    use provenant_records::run::PreparationFailure;
    let limits = crate::config::GraphLimits::default();
    for (outcome, failure) in [
        (
            AttemptOutcome::AuthRequired,
            PreparationFailure::AuthRequired,
        ),
        (
            AttemptOutcome::UsageLimited,
            PreparationFailure::UsageLimited,
        ),
        (
            AttemptOutcome::AdapterUnavailable,
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
                        node, &progress, &limits, None, &facts,
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
