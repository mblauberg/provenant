use super::*;

#[test]
fn usage_fetch_failure_is_unknown_and_does_not_wedge_graph() {
    let document = graph(vec![
        node("one", route("first")),
        node("two", route("first")),
    ]);
    let observation = || AttemptObservation::Terminal {
        attempt_ref: Some("host/attempt".into()),
        outcome: AttemptOutcome::Succeeded,
        launched: true,
        result: None,
        result_warning: None,
        result_path: None,
        branch: None,
        commit: None,
        stop: None,
    };
    for policy in [
        crate::config::UnknownUsage::Warn,
        crate::config::UnknownUsage::Pause,
    ] {
        let mut owner = FakeOwner {
            observations: VecDeque::from([observation(), observation()]),
            usage_fails: true,
            ..Default::default()
        };
        let budget = crate::config::GraphBudget {
            per_graph: crate::config::BudgetLimit {
                tokens: Some(100),
                ..Default::default()
            },
            unknown_usage: policy,
            ..Default::default()
        };
        let mut journal = MemoryJournal::default();
        let run = advance_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &Default::default(),
            &budget,
        )
        .unwrap();
        assert_eq!(
            run.status,
            if policy == crate::config::UnknownUsage::Warn {
                GraphStatus::Complete
            } else {
                GraphStatus::Paused
            }
        );
        assert_eq!(run.checkpoint.nodes[0].state, NodeState::Succeeded);
        assert!(
            run.checkpoint
                .usage_events
                .iter()
                .any(|event| event.metric == "telemetry"
                    && event.reason.as_deref() == Some("no fake observation remains"))
        );
    }
}

#[test]
fn cost_unknown_reason_is_recorded() {
    let document = graph(vec![node("one", route("first"))]);
    let mut owner = FakeOwner {
        observations: VecDeque::from([AttemptObservation::Terminal {
            attempt_ref: Some("host/attempt".into()),
            outcome: AttemptOutcome::Succeeded,
            launched: true,
            result: None,
            result_warning: None,
            result_path: None,
            branch: None,
            commit: None,
            stop: None,
        }]),
        usage: AttemptUsage {
            cost_unknown_reason: Some("config/telemetry-prices.toml: missing".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    let run = advance_with_budget(
        &document,
        &mut owner,
        &mut MemoryJournal::default(),
        &Default::default(),
        &Default::default(),
    )
    .unwrap();
    assert!(
        run.checkpoint
            .usage_events
            .iter()
            .any(|event| event.metric == "cost_usd"
                && event.reason.as_deref() == Some("config/telemetry-prices.toml: missing"))
    );
}

#[test]
fn document_limits_are_ignored_with_a_warning() {
    let mut document = graph(vec![
        node("one", route("first")),
        node("two", route("first")),
    ]);
    document.limits = Some(serde_json::json!({"nodes": 999, "future": 0}));
    let limits = crate::config::GraphLimits {
        nodes: 1,
        ..Default::default()
    };
    let run = advance_with_budget(
        &document,
        &mut FakeOwner::default(),
        &mut MemoryJournal::default(),
        &limits,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert!(run.checkpoint.limit_notes[0].contains("ignored"));
}

#[test]
fn settled_graph_never_saves_a_new_structural_pause() {
    struct RecordingJournal {
        checkpoint: GraphCheckpoint,
        saves: Vec<GraphCheckpoint>,
    }
    impl GraphJournal for RecordingJournal {
        type Error = Infallible;
        fn load(&mut self, _: &str) -> Result<Option<GraphCheckpoint>, Self::Error> {
            Ok(Some(self.checkpoint.clone()))
        }
        fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error> {
            self.saves.push(checkpoint.clone());
            Ok(())
        }
    }
    let document = graph(vec![
        node("one", route("first")),
        node("two", route("first")),
    ]);
    let mut owner = FakeOwner {
        observations: VecDeque::from([
            terminal(AttemptOutcome::Succeeded),
            terminal(AttemptOutcome::Succeeded),
        ]),
        ..Default::default()
    };
    let completed = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    let mut journal = RecordingJournal {
        checkpoint: completed.checkpoint,
        saves: Vec::new(),
    };
    let limits = crate::config::GraphLimits {
        nodes: 1,
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
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(
        journal
            .saves
            .iter()
            .all(|checkpoint| checkpoint.pause.is_none())
    );
}

#[test]
fn unset_extensions_are_refused() {
    for extension in [
        "per_graph.tokens=20",
        "per_node.cost_usd=5",
        "per_graph.wall_time_seconds=10",
    ] {
        let err = apply_extension(&mut Default::default(), extension, false).unwrap_err();
        assert_eq!(err.code(), "budget_extension_unconfigured");
    }
}

#[test]
fn structural_limit_pauses_and_host_limit_change_resumes() {
    let document = graph(vec![
        node("one", route("first")),
        node("two", route("first")),
    ]);
    let limits = crate::config::GraphLimits {
        nodes: 1,
        ..Default::default()
    };
    let mut owner = FakeOwner {
        observations: VecDeque::from([
            terminal(AttemptOutcome::Succeeded),
            terminal(AttemptOutcome::Succeeded),
        ]),
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
    assert_eq!(
        paused.checkpoint.pause_cause(),
        Some(PauseCause::StructuralLimit)
    );
    let reason = limits.nodes_bound().refusal(2).unwrap();
    assert_eq!(document.admit(&limits).unwrap_err().to_string(), reason);
    assert_eq!(
        paused.checkpoint.pause_reason(),
        Some(reason.as_str())
    );
    assert!(owner.calls.is_empty());
    let raised = crate::config::GraphLimits { nodes: 2, ..limits };
    let resumed = resume_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &raised,
        &Default::default(),
        &[],
    )
    .unwrap();
    assert_eq!(resumed.status, GraphStatus::Complete);
}

#[test]
fn fan_out_and_depth_pause_before_launch() {
    let root = node("root", route("first"));
    let mut a = node("a", route("first"));
    a.depends_on.push("root".into());
    let mut b = node("b", route("first"));
    b.depends_on.push("root".into());
    let mut c = node("c", route("first"));
    c.depends_on.push("a".into());
    let document = graph(vec![root, a, b, c]);
    for limits in [
        crate::config::GraphLimits {
            fan_out_width: 1,
            ..Default::default()
        },
        crate::config::GraphLimits {
            expansion_depth: 1,
            ..Default::default()
        },
    ] {
        let mut owner = FakeOwner::default();
        let run = advance_with_budget(
            &document,
            &mut owner,
            &mut MemoryJournal::default(),
            &limits,
            &Default::default(),
        )
        .unwrap();
        assert_eq!(run.status, GraphStatus::Paused);
        assert_eq!(
            run.checkpoint.pause_cause(),
            Some(PauseCause::StructuralLimit)
        );
        if limits.expansion_depth == 1 {
            let reason = limits.expansion_depth_bound().refusal(2).unwrap();
            assert_eq!(document.admit(&limits).unwrap_err().to_string(), reason);
            assert_eq!(
                run.checkpoint.pause_reason(),
                Some(reason.as_str())
            );
        }
        assert!(owner.calls.is_empty());
    }
}

#[test]
fn final_budget_exhaustion_still_delivers() {
    let mut document = graph(vec![node("one", route("first"))]);
    document.delivery = Some(provenant_records::run::DeliveryMode::Pr);
    document.integration_branch = Some("integration".into());
    let mut owner = FakeOwner {
        observations: VecDeque::from([AttemptObservation::Terminal {
            attempt_ref: Some("host/attempt".into()),
            outcome: AttemptOutcome::Succeeded,
            launched: true,
            result: None,
            result_warning: None,
            result_path: None,
            branch: None,
            commit: None,
            stop: None,
        }]),
        usage: AttemptUsage {
            tokens: Some(10),
            ..Default::default()
        },
        ..Default::default()
    };
    let budget = crate::config::GraphBudget {
        per_graph: crate::config::BudgetLimit {
            tokens: Some(10),
            ..Default::default()
        },
        ..Default::default()
    };
    let run = advance_with_budget(
        &document,
        &mut owner,
        &mut MemoryJournal::default(),
        &Default::default(),
        &budget,
    )
    .unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(run.checkpoint.delivered);
    assert_eq!(owner.deliveries, 1);
}

#[test]
fn budget_pause_stops_admission_and_extension_resumes() {
    let document = graph(vec![
        node("one", route("first")),
        node("two", route("first")),
    ]);
    let budget = crate::config::GraphBudget {
        per_graph: crate::config::BudgetLimit {
            tokens: Some(10),
            ..Default::default()
        },
        ..Default::default()
    };
    let observation = || AttemptObservation::Terminal {
        attempt_ref: Some("host/attempt".into()),
        outcome: AttemptOutcome::Succeeded,
        launched: true,
        result: None,
        result_warning: None,
        result_path: None,
        branch: None,
        commit: None,
        stop: None,
    };
    let mut owner = FakeOwner {
        observations: VecDeque::from([observation(), observation()]),
        usage: AttemptUsage {
            tokens: Some(10),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let paused = advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &Default::default(),
        &budget,
    )
    .unwrap();
    assert_eq!(paused.status, GraphStatus::Paused);
    assert_eq!(owner.calls.len(), 1);
    let resumed = resume_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &Default::default(),
        &budget,
        &["per_graph.tokens=30".into()],
    )
    .unwrap();
    assert_eq!(resumed.status, GraphStatus::Complete);
    assert_eq!(owner.calls.len(), 2);
}

#[test]
fn unknown_usage_warns_or_pauses_and_events_are_idempotent() {
    let document = graph(vec![
        node("one", route("first")),
        node("two", route("first")),
    ]);
    let observation = || AttemptObservation::Terminal {
        attempt_ref: Some("host/attempt".into()),
        outcome: AttemptOutcome::Succeeded,
        launched: true,
        result: None,
        result_warning: None,
        result_path: None,
        branch: None,
        commit: None,
        stop: None,
    };
    for policy in [
        crate::config::UnknownUsage::Warn,
        crate::config::UnknownUsage::Pause,
    ] {
        let budget = crate::config::GraphBudget {
            per_graph: crate::config::BudgetLimit {
                tokens: Some(100),
                ..Default::default()
            },
            unknown_usage: policy,
            ..Default::default()
        };
        let mut owner = FakeOwner {
            observations: VecDeque::from([observation(), observation()]),
            usage: AttemptUsage {
                tokens: Some(5),
                tokens_partial: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut journal = MemoryJournal::default();
        let run = advance_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &Default::default(),
            &budget,
        )
        .unwrap();
        assert_eq!(
            run.status,
            if policy == crate::config::UnknownUsage::Pause {
                GraphStatus::Paused
            } else {
                GraphStatus::Complete
            }
        );
        assert_eq!(
            owner.calls.len(),
            if policy == crate::config::UnknownUsage::Pause {
                1
            } else {
                2
            }
        );
        assert!(
            run.checkpoint
                .unknown_usage
                .iter()
                .any(|warning| warning.contains("tokens"))
        );
        let mut checkpoint = run.checkpoint;
        let events = checkpoint.usage_events.clone();
        update_usage_warnings(&mut checkpoint);
        assert_eq!(checkpoint.usage_events, events);
    }
}

#[test]
fn graph_wall_budget_sums_provider_time_and_preserves_partial_lower_bound() {
    let document = graph(vec![
        node("one", route("first")),
        node("two", route("first")),
    ]);
    let mut owner = FakeOwner {
        observations: VecDeque::from([AttemptObservation::Terminal {
            attempt_ref: Some("host/attempt".into()),
            outcome: AttemptOutcome::Succeeded,
            launched: true,
            result: None,
            result_warning: None,
            result_path: None,
            branch: None,
            commit: None,
            stop: None,
        }]),
        usage: AttemptUsage {
            tokens: Some(12),
            tokens_partial: true,
            wall_time_seconds: Some(10),
            ..Default::default()
        },
        ..Default::default()
    };
    let budget = crate::config::GraphBudget {
        per_graph: crate::config::BudgetLimit {
            tokens: Some(12),
            wall_time_seconds: Some(100),
            ..Default::default()
        },
        ..Default::default()
    };
    let run = advance_with_budget(
        &document,
        &mut owner,
        &mut MemoryJournal::default(),
        &Default::default(),
        &budget,
    )
    .unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert_eq!(
        run.checkpoint.pause_cause(),
        Some(PauseCause::BudgetExceeded)
    );
    let mut checkpoint = run.checkpoint;
    let wall_budget = crate::config::GraphBudget {
        per_graph: crate::config::BudgetLimit {
            wall_time_seconds: Some(11),
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(budget_reason(&checkpoint, &wall_budget).is_none());
    checkpoint.nodes[0].attempts[0]
        .usage
        .as_mut()
        .unwrap()
        .wall_time_seconds = Some(11);
    assert!(
        budget_reason(&checkpoint, &wall_budget)
            .unwrap()
            .contains("wall_time_seconds")
    );
}

#[test]
fn node_titles_are_validated_and_sent_to_attempts_and_status() {
    let mut named = node("named", route("first"));
    named.title = Some("Review changes".into());
    let defaulted = node("defaulted", route("first"));
    let document = graph(vec![named, defaulted]);
    let mut owner = FakeOwner {
        observations: VecDeque::from([
            terminal(AttemptOutcome::Succeeded),
            terminal(AttemptOutcome::Succeeded),
        ]),
        ..FakeOwner::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(owner.titles, ["Review changes", "defaulted"]);
    let status = serde_json::to_value(&run).unwrap();
    assert_eq!(status["checkpoint"]["nodes"][0]["title"], "Review changes");
    assert_eq!(status["checkpoint"]["nodes"][1]["title"], "defaulted");

    let mut invalid = node("invalid", route("first"));
    invalid.title = Some("bad\ntitle".into());
    assert_eq!(
        validate_plan(&graph(vec![invalid])).err(),
        Some("node invalid: task title must be 1 to 256 bytes and contain no control characters")
    );
}
