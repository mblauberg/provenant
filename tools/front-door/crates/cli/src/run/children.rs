//! The parent guardian's host-side consumer of lane child submissions.
use super::*;
use provenant_os::fs::Directory;
use provenant_supervisor::{Holder, custody, machine::StopReason};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

type Stop = Arc<Mutex<Option<StopReason>>>;
struct Service {
    cancel: CancelToken,
    thread: JoinHandle<Result<(), String>>,
    reason: Stop,
}
/// Wrap Run's holder without moving provider launch into Fabric or supervisor.
/// The worker owns its SQLite connection; the guardian keeps draining output
/// and observing cancellation while child provider preparation runs.
pub struct HostHolder<'a> {
    inner: &'a dyn Holder,
    fabric_relay: Mutex<Option<provenant_fabric_mcp::lane::HostRelay>>,
    service: Mutex<Option<Service>>,
    settled: Mutex<Option<run::HolderContext>>,
}
impl<'a> HostHolder<'a> {
    pub fn new(inner: &'a dyn Holder) -> Self {
        Self {
            inner,
            fabric_relay: Mutex::new(None),
            service: Mutex::new(None),
            settled: Mutex::new(None),
        }
    }
    /// Available after supervise returns, when custody locks have been released.
    pub fn settled(&self) -> custody::Result<Option<run::HolderContext>> {
        Ok(self
            .settled
            .lock()
            .map_err(|_| custody::CustodyError::InvalidRecord)?
            .take())
    }
    fn stop(&self, reason: Option<StopReason>) -> custody::Result<()> {
        drop(
            self.fabric_relay
                .lock()
                .map_err(|_| custody::CustodyError::InvalidRecord)?
                .take(),
        );
        let service = self
            .service
            .lock()
            .map_err(|_| custody::CustodyError::InvalidRecord)?
            .take();
        if let Some(service) = service {
            *service
                .reason
                .lock()
                .map_err(|_| custody::CustodyError::InvalidRecord)? = reason;
            service.cancel.cancel();
            service.thread.thread().unpark();
            service
                .thread
                .join()
                .map_err(|_| custody::CustodyError::InvalidRecord)?
                .map_err(|error| custody::CustodyError::Io(std::io::Error::other(error)))?;
        }
        Ok(())
    }
}
impl Drop for HostHolder<'_> {
    fn drop(&mut self) {
        // The normal exit hook has already joined. This also stops the worker
        // when supervisor returns an error, without signalling any provider.
        let _ = self.stop(None);
    }
}
impl provenant_supervisor::NestedChildStore for HostHolder<'_> {
    /// The lane's MCP creates its child store at startup, so the store existing
    /// proves nothing. No nested lane can exist while the store holds no `runs`
    /// entry and its database has no run row. Any failed read is not "none".
    fn has_no_nested_runs(&self, raw: &[u8]) -> bool {
        let Ok(context) = serde_json::from_slice::<run::HolderContext>(raw) else {
            return false;
        };
        let state = context
            .roots
            .state_root
            .join("children")
            .join(context.attempt_id.to_string());
        match std::fs::read_dir(state.join("runs")) {
            Ok(mut entries) => {
                if entries.next().is_some() {
                    return false;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return false,
        }
        let mut inputs = provenant_core::RootInputs::new(
            context.roots.home.clone(),
            context.roots.checkout.clone(),
        );
        inputs.state_dir = Some(state);
        let Ok(roots) = inputs.resolve(&provenant_os::fs::SystemPathResolver) else {
            return false;
        };
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = run::Request {
            clock: &clock,
            cancel: &cancel,
        };
        let execution = run::ExecutionContext {
            state_root: roots.state,
            project_id: context.project_id,
            actor_id: context.actor_id,
            read_prompts: false,
        };
        match run::RunStore::open_read_only(&execution, &call) {
            Ok(None) => true,
            Ok(Some(store)) => store.has_runs(&call).is_ok_and(|any| !any),
            Err(_) => false,
        }
    }
}
impl Holder for HostHolder<'_> {
    fn admit(&self, raw: &[u8], attempt: &Directory) -> custody::Result<()> {
        self.inner.admit(raw, attempt)?;
        let context: run::HolderContext =
            serde_json::from_slice(raw).map_err(|_| custody::CustodyError::InvalidRecord)?;
        if context.launch.is_none() {
            return Ok(());
        }
        *self
            .fabric_relay
            .lock()
            .map_err(|_| custody::CustodyError::InvalidRecord)? = Some(
            provider::start_fabric_relay(&context)
                .map_err(|error| custody::CustodyError::Io(std::io::Error::other(error)))?,
        );
        let cancel = CancelToken::new();
        let worker_cancel = cancel.clone();
        let reason = Arc::new(Mutex::new(None));
        let worker_reason = Arc::clone(&reason);
        let thread = std::thread::Builder::new()
            .name("child-launch".into())
            .spawn(move || serve(context, worker_cancel, worker_reason))?;
        *self
            .service
            .lock()
            .map_err(|_| custody::CustodyError::InvalidRecord)? = Some(Service {
            cancel,
            thread,
            reason,
        });
        Ok(())
    }
    fn provider_exited(&self, context: &[u8], reason: Option<StopReason>) -> custody::Result<()> {
        self.inner.provider_exited(context, reason)?;
        self.stop(reason)
    }
    fn terminal(
        &self,
        context: &[u8],
        attempt: &Directory,
        exit: &custody::ExitObservation,
        recovery: &custody::Recovery,
    ) -> custody::Result<()> {
        self.stop(exit.stop_reason)?;
        self.inner.terminal(context, attempt, exit, recovery)?;
        let context =
            serde_json::from_slice(context).map_err(|_| custody::CustodyError::InvalidRecord)?;
        *self
            .settled
            .lock()
            .map_err(|_| custody::CustodyError::InvalidRecord)? = Some(context);
        Ok(())
    }
}

// Child launcher polling is a real-time pause between bounded store scans.
#[allow(clippy::disallowed_methods)]
fn serve(context: run::HolderContext, cancel: CancelToken, reason: Stop) -> Result<(), String> {
    let launch = context
        .launch
        .as_ref()
        .ok_or("host launch context missing")?;
    let clock = SystemClock::new();
    let setup_cancel = CancelToken::new();
    let call = request(&clock, &setup_cancel).map_err(|error| error.message)?;
    let (_, parent) =
        run::HolderContext::open(&context.encode().map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    // Children are bounded by this attempt's own sealed task authority. The
    // store may be a parent lane's, so trust its request only while it still
    // matches the digest the host launched this attempt from.
    let sealed = context.sealed.as_ref().ok_or("host sealed task missing")?;
    let parent_request = parent
        .request(context.run_id, &call)
        .map_err(|error| error.to_string())?;
    if parent_request.digest().map_err(|error| error.to_string())? != sealed.request_digest {
        return Err("parent request changed after launch".into());
    }
    let parent_depth = parent_request.lane_depth.unwrap_or(1);
    let mut parent_task = parent_request
        .tasks
        .iter()
        .find(|task| task.task_id == sealed.task_id)
        .ok_or("parent task missing from its request")?
        .clone();
    sealed.apply_network(&mut parent_task);
    let mut inputs =
        provenant_core::RootInputs::new(context.roots.home.clone(), context.roots.checkout.clone());
    inputs.state_dir = Some(launch.configuration_root.clone());
    let roots = inputs
        .resolve(&provenant_os::fs::SystemPathResolver)
        .map_err(|error| error.to_string())?;
    inputs.state_dir = Some(
        context
            .roots
            .state_root
            .join("children")
            .join(context.attempt_id.to_string()),
    );
    let child_state = inputs
        .resolve(&provenant_os::fs::SystemPathResolver)
        .map_err(|error| error.to_string())?
        .state;
    let session = Session {
        store: run::RunStore::open(
            &run::ExecutionContext {
                state_root: child_state.clone(),
                project_id: context.project_id,
                actor_id: context.actor_id,
                read_prompts: true,
            },
            &call,
        )
        .map_err(|error| error.to_string())?
        .with_writer_registry(roots.state.as_path())
        // Launch, ownership and cancellation of children use the requester
        // sealed in this attempt's host-built context, never the lane's actor.
        .with_sealed_requester(context.actor_id)
        .with_sealed_parent_network(parent_task.access.network(parent_task.writer.is_some())),
        context: run::ExecutionContext {
            state_root: child_state,
            project_id: context.project_id,
            actor_id: context.actor_id,
            read_prompts: true,
        },
    };
    let edge = Edge {
        roots: &roots,
        home: &context.roots.home,
        assets: &context.roots.checkout,
        cwd: &launch.cwd,
    };
    let mut accepted = std::collections::BTreeSet::new();
    let mut launch_error = None;
    loop {
        // A final bounded scan drains submissions made before provider exit.
        // A normal parent completion leaves accepted children independent;
        // explicit parent stops request cancellation through their own custody.
        let stopping = cancel.is_cancelled();
        let scan_cancel = CancelToken::new();
        let call = request(&clock, &scan_cancel).map_err(|error| error.message)?;
        let config = provenant_config::read_with_domains(&roots, &crate::domains::SECTIONS)
            .map_err(|error| error.to_string())?;
        let maximum = config
            .knobs
            .get("lanes.max_depth")
            .and_then(|knob| knob.value.as_u64())
            .ok_or("lanes.max_depth missing")?;
        // The lane can write every record this scan reads, so a read
        // failure is reported, never fatal: the stop below must still run.
        let queued = session.store.queued_runs(&call).unwrap_or_else(|error| {
            launch_error = Some(error.to_string());
            Vec::new()
        });
        let scan = scan_queued(
            &session.store,
            queued,
            &ChildAuthority {
                parent: context.attempt_id,
                task: &parent_task,
                depth: parent_depth,
                maximum,
            },
            &reason,
            &call,
            |id, validated| {
                inherit_child_chair(
                    &session,
                    &roots,
                    (context.run_id, sealed.request_digest),
                    id,
                    validated,
                    &call,
                )?;
                launch_on_host(
                    &session,
                    id,
                    &edge,
                    &call,
                    Some(launch),
                    Some(validated),
                    None,
                )
            },
        )?;
        accepted.extend(scan.accepted);
        if let Some(error) = scan.error {
            launch_error = Some(error);
        }
        if stopping {
            let stop_reason = *reason
                .lock()
                .map_err(|_| "child launcher stop state poisoned")?;
            for error in finish_children(
                &session.store,
                &session.context,
                accepted,
                stop_reason,
                &call,
            ) {
                launch_error = Some(error);
            }
            return match launch_error {
                Some(error) => Err(error),
                None => Ok(()),
            };
        }
        std::thread::park_timeout(Duration::from_millis(100));
    }
}

fn inherit_child_chair(
    session: &Session,
    roots: &Roots,
    parent: (RunId, provenant_core::Digest),
    run: RunId,
    validated: &run::CanonicalRequest,
    call: &run::Request<'_>,
) -> Result<(), Fail> {
    check_chair_run_id(session, roots, run, validated, call)?;
    let operation = session.store.run(run, call)?.operation_id;
    with_chair_index(roots, call, |index, index_call| {
        index.inherit(
            session.context.project_id,
            parent,
            run,
            provenant_fabric_runtime::chairs::Admission::Lane(&operation),
            validated,
            index_call,
        )
    })
}

struct ChildAuthority<'a> {
    parent: AttemptId,
    task: &'a run::TaskRequest,
    depth: u64,
    maximum: u64,
}

#[derive(Default)]
struct ChildScan {
    accepted: std::collections::BTreeSet<(AttemptId, u64)>,
    error: Option<String>,
}

fn scan_queued(
    store: &run::RunStore,
    queued: Vec<RunId>,
    authority: &ChildAuthority<'_>,
    reason: &Stop,
    call: &run::Request<'_>,
    mut launch: impl FnMut(RunId, &run::ValidatedChild) -> Result<Vec<run::Started>, Fail>,
) -> Result<ChildScan, String> {
    let mut scan = ChildScan::default();
    for id in queued {
        let stop_reason = *reason
            .lock()
            .map_err(|_| "child launcher stop state poisoned")?;
        if matches!(
            stop_reason,
            Some(
                StopReason::ExplicitCancellation
                    | StopReason::WallDeadline
                    | StopReason::SupervisorSignal
            )
        ) {
            if let Err(error) = store.refuse_queued(id, "parent stopped before child launch", call)
            {
                scan.error = Some(error.to_string());
            }
            continue;
        }
        let validated = match store.validate_child(
            id,
            authority.parent,
            authority.task,
            authority.depth,
            authority.maximum,
            call,
        ) {
            Ok(validated) => validated,
            Err(error) => {
                if let Err(error) = store.refuse_queued(id, &error.to_string(), call) {
                    scan.error = Some(error.to_string());
                }
                continue;
            }
        };
        // Accept refs before handoff: start can launch an earlier task
        // and then fail on a later one. Keep all scopes for cancellation.
        let pending = match store.pending_attempts(id, call) {
            Ok(pending) => pending,
            Err(error) => {
                scan.error = Some(error.to_string());
                continue;
            }
        };
        for record in pending {
            let generation = if matches!(record.state, run::AttemptState::Queued { .. }) {
                record.generation.checked_add(1)
            } else {
                Some(record.generation)
            };
            match generation {
                Some(generation) => {
                    scan.accepted.insert((record.attempt_id, generation));
                }
                None => scan.error = Some("child custody generation exhausted".into()),
            }
        }
        match launch(id, &validated) {
            // Capacity is store-wide. Leave later runs untouched until the
            // next scan, including their request validation and preparation.
            Ok(started)
                if started
                    .iter()
                    .any(|s| matches!(s, run::Started::Deferred(_))) =>
            {
                break;
            }
            Ok(_) => {}
            Err(error) if error.status == Status::Refused => {
                if let Err(error) = store.refuse_queued(id, &error.message, call) {
                    scan.error = Some(error.to_string());
                }
            }
            Err(error) => scan.error = Some(error.message),
        }
    }
    Ok(scan)
}

/// Finish the last scan without leaving queued attempts for a worker that
/// has stopped. Normal completion preserves children already in custody.
fn finish_children(
    store: &run::RunStore,
    context: &run::ExecutionContext,
    accepted: std::collections::BTreeSet<(AttemptId, u64)>,
    reason: Option<StopReason>,
    call: &run::Request<'_>,
) -> Vec<String> {
    let mut errors = Vec::new();
    let refusal = if matches!(
        reason,
        Some(
            StopReason::ExplicitCancellation
                | StopReason::WallDeadline
                | StopReason::SupervisorSignal
        )
    ) {
        errors.extend(store.stop_children(accepted.clone(), "host parent stopped", call));
        for (id, generation) in accepted {
            let observation = (|| -> Result<bool, Box<dyn std::error::Error>> {
                let record = store.record(id, call)?;
                if matches!(record.state, run::AttemptState::Terminal { .. }) {
                    return Ok(true);
                }
                let attempt = Directory::open_anchor(
                    &context
                        .state_root
                        .as_path()
                        .join(format!("runs/{}/attempts/{id}", record.run_id)),
                )?;
                let handle = custody::reattach(
                    attempt,
                    custody::Scope {
                        run: record.run_id,
                        attempt: id,
                        generation,
                    },
                )?;
                Ok(matches!(
                    custody::observe(
                        &handle,
                        provenant_core::WaitBudget::nonblocking(call.clock.mono_now()),
                        call.cancel,
                        call.clock,
                    )?,
                    custody::Observation::Live | custody::Observation::Exited(_)
                ))
            })();
            if !matches!(observation, Ok(true)) {
                errors.push(format!(
                    "child {id}: stop requested but guardian is not observable"
                ));
            }
        }
        "parent stopped before child launch"
    } else {
        "parent finished before child launch"
    };
    let mut processed = std::collections::BTreeSet::new();
    loop {
        let queued = match store.queued_runs(call) {
            Ok(queued) => queued,
            Err(error) => {
                errors.push(error.to_string());
                return errors;
            }
        };
        if queued.is_empty() {
            return errors;
        }
        // Discovery is bounded to 256 non-terminal rows, including launched
        // children. Refusing a batch exposes queued runs beyond that window.
        let errors_before = errors.len();
        for id in queued {
            // Lane-writable rows must not make shutdown revisit an unchanged
            // run forever when their run IDs disagree with the index.
            if !processed.insert(id) {
                errors.push("child queue did not drain after parent completion".into());
                continue;
            }
            if let Err(error) = store.refuse_queued(id, refusal, call) {
                errors.push(error.to_string());
            }
        }
        if errors.len() != errors_before {
            return errors;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::{ActorId, Digest, OperationId, ProjectId, RootInputs, RunId, TaskId};
    use provenant_records::CanonicalV1;

    fn chair_fixture(
        base: &Path,
        call: &run::Request<'_>,
    ) -> Result<(Roots, Session, run::PreparedRun, RunId), Box<dyn std::error::Error>> {
        let (context, child) = child_fixture(base, &SystemClock::new())?;
        let base = std::fs::canonicalize(base)?;
        let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(base.join("host"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let session = Session {
            store: run::RunStore::open(&context, call)?
                .with_writer_registry(roots.state.as_path())
                .with_sealed_requester(context.actor_id),
            context,
        };
        let parent = RunId::from_bytes([77; 16]);
        let mut original = child.request.clone();
        original.parent_attempt = None;
        original.lane_depth = Some(1);
        original.chair = Some(run::ChairBinding {
            extra: Default::default(),
            session: None,
            host: provenant_core::HostId::from_bytes([78; 16]),
            actor: ActorId::from_bytes([79; 16]),
        });
        with_chair_index(&roots, call, |index, c| {
            index.submit(session.context.project_id, parent, &original, c)
        })
        .map_err(|f| f.message)?;
        Ok((roots, session, child, parent))
    }

    fn parent_digest(
        roots: &Roots,
        project: ProjectId,
        parent: RunId,
        call: &run::Request<'_>,
    ) -> Result<Digest, String> {
        with_chair_index(roots, call, |index, c| {
            Ok(index.read(project, parent, c)?.unwrap().request_digest)
        })
        .map_err(|f| f.message)
    }

    #[test]
    fn legacy_child_cannot_claim_a_chair_under_a_lane_chosen_run_id()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = request(&clock, &cancel).map_err(|f| f.message)?;
        let (roots, session, child, parent) = chair_fixture(temp.path(), &call)?;
        session.store.create(&child, &call)?;
        let digest = parent_digest(&roots, session.context.project_id, parent, &call)?;
        let mut refusal = None;
        let scan = scan_queued(
            &session.store,
            vec![child.run_id],
            &ChildAuthority {
                parent: child.request.parent_attempt.unwrap(),
                task: &child.request.tasks[0],
                depth: 1,
                maximum: 2,
            },
            &Arc::new(Mutex::new(None)),
            &call,
            |id, validated| {
                let result = check_chair_run_id(&session, &roots, id, validated, &call);
                if matches!(result, Err(ChairRunError::Unauthorised(_))) {
                    refusal = Some(());
                }
                result.map_err(Fail::from)?;
                inherit_child_chair(&session, &roots, (parent, digest), id, validated, &call)?;
                Ok(vec![])
            },
        )?;
        assert!(scan.error.is_none(), "{:?}", scan.error);
        with_chair_index(&roots, &call, |index, c| {
            assert!(
                index
                    .read(session.context.project_id, child.run_id, c)?
                    .is_none()
            );
            Ok(())
        })
        .map_err(|f| f.message)?;
        assert_eq!(refusal, Some(()));
        assert!(matches!(
            session
                .store
                .record(child.attempts[0].attempt_id, &call)?
                .state,
            run::AttemptState::Terminal {
                outcome: run::Outcome::Rejected,
                reason: Some(_),
                ..
            }
        ));
        // Retained legacy host rows remain readable and replayable. A different
        // request cannot shadow that row or repair it to another digest.
        with_chair_index(&roots, &call, |index, c| {
            index.inherit_retained(
                session.context.project_id,
                (parent, digest),
                child.run_id,
                &child.request,
                c,
            )
        })
        .map_err(|f| f.message)?;
        inherit_child_chair(
            &session,
            &roots,
            (parent, digest),
            child.run_id,
            &child.request,
            &call,
        )
        .map_err(|f| f.message)?;
        let mut forged = child.request.clone();
        forged.concurrency = 2;
        let failure = inherit_child_chair(
            &session,
            &roots,
            (parent, digest),
            child.run_id,
            &forged,
            &call,
        )
        .unwrap_err();
        assert_eq!(failure.kind, run::ErrorKind::Conflict);
        with_chair_index(&roots, &call, |index, c| {
            let binding = index
                .read(session.context.project_id, child.run_id, c)?
                .unwrap();
            assert_eq!(binding.request_digest, child.request.digest()?);
            assert_eq!(
                binding.chair,
                index
                    .read(session.context.project_id, parent, c)?
                    .unwrap()
                    .chair
            );
            Ok(())
        })
        .map_err(|f| f.message)?;
        Ok(())
    }

    fn queue_versioned_child(
        session: &Session,
        child: &run::PreparedRun,
        cwd: &Path,
        call: &run::Request<'_>,
    ) -> Result<run::RunRecord, run::RunError> {
        let task = &child.request.tasks[0];
        session.service().dispatch(
            run::DispatchRequest {
                chair: None,
                operation_id: child.operation_id.clone(),
                concurrency: 1,
                lane_depth: 2,
                parent_attempt: child.request.parent_attempt,
                tasks: vec![run::DispatchTask {
                    context: None,
                    build_heavy: false,
                    title: None,
                    task_id: task.task_id,
                    prompt: "child".into(),
                    base: None,
                    pinned_base: None,
                    head: None,
                    route: task.route.clone(),
                    account: None,
                    fallback: vec![],
                    profile: task.profile.clone(),
                    timeout_seconds: 3600,
                    delivery_id: None,
                    deliver: None,
                    worktree: None,
                    access: Default::default(),
                }],
            },
            cwd,
            call,
        )
    }

    #[test]
    fn launch_seal_records_root_network_and_legacy_defaults_restrictively()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_confinement::permissions::NetworkAccess;
        let temp = tempfile::tempdir()?;
        let (_, child) = child_fixture(temp.path(), &SystemClock::new())?;
        let mut task = child.request.tasks[0].clone();
        assert_eq!(
            seal_launch_task(child.request.digest()?, &task).effective_network,
            NetworkAccess::Open
        );
        for network in [
            NetworkAccess::Provider,
            NetworkAccess::Local,
            NetworkAccess::Open,
        ] {
            task.access.network = Some(network);
            let sealed = seal_launch_task(child.request.digest()?, &task);
            assert_eq!(sealed.effective_network, network);
        }
        let mut json = serde_json::to_value(seal_launch_task(child.request.digest()?, &task))?;
        json.as_object_mut()
            .ok_or("sealed task")?
            .remove("effective_network");
        let old: run::SealedTask = serde_json::from_value(json.clone())?;
        assert_eq!(old.effective_network, NetworkAccess::Provider);
        json["effective_network"] = serde_json::Value::Null;
        assert!(serde_json::from_value::<run::SealedTask>(json).is_err());
        Ok(())
    }

    #[test]
    fn inherited_network_survives_holder_context_for_grandchildren()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_confinement::permissions::NetworkAccess;
        let temp = tempfile::tempdir()?;
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = request(&clock, &cancel).map_err(|f| f.message)?;
        let (context, mut child) = child_fixture(temp.path(), &clock)?;
        let store = run::RunStore::open(&context, &call)?;
        store.create(&child, &call)?;
        let mut parent = child.request.tasks[0].clone();
        parent.access.network = Some(NetworkAccess::Provider);
        let validated = store.validate_child(
            child.run_id,
            child.request.parent_attempt.unwrap(),
            &parent,
            1,
            3,
            &call,
        )?;
        let sealed = seal_launch_task(
            validated.digest()?,
            &store.effective_launch_task(&validated, &validated.tasks[0], Some(&validated)),
        );
        let sealed: run::SealedTask = serde_json::from_slice(&serde_json::to_vec(&sealed)?)?;
        let mut next_parent = validated.tasks[0].clone();
        sealed.apply_network(&mut next_parent);
        assert_eq!(sealed.effective_network, NetworkAccess::Provider);
        child.run_id = RunId::from_bytes([11; 16]);
        child.operation_id = OperationId::parse("grandchild")?;
        child.request.parent_attempt = Some(child.attempts[0].attempt_id);
        child.request.lane_depth = Some(3);
        child.attempts[0].attempt_id = AttemptId::from_bytes([12; 16]);
        store.create(&child, &call)?;
        let grandchild = store.validate_child(
            child.run_id,
            child.request.parent_attempt.unwrap(),
            &next_parent,
            2,
            3,
            &call,
        )?;
        assert_eq!(
            grandchild
                .effective_task(&grandchild.tasks[0])
                .access
                .network(false),
            NetworkAccess::Provider
        );
        Ok(())
    }

    #[test]
    fn restricted_parent_launches_child_with_inherited_chair()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_confinement::permissions::NetworkAccess;
        use std::cell::Cell;
        struct Observe(Cell<Option<NetworkAccess>>);
        impl run::Providers for Observe {
            fn prepare(
                &self,
                task: &run::TaskRequest,
                _: &str,
                _: &run::LaunchPaths,
                _: Option<&run::AttemptRecord>,
            ) -> Result<run::PreparedLaunch, String> {
                self.0.set(task.access.network);
                Ok(run::PreparedLaunch {
                    argv: vec!["fixture".into()],
                    environment: vec![],
                    stdin: vec![],
                    secret_input: None,
                    cwd: "/".into(),
                    route: run::RouteRecord::default(),
                    skipped_routes: vec![],
                    observed_head: None,
                    repository: None,
                    confinement: None,
                })
            }
            fn decode(
                &self,
                _: &run::RouteRecord,
                _: &[u8],
                _: &custody::ExitObservation,
                _: &Path,
            ) -> Result<run::Decoded, String> {
                unreachable!("fixture stops at handoff")
            }
        }
        let temp = tempfile::tempdir()?;
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = request(&clock, &cancel).map_err(|f| f.message)?;
        let (roots, session, child, parent) = chair_fixture(temp.path(), &call)?;
        let admitted = queue_versioned_child(
            &session,
            &child,
            &std::fs::canonicalize(temp.path())?.join("checkout"),
            &call,
        )?;
        let original = session.store.request(admitted.run_id, &call)?;
        let mut parent_task = child.request.tasks[0].clone();
        parent_task.access.network = Some(NetworkAccess::Provider);
        let digest = parent_digest(&roots, session.context.project_id, parent, &call)?;
        let provider = Observe(Cell::new(None));
        let handed_off = Cell::new(false);
        let scan = scan_queued(
            &session.store,
            vec![admitted.run_id],
            &ChildAuthority {
                parent: child.request.parent_attempt.unwrap(),
                task: &parent_task,
                depth: 1,
                maximum: 2,
            },
            &Arc::new(Mutex::new(None)),
            &call,
            |id, validated| {
                inherit_child_chair(&session, &roots, (parent, digest), id, validated, &call)?;
                session
                    .store
                    .start_child(
                        id,
                        validated,
                        &provider,
                        &|_, _| {
                            handed_off.set(true);
                            Err(run::RunError::Validation("fixture observed handoff"))
                        },
                        &call,
                    )
                    .map_err(Fail::from)
            },
        )?;
        assert!(scan.error.is_none(), "{:?}", scan.error);
        assert!(handed_off.get());
        assert_eq!(provider.0.get(), Some(NetworkAccess::Provider));
        with_chair_index(&roots, &call, |index, c| {
            let binding = index
                .read(session.context.project_id, admitted.run_id, c)?
                .unwrap();
            assert_eq!(binding.request_digest, original.digest()?);
            assert_eq!(
                binding.chair,
                index
                    .read(session.context.project_id, parent, c)?
                    .unwrap()
                    .chair
            );
            Ok(())
        })
        .map_err(|f| f.message)?;
        Ok(())
    }

    #[test]
    fn child_chair_seals_the_validated_request_despite_a_lane_rewrite()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = request(&clock, &cancel).map_err(|f| f.message)?;
        let (roots, session, child, parent) = chair_fixture(temp.path(), &call)?;
        let task = child.request.tasks[0].clone();
        let admitted = queue_versioned_child(
            &session,
            &child,
            &std::fs::canonicalize(temp.path())?.join("checkout"),
            &call,
        )?;
        let validated = session.store.validate_child(
            admitted.run_id,
            child.request.parent_attempt.unwrap(),
            &task,
            1,
            2,
            &call,
        )?;
        // The lane replaces both the bytes and their lane-local digest after
        // validation, before the host writes the association.
        let mut forged = (*validated).clone();
        forged.child_attempt_id_version = None;
        forged.concurrency = 2;
        std::fs::write(
            session
                .context
                .state_root
                .as_path()
                .join(format!("runs/{}/request.json", admitted.run_id)),
            serde_json::to_vec(&forged)?,
        )?;
        let mut record = admitted.clone();
        record.request_digest = forged.digest()?;
        let db =
            rusqlite::Connection::open(session.context.state_root.as_path().join("run.sqlite3"))?;
        db.execute(
            "UPDATE runs SET record=?1 WHERE id=?2",
            rusqlite::params![serde_json::to_vec(&record)?, admitted.run_id.to_string()],
        )?;
        assert_eq!(session.store.request(admitted.run_id, &call)?, forged);
        let digest = parent_digest(&roots, session.context.project_id, parent, &call)?;
        inherit_child_chair(
            &session,
            &roots,
            (parent, digest),
            admitted.run_id,
            &validated,
            &call,
        )
        .map_err(|f| f.message)?;
        with_chair_index(&roots, &call, |index, c| {
            assert_eq!(
                index
                    .read(session.context.project_id, admitted.run_id, c)?
                    .unwrap()
                    .request_digest,
                validated.digest()?
            );
            Ok(())
        })
        .map_err(|f| f.message)?;
        Ok(())
    }

    #[test]
    fn continuation_cannot_redirect_its_source_to_another_runs_chair()
    -> Result<(), Box<dyn std::error::Error>> {
        for versioned in [false, true] {
            let temp = tempfile::tempdir()?;
            let base = std::fs::canonicalize(temp.path())?;
            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let call = request(&clock, &cancel).map_err(|f| f.message)?;
            let (roots, session, child, parent) = chair_fixture(temp.path(), &call)?;
            let source = if versioned {
                queue_versioned_child(&session, &child, &base.join("checkout"), &call)?
            } else {
                session.store.create(&child, &call)?
            };
            let canonical = session.store.request(source.run_id, &call)?;
            let digest = parent_digest(&roots, session.context.project_id, parent, &call)?;
            with_chair_index(&roots, &call, |index, c| {
                index.inherit_retained(
                    session.context.project_id,
                    (parent, digest),
                    source.run_id,
                    &canonical,
                    c,
                )
            })
            .map_err(|f| f.message)?;
            session
                .store
                .refuse_queued(source.run_id, "completed source", &call)?;
            let mut prior = session
                .store
                .lanes(
                    &run::LaneQuery {
                        targets: vec![],
                        all: true,
                        limit: 1,
                        after: 0,
                        newest_first: false,
                        owner: None,
                        unseen_after: None,
                    },
                    &call,
                )?
                .lanes
                .remove(0);
            let old = prior.attempt_id;
            if versioned {
                let honest = session.store.resume_fresh(
                    &run::Resume {
                        prior: old,
                        expected_generation: 0,
                        operation_id: "honest-continuation".parse()?,
                        run_id: RunId::from_bytes([95; 16]),
                        attempt_id: AttemptId::from_bytes([96; 16]),
                        prompt: "answer".into(),
                        created_at: clock.wall_now()?,
                    },
                    "fixture",
                    &call,
                )?;
                let home = base.join("home");
                let cwd = base.join("checkout");
                let edge = Edge {
                    roots: &roots,
                    home: &home,
                    assets: &cwd,
                    cwd: &cwd,
                };
                launch_on_host(&session, honest.run_id, &edge, &call, None, None, None)
                    .map_err(|f| f.message)?;
                with_chair_index(&roots, &call, |index, c| {
                    assert_eq!(
                        index
                            .read(session.context.project_id, honest.run_id, c)?
                            .unwrap()
                            .chair,
                        index
                            .read(session.context.project_id, source.run_id, c)?
                            .unwrap()
                            .chair
                    );
                    Ok(())
                })
                .map_err(|f| f.message)?;
            }
            // The lane substitutes the selected attempt while copying another
            // run's canonical request, operation and valid run namespace.
            prior.attempt_id = AttemptId::from_bytes([92; 16]);
            prior.prompt.path = provenant_records::PathWire::from_relative(
                &provenant_core::RelativePath::parse(format!(
                    "attempts/{}/{}",
                    prior.attempt_id,
                    provenant_records::AttemptFile::Prompt.name()
                ))?,
            )?;
            if let Some(result) = &mut prior.result {
                result.path = provenant_records::PathWire::from_relative(
                    &provenant_core::RelativePath::parse(format!(
                        "attempts/{}/result.md",
                        prior.attempt_id
                    ))?,
                )?;
            }
            let db = rusqlite::Connection::open(
                session.context.state_root.as_path().join("run.sqlite3"),
            )?;
            db.execute_batch("PRAGMA foreign_keys=OFF; DROP TRIGGER immutable_terminal_update")?;
            db.execute(
                "UPDATE attempts SET id=?1,record=?2 WHERE id=?3",
                rusqlite::params![
                    prior.attempt_id.to_string(),
                    serde_json::to_vec(&prior)?,
                    old.to_string()
                ],
            )?;
            let continuation = session.store.resume_fresh(
                &run::Resume {
                    prior: prior.attempt_id,
                    expected_generation: 0,
                    operation_id: "redirected-continuation".parse()?,
                    run_id: RunId::from_bytes([93; 16]),
                    attempt_id: AttemptId::from_bytes([94; 16]),
                    prompt: "answer".into(),
                    created_at: clock.wall_now()?,
                },
                "fixture",
                &call,
            )?;
            let home = base.join("home");
            let cwd = base.join("checkout");
            let edge = Edge {
                roots: &roots,
                home: &home,
                assets: &cwd,
                cwd: &cwd,
            };
            let result = launch_on_host(
                &session,
                continuation.run_id,
                &edge,
                &call,
                None,
                None,
                None,
            );
            with_chair_index(&roots, &call, |index, c| {
                assert!(
                    index
                        .read(session.context.project_id, continuation.run_id, c)?
                        .is_none(),
                    "versioned source: {versioned}"
                );
                Ok(())
            })
            .map_err(|f| f.message)?;
            let failure = result.unwrap_err();
            assert_eq!(failure.status, Status::Refused);
            assert!(matches!(
                check_chair_source(&session.store, &canonical, &prior),
                Err(ChairSourceError)
            ));
        }
        Ok(())
    }

    #[test]
    fn continuation_cannot_copy_a_chair_to_a_lane_chosen_replay_run()
    -> Result<(), Box<dyn std::error::Error>> {
        // Run the host entrypoint outside inherited lane submission mode,
        // without changing the process environment shared by other tests.
        if std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV).is_some() {
            use provenant_os::exec::{
                ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination,
            };
            let argv = vec![
                std::env::current_exe()?.into_os_string(),
                "--exact".into(),
                "run::children::tests::continuation_cannot_copy_a_chair_to_a_lane_chosen_replay_run".into(),
                "--nocapture".into(),
            ];
            let env: Vec<_> = std::env::vars_os()
                .filter(|(key, _)| key != provenant_core::RUN_STATE_DIRECTORY_ENV)
                .collect();
            let cwd = std::env::current_dir()?;
            let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(30));
            options.env = Some(&env);
            options.stdout = StreamPolicy::Capture { limit: 65536 };
            options.stderr = StreamPolicy::Capture { limit: 65536 };
            let out = SystemExecutor::new(1)?.execute(options)?;
            assert!(
                out.termination == Termination::Exit(0) && !out.timed_out,
                "{out:?}"
            );
            assert!(
                String::from_utf8_lossy(&out.stdout.bytes).contains("test result: ok. 1 passed;"),
                "host subprocess must execute the regression: {out:?}"
            );
            return Ok(());
        }
        let temp = tempfile::tempdir()?;
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = request(&clock, &cancel).map_err(|f| f.message)?;
        let (roots, session, mut child, parent) = chair_fixture(temp.path(), &call)?;
        session.store.create(&child, &call)?;
        session
            .store
            .refuse_queued(child.run_id, "completed legacy child", &call)?;
        let digest = parent_digest(&roots, session.context.project_id, parent, &call)?;
        // A retained legacy source association is trusted host state. The lane
        // can still forge a continuation operation in its own store.
        with_chair_index(&roots, &call, |index, c| {
            index.inherit_retained(
                session.context.project_id,
                (parent, digest),
                child.run_id,
                &child.request,
                c,
            )
        })
        .map_err(|f| f.message)?;
        let prior = child.attempts[0].attempt_id;
        child.request.parent = Some(prior);
        child.run_id = RunId::from_bytes([90; 16]);
        child.operation_id = OperationId::parse("forged-continuation-replay")?;
        child.attempts[0].attempt_id = AttemptId::from_bytes([91; 16]);
        session.store.create(&child, &call)?;
        // Defensively exercise a host continuation given a reopened lane
        // store, without the scanner's in-memory sealed requester.
        let session = Session {
            store: run::RunStore::open(&session.context, &call)?
                .with_writer_registry(roots.state.as_path()),
            context: session.context,
        };
        let base = std::fs::canonicalize(temp.path())?;
        let home = base.join("home");
        let cwd = base.join("checkout");
        let edge = Edge {
            roots: &roots,
            home: &home,
            assets: &cwd,
            cwd: &cwd,
        };
        // The host's early operation replay adopts the lane's stored run ID.
        // Authenticate it before copying any chair association.
        let result = continue_attempt(
            &session,
            &edge,
            prior,
            child.operation_id.clone(),
            child.attempts[0].prompt.clone(),
            0,
            &call,
        );
        with_chair_index(&roots, &call, |index, c| {
            assert!(
                index
                    .read(session.context.project_id, child.run_id, c)?
                    .is_none()
            );
            Ok(())
        })
        .map_err(|f| f.message)?;
        let failure = result.unwrap_err();
        assert_eq!(failure.status, Status::Refused);
        let child_request = session.store.request(child.run_id, &call)?;
        assert!(matches!(
            check_chair_run_id(&session, &roots, child.run_id, &child_request, &call),
            Err(ChairRunError::Unauthorised(_))
        ));
        assert_eq!(
            session
                .store
                .operation(&child.operation_id, &call)?
                .unwrap()
                .run_id,
            child.run_id,
            "replay must authenticate the stored ID rather than silently rekey it"
        );
        // A retained legacy destination seal replays without trusting the
        // lane-local source again. Old records remain readable as well.
        with_chair_index(&roots, &call, |index, c| {
            index.inherit_retained(
                session.context.project_id,
                (parent, digest),
                child.run_id,
                &child.request,
                c,
            )
        })
        .map_err(|f| f.message)?;
        let (replayed, _) = continue_attempt(
            &session,
            &edge,
            prior,
            child.operation_id.clone(),
            child.attempts[0].prompt.clone(),
            0,
            &call,
        )
        .map_err(|f| f.message)?;
        assert_eq!(replayed.run_id, child.run_id);
        assert_eq!(session.store.request(child.run_id, &call)?, child.request);
        with_chair_index(&roots, &call, |index, c| {
            let binding = index
                .read(session.context.project_id, child.run_id, c)?
                .unwrap();
            assert_eq!(binding.request_digest, child.request.digest()?);
            assert_eq!(
                binding.chair,
                index
                    .read(session.context.project_id, parent, c)?
                    .unwrap()
                    .chair
            );
            Ok(())
        })
        .map_err(|f| f.message)?;
        Ok(())
    }

    #[test]
    fn child_policy_refusal_settles_attempt_without_parent_launch_error()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = SystemClock::new();
        let (context, child) = child_fixture(temp.path(), &clock)?;
        let cancel = CancelToken::new();
        let call = request(&clock, &cancel).map_err(|error| error.message)?;
        let store = run::RunStore::open(&context, &call)?.with_sealed_requester(context.actor_id);
        store.create(&child, &call)?;
        let authority = ChildAuthority {
            parent: child.request.parent_attempt.ok_or("parent missing")?,
            task: &child.request.tasks[0],
            depth: 1,
            maximum: 2,
        };
        let refusal = "host resource policy refused the child";
        let scan = scan_queued(
            &store,
            vec![child.run_id],
            &authority,
            &std::sync::Arc::new(std::sync::Mutex::new(None)),
            &call,
            |_, _| Err(Fail::new(Status::Refused, run::ErrorKind::Refused, refusal)),
        )?;
        assert!(scan.error.is_none(), "{:?}", scan.error);
        let record = store.record(child.attempts[0].attempt_id, &call)?;
        assert!(
            matches!(&record.state, run::AttemptState::Terminal { outcome: run::Outcome::Rejected, reason: Some(reason), .. }
            if reason == refusal),
            "{:?}",
            record.state
        );
        assert!(record.applied_route.is_none());
        Ok(())
    }

    #[test]
    fn deferral_stops_the_scan_before_validating_the_next_run()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = SystemClock::new();
        let (context, mut child) = child_fixture(temp.path(), &clock)?;
        let cancel = CancelToken::new();
        let call = request(&clock, &cancel).map_err(|error| error.message)?;
        let store = run::RunStore::open(&context, &call)?.with_sealed_requester(context.actor_id);
        let parent_task = child.request.tasks[0].clone();
        let authority = ChildAuthority {
            parent: AttemptId::from_bytes([9; 16]),
            task: &parent_task,
            depth: 1,
            maximum: 2,
        };
        store.create(&child, &call)?;
        let first = child.run_id;
        let first_attempt = child.attempts[0].attempt_id;
        child.run_id = RunId::from_bytes([8; 16]);
        child.operation_id = OperationId::parse("invalid-later-child")?;
        child.attempts[0].attempt_id = AttemptId::from_bytes([7; 16]);
        // This later proposal would be refused if the scan validated it.
        child.request.lane_depth = Some(3);
        store.create(&child, &call)?;
        let second = child.run_id;
        assert!(
            store
                .validate_child(
                    second,
                    authority.parent,
                    authority.task,
                    authority.depth,
                    authority.maximum,
                    &call
                )
                .is_err()
        );
        let queued = store.queued_runs(&call)?;
        assert_eq!(queued, vec![first, second]);
        let mut launches = Vec::new();
        let scan = scan_queued(
            &store,
            queued,
            &authority,
            &Arc::new(Mutex::new(None)),
            &call,
            |id, _| {
                launches.push(id);
                // The run store reports this result when held custody is full.
                Ok(vec![run::Started::Deferred(first_attempt)])
            },
        )?;
        assert!(scan.error.is_none());
        assert_eq!(launches, vec![first]);
        assert_eq!(scan.accepted, [(first_attempt, 1)].into());
        for id in [first_attempt, child.attempts[0].attempt_id] {
            let record = store.record(id, &call)?;
            assert_eq!(
                record.state,
                run::AttemptState::Queued {
                    extra: Default::default()
                }
            );
            assert_eq!(record.generation, 0);
        }
        assert_eq!(store.queued_runs(&call)?, vec![first, second]);
        Ok(())
    }

    #[test]
    fn final_scan_refuses_deferred_children_after_normal_parent_completion()
    -> Result<(), Box<dyn std::error::Error>> {
        final_scan_drains_queue(None, false)
    }

    #[test]
    fn final_scan_drains_all_queued_children_after_parent_stop()
    -> Result<(), Box<dyn std::error::Error>> {
        for reason in [
            StopReason::ExplicitCancellation,
            StopReason::WallDeadline,
            StopReason::SupervisorSignal,
        ] {
            final_scan_drains_queue(Some(reason), false)?;
        }
        Ok(())
    }

    #[test]
    fn final_scan_drains_queue_even_when_an_accepted_child_cannot_be_stopped()
    -> Result<(), Box<dyn std::error::Error>> {
        final_scan_drains_queue(Some(StopReason::ExplicitCancellation), true)
    }

    fn final_scan_drains_queue(
        reason: Option<StopReason>,
        unobservable_child: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Prepare and admit without spawning a guardian or provider.
        struct Ready;
        impl run::Providers for Ready {
            fn prepare(
                &self,
                _: &run::TaskRequest,
                _: &str,
                _: &run::LaunchPaths,
                _: Option<&run::AttemptRecord>,
            ) -> Result<run::PreparedLaunch, String> {
                Ok(run::PreparedLaunch {
                    argv: vec!["fixture".into()],
                    environment: vec![],
                    stdin: vec![],
                    secret_input: None,
                    cwd: "/".into(),
                    route: run::RouteRecord::default(),
                    skipped_routes: vec![],
                    observed_head: None,
                    repository: None,
                    confinement: None,
                })
            }
            fn decode(
                &self,
                _: &run::RouteRecord,
                _: &[u8],
                _: &custody::ExitObservation,
                _: &Path,
            ) -> Result<run::Decoded, String> {
                unreachable!("no provider runs in this fixture")
            }
        }
        struct Available;
        impl run::Resources for Available {
            fn memory(&self) -> std::io::Result<provenant_os::sys::resources::MemorySample> {
                Ok(provenant_os::sys::resources::MemorySample {
                    total_bytes: 1,
                    available_bytes: 1,
                })
            }
            fn disk(&self, _: &Directory) -> std::io::Result<u64> {
                Ok(u64::MAX)
            }
        }
        let temp = tempfile::tempdir()?;
        let clock = SystemClock::new();
        let (context, mut child) = child_fixture(temp.path(), &clock)?;
        let cancel = CancelToken::new();
        let call = request(&clock, &cancel).map_err(|error| error.message)?;
        let mut store =
            run::RunStore::open(&context, &call)?.with_sealed_requester(context.actor_id);
        store.set_admission_policy(run::AdmissionPolicy {
            startup_spacing_ms: 0,
            read_only_spacing_ms: 0,
            ..Default::default()
        });
        let task_id = child.request.tasks[0].task_id;
        let running = child.attempts[0].attempt_id;
        // Two running rows keep the tail beyond the first discovery window
        // even after stop_children cancels one accepted queued attempt.
        for index in 0_u8..2 {
            child.run_id = RunId::from_bytes([index + 4; 16]);
            child.operation_id = OperationId::parse(&format!("running-child-{index}"))?;
            child.attempts[0].attempt_id = AttemptId::from_bytes([index + 6; 16]);
            store.create(&child, &call)?;
            let validated = store.validate_child(
                child.run_id,
                child.request.parent_attempt.unwrap(),
                &child.request.tasks[0],
                1,
                2,
                &call,
            )?;
            // Admit in the context callback, then stop handoff before custody.
            // The launcher's stale Starting withdrawal must leave Running intact.
            assert!(matches!(
                store.start_child(
                    child.run_id,
                    &validated,
                    &Ready,
                    &|record, _| {
                        let directory =
                            Directory::open_anchor(&context.state_root.as_path().join(format!(
                                "runs/{}/attempts/{}",
                                record.run_id, record.attempt_id
                            )))?;
                        store.admit(record.attempt_id, &directory, &Available, &clock, &cancel)?;
                        Err(run::RunError::Validation(
                            "fixture admitted without handoff",
                        ))
                    },
                    &call
                ),
                Err(run::RunError::Conflict)
            ));
            assert_eq!(
                store.record(child.attempts[0].attempt_id, &call)?.state,
                run::AttemptState::Running {
                    extra: Default::default()
                }
            );
        }
        // Admission permits 256 queued attempts in addition to active ones.
        // Put the last queued run beyond queued_runs' 256-row scan window.
        let template = child.request.tasks[0].clone();
        let mut deferred = Vec::new();
        for batch in 0_u8..4 {
            child.run_id = RunId::from_bytes([batch + 10; 16]);
            child.operation_id = OperationId::parse(&format!("queued-batch-{batch}"))?;
            child.request.tasks.clear();
            child.attempts.clear();
            let first = u128::from(batch) * 64 + 1;
            for index in first..(first + 64).min(256) {
                let task_id = TaskId::from_bytes(index.to_be_bytes());
                child.request.tasks.push(run::TaskRequest {
                    context: None,
                    title: None,
                    task_id,
                    ..template.clone()
                });
                child.attempts.push(run::NewAttempt {
                    task_id,
                    attempt_id: AttemptId::from_bytes(index.to_be_bytes()),
                    prompt: "child".into(),
                });
            }
            store.create(&child, &call)?;
            deferred.extend(child.attempts.iter().map(|attempt| attempt.attempt_id));
        }
        child.run_id = RunId::from_bytes([8; 16]);
        child.operation_id = OperationId::parse("queued-tail")?;
        child.request.tasks = vec![template];
        child.attempts = vec![run::NewAttempt {
            task_id,
            attempt_id: AttemptId::from_bytes([9; 16]),
            prompt: "child".into(),
        }];
        store.create(&child, &call)?;
        deferred.push(child.attempts[0].attempt_id);
        assert!(!store.queued_runs(&call)?.contains(&child.run_id));
        let stopped = reason.is_some().then(|| deferred.remove(0));
        let mut accepted = std::collections::BTreeSet::from([(stopped.unwrap_or(running), 1)]);
        if unobservable_child {
            // A durable stop request cannot prove a guardian observed it. Its
            // diagnostic must survive the drain without hiding later proposals.
            accepted.insert((running, 1));
        }
        let errors = finish_children(&store, &context, accepted, reason, &call);
        assert_eq!(errors.len(), usize::from(unobservable_child), "{errors:?}");
        if unobservable_child {
            assert_eq!(
                errors,
                vec![format!(
                    "child {running}: stop requested but guardian is not observable"
                )]
            );
            let cancellation: serde_json::Value = serde_json::from_slice(&std::fs::read(
                context.state_root.as_path().join(format!(
                    "runs/{}/attempts/{running}/cancel.json",
                    RunId::from_bytes([4; 16])
                )),
            )?)?;
            assert_eq!(
                cancellation["scope"],
                serde_json::to_value(custody::Scope {
                    run: RunId::from_bytes([4; 16]),
                    attempt: running,
                    generation: 1,
                })?
            );
        }
        if let Some(id) = stopped {
            assert!(matches!(
                store.record(id, &call)?.state,
                run::AttemptState::Terminal {
                    outcome: run::Outcome::Cancelled,
                    ..
                }
            ));
        }
        let refusal = if reason.is_some() {
            "parent stopped before child launch"
        } else {
            "parent finished before child launch"
        };
        for id in deferred {
            let record = store.record(id, &call)?;
            assert!(matches!(record.state, run::AttemptState::Terminal {
                outcome: run::Outcome::Rejected, reason: Some(reason), ..
            } if reason == refusal));
            assert_eq!(record.generation, 0);
        }
        assert_eq!(
            store.record(running, &call)?.state,
            run::AttemptState::Running {
                extra: Default::default()
            }
        );
        assert!(store.queued_runs(&call)?.is_empty());
        Ok(())
    }

    fn child_fixture(
        base: &Path,
        clock: &SystemClock,
    ) -> Result<(run::ExecutionContext, run::PreparedRun), Box<dyn std::error::Error>> {
        let base = std::fs::canonicalize(base)?;
        let home = base.join("home");
        let checkout = base.join("checkout");
        std::fs::create_dir_all(&home)?;
        std::fs::create_dir_all(&checkout)?;
        let mut inputs = RootInputs::new(home, checkout);
        inputs.state_dir = Some(base.join("state"));
        let context = run::ExecutionContext {
            state_root: inputs.resolve(&provenant_os::fs::SystemPathResolver)?.state,
            project_id: ProjectId::from_bytes([1; 16]),
            actor_id: ActorId::from_bytes([2; 16]),
            read_prompts: true,
        };
        let task_id = TaskId::from_bytes([3; 16]);
        let id = AttemptId::from_bytes([6; 16]);
        let child = run::PreparedRun {
            run_id: RunId::from_bytes([4; 16]),
            operation_id: OperationId::parse("deferred-child")?,
            created_at: clock.wall_now()?,
            request: run::CanonicalRequest {
                extra: Default::default(),
                child_attempt_id_version: None,
                chair: None,
                fresh_continuation: None,
                schema_version: 1,
                tasks: vec![run::TaskRequest {
                    context: None,
                    extra: Default::default(),
                    build_heavy: false,
                    title: None,
                    base: None,
                    head: None,
                    base_ref: None,
                    task_id,
                    prompt_digest: CanonicalV1::file_digest(b"child"),
                    route: run::RequestedRoute::Model {
                        extra: Default::default(),
                        provider: "fixture".into(),
                        model: "fixture/model".into(),
                        effort: None,
                    },
                    account: None,
                    fallback: vec![],
                    profile: Default::default(),
                    authority_digest: Digest::from_bytes([5; 32]),
                    provider_timeout_seconds: 3600,
                    writer: None,
                    delivery_id: None,
                    deliver: None,
                    access: Default::default(),
                }],
                concurrency: 1,
                parent: None,
                lane_depth: Some(2),
                parent_attempt: Some(AttemptId::from_bytes([9; 16])),
            },
            attempts: vec![run::NewAttempt {
                task_id,
                attempt_id: id,
                prompt: "child".into(),
            }],
        };
        Ok((context, child))
    }
}
