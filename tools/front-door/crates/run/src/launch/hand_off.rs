use super::*;

impl RunStore {
    #[expect(
        clippy::too_many_arguments,
        reason = "the requester is resolved once per run, not per attempt"
    )]
    pub(super) fn hand_off(
        &self,
        task: &TaskRequest,
        mut plan: PreparedLaunch,
        requester: ActorId,
        context: &dyn Fn(&AttemptRecord, ActorId) -> Result<Vec<u8>>,
        acquire_writer: &dyn Fn(
            provenant_os::fs::Directory,
            Scope,
            custody::WriterBinding,
        ) -> custody::Result<custody::CustodyLease>,
        record: &AttemptRecord,
        runtime_seconds: u64,
        call: &Request<'_>,
    ) -> std::result::Result<(), HandOff> {
        // Each failure names its phase, so the refusal says what was not ready.
        let local = |phase: &str, error: &dyn std::fmt::Display| {
            HandOff::Local(format!("{phase}: {error}"))
        };
        let id = record.attempt_id;
        let scope = Scope {
            run: record.run_id,
            attempt: id,
            generation: record.generation,
        };
        let directory = || {
            self.attempt_dir(record.run_id, id, false)
                .map_err(|e| local("attempt directory", &e))
        };
        let mut holder_context =
            context(record, requester).map_err(|e| local("launch context", &e))?;
        let reservation = PublicationReservation::prepare(
            directory()?,
            call.lock_budget(),
            call.cancel,
            call.clock,
        )
        .map_err(|e| local("publication reservation", &e))?;
        // Clone before acquiring custody: failure leaves no writer reservation.
        let held = if self.sealed_requester.is_some() {
            Some((
                custody::hold(
                    reservation
                        .directory()
                        .try_clone()
                        .map_err(|e| local("publication reservation", &e))?,
                    scope,
                    requester,
                ),
                reservation
                    .custody_metadata()
                    .map_err(|e| local("publication reservation", &e))?,
            ))
        } else {
            None
        };
        let writer = plan.confinement.as_mut().and_then(|c| c.writer.as_mut());
        if task.writer.as_ref().map(|w| w.worktree.as_path())
            != writer.as_ref().map(|w| w.worktree.path())
        {
            return Err(local("writer confinement", &"does not match the task"));
        }
        let writer_path = writer
            .as_ref()
            .map(|writer| writer.worktree.path().to_path_buf());
        let mut lease = match writer {
            Some(writer) => {
                let (directory, binding) = writer
                    .custody
                    .take()
                    .ok_or_else(|| local("writer custody", &"missing"))?;
                self.acquire_writer_fenced(directory, scope, binding, acquire_writer, call)
            }
            // Readers take the same injected acquisition as writers, so a
            // fixture guardian and its reservation share one boot observer.
            None => acquire_writer(directory()?, scope, custody::WriterBinding::read_only())
                .map_err(crate::writer::WriterCustodyError::Admission),
        }
        .map_err(|e| match e {
            crate::writer::WriterCustodyError::Admission(custody::CustodyError::Busy) => {
                HandOff::Busy
            }
            crate::writer::WriterCustodyError::Admission(
                custody::CustodyError::ReservationRetained(error),
            ) => HandOff::Protected(format!("custody reservation: {error}")),
            crate::writer::WriterCustodyError::Admission(
                custody::CustodyError::ImportRetained { run, attempt },
            ) => HandOff::RetainedImport {
                run,
                attempt,
                worktree: writer_path.clone().unwrap_or_default(),
            },
            crate::writer::WriterCustodyError::Admission(error) => local("custody", &error),
            crate::writer::WriterCustodyError::Withdrawal { admission, release } => {
                HandOff::Protected(format!(
                    "custody admission: {admission}; custody withdrawal: {release}"
                ))
            }
        })?;
        // Once custody is acquired, every local preparation failure must
        // withdraw it before fallback can consider another candidate.
        macro_rules! local_or_withdraw {
            ($result:expr) => {
                match $result {
                    Ok(value) => value,
                    Err(error) => {
                        lease.withdraw_unlaunched().map_err(|release| {
                            let cause = match &error {
                                HandOff::Local(cause) => cause.as_str(),
                                _ => "launch preparation failed",
                            };
                            HandOff::Protected(format!("{cause}; custody withdrawal: {release}"))
                        })?;
                        return Err(error);
                    }
                }
            };
        }
        if record.retry_of.is_some()
            && let Some(writer) = plan
                .confinement
                .as_ref()
                .and_then(|plan| plan.writer.as_ref())
            && let Err(error) = self.check_writer_lineage(record, &writer.worktree, call)
        {
            local_or_withdraw!(Err::<(), _>(local("writer lineage", &error)));
        }
        if task.source_head.is_some()
            && let Some(writer) = plan
                .confinement
                .as_ref()
                .and_then(|plan| plan.writer.as_ref())
        {
            let git = local_or_withdraw!(
                provenant_os::git::git_executable()
                    .ok_or_else(|| local("Git", &RunError::GitUnavailable))
            );
            let paths = local_or_withdraw!(
                writer
                    .uncommitted_paths(&git)
                    .map_err(|error| local("writer status", &error))
            );
            if !paths.is_empty() {
                local_or_withdraw!(Err::<(), _>(local(
                    "writer continuation",
                    &format!("uncommitted predecessor edits: {}", paths.join(", ")),
                )));
            }
        }
        // Reconcile may have withdrawn this attempt before the lease existed.
        match self.record(id, call) {
            Ok(now)
                if now.revision == record.revision
                    && now.run_id == record.run_id
                    && now.task_id == record.task_id => {}
            Ok(_) => {
                local_or_withdraw!(Err::<(), _>(local("attempt", &"changed before handoff")));
            }
            Err(error) => {
                local_or_withdraw!(Err::<(), _>(local("attempt record", &error)));
            }
        }
        if let Some(deliver) = &task.deliver {
            let prepare = || -> Result<Vec<u8>> {
                let mut holder: HolderContext = decode(&holder_context)?;
                let writer = task
                    .writer
                    .as_ref()
                    .ok_or(RunError::Validation("delivery requires a writer"))?;
                let host = holder
                    .host_delivery
                    .take()
                    .ok_or(RunError::Validation("delivery lacks host context"))?;
                let host = host.prepare(writer, deliver)?;
                host.seal_launch_binding(&self.attempt_dir(record.run_id, id, false)?, scope)?;
                holder.host_delivery = Some(host);
                holder.encode()
            };
            match prepare() {
                Ok(context) => holder_context = context,
                Err(error) => {
                    local_or_withdraw!(Err::<(), _>(local("delivery", &error)));
                }
            }
        }
        // Keep the shared config lock until custody confirms the child launch.
        let mut _git_setup_lock = None;
        if let Some(confinement) = &mut plan.confinement {
            let writer_attempt = local_or_withdraw!(directory());
            _git_setup_lock = match retry_setup_lock(
                record.retry_of.is_some(),
                |budget| {
                    confinement.prepare_git(
                        &writer_attempt,
                        call.cancel,
                        record.retry_of.map(|_| budget),
                    )
                },
                || call.check(),
            ) {
                Ok(lock) => lock,
                Err(error) => {
                    lease.withdraw_unlaunched().map_err(|release| {
                        HandOff::Protected(format!(
                            "Git setup: {error}; custody withdrawal: {release}"
                        ))
                    })?;
                    return Err(match error {
                        RunError::SetupLockTimeout { seconds } => {
                            HandOff::SetupLockTimeout(seconds)
                        }
                        error => local("Git setup", &error),
                    });
                }
            };
            // Cache seeding can walk and sync a large registry. Release the
            // repository lock during that work, then revalidate Git under a
            // fresh lock before constructing the confined command and launch.
            let seed_attempt = local_or_withdraw!(directory());
            seed_without_setup_lock(&mut _git_setup_lock, || {
                #[cfg(feature = "fixture")]
                hold_seed_for_fixture();
                confinement.seed_cargo(&plan.environment, &seed_attempt);
            });
            _git_setup_lock = match retry_setup_lock(
                record.retry_of.is_some(),
                |budget| {
                    confinement.prepare_git(
                        &writer_attempt,
                        call.cancel,
                        record.retry_of.map(|_| budget),
                    )
                },
                || call.check(),
            ) {
                Ok(lock) => lock,
                Err(error) => {
                    lease.withdraw_unlaunched().map_err(|release| {
                        HandOff::Protected(format!(
                            "Git setup: {error}; custody withdrawal: {release}"
                        ))
                    })?;
                    return Err(match error {
                        RunError::SetupLockTimeout { seconds } => {
                            HandOff::SetupLockTimeout(seconds)
                        }
                        error => local("Git setup", &error),
                    });
                }
            };
            let writer_attempt = local_or_withdraw!(directory());
            if let Some(writer) = &confinement.writer
                && let Err(error) = crate::objects::prepare_bound(
                    &local_or_withdraw!(
                        provenant_os::git::git_executable()
                            .ok_or_else(|| local("Git", &RunError::GitUnavailable))
                    ),
                    writer.worktree.repo(),
                    &writer_attempt,
                    &confinement.plan.permissions().scratch.path,
                    writer.branch.as_str(),
                    writer.base,
                )
                .and_then(|()| {
                    writer.snapshot(self.context.state_root.as_path(), &writer_attempt, scope)
                })
            {
                local_or_withdraw!(Err::<(), _>(local("lane Git objects", &error)));
            }
            match confinement.command(&plan.argv, &id, call.cancel) {
                Ok(argv) => plan.argv = argv,
                Err(error) => {
                    local_or_withdraw!(Err::<(), _>(local("confinement command", &error)));
                }
            }
        }
        // Route records name models as `adapter/model`; prefix only a
        // provider-native one (which may itself contain `/`).
        let provider = plan.route.provider.as_deref().unwrap_or_default();
        let model = plan.route.model.as_deref().unwrap_or_default();
        let route = match model.strip_prefix(provider) {
            Some(rest) if rest.starts_with('/') => model.to_owned(),
            _ => format!("{provider}/{model}"),
        };
        let lane_ports = local_or_withdraw!(
            plan.confinement
                .as_ref()
                .filter(|c| c.writer.is_some())
                .map(|c| lane_port_plan(&c.state_root))
                .transpose()
                .map_err(|error| local("lane ports", &error))
        )
        .flatten();
        let command = ProviderCommand {
            lane_ports,
            resolved_route: route,
            permission_plan_digest: task.authority_digest,
            task: task.task_id,
            requester,
            protected_commands: vec![],
            // OpenCode stores answering-model evidence in its shared XDG data
            // directory. The guardian otherwise starts with an empty environment
            // and no Registry home; retain the absolute path selected at launch.
            config_environment: if provider == "opencode" {
                vec!["XDG_DATA_HOME".into()]
            } else {
                vec![]
            },
            argv: plan.argv,
            environment: plan.environment,
            cwd: plan.cwd,
            stdin: plan.stdin,
            secret_input: plan.secret_input,
            holder_context,
            priority: local_or_withdraw!(
                record
                    .requested_priority
                    .ok_or_else(|| local("lane priority", &"missing"))
            ),
        };
        let capture = capture_plan(task, runtime_seconds);
        // Held before launch: an unconfirmed handoff may still have a guardian.
        if let Some((held, custody)) = held {
            self.hold_child(held, custody);
        }
        custody::launch(
            OwnedLaunch {
                command,
                lease: &mut lease,
                handshake: call.handshake_budget(),
                cancel: call.cancel,
                clock: call.clock,
            },
            capture,
            reservation,
        )
        .map_err(|failure| HandOff::Uncertain(failure.error.to_string()))?;
        Ok(())
    }
}

// The seed can be slow, while the lock protects shared Git state. The caller
// must reacquire it and revalidate Git after this function returns.
pub(crate) fn seed_without_setup_lock<T>(lock: &mut Option<T>, seed: impl FnOnce()) {
    drop(lock.take());
    seed();
}

/// Integration fixtures can hold the actual handoff at the seed boundary.
#[cfg(feature = "fixture")]
#[allow(clippy::disallowed_methods)]
fn hold_seed_for_fixture() {
    let Some(directory) = std::env::var_os("PROVENANT_FIXTURE_HOLD_SEED") else {
        return;
    };
    let directory = std::path::Path::new(&directory);
    if std::fs::write(directory.join("entered"), b"entered").is_err() {
        return;
    }
    let _ = provenant_test_support::wait_for(
        "seed preparation release",
        std::time::Duration::from_secs(30),
        || !directory.join("hold").exists(),
        |released| *released,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{call, clock, context, input, route};
    use provenant_core::CancelToken;

    #[test]
    fn missing_priority_withdraws_acquired_custody()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let request = input();
        store.create(&request, &call)?;
        let id = request.attempts[0].attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let record = store.claim(id, &clock, &call)?.ok_or("claim")?;
        assert!(record.requested_priority.is_none());
        let plan = || PreparedLaunch {
            argv: vec!["fixture".into()],
            environment: Vec::new(),
            stdin: Vec::new(),
            secret_input: None,
            cwd: temp.path().into(),
            route: route(),
            skipped_routes: Vec::new(),
            observed_head: None,
            repository: None,
            confinement: None,
        };
        let acquire = |directory, scope, binding| {
            custody::acquire_writer(
                directory,
                scope,
                binding,
                call.lock_budget(),
                call.cancel,
                call.clock,
            )
        };
        let result = store.hand_off(
            &request.request.tasks[0],
            plan(),
            provenant_core::ActorId::from_bytes([2; 16]),
            &|_, _| Ok(Vec::new()),
            &acquire,
            &record,
            60,
            &call,
        );
        match result {
            Err(HandOff::Local(reason)) => assert!(reason.contains("lane priority"), "{reason}"),
            Err(HandOff::Busy) => panic!("custody busy"),
            Err(HandOff::SetupLockTimeout(_) | HandOff::RetainedImport { .. }) => {
                panic!("unexpected refusal")
            }
            Err(HandOff::Uncertain(reason)) => panic!("uncertain: {reason}"),
            Err(HandOff::Protected(reason)) => panic!("protected: {reason}"),
            Ok(()) => panic!("unexpected launch"),
        }
        let directory = store.attempt_dir(record.run_id, record.attempt_id, false)?;
        let lease = custody::acquire_reader(
            directory,
            RunStore::scope(&record),
            call.lock_budget(),
            call.cancel,
            call.clock,
        )?;
        lease.withdraw_unlaunched()?;
        // Each handoff reserves a fresh publication file; these calls exercise
        // separate failure paths against the same attempt fixture.
        store
            .attempt_dir(record.run_id, id, false)?
            .unlink(std::ffi::OsStr::new("reserve"))?;
        let result = store.hand_off(
            &request.request.tasks[0],
            plan(),
            provenant_core::ActorId::from_bytes([2; 16]),
            &|_, _| Ok(Vec::new()),
            &|_, _, _| Err(custody::CustodyError::ReservationRetained("fixture".into())),
            &record,
            60,
            &call,
        );
        match result {
            Err(HandOff::Protected(reason)) => assert!(reason.contains("fixture"), "{reason}"),
            Err(HandOff::Uncertain(reason)) => panic!("uncertain: {reason}"),
            Err(HandOff::Local(reason)) => panic!("local: {reason}"),
            Err(HandOff::Busy) => panic!("busy"),
            Err(HandOff::SetupLockTimeout(_) | HandOff::RetainedImport { .. }) => {
                panic!("unexpected refusal")
            }
            Ok(()) => panic!("unexpected launch"),
        }
        store
            .attempt_dir(record.run_id, id, false)?
            .unlink(std::ffi::OsStr::new("reserve"))?;
        let result = store.hand_off(
            &request.request.tasks[0],
            plan(),
            provenant_core::ActorId::from_bytes([2; 16]),
            &|_, _| Ok(Vec::new()),
            &|directory, scope, binding| {
                let slot = directory.try_clone()?;
                let lease = acquire(directory, scope, binding)?;
                let evidence =
                    slot.open_directory(std::ffi::OsStr::new(&scope.attempt.to_string()))?;
                evidence.unlink(std::ffi::OsStr::new("settlement.json"))?;
                evidence.mkdir(std::ffi::OsStr::new("settlement.json"), 0o700)?;
                Ok(lease)
            },
            &record,
            60,
            &call,
        );
        match result {
            Err(HandOff::Protected(reason)) => {
                assert!(reason.contains("lane priority") && reason.contains("custody withdrawal"))
            }
            Err(HandOff::Uncertain(reason)) => panic!("uncertain: {reason}"),
            Err(HandOff::Local(reason)) => panic!("local: {reason}"),
            Err(HandOff::Busy) => panic!("busy"),
            Err(HandOff::SetupLockTimeout(_) | HandOff::RetainedImport { .. }) => {
                panic!("unexpected refusal")
            }
            Ok(()) => panic!("unexpected launch"),
        }
        assert!(custody::reservation_present(
            &store.attempt_dir(record.run_id, id, false)?,
            RunStore::scope(&record),
        )?);
        Ok(())
    }

    #[test]
    fn failed_unknown_disposition_write_keeps_custody_and_prior_operation()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let request = input();
        store.create(&request, &call)?;
        let id = request.attempts[0].attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let record = store.claim(id, &clock, &call)?.ok_or("claim")?;
        let scope = RunStore::scope(&record);
        let directory = store.attempt_dir(record.run_id, record.attempt_id, false)?;
        let lease = custody::acquire_writer(
            directory.try_clone()?,
            scope,
            custody::WriterBinding::read_only(),
            call.lock_budget(),
            call.cancel,
            call.clock,
        )?;
        let prior = store
            .operation(&request.operation_id, &call)?
            .ok_or("operation")?;
        store.db.execute_batch("CREATE TEMP TRIGGER fail_unknown BEFORE UPDATE ON operations BEGIN SELECT RAISE(ABORT, 'fixture unknown write'); END;")?;
        assert!(matches!(
            store.launch_unknown(request.run_id, &call),
            Err(RunError::Sqlite(_))
        ));
        assert_eq!(store.operation(&request.operation_id, &call)?, Some(prior));
        assert!(custody::reservation_present(&directory, scope)?);
        store.db.execute_batch("DROP TRIGGER fail_unknown")?;
        store.launch_unknown(request.run_id, &call)?;
        assert_eq!(
            store
                .operation(&request.operation_id, &call)?
                .ok_or("operation")?
                .state,
            OperationState::LaunchUnknown
        );
        assert!(custody::reservation_present(&directory, scope)?);
        drop(lease);
        Ok(())
    }

    #[test]
    fn failed_withdrawal_blocks_the_operation_without_relaunch()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        for fail_save in [false, true] {
            let temp = tempfile::tempdir()?;
            let (clock, cancel) = (clock(), CancelToken::new());
            let call = call(&clock, &cancel);
            let store = RunStore::open(&context(temp.path())?, &call)?;
            let request = input();
            store.create(&request, &call)?;
            let acquisitions = std::cell::Cell::new(0);
            let acquire = |directory: provenant_os::fs::Directory, scope: Scope, binding| {
                acquisitions.set(acquisitions.get() + 1);
                let slot = directory.try_clone()?;
                let lease = custody::acquire_writer(
                    directory,
                    scope,
                    binding,
                    call.lock_budget(),
                    call.cancel,
                    call.clock,
                )?;
                slot.open_directory(std::ffi::OsStr::new(&scope.attempt.to_string()))?
                    .mkdir(std::ffi::OsStr::new("settlement.json"), 0o700)?;
                let mut changed = store.record(scope.attempt, &call).expect("attempt record");
                changed.revision += 1;
                store
                    .write(&call, |tx| {
                        tx.execute(
                            "UPDATE attempts SET revision=?1, record=?2 WHERE id=?3",
                            params![
                                i64::try_from(changed.revision).expect("fixture revision"),
                                crate::db::bytes(&changed)?,
                                scope.attempt.to_string()
                            ],
                        )?;
                        Ok(())
                    })
                    .expect("change attempt before handoff");
                if fail_save {
                    store.db.execute_batch("CREATE TEMP TRIGGER fail_blocked BEFORE UPDATE ON operations BEGIN SELECT RAISE(ABORT, 'fixture blocked write'); END;").expect("inject write failure");
                }
                Ok(lease)
            };
            let launch = || {
                store.start_inner(
                    request.run_id,
                    &crate::dispatch_tests::Ready,
                    &|_, _| Ok(Vec::new()),
                    &acquire,
                    None,
                    None,
                    &call,
                )
            };
            if fail_save {
                assert!(matches!(launch(), Err(RunError::Sqlite(_))));
                store.db.execute_batch("DROP TRIGGER fail_blocked")?;
            } else {
                assert!(
                    matches!(launch()?.as_slice(), [Started::Blocked { error, .. }] if error.contains("changed before handoff") && error.contains("custody withdrawal"))
                );
            }
            let operation = store
                .operation(&request.operation_id, &call)?
                .ok_or("operation")?;
            if fail_save {
                assert_eq!(operation.state, OperationState::Accepted);
                assert!(operation.blocked_reason.is_none());
            } else {
                assert_eq!(operation.state, OperationState::Blocked);
                assert!(
                    operation
                        .blocked_reason
                        .as_deref()
                        .is_some_and(|reason| reason.contains("custody withdrawal"))
                );
            }
            let reopened = RunStore::open(&context(temp.path())?, &call)?;
            assert_eq!(
                reopened.operation(&request.operation_id, &call)?,
                Some(operation)
            );
            let record = store.record(request.attempts[0].attempt_id, &call)?;
            assert!(custody::reservation_present(
                &store.attempt_dir(request.run_id, record.attempt_id, false)?,
                RunStore::scope(&record)
            )?);
            assert!(launch()?.is_empty());
            assert_eq!(acquisitions.get(), 1);
        }
        Ok(())
    }
}

// A setup lock timeout occurs before Git preparation and before any guardian
// launch. Keep the same attempt and custody while waiting for another turn.
const SETUP_RETRY_SECONDS: u64 = 240;
fn retry_setup_lock<T>(
    continuation: bool,
    mut prepare: impl FnMut(provenant_core::WaitBudget) -> Result<T>,
    mut check: impl FnMut() -> Result<()>,
) -> Result<T> {
    use provenant_core::Clock;
    let clock = provenant_os::clock::SystemClock::new();
    let budget = provenant_core::WaitBudget::cli(clock.mono_now(), SETUP_RETRY_SECONDS)
        .map_err(|_| RunError::Validation("setup retry deadline"))?;
    loop {
        match prepare(budget) {
            Err(RunError::LaneSetupLockTimeout)
                if continuation && budget.remaining(clock.mono_now()).as_micros() > 0 =>
            {
                check()?
            }
            result => return result,
        }
    }
}

#[cfg(test)]
mod retry_tests {
    use super::*;

    #[test]
    fn continuation_retries_only_setup_lock_timeouts() {
        let mut calls = 0;
        let result = retry_setup_lock(
            true,
            |_| {
                calls += 1;
                if calls == 1 {
                    Err(RunError::LaneSetupLockTimeout)
                } else {
                    Ok(42)
                }
            },
            || Ok(()),
        );
        assert_eq!(result.unwrap(), 42);
        assert_eq!(calls, 2);
        let mut calls = 0;
        let result: Result<()> = retry_setup_lock(
            false,
            |_| {
                calls += 1;
                Err(RunError::LaneSetupLockTimeout)
            },
            || Ok(()),
        );
        assert!(matches!(result, Err(RunError::LaneSetupLockTimeout)));
        assert_eq!(calls, 1);
        let result: Result<()> = retry_setup_lock(true, |_| Err(RunError::Conflict), || Ok(()));
        assert!(matches!(result, Err(RunError::Conflict)));
    }
}
