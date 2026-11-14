use super::*;

/// Called once, as soon as a submitted graph's first checkpoint is saved.
pub(crate) type Announce<'a> = &'a dyn Fn(
    &provenant_fabric_runtime::graph::GraphDocument,
    &provenant_fabric_runtime::graph::GraphCheckpoint,
);

/// A journal that announces the first saved checkpoint.
struct AnnouncingJournal<'a, J> {
    journal: &'a mut J,
    document: &'a provenant_fabric_runtime::graph::GraphDocument,
    announce: Option<Announce<'a>>,
}
impl<J: provenant_fabric_runtime::graph::GraphJournal> provenant_fabric_runtime::graph::GraphJournal
    for AnnouncingJournal<'_, J>
{
    type Error = J::Error;
    fn load(
        &mut self,
        graph_id: &str,
    ) -> Result<Option<provenant_fabric_runtime::graph::GraphCheckpoint>, Self::Error> {
        self.journal.load(graph_id)
    }
    fn save(
        &mut self,
        checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
    ) -> Result<(), Self::Error> {
        self.journal.save(checkpoint)?;
        if let Some(announce) = self.announce.take() {
            announce(self.document, checkpoint);
        }
        Ok(())
    }
    fn apply_cancellation(
        &mut self,
        document: &provenant_fabric_runtime::graph::GraphDocument,
        checkpoint: &mut provenant_fabric_runtime::graph::GraphCheckpoint,
    ) -> Result<bool, Self::Error> {
        self.journal.apply_cancellation(document, checkpoint)
    }
}

pub(super) fn cancellation_receipt(
    mut run: provenant_fabric_runtime::graph::GraphRun,
    request_id: String,
) -> provenant_fabric_runtime::graph::GraphRun {
    run.checkpoint.extra.insert(
        "cancellation_request_receipt".into(),
        serde_json::json!({"request_id":request_id}),
    );
    run
}

/// Discover pending journals and check deadlines under the same lock as explicit advance.
pub(crate) fn tick_graphs(
    resolved: &crate::roots::Resolved,
    now: provenant_core::WallTime,
    workers: &'static dyn WorkerObserver,
) -> Result<crate::tick::Graphs, Box<dyn std::error::Error>> {
    use provenant_fabric_runtime::graph::{FileJournal, GraphJournal, advance_with_budget};
    let mut report = crate::tick::Graphs::default();
    for binding in provenant_config::registered_projects(&resolved.roots)? {
        let project = binding.project_id();
        let ids = match FileJournal::pending_graph_ids(&resolved.roots.state, project) {
            Ok(ids) => ids,
            Err(error) => {
                report.errors.push(format!("{project}: {error}"));
                continue;
            }
        };
        for id in ids {
            let result = (|| -> Result<bool, Box<dyn std::error::Error>> {
                let checkpoint =
                    FileJournal::read_checkpoint_for_tick(&resolved.roots.state, project, &id)?;
                let Some(checkpoint) = checkpoint else {
                    // An already reported unreadable checkpoint must stay marked.
                    if matches!(
                        FileJournal::read_checkpoint(&resolved.roots.state, project, &id),
                        Ok(None)
                    ) {
                        let clock = SystemClock::new();
                        let cancel = CancelToken::new();
                        match FileJournal::open(
                            &resolved.roots.state,
                            project,
                            &id,
                            WaitBudget::nonblocking(clock.mono_now()),
                            &cancel,
                            &clock,
                        ) {
                            Ok(mut journal) => journal.clear_stale_pending()?,
                            Err(provenant_fabric_runtime::graph::JournalError::Filesystem(
                                provenant_os::fs::FsError::LockTimeout { .. },
                            )) => {}
                            Err(error) => return Err(error.into()),
                        }
                    }
                    return Ok(false);
                };
                let due_at_start = checkpoint.cancelled || graph_wake_due(&checkpoint, now);
                let clock = SystemClock::new();
                let cancel = CancelToken::new();
                let mut journal = match FileJournal::open(
                    &resolved.roots.state,
                    project,
                    &id,
                    WaitBudget::nonblocking(clock.mono_now()),
                    &cancel,
                    &clock,
                ) {
                    Ok(journal) => journal,
                    Err(provenant_fabric_runtime::graph::JournalError::Filesystem(
                        provenant_os::fs::FsError::LockTimeout { .. },
                    )) => return Ok(false),
                    Err(error) => return Err(error.into()),
                };
                let Some(checkpoint) = journal.load(&id)? else {
                    return Ok(false);
                };
                // Execution can be rebound to another worktree by a manual
                // resume, so resolve it only after acquiring journal custody.
                let Some(execution) =
                    FileJournal::pending_execution(&resolved.roots.state, project, &id)?
                else {
                    journal.clear_stale_pending()?;
                    return Ok(false);
                };
                match std::fs::metadata(&execution.directory) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        let reason = format!(
                            "graph {id}: worktree is missing; restore it or cancel this graph"
                        );
                        if FileJournal::read_advance_error(&resolved.roots.state, project, &id)?
                            .as_deref()
                            != Some(reason.as_str())
                        {
                            FileJournal::set_advance_error(
                                &resolved.roots.state,
                                project,
                                &id,
                                Some(&reason),
                            )?;
                            report.warnings.push(reason);
                        }
                        return Ok(false);
                    }
                    Err(error) => return Err(error.into()),
                    Ok(_) => {}
                }
                if !due_at_start || !graph_wake_due(&checkpoint, clock.wall_now()?) {
                    return Ok(false);
                }
                let context =
                    Context::open(resolved.roots.clone(), &resolved.home, &execution.directory)?;
                if context.identity.project != project || context.in_lane {
                    return Err(EdgeError::Invalid.into());
                }
                // A tick has no caller wait: its launches, including writer
                // binding, have the configured launch allowance.
                let budget =
                    WaitBudget::cli(clock.mono_now(), context.config.graph.launch_wait_seconds)?;
                let factory = Factory {
                    project_attribution: crate::usage::attribution_enabled(&context.roots),
                    context,
                    clock: SystemClock::new(),
                    backoff: ThreadPause,
                    executor: SystemExecutor::new(4)?,
                    home: resolved.home.clone(),
                    cwd: execution.directory,
                    assets: resolved.assets.clone(),
                    host_launch: None,
                    workers,
                };
                let mut owner = BoundGraphOwner {
                    writer_launch_budget: None,
                    admission_timeout: None,
                    launch_warnings: Vec::new(),
                    repo_facts: Default::default(),
                    factory: &factory,
                    integration_base: checkpoint.bases,
                    integration_branch: execution.document.integration_branch.clone(),
                    network: graph_network(&execution.document)?,
                    capability_overrides: graph_capability_overrides(&execution.document)?,
                    exclude: graph_exclude(&execution.document)?,
                    budget,
                    cancel: &cancel,
                    prices: None,
                    tick: true,
                };
                let config = &factory.context.config.graph;
                advance_with_budget(
                    &execution.document,
                    &mut owner,
                    &mut journal,
                    &config.limits,
                    &config.budget,
                )?;
                Ok(true)
            })();
            match result {
                Ok(true) => {
                    report.advanced += 1;
                    if let Err(error) =
                        FileJournal::set_advance_error(&resolved.roots.state, project, &id, None)
                    {
                        report.errors.push(format!("{project}/{id}: {error}"));
                    }
                }
                Ok(false) => {}
                Err(error) => {
                    let reason = error.to_string();
                    if !error.is::<provenant_fabric_runtime::graph::UnreadableCheckpointNotice>() {
                        let _ = FileJournal::set_advance_error(
                            &resolved.roots.state,
                            project,
                            &id,
                            Some(&reason),
                        );
                    }
                    report.errors.push(format!("{project}/{id}: {reason}"));
                }
            }
        }
    }
    Ok(report)
}

pub(super) fn graph_wake_due(
    checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
    now: provenant_core::WallTime,
) -> bool {
    if checkpoint.cancelled {
        checkpoint.nodes.iter().any(|node| {
            matches!(
                node.state,
                provenant_fabric_runtime::graph::NodeState::Pending
                    | provenant_fabric_runtime::graph::NodeState::Running
            )
        })
    } else {
        checkpoint.wake_at.is_none_or(|deadline| now >= deadline)
            || checkpoint.nodes.iter().any(|node| {
                use provenant_fabric_runtime::graph::NodeState;
                node.state == NodeState::Running
                    && !node.attempts.is_empty()
                    && node
                        .launch_retry
                        .as_ref()
                        .and_then(|retry| retry.wake_at)
                        .is_none()
            })
    }
}
impl Factory {
    /// Read a project-local graph document through the confined, bounded helpers.
    pub(crate) fn read_graph_document(
        &self,
        path: &Path,
    ) -> Result<provenant_fabric_runtime::graph::GraphDocument, FacadeError> {
        Ok(self.read_graph_document_bytes(path)?.0)
    }

    pub(crate) fn read_graph_document_bytes(
        &self,
        path: &Path,
    ) -> Result<
        (
            provenant_fabric_runtime::graph::GraphDocument,
            PathBuf,
            Vec<u8>,
        ),
        FacadeError,
    > {
        let path = provenant_os::fs::canonicalize(path)
            .map_err(|_| FacadeError::io("read project-local graph document; a bare name is read only from .provenant/scratch/graphs, so pass the explicit path of a legacy .agent-run/graphs document"))?;
        let limit =
            provenant_fabric_runtime::config::GraphLimits::document_bytes_bound().maximum_u64();
        let bytes = if !path.starts_with(&self.cwd) {
            // A submission from a subdirectory is materialised in the enclosing
            // checkout's scratch. Only that graph directory may sit outside cwd.
            let graphs = self
                .graph_documents_directory()
                .map_err(|_| FacadeError::ConfinementRefused { resource: None })?;
            let directory = graphs
                .current_path()
                .map_err(|_| FacadeError::ConfinementRefused { resource: None })?;
            if path.parent() != Some(directory.as_path()) {
                return Err(FacadeError::ConfinementRefused { resource: None });
            }
            let name = path
                .file_name()
                .ok_or(FacadeError::ConfinementRefused { resource: None })?;
            // Keep the verified directory handle through the read: replacing a
            // scratch component after validation must not redirect this access.
            graphs.read_bounded(name, limit)
        } else {
            provenant_os::fs::FileHandle::open_source(&path)
                .and_then(|mut file| file.read_bounded(limit))
        }
        .map_err(|error| match error {
            provenant_os::fs::FsError::TooLarge { .. } => {
                FacadeError::invalid(InvalidInputDetail::GraphDocumentTooLarge)
            }
            _ => FacadeError::io("access graph journal"),
        })?;
        let document = decode_graph_document(&bytes)?;
        Ok((document, path, bytes))
    }

    /// The one selector resolver for graph status, cancel and resume: a graph
    /// ID, a project-local document path or a document SHA-256 hash (prefix)
    /// becomes the graph ID and, when one is retained or named, its document.
    pub(crate) fn resolve_graph(
        &self,
        selector: &str,
    ) -> Result<
        (
            String,
            Option<provenant_fabric_runtime::graph::GraphDocument>,
        ),
        FacadeError,
    > {
        use provenant_fabric_runtime::graph::FileJournal;
        let refused = || FacadeError::DispatchRefused {
            fact: None,
            disk: None,
            reason: graph_selector_forms(),
        };
        // An exact graph ID wins over every other form, but a hex ID that is
        // also a hash prefix of a document declaring another graph is ambiguous.
        let hash = selector.strip_prefix("sha256:").unwrap_or(selector);
        let hex = !hash.is_empty() && hash.bytes().all(|byte| byte.is_ascii_hexdigit());
        match FileJournal::execution(
            &self.context.roots.state,
            self.context.identity.project,
            selector,
        ) {
            Ok(Some(execution)) => {
                if hex
                    && let Ok(found) = self.documents_by_hash(hash)
                    && found.iter().any(|document| document.graph_id != selector)
                {
                    return Err(refused());
                }
                return Ok((selector.to_owned(), Some(execution.document)));
            }
            Err(error) if error.is_unreadable() => return Ok((selector.to_owned(), None)),
            _ => {}
        }
        // Then a document path: any path-shaped selector, or a bare name of an
        // existing file in the working directory or the scratch graphs directory.
        let path = Path::new(selector);
        let checkout = self.graph_checkout().map_err(|_| refused())?;
        let scratch = checkout.join(".provenant/scratch/graphs").join(path);
        let local = self.cwd.join(path);
        if path.is_absolute()
            || selector.contains(['/', '\\'])
            || selector.ends_with(".json")
            || local.is_file()
            || scratch.is_file()
        {
            let path = if path.components().count() == 1 && !local.exists() {
                scratch
            } else {
                local
            };
            return match self.read_graph_document(&path) {
                Ok(document) => Ok((document.graph_id.clone(), Some(document))),
                Err(FacadeError::Io { .. }) => Err(refused()),
                Err(error) => Err(error),
            };
        }
        // An unknown ID, or a hash with no match, is not found; an ambiguous
        // prefix or an unsafe graphs directory is refused.
        if !hex {
            return Err(FacadeError::NotFound);
        }
        let Ok(found) = self.documents_by_hash(hash) else {
            return Err(refused());
        };
        match <[_; 1]>::try_from(found) {
            Ok([document]) => Ok((document.graph_id.clone(), Some(document))),
            Err(found) if found.is_empty() => Err(FacadeError::NotFound),
            Err(_) => Err(refused()),
        }
    }

    /// The scratch graph documents whose SHA-256 starts with `hash`. Symlinked
    /// and non-regular entries fail the no-follow bounded read; an unsafe
    /// graphs directory is an error.
    pub(crate) fn documents_by_hash(
        &self,
        hash: &str,
    ) -> Result<Vec<provenant_fabric_runtime::graph::GraphDocument>, provenant_os::fs::FsError>
    {
        let graphs = self.graph_documents_directory()?;
        let prefix = hash.to_ascii_lowercase();
        let mut found = Vec::new();
        for name in graphs.entries()?.flatten() {
            if let Ok(bytes) = graphs.read_bounded(
                &name,
                provenant_fabric_runtime::config::GraphLimits::document_bytes_bound().maximum_u64(),
            ) && provenant_records::CanonicalV1::file_digest(&bytes)
                .to_string()
                .strip_prefix("sha256:")
                .is_some_and(|digest| digest.starts_with(&prefix))
                && let Ok(document) = decode_graph_document(&bytes)
            {
                found.push(document);
            }
        }
        Ok(found)
    }

    pub(crate) fn graph(
        &self,
        input: GraphInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<provenant_fabric_runtime::graph::GraphRun, FacadeError> {
        self.graph_with_scratch(input, budget, cancel, None, None, None)
    }

    pub(crate) fn graph_with_scratch(
        &self,
        input: GraphInput,
        budget: WaitBudget,
        cancel: &CancelToken,
        prepared_scratch: Option<&Path>,
        admission_timeout: Option<WaitBudget>,
        announce: Option<Announce<'_>>,
    ) -> Result<provenant_fabric_runtime::graph::GraphRun, FacadeError> {
        self.graph_launch_result(
            input,
            budget,
            cancel,
            prepared_scratch,
            admission_timeout,
            announce,
            None,
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "submission replay disposition is separate from graph advancement"
    )]
    pub(crate) fn graph_launch_result(
        &self,
        input: GraphInput,
        budget: WaitBudget,
        cancel: &CancelToken,
        prepared_scratch: Option<&Path>,
        admission_timeout: Option<WaitBudget>,
        announce: Option<Announce<'_>>,
        replayed: Option<&mut bool>,
    ) -> Result<provenant_fabric_runtime::graph::GraphRun, FacadeError> {
        let facts = provenant_confinement::worktrees::RepoFacts::default();
        use provenant_fabric_runtime::coordination::input::ValidateInput;
        use provenant_fabric_runtime::graph::{FileJournal, GraphJournal};
        let input = match input {
            GraphInput::Status { extra, id } => GraphInput::Status {
                extra,
                id: self.resolve_graph(&id)?.0,
            },
            GraphInput::Cancel { extra, id, reason } => GraphInput::Cancel {
                extra,
                id: self.resolve_graph(&id)?.0,
                reason,
            },
            input => input,
        };
        input
            .validate_input(provenant_core::WaitTransport::Cli)
            .map_err(|error| {
                FacadeError::from(
                    &provenant_fabric_runtime::coordination::CoordinationError::Input(error),
                )
            })?;
        let resume = matches!(&input, GraphInput::Resume { .. });
        let mut scratch_source = None;
        let document = match &input {
            GraphInput::Run { document, .. } | GraphInput::Resume { document, .. } => {
                let scratch = match prepared_scratch {
                    Some(scratch) => scratch.to_path_buf(),
                    None => self
                        .prepare_scratch(cancel)
                        .map_err(scratch_prepare_error)?,
                };
                let mut document = if resume {
                    self.resolve_graph(&document.to_string_lossy())?
                        .1
                        .ok_or_else(|| FacadeError::DispatchRefused {
                            fact: None,
                            disk: None,
                            reason: graph_selector_forms(),
                        })?
                } else {
                    let path = if document.is_absolute() {
                        document.clone()
                    } else if document.components().count() == 1
                        && !self.cwd.join(document).exists()
                    {
                        scratch.join("graphs").join(document)
                    } else {
                        self.cwd.join(document)
                    };
                    let (document, source, bytes) = self.read_graph_document_bytes(&path)?;
                    if let Ok(graphs) = provenant_os::fs::canonicalize(&scratch.join("graphs"))
                        && source.parent() == Some(graphs.as_path())
                        && let Some(name) = source.file_name()
                    {
                        scratch_source = Some((graphs, name.to_owned(), bytes));
                    }
                    document
                };
                if !document.extra.contains_key("submission_patch") {
                    let submission_key = document
                        .extra
                        .get("provenance")
                        .and_then(|value| value.get("operation_id"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("submit:{}", document.graph_id));
                    provenant_fabric_runtime::graph::patch::submission(
                        &mut document,
                        &submission_key,
                        &self.context.config.graph.limits,
                        self.context.port_table.worktree_layout.clone(),
                    )
                    .map_err(|_| FacadeError::InvalidInput { detail: None })?;
                }
                if let Some(branch) = &document.integration_branch {
                    provenant_run::HostDeliveryContext::validate_integration_target(
                        &self.cwd, branch,
                    )
                    .map_err(run_error)?;
                }
                let checkpoint = FileJournal::read_checkpoint(
                    &self.context.roots.state,
                    self.context.identity.project,
                    &document.graph_id,
                )
                .map_err(graph_journal_error)?;
                let admitted = checkpoint.is_some();
                document
                    .admit(&self.context.config.graph.limits)
                    .map_err(FacadeError::graph_admission)?;
                let expanded = document
                    .expanded(&self.context.config.graph.limits)
                    .map_err(|reason| FacadeError::DispatchRefused {
                        reason,
                        disk: None,
                        fact: None,
                    })?;
                // A new writer does not exist until the kernel has journalled
                // its provisioning intent. Check its declared branch here;
                // provisioning still verifies the actual checkout at launch.
                let writer_branch = |node: &provenant_fabric_runtime::graph::GraphNode,
                                     path: &str|
                 -> Result<String, FacadeError> {
                    if let Some(new) = node
                        .attempt
                        .as_ref()
                        .and_then(|spec| spec.new_worktree.as_ref())
                    {
                        return Ok(new.branch.clone());
                    }
                    let writer = provenant_run::resolve_writer(
                        self.context.roots.state.as_path(),
                        &self.cwd,
                        Path::new(path),
                    )
                    .map_err(run_error)?;
                    let writer = provenant_run::prepare_writer_with_facts(
                        &writer,
                        self.context.roots.state.as_path(),
                        self.context.identity.project,
                        &facts,
                    )
                    .map_err(run_error)?;
                    Ok(writer
                        .branch
                        .as_str()
                        .trim_start_matches("refs/heads/")
                        .to_owned())
                };
                for node in &expanded.nodes {
                    if let Some(provenant_records::run::DeliveryMode::Integrate(target)) =
                        &node.delivery
                    {
                        provenant_run::HostDeliveryContext::validate_integration_target(
                            &self.cwd, target,
                        )
                        .map_err(run_error)?;
                    }
                    if !admitted
                        && node.delivery == Some(provenant_records::run::DeliveryMode::Push)
                    {
                        let path = node
                            .worktree
                            .as_deref()
                            .ok_or(FacadeError::InvalidInput { detail: None })?;
                        provenant_run::HostDeliveryContext::check_delivery_branch(
                            &self.cwd,
                            &writer_branch(node, path)?,
                        )
                        .map_err(|_| FacadeError::ConfinementRefused { resource: None })?;
                    }
                    // A local target checked out in the writer's own worktree
                    // cannot be advanced by compare-and-swap, so refuse the
                    // graph before any attempt rather than at delivery.
                    if !admitted
                        && let Some(provenant_records::run::DeliveryMode::Integrate(target)) =
                            &node.delivery
                        && !target.starts_with("origin/")
                        && let Some(path) = node.worktree.as_deref()
                        && writer_branch(node, path)? == *target
                    {
                        return Err(FacadeError::DispatchRefused {
                            fact: None,
                            disk: None,
                            reason: format!(
                                "worktree {path} is on the integration branch {target}; create it with `git worktree add -b batch/work PATH BASE`"
                            ),
                        });
                    }
                }
                Some(document)
            }
            _ => None,
        };
        let id = match &input {
            GraphInput::Status { id, .. } | GraphInput::Cancel { id, .. } => id.as_str(),
            _ => document
                .as_ref()
                .ok_or(FacadeError::InvalidInput { detail: None })?
                .graph_id
                .as_str(),
        };
        // An unreadable record has no trustworthy node or attempt state.
        let unreadable_snapshot = |message: String| {
            use provenant_fabric_runtime::graph::{GraphCheckpoint, GraphRun, GraphStatus};
            GraphRun {
                status: GraphStatus::Paused,
                outcome: None,
                checkpoint: GraphCheckpoint {
                    bases: None,
                    extra: Default::default(),
                    expansions: Vec::new(),
                    remaining: None,
                    instance_id: None,
                    delivery_required: false,
                    delivered: false,
                    pull_request: None,
                    integration_pin: None,
                    cancelled: false,
                    cancel_reason: None,
                    pause: None,
                    unknown_usage: Vec::new(),
                    unknown_usage_omitted: 0,
                    usage_events: Vec::new(),
                    usage_events_omitted: 0,
                    settlements: 0,
                    extensions: Vec::new(),
                    asks: Vec::new(),
                    ask_actor: None,
                    wake_at: None,
                    pause_asks: 0,
                    limit_notes: Vec::new(),
                    schema_version: 2,
                    graph_id: id.to_owned(),
                    plan_digest: String::new(),
                    nodes: Vec::new(),
                },
                advance_error: Some(message),
            }
        };
        if matches!(input, GraphInput::Status { .. }) {
            let mut checkpoint = match FileJournal::read_status_checkpoint(
                &self.context.roots.state,
                self.context.identity.project,
                id,
            ) {
                Ok(checkpoint) => checkpoint.ok_or(FacadeError::NotFound)?,
                Err(error) if error.is_unreadable() => {
                    return Ok(unreadable_snapshot(
                        provenant_fabric_runtime::graph::unreadable_graph_explanation(id, &error),
                    ));
                }
                Err(error) => return Err(graph_journal_error(error)),
            };
            if let (Ok(boot), Ok(now)) = (
                self.clock.boot_identity(),
                self.clock.host_monotonic_micros(),
            ) {
                graph_status_elapsed(&mut checkpoint, &boot, now);
            }
            let mut run = graph_snapshot(checkpoint);
            let sidecar_error = FileJournal::read_advance_error(
                &self.context.roots.state,
                self.context.identity.project,
                id,
            )
            .map_err(|_| FacadeError::io("access graph journal"))?;
            run.advance_error = sidecar_error.or(run.advance_error);
            return Ok(run);
        }
        if matches!(input, GraphInput::Cancel { .. })
            && FileJournal::retire_unreadable(
                &self.context.roots.state,
                self.context.identity.project,
                id,
                budget,
                cancel,
                &self.clock,
                |checkpoint| {
                    let Some(checkpoint) = checkpoint else {
                        return Ok(());
                    };
                    let mut first_error = None;
                    let mut owner = BinaryRunOwner {
                        factory: self,
                        allow_remote: true,
                        actor: None,
                        chair: None,
                    };
                    for attempt in checkpoint.nodes.iter().flat_map(|node| &node.attempts) {
                        if (attempt.state
                            != provenant_fabric_runtime::graph::AttemptState::Terminal
                            || self.remote_graph_attempt(attempt))
                            && let Err(error) =
                                self.cancel_graph_attempt(&mut owner, attempt, budget, cancel)
                        {
                            first_error.get_or_insert(error);
                        }
                    }
                    if let Some(actor) = &checkpoint.ask_actor {
                        for ask in &checkpoint.asks {
                            let scope = provenant_fabric_runtime::graph::AskScope { actor, ask };
                            if let Err(error) =
                                graph_ask_with(self, cancel, actor, &ask.ask_id, |service| {
                                    graph_ask_close(service, &scope, cancel)
                                })
                            {
                                first_error.get_or_insert(error);
                            }
                        }
                    } else if !checkpoint.asks.is_empty() {
                        first_error.get_or_insert(FacadeError::StateCorrupt);
                    }
                    if let Some(error) = first_error {
                        return Err(Box::new(error));
                    }
                    Ok(())
                },
            )
            .map_err(|error| FacadeError::DispatchRefused {
                reason: format!("cancel graph {id}: {error}"),
                fact: None,
                disk: None,
            })?
        {
            return Ok(unreadable_snapshot(
                "Unreadable graph record retired. Identifiable attempts and inbox requests were cancelled; unidentifiable work and writer fences require inspection.".into(),
            ));
        }
        let cancel_request = match &input {
            GraphInput::Cancel { reason, .. } => Some(
                FileJournal::request_cancellation(
                    &self.context.roots.state,
                    self.context.identity.project,
                    provenant_fabric_runtime::graph::CancellationTarget {
                        graph_id: id,
                        node: None,
                        reason: reason.as_deref(),
                    },
                    budget,
                    cancel,
                    &self.clock,
                )
                .map_err(graph_journal_error)?,
            ),
            _ => None,
        };
        let open = |budget| {
            FileJournal::open(
                &self.context.roots.state,
                self.context.identity.project,
                id,
                budget,
                cancel,
                &self.clock,
            )
        };
        let launch_digest = document
            .as_ref()
            .and_then(|document| document.replay_digest());
        let mut journal = if let Some(request_id) = cancel_request.as_ref() {
            match FileJournal::try_open(
                &self.context.roots.state,
                self.context.identity.project,
                id,
            ) {
                Ok(journal) => journal,
                Err(provenant_fabric_runtime::graph::JournalError::Filesystem(
                    provenant_os::fs::FsError::LockTimeout { .. },
                )) => {
                    let checkpoint = FileJournal::read_checkpoint(
                        &self.context.roots.state,
                        self.context.identity.project,
                        id,
                    )
                    .map_err(graph_journal_error)?
                    .ok_or(FacadeError::NotFound)?;
                    return Ok(cancellation_receipt(
                        graph_snapshot(checkpoint),
                        request_id.clone(),
                    ));
                }
                Err(error) => return Err(graph_journal_error(error)),
            }
        } else {
            match launch_digest {
                // A fresh launch does not queue behind another launch that bound
                // this name to other input: it loses the name at once and takes
                // the next one. While the holder has not bound yet, probe in short
                // slices so the answer arrives as soon as it binds. The same input
                // waits, as a replay.
                Some(digest) => loop {
                    let slice = WaitBudget::up_to(
                        self.clock.mono_now(),
                        provenant_core::DurationMicros::from_micros(200_000),
                    )
                    .cap_at(budget.deadline().as_mono_time());
                    match open(slice) {
                        Err(provenant_fabric_runtime::graph::JournalError::Filesystem(
                            provenant_os::fs::FsError::LockTimeout { .. },
                        )) if budget.remaining(self.clock.mono_now()).as_micros() > 0 => {
                            let bound = FileJournal::execution(
                                &self.context.roots.state,
                                self.context.identity.project,
                                id,
                            )
                            .ok()
                            .flatten();
                            match bound {
                                Some(bound) if bound.document.replay_digest() != Some(digest) => {
                                    return Err(FacadeError::Conflict);
                                }
                                Some(_) => break open(budget),
                                None => continue,
                            }
                        }
                        opened => break opened,
                    }
                },
                None => open(budget),
            }
            .map_err(|_| FacadeError::io("access graph journal"))?
        };
        if let Some(document) = &document {
            // GC and admission hold this graph's lock. If collection removed
            // the source after it was read, restore the exact bytes before
            // publishing a new pending checkpoint or launching a node.
            if let Some((directory, name, bytes)) = &scratch_source {
                restore_scratch_source(directory, name, bytes)?;
            }
            let checkpoint = journal.load(id).map_err(graph_journal_error)?;
            let existing = checkpoint.is_some();
            let bound_execution = if existing {
                Some(
                    FileJournal::execution(
                        &self.context.roots.state,
                        self.context.identity.project,
                        id,
                    )
                    .map_err(graph_journal_error)?
                    .ok_or(FacadeError::StateCorrupt)?,
                )
            } else {
                None
            };
            // A graph name holds one launch input. A new launch that finds the
            // name bound to another digest lost a race for it; never resume or
            // patch the other launch's graph.
            if let (Some(bound), Some(digest)) = (&bound_execution, document.replay_digest()) {
                if bound.document.replay_digest() != Some(digest) {
                    return Err(FacadeError::Conflict);
                }
                if let Some(replayed) = replayed {
                    // The journal lock, not the optimistic name lookup, decides
                    // who created this submission. A matching waiter only reads.
                    *replayed = true;
                    let mut run = graph_snapshot(checkpoint.ok_or(FacadeError::StateCorrupt)?);
                    redact_checkout_paths(&mut run.checkpoint);
                    run.advance_error = FileJournal::read_advance_error(
                        &self.context.roots.state,
                        self.context.identity.project,
                        id,
                    )
                    .map_err(graph_journal_error)?
                    .or(run.advance_error);
                    return Ok(run);
                }
            }
            let mut ledger_warning = None;
            let integration_base = if existing {
                checkpoint
                    .as_ref()
                    .and_then(|checkpoint| checkpoint.bases)
                    .and_then(|bases| bases.integration)
            } else if let Some(branch) = &document.integration_branch {
                let reference = match branch.strip_prefix("origin/") {
                    Some(branch) => format!("refs/remotes/origin/{branch}"),
                    None => format!("refs/heads/{branch}"),
                };
                let git = provenant_os::git::git_executable()
                    .ok_or(FacadeError::InvalidInput { detail: None })?;
                // A missing local integration branch starts where the batch is
                // measured from, rather than refusing the graph.
                // The shared ref ledger records whether this graph made the
                // branch or found it, so cleanup never claims a human's ref.
                if !branch.starts_with("origin/") {
                    let start = document
                        .review_base
                        .as_deref()
                        .filter(|base| !base.is_empty());
                    match provenant_confinement::worktrees::RefLedger::open(
                        self.context.roots.state.as_path(),
                        self.context.identity.project,
                    ) {
                        Ok(ledger) => {
                            (_, ledger_warning) = provenant_run::ensure_recorded_local_branch(
                                &git,
                                &self.cwd,
                                branch,
                                start,
                                &ledger,
                                &provenant_run::Request {
                                    clock: &self.clock,
                                    cancel,
                                },
                            )
                            .map_err(run_error)?;
                        }
                        Err(error) => {
                            provenant_run::ensure_local_branch(&git, &self.cwd, branch, start)
                                .map_err(run_error)?;
                            ledger_warning = Some(format!(
                                "could not record ownership of {branch}: {error}; it is retained as unobserved"
                            ));
                        }
                    }
                }
                Some(self.resolve_base(&self.cwd, &reference, &facts, cancel)?)
            } else {
                None
            };
            let review_base = if existing {
                checkpoint
                    .as_ref()
                    .and_then(|checkpoint| checkpoint.bases)
                    .and_then(|bases| bases.review)
            } else if let Some(reference) = document
                .review_base
                .as_deref()
                .filter(|base| !base.is_empty())
            {
                Some(self.resolve_base(&self.cwd, reference, &facts, cancel)?)
            } else {
                None
            };
            journal
                .bind(provenant_fabric_runtime::graph::GraphExecution {
                    extra: Default::default(),
                    bases: GraphBaseBinding::new(review_base, integration_base),
                    directory: self.cwd.clone(),
                    document: document.clone(),
                })
                .map_err(|error| match error {
                    provenant_fabric_runtime::graph::JournalError::TooLarge => {
                        graph_journal_error(error)
                    }
                    _ => FacadeError::Conflict,
                })?;
            // Manual replay must use the same host directory as the original
            // execution, just as background advancement does.
            let rebound = if let Some(execution) = bound_execution.as_ref()
                && execution.directory != self.cwd
            {
                Some(self.for_graph_directory(&execution.directory)?)
            } else {
                None
            };
            let mut owner = BoundGraphOwner {
                writer_launch_budget: None,
                admission_timeout,
                launch_warnings: ledger_warning.into_iter().collect(),
                repo_facts: facts.clone(),
                factory: rebound.as_ref().unwrap_or(self),
                integration_base: GraphBaseBinding::new(review_base, integration_base),
                integration_branch: document.integration_branch.clone(),
                network: graph_network(document)?,
                capability_overrides: graph_capability_overrides(document)?,
                exclude: graph_exclude(document)?,
                budget,
                cancel,
                prices: None,
                tick: false,
            };
            let graph_config = &owner.factory.context.config.graph;
            let mut journal = AnnouncingJournal {
                journal: &mut journal,
                document,
                announce,
            };
            let run = if resume {
                let extensions = match &input {
                    GraphInput::Resume { extend, .. } => extend.as_slice(),
                    _ => &[],
                };
                provenant_fabric_runtime::graph::resume_with_budget(
                    document,
                    &mut owner,
                    &mut journal,
                    &graph_config.limits,
                    &graph_config.budget,
                    extensions,
                )
            } else {
                provenant_fabric_runtime::graph::advance_with_budget(
                    document,
                    &mut owner,
                    &mut journal,
                    &graph_config.limits,
                    &graph_config.budget,
                )
            };
            let run = run.map_err(|error| match error {
                provenant_fabric_runtime::graph::GraphError::Run(error) => error,
                provenant_fabric_runtime::graph::GraphError::Invalid(_) => {
                    FacadeError::InvalidInput { detail: None }
                }
                provenant_fabric_runtime::graph::GraphError::Admission(reason) => match reason {
                    provenant_fabric_runtime::graph::AdmissionError::Schema(_) => {
                        FacadeError::invalid(provenant_fabric_runtime::coordination::facade::InvalidInputDetail::Schema)
                    }
                    _ => FacadeError::InvalidInput { detail: None },
                },
                provenant_fabric_runtime::graph::GraphError::Extension(reason)
                | provenant_fabric_runtime::graph::GraphError::Refused(reason) => {
                    FacadeError::DispatchRefused {
                        reason,
                        disk: None,
                        fact: None,
                    }
                }
                provenant_fabric_runtime::graph::GraphError::PlanChanged => FacadeError::Conflict,
                provenant_fabric_runtime::graph::GraphError::Journal(error) => {
                    graph_journal_error(error)
                }
                _ => FacadeError::StateCorrupt,
            })?;
            let mut run = run;
            redact_checkout_paths(&mut run.checkpoint);
            // A manual advance that succeeded supersedes an earlier failure.
            FileJournal::set_advance_error(
                &self.context.roots.state,
                self.context.identity.project,
                id,
                None,
            )
            .map_err(|_| FacadeError::io("access graph journal"))?;
            return Ok(run);
        }
        let mut checkpoint = journal
            .load(id)
            .map_err(graph_journal_error)?
            .ok_or(FacadeError::NotFound)?;
        if let GraphInput::Cancel { reason, .. } = &input {
            let execution = FileJournal::execution(
                &self.context.roots.state,
                self.context.identity.project,
                id,
            )
            .map_err(graph_journal_error)?
            .ok_or(FacadeError::NotFound)?;
            journal
                .apply_cancellation(&execution.document, &mut checkpoint)
                .map_err(graph_journal_error)?;
            // Persist the admission fence before asking the run owner to stop live attempts.
            if !checkpoint.cancelled || reason.is_some() {
                checkpoint.cancel_reason = reason.clone();
            }
            checkpoint.cancelled = true;
            let detached = provenant_fabric_runtime::graph::relay::fence_detached_cancellation(
                &execution.document,
                &mut checkpoint,
            )
            .map_err(|_| FacadeError::InvalidInput { detail: None })?;
            provenant_fabric_runtime::graph::mark_cancelled_nodes_without_live_attempts(
                &mut checkpoint,
            );
            journal.save(&checkpoint).map_err(graph_journal_error)?;
            let mut owner = BinaryRunOwner {
                factory: self,
                allow_remote: true,
                actor: None,
                chair: None,
            };
            // One unresolvable target must not strand the others; report the first failure last.
            let mut first_error = None;
            for node in &checkpoint.nodes {
                if detached.contains(&node.id) {
                    continue;
                }
                for attempt in &node.attempts {
                    if attempt.state == provenant_fabric_runtime::graph::AttemptState::Terminal
                        && !self.remote_graph_attempt(attempt)
                    {
                        continue;
                    }
                    if let Err(error) =
                        self.cancel_graph_attempt(&mut owner, attempt, budget, cancel)
                    {
                        first_error.get_or_insert(error);
                    }
                }
            }
            let mut graph_owner = BoundGraphOwner {
                writer_launch_budget: None,
                admission_timeout: None,
                launch_warnings: Vec::new(),
                repo_facts: facts.clone(),
                factory: self,
                integration_base: None,
                integration_branch: None,
                network: None,
                capability_overrides: Default::default(),
                exclude: Vec::new(),
                budget,
                cancel,
                prices: None,
                tick: false,
            };
            if provenant_fabric_runtime::graph::settle_cancelled_nodes(
                &mut checkpoint,
                &mut graph_owner,
                &mut journal,
            )
            .is_err()
            {
                first_error.get_or_insert_with(|| FacadeError::io("settle cancelled graph nodes"));
            }
            if provenant_fabric_runtime::graph::close_resolved_asks(
                &mut checkpoint,
                &mut graph_owner,
                &mut journal,
            )
            .is_err()
            {
                first_error.get_or_insert_with(|| FacadeError::io("close cancelled graph asks"));
            }
            if let Some(error) = first_error {
                return Err(error);
            }
        }
        let run = graph_snapshot(checkpoint);
        Ok(match cancel_request {
            Some(request_id) => cancellation_receipt(run, request_id),
            None => run,
        })
    }

    fn graph_checkout(&self) -> Result<PathBuf, provenant_os::fs::FsError> {
        provenant_os::git::repository_path(&self.cwd, provenant_os::git::RepositoryPath::Toplevel)
            .map_err(|source| provenant_os::fs::FsError::Io {
                operation: "locate graph checkout",
                path: self.cwd.clone(),
                source,
            })
    }

    fn graph_documents_directory(
        &self,
    ) -> Result<provenant_os::fs::Directory, provenant_os::fs::FsError> {
        [".provenant", "scratch", "graphs"].iter().try_fold(
            provenant_os::fs::Directory::open_anchor(&self.graph_checkout()?)?,
            |directory, name| directory.open_verified_directory(std::ffi::OsStr::new(name)),
        )
    }

    /// Create and exclude project scratch. The exclude lock guards a short
    /// read-modify-write, so it has its own bounded wait: a caller's `--wait`
    /// bounds the result, and may already be spent when parallel launches
    /// queue here (#3232).
    pub(crate) fn prepare_scratch(
        &self,
        cancel: &CancelToken,
    ) -> Result<PathBuf, provenant_confinement::worktrees::WorktreeError> {
        let budget = WaitBudget::cli(self.clock.mono_now(), 30)
            .map_err(|_| provenant_confinement::worktrees::WorktreeError::InvalidRequest)?;
        // Scratch belongs to the selected checkout, even when the command was
        // invoked from one of its subdirectories. Keep self.cwd unchanged for
        // relative launch arguments and the persisted execution directory.
        let checkout = self.graph_checkout()?;
        provenant_confinement::worktrees::with_context(
            &self.context.roots,
            &self.home,
            &checkout,
            &self.clock,
            cancel,
            |context| {
                let repo = context.utility.open_repo(
                    context.executor,
                    &checkout,
                    context.utility_budget,
                    cancel,
                )?;
                provenant_confinement::scratch::ensure_with_budget(
                    &repo,
                    &self.clock,
                    cancel,
                    budget,
                )
            },
        )
    }
}

pub(super) fn graph_selector_forms() -> String {
    "unknown or ambiguous graph selector; accepted forms are a graph ID, a project-local document path, or the document SHA-256 hash".to_owned()
}

impl Factory {
    /// A finished peer attempt may still own attached descendants.
    pub(crate) fn remote_graph_attempt(
        &self,
        attempt: &provenant_fabric_runtime::graph::GraphAttempt,
    ) -> bool {
        attempt.extra.contains_key("placement_host")
            && attempt
                .attempt_ref
                .as_deref()
                .and_then(|reference| reference.split_once('/'))
                .is_some_and(|(host, _)| host != self.context.identity.host.to_string())
    }

    /// Address a receiver-owned graph even when its launch reply was lost.
    pub(super) fn cancel_peer_submission(
        &self,
        attempt: &provenant_fabric_runtime::graph::GraphAttempt,
        stop: bool,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<Option<bool>, FacadeError> {
        let host = if let Some(reference) = attempt.attempt_ref.as_deref() {
            Some(
                reference
                    .split_once('/')
                    .ok_or(FacadeError::StateCorrupt)?
                    .0
                    .parse()
                    .map_err(|_| FacadeError::StateCorrupt)?,
            )
        } else if let Some(host) = attempt
            .extra
            .get("placement_host_id")
            .and_then(serde_json::Value::as_str)
        {
            Some(host.parse().map_err(|_| FacadeError::StateCorrupt)?)
        } else if let Some(name) = attempt
            .extra
            .get("placement_host")
            .and_then(serde_json::Value::as_str)
        {
            let local = self.context.identity.host;
            Some(
                if matches!(name, "local" | "localhost")
                    || name
                        .parse::<provenant_core::HostId>()
                        .is_ok_and(|host| host == local)
                {
                    local
                } else {
                    match self.context.peer_host(name) {
                        Some(host) => host,
                        // An older checkpoint may retain only a removed alias.
                        // Missing routing is uncertainty, never proof of drainage.
                        None if !stop => return Ok(Some(false)),
                        None => return Err(FacadeError::UnknownHost),
                    }
                },
            )
        } else {
            None
        };
        let Some(host) = host.filter(|host| *host != self.context.identity.host) else {
            return Ok(None);
        };
        let submission = graph_operation("graph-attempt-operation", &attempt.key)?;
        let operation_id = peer::peer_cancel_operation(&submission)?;
        let owner = BinaryRunOwner {
            factory: self,
            allow_remote: true,
            actor: Some(
                attempt
                    .launch_actor
                    .parse()
                    .map_err(|_| FacadeError::StateCorrupt)?,
            ),
            chair: None,
        };
        match owner.peer(
            host,
            PeerAction::CancelSubmission {
                extra: [(
                    "graph_launch_actor".into(),
                    serde_json::json!(attempt.launch_actor),
                )]
                .into(),
                submission,
                operation_id,
                stop,
            },
            budget,
            cancel,
        ) {
            Ok(PeerResult::SubmissionCancelled { settled, .. }) => Ok(Some(settled)),
            // Pre-cutover peers admitted direct run attempts without a receiver
            // graph. Only a retained target and definite absence permit using
            // the existing by-reference cancellation and observation contract.
            Err(FacadeError::NotFound) if attempt.attempt_ref.is_some() => Ok(None),
            Err(error) => Err(error),
            Ok(_) => Err(FacadeError::StateCorrupt),
        }
    }

    /// Cancel one live graph attempt. A missing checkpoint reference (a crash
    /// between acceptance and the save) is recovered from the stable operation ID.
    pub(crate) fn cancel_graph_attempt(
        &self,
        owner: &mut BinaryRunOwner<'_>,
        attempt: &provenant_fabric_runtime::graph::GraphAttempt,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<(), FacadeError> {
        if self
            .cancel_peer_submission(attempt, true, budget, cancel)?
            .is_some()
        {
            return Ok(());
        }
        owner.actor = Some(
            attempt
                .launch_actor
                .parse()
                .map_err(|_| FacadeError::StateCorrupt)?,
        );
        let targets = match attempt.attempt_ref.as_deref() {
            Some(reference) => {
                let (host, attempt_id) =
                    reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
                vec![AttemptTarget {
                    extra: Default::default(),
                    host: Some(host.parse().map_err(|_| FacadeError::StateCorrupt)?),
                    attempt: attempt_id.parse().map_err(|_| FacadeError::StateCorrupt)?,
                }]
            }
            None => {
                let accepted = graph_operation("graph-attempt-operation", &attempt.key)?;
                let store = owner.store(cancel)?;
                let call = owner.call(cancel);
                // An operation this actor never accepted launched nothing to stop.
                match store.operation_attempt(&accepted, &call) {
                    Ok(admitted) => vec![AttemptTarget {
                        extra: Default::default(),
                        host: Some(self.context.identity.host),
                        attempt: admitted,
                    }],
                    Err(provenant_run::RunError::NotFound) => vec![],
                    Err(error) => return Err(run_error(error)),
                }
            }
        };
        let mut first_error = None;
        for mut target in targets {
            if target
                .host
                .is_none_or(|host| host == self.context.identity.host)
            {
                let store = owner.store(cancel)?;
                let call = owner.call(cancel);
                let mut record = store.record(target.attempt, &call).map_err(run_error)?;
                while let Some(next) = record.superseded_by {
                    target.attempt = next;
                    record = store.record(next, &call).map_err(run_error)?;
                }
            }
            let operation_id =
                graph_operation(&format!("graph-cancel-{}", target.attempt), &attempt.key)?;
            if let Err(error) = owner.run(
                RunInput::Cancel {
                    extra: Default::default(),
                    target,
                    operation_id,
                },
                budget,
                cancel,
            ) {
                first_error.get_or_insert(error);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}
pub(super) fn graph_operation(
    domain: &str,
    key: &provenant_fabric_runtime::graph::AttemptKey,
) -> Result<provenant_core::OperationId, FacadeError> {
    let digest = provenant_records::CanonicalV1::record_digest(domain, 1, key)
        .map_err(|_| FacadeError::InvalidInput { detail: None })?;
    provenant_core::OperationId::parse(&format!("graph-{}", provenant_core::hex(digest.as_bytes())))
        .map_err(|_| FacadeError::InvalidInput { detail: None })
}

#[cfg(test)]
#[allow(
    dead_code,
    unused_imports,
    reason = "the custom worktree driver includes this module without libtest"
)]
mod tests {
    use super::*;

    #[test]
    fn nested_graph_selectors_refuse_symlinked_scratch_directories()
    -> Result<(), Box<dyn std::error::Error>> {
        for component in [
            ".provenant",
            ".provenant/scratch",
            ".provenant/scratch/graphs",
        ] {
            let rig = crate::worktree_tests::repository()?;
            let nested = rig.project.join("nested/directory");
            let graphs = rig.project.join(".provenant/scratch/graphs");
            std::fs::create_dir_all(&nested)?;
            std::fs::create_dir_all(&graphs)?;
            let document = serde_json::to_vec(&serde_json::json!({
                "schema_version": 1, "graph_id": "nested-selector", "nodes": []
            }))?;
            let source = graphs.join("plan.json");
            std::fs::write(&source, &document)?;
            let digest = provenant_records::CanonicalV1::file_digest(&document).to_string();
            let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
            let factory = Factory {
                context: Context::open(roots, &rig.home, &nested)?,
                project_attribution: false,
                clock: SystemClock,
                backoff: ThreadPause,
                executor: SystemExecutor::new(1)?,
                home: rig.home.clone(),
                cwd: nested,
                assets: rig.project.clone(),
                host_launch: None,
                workers: &provenant_confinement::worktrees::SystemWorkerObserver,
            };
            for selector in ["plan.json", source.to_str().ok_or("graph path")?, &digest] {
                let (id, graph) = factory.resolve_graph(selector)?;
                assert_eq!(id, "nested-selector");
                assert_eq!(graph.ok_or("resolved document")?.graph_id, id);
            }
            // The exception grants the scratch graph directory, not sibling
            // documents elsewhere in the enclosing checkout.
            let sibling = rig.project.join("sibling.json");
            std::fs::write(&sibling, &document)?;
            assert!(matches!(
                factory.resolve_graph(sibling.to_str().ok_or("sibling path")?),
                Err(FacadeError::ConfinementRefused { .. })
            ));

            let directory = rig.project.join(component);
            let external = rig.root.join("external-graphs");
            std::fs::rename(&directory, &external)?;
            std::os::unix::fs::symlink(&external, &directory)?;
            for selector in ["plan.json", source.to_str().ok_or("graph path")?] {
                assert!(
                    matches!(
                        factory.resolve_graph(selector),
                        Err(FacadeError::ConfinementRefused { .. })
                    ),
                    "accepted {selector} through linked {component}"
                );
            }
            assert!(matches!(
                factory.resolve_graph(&digest),
                Err(FacadeError::DispatchRefused { .. })
            ));
        }
        Ok(())
    }
}
