// #4106 repro: every non-expansion pause writer yields an active expansion
// pause, so a checkpoint can never be saved with a disagreeing pause pair.

use super::*;
use serde_json::json;

fn pause_owner_planner(max_nodes: usize) -> GraphDocument {
    serde_json::from_value(json!({
        "schema_version":1, "graph_id":"planned", "nodes":[
            {"id":"plan", "kind":"task", "access":"read_only", "prompt":"Slice the work",
             "route":{"alias":"senior"}, "plan":{"kinds":["task", "gate"], "max_nodes":max_nodes}},
            {"id":"after", "access":"read_only", "prompt":"Check all slices", "depends_on":["plan"], "route":{"alias":"senior"}}
        ]
    }))
    .unwrap()
}

fn pause_owner_owner(results: &[&str]) -> FakeOwner {
    FakeOwner {
        observations: results
            .iter()
            .map(|text| terminal_with_result(GraphAttemptOutcome::Succeeded, text))
            .collect(),
        ..Default::default()
    }
}

fn has_active_expansion(checkpoint: &GraphCheckpoint) -> bool {
    checkpoint.nodes.iter().any(|node| {
        matches!(
            node.expansion_cause,
            Some(NodeDecisionCause::PlanExpansionPaused { active: true, .. })
        )
    })
}

#[test]
fn every_pause_writer_sets_owner() {
    // A refused planner expansion leaves an active expansion pause owning the
    // InvalidPlanExpansion graph pause.
    let document = pause_owner_planner(1);
    let mut owner = pause_owner_owner(&["not a fragment"]);
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(
        run.checkpoint.pause_cause(),
        Some(PauseCause::InvalidPlanExpansion)
    );
    assert!(has_active_expansion(&run.checkpoint));

    // A later scheduler pass sees tighter host limits. The structural pause
    // takes ownership; the expansion pause must yield so the saved checkpoint
    // is not quarantined as unreadable (#4106).
    let limits = crate::config::GraphLimits {
        nodes: 1,
        ..Default::default()
    };
    advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &limits,
        &Default::default(),
    )
    .unwrap();
    let saved = journal.0.as_ref().unwrap();
    assert_eq!(saved.pause_cause(), Some(PauseCause::StructuralLimit));
    assert!(!has_active_expansion(saved));

    // The steer handoff refusal writes a Stop pause through the same owner, so
    // it yields an active expansion pause too rather than disagreeing with it.
    let mut checkpoint = saved.clone();
    checkpoint.nodes[0].expansion_pause = Some("expansion".into());
    checkpoint.nodes[0].expansion_cause = Some(NodeDecisionCause::PlanExpansionPaused {
        active: true,
        fallback: false,
        fragment_error: None,
        structural_limit: None,
    });
    assert!(has_active_expansion(&checkpoint));
    checkpoint.set_graph_pause_owner(
        PauseCause::Stop(provenant_records::StopCause::LaunchHandoffRefused),
        "steer successor preparation was refused",
    );
    assert_eq!(
        checkpoint.pause_cause(),
        Some(PauseCause::Stop(
            provenant_records::StopCause::LaunchHandoffRefused
        ))
    );
    assert!(!has_active_expansion(&checkpoint));
}
