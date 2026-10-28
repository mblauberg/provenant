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
    attempts: usize,
    deliveries: usize,
}
impl CiStatus for Owner {
    fn now_seconds(&self) -> Option<u64> {
        Some(10_000)
    }
    fn observe_ci(&mut self, target: &str) -> CiObservation {
        self.targets.push(target.into());
        self.observations
            .pop_front()
            .expect("CI fixture observation")
    }
}
impl AttemptOwner for Owner {
    type Error = Infallible;
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
        "id": "ci", "kind": "gate", "access": "read_only", "condition": {"type": "ci", "target": "feature/example", "required": "passed"}
    }] })).unwrap()
}
fn observation(state: CiState, summary: &str) -> CiObservation {
    CiObservation {
        state,
        summary: summary.into(),
    }
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
    let first = node.ci.as_ref().unwrap().first_observed_seconds;
    let encoded = serde_json::to_vec(&waiting.checkpoint).unwrap();
    let restored: GraphCheckpoint = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(restored, waiting.checkpoint);
    journal.0 = Some(restored);
    let finished = advance(&document(), &mut owner, &mut journal).unwrap();
    assert_eq!(finished.checkpoint.nodes[0].state, NodeState::Succeeded);
    let evidence = finished.checkpoint.nodes[0].ci.as_ref().unwrap();
    assert_eq!(evidence.first_observed_seconds, first);
    assert_eq!(evidence.state, CiState::Passed);
    assert_eq!(evidence.summary, "1 check passed");
    assert_eq!(owner.attempts, 0);
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
    journal.0.as_mut().unwrap().nodes[0]
        .ci
        .as_mut()
        .unwrap()
        .first_observed_seconds = 0;
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
            target: target.into(),
            required: CiRequired::Passed,
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
