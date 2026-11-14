use serde_json::{Value, json};
#[test]
fn verifier_copies_count_once_and_unknown_dispositions_stay_unknown()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_fabric_runtime::graph::GraphCheckpoint;
    let finding = |id: &str, disposition: Option<&str>| {
        json!({
            "id":id, "severity":"high", "file":"a.rs", "line":1,
            "summary":"defect", "status":"open", "disposition":disposition,
        })
    };
    let node = |id: &str, findings: Vec<Value>| {
        json!({
            "id":id, "state":"succeeded", "attempts":[], "worktree":null,
            "outputs":{"schema_version":1,"result":null,"verdict":null,"findings":findings,"branch":null,"commit":null,"warnings":[]}
        })
    };
    let mut checkpoint: GraphCheckpoint = serde_json::from_value(json!({
        "schema_version":1,"graph_id":"g","plan_digest":"digest","nodes":[
            node("review", vec![finding("F1", None), finding("F2", None)]),
            node("verify", vec![finding("F1", Some("accepted")), finding("F2", Some("rejected"))]),
            node("copy", vec![finding("F1", Some("accepted")), finding("F2", Some("rejected"))]),
        ]
    }))?;
    assert_eq!(
        super::pipeline_dispositions(Some(&checkpoint), "review"),
        (Some(1), Some(1))
    );
    assert_eq!(
        super::pipeline_dispositions(Some(&checkpoint), "verify"),
        (Some(0), Some(0))
    );
    assert_eq!(super::pipeline_dispositions(None, "review"), (None, None));
    checkpoint.nodes[2]
        .outputs
        .as_mut()
        .ok_or("outputs")?
        .findings[0]
        .disposition = Some("deferred".into());
    assert_eq!(
        super::pipeline_dispositions(Some(&checkpoint), "review"),
        (None, None)
    );
    checkpoint.nodes[2]
        .outputs
        .as_mut()
        .ok_or("outputs")?
        .findings[0]
        .disposition = None;
    assert_eq!(
        super::pipeline_dispositions(Some(&checkpoint), "review"),
        (None, None)
    );
    Ok(())
}

#[test]
fn mixed_model_node_stays_unknown_instead_of_charging_the_final_model()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::{AttemptId, ProjectId, WallTime};
    use provenant_records::{AttemptFact, AttemptOutcome, RouteFact, Usage};
    let mut fact = AttemptFact {
        context: None,
        attempt_id: AttemptId::from_bytes([1; 16]),
        key: provenant_records::WorkKey::new(ProjectId::from_bytes([3; 16]), "graph", "node", 1)?,
        attempt_number: 1,
        project_id: ProjectId::from_bytes([3; 16]),
        terminal_state: AttemptOutcome::Succeeded,
        cause_source: provenant_records::run::CauseSource::Unavailable,
        exit_code: Some(0),
        queued_at: WallTime::from_micros(0)?,
        started_at: None,
        provider_ms: None,
        finished_at: WallTime::from_micros(1)?,
        resolved_route: Some(RouteFact {
            provider: Some("p".into()),
            model: Some("p/a".into()),
            effort: None,
            account_label: None,
        }),
        usage: Usage::default(),
        reviewer: None,
        delivery_id: None,
        subject: None,
        head: None,
        output_refs: vec![],
        requested_route: None,
        applied_route: None,
        observed_route: None,
        verified_owner: None,
        lineage: vec![],
        tier: None,
        template: None,
        parent_node: None,
        slot_wait_ms: None,
        reason: None,
        trace: Default::default(),
        transcript: None,
    };
    let prior = fact.clone();
    assert_eq!(
        super::pipeline_node_model(&[prior.clone(), fact.clone()]).as_deref(),
        Some("p/a")
    );
    fact.applied_route = Some(RouteFact {
        model: Some("p/b".into()),
        ..fact.resolved_route.clone().ok_or("route")?
    });
    assert_eq!(
        super::pipeline_node_model(&[prior.clone(), fact.clone()]),
        None
    );
    fact.applied_route = None;
    fact.resolved_route = None;
    assert_eq!(super::pipeline_node_model(&[prior, fact]), None);
    Ok(())
}
