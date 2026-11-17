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
    source: Mutex<Option<provenant_os::capture::CaptureSourceIdentity>>,
    identity_conflict_observed: std::sync::atomic::AtomicBool,
    identity_evidence_incomplete: std::sync::atomic::AtomicBool,
    identity_eof_fragment_observed: std::sync::atomic::AtomicBool,
    // Capture invokes one stream decoder serially; scope the existing parser's
    // typed EOF error without adding another provider parser.
    eof_fragment_in_progress: std::sync::atomic::AtomicBool,
    stderr: bool,
}
/// Admission binds the adapter before capture first asks for filtered bytes.
struct StdoutFilter {
    evidence: Arc<Mutex<Option<QuotaCapture>>>,
    inner: Option<Box<dyn provenant_os::exec::CaptureFilter>>,
}

pub(super) fn stdout_filter(
    contract: provenant_records::StreamContract,
) -> Result<Box<dyn provenant_os::exec::CaptureFilter>, provenant_os::exec::FilterError> {
    use provenant_confinement::redact::{JsonlRedactor, Redactor};
    use provenant_records::StreamContract;
    match contract {
        StreamContract::ProviderJsonl => JsonlRedactor::provider([])
            .map(|r| Box::new(r) as Box<dyn provenant_os::exec::CaptureFilter>),
        StreamContract::Raw => {
            Redactor::new([]).map(|r| Box::new(r) as Box<dyn provenant_os::exec::CaptureFilter>)
        }
    }
    .map_err(|_| provenant_os::exec::FilterError)
}

impl StdoutFilter {
    fn inner(
        &mut self,
    ) -> Result<
        &mut (dyn provenant_os::exec::CaptureFilter + 'static),
        provenant_os::exec::FilterError,
    > {
        use provenant_os::exec::FilterError;
        if self.inner.is_none() {
            let evidence = self.evidence.lock().map_err(|_| FilterError)?;
            let capture = evidence.as_ref().ok_or(FilterError)?;
            let spec =
                provenant_records::AdapterSpec::get(capture.adapter.id()).ok_or(FilterError)?;
            self.inner = Some(stdout_filter(spec.stdout)?);
        }
        match self.inner.as_mut() {
            Some(filter) => Ok(filter.as_mut()),
            None => Err(FilterError),
        }
    }
}

impl provenant_os::exec::CaptureFilter for StdoutFilter {
    fn filter(
        &mut self,
        bytes: &[u8],
        sink: &mut dyn FnMut(&[u8]),
    ) -> Result<(), provenant_os::exec::FilterError> {
        self.inner()?.filter(bytes, sink)
    }
    fn finish(
        &mut self,
        sink: &mut dyn FnMut(&[u8]),
    ) -> Result<(), provenant_os::exec::FilterError> {
        self.inner()?.finish(sink)
    }
}

struct QuotaCapture {
    adapter: &'static dyn provenant_providers::Adapter,
    stream: provenant_providers::StreamState,
    attempt: Directory,
    state_root: PathBuf,
    route: run::RouteRecord,
    holder_context: Vec<u8>,
    shared: bool,
    recorded: bool,
    pending_session: Option<String>,
    session_ready: Option<String>,
    pending_conflict_session: Option<String>,
    ready_source: Option<provenant_os::capture::CaptureSourceIdentity>,
    pending_source_conflict: bool,
    pending_identity_conflict: bool,
    identity_conflict: bool,
}

impl provenant_os::capture::FrameDecoder for QuotaFrames {
    fn bind_source(&self, identity: &provenant_os::capture::CaptureSourceIdentity) {
        let changed = if let Ok(mut source) = self.source.lock() {
            let changed = source.as_ref().is_some_and(|saved| saved != identity);
            *source = Some(identity.clone());
            changed
        } else {
            false
        };
        if changed
            && let Ok(mut guard) = self.evidence.lock()
            && let Some(capture) = guard.as_mut()
            && capture.session_ready.is_some()
        {
            capture.pending_source_conflict = true;
            self.identity_conflict_observed
                .store(true, std::sync::atomic::Ordering::Release);
        }
    }

    fn decode_eof_fragment(
        &self,
        frame: &[u8],
    ) -> Result<provenant_os::capture::FrameEvent, provenant_os::capture::DecodeFailure> {
        self.eof_fragment_in_progress
            .store(true, std::sync::atomic::Ordering::Release);
        let result = self.decode(frame);
        self.eof_fragment_in_progress
            .store(false, std::sync::atomic::Ordering::Release);
        result
    }

    fn identity_eof_fragment_observed(&self) -> bool {
        self.identity_eof_fragment_observed
            .load(std::sync::atomic::Ordering::Acquire)
    }

    fn decode(
        &self,
        frame: &[u8],
    ) -> Result<provenant_os::capture::FrameEvent, provenant_os::capture::DecodeFailure> {
        use provenant_os::capture::{DecodeFailure, FrameEvent};
        let result = match *self.decoder.lock().map_err(|_| DecodeFailure)? {
            Some(decoder) => decoder.decode(frame)?,
            None => FrameEvent::Progress,
        };
        let eof_fragment = self
            .eof_fragment_in_progress
            .load(std::sync::atomic::Ordering::Acquire);
        let source = self.source.lock().ok().and_then(|source| source.clone());
        let mut guard = self.evidence.lock().map_err(|_| DecodeFailure)?;
        if let Some(capture) = guard.as_mut() {
            let mut stream_identity_conflict = false;
            let events: Vec<ProviderEvent> = if self.stderr && result == FrameEvent::Terminal {
                provenant_providers::opencode::stderr_quota(frame)
                    .into_iter()
                    .collect()
            } else if self.stderr {
                Vec::new()
            } else {
                let observed = if eof_fragment {
                    capture
                        .adapter
                        .observe_eof_fragment(&mut capture.stream, frame)
                } else {
                    let mut line = frame.to_vec();
                    line.push(b'\n');
                    capture.adapter.observe(&mut capture.stream, &line)
                };
                match observed {
                    Ok(events) => events,
                    Err(provenant_providers::ProviderError::IdentityMismatch) => {
                        stream_identity_conflict = true;
                        Vec::new()
                    }
                    Err(provenant_providers::ProviderError::IncompleteFrame) if eof_fragment => {
                        self.identity_eof_fragment_observed
                            .store(true, std::sync::atomic::Ordering::Release);
                        Vec::new()
                    }
                    Err(_) => {
                        self.identity_evidence_incomplete
                            .store(true, std::sync::atomic::Ordering::Release);
                        Vec::new()
                    }
                }
            };
            if !self.stderr {
                capture.pending_identity_conflict |= stream_identity_conflict;
                for session in events.iter().filter_map(|event| match event {
                    ProviderEvent::Session(session) => Some(session),
                    _ => None,
                }) {
                    if capture.identity_conflict {
                        break;
                    }
                    match capture.session_ready.as_deref() {
                        Some(ready) if ready != session => {
                            capture
                                .pending_conflict_session
                                .get_or_insert_with(|| session.clone());
                        }
                        Some(_) => {}
                        None => match capture.pending_session.as_deref() {
                            Some(first) if first != session => {
                                capture
                                    .pending_conflict_session
                                    .get_or_insert_with(|| session.clone());
                            }
                            Some(_) => {}
                            None => capture.pending_session = Some(session.clone()),
                        },
                    }
                }
                let source_conflict = capture.pending_source_conflict
                    && capture.ready_source.as_ref() != source.as_ref();
                if capture.pending_identity_conflict
                    || capture.pending_conflict_session.is_some()
                    || source_conflict
                {
                    self.identity_conflict_observed
                        .store(true, std::sync::atomic::Ordering::Release);
                }
                if (capture.pending_session.is_some()
                    || capture.pending_conflict_session.is_some()
                    || capture.pending_source_conflict
                    || capture.pending_identity_conflict)
                    && let Some(source) = source.as_ref()
                    && let Ok((context, store)) = run::HolderContext::open(&capture.holder_context)
                {
                    let clock = SystemClock::new();
                    let cancel = CancelToken::new();
                    let call = run::Request {
                        clock: &clock,
                        cancel: &cancel,
                    };
                    if capture.session_ready.is_none()
                        && let Some(session) = capture.pending_session.as_deref()
                        && store
                            .record_provider_session_ready(
                                context.attempt_id,
                                &capture.route,
                                session,
                                source,
                                &call,
                            )
                            .unwrap_or(false)
                    {
                        capture.session_ready = Some(session.to_owned());
                        capture.ready_source = Some(source.clone());
                        capture.pending_session = None;
                    }
                    let conflict_session = capture.pending_conflict_session.clone().or_else(|| {
                        (capture.pending_source_conflict
                            && capture.ready_source.as_ref() != Some(source))
                        .then(|| capture.session_ready.clone())
                        .flatten()
                    });
                    if !capture.identity_conflict
                        && (capture.pending_identity_conflict
                            || (capture.session_ready.is_some() && conflict_session.is_some()))
                    {
                        let recorded = if capture.pending_identity_conflict {
                            store.record_provider_identity_conflict(
                                context.attempt_id,
                                &capture.route,
                                source,
                                &call,
                            )
                        } else if let Some(session) = conflict_session.as_deref() {
                            store.record_provider_session_ready(
                                context.attempt_id,
                                &capture.route,
                                session,
                                source,
                                &call,
                            )
                        } else {
                            Ok(false)
                        }
                        .unwrap_or(false);
                        if recorded {
                            // The RunStore has durably marked this identity
                            // disagreement; no later frame can make it safe again.
                            capture.identity_conflict = true;
                            capture.pending_conflict_session = None;
                            capture.pending_source_conflict = false;
                            capture.pending_identity_conflict = false;
                        }
                    }
                }
            }
            if !capture.recorded
                && let Some(ProviderEvent::QuotaLimited {
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

    fn identity_conflict_observed(&self) -> bool {
        self.identity_conflict_observed
            .load(std::sync::atomic::Ordering::Acquire)
    }
    fn identity_evidence_incomplete(&self) -> bool {
        self.identity_evidence_incomplete
            .load(std::sync::atomic::Ordering::Acquire)
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
        let roots = provenant_os::fs::bind_roots(&inputs)?;
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
    /// Select framing from the adapter bound during admission, before capture.
    pub fn stdout_filter(&self) -> impl provenant_os::exec::CaptureFilter + use<> {
        StdoutFilter {
            evidence: Arc::clone(&self.stdout_frames.evidence),
            inner: None,
        }
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
        let call = run::Request {
            clock: &clock,
            cancel: &cancel,
        };
        let record = store
            .record(context.attempt_id, &call)
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
            let continuation = store
                .continuation_expectation(context.attempt_id, &call)
                .map_err(|error| custody::CustodyError::Io(std::io::Error::other(error)))?;
            let mut stream = match continuation.as_ref() {
                Some(continuation) => {
                    let model =
                        super::stream_model(&route).ok_or(custody::CustodyError::InvalidRecord)?;
                    registration
                        .adapter
                        .stream_for_continuation(&continuation.session, &model)
                        .map_err(|error| custody::CustodyError::Io(std::io::Error::other(error)))?
                }
                None => {
                    let mut stream = registration
                        .adapter
                        .stream(None)
                        .map_err(|error| custody::CustodyError::Io(std::io::Error::other(error)))?;
                    if let Some(model) = super::stream_model(&route) {
                        stream.expect_model(&model);
                    }
                    stream
                }
            };
            stream.filtered_capture();
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
                holder_context: raw.to_vec(),
                shared: configuration_root == context.roots.state_root,
                recorded: false,
                pending_session: None,
                session_ready: None,
                pending_conflict_session: None,
                ready_source: None,
                pending_source_conflict: false,
                pending_identity_conflict: false,
                identity_conflict: false,
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
    fn outcome(
        &self,
        context: &[u8],
        attempt: &Directory,
        exit: &custody::ExitObservation,
        recovery: Option<&custody::Recovery>,
    ) -> custody::Result<()> {
        self.inner.outcome(context, attempt, exit, recovery)
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

    // Spec: docs/specs/fabric.md#session-ready-steering
    #[test]
    fn provider_identity_mismatches_are_retained_for_host_conflict_recording()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_os::capture::FrameDecoder;

        let temp = tempfile::tempdir()?;
        let attempt_path = temp.path().join("attempt");
        std::fs::create_dir(&attempt_path)?;
        let registration = &provenant_providers::agy::REGISTRATION;
        let source = provenant_os::capture::CaptureSourceIdentity {
            attempt: "fixture-attempt".into(),
            generation: 1,
            boot: "fixture-boot".into(),
            pid: 123,
            native_start: 456,
        };
        let route = run::RouteRecord {
            provider: Some("agy".into()),
            model: Some("agy/gemini-3.8-flash".into()),
            ..Default::default()
        };
        assert_eq!(
            crate::run::stream_model(&route).as_deref(),
            Some("gemini-3.8-flash")
        );
        const EXPECTED_SESSION: &str = "FIXTURE_SESSION_IDENTIFIER";
        let initial =
            include_str!("../../../../tests/fixtures/providers/agy/live-read_only-resume.jsonl")
                .lines()
                .next()
                .ok_or("Agy resume fixture has no initial frame")?;
        // Agy streams newline-delimited JSON; complete the valid producer frame
        // before injecting a later identity mismatch.
        let mut initial_frame = initial.as_bytes().to_vec();
        initial_frame.push(b'\n');

        let stream = registration
            .adapter
            .stream_for_continuation(EXPECTED_SESSION, "gemini-3.8-flash")?;
        let first_frame = QuotaFrames {
            decoder: Mutex::new(None),
            evidence: Arc::new(Mutex::new(Some(QuotaCapture {
                adapter: registration.adapter,
                stream,
                attempt: Directory::open_anchor(&attempt_path)?,
                state_root: temp.path().into(),
                route: route.clone(),
                holder_context: Vec::new(),
                shared: true,
                recorded: false,
                pending_session: None,
                session_ready: None,
                pending_conflict_session: None,
                ready_source: None,
                pending_source_conflict: false,
                pending_identity_conflict: false,
                identity_conflict: false,
            }))),
            source: Mutex::new(Some(source.clone())),
            identity_conflict_observed: std::sync::atomic::AtomicBool::new(false),
            identity_evidence_incomplete: std::sync::atomic::AtomicBool::new(false),
            identity_eof_fragment_observed: std::sync::atomic::AtomicBool::new(false),
            eof_fragment_in_progress: std::sync::atomic::AtomicBool::new(false),
            stderr: false,
        };
        let wrong_first_model =
            br#"{"event":"init","conversation_id":"FIXTURE_SESSION_IDENTIFIER","init":{"model":"other-model"}}"#;
        assert_eq!(
            first_frame.decode(wrong_first_model),
            Ok(provenant_os::capture::FrameEvent::Progress)
        );
        assert!(first_frame.identity_conflict_observed());
        assert!(
            first_frame
                .evidence
                .lock()
                .map_err(|_| "capture lock")?
                .as_ref()
                .ok_or("capture")?
                .pending_identity_conflict
        );

        let stream = registration
            .adapter
            .stream_for_continuation(EXPECTED_SESSION, "gemini-3.8-flash")?;
        let wrong_first_session = QuotaFrames {
            decoder: Mutex::new(None),
            evidence: Arc::new(Mutex::new(Some(QuotaCapture {
                adapter: registration.adapter,
                stream,
                attempt: Directory::open_anchor(&attempt_path)?,
                state_root: temp.path().into(),
                route: route.clone(),
                holder_context: Vec::new(),
                shared: true,
                recorded: false,
                pending_session: None,
                session_ready: None,
                pending_conflict_session: None,
                ready_source: None,
                pending_source_conflict: false,
                pending_identity_conflict: false,
                identity_conflict: false,
            }))),
            source: Mutex::new(Some(source.clone())),
            identity_conflict_observed: std::sync::atomic::AtomicBool::new(false),
            identity_evidence_incomplete: std::sync::atomic::AtomicBool::new(false),
            identity_eof_fragment_observed: std::sync::atomic::AtomicBool::new(false),
            eof_fragment_in_progress: std::sync::atomic::AtomicBool::new(false),
            stderr: false,
        };
        assert_eq!(
            wrong_first_session.decode(
                br#"{"event":"init","conversation_id":"fresh-session","init":{"model":"gemini-3.8-flash"}}"#
            ),
            Ok(provenant_os::capture::FrameEvent::Progress)
        );
        assert!(wrong_first_session.identity_conflict_observed());
        assert!(
            wrong_first_session
                .evidence
                .lock()
                .map_err(|_| "capture lock")?
                .as_ref()
                .ok_or("capture")?
                .pending_identity_conflict
        );

        for mismatch in [
            br#"{"event":"init","conversation_id":"another-session","init":{"model":"gemini-3.8-flash"}}"#
                .as_slice(),
            br#"{"event":"init","conversation_id":"FIXTURE_SESSION_IDENTIFIER","init":{"model":"other-model"}}"#.as_slice(),
        ] {
            let mut stream = registration
                .adapter
                .stream_for_continuation(EXPECTED_SESSION, "gemini-3.8-flash")?;
            let observed = registration.adapter.observe(&mut stream, &initial_frame)?;
            assert!(observed.contains(&ProviderEvent::Session(EXPECTED_SESSION.into())));
            assert!(observed.contains(&ProviderEvent::Model("gemini-3.8-flash".into())));
            let frames = QuotaFrames {
                decoder: Mutex::new(None),
                evidence: Arc::new(Mutex::new(Some(QuotaCapture {
                    adapter: registration.adapter,
                    stream,
                    attempt: Directory::open_anchor(&attempt_path)?,
                    state_root: temp.path().into(),
                    route: route.clone(),
                    holder_context: Vec::new(),
                    shared: true,
                    recorded: false,
                    pending_session: None,
                    session_ready: Some(EXPECTED_SESSION.into()),
                    pending_conflict_session: None,
                    ready_source: Some(source.clone()),
                    pending_source_conflict: false,
                    pending_identity_conflict: false,
                    identity_conflict: false,
                }))),
                source: Mutex::new(Some(source.clone())),
                identity_conflict_observed: std::sync::atomic::AtomicBool::new(false),
                identity_evidence_incomplete: std::sync::atomic::AtomicBool::new(false),
                identity_eof_fragment_observed: std::sync::atomic::AtomicBool::new(false),
                eof_fragment_in_progress: std::sync::atomic::AtomicBool::new(false),
                stderr: false,
            };
            assert_eq!(
                frames.decode(mismatch),
                Ok(provenant_os::capture::FrameEvent::Progress)
            );
            assert!(frames.identity_conflict_observed());
            let guard = frames.evidence.lock().map_err(|_| "capture lock")?;
            let capture = guard.as_ref().ok_or("capture")?;
            assert!(capture.pending_identity_conflict);
            assert!(!capture.identity_conflict);
        }
        let malformed = QuotaFrames {
            decoder: Mutex::new(None),
            evidence: Arc::new(Mutex::new(Some(QuotaCapture {
                adapter: registration.adapter,
                stream: registration.adapter.stream(None)?,
                attempt: Directory::open_anchor(&attempt_path)?,
                state_root: temp.path().into(),
                route,
                holder_context: Vec::new(),
                shared: true,
                recorded: false,
                pending_session: None,
                session_ready: None,
                pending_conflict_session: None,
                ready_source: None,
                pending_source_conflict: false,
                pending_identity_conflict: false,
                identity_conflict: false,
            }))),
            source: Mutex::new(Some(source)),
            identity_conflict_observed: std::sync::atomic::AtomicBool::new(false),
            identity_evidence_incomplete: std::sync::atomic::AtomicBool::new(false),
            identity_eof_fragment_observed: std::sync::atomic::AtomicBool::new(false),
            eof_fragment_in_progress: std::sync::atomic::AtomicBool::new(false),
            stderr: false,
        };
        assert_eq!(
            malformed.decode(b"not-json"),
            Ok(provenant_os::capture::FrameEvent::Progress)
        );
        assert!(!malformed.identity_conflict_observed());
        assert!(malformed.identity_evidence_incomplete());
        Ok(())
    }

    #[test]
    fn only_an_actual_eof_incomplete_record_is_a_final_fragment()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_os::capture::FrameDecoder;

        let temp = tempfile::tempdir()?;
        let attempt_path = temp.path().join("attempt");
        std::fs::create_dir(&attempt_path)?;
        let registration = &provenant_providers::agy::REGISTRATION;
        let source = provenant_os::capture::CaptureSourceIdentity {
            attempt: "fixture-attempt".into(),
            generation: 1,
            boot: "fixture-boot".into(),
            pid: 123,
            native_start: 456,
        };
        let route = run::RouteRecord {
            provider: Some("agy".into()),
            model: Some("agy/expected-model".into()),
            ..Default::default()
        };
        let make_frames = || -> Result<QuotaFrames, Box<dyn std::error::Error>> {
            let stream = registration
                .adapter
                .stream_for_continuation("s1", "expected-model")?;
            Ok(QuotaFrames {
                decoder: Mutex::new(None),
                evidence: Arc::new(Mutex::new(Some(QuotaCapture {
                    adapter: registration.adapter,
                    stream,
                    attempt: Directory::open_anchor(&attempt_path)?,
                    state_root: temp.path().into(),
                    route: route.clone(),
                    holder_context: Vec::new(),
                    shared: true,
                    recorded: false,
                    pending_session: None,
                    session_ready: None,
                    pending_conflict_session: None,
                    ready_source: None,
                    pending_source_conflict: false,
                    pending_identity_conflict: false,
                    identity_conflict: false,
                }))),
                source: Mutex::new(Some(source.clone())),
                identity_conflict_observed: std::sync::atomic::AtomicBool::new(false),
                identity_evidence_incomplete: std::sync::atomic::AtomicBool::new(false),
                identity_eof_fragment_observed: std::sync::atomic::AtomicBool::new(false),
                eof_fragment_in_progress: std::sync::atomic::AtomicBool::new(false),
                stderr: false,
            })
        };

        let partial = br#"{"event":"init","conversation_id":"s1","init":{"model":"expected-model""#;
        let init = br#"{"event":"init","conversation_id":"s1","init":{"model":"expected-model"}}"#;
        let eof = make_frames()?;
        assert_eq!(
            eof.decode(init),
            Ok(provenant_os::capture::FrameEvent::Progress)
        );
        assert_eq!(
            eof.decode_eof_fragment(partial),
            Ok(provenant_os::capture::FrameEvent::Progress)
        );
        assert!(eof.identity_eof_fragment_observed());
        assert!(!eof.identity_evidence_incomplete());
        let mut stop = super::super::tests::clean_exit();
        stop.stop_reason = Some(provenant_supervisor::machine::StopReason::Steered);
        stop.cancel_delivered_before_exit = Some(true);
        stop.provider_identity_eof_fragment = eof.identity_eof_fragment_observed();
        let mut stdout = init.to_vec();
        stdout.push(b'\n');
        stdout.extend_from_slice(partial);
        let decoded = Registry::default().decode_inner(
            &route,
            &stdout,
            &[],
            &stop,
            Path::new("/nonexistent"),
            false,
        )?;
        assert_eq!(decoded.session.as_deref(), Some("s1"));
        assert_eq!(
            decoded.reason.as_deref(),
            Some("provider output is partial")
        );
        assert_ne!(
            decoded.terminal_cause,
            Some(run::TerminalCause::IdentityMismatch)
        );

        let complete = make_frames()?;
        assert_eq!(
            complete.decode(partial),
            Ok(provenant_os::capture::FrameEvent::Progress)
        );
        assert!(!complete.identity_eof_fragment_observed());
        assert!(complete.identity_evidence_incomplete());
        let mut malformed_stop = super::super::tests::clean_exit();
        malformed_stop.stop_reason = Some(provenant_supervisor::machine::StopReason::Steered);
        malformed_stop.cancel_delivered_before_exit = Some(true);
        malformed_stop.provider_identity_evidence_incomplete =
            complete.identity_evidence_incomplete();
        let mut malformed_stdout = init.to_vec();
        malformed_stdout.push(b'\n');
        malformed_stdout.extend_from_slice(partial);
        malformed_stdout.push(b'\n');
        let decoded = Registry::default().decode_inner(
            &route,
            &malformed_stdout,
            &[],
            &malformed_stop,
            Path::new("/nonexistent"),
            false,
        )?;
        assert_eq!(
            decoded.reason.as_deref(),
            Some("evidence defect: provider protocol frame on line 2 is malformed")
        );

        let valid_tail = make_frames()?;
        assert_eq!(
            valid_tail.decode_eof_fragment(init),
            Ok(provenant_os::capture::FrameEvent::Progress)
        );
        assert!(!valid_tail.identity_eof_fragment_observed());
        assert!(!valid_tail.identity_evidence_incomplete());

        let valid_mismatch = make_frames()?;
        let wrong_model =
            br#"{"event":"init","conversation_id":"s1","init":{"model":"other-model"}}"#;
        assert_eq!(
            valid_mismatch.decode_eof_fragment(wrong_model),
            Ok(provenant_os::capture::FrameEvent::Progress)
        );
        assert!(!valid_mismatch.identity_eof_fragment_observed());
        assert!(valid_mismatch.identity_conflict_observed());
        Ok(())
    }

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
                holder_context: Vec::new(),
                shared: true,
                recorded: false,
                pending_session: None,
                session_ready: None,
                pending_conflict_session: None,
                ready_source: None,
                pending_source_conflict: false,
                pending_identity_conflict: false,
                identity_conflict: false,
            }))),
            source: Mutex::new(None),
            identity_conflict_observed: std::sync::atomic::AtomicBool::new(false),
            identity_evidence_incomplete: std::sync::atomic::AtomicBool::new(false),
            identity_eof_fragment_observed: std::sync::atomic::AtomicBool::new(false),
            eof_fragment_in_progress: std::sync::atomic::AtomicBool::new(false),
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
                holder_context: Vec::new(),
                shared: true,
                recorded: false,
                pending_session: None,
                session_ready: None,
                pending_conflict_session: None,
                ready_source: None,
                pending_source_conflict: false,
                pending_identity_conflict: false,
                identity_conflict: false,
            }))),
            source: Mutex::new(None),
            identity_conflict_observed: std::sync::atomic::AtomicBool::new(false),
            identity_evidence_incomplete: std::sync::atomic::AtomicBool::new(false),
            identity_eof_fragment_observed: std::sync::atomic::AtomicBool::new(false),
            eof_fragment_in_progress: std::sync::atomic::AtomicBool::new(false),
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

    // Spec: docs/specs/fabric.md#custody-and-cursors
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
            "schema_version":2,"graph_id":"custody",
            "nodes":[{"id":"parent","access":"read_only","prompt":"Go","route":{"alias":"senior"}}]
        }))?;
        journal.bind(GraphExecution {
            peer_origin_context: None,
            peer_submission_digest: None,
            requester_path: None,
            extra: Default::default(),
            bases: None,
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
                "schema_version":2,"graph_id":"custody","plan_digest":"fixture",
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
                    limit_extensions: Default::default(),
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
                        required_family: None,
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
