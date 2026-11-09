    use super::*;
    use serde_json::{Value, json};

    fn review(name: &str, id: &str) -> GraphNode {
        template::Compiler::new(&template::TemplateLibrary::bundled())
            .compile(&json!({"id":id,"use":name,"route":{"alias":"fixture"}}))
            .unwrap()
    }

    fn finding() -> Value {
        json!({"id":"F1","severity":"high","file":"a.rs","line":1,
            "summary":"The failing path is reachable","status":"open",
            "evidence":"Input x reaches the panic","proposed_fix":"Handle x"})
    }

    fn answer(findings: Value, verdict: &str) -> String {
        format!(
            "```findings\n{}\n```\n{verdict}\n",
            json!({"schema_version":1,"findings":findings})
        )
    }

    fn panel(topology: &str) -> GraphDocument {
        let library = template::TemplateLibrary::bundled();
        let resolvers = std::collections::BTreeMap::from([
            ("review-topology".into(), vec![topology.to_owned()]),
            (
                "review-panel".into(),
                vec!["correctness".into(), "tests".into()],
            ),
            (
                "review-verifiers".into(),
                vec!["first".into(), "second".into()],
            ),
            ("review-advice-correctness".into(), vec![String::new()]),
            ("review-advice-tests".into(), vec![String::new()]),
        ]);
        let panel = template::Compiler {
            library: &library,
            resolvers: &resolvers,
        }
        .compile(&json!({"id":"panel","use":"review-panel"}))
        .unwrap();
        graph(vec![panel])
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
                            AttemptOutcome::Succeeded,
                            "Pinned subject and acceptance criteria",
                        ));
                    }
                    let mut note = finding();
                    note["severity"] = json!("low");
                    observations.push_back(terminal_with_result(
                        AttemptOutcome::Succeeded,
                        &answer(json!([note]), "VERDICT: PASS"),
                    ));
                    observations.push_back(terminal(AttemptOutcome::UsageLimited));
                    if verify {
                        observations.push_back(terminal_with_result(
                            AttemptOutcome::Succeeded,
                            &answer(
                                json!([{
                                    "id":"F1","verification":"upheld","reason":"Reproduced"
                                }]),
                                "VERDICT: PASS",
                            ),
                        ));
                        observations.push_back(terminal(AttemptOutcome::UsageLimited));
                    }
                    if synthesis {
                        observations.push_back(terminal_with_result(AttemptOutcome::Succeeded, &answer(json!([{
                            "id":"F1","disposition":"rejected","reason":"Permitted by the contract"
                        }]), "VERDICT: PASS")));
                    }
                    let mut owner = FakeOwner {
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
                    assert_eq!(
                        output.extra.get("review_partial"),
                        Some(&json!(true)),
                        "{topology}"
                    );
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
                            .push_back(terminal_with_result(AttemptOutcome::Succeeded, "Frame"));
                    }
                    observations.push_back(terminal_with_result(
                        AttemptOutcome::Succeeded,
                        &answer(json!([]), "VERDICT: PASS"),
                    ));
                    observations.push_back(if quota_limited {
                        terminal(AttemptOutcome::UsageLimited)
                    } else {
                        terminal_with_result(
                            AttemptOutcome::Succeeded,
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
                        AttemptOutcome::Succeeded,
                        &answer(findings, verdict),
                    ));
                    observations.push_back(terminal_with_result(
                        AttemptOutcome::Succeeded,
                        &answer(json!([]), "VERDICT: PASS"),
                    ));
                    let mut owner = FakeOwner {
                        observations,
                        ..Default::default()
                    };
                    let run =
                        advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
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
            terminal_with_result(AttemptOutcome::Succeeded, &answer(json!([finding(), {}]), "VERDICT: FAIL")),
            terminal_with_result(AttemptOutcome::Succeeded, &answer(json!([]), "VERDICT: PASS")),
            terminal_with_result(AttemptOutcome::Succeeded, &answer(json!([{"id":"F1","verification":"upheld","reason":"Reproduced"}]), "VERDICT: FAIL")),
            terminal_with_result(AttemptOutcome::Succeeded, &answer(json!([{"id":"F1","verification":"upheld","reason":"Independently reproduced"}]), "VERDICT: FAIL")),
            terminal_with_result(AttemptOutcome::Succeeded, &answer(json!([{"id":"F1","disposition":"accepted","reason":"Input x still panics"}]), "VERDICT: FAIL")),
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
                    &node.advance_error_code,
                    &node.advance_error
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
            invalid.advance_error_code.as_deref(),
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
                    && finding.extra.get("evidence") == Some(&json!("Input x reaches the panic")))
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
            .map(|verdict| {
                terminal_with_result(AttemptOutcome::Succeeded, &answer(json!([]), verdict))
            })
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
                    AttemptOutcome::Succeeded,
                    &answer(json!([forged]), "VERDICT: FAIL"),
                ),
                terminal_with_result(
                    AttemptOutcome::Succeeded,
                    &answer(json!([second]), "VERDICT: FAIL"),
                ),
                terminal_with_result(
                    AttemptOutcome::Succeeded,
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
            invalid.advance_error_code.as_deref(),
            Some("review_propagation_invalid")
        );
        let synthesis = run
            .checkpoint
            .nodes
            .iter()
            .find(|node| node.id.ends_with("--synthesis"))
            .unwrap();
        assert_eq!(
            synthesis.advance_error_code.as_deref(),
            Some("disposition_count")
        );
        let findings = &synthesis.outputs.as_ref().unwrap().findings;
        assert_eq!(findings.len(), 2);
        assert_ne!(findings[0].id, findings[1].id);
        assert_ne!(findings[0].file, findings[1].file);
    }

    #[test]
    fn typed_contract_rejects_invalid_verdicts_even_without_a_pass_requirement() {
        for verdict in [
            "",
            "VERDICT: BANANA",
            "VERDICT: APPROVE",
            "VERDICT: PASS\nVERDICT: PASS",
            "VERDICT: PASS\nmore text",
        ] {
            let mut node = review("review.tests", "review");
            // Repair panels remove the PASS requirement, not the output contract.
            node.required_verdict = None;
            let mut owner = FakeOwner {
                observations: [terminal_with_result(
                    AttemptOutcome::Succeeded,
                    &answer(json!([]), verdict),
                ), terminal_with_result(AttemptOutcome::Succeeded, &answer(json!([]), verdict))]
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
                Some(AttemptOutcome::TaskFailed)
            );
            assert_eq!(
                progress.advance_error_code.as_deref(),
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

    #[test]
    fn review_contract_accepts_verdict_followed_only_by_handoff() {
        let mut node = review("review.tests", "review");
        node.required_verdict = None;
        let response = format!(
            "{}\nReview completed.\nVERDICT: FAIL\n```handoff\n{{\"schema_version\":2,\"status\":\"done\",\"note\":\"Reviewed\",\"remaining\":[],\"decisions\":[],\"verify\":[]}}\n```\n",
            answer(json!([]), "").trim_end()
        );
        let mut owner = FakeOwner {
            observations: [terminal_with_result(AttemptOutcome::Succeeded, &response)].into(),
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
        assert_eq!(progress.advance_error_code, None);
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
            verifier.inputs = vec![
                serde_json::from_value(json!({"node":"source","outputs":["findings"]})).unwrap(),
            ];
            let mut owner = FakeOwner {
                observations: [
                    terminal_with_result(
                        AttemptOutcome::Succeeded,
                        &answer(json!([finding()]), "VERDICT: FAIL"),
                    ),
                    terminal_with_result(
                        AttemptOutcome::Succeeded,
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
                progress.advance_error_code.as_deref(),
                Some("verification_incomplete")
            );
            assert_eq!(
                progress.outputs.as_ref().unwrap().findings[0].extra["evidence"],
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
                    AttemptOutcome::Succeeded,
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
                progress.advance_error_code.as_deref(),
                Some("review_findings_invalid")
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
                        AttemptOutcome::Succeeded,
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
                    AttemptOutcome::DeclaredStop,
                    &answer(findings.clone(), verdict),
                ));
                let mut owner = FakeOwner {
                    observations,
                    ..Default::default()
                };
                let run =
                    advance(&graph(nodes), &mut owner, &mut MemoryJournal::default()).unwrap();
                let progress = run
                    .checkpoint
                    .nodes
                    .iter()
                    .find(|node| node.id == "review")
                    .unwrap();
                if verdict == "VERDICT: PASS" {
                    assert_eq!(progress.state, NodeState::Failed);
                    assert_eq!(
                        progress.advance_error_code.as_deref(),
                        Some("review_verdict_conflict")
                    );
                } else {
                    assert_eq!(progress.state, NodeState::Succeeded);
                    assert_eq!(
                        progress.attempts.last().unwrap().outcome,
                        Some(AttemptOutcome::Succeeded)
                    );
                }
            }
        }
    }

#[test]
fn malformed_lens_is_corrected_once_without_repeating_siblings() {
    for corrected in [true, false] {
        let document = panel("f0-v0-s0");
        let mut owner = FakeOwner {
            observations: [
                terminal_with_result(AttemptOutcome::Succeeded, &answer(json!([finding()]), "")),
                terminal_with_result(
                    AttemptOutcome::Succeeded,
                    &answer(
                        json!([finding()]),
                        if corrected { "VERDICT: FAIL" } else { "" },
                    ),
                ),
                terminal_with_result(
                    AttemptOutcome::Succeeded,
                    &answer(json!([]), "VERDICT: PASS"),
                ),
            ]
            .into(),
            ..Default::default()
        };
        let mut journal = MemoryJournal::default();
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        assert!(owner.observations.is_empty());
        assert_eq!(
            run.checkpoint
                .nodes
                .iter()
                .filter(|node| !node.previous_attempts.is_empty())
                .count(),
            1
        );
        assert!(
            owner
                .prompts
                .iter()
                .any(|prompt| prompt.contains("only automatic verdict correction"))
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
        assert_ne!(output.verdict.as_deref(), Some("VERDICT: PASS"));
        assert_eq!(
            advance(&document, &mut owner, &mut journal)
                .unwrap()
                .checkpoint,
            run.checkpoint
        );
    }
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
                    json!("A realistic code path reaches an unchecked state. ".repeat(20));
                entry["proposed_fix"] =
                    json!("Validate the state before dispatch and preserve the error. ".repeat(15));
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
                    AttemptOutcome::Succeeded,
                    &answer(json!(entries[..count / 2]), "VERDICT: FAIL"),
                ),
                terminal_with_result(
                    AttemptOutcome::Succeeded,
                    &answer(json!(entries[count / 2..]), "VERDICT: FAIL"),
                ),
                terminal_with_result(
                    AttemptOutcome::Succeeded,
                    &answer(json!(decisions), "VERDICT: FAIL"),
                ),
                terminal_with_result(
                    AttemptOutcome::Succeeded,
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
                .all(|node| node.advance_error_code.is_none()),
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
        let verifiers: Vec<_> = plan.nodes.iter().filter(|node| template::review_verifier(node)).collect();
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
    let mut owner = FakeOwner {
        observations: [
            terminal_with_result(
                AttemptOutcome::Succeeded,
                &answer(json!([finding()]), "VERDICT: FAIL"),
            ),
            terminal(AttemptOutcome::AdapterUnavailable),
        ]
        .into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    let boundary = &run.checkpoint.nodes[0];
    assert_eq!(
        boundary.advance_error_code.as_deref(),
        Some("coverage_below_minimum")
    );
    let output = boundary.outputs.as_ref().unwrap();
    assert_eq!(output.findings.len(), 1);
    assert_ne!(output.verdict.as_deref(), Some("VERDICT: PASS"));
    assert_eq!(
        advance(&document, &mut owner, &mut journal)
            .unwrap()
            .checkpoint,
        run.checkpoint
    );
}
