//! The guardian's provider decoders and graph relay.
use super::*;
use provenant_os::fs::Directory;
use provenant_supervisor::{Holder, custody, machine::StopReason};
use std::sync::{Arc, Mutex, OnceLock};

/// Bound to the admitted route, never provider-controlled output.
#[derive(Default)]
struct QuotaFrames {
    decoder: Mutex<Option<&'static dyn provenant_os::capture::FrameDecoder>>,
    evidence: Arc<Mutex<Option<QuotaCapture>>>,
    stderr: bool,
}
struct QuotaCapture {
    adapter: &'static dyn provenant_providers::Adapter,
    stream: provenant_providers::StreamState,
    attempt: Directory,
    state_root: PathBuf,
    route: run::RouteRecord,
    shared: bool,
    recorded: bool,
}
impl provenant_os::capture::FrameDecoder for QuotaFrames {
    fn decode(
        &self,
        frame: &[u8],
    ) -> Result<provenant_os::capture::FrameEvent, provenant_os::capture::DecodeFailure> {
        use provenant_os::capture::{DecodeFailure, FrameEvent};
        let result = match *self.decoder.lock().map_err(|_| DecodeFailure)? {
            Some(decoder) => decoder.decode(frame)?,
            None => FrameEvent::Progress,
        };
        let mut guard = self.evidence.lock().map_err(|_| DecodeFailure)?;
        if let Some(capture) = guard.as_mut().filter(|capture| !capture.recorded) {
            let events = if self.stderr && result == FrameEvent::Terminal {
                provenant_providers::opencode::stderr_quota(frame)
                    .into_iter()
                    .collect()
            } else if self.stderr {
                Vec::new()
            } else {
                let mut line = frame.to_vec();
                line.push(b'\n');
                capture
                    .adapter
                    .observe(&mut capture.stream, &line)
                    .unwrap_or_default()
            };
            if let Some(ProviderEvent::QuotaLimited {
                pool,
                resets_in_seconds,
                ..
            }) = events
                .into_iter()
                .find(|event| matches!(event, ProviderEvent::QuotaLimited { .. }))
            {
                let now = SystemClock::new().unix_seconds();
                let evidence = QuotaEvidence {
                    pool,
                    observed: now,
                    resets: resets_in_seconds
                        .and_then(|seconds| i64::try_from(seconds).ok())
                        .and_then(|seconds| now.checked_add(seconds)),
                    shared: capture.shared,
                };
                evidence
                    .write(&capture.attempt)
                    .map_err(|_| DecodeFailure)?;
                capture.recorded = true;
                // Settlement replays the sealed evidence if publication fails.
                // A busy cache must not suppress the provider's terminal event.
                let _ = evidence.apply(&capture.state_root, &capture.route, false);
            }
        }
        Ok(result)
    }
    fn partial_terminal(&self, frame: &[u8]) -> bool {
        self.decoder
            .lock()
            .ok()
            .and_then(|decoder| *decoder)
            .is_some_and(|decoder| decoder.partial_terminal(frame))
    }
    fn reports_usage(&self) -> bool {
        self.decoder
            .lock()
            .ok()
            .and_then(|decoder| *decoder)
            .is_some_and(|decoder| decoder.reports_usage())
    }
}

pub(crate) type GraphWake = dyn Fn(&run::HolderContext, &str) -> Result<(), String> + Send + Sync;

pub(crate) type GraphRead = dyn Fn(
        &run::HolderContext,
        serde_json::Value,
        &provenant_core::CancelToken,
        std::time::Duration,
    ) -> Result<serde_json::Value, provenant_fabric_runtime::relay::ServiceError>
    + Send
    + Sync;

/// Wrap Run's holder without moving provider launch into Fabric or supervisor.
/// Graph submissions use the host relay while the guardian drains output.
pub struct HostHolder<'a> {
    inner: &'a dyn Holder,
    graph_wake: Option<Arc<GraphWake>>,
    graph_read: Option<Arc<GraphRead>>,
    stdout_frames: QuotaFrames,
    stderr_frames: QuotaFrames,
    fabric_relay: Mutex<Option<provenant_fabric_runtime::relay::HostRelay>>,
    fabric: OnceLock<BoundFabric>,
    settled: Mutex<Option<run::HolderContext>>,
}

/// Resolve host identity and configuration once, sharing the context with the
/// relay. Only the graph journal is reread during census, including after exit.
struct BoundFabric {
    holder_context: Vec<u8>,
    source: Result<Option<HolderFabric>, String>,
}
struct HolderFabric {
    context: Arc<crate::fabric::Context>,
    reference: String,
}
impl HolderFabric {
    fn open(raw: &[u8]) -> Result<Option<Self>, Box<dyn std::error::Error>> {
        let context: run::HolderContext = serde_json::from_slice(raw)?;
        let Some(launch) = &context.launch else {
            return Ok(None);
        };
        let mut inputs = provenant_core::RootInputs::new(
            context.roots.home.clone(),
            context.roots.checkout.clone(),
        );
        inputs.state_dir = Some(launch.configuration_root.clone());
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let directory = launch.graph_directory.as_deref().unwrap_or(&launch.cwd);
        let mut ctx = crate::fabric::Context::open(roots, &context.roots.home, directory)?;
        if ctx.identity.project != context.project_id {
            return Err("Fabric custody project mismatch".into());
        }
        let reference = format!("{}/{}", ctx.identity.host, context.attempt_id);
        ctx.identity.inbox = provenant_fabric_runtime::identity::Address::new(
            ctx.identity.host,
            &format!("lane-{}", context.attempt_id),
        )?;
        Ok(Some(Self {
            context: Arc::new(ctx),
            reference,
        }))
    }
}
impl<'a> HostHolder<'a> {
    pub fn new(inner: &'a dyn Holder) -> Self {
        let evidence = Arc::new(Mutex::new(None));
        Self {
            inner,
            graph_wake: None,
            graph_read: None,
            stdout_frames: QuotaFrames {
                evidence: evidence.clone(),
                ..Default::default()
            },
            stderr_frames: QuotaFrames {
                evidence,
                stderr: true,
                ..Default::default()
            },
            fabric_relay: Mutex::new(None),
            fabric: OnceLock::new(),
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
    /// Supply the composition edge's ordinary graph readers to the host relay.
    pub fn with_graph_read(
        mut self,
        read: impl Fn(
            &run::HolderContext,
            serde_json::Value,
            &provenant_core::CancelToken,
            std::time::Duration,
        )
            -> Result<serde_json::Value, provenant_fabric_runtime::relay::ServiceError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.graph_read = Some(Arc::new(read));
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
    fn fabric(&self, raw: &[u8]) -> custody::Result<Option<&HolderFabric>> {
        let bound = self.fabric.get_or_init(|| BoundFabric {
            holder_context: raw.to_vec(),
            source: HolderFabric::open(raw).map_err(|error| error.to_string()),
        });
        // A holder belongs to one supervisor. Never reuse its identity for a
        // different attempt, even if a caller mistakenly supplies other bytes.
        if bound.holder_context != raw {
            return Err(custody::CustodyError::InvalidRecord);
        }
        bound
            .source
            .as_ref()
            .map(Option::as_ref)
            .map_err(|error| custody::CustodyError::Io(std::io::Error::other(error.clone())))
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
/// Read retained graph state after the relay has closed, including children
/// submitted before provider exit and descendants of composite children.
fn live_graph_children(
    state: &provenant_core::StateRoot,
    project: provenant_core::ProjectId,
    reference: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    use provenant_fabric_runtime::graph::{FileJournal, NodeState, relay};
    for id in FileJournal::pending_graph_ids(state, project)? {
        let checkpoint = match FileJournal::read_checkpoint(state, project, &id) {
            Ok(Some(checkpoint)) => checkpoint,
            Ok(None) => return Err("missing graph checkpoint".into()),
            Err(error)
                if error.is_unreadable()
                    && FileJournal::unreadable_settled(state, project, &id)? =>
            {
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        if let Some(parent) = relay::historical_parent_of_attempt(&checkpoint, reference) {
            let prefix = format!("{parent}--");
            if checkpoint.nodes.iter().any(|node| {
                node.id.starts_with(&prefix)
                    && matches!(node.state, NodeState::Pending | NodeState::Running)
            }) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

impl Holder for HostHolder<'_> {
    fn has_live_children(&self, raw: &[u8]) -> custody::Result<bool> {
        let read = || -> Result<bool, Box<dyn std::error::Error>> {
            let Some(fabric) = self.fabric(raw)? else {
                return Ok(false);
            };
            live_graph_children(
                &fabric.context.roots.state,
                fabric.context.identity.project,
                &fabric.reference,
            )
        };
        read().map_err(|error| custody::CustodyError::Io(std::io::Error::other(error.to_string())))
    }

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
            .decoder
            .lock()
            .map_err(|_| custody::CustodyError::InvalidRecord)? =
            registration.and_then(|entry| entry.stdout_decoder);
        *self
            .stderr_frames
            .decoder
            .lock()
            .map_err(|_| custody::CustodyError::InvalidRecord)? =
            registration.and_then(|entry| entry.stderr_decoder);
        if let (Some(registration), Some(route)) = (registration, record.applied_route.clone()) {
            let stream = registration
                .adapter
                .stream(None)
                .map_err(|error| custody::CustodyError::Io(std::io::Error::other(error)))?;
            let (configuration_root, _) = context.disk_roots();
            *self
                .stdout_frames
                .evidence
                .lock()
                .map_err(|_| custody::CustodyError::InvalidRecord)? = Some(QuotaCapture {
                adapter: registration.adapter,
                stream,
                attempt: attempt.try_clone()?,
                state_root: context.roots.state_root.clone(),
                route,
                shared: configuration_root == context.roots.state_root,
                recorded: false,
            });
        }
        let Some(fabric) = self.fabric(raw)? else {
            return Ok(());
        };
        *self
            .fabric_relay
            .lock()
            .map_err(|_| custody::CustodyError::InvalidRecord)? = Some(
            provider::start_fabric_relay(
                &context,
                Arc::clone(&fabric.context),
                self.graph_wake.clone(),
                self.graph_read.clone(),
            )
            .map_err(|error| custody::CustodyError::Io(std::io::Error::other(error)))?,
        );
        Ok(())
    }
    fn budget_warning(&self, context: &[u8], tokens: u64, budget: u64) -> custody::Result<()> {
        self.inner.budget_warning(context, tokens, budget)
    }
    fn budget_clean_point(&self, context: &[u8]) -> custody::Result<bool> {
        self.inner.budget_clean_point(context)
    }
    fn budget_crossed(&self, context: &[u8], tokens: u64, budget: u64) -> custody::Result<()> {
        self.inner.budget_crossed(context, tokens, budget)?;
        let notify = || -> Result<(), Box<dyn std::error::Error>> {
            let (holder, runs) = run::HolderContext::open(context)?;
            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let call = run::Request {
                clock: &clock,
                cancel: &cancel,
            };
            let request = runs.request(holder.attempt_id, &call)?;
            let Some(chair) = request.chair else {
                return Ok(());
            };
            let Some(fabric) = self.fabric(context)? else {
                return Ok(());
            };
            let record = runs.record(holder.attempt_id, &call)?;
            let digest = provenant_records::CanonicalV1::record_digest(
                "budget-overrun-notice",
                1,
                &holder.attempt_id,
            )?;
            let mut id = [0; 16];
            id.copy_from_slice(&digest.as_bytes()[..16]);
            let inbox_call = provenant_fabric_store::Request {
                clock: &clock,
                cancel: &cancel,
            };
            let inbox =
                provenant_fabric_store::Store::open(&fabric.context.roots.state, &inbox_call)?;
            inbox.send(&provenant_fabric_store::Message {
                extra: Default::default(), header: provenant_records::RecordHeader::V1,
                project: holder.project_id, id: provenant_core::MessageId::from_bytes(id),
                sender: holder.actor_id, recipients: vec![chair.actor], conversation: None,
                kind: provenant_fabric_store::MessageKind::Note,
                body: format!("Writer {} crossed its token target: {tokens} fresh tokens, budget {budget}. Its configured budget action applies; status records the overrun.", holder.attempt_id),
                artifacts: Vec::new(), reply_to: None, created_at: record.queued_at,
            }, &inbox_call)?;
            Ok(())
        };
        notify()
            .map_err(|error| custody::CustodyError::Io(std::io::Error::other(error.to_string())))
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
    use provenant_core::{ActorId, Digest, OperationId, ProjectId, RootInputs};
    use provenant_records::CanonicalV1;

    #[test]
    fn quota_event_survives_guardian_death_and_recovery_writes_once()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_os::capture::FrameDecoder;
        use std::os::unix::fs::PermissionsExt;
        const CHILD: &str = "PROVENANT_TEST_QUOTA_EVENT_CHILD";
        let temp = tempfile::tempdir()?;
        let state = if let Some(path) = std::env::var_os(CHILD) {
            PathBuf::from(path)
        } else {
            let state = temp.path().join("state");
            std::fs::create_dir(&state)?;
            std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;
            use provenant_os::exec::{
                ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination,
            };
            let argv = [
                std::env::current_exe()?.into_os_string(),
                "--exact".into(),
                "run::holder::tests::quota_event_survives_guardian_death_and_recovery_writes_once"
                    .into(),
                "--nocapture".into(),
            ];
            let mut env: Vec<_> = std::env::vars_os().collect();
            env.push((CHILD.into(), state.clone().into_os_string()));
            let cwd = std::env::current_dir()?;
            let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(30));
            options.env = Some(&env);
            options.stdout = StreamPolicy::Capture { limit: 65536 };
            options.stderr = StreamPolicy::Capture { limit: 65536 };
            let output = SystemExecutor::new(1)?.execute(options)?;
            assert_eq!(output.termination, Termination::Exit(0), "{output:?}");
            let path = state.join("quota/key/opencode.json");
            let before = std::fs::read(&path)?;
            let provider_state = state.join("attempts/attempt/provider-state");
            let route = quota_test_route();
            let decoded = run::Providers::decode_captures(
                &Registry {
                    state_root: Some(&state),
                    ..Default::default()
                },
                &route,
                b"",
                b"",
                &super::super::tests::clean_exit(),
                &provider_state,
                None,
            )?;
            assert_eq!(decoded.outcome, run::Outcome::QuotaLimited);
            assert_eq!(std::fs::read(&path)?, before);
            let record = crate::quota::read_cache_at(&state)?;
            assert_eq!(record[0].refusals.len(), 1);
            return Ok(());
        };
        let attempt_path = state.join("attempts/attempt");
        std::fs::create_dir_all(&attempt_path)?;
        let registration = &provenant_providers::opencode::REGISTRATION;
        let frames = QuotaFrames {
            decoder: Mutex::new(registration.stdout_decoder),
            evidence: Arc::new(Mutex::new(Some(QuotaCapture {
                adapter: registration.adapter,
                stream: registration.adapter.stream(None)?,
                attempt: Directory::open_anchor(&attempt_path)?,
                state_root: state.clone(),
                route: quota_test_route(),
                shared: true,
                recorded: false,
            }))),
            stderr: false,
        };
        frames.decode(br#"{"type":"provider.quota","response":{"body":{"error":{"type":"FreeUsageLimitError","message":"fixture"}}}}"#)?;
        assert_eq!(crate::quota::read_cache_at(&state)?[0].refusals.len(), 1);
        // End without settlement or dropping the live holder.
        std::process::exit(0);
    }

    #[test]
    fn recovery_publishes_sealed_quota_evidence_once() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        let state = temp.path().join("state");
        let attempt = state.join("attempts/attempt");
        std::fs::create_dir_all(&attempt)?;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;
        let observed = SystemClock::new().unix_seconds();
        QuotaEvidence {
            pool: Some("opencode-free".into()),
            observed,
            resets: Some(observed + 60),
            shared: true,
        }
        .write(&Directory::open_anchor(&attempt)?)?;
        assert!(crate::quota::read_cache_at(&state)?.is_empty());
        let provider_state = attempt.join("provider-state");
        let route = quota_test_route();
        let decode = || {
            run::Providers::decode_captures(
                &Registry {
                    state_root: Some(&state),
                    ..Default::default()
                },
                &route,
                b"",
                b"",
                &super::super::tests::clean_exit(),
                &provider_state,
                None,
            )
        };
        assert_eq!(decode()?.outcome, run::Outcome::QuotaLimited);
        let path = state.join("quota/key/opencode.json");
        let before = std::fs::read(&path)?;
        let records = crate::quota::read_cache_at(&state)?;
        assert_eq!(records[0].refusals[0].observed_unix_seconds, observed);
        assert_eq!(
            records[0].refusals[0].resets_unix_seconds,
            Some(observed + 60)
        );
        assert_eq!(decode()?.outcome, run::Outcome::QuotaLimited);
        assert_eq!(std::fs::read(path)?, before);
        Ok(())
    }

    #[test]
    fn an_account_usage_limit_settles_typed_and_holds_every_model_until_reset()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        let state = temp.path().join("state");
        let attempt = state.join("attempts/attempt");
        std::fs::create_dir_all(&attempt)?;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;
        let observed = SystemClock::new().unix_seconds();
        let resets = observed + 4 * 86_400;
        QuotaEvidence {
            pool: Some(provenant_quota::ACCOUNT_ROUTE.into()),
            observed,
            resets: Some(resets),
            shared: true,
        }
        .write(&Directory::open_anchor(&attempt)?)?;
        let route = quota_test_route();
        let decoded = run::Providers::decode_captures(
            &Registry {
                state_root: Some(&state),
                ..Default::default()
            },
            &route,
            b"",
            b"",
            &super::super::tests::clean_exit(),
            &attempt.join("provider-state"),
            None,
        )?;
        assert_eq!(decoded.outcome, run::Outcome::QuotaLimited);
        let reason = decoded.reason.unwrap_or_default();
        let reset = jiff::Timestamp::from_second(resets)?.to_string();
        assert!(reason.contains("; account key"), "{reason}");
        assert!(reason.contains(&format!("; resets at {reset}")), "{reason}");
        let records = crate::quota::read_cache_at(&state)?;
        assert_eq!(records[0].refusals[0].resets_unix_seconds, Some(resets));
        let admission = provenant_quota::Admission {
            policies: &[],
            now_unix_seconds: observed + 1,
            records: &records,
            pools: &[],
        };
        // Another model on the same account is held until the reset.
        assert_eq!(
            admission.account_block("opencode", "key", "another")?,
            Some(provenant_quota::AccountBlock::ScopedRefusal)
        );
        assert_eq!(
            admission.account_block("opencode", "other", "another")?,
            None
        );
        let after = provenant_quota::Admission {
            now_unix_seconds: resets,
            ..admission
        };
        assert_eq!(after.account_block("opencode", "key", "another")?, None);
        Ok(())
    }

    #[test]
    fn busy_quota_cache_preserves_terminal_and_sealed_evidence()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_os::{
            capture::{FrameDecoder, FrameEvent},
            fs::LockMode,
        };
        use std::{ffi::OsStr, os::unix::fs::PermissionsExt};

        let temp = tempfile::tempdir()?;
        let state = temp.path().join("state");
        let attempt = state.join("attempts/attempt");
        let quota = state.join("quota");
        std::fs::create_dir_all(&attempt)?;
        std::fs::create_dir(&quota)?;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;
        std::fs::set_permissions(&quota, std::fs::Permissions::from_mode(0o700))?;
        let directory = Directory::open_anchor(&quota)?;
        let lock = directory.try_lock(OsStr::new("quota.lock"), LockMode::Exclusive)?;
        let registration = &provenant_providers::opencode::REGISTRATION;
        let frames = QuotaFrames {
            decoder: Mutex::new(registration.stdout_decoder),
            evidence: Arc::new(Mutex::new(Some(QuotaCapture {
                adapter: registration.adapter,
                stream: registration.adapter.stream(None)?,
                attempt: Directory::open_anchor(&attempt)?,
                state_root: state.clone(),
                route: quota_test_route(),
                shared: true,
                recorded: false,
            }))),
            stderr: false,
        };
        let frame = br#"{"type":"provider.quota","response":{"body":{"error":{"type":"FreeUsageLimitError","message":"fixture"}}}}"#;
        assert_eq!(frames.decode(frame), Ok(FrameEvent::Terminal));
        let evidence = std::fs::read(attempt.join("quota-evidence.json"))?;
        drop(lock);
        assert_eq!(frames.decode(frame), Ok(FrameEvent::Terminal));
        assert_eq!(
            std::fs::read(attempt.join("quota-evidence.json"))?,
            evidence
        );
        let provider_state = attempt.join("provider-state");
        for _ in 0..2 {
            let decoded = run::Providers::decode_captures(
                &Registry {
                    state_root: Some(&state),
                    ..Default::default()
                },
                &quota_test_route(),
                b"",
                b"",
                &super::super::tests::clean_exit(),
                &provider_state,
                None,
            )?;
            assert_eq!(decoded.outcome, run::Outcome::QuotaLimited);
        }
        assert_eq!(crate::quota::read_cache_at(&state)?[0].refusals.len(), 1);
        Ok(())
    }

    fn quota_test_route() -> run::RouteRecord {
        run::RouteRecord {
            provider: Some("opencode".into()),
            model: Some("opencode/example".into()),
            account_key: Some("key".into()),
            account_label: Some("key".into()),
            ..Default::default()
        }
    }

    #[test]
    fn custody_reads_live_graph_children_after_parent_exit()
    -> Result<(), Box<dyn std::error::Error>> {
        if super::super::tests::reexec_test_in_lane(
            "run::holder::tests::custody_reads_live_graph_children_after_parent_exit",
        )? {
            return Ok(());
        }
        use provenant_fabric_runtime::graph::{
            FileJournal, GraphCheckpoint, GraphDocument, GraphExecution, GraphJournal,
        };
        use serde_json::json;
        let temp = tempfile::tempdir()?;
        let clock = SystemClock::new();
        let root = std::fs::canonicalize(temp.path())?;
        let home = root.join("home");
        let checkout = root.join("checkout");
        std::fs::create_dir_all(&home)?;
        std::fs::create_dir_all(&checkout)?;
        let mut inputs = RootInputs::new(home.clone(), checkout.clone());
        inputs.state_dir = Some(root.join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let ctx = crate::fabric::Context::open(roots.clone(), &home, &checkout)?;
        let state = &roots.state;
        let project = ctx.identity.project;
        let holder_context = run::HolderContext {
            schema_version: 1,
            roots: run::RootSeed {
                home,
                checkout: checkout.clone(),
                state_root: state.as_path().into(),
            },
            project_id: project,
            actor_id: ActorId::from_bytes([2; 16]),
            attempt_id: AttemptId::from_bytes([4; 16]),
            host_delivery: None,
            sealed: None,
            launch: Some(run::HostLaunchContext::from_environment(
                state.as_path(),
                &checkout,
                provenant_os::git::git_executable().ok_or("Git unavailable")?,
            )),
        };
        struct UnusedHolder;
        impl Holder for UnusedHolder {
            fn admit(&self, _: &[u8], _: &Directory) -> custody::Result<()> {
                Err(custody::CustodyError::InvalidRecord)
            }
            fn terminal(
                &self,
                _: &[u8],
                _: &Directory,
                _: &custody::ExitObservation,
                _: &custody::Recovery,
            ) -> custody::Result<()> {
                Err(custody::CustodyError::InvalidRecord)
            }
        }
        let holder = HostHolder::new(&UnusedHolder);
        let raw = serde_json::to_vec(&holder_context)?;
        let reference = format!("{}/{}", ctx.identity.host, holder_context.attempt_id);
        assert!(!holder.has_live_children(&raw)?);
        // Once resolved, census must not need Git, the checkout or config again.
        std::fs::rename(&checkout, root.join("moved-checkout"))?;
        std::fs::write(state.as_path().join("config.toml"), b"invalid = [")?;
        let cancel = CancelToken::new();
        let budget = provenant_core::WaitBudget::cli(clock.mono_now(), 5)?;
        let mut journal = FileJournal::open(state, project, "custody", budget, &cancel, &clock)?;
        let document: GraphDocument = serde_json::from_value(json!({
            "schema_version":2,"graph_id":"custody","integration_branch":"main",
            "nodes":[{"id":"parent","access":"read_only","prompt":"Go","route":{"alias":"senior"}}]
        }))?;
        journal.bind(GraphExecution {
            review_base: None,
            extra: Default::default(),
            integration_base: None,
            directory: temp.path().to_path_buf(),
            document,
        })?;
        for status in [
            "pending",
            "running",
            "succeeded",
            "failed",
            "blocked",
            "skipped",
        ] {
            let checkpoint: GraphCheckpoint = serde_json::from_value(json!({
                "schema_version":1,"graph_id":"custody","plan_digest":"fixture",
                "nodes":[{"id":"parent","state":"succeeded","attempts":[{
                    "launched":true,"key":{"graph_id":"custody","node_id":"parent","route_index":0, "attempt": 1},
                    "route":{"alias":"senior"},"state":"terminal","attempt_ref":reference,
                    "outcome":null,"launch_actor":"fixture"
                }]}, {"id":"parent--child--grandchild","state":status,"attempts":[]},
                {"id":"parental--other","state":"running","attempts":[]}]
            }))?;
            journal.save(&checkpoint)?;
            assert_eq!(
                holder.has_live_children(&raw)?,
                matches!(status, "pending" | "running"),
                "{status}"
            );
            assert!(!live_graph_children(state, project, "host/other-attempt")?);
        }
        drop(journal);
        std::fs::write(
            state
                .as_path()
                .join("fabric/graphs")
                .join(project.to_string())
                .join("graph-custody.json"),
            b"broken checkpoint",
        )?;
        assert!(holder.has_live_children(&raw).is_err());
        std::fs::write(
            state
                .as_path()
                .join("fabric/graphs")
                .join(project.to_string())
                .join("graph-custody.json"),
            br#"{"checkpoint":{"nodes":[{"state":"succeeded"}]}}"#,
        )?;
        assert!(!holder.has_live_children(&raw)?);
        let mut other = holder_context;
        other.attempt_id = AttemptId::from_bytes([5; 16]);
        assert!(matches!(
            holder.fabric(&serde_json::to_vec(&other)?),
            Err(custody::CustodyError::InvalidRecord)
        ));
        Ok(())
    }

    #[test]
    fn launch_seal_records_root_network_and_refuses_an_incomplete_seal()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_confinement::permissions::NetworkAccess;
        let temp = tempfile::tempdir()?;
        let (_, child) = child_fixture(temp.path(), &SystemClock::new())?;
        let mut task = child.request.task.clone();
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
        // A seal missing either effective bound is refused, never defaulted.
        let json = serde_json::to_value(seal_launch_task(child.request.digest()?, &task))?;
        for field in ["effective_network", "effective_database_ipc"] {
            let mut missing = json.clone();
            missing.as_object_mut().ok_or("sealed task")?.remove(field);
            assert!(
                serde_json::from_value::<run::SealedTask>(missing).is_err(),
                "{field}"
            );
            let mut null = json.clone();
            null[field] = serde_json::Value::Null;
            assert!(
                serde_json::from_value::<run::SealedTask>(null).is_err(),
                "{field}"
            );
        }
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
        let id = AttemptId::from_bytes([6; 16]);
        let child = run::PreparedRun {
            attempt_id: id,
            operation_id: OperationId::parse("deferred-child")?,
            created_at: clock.wall_now()?,
            request: run::CanonicalRequest {
                extra: Default::default(),
                chair: None,
                fresh_continuation: None,
                schema_version: 1,
                task: run::TaskRequest {
                    context: Some(provenant_records::run::WorkContext {
                        graph_id: Some(format!("lane-{id}")),
                        node_id: Some("node".into()),
                        attempt: Some(1),
                        ..Default::default()
                    }),
                    extra: Default::default(),
                    build_heavy: false,
                    title: None,
                    base: None,
                    head: None,
                    source_head: None,
                    base_ref: None,
                    prompt_digest: CanonicalV1::file_digest(b"child"),
                    route: run::RequestedRoute::Model {
                        extra: Default::default(),
                        provider: "fixture".into(),
                        model: "fixture/model".into(),
                        effort: None,
                    },
                    account: None,
                    profile: Default::default(),
                    authority_digest: Digest::from_bytes([5; 32]),
                    provider_timeout_seconds: 3600,
                    writer: None,
                    delivery_id: None,
                    deliver: None,
                    access: Default::default(),
                },
                parent: None,
                lane_depth: Some(2),
                parent_attempt: Some(AttemptId::from_bytes([9; 16])),
            },
            prompt: "child".into(),
        };
        Ok((context, child))
    }
}
