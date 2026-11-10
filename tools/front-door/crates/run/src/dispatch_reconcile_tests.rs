use super::*;

#[test]
fn reconcile_does_not_spawn_for_queued_or_terminal_attempts() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    store.create(&run, &call)?;
    store.queue_reconcile_with(run.run_id, &call, |_, _| {
        panic!("a queued attempt has no settlement work");
    })?;
    store.withdraw(
        run.attempts[0].attempt_id,
        AttemptState::Queued {
            extra: Default::default(),
        },
        Outcome::Cancelled,
        "cancelled before launch",
        clock.wall_now()?,
        &call,
    )?;
    // The terminal commit owes its reactions, so recovery is queued for them.
    let mut spawned = 0;
    store.queue_reconcile_with(run.run_id, &call, |_, _| {
        spawned += 1;
        Ok(())
    })?;
    assert_eq!(spawned, 1);
    // Once they are done, a terminal run has no settlement work.
    store.drain_reactions(run.run_id, &clock, &cancel, &mut |reaction, _| {
        reaction.graph_done = true;
        reaction.notice_done = true;
        reaction.log_done = true;
        reaction.ledger_done = true;
        Ok(())
    })?;
    assert!(
        store
            .queue_reconcile_with(run.run_id, &call, |_, _| {
                panic!("a terminal run has no settlement work");
            })?
            .is_empty()
    );
    Ok(())
}
#[test]
fn failed_fresh_preparation_leaves_no_short_alias()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let short = std::fs::canonicalize(temp.path())?.join("short");
    std::fs::create_dir(&short)?;
    crate::short_temporary::use_parent_in_tests(&short);
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let providers = RefusesAfterAlias(std::cell::Cell::new(None));

    let refused = store.prepare_fresh_start(&input(), &providers, &call);

    assert!(refused.is_err());
    assert_eq!(providers.0.get(), Some(true), "the launch had an alias");
    assert_eq!(
        std::fs::read_dir(&short)?.count(),
        0,
        "the alias was unlinked"
    );
    Ok(())
}
/// Persist a named route's resolved effort at launch admission.
#[test]
fn resolved_named_route_survives_launch_admission() -> TestResult {
    let resolved = provenant_records::ResolvedRoute {
        extra: Default::default(),
        adapter: "codex".into(),
        model: "gpt-6.1-sol".into(),
        effort: Some("high".into()),
        catalogue_revision: "fixture".into(),
        account_key: None,
        host: "fixture".into(),
        alternatives: Default::default(),
        exclusions: vec![],
        eligibility: provenant_records::RouteEligibility::Eligible,
        readiness: provenant_records::RouteReadiness::unknown(),
        confidentiality: provenant_records::RouteConfidentiality {
            extra: Default::default(),
            evidence_ref: "fixture".into(),
        },
        pool_selection: None,
    };
    let route = RouteRecord::from_resolved(&resolved, Some("fixture"), "named_route");
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let mut prepared = input();
    prepared.request.tasks[0].route = RequestedRoute::Named {
        extra: Default::default(),
        name: "fixture".into(),
        effort: None,
    };
    store.create(&prepared, &call)?;
    let pending = store.record(prepared.attempts[0].attempt_id, &call)?;
    store.mark_starting(&pending, &route, &[], &Default::default(), None, &call)?;
    let record = store.record(prepared.attempts[0].attempt_id, &call)?;
    assert_eq!(
        record
            .applied_route
            .as_ref()
            .and_then(|route| route.effort.as_deref()),
        Some("high")
    );
    assert_eq!(record.resolved_route, record.applied_route);
    Ok(())
}
#[test]
fn preparation_pacing_refusal_uses_typed_skipped_route_cause() -> TestResult {
    use provenant_records::RouteExclusionReason;
    struct Excluded {
        cause: Option<RouteExclusionReason>,
        message: &'static str,
    }
    impl Providers for Excluded {
        fn prepare(
            &self,
            _: &TaskRequest,
            _: &str,
            _: &LaunchPaths,
            _: Option<&AttemptRecord>,
        ) -> std::result::Result<PreparedLaunch, crate::PrepareRefusal> {
            Err(crate::PrepareRefusal {
                boundary_class: provenant_records::diagnostic::BoundaryClass::None,
                resource: None,
                terminal_cause: provenant_records::run::TerminalCause::HandoffRefused,
                terminal_note: None,
                message: self.message.into(),
                fallback_outcome: None,
                skipped_routes: [
                    Some(RouteExclusionReason::UnknownAvailability),
                    self.cause.clone(),
                ]
                .into_iter()
                .enumerate()
                .map(|(index, cause)| SkippedRoute {
                    extra: Default::default(),
                    route: format!("codex/example-{index}"),
                    kind: SkipKind::Unavailable,
                    cause,
                    detail: "opaque candidate detail".into(),
                })
                .collect(),
            })
        }
        fn decode(
            &self,
            _: &RouteRecord,
            _: &[u8],
            _: &ExitObservation,
            _: &std::path::Path,
        ) -> std::result::Result<Decoded, String> {
            unreachable!()
        }
    }
    for (cause, message, expected_kind) in [
        (
            Some(RouteExclusionReason::PacingReserve),
            "opaque preparation refusal",
            RefusalKind::PacingReserve,
        ),
        (
            Some(RouteExclusionReason::UnknownAvailability),
            "opaque preparation refusal",
            RefusalKind::Preparation,
        ),
        (
            None,
            "codex/example: PacingReserve",
            RefusalKind::Preparation,
        ),
    ] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let run = input();
        let id = run.attempts[0].attempt_id;
        store.create(&run, &call)?;
        let started = store.start(
            run.run_id,
            &Excluded {
                cause: cause.clone(),
                message,
            },
            &|_, _| unreachable!(),
            &call,
        )?;
        assert!(matches!(
            started.as_slice(),
            [Started::Refused { attempt, kind, reason }]
                if *attempt == id && *kind == expected_kind && reason == message
        ));
        assert_eq!(terminal(&store, id, &call)?, Outcome::Rejected);
        let record = store.record(id, &call)?;
        assert_eq!(record.provider_launched, Some(false));
        assert_eq!(record.skipped_routes[1].cause, cause);
        assert_eq!(
            record.terminal_evidence,
            Some(provenant_records::run::TerminalEvidence::PreLaunchRefusal)
        );
    }
    Ok(())
}
#[test]
fn all_candidates_skipped_for_auth_settle_auth_required() -> TestResult {
    struct SignedOut(bool);
    impl Providers for SignedOut {
        fn prepare(
            &self,
            _: &TaskRequest,
            _: &str,
            _: &LaunchPaths,
            _: Option<&AttemptRecord>,
        ) -> std::result::Result<PreparedLaunch, crate::PrepareRefusal> {
            Err(crate::PrepareRefusal {
                boundary_class: provenant_records::diagnostic::BoundaryClass::None,
                resource: None,
                terminal_cause: provenant_records::run::TerminalCause::HandoffRefused,
                terminal_note: None,
                message: "all candidates require sign-in; run provenant accounts".into(),
                fallback_outcome: (!self.0)
                    .then_some(provenant_records::run::PreparationFailure::AuthRequired),
                skipped_routes: ["cursor/example", "codex/example"]
                    .map(|route| SkippedRoute {
                        extra: Default::default(),
                        route: route.into(),
                        kind: if self.0 && route.starts_with("codex/") {
                            SkipKind::Unavailable
                        } else {
                            SkipKind::AuthRequired
                        },
                        cause: None,
                        detail: "sign-in required".into(),
                    })
                    .to_vec(),
            })
        }
        fn decode(
            &self,
            _: &RouteRecord,
            _: &[u8],
            _: &ExitObservation,
            _: &std::path::Path,
        ) -> std::result::Result<Decoded, String> {
            unreachable!()
        }
    }
    for mixed in [false, true] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let run = input();
        let id = run.attempts[0].attempt_id;
        store.create(&run, &call)?;
        store.start(run.run_id, &SignedOut(mixed), &|_, _| unreachable!(), &call)?;
        assert_eq!(
            terminal(&store, id, &call)?,
            if mixed {
                Outcome::Rejected
            } else {
                Outcome::AuthRequired
            }
        );
        let record = store.record(id, &call)?;
        assert_eq!(
            record.terminal_evidence,
            Some(provenant_records::run::TerminalEvidence::PreLaunchRefusal)
        );
        assert_eq!(record.provider_launched, Some(false));
        assert_eq!(
            record.preparation_failure,
            (!mixed).then_some(provenant_records::run::PreparationFailure::AuthRequired)
        );
        assert!(store.custody_released(&record, &call)?);
        if !mixed {
            let mut corrupt = record.clone();
            corrupt.provider_launched = None;
            assert!(matches!(
                crate::reader::validate_attempt(&corrupt),
                Err(RunError::Validation("preparation failure evidence"))
            ));
        }
        assert_eq!(record.skipped_routes.len(), 2);
        assert_eq!(
            record
                .skipped_routes
                .iter()
                .all(|route| route.kind == SkipKind::AuthRequired),
            !mixed
        );
    }
    Ok(())
}

#[test]
fn reconcile_retries_busy_store_and_keeps_judging_other_records() -> TestResult {
    use std::{sync::mpsc, time::Duration};
    for clears in [true, false] {
        provenant_sqlite::with_busy_timeout(Duration::from_millis(300), || -> TestResult {
            let temp = tempfile::tempdir()?;
            let clock = provenant_os::clock::SystemClock::new();
            let cancel = CancelToken::new();
            let call = Request {
                clock: &clock,
                cancel: &cancel,
            };
            let context = context(temp.path())?;
            let store = RunStore::open(&context, &call)?;
            let run = two_tasks(2);
            store.create(&run, &call)?;
            let first = run.attempts[0].attempt_id;
            let second = run.attempts[1].attempt_id;
            store.mark_starting(
                &store.record(first, &call)?,
                &route(),
                &[],
                &Default::default(),
                None,
                &call,
            )?;
            let blocker =
                rusqlite::Connection::open(context.state_root.as_path().join("run.sqlite3"))?;
            blocker.execute_batch("BEGIN IMMEDIATE")?;
            let (release, released) = mpsc::channel();
            let holder = std::thread::spawn(move || {
                if clears {
                    let _ = released.recv_timeout(Duration::from_millis(150));
                } else {
                    released.recv().unwrap();
                }
                blocker.execute_batch("ROLLBACK").unwrap();
            });
            let found = store.reconcile_with_observer(
                run.run_id,
                &Fake::default(),
                &call,
                &provenant_supervisor::census::SystemProcessObserver,
            )?;
            if !clears {
                release.send(())?;
            }
            holder.join().unwrap();
            assert!(found.contains(&(second, Reconciled::Queued)), "{found:?}");
            if clears {
                assert!(found.contains(&(first, Reconciled::Withdrawn)), "{found:?}");
                assert!(
                    store
                        .reconcile_with_observer(
                            run.run_id,
                            &Fake::default(),
                            &call,
                            &provenant_supervisor::census::SystemProcessObserver
                        )?
                        .iter()
                        .all(|(id, _)| *id == second)
                );
            } else {
                assert!(
                    found
                        .iter()
                        .any(|(id, state)| *id == first
                            && matches!(state, Reconciled::Uncertain { .. })),
                    "{found:?}"
                );
            }
            Ok(())
        })?;
    }
    Ok(())
}
