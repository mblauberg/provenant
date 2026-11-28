use super::*;

fn finding(id: &str, severity: &str, status: &str) -> Finding {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "severity": severity,
        "file": "a.rs",
        "line": 1,
        "summary": "defect",
        "status": status
    }))
    .unwrap()
}

#[test]
fn initial_synthesis_binding_resolves_panel_owner_and_shared_seed_stays_unassigned() {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "graph_id": "synthesis-panel-owner",
        "nodes": [
            {
                "id": "initial-panel",
                "kind": "composite",
                "access": "read_only",
                "composite": {
                    "reduce_verdict": true,
                    "nodes": [{"id":"lens", "route":{"alias":"fixture"}, "access":"read_only", "prompt":"review"}],
                    "exports": [{"node":"lens", "outputs":["findings", "verdict"]}]
                }
            },
            {"route":{"alias":"fixture"}, 
                "id": "initial-synthesis",
                "access": "read_only",
                "prompt": "synthesise the panel",
                "depends_on": ["initial-panel"],
                "inputs": [{"node":"initial-panel", "outputs":["findings", "verdict"]}]
            },
            {"id":"cycle-a", "route":{"alias":"fixture"}, "access":"read_only", "prompt":"repair"}
        ]
    }))
    .unwrap();
    let limits = crate::config::GraphLimits::default();
    let expanded = plans::expand(&document, &[], &[], &limits).unwrap();
    let (mut plan, _) = validate_flat_plan(&expanded, &limits).unwrap();
    let cycle = plan
        .nodes
        .iter_mut()
        .find(|node| node.id == "cycle-a")
        .unwrap();
    cycle.loop_body = Some(
        serde_json::from_value(serde_json::json!({
            "body": [], "until": "accepted", "max_rounds": 2,
            "previous_bindings": {"review": "initial-synthesis"}
        }))
        .unwrap(),
    );
    let panel = plan
        .nodes
        .iter()
        .position(|node| node.id == "initial-panel")
        .unwrap();
    assert_eq!(chain_owner_id(panel, &plan), "cycle-a");

    let mut second_cycle = plan.nodes[plan
        .nodes
        .iter()
        .position(|node| node.id == "cycle-a")
        .unwrap()]
    .clone();
    second_cycle.id = "cycle-b".into();
    plan.nodes.push(second_cycle);
    assert_eq!(
        chain_owner_id(panel, &plan),
        "initial-panel",
        "a shared initial panel remains a standalone seed for both consumers"
    );
    let mut checkpoint = transitions::initial_checkpoint(
        &document,
        &plan,
        "synthesis-panel-owner".into(),
        None,
        None,
        None,
    );
    let mut seed = FindingLedger::default();
    record_head(
        &mut seed,
        "1".repeat(40).as_str(),
        &[finding("F1", "high", "open")],
        BTreeMap::new(),
        BTreeSet::from(["correctness".into()]),
        BTreeMap::new(),
    );
    let seed_row = checkpoint
        .nodes
        .iter_mut()
        .find(|progress| progress.id == "initial-panel")
        .unwrap();
    seed_row.finding_ledger = Some(seed.clone());
    assert_eq!(
        initial_chain_ledger("cycle-a", &plan, &checkpoint),
        Some(seed.clone())
    );
    assert_eq!(
        initial_chain_ledger("cycle-b", &plan, &checkpoint),
        Some(seed)
    );
}

#[test]
fn severity_aliases_decode() {
    assert_eq!(
        model::decode_severity("critical"),
        (Severity::Blocking, false)
    );
    assert_eq!(
        model::decode_severity("blocker"),
        (Severity::Blocking, false)
    );
    assert_eq!(
        model::decode_severity("blocking"),
        (Severity::Blocking, false)
    );
    assert_eq!(model::decode_severity("major"), (Severity::High, false));
    assert_eq!(model::decode_severity("high"), (Severity::High, false));
    assert_eq!(model::decode_severity("medium"), (Severity::Medium, false));
    assert_eq!(
        model::decode_severity("moderate"),
        (Severity::Medium, false)
    );
    assert_eq!(model::decode_severity("minor"), (Severity::Low, false));
    assert_eq!(model::decode_severity("low"), (Severity::Low, false));
    assert_eq!(model::decode_severity("nit"), (Severity::Nit, false));
    assert_eq!(model::decode_severity("info"), (Severity::Nit, false));
    assert_eq!(model::decode_severity("style"), (Severity::Nit, false));
    assert_eq!(model::decode_severity("  HIGH  "), (Severity::High, false));
    assert_eq!(
        model::decode_severity("catastrophic"),
        (Severity::High, true)
    );
    assert_eq!(
        FINDING_SEVERITY_UNKNOWN.as_str(),
        "FINDING_SEVERITY_UNKNOWN"
    );
}

#[test]
fn accept_below_respects_blocking_class() {
    let high = finding("F1", "high", "open");
    assert!(!blocks_threshold(&high, AcceptBelow::Blocking));
    assert!(blocks_threshold(&high, AcceptBelow::High));
    assert_eq!(
        acceptance_decision(std::slice::from_ref(&high), AcceptBelow::Blocking, true),
        Acceptance::Accept
    );
    assert_eq!(
        acceptance_decision(std::slice::from_ref(&high), AcceptBelow::High, true),
        Acceptance::Reject("finding F1 severity High is not below High".into())
    );
    let boundary: Finding = serde_json::from_value(serde_json::json!({
        "id":"F2", "severity":"nit", "file":"a.rs", "summary":"defect", "status":"open",
        "triage": {
            "decision":"confirmed", "reason":"r", "worth_fixing":true,
            "root_cause":"c", "blocking_class":"boundary"
        }
    }))
    .unwrap();
    assert!(blocks_threshold(&boundary, AcceptBelow::Blocking));
    assert!(matches!(
        acceptance_decision(std::slice::from_ref(&boundary), AcceptBelow::Blocking, true),
        Acceptance::Reject(_)
    ));
    let mut closed_boundary = boundary.clone();
    closed_boundary.status = "closed".into();
    closed_boundary.disposition = Some("accepted".into());
    assert!(blocks_threshold(&closed_boundary, AcceptBelow::Blocking));
    assert!(matches!(
        acceptance_decision(&[closed_boundary], AcceptBelow::Blocking, true),
        Acceptance::Reject(_)
    ));
    let mut falsified_boundary = boundary.clone();
    falsified_boundary.verification = Some("falsified".into());
    falsified_boundary.evidence = Some("counterexample disproved by test".into());
    assert!(!blocks_threshold(
        &falsified_boundary,
        AcceptBelow::Blocking
    ));
    assert_eq!(
        acceptance_decision(&[falsified_boundary], AcceptBelow::Blocking, true),
        Acceptance::Accept
    );
    assert!(blocks_threshold(
        &finding("F-off", "nit", "open"),
        AcceptBelow::Off
    ));
    let mut no_trigger = finding("F3", "nit", "open");
    no_trigger.triage = Some(FindingTriage {
        decision: "confirmed".into(),
        reason: "r".into(),
        worth_fixing: true,
        root_cause: "c".into(),
        blocking_class: "acceptance".into(),
        shared_flaw: false,
    });
    let mut closed = no_trigger.clone();
    closed.status = "closed".into();
    assert!(!blocks_threshold(&closed, AcceptBelow::Off));
    assert!(!repair::blocks_acceptance_for(&closed, AcceptBelow::Off));
    assert_eq!(
        acceptance_decision(&[closed], AcceptBelow::Off, true),
        Acceptance::Accept
    );
    assert!(matches!(
        acceptance_decision(&[no_trigger], AcceptBelow::Blocking, true),
        Acceptance::Reject(_)
    ));
    assert!(matches!(
        acceptance_decision(&[], AcceptBelow::Off, true),
        Acceptance::Accept
    ));
    assert!(matches!(
        acceptance_decision(&[], AcceptBelow::High, false),
        Acceptance::Reject(_)
    ));
}

#[test]
fn delta_round_carries_open_findings() {
    let mut ledger = FindingLedger::default();
    let mut open = finding("F1", "high", "open");
    open.lens = Some("correctness".into());
    let fixed = finding("F2", "low", "fixed");
    record_head(
        &mut ledger,
        "aaa",
        &[open.clone(), fixed],
        BTreeMap::new(),
        BTreeSet::from(["correctness".to_owned()]),
        BTreeMap::new(),
    );
    assert_eq!(ledger.last_reviewed_head.as_deref(), Some("aaa"));
    assert_eq!(carried_findings(&ledger, "aaa"), vec![open]);
    let scope = round_scope(&ledger, "bbb", "base1", Some("base1"), RoundMode::Delta);
    assert_eq!(
        scope,
        RoundScope::Delta {
            base: "aaa".into(),
            head: "bbb".into()
        }
    );
}

#[test]
fn delta_widens_for_unattributable_findings_or_uncovered_invariants() {
    let open = finding("F1", "high", "open");
    let mut ledger = FindingLedger::default();
    record_head(
        &mut ledger,
        "aaa",
        std::slice::from_ref(&open),
        BTreeMap::new(),
        BTreeSet::new(),
        BTreeMap::new(),
    );
    assert_eq!(
        round_scope(&ledger, "bbb", "base", Some("base"), RoundMode::Delta),
        RoundScope::Full { head: "bbb".into() }
    );

    let mut attributable = open;
    attributable.lens = Some("correctness".into());
    attributable.class = Some("invariant-a".into());
    record_head(
        &mut ledger,
        "aaa",
        &[attributable],
        BTreeMap::new(),
        BTreeSet::from(["correctness".into()]),
        BTreeMap::new(),
    );
    assert_eq!(
        round_scope(&ledger, "bbb", "base", Some("base"), RoundMode::Delta),
        RoundScope::Full { head: "bbb".into() }
    );

    // An invariant still has a usable prior lens, so one unrelated
    // unavailable lens does not widen every later round.
    ledger
        .heads
        .get_mut("aaa")
        .unwrap()
        .invariant_coverage
        .entry("invariant-a".into())
        .or_default()
        .insert("tests".into());
    assert_eq!(
        round_scope(&ledger, "bbb", "base", Some("base"), RoundMode::Delta),
        RoundScope::Delta {
            base: "aaa".into(),
            head: "bbb".into()
        }
    );
    // Without a pinned prior base the old delta cannot be attributed.
    assert_eq!(
        round_scope(&ledger, "bbb", "base", None, RoundMode::Delta),
        RoundScope::Full { head: "bbb".into() }
    );

    let entry = ledger.heads.get_mut("aaa").unwrap();
    entry
        .invariant_coverage
        .insert("invariant-a".into(), BTreeSet::from(["correctness".into()]));
    entry.unavailable_lenses.insert("correctness".into());
    assert_eq!(
        round_scope(&ledger, "bbb", "base", Some("base"), RoundMode::Delta),
        RoundScope::Full { head: "bbb".into() }
    );
}

#[test]
fn delta_round_is_valid_when_no_findings_remain_open() {
    let mut ledger = FindingLedger::default();
    record_head(
        &mut ledger,
        "aaa",
        &[finding("F1", "low", "fixed")],
        BTreeMap::new(),
        BTreeSet::from(["correctness".into()]),
        BTreeMap::new(),
    );
    assert_eq!(
        round_scope(&ledger, "bbb", "base", Some("base"), RoundMode::Delta),
        RoundScope::Delta {
            base: "aaa".into(),
            head: "bbb".into()
        }
    );
}

#[test]
fn base_move_widens_to_full() {
    let mut ledger = FindingLedger::default();
    record_head(
        &mut ledger,
        "aaa",
        &[finding("F1", "high", "open")],
        BTreeMap::new(),
        BTreeSet::new(),
        BTreeMap::new(),
    );
    let moved = round_scope(&ledger, "bbb", "base2", Some("base1"), RoundMode::Delta);
    assert_eq!(moved, RoundScope::Full { head: "bbb".into() });
    let explicit = round_scope(&ledger, "bbb", "base1", Some("base1"), RoundMode::Full);
    assert_eq!(explicit, RoundScope::Full { head: "bbb".into() });
}

#[test]
fn sole_unavailable_lens_leaves_invariant_uncovered() {
    let gaps = coverage_gaps(
        &BTreeSet::new(),
        &["correctness".to_owned()],
        &BTreeMap::new(),
        &["invariant-A".to_owned()],
    );
    assert!(gaps.iter().any(|gap| gap.contains("invariant-A")));
    assert!(gaps.iter().any(|gap| gap.contains("correctness")));
    // An uncovered invariant is INCONCLUSIVE, never an agreed PASS.
    assert!(!gaps.is_empty());
}

#[test]
fn contradictory_blocking_findings_split_even_with_matching_verdicts() {
    let mut confirmed = finding("a:F1", "high", "open");
    confirmed.class = Some("custody".into());
    confirmed.triage = Some(FindingTriage {
        decision: "confirmed".into(),
        reason: "observed".into(),
        worth_fixing: true,
        root_cause: "race".into(),
        blocking_class: "boundary".into(),
        shared_flaw: false,
    });
    let mut rejected = confirmed.clone();
    rejected.id = "b:F1".into();
    rejected.triage.as_mut().unwrap().decision = "rejected".into();
    assert!(contradictory_blocking_findings(&[
        confirmed.clone(),
        rejected.clone()
    ]));
    confirmed.triage.as_mut().unwrap().blocking_class = "none".into();
    rejected.triage.as_mut().unwrap().blocking_class = "none".into();
    assert!(contradictory_blocking_findings(&[confirmed, rejected]));
}

#[test]
fn split_goes_to_one_adjudicator_no_round() {
    use provenant_records::workflow::ReviewVerdict;
    assert!(split_needs_adjudicator(&[
        ReviewVerdict::Approve,
        ReviewVerdict::RequestChanges
    ]));
    assert!(!split_needs_adjudicator(&[
        ReviewVerdict::Approve,
        ReviewVerdict::Approve
    ]));
    assert!(split_needs_adjudicator(&[
        ReviewVerdict::Approve,
        ReviewVerdict::Inconclusive
    ]));
    assert!(split_needs_adjudicator(&[
        ReviewVerdict::ApproveWithNits,
        ReviewVerdict::RequestChanges
    ]));
    assert!(split_needs_adjudicator(&[
        ReviewVerdict::Inconclusive,
        ReviewVerdict::RequestChanges
    ]));
    assert!(!split_needs_adjudicator(&[
        ReviewVerdict::Inconclusive,
        ReviewVerdict::Inconclusive
    ]));
    assert!(!split_needs_adjudicator(&[]));
}

#[test]
fn missing_evidence_rereviews_never_repairs() {
    assert_eq!(
        lens_outcome(Some("missing_evidence"), 0, 2, true),
        LensOutcome::Retry
    );
    assert_eq!(
        lens_outcome(Some("missing_evidence"), 2, 2, true),
        LensOutcome::Unavailable
    );
    assert_eq!(
        lens_outcome(Some("blocked_environment"), 0, 1, true),
        LensOutcome::Retry
    );
    assert_eq!(
        lens_outcome(Some("blocked_environment"), 1, 1, true),
        LensOutcome::Unavailable
    );
    assert_eq!(
        lens_outcome(Some("model_unavailable"), 1, 1, false),
        LensOutcome::Unavailable
    );
    assert_eq!(lens_outcome(None, 0, 1, true), LensOutcome::Completed);
}
