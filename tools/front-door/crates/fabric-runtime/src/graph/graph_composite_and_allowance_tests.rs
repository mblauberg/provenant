use super::*;

// Spec: docs/specs/fabric.md#composite-settlement
#[test]
fn quota_reroute_recomputes_composite_before_checkpoint_resume() {
    #[derive(Default)]
    struct RestartBoundaryJournal {
        saved: Option<GraphCheckpoint>,
        boundary: Option<GraphCheckpoint>,
    }

    impl GraphJournal for RestartBoundaryJournal {
        type Error = std::convert::Infallible;

        fn load(&mut self, graph_id: &str) -> Result<Option<GraphCheckpoint>, Self::Error> {
            Ok(self
                .saved
                .as_ref()
                .filter(|checkpoint| checkpoint.graph_id == graph_id)
                .cloned())
        }

        fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error> {
            let rerouted_member_is_pending = checkpoint
                .nodes
                .iter()
                .any(|node| node.id == "task--plan" && node.state == NodeState::Pending);
            let reroute_is_recorded = checkpoint.expansions.iter().any(|expansion| {
                expansion
                    .patch
                    .as_ref()
                    .is_some_and(|patch| patch.restart.is_some())
            });
            if self.boundary.is_none() && rerouted_member_is_pending && reroute_is_recorded {
                self.boundary = Some(checkpoint.clone());
            }
            self.saved = Some(checkpoint.clone());
            Ok(())
        }
    }

    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "graph_id": "composite-quota-reroute",
        "nodes": [{
            "id": "task",
            "kind": "composite",
            "access": "read_only",
            "composite": {
                "nodes": [{
                    "id": "plan",
                    "access": "read_only",
                    "prompt": "Plan the work",
                    "routes": [
                        {"adapter": "first", "model": "first/model"},
                        {"adapter": "second", "model": "second/model"}
                    ]
                }],
                "exports": [{"node": "plan", "outputs": ["result"]}]
            }
        }]
    }))
    .unwrap();
    let mut owner = FakeOwner {
        observations: [
            terminal(AttemptOutcome::UsageLimited),
            terminal_with_result(AttemptOutcome::Succeeded, "plan complete"),
        ]
        .into(),
        ..Default::default()
    };
    let mut journal = RestartBoundaryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert_eq!(owner.calls.len(), 2);
    assert_eq!(owner.calls[0].1.adapter.as_deref(), Some("first"));
    assert_eq!(owner.calls[1].1.adapter.as_deref(), Some("second"));

    let boundary = journal
        .boundary
        .expect("host reroute checkpoint boundary was saved");
    let reroute = boundary
        .expansions
        .iter()
        .filter_map(|expansion| expansion.patch.as_ref())
        .find(|patch| patch.restart.is_some())
        .expect("quota reroute patch was recorded");
    assert_eq!(reroute.replaced.len(), 1);
    assert_eq!(reroute.replaced[0].id, "task--plan");
    assert_eq!(
        reroute.restart.as_ref().unwrap().cause,
        provenant_records::StopCause::UsageLimited
    );
    let boundary: GraphCheckpoint =
        serde_json::from_slice(&serde_json::to_vec(&boundary).unwrap()).unwrap();
    assert_eq!(
        boundary.nodes.iter().find(|node| node.id == "task").unwrap().state,
        NodeState::Pending
    );
    assert!(boundary
        .nodes
        .iter()
        .find(|node| node.id == "task")
        .unwrap()
        .outputs
        .is_none());

    // Loading the exact patch boundary validates the composite/member pairing
    // before it can launch the successor route, then continuation is idempotent.
    let mut resumed_journal = RestartBoundaryJournal {
        saved: Some(boundary),
        boundary: None,
    };
    let mut resumed_owner = FakeOwner {
        observations: [terminal_with_result(
            AttemptOutcome::Succeeded,
            "plan complete",
        )]
        .into(),
        ..Default::default()
    };
    let resumed = advance(&document, &mut resumed_owner, &mut resumed_journal).unwrap();
    assert_eq!(resumed.status, GraphStatus::Complete);
    assert_eq!(resumed_owner.calls.len(), 1);
    assert_eq!(
        resumed_owner.calls[0].1.adapter.as_deref(),
        Some("second")
    );
    advance(&document, &mut resumed_owner, &mut resumed_journal).unwrap();
    assert_eq!(resumed_owner.calls.len(), 1);
}

// Spec: docs/specs/fabric.md#composite-settlement
#[test]
fn public_patch_preserves_inactive_composite_and_replays_derived_state() {
    // Raw GraphDocument input bypasses template lowering, so mirror the
    // parent's inherited false condition on its member.
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 2,
        "graph_id": "inactive-composite-patch",
        "nodes": [
            {"id":"hold","access":"read_only","prompt":"Hold","route":{"alias":"fixture"}},
            {"id":"inactive","kind":"composite","access":"read_only","depends_on":["hold"],
             "when":[{"name":"enabled","expected":"true","actual":"false"}],
             "composite":{"nodes":[
                 {"id":"member","access":"read_only","prompt":"Inactive member","route":{"alias":"fixture"},
                  "when":[{"name":"enabled","expected":"true","actual":"false"}]}
             ],"exports":[{"node":"member","outputs":["result"]}]}},
            {"id":"active","kind":"composite","access":"read_only","composite":{"nodes":[
                {"id":"member","access":"read_only","prompt":"Active member","route":{"alias":"fixture"}}
            ],"exports":[{"node":"member","outputs":["result"]}]}}
        ]
    }))
    .unwrap();
    let mut owner = FakeOwner {
        observations: [
            AttemptObservation::Pending {
                attempt_ref: Some("hold-running".into()),
            },
            terminal_with_result(AttemptOutcome::Succeeded, "active before retry"),
        ]
        .into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let started = advance(&document, &mut owner, &mut journal).unwrap();
    let mut checkpoint = started.checkpoint;
    let state = |checkpoint: &GraphCheckpoint, id: &str| {
        checkpoint
            .nodes
            .iter()
            .find(|progress| progress.id == id)
            .map(|progress| progress.state)
    };
    assert_eq!(state(&checkpoint, "inactive"), Some(NodeState::Skipped));
    assert_eq!(
        state(&checkpoint, "inactive--member"),
        Some(NodeState::Skipped)
    );
    assert_eq!(state(&checkpoint, "active"), Some(NodeState::Succeeded));

    let library = template::TemplateLibrary::bundled();
    let resolvers = Default::default();
    let limits = plans::generous();
    let context = patch::PatchContext {
        library: &library,
        resolvers: &resolvers,
        scan: &|_| Ok(()),
        journal: None,
        limits: &limits,
        worktree_layout: provenant_config::PortTable::builtin().worktree_layout,
    };
    patch::retry_node(
        &mut patch::LiveGraph {
            document: &document,
            checkpoint: &mut checkpoint,
        },
        "active--member",
        None,
        &context,
    )
    .unwrap();
    // A retry in the active composite must leave the unrelated conditionally
    // inactive boundary and member untouched while recomputing the active boundary.
    assert_eq!(state(&checkpoint, "inactive"), Some(NodeState::Skipped));
    assert_eq!(
        state(&checkpoint, "inactive--member"),
        Some(NodeState::Skipped)
    );
    assert_eq!(
        state(&checkpoint, "active--member"),
        Some(NodeState::Pending)
    );
    assert_eq!(state(&checkpoint, "active"), Some(NodeState::Pending));
    assert!(
        checkpoint
            .nodes
            .iter()
            .find(|progress| progress.id == "active")
            .unwrap()
            .outputs
            .is_none()
    );

    let restored: GraphCheckpoint =
        serde_json::from_slice(&serde_json::to_vec(&checkpoint).unwrap()).unwrap();
    let mut resumed_owner = FakeOwner {
        observations: [
            terminal_with_result(AttemptOutcome::Succeeded, "hold complete"),
            terminal_with_result(AttemptOutcome::Succeeded, "active member complete"),
        ]
        .into(),
        ..Default::default()
    };
    let mut resumed_journal = MemoryJournal(Some(restored));
    let resumed = advance(&document, &mut resumed_owner, &mut resumed_journal).unwrap();
    assert_eq!(resumed.status, GraphStatus::Complete);
    assert_eq!(
        state(&resumed.checkpoint, "inactive"),
        Some(NodeState::Skipped)
    );
    assert_eq!(
        state(&resumed.checkpoint, "inactive--member"),
        Some(NodeState::Skipped)
    );
    assert_eq!(
        state(&resumed.checkpoint, "active"),
        Some(NodeState::Succeeded)
    );
    // The inactive child has no success observation because its own condition
    // skips it, matching the form emitted by template lowering.
    assert_eq!(resumed_owner.calls.len(), 2);
    assert_eq!(
        resumed_owner
            .calls
            .iter()
            .filter(|call| call.0.node_id == "active--member")
            .count(),
        1
    );
    assert!(
        resumed_owner
            .calls
            .iter()
            .all(|call| call.0.node_id != "inactive--member")
    );

    let replayed_checkpoint: GraphCheckpoint =
        serde_json::from_slice(&serde_json::to_vec(&resumed.checkpoint).unwrap()).unwrap();
    let mut replay_owner = FakeOwner::default();
    let replayed = advance(
        &document,
        &mut replay_owner,
        &mut MemoryJournal(Some(replayed_checkpoint)),
    )
    .unwrap();
    assert_eq!(replayed.status, GraphStatus::Complete);
    assert!(replay_owner.calls.is_empty());
}

// Spec: docs/specs/fabric.md#composite-settlement
#[test]
fn public_patch_skip_survives_unrelated_edits_and_retry_supersedes_it() {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 2,
        "graph_id": "explicit-composite-skip",
        "nodes": [
            {"id":"hold","access":"read_only","prompt":"Hold","route":{"alias":"fixture"}},
            {"id":"skipped","kind":"composite","access":"read_only","depends_on":["hold"],
             "composite":{"nodes":[
                 {"id":"member","access":"read_only","prompt":"Skipped group member","route":{"alias":"fixture"}}
             ],"exports":[{"node":"member","outputs":["result"]}]}},
            {"id":"active","kind":"composite","access":"read_only","depends_on":["hold"],
             "composite":{"nodes":[
                 {"id":"member","access":"read_only","prompt":"Active group member","route":{"alias":"fixture"}}
             ],"exports":[{"node":"member","outputs":["result"]}]}},
            {"id":"loose","access":"read_only","prompt":"Original","depends_on":["hold"],"route":{"alias":"fixture"}}
        ]
    }))
    .unwrap();
    let mut owner = FakeOwner {
        observations: [AttemptObservation::Pending {
            attempt_ref: Some("hold-running".into()),
        }]
        .into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let started = advance(&document, &mut owner, &mut journal).unwrap();
    let mut checkpoint = started.checkpoint;
    let state = |checkpoint: &GraphCheckpoint, id: &str| {
        checkpoint
            .nodes
            .iter()
            .find(|progress| progress.id == id)
            .map(|progress| progress.state)
    };
    assert_eq!(state(&checkpoint, "skipped"), Some(NodeState::Pending));
    assert_eq!(
        state(&checkpoint, "skipped--member"),
        Some(NodeState::Pending)
    );

    let library = template::TemplateLibrary::bundled();
    let resolvers = Default::default();
    let limits = plans::generous();
    let context = patch::PatchContext {
        library: &library,
        resolvers: &resolvers,
        scan: &|_| Ok(()),
        journal: None,
        limits: &limits,
        worktree_layout: provenant_config::PortTable::builtin().worktree_layout,
    };
    let edit: patch::Patch = serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "expected_revision": 0,
        "skip": ["skipped", "active--member"],
        "replace": [{"id":"loose","access":"read_only","prompt":"Replacement","route":{"alias":"fixture"}}]
    }))
    .unwrap();
    patch::admit_patch(
        &mut patch::LiveGraph {
            document: &document,
            checkpoint: &mut checkpoint,
        },
        None,
        &edit,
        &context,
    )
    .unwrap();
    assert_eq!(state(&checkpoint, "skipped"), Some(NodeState::Skipped));
    assert_eq!(
        state(&checkpoint, "active--member"),
        Some(NodeState::Skipped)
    );
    assert_eq!(state(&checkpoint, "active"), Some(NodeState::Blocked));
    assert_eq!(state(&checkpoint, "loose"), Some(NodeState::Pending));

    let restored: GraphCheckpoint =
        serde_json::from_slice(&serde_json::to_vec(&checkpoint).unwrap()).unwrap();
    let mut resume_owner = FakeOwner {
        observations: [
            terminal_with_result(AttemptOutcome::Succeeded, "hold complete"),
            terminal_with_result(AttemptOutcome::Succeeded, "member complete"),
            terminal_with_result(AttemptOutcome::Succeeded, "replacement complete"),
        ]
        .into(),
        ..Default::default()
    };
    let mut resume_journal = MemoryJournal(Some(restored));
    let resumed = advance(&document, &mut resume_owner, &mut resume_journal).unwrap();
    assert_eq!(resumed.status, GraphStatus::Complete);
    assert_eq!(
        state(&resumed.checkpoint, "skipped"),
        Some(NodeState::Skipped)
    );
    assert_eq!(
        state(&resumed.checkpoint, "active"),
        Some(NodeState::Blocked)
    );
    assert_eq!(
        state(&resumed.checkpoint, "skipped--member"),
        Some(NodeState::Succeeded)
    );
    assert_eq!(
        resume_owner
            .calls
            .iter()
            .filter(|call| call.0.node_id == "loose")
            .count(),
        1
    );
    assert_eq!(
        resume_owner
            .calls
            .iter()
            .filter(|call| call.0.node_id == "skipped--member")
            .count(),
        1
    );

    let mut retry_checkpoint = resumed.checkpoint;
    patch::retry_node(
        &mut patch::LiveGraph {
            document: &document,
            checkpoint: &mut retry_checkpoint,
        },
        "skipped",
        None,
        &context,
    )
    .unwrap();
    assert_eq!(
        state(&retry_checkpoint, "skipped"),
        Some(NodeState::Pending)
    );

    let retry_restored: GraphCheckpoint =
        serde_json::from_slice(&serde_json::to_vec(&retry_checkpoint).unwrap()).unwrap();
    let mut retry_owner = FakeOwner {
        observations: [terminal_with_result(
            AttemptOutcome::Succeeded,
            "retried member",
        )]
        .into(),
        ..Default::default()
    };
    let mut retry_journal = MemoryJournal(Some(retry_restored));
    let retried = advance(&document, &mut retry_owner, &mut retry_journal).unwrap();
    assert_eq!(retried.status, GraphStatus::Complete);
    assert_eq!(
        state(&retried.checkpoint, "skipped"),
        Some(NodeState::Succeeded)
    );
    assert_eq!(retry_owner.calls.len(), 1);
    assert_eq!(retry_owner.calls[0].0.node_id, "skipped--member");

    let replayed_checkpoint: GraphCheckpoint =
        serde_json::from_slice(&serde_json::to_vec(&retried.checkpoint).unwrap()).unwrap();
    let mut replay_owner = FakeOwner::default();
    let replayed = advance(
        &document,
        &mut replay_owner,
        &mut MemoryJournal(Some(replayed_checkpoint)),
    )
    .unwrap();
    assert_eq!(replayed.status, GraphStatus::Complete);
    assert!(replay_owner.calls.is_empty());
}

#[test]
fn advisory_composite_settlement_preserves_cause_and_launches_dependant() {
    for outcome in [AttemptOutcome::ReviewRequired, AttemptOutcome::TaskFailed] {
        let document: GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version":1, "graph_id":"advisory", "nodes":[
                {"id":"scout", "kind":"composite", "access":"read_only", "advisory":true,
                 "composite":{"nodes":[{"id":"synthesis","access":"read_only","prompt":"research","route":{"alias":"fixture"}}],
                              "exports":[{"node":"synthesis","outputs":["result"]}]}},
                {"id":"lane", "access":"read_only", "prompt":"write", "route":{"alias":"fixture"},
                 "depends_on":["scout"], "inputs":[{"node":"scout","outputs":["result"]}]}
            ]
        })).unwrap();
        let mut owner = FakeOwner {
            observations: VecDeque::from([terminal(outcome), terminal(AttemptOutcome::Succeeded)]),
            ..Default::default()
        };
        let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
        let scout = run
            .checkpoint
            .nodes
            .iter()
            .find(|n| n.id == "scout--synthesis")
            .unwrap();
        assert_eq!(scout.attempts[0].outcome, Some(outcome));
        assert_eq!(
            run.checkpoint
                .nodes
                .iter()
                .find(|n| n.id == "lane")
                .unwrap()
                .state,
            NodeState::Succeeded
        );
        if outcome == AttemptOutcome::TaskFailed {
            assert!(owner.prompts.last().unwrap().contains("scout: unavailable"));
        } else {
            assert_eq!(scout.state, NodeState::Succeeded);
        }
    }
}

#[test]
fn cumulative_allowance_counts_graph_attempts_and_survives_observation_replay() {
    let mut lane = node("lane", route("first"));
    lane.route = None;
    lane.routes = Some(vec![route("first"), route("second"), route("third")]);
    let document = graph(vec![lane]);
    let mut owner = FakeOwner {
        retry_limit: Some(1),
        observations: [AttemptObservation::Pending {
            attempt_ref: Some("first".into()),
        }]
        .into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(owner.retry_budgets, vec![Some(1)]);
    owner.retry_limit = Some(9); // Host changes cannot renew an admitted node.
    let mut refused = terminal(AttemptOutcome::UsageLimited);
    if let AttemptObservation::Terminal { attempt_ref, .. } = &mut refused {
        *attempt_ref = Some("first".into());
    }
    owner.observations.push_back(refused);
    owner
        .observations
        .push_back(terminal(AttemptOutcome::UsageLimited));
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.checkpoint.nodes[0].state, NodeState::Failed);
    assert_eq!(run.checkpoint.nodes[0].recorded_attempts().count(), 2);
    assert_eq!(run.checkpoint.nodes[0].attempts.len(), 1);
    assert_eq!(run.checkpoint.nodes[0].previous_attempts.len(), 1);
    assert_eq!(
        run.checkpoint.nodes[0]
            .retry
            .as_ref()
            .map(|retry| retry.retries),
        Some(1)
    );
    assert_eq!(owner.calls.len(), 3); // One observation replay and one stop-policy restart.
    assert_eq!(owner.retry_budgets, vec![Some(1), Some(1), Some(0)]);
    assert_eq!(owner.calls[0].0, owner.calls[1].0);
    advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(owner.calls.len(), 3);
}

#[test]
fn preparation_fallback_does_not_consume_settlement_allowance() {
    use provenant_records::run::PreparationFailure;
    let mut lane = node("lane", route("first"));
    lane.route = None;
    lane.routes = Some(vec![route("first"), route("second"), route("third")]);
    let mut owner = FakeOwner {
        retry_limit: Some(0),
        fallback_evidence: [
            ("first".into(), PreparationFailure::UsageLimited),
            ("second".into(), PreparationFailure::UsageLimited),
        ]
        .into(),
        observations: [
            preparation_refusal("first", AttemptOutcome::UsageLimited),
            preparation_refusal("second", AttemptOutcome::UsageLimited),
            terminal(AttemptOutcome::Succeeded),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![lane]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert!(run.checkpoint.successful());
    assert_eq!(owner.retry_budgets, vec![Some(0), Some(0), Some(0)]);
}

#[test]
fn writer_quota_continuation_succeeds_and_lens_diff_base_stays_node_source() {
    let node_source = "0".repeat(40);
    let head_2 = "2".repeat(40);

    let mut write = node("writer", route("first"));
    write.access = AccessMode::WorktreeWrite;
    write.worktree = Some(".worktrees/write".into());
    write.source = Some(SourceBinding::Commit {
        commit: node_source.clone(),
    });

    let mut lens = node("lens", route("first"));
    lens.access = AccessMode::ReadOnly;
    lens.depends_on = vec!["writer".into()];
    lens.source = Some(SourceBinding::Commit {
        commit: node_source.clone(),
    });

    // Writer predecessor committed and hit quota; its continuation succeeded with head_2.
    let writer_done = AttemptObservation::Terminal {
        attempt_ref: Some("host/writer-cont".into()),
        outcome: AttemptOutcome::Succeeded,
        launched: true,
        result: None,
        result_warning: None,
        result_path: None,
        branch: Some("writer".into()),
        commit: Some(head_2.clone()),
        settlement_reason: Some(provenant_records::run::SettlementReason::Completed),
        stop: None,
    };
    let lens_done = terminal(AttemptOutcome::Succeeded);

    let mut owner = FakeOwner {
        observations: [writer_done, lens_done].into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&graph(vec![write, lens]), &mut owner, &mut journal).unwrap();

    assert!(run.checkpoint.successful());
    assert_eq!(run.checkpoint.nodes[0].state, NodeState::Succeeded);
    assert_eq!(
        run.checkpoint.nodes[0]
            .outputs
            .as_ref()
            .and_then(|o| o.commit.as_ref()),
        Some(&head_2)
    );
    // Writer launched pinned to node_source.
    assert_eq!(owner.source_commits[0], Some(node_source.clone()));
    // Lens diff base stays the original node_source, not advancing to writer's intermediate/final head.
    assert_eq!(owner.source_commits[1], Some(node_source));
}

#[test]
fn composite_truncates_over_cap_findings_and_reaches_downstream() {
    for long in [false, true] {
        let document: GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version":1,"graph_id":"over-cap","nodes":[
                {"id":"reviews","kind":"composite","access":"read_only","composite":{
                    "nodes":[{"id":"a","access":"read_only","prompt":"review","route":{"alias":"senior"}},
                             {"id":"b","access":"read_only","prompt":"review","route":{"alias":"senior"}}],
                    "exports":[{"node":"a","outputs":["result","findings"]},{"node":"b","outputs":["result","findings"]}]
                }},
                {"id":"verify","access":"read_only","prompt":"verify","route":{"alias":"senior"},
                 "depends_on":["reviews"],"inputs":[{"node":"reviews","outputs":["result","findings"]}]}
            ]
        })).unwrap();
        let count = 20;
        let entries: Vec<_> = (0..count).map(|i| serde_json::json!({"id":format!("F{i}"),"severity":"high","file":"a", "summary":if long {"s".repeat(200)} else {"s".into()},"status":"open"})).collect();
        let text = format!(
            "```findings\n{}\n```",
            serde_json::json!({"schema_version":1,"findings":entries})
        );
        let observation = |path: &str| {
            let mut result = terminal_with_result(AttemptOutcome::Succeeded, &text);
            if let AttemptObservation::Terminal {
                result_path,
                attempt_ref,
                ..
            } = &mut result
            {
                *result_path = Some(path.into());
                *attempt_ref = Some(format!("fixture/{}", path.rsplit('/').next().unwrap()));
            }
            result
        };
        let mut owner = FakeOwner {
            observations: [
                observation("/results/a"),
                observation("/results/b"),
                terminal(AttemptOutcome::Succeeded),
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
            .find(|n| n.id == "reviews")
            .unwrap();
        assert_eq!(composite.state, NodeState::Succeeded);
        assert!(matches!(
            composite.truncation_cause,
            Some(NodeDecisionCause::CompositeFindingsTruncated { .. })
        ));
        let decoded: GraphCheckpoint =
            serde_json::from_slice(&serde_json::to_vec(&run.checkpoint).unwrap()).unwrap();
        assert_eq!(decoded, run.checkpoint);
        let mut forged = run.checkpoint.clone();
        forged
            .nodes
            .iter_mut()
            .find(|node| node.id == "reviews")
            .unwrap()
            .truncation_cause = None;
        let mut forged_journal = MemoryJournal(Some(forged));
        assert!(matches!(
            advance(&document, &mut FakeOwner::default(), &mut forged_journal),
            Err(GraphError::BadCheckpoint { .. })
        ));
        let outputs = composite.outputs.as_ref().unwrap();
        assert_eq!(outputs.findings_available, Some(false));
        let notices = outputs.warnings.join(" ");
        assert!(notices.contains(&format!("limits: {MAX_FINDINGS} entries")));
        assert!(notices.contains("DISPOSITION lines for every finding"));
        assert!(outputs.findings.len() < 2 * count);
        assert!(outputs.findings.len() <= MAX_FINDINGS);
        assert!(canonical_len(&outputs.findings) <= DISPOSITION_FINDINGS_BYTES);
        assert!(
            outputs.warnings[0]
                .contains(&format!("{} dropped", 2 * count - outputs.findings.len()))
        );
        assert!(
            outputs.warnings[0].contains("reviews--a")
                && outputs.warnings[0].contains("reviews--b")
                && !outputs.warnings[0].contains("/results/")
                && outputs.warnings[0].contains("fixture/a")
                && outputs.warnings[0].contains("fixture/b")
        );
        assert_eq!(
            run.checkpoint
                .nodes
                .iter()
                .find(|n| n.id == "verify")
                .unwrap()
                .state,
            NodeState::Succeeded
        );
        assert!(owner.prompts[2].contains("composite findings truncated"));
        if !long {
            let (plan, positions) = validate_plan(&document).unwrap();
            let mut spoofed = run.checkpoint.clone();
            let source = &mut spoofed.nodes[positions["reviews"]];
            source.truncation_cause = None;
            source.outputs.as_mut().unwrap().warnings =
                vec!["composite findings truncated: TOKEN_SPOOF".into()];
            let (prompt, _) = render_input(
                &plan.nodes[positions["verify"]],
                &plan,
                &spoofed,
                &positions,
                usize::MAX,
            );
            assert!(!prompt.contains("TOKEN_SPOOF"));
            let mut checkpoint = run.checkpoint.clone();
            checkpoint.nodes[positions["reviews--a"]]
                .outputs
                .as_mut()
                .unwrap()
                .result = None;
            let requests = upstream_results(
                &plan.nodes[positions["verify"]],
                &plan,
                &checkpoint,
                &positions,
            );
            let gate = requests
                .iter()
                .find(|request| request.label == "reviews--a")
                .unwrap();
            assert_eq!(gate.inline_findings.as_ref().unwrap().len(), count);
            assert_eq!(gate.finding_prefix.as_deref(), Some("reviews--a:"));
        }
        assert_eq!(
            advance(&document, &mut FakeOwner::default(), &mut journal)
                .unwrap()
                .checkpoint,
            run.checkpoint
        );
    }
}

#[test]
fn nested_composite_propagates_its_members_truncation_notice() {
    let doc: GraphDocument = serde_json::from_value(serde_json::json!({"schema_version":1,"graph_id":"nested-cap","nodes":[
        {"id":"outer","kind":"composite","access":"read_only","composite":{"nodes":[
            {"id":"inner","kind":"composite","access":"read_only","composite":{"nodes":[
                {"id":"a","access":"read_only","prompt":"review","route":{"alias":"senior"}},
                {"id":"b","access":"read_only","prompt":"review","route":{"alias":"senior"}}
            ],"exports":[{"node":"a","outputs":["result","findings"]},{"node":"b","outputs":["result","findings"]}]}}
        ],"exports":[{"node":"inner","outputs":["findings"]}]}},
        {"id":"next","access":"read_only","prompt":"use findings","route":{"alias":"senior"},"depends_on":["outer"],"inputs":[{"node":"outer","outputs":["findings"]}]}
    ]})).unwrap();
    let entries: Vec<_> = (0..20).map(|i| serde_json::json!({"id":format!("F{i}"),"severity":"high","file":"a","summary":"s","status":"open"})).collect();
    let text = format!(
        "```findings\n{}\n```",
        serde_json::json!({"schema_version":1,"findings":entries})
    );
    let mut owner = FakeOwner {
        observations: [
            terminal_with_result(AttemptOutcome::Succeeded, &text),
            terminal_with_result(AttemptOutcome::Succeeded, &text),
            terminal(AttemptOutcome::Succeeded),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(&doc, &mut owner, &mut MemoryJournal::default()).unwrap();
    let outer = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "outer")
        .unwrap();
    assert_eq!(outer.state, NodeState::Succeeded);
    assert!(
        outer
            .outputs
            .as_ref()
            .unwrap()
            .warnings
            .iter()
            .any(|warning| warning.contains("8 dropped"))
    );
    assert!(owner.prompts[2].contains("8 dropped"));
    let (plan, positions) = validate_plan(&doc).unwrap();
    let requests = upstream_results(
        &plan.nodes[positions["next"]],
        &plan,
        &run.checkpoint,
        &positions,
    );
    let member = requests
        .iter()
        .find(|request| request.label == "outer--inner--a")
        .unwrap();
    assert_eq!(
        member.finding_prefix.as_deref(),
        Some("outer--inner:outer--inner--a:")
    );
    let ids = truncated_finding_ids(
        &plan.nodes[positions["next"]],
        &plan,
        &run.checkpoint,
        &positions,
    );
    assert_eq!(ids.len(), 40);
    assert!(ids.contains(&(
        "outer--inner:outer--inner--a:F0".into(),
        "outer--inner--a:0".into()
    )));
    assert!(ids.contains(&(
        "outer--inner:outer--inner--b:F19".into(),
        "outer--inner--b:19".into()
    )));
}

// Small: a qualified ID that cannot fit must fail the composite, including on replay.
#[test]
fn composite_rejects_overlong_qualified_finding_id() {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version":1,"graph_id":"long-id","nodes":[
            {"id":"reviews","kind":"composite","access":"read_only","composite":{
                "nodes":[{"id":"a","access":"read_only","prompt":"review","route":{"alias":"senior"}}],
                "exports":[{"node":"a","outputs":["findings"]}]
            }},
            {"id":"synthesis","kind":"synthesise","access":"read_only","prompt":"dispose","route":{"alias":"senior"},
             "depends_on":["reviews"],"inputs":[{"node":"reviews","outputs":["findings"]}],"dispositions":["rejected"]}
        ]
    })).unwrap();
    let text = findings_text(&entry("").replace("F1", &"x".repeat(1024)));
    let mut owner = FakeOwner {
        observations: [terminal_with_result(AttemptOutcome::Succeeded, &text)].into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    let composite = run
        .checkpoint
        .nodes
        .iter()
        .find(|n| n.id == "reviews")
        .unwrap();
    assert_eq!(composite.state, NodeState::Failed);
    assert_eq!(
        composite.advance_error_code.as_deref(),
        Some("composite_findings_exceed_cap")
    );
    assert_ne!(
        run.checkpoint
            .nodes
            .iter()
            .find(|n| n.id == "synthesis")
            .unwrap()
            .state,
        NodeState::Succeeded
    );
    assert_eq!(
        advance(&document, &mut FakeOwner::default(), &mut journal)
            .unwrap()
            .checkpoint,
        run.checkpoint
    );
}

// Small: inherited descriptions can exceed the ordinary findings budget.
#[test]
fn identity_dispositions_preserve_large_inherited_fields() {
    let mut synthesis = node("synthesis", route("first"));
    synthesis.dispositions = DISPOSITIONS.map(String::from).to_vec();
    synthesis.depends_on = vec!["review".into()];
    synthesis.inputs = vec![GraphInput {
        required_outputs: Vec::new(),
        extra: Default::default(),
        node: "review".into(),
        outputs: vec![OutputKind::Findings],
    }];
    let long = "x".repeat(900);
    let entries: Vec<_> = (0..3).map(|i| serde_json::json!({"id":format!("F{i}"),"severity":"high","file":long,"summary":long,"status":long})).collect();
    let review = format!(
        "```findings\n{}\n```\nVERDICT: PASS",
        serde_json::json!({"schema_version":1,"findings":entries})
    );
    let decisions = (0..3)
        .map(|i| {
            format!(
                r#"{{"id":"F{i}","disposition":"rejected","reason":"{}"}}"#,
                "r".repeat(700)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let mut owner = FakeOwner {
        observations: [
            terminal_with_result(AttemptOutcome::Succeeded, &review),
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&decisions)),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![node("review", route("first")), synthesis]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    let result = &run.checkpoint.nodes[1];
    assert_eq!(
        result.state,
        NodeState::Succeeded,
        "{:?}",
        result.advance_error
    );
    let findings = &result.outputs.as_ref().unwrap().findings;
    assert!(canonical_len(findings) > FINDINGS_BYTES);
    assert_eq!(findings[0].summary, long);
}

// Small: directly selected sources with the same ID remain distinct.
#[test]
fn direct_reviewers_with_duplicate_ids_require_distinct_dispositions() {
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
            r#"{"id":"F1","disposition":"rejected","reason":"no"}"#,
            NodeState::Failed,
        ),
        (
            r#"{"id":"a:F1","disposition":"rejected","reason":"no"},{"id":"b:F1","disposition":"rejected","reason":"no"}"#,
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
            &graph(vec![
                node("a", route("first")),
                node("b", route("first")),
                synthesis.clone(),
            ]),
            &mut owner,
            &mut MemoryJournal::default(),
        )
        .unwrap();
        let result = &run.checkpoint.nodes[2];
        assert!(owner.prompts[2].contains("a:F1"));
        assert!(owner.prompts[2].contains("b:F1"));
        assert_eq!(result.state, expected, "{:?}", result.advance_error);
        if expected == NodeState::Succeeded {
            let findings = &result.outputs.as_ref().unwrap().findings;
            assert_eq!(
                (&findings[0].summary, &findings[1].summary),
                (&"s".to_owned(), &"other".to_owned())
            );
        } else {
            assert!(
                result
                    .advance_error
                    .as_deref()
                    .unwrap()
                    .contains("ambiguous")
            );
        }
    }
}

#[test]
fn empty_review_range_stops_before_lens_launch() {
    let head = "1".repeat(40);
    let mut review = node("review", route("review"));
    review.role = Some("lens.correctness-spec".into());
    review.source = Some(SourceBinding::Commit {
        commit: head.clone(),
    });
    let mut owner = FakeOwner {
        graph_bases: (Some(head.parse().unwrap()), None),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![review]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert!(owner.calls.is_empty());
    assert_eq!(run.checkpoint.nodes[0].state, NodeState::Failed);
    assert_eq!(
        run.checkpoint.nodes[0].advance_error_code.as_deref(),
        Some("review_empty_range")
    );
}

#[test]
fn equal_review_range_cannot_pass_a_reduced_panel_gate() {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "graph_id": "empty-quick-review", "nodes": [
            {"id":"lenses", "kind":"composite", "access":"read_only",
             "composite":{"reduce_verdict":true,"min_successful":1,
                "nodes":[{"id":"lens","role":"lens.correctness-spec","access":"read_only",
                          "prompt":"review","route":{"alias":"fixture"}}],
                "exports":[{"node":"lens","outputs":["result"]}]}},
            {"id":"accept","kind":"gate","access":"read_only","depends_on":["lenses"],
             "inputs":[{"node":"lenses","outputs":["verdict"]}],
             "condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"}}
        ]
    }))
    .unwrap();
    let mut owner = FakeOwner {
        equal_range: true,
        observations: [terminal_from("VERDICT: PASS", "host/one")].into(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    let panel = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "lenses")
        .unwrap();
    assert_eq!(
        panel
            .outputs
            .as_ref()
            .and_then(|output| output.verdict.as_deref()),
        Some("VERDICT: INCONCLUSIVE")
    );
    let gate = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "accept")
        .unwrap();
    assert!(!gate.gate.as_ref().unwrap().passed);
}

#[test]
fn equal_review_range_warns_on_review_nodes() {
    let mut review = node("review", route("review"));
    review.role = Some("lens.correctness-spec".into());
    let mut synthesis = node("synthesis", route("review"));
    synthesis.role = Some("review.synthesis".into());
    synthesis.depends_on = vec!["review".into()];
    let mut other = node("other", route("other"));
    other.role = Some("scout".into());
    other.depends_on = vec!["synthesis".into()];
    let mut gate = gate_node(GateCondition::Verdict {
        extra: Default::default(),
        expected: "VERDICT: PASS".into(),
        threshold: VerdictThreshold::All,
    });
    gate.depends_on = vec!["synthesis".into()];
    gate.inputs = vec![GraphInput {
        extra: Default::default(),
        node: "synthesis".into(),
        outputs: vec![OutputKind::Verdict],
        required_outputs: Vec::new(),
    }];
    let pass = "```findings\n{\"schema_version\":1,\"findings\":[]}\n```\nVERDICT: PASS";
    let mut owner = FakeOwner {
        equal_range: true,
        observations: [
            terminal_from(pass, "host/one"),
            terminal_from(pass, "host/two"),
            terminal_from(pass, "host/three"),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![review, synthesis, other, gate]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    for progress in &run.checkpoint.nodes[..2] {
        let outputs = progress.outputs.as_ref().unwrap();
        assert_eq!(outputs.verdict.as_deref(), Some("VERDICT: INCONCLUSIVE"));
        assert!(
            outputs
                .warnings
                .iter()
                .any(|warning| warning.contains(&format!(
                    "base {}, head {}",
                    "1".repeat(40),
                    "1".repeat(40)
                )))
        );
    }
    assert!(
        run.checkpoint.nodes[2]
            .outputs
            .as_ref()
            .unwrap()
            .warnings
            .is_empty()
    );
    assert!(!run.checkpoint.nodes[3].gate.as_ref().unwrap().passed);

    let mut review = node("review", route("review"));
    review.role = Some("lens.correctness-spec".into());
    let mut owner = FakeOwner {
        no_review_base: true,
        observations: [terminal_from(pass, "host/one")].into(),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![review]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    let outputs = run.checkpoint.nodes[0].outputs.as_ref().unwrap();
    assert_eq!(outputs.verdict.as_deref(), Some("VERDICT: PASS"));
    assert!(
        outputs
            .warnings
            .iter()
            .any(|warning| warning.contains("no base was resolved"))
    );
    assert!(
        !outputs
            .warnings
            .iter()
            .any(|warning| warning.contains("equal_base_and_head"))
    );

    let mut review = node("review", route("review"));
    review.role = Some("lens.correctness-spec".into());
    let malformed =
        findings_text(r#"{"id":"F1","severity":1,"file":1,"summary":1,"status":1,"line":-1}"#);
    let mut owner = FakeOwner {
        equal_range: true,
        observations: [terminal_from(&malformed, "host/one")].into(),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![review]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    let outputs = run.checkpoint.nodes[0].outputs.as_ref().unwrap();
    assert!(
        outputs
            .warnings
            .iter()
            .any(|warning| warning.contains("equal_base_and_head"))
    );
}

#[test]
fn composite_byte_budget_overflow_below_entry_cap_requires_verifier_proof() {
    let count = 20;
    assert!(count < MAX_FINDINGS);
    let nodes: Vec<_> = (0..count).map(|i| serde_json::json!({
        "id":format!("lens{i}"),"access":"read_only","prompt":"review","route":{"alias":"fixture"}
    })).collect();
    let exports: Vec<_> = (0..count)
        .map(|i| serde_json::json!({"node":format!("lens{i}"),"outputs":["findings","result"]}))
        .collect();
    let mut verifier = template::Compiler::new(&template::TemplateLibrary::bundled())
        .compile(
            &serde_json::json!({"id":"verify","use":"review.verify","route":{"alias":"fixture"}}),
        )
        .unwrap();
    verifier.depends_on = vec!["reviews".into()];
    verifier.inputs = vec![
        serde_json::from_value(
            serde_json::json!({"node":"reviews","outputs":["findings","result"]}),
        )
        .unwrap(),
    ];
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version":1,"graph_id":"byte-overflow","nodes":[{
            "id":"reviews","kind":"composite","access":"read_only",
            "composite":{"nodes":nodes,"exports":exports}
        },verifier]
    }))
    .unwrap();
    let entry = serde_json::json!({"id":"F1","severity":"high","file":"a.rs","summary":"Unchecked dispatch","status":"open",
        "evidence":"A reachable dispatch path fails. ".repeat(210),"proposed_fix":"Validate before dispatch"});
    let text = format!(
        "```findings\n{}\n```\nVERDICT: FAIL\n",
        serde_json::json!({"schema_version":1,"findings":[entry]})
    );
    let mut observations = VecDeque::new();
    for _ in 0..count {
        observations.push_back(terminal_with_result(AttemptOutcome::Succeeded, &text));
    }
    observations.push_back(terminal_with_result(
        AttemptOutcome::Succeeded,
        "```findings\n{\"schema_version\":1,\"findings\":[]}\n```\nVERDICT: INCONCLUSIVE\n",
    ));
    let mut owner = FakeOwner {
        observations,
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    let (plan, positions) = validate_plan(&document).unwrap();
    let full: Vec<_> = (0..count)
        .flat_map(|i| {
            materialised_findings(
                positions[&format!("reviews--lens{i}")],
                &plan,
                &run.checkpoint,
                &positions,
            )
        })
        .collect();
    assert_eq!(full.len(), count);
    assert!(canonical_len(&full) > DISPOSITION_FINDINGS_BYTES);
    let composite = &run.checkpoint.nodes[positions["reviews"]];
    assert!(matches!(
        composite.truncation_cause,
        Some(NodeDecisionCause::CompositeFindingsTruncated { .. })
    ));
    assert!(composite.outputs.as_ref().unwrap().findings.len() < count);
    let verifier = &run.checkpoint.nodes[positions["verify"]];
    assert_eq!(
        verifier.advance_error_code.as_deref(),
        Some("disposition_overflow_incomplete")
    );
    assert!(owner.prompts.last().unwrap().contains("DISPOSITION"));
    assert_eq!(
        advance(&document, &mut owner, &mut journal)
            .unwrap()
            .checkpoint,
        run.checkpoint
    );
}
