//! One typed execution API for command and MCP edges.
use crate::*;
use provenant_core::{AttemptId, OperationId, RunId};
use provenant_records::CanonicalV1;
use std::path::Path;

pub struct Service<'a> {
    store: &'a RunStore,
}
impl Service<'_> {
    /// Resolve a bare ID, accepting runs only where the operation allows them.
    /// Run and attempt IDs share a wire format, so a collision is ambiguous.
    pub fn resolve_target(
        &self,
        id: AttemptId,
        allow_run: bool,
        call: &Request<'_>,
    ) -> Result<LaneTarget> {
        let attempt = match self.store.record(id, call) {
            Ok(record) => Some(record),
            Err(RunError::NotFound) => None,
            Err(error) => return Err(error),
        };
        let run = if allow_run {
            match self.store.run(RunId::from_bytes(*id.as_bytes()), call) {
                Ok(record) => Some(record),
                Err(RunError::NotFound) => None,
                Err(error) => return Err(error),
            }
        } else {
            None
        };
        match (attempt, run) {
            (Some(_), Some(_)) => Err(RunError::Conflict),
            (Some(record), None) => Ok(LaneTarget {
                run: record.run_id,
                task: Some(record.task_id),
                attempt: Some(id),
            }),
            (None, Some(record)) => Ok(LaneTarget {
                run: record.run_id,
                task: None,
                attempt: None,
            }),
            (None, None) => Err(RunError::NotFound),
        }
    }
}
impl<'a> Service<'a> {
    pub fn new(store: &'a RunStore) -> Self {
        Self { store }
    }
    pub fn dispatch(
        &self,
        input: DispatchRequest,
        cwd: &std::path::Path,
        call: &Request<'_>,
    ) -> Result<RunRecord> {
        // Reuse the originally resolved boundary before consulting Git. Retrying
        // the same operation must work even after its caller-supplied ref vanishes.
        let operation = self.store.operation(&input.operation_id, call)?;
        let previous = match &operation {
            Some(operation) => match self.store.request(operation.run_id, call) {
                Ok(request) => Some(request),
                // A prepared operation may have crashed before request publication;
                // leave its existing adoption/repair path to RunStore::create.
                Err(RunError::NotFound) if operation.state == OperationState::Prepared => None,
                Err(error) => return Err(error),
            },
            None => None,
        };
        let mut requests = Vec::new();
        let mut attempts = Vec::new();
        let mut contribution_branches = std::collections::BTreeMap::new();
        // Refuse every task's unsupported launch before any task prepares a writer.
        for task in &input.tasks {
            let route = Self::route(task.route.clone())?;
            check_launch_capabilities(task, &route)?;
        }
        for mut task in input.tasks {
            if task
                .context
                .as_ref()
                .is_some_and(|context| !context.valid())
            {
                return Err(input_error(ErrorKind::Invalid, "invalid work context"));
            }
            task.route = Self::route(task.route)?;
            Self::scan(&task.prompt)?;
            if let Some(title) = &task.title {
                provenant_core::validate_task_title(title)
                    .map_err(|message| RunError::InvalidTaskTitle(message.into()))?;
            }
            // A lane's child launches outside the lane's sandbox, so it may not
            // name access its parent never had.
            if input.parent_attempt.is_some() && !task.access.is_default() {
                return Err(input_error(
                    ErrorKind::Unsupported,
                    "a nested dispatch cannot set network, capability, read-root or write-root access",
                ));
            }
            if task.deliver.is_some() && task.worktree.is_none() {
                return Err(input_error(
                    ErrorKind::Invalid,
                    "delivery requires a writer worktree",
                ));
            }
            let writer = task
                .worktree
                .as_ref()
                .map(|path| {
                    // A lane cannot read another worktree's registration; the host
                    // verifies the proposal before it launches the child.
                    let writer = if input.parent_attempt.is_some() {
                        resolve_writer_proposal(self.store.context.state_root.as_path(), cwd, path)?
                    } else {
                        resolve_writer(self.store.context.state_root.as_path(), cwd, path)?
                    };
                    Ok::<_, RunError>(writer)
                })
                .transpose()?;
            if input.parent_attempt.is_none() {
                let branch = match &writer {
                    Some(writer) => Some(
                        prepare_writer(
                            writer,
                            self.store.writer_registry(),
                            self.store.context.project_id,
                        )?
                        .branch,
                    ),
                    None => crate::pr_contribution::read_only_branch(
                        self.store.writer_registry(),
                        self.store.context.project_id,
                        cwd,
                    )?,
                };
                if let Some(branch) = branch {
                    contribution_branches.insert(
                        task.task_id,
                        (
                            branch.as_str().trim_start_matches("refs/heads/").to_owned(),
                            if writer.is_some() {
                                "worker"
                            } else {
                                "read_only"
                            },
                        ),
                    );
                }
            }
            let base_ref = task.base.clone();
            let prior = previous.as_ref().and_then(|request| {
                request.tasks.iter().find(|prior| {
                    prior.task_id == task.task_id
                        && prior.base_ref == base_ref
                        && prior.head == task.head
                })
            });
            let git_cwd = writer.as_ref().map_or(cwd, |writer| &writer.worktree);
            if prior.is_none()
                && let Some(head) = &task.head
            {
                let validate = |sha: &str, label: &str| -> Result<()> {
                    provenant_core::GitOid::parse(sha).map_err(|_| {
                        input_error(
                            ErrorKind::Invalid,
                            format!("{label} must be a full Git SHA"),
                        )
                    })?;
                    let git =
                        provenant_os::git::git_executable().ok_or(RunError::GitUnavailable)?;
                    let resolved = resolve_git_base(&git, git_cwd, sha).map_err(|_| {
                        input_error(
                            ErrorKind::Invalid,
                            format!("invalid Git {label}: expected a resolvable commit SHA"),
                        )
                    })?;
                    if resolved != sha {
                        return Err(input_error(
                            ErrorKind::Invalid,
                            format!("{label} SHA must name a commit"),
                        ));
                    }
                    Ok(())
                };
                validate(head, "head")?;
                if let Some(pinned) = &task.pinned_base {
                    validate(&pinned.to_string(), "base")?;
                } else if let Some(base) = &task.base {
                    validate(base, "base")?;
                }
            }
            let base = if let Some(prior) = prior {
                prior.base.clone()
            } else if let Some(pinned) = task.pinned_base {
                Some(pinned.to_string())
            } else if let Some(head) = task.head.as_deref().filter(|_| task.base.is_none()) {
                let git = provenant_os::git::git_executable().ok_or(RunError::GitUnavailable)?;
                Some(review_merge_base(&git, git_cwd, head)?)
            } else if input.parent_attempt.is_some() {
                task.base
            } else {
                task.base
                    .as_deref()
                    .map(|base| {
                        let git =
                            provenant_os::git::git_executable().ok_or(RunError::GitUnavailable)?;
                        resolve_git_base(&git, git_cwd, base)
                    })
                    .transpose()?
            };
            #[derive(serde::Serialize)]
            struct RunAuthority<'a> {
                writer: Option<&'a WriterRequest>,
                #[serde(skip_serializing_if = "Option::is_none")]
                deliver: Option<&'a WriterDeliveryRequest>,
                cwd: &'a Path,
                #[serde(skip_serializing_if = "LaneAccess::is_default")]
                access: &'a LaneAccess,
            }
            let authority = CanonicalV1::record_digest(
                "run-authority",
                2,
                &RunAuthority {
                    writer: writer.as_ref(),
                    deliver: task.deliver.as_ref(),
                    cwd,
                    access: &task.access,
                },
            )
            .map_err(|error| input_error(ErrorKind::Io, error.to_string()))?;

            requests.push(TaskRequest {
                context: task.context,
                extra: Default::default(),
                build_heavy: task.build_heavy,
                title: task.title,
                profile: task.profile,
                task_id: task.task_id,
                prompt_digest: CanonicalV1::file_digest(task.prompt.as_bytes()),
                base_ref,
                base,
                head: task.head,
                route: task.route,
                account: task.account,
                fallback: task.fallback,
                authority_digest: authority,
                writer,
                provider_timeout_seconds: task.timeout_seconds,
                delivery_id: task.delivery_id,
                deliver: task.deliver,
                access: task.access,
            });
            attempts.push(NewAttempt {
                attempt_id: fresh(AttemptId::from_bytes)?,
                task_id: task.task_id,
                prompt: task.prompt,
            });
        }
        let mut prepared = PreparedRun {
            run_id: fresh(RunId::from_bytes)?,
            operation_id: input.operation_id,
            request: CanonicalRequest {
                extra: Default::default(),
                child_attempt_id_version: previous
                    .as_ref()
                    .map_or(input.parent_attempt.map(|_| 1), |request| {
                        request.child_attempt_id_version
                    }),
                chair: previous
                    .as_ref()
                    .map_or(input.chair, |request| request.chair.clone()),
                fresh_continuation: None,
                schema_version: 1,
                tasks: requests,
                concurrency: input.concurrency,
                parent: None,
                lane_depth: Some(input.lane_depth),
                parent_attempt: input.parent_attempt,
            },
            attempts,
            created_at: call
                .clock
                .wall_now()
                .map_err(|_| RunError::Validation("wall clock"))?,
        };
        // Repair an older prepared operation whose request was never published.
        // Only an exact digest match may retain the legacy, unindexed scheme.
        if previous.is_none()
            && let Some(operation) = operation
            && operation.request_digest != prepared.request.digest()?
        {
            let mut legacy = prepared.request.clone();
            legacy.child_attempt_id_version = None;
            if legacy.digest()? == operation.request_digest {
                prepared.request = legacy;
            }
        }
        if let Some(id) = crate::correlation::child_run_id(
            self.store.context.project_id,
            &prepared.request,
            &prepared.operation_id,
        )? {
            prepared.run_id = id;
        }
        for attempt in &mut prepared.attempts {
            if let Some(id) = crate::correlation::child_attempt_id(
                self.store.context.project_id,
                prepared.run_id,
                &prepared.request,
                attempt.task_id,
            )? {
                attempt.attempt_id = id;
            }
        }
        let record = self.store.create(&prepared, call)?;
        if !contribution_branches.is_empty() {
            let captured = (|| {
                let db = Contributions::open(self.store.writer_registry(), call)?;
                // Read the accepted run, including on operation replay. Newly
                // generated candidate IDs are not authoritative after create.
                for attempt in self.store.contribution_attempts(record.run_id, call)? {
                    if let Some((branch, role)) = contribution_branches.get(&attempt.task_id) {
                        db.capture(&attempt, branch, role, call)?;
                    }
                }
                Ok(())
            })();
            self.store.contribution_diagnostic(record.run_id, captured);
        }
        Ok(record)
    }
    /// Canonical model IDs carry the provider prefix in every stored request.
    pub fn route(route: RequestedRoute) -> Result<RequestedRoute> {
        match route {
            RequestedRoute::Adapter {
                provider,
                name,
                extra,
            } if !provider.is_empty()
                && !provider.contains('/')
                && !name.is_empty()
                && !extra.contains_key("model")
                && !extra.contains_key("effort") =>
            {
                Ok(RequestedRoute::Adapter {
                    provider,
                    name,
                    extra,
                })
            }
            RequestedRoute::Model {
                provider,
                model,
                effort,
                ..
            } => {
                if provider.is_empty() || model.is_empty() || provider.contains('/') {
                    return Err(input_error(ErrorKind::Invalid, "invalid model route"));
                }
                let model = match model.split_once('/') {
                    Some((prefix, id)) if prefix == provider && !id.is_empty() => model,
                    Some(_) => {
                        return Err(input_error(
                            ErrorKind::Invalid,
                            "model provider does not match route provider",
                        ));
                    }
                    None => format!("{provider}/{model}"),
                };
                Ok(RequestedRoute::Model {
                    extra: Default::default(),
                    provider,
                    model,
                    effort,
                })
            }
            RequestedRoute::Named { name, effort, .. } if !name.is_empty() => {
                Ok(RequestedRoute::Named {
                    extra: Default::default(),
                    name,
                    effort,
                })
            }
            RequestedRoute::Review {
                name,
                effort,
                excluded_families,
                distinct_families,
                require_distinct,
                ..
            } if !name.is_empty() => Ok(RequestedRoute::Review {
                extra: Default::default(),
                name,
                effort,
                excluded_families,
                distinct_families,
                require_distinct,
            }),
            _ => Err(input_error(
                ErrorKind::Invalid,
                "named route requires a name",
            )),
        }
    }
    pub fn scan(prompt: &str) -> Result<()> {
        provenant_confinement::disclosure::scan_snapshot(prompt.as_bytes(), &[]).map_err(|_| {
            RunError::Input {
                kind: ErrorKind::SecretInput,
                message: "input matches a secret pattern; remove the secret and retry".into(),
            }
        })
    }
    pub fn status(&self, id: AttemptId, call: &Request<'_>) -> Result<AttemptRecord> {
        self.store.record(id, call)
    }
    /// Reader waits include custody release after the terminal transaction.
    pub fn status_for_wait(&self, id: AttemptId, call: &Request<'_>) -> Result<AttemptRecord> {
        self.custody_state(self.status(id, call)?, call)
    }
    fn custody_state(
        &self,
        mut record: AttemptRecord,
        call: &Request<'_>,
    ) -> Result<AttemptRecord> {
        if matches!(record.state, AttemptState::Terminal { .. })
            && !self.store.custody_released(&record, call)?
        {
            record.state = AttemptState::Settling {
                publication_pending: false,
                extra: Default::default(),
            };
        }
        Ok(record)
    }
    pub fn lanes(&self, query: &LaneQuery, call: &Request<'_>) -> Result<LanePage> {
        self.store.lanes(query, call)
    }
    /// Peer waits consume the same custody-aware view as local waits.
    pub fn lanes_for_wait(&self, query: &LaneQuery, call: &Request<'_>) -> Result<LanePage> {
        let mut page = self.lanes(query, call)?;
        page.lanes = page
            .lanes
            .into_iter()
            .map(|record| self.custody_state(record, call))
            .collect::<Result<_>>()?;
        Ok(page)
    }
    pub fn output(
        &self,
        id: AttemptId,
        part: ArtifactPart,
        range: ByteRange,
        call: &Request<'_>,
    ) -> Result<OutputChunk> {
        self.store
            .read_output(id, part, range, &self.store.context, call)
    }
    /// Bind a cancel operation to its target once per command, before any
    /// attempt is asked to stop.
    pub fn bind_cancel(
        &self,
        operation: &OperationId,
        target: &str,
        call: &Request<'_>,
    ) -> Result<()> {
        self.store.bind_cancel(operation, target, call)
    }
    pub fn cancel(&self, id: AttemptId, reason: &str, call: &Request<'_>) -> Result<CancelReceipt> {
        self.store.cancel(id, reason, call)
    }
    pub fn start(
        &self,
        run: RunId,
        providers: &dyn Providers,
        holder: &dyn Fn(&AttemptRecord, provenant_core::ActorId) -> Result<Vec<u8>>,
        validated: Option<&ValidatedChild>,
        expected: &CanonicalRequest,
        call: &Request<'_>,
    ) -> Result<Vec<Started>> {
        match validated {
            Some(request) => self
                .store
                .start_child(run, request, providers, holder, call),
            None => self
                .store
                .start_checked(run, expected, providers, holder, call),
        }
    }
    pub fn resume(&self, input: &Resume, call: &Request<'_>) -> Result<RunRecord> {
        Self::scan(&input.prompt)?;
        self.store.resume(input, call)
    }
    pub fn events(
        &self,
        after: u64,
        limit: usize,
        attempt: Option<AttemptId>,
        call: &Request<'_>,
    ) -> Result<Vec<RunEvent>> {
        self.store.events(after, limit, attempt, call)
    }
    pub fn reconcile(
        &self,
        run: RunId,
        providers: &dyn Providers,
        call: &Request<'_>,
    ) -> Result<Vec<(AttemptId, Reconciled)>> {
        self.store.reconcile(run, providers, call)
    }
}

/// One declaration per kind keeps the enum, its stable code and its catalogue
/// summary together; the CLI help catalogue and generated reference read them.
macro_rules! error_kinds {
    ($($kind:ident => $code:literal, $summary:literal;)*) => {
        /// Refusal categories stay typed until an edge renders its stable code.
        /// Each refusal has its own code, so callers and tests match the code,
        /// never the message.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum ErrorKind {
            $(#[doc = $summary] $kind,)*
        }
        impl ErrorKind {
            /// Every kind, in catalogue order.
            pub const ALL: &[Self] = &[$(Self::$kind,)*];
            /// The stable `error.code` the CLI envelope carries.
            pub const fn code(self) -> &'static str {
                match self {
                    $(Self::$kind => $code,)*
                }
            }
            /// What the code means, for the help catalogue and reference.
            pub const fn summary(self) -> &'static str {
                match self {
                    $(Self::$kind => $summary,)*
                }
            }
        }
    };
}
error_kinds! {
    Invalid => "INVALID", "The request or a stored run record is invalid.";
    Refused => "REFUSED", "The execution context or a policy refused the operation.";
    WorktreeMissing => "WORKTREE_MISSING", "The named local worktree does not exist.";
    WorktreeUnregistered => "WORKTREE_UNREGISTERED", "The worktree is not registered with Provenant.";
    OperationConflict => "OPERATION_CONFLICT", "The operation ID was already used for a different request.";
    NotFound => "NOT_FOUND", "No run record matches in this scope.";
    Nonterminal => "NONTERMINAL", "The attempt has not finished.";
    ProviderStateExpired => "PROVIDER_STATE_EXPIRED", "Retention removed the provider state this attempt needs to resume.";
    Conflict => "CONFLICT", "The run record changed concurrently or is immutable.";
    Cancelled => "CANCELLED", "The call was cancelled.";
    LaneDepth => "LANE_DEPTH", "Nested lane depth exceeds the configured maximum.";
    QueueFull => "QUEUE_FULL", "The host queue is full.";
    Contention => "CONTENTION", "The run store is contended or the call budget expired.";
    Io => "IO", "Filesystem, database or other I/O failed.";
    Unsupported => "UNSUPPORTED", "The route or adapter does not support the requested launch.";
    ContinuationUnsupported => "CONTINUATION_UNSUPPORTED", "The attempt's provider session cannot be continued.";
    Time => "TIME", "The host clock could not be read.";
    Entropy => "ENTROPY", "The host could not supply random identifiers.";
    LaneContext => "LANE_CONTEXT", "The lane environment is missing or inconsistent.";
    Identity => "IDENTITY", "The project or actor identity could not be resolved.";
    Config => "CONFIG", "Configuration could not be loaded or is invalid.";
    Git => "GIT", "Git is unavailable or a Git query failed.";
    SecretInput => "SECRET_INPUT", "The prompt or input appears to contain a secret.";
    WorktreeRequired => "WORKTREE_REQUIRED", "The requested mode needs a worktree.";
    WorktreeNotApplicable => "WORKTREE_NOT_APPLICABLE", "A worktree was given for a mode that does not use one.";
    ParentAttempt => "PARENT_ATTEMPT", "The parent attempt is missing or cannot start children.";
    Internal => "INTERNAL", "An internal invariant failed.";
    Limit => "LIMIT", "A requested size or count exceeds its bound.";
    Prices => "PRICES", "Plan prices could not be read or are invalid.";
    Fabric => "FABRIC", "A Fabric operation failed.";
    StateCorrupt => "STATE_CORRUPT", "A stored record or artifact is corrupt.";
    LaunchUnknown => "LAUNCH_UNKNOWN", "The launch handoff was not confirmed; reconcile the run.";
    UnknownHost => "UNKNOWN_HOST", "No configured peer has this host label.";
    PeerUnavailable => "PEER_UNAVAILABLE", "The peer host could not be reached.";
    PeerVersion => "PEER_VERSION", "The peer host runs an incompatible version.";
    OwnerUnavailable => "OWNER_UNAVAILABLE", "The run owner is unavailable in this build or host.";
    BaseUnreachable => "BASE_UNREACHABLE", "The Git base cannot be resolved or fetched.";
    BranchExists => "BRANCH_EXISTS", "The branch already exists.";
    LaneCoordinationRefused => "lane_coordination_refused", "Lane coordination policy refused the operation.";
    UpgradeRefused => "UPGRADE_REFUSED", "The upgrade was refused.";
    UpgradeRequired => "upgrade_required", "A record needs a newer Provenant to read it.";
    Timeout => "TIMEOUT", "The wait budget expired.";
    Peer => "PEER", "The peer host returned an error.";
    WriterScope => "WRITER_SCOPE", "The writer worktree scope was refused.";
    Confinement => "CONFINEMENT", "Confinement refused the requested access.";
    DiskFloor => "DISK_FLOOR", "Free disk space is below gc.free_floor; run `provenant gc --apply`.";
    Processes => "HOST_PROCESSES", "The user process limit has insufficient headroom.";
    MemoryPressure => "MEMORY_PRESSURE", "Host memory pressure is too high to admit a lane.";
    LaneCap => "LANE_CAP", "The host or project writer lane cap is reached.";
    BuildSettings => "BUILD_SETTINGS", "The host lane build settings are invalid.";
    GuardianIdentity => "GUARDIAN_IDENTITY", "The guardian process identity could not be observed.";
    HostClock => "HOST_CLOCK", "Startup spacing cannot be judged: the monotonic clock or boot identity is unreadable.";
    WriterRefLocked => "WRITER_REF_LOCKED", "A writer Git ref lock already exists.";
    ObjectImportLimit => "OBJECT_IMPORT_LIMIT", "A lane's Git object import exceeded its limits.";
}
impl From<&RunError> for ErrorKind {
    fn from(error: &RunError) -> Self {
        match error {
            RunError::Input { kind, .. } => *kind,
            RunError::InvalidTaskTitle(_) | RunError::AttemptReservationRetained => Self::Invalid,
            RunError::WorktreeMissing(_) => Self::WorktreeMissing,
            RunError::WorktreeUnregistered(_) => Self::WorktreeUnregistered,
            RunError::Writer(_) => Self::WriterScope,
            RunError::Confinement(_) => Self::Confinement,
            RunError::Refused => Self::Refused,
            RunError::Admission(AdmissionError::Disk { .. }) => Self::DiskFloor,
            RunError::Admission(AdmissionError::MemoryPressure { .. }) => Self::MemoryPressure,
            RunError::Admission(AdmissionError::Processes { .. }) => Self::Processes,
            RunError::Admission(AdmissionError::LaneCap { .. }) => Self::LaneCap,
            RunError::Admission(AdmissionError::BuildSettings) => Self::BuildSettings,
            RunError::Admission(AdmissionError::GuardianIdentity) => Self::GuardianIdentity,
            RunError::Admission(AdmissionError::Clock) => Self::HostClock,
            RunError::WriterRefLocked(_) => Self::WriterRefLocked,
            RunError::GitUnavailable => Self::Git,
            RunError::ObjectImportLimit(_) => Self::ObjectImportLimit,
            RunError::Validation(_)
            | RunError::WriterHeadNotLaneBranch
            | RunError::ReviewAcceptanceRequired
            | RunError::InvalidBase(_)
            | RunError::TooLarge { .. } => Self::Invalid,
            RunError::OperationConflict => Self::OperationConflict,
            RunError::NotFound => Self::NotFound,
            RunError::Nonterminal => Self::Nonterminal,
            RunError::ProviderStateExpired => Self::ProviderStateExpired,
            RunError::Conflict | RunError::GenerationConflict { .. } => Self::Conflict,
            RunError::Cancelled => Self::Cancelled,
            RunError::DepthExceeded { .. } | RunError::DepthOverflow => Self::LaneDepth,
            RunError::Capacity => Self::QueueFull,
            RunError::Contention => Self::Contention,
            RunError::Decode(provenant_records::DecodeError::UnsupportedVersion {
                found, ..
            }) if *found > 1 => Self::UpgradeRequired,
            RunError::Decode(_) | RunError::ArtifactMismatch(_) => Self::StateCorrupt,
            _ => Self::Io,
        }
    }
}

/// Prepared transport-independent dispatch input.
pub struct DispatchTask {
    pub context: Option<provenant_records::run::WorkContext>,
    pub build_heavy: bool,
    pub title: Option<String>,
    pub task_id: provenant_core::TaskId,
    pub prompt: String,
    pub base: Option<String>,
    /// Commit pinned by owned worktree creation. Keep `base` for replay comparison.
    pub pinned_base: Option<provenant_core::GitOid>,
    pub head: Option<String>,
    pub route: RequestedRoute,
    pub account: Option<String>,
    pub fallback: Vec<RouteRecord>,
    pub profile: HarnessProfile,
    pub timeout_seconds: u64,
    pub delivery_id: Option<provenant_core::DeliveryId>,
    pub deliver: Option<WriterDeliveryRequest>,
    pub worktree: Option<std::path::PathBuf>,
    pub access: LaneAccess,
}
pub struct DispatchRequest {
    pub chair: Option<ChairBinding>,
    pub operation_id: OperationId,
    pub tasks: Vec<DispatchTask>,
    pub concurrency: u64,
    pub lane_depth: u64,
    pub parent_attempt: Option<AttemptId>,
}
fn input_error(kind: ErrorKind, message: impl Into<String>) -> RunError {
    RunError::Input {
        kind,
        message: message.into(),
    }
}
fn fresh<T>(make: impl FnOnce([u8; 16]) -> T) -> Result<T> {
    provenant_os::fs::entropy_128()
        .map(make)
        .map_err(RunError::from)
}

/// Refuse a request outside an adapter's declared launch table. This runs
/// before any writer is prepared or any run, reservation or receipt exists.
/// Named routes resolve their adapter later, where routing checks effort.
fn check_launch_capabilities(task: &DispatchTask, route: &RequestedRoute) -> Result<()> {
    let write = task.worktree.is_some() || !task.access.write_roots.is_empty();
    let check = |provider: &str, model: Option<&str>, effort: Option<&str>| {
        let Some(spec) = provenant_records::AdapterSpec::get(provider) else {
            return Ok(());
        };
        spec.check_launch(&provenant_records::LaunchRequest {
            model,
            effort,
            resume: false,
            network: task.access.network,
            write,
        })
        .map_err(|error| input_error(ErrorKind::Unsupported, error.to_string()))
    };
    match route {
        RequestedRoute::Model {
            provider,
            model,
            effort,
            ..
        } => check(provider, Some(model), effort.as_deref())?,
        RequestedRoute::Adapter { provider, .. } => check(provider, None, None)?,
        _ => {}
    }
    for route in &task.fallback {
        if let Some(provider) = route.provider.as_deref().or_else(|| {
            route
                .model
                .as_deref()
                .and_then(|model| model.split_once('/'))
                .map(|(provider, _)| provider)
        }) {
            check(provider, route.model.as_deref(), route.effort.as_deref())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod target_tests {
    use super::*;
    use crate::tests::{call, clock, context, input};
    use provenant_core::CancelToken;

    #[test]
    fn bare_target_resolves_attempts_and_unambiguous_runs()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let first = input();
        store.create(&first, &call)?;
        let service = Service::new(&store);
        let attempt_id = first.attempts[0].attempt_id;
        let attempt = service.resolve_target(attempt_id, true, &call)?;
        assert_eq!(attempt.run, first.run_id);
        assert_eq!(attempt.attempt, Some(attempt_id));
        let run_id = AttemptId::from_bytes(*first.run_id.as_bytes());
        let run = service.resolve_target(run_id, true, &call)?;
        assert_eq!(run.run, first.run_id);
        assert_eq!(run.attempt, None);
        assert!(matches!(
            service.resolve_target(run_id, false, &call),
            Err(RunError::NotFound)
        ));
        assert!(matches!(
            service.resolve_target(AttemptId::from_bytes([9; 16]), true, &call),
            Err(RunError::NotFound)
        ));

        let mut colliding = input();
        colliding.operation_id = OperationId::parse("colliding")?;
        colliding.run_id = RunId::from_bytes(*attempt_id.as_bytes());
        colliding.attempts[0].attempt_id = AttemptId::from_bytes([8; 16]);
        store.create(&colliding, &call)?;
        assert!(matches!(
            service.resolve_target(attempt_id, true, &call),
            Err(RunError::Conflict)
        ));
        assert_eq!(
            service.resolve_target(attempt_id, false, &call)?.attempt,
            Some(attempt_id)
        );
        Ok(())
    }
}
