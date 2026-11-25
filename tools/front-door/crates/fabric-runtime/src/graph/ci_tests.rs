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
    conflict_observations: VecDeque<ConflictObservation>,
    targets: Vec<String>,
    checks: Vec<Vec<String>>,
    attempts: usize,
    deliveries: usize,
    wall_seconds: i64,
    wall_unavailable: bool,
    monotonic_micros: u64,
    boot: String,
    tick: bool,
    backoff: CiPollBackoff,
    policies: std::collections::BTreeMap<String, CiPollBackoff>,
    branch: Option<String>,
    commit: Option<String>,
    integration_base: Option<provenant_core::GitOid>,
    prompts: Vec<String>,
    source_commits: Vec<Option<String>>,
    park_operations: Vec<String>,
}
impl CiStatus for Owner {
    fn host_stamp(&self) -> Option<CiHostStamp> {
        Some(CiHostStamp {
            extra: Default::default(),
            boot_identity: self.boot.clone(),
            host_monotonic_micros: self.monotonic_micros,
        })
    }
    fn observe_ci(
        &mut self,
        target: &str,
        checks: &[String],
        _: bool,
        _: usize,
        _: usize,
    ) -> CiObservation {
        self.targets.push(target.into());
        self.checks.push(checks.to_vec());
        self.observations
            .pop_front()
            .expect("CI fixture observation")
    }
    fn observe_conflict(&mut self, target: &str) -> ConflictObservation {
        self.targets.push(target.into());
        self.conflict_observations
            .pop_front()
            .expect("conflict fixture observation")
    }
}
impl AttemptOwner for Owner {
    type Error = Infallible;
    fn worktree_layout(&self) -> provenant_config::WorktreeLayoutPort {
        provenant_config::PortTable::builtin().worktree_layout
    }
    fn wall_now(&self) -> Option<provenant_core::WallTime> {
        if self.wall_unavailable {
            return None;
        }
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
    fn bound_graph_bases(
        &self,
    ) -> (
        Option<provenant_core::GitOid>,
        Option<provenant_core::GitOid>,
    ) {
        (None, self.integration_base)
    }
    fn bind_writer_worktree(
        &mut self,
        worktree: &str,
    ) -> Result<Option<WriterWorktreeBinding>, Self::Error> {
        Ok(Some(WriterWorktreeBinding {
            extra: Default::default(),
            source_directory_identity: None,
            shared_worktree: None,
            worktree: format!("/fixture/{worktree}"),
            head: self.commit.clone().unwrap_or_else(|| "a".repeat(40)),
        }))
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
    fn run_hygiene(
        &mut self,
        request: HygieneRequest<'_>,
    ) -> Result<HygieneExecution, Self::Error> {
        Ok(HygieneExecution {
            outcome: provenant_records::graph::HygieneOutcome {
                kind: provenant_records::graph::HygieneOutcomeKind::Clean,
                before_head: Some(request.input_head.to_owned()),
                after_head: Some(request.input_head.to_owned()),
                handoff: Some("fixture hygiene completed".into()),
            },
            branch: self.branch.clone(),
            formatter_source: Some("fixture".into()),
            diagnostic: None,
        })
    }
    fn publish_delivery(
        &mut self,
        request: PublicationRequest<'_>,
    ) -> Result<Option<DeliveryPublication>, Self::Error> {
        Ok(Some(DeliveryPublication {
            branch: format!("provenant/graph/{}", request.graph_id),
            head: request.expected_head.to_owned(),
            pull_request: "https://example.invalid/pr/ci".into(),
        }))
    }
    fn observe_or_start(
        &mut self,
        key: &AttemptKey,
        input: NodeInput<'_>,
        _: &GraphRoute,
        _: AccessMode,
        source_commit: Option<&str>,
        _: Replay<'_>,
    ) -> Result<AttemptObservation, Self::Error> {
        self.attempts += 1;
        self.prompts.push(input.prompt.into());
        self.source_commits.push(source_commit.map(str::to_owned));
        Ok(AttemptObservation::Terminal {
            attempt_ref: Some(format!("fixture/{}/{}", key.node_id, self.attempts)),
            outcome: AttemptOutcome::Succeeded,
            launched: true,
            result: None,
            result_warning: None,
            result_path: None,
            branch: self.branch.clone(),
            commit: self.commit.clone(),
            settlement_reason: Some(provenant_records::run::SettlementReason::Completed),
            stop: None,
        })
    }
    fn park_delivery(
        &mut self,
        request: DeliveryParkRequest<'_>,
    ) -> Result<Option<provenant_core::HumanRequestId>, Self::Error> {
        self.park_operations.push(request.operation_id.to_owned());
        Ok(None)
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
/// An observation whose checks have registered for the head.
fn registered(state: CiState, summary: &str) -> CiObservation {
    CiObservation {
        check_names: vec!["build".into()],
        ..observation(state, summary)
    }
}

#[test]
fn ci_outcomes_are_recorded_and_fail_closed() {
    for (state, expected_state, reason) in [
        (CiState::Passed, NodeState::Succeeded, None),
        (CiState::Failed, NodeState::Failed, Some("failed")),
        (CiState::Unavailable, NodeState::Failed, Some("unavailable")),
        (
            CiState::Conflict,
            NodeState::Failed,
            Some("conflict; next: rebase the branch onto its base and push"),
        ),
        (
            CiState::NotScheduled,
            NodeState::Failed,
            Some(
                "not_scheduled; next: check the workflow triggers or push an empty commit to start CI",
            ),
        ),
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
fn ci_without_a_published_head_or_registered_checks_remains_pending() {
    for (state, head) in [
        (CiState::NotScheduled, Some("a".repeat(40))),
        (CiState::Unavailable, None),
    ] {
        let mut result = observation(state, "waiting for head or checks");
        result.head_sha = head;
        let mut owner = Owner {
            branch: Some("delivered/topic".into()),
            commit: Some("a".repeat(40)),
            integration_base: Some("f".repeat(40).parse().unwrap()),
            conflict_observations: VecDeque::from([ConflictObservation {
                state: ConflictState::Clear,
                head_sha: Some("a".repeat(40)),
                summary: "no merge conflicts".into(),
            }]),
            observations: VecDeque::from([result]),
            ..Default::default()
        };
        let doc = queue_published_document();
        let run = advance(&doc, &mut owner, &mut Journal::default()).unwrap();
        let ci_index = doc.nodes.iter().position(|node| node.id == "ci").unwrap();
        let node = &run.checkpoint.nodes[ci_index];
        assert_eq!(node.state, NodeState::Running);
        assert!(node.gate.is_none());
        assert_eq!(node.ci.as_ref().unwrap().state, CiState::Pending);
        assert!(node.ci.as_ref().unwrap().wake_at.is_some());
    }
}

fn conflict_document() -> GraphDocument {
    serde_json::from_value(serde_json::json!({
        "schema_version":2,
        "graph_id":"g-conflict",
        "nodes":[{
            "id":"conflict",
            "kind":"gate",
            "access":"read_only",
            "condition":{"type":"conflict","target":"feature/example"},
            "on_fail":"rebase"
        },{
            "id":"rebase",
            "access":"worktree_write",
            "worktree":"retained",
            "prompt":"Rebase the retained branch",
            "depends_on":["conflict"],
            "route":{"adapter":"fixture","model":"fixture/model"}
        }]
    }))
    .unwrap()
}

#[test]
fn conflict_gate_only_routes_definite_conflicts_and_replays_pending_heads() {
    let head = "a".repeat(40);
    let mut owner = Owner {
        tick: true,
        wall_seconds: 100,
        backoff: CiPollBackoff {
            initial_seconds: 60,
            max_seconds: 60,
        },
        conflict_observations: VecDeque::from([
            ConflictObservation {
                state: ConflictState::Pending,
                head_sha: None,
                summary: "published head not visible".into(),
            },
            ConflictObservation {
                state: ConflictState::Pending,
                head_sha: Some(head.clone()),
                summary: "mergeability is still unknown".into(),
            },
            ConflictObservation {
                state: ConflictState::Conflicting,
                head_sha: Some(head.clone()),
                summary: "pull request conflicts with its base".into(),
            },
        ]),
        ..Default::default()
    };
    let mut journal = Journal::default();
    let first = advance(&conflict_document(), &mut owner, &mut journal).unwrap();
    let progress = &first.checkpoint.nodes[0];
    assert_eq!(progress.state, NodeState::Running);
    assert!(progress.gate.is_none());
    assert!(progress.outputs.is_none());
    assert_eq!(
        progress.conflict.as_ref().unwrap().state,
        ConflictState::Pending
    );
    assert_eq!(progress.conflict.as_ref().unwrap().head_sha, None);
    let deadline = first.checkpoint.wake_at.unwrap();
    let encoded = serde_json::to_vec(&first.checkpoint).unwrap();
    journal.0 = Some(serde_json::from_slice(&encoded).unwrap());

    // A tick before the wake leaves the pending observation intact.
    let early = advance(&conflict_document(), &mut owner, &mut journal).unwrap();
    assert_eq!(early.checkpoint.wake_at, Some(deadline));
    assert_eq!(owner.targets.len(), 1);

    owner.wall_seconds = 160;
    let second = advance(&conflict_document(), &mut owner, &mut journal).unwrap();
    assert_eq!(second.checkpoint.nodes[0].state, NodeState::Running);
    assert_eq!(
        second.checkpoint.nodes[0]
            .conflict
            .as_ref()
            .unwrap()
            .head_sha,
        Some(head.clone())
    );
    assert_eq!(second.checkpoint.nodes[0].gate, None);

    owner.wall_seconds = 220;
    let final_run = advance(&conflict_document(), &mut owner, &mut journal).unwrap();
    let conflict = &final_run.checkpoint.nodes[0];
    assert_eq!(conflict.state, NodeState::Failed);
    assert_eq!(
        conflict.gate.as_ref().unwrap().on_fail.as_deref(),
        Some("rebase")
    );
    assert_eq!(
        conflict.gate.as_ref().unwrap().reason.as_deref(),
        Some("conflicting; next: rebase the branch onto its base and push")
    );
    assert_eq!(
        conflict.conflict.as_ref().unwrap().state,
        ConflictState::Conflicting
    );
    assert_eq!(final_run.checkpoint.nodes[1].state, NodeState::Succeeded);
    assert_eq!(owner.attempts, 1);
    assert_eq!(owner.prompts, ["Rebase the retained branch"]);
}

#[test]
fn clear_conflict_gate_passes_and_unavailable_is_pending() {
    let mut owner = Owner {
        conflict_observations: VecDeque::from([
            ConflictObservation {
                state: ConflictState::Unavailable,
                head_sha: None,
                summary: "mergeability unavailable".into(),
            },
            ConflictObservation {
                state: ConflictState::Clear,
                head_sha: Some("a".repeat(40)),
                summary: "pull request is mergeable".into(),
            },
        ]),
        ..Default::default()
    };
    let mut journal = Journal::default();
    let waiting = advance(&conflict_document(), &mut owner, &mut journal).unwrap();
    assert_eq!(waiting.checkpoint.nodes[0].state, NodeState::Running);
    assert_eq!(
        waiting.checkpoint.nodes[0].conflict.as_ref().unwrap().state,
        ConflictState::Pending
    );
    let finished = advance(&conflict_document(), &mut owner, &mut journal).unwrap();
    assert_eq!(finished.checkpoint.nodes[0].state, NodeState::Succeeded);
    assert!(finished.checkpoint.nodes[0].gate.as_ref().unwrap().passed);
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
            "decide": {"on_unanswered":"block_dependants",
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
    assert!(decision.checkpoint.asks.is_empty());
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
            registered(CiState::Pending, "waiting"),
            registered(CiState::Pending, "still waiting"),
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
            registered(CiState::Pending, "waiting"),
            registered(CiState::Pending, "still waiting"),
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
fn ci_timeout_without_registered_checks_reports_not_scheduled() {
    let mut owner = Owner {
        observations: VecDeque::from([
            observation(CiState::Pending, "awaiting check registration"),
            observation(CiState::Pending, "awaiting check registration"),
        ]),
        ..Default::default()
    };
    let mut journal = Journal::default();
    advance(&document(), &mut owner, &mut journal).unwrap();
    owner.monotonic_micros = 7_200_000_000;
    let timed_out = advance(&document(), &mut owner, &mut journal).unwrap();
    let node = &timed_out.checkpoint.nodes[0];
    assert_eq!(node.state, NodeState::Failed);
    assert_eq!(
        node.gate.as_ref().unwrap().reason.as_deref(),
        Some(
            "timeout: not_scheduled; next: check the workflow triggers or push an empty commit to start CI"
        )
    );
}

#[test]
fn queue_quota_reset_defers_timeout_across_restart() {
    let mut doc = queue_published_document();
    let ci_index = doc.nodes.iter().position(|node| node.id == "ci").unwrap();
    let Some(GateCondition::Ci {
        timeout_seconds, ..
    }) = &mut doc.nodes[ci_index].condition
    else {
        unreachable!()
    };
    *timeout_seconds = 1;
    let head = "a".repeat(40);
    let mut limited = observation(CiState::Pending, "forge quota");
    limited.set_quota_reset(provenant_core::WallTime::from_micros(10_000_000).unwrap());
    let mut owner = Owner {
        branch: Some("delivered/topic".into()),
        commit: Some(head.clone()),
        integration_base: Some("f".repeat(40).parse().unwrap()),
        conflict_observations: VecDeque::from([ConflictObservation {
            state: ConflictState::Clear,
            head_sha: Some(head),
            summary: "clear".into(),
        }]),
        observations: VecDeque::from([
            limited,
            observation(CiState::Failed, "failure has no details"),
            observation(CiState::Failed, "failure has no details"),
            observation(CiState::Failed, "failure has no details"),
        ]),
        tick: true,
        ..Default::default()
    };
    let mut journal = Journal::default();
    let first = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(first.checkpoint.wake_at.unwrap().as_micros(), 10_000_000);
    journal.0 =
        Some(serde_json::from_slice(&serde_json::to_vec(&first.checkpoint).unwrap()).unwrap());
    owner.wall_seconds = 9;
    owner.monotonic_micros = 9_000_000;
    // An explicit advance can probe early and fail to retrieve a fresh reset.
    // The persisted reset still protects the gate through that observation.
    owner.tick = false;
    owner.wall_unavailable = true;
    assert!(advance(&doc, &mut owner, &mut journal).is_err());
    assert!(
        journal.0.as_ref().unwrap().nodes[ci_index]
            .delivery_park
            .is_none()
    );
    owner.wall_unavailable = false;
    let early = advance(&doc, &mut owner, &mut journal).unwrap();
    assert!(early.checkpoint.nodes[ci_index].delivery_park.is_none());
    assert_eq!(early.checkpoint.wake_at.unwrap().as_micros(), 10_000_000);
    owner.wall_seconds = 10;
    owner.monotonic_micros = 10_000_000;
    owner.tick = true;
    let expired = advance(&doc, &mut owner, &mut journal).unwrap();
    assert!(expired.checkpoint.nodes[ci_index].delivery_park.is_some());
}

#[test]
fn queue_ci_timeout_parks_pending_checks_without_red_or_green_and_replays() {
    let mut doc = queue_published_document();
    let ci_index = doc.nodes.iter().position(|node| node.id == "ci").unwrap();
    let conflict_head = "a".repeat(40);
    let Some(GateCondition::Ci {
        timeout_seconds, ..
    }) = &mut doc.nodes[ci_index].condition
    else {
        unreachable!()
    };
    *timeout_seconds = 1;
    let head = "a".repeat(40);
    let mut owner = Owner {
        branch: Some("delivered/topic".into()),
        commit: Some(head.clone()),
        integration_base: Some("f".repeat(40).parse().unwrap()),
        conflict_observations: VecDeque::from([ConflictObservation {
            state: ConflictState::Clear,
            head_sha: Some(conflict_head),
            summary: "no merge conflicts".into(),
        }]),
        observations: VecDeque::from([
            observation(CiState::Pending, "awaiting check registration"),
            observation(CiState::Pending, "still awaiting checks"),
            observation(CiState::Pending, "still awaiting checks"),
        ]),
        ..Default::default()
    };
    let mut journal = Journal::default();
    let first = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(first.status, GraphStatus::Pending);
    assert_eq!(
        first.checkpoint.nodes[ci_index].ci.as_ref().unwrap().state,
        CiState::Pending
    );

    owner.wall_seconds = 1;
    owner.monotonic_micros = 1_000_000;
    let timed_out = advance(&doc, &mut owner, &mut journal).unwrap();
    let node = &timed_out.checkpoint.nodes[ci_index];
    assert_eq!(node.state, NodeState::Running);
    assert!(
        node.gate.is_none(),
        "pending CI must not manufacture a gate failure or pass"
    );
    assert_eq!(node.ci.as_ref().unwrap().state, CiState::Pending);
    assert!(
        node.ci
            .as_ref()
            .unwrap()
            .summary
            .contains("through the 1 second gate timeout")
    );
    let park = node.delivery_park.as_ref().unwrap();
    assert_eq!(
        park.reason,
        provenant_records::graph::ParkReason::CiUnavailable
    );
    let transition_id = park.transition_id.clone();

    let replay = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(
        replay.checkpoint.nodes[ci_index]
            .delivery_park
            .as_ref()
            .unwrap()
            .transition_id,
        transition_id
    );
    assert!(owner.park_operations.len() >= 2);
    assert!(
        owner
            .park_operations
            .iter()
            .all(|operation| operation == &owner.park_operations[0])
    );
    assert_eq!(
        owner.attempts, 1,
        "a pending CI timeout must not launch repair"
    );
}

#[test]
fn ci_timeout_ignores_wall_clock_steps_and_reanchors_after_boot() {
    let mut owner = Owner {
        observations: VecDeque::from([
            registered(CiState::Pending, "first"),
            registered(CiState::Pending, "forward wall step"),
            registered(CiState::Pending, "backward wall step"),
            registered(CiState::Pending, "new boot"),
            registered(CiState::Pending, "timeout"),
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
fn ci_clock_reanchors_do_not_override_the_configured_timeout() {
    let mut doc = document();
    let Some(GateCondition::Ci {
        timeout_seconds, ..
    }) = &mut doc.nodes[0].condition
    else {
        unreachable!()
    };
    let configured_timeout_seconds = 5;
    *timeout_seconds = configured_timeout_seconds;
    let mut owner = Owner {
        observations: VecDeque::from(vec![observation(CiState::Pending, "waiting"); 6]),
        monotonic_micros: 100,
        ..Default::default()
    };
    let mut journal = Journal::default();
    advance(&doc, &mut owner, &mut journal).unwrap();
    for _ in 1..=4 {
        owner.monotonic_micros -= 1;
        let result = advance(&doc, &mut owner, &mut journal).unwrap();
        assert_eq!(result.status, GraphStatus::Pending);
        assert!(
            result.checkpoint.nodes[0]
                .ci
                .as_ref()
                .unwrap()
                .clock_reanchors
                > 0
        );
        journal.0 =
            Some(serde_json::from_slice(&serde_json::to_vec(&result.checkpoint).unwrap()).unwrap());
    }
    owner.monotonic_micros += 5_000_000;
    let result = advance(&doc, &mut owner, &mut journal).unwrap();
    let node = &result.checkpoint.nodes[0];
    assert_eq!(node.state, NodeState::Failed);
    assert_eq!(result.outcome, Some(GraphOutcome::Failed));
    assert!(!node.gate.as_ref().unwrap().passed);
    let ci = node.ci.as_ref().unwrap();
    assert_eq!(ci.state, CiState::Pending);
    assert!(ci.elapsed_seconds >= configured_timeout_seconds);
    assert!(ci.clock_reanchors > 0);
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
            Some("node ci: invalid CI gate check names")
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
    let mut failure = observation(CiState::Failed, "failed check");
    failure.failed_checks.push(failed_check());
    let mut owner = Owner {
        observations: VecDeque::from([failure]),
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
            Some("node ci: invalid CI gate target or timeout")
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
        "schema_version":2, "graph_id":"node-ci", "nodes":[
            {"id":"write","access":"worktree_write","worktree":"lane","prompt":"write",
             "delivery":"pr","route":{"adapter":"fixture","model":"fixture/model"}},
            {"id":"ci","kind":"gate","access":"read_only","depends_on":["write"],
             "condition":{"type":"ci","target":{"node":"write"}}}
        ]
    }))
    .unwrap()
}

fn queue_published_document() -> GraphDocument {
    let mut document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version":2, "graph_id":"queue-ci", "integration_branch":"delivery/item",
        "nodes":[
            {"id":"write","access":"worktree_write","worktree":"lane","prompt":"write",
             "route":{"adapter":"fixture","model":"fixture/model"}},
            {"id":"hygiene","kind":"hygiene","access":"worktree_write","worktree":"lane",
             "depends_on":["write"],"source":{"type":"node","node":"write"}},
            {"id":"publish","kind":"publish","access":"worktree_write","worktree":"lane",
             "depends_on":["hygiene"],"source":{"type":"node","node":"hygiene"}},
            {"id":"conflict","kind":"gate","access":"read_only","depends_on":["publish"],
             "on_fail":"rebase","condition":{"type":"conflict","target":{"node":"publish"}}},
            {"id":"rebase","access":"worktree_write","worktree":"lane","prompt":"rebase",
             "source":{"type":"head","node":"publish"},"depends_on":["conflict"],
             "route":{"adapter":"fixture","model":"fixture/model"}},
            {"id":"ci","kind":"gate","access":"read_only","depends_on":["conflict","publish"],
             "on_fail":"repair","condition":{"type":"ci","target":{"node":"publish"}}},
            {"id":"repair","access":"worktree_write","worktree":"lane",
             "prompt":"Repair CI findings and sweep the whole defect class.",
             "source":{"type":"head","node":"publish"},"depends_on":["ci","publish"],
             "inputs":[{"node":"ci","outputs":["findings","commit"]}],
             "route":{"adapter":"fixture","model":"fixture/model"}},
            {"id":"hygiene_after_rebase","kind":"hygiene","access":"worktree_write","worktree":"lane",
             "depends_on":["rebase"],"source":{"type":"node","node":"rebase"}},
            {"id":"publish_after_rebase","kind":"publish","access":"worktree_write","worktree":"lane",
             "depends_on":["hygiene_after_rebase"],"source":{"type":"node","node":"hygiene_after_rebase"}},
            {"id":"conflict_after_rebase","kind":"gate","access":"read_only",
             "depends_on":["publish_after_rebase"],"on_fail":"rebase_again",
             "condition":{"type":"conflict","target":{"node":"publish_after_rebase"}}},
            {"id":"ci_after_rebase","kind":"gate","access":"read_only",
             "depends_on":["conflict_after_rebase"],
             "condition":{"type":"ci","target":{"node":"publish_after_rebase"}}},
            {"id":"rebase_again","kind":"loop","access":"read_only",
             "depends_on":["conflict_after_rebase","publish_after_rebase"],
             "loop":{"max_rounds":1,"until":"round_ci",
              "previous_bindings":{"refresh":"publish_after_rebase"},"body":[
                {"id":"refresh_conflict_initial","kind":"gate","access":"read_only","on_fail":"fix",
                 "condition":{"type":"conflict","target":{"node":"previous.refresh"}}},
                {"id":"fix","access":"worktree_write","worktree":"lane","prompt":"rebase the fresh publication",
                 "source":{"type":"head","node":"previous.refresh"},"depends_on":["refresh_conflict_initial"],
                 "route":{"adapter":"fixture","model":"fixture/model"}},
                {"id":"hygiene","kind":"hygiene","access":"worktree_write","worktree":"lane",
                 "depends_on":["fix"],"source":{"type":"node","node":"fix"}},
                {"id":"refresh","kind":"publish","access":"worktree_write","worktree":"lane",
                 "depends_on":["hygiene"],"source":{"type":"node","node":"hygiene"}},
                {"id":"refresh_conflict","kind":"gate","access":"read_only","depends_on":["refresh"],
                 "condition":{"type":"conflict","target":{"node":"refresh"}}},
                {"id":"refresh_ci","kind":"gate","access":"read_only","depends_on":["refresh_conflict"],
                 "condition":{"type":"ci","target":{"node":"refresh"}}},
                {"id":"round_ci","kind":"gate","access":"read_only",
                 "depends_on":["refresh_conflict_initial"],
                 "after_settled":["fix","hygiene","refresh","refresh_conflict","refresh_ci"],
                 "condition":{"type":"ci","target":{"node":"previous.refresh"}}}
             ]}},
            {"id":"hygiene_after_repair","kind":"hygiene","access":"worktree_write","worktree":"lane",
             "depends_on":["repair"],"source":{"type":"node","node":"repair"}},
            {"id":"publish_after_repair","kind":"publish","access":"worktree_write","worktree":"lane",
             "depends_on":["hygiene_after_repair"],"source":{"type":"node","node":"hygiene_after_repair"}},
            {"id":"conflict_after_repair","kind":"gate","access":"read_only",
             "depends_on":["publish_after_repair"],"on_fail":"repair_again",
             "condition":{"type":"conflict","target":{"node":"publish_after_repair"}}},
            {"id":"ci_after_repair","kind":"gate","access":"read_only",
             "depends_on":["conflict_after_repair"],
             "condition":{"type":"ci","target":{"node":"publish_after_repair"}}},
            {"id":"repair_again","kind":"loop","access":"read_only",
             "depends_on":["conflict_after_repair","publish_after_repair"],
             "loop":{"max_rounds":1,"until":"round_ci",
              "previous_bindings":{"refresh":"publish_after_repair"},"body":[
                {"id":"refresh_conflict_initial","kind":"gate","access":"read_only","on_fail":"fix",
                 "condition":{"type":"conflict","target":{"node":"previous.refresh"}}},
                {"id":"fix","access":"worktree_write","worktree":"lane","prompt":"repair the fresh publication",
                 "source":{"type":"head","node":"previous.refresh"},"depends_on":["refresh_conflict_initial"],
                 "route":{"adapter":"fixture","model":"fixture/model"}},
                {"id":"hygiene","kind":"hygiene","access":"worktree_write","worktree":"lane",
                 "depends_on":["fix"],"source":{"type":"node","node":"fix"}},
                {"id":"refresh","kind":"publish","access":"worktree_write","worktree":"lane",
                 "depends_on":["hygiene"],"source":{"type":"node","node":"hygiene"}},
                {"id":"refresh_conflict","kind":"gate","access":"read_only","depends_on":["refresh"],
                 "condition":{"type":"conflict","target":{"node":"refresh"}}},
                {"id":"refresh_ci","kind":"gate","access":"read_only","depends_on":["refresh_conflict"],
                 "condition":{"type":"ci","target":{"node":"refresh"}}},
                {"id":"round_ci","kind":"gate","access":"read_only",
                 "depends_on":["refresh_conflict_initial"],
                 "after_settled":["fix","hygiene","refresh","refresh_conflict","refresh_ci"],
                 "condition":{"type":"ci","target":{"node":"previous.refresh"}}}
             ]}}
        ]
    }))
    .unwrap();
    document.queue = Some(QueueKnobs::default());
    document
}

#[test]
fn expanded_gates_in_distinct_members_of_one_loop_share_a_round() {
    let conflict = loops::LoopRound {
        extra: Default::default(),
        loop_id: "checks".into(),
        round: 2,
        node_id: "conflict".into(),
        until: false,
        retry_on: false,
        previous_gate: Some("checks--r1--ci".into()),
    };
    let ci = loops::LoopRound {
        node_id: "ci".into(),
        until: true,
        ..conflict.clone()
    };
    let next_round = loops::LoopRound {
        round: 3,
        ..ci.clone()
    };

    assert!(super::same_expanded_round(Some(&conflict), Some(&ci)));
    assert!(!super::same_expanded_round(
        Some(&conflict),
        Some(&next_round)
    ));
    assert!(!super::same_expanded_round(Some(&conflict), None));
}

#[test]
fn queue_conflict_recheck_matches_a_bounded_round_and_reactivates_composite_rebase() {
    let mut document = queue_published_document();
    document
        .nodes
        .retain(|node| matches!(node.id.as_str(), "write" | "hygiene" | "publish"));
    document.nodes.push(
        serde_json::from_value(serde_json::json!({
            "id":"cycle","kind":"loop","access":"read_only","depends_on":["publish"],
            "loop":{"max_rounds":1,"until":"done","body":[
                {"id":"conflict","kind":"gate","access":"read_only",
                 "on_fail":"rebase","condition":{"type":"conflict","target":{"node":"publish"}}},
                {"id":"rebase","kind":"composite","access":"read_only","depends_on":["conflict"],
                 "composite":{"nodes":[
                    {"id":"fix","access":"worktree_write","worktree":"lane","prompt":"rebase",
                     "source":{"type":"head","node":"publish"},
                     "route":{"adapter":"fixture","model":"fixture/model"}}
                 ],"exports":[{"node":"fix","outputs":["result"]}]}},
                {"id":"rebase_hygiene","kind":"hygiene","access":"worktree_write","worktree":"lane",
                 "depends_on":["rebase"],"source":{"type":"node","node":"rebase"}},
                {"id":"rebase_publish","kind":"publish","access":"worktree_write","worktree":"lane",
                 "depends_on":["rebase_hygiene"],"source":{"type":"node","node":"rebase_hygiene"}},
                {"id":"rebase_conflict","kind":"gate","access":"read_only","depends_on":["rebase_publish"],
                 "condition":{"type":"conflict","target":{"node":"rebase_publish"}}},
                {"id":"rebase_ci","kind":"gate","access":"read_only","depends_on":["rebase_conflict"],
                 "condition":{"type":"ci","target":{"node":"rebase_publish"}}},
                {"id":"ci_repair","access":"worktree_write","worktree":"lane","prompt":"repair",
                 "source":{"type":"head","node":"publish"},"depends_on":["ci"],
                 "inputs":[{"node":"ci","outputs":["findings","commit"]}],
                 "route":{"adapter":"fixture","model":"fixture/model"}},
                {"id":"ci_hygiene","kind":"hygiene","access":"worktree_write","worktree":"lane",
                 "depends_on":["ci_repair"],"source":{"type":"node","node":"ci_repair"}},
                {"id":"ci_publish","kind":"publish","access":"worktree_write","worktree":"lane",
                 "depends_on":["ci_hygiene"],"source":{"type":"node","node":"ci_hygiene"}},
                {"id":"ci_conflict","kind":"gate","access":"read_only","depends_on":["ci_publish"],
                 "condition":{"type":"conflict","target":{"node":"ci_publish"}}},
                {"id":"ci_refresh","kind":"gate","access":"read_only","depends_on":["ci_conflict"],
                 "condition":{"type":"ci","target":{"node":"ci_publish"}}},
                {"id":"ci","kind":"gate","access":"read_only","depends_on":["conflict"],
                 "on_fail":"ci_repair","condition":{"type":"ci","target":{"node":"publish"}}},
                {"id":"done","kind":"gate","access":"read_only",
                 "depends_on":["ci"],
                 "after_settled":["conflict","rebase","rebase_hygiene","rebase_publish","rebase_conflict",
                                  "rebase_ci","ci_repair","ci_hygiene","ci_publish","ci_conflict","ci_refresh"],
                 "condition":{"type":"budget"}}
            ]}
        }))
        .unwrap(),
    );
    let limits = crate::config::GraphLimits::default();
    let expanded = loops::expand(&document, &[], &limits).unwrap();
    let (plan, positions) = validate_flat_plan(&expanded, &limits).unwrap();
    let conflict_index = positions["cycle--r1--conflict"];
    let rebase_index = positions["cycle--r1--rebase"];
    let rebase_writer_index = positions["cycle--r1--rebase--fix"];
    let ci_index = positions["cycle--r1--ci"];
    let conflict_round = plan.nodes[conflict_index].round.as_ref().unwrap();
    let ci_round = plan.nodes[ci_index].round.as_ref().unwrap();
    assert_eq!(
        (conflict_round.loop_id.as_str(), conflict_round.round),
        ("cycle", 1)
    );
    assert_eq!((ci_round.loop_id.as_str(), ci_round.round), ("cycle", 1));
    assert_ne!(conflict_round.node_id, ci_round.node_id);

    let mut checkpoint = transitions::initial_checkpoint(
        &expanded,
        &plan,
        "queue-loop-conflict-recheck".into(),
        None,
        None,
        None,
    );
    let head = "a".repeat(40);
    let publication = &mut checkpoint.nodes[positions["publish"]];
    publication.state = NodeState::Succeeded;
    publication.outputs = Some(NodeOutputs {
        branch: Some("provenant/graph/queue-ci".into()),
        commit: Some(head.clone()),
        publication: Some(DeliveryPublication {
            branch: "provenant/graph/queue-ci".into(),
            head: head.clone(),
            pull_request: "https://example.invalid/pr/ci".into(),
        }),
        ..Default::default()
    });
    let conflict = &mut checkpoint.nodes[conflict_index];
    conflict.state = NodeState::Succeeded;
    conflict.gate = Some(GateResult {
        extra: Default::default(),
        passed: true,
        reason: None,
        on_fail: Some("cycle--r1--rebase".into()),
    });
    conflict.conflict = Some(super::ci::ConflictEvidence {
        state: ConflictState::Clear,
        head_sha: Some(head.clone()),
        summary: "clear at published head".into(),
        wake_at: None,
        pending_observations: 0,
        ci_rechecks: 0,
    });
    checkpoint.nodes[rebase_index].state = NodeState::Skipped;
    checkpoint.nodes[rebase_writer_index].state = NodeState::Skipped;
    checkpoint.nodes[ci_index].state = NodeState::Running;
    checkpoint.nodes[ci_index].ci = Some(CiEvidence {
        wake_at: None,
        pending_observations: 1,
        extra: Default::default(),
        first_observed_stamp: None,
        clock_reanchors: 0,
        clock_reanchor_reason: None,
        state: CiState::Conflict,
        summary: "PR base moved".into(),
        head_sha: Some(head),
        failed_checks: Vec::new(),
        check_names: vec!["build".into()],
        truncated: false,
        elapsed_seconds: 1,
    });
    let condition = plan.nodes[ci_index].condition.clone().unwrap();

    let reset = super::queue_conflict_recheck(
        ci_index,
        &condition,
        &plan,
        &positions,
        &mut checkpoint,
        Some(provenant_core::WallTime::from_micros(1_000_000).unwrap()),
    )
    .unwrap();

    assert_eq!(reset, (conflict_index, rebase_index));
    assert_eq!(checkpoint.nodes[conflict_index].state, NodeState::Running);
    assert!(checkpoint.nodes[conflict_index].gate.is_none());
    assert_eq!(
        checkpoint.nodes[conflict_index]
            .conflict
            .as_ref()
            .unwrap()
            .wake_at
            .unwrap()
            .as_micros(),
        1_000_000,
        "a conflict recheck wakes at the observed wall time"
    );
    assert_eq!(
        checkpoint.nodes[conflict_index]
            .conflict
            .as_ref()
            .unwrap()
            .ci_rechecks,
        1
    );
    assert_eq!(checkpoint.nodes[rebase_index].state, NodeState::Pending);
    assert_eq!(
        checkpoint.nodes[rebase_writer_index].state,
        NodeState::Pending
    );
    assert_eq!(checkpoint.nodes[ci_index].state, NodeState::Pending);
    assert!(checkpoint.nodes[ci_index].ci.is_none());
}

#[test]
fn queue_conflict_recheck_replays_into_a_later_recovery_round() {
    let mut document = queue_published_document();
    document
        .nodes
        .retain(|node| matches!(node.id.as_str(), "write" | "hygiene" | "publish"));
    document.nodes.extend(
        serde_json::from_value::<Vec<GraphNode>>(serde_json::json!([
            {"id":"conflict","kind":"gate","access":"read_only","depends_on":["publish"],
             "on_fail":"rebase","condition":{"type":"conflict","target":{"node":"publish"}}},
            {"id":"rebase","kind":"loop","access":"read_only","depends_on":["conflict","publish"],
             "loop":{"max_rounds":2,"until":"round_ci",
              "previous_bindings":{"republish":"publish"},"body":[
                {"id":"conflict","kind":"gate","access":"read_only","on_fail":"fix",
                 "condition":{"type":"conflict","target":{"node":"previous.republish"}}},
                {"id":"fix","access":"worktree_write","worktree":"lane","prompt":"rebase",
                 "source":{"type":"head","node":"previous.republish"},"depends_on":["conflict"],
                 "route":{"adapter":"fixture","model":"fixture/model"}},
                {"id":"hygiene","kind":"hygiene","access":"worktree_write","worktree":"lane",
                 "depends_on":["fix"],"source":{"type":"node","node":"fix"}},
                {"id":"republish","kind":"publish","access":"worktree_write","worktree":"lane",
                 "depends_on":["hygiene"],"source":{"type":"node","node":"hygiene"}},
                {"id":"fresh_conflict","kind":"gate","access":"read_only","depends_on":["republish"],
                 "condition":{"type":"conflict","target":{"node":"republish"}}},
                {"id":"fresh_ci","kind":"gate","access":"read_only","depends_on":["fresh_conflict"],
                 "condition":{"type":"ci","target":{"node":"republish"}}},
                {"id":"round_ci","kind":"gate","access":"read_only","depends_on":["conflict"],
                 "after_settled":["fix","hygiene","republish","fresh_conflict","fresh_ci"],
                 "condition":{"type":"ci","target":{"node":"previous.republish"}}}
             ]}},
            {"id":"ci","kind":"gate","access":"read_only","depends_on":["conflict","publish"],
             "condition":{"type":"ci","target":{"node":"publish"}}}
        ]))
        .unwrap(),
    );

    let limits = crate::config::GraphLimits::default();
    let expanded = loops::expand(&document, &[], &limits).unwrap();
    let (plan, positions) = validate_flat_plan(&expanded, &limits).unwrap();
    let conflict_index = positions["conflict"];
    let rebase_index = positions["rebase"];
    let first_until_index = positions["rebase--r1--round_ci"];
    let second_conflict_index = positions["rebase--r2--conflict"];
    let second_fix_index = positions["rebase--r2--fix"];
    let ci_index = positions["ci"];
    let mut checkpoint = transitions::initial_checkpoint(
        &expanded,
        &plan,
        "queue-conflict-recovery-round-replay".into(),
        None,
        None,
        None,
    );
    let head = "a".repeat(40);
    checkpoint.nodes[positions["publish"]].state = NodeState::Succeeded;
    checkpoint.nodes[positions["publish"]].outputs = Some(NodeOutputs {
        branch: Some("provenant/graph/queue-ci".into()),
        commit: Some(head.clone()),
        publication: Some(DeliveryPublication {
            branch: "provenant/graph/queue-ci".into(),
            head: head.clone(),
            pull_request: "https://example.invalid/pr/ci".into(),
        }),
        ..Default::default()
    });
    checkpoint.nodes[conflict_index].state = NodeState::Succeeded;
    checkpoint.nodes[conflict_index].gate = Some(GateResult {
        extra: Default::default(),
        passed: true,
        reason: None,
        on_fail: Some("rebase".into()),
    });
    checkpoint.nodes[conflict_index].conflict = Some(super::ci::ConflictEvidence {
        state: ConflictState::Clear,
        head_sha: Some(head.clone()),
        summary: "clear at published head".into(),
        wake_at: None,
        pending_observations: 0,
        ci_rechecks: 0,
    });
    checkpoint.nodes[rebase_index].state = NodeState::Skipped;
    for id in [
        "rebase--r1--conflict",
        "rebase--r1--fix",
        "rebase--r1--hygiene",
        "rebase--r1--republish",
        "rebase--r1--fresh_conflict",
        "rebase--r1--fresh_ci",
        "rebase--r1--round_ci",
        "rebase--r2--conflict",
        "rebase--r2--fix",
        "rebase--r2--hygiene",
        "rebase--r2--republish",
        "rebase--r2--fresh_conflict",
        "rebase--r2--fresh_ci",
        "rebase--r2--round_ci",
    ] {
        checkpoint.nodes[positions[id]].state = NodeState::Skipped;
    }
    checkpoint.nodes[ci_index].state = NodeState::Running;
    checkpoint.nodes[ci_index].ci = Some(CiEvidence {
        wake_at: None,
        pending_observations: 1,
        extra: Default::default(),
        first_observed_stamp: None,
        clock_reanchors: 0,
        clock_reanchor_reason: None,
        state: CiState::Conflict,
        summary: "PR base moved".into(),
        head_sha: Some(head),
        failed_checks: Vec::new(),
        check_names: vec!["build".into()],
        truncated: false,
        elapsed_seconds: 1,
    });
    let condition = plan.nodes[ci_index].condition.clone().unwrap();
    super::queue_conflict_recheck(
        ci_index,
        &condition,
        &plan,
        &positions,
        &mut checkpoint,
        Some(provenant_core::WallTime::from_micros(1_000_000).unwrap()),
    )
    .unwrap();

    for id in [
        "rebase--r1--conflict",
        "rebase--r1--fix",
        "rebase--r1--hygiene",
        "rebase--r1--republish",
        "rebase--r1--fresh_conflict",
        "rebase--r1--fresh_ci",
        "rebase--r1--round_ci",
        "rebase--r2--conflict",
        "rebase--r2--fix",
        "rebase--r2--hygiene",
        "rebase--r2--republish",
        "rebase--r2--fresh_conflict",
        "rebase--r2--fresh_ci",
        "rebase--r2--round_ci",
    ] {
        assert_eq!(checkpoint.nodes[positions[id]].state, NodeState::Pending);
    }

    let mut replay: GraphCheckpoint =
        serde_json::from_value(serde_json::to_value(&checkpoint).unwrap()).unwrap();
    let mut inactive_replay = replay.clone();
    inactive_replay.nodes[first_until_index].state = NodeState::Succeeded;
    assert!(matches!(
        super::transitions::node_action(second_fix_index, &plan, &mut inactive_replay, &positions),
        super::transitions::NodeAction::Wait { changed: true }
    ));
    assert_eq!(
        inactive_replay.nodes[second_fix_index].state,
        NodeState::Skipped,
        "a successful prior until gate keeps the later recovery round inactive"
    );

    replay.nodes[conflict_index].state = NodeState::Failed;
    replay.nodes[conflict_index].gate = Some(GateResult {
        extra: Default::default(),
        passed: false,
        reason: Some("CONFLICTING".into()),
        on_fail: Some("rebase".into()),
    });
    replay.nodes[first_until_index].state = NodeState::Failed;
    replay.nodes[first_until_index].gate = Some(GateResult {
        extra: Default::default(),
        passed: false,
        reason: Some("CI still reports a conflict".into()),
        on_fail: None,
    });
    replay.nodes[second_conflict_index].state = NodeState::Failed;
    replay.nodes[second_conflict_index].gate = Some(GateResult {
        extra: Default::default(),
        passed: false,
        reason: Some("CONFLICTING at the previous publication".into()),
        on_fail: Some("rebase--r2--fix".into()),
    });
    assert!(matches!(
        super::transitions::node_action(rebase_index, &plan, &mut replay, &positions),
        super::transitions::NodeAction::Loop(_)
    ));
    assert!(matches!(
        super::transitions::node_action(second_fix_index, &plan, &mut replay, &positions),
        super::transitions::NodeAction::Attempt
    ));
    assert_eq!(replay.nodes[second_fix_index].state, NodeState::Pending);
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
fn ci_node_target_keeps_a_moved_published_head_pending_even_if_checks_are_green() {
    let mut green = observation(CiState::Passed, "green for a different head");
    green.head_sha = Some("b".repeat(40));
    let head = "a".repeat(40);
    let mut owner = Owner {
        branch: Some("delivered/topic".into()),
        commit: Some(head.clone()),
        integration_base: Some("f".repeat(40).parse().unwrap()),
        conflict_observations: VecDeque::from([ConflictObservation {
            state: ConflictState::Clear,
            head_sha: Some(head.clone()),
            summary: "no merge conflicts".into(),
        }]),
        observations: VecDeque::from([green]),
        ..Default::default()
    };
    let doc = queue_published_document();
    let result = advance(&doc, &mut owner, &mut Journal::default()).unwrap();
    let ci_index = doc.nodes.iter().position(|node| node.id == "ci").unwrap();
    let ci = result.checkpoint.nodes[ci_index].ci.as_ref().unwrap();
    assert_eq!(result.checkpoint.nodes[ci_index].state, NodeState::Running);
    assert!(result.checkpoint.nodes[ci_index].gate.is_none());
    assert_eq!(ci.state, CiState::Pending);
    assert_eq!(ci.head_sha.as_deref(), Some(head.as_str()));
    assert!(
        ci.summary
            .contains("waiting for publication reconciliation")
    );
    assert!(ci.failed_checks.is_empty());
}

#[test]
fn queue_ci_without_a_published_head_does_not_reach_or_settle_the_gate() {
    let doc = queue_published_document();
    let ci_index = doc.nodes.iter().position(|node| node.id == "ci").unwrap();
    let mut owner = Owner {
        branch: Some("delivered/topic".into()),
        commit: None,
        ..Default::default()
    };

    let run = advance(&doc, &mut owner, &mut Journal::default()).unwrap();

    let ci = &run.checkpoint.nodes[ci_index];
    assert_eq!(ci.state, NodeState::Pending);
    assert!(ci.gate.is_none());
    assert!(ci.outputs.is_none());
    assert!(ci.ci.is_none());
    let repair_index = doc
        .nodes
        .iter()
        .position(|node| node.id == "repair")
        .unwrap();
    let repair = &run.checkpoint.nodes[repair_index];
    assert_eq!(repair.state, NodeState::Pending);
    assert!(
        owner.targets.is_empty(),
        "CI must not be queried without a published head to bind"
    );
    assert_eq!(
        owner.attempts, 1,
        "no CI repair may launch without a published head"
    );
}

#[test]
fn queue_ci_missing_published_head_ignores_green_and_red_reports() {
    let document = queue_published_document();
    let (plan, positions) = validate_plan(&document).unwrap();
    let ci_index = positions["ci"];
    for state in [CiState::Passed, CiState::Failed] {
        let mut report = registered(state, "report without a bound publication");
        if state == CiState::Failed {
            report.failed_checks.push(failed_check());
        }
        let mut owner = Owner {
            observations: VecDeque::from([report]),
            ..Default::default()
        };
        let mut checkpoint = transitions::initial_checkpoint(
            &document,
            &plan,
            "queue-ci-missing-head".into(),
            None,
            None,
            None,
        );
        checkpoint.nodes[ci_index].state = NodeState::Running;
        let wall_now = owner.wall_now();
        let backoff = owner.ci_poll_backoff("ci");

        let evaluation = super::ci::evaluate(
            true,
            ci_index,
            &plan.nodes[ci_index],
            &mut checkpoint,
            &mut owner,
            wall_now,
            backoff,
        )
        .unwrap();

        assert!(matches!(evaluation, super::ci::Evaluation::Pending));
        let progress = &checkpoint.nodes[ci_index];
        assert_eq!(progress.state, NodeState::Running);
        assert!(progress.gate.is_none());
        assert!(progress.outputs.is_none());
        assert_eq!(progress.ci.as_ref().unwrap().state, CiState::Pending);
        assert!(progress.ci.as_ref().unwrap().head_sha.is_none());
        assert!(progress.ci.as_ref().unwrap().failed_checks.is_empty());
        assert!(owner.targets.is_empty());
        assert_eq!(owner.observations.len(), 1);
    }
}

#[test]
fn a_v1_graph_without_queue_keeps_branch_only_ci_target_semantics() {
    let mut doc = node_target_document();
    doc.schema_version = 1;
    doc.queue = None;
    let mut owner = Owner {
        branch: Some("delivered/topic".into()),
        commit: None,
        observations: VecDeque::from([observation(CiState::Passed, "green")]),
        ..Default::default()
    };
    let result = advance(&doc, &mut owner, &mut Journal::default()).unwrap();
    assert_eq!(result.checkpoint.nodes[1].state, NodeState::Succeeded);
    assert_eq!(
        result.checkpoint.nodes[1].ci.as_ref().unwrap().state,
        CiState::Passed
    );
}

#[test]
fn conflict_node_target_does_not_rebase_a_different_head() {
    let mut doc = node_target_document();
    doc.schema_version = 2;
    doc.nodes[1].condition = Some(GateCondition::Conflict {
        extra: Default::default(),
        target: CiTarget::Node {
            node: "write".into(),
            extra: Default::default(),
        },
    });
    doc.nodes[1].on_fail = Some("rebase".into());
    doc.nodes.push(
        serde_json::from_value(serde_json::json!({
            "id":"rebase","access":"worktree_write","worktree":"lane",
            "prompt":"Rebase the retained branch","depends_on":["ci"],
            "route":{"adapter":"fixture","model":"fixture/model"}
        }))
        .unwrap(),
    );
    let mut owner = Owner {
        branch: Some("delivered/topic".into()),
        commit: Some("a".repeat(40)),
        conflict_observations: VecDeque::from([ConflictObservation {
            state: ConflictState::Conflicting,
            head_sha: Some("b".repeat(40)),
            summary: "conflicting old branch head".into(),
        }]),
        ..Default::default()
    };
    let result = advance(&doc, &mut owner, &mut Journal::default()).unwrap();
    let conflict = &result.checkpoint.nodes[1];
    assert_eq!(conflict.state, NodeState::Running);
    assert!(conflict.gate.is_none());
    assert_eq!(
        conflict.conflict.as_ref().unwrap().state,
        ConflictState::Pending
    );
    assert_eq!(
        conflict.conflict.as_ref().unwrap().head_sha,
        Some("a".repeat(40))
    );
    assert_eq!(result.checkpoint.nodes[2].state, NodeState::Pending);
    assert_eq!(owner.attempts, 1);
}

#[test]
fn queue_conflict_without_a_published_head_waits_instead_of_rebasing() {
    let doc = queue_published_document();
    let mut owner = Owner {
        branch: Some("delivered/topic".into()),
        commit: None,
        conflict_observations: VecDeque::from([ConflictObservation {
            state: ConflictState::Conflicting,
            head_sha: Some("a".repeat(40)),
            summary: "branch conflicts".into(),
        }]),
        ..Default::default()
    };

    let run = advance(&doc, &mut owner, &mut Journal::default()).unwrap();

    let conflict_index = doc
        .nodes
        .iter()
        .position(|node| node.id == "conflict")
        .unwrap();
    let rebase_index = doc
        .nodes
        .iter()
        .position(|node| node.id == "rebase")
        .unwrap();
    let conflict = &run.checkpoint.nodes[conflict_index];
    assert_ne!(conflict.state, NodeState::Succeeded);
    assert!(conflict.gate.is_none());
    assert!(conflict.conflict.is_none());
    assert_ne!(
        run.checkpoint.nodes[rebase_index].state,
        NodeState::Succeeded
    );
    assert_eq!(owner.attempts, 1);
    assert!(owner.targets.is_empty());
}

#[test]
fn ci_node_target_requires_a_delivered_branch_and_upstream_dependency() {
    let doc = node_target_document();
    let mut owner = Owner::default();
    let result = advance(&doc, &mut owner, &mut Journal::default()).unwrap();
    assert_eq!(result.checkpoint.nodes[1].state, NodeState::Running);
    assert_eq!(
        result.checkpoint.nodes[1].ci.as_ref().unwrap().state,
        CiState::Pending
    );
    assert!(result.checkpoint.nodes[1].gate.is_none());
    assert!(owner.targets.is_empty());
    let mut doc = doc;
    doc.nodes[1].depends_on.clear();
    assert_eq!(
        validate_plan(&doc).err(),
        Some("node ci: CI target node must be an upstream dependency")
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
        job_id: Some("12345678".into()),
        name: "test".into(),
        url: Some("https://checks/1".into()),
        excerpt: "assertion failed".into(),
    }
}

#[test]
fn failed_job_identity_is_kept_only_when_bounded_and_numeric() {
    let mut observation = observation(CiState::Failed, "failed");
    let mut check = failed_check();
    check.job_id = Some("7x".into());
    observation.failed_checks.push(check);
    observation.bound_failed_checks(256, 1);
    assert!(observation.truncated);
    assert_eq!(observation.failed_checks[0].job_id, None);
    assert!(canonical_len(&observation.failed_checks) <= 256);

    let json = serde_json::to_value(failed_check()).unwrap();
    assert_eq!(json["job_id"], "12345678");
}

#[test]
fn only_complete_delivery_review_failures_advance_the_ladder() {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 2,
        "graph_id": "review-escalation",
        "queue": QueueKnobs::default(),
        "nodes": [{
            "id": "review",
            "access": "read_only",
            "prompt": "review the repair",
            "route": {"alias": "senior"},
            "delivery_review": {}
        }, {
            "id": "gate",
            "kind": "gate",
            "access": "read_only",
            "depends_on": ["review"],
            "inputs": [{"node": "review", "outputs": ["verdict"]}],
            "condition": {"type": "verdict", "expected": "VERDICT: PASS", "threshold": "all"}
        }]
    }))
    .unwrap();
    let (plan, positions) = validate_plan(&document).unwrap();
    let mut checkpoint = transitions::initial_checkpoint(
        &document,
        &plan,
        "review-escalation-digest".into(),
        None,
        None,
        None,
    );
    checkpoint.nodes[0].state = NodeState::Succeeded;
    checkpoint.nodes[0].outputs = Some(NodeOutputs {
        verdict: Some("VERDICT: FAIL".into()),
        findings_available: Some(true),
        ..Default::default()
    });

    assert!(delivery_review_gate_failed(
        plan.nodes[1].condition.as_ref().unwrap(),
        &plan.nodes[1],
        &plan,
        &checkpoint,
        &positions,
    ));

    checkpoint.nodes[0]
        .outputs
        .as_mut()
        .unwrap()
        .findings_available = None;
    assert!(!delivery_review_gate_failed(
        plan.nodes[1].condition.as_ref().unwrap(),
        &plan.nodes[1],
        &plan,
        &checkpoint,
        &positions,
    ));
}

#[test]
fn ladder_exhaustion_park_replay_reuses_one_operation_identity() {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 2,
        "graph_id": "park-replay",
        "queue": QueueKnobs::default(),
        "nodes": [{"id":"park", "access":"read_only", "prompt":"park", "route":{"alias":"senior"}}]
    }))
    .unwrap();
    let (plan, _) = validate_plan(&document).unwrap();
    let mut checkpoint = transitions::initial_checkpoint(
        &document,
        &plan,
        "park-replay-digest".into(),
        None,
        None,
        None,
    );
    let attempt_key = AttemptKey {
        extra: Default::default(),
        retry_generation: None,
        previous_attempt_ref: None,
        graph_id: document.graph_id.clone(),
        node_id: "repair".into(),
        attempt: 1,
        route_index: 0,
    };
    let mut owner = Owner::default();
    let mut journal = Journal::default();
    let head = "a".repeat(40);

    for _ in 0..2 {
        park_delivery_transition(
            &document,
            0,
            provenant_records::graph::ParkReason::LadderExhausted,
            Some(&attempt_key),
            Some("attempt-ref"),
            "the repair ladder is exhausted",
            Some(head.as_str()),
            &["final review failed".into()],
            &mut owner,
            &mut journal,
            &mut checkpoint,
        )
        .unwrap();
    }

    assert_eq!(owner.park_operations.len(), 2);
    assert_eq!(owner.park_operations[0], owner.park_operations[1]);
    let saved = checkpoint.nodes[0].delivery_park.as_ref().unwrap();
    assert_eq!(
        saved.reason,
        provenant_records::graph::ParkReason::LadderExhausted
    );
    assert_eq!(saved.operation_id, owner.park_operations[0]);
}

// Spec: docs/specs/fabric.md#checkpoint-envelope
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
    let secret = "AKIAIOSFODNN7EXAMPLE";
    let mut check = failed_check();
    check.excerpt.push_str(&format!(" using {secret}"));
    failure.failed_checks.push(check);
    let mut owner = Owner {
        observations: VecDeque::from([failure]),
        ..Default::default()
    };
    let mut journal = Journal::default();
    let run = advance(&doc, &mut owner, &mut journal).unwrap();
    let outputs = run.checkpoint.nodes[0].outputs.as_ref().unwrap();
    assert!(
        outputs.commit.is_none(),
        "legacy CI gates retain their prior outputs"
    );
    assert!(
        outputs.findings[0]
            .summary
            .starts_with("test (job 12345678): assertion failed")
    );
    assert!(
        serde_json::to_string(&run.checkpoint)
            .unwrap()
            .contains(secret)
    );
    assert!(owner.prompts[0].contains(secret));
    assert!(owner.prompts[0].contains("12345678"));
    assert_eq!(outputs.findings[0].url.as_deref(), Some("https://checks/1"));
    assert!(owner.prompts[0].contains("assertion failed"));
    assert!(owner.prompts[0].contains("https://checks/1"));
    assert_eq!(
        advance(&doc, &mut owner, &mut journal).unwrap().checkpoint,
        run.checkpoint
    );
}

#[test]
fn queue_ci_repair_receives_failing_head_findings_and_sweep_instruction() {
    let mut doc = queue_published_document();
    let ci_index = doc.nodes.iter().position(|node| node.id == "ci").unwrap();
    let repair_index = doc
        .nodes
        .iter()
        .position(|node| node.id == "repair")
        .unwrap();
    doc.nodes[repair_index].prompt =
        "Repair the failed CI head and sweep the whole defect class.".into();
    let head = "a".repeat(40);
    let mut failure = observation(CiState::Failed, "failed");
    failure.failed_checks.push(failed_check());
    let mut green_after_repair = observation(
        CiState::Passed,
        "checks passed for the repaired publication",
    );
    green_after_repair.head_sha = Some(head.clone());
    let mut owner = Owner {
        branch: Some("delivered/topic".into()),
        commit: Some(head.clone()),
        integration_base: Some("f".repeat(40).parse().unwrap()),
        conflict_observations: VecDeque::from([
            ConflictObservation {
                state: ConflictState::Clear,
                head_sha: Some(head.clone()),
                summary: "no merge conflicts".into(),
            },
            ConflictObservation {
                state: ConflictState::Clear,
                head_sha: Some(head.clone()),
                summary: "no merge conflicts after repair".into(),
            },
        ]),
        observations: VecDeque::from([failure, green_after_repair]),
        ..Default::default()
    };
    let mut journal = Journal::default();

    let run = advance(&doc, &mut owner, &mut journal).unwrap();

    let ci = &run.checkpoint.nodes[ci_index];
    assert_eq!(
        ci.outputs.as_ref().unwrap().commit.as_deref(),
        Some(head.as_str())
    );
    assert_eq!(ci.state, NodeState::Failed);
    let repair = &run.checkpoint.nodes[repair_index];
    assert_eq!(repair.state, NodeState::Succeeded);
    assert_eq!(repair.source_commit.as_deref(), Some(head.as_str()));
    assert_eq!(
        owner.source_commits,
        vec![Some(head.clone()), Some(head.clone())]
    );
    let manifest = repair.attempts[0].input_manifest.as_ref().unwrap();
    assert_eq!(manifest.source_commit.as_deref(), Some(head.as_str()));
    assert_eq!(manifest.inputs.len(), 2);
    assert!(manifest.inputs.iter().any(|input| {
        input.kind == OutputKind::Findings
            && input.producer_node == "ci"
            && input.delivery == InputDelivery::Delivered
    }));
    assert!(manifest.inputs.iter().any(|input| {
        input.kind == OutputKind::Commit
            && input.producer_node == "ci"
            && input.commit.as_deref() == Some(head.as_str())
            && input.delivery == InputDelivery::Delivered
    }));
    let prompt = &owner.prompts[1];
    assert!(prompt.contains(&format!("ci commit: \"{head}\"")));
    assert!(prompt.contains("12345678"));
    assert!(prompt.contains("assertion failed"));
    assert!(prompt.contains("whole defect class"));
}

#[test]
fn ci_failure_without_findings_does_not_launch_repair() {
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
    let mut owner = Owner {
        observations: VecDeque::from([observation(CiState::Failed, "failed")]),
        ..Default::default()
    };
    let mut journal = Journal::default();
    let run = advance(&doc, &mut owner, &mut journal).unwrap();
    assert!(
        run.checkpoint.nodes[0]
            .outputs
            .as_ref()
            .unwrap()
            .findings
            .is_empty()
    );
    assert_eq!(run.checkpoint.nodes[1].state, NodeState::Skipped);
    assert!(owner.prompts.is_empty());
}

fn recovery_after_findingless_failure(recovery: serde_json::Value) -> (Vec<NodeState>, usize) {
    let mut doc = document();
    doc.nodes[0].on_fail = Some("recovery".into());
    doc.nodes.push(serde_json::from_value(recovery).unwrap());
    let mut owner = Owner {
        observations: VecDeque::from([observation(CiState::Failed, "failed")]),
        ..Default::default()
    };
    let run = advance(&doc, &mut owner, &mut Journal::default()).unwrap();
    (
        run.checkpoint.nodes.iter().map(|n| n.state).collect(),
        owner.prompts.len(),
    )
}

#[test]
fn findingless_ci_failure_still_runs_a_recovery_node_that_ignores_findings() {
    let node = |extra: serde_json::Value| {
        let mut node = serde_json::json!({
            "id":"recovery","access":"read_only","prompt":"notify","depends_on":["ci"],
            "route":{"adapter":"fixture","model":"fixture/model"}
        });
        node.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        node
    };
    let (states, prompts) = recovery_after_findingless_failure(node(serde_json::json!({})));
    assert_eq!(states[1], NodeState::Succeeded);
    assert_eq!(prompts, 1);
    // The node can opt in to skipping, or a findings consumer can opt out.
    let (states, _) = recovery_after_findingless_failure(node(
        serde_json::json!({"skip_without_findings": true}),
    ));
    assert_eq!(states[1], NodeState::Skipped);
    let (states, prompts) = recovery_after_findingless_failure(node(serde_json::json!({
        "skip_without_findings": false,
        "inputs":[{"node":"ci","outputs":["findings"]}]
    })));
    assert_eq!(states[1], NodeState::Succeeded);
    assert_eq!(prompts, 1);
}

#[test]
fn findingless_ci_failure_respects_loop_recovery_override() {
    for skip in [true, false] {
        let mut doc = document();
        doc.nodes[0].on_fail = Some("recovery".into());
        let inputs = if skip {
            serde_json::json!([])
        } else {
            serde_json::json!([{"node":"ci","outputs":["findings"]}])
        };
        doc.nodes.push(
            serde_json::from_value(serde_json::json!({
                "id":"recovery", "kind":"loop", "access":"read_only", "depends_on":["ci"],
                "skip_without_findings":skip, "inputs":inputs,
                "loop":{"max_rounds":1,"until":"gate","body":[
                    {"id":"notify","access":"read_only","prompt":"notify",
                     "route":{"adapter":"fixture","model":"fixture/model"}},
                    {"id":"gate","kind":"gate","access":"read_only","depends_on":["notify"],
                     "inputs":[{"node":"notify","outputs":["verdict"]}],
                     "condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"}}
                ]}
            }))
            .unwrap(),
        );
        let mut owner = Owner {
            observations: VecDeque::from([observation(CiState::Failed, "failed")]),
            ..Default::default()
        };
        let run = advance(&doc, &mut owner, &mut Journal::default()).unwrap();
        let child = run
            .checkpoint
            .nodes
            .iter()
            .find(|n| n.id == "recovery--r1--notify")
            .unwrap();
        assert_eq!(
            child.state,
            if skip {
                NodeState::Skipped
            } else {
                NodeState::Succeeded
            }
        );
        assert_eq!(owner.prompts.len(), usize::from(!skip));
    }
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
    let error = advance(&doc, &mut Owner::default(), &mut Journal::default()).unwrap_err();
    assert_eq!(error.code(), "invalid_graph");
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
