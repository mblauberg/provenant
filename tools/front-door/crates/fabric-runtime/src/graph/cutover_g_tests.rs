//! Depletion settlement invariants retained across the graph format cut-over.
use super::*;
use serde_json::json;

#[test]
fn depleted_single_route_is_fenced_for_the_chair() {
    let document = super::model::document_fixture(json!({"graph_id":"g","nodes":[
        {"id":"n","access":"read_only","prompt":"work","route":{"adapter":"a","model":"a/m"}}
    ]}));
    let limits = crate::config::GraphLimits::default();
    let (plan, _) = validate_flat_plan(&document, &limits).unwrap();
    let mut checkpoint =
        transitions::initial_checkpoint(&document, &plan, "digest".into(), None, None, None);
    checkpoint.nodes[0].state = NodeState::Failed;
    let mut attempt = super::model::graph_attempt_fixture(
        AttemptKey {
            extra: Default::default(),
            retry_generation: None,
            previous_attempt_ref: None,
            graph_id: "g".into(),
            node_id: "n".into(),
            route_index: 0,
            attempt: 1,
        },
        plan.nodes[0].routes[0].clone(),
        Some(AttemptOutcome::UsageLimited),
    );
    attempt.launched = true;
    attempt.attempt_ref = Some("a/1".into());
    attempt.launch_actor = "chair".into();
    attempt.settled = 1;
    attempt.stop = Some(provenant_records::StopCause::UsageLimited);
    checkpoint.nodes[0].attempts.push(attempt);

    let mut no_facts = |_request: next_step::DepletionRequest<'_>| Ok(None);
    let (_, settled) = next_step::settle_stop(
        &document,
        &plan,
        &checkpoint,
        &limits,
        &provenant_config::PortTable::builtin().worktree_layout,
        &mut no_facts,
    )
    .expect("depleted single-route node settles");
    assert_eq!(settled.nodes[0].state, NodeState::Failed);
    assert!(settled.nodes[0].stop_fence);
    assert!(settled.nodes[0].advance_error.is_none());
}
