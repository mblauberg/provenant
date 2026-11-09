use super::*;

fn advance(
    document: &GraphDocument,
    owner: &mut FakeOwner,
    journal: &mut MemoryJournal,
) -> Result<GraphRun, GraphError<NoObservation, Infallible>> {
    let limits = crate::config::GraphLimits {
        // Full templated review stages extend the repair DAG beyond the old
        // direct lens fanout. Exercise the shipped host depth allowance.
        expansion_depth: 64,
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

/// The inbox reply that carries a loop decision.
fn loop_reply(decision: &LoopDecision) -> AskReply {
    match decision {
        LoopDecision::Stop => reply(Some("stop"), None),
        LoopDecision::Accept => reply(Some("accept"), None),
        LoopDecision::Extend { rounds } => reply(
            None,
            Some(&format!(r#"{{"decision":"extend","rounds":{rounds}}}"#)),
        ),
    }
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

/// A host ceiling of four, so a third round is refused.
fn tight_limits() -> crate::config::GraphLimits {
    crate::config::GraphLimits {
        expansion_depth: 4,
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
        &tight_limits(),
        &Default::default(),
        values,
    )
    .unwrap()
}

#[test]
fn loop_composite_review_pins_each_repaired_commit() {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version":1,"graph_id":"repair-composite","nodes":[{
            "id":"repair","kind":"loop","access":"read_only",
            "loop":{"max_rounds":2,"until":"accepted","body":[
                {"id":"writer","access":"worktree_write","worktree":"/fixture/writer", "prompt":"repair","route":{"alias":"senior"}},
                {"id":"council","kind":"composite","access":"read_only", "depends_on":["writer"],
                    "source":{"type":"node","node":"writer"}, "composite":{"nodes":[
                        {"id":"lens","access":"read_only","prompt":"review pinned repair", "route":{"alias":"senior"},
                            "depends_on":["writer"],"source":{"type":"node","node":"writer"}}
                    ],"exports":[{"node":"lens","outputs":["verdict"]}]}},
                {"id":"accepted","kind":"gate","access":"read_only","depends_on":["council"],
                    "inputs":[{"node":"council","outputs":["verdict"]}],
                    "condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"}}
            ]}
        }]
    })).unwrap();
    let commit1 = "1".repeat(40);
    let commit2 = "2".repeat(40);
    let writer = |commit: &str| AttemptObservation::Terminal {
        attempt_ref: None, outcome: AttemptOutcome::Succeeded, launched: true,
        result: None, result_warning: None, result_path: None, branch: None,
        commit: Some(commit.to_owned()),
    };
    let mut owner = FakeOwner {
        observations: [writer(&commit1), terminal_with_result(AttemptOutcome::Succeeded, "VERDICT: FAIL"),
            writer(&commit2), terminal_with_result(AttemptOutcome::Succeeded, "VERDICT: PASS")].into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(run.checkpoint.successful());
    assert_eq!(owner.source_commits, vec![Some("a".repeat(40)), Some(commit1), Some("a".repeat(40)), Some(commit2)]);
    assert_eq!(owner.calls[1].0.node_id, "repair--r1--council--lens");
    assert_eq!(owner.calls[3].0.node_id, "repair--r2--council--lens");
    assert_eq!(advance(&document, &mut FakeOwner::default(), &mut journal).unwrap().checkpoint, run.checkpoint);
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
    assert_eq!(run.checkpoint.pause_code.as_deref(), Some("loop_paused"));
    let gate = run.checkpoint.nodes.iter().find(|node| node.id == "revise").unwrap();
    assert_eq!(gate.loop_cause, Some(NodeDecisionCause::LoopPaused));
    let decoded: GraphCheckpoint = serde_json::from_slice(&serde_json::to_vec(&run.checkpoint).unwrap()).unwrap();
    assert_eq!(decoded, run.checkpoint);
    let mut forged = run.checkpoint.clone();
    forged.nodes.iter_mut().find(|node| node.id == "revise").unwrap().loop_cause = None;
    let mut forged_journal = MemoryJournal(Some(forged));
    assert!(matches!(advance(&document, &mut reviews(&[]), &mut forged_journal), Err(GraphError::BadCheckpoint { .. })));
    assert!(!run.checkpoint.successful());
    assert_eq!(owner.deliveries, 0);
    assert_eq!(run.checkpoint.ask_actor.as_deref(), Some("chair"));
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
    assert_eq!(run.checkpoint.pause_code.as_deref(), Some("structural_limit"));
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
fn loop_writers_keep_one_worktree_when_the_body_does_not_deliver() {
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
        let mut gate = gate_node(GateCondition::Verdict {extra: Default::default(),
            expected: "VERDICT: PASS".into(),
            threshold: VerdictThreshold::All,
        });
        gate.id = "initial".into();
        gate.depends_on = vec!["initial-review".into()];
        gate.inputs = vec![GraphInput { required_outputs: Vec::new(),extra: Default::default(),
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
        fn node_ask(&mut self, scope: AskScope<'_>) -> Result<Option<AskReply>, NoObservation> {
            self.targets.push((
                scope.ask.rounds.unwrap(),
                scope.ask.session.clone(),
                scope.actor.into(),
            ));
            Ok(self.decision.take().as_ref().map(loop_reply))
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
    assert_eq!(paused.checkpoint.pause_code.as_deref(), Some("budget_exceeded"));
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
        answer: AskReply,
    }
    impl AttemptOwner for Owner {
        type Error = NoObservation;
        fn actor(&self) -> String {
            self.inner.actor()
        }
        fn node_ask(&mut self, _: AskScope<'_>) -> Result<Option<AskReply>, NoObservation> {
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
        reply(None, Some("extend by two")),
        loop_reply(&LoopDecision::Extend { rounds: 6 }),
        loop_reply(&LoopDecision::Extend { rounds: 2000 }),
        loop_reply(&LoopDecision::Extend { rounds: usize::MAX }),
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
        assert_eq!(paused.checkpoint.pause_code.as_deref(), Some("invalid_loop_decision"));
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
            .bind(GraphExecution {review_base: None, extra: Default::default(),
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
    fn node_ask(&mut self, _: AskScope<'_>) -> Result<Option<AskReply>, Self::Error> {
        match &self.response {
            Ok(decision) => Ok(decision.as_ref().map(loop_reply)),
            Err(_) => Err(NoObservation),
        }
    }
    fn close_node_ask(&mut self, scope: AskScope<'_>) -> Result<(), Self::Error> {
        self.closed.push((
            scope.ask.node.clone().unwrap_or_default(),
            scope.ask.rounds.unwrap_or_default(),
        ));
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
fn loop_host_ceiling_rejects_third_round_before_saving_and_can_accept() {
    let document = fixture("exhaust");
    let limits = tight_limits();
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
    assert_eq!(rejected.checkpoint.pause_code.as_deref(), Some("invalid_loop_decision"));
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
fn loop_inbox_extension_respects_host_ceiling_and_closes_rejected_ask() {
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
    let limits = tight_limits();
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
    assert_eq!(paused.checkpoint.pause_code.as_deref(), Some("invalid_loop_decision"));
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
    assert_eq!(paused.checkpoint.pause_code.as_deref(), Some("invalid_loop_decision"));
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
        assert_eq!(paused.checkpoint.nodes.iter().find(|node| node.id == name).unwrap().loop_pause_code.as_deref(), Some("loop_paused"));
    }
    let one = extension(
        &document,
        &mut reviews(&[]),
        &mut journal,
        &["loop.revise.decision=accept".into()],
    );
    assert_eq!(one.status, GraphStatus::Paused);
    assert!(one.checkpoint.nodes.iter().find(|node| node.id == "revise").unwrap().loop_pause_code.is_none());
    assert_eq!(one.checkpoint.nodes.iter().find(|node| node.id == "second").unwrap().loop_pause_code.as_deref(), Some("loop_paused"));
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
            _ => document.nodes[0].id = "x".repeat(198),
        }
        let expected = match case {
            0 | 1 => "loop body cannot nest loops or route on_fail, and may only deliver by integrate:",
            2 => "loop writers must share one worktree",
            3 => "previous. is reserved for prior round inputs",
            _ => "expanded loop node id exceeds 200 bytes",
        };
        assert_eq!(
            validate_plan(&document).unwrap_err(),
            expected,
            "case {case}"
        );
    }
}

fn integrating_repair(delivery: Option<&str>) -> GraphDocument {
    let mut writer = serde_json::json!({
        "id":"fix","access":"worktree_write","worktree":"/fixture/batch",
        "prompt":"repair","route":{"alias":"senior"}
    });
    if let Some(delivery) = delivery {
        writer["delivery"] = delivery.into();
    }
    serde_json::from_value(serde_json::json!({
        "schema_version":1,"graph_id":"integrating-repair","integration_branch":"batch/graph",
        "nodes":[
            {"id":"principal","access":"worktree_write","worktree":"/fixture/batch",
                "prompt":"refactor","route":{"alias":"senior"},"delivery":"integrate:batch/graph"},
            {"id":"repair","kind":"loop","access":"read_only","depends_on":["principal"],
                "loop":{"max_rounds":2,"until":"accepted","body":[
                    writer,
                    {"id":"review","access":"read_only","depends_on":["fix"],
                        "prompt":"review","route":{"alias":"senior"},
                        "source":{"type":"node","node":"fix"}},
                    {"id":"accepted","kind":"gate","access":"read_only","depends_on":["review"],
                        "inputs":[{"node":"review","outputs":["verdict"]}],
                        "condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"}}
                ]}}
        ]
    }))
    .unwrap()
}

fn commit_terminal(commit: &str) -> AttemptObservation {
    AttemptObservation::Terminal {
        attempt_ref: None,
        outcome: AttemptOutcome::Succeeded,
        launched: true,
        result: None,
        result_warning: None,
        result_path: None,
        branch: None,
        commit: Some(commit.to_owned()),
    }
}

#[test]
fn loop_body_integrate_requires_the_graph_integration_branch() {
    const REASON: &str =
        "loop body integrate: requires graph integration_branch equal to the node target";
    let mut missing = integrating_repair(Some("integrate:batch/graph"));
    missing.integration_branch = None;
    // The principal's own integrate: also needs the branch, so drop it.
    missing.nodes[0].delivery = None;
    assert_eq!(validate_plan(&missing).unwrap_err(), REASON);
    let mut other = integrating_repair(Some("integrate:batch/other"));
    assert_eq!(validate_plan(&other).unwrap_err(), REASON);
    other.integration_branch = Some("batch/other".into());
    // The principal still targets the graph branch, so the flat check refuses.
    assert_eq!(
        validate_plan(&other).unwrap_err(),
        "node principal: node integration target differs from graph integration branch"
    );
}

#[test]
fn loop_body_delivers_only_by_integrate() {
    use provenant_records::run::DeliveryMode;
    validate_plan(&integrating_repair(Some("integrate:batch/graph"))).unwrap();
    validate_plan(&integrating_repair(None)).unwrap();
    for refused in ["push", "pr"] {
        assert_eq!(
            validate_plan(&integrating_repair(Some(refused))).unwrap_err(),
            "loop body cannot nest loops or route on_fail, and may only deliver by integrate:",
            "{refused}"
        );
    }
    // The accepted mode reaches every expanded round.
    let document = integrating_repair(Some("integrate:batch/graph"));
    let expanded = document.expanded(&Default::default()).unwrap();
    let delivering: Vec<_> = expanded
        .nodes
        .iter()
        .filter(|node| node.delivery.is_some())
        .map(|node| (node.id.as_str(), node.delivery.clone()))
        .collect();
    let integrate = Some(DeliveryMode::Integrate("batch/graph".into()));
    assert_eq!(
        delivering,
        [
            ("principal", integrate.clone()),
            ("repair--r1--fix", integrate.clone()),
            ("repair--r2--fix", integrate)
        ]
    );
}

#[test]
fn loop_rounds_integrate_from_the_head_the_previous_round_delivered() {
    let document = integrating_repair(Some("integrate:batch/graph"));
    let (c0, c1, c2) = ("a".repeat(40), "b".repeat(40), "c".repeat(40));
    let mut owner = FakeOwner {
        observations: [
            commit_terminal(&c0),
            commit_terminal(&c1),
            terminal_with_result(AttemptOutcome::Succeeded, "VERDICT: FAIL"),
            commit_terminal(&c2),
            terminal_with_result(AttemptOutcome::Succeeded, "VERDICT: PASS"),
        ]
        .into(),
        // Each round starts when the target is at the previous delivery.
        integration_heads: [c0.clone(), c1.clone()].into(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert!(run.checkpoint.successful(), "{:?}", run.checkpoint);
    let fix: Vec<_> = owner
        .calls
        .iter()
        .zip(&owner.fast_forward_only)
        .filter(|((key, _), _)| key.node_id.ends_with("--fix") || key.node_id == "principal")
        .map(|((key, _), flag)| (key.node_id.as_str(), *flag))
        .collect();
    // Only loop rounds are restricted to fast-forward; the principal may merge.
    assert_eq!(
        fix,
        [
            ("principal", false),
            ("repair--r1--fix", true),
            ("repair--r2--fix", true)
        ]
    );
    assert!(owner.integration_heads.is_empty());
}

#[test]
fn loop_stops_when_the_target_moved_after_the_previous_delivery() {
    let document = integrating_repair(Some("integrate:batch/graph"));
    let (c0, c1) = ("a".repeat(40), "b".repeat(40));
    let mut owner = FakeOwner {
        observations: [
            commit_terminal(&c0),
            commit_terminal(&c1),
            terminal_with_result(AttemptOutcome::Succeeded, "VERDICT: FAIL"),
        ]
        .into(),
        // Someone else advanced the target after round one delivered.
        integration_heads: [c0, "d".repeat(40)].into(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert!(!run.checkpoint.successful());
    let second = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "repair--r2--fix")
        .unwrap();
    assert_eq!(second.state, NodeState::Failed);
    assert_eq!(
        second.advance_error_code.as_deref(),
        Some("integration_head_moved")
    );
    assert!(second.attempts.is_empty());
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
            assert_eq!(resumed.checkpoint.pause_code.as_deref(), Some("loop_ask_closed"));
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
    assert_eq!(paused.checkpoint.pause_code.as_deref(), Some("invalid_loop_decision"));
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
        fn node_ask(&mut self, scope: AskScope<'_>) -> Result<Option<AskReply>, Self::Error> {
            self.inner.node_ask(scope)
        }
        fn close_node_ask(&mut self, scope: AskScope<'_>) -> Result<(), Self::Error> {
            if self.fail {
                Err(NoObservation)
            } else {
                self.inner.close_node_ask(scope)
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
            .bind(GraphExecution {review_base: None, extra: Default::default(),
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
                .asks
                .iter()
                .any(|ask| ask.inbox_error.is_some())
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
    assert_eq!(recovered.checkpoint.pause_code.as_deref(), Some("loop_paused"));
    assert!(
        recovered
            .checkpoint
            .asks
            .iter()
            .all(|ask| ask.inbox_error.is_none())
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
        fn node_ask(&mut self, scope: AskScope<'_>) -> Result<Option<AskReply>, Self::Error> {
            self.inner.node_ask(scope)
        }
        fn close_node_ask(&mut self, scope: AskScope<'_>) -> Result<(), Self::Error> {
            if scope.ask.rounds == Some(1) && self.fail_first {
                Err(NoObservation)
            } else {
                self.inner.close_node_ask(scope)
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
    let mut first = asks::loop_ask(&checkpoint.graph_id, "revise", 1, "chair-session");
    first.resolved = true;
    checkpoint.asks.insert(0, first);
    checkpoint.cancelled = true;
    close_resolved_asks(&mut checkpoint, &mut owner, &mut journal).unwrap();
    assert!(!checkpoint.asks[0].closed);
    assert!(checkpoint.asks[0].inbox_error.is_some());
    assert!(checkpoint.asks[1].closed);
    owner.fail_first = false;
    close_resolved_asks(&mut checkpoint, &mut owner, &mut journal).unwrap();
    assert!(checkpoint.asks.iter().all(|ask| ask.closed));
    assert!(checkpoint.asks.iter().all(|ask| ask.inbox_error.is_none()));
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
    let mut first = asks::loop_ask(&checkpoint.graph_id, "revise", 1, "chair-session");
    first.resolved = true;
    checkpoint.asks.insert(0, first);
    close_resolved_asks(&mut checkpoint, &mut owner, &mut journal).unwrap();
    owner.response = Ok(None);
    let run = advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &limits,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(run.checkpoint.pause_code.as_deref(), Some("loop_paused"));
}

#[test]
fn loop_reply_rejects_zero_unknown_fields_and_unstructured_text() {
    for text in [
        "{\"decision\":\"extend\",\"rounds\":0}",
        "{\"decision\":\"extend\",\"rounds\":-1}",
        "{\"decision\":\"extend\",\"rounds\":2,\"other\":true}",
        "{\"decision\":\"accept\",\"rounds\":2}",
        "extend by two",
    ] {
        let error = LoopDecision::parse(None, Some(text)).unwrap_err();
        assert_eq!(error.code(), "invalid_loop_decision", "{text}");
        assert_eq!(error.to_string(), asks::LOOP_ANSWER_HELP);
    }
    assert_eq!(LoopDecision::parse(Some("stop"), Some("{}")).unwrap_err().code(), "invalid_loop_decision");
    assert_eq!(
        LoopDecision::parse(None, Some("{\"decision\":\"extend\",\"rounds\":2}")),
        Ok(LoopDecision::Extend { rounds: 2 })
    );
}

/// Repair writer fixtures report the checked root-cause item they handled.
fn repair_reviews(results: &[&str]) -> FakeOwner {
    let reports: Vec<String> = results.iter().map(|result| {
        if result.starts_with("fix") && !result.contains("```findings") {
            format!("{result}\n```findings\n{}\n```", serde_json::json!({
                "schema_version":1,"findings":[],"repair_items":[{
                    "id":"constraint","status":"fixed","reason":"The default-path defect is repaired"
                }]
            }))
        } else {
            (*result).to_owned()
        }
    }).collect();
    reviews(&reports.iter().map(String::as_str).collect::<Vec<_>>())
}

fn review_repair_document() -> GraphDocument {
    serde_json::from_value(serde_json::json!({
        "schema_version":1,"graph_id":"review-repair","nodes":[
            {"id":"initial","access":"read_only","prompt":"review", "route":{"alias":"senior"},"require_findings":true},
            {"id":"repair","kind":"loop","access":"read_only","depends_on":["initial"],"on_fail":"design",
                "inputs":[{"node":"initial","outputs":["findings","verdict"]}],
                "loop":{"review_repair":true,"max_rounds":3,"until":"accepted","body":[
                    {"id":"fix","access":"worktree_write","worktree":"/fixture/writer","prompt":"repair", "route":{"alias":"senior"}},
                    {"id":"check","access":"read_only","depends_on":["fix"],"prompt":"delta", "route":{"alias":"senior"},"require_findings":true},
                    {"id":"accepted","kind":"gate","access":"read_only","depends_on":["check"],"inputs":[{"node":"check","outputs":["verdict"]}],
                        "condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"}}
                ]}},
            {"id":"design","access":"read_only","depends_on":["repair"],"prompt":"principal", "route":{"alias":"principal"}}
        ]
    })).unwrap()
}

fn repair_finding(design: bool) -> String {
    format!(
        "```findings\n{}\n```\nVERDICT: FAIL",
        serde_json::json!({"schema_version":1,"findings":[{
            "id":"F1","lens":"correctness","severity":"high","file":"a.rs","line":1,
            "summary":"constraint","status":"open","evidence":"counterexample","proposed_fix":"repair",
            "design_defect":design,
            "trigger":{"sequence":"invoke the default operation","reach":"default_path","observed":true},
            "triage":{"decision":"confirmed","reason":"counterexample reproduced","worth_fixing":true,
                "root_cause":"constraint","blocking_class":"default_path_defect","shared_flaw":false}
        }]})
    )
}

#[test]
fn clean_initial_review_starts_no_repair_attempt() {
    let document = review_repair_document();
    let mut owner =
        repair_reviews(&["```findings\n{\"schema_version\":1,\"findings\":[]}\n```\nVERDICT: PASS"]);
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(run.checkpoint.successful());
    assert_eq!(owner.calls.len(), 1);
    assert_eq!(
        run.checkpoint
            .nodes
            .iter()
            .find(|node| node.id == "repair")
            .unwrap()
            .outputs
            .as_ref()
            .unwrap()
            .verdict
            .as_deref(),
        Some("PASS")
    );
    assert_eq!(
        advance(&document, &mut FakeOwner::default(), &mut journal)
            .unwrap()
            .checkpoint,
        run.checkpoint
    );
}

#[test]
fn recurring_review_finding_escalates_before_second_repair() {
    let document = review_repair_document();
    let finding = repair_finding(false);
    let mut owner = repair_reviews(&[&finding, "fixed", &finding, "root constraint and options"]);
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(run.checkpoint.successful());
    assert_eq!(owner.calls.len(), 4);
    assert_eq!(owner.calls.last().unwrap().0.node_id, "design");
    let repair = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "repair")
        .unwrap();
    assert_eq!(
        repair.gate.as_ref().unwrap().reason.as_deref(),
        Some("recurring finding after repair")
    );
    assert_eq!(
        advance(&document, &mut FakeOwner::default(), &mut journal)
            .unwrap()
            .checkpoint,
        run.checkpoint
    );
}

#[test]
fn concrete_design_defect_escalates_without_repair_attempt() {
    let document = review_repair_document();
    let finding = repair_finding(true);
    let mut owner = repair_reviews(&[&finding, "root constraint and options"]);
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert!(run.checkpoint.successful());
    assert_eq!(owner.calls.len(), 2);
    assert_eq!(owner.calls.last().unwrap().0.node_id, "design");
}

#[test]
fn checked_shared_flaw_at_first_round_routes_to_design() {
    let document = review_repair_document();
    let finding = repair_finding(false).replace("\"shared_flaw\":false", "\"shared_flaw\":true");
    let mut owner = repair_reviews(&[&finding, "structural alternative"]);
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert!(run.checkpoint.successful());
    assert_eq!(owner.calls.len(), 2);
    assert_eq!(owner.calls.last().unwrap().0.node_id, "design");
    assert_eq!(run.checkpoint.nodes.iter().find(|node| node.id == "repair")
        .unwrap().gate.as_ref().unwrap().reason.as_deref(),
        Some("checked findings share a design flaw"));
}

#[test]
fn writer_design_cannot_hold_stops_round_before_delta_review() {
    let document = review_repair_document();
    let finding = repair_finding(false);
    let report = "```findings\n{\"schema_version\":1,\"findings\":[],\"repair_items\":[{\"id\":\"constraint\",\"status\":\"design-cannot-hold\",\"reason\":\"the representation loses identity\",\"alternative\":\"retain identity in the owner\"}]}\n```";
    let mut owner = repair_reviews(&[&finding, report, "structural alternative"]);
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert!(run.checkpoint.successful());
    assert_eq!(owner.calls.len(), 3);
    assert_eq!(owner.calls.last().unwrap().0.node_id, "design");
    assert!(!owner.calls.iter().any(|call| call.0.node_id.ends_with("--check")));
    assert_eq!(run.checkpoint.nodes.iter().find(|node| node.id == "repair")
        .unwrap().gate.as_ref().unwrap().reason.as_deref(),
        Some("writer reports design cannot hold"));
}

#[test]
fn repair_cycle_budget_cannot_exceed_three_or_be_accepted_without_review() {
    let mut document = review_repair_document();
    assert!(
        loops::validate_extension(&document, "loop.repair.rounds=4", &Default::default()).is_err()
    );
    assert!(
        loops::validate_extension(
            &document,
            "loop.repair.decision=accept",
            &Default::default()
        )
        .is_err()
    );
    document.nodes[1].loop_body.as_mut().unwrap().max_rounds = 4;
    assert!(loops::expanded_node_count(&document, &[]).is_err());
}

#[test]
fn delta_review_runs_only_the_lens_that_raised_findings() {
    let mut document = review_repair_document();
    document.context = Some(serde_json::from_value(serde_json::json!({"risk_tier":"substantial"})).unwrap());
    let check = serde_json::from_value(serde_json::json!({
        "id":"check","kind":"composite","access":"read_only","depends_on":["fix"],
        "composite":{"nodes":[
            {"id":"correctness","repair_lens":"correctness","repair_risk_assessment":true,"access":"read_only","prompt":"correctness delta","route":{"alias":"senior"},"require_findings":true},
            {"id":"security","repair_lens":"security","repair_risk_assessment":true,"access":"read_only","prompt":"security delta","route":{"alias":"senior"},"require_findings":true}
        ],"exports":[{"node":"correctness","outputs":["findings","verdict"]},{"node":"security","outputs":["findings"]}]}
    })).unwrap();
    document.nodes[1].loop_body.as_mut().unwrap().body[1] = check;
    let finding = repair_finding(false);
    let pass = "```findings\n{\"schema_version\":1,\"risk_tier\":\"substantial\",\"findings\":[]}\n```\nVERDICT: PASS";
    let mut owner = repair_reviews(&[&finding, "fixed", pass]);
    if let AttemptObservation::Terminal { commit, .. } = &mut owner.observations[1] {
        *commit = Some("a".repeat(40));
    }
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert!(run.checkpoint.successful());
    assert_eq!(owner.calls.len(), 3);
    assert_eq!(owner.repair_growth_calls, 1);
    assert_eq!(owner.repair_panel_calls, 0);
    assert!(
        owner
            .calls
            .last()
            .unwrap()
            .0
            .node_id
            .ends_with("check--correctness")
    );
    let skipped = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "repair--r1--check--security")
        .unwrap();
    assert_eq!(skipped.state, NodeState::Skipped);
    assert!(skipped.attempts.is_empty());
    assert_eq!(
        advance(&document, &mut FakeOwner::default(), &mut journal)
            .unwrap()
            .checkpoint,
        run.checkpoint
    );
}

#[test]
fn later_repair_round_uses_latest_raising_lenses() {
    let mut document = review_repair_document();
    let check = serde_json::from_value(serde_json::json!({
        "id":"check","kind":"composite","access":"read_only","depends_on":["fix"],
        "composite":{"min_successful":1,"reduce_verdict":true,"nodes":[
            {"id":"correctness","repair_lens":"correctness","access":"read_only","prompt":"correctness delta","route":{"alias":"senior"},"require_findings":true},
            {"id":"security","repair_lens":"security","access":"read_only","prompt":"security delta","route":{"alias":"senior"},"require_findings":true}
        ],"exports":[{"node":"correctness","outputs":["findings","result"]},{"node":"security","outputs":["findings","result"]}]}
    })).unwrap();
    document.nodes[1].loop_body.as_mut().unwrap().body[1] = check;
    let initial = repair_finding(false);
    let next = initial.replace("F1", "F2").replace("correctness", "security");
    let pass = "```findings\n{\"schema_version\":1,\"findings\":[]}\n```\nVERDICT: PASS";
    let mut owner = repair_reviews(&[&initial, "fix one", &next, "fix two", pass]);
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert!(run.checkpoint.successful());
    let calls: Vec<_> = owner.calls.iter().map(|call| call.0.node_id.as_str()).collect();
    assert_eq!(calls, ["initial", "repair--r1--fix", "repair--r1--check--correctness", "repair--r2--fix", "repair--r2--check--security"]);
}

#[test]
fn risk_or_scope_growth_cannot_pass_the_original_panel() {
    for field in ["risk_grew", "scope_grew"] {
        let document = review_repair_document();
        let initial = repair_finding(false);
        let expanded = initial
            .replace("\"design_defect\":false", &format!("\"design_defect\":false,\"{field}\":true"))
            .replace("VERDICT: FAIL", "VERDICT: PASS");
        let mut owner = repair_reviews(&[&initial, "fix", &expanded, "principal options"]);
        let mut journal = MemoryJournal::default();
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        let gate = run.checkpoint.nodes.iter().find(|n| n.id == "repair--r1--accepted").unwrap();
        assert_eq!(gate.state, NodeState::Failed);
        assert!(gate.gate.as_ref().unwrap().reason.as_deref().unwrap().contains("full panel replan required"));
        assert_eq!(owner.calls.last().unwrap().0.node_id, "design");
        assert_eq!(advance(&document, &mut FakeOwner::default(), &mut journal).unwrap().checkpoint, run.checkpoint);
    }
}

#[test]
fn omitted_delta_risk_assessment_cannot_accept_repair() {
    let mut document = review_repair_document();
    document.nodes[1].loop_body.as_mut().unwrap().max_rounds = 1;
    document.nodes[1].loop_body.as_mut().unwrap().body[1].extra.insert("repair_risk_assessment".into(), true.into());
    let initial = repair_finding(false);
    let pass = "```findings\n{\"schema_version\":1,\"findings\":[]}\n```\nVERDICT: PASS";
    let mut owner = repair_reviews(&[&initial, "fix", pass, "principal options"]);
    if let AttemptObservation::Terminal { commit, .. } = &mut owner.observations[1] { *commit = Some("a".repeat(40)); }
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    let gate = run.checkpoint.nodes.iter().find(|node| node.id == "repair--r1--accepted").unwrap();
    assert_eq!(gate.state, NodeState::Failed);
    assert!(gate.gate.as_ref().unwrap().reason.as_deref().unwrap().contains("omitted a valid risk tier"));
}

#[test]
fn repair_growth_resolves_and_runs_a_fresh_panel() {
    for (field, reviewer_passes) in [("risk_grew", true), ("scope_grew", true), ("host_detected", true), ("assessed_tier", true)] {
        let mut document = review_repair_document();
        document.context = Some(serde_json::from_value(serde_json::json!({"risk_tier":"substantial"})).unwrap());
        let initial = repair_finding(false);
        let growth = if field == "assessed_tier" { "```findings\n{\"schema_version\":1,\"risk_tier\":\"crucial\",\"findings\":[]}\n```\nVERDICT: PASS".to_owned() } else if field == "host_detected" { initial.clone() } else { initial.replace("\"design_defect\":false", &format!("\"design_defect\":false,\"{field}\":true")) };
        let pass = "```findings\n{\"schema_version\":1,\"findings\":[]}\n```\nVERDICT: PASS";
        let fail = "```findings\n{\"schema_version\":1,\"findings\":[]}\n```\nVERDICT: FAIL";
        let mut owner = repair_reviews(&[&initial, "fix", &growth, pass, if reviewer_passes { pass } else { fail }, pass]);
        owner.repair_panel = Some(vec!["correctness".into(), "security".into()]);
        owner.repair_growth = field == "host_detected";
        if let AttemptObservation::Terminal { commit, .. } = &mut owner.observations[1] {
            *commit = Some("a".repeat(40));
        }
        let mut journal = MemoryJournal::default();
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        assert_eq!(owner.repair_panel_calls, 1);
        assert_eq!(owner.repair_growth_calls, 1);
        assert_eq!(owner.calls.iter().filter(|call| call.0.node_id.contains("--panel--")).count(), 3, "calls: {:?}; gate: {:?}", owner.calls, run.checkpoint.nodes.iter().find(|node| node.id == "repair--r1--accepted"));
        for (lens, expected) in [("correctness", "senior"), ("security", "principal")] {
            let call = owner.calls.iter().find(|call| call.0.node_id.ends_with(&format!("--lenses--{lens}"))).unwrap();
            assert_eq!(call.1.alias.as_deref(), Some(expected));
        }
        let gate = run.checkpoint.nodes.iter().find(|node| node.id == "repair--r1--accepted").unwrap();
        assert_eq!(gate.state == NodeState::Succeeded, reviewer_passes, "gate: {:?}; panels: {:?}", gate.gate, run.checkpoint.nodes.iter().filter(|node| node.id.contains("--panel")).map(|node| (&node.id, node.state, &node.outputs)).collect::<Vec<_>>());
        for (call, source) in owner.calls.iter().zip(&owner.source_commits).filter(|(call, _)| call.0.node_id.contains("--panel--")) {
            assert_eq!(source.as_deref(), Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"), "{}", call.0.node_id);
        }
        assert_eq!(advance(&document, &mut FakeOwner::default(), &mut journal).unwrap().checkpoint, run.checkpoint);
    }
}

#[test]
fn failed_replanned_panel_repairs_its_findings_within_cycle_limit() {
    for limit in [1, 3] {
        let mut document = review_repair_document();
        document.nodes[1].loop_body.as_mut().unwrap().max_rounds = limit;
        document.nodes[1].loop_body.as_mut().unwrap().body[0].inputs.push(serde_json::from_value(serde_json::json!({"node":"previous.check","outputs":["findings"]})).unwrap());
        let initial = repair_finding(false);
        let growth = initial.replace("\"design_defect\":false", "\"design_defect\":false,\"risk_grew\":true");
        let new_finding = initial.replace("F1", "F2").replace("correctness", "security");
        let pass = "```findings\n{\"schema_version\":1,\"findings\":[]}\n```\nVERDICT: PASS";
        let mut owner = if limit == 1 {
            repair_reviews(&[&initial, "fix one", &growth, pass, &new_finding, &new_finding, "principal options"])
        } else {
            repair_reviews(&[&initial, "fix one", &growth, pass, &new_finding, &new_finding, "fix two", pass, pass, pass, pass])
        };
        owner.repair_panel = Some(vec!["correctness".into(), "security".into()]);
        for (index, oid) in [(1, 'a'), (6, 'b')] {
            if let Some(AttemptObservation::Terminal { commit, .. }) = owner.observations.get_mut(index) {
                *commit = Some(oid.to_string().repeat(40));
            }
        }
        let mut journal = MemoryJournal::default();
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        assert_eq!(run.status, GraphStatus::Complete);
        assert!(run.checkpoint.successful(), "{:?}", run.checkpoint.nodes);
        let first_gate = run.checkpoint.nodes.iter().find(|node| node.id == "repair--r1--accepted").unwrap();
        assert_eq!(first_gate.state, NodeState::Failed);
        if limit == 1 {
            assert_eq!(owner.calls.last().unwrap().0.node_id, "design");
            let findings = &run.checkpoint.nodes.iter().find(|node| node.id == "repair").unwrap().outputs.as_ref().unwrap().findings;
            assert!(findings.iter().any(|finding| finding.id.ends_with("F2") && finding.extra["lens"] == "security"));
        } else {
            let writer = owner.calls.iter().position(|call| call.0.node_id == "repair--r2--fix").unwrap();
            assert!(owner.prompts[writer].contains("F2"), "{}", owner.prompts[writer]);
            assert!(owner.prompts[writer].contains("security"));
            assert!(!owner.calls.iter().any(|call| call.0.node_id == "design"));
        }
        assert_eq!(advance(&document, &mut FakeOwner::default(), &mut journal).unwrap().checkpoint, run.checkpoint);
    }
}

#[test]
fn review_repair_exhaustion_runs_principal_instead_of_owner_wait() {
    let document = review_repair_document();
    let initial = repair_finding(false);
    let second = initial.replace("F1", "F2");
    let third = initial.replace("F1", "F3");
    let mut owner = repair_reviews(&[
        &initial,
        "fix one",
        &second,
        "fix two",
        &third,
        "principal options",
    ]);
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(run.checkpoint.successful());
    assert_eq!(owner.calls.len(), 6);
    assert_eq!(run.checkpoint.nodes.iter().find(|node| node.id == "repair")
        .unwrap().gate.as_ref().unwrap().reason.as_deref(),
        Some("third review round requires design judgement"));
    assert_eq!(owner.calls.last().unwrap().0.node_id, "design");
    assert!(run.checkpoint.asks.is_empty());
    assert_eq!(
        advance(&document, &mut FakeOwner::default(), &mut journal)
            .unwrap()
            .checkpoint,
        run.checkpoint
    );
}

#[test]
fn principal_recovery_completes_enclosing_composite_and_delivery_continues() {
    let mut document = review_repair_document();
    let members = std::mem::take(&mut document.nodes);
    document.nodes.push(GraphNode::composite(
        "assurance".into(),
        vec![],
        CompositeSpec {
            nodes: members,
            min_successful: None,
            reduce_verdict: false,
            exports: vec![],
        },
    ));
    document.nodes.push(serde_json::from_value(serde_json::json!({
        "id":"next","access":"read_only","depends_on":["assurance"],"prompt":"continue", "route":{"alias":"senior"}
    })).unwrap());
    let finding = repair_finding(true);
    let mut owner = repair_reviews(&[&finding, "principal options", "continued delivery"]);
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(run.checkpoint.successful());
    assert_eq!(owner.calls.last().unwrap().0.node_id, "next");
    let escalation = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "assurance--repair")
        .unwrap();
    let findings = &escalation.outputs.as_ref().unwrap().findings;
    assert_eq!(findings[0].extra["lens"], "correctness");
    assert_eq!(findings[0].extra["evidence"], "counterexample");
    assert_eq!(findings[0].extra["proposed_fix"], "repair");
    assert_eq!(
        advance(&document, &mut FakeOwner::default(), &mut journal)
            .unwrap()
            .checkpoint,
        run.checkpoint
    );
}

#[test]
fn compiled_repair_design_continuation_keeps_the_original_writer() {
    let library = template::TemplateLibrary::bundled();
    let compiler = template::Compiler::new(&library);
    let assurance = compiler.compile(&serde_json::json!({
        "id":"assurance","use":"review-repair","after":["initial"],
        "inputs":[{"node":"initial","outputs":["findings","verdict"]}],
        "with":{"worktree":"/fixture/writer","write_scope":["src"],"writer":"implement","lenses":["correctness"],"max_repair_cycles":1}
    })).unwrap();
    let implement = serde_json::from_value(serde_json::json!({
        "id":"implement","access":"worktree_write","worktree":"/fixture/writer","write_scope":["src"],"delivery":"pr","prompt":"implement","route":{"alias":"senior"}
    })).unwrap();
    let initial = serde_json::from_value(serde_json::json!({
        "id":"initial","access":"read_only","depends_on":["implement"],"prompt":"review","route":{"alias":"senior"},"require_findings":true
    })).unwrap();
    let mut document = review_repair_document();
    document.nodes = vec![GraphNode::composite(
        "delivery".into(),
        vec![],
        CompositeSpec {
            nodes: vec![implement, initial, assurance],
            min_successful: None,
            reduce_verdict: false,
            exports: vec![],
        },
    )];
    let finding = repair_finding(true);
    let fragment = format!(
        "```fragment\n{}\n```",
        serde_json::json!({"schema_version":1,"nodes":[{
            "id":"slice","use":"repair-slice","brief":"Meet principal acceptance evidence within approved scope"
        }]})
    );
    let writer = AttemptObservation::Terminal {
        attempt_ref: None,
        outcome: AttemptOutcome::Succeeded,
        launched: true,
        result: None,
        result_warning: None,
        result_path: None,
        branch: Some("work/item".into()),
        commit: Some("1".repeat(40)),
    };
    let mut owner = repair_reviews(&[
        &finding,
        "root constraint and approved-scope option\n```findings\n{\"schema_version\":1,\"findings\":[],\"repair_design\":{\"within_scope\":true,\"one_way_door\":false}}\n```",
        &fragment,
        "fixed",
    ]);
    if let Some(AttemptObservation::Terminal { branch, commit, .. }) = owner.observations.back_mut()
    {
        *branch = Some("work/item".into());
        *commit = Some("2".repeat(40));
    }
    owner.observations.push_front(writer);
    let observations = owner.observations.clone();
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(
        run.status,
        GraphStatus::Complete,
        "{:?}",
        run.checkpoint.pause_reason
    );
    assert!(run.checkpoint.successful());
    let slice = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id.ends_with("continue--slice"))
        .unwrap();
    assert_eq!(
        slice.writer_binding.as_ref().unwrap().worktree,
        "/fixture/writer"
    );
    assert_eq!(
        slice.outputs.as_ref().unwrap().branch.as_deref(),
        Some("work/item")
    );
    assert_eq!(
        slice.outputs.as_ref().unwrap().commit.as_deref(),
        Some("2".repeat(40).as_str())
    );
    assert!(owner.fast_forward_only.last().copied().unwrap());
    let expanded = run
        .checkpoint
        .expansions
        .iter()
        .flat_map(|expansion| &expansion.nodes)
        .find(|node| node.id.ends_with("continue--slice"))
        .unwrap();
    assert_eq!(expanded.continues.as_deref(), Some("delivery--implement"));
    assert_eq!(
        advance(&document, &mut FakeOwner::default(), &mut journal)
            .unwrap()
            .checkpoint,
        run.checkpoint
    );
    let mut wrong_worktree = document.clone();
    let assurance = &mut wrong_worktree.nodes[0].composite.as_mut().unwrap().nodes[2];
    let continuation = &mut assurance.composite.as_mut().unwrap().nodes[2];
    continuation.plan.as_mut().unwrap().templates[0]
        .node
        .insert("worktree".into(), "/fixture/other".into());
    let mut refused_owner = FakeOwner {
        actor: "chair".into(),
        observations,
        ..Default::default()
    };
    let refused = advance(
        &wrong_worktree,
        &mut refused_owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert_eq!(refused.status, GraphStatus::Paused);
    assert_eq!(
        refused.checkpoint.pause_code.as_deref(),
        Some("invalid_plan_expansion")
    );
    assert!(refused.checkpoint.expansions.is_empty());
}

#[test]
fn repair_replan_preserves_full_topology_and_single_reviewer_flattens() {
    for (lens_count, synthesis) in [(2, true), (2, false), (1, true)] {
        let document = review_repair_document();
        let initial = repair_finding(false);
        let growth = initial.replace("\"design_defect\":false", "\"design_defect\":false,\"scope_grew\":true");
        let pass = "```findings\n{\"schema_version\":1,\"findings\":[]}\n```\nVERDICT: PASS";
        let verifier = pass;
        let mut results = vec![initial.as_str(), "fixed", growth.as_str()];
        if lens_count > 1 { results.push("framing only, no findings or verdict"); }
        results.extend(std::iter::repeat_n(pass, lens_count));
        if lens_count > 1 {
            results.push(verifier);
        }
        results.push(pass); // Every repair panel performs checked synthesis.
        let mut owner = repair_reviews(&results);
        owner.repair_panel = Some(["correctness", "security"][..lens_count].iter().map(|lens| (*lens).into()).collect());
        owner.repair_panel_frame = true;
        owner.repair_panel_verifiers = 1;
        owner.repair_panel_synthesis = synthesis;
        if let AttemptObservation::Terminal { commit, .. } = &mut owner.observations[1] { *commit = Some("a".repeat(40)); }
        let mut journal = MemoryJournal::default();
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        let gate = run.checkpoint.nodes.iter().find(|node| node.id == "repair--r1--accepted").unwrap();
        assert_eq!(gate.state, NodeState::Succeeded, "{:?}", gate.gate);
        let panel_calls: Vec<_> = owner.calls.iter().filter(|call| call.0.node_id.contains("--panel--")).collect();
        let expected = if lens_count == 1 { 2 } else { 5 };
        assert_eq!(panel_calls.len(), expected);
        for (stage, expected) in [("frame", lens_count > 1), ("verify--1", lens_count > 1), ("synthesis", true)] {
            assert_eq!(panel_calls.iter().any(|call| call.0.node_id.ends_with(&format!("--{stage}"))), expected);
        }
        for ((key, _), source) in owner.calls.iter().zip(&owner.source_commits).filter(|((key, _), _)| key.node_id.contains("--panel--")) {
            assert_eq!(source.as_deref(), Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"), "{}", key.node_id);
        }
        assert_eq!(owner.repair_panel_calls, 1);
        let replay = advance(&document, &mut FakeOwner::default(), &mut journal).unwrap();
        assert_eq!(replay.checkpoint, run.checkpoint);
    }
}

struct CrashBeforeRepairGate {
    memory: MemoryJournal,
    after_panel: bool,
    crashed: bool,
}
impl GraphJournal for CrashBeforeRepairGate {
    type Error = NoObservation;
    fn load(&mut self, _: &str) -> Result<Option<GraphCheckpoint>, Self::Error> { Ok(self.memory.0.clone()) }
    fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error> {
        self.memory.0 = Some(checkpoint.clone());
        let gate_pending = checkpoint.nodes.iter().any(|node| node.id == "repair--r1--accepted" && node.state == NodeState::Pending);
        let ready = if self.after_panel {
            checkpoint.expansions.iter().any(|expansion| expansion.patch.as_ref().is_some_and(|record| record.reason.as_deref() == Some("repair expanded risk or scope"))
                && !repair::panel_pending(checkpoint, expansion) && repair::panel_failure(checkpoint, expansion).is_none())
        } else {
            checkpoint.nodes.iter().any(|node| node.extra.get("repair_growth").and_then(|cache| cache.get("grew")).and_then(serde_json::Value::as_bool) == Some(false))
        };
        if !self.crashed && gate_pending && ready { self.crashed = true; return Err(NoObservation); }
        Ok(())
    }
}

#[test]
fn pending_repair_cache_and_panel_never_certify_a_redone_fix() {
    for after_panel in [false, true] {
        let document = review_repair_document();
        let limits = crate::config::GraphLimits { expansion_depth: 64, ..Default::default() };
        let initial = repair_finding(false);
        let pass = "```findings\n{\"schema_version\":1,\"findings\":[]}\n```\nVERDICT: PASS";
        let mut owner = repair_reviews(&[&initial, "fixed", pass, pass, pass]);
        owner.repair_growth = after_panel;
        owner.repair_panel = Some(vec!["correctness".into()]);
        if let AttemptObservation::Terminal { commit, .. } = &mut owner.observations[1] { *commit = Some("a".repeat(40)); }
        let mut journal = CrashBeforeRepairGate { memory: MemoryJournal::default(), after_panel, crashed: false };
        let result = advance_with_budget(&document, &mut owner, &mut journal, &limits, &Default::default());
        assert!(matches!(&result, Err(GraphError::Journal(_))), "after_panel={after_panel}: {:?}", result.as_ref().map(|run| run.checkpoint.nodes.iter().map(|node| (&node.id, node.state, &node.gate)).collect::<Vec<_>>()));
        assert!(journal.crashed);
        let mut checkpoint = journal.memory.0.take().unwrap();
        let before_check = checkpoint.nodes.iter().find(|node| node.id == "repair--r1--check").unwrap().outputs.clone();
        let library = template::TemplateLibrary::bundled();
        let resolvers = Default::default();
        patch::retry_node(&mut patch::LiveGraph { document: &document, checkpoint: &mut checkpoint }, "repair--r1--fix", None,
            &patch::PatchContext { library: &library, resolvers: &resolvers, scan: &patch::scan_for_secrets, journal: None, limits: &Default::default() }).unwrap();
        journal.memory.0 = Some(checkpoint);
        let mut next = repair_reviews(&["fixed differently", "principal recovery options"]);
        next.repair_growth = true;
        if let AttemptObservation::Terminal { commit, .. } = &mut next.observations[0] { *commit = Some("b".repeat(40)); }
        let run = advance_with_budget(&document, &mut next, &mut journal, &limits, &Default::default()).unwrap();
        assert_eq!(next.repair_growth_calls, 1);
        let gate = run.checkpoint.nodes.iter().find(|node| node.id == "repair--r1--accepted").unwrap();
        assert_eq!(gate.state, NodeState::Failed);
        assert_eq!(gate.extra["repair_growth"]["after"], "b".repeat(40));
        assert_eq!(gate.extra["repair_growth"]["grew"], true);
        if after_panel {
            assert_eq!(next.repair_panel_calls, 0);
            assert!(gate.gate.as_ref().unwrap().reason.as_deref().unwrap().contains("different repaired commit"));
        } else { assert_eq!(next.repair_panel_calls, 1); }
        let check = run.checkpoint.nodes.iter().find(|node| node.id == "repair--r1--check").unwrap();
        assert_eq!(check.outputs, before_check);
        assert!(check.extra.contains_key("settled_dependency_snapshots"));
    }
}

#[test]
fn compiled_repair_rechecks_new_panel_lens_after_the_next_fix() {
    let library = template::TemplateLibrary::bundled();
    let repair = template::Compiler::new(&library)
        .compile(&serde_json::json!({
            "id":"repair","use":"repair",
            "after":["initial"],
            "inputs":[{"node":"initial","outputs":["findings","verdict"]}],
            "repeat":{"until":"accepted","max_rounds":3,"review_repair":true},
            "with":{"worktree":"/fixture/writer","write_scope":["src"],"lenses":["correctness"]}
        }))
        .unwrap();
    let mut document = review_repair_document();
    document.nodes.truncate(1);
    document.nodes.push(repair);
    document.context = Some(serde_json::from_value(serde_json::json!({"risk_tier":"substantial"})).unwrap());
    let initial = repair_finding(false);
    let security = initial.replace("F1", "F2").replace("correctness", "security");
    let growth = "```findings\n{\"schema_version\":1,\"risk_tier\":\"crucial\",\"findings\":[]}\n```\nVERDICT: PASS";
    let pass = "```findings\n{\"schema_version\":1,\"risk_tier\":\"substantial\",\"findings\":[]}\n```\nVERDICT: PASS";
    let mut owner = repair_reviews(&[
        &initial, "fix one", growth, growth, pass, &security, &security,
        "fix two", pass, pass, pass, pass, pass,
    ]);
    owner.repair_panel = Some(vec!["correctness".into(), "security".into()]);
    for (index, oid) in [(1, 'a'), (7, 'b')] {
        if let AttemptObservation::Terminal { commit, .. } = &mut owner.observations[index] {
            *commit = Some(oid.to_string().repeat(40));
        }
    }
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(run.checkpoint.successful(), "{:?}", run.checkpoint.nodes.iter().map(|node| (&node.id, node.state, &node.gate, &node.advance_error)).collect::<Vec<_>>());
    assert_eq!(owner.repair_panel_calls, 2);
    // The first repair receives the complete structured finding, not just the
    // prose from the review. Later rounds must receive the replanned panel's
    // finding instead (checked below).
    let first_writer = owner.calls.iter().position(|call| call.0.node_id == "repair--r1--fix").unwrap();
    let first_prompt = &owner.prompts[first_writer];
    for field in [
        "\"id\":\"F1\"", "\"lens\":\"correctness\"", "\"severity\":\"high\"",
        "\"file\":\"a.rs\"", "\"line\":1", "\"evidence\":\"counterexample\"",
        "\"proposed_fix\":\"repair\"",
    ] {
        assert!(first_prompt.contains(field), "missing {field} in {first_prompt}");
    }
    let sources: Vec<_> = owner.calls.iter().zip(&owner.source_commits)
        .filter(|(call, _)| call.0.node_id.ends_with("--lenses--security"))
        .map(|(_, source)| source.clone()).collect();
    assert_eq!(sources, [Some("a".repeat(40)), Some("b".repeat(40))]);
    let writer = owner.calls.iter().position(|call| call.0.node_id == "repair--r2--fix").unwrap();
    assert!(owner.prompts[writer].contains("F2"));
    assert!(owner.prompts[writer].contains("security"));
    assert_eq!(advance(&document, &mut FakeOwner::default(), &mut journal).unwrap().checkpoint, run.checkpoint);
}
