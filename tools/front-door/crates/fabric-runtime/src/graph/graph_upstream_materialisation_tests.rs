use super::*;

struct FallbackManifestFixture {
    source: String,
    base: String,
    document: GraphDocument,
    run: GraphRun,
    journal: MemoryJournal,
}

impl FallbackManifestFixture {
    fn child(&self) -> &NodeProgress {
        self.run
            .checkpoint
            .nodes
            .iter()
            .find(|node| node.id == "child")
            .unwrap()
    }
}

fn fallback_manifest_fixture() -> FallbackManifestFixture {
    let source = "1".repeat(40);
    let base = "2".repeat(40);
    let mut child = node("child", route("first"));
    child.route = None;
    child.routes = Some(vec![route("first"), route("second")]);
    child.depends_on.push("parent".into());
    child.source = Some(SourceBinding::Node {
        node: "parent".into(),
        extra: Default::default(),
    });
    child.review_base = Some(base.clone());
    child.inputs.push(GraphInput {
        extra: Default::default(),
        node: "parent".into(),
        outputs: vec![OutputKind::Result, OutputKind::Commit],
        required_outputs: vec![OutputKind::Commit],
    });
    let mut producer = node("parent", route("fixture"));
    producer.source = Some(SourceBinding::Commit {
        commit: source.clone(),
        extra: Default::default(),
    });
    let document = graph(vec![child, producer]);
    let mut parent = terminal_from("private input content", "host/parent");
    if let AttemptObservation::Terminal { commit, .. } = &mut parent {
        *commit = Some(source.clone());
    }
    let mut owner = FakeOwner {
        observations: VecDeque::from([
            parent,
            preparation_refusal("host/first", AttemptOutcome::AuthRequired),
            terminal_from("done", "host/second"),
        ]),
        resolved_review_base: Some(base.clone()),
        graph_bases: (
            Some("3".repeat(40).parse().unwrap()),
            Some("4".repeat(40).parse().unwrap()),
        ),
        models: [("host/second".into(), "fixture/model".into())].into(),
        materialise: Some(|requests| MaterialisedInputs {
            directory: Some("/private/inputs".into()),
            index: None,
            entries: requests
                .iter()
                .map(|request| MaterialisedInput::File {
                    label: request.label.clone(),
                    path: "/private/inputs/result".into(),
                    bytes: 3,
                    omitted: 5,
                })
                .collect(),
        }),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    FallbackManifestFixture {
        source,
        base,
        document,
        run,
        journal,
    }
}

#[test]
fn attempt_manifests_record_selected_evidence_across_route_fallback() {
    let fixture = fallback_manifest_fixture();
    let child = fixture.child();
    let first = child.attempts.first().unwrap();
    let last = child.attempts.last().unwrap();
    assert_eq!(first.route.alias.as_deref(), Some("first"));
    assert_eq!(first.outcome, Some(AttemptOutcome::AuthRequired));
    assert_eq!(last.route.alias.as_deref(), Some("second"));
    assert!(last.launched);
    for attempt in &child.attempts {
        let manifest = attempt.input_manifest.as_ref().unwrap();
        assert_eq!(manifest.schema_version, 1);
        assert_eq!(
            manifest.source_commit.as_deref(),
            Some(fixture.source.as_str())
        );
        assert_eq!(manifest.review_base.as_deref(), Some(fixture.base.as_str()));
        assert_eq!(
            manifest.graph_review_base.as_deref(),
            Some("3333333333333333333333333333333333333333")
        );
        assert_eq!(
            manifest.integration_base.as_deref(),
            Some("4444444444444444444444444444444444444444")
        );
        assert_eq!(manifest.integration_source, None);
        assert_eq!(manifest.inputs.len(), 2);
        let input = manifest
            .inputs
            .iter()
            .find(|input| input.kind == OutputKind::Result)
            .unwrap();
        assert_eq!(input.producer_attempt.as_ref().unwrap().node_id, "parent");
        assert_eq!(
            input.producer_attempt.as_ref().unwrap().graph_id,
            fixture.document.graph_id
        );
        assert!(
            input
                .producer_result_digest
                .as_deref()
                .unwrap()
                .starts_with("sha256:")
        );
        assert_eq!(input.file_delivery, InputDelivery::Capped);
        assert_eq!(
            manifest
                .inputs
                .iter()
                .find(|input| input.kind == OutputKind::Commit)
                .unwrap()
                .commit
                .as_deref(),
            Some(fixture.source.as_str())
        );
        assert!(
            manifest
                .inputs
                .iter()
                .find(|input| input.kind == OutputKind::Commit)
                .unwrap()
                .required
        );
    }
}

#[test]
fn attempt_manifest_records_the_applied_fallback_route() {
    let fixture = fallback_manifest_fixture();
    let attempt = fixture.child().attempts.last().unwrap();
    let route = attempt
        .input_manifest
        .as_ref()
        .unwrap()
        .applied_route
        .as_ref()
        .unwrap();
    assert_eq!(route.model.as_deref(), Some("fixture/model"));
}

#[test]
fn attempt_manifest_excludes_private_payloads_and_paths() {
    let fixture = fallback_manifest_fixture();
    for attempt in &fixture.child().attempts {
        let manifest = attempt.input_manifest.as_ref().unwrap();
        let encoded = serde_json::to_string(manifest).unwrap();
        assert!(!encoded.contains("private input content"));
        assert!(!encoded.contains("/private/inputs"));
    }
}

#[test]
fn attempt_manifest_replay_keeps_the_checkpoint() {
    let mut fixture = fallback_manifest_fixture();
    let prior = fixture.run.checkpoint.clone();
    let replay = advance(
        &fixture.document,
        &mut FakeOwner::default(),
        &mut fixture.journal,
    )
    .unwrap();
    assert_eq!(replay.checkpoint, prior);
}

#[test]
fn attempt_manifest_preserves_unknown_fields() {
    let fixture = fallback_manifest_fixture();
    let mut value =
        serde_json::to_value(fixture.child().attempts[1].input_manifest.as_ref().unwrap()).unwrap();
    value["future_field"] = serde_json::json!({"kept":true});
    let decoded: AttemptInputManifest = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), value);
}

#[test]
fn checkpoint_without_an_attempt_manifest_is_rejected() {
    let fixture = fallback_manifest_fixture();
    let mut broken = fixture.run.checkpoint.clone();
    broken
        .nodes
        .iter_mut()
        .find(|node| node.id == "child")
        .unwrap()
        .attempts[0]
        .input_manifest = None;
    assert!(matches!(
        advance(
            &fixture.document,
            &mut FakeOwner::default(),
            &mut MemoryJournal(Some(broken))
        ),
        Err(GraphError::BadCheckpoint { .. })
    ));
}

#[test]
fn attempt_manifest_bounds_the_number_of_selected_values() {
    let count = outputs::MANIFEST_INPUT_LIMIT + 1;
    let mut consumer = node("consumer", route("fixture"));
    let mut nodes = Vec::new();
    for index in 0..count {
        let id = format!("producer-{index}");
        consumer.depends_on.push(id.clone());
        consumer.inputs.push(GraphInput {
            extra: Default::default(),
            node: id.clone(),
            outputs: vec![OutputKind::Result],
            required_outputs: Vec::new(),
        });
        nodes.push(node(&id, route("fixture")));
    }
    nodes.push(consumer);
    let document = graph(nodes);
    let mut owner = FakeOwner {
        observations: (0..=count)
            .map(|_| terminal_with_result(AttemptOutcome::Succeeded, "private body"))
            .collect(),
        ..Default::default()
    };
    let run = advance_with_budget(
        &document,
        &mut owner,
        &mut MemoryJournal::default(),
        &plans::generous(),
        &Default::default(),
    )
    .unwrap();
    let manifest = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "consumer")
        .unwrap()
        .attempts[0]
        .input_manifest
        .as_ref()
        .unwrap();
    assert_eq!(manifest.inputs.len(), outputs::MANIFEST_INPUT_LIMIT);
    assert_eq!(manifest.omitted, 1);
    assert!(
        !serde_json::to_string(manifest)
            .unwrap()
            .contains("private body")
    );
}

#[test]
fn materialised_upstream_delivery_is_recorded_by_manifest_status() {
    fn file(requests: &[InputRequest]) -> MaterialisedInputs {
        MaterialisedInputs {
            index: None,
            directory: Some("/inputs".into()),
            entries: requests
                .iter()
                .map(|request| MaterialisedInput::File {
                    label: request.label.clone(),
                    path: format!("/inputs/{}.md", request.label).into(),
                    bytes: 9,
                    omitted: 0,
                })
                .collect(),
        }
    }
    fn capped_file(requests: &[InputRequest]) -> MaterialisedInputs {
        MaterialisedInputs {
            index: None,
            directory: Some("/inputs".into()),
            entries: requests
                .iter()
                .map(|request| MaterialisedInput::File {
                    label: request.label.clone(),
                    path: format!("/inputs/{}.md", request.label).into(),
                    bytes: outputs::NODE_INPUT_FILE_BYTES as u64,
                    omitted: 1,
                })
                .collect(),
        }
    }
    fn omitted(requests: &[InputRequest]) -> MaterialisedInputs {
        MaterialisedInputs {
            index: None,
            directory: Some("/inputs".into()),
            entries: requests
                .iter()
                .map(|request| MaterialisedInput::Capped {
                    label: request.label.clone(),
                })
                .collect(),
        }
    }
    fn unavailable(requests: &[InputRequest]) -> MaterialisedInputs {
        MaterialisedInputs {
            index: None,
            directory: None,
            entries: requests
                .iter()
                .map(|request| MaterialisedInput::Unavailable {
                    label: request.label.clone(),
                    reason: "peer unavailable".into(),
                })
                .collect(),
        }
    }

    let cases: [(fn(&[InputRequest]) -> MaterialisedInputs, InputDelivery); 4] = [
        (file, InputDelivery::Delivered),
        (capped_file, InputDelivery::Capped),
        (omitted, InputDelivery::Omitted),
        (unavailable, InputDelivery::Unavailable),
    ];
    for (materialise, expected_file_delivery) in cases {
        let (document, mut owner) = parent_and_child(Some(materialise));
        let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
        let child = run
            .checkpoint
            .nodes
            .iter()
            .find(|node| node.id == "child")
            .unwrap();
        let evidence = &child.attempts[0].input_manifest.as_ref().unwrap().inputs[0];
        assert_eq!(evidence.file_delivery, expected_file_delivery);
        assert_eq!(evidence.delivery, InputDelivery::Delivered);
        assert_eq!(child.state, NodeState::Succeeded);
    }

    let entries: Vec<_> = (0..=UPSTREAM_INPUT_LIST_LIMIT)
        .map(|index| MaterialisedInput::File {
            label: format!("member-{index}"),
            path: format!("/inputs/{index}").into(),
            bytes: 1,
            omitted: 0,
        })
        .collect();
    let index = std::path::Path::new("/inputs/upstream-index.json");
    let listing = outputs::render_files(&entries, &[], Some(index));
    let last_listed = format!("member-{}", UPSTREAM_INPUT_LIST_LIMIT - 1);
    let first_omitted = format!("member-{UPSTREAM_INPUT_LIST_LIMIT}");
    assert!(listing.contains(&last_listed));
    assert!(!listing.contains(&first_omitted));
    assert!(listing.contains(index.to_str().unwrap()));
}

#[test]
fn file_notices_must_fit_the_prompt_before_a_consumer_attempt() {
    let (document, mut owner) = parent_and_child(Some(|requests| MaterialisedInputs {
        directory: Some("/inputs".into()),
        index: None,
        entries: requests
            .iter()
            .map(|request| MaterialisedInput::File {
                label: request.label.clone(),
                path: format!("/inputs/{}", "x".repeat(1000)).into(),
                bytes: 20,
                omitted: 0,
            })
            .collect(),
    }));
    let limits = crate::config::GraphLimits {
        node_prompt_bytes: 500,
        ..Default::default()
    };
    let run = advance_with_budget(
        &document,
        &mut owner,
        &mut MemoryJournal::default(),
        &limits,
        &Default::default(),
    )
    .unwrap();
    let consumer = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "child")
        .unwrap();
    assert_eq!(consumer.state, NodeState::Failed);
    assert_eq!(
        consumer.advance_error_code.as_deref(),
        Some("upstream_outputs_too_large")
    );
    assert!(consumer.attempts.is_empty());
}

#[test]
fn manifest_reports_a_file_omitted_at_the_delivery_cap() {
    let (document, mut owner) = parent_and_child(Some(|requests| MaterialisedInputs {
        directory: None,
        index: None,
        entries: requests
            .iter()
            .map(|request| MaterialisedInput::Capped {
                label: request.label.clone(),
            })
            .collect(),
    }));
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    let child = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "child")
        .unwrap();
    let input = &child.attempts[0].input_manifest.as_ref().unwrap().inputs[0];
    assert_eq!(input.file_delivery, InputDelivery::Omitted);
    assert_eq!(input.delivery, InputDelivery::Delivered);
    assert!(owner.prompts[1].contains("CAPPED"));
}

#[test]
fn truncation_notice_names_readable_staged_results() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reviews--a.md");
    std::fs::write(&path, "full findings from a").unwrap();
    let entries = vec![MaterialisedInput::File {
        label: "reviews--a".into(),
        path,
        bytes: 20,
        omitted: 0,
    }];
    let requests = vec![InputRequest {
        label: "reviews--a".into(),
        attempt_ref: None,
        attempt_key: None,
        inline_findings: None,
        finding_prefix: Some("reviews--a:".into()),
    }];
    let notice = render_files(&entries, &requests, None);
    let MaterialisedInput::File { label, path, .. } = &entries[0] else {
        unreachable!("fixture declares a staged file")
    };
    assert_eq!(label, "reviews--a");
    assert!(notice.contains(path.to_str().unwrap()));
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "full findings from a"
    );
}

#[test]
fn a_host_without_materialisation_leaves_the_prompt_as_it_was() {
    let (document, mut owner) = parent_and_child(None);
    advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(owner.file_requests.len(), 1, "the owner was still asked");
    assert!(!owner.prompts[1].contains("UPSTREAM_FILES"));
    assert!(owner.prompts[1].contains("short excerpt"));
}

#[test]
fn partial_composite_materialises_only_its_successful_members() {
    let (document, mut owner) = partial_review(1);
    owner.materialise = Some(|requests| MaterialisedInputs {
        index: None,
        directory: Some("/state/graph-inputs/verify-0".into()),
        entries: requests
            .iter()
            .map(|request| MaterialisedInput::File {
                label: request.label.clone(),
                path: format!("/inputs/{}.md", request.label).into(),
                bytes: 1,
                omitted: 0,
            })
            .collect(),
    });
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    let labels: Vec<_> = owner
        .file_requests
        .iter()
        .flatten()
        .map(|request| request.label.as_str())
        .collect();
    assert_eq!(labels, vec!["lenses--lens-a"], "{labels:?}");
    let verify = owner.prompts.last().unwrap();
    assert!(verify.contains("/inputs/lenses--lens-a.md"));
    assert!(!verify.contains("lens-b.md") && !verify.contains("lens-c.md"));
}
