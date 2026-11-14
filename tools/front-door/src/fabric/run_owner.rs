use super::*;
use provenant_fabric_runtime::runs::SubmissionReceipt;

struct SubmissionPolicy<'a> {
    facts: &'a provenant_confinement::worktrees::RepoFacts,
    access: provenant_records::run::LaneAccess,
    settlement_retry_limit: Option<u8>,
    attempt: Option<provenant_fabric_runtime::graph::AttemptSpec>,
    refusal: provenant_run::RefusalContext,
}

pub(crate) struct BinaryRunOwner<'a> {
    pub(crate) factory: &'a Factory,
    /// False only at peer ingress: no forwarding, and writers name a new
    /// worktree instead of a path.
    pub(crate) allow_remote: bool,
    /// Replay submissions under this actor instead of the process's own.
    pub(crate) actor: Option<provenant_core::ActorId>,
    /// Only the direct chair entry point supplies a verified origin.
    pub(crate) chair: Option<provenant_records::custody::Chair>,
}
/// Retain admitted preparation records when the CLI projects a launch refusal.
/// Callers still classify the durable records with `BinaryRunOwner::receipt`.
pub(crate) fn recover_accepted_launch(
    session: &provenant_cli::run::Session,
    operation_id: &provenant_core::OperationId,
    call: &provenant_run::Request<'_>,
    submitted: Result<
        (provenant_run::AttemptRecord, Vec<provenant_run::Started>),
        provenant_cli::run::Fail,
    >,
) -> Result<(provenant_run::AttemptRecord, Vec<provenant_run::Started>), provenant_cli::run::Fail> {
    match submitted {
        // The CLI reports an all-refused launch batch as an error even
        // after admitting its terminal preparation records. The graph
        // must observe those records, including typed fallback evidence;
        // receipt() still rejects sealed policy refusals and custody.
        Err(fail) if fail.kind == provenant_run::ErrorKind::Refused => {
            match session.store.operation(operation_id, call)? {
                Some(operation)
                    if matches!(
                        operation.state,
                        provenant_run::OperationState::Accepted
                            | provenant_run::OperationState::Terminal
                    ) =>
                {
                    let admitted = session.store.operation_attempt(operation_id, call)?;
                    Ok((session.store.record(admitted, call)?, Vec::new()))
                }
                _ => Err(fail),
            }
        }
        result => result,
    }
}

pub(crate) fn peer_result<T>(
    result: Result<T, FacadeError>,
    wrap: impl FnOnce(T) -> PeerResult,
) -> PeerResult {
    match result {
        Ok(value) => wrap(value),
        Err(error) => PeerResult::Error {
            data: provenant_fabric_runtime::hosts::PeerResultError {
                extra: Default::default(),
                error,
            },
            extra: Default::default(),
        },
    }
}
/// How many leading events fit in a JSON array of at most `max_bytes`.
pub(crate) fn events_within<T: serde::Serialize>(
    events: &[T],
    max_bytes: u64,
) -> Result<usize, FacadeError> {
    let mut size = 2;
    for (index, event) in events.iter().enumerate() {
        let len = serde_json::to_vec(event)
            .map_err(|_| FacadeError::StateCorrupt)?
            .len() as u64;
        size += len + u64::from(index > 0);
        if size > max_bytes {
            return Ok(index);
        }
    }
    Ok(events.len())
}
pub(crate) fn run_error(error: provenant_run::RunError) -> FacadeError {
    // Preserve resource contention through the public error boundary; an
    // immediate repository probe must schedule a retry, not fail the writer.
    if matches!(
        &error,
        provenant_run::RunError::Filesystem(provenant_os::fs::FsError::LockTimeout { .. })
    ) {
        return FacadeError::Busy;
    }
    if matches!(
        &error,
        provenant_run::RunError::Writer(
            provenant_confinement::worktrees::WorktreeError::LockBusy
                | provenant_confinement::worktrees::WorktreeError::RegistryLockTimeout
        )
    ) {
        return FacadeError::WriterLockBusy;
    }
    if let provenant_run::RunError::Writer(writer) = &error
        && let Some((operation, lock_failure)) = writer.mutation_failure()
    {
        return FacadeError::GitMutationFailed {
            operation,
            lock_failure,
        };
    }

    if matches!(
        &error,
        provenant_run::RunError::Writer(
            provenant_confinement::worktrees::WorktreeError::CheckedOut
        )
    ) {
        return FacadeError::DispatchRefused {
            fact: Some(provenant_fabric_runtime::coordination::facade::DispatchRefusalFact::CheckedOutWriter),
            disk: None,
            reason: String::new(),
        };
    }
    if matches!(
        &error,
        provenant_run::RunError::Writer(provenant_confinement::worktrees::WorktreeError::Dirty(_))
    ) {
        return FacadeError::DispatchRefused {
            fact: Some(
                provenant_fabric_runtime::coordination::facade::DispatchRefusalFact::DirtyWriter,
            ),
            disk: None,
            reason: String::new(),
        };
    }
    // The unresolvable ref is the caller's own input; name it instead of
    // collapsing to a bare "invalid input".
    if let provenant_run::RunError::InvalidBase(base) = &error {
        return FacadeError::invalid(
            provenant_fabric_runtime::coordination::facade::InvalidInputDetail::UnresolvableBase(
                base.clone(),
            ),
        );
    }
    if let provenant_run::RunError::Writer(
        provenant_confinement::worktrees::WorktreeError::WriterBusy { holder },
    ) = &error
    {
        return FacadeError::WriterBusy { holder: *holder };
    }
    fail_error(error.into())
}
pub(crate) fn fail_error(fail: provenant_cli::run::Fail) -> FacadeError {
    use provenant_records::diagnostic::DiagnosticFact;
    let fact = fail
        .projection
        .as_ref()
        .and_then(|value| value.fact.as_ref());
    if fail.detail.is_none() {
        match fact {
            Some(DiagnosticFact::CredentialGrant { access }) => {
                return FacadeError::CredentialRoot {
                    access: (*access).into(),
                };
            }
            Some(DiagnosticFact::DeliveryLimit(fact)) => {
                return FacadeError::HostDeliveryLimit { fact: fact.clone() };
            }
            Some(DiagnosticFact::DeliveryStage(stage)) => {
                return FacadeError::HostDeliveryStage { stage: *stage };
            }
            Some(DiagnosticFact::GenerationConflict(current_generation)) => {
                return FacadeError::GenerationConflict {
                    current_generation: *current_generation,
                };
            }
            Some(DiagnosticFact::WorktreeUnregistered) => {
                return FacadeError::WorktreeUnregistered {
                    path: "inspect worktree registration".into(),
                };
            }
            Some(DiagnosticFact::Filesystem { operation, cause }) => {
                return FacadeError::io_caused(*operation, *cause);
            }
            Some(DiagnosticFact::DiskPressure {
                available,
                floor,
                reclaimed,
                origin,
            }) => {
                return FacadeError::DispatchRefused {
                    fact: None,
                    reason: "insufficient disk space".into(),
                    disk: Some(
                        provenant_fabric_runtime::coordination::facade::DiskPressure {
                            available: *available,
                            floor: *floor,
                            reclaimed: *reclaimed,
                            origin: *origin,
                        },
                    ),
                };
            }
            Some(DiagnosticFact::DiskUnobserved) => {
                return FacadeError::DispatchUnobserved {
                    reason: "disk observation failed".into(),
                };
            }
            Some(DiagnosticFact::PromptLimit { limit }) => {
                let field = "prompt".to_owned();
                return usize::try_from(*limit).map_or(FacadeError::InvalidInput { detail: None }, |limit| {
                    FacadeError::invalid(provenant_fabric_runtime::coordination::facade::InvalidInputDetail::FieldTooLarge { field, limit })
                });
            }
            Some(DiagnosticFact::Resource(fact)) => {
                if matches!(
                    fact,
                    provenant_records::diagnostic::ResourceFact::Processes { .. }
                ) {
                    return FacadeError::Io {
                        operation: "run operation".into(),
                        path: None,
                        delivery_limit: None,
                        delivery_stage: None,
                        resource: Some(Box::new(*fact)),
                        input_failure: None,
                        cause: None,
                    };
                }
                return FacadeError::ConfinementRefused {
                    resource: Some(*fact),
                };
            }
            None => {}
        }
    }
    match fail.detail.map(|detail| *detail) {
        Some(FacadeError::WorktreeUnregistered { .. }) => FacadeError::WorktreeUnregistered {
            path: "inspect worktree registration".into(),
        },
        Some(FacadeError::Io {
            operation,
            delivery_limit,
            delivery_stage,
            resource,
            input_failure,
            cause,
            ..
        }) => FacadeError::Io {
            delivery_limit,
            delivery_stage,
            resource,
            input_failure,
            cause,
            operation,
            path: None,
        },
        Some(detail) => detail,
        None => facade_error_for_kind(fail.kind),
    }
}
/// Translate a submission's refusal at the facade edge. Dispatch access
/// failures keep their own message; rendering it by kind would read as
/// invalid input instead of a refusal.
pub(crate) fn dispatch_refused(fail: provenant_cli::run::Fail) -> FacadeError {
    if let Some(provenant_records::diagnostic::DiagnosticFact::Resource(fact)) = fail
        .projection
        .as_ref()
        .and_then(|projection| projection.fact.as_ref())
    {
        return FacadeError::DispatchRefused {
            fact: Some(
                provenant_fabric_runtime::coordination::facade::DispatchRefusalFact::Resource(
                    *fact,
                ),
            ),
            disk: None,
            reason: "dispatch was refused".into(),
        };
    }
    if matches!(
        fail.projection
            .as_ref()
            .and_then(|projection| projection.fact.as_ref()),
        Some(provenant_records::diagnostic::DiagnosticFact::DiskPressure { .. })
    ) {
        return fail_error(fail);
    }
    let reason = fail
        .projection
        .as_ref()
        .and_then(|projection| projection.detail.as_deref())
        .filter(|detail| !detail.is_empty())
        .unwrap_or(&fail.message);
    let reason = provenant_confinement::redact::bounded_line(reason, 4096);
    match fail.detail.map(|detail| *detail) {
        Some(detail @ FacadeError::DispatchRefused { .. }) => detail,
        _ => FacadeError::DispatchRefused {
            fact: None,
            disk: None,
            reason,
        },
    }
}
/// Translate a typed run category at the command edge. `ErrorKind` is the one
/// categorisation of run errors; this only renders it.
pub(crate) fn facade_error_for_kind(kind: provenant_run::ErrorKind) -> FacadeError {
    use provenant_run::ErrorKind as E;
    match kind {
        E::SecretInput
        | E::Refused
        | E::Unsupported
        | E::WriterScope
        | E::Confinement
        | E::DiskFloor
        | E::MemoryPressure
        | E::LaneCap
        | E::BuildSettings
        | E::GuardianIdentity => FacadeError::ConfinementRefused { resource: None },
        E::WriterBusy => FacadeError::WriterBusy { holder: None },
        E::WriterLockBusy => FacadeError::WriterLockBusy,
        E::WriterUnknown => FacadeError::WriterUnknown,
        E::CredentialRoot => FacadeError::CredentialRoot {
            access: "requested".into(),
        },
        E::LaneDepth => FacadeError::LaneDepth,
        E::LaneContext | E::ParentAttempt | E::Invalid | E::WorktreeRequired => {
            FacadeError::InvalidInput { detail: None }
        }
        E::WorktreeNotApplicable => FacadeError::WorktreeNotApplicable,
        E::UnknownHost => FacadeError::UnknownHost,
        E::WorktreeMissing => FacadeError::WorktreeMissing,
        E::NotFound => FacadeError::NotFound,
        E::Conflict => FacadeError::Conflict,
        E::OperationConflict => FacadeError::OperationConflict,
        E::Nonterminal => FacadeError::NotTerminal,
        E::Cancelled => FacadeError::Cancelled,
        E::Timeout => FacadeError::Timeout,
        E::QueueFull | E::Contention => FacadeError::Busy,
        E::ContinuationUnsupported => FacadeError::ContinuationUnsupported,
        E::ProviderStateExpired => FacadeError::ProviderStateExpired,
        E::OwnerUnavailable => FacadeError::OwnerUnavailable,
        // An unregistered worktree must arrive with typed detail and its path.
        E::StateCorrupt | E::WorktreeUnregistered => FacadeError::StateCorrupt,
        E::BaseUnreachable => FacadeError::BaseUnreachable,
        E::BranchExists => FacadeError::BranchExists,
        E::LaneCoordinationRefused => FacadeError::LaneCoordinationRefused,
        E::UpgradeRefused => FacadeError::UpgradeRefused,
        E::PeerVersion => FacadeError::PeerVersion,
        _ => FacadeError::io("run operation"),
    }
}
fn terminal_cause(
    lane: &provenant_run::AttemptRecord,
) -> Option<provenant_records::run::TerminalCause> {
    match &lane.state {
        provenant_records::run::AttemptState::Terminal { extra, .. } => extra
            .get("terminal_cause")
            .and_then(|value| serde_json::from_value(value.clone()).ok()),
        _ => None,
    }
}
/// Only positive sealed evidence of a pre-launch refusal can reject a replay.
pub(crate) fn durable_launch_refusal(
    lanes: &[provenant_run::AttemptRecord],
) -> Option<provenant_core::AttemptId> {
    use provenant_records::run::{AttemptState, SettlementReason, TerminalCause};
    (!lanes.is_empty()
        && lanes.iter().all(|lane| {
            lane.provider_launched == Some(false)
                && lane.settlement_reason == Some(SettlementReason::PreLaunchRefusal)
                && matches!(lane.state, AttemptState::Terminal { .. })
                && (terminal_cause(lane).is_some_and(|cause| {
                    matches!(
                        cause,
                        TerminalCause::DiskFloor
                            | TerminalCause::DiskObservationUnavailable
                            | TerminalCause::RetainedWriterImport
                    )
                }) || lane.skipped_routes.iter().any(|route| {
                    route.cause == Some(provenant_records::RouteExclusionReason::PacingReserve)
                }))
        }))
    .then(|| lanes[0].attempt_id)
}

/// Whether a launch batch is refused, judged identically on first call and
/// replay. Preparation failures are accepted terminal attempts: graph routing
/// observes their typed outcome and may advance to a fallback route. Only
/// retained custody, or sealed pre-launch evidence whose typed cause names the
/// refusal, refuses. Reasons carry typed text, never a refusal's source text.
pub(crate) fn launch_refusal(
    started: &[provenant_run::Started],
    lanes: &[provenant_run::AttemptRecord],
) -> Option<FacadeError> {
    let blocked = started
        .iter()
        .filter_map(|item| match item {
            provenant_run::Started::Blocked { attempt, .. } => Some(*attempt),
            _ => None,
        })
        .collect::<Vec<_>>();
    if !blocked.is_empty()
        && started.iter().all(|item| {
            matches!(
                item,
                provenant_run::Started::Blocked { .. } | provenant_run::Started::Refused { .. }
            )
        })
    {
        return Some(FacadeError::DispatchRefused {
            fact: None,
            disk: None,
            reason: blocked
                .iter()
                .map(|attempt| format!("{attempt}: launch blocked by retained custody"))
                .collect::<Vec<_>>()
                .join("; "),
        });
    }
    let sealed: Vec<provenant_run::AttemptRecord> = if started.is_empty() {
        lanes.to_vec()
    } else if started.iter().all(|item| {
        matches!(
            item,
            provenant_run::Started::Refused {
                kind: provenant_run::RefusalKind::Policy
                    | provenant_run::RefusalKind::PolicyResource(_)
                    | provenant_run::RefusalKind::PacingReserve
                    | provenant_run::RefusalKind::PacingReserveResource(_)
                    | provenant_run::RefusalKind::RetainedImport { .. },
                ..
            }
        )
    }) {
        lanes
            .iter()
            .filter(|lane| {
                started.iter().any(|item| {
                    matches!(item, provenant_run::Started::Refused { attempt, .. } if *attempt == lane.attempt_id)
                })
            })
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    durable_launch_refusal(&sealed)?;
    let pacing = sealed
        .iter()
        .flat_map(|lane| &lane.skipped_routes)
        .find_map(|route| {
            if route.cause != Some(provenant_records::RouteExclusionReason::PacingReserve) {
                return None;
            }
            match route.diagnostic().fact {
                Some(provenant_records::diagnostic::DiagnosticFact::Resource(
                    fact @ provenant_records::diagnostic::ResourceFact::PacingReserve { .. },
                )) => Some(fact),
                _ => None,
            }
        });
    Some(FacadeError::DispatchRefused {
        fact: pacing
            .map(provenant_fabric_runtime::coordination::facade::DispatchRefusalFact::Resource),
        disk: None,
        reason: sealed
            .iter()
            .map(|lane| {
                format!(
                    "{}: {}",
                    lane.attempt_id,
                    terminal_cause(lane)
                        .map(|cause| cause.detail())
                        .unwrap_or("pacing reserve refused")
                )
            })
            .collect::<Vec<_>>()
            .join("; "),
    })
}

impl BinaryRunOwner<'_> {
    pub(crate) fn call<'a>(&'a self, cancel: &'a CancelToken) -> provenant_run::Request<'a> {
        provenant_run::Request {
            clock: &self.factory.clock,
            cancel,
        }
    }
    pub(crate) fn session(
        &self,
        cancel: &CancelToken,
    ) -> Result<provenant_cli::run::Session, FacadeError> {
        let ctx = &self.factory.context;
        let context = provenant_run::ExecutionContext {
            state_root: ctx.roots.state.clone(),
            project_id: ctx.identity.project,
            actor_id: self.actor.unwrap_or(ctx.identity.inbox.actor),
            read_prompts: true,
        };
        let store = provenant_run::RunStore::open(&context, &self.call(cancel))
            .map_err(run_error)?
            .with_host(ctx.identity.host);
        Ok(provenant_cli::run::Session { store, context })
    }
    pub(crate) fn session_with_facts(
        &self,
        cancel: &CancelToken,
        facts: &provenant_confinement::worktrees::RepoFacts,
    ) -> Result<provenant_cli::run::Session, FacadeError> {
        let mut session = self.session(cancel)?;
        session.store = session.store.with_repo_facts(facts);
        Ok(session)
    }

    pub(crate) fn store_with_facts(
        &self,
        cancel: &CancelToken,
        facts: &provenant_confinement::worktrees::RepoFacts,
    ) -> Result<provenant_run::RunStore, FacadeError> {
        Ok(self.session_with_facts(cancel, facts)?.store)
    }

    /// Whether the node that admitted this launch's node floors it; see
    /// [`admitter_floors`].
    fn admitter_floors_launch(
        &self,
        task: &provenant_fabric_runtime::coordination::facade::SubmittedTask,
        attempt: Option<&provenant_fabric_runtime::graph::AttemptSpec>,
        cancel: &CancelToken,
    ) -> bool {
        let Some(admitter) = attempt.and_then(|spec| spec.admitted_by.as_deref()) else {
            return false;
        };
        let ctx = &self.factory.context;
        let graph = task
            .context
            .as_ref()
            .and_then(|context| context.graph_id.as_deref());
        let mut store = None;
        admitter_floors(
            admitter,
            || {
                provenant_fabric_runtime::graph::FileJournal::read_checkpoint(
                    &ctx.roots.state,
                    ctx.identity.project,
                    graph?,
                )
                .ok()
                .flatten()
            },
            &ctx.identity.host.to_string(),
            &mut |id| {
                store
                    .get_or_insert_with(|| self.read_session(cancel).ok())
                    .as_ref()
                    .is_some_and(|session| {
                        session
                            .store
                            .attempt_floors_children(id, &self.call(cancel))
                    })
            },
        )
    }

    pub(crate) fn read_session(
        &self,
        cancel: &CancelToken,
    ) -> Result<provenant_cli::run::Session, FacadeError> {
        let ctx = &self.factory.context;
        let context = provenant_run::ExecutionContext {
            state_root: ctx.roots.state.clone(),
            project_id: ctx.identity.project,
            actor_id: self.actor.unwrap_or(ctx.identity.inbox.actor),
            read_prompts: true,
        };
        let store = provenant_run::RunStore::open_read_only(&context, &self.call(cancel))
            .map_err(run_error)?
            .ok_or(FacadeError::NotFound)?
            .with_host(ctx.identity.host);
        Ok(provenant_cli::run::Session { store, context })
    }
    fn require_graph_control(
        &self,
        target: &AttemptTarget,
        cancel: &CancelToken,
    ) -> Result<(), FacadeError> {
        let record = self
            .read_session(cancel)?
            .service()
            .status_for_wait(target.attempt, &self.call(cancel))
            .map_err(run_error)?;
        require_graph_context(record.context.as_ref())
    }
    pub(crate) fn store(
        &self,
        cancel: &CancelToken,
    ) -> Result<provenant_run::RunStore, FacadeError> {
        Ok(self.session(cancel)?.store)
    }
    /// Run the shared launch path with this process's resolved roots.
    pub(crate) fn with_edge<T>(
        &self,
        action: impl FnOnce(&provenant_cli::run::Edge<'_>) -> Result<T, provenant_cli::run::Fail>,
    ) -> Result<T, FacadeError> {
        action(&provenant_cli::run::Edge {
            roots: &self.factory.context.roots,
            home: &self.factory.home,
            assets: &self.factory.assets,
            cwd: &self.factory.cwd,
        })
        .map_err(fail_error)
    }
    /// The attempt this owner just accepted or continued.
    pub(crate) fn receipt(
        &self,
        session: &provenant_cli::run::Session,
        operation_id: provenant_core::OperationId,
        attempt: &provenant_run::AttemptRecord,
        started: &[provenant_run::Started],
        call: &provenant_run::Request<'_>,
    ) -> Result<SubmissionReceipt, FacadeError> {
        // An uncertain handoff is reconciled by receipt lookup, never relaunched.
        if started
            .iter()
            .any(|s| matches!(s, provenant_run::Started::Unknown { .. }))
        {
            return Err(FacadeError::LaunchUnknown { operation_id });
        }
        let mut lanes = Vec::new();
        let mut after = 0;
        loop {
            let page = session
                .service()
                .lanes(
                    &provenant_run::LaneQuery {
                        targets: vec![attempt.attempt_id],
                        all: true,
                        limit: 100,
                        after,
                        newest_first: false,
                        owner: None,
                        unseen_after: None,
                    },
                    call,
                )
                .map_err(run_error)?;
            lanes.extend(page.lanes);
            match page.next {
                Some(next) => after = next,
                None => break,
            }
        }
        if let Some(refusal) = launch_refusal(started, &lanes) {
            return Err(refusal);
        }
        if started.is_empty()
            && session
                .store
                .operation(&operation_id, call)
                .map_err(run_error)?
                .is_some_and(|operation| operation.state == provenant_run::OperationState::Blocked)
        {
            return Err(FacadeError::DispatchRefused {
                fact: None,
                disk: None,
                reason: "dispatch remains blocked by retained custody; reconcile before retrying"
                    .into(),
            });
        }
        Ok(SubmissionReceipt {
            extra: Default::default(),
            worktree: None,
            operation_id,
            targets: lanes
                .iter()
                .map(|lane| AttemptTarget {
                    extra: Default::default(),
                    attempt: lane.attempt_id,
                    host: Some(self.factory.context.identity.host),
                })
                .collect(),
        })
    }
    /// A local attempt, or the forwarded answer for another host's attempt.
    pub(crate) fn remote(
        &self,
        target: &AttemptTarget,
        action: PeerAction,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<Option<PeerResult>, FacadeError> {
        match target.host {
            Some(host) if host != self.factory.context.identity.host => {
                if !self.allow_remote {
                    return Err(FacadeError::ConfinementRefused { resource: None });
                }
                self.peer(host, action, budget, cancel).map(Some)
            }
            _ => Ok(None),
        }
    }
    pub(crate) fn snapshot(
        &self,
        target: AttemptTarget,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<provenant_records::run::AttemptRecord, FacadeError> {
        let identity = &self.factory.context.identity;
        if target.host.is_some_and(|h| h != identity.host) {
            if !self.allow_remote {
                return Err(FacadeError::ConfinementRefused { resource: None });
            }
            let host = target
                .host
                .ok_or(FacadeError::InvalidInput { detail: None })?;
            let attempt_id = target.attempt;
            return match self.peer(
                host,
                PeerAction::Run {
                    extra: Default::default(),
                    input: RunInput::Status {
                        extra: Default::default(),
                        target,
                    },
                },
                budget,
                cancel,
            )? {
                PeerResult::Run { data: value, .. } => match *value {
                    RunResult::Status { attempt, .. } if attempt.attempt_id == attempt_id => {
                        Ok(attempt)
                    }
                    _ => Err(FacadeError::StateCorrupt),
                },
                _ => Err(FacadeError::StateCorrupt),
            };
        }
        provenant_run::Service::new(&self.read_session(cancel)?.store)
            .status(
                target.attempt,
                &provenant_run::Request {
                    clock: &self.factory.clock,
                    cancel,
                },
            )
            .map_err(run_error)
    }
    pub(crate) fn snapshots_local(
        &self,
        targets: &[AttemptTarget],
        cancel: &CancelToken,
    ) -> Result<Vec<provenant_records::run::AttemptRecord>, FacadeError> {
        let host = self.factory.context.identity.host;
        if targets
            .iter()
            .any(|target| target.host.is_some_and(|target_host| target_host != host))
        {
            return Err(FacadeError::InvalidInput { detail: None });
        }
        let session = self.read_session(cancel)?;
        let call = self.call(cancel);
        targets
            .iter()
            .map(|target| {
                let record = session
                    .service()
                    .status_for_wait(target.attempt, &call)
                    .map_err(run_error)?;
                Ok(record)
            })
            .collect()
    }
    /// This host's lanes for `PeerAction::Lanes`: active ones oldest first,
    /// or with `all` the newest retained ones.
    pub(crate) fn lanes_local(
        &self,
        all: bool,
        limit: u16,
        cancel: &CancelToken,
    ) -> Result<Vec<provenant_records::run::AttemptRecord>, FacadeError> {
        if !(1..=100).contains(&limit) {
            return Err(FacadeError::InvalidInput { detail: None });
        }
        let store = self.read_session(cancel)?.store;
        let page = provenant_run::Service::new(&store)
            .lanes_for_wait(
                &provenant_run::LaneQuery {
                    targets: Vec::new(),
                    all,
                    limit: u64::from(limit),
                    after: 0,
                    newest_first: all,
                    owner: None,
                    unseen_after: None,
                },
                &self.call(cancel),
            )
            .map_err(run_error)?;
        Ok(page.lanes)
    }
    /// The wait of each queued or starting lane among `targets`, for `PeerAction::LaneWaits`.
    pub(crate) fn waits_local(
        &self,
        targets: &[AttemptTarget],
        cancel: &CancelToken,
    ) -> Result<Vec<provenant_records::run::LaneWait>, FacadeError> {
        let lanes = self.snapshots_local(targets, cancel)?;
        let store = self.read_session(cancel)?.store;
        Ok(store.lane_waits(&lanes, &self.call(cancel)))
    }
    pub(crate) fn peer(
        &self,
        host: provenant_core::HostId,
        action: PeerAction,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<PeerResult, FacadeError> {
        self.factory
            .context
            .peer_with(
                &self.factory.executor,
                &self.factory.clock,
                host,
                action,
                budget,
                cancel,
            )
            .map_err(|failure| failure.error)
    }
}
impl BinaryRunOwner<'_> {
    pub(crate) fn capabilities(&self) -> RunCapabilities {
        RunCapabilities {
            // Peer submission admits graph attempts even though the legacy
            // top-level run mutation tools have been removed.
            submit: true,
            list: true,
            status: true,
            wait: true,
            cancel: true,
            resume: false,
            output: true,
            events: true,
        }
    }
    /// Graph nodes use the same validated, sealed access grants as CLI lanes.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn submit_with_access(
        &mut self,
        input: SubmitInput,
        budget: WaitBudget,
        cancel: &CancelToken,
        access: provenant_records::run::LaneAccess,
        settlement_retry_limit: Option<u8>,
        attempt: Option<provenant_fabric_runtime::graph::AttemptSpec>,
        facts: &provenant_confinement::worktrees::RepoFacts,
        refusal: provenant_run::RefusalContext,
    ) -> Result<SubmissionReceipt, FacadeError> {
        self.launch_graph_attempt(
            input,
            budget,
            cancel,
            SubmissionPolicy {
                facts,
                access,
                settlement_retry_limit,
                attempt,
                refusal,
            },
        )
        .map_err(|error| scope_io(error, "submit"))
    }

    /// Read an accepted graph operation in the authenticated owner's namespace.
    pub(crate) fn retained_graph_receipt(
        &self,
        operation_id: &provenant_core::OperationId,
        cancel: &CancelToken,
    ) -> Result<Option<SubmissionReceipt>, FacadeError> {
        let session = match self.read_session(cancel) {
            Ok(session) => session,
            Err(FacadeError::NotFound) => return Ok(None),
            Err(error) => return Err(error),
        };
        let call = self.call(cancel);
        let Some(operation) = session
            .store
            .operation(operation_id, &call)
            .map_err(run_error)?
            .filter(|operation| {
                !matches!(
                    operation.state,
                    provenant_run::OperationState::Prepared
                        | provenant_run::OperationState::Refused
                )
            })
        else {
            return Ok(None);
        };
        let admitted = session
            .store
            .operation_attempt(&operation.operation_id, &call)
            .map_err(run_error)?;
        let request = session.store.request(admitted, &call).map_err(run_error)?;
        if request.task.context.as_ref().is_none_or(|context| {
            !context.graph_template_launch
                || context.graph_id.as_ref().is_none_or(|id| id.is_empty())
                || context.node_id.as_ref().is_none_or(|id| id.is_empty())
        }) {
            return Ok(None);
        }
        let page = session
            .service()
            .lanes(
                &provenant_run::LaneQuery {
                    targets: vec![admitted],
                    all: true,
                    limit: 100,
                    after: 0,
                    newest_first: false,
                    owner: None,
                    unseen_after: None,
                },
                &call,
            )
            .map_err(run_error)?;
        if page.lanes.is_empty() {
            return Ok(None);
        }
        Ok(Some(SubmissionReceipt {
            extra: Default::default(),
            operation_id: operation_id.clone(),
            worktree: None,
            targets: page
                .lanes
                .into_iter()
                .map(|attempt| AttemptTarget {
                    extra: Default::default(),
                    host: Some(self.factory.context.identity.host),
                    attempt: attempt.attempt_id,
                })
                .collect(),
        }))
    }

    #[allow(clippy::disallowed_methods)] // Translate the caller's remaining wait to the launcher's deadline.
    fn launch_graph_attempt(
        &mut self,
        input: SubmitInput,
        budget: WaitBudget,
        cancel: &CancelToken,
        policy: SubmissionPolicy<'_>,
    ) -> Result<SubmissionReceipt, FacadeError> {
        use provenant_cli::run::{Submission, Submitted, worktree_access};
        let SubmissionPolicy {
            facts,
            access,
            settlement_retry_limit,
            mut attempt,
            refusal,
        } = policy;
        let SubmitInput::Submit {
            operation_id,
            origin_work_key,
            task,
            knobs,
            host,
            ..
        } = input;
        if task
            .context
            .as_ref()
            .is_none_or(|context| context.graph_id.is_none() || context.node_id.is_none())
        {
            let invalid =
                || FacadeError::invalid(InvalidInputDetail::Field("graph context".into()));
            let session = match self.read_session(cancel) {
                Ok(session) => session,
                Err(FacadeError::NotFound) => return Err(invalid()),
                Err(error) => return Err(error),
            };
            let call = self.call(cancel);
            let retained = session
                .store
                .operation(&operation_id, &call)
                .map_err(run_error)?;
            if retained.is_none_or(|operation| {
                matches!(operation.state, provenant_run::OperationState::Prepared)
            }) {
                return Err(invalid());
            }
            // An accepted pre-cutover operation may have no context. The sealed
            // request digest below still rejects any change to its launch inputs.
        }
        // Replays use sealed admission facts, never a fresh checkpoint lookup.
        let floored = !attempt.as_ref().is_some_and(|spec| spec.policy_sealed)
            && self.admitter_floors_launch(&task, attempt.as_ref(), cancel);
        let spec = attempt.get_or_insert_with(Default::default);
        spec.parent_sandboxed |= floored;
        let directory = task
            .worktree
            .as_deref()
            .map(Path::new)
            .or_else(|| spec.directory.as_deref().map(Path::new))
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.factory.cwd.clone());
        provenant_cli::run::seal_launch_policy(
            spec,
            &directory,
            &task.prompt,
            &access,
            task.base.as_deref(),
            task.head.as_deref(),
        )
        .map_err(fail_error)?;
        if let Some(host) = host.filter(|host| *host != self.factory.context.identity.host) {
            if !self.allow_remote
                || task.access != provenant_fabric_runtime::graph::AccessMode::ReadOnly
                || task.worktree.is_some()
                || task.new_worktree.is_some()
            {
                return Err(FacadeError::ConfinementRefused { resource: None });
            }
            // Filesystem grants belong to this machine. The peer binds its own
            // source checkout and never accepts caller-supplied absolute paths.
            if !access.attachments.is_empty()
                || !access.read_roots.is_empty()
                || !access.write_roots.is_empty()
                || access.database_ipc
                || access.inputs_root.is_some()
                || access.source_live
            {
                return Err(FacadeError::ConfinementRefused { resource: None });
            }
            let mut attempt = attempt.unwrap_or_default();
            attempt.host = None;
            attempt.parent_sandboxed |= floored;
            let mut extra = provenant_records::ExtraFields::default();
            extra.insert(
                "graph_launch_actor".into(),
                serde_json::json!(
                    self.actor
                        .unwrap_or(self.factory.context.identity.inbox.actor)
                ),
            );
            extra.insert(
                "graph_attempt".into(),
                serde_json::to_value(attempt).map_err(|_| FacadeError::StateCorrupt)?,
            );
            extra.insert(
                "graph_retry_limit".into(),
                serde_json::json!(settlement_retry_limit),
            );
            extra.insert(
                "graph_network".into(),
                serde_json::to_value(access.network).map_err(|_| FacadeError::StateCorrupt)?,
            );
            let input = Box::new(SubmitInput::Submit {
                extra: Default::default(),
                origin_work_key: Some(origin_work_key.ok_or(FacadeError::StateCorrupt)?),
                operation_id: operation_id.clone(),
                task,
                knobs,
                host: None,
            });
            return match self.peer(host, PeerAction::Submit { extra, input }, budget, cancel)? {
                PeerResult::Submitted { data, .. }
                    if data.operation_id == operation_id
                        && data.worktree.is_none()
                        && !data.targets.is_empty()
                        && data.targets.iter().all(|target| target.host == Some(host)) =>
                {
                    Ok(data)
                }
                _ => Err(FacadeError::StateCorrupt),
            };
        }
        // Graph worktree and placement are resolved before the sealed attempt reaches this owner.
        if host.is_some_and(|host| host != self.factory.context.identity.host)
            || task.new_worktree.is_some()
        {
            return Err(FacadeError::invalid(InvalidInputDetail::Field(
                "attempt placement".into(),
            )));
        }
        let launch_directory = task
            .worktree
            .as_deref()
            .map(Path::new)
            .filter(|path| path.is_absolute())
            .map(Path::to_path_buf)
            .or_else(|| {
                attempt
                    .as_ref()
                    .and_then(|spec| spec.directory.as_deref())
                    .map(PathBuf::from)
            })
            .unwrap_or_else(|| self.factory.cwd.clone());
        let submitted = {
            worktree_access(
                task.access == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite,
                false,
                task.worktree.as_deref().map(Path::new),
            )
            .map_err(fail_error)?;
            Submitted {
                attempt: {
                    let mut spec = attempt.clone().unwrap_or_else(|| {
                        provenant_fabric_runtime::graph::AttemptSpec {
                            account: task.account,
                            build_heavy: task.build_heavy,
                            ..Default::default()
                        }
                    });
                    spec.parent_sandboxed |= floored;
                    spec
                },
                require_skill_location: task.require_skill_location,
                settlement_retry_limit,
                context: task.context,
                base: task.base,
                pinned_base: None,
                head: task.head,
                source_head: task.source_head,
                prompt: task.prompt,
                title: task.title,
                route: task.route,
                provider_timeout: task.timeout_seconds,
                delivery_id: task.delivery_id,
                deliver: task.deliver,
                worktree: task.worktree.map(PathBuf::from),
                access: access.clone(),
            }
        };
        let session = self.session_with_facts(cancel, facts)?;
        let call = self.call(cancel);
        let mut host_launch = match &self.factory.host_launch {
            Some(host) => host.clone(),
            None => super::settlement::recovery_launch(
                self.factory.context.roots.state.as_path(),
                &self.factory.cwd,
            )
            .map_err(|_| FacadeError::io("resolve host graph launch context"))?,
        };
        host_launch.graph_directory = Some(self.factory.cwd.clone());
        host_launch.cwd = launch_directory.clone();
        let submission = self.with_edge(|edge| {
            let selected_edge = provenant_cli::run::Edge {
                roots: edge.roots,
                home: edge.home,
                assets: edge.assets,
                cwd: &launch_directory,
            };
            let submitted = provenant_cli::run::submit_with_host_for_refusal(
                &session,
                &selected_edge,
                Submission {
                    chair: self.chair.clone(),
                    operation_id: operation_id.clone(),
                    task: submitted,
                    knobs,
                },
                &call,
                Some(&host_launch),
                Some(
                    std::time::Instant::now()
                        + std::time::Duration::from_micros(
                            budget.remaining(self.factory.clock.mono_now()).as_micros(),
                        ),
                ),
                refusal,
            );
            recover_accepted_launch(&session, &operation_id, &call, submitted)
        });
        if submission.is_err()
            && crate::settlement::post_operation_refusal(
                &operation_id,
                &self.factory.context,
                &session.store,
                &call,
            )
            .is_err()
        {
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr().lock(),
                "refusal notice could not be delivered; inspect the chair binding and notice configuration, then run `provenant reconcile`"
            );
        }
        let (admitted, started) = submission?;
        self.receipt(&session, operation_id, &admitted, &started, &call)
    }
    pub(crate) fn output(
        &mut self,
        input: OutputInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<OutputResult, FacadeError> {
        let target = match &input {
            OutputInput::Read { target, .. } | OutputInput::Fetch { target, .. } => target.clone(),
        };
        if let Some(answer) = self.remote(
            &target,
            PeerAction::Output {
                extra: Default::default(),
                input: input.clone(),
            },
            budget,
            cancel,
        )? {
            return match answer {
                PeerResult::Output { data: value, .. } => Ok(value),
                _ => Err(FacadeError::StateCorrupt),
            };
        }
        let session = self.read_session(cancel)?;
        let call = self.call(cancel);
        let record = session
            .service()
            .status(target.attempt, &call)
            .map_err(run_error)?;
        let mut row = provenant_cli::run::project_lane(
            &record,
            &[],
            session.context.state_root.as_path(),
            self.factory.clock.wall_now().ok(),
            false,
        );
        row.host = target.host.map(|host| host.to_string());
        row.provider_seconds = provenant_records::wire::provider_seconds(record.provider_ms);
        let state = row.state.clone();
        let reason = row.reason.clone();
        let mut fields: provenant_records::ExtraFields = serde_json::from_value(
            serde_json::to_value(row).map_err(|_| FacadeError::StateCorrupt)?,
        )
        .map_err(|_| FacadeError::StateCorrupt)?;
        fields.insert(
            "provider_seconds".into(),
            serde_json::json!(provenant_records::wire::provider_seconds(
                record.provider_ms
            )),
        );
        let mut chunk_fields = fields.clone();
        chunk_fields.remove("state");
        chunk_fields.remove("reason");
        let digest = |chunk: &provenant_run::OutputChunk| {
            provenant_core::Digest::from_bytes(*chunk.digest.bytes())
        };
        match input {
            OutputInput::Read {
                part,
                offset,
                max_bytes,
                ..
            } => {
                if matches!(part, OutputPart::Events) {
                    let mut events = session
                        .service()
                        .events(offset, 64, Some(record.attempt_id), &call)
                        .map_err(run_error)?;
                    let eof = events.len() < 64;
                    // Trim the page, never an event, so a caller can page on.
                    let kept = events_within(&events, max_bytes)?;
                    if kept == 0 && !events.is_empty() {
                        return Err(FacadeError::OutputTooLarge);
                    }
                    let eof = eof && kept == events.len();
                    events.truncate(kept);
                    let next_offset = events.last().map_or(offset, |event| event.sequence);
                    let bytes =
                        serde_json::to_vec(&events).map_err(|_| FacadeError::StateCorrupt)?;
                    if bytes.len() as u64 > max_bytes {
                        return Err(FacadeError::OutputTooLarge);
                    }
                    return Ok(OutputResult::Chunk {
                        extra: chunk_fields,
                        target,
                        part,
                        state,
                        reason,
                        offset,
                        next_offset,
                        eof,
                        digest: provenant_core::Digest::from_bytes(
                            *provenant_records::CanonicalV1::file_digest(&bytes).bytes(),
                        ),
                        bytes: OutputBytes::from_bytes(bytes),
                    });
                }
                let artifact = match part {
                    OutputPart::Result => provenant_run::ArtifactPart::Result,
                    OutputPart::Prompt => provenant_run::ArtifactPart::Prompt,
                    OutputPart::Stderr => provenant_run::ArtifactPart::Stderr,
                    OutputPart::Events => return Err(FacadeError::InvalidInput { detail: None }),
                };
                let mut chunk = session
                    .service()
                    .output(
                        record.attempt_id,
                        artifact,
                        provenant_run::ByteRange { offset, max_bytes },
                        &call,
                    )
                    .map_err(run_error)?;
                provenant_cli::run::align_utf8_tail(&mut chunk);
                // The serving side reads its own graph, so `output --host` can
                // name the planner fallback it recorded.
                let mut extra = chunk_fields;
                extra.insert("length".into(), serde_json::Value::from(chunk.len));
                if matches!(part, OutputPart::Result)
                    && let Some(reason) = provenant_cli::run::planner_fallback_reason(
                        &self.factory.context.roots.state,
                        session.context.project_id,
                        &record,
                    )
                {
                    extra.insert(
                        provenant_cli::run::PLANNER_FALLBACK_KEY.into(),
                        serde_json::Value::String(reason),
                    );
                }
                Ok(OutputResult::Chunk {
                    extra,
                    target,
                    part,
                    state,
                    reason,
                    offset: chunk.offset,
                    next_offset: chunk.next_offset,
                    eof: chunk.eof,
                    digest: digest(&chunk),
                    bytes: OutputBytes::from_bytes(chunk.bytes),
                })
            }
            OutputInput::Fetch {
                artifact,
                max_bytes,
                ..
            } => {
                // Only the attempt's own bound result is fetchable.
                let bound = record.result.as_ref().ok_or(FacadeError::NotFound)?;
                if artifact.path != bound.path || artifact.digest.is_some_and(|d| d != bound.digest)
                {
                    return Err(FacadeError::NotFound);
                }
                // A fetch returns the whole artifact or refuses; the reader
                // pages at 64 KiB, below Fabric's fetch limit.
                let mut bytes = Vec::new();
                loop {
                    let chunk = session
                        .service()
                        .output(
                            record.attempt_id,
                            provenant_run::ArtifactPart::Result,
                            provenant_run::ByteRange {
                                offset: bytes.len() as u64,
                                max_bytes: provenant_records::run::OUTPUT_LIMIT,
                            },
                            &call,
                        )
                        .map_err(run_error)?;
                    if chunk.len > max_bytes {
                        return Err(FacadeError::OutputTooLarge);
                    }
                    bytes.extend_from_slice(&chunk.bytes);
                    if chunk.eof {
                        return Ok(OutputResult::Artifact {
                            extra: fields,
                            target,
                            artifact,
                            digest: digest(&chunk),
                            bytes: OutputBytes::from_bytes(bytes),
                        });
                    }
                }
            }
        }
    }
    pub(crate) fn run(
        &mut self,
        input: RunInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<RunResult, FacadeError> {
        match input {
            RunInput::Status { target, .. } => {
                if let Some(response) = self.remote(
                    &target,
                    PeerAction::Run {
                        extra: Default::default(),
                        input: RunInput::Status {
                            extra: Default::default(),
                            target: target.clone(),
                        },
                    },
                    budget,
                    cancel,
                )? {
                    return match response {
                        PeerResult::Run { data, .. } => {
                            if matches!(&*data, RunResult::Status { attempt, .. }
                                if attempt.attempt_id == target.attempt)
                            {
                                Ok(*data)
                            } else {
                                Err(FacadeError::StateCorrupt)
                            }
                        }
                        _ => Err(FacadeError::StateCorrupt),
                    };
                }
                let attempt = self.snapshot(target, budget, cancel)?;
                let store = self.store(cancel)?;
                let waits = store.lane_waits(std::slice::from_ref(&attempt), &self.call(cancel));
                let mut extra = provenant_records::ExtraFields::default();
                extra.insert(
                    "lane_waits".into(),
                    serde_json::to_value(waits).map_err(|_| FacadeError::StateCorrupt)?,
                );
                Ok(RunResult::Status { extra, attempt })
            }
            RunInput::Wait {
                targets,
                condition,
                wait_seconds,
                ..
            } => {
                let requested = WaitBudget::new(
                    self.factory.clock.mono_now(),
                    provenant_core::DurationMicros::from_micros(
                        wait_seconds
                            .checked_mul(1_000_000)
                            .ok_or(FacadeError::InvalidInput { detail: None })?,
                    ),
                    provenant_core::WaitTransport::Cli,
                )
                .map_err(|_| FacadeError::InvalidInput { detail: None })?;
                let budget =
                    if requested.deadline().as_mono_time() < budget.deadline().as_mono_time() {
                        requested
                    } else {
                        budget
                    };
                if !(1..=100).contains(&targets.len()) {
                    return Err(FacadeError::InvalidInput { detail: None });
                }
                let local_host = self.factory.context.identity.host;
                let mut local_indices = Vec::new();
                let mut remote_batches = std::collections::BTreeMap::new();
                for (index, target) in targets.iter().enumerate() {
                    match target.host {
                        Some(host) if host != local_host => {
                            remote_batches
                                .entry(host)
                                .or_insert_with(Vec::new)
                                .push((index, target.clone()));
                        }
                        _ => local_indices.push(index),
                    }
                }
                if !self.allow_remote && !remote_batches.is_empty() {
                    return Err(FacadeError::ConfinementRefused { resource: None });
                }
                let wait_config =
                    provenant_cli::config_reads::read_for(&self.factory.context.roots, "wait")?;
                let interval =
                    std::time::Duration::from_millis(wait_config.domains.wait.local_interval_ms);
                let local_session = if local_indices.is_empty() {
                    None
                } else {
                    Some(self.read_session(cancel)?)
                };
                let local_call = self.call(cancel);
                // Subscribe once before the first target read. A settlement
                // during registration or the first read remains observable.
                let event_paths = local_session
                    .as_ref()
                    .map(|session| session.store.observation_paths())
                    .unwrap_or_default();
                let mut observation =
                    crate::status_wait::LocalObservation::subscribe(&event_paths, interval);
                let remote_pending = !remote_batches.is_empty();
                let observation_extra = |observation: &crate::status_wait::LocalObservation| {
                    let mut extra = provenant_records::ExtraFields::default();
                    extra.insert(
                        "observation".into(),
                        serde_json::Value::String(observation.report(remote_pending)),
                    );
                    extra
                };
                // SQLite events can precede commit visibility and the shared
                // hint is lossy under contention; deadline results get one final
                // committed read from the owning store.
                let refresh_local = |attempts: &mut Vec<provenant_records::run::AttemptRecord>| {
                    if let Some(session) = &local_session {
                        for index in &local_indices {
                            attempts[*index] = session
                                .store
                                .record(targets[*index].attempt, &local_call)
                                .map_err(run_error)?;
                        }
                    }
                    Ok::<(), FacadeError>(())
                };
                let mut last_attempts = None;
                let mut observed_attempts: Vec<Option<provenant_records::run::AttemptRecord>> =
                    std::iter::repeat_with(|| None)
                        .take(targets.len())
                        .collect();
                let mut read_local = !local_indices.is_empty();
                let mut read_remote = !remote_batches.is_empty();
                let interval_micros = interval.as_micros() as u64;
                let mut next_remote_poll = self
                    .factory
                    .clock
                    .mono_now()
                    .checked_add(provenant_core::DurationMicros::from_micros(interval_micros))
                    .map_err(FacadeError::Time)?;
                loop {
                    if cancel.is_cancelled() {
                        return Err(FacadeError::Cancelled);
                    }
                    if budget.remaining(self.factory.clock.mono_now()).as_micros() == 0
                        && !(budget.is_nonblocking() && last_attempts.is_none())
                    {
                        if let Some(mut attempts) = last_attempts {
                            refresh_local(&mut attempts)?;
                            let satisfied = provenant_fabric_runtime::runs::wait_satisfied(
                                condition,
                                attempts.iter().map(|record| &record.state),
                            );
                            return Ok(RunResult::Wait {
                                extra: observation_extra(&observation),
                                condition,
                                attempts,
                                satisfied,
                                timed_out: !satisfied,
                            });
                        }
                        return Err(FacadeError::Timeout);
                    }
                    if read_local {
                        let Some(session) = &local_session else {
                            return Err(FacadeError::StateCorrupt);
                        };
                        for index in &local_indices {
                            observed_attempts[*index] = Some(
                                session
                                    .store
                                    .record(targets[*index].attempt, &local_call)
                                    .map_err(run_error)?,
                            );
                        }
                        read_local = false;
                    }
                    if read_remote {
                        for (host, batch) in &remote_batches {
                            let requested = batch
                                .iter()
                                .map(|(_, target)| target.clone())
                                .collect::<Vec<_>>();
                            let result = match self.peer(
                                *host,
                                PeerAction::RunStatuses {
                                    extra: Default::default(),
                                    targets: requested,
                                },
                                budget,
                                cancel,
                            ) {
                                Ok(result) => result,
                                // A poll cut short by the wait's own deadline is a
                                // timed-out wait, not an unavailable peer.
                                Err(error) => {
                                    let expired =
                                        budget.remaining(self.factory.clock.mono_now()).as_micros()
                                            == 0;
                                    return match last_attempts {
                                        Some(mut attempts) if expired => {
                                            refresh_local(&mut attempts)?;
                                            let satisfied =
                                                provenant_fabric_runtime::runs::wait_satisfied(
                                                    condition,
                                                    attempts.iter().map(|record| &record.state),
                                                );
                                            Ok(RunResult::Wait {
                                                extra: observation_extra(&observation),
                                                condition,
                                                attempts,
                                                satisfied,
                                                timed_out: !satisfied,
                                            })
                                        }
                                        None if expired => Err(FacadeError::Timeout),
                                        _ => Err(error),
                                    };
                                }
                            };
                            let PeerResult::RunStatuses { data: records, .. } = result else {
                                return Err(FacadeError::StateCorrupt);
                            };
                            if records.len() != batch.len() {
                                return Err(FacadeError::StateCorrupt);
                            }
                            for ((index, target), record) in batch.iter().zip(records) {
                                if record.attempt_id != target.attempt {
                                    return Err(FacadeError::StateCorrupt);
                                }
                                observed_attempts[*index] = Some(record);
                            }
                        }
                        read_remote = false;
                        next_remote_poll = self
                            .factory
                            .clock
                            .mono_now()
                            .checked_add(provenant_core::DurationMicros::from_micros(
                                interval_micros,
                            ))
                            .map_err(FacadeError::Time)?;
                    }
                    let attempts = observed_attempts
                        .iter()
                        .cloned()
                        .collect::<Option<Vec<_>>>()
                        .ok_or(FacadeError::StateCorrupt)?;
                    let satisfied = provenant_fabric_runtime::runs::wait_satisfied(
                        condition,
                        attempts.iter().map(|record| &record.state),
                    );
                    let timed_out =
                        budget.remaining(self.factory.clock.mono_now()).as_micros() == 0;
                    if satisfied || timed_out {
                        return Ok(RunResult::Wait {
                            extra: observation_extra(&observation),
                            condition,
                            attempts,
                            satisfied,
                            timed_out: !satisfied && timed_out,
                        });
                    }
                    last_attempts = Some(attempts);

                    let remaining = std::time::Duration::from_micros(
                        budget.remaining(self.factory.clock.mono_now()).as_micros(),
                    );
                    let until_remote = if remote_batches.is_empty() {
                        remaining
                    } else {
                        std::time::Duration::from_micros(
                            next_remote_poll
                                .as_micros()
                                .saturating_sub(self.factory.clock.mono_now().as_micros()),
                        )
                    };
                    let maximum = remaining.min(until_remote);
                    let wake = observation
                        .wait(maximum, cancel, &self.factory.clock)
                        .map_err(FacadeError::Time)?;
                    match wake {
                        crate::status_wait::ObservationWake::Changed
                        | crate::status_wait::ObservationWake::Woken => {
                            read_local = !local_indices.is_empty();
                        }
                        crate::status_wait::ObservationWake::PacedFallback => {
                            read_local = !local_indices.is_empty();
                            read_remote = !remote_batches.is_empty();
                        }
                        crate::status_wait::ObservationWake::NativeTimeout => {}
                        crate::status_wait::ObservationWake::Cancelled => {
                            return Err(FacadeError::Cancelled);
                        }
                    }
                    if !remote_batches.is_empty()
                        && self.factory.clock.mono_now() >= next_remote_poll
                    {
                        read_remote = true;
                    }
                }
            }
            RunInput::Cancel {
                ref target,
                ref operation_id,
                ..
            } => {
                if self.factory.context.in_lane
                    && target
                        .host
                        .is_some_and(|host| host != self.factory.context.identity.host)
                {
                    return Err(FacadeError::LaneCoordinationRefused);
                }
                if let Some(answer) = self.remote(
                    target,
                    PeerAction::Run {
                        extra: Default::default(),
                        input: input.clone(),
                    },
                    budget,
                    cancel,
                )? {
                    return match answer {
                        PeerResult::Run { data: value, .. }
                            if matches!(&*value, RunResult::Cancel { attempt, .. }
                                if attempt.attempt_id == target.attempt) =>
                        {
                            Ok(*value)
                        }
                        _ => Err(FacadeError::StateCorrupt),
                    };
                }
                self.require_graph_control(target, cancel)?;
                let store = self.store(cancel)?;
                let call = self.call(cancel);
                let reason = format!("cancelled by Fabric operation {operation_id}");
                let service = provenant_run::Service::new(&store);
                let receipt = service
                    .cancel(target.attempt, &reason, &call)
                    .map_err(run_error)?;
                // Without a launch summary nothing was asked to stop.
                if let provenant_run::CancelReceipt::Unobservable { .. } = receipt {
                    return Err(FacadeError::LaunchUnknown {
                        operation_id: operation_id.clone(),
                    });
                }
                Ok(RunResult::Cancel {
                    extra: Default::default(),
                    attempt: service.status(target.attempt, &call).map_err(run_error)?,
                })
            }
            RunInput::Resume { .. } => Err(FacadeError::invalid(InvalidInputDetail::Field(
                "retry requires a graph target".into(),
            ))),
            RunInput::List { after, limit, .. } => {
                if !(1..=100).contains(&limit) {
                    return Err(FacadeError::InvalidInput { detail: None });
                }
                let host = self.factory.context.identity.host;
                if after
                    .as_ref()
                    .and_then(|target| target.host)
                    .is_some_and(|cursor_host| cursor_host != host)
                {
                    return Err(FacadeError::OwnerUnavailable);
                }
                let store = match self.read_session(cancel) {
                    Ok(session) => session.store,
                    Err(FacadeError::NotFound) if after.is_none() => {
                        return Ok(RunResult::List {
                            extra: Default::default(),
                            page: AttemptPage {
                                extra: Default::default(),
                                items: Vec::new(),
                                next_after: None,
                                truncated: false,
                            },
                        });
                    }
                    Err(error) => return Err(error),
                };
                let call = self.call(cancel);
                let after_sequence = after
                    .as_ref()
                    .map(|target| {
                        store
                            .attempt_sequence(target.attempt, &call)
                            .map_err(run_error)
                    })
                    .transpose()?
                    .unwrap_or(0);
                let page = provenant_run::Service::new(&store)
                    .lanes(
                        &provenant_run::LaneQuery {
                            targets: Vec::new(),
                            all: true,
                            limit: u64::from(limit),
                            after: after_sequence,
                            newest_first: true,
                            owner: None,
                            unseen_after: None,
                        },
                        &call,
                    )
                    .map_err(run_error)?;
                let truncated = page.next.is_some();
                let next_after = if truncated {
                    page.lanes.last().map(|lane| AttemptTarget {
                        extra: Default::default(),
                        attempt: lane.attempt_id,
                        host: Some(host),
                    })
                } else {
                    None
                };
                let items = page
                    .lanes
                    .into_iter()
                    .map(provenant_fabric_runtime::runs::AttemptSummary::from)
                    .collect();
                Ok(RunResult::List {
                    extra: Default::default(),
                    page: AttemptPage {
                        extra: Default::default(),
                        items,
                        next_after,
                        truncated,
                    },
                })
            }
        }
    }
    pub(crate) fn events(
        &mut self,
        after: u64,
        limit: u16,
        cancel: &CancelToken,
    ) -> Result<RunEventsPage, FacadeError> {
        if !(1..=64).contains(&limit) {
            return Err(FacadeError::InvalidInput { detail: None });
        }
        let store = match self.read_session(cancel) {
            Ok(session) => session.store,
            // Coordination can have events before the first graph is launched.
            // Reading them must neither require nor create an execution store.
            Err(FacadeError::NotFound) => {
                return Ok(RunEventsPage {
                    extra: Default::default(),
                    items: Vec::new(),
                    cursor: after,
                    truncated: false,
                });
            }
            Err(error) => return Err(error),
        };
        let call = provenant_run::Request {
            clock: &self.factory.clock,
            cancel,
        };
        let requested = usize::from(limit).saturating_add(1).min(64);
        let mut items = provenant_run::Service::new(&store)
            .events(after, requested, None, &call)
            .map_err(run_error)?;
        let truncated = if limit < 64 {
            items.len() > usize::from(limit)
        } else if items.len() == 64 {
            let cursor = items.last().map_or(after, |event| event.sequence);
            !provenant_run::Service::new(&store)
                .events(cursor, 1, None, &call)
                .map_err(run_error)?
                .is_empty()
        } else {
            false
        };
        items.truncate(usize::from(limit));
        let cursor = items.last().map_or(after, |event| event.sequence);
        Ok(RunEventsPage {
            extra: Default::default(),
            items,
            cursor,
            truncated,
        })
    }
}

fn require_graph_context(
    context: Option<&provenant_records::run::WorkContext>,
) -> Result<(), FacadeError> {
    if context.is_some_and(|context| context.graph_id.is_some() && context.node_id.is_some()) {
        Ok(())
    } else {
        Err(FacadeError::invalid(InvalidInputDetail::Field(
            "retained non-graph lanes are read-only".into(),
        )))
    }
}

/// Whether the lane of `admitter`, the node whose lane or plan admitted the
/// node being launched, has sealed user-enabled sandbox evidence. Missing
/// evidence does not invent an enabled sandbox; peers carry the host-sealed
/// parent flag alongside the launch.
pub(crate) fn admitter_floors(
    admitter: &str,
    checkpoint: impl FnOnce() -> Option<provenant_fabric_runtime::graph::GraphCheckpoint>,
    local_host: &str,
    floors: &mut dyn FnMut(provenant_core::AttemptId) -> bool,
) -> bool {
    let Some(checkpoint) = checkpoint() else {
        return false;
    };
    let mut references = checkpoint
        .nodes
        .iter()
        .filter(|row| row.id == admitter)
        .flat_map(|row| row.recorded_attempts())
        .filter_map(|row| row.attempt_ref.as_deref())
        .peekable();
    if references.peek().is_none() {
        return false;
    }
    references.any(|reference| {
        match reference
            .split_once('/')
            .filter(|(host, _)| *host == local_host)
            .and_then(|(_, id)| id.parse::<provenant_core::AttemptId>().ok())
        {
            Some(id) => floors(id),
            None => false,
        }
    })
}

#[cfg(test)]
#[allow(
    unused_imports,
    reason = "the custom worktree driver omits libtest functions"
)]
mod graph_control_tests {
    use super::*;
    #[test]
    fn admission_requires_user_enabled_sandbox_evidence() {
        let open = provenant_core::AttemptId::from_bytes([1; 16]);
        let closed = provenant_core::AttemptId::from_bytes([2; 16]);
        let checkpoint = |reference: String| {
            move || {
                serde_json::from_value(serde_json::json!({
                    "schema_version":1,"graph_id":"g","plan_digest":"fixture",
                    "nodes":[
                        {"id":"composite","state":"running","attempts":[]},
                        {"id":"lane","state":"running","attempts":[{
                            "key":{"graph_id":"g","node_id":"lane","route_index":0, "attempt": 1},
                            "launched":true,"route":{"alias":"junior"},"state":"running",
                            "attempt_ref":reference,"launch_actor":"fixture"}]},
                    ]
                }))
                .ok()
            }
        };
        let mut lookup = |id| id == closed;
        let local = |id: provenant_core::AttemptId| format!("host/{id}");
        assert!(!admitter_floors(
            "lane",
            checkpoint(local(open)),
            "host",
            &mut lookup
        ));
        assert!(admitter_floors(
            "lane",
            checkpoint(local(closed)),
            "host",
            &mut lookup
        ));
        // Missing evidence cannot turn optional sandboxing on. A peer's
        // host-sealed floor travels on AttemptSpec instead.
        assert!(!admitter_floors(
            "lane",
            checkpoint(format!("peer/{open}")),
            "host",
            &mut lookup
        ));
        assert!(!admitter_floors("lane", || None, "host", &mut lookup));
        assert!(!admitter_floors(
            "composite",
            checkpoint(local(open)),
            "host",
            &mut lookup
        ));
    }
    #[test]
    fn blocked_launches_refuse_without_echoing_untrusted_text() {
        let attempt = provenant_core::AttemptId::from_bytes([7; 16]);
        let blocked = provenant_run::Started::Blocked {
            attempt,
            error: "untrusted preparation text".into(),
        };
        let Some(FacadeError::DispatchRefused { reason, .. }) = launch_refusal(&[blocked], &[])
        else {
            panic!("retained custody must refuse the launch");
        };
        assert!(reason.contains(&attempt.to_string()));
        assert!(reason.contains("retained custody"));
        assert!(!reason.contains("untrusted preparation text"));
        assert!(launch_refusal(&[], &[]).is_none());
        assert!(
            launch_refusal(
                &[
                    provenant_run::Started::Launched(attempt),
                    provenant_run::Started::Blocked {
                        attempt,
                        error: "blocked".into()
                    },
                ],
                &[]
            )
            .is_none()
        );
    }

    #[test]
    fn retained_attempts_require_graph_context_before_controls() {
        assert!(require_graph_context(None).is_err());
        let mut context = provenant_records::run::WorkContext::default();
        assert!(require_graph_context(Some(&context)).is_err());
        context.graph_id = Some("graph".into());
        assert!(require_graph_context(Some(&context)).is_err());
        context.node_id = Some("task".into());
        assert!(require_graph_context(Some(&context)).is_ok());
    }
}
