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
                "nodes":[{"id":"planner","kind":"task","access":"read_only","prompt":"Plan work",
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
            && node
                .dependencies()
                .all(|parent| parent != "work--planner--broken")
            && !node
                .after_expansion
                .iter()
                .any(|parent| parent == "work--planner--broken")
            && !node
                .composite_members
                .iter()
                .any(|member| member == "work--planner--broken")
    }));
    // A settled consumer keeps what it observed until it is redone itself.
    let mut checkpoint = run.checkpoint;
    retry_id(
        &document,
        &mut checkpoint,
        "sibling",
        None,
        &|_| Ok(()),
        None,
    )
    .unwrap();
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
        advance(
            &document,
            &mut replay,
            &mut MemoryJournal(Some(reloaded.clone()))
        )
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
    // Dispatch sets this flag before the host can refuse acceptance.
    attempt.launched = true;
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
            1 => leaf.attempts[0].state = AttemptState::Running,
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
            "id":"work","kind":"task","access":"read_only","prompt":"Plan work",
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
            "id":"work","kind":"task","access":"read_only","prompt":"Plan work",
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
            run.checkpoint.pause_reason()
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
        LaunchTiming {
            clock: Some(at(0)),
            admission_seconds: 60,
            policy: LaunchRetryPolicy::default(),
        },
        &mut MemoryJournal::default(),
    )
    .unwrap();
    let mut checkpoint: GraphCheckpoint =
        serde_json::from_value(serde_json::to_value(checkpoint).unwrap()).unwrap();
    assert_eq!(checkpoint.nodes[0].advance_error_code.as_deref(), Some("launch_refused"));
    assert_eq!(checkpoint.nodes[0].advance_owner_code.as_deref(), Some("invalid_input"));
    let wait = graph_waits(&checkpoint).into_iter().find(|wait| wait.node == "work").unwrap();
    assert_eq!(wait.code, "invalid_input");
    assert_eq!(wait.cause.as_deref(), Some("invalid_input"));
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
            LaunchTiming {
                clock: Some(at(0)),
                admission_seconds: 60,
                policy: LaunchRetryPolicy::default(),
            },
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
        LaunchTiming {
            clock: Some(at(0)),
            admission_seconds: 60,
            policy: LaunchRetryPolicy::default(),
        },
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
        LaunchTiming {
            clock: Some(at(0)),
            admission_seconds: 60,
            policy: LaunchRetryPolicy {
                limit: 0,
                ..Default::default()
            },
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
    // The launch flag is set before dispatch; explicit host proof overrides it.
    for unsafe_case in 1..6 {
        let mut candidate = checkpoint.clone();
        match unsafe_case {
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
    assert_eq!(checkpoint.nodes[0].advance_owner_code, None);
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

#[test]
fn retry_node_and_dependants_resets_consumers_and_preserves_unrelated_work() {
    let mut consumer = node("consumer", route("first"));
    consumer.depends_on = vec!["work".into()];
    let mut last = node("last", route("first"));
    last.depends_on = vec!["consumer".into()];
    let document = graph(vec![
        node("work", route("first")),
        consumer,
        last,
        node("unrelated", route("first")),
    ]);
    let mut owner = FakeOwner {
        observations: [
            done_as("old-work"),
            done_as("old-consumer"),
            done_as("old-last"),
            done_as("unrelated"),
        ]
        .into(),
        ..Default::default()
    };
    use provenant_os::{clock::SystemClock, fs::SystemPathResolver};
    use std::os::unix::fs::PermissionsExt;
    let temp = crate::test_support::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let base = std::fs::canonicalize(temp.path()).unwrap();
    let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("checkout"));
    inputs.state_dir = Some(base.clone());
    let root = StateRoot::resolve(&inputs, &SystemPathResolver).unwrap();
    let project = ProjectId::from_bytes([39; 16]);
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
    let mut checkpoint = advance(&document, &mut owner, &mut journal)
        .unwrap()
        .checkpoint;
    let original = checkpoint.clone();
    let event_count = FileJournal::read_events(&root, project, &document.graph_id)
        .unwrap()
        .len();
    let library = template::TemplateLibrary::bundled();
    let resolvers = Default::default();
    let limits = plans::generous();
    let context = patch::PatchContext {
        library: &library,
        resolvers: &resolvers,
        scan: &|_| Ok(()),
        journal: None,
        limits: &limits,
    };
    let mut live = original.clone();
    live.nodes
        .iter_mut()
        .find(|node| node.id == "last")
        .unwrap()
        .attempts[0]
        .state = AttemptState::Running;
    let before = live.clone();
    assert!(
        patch::retry_node_and_dependants(
            &mut patch::LiveGraph {
                document: &document,
                checkpoint: &mut live
            },
            "work",
            None,
            &context
        )
        .is_err()
    );
    assert_eq!(live, before);
    let admitted = patch::retry_node_and_dependants(
        &mut patch::LiveGraph {
            document: &document,
            checkpoint: &mut checkpoint,
        },
        "work",
        Some("Repaired work"),
        &context,
    )
    .unwrap();
    assert_eq!(admitted.revision, 1);
    assert_eq!(
        checkpoint
            .nodes
            .iter()
            .filter(|node| node.state == NodeState::Pending)
            .count(),
        3
    );
    let mut owner = FakeOwner {
        observations: [
            done_as("new-work"),
            done_as("new-consumer"),
            done_as("new-last"),
        ]
        .into(),
        ..Default::default()
    };
    journal.save(&checkpoint).unwrap();
    drop(journal);
    let mut journal = open();
    let events = FileJournal::read_events(&root, project, &document.graph_id).unwrap();
    let retry_events = &events[event_count..];
    assert_eq!(
        retry_events
            .iter()
            .filter(|event| event.kind == GraphEventKind::Patch)
            .count(),
        1
    );
    assert_eq!(
        retry_events
            .iter()
            .filter(|event| event.kind == GraphEventKind::NodeState && event.code == "pending")
            .count(),
        3
    );
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert!(run.checkpoint.successful());
    assert_eq!(
        owner
            .calls
            .iter()
            .map(|(key, _)| key.node_id.as_str())
            .collect::<Vec<_>>(),
        ["work", "consumer", "last"]
    );
    assert_eq!(
        run.checkpoint
            .nodes
            .iter()
            .find(|node| node.id == "unrelated"),
        original.nodes.iter().find(|node| node.id == "unrelated")
    );
    for id in ["work", "consumer", "last"] {
        let old = original.nodes.iter().find(|node| node.id == id).unwrap();
        let new = run
            .checkpoint
            .nodes
            .iter()
            .find(|node| node.id == id)
            .unwrap();
        assert_eq!(new.previous_attempts, old.attempts);
        assert_ne!(new.attempts[0].key, old.attempts[0].key);
    }
    let mut replay = FakeOwner::default();
    advance(&document, &mut replay, &mut journal).unwrap();
    assert!(replay.calls.is_empty());
}

#[test]
fn planner_patched_loop_body_redo_recovers_after_checkpoint_crash() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1,"graph_id":"redo-dynamic-loop-crash","nodes":[
            {"id":"work","kind":"loop","access":"read_only","loop":{
                "max_rounds":1,"until":"check","body":[
                    {"id":"planner","kind":"task","prompt":"Plan work","access":"read_only","route":{"alias":"senior"},"plan":{"kinds":["task"],"max_nodes":1}},
                    {"id":"check","kind":"gate","access":"read_only","depends_on":["planner"],"condition":{"type":"budget"}}
                ]
            }},
            {"id":"sibling","prompt":"Independent work","access":"read_only","route":{"alias":"junior"}}
        ]
    })).unwrap();
    let checkpoint = redo_container_after_checkpoint_crash(
        document,
        "work",
        "work--r1--planner--slice",
        vec![
            terminal_from(&slice_fragment("slice"), "planner-old"),
            done_as("sibling-old"),
            terminal_from("old slice", "slice-old"),
        ],
        vec![terminal_from(&slice_fragment("slice"), "planner-new")],
        vec![terminal_from("new slice", "slice-new")],
        &["work--r1--planner--slice"],
    );
    assert_eq!(
        checkpoint
            .expansions
            .iter()
            .filter(|expansion| expansion.retired())
            .count(),
        1
    );
    let slice = checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "work--r1--planner--slice")
        .unwrap();
    assert!(slice.round.is_some());
    assert_eq!(slice.attempts[0].attempt_ref.as_deref(), Some("slice-new"));
    assert!(
        slice
            .previous_attempts
            .iter()
            .any(|attempt| attempt.attempt_ref.as_deref() == Some("slice-old"))
    );
}

#[test]
fn public_agent_retry_replaces_dynamic_children_instead_of_relaunching_stale_work() {
    for replacement in ["slice", "replacement"] {
        let document: GraphDocument = serde_json::from_value(json!({
            "schema_version":1,"graph_id":"retry-planner-consumers","nodes":[{
                "id":"work","kind":"task","access":"read_only","prompt":"Plan work",
                "route":{"alias":"senior"}
            }]
        }))
        .unwrap();
        let mut owner = FakeOwner {
            observations: [
                terminal_from(&slice_fragment("slice").replace("```fragment", "```patch"), "old-plan"),
                done_as("old-child"),
            ]
            .into(),
            ..Default::default()
        };
        let mut checkpoint = advance(&document, &mut owner, &mut MemoryJournal::default())
            .unwrap()
            .checkpoint;
        let mut busy = checkpoint.clone();
        let child = busy.nodes.iter_mut().find(|node| node.id == "work--slice").unwrap();
        child.state = NodeState::Running;
        child.attempts[0].state = AttemptState::Running;
        let before = busy.clone();
        assert!(patch::retry_node_and_dependants(
            &mut patch::LiveGraph {
                document: &document,
                checkpoint: &mut busy,
            },
            "work",
            None,
            &patch::PatchContext {
                library: &template::TemplateLibrary::bundled(),
                resolvers: &Default::default(),
                scan: &|_| Ok(()),
                journal: None,
                limits: &plans::generous(),
            },
        ).is_err());
        assert_eq!(busy, before, "a live dynamic child must retain its custody");
        patch::retry_node_and_dependants(
            &mut patch::LiveGraph {
                document: &document,
                checkpoint: &mut checkpoint,
            },
            "work",
            None,
            &patch::PatchContext {
                library: &template::TemplateLibrary::bundled(),
                resolvers: &Default::default(),
                scan: &|_| Ok(()),
                journal: None,
                limits: &plans::generous(),
            },
        )
        .unwrap();
        assert!(
            checkpoint
                .expansions
                .iter()
                .any(|expansion| expansion.retired())
        );
        let mut owner = FakeOwner {
            observations: [
                terminal_from(&slice_fragment(replacement).replace("```fragment", "```patch"), "new-plan"),
                done_as("new-child"),
            ]
            .into(),
            ..Default::default()
        };
        let mut journal = MemoryJournal(Some(checkpoint));
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        assert!(
            run.checkpoint.successful(),
            "{:?}",
            run.checkpoint.pause_reason()
        );
        assert_eq!(
            owner
                .calls
                .iter()
                .map(|(key, _)| key.node_id.as_str())
                .collect::<Vec<_>>(),
            ["work", &format!("work--{replacement}")]
        );
        let child = run
            .checkpoint
            .nodes
            .iter()
            .find(|node| node.id == format!("work--{replacement}"))
            .unwrap();
        assert_eq!(child.attempts[0].attempt_ref.as_deref(), Some("new-child"));
        if replacement == "slice" {
            assert!(
                child
                    .previous_attempts
                    .iter()
                    .any(|attempt| attempt.attempt_ref.as_deref() == Some("old-child"))
            );
        } else {
            assert!(
                !run.checkpoint
                    .nodes
                    .iter()
                    .any(|node| node.id == "work--slice")
            );
        }
        let mut replay = FakeOwner::default();
        advance(&document, &mut replay, &mut journal).unwrap();
        assert!(replay.calls.is_empty());
    }
}

#[test]
fn public_retry_of_dynamic_member_recomputes_enclosing_join_without_retiring_the_member() {
    let document = planner_container("retry-dynamic-member");
    let mut owner = FakeOwner {
        observations: [
            terminal_from(&slice_fragment("slice"), "old-plan"),
            done_as("old-slice"),
            done_as("old-consumer"),
        ]
        .into(),
        ..Default::default()
    };
    let mut checkpoint = advance(&document, &mut owner, &mut MemoryJournal::default())
        .unwrap()
        .checkpoint;
    patch::retry_node_and_dependants(
        &mut patch::LiveGraph {
            document: &document,
            checkpoint: &mut checkpoint,
        },
        "work--planner--slice",
        None,
        &patch::PatchContext {
            library: &template::TemplateLibrary::bundled(),
            resolvers: &Default::default(),
            scan: &|_| Ok(()),
            journal: None,
            limits: &plans::generous(),
        },
    )
    .unwrap();
    let mut owner = FakeOwner {
        observations: [done_as("new-slice"), done_as("new-consumer")].into(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal(Some(checkpoint))).unwrap();
    assert!(
        run.checkpoint.successful(),
        "{:?}",
        run.checkpoint.pause_reason()
    );
    assert_eq!(
        owner
            .calls
            .iter()
            .map(|(key, _)| key.node_id.as_str())
            .collect::<Vec<_>>(),
        ["work--planner--slice", "sibling"]
    );
    let planner = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "work--planner")
        .unwrap();
    assert_eq!(planner.attempts[0].attempt_ref.as_deref(), Some("old-plan"));
}

#[test]
fn public_retry_preserves_live_spawn_only_children() {
    let (document, mut checkpoint, _) = settled();
    let library = template::TemplateLibrary::bundled();
    let resolvers = Default::default();
    let limits = plans::generous();
    let context = patch::PatchContext {
        library: &library,
        resolvers: &resolvers,
        scan: &|_| Ok(()),
        journal: None,
        limits: &limits,
    };
    patch::admit_patch(
        &mut patch::LiveGraph {
            document: &document,
            checkpoint: &mut checkpoint,
        },
        Some("work"),
        &patch::Patch {
            schema_version: patch::PATCH_SCHEMA_VERSION,
            nodes: vec![json!({"id":"spawned", "use":"task", "brief":"Independent child"})],
            ..Default::default()
        },
        &context,
    )
    .unwrap();
    let mut owner = FakeOwner {
        observations: [AttemptObservation::Pending {
            attempt_ref: Some("live-child".into()),
        }]
        .into(),
        ..Default::default()
    };
    let mut checkpoint = advance(&document, &mut owner, &mut MemoryJournal(Some(checkpoint)))
        .unwrap()
        .checkpoint;
    let child = checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "work--spawned")
        .unwrap()
        .clone();
    assert_eq!(child.state, NodeState::Running);
    patch::retry_node_and_dependants(
        &mut patch::LiveGraph {
            document: &document,
            checkpoint: &mut checkpoint,
        },
        "work",
        None,
        &context,
    )
    .unwrap();
    assert_eq!(
        checkpoint.nodes.iter().find(|node| node.id == child.id),
        Some(&child)
    );
    assert!(
        checkpoint
            .expansions
            .iter()
            .all(|expansion| !expansion.retired())
    );
}


#[test]
fn no_work_writer_continues_in_place_and_records_the_patch() {
    for final_outcome in [AttemptOutcome::StoppedNoWork, AttemptOutcome::Succeeded] {
        let mut work = node("work", pool_route("junior"));
        work.access = AccessMode::WorktreeWrite;
        work.worktree = Some(".worktrees/work".into());
        work.write_scope = vec!["src".into()];
        work.prompt = "Finish the accepted brief".into();
        let document = graph(vec![work]);
        // A prior writer or explicit retry may already have advanced the
        // branch beyond this graph's admission binding at A. Settlement proves
        // that this attempt started and stopped at B.
        let stopped = || no_work_stop(Some(&"b".repeat(40)));
        let second = if final_outcome == AttemptOutcome::Succeeded {
            done_as("finished")
        } else {
            stopped()
        };
        // A writer that stops again restarts until the graph-wide budget of
        // two restarts is spent, then pauses for the chair.
        let again = final_outcome != AttemptOutcome::Succeeded;
        let mut observations = vec![stopped(), second];
        if again {
            observations.push(stopped());
        }
        let calls = observations.len();
        let mut owner = FakeOwner {
            observations: observations.into(),
            ..Default::default()
        };
        let mut journal = MemoryJournal::default();
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        assert_eq!(owner.calls.len(), calls);
        assert_ne!(owner.calls[0].0, owner.calls[1].0);
        assert_eq!(owner.permissions[0], owner.permissions[1]);
        assert!(owner.prompts[1].contains("Finish the accepted brief"));
        let progress = &run.checkpoint.nodes[0];
        let expected = if final_outcome == AttemptOutcome::Succeeded {
            NodeState::Succeeded
        } else {
            NodeState::Failed
        };
        assert_eq!(progress.state, expected);
        assert_eq!(progress.previous_attempts.len(), calls - 1);
        assert_eq!(
            progress.previous_attempts[0].outcome,
            Some(AttemptOutcome::StoppedNoWork)
        );
        let records: Vec<_> = run
            .checkpoint
            .expansions
            .iter()
            .filter_map(|expansion| expansion.patch.as_ref())
            .collect();
        assert_eq!(records.len(), calls - 1);
        assert_eq!(records[0].replaced[0].id, "work");
        assert_eq!(records[0].replaced[0].write_scope, ["src"]);
        assert_eq!(
            records[0].restart.as_ref().map(|restart| restart.cause),
            Some(provenant_records::StopCause::StoppedEarly)
        );
        let reloaded = advance(&document, &mut owner, &mut journal).unwrap();
        assert_eq!(reloaded.checkpoint.nodes[0].state, expected);
        assert_eq!(owner.calls.len(), calls);
    }
}

fn no_work_stop(head: Option<&str>) -> AttemptObservation {
    AttemptObservation::Terminal {
        attempt_ref: Some("stopped".into()),
        outcome: AttemptOutcome::StoppedNoWork,
        launched: true,
        result: None,
        result_warning: None,
        result_path: None,
        branch: None,
        commit: head.map(str::to_owned),
        stop: None,
    }
}

#[test]
fn writer_stop_restarts_only_for_enabled_writers_and_leaves_others_failed() {
    // (access, Some(false) sends a stopped_early stop to the chair, head, restarts)
    for (access, enabled, head, restarts) in [
        (AccessMode::WorktreeWrite, Some(false), Some("a".repeat(40)), false),
        (AccessMode::ReadOnly, None, Some("a".repeat(40)), false),
        // A committed stop (moved or missing head) restarts in place too.
        (AccessMode::WorktreeWrite, None, Some("b".repeat(40)), true),
        (AccessMode::WorktreeWrite, None, None, true),
    ] {
        let mut work = node("work", pool_route("junior"));
        work.access = access;
        work.worktree = (access == AccessMode::WorktreeWrite).then(|| ".worktrees/work".into());
        if enabled == Some(false) {
            work.on_stop = provenant_records::OnStop::new(
                [(provenant_records::StopCause::StoppedEarly, provenant_records::NextStep::Chair)].into(),
            );
        }
        let document = graph(vec![work]);
        let mut stopped = no_work_stop(head.as_deref());
        if head.as_deref() != Some("a".repeat(40).as_str())
            && let AttemptObservation::Terminal { outcome, .. } = &mut stopped
        {
            *outcome = AttemptOutcome::DeclaredStop;
        }
        let mut observations = vec![stopped];
        if restarts {
            observations.push(done_as("finished"));
        }
        let mut owner = FakeOwner {
            observations: observations.into(),
            ..Default::default()
        };
        let mut journal = MemoryJournal::default();
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        if restarts {
            assert_eq!(run.checkpoint.nodes[0].state, NodeState::Succeeded);
            assert_eq!(owner.calls.len(), 2);
            assert_eq!(run.checkpoint.expansions.len(), 1);
        } else {
            assert_eq!(run.checkpoint.nodes[0].state, NodeState::Failed);
            assert_eq!(owner.calls.len(), 1);
            assert!(run.checkpoint.expansions.is_empty());
        }
    }
}

#[test]
fn no_work_continuation_recovers_after_checkpoint_failure_before_launch() {
    struct FailPatch {
        saved: FileJournal,
        fail: bool,
    }
    impl GraphJournal for FailPatch {
        type Error = std::io::Error;
        fn load(&mut self, id: &str) -> Result<Option<GraphCheckpoint>, Self::Error> {
            self.saved.load(id).map_err(std::io::Error::other)
        }
        fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error> {
            if self.fail && !checkpoint.expansions.is_empty() {
                return Err(std::io::Error::other("injected patch save failure"));
            }
            self.saved.save(checkpoint).map_err(std::io::Error::other)?;
            Ok(())
        }
    }
    let mut work = node("work", pool_route("junior"));
    work.access = AccessMode::WorktreeWrite;
    work.worktree = Some(".worktrees/work".into());
    let document = graph(vec![work]);
    let mut owner = FakeOwner {
        // The stop is observed again after the failed save, then the restart.
        observations: [
            no_work_stop(Some(&"a".repeat(40))),
            no_work_stop(Some(&"a".repeat(40))),
            done_as("finished"),
        ]
        .into(),
        ..Default::default()
    };
    use provenant_os::{clock::SystemClock, fs::SystemPathResolver};
    use std::os::unix::fs::PermissionsExt;
    let temp = crate::test_support::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let base = std::fs::canonicalize(temp.path()).unwrap();
    let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("checkout"));
    inputs.state_dir = Some(base.clone());
    let root = StateRoot::resolve(&inputs, &SystemPathResolver).unwrap();
    let project = ProjectId::from_bytes([42; 16]);
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
    let open =
        || FileJournal::open(&root, project, &document.graph_id, budget, &cancel, &clock).unwrap();
    let mut saved = open();
    saved
        .bind(GraphExecution {
            review_base: None,
            extra: Default::default(),
            integration_base: None,
            directory: base,
            document: document.clone(),
        })
        .unwrap();
    let mut journal = FailPatch { saved, fail: true };
    assert!(matches!(
        advance(&document, &mut owner, &mut journal),
        Err(GraphError::Journal(_))
    ));
    assert_eq!(owner.calls.len(), 1);
    // The stop and its restart patch are one save, so the failed save keeps
    // the stop out of the journal too: the writer still reads as running.
    assert_eq!(
        journal
            .saved
            .load(&document.graph_id)
            .unwrap()
            .unwrap()
            .nodes[0]
            .state,
        NodeState::Running
    );
    drop(journal);
    // Ordinary tick and settlement replay use this API to discover work.
    assert!(
        FileJournal::pending_execution(&root, project, &document.graph_id)
            .unwrap()
            .is_some()
    );
    // No reader can see the stop without its restart, so status never
    // reports the stopped attempt as the node's final one.
    let status = FileJournal::read_status_checkpoint(&root, project, &document.graph_id)
        .unwrap()
        .unwrap();
    assert!(!status.nodes.iter().all(|node| node.state.settled()));
    let mut journal = open();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(owner.calls.len(), 3);
    assert_eq!(owner.calls[0].0, owner.calls[1].0);
    assert_ne!(owner.calls[1].0, owner.calls[2].0);
    assert_eq!(run.checkpoint.nodes[0].state, NodeState::Succeeded);
    assert_eq!(
        run.checkpoint
            .expansions
            .iter()
            .filter(|e| e.patch.is_some())
            .count(),
        1
    );
    drop(journal);
    assert!(
        FileJournal::pending_execution(&root, project, &document.graph_id)
            .unwrap()
            .is_none()
    );
    let events = FileJournal::read_events(&root, project, &document.graph_id).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == GraphEventKind::Patch)
            .count(),
        1
    );
}

#[test]
fn stopping_settled_no_work_fences_failed_checkpoint_recovery() {
    // One exact route: the restart has nowhere else to go, so the stop is
    // saved with a chair pause. The chair then resumes, leaving an unfenced
    // stop that owes a restart, the state an earlier host saved alone.
    let mut work = node("work", route("first"));
    work.access = AccessMode::WorktreeWrite;
    work.worktree = Some(".worktrees/work".into());
    let document = graph(vec![work]);
    let mut owner = FakeOwner {
        observations: [no_work_stop(Some(&"a".repeat(40)))].into(),
        ..Default::default()
    };
    use provenant_os::{clock::SystemClock, fs::SystemPathResolver};
    use std::os::unix::fs::PermissionsExt;
    let temp = crate::test_support::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let base = std::fs::canonicalize(temp.path()).unwrap();
    let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("checkout"));
    inputs.state_dir = Some(base.clone());
    let root = StateRoot::resolve(&inputs, &SystemPathResolver).unwrap();
    let project = ProjectId::from_bytes([42; 16]);
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
    let open =
        || FileJournal::open(&root, project, &document.graph_id, budget, &cancel, &clock).unwrap();
    let mut saved = open();
    saved
        .bind(GraphExecution {
            review_base: None,
            extra: Default::default(),
            integration_base: None,
            directory: base,
            document: document.clone(),
        })
        .unwrap();
    let mut journal = saved;
    assert_eq!(
        advance(&document, &mut owner, &mut journal).unwrap().status,
        GraphStatus::Paused
    );
    assert_eq!(owner.calls.len(), 1);
    let mut checkpoint = journal.load(&document.graph_id).unwrap().unwrap();
    assert_eq!(checkpoint.nodes[0].state, NodeState::Failed);
    plans::clear_expansion_pause(&mut checkpoint, 0, true);
    assert!(checkpoint.pause.is_none());
    let original = checkpoint.nodes[0].clone();
    assert_eq!(original.advance_error_code.as_deref(), Some("stopped_no_work"));
    assert!(next_step::pending(&checkpoint));
    cancel_node_subtree(&document, &mut checkpoint, "work", Some("chair stop")).unwrap();
    assert_eq!(checkpoint.nodes[0].attempts, original.attempts);
    assert_eq!(checkpoint.nodes[0].state, original.state);
    assert_eq!(checkpoint.nodes[0].advance_error, original.advance_error);
    assert_eq!(checkpoint.nodes[0].advance_error_code, original.advance_error_code);
    journal.save(&checkpoint).unwrap();
    drop(journal);

    assert!(FileJournal::pending_execution(&root, project, &document.graph_id)
        .unwrap().is_none());
    let mut journal = open();
    let checkpoint = journal.load(&document.graph_id).unwrap().unwrap();
    assert!(next_step::stop_cause(&checkpoint.nodes[0], &checkpoint).is_none());
    assert!(!next_step::pending(&checkpoint));
    assert_eq!(checkpoint.nodes[0].attempts, original.attempts);
    assert_eq!(checkpoint.nodes[0].advance_error, original.advance_error);
    assert_eq!(checkpoint.nodes[0].advance_error_code, original.advance_error_code);
    let mut recovery_owner = FakeOwner::default();
    let run = advance(&document, &mut recovery_owner, &mut journal).unwrap();
    assert!(recovery_owner.calls.is_empty());
    assert_eq!(run.checkpoint.nodes[0].attempts, original.attempts);
    assert_eq!(run.checkpoint.nodes[0].state, NodeState::Failed);
}

#[test]
fn no_work_continuations_share_the_graph_budget_and_resume_after_extension() {
    let writer = |id| {
        let mut work = node(id, pool_route("junior"));
        work.access = AccessMode::WorktreeWrite;
        work.worktree = Some(format!(".worktrees/{id}"));
        work
    };
    for allowance in [0, 1] {
        let document = graph(vec![writer("first"), writer("second")]);
        let mut owner = FakeOwner {
            observations: [
                no_work_stop(Some(&"a".repeat(40))),
                done_as("first-done"),
                no_work_stop(Some(&"a".repeat(40))),
                done_as("second-done"),
            ]
            .into(),
            ..Default::default()
        };
        let mut limits = crate::config::GraphLimits {
            continuations: allowance,
            ..Default::default()
        };
        let mut journal = MemoryJournal::default();
        let run = advance_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &limits,
            &Default::default(),
        )
        .unwrap();
        assert_eq!(run.status, GraphStatus::Paused);
        assert_eq!(owner.calls.len(), if allowance == 0 { 1 } else { 3 });
        assert_eq!(run.checkpoint.remaining.as_ref().unwrap().continuations, 0);
        limits.continuations = 2;
        let run = resume_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &limits,
            &Default::default(),
            &[],
        )
        .unwrap();
        assert!(run.checkpoint.successful());
        assert_eq!(owner.calls.len(), 4);
        assert_eq!(run.checkpoint.remaining.as_ref().unwrap().continuations, 0);
    }
}

#[test]
fn no_work_continuation_retains_sealed_quota_retry_spending_and_models() {
    let mut alias = pool_route("junior");
    alias.adapter = None;
    alias.model = None;
    alias.alias = Some("junior".into());
    let mut work = node("work", alias);
    work.access = AccessMode::WorktreeWrite;
    work.worktree = Some(".worktrees/work".into());
    let document = graph(vec![work]);
    let quota = |reference: &str| {
        let mut observation = terminal(AttemptOutcome::UsageLimited);
        if let AttemptObservation::Terminal { attempt_ref, .. } = &mut observation {
            *attempt_ref = Some(reference.into());
        }
        observation
    };
    let mut owner = FakeOwner {
        quota_redraw: true,
        retry_limit: Some(1),
        observations: [
            quota("quota/first"),
            no_work_stop(Some(&"a".repeat(40))),
            quota("quota/last"),
        ]
        .into(),
        models: [
            ("quota/first".into(), "codex/limited".into()),
            ("stopped".into(), "claude/stopped".into()),
        ]
        .into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    // The spent quota retry stays spent, so the last usage limit cannot
    // restart the writer again.
    assert_eq!(owner.calls.len(), 3);
    assert_eq!(run.checkpoint.nodes[0].state, NodeState::Failed);
    assert_eq!(run.checkpoint.nodes[0].extra["retry"]["retries"], 1);
    assert_eq!(owner.prior_attempts[2], ["codex/limited", "claude/stopped"]);
}

#[test]
fn no_work_repair_continues_after_upstream_advanced_its_worktree() {
    use provenant_records::run::DeliveryMode;
    let mut write = node("write", pool_route("junior"));
    write.access = AccessMode::WorktreeWrite;
    write.worktree = Some(".worktrees/work".into());
    write.delivery = Some(DeliveryMode::Push);
    let mut repair = node("repair", pool_route("junior"));
    repair.access = AccessMode::WorktreeWrite;
    repair.worktree = write.worktree.clone();
    repair.continues = Some("write".into());
    repair.depends_on = vec!["write".into()];
    let document = graph(vec![write, repair]);
    let mut committed = done_as("upstream");
    if let AttemptObservation::Terminal { commit, branch, .. } = &mut committed {
        *commit = Some("b".repeat(40));
        *branch = Some("topic/work".into());
    }
    let mut owner = FakeOwner {
        observations: [
            committed,
            no_work_stop(Some(&"b".repeat(40))),
            done_as("repaired"),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert!(run.checkpoint.successful());
    assert_eq!(
        owner
            .calls
            .iter()
            .map(|(key, _)| key.node_id.as_str())
            .collect::<Vec<_>>(),
        ["write", "repair", "repair"]
    );
    assert_eq!(owner.permissions[1], owner.permissions[2]);
    assert!(owner.fast_forward_only[2]);
    assert_eq!(run.checkpoint.nodes[1].previous_attempts.len(), 1);
}

#[test]
fn stopped_writer_restarts_fresh_on_another_route_with_its_handoff() {
    let mut work = node("work", route("first"));
    work.route = None;
    work.routes = Some(vec![route("first"), route("second")]);
    work.access = AccessMode::WorktreeWrite;
    work.worktree = Some(".worktrees/work".into());
    let document = graph(vec![work]);
    let mut stop = no_work_stop(Some(&"a".repeat(40)));
    let note = "```handoff\n{\"schema_version\":2,\"status\":\"stopped_early\",\"note\":\"Parser is ready; finish renderer\",\"remaining\":[\"Finish renderer\"],\"decisions\":[\"Keep parser API\"],\"verify\":[]}\n```";
    if let AttemptObservation::Terminal { result, .. } = &mut stop {
        *result = Some(Box::new(capture(note)));
    }
    let mut owner = FakeOwner { observations: [stop, done_as("finished")].into(), ..Default::default() };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert!(run.checkpoint.successful());
    assert_eq!(owner.calls.len(), 2);
    assert_eq!(owner.calls[0].1, route("first"));
    assert_eq!(owner.calls[1].1, route("second"));
    assert_ne!(owner.calls[0].0, owner.calls[1].0);
    assert_eq!(owner.stored_refs, [None, None]);
    assert!(owner.prompts[1].contains("Finish renderer"));
    assert!(owner.prompts[1].contains("Keep parser API"));
    assert_eq!(owner.permissions[0], owner.permissions[1]);
}

#[test]
fn no_work_stop_is_never_saved_without_its_restart() {
    // Every reader, status included, sees the saved checkpoints only. None of
    // them may hold a stop that still owes its restart.
    struct Watch(MemoryJournal, usize);
    impl GraphJournal for Watch {
        type Error = std::convert::Infallible;
        fn load(&mut self, id: &str) -> Result<Option<GraphCheckpoint>, Self::Error> {
            self.0.load(id)
        }
        fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error> {
            assert!(!next_step::pending(checkpoint));
            self.1 += 1;
            self.0.save(checkpoint)
        }
    }
    let mut work = node("work", pool_route("junior"));
    work.access = AccessMode::WorktreeWrite;
    work.worktree = Some(".worktrees/work".into());
    let document = graph(vec![work]);
    let mut owner = FakeOwner {
        observations: [no_work_stop(Some(&"a".repeat(40))), done_as("finished")].into(),
        ..Default::default()
    };
    let mut journal = Watch(MemoryJournal::default(), 0);
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert!(run.checkpoint.successful());
    assert_eq!(owner.calls.len(), 2);
    assert!(journal.1 > 0);
}

#[test]
fn no_work_restart_refused_before_planning_saves_a_chair_pause_with_the_stop() {
    // The handoff lands in the restart brief, and patch admission scans the
    // brief for secrets, so this restart is refused before it can be planned.
    let mut work = node("work", route("first"));
    work.route = None;
    work.routes = Some(vec![route("first"), route("second")]);
    work.access = AccessMode::WorktreeWrite;
    work.worktree = Some(".worktrees/work".into());
    let document = graph(vec![work]);
    let mut stop = no_work_stop(Some(&"a".repeat(40)));
    let note = "```handoff\n{\"schema_version\":2,\"status\":\"stopped_early\",\"note\":\"token ghp_abcdefghijklmnopqrstuvwxyz0123456789\",\"remaining\":[\"Finish renderer\"],\"decisions\":[],\"verify\":[]}\n```";
    if let AttemptObservation::Terminal { result, .. } = &mut stop {
        *result = Some(Box::new(capture(note)));
    }
    let mut owner = FakeOwner { observations: [stop].into(), ..Default::default() };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    // Settled at Paused, not Pending: the stop and its chair pause are one save.
    assert_eq!(run.status, GraphStatus::Paused);
    assert_eq!(owner.calls.len(), 1);
    assert!(run.checkpoint.expansions.is_empty());
    let reason = run.checkpoint.pause_reason().unwrap();
    assert!(reason.contains("its restart was refused"), "{reason}");
    assert!(reason.contains("secret"), "{reason}");
    assert_eq!(run.checkpoint.nodes[0].state, NodeState::Failed);
    assert!(!next_step::pending(&run.checkpoint));
    let saved = journal.load(&document.graph_id).unwrap().unwrap();
    assert_eq!(saved.pause_reason(), run.checkpoint.pause_reason());
    assert_eq!(saved.nodes[0].state, NodeState::Failed);
    // Another advance stays paused and launches nothing.
    assert_eq!(advance(&document, &mut owner, &mut journal).unwrap().status, GraphStatus::Paused);
    assert_eq!(owner.calls.len(), 1);
}

#[test]
fn stopped_writer_without_another_route_returns_to_the_chair() {
    let mut work = node("work", route("first"));
    work.access = AccessMode::WorktreeWrite;
    work.worktree = Some(".worktrees/work".into());
    let document = graph(vec![work]);
    let mut owner = FakeOwner { observations: [no_work_stop(Some(&"a".repeat(40)))].into(), ..Default::default() };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert_eq!(owner.calls.len(), 1);
    assert!(run.checkpoint.expansions.is_empty());
    assert!(!next_step::pending(&run.checkpoint));
    assert_eq!(advance(&document, &mut owner, &mut journal).unwrap().status, GraphStatus::Paused);
    assert_eq!(owner.calls.len(), 1);
}

#[test]
fn writer_owner_decision_or_missing_capability_returns_to_chair_without_launch() {
    for note in ["Owner must choose the release target", "Missing capability: network access is required"] {
        for outcome in [AttemptOutcome::Succeeded, AttemptOutcome::DeclaredStop, AttemptOutcome::StoppedNoWork] {
            let no_work = outcome == AttemptOutcome::StoppedNoWork;
            let mut work = node("work", pool_route("senior"));
            work.access = AccessMode::WorktreeWrite;
            work.worktree = Some(".worktrees/work".into());
            let document = graph(vec![work]);
            let handoff = format!("```handoff\n{}\n```", serde_json::json!({"schema_version":2,"status":"blocked","note":note,"remaining":["Finish accepted brief"],"decisions":[],"verify":[]}));
            let mut observation = if no_work { no_work_stop(Some(&"a".repeat(40))) } else { done_as("blocked") };
            if let AttemptObservation::Terminal { result, outcome: observed, .. } = &mut observation {
                *observed = outcome;
                *result = Some(Box::new(capture(&handoff)));
            }
            let mut owner = FakeOwner { observations: [observation].into(), ..Default::default() };
            let mut journal = MemoryJournal::default();
            let run = advance(&document, &mut owner, &mut journal).unwrap();
            assert_eq!(run.status, GraphStatus::Paused, "{note}, no_work={no_work}");
            assert_eq!(owner.calls.len(), 1);
            assert!(run.checkpoint.expansions.is_empty());
            assert!(run.checkpoint.nodes[0].handoff.as_ref().unwrap().fenced().contains(note));
            assert!(!next_step::pending(&run.checkpoint));
            assert_eq!(advance(&document, &mut owner, &mut journal).unwrap().status, GraphStatus::Paused);
            assert_eq!(owner.calls.len(), 1);
        }
    }
}

#[test]
fn container_retry_brief_names_the_field_and_template_the_container_came_from() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1,"graph_id":"redo-container-brief","nodes":[{
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
    // A container built from a template that declares no `brief` parameter.
    checkpoint.expansions.push(plans::Expansion {
        extra: Default::default(),
        source: String::new(),
        nodes: Vec::new(),
        uses: vec![patch::TemplateUse {
            extra: Default::default(),
            id: "work".into(),
            name: "review-panel".into(),
            parameters: serde_json::Value::Null,
            nodes: vec!["work".into()],
        }],
        patch: Some(
            serde_json::from_value(json!({"revision":1,"digest":"sha256:0","journaled":true}))
                .unwrap(),
        ),
    });
    let error = retry_id(
        &document,
        &mut checkpoint,
        "work",
        Some("Redo it"),
        &|_| Ok(()),
        None,
    )
    .unwrap_err();
    let refusal = error.parameter.clone().unwrap_or_else(|| panic!("{}", error.message));
    assert_eq!(refusal.template, "review-panel");
    assert_eq!(refusal.node.as_deref(), Some("work"));
    assert!(matches!(
        refusal.fault,
        template::ParameterFault::Unknown { key: Some(ref key), .. } if key == "brief"
    ));
}
