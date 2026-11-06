use super::*;

#[derive(Clone, Copy)]
pub(crate) struct BoundGraphBases {
    pub(crate) integration: Option<provenant_core::GitOid>,
    pub(crate) review: Option<provenant_core::GitOid>,
}

pub(crate) struct BoundGraphOwner<'a> {
    pub(crate) factory: &'a Factory,
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
    provenant_fabric_runtime::graph::AttemptUsage {
        extra: Default::default(),
        tokens,
        cost_micro_usd,
        wall_time_seconds: record.provider_ms.map(|ms| ms.saturating_add(999) / 1000),
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
        .map_err(|_| FacadeError::invalid("graph document is not a valid record"))?;
    match header
        .schema_version
        .as_ref()
        .and_then(serde_json::Value::as_u64)
    {
        Some(1 | 2) => provenant_records::decode(bytes, limits)
            .map_err(|error| FacadeError::invalid(format!("graph document: {error}"))),
        Some(version) if version > 2 => Err(FacadeError::UpgradeRequired {
            reader_release: env!("CARGO_PKG_VERSION").into(),
            writer_release: "unknown".into(),
            record: "graph document schema".into(),
            version,
        }),
        _ => Err(FacadeError::invalid(
            "graph document schema_version must be 1 or 2",
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
    document.nodes.iter().any(|node| {
        node.id == id
            && node.access == provenant_fabric_runtime::graph::AccessMode::ReadOnly
            && node.role.as_deref().is_some_and(|role| {
                role == "lens" || role.starts_with("lens.") || role.starts_with("review.")
            })
    })
}

/// What the host learned about a deferred review's pinned subject.
pub(crate) struct ReviewMeasure {
    pub(crate) routing: provenant_routing::RoutingConfig,
    pub(crate) directory: PathBuf,
    pub(crate) subject: provenant_core::GitOid,
    pub(crate) base: provenant_core::GitOid,
    pub(crate) base_known: bool,
    pub(crate) measured: provenant_routing::Measured,
}

#[cfg(test)]
#[allow(
    dead_code,
    reason = "the worktree driver includes test helpers without libtest functions"
)]
pub(crate) fn fixture_review_expansion(
    node: &provenant_fabric_runtime::graph::GraphNode,
    subject: &str,
    shape: &str,
) -> Result<
    provenant_fabric_runtime::graph::Expansion,
    provenant_fabric_runtime::graph::ReviewResolveError,
> {
    fixture_review_expansion_with_paths(node, subject, shape, &[])
}

#[cfg(test)]
pub(crate) fn fixture_review_expansion_with_paths(
    node: &provenant_fabric_runtime::graph::GraphNode,
    subject: &str,
    shape: &str,
    paths: &[String],
) -> Result<
    provenant_fabric_runtime::graph::Expansion,
    provenant_fabric_runtime::graph::ReviewResolveError,
> {
    use provenant_fabric_runtime::graph::ReviewResolveError;
    let routing = provenant_routing::RoutingConfig::default();
    let panel = routing
        .review
        .resolve_panel(
            &provenant_routing::ReviewRequest {
                shape: Some(shape.into()),
                ..Default::default()
            },
            None,
            paths,
            false,
        )
        .map_err(ReviewResolveError::Failed)?;
    let subject = provenant_core::GitOid::parse(subject)
        .map_err(|error| ReviewResolveError::Failed(error.to_string()))?;
    let base = node
        .extra
        .get("review_base")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| provenant_core::GitOid::parse(value).ok())
        .unwrap_or(subject);
    let measure = ReviewMeasure {
        routing,
        directory: PathBuf::new(),
        subject,
        base,
        base_known: true,
        measured: provenant_routing::Measured {
            size: Some("small"),
            warning: None,
        },
    };
    let floors = measure.routing.review.path_floors(paths);
    let floor_min = if floors.tier.is_some() {
        measure
            .routing
            .review
            .select_shape(Some(shape), None, None, paths, None)
            .map_err(ReviewResolveError::Failed)?
            .shape
            .reviewers
            .iter()
            .map(|row| row.count)
            .sum()
    } else {
        0
    };
    BoundGraphOwner::review_expansion(node, &measure, &panel, &floors, floor_min, "fixture".into())
}

impl BoundGraphOwner<'_> {
    pub(crate) fn review_git(&self, directory: &Path, args: &[&str]) -> Result<String, String> {
        use provenant_os::exec::{ExecOptions, Executor, Termination};
        let git = provenant_os::git::git_executable()
            .ok_or_else(|| "Git executable unavailable".to_owned())?;
        let mut argv = vec![git.into_os_string()];
        argv.extend(args.iter().map(std::ffi::OsString::from));
        let output = self
            .factory
            .executor
            .execute(ExecOptions::new(
                &argv,
                directory,
                std::time::Duration::from_secs(30),
            ))
            .map_err(|error| error.to_string())?;
        if output.termination != Termination::Exit(0) || output.timed_out || output.stdout.truncated
        {
            return Err(format!(
                "Git failed: {}",
                String::from_utf8_lossy(&output.stderr.bytes).trim()
            ));
        }
        String::from_utf8(output.stdout.bytes).map_err(|error| error.to_string())
    }

    pub(crate) fn measure_review(
        &self,
        node: &provenant_fabric_runtime::graph::GraphNode,
        subject: &str,
    ) -> Result<ReviewMeasure, provenant_fabric_runtime::graph::ReviewResolveError> {
        use provenant_fabric_runtime::graph::ReviewResolveError;
        let spec = node
            .review
            .as_ref()
            .ok_or_else(|| ReviewResolveError::Failed("missing review specification".into()))?;
        let subject = provenant_core::GitOid::parse(subject).map_err(|error| {
            ReviewResolveError::Failed(format!("invalid pinned review subject: {error}"))
        })?;
        let resolved_base = node
            .extra
            .get("review_base")
            .and_then(serde_json::Value::as_str)
            .and_then(|base| provenant_core::GitOid::parse(base).ok())
            .or_else(|| {
                self.integration_base
                    .and_then(|bases| bases.review.or(bases.integration))
            });
        // The checkpoint's review resolution requires a commit, but the
        // fallback must not pretend an unresolved base describes an empty diff.
        let base = resolved_base.unwrap_or(subject);
        let directory = spec.worktree.as_deref().map_or_else(
            || self.factory.cwd.clone(),
            |path| self.factory.cwd.join(path),
        );
        let routing: provenant_routing::RoutingConfig =
            provenant_config::read_domain(self.factory.context.roots.state.as_path(), "routing")
                .map_err(|error| {
                    ReviewResolveError::Failed(format!("read review configuration: {error}"))
                })?;
        let measured = if resolved_base.is_some() {
            routing.review.size.measure(
                spec.kind.as_deref(),
                &base.to_string(),
                &subject.to_string(),
                &mut |args| self.review_git(&directory, args),
            )
        } else {
            provenant_routing::Measured {
                size: None,
                warning: Some("no review base was resolved; size unknown".into()),
            }
        };
        Ok(ReviewMeasure {
            routing,
            directory,
            subject,
            base,
            base_known: resolved_base.is_some(),
            measured,
        })
    }

    /// The lens members and composite root for a resolved panel, built from the
    /// lowered lenses the preset stored on the deferred node. Every selected
    /// lens runs: a family preference only labels it, and a lens the panel has
    /// no room for is refused by name rather than dropped.
    pub(crate) fn review_expansion(
        node: &provenant_fabric_runtime::graph::GraphNode,
        measure: &ReviewMeasure,
        panel: &provenant_routing::PanelResolution,
        floors: &provenant_routing::AppliedFloors,
        floor_min: usize,
        source: String,
    ) -> Result<
        provenant_fabric_runtime::graph::Expansion,
        provenant_fabric_runtime::graph::ReviewResolveError,
    > {
        use provenant_fabric_runtime::graph::{
            CompositeSpec, Expansion, GraphInput, GraphNode, OutputKind, ReviewResolution,
            ReviewResolveError, SourceBinding,
        };
        let failed = |message: String| ReviewResolveError::Failed(message);
        let spec = node
            .review
            .as_ref()
            .ok_or_else(|| failed("missing review specification".into()))?;
        let templates = node
            .extra
            .get("lens_templates")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| failed("deferred review carries no lens templates".into()))?;
        let root_id = format!("{}--r", node.id);
        let panel_templates = node.extra.get("panel_templates").and_then(serde_json::Value::as_object)
            .ok_or_else(|| failed("deferred review carries no panel templates; finish old reviews before upgrading".into()))?;
        let lens_id = format!("{root_id}--p");
        let frame_id = format!("{root_id}--f");
        let subject = measure.subject.to_string();
        let mut members: Vec<GraphNode> = Vec::new();
        for slot in &panel.slots {
            let mut template = templates
                .get(&slot.lens)
                .cloned()
                .ok_or_else(|| failed(format!("no lowered lens for {}", slot.lens)))?;
            let map = template
                .as_object_mut()
                .ok_or_else(|| failed(format!("lens template {} is not a table", slot.lens)))?;
            let local = map
                .get("id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| failed(format!("lens template {} has no id", slot.lens)))?
                .to_owned();
            let effort = map.remove("effort");
            let read_only = map.get("access").is_none_or(|access| access == "read_only");
            let (key, route) =
                crate::preset::tier_route(&local, &slot.tier, effort.as_ref(), read_only)
                    .map_err(failed)?;
            map.insert(key.into(), route);
            map.insert(
                "id".into(),
                serde_json::json!(format!("{lens_id}--{local}")),
            );
            let lens_deps = if panel.frame {
                vec![frame_id.clone()]
            } else {
                node.depends_on.clone()
            };
            map.insert("depends_on".into(), serde_json::json!(lens_deps));
            if panel.frame {
                map.insert(
                    "inputs".into(),
                    serde_json::json!([{"node":frame_id,"outputs":["result"]}]),
                );
            } else if !node.inputs.is_empty() {
                map.insert("inputs".into(), serde_json::json!(node.inputs));
            }
            if !node.read_roots.is_empty() {
                map.insert("read_roots".into(), serde_json::json!(node.read_roots));
            }
            if !node.attachments.is_empty() {
                map.insert("attachments".into(), serde_json::json!(node.attachments));
            }
            if node.require_skill_location {
                map.insert("require_skill_location".into(), serde_json::json!(true));
            }
            map.insert(
                "source".into(),
                serde_json::json!({"type":"commit","commit":subject}),
            );
            if slot.distinct_families {
                map.insert(
                    "family_group".into(),
                    serde_json::json!(crate::preset::format_family_group(slot.group_index)),
                );
            }
            let member: GraphNode = serde_json::from_value(template)
                .map_err(|error| failed(format!("lens {} does not lower: {error}", slot.lens)))?;
            if members.iter().any(|existing| existing.id == member.id) {
                return Err(failed(format!("lens {} is selected twice", slot.lens)));
            }
            members.push(member);
        }
        if members.is_empty() {
            return Err(failed("the review panel selected no lenses".into()));
        }
        if let Some(captured) = node.extra.get("review_planner") {
            let catalogue = captured
                .pointer("/catalogue/patterns")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| failed("deferred review lacks its planner catalogue".into()))?;
            let mut patterns = Vec::new();
            for slot in &panel.slots {
                let name = format!("lens-{}", slot.lens);
                let mut pattern = catalogue
                    .iter()
                    .find(|pattern| pattern["name"] == name)
                    .cloned()
                    .ok_or_else(|| failed(format!("review catalogue lacks {name}")))?;
                if let Some(tier) = slot.tier.as_str() {
                    pattern["parameters"]["tier"]["default"] = serde_json::json!(tier);
                    if let Some(allowed) = pattern["parameters"]["tier"]["enum"].as_array_mut()
                        && !allowed.iter().any(|value| value == tier)
                    {
                        allowed.push(serde_json::json!(tier));
                    }
                }
                if slot.distinct_families {
                    for leaf in pattern["nodes"].as_array_mut().into_iter().flatten() {
                        leaf["family_group"] =
                            serde_json::json!(crate::preset::format_family_group(slot.group_index));
                    }
                }
                if let Some(base) = node.extra.get("review_base") {
                    for leaf in pattern["nodes"].as_array_mut().into_iter().flatten() {
                        leaf["review_base"] = base.clone();
                    }
                }
                patterns.push(pattern);
            }
            let mut fallback_members = members.clone();
            let prefix = format!("{lens_id}--");
            let mut fallback_exports = Vec::new();
            for (index, member) in fallback_members.iter_mut().enumerate() {
                if !member.id.starts_with(&prefix) {
                    return Err(failed("invalid lens identity".into()));
                }
                member.id = format!("l{}", index + 1);
                member.depends_on.clear();
                member.inputs.clear();
                if let Some(base) = node.extra.get("review_base") {
                    member.extra.insert("review_base".into(), base.clone());
                }
                let outputs = if index == 0 {
                    serde_json::json!(["result", "findings", "commit"])
                } else {
                    serde_json::json!(["result", "findings"])
                };
                fallback_exports.push(serde_json::json!({"node":member.id,"outputs":outputs}));
            }
            let minimum = spec.min_successful.map(|n| {
                if n == 0 {
                    members.len().saturating_sub(1).max(1)
                } else {
                    n
                }
            });
            let required: Vec<_> = floors
                .lenses
                .iter()
                .map(|name| format!("lens-{name}"))
                .collect();
            let candidates: Vec<String> = patterns
                .iter()
                .filter_map(|pattern| pattern["name"].as_str().map(str::to_owned))
                .collect();
            let floor_count = floor_min.max(minimum.unwrap_or(0));
            let reserve = required.len().max(floor_count) as u64;
            let max_nodes = captured
                .get("max_nodes")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(24)
                .min(512_u64.saturating_sub(reserve))
                .max(1);
            let mut plan: provenant_fabric_runtime::graph::PlanSpec = serde_json::from_value(serde_json::json!({
                "kinds":["task","synthesise"], "max_nodes":max_nodes.saturating_add(reserve),
                "model_max_nodes":max_nodes, "patterns_only":true, "wire_exports":true,
                "patterns":patterns, "floor_candidate_patterns":candidates,
                "required_floor_patterns":required, "floor_min_reviewers":floor_count,
                "floor_classes":floors.classes,
                "planned_review":true, "review_min_successful":minimum,
                "fallback": {"nodes":[{"id":"static", "kind":"composite", "access":"read_only",
                    "composite":{"nodes":fallback_members,"exports":fallback_exports,"min_successful":minimum,
                        "reduce_verdict": true}}],
                    "exports":[{"node":"static","outputs":["result","findings","verdict","commit"]}]}
            })).map_err(|error| failed(format!("invalid review planner: {error}")))?;
            plan.pin_review_subject(&subject).map_err(failed)?;
            let mut template = captured
                .get("template")
                .cloned()
                .ok_or_else(|| failed("deferred review lacks its planner template".into()))?;
            template["id"] = serde_json::json!(format!("{lens_id}--plan"));
            template["source"] = serde_json::json!({"type":"commit","commit":subject});
            template["depends_on"] = serde_json::json!(if panel.frame {
                vec![frame_id.clone()]
            } else {
                node.depends_on.clone()
            });
            template["inputs"] = if panel.frame {
                serde_json::json!([{"node":frame_id,"outputs":["result"]}])
            } else {
                serde_json::json!(node.inputs)
            };
            template["read_roots"] = serde_json::json!(node.read_roots);
            template["plan"] =
                serde_json::to_value(plan).map_err(|error| failed(error.to_string()))?;
            members = vec![
                serde_json::from_value(template)
                    .map_err(|error| failed(format!("invalid review planner node: {error}")))?,
            ];
        }
        let verifiers = node
            .extra
            .get("panel_verifiers")
            .and_then(serde_json::Value::as_u64)
            .map_or(panel.verifiers, |count| {
                usize::try_from(count).unwrap_or(usize::MAX)
            });
        if verifiers > 32 {
            return Err(failed("review verifier count exceeds 32".into()));
        }
        let total = members.len()
            + 2
            + usize::from(panel.frame)
            + verifiers
            + usize::from(panel.synthesis == provenant_routing::ReviewSynthesis::Model);
        if total > spec.max_nodes {
            return Err(failed(format!(
                "the review panel selected {} lenses; the review allows {}",
                members.len(),
                spec.max_nodes.saturating_sub(1)
            )));
        }
        let exports: Vec<_> = members
            .iter()
            .enumerate()
            .map(|(index, member)| {
                let mut outputs = vec![OutputKind::Result, OutputKind::Findings];
                if node.extra.contains_key("review_planner") {
                    outputs.push(OutputKind::Verdict);
                    outputs.push(OutputKind::Commit);
                }
                if index == 0 {
                    if panel.synthesis == provenant_routing::ReviewSynthesis::Reduce
                        && !outputs.contains(&OutputKind::Commit)
                    {
                        outputs.push(OutputKind::Commit);
                    }
                    for output in &spec.export_first {
                        if !outputs.contains(output) {
                            outputs.push(*output);
                        }
                    }
                }
                GraphInput {
                    extra: Default::default(),
                    node: member.id.clone(),
                    outputs,
                    required_outputs: Vec::new(),
                }
            })
            .collect();
        let reduction = panel.synthesis == provenant_routing::ReviewSynthesis::Reduce;
        let lens_deps = if panel.frame {
            vec![frame_id.clone()]
        } else {
            node.depends_on.clone()
        };
        let min_successful = if node.extra.contains_key("review_planner") {
            Some(1)
        } else {
            spec.min_successful.map(|n| {
                if n == 0 {
                    members.len().saturating_sub(1).max(1)
                } else {
                    n
                }
            })
        };
        let lens_group = GraphNode::composite(
            lens_id.clone(),
            lens_deps,
            CompositeSpec {
                min_successful,
                reduce_verdict: reduction,
                nodes: Vec::new(),
                exports,
            },
        );
        let template = |key: &str,
                        id: &str,
                        deps: Vec<String>,
                        inputs: Vec<GraphInput>|
         -> Result<GraphNode, ReviewResolveError> {
            let mut value = panel_templates
                .get(key)
                .cloned()
                .filter(|value| !value.is_null())
                .ok_or_else(|| failed(format!("deferred review lacks {key} template")))?;
            let map = value
                .as_object_mut()
                .ok_or_else(|| failed(format!("invalid {key} template")))?;
            map.insert("id".into(), serde_json::Value::String(id.into()));
            // These fields are supplied by the pinned execution, so template
            // values must not affect decoding or survive the binding.
            for field in [
                "source",
                "read_roots",
                "attachments",
                "depends_on",
                "inputs",
            ] {
                map.remove(field);
            }
            let mut result: GraphNode = serde_json::from_value(value)
                .map_err(|error| failed(format!("review {key} does not lower: {error}")))?;
            result.source = Some(SourceBinding::Commit {
                commit: subject.clone(),
            });
            result.read_roots = node.read_roots.clone();
            result.attachments = node.attachments.clone();
            result.depends_on = deps;
            result.inputs = inputs;
            Ok(result)
        };
        let selected = |id: String, outputs: Vec<OutputKind>| GraphInput {
            extra: Default::default(),
            node: id,
            outputs,
            required_outputs: Vec::new(),
        };
        let mut children = Vec::new();
        if panel.frame {
            children.push(template(
                "frame",
                &frame_id,
                node.depends_on.clone(),
                node.inputs.clone(),
            )?);
        }
        children.push(lens_group);
        children.extend(members);
        let mut previous = lens_id.clone();
        for index in 0..verifiers {
            let id = format!("{root_id}--v-{}", index + 1);
            let inputs = vec![selected(
                previous.clone(),
                vec![OutputKind::Result, OutputKind::Findings],
            )];
            children.push(template("verifier", &id, vec![previous], inputs)?);
            previous = id;
        }
        let verifier_output = (reduction && previous != lens_id).then(|| previous.clone());
        let output = if reduction {
            lens_id.clone()
        } else {
            let id = format!("{root_id}--s");
            let mut inputs = vec![selected(
                lens_id.clone(),
                vec![OutputKind::Result, OutputKind::Findings],
            )];
            if previous != lens_id {
                inputs.push(selected(
                    previous.clone(),
                    vec![OutputKind::Result, OutputKind::Findings],
                ));
            }
            let deps = if previous == lens_id {
                vec![lens_id.clone()]
            } else {
                vec![lens_id.clone(), previous]
            };
            children.push(template("synthesis", &id, deps, inputs)?);
            id
        };
        let mut root_exports = vec![GraphInput {
            extra: Default::default(),
            node: output,
            outputs: vec![
                OutputKind::Result,
                OutputKind::Findings,
                OutputKind::Verdict,
                OutputKind::Commit,
            ],
            required_outputs: Vec::new(),
        }];
        if let Some(verifier) = verifier_output {
            root_exports.push(GraphInput {
                extra: Default::default(),
                node: verifier,
                outputs: vec![OutputKind::Result, OutputKind::Findings],
                required_outputs: Vec::new(),
            });
        }
        let root = GraphNode::composite(
            root_id,
            node.depends_on.clone(),
            CompositeSpec {
                min_successful: None,
                reduce_verdict: false,
                nodes: Vec::new(),
                exports: root_exports,
            },
        );
        let mut nodes = vec![root];
        nodes.extend(children);
        Ok(Expansion {
            review: Some(ReviewResolution {
                subject: measure.subject,
                base: measure.base,
                measured_size: measure.measured.size.unwrap_or("unknown").to_owned(),
                shape: panel.shape.clone(),
                source,
                substitutions: Vec::new(),
            }),
            extra: Default::default(),
            source: node.id.clone(),
            continuation: false,
            nodes,
            patterns: Vec::new(),
        })
    }
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

impl provenant_fabric_runtime::graph::AttemptOwner for BoundGraphOwner<'_> {
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

    fn admission_timeout_seconds(&self) -> Option<u64> {
        let configured = provenant_confinement::run_config::RunConfig::read(
            self.factory.context.roots.state.as_path(),
        )
        .ok()
        .and_then(|config| {
            config.admission_seconds_for(&self.factory.context.identity.project.to_string())
        });
        let effective = provenant_run::AdmissionPolicy::resolve_timeout(None, configured);
        Some(effective.seconds)
    }
    type Error = FacadeError;
    fn resolve_review_base(
        &mut self,
        reference: &str,
        worktree: Option<&str>,
    ) -> Result<Option<String>, Self::Error> {
        let git = provenant_os::git::git_executable()
            .ok_or(FacadeError::InvalidInput { detail: None })?;
        let directory = worktree.map_or_else(
            || self.factory.cwd.clone(),
            |path| self.factory.cwd.join(path),
        );
        provenant_run::resolve_git_base(&git, &directory, reference)
            .map(Some)
            .map_err(run_error)
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

    fn retry_successors(&mut self, reference: &str) -> Result<u64, FacadeError> {
        let (host, attempt) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
        if host != self.factory.context.identity.host.to_string() {
            // Unknown remote lineage must never grant fresh retry allowance.
            return Ok(u64::from(provenant_routing::MAX_SETTLEMENT_RETRIES));
        }
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
            actor: None,
            chair: None,
        };
        let store = owner.store(self.cancel)?;
        let attempt = attempt.parse().map_err(|_| FacadeError::StateCorrupt)?;
        let call = owner.call(self.cancel);
        let record = store.record(attempt, &call).map_err(run_error)?;
        Ok(store
            .policy_attempt_number(&record, &call)
            .map_err(run_error)?
            .saturating_sub(1))
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
        let store = owner.store(self.cancel)?;
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

    fn resolve_review(
        &mut self,
        node: &provenant_fabric_runtime::graph::GraphNode,
        subject: &str,
    ) -> Result<
        provenant_fabric_runtime::graph::Expansion,
        provenant_fabric_runtime::graph::ReviewResolveError,
    > {
        use provenant_fabric_runtime::graph::ReviewResolveError;
        let measure = self.measure_review(node, subject)?;
        let spec = node
            .review
            .as_ref()
            .ok_or_else(|| ReviewResolveError::Failed("missing review specification".into()))?;
        // An unreadable path list must retain every potentially applicable floor.
        let mut warnings: Vec<String> = measure.measured.warning.iter().cloned().collect();
        let (mut paths, mut marker) = (Vec::new(), false);
        let mut paths_unreadable = !measure.base_known;
        if paths_unreadable {
            warnings.push("cannot resolve review base; applying highest configured floor and all required floor lenses".into());
        }
        let range = format!("{}...{}", measure.base, measure.subject);
        if measure.base != measure.subject {
            match self.review_git(
                &measure.directory,
                &["diff", "--no-renames", "--name-only", "-z", &range, "--"],
            ) {
                Ok(out) => {
                    paths = out
                        .split('\0')
                        .filter(|path| !path.is_empty())
                        .map(str::to_owned)
                        .collect();
                }
                Err(error) => {
                    paths_unreadable = true;
                    warnings.push(format!("cannot read changed paths: {error}; applying highest configured floor and all required floor lenses"));
                }
            }
            match self.review_git(
                &measure.directory,
                &["diff", "--no-ext-diff", "--unified=0", &range, "--"],
            ) {
                Ok(out) => marker = provenant_routing::diff_has_test_markers(&out),
                Err(error) => warnings.push(format!("cannot scan test markers: {error}")),
            }
        }
        let request = provenant_routing::ReviewRequest {
            shape: spec.shape.clone(),
            risk_tier: spec.risk_tier.clone(),
            kind: spec.kind.clone(),
            size: spec.size.clone(),
            risk_tags: spec.risk_tags.clone(),
            lenses: spec
                .lenses
                .iter()
                .map(|lens| provenant_routing::ReviewLens {
                    name: lens.name.clone(),
                    when: lens.when.clone(),
                    tier: lens.tier.clone(),
                })
                .collect(),
        };
        let fallback;
        let review = if paths_unreadable {
            let (config, unknown_paths) = measure.routing.review.unreadable_paths();
            paths = unknown_paths;
            fallback = config;
            &fallback
        } else {
            &measure.routing.review
        };
        let mut panel = review
            .resolve_panel(&request, measure.measured.size, &paths, marker)
            .map_err(ReviewResolveError::Failed)?;
        // A graph retains the stage configuration it was lowered with. The
        // launch still chooses the shape from the pinned subject.
        if let Some(shape) = node
            .extra
            .get("panel_shapes")
            .and_then(|shapes| shapes.get(&panel.shape))
        {
            panel.frame = shape
                .get("frame")
                .and_then(serde_json::Value::as_bool)
                .ok_or_else(|| {
                    ReviewResolveError::Failed("invalid deferred review frame".into())
                })?;
            panel.verifiers = shape
                .get("verifiers")
                .and_then(serde_json::Value::as_u64)
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| {
                    ReviewResolveError::Failed("invalid deferred verifier count".into())
                })?;
            panel.synthesis =
                serde_json::from_value(shape.get("synthesis").cloned().unwrap_or_default())
                    .map_err(|error| {
                        ReviewResolveError::Failed(format!("invalid deferred synthesis: {error}"))
                    })?;
        }
        let floors = review.path_floors(&paths);
        if !floors.classes.is_empty() {
            warnings.push(format!(
                "review path floors applied: {}; required lenses: {}",
                floors.classes.join(", "),
                floors.lenses.join(", ")
            ));
        }
        let mut source = format!(
            "{}; size={}; kind={}; risk={}",
            panel.source,
            spec.size
                .as_deref()
                .or(measure.measured.size)
                .unwrap_or("unknown"),
            spec.kind.as_deref().unwrap_or("unknown"),
            spec.risk_tier.as_deref().unwrap_or("unknown"),
        );
        if !panel.omitted.is_empty() {
            let omitted: Vec<String> = panel
                .omitted
                .iter()
                .map(|lens| format!("{} ({})", lens.lens, lens.reason))
                .collect();
            source.push_str(&format!("; omitted={}", omitted.join(", ")));
        }
        if !warnings.is_empty() {
            source.push_str(&format!("; warnings={}", warnings.join(" | ")));
        }
        let floor_min = if floors.tier.is_some() {
            review
                .select_shape(
                    spec.shape.as_deref(),
                    spec.risk_tier.as_deref(),
                    spec.kind.as_deref(),
                    &paths,
                    spec.size.as_deref().or(measure.measured.size),
                )
                .map_err(ReviewResolveError::Failed)?
                .shape
                .reviewers
                .iter()
                .map(|row| row.count)
                .sum()
        } else {
            0
        };
        Self::review_expansion(node, &measure, &panel, &floors, floor_min, source)
    }

    fn attempt_models(&mut self, attempts: &[String]) -> Result<Vec<String>, Self::Error> {
        self.prior_models(attempts)
    }

    fn bind_writer_worktree(
        &mut self,
        path: &str,
    ) -> Result<Option<provenant_fabric_runtime::graph::WriterWorktreeBinding>, Self::Error> {
        let request = provenant_run::resolve_writer(
            self.factory.context.roots.state.as_path(),
            &self.factory.cwd,
            Path::new(path),
        )
        .map_err(run_error)?;
        let writer = provenant_run::prepare_writer(
            &request,
            self.factory.context.roots.state.as_path(),
            self.factory.context.identity.project,
        )
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
        let store = owner.store(self.cancel)?;
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
        let git = provenant_os::git::git_executable()
            .ok_or(FacadeError::InvalidInput { detail: None })?;
        provenant_run::resolve_git_base(&git, &self.factory.cwd, &reference)
            .map(Some)
            .map_err(run_error)
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
        .session(self.cancel)?;
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
            let record = owner.graph_policy_tail(target, self.budget, self.cancel)?;
            return Ok(matches!(
                record.state,
                provenant_records::run::AttemptState::Terminal { .. }
            ) && record.custody_pending == Some(false));
        }
        let operation = graph_operation("graph-attempt-operation", &attempt.key)?;
        let store = owner.store(self.cancel)?;
        let call = owner.call(self.cancel);
        match store.operation(&operation, &call).map_err(run_error)? {
            None => Ok(true),
            Some(operation) => {
                use provenant_run::{AttemptFilter, AttemptReader};
                if !store
                    .pending_attempts(operation.run_id, &call)
                    .map_err(run_error)?
                    .is_empty()
                {
                    return Ok(false);
                }
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
        let store = owner.store(self.cancel)?;
        let call = owner.call(self.cancel);
        Ok(store
            .operation(&operation_id, &call)
            .map_err(run_error)?
            .is_some_and(|operation| operation.state != provenant_run::OperationState::Prepared))
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
            let store = owner.store(cancel)?;
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
            let store = owner.store(self.cancel)?;
            let call = owner.call(self.cancel);
            let operation = store.operation(&operation_id, &call).map_err(run_error)?;
            let accepted = operation.as_ref().is_some_and(|operation| {
                operation.state != provenant_run::OperationState::Prepared
            });
            // A crash can leave an accepted operation without a checkpoint
            // reference. Preserve its immutable labels, including legacy None.
            let mut accepted_knobs = None;
            let mut accepted_route = None;
            let mut accepted_retry_limit = None;
            let mut accepted_access = None;
            let mut accepted_skill_location = None;
            let context = match &operation {
                Some(operation) => match store.request(operation.run_id, &call) {
                    Ok(request) => {
                        let task = request.tasks.first().ok_or(FacadeError::StateCorrupt)?;
                        if accepted {
                            accepted_knobs = Some(task.profile.overrides.clone());
                            accepted_route = Some(task.route.clone());
                            accepted_retry_limit = Some(task.profile.settlement_retry_limit);
                            accepted_access = Some(task.access.clone());
                            accepted_skill_location = Some(task.profile.require_skill_location);
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
                && let Some(path) = worktree
            {
                let writer = provenant_run::resolve_writer(
                    self.factory.context.roots.state.as_path(),
                    &self.factory.cwd,
                    Path::new(path),
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
                    provenant_run::prepare_writer(
                        &writer,
                        self.factory.context.roots.state.as_path(),
                        self.factory.context.identity.project,
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
                    node.source_commit,
                )
            {
                let head = commit
                    .parse()
                    .map_err(|_| FacadeError::InvalidInput { detail: None })?;
                let descends = provenant_confinement::worktrees::with_context(
                    &self.factory.context.roots,
                    &self.factory.home,
                    &self.factory.cwd,
                    &self.factory.clock,
                    self.cancel,
                    |context| {
                        provenant_confinement::worktrees::is_ancestor(
                            context,
                            base,
                            head,
                            self.cancel,
                        )
                    },
                )
                .map_err(|error| graph_writer_initialisation_error(commit, error))?;
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
                && let (Some(path), Some(commit)) = (worktree, node.source_commit)
            {
                let head = commit
                    .parse()
                    .map_err(|_| FacadeError::InvalidInput { detail: None })?;
                provenant_confinement::worktrees::with_context(
                    &self.factory.context.roots,
                    &self.factory.home,
                    &self.factory.cwd,
                    &self.factory.clock,
                    self.cancel,
                    |context| {
                        provenant_confinement::worktrees::fast_forward(
                            context,
                            Path::new(path),
                            head,
                            self.cancel,
                        )
                    },
                )
                .map_err(|error| graph_writer_initialisation_error(commit, error))?;
            }
            let delivery = if let Some(mode) = replay.delivery {
                let branch = if let Some(path) = worktree {
                    let writer = provenant_run::resolve_writer(
                        self.factory.context.roots.state.as_path(),
                        &self.factory.cwd,
                        Path::new(path),
                    )
                    .map_err(run_error)?;
                    provenant_run::prepare_writer(
                        &writer,
                        self.factory.context.roots.state.as_path(),
                        self.factory.context.identity.project,
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
            let review_base = if access == provenant_fabric_runtime::graph::AccessMode::ReadOnly {
                provenant_fabric_runtime::graph::FileJournal::execution(
                    &self.factory.context.roots.state,
                    self.factory.context.identity.project,
                    &key.graph_id,
                )
                .map_err(graph_journal_error)?
                .and_then(|execution| execution.review_base)
            } else {
                None
            };
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
            } else if access == provenant_fabric_runtime::graph::AccessMode::ReadOnly
                && node.source_commit.is_some()
            {
                node.review_base
                    .map(str::to_owned)
                    // A review base recorded with the graph execution comes next,
                    // then the integration base.
                    .or_else(|| review_base.map(|base| base.to_string()))
                    .or_else(|| {
                        self.integration_base
                            .and_then(|bases| bases.integration)
                            .map(|base| base.to_string())
                    })
                    // Preserve the prior fallback without requiring origin/HEAD.
                    .or_else(|| node.source_commit.map(str::to_owned))
            } else {
                node.source_commit.map(|head| {
                    self.integration_base
                        .and_then(|bases| bases.integration)
                        .map_or_else(|| head.to_owned(), |base| base.to_string())
                })
            };
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
                    head: node.source_commit.map(str::to_owned),
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
                    prompt: node.prompt.into(),
                    title: Some(node.title.to_owned()),
                    route: accepted_route.unwrap_or(match &route.model {
                        None if route.name.is_some() => {
                            provenant_records::run::RequestedRoute::Adapter {
                                extra: Default::default(),
                                provider: route
                                    .adapter
                                    .clone()
                                    .ok_or(FacadeError::InvalidInput { detail: None })?,
                                name: route
                                    .name
                                    .clone()
                                    .ok_or(FacadeError::InvalidInput { detail: None })?,
                            }
                        }
                        Some(model) => {
                            let adapter = route
                                .adapter
                                .as_ref()
                                .ok_or(FacadeError::InvalidInput { detail: None })?;
                            provenant_records::run::RequestedRoute::Model {
                                extra: Default::default(),
                                provider: adapter.clone(),
                                model: exact_model(Some(adapter), model),
                                effort: route.effort.clone(),
                            }
                        }
                        None if !avoided_families.is_empty() => {
                            provenant_records::run::RequestedRoute::Review {
                                extra: Default::default(),
                                name: route
                                    .alias
                                    .clone()
                                    .ok_or(FacadeError::InvalidInput { detail: None })?,
                                effort: route.effort.clone(),
                                excluded_families: avoided_families,
                                distinct_families: true,
                                preferred_families,
                                require_distinct: enforced,
                            }
                        }
                        None => provenant_records::run::RequestedRoute::Named {
                            extra: Default::default(),
                            name: route
                                .alias
                                .clone()
                                .ok_or(FacadeError::InvalidInput { detail: None })?,
                            effort: route.effort.clone(),
                        },
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
                    if !exclude.is_empty() {
                        knobs.push(format!(
                            "routing.graph_exclude={}",
                            serde_json::Value::from(exclude)
                        ));
                    }
                    knobs
                },
                host: None,
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
                        worktree: None,
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
                        write_root: Vec::new(),
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
                    && node.source_commit.is_some()
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
                            .map_err(|_| FacadeError::ConfinementRefused)?;
                        let identity = directory
                            .metadata()
                            .map_err(|_| FacadeError::ConfinementRefused)?;
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
                        // A preset may lower its source worktree to a read root.
                        // The immutable view replaces those live-source grants.
                        grants.read_roots.retain(|root| !root.starts_with(&source));
                        Some(source)
                    };
                }
            }
            let retry_limit = accepted_retry_limit.unwrap_or(node.settlement_retries);
            let receipt =
                owner.submit_with_access(input, self.budget, self.cancel, grants, retry_limit)?;
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
        let mut attempt = owner.snapshot(target.clone(), self.budget, self.cancel)?;
        let mut reference = reference;
        if target
            .host
            .is_none_or(|host| host == self.factory.context.identity.host)
        {
            let store = owner.store(self.cancel)?;
            attempt = store
                .policy_tail(attempt.attempt_id, &owner.call(self.cancel))
                .map_err(run_error)?;
            reference = Some(format!(
                "{}/{}",
                self.factory.context.identity.host, attempt.attempt_id
            ));
        }
        use provenant_records::run::{AttemptState, Outcome};
        match attempt.state {
            AttemptState::Terminal { outcome, .. } => {
                let store = owner.store(self.cancel)?;
                let call = owner.call(self.cancel);
                match store.custody_released(&attempt, &call) {
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
                let pickup = store
                    .delivered(attempt.attempt_id, &call)
                    .map_err(run_error)?;
                let effective_delivery = pickup
                    .as_ref()
                    .map(|(_, receipt)| receipt)
                    .or(attempt.delivery.as_ref());
                let launched = attempt.provider_launched.unwrap_or(true);
                let (result, result_warning) = if attempt.result.is_some()
                    || outcome == Outcome::Succeeded
                {
                    match store.stream_result(attempt.attempt_id, &call, node.required_verdict) {
                        Ok(result) => (Some(Box::new(result)), None),
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
                            Outcome::TimedOut if node.deadline_seconds.is_some() => {
                                AttemptOutcome::DeadlineExceeded
                            }
                            Outcome::TimedOut => AttemptOutcome::OtherFailure,
                            // A route refused before launch leaves the next route free to try.
                            Outcome::ModelUnavailable => AttemptOutcome::AdapterUnavailable,
                            Outcome::Rejected if !launched => AttemptOutcome::AdapterUnavailable,
                            Outcome::ReviewRequired
                                if attempt.partial_signals.as_slice() == [provenant_records::run::PartialSignal::DeclaredStop] =>
                            {
                                AttemptOutcome::DeclaredStop
                            }
                            Outcome::Failed => AttemptOutcome::TaskFailed,
                            Outcome::ReviewRequired => AttemptOutcome::ReviewRequired,
                            // Uncommitted edits need the chair, so the node fails without fallback.
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
