use super::*;

#[test]
fn verifier_markers_without_the_composite_prefix_keep_each_lens_finding_separate() {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version":1, "graph_id":"short-markers", "nodes":[
            {"id":"lenses", "kind":"composite", "access":"read_only", "composite":{
                "nodes":[
                    {"id":"lens-a", "access":"read_only", "prompt":"review", "route":{"alias":"senior"}},
                    {"id":"lens-b", "access":"read_only", "prompt":"review", "route":{"alias":"senior"}}
                ],
                "exports":[
                    {"node":"lens-a", "outputs":["findings"]},
                    {"node":"lens-b", "outputs":["findings"]}
                ]
            }},
            {"id":"verify", "access":"read_only", "prompt":"verify", "route":{"alias":"senior"},
             "depends_on":["lenses"], "inputs":[{"node":"lenses", "outputs":["findings"]}]},
            {"id":"synthesis", "kind":"synthesise", "access":"read_only", "prompt":"dispose", "route":{"alias":"senior"},
             "depends_on":["lenses", "verify"], "inputs":[{"node":"lenses", "outputs":["findings"]}, {"node":"verify", "outputs":["findings"]}],
             "dispositions":["accepted", "rejected", "deferred"]}
        ]
    })).unwrap();
    let verified = [
        entry(",\"propagates\":\"lenses--lens-a:F1\""),
        entry(",\"propagates\":\"lenses--lens-b:F1\""),
    ]
    .join(",");
    let settled = [
        r#"{"id":"lenses--lens-a:F1","disposition":"accepted","reason":"reproduced"}"#,
        r#"{"id":"lenses--lens-b:F1","disposition":"rejected","reason":"contradicted"}"#,
    ]
    .join(",");
    let mut owner = FakeOwner {
        observations: [
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&entry(""))),
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&entry(""))),
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&verified)),
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&settled)),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    let node = |id: &str| {
        run.checkpoint
            .nodes
            .iter()
            .find(|node| node.id == id)
            .unwrap()
    };
    let synthesis = node("synthesis");
    assert_eq!(
        synthesis.state,
        NodeState::Succeeded,
        "{:?}",
        synthesis.advance_error
    );
    assert!(
        !node("verify")
            .outputs
            .as_ref()
            .unwrap()
            .warnings
            .iter()
            .any(|warning| warning.contains("unresolved propagates"))
    );
    let disposition = |id: &str| {
        synthesis
            .outputs
            .as_ref()
            .unwrap()
            .findings
            .iter()
            .filter(|finding| finding.id == id)
            .map(|finding| finding.disposition.as_deref())
            .collect::<std::collections::BTreeSet<_>>()
    };
    assert_eq!(
        disposition("lenses--lens-a:F1"),
        [Some("accepted")].into_iter().collect()
    );
    assert_eq!(
        disposition("lenses--lens-b:F1"),
        [Some("rejected")].into_iter().collect()
    );
}

// A bare id shared by two roots names neither of them.
#[test]
fn a_marker_shared_by_several_findings_stays_unresolved() {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version":1, "graph_id":"ambiguous-marker", "nodes":[
            {"id":"lenses", "kind":"composite", "access":"read_only", "composite":{
                "nodes":[
                    {"id":"lens-a", "access":"read_only", "prompt":"review", "route":{"alias":"senior"}},
                    {"id":"lens-b", "access":"read_only", "prompt":"review", "route":{"alias":"senior"}}
                ],
                "exports":[
                    {"node":"lens-a", "outputs":["findings"]},
                    {"node":"lens-b", "outputs":["findings"]}
                ]
            }},
            {"id":"verify", "access":"read_only", "prompt":"verify", "route":{"alias":"senior"},
             "depends_on":["lenses"], "inputs":[{"node":"lenses", "outputs":["findings"]}]}
        ]
    })).unwrap();
    let mut owner = FakeOwner {
        observations: [
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&entry(""))),
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&entry(""))),
            terminal_with_result(
                AttemptOutcome::Succeeded,
                &findings_text(&entry(",\"propagates\":\"F1\"")),
            ),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    let verify = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "verify")
        .unwrap();
    assert!(
        verify
            .outputs
            .as_ref()
            .unwrap()
            .warnings
            .iter()
            .any(|warning| warning.contains("unresolved propagates"))
    );
}

#[test]
fn qualified_propagations_group_colliding_originals() {
    let input = |id: &str| GraphInput {
        required_outputs: Vec::new(),
        extra: Default::default(),
        node: id.into(),
        outputs: vec![OutputKind::Findings],
    };
    let mut verify = node("verify", route("first"));
    verify.depends_on = vec!["a".into(), "b".into()];
    verify.inputs = vec![input("a"), input("b")];
    let mut synthesis = node("synthesis", route("first"));
    synthesis.dispositions = DISPOSITIONS.map(String::from).to_vec();
    synthesis.depends_on = vec!["a".into(), "b".into(), "verify".into()];
    synthesis.inputs = vec![input("a"), input("b"), input("verify")];
    let propagated = ["a", "b"]
        .map(|source| {
            entry(&format!(
                ",\"verification\":\"{source}-verified\",\"propagates\":\"{source}:F1\""
            ))
            .replace("\"id\":\"F1\"", &format!("\"id\":\"{source}:F1\""))
        })
        .join(",");
    let decisions = r#"{"id":"a:F1","disposition":"accepted","reason":"yes"},{"id":"b:F1","disposition":"rejected","reason":"no"}"#;
    let mut owner = FakeOwner {
        observations: [
            terminal_with_result(
                AttemptOutcome::Succeeded,
                &findings_text(&entry(",\"original\":\"a-original\"")),
            ),
            terminal_with_result(
                AttemptOutcome::Succeeded,
                &findings_text(&entry(",\"original\":\"b-original\"")),
            ),
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&propagated)),
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text(decisions)),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![
            node("a", route("first")),
            node("b", route("first")),
            verify,
            synthesis,
        ]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert_eq!(
        run.checkpoint.nodes[3].state,
        NodeState::Succeeded,
        "{:?}",
        run.checkpoint.nodes[3].advance_error
    );
    let prompt = &owner.prompts[3];
    assert!(prompt.contains("a-verified") && prompt.contains("b-verified"));
    assert!(prompt.contains("a-original") && prompt.contains("b-original"));
    let findings = &run.checkpoint.nodes[3].outputs.as_ref().unwrap().findings;
    assert_eq!(
        findings.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(),
        vec!["a:F1", "a:F1", "b:F1", "b:F1"]
    );
    assert_eq!(findings[0].extra["original"], "a-original");
    assert_eq!(findings[1].extra["verification"], "a-verified");
    assert_eq!(findings[2].extra["original"], "b-original");
    assert_eq!(findings[3].extra["verification"], "b-verified");
}

#[test]
fn multiple_verifiers_share_one_disposition_and_keep_evidence() {
    let input = |id: &str| GraphInput {
        required_outputs: Vec::new(),
        extra: Default::default(),
        node: id.into(),
        outputs: vec![OutputKind::Findings],
    };
    let verifier = |id: &str| {
        let mut n = node(id, route("first"));
        n.depends_on = vec!["lens".into()];
        n.inputs = vec![input("lens")];
        n
    };
    let mut synthesis = node("synthesis", route("first"));
    synthesis.dispositions = DISPOSITIONS.map(String::from).to_vec();
    synthesis.depends_on = vec!["lens".into(), "verify".into(), "verify2".into()];
    synthesis.inputs = vec![input("lens"), input("verify"), input("verify2")];
    let mut owner = FakeOwner {
        observations: [
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&entry(""))),
            terminal_with_result(
                AttemptOutcome::Succeeded,
                &findings_text(&entry(
                    ",\"verification\":\"evidence-one\",\"propagates\":\"lens:F1\"",
                )),
            ),
            terminal_with_result(
                AttemptOutcome::Succeeded,
                &findings_text(&entry(
                    ",\"verification\":\"evidence-two\",\"propagates\":\"lens:F1\"",
                )),
            ),
            terminal_with_result(
                AttemptOutcome::Succeeded,
                &findings_text(r#"{"id":"F1","disposition":"accepted","reason":"verified","verification":"combined"}"#),
            ),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![
            node("lens", route("first")),
            verifier("verify"),
            verifier("verify2"),
            synthesis,
        ]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert_eq!(
        run.checkpoint.nodes[3].state,
        NodeState::Succeeded,
        "{:?}",
        run.checkpoint.nodes[3].advance_error
    );
    let prompt = &owner.prompts[3];
    assert!(prompt.contains("evidence-one") && prompt.contains("evidence-two"));
    assert!(!prompt.contains("verify:F1") && !prompt.contains("verify2:F1"));
    let findings = &run.checkpoint.nodes[3].outputs.as_ref().unwrap().findings;
    assert_eq!(findings.len(), 3);
    assert!(
        findings
            .iter()
            .all(|f| f.id == "F1" && f.disposition.as_deref() == Some("accepted"))
    );
    assert_eq!(findings[1].extra["verification"], "evidence-one");
    assert_eq!(findings[2].extra["verification"], "evidence-two");
}

#[test]
fn chain_resolves_rendered_markers_and_preserves_every_copy() {
    let input = |id: &str| GraphInput {
        required_outputs: Vec::new(),
        extra: Default::default(),
        node: id.into(),
        outputs: vec![OutputKind::Findings],
    };
    let mut v1 = node("v1", route("first"));
    v1.depends_on = vec!["a".into(), "b".into()];
    v1.inputs = vec![input("a"), input("b")];
    let mut v2 = node("v2", route("first"));
    v2.depends_on = vec!["v1".into()];
    v2.inputs = vec![input("v1")];
    let mut synth = node("synthesis", route("first"));
    synth.dispositions = DISPOSITIONS.map(String::from).to_vec();
    synth.depends_on = vec!["a".into(), "b".into(), "v1".into(), "v2".into()];
    synth.inputs = vec![input("a"), input("b"), input("v1"), input("v2")];
    let propagated = entry(",\"evidence\":\"middle\",\"propagates\":\"a:a:F1\"");
    let last = entry(",\"evidence\":\"last\",\"propagates\":\"v1:F1\"");
    let decisions = r#"{"id":"a:F1","disposition":"accepted","reason":"yes"},{"id":"b:F1","disposition":"rejected","reason":"no"}"#;
    let mut owner = FakeOwner {
        observations: [
            terminal_with_result(
                AttemptOutcome::Succeeded,
                &findings_text(&entry(",\"evidence\":\"first\"")),
            ),
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&entry(""))),
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&propagated)),
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&last)),
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text(decisions)),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![
            node("a", route("first")),
            node("b", route("first")),
            v1,
            v2,
            synth,
        ]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    let progress = &run.checkpoint.nodes[4];
    assert_eq!(
        progress.state,
        NodeState::Succeeded,
        "{:?}",
        progress.advance_error
    );
    let findings = &progress.outputs.as_ref().unwrap().findings;
    assert_eq!(findings.len(), 4);
    let copies: Vec<_> = findings.iter().filter(|f| f.id == "a:F1").collect();
    assert_eq!(copies.len(), 3);
    for (finding, evidence) in copies.iter().zip(["first", "middle", "last"]) {
        assert_eq!(finding.extra["evidence"], evidence);
        assert_eq!(finding.disposition.as_deref(), Some("accepted"));
        assert!(owner.prompts[4].contains(evidence));
    }
    assert!(
        findings
            .iter()
            .any(|f| f.id == "b:F1" && f.disposition.as_deref() == Some("rejected"))
    );
}

#[test]
fn result_only_inputs_do_not_change_rendered_finding_ids() {
    let input = |id: &str, output| GraphInput {
        required_outputs: Vec::new(),
        extra: Default::default(),
        node: id.into(),
        outputs: vec![output],
    };
    let mut verify = node("verify", route("first"));
    verify.depends_on = vec!["a".into(), "b".into(), "c".into()];
    verify.inputs = vec![
        input("a", OutputKind::Result),
        input("b", OutputKind::Findings),
        input("c", OutputKind::Findings),
    ];
    let mut synth = node("synthesis", route("first"));
    synth.dispositions = DISPOSITIONS.map(String::from).to_vec();
    synth.depends_on = vec!["b".into(), "verify".into()];
    synth.inputs = vec![
        input("b", OutputKind::Findings),
        input("verify", OutputKind::Findings),
    ];
    let mut owner = FakeOwner {
        observations: [
            terminal_with_result(
                AttemptOutcome::Succeeded,
                &findings_text(&entry("").replace("\"id\":\"F1\"", "\"id\":\"b:F1\"")),
            ),
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&entry(""))),
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&entry(""))),
            terminal_with_result(
                AttemptOutcome::Succeeded,
                &findings_text(&entry(",\"propagates\":\"b:b:F1\"")),
            ),
            terminal_with_result(
                AttemptOutcome::Succeeded,
                &findings_text(r#"{"id":"F1","disposition":"accepted","reason":"yes"}"#),
            ),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![
            node("a", route("first")),
            node("b", route("first")),
            node("c", route("first")),
            verify,
            synth,
        ]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert_eq!(
        run.checkpoint.nodes[4].state,
        NodeState::Succeeded,
        "{:?}",
        run.checkpoint.nodes[4].advance_error
    );
    assert_eq!(
        run.checkpoint.nodes[4]
            .outputs
            .as_ref()
            .unwrap()
            .findings
            .len(),
        2
    );
    assert!(
        !run.checkpoint.nodes[3]
            .outputs
            .as_ref()
            .unwrap()
            .warnings
            .iter()
            .any(|warning| warning.contains("unresolved propagates"))
    );
}

#[test]
fn qualified_id_collision_requires_three_distinct_dispositions() {
    let input = |id: &str| GraphInput {
        required_outputs: Vec::new(),
        extra: Default::default(),
        node: id.into(),
        outputs: vec![OutputKind::Findings],
    };
    for (decisions, expected) in [
        (
            r#"{"id":"a:F1","disposition":"accepted","reason":"yes"},{"id":"b:F1","disposition":"rejected","reason":"no"}"#,
            NodeState::Failed,
        ),
        (
            r#"{"id":"a:F1:1","disposition":"accepted","reason":"yes"},{"id":"b:F1","disposition":"rejected","reason":"no"},{"id":"a:F1","disposition":"deferred","reason":"later"}"#,
            NodeState::Succeeded,
        ),
    ] {
        let mut synth = node("synthesis", route("first"));
        synth.dispositions = DISPOSITIONS.map(String::from).to_vec();
        synth.depends_on = vec!["a".into(), "b".into(), "c".into()];
        synth.inputs = vec![input("a"), input("b"), input("c")];
        let mut owner = FakeOwner {
            observations: [
                terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&entry(""))),
                terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&entry(""))),
                terminal_with_result(
                    AttemptOutcome::Succeeded,
                    &findings_text(&entry("").replace("\"id\":\"F1\"", "\"id\":\"a:F1\"")),
                ),
                terminal_with_result(AttemptOutcome::Succeeded, &findings_text(decisions)),
            ]
            .into(),
            ..Default::default()
        };
        let run = advance(
            &graph(vec![
                node("a", route("first")),
                node("b", route("first")),
                node("c", route("first")),
                synth,
            ]),
            &mut owner,
            &mut MemoryJournal::default(),
        )
        .unwrap();
        assert_eq!(
            run.checkpoint.nodes[3].state, expected,
            "{:?}",
            run.checkpoint.nodes[3].advance_error
        );
    }
}

#[test]
fn ambiguous_propagation_warns_and_remains_independent() {
    let input = |id: &str| GraphInput {
        required_outputs: Vec::new(),
        extra: Default::default(),
        node: id.into(),
        outputs: vec![OutputKind::Findings],
    };
    let mut a = node("a", route("first"));
    a.depends_on = vec!["b".into()];
    a.inputs = vec![input("b")];
    let mut verify = node("verify", route("first"));
    verify.depends_on = vec!["a".into(), "b".into(), "c".into()];
    verify.inputs = vec![input("a"), input("b"), input("c")];
    let mut synth = node("synthesis", route("first"));
    synth.dispositions = DISPOSITIONS.map(String::from).to_vec();
    synth.depends_on = vec!["b".into(), "verify".into()];
    synth.inputs = vec![input("b"), input("verify")];
    // a:b:F1 matches the stored ID of a's independent finding and the
    // rendered ID of a's propagated copy of b:F1.
    let renamed = entry("").replace("\"id\":\"F1\"", "\"id\":\"b:F1\"");
    let a_findings = format!("{},{}", entry(",\"propagates\":\"b:F1\""), renamed);
    let c_findings = format!("{},{}", entry(""), renamed);
    let decisions = r#"{"id":"b:F1","disposition":"accepted","reason":"yes"},{"id":"verify:F1","disposition":"rejected","reason":"no"}"#;
    let mut owner = FakeOwner {
        observations: [
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&entry(""))),
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&c_findings)),
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&a_findings)),
            terminal_with_result(
                AttemptOutcome::Succeeded,
                &findings_text(&entry(",\"propagates\":\"a:b:F1\"")),
            ),
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text(decisions)),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![
            node("b", route("first")),
            node("c", route("first")),
            a,
            verify,
            synth,
        ]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    let warning = &run.checkpoint.nodes[3].outputs.as_ref().unwrap().warnings;
    assert!(
        warning
            .iter()
            .any(|w| w.contains("unresolved propagates marker") && w.contains("a:b:F1"))
    );
    assert_eq!(
        run.checkpoint.nodes[4].state,
        NodeState::Succeeded,
        "{:?}",
        run.checkpoint.nodes[4].advance_error
    );
    let findings = &run.checkpoint.nodes[4].outputs.as_ref().unwrap().findings;
    assert_eq!(
        findings.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(),
        ["b:F1", "verify:F1"]
    );
    assert_eq!(findings[0].disposition.as_deref(), Some("accepted"));
    assert_eq!(findings[1].disposition.as_deref(), Some("rejected"));
}

#[test]
fn unresolved_marker_warns_and_remains_an_independent_identity() {
    let input = |id: &str| GraphInput {
        required_outputs: Vec::new(),
        extra: Default::default(),
        node: id.into(),
        outputs: vec![OutputKind::Findings],
    };
    let mut verify = node("verify", route("first"));
    verify.depends_on = vec!["a".into()];
    verify.inputs = vec![input("a")];
    let mut synth = node("synthesis", route("first"));
    synth.dispositions = DISPOSITIONS.map(String::from).to_vec();
    synth.depends_on = vec!["a".into(), "verify".into()];
    synth.inputs = vec![input("a"), input("verify")];
    let decisions = r#"{"id":"a:F1","disposition":"accepted","reason":"yes"},{"id":"verify:F1","disposition":"rejected","reason":"no"}"#;
    let mut owner = FakeOwner {
        observations: [
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&entry(""))),
            terminal_with_result(
                AttemptOutcome::Succeeded,
                &findings_text(&entry(",\"propagates\":\"not-an-input:F1\"")),
            ),
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text(decisions)),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![node("a", route("first")), verify, synth]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert!(
        run.checkpoint.nodes[1]
            .outputs
            .as_ref()
            .unwrap()
            .warnings
            .iter()
            .any(|w| w.contains("unresolved propagates marker") && w.contains("not-an-input"))
    );
    assert_eq!(run.checkpoint.nodes[2].state, NodeState::Succeeded);
    let findings = &run.checkpoint.nodes[2].outputs.as_ref().unwrap().findings;
    assert_eq!(
        findings.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(),
        ["a:F1", "verify:F1"]
    );
}

#[test]
fn dependent_reviewers_with_independent_same_ids_require_distinct_dispositions() {
    let a = node("a", route("first"));
    let mut b = node("b", route("first"));
    b.depends_on = vec!["a".into()];
    b.inputs = vec![GraphInput {
        required_outputs: Vec::new(),
        extra: Default::default(),
        node: "a".into(),
        outputs: vec![OutputKind::Result],
    }];
    let mut synthesis = node("synthesis", route("first"));
    synthesis.dispositions = DISPOSITIONS.map(String::from).to_vec();
    synthesis.depends_on = vec!["a".into(), "b".into()];
    synthesis.inputs = ["a", "b"]
        .into_iter()
        .map(|id| GraphInput {
            required_outputs: Vec::new(),
            extra: Default::default(),
            node: id.into(),
            outputs: vec![OutputKind::Findings],
        })
        .collect();
    for (decisions, expected) in [
        (
            r#"{"id":"a:F1","disposition":"accepted","reason":"yes"}"#,
            NodeState::Failed,
        ),
        (
            r#"{"id":"a:F1","disposition":"accepted","reason":"yes"},{"id":"b:F1","disposition":"rejected","reason":"no"}"#,
            NodeState::Succeeded,
        ),
    ] {
        let mut owner = FakeOwner {
            observations: [
                terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&entry(""))),
                terminal_with_result(
                    AttemptOutcome::Succeeded,
                    &findings_text(
                        &entry("").replace("\"summary\":\"s\"", "\"summary\":\"other\""),
                    ),
                ),
                terminal_with_result(AttemptOutcome::Succeeded, &findings_text(decisions)),
            ]
            .into(),
            ..Default::default()
        };
        let run = advance(
            &graph(vec![a.clone(), b.clone(), synthesis.clone()]),
            &mut owner,
            &mut MemoryJournal::default(),
        )
        .unwrap();
        assert!(owner.prompts[2].contains("a:F1") && owner.prompts[2].contains("b:F1"));
        assert_eq!(run.checkpoint.nodes[2].state, expected);
    }
}

#[test]
fn unknown_disposition_does_not_hide_an_undisposed_finding_on_replay() {
    let mut synthesis = node("synthesis", route("first"));
    synthesis.dispositions = DISPOSITIONS.map(String::from).to_vec();
    synthesis.depends_on = vec!["review".into()];
    synthesis.inputs = vec![GraphInput {
        required_outputs: Vec::new(),
        extra: Default::default(),
        node: "review".into(),
        outputs: vec![OutputKind::Findings],
    }];
    let document = graph(vec![node("review", route("first")), synthesis]);
    let upstream = format!("{},{}", entry(""), entry("").replace("F1", "F2"));
    let decisions = r#"{"id":"F1","disposition":"accepted","reason":"yes"},{"id":"F9","disposition":"rejected","reason":"no"}"#;
    let mut owner = FakeOwner {
        observations: [
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&upstream)),
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text(decisions)),
        ]
        .into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.checkpoint.nodes[1].state, NodeState::Failed);
    assert_eq!(
        run.checkpoint.nodes[1].advance_error_code.as_deref(),
        Some("disposition_count")
    );
    let replay = advance(&document, &mut FakeOwner::default(), &mut journal).unwrap();
    assert_eq!(replay.checkpoint.nodes[1].state, NodeState::Failed);
    assert_eq!(
        replay.checkpoint.nodes[1].advance_error_code.as_deref(),
        Some("disposition_count")
    );
}

#[test]
fn required_findings_prompt_includes_empty_example_even_when_fence_is_mentioned() {
    let mut lens = node("lens", route("first"));
    lens.require_findings = true;
    lens.prompt = "Review the existing ```findings block.".into();
    let mut owner = FakeOwner {
        observations: VecDeque::from([terminal_with_result(
            AttemptOutcome::Succeeded,
            "```findings\n{\"schema_version\":1,\"findings\":[]}\n```",
        )]),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![lens]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert_eq!(run.checkpoint.nodes[0].state, NodeState::Succeeded);
    let (_, example) = owner.prompts[0]
        .rsplit_once("```findings\n")
        .expect("required findings prompt includes a fenced example");
    let (example, _) = example
        .split_once("\n```")
        .expect("findings example has a closing fence");
    let example: serde_json::Value =
        serde_json::from_str(example).expect("findings example is valid JSON");
    assert_eq!(example["schema_version"], 1);
    let findings = example["findings"]
        .as_array()
        .expect("findings example contains an array");
    let finding = findings
        .first()
        .and_then(serde_json::Value::as_object)
        .expect("findings example demonstrates a finding");
    for field in ["id", "severity", "file", "summary", "status"] {
        assert!(
            finding.get(field).and_then(serde_json::Value::as_str).is_some(),
            "finding example has a string {field}"
        );
    }
}

#[test]
fn synthesis_recovers_upheld_findings_when_verifier_block_is_truncated() {
    let mut lens = node("lens", route("first"));
    lens.require_findings = true;

    let mut verify = node("verify", route("first"));
    verify.depends_on = vec!["lens".into()];
    verify.inputs = vec![GraphInput {
        required_outputs: Vec::new(),
        extra: Default::default(),
        node: "lens".into(),
        outputs: vec![OutputKind::Result, OutputKind::Findings],
    }];

    let mut synthesis = node("synthesis", route("first"));
    synthesis.kind = Some(NodeKind::Synthesise);
    synthesis.dispositions = DISPOSITIONS.map(String::from).to_vec();
    synthesis.depends_on = vec!["lens".into(), "verify".into()];
    synthesis.inputs = vec![
        GraphInput {
            required_outputs: Vec::new(),
            extra: Default::default(),
            node: "lens".into(),
            outputs: vec![OutputKind::Findings],
        },
        GraphInput {
            required_outputs: Vec::new(),
            extra: Default::default(),
            node: "verify".into(),
            outputs: vec![OutputKind::Result, OutputKind::Findings],
        },
    ];

    let upheld = "F1 remains valid: UPHELD because the call still panics.\n```findings\n{\"schema_version\":1,\"findings\":[";
    let mut owner = FakeOwner {
        observations: [
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&entry(""))),
            terminal_with_result(AttemptOutcome::Succeeded, upheld),
            terminal_with_result(
                AttemptOutcome::Succeeded,
                &findings_text(&entry(
                    ",\"disposition\":\"accepted\",\"reason\":\"Verifier reproduced the panic\"",
                )),
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
    let verification = run.checkpoint.nodes[1].outputs.as_ref().unwrap();
    assert!(verification.findings.is_empty());
    assert!(
        verification
            .warnings
            .iter()
            .any(|warning| warning.contains("findings block"))
    );
    let synthesis = run.checkpoint.nodes[2].outputs.as_ref().unwrap();
    assert_eq!(synthesis.findings[0].id, "F1");
    assert_eq!(
        synthesis.findings[0].disposition.as_deref(),
        Some("accepted")
    );
    assert!(owner.prompts[2].contains("UPHELD because the call still panics"));
}

#[test]
fn a_synthesis_without_valid_dispositions_settles_as_failed_with_a_reason() {
    let cases = [
        (findings_text(&entry("")), "disposition_invalid"),
        (
            findings_text(&entry(",\"disposition\":\"maybe\",\"reason\":\"x\"")),
            "disposition_invalid",
        ),
        (
            findings_text(&entry(",\"disposition\":\"rejected\"")),
            "disposition_reason_missing",
        ),
        (
            findings_text(&entry(",\"disposition\":\"rejected\",\"reason\":\" \"")),
            "disposition_reason_missing",
        ),
        ("VERDICT: PASS\n".to_owned(), "disposition_findings_invalid"),
    ];
    for (text, expected) in cases {
        let progress = synthesis_run(&text);
        assert_eq!(progress.state, NodeState::Failed, "{text}");
        assert_eq!(progress.advance_error_code.as_deref(), Some(expected));
    }
    // A node that declares no dispositions keeps accepting bare findings.
    let plain = node("plain", route("first"));
    let mut owner = FakeOwner {
        observations: [terminal_with_result(
            AttemptOutcome::Succeeded,
            &findings_text(&entry("")),
        )]
        .into(),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![plain]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert!(run.checkpoint.successful());
}

fn settle_single(node: GraphNode, result: &str) -> NodeProgress {
    let mut owner = FakeOwner {
        observations: [terminal_with_result(AttemptOutcome::Succeeded, result)].into(),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![node]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    run.checkpoint.nodes[0].clone()
}

#[test]
fn writer_without_findings_contract_settles_empty_result_without_findings_warning() {
    let mut writer = node("writer", route("first"));
    writer.access = AccessMode::WorktreeWrite;
    writer.worktree = Some(".worktrees/writer".into());
    let settled = settle_single(writer, "");
    assert_eq!(settled.state, NodeState::Succeeded);
    assert!(
        !settled
            .outputs
            .unwrap()
            .warnings
            .iter()
            .any(|warning| warning.contains("findings block"))
    );
}

#[test]
fn findings_required_and_review_nodes_still_fail_on_a_missing_block() {
    let mut required = node("required", route("first"));
    required.require_findings = true;
    let settled = settle_single(required, "");
    assert_eq!(settled.state, NodeState::Failed);
    assert_eq!(settled.advance_error_code.as_deref(), Some("findings_required"));

    let mut lens = node("lens", route("first"));
    lens.role = Some("lens.security".into());
    let settled = settle_single(lens, "prose only");
    assert!(
        settled
            .outputs
            .unwrap()
            .warnings
            .iter()
            .any(|warning| warning.contains("findings block"))
    );
}
