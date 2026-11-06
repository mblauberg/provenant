use super::*;
use serde_json::json;

fn retry(
    document: &GraphDocument,
    checkpoint: &mut GraphCheckpoint,
    brief: Option<&str>,
    scan: &dyn Fn(&str) -> Result<(), String>,
    journal: Option<&dyn Fn(&patch::PatchRecord) -> Result<(), String>>,
) -> Result<patch::Admitted, patch::PatchError> {
    retry_id(document, checkpoint, "work", brief, scan, journal)
}

fn retry_id(
    document: &GraphDocument,
    checkpoint: &mut GraphCheckpoint,
    id: &str,
    brief: Option<&str>,
    scan: &dyn Fn(&str) -> Result<(), String>,
    journal: Option<&dyn Fn(&patch::PatchRecord) -> Result<(), String>>,
) -> Result<patch::Admitted, patch::PatchError> {
    patch::retry_node(
        &mut patch::LiveGraph {
            document,
            checkpoint,
        },
        id,
        brief,
        &patch::PatchContext {
            library: &template::TemplateLibrary::bundled(),
            resolvers: &Default::default(),
            scan,
            journal,
            limits: &plans::generous(),
        },
    )
}

fn settled() -> (GraphDocument, GraphCheckpoint, AttemptKey) {
    let document = graph(vec![node("work", route("first"))]);
    let mut owner = FakeOwner {
        observations: [done_as("first-attempt")].into(),
        ..Default::default()
    };
    let checkpoint = advance(&document, &mut owner, &mut MemoryJournal::default())
        .unwrap()
        .checkpoint;
    assert_eq!(checkpoint.nodes[0].state, NodeState::Succeeded);
    (document, checkpoint, owner.calls[0].0.clone())
}

#[test]
fn node_redo_journals_the_brief_and_launches_a_new_attempt_without_losing_history() {
    let (document, mut checkpoint, original_key) = settled();
    let original_attempt = checkpoint.nodes[0].attempts[0].clone();
    let records = std::cell::RefCell::new(Vec::new());
    let record = |patch: &patch::PatchRecord| {
        records.borrow_mut().push(patch.clone());
        Ok(())
    };
    let admitted = retry(
        &document,
        &mut checkpoint,
        Some("Replacement brief"),
        &|_| Ok(()),
        Some(&record),
    )
    .unwrap();
    assert_eq!(admitted.revision, 1);
    assert_eq!(records.borrow().len(), 1);
    assert_eq!(records.borrow()[0].replaced[0].prompt, "Replacement brief");
    assert_eq!(
        checkpoint.expansions[0].patch.as_ref().unwrap().digest,
        records.borrow()[0].digest
    );
    assert_eq!(checkpoint.nodes[0].state, NodeState::Pending);
    assert_eq!(
        checkpoint.nodes[0].previous_attempts,
        vec![original_attempt]
    );
    let mut owner = FakeOwner {
        observations: [done_as("second-attempt")].into(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal(Some(checkpoint))).unwrap();
    assert_eq!(run.checkpoint.nodes[0].state, NodeState::Succeeded);
    assert_eq!(owner.prompts, vec!["Replacement brief"]);
    assert_ne!(owner.calls[0].0, original_key);
    assert_eq!(
        run.checkpoint.nodes[0].attempts[0].attempt_ref.as_deref(),
        Some("second-attempt")
    );
    let mut checkpoint = run.checkpoint;
    let second_key = owner.calls[0].0.clone();
    retry(&document, &mut checkpoint, None, &|_| Ok(()), None).unwrap();
    assert_eq!(checkpoint.nodes[0].previous_attempts.len(), 2);
    let mut owner = FakeOwner {
        observations: [
            AttemptObservation::Pending {
                attempt_ref: Some("third-attempt".into()),
            },
            done_as("third-attempt"),
        ]
        .into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal(Some(checkpoint));
    advance(&document, &mut owner, &mut journal).unwrap();
    assert_ne!(owner.calls[0].0, second_key);
    assert_eq!(owner.prompts, vec!["Replacement brief"]);
    let reloaded: GraphCheckpoint =
        serde_json::from_value(serde_json::to_value(journal.0.take().unwrap()).unwrap()).unwrap();
    journal.0 = Some(reloaded);
    let completed = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(completed.checkpoint.nodes[0].state, NodeState::Succeeded);
    assert_eq!(owner.calls[0].0, owner.calls[1].0);
}

#[test]
fn node_redo_refuses_live_attempts_secrets_and_journal_failures_atomically() {
    let (document, checkpoint, _) = settled();
    for (state, attempt_state) in [
        (NodeState::Running, AttemptState::Terminal),
        (NodeState::Succeeded, AttemptState::Running),
    ] {
        let mut candidate = checkpoint.clone();
        candidate.nodes[0].state = state;
        candidate.nodes[0].attempts[0].state = attempt_state;
        let before = candidate.clone();
        assert!(retry(&document, &mut candidate, None, &|_| Ok(()), None).is_err());
        assert_eq!(candidate, before);
    }
    let mut candidate = checkpoint.clone();
    let secret = retry(
        &document,
        &mut candidate,
        Some("private brief"),
        &|text| {
            if text.contains("private brief") {
                Err("secret".into())
            } else {
                Ok(())
            }
        },
        None,
    )
    .unwrap_err();
    assert_eq!(secret.code, "secret");
    assert_eq!(candidate, checkpoint);
    let error = retry(
        &document,
        &mut candidate,
        Some("replacement"),
        &|_| Ok(()),
        Some(&|_| Err("journal unavailable".into())),
    )
    .unwrap_err();
    assert_eq!(error.code, "journal");
    assert_eq!(candidate, checkpoint);
}

#[test]
fn node_redo_refuses_structural_nodes_without_mutating_the_checkpoint() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1,"graph_id":"composite-retry","nodes":[{
            "id":"work","kind":"composite","access":"read_only","composite":{
                "nodes":[{"id":"leaf","prompt":"work","access":"read_only","route":{"adapter":"first","model":"first/model"}}],
                "exports":[{"node":"leaf","outputs":["result"]}]
            }
        }]
    })).unwrap();
    let mut owner = FakeOwner {
        observations: [done_as("first-attempt")].into(),
        ..Default::default()
    };
    let mut checkpoint = advance(&document, &mut owner, &mut MemoryJournal::default())
        .unwrap()
        .checkpoint;
    let before = checkpoint.clone();
    assert!(
        retry(
            &document,
            &mut checkpoint,
            Some("replacement"),
            &|_| Ok(()),
            None
        )
        .is_err()
    );
    assert_eq!(checkpoint, before);
}

#[test]
fn node_redo_reopens_only_the_selected_node_of_a_cancelled_graph() {
    let mut sibling = node("sibling", route("first"));
    sibling.depends_on = vec!["work".into()];
    let document = graph(vec![node("work", route("first")), sibling]);
    let mut owner = FakeOwner {
        observations: [AttemptObservation::Pending {
            attempt_ref: Some("first-attempt".into()),
        }]
        .into(),
        ..Default::default()
    };
    let mut checkpoint = advance(&document, &mut owner, &mut MemoryJournal::default())
        .unwrap()
        .checkpoint;
    checkpoint.cancelled = true;
    checkpoint.cancel_reason = Some("chair stopped graph".into());
    let mut cancellation_owner = FakeOwner {
        cancelled_attempt_settled: true,
        ..Default::default()
    };
    settle_cancelled_nodes(
        &mut checkpoint,
        &mut cancellation_owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert_eq!(checkpoint.nodes[0].state, NodeState::Skipped);
    assert_eq!(checkpoint.nodes[0].attempts[0].state, AttemptState::Running);
    retry(
        &document,
        &mut checkpoint,
        Some("redo selected work"),
        &|_| Ok(()),
        None,
    )
    .unwrap();
    assert_eq!(checkpoint.nodes[0].state, NodeState::Pending);
    assert_eq!(checkpoint.nodes[1].state, NodeState::Skipped);
    assert!(!checkpoint.cancelled);
    let mut owner = FakeOwner {
        observations: [done_as("second-attempt")].into(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal(Some(checkpoint))).unwrap();
    assert_eq!(run.checkpoint.nodes[0].state, NodeState::Succeeded);
    assert_eq!(run.checkpoint.nodes[1].state, NodeState::Skipped);
    assert_eq!(owner.calls.len(), 1);
    assert_eq!(owner.calls[0].0.node_id, "work");
}

#[test]
fn node_redo_retains_spent_budget_and_unknown_usage_across_attempt_generations() {
    let (document, mut checkpoint, _) = settled();
    checkpoint.nodes[0].attempts[0].usage = Some(AttemptUsage {
        tokens: Some(10),
        cost_micro_usd: Some(20),
        wall_time_seconds: Some(30),
        ..Default::default()
    });
    let budgets = [
        crate::config::GraphBudget {
            per_graph: crate::config::BudgetLimit {
                tokens: Some(10),
                ..Default::default()
            },
            ..Default::default()
        },
        crate::config::GraphBudget {
            per_node: crate::config::BudgetLimit {
                cost_usd: Some(0.000020),
                ..Default::default()
            },
            ..Default::default()
        },
        crate::config::GraphBudget {
            per_graph: crate::config::BudgetLimit {
                wall_time_seconds: Some(30),
                ..Default::default()
            },
            ..Default::default()
        },
    ];
    let reasons: Vec<_> = budgets
        .iter()
        .map(|budget| budget_reason(&checkpoint, budget))
        .collect();
    assert!(reasons.iter().all(Option::is_some));
    retry(&document, &mut checkpoint, None, &|_| Ok(()), None).unwrap();
    assert_eq!(
        budgets
            .iter()
            .map(|budget| budget_reason(&checkpoint, budget))
            .collect::<Vec<_>>(),
        reasons
    );
    let mut owner = FakeOwner::default();
    let run = advance_with_budget(
        &document,
        &mut owner,
        &mut MemoryJournal(Some(checkpoint)),
        &Default::default(),
        &budgets[0],
    )
    .unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert!(owner.calls.is_empty());

    let (_, mut checkpoint, _) = settled();
    checkpoint.nodes[0].attempts[0].usage = None;
    let budget = crate::config::GraphBudget {
        per_graph: crate::config::BudgetLimit {
            tokens: Some(100),
            ..Default::default()
        },
        unknown_usage: crate::config::UnknownUsage::Pause,
        ..Default::default()
    };
    let reason = budget_reason(&checkpoint, &budget);
    assert!(reason.is_some());
    retry(&document, &mut checkpoint, None, &|_| Ok(()), None).unwrap();
    assert_eq!(budget_reason(&checkpoint, &budget), reason);
}

#[test]
fn subtree_stop_clears_a_launch_retry_wake_when_no_process_was_accepted() {
    let document = graph(vec![node("work", route("first"))]);
    let mut owner = FakeOwner {
        observations: [AttemptObservation::Pending { attempt_ref: None }].into(),
        ..Default::default()
    };
    let mut checkpoint = advance(&document, &mut owner, &mut MemoryJournal::default())
        .unwrap()
        .checkpoint;
    let now = provenant_core::WallTime::from_micros(1_800_000_000_000_000).unwrap();
    apply_launch_disposition(
        &mut checkpoint,
        0,
        &crate::coordination::facade::FacadeError::Busy,
        Some(now),
        60,
        LaunchRetryPolicy::default(),
    )
    .unwrap();
    asks::refresh_wake(&mut checkpoint);
    assert!(checkpoint.wake_at.is_some());
    let effect =
        cancel_node_subtree(&document, &mut checkpoint, "work", Some("chair stop")).unwrap();
    assert_eq!(effect.running, vec!["work"]);
    let mut owner = FakeOwner {
        cancelled_attempt_settled: true,
        ..Default::default()
    };
    let mut journal = MemoryJournal(Some(checkpoint.clone()));
    settle_stopped_subtrees(&document, &mut checkpoint, &mut owner, &mut journal).unwrap();
    assert_eq!(checkpoint.nodes[0].state, NodeState::Skipped);
    assert_eq!(checkpoint.wake_at, None);
    let replay = advance(&document, &mut owner, &mut MemoryJournal(Some(checkpoint))).unwrap();
    assert_eq!(replay.checkpoint.nodes[0].state, NodeState::Skipped);
    assert!(owner.calls.is_empty());
}

#[test]
fn subtree_stop_fences_live_parent_and_children_across_replay_and_preserves_detached_work() {
    let mut detached = node("work--keep", route("first"));
    detached.extra.insert(relay::DETACHED.into(), true.into());
    let document = graph(vec![
        node("work", route("first")),
        node("work--child", route("first")),
        detached,
    ]);
    let mut owner = FakeOwner {
        observations: ["parent", "child", "detached"]
            .into_iter()
            .map(|id| AttemptObservation::Pending {
                attempt_ref: Some(id.into()),
            })
            .collect(),
        ..Default::default()
    };
    let mut checkpoint = advance(&document, &mut owner, &mut MemoryJournal::default())
        .unwrap()
        .checkpoint;
    let detached_before = checkpoint.nodes[2].clone();
    let effect = cancel_node_subtree(&document, &mut checkpoint, "work", None).unwrap();
    assert_eq!(
        effect
            .running
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        ["work".to_owned(), "work--child".to_owned()].into()
    );
    assert_eq!(effect.detached, vec!["work--keep"]);
    let mut stopping_owner = FakeOwner::default();
    let mut journal = MemoryJournal(Some(checkpoint.clone()));
    settle_stopped_subtrees(
        &document,
        &mut checkpoint,
        &mut stopping_owner,
        &mut journal,
    )
    .unwrap();
    assert_eq!(stopping_owner.cancellation_stops, 2);
    assert_eq!(checkpoint.nodes[2], detached_before);
    let mut checkpoint: GraphCheckpoint =
        serde_json::from_value(serde_json::to_value(checkpoint).unwrap()).unwrap();
    let mut settled_owner = FakeOwner {
        cancelled_attempt_settled: true,
        ..Default::default()
    };
    settle_stopped_subtrees(&document, &mut checkpoint, &mut settled_owner, &mut journal).unwrap();
    assert_eq!(checkpoint.nodes[0].state, NodeState::Skipped);
    assert_eq!(checkpoint.nodes[1].state, NodeState::Skipped);
    assert_eq!(checkpoint.nodes[2], detached_before);
    assert_eq!(settled_owner.cancellation_stops, 0);
    let mut owner = FakeOwner {
        observations: [AttemptObservation::Pending {
            attempt_ref: Some("detached".into()),
        }]
        .into(),
        ..Default::default()
    };
    let replay = advance(&document, &mut owner, &mut MemoryJournal(Some(checkpoint))).unwrap();
    assert_eq!(replay.checkpoint.nodes[0].state, NodeState::Skipped);
    assert_eq!(replay.checkpoint.nodes[1].state, NodeState::Skipped);
    assert_eq!(owner.calls.len(), 1);
    assert_eq!(owner.calls[0].0.node_id, "work--keep");
}

#[test]
fn node_redo_planner_admits_a_new_fragment_without_losing_the_first_expansion() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1,"graph_id":"redo-plan","nodes":[{
            "id":"work","kind":"plan","access":"read_only","prompt":"Plan work",
            "route":{"alias":"senior"},"plan":{"kinds":["task"],"max_nodes":4}
        }]
    }))
    .unwrap();
    let fragment = |id: &str| {
        format!(
            "```fragment\n{}\n```",
            json!({"schema_version":1,"nodes":[{
                "id":id,"goal":"Do a slice","acceptance_checks":["Verify it"],"access":"read_only"
            }]})
        )
    };
    let mut owner = FakeOwner {
        observations: [
            terminal_from(&fragment("first"), "first-plan"),
            done_as("first-child"),
        ]
        .into(),
        ..Default::default()
    };
    let mut checkpoint = advance(&document, &mut owner, &mut MemoryJournal::default())
        .unwrap()
        .checkpoint;
    assert!(
        checkpoint
            .nodes
            .iter()
            .any(|n| n.id == "work--first" && n.state == NodeState::Succeeded)
    );
    retry(
        &document,
        &mut checkpoint,
        Some("Plan another slice"),
        &|_| Ok(()),
        None,
    )
    .unwrap();
    let mut owner = FakeOwner {
        observations: [
            terminal_from(&fragment("second"), "second-plan"),
            done_as("second-child"),
        ]
        .into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal(Some(checkpoint));
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    for id in ["work--first", "work--second"] {
        assert!(
            run.checkpoint
                .nodes
                .iter()
                .any(|n| n.id == id && n.state == NodeState::Succeeded)
        );
    }
    assert_eq!(owner.calls.len(), 2);
    assert_eq!(owner.calls[0].0.node_id, "work");
    assert_eq!(owner.calls[1].0.node_id, "work--second");
    let mut checkpoint = run.checkpoint;
    retry_id(
        &document,
        &mut checkpoint,
        "work--first",
        Some("Redo first slice"),
        &|_| Ok(()),
        None,
    )
    .unwrap();
    let mut owner = FakeOwner {
        observations: [done_as("first-child-redone")].into(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal(Some(checkpoint))).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert_eq!(owner.calls.len(), 1);
    assert_eq!(owner.calls[0].0.node_id, "work--first");
}

#[test]
fn node_redo_can_launch_a_leaf_from_an_inactive_loop_round() {
    let document: GraphDocument =
        serde_json::from_str(include_str!("../fixtures/loop-pass-round-two.json")).unwrap();
    let mut owner = FakeOwner {
        observations: [
            terminal_from("VERDICT: FAIL", "first-review"),
            terminal_from("VERDICT: PASS", "second-review"),
        ]
        .into(),
        ..Default::default()
    };
    let mut checkpoint = advance(&document, &mut owner, &mut MemoryJournal::default())
        .unwrap()
        .checkpoint;
    let id = "revise--r1--review";
    assert_eq!(
        checkpoint
            .nodes
            .iter()
            .find(|n| n.id == "revise")
            .unwrap()
            .state,
        NodeState::Succeeded
    );
    retry_id(
        &document,
        &mut checkpoint,
        id,
        Some("Redo the earlier review"),
        &|_| Ok(()),
        None,
    )
    .unwrap();
    let mut owner = FakeOwner {
        observations: [terminal_from("VERDICT: PASS", "first-review-redone")].into(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal(Some(checkpoint))).unwrap();
    assert_eq!(
        run.checkpoint
            .nodes
            .iter()
            .find(|n| n.id == id)
            .unwrap()
            .state,
        NodeState::Succeeded
    );
    assert_eq!(owner.calls.len(), 1);
    assert_eq!(owner.calls[0].0.node_id, id);
}

#[test]
fn node_redo_repeated_planner_fallbacks_have_distinct_nodes_and_replay_without_relaunching() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1,"graph_id":"redo-fallback","nodes":[{
            "id":"work","kind":"plan","access":"read_only","prompt":"Plan work",
            "route":{"alias":"senior"},"plan":{"kinds":["task"],"max_nodes":4,
                "fallback":{"nodes":[{"id":"static","access":"read_only","prompt":"Fallback work","route":{"alias":"junior"}}],
                    "exports":[{"node":"static","outputs":["result"]}]}
            }
        }]
    })).unwrap();
    let mut journal = MemoryJournal::default();
    let mut fallback_ids = std::collections::BTreeSet::new();
    for generation in 0..3 {
        if generation > 0 {
            let checkpoint = journal.0.as_mut().unwrap();
            retry(
                &document,
                checkpoint,
                Some("Try planning again"),
                &|_| Ok(()),
                None,
            )
            .unwrap();
        }
        let mut owner = FakeOwner {
            observations: [
                terminal_from("not a fragment", &format!("planner-{generation}")),
                terminal_from("fallback result", &format!("fallback-{generation}")),
            ]
            .into(),
            ..Default::default()
        };
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        assert_eq!(
            run.status,
            GraphStatus::Complete,
            "{:?}",
            run.checkpoint.pause_reason
        );
        assert_eq!(owner.calls.len(), 2);
        assert!(fallback_ids.insert(owner.calls[1].0.node_id.clone()));
        let checkpoint: GraphCheckpoint =
            serde_json::from_value(serde_json::to_value(run.checkpoint).unwrap()).unwrap();
        journal.0 = Some(checkpoint.clone());
        let mut replay_owner = FakeOwner::default();
        let replay = advance(&document, &mut replay_owner, &mut journal).unwrap();
        assert_eq!(replay.checkpoint, checkpoint);
        assert!(replay_owner.calls.is_empty());
    }
}

#[test]
fn node_redo_can_launch_a_leaf_of_a_settled_composite() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1,"graph_id":"redo-composite-leaf","nodes":[{
            "id":"work","kind":"composite","access":"read_only","composite":{
                "nodes":[{"id":"leaf","access":"read_only","prompt":"Do the work","route":{"alias":"senior"}}],
                "exports":[{"node":"leaf","outputs":["result"]}]
            }
        }]
    })).unwrap();
    let mut owner = FakeOwner {
        observations: [terminal_from("first result", "first-attempt")].into(),
        ..Default::default()
    };
    let mut checkpoint = advance(&document, &mut owner, &mut MemoryJournal::default())
        .unwrap()
        .checkpoint;
    assert_eq!(
        checkpoint
            .nodes
            .iter()
            .find(|n| n.id == "work")
            .unwrap()
            .state,
        NodeState::Succeeded
    );
    retry_id(
        &document,
        &mut checkpoint,
        "work--leaf",
        Some("Redo the leaf"),
        &|_| Ok(()),
        None,
    )
    .unwrap();
    let mut owner = FakeOwner {
        observations: [terminal_from("replacement result", "replacement-attempt")].into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal(Some(checkpoint));
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert_eq!(
        run.checkpoint
            .nodes
            .iter()
            .find(|n| n.id == "work--leaf")
            .unwrap()
            .state,
        NodeState::Succeeded
    );
    assert_eq!(owner.calls.len(), 1);
    assert_eq!(owner.calls[0].0.node_id, "work--leaf");
    let mut replay_owner = FakeOwner::default();
    assert_eq!(
        advance(&document, &mut replay_owner, &mut journal)
            .unwrap()
            .checkpoint,
        run.checkpoint
    );
    assert!(replay_owner.calls.is_empty());
}

#[test]
fn retry_of_explicit_launch_refusal_preserves_pending_siblings_and_rejects_uncertain_attempts() {
    let document = graph(vec![
        node("work", route("first")),
        node("sibling", route("first")),
    ]);
    let limits = crate::config::GraphLimits {
        concurrency: Some(1),
        ..Default::default()
    };
    let mut owner = FakeOwner {
        observations: [AttemptObservation::Pending { attempt_ref: None }].into(),
        ..Default::default()
    };
    let mut checkpoint = advance_with_budget(
        &document,
        &mut owner,
        &mut MemoryJournal::default(),
        &limits,
        &Default::default(),
    )
    .unwrap()
    .checkpoint;
    assert_eq!(checkpoint.nodes[1].state, NodeState::Pending);
    // Recreate the durable checkpoint before the synthetic submission returned.
    // Acceptance is deliberately checked through the owner, not these stale flags.
    checkpoint.nodes[0].attempts[0].state = AttemptState::Starting;
    checkpoint.nodes[0].attempts[0].launched = false;
    let seeded = checkpoint.clone();
    let mut refusing_owner = ErrorLauncher::default();
    launch_disposition::<ErrorLauncher, MemoryJournal>(
        &mut refusing_owner,
        &mut checkpoint,
        0,
        &crate::coordination::facade::FacadeError::InvalidInput { detail: None },
        Some(at(0)),
        60,
        LaunchRetryPolicy::default(),
        &mut MemoryJournal::default(),
    )
    .unwrap();
    let mut checkpoint: GraphCheckpoint =
        serde_json::from_value(serde_json::to_value(checkpoint).unwrap()).unwrap();
    assert_eq!(
        checkpoint.nodes[0].attempts[0]
            .extra
            .get(PRELAUNCH_REFUSAL_PROOF),
        Some(&json!(true))
    );
    // Accepted operations may still have stale Starting/false/None graph rows.
    // Neither a blocked replay nor corrupt accepted state proves non-acceptance.
    for error in [
        crate::coordination::facade::FacadeError::DispatchRefused {
            reason: "custody release failed".into(),
            disk: None,
            fact: None,
        },
        crate::coordination::facade::FacadeError::StateCorrupt,
    ] {
        let mut candidate = seeded.clone();
        let mut accepted_owner = ErrorLauncher {
            accepted: true,
            ..Default::default()
        };
        launch_disposition::<ErrorLauncher, MemoryJournal>(
            &mut accepted_owner,
            &mut candidate,
            0,
            &error,
            Some(at(0)),
            60,
            LaunchRetryPolicy::default(),
            &mut MemoryJournal::default(),
        )
        .unwrap();
        assert!(
            !candidate.nodes[0].attempts[0]
                .extra
                .contains_key(PRELAUNCH_REFUSAL_PROOF)
        );
        let before = candidate.clone();
        assert!(retry(&document, &mut candidate, None, &|_| Ok(()), None).is_err());
        assert_eq!(candidate, before);
    }
    let mut lookup_failed = checkpoint.clone();
    let mut unavailable_owner = ErrorLauncher {
        acceptance_lookup_error: true,
        ..Default::default()
    };
    launch_disposition::<ErrorLauncher, MemoryJournal>(
        &mut unavailable_owner,
        &mut lookup_failed,
        0,
        &crate::coordination::facade::FacadeError::StateCorrupt,
        Some(at(0)),
        60,
        LaunchRetryPolicy::default(),
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert!(
        !lookup_failed.nodes[0].attempts[0]
            .extra
            .contains_key(PRELAUNCH_REFUSAL_PROOF)
    );
    let before = lookup_failed.clone();
    assert!(retry(&document, &mut lookup_failed, None, &|_| Ok(()), None).is_err());
    assert_eq!(lookup_failed, before);
    let mut unknown = seeded;
    launch_disposition::<ErrorLauncher, MemoryJournal>(
        &mut refusing_owner,
        &mut unknown,
        0,
        &crate::coordination::facade::FacadeError::LaunchUnknown {
            operation_id: provenant_core::OperationId::parse("unknown-operation").unwrap(),
        },
        Some(at(0)),
        60,
        LaunchRetryPolicy {
            limit: 0,
            ..Default::default()
        },
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert!(
        !unknown.nodes[0].attempts[0]
            .extra
            .contains_key(PRELAUNCH_REFUSAL_PROOF)
    );
    let before = unknown.clone();
    assert!(retry(&document, &mut unknown, None, &|_| Ok(()), None).is_err());
    assert_eq!(unknown, before);
    let prior = checkpoint.nodes[0].attempts[0].clone();
    assert_eq!(prior.state, AttemptState::Starting);
    assert!(!prior.launched);
    assert!(prior.attempt_ref.is_none());
    let sibling = checkpoint.nodes[1].clone();
    for unsafe_case in 0..6 {
        let mut candidate = checkpoint.clone();
        match unsafe_case {
            0 => candidate.nodes[0].attempts[0].launched = true,
            1 => candidate.nodes[0].attempts[0].attempt_ref = Some("host/uncertain".into()),
            2 => candidate.nodes[0].advance_error_code = Some("launch_retry_exhausted".into()),
            3 => candidate.nodes[0].state = NodeState::Running,
            4 => candidate.nodes[0].attempts[0].state = AttemptState::Running,
            _ => {
                candidate.nodes[0].attempts[0]
                    .extra
                    .remove(PRELAUNCH_REFUSAL_PROOF);
            }
        }
        let before = candidate.clone();
        assert!(retry(&document, &mut candidate, None, &|_| Ok(()), None).is_err());
        assert_eq!(candidate, before);
    }
    retry(&document, &mut checkpoint, None, &|_| Ok(()), None).unwrap();
    assert!(!checkpoint.cancelled);
    assert_eq!(checkpoint.nodes[0].state, NodeState::Pending);
    assert_eq!(checkpoint.nodes[0].previous_attempts, vec![prior]);
    assert_eq!(checkpoint.nodes[1], sibling);
    let mut owner = FakeOwner {
        observations: [done_as("retried-work"), done_as("pending-sibling")].into(),
        ..Default::default()
    };
    let completed = advance(&document, &mut owner, &mut MemoryJournal(Some(checkpoint))).unwrap();
    assert_eq!(completed.status, GraphStatus::Complete);
    assert!(
        completed
            .checkpoint
            .nodes
            .iter()
            .all(|node| node.state == NodeState::Succeeded)
    );
    assert_eq!(owner.calls.len(), 2);
}
