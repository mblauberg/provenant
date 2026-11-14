use super::*;

#[test]
fn delivery_restart_drives_pending_retryable_and_uncertain_receipts()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    for mode in [
        "pending",
        "retryable",
        "uncertain_absent",
        "uncertain_present",
        "uncertain_other",
        "ambiguous_error",
        "uncertain_binding_mismatch",
        "cancelled",
        "late_cancel",
        "failed_legacy",
        "cancelled_legacy",
        "definite_failed",
        "binding_mismatch",
        "head_mismatch",
        "writer_busy",
        "effects_busy",
    ] {
        let mut fixture = PrepareFixture::new()?;
        let execution = registered_landed_writer_fixture(&fixture)?;
        if mode == "ambiguous_error" {
            let git = provenant_os::git::git_executable().ok_or("git")?;
            let wrapper = fixture.root.join("ambiguous-git");
            fixture.executable(&wrapper, &format!("#!/bin/sh\nfor arg in \"$@\"; do\n  if [ \"$arg\" = push ]; then '{}' \"$@\" || exit $?; exit 1; fi\ndone\nexec '{}' \"$@\"\n", git.display(), git.display()))?;
            fixture
                .environment
                .push(("PROVENANT_GIT".into(), wrapper.into_os_string()));
        }
        let host = fixture.prepare()?;
        let base = host.head(&fixture.writer)?;
        fixture.commit(&fixture.writer.worktree, "writer change")?;
        let head = host.head(&fixture.writer)?;
        let (clock, cancel) = (tests::clock(), provenant_core::CancelToken::new());
        let call = tests::call(&clock, &cancel);
        let store = RunStore::open(&execution, &call)?;
        let mut input = tests::input();
        input.request.task.writer = Some(fixture.writer.clone());
        input.request.task.deliver = Some(fixture.request.clone());
        store.create(&input, &call)?;
        let id = input.attempt_id;
        let attempt = store.attempt_dir(id, false)?;
        let scope = RunStore::scope(&store.record(id, &call)?);
        let mut driver =
            crate::settlement::Guard::claim(&attempt, scope, &clock)?.ok_or("driver")?;
        driver.set_base(base)?;
        driver.set_head(head)?;
        driver.advance(crate::settlement::Phase::Record)?;
        drop(driver);
        host.seal_launch_binding(&attempt, scope)?;
        if matches!(mode, "binding_mismatch" | "uncertain_binding_mismatch") {
            let other = fixture.root.join("other.git");
            fixture.git(&["init", "--bare", "-q", other.to_str().ok_or("other")?])?;
            fixture.git(&[
                "remote",
                "set-url",
                "--push",
                "origin",
                other.to_str().ok_or("other")?,
            ])?;
        }
        if mode == "head_mismatch" {
            fixture.commit(&fixture.writer.worktree, "after the sealed head")?;
        }
        artifacts::seal(
            &attempt,
            "writer-terminal.json",
            &db::bytes(&writer::WriterTerminal {
                schema_version: 1,
                head,
            })?,
            RECORD_LIMIT,
        )?;
        let initial = if mode == "retryable" {
            EffectDisposition::Failed {
                reason: "prepare failed before start".into(),
                retryable: true,
            }
        } else {
            EffectDisposition::Pending
        };
        if !mode.ends_with("_legacy") {
            artifacts::seal(
                &attempt,
                "terminal-effects.json",
                &db::bytes(&store::TerminalEffects {
                    schema_version: 1,
                    scope,
                    effects: vec![
                        (SettlementEffect::GitImport, EffectDisposition::Done),
                        (SettlementEffect::ManifestConflict, EffectDisposition::Done),
                        (SettlementEffect::Delivery, initial),
                    ],
                })?,
                RECORD_LIMIT,
            )?;
        }
        let lock = attempt.try_lock(
            std::ffi::OsStr::new("custody.lock"),
            provenant_os::fs::LockMode::Exclusive,
        )?;
        drop(lock);
        let mut observed = tests::exit();
        let mut publication = tests::publication();
        if mode == "failed_legacy" {
            publication.outcome = Outcome::Failed;
        }
        if mode == "cancelled_legacy" {
            publication.outcome = Outcome::Cancelled;
            observed.cancel_delivered_before_exit = Some(true);
        }
        dispatch_tests::seal_exit(&store, &observed)?;
        let record = store.publish_terminal(id, 1, &publication, &observed, &call)?;
        let decision =
            artifacts::read_sealed(&attempt, "terminal-pending.json", RECORD_LIMIT)?.unwrap();
        if mode.starts_with("uncertain_") {
            if matches!(mode, "uncertain_present" | "uncertain_other") {
                let remote_head = if mode == "uncertain_present" {
                    head
                } else {
                    base
                };
                fixture.lane_git(&[
                    "push",
                    "origin",
                    &format!("{remote_head}:refs/heads/{}", fixture.request.branch),
                ])?;
            }
            let hook = fixture.root.join("remote.git/hooks/pre-receive");
            fixture.executable(
                &hook,
                &format!(
                    "#!/bin/sh\nprintf 'push\\n' >> '{}'\n",
                    fixture.root.join("pushes").display()
                ),
            )?;
            artifacts::seal(
                &attempt,
                "delivery-started.json",
                &db::bytes(&WriterDeliveryReceipt {
                    extra: Default::default(),
                    schema_version: 1,
                    remote: fixture.request.remote.clone(),
                    branch: fixture.request.branch.clone(),
                    head: Some(head),
                    review_acceptance: None,
                    state: WriterDeliveryState::Failed {
                        extra: Default::default(),
                        stage: WriterDeliveryStage::Interrupted,
                    },
                    integration: None,
                })?,
                RECORD_LIMIT,
            )?;
        } else if mode == "ambiguous_error" {
            let hook = fixture.root.join("remote.git/hooks/pre-receive");
            fixture.executable(
                &hook,
                &format!(
                    "#!/bin/sh\nprintf 'push\\n' >> '{}'\n",
                    fixture.root.join("pushes").display()
                ),
            )?;
            assert!(matches!(
                deliver_once(
                    &host,
                    &fixture.writer,
                    &fixture.request,
                    &attempt,
                    &format!("Run {}\nAttempt {id}", input.attempt_id)
                ),
                Err(RunError::Delivery(WriterDeliveryStage::Push))
            ));
            assert!(artifacts::read_sealed(&attempt, "delivery.json", RECORD_LIMIT)?.is_none());
        } else if mode == "definite_failed" {
            artifacts::seal(
                &attempt,
                "delivery.json",
                &db::bytes(&WriterDeliveryReceipt {
                    extra: Default::default(),
                    schema_version: 1,
                    remote: fixture.request.remote.clone(),
                    branch: fixture.request.branch.clone(),
                    head: Some(head),
                    review_acceptance: None,
                    state: WriterDeliveryState::Failed {
                        extra: Default::default(),
                        stage: WriterDeliveryStage::Push,
                    },
                    integration: None,
                })?,
                RECORD_LIMIT,
            )?;
        } else if mode == "cancelled" {
            let token = provenant_core::CancelToken::new();
            token.cancel();
            deliver_once(
                &host.cancellable(&token),
                &fixture.writer,
                &fixture.request,
                &attempt,
                "cancelled",
            )?;
        } else if mode == "late_cancel" {
            let completed = deliver_once(
                &host,
                &fixture.writer,
                &fixture.request,
                &attempt,
                "completed",
            )?;
            let token = provenant_core::CancelToken::new();
            token.cancel();
            assert_eq!(
                deliver_once(
                    &host.cancellable(&token),
                    &fixture.writer,
                    &fixture.request,
                    &attempt,
                    "late cancel"
                )?,
                completed
            );
        }
        drop(store);
        let store = RunStore::open(&execution, &call)?;
        assert_eq!(
            store
                .pending_effect_attempts(input.attempt_id, &call)?
                .contains(&id),
            !mode.ends_with("_legacy")
        );
        if matches!(mode, "writer_busy" | "effects_busy") {
            // A busy lock leaves the effect pending without an error.
            let slot = store.writer_custody_directory(&record, &fixture.writer)?;
            let (directory, name) = if mode == "writer_busy" {
                (&slot, "writer.lock")
            } else {
                (&attempt, "terminal-effects.lock")
            };
            let held = directory.try_lock(
                std::ffi::OsStr::new(name),
                provenant_os::fs::LockMode::Exclusive,
            )?;
            store.reconcile_terminal_effects(&record, &call)?;
            assert!(matches!(
                store
                    .record(id, &call)?
                    .settlement_receipts
                    .unwrap()
                    .delivery,
                EffectDisposition::Pending
            ));
            assert!(
                !fixture
                    .root
                    .join("remote.git/refs/heads/published/lane")
                    .exists(),
                "{mode}: nothing is delivered while the lock is busy"
            );
            drop(held);
        }
        store.reconcile_terminal_effects(&record, &call)?;
        let recovered = store.record(id, &call)?;
        assert_eq!(recovered.head, Some(head));
        assert_eq!(recovered.state, record.state);
        assert_eq!(
            artifacts::read_sealed(&attempt, "terminal-pending.json", RECORD_LIMIT)?.unwrap(),
            decision
        );
        let receipts = recovered.settlement_receipts.unwrap();
        if matches!(
            mode,
            "pending"
                | "retryable"
                | "late_cancel"
                | "writer_busy"
                | "effects_busy"
                | "uncertain_absent"
                | "uncertain_present"
                | "uncertain_other"
                | "ambiguous_error"
        ) {
            assert!(
                matches!(receipts.delivery, EffectDisposition::Done),
                "{mode}: {receipts:?}"
            );
            assert_ne!(
                receipts.cancellation,
                Some(CancellationDisposition::DeliveryEffective)
            );
            fixture.git(&[
                "--git-dir",
                fixture.root.join("remote.git").to_str().ok_or("remote")?,
                "show-ref",
                "--verify",
                &format!("refs/heads/{}", fixture.request.branch),
            ])?;
            if mode.starts_with("uncertain_") || mode == "ambiguous_error" {
                let receipt = artifacts::read_sealed(&attempt, "delivery.json", RECORD_LIMIT)?
                    .ok_or("receipt")?;
                let receipt: WriterDeliveryReceipt = db::decode(&receipt)?;
                assert_eq!(receipt.head, Some(head));
                assert!(matches!(
                    receipt.state,
                    WriterDeliveryState::Delivered { .. }
                ));
                let calls = std::cell::Cell::new((0, 0));
                let mut react = |reaction: &mut Reaction, _: &mut crate::reactions::Saver<'_>| {
                    let (graph, notice) = calls.get();
                    calls.set((
                        graph + usize::from(!reaction.graph_done),
                        notice + usize::from(!reaction.notice_done),
                    ));
                    reaction.graph_done = true;
                    reaction.notice_done = true;
                    reaction.log_done = true;
                    reaction.ledger_done = true;
                    Ok(())
                };
                store.drain_reactions(input.attempt_id, &clock, &cancel, &mut react)?;
                store.reconcile_terminal_effects(&store.record(id, &call)?, &call)?;
                store.drain_reactions(input.attempt_id, &clock, &cancel, &mut react)?;
                assert_eq!(calls.get(), (1, 1));
                let pushes =
                    std::fs::read_to_string(fixture.root.join("pushes")).unwrap_or_default();
                assert_eq!(
                    pushes.lines().count(),
                    usize::from(mode != "uncertain_present")
                );
            }
        } else if mode.ends_with("_legacy") {
            assert!(matches!(receipts.delivery, EffectDisposition::Done));
            assert!(
                !fixture
                    .root
                    .join("remote.git/refs/heads/published/lane")
                    .exists(),
                "ineligible legacy receipt must not deliver"
            );
        } else {
            assert!(
                !fixture
                    .root
                    .join("remote.git/refs/heads/published/lane")
                    .exists(),
                "uncertain delivery must not repeat"
            );
            if mode == "cancelled" {
                assert!(matches!(
                    receipts.delivery,
                    EffectDisposition::Cancelled {
                        retryable: true,
                        ..
                    }
                ));
                assert_eq!(
                    receipts.cancellation,
                    Some(CancellationDisposition::DeliveryEffective)
                );
                let pickup = store.deliver(
                    id,
                    None,
                    HostDeliveryContext::new(fixture.environment.clone()),
                    false,
                    &call,
                )?;
                assert!(matches!(
                    pickup.state,
                    WriterDeliveryState::Delivered { .. }
                ));
                store.reconcile_terminal_effects(&store.record(id, &call)?, &call)?;
                assert!(matches!(
                    store
                        .record(id, &call)?
                        .settlement_receipts
                        .unwrap()
                        .delivery,
                    EffectDisposition::Done
                ));
            } else {
                let EffectDisposition::Failed { reason, retryable } = &receipts.delivery else {
                    panic!("{mode}: {receipts:?}");
                };
                assert!(!retryable, "{mode}: {reason}");
                assert!(!reason.contains("uncertain"), "{reason}");
                match mode {
                    "binding_mismatch" | "uncertain_binding_mismatch" => {
                        assert!(reason.contains("launch binding"), "{reason}")
                    }
                    "head_mismatch" => assert!(reason.contains("sealed delivery head"), "{reason}"),
                    "definite_failed" => assert!(reason.contains("Push"), "{reason}"),
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

#[test]
fn prepare_reports_missing_git_with_git_code() -> std::result::Result<(), Box<dyn std::error::Error>>
{
    let fixture = PrepareFixture::new()?;
    let mut environment = fixture.environment.clone();
    environment.retain(|(key, _)| key != "PROVENANT_GIT");
    environment.push((
        "PROVENANT_GIT".into(),
        fixture.root.join("missing-git").into_os_string(),
    ));
    let error = HostDeliveryContext::new(environment)
        .prepare(&fixture.writer, &fixture.request)
        .err()
        .ok_or("expected missing Git error")?;
    assert!(matches!(error, RunError::GitUnavailable));
    assert_eq!(error.code(), "GIT");
    Ok(())
}

#[test]
fn prepare_reports_common_git_dir_discovery_failure()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let git = fixture.root.join("failing-git");
    fixture.executable(
        &git,
        "#!/bin/sh\nprintf '%s\\n' 'fatal: https://user:secret@example.com/repo' >&2\nexit 1\n",
    )?;
    let mut environment = fixture.environment.clone();
    environment.push(("PROVENANT_GIT".into(), git.into_os_string()));
    match HostDeliveryContext::new(environment).prepare(&fixture.writer, &fixture.request) {
        Err(RunError::DeliveryPrepare { step, reason }) => {
            assert_eq!(step, "HEAD");
            assert!(reason.contains("example.com"), "{reason}");
            assert!(!reason.contains("secret"), "{reason}");
        }
        other => panic!("expected a prepare failure, got {other:?}"),
    }
    Ok(())
}

#[test]
fn reconcile_settles_exhausted_writer_import_after_durable_exit()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    use provenant_supervisor::custody::WriterBinding;
    let fixture = PrepareFixture::new()?;
    let execution = registered_landed_writer_fixture(&fixture)?;
    let (clock, cancel) = (tests::clock(), provenant_core::CancelToken::new());
    let call = tests::call(&clock, &cancel);
    let store = RunStore::open(&execution, &call)?;
    let mut input = tests::input();
    input.request.task.writer = Some(fixture.writer.clone());
    store.create(&input, &call)?;
    let id = input.attempt_id;
    let record = store.mark_starting(
        &store.record(id, &call)?,
        &tests::route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let scope = RunStore::scope(&record);
    let attempt = store.attempt_dir(id, false)?;
    let mut launch = crate::prepare_writer(
        &fixture.writer,
        &store.writer_registry,
        execution.project_id,
    )?;
    let binding = WriterBinding::from_verified(
        launch.worktree.repo().root_dir(),
        launch.worktree.repo().private_dir(),
        launch.branch.as_str(),
        launch.base,
    )?;
    let directory = launch.custody.take().ok_or("writer custody")?.0;
    drop(provenant_supervisor::fixture::acquire_writer_with_binding(
        directory,
        scope,
        binding,
        call.lock_budget(),
        &cancel,
        &clock,
    )?);
    let reservation = store
        .writer_registry
        .join("worktrees")
        .join(execution.project_id.to_string())
        .join(provenant_confinement::worktrees::state_dir_name(
            &fixture.writer.worktree,
        ))
        .join(scope.attempt.to_string())
        .join("reservation.json");
    let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&reservation)?)?;
    let metadata = attempt.metadata()?;
    value["launch_state"] = "settling".into();
    value["guardian"] = serde_json::json!({"boot":value["boot"], "pid":123, "native_start":1, "group_id":123, "session_id":123});
    value["unresolved_cleanup"] = false.into();
    value["census_incomplete"] = false.into();
    value["attempt_device"] = metadata.device.into();
    value["attempt_inode"] = metadata.inode.into();
    std::fs::write(&reservation, serde_json::to_vec(&value)?)?;
    artifacts::seal(
        &attempt,
        "launch.json",
        &serde_json::to_vec(
            &serde_json::json!({"schema_version":1,"scope":scope,"requester":execution.actor_id}),
        )?,
        RECORD_LIMIT,
    )?;
    let mut exit = tests::exit();
    exit.scope = scope;
    dispatch_tests::seal_exit(&store, &exit)?;
    artifacts::seal(
        &attempt,
        "settlement-progress.json",
        &serde_json::to_vec(
            &serde_json::json!({"schema_version":1,"scope":scope,"phase":"import","attempts":3,"head":null,"base":null,"owner":null,"owner_since_ms":0,"retry_after_ms":0}),
        )?,
        RECORD_LIMIT,
    )?;
    let found = store.reconcile_with_observer(
        input.attempt_id,
        &dispatch_tests::Fake::default(),
        &call,
        &provenant_supervisor::census::SystemProcessObserver,
    )?;
    assert_eq!(found, vec![(id, Reconciled::Finished)]);
    let refusal =
        crate::holder::object_import_refused(&attempt, scope)?.ok_or("refusal evidence")?;
    assert!(matches!(
        store.record(id, &call)?.state,
        AttemptState::Terminal { outcome: Outcome::Failed, reason: Some(reason), .. } if reason == refusal
    ));
    assert!(!reservation.exists());
    Ok(())
}

#[test]
fn terminal_refuses_an_unfinished_host_sequencer()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    std::fs::write(
        fixture.writer.worktree.join("AGENTS.md"),
        b"base instructions\n",
    )?;
    fixture.lane_git(&["add", "AGENTS.md"])?;
    fixture.commit(&fixture.writer.worktree, "instruction baseline")?;
    let result = fixture.terminal(|| {
        std::fs::write(
            fixture.writer.worktree.join("AGENTS.md"),
            b"changed instructions\n",
        )?;
        std::fs::write(
            fixture
                .writer
                .project
                .join(".git/worktrees/lane/CHERRY_PICK_HEAD"),
            b"fixture sequencer state",
        )?;
        Ok(())
    });
    assert!(
        matches!(&result, Err(error) if matches!(error.downcast_ref::<RunError>(), Some(RunError::Writer(provenant_confinement::worktrees::WorktreeError::GitOperationInProgress)))),
        "{result:?}"
    );
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(
        fixture.root.join("attempt/quarantine/manifest.json"),
    )?)?;
    assert_eq!(manifest["disposition"], "writer_settled");
    Ok(())
}

#[test]
fn writer_revalidation_refusal_settles_before_deferred_pin_cleanup()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let execution = registered_landed_writer_fixture(&fixture)?;
    let (clock, cancel) = (tests::clock(), provenant_core::CancelToken::new());
    let call = tests::call(&clock, &cancel);
    let store = RunStore::open(&execution, &call)?;
    let mut input = tests::input();
    input.request.task.writer = Some(fixture.writer.clone());
    store.create(&input, &call)?;
    let id = input.attempt_id;
    let temporary = store.temporary_path(id, &call)?;
    let launch = crate::prepare_writer(
        &fixture.writer,
        execution.state_root.as_path(),
        execution.project_id,
    )?;
    let attempt = store.attempt_dir(id, false)?;
    store.mark_starting(
        &store.record(id, &call)?,
        &tests::route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let scope = RunStore::scope(&store.record(id, &call)?);
    launch.snapshot(execution.state_root.as_path(), &attempt, scope)?;
    let pin = format!("refs/provenant/lane-base/{id}");
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    assert_eq!(
        crate::objects::resolve_ref(&git, launch.worktree.repo(), &pin)?,
        launch.base
    );
    let marker = fixture.writer.worktree.join(".git");
    let original = std::fs::read(&marker)?;
    std::fs::write(&marker, b"gitdir: /nonexistent\n")?;
    let error = launch
        .worktree
        .repo()
        .revalidate()
        .expect_err("changed gitdir must refuse");
    let reason = provenant_confinement::permissions::PermissionError::from(error).to_string();
    let terminal = store
        .withdraw(
            id,
            AttemptState::Starting {
                extra: Default::default(),
            },
            Outcome::Rejected,
            &reason,
            provenant_core::WallTime::from_micros(150)?,
            &call,
        )?
        .ok_or("refusal missing")?;
    assert!(matches!(
        terminal.state,
        AttemptState::Terminal {
            outcome: Outcome::Rejected,
            ..
        }
    ));
    assert!(store.open_attempts(100, &call)?.contains(&input.attempt_id));
    std::fs::write(&marker, original)?;

    // A failed cleanup must retain its retry marker and the pin.
    let common = fixture.writer.project.join(".git");
    let unavailable = fixture.writer.project.join("git-unavailable");
    std::fs::rename(&common, &unavailable)?;
    let observer = provenant_supervisor::census::SystemProcessObserver;
    let result = store.reconcile_with_observer(
        input.attempt_id,
        &crate::dispatch_tests::Ready,
        &call,
        &observer,
    )?;
    assert!(
        result
            .iter()
            .any(|(_, result)| matches!(result, crate::Reconciled::TemporaryRetained { .. }))
    );
    assert!(store.open_attempts(100, &call)?.contains(&input.attempt_id));
    let gc = store.gc_temporary(
        &Directory::open_anchor(execution.state_root.as_path())?,
        7,
        30,
        provenant_core::WallTime::from_micros(86_400_000_150)?,
        false,
        &call,
    )?;
    assert!(gc.errors.is_empty(), "{:?}", gc.errors);
    assert!(!temporary.exists());
    let retained: bool = store.db.query_row(
        "SELECT tmp_retained FROM attempts WHERE id=?1",
        [id.to_string()],
        |row| row.get(0),
    )?;
    assert!(retained);
    let spawned = std::cell::Cell::new(false);
    assert_eq!(
        store.reap_with(&call, |_, _| {
            spawned.set(true);
            Ok(())
        })?,
        1
    );
    assert!(spawned.get());
    std::fs::rename(&unavailable, &common)?;
    let second = store.reconcile_with_observer(
        input.attempt_id,
        &crate::dispatch_tests::Ready,
        &call,
        &observer,
    )?;
    let common = common.to_str().ok_or("non-UTF-8 common Git dir")?;
    let remaining = provenant_os::fixture::run_git(
        &fixture.root,
        &["--git-dir", common, "for-each-ref", &pin],
    )?;
    assert!(
        remaining.is_empty(),
        "{second:?} {:?}",
        store.record(id, &call)?.settlement_receipts
    );
    let retained: bool = store.db.query_row(
        "SELECT tmp_retained FROM attempts WHERE id=?1",
        [id.to_string()],
        |row| row.get(0),
    )?;
    assert!(!retained);

    // Exercise the actual launch path too: damage the verified marker after
    // provider preparation and before handoff obtains writer custody.
    struct WriterProvider {
        writer: WriterRequest,
        state: std::path::PathBuf,
        home: std::path::PathBuf,
        project: provenant_core::ProjectId,
    }
    impl crate::Providers for WriterProvider {
        fn prepare(
            &self,
            task: &TaskRequest,
            prompt: &str,
            paths: &LaunchPaths,
            resume: Option<&AttemptRecord>,
        ) -> std::result::Result<PreparedLaunch, Box<crate::PrepareRefusal>> {
            use provenant_confinement::permissions::{
                AccessMode, NetworkAccess, PathGrant, PathScope, PlanPermissions, ProviderProfile,
                ProviderProfileSpec, WorktreeWriteAuthority,
            };
            let writer = crate::prepare_writer(&self.writer, &self.state, self.project)
                .map_err(|error| crate::fixture_refusal(error.to_string()))?;
            let authority =
                WorktreeWriteAuthority::for_worktree(&writer.worktree, &writer.branch, &self.home)
                    .map_err(|error| crate::fixture_refusal(error.to_string()))?;
            let grant = |path: std::path::PathBuf| PathGrant {
                path,
                scope: PathScope::Subtree,
            };
            let plan =
                provenant_confinement::ConfinedPlan::new(provenant_confinement::PlanRequest {
                    scope: provenant_core::WorkScope::Project(self.project),
                    codex: provenant_confinement::CodexPolicy::Clean,
                    extensions: provenant_confinement::ExtensionPolicy::Deny,
                    agy_state: None,
                    permissions: PlanPermissions {
                        policy: None,
                        read_only: None,
                        mode: AccessMode::WorktreeWrite(Box::new(authority)),
                        scratch: grant(paths.scratch.clone()),
                        private_state: Some(grant(paths.provider_state.clone())),
                        read_roots: vec![grant(self.writer.worktree.clone())],
                        protected_roots: vec![],
                        secret_roots: vec![],
                        selected_authentication: None,
                        include_continuation: false,
                        write_roots: vec![],
                        network: NetworkAccess::Provider,
                        nested_confinement: false,
                    },
                })
                .map_err(|error| crate::fixture_refusal(error.to_string()))?;
            let mut prepared = crate::dispatch_tests::Ready.prepare(task, prompt, paths, resume)?;
            prepared.confinement = Some(crate::LaunchConfinement {
                plan,
                profile: ProviderProfile {
                    spec: ProviderProfileSpec::default(),
                    observations: Default::default(),
                },
                writer: Some(writer),
                home: self.home.clone(),
                cargo_registry_seed: false,
                state_root: self.state.clone(),
                sandboxed: true,
            });
            Ok(prepared)
        }
        fn decode(
            &self,
            route: &RouteRecord,
            output: &[u8],
            exit: &provenant_supervisor::custody::ExitObservation,
            path: &Path,
        ) -> std::result::Result<Decoded, String> {
            crate::dispatch_tests::Ready.decode(route, output, exit, path)
        }
    }
    let fixture = PrepareFixture::new()?;
    let execution = registered_landed_writer_fixture(&fixture)?;
    let store = RunStore::open(&execution, &call)?;
    let mut input = tests::input();
    input.request.task.writer = Some(fixture.writer.clone());
    store.create(&input, &call)?;
    let marker = fixture.writer.worktree.join(".git");
    let original = std::fs::read(&marker)?;
    let provider = WriterProvider {
        writer: fixture.writer.clone(),
        state: execution.state_root.as_path().to_owned(),
        home: fixture.root.join("home"),
        project: execution.project_id,
    };
    let result = store.start(
        input.attempt_id,
        &provider,
        &|_, _| {
            std::fs::write(&marker, b"gitdir: /nonexistent\n").map_err(RunError::Io)?;
            Ok(Vec::new())
        },
        &call,
    )?;
    std::fs::write(&marker, original)?;
    assert!(
        matches!(&result[..], [crate::Started::Refused { reason, .. }] if reason.contains("could not be revalidated")),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn terminal_outcomes_remove_the_launch_base_pin_and_interrupted_import_ref()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    for outcome in [Outcome::Succeeded, Outcome::Cancelled, Outcome::Failed] {
        let fixture = PrepareFixture::new()?;
        let execution = registered_landed_writer_fixture(&fixture)?;
        let (clock, cancel) = (tests::clock(), provenant_core::CancelToken::new());
        let call = tests::call(&clock, &cancel);
        let store = RunStore::open(&execution, &call)?;
        let mut input = tests::input();
        input.request.task.writer = Some(fixture.writer.clone());
        store.create(&input, &call)?;
        let id = input.attempt_id;
        let mut exit = tests::exit();
        if outcome == Outcome::Cancelled {
            exit.cancel_delivered_before_exit = Some(true);
        }
        let attempt = store.attempt_dir(id, false)?;
        let launch = crate::prepare_writer(
            &fixture.writer,
            execution.state_root.as_path(),
            execution.project_id,
        )?;
        launch.snapshot(execution.state_root.as_path(), &attempt, exit.scope)?;
        let pin = format!("refs/provenant/lane-base/{id}");
        let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
        assert_eq!(
            crate::objects::resolve_ref(&git, launch.worktree.repo(), &pin)?,
            launch.base
        );
        // A cancelled import may have pinned its head without publishing the
        // writer-terminal record. Terminal cleanup must consult the journal.
        let mut driver = crate::settlement::Guard::claim(&attempt, exit.scope, &clock)?
            .ok_or("settlement owner")?;
        driver.set_head(launch.base)?;
        drop(driver);
        let stage = format!("refs/provenant/import/{}", launch.base);
        fixture.git(&["update-ref", &stage, &launch.base.to_string()])?;
        tests::seal_observation(&store, &input, &exit)?;
        let mut publication = tests::publication();
        publication.outcome = outcome;
        let terminal = store.publish_terminal(id, 1, &publication, &exit, &call)?;
        assert!(matches!(terminal.state, AttemptState::Terminal { .. }));
        for reference in [&pin, &stage] {
            assert!(
                crate::objects::resolve_ref(&git, launch.worktree.repo(), reference).is_err(),
                "retained terminal ref {reference} after {outcome:?}"
            );
        }
    }
    Ok(())
}

#[test]
fn terminal_verifies_private_objects_before_publication()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    for corrupt in [false, true] {
        let fixture = PrepareFixture::new()?;
        let result = fixture.terminal(|| {
            let attempt_path = fixture.root.join("attempt");
            let scratch = attempt_path.join("tmp/candidate-0");
            let objects = scratch.join("git-objects");
            std::fs::create_dir_all(objects.join("info"))?;
            let executable = provenant_os::git::git_executable().ok_or("Git unavailable")?;
            let executor = SystemExecutor::new(1)?;
            let runtime =
                provenant_confinement::UtilityRuntime::new(&executable, fixture.root.join("home"))?;
            let repo = runtime.open_repo(
                &executor,
                &fixture.writer.project,
                provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))?,
                &provenant_core::CancelToken::new(),
            )?;
            let worktree = provenant_confinement::verify_worktree(&repo, &fixture.writer.worktree)?;
            let shared_objects = worktree.repo().common_git_dir().join("objects");
            std::fs::write(
                objects.join("info/alternates"),
                format!("{}\n", crate::objects::quote_alternate(&shared_objects)),
            )?;
            let base =
                crate::objects::resolve_ref(&executable, worktree.repo(), "refs/heads/lane")?;
            crate::objects::prepare_refs(
                &executable,
                worktree.repo(),
                &Directory::open_anchor(&scratch)?,
                "refs/heads/lane",
                base,
            )?;
            crate::objects::bind(&Directory::open_anchor(&attempt_path)?, &scratch)?;
            std::fs::write(
                fixture.writer.worktree.join("private.txt"),
                "private lane content\n",
            )?;
            for args in [
                vec!["add", "private.txt"],
                vec!["commit", "-qm", "private commit"],
            ] {
                let argv: Vec<OsString> = std::iter::once(executable.as_os_str().to_owned())
                    .chain(args.into_iter().map(Into::into))
                    .collect();
                let mut environment = fixture.environment.clone();
                environment.retain(|(key, _)| {
                    !matches!(
                        key.to_str(),
                        Some(
                            "GIT_OBJECT_DIRECTORY"
                                | "GIT_COMMON_DIR"
                                | "GIT_DIR"
                                | "GIT_WORK_TREE"
                                | "GIT_INDEX_FILE"
                        )
                    )
                });
                environment.push((
                    "GIT_OBJECT_DIRECTORY".into(),
                    objects.clone().into_os_string(),
                ));
                environment.push((
                    "GIT_COMMON_DIR".into(),
                    scratch.join("git-common").into_os_string(),
                ));
                environment.push((
                    "GIT_DIR".into(),
                    scratch.join("git-common").into_os_string(),
                ));
                environment.push((
                    "GIT_WORK_TREE".into(),
                    fixture.writer.worktree.clone().into_os_string(),
                ));
                environment.push((
                    "GIT_INDEX_FILE".into(),
                    scratch.join("git-common/index").into_os_string(),
                ));
                let mut options =
                    ExecOptions::new(&argv, &fixture.writer.worktree, Duration::from_secs(5));
                options.env = Some(&environment);
                let output = SystemExecutor::new(1)?.execute(options)?;
                assert_eq!(
                    output.termination,
                    Termination::Exit(0),
                    "{}",
                    String::from_utf8_lossy(&output.stderr.bytes)
                );
            }
            assert_eq!(
                crate::objects::resolve_ref(&executable, worktree.repo(), "refs/heads/lane")?,
                base
            );
            if corrupt {
                let fanout = std::fs::read_dir(&objects)?
                    .filter_map(|entry| entry.ok())
                    .find(|entry| entry.file_name().len() == 2)
                    .ok_or("loose fanout")?
                    .path();
                let object = std::fs::read_dir(fanout)?
                    .next()
                    .ok_or("loose object")??
                    .path();
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&object, std::fs::Permissions::from_mode(0o600))?;
                std::fs::write(object, "corrupt")?;
            }
            Ok(())
        });
        if corrupt {
            assert!(result.is_err());
            assert!(!fixture.root.join("attempt/writer-terminal.json").exists());
        } else {
            assert!(!result?);
            fixture.lane_git(&["cat-file", "-e", "HEAD:private.txt"])?;
        }
    }
    Ok(())
}
