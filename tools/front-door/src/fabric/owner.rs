use super::*;
use std::io::Write as _;

#[derive(Clone, Copy)]
pub(crate) struct BoundGraphBases {
    pub(crate) integration: Option<provenant_core::GitOid>,
    pub(crate) review: Option<provenant_core::GitOid>,
}

pub(crate) struct BoundGraphOwner<'a> {
    /// Active node's sealed launch allowance, independent of settlement observation.
    pub(crate) writer_launch_budget: Option<WaitBudget>,
    /// Optional deadline for initial admission, supplied by a waited submission.
    pub(crate) admission_timeout: Option<WaitBudget>,
    pub(crate) factory: &'a Factory,
    pub(crate) repo_facts: provenant_confinement::worktrees::RepoFacts,
    pub(crate) launch_warnings: Vec<String>,
    // Both immutable graph bases travel together through every host owner.
    pub(crate) integration_base: Option<BoundGraphBases>,
    pub(crate) integration_branch: Option<String>,
    /// The graph's explicit network reach for every node lane, such as
    /// `run --network off` bound before automatic preset selection.
    pub(crate) network: Option<provenant_cli::Switch>,
    pub(crate) capability_overrides: (Vec<String>, Vec<String>),
    /// The graph's route-exclusion selectors, passed to each node as the
    /// `routing.graph_exclude` knob.
    pub(crate) exclude: Vec<String>,
    pub(crate) budget: WaitBudget,
    pub(crate) cancel: &'a CancelToken,
    pub(crate) prices: Option<Result<provenant_telemetry::PriceTable, String>>,
    pub(crate) tick: bool,
}

/// Frozen intent for a launch refused before it has a canonical run request.
/// Only the digest is retained; prompts and host paths stay out of the record.
struct GraphRefusalScope {
    operation: provenant_core::OperationId,
    actor: provenant_core::ActorId,
    digest: provenant_core::Digest,
    worktree: Option<String>,
    chair: Option<provenant_run::ChairBinding>,
}

impl GraphRefusalScope {
    fn new(
        owner: &BoundGraphOwner<'_>,
        key: &provenant_fabric_runtime::graph::AttemptKey,
        node: &provenant_fabric_runtime::graph::NodeInput<'_>,
        route: &provenant_fabric_runtime::graph::GraphRoute,
        access: provenant_fabric_runtime::graph::AccessMode,
        worktree: Option<&str>,
        replay: provenant_fabric_runtime::graph::Replay<'_>,
    ) -> Result<Self, FacadeError> {
        let actor = replay
            .launch_actor
            .parse()
            .map_err(|_| FacadeError::StateCorrupt)?;
        let context = &owner.factory.context;
        // Use the verified host inbox derivation, never node work labels. A
        // different launch actor cannot acquire the current chair's binding.
        let chair = (!provenant_cli::fabric::in_lane()
            && provenant_cli::workflow::session_actor(context, None).ok() == Some(actor))
        .then(|| provenant_run::ChairBinding {
            extra: Default::default(),
            host: context.identity.inbox.host,
            actor,
            session: provenant_workflow::session::chair_session_for(
                &context.roots.state,
                context.identity.project,
                actor,
            )
            .map(|session| session.as_str().to_owned()),
        });
        let intent = serde_json::json!({
            "key":key, "route":route, "access":access, "worktree":worktree,
            "context":node.context, "prompt":node.prompt, "title":node.title,
            "source_commit":node.source_commit, "review_base":node.review_base,
            "attempt":node.attempt, "read_roots":node.read_roots,
            "attachments":node.attachments, "capabilities":node.capabilities,
            "capability_sources":node.capability_sources,
            "skill_location":node.require_skill_location,
            "distinct_from_attempts":node.distinct_from_attempts,
            "require_distinct":node.require_distinct,
            "required_verdict":node.required_verdict, "deadline":node.deadline_seconds,
            "delivery":replay.delivery, "branch":replay.branch,
            "fast_forward_only":replay.fast_forward_only,
        });
        Ok(Self {
            operation: graph_operation("graph-attempt-operation", key)?,
            actor,
            digest: provenant_records::CanonicalV1::record_digest(
                "graph-dispatch-preflight",
                1,
                &intent,
            )
            .map_err(|_| FacadeError::StateCorrupt)?,
            worktree: worktree
                .and_then(|path| Path::new(path).file_name())
                .and_then(|name| name.to_str())
                .map(str::to_owned),
            chair,
        })
    }
}

/// A selected read-only checkout uses the ordinary launch root policy.
fn bind_child_directory(
    grants: &mut provenant_records::run::LaneAccess,
    directory: &str,
    state: &Path,
    home: &Path,
    network: Option<provenant_cli::Switch>,
) -> Result<(), FacadeError> {
    let directory = PathBuf::from(directory);
    if !directory.is_absolute() {
        return Err(FacadeError::invalid(InvalidInputDetail::Field(
            "attempt.directory".into(),
        )));
    }
    let checked = provenant_cli::run::lane_access(
        &provenant_cli::Access {
            mode: None,
            worktree: None,
            read_root: vec![directory],
            attach: Vec::new(),
            write_root: Vec::new(),
            network,
            capability: Vec::new(),
            no_capability: Vec::new(),
        },
        state,
        home,
    )
    .map_err(dispatch_refused)?;
    let source = checked
        .read_roots
        .into_iter()
        .next()
        .ok_or(FacadeError::StateCorrupt)?;
    let directory = provenant_os::fs::Directory::open_anchor(&source)
        .map_err(|_| FacadeError::ConfinementRefused { resource: None })?;
    let metadata = directory
        .metadata()
        .map_err(|_| FacadeError::ConfinementRefused { resource: None })?;
    grants.source_identity = Some(provenant_records::run::SourceIdentity {
        device: metadata.device,
        inode: metadata.inode,
    });
    grants.source_root = Some(source);
    grants.source_live = true;
    Ok(())
}

/// Explicit writer checkouts carry their own repository, while their attempt
/// remains attributed to the parent graph's project.
fn selected_writer_project(
    factory: &Factory,
    path: &str,
) -> Result<(PathBuf, provenant_core::ProjectId), FacadeError> {
    let given = Path::new(path);
    let selected = if given.is_absolute() {
        given.to_path_buf()
    } else {
        factory.cwd.join(given)
    };
    if !selected
        .try_exists()
        .map_err(|_| FacadeError::io("inspect writer worktree"))?
    {
        return Err(run_error(provenant_run::RunError::WorktreeMissing(
            selected,
        )));
    }
    let mut checked = provenant_records::run::LaneAccess::default();
    bind_child_directory(
        &mut checked,
        selected
            .to_str()
            .ok_or(FacadeError::InvalidInput { detail: None })?,
        factory.context.roots.state.as_path(),
        &factory.home,
        None,
    )?;
    let directory = checked.source_root.ok_or(FacadeError::StateCorrupt)?;
    let context = Context::open(factory.context.roots.clone(), &factory.home, &directory)
        .map_err(|_| FacadeError::invalid(InvalidInputDetail::Field("worktree".into())))?;
    Ok((directory, context.identity.project))
}

fn stored_graph_refusal(
    record: &provenant_run::OperationRecord,
) -> Result<FacadeError, FacadeError> {
    if let Some(error) = record.extra.get("graph_refusal_error") {
        return serde_json::from_value(error.clone()).map_err(|_| FacadeError::StateCorrupt);
    }
    let diagnostic = record.refusal.clone().ok_or(FacadeError::StateCorrupt)?;
    Ok(run_error(provenant_run::RunError::RecordedRefusal(
        Box::new(diagnostic),
    )))
}

impl BoundGraphOwner<'_> {
    fn fast_forward_writer(
        &self,
        directory: &Path,
        path: &Path,
        head: provenant_core::GitOid,
    ) -> Result<(), provenant_confinement::worktrees::WorktreeError> {
        let budget = self.writer_launch_budget.unwrap_or(self.budget);
        if budget.remaining(self.factory.clock.mono_now()).as_micros() == 0 {
            return Err(provenant_confinement::worktrees::WorktreeError::RegistryLockTimeout);
        }
        provenant_confinement::worktrees::with_context_using_facts(
            &self.factory.context.roots,
            &self.factory.home,
            directory,
            &self.factory.clock,
            self.cancel,
            &self.repo_facts,
            |context| {
                if budget.remaining(self.factory.clock.mono_now()).as_micros() == 0 {
                    return Err(
                        provenant_confinement::worktrees::WorktreeError::RegistryLockTimeout,
                    );
                }
                let context = provenant_confinement::worktrees::WorktreeContext {
                    lock_budget: budget,
                    ..*context
                };
                provenant_confinement::worktrees::fast_forward(&context, path, head, self.cancel)
            },
        )
    }

    fn refusal_store(
        &self,
        scope: &GraphRefusalScope,
    ) -> Result<provenant_run::RunStore, FacadeError> {
        BinaryRunOwner {
            factory: self.factory,
            allow_remote: false,
            actor: Some(scope.actor),
            chair: None,
        }
        .store_with_facts(self.cancel, &self.repo_facts)
    }

    fn post_graph_refusal(
        &self,
        store: &provenant_run::RunStore,
        operation: &provenant_core::OperationId,
        call: &provenant_run::Request<'_>,
    ) {
        if crate::settlement::post_operation_refusal(operation, &self.factory.context, store, call)
            .is_err()
        {
            let _ = writeln!(
                std::io::stderr().lock(),
                "refusal notice could not be delivered; run `provenant reconcile`"
            );
        }
    }

    fn retain_graph_refusal(
        &self,
        scope: &GraphRefusalScope,
        error: &FacadeError,
        diagnostic: provenant_records::diagnostic::Diagnostic,
    ) -> Result<(), FacadeError> {
        let store = self.refusal_store(scope)?;
        let call = provenant_run::Request {
            clock: &self.factory.clock,
            cancel: self.cancel,
        };
        let config = provenant_config::read_with_domains(
            &self.factory.context.roots,
            &provenant_cli::domains::SECTIONS,
        )
        .map_err(|_| FacadeError::io("refusal notice configuration"))?;
        let notice = config
            .knobs
            .get("chair.refusal_notice")
            .map_or_else(|| serde_json::json!("inbox"), |knob| knob.value.clone());
        // Retain the safe diagnostic reason for dispatch refusals; persisting
        // the original error can include host paths. Relay codes need the
        // same grammar check as the facade's public code projection.
        let replay_error = match error {
            FacadeError::GraphRelayRefused { detail, .. } => FacadeError::GraphRelayRefused {
                refusal_code: error.code().to_owned(),
                detail: detail.clone(),
            },
            FacadeError::DispatchRefused { disk, fact, .. } => FacadeError::DispatchRefused {
                reason: diagnostic
                    .detail
                    .clone()
                    .unwrap_or_else(|| "dispatch was refused".into()),
                disk: disk.clone(),
                fact: *fact,
            },
            error => error.clone(),
        };
        let record = provenant_run::OperationRecord {
            extra: [
                (
                    "graph_refusal_error".into(),
                    serde_json::to_value(replay_error).map_err(|_| FacadeError::StateCorrupt)?,
                ),
                ("refusal_notice".into(), notice),
                (
                    "origin_intent_digest".into(),
                    serde_json::json!(scope.digest),
                ),
            ]
            .into(),
            schema_version: 1,
            project_id: self.factory.context.identity.project,
            actor_id: scope.actor,
            operation_id: scope.operation.clone(),
            kind: provenant_run::OperationKind::Dispatch,
            request_digest: scope.digest,
            run_id: provenant_core::RunId::from_bytes(
                provenant_os::fs::entropy_128().map_err(|_| FacadeError::io("refusal identity"))?,
            ),
            state: provenant_run::OperationState::Refused,
            blocked_reason: None,
            refusal: Some(diagnostic),
            worktree: scope.worktree.clone(),
            refused_at: Some(
                self.factory
                    .clock
                    .wall_now()
                    .map_err(|_| FacadeError::io("refusal time"))?,
            ),
            refusal_chair: scope.chair.clone(),
        };
        let saved = store.insert_refusal(record, &call).map_err(run_error)?;
        if saved.state != provenant_run::OperationState::Refused {
            return Err(run_error(provenant_run::RunError::OperationConflict));
        }
        self.post_graph_refusal(&store, &scope.operation, &call);
        Ok(())
    }
}

/// The inbox request for a stored graph ask. The checkpoint keeps the rendered
/// text, so a replay sends the identical request under the same operation.
pub(crate) fn graph_ask_input(
    scope: &provenant_fabric_runtime::graph::AskScope<'_>,
) -> Result<provenant_fabric_runtime::inbox::InboxInput, FacadeError> {
    use provenant_fabric_runtime::inbox::InboxInput;
    use provenant_records::inbox::{RequestDraft, RequestKind, RequestOption};
    let ask = scope.ask;
    let operation = provenant_records::CanonicalV1::record_digest(
        "graph-node-ask",
        1,
        &(scope.graph_id, scope.instance_id, &ask.ask_id, &ask.session),
    )
    .map_err(|_| FacadeError::StateCorrupt)?;
    let operation_id = provenant_core::OperationId::parse(&format!(
        "ask-{}",
        provenant_core::hex(operation.as_bytes())
    ))
    .map_err(|_| FacadeError::StateCorrupt)?;
    let request = RequestDraft {
        extra: Default::default(),
        supersedes: None,
        deadline: ask.wake_at,
        work: None,
        kind: RequestKind::Decision,
        title: ask.title.clone(),
        body: ask.body.clone(),
        blocking: true,
        options: ask
            .options
            .iter()
            .map(|option| RequestOption {
                extra: Default::default(),
                id: option.id.clone(),
                label: option.label.clone(),
                // The inbox needs a description; a decide option may leave it out.
                description: if option.description.is_empty() {
                    option.label.clone()
                } else {
                    option.description.clone()
                },
            })
            .collect(),
        recommended_option_id: ask.default.clone(),
        proceeding_with: None,
    };
    Ok(InboxInput::Ask {
        extra: Default::default(),
        session: ask.session.clone(),
        request: Box::new(request),
        operation_id: Some(operation_id),
    })
}

/// A notification uses its own immutable inbox operation and never asks for an
/// answer. Replaying it after a checkpoint failure returns the same receipt.
pub(crate) fn graph_notification_input(
    scope: &provenant_fabric_runtime::graph::AskScope<'_>,
) -> Result<provenant_fabric_runtime::inbox::InboxInput, FacadeError> {
    use provenant_fabric_runtime::inbox::InboxInput;
    let mut input = graph_ask_input(scope)?;
    if let InboxInput::Ask { request, .. } = &mut input {
        request.kind = provenant_records::inbox::RequestKind::Question;
        request.blocking = false;
        request.options.clear();
        request.recommended_option_id = None;
    }
    Ok(input)
}

pub(crate) fn graph_ask_poll(
    service: &provenant_fabric_runtime::inbox::InboxService<'_>,
    scope: &provenant_fabric_runtime::graph::AskScope<'_>,
    cancel: &CancelToken,
) -> Result<Option<provenant_fabric_runtime::graph::AskReply>, FacadeError> {
    use provenant_fabric_runtime::inbox::{InboxFilter, InboxInput, InboxResult};
    use provenant_records::inbox::RequestState;
    let inbox_error =
        |error: provenant_fabric_runtime::inbox::InboxError| FacadeError::DispatchRefused {
            fact: None,
            disk: None,
            reason: format!("graph ask {}: {error}", scope.ask.ask_id),
        };
    // The immutable ask and its operation receipt commit together. A crash before
    // saving graph state replays this exact request, including its original ID.
    let InboxResult::Saved { receipt } = service
        .execute(graph_ask_input(scope)?, cancel)
        .map_err(inbox_error)?
    else {
        return Err(FacadeError::StateCorrupt);
    };
    let InboxResult::Item { item, .. } = service
        .execute(
            InboxInput::List {
                extra: Default::default(),
                id: Some(receipt.request_id),
                session: None,
                state: InboxFilter::All,
                limit: 1,
                cursor: None,
            },
            cancel,
        )
        .map_err(inbox_error)?
    else {
        return Err(FacadeError::StateCorrupt);
    };
    if item.request.state != RequestState::Answered {
        return Ok(None);
    }
    let Some(answer) = item.answer else {
        return Ok(None);
    };
    // Acknowledgement precedes action. The immutable answer stays readable after
    // acknowledgement, so a failed graph checkpoint save loses no decision.
    service
        .execute(
            InboxInput::Acknowledge {
                extra: Default::default(),
                id: receipt.request_id,
                answer_id: answer.id,
                session: scope.ask.session.clone(),
                operation_id: Some(
                    provenant_core::OperationId::parse(&format!("ask-ack-{}", answer.id))
                        .map_err(|_| FacadeError::StateCorrupt)?,
                ),
            },
            cancel,
        )
        .map_err(inbox_error)?;
    // The graph runtime reads the option or reply text against the ask it stored.
    Ok(Some(provenant_fabric_runtime::graph::AskReply {
        option: answer.option_id,
        text: answer.text,
        actor: answer.actor.to_string(),
        at: Some(answer.answered_at),
    }))
}

pub(crate) fn graph_ask_edge(
    factory: &Factory,
    cancel: &CancelToken,
    scope: provenant_fabric_runtime::graph::AskScope<'_>,
    close: bool,
) -> Result<Option<provenant_fabric_runtime::graph::AskReply>, FacadeError> {
    use provenant_fabric_runtime::inbox::InboxService;
    let context = &factory.context;
    let mut identity = context.identity.clone();
    // Settlement can run under another client label. Keep the actor saved
    // before the first ask, just as an attempt retains its launch actor.
    identity.inbox.actor = scope.actor.parse().map_err(|_| FacadeError::StateCorrupt)?;
    let refused = |error: &dyn std::fmt::Display| FacadeError::DispatchRefused {
        fact: None,
        disk: None,
        reason: format!("graph ask {}: {error}", scope.ask.ask_id),
    };
    // Validate the ordinary inbox edge before opening local state. Graph
    // asks obey lane and configured home-host restrictions too.
    provenant_fabric_runtime::inbox::validate_request(
        &identity,
        &context.config,
        context.in_lane,
        &graph_ask_input(&scope)?,
    )
    .map_err(|error| refused(&error))?;
    let service = InboxService::open(&context.roots.state, &identity, &factory.clock, cancel)
        .map_err(|error| refused(&error))?;
    if close {
        graph_ask_close(&service, &scope, identity.inbox.actor, cancel)?;
        Ok(None)
    } else {
        graph_ask_poll(&service, &scope, cancel)
    }
}

pub(crate) fn graph_ask_close(
    service: &provenant_fabric_runtime::inbox::InboxService<'_>,
    scope: &provenant_fabric_runtime::graph::AskScope<'_>,
    actor: provenant_core::ActorId,
    cancel: &CancelToken,
) -> Result<(), FacadeError> {
    use provenant_fabric_runtime::inbox::InboxInput;
    let InboxInput::Ask {
        operation_id: Some(operation),
        ..
    } = graph_ask_input(scope)?
    else {
        return Err(FacadeError::StateCorrupt);
    };
    graph_inbox_close(
        service,
        &scope.ask.session,
        &operation,
        actor,
        &format!("close graph ask {}", scope.ask.ask_id),
        cancel,
    )
}

/// Supersede the open request raised under `operation` by `actor`, if any.
/// Closing never creates a request, and a closed one is left as it is.
pub(crate) fn graph_inbox_close(
    service: &provenant_fabric_runtime::inbox::InboxService<'_>,
    session: &str,
    operation: &provenant_core::OperationId,
    actor: provenant_core::ActorId,
    context: &str,
    cancel: &CancelToken,
) -> Result<(), FacadeError> {
    use provenant_fabric_runtime::inbox::{InboxFilter, InboxInput, InboxResult};
    use provenant_records::inbox::RequestState;
    let map = |e: provenant_fabric_runtime::inbox::InboxError| FacadeError::DispatchRefused {
        fact: None,
        disk: None,
        reason: format!("{context}: {e}"),
    };
    // Find the request through the normal bounded inbox list.
    let mut cursor = None;
    loop {
        let InboxResult::Listed {
            items, next_cursor, ..
        } = service
            .execute(
                InboxInput::List {
                    extra: Default::default(),
                    id: None,
                    session: Some(session.into()),
                    state: InboxFilter::All,
                    limit: 50,
                    cursor,
                },
                cancel,
            )
            .map_err(map)?
        else {
            return Err(FacadeError::StateCorrupt);
        };
        if let Some(item) = items
            .into_iter()
            .find(|item| &item.operation_id == operation && item.chair_actor == actor)
        {
            if matches!(
                item.state,
                RequestState::Withdrawn | RequestState::Superseded | RequestState::Expired
            ) {
                return Ok(());
            }
            service
                .execute(
                    InboxInput::Close {
                        extra: Default::default(),
                        id: item.id,
                        state: RequestState::Superseded,
                        reason: "Graph decision resolved or cancelled".into(),
                        expect_revision: item.revision,
                        operation_id: Some(
                            provenant_core::OperationId::parse(&format!("ask-close-{}", item.id))
                                .map_err(|_| FacadeError::StateCorrupt)?,
                        ),
                    },
                    cancel,
                )
                .map_err(map)?;
            return Ok(());
        }
        let Some(next) = next_cursor else {
            return Ok(());
        };
        cursor = Some(next);
    }
}

pub(crate) fn load_graph_prices(
    assets: &std::path::Path,
) -> Result<provenant_telemetry::PriceTable, String> {
    let path = assets.join("config/telemetry-prices.toml");
    let text =
        std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    let config = toml_edit::de::from_str::<provenant_telemetry::ConfiguredPrices>(&text)
        .map_err(|error| format!("{}: invalid price table: {error}", path.display()))?;
    provenant_telemetry::validate_prices(&config)
        .map_err(|error| format!("{}: invalid price table: {error}", path.display()))
}

pub(crate) fn graph_attempt_usage(
    record: provenant_records::run::AttemptRecord,
    prices: Option<&provenant_telemetry::PriceTable>,
    cost_unknown_reason: Option<String>,
) -> provenant_fabric_runtime::graph::AttemptUsage {
    let usage = &record.usage;
    let categories = [
        usage.uncached_input,
        usage.cache_read,
        usage.cache_write,
        usage.output_excluding_reasoning,
        usage.reasoning,
    ];
    // Reasoning is counted inside output, so providers never report it apart.
    let tokens_partial = categories[..4].iter().any(Option::is_none);
    let tokens = categories.iter().any(Option::is_some).then(|| {
        categories
            .into_iter()
            .flatten()
            .fold(0u64, u64::saturating_add)
    });
    let cost = prices
        .and_then(|prices| {
            provenant_run::fact(record.clone()).ok().and_then(|fact| {
                provenant_telemetry::stats([fact], prices, Default::default()).ok()
            })
        })
        .map(|report| report.totals.cost);
    let cost_partial = cost.as_ref().is_none_or(|cost| cost.complete.is_none());
    let cost_micro_usd =
        cost.and_then(|cost| cost.known_lower_bound.map(|money| money.microdollars));
    // Persist only reporting facts. Provider text, paths and traces never enter events.
    let mut extra = provenant_records::ExtraFields::default();
    let mut wall_time_seconds = record.provider_ms.map(|ms| ms.saturating_add(999) / 1000);
    if let Ok(fact) = provenant_run::fact(record.clone()) {
        if let Some(started) = fact.started_at {
            wall_time_seconds = fact
                .finished_at
                .as_micros()
                .checked_sub(started.as_micros())
                .and_then(|micros| u64::try_from(micros).ok())
                .map(|micros| micros.saturating_add(999_999) / 1_000_000);
        }
        let inputs = [
            fact.usage.uncached_input,
            fact.usage.cache_read,
            fact.usage.cache_write,
        ];
        let input_tokens = inputs
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .map(|counts| counts.into_iter().fold(0u64, u64::saturating_add));
        if let Some(route) = fact
            .observed_route
            .as_ref()
            .or(fact.applied_route.as_ref())
            .or(fact.resolved_route.as_ref())
        {
            extra.insert(
                "resolved_route".into(),
                serde_json::json!({
                    "provider": route.provider, "model": route.model, "effort": route.effort
                }),
            );
        }
        for (key, value) in [
            (
                "cache_read_tokens",
                serde_json::json!(fact.usage.cache_read),
            ),
            ("input_tokens", serde_json::json!(input_tokens)),
            ("queue_time_ms", serde_json::json!(fact.slot_wait_ms)),
            ("outcome", serde_json::json!(fact.terminal_state)),
            ("tier", serde_json::json!(fact.tier)),
            ("template", serde_json::json!(fact.template)),
        ] {
            if !value.is_null() {
                extra.insert(key.into(), value);
            }
        }
    }
    provenant_fabric_runtime::graph::AttemptUsage {
        extra,
        tokens,
        cost_micro_usd,
        wall_time_seconds,
        tokens_partial,
        cost_partial,
        cost_unknown_reason,
    }
}

pub(crate) fn decode_graph_document(
    bytes: &[u8],
) -> Result<provenant_fabric_runtime::graph::GraphDocument, FacadeError> {
    #[derive(serde::Deserialize)]
    struct Header {
        schema_version: Option<serde_json::Value>,
    }
    // Check the version before decoding the graph, so a future schema can
    // request an upgrade even when its node fields are unknown to this reader.
    let limits = provenant_records::Limits {
        max_bytes: provenant_fabric_runtime::config::GraphLimits::document_bytes_bound()
            .maximum_u64(),
        ..Default::default()
    };
    let header: Header = provenant_records::decode(bytes, limits)
        .map_err(|_| FacadeError::InvalidInput { detail: None })?;
    match header
        .schema_version
        .as_ref()
        .and_then(serde_json::Value::as_u64)
    {
        Some(1 | 2) => provenant_records::decode(bytes, limits)
            .map_err(|_| FacadeError::InvalidInput { detail: None }),
        Some(version) if version > 2 => Err(FacadeError::UpgradeRequired {
            reader_release: env!("CARGO_PKG_VERSION").into(),
            writer_release: "unknown".into(),
            record: "graph document schema".into(),
            version,
        }),
        _ => Err(FacadeError::invalid(
            provenant_fabric_runtime::coordination::facade::InvalidInputDetail::Schema,
        )),
    }
}

pub(crate) fn graph_journal_error(
    error: provenant_fabric_runtime::graph::JournalError,
) -> FacadeError {
    use provenant_fabric_runtime::graph::JournalError;
    match error {
        JournalError::Decode(provenant_records::DecodeError::UnsupportedVersion {
            found, ..
        }) if found > 1 => FacadeError::UpgradeRequired {
            reader_release: env!("CARGO_PKG_VERSION").into(),
            writer_release: "unknown".into(),
            record: "graph checkpoint schema".into(),
            version: found,
        },
        JournalError::TooLarge => FacadeError::DispatchRefused {
            fact: None,
            disk: None,
            reason: "graph checkpoint exceeds its byte limit".into(),
        },
        JournalError::Filesystem(_)
        | JournalError::Io(_)
        | JournalError::DurabilityUnconfirmed(_) => FacadeError::io("access graph journal"),
        e if e.is_unreadable() => FacadeError::DispatchRefused {
            fact: None,
            disk: None,
            reason: e.to_string(),
        },
        _ => FacadeError::StateCorrupt,
    }
}

pub(crate) fn graph_result_warning(error: &provenant_run::RunError) -> &'static str {
    match error {
        provenant_run::RunError::ArtifactMismatch(_) => {
            "artifact digest or length does not match its record"
        }
        provenant_run::RunError::NotFound => "result artifact is missing",
        provenant_run::RunError::Filesystem(source) if source.is_not_found() => {
            "result artifact is missing"
        }
        provenant_run::RunError::TooLarge { .. } => "result artifact exceeds the byte limit",
        _ => "result artifact could not be verified",
    }
}

impl BoundGraphOwner<'_> {
    fn uncertain_peer_placement(
        &self,
        attempt: &provenant_fabric_runtime::graph::GraphAttempt,
    ) -> bool {
        let Some(name) = attempt
            .extra
            .get("placement_host")
            .and_then(serde_json::Value::as_str)
        else {
            return false;
        };
        let local = self.factory.context.identity.host;
        !matches!(name, "local" | "localhost")
            && !name.parse::<provenant_core::HostId>().is_ok_and(|host| host == local)
            // A removed peer configuration cannot turn a retained placement
            // into proof that nothing was accepted on that peer.
            && self.factory.context.peer_host(name) != Some(local)
    }

    pub(crate) fn prior_models(&self, attempts: &[String]) -> Result<Vec<String>, FacadeError> {
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
            actor: None,
            chair: None,
        };
        let mut models = Vec::new();
        for reference in attempts {
            let (host, attempt) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
            let target = AttemptTarget {
                extra: Default::default(),
                host: Some(host.parse().map_err(|_| FacadeError::StateCorrupt)?),
                attempt: attempt.parse().map_err(|_| FacadeError::StateCorrupt)?,
            };
            if let Some(model) = owner
                .snapshot(target, self.budget, self.cancel)?
                .applied_route
                .and_then(|route| route.model)
            {
                models.push(model);
            }
        }
        Ok(models)
    }

    /// The graph exclusions plus, on an alias retry, the models that earlier
    /// attempts of this node already used. The runtime resolves those models
    /// from the attempt records, so they arrive as model ids, not references.
    pub(crate) fn route_exclusions(
        &self,
        route: &provenant_fabric_runtime::graph::GraphRoute,
        prior_models: &[String],
        accepted: bool,
    ) -> Vec<String> {
        let mut exclude = self.exclude.clone();
        if !accepted && route.alias.is_some() {
            for model in prior_models {
                if !exclude.contains(model) {
                    exclude.push(model.clone());
                }
            }
        }
        exclude
    }

    /// Model families that served the writer attempts a reviewer must differ from.
    pub(crate) fn writer_families(&self, attempts: &[String]) -> Result<Vec<String>, FacadeError> {
        if attempts.is_empty() {
            return Ok(Vec::new());
        }
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
            actor: None,
            chair: None,
        };
        let mut families = Vec::new();
        for reference in attempts {
            let (host, attempt) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
            let target = AttemptTarget {
                extra: Default::default(),
                host: Some(host.parse().map_err(|_| FacadeError::StateCorrupt)?),
                attempt: attempt.parse().map_err(|_| FacadeError::StateCorrupt)?,
            };
            let record = owner.snapshot(target, self.budget, self.cancel)?;
            let family = record
                .applied_route
                .and_then(|route| route.model)
                .and_then(|model| provenant_cli::run::model_family(&model));
            if let Some(family) = family
                && !families.contains(&family)
            {
                families.push(family);
            }
        }
        Ok(families)
    }
}

impl provenant_fabric_runtime::graph::CiStatus for BoundGraphOwner<'_> {
    fn host_stamp(&self) -> Option<provenant_fabric_runtime::graph::CiHostStamp> {
        Some(provenant_fabric_runtime::graph::CiHostStamp {
            extra: Default::default(),
            boot_identity: self.factory.clock.boot_identity().ok()?,
            host_monotonic_micros: self.factory.clock.host_monotonic_micros().ok()?,
        })
    }
    fn observe_ci(
        &mut self,
        target: &str,
        checks: &[String],
        failed_checks_max_bytes: usize,
        max_failed_checks: usize,
    ) -> provenant_fabric_runtime::graph::CiObservation {
        crate::pr::status::observe_ci(
            &self.factory.cwd,
            target,
            checks,
            failed_checks_max_bytes,
            max_failed_checks,
        )
    }
}

// Review roles come from the shared role library. The framing role
// `plan.review` prepares the subject and is not an independent review.
pub(crate) fn graph_review_node(
    document: &provenant_fabric_runtime::graph::GraphDocument,
    id: &str,
) -> bool {
    document
        .nodes
        .iter()
        .any(|node| node.id == id && provenant_fabric_runtime::graph::is_review_node(node))
}

pub(crate) fn writer_source_identity_matches(
    checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
    source: &Path,
    identity: &provenant_records::run::SourceIdentity,
) -> bool {
    checkpoint
        .nodes
        .iter()
        .filter_map(|node| node.writer_binding.as_ref())
        .filter(|binding| Path::new(&binding.worktree) == source)
        .all(|binding| {
            binding
                .extra
                .get("source_directory_identity")
                .and_then(|value| {
                    serde_json::from_value::<provenant_records::run::SourceIdentity>(value.clone())
                        .ok()
                })
                .as_ref()
                == Some(identity)
        })
}

pub(crate) fn unanimous_preparation_cause(
    skipped: &[provenant_records::run::SkippedRoute],
) -> Option<provenant_records::RouteExclusionReason> {
    let mut causes = skipped.iter().map(|route| route.cause.as_ref());
    let first = causes.next()??;
    causes
        .all(|cause| cause == Some(first))
        .then(|| first.clone())
}

fn repair_scope_grew(
    review: &provenant_routing::ReviewConfig,
    before: &[String],
    after: &[String],
) -> bool {
    let rank = |tier: Option<&str>| match tier {
        Some("terminal") => 3,
        Some("crucial") => 2,
        Some("substantial") => 1,
        _ => 0,
    };
    let old_floor = review.path_floors(before);
    let new_floor = review.path_floors(after);
    after.iter().any(|path| !before.contains(path))
        || rank(new_floor.tier.as_deref()) > rank(old_floor.tier.as_deref())
}

/// Re-touching a protected surface can grow semantic risk even when the path
/// set and its tier are unchanged. This deliberately loose signal only replans.
fn repair_touches_risk(review: &provenant_routing::ReviewConfig, touched: &[String]) -> bool {
    review
        .path_floors(touched)
        .classes
        .iter()
        .any(|class| !matches!(class.as_str(), "routine" | "unknown"))
}

impl BoundGraphOwner<'_> {
    fn repair_git(&self, directory: &Path, args: &[&str]) -> Result<String, String> {
        use provenant_os::exec::{ExecOptions, Executor, Termination};
        let git = provenant_os::git::git_executable().ok_or("Git executable unavailable")?;
        let mut argv = vec![git.into_os_string()];
        argv.extend(args.iter().map(std::ffi::OsString::from));
        let remaining = std::time::Duration::from_micros(
            self.budget
                .remaining(self.factory.clock.mono_now())
                .as_micros(),
        )
        .min(std::time::Duration::from_secs(30));
        let mut options = ExecOptions::new(&argv, directory, remaining);
        options.cancel = Some(self.cancel);
        let output = self
            .factory
            .executor
            .execute(options)
            .map_err(|_| "repair Git command could not run".to_owned())?;
        if output.termination != Termination::Exit(0) || output.timed_out || output.stdout.truncated
        {
            return Err("repair Git command did not complete within its bounds".into());
        }
        String::from_utf8(output.stdout.bytes).map_err(|_| "repair Git output is not UTF-8".into())
    }
}

impl BoundGraphOwner<'_> {
    /// The skipped routes of this graph attempt's own unlaunched run record.
    fn preparation_skipped_routes(
        &mut self,
        key: &provenant_fabric_runtime::graph::AttemptKey,
        reference: &str,
    ) -> Result<Option<Vec<provenant_records::run::SkippedRoute>>, FacadeError> {
        let (host, attempt) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
        if host != self.factory.context.identity.host.to_string() {
            return Ok(None);
        }
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: false,
            actor: None,
            chair: None,
        };
        let store = owner.store_with_facts(self.cancel, &self.repo_facts)?;
        let call = owner.call(self.cancel);
        let record = store
            .record(
                attempt.parse().map_err(|_| FacadeError::StateCorrupt)?,
                &call,
            )
            .map_err(run_error)?;
        if record.operation_id != graph_operation("graph-attempt-operation", key)?
            || record.context.as_ref().is_none_or(|context| {
                context.graph_id.as_deref() != Some(key.graph_id.as_str())
                    || context.node_id.as_deref() != Some(key.node_id.as_str())
            })
        {
            return Ok(None);
        }
        Ok(Some(record.skipped_routes))
    }
}

impl provenant_fabric_runtime::graph::AttemptOwner for BoundGraphOwner<'_> {
    fn detect_repair_growth(&mut self, before: &str, after: &str) -> Result<bool, String> {
        let before =
            provenant_core::GitOid::parse(before).map_err(|_| "repair source commit is invalid")?;
        let after =
            provenant_core::GitOid::parse(after).map_err(|_| "repaired commit is invalid")?;
        let base = self
            .integration_base
            .and_then(|bases| bases.review.or(bases.integration))
            .ok_or("cannot assess repair growth without a pinned review base")?;
        let paths = |subject: &provenant_core::GitOid| -> Result<Vec<String>, String> {
            let range = format!("{base}...{subject}");
            let names = self.repair_git(
                &self.factory.cwd,
                &["diff", "--no-renames", "--name-only", "-z", &range, "--"],
            )?;
            Ok(names
                .split('\0')
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect())
        };
        let old = paths(&before)?;
        let new = paths(&after)?;
        let routing: provenant_routing::RoutingConfig =
            provenant_config::read_domain(self.factory.context.roots.state.as_path(), "routing")
                .map_err(
                    |_| "repair review configuration is unavailable; correct routing settings",
                )?;
        let delta = self.repair_git(
            &self.factory.cwd,
            &[
                "diff",
                "--no-renames",
                "--name-only",
                "-z",
                &before.to_string(),
                &after.to_string(),
                "--",
            ],
        )?;
        let touched: Vec<String> = delta
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect();
        Ok(repair_scope_grew(&routing.review, &old, &new)
            || repair_touches_risk(&routing.review, &touched))
    }

    fn resolve_repair_panel(
        &mut self,
        document: &provenant_fabric_runtime::graph::GraphDocument,
        subject: &str,
    ) -> Result<provenant_fabric_runtime::graph::RepairPanelResolution, String> {
        let subject = provenant_core::GitOid::parse(subject)
            .map_err(|_| "repair review subject is invalid")?;
        let base = self
            .integration_base
            .and_then(|bases| bases.review.or(bases.integration))
            .ok_or("cannot replan review without a pinned base")?;
        let range = format!("{base}...{subject}");
        let names = self.repair_git(
            &self.factory.cwd,
            &["diff", "--no-renames", "--name-only", "-z", &range, "--"],
        )?;
        let paths: Vec<String> = names
            .split('\0')
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect();
        let routing: provenant_routing::RoutingConfig =
            provenant_config::read_domain(self.factory.context.roots.state.as_path(), "routing")
                .map_err(
                    |_| "repair review configuration is unavailable; correct routing settings",
                )?;
        let raw = serde_json::to_value(document)
            .map_err(|_| "repair review document could not be encoded")?;
        let mut request: provenant_routing::ReviewRequest = raw
            .get("review")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| "repair review request is invalid; correct graph review settings")?
            .unwrap_or_default();
        if request.kind.is_none() {
            request.kind = raw["context"]["task_kind"].as_str().map(str::to_owned);
        }
        // Replanning may raise coverage, but must retain the original hard
        // family floors and selected named-lens requirements.
        let declared_panel = routing
            .review
            .resolve_panel(&request, None, &paths, false)?;
        // A growth signal does not carry a new tier. Resolve at the highest
        // tier until the graph carries a measured replacement risk record.
        request.risk_tier = Some("terminal".into());
        request.shape = None;
        request.lenses.clear();
        let mut panel = routing
            .review
            .resolve_panel(&request, None, &paths, false)
            .map_err(
                |_| "repair review panel could not resolve; correct review routing settings",
            )?;
        for (family, count) in declared_panel.minimum_family_counts {
            let minimum = panel.minimum_family_counts.entry(family).or_default();
            *minimum = (*minimum).max(count);
        }
        for mut declared in declared_panel
            .slots
            .into_iter()
            .filter(|slot| slot.required_family.is_some())
        {
            if let Some(slot) = panel
                .slots
                .iter_mut()
                .find(|slot| slot.lens == declared.lens)
            {
                slot.required_family = declared.required_family;
            } else {
                // The raised shape can use a different lens order. Keep an
                // explicitly constrained lens rather than dropping its read.
                declared.distinct_families = false;
                panel
                    .omitted
                    .retain(|omitted| omitted.lens != declared.lens);
                panel.slots.push(declared);
            }
        }
        panel.resolve_families(|slot, family| {
            provenant_cli::route::resolve_review_family(
                &self.factory.context.roots,
                &slot.tier,
                family,
                &document
                    .extra
                    .get("exclude")
                    .and_then(serde_json::Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(serde_json::Value::as_str)
                            .map(str::to_owned)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default(),
            )
        })?;
        let single = panel.slots.len() == 1;
        let frame = !single && panel.frame;
        let verifiers = if single { 0 } else { panel.verifiers };
        let synthesis = !single && panel.synthesis == provenant_routing::ReviewSynthesis::Model;
        let advice = panel
            .slots
            .iter()
            .map(|slot| (slot.lens.clone(), panel.lens_advice(&slot.lens, true)))
            .collect();
        let slots = panel
            .slots
            .into_iter()
            .map(|slot| {
                let (key, mut route) = crate::graph_support::tier_route(
                    &slot.lens, &slot.tier, None, true,
                )
                .map_err(|_| "repair review route is invalid; correct reviewer tier settings")?;
                if let Some(family) = slot
                    .satisfied_family
                    .as_ref()
                    .or(slot.required_family.as_ref())
                {
                    if let Some(routes) = route.as_array_mut() {
                        for route in routes {
                            route["required_family"] = serde_json::json!(family);
                        }
                    } else {
                        route["required_family"] = serde_json::json!(family);
                    }
                }
                let mut fields = serde_json::Map::new();
                fields.insert(key.into(), route);
                Ok(provenant_fabric_runtime::graph::RepairPanelSlot {
                    lens: slot.lens,
                    route: fields.into(),
                    family_group: slot
                        .distinct_families
                        .then(|| crate::graph_support::format_family_group(slot.group_index)),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(provenant_fabric_runtime::graph::RepairPanelResolution {
            slots,
            frame,
            verifiers,
            synthesis,
            advice,
        })
    }

    fn input_cap_source(&self) -> provenant_fabric_runtime::graph::InputCapSource {
        self.factory.context.graph_prompt_limit_source
    }

    fn launch_retry_policy(&self) -> provenant_fabric_runtime::graph::LaunchRetryPolicy {
        use provenant_config::KnobSource;
        let mut policy = provenant_fabric_runtime::graph::LaunchRetryPolicy::default();
        let config = match provenant_config::read_with_domains(
            &self.factory.context.roots,
            &provenant_cli::domains::SECTIONS,
        ) {
            Ok(config) => config,
            Err(error) => {
                use std::io::Write;
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "warning: graph launch retry settings unavailable: {error}; using defaults"
                );
                return policy;
            }
        };
        if let Some(knob) = config.knobs.get("lanes.launch_retry_limit") {
            if let Some(value) = knob.value.as_u64().and_then(|n| u8::try_from(n).ok()) {
                policy.limit = value;
            }
            policy.limit_source = match knob.source {
                KnobSource::Default => "default",
                KnobSource::Profile => "profile",
                KnobSource::Override => "override",
            }
            .into();
        }
        if let Some(value) = config
            .knobs
            .get("lanes.launch_retry_first_seconds")
            .and_then(|knob| knob.value.as_u64())
        {
            policy.first_seconds = value;
        }
        if let Some(value) = config
            .knobs
            .get("lanes.launch_retry_max_seconds")
            .and_then(|knob| knob.value.as_u64())
        {
            policy.max_seconds = value;
        }
        policy
    }

    fn begin_launch(
        &mut self,
        access: provenant_fabric_runtime::graph::AccessMode,
        deadline: Option<provenant_core::WallTime>,
    ) -> Result<Option<provenant_core::WallTime>, Self::Error> {
        if access != provenant_fabric_runtime::graph::AccessMode::WorktreeWrite {
            self.writer_launch_budget = None;
            return Ok(None);
        }
        let now = self.factory.clock.mono_now();
        let wall = self
            .factory
            .clock
            .wall_now()
            .map_err(|_| FacadeError::StateCorrupt)?;
        let budget = match deadline {
            Some(end) => WaitBudget::up_to(
                now,
                provenant_core::DurationMicros::from_micros(
                    end.as_micros().saturating_sub(wall.as_micros()).max(0) as u64,
                ),
            ),
            None => provenant_cli::fabric::graph_relay::writer_launch_budget(
                &self.factory.context,
                &self.factory.clock,
                self.budget,
            ),
        };
        self.writer_launch_budget = Some(budget);
        Ok(Some(
            deadline.unwrap_or(
                wall.checked_add(budget.remaining(now))
                    .map_err(|_| FacadeError::StateCorrupt)?,
            ),
        ))
    }

    fn admission_timeout_seconds(&self) -> Option<u64> {
        if let Some(budget) = self.writer_launch_budget {
            return Some(
                budget
                    .remaining(self.factory.clock.mono_now())
                    .as_micros()
                    .div_ceil(1_000_000),
            );
        }
        let configured = provenant_confinement::run_config::RunConfig::read(
            self.factory.context.roots.state.as_path(),
        )
        .ok()
        .and_then(|config| {
            config.admission_seconds_for(&self.factory.context.identity.project.to_string())
        });
        let effective = provenant_run::AdmissionPolicy::resolve_timeout(None, configured);
        Some(self.admission_timeout.map_or(effective.seconds, |limit| {
            let micros = limit.remaining(self.factory.clock.mono_now()).as_micros();
            effective.seconds.min(micros / 1_000_000)
        }))
    }
    type Error = FacadeError;
    fn resolve_review_base(
        &mut self,
        reference: &str,
        worktree: Option<&str>,
    ) -> Result<Option<String>, Self::Error> {
        let directory = worktree.map_or_else(
            || self.factory.cwd.clone(),
            |path| self.factory.cwd.join(path),
        );
        self.factory
            .resolve_base(&directory, reference, &self.repo_facts, self.cancel)
            .map(|base| Some(base.to_string()))
    }

    fn take_launch_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.launch_warnings)
    }

    fn resolve_creation_base(
        &mut self,
        branch: &str,
        reference: &str,
        default_base: bool,
    ) -> Result<Option<String>, Self::Error> {
        // Only a new top-level writer may prefer a remote-only branch. Child
        // provisioning and recovery always use their checkpointed source.
        let remote_tip = provenant_confinement::worktrees::with_context_using_facts(
            &self.factory.context.roots,
            &self.factory.home,
            &self.factory.cwd,
            &self.factory.clock,
            self.cancel,
            &self.repo_facts,
            |context| {
                provenant_confinement::worktrees::fetch_remote_branch_tip(
                    context,
                    branch,
                    self.cancel,
                )
            },
        )
        .map_err(landing::repository_fact_error)?;
        if let Some(tip) = remote_tip {
            if !default_base && reference != tip.to_string() {
                let warning = format!(
                    "ignoring --base {reference} for --branch {branch}; using remote tip {tip}"
                );
                let _ = writeln!(std::io::stderr().lock(), "warning: {warning}");
                self.launch_warnings.push(warning);
            }
            return Ok(Some(tip.to_string()));
        }
        if default_base {
            let (base, warnings) = provenant_confinement::worktrees::with_context_using_facts(
                &self.factory.context.roots,
                &self.factory.home,
                &self.factory.cwd,
                &self.factory.clock,
                self.cancel,
                &self.repo_facts,
                |context| {
                    provenant_confinement::worktrees::default_creation_base(
                        context,
                        &self.repo_facts,
                        self.cancel,
                    )
                },
            )
            .map_err(landing::repository_fact_error)?;
            for warning in warnings {
                let _ = writeln!(std::io::stderr().lock(), "warning: {warning}");
                self.launch_warnings.push(warning);
            }
            return Ok(Some(base.to_string()));
        }
        self.resolve_review_base(reference, None)
    }

    fn retry_allowance(
        &self,
        node: &provenant_records::ExtraFields,
        graph: &provenant_records::ExtraFields,
    ) -> Result<Option<(u8, String)>, Self::Error> {
        Ok(Some(graph_retry_allowance(
            node,
            graph,
            &self.factory.context.roots,
        )))
    }

    fn preparation_fallback_evidence(
        &mut self,
        key: &provenant_fabric_runtime::graph::AttemptKey,
        reference: &str,
    ) -> Result<Option<provenant_fabric_runtime::graph::PreparationFallbackEvidence>, Self::Error>
    {
        let (host, attempt) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
        // Only this host can verify its writer fences and custody lock.
        if host != self.factory.context.identity.host.to_string() {
            return Ok(None);
        }
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: false,
            actor: None,
            chair: None,
        };
        let store = owner.store_with_facts(self.cancel, &self.repo_facts)?;
        let call = owner.call(self.cancel);
        let record = store
            .record(
                attempt.parse().map_err(|_| FacadeError::StateCorrupt)?,
                &call,
            )
            .map_err(run_error)?;
        if record.operation_id != graph_operation("graph-attempt-operation", key)?
            || record.context.as_ref().is_none_or(|context| {
                context.graph_id.as_deref() != Some(key.graph_id.as_str())
                    || context.node_id.as_deref() != Some(key.node_id.as_str())
            })
        {
            return Ok(None);
        }
        let custody_released = store.custody_released(&record, &call).map_err(run_error)?;
        Ok(record.preparation_failure.map(|failure| {
            provenant_fabric_runtime::graph::PreparationFallbackEvidence {
                failure,
                handoff: if record.provider_launched == Some(false) {
                    provenant_routing::FallbackHandoff::Local
                } else {
                    provenant_routing::FallbackHandoff::UncertainOrLaunched
                },
                custody_released,
            }
        }))
    }

    fn quota_alias_redraw(
        &mut self,
        key: &provenant_fabric_runtime::graph::AttemptKey,
        reference: &str,
    ) -> Result<bool, Self::Error> {
        use provenant_records::run::{AttemptState, Outcome, RequestedRoute};
        let (host, attempt) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
        // Only the owning host can prove terminal custody and the sealed output.
        if host != self.factory.context.identity.host.to_string() {
            return Ok(false);
        }
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: false,
            actor: None,
            chair: None,
        };
        let store = owner.store_with_facts(self.cancel, &self.repo_facts)?;
        let call = owner.call(self.cancel);
        let record = store
            .record(
                attempt.parse().map_err(|_| FacadeError::StateCorrupt)?,
                &call,
            )
            .map_err(run_error)?;
        if record.operation_id != graph_operation("graph-attempt-operation", key)?
            || record.context.as_ref().is_none_or(|context| {
                context.graph_id.as_deref() != Some(key.graph_id.as_str())
                    || context.node_id.as_deref() != Some(key.node_id.as_str())
            })
            || !matches!(
                record.state,
                AttemptState::Terminal {
                    outcome: Outcome::QuotaLimited,
                    ..
                }
            )
            || !matches!(record.requested_route, RequestedRoute::Named { .. })
            || record
                .applied_route
                .as_ref()
                .and_then(|route| route.model.as_ref())
                .is_none()
            || !store.custody_released(&record, &call).map_err(run_error)?
        {
            return Ok(false);
        }
        // Main 4c52d7f28 deliberately permits partial output and writer commits.
        // The checks above prove identity, typed quota, named alias, an applied
        // member, and released custody; the kernel separately bounds redraws.
        Ok(matches!(
            record.requested_route,
            RequestedRoute::Named { .. }
        ))
    }

    fn preparation_refusal_cause(
        &mut self,
        key: &provenant_fabric_runtime::graph::AttemptKey,
        reference: &str,
    ) -> Result<Option<provenant_records::RouteExclusionReason>, Self::Error> {
        Ok(self
            .preparation_skipped_routes(key, reference)?
            .and_then(|skipped| unanimous_preparation_cause(&skipped)))
    }

    fn preparation_skipped_accounts(
        &mut self,
        key: &provenant_fabric_runtime::graph::AttemptKey,
        reference: &str,
    ) -> Result<Vec<provenant_records::SkippedAccount>, Self::Error> {
        Ok(self
            .preparation_skipped_routes(key, reference)?
            .unwrap_or_default()
            .iter()
            .flat_map(|route| provenant_records::SkippedAccount::read(&route.extra))
            .collect())
    }

    fn attempt_models(&mut self, attempts: &[String]) -> Result<Vec<String>, Self::Error> {
        self.prior_models(attempts)
    }

    fn bind_writer_worktree(
        &mut self,
        path: &str,
    ) -> Result<Option<provenant_fabric_runtime::graph::WriterWorktreeBinding>, Self::Error> {
        let (directory, _) = selected_writer_project(self.factory, path)?;
        let request = provenant_run::resolve_writer(
            self.factory.context.roots.state.as_path(),
            &directory,
            &directory,
        )
        .map_err(run_error)?;
        let writer = provenant_run::prepare_writer_with_facts(
            &request,
            self.factory.context.roots.state.as_path(),
            self.factory.context.identity.project, &self.repo_facts)
        .map_err(|error| match error {
            provenant_run::RunError::Validation("writer needs a branch") => FacadeError::DispatchRefused {
                fact: Some(provenant_fabric_runtime::coordination::facade::DispatchRefusalFact::DetachedWriter),
                disk: None,
                reason: "writer needs a branch".into(),
            },
            other => run_error(other),
        })?;
        let worktree = writer
            .worktree
            .path()
            .to_str()
            .ok_or(FacadeError::InvalidInput { detail: None })?
            .to_owned();
        let identity = writer.worktree.identity();
        Ok(Some(
            provenant_fabric_runtime::graph::WriterWorktreeBinding {
                extra: [(
                    "source_directory_identity".into(),
                    serde_json::json!({
                        "device": identity.device, "inode": identity.inode,
                    }),
                )]
                .into(),
                worktree,
                head: writer.source_commit().to_string(),
            },
        ))
    }

    fn provision_writer_worktree(
        &mut self,
        path: &str,
        branch: &str,
        base: &str,
    ) -> Result<Option<provenant_fabric_runtime::graph::WriterWorktreeBinding>, Self::Error> {
        provenant_cli::fabric::graph_relay::provision_writer_checked(
            &self.factory.context,
            path,
            branch,
            base,
            self.writer_launch_budget.unwrap_or(self.budget),
            self.cancel,
        )
        .map_err(|error| run_error(error.into()))?;
        self.bind_writer_worktree(path)
    }

    fn provision_child_worktree(&mut self, path: &str, source: &str) -> Result<bool, Self::Error> {
        provenant_cli::fabric::graph_relay::provision_child_with_budget(
            &self.factory.context,
            path,
            source,
            self.writer_launch_budget.unwrap_or(self.budget),
            self.cancel,
        )
        .map(|()| true)
        .map_err(super::interface::relay_error)
    }

    fn bound_graph_bases(
        &self,
    ) -> (
        Option<provenant_core::GitOid>,
        Option<provenant_core::GitOid>,
    ) {
        self.integration_base
            .map_or((None, None), |bases| (bases.review, bases.integration))
    }

    fn applied_route(
        &mut self,
        reference: &str,
    ) -> Result<Option<provenant_fabric_runtime::graph::GraphRoute>, Self::Error> {
        let (host, attempt) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
        let target = AttemptTarget {
            extra: Default::default(),
            host: Some(host.parse().map_err(|_| FacadeError::StateCorrupt)?),
            attempt: attempt.parse().map_err(|_| FacadeError::StateCorrupt)?,
        };
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
            actor: None,
            chair: None,
        };
        Ok(owner
            .snapshot(target, self.budget, self.cancel)?
            .applied_route
            .map(|route| provenant_fabric_runtime::graph::GraphRoute {
                extra: Default::default(),
                adapter: route.provider,
                model: route.model,
                effort: route.effort,
                account: route.account_label,
                name: None,
                alias: None,
            }))
    }

    fn review_range(
        &mut self,
        reference: &str,
        explicit_base: bool,
    ) -> Result<provenant_fabric_runtime::graph::ReviewRange, Self::Error> {
        use provenant_fabric_runtime::graph::ReviewRange;
        let (host, attempt) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
        if host != self.factory.context.identity.host.to_string() {
            return Ok(ReviewRange::Unknown);
        }
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
            actor: None,
            chair: None,
        };
        let store = owner.store_with_facts(self.cancel, &self.repo_facts)?;
        let call = owner.call(self.cancel);
        let record = store
            .record(
                attempt.parse().map_err(|_| FacadeError::StateCorrupt)?,
                &call,
            )
            .map_err(run_error)?;
        let request = store.request(record.run_id, &call).map_err(run_error)?;
        let task = request
            .tasks
            .iter()
            .find(|task| task.task_id == record.task_id)
            .ok_or(FacadeError::StateCorrupt)?;
        if !explicit_base
            && self
                .integration_base
                .is_none_or(|bases| bases.review.is_none() && bases.integration.is_none())
        {
            return Ok(ReviewRange::NoBase);
        }
        Ok(match task.base.as_ref().zip(task.head.as_ref()) {
            Some((base, head)) => ReviewRange::Resolved {
                base: base.clone(),
                head: head.clone(),
            },
            None => ReviewRange::Unknown,
        })
    }
    fn respect_ci_wake(&self) -> bool {
        self.tick
    }
    fn ci_poll_backoff(&self, node_id: &str) -> provenant_fabric_runtime::graph::CiPollBackoff {
        let config = &self.factory.context.config.graph;
        config
            .ci_poll_overrides
            .get(node_id)
            .copied()
            .unwrap_or(config.ci_poll_backoff)
    }
    fn integration_head(&mut self) -> Result<Option<String>, Self::Error> {
        let branch =
            self.integration_branch
                .as_deref()
                .ok_or_else(|| FacadeError::DispatchRefused {
                    fact: None,
                    disk: None,
                    reason: "an integration source requires graph.integration_branch".into(),
                })?;
        let reference = match branch.strip_prefix("origin/") {
            Some(branch) => format!("refs/remotes/origin/{branch}"),
            None => format!("refs/heads/{branch}"),
        };
        self.factory
            .resolve_base(&self.factory.cwd, &reference, &self.repo_facts, self.cancel)
            .map(|base| Some(base.to_string()))
    }
    fn ci_status(&mut self) -> Option<&mut dyn provenant_fabric_runtime::graph::CiStatus> {
        Some(self)
    }
    fn actor(&self) -> String {
        self.factory.context.identity.inbox.actor.to_string()
    }
    fn validate_expansion(
        &mut self,
        document: &provenant_fabric_runtime::graph::GraphDocument,
        _checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
    ) -> Result<(), Self::Error> {
        for node in &document.nodes {
            if let Some(provenant_records::run::DeliveryMode::Integrate(target)) = &node.delivery {
                provenant_run::HostDeliveryContext::validate_integration_target(
                    &self.factory.cwd,
                    target,
                )
                .map_err(run_error)?;
            }
        }
        Ok(())
    }
    fn node_ask(
        &mut self,
        scope: provenant_fabric_runtime::graph::AskScope<'_>,
    ) -> Result<Option<provenant_fabric_runtime::graph::AskReply>, Self::Error> {
        graph_ask_edge(self.factory, self.cancel, scope, false)
    }
    fn notify_owner(
        &mut self,
        scope: provenant_fabric_runtime::graph::AskScope<'_>,
    ) -> Result<bool, Self::Error> {
        use provenant_fabric_runtime::inbox::{InboxResult, InboxService};
        let context = &self.factory.context;
        let mut identity = context.identity.clone();
        identity.inbox.actor = scope.actor.parse().map_err(|_| FacadeError::StateCorrupt)?;
        let input = graph_notification_input(&scope)?;
        let refused = |error: &dyn std::fmt::Display| FacadeError::DispatchRefused {
            fact: None,
            disk: None,
            reason: format!("graph owner notification {}: {error}", scope.ask.ask_id),
        };
        provenant_fabric_runtime::inbox::validate_request(
            &identity,
            &context.config,
            context.in_lane,
            &input,
        )
        .map_err(|error| refused(&error))?;
        let service = InboxService::open(
            &context.roots.state,
            &identity,
            &self.factory.clock,
            self.cancel,
        )
        .map_err(|error| refused(&error))?;
        let result = service
            .execute(input, self.cancel)
            .map_err(|error| refused(&error))?;
        Ok(matches!(result, InboxResult::Saved { .. }))
    }
    fn close_node_ask(
        &mut self,
        scope: provenant_fabric_runtime::graph::AskScope<'_>,
    ) -> Result<(), Self::Error> {
        graph_ask_edge(self.factory, self.cancel, scope, true).map(|_| ())
    }
    fn wall_now(&self) -> Option<provenant_core::WallTime> {
        self.factory.clock.wall_now().ok()
    }
    fn land_policy(
        &self,
        tier: provenant_records::workflow::RiskTier,
    ) -> provenant_fabric_runtime::config::LandChoice {
        let project = self.factory.context.identity.project.to_string();
        self.factory
            .context
            .config
            .graph
            .land
            .choose(Some(&project), tier)
    }
    fn review_coverage(
        &mut self,
        branch: &str,
        reviewed: provenant_core::review_cover::Reviewed<'_>,
        head: &str,
    ) -> Result<Option<provenant_core::review_cover::Coverage>, Self::Error> {
        // A proof that cannot be read, including before the branch has a
        // pull request, refuses the land as an unproved one does.
        Ok(
            crate::pr::landing::approval::graph_coverage(&self.factory.cwd, branch, reviewed, head)
                .ok(),
        )
    }
    fn land(
        &mut self,
        request: provenant_fabric_runtime::graph::LandRequest<'_>,
    ) -> Result<provenant_fabric_runtime::graph::LandObservation, Self::Error> {
        Ok(crate::pr::land::graph_land(
            &self.factory.cwd,
            self.factory.context.roots.state.as_path(),
            request.branch,
            request.head,
            request.pull_request,
            request.authority.is_some(),
            self.factory.workers,
        ))
    }
    fn cost_unknown_reason(&mut self) -> Option<String> {
        let graph_budget = &self.factory.context.config.graph.budget;
        if graph_budget.per_graph.cost_usd.is_none() && graph_budget.per_node.cost_usd.is_none() {
            return None;
        }
        self.prices
            .get_or_insert_with(|| load_graph_prices(&self.factory.assets))
            .as_ref()
            .err()
            .cloned()
    }
    fn deliver_graph(
        &mut self,
        document: &provenant_fabric_runtime::graph::GraphDocument,
        checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
    ) -> Result<Option<String>, Self::Error> {
        let branch = document
            .integration_branch
            .as_deref()
            .ok_or(FacadeError::InvalidInput { detail: None })?;
        let session = BinaryRunOwner {
            factory: self.factory,
            allow_remote: false,
            actor: None,
            chair: None,
        }
        .session_with_facts(self.cancel, &self.repo_facts)?;
        let call = provenant_run::Request {
            clock: &self.factory.clock,
            cancel: self.cancel,
        };
        let mut body = format!("Graph {}\n\n", document.graph_id);
        let mut reviews = Vec::new();
        for node in &checkpoint.nodes {
            if let Some(summary) = node.deterministic_summary() {
                body.push_str(&summary);
                continue;
            }
            let reference = node
                .attempts
                .last()
                .and_then(|attempt| attempt.attempt_ref.as_deref())
                .ok_or(FacadeError::StateCorrupt)?;
            let (_, id) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
            let id = id.parse().map_err(|_| FacadeError::StateCorrupt)?;
            let attempt = session.service().status(id, &call).map_err(run_error)?;
            body.push_str(&format!(
                "Node {}: attempt {id}, run {}\n\n",
                node.id, attempt.run_id
            ));
            if graph_review_node(document, &node.id) {
                // Retain each launched review route, including failed attempts
                // before a successful retry. These are evidence, not a verdict.
                for review in &node.attempts {
                    let Some(reference) = review.attempt_ref.as_deref() else {
                        continue;
                    };
                    let (_, id) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
                    let id = id.parse().map_err(|_| FacadeError::StateCorrupt)?;
                    let record = session.service().status(id, &call).map_err(run_error)?;
                    let mut facts = provenant_cli::run::recorded_attempt_facts(&record);
                    facts.role = document
                        .nodes
                        .iter()
                        .find(|spec| spec.id == node.id)
                        .and_then(|spec| spec.role.clone())
                        .or(facts.role);
                    // Each row describes its own result, including failed
                    // routes before a retry. Never borrow a later verdict.
                    facts.verdict = record.result.as_ref().and_then(|_| {
                        let captured = session.store.stream_result(id, &call, None).ok()?;
                        (!captured.verdict_invalid)
                            .then_some(captured.verdict)?
                            .map(String::from)
                    });
                    facts.head = node
                        .source_commit
                        .as_deref()
                        .and_then(|head| provenant_core::GitOid::parse(head).ok())
                        .or(facts.head);
                    reviews.push(facts);
                }
            }
        }
        body.push_str(&provenant_cli::pr_body::independent_review_rows(&reviews));
        let url = provenant_run::HostDeliveryContext::capture()
            .with_repo_facts(&self.repo_facts)
            .deliver_graph_pr(
                &self.factory.cwd,
                branch,
                self.integration_base
                    .and_then(|bases| bases.integration)
                    .ok_or(FacadeError::Conflict)?,
                &body,
            )
            .map_err(run_error)?;
        Ok(Some(url))
    }
    fn usage(
        &mut self,
        attempt_ref: &str,
    ) -> Result<provenant_fabric_runtime::graph::AttemptUsage, Self::Error> {
        let (host, attempt) = attempt_ref
            .split_once('/')
            .ok_or(FacadeError::StateCorrupt)?;
        let target = AttemptTarget {
            extra: Default::default(),
            host: Some(host.parse().map_err(|_| FacadeError::StateCorrupt)?),
            attempt: attempt.parse().map_err(|_| FacadeError::StateCorrupt)?,
        };
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
            actor: None,
            chair: None,
        };
        let record = owner.snapshot(target, self.budget, self.cancel)?;
        let cost_unknown_reason = self.cost_unknown_reason();
        let prices = self
            .prices
            .get_or_insert_with(|| load_graph_prices(&self.factory.assets));
        Ok(graph_attempt_usage(
            record,
            prices.as_ref().ok(),
            cost_unknown_reason,
        ))
    }

    fn attempt_families(&mut self, attempts: &[String]) -> Result<Vec<String>, Self::Error> {
        self.writer_families(attempts)
    }

    fn materialise_inputs(
        &mut self,
        key: &provenant_fabric_runtime::graph::AttemptKey,
        requests: &[provenant_fabric_runtime::graph::InputRequest],
        cap: usize,
    ) -> Option<provenant_fabric_runtime::graph::MaterialisedInputs> {
        Some(graph_inputs::materialise(
            self.factory,
            self.budget,
            self.cancel,
            key,
            requests,
            cap,
        ))
    }

    fn placement_host_id(&self, name: &str) -> Result<Option<String>, Self::Error> {
        let local = self.factory.context.identity.host;
        let host = if matches!(name, "local" | "localhost")
            || name
                .parse::<provenant_core::HostId>()
                .is_ok_and(|host| host == local)
        {
            local
        } else {
            self.factory
                .context
                .peer_host(name)
                .ok_or(FacadeError::UnknownHost)?
        };
        Ok(Some(host.to_string()))
    }

    fn stop_cancelled_attempt(
        &mut self,
        attempt: &provenant_fabric_runtime::graph::GraphAttempt,
    ) -> Result<(), Self::Error> {
        let mut owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
            actor: None,
            chair: None,
        };
        self.factory
            .cancel_graph_attempt(&mut owner, attempt, self.budget, self.cancel)
    }

    fn cancelled_attempt_settled(
        &mut self,
        attempt: &provenant_fabric_runtime::graph::GraphAttempt,
    ) -> Result<bool, Self::Error> {
        if let Some(settled) =
            self.factory
                .cancel_peer_submission(attempt, false, self.budget, self.cancel)?
        {
            return Ok(settled);
        }
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
            actor: Some(
                attempt
                    .launch_actor
                    .parse()
                    .map_err(|_| FacadeError::StateCorrupt)?,
            ),
            chair: None,
        };
        if let Some(reference) = attempt.attempt_ref.as_deref() {
            let (host, id) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
            let host = host.parse().map_err(|_| FacadeError::StateCorrupt)?;
            let target = AttemptTarget {
                extra: Default::default(),
                host: Some(host),
                attempt: id.parse().map_err(|_| FacadeError::StateCorrupt)?,
            };
            if host == self.factory.context.identity.host {
                let store = owner.store_with_facts(self.cancel, &self.repo_facts)?;
                let call = owner.call(self.cancel);
                if let Some(observation) = provenant_cli::fabric::graph_cloud::installed(
                    &self.factory.context.roots,
                    &store,
                    target.attempt,
                    "",
                    reference,
                    &call,
                    &self.factory.cwd,
                    std::time::Duration::from_micros(
                        self.budget
                            .remaining(self.factory.clock.mono_now())
                            .as_micros(),
                    ),
                    true,
                )
                .map_err(|_| FacadeError::InvalidInput { detail: None })?
                {
                    return Ok(matches!(
                        observation,
                        provenant_fabric_runtime::graph::AttemptObservation::Terminal { .. }
                    ));
                }
            }
            let record = owner.snapshot(target, self.budget, self.cancel)?;
            if record.extra.get("peer_subtree").is_some_and(|subtree| {
                matches!(subtree["status"].as_str(), Some("pending" | "paused"))
            }) {
                return Ok(false);
            }
            return Ok(matches!(
                record.state,
                provenant_records::run::AttemptState::Terminal { .. }
            ) && record.custody_pending == Some(false));
        }
        let operation = graph_operation("graph-attempt-operation", &attempt.key)?;
        let store = owner.store_with_facts(self.cancel, &self.repo_facts)?;
        let call = owner.call(self.cancel);
        match store.operation(&operation, &call).map_err(run_error)? {
            // Absence in this host's store cannot settle an uncertain peer
            // launch whose accepted target has not reached the graph yet.
            None => Ok(!self.uncertain_peer_placement(attempt)),
            Some(operation) if operation.state == provenant_run::OperationState::Refused => {
                Ok(true)
            }
            Some(operation) => {
                use provenant_run::{AttemptFilter, AttemptReader};
                let attempts = store
                    .attempts(
                        AttemptFilter {
                            run_id: Some(operation.run_id),
                            task_id: None,
                        },
                        &call,
                    )
                    .map_err(run_error)?;
                let mut found = false;
                for fact in attempts {
                    let fact = fact.map_err(run_error)?;
                    let record = store.record(fact.attempt_id, &call).map_err(run_error)?;
                    found = true;
                    let reference = format!(
                        "{}/{}",
                        self.factory.context.identity.host, record.attempt_id
                    );
                    if let Some(observation) = provenant_cli::fabric::graph_cloud::installed(
                        &self.factory.context.roots,
                        &store,
                        record.attempt_id,
                        "",
                        &reference,
                        &call,
                        &self.factory.cwd,
                        std::time::Duration::from_micros(
                            self.budget
                                .remaining(self.factory.clock.mono_now())
                                .as_micros(),
                        ),
                        true,
                    )
                    .map_err(|_| FacadeError::InvalidInput { detail: None })?
                    {
                        if matches!(
                            observation,
                            provenant_fabric_runtime::graph::AttemptObservation::Terminal { .. }
                        ) {
                            continue;
                        }
                        return Ok(false);
                    }
                    if !store.custody_released(&record, &call).map_err(run_error)? {
                        return Ok(false);
                    }
                }
                // A prepared operation with no attempt never admitted work;
                // an unknown launch without evidence must remain unresolved.
                Ok(found || operation.state == provenant_run::OperationState::Prepared)
            }
        }
    }

    fn launch_accepted(
        &mut self,
        attempt: &provenant_fabric_runtime::graph::GraphAttempt,
    ) -> Result<bool, Self::Error> {
        if attempt.attempt_ref.is_some() {
            return Ok(true);
        }
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
            actor: Some(
                attempt
                    .launch_actor
                    .parse()
                    .map_err(|_| FacadeError::StateCorrupt)?,
            ),
            chair: None,
        };
        let operation_id = graph_operation("graph-attempt-operation", &attempt.key)?;
        let store = owner.store_with_facts(self.cancel, &self.repo_facts)?;
        let call = owner.call(self.cancel);
        Ok(store
            .operation(&operation_id, &call)
            .map_err(run_error)?
            .is_some_and(|operation| {
                !matches!(
                    operation.state,
                    provenant_run::OperationState::Prepared
                        | provenant_run::OperationState::Refused
                )
            }))
    }

    fn launch_refused_before_acceptance(
        &mut self,
        attempt: &provenant_fabric_runtime::graph::GraphAttempt,
    ) -> Result<bool, Self::Error> {
        if attempt.attempt_ref.is_some() {
            return Ok(false);
        }
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
            actor: Some(
                attempt
                    .launch_actor
                    .parse()
                    .map_err(|_| FacadeError::StateCorrupt)?,
            ),
            chair: None,
        };
        let operation = graph_operation("graph-attempt-operation", &attempt.key)?;
        let store = owner.store_with_facts(self.cancel, &self.repo_facts)?;
        let call = owner.call(self.cancel);
        // An absent local operation proves nothing about a peer submission;
        // local prelaunch refusals keep their existing cancellation semantics.
        Ok(
            match store.operation(&operation, &call).map_err(run_error)? {
                Some(operation) => matches!(
                    operation.state,
                    provenant_run::OperationState::Prepared
                        | provenant_run::OperationState::Refused
                ),
                None => !self.uncertain_peer_placement(attempt),
            },
        )
    }

    fn observe_or_start(
        &mut self,
        key: &provenant_fabric_runtime::graph::AttemptKey,
        node: provenant_fabric_runtime::graph::NodeInput<'_>,
        route: &provenant_fabric_runtime::graph::GraphRoute,
        access: provenant_fabric_runtime::graph::AccessMode,
        worktree: Option<&str>,
        replay: provenant_fabric_runtime::graph::Replay<'_>,
    ) -> Result<provenant_fabric_runtime::graph::AttemptObservation, Self::Error> {
        if replay.stored_ref.is_some() {
            return self.observe_or_start_inner(key, node, route, access, worktree, replay, None);
        }
        let refusal = GraphRefusalScope::new(self, key, &node, route, access, worktree, replay)?;
        // Refusals have no admitted run. Replay them before reading a request,
        // consulting current writer state, or repeating any preparation effect.
        if replay.stored_ref.is_none() {
            let store = self.refusal_store(&refusal)?;
            let call = provenant_run::Request {
                clock: &self.factory.clock,
                cancel: self.cancel,
            };
            if let Some(record) = store
                .operation(&refusal.operation, &call)
                .map_err(run_error)?
                && record.state == provenant_run::OperationState::Refused
            {
                let intent = record
                    .extra
                    .get("origin_intent_digest")
                    .ok_or(FacadeError::StateCorrupt)?;
                if intent != &serde_json::json!(refusal.digest) {
                    return Err(run_error(provenant_run::RunError::OperationConflict));
                }
                self.post_graph_refusal(&store, &refusal.operation, &call);
                return Err(stored_graph_refusal(&record)?);
            }
        }
        let result =
            self.observe_or_start_inner(key, node, route, access, worktree, replay, Some(&refusal));
        if replay.stored_ref.is_none()
            && let Err(error) = &result
            && matches!(
                error,
                FacadeError::DispatchRefused { .. }
                    | FacadeError::InvalidInput { .. }
                    | FacadeError::GraphRelayRefused { .. }
                    | FacadeError::ConfinementRefused { .. }
                    | FacadeError::CredentialRoot { .. }
                    | FacadeError::ContinuationUnsupported
                    | FacadeError::ProviderStateExpired
                    | FacadeError::LaneDepth
                    | FacadeError::LaneCoordinationRefused
                    | FacadeError::WorktreeMissing
                    | FacadeError::WorktreeUnregistered { .. }
                    | FacadeError::WorktreeNotApplicable
                    | FacadeError::BaseUnreachable
                    | FacadeError::BranchExists
            )
        {
            // Busy/unknown handoffs and storage errors are not terminal
            // refusals. An admitted operation retains its own custody owner.
            let store = self.refusal_store(&refusal)?;
            let call = provenant_run::Request {
                clock: &self.factory.clock,
                cancel: self.cancel,
            };
            if store
                .operation(&refusal.operation, &call)
                .map_err(run_error)?
                .is_none()
            {
                self.retain_graph_refusal(&refusal, error, error.diagnostic())?;
            }
        }
        result
    }
}
impl BoundGraphOwner<'_> {
    #[allow(clippy::too_many_arguments)] // Launch facts stay outside the kernel's public input shape.
    fn observe_or_start_inner(
        &mut self,
        key: &provenant_fabric_runtime::graph::AttemptKey,
        node: provenant_fabric_runtime::graph::NodeInput<'_>,
        route: &provenant_fabric_runtime::graph::GraphRoute,
        access: provenant_fabric_runtime::graph::AccessMode,
        worktree: Option<&str>,
        replay: provenant_fabric_runtime::graph::Replay<'_>,
        refusal: Option<&GraphRefusalScope>,
    ) -> Result<provenant_fabric_runtime::graph::AttemptObservation, FacadeError> {
        use provenant_fabric_runtime::graph::{AttemptObservation, AttemptOutcome};
        let mut owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
            actor: None,
            chair: None,
        };
        let operation_id = graph_operation("graph-attempt-operation", key)?;
        let cancel = self.cancel;
        let blocked = |owner: &BinaryRunOwner<'_>| -> Result<Option<String>, FacadeError> {
            let store = owner.store_with_facts(cancel, &self.repo_facts)?;
            Ok(store
                .operation(&operation_id, &owner.call(cancel))
                .map_err(run_error)?
                .filter(|operation| operation.state == provenant_run::OperationState::Blocked)
                .map(|operation| {
                    operation
                        .blocked_reason
                        .unwrap_or_else(|| "custody release failed".into())
                }))
        };
        let (target, reference) = if let Some(reference) = replay.stored_ref {
            // An accepted attempt is observed by reference: operation replay is
            // actor-scoped, so resubmitting under another actor would launch again.
            let (host, attempt) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
            let target = AttemptTarget {
                extra: Default::default(),
                host: Some(host.parse().map_err(|_| FacadeError::StateCorrupt)?),
                attempt: attempt.parse().map_err(|_| FacadeError::StateCorrupt)?,
            };
            (target, Some(reference.to_owned()))
        } else {
            let refusal = refusal.ok_or(FacadeError::StateCorrupt)?;
            let host = node
                .attempt
                .and_then(|spec| spec.host.as_deref())
                .map(|name| {
                    let local = self.factory.context.identity.host;
                    if matches!(name, "local" | "localhost")
                        || name
                            .parse::<provenant_core::HostId>()
                            .is_ok_and(|host| host == local)
                    {
                        Ok(local)
                    } else {
                        self.factory
                            .context
                            .peer_host(name)
                            .ok_or(FacadeError::UnknownHost)
                    }
                })
                .transpose()?;
            if host.is_some_and(|host| host != self.factory.context.identity.host)
                && (access != provenant_fabric_runtime::graph::AccessMode::ReadOnly
                    || node.attempt.is_some_and(|spec| {
                        spec.placement.is_some() || spec.new_worktree.is_some()
                    }))
            {
                return Err(FacadeError::WorktreeNotApplicable);
            }
            if let Some(cloud) = node.attempt.and_then(|spec| spec.placement.as_ref()) {
                use provenant_fabric_runtime::coordination::facade::CloudPlacementIssue as Issue;
                let invalid =
                    |issue| FacadeError::invalid(InvalidInputDetail::CloudPlacement(issue));
                for (issue, value) in [
                    (Issue::Target, &cloud.target),
                    (Issue::Provider, &cloud.provider),
                    (Issue::Repository, &cloud.repository),
                    (Issue::Branch, &cloud.branch),
                    (Issue::Model, &cloud.model),
                ] {
                    if value.trim().is_empty() {
                        return Err(invalid(issue));
                    }
                }
                let Some(transport) = provenant_records::AdapterSpec::get(&cloud.provider)
                    .and_then(|spec| spec.cloud_transport)
                else {
                    return Err(invalid(Issue::Provider));
                };
                if transport == provenant_records::CloudTransport::Codex
                    && cloud.environment.trim().is_empty()
                {
                    return Err(invalid(Issue::Environment));
                }
                if cloud.deadline_seconds == 0 {
                    return Err(invalid(Issue::Deadline));
                }
            }
            // Replaying an accepted operation must observe its existing writer,
            // even after that writer has advanced beyond the pinned start.
            owner.actor = Some(
                replay
                    .launch_actor
                    .parse()
                    .map_err(|_| FacadeError::StateCorrupt)?,
            );
            if let Some(reason) = blocked(&owner)? {
                return Err(FacadeError::DispatchRefused {
                    fact: None,
                    disk: None,
                    reason,
                });
            }
            let operation_id = graph_operation("graph-attempt-operation", key)?;
            let store = owner.store_with_facts(self.cancel, &self.repo_facts)?;
            let call = owner.call(self.cancel);
            let operation = store.operation(&operation_id, &call).map_err(run_error)?;
            let accepted = operation.as_ref().is_some_and(|operation| {
                !matches!(
                    operation.state,
                    provenant_run::OperationState::Prepared
                        | provenant_run::OperationState::Refused
                )
            });
            let writer_project =
                if access == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite {
                    worktree
                        .map(|path| selected_writer_project(self.factory, path))
                        .transpose()?
                } else {
                    None
                };
            let writer_directory = writer_project
                .as_ref()
                .map_or(self.factory.cwd.as_path(), |(directory, _)| {
                    directory.as_path()
                });
            let foreign_writer = writer_project
                .as_ref()
                .is_some_and(|(_, project)| *project != self.factory.context.identity.project);

            // A graph retry is a new attempt, but retains the predecessor's
            // original source pin and terminal writer head. Mark continuations
            // even when both heads agree so preparation checks dirty remnants.
            let mut submission_prompt = node.prompt.to_owned();
            let mut writer_predecessor = None;
            let (mut source_head, mut successor_head) = if !accepted
                && access == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite
                && let Some(previous) = key
                    .extra
                    .get("previous_attempt_ref")
                    .and_then(serde_json::Value::as_str)
            {
                let (host, previous) = previous.split_once('/').ok_or(FacadeError::StateCorrupt)?;
                if host != self.factory.context.identity.host.to_string() {
                    return Err(FacadeError::OwnerUnavailable);
                }
                let previous = store
                    .record(
                        previous.parse().map_err(|_| FacadeError::StateCorrupt)?,
                        &call,
                    )
                    .map_err(run_error)?;
                if previous.context.as_ref().is_none_or(|context| {
                    context.graph_id.as_deref() != Some(key.graph_id.as_str())
                        || context.node_id.as_deref() != Some(key.node_id.as_str())
                }) || !matches!(
                    previous.state,
                    provenant_records::run::AttemptState::Terminal { .. }
                ) || !store
                    .custody_released(&previous, &call)
                    .map_err(run_error)?
                {
                    return Err(FacadeError::Conflict);
                }
                if matches!(
                    previous.requested_route,
                    provenant_records::run::RequestedRoute::Named { .. }
                ) && matches!(
                    previous.state,
                    provenant_records::run::AttemptState::Terminal {
                        outcome: provenant_records::run::Outcome::QuotaLimited,
                        ..
                    }
                ) {
                    submission_prompt.push_str("\n\nThe previous member stopped on quota. Inspect the retained worktree and any commits, then continue the original task from that work rather than restart. Use another eligible member of the same alias.");
                }
                let request = store.request(previous.run_id, &call).map_err(run_error)?;
                let previous_task = request
                    .tasks
                    .iter()
                    .find(|task| task.task_id == previous.task_id)
                    .ok_or(FacadeError::StateCorrupt)?;
                let writer = provenant_run::resolve_writer(
                    self.factory.context.roots.state.as_path(),
                    writer_directory,
                    Path::new(worktree.ok_or(FacadeError::StateCorrupt)?),
                )
                .map_err(run_error)?;
                if !previous_task.writer.as_ref().is_some_and(|previous| {
                    provenant_run::equivalent_writer(
                        previous,
                        &writer,
                        self.factory.context.identity.project,
                    )
                }) {
                    return Err(FacadeError::Conflict);
                }
                store
                    .check_continuation_writer(previous.attempt_id, &call)
                    .map_err(run_error)?;
                writer_predecessor = Some(previous.attempt_id);
                let original = previous_task
                    .source_head
                    .as_ref()
                    .or(previous_task.head.as_ref())
                    .cloned()
                    .or_else(|| node.source_commit.map(str::to_owned));
                let current = previous
                    .head
                    .map(|head| head.to_string())
                    .or_else(|| previous_task.head.clone());
                (original, current)
            } else {
                (None, None)
            };

            // A crash can leave an accepted operation without a checkpoint
            // reference. Preserve its immutable labels, including legacy None.
            let mut accepted_knobs = None;
            let mut accepted_route = None;
            let mut accepted_retry_limit = None;
            let mut accepted_access = None;
            let mut accepted_skill_location = None;
            let mut accepted_attempt = None;
            let mut context = match &operation {
                Some(operation) => match store.request(operation.run_id, &call) {
                    Ok(request) => {
                        let task = request.tasks.first().ok_or(FacadeError::StateCorrupt)?;
                        if accepted {
                            use provenant_run::{AttemptFilter, AttemptReader};
                            let session = owner.session(self.cancel)?;
                            let mut attempts = session
                                .store
                                .attempts(
                                    AttemptFilter {
                                        run_id: Some(operation.run_id),
                                        task_id: Some(task.task_id),
                                    },
                                    &call,
                                )
                                .map_err(run_error)?;
                            let attempt = attempts
                                .next()
                                .ok_or(FacadeError::StateCorrupt)?
                                .map_err(run_error)?;
                            let record = session
                                .store
                                .record(attempt.attempt_id, &call)
                                .map_err(run_error)?;
                            if record.prompt.bytes > provenant_records::run::RECORD_LIMIT {
                                return Err(FacadeError::StateCorrupt);
                            }
                            let mut bytes = Vec::new();
                            loop {
                                let offset = bytes.len() as u64;
                                let prompt = session
                                    .store
                                    .read_output(
                                        attempt.attempt_id,
                                        provenant_run::ArtifactPart::Prompt,
                                        provenant_run::ByteRange {
                                            offset,
                                            ..Default::default()
                                        },
                                        &session.context,
                                        &call,
                                    )
                                    .map_err(run_error)?;
                                if !prompt.complete
                                    || prompt.len != record.prompt.bytes
                                    || prompt.digest != task.prompt_digest
                                    || prompt.offset != offset
                                    || prompt.next_offset != offset + prompt.bytes.len() as u64
                                    || prompt.next_offset > record.prompt.bytes
                                    || (!prompt.eof && prompt.next_offset <= offset)
                                    || prompt.eof != (prompt.next_offset == record.prompt.bytes)
                                {
                                    return Err(FacadeError::StateCorrupt);
                                }
                                bytes.extend(prompt.bytes);
                                if prompt.eof {
                                    break;
                                }
                            }
                            submission_prompt =
                                String::from_utf8(bytes).map_err(|_| FacadeError::StateCorrupt)?;
                            source_head = task.source_head.clone();
                            successor_head = task.head.clone();
                            accepted_knobs = Some(task.profile.overrides.clone());
                            accepted_route = Some(task.route.clone());
                            accepted_retry_limit = Some(task.profile.settlement_retry_limit);
                            accepted_access = Some(task.access.clone());
                            accepted_skill_location = Some(task.profile.require_skill_location);
                            accepted_attempt = Some(provenant_fabric_runtime::graph::AttemptSpec {
                                placement: task.cloud.clone(),
                                account: task.account.clone(),
                                build_heavy: task.build_heavy,
                                confidential: task
                                    .profile
                                    .extra
                                    .get("confidential")
                                    .and_then(serde_json::Value::as_bool)
                                    .unwrap_or(false),
                                provider_timeout: Some(task.provider_timeout_seconds),
                                ..Default::default()
                            });
                        }
                        task.context.clone()
                    }
                    Err(provenant_run::RunError::NotFound)
                        if operation.state == provenant_run::OperationState::Prepared =>
                    {
                        Some(node.context)
                    }
                    Err(error) => return Err(run_error(error)),
                },
                None => Some(node.context),
            };
            if !accepted && let Some(context) = &mut context {
                context.writer_predecessor = writer_predecessor;
            }
            // Cloud jobs require an immutable commit even for the default
            // read-only task, which otherwise reads the live checkout. On
            // replay successor_head comes from the sealed request.
            let cloud = node.attempt.and_then(|spec| spec.placement.as_ref());
            let cloud_head = if !accepted && cloud.is_some() && node.source_commit.is_none() {
                Some(
                    self.factory
                        .resolve_base(&self.factory.cwd, "HEAD", &self.repo_facts, self.cancel)
                        .map_err(|_| FacadeError::DispatchRefused {
                            fact: None,
                            disk: None,
                            reason: "attempt.cloud requires a resolvable checkout HEAD".into(),
                        })?
                        .to_string(),
                )
            } else {
                None
            };
            let source_commit = successor_head
                .as_deref()
                .or(node.source_commit)
                .or(cloud_head.as_deref());
            let excluded_families = self.writer_families(node.distinct_from_attempts)?;
            // An exact route cannot be substituted, so required separation is
            // decided here, before any worktree advance or submission. An
            // accepted operation is observed as it was launched.
            if !accepted
                && let Some(model) = &route.model
                && exact_route_refused(
                    &exact_model(route.adapter.as_deref(), model),
                    &excluded_families,
                    node.require_distinct,
                )
            {
                return Ok(AttemptObservation::Terminal {
                    attempt_ref: None,
                    outcome: AttemptOutcome::AdapterUnavailable,
                    launched: false,
                    result: None,
                    result_warning: None,
                    result_path: None,
                    branch: None,
                    commit: None,
                });
            }
            // Preserve the specific detached-writer refusal in node status.
            if !accepted
                && access == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite
                && worktree.is_some()
            {
                let writer = provenant_run::resolve_writer(
                    self.factory.context.roots.state.as_path(),
                    writer_directory,
                    writer_directory,
                )
                .map_err(|error| match error {
                    provenant_run::RunError::Validation("writer needs a branch") => {
                        FacadeError::DispatchRefused {
                            fact: Some(provenant_fabric_runtime::coordination::facade::DispatchRefusalFact::DetachedWriter),
                            disk: None,
                            reason: "writer needs a branch".into(),
                        }
                    }
                    other => run_error(other),
                })?;
                if let Err(provenant_run::RunError::Validation("writer needs a branch")) =
                    provenant_run::prepare_writer_with_facts(
                        &writer,
                        self.factory.context.roots.state.as_path(),
                        self.factory.context.identity.project,
                        &self.repo_facts,
                    )
                {
                    return Err(FacadeError::DispatchRefused {
                        fact: Some(provenant_fabric_runtime::coordination::facade::DispatchRefusalFact::DetachedWriter),
                        disk: None,
                        reason: "writer needs a branch".into(),
                    });
                }
            }
            if !accepted
                && access == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite
                && matches!(
                    replay.delivery,
                    Some(provenant_records::run::DeliveryMode::Integrate(_))
                )
                && let (Some(base), Some(commit)) = (
                    self.integration_base.and_then(|bases| bases.integration),
                    source_commit,
                )
            {
                let head = commit
                    .parse()
                    .map_err(|_| FacadeError::InvalidInput { detail: None })?;
                let descends = provenant_confinement::worktrees::with_context_using_facts(
                    &self.factory.context.roots,
                    &self.factory.home,
                    &self.factory.cwd,
                    &self.factory.clock,
                    self.cancel,
                    &self.repo_facts,
                    |context| {
                        provenant_confinement::worktrees::is_ancestor(
                            context,
                            base,
                            head,
                            self.cancel,
                        )
                    },
                )
                .map_err(|error| {
                    let diagnostic = matches!(
                        error,
                        provenant_confinement::worktrees::WorktreeError::Dirty(_)
                    )
                    .then(|| {
                        provenant_records::diagnostic::Diagnostic::new("WRITER_SCOPE")
                            .category("refused")
                            .detail("the writer worktree has uncommitted edits")
                            .next("commit or preserve the worktree changes before retrying")
                    });
                    let error = graph_writer_initialisation_error(commit, error);
                    match diagnostic {
                        Some(diagnostic) => self
                            .retain_graph_refusal(refusal, &error, diagnostic)
                            .err()
                            .unwrap_or(error),
                        None => error,
                    }
                })?;
                if !descends {
                    return Err(FacadeError::DispatchRefused {
                        fact: None,
                        disk: None,
                        reason: format!(
                            "node {}: lane source {head} does not descend from integration base {base}",
                            key.node_id
                        ),
                    });
                }
            }
            if !accepted
                && access == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite
                // A continuation already owns its predecessor's checkout.
                // Preparation verifies its exact head and ancestry; advancing
                // it here would refuse the uncommitted work it must retain.
                && source_head.is_none()
                && let (Some(path), Some(commit)) = (worktree, source_commit)
            {
                let head = commit
                    .parse()
                    .map_err(|_| FacadeError::InvalidInput { detail: None })?;
                self.fast_forward_writer(writer_directory, Path::new(path), head)
                    .map_err(|error| {
                        let diagnostic = matches!(
                            error,
                            provenant_confinement::worktrees::WorktreeError::Dirty(_)
                        )
                        .then(|| {
                            provenant_records::diagnostic::Diagnostic::new("WRITER_SCOPE")
                                .category("refused")
                                .detail("the writer worktree has uncommitted edits")
                                .next("commit or preserve the worktree changes before retrying")
                        });
                        let error = graph_writer_initialisation_error(commit, error);
                        match diagnostic {
                            Some(diagnostic) => self
                                .retain_graph_refusal(refusal, &error, diagnostic)
                                .err()
                                .unwrap_or(error),
                            None => error,
                        }
                    })?;
            }
            // Delivery is a writer obligation independent of the receipt preset.
            let delivery = if let Some(mode) = replay.delivery {
                let branch = if let Some(path) = worktree {
                    let writer = provenant_run::resolve_writer(
                        self.factory.context.roots.state.as_path(),
                        writer_directory,
                        Path::new(path),
                    )
                    .map_err(run_error)?;
                    provenant_run::prepare_writer_with_facts(
                        &writer,
                        self.factory.context.roots.state.as_path(),
                        self.factory.context.identity.project,
                        &self.repo_facts,
                    )
                    .map_err(run_error)?
                    .branch
                    .as_str()
                    .trim_start_matches("refs/heads/")
                    .to_owned()
                } else {
                    String::new()
                };
                // A continuing writer pushes only to the branch it continues.
                if let Some(bound) = replay.branch
                    && bound != branch
                {
                    return Err(FacadeError::DispatchRefused {
                        fact: None,
                        disk: None,
                        reason: format!(
                            "graph writer worktree is on {branch}, not the continued branch {bound}"
                        ),
                    });
                }
                provenant_run::HostDeliveryContext::destination(mode.clone(), &branch)
                    .map_err(run_error)?
                    .map(|mut request| {
                        request.fast_forward_only = replay.fast_forward_only;
                        request
                    })
            } else {
                None
            };
            let execution = provenant_fabric_runtime::graph::FileJournal::execution(
                &self.factory.context.roots.state,
                self.factory.context.identity.project,
                &key.graph_id,
            )
            .map_err(graph_journal_error)?;
            let review_subject_missing = execution.as_ref().is_some_and(|execution| {
                execution
                    .document
                    .extra
                    .get("review_subject_missing")
                    .and_then(|v| v.as_bool())
                    == Some(true)
            });
            let review_launch = node.review_launch;
            let review_base = execution
                .as_ref()
                .and_then(|execution| execution.review_base);
            // Writer separation is mandatory; sibling diversity remains a preference.
            let enforced = node.require_distinct && !excluded_families.is_empty();
            let mut avoided_families = excluded_families;
            let mut preferred_families = Vec::new();
            for family in node.family_peers {
                let target = if enforced {
                    &mut preferred_families
                } else {
                    &mut avoided_families
                };
                if !target.contains(family) {
                    target.push(family.clone());
                }
            }
            let exclude = self.route_exclusions(route, node.prior_attempts, accepted);
            let base = if accepted {
                store
                    .request(
                        operation.as_ref().ok_or(FacadeError::StateCorrupt)?.run_id,
                        &call,
                    )
                    .map_err(run_error)?
                    .tasks
                    .first()
                    .ok_or(FacadeError::StateCorrupt)?
                    .base_ref
                    .clone()
            } else if review_subject_missing {
                None
            } else if access == provenant_fabric_runtime::graph::AccessMode::ReadOnly
                && source_commit.is_some()
            {
                let base = node
                    .review_base
                    .map(str::to_owned)
                    // A review base recorded with the graph execution comes next,
                    // then the integration base.
                    .or_else(|| review_base.map(|base| base.to_string()))
                    .or_else(|| {
                        self.integration_base
                            .and_then(|bases| bases.integration)
                            .map(|base| base.to_string())
                    });
                if review_launch {
                    let directory = worktree.map_or_else(
                        || self.factory.cwd.clone(),
                        |path| self.factory.cwd.join(path),
                    );
                    Some(self.factory.review_comparison_base(
                        &directory,
                        base.as_deref(),
                        source_commit.ok_or(FacadeError::StateCorrupt)?,
                    )?)
                } else {
                    // Ordinary source readers retain their source-commit fallback.
                    base.or_else(|| source_commit.map(str::to_owned))
                }
            } else if foreign_writer {
                source_commit.map(str::to_owned)
            } else {
                source_commit.map(|head| {
                    self.integration_base
                        .and_then(|bases| bases.integration)
                        .map_or_else(|| head.to_owned(), |base| base.to_string())
                })
            };
            // Peer roots retain the exact requested route, including family
            // exclusions, rather than resolving a second, broader alias.
            let peer_root = provenant_fabric_runtime::graph::FileJournal::execution(
                &self.factory.context.roots.state,
                self.factory.context.identity.project,
                &key.graph_id,
            )
            .map_err(graph_journal_error)?
            .is_some_and(|execution| {
                execution
                    .extra
                    .get("peer_origin_context")
                    .and_then(|origin| origin["node_id"].as_str())
                    == Some(key.node_id.as_str())
            });
            if peer_root && !accepted {
                source_head = route
                    .extra
                    .get("peer_source_head")
                    .map(|value| {
                        serde_json::from_value(value.clone()).map_err(|_| FacadeError::StateCorrupt)
                    })
                    .transpose()?
                    .flatten();
            }
            let accepted_route = accepted_route.or(if peer_root {
                route
                    .extra
                    .get("peer_requested_route")
                    .map(|value| {
                        serde_json::from_value(value.clone()).map_err(|_| FacadeError::StateCorrupt)
                    })
                    .transpose()?
            } else {
                None
            });
            let input = SubmitInput::Submit {
                extra: Default::default(),
                operation_id,
                tasks: vec![SubmittedTask {
                    require_skill_location: accepted_skill_location
                        .unwrap_or(node.require_skill_location),
                    context,
                    extra: Default::default(),
                    build_heavy: node.attempt.is_some_and(|spec| spec.build_heavy),
                    base,
                    head: source_commit.map(str::to_owned),
                    source_head,
                    account: route
                        .account
                        .clone()
                        .or_else(|| node.attempt.and_then(|spec| spec.account.clone())),
                    access,
                    capabilities: None,
                    capability_sources: Default::default(),
                    worktree: (access
                        == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite)
                        .then(|| worktree.map(str::to_owned))
                        .flatten(),
                    prompt: submission_prompt,
                    title: Some(node.title.to_owned()),
                    route: accepted_route.unwrap_or(match route.selector() {
                        provenant_fabric_runtime::graph::GraphRouteSelector::Adapter(name) => {
                            provenant_records::run::RequestedRoute::Adapter {
                                extra: route
                                    .extra
                                    .iter()
                                    .filter(|(key, _)| key.as_str() == "required_family")
                                    .map(|(key, value)| (key.clone(), value.clone()))
                                    .collect(),
                                provider: route.adapter.clone().ok_or_else(|| {
                                    FacadeError::invalid(InvalidInputDetail::FieldDecode {
                                        field: "route.adapter".into(),
                                        missing: true,
                                    })
                                })?,
                                name: name.to_owned(),
                            }
                        }
                        provenant_fabric_runtime::graph::GraphRouteSelector::Model(model) => {
                            let adapter = route.adapter.as_ref().ok_or_else(|| {
                                FacadeError::invalid(InvalidInputDetail::FieldDecode {
                                    field: "route.adapter".into(),
                                    missing: true,
                                })
                            })?;
                            provenant_records::run::RequestedRoute::Model {
                                extra: route
                                    .extra
                                    .iter()
                                    .filter(|(key, _)| key.as_str() == "required_family")
                                    .map(|(key, value)| (key.clone(), value.clone()))
                                    .collect(),
                                provider: adapter.clone(),
                                model: exact_model(Some(adapter), model),
                                effort: route.effort.clone(),
                            }
                        }
                        provenant_fabric_runtime::graph::GraphRouteSelector::Alias(_)
                            if !avoided_families.is_empty() =>
                        {
                            provenant_records::run::RequestedRoute::Review {
                                extra: route
                                    .extra
                                    .iter()
                                    .filter(|(key, _)| key.as_str() == "required_family")
                                    .map(|(key, value)| (key.clone(), value.clone()))
                                    .collect(),
                                name: route.alias.clone().ok_or_else(|| {
                                    FacadeError::invalid(InvalidInputDetail::FieldDecode {
                                        field: "route.alias".into(),
                                        missing: true,
                                    })
                                })?,
                                effort: route.effort.clone(),
                                excluded_families: avoided_families,
                                distinct_families: true,
                                preferred_families,
                                require_distinct: enforced,
                            }
                        }
                        provenant_fabric_runtime::graph::GraphRouteSelector::Alias(_) => {
                            provenant_records::run::RequestedRoute::Named {
                                extra: route
                                    .extra
                                    .iter()
                                    .filter(|(key, _)| key.as_str() == "required_family")
                                    .map(|(key, value)| (key.clone(), value.clone()))
                                    .collect(),
                                name: route.alias.clone().ok_or_else(|| {
                                    FacadeError::invalid(InvalidInputDetail::FieldDecode {
                                        field: "route.alias".into(),
                                        missing: true,
                                    })
                                })?,
                                effort: route.effort.clone(),
                            }
                        }
                    }),
                    timeout_seconds: lane_timeout(
                        node.deadline_seconds,
                        self.factory.context.config.limits.graph_timeout_seconds,
                    ),
                    delivery_id: None,
                    deliver: delivery,
                    new_worktree: None,
                }],
                concurrency: Some(1),
                knobs: if let Some(knobs) = accepted_knobs {
                    knobs
                } else {
                    let mut knobs = Vec::new();
                    if let Some(scope) = node.attempt.and_then(|a| a.local_cargo.as_deref()) {
                        knobs.push(format!("lanes.local_cargo={scope}"));
                    }
                    // Isolation, supervision and receipts, from the node or its preset.
                    if let Some(spec) = node.attempt {
                        knobs.extend(spec.lane_knobs());
                    }
                    if !exclude.is_empty() {
                        knobs.push(format!(
                            "routing.graph_exclude={}",
                            serde_json::Value::from(exclude)
                        ));
                    }
                    knobs
                },
                host,
            };
            // Operation replay is actor-scoped: submit under the recorded launch actor.
            owner.actor = Some(
                provenant_core::ActorId::parse(replay.launch_actor)
                    .map_err(|_| FacadeError::StateCorrupt)?,
            );
            let mut grants = if accepted {
                accepted_access.ok_or(FacadeError::StateCorrupt)?
            } else {
                let configured = provenant_config::read_with_domains(
                    &self.factory.context.roots,
                    &provenant_cli::domains::SECTIONS,
                )
                .map_err(|_| FacadeError::InvalidInput { detail: None })?;
                let default_capabilities = configured
                    .domains
                    .lanes
                    .projects
                    .get(&self.factory.context.identity.project.to_string())
                    .map(|settings| settings.capabilities.as_slice())
                    .unwrap_or_default();
                let mut capabilities = node
                    .capabilities
                    .unwrap_or_else(|| {
                        if access == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite {
                            default_capabilities
                        } else {
                            &[]
                        }
                    })
                    .to_vec();
                capabilities.retain(|name| {
                    if name == "postgres" {
                        true
                    } else {
                        provenant_cli::run::warn_ignored_capability("node or project ", name);
                        false
                    }
                });
                if access == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite {
                    for capability in &self.capability_overrides.0 {
                        if capability != "postgres" {
                            provenant_cli::run::warn_ignored_capability("flag ", capability);
                            continue;
                        }
                        if !capabilities.contains(capability) {
                            capabilities.push(capability.clone());
                        }
                    }
                }
                capabilities.retain(|name| !self.capability_overrides.1.contains(name));
                let mut seen = std::collections::BTreeSet::new();
                capabilities.retain(|name| seen.insert(name.clone()));
                let mut grants = provenant_cli::run::lane_access(
                    &provenant_cli::Access {
                        attach: node
                            .attachments
                            .iter()
                            .map(|path| {
                                let path = Path::new(path);
                                if path.is_absolute() {
                                    path.to_owned()
                                } else {
                                    self.factory.cwd.join(path)
                                }
                            })
                            .collect(),
                        mode: Some(provenant_cli::Mode::ReadOnly),
                        worktree: (access
                            == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite)
                            .then(|| worktree.map(PathBuf::from))
                            .flatten(),
                        read_root: node
                            .read_roots
                            .iter()
                            .map(|root| {
                                let path = Path::new(root);
                                if path.is_absolute() {
                                    path.to_path_buf()
                                } else {
                                    self.factory.cwd.join(path)
                                }
                            })
                            .collect(),
                        write_root: node
                            .attempt
                            .into_iter()
                            .flat_map(|spec| &spec.write_roots)
                            .map(|root| {
                                let path = Path::new(root);
                                if path.is_absolute() {
                                    path.to_owned()
                                } else {
                                    self.factory.cwd.join(path)
                                }
                            })
                            .collect(),
                        network: self.network,
                        capability: capabilities.clone(),
                        no_capability: Vec::new(),
                    },
                    self.factory.context.roots.state.as_path(),
                    &self.factory.home,
                )
                .map_err(dispatch_refused)?;
                if !capabilities.is_empty() {
                    let sources: Vec<_> = capabilities.iter().map(|name| {
                    let inherited = node.capability_sources.and_then(|v| v.as_array()).into_iter().flatten()
                        .find(|entry| entry["name"] == *name).and_then(|entry| entry["source"].as_str());
                    serde_json::json!({"name": name, "source": if self.capability_overrides.0.contains(name) { "flag" } else { inherited.unwrap_or(if node.capabilities.is_some() { "node" } else { "project default" }) }})
                }).collect();
                    grants
                        .extra
                        .insert("capability_sources".into(), serde_json::json!(sources));
                }
                grants
            };
            if !accepted {
                grants.inputs_root = node.inputs_dir.map(Path::to_path_buf);
                if access == provenant_fabric_runtime::graph::AccessMode::ReadOnly
                    && source_commit.is_none()
                    && let Some(directory) = node.attempt.and_then(|spec| spec.directory.as_deref())
                {
                    bind_child_directory(
                        &mut grants,
                        directory,
                        self.factory.context.roots.state.as_path(),
                        &self.factory.home,
                        self.network,
                    )?;
                }
                // Every pinned reader resolves and verifies its snapshot source.
                if access == provenant_fabric_runtime::graph::AccessMode::ReadOnly
                    && source_commit.is_some()
                {
                    grants.source_root = {
                        let path = worktree.map_or(self.factory.cwd.as_path(), Path::new);
                        let source = provenant_run::resolve_reader_source(
                            self.factory.context.roots.state.as_path(),
                            &self.factory.cwd,
                            path,
                        )
                        .map_err(|_| FacadeError::DispatchRefused {
                            fact: None,
                            disk: None,
                            reason:
                                "graph source checkout is no longer registered with the project"
                                    .into(),
                        })?;
                        let directory = provenant_os::fs::Directory::open_anchor(&source)
                            .map_err(|_| FacadeError::ConfinementRefused { resource: None })?;
                        let identity = directory
                            .metadata()
                            .map_err(|_| FacadeError::ConfinementRefused { resource: None })?;
                        let source_identity = provenant_records::run::SourceIdentity {
                            device: identity.device,
                            inode: identity.inode,
                        };
                        if worktree.is_some()
                            && let Some(checkpoint) =
                                provenant_fabric_runtime::graph::FileJournal::read_checkpoint(
                                    &self.factory.context.roots.state,
                                    self.factory.context.identity.project,
                                    &key.graph_id,
                                )
                                .map_err(graph_journal_error)?
                            && !writer_source_identity_matches(
                                &checkpoint,
                                &source,
                                &source_identity,
                            )
                        {
                            return Err(FacadeError::DispatchRefused {
                                fact: None,
                                disk: None,
                                reason: "graph source checkout identity changed or is unavailable"
                                    .into(),
                            });
                        }
                        grants.source_identity = Some(source_identity);
                        grants.source_live = false;
                        // The immutable view replaces live-source grants.
                        grants.read_roots.retain(|root| !root.starts_with(&source));
                        Some(source)
                    };
                }
            }
            if !accepted
                && access == provenant_fabric_runtime::graph::AccessMode::ReadOnly
                && source_commit.is_none()
                && let Some(warning) = provenant_cli::run::read_only_checkout_warning(
                    self.factory.context.roots.state.as_path(),
                    grants.source_root.as_deref().unwrap_or(&self.factory.cwd),
                )
            {
                let _ = writeln!(std::io::stderr().lock(), "warning: {warning}");
                self.launch_warnings.push(warning);
            }
            if let Some(network) = route.extra.get("peer_network").filter(|_| peer_root) {
                grants.network = serde_json::from_value(network.clone())
                    .map_err(|_| FacadeError::StateCorrupt)?;
            }
            let peer_retry = route
                .extra
                .get("peer_retry_limit")
                .filter(|_| peer_root)
                .map(|value| {
                    serde_json::from_value(value.clone()).map_err(|_| FacadeError::StateCorrupt)
                })
                .transpose()?;
            let retry_limit =
                accepted_retry_limit.unwrap_or(peer_retry.unwrap_or(node.settlement_retries));
            let mut attempt = accepted_attempt.unwrap_or_else(|| {
                let mut attempt = node.attempt.cloned().unwrap_or_default();
                attempt.account = route.account.clone().or(attempt.account);
                attempt
            });
            // The graph checkpoint owns worktree provisioning; submission receives
            // the resolved checkout, never a second request to create it.
            attempt.new_worktree = None;
            // These fields have already been resolved into the sealed access grants.
            attempt.write_roots.clear();
            attempt.capabilities.clear();
            // Preparation runs in a detached launcher whose stderr is not
            // inherited. Warn at the graph host and retain it in the checkpoint.
            if !accepted
                && let Some(model) = route.model.as_deref()
                && let Ok(model) =
                    provenant_routing::ModelId::parse(&exact_model(route.adapter.as_deref(), model))
                && let Ok(catalogue) = provenant_routing::Catalogue::bundled()
                && catalogue.model(&model).is_none()
            {
                let warning = format!(
                    "model {} is not in the catalogue; the provider will check it",
                    model.as_str()
                );
                let _ = writeln!(std::io::stderr().lock(), "warning: {warning}");
                self.launch_warnings.push(warning);
            }
            let receipt = owner.submit_with_access(
                input,
                self.writer_launch_budget.unwrap_or(self.budget),
                self.cancel,
                grants,
                retry_limit,
                Some(attempt),
                &self.repo_facts,
                provenant_run::RefusalContext {
                    chair: refusal.chair.clone(),
                    intent_digest: Some(refusal.digest),
                },
            )?;
            if let Some(reason) = blocked(&owner)? {
                return Err(FacadeError::DispatchRefused {
                    fact: None,
                    disk: None,
                    reason,
                });
            }
            owner.actor = None;
            let target = receipt
                .targets
                .into_iter()
                .next()
                .ok_or(FacadeError::StateCorrupt)?;
            let reference = Some(format!(
                "{}/{}",
                target.host.unwrap_or(self.factory.context.identity.host),
                target.attempt
            ));
            (target, reference)
        };
        if target
            .host
            .is_none_or(|host| host == self.factory.context.identity.host)
        {
            let store = owner.store_with_facts(self.cancel, &self.repo_facts)?;
            let call = owner.call(self.cancel);
            if let Some(observation) = provenant_cli::fabric::graph_cloud::installed(
                &self.factory.context.roots,
                &store,
                target.attempt,
                node.prompt,
                reference.as_deref().ok_or(FacadeError::StateCorrupt)?,
                &call,
                &self.factory.cwd,
                std::time::Duration::from_micros(
                    self.budget
                        .remaining(self.factory.clock.mono_now())
                        .as_micros(),
                ),
                false,
            )
            .map_err(|error| cloud_refusal(&error))?
            {
                return Ok(observation);
            }
        }
        let mut target = target;
        let mut reference = reference;
        let mut attempt = owner.snapshot(target.clone(), self.budget, self.cancel)?;
        while let Some(next) = attempt.superseded_by {
            target.attempt = next;
            reference = Some(format!(
                "{}/{}",
                target.host.unwrap_or(self.factory.context.identity.host),
                next
            ));
            attempt = owner.snapshot(target.clone(), self.budget, self.cancel)?;
        }
        // A remote parent is one origin node until its receiver-owned graph
        // has drained, including children that outlive the provider process.
        if target
            .host
            .is_some_and(|host| host != self.factory.context.identity.host)
            && attempt
                .extra
                .get("peer_subtree")
                .and_then(|value| value["status"].as_str())
                .is_some_and(|status| matches!(status, "pending" | "paused"))
        {
            return Ok(AttemptObservation::Pending {
                attempt_ref: reference,
            });
        }
        use provenant_records::run::{AttemptState, Outcome};
        match attempt.state {
            AttemptState::Terminal { outcome, .. } => {
                let remote = target
                    .host
                    .is_some_and(|host| host != self.factory.context.identity.host);
                let store = owner.store_with_facts(self.cancel, &self.repo_facts)?;
                let call = owner.call(self.cancel);
                let custody = if remote {
                    attempt
                        .extra
                        .get("graph_custody_released")
                        .and_then(serde_json::Value::as_bool)
                        .ok_or(provenant_run::RunError::NotFound)
                } else {
                    store.custody_released(&attempt, &call)
                };
                match custody {
                    Ok(true) => {}
                    Ok(false) => {
                        return Ok(AttemptObservation::Pending {
                            attempt_ref: reference,
                        });
                    }
                    Err(_) => {
                        return Ok(AttemptObservation::Unknown {
                            attempt_ref: reference,
                        });
                    }
                }
                let pickup = if remote {
                    None
                } else {
                    store
                        .delivered(attempt.attempt_id, &call)
                        .map_err(run_error)?
                };
                let effective_delivery = pickup
                    .as_ref()
                    .map(|(_, receipt)| receipt)
                    .or(attempt.delivery.as_ref());
                let launched = attempt.provider_launched.unwrap_or(true);
                let preparation_cause = match &attempt.state {
                    AttemptState::Terminal { extra, .. } => {
                        extra.get("terminal_cause").cloned().and_then(|value| {
                            serde_json::from_value::<provenant_records::run::TerminalCause>(value)
                                .ok()
                        })
                    }
                    _ => None,
                };
                let (result, result_warning) = if attempt.result.is_some()
                    || outcome == Outcome::Succeeded
                {
                    let captured = if remote {
                        remote_graph_result(
                            &mut owner,
                            &target,
                            node.required_verdict,
                            self.budget,
                            self.cancel,
                        )
                        .map_err(|_| provenant_run::RunError::NotFound)
                    } else {
                        store.stream_result(attempt.attempt_id, &call, node.required_verdict)
                    };
                    match captured {
                        Ok(mut result) => {
                            if remote && let Some(artifact) = &attempt.result {
                                // Output pages prove the redacted stream. Graph evidence
                                // still identifies the peer's sealed original artifact.
                                result.digest = artifact.digest.to_string();
                                // The peer's sealed settlement handoff, carried in its
                                // status; the output pages hold only text.
                                result.handoff = peer_handoff(attempt.extra.get("graph_handoff"));
                            }
                            (Some(Box::new(result)), None)
                        }
                        Err(error) => (None, Some(graph_result_warning(&error).to_owned())),
                    }
                } else {
                    (None, None)
                };
                // An unverifiable artifact is an integrity failure regardless
                // of the provider outcome. In particular, it must not trigger
                // route fallback for authentication or quota failures.
                Ok(AttemptObservation::Terminal {
                    launched,
                    attempt_ref: reference,
                    outcome: if result_warning.is_some() {
                        AttemptOutcome::OtherFailure
                    } else if !launched
                        && outcome == Outcome::Rejected
                        && attempt.preparation_failure.is_none_or(|failure| {
                            failure == provenant_records::run::PreparationFailure::AdapterUnavailable
                        })
                        && let Some(cause) = preparation_cause
                        && !matches!(
                            cause,
                            provenant_records::run::TerminalCause::PreparationRefused
                                | provenant_records::run::TerminalCause::CommandUnavailable
                        )
                    {
                        // The fallback classification describes route policy,
                        // not the cause (for example an unregistered account).
                        AttemptOutcome::PreparationRefused(cause)
                    } else if let Some(failure) = attempt.preparation_failure {
                        match failure {
                            provenant_records::run::PreparationFailure::AuthRequired => {
                                AttemptOutcome::AuthRequired
                            }
                            provenant_records::run::PreparationFailure::UsageLimited => {
                                AttemptOutcome::UsageLimited
                            }
                            provenant_records::run::PreparationFailure::AdapterUnavailable => {
                                AttemptOutcome::AdapterUnavailable
                            }
                        }
                    } else {
                        match outcome {
                            Outcome::Succeeded
                                if effective_delivery.is_some_and(|delivery| {
                                    matches!(
                                    delivery.state,
                                    provenant_records::run::WriterDeliveryState::Failed { .. }
                                        | provenant_records::run::WriterDeliveryState::NotAttempted { .. }
                                )
                                }) =>
                            {
                                AttemptOutcome::OtherFailure
                            }
                            Outcome::Succeeded => AttemptOutcome::Succeeded,
                            Outcome::AuthRequired => AttemptOutcome::AuthRequired,
                            Outcome::RateLimited | Outcome::QuotaLimited => {
                                AttemptOutcome::UsageLimited
                            }
                            Outcome::TimedOut if matches!(
                                &attempt.state,
                                AttemptState::Terminal { extra, .. }
                                    if extra.get("terminal_cause").cloned()
                                        .and_then(|value| serde_json::from_value::<provenant_records::run::TerminalCause>(value).ok())
                                        == Some(provenant_records::run::TerminalCause::TokenBudget)
                            ) => AttemptOutcome::OtherFailure,
                            Outcome::TimedOut if node.deadline_seconds.is_some() => {
                                AttemptOutcome::DeadlineExceeded
                            }
                            Outcome::TimedOut => AttemptOutcome::OtherFailure,
                            Outcome::ModelUnavailable => AttemptOutcome::AdapterUnavailable,
                            Outcome::Rejected if !launched => AttemptOutcome::PreparationRefused(
                                preparation_cause.unwrap_or(provenant_records::run::TerminalCause::PreparationRefused),
                            ),
                            Outcome::ReviewRequired if matches!(
                                &attempt.state,
                                AttemptState::Terminal { extra, .. }
                                    if extra.get("terminal_cause") == Some(&serde_json::json!(provenant_records::run::TerminalCause::StoppedNoWork))
                            ) => AttemptOutcome::StoppedNoWork,
                            // A declared stop is one fact whether or not the
                            // writer left uncommitted edits; the graph restarts
                            // it on the retained worktree.
                            Outcome::ReviewRequired | Outcome::Uncommitted
                                if attempt.partial_signals.contains(&provenant_records::run::PartialSignal::DeclaredStop)
                                    && attempt.partial_signals.iter().all(|signal| matches!(
                                        signal,
                                        provenant_records::run::PartialSignal::DeclaredStop
                                            | provenant_records::run::PartialSignal::UncommittedWork
                                    )) =>
                            {
                                AttemptOutcome::DeclaredStop
                            }
                            Outcome::Failed => AttemptOutcome::TaskFailed,
                            Outcome::ReviewRequired => AttemptOutcome::ReviewRequired,
                            // Uncommitted edits without a declared stop need the
                            // chair, so the node fails without fallback.
                            Outcome::Uncommitted => AttemptOutcome::OtherFailure,
                            _ => AttemptOutcome::OtherFailure,
                        }
                    },
                    result,
                    result_warning,
                    result_path: attempt.result.as_ref().and_then(|_| {
                        (target.host.unwrap_or(self.factory.context.identity.host)
                            == self.factory.context.identity.host)
                            .then(|| {
                                self.factory
                                    .context
                                    .roots
                                    .state
                                    .as_path()
                                    .join(format!(
                                        "runs/{}/attempts/{}/result.md",
                                        attempt.run_id, attempt.attempt_id
                                    ))
                                    .to_string_lossy()
                                    .into_owned()
                            })
                    }),
                    branch: effective_delivery.map(|receipt| receipt.branch.clone()),
                    commit: if access == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite
                    {
                        attempt.head.map(|head| head.to_string())
                    } else {
                        node.source_commit.map(str::to_owned)
                    },
                })
            }
            _ => Ok(AttemptObservation::Pending {
                attempt_ref: reference,
            }),
        }
    }
}
/// Resolve the existing routing allowance, retaining explicit zero overrides.
pub(crate) fn graph_retry_allowance(
    node: &provenant_records::ExtraFields,
    graph: &provenant_records::ExtraFields,
    roots: &provenant_core::Roots,
) -> (u8, String) {
    let mut warnings = Vec::new();
    let mut read = |extra: &provenant_records::ExtraFields, key: &str| -> Option<u8> {
        let retry = extra.get(key)?;
        let value = retry
            .get("retries")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u8::try_from(value).ok())
            .filter(|value| *value <= provenant_routing::MAX_SETTLEMENT_RETRIES);
        if value.is_none() {
            warnings.push(format!("ignored invalid {key}.retries"));
        }
        value
    };
    let node_retry = read(node, "retry");
    let preset_retry = read(node, "preset_retry").or_else(|| read(graph, "retry"));
    let host = match provenant_config::read_with_domains(roots, &provenant_cli::domains::SECTIONS) {
        Ok(config) => matches!(
            config.origins.get("domains.routing.settlement_retries"),
            Some(provenant_config::Origin::User { .. })
        )
        .then_some(config.domains.routing.settlement_retries),
        Err(_) => {
            warnings.push("host retry setting unavailable".into());
            None
        }
    };
    let allowance = provenant_routing::resolve_retry_allowance(node_retry, preset_retry, host);
    let origin = match allowance.origin {
        provenant_routing::RetryAllowanceOrigin::Node => "node",
        provenant_routing::RetryAllowanceOrigin::Preset => "preset",
        provenant_routing::RetryAllowanceOrigin::HostConfig => "host_config",
        provenant_routing::RetryAllowanceOrigin::ShippedDefault => "shipped_default",
    };
    let origin = if warnings.is_empty() {
        origin.to_owned()
    } else {
        format!("{origin}; {}", warnings.join("; "))
    };
    (allowance.retries, origin)
}

/// A graph document's optional top-level `exclude` list of route-exclusion
/// selectors, inherited by every node it launches.
pub(crate) fn graph_exclude(
    document: &provenant_fabric_runtime::graph::GraphDocument,
) -> Result<Vec<String>, FacadeError> {
    match document.extra.get("exclude") {
        None => Ok(Vec::new()),
        Some(value) => value
            .as_array()
            .ok_or(FacadeError::InvalidInput { detail: None })?
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or(FacadeError::InvalidInput { detail: None })
            })
            .collect(),
    }
}
/// A graph document's optional top-level `network` (`"on"` or `"off"`),
/// applied to every node lane it launches.
pub(crate) fn graph_network(
    document: &provenant_fabric_runtime::graph::GraphDocument,
) -> Result<Option<provenant_cli::Switch>, FacadeError> {
    match document.extra.get("network") {
        None => Ok(None),
        Some(value) => match value.as_str() {
            Some("on") => Ok(Some(provenant_cli::Switch::On)),
            Some("off") => Ok(Some(provenant_cli::Switch::Off)),
            _ => Err(FacadeError::InvalidInput { detail: None }),
        },
    }
}

pub(crate) fn graph_capability_overrides(
    document: &provenant_fabric_runtime::graph::GraphDocument,
) -> Result<(Vec<String>, Vec<String>), FacadeError> {
    let parse = |key| {
        document.extra.get(key).map_or(Ok(Vec::new()), |value| {
            serde_json::from_value(value.clone())
                .map_err(|_| FacadeError::InvalidInput { detail: None })
        })
    };
    Ok((parse("capability")?, parse("no_capability")?))
}

/// A graph route's exact model in `ADAPTER/ID` form.
pub(crate) fn exact_model(adapter: Option<&str>, model: &str) -> String {
    match adapter {
        Some(adapter) if !model.contains('/') => format!("{adapter}/{model}"),
        _ => model.to_owned(),
    }
}

/// Whether required family separation refuses an exact route before launch.
/// As for routed reviews, a model whose family is unknown is treated as
/// excluded once any family is.
pub(crate) fn exact_route_refused(
    model: &str,
    excluded_families: &[String],
    require_distinct: bool,
) -> bool {
    require_distinct
        && !excluded_families.is_empty()
        && provenant_cli::run::model_family(model)
            .is_none_or(|family| excluded_families.contains(&family))
}

pub(crate) fn restore_scratch_source(
    directory: &Path,
    name: &std::ffi::OsStr,
    bytes: &[u8],
) -> Result<(), FacadeError> {
    let directory = provenant_os::fs::Directory::open_anchor(directory)
        .map_err(|_| FacadeError::io("access graph scratch"))?;
    match directory.metadata_at(name, false) {
        Ok(_) => Ok(()),
        Err(error) if error.is_not_found() => {
            match directory.atomic_write(
                name,
                ".graph-recover.",
                bytes,
                provenant_os::fs::PublishMode::NoReplace,
            ) {
                Ok(publication) => publication
                    .require_durable()
                    .map_err(|_| FacadeError::io("restore graph scratch")),
                Err(error) if error.is_exists() => Ok(()),
                Err(_) => Err(FacadeError::io("restore graph scratch")),
            }
        }
        Err(_) => Err(FacadeError::io("access graph scratch")),
    }
}

/// Map a cloud submission or observation error to the facade refusal.
fn cloud_refusal(error: &str) -> FacadeError {
    FacadeError::DispatchRefused {
        fact: None,
        disk: None,
        reason: format!(
            "cloud submission or observation failed: {}",
            provenant_confinement::redact::bounded_line(error, 600)
        ),
    }
}

#[cfg(test)]
mod cloud_refusal_tests {
    #[allow(
        unused_imports,
        reason = "the custom worktree driver includes this module without libtest"
    )]
    use super::*;

    #[test]
    fn cloud_error_maps_to_a_redacted_bounded_dispatch_refusal() {
        let secret = format!("sk-{}", "a1b2c3d4".repeat(6));
        let error = format!("submit failed with {secret} {}", "x".repeat(2000));
        let FacadeError::DispatchRefused { fact, disk, reason } = cloud_refusal(&error) else {
            panic!("expected a dispatch refusal");
        };
        assert!(fact.is_none() && disk.is_none());
        assert!(reason.starts_with("cloud submission or observation failed: submit failed"));
        assert!(!reason.contains(&secret));
        assert!(reason.len() < 700);
    }
}

#[cfg(test)]
mod journal_error_tests {
    #[test]
    fn graph_review_uses_merge_base_of_advanced_named_ref() -> Result<(), Box<dyn std::error::Error>>
    {
        if super::super::peer_tests::host_test(
            "fabric::owner::journal_error_tests::graph_review_uses_merge_base_of_advanced_named_ref",
        )? {
            return Ok(());
        }
        for scenario in ["node-base", "graph-base", "behaviour"] {
            review_launch_fixture(scenario)?;
        }
        Ok(())
    }

    #[test]
    fn empty_graph_review_range_does_not_launch_a_lens() -> Result<(), Box<dyn std::error::Error>> {
        if super::super::peer_tests::host_test(
            "fabric::owner::journal_error_tests::empty_graph_review_range_does_not_launch_a_lens",
        )? {
            return Ok(());
        }
        for scenario in ["same-head", "ancestor", "no-base"] {
            review_launch_fixture(scenario)?;
        }
        Ok(())
    }

    #[test]
    fn repaired_subject_review_launches_with_integration_base()
    -> Result<(), Box<dyn std::error::Error>> {
        if super::super::peer_tests::host_test(
            "fabric::owner::journal_error_tests::repaired_subject_review_launches_with_integration_base",
        )? {
            return Ok(());
        }
        review_launch_fixture("repair")
    }

    #[test]
    fn repair_loop_composite_review_uses_merge_base_and_refuses_empty_range()
    -> Result<(), Box<dyn std::error::Error>> {
        if super::super::peer_tests::host_test(
            "fabric::owner::journal_error_tests::repair_loop_composite_review_uses_merge_base_and_refuses_empty_range",
        )? {
            return Ok(());
        }
        for scenario in ["expanded", "expanded-empty"] {
            review_launch_fixture(scenario)?;
        }
        Ok(())
    }

    #[allow(dead_code)] // The custom worktree driver includes this module without libtest.
    fn review_launch_fixture(scenario: &str) -> Result<(), Box<dyn std::error::Error>> {
        use super::*;
        use provenant_fabric_runtime::graph::{self, GraphJournal};
        struct Free;
        impl WorkerObserver for Free {
            fn observe_roots(
                &self,
                roots: &[PathBuf],
            ) -> std::collections::BTreeMap<PathBuf, provenant_confinement::worktrees::Occupancy>
            {
                roots
                    .iter()
                    .cloned()
                    .map(|root| (root, provenant_confinement::worktrees::Occupancy::Available))
                    .collect()
            }
        }
        let rig = crate::worktree_tests::repository()?;
        let fork = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
        crate::worktree_tests::git(&rig, &["checkout", "-b", "review-subject"])?;
        crate::worktree_tests::git(&rig, &["commit", "--allow-empty", "-m", "subject"])?;
        if scenario == "repair" {
            crate::worktree_tests::git(&rig, &["commit", "--allow-empty", "-m", "repair subject"])?;
        }
        let subject = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
        let target_start = if matches!(scenario, "ancestor" | "expanded-empty") {
            &subject
        } else {
            &fork
        };
        crate::worktree_tests::git(&rig, &["checkout", "-b", "review-target", target_start])?;
        crate::worktree_tests::git(&rig, &["commit", "--allow-empty", "-m", "advanced target"])?;
        let tip = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
        let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
        let factory = Factory {
            context: Context::open(roots, &rig.home, &rig.project)?,
            project_attribution: false,
            clock: SystemClock,
            backoff: ThreadPause,
            executor: SystemExecutor::new(1)?,
            home: rig.home.clone(),
            cwd: rig.project.clone(),
            assets: rig.project.clone(),
            host_launch: None,
            mcp_project: None,
            workers: &Free,
        };
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let mut document = serde_json::json!({
            "schema_version":2,"graph_id":format!("review-launch-{scenario}"),
            "nodes":[{"id":"review","role":"lens.correctness-spec","prompt":"Review subject",
                "access":"read_only","route":{"alias":"senior"},"attempt":{"preset":"light"},
                "source":{"type":"commit","commit":subject}}]
        });
        let graph_base = scenario == "graph-base" || scenario == "behaviour";
        if graph_base {
            document["review_base"] = serde_json::json!("review-target");
        } else if !matches!(scenario, "repair" | "no-base") {
            document["nodes"][0]["review_base"] = serde_json::json!(if scenario == "same-head" {
                subject.as_str()
            } else {
                "review-target"
            });
        }
        if scenario == "behaviour" {
            document["nodes"][0]
                .as_object_mut()
                .ok_or("node")?
                .remove("role");
            document["nodes"][0]["review_behaviour"] = serde_json::json!("lens");
        }
        if scenario.starts_with("expanded") {
            let review = document["nodes"][0].clone();
            document["nodes"] = serde_json::json!([{
                "id":"repair", "kind":"loop", "access":"read_only",
                "loop":{"review_repair":true,"max_rounds":1,"until":"accepted","body":[
                    {"id":"panel","kind":"composite","access":"read_only",
                        "composite":{"nodes":[review],
                            "exports":[{"node":"review","outputs":["verdict"]}]}},
                    {"id":"accepted","kind":"gate","access":"read_only",
                        "depends_on":["panel"],"inputs":[{"node":"panel","outputs":["verdict"]}],
                        "condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"}}
                ]}
            }]);
        }
        let document: graph::GraphDocument = serde_json::from_value(document)?;
        let integration = (scenario == "repair").then(|| fork.parse()).transpose()?;
        let bound = BoundGraphBases {
            integration,
            review: graph_base.then(|| tip.parse()).transpose()?,
        };
        // Real admission performs the upgrade preflight before binding a pending graph.
        crate::install::cutover_pending_guard(&factory.context.roots.state)?;
        let mut journal = graph::FileJournal::open(
            &factory.context.roots.state,
            factory.context.identity.project,
            &document.graph_id,
            budget,
            &cancel,
            &factory.clock,
        )?;
        journal.bind(graph::GraphExecution {
            review_base: bound.review,
            integration_base: integration,
            directory: factory.cwd.clone(),
            extra: Default::default(),
            document: document.clone(),
        })?;
        let mut owner = BoundGraphOwner {
            writer_launch_budget: None,
            admission_timeout: None,
            factory: &factory,
            repo_facts: Default::default(),
            launch_warnings: Vec::new(),
            integration_base: Some(bound),
            integration_branch: None,
            network: None,
            capability_overrides: Default::default(),
            exclude: Vec::new(),
            budget,
            cancel: &cancel,
            prices: None,
            tick: false,
        };
        let result = provenant_cli::run::with_deferred_provider_launch(|| {
            graph::advance_with_budget(
                &document,
                &mut owner,
                &mut journal,
                &factory.context.config.graph.limits,
                &factory.context.config.graph.budget,
            )
        });
        let checkpoint = journal.load(&document.graph_id)?.ok_or("checkpoint")?;
        let review = checkpoint
            .nodes
            .iter()
            .find(|node| node.id == "review" || node.id == "repair--r1--panel--review")
            .ok_or("review checkpoint")?;
        if matches!(
            scenario,
            "same-head" | "ancestor" | "no-base" | "expanded-empty"
        ) {
            // Equal bound inputs fail in the scheduler; derived empty ranges are
            // invalid-input refusals from the launch owner.
            if scenario == "same-head" {
                assert_eq!(
                    review.advance_error_code.as_deref(),
                    Some("review_empty_range"),
                    "{scenario}: {result:?} {checkpoint:?}"
                );
            } else {
                assert_eq!(
                    review.advance_error_code.as_deref(),
                    Some("launch_refused"),
                    "{scenario}: {result:?} {checkpoint:?}"
                );
                assert_eq!(
                    review.advance_owner_code.as_deref(),
                    Some("invalid_input"),
                    "{scenario}: {result:?} {checkpoint:?}"
                );
            }
            assert!(
                review
                    .attempts
                    .iter()
                    .all(|attempt| attempt.attempt_ref.is_none()),
                "{scenario}: lens launched"
            );
            return Ok(());
        }
        let run = result?;
        let reference = review
            .attempts
            .iter()
            .find_map(|attempt| attempt.attempt_ref.as_deref())
            .ok_or_else(|| format!("no sealed review: {run:?}"))?;
        let (_, id) = reference.split_once('/').ok_or("attempt reference")?;
        let binary = BinaryRunOwner {
            factory: &factory,
            allow_remote: false,
            actor: None,
            chair: None,
        };
        let store = binary.store(&cancel)?;
        let call = binary.call(&cancel);
        let record = store.record(id.parse()?, &call)?;
        let request = store.request(record.run_id, &call)?;
        let task = request.tasks.first().ok_or("review task")?;
        assert_eq!(task.base_ref.as_deref(), Some(fork.as_str()), "{scenario}");
        assert_eq!(task.head.as_deref(), Some(subject.as_str()), "{scenario}");
        // The checkpoint keeps the bound ref tip, never the derived comparison base.
        if !graph_base && scenario != "repair" {
            assert_eq!(review.review_base.as_deref(), Some(tip.as_str()));
        }
        Ok(())
    }

    #[test]
    fn child_directory_preserves_scratch_and_refuses_state_and_credentials()
    -> Result<(), Box<dyn std::error::Error>> {
        use super::*;
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        let state = root.path().join("state");
        let scratch = root.path().join("scratch");
        let credentials = home.join(".codex");
        for path in [&home, &state, &scratch, &credentials] {
            std::fs::create_dir_all(path)?;
        }
        let mut grants = provenant_records::run::LaneAccess::default();
        bind_child_directory(
            &mut grants,
            scratch.to_str().ok_or("scratch")?,
            &state,
            &home,
            None,
        )?;
        assert_eq!(
            grants.source_root,
            Some(provenant_os::fs::canonicalize(&scratch)?)
        );
        assert!(grants.source_live);
        for path in [&state, &credentials] {
            assert!(
                bind_child_directory(
                    &mut grants,
                    path.to_str().ok_or("root")?,
                    &state,
                    &home,
                    None
                )
                .is_err()
            );
        }
        Ok(())
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // Real registry contention across two launch stages.
    fn provisioned_writer_fast_forward_leaves_sibling_registry_mutation_alone()
    -> Result<(), Box<dyn std::error::Error>> {
        use super::*;
        if super::super::peer_tests::host_test(
            "fabric::owner::journal_error_tests::provisioned_writer_fast_forward_leaves_sibling_registry_mutation_alone",
        )? {
            return Ok(());
        }
        use provenant_fabric_runtime::graph::{AccessMode, AttemptOwner};
        use provenant_os::fs::{Directory, LockMode};
        let rig = crate::worktree_tests::repository()?;
        crate::worktree_tests::git(&rig, &["commit", "--allow-empty", "-m", "base"])?;
        let source = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
        let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
        let mut factory = Factory {
            context: Context::open(roots, &rig.home, &rig.project)?,
            project_attribution: false,
            clock: SystemClock,
            backoff: ThreadPause,
            executor: SystemExecutor::new(1)?,
            home: rig.home.clone(),
            cwd: rig.project.clone(),
            assets: rig.project.clone(),
            host_launch: None,
            mcp_project: None,
            workers: &provenant_confinement::worktrees::SystemWorkerObserver,
        };
        factory.context.config.graph.writer_launch_seconds = 12;
        let cancel = CancelToken::new();
        let settlement = WaitBudget::cli(factory.clock.mono_now(), 1)?;
        let mut owner = BoundGraphOwner {
            writer_launch_budget: None,
            admission_timeout: Some(settlement),
            factory: &factory,
            repo_facts: Default::default(),
            launch_warnings: Vec::new(),
            integration_base: None,
            integration_branch: None,
            network: None,
            capability_overrides: Default::default(),
            exclude: Vec::new(),
            budget: settlement,
            cancel: &cancel,
            prices: None,
            tick: false,
        };
        let deadline = owner
            .begin_launch(AccessMode::WorktreeWrite, None)?
            .ok_or("deadline")?;
        owner.provision_writer_worktree(".worktrees/later", "later", source.trim())?;
        let budget = owner.writer_launch_budget.ok_or("budget")?;
        let directory = Directory::open_anchor(&rig.project.join(".git"))?;
        let held = directory.lock(
            std::ffi::OsStr::new(provenant_confinement::worktrees::REPOSITORY_LOCK),
            LockMode::Exclusive,
            WaitBudget::nonblocking(factory.clock.mono_now()),
            &cancel,
            &factory.clock,
        )?;
        owner.fast_forward_writer(
            &rig.project,
            &rig.project.join(".worktrees/later"),
            source.trim().parse()?,
        )?;
        assert_eq!(
            owner.writer_launch_budget.ok_or("budget")?.deadline(),
            budget.deadline()
        );
        assert_eq!(
            owner.begin_launch(AccessMode::WorktreeWrite, Some(deadline))?,
            Some(deadline)
        );
        drop(held);
        assert_eq!(
            crate::worktree_tests::git(&rig, &["-C", ".worktrees/later", "rev-parse", "HEAD"])?
                .trim(),
            source.trim()
        );
        Ok(())
    }

    #[test]
    fn graph_refusal_replays_before_preparation_and_rejects_changed_intent()
    -> Result<(), Box<dyn std::error::Error>> {
        use super::*;
        if super::super::peer_tests::host_test(
            "fabric::owner::journal_error_tests::graph_refusal_replays_before_preparation_and_rejects_changed_intent",
        )? {
            return Ok(());
        }
        use provenant_fabric_runtime::graph::{self, AttemptOwner};
        let rig = crate::worktree_tests::repository()?;
        let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
        let factory = Factory {
            context: Context::open(roots, &rig.home, &rig.project)?,
            project_attribution: false,
            clock: SystemClock,
            backoff: ThreadPause,
            executor: SystemExecutor::new(1)?,
            home: rig.home.clone(),
            cwd: rig.project.clone(),
            assets: rig.project.clone(),
            host_launch: None,
            mcp_project: None,
            workers: &provenant_confinement::worktrees::SystemWorkerObserver,
        };
        let cancel = CancelToken::new();
        let mut owner = BoundGraphOwner {
            writer_launch_budget: None,
            admission_timeout: None,
            factory: &factory,
            repo_facts: Default::default(),
            launch_warnings: Vec::new(),
            integration_base: None,
            integration_branch: None,
            network: None,
            capability_overrides: Default::default(),
            exclude: Vec::new(),
            budget: WaitBudget::cli(factory.clock.mono_now(), 30)?,
            cancel: &cancel,
            prices: None,
            tick: false,
        };
        let key = graph::AttemptKey {
            extra: Default::default(),
            graph_id: "preflight-replay".into(),
            node_id: "writer".into(),
            route_index: 0,
        };
        let node = |prompt| graph::NodeInput {
            review_launch: false,
            require_skill_location: false,
            settlement_retries: Some(1),
            prior_attempts: &[],
            family_peers: &[],
            context: provenant_records::run::WorkContext {
                graph_id: Some(key.graph_id.clone()),
                node_id: Some(key.node_id.clone()),
                ..Default::default()
            },
            read_roots: &[],
            attachments: &[],
            capabilities: None,
            capability_sources: None,
            attempt: None,
            distinct_from_attempts: &[],
            require_distinct: false,
            source_commit: None,
            review_base: None,
            title: "writer",
            prompt,
            required_verdict: None,
            deadline_seconds: None,
            inputs_dir: None,
        };
        let route: graph::GraphRoute =
            serde_json::from_value(serde_json::json!({"alias":"senior"}))?;
        let actor = owner.actor();
        let replay = graph::Replay {
            delivery: None,
            branch: None,
            fast_forward_only: false,
            stored_ref: None,
            launch_actor: &actor,
        };
        let access = graph::AccessMode::WorktreeWrite;
        let scope = GraphRefusalScope::new(
            &owner,
            &key,
            &node("PROMPT_SENTINEL"),
            &route,
            access,
            Some("/HOST_PATH_SENTINEL/missing"),
            replay,
        )?;
        let original = FacadeError::DispatchRefused {
            fact: None,
            disk: None,
            reason: "/HOST_PATH_SENTINEL uncommitted edits".into(),
        };
        owner.retain_graph_refusal(
            &scope,
            &original,
            provenant_records::diagnostic::Diagnostic::new("WRITER_SCOPE")
                .detail("the writer worktree has uncommitted edits")
                .next("preserve the edits"),
        )?;
        let store = owner.refusal_store(&scope)?;
        let call = provenant_run::Request {
            clock: &factory.clock,
            cancel: &cancel,
        };
        let first = store.operation(&scope.operation, &call)?.ok_or("refusal")?;
        let serialised = serde_json::to_string(&first)?;
        assert!(!serialised.contains("HOST_PATH_SENTINEL"));
        assert!(!serialised.contains("PROMPT_SENTINEL"));
        assert!(serialised.contains("the writer worktree has uncommitted edits"));
        // This worktree cannot be prepared. The stored dispatch refusal must
        // win without consulting the filesystem or looking for an admitted run.
        let repeated = owner
            .observe_or_start(
                &key,
                node("PROMPT_SENTINEL"),
                &route,
                access,
                Some("/HOST_PATH_SENTINEL/missing"),
                replay,
            )
            .unwrap_err();
        assert_eq!(repeated.code(), original.code());
        assert!(
            repeated
                .to_string()
                .contains("the writer worktree has uncommitted edits")
        );
        assert_eq!(
            store
                .operation(&scope.operation, &call)?
                .ok_or("refusal")?
                .run_id,
            first.run_id
        );
        let changed = owner
            .observe_or_start(
                &key,
                node("changed"),
                &route,
                access,
                Some("/HOST_PATH_SENTINEL/missing"),
                replay,
            )
            .unwrap_err();
        assert_eq!(changed.code(), "operation_conflict");
        let missing_key = graph::AttemptKey {
            route_index: 2,
            ..key.clone()
        };
        let mut missing = first.clone();
        missing.operation_id = graph_operation("graph-attempt-operation", &missing_key)?;
        missing.run_id = provenant_core::RunId::from_bytes([31; 16]);
        missing.extra.remove("origin_intent_digest");
        store.insert_refusal(missing, &call)?;
        let missing = owner
            .observe_or_start(
                &missing_key,
                node("PROMPT_SENTINEL"),
                &route,
                access,
                Some("/HOST_PATH_SENTINEL/missing"),
                replay,
            )
            .unwrap_err();
        assert_eq!(missing.code(), "state_corrupt");
        // Exercise the other record writer too: adapter capability checks
        // refuse inside RunService, after the graph owner has built its intent.
        let service_key = graph::AttemptKey {
            route_index: 1,
            ..key.clone()
        };
        let unsupported: graph::GraphRoute = serde_json::from_value(serde_json::json!({
            "adapter":"cursor", "model":"cursor/m", "effort":"high"
        }))?;
        let service_refusal = owner
            .observe_or_start(
                &service_key,
                node("service preflight"),
                &unsupported,
                graph::AccessMode::ReadOnly,
                None,
                replay,
            )
            .unwrap_err();
        let operation = graph_operation("graph-attempt-operation", &service_key)?;
        let saved = store
            .operation(&operation, &call)?
            .ok_or("service refusal")?;
        assert_eq!(
            saved.extra.get("preflight_digest_version"),
            Some(&serde_json::json!(1))
        );
        assert!(saved.extra.contains_key("origin_intent_digest"));
        let repeated = owner
            .observe_or_start(
                &service_key,
                node("service preflight"),
                &unsupported,
                graph::AccessMode::ReadOnly,
                None,
                replay,
            )
            .unwrap_err();
        assert_eq!(repeated.code(), service_refusal.code());
        let changed = owner
            .observe_or_start(
                &service_key,
                node("changed service input"),
                &unsupported,
                graph::AccessMode::ReadOnly,
                None,
                replay,
            )
            .unwrap_err();
        assert_eq!(changed.code(), "operation_conflict");
        let attempt: graph::GraphAttempt = serde_json::from_value(serde_json::json!({
            "launched":false,"key":key,"route":route,"state":"starting","attempt_ref":null,
            "outcome":null,"launch_actor":actor,"settled":0
        }))?;
        assert!(!owner.launch_accepted(&attempt)?);
        assert!(owner.launch_refused_before_acceptance(&attempt)?);
        assert!(owner.cancelled_attempt_settled(&attempt)?);
        assert!(!store.has_runs(&call)?);
        Ok(())
    }

    #[test]
    fn repair_scope_comparison_detects_new_paths() {
        let review = provenant_routing::ReviewConfig::default();
        let original = vec!["src/a.rs".to_owned()];
        assert!(!super::repair_scope_grew(&review, &original, &original));
        assert!(super::repair_scope_grew(
            &review,
            &original,
            &["src/a.rs".into(), "src/b.rs".into()]
        ));
        assert!(super::repair_scope_grew(
            &review,
            &original,
            &["src/b.rs".into()]
        ));
        assert!(!super::repair_scope_grew(&review, &original, &[]));
    }

    #[test]
    fn repair_detects_risk_inside_already_reviewed_paths() {
        let review = provenant_routing::ReviewConfig::default();
        let paths = vec!["src/credentials.rs".to_owned()];
        assert!(!super::repair_scope_grew(&review, &paths, &paths));
        assert!(super::repair_touches_risk(&review, &paths));
        assert!(!super::repair_touches_risk(&review, &[]));
        assert!(!super::repair_touches_risk(&review, &["README.md".into()]));
    }

    #[test]
    fn unreadable_graph_journal_preserves_the_cause() {
        use super::{FacadeError, graph_journal_error};
        use provenant_fabric_runtime::graph::JournalError;
        for error in [
            JournalError::BadEnvelope,
            JournalError::BadDigest,
            JournalError::RetiredLoopCheckpoint,
            JournalError::InvalidTopology,
        ] {
            let reason = error.to_string();
            assert_eq!(
                graph_journal_error(error),
                FacadeError::DispatchRefused {
                    fact: None,
                    disk: None,
                    reason
                },
            );
        }
        assert_eq!(
            graph_journal_error(JournalError::GraphMismatch),
            FacadeError::StateCorrupt
        );
    }
}

/// Stream the result through the existing authenticated, bounded output API.
/// The serving host verifies each artifact read; capture remains on the chair.
/// A peer's handoff is untrusted at this boundary: validate it as any decoded
/// handoff, then redact it, before a continuation prompt can carry it.
/// An absent key or an explicit null means no handoff.
fn peer_handoff(
    sealed: Option<&serde_json::Value>,
) -> Result<Option<provenant_records::handoff::Handoff>, &'static str> {
    let handoff = match sealed {
        None | Some(serde_json::Value::Null) => return Ok(None),
        Some(value) => {
            let bytes = serde_json::to_vec(value).map_err(|_| "peer handoff unreadable")?;
            provenant_records::handoff::Handoff::from_json(&bytes)
                .map_err(|_| "peer handoff unreadable")?
        }
    };
    provenant_run::sanitise_handoff(&handoff)
        .map(Some)
        .map_err(|_| "peer handoff unreadable")
}

fn remote_graph_result(
    owner: &mut BinaryRunOwner<'_>,
    target: &AttemptTarget,
    required: Option<&str>,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<provenant_records::result_capture::CapturedResult, FacadeError> {
    capture_graph_output(target, required, |offset| {
        owner.output(
            OutputInput::Read {
                extra: Default::default(),
                target: target.clone(),
                part: OutputPart::Result,
                offset,
                max_bytes: provenant_records::run::OUTPUT_LIMIT,
            },
            budget,
            cancel,
        )
    })
}

fn capture_graph_output(
    target: &AttemptTarget,
    required: Option<&str>,
    mut read: impl FnMut(u64) -> Result<OutputResult, FacadeError>,
) -> Result<provenant_records::result_capture::CapturedResult, FacadeError> {
    let mut capture = provenant_records::result_capture::ResultCapture::new(required);
    let mut offset = 0;
    let mut expected_digest = None;
    loop {
        let response = read(offset)?;
        let OutputResult::Chunk {
            target: observed,
            part: OutputPart::Result,
            offset: observed_offset,
            next_offset,
            eof,
            digest,
            bytes,
            ..
        } = response
        else {
            return Err(FacadeError::StateCorrupt);
        };
        if observed != *target
            || observed_offset != offset
            || next_offset < offset
            || next_offset > provenant_records::run::RESULT_LIMIT
            || expected_digest.is_some_and(|expected| expected != digest)
        {
            return Err(FacadeError::StateCorrupt);
        }
        expected_digest = Some(digest);
        let bytes = match bytes {
            OutputBytes::Utf8(text) => text.into_bytes(),
            OutputBytes::Base64(encoded) => {
                provenant_fabric_runtime::handoff::decode_base64(&encoded)
                    .ok_or(FacadeError::StateCorrupt)?
            }
        };
        if next_offset - offset != bytes.len() as u64 || (!eof && next_offset == offset) {
            return Err(FacadeError::StateCorrupt);
        }
        capture.push(&bytes);
        offset = next_offset;
        if eof {
            let result = capture.finish();
            if result.digest != digest.to_string() {
                return Err(FacadeError::StateCorrupt);
            }
            return Ok(result);
        }
    }
}

#[cfg(test)]
mod peer_handoff_tests {
    #[allow(
        unused_imports,
        reason = "the custom worktree driver includes this module without libtest"
    )]
    use super::*;
    #[test]
    fn peer_handoff_is_validated_and_redacted_before_it_reaches_the_capture() {
        let sealed = |note: &str| {
            serde_json::json!({"schema_version":2,"status":"done","note":note,
                "remaining":[],"decisions":[],"verify":[]})
        };
        let token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let clean = peer_handoff(Some(&sealed(&format!("key {token}"))));
        let note = clean.unwrap().unwrap().note;
        assert!(!note.contains(token));
        let mut bad = sealed("x");
        bad["schema_version"] = 1.into();
        assert!(peer_handoff(Some(&bad)).is_err());
        assert!(peer_handoff(Some(&sealed(" "))).is_err());
        assert!(peer_handoff(Some(&sealed(&"n".repeat(5000)))).is_err());
        assert!(
            peer_handoff(Some(&serde_json::Value::Null))
                .unwrap()
                .is_none()
        );
        assert!(peer_handoff(None).unwrap().is_none());
    }
}

#[cfg(test)]
mod peer_capture_tests {
    #[allow(
        unused_imports,
        reason = "the custom worktree driver includes this module without libtest"
    )]
    use super::*;
    #[test]
    fn peer_result_capture_pages_the_complete_stream_and_checks_each_chunk()
    -> Result<(), Box<dyn std::error::Error>> {
        let target = AttemptTarget {
            extra: Default::default(),
            host: Some(provenant_core::HostId::from_bytes([1; 16])),
            attempt: provenant_core::AttemptId::from_bytes([2; 16]),
        };
        let text = "x".repeat(130_000) + "\nVERDICT: PASS\n";
        let digest = provenant_core::Digest::from_bytes(
            *provenant_records::CanonicalV1::file_digest(text.as_bytes()).bytes(),
        );
        let chunk = |offset: u64| {
            let end = (offset + provenant_records::run::OUTPUT_LIMIT).min(text.len() as u64);
            OutputResult::Chunk {
                extra: Default::default(),
                target: target.clone(),
                part: OutputPart::Result,
                state: "succeeded".into(),
                reason: None,
                offset,
                next_offset: end,
                eof: end == text.len() as u64,
                digest,
                bytes: OutputBytes::Utf8(text[offset as usize..end as usize].into()),
            }
        };
        let mut pages = 0;
        let result = capture_graph_output(&target, Some("VERDICT: PASS"), |offset| {
            pages += 1;
            Ok(chunk(offset))
        })?;
        assert_eq!(pages, 2);
        assert!(result.matched_required);
        assert_eq!(result.verdict.as_deref(), Some("VERDICT: PASS"));
        assert_eq!(result.digest, digest.to_string());
        for malformed in [0, 1, 2, 3] {
            assert!(matches!(
                capture_graph_output(&target, None, |offset| {
                    let mut response = chunk(offset);
                    if let OutputResult::Chunk {
                        target,
                        offset: start,
                        next_offset,
                        digest,
                        ..
                    } = &mut response
                    {
                        match malformed {
                            0 => target.attempt = provenant_core::AttemptId::from_bytes([9; 16]),
                            1 => *start += 1,
                            2 => *next_offset = offset,
                            _ => *digest = provenant_core::Digest::from_bytes([9; 32]),
                        }
                    }
                    Ok(response)
                }),
                Err(FacadeError::StateCorrupt)
            ));
        }
        Ok(())
    }
}
