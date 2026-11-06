use super::*;
use serde_json::json;

fn retry(
    document: &GraphDocument,
    checkpoint: &mut GraphCheckpoint,
    brief: Option<&str>,
    scan: &dyn Fn(&str) -> Result<(), String>,
    journal: Option<&patch::PatchJournal<'_>>,
) -> Result<patch::Admitted, patch::PatchError> {
    retry_id(document, checkpoint, "work", brief, scan, journal)
}

fn retry_id(
    document: &GraphDocument,
    checkpoint: &mut GraphCheckpoint,
    id: &str,
    brief: Option<&str>,
    scan: &dyn Fn(&str) -> Result<(), String>,
    journal: Option<&patch::PatchJournal<'_>>,
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

/// Persist a real checkpoint, then interrupt at the boundary before launching
/// the selected attempt. Recovery must use the file, not the caller's memory.
struct CrashAfterCheckpoint {
    journal: FileJournal,
    before_launch: &'static str,
}

impl GraphJournal for CrashAfterCheckpoint {
    type Error = JournalError;

    fn load(&mut self, graph_id: &str) -> Result<Option<GraphCheckpoint>, Self::Error> {
        self.journal.load(graph_id)
    }

    fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error> {
        self.journal.save(checkpoint)?;
        if checkpoint.nodes.iter().any(|node| {
            node.id == self.before_launch
                && node.attempts.iter().any(|attempt| {
                    attempt.state == AttemptState::Starting && attempt.attempt_ref.is_none()
                })
        }) {
            panic!("injected crash after checkpoint and before launch");
        }
        Ok(())
    }
}

fn redo_container_after_checkpoint_crash(
    document: GraphDocument,
    container: &str,
    before_launch: &'static str,
    initial: Vec<AttemptObservation>,
    before_crash: Vec<AttemptObservation>,
    resumed: Vec<AttemptObservation>,
    expected_calls: &[&str],
) -> GraphCheckpoint {
    use provenant_os::{clock::SystemClock, fs::SystemPathResolver};
    use std::os::unix::fs::PermissionsExt;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    let temp = crate::test_support::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let base = std::fs::canonicalize(temp.path()).unwrap();
    let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("checkout"));
    inputs.state_dir = Some(base.clone());
    let root = StateRoot::resolve(&inputs, &SystemPathResolver).unwrap();
    let project = ProjectId::from_bytes([37; 16]);
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
    let open =
        || FileJournal::open(&root, project, &document.graph_id, budget, &cancel, &clock).unwrap();
    let mut journal = open();
    journal
        .bind(GraphExecution {
            review_base: None,
            extra: Default::default(),
            integration_base: None,
            directory: base,
            document: document.clone(),
        })
        .unwrap();
    let mut initial_owner = FakeOwner {
        observations: initial.into(),
        ..Default::default()
    };
    let mut checkpoint = advance(&document, &mut initial_owner, &mut journal)
        .unwrap()
        .checkpoint;
    assert!(checkpoint.successful());
    let original = checkpoint.clone();
    let sibling = original
        .nodes
        .iter()
        .find(|node| node.id == "sibling")
        .unwrap();
    retry_id(
        &document,
        &mut checkpoint,
        container,
        None,
        &|_| Ok(()),
        None,
    )
    .unwrap();
    assert_eq!(
        checkpoint
            .nodes
            .iter()
            .find(|node| node.id == container)
            .unwrap()
            .state,
        NodeState::Pending
    );
    journal.save(&checkpoint).unwrap();
    let mut crashing = CrashAfterCheckpoint {
        journal,
        before_launch,
    };
    let mut owner = FakeOwner {
        observations: before_crash.into(),
        ..Default::default()
    };
    assert!(
        catch_unwind(AssertUnwindSafe(|| advance(
            &document,
            &mut owner,
            &mut crashing
        )))
        .is_err()
    );
    assert!(
        owner
            .calls
            .iter()
            .all(|(key, _)| key.node_id != before_launch)
    );
    drop(crashing);
    // Reopen from disk after discarding every in-memory checkpoint.
    let mut journal = open();
    let saved = journal.load(&document.graph_id).unwrap().unwrap();
    let fenced = saved
        .nodes
        .iter()
        .find(|node| node.id == before_launch)
        .unwrap()
        .attempts
        .last()
        .unwrap()
        .key
        .clone();
    let old = original
        .nodes
        .iter()
        .find(|node| node.id == before_launch)
        .unwrap();
    assert!(old.attempts.iter().all(|attempt| attempt.key != fenced));
    let mut resumed_owner = FakeOwner {
        observations: resumed.into(),
        ..Default::default()
    };
    let run = advance(&document, &mut resumed_owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(run.checkpoint.successful());
    assert_eq!(
        resumed_owner
            .calls
            .iter()
            .map(|(key, _)| key.node_id.as_str())
            .collect::<Vec<_>>(),
        expected_calls
    );
    assert_eq!(resumed_owner.calls[0].0, fenced);
    let preserved = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "sibling")
        .unwrap();
    assert_eq!(preserved.attempts, sibling.attempts);
    assert_eq!(preserved.outputs, sibling.outputs);
    for prior in original
        .nodes
        .iter()
        .filter(|node| node.id.starts_with(&format!("{container}--")))
    {
        let redone = run
            .checkpoint
            .nodes
            .iter()
            .find(|node| node.id == prior.id)
            .unwrap();
        for attempt in &prior.attempts {
            assert!(
                redone.previous_attempts.contains(attempt),
                "history lost for {}",
                prior.id
            );
            assert!(
                redone
                    .attempts
                    .iter()
                    .all(|current| current.key != attempt.key)
            );
        }
    }
    let mut replay_owner = FakeOwner::default();
    assert_eq!(
        advance(&document, &mut replay_owner, &mut journal)
            .unwrap()
            .checkpoint,
        run.checkpoint
    );
    assert!(replay_owner.calls.is_empty());
    run.checkpoint
}

#[test]
fn composite_redo_recovers_nested_subtree_after_checkpoint_before_launch_crash() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1,"graph_id":"composite-redo-crash","nodes":[{
            "id":"work","kind":"composite","access":"read_only","composite":{
                "nodes":[
                    {"id":"first","prompt":"First task","access":"read_only","route":{"alias":"junior"}},
                    {"id":"nested","kind":"composite","access":"read_only","depends_on":["first"],"composite":{
                        "nodes":[{"id":"leaf","prompt":"Nested task","access":"read_only","route":{"alias":"junior"}}],
                        "exports":[{"node":"leaf","outputs":["result"]}]
                    }}
                ],"exports":[{"node":"first","outputs":["result"]},{"node":"nested","outputs":["result"]}]
            }
        },{"id":"sibling","prompt":"Other work","access":"read_only","depends_on":["work"],"route":{"alias":"junior"}}]
    })).unwrap();
    let checkpoint = redo_container_after_checkpoint_crash(
        document,
        "work",
        "work--nested--leaf",
        vec![
            terminal_from("old first", "first-old"),
            terminal_from("old nested", "nested-old"),
            done_as("sibling-old"),
        ],
        vec![terminal_from("new first", "first-new")],
        vec![terminal_from("new nested", "nested-new")],
        &["work--nested--leaf"],
    );
    let result = checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "work")
        .unwrap()
        .outputs
        .as_ref()
        .unwrap()
        .result
        .as_ref()
        .unwrap();
    assert!(result.excerpt.contains("new first") && result.excerpt.contains("new nested"));
    assert!(!result.excerpt.contains("old first") && !result.excerpt.contains("old nested"));
}

#[test]
fn loop_redo_restarts_rounds_after_checkpoint_before_launch_crash() {
    let mut document: GraphDocument =
        serde_json::from_str(include_str!("../fixtures/loop-pass-round-two.json")).unwrap();
    let mut sibling = node("sibling", route("first"));
    sibling.depends_on = vec!["revise".into()];
    document.nodes.push(sibling);
    let checkpoint = redo_container_after_checkpoint_crash(
        document,
        "revise",
        "revise--r1--review",
        vec![
            terminal_from("VERDICT: FAIL", "review-old-1"),
            terminal_from("VERDICT: PASS", "review-old-2"),
            done_as("sibling-old"),
        ],
        vec![],
        vec![terminal_from("VERDICT: PASS", "review-new-1")],
        &["revise--r1--review"],
    );
    assert_eq!(
        checkpoint
            .nodes
            .iter()
            .find(|node| node.id == "revise--r1--approved")
            .unwrap()
            .state,
        NodeState::Succeeded
    );
    for id in ["revise--r2--review", "revise--r3--review"] {
        let skipped = checkpoint.nodes.iter().find(|node| node.id == id).unwrap();
        assert_eq!(skipped.state, NodeState::Skipped);
        assert!(
            skipped.attempts.is_empty(),
            "inactive round was relaunched: {id}"
        );
    }
}

fn planner_container(graph_id: &str) -> GraphDocument {
    serde_json::from_value(json!({
        "schema_version":1,"graph_id":graph_id,"nodes":[{
            "id":"work","kind":"composite","access":"read_only","composite":{
                "nodes":[{"id":"planner","kind":"plan","access":"read_only","prompt":"Plan work",
                    "route":{"alias":"senior"},"plan":{"kinds":["task"],"max_nodes":4}}],
                "exports":[{"node":"planner","outputs":["result"]}]
            }
        },{"id":"sibling","prompt":"Other work","access":"read_only","depends_on":["work"],"route":{"alias":"junior"}}]
    }))
    .unwrap()
}

fn slice_fragment(id: &str) -> String {
    format!(
        "```fragment\n{}\n```",
        json!({
            "schema_version":1,"nodes":[{"id":id,"goal":"Do work","acceptance_checks":["Verify work"],"access":"read_only"}]
        })
    )
}

#[test]
fn dynamic_composite_redo_retires_its_expansion_and_recovers_after_checkpoint_crash() {
    let checkpoint = redo_container_after_checkpoint_crash(
        planner_container("redo-dynamic-crash"),
        "work",
        "work--planner--slice",
        vec![
            terminal_from(&slice_fragment("slice"), "planner-old"),
            terminal_from("old slice", "slice-old"),
            done_as("sibling-old"),
        ],
        vec![terminal_from(&slice_fragment("slice"), "planner-new")],
        vec![terminal_from("new slice", "slice-new")],
        &["work--planner--slice"],
    );
    let live: Vec<_> = checkpoint
        .expansions
        .iter()
        .filter(|expansion| !expansion.nodes.is_empty())
        .collect();
    assert_eq!(live.len(), 1, "the old generation must not stay live");
    assert_eq!(live[0].source, "work--planner");
    assert_eq!(
        checkpoint
            .expansions
            .iter()
            .filter(|expansion| expansion.retired())
            .count(),
        1
    );
    let slices: Vec<_> = checkpoint
        .nodes
        .iter()
        .filter(|node| node.id == "work--planner--slice")
        .collect();
    assert_eq!(slices.len(), 1);
    assert_eq!(slices[0].attempts.len(), 1);
    assert_eq!(
        slices[0].attempts[0].attempt_ref.as_deref(),
        Some("slice-new")
    );
    assert!(
        slices[0]
            .previous_attempts
            .iter()
            .any(|attempt| attempt.attempt_ref.as_deref() == Some("slice-old"))
    );
}

#[test]
fn dynamic_composite_redo_drops_stale_failed_dependencies_and_launches_once() {
    let document = planner_container("redo-dynamic-failed");
    let mut owner = FakeOwner {
        observations: [
            terminal_from(&slice_fragment("broken"), "planner-old"),
            terminal_with_result(AttemptOutcome::TaskFailed, "broken"),
        ]
        .into(),
        ..Default::default()
    };
    let mut checkpoint = advance(&document, &mut owner, &mut MemoryJournal::default())
        .unwrap()
        .checkpoint;
    let state = |checkpoint: &GraphCheckpoint, id: &str| {
        checkpoint
            .nodes
            .iter()
            .find(|node| node.id == id)
            .map(|node| node.state)
    };
    assert_eq!(
        state(&checkpoint, "work--planner--broken"),
        Some(NodeState::Failed)
    );
    assert!(!checkpoint.successful());
    let revision = patch::revision(&checkpoint);
    retry_id(&document, &mut checkpoint, "work", None, &|_| Ok(()), None).unwrap();
    assert_eq!(patch::revision(&checkpoint), revision + 1);
    assert_eq!(state(&checkpoint, "work--planner--broken"), None);
    let mut owner = FakeOwner {
        observations: [
            terminal_from(&slice_fragment("fixed"), "planner-new"),
            done_as("fixed"),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal(Some(checkpoint))).unwrap();
    assert_eq!(state(&run.checkpoint, "work"), Some(NodeState::Succeeded));
    assert_eq!(state(&run.checkpoint, "work--planner--broken"), None);
    assert_eq!(
        owner
            .calls
            .iter()
            .map(|(key, _)| key.node_id.as_str())
            .collect::<Vec<_>>(),
        ["work--planner", "work--planner--fixed"]
    );
    // No live node still waits on the retired, failed slice.
    let plan = plans::expand(
        &document,
        &run.checkpoint.extensions,
        &run.checkpoint.expansions,
        &plans::generous(),
    )
    .unwrap();
    assert!(plan.nodes.iter().all(|node| {
        node.id != "work--planner--broken"
            && node.dependencies().all(|parent| parent != "work--planner--broken")
            && !node.after_expansion.iter().any(|parent| parent == "work--planner--broken")
            && !node.composite_members.iter().any(|member| member == "work--planner--broken")
    }));
    // A settled consumer keeps what it observed until it is redone itself.
    let mut checkpoint = run.checkpoint;
    retry_id(&document, &mut checkpoint, "sibling", None, &|_| Ok(()), None).unwrap();
    let mut owner = FakeOwner {
        observations: [done_as("sibling")].into(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal(Some(checkpoint))).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(run.checkpoint.successful());
    assert_eq!(owner.calls.len(), 1);
    assert_eq!(owner.calls[0].0.node_id, "sibling");
    // The redone container can be redone again, and the reloaded journal
    // replays without relaunching anything.
    let reloaded: GraphCheckpoint =
        serde_json::from_value(serde_json::to_value(&run.checkpoint).unwrap()).unwrap();
    let mut replay = FakeOwner::default();
    assert_eq!(
        advance(&document, &mut replay, &mut MemoryJournal(Some(reloaded.clone())))
            .unwrap()
            .checkpoint,
        reloaded
    );
    assert!(replay.calls.is_empty());
    let mut again = reloaded;
    retry_id(&document, &mut again, "work", None, &|_| Ok(()), None).unwrap();
    assert_eq!(
        again
            .expansions
            .iter()
            .filter(|expansion| expansion.retired())
            .count(),
        2
    );
}

#[test]
fn container_redo_accepts_only_host_proven_prelaunch_refusal_of_a_descendant() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1,"graph_id":"redo-refused-container","nodes":[{
            "id":"work","kind":"composite","access":"read_only","composite":{
                "nodes":[{"id":"leaf","prompt":"Do work","access":"read_only","route":{"alias":"senior"}}],
                "exports":[{"node":"leaf","outputs":["result"]}]
            }
        }]
    })).unwrap();
    let mut owner = FakeOwner {
        observations: [AttemptObservation::Pending { attempt_ref: None }].into(),
        ..Default::default()
    };
    let mut checkpoint = advance(&document, &mut owner, &mut MemoryJournal::default())
        .unwrap()
        .checkpoint;
    for progress in &mut checkpoint.nodes {
        progress.state = NodeState::Blocked;
        progress.outputs = None;
    }
    let leaf = checkpoint
        .nodes
        .iter_mut()
        .find(|node| node.id == "work--leaf")
        .unwrap();
    leaf.advance_error_code = Some("launch_refused".into());
    let attempt = &mut leaf.attempts[0];
    attempt.state = AttemptState::Starting;
    attempt.launched = false;
    attempt.attempt_ref = None;
    attempt
        .extra
        .insert(PRELAUNCH_REFUSAL_PROOF.into(), json!(true));
    let prior = attempt.clone();
    for unsafe_case in 0..4 {
        let mut candidate = checkpoint.clone();
        let leaf = candidate
            .nodes
            .iter_mut()
            .find(|node| node.id == "work--leaf")
            .unwrap();
        match unsafe_case {
            0 => {
                leaf.attempts[0].extra.remove(PRELAUNCH_REFUSAL_PROOF);
            }
            1 => leaf.attempts[0].launched = true,
            2 => leaf.attempts[0].attempt_ref = Some("host/uncertain".into()),
            _ => leaf.advance_error_code = Some("launch_retry_exhausted".into()),
        }
        let before = candidate.clone();
        assert!(retry_id(&document, &mut candidate, "work", None, &|_| Ok(()), None).is_err());
        assert_eq!(candidate, before);
    }
    retry_id(&document, &mut checkpoint, "work", None, &|_| Ok(()), None).unwrap();
    let leaf = checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "work--leaf")
        .unwrap();
    assert_eq!(leaf.state, NodeState::Pending);
    assert_eq!(leaf.previous_attempts, vec![prior]);
    assert!(leaf.attempts.is_empty());
    let mut owner = FakeOwner {
        observations: [done_as("leaf-new")].into(),
        ..Default::default()
    };
    let completed = advance(&document, &mut owner, &mut MemoryJournal(Some(checkpoint))).unwrap();
    assert_eq!(completed.status, GraphStatus::Complete);
    assert!(completed.checkpoint.successful());
    assert_eq!(owner.calls.len(), 1);
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
        LaunchTiming { clock: Some(at(0)), admission_seconds: 60, policy: LaunchRetryPolicy::default() },
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
            LaunchTiming { clock: Some(at(0)), admission_seconds: 60, policy: LaunchRetryPolicy::default() },
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
        LaunchTiming { clock: Some(at(0)), admission_seconds: 60, policy: LaunchRetryPolicy::default() },
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
        LaunchTiming { clock: Some(at(0)), admission_seconds: 60, policy: LaunchRetryPolicy {
            limit: 0,
            ..Default::default()
        } },
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
