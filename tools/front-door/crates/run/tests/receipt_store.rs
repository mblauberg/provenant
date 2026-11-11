use provenant_core::{
    ActorId, AttemptId, CancelToken, Digest, MonoTime, OperationId, ProjectId, RootInputs, RunId,
    TaskId, TestClock, WallTime,
};
use provenant_records::CanonicalV1;
use provenant_run::*;

type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

fn context(base: &std::path::Path) -> TestResultContext {
    std::fs::create_dir_all(base.join("home"))?;
    std::fs::create_dir_all(base.join("checkout"))?;
    let mut roots = RootInputs::new(base.join("home"), base.join("checkout"));
    roots.state_dir = Some(base.join("state"));
    Ok(ExecutionContext {
        state_root: roots.resolve(&provenant_os::fs::SystemPathResolver)?.state,
        project_id: ProjectId::from_bytes([1; 16]),
        actor_id: ActorId::from_bytes([2; 16]),
        read_prompts: true,
    })
}
type TestResultContext = std::result::Result<ExecutionContext, Box<dyn std::error::Error>>;

fn input() -> std::result::Result<PreparedRun, Box<dyn std::error::Error>> {
    let task = TaskId::from_bytes([3; 16]);
    Ok(PreparedRun {
        run_id: RunId::from_bytes([4; 16]),
        operation_id: OperationId::parse("receipt")?,
        created_at: WallTime::from_micros(100)?,
        request: CanonicalRequest {
            extra: Default::default(),
            child_attempt_id_version: None,
            chair: None,
            fresh_continuation: None,
            schema_version: 1,
            tasks: vec![TaskRequest {
                context: None,
                extra: Default::default(),
                build_heavy: false,
                title: None,
                base: None,
                head: None,
                source_head: None,
                base_ref: None,
                writer: None,
                profile: HarnessProfile::default(),
                task_id: task,
                prompt_digest: CanonicalV1::file_digest(b"prompt"),
                route: RequestedRoute::Model {
                    extra: Default::default(),
                    provider: "fixture".into(),
                    model: "fixture/model".into(),
                    effort: None,
                },
                account: None,
                authority_digest: Digest::from_bytes([5; 32]),
                provider_timeout_seconds: 3600,
                delivery_id: None,
                deliver: None,
                access: Default::default(),
            }],
            concurrency: 1,
            parent: None,
            lane_depth: Some(1),
            parent_attempt: None,
        },
        attempts: vec![NewAttempt {
            task_id: task,
            attempt_id: AttemptId::from_bytes([6; 16]),
            prompt: "prompt".into(),
        }],
    })
}

#[test]
fn receipt_replay_and_independent_effects() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = TestClock::new(WallTime::from_micros(100)?, MonoTime::from_micros(0));
    let cancel = CancelToken::new();
    let call = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let store = RunStore::open(&context, &call)?;
    let input = input()?;
    store.create(&input, &call)?;
    let id = input.attempts[0].attempt_id;
    store.withdraw(
        id,
        AttemptState::Queued {
            extra: Default::default(),
        },
        Outcome::Rejected,
        "refused",
        WallTime::from_micros(200)?,
        &call,
    )?;
    let receipt = store.record_effect(
        id,
        0,
        SettlementEffect::GitImport,
        EffectDisposition::Failed {
            reason: "busy".into(),
            retryable: true,
        },
        &call,
    )?;
    assert!(matches!(
        receipt.git_import,
        EffectDisposition::Failed {
            retryable: true,
            ..
        }
    ));
    let receipt = store.record_effect(
        id,
        0,
        SettlementEffect::CleanupCustodyRelease,
        EffectDisposition::Failed {
            reason: "cleanup failed".into(),
            retryable: true,
        },
        &call,
    )?;
    assert!(matches!(
        receipt.cleanup_custody_release,
        EffectDisposition::Failed {
            retryable: false,
            ..
        }
    ));
    let record = store.record(id, &call)?;
    assert_eq!(record.settlement_receipts, Some(receipt));
    let replay = store.replay_terminal(id, &call)?;
    assert!(matches!(
        replay.state,
        AttemptState::Terminal {
            outcome: Outcome::Rejected,
            ..
        }
    ));
    assert!(
        store
            .record_effect(
                id,
                1,
                SettlementEffect::GitImport,
                EffectDisposition::Done,
                &call
            )
            .is_err()
    );
    Ok(())
}

#[test]
fn receipt_withdrawal_does_not_invent_an_exit() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = TestClock::new(WallTime::from_micros(100)?, MonoTime::from_micros(0));
    let cancel = CancelToken::new();
    let call = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let store = RunStore::open(&context, &call)?;
    let input = input()?;
    store.create(&input, &call)?;
    let id = input.attempts[0].attempt_id;
    let terminal = store
        .withdraw(
            id,
            AttemptState::Queued {
                extra: Default::default(),
            },
            Outcome::Rejected,
            "missing evidence",
            WallTime::from_micros(200)?,
            &call,
        )?
        .unwrap();
    assert!(matches!(
        terminal.state,
        AttemptState::Terminal {
            outcome: Outcome::Rejected,
            exit_code: None,
            ..
        }
    ));
    assert_eq!(terminal.exit_evidence, None);
    assert!(matches!(
        store.replay_terminal(id, &call)?.state,
        AttemptState::Terminal {
            outcome: Outcome::Rejected,
            ..
        }
    ));
    assert!(matches!(
        store.cancel(id, "late", &call)?,
        CancelReceipt::AlreadyTerminal
    ));
    assert_eq!(
        store
            .record(id, &call)?
            .settlement_receipts
            .and_then(|receipt| receipt.cancellation),
        Some(CancellationDisposition::AcknowledgedIneffective)
    );
    Ok(())
}

#[test]
fn receipt_failed_seal_does_not_commit_terminal() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = TestClock::new(WallTime::from_micros(100)?, MonoTime::from_micros(0));
    let cancel = CancelToken::new();
    let call = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let store = RunStore::open(&context, &call)?;
    let input = input()?;
    store.create(&input, &call)?;
    let id = input.attempts[0].attempt_id;
    let attempt = context
        .state_root
        .as_path()
        .join("runs")
        .join(input.run_id.to_string())
        .join("attempts")
        .join(id.to_string());
    std::fs::create_dir(attempt.join("terminal-pending.json"))?;
    assert!(
        store
            .withdraw(
                id,
                AttemptState::Queued {
                    extra: Default::default()
                },
                Outcome::Rejected,
                "no exit",
                WallTime::from_micros(200)?,
                &call
            )
            .is_err()
    );
    assert!(matches!(
        store.record(id, &call)?.state,
        AttemptState::Queued { .. }
    ));
    Ok(())
}

#[test]
fn receipt_delivery_effect_and_late_cancellation_persist() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = TestClock::new(WallTime::from_micros(100)?, MonoTime::from_micros(0));
    let cancel = CancelToken::new();
    let call = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let store = RunStore::open(&context, &call)?;
    let input = input()?;
    store.create(&input, &call)?;
    let id = input.attempts[0].attempt_id;
    store.withdraw(
        id,
        AttemptState::Queued {
            extra: Default::default(),
        },
        Outcome::Rejected,
        "decided",
        WallTime::from_micros(200)?,
        &call,
    )?;
    let failed = EffectDisposition::Failed {
        reason: "push refused".into(),
        retryable: true,
    };
    let receipts = store.record_effect(id, 0, SettlementEffect::Delivery, failed.clone(), &call)?;
    assert_eq!(receipts.delivery, failed);
    assert_eq!(receipts.git_import, EffectDisposition::Pending);
    let receipts = store.record_effect(
        id,
        0,
        SettlementEffect::Delivery,
        EffectDisposition::Done,
        &call,
    )?;
    assert_eq!(receipts.delivery, EffectDisposition::Done);
    store.record_cancellation(
        id,
        0,
        CancellationDisposition::AcknowledgedIneffective,
        &call,
    )?;
    assert!(matches!(
        store.record(id, &call)?.state,
        AttemptState::Terminal {
            outcome: Outcome::Rejected,
            ..
        }
    ));
    Ok(())
}
