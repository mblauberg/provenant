use super::*;

fn advance(
    document: &GraphDocument,
    owner: &mut FakeOwner,
    journal: &mut MemoryJournal,
) -> Result<GraphRun, GraphError<NoObservation, Infallible>> {
    let limits = crate::config::GraphLimits {
        expansion_depth: 16,
        ..Default::default()
    };
    advance_with_budget(
        document,
        owner,
        journal,
        &limits,
        &Default::default(),
    )
}

fn fixture(name: &str) -> GraphDocument {
    let text = match name {
        "pass" => include_str!("../fixtures/loop-pass-round-two.json"),
        "exhaust" => include_str!("../fixtures/loop-exhausts.json"),
        "extend" => include_str!("../fixtures/loop-extends.json"),
        _ => panic!("unknown fixture"),
    };
    serde_json::from_str(text).unwrap()
}

fn reviews(results: &[&str]) -> FakeOwner {
    FakeOwner {
        actor: "chair".into(),
        observations: results
            .iter()
            .map(|text| terminal_with_result(AttemptOutcome::Succeeded, text))
            .collect(),
        ..Default::default()
    }
}

fn extension(
    document: &GraphDocument,
    owner: &mut FakeOwner,
    journal: &mut MemoryJournal,
    values: &[String],
) -> GraphRun {
    resume_with_budget(
        document,
        owner,
        journal,
        &Default::default(),
        &Default::default(),
        values,
    )
    .unwrap()
}

#[test]
fn loop_fixture_passes_on_round_two_and_status_retains_outputs() {
    let document = fixture("pass");
    let mut owner = reviews(&["VERDICT: FAIL", "VERDICT: PASS"]);
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(run.checkpoint.successful());
    assert_eq!(owner.calls.len(), 2);
    assert!(owner.prompts[1].contains("revise--r1--review verdict: "));
    assert!(owner.prompts[1].contains("VERDICT: FAIL"));
    assert_ne!(owner.calls[0].0, owner.calls[1].0);
    let status = serde_json::to_value(&run).unwrap();
    let rows = status["checkpoint"]["nodes"].as_array().unwrap();
    let review = rows
        .iter()
        .find(|row| row["id"] == "revise--r2--review")
        .unwrap();
    assert_eq!(review["round"]["round"], 2);
    assert_eq!(review["round"]["loop_id"], "revise");
    assert_eq!(review["outputs"]["verdict"], "VERDICT: PASS");
    assert!(
        run.checkpoint
            .nodes
            .iter()
            .find(|n| n.id == "revise")
            .unwrap()
            .deterministic_summary()
            .is_some()
    );
    let replay = advance(&document, &mut reviews(&[]), &mut journal).unwrap();
    assert_eq!(run.checkpoint, replay.checkpoint);
}

#[test]
fn loop_fixture_exhausts_and_pauses_without_delivery() {
    let mut document = fixture("exhaust");
    document.delivery = Some(provenant_records::run::DeliveryMode::Pr);
    document.integration_branch = Some("integration".into());
    let mut owner = reviews(&["VERDICT: FAIL", "VERDICT: FAIL"]);
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert!(
        run.checkpoint
            .pause_reason
            .as_ref()
            .unwrap()
            .contains("after 2 rounds")
    );
    assert!(!run.checkpoint.successful());
    assert_eq!(owner.deliveries, 0);
    assert_eq!(
        run.checkpoint
            .nodes
            .iter()
            .find(|n| n.id == "revise")
            .unwrap()
            .loop_decision_actor
            .as_deref(),
        Some("chair")
    );
    assert_eq!(
        advance(&document, &mut reviews(&[]), &mut journal)
            .unwrap()
            .checkpoint,
        run.checkpoint
    );
    assert_eq!(
        resume(&document, &mut reviews(&[]), &mut journal)
            .unwrap()
            .status,
        GraphStatus::Paused
    );
}

#[test]
fn loop_fixture_resumes_after_extension_without_repeating_rounds() {
    let document = fixture("extend");
    let mut journal = MemoryJournal::default();
    let first = advance(&document, &mut reviews(&["VERDICT: FAIL"]), &mut journal).unwrap();
    assert_eq!(first.status, GraphStatus::Paused);
    let mut owner = reviews(&["VERDICT: PASS"]);
    let run = extension(
        &document,
        &mut owner,
        &mut journal,
        &["loop.revise.rounds=2".into()],
    );
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(run.checkpoint.successful());
    assert_eq!(owner.calls.len(), 1);
    assert_eq!(owner.calls[0].0.node_id, "revise--r2--review");
    assert!(owner.prompts[0].contains("VERDICT: FAIL"));
    assert_eq!(run.checkpoint.nodes[0], first.checkpoint.nodes[0]);
    let replay = extension(
        &document,
        &mut reviews(&[]),
        &mut journal,
        &["loop.revise.rounds=2".into()],
    );
    assert_eq!(run.checkpoint, replay.checkpoint);
}

#[test]
fn loop_replay_mid_round_keeps_attempt_actor_and_input() {
    let document = fixture("pass");
    let mut journal = MemoryJournal::default();
    let mut owner = reviews(&["VERDICT: FAIL"]);
    owner.observations.push_back(AttemptObservation::Pending {
        attempt_ref: Some("accepted".into()),
    });
    let pending = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(pending.status, GraphStatus::Pending);
    let mut resumed = reviews(&["VERDICT: PASS"]);
    resumed.actor = "different".into();
    let run = advance(&document, &mut resumed, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert_eq!(owner.calls[1].0, resumed.calls[0].0);
    assert_eq!(owner.prompts[1], resumed.prompts[0]);
    assert_eq!(resumed.launch_actors, ["chair"]);
    assert_eq!(resumed.stored_refs, [Some("accepted".into())]);
}

#[test]
fn loop_admission_counts_every_round_before_any_launch() {
    let document = fixture("pass");
    let mut journal = MemoryJournal::default();
    let mut owner = reviews(&[]);
    let limits = crate::config::GraphLimits {
        nodes: 6,
        ..Default::default()
    };
    let run = advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &limits,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert!(
        run.checkpoint
            .pause_reason
            .unwrap()
            .contains("limits.nodes=6 reached (7)")
    );
    assert!(owner.calls.is_empty());
}

#[test]
fn loop_decisions_stop_or_accept_exhausted_outputs() {
    for (decision, succeeded) in [("stop", false), ("accept", true)] {
        let document = fixture("extend");
        let mut journal = MemoryJournal::default();
        advance(&document, &mut reviews(&["VERDICT: FAIL"]), &mut journal).unwrap();
        let run = extension(
            &document,
            &mut reviews(&[]),
            &mut journal,
            &[format!("loop.revise.decision={decision}")],
        );
        assert_eq!(run.status, GraphStatus::Complete);
        assert_eq!(run.checkpoint.successful(), succeeded);
        assert_eq!(
            advance(&document, &mut reviews(&[]), &mut journal)
                .unwrap()
                .checkpoint,
            run.checkpoint
        );
    }
}

#[test]
fn loop_writers_keep_one_worktree_and_never_deliver_between_rounds() {
    let mut document = fixture("pass");
    let spec = document.nodes[0].loop_body.as_mut().unwrap();
    spec.body[0].access = AccessMode::WorktreeWrite;
    spec.body[0].worktree = Some("/repo/writer".into());
    let mut owner = reviews(&["VERDICT: FAIL", "VERDICT: PASS"]);
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert!(run.checkpoint.successful());
    assert_eq!(
        owner.permissions,
        vec![(AccessMode::WorktreeWrite, Some("/repo/writer".into())); 2]
    );
    assert_eq!(owner.delivery_modes, [None, None]);
}

#[test]
fn loop_body_failure_blocks_later_rounds_and_dependants() {
    let mut document = fixture("pass");
    let mut child = node("after", route("fixture"));
    child.depends_on = vec!["revise".into()];
    document.nodes.push(child);
    let mut owner = FakeOwner {
        observations: [terminal(AttemptOutcome::TaskFailed)].into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(!run.checkpoint.successful());
    assert_eq!(owner.calls.len(), 1);
    assert_eq!(
        advance(&document, &mut reviews(&[]), &mut journal)
            .unwrap()
            .checkpoint,
        run.checkpoint
    );
}

#[test]
fn loop_on_fail_and_inactive_paths_reuse_gate_routing() {
    for fail in [false, true] {
        let mut document = fixture("extend");
        let mut gate = gate_node(GateCondition::Verdict {
            expected: "VERDICT: PASS".into(),
            threshold: VerdictThreshold::All,
        });
        gate.id = "initial".into();
        gate.depends_on = vec!["initial-review".into()];
        gate.inputs = vec![GraphInput {
            node: "initial-review".into(),
            outputs: vec![OutputKind::Verdict],
        }];
        gate.on_fail = Some("revise".into());
        document.nodes[0].depends_on = vec!["initial".into()];
        document.nodes.insert(0, gate);
        document
            .nodes
            .insert(0, node("initial-review", route("fixture")));
        let mut owner = reviews(if fail {
            &["VERDICT: FAIL", "VERDICT: PASS"]
        } else {
            &["VERDICT: PASS"]
        });
        let mut journal = MemoryJournal::default();
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        assert_eq!(run.status, GraphStatus::Complete);
        assert!(run.checkpoint.successful());
        assert_eq!(owner.calls.len(), if fail { 2 } else { 1 });
        assert_eq!(
            advance(&document, &mut reviews(&[]), &mut journal)
                .unwrap()
                .checkpoint,
            run.checkpoint
        );
    }
}

#[test]
fn loop_rejects_decisions_before_exhaustion_and_conflicting_terminal_decisions() {
    let document = fixture("extend");
    let mut journal = MemoryJournal::default();
    let mut owner = reviews(&[]);
    owner
        .observations
        .push_back(AttemptObservation::Pending { attempt_ref: None });
    advance(&document, &mut owner, &mut journal).unwrap();
    for value in [
        "loop.revise.rounds=2",
        "loop.revise.decision=accept",
        "loop.revise.decision=stop",
    ] {
        assert!(
            resume_with_budget(
                &document,
                &mut reviews(&[]),
                &mut journal,
                &Default::default(),
                &Default::default(),
                &[value.into()]
            )
            .is_err()
        );
    }
    advance(&document, &mut reviews(&["VERDICT: FAIL"]), &mut journal).unwrap();
    extension(
        &document,
        &mut reviews(&[]),
        &mut journal,
        &["loop.revise.decision=stop".into()],
    );
    assert!(
        resume_with_budget(
            &document,
            &mut reviews(&[]),
            &mut journal,
            &Default::default(),
            &Default::default(),
            &["loop.revise.decision=accept".into()]
        )
        .is_err()
    );
}

#[test]
fn loop_replays_every_checkpoint_boundary_including_extensions() {
    struct Recording {
        current: MemoryJournal,
        snapshots: Vec<GraphCheckpoint>,
    }
    impl GraphJournal for Recording {
        type Error = Infallible;
        fn load(&mut self, id: &str) -> Result<Option<GraphCheckpoint>, Infallible> {
            self.current.load(id)
        }
        fn save(&mut self, c: &GraphCheckpoint) -> Result<(), Infallible> {
            self.snapshots.push(c.clone());
            self.current.save(c)
        }
    }
    struct Owner;
    impl AttemptOwner for Owner {
        type Error = Infallible;
        fn actor(&self) -> String {
            "chair".into()
        }
        fn observe_or_start(
            &mut self,
            key: &AttemptKey,
            _: NodeInput<'_>,
            _: &GraphRoute,
            _: AccessMode,
            _: Option<&str>,
            _: Replay<'_>,
        ) -> Result<AttemptObservation, Infallible> {
            Ok(terminal_with_result(
                AttemptOutcome::Succeeded,
                if key.node_id.contains("--r1--") {
                    "VERDICT: FAIL"
                } else {
                    "VERDICT: PASS"
                },
            ))
        }
    }
    let document = fixture("extend");
    let mut journal = Recording {
        current: MemoryJournal::default(),
        snapshots: vec![],
    };
    let limits = crate::config::GraphLimits::default();
    advance_with_budget(
        &document,
        &mut Owner,
        &mut journal,
        &limits,
        &Default::default(),
    )
    .unwrap();
    let run = resume_with_budget(
        &document,
        &mut Owner,
        &mut journal,
        &limits,
        &Default::default(),
        &["loop.revise.rounds=2".into()],
    )
    .unwrap();
    for snapshot in journal.snapshots {
        let snapshot: GraphCheckpoint =
            serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
        let mut replay = MemoryJournal(Some(snapshot));
        let mut settled = advance_with_budget(
            &document,
            &mut Owner,
            &mut replay,
            &limits,
            &Default::default(),
        )
        .unwrap();
        if settled.status == GraphStatus::Paused {
            settled = resume_with_budget(
                &document,
                &mut Owner,
                &mut replay,
                &limits,
                &Default::default(),
                &["loop.revise.rounds=2".into()],
            )
            .unwrap();
        }
        assert_eq!(settled.checkpoint, run.checkpoint);
    }
}

#[test]
fn loop_rejects_unbounded_or_ambiguous_body_configuration() {
    let original = fixture("pass");
    for change in 0..7 {
        let mut document = original.clone();
        let spec = document.nodes[0].loop_body.as_mut().unwrap();
        match change {
            0 => spec.max_rounds = 0,
            1 => spec.max_rounds = 1025,
            2 => spec.until = "review".into(),
            3 => spec.body[0].delivery = Some(provenant_records::run::DeliveryMode::Push),
            4 => spec.body[0].inputs[0].node = "previous.missing".into(),
            5 => spec.body.push(node("orphan", route("fixture"))),
            _ => spec.body[0].inputs[0].outputs = vec![],
        }
        assert!(validate_plan(&document).is_err(), "case {change}");
    }
}

#[test]
fn loop_inbox_extension_keeps_original_actor_and_applies_once() {
    struct InboxOwner {
        inner: FakeOwner,
        decision: Option<LoopDecision>,
        targets: Vec<(usize, String, String)>,
    }
    impl AttemptOwner for InboxOwner {
        type Error = NoObservation;
        fn actor(&self) -> String {
            self.inner.actor()
        }
        fn loop_decision(
            &mut self,
            scope: LoopScope<'_>,
        ) -> Result<Option<LoopDecision>, NoObservation> {
            self.targets
                .push((scope.rounds, scope.session.into(), scope.actor.into()));
            Ok(self.decision.take())
        }
        fn observe_or_start(
            &mut self,
            key: &AttemptKey,
            node: NodeInput<'_>,
            route: &GraphRoute,
            access: AccessMode,
            worktree: Option<&str>,
            replay: Replay<'_>,
        ) -> Result<AttemptObservation, NoObservation> {
            self.inner
                .observe_or_start(key, node, route, access, worktree, replay)
        }
    }
    let mut document = fixture("extend");
    document.nodes[0]
        .loop_body
        .as_mut()
        .unwrap()
        .decision_session = Some("chair-session".into());
    let mut owner = InboxOwner {
        inner: reviews(&["VERDICT: FAIL"]),
        decision: None,
        targets: vec![],
    };
    let mut journal = MemoryJournal::default();
    let limits = crate::config::GraphLimits::default();
    let paused = advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &limits,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(paused.status, GraphStatus::Paused);
    owner.inner.actor = "guardian".into();
    owner.inner.observations.push_back(terminal_with_result(
        AttemptOutcome::Succeeded,
        "VERDICT: PASS",
    ));
    owner.decision = Some(LoopDecision::Extend { rounds: 1 });
    let run = resume_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &limits,
        &Default::default(),
        &[],
    )
    .unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert_eq!(
        owner.targets,
        vec![(1, "chair-session".into(), "chair".into()); 2]
    );
    assert_eq!(run.checkpoint.extensions, ["loop.revise.rounds=2"]);
    let replay = advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &limits,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(run.checkpoint, replay.checkpoint);
    assert_eq!(owner.targets.len(), 2);
}

#[test]
fn loop_budget_counts_prior_round_attempts() {
    let document = fixture("exhaust");
    let mut owner = reviews(&["VERDICT: FAIL", "VERDICT: PASS"]);
    for (index, observation) in owner.observations.iter_mut().enumerate() {
        if let AttemptObservation::Terminal { attempt_ref, .. } = observation {
            *attempt_ref = Some(format!("attempt-{index}"));
        }
    }
    owner.usage.tokens = Some(5);
    let budget = crate::config::GraphBudget {
        per_graph: crate::config::BudgetLimit {
            tokens: Some(5),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let limits = crate::config::GraphLimits::default();
    let paused = advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &limits,
        &budget,
    )
    .unwrap();
    assert_eq!(paused.status, GraphStatus::Paused);
    assert_eq!(owner.calls.len(), 1);
    assert!(
        paused
            .checkpoint
            .pause_reason
            .unwrap()
            .contains("per_graph.tokens")
    );
    let run = resume_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &limits,
        &budget,
        &["budget.per_graph.tokens=10".into()],
    )
    .unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(run.checkpoint.successful());
    assert_eq!(owner.calls.len(), 2);
}

#[test]
fn loop_outside_dependencies_do_not_multiply_fan_out() {
    let mut document = fixture("pass");
    document.nodes[0].depends_on = vec!["plan".into()];
    document.nodes[0].loop_body.as_mut().unwrap().max_rounds = 11;
    document.nodes.insert(0, node("plan", route("fixture")));
    let limits = crate::config::GraphLimits {
        expansion_depth: 32,
        ..Default::default()
    };
    let mut owner = reviews(&["Plan complete", "VERDICT: PASS"]);
    let mut journal = MemoryJournal::default();
    let run = advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &limits,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(run.checkpoint.successful());
    assert_eq!(owner.calls.len(), 2);
}

#[test]
fn loop_invalid_inbox_answers_pause_and_keep_observing_other_attempts() {
    struct Owner {
        inner: FakeOwner,
        answer: LoopDecision,
    }
    impl AttemptOwner for Owner {
        type Error = NoObservation;
        fn actor(&self) -> String {
            self.inner.actor()
        }
        fn loop_decision(
            &mut self,
            _: LoopScope<'_>,
        ) -> Result<Option<LoopDecision>, NoObservation> {
            Ok(Some(self.answer.clone()))
        }
        fn observe_or_start(
            &mut self,
            key: &AttemptKey,
            node: NodeInput<'_>,
            route: &GraphRoute,
            access: AccessMode,
            worktree: Option<&str>,
            replay: Replay<'_>,
        ) -> Result<AttemptObservation, NoObservation> {
            self.inner
                .observe_or_start(key, node, route, access, worktree, replay)
        }
    }
    for answer in [
        LoopDecision::Invalid {
            reason: "invalid JSON reply".into(),
        },
        LoopDecision::Extend { rounds: 6 },
        LoopDecision::Extend { rounds: 2000 },
        LoopDecision::Extend { rounds: usize::MAX },
    ] {
        let mut document = fixture("extend");
        document.nodes[0]
            .loop_body
            .as_mut()
            .unwrap()
            .decision_session = Some("chair-session".into());
        document.nodes.push(node("start", route("fixture")));
        let mut middle = node("middle", route("fixture"));
        middle.depends_on = vec!["start".into()];
        document.nodes.push(middle);
        let mut independent = node("independent", route("fixture"));
        independent.depends_on = vec!["middle".into()];
        document.nodes.push(independent);
        let mut owner = Owner {
            inner: reviews(&[]),
            answer,
        };
        owner
            .inner
            .observations
            .push_back(AttemptObservation::Pending {
                attempt_ref: Some("review-attempt".into()),
            });
        owner.inner.observations.extend([
            terminal(AttemptOutcome::Succeeded),
            terminal(AttemptOutcome::Succeeded),
        ]);
        owner
            .inner
            .observations
            .push_back(AttemptObservation::Pending {
                attempt_ref: Some("independent-attempt".into()),
            });
        let mut journal = MemoryJournal::default();
        let limits = crate::config::GraphLimits::default();
        advance_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &limits,
            &Default::default(),
        )
        .unwrap();
        owner.inner.observations.extend([
            terminal_with_result(AttemptOutcome::Succeeded, "VERDICT: FAIL"),
            terminal(AttemptOutcome::Succeeded),
        ]);
        let paused = advance_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &limits,
            &Default::default(),
        )
        .unwrap();
        assert_eq!(paused.status, GraphStatus::Paused);
        let wrapper = paused
            .checkpoint
            .nodes
            .iter()
            .position(|n| n.id == "revise")
            .unwrap();
        let observer = paused
            .checkpoint
            .nodes
            .iter()
            .position(|n| n.id == "independent")
            .unwrap();
        assert!(
            observer > wrapper,
            "regression requires polling after the invalid decision"
        );
        assert!(
            paused
                .checkpoint
                .pause_reason
                .as_ref()
                .unwrap()
                .contains("decision cannot be applied")
        );
        assert_eq!(
            paused
                .checkpoint
                .nodes
                .iter()
                .find(|n| n.id == "independent")
                .unwrap()
                .state,
            NodeState::Succeeded
        );
        assert_eq!(
            advance_with_budget(
                &document,
                &mut owner,
                &mut journal,
                &limits,
                &Default::default()
            )
            .unwrap()
            .checkpoint,
            paused.checkpoint
        );
        let accepted = resume_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &limits,
            &Default::default(),
            &["loop.revise.decision=accept".into()],
        )
        .unwrap();
        assert_eq!(accepted.status, GraphStatus::Complete);
        assert!(accepted.checkpoint.successful());
    }
}

#[test]
fn loop_delivery_retry_reopens_failed_wrapper_and_dependants() {
    use provenant_os::{clock::SystemClock, fs::SystemPathResolver};
    use std::os::unix::fs::PermissionsExt;
    let temp = crate::test_support::tempdir().unwrap();
    let base = std::fs::canonicalize(temp.path()).unwrap();
    std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("checkout"));
    inputs.state_dir = Some(base.clone());
    let root = StateRoot::resolve(&inputs, &SystemPathResolver).unwrap();
    let project = ProjectId::from_bytes([1; 16]);
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
    let attempt = provenant_core::AttemptId::from_bytes([2; 16]);
    let mut document = fixture("extend");
    let writer = &mut document.nodes[0].loop_body.as_mut().unwrap().body[0];
    writer.access = AccessMode::WorktreeWrite;
    writer.worktree = Some("/repo/writer".into());
    let mut after = node("after", route("fixture"));
    after.depends_on = vec!["revise".into()];
    document.nodes.push(after);
    let mut observation = terminal(AttemptOutcome::OtherFailure);
    if let AttemptObservation::Terminal { attempt_ref, .. } = &mut observation {
        *attempt_ref = Some(format!("host/{attempt}"));
    }
    let mut owner = reviews(&[]);
    owner.observations.push_back(observation);
    let limits = crate::config::GraphLimits::default();
    {
        let mut journal =
            FileJournal::open(&root, project, &document.graph_id, budget, &cancel, &clock).unwrap();
        journal
            .bind(GraphExecution {
                integration_base: None,
                directory: base.clone(),
                document: document.clone(),
            })
            .unwrap();
        let failed = advance_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &limits,
            &Default::default(),
        )
        .unwrap();
        assert_eq!(
            failed
                .checkpoint
                .nodes
                .iter()
                .find(|n| n.id == "revise")
                .unwrap()
                .state,
            NodeState::Failed
        );
    }
    FileJournal::retry_delivered_attempt(&root, project, attempt, budget, &cancel, &clock).unwrap();
    let reopened = FileJournal::read_checkpoint(&root, project, &document.graph_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        reopened
            .nodes
            .iter()
            .find(|n| n.id == "revise")
            .unwrap()
            .state,
        NodeState::Pending
    );
    let mut journal =
        FileJournal::open(&root, project, &document.graph_id, budget, &cancel, &clock).unwrap();
    let mut owner = reviews(&["VERDICT: PASS", "Done"]);
    let completed = advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &limits,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(completed.status, GraphStatus::Complete);
    assert!(completed.checkpoint.successful());
    assert_eq!(owner.stored_refs[0], Some(format!("host/{attempt}")));
}

struct DecisionOwner {
    inner: FakeOwner,
    response: Result<Option<LoopDecision>, NoObservation>,
    closed: Vec<(String, usize)>,
}
impl AttemptOwner for DecisionOwner {
    type Error = NoObservation;
    fn actor(&self) -> String {
        self.inner.actor()
    }
    fn loop_decision(&mut self, _: LoopScope<'_>) -> Result<Option<LoopDecision>, Self::Error> {
        match &self.response {
            Ok(decision) => Ok(decision.clone()),
            Err(_) => Err(NoObservation),
        }
    }
    fn close_loop_ask(&mut self, scope: LoopScope<'_>) -> Result<(), Self::Error> {
        self.closed.push((scope.node_id.into(), scope.rounds));
        Ok(())
    }
    fn observe_or_start(
        &mut self,
        key: &AttemptKey,
        node: NodeInput<'_>,
        route: &GraphRoute,
        access: AccessMode,
        worktree: Option<&str>,
        replay: Replay<'_>,
    ) -> Result<AttemptObservation, Self::Error> {
        self.inner
            .observe_or_start(key, node, route, access, worktree, replay)
    }
}

#[test]
fn loop_default_host_allows_one_to_two_round_extension() {
    let document = fixture("extend");
    let limits = crate::config::GraphLimits::default();
    let mut journal = MemoryJournal::default();
    advance_with_budget(
        &document,
        &mut reviews(&["VERDICT: FAIL"]),
        &mut journal,
        &limits,
        &Default::default(),
    )
    .unwrap();
    let run = extension(
        &document,
        &mut reviews(&["VERDICT: PASS"]),
        &mut journal,
        &["loop.revise.rounds=2".into()],
    );
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(run.checkpoint.successful());
}

#[test]
fn loop_default_host_rejects_third_round_before_saving_and_can_accept() {
    let document = fixture("exhaust");
    let limits = crate::config::GraphLimits::default();
    let mut journal = MemoryJournal::default();
    let paused = advance_with_budget(
        &document,
        &mut reviews(&["VERDICT: FAIL", "VERDICT: FAIL"]),
        &mut journal,
        &limits,
        &Default::default(),
    )
    .unwrap();
    let rejected = extension(
        &document,
        &mut reviews(&[]),
        &mut journal,
        &["loop.revise.rounds=3".into()],
    );
    assert_eq!(rejected.status, GraphStatus::Paused);
    assert!(
        rejected
            .checkpoint
            .pause_reason
            .as_ref()
            .unwrap()
            .contains("limits.expansion_depth=4")
    );
    assert_eq!(
        rejected.checkpoint.nodes.len(),
        paused.checkpoint.nodes.len()
    );
    assert!(rejected.checkpoint.extensions.is_empty());
    let accepted = extension(
        &document,
        &mut reviews(&[]),
        &mut journal,
        &["loop.revise.decision=accept".into()],
    );
    assert!(accepted.checkpoint.successful());
}

#[test]
fn loop_inbox_extension_respects_default_ceiling_and_closes_rejected_ask() {
    let mut document = fixture("exhaust");
    document.nodes[0]
        .loop_body
        .as_mut()
        .unwrap()
        .decision_session = Some("chair-session".into());
    let mut owner = DecisionOwner {
        inner: reviews(&["VERDICT: FAIL", "VERDICT: FAIL"]),
        response: Ok(Some(LoopDecision::Extend { rounds: 1 })),
        closed: vec![],
    };
    let limits = crate::config::GraphLimits::default();
    let mut journal = MemoryJournal::default();
    let paused = advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &limits,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(paused.status, GraphStatus::Paused);
    assert!(
        paused
            .checkpoint
            .pause_reason
            .unwrap()
            .contains("limits.expansion_depth=4")
    );
    assert!(paused.checkpoint.extensions.is_empty());
    assert_eq!(owner.closed, [("revise".into(), 2)]);
    let stopped = resume_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &limits,
        &Default::default(),
        &["loop.revise.decision=stop".into()],
    )
    .unwrap();
    assert_eq!(stopped.status, GraphStatus::Complete);
}

#[test]
fn loop_extension_uses_updated_host_limits_before_committing() {
    let document = fixture("exhaust");
    let limits = crate::config::GraphLimits::default();
    let raised = crate::config::GraphLimits {
        expansion_depth: 6,
        ..limits.clone()
    };
    let mut journal = MemoryJournal::default();
    advance_with_budget(
        &document,
        &mut reviews(&["VERDICT: FAIL", "VERDICT: FAIL"]),
        &mut journal,
        &limits,
        &Default::default(),
    )
    .unwrap();
    let completed = resume_with_budget(
        &document,
        &mut reviews(&["VERDICT: PASS"]),
        &mut journal,
        &raised,
        &Default::default(),
        &["loop.revise.rounds=3".into()],
    )
    .unwrap();
    assert!(completed.checkpoint.successful());
}

#[test]
fn loop_extension_default_cap_and_total_round_bound_are_enforced() {
    let document = fixture("extend");
    let mut journal = MemoryJournal::default();
    advance(&document, &mut reviews(&["VERDICT: FAIL"]), &mut journal).unwrap();
    for target in [7, 1025] {
        assert!(
            resume_with_budget(
                &document,
                &mut reviews(&[]),
                &mut journal,
                &Default::default(),
                &Default::default(),
                &[format!("loop.revise.rounds={target}")]
            )
            .is_err()
        );
        assert!(journal.0.as_ref().unwrap().extensions.is_empty());
    }
}

#[test]
fn loop_inbox_failure_pauses_without_stopping_later_attempt_observation() {
    let mut document = fixture("extend");
    document.nodes[0]
        .loop_body
        .as_mut()
        .unwrap()
        .decision_session = Some("chair-session".into());
    for (name, parent) in [
        ("start", None),
        ("middle", Some("start")),
        ("late", Some("middle")),
    ] {
        let mut task = node(name, route("fixture"));
        if let Some(parent) = parent {
            task.depends_on.push(parent.into());
        }
        document.nodes.push(task);
    }
    let mut owner = DecisionOwner {
        inner: reviews(&[]),
        response: Err(NoObservation),
        closed: vec![],
    };
    owner.inner.observations.extend([
        AttemptObservation::Pending {
            attempt_ref: Some("review".into()),
        },
        terminal(AttemptOutcome::Succeeded),
        terminal(AttemptOutcome::Succeeded),
        AttemptObservation::Pending {
            attempt_ref: Some("late".into()),
        },
    ]);
    let limits = crate::config::GraphLimits::default();
    let mut journal = MemoryJournal::default();
    advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &limits,
        &Default::default(),
    )
    .unwrap();
    owner.inner.observations.extend([
        terminal_with_result(AttemptOutcome::Succeeded, "VERDICT: FAIL"),
        terminal(AttemptOutcome::Succeeded),
    ]);
    let paused = advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &limits,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(paused.status, GraphStatus::Paused);
    assert!(
        paused
            .checkpoint
            .pause_reason
            .unwrap()
            .contains("inbox unavailable")
    );
    assert_eq!(
        paused
            .checkpoint
            .nodes
            .iter()
            .find(|n| n.id == "late")
            .unwrap()
            .state,
        NodeState::Succeeded
    );
    let complete = resume_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &limits,
        &Default::default(),
        &["loop.revise.decision=accept".into()],
    )
    .unwrap();
    assert!(complete.checkpoint.successful());
}

#[test]
fn loop_simultaneous_exhaustion_keeps_both_reasons_and_resolves_individually() {
    let mut document = fixture("extend");
    let mut second = document.nodes[0].clone();
    second.id = "second".into();
    document.nodes.push(second);
    let limits = crate::config::GraphLimits::default();
    let mut journal = MemoryJournal::default();
    let paused = advance_with_budget(
        &document,
        &mut reviews(&["VERDICT: FAIL", "VERDICT: FAIL"]),
        &mut journal,
        &limits,
        &Default::default(),
    )
    .unwrap();
    for name in ["revise", "second"] {
        assert!(
            paused
                .checkpoint
                .pause_reason
                .as_ref()
                .unwrap()
                .contains(name)
        );
    }
    let one = extension(
        &document,
        &mut reviews(&[]),
        &mut journal,
        &["loop.revise.decision=accept".into()],
    );
    assert_eq!(one.status, GraphStatus::Paused);
    assert!(
        !one.checkpoint
            .pause_reason
            .as_ref()
            .unwrap()
            .contains("loop revise")
    );
    assert!(
        one.checkpoint
            .pause_reason
            .as_ref()
            .unwrap()
            .contains("loop second")
    );
    let both = extension(
        &document,
        &mut reviews(&[]),
        &mut journal,
        &["loop.second.decision=accept".into()],
    );
    assert!(both.checkpoint.successful());
}

#[test]
fn loop_rejects_nested_routing_worktree_prefix_and_identifier_ambiguities() {
    for case in 0..5 {
        let mut document = fixture("extend");
        let nested = document.nodes[0].clone();
        let spec = document.nodes[0].loop_body.as_mut().unwrap();
        match case {
            0 => spec.body[0] = nested,
            1 => spec.body[1].on_fail = Some("review".into()),
            2 => {
                spec.body[0].access = AccessMode::WorktreeWrite;
                spec.body[0].worktree = Some("/one".into());
                let mut writer = node("other", route("fixture"));
                writer.access = AccessMode::WorktreeWrite;
                writer.worktree = Some("/two".into());
                spec.body.push(writer);
            }
            3 => spec.body[0].id = "previous.reserved".into(),
            _ => document.nodes[0].id = "x".repeat(60),
        }
        let expected = match case {
            0 | 1 => "loop body cannot nest loops or deliver or route on_fail",
            2 => "loop writers must share one worktree",
            3 => "previous. is reserved for prior round inputs",
            _ => "expanded loop node id exceeds 64 bytes",
        };
        assert_eq!(
            validate_plan(&document).unwrap_err(),
            expected,
            "case {case}"
        );
    }
}

#[test]
fn loop_recreated_state_has_new_instance_with_same_plan() {
    let document = fixture("extend");
    let first = advance(
        &document,
        &mut reviews(&["VERDICT: FAIL"]),
        &mut MemoryJournal::default(),
    )
    .unwrap();
    let second = advance(
        &document,
        &mut reviews(&["VERDICT: FAIL"]),
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert_eq!(first.checkpoint.plan_digest, second.checkpoint.plan_digest);
    assert_ne!(first.checkpoint.instance_id, second.checkpoint.instance_id);
}

#[test]
fn loop_cancel_and_manual_decisions_close_unanswered_asks() {
    for action in ["cancel", "stop", "accept", "extend"] {
        let mut document = fixture("extend");
        document.nodes[0]
            .loop_body
            .as_mut()
            .unwrap()
            .decision_session = Some("chair-session".into());
        let mut owner = DecisionOwner {
            inner: reviews(&["VERDICT: FAIL"]),
            response: Ok(None),
            closed: vec![],
        };
        let mut journal = MemoryJournal::default();
        let limits = crate::config::GraphLimits::default();
        advance_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &limits,
            &Default::default(),
        )
        .unwrap();
        if action == "cancel" {
            journal.0.as_mut().unwrap().cancelled = true;
            let cancelled = advance_with_budget(
                &document,
                &mut owner,
                &mut journal,
                &limits,
                &Default::default(),
            )
            .unwrap();
            assert_eq!(cancelled.status, GraphStatus::Cancelled);
            let resumed = resume_with_budget(&document, &mut owner, &mut journal, &limits, &Default::default(), &[]).unwrap();
            assert!(resumed.checkpoint.pause_reason.as_ref().unwrap().contains("ask closed; use graph resume"));
        } else {
            let extension = if action == "extend" {
                owner.inner.observations.push_back(terminal_with_result(
                    AttemptOutcome::Succeeded,
                    "VERDICT: PASS",
                ));
                "loop.revise.rounds=2".into()
            } else {
                format!("loop.revise.decision={action}")
            };
            resume_with_budget(
                &document,
                &mut owner,
                &mut journal,
                &limits,
                &Default::default(),
                &[extension],
            )
            .unwrap();
        }
        assert_eq!(owner.closed, [("revise".into(), 1)], "{action}");
    }
}

#[test]
fn loop_inbox_large_extension_cannot_bypass_host_node_limit() {
    let mut document = fixture("exhaust");
    let spec = document.nodes[0].loop_body.as_mut().unwrap();
    spec.decision_session = Some("chair-session".into());
    spec.max_extension_rounds = 1024;
    let mut owner = DecisionOwner {
        inner: reviews(&["VERDICT: FAIL", "VERDICT: FAIL"]),
        response: Ok(Some(LoopDecision::Extend { rounds: 500 })),
        closed: vec![],
    };
    let limits = crate::config::GraphLimits {
        expansion_depth: 2048,
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let paused = advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &limits,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(paused.status, GraphStatus::Paused);
    assert!(
        paused
            .checkpoint
            .pause_reason
            .unwrap()
            .contains("node count")
    );
    assert_eq!(paused.checkpoint.nodes.len(), 5);
    assert!(paused.checkpoint.extensions.is_empty());
    let accepted = resume_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &limits,
        &Default::default(),
        &["loop.revise.decision=accept".into()],
    )
    .unwrap();
    assert!(accepted.checkpoint.successful());
}

#[test]
fn loop_cancelled_ask_cleanup_replays_after_failure_and_crash() {
    struct CleanupOwner {
        inner: DecisionOwner,
        fail: bool,
    }
    impl AttemptOwner for CleanupOwner {
        type Error = NoObservation;
        fn actor(&self) -> String {
            self.inner.actor()
        }
        fn loop_decision(
            &mut self,
            scope: LoopScope<'_>,
        ) -> Result<Option<LoopDecision>, Self::Error> {
            self.inner.loop_decision(scope)
        }
        fn close_loop_ask(&mut self, scope: LoopScope<'_>) -> Result<(), Self::Error> {
            if self.fail {
                Err(NoObservation)
            } else {
                self.inner.close_loop_ask(scope)
            }
        }
        fn observe_or_start(
            &mut self,
            key: &AttemptKey,
            node: NodeInput<'_>,
            route: &GraphRoute,
            access: AccessMode,
            worktree: Option<&str>,
            replay: Replay<'_>,
        ) -> Result<AttemptObservation, Self::Error> {
            self.inner
                .observe_or_start(key, node, route, access, worktree, replay)
        }
    }
    use provenant_os::{clock::SystemClock, fs::SystemPathResolver};
    use std::os::unix::fs::PermissionsExt;
    let temp = crate::test_support::tempdir().unwrap();
    let base = std::fs::canonicalize(temp.path()).unwrap();
    std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("checkout"));
    inputs.state_dir = Some(base.clone());
    let root = StateRoot::resolve(&inputs, &SystemPathResolver).unwrap();
    let project = ProjectId::from_bytes([1; 16]);
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
    let mut document = fixture("extend");
    document.nodes[0]
        .loop_body
        .as_mut()
        .unwrap()
        .decision_session = Some("chair-session".into());
    let limits = crate::config::GraphLimits::default();
    let mut owner = CleanupOwner {
        inner: DecisionOwner {
            inner: reviews(&["VERDICT: FAIL"]),
            response: Ok(None),
            closed: vec![],
        },
        fail: true,
    };
    {
        let mut journal =
            FileJournal::open(&root, project, &document.graph_id, budget, &cancel, &clock).unwrap();
        journal
            .bind(GraphExecution {
                integration_base: None,
                directory: base.clone(),
                document: document.clone(),
            })
            .unwrap();
        let mut paused = advance_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &limits,
            &Default::default(),
        )
        .unwrap()
        .checkpoint;
        paused.cancelled = true;
        journal.save(&paused).unwrap();
    }
    assert!(
        FileJournal::pending_execution(&root, project, &document.graph_id)
            .unwrap()
            .is_some(),
        "crash after cancel fence must not lose cleanup"
    );
    {
        let mut journal =
            FileJournal::open(&root, project, &document.graph_id, budget, &cancel, &clock).unwrap();
        let run = advance_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &limits,
            &Default::default(),
        )
        .unwrap();
        assert_eq!(run.status, GraphStatus::Cancelled);
        assert!(
            run.checkpoint
                .nodes
                .iter()
                .find(|n| n.id == "revise")
                .unwrap()
                .loop_inbox_error
                .is_some()
        );
    }
    assert!(
        FileJournal::pending_execution(&root, project, &document.graph_id)
            .unwrap()
            .is_some()
    );
    owner.fail = false;
    {
        let mut journal =
            FileJournal::open(&root, project, &document.graph_id, budget, &cancel, &clock).unwrap();
        assert_eq!(
            advance_with_budget(
                &document,
                &mut owner,
                &mut journal,
                &limits,
                &Default::default()
            )
            .unwrap()
            .status,
            GraphStatus::Cancelled
        );
    }
    assert_eq!(owner.inner.closed, [("revise".into(), 1)]);
    assert!(
        FileJournal::pending_execution(&root, project, &document.graph_id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn loop_inbox_recovery_clears_stale_unavailable_pause_durably() {
    let mut document = fixture("extend");
    document.nodes[0]
        .loop_body
        .as_mut()
        .unwrap()
        .decision_session = Some("chair-session".into());
    let mut owner = DecisionOwner {
        inner: reviews(&["VERDICT: FAIL"]),
        response: Err(NoObservation),
        closed: vec![],
    };
    let mut journal = MemoryJournal::default();
    let limits = crate::config::GraphLimits::default();
    advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &limits,
        &Default::default(),
    )
    .unwrap();
    owner.response = Ok(None);
    let recovered = advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &limits,
        &Default::default(),
    )
    .unwrap();
    assert!(
        !recovered
            .checkpoint
            .pause_reason
            .as_ref()
            .unwrap()
            .contains("unavailable")
    );
    assert!(
        recovered
            .checkpoint
            .nodes
            .iter()
            .all(|n| n.loop_inbox_error.is_none())
    );
    assert_eq!(journal.0.as_ref().unwrap(), &recovered.checkpoint);
}

#[test]
fn loop_terminal_decisions_work_after_host_limits_fall() {
    for decision in ["stop", "accept"] {
        let document = fixture("exhaust");
        let mut journal = MemoryJournal::default();
        advance(
            &document,
            &mut reviews(&["VERDICT: FAIL", "VERDICT: FAIL"]),
            &mut journal,
        )
        .unwrap();
        let limits = crate::config::GraphLimits {
            nodes: 1,
            expansion_depth: 1,
            ..Default::default()
        };
        let mut owner = reviews(&[]);
        let run = resume_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &limits,
            &Default::default(),
            &[format!("loop.revise.decision={decision}")],
        )
        .unwrap();
        assert_eq!(run.status, GraphStatus::Complete);
        assert_eq!(run.checkpoint.successful(), decision == "accept");
        assert!(owner.calls.is_empty());
    }
}

#[test]
fn loop_refused_resume_preserves_cancellation_fence_and_status() {
    let document = fixture("exhaust");
    let mut journal = MemoryJournal::default();
    advance(
        &document,
        &mut reviews(&["VERDICT: FAIL", "VERDICT: FAIL"]),
        &mut journal,
    )
    .unwrap();
    journal.0.as_mut().unwrap().cancelled = true;
    let run = extension(
        &document,
        &mut reviews(&[]),
        &mut journal,
        &["loop.revise.rounds=3".into()],
    );
    assert_eq!(run.status, GraphStatus::Cancelled);
    assert!(run.checkpoint.cancelled);
    assert!(run.checkpoint.extensions.is_empty());
}

#[test]
fn loop_close_success_does_not_hide_another_pending_close_failure() {
    struct Owner {
        inner: DecisionOwner,
        fail_first: bool,
    }
    impl AttemptOwner for Owner {
        type Error = NoObservation;
        fn actor(&self) -> String {
            self.inner.actor()
        }
        fn loop_decision(
            &mut self,
            scope: LoopScope<'_>,
        ) -> Result<Option<LoopDecision>, Self::Error> {
            self.inner.loop_decision(scope)
        }
        fn close_loop_ask(&mut self, scope: LoopScope<'_>) -> Result<(), Self::Error> {
            if scope.rounds == 1 && self.fail_first {
                Err(NoObservation)
            } else {
                self.inner.close_loop_ask(scope)
            }
        }
        fn observe_or_start(
            &mut self,
            key: &AttemptKey,
            node: NodeInput<'_>,
            route: &GraphRoute,
            access: AccessMode,
            worktree: Option<&str>,
            replay: Replay<'_>,
        ) -> Result<AttemptObservation, Self::Error> {
            self.inner
                .observe_or_start(key, node, route, access, worktree, replay)
        }
    }
    let mut document = fixture("exhaust");
    document.nodes[0]
        .loop_body
        .as_mut()
        .unwrap()
        .decision_session = Some("chair-session".into());
    let mut owner = Owner {
        inner: DecisionOwner {
            inner: reviews(&["VERDICT: FAIL", "VERDICT: FAIL"]),
            response: Ok(None),
            closed: vec![],
        },
        fail_first: true,
    };
    let limits = crate::config::GraphLimits::default();
    let mut journal = MemoryJournal::default();
    let mut checkpoint = advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &limits,
        &Default::default(),
    )
    .unwrap()
    .checkpoint;
    let wrapper = checkpoint
        .nodes
        .iter_mut()
        .find(|n| n.id == "revise")
        .unwrap();
    wrapper.loop_asks.insert(
        0,
        LoopAsk {
            rounds: 1,
            session: "chair-session".into(),
            resolved: true,
            closed: false,
        },
    );
    checkpoint.cancelled = true;
    close_resolved_loop_asks(&mut checkpoint, &mut owner, &mut journal).unwrap();
    let wrapper = checkpoint.nodes.iter().find(|n| n.id == "revise").unwrap();
    assert!(!wrapper.loop_asks[0].closed);
    assert!(wrapper.loop_asks[1].closed);
    assert!(wrapper.loop_inbox_error.is_some());
    owner.fail_first = false;
    close_resolved_loop_asks(&mut checkpoint, &mut owner, &mut journal).unwrap();
    let wrapper = checkpoint.nodes.iter().find(|n| n.id == "revise").unwrap();
    assert!(wrapper.loop_asks.iter().all(|ask| ask.closed));
    assert!(wrapper.loop_inbox_error.is_none());
}

#[test]
fn loop_decision_recovery_resets_pause_even_after_prior_ask_cleanup() {
    let mut document = fixture("exhaust");
    document.nodes[0]
        .loop_body
        .as_mut()
        .unwrap()
        .decision_session = Some("chair-session".into());
    let mut owner = DecisionOwner {
        inner: reviews(&["VERDICT: FAIL", "VERDICT: FAIL"]),
        response: Err(NoObservation),
        closed: vec![],
    };
    let mut journal = MemoryJournal::default();
    let limits = crate::config::GraphLimits::default();
    let mut checkpoint = advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &limits,
        &Default::default(),
    )
    .unwrap()
    .checkpoint;
    checkpoint
        .nodes
        .iter_mut()
        .find(|n| n.id == "revise")
        .unwrap()
        .loop_asks
        .insert(
            0,
            LoopAsk {
                rounds: 1,
                session: "chair-session".into(),
                resolved: true,
                closed: false,
            },
        );
    close_resolved_loop_asks(&mut checkpoint, &mut owner, &mut journal).unwrap();
    owner.response = Ok(None);
    let run = advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &limits,
        &Default::default(),
    )
    .unwrap();
    assert!(!run.checkpoint.pause_reason.unwrap().contains("unavailable"));
}
