use super::*;

#[test]
fn a_nested_lane_reads_the_host_ledger_when_it_opens_read_only()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|f| f.message)?;
    let host = root.join("state");
    let mut inputs = provenant_core::RootInputs::new(root.join("home"), root.join("checkout"));
    inputs.state_dir = Some(host.join("children").join("lane"));
    let context = run::ExecutionContext {
        state_root: inputs.resolve(&provenant_os::fs::SystemPathResolver)?.state,
        project_id: provenant_core::ProjectId::from_bytes([1; 16]),
        actor_id: provenant_core::ActorId::from_bytes([2; 16]),
        read_prompts: true,
    };
    // The store must exist to be opened; the writable open creates it.
    run::RunStore::open(&context, &call)?;
    let store = read_only_store(&context, &host, &call)
        .map_err(|f| f.message)?
        .ok_or("nested store")?;
    assert_eq!(store.writer_registry(), host);
    assert_ne!(store.writer_registry(), context.state_root.as_path());
    Ok(())
}
#[test]
fn refused_provider_continuation_keeps_a_durable_attempt() -> Result<(), Box<dyn std::error::Error>>
{
    struct Refusing {
        prior: Option<AttemptId>,
    }
    impl run::Providers for Refusing {
        fn prepare(
            &self,
            _: &run::TaskRequest,
            _: &str,
            _: &run::LaunchPaths,
            resume: Option<&run::AttemptRecord>,
        ) -> Result<run::PreparedLaunch, run::PrepareRefusal> {
            assert_eq!(resume.map(|prior| prior.attempt_id), self.prior);
            Err(run::PrepareRefusal::new(
                provenant_records::run::TerminalCause::HandoffRefused,
                "fixture continuation refused; run provenant run",
            ))
        }
        fn decode(
            &self,
            _: &run::RouteRecord,
            _: &[u8],
            _: &ExitObservation,
            _: &Path,
        ) -> Result<run::Decoded, String> {
            unreachable!("a refused launch never reaches a provider")
        }
    }
    let temp = tempfile::tempdir()?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|fail| fail.message)?;
    let store = trace_store(&temp, &call)?;
    let prior = trace_run(10, AttemptId::from_bytes([9; 16]), clock.wall_now()?)?;
    store.create(&prior, &call)?;
    store.start(
        prior.run_id,
        &Refusing { prior: None },
        &|_, _| unreachable!(),
        &call,
    )?;
    let prior_id = prior.attempts[0].attempt_id;
    let mut resumed = trace_run(20, prior_id, clock.wall_now()?)?;
    resumed.request.parent = Some(prior_id);
    resumed.request.tasks[0].task_id = prior.attempts[0].task_id;
    resumed.attempts[0].task_id = prior.attempts[0].task_id;
    store.create(&resumed, &call)?;
    let started = store.start(
        resumed.run_id,
        &Refusing {
            prior: Some(prior_id),
        },
        &|_, _| unreachable!(),
        &call,
    )?;
    let [run::Started::Refused { attempt, .. }] = started.as_slice() else {
        return Err("continuation must be refused".into());
    };
    assert_eq!(*attempt, resumed.attempts[0].attempt_id);
    // Reopen the store: every ID printed by the refusal must remain
    // addressable after the command exits.
    drop(store);
    let reopened = trace_store(&temp, &call)?;
    let record = run::Service::new(&reopened).status(*attempt, &call)?;
    assert_eq!(record.run_id, resumed.run_id);
    assert!(matches!(
        record.state,
        run::AttemptState::Terminal { outcome: run::Outcome::Rejected, reason: Some(ref reason), .. }
            if reason.contains("fixture continuation refused")
    ));
    Ok(())
}
#[test]
fn cancel_and_reconcile_report_leftover_paths_and_reasons_without_terminal_controls() {
    let state = run::Reconciled::TemporaryRetained {
        path: PathBuf::from("/state/attempts/attempt/tmp"),
        reason: "permission denied\n\x1b[31mretry later\x1b[0m".into(),
    };
    let rendered = reconcile_summary(&state);
    assert!(rendered.contains("/state/attempts/attempt/tmp"));
    assert!(rendered.contains("permission denied"));
    assert!(rendered.contains("retry later"));
    assert!(!rendered.chars().any(char::is_control));

    let state = run::Reconciled::Uncertain {
        reason: run::UncertainReason::ReconcileFailed {
            error: "writer fence listing failed".into(),
        },
    };
    assert!(reconcile_summary(&state).contains("writer fence listing failed"));

    let data =
        json!({"writer_reservation_errors": [{"reason": "legacy reservation has invalid anchor"}]});
    let blocked = reconcile_reply(
        Ok((
            data.clone(),
            "writer reservation: legacy reservation has invalid anchor\n".into(),
        )),
        true,
    );
    assert_eq!(blocked.result.status, Status::Failed);
    assert_eq!(blocked.result.data, data);
    let error = blocked
        .result
        .error
        .expect("failed reconcile must explain its blocker");
    assert_eq!(error.code, "RECONCILE_INCOMPLETE");
    assert!(error.message.contains("invalid anchor"));
}
