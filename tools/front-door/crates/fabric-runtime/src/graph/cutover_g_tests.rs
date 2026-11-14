//! Cut-over invariants for removed shapes and skipped required inputs.
use super::*;
use serde_json::json;

#[test]
fn obsolete_graph_continuation_is_rejected_during_decode() {
    let node =
        json!({"id":"n", "access":"read_only", "prompt":"work", "auto_continue_no_work":true});
    assert!(serde_json::from_value::<GraphNode>(node).is_err());
}

#[test]
fn skipped_required_inputs_are_recorded_in_the_plan() {
    let document: GraphDocument = serde_json::from_value(json!({"schema_version":1,"graph_id":"g","nodes":[
        {"id":"a","access":"read_only","prompt":"a","route":{"adapter":"a","model":"a/m"},"when":[{"name":"on","expected":"true","actual":"false"}]},
        {"id":"b","access":"read_only","prompt":"b","route":{"adapter":"a","model":"a/m"},"depends_on":["a"],"inputs":[{"node":"a","outputs":["result"],"required_outputs":["result"]}]}
    ]})).unwrap();
    let pruned = when::prune(&document).unwrap().unwrap();
    assert!(pruned.nodes[0].extra.contains_key("removed_requirements"));
    let (plan, _) = validate_flat_plan(&document, &crate::config::GraphLimits::default()).unwrap();
    let checkpoint =
        transitions::initial_checkpoint(&document, &plan, "digest".into(), None, None, None, None);
    assert!(
        checkpoint.nodes[0]
            .extra
            .contains_key("removed_requirements")
    );
}

#[test]
fn chair_bound_depletion_reads_facts_once_across_passes() {
    let document: GraphDocument =
        serde_json::from_value(json!({"schema_version":1,"graph_id":"g","nodes":[
            {"id":"n","access":"read_only","prompt":"work","route":{"adapter":"a","model":"a/m"}}
        ]}))
        .unwrap();
    let limits = crate::config::GraphLimits::default();
    let (plan, _) = validate_flat_plan(&document, &limits).unwrap();
    let mut checkpoint =
        transitions::initial_checkpoint(&document, &plan, "digest".into(), None, None, None, None);
    checkpoint.nodes[0].state = NodeState::Failed;
    checkpoint.nodes[0].attempts.push(GraphAttempt {
        extra: Default::default(),
        launched: true,
        review_substitution: false,
        key: AttemptKey {
            extra: Default::default(),
            graph_id: "g".into(),
            node_id: "n".into(),
            route_index: 0,
            attempt: 1,
        },
        route: plan.nodes[0].routes[0].clone(),
        state: AttemptState::Terminal,
        attempt_ref: Some("a/1".into()),
        outcome: Some(AttemptOutcome::UsageLimited),
        launch_actor: "chair".into(),
        input_cap: None,
        input_cap_source: None,
        input_manifest: None,
        usage: None,
        settled: 1,
        stop: Some(provenant_records::StopCause::UsageLimited),
    });
    let mut reads = 0;
    let mut facts = |_request: next_step::DepletionRequest<'_>| {
        reads += 1;
        Ok(None)
    };
    for _ in 0..10 {
        if let Some((_, next)) = next_step::settle_stop(
            &document,
            &plan,
            &checkpoint,
            &limits,
            &provenant_config::PortTable::builtin().worktree_layout,
            &mut facts,
        ) {
            checkpoint = next;
        }
    }
    drop(facts);
    assert_eq!(reads, 1);
}
