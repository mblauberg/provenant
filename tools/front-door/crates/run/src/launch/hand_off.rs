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
        .map_err(|e| {
            if matches!(e, custody::CustodyError::Busy) {
                HandOff::Busy
            } else {
                local("custody", &e)
            }
        })?;
        // Once custody is acquired, every local preparation failure must
        // withdraw it before fallback can consider another candidate.
        macro_rules! local_or_withdraw {
            ($result:expr) => {
                match $result {
                    Ok(value) => value,
                    Err(error) => {
                        lease
                            .withdraw_unlaunched()
                            .map_err(|e| HandOff::Uncertain(e.to_string()))?;
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
            lease
                .withdraw_unlaunched()
                .map_err(|error| HandOff::Uncertain(error.to_string()))?;
            return Err(local("writer lineage", &error));
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
            _git_setup_lock = match confinement.prepare_git() {
                Ok(lock) => lock,
                Err(error) => {
                    lease
                        .withdraw_unlaunched()
                        .map_err(|e| HandOff::Uncertain(e.to_string()))?;
                    return Err(local("Git setup", &error));
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
                lease
                    .withdraw_unlaunched()
                    .map_err(|e| HandOff::Uncertain(e.to_string()))?;
                return Err(local("lane Git objects", &error));
            }
            match confinement.command(
                &plan.argv,
                &plan.environment,
                &id,
                call.cancel,
                &local_or_withdraw!(directory()),
            ) {
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
        let plan = PreparedLaunch {
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
            plan,
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
            Err(HandOff::Uncertain(reason)) => panic!("uncertain: {reason}"),
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
        Ok(())
    }
}
