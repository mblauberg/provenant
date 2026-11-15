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
    fn worktree_layout(&self) -> provenant_config::WorktreeLayoutPort {
        provenant_config::PortTable::builtin().worktree_layout
    }
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
            settlement_reason: Some(provenant_records::run::SettlementReason::Completed),
            stop: None,
        })
    }
}

fn document() -> GraphDocument {
    super::model::document_fixture(serde_json::json!({ "graph_id": "g-ci", "nodes": [{
        "id": "ci", "kind": "gate", "access": "read_only", "condition": {"type": "ci", "target": "feature/example"}
    }] }))
}

fn expected_wake(
    wall_seconds: i64,
    backoff: CiPollBackoff,
    prior_observations: u64,
) -> provenant_core::WallTime {
    let seconds = wall_seconds.saturating_add(backoff.delay_seconds(prior_observations) as i64);
    provenant_core::WallTime::from_unix_seconds(seconds).unwrap()
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
    for state in CiState::ALL {
        let (expected_state, has_gate, reason_present) = match state {
            CiState::Passed => (NodeState::Succeeded, true, false),
            CiState::Pending => (NodeState::Running, false, false),
            CiState::Failed | CiState::Unavailable | CiState::Conflict | CiState::NotScheduled => {
                (NodeState::Failed, true, true)
            }
        };
        let mut owner = Owner {
            observations: VecDeque::from([observation(*state, "checks: fixture")]),
            ..Default::default()
        };
        let mut journal = Journal::default();
        let run = advance(&document(), &mut owner, &mut journal).unwrap();
        let node = &run.checkpoint.nodes[0];
        assert_eq!(node.state, expected_state);
        assert_eq!(node.gate.is_some(), has_gate);
        if let Some(gate) = &node.gate {
            assert_eq!(gate.reason.is_some(), reason_present);
        }
        let evidence = node.ci.as_ref().unwrap();
        assert_eq!(evidence.state, *state);
        assert_eq!(evidence.summary, "checks: fixture");
        assert!(node.attempts.is_empty());
        assert_eq!(
            advance(&document(), &mut owner, &mut journal)
                .unwrap()
                .checkpoint,
            run.checkpoint
        );
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
    assert!(node.attempts.is_empty());
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
    assert!(finished.checkpoint.nodes[0].attempts.is_empty());
}

#[test]
fn ci_tick_replays_backoff_and_tracks_head_changes() {
    let backoff = CiPollBackoff {
        initial_seconds: 60,
        max_seconds: 240,
    };
    for changes_head in [false, true] {
        let (doc, ci_index, mut owner, third_head) = if changes_head {
            let mut new_head = observation(CiState::Pending, "new push");
            new_head.head_sha = Some("b".repeat(40));
            (
                node_target_document(),
                1,
                Owner {
                    tick: true,
                    backoff,
                    branch: Some("delivered/topic".into()),
                    commit: Some("a".repeat(40)),
                    observations: VecDeque::from([
                        observation(CiState::Pending, "old push"),
                        observation(CiState::Pending, "old push still"),
                        new_head,
                    ]),
                    ..Default::default()
                },
                "b".repeat(40),
            )
        } else {
            let mut no_head = observation(CiState::Pending, "head omitted");
            no_head.head_sha = None;
            (
                document(),
                0,
                Owner {
                    tick: true,
                    backoff,
                    observations: VecDeque::from([
                        observation(CiState::Pending, "waiting"),
                        no_head,
                        observation(CiState::Passed, "green"),
                    ]),
                    ..Default::default()
                },
                "a".repeat(40),
            )
        };
        let mut journal = Journal::default();
        let first = advance(&doc, &mut owner, &mut journal).unwrap();
        let first_wake = expected_wake(0, backoff, 0);
        assert_eq!(first.checkpoint.wake_at, Some(first_wake));
        journal.0 =
            Some(serde_json::from_slice(&serde_json::to_vec(&first.checkpoint).unwrap()).unwrap());

        owner.wall_seconds = first_wake.unix_seconds().saturating_sub(1);
        owner.monotonic_micros = u64::try_from(first_wake.as_micros())
            .unwrap()
            .saturating_sub(1_000_000);
        let early = advance(&doc, &mut owner, &mut journal).unwrap();
        assert_eq!(early.checkpoint, first.checkpoint);

        owner.wall_seconds = first_wake.unix_seconds();
        owner.monotonic_micros = u64::try_from(first_wake.as_micros()).unwrap();
        let second = advance(&doc, &mut owner, &mut journal).unwrap();
        let second_ci = second.checkpoint.nodes[ci_index].ci.as_ref().unwrap();
        assert_eq!(second_ci.head_sha.as_deref(), Some("a".repeat(40).as_str()));
        assert_eq!(second_ci.pending_observations, 2);
        assert_eq!(second_ci.elapsed_seconds, owner.wall_seconds as u64);
        assert_eq!(
            second_ci
                .first_observed_stamp
                .as_ref()
                .unwrap()
                .host_monotonic_micros,
            0
        );
        assert_eq!(
            second.checkpoint.wake_at,
            Some(expected_wake(owner.wall_seconds, backoff, 1))
        );
        journal.0 =
            Some(serde_json::from_slice(&serde_json::to_vec(&second.checkpoint).unwrap()).unwrap());

        let second_wake = expected_wake(owner.wall_seconds, backoff, 1);
        owner.wall_seconds = second_wake.unix_seconds();
        owner.monotonic_micros = u64::try_from(second_wake.as_micros()).unwrap();
        let third = advance(&doc, &mut owner, &mut journal).unwrap();
        let third_ci = third.checkpoint.nodes[ci_index].ci.as_ref().unwrap();
        assert_eq!(third_ci.head_sha.as_deref(), Some(third_head.as_str()));
        if changes_head {
            assert_eq!(third_ci.elapsed_seconds, 0);
            assert_eq!(third_ci.pending_observations, 1);
            assert_eq!(
                third_ci
                    .first_observed_stamp
                    .as_ref()
                    .unwrap()
                    .host_monotonic_micros,
                owner.monotonic_micros
            );
            assert_eq!(
                third.checkpoint.wake_at,
                Some(expected_wake(owner.wall_seconds, backoff, 0))
            );
        } else {
            assert_eq!(third.status, GraphStatus::Complete);
            assert!(third.checkpoint.wake_at.is_none());
            assert!(third_ci.wake_at.is_none());
        }
    }
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
    let fast_wake = expected_wake(0, owner.backoff, 0);
    assert_eq!(first.checkpoint.wake_at, Some(fast_wake));
    owner.wall_seconds = fast_wake.unix_seconds();
    let next = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(next.checkpoint.nodes[0].state, NodeState::Succeeded);
    assert_eq!(next.checkpoint.nodes[1].state, NodeState::Running);
    let slow_backoff = owner.policies["slow"];
    let slow_wake = expected_wake(0, slow_backoff, 0);
    assert_eq!(next.checkpoint.wake_at, Some(slow_wake));
    assert_eq!(
        next.checkpoint.nodes[1]
            .ci
            .as_ref()
            .unwrap()
            .pending_observations,
        1
    );
    owner.wall_seconds = slow_wake.unix_seconds();
    let done = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(done.status, GraphStatus::Complete);
    assert_eq!(
        done.checkpoint.nodes[1].ci.as_ref().unwrap().state,
        CiState::Passed
    );
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
    let ci_wake = first.checkpoint.nodes[0]
        .ci
        .as_ref()
        .unwrap()
        .wake_at
        .unwrap();
    assert_eq!(
        first.checkpoint.wake_at,
        Some(provenant_core::WallTime::from_unix_seconds(30).unwrap())
    );
    owner.wall_seconds = 30;
    let decision = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(decision.checkpoint.wake_at, Some(ci_wake));
    let ci = decision.checkpoint.nodes[0].ci.as_ref().unwrap();
    assert_eq!(ci.state, CiState::Pending);
    assert_eq!(ci.summary, "waiting");
    assert_eq!(ci.pending_observations, 1);
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
    assert_eq!(
        run.checkpoint.nodes[0]
            .ci
            .as_ref()
            .unwrap()
            .pending_observations,
        1
    );
    assert_eq!(
        run.checkpoint.nodes[0].ci.as_ref().unwrap().state,
        CiState::Pending
    );
    assert_eq!(
        run.checkpoint.wake_at,
        Some(provenant_core::WallTime::from_unix_seconds(0).unwrap())
    );
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
    assert_eq!(
        first.checkpoint.wake_at,
        Some(provenant_core::WallTime::from_unix_seconds(*timeout_seconds as i64).unwrap())
    );
    owner.wall_seconds = 120;
    owner.monotonic_micros = 120_000_000;
    let done = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(done.checkpoint.nodes[0].state, NodeState::Failed);
    assert_eq!(
        done.checkpoint.nodes[0].ci.as_ref().unwrap().state,
        CiState::Pending
    );
    assert!(!done.checkpoint.nodes[0].gate.as_ref().unwrap().passed);
    assert!(done.checkpoint.wake_at.is_none());
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
    owner.monotonic_micros = default_ci_timeout().saturating_mul(1_000_000);
    let timed_out = advance(&document(), &mut owner, &mut journal).unwrap();
    let node = &timed_out.checkpoint.nodes[0];
    assert_eq!(node.state, NodeState::Failed);
    assert!(!node.gate.as_ref().unwrap().passed);
    assert_eq!(node.ci.as_ref().unwrap().summary, "still waiting");
    assert_eq!(node.ci.as_ref().unwrap().pending_observations, 2);
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
    owner.monotonic_micros = default_ci_timeout().saturating_mul(1_000_000);
    let timed_out = advance(&document(), &mut owner, &mut journal).unwrap();
    let node = &timed_out.checkpoint.nodes[0];
    assert_eq!(node.state, NodeState::Failed);
    assert_eq!(node.ci.as_ref().unwrap().state, CiState::NotScheduled);
    assert!(node.ci.as_ref().unwrap().check_names.is_empty());
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
    assert!(evidence.clock_reanchor_reason.is_some());
    assert_eq!(
        evidence
            .first_observed_stamp
            .as_ref()
            .unwrap()
            .boot_identity,
        "boot-b"
    );
    owner.monotonic_micros += default_ci_timeout().saturating_mul(1_000_000);
    let result = advance(&document(), &mut owner, &mut journal).unwrap();
    assert!(!result.checkpoint.nodes[0].gate.as_ref().unwrap().passed);
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
    assert!(owner.checks.contains(&vec!["build".into(), "test".into()]));
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
        observations: VecDeque::from(vec![observation(CiState::Pending, "waiting"); 4]),
        boot: "boot-a".into(),
        monotonic_micros: 100,
        ..Default::default()
    };
    let mut journal = Journal::default();
    advance(&doc, &mut owner, &mut journal).unwrap();
    owner.boot = "boot-b".into();
    owner.monotonic_micros = 10_000_000;
    let reanchored = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(reanchored.status, GraphStatus::Pending);
    let evidence = reanchored.checkpoint.nodes[0].ci.as_ref().unwrap();
    assert_eq!(
        evidence
            .first_observed_stamp
            .as_ref()
            .unwrap()
            .boot_identity,
        "boot-b"
    );
    assert_eq!(evidence.elapsed_seconds, 0);

    owner.monotonic_micros += (configured_timeout_seconds - 1).saturating_mul(1_000_000);
    let before_timeout = advance(&doc, &mut owner, &mut journal).unwrap();
    assert_eq!(before_timeout.status, GraphStatus::Pending);
    owner.monotonic_micros += 1_000_000;
    let result = advance(&doc, &mut owner, &mut journal).unwrap();
    let node = &result.checkpoint.nodes[0];
    assert_eq!(node.state, NodeState::Failed);
    assert_eq!(result.outcome, Some(GraphOutcome::Failed));
    assert!(!node.gate.as_ref().unwrap().passed);
    let ci = node.ci.as_ref().unwrap();
    assert_eq!(ci.state, CiState::Pending);
    assert!(ci.elapsed_seconds >= configured_timeout_seconds);
    assert_eq!(
        ci.first_observed_stamp.as_ref().unwrap().boot_identity,
        "boot-b"
    );
}

#[test]
fn ci_check_names_are_validated() {
    for checks in [vec![""], vec![" build"], vec!["build", "build"]] {
        let mut doc = document();
        let Some(GateCondition::Ci { checks: names, .. }) = &mut doc.nodes[0].condition else {
            unreachable!()
        };
        *names = checks.into_iter().map(str::to_owned).collect();
        assert!(matches!(
            validate_plan_with_limits(&doc, &crate::config::GraphLimits::default()),
            Err(AdmissionError::Plan(_))
        ));
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
}

#[test]
fn ci_validation_rejects_bad_targets_and_timeouts() {
    for (target, timeout) in [
        ("", 60),
        (" ", 60),
        ("-bad", 60),
        ("valid", MIN_CI_TIMEOUT_SECONDS.saturating_sub(1)),
        ("valid", MAX_CI_TIMEOUT_SECONDS + 1),
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
        assert!(matches!(
            validate_plan_with_limits(&doc, &crate::config::GraphLimits::default()),
            Err(AdmissionError::Plan(_))
        ));
    }
    let defaulted = document();
    let Some(GateCondition::Ci {
        timeout_seconds, ..
    }) = &defaulted.nodes[0].condition
    else {
        unreachable!()
    };
    assert_eq!(*timeout_seconds, default_ci_timeout());
    assert!(validate_plan(&defaulted).is_ok());
}

fn node_target_document() -> GraphDocument {
    super::model::document_fixture(serde_json::json!({
        "graph_id":"node-ci", "nodes":[
            {"id":"write","access":"worktree_write","worktree":"lane","prompt":"write",
             "delivery":"pr","route":{"adapter":"fixture","model":"fixture/model"}},
            {"id":"ci","kind":"gate","access":"read_only","depends_on":["write"],
             "condition":{"type":"ci","target":{"node":"write"}}}
        ]
    }))
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
    assert!(
        owner
            .targets
            .iter()
            .any(|target| target == "refs/heads/delivered/topic")
    );
    assert_eq!(result.checkpoint.nodes[0].state, NodeState::Succeeded);
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
    assert_eq!(
        result.checkpoint.nodes[1].ci.as_ref().unwrap().state,
        CiState::Unavailable
    );
    assert!(owner.targets.is_empty());
    let mut doc = doc;
    doc.nodes[1].depends_on.clear();
    assert!(matches!(
        validate_plan_with_limits(&doc, &crate::config::GraphLimits::default()),
        Err(AdmissionError::Plan(_))
    ));
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
    owner.monotonic_micros = default_ci_timeout().saturating_mul(1_000_000);
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
    assert_eq!(done.checkpoint.nodes[0].state, NodeState::Succeeded);
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
    assert_eq!(outputs.findings[0].url.as_deref(), Some("https://checks/1"));
    assert!(owner.prompts[0].contains("assertion failed"));
    assert!(owner.prompts[0].contains("https://checks/1"));
    assert_eq!(
        advance(&doc, &mut owner, &mut journal).unwrap().checkpoint,
        run.checkpoint
    );
}

fn recovery_after_findingless_failure(recovery: serde_json::Value) -> Vec<NodeProgress> {
    let mut doc = document();
    doc.nodes[0].on_fail = Some("recovery".into());
    doc.nodes.push(serde_json::from_value(recovery).unwrap());
    let mut owner = Owner {
        observations: VecDeque::from([observation(CiState::Failed, "failed")]),
        ..Default::default()
    };
    let run = advance(&doc, &mut owner, &mut Journal::default()).unwrap();
    run.checkpoint.nodes
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
    let cases = [
        (serde_json::json!({}), NodeState::Succeeded, true),
        (
            serde_json::json!({"skip_without_findings": true}),
            NodeState::Skipped,
            false,
        ),
        (
            serde_json::json!({
                "skip_without_findings": false,
                "inputs":[{"node":"ci","outputs":["findings"]}]
            }),
            NodeState::Succeeded,
            true,
        ),
        (
            serde_json::json!({"inputs":[{"node":"ci","outputs":["findings"]}]}),
            NodeState::Skipped,
            false,
        ),
    ];
    for (shape, state, launched) in cases {
        let nodes = recovery_after_findingless_failure(node(shape));
        let recovery = nodes.iter().find(|node| node.id == "recovery").unwrap();
        assert_eq!(recovery.state, state);
        assert_eq!(!recovery.attempts.is_empty(), launched);
    }
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
        assert_eq!(child.attempts.is_empty(), skip);
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
    let max_bytes = 150;
    let max_checks = 1;
    let mut doc = document();
    let Some(GateCondition::Ci {
        failed_checks_max_bytes,
        max_failed_checks,
        ..
    }) = &mut doc.nodes[0].condition
    else {
        unreachable!()
    };
    *failed_checks_max_bytes = Some(max_bytes);
    *max_failed_checks = Some(max_checks);
    let mut failure = observation(CiState::Failed, "failed");
    failure.failed_checks = vec![failed_check(); 10];
    let mut owner = Owner {
        observations: VecDeque::from([failure]),
        ..Default::default()
    };
    let mut journal = Journal::default();
    let run = advance(&doc, &mut owner, &mut journal).unwrap();
    let ci = run.checkpoint.nodes[0].ci.as_ref().unwrap();
    assert!(ci.failed_checks.len() <= max_checks);
    assert!(canonical_len(&ci.failed_checks) <= max_bytes);
    assert!(ci.truncated);
    let policy = ci.failed_checks_policy.as_ref().unwrap();
    assert_eq!(policy.max_bytes, max_bytes);
    assert_eq!(policy.max_bytes_source, CiPolicySource::Condition);
    assert_eq!(policy.max_checks, max_checks);
    assert_eq!(policy.max_checks_source, CiPolicySource::Condition);
    assert_eq!(
        advance(&doc, &mut owner, &mut journal).unwrap().checkpoint,
        run.checkpoint
    );
}

#[test]
fn ci_default_failed_check_policy_uses_declared_sources() {
    for source in CiPolicySource::ALL {
        let expected = match source {
            CiPolicySource::Condition => "condition",
            CiPolicySource::Default => "default",
        };
        assert_eq!(source.as_str(), expected);
        assert_eq!(
            serde_json::to_value(source).unwrap(),
            serde_json::json!(expected)
        );
    }

    let mut owner = Owner {
        observations: VecDeque::from([observation(CiState::Failed, "failed")]),
        ..Default::default()
    };
    let run = advance(&document(), &mut owner, &mut Journal::default()).unwrap();
    let policy = run.checkpoint.nodes[0]
        .ci
        .as_ref()
        .unwrap()
        .failed_checks_policy
        .as_ref()
        .unwrap();
    assert_eq!(
        policy.max_bytes,
        super::ci::default_failed_checks_max_bytes()
    );
    assert_eq!(policy.max_bytes_source, CiPolicySource::Default);
    assert_eq!(policy.max_checks, super::ci::default_max_failed_checks());
    assert_eq!(policy.max_checks_source, CiPolicySource::Default);
}

#[test]
fn ci_node_targets_follow_composite_and_loop_namespaces() {
    let doc = node_target_document();
    let nested: GraphDocument = super::model::document_fixture(serde_json::json!({
        "graph_id":"nested","nodes":[{
            "id":"bundle","kind":"composite","access":"read_only",
            "composite":{"nodes":doc.nodes,"exports":[{"node":"ci","outputs":["verdict"]}]}
        }]
    }));
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
