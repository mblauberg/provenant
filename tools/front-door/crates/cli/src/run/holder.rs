//! The guardian's provider decoders and graph relay.
use super::*;
use provenant_os::fs::Directory;
use provenant_supervisor::{Holder, custody, machine::StopReason};
use std::sync::{Arc, Mutex};

/// Bound to the admitted route, never provider-controlled output.
#[derive(Default)]
struct QuotaFrames(Mutex<Option<&'static dyn provenant_os::capture::FrameDecoder>>);
impl provenant_os::capture::FrameDecoder for QuotaFrames {
    fn decode(
        &self,
        frame: &[u8],
    ) -> Result<provenant_os::capture::FrameEvent, provenant_os::capture::DecodeFailure> {
        match *self
            .0
            .lock()
            .map_err(|_| provenant_os::capture::DecodeFailure)?
        {
            Some(decoder) => decoder.decode(frame),
            None => Ok(provenant_os::capture::FrameEvent::Progress),
        }
    }
    fn partial_terminal(&self, frame: &[u8]) -> bool {
        self.0
            .lock()
            .ok()
            .and_then(|decoder| *decoder)
            .is_some_and(|decoder| decoder.partial_terminal(frame))
    }
}

pub(crate) type GraphWake = dyn Fn(&run::HolderContext, &str) -> Result<(), String> + Send + Sync;

/// Wrap Run's holder without moving provider launch into Fabric or supervisor.
/// Graph submissions use the host relay while the guardian drains output.
pub struct HostHolder<'a> {
    inner: &'a dyn Holder,
    graph_wake: Option<Arc<GraphWake>>,
    stdout_frames: QuotaFrames,
    stderr_frames: QuotaFrames,
    fabric_relay: Mutex<Option<provenant_fabric_mcp::lane::HostRelay>>,
    settled: Mutex<Option<run::HolderContext>>,
}
impl<'a> HostHolder<'a> {
    pub fn new(inner: &'a dyn Holder) -> Self {
        Self {
            inner,
            graph_wake: None,
            stdout_frames: QuotaFrames::default(),
            stderr_frames: QuotaFrames::default(),
            fabric_relay: Mutex::new(None),
            settled: Mutex::new(None),
        }
    }
    /// Let the binary wake its graph owner after a durable relay admission.
    pub fn with_graph_wake(
        mut self,
        wake: impl Fn(&run::HolderContext, &str) -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        self.graph_wake = Some(Arc::new(wake));
        self
    }
    /// Live stdout lifecycle evidence, bound during admission before spawn.
    pub fn stdout_decoder(&self) -> &dyn provenant_os::capture::FrameDecoder {
        &self.stdout_frames
    }
    /// Live stderr lifecycle evidence, bound during admission before spawn.
    pub fn stderr_decoder(&self) -> &dyn provenant_os::capture::FrameDecoder {
        &self.stderr_frames
    }
    /// Available after supervise returns, when custody locks have been released.
    pub fn settled(&self) -> custody::Result<Option<run::HolderContext>> {
        Ok(self
            .settled
            .lock()
            .map_err(|_| custody::CustodyError::InvalidRecord)?
            .take())
    }
    fn stop(&self) -> custody::Result<()> {
        drop(
            self.fabric_relay
                .lock()
                .map_err(|_| custody::CustodyError::InvalidRecord)?
                .take(),
        );
        Ok(())
    }
}
impl Drop for HostHolder<'_> {
    fn drop(&mut self) {
        // Close the relay if supervision returns before the normal exit hook.
        let _ = self.stop();
    }
}
impl Holder for HostHolder<'_> {
    fn admit(&self, raw: &[u8], attempt: &Directory) -> custody::Result<()> {
        self.inner.admit(raw, attempt)?;
        let (context, store) = run::HolderContext::open(raw)
            .map_err(|error| custody::CustodyError::Io(std::io::Error::other(error)))?;
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let record = store
            .record(
                context.attempt_id,
                &run::Request {
                    clock: &clock,
                    cancel: &cancel,
                },
            )
            .map_err(|error| custody::CustodyError::Io(std::io::Error::other(error)))?;
        let registration = provenant_providers::REGISTRY.iter().find(|entry| {
            Some(entry.adapter.id())
                == record
                    .applied_route
                    .as_ref()
                    .and_then(|route| route.provider.as_deref())
        });
        *self
            .stdout_frames
            .0
            .lock()
            .map_err(|_| custody::CustodyError::InvalidRecord)? =
            registration.and_then(|entry| entry.stdout_decoder);
        *self
            .stderr_frames
            .0
            .lock()
            .map_err(|_| custody::CustodyError::InvalidRecord)? =
            registration.and_then(|entry| entry.stderr_decoder);
        if context.launch.is_none() {
            return Ok(());
        }
        *self
            .fabric_relay
            .lock()
            .map_err(|_| custody::CustodyError::InvalidRecord)? = Some(
            provider::start_fabric_relay(&context, self.graph_wake.clone())
                .map_err(|error| custody::CustodyError::Io(std::io::Error::other(error)))?,
        );
        Ok(())
    }
    fn provider_exited(&self, context: &[u8], reason: Option<StopReason>) -> custody::Result<()> {
        self.inner.provider_exited(context, reason)?;
        self.stop()
    }
    fn terminal(
        &self,
        context: &[u8],
        attempt: &Directory,
        exit: &custody::ExitObservation,
        recovery: &custody::Recovery,
    ) -> custody::Result<()> {
        self.stop()?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::{ActorId, Digest, OperationId, ProjectId, RootInputs, RunId, TaskId};
    use provenant_records::CanonicalV1;

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
        for database in [false, true] {
            task.access.database_ipc = database;
            let sealed = seal_launch_task(child.request.digest()?, &task);
            let restored: run::SealedTask = serde_json::from_value(serde_json::to_value(sealed)?)?;
            assert_eq!(restored.effective_database_ipc, database);
        }
        let mut json = serde_json::to_value(seal_launch_task(child.request.digest()?, &task))?;
        json.as_object_mut()
            .ok_or("sealed task")?
            .remove("effective_network");
        json.as_object_mut()
            .ok_or("sealed task")?
            .remove("effective_database_ipc");
        let old: run::SealedTask = serde_json::from_value(json.clone())?;
        assert_eq!(old.effective_network, NetworkAccess::Provider);
        assert!(!old.effective_database_ipc);
        old.apply_access(&mut task);
        assert!(!task.access.database_ipc);
        json["effective_database_ipc"] = serde_json::Value::Null;
        assert!(serde_json::from_value::<run::SealedTask>(json.clone()).is_err());
        json.as_object_mut()
            .ok_or("sealed task")?
            .remove("effective_database_ipc");
        json["effective_network"] = serde_json::Value::Null;
        assert!(serde_json::from_value::<run::SealedTask>(json).is_err());
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
                    cloud: None,
                    context: None,
                    extra: Default::default(),
                    build_heavy: false,
                    title: None,
                    base: None,
                    head: None,
                    source_head: None,
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
