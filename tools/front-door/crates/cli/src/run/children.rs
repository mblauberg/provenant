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
}
impl<'a> HostHolder<'a> {
    pub fn new(inner: &'a dyn Holder) -> Self {
        Self {
            inner,
            service: Mutex::new(None),
        }
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
        self.inner.terminal(context, attempt, exit, recovery)
    }
}

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
        for id in session
            .store
            .queued_runs(&call)
            .map_err(|error| error.to_string())?
        {
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
                session
                    .store
                    .refuse_queued(id, "parent stopped before child launch", &call)
                    .map_err(|error| error.to_string())?;
                continue;
            }
            let validated = match session.store.validate_child(
                id,
                context.attempt_id,
                parent_task,
                parent_depth,
                maximum,
                &call,
            ) {
                Ok(validated) => validated,
                Err(error) => {
                    session
                        .store
                        .refuse_queued(id, &error.to_string(), &call)
                        .map_err(|error| error.to_string())?;
                    continue;
                }
            };
            // Accept refs before handoff: start can launch an earlier task
            // and then fail on a later one. Keep all scopes for cancellation.
            for record in session
                .store
                .pending_attempts(id, &call)
                .map_err(|error| error.to_string())?
            {
                let generation = if record.state == run::AttemptState::Queued {
                    record
                        .generation
                        .checked_add(1)
                        .ok_or("child custody generation exhausted")?
                } else {
                    record.generation
                };
                accepted.insert((record.attempt_id, generation));
            }
            if let Err(error) =
                launch_on_host(&session, id, &edge, &call, Some((launch, &validated)))
            {
                launch_error = Some(error.message);
            }
        }
        if stopping {
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
                for (id, generation) in accepted {
                    let record = match session.store.record(id, &call) {
                        Ok(record) => record,
                        Err(error) => {
                            launch_error = Some(error.to_string());
                            continue;
                        }
                    };
                    if matches!(record.state, run::AttemptState::Terminal { .. }) {
                        continue;
                    }
                    let still_queued = record.state == run::AttemptState::Queued
                        && record.generation.checked_add(1) == Some(generation);
                    if record.generation != generation && !still_queued {
                        launch_error = Some("accepted child generation changed".into());
                        continue;
                    }
                    match session.store.cancel(id, "host parent stopped", &call) {
                        Ok(run::CancelReceipt::Unobservable { reason }) => {
                            launch_error = Some(reason)
                        }
                        Err(error) => launch_error = Some(error.to_string()),
                        Ok(_) => {}
                    }
                }
            }
            return match launch_error {
                Some(error) => Err(error),
                None => Ok(()),
            };
        }
        std::thread::park_timeout(Duration::from_millis(100));
    }
}
