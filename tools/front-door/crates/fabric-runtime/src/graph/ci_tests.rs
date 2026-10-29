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
    branch: Option<String>,
    commit: Option<String>,
    prompts: Vec<String>,
}
impl CiStatus for Owner {
    fn host_stamp(&self) -> Option<CiHostStamp> {
        Some(CiHostStamp {
            extra: Default::default(),
            boot_identity: self.boot.clone(),
            host_monotonic_micros: self.monotonic_micros,
        })
    }
    fn observe_ci(&mut self, target: &str, checks: &[String], _: usize, _: usize) -> CiObservation {
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
        input: NodeInput<'_>,
        _: &GraphRoute,
        _: AccessMode,
        _: Option<&str>,
        _: Replay<'_>,
    ) -> Result<AttemptObservation, Self::Error> {
        self.attempts += 1;
        self.prompts.push(input.prompt.into());
        Ok(AttemptObservation::Terminal {
            attempt_ref: None,
            outcome: AttemptOutcome::Succeeded,
            launched: true,
            result: None,
            result_warning: None,
            result_path: None,
            branch: self.branch.clone(),
            commit: self.commit.clone(),
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
        head_sha: Some("a".repeat(40)),
        failed_checks: Vec::new(),
        check_names: Vec::new(),
        truncated: false,
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
        observations: VecDeque::from([
            observation(CiState::Pending, "waiting"),
            observation(CiState::Pending, "still waiting"),
        ]),
        ..Default::default()
    };
    let mut journal = Journal::default();
    let first = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(first.checkpoint.wake_at.unwrap().as_micros(), 120_000_000);
    owner.wall_seconds = 120;
    owner.monotonic_micros = 120_000_000;
    let done = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(done.checkpoint.nodes[0].state, NodeState::Failed);
    assert_eq!(
        done.checkpoint.nodes[0]
            .gate
            .as_ref()
            .unwrap()
            .reason
            .as_deref(),
        Some("timeout")
    );
    assert!(done.checkpoint.wake_at.is_none());
    // The deadline poll observes once more so a newer head could restart it.
    assert_eq!(owner.targets.len(), 2);
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
    assert_eq!(node.ci.as_ref().unwrap().summary, "still waiting");
    assert_eq!(owner.targets.len(), 2);
}

#[test]
fn ci_timeout_ignores_wall_clock_steps_and_reanchors_after_boot() {
    let mut owner = Owner {
        observations: VecDeque::from([
            observation(CiState::Pending, "first"),
            observation(CiState::Pending, "forward wall step"),
            observation(CiState::Pending, "backward wall step"),
            observation(CiState::Pending, "new boot"),
            observation(CiState::Pending, "timeout"),
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
        observations: VecDeque::from(vec![observation(CiState::Pending, "waiting"); 5]),
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
    assert_eq!(owner.targets.len(), 5);
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
            failed_checks_max_bytes: None,
            max_failed_checks: None,
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

fn node_target_document() -> GraphDocument {
    serde_json::from_value(serde_json::json!({
        "schema_version":1, "graph_id":"node-ci", "nodes":[
            {"id":"write","access":"worktree_write","worktree":"lane","prompt":"write",
             "delivery":"pr","route":{"adapter":"fixture","model":"fixture/model"}},
            {"id":"ci","kind":"gate","access":"read_only","depends_on":["write"],
             "condition":{"type":"ci","target":{"node":"write"}}}
        ]
    }))
    .unwrap()
}

#[test]
fn ci_node_target_resolves_delivered_branch_and_records_head() {
    let mut owner = Owner {
        branch: Some("delivered/topic".into()),
        commit: Some("a".repeat(40)),
        observations: VecDeque::from([observation(CiState::Passed, "green")]),
        ..Default::default()
    };
    let doc = node_target_document();
    let mut journal = Journal::default();
    let result = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(owner.targets, ["refs/heads/delivered/topic"]);
    assert_eq!(owner.attempts, 1);
    let ci = &result.checkpoint.nodes[1];
    assert_eq!(
        ci.ci.as_ref().unwrap().head_sha.as_deref(),
        Some("a".repeat(40).as_str())
    );
    assert!(ci.attempts.is_empty());
    assert!(ci.outputs.as_ref().unwrap().findings.is_empty());
    assert_eq!(
        advance(&doc, &mut owner, &mut journal).unwrap().checkpoint,
        result.checkpoint
    );
}

#[test]
fn ci_node_target_requires_a_delivered_branch_and_upstream_dependency() {
    let doc = node_target_document();
    let mut owner = Owner::default();
    let result = advance(&doc, &mut owner, &mut Journal::default()).unwrap();
    assert_eq!(result.checkpoint.nodes[1].state, NodeState::Failed);
    assert!(
        result.checkpoint.nodes[1]
            .ci
            .as_ref()
            .unwrap()
            .summary
            .contains("no delivered branch")
    );
    assert!(owner.targets.is_empty());
    let mut doc = doc;
    doc.nodes[1].depends_on.clear();
    assert_eq!(
        validate_plan(&doc).err(),
        Some("CI target node must be an upstream dependency")
    );
}

#[test]
fn ci_new_head_supersedes_pin_and_restarts_timeout() {
    let mut next = observation(CiState::Pending, "new push");
    next.head_sha = Some("b".repeat(40));
    let mut green = observation(CiState::Passed, "new head green");
    green.head_sha = next.head_sha.clone();
    let mut owner = Owner {
        branch: Some("delivered/topic".into()),
        commit: Some("a".repeat(40)),
        observations: VecDeque::from([observation(CiState::Pending, "old push"), next, green]),
        ..Default::default()
    };
    let doc = node_target_document();
    let mut journal = Journal::default();
    advance(&doc, &mut owner, &mut journal).unwrap();
    owner.monotonic_micros = 7_200_000_000;
    let next = advance(&doc, &mut owner, &mut journal).unwrap();
    let ci = next.checkpoint.nodes[1].ci.as_ref().unwrap();
    assert_eq!(ci.head_sha.as_deref(), Some("b".repeat(40).as_str()));
    assert_eq!(ci.elapsed_seconds, 0);
    assert_eq!(next.checkpoint.nodes[1].state, NodeState::Running);
    owner.monotonic_micros += 10_000_000;
    let done = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(done.checkpoint.nodes[1].state, NodeState::Succeeded);
    assert_eq!(
        done.checkpoint.nodes[1]
            .ci
            .as_ref()
            .unwrap()
            .elapsed_seconds,
        10
    );
    assert_eq!(owner.attempts, 1);
}

#[test]
fn ci_tick_gates_node_target_and_new_head_resets_backoff() {
    let mut next = observation(CiState::Pending, "new push");
    next.head_sha = Some("b".repeat(40));
    let mut owner = Owner {
        tick: true,
        backoff: CiPollBackoff {
            initial_seconds: 60,
            max_seconds: 240,
        },
        branch: Some("delivered/topic".into()),
        commit: Some("a".repeat(40)),
        observations: VecDeque::from([
            observation(CiState::Pending, "old push"),
            observation(CiState::Pending, "old push still"),
            next,
        ]),
        ..Default::default()
    };
    let doc = node_target_document();
    let mut journal = Journal::default();
    let first = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(first.checkpoint.wake_at.unwrap().as_micros(), 60_000_000);
    owner.wall_seconds = 59;
    advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(owner.targets.len(), 1);
    owner.wall_seconds = 60;
    owner.monotonic_micros = 60_000_000;
    let second = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(second.checkpoint.wake_at.unwrap().as_micros(), 180_000_000);
    assert_eq!(
        second.checkpoint.nodes[1]
            .ci
            .as_ref()
            .unwrap()
            .pending_observations,
        2
    );
    owner.wall_seconds = 180;
    owner.monotonic_micros = 180_000_000;
    let third = advance(&doc, &mut owner, &mut journal).unwrap();
    let ci = third.checkpoint.nodes[1].ci.as_ref().unwrap();
    assert_eq!(ci.head_sha.as_deref(), Some("b".repeat(40).as_str()));
    assert_eq!(ci.elapsed_seconds, 0);
    assert_eq!(ci.pending_observations, 1);
    assert_eq!(
        ci.first_observed_stamp
            .as_ref()
            .unwrap()
            .host_monotonic_micros,
        180_000_000
    );
    assert_eq!(third.checkpoint.wake_at.unwrap().as_micros(), 240_000_000);
    assert_eq!(owner.targets, ["refs/heads/delivered/topic"; 3]);
}

fn failed_check() -> CiFailedCheck {
    CiFailedCheck {
        extra: Default::default(),
        name: "test".into(),
        url: Some("https://checks/1".into()),
        excerpt: "assertion failed".into(),
    }
}

#[test]
fn ci_failure_findings_reach_repair_input_and_survive_resume() {
    let mut doc = document();
    doc.nodes[0].on_fail = Some("repair".into());
    doc.nodes.push(
        serde_json::from_value(serde_json::json!({
            "id":"repair","access":"read_only","prompt":"repair","depends_on":["ci"],
            "inputs":[{"node":"ci","outputs":["findings"]}],
            "route":{"adapter":"fixture","model":"fixture/model"}
        }))
        .unwrap(),
    );
    let mut failure = observation(CiState::Failed, "failed");
    failure.failed_checks.push(failed_check());
    let mut owner = Owner {
        observations: VecDeque::from([failure]),
        ..Default::default()
    };
    let mut journal = Journal::default();
    let run = advance(&doc, &mut owner, &mut journal).unwrap();
    let outputs = run.checkpoint.nodes[0].outputs.as_ref().unwrap();
    assert_eq!(outputs.findings[0].summary, "test: assertion failed");
    assert_eq!(outputs.findings[0].extra["url"], "https://checks/1");
    assert!(owner.prompts[0].contains("assertion failed"));
    assert!(owner.prompts[0].contains("https://checks/1"));
    assert_eq!(
        advance(&doc, &mut owner, &mut journal).unwrap().checkpoint,
        run.checkpoint
    );
}

#[test]
fn ci_failed_checks_obey_count_and_canonical_byte_caps_with_unicode_and_escapes() {
    let mut check = failed_check();
    check.name = "é\"\\\n".repeat(20);
    check.url = Some("https://checks/\"é".repeat(20));
    check.excerpt = "assert\n\"🦀\\".repeat(300);
    for cap in 2..512 {
        for count in 0..=3 {
            let mut failure = observation(CiState::Failed, "failed");
            failure.failed_checks = vec![check.clone(); 4];
            failure.bound_failed_checks(cap, count);
            assert!(
                canonical_len(&failure.failed_checks) <= cap,
                "cap={cap}, count={count}"
            );
            assert!(failure.failed_checks.len() <= count);
            assert!(failure.truncated);
        }
    }
}

#[test]
fn ci_configured_evidence_caps_apply_before_checkpoint_and_findings() {
    let mut doc = document();
    let Some(GateCondition::Ci {
        failed_checks_max_bytes,
        max_failed_checks,
        ..
    }) = &mut doc.nodes[0].condition
    else {
        unreachable!()
    };
    *failed_checks_max_bytes = Some(150);
    *max_failed_checks = Some(1);
    let mut failure = observation(CiState::Failed, "failed");
    failure.failed_checks = vec![failed_check(); 10];
    let mut owner = Owner {
        observations: VecDeque::from([failure]),
        ..Default::default()
    };
    let mut journal = Journal::default();
    let run = advance(&doc, &mut owner, &mut journal).unwrap();
    let ci = run.checkpoint.nodes[0].ci.as_ref().unwrap();
    assert_eq!(ci.failed_checks.len(), 1);
    assert_eq!(ci.extra["failed_checks_policy"]["max_bytes"], 150);
    assert_eq!(
        ci.extra["failed_checks_policy"]["max_bytes_source"],
        "condition"
    );
    assert!(canonical_len(&ci.failed_checks) <= 150);
    assert!(ci.truncated);
    assert!(run.checkpoint.nodes[0].outputs.as_ref().unwrap().warnings[0].contains("truncated"));
    assert_eq!(
        advance(&doc, &mut owner, &mut journal).unwrap().checkpoint,
        run.checkpoint
    );
}

#[test]
fn ci_node_targets_follow_composite_and_loop_namespaces() {
    let doc = node_target_document();
    let nested: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version":1,"graph_id":"nested","nodes":[{
            "id":"bundle","kind":"composite","access":"read_only",
            "composite":{"nodes":doc.nodes,"exports":[{"node":"ci","outputs":["verdict"]}]}
        }]
    }))
    .unwrap();
    let expanded = nested.expanded(&Default::default()).unwrap();
    let ci = expanded
        .nodes
        .iter()
        .find(|n| n.id == "bundle--ci")
        .unwrap();
    assert!(
        matches!(&ci.condition, Some(GateCondition::Ci { target:CiTarget::Node { node,.. },.. }) if node=="bundle--write")
    );
    for target in ["write", "repair"] {
        let mut doc = node_target_document();
        doc.nodes.pop();
        doc.nodes.push(
            serde_json::from_value(serde_json::json!({
                "id":"repairs","kind":"loop","access":"read_only","depends_on":["write"],
                "loop":{"max_rounds":2,"until":"ci","body":[
                    {"id":"repair","access":"worktree_write","worktree":"lane","prompt":"repair",
                     "route":{"adapter":"fixture","model":"fixture/model"}},
                    {"id":"ci","kind":"gate","access":"read_only","depends_on":["repair"],
                     "condition":{"type":"ci","target":{"node":target}}}
                ]}
            }))
            .unwrap(),
        );
        let expanded = doc.expanded(&Default::default()).unwrap();
        for round in 1..=2 {
            let ci = expanded
                .nodes
                .iter()
                .find(|n| n.id == format!("repairs--r{round}--ci"))
                .unwrap();
            let expected = if target == "write" {
                "write".into()
            } else {
                format!("repairs--r{round}--repair")
            };
            assert!(
                matches!(&ci.condition, Some(GateCondition::Ci { target:CiTarget::Node { node,.. },.. }) if node==&expected)
            );
        }
    }
}

#[test]
fn ci_evidence_configuration_cannot_outgrow_the_checkpoint_envelope() {
    let mut doc = document();
    let Some(GateCondition::Ci {
        failed_checks_max_bytes,
        ..
    }) = &mut doc.nodes[0].condition
    else {
        unreachable!()
    };
    *failed_checks_max_bytes = Some(usize::MAX);
    assert!(
        doc.expanded(&Default::default())
            .unwrap_err()
            .contains("checkpoint could reach")
    );
}

#[test]
fn ci_cannot_pass_without_a_head_sha() {
    let mut green = observation(CiState::Passed, "green");
    green.head_sha = None;
    let mut owner = Owner {
        observations: VecDeque::from([green]),
        ..Default::default()
    };
    let run = advance(&document(), &mut owner, &mut Journal::default()).unwrap();
    assert_eq!(run.checkpoint.nodes[0].state, NodeState::Failed);
    assert_eq!(
        run.checkpoint.nodes[0].ci.as_ref().unwrap().state,
        CiState::Unavailable
    );
}

#[test]
fn ci_composite_target_resolves_a_transitive_external_ancestor() {
    let mut doc = node_target_document();
    let mut gate = doc.nodes.pop().unwrap();
    gate.depends_on.clear();
    doc.nodes.push(
        serde_json::from_value(serde_json::json!({
            "id":"bridge","access":"read_only","prompt":"bridge","depends_on":["write"],
            "route":{"adapter":"fixture","model":"fixture/model"}
        }))
        .unwrap(),
    );
    doc.nodes.push(
        serde_json::from_value(serde_json::json!({
            "id":"bundle","kind":"composite","access":"read_only","depends_on":["bridge"],
            "composite":{"nodes":[gate],"exports":[{"node":"ci","outputs":["verdict"]}]}
        }))
        .unwrap(),
    );
    let expanded = doc.expanded(&Default::default()).unwrap();
    let gate = expanded
        .nodes
        .iter()
        .find(|node| node.id == "bundle--ci")
        .unwrap();
    assert!(
        matches!(&gate.condition,Some(GateCondition::Ci {target:CiTarget::Node {node,..},..}) if node=="write")
    );
    assert!(validate_plan(&doc).is_ok());
}

#[test]
fn ci_observation_without_a_sha_keeps_the_bound_head_across_replay() {
    let mut incomplete = observation(CiState::Pending, "head unknown");
    incomplete.head_sha = None;
    let mut owner = Owner {
        tick: true,
        backoff: CiPollBackoff {
            initial_seconds: 60,
            max_seconds: 240,
        },
        observations: VecDeque::from([
            observation(CiState::Pending, "A running"),
            incomplete,
            observation(CiState::Pending, "A still running"),
        ]),
        ..Default::default()
    };
    let mut journal = Journal::default();
    let replay = |journal: &mut Journal, run: &GraphRun| {
        journal.0 =
            Some(serde_json::from_slice(&serde_json::to_vec(&run.checkpoint).unwrap()).unwrap());
    };
    let first = advance(&document(), &mut owner, &mut journal).unwrap();
    replay(&mut journal, &first);
    for (seconds, observations, wake) in [(60, 2, 180), (180, 3, 420)] {
        owner.wall_seconds = seconds;
        owner.monotonic_micros = u64::try_from(seconds).unwrap() * 1_000_000;
        let run = advance(&document(), &mut owner, &mut journal).unwrap();
        let ci = run.checkpoint.nodes[0].ci.as_ref().unwrap();
        assert_eq!(ci.head_sha.as_deref(), Some("a".repeat(40).as_str()));
        // No head change: neither the timeout anchor nor the backoff resets.
        assert_eq!(ci.pending_observations, observations);
        assert_eq!(ci.elapsed_seconds, u64::try_from(seconds).unwrap());
        assert_eq!(
            ci.first_observed_stamp
                .as_ref()
                .unwrap()
                .host_monotonic_micros,
            0
        );
        assert_eq!(
            run.checkpoint.wake_at.unwrap().as_micros(),
            wake * 1_000_000
        );
        replay(&mut journal, &run);
    }
    assert_eq!(owner.targets.len(), 3);
}
