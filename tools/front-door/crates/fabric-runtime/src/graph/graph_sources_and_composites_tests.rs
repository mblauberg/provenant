use super::*;

#[test]
fn synthesis_stops_when_the_pinned_integration_head_moves() {
    let pin = "1".repeat(40);
    for (second, expected) in [(pin.clone(), true), ("2".repeat(40), false)] {
        let mut synthesis = node("synthesis", route("first"));
        synthesis.kind = Some(NodeKind::Synthesise);
        synthesis.source = Some(SourceBinding::Integration {
            extra: Default::default(),
        });
        synthesis.depends_on = vec!["lens".into()];
        synthesis.inputs = vec![GraphInput {
            required_outputs: Vec::new(),
            extra: Default::default(),
            node: "lens".into(),
            outputs: vec![OutputKind::Result],
        }];
        let mut document = graph(vec![node("lens", route("first")), synthesis]);
        document.integration_branch = Some("batch".into());
        let mut owner = FakeOwner {
            integration_heads: [pin.clone(), second].into(),
            observations: [
                terminal(AttemptOutcome::Succeeded),
                terminal(AttemptOutcome::Succeeded),
            ]
            .into(),
            ..Default::default()
        };
        let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
        assert_eq!(run.checkpoint.successful(), expected);
        assert_eq!(run.checkpoint.integration_pin, Some(pin.clone()));
        if !expected {
            assert_eq!(
                run.checkpoint.nodes[1].advance_error_code.as_deref(),
                Some("integration_head_moved")
            );
            assert_eq!(owner.calls.len(), 1);
        }
    }
}

#[test]
fn source_and_read_roots_survive_retry_and_replay() {
    let commit = "1".repeat(40);
    let mut leaf = node("review", route("first"));
    leaf.source = Some(SourceBinding::Integration {
        extra: Default::default(),
    });
    leaf.read_roots = vec!["/authorised/other-worktree".into()];
    leaf.attachments = vec!["/authorised/shot.png".into()];
    leaf.route = None;
    leaf.routes = Some(vec![route("first"), route("fallback")]);
    let mut document = graph(vec![leaf]);
    document.integration_branch = Some("batch".into());
    let mut owner = FakeOwner {
        integration_heads: [commit.clone(), "2".repeat(40)].into(),
        observations: [
            AttemptObservation::Pending {
                attempt_ref: Some("existing".into()),
            },
            preparation_refusal("existing", AttemptOutcome::AuthRequired),
            terminal(AttemptOutcome::Succeeded),
        ]
        .into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let pending = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(
        pending.checkpoint.nodes[0].source_commit,
        Some(commit.clone())
    );
    let done = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(done.status, GraphStatus::Complete);
    assert_eq!(owner.integration_heads.len(), 1);
    assert_eq!(owner.source_commits, vec![Some(commit.clone()); 3]);
    assert_eq!(
        owner.read_roots,
        vec![vec!["/authorised/other-worktree".to_owned()]; 3]
    );
    assert_eq!(
        owner.attachments,
        vec![vec!["/authorised/shot.png".to_owned()]; 3]
    );
    let replay = advance(&document, &mut FakeOwner::default(), &mut journal).unwrap();
    assert_eq!(replay.checkpoint.nodes[0].source_commit, Some(commit));
}

#[test]
fn reader_review_base_resolves_in_its_bound_source_writer_and_replays() {
    let original = "1".repeat(40);
    let predecessor = "2".repeat(40);
    let base = "3".repeat(40);
    let mut writer = node("writer", route("fixture"));
    writer.access = AccessMode::WorktreeWrite;
    writer.worktree = Some("lane".into());
    writer.source = Some(SourceBinding::Commit {
        commit: original,
        extra: Default::default(),
    });
    let mut reader = node("reader", route("fixture"));
    reader.depends_on.push("writer".into());
    reader.source = Some(SourceBinding::Node {
        node: "writer".into(),
        extra: Default::default(),
    });
    reader.review_base = Some("moving-ref".into());
    let mut document = graph(vec![reader, writer]);
    let mut written = terminal(AttemptOutcome::Succeeded);
    if let AttemptObservation::Terminal { commit, .. } = &mut written {
        *commit = Some(predecessor.clone());
    }
    let mut owner = FakeOwner {
        resolved_review_base: Some(base.clone()),
        observations: VecDeque::from([
            AttemptObservation::Pending {
                attempt_ref: Some("host/writer".into()),
            },
            written,
            AttemptObservation::Pending {
                attempt_ref: Some("host/reader".into()),
            },
            terminal(AttemptOutcome::Succeeded),
        ]),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let admitted = advance(&document, &mut owner, &mut journal).unwrap();
    let reader = admitted
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "reader")
        .unwrap();
    assert!(reader.attempts.is_empty());
    assert_eq!(reader.review_base.as_deref(), Some(base.as_str()));
    let mut broken = admitted.checkpoint.clone();
    broken
        .nodes
        .iter_mut()
        .find(|node| node.id == "reader")
        .unwrap()
        .review_base = None;
    assert!(matches!(
        advance(
            &document,
            &mut FakeOwner::default(),
            &mut MemoryJournal(Some(broken))
        ),
        Err(GraphError::BadCheckpoint { .. })
    ));
    owner.resolved_review_base = Some("4".repeat(40));
    let pending = advance(&document, &mut owner, &mut journal).unwrap();
    let reader = pending
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "reader")
        .unwrap();
    assert_eq!(reader.source_commit.as_deref(), Some(predecessor.as_str()));
    assert_eq!(reader.review_base.as_deref(), Some(base.as_str()));
    assert_eq!(
        reader.attempts[0]
            .input_manifest
            .as_ref()
            .unwrap()
            .source_producer
            .as_ref()
            .unwrap()
            .node_id,
        "writer"
    );
    assert_eq!(
        owner.review_base_worktrees,
        vec![Some("/fixture/lane".into())]
    );
    owner.resolved_review_base = Some("4".repeat(40));
    let replay = advance(&document, &mut owner, &mut journal).unwrap();
    let reader = replay
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "reader")
        .unwrap();
    assert_eq!(reader.source_commit.as_deref(), Some(predecessor.as_str()));
    assert_eq!(reader.review_base.as_deref(), Some(base.as_str()));
    assert_eq!(owner.review_base_resolutions, 1);
    let reader_locations: Vec<_> = owner
        .calls
        .iter()
        .zip(&owner.permissions)
        .filter(|((key, _), _)| key.node_id == "reader")
        .map(|(_, (_, path))| path.as_deref())
        .collect();
    assert_eq!(
        reader_locations,
        vec![Some("/fixture/lane"), Some("/fixture/lane")]
    );
}

#[test]
fn known_commit_source_is_visible_before_dependencies_settle() {
    let mut reader = node("reader", route("fixture"));
    reader.depends_on.push("gate".into());
    reader.source = Some(SourceBinding::Commit {
        commit: "1".repeat(40),
        extra: Default::default(),
    });
    let mut document = graph(vec![reader, node("gate", route("fixture"))]);
    let mut owner = FakeOwner {
        observations: VecDeque::from([AttemptObservation::Pending {
            attempt_ref: Some("host/gate".into()),
        }]),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    let reader = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "reader")
        .unwrap();
    assert!(reader.attempts.is_empty());
    assert_eq!(
        reader.source_commit.as_deref(),
        Some("1111111111111111111111111111111111111111")
    );
}

#[test]
fn explicit_writer_source_stays_distinct_from_its_bound_worktree_head() {
    let mut writer = node("writer", route("fixture"));
    writer.access = AccessMode::WorktreeWrite;
    writer.worktree = Some("lane".into());
    writer.source = Some(SourceBinding::Commit {
        commit: "1".repeat(40),
        extra: Default::default(),
    });
    writer.depends_on.push("gate".into());
    let mut document = graph(vec![writer, node("gate", route("fixture"))]);
    let mut owner = FakeOwner {
        implicit_head: Some("2".repeat(40)),
        observations: VecDeque::from([AttemptObservation::Pending {
            attempt_ref: Some("host/gate".into()),
        }]),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    let writer = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "writer")
        .unwrap();
    assert!(writer.attempts.is_empty());
    assert_eq!(
        writer.source_commit.as_deref(),
        Some("1111111111111111111111111111111111111111")
    );
    assert_eq!(writer.writer_binding.as_ref().unwrap().head, "2".repeat(40));
    let mut broken = run.checkpoint.clone();
    broken
        .nodes
        .iter_mut()
        .find(|node| node.id == "writer")
        .unwrap()
        .writer_binding = None;
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
fn writer_binding_requires_host_support() {
    let mut writer = node("writer", route("fixture"));
    writer.access = AccessMode::WorktreeWrite;
    writer.worktree = Some("lane".into());
    let mut document = graph(vec![writer]);
    let mut journal = MemoryJournal::default();
    let mut owner = ErrorLauncher::default();
    assert!(matches!(
        advance(&document, &mut owner, &mut journal),
        Err(GraphError::Refused(_))
    ));
    assert!(owner.calls.is_empty());
    assert!(
        journal.0.as_ref().is_none_or(|checkpoint| {
            checkpoint.nodes.iter().all(|node| node.attempts.is_empty())
        })
    );
}

#[test]
fn implicit_writer_binds_before_its_dependency_settles() {
    let mut writer = node("writer", route("fixture"));
    writer.access = AccessMode::WorktreeWrite;
    writer.worktree = Some("lane".into());
    writer.depends_on.push("gate".into());
    let mut document = graph(vec![writer, node("gate", route("fixture"))]);
    let first_head = "1".repeat(40);
    let mut owner = FakeOwner {
        implicit_head: Some(first_head.clone()),
        observations: VecDeque::from([
            AttemptObservation::Pending {
                attempt_ref: Some("host/gate".into()),
            },
            terminal(AttemptOutcome::Succeeded),
            terminal(AttemptOutcome::Succeeded),
        ]),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let pending = advance(&document, &mut owner, &mut journal).unwrap();
    let progress = pending
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "writer")
        .unwrap();
    assert!(progress.attempts.is_empty());
    assert_eq!(progress.source_commit.as_deref(), Some(first_head.as_str()));
    let mut broken = pending.checkpoint.clone();
    broken
        .nodes
        .iter_mut()
        .find(|node| node.id == "writer")
        .unwrap()
        .writer_binding = None;
    assert!(matches!(
        advance(
            &document,
            &mut FakeOwner::default(),
            &mut MemoryJournal(Some(broken))
        ),
        Err(GraphError::BadCheckpoint { .. })
    ));
    owner.implicit_head = Some("2".repeat(40));
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(owner.implicit_bindings, 1);
    assert_eq!(
        run.checkpoint
            .nodes
            .iter()
            .find(|node| node.id == "writer")
            .unwrap()
            .source_commit
            .as_deref(),
        Some(first_head.as_str())
    );
    assert!(owner.source_commits.contains(&Some(first_head)));
}

#[test]
fn implicit_writer_source_is_bound_at_admission_and_survives_replay() {
    let mut writer = node("writer", route("fixture"));
    writer.access = AccessMode::WorktreeWrite;
    writer.worktree = Some("lane".into());
    let mut document = graph(vec![writer]);
    let first_head = "1".repeat(40);
    let mut owner = FakeOwner {
        implicit_head: Some(first_head.clone()),
        observations: VecDeque::from([
            AttemptObservation::Pending {
                attempt_ref: Some("host/attempt".into()),
            },
            terminal(AttemptOutcome::Succeeded),
        ]),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let pending = advance(&document, &mut owner, &mut journal).unwrap();
    let progress = &pending.checkpoint.nodes[0];
    assert_eq!(progress.source_commit.as_deref(), Some(first_head.as_str()));
    assert_eq!(
        progress.writer_binding.as_ref().unwrap().worktree,
        "/fixture/lane"
    );
    assert_eq!(owner.implicit_bindings, 1);
    owner.implicit_head = Some("2".repeat(40));
    let done = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(owner.implicit_bindings, 1);
    assert_eq!(
        owner.source_commits,
        vec![Some(first_head.clone()), Some(first_head.clone())]
    );
    assert_eq!(owner.permissions[0].1.as_deref(), Some("/fixture/lane"));
    assert_eq!(
        done.checkpoint.nodes[0].source_commit.as_deref(),
        Some(first_head.as_str())
    );
    let mut broken = done.checkpoint.clone();
    broken.nodes[0].writer_binding = None;
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
fn source_requires_upstream_commit_and_fails_before_launch_when_missing() {
    let mut child = node("review", route("review"));
    child.depends_on.push("author".into());
    child.source = Some(SourceBinding::Node {
        node: "author".into(),
        extra: Default::default(),
    });
    let document = graph(vec![node("author", route("writer")), child]);
    let mut owner = FakeOwner {
        observations: [terminal(AttemptOutcome::Succeeded)].into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(
        run.checkpoint.nodes[1].advance_error_code.as_deref(),
        Some("source_commit_unavailable")
    );
    assert!(run.checkpoint.nodes[1].attempts.is_empty());
    assert_eq!(run.checkpoint.nodes[0].state, NodeState::Succeeded);
    assert_eq!(run.checkpoint.nodes[1].state, NodeState::Failed);
    assert_eq!(
        advance(&document, &mut FakeOwner::default(), &mut journal)
            .unwrap()
            .checkpoint,
        run.checkpoint
    );
}

#[test]
fn composite_inherits_inputs_and_combines_results_from_parallel_children() {
    let document = super::model::document_fixture(serde_json::json!({
        "graph_id":"parallel-composite","nodes":[
            {"id":"start","access":"read_only","prompt":"start","route":{"alias":"senior"}},
            {"id":"parallel","kind":"composite","access":"read_only","require_skill_location":true,"depends_on":["start"],
            "inputs":[{"node":"start","outputs":["result"]}],
            "composite":{"nodes":[
                {"id":"a","access":"read_only","prompt":"a","route":{"alias":"junior"}},
                {"id":"b","access":"read_only","prompt":"b","route":{"alias":"junior"}}
            ],"exports":[{"node":"a","outputs":["result"]},{"node":"b","outputs":["result"]}]}}
        ]
    }));
    let mut owner = FakeOwner {
        observations: [
            terminal_with_result(AttemptOutcome::Succeeded, "initial evidence"),
            terminal_with_result(AttemptOutcome::Succeeded, "evidence A"),
            terminal_with_result(AttemptOutcome::Succeeded, "evidence B"),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert!(run.checkpoint.successful());
    assert!(owner.prompts[1].contains("initial evidence"));
    assert!(owner.prompts[2].contains("initial evidence"));
    assert_eq!(owner.skill_requirements, [false, true, true]);
    let result = run
        .checkpoint
        .nodes
        .last()
        .unwrap()
        .outputs
        .as_ref()
        .unwrap()
        .result
        .as_ref()
        .unwrap();
    let member_result = |id: &str| {
        run.checkpoint
            .nodes
            .iter()
            .find(|node| node.id == id)
            .and_then(|node| node.outputs.as_ref())
            .and_then(|outputs| outputs.result.as_ref())
            .map(|result| result.excerpt.as_str())
    };
    assert_eq!(member_result("parallel--a"), Some("evidence A"));
    assert_eq!(member_result("parallel--b"), Some("evidence B"));
    assert!(!result.truncated);
}

#[test]
fn composite_merge_retains_inherited_required_outputs() {
    let document = super::model::document_fixture(serde_json::json!({
        "graph_id":"required-composite-input","nodes":[
            {"id":"start","access":"read_only","prompt":"start","route":{"alias":"senior"}},
            {"id":"parallel","kind":"composite","access":"read_only","depends_on":["start"],
             "inputs":[{"node":"start","outputs":["findings"],"required_outputs":["findings"]}],
             "composite":{"nodes":[
                 {"id":"a","access":"read_only","prompt":"a","route":{"alias":"junior"},
                  "depends_on":["start"],"inputs":[{"node":"start","outputs":["result"]}]}
             ],"exports":[{"node":"a","outputs":["result"]}]}}
        ]
    }));
    let expanded = loops::expand(&document, &[], &crate::config::GraphLimits::default()).unwrap();
    let child = expanded
        .nodes
        .iter()
        .find(|node| node.id == "parallel--a")
        .unwrap();
    let input = child
        .inputs
        .iter()
        .find(|input| input.node == "start")
        .unwrap();
    assert!(input.outputs.contains(&OutputKind::Result));
    assert!(input.outputs.contains(&OutputKind::Findings));
    assert_eq!(input.required_outputs, vec![OutputKind::Findings]);
}

#[test]
fn required_composite_findings_need_complete_member_evidence() {
    let empty = "```findings\n{\"schema_version\":1,\"findings\":[]}\n```";
    for (second, succeeds) in [(empty, true), ("missing findings", false)] {
        let document = super::model::document_fixture(serde_json::json!({
            "graph_id":"required-composite-findings","nodes":[
                {"id":"reviews","kind":"composite","access":"read_only","composite":{
                    "nodes":[
                        {"id":"a","access":"read_only","prompt":"a","route":{"alias":"senior"}},
                        {"id":"b","access":"read_only","prompt":"b","route":{"alias":"senior"}}
                    ],"exports":[{"node":"a","outputs":["findings"]},{"node":"b","outputs":["findings"]}]
                }},
                {"id":"consumer","access":"read_only","prompt":"check","route":{"alias":"senior"},
                 "depends_on":["reviews"],"inputs":[{"node":"reviews","outputs":["findings"],"required_outputs":["findings"]}]}
            ]
        }));
        let mut owner = FakeOwner {
            observations: VecDeque::from([
                terminal_with_result(AttemptOutcome::Succeeded, empty),
                terminal_with_result(AttemptOutcome::Succeeded, second),
                terminal(AttemptOutcome::Succeeded),
            ]),
            ..Default::default()
        };
        let mut journal = MemoryJournal::default();
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        let consumer = run
            .checkpoint
            .nodes
            .iter()
            .find(|node| node.id == "consumer")
            .unwrap();
        if succeeds {
            assert_eq!(consumer.state, NodeState::Succeeded);
            assert_eq!(consumer.attempts.len(), 1);
        } else {
            assert_eq!(consumer.state, NodeState::Failed);
            assert_eq!(
                consumer.advance_error_code.as_deref(),
                Some("required_input_unavailable")
            );
            assert!(consumer.attempts.is_empty());
            assert_eq!(owner.calls.len(), 2);
        }
        let replay = advance(&document, &mut FakeOwner::default(), &mut journal).unwrap();
        assert_eq!(replay.checkpoint, run.checkpoint);
    }
}

#[test]
fn crash_mid_composite_resumes_without_rerunning_settled_children() {
    let document = super::model::document_fixture(serde_json::json!({
        "graph_id":"composite-crash","nodes":[{
            "id":"group","kind":"composite","access":"read_only",
            "composite":{"nodes":[
                {"id":"a","access":"read_only","prompt":"a","route":{"alias":"junior"}},
                {"id":"b","access":"read_only","prompt":"b","depends_on":["a"],"route":{"alias":"junior"}}
            ],"exports":[{"node":"a","outputs":["result"]},{"node":"b","outputs":["result"]}]}
        }]
    }));
    let mut journal = MemoryJournal::default();
    // A progress boundary inside a composite: the crash window is an
    // accepted but unobserved child, whose attempt stays starting with no
    // reference, and the settled child must never rerun.
    let mut crashing = FakeOwner {
        observations: [
            terminal_with_result(AttemptOutcome::Succeeded, "evidence A"),
            AttemptObservation::Pending { attempt_ref: None },
        ]
        .into(),
        ..Default::default()
    };
    let interrupted = advance(&document, &mut crashing, &mut journal).unwrap();
    assert_eq!(interrupted.status, GraphStatus::Pending);
    let progress = interrupted
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "group--b")
        .unwrap();
    let settled_attempts = interrupted
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "group--a")
        .unwrap()
        .attempts
        .clone();
    assert_eq!(progress.state, NodeState::Running);
    assert!(progress.launch_retry.is_none());
    assert_eq!(
        progress
            .attempts
            .last()
            .map(|last| (last.state, last.outcome.is_none())),
        Some((AttemptState::Running, true))
    );
    let mut resumed = FakeOwner {
        observations: [terminal_with_result(
            AttemptOutcome::Succeeded,
            "evidence B",
        )]
        .into(),
        ..Default::default()
    };
    let run = advance(&document, &mut resumed, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(run.checkpoint.successful());
    let child = |id: &str| {
        run.checkpoint
            .nodes
            .iter()
            .find(|node| node.id == id)
            .unwrap()
    };
    assert_eq!(child("group--a").state, NodeState::Succeeded);
    assert_eq!(child("group--a").attempts, settled_attempts);
    assert_eq!(child("group--b").state, NodeState::Succeeded);
    assert_eq!(
        child("group--a")
            .outputs
            .as_ref()
            .and_then(|outputs| outputs.result.as_ref())
            .map(|result| result.excerpt.as_str()),
        Some("evidence A")
    );
    assert_eq!(
        child("group--b")
            .outputs
            .as_ref()
            .and_then(|outputs| outputs.result.as_ref())
            .map(|result| result.excerpt.as_str()),
        Some("evidence B")
    );
}

#[test]
fn composite_running_status_does_not_consume_an_attempt_slot() {
    let document = super::model::document_fixture(serde_json::json!({
        "graph_id":"composite-running","nodes":[{
            "id":"group","kind":"composite","access":"read_only",
            "composite":{"nodes":[
                {"id":"a","access":"read_only","prompt":"a","route":{"alias":"junior"}},
                {"id":"b","access":"read_only","prompt":"b","route":{"alias":"junior"}}
            ],"exports":[]}
        }]
    }));
    let limits = crate::config::GraphLimits {
        concurrency: Some(1),
        ..Default::default()
    };
    let mut owner = FakeOwner {
        admission_policy: Some({
            use provenant_records::policy::{Policy, Source, Strength};
            let mut policy = Policy::default();
            policy.overlay(
                &[("GRAPH_CONCURRENCY_CAP".to_owned(), Strength::Refuse)].into(),
                Source::Project,
            );
            policy
        }),
        observations: [AttemptObservation::Pending { attempt_ref: None }].into(),
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
    let member = |id: &str| {
        run.checkpoint
            .nodes
            .iter()
            .find(|node| node.id == format!("group--{id}"))
            .unwrap()
    };
    assert_eq!(member("a").state, NodeState::Running);
    assert_eq!(member("b").state, NodeState::Blocked);
    assert_eq!(
        member("b").advance_error_code.as_deref(),
        Some("concurrency_cap")
    );
}

#[test]
fn typed_composite_constructor_matches_decoded_graph_node() {
    let typed = GraphNode::composite(
        "panel".into(),
        vec!["source".into()],
        CompositeSpec {
            extra: Default::default(),
            min_successful: Some(1),
            reduce_verdict: true,
            nodes: Vec::new(),
            exports: vec![GraphInput {
                extra: Default::default(),
                node: "member".into(),
                outputs: vec![OutputKind::Result, OutputKind::Findings],
                required_outputs: Vec::new(),
            }],
        },
    );
    let decoded: GraphNode = serde_json::from_value(serde_json::json!({
        "id": "panel", "kind": "composite", "access": "read_only", "depends_on": ["source"],
        "composite": {"min_successful": 1, "reduce_verdict": true, "nodes": [],
            "exports": [{"node": "member", "outputs": ["result", "findings"]}]}
    }))
    .unwrap();
    assert_eq!(typed, decoded);
    assert_eq!(
        serde_json::to_value(&typed).unwrap(),
        serde_json::to_value(decoded).unwrap()
    );
}

#[test]
fn composite_expansion_reports_the_bound_and_owner() {
    let document = super::model::document_fixture(serde_json::json!({
        "graph_id": "composite-bound", "nodes": [{
            "id": "group", "kind": "composite", "access": "read_only",
            "composite": {"nodes": [
                {"id": "a", "access": "read_only", "prompt": "a", "route": {"alias": "fixture"}},
                {"id": "b", "access": "read_only", "prompt": "b", "route": {"alias": "fixture"}}
            ], "exports": []}
        }]
    }));
    let limits = crate::config::GraphLimits {
        nodes: 1,
        ..Default::default()
    };
    let bound = crate::config::GraphLimits::composite_nodes_bound(limits.nodes);
    let member_count = document.nodes[0].composite.as_ref().unwrap().nodes.len();
    assert!(bound.refusal(member_count).is_some());
    assert!(matches!(
        composites::expand(&document, &limits),
        Err(AdmissionError::Composite(_))
    ));
}

#[test]
fn loop_expansion_safety_refusal_names_bound_and_owner_at_each_boundary() {
    let limits = crate::config::GraphLimits::default();
    let node_bound = crate::config::GraphLimits::loop_expansion_nodes_bound();
    let round_bound = crate::config::GraphLimits::loop_expansion_rounds_bound();
    let body_len = node_bound.maximum / round_bound.maximum + 1;
    let body: Vec<_> = (0..body_len)
        .map(|index| {
            serde_json::json!({
                "id": format!("member-{index}"), "access": "read_only",
                "prompt": "work", "route": {"alias": "fixture"}
            })
        })
        .collect();
    let document = super::model::document_fixture(serde_json::json!({
        "graph_id": "large-loop", "nodes": [{
            "id": "repair", "kind": "loop", "access": "read_only",
            "loop": {"body": body, "until": "member-0", "max_rounds": round_bound.maximum}
        }]
    }));
    assert!(node_bound.refusal(node_bound.maximum).is_none());
    assert!(node_bound.refusal(node_bound.maximum + 1).is_some());
    assert!(document.check_composites(&limits).is_err());
    assert!(matches!(
        document.admit(&limits),
        Err(AdmissionError::Composite(_))
    ));
    let mut owner = FakeOwner::default();
    let result = advance_with_budget(
        &document,
        &mut owner,
        &mut MemoryJournal::default(),
        &limits,
        &Default::default(),
    );
    assert!(matches!(
        result,
        Err(GraphError::Admission(AdmissionError::Composite(_)))
    ));

    let mut too_many_rounds = document.clone();
    let exceeded_rounds = round_bound.maximum + 1;
    too_many_rounds.nodes[0]
        .loop_body
        .as_mut()
        .unwrap()
        .max_rounds = exceeded_rounds;
    assert!(round_bound.refusal(exceeded_rounds).is_some());
    assert!(too_many_rounds.check_composites(&limits).is_err());
    assert!(matches!(
        too_many_rounds.admit(&limits),
        Err(AdmissionError::Composite(_))
    ));
    let result = advance_with_budget(
        &too_many_rounds,
        &mut owner,
        &mut MemoryJournal::default(),
        &limits,
        &Default::default(),
    );
    assert!(matches!(
        result,
        Err(GraphError::Admission(AdmissionError::Composite(_)))
    ));

    let mut zero_rounds = document.clone();
    zero_rounds.nodes[0].loop_body.as_mut().unwrap().max_rounds = 0;
    assert!(zero_rounds.check_composites(&limits).is_err());
    assert!(matches!(
        zero_rounds.admit(&limits),
        Err(AdmissionError::Composite(_))
    ));
    let result = advance_with_budget(
        &zero_rounds,
        &mut owner,
        &mut MemoryJournal::default(),
        &limits,
        &Default::default(),
    );
    assert!(matches!(
        result,
        Err(GraphError::Admission(AdmissionError::Composite(_)))
    ));
}

#[test]
fn composite_depth_uses_the_same_bound_in_preflight_and_runtime() {
    let document = super::model::document_fixture(serde_json::json!({
        "graph_id": "composite-depth", "nodes": [{
            "id": "outer", "kind": "composite", "access": "read_only",
            "composite": {"nodes": [{
                "id": "inner", "kind": "composite", "access": "read_only",
                "composite": {"nodes": [{
                    "id": "member", "access": "read_only", "prompt": "work",
                    "route": {"alias": "fixture"}
                }], "exports": []}
            }], "exports": []}
        }]
    }));
    let limits = crate::config::GraphLimits {
        expansion_depth: 1,
        ..Default::default()
    };
    let boundary = crate::config::GraphLimits {
        expansion_depth: 2,
        ..limits.clone()
    };
    assert!(document.check_composites(&boundary).is_ok());
    assert!(document.admit(&boundary).is_ok());
    let depth_bound = limits.expansion_depth_bound();
    assert!(depth_bound.refusal(depth_bound.maximum).is_none());
    assert!(depth_bound.refusal(depth_bound.maximum + 1).is_some());
    assert!(document.check_composites(&limits).is_err());
    assert!(matches!(
        document.admit(&limits),
        Err(AdmissionError::Composite(_))
    ));
    let mut owner = FakeOwner::default();
    let run = advance_with_budget(
        &document,
        &mut owner,
        &mut MemoryJournal::default(),
        &limits,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(
        run.checkpoint.pause_cause(),
        Some(PauseCause::InvalidPlanExpansion {
            extra: Default::default()
        })
    );
    assert!(
        run.checkpoint
            .nodes
            .iter()
            .all(|node| node.attempts.is_empty())
    );
}

#[test]
fn composite_expansion_identifies_duplicate_and_invalid_child_ids() {
    let mut document = super::model::document_fixture(serde_json::json!({
        "graph_id":"composite-identifiers","nodes":[{
            "id":"group","kind":"composite","access":"read_only",
            "composite":{"nodes":[
                {"id":"same","access":"read_only","prompt":"a","route":{"alias":"junior"}},
                {"id":"same","access":"read_only","prompt":"b","route":{"alias":"junior"}}
            ],"exports":[]}
        }]
    }));
    let limits = crate::config::GraphLimits::default();
    assert!(matches!(
        composites::expand(&document, &limits),
        Err(AdmissionError::Composite(_))
    ));
    let children = &mut document.nodes[0].composite.as_mut().unwrap().nodes;
    children[1].id = "bad name".into();
    assert!(matches!(
        composites::expand(&document, &limits),
        Err(AdmissionError::Composite(_))
    ));
}

#[test]
fn composite_rejects_ambiguous_exports_and_blocks_on_child_failure() {
    let mut document = super::model::document_fixture(serde_json::json!({
        "graph_id":"failed-composite","nodes":[{
            "id":"group","kind":"composite","access":"read_only",
            "composite":{"nodes":[
                {"id":"a","access":"read_only","prompt":"a","route":{"alias":"junior"}},
                {"id":"b","access":"read_only","prompt":"b","route":{"alias":"junior"}}
            ],"exports":[{"node":"a","outputs":["verdict"]},{"node":"b","outputs":["verdict"]}]}
        }]
    }));
    assert!(matches!(
        document.admit(&crate::config::GraphLimits::default()),
        Err(AdmissionError::Plan(_))
    ));
    document.nodes[0].composite.as_mut().unwrap().exports.pop();
    let mut owner = FakeOwner {
        observations: [
            terminal(AttemptOutcome::TaskFailed),
            terminal(AttemptOutcome::Succeeded),
        ]
        .into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(
        run.checkpoint.nodes.last().unwrap().state,
        NodeState::Blocked
    );
    assert_eq!(run.checkpoint.nodes.last().unwrap().advance_error, None);
    assert!(!run.checkpoint.successful());
    assert_eq!(
        advance(&document, &mut FakeOwner::default(), &mut journal)
            .unwrap()
            .checkpoint,
        run.checkpoint
    );
}
