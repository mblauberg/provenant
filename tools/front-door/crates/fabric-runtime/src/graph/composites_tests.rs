use super::*;

#[test]
fn covered_review_pass_survives_replay() {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 2, "graph_id": "covered-review", "nodes": [{
            "id": "panel", "kind": "composite", "access": "read_only",
            "composite": {
                "reduce_verdict": true, "required_review_lenses": ["correctness"],
                "nodes": [{"id": "lens", "access": "read_only", "prompt": "review",
                    "review_behaviour": "lens", "review_lens": "correctness",
                    "route": {"alias": "fixture"}}],
                "exports": [{"node": "lens", "outputs": ["verdict", "findings"]}]
            }
        }]
    }))
    .unwrap();
    let limits = crate::config::GraphLimits::default();
    let expanded = plans::expand(&document, &[], &[], &limits).unwrap();
    let (plan, positions) = validate_flat_plan(&expanded, &limits).unwrap();
    let mut checkpoint =
        transitions::initial_checkpoint(&expanded, &plan, "digest".into(), None, None, None);
    let boundary = positions["panel"];
    let spec = plan.nodes[boundary].composite.as_ref().unwrap();
    let lens = positions[&spec.exports[0].node];
    let mut member = outputs::make_outputs(None, None, None, None, None);
    member.verdict = Some("VERDICT: APPROVE".into());
    member.findings_available = Some(true);
    checkpoint.nodes[lens].outputs = Some(member);
    checkpoint.nodes[lens].state = NodeState::Succeeded;
    checkpoint.nodes[lens].review_settlement = Some(ReviewSettlement {
        attempt_id: provenant_core::AttemptId::from_bytes([1; 16]),
        head: provenant_core::GitOid::parse(&"1".repeat(40)).unwrap(),
        base: None,
        verdict: provenant_records::workflow::ReviewVerdict::Approve,
        independent: true,
    });
    let (first, _, _) = outputs(boundary, spec, &plan, &checkpoint, &positions).unwrap();
    assert_eq!(first.verdict.as_deref(), Some("VERDICT: APPROVE"));
    assert!(
        !first
            .warnings
            .iter()
            .any(|warning| warning.contains("coverage_gaps"))
    );
    checkpoint.nodes[boundary].outputs = Some(first.clone());
    let restored: GraphCheckpoint =
        serde_json::from_value(serde_json::to_value(&checkpoint).unwrap()).unwrap();
    let (replayed, _, _) = outputs(boundary, spec, &plan, &restored, &positions).unwrap();
    assert_eq!(replayed, first);
}

#[test]
fn ledger_is_chain_scoped_materialises_overflow_and_uses_adjudicator() {
    let panel = |id: &str| {
        serde_json::json!({
            "id": id,
            "kind": "composite",
            "access": "read_only",
            "composite": {
                "reduce_verdict": true,
                "nodes": [{
                    "id": "lens", "access": "read_only", "prompt": "review",
                    "review_behaviour": "lens", "review_lens": "correctness",
                    "route": {"alias": "fixture"}
                }],
                "exports": [{"node": "lens", "outputs": ["verdict", "findings"]}]
            }
        })
    };
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 2,
        "graph_id": "review-ledger-chain",
        "nodes": [
            panel("initial"),
            panel("current"),
            panel("other-chain"),
            panel("patch-skipped")
        ]
    }))
    .unwrap();
    let limits = crate::config::GraphLimits::default();
    let expanded = plans::expand(&document, &[], &[], &limits).unwrap();
    let (mut plan, mut positions) = validate_flat_plan(&expanded, &limits).unwrap();
    let mut checkpoint =
        transitions::initial_checkpoint(&expanded, &plan, "digest".into(), None, None, None);
    let initial_head = "1".repeat(40);
    let current_head = "2".repeat(40);
    let finding = |id: String, disposition: Option<&str>| {
        serde_json::from_value::<Finding>(serde_json::json!({
            "id": id,
            "severity": "high",
            "file": "src/review.rs",
            "line": 1,
            "summary": "open finding",
            "status": "open",
            "disposition": disposition
        }))
        .unwrap()
    };
    let set_review = |checkpoint: &mut GraphCheckpoint,
                      index: usize,
                      head: &str,
                      findings: Vec<Finding>,
                      attempt: u8| {
        let progress = &mut checkpoint.nodes[index];
        let mut output = outputs::make_outputs(None, None, None, None, None);
        output.verdict = Some("VERDICT: FAIL".into());
        output.findings_available = Some(true);
        output.findings = findings.iter().take(1).cloned().collect();
        output.full_artifact_findings = Some(findings);
        progress.outputs = Some(output);
        progress.state = NodeState::Succeeded;
        progress.review_settlement = Some(ReviewSettlement {
            attempt_id: provenant_core::AttemptId::from_bytes([attempt; 16]),
            head: provenant_core::GitOid::parse(head).unwrap(),
            base: None,
            verdict: provenant_records::workflow::ReviewVerdict::RequestChanges,
            independent: true,
        });
    };
    let initial_boundary = positions["initial"];
    let initial_lens = positions["initial--lens"];
    let current_boundary = positions["current"];
    let current_lens = positions["current--lens"];
    let unrelated_lens = positions["other-chain--lens"];
    let skipped_boundary = positions["patch-skipped"];
    set_review(
        &mut checkpoint,
        initial_lens,
        &initial_head,
        vec![finding("initial:F1".into(), None)],
        1,
    );
    let full_current: Vec<_> = (0..40)
        .map(|index| finding(format!("current:F{index}"), None))
        .collect();
    set_review(
        &mut checkpoint,
        current_lens,
        &current_head,
        full_current.clone(),
        2,
    );
    set_review(
        &mut checkpoint,
        unrelated_lens,
        &current_head,
        vec![finding("other:F1".into(), None)],
        3,
    );
    checkpoint.nodes[skipped_boundary].state = NodeState::Skipped;
    checkpoint.nodes[skipped_boundary].skip_reason = Some("skipped by patch revision 7".into());
    checkpoint.nodes[skipped_boundary].outputs = Some(NodeOutputs {
        verdict: Some("PATCH SKIP SENTINEL".into()),
        ..Default::default()
    });

    // This synthetic owner row expresses the production repeat bindings:
    // the first round consumes the initial panel, later rows use loop_id.
    let loop_round = LoopRound {
        extra: Default::default(),
        loop_id: "cycle-a".into(),
        round: 1,
        node_id: "review".into(),
        until: false,
        retry_on: false,
        previous_gate: None,
    };
    plan.nodes[current_boundary].round = Some(loop_round.clone());
    plan.nodes[current_lens].round = Some(loop_round);
    let mut cycle = plan.nodes[current_boundary].clone();
    cycle.id = "cycle-a".into();
    cycle.composite = None;
    cycle.composite_members.clear();
    cycle.loop_body = Some(
        serde_json::from_value(serde_json::json!({
            "body": [], "until": "accepted", "max_rounds": 3,
            "previous_bindings": {"review": "initial"}
        }))
        .unwrap(),
    );
    let cycle_index = plan.nodes.len();
    positions.insert(cycle.id.clone(), cycle_index);
    plan.nodes.push(cycle);
    let mut cycle_progress = checkpoint.nodes[current_boundary].clone();
    cycle_progress.id = "cycle-a".into();
    cycle_progress.state = NodeState::Pending;
    cycle_progress.outputs = None;
    cycle_progress.review_settlement = None;
    cycle_progress.children.clear();
    checkpoint.nodes.push(cycle_progress);

    // The panel parent is a composite boundary with no settlement of its
    // own. The adjudicator is present only in the expansion record.
    let mut adjudicator = plan.nodes[current_lens].clone();
    adjudicator.id = "current--adjudicate".into();
    adjudicator.kind = Some(NodeKind::Adjudicate);
    adjudicator.parent_node = Some("current".into());
    adjudicator.review_lens = None;
    adjudicator.round = None;
    adjudicator.inputs = serde_json::from_value(serde_json::json!([{
        "node":"current", "outputs":["findings", "verdict"]
    }]))
    .unwrap();
    adjudicator.source = Some(SourceBinding::Commit {
        commit: current_head.clone(),
    });
    let adjudicator_index = plan.nodes.len();
    positions.insert(adjudicator.id.clone(), adjudicator_index);
    plan.nodes.push(adjudicator.clone());
    let mut adjudicator_progress = checkpoint.nodes[current_lens].clone();
    adjudicator_progress.id.clone_from(&adjudicator.id);
    adjudicator_progress.review_settlement = None;
    adjudicator_progress.round = None;
    let mut adjudication = outputs::make_outputs(None, None, None, None, None);
    adjudication.verdict = Some("VERDICT: PASS".into());
    adjudication.findings_available = Some(true);
    let mut adjudicated_findings = full_current;
    adjudicated_findings[0].disposition = Some("rejected".into());
    adjudication.findings = adjudicated_findings.iter().take(1).cloned().collect();
    adjudication.full_artifact_findings = Some(adjudicated_findings);
    adjudicator_progress.outputs = Some(adjudication);
    adjudicator_progress.state = NodeState::Succeeded;
    checkpoint.nodes.push(adjudicator_progress);
    let expansion_node: GraphNode = serde_json::from_value(serde_json::json!({
        "id": adjudicator.id,
        "kind": "adjudicate",
        "access": "read_only",
        "parent_node": "current",
        "inputs": [{"node":"current", "outputs":["findings", "verdict"]}],
        "source": {"type": "commit", "commit": current_head}
    }))
    .unwrap();
    checkpoint.expansions.push(Expansion {
        extra: Default::default(),
        delivery_review_disposition: None,
        delivery_review_node: None,
        retired: None,
        retry_generation: None,
        final_result: false,
        final_fragment: false,
        floor_warnings: Vec::new(),
        source: "current".into(),
        nodes: vec![expansion_node],
        uses: Vec::new(),
        patch: None,
    });
    checkpoint.expansions.push(Expansion {
        extra: Default::default(),
        delivery_review_disposition: None,
        delivery_review_node: None,
        retired: None,
        retry_generation: None,
        final_result: false,
        final_fragment: false,
        floor_warnings: Vec::new(),
        source: "patch-skipped".into(),
        nodes: Vec::new(),
        uses: Vec::new(),
        patch: Some(patch::PatchRecord {
            extra: Default::default(),
            child_receipt: None,
            revision: 7,
            digest: "skip-digest".into(),
            key: None,
            parent: Some("patch-skipped".into()),
            reason: Some("test patch skip".into()),
            skipped: vec!["patch-skipped".into()],
            replaced: Vec::new(),
            journaled: true,
            restart: None,
        }),
    });

    let current_spec = plan.nodes[current_boundary].composite.as_ref().unwrap();
    let (projected, _, persisted) = outputs(
        current_boundary,
        current_spec,
        &plan,
        &checkpoint,
        &positions,
    )
    .unwrap();
    checkpoint.nodes[current_boundary].outputs = Some(projected.clone());
    checkpoint.nodes[current_boundary].state = NodeState::Succeeded;
    assert_eq!(
        persisted.as_ref().map(|(owner, _)| owner.as_str()),
        Some("cycle-a")
    );
    assert!(
        projected
            .warnings
            .iter()
            .any(|warning| warning.contains("composite findings truncated"))
    );
    let ledger =
        review::ledger_from_checkpoint(&plan, &checkpoint, current_boundary, &current_head);
    assert!(ledger.heads.contains_key(&initial_head));
    let current = &ledger.heads[&current_head];
    assert_eq!(current.unresolved.len(), 39);
    assert!(
        current
            .unresolved
            .iter()
            .all(|finding| finding.id != "current:F0")
    );
    assert!(
        current
            .unresolved
            .iter()
            .all(|finding| finding.id != "other:F1")
    );
    assert_eq!(
        current
            .dispositions
            .get("adjudicator_verdict:current")
            .map(String::as_str),
        Some("APPROVE")
    );
    assert_eq!(
        current
            .dispositions
            .get("finding_disposition:current:F0")
            .map(String::as_str),
        Some("rejected")
    );
    assert_eq!(ledger.heads[&initial_head].unresolved[0].id, "initial:F1");
    assert_eq!(positions["initial"], initial_boundary);

    // The production reconciler commits the complete head ledger on the
    // loop owner in the same checkpoint as its composite output.
    reconcile_checkpoint(&plan, &mut checkpoint);
    let stored_ledger = checkpoint.nodes[cycle_index]
        .finding_ledger
        .as_ref()
        .expect("reconciliation persists the chain ledger");
    assert_eq!(
        Some(stored_ledger),
        persisted.as_ref().map(|(_, projected)| projected),
        "reconciliation stores the ledger the composite projection committed"
    );
    assert_eq!(
        stored_ledger.heads[&current_head].unresolved, ledger.heads[&current_head].unresolved,
        "the committed head carries the reconstructed unresolved findings"
    );
    assert_eq!(
        repair::ledger_findings(&plan.nodes[current_boundary], &plan, &checkpoint)
            .unwrap()
            .len(),
        39,
        "repair reads the adjudicated unresolved head from the chain ledger"
    );
    let skipped_snapshot = checkpoint.nodes[skipped_boundary].clone();
    assert_eq!(skipped_snapshot.state, NodeState::Skipped);
    assert!(patch::skipped_by_patch(&checkpoint, "patch-skipped"));
    let unrelated_boundary = positions["other-chain"];
    let unrelated_snapshot = checkpoint.nodes[unrelated_boundary].clone();

    // The typed ledger string is externalised by the existing prompt store
    // and restored without losing the full per-finding evidence snapshot.
    let directory_root = tempfile::tempdir().unwrap();
    let directory = provenant_os::fs::Directory::open_anchor(directory_root.path()).unwrap();
    let mut stored = serde_json::to_value(&checkpoint).unwrap();
    prompt_store::externalise(&directory, "review-ledger-test", &mut stored).unwrap();
    assert!(
        stored["stored_text"]
            .as_object()
            .unwrap()
            .keys()
            .any(|path| path.contains("finding_ledger"))
    );
    let bytes = serde_json::to_vec(&stored).unwrap();
    let restored = prompt_store::rehydrate(&directory, "review-ledger-test", &bytes).unwrap();
    let restored: GraphCheckpoint = serde_json::from_slice(&restored).unwrap();
    assert_eq!(
        restored.nodes[cycle_index].finding_ledger,
        checkpoint.nodes[cycle_index].finding_ledger
    );

    // Replaying an older composite cannot move the loop owner's reviewed
    // head backwards, mutate an unrelated settled panel, or clear a patch
    // skip that remains authoritative.
    let newer_head = "3".repeat(40);
    let mut newer_ledger = stored_ledger.clone();
    newer_ledger.heads.insert(
        newer_head.clone(),
        FindingHead {
            round: 2,
            unresolved: vec![finding("current:newer".into(), None)],
            ..Default::default()
        },
    );
    newer_ledger.last_reviewed_head = Some(newer_head.clone());
    checkpoint.nodes[cycle_index].finding_ledger = Some(newer_ledger.clone());
    reconcile_checkpoint(&plan, &mut checkpoint);
    assert_eq!(
        checkpoint.nodes[cycle_index].finding_ledger.as_ref(),
        Some(&newer_ledger)
    );
    assert_eq!(checkpoint.nodes[unrelated_boundary], unrelated_snapshot);
    assert_eq!(checkpoint.nodes[skipped_boundary], skipped_snapshot);

    // A retry replaces the source node output. Reconciliation must retain
    // the owner ledger snapshot so repair can still consume the completed
    // review evidence after replay.
    checkpoint.nodes[current_lens].outputs = None;
    checkpoint.nodes[current_lens].review_settlement = None;
    checkpoint.nodes[current_lens].state = NodeState::Pending;
    reconcile_checkpoint(&plan, &mut checkpoint);
    assert_eq!(
        checkpoint.nodes[cycle_index].finding_ledger.as_ref(),
        Some(&newer_ledger)
    );
    assert_eq!(
        repair::ledger_findings(&plan.nodes[current_boundary], &plan, &checkpoint)
            .unwrap()
            .iter()
            .map(|finding| finding.id.as_str())
            .collect::<Vec<_>>(),
        vec!["current:newer"]
    );
}

#[test]
fn outer_adjudicator_updates_nested_identity_and_keeps_unmatched_for_repair() {
    let lens = |id: &str, name: &str| {
        serde_json::json!({
            "id": id, "access": "read_only", "prompt": "review",
            "review_behaviour": "lens", "review_lens": name,
            "route": {"alias": "fixture"}
        })
    };
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 2,
        "graph_id": "nested-review-ledger",
        "nodes": [{
            "id": "current", "kind": "composite", "access": "read_only",
            "composite": {
                "reduce_verdict": true,
                "nodes": [{
                    "id": "nested", "kind": "composite", "access": "read_only",
                    "composite": {
                        "reduce_verdict": true,
                        "nodes": [lens("lens-a", "correctness"), lens("lens-b", "tests")],
                        "exports": [
                            {"node": "lens-a", "outputs": ["verdict", "findings"]},
                            {"node": "lens-b", "outputs": ["verdict", "findings"]}
                        ]
                    }
                }],
                "exports": [{"node": "nested", "outputs": ["verdict", "findings"]}]
            }
        }]
    }))
    .unwrap();
    let limits = crate::config::GraphLimits::default();
    let expanded = plans::expand(&document, &[], &[], &limits).unwrap();
    let (mut plan, mut positions) = validate_flat_plan(&expanded, &limits).unwrap();
    let mut checkpoint =
        transitions::initial_checkpoint(&expanded, &plan, "digest".into(), None, None, None);
    let head = "4".repeat(40);
    let make_finding = |id: &str, file: &str, summary: &str, severity: &str| {
        serde_json::from_value::<Finding>(serde_json::json!({
            "id": id, "severity": severity, "file": file, "line": 3,
            "summary": summary, "status": "open"
        }))
        .unwrap()
    };
    let blocker = make_finding("F1", "src/a.rs", "nested blocker", "blocking");
    let second_lens = make_finding("F1", "src/b.rs", "same raw ID from another lens", "high");
    let omitted = make_finding("F2", "src/a.rs", "unmatched evidence remains open", "high");
    let set_review =
        |checkpoint: &mut GraphCheckpoint, index: usize, findings: Vec<Finding>, attempt: u8| {
            let progress = &mut checkpoint.nodes[index];
            let mut output = outputs::make_outputs(None, None, None, None, None);
            output.verdict = Some("VERDICT: FAIL".into());
            output.findings_available = Some(true);
            output.full_artifact_findings = Some(findings.clone());
            output.findings = findings;
            progress.outputs = Some(output);
            progress.state = NodeState::Succeeded;
            progress.review_settlement = Some(ReviewSettlement {
                attempt_id: provenant_core::AttemptId::from_bytes([attempt; 16]),
                head: provenant_core::GitOid::parse(&head).unwrap(),
                base: None,
                verdict: provenant_records::workflow::ReviewVerdict::RequestChanges,
                independent: true,
            });
        };
    let leaf_a = positions["current--nested--lens-a"];
    let leaf_b = positions["current--nested--lens-b"];
    let mut first_findings = vec![blocker.clone(), omitted];
    first_findings.extend((3..=42).map(|number| {
        make_finding(
            &format!("F{number}"),
            "src/a.rs",
            &format!("unmatched capped evidence {number}"),
            "high",
        )
    }));
    set_review(&mut checkpoint, leaf_a, first_findings, 1);
    set_review(&mut checkpoint, leaf_b, vec![second_lens], 2);

    let mut adjudicator = plan.nodes[leaf_a].clone();
    adjudicator.id = "current--adjudicate".into();
    adjudicator.kind = Some(NodeKind::Adjudicate);
    adjudicator.parent_node = Some("current".into());
    adjudicator.review_lens = None;
    adjudicator.source = Some(SourceBinding::Commit {
        commit: head.clone(),
    });
    adjudicator.inputs = serde_json::from_value(serde_json::json!([{
        "node":"current", "outputs":["findings", "verdict"]
    }]))
    .unwrap();
    let adjudicator_index = plan.nodes.len();
    positions.insert(adjudicator.id.clone(), adjudicator_index);
    plan.nodes.push(adjudicator.clone());
    let mut progress = checkpoint.nodes[leaf_a].clone();
    progress.id.clone_from(&adjudicator.id);
    progress.outputs = None;
    progress.review_settlement = None;
    progress.state = NodeState::Pending;
    checkpoint.nodes.push(progress);
    checkpoint.expansions.push(Expansion {
        extra: Default::default(),
        delivery_review_disposition: None,
        delivery_review_node: None,
        retired: None,
        retry_generation: None,
        final_result: false,
        final_fragment: false,
        floor_warnings: Vec::new(),
        source: "current".into(),
        nodes: vec![
            serde_json::from_value(serde_json::json!({
                "id": adjudicator.id, "kind": "adjudicate", "access": "read_only",
                "parent_node": "current", "source": {"type":"commit", "commit":head},
                "inputs": [{"node":"current", "outputs":["findings", "verdict"]}]
            }))
            .unwrap(),
        ],
        uses: Vec::new(),
        patch: None,
    });

    // First materialise both composite boundaries so the adjudicator input
    // resolver sees the same qualified IDs as a live panel. The nested
    // aggregate exceeds its projection cap; the adjudicator must account
    // for full member artifacts and preserve every unmatched source row.
    reconcile_checkpoint(&plan, &mut checkpoint);
    let nested_boundary = positions["current--nested"];
    assert_eq!(
        checkpoint.nodes[nested_boundary]
            .outputs
            .as_ref()
            .and_then(|output| output.findings_available),
        Some(false)
    );
    assert!(
        checkpoint.nodes[nested_boundary]
            .outputs
            .as_ref()
            .is_some_and(|output| output
                .warnings
                .iter()
                .any(|warning| warning.contains("composite findings truncated")))
    );
    let mut adjudicated = blocker.clone();
    adjudicated.id = "current--nested--lens-a:F1".into();
    adjudicated.disposition = Some("rejected".into());
    let mut output = outputs::make_outputs(None, None, None, None, None);
    output.verdict = Some("VERDICT: PASS".into());
    output.findings_available = Some(true);
    output.findings = vec![adjudicated.clone()];
    output.full_artifact_findings = Some(vec![adjudicated]);
    checkpoint.nodes[adjudicator_index].outputs = Some(output);
    checkpoint.nodes[adjudicator_index].state = NodeState::Succeeded;
    reconcile_checkpoint(&plan, &mut checkpoint);

    let boundary = positions["current"];
    let ledger = checkpoint.nodes[boundary]
        .finding_ledger
        .as_ref()
        .expect("outer composite persists its chain ledger");
    let reviewed = &ledger.heads[&head];
    assert_eq!(reviewed.findings.len(), 43);
    assert_ne!(reviewed.findings[0].id, reviewed.findings[1].id);
    assert_eq!(reviewed.unresolved.len(), 42);
    assert!(
        reviewed
            .unresolved
            .iter()
            .any(|finding| finding.summary == "same raw ID from another lens")
    );
    assert!(
        reviewed
            .unresolved
            .iter()
            .any(|finding| finding.summary == "unmatched evidence remains open")
    );
    assert_eq!(
        reviewed
            .unresolved
            .iter()
            .filter(|finding| finding.summary.starts_with("unmatched capped evidence"))
            .count(),
        40,
        "all findings hidden by the nested aggregate projection remain available to the ledger"
    );
    assert!(
        reviewed
            .unresolved
            .iter()
            .all(|finding| finding.summary != "nested blocker")
    );
    // Every displayed finding, including the ones the nested aggregate
    // projection hid, keeps a structural origin so its ID stays stable.
    assert_eq!(reviewed.finding_origins.len(), reviewed.findings.len());
    assert!(
        reviewed
            .findings
            .iter()
            .all(|finding| reviewed.finding_origins.contains_key(&finding.id))
    );

    let repair = repair::ledger_findings(&plan.nodes[boundary], &plan, &checkpoint).unwrap();
    assert_eq!(repair.len(), 42);
    assert!(
        repair
            .iter()
            .all(|finding| finding.summary != "nested blocker")
    );
}

#[test]
fn later_collision_keeps_prior_ledger_display_id_for_same_origin() {
    let panel: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 2,
        "graph_id": "review-id-collision-rounds",
        "nodes": [{
            "id":"panel", "kind":"composite", "access":"read_only",
            "composite": {
                "reduce_verdict": true,
                "nodes": [
                    {"id":"lens-a", "access":"read_only", "prompt":"review",
                        "review_behaviour":"lens", "review_lens":"correctness",
                        "route":{"alias":"fixture"}},
                    {"id":"lens-b", "access":"read_only", "prompt":"review",
                        "review_behaviour":"lens", "review_lens":"tests",
                        "route":{"alias":"fixture"}}
                ],
                "exports": [
                    {"node":"lens-a", "outputs":["verdict", "findings"]},
                    {"node":"lens-b", "outputs":["verdict", "findings"]}
                ]
            }
        }]
    }))
    .unwrap();
    let limits = crate::config::GraphLimits::default();
    let expanded = plans::expand(&panel, &[], &[], &limits).unwrap();
    let (plan, positions) = validate_flat_plan(&expanded, &limits).unwrap();
    let mut checkpoint =
        transitions::initial_checkpoint(&expanded, &plan, "digest".into(), None, None, None);
    let head = "5".repeat(40);
    let finding = |file: &str| {
        serde_json::from_value::<Finding>(serde_json::json!({
            "id":"F1", "severity":"high", "file":file, "summary":"duplicate ID",
            "status":"open"
        }))
        .unwrap()
    };
    let set_review = |checkpoint: &mut GraphCheckpoint, index: usize, file: &str, attempt: u8| {
        let mut output = outputs::make_outputs(None, None, None, None, None);
        let finding = finding(file);
        output.verdict = Some("VERDICT: FAIL".into());
        output.findings_available = Some(true);
        output.findings = vec![finding.clone()];
        output.full_artifact_findings = Some(vec![finding]);
        let progress = &mut checkpoint.nodes[index];
        progress.outputs = Some(output);
        progress.state = NodeState::Succeeded;
        progress.review_settlement = Some(ReviewSettlement {
            attempt_id: provenant_core::AttemptId::from_bytes([attempt; 16]),
            head: provenant_core::GitOid::parse(&head).unwrap(),
            base: None,
            verdict: provenant_records::workflow::ReviewVerdict::RequestChanges,
            independent: true,
        });
    };
    let panel_index = positions["panel"];
    let first_lens = positions["panel--lens-a"];
    let second_lens = positions["panel--lens-b"];
    set_review(&mut checkpoint, first_lens, "src/a.rs", 1);
    let first = review::ledger_from_checkpoint(&plan, &checkpoint, panel_index, &head);
    let stored = &first.heads[&head];
    assert_eq!(stored.findings[0].id, "F1");
    assert_eq!(stored.finding_origins["F1"].source, "panel--lens-a");
    checkpoint.nodes[panel_index].finding_ledger = Some(first);

    set_review(&mut checkpoint, second_lens, "src/b.rs", 2);
    let second = review::ledger_from_checkpoint(&plan, &checkpoint, panel_index, &head);
    let stored = &second.heads[&head];
    assert_eq!(stored.finding_origins["F1"].source, "panel--lens-a");
    assert!(stored.findings.iter().any(|finding| finding.id == "F1"));
    assert!(
        stored
            .findings
            .iter()
            .any(|finding| finding.id == "panel--lens-b:F1")
    );
    assert_eq!(stored.finding_origins.len(), 2);
}

#[test]
fn delta_carry_keeps_prior_identity_when_new_head_adds_raw_id_collision() {
    let finding = |id: &str, file: &str, summary: &str| {
        serde_json::from_value::<Finding>(serde_json::json!({
            "id": id,
            "severity": "high",
            "file": file,
            "summary": summary,
            "status": "open"
        }))
        .unwrap()
    };
    let previous_head = "6".repeat(40);
    let current_head = "7".repeat(40);
    let prior = finding("F1", "src/old.rs", "carried source");
    let current = finding("F1", "src/new.rs", "new colliding source");
    let origin = |source: &str| FindingOrigin {
        source: source.into(),
        id: "F1".into(),
    };
    let mut ledger = FindingLedger::default();
    ledger.heads.insert(
        previous_head.clone(),
        FindingHead {
            findings: vec![prior.clone()],
            unresolved: vec![prior],
            finding_origins: BTreeMap::from([("F1".into(), origin("panel--lens-a"))]),
            ..Default::default()
        },
    );
    ledger.heads.insert(
        current_head.clone(),
        FindingHead {
            findings: vec![current],
            unresolved: vec![finding("F1", "src/new.rs", "new colliding source")],
            finding_origins: BTreeMap::from([("F1".into(), origin("panel--lens-b"))]),
            ..Default::default()
        },
    );
    ledger.last_reviewed_head = Some(previous_head.clone());

    let carried = review::carried_findings(&ledger, &previous_head);
    review::merge_carried_findings(&mut ledger, &current_head, &previous_head, &carried);

    let current = &ledger.heads[&current_head];
    assert_eq!(current.unresolved.len(), 2);
    assert_eq!(current.finding_origins["F1"].source, "panel--lens-a");
    assert_eq!(
        current.finding_origins["panel--lens-b:F1"].source,
        "panel--lens-b"
    );
    assert_eq!(
        current
            .unresolved
            .iter()
            .find(|finding| finding.summary == "carried source")
            .map(|finding| finding.id.as_str()),
        Some("F1")
    );
}

#[test]
fn incomplete_retry_reconciliation_keeps_the_persisted_head_snapshot() {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "graph_id": "persisted-review-retry",
        "nodes": [{
            "id": "panel",
            "kind": "composite",
            "access": "read_only",
            "composite": {
                "reduce_verdict": true,
                "min_successful": 1,
                "nodes": [
                    {"id":"lens-a", "route":{"alias":"fixture"}, "access":"read_only", "prompt":"review a",
                     "review_behaviour":"lens", "review_lens":"correctness"},
                    {"id":"lens-b", "route":{"alias":"fixture"}, "access":"read_only", "prompt":"review b",
                     "review_behaviour":"lens", "review_lens":"tests"}
                ],
                "exports": [
                    {"node":"lens-a", "outputs":["findings", "verdict"]},
                    {"node":"lens-b", "outputs":["findings", "verdict"]}
                ]
            }
        }]
    }))
    .unwrap();
    let limits = crate::config::GraphLimits::default();
    let expanded = plans::expand(&document, &[], &[], &limits).unwrap();
    let (plan, positions) = validate_flat_plan(&expanded, &limits).unwrap();
    let mut checkpoint =
        transitions::initial_checkpoint(&expanded, &plan, "digest".into(), None, None, None);
    let boundary = positions["panel"];
    let head = "4".repeat(40);
    let base = provenant_core::GitOid::parse(&"5".repeat(40)).unwrap();
    for (name, attempt, id) in [
        ("panel--lens-a", 1, "lens-a:F1"),
        ("panel--lens-b", 2, "lens-b:F2"),
    ] {
        let index = positions[name];
        let progress = &mut checkpoint.nodes[index];
        progress.state = NodeState::Succeeded;
        let mut output = outputs::make_outputs(None, None, None, None, None);
        output.verdict = Some("VERDICT: FAIL".into());
        output.findings_available = Some(true);
        let finding = serde_json::from_value::<Finding>(serde_json::json!({
            "id": id,
            "severity": "high",
            "file": "src/lib.rs",
            "line": 1,
            "summary": "unresolved review evidence",
            "status": "open"
        }))
        .unwrap();
        output.findings = vec![finding.clone()];
        output.full_artifact_findings = Some(vec![finding]);
        progress.outputs = Some(output);
        progress.review_settlement = Some(ReviewSettlement {
            attempt_id: provenant_core::AttemptId::from_bytes([attempt; 16]),
            head: provenant_core::GitOid::parse(&head).unwrap(),
            base: Some(base),
            verdict: provenant_records::workflow::ReviewVerdict::RequestChanges,
            independent: true,
        });
    }
    reconcile_checkpoint(&plan, &mut checkpoint);
    let persisted = checkpoint.nodes[boundary]
        .finding_ledger
        .clone()
        .expect("completed panel stores its ledger");
    assert_eq!(persisted.heads[&head].unresolved.len(), 2);

    // One retried leaf has replaced its output, while the second leaf still
    // supplies a settlement. The partial reconstruction must not erase the
    // durable finding from the retried leaf.
    let retried = positions["panel--lens-a"];
    checkpoint.nodes[retried].state = NodeState::Failed;
    checkpoint.nodes[retried].outputs = None;
    checkpoint.nodes[retried].review_settlement = None;
    reconcile_checkpoint(&plan, &mut checkpoint);
    assert_eq!(
        checkpoint.nodes[boundary].finding_ledger.as_ref(),
        Some(&persisted)
    );
    assert_eq!(
        repair::ledger_findings(&plan.nodes[boundary], &plan, &checkpoint)
            .unwrap()
            .iter()
            .map(|finding| finding.id.as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["lens-a:F1", "lens-b:F2"])
    );
}

#[test]
fn bounded_findings_reserves_members_then_prefers_severity() {
    let finding = |id: &str, severity: &str| {
        serde_json::from_value::<Finding>(serde_json::json!({
            "id": id, "severity": severity, "file": "a", "summary": "s", "status": "open"
        }))
        .unwrap()
    };
    let mut findings: Vec<_> = (0..40)
        .map(|i| finding(&format!("a:F{i}"), if i < 34 { "high" } else { "low" }))
        .collect();
    findings.push(finding("b:F1", "low"));
    let selected = bounded_findings(findings);
    assert_eq!(selected.len(), MAX_FINDINGS);
    assert!(selected.iter().any(|f| f.id == "b:F1"));
    assert_eq!(selected.iter().filter(|f| f.severity == "high").count(), 31);
    assert!(canonical_len(&selected) <= DISPOSITION_FINDINGS_BYTES);
}
#[test]
fn member_coverage_uses_smaller_entries_when_strongest_do_not_fit() {
    let finding = |id: &str, severity: &str, summary: String| {
        serde_json::from_value::<Finding>(serde_json::json!({
            "id": id, "severity": severity, "file": "a", "summary": summary, "status": "open"
        }))
        .unwrap()
    };
    let mut findings = Vec::new();
    for member in 0..10 {
        findings.push(finding(
            &format!("m{member}:strong"),
            "high",
            "x".repeat(1000),
        ));
        findings.push(finding(&format!("m{member}:small"), "low", "s".into()));
    }
    let kept = bounded_findings(findings);
    assert!(canonical_len(&kept) <= DISPOSITION_FINDINGS_BYTES);
    for member in 0..10 {
        assert!(
            kept.iter()
                .any(|f| f.id.starts_with(&format!("m{member}:")))
        );
    }
}
