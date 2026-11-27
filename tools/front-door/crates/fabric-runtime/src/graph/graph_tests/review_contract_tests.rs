use super::*;
use serde_json::{Value, json};

fn review(name: &str, id: &str) -> GraphNode {
    template::Compiler::new(&template::TemplateLibrary::bundled())
        .compile(&json!({"id":id,"use":name,"route":{"alias":"fixture"}}))
        .unwrap()
}

/// Plan a compiled node the way the graph kernel sees it: the review and
/// repair-writer behaviours used by the contract checks are derived during
/// planning, not present on the raw `GraphNode`.
fn planned(node: GraphNode) -> PlannedNode {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version": 1, "graph_id": "g", "nodes": [node]
    }))
    .unwrap();
    validate_plan(&document).unwrap().0.nodes.remove(0)
}

fn finding() -> Value {
    json!({"id":"F1","severity":"high","file":"a.rs","line":1,
            "summary":"The failing path is reachable","status":"open",
            "evidence":"Input x reaches the panic","proposed_fix":"Handle x",
            "trigger":{"sequence":"Invoke the default operation with x","reach":"default_path","observed":true},
            "triage":{"decision":"confirmed","reason":"Input x reproduces the failure","worth_fixing":true,
                "root_cause":"unchecked input x","blocking_class":"default_path_defect","shared_flaw":false}})
}

fn answer(findings: Value, verdict: &str) -> String {
    format!(
        "```findings\n{}\n```\n{verdict}\n",
        json!({"schema_version":1,"findings":findings})
    )
}

fn panel(topology: &str) -> GraphDocument {
    panel_with_lenses(topology, &["correctness", "tests"])
}

fn panel_with_lenses(topology: &str, lenses: &[&str]) -> GraphDocument {
    let library = template::TemplateLibrary::bundled();
    let resolvers = std::collections::BTreeMap::from([
        (
            "review-panel".into(),
            lenses.iter().map(|lens| (*lens).to_owned()).collect(),
        ),
        (
            "review-verifiers".into(),
            vec!["first".into(), "second".into()],
        ),
        ("review-advice-correctness".into(), vec![String::new()]),
        ("review-advice-tests".into(), vec![String::new()]),
    ]);
    let mut resolvers = resolvers;
    resolvers.extend(super::when_tests::stages(topology));
    for lens in lenses {
        resolvers.insert(format!("review-advice-{lens}"), vec![String::new()]);
    }
    let panel = template::Compiler {
        library: &library,
        resolvers: &resolvers,
    }
    .compile(&json!({"id":"panel","use":"review-panel"}))
    .unwrap();
    graph(vec![panel])
}

fn pinned_review_subject() -> provenant_records::subject::PinnedSubject {
    provenant_records::subject::PinnedSubject {
        extra: Default::default(),
        base: None,
        head: provenant_core::GitOid::parse(&"1".repeat(40)).unwrap(),
        origin: provenant_records::subject::SubjectOrigin::LocalRef {
            name: "main".into(),
        },
        pinned_at: provenant_core::WallTime::from_micros(1).unwrap(),
        policy: provenant_records::subject::SubjectPolicy::Pinned,
        repository: None,
        base_ref: None,
        policy_source: None,
        followed_from: Vec::new(),
    }
}

#[test]
fn every_review_topology_preserves_partial_panel_coverage() {
    for frame in [false, true] {
        for verify in [false, true] {
            for synthesis in [false, true] {
                let topology = format!(
                    "f{}-v{}-s{}",
                    u8::from(frame),
                    u8::from(verify),
                    u8::from(synthesis)
                );
                let document = panel(&topology);
                let mut observations = VecDeque::new();
                if frame {
                    observations.push_back(terminal_with_result(
                        GraphAttemptOutcome::Succeeded,
                        "Pinned subject and acceptance criteria",
                    ));
                }
                let mut note = finding();
                note["severity"] = json!("low");
                note["triage"]["worth_fixing"] = json!(false);
                note["triage"]["blocking_class"] = json!("none");
                observations.push_back(terminal_with_result(
                    GraphAttemptOutcome::Succeeded,
                    &answer(json!([note]), "VERDICT: PASS"),
                ));
                observations.push_back(terminal(GraphAttemptOutcome::UsageLimited));
                if verify {
                    observations.push_back(terminal_with_result(
                        GraphAttemptOutcome::Succeeded,
                        &answer(
                            json!([{
                                "id":"F1","verification":"upheld","reason":"Reproduced"
                            }]),
                            "VERDICT: PASS",
                        ),
                    ));
                    observations.push_back(terminal(GraphAttemptOutcome::UsageLimited));
                }
                if synthesis {
                    observations.push_back(terminal_with_result(GraphAttemptOutcome::Succeeded, &answer(json!([{
                            "id":"F1","disposition":"rejected","reason":"Permitted by the contract"
                        }]), "VERDICT: PASS")));
                }
                let mut owner = FakeOwner {
                    // These members have exhausted their restart allowance;
                    // this test judges final panel coverage, not alias redraw.
                    retry_limit: Some(0),
                    observations,
                    ..Default::default()
                };
                let mut journal = MemoryJournal::default();
                let run = advance(&document, &mut owner, &mut journal).unwrap();
                assert_eq!(run.status, GraphStatus::Complete, "{topology}");
                assert!(
                    owner.observations.is_empty(),
                    "downstream stages must run: {topology}"
                );
                let output = run
                    .checkpoint
                    .nodes
                    .iter()
                    .find(|node| node.id == "panel")
                    .unwrap()
                    .outputs
                    .as_ref()
                    .unwrap();
                assert_eq!(&output.partial, &true, "{topology}");
                assert_eq!(output.findings_available, Some(false), "{topology}");
                assert_eq!(
                    output.verdict.as_deref(),
                    Some("VERDICT: INCONCLUSIVE"),
                    "{topology}"
                );
                assert!(
                    output
                        .findings
                        .iter()
                        .any(|finding| finding.summary == "The failing path is reachable"),
                    "{topology}"
                );
                assert_eq!(
                    advance(&document, &mut owner, &mut journal)
                        .unwrap()
                        .checkpoint,
                    run.checkpoint,
                    "{topology}"
                );
            }
        }
    }
}

#[test]
fn complete_panels_reduce_verifier_verdicts_and_partial_panels_keep_fail() {
    for frame in [false, true] {
        for verdict in ["VERDICT: PASS", "VERDICT: FAIL", "VERDICT: INCONCLUSIVE"] {
            for quota_limited in [false, true] {
                let document = panel(&format!("f{}-v1-s0", u8::from(frame)));
                let mut observations = VecDeque::new();
                if frame {
                    observations
                        .push_back(terminal_with_result(GraphAttemptOutcome::Succeeded, "Frame"));
                }
                observations.push_back(terminal_with_result(
                    GraphAttemptOutcome::Succeeded,
                    &answer(json!([]), "VERDICT: PASS"),
                ));
                observations.push_back(if quota_limited {
                    terminal(GraphAttemptOutcome::UsageLimited)
                } else {
                    terminal_with_result(
                        GraphAttemptOutcome::Succeeded,
                        &answer(json!([]), "VERDICT: PASS"),
                    )
                });
                let findings = if verdict == "VERDICT: FAIL" {
                    let mut new = finding();
                    new["verification"] = json!("new");
                    new["reason"] = json!("Found while testing the boundary");
                    json!([new])
                } else {
                    json!([])
                };
                observations.push_back(terminal_with_result(
                    GraphAttemptOutcome::Succeeded,
                    &answer(findings, verdict),
                ));
                observations.push_back(terminal_with_result(
                    GraphAttemptOutcome::Succeeded,
                    &answer(json!([]), "VERDICT: PASS"),
                ));
                let mut owner = FakeOwner {
                    // These members have exhausted their restart allowance;
                    // this test judges final panel coverage, not alias redraw.
                    retry_limit: Some(0),
                    observations,
                    ..Default::default()
                };
                let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
                let output = run
                    .checkpoint
                    .nodes
                    .iter()
                    .find(|node| node.id == "panel")
                    .unwrap()
                    .outputs
                    .as_ref()
                    .unwrap();
                let expected = if verdict == "VERDICT: FAIL" {
                    "VERDICT: REQUEST_CHANGES"
                } else if quota_limited || verdict == "VERDICT: INCONCLUSIVE" {
                    "VERDICT: INCONCLUSIVE"
                } else {
                    "VERDICT: APPROVE"
                };
                // Reduction uses the canonical review vocabulary at composite boundaries.
                assert_eq!(
                    output.verdict.as_deref(),
                    Some(expected),
                    "{frame} {verdict} {quota_limited}"
                );
                assert_eq!(output.findings_available, Some(!quota_limited));
                assert_eq!(review::partial(output), quota_limited);
                assert!(owner.observations.is_empty());
            }
        }
    }
}

#[test]
fn a_contract_invalid_member_keeps_its_finding_through_verification_and_synthesis() {
    let document = panel("f0-v1-s1");
    let mut owner = FakeOwner { observations: [
            terminal_with_result(GraphAttemptOutcome::Succeeded, &answer(json!([finding(), {}]), "VERDICT: FAIL")),
            terminal_with_result(GraphAttemptOutcome::Succeeded, &answer(json!([]), "VERDICT: PASS")),
            terminal_with_result(GraphAttemptOutcome::Succeeded, &answer(json!([{"id":"F1","verification":"upheld","reason":"Reproduced"}]), "VERDICT: FAIL")),
            terminal_with_result(GraphAttemptOutcome::Succeeded, &answer(json!([{"id":"F1","verification":"upheld","reason":"Independently reproduced"}]), "VERDICT: FAIL")),
            terminal_with_result(GraphAttemptOutcome::Succeeded, &answer(json!([{"id":"F1","disposition":"accepted","reason":"Input x still panics"}]), "VERDICT: FAIL")),
        ].into(), ..Default::default() };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert!(owner.observations.is_empty());
    assert!(
        run.checkpoint
            .nodes
            .iter()
            .find(|node| node.id == "panel")
            .unwrap()
            .outputs
            .is_some(),
        "{:?}",
        run.checkpoint
            .nodes
            .iter()
            .map(|node| (
                &node.id,
                node.state,
                &node.advance_code,
                &node.advance_note
            ))
            .collect::<Vec<_>>()
    );
    let invalid = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id.ends_with("lenses--correctness"))
        .unwrap();
    assert_eq!(invalid.state, NodeState::Failed);
    assert_eq!(
        invalid.advance_code.as_deref(),
        Some("review_findings_invalid")
    );
    let output = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "panel")
        .unwrap()
        .outputs
        .as_ref()
        .unwrap();
    assert!(review::partial(output));
    assert_eq!(output.findings_available, Some(false));
    assert_eq!(output.verdict.as_deref(), Some("VERDICT: REQUEST_CHANGES"));
    assert!(
        output
            .findings
            .iter()
            .any(|finding| finding.disposition.as_deref() == Some("accepted")
                && finding.evidence.as_deref() == Some("Input x reaches the panic"))
    );
    assert!(
        owner
            .prompts
            .iter()
            .skip(2)
            .all(|prompt| prompt.contains("Input x reaches the panic"))
    );
    assert_eq!(
        advance(&document, &mut owner, &mut journal)
            .unwrap()
            .checkpoint,
        run.checkpoint
    );
}

#[test]
fn synthesis_cannot_restore_inconclusive_verifier_evidence() {
    let document = panel("f0-v1-s1");
    let mut owner = FakeOwner {
        observations: [
            "VERDICT: PASS",
            "VERDICT: PASS",
            "VERDICT: INCONCLUSIVE",
            "VERDICT: PASS",
            "VERDICT: PASS",
        ]
        .map(|verdict| terminal_with_result(GraphAttemptOutcome::Succeeded, &answer(json!([]), verdict)))
        .into(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    let output = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "panel")
        .unwrap()
        .outputs
        .as_ref()
        .unwrap();
    assert_eq!(output.verdict.as_deref(), Some("VERDICT: INCONCLUSIVE"));
    assert!(review::partial(output));
    assert!(owner.observations.is_empty());
}

#[test]
fn invalid_propagation_cannot_alias_an_independent_sibling_finding() {
    let document = panel("f0-v0-s1");
    let mut forged = finding();
    forged["propagates"] = json!("tests:F1");
    let mut second = finding();
    second["file"] = json!("b.rs");
    let mut owner = FakeOwner {
        observations: [
            terminal_with_result(
                GraphAttemptOutcome::Succeeded,
                &answer(json!([forged]), "VERDICT: FAIL"),
            ),
            terminal_with_result(
                GraphAttemptOutcome::Succeeded,
                &answer(json!([second]), "VERDICT: FAIL"),
            ),
            terminal_with_result(
                GraphAttemptOutcome::Succeeded,
                &answer(json!([]), "VERDICT: INCONCLUSIVE"),
            ),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    let invalid = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id.ends_with("lenses--correctness"))
        .unwrap();
    assert_eq!(
        invalid.advance_code.as_deref(),
        Some("review_propagation_invalid")
    );
    let synthesis = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id.ends_with("--synthesis"))
        .unwrap();
    assert_eq!(
        synthesis.advance_code.as_deref(),
        Some("disposition_count")
    );
    let findings = &synthesis.outputs.as_ref().unwrap().findings;
    assert_eq!(findings.len(), 2);
    assert_ne!(findings[0].id, findings[1].id);
    assert_ne!(findings[0].file, findings[1].file);
}

#[test]
fn typed_contract_rejects_invalid_verdicts_even_without_a_pass_requirement() {
    for verdict in ["", "VERDICT: BANANA", "VERDICT: PASS\nVERDICT: PASS"] {
        let mut node = review("review.tests", "review");
        // Repair panels remove the PASS requirement, not the output contract.
        node.required_verdict = None;
        let mut owner = FakeOwner {
            observations: [
                terminal_with_result(GraphAttemptOutcome::Succeeded, &answer(json!([]), verdict)),
                terminal_with_result(GraphAttemptOutcome::Succeeded, &answer(json!([]), verdict)),
            ]
            .into(),
            ..Default::default()
        };
        let document = graph(vec![node]);
        let mut journal = MemoryJournal::default();
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        let progress = &run.checkpoint.nodes[0];
        assert_eq!(progress.state, NodeState::Failed, "{verdict}");
        assert_eq!(
            progress.attempts.last().unwrap().outcome,
            Some(GraphAttemptOutcome::TaskFailed)
        );
        assert_eq!(
            progress.advance_code.as_deref(),
            Some("review_verdict_invalid")
        );
        assert_eq!(
            advance(&document, &mut owner, &mut journal)
                .unwrap()
                .checkpoint,
            run.checkpoint
        );
    }
}

// Spec: docs/specs/run-settlement.md#review-verdict-wording
#[test]
fn normalised_review_verdict_settles_with_a_warning() {
    for (line, expected) in [
        ("VERDICT: approve abc123", "VERDICT: PASS"),
        ("Verdict FAIL: reachable panic", "VERDICT: FAIL"),
    ] {
        let mut node = review("review.tests", "review");
        node.required_verdict = None;
        let mut owner = FakeOwner {
            observations: [terminal_with_result(
                GraphAttemptOutcome::Succeeded,
                &answer(json!([]), line),
            )]
            .into(),
            ..Default::default()
        };
        let run = advance(
            &graph(vec![node]),
            &mut owner,
            &mut MemoryJournal::default(),
        )
        .unwrap();
        let progress = &run.checkpoint.nodes[0];
        assert_eq!(progress.state, NodeState::Succeeded);
        let outputs = progress.outputs.as_ref().unwrap();
        assert_eq!(outputs.verdict.as_deref(), Some(expected));
        assert!(
            outputs
                .warnings
                .iter()
                .any(|warning| warning.contains("review_verdict_normalised")
                    && warning.contains(line))
        );
    }
}

#[test]
fn review_contract_accepts_verdict_followed_only_by_handoff() {
    let mut node = review("review.tests", "review");
    node.required_verdict = None;
    let response = format!(
        "{}\nReview completed.\nVERDICT: FAIL\n```handoff\n{{\"schema_version\":2,\"status\":\"done\",\"note\":\"Reviewed\",\"remaining\":[],\"decisions\":[],\"verify\":[]}}\n```\n",
        answer(json!([]), "").trim_end()
    );
    let mut owner = FakeOwner {
        observations: [terminal_with_result(GraphAttemptOutcome::Succeeded, &response)].into(),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![node]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    let progress = &run.checkpoint.nodes[0];
    assert_eq!(progress.state, NodeState::Succeeded);
    assert_eq!(
        progress.outputs.as_ref().unwrap().verdict.as_deref(),
        Some("VERDICT: FAIL")
    );
    assert_eq!(progress.advance_code, None);
}

// Spec: docs/specs/run-settlement.md#settlement
#[test]
fn review_contract_accepts_one_verdict_after_a_valid_closed_handoff() {
    let mut node = review("review.tests", "review");
    node.required_verdict = None;
    // The verdict is captured from the answer on its own. The handoff is the
    // accepted declaration the host sealed, never a fence read from the text.
    let response = format!("{}\nVERDICT: PASS\n", answer(json!([]), "").trim_end());
    let mut capture = crate::result_capture::ResultCapture::new(Some("VERDICT: PASS"));
    capture.push(response.as_bytes());
    let mut result = capture.finish();
    result.handoff = Ok(Some(
        provenant_records::handoff::Handoff::from_json(
            br#"{"schema_version":2,"status":"done","note":"Reviewed","remaining":[],"decisions":[],"verify":[]}"#,
        )
        .unwrap(),
    ));
    let mut owner = FakeOwner {
        observations: [AttemptObservation::Terminal {
            attempt_ref: None,
            outcome: GraphAttemptOutcome::Succeeded,
            launched: true,
            result: Some(Box::new(result)),
            result_warning: None,
            result_path: None,
            branch: None,
            commit: None,
            settlement_class: Some(provenant_records::run::SettlementClass::Completed),
            stop: None,
        cause: None,
        }]
        .into(),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![node]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    let progress = &run.checkpoint.nodes[0];
    assert_eq!(progress.state, NodeState::Succeeded);
    assert_eq!(
        progress.outputs.as_ref().unwrap().verdict.as_deref(),
        Some("VERDICT: PASS")
    );
    assert_eq!(progress.advance_code, None);
}

/// A long review ending in one verdict, a blank line and a pretty-printed
/// handoff block is one verdict. Size alone (past the run store's head
/// window) costs neither complete findings nor the verdict.
#[test]
fn a_long_review_with_a_spaced_handoff_keeps_its_single_verdict() {
    let overlong = format!("Traced {} to its call sites.", "a/b/c ".repeat(400));
    let sections = (1..=40)
        .map(|index| format!("{index}. Checked the boundary of module {index}: nothing found."))
        .collect::<Vec<_>>()
        .join("\n");
    let handoff = serde_json::to_string_pretty(&json!({
        "schema_version": 2, "status": "done", "note": "Reviewed",
        "remaining": [], "decisions": ["Read the diff in full"],
        "verify": ["Ran the focused tests"]
    }))
    .unwrap();
    let response = format!(
        "# Review\n\n{overlong}\n\n{sections}\n\n{}\nVERDICT: PASS\n\n```handoff\n{handoff}\n```\n",
        answer(json!([]), "").trim_end()
    );
    assert!(response.len() > 4096 && overlong.len() > 1024);
    for source_truncated in [false, true] {
        let mut node = review("review.tests", "review");
        node.required_verdict = None;
        let mut result = capture(&response);
        assert_eq!(result.verdict.as_deref(), Some("VERDICT: PASS"));
        assert!(!result.verdict_invalid && result.handoff.is_ok());
        result.source_truncated = source_truncated;
        let mut owner = FakeOwner {
            observations: [AttemptObservation::Terminal {
                attempt_ref: None,
                outcome: GraphAttemptOutcome::Succeeded,
                launched: true,
                result: Some(Box::new(result)),
                result_warning: None,
                result_path: None,
                branch: None,
                commit: None,
                settlement_class: Some(provenant_records::run::SettlementClass::Completed),
                stop: None,
            cause: None,
            }]
            .into(),
            ..Default::default()
        };
        let run = advance(
            &graph(vec![node]),
            &mut owner,
            &mut MemoryJournal::default(),
        )
        .unwrap();
        let progress = &run.checkpoint.nodes[0];
        assert_eq!(
            progress.outputs.as_ref().unwrap().verdict.as_deref(),
            Some("VERDICT: PASS"),
            "{source_truncated}"
        );
        assert_ne!(
            progress.advance_code.as_deref(),
            Some("review_verdict_invalid"),
            "{source_truncated}"
        );
        assert_eq!(
            progress.state == NodeState::Succeeded,
            !source_truncated,
            "{source_truncated}: {:?}",
            progress.advance_code
        );
    }
}

#[test]
fn verifier_requires_a_reasoned_assessment_of_every_finding() {
    for entries in [
        json!([]),
        json!([{"id":"F1"}]),
        json!([{"id":"F1","verification":"maybe","reason":"Unclear"}]),
        json!([{"id":"F1","verification":"upheld","reason":" "}]),
    ] {
        let mut verifier = review("review.verify", "verify");
        verifier.depends_on = vec!["source".into()];
        verifier.inputs =
            vec![serde_json::from_value(json!({"node":"source","outputs":["findings"]})).unwrap()];
        let mut owner = FakeOwner {
            observations: [
                terminal_with_result(
                    GraphAttemptOutcome::Succeeded,
                    &answer(json!([finding()]), "VERDICT: FAIL"),
                ),
                terminal_with_result(
                    GraphAttemptOutcome::Succeeded,
                    &answer(entries, "VERDICT: INCONCLUSIVE"),
                ),
            ]
            .into(),
            ..Default::default()
        };
        let run = advance(
            &graph(vec![node("source", route("fixture")), verifier]),
            &mut owner,
            &mut MemoryJournal::default(),
        )
        .unwrap();
        let progress = run
            .checkpoint
            .nodes
            .iter()
            .find(|node| node.id == "verify")
            .unwrap();
        assert_eq!(progress.state, NodeState::Failed);
        assert_eq!(
            progress.advance_code.as_deref(),
            Some("verification_incomplete")
        );
        assert_eq!(
            progress.outputs.as_ref().unwrap().findings[0]
                .evidence
                .as_deref()
                .unwrap(),
            "Input x reaches the panic"
        );
    }
}

#[test]
fn typed_contract_reports_malformed_findings_and_preserves_usable_entries() {
    let good = finding();
    let mut cases = vec![json!({}), json!(null)];
    for field in [
        "id",
        "severity",
        "file",
        "summary",
        "status",
        "evidence",
        "proposed_fix",
    ] {
        let mut bad = good.clone();
        bad.as_object_mut().unwrap().remove(field);
        cases.push(bad);
        let mut blank = good.clone();
        blank[field] = json!(" ");
        cases.push(blank);
        let mut wrong_type = good.clone();
        wrong_type[field] = json!(42);
        cases.push(wrong_type);
    }
    for mut bad in cases {
        if bad.get("id") == Some(&json!("F1")) {
            bad["id"] = json!("F2");
        }
        let mut owner = FakeOwner {
            observations: [terminal_with_result(
                GraphAttemptOutcome::Succeeded,
                &answer(json!([good, bad]), "VERDICT: FAIL"),
            )]
            .into(),
            ..Default::default()
        };
        let run = advance(
            &graph(vec![review("review.tests", "review")]),
            &mut owner,
            &mut MemoryJournal::default(),
        )
        .unwrap();
        let progress = &run.checkpoint.nodes[0];
        assert_eq!(progress.state, NodeState::Failed);
        assert_eq!(
            progress.advance_code.as_deref(),
            Some("review_findings_invalid"),
            "{bad} {:?}",
            progress.advance_note
        );
        assert!(
            progress
                .outputs
                .as_ref()
                .unwrap()
                .findings
                .iter()
                .any(|finding| finding.id == "F1")
        );
    }
}

#[test]
fn lens_pass_conflicts_with_untriaged_boundary_and_default_path_findings() {
    for class in ["boundary", "data_loss", "none"] {
        let mut item = finding();
        item.as_object_mut().unwrap().remove("triage");
        item["class"] = json!(class);
        if class != "none" {
            item.as_object_mut().unwrap().remove("trigger");
        }
        let document = graph(vec![review("review.tests", "review")]);
        let mut owner = FakeOwner {
            observations: VecDeque::from([terminal_with_result(
                GraphAttemptOutcome::DeclaredStop,
                &answer(json!([item]), "VERDICT: PASS"),
            )]),
            ..Default::default()
        };
        let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
        assert_eq!(
            run.checkpoint.nodes[0].advance_code.as_deref(),
            Some("review_verdict_conflict"),
            "{class}"
        );
    }
}

#[test]
fn typed_contract_distinguishes_valid_fail_from_a_contradictory_pass() {
    let mut accepted = finding();
    accepted["disposition"] = json!("accepted");
    accepted["reason"] = json!("Reproduced");
    for (name, findings) in [
        ("review.tests", json!([finding()])),
        ("review.synthesis", json!([accepted])),
    ] {
        for verdict in ["VERDICT: PASS", "VERDICT: FAIL"] {
            let mut nodes = Vec::new();
            let mut observations = VecDeque::new();
            let mut reviewer = review(name, "review");
            if name == "review.synthesis" {
                nodes.push(node("source", route("fixture")));
                observations.push_back(terminal_with_result(
                    GraphAttemptOutcome::Succeeded,
                    &answer(json!([]), "VERDICT: PASS"),
                ));
                reviewer.depends_on.push("source".into());
                reviewer.inputs.push(
                    serde_json::from_value(json!({"node":"source","outputs":["findings"]}))
                        .unwrap(),
                );
            }
            nodes.push(reviewer);
            observations.push_back(terminal_with_result(
                GraphAttemptOutcome::DeclaredStop,
                &answer(findings.clone(), verdict),
            ));
            let mut owner = FakeOwner {
                observations,
                ..Default::default()
            };
            let run = advance(&graph(nodes), &mut owner, &mut MemoryJournal::default()).unwrap();
            let progress = run
                .checkpoint
                .nodes
                .iter()
                .find(|node| node.id == "review")
                .unwrap();
            if verdict == "VERDICT: PASS" {
                assert_eq!(progress.state, NodeState::Failed);
                assert_eq!(
                    progress.advance_code.as_deref(),
                    Some("review_verdict_conflict")
                );
            } else {
                assert_eq!(progress.state, NodeState::Succeeded);
                assert_eq!(
                    progress.attempts.last().unwrap().outcome,
                    Some(GraphAttemptOutcome::Succeeded)
                );
            }
        }
    }
}

// Spec: docs/specs/agent-native.md#37-review-convergence
#[test]
fn malformed_lens_retries_same_route_with_durable_budget_without_repeating_siblings() {
    let mut document = panel("f0-v0-s0");
    document.subject = Some(pinned_review_subject());
    let (plan, _) = validate_plan(&document).unwrap();
    let lens = plan
        .nodes
        .iter()
        .find(|node| node.review_lens.as_deref() == Some("correctness"))
        .unwrap();
    let lens_id = lens.id.clone();
    let mut owner = FakeOwner {
        observations: [
            terminal_with_result(GraphAttemptOutcome::Succeeded, &answer(json!([finding()]), "")),
            terminal_with_result(
                GraphAttemptOutcome::Succeeded,
                &answer(json!([finding()]), "VERDICT: FAIL"),
            ),
            terminal_with_result(
                GraphAttemptOutcome::Succeeded,
                &answer(json!([]), "VERDICT: PASS"),
            ),
        ]
        .into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert!(owner.observations.is_empty());
    let retried = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == lens_id)
        .unwrap();
    assert_eq!(retried.state, NodeState::Succeeded);
    assert_eq!(retried.previous_attempts.len(), 1);
    assert_eq!(retried.attempts.len(), 1);
    assert_eq!(retried.previous_attempts[0].route, retried.attempts[0].route);
    let retry = retried.review_retry.as_ref().unwrap();
    assert_eq!(retry.evidence_retries.len(), 1);
    assert_eq!(retry.evidence_retries[0].retries, 1);
    assert_eq!(retry.evidence_retries[0].route, retried.attempts[0].route);
    assert!(retry.pending_retry.is_none());
    assert_eq!(
        owner
            .calls
            .iter()
            .filter(|(key, _)| key.node_id == lens_id)
            .count(),
        2
    );
    assert_eq!(owner.prompts.len(), 3, "the sibling reviewer runs once");
    for prompt in &owner.prompts {
        assert!(prompt.contains(super::review::VERDICT_LAST), "{prompt}");
        assert!(!prompt.contains("only automatic verdict correction"));
    }
    assert!(run.checkpoint.expansions.iter().all(|expansion| {
        expansion
            .patch
            .as_ref()
            .and_then(|patch| patch.reason.as_deref())
            .is_none_or(|reason| !reason.starts_with("correct review verdict"))
    }));
    assert_eq!(
        advance(&document, &mut owner, &mut journal)
            .unwrap()
            .checkpoint,
        run.checkpoint
    );
}

#[test]
fn realistic_aggregate_findings_do_not_require_overflow_proof() {
    for count in 5..=8 {
        let document = panel("f0-v1-s0");
        let entries: Vec<_> = (0..count)
            .map(|i| {
                let mut entry = finding();
                entry["id"] = json!(format!("F{i}"));
                entry["evidence"] =
                    json!("A realistic code path reaches an unchecked state. ".repeat(18));
                entry["proposed_fix"] =
                    json!("Validate the state before dispatch and preserve the error. ".repeat(12));
                entry
            })
            .collect();
        let typed: Vec<Finding> = entries
            .iter()
            .cloned()
            .map(|entry| serde_json::from_value(entry).unwrap())
            .collect();
        assert!(canonical_len(&typed) > FINDINGS_BYTES);
        let decisions: Vec<_> = (0..count).map(|i| json!({"id":format!("F{i}"),"verification":"upheld","reason":"Reproduced in the source"})).collect();
        let mut owner = FakeOwner {
            observations: [
                terminal_with_result(
                    GraphAttemptOutcome::Succeeded,
                    &answer(json!(entries[..count / 2]), "VERDICT: FAIL"),
                ),
                terminal_with_result(
                    GraphAttemptOutcome::Succeeded,
                    &answer(json!(entries[count / 2..]), "VERDICT: FAIL"),
                ),
                terminal_with_result(
                    GraphAttemptOutcome::Succeeded,
                    &answer(json!(decisions), "VERDICT: FAIL"),
                ),
                terminal_with_result(
                    GraphAttemptOutcome::Succeeded,
                    &answer(json!(decisions), "VERDICT: FAIL"),
                ),
            ]
            .into(),
            ..Default::default()
        };
        let mut journal = MemoryJournal::default();
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        assert!(owner.observations.is_empty());
        assert!(
            run.checkpoint
                .nodes
                .iter()
                .all(|node| node.advance_code.is_none()),
            "{count}: {:?}",
            run.checkpoint.nodes
        );
        let output = run
            .checkpoint
            .nodes
            .iter()
            .find(|node| node.id == "panel")
            .unwrap()
            .outputs
            .as_ref()
            .unwrap();
        assert!(!output.findings.is_empty());
        let (plan, positions) = validate_plan(&document).unwrap();
        let verifiers: Vec<_> = plan
            .nodes
            .iter()
            .filter(|node| template::review_verifier(node))
            .collect();
        assert!(!verifiers.is_empty());
        for verifier in verifiers {
            let progress = &run.checkpoint.nodes[positions[&verifier.id]];
            assert_eq!(progress.state, NodeState::Succeeded);
            assert_eq!(progress.outputs.as_ref().unwrap().findings.len(), count);
        }
        assert_eq!(
            advance(&document, &mut owner, &mut journal)
                .unwrap()
                .checkpoint,
            run.checkpoint
        );
    }
}

#[test]
fn below_minimum_review_keeps_returned_findings_on_replay() {
    for export_failure in [false, true] {
        let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1,"graph_id":"below-review-minimum","nodes":[{
            "id":"panel","kind":"composite","access":"read_only","composite":{"min_successful":1,"nodes":[{
                "id":"lenses","kind":"composite","access":"read_only",
                "composite":{"min_successful":2,"reduce_verdict":true,
                    "nodes":[review("review.tests","good"),review("review.correctness","missing")],
                    "exports":[{"node":"good","outputs":["findings","result"]},{"node":"missing","outputs":["findings","result"]}]}
            }],"exports":[{"node":"lenses","outputs":["findings","result"]}]}
        }]
    })).unwrap();
        let mut entry = finding();
        if export_failure {
            entry["id"] = json!("x".repeat(outputs::OUTPUT_FIELD_BYTES));
        }
        let mut owner = FakeOwner {
            observations: [
                terminal_with_result(
                    GraphAttemptOutcome::Succeeded,
                    &answer(json!([entry]), "VERDICT: FAIL"),
                ),
                terminal(GraphAttemptOutcome::AdapterUnavailable),
            ]
            .into(),
            ..Default::default()
        };
        let mut journal = MemoryJournal::default();
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        let boundary = run
            .checkpoint
            .nodes
            .iter()
            .find(|node| node.id == "panel")
            .unwrap();
        assert_eq!(
            boundary.advance_code.as_deref(),
            Some("coverage_below_minimum")
        );
        let output = boundary.outputs.as_ref().unwrap();
        assert!(review::partial(output));
        assert_ne!(output.verdict.as_deref(), Some("VERDICT: PASS"));
        if export_failure {
            let lenses = run
                .checkpoint
                .nodes
                .iter()
                .find(|node| node.id == "panel--lenses")
                .unwrap();
            assert!(lenses.outputs.is_none());
            assert_eq!(lenses.state, NodeState::Failed);
            assert!(output.findings.is_empty());
            assert_eq!(output.verdict.as_deref(), Some("VERDICT: INCONCLUSIVE"));
        } else {
            assert_eq!(output.findings.len(), 1);
        }
        assert_eq!(
            advance(&document, &mut owner, &mut journal)
                .unwrap()
                .checkpoint,
            run.checkpoint
        );
    }
}

#[test]
fn pending_evidence_retry_survives_host_validation_refusal_and_replays_once() {
    let mut document = panel_with_lenses("f0-v0-s0", &["correctness"]);
    document.subject = Some(pinned_review_subject());
    let (plan, _) = validate_plan(&document).unwrap();
    let lens = plan
        .nodes
        .iter()
        .find(|node| node.review_lens.as_deref() == Some("correctness"))
        .unwrap();
    let mut owner = FakeOwner {
        observations: [
            terminal_with_result(
                GraphAttemptOutcome::Succeeded,
                &answer(json!([finding()]), ""),
            ),
            terminal_with_result(GraphAttemptOutcome::Succeeded, &answer(json!([]), "VERDICT: PASS")),
        ]
        .into(),
        refuse_review_retry_once: true,
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    assert!(matches!(
        advance(&document, &mut owner, &mut journal),
        Err(GraphError::Run(_))
    ));
    let interrupted = journal.0.as_ref().unwrap();
    let lens = interrupted
        .nodes
        .iter()
        .find(|node| node.id == lens.id)
        .unwrap();
    assert_eq!(lens.state, NodeState::Failed);
    assert_eq!(lens.attempts.len(), 1);
    let retry = lens.review_retry.as_ref().unwrap();
    assert_eq!(retry.evidence_retries[0].retries, 1);
    assert_eq!(retry.pending_retry.as_ref().unwrap().cause, ReviewRetryCause::Evidence);
    assert_eq!(owner.observations.len(), 1, "refused reset must not launch the retry");
    assert_eq!(owner.calls.len(), 1);

    // Reopen the durable pending intent as a fresh host would.
    let saved = serde_json::to_vec(&journal.0).unwrap();
    let mut journal = MemoryJournal(serde_json::from_slice(&saved).unwrap());
    let resumed = advance(&document, &mut owner, &mut journal).unwrap();
    let lens = resumed.checkpoint.nodes.iter().find(|node| node.id == lens.id).unwrap();
    assert_eq!(lens.state, NodeState::Succeeded);
    assert_eq!(lens.previous_attempts.len(), 1);
    assert_eq!(lens.attempts.len(), 1);
    let retry = lens.review_retry.as_ref().unwrap();
    assert_eq!(retry.evidence_retries[0].retries, 1);
    assert!(retry.pending_retry.is_none());
    assert_eq!(owner.calls.len(), 2);
    assert_eq!(owner.calls[0].1, owner.calls[1].1);
    assert!(owner.observations.is_empty());
    assert_eq!(owner.prompts.len(), 2);
}

#[test]
fn maximal_panel_verify_and_synthesis_prompts_fit_default_limits() {
    let document = panel_with_lenses(
        "f0-v1-s1",
        &["correctness", "tests", "security", "design", "operations"],
    );
    let entries: Vec<_> = (0..3)
        .map(|i| {
            let mut entry = finding();
            entry["id"] = json!(format!("F{i}"));
            entry["evidence"] =
                json!("A checked request reaches an unchecked dispatch state. ".repeat(10));
            entry["proposed_fix"] =
                json!("Validate the state and preserve the original error. ".repeat(10));
            entry
        })
        .collect();
    let mut observations = VecDeque::new();
    let mut ids = Vec::new();
    for lens in 0..5 {
        let entries: Vec<_> = entries
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let mut entry = entry.clone();
                let id = format!("F{lens}-{i}");
                entry["id"] = json!(id);
                ids.push(id);
                entry
            })
            .collect();
        observations.push_back(terminal_with_result(
            GraphAttemptOutcome::Succeeded,
            &answer(json!(entries), "VERDICT: FAIL"),
        ));
    }
    let verified: Vec<_> = ids.iter().map(|id| json!({"id":id,"verification":"upheld","reason":"Reproduced from the pinned source"})).collect();
    for _ in 0..2 {
        observations.push_back(terminal_with_result(
            GraphAttemptOutcome::Succeeded,
            &answer(json!(verified), "VERDICT: FAIL"),
        ));
    }
    let accepted: Vec<_> = ids.iter().map(|id| json!({"id":id,"disposition":"accepted","reason":"The unchecked dispatch remains reachable"})).collect();
    observations.push_back(terminal_with_result(
        GraphAttemptOutcome::Succeeded,
        &answer(json!(accepted), "VERDICT: FAIL"),
    ));
    let mut owner = FakeOwner {
        observations,
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert!(owner.observations.is_empty());
    assert!(
        run.checkpoint
            .nodes
            .iter()
            .all(|node| node.advance_code.is_none()),
        "{:?}",
        run.checkpoint.nodes
    );
    let (plan, positions) = validate_plan(&document).unwrap();
    let limits = crate::config::GraphLimits::default();
    let mut verifiers = 0;
    let mut synthesis = 0;
    for node in &plan.nodes {
        if template::review_verifier(node)
            || template::review_behaviour(node) == Some(template::ReviewBehaviour::Synthesis)
        {
            let (prompt, overflow) = render_input(
                node,
                &plan,
                &run.checkpoint,
                &positions,
                limits.node_prompt_bytes,
            );
            assert!(!overflow, "{}: {} bytes", node.id, prompt.len());
            assert!(prompt.len() < limits.node_prompt_bytes);
            if template::review_verifier(node) {
                verifiers += 1;
            } else {
                synthesis += 1;
            }
        }
    }
    assert_eq!(verifiers, 2);
    assert_eq!(synthesis, 1);
}

/// Medium: size affects only the checkpoint summary. The full artifact,
/// verdict and every blocking finding survive storage, replay and retrieval.
#[test]
fn oversized_blocking_findings_remain_retrievable_with_the_verdict() {
    let mut entries = Vec::new();
    while canonical_len(&entries) <= FINDINGS_BYTES {
        let index = entries.len();
        let mut entry = finding();
        entry["id"] = json!(format!("F{index}"));
        entry["summary"] = json!(format!("Finding {index}: {}", "full evidence ".repeat(60)));
        entry["triage"]["reason"] = json!("Verified the failing path. ".repeat(15));
        entry["evidence"] = json!("Trace the reachable failing path. ".repeat(12));
        entries.push(entry);
    }
    let response = answer(json!(entries), "VERDICT: FAIL");
    let mut capture = crate::result_capture::ResultCapture::new(None);
    for chunk in response.as_bytes().chunks(37) {
        capture.push(chunk);
    }
    let captured = capture.finish();
    assert!(!captured.source_truncated);
    let mut observation = terminal_with_result(GraphAttemptOutcome::Succeeded, &response);
    if let AttemptObservation::Terminal {
        result,
        result_path,
        ..
    } = &mut observation
    {
        *result = Some(Box::new(captured));
        *result_path = Some("attempts/review/result.md".into());
    }
    let document = graph(vec![review("review.tests", "review")]);
    let mut owner = FakeOwner {
        observations: [observation].into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    let progress = &run.checkpoint.nodes[0];
    let outputs = progress.outputs.as_ref().unwrap();
    assert_eq!(outputs.verdict.as_deref(), Some("VERDICT: FAIL"));
    assert!(canonical_len(&outputs.findings) <= FINDINGS_BYTES);
    assert_eq!(outputs.findings_available, Some(true));
    assert_eq!(
        outputs.result.as_ref().unwrap().path.as_deref(),
        Some("attempts/review/result.md")
    );
    let temp = crate::test_support::tempdir().unwrap();
    let directory = Directory::open_anchor(temp.path()).unwrap();
    let mut stored = json!({"checkpoint":run.checkpoint});
    prompt_store::externalise(&directory, &document.graph_id, &mut stored).unwrap();
    assert!(stored["checkpoint"]["nodes"][0]["outputs"]["_artifact_findings"].is_null());
    let bytes = serde_json::to_vec(&stored).unwrap();
    assert!(bytes.len() < response.len());
    let hydrated = prompt_store::rehydrate(&directory, &document.graph_id, &bytes).unwrap();
    let hydrated: Value = serde_json::from_slice(&hydrated).unwrap();
    let mut checkpoint: GraphCheckpoint =
        serde_json::from_value(hydrated["checkpoint"].clone()).unwrap();
    materialise_checkpoint_findings(&document, &mut checkpoint).unwrap();
    let findings = &checkpoint.nodes[0].outputs.as_ref().unwrap().findings;
    for entry in &entries {
        let id = entry["id"].as_str().unwrap();
        let finding = findings.iter().find(|finding| finding.id == id).unwrap();
        assert_eq!(finding.summary, entry["summary"].as_str().unwrap());
        assert_eq!(finding.evidence.as_deref(), entry["evidence"].as_str());
        assert!(repair::repair_eligible(finding));
    }
    assert_eq!(
        advance(&document, &mut owner, &mut journal)
            .unwrap()
            .checkpoint,
        run.checkpoint
    );
}

#[test]
// Spec: docs/specs/fabric.md#fix-round-invariants-and-split-verdict-adjudication
fn round_two_findings_without_a_named_invariant_are_warned_then_refused() {
    let mut node = planned(review("review.tests", "check"));
    node.round = Some(LoopRound {
        extra: Default::default(),
        loop_id: "repair".into(),
        round: 2,
        node_id: "check".into(),
        until: false,
        retry_on: false,
        previous_gate: None,
    });
    let unnamed: Vec<Finding> = serde_json::from_value(json!([finding()])).unwrap();
    let outputs = NodeOutputs {
        schema_version: 1,
        verdict: Some("VERDICT: FAIL".into()),
        findings_available: Some(true),
        findings: unnamed,
        ..Default::default()
    };
    let warnings = review::invariant_warnings(&node, &outputs);
    assert!(
        warnings
            .iter()
            .any(|warning| warning.starts_with("finding_invariant_missing:")),
        "{warnings:?}"
    );
    assert_eq!(
        review::invariant_error(&node, &outputs).map(|(code, _)| code),
        Some("review_invariant_missing")
    );
    let mut named = finding();
    named["class"] = json!("unchecked input x");
    let named_outputs = NodeOutputs {
        findings: serde_json::from_value(json!([named])).unwrap(),
        ..outputs.clone()
    };
    assert!(review::invariant_warnings(&node, &named_outputs).is_empty());
    assert!(review::invariant_error(&node, &named_outputs).is_none());

    // Round one is not the delta review: the first gap is only a warning there.
    node.round.as_mut().unwrap().round = 1;
    assert!(review::invariant_warnings(&node, &outputs).is_empty());
}

#[test]
// Spec: docs/specs/fabric.md#fix-round-invariants-and-split-verdict-adjudication
fn fix_round_without_sweep_sites_and_pin_is_flagged() {
    let mut node = planned(review("task", "fix"));
    node.repair_writer = true;
    let bare: Vec<RepairItem> = serde_json::from_value(json!([{
        "id": "unchecked input x", "status": "fixed", "reason": "removed the case"
    }]))
    .unwrap();
    let outputs = NodeOutputs {
        schema_version: 1,
        repair_items: Some(bare),
        ..Default::default()
    };
    let warnings = review::repair_return_warnings(&node, &outputs);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].starts_with("repair_invariant_return_missing:"));
    for part in ["sweep", "sites", "pin"] {
        assert!(warnings[0].contains(part), "{warnings:?}");
    }

    let complete: Vec<RepairItem> = serde_json::from_value(json!([{
        "id": "unchecked input x", "status": "fixed", "reason": "removed the case",
        "sweep": "rg -n 'parse(.*)input' src", "sites": ["src/a.rs", "src/b.rs"],
        "pin": "input_is_checked_when_parsed"
    }]))
    .unwrap();
    let complete_outputs = NodeOutputs {
        repair_items: Some(complete),
        ..outputs
    };
    assert!(review::repair_return_warnings(&node, &complete_outputs).is_empty());
}

#[test]
fn repair_writer_prompt_uses_full_chain_ledger_and_stages_overflow() {
    let mut source = node("source", route("fixture"));
    source.access = AccessMode::ReadOnly;
    let mut writer = node("fix", route("fixture"));
    writer.repair_writer = true;
    writer.depends_on = vec!["source".into()];
    writer.inputs = vec![GraphInput {
        extra: Default::default(),
        required_outputs: vec![OutputKind::Findings],
        node: "source".into(),
        outputs: vec![OutputKind::Findings],
    }];
    let document = graph(vec![source, writer, node("other", route("fixture"))]);
    let (plan, positions) = validate_plan(&document).unwrap();
    let mut checkpoint = transitions::initial_checkpoint(
        &document,
        &plan,
        "repair-ledger-prompt".into(),
        None,
        None,
        None,
    );
    checkpoint.review_policy = Some(ReviewPolicy::default());
    let mut large = finding();
    large["id"] = json!("F-LARGE");
    let evidence = "FULL-LEDGER-EVIDENCE-".repeat(900);
    large["evidence"] = json!(evidence);
    let large: Finding = serde_json::from_value(large).unwrap();
    let mut rejected = large.clone();
    rejected.id = "F-REJECTED".into();
    rejected.disposition = Some("rejected".into());
    let mut other_chain = large.clone();
    other_chain.id = "F-OTHER-CHAIN".into();

    checkpoint.nodes[positions["source"]].state = NodeState::Succeeded;
    checkpoint.nodes[positions["source"]].outputs = Some(NodeOutputs {
        findings_available: Some(true),
        ..Default::default()
    });
    checkpoint.nodes[positions["fix"]].finding_ledger = Some(FindingLedger {
        heads: std::collections::BTreeMap::from([(
            "a".repeat(40),
            FindingHead {
                findings: vec![large.clone(), rejected],
                unresolved: vec![large.clone()],
                ..Default::default()
            },
        )]),
        last_reviewed_head: Some("a".repeat(40)),
    });
    checkpoint.nodes[positions["other"]].finding_ledger = Some(FindingLedger {
        heads: std::collections::BTreeMap::from([(
            "b".repeat(40),
            FindingHead {
                findings: vec![other_chain.clone()],
                unresolved: vec![other_chain],
                ..Default::default()
            },
        )]),
        last_reviewed_head: Some("b".repeat(40)),
    });

    let selected = selected_findings(
        &plan.nodes[positions["fix"]],
        &plan,
        &checkpoint,
        &positions,
    );
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].0, "review_ledger");
    assert_eq!(selected[0].1.id, "F-LARGE");

    let (prompt, overflow) = render_input(
        &plan.nodes[positions["fix"]],
        &plan,
        &checkpoint,
        &positions,
        crate::config::GraphLimits::default().node_prompt_bytes,
    );
    assert!(!overflow, "the compact ledger projection should fit");
    assert!(prompt.contains("F-LARGE"));
    assert!(prompt.contains("staged full review_ledger artifact"));
    assert!(!prompt.contains("FULL-LEDGER-EVIDENCE-"));

    let requests = upstream_results(
        &plan.nodes[positions["fix"]],
        &plan,
        &checkpoint,
        &positions,
    );
    let ledger = requests
        .iter()
        .find(|request| request.label == "review_ledger")
        .expect("the complete chain ledger is staged under its own label");
    let staged = ledger.inline_findings.as_ref().unwrap();
    assert_eq!(staged.len(), 1);
    assert_eq!(staged[0].evidence.as_deref(), Some(evidence.as_str()));
    let staged_bytes = serde_json::to_vec(staged).unwrap();
    assert!(staged_bytes.len() <= NODE_INPUT_FILE_BYTES);
    let full_input = MaterialisedInputs {
        directory: Some(std::path::PathBuf::from("/fixture/inputs")),
        index: None,
        entries: vec![MaterialisedInput::File {
            label: "review_ledger".into(),
            path: std::path::PathBuf::from("/fixture/inputs/review_ledger.json"),
            bytes: staged_bytes.len() as u64,
            omitted: 0,
        }],
    };
    assert!(complete_review_ledger_artifact(
        &requests,
        Some(&full_input)
    ));
    let files = render_files(&full_input.entries, &requests, full_input.index.as_deref());
    assert!(files.contains("review_ledger.json"));
    let capped = MaterialisedInputs {
        entries: vec![MaterialisedInput::Capped {
            label: "review_ledger".into(),
        }],
        ..Default::default()
    };
    assert!(!complete_review_ledger_artifact(
        &requests,
        Some(&capped)
    ));
}

#[test]
fn unaccepted_replayed_repair_attempt_refuses_a_capped_ledger_artifact() {
    let mut source = node("source", route("fixture"));
    source.access = AccessMode::ReadOnly;
    let mut writer = node("fix", route("fixture"));
    writer.repair_writer = true;
    writer.depends_on = vec!["source".into()];
    writer.inputs = vec![GraphInput {
        extra: Default::default(),
        required_outputs: vec![OutputKind::Findings],
        node: "source".into(),
        outputs: vec![OutputKind::Findings],
    }];
    let mut document = graph(vec![source, writer]);
    document.schema_version = 2;
    let limits = crate::config::GraphLimits::default();
    let admitted = candidate::admit_candidate_with_review_policy(
        &document,
        None,
        &[],
        &limits,
        &Default::default(),
        None,
        &ReviewPolicy::default(),
    )
    .unwrap();
    let (plan, positions) = validate_flat_plan(admitted.expanded(), &limits).unwrap();
    let digest = plan_digest(&plan).unwrap().to_string();
    let mut checkpoint = transitions::initial_checkpoint(
        &document,
        &plan,
        digest,
        None,
        None,
        None,
    );
    checkpoint.review_policy = Some(ReviewPolicy::default());
    let source_key = AttemptKey {
        extra: Default::default(),
        retry_generation: None,
        previous_attempt_ref: None,
        graph_id: document.graph_id.clone(),
        node_id: "source".into(),
        route_index: 0,
        attempt: 1,
    };
    transitions::begin_attempt(
        positions["source"],
        &plan,
        &mut checkpoint,
        &positions,
        &source_key,
        &plan.nodes[positions["source"]].routes[0],
        transitions::LaunchFacts {
            input_cap: limits.node_prompt_bytes,
            placement_host_id: None,
            launch_actor: "fixture".into(),
            input_cap_source: provenant_records::Layer::Detected,
            timing: None,
        },
    );
    let source_attempt = checkpoint.nodes[positions["source"]]
        .attempts
        .last_mut()
        .unwrap();
    source_attempt.state = AttemptState::Terminal;
    source_attempt.attempt_ref = Some("host-a/source".into());
    source_attempt.outcome = Some(GraphAttemptOutcome::Succeeded);
    checkpoint.nodes[positions["source"]].state = NodeState::Succeeded;
    checkpoint.nodes[positions["source"]].outputs = Some(NodeOutputs {
        findings_available: Some(true),
        ..Default::default()
    });
    let mut large = finding();
    large["id"] = json!("F-LARGE-REPLAY");
    large["evidence"] = json!("FULL-LEDGER-EVIDENCE-".repeat(900));
    let large: Finding = serde_json::from_value(large).unwrap();
    let head = "a".repeat(40);
    checkpoint.nodes[positions["fix"]].finding_ledger = Some(FindingLedger {
        heads: std::collections::BTreeMap::from([(
            head.clone(),
            FindingHead {
                findings: vec![large.clone()],
                unresolved: vec![large],
                ..Default::default()
            },
        )]),
        last_reviewed_head: Some(head),
    });
    let key = AttemptKey {
        extra: Default::default(),
        retry_generation: None,
        previous_attempt_ref: None,
        graph_id: document.graph_id.clone(),
        node_id: "fix".into(),
        route_index: 0,
        attempt: 1,
    };
    transitions::begin_attempt(
        positions["fix"],
        &plan,
        &mut checkpoint,
        &positions,
        &key,
        &plan.nodes[positions["fix"]].routes[0],
        transitions::LaunchFacts {
            input_cap: limits.node_prompt_bytes,
            placement_host_id: None,
            launch_actor: "fixture".into(),
            input_cap_source: provenant_records::Layer::Detected,
            timing: None,
        },
    );
    let checkpoint: GraphCheckpoint =
        serde_json::from_value(serde_json::to_value(checkpoint).unwrap()).unwrap();
    let mut journal = MemoryJournal(Some(checkpoint));

    let mut first_owner = FakeOwner {
        materialise: Some(fully_materialise_inputs),
        ..Default::default()
    };
    assert!(matches!(
        advance(&document, &mut first_owner, &mut journal),
        Err(GraphError::Run(_))
    ));
    assert_eq!(first_owner.calls.len(), 1);

    let mut replay_owner = FakeOwner {
        materialise: Some(cap_materialised_inputs),
        ..Default::default()
    };
    let replay = advance(&document, &mut replay_owner, &mut journal);
    assert!(matches!(replay, Err(GraphError::Refused(reason)) if reason.contains("full review-chain ledger")));
    assert!(replay_owner.calls.is_empty(), "the unaccepted attempt must not launch");
    assert!(replay_owner.file_requests[0]
        .iter()
        .any(|request| request.label == "review_ledger"));
}

#[test]
// Spec: docs/specs/fabric.md#fix-round-invariants-and-split-verdict-adjudication
fn round_two_missing_invariant_is_refused_at_graph_settlement() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version": 1, "graph_id": "delta-contract", "nodes": [{
            "id": "repair", "kind": "loop", "access": "read_only",
            "loop": {"max_rounds": 2, "until": "accepted", "body": [
                review("review.tests", "check"),
                {"id":"accepted", "kind":"gate", "access":"read_only", "depends_on":["check"],
                 "inputs":[{"node":"check","outputs":["verdict"]}],
                 "condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"}}
            ]}
        }]
    }))
    .unwrap();
    let mut owner = FakeOwner {
        observations: [
            terminal_with_result(
                GraphAttemptOutcome::Succeeded,
                &answer(json!([]), "VERDICT: FAIL"),
            ),
            terminal_with_result(
                GraphAttemptOutcome::Succeeded,
                &answer(json!([finding()]), "VERDICT: FAIL"),
            ),
        ]
        .into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    let checkpoint = journal.0.as_ref().unwrap();
    assert_eq!(checkpoint, &run.checkpoint);
    let check = checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "repair--r2--check")
        .unwrap();
    assert_eq!(check.state, NodeState::Failed);
    assert_eq!(
        check.advance_code.as_deref(),
        Some("review_invariant_missing")
    );
    assert!(
        check
            .outputs
            .as_ref()
            .unwrap()
            .warnings
            .iter()
            .any(|warning| warning.starts_with("finding_invariant_missing:"))
    );
}

#[test]
// Spec: docs/specs/fabric.md#fix-round-invariants-and-split-verdict-adjudication
fn fix_round_missing_sweep_sites_and_pin_is_flagged_at_graph_settlement() {
    let mut writer = node("fix", route("fixture"));
    writer.repair_writer = true;
    writer.depends_on = vec!["source".into()];
    writer.inputs = vec![GraphInput {
        extra: Default::default(),
        required_outputs: vec![],
        node: "source".into(),
        outputs: vec![OutputKind::Findings],
    }];
    let document = graph(vec![node("source", route("fixture")), writer]);
    let report = format!(
        "```findings\n{}\n```",
        json!({
            "schema_version": 1, "findings": [], "repair_items": [{
                "id":"unchecked input x", "status":"fixed", "reason":"removed the case"
            }]
        })
    );
    let mut owner = FakeOwner {
        observations: [
            terminal_with_result(
                GraphAttemptOutcome::Succeeded,
                &answer(json!([finding()]), "VERDICT: FAIL"),
            ),
            terminal_with_result(GraphAttemptOutcome::Succeeded, &report),
        ]
        .into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    let checkpoint = journal.0.as_ref().unwrap();
    assert_eq!(checkpoint, &run.checkpoint);
    let fix = checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "fix")
        .unwrap();
    assert_eq!(fix.state, NodeState::Succeeded);
    let warnings = &fix.outputs.as_ref().unwrap().warnings;
    let warning = warnings
        .iter()
        .find(|warning| warning.starts_with("repair_invariant_return_missing:"))
        .unwrap();
    for part in ["sweep", "sites", "pin"] {
        assert!(warning.contains(part), "{warnings:?}");
    }
}

fn delivery_review_panel() -> GraphDocument {
    let panel = json!({
        "id":"panel",
        "kind":"composite",
        "access":"read_only",
        "composite":{
            "min_successful":1,
            "reduce_verdict":true,
            "nodes":[review("review.tests", "tests"), review("review.correctness", "correctness")],
            "exports":[
                {"node":"tests","outputs":["findings","result","verdict"]},
                {"node":"correctness","outputs":["findings","result","verdict"]}
            ]
        },
        "delivery_review":DeliveryReviewSpec::default()
    });
    let mut document: GraphDocument = serde_json::from_value(json!({
        "schema_version":2,
        "graph_id":"delivery-review-panel",
        "nodes":[panel]
    }))
    .unwrap();
    document.queue = Some(QueueKnobs::default());
    document
}

#[test]
fn malformed_composite_delivery_review_parks_failed_and_partial_members_on_replay() {
    for failed_member in [true, false] {
        let document = delivery_review_panel();
        let expanded = plans::expand(&document, &[], &[], &plans::generous()).unwrap();
        let (plan, positions) = validate_flat_plan(&expanded, &plans::generous()).unwrap();
        let mut checkpoint = transitions::initial_checkpoint(
            &expanded,
            &plan,
            "composite-review-park-replay".into(),
            None,
            None,
            None,
        );
        let boundary = positions["panel"];
        let members = plan.nodes[boundary].composite_members.clone();
        assert!(members.len() >= 2);
        for (ordinal, member_id) in members.iter().enumerate() {
            let progress = &mut checkpoint.nodes[positions[member_id]];
            progress.state = NodeState::Succeeded;
            progress.outputs = Some(NodeOutputs {
                verdict: Some("VERDICT: PASS".into()),
                findings_available: Some(true),
                ..Default::default()
            });
            if ordinal == 0 && failed_member {
                progress.state = NodeState::Failed;
                progress.outputs = None;
                progress.advance_code = Some("review_failed".into());
                progress.advance_note = Some("panel member failed".into());
            } else if ordinal == 0 {
                let outputs = progress.outputs.as_mut().unwrap();
                outputs.partial = true;
                outputs.findings_available = Some(false);
            }
        }

        let mut owner = FakeOwner::default();
        let mut journal = MemoryJournal::default();
        let mut transition_ids = Vec::new();
        for _ in 0..2 {
            composites::settle::<FakeOwner, MemoryJournal>(
                boundary,
                &plan,
                &positions,
                &mut checkpoint,
                &mut journal,
            )
            .unwrap();
            assert!(park_malformed_composite_delivery_review(
                &expanded,
                boundary,
                &plan,
                &positions,
                &mut owner,
                &mut journal,
                &mut checkpoint,
            )
            .unwrap());
            transition_ids.push(
                checkpoint.nodes[boundary]
                    .delivery_park
                    .as_ref()
                    .unwrap()
                    .transition_id
                    .clone(),
            );
        }

        let parked = checkpoint.nodes[boundary].delivery_park.as_ref().unwrap();
        assert_eq!(parked.reason, provenant_records::graph::ParkReason::ReviewEvidence);
        assert_eq!(transition_ids[0], transition_ids[1]);
        assert!(parked.attempt_key.is_none());
        assert!(parked.evidence.iter().any(|entry| entry.starts_with("member ")));
    }

    let document = delivery_review_panel();
    let expanded = plans::expand(&document, &[], &[], &plans::generous()).unwrap();
    let (plan, positions) = validate_flat_plan(&expanded, &plans::generous()).unwrap();
    let mut checkpoint = transitions::initial_checkpoint(
        &expanded,
        &plan,
        "composite-review-valid-fail".into(),
        None,
        None,
        None,
    );
    let boundary = positions["panel"];
    for member_id in &plan.nodes[boundary].composite_members {
        let progress = &mut checkpoint.nodes[positions[member_id]];
        progress.state = NodeState::Succeeded;
        progress.outputs = Some(NodeOutputs {
            verdict: Some("VERDICT: FAIL".into()),
            findings_available: Some(true),
            ..Default::default()
        });
    }
    let mut journal = MemoryJournal::default();
    composites::settle::<FakeOwner, MemoryJournal>(
        boundary,
        &plan,
        &positions,
        &mut checkpoint,
        &mut journal,
    )
    .unwrap();
    assert!(!review::malformed_composite_delivery_evidence(
        &plan.nodes[boundary],
        &checkpoint.nodes[boundary],
    ));
}
