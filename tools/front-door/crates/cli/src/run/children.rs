//! The parent guardian's host-side consumer of lane child submissions.
use super::*;
use provenant_supervisor::{Holder, custody, fs::Directory, machine::StopReason};
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
    service: Mutex<Option<Service>>,
    settled: Mutex<Option<run::HolderContext>>,
}
impl<'a> HostHolder<'a> {
    pub fn new(inner: &'a dyn Holder) -> Self {
        Self {
            inner,
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
        let Ok(roots) = inputs.resolve(&provenant_supervisor::fs::LocalFs) else {
            return false;
        };
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let Ok(budget) = provenant_core::WaitBudget::cli(clock.mono_now(), 1) else {
            return false;
        };
        let call = run::Request {
            clock: &clock,
            budget,
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
    fn provider_exited(&self, _context: &[u8], reason: Option<StopReason>) -> custody::Result<()> {
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
    let parent_task = parent_request
        .tasks
        .iter()
        .find(|task| task.task_id == sealed.task_id)
        .ok_or("parent task missing from its request")?;
    let mut inputs =
        provenant_core::RootInputs::new(context.roots.home.clone(), context.roots.checkout.clone());
    inputs.state_dir = Some(launch.configuration_root.clone());
    let roots = inputs
        .resolve(&provenant_supervisor::fs::LocalFs)
        .map_err(|error| error.to_string())?;
    inputs.state_dir = Some(
        context
            .roots
            .state_root
            .join("children")
            .join(context.attempt_id.to_string()),
    );
    let child_state = inputs
        .resolve(&provenant_supervisor::fs::LocalFs)
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
        .with_sealed_requester(context.actor_id),
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
        let config = provenant_config::read_with_domains(
            &roots,
            &provenant_supervisor::fs::LocalFs,
            &crate::domains::SECTIONS,
        )
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
                task: parent_task,
                depth: parent_depth,
                maximum,
            },
            &reason,
            &call,
            |id, validated| {
                launch_on_host(&session, id, &edge, &call, Some(launch), Some(validated))
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
            for error in finish_children(&session.store, accepted, stop_reason, &call) {
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
    mut launch: impl FnMut(RunId, &run::CanonicalRequest) -> Result<Vec<run::Started>, Fail>,
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
            let generation = if record.state == run::AttemptState::Queued {
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
            Err(error) => scan.error = Some(error.message),
        }
    }
    Ok(scan)
}

/// Finish the last scan without leaving queued attempts for a worker that
/// has stopped. Normal completion preserves children already in custody.
fn finish_children(
    store: &run::RunStore,
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
        errors.extend(store.stop_children(accepted, "host parent stopped", call));
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
            assert_eq!(record.state, run::AttemptState::Queued);
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
            fn memory(
                &self,
            ) -> std::io::Result<provenant_supervisor::sys::resources::MemorySample> {
                Ok(provenant_supervisor::sys::resources::MemorySample {
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
            // Admit in the context callback, then stop handoff before custody.
            // The launcher's stale Starting withdrawal must leave Running intact.
            assert!(matches!(
                store.start_child(
                    child.run_id,
                    &child.request,
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
                run::AttemptState::Running
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
            // Running without a guardian cannot be stopped. Its error must
            // survive the drain without hiding proposals beyond the window.
            accepted.insert((running, 1));
        }
        let errors = finish_children(&store, accepted, reason, &call);
        assert_eq!(errors.len(), usize::from(unobservable_child), "{errors:?}");
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
            run::AttemptState::Running
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
            state_root: inputs.resolve(&provenant_supervisor::fs::LocalFs)?.state,
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
                schema_version: 1,
                tasks: vec![run::TaskRequest {
                    base: None,
                    base_ref: None,
                    task_id,
                    prompt_digest: CanonicalV1::file_digest(b"child"),
                    route: run::RequestedRoute::Model {
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
