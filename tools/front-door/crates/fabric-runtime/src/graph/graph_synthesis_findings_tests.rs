use super::*;

#[test]
fn a_synthesis_settles_with_a_disposition_and_reason_for_every_finding() {
    let progress = synthesis_run(&findings_text(&entry(
        ",\"disposition\":\"accepted\",\"reason\":\"reproduced\"",
    )));
    assert_eq!(progress.state, NodeState::Succeeded);
    let finding = &progress.outputs.unwrap().findings[0];
    assert_eq!(finding.disposition.as_deref(), Some("accepted"));
    assert_eq!(finding.reason.as_deref(), Some("reproduced"));
    // A reviewed change with no findings has nothing to dispose of.
    assert_eq!(
        synthesis_run(&findings_text("")).state,
        NodeState::Succeeded
    );
}

// Small: settlement uses only the in-memory graph owner.
#[test]
fn identity_only_dispositions_inherit_selected_findings() {
    let mut synthesis = node("synthesis", route("first"));
    synthesis.dispositions = DISPOSITIONS.map(String::from).to_vec();
    synthesis.depends_on = vec!["review".into()];
    synthesis.inputs = vec![GraphInput {
        required_outputs: Vec::new(),
        extra: Default::default(),
        node: "review".into(),
        outputs: vec![OutputKind::Findings],
    }];
    for (id, state) in [
        ("F1", NodeState::Succeeded),
        ("missing-F9", NodeState::Failed),
    ] {
        let text = findings_text(&format!(
            r#"{{"id":"{id}","disposition":"rejected","reason":"falsified"}}"#
        ));
        let mut owner = FakeOwner {
            observations: [
                terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&entry(""))),
                terminal_with_result(AttemptOutcome::Succeeded, &text),
            ]
            .into(),
            ..Default::default()
        };
        let run = advance(
            &graph(vec![node("review", route("first")), synthesis.clone()]),
            &mut owner,
            &mut MemoryJournal::default(),
        )
        .unwrap();
        let progress = &run.checkpoint.nodes[1];
        assert_eq!(progress.state, state);
        if state == NodeState::Succeeded {
            let finding = &progress.outputs.as_ref().unwrap().findings[0];
            assert_eq!(finding.summary, "s");
            assert_eq!(finding.file, "a.rs");
            assert_eq!(finding.severity, "note");
            assert_eq!(finding.status, "open");
            assert_eq!(finding.reason.as_deref(), Some("falsified"));
        } else {
            let error = progress.advance_error.as_deref().unwrap();
            assert!(
                error.contains("(missing-F9) field id: unknown upstream identity"),
                "{error}"
            );
        }
    }
}

// Small: composite export and downstream settlement share the real graph path.
#[test]
fn composite_findings_have_qualified_ids_and_unambiguous_bare_aliases() {
    for (second_id, cited, state) in [
        (
            "F1",
            vec!["reviews--a:F1", "reviews--b:F1"],
            NodeState::Succeeded,
        ),
        ("F2", vec!["F1", "F2"], NodeState::Succeeded),
        ("inner:F2", vec!["F1", "inner:F2"], NodeState::Succeeded),
        (
            "inner:F2",
            vec!["reviews--a:F1", "reviews--b:inner:F2"],
            NodeState::Succeeded,
        ),
        ("F1", vec!["F1", "reviews--b:F1"], NodeState::Failed),
    ] {
        let document: GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version":1, "graph_id":"qualified-findings", "nodes":[
                {"id":"reviews", "kind":"composite", "access":"read_only", "composite":{
                    "nodes":[
                        {"id":"a", "access":"read_only", "prompt":"review", "route":{"alias":"senior"}},
                        {"id":"b", "access":"read_only", "prompt":"review", "route":{"alias":"senior"}}
                    ], "exports":[{"node":"a", "outputs":["findings"]}, {"node":"b", "outputs":["findings"]}]
                }},
                {"id":"synthesis", "kind":"synthesise", "access":"read_only", "prompt":"dispose", "route":{"alias":"senior"},
                 "depends_on":["reviews"], "inputs":[{"node":"reviews", "outputs":["findings"]}],
                 "dispositions":["accepted", "rejected", "deferred"]}
            ]
        })).unwrap();
        let entries: Vec<_> = cited
            .iter()
            .map(|id| serde_json::json!({"id":id, "disposition":"rejected", "reason":"falsified"}))
            .collect();
        let text = format!(
            "```findings\n{}\n```\nVERDICT: PASS",
            serde_json::json!({"schema_version":1,"findings":entries})
        );
        let mut owner = FakeOwner {
            observations: [
                terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&entry(""))),
                terminal_with_result(
                    AttemptOutcome::Succeeded,
                    &findings_text(&entry("").replace("F1", second_id)),
                ),
                terminal_with_result(AttemptOutcome::Succeeded, &text),
            ]
            .into(),
            ..Default::default()
        };
        let mut journal = MemoryJournal::default();
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        let composite = run
            .checkpoint
            .nodes
            .iter()
            .find(|node| node.id == "reviews")
            .unwrap();
        assert_eq!(
            composite.outputs.as_ref().unwrap().findings_available,
            Some(true)
        );
        let ids: Vec<_> = composite
            .outputs
            .as_ref()
            .unwrap()
            .findings
            .iter()
            .map(|finding| finding.id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec!["reviews--a:F1", &format!("reviews--b:{second_id}")]
        );
        let synthesis = run
            .checkpoint
            .nodes
            .iter()
            .find(|node| node.id == "synthesis")
            .unwrap();
        assert_eq!(synthesis.state, state);
        if state == NodeState::Succeeded {
            assert_eq!(
                synthesis
                    .outputs
                    .as_ref()
                    .unwrap()
                    .findings
                    .iter()
                    .map(|finding| finding.id.as_str())
                    .collect::<Vec<_>>(),
                ids
            );
        } else {
            let error = synthesis.advance_error.as_deref().unwrap();
            assert!(
                error.contains("F1") && error.contains("ambiguous"),
                "{error}"
            );
        }
        assert_eq!(
            advance(&document, &mut FakeOwner::default(), &mut journal)
                .unwrap()
                .checkpoint,
            run.checkpoint
        );
    }
}

// Small: malformed entries must retain a bounded, actionable diagnostic.
#[test]
fn findings_error_names_first_entry_and_field() {
    for (entry, field) in [
        (
            r#"{"id":"broken","severity":42},{"id":"later"}"#.to_owned(),
            "severity",
        ),
        (
            r#"{"id":"broken","severity":"high","file":"a"}"#.to_owned(),
            "summary",
        ),
        (
            entry("").replace("null", "-1").replace("F1", "broken"),
            "line",
        ),
    ] {
        let capture = capture(&findings_text(&entry));
        let outputs = make_outputs(Some(&capture), None, None, None, None);
        assert!(
            outputs
                .findings
                .iter()
                .any(|finding| finding.id == "broken")
        );
        let error = &outputs.warnings[0];
        assert!(
            error.contains("entry 1") && error.contains("broken"),
            "{error}"
        );
        assert!(error.contains(field), "{error}");
        assert!(!error.contains("later"), "{error}");
    }
    let progress = synthesis_run(&findings_text(&format!(
        r#"{{"id":"{}","disposition":"rejected","reason":"unknown"}}"#,
        "界".repeat(2000)
    )));
    assert!(progress.advance_error.as_ref().unwrap().len() <= MAX_ADVANCE_ERROR_BYTES);
}

#[test]
fn a_synthesis_must_dispose_of_every_selected_upstream_finding_exactly_once() {
    let disposed = |id: &str| {
        format!(
            "{{\"id\":\"{id}\",\"severity\":\"high\",\"file\":\"a.rs\",\"line\":null,\"summary\":\"s\",\"status\":\"open\",\"disposition\":\"accepted\",\"reason\":\"reproduced\"}}"
        )
    };
    let run = |text: &str| {
        let mut synthesis = node("synthesis", route("first"));
        synthesis.kind = Some(NodeKind::Synthesise);
        synthesis.dispositions = DISPOSITIONS.map(String::from).to_vec();
        synthesis.depends_on = vec!["review".into()];
        synthesis.inputs = vec![GraphInput {
            required_outputs: Vec::new(),
            extra: Default::default(),
            node: "review".into(),
            outputs: vec![OutputKind::Findings],
        }];
        let mut owner = FakeOwner {
            observations: [
                terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&entry(""))),
                terminal_with_result(AttemptOutcome::Succeeded, text),
            ]
            .into(),
            ..Default::default()
        };
        advance(
            &graph(vec![node("review", route("first")), synthesis]),
            &mut owner,
            &mut MemoryJournal::default(),
        )
        .unwrap()
        .checkpoint
        .nodes
        .remove(1)
    };
    assert_eq!(
        run(&findings_text(&disposed("F1"))).state,
        NodeState::Succeeded
    );
    for text in [
        findings_text(""),
        findings_text(&disposed("F9")),
        findings_text(&format!("{},{}", disposed("F1"), disposed("F1"))),
    ] {
        let progress = run(&text);
        assert_eq!(progress.state, NodeState::Failed, "{text}");
        assert_eq!(
            progress.advance_error_code.as_deref(),
            Some("disposition_count")
        );
    }
}

#[test]
fn synthesis_disposes_once_of_findings_propagated_through_verifier() {
    let lens = node("lens", route("first"));
    let mut verify = node("verify", route("first"));
    verify.depends_on = vec!["lens".into()];
    verify.inputs = vec![GraphInput {
        required_outputs: Vec::new(),
        extra: Default::default(),
        node: "lens".into(),
        outputs: vec![OutputKind::Result],
    }];
    let mut synthesis = node("synthesis", route("first"));
    synthesis.dispositions = DISPOSITIONS.map(String::from).to_vec();
    synthesis.depends_on = vec!["lens".into(), "verify".into()];
    synthesis.inputs = ["verify", "lens"]
        .into_iter()
        .map(|id| GraphInput {
            required_outputs: Vec::new(),
            extra: Default::default(),
            node: id.into(),
            outputs: vec![OutputKind::Findings],
        })
        .collect();
    let mut owner = FakeOwner {
        observations: [
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&entry(""))),
            terminal_with_result(
                AttemptOutcome::Succeeded,
                &findings_text(&entry(
                    ",\"verification\":\"upheld\",\"propagates\":\"lens:F1\"",
                )),
            ),
            terminal_with_result(
                AttemptOutcome::Succeeded,
                &findings_text(r#"{"id":"F1","disposition":"accepted","reason":"verified"}"#),
            ),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![lens, verify, synthesis]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    let finding = &run.checkpoint.nodes[2].outputs.as_ref().unwrap().findings[0];
    assert_eq!(finding.id, "F1");
    assert_eq!(finding.extra["verification"], "upheld");
}

#[test]
fn large_synthesis_trims_metadata_and_replays_with_finding_identities() {
    let input = |id: &str| GraphInput {
        required_outputs: Vec::new(),
        extra: Default::default(),
        node: id.into(),
        outputs: vec![OutputKind::Findings],
    };
    let mut nodes = vec![node("seed", route("first"))];
    let mut observations = VecDeque::from([terminal_with_result(
        AttemptOutcome::Succeeded,
        &findings_text(&entry("")),
    )]);
    let entries = vec![entry(",\"propagates\":\"seed:F1\""); MAX_FINDINGS].join(",");
    let verifier_text = findings_text(&entries);
    for i in 0..8 {
        let mut verifier = node(&format!("verifier{i}"), route("first"));
        verifier.depends_on = vec!["seed".into()];
        verifier.inputs = vec![input("seed")];
        nodes.push(verifier);
        observations.push_back(terminal_with_result(
            AttemptOutcome::Succeeded,
            &verifier_text,
        ));
    }
    let mut synthesis = node("synthesis", route("first"));
    synthesis.depends_on = (0..8).map(|i| format!("verifier{i}")).collect();
    synthesis.inputs = synthesis.depends_on.iter().map(|id| input(id)).collect();
    synthesis.dispositions = DISPOSITIONS.map(String::from).to_vec();
    nodes.push(synthesis);
    let mut next = node("next", route("first"));
    next.depends_on = vec!["synthesis".into()];
    next.inputs = vec![input("synthesis")];
    nodes.push(next);
    let decisions = serde_json::json!({"id":"F1", "disposition":"accepted", "reason":"yes", "detail":"x".repeat(8000)});
    observations.push_back(terminal_with_result(
        AttemptOutcome::Succeeded,
        &findings_text(&decisions.to_string()),
    ));
    observations.push_back(terminal_with_result(
        AttemptOutcome::Succeeded,
        &findings_text(""),
    ));
    let document = graph(nodes);
    let mut journal = MemoryJournal::default();
    let mut owner = FakeOwner {
        observations,
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    let synthesis = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "synthesis")
        .unwrap();
    assert_eq!(
        synthesis.state,
        NodeState::Succeeded,
        "{:?}",
        synthesis.advance_error
    );
    let outputs = synthesis.outputs.as_ref().unwrap();
    assert!(
        outputs
            .warnings
            .iter()
            .any(|warning| warning.contains("findings metadata trimmed"))
    );
    assert_eq!(outputs.findings.len(), 8 * MAX_FINDINGS);
    assert!(
        outputs.findings.iter().all(|finding| finding.id == "F1"
            && finding.severity == "note"
            && finding.file == "a.rs")
    );
    assert_eq!(
        run.checkpoint.nodes.last().unwrap().state,
        NodeState::Succeeded
    );
    let prompt = owner.prompts.last().unwrap();
    assert!(prompt.contains("F1") && prompt.contains("note") && prompt.contains("a.rs"));
    let stored = serde_json::to_vec(journal.0.as_ref().unwrap()).unwrap();
    journal.0 = Some(serde_json::from_slice(&stored).unwrap());
    let replay = advance(&document, &mut FakeOwner::default(), &mut journal).unwrap();
    assert_eq!(run.checkpoint, replay.checkpoint);
}

#[test]
fn large_synthesis_compacts_immutable_fields_and_replays_with_finding_identities() {
    for verifying in [false, true] {
        let input = |id: &str| GraphInput {
            required_outputs: Vec::new(),
            extra: Default::default(),
            node: id.into(),
            outputs: vec![OutputKind::Findings],
        };
        let mut nodes = vec![node("seed", route("first"))];
        let qualified =
            entry(",\"lens\":\"tests\",\"evidence\":\"observed\",\"proposed_fix\":\"repair\"");
        let mut observations = VecDeque::from([terminal_with_result(
            AttemptOutcome::Succeeded,
            &findings_text(&format!("{},{}", qualified, qualified.replace("F1", "F2"))),
        )]);
        let long_file = "a".repeat(80);
        let verifier_entry = entry(",\"propagates\":\"seed:F1\",\"lens\":\"tests\",\"evidence\":\"observed\",\"proposed_fix\":\"repair\"").replace("a.rs", &long_file);
        let entries = [
            vec![verifier_entry.clone(); MAX_FINDINGS / 2],
            vec![verifier_entry.replace("F1", "F2"); MAX_FINDINGS / 2],
        ]
        .concat()
        .join(",");
        let verifier_text = findings_text(&if verifying {
            entries.replace(
                "\"status\":\"open\"",
                "\"status\":\"open\",\"verification\":\"upheld\"",
            )
        } else {
            entries
        });
        for i in 0..20 {
            let mut verifier = node(&format!("verifier{i}"), route("first"));
            verifier.depends_on = vec!["seed".into()];
            verifier.inputs = vec![input("seed")];
            nodes.push(verifier);
            observations.push_back(terminal_with_result(
                AttemptOutcome::Succeeded,
                &verifier_text,
            ));
        }
        let mut synthesis = node("synthesis", route("first"));
        synthesis.depends_on = (0..20).map(|i| format!("verifier{i}")).collect();
        synthesis.inputs = synthesis.depends_on.iter().map(|id| input(id)).collect();
        if verifying {
            synthesis
                .extra
                .insert("review_behaviour".into(), "verify".into());
        } else {
            synthesis.dispositions = DISPOSITIONS.map(String::from).to_vec();
        }
        nodes.push(synthesis);
        let mut next = node("next", route("first"));
        next.depends_on = vec!["synthesis".into()];
        next.inputs = vec![input("synthesis")];
        nodes.push(next);
        let mut decisions = serde_json::json!({"id":"F1", "disposition":"accepted", "reason":"yes", "detail":"detail", "verification":"falsified"});
        if verifying {
            decisions["triage"] = serde_json::json!({"decision":"rejected","reason":"falsified against code","worth_fixing":false,
                "root_cause":"input handling","blocking_class":"none"});
        }
        observations.push_back(terminal_with_result(
            AttemptOutcome::Succeeded,
            &findings_text(&format!(
                "{},{}",
                decisions,
                decisions.to_string().replace("F1", "F2")
            ))
            .replace("VERDICT: PASS", "VERDICT: FAIL"),
        ));
        observations.push_back(terminal_with_result(
            AttemptOutcome::Succeeded,
            &findings_text(""),
        ));
        let document = graph(nodes);
        let mut journal = MemoryJournal::default();
        let mut owner = FakeOwner {
            observations,
            ..Default::default()
        };
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        let synthesis = run
            .checkpoint
            .nodes
            .iter()
            .find(|node| node.id == "synthesis")
            .unwrap();
        assert_eq!(
            synthesis.state,
            NodeState::Succeeded,
            "{:?}",
            synthesis.advance_error
        );
        let outputs = synthesis.outputs.as_ref().unwrap();
        assert!(
            outputs
                .warnings
                .iter()
                .any(|warning| warning.contains("findings compacted"))
        );
        assert!(outputs.findings.is_empty());
        let expanded =
            loops::expand(&document, &[], &crate::config::GraphLimits::default()).unwrap();
        let (plan, positions) =
            validate_flat_plan(&expanded, &crate::config::GraphLimits::default()).unwrap();
        let reconstructed =
            materialised_findings(positions["synthesis"], &plan, &run.checkpoint, &positions);
        let bound = graph_envelope_bound(&document, &[], &[], &plan);
        assert!(canonical_len(&run.checkpoint) <= bound);
        let mut reporting = run.checkpoint.clone();
        materialise_checkpoint_findings(&document, &mut reporting).unwrap();
        assert_eq!(
            reporting.nodes[positions["synthesis"]]
                .outputs
                .as_ref()
                .unwrap()
                .findings
                .len(),
            20 * MAX_FINDINGS
        );
        assert_eq!(reconstructed.len(), 20 * MAX_FINDINGS);
        if verifying {
            assert!(
                reconstructed
                    .iter()
                    .all(|finding| finding.extra["verification"] == "falsified")
            );
        }
        assert!(reconstructed.iter().all(|finding| {
            (finding.id == "F1" || finding.id == "F2")
                && finding.severity == "note"
                && finding.file == long_file
                && finding
                    .extra
                    .get("lens")
                    .and_then(serde_json::Value::as_str)
                    == Some("tests")
                && finding
                    .extra
                    .get("evidence")
                    .and_then(serde_json::Value::as_str)
                    == Some("observed")
                && finding
                    .extra
                    .get("proposed_fix")
                    .and_then(serde_json::Value::as_str)
                    == Some("repair")
        }));
        assert_eq!(
            run.checkpoint.nodes.last().unwrap().state,
            NodeState::Succeeded
        );
        let prompt = owner.prompts.last().unwrap();
        assert!(
            prompt.contains("F1")
                && prompt.contains("F2")
                && prompt.contains("note")
                && prompt.contains(&long_file)
        );
        let stored = serde_json::to_vec(journal.0.as_ref().unwrap()).unwrap();
        journal.0 = Some(serde_json::from_slice(&stored).unwrap());
        let replay = advance(&document, &mut FakeOwner::default(), &mut journal).unwrap();
        assert_eq!(run.checkpoint, replay.checkpoint);
        let checkpoint = journal.0.as_mut().unwrap();
        let outputs = checkpoint
            .nodes
            .iter_mut()
            .find(|node| node.id == "synthesis")
            .unwrap()
            .outputs
            .as_mut()
            .unwrap();
        outputs.extra.insert(
            "_compact_disposition_findings".into(),
            serde_json::json!("{}"),
        );
        assert!(matches!(
            advance(&document, &mut FakeOwner::default(), &mut journal),
            Err(GraphError::BadCheckpoint { .. })
        ));
    }
}

#[test]
fn compacted_synthesis_survives_composite_export_and_replay() {
    let input = |id: &str| GraphInput {
        required_outputs: Vec::new(),
        extra: Default::default(),
        node: id.into(),
        outputs: vec![OutputKind::Findings],
    };
    let mut nodes = vec![node("seed", route("first"))];
    let mut observations = VecDeque::from([terminal_with_result(
        AttemptOutcome::Succeeded,
        &findings_text(&entry("")),
    )]);
    let long_file = "a".repeat(180);
    let verifier_entry = entry(",\"propagates\":\"seed:F1\"").replace("a.rs", &long_file);
    let entries = vec![verifier_entry; MAX_FINDINGS].join(",");
    let verifier_text = findings_text(&entries);
    for i in 0..20 {
        let mut verifier = node(&format!("verifier{i}"), route("first"));
        verifier.depends_on = vec!["seed".into()];
        verifier.inputs = vec![input("seed")];
        nodes.push(verifier);
        observations.push_back(terminal_with_result(
            AttemptOutcome::Succeeded,
            &verifier_text,
        ));
    }
    let mut synthesis = node("synthesis", route("first"));
    synthesis.depends_on = (0..20).map(|i| format!("verifier{i}")).collect();
    synthesis.inputs = synthesis.depends_on.iter().map(|id| input(id)).collect();
    synthesis.dispositions = DISPOSITIONS.map(String::from).to_vec();
    let mut outer = node("outer", route("first"));
    outer.kind = Some(NodeKind::Composite);
    outer.route = None;
    outer.prompt.clear();
    outer.depends_on = synthesis.depends_on.clone();
    outer.inputs = synthesis.inputs.clone();
    synthesis.depends_on.clear();
    synthesis.inputs.clear();
    outer.composite = Some(CompositeSpec {
        reduce_verdict: false,
        min_successful: None,
        nodes: vec![synthesis],
        exports: vec![input("synthesis")],
    });
    nodes.push(outer);
    let mut next = node("next", route("first"));
    next.depends_on = vec!["outer".into()];
    next.inputs = vec![input("outer")];
    nodes.push(next);
    let decisions =
        serde_json::json!({"id":"F1", "disposition":"accepted", "reason":"yes", "detail":"detail"});
    observations.push_back(terminal_with_result(
        AttemptOutcome::Succeeded,
        &findings_text(&decisions.to_string()),
    ));
    observations.push_back(terminal_with_result(
        AttemptOutcome::Succeeded,
        &findings_text(""),
    ));
    let document = graph(nodes);
    let mut journal = MemoryJournal::default();
    let mut owner = FakeOwner {
        observations,
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    let synthesis = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "outer--synthesis")
        .unwrap();
    assert_eq!(
        synthesis.state,
        NodeState::Succeeded,
        "{:?}",
        synthesis.advance_error
    );
    let outputs = synthesis.outputs.as_ref().unwrap();
    assert!(
        outputs
            .warnings
            .iter()
            .any(|warning| warning.contains("findings compacted"))
    );
    assert!(outputs.findings.is_empty());
    let expanded = loops::expand(&document, &[], &crate::config::GraphLimits::default()).unwrap();
    let (plan, positions) =
        validate_flat_plan(&expanded, &crate::config::GraphLimits::default()).unwrap();
    let reconstructed = materialised_findings(
        positions["outer--synthesis"],
        &plan,
        &run.checkpoint,
        &positions,
    );
    let bound = graph_envelope_bound(&document, &[], &[], &plan);
    assert!(canonical_len(&run.checkpoint) <= bound);
    assert_eq!(reconstructed.len(), 20 * MAX_FINDINGS);
    assert!(reconstructed.iter().all(|finding| finding.id == "F1"
        && finding.severity == "note"
        && finding.file == long_file));
    assert_eq!(
        run.checkpoint.nodes.last().unwrap().state,
        NodeState::Succeeded
    );
    let prompt = owner.prompts.last().unwrap();
    assert!(prompt.contains("F1") && prompt.contains("note") && prompt.contains(&long_file));
    let identities = truncated_finding_ids(
        &plan.nodes[positions["next"]],
        &plan,
        &run.checkpoint,
        &positions,
    );
    assert_eq!(identities.len(), 20 * MAX_FINDINGS);
    let stored = serde_json::to_vec(journal.0.as_ref().unwrap()).unwrap();
    journal.0 = Some(serde_json::from_slice(&stored).unwrap());
    let replay = advance(&document, &mut FakeOwner::default(), &mut journal).unwrap();
    assert_eq!(run.checkpoint, replay.checkpoint);
}

#[test]
fn settled_copies_keep_origin_and_evidence_for_downstream_disposition() {
    let input = |id: &str| GraphInput {
        required_outputs: Vec::new(),
        extra: Default::default(),
        node: id.into(),
        outputs: vec![OutputKind::Findings],
    };
    let mut verify = node("verify", route("first"));
    verify.depends_on = vec!["lens".into()];
    verify.inputs = vec![input("lens")];
    let mut synthesis = node("synthesis", route("first"));
    synthesis.dispositions = DISPOSITIONS.map(String::from).to_vec();
    synthesis.depends_on = vec!["lens".into(), "verify".into()];
    synthesis.inputs = vec![input("lens"), input("verify")];
    let mut downstream = node("downstream", route("first"));
    downstream.dispositions = DISPOSITIONS.map(String::from).to_vec();
    downstream.depends_on = vec!["lens".into(), "synthesis".into()];
    downstream.inputs = vec![input("lens"), input("synthesis")];
    let decision = findings_text(r#"{"id":"F1","disposition":"accepted","reason":"verified"}"#);
    let mut owner = FakeOwner {
        observations: [
            terminal_with_result(
                AttemptOutcome::Succeeded,
                &findings_text(&entry(",\"original\":\"lens-evidence\"")),
            ),
            terminal_with_result(
                AttemptOutcome::Succeeded,
                &findings_text(&entry(
                    ",\"verification\":\"verified-evidence\",\"propagates\":\"lens:F1\"",
                )),
            ),
            terminal_with_result(AttemptOutcome::Succeeded, &decision),
            terminal_with_result(AttemptOutcome::Succeeded, &decision),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![
            node("lens", route("first")),
            verify,
            synthesis,
            downstream,
        ]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    let settled = &run.checkpoint.nodes[3];
    assert_eq!(
        settled.state,
        NodeState::Succeeded,
        "{:?}",
        settled.advance_error
    );
    let findings = &settled.outputs.as_ref().unwrap().findings;
    assert_eq!(findings.len(), 3);
    assert!(findings.iter().all(|f| f.id == "F1"
        && f.disposition.as_deref() == Some("accepted")
        && f.reason.as_deref() == Some("verified")));
    assert_eq!(
        findings
            .iter()
            .filter(|f| f
                .extra
                .get("original")
                .is_some_and(|v| v == "lens-evidence"))
            .count(),
        2
    );
    assert_eq!(
        findings
            .iter()
            .filter(|f| f
                .extra
                .get("verification")
                .is_some_and(|v| v == "verified-evidence"))
            .count(),
        1
    );
}

// Small: the verifier sees a composite-qualified ID and synthesis accepts its bare alias.
#[test]
fn composite_qualified_propagation_resolves_identity_only_disposition() {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version":1, "graph_id":"composite-propagation", "nodes":[
            {"id":"lenses", "kind":"composite", "access":"read_only", "composite":{
                "nodes":[{"id":"lens-correctness-spec", "access":"read_only", "prompt":"review", "route":{"alias":"senior"}}],
                "exports":[{"node":"lens-correctness-spec", "outputs":["findings"]}]
            }},
            {"id":"verify", "access":"read_only", "prompt":"verify", "route":{"alias":"senior"},
             "depends_on":["lenses"], "inputs":[{"node":"lenses", "outputs":["findings"]}]},
            {"id":"synthesis", "kind":"synthesise", "access":"read_only", "prompt":"dispose", "route":{"alias":"senior"},
             "depends_on":["lenses", "verify"], "inputs":[{"node":"lenses", "outputs":["findings"]}, {"node":"verify", "outputs":["findings"]}],
             "dispositions":["accepted", "rejected", "deferred"]}
        ]
    })).unwrap();
    let propagated = entry(
        ",\"verification\":\"upheld\",\"propagates\":\"lenses:lenses--lens-correctness-spec:F1\"",
    )
    .replace(
        "\"id\":\"F1\"",
        "\"id\":\"lenses--lens-correctness-spec:F1\"",
    );
    let mut owner = FakeOwner {
        observations: [
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&entry(""))),
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&propagated)),
            terminal_with_result(
                AttemptOutcome::Succeeded,
                &findings_text(r#"{"id":"F1","disposition":"accepted","reason":"verified"}"#),
            ),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    let synthesis = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "synthesis")
        .unwrap();
    assert_eq!(
        synthesis.state,
        NodeState::Succeeded,
        "{:?}",
        synthesis.advance_error
    );
    let findings = &synthesis.outputs.as_ref().unwrap().findings;
    assert_eq!(findings.len(), 2);
    assert!(
        findings
            .iter()
            .all(|f| f.id == "lenses--lens-correctness-spec:F1")
    );
    assert!(
        findings
            .iter()
            .any(|f| f.extra.get("verification").is_some_and(|v| v == "upheld"))
    );
}

// A verifier reuses short ids across lenses and names each lens finding without
// the composite prefix; synthesis settles the lens findings by qualified id.
