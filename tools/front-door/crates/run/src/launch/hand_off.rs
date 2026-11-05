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
            None => acquire_writer(directory()?, scope, custody::WriterBinding::read_only()),
        }
        .map_err(|e| match e {
            custody::CustodyError::Busy => HandOff::Busy,
            custody::CustodyError::ImportRetained { run, attempt } => HandOff::RetainedImport {
                run,
                attempt,
                worktree: writer_path.clone().unwrap_or_default(),
            },
            error => local("custody", &error),
        })?;
        if record.retry_of.is_some()
            && let Some(writer) = plan
                .confinement
                .as_ref()
                .and_then(|plan| plan.writer.as_ref())
            && let Err(error) = self.check_writer_lineage(record, &writer.worktree, call)
        {
            lease
                .withdraw_unlaunched()
                .map_err(|error| HandOff::Uncertain(error.to_string()))?;
            return Err(local("writer lineage", &error));
        }
        // Reconcile may have withdrawn this attempt before the lease existed.
        match self.record(id, call) {
            Ok(now)
                if now.revision == record.revision
                    && now.run_id == record.run_id
                    && now.task_id == record.task_id => {}
            Ok(_) => {
                lease
                    .withdraw_unlaunched()
                    .map_err(|e| HandOff::Uncertain(e.to_string()))?;
                return Err(local("attempt", &"changed before handoff"));
            }
            Err(error) => {
                lease
                    .withdraw_unlaunched()
                    .map_err(|e| HandOff::Uncertain(e.to_string()))?;
                return Err(local("attempt record", &error));
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
                    lease
                        .withdraw_unlaunched()
                        .map_err(|e| HandOff::Uncertain(e.to_string()))?;
                    return Err(local("delivery", &error));
                }
            }
        }
        // Keep the shared config lock until custody confirms the child launch.
        let mut _git_setup_lock = None;
        if let Some(confinement) = &mut plan.confinement {
            let writer_attempt = directory()?;
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
                    lease
                        .withdraw_unlaunched()
                        .map_err(|e| HandOff::Uncertain(e.to_string()))?;
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
            let seed_attempt = directory()?;
            seed_without_setup_lock(&mut _git_setup_lock, || {
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
                    lease
                        .withdraw_unlaunched()
                        .map_err(|e| HandOff::Uncertain(e.to_string()))?;
                    return Err(match error {
                        RunError::SetupLockTimeout { seconds } => {
                            HandOff::SetupLockTimeout(seconds)
                        }
                        error => local("Git setup", &error),
                    });
                }
            };
            let writer_attempt = directory()?;
            if let Some(writer) = &confinement.writer
                && let Err(error) = crate::objects::prepare_bound(
                    &provenant_os::git::git_executable()
                        .ok_or_else(|| local("Git", &RunError::GitUnavailable))?,
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
                lease
                    .withdraw_unlaunched()
                    .map_err(|e| HandOff::Uncertain(e.to_string()))?;
                return Err(local("lane Git objects", &error));
            }
            match confinement.command(&plan.argv, &id, call.cancel) {
                Ok(argv) => plan.argv = argv,
                Err(error) => {
                    lease
                        .withdraw_unlaunched()
                        .map_err(|e| HandOff::Uncertain(e.to_string()))?;
                    return Err(local("confinement command", &error));
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
        let lane_ports = match plan
            .confinement
            .as_ref()
            .filter(|c| c.writer.is_some())
            .map(|c| lane_port_plan(&c.state_root))
            .transpose()
        {
            Ok(plan) => plan.flatten(),
            Err(error) => {
                lease
                    .withdraw_unlaunched()
                    .map_err(|e| HandOff::Uncertain(e.to_string()))?;
                return Err(local("lane ports", &error));
            }
        };
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
            priority: record
                .requested_priority
                .ok_or_else(|| local("lane priority", &"missing"))?,
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
mod tests {
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
