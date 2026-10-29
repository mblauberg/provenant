use super::*;
use std::collections::VecDeque;
use std::convert::Infallible;

#[derive(Default)]
struct Journal(Option<GraphCheckpoint>);
impl GraphJournal for Journal {
    type Error = Infallible;
    fn load(&mut self, _: &str) -> Result<Option<GraphCheckpoint>, Self::Error> {
        Ok(self.0.clone())
    }
    fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error> {
        self.0 = Some(checkpoint.clone());
        Ok(())
    }
}

#[derive(Default)]
struct Owner {
    observations: VecDeque<CiObservation>,
    targets: Vec<String>,
    checks: Vec<Vec<String>>,
    attempts: usize,
    deliveries: usize,
    wall_seconds: i64,
    monotonic_micros: u64,
    boot: String,
    tick: bool,
    backoff: CiPollBackoff,
    policies: std::collections::BTreeMap<String, CiPollBackoff>,
}
impl CiStatus for Owner {
    fn host_stamp(&self) -> Option<CiHostStamp> {
        Some(CiHostStamp {
            extra: Default::default(),
            boot_identity: self.boot.clone(),
            host_monotonic_micros: self.monotonic_micros,
        })
    }
    fn observe_ci(&mut self, target: &str, checks: &[String]) -> CiObservation {
        self.targets.push(target.into());
        self.checks.push(checks.to_vec());
        self.observations
            .pop_front()
            .expect("CI fixture observation")
    }
}
impl AttemptOwner for Owner {
    type Error = Infallible;
    fn wall_now(&self) -> Option<provenant_core::WallTime> {
        provenant_core::WallTime::from_micros(self.wall_seconds.saturating_mul(1_000_000)).ok()
    }
    fn ci_poll_backoff(&self, node_id: &str) -> CiPollBackoff {
        self.policies.get(node_id).copied().unwrap_or(self.backoff)
    }
    fn respect_ci_wake(&self) -> bool {
        self.tick
    }
    fn actor(&self) -> String {
        "fixture".into()
    }
    fn ci_status(&mut self) -> Option<&mut dyn CiStatus> {
        Some(self)
    }
    fn deliver_graph(
        &mut self,
        _: &GraphDocument,
        _: &GraphCheckpoint,
    ) -> Result<Option<String>, Self::Error> {
        self.deliveries += 1;
        Ok(None)
    }
    fn observe_or_start(
        &mut self,
        _: &AttemptKey,
        _: NodeInput<'_>,
        _: &GraphRoute,
        _: AccessMode,
        _: Option<&str>,
        _: Replay<'_>,
    ) -> Result<AttemptObservation, Self::Error> {
        self.attempts += 1;
        Ok(AttemptObservation::Terminal {
            attempt_ref: None,
            outcome: AttemptOutcome::Succeeded,
            launched: true,
            result: None,
            result_warning: None,
            result_path: None,
            branch: None,
            commit: None,
        })
    }
}

fn document() -> GraphDocument {
    serde_json::from_value(serde_json::json!({ "schema_version": 1, "graph_id": "g-ci", "nodes": [{
        "id": "ci", "kind": "gate", "access": "read_only", "condition": {"type": "ci", "target": "feature/example"}
    }] })).unwrap()
}
fn observation(state: CiState, summary: &str) -> CiObservation {
    CiObservation {
        extra: Default::default(),
        state,
        summary: summary.into(),
    }
}

#[test]
fn ci_optional_evidence_fields_remain_absent() {
    let value = serde_json::json!({"state": "pending", "summary": "waiting"});
    let evidence: CiEvidence = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(evidence).unwrap(), value);
}

#[test]
fn ci_outcomes_are_recorded_and_fail_closed() {
    for (state, expected_state, reason) in [
        (CiState::Passed, NodeState::Succeeded, None),
        (CiState::Failed, NodeState::Failed, Some("failed")),
        (CiState::Unavailable, NodeState::Failed, Some("unavailable")),
    ] {
        let mut owner = Owner {
            observations: VecDeque::from([observation(state, "checks: fixture")]),
            ..Default::default()
        };
        let mut journal = Journal::default();
        let run = advance(&document(), &mut owner, &mut journal).unwrap();
        let node = &run.checkpoint.nodes[0];
        assert_eq!(node.state, expected_state);
        assert_eq!(node.gate.as_ref().unwrap().reason.as_deref(), reason);
        let evidence = node.ci.as_ref().unwrap();
        assert_eq!(evidence.state, state);
        assert_eq!(evidence.summary, "checks: fixture");
        assert_eq!(owner.targets, ["feature/example"]);
        assert_eq!(owner.attempts, 0);
        assert_eq!(
            advance(&document(), &mut owner, &mut journal)
                .unwrap()
                .checkpoint,
            run.checkpoint
        );
        assert_eq!(owner.targets.len(), 1);
    }
}

#[test]
fn ci_pending_checkpoint_replays_then_passes() {
    let mut owner = Owner {
        observations: VecDeque::from([
            observation(CiState::Pending, "1 check running"),
            observation(CiState::Passed, "1 check passed"),
        ]),
        ..Default::default()
    };
    let mut journal = Journal::default();
    let waiting = advance(&document(), &mut owner, &mut journal).unwrap();
    assert_eq!(waiting.status, GraphStatus::Pending);
    let node = &waiting.checkpoint.nodes[0];
    assert_eq!(node.state, NodeState::Running);
    assert!(node.gate.is_none());
    assert!(node.outputs.is_none());
    assert_eq!(node.ci.as_ref().unwrap().state, CiState::Pending);
    assert_eq!(node.ci.as_ref().unwrap().summary, "1 check running");
    assert_eq!(owner.attempts, 0);
    assert_eq!(owner.deliveries, 0);
    let first = node.ci.as_ref().unwrap().first_observed_stamp.clone();
    let encoded = serde_json::to_vec(&waiting.checkpoint).unwrap();
    let restored: GraphCheckpoint = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(restored, waiting.checkpoint);
    journal.0 = Some(restored);
    let finished = advance(&document(), &mut owner, &mut journal).unwrap();
    assert_eq!(finished.checkpoint.nodes[0].state, NodeState::Succeeded);
    let evidence = finished.checkpoint.nodes[0].ci.as_ref().unwrap();
    assert_eq!(evidence.first_observed_stamp, first);
    assert_eq!(evidence.state, CiState::Passed);
    assert_eq!(evidence.summary, "1 check passed");
    assert_eq!(owner.attempts, 0);
}

#[test]
fn ci_tick_deadlines_and_backoff_survive_restart_and_stop_after_settlement() {
    let mut owner = Owner {
        tick: true,
        backoff: CiPollBackoff {
            initial_seconds: 60,
            max_seconds: 240,
        },
        observations: VecDeque::from([
            observation(CiState::Pending, "waiting"),
            observation(CiState::Pending, "still waiting"),
            observation(CiState::Passed, "green"),
        ]),
        ..Default::default()
    };
    let mut journal = Journal::default();
    let first = advance(&document(), &mut owner, &mut journal).unwrap();
    assert_eq!(first.checkpoint.wake_at.unwrap().as_micros(), 60_000_000);
    journal.0 =
        Some(serde_json::from_slice(&serde_json::to_vec(&first.checkpoint).unwrap()).unwrap());
    owner.wall_seconds = 59;
    let early = advance(&document(), &mut owner, &mut journal).unwrap();
    assert_eq!(early.checkpoint, first.checkpoint);
    assert_eq!(owner.targets.len(), 1);
    owner.wall_seconds = 60;
    let second = advance(&document(), &mut owner, &mut journal).unwrap();
    assert_eq!(second.checkpoint.wake_at.unwrap().as_micros(), 180_000_000);
    assert_eq!(
        second.checkpoint.nodes[0]
            .ci
            .as_ref()
            .unwrap()
            .pending_observations,
        2
    );
    journal.0 =
        Some(serde_json::from_slice(&serde_json::to_vec(&second.checkpoint).unwrap()).unwrap());
    owner.wall_seconds = 179;
    advance(&document(), &mut owner, &mut journal).unwrap();
    assert_eq!(owner.targets.len(), 2);
    owner.wall_seconds = 180;
    let done = advance(&document(), &mut owner, &mut journal).unwrap();
    assert_eq!(done.status, GraphStatus::Complete);
    assert!(done.checkpoint.wake_at.is_none());
    assert!(
        done.checkpoint.nodes[0]
            .ci
            .as_ref()
            .unwrap()
            .wake_at
            .is_none()
    );
    advance(&document(), &mut owner, &mut journal).unwrap();
    assert_eq!(owner.targets.len(), 3);
}

#[test]
fn ci_tick_observes_only_each_due_gate() {
    let mut doc = document();
    let mut slow = doc.nodes[0].clone();
    slow.id = "slow".into();
    doc.nodes.push(slow);
    let mut owner = Owner {
        tick: true,
        backoff: CiPollBackoff {
            initial_seconds: 60,
            max_seconds: 60,
        },
        policies: std::collections::BTreeMap::from([(
            "slow".into(),
            CiPollBackoff {
                initial_seconds: 300,
                max_seconds: 300,
            },
        )]),
        observations: VecDeque::from([
            observation(CiState::Pending, "fast waiting"),
            observation(CiState::Pending, "slow waiting"),
            observation(CiState::Passed, "fast green"),
            observation(CiState::Passed, "slow green"),
        ]),
        ..Default::default()
    };
    let mut journal = Journal::default();
    let first = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(first.checkpoint.wake_at.unwrap().as_micros(), 60_000_000);
    owner.wall_seconds = 60;
    let next = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(next.checkpoint.nodes[0].state, NodeState::Succeeded);
    assert_eq!(next.checkpoint.nodes[1].state, NodeState::Running);
    assert_eq!(next.checkpoint.wake_at.unwrap().as_micros(), 300_000_000);
    assert_eq!(owner.targets.len(), 3);
    owner.wall_seconds = 300;
    assert_eq!(
        advance(&doc, &mut owner, &mut journal).unwrap().status,
        GraphStatus::Complete
    );
    assert_eq!(owner.targets.len(), 4);
}

#[test]
fn ci_and_ask_deadlines_share_the_earliest_wake() {
    let mut doc = document();
    doc.decision_session = Some("chair-session".into());
    doc.nodes.push(
        serde_json::from_value(serde_json::json!({
            "id": "choose", "kind": "decide", "access": "read_only",
            "decide": {
                "question": "Continue?", "default": "yes", "timeout_seconds": 30,
                "options": [{"id": "yes", "label": "Continue"}, {"id": "no", "label": "Stop"}]
            }
        }))
        .unwrap(),
    );
    let mut owner = Owner {
        tick: true,
        backoff: CiPollBackoff {
            initial_seconds: 60,
            max_seconds: 60,
        },
        observations: VecDeque::from([
            observation(CiState::Pending, "waiting"),
            observation(CiState::Passed, "green"),
        ]),
        ..Default::default()
    };
    let mut journal = Journal::default();
    let first = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(first.checkpoint.wake_at.unwrap().as_micros(), 30_000_000);
    owner.wall_seconds = 30;
    let decision = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(decision.checkpoint.wake_at.unwrap().as_micros(), 60_000_000);
    assert_eq!(owner.targets.len(), 1);
    assert!(decision.checkpoint.asks.iter().all(|ask| ask.resolved));
    owner.wall_seconds = 60;
    let done = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(done.status, GraphStatus::Complete);
    assert!(done.checkpoint.wake_at.is_none());
}

#[test]
fn zero_backoff_polls_once_even_when_a_loop_reschedules_the_advance() {
    let mut doc: GraphDocument =
        serde_json::from_str(include_str!("../fixtures/loop-exhausts.json")).unwrap();
    doc.nodes.insert(0, document().nodes[0].clone());
    let mut owner = Owner {
        tick: true,
        observations: VecDeque::from([observation(CiState::Pending, "waiting")]),
        ..Default::default()
    };
    let limits = crate::config::GraphLimits {
        expansion_depth: 16,
        ..Default::default()
    };
    let mut journal = Journal::default();
    let run =
        advance_with_budget(&doc, &mut owner, &mut journal, &limits, &Default::default()).unwrap();
    assert_eq!(owner.targets.len(), 1);
    assert_eq!(owner.attempts, 2);
    assert_eq!(run.checkpoint.wake_at.unwrap().as_micros(), 0);
}

#[test]
fn ci_backoff_never_delays_the_gate_timeout() {
    let mut doc = document();
    let Some(GateCondition::Ci {
        timeout_seconds, ..
    }) = &mut doc.nodes[0].condition
    else {
        unreachable!()
    };
    *timeout_seconds = 120;
    let mut owner = Owner {
        tick: true,
        backoff: CiPollBackoff {
            initial_seconds: u64::MAX,
            max_seconds: u64::MAX,
        },
        observations: VecDeque::from([observation(CiState::Pending, "waiting")]),
        ..Default::default()
    };
    let mut journal = Journal::default();
    let first = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(first.checkpoint.wake_at.unwrap().as_micros(), 120_000_000);
    owner.wall_seconds = 120;
    owner.monotonic_micros = 120_000_000;
    let done = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(done.checkpoint.nodes[0].state, NodeState::Failed);
    assert!(done.checkpoint.wake_at.is_none());
    assert_eq!(owner.targets.len(), 1);
}

#[test]
fn ci_pending_times_out_using_original_observation_time() {
    let mut owner = Owner {
        observations: VecDeque::from([
            observation(CiState::Pending, "waiting"),
            observation(CiState::Pending, "still waiting"),
        ]),
        ..Default::default()
    };
    let mut journal = Journal::default();
    assert_eq!(
        advance(&document(), &mut owner, &mut journal)
            .unwrap()
            .status,
        GraphStatus::Pending
    );
    owner.monotonic_micros = 7_200_000_000;
    let timed_out = advance(&document(), &mut owner, &mut journal).unwrap();
    let node = &timed_out.checkpoint.nodes[0];
    assert_eq!(node.state, NodeState::Failed);
    assert_eq!(
        node.gate.as_ref().unwrap().reason.as_deref(),
        Some("timeout")
    );
    assert_eq!(node.ci.as_ref().unwrap().summary, "waiting");
    assert_eq!(owner.targets.len(), 1);
}

#[test]
fn ci_timeout_ignores_wall_clock_steps_and_reanchors_after_boot() {
    let mut owner = Owner {
        observations: VecDeque::from([
            observation(CiState::Pending, "first"),
            observation(CiState::Pending, "forward wall step"),
            observation(CiState::Pending, "backward wall step"),
            observation(CiState::Pending, "new boot"),
        ]),
        boot: "boot-a".into(),
        ..Default::default()
    };
    let mut journal = Journal::default();
    advance(&document(), &mut owner, &mut journal).unwrap();
    owner.wall_seconds = 1_000_000;
    owner.monotonic_micros = 1_000_000;
    assert_eq!(
        advance(&document(), &mut owner, &mut journal)
            .unwrap()
            .status,
        GraphStatus::Pending
    );
    owner.wall_seconds = -1_000_000;
    owner.monotonic_micros = 2_000_000;
    assert_eq!(
        advance(&document(), &mut owner, &mut journal)
            .unwrap()
            .status,
        GraphStatus::Pending
    );
    owner.boot = "boot-b".into();
    owner.monotonic_micros = 8_000_000_000;
    let result = advance(&document(), &mut owner, &mut journal).unwrap();
    assert_eq!(result.status, GraphStatus::Pending);
    let evidence = result.checkpoint.nodes[0].ci.as_ref().unwrap();
    assert_eq!(
        evidence.clock_reanchor_reason.as_deref(),
        Some("host boot changed")
    );
    assert_eq!(
        evidence
            .first_observed_stamp
            .as_ref()
            .unwrap()
            .boot_identity,
        "boot-b"
    );
    owner.monotonic_micros += 7_200_000_000;
    let result = advance(&document(), &mut owner, &mut journal).unwrap();
    assert_eq!(
        result.checkpoint.nodes[0]
            .gate
            .as_ref()
            .unwrap()
            .reason
            .as_deref(),
        Some("timeout")
    );
}

#[test]
fn ci_document_checks_reach_host_query() {
    let mut doc = document();
    let Some(GateCondition::Ci { checks, .. }) = &mut doc.nodes[0].condition else {
        unreachable!()
    };
    *checks = vec!["build".into(), "test".into()];
    let mut owner = Owner {
        observations: VecDeque::from([observation(CiState::Passed, "passed")]),
        ..Default::default()
    };
    advance(&doc, &mut owner, &mut Journal::default()).unwrap();
    assert_eq!(owner.checks, [vec!["build", "test"]]);
}

#[test]
fn ci_backwards_clock_reanchors_are_bounded() {
    let mut owner = Owner {
        observations: VecDeque::from(vec![observation(CiState::Pending, "waiting"); 4]),
        monotonic_micros: 100,
        ..Default::default()
    };
    let mut journal = Journal::default();
    advance(&document(), &mut owner, &mut journal).unwrap();
    for count in 1..=3 {
        owner.monotonic_micros -= 1;
        let result = advance(&document(), &mut owner, &mut journal).unwrap();
        assert_eq!(result.status, GraphStatus::Pending);
        assert_eq!(
            result.checkpoint.nodes[0]
                .ci
                .as_ref()
                .unwrap()
                .clock_reanchors,
            count
        );
        journal.0 =
            Some(serde_json::from_slice(&serde_json::to_vec(&result.checkpoint).unwrap()).unwrap());
    }
    owner.monotonic_micros -= 1;
    let result = advance(&document(), &mut owner, &mut journal).unwrap();
    let node = &result.checkpoint.nodes[0];
    assert_eq!(node.state, NodeState::Failed);
    assert_eq!(
        node.gate.as_ref().unwrap().reason.as_deref(),
        Some("timeout: clock re-anchor limit (3) exceeded")
    );
    let evidence = node.ci.as_ref().unwrap();
    assert_eq!(evidence.clock_reanchors, 4);
    assert_eq!(
        evidence.clock_reanchor_reason.as_deref(),
        Some("host monotonic clock moved backwards")
    );
    assert_eq!(owner.targets.len(), 4);
}

#[test]
fn ci_check_names_are_validated() {
    for checks in [vec![""], vec![" build"], vec!["build", "build"]] {
        let mut doc = document();
        let Some(GateCondition::Ci { checks: names, .. }) = &mut doc.nodes[0].condition else {
            unreachable!()
        };
        *names = checks.into_iter().map(str::to_owned).collect();
        assert_eq!(
            validate_plan(&doc).err(),
            Some("invalid CI gate check names")
        );
    }
}

#[test]
fn pending_ci_does_not_consume_attempt_concurrency() {
    let mut doc = document();
    doc.nodes.push(
        serde_json::from_value(serde_json::json!({
            "id": "task", "prompt": "work", "access": "read_only",
            "route": {"adapter": "fixture", "model": "fixture/model"}
        }))
        .unwrap(),
    );
    let mut owner = Owner {
        observations: VecDeque::from([observation(CiState::Pending, "waiting")]),
        ..Default::default()
    };
    let limits = crate::config::GraphLimits {
        concurrency: Some(1),
        ..Default::default()
    };
    let result = advance_with_budget(
        &doc,
        &mut owner,
        &mut Journal::default(),
        &limits,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(result.checkpoint.nodes[0].state, NodeState::Running);
    assert_eq!(result.checkpoint.nodes[1].state, NodeState::Succeeded);
    assert_eq!(owner.attempts, 1);
}

#[test]
fn ci_failure_activates_recovery_path() {
    let mut doc = document();
    doc.nodes[0].on_fail = Some("recovery".into());
    let recovery: GraphNode = serde_json::from_value(serde_json::json!({ "id": "recovery", "prompt": "recover", "access": "read_only", "depends_on": ["ci"], "route": {"adapter": "fixture", "model": "fixture/model"} })).unwrap();
    let success: GraphNode = serde_json::from_value(serde_json::json!({ "id": "success", "prompt": "continue", "access": "read_only", "depends_on": ["ci"], "route": {"adapter": "fixture", "model": "fixture/model"} })).unwrap();
    doc.nodes.extend([recovery, success]);
    let mut owner = Owner {
        observations: VecDeque::from([observation(CiState::Failed, "failed check")]),
        ..Default::default()
    };
    let result = advance(&doc, &mut owner, &mut Journal::default()).unwrap();
    assert_eq!(result.checkpoint.nodes[1].state, NodeState::Succeeded);
    assert_eq!(result.checkpoint.nodes[2].state, NodeState::Skipped);
    assert_eq!(owner.attempts, 1);
}

#[test]
fn ci_validation_rejects_bad_targets_and_timeouts() {
    for (target, timeout) in [
        ("", 60),
        (" ", 60),
        ("-bad", 60),
        ("valid", 0),
        ("valid", 604801),
    ] {
        let mut doc = document();
        doc.nodes[0].condition = Some(GateCondition::Ci {
            extra: Default::default(),
            target: target.into(),
            checks: vec![],
            timeout_seconds: timeout,
        });
        assert_eq!(
            validate_plan(&doc).err(),
            Some("invalid CI gate target or timeout")
        );
    }
    let doc = document();
    assert!(matches!(
        doc.nodes[0].condition,
        Some(GateCondition::Ci {
            timeout_seconds: 7200,
            ..
        })
    ));
    assert!(validate_plan(&doc).is_ok());
}
