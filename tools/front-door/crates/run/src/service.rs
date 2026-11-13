//! One typed execution API for command and MCP edges.
use crate::*;
use provenant_core::{AttemptId, CancelToken, OperationId, RunId};
use provenant_records::CanonicalV1;
use std::path::Path;

/// Trusted origin facts retained only if submission is refused before admission.
/// They never enter the canonical request or an accepted operation's identity.
#[derive(Clone, Debug, Default)]
pub struct RefusalContext {
    pub chair: Option<ChairBinding>,
    pub intent_digest: Option<provenant_core::Digest>,
}

pub struct Service<'a> {
    store: &'a RunStore,
    refusal: RefusalContext,
}
impl Service<'_> {
    /// Whether the lane access grants any write authority.
    pub fn has_writes(writer: bool, write_roots: &[std::path::PathBuf]) -> bool {
        writer || !write_roots.is_empty()
    }

    /// Admission treats declared writes and prepared tool/project exceptions
    /// alike. Provider bookkeeping and private scratch remain advisory.
    pub(crate) fn lane_class(
        writer: bool,
        write_roots: &[std::path::PathBuf],
        grants: &[WriteGrantRecord],
    ) -> LaneClass {
        use provenant_records::run::WriteSource;
        let explicit_writes = grants.iter().any(|grant| {
            matches!(
                grant.source,
                WriteSource::WriteRoot
                    | WriteSource::ToolWrite
                    | WriteSource::ReadOnlyException
                    | WriteSource::LaneCache
            )
        });
        if Self::has_writes(writer || explicit_writes, write_roots) {
            LaneClass::Writer
        } else {
            LaneClass::ReadOnly
        }
    }

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
        Self {
            store,
            refusal: RefusalContext::default(),
        }
    }
    /// A trusted host may bind refusal notices without changing the canonical
    /// request, accepted operation digest or ordinary settlement notifications.
    pub fn with_refusal_context(mut self, context: RefusalContext) -> Self {
        self.refusal = context;
        self
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
        if let Some(operation) = &operation
            && operation.state == OperationState::Refused
            && operation.extra.get("preflight_digest_version") == Some(&serde_json::json!(1))
        {
            if operation.request_digest != preflight_digest(&input, cwd)? {
                return Err(RunError::OperationConflict);
            }
            return Err(RunError::RecordedRefusal(Box::new(
                operation
                    .refusal
                    .clone()
                    .ok_or(RunError::Validation("refusal diagnostic missing"))?,
            )));
        }
        let previous = match &operation {
            Some(operation) if operation.state == OperationState::Refused => None,
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
        let mut nested_refusal = None;
        // Refuse every task's unsupported launch before any task prepares a writer.
        for task in &input.tasks {
            let route = Self::route(task.route.clone())?;
            if let Err(error) = check_launch_capabilities(task, &route) {
                if input.parent_attempt.is_none() {
                    return self.preflight_refusal(&input, task, cwd, error, call);
                }
                nested_refusal.get_or_insert((error, task.worktree.clone()));
            }
        }
        // Scan every prompt before preparing any writer. A later task must not
        // leave an earlier task's writer behind when its input is refused.
        for task in &input.tasks {
            if let Err(error) = Self::scan(&task.prompt) {
                if input.parent_attempt.is_none() {
                    return self.preflight_refusal(&input, task, cwd, error, call);
                }
                nested_refusal.get_or_insert((error, task.worktree.clone()));
            }
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
            if let Some(title) = &task.title {
                provenant_core::validate_task_title(title)
                    .map_err(|message| RunError::InvalidTaskTitle(message.into()))?;
            }
            // The host validates a requested database capability against the
            // sealed parent. Other nested access additions keep their existing
            // preflight restriction.
            if input.parent_attempt.is_some() {
                let mut other_access = task.access.clone();
                other_access.database_ipc = false;
                other_access.extra.remove("capability_sources");
                if !other_access.is_default() {
                    return Err(input_error(
                        ErrorKind::Unsupported,
                        "a nested dispatch cannot set network, read-root or write-root access",
                    ));
                }
            }
            if task.deliver.is_some() && task.worktree.is_none() {
                return Err(input_error(
                    ErrorKind::Invalid,
                    "delivery requires a writer worktree",
                ));
            }
            let mut writer = task
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
            // Preserve the sealed request and authority digest across custody metadata
            // upgrades. Only the absent legacy attribution binding is equivalent.
            if let Some(saved) = previous
                .as_ref()
                .and_then(|request| {
                    request
                        .tasks
                        .iter()
                        .find(|prior| prior.task_id == task.task_id)
                })
                .and_then(|task| task.writer.as_ref())
                && writer.as_ref().is_some_and(|resolved| {
                    equivalent_writer(saved, resolved, self.store.context.project_id)
                })
            {
                writer = Some(saved.clone());
            }
            if input.parent_attempt.is_none() {
                let branch = match &writer {
                    Some(writer) => Some(
                        prepare_writer_with_facts(
                            writer,
                            self.store.writer_registry(),
                            self.store.context.project_id,
                            self.store.repo_facts(),
                        )?
                        .branch,
                    ),
                    None => crate::pr_contribution::read_only_branch(
                        self.store.writer_registry(),
                        self.store.context.project_id,
                        task.access
                            .source_root
                            .as_deref()
                            .filter(|_| task.access.source_live)
                            .unwrap_or(cwd),
                        self.store.repo_facts(),
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
            let git_cwd = writer.as_ref().map_or_else(
                || {
                    task.access
                        .source_root
                        .as_deref()
                        .filter(|_| task.access.source_live)
                        .unwrap_or(cwd)
                },
                |writer| &writer.worktree,
            );
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
                    let resolved = resolve_git_base_with_facts(
                        &git,
                        git_cwd,
                        sha,
                        self.store.repo_facts(),
                        &CancelToken::new(),
                    )
                    .map_err(|_| {
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
                        resolve_git_base_with_facts(
                            &git,
                            git_cwd,
                            base,
                            self.store.repo_facts(),
                            &CancelToken::new(),
                        )
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
                context: Some(crate::store::lane_context(task.context, task.task_id)),
                extra: Default::default(),
                build_heavy: task.build_heavy,
                title: task.title,
                profile: task.profile,
                task_id: task.task_id,
                prompt_digest: CanonicalV1::file_digest(task.prompt.as_bytes()),
                base_ref,
                base,
                head: task.head,
                source_head: task.source_head,
                route: task.route,
                account: task.account,
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
        if let Some((error, worktree)) = nested_refusal {
            let diagnostic = ErrorKind::from(&error)
                .diagnostic()
                .detail(error.to_string());
            let worktree = worktree
                .as_deref()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .map(str::to_owned);
            let saved = self.store.record_refusal_with_context(
                &prepared,
                diagnostic,
                worktree,
                &self.refusal,
                call,
            )?;
            if saved.state != OperationState::Refused {
                return Err(RunError::OperationConflict);
            }
            return Err(RunError::RecordedRefusal(Box::new(
                saved
                    .refusal
                    .ok_or(RunError::Validation("refusal diagnostic missing"))?,
            )));
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
    fn preflight_refusal(
        &self,
        input: &DispatchRequest,
        task: &DispatchTask,
        cwd: &Path,
        error: RunError,
        call: &Request<'_>,
    ) -> Result<RunRecord> {
        let mut extra = provenant_records::ExtraFields::default();
        extra.insert("preflight_digest_version".into(), serde_json::json!(1));
        if let Some(notice) = task.profile.knobs.get("chair.refusal_notice") {
            extra.insert("refusal_notice".into(), notice.clone());
        }
        if let Some(digest) = self.refusal.intent_digest {
            extra.insert("origin_intent_digest".into(), serde_json::json!(digest));
        }
        let diagnostic = ErrorKind::from(&error)
            .diagnostic()
            .detail(error.to_string());
        let worktree = task
            .worktree
            .as_ref()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .map(str::to_owned);
        let record = OperationRecord {
            extra,
            schema_version: 1,
            project_id: self.store.context.project_id,
            actor_id: self.store.context.actor_id,
            operation_id: input.operation_id.clone(),
            kind: OperationKind::Dispatch,
            request_digest: preflight_digest(input, cwd)?,
            run_id: fresh(RunId::from_bytes)?,
            state: OperationState::Refused,
            blocked_reason: None,
            refusal: Some(diagnostic),
            worktree,
            refused_at: Some(
                call.clock
                    .wall_now()
                    .map_err(|_| RunError::Validation("wall clock"))?,
            ),
            refusal_chair: self.refusal.chair.clone().or_else(|| input.chair.clone()),
        };
        // A launch is refused under the work key it would have taken.
        let context = crate::store::lane_context(task.context.clone(), task.task_id);
        let key = crate::store::work_key(self.store.context.project_id, Some(&context))?;
        let saved = self.store.insert_refusal_under(&key, record, call)?;
        if saved.state != OperationState::Refused {
            return Err(RunError::OperationConflict);
        }
        Err(RunError::RecordedRefusal(Box::new(saved.refusal.ok_or(
            RunError::Validation("refusal diagnostic missing"),
        )?)))
    }
    /// Canonical model IDs carry the provider prefix in every stored request.
    pub fn route(route: RequestedRoute) -> Result<RequestedRoute> {
        let extra = match &route {
            RequestedRoute::Adapter { extra, .. }
            | RequestedRoute::Model { extra, .. }
            | RequestedRoute::Named { extra, .. }
            | RequestedRoute::Review { extra, .. } => extra,
        };
        if let Some(family) = extra.get("required_family")
            && family.as_str().is_none_or(str::is_empty)
        {
            return Err(input_error(
                ErrorKind::Invalid,
                "required_family must name a provider family",
            ));
        }
        let family_extra = |extra: provenant_records::ExtraFields| {
            extra
                .into_iter()
                .filter(|(key, _)| key == "required_family")
                .collect()
        };
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
                extra,
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
                    extra: family_extra(extra),
                    provider,
                    model,
                    effort,
                })
            }
            RequestedRoute::Named {
                name,
                effort,
                extra,
            } if !name.is_empty() => Ok(RequestedRoute::Named {
                extra: family_extra(extra),
                name,
                effort,
            }),
            RequestedRoute::Review {
                name,
                effort,
                excluded_families,
                distinct_families,
                preferred_families,
                require_distinct,
                extra,
            } if !name.is_empty() => Ok(RequestedRoute::Review {
                extra: family_extra(extra),
                name,
                effort,
                excluded_families,
                distinct_families,
                preferred_families,
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
        self.custody_state(self.store.record(id, call)?, call)
    }
    /// Return the terminal decision and separately indicate custody retention.
    pub fn status_for_wait(&self, id: AttemptId, call: &Request<'_>) -> Result<AttemptRecord> {
        self.status(id, call)
    }
    fn custody_state(
        &self,
        mut record: AttemptRecord,
        call: &Request<'_>,
    ) -> Result<AttemptRecord> {
        if matches!(record.state, AttemptState::Terminal { .. }) {
            record.custody_pending =
                Some(!self.store.custody_released(&record, call).unwrap_or(false));
            let restored = self
                .store
                .attempt_dir(record.attempt_id, false)
                .ok()
                .and_then(|attempt| {
                    crate::artifacts::read_sealed_observed(
                        &attempt,
                        "merge-restored.json",
                        crate::RECORD_LIMIT,
                    )
                    .ok()
                    .flatten()
                })
                .and_then(|raw| crate::db::decode::<crate::writer::MergeRestored>(&raw).ok())
                .is_some_and(|marker| {
                    marker.schema_version == 1 && marker.scope == crate::RunStore::scope(&record)
                });
            if restored && let AttemptState::Terminal { reason, .. } = &mut record.state {
                let note = "merge index, parents and working files were retained in the worktree";
                *reason = Some(
                    reason
                        .take()
                        .map_or_else(|| note.to_owned(), |old| format!("{old}; {note}")),
                );
            }
        }
        Ok(record)
    }
    pub fn lanes(&self, query: &LaneQuery, call: &Request<'_>) -> Result<LanePage> {
        let mut page = self.store.lanes(query, call)?;
        page.lanes = page
            .lanes
            .into_iter()
            .map(|record| self.custody_state(record, call))
            .collect::<Result<_>>()?;
        Ok(page)
    }
    /// Peer waits consume the same custody-aware view as local waits.
    pub fn lanes_for_wait(&self, query: &LaneQuery, call: &Request<'_>) -> Result<LanePage> {
        self.lanes(query, call)
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
    pub fn cancel(&self, id: AttemptId, reason: &str, call: &Request<'_>) -> Result<CancelReceipt> {
        self.store.cancel(id, reason, call)
    }
    pub fn start(
        &self,
        run: RunId,
        providers: &dyn Providers,
        holder: &dyn Fn(&AttemptRecord, provenant_core::ActorId) -> Result<Vec<u8>>,
        expected: &CanonicalRequest,
        call: &Request<'_>,
    ) -> Result<Vec<Started>> {
        self.store
            .start_checked(run, expected, providers, holder, call)
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
    pub fn abandon_settlement(
        &self,
        id: AttemptId,
        reason: &str,
        call: &Request<'_>,
    ) -> Result<Reconciled> {
        self.store.abandon_settlement(id, reason, call)
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
/// summary together; the CLI help catalogue reads them.
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
    SharedSettingsNeverSynced => "SHARED_SETTINGS_NEVER_SYNCED", "Identity and credential settings cannot be shared between accounts.";
    UnknownAccount => "UNKNOWN_ACCOUNT", "The requested account is not registered.";
    DuplicatePacingProfile => "DUPLICATE_PACING_PROFILE", "Multiple pacing profiles resolve to the same account.";
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
    WriterBusy => "WRITER_BUSY", "The writer or its lock is held by another operation.";
    WriterLockBusy => "WRITER_LOCK_BUSY", "The writer registry lock is held by another operation.";
    WriterUnknown => "WRITER_UNKNOWN", "Writer custody could not be determined.";
    CredentialRoot => "CREDENTIAL_ROOT", "A requested grant overlaps a credential root.";
    WriterProjectMismatch => "WRITER_PROJECT_MISMATCH", "The writer worktree belongs to a different project than the selected one.";
    Confinement => "CONFINEMENT", "Confinement refused the requested access.";
    DiskFloor => "DISK_FLOOR", "Free disk space is below gc.free_floor; run `provenant gc --apply`.";
    Processes => "HOST_PROCESSES", "The user process limit has insufficient headroom.";
    MemoryPressure => "MEMORY_PRESSURE", "Host memory pressure is too high to admit a lane.";
    MemoryFloor => "MEMORY_FLOOR", "Available host memory is below the advisory admission floor.";
    StaleReadiness => "STALE_READINESS", "The selected account's readiness observation is stale.";
    LaneCap => "LANE_CAP", "The host or project lane cap is reached.";
    BuildSettings => "BUILD_SETTINGS", "The host lane build settings are invalid.";
    GuardianIdentity => "GUARDIAN_IDENTITY", "The guardian process identity could not be observed.";
    HostClock => "HOST_CLOCK", "Startup spacing cannot be judged: the monotonic clock or boot identity is unreadable.";
    RetainedWriterImport => "WRITER_IMPORT_RETAINED", "A predecessor writer import remains unresolved.";
    ObjectImportLimit => "OBJECT_IMPORT_LIMIT", "A lane's Git object import exceeded its limits.";
}
impl ErrorKind {
    /// Safe projection for refusal kinds that do not need additional facts.
    pub fn diagnostic(self) -> provenant_records::diagnostic::Diagnostic {
        if self == Self::SharedSettingsNeverSynced {
            return provenant_config::shared_settings::never_synced_diagnostic();
        }
        if self == Self::StaleReadiness {
            return provenant_records::RouteExclusionReason::StaleAvailability
                .diagnostic(self.code())
                .category(self.category());
        }
        let diagnostic =
            provenant_records::diagnostic::Diagnostic::new(self.code()).category(self.category());
        match self {
            Self::UnknownAccount => {
                diagnostic.next("run `provenant accounts list` and select a registered account")
            }
            Self::DuplicatePacingProfile => {
                diagnostic.next("remove duplicate pacing profiles for the same account")
            }
            _ => diagnostic,
        }
    }

    /// Stable category shared by the output adapters.
    pub const fn category(self) -> &'static str {
        match self {
            Self::MemoryFloor | Self::StaleReadiness => "warning",
            Self::Invalid
            | Self::OperationConflict
            | Self::NotFound
            | Self::WorktreeMissing
            | Self::UnknownAccount
            | Self::SharedSettingsNeverSynced
            | Self::DuplicatePacingProfile => "invalid",
            Self::WorktreeUnregistered
            | Self::Refused
            | Self::WriterScope
            | Self::WriterBusy
            | Self::WriterLockBusy
            | Self::WriterUnknown
            | Self::RetainedWriterImport
            | Self::CredentialRoot
            | Self::WriterProjectMismatch
            | Self::Confinement
            | Self::DiskFloor
            | Self::MemoryPressure
            | Self::LaneCap
            | Self::BuildSettings
            | Self::GuardianIdentity
            | Self::Nonterminal
            | Self::ProviderStateExpired
            | Self::Conflict
            | Self::LaneDepth
            | Self::QueueFull
            | Self::SecretInput
            | Self::Unsupported
            | Self::ContinuationUnsupported => "refused",
            _ => "error",
        }
    }

    /// The hard boundary this code protects; non-boundary failures remain typed.
    pub const fn boundary_class(self) -> provenant_records::diagnostic::BoundaryClass {
        use provenant_records::diagnostic::BoundaryClass as B;
        match self {
            Self::CredentialRoot | Self::SharedSettingsNeverSynced => B::Credentials,
            Self::ObjectImportLimit | Self::StateCorrupt => B::Integrity,
            Self::RetainedWriterImport => B::LaneGit,
            Self::WriterScope
            | Self::WriterBusy
            | Self::WriterLockBusy
            | Self::WriterUnknown
            | Self::WriterProjectMismatch
            | Self::GuardianIdentity
            | Self::LaneCoordinationRefused => B::Authority,
            _ => B::None,
        }
    }
}
impl From<&RunError> for ErrorKind {
    fn from(error: &RunError) -> Self {
        match error {
            RunError::Input { kind, .. } => *kind,
            RunError::ContinuationRefused { .. } => Self::ContinuationUnsupported,
            RunError::InvalidTaskTitle(_) | RunError::AttemptReservationRetained => Self::Invalid,
            RunError::WorktreeMissing(_) => Self::WorktreeMissing,
            RunError::WorktreeUnregistered(_) => Self::WorktreeUnregistered,
            RunError::WriterProjectMismatch { .. } => Self::WriterProjectMismatch,
            RunError::Writer(
                provenant_confinement::worktrees::WorktreeError::RegistryLockTimeout,
            ) => Self::WriterLockBusy,
            RunError::Writer(
                provenant_confinement::worktrees::WorktreeError::LockBusy
                | provenant_confinement::worktrees::WorktreeError::LiveWriter
                | provenant_confinement::worktrees::WorktreeError::WriterBusy { .. },
            ) => Self::WriterBusy,
            RunError::Writer(
                provenant_confinement::worktrees::WorktreeError::UnknownWriter
                | provenant_confinement::worktrees::WorktreeError::UnknownWriterAt { .. }
                | provenant_confinement::worktrees::WorktreeError::CensusUnknown { .. }
                | provenant_confinement::worktrees::WorktreeError::UnknownOccupancy { .. }
                | provenant_confinement::worktrees::WorktreeError::IncompleteCensus { .. },
            ) => Self::WriterUnknown,
            RunError::Writer(_) => Self::WriterScope,
            RunError::Confinement(
                provenant_confinement::permissions::PermissionError::CredentialRoot
                | provenant_confinement::permissions::PermissionError::CredentialRootAt { .. }
                | provenant_confinement::permissions::PermissionError::CredentialGrant { .. },
            ) => Self::CredentialRoot,
            RunError::Confinement(_) => Self::Confinement,
            RunError::Refused => Self::Refused,
            RunError::RecordedRefusal(diagnostic) => Self::ALL
                .iter()
                .copied()
                .find(|kind| kind.code() == diagnostic.code)
                .unwrap_or(Self::Refused),
            RunError::Admission(AdmissionError::Disk { .. }) => Self::DiskFloor,
            RunError::Admission(AdmissionError::MemoryPressure { .. }) => Self::MemoryPressure,
            RunError::Admission(AdmissionError::Processes { .. }) => Self::Processes,
            RunError::Admission(AdmissionError::LaneCap { .. }) => Self::LaneCap,
            RunError::Admission(AdmissionError::BuildSettings) => Self::BuildSettings,
            RunError::Admission(AdmissionError::GuardianIdentity) => Self::GuardianIdentity,
            RunError::Admission(AdmissionError::Clock) => Self::HostClock,
            RunError::RetainedWriterImport { .. } => Self::RetainedWriterImport,
            RunError::GitUnavailable => Self::Git,
            RunError::ObjectImportLimit(_) => Self::ObjectImportLimit,
            RunError::Validation(_)
            | RunError::WriterLineage(_)
            | RunError::WriterHeadNotLaneBranch
            | RunError::WriterSourceMismatch { .. }
            | RunError::ReviewAcceptanceRequired
            | RunError::InvalidBase(_)
            | RunError::PromptTooLarge { .. }
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
#[derive(serde::Serialize)]
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
    /// Original source pin for a writer continuing a settled predecessor.
    pub source_head: Option<String>,
    pub route: RequestedRoute,
    pub account: Option<String>,
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
/// Bind an early refusal to all supplied inputs without persisting prompt bytes.
fn preflight_digest(input: &DispatchRequest, cwd: &Path) -> Result<provenant_core::Digest> {
    Ok(CanonicalV1::record_digest(
        "dispatch-preflight",
        1,
        &(
            &input.tasks,
            input.concurrency,
            input.lane_depth,
            input.parent_attempt,
            cwd,
        ),
    )?)
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
    let write = Service::has_writes(task.worktree.is_some(), &task.access.write_roots);
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
            skill_location: task.profile.require_skill_location,
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
    Ok(())
}

#[cfg(test)]
mod target_tests {
    use super::*;
    use crate::tests::{call, clock, context, input};
    use provenant_core::CancelToken;

    #[test]
    fn registry_lock_contention_is_distinct_from_a_live_writer() {
        use provenant_confinement::worktrees::WorktreeError;
        assert_eq!(
            ErrorKind::from(&RunError::Writer(WorktreeError::LockBusy)),
            ErrorKind::WriterBusy
        );
        assert_eq!(
            ErrorKind::from(&RunError::Writer(WorktreeError::RegistryLockTimeout)),
            ErrorKind::WriterLockBusy
        );
        assert_eq!(
            ErrorKind::from(&RunError::Writer(WorktreeError::LiveWriter)),
            ErrorKind::WriterBusy
        );
        assert_eq!(
            ErrorKind::from(&RunError::Writer(WorktreeError::UnknownWriter)),
            ErrorKind::WriterUnknown
        );
    }

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
