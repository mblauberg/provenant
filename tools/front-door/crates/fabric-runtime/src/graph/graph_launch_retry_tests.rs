use super::*;

#[test]
fn launch_retry_save_failure_keeps_the_prior_checkpoint() {
    #[derive(Default)]
    struct FailingJournal {
        checkpoint: Option<GraphCheckpoint>,
        before_failure: Option<Vec<u8>>,
    }
    impl GraphJournal for FailingJournal {
        type Error = std::io::Error;
        fn load(&mut self, _: &str) -> Result<Option<GraphCheckpoint>, Self::Error> {
            Ok(self.checkpoint.clone())
        }
        fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error> {
            if checkpoint.nodes.iter().any(|node| {
                node.launch_retry
                    .as_ref()
                    .is_some_and(|retry| retry.failures > 0)
            }) {
                self.before_failure = Some(serde_json::to_vec(&self.checkpoint).unwrap());
                return Err(std::io::Error::other("injected retry save failure"));
            }
            self.checkpoint = Some(checkpoint.clone());
            Ok(())
        }
    }
    let document = graph(vec![node("one", route("first"))]);
    let mut owner = ErrorLauncher {
        now: Some(at(0)),
        errors: busy_times(1),
        ..Default::default()
    };
    let mut journal = FailingJournal::default();
    let error = advance(&document, &mut owner, &mut journal).unwrap_err();
    assert!(
        matches!(error, GraphError::Journal(ref error) if error.to_string().contains("injected retry save"))
    );
    assert_eq!(
        serde_json::to_vec(&journal.checkpoint).unwrap(),
        journal.before_failure.unwrap()
    );
    let saved = journal.checkpoint.as_ref().unwrap();
    assert_eq!(saved.nodes[0].state, NodeState::Running);
    assert_eq!(retry_of(saved).failures, 0);
    assert!(retry_of(saved).wake_at.is_none());
    assert!(saved.nodes[0].advance_error_code.is_none());
}

#[test]
fn launch_retry_policy_limits_failures_and_persists_the_source() {
    for limit in [0, 1] {
        let document = graph(vec![node("one", route("first"))]);
        let mut owner = ErrorLauncher {
            now: Some(at(0)),
            errors: busy_times(2),
            launch_policy: Some(LaunchRetryPolicy {
                limit,
                limit_source: "override".into(),
                first_seconds: 5,
                max_seconds: 2,
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut journal = MemoryJournal::default();
        let first = advance(&document, &mut owner, &mut journal).unwrap();
        if limit == 1 {
            let retry = retry_of(&first.checkpoint);
            assert_eq!(retry.policy.limit_source, "override");
            assert_eq!(
                retry.wake_at,
                Some(at(5)),
                "cap cannot undercut first delay"
            );
            // A configuration change cannot widen the in-flight retry budget.
            owner.launch_policy = Some(LaunchRetryPolicy {
                limit: 20,
                ..Default::default()
            });
            owner.now = Some(at(5));
            advance(&document, &mut owner, &mut journal).unwrap();
        }
        let node = &journal.0.as_ref().unwrap().nodes[0];
        assert_eq!(node.state, NodeState::Blocked);
        assert_eq!(
            node.advance_error_code.as_deref(),
            Some("launch_retry_exhausted")
        );
        let last = node
            .last_launch_retry
            .as_ref()
            .expect("exhausted retry evidence");
        assert_eq!(last.policy.limit, limit);
        assert_eq!(last.policy.limit_source, "override");
        assert_eq!(last.failures, limit + 1);
        assert!(last.wake_at.is_none());
        assert_eq!(owner.calls.len(), usize::from(limit) + 1);
    }
}

#[test]
fn transient_launch_error_waits_and_retries_the_same_attempt_once_due() {
    let document = graph(vec![node("one", route("first"))]);
    let mut owner = ErrorLauncher {
        now: Some(at(0)),
        errors: busy_times(1),
        observations: VecDeque::from([terminal(AttemptOutcome::Succeeded)]),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let first = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(first.status, GraphStatus::Pending);
    let progress = &first.checkpoint.nodes[0];
    assert_eq!(progress.state, NodeState::Running);
    assert_eq!(progress.attempts.len(), 1, "no duplicate attempt row");
    let attempt = &progress.attempts[0];
    assert_eq!(attempt.state, AttemptState::Starting);
    assert!(attempt.attempt_ref.is_none());
    let retry = progress.launch_retry.as_ref().expect("persisted wait");
    assert_eq!(retry.failures, 1);
    assert_eq!(retry.deadline, Some(at(3600)));
    assert_eq!(retry.wake_at, Some(at(5)));
    assert!(
        retry
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("busy"))
    );
    assert_eq!(first.checkpoint.wake_at, Some(at(5)));

    // Before the wake the node is never relaunched, on tick or manually.
    owner.now = Some(at(2));
    let waiting = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(waiting.status, GraphStatus::Pending);
    assert_eq!(owner.calls.len(), 1);

    // After the wake the same attempt key is replayed and settles once.
    owner.now = Some(at(6));
    let done = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(done.status, GraphStatus::Complete);
    assert_eq!(owner.calls.len(), 2);
    assert_eq!(owner.calls[1], owner.calls[0], "one attempt, one key");
    assert_eq!(
        owner.stored_refs,
        vec![None, None],
        "relaunched, not replayed"
    );
    let progress = &done.checkpoint.nodes[0];
    assert_eq!(progress.state, NodeState::Succeeded);
    assert!(progress.launch_retry.is_none());
    assert_eq!(done.checkpoint.wake_at, None);
}

#[test]
fn launch_retry_backoff_starts_after_a_slow_failed_submission() {
    let document = graph(vec![node("one", route("first"))]);
    let mut owner = ErrorLauncher {
        now: Some(at(0)),
        now_after_error: Some(at(30)),
        errors: [(1, crate::coordination::facade::FacadeError::Busy)].into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(retry_of(journal.0.as_ref().unwrap()).wake_at, Some(at(35)));
}

#[test]
fn node_review_base_is_pinned_before_launch_and_reused_on_replay() {
    let mut reviewer = node("review", route("first"));
    reviewer
        .extra
        .insert("review_base".into(), serde_json::json!("moving-ref"));
    let mut document = graph(vec![reviewer]);
    document.schema_version = 2;
    let pinned = "1".repeat(40);
    let mut owner = FakeOwner {
        resolved_review_base: Some(pinned.clone()),
        observations: VecDeque::from([
            AttemptObservation::Pending {
                attempt_ref: Some("host/review".into()),
            },
            terminal(AttemptOutcome::Succeeded),
        ]),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let pending = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(
        pending.checkpoint.nodes[0].review_base.as_deref(),
        Some(pinned.as_str())
    );
    owner.resolved_review_base = Some("2".repeat(40));
    advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(owner.review_base_resolutions, 1);
    assert_eq!(
        owner.observed_review_bases,
        vec![Some(pinned.clone()), Some(pinned)]
    );
    let mut broken = journal.0.clone().unwrap();
    broken.nodes[0].state = NodeState::Running;
    broken.nodes[0].review_base = None;
    journal.0 = Some(broken);
    assert!(matches!(
        advance(&document, &mut owner, &mut journal),
        Err(GraphError::BadCheckpoint { .. })
    ));
    assert_eq!(owner.review_base_resolutions, 1);
}

#[test]
fn review_base_save_failure_prevents_attempt_creation() {
    #[derive(Default)]
    struct FailingJournal(Option<GraphCheckpoint>);
    impl GraphJournal for FailingJournal {
        type Error = std::io::Error;
        fn load(&mut self, _: &str) -> Result<Option<GraphCheckpoint>, Self::Error> {
            Ok(self.0.clone())
        }
        fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error> {
            if checkpoint.nodes[0].review_base.is_some() {
                return Err(std::io::Error::other("injected review-base save failure"));
            }
            self.0 = Some(checkpoint.clone());
            Ok(())
        }
    }
    let mut reviewer = node("review", route("first"));
    reviewer
        .extra
        .insert("review_base".into(), serde_json::json!("moving-ref"));
    let mut document = graph(vec![reviewer]);
    document.schema_version = 2;
    let mut owner = FakeOwner {
        resolved_review_base: Some("1".repeat(40)),
        ..Default::default()
    };
    let mut journal = FailingJournal::default();
    assert!(matches!(
        advance(&document, &mut owner, &mut journal),
        Err(GraphError::Journal(_))
    ));
    assert!(owner.calls.is_empty());
    assert!(
        journal.0.is_none(),
        "failed admission must not save a partial graph"
    );
}

#[test]
fn writer_lock_contention_retries_the_same_attempt_after_release() {
    let document = graph(vec![node("one", route("first"))]);
    let mut owner = ErrorLauncher {
        now: Some(at(0)),
        errors: [(1, crate::coordination::facade::FacadeError::WriterLockBusy)].into(),
        observations: VecDeque::from([terminal(AttemptOutcome::Succeeded)]),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let pending = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(pending.checkpoint.nodes[0].state, NodeState::Running);
    assert_eq!(retry_of(&pending.checkpoint).wake_at, Some(at(5)));
    owner.now = Some(at(5));
    let completed = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(completed.checkpoint.nodes[0].state, NodeState::Succeeded);
    assert_eq!(completed.checkpoint.nodes[0].attempts.len(), 1);
    assert_eq!(owner.calls.len(), 2);
    assert_eq!(owner.calls[0], owner.calls[1]);
}

#[test]
fn an_unobserved_writer_disk_check_is_a_retryable_launch_error() {
    let document = graph(vec![node("one", route("first"))]);
    let mut owner = ErrorLauncher {
        now: Some(at(0)),
        errors: [(
            1,
            crate::coordination::facade::FacadeError::DispatchUnobserved {
                reason: "disk observation failed".into(),
            },
        )]
        .into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    advance(&document, &mut owner, &mut journal).unwrap();
    let retry = retry_of(journal.0.as_ref().unwrap());
    assert_eq!(retry.failures, 1);
    assert!(
        retry
            .reason
            .as_deref()
            .unwrap()
            .contains("disk observation failed")
    );
    assert_eq!(retry.wake_at, Some(at(5)));
}

#[test]
fn host_delivery_limit_reaches_graph_status_without_exposing_io_sources() {
    let fact = provenant_records::diagnostic::DeliveryLimit::new(
        provenant_records::run::WriterDeliveryStage::Prepare,
        "git",
        "worktree list",
        8388608,
    )
    .unwrap();
    let reason = fact.detail();
    let error = || {
        // Exercise the envelope consumed by a peer launch owner, including
        // source text that is not an approved diagnostic fact.
        let mut wire = serde_json::to_value(crate::coordination::facade::FacadeError::Io {
            operation: "host delivery".into(),
            path: None,
            delivery_limit: Some(fact.clone()),
            delivery_stage: None,
            resource: None,
            input_failure: None,
        })
        .unwrap();
        assert_eq!(wire["code"], "io");
        wire["operation"] = "TOKEN_SENTINEL\u{1b}".into();
        wire["path"] = "/private/host".into();
        serde_json::from_value::<crate::coordination::facade::FacadeError>(wire).unwrap()
    };
    let document = graph(vec![node("one", route("first"))]);
    let mut owner = ErrorLauncher {
        now: Some(at(0)),
        errors: [(1, error())].into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    advance(&document, &mut owner, &mut journal).unwrap();
    let retry = retry_of(journal.0.as_ref().unwrap());
    assert_eq!(retry.failures, 1);
    assert!(retry.reason.as_deref().unwrap().contains(&reason));

    let mut owner = ErrorLauncher {
        now: Some(at(0)),
        errors: (1..=4).map(|call| (call, error())).collect(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    advance(&document, &mut owner, &mut journal).unwrap();
    for now in [6, 17, 38] {
        owner.now = Some(at(now));
        advance(&document, &mut owner, &mut journal).unwrap();
    }
    owner.now = Some(at(3600));
    advance(&document, &mut owner, &mut journal).unwrap();
    let progress = &journal.0.as_ref().unwrap().nodes[0];
    assert_eq!(progress.state, NodeState::Blocked);
    let text = progress.advance_error.as_deref().unwrap();
    assert!(text.contains(&reason), "{text}");
    assert!(text.contains("[io]"));
    assert!(text.contains("git worktree list"));
    assert!(text.contains("8388608"));
    assert!(text.contains("inspect host delivery"));
    assert!(!text.contains("host capacity"));
    assert!(!text.contains("TOKEN_SENTINEL"));
    assert!(!text.contains("/private/host"));
    let stored = serde_json::to_string(journal.0.as_ref().unwrap()).unwrap();
    let restored: GraphCheckpoint = serde_json::from_str(&stored).unwrap();
    assert_eq!(restored.nodes[0].advance_error.as_deref(), Some(text));
    assert!(!stored.contains("TOKEN_SENTINEL"));
    assert!(!stored.contains("/private/host"));

    // Generic I/O retains its established code without inheriting the safe
    // command exception merely because its untrusted source mentions a limit.
    let generic = crate::coordination::facade::FacadeError::Io {
        operation: format!("TOKEN_SENTINEL\u{1b} {reason}"),
        path: Some("/private/host".into()),
        delivery_stage: None,
        resource: None,
        input_failure: None,
        delivery_limit: None,
    };
    let mut owner = ErrorLauncher {
        now: Some(at(0)),
        errors: [(1, generic)].into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    advance(&document, &mut owner, &mut journal).unwrap();
    let reason = retry_of(journal.0.as_ref().unwrap())
        .reason
        .as_deref()
        .unwrap();
    assert!(reason.contains("[io]"));
    for unsafe_detail in [
        "TOKEN_SENTINEL",
        "/private/host",
        "git worktree list",
        "8388608",
    ] {
        assert!(!reason.contains(unsafe_detail), "{reason}");
    }
}

#[test]
fn launch_errors_retry_until_the_persisted_deadline() {
    let document = graph(vec![node("one", route("first"))]);
    let mut owner = ErrorLauncher {
        now: Some(at(0)),
        errors: busy_times(4),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    advance(&document, &mut owner, &mut journal).unwrap();
    let retry = retry_of(journal.0.as_ref().unwrap());
    assert_eq!(retry.failures, 1);
    assert_eq!(retry.wake_at, Some(at(5)));
    for (now, failures, wake) in [(6, 2, Some(at(16))), (17, 3, Some(at(37)))] {
        owner.now = Some(at(now));
        advance(&document, &mut owner, &mut journal).unwrap();
        let retry = retry_of(journal.0.as_ref().unwrap());
        assert_eq!(retry.failures, failures);
        assert_eq!(retry.wake_at, wake);
    }
    // The fourth failure remains retryable; the persisted window does not
    // reset when the journal is reloaded on the next advance.
    owner.now = Some(at(38));
    advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(
        retry_of(journal.0.as_ref().unwrap()).deadline,
        Some(at(3600))
    );
    assert_eq!(retry_of(journal.0.as_ref().unwrap()).failures, 4);
    owner.now = Some(at(3600));
    advance(&document, &mut owner, &mut journal).unwrap();
    let checkpoint = journal.0.as_ref().unwrap();
    let progress = &checkpoint.nodes[0];
    assert_eq!(progress.state, NodeState::Blocked);
    assert!(progress.launch_retry.is_none());
    assert_eq!(progress.attempts.len(), 1);
    assert_eq!(progress.attempts[0].state, AttemptState::Starting);
    assert_eq!(checkpoint.wake_at, None);
    assert_eq!(
        progress.advance_error_code.as_deref(),
        Some("launch_retry_exhausted")
    );
    let reason = progress.advance_error.as_deref().expect("blocked reason");
    assert!(reason.contains("service is busy"));
    assert!(!reason.contains("new graph_id"));
    assert_eq!(owner.calls.len(), 4);

    // A blocked node never launches again.
    owner.now = Some(at(3601));
    let stopped = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(stopped.checkpoint.nodes[0].state, NodeState::Blocked);
    assert_eq!(owner.calls.len(), 4);
}

#[test]
fn expired_launch_replays_an_accepted_operation() {
    let document = graph(vec![node("one", route("first"))]);
    let mut owner = ErrorLauncher {
        now: Some(at(0)),
        errors: busy_times(1),
        observations: VecDeque::from([terminal(AttemptOutcome::Succeeded)]),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    advance(&document, &mut owner, &mut journal).unwrap();
    owner.accepted = true;
    owner.now = Some(at(3600));
    let result = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(result.checkpoint.nodes[0].state, NodeState::Succeeded);
    assert_eq!(owner.calls.len(), 2);
    assert_eq!(owner.calls[0], owner.calls[1]);
}

#[test]
fn launch_succeeds_after_more_than_three_transient_failures() {
    let document = graph(vec![node("one", route("first"))]);
    let mut owner = ErrorLauncher {
        now: Some(at(0)),
        errors: busy_times(4),
        observations: VecDeque::from([terminal(AttemptOutcome::Succeeded)]),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    for now in [0, 6, 17, 38] {
        owner.now = Some(at(now));
        let waiting = advance(&document, &mut owner, &mut journal).unwrap();
        assert_eq!(waiting.checkpoint.nodes[0].state, NodeState::Running);
        assert_eq!(waiting.checkpoint.nodes[0].attempts.len(), 1);
        assert_eq!(retry_of(&waiting.checkpoint).deadline, Some(at(3600)));
    }
    owner.now = Some(at(79));
    let done = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(done.checkpoint.nodes[0].state, NodeState::Succeeded);
    assert_eq!(done.checkpoint.nodes[0].attempts.len(), 1);
    assert!(owner.calls.iter().all(|key| key == &owner.calls[0]));
    assert_eq!(owner.calls.len(), 5);
}

#[test]
fn launch_wake_is_capped_by_the_original_deadline() {
    let document = graph(vec![node("one", route("first"))]);
    let mut owner = ErrorLauncher {
        now: Some(at(0)),
        admission_seconds: Some(3),
        errors: busy_times(1),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    advance(&document, &mut owner, &mut journal).unwrap();
    let retry = retry_of(journal.0.as_ref().unwrap());
    assert_eq!(retry.deadline, Some(at(3)));
    assert_eq!(retry.wake_at, Some(at(3)));
    // A new policy on restart cannot extend the saved window.
    owner.admission_seconds = Some(3600);
    owner.now = Some(at(3));
    let ended = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(ended.checkpoint.nodes[0].state, NodeState::Blocked);
    assert_eq!(owner.calls.len(), 1);
}

#[test]
fn nonterminal_observation_transitions_need_no_owner_or_journal() {
    let mut attempt = GraphAttempt {
        extra: Default::default(),
        review_substitution: false,
        key: AttemptKey {
            extra: Default::default(),
            graph_id: "g".into(),
            node_id: "n".into(),
            route_index: 0,
        },
        route: route("first"),
        state: AttemptState::Starting,
        attempt_ref: None,
        outcome: None,
        launched: true,
        launch_actor: "actor".into(),
        settled: 0,
        input_cap: None,
        input_cap_source: None,
        input_manifest: None,
        usage: None,
    };
    let mut state = NodeState::Pending;
    apply_unknown_observation(&mut attempt, &mut state, None);
    assert_eq!(attempt.state, AttemptState::Unresolved);
    assert_eq!(state, NodeState::Running);
    apply_pending_observation(&mut attempt, &mut state, Some("host/1".into()));
    assert_eq!(attempt.state, AttemptState::Running);
    assert_eq!(attempt.attempt_ref.as_deref(), Some("host/1"));
    assert_eq!(state, NodeState::Running);
    assert_eq!(attempt.key.route_index, 0);
}

#[test]
fn launch_retry_without_a_wall_clock_keeps_the_prior_checkpoint() {
    let document = graph(vec![node("one", route("first"))]);
    let mut owner = ErrorLauncher {
        errors: busy_times(1),
        observations: VecDeque::from([terminal(AttemptOutcome::Succeeded)]),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let error = advance(&document, &mut owner, &mut journal).unwrap_err();
    assert!(matches!(error, GraphError::Refused(ref reason) if reason.contains("wall clock")));
    let prior = journal.0.as_ref().unwrap().clone();
    assert_eq!(prior.nodes[0].state, NodeState::Running);
    assert_eq!(retry_of(&prior).failures, 0);
    assert!(retry_of(&prior).wake_at.is_none());
    owner.now = Some(at(1));
    let result = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(result.checkpoint.nodes[0].state, NodeState::Succeeded);
    assert_eq!(owner.calls.len(), 2);
    assert_eq!(owner.calls[0], owner.calls[1]);
}

#[test]
fn a_permanent_refusal_blocks_the_node_at_once_with_reason_and_remedy() {
    let document = graph(vec![node("one", route("first"))]);
    let mut owner = ErrorLauncher {
        now: Some(at(0)),
        errors: [(
            1,
            crate::coordination::facade::FacadeError::InvalidInput { detail: None },
        )]
        .into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    // Like failed nodes, a blocked node settles the graph; the refusal
    // itself stays on the node for status.
    assert_eq!(run.status, GraphStatus::Complete);
    let progress = &run.checkpoint.nodes[0];
    assert_eq!(progress.state, NodeState::Blocked);
    assert!(progress.launch_retry.is_none());
    assert_eq!(progress.attempts.len(), 1);
    assert_eq!(progress.attempts[0].state, AttemptState::Starting);
    assert_eq!(
        progress.advance_error_code.as_deref(),
        Some("launch_refused")
    );
    let reason = progress.advance_error.as_deref().expect("blocked reason");
    assert!(reason.contains("invalid input"));
    assert!(reason.contains("provenant retry g-test --node one"));
    assert_eq!(run.checkpoint.wake_at, None);

    // The refusal is durable: the next advance never relaunches the node.
    let again = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(again.checkpoint.nodes[0].state, NodeState::Blocked);
    assert_eq!(owner.calls.len(), 1);
}

#[test]
fn a_saved_attempt_reference_never_gains_a_launch_retry_or_a_second_row() {
    let document = graph(vec![node("one", route("first"))]);
    let mut owner = ErrorLauncher {
        now: Some(at(0)),
        errors: [(
            2,
            crate::coordination::facade::FacadeError::ConfinementRefused { resource: None },
        )]
        .into(),
        observations: VecDeque::from([
            AttemptObservation::Pending {
                attempt_ref: Some("host/1".into()),
            },
            terminal(AttemptOutcome::Succeeded),
        ]),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let pending = advance(&document, &mut owner, &mut journal).unwrap();
    let progress = &pending.checkpoint.nodes[0];
    assert_eq!(progress.state, NodeState::Running);
    assert!(progress.launch_retry.is_none());
    assert_eq!(progress.attempts.len(), 1);
    assert_eq!(progress.attempts[0].attempt_ref.as_deref(), Some("host/1"));

    // After the reference was saved a launch error cannot manage the
    // attempt's disposition: the attempt is owned by its reference, so the
    // error propagates and the node is neither retried nor blocked.
    let error = advance(&document, &mut owner, &mut journal)
        .expect_err("error propagates without a disposition");
    assert!(matches!(error, GraphError::Run(_)));
    let progress = &journal.0.as_ref().unwrap().nodes[0];
    assert!(progress.launch_retry.is_none());
    assert_eq!(progress.state, NodeState::Running);
    assert_eq!(progress.attempts[0].attempt_ref.as_deref(), Some("host/1"));

    // The next advance observes the stored reference and never resubmits.
    let done = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(done.status, GraphStatus::Complete);
    for call in &owner.calls {
        assert_eq!(call, &owner.calls[0], "one attempt, one key");
    }
    assert_eq!(
        owner.stored_refs,
        vec![None, Some("host/1".into()), Some("host/1".into())]
    );
}

#[test]
fn launch_retry_kinds_are_decided_from_the_typed_error_only() {
    use crate::coordination::facade::FacadeError;
    let operation = provenant_core::OperationId::parse("op").unwrap();
    for transient in [
        FacadeError::Busy,
        FacadeError::Timeout,
        FacadeError::HomeUnavailable,
        FacadeError::WriterUnknown,
        FacadeError::WriterLockBusy,
        FacadeError::Io {
            delivery_limit: None,
            delivery_stage: None,
            resource: None,
            input_failure: None,
            operation: "read file".into(),
            path: None,
        },
        FacadeError::LaunchUnknown {
            operation_id: operation,
        },
    ] {
        assert!(launch_retry_kind(&transient), "{transient}");
    }
    for permanent in [
        FacadeError::InvalidInput { detail: None },
        FacadeError::ConfinementRefused { resource: None },
        FacadeError::WriterBusy { holder: None },
        FacadeError::DispatchRefused {
            fact: None,
            disk: None,
            reason: "why".into(),
        },
        FacadeError::NotFound,
        FacadeError::Cancelled,
        FacadeError::OwnerUnavailable,
        FacadeError::StateCorrupt,
    ] {
        assert!(!launch_retry_kind(&permanent), "{permanent}");
    }
    #[derive(Debug)]
    struct Untyped;
    impl std::fmt::Display for Untyped {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("untyped")
        }
    }
    impl Error for Untyped {}
    assert!(!launch_retry_kind(&Untyped));
}
