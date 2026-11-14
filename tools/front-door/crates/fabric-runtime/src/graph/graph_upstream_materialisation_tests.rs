use super::*;

#[test]
fn attempt_manifests_retain_selected_evidence_across_fallback_and_replay() {
    let source = "1".repeat(40);
    let base = "2".repeat(40);
    let mut child = node("child", route("first"));
    child.route = None;
    child.routes = Some(vec![route("first"), route("second")]);
    child.depends_on.push("parent".into());
    child.source = Some(SourceBinding::Node {
        node: "parent".into(),
    });
    child
        .extra
        .insert("review_base".into(), serde_json::json!(base));
    child.inputs.push(GraphInput {
        extra: Default::default(),
        node: "parent".into(),
        outputs: vec![OutputKind::Result, OutputKind::Commit],
        required_outputs: vec![OutputKind::Commit],
    });
    let mut producer = node("parent", route("fixture"));
    producer.source = Some(SourceBinding::Commit {
        commit: source.clone(),
    });
    let mut document = graph(vec![child, producer]);
    document.schema_version = 2;
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
    let child = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "child")
        .unwrap();
    assert_eq!(child.attempts.len(), 2);
    for attempt in &child.attempts {
        let manifest = attempt.input_manifest.as_ref().unwrap();
        assert_eq!(manifest.schema_version, 1);
        assert_eq!(manifest.source_commit.as_deref(), Some(source.as_str()));
        assert_eq!(manifest.review_base.as_deref(), Some(base.as_str()));
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
        let input = &manifest.inputs[0];
        assert_eq!(input.producer_attempt.as_ref().unwrap().node_id, "parent");
        assert_eq!(
            input.producer_attempt.as_ref().unwrap().graph_id,
            document.graph_id
        );
        assert!(
            input
                .producer_result_digest
                .as_deref()
                .unwrap()
                .starts_with("sha256:")
        );
        assert_eq!(input.file_delivery, InputDelivery::Capped);
        assert_eq!(manifest.inputs[1].commit.as_deref(), Some(source.as_str()));
        assert!(manifest.inputs[1].required);
        let encoded = serde_json::to_string(manifest).unwrap();
        assert!(!encoded.contains("private input content"));
        assert!(!encoded.contains("/private/inputs"));
    }
    assert_eq!(
        child.attempts[1]
            .input_manifest
            .as_ref()
            .unwrap()
            .applied_route
            .as_ref()
            .and_then(|route| route.model.as_deref()),
        Some("fixture/model")
    );
    let replay = advance(&document, &mut FakeOwner::default(), &mut journal).unwrap();
    assert_eq!(replay.checkpoint, run.checkpoint);
    let mut value =
        serde_json::to_value(child.attempts[1].input_manifest.as_ref().unwrap()).unwrap();
    value["future_field"] = serde_json::json!({"kept":true});
    let decoded: AttemptInputManifest = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), value);
    let mut broken = run.checkpoint.clone();
    broken
        .nodes
        .iter_mut()
        .find(|node| node.id == "child")
        .unwrap()
        .attempts[0]
        .input_manifest = None;
    assert!(matches!(
        advance(
            &document,
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
fn upstream_files_are_listed_next_to_the_excerpts_and_the_launch_is_unchanged() {
    let (document, mut owner) = parent_and_child(Some(|requests| MaterialisedInputs {
        index: None,
        directory: Some("/state/graph-inputs/child-0".into()),
        entries: requests
            .iter()
            .map(|request| MaterialisedInput::File {
                label: request.label.clone(),
                path: format!("/state/graph-inputs/child-0/{}.md", request.label).into(),
                bytes: 9000,
                omitted: 0,
            })
            .collect(),
    }));
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert!(run.checkpoint.successful());
    // The parent selects nothing, so only the child is asked to materialise.
    assert_eq!(
        owner.file_requests,
        vec![vec![InputRequest {
            label: "parent".into(),
            attempt_ref: Some("host-a/attempt-1".into()),
            attempt_key: Some(run.checkpoint.nodes[0].attempts[0].key.clone()),
            inline_findings: None,
            finding_prefix: None,
        }]]
    );
    assert_eq!(owner.file_caps, vec![outputs::NODE_INPUT_FILE_BYTES]);
    let prompt = &owner.prompts[1];
    let excerpt_at = prompt.find("short excerpt").expect("excerpt kept");
    let files_at = prompt.find("<UPSTREAM_FILES>").expect("files block");
    assert!(excerpt_at < files_at, "{prompt}");
    assert!(prompt.contains("- parent: /state/graph-inputs/child-0/parent.md (9000 bytes)"));
    assert!(!owner.prompts[0].contains("UPSTREAM_FILES"));
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
    assert_eq!(owner.calls.len(), 1);
}

#[test]
fn a_remote_upstream_is_named_unavailable_and_the_node_still_runs() {
    let (document, mut owner) = parent_and_child(Some(|requests| MaterialisedInputs {
        index: None,
        directory: None,
        entries: requests
            .iter()
            .map(|request| MaterialisedInput::Unavailable {
                label: request.label.clone(),
                reason: "its result could not be read: peer unavailable".into(),
            })
            .collect(),
    }));
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert!(run.checkpoint.successful());
    assert_eq!(run.checkpoint.nodes[0].state, NodeState::Succeeded);
    assert!(
        owner.prompts[1]
            .contains("- parent: unavailable: its result could not be read: peer unavailable; full result attempt: host-a/attempt-1")
    );
}

#[test]
fn capped_upstream_results_are_named_in_the_prompt() {
    let (document, mut owner) = parent_and_child(Some(|requests| MaterialisedInputs {
        index: Some("/state/graph-inputs/child-0/upstream-index.json".into()),
        directory: Some("/state/graph-inputs/child-0".into()),
        entries: vec![
            MaterialisedInput::File {
                label: requests[0].label.clone(),
                path: "/state/graph-inputs/child-0/parent.md".into(),
                bytes: 2_097_300,
                omitted: 1234,
            },
            MaterialisedInput::Capped {
                label: "later".into(),
            },
        ],
    }));
    advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    let prompt = &owner.prompts[1];
    assert!(
        prompt.contains("CAPPED: 1234 more bytes omitted"),
        "{prompt}"
    );
    assert!(
        prompt.contains("full result attempt: host-a/attempt-1"),
        "{prompt}"
    );
    assert!(prompt.contains("- later: CAPPED: not written"), "{prompt}");
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
fn long_upstream_listing_points_to_complete_index() {
    let (document, mut owner) = parent_and_child(Some(|_| MaterialisedInputs {
        directory: Some("/inputs".into()),
        index: Some("/inputs/upstream-index.json".into()),
        entries: (0..=UPSTREAM_INPUT_LIST_LIMIT)
            .map(|i| MaterialisedInput::File {
                label: format!("member-{i}"),
                path: format!("/inputs/{i}").into(),
                bytes: 1,
                omitted: 0,
            })
            .collect(),
    }));
    advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    let prompt = &owner.prompts[1];
    assert!(prompt.contains("Complete input index: /inputs/upstream-index.json"));
    assert!(prompt.contains("1 more upstream results are not listed"));
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
    let source = notice
        .lines()
        .find_map(|line| line.strip_prefix("composite findings truncated: full member result: "))
        .unwrap();
    let source: serde_json::Value = serde_json::from_str(source).unwrap();
    assert_eq!(source["label"], "reviews--a");
    assert_eq!(
        std::fs::read_to_string(source["path"].as_str().unwrap()).unwrap(),
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
