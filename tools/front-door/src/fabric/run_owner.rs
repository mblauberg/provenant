use super::*;

pub(crate) struct BinaryRunOwner<'a> {
    pub(crate) factory: &'a Factory,
    /// False only at peer ingress: no forwarding, and writers name a new
    /// worktree instead of a path.
    pub(crate) allow_remote: bool,
    /// Replay submissions under this actor instead of the process's own.
    pub(crate) actor: Option<provenant_core::ActorId>,
    /// Only the direct chair entry point supplies a verified origin.
    pub(crate) chair: Option<provenant_run::ChairBinding>,
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
    // The unresolvable ref is the caller's own input; name it instead of
    // collapsing to a bare "invalid input".
    if let provenant_run::RunError::InvalidBase(base) = &error {
        return FacadeError::invalid(format!("unresolvable base `{base}`"));
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
            Some(DiagnosticFact::Filesystem { operation }) => return FacadeError::io(*operation),
            Some(DiagnosticFact::DiskPressure {
                available,
                floor,
                reclaimed,
            }) => {
                return FacadeError::DispatchRefused {
                    fact: None,
                    reason: "insufficient disk space".into(),
                    disk: Some(
                        provenant_fabric_runtime::coordination::facade::DiskPressure {
                            available: *available,
                            floor: *floor,
                            reclaimed: *reclaimed,
                        },
                    ),
                };
            }
            Some(DiagnosticFact::DiskUnobserved) => {
                return FacadeError::DispatchUnobserved {
                    reason: "disk observation failed".into(),
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
            ..
        }) => FacadeError::Io {
            delivery_limit,
            delivery_stage,
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
    if matches!(
        fail.projection
            .as_ref()
            .and_then(|projection| projection.fact.as_ref()),
        Some(provenant_records::diagnostic::DiagnosticFact::DiskPressure { .. })
    ) {
        return fail_error(fail);
    }
    match fail.detail.map(|detail| *detail) {
        Some(
            detail @ FacadeError::DispatchRefused {
                fact: None,
                disk: Some(_),
                ..
            },
        ) => detail,
        _ => FacadeError::DispatchRefused {
            fact: None,
            disk: None,
            reason: "dispatch was refused; inspect the request and access policy".into(),
        },
    }
}
/// Translate a typed run category at the MCP edge. `ErrorKind` is the one
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
        | E::GuardianIdentity => FacadeError::ConfinementRefused,
        E::WriterBusy => FacadeError::WriterBusy,
        E::WriterLockBusy => FacadeError::WriterLockBusy,
        E::WriterUnknown => FacadeError::WriterUnknown,
        E::CredentialRoot => FacadeError::CredentialRoot {
            access: "requested".into(),
        },
        E::LaneDepth => FacadeError::LaneDepth,
        E::LaneContext
        | E::ParentAttempt
        | E::Invalid
        | E::WorktreeRequired
        | E::WorktreeNotApplicable => FacadeError::InvalidInput { detail: None },
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
    /// The attempts of a run this owner just accepted or continued.
    pub(crate) fn receipt(
        &self,
        session: &provenant_cli::run::Session,
        operation_id: provenant_core::OperationId,
        run: &provenant_run::RunRecord,
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
        let page = session
            .service()
            .lanes(
                &provenant_run::LaneQuery {
                    targets: vec![provenant_run::LaneTarget {
                        run: run.run_id,
                        task: None,
                        attempt: None,
                    }],
                    all: true,
                    limit: 100,
                    after: 0,
                    newest_first: false,
                    owner: None,
                    unseen_after: None,
                },
                call,
            )
            .map_err(run_error)?;
        Ok(SubmissionReceipt {
            extra: Default::default(),
            worktree: None,
            operation_id,
            targets: page
                .lanes
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
                    return Err(FacadeError::ConfinementRefused);
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
                return Err(FacadeError::ConfinementRefused);
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
        provenant_run::Service::new(&self.store(cancel)?)
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
        let session = self.session(cancel)?;
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
        let store = self.store(cancel)?;
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
        let store = self.store(cancel)?;
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
            submit: true,
            list: true,
            status: true,
            wait: true,
            cancel: true,
            resume: provenant_cli::run::Registry::default().continues_sessions(),
            output: true,
            events: true,
        }
    }
    /// Submissions take the CLI's scan, record and launch path. Fabric never
    /// creates queued rows itself or reconstructs a provider invocation.
    pub(crate) fn submit(
        &mut self,
        input: SubmitInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<SubmissionReceipt, FacadeError> {
        self.submit_inner(
            input,
            budget,
            cancel,
            provenant_records::run::LaneAccess::default(),
            None,
            true,
            None,
        )
        .map_err(|error| scope_io(error, "submit"))
    }
    /// Graph nodes use the same validated, sealed access grants as CLI lanes.
    pub(crate) fn submit_with_access(
        &mut self,
        input: SubmitInput,
        budget: WaitBudget,
        cancel: &CancelToken,
        access: provenant_records::run::LaneAccess,
        settlement_retry_limit: Option<u8>,
        attempt: Option<provenant_fabric_runtime::graph::AttemptSpec>,
    ) -> Result<SubmissionReceipt, FacadeError> {
        self.submit_inner(
            input,
            budget,
            cancel,
            access,
            settlement_retry_limit,
            false,
            attempt,
        )
        .map_err(|error| scope_io(error, "submit"))
    }
    pub(crate) fn submit_inner(
        &mut self,
        input: SubmitInput,
        budget: WaitBudget,
        cancel: &CancelToken,
        access: provenant_records::run::LaneAccess,
        settlement_retry_limit: Option<u8>,
        apply_project_defaults: bool,
        attempt: Option<provenant_fabric_runtime::graph::AttemptSpec>,
    ) -> Result<SubmissionReceipt, FacadeError> {
        use provenant_cli::run::{Submission, Submitted, derived_task, worktree_access};
        let SubmitInput::Submit {
            operation_id,
            tasks,
            concurrency,
            knobs,
            host,
            ..
        } = input;
        // Explicit placement forwards the whole batch; the peer confines it.
        if let Some(host) = host.filter(|h| *h != self.factory.context.identity.host) {
            if !access.is_default() {
                return Err(FacadeError::ConfinementRefused);
            }
            if !self.allow_remote {
                return Err(FacadeError::ConfinementRefused);
            }
            // MCP forwards read-only work only; a peer writer comes from `dispatch`.
            if tasks
                .iter()
                .any(|t| t.access != provenant_fabric_runtime::graph::AccessMode::ReadOnly)
            {
                return Err(FacadeError::OwnerUnavailable);
            }
            let input = SubmitInput::Submit {
                extra: Default::default(),
                operation_id: operation_id.clone(),
                tasks,
                concurrency,
                knobs,
                host: None,
            };
            return match self.peer(
                host,
                PeerAction::Submit {
                    extra: Default::default(),
                    input,
                },
                budget,
                cancel,
            )? {
                PeerResult::Submitted { data: receipt, .. }
                    if receipt.operation_id == operation_id
                        && receipt.worktree.is_none()
                        && !receipt.targets.is_empty()
                        && receipt.targets.iter().all(|t| t.host == Some(host)) =>
                {
                    Ok(receipt)
                }
                _ => Err(FacadeError::StateCorrupt),
            };
        }
        // Peer ingress creates a writer's worktree in this host's clone; a
        // caller never names a path here, and a batch creates at most one.
        let ingress = !self.allow_remote;
        let mut tasks = tasks;
        let mut created = None;
        for task in &mut tasks {
            let Some(spec) = task.new_worktree.take() else {
                if ingress && task.worktree.is_some() {
                    return Err(FacadeError::InvalidInput { detail: None });
                }
                continue;
            };
            if !ingress
                || created.is_some()
                || task.worktree.is_some()
                || task.access != provenant_fabric_runtime::graph::AccessMode::WorktreeWrite
            {
                return Err(FacadeError::InvalidInput { detail: None });
            }
            let worktree = self.peer_worktree(&operation_id, spec, budget, cancel)?;
            task.worktree = Some(worktree.path.clone());
            created = Some(worktree);
        }
        let defaults = if apply_project_defaults {
            let config = provenant_config::read_with_domains(
                &self.factory.context.roots,
                &provenant_cli::domains::SECTIONS,
            )
            .map_err(|_| FacadeError::InvalidInput { detail: None })?;
            config
                .domains
                .lanes
                .projects
                .get(&self.factory.context.identity.project.to_string())
                .map(|settings| settings.capabilities.clone())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let mut submitted = Vec::new();
        for (index, task) in tasks.into_iter().enumerate() {
            worktree_access(
                task.access == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite,
                false,
                task.worktree.as_deref().map(Path::new),
            )
            .map_err(fail_error)?;
            let mut task_access = access.clone();
            if apply_project_defaults {
                let capabilities = task.capabilities.as_deref().unwrap_or_else(|| {
                    if task.access == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite {
                        defaults.as_slice()
                    } else {
                        &[]
                    }
                });
                for capability in capabilities {
                    if capability == "postgres" {
                        task_access.database_ipc = true;
                        task_access.extra.insert("capability_sources".into(), serde_json::json!([{"name":"postgres", "source": if task.capabilities.is_some() { task.capability_sources.get("postgres").map(String::as_str).filter(|source| matches!(*source, "flag" | "project default" | "task")).unwrap_or("task") } else { "project default" }}]));
                    } else {
                        provenant_cli::run::warn_ignored_capability("project ", capability);
                    }
                }
            }
            submitted.push(Submitted {
                attempt: attempt.clone().unwrap_or_else(|| {
                    provenant_fabric_runtime::graph::AttemptSpec {
                        account: task.account,
                        build_heavy: task.build_heavy,
                        ..Default::default()
                    }
                }),
                require_skill_location: task.require_skill_location,
                settlement_retry_limit,
                context: task.context,
                base: task.base,
                pinned_base: None,
                head: task.head,
                task_id: derived_task(&operation_id, index).map_err(fail_error)?,
                prompt: task.prompt,
                title: task.title,
                route: task.route,
                fallback: Vec::new(),
                provider_timeout: task.timeout_seconds,
                delivery_id: task.delivery_id,
                deliver: task.deliver,
                worktree: task.worktree.map(PathBuf::from),
                access: task_access,
            });
        }
        let session = self.session(cancel)?;
        let call = self.call(cancel);
        let (run, started) = self.with_edge(|edge| {
            provenant_cli::run::submit_with_host(
                &session,
                edge,
                Submission {
                    chair: self.chair.clone(),
                    operation_id: operation_id.clone(),
                    tasks: submitted,
                    concurrency: concurrency.map(u64::from),
                    knobs,
                },
                &call,
                self.factory.host_launch.as_ref(),
            )
        })?;
        let mut receipt = self.receipt(&session, operation_id, &run, &started, &call)?;
        receipt.worktree = created;
        Ok(receipt)
    }
    /// The writer worktree a peer request names. A first request fetches every
    /// remote, requires the base on a remote-tracking ref and creates the
    /// branch at it. A retry adopts the branch's worktree while it is still
    /// clean at that base; a replayed operation finds the one it created.
    pub(crate) fn peer_worktree(
        &self,
        operation_id: &provenant_core::OperationId,
        spec: NewWorktree,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<provenant_fabric_runtime::runs::PeerWorktree, FacadeError> {
        use provenant_confinement::worktrees::{
            self, WorktreeCondition, WorktreeCreate, WorktreeError, WriterState,
        };
        let branch = provenant_confinement::repo::GitRefName::branch(&spec.branch)
            .map_err(|_| FacadeError::InvalidInput { detail: None })?;
        provenant_confinement::repo::GitRevision::new(&spec.base)
            .map_err(|_| FacadeError::InvalidInput { detail: None })?;
        let replay = self
            .store(cancel)?
            .operation(operation_id, &self.call(cancel))
            .map_err(run_error)?
            .is_some();
        // Fetch and create each take at most half of what remains, so the
        // submission keeps a reserve; a retry adopts a create that ran late.
        let half = || {
            let micros = budget.remaining(self.factory.clock.mono_now()).as_micros() / 2;
            std::time::Duration::from_micros(micros)
        };
        let project = &self.factory.cwd;
        let base = if replay {
            None
        } else {
            Some(fetch_remote_base(project, &spec.base, half())?)
        };
        let info = crate::worktrees::with_context(
            &self.factory.context.roots,
            &self.factory.home,
            project,
            half(),
            self.factory.workers,
            |context, cancel| {
                let existing = worktrees::list(context, cancel)?
                    .into_iter()
                    .find(|w| w.branch.as_deref() == Some(branch.as_str()));
                let Some(base) = base else {
                    return existing.ok_or(WorktreeError::NotFound.into());
                };
                match existing {
                    Some(info)
                        if info.head == base
                            && info.dirty == WorktreeCondition::Clean
                            && info.writer != WriterState::Live =>
                    {
                        Ok(info)
                    }
                    Some(info) => Err(WorktreeError::BranchExistsWithBase {
                        branch: spec.branch.clone(),
                        head: info.head,
                        base,
                    }
                    .into()),
                    None => {
                        let request = WorktreeCreate {
                            branch: spec.branch.clone(),
                            base: Some(base.to_string()),
                        };
                        Ok(worktrees::create(context, request, cancel)?)
                    }
                }
            },
        )
        .map_err(|error| match error {
            // The branch name is valid, so either refusal means it already exists.
            crate::worktrees::Error::Worktree(
                WorktreeError::InvalidRequest | WorktreeError::BranchExistsWithBase { .. },
            ) if !replay => FacadeError::BranchExists,
            crate::worktrees::Error::Worktree(WorktreeError::NotFound) if replay => {
                FacadeError::WorktreeMissing
            }
            crate::worktrees::Error::Worktree(WorktreeError::NotFound) => {
                FacadeError::BaseUnreachable
            }
            crate::worktrees::Error::Worktree(WorktreeError::Cancelled) => FacadeError::Cancelled,
            _ => FacadeError::io("create peer worktree"),
        })?;
        Ok(provenant_fabric_runtime::runs::PeerWorktree {
            extra: Default::default(),
            path: info
                .path
                .to_str()
                .ok_or(FacadeError::io("create peer worktree"))?
                .to_owned(),
            branch: spec.branch,
        })
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
        let session = self.session(cancel)?;
        let call = self.call(cancel);
        let record = session
            .service()
            .status(target.attempt, &call)
            .map_err(run_error)?;
        let successors = Default::default();
        let mut row = provenant_cli::run::project_lane(
            &record,
            &[],
            session.context.state_root.as_path(),
            &successors,
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
                if matches!(part, OutputPart::Result)
                    && provenant_cli::run::cloud_owner::placement(&session.store, &record, &call)
                        .map_err(|error| FacadeError::invalid(&error))?.is_some()
                {
                    let job = provenant_cli::run::cloud_owner::load(&session.store, record.attempt_id, &call)
                        .map_err(|error| FacadeError::invalid(&error))?.ok_or(FacadeError::NotFound)?;
                    let view = provenant_cli::run::cloud_owner::view(&job);
                    let bytes = serde_json::to_vec(&view).map_err(|_| FacadeError::StateCorrupt)?;
                    let start = usize::try_from(offset).map_err(|_| FacadeError::InvalidInput { detail: None })?;
                    if start > bytes.len() || max_bytes == 0 || max_bytes > provenant_records::run::OUTPUT_LIMIT {
                        return Err(FacadeError::InvalidInput { detail: None });
                    }
                    let end = start.saturating_add(max_bytes as usize).min(bytes.len());
                    let digest = provenant_core::Digest::from_bytes(*provenant_records::CanonicalV1::file_digest(&bytes).bytes());
                    let mut extra = chunk_fields;
                    extra.insert("length".into(), serde_json::json!(bytes.len()));
                    return Ok(OutputResult::Chunk {
                        extra, target, part, state: view["state"]["state"].as_str().unwrap_or("unknown").into(),
                        reason, offset, next_offset: end as u64, eof: end == bytes.len(), digest,
                        bytes: OutputBytes::from_bytes(bytes[start..end].to_vec()),
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
            RunInput::Status { target, .. } => Ok(RunResult::Status {
                extra: Default::default(),
                attempt: self.snapshot(target, budget, cancel)?,
            }),
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
                    return Err(FacadeError::ConfinementRefused);
                }
                let local_session = if local_indices.is_empty() {
                    None
                } else {
                    Some(self.session(cancel)?)
                };
                let local_call = self.call(cancel);
                let mut last_attempts = None;
                let mut backoff_micros = 50_000_u64;
                loop {
                    if cancel.is_cancelled() {
                        return Err(FacadeError::Cancelled);
                    }
                    if budget.remaining(self.factory.clock.mono_now()).as_micros() == 0 {
                        if let Some(attempts) = last_attempts {
                            return Ok(RunResult::Wait {
                                extra: Default::default(),
                                condition,
                                attempts,
                                satisfied: false,
                                timed_out: true,
                            });
                        }
                        return Err(FacadeError::Timeout);
                    }
                    let mut attempts: Vec<Option<provenant_records::run::AttemptRecord>> =
                        std::iter::repeat_with(|| None)
                            .take(targets.len())
                            .collect();
                    if let Some(session) = &local_session {
                        for index in &local_indices {
                            attempts[*index] = Some(
                                session
                                    .store
                                    .record(targets[*index].attempt, &local_call)
                                    .map_err(run_error)?,
                            );
                        }
                    }
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
                                    Some(attempts) if expired => Ok(RunResult::Wait {
                                        extra: Default::default(),
                                        condition,
                                        attempts,
                                        satisfied: false,
                                        timed_out: true,
                                    }),
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
                            attempts[*index] = Some(record);
                        }
                    }
                    let attempts = attempts
                        .into_iter()
                        .collect::<Option<Vec<_>>>()
                        .ok_or(FacadeError::StateCorrupt)?;
                    let satisfied = provenant_fabric_runtime::runs::wait_satisfied(
                        condition,
                        attempts.iter().map(|r| &r.state),
                    );
                    let timed_out =
                        budget.remaining(self.factory.clock.mono_now()).as_micros() == 0;
                    if satisfied || timed_out {
                        return Ok(RunResult::Wait {
                            extra: Default::default(),
                            condition,
                            attempts,
                            satisfied,
                            timed_out: !satisfied && timed_out,
                        });
                    }
                    last_attempts = Some(attempts);
                    provenant_core::Pause::wait(
                        &self.factory.backoff,
                        provenant_core::DurationMicros::from_micros(
                            backoff_micros
                                .min(budget.remaining(self.factory.clock.mono_now()).as_micros()),
                        ),
                        cancel,
                    );
                    backoff_micros = backoff_micros.saturating_mul(2).min(500_000);
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
                let store = self.store(cancel)?;
                let call = self.call(cancel);
                let reason = format!("cancelled by Fabric operation {operation_id}");
                let service = provenant_run::Service::new(&store);
                service
                    .bind_cancel(operation_id, &target.attempt.to_string(), &call)
                    .map_err(run_error)?;
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
            RunInput::Resume {
                ref target,
                ref operation_id,
                ref prompt,
                expected_generation,
                no_fallback,
                ..
            } => {
                if self.factory.context.in_lane {
                    if target
                        .host
                        .is_some_and(|host| host != self.factory.context.identity.host)
                    {
                        return Err(FacadeError::LaneCoordinationRefused);
                    }
                    return Err(FacadeError::ContinuationUnsupported);
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
                            if matches!(&*value, RunResult::Resume { attempt, .. }
                                if attempt.operation_id == *operation_id) =>
                        {
                            Ok(*value)
                        }
                        _ => Err(FacadeError::StateCorrupt),
                    };
                }
                if !self.capabilities().resume {
                    return Err(FacadeError::ContinuationUnsupported);
                }
                let session = self.session(cancel)?;
                let call = self.call(cancel);
                let (run, started) = self.with_edge(|edge| {
                    provenant_cli::run::continue_attempt_mode(
                        &session,
                        edge,
                        target.attempt,
                        operation_id.clone(),
                        prompt.clone(),
                        expected_generation,
                        &call,
                        !no_fallback,
                    )
                })?;
                let receipt =
                    self.receipt(&session, operation_id.clone(), &run, &started, &call)?;
                let attempt = receipt.targets.first().ok_or(FacadeError::StateCorrupt)?;
                Ok(RunResult::Resume {
                    extra: Default::default(),
                    attempt: session
                        .service()
                        .status(attempt.attempt, &call)
                        .map_err(run_error)?,
                })
            }
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
                let store = self.store(cancel)?;
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
                Ok(RunResult::List {
                    extra: Default::default(),
                    page: AttemptPage {
                        extra: Default::default(),
                        items: page.lanes.into_iter().map(Into::into).collect(),
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
        let store = self.store(cancel)?;
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
