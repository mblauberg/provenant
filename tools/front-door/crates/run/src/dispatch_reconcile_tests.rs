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
                terminal_cause: None,
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
                terminal_cause: None,
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
fn upgrading_a_version_1_ledger_is_atomic() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (host, cancel) = (Stepped::up(100_000), CancelToken::new());
    let call = stepped_call(&host, &cancel);
    let mut store = RunStore::open(&context(temp.path())?, &call)?;
    store.set_admission_policy(AdmissionPolicy {
        startup_spacing_ms: 20_000,
        read_only_spacing_ms: 2_000,
        ..AdmissionPolicy::default()
    });
    let version_1 = "CREATE TABLE leases (attempt TEXT PRIMARY KEY, project TEXT NOT NULL, store BLOB NOT NULL) STRICT;
         CREATE INDEX lease_projects ON leases(project);";
    // A spacing that cannot be dropped fails the upgrade after it has added
    // the lease class column. A crash at that point must leave version 1 whole.
    old_ledger(
        &store,
        &format!(
            "{version_1}
             CREATE VIEW spacing AS SELECT 1 AS id, 4000000000000 AS last_start;
             PRAGMA user_version=1;"
        ),
    )?;
    let [first, second] = writers(&store, [98, 99], &call)?;
    assert!(store.claim(first, &host, &call).is_err());
    let file = store.writer_registry.join("admission.sqlite3");
    let raw = rusqlite::Connection::open(&file)?;
    let classes: i64 = raw.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('leases') WHERE name='class'",
        [],
        |r| r.get(0),
    )?;
    let version: i64 = raw.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    assert_eq!(
        (classes, version),
        (0, 1),
        "the failed upgrade left no trace"
    );
    raw.execute_batch(
        "DROP VIEW spacing;
         CREATE TABLE spacing (id INTEGER PRIMARY KEY CHECK(id=1), last_start INTEGER NOT NULL) STRICT;
         INSERT INTO spacing(id,last_start) VALUES (1,4000000000000);",
    )?;
    drop(raw);
    assert!(
        store.claim(first, &host, &call)?.is_none(),
        "the upgrade carried the start into the new ledger"
    );
    host.pass_ms(20_500);
    assert!(
        store.claim(first, &host, &call)?.is_some(),
        "a start in the wall future held the class one spacing, and no longer"
    );
    assert!(store.claim(second, &host, &call)?.is_none());
    Ok(())
}
