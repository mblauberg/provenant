//! Host side of a lane's graph frames.
//!
//! The relay serves one attempt, so the host finds the running node that owns
//! that attempt and acts as it: the lane never names its graph or parent.
//! Admission takes the graph's journal lock, goes through
//! `provenant_fabric_runtime::graph::relay#admit_child` and saves the
//! checkpoint before the receipt is returned, so a lost reply is answered by
//! replaying the same idempotency key.
use super::Context;
use provenant_core::{CancelToken, Clock, WaitBudget};
use provenant_fabric_mcp::tools::{FacadeError, ServiceError};
use provenant_fabric_runtime::graph::{
    FileJournal, GraphJournal,
    patch::{LiveGraph, PatchContext, scan_for_secrets},
    relay::{self, RelayOp},
    template::{Layer, TemplateLibrary},
};
use serde_json::{Value, json};

fn refused(code: &str, message: impl Into<String>) -> ServiceError {
    use provenant_fabric_runtime::coordination::facade::{
        GraphRelayCause as Cause, InvalidInputDetail,
    };
    let message = message.into();
    let cause = match code {
        "parent_settled" => Some(Cause::ParentSettled),
        "child_depth" => Some(Cause::ChildDepth),
        "writer_parent_required" => Some(Cause::WriterParentRequired),
        "writer_worktree" => Some(Cause::WriterWorktree),
        "not_found" | "confinement_refused" => Some(Cause::TargetOutsideGraph),
        "graph_relay_unavailable" => Some(Cause::RelayUnavailable),
        "invalid_input" => Some(Cause::InvalidFrame),
        "graph_read_unavailable" => Some(Cause::ReadUnavailable),
        "writer_source" => Some(Cause::WriterSource),
        "started" => Some(Cause::StartedNode),
        "edit_scope" => Some(Cause::TargetOutsideGraph),
        "journal" => Some(Cause::JournalUnavailable),
        "invalid" if message.starts_with("child id must be") => Some(Cause::InvalidChildId),
        "invalid_template" => Some(Cause::InvalidTemplate),
        "invalid_config" => Some(Cause::InvalidConfig),
        "invalid_review" => Some(Cause::InvalidReview),
        "secret" => Some(Cause::Secret),
        "cycle" => Some(Cause::Cycle),
        "conflict" => Some(Cause::Conflict),
        "invalid" if message.ends_with("writer worktree") => Some(Cause::WriterWorktree),
        "invalid"
            if message == "writer composite children require explicit per-node work bindings" =>
        {
            Some(Cause::WriterComposite)
        }
        "invalid" if message == "write_scope requires a writer child" => Some(Cause::WriteScope),
        "invalid" | "invalid_graph" => Some(Cause::InvalidLaunch),
        _ => None,
    };
    match cause {
        Some(cause) => ServiceError::Facade(FacadeError::GraphRelayRefused {
            refusal_code: code.into(),
            detail: Some(InvalidInputDetail::GraphRelayCause(cause)),
        }),
        None => ServiceError::Cli {
            code: code.into(),
            message,
            data: Value::Null,
        },
    }
}

fn patch_refused(error: provenant_fabric_runtime::graph::patch::PatchError) -> ServiceError {
    match error.admission {
        Some(admission) => ServiceError::Facade(FacadeError::graph_admission(admission)),
        None => refused(error.code, error.message),
    }
}

fn cutover_refusal(findings: String) -> ServiceError {
    use provenant_fabric_runtime::coordination::facade::InvalidInputDetail;
    ServiceError::Facade(FacadeError::GraphRelayRefused {
        refusal_code: "UPGRADE_DRAIN_REQUIRED".into(),
        detail: Some(InvalidInputDetail::CutoverFindings(findings)),
    })
}

fn journal_error<E>(_: E) -> ServiceError {
    ServiceError::Facade(FacadeError::io("access graph journal"))
}

/// Serve one graph frame for `attempt` on this host.
pub fn serve(
    context: &Context,
    attempt: &str,
    arguments: Value,
    budget: WaitBudget,
    cancel: &CancelToken,
    clock: &dyn Clock,
) -> Result<Value, ServiceError> {
    serve_with_read(context, attempt, arguments, budget, cancel, clock, &|_| {
        Err(refused(
            "graph_read_unavailable",
            "the host has no graph reader; upgrade the host",
        ))
    })
}

/// Serve reads with the binary's existing graph service after attempt-bound authorisation.
pub fn serve_with_read(
    context: &Context,
    attempt: &str,
    arguments: Value,
    budget: WaitBudget,
    cancel: &CancelToken,
    clock: &dyn Clock,
    read: &impl Fn(Value) -> Result<Value, ServiceError>,
) -> Result<Value, ServiceError> {
    let op: RelayOp = serde_json::from_value(arguments)
        .map_err(|error| refused("invalid_input", format!("invalid graph frame: {error}")))?;
    let state = &context.roots.state;
    let project = context.identity.project;
    let reference = format!("{}/{attempt}", context.identity.host);
    let not_a_node = || {
        refused(
            "graph_relay_unavailable",
            "this attempt is not a running graph node",
        )
    };
    // Settled checkpoints retain admitted child receipts after their pending
    // marker disappears. Only the replay path below may act as a settled node.
    let graph_id = FileJournal::graph_ids(state, project)
        .map_err(journal_error)?
        .into_iter()
        .find(|id| {
            FileJournal::read_checkpoint(state, project, id)
                .ok()
                .flatten()
                .is_some_and(|checkpoint| {
                    relay::historical_parent_of_attempt(&checkpoint, &reference).is_some()
                })
        })
        .ok_or_else(not_a_node)?;
    if let RelayOp::Read { mut input } = op {
        if !matches!(input["op"].as_str(), Some("status" | "output" | "trace")) {
            return Err(refused(
                "invalid_input",
                "graph reads support status, output and trace",
            ));
        }
        let checkpoint = FileJournal::read_status_checkpoint(state, project, &graph_id)
            .map_err(journal_error)?
            .ok_or_else(not_a_node)?;
        let requested = input["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .unwrap_or(&graph_id);
        let selected = if requested == graph_id {
            graph_id.clone()
        } else {
            let node = requested
                .strip_prefix(&format!("{graph_id}/"))
                .unwrap_or(requested);
            let found = checkpoint.nodes.iter().find(|row| {
                row.id == node || row.recorded_attempts().any(|attempt| {
                    attempt.attempt_ref.as_deref().is_some_and(|reference| {
                        reference == requested || reference.rsplit('/').next() == Some(requested)
                    })
                })
            }).ok_or_else(|| refused("not_found", "the target is outside the calling lane's graph; use the graph or child ID from its launch receipt"))?;
            // Canonicalise node and attempt selectors before calling the ordinary reader.
            if node == found.id || input["op"] == "trace" || input["trace"] == true {
                format!("{graph_id}/{}", found.id)
            } else {
                requested.to_owned()
            }
        };
        input["id"] = selected.into();
        return read(input);
    }
    let mut journal = match FileJournal::open(state, project, &graph_id, budget, cancel, clock) {
        Ok(journal) => journal,
        Err(provenant_fabric_runtime::graph::JournalError::Filesystem(
            provenant_os::fs::FsError::LockTimeout { .. },
        )) if matches!(&op, RelayOp::Wait { waiting: false }) => {
            return Ok(json!({"graph_id": graph_id, "waiting": true, "reclaiming": true}));
        }
        Err(error) => return Err(journal_error(error)),
    };
    let mut checkpoint = journal
        .load(&graph_id)
        .map_err(journal_error)?
        .ok_or_else(not_a_node)?;
    let document = FileJournal::execution(state, project, &graph_id)
        .map_err(journal_error)?
        .ok_or_else(not_a_node)?
        .document;
    // Finish provisioning fenced by an earlier crashed call before anything else.
    recover_provisioning(context, &mut journal, &mut checkpoint, cancel)?;
    // A settled parent or superseded attempt may only replay an earlier receipt.
    let live = relay::parent_of_attempt(&checkpoint, &reference).map(str::to_owned);
    let parent = match (&live, &op) {
        (Some(parent), _) => parent.clone(),
        (None, RelayOp::Add(request)) => {
            let historical = relay::historical_parent_of_attempt(&checkpoint, &reference)
                .ok_or_else(not_a_node)?;
            return match relay::replay_child(&document, &checkpoint, historical, request)
                .map_err(patch_refused)?
            {
                Some(receipt) => Ok(serde_json::to_value(receipt)?),
                None => Err(not_a_node()),
            };
        }
        (None, _) => return Err(not_a_node()),
    };
    let value = match op {
        RelayOp::Read { .. } => unreachable!("reads return before journal locking"),
        RelayOp::Add(request) => {
            if let Some(receipt) = relay::replay_child(&document, &checkpoint, &parent, &request)
                .map_err(patch_refused)?
            {
                return Ok(serde_json::to_value(receipt)?);
            }
            provenant_fabric_runtime::graph::cutover::pending_guard(state)
                .map_err(cutover_refusal)?;
            let library = template_library(&context.home, &context.cwd)
                .map_err(|error| refused("invalid_template", error))?;
            let config =
                provenant_config::read_with_domains(&context.roots, &crate::domains::SECTIONS)
                    .map_err(|error| refused("invalid_config", error.to_string()))?;
            let resolvers = launch_resolvers(&config.domains.routing.review, &document)
                .map_err(|error| refused("invalid_review", error))?;
            let patch_context = PatchContext {
                journal: None,
                library: &library,
                resolvers: &resolvers,
                scan: &scan_for_secrets,
                limits: &context.config.graph.limits,
            };
            fn needs_parent_source(node: &Value) -> bool {
                (node["access"] == "worktree_write"
                    && (node.get("worktree").and_then(Value::as_str).is_none()
                        || node["provision_worktree"] == true))
                    || ["/composite/nodes", "/loop/body"].iter().any(|pointer| {
                        node.pointer(pointer)
                            .and_then(Value::as_array)
                            .is_some_and(|nodes| nodes.iter().any(needs_parent_source))
                    })
            }
            let needs_parent = request.needs_worktree_provisioning()
                || request
                    .launch
                    .as_ref()
                    .is_some_and(|launch| needs_parent_source(&launch.node));
            // An explicit checkout can launch independently of a read-only
            // parent's writer registration. Only new child checkouts need a
            // parent source. Explicit checkouts retain their own launch bindings.
            let writer_base = if request.has_writers() && needs_parent {
                let parent_path = relay::parent_worktree(
                    &document,
                    &checkpoint,
                    &parent,
                    &context.config.graph.limits,
                )
                .map_err(|error| refused("invalid_graph", error))?;
                if let (None, Some(branch), Some(source)) = (
                    &parent_path,
                    &document.integration_branch,
                    &checkpoint.bound_integration_base,
                ) {
                    Some((branch.clone(), source.clone()))
                } else {
                    let path = parent_path
                        .clone()
                        .unwrap_or_else(|| context.cwd.to_string_lossy().into_owned());
                    let writer_cwd = parent_path
                        .as_deref()
                        .map(std::path::Path::new)
                        .unwrap_or(&context.cwd);
                    let writer = provenant_run::resolve_writer(
                        state.as_path(),
                        writer_cwd,
                        std::path::Path::new(&path),
                    )
                    .map_err(|error| refused("writer_parent_required", error.to_string()))?;
                    let launch = provenant_run::prepare_writer(&writer, state.as_path(), project)
                        .map_err(|error| {
                        refused("writer_parent_required", error.to_string())
                    })?;
                    let branch = if parent_path.is_none() {
                        document.integration_branch.clone()
                    } else {
                        None
                    }
                    .unwrap_or_else(|| {
                        launch
                            .branch
                            .as_str()
                            .trim_start_matches("refs/heads/")
                            .to_owned()
                    });
                    let source = if parent_path.is_none() {
                        checkpoint.bound_integration_base.clone()
                    } else {
                        None
                    }
                    .unwrap_or_else(|| launch.source_commit().to_string());
                    Some((branch, source))
                }
            } else {
                None
            };
            let receipt = relay::admit_child_bound(
                &mut LiveGraph {
                    document: &document,
                    checkpoint: &mut checkpoint,
                },
                &parent,
                &request,
                &patch_context,
                writer_base
                    .as_ref()
                    .map(|(branch, commit)| (branch.as_str(), commit.as_str())),
            )
            .map_err(patch_refused)?;
            if receipt.replayed {
                // Nothing changed; the earlier admission is already journalled.
                return Ok(serde_json::to_value(receipt)?);
            }
            if request.needs_worktree_provisioning()
                && let Some(path) = receipt.worktree.as_deref()
            {
                let source = relay::child_source(
                    &document,
                    &checkpoint,
                    &receipt.node,
                    &context.config.graph.limits,
                )
                .map_err(|error| refused("invalid_graph", error))?
                .ok_or_else(|| refused("invalid_graph", "writer child has no source"))?;
                // Checkpoint the fence before any Git effect; a crash after
                // this point is finished by `recover_provisioning`.
                relay::fence_provisioning(&mut checkpoint, &receipt.node, path, &source);
                journal.save(&checkpoint).map_err(journal_error)?;
                provision_child(context, path, &source, cancel)?;
                relay::clear_provisioning(&mut checkpoint, &receipt.node);
            }
            provision_added_writers(context, &mut journal, &mut checkpoint, &[], cancel)?;
            serde_json::to_value(receipt)?
        }
        RelayOp::Edit { id, patch } => {
            let belongs = id == graph_id
                || checkpoint.nodes.iter().any(|node| {
                    id == node.id
                        || id == format!("{graph_id}/{}", node.id)
                        || node.recorded_attempts().any(|row| {
                            row.attempt_ref.as_ref().is_some_and(|reference| {
                                id == *reference
                                    || reference.rsplit('/').next() == Some(id.as_str())
                            })
                        })
                });
            if !belongs {
                return Err(refused(
                    "not_found",
                    "the target is not the calling lane's graph",
                ));
            }
            let library = template_library(&context.home, &context.cwd)
                .map_err(|error| refused("invalid_template", error))?;
            let config =
                provenant_config::read_with_domains(&context.roots, &crate::domains::SECTIONS)
                    .map_err(|error| refused("invalid_config", error.to_string()))?;
            let resolvers = launch_resolvers(&config.domains.routing.review, &document)
                .map_err(|error| refused("invalid_review", error))?;
            // The ID selects this graph; prepare checks each skip/replace target
            // against the calling parent's subtree, with the planner exemption.
            let admitted = provenant_fabric_runtime::graph::patch::admit_patch(
                &mut LiveGraph {
                    document: &document,
                    checkpoint: &mut checkpoint,
                },
                Some(&parent),
                &patch,
                &PatchContext {
                    journal: None,
                    library: &library,
                    resolvers: &resolvers,
                    scan: &scan_for_secrets,
                    limits: &context.config.graph.limits,
                },
            )
            .map_err(patch_refused)?;
            if !admitted.replayed {
                provenant_fabric_runtime::graph::cutover::pending_guard(state)
                    .map_err(cutover_refusal)?;
            }
            provision_added_writers(
                context,
                &mut journal,
                &mut checkpoint,
                &admitted.added,
                cancel,
            )?;
            json!({"graph_id":graph_id,"revision":admitted.revision,"replayed":admitted.replayed,"added":admitted.added})
        }
        RelayOp::Stop { node } => {
            let selected = stop_target(&graph_id, &parent, &node)?;
            let subtree = provenant_fabric_runtime::graph::cancel_node_subtree(
                &document,
                &mut checkpoint,
                selected,
                Some("stopped by parent lane"),
            )
            .map_err(|error| refused("invalid_input", error))?;
            // The graph owner observes this durable fence and stops attempts.
            json!({"graph_id":graph_id,"node":selected,"requested":true,
                "running":subtree.running,"skipped":subtree.skipped,"detached":subtree.detached})
        }
        RelayOp::Children => {
            return Ok(relay::child_states(&checkpoint, &parent));
        }
        RelayOp::Wait { waiting } => {
            let call = provenant_run::Request { clock, cancel };
            let execution = provenant_run::ExecutionContext {
                state_root: state.clone(),
                project_id: project,
                actor_id: context.identity.inbox.actor,
                read_prompts: false,
            };
            let id = provenant_core::AttemptId::parse(attempt).map_err(journal_error)?;
            let admission = provenant_run::RunStore::open(&execution, &call)
                .and_then(|store| store.set_child_wait(id, waiting, &call));
            let admitted = match admission {
                Ok(admitted) => admitted,
                // Store/ledger locks are another admission wait. Preserve the
                // completed child result and retry after releasing the journal.
                Err(provenant_run::RunError::Contention) if !waiting => false,
                Err(error) => return Err(journal_error(error)),
            };
            // Keep the graph slot yielded until host admission is reclaimed.
            // Each retry releases the journal lock so children can settle.
            if !admitted {
                return Ok(json!({"graph_id": graph_id, "node": parent,
                    "waiting": true, "reclaiming": true}));
            }
            relay::set_waiting(&mut checkpoint, &parent, waiting);
            json!({"graph_id": graph_id, "node": parent, "waiting": waiting})
        }
    };
    journal.save(&checkpoint).map_err(journal_error)?;
    Ok(value)
}

fn stop_target<'a>(graph: &str, parent: &str, target: &'a str) -> Result<&'a str, ServiceError> {
    let selected = target.strip_prefix(&format!("{graph}/")).unwrap_or(target);
    if selected != parent && !selected.starts_with(&format!("{parent}--")) {
        return Err(refused(
            "confinement_refused",
            "a lane may stop only its own subtree",
        ));
    }
    Ok(selected)
}

/// Fence provisionable writer leaves from a new revision before any Git effect.
fn provision_added_writers(
    context: &Context,
    journal: &mut FileJournal,
    checkpoint: &mut provenant_fabric_runtime::graph::GraphCheckpoint,
    added: &[String],
    cancel: &CancelToken,
) -> Result<(), ServiceError> {
    use provenant_fabric_runtime::graph::{AccessMode, SourceBinding};
    let writers: Vec<_> = checkpoint
        .expansions
        .iter()
        .flat_map(|expansion| &expansion.nodes)
        .filter(|node| {
            (added.is_empty() || added.contains(&node.id))
                && node.access == AccessMode::WorktreeWrite
                && node
                    .extra
                    .get("provision_worktree")
                    .and_then(Value::as_bool)
                    == Some(true)
                && !checkpoint.nodes.iter().any(|progress| {
                    (progress.worktree == node.worktree && progress.writer_binding.is_some())
                        || (progress.id == node.id && !progress.attempts.is_empty())
                })
        })
        .cloned()
        .collect();
    for node in writers {
        let Some(path) = node.worktree.as_deref() else {
            continue;
        };
        let source = if let Some(source) = checkpoint
            .nodes
            .iter()
            .find(|progress| progress.id == node.id)
            .and_then(|progress| progress.extra.get("provisioned_source"))
            .and_then(Value::as_str)
        {
            source.to_owned()
        } else {
            match node.source {
                Some(SourceBinding::Commit { commit }) => commit,
                // These sources become available only when dependencies settle.
                Some(_) => continue,
                None => match checkpoint.bound_integration_base.clone() {
                    Some(source) => source,
                    None => {
                        let request = provenant_run::resolve_writer(
                            context.roots.state.as_path(),
                            &context.cwd,
                            &context.cwd,
                        )
                        .map_err(|error| refused("writer_source", error.to_string()))?;
                        provenant_run::prepare_writer(
                            &request,
                            context.roots.state.as_path(),
                            context.identity.project,
                        )
                        .map_err(|error| refused("writer_source", error.to_string()))?
                        .source_commit()
                        .to_string()
                    }
                },
            }
        };
        if let Some(progress) = checkpoint
            .nodes
            .iter_mut()
            .find(|progress| progress.id == node.id)
        {
            progress
                .extra
                .insert("provisioned_source".into(), source.clone().into());
        }
        relay::fence_provisioning(checkpoint, &node.id, path, &source);
    }
    if !relay::pending_provisioning(checkpoint).is_empty() {
        journal.save(checkpoint).map_err(journal_error)?;
        recover_provisioning(context, journal, checkpoint, cancel)?;
    }
    Ok(())
}

/// Redo every fenced provisioning, then save the cleared fences.
fn recover_provisioning(
    context: &Context,
    journal: &mut FileJournal,
    checkpoint: &mut provenant_fabric_runtime::graph::GraphCheckpoint,
    cancel: &CancelToken,
) -> Result<(), ServiceError> {
    let pending = relay::pending_provisioning(checkpoint);
    if pending.is_empty() {
        return Ok(());
    }
    for (node, path, source) in pending {
        provision_child(context, &path, &source, cancel)?;
        relay::clear_provisioning(checkpoint, &node);
    }
    journal.save(checkpoint).map_err(journal_error)
}

/// Lower a tier (a pool name, an exact `ADAPTER/ID[@effort]`, a route table or
/// a route list) to the `route` or `routes` entry a node carries. A read-only
/// pool name repeats once, so a usage-limited first member falls back to the
/// next resolution. Writers keep one entry. A route table that names neither
/// an alias nor an adapter and model is refused rather than dropped.
pub fn tier_route(
    id: &str,
    tier: &Value,
    effort: Option<&Value>,
    read_only: bool,
) -> Result<(&'static str, Value), String> {
    if tier.is_array() {
        if effort.is_some() {
            return Err(format!(
                "node {id}: route lists cannot be combined with effort"
            ));
        }
        return Ok(("routes", tier.clone()));
    }
    let mut route = match tier {
        Value::String(name) if !name.is_empty() => {
            if let Some((adapter, model)) = name.split_once('/') {
                let (model, embedded_effort) = model
                    .split_once('@')
                    .map_or((model, None), |(model, effort)| (model, Some(effort)));
                if adapter.is_empty() || model.is_empty() || embedded_effort == Some("") {
                    return Err(format!("node {id}: invalid exact-model tier {name}"));
                }
                if embedded_effort.is_some() && effort.is_some() {
                    return Err(format!(
                        "node {id}: exact-model tier and effort both specify effort"
                    ));
                }
                let mut route = json!({"adapter":adapter,"model":model});
                if let Some(embedded_effort) = embedded_effort {
                    route["effort"] = json!(embedded_effort);
                }
                route
            } else {
                json!({"alias":name})
            }
        }
        Value::Object(table) => {
            let named = |key: &str| table.get(key).is_some_and(|value| value.is_string());
            if !named("alias") && !(named("adapter") && named("model")) {
                return Err(format!(
                    "node {id}: route table needs an alias, or an adapter and a model"
                ));
            }
            tier.clone()
        }
        _ => {
            return Err(format!(
                "node {id}: tier must be a name, route table or route list"
            ));
        }
    };
    if let Some(effort) = effort {
        route["effort"] = effort.clone();
    }
    let alias_only = route.get("alias").is_some() && route.get("model").is_none();
    if read_only && alias_only {
        Ok(("routes", json!([route.clone(), route])))
    } else {
        Ok(("route", route))
    }
}

/// Resolve dynamic panels at the host, using the graph's declared review risk.
pub fn launch_resolvers(
    review: &provenant_routing::ReviewConfig,
    document: &provenant_fabric_runtime::graph::GraphDocument,
) -> Result<std::collections::BTreeMap<String, Vec<String>>, String> {
    let mut request: provenant_routing::ReviewRequest = document
        .extra
        .get("review")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| format!("invalid graph review request: {error}"))?
        .unwrap_or_default();
    if let Some(context) = &document.context {
        request.risk_tier = request.risk_tier.or_else(|| context.risk_tier.clone());
        request.kind = request.kind.or_else(|| context.task_kind.clone());
    }
    let panel = review.resolve_panel(&request, None, &[], false)?;
    let mut resolvers = std::collections::BTreeMap::new();
    let single = panel.slots.len() == 1;
    let frame = !single && panel.frame;
    let verifiers = if single { 0 } else { panel.verifiers };
    let synthesis = !single && panel.synthesis == provenant_routing::ReviewSynthesis::Model;
    resolvers.insert(
        "review-topology".into(),
        vec![format!(
            "f{}-v{}-s{}",
            u8::from(frame),
            u8::from(verifiers > 0),
            u8::from(synthesis)
        )],
    );
    resolvers.insert(
        "review-verifiers".into(),
        (1..=verifiers.max(1))
            .map(|number| number.to_string())
            .collect(),
    );
    for lens in panel.slots.iter().map(|slot| slot.lens.as_str()).chain([
        "correctness",
        "security",
        "tests",
        "contract",
        "design",
        "adversarial",
    ]) {
        let advice = if lens == "tests"
            && matches!(request.risk_tier.as_deref(), Some("crucial" | "terminal"))
        {
            include_str!("../../../../assets/review-tests-advice.txt")
        } else {
            ""
        };
        resolvers.insert(format!("review-advice-{lens}"), vec![advice.into()]);
    }
    let mut lenses = Vec::with_capacity(panel.slots.len());
    let mut routes = Vec::with_capacity(panel.slots.len());
    for slot in panel.slots {
        let (key, route) = tier_route(&slot.lens, &slot.tier, None, true)?;
        let mut fields = serde_json::Map::new();
        fields.insert(key.into(), route);
        routes.push(serde_json::to_string(&fields).map_err(|error| error.to_string())?);
        lenses.push(slot.lens);
    }
    resolvers.insert("review-panel".into(), lenses);
    resolvers.insert("review-panel-routes".into(), routes);
    Ok(resolvers)
}

/// Create a fenced writer checkout, or verify its clean recovery at the captured source.
pub fn provision_child(
    context: &Context,
    path: &str,
    source: &str,
    cancel: &CancelToken,
) -> Result<(), ServiceError> {
    let branch = path
        .strip_prefix(".worktrees/")
        .ok_or_else(|| refused("writer_worktree", "invalid managed worktree path"))?;
    provision_writer(context, path, branch, source, cancel)
}

/// Provision a graph-owned writer at its checkpointed source commit.
pub fn provision_writer(
    context: &Context,
    path: &str,
    branch: &str,
    source: &str,
    cancel: &CancelToken,
) -> Result<(), ServiceError> {
    provision_writer_checked(context, path, branch, source, cancel).map_err(|error| {
        use provenant_confinement::worktrees::WorktreeError;
        use provenant_fabric_runtime::coordination::facade::DispatchRefusalFact;
        let fact = match &error {
            WorktreeError::Dirty(_) => Some(DispatchRefusalFact::DirtyWriter),
            WorktreeError::DetachedWorktree { .. } | WorktreeError::DetachedPrimary => {
                Some(DispatchRefusalFact::DetachedWriter)
            }
            WorktreeError::CheckedOut => Some(DispatchRefusalFact::CheckedOutWriter),
            WorktreeError::WriterBusy { holder } => {
                return ServiceError::Facade(FacadeError::WriterBusy { holder: *holder });
            }
            _ => None,
        };
        match fact {
            Some(fact) => ServiceError::Facade(FacadeError::DispatchRefused {
                fact: Some(fact),
                disk: None,
                reason: String::new(),
            }),
            None => refused("writer_worktree", error.to_string()),
        }
    })
}

/// Preserve the typed worktree failure for the graph launch owner.
pub fn provision_writer_checked(
    context: &Context,
    path: &str,
    branch: &str,
    source: &str,
    cancel: &CancelToken,
) -> Result<(), provenant_confinement::worktrees::WorktreeError> {
    use provenant_confinement::worktrees;
    worktrees::with_context(
        &context.roots,
        &context.home,
        &context.cwd,
        &provenant_os::clock::SystemClock::new(),
        cancel,
        |worktree_context| provision_in_context(worktree_context, path, branch, source, cancel),
    )
}

fn provision_in_context(
    context: &provenant_confinement::worktrees::WorktreeContext<'_>,
    path: &str,
    branch: &str,
    source: &str,
    cancel: &CancelToken,
) -> Result<(), provenant_confinement::worktrees::WorktreeError> {
    use provenant_confinement::worktrees::{self, WorktreeCreate, WorktreeError};
    if path != format!(".worktrees/{}", branch.replace('/', "-")) {
        return Err(WorktreeError::InvalidRequest);
    }
    let expected = context.repo.root_path().join(path);
    if let Some(existing) = worktrees::list(context, cancel)?
        .into_iter()
        .find(|entry| entry.path == expected)
    {
        let expected_branch = format!("refs/heads/{branch}");
        if existing.branch.as_deref() != Some(expected_branch.as_str())
            || existing.head.to_string() != source
        {
            return Err(WorktreeError::InvalidRequest);
        }
        // Process census is advisory. Reclaim an already provisioned tree only
        // under the writer lease and with terminal custody verified.
        if matches!(
            existing.occupancy,
            worktrees::Occupancy::Unknown {
                source: worktrees::OccupancyUnknownSource::Custody,
                ..
            }
        ) {
            return Err(worktrees::custody_refusal(existing.occupancy));
        }
        let mut tree = provenant_confinement::verify_worktree(context.repo, &existing.path)
            .map_err(|error| WorktreeError::UnknownOccupancy {
                path: existing.path.clone(),
                detail: format!("worktree registration could not be verified: {error}"),
            })?;
        let _writer = worktrees::cache_writer_guard(context.project_state, tree.path()).map_err(
            |occupancy| match occupancy {
                worktrees::Occupancy::Held => WorktreeError::WriterBusy {
                    holder: worktrees::load_owner(&tree)
                        .ok()
                        .flatten()
                        .and_then(|owner| owner.attempt_id),
                },
                other => worktrees::custody_refusal(other),
            },
        )?;
        worktrees::check_writer_terminal(tree.repo())?;
        let reference = provenant_confinement::repo::GitRefName::new(expected_branch)
            .map_err(|_| WorktreeError::IdentityChanged)?;
        let head = tree
            .repo()
            .private_dir()
            .read_stable(std::ffi::OsStr::new("HEAD"), 4096)?;
        let branch_tip = context
            .facts
            .resolve_commit(context, reference.clone(), cancel)?;
        if head != format!("ref: {}\n", reference.as_str()).as_bytes()
            || branch_tip.to_string() != source
        {
            return Err(WorktreeError::IdentityChanged);
        }
        worktrees::require_clean_worktree(context, &mut tree, cancel)?;
        return Ok(());
    }
    worktrees::create(
        context,
        WorktreeCreate {
            branch: branch.into(),
            base: Some(source.into()),
        },
        cancel,
    )
    .map(|_| ())
}

/// Lane side: send `op` to the host relay of the attempt this lane runs as.
/// `provenant run` and `fabric_run` inside a lane call this to add a child.
pub fn from_lane(
    op: &RelayOp,
    cancel: &CancelToken,
    within: std::time::Duration,
) -> Result<Value, ServiceError> {
    let attempt = std::env::var("PROVENANT_PARENT_ATTEMPT_ID")
        .ok()
        .and_then(|id| provenant_core::AttemptId::parse(&id).ok())
        .ok_or_else(|| refused("graph_relay_unavailable", "not running inside a lane"))?;
    provenant_fabric_mcp::lane::graph(
        &provenant_providers::LaneRuntime::fabric_socket(attempt),
        serde_json::to_value(op)?,
        cancel,
        within,
    )
}

/// Wait for this lane's children, yielding its graph concurrency slot. Intake
/// calls this after adding children when a wait is requested. Returns terminal
/// child states, including failures, for the caller to report.
pub fn wait_for_children(
    cancel: &CancelToken,
    within: std::time::Duration,
) -> Result<Value, ServiceError> {
    while_waiting(cancel, within, || {
        poll_children(
            cancel,
            within,
            &provenant_os::clock::SystemClock::new(),
            |remaining| from_lane(&RelayOp::Children, cancel, remaining),
        )
    })
}

fn poll_children(
    cancel: &CancelToken,
    within: std::time::Duration,
    clock: &dyn Clock,
    mut poll: impl FnMut(std::time::Duration) -> Result<Value, ServiceError>,
) -> Result<Value, ServiceError> {
    let started = clock.mono_now();
    loop {
        if cancel.is_cancelled() {
            return Err(refused("cancelled", "child wait cancelled"));
        }
        let remaining = within
            .checked_sub(std::time::Duration::from_micros(
                clock
                    .mono_now()
                    .as_micros()
                    .saturating_sub(started.as_micros()),
            ))
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| refused("timeout", "child wait timed out"))?;
        let value = poll(remaining)?;
        let children = value["children"]
            .as_array()
            .ok_or_else(|| refused("invalid_reply", "missing child states"))?;
        if children.iter().all(|child| child["settled"] == true) {
            return Ok(value);
        }
        provenant_os::sys::wait_pause(remaining.min(std::time::Duration::from_millis(100)));
    }
}

fn template_library(
    home: &std::path::Path,
    repository: &std::path::Path,
) -> Result<TemplateLibrary, String> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    if !config.is_absolute() {
        return Err("XDG_CONFIG_HOME must be absolute".into());
    }
    layered_templates(
        &config.join("provenant/templates"),
        &repository.join(".provenant/templates"),
    )
}

fn layered_templates(
    user: &std::path::Path,
    repository: &std::path::Path,
) -> Result<TemplateLibrary, String> {
    let mut library = TemplateLibrary::bundled();
    library.load_dir(Layer::User, user)?;
    library.load_dir(Layer::Repository, repository)?;
    Ok(library)
}

/// Yield the parent slot for a child wait, then reclaim it on every return path.
/// Cleanup uses its own cancellation token so cancelling the wait still clears
/// the host's retained waiting flag.
pub fn while_waiting<T>(
    cancel: &CancelToken,
    within: std::time::Duration,
    wait: impl FnOnce() -> Result<T, ServiceError>,
) -> Result<T, ServiceError> {
    with_waiting(
        |waiting| {
            let cleanup = CancelToken::new();
            wait_for_admission(|| {
                from_lane(
                    &RelayOp::Wait { waiting },
                    if waiting { cancel } else { &cleanup },
                    within,
                )
            })
        },
        wait,
    )
}

fn wait_for_admission(
    mut send: impl FnMut() -> Result<Value, ServiceError>,
) -> Result<(), ServiceError> {
    while send()?["reclaiming"] == true {
        // Each frame releases the graph lock. Retain the completed child-wait
        // result in with_waiting until the parent has admission again.
        provenant_os::sys::wait_pause(std::time::Duration::from_millis(100));
    }
    Ok(())
}

fn with_waiting<T>(
    mut send: impl FnMut(bool) -> Result<(), ServiceError>,
    wait: impl FnOnce() -> Result<T, ServiceError>,
) -> Result<T, ServiceError> {
    // A failed reply may follow a committed Wait true frame. Always clear it.
    let result = send(true).and_then(|()| wait());
    let cleared = send(false);
    match result {
        Ok(value) => cleared.map(|()| value),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod wait_tests {
    use super::*;

    #[test]
    fn lane_stop_is_confined_to_its_own_subtree() {
        for target in ["parent", "parent--child", "graph/parent--child--grandchild"] {
            assert!(stop_target("graph", "parent", target).is_ok());
        }
        for target in [
            "graph",
            "other",
            "parental",
            "other/parent--child",
            "parent--sibling",
        ] {
            assert!(stop_target("graph", "parent--child", target).is_err());
        }
    }

    #[test]
    fn pending_reclaims_retain_the_completed_child_wait_result() {
        let mut reclaims = 0;
        let mut waits = 0;
        let result = with_waiting(
            |waiting| {
                if waiting {
                    return Ok(());
                }
                wait_for_admission(|| {
                    reclaims += 1;
                    Ok(json!({"reclaiming": reclaims < 3}))
                })
            },
            || {
                waits += 1;
                Ok(json!({"children": [{"node": "parent--child", "result": "saved work"}]}))
            },
        )
        .unwrap();
        assert_eq!(reclaims, 3);
        assert_eq!(waits, 1);
        assert_eq!(result["children"][0]["result"], "saved work");
    }

    #[test]
    fn returning_from_a_child_wait_clears_waiting_on_success_and_error() {
        for succeeds in [true, false] {
            let mut frames = Vec::new();
            let result = with_waiting(
                |waiting| {
                    frames.push(waiting);
                    Ok(())
                },
                || {
                    if succeeds {
                        Ok(42)
                    } else {
                        Err(refused("cancelled", "child wait cancelled"))
                    }
                },
            );
            assert_eq!(frames, [true, false]);
            assert_eq!(result.is_ok(), succeeds);
            match result {
                Ok(value) => assert_eq!(value, 42),
                Err(ServiceError::Cli { code, .. }) => assert_eq!(code, "cancelled"),
                Err(error) => panic!("unexpected error: {error}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_fabric_runtime::graph::template::Compiler;

    #[test]
    fn relay_templates_apply_user_then_repository_layers() {
        let root = tempfile::tempdir().unwrap();
        let user = root.path().join("user");
        let repo = root.path().join("repo");
        std::fs::create_dir(&user).unwrap();
        std::fs::create_dir(&repo).unwrap();
        std::fs::write(
            user.join("custom.toml"),
            "brief = 'User brief'\ntier = 'junior'\naccess = 'read_only'\n",
        )
        .unwrap();
        std::fs::write(
            repo.join("custom.toml"),
            "brief = 'Repository brief'\ntier = 'senior'\naccess = 'read_only'\n",
        )
        .unwrap();
        let library = layered_templates(&user, &root.path().join("missing")).unwrap();
        let node = Compiler::new(&library)
            .compile(&json!({"id":"child", "use":"custom"}))
            .unwrap();
        assert_eq!(node.prompt, "User brief");
        let library = layered_templates(&user, &repo).unwrap();
        let node = Compiler::new(&library)
            .compile(&json!({"id":"child", "use":"custom"}))
            .unwrap();
        assert_eq!(node.prompt, "Repository brief");
        assert!(library.resolve("task").is_ok());
    }
}

#[cfg(test)]
mod child_wait_tests {
    use super::*;

    #[test]
    fn child_wait_polls_until_terminal_and_clears_slot_on_cancellation() {
        for cancelled in [false, true] {
            let cancel = CancelToken::new();
            let mut frames = Vec::new();
            let mut polls = 0;
            let result = with_waiting(
                |waiting| {
                    frames.push(waiting);
                    Ok(())
                },
                || {
                    poll_children(
                        &cancel,
                        std::time::Duration::from_secs(2),
                        &provenant_os::clock::SystemClock::new(),
                        |_| {
                            polls += 1;
                            if cancelled {
                                cancel.cancel();
                            }
                            Ok(json!({"children":[{"node":"a--b", "settled":polls == 2}]}))
                        },
                    )
                },
            );
            assert_eq!(frames, [true, false]);
            assert_eq!(result.is_ok(), !cancelled);
            assert_eq!(polls, if cancelled { 1 } else { 2 });
        }
    }

    #[test]
    fn uncertain_yield_and_timeout_still_clear_the_slot() {
        let mut frames = Vec::new();
        let result: Result<(), _> = with_waiting(
            |waiting| {
                frames.push(waiting);
                if waiting {
                    Err(refused("io", "lost reply"))
                } else {
                    Ok(())
                }
            },
            || panic!("must not wait after uncertain yield"),
        );
        assert!(result.is_err());
        assert_eq!(frames, [true, false]);
        let result = with_waiting(
            |_| Ok(()),
            || {
                poll_children(
                    &CancelToken::new(),
                    std::time::Duration::ZERO,
                    &provenant_os::clock::SystemClock::new(),
                    |_| panic!("expired"),
                )
            },
        );
        assert!(matches!(result, Err(ServiceError::Cli { code, .. }) if code == "timeout"));
    }
}

#[cfg(test)]
mod provisioning_tests {
    use super::*;

    #[test]
    fn relay_refusal_cause_reaches_the_lane_without_raw_source_text()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let socket = root.path().join("relay/socket");
        let _host = provenant_fabric_mcp::lane::HostRelay::start_with_graph(
            socket.clone(),
            Default::default(),
            |_, _, _| Err(ServiceError::FabricOwnerUnavailable),
            |_, _, _| Err(refused("writer_parent_required", "PROMPT_SENTINEL")),
        )?;
        let error = provenant_fabric_mcp::lane::graph(
            &socket,
            json!({"op":"children"}),
            &CancelToken::new(),
            std::time::Duration::from_secs(5),
        )
        .expect_err("writer parent refusal");
        assert_eq!(error.code(), "writer_parent_required");
        let wire = error.structured_error();
        assert!(
            wire["message"]
                .as_str()
                .is_some_and(|message| message.contains("registered parent worktree"))
        );
        assert!(!wire.to_string().contains("PROMPT_SENTINEL"));
        Ok(())
    }

    #[test]
    fn writer_child_worktree_is_created_at_source_and_dirty_reuse_is_refused()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        let project = home.join("project");
        std::fs::create_dir_all(&project)?;
        for args in [
            vec!["init", "-q"],
            vec!["commit", "--allow-empty", "-qm", "base"],
        ] {
            let output = provenant_os::fixture::git_command()
                .current_dir(&project)
                .args(args)
                .output()?;
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let output = provenant_os::fixture::git_command()
            .current_dir(&project)
            .args(["rev-parse", "HEAD"])
            .output()?;
        assert!(output.status.success());
        let source = String::from_utf8(output.stdout)?.trim().to_owned();
        let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
        inputs.state_dir = Some(root.path().join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let context = Context::open(roots, &home, &project)?;
        let cancel = CancelToken::new();
        // Explicit top-level writer branches use the same managed path rules
        // as worktree creation, preserving slashes in the Git branch itself.
        provision_writer(
            &context,
            ".worktrees/topic-fix",
            "topic/fix",
            &source,
            &cancel,
        )?;
        let branch = provenant_os::fixture::git_command()
            .current_dir(project.join(".worktrees/topic-fix"))
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()?;
        assert!(branch.status.success());
        assert_eq!(String::from_utf8(branch.stdout)?.trim(), "topic/fix");
        assert!(
            provision_writer(&context, ".worktrees/topic-fix", "other", &source, &cancel).is_err()
        );
        let path = ".worktrees/graph-parent--child";
        provision_child(&context, path, &source, &cancel)?;
        let output = provenant_os::fixture::git_command()
            .current_dir(project.join(path))
            .args(["rev-parse", "HEAD"])
            .output()?;
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout)?.trim(), source);
        struct Census(provenant_confinement::worktrees::Occupancy);
        impl provenant_confinement::worktrees::WorkerObserver for Census {
            fn observe_roots(
                &self,
                roots: &[std::path::PathBuf],
            ) -> std::collections::BTreeMap<
                std::path::PathBuf,
                provenant_confinement::worktrees::Occupancy,
            > {
                roots
                    .iter()
                    .cloned()
                    .map(|root| (root, self.0.clone()))
                    .collect()
            }
        }
        let census = Census(provenant_confinement::worktrees::Occupancy::Available);
        let provision = || {
            provenant_confinement::worktrees::with_context_using_workers(
                &context.roots,
                &context.home,
                &context.cwd,
                &provenant_os::clock::SystemClock::new(),
                &cancel,
                &census,
                |ctx| {
                    provision_in_context(
                        ctx,
                        path,
                        path.trim_start_matches(".worktrees/"),
                        &source,
                        &cancel,
                    )
                },
            )
        };
        provision()?;
        for occupancy in [
            provenant_confinement::worktrees::Occupancy::Observed { process: None },
            provenant_confinement::worktrees::Occupancy::census_unknown(
                project.join(path),
                "fixture incomplete census",
            ),
        ] {
            provenant_confinement::worktrees::with_context_using_workers(
                &context.roots,
                &context.home,
                &context.cwd,
                &provenant_os::clock::SystemClock::new(),
                &cancel,
                &Census(occupancy),
                |ctx| {
                    provision_in_context(
                        ctx,
                        path,
                        path.trim_start_matches(".worktrees/"),
                        &source,
                        &cancel,
                    )?;
                    let _held = provenant_confinement::worktrees::cache_writer_guard(
                        ctx.project_state,
                        &project.join(path),
                    )
                    .map_err(provenant_confinement::worktrees::custody_refusal)?;
                    let held = provision_in_context(
                        ctx,
                        path,
                        path.trim_start_matches(".worktrees/"),
                        &source,
                        &cancel,
                    );
                    assert!(
                        matches!(
                            held,
                            Err(provenant_confinement::worktrees::WorktreeError::WriterBusy { .. })
                        ),
                        "held custody must refuse reuse: {held:?}"
                    );
                    Ok(())
                },
            )?;
        }
        std::fs::write(project.join(path).join("parent-work"), "preserve")?;
        assert!(provision().is_err());
        assert_eq!(
            std::fs::read_to_string(project.join(path).join("parent-work"))?,
            "preserve"
        );
        Ok(())
    }

    #[test]
    fn readonly_parent_add_and_edit_provision_writers_at_the_pinned_graph_source()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_fabric_runtime::graph::{GraphCheckpoint, GraphDocument, GraphExecution};
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        let project = home.join("project");
        std::fs::create_dir_all(&project)?;
        let git = |args: &[&str]| -> Result<String, Box<dyn std::error::Error>> {
            let output = provenant_os::fixture::git_command()
                .current_dir(&project)
                .args(args)
                .output()?;
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            Ok(String::from_utf8(output.stdout)?.trim().to_owned())
        };
        git(&["init", "-q"])?;
        git(&["commit", "--allow-empty", "-qm", "base"])?;
        let source = git(&["rev-parse", "HEAD"])?;
        let branch = git(&["symbolic-ref", "--short", "HEAD"])?;
        let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
        inputs.state_dir = Some(root.path().join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let context = Context::open(roots, &home, &project)?;
        let cancel = CancelToken::new();
        let clock = provenant_os::clock::SystemClock::new();
        let budget = WaitBudget::cli(clock.mono_now(), 30)?;
        let document: GraphDocument = serde_json::from_value(json!({
            "schema_version":2,"graph_id":"g-writers","integration_branch":branch,"template_library":{},
            "nodes":[{"id":"parent","access":"read_only","prompt":"Go","route":{"alias":"senior"}}]
        }))?;
        let reference = format!("{}/caller", context.identity.host);
        let checkpoint: GraphCheckpoint = serde_json::from_value(json!({
            "schema_version":1,"graph_id":"g-writers","plan_digest":"fixture","bound_integration_base":source,
            "nodes":[{"id":"parent","state":"running","worktree":null,"attempts":[{
                "launched":true,"key":{"graph_id":"g-writers","node_id":"parent","route_index":0},
                "route":{"alias":"senior"},"state":"running","attempt_ref":reference,"outcome":null,"launch_actor":"fixture"
            }]}]
        }))?;
        {
            let mut journal = FileJournal::open(
                &context.roots.state,
                context.identity.project,
                "g-writers",
                budget,
                &cancel,
                &clock,
            )?;
            journal.bind(GraphExecution {
                review_base: None,
                extra: Default::default(),
                integration_base: Some(source.parse()?),
                directory: project.clone(),
                document,
            })?;
            journal.save(&checkpoint)?;
        }
        git(&["commit", "--allow-empty", "-qm", "move host head"])?;
        // Remote-only names must not override the children's captured source.
        let remote = root.path().join("origin.git");
        let remote = remote.to_str().ok_or("remote path")?;
        git(&["clone", "--bare", ".", remote])?;
        for child in ["writer", "edited"] {
            git(&[
                "--git-dir",
                remote,
                "branch",
                &format!("g-writers-parent--{child}"),
                "HEAD",
            ])?;
        }
        git(&["remote", "add", "origin", remote])?;
        git(&["checkout", "--detach", "-q"])?;
        let request = json!({"op":"add","id":"writer","use":"implement","brief":"Write","writer":true,"write_scope":["src"],"idempotency_key":"write"});
        let receipt = serve(&context, "caller", request.clone(), budget, &cancel, &clock)?;
        let path = receipt["worktree"].as_str().ok_or("missing worktree")?;
        assert_eq!(git(&["-C", path, "rev-parse", "HEAD"])?, source);
        let replay = serve(&context, "caller", request, budget, &cancel, &clock)?;
        assert_eq!(replay["node"], receipt["node"]);
        assert_eq!(replay["replayed"], true);
        let edit = json!({"op":"edit","id":"g-writers","patch":{"schema_version":1,"idempotency_key":"edit-writer","nodes":[{
            "id":"edited","use":"implement","brief":"Edit","write_scope":["other"]
        }]}});
        let _ = serve(&context, "caller", edit, budget, &cancel, &clock)?;
        assert_eq!(
            git(&[
                "-C",
                ".worktrees/g-writers-parent--edited",
                "rev-parse",
                "HEAD"
            ])?,
            source
        );
        Ok(())
    }

    #[test]
    fn lost_child_receipt_replays_after_graph_settlement() -> Result<(), Box<dyn std::error::Error>>
    {
        use provenant_fabric_runtime::graph::{
            AttemptState, GraphCheckpoint, GraphExecution, NodeState,
        };
        for terminal in [NodeState::Succeeded, NodeState::Skipped] {
            let root = tempfile::tempdir()?;
            let home = root.path().join("home");
            let project = home.join("project");
            std::fs::create_dir_all(&project)?;
            for args in [
                vec!["init", "-q"],
                vec!["commit", "--allow-empty", "-qm", "base"],
            ] {
                let output = provenant_os::fixture::git_command()
                    .current_dir(&project)
                    .args(args)
                    .output()?;
                assert!(output.status.success());
            }
            let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
            inputs.state_dir = Some(root.path().join("state"));
            let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
            let context = Context::open(roots, &home, &project)?;
            let cancel = CancelToken::new();
            let clock = provenant_os::clock::SystemClock::new();
            let budget = WaitBudget::cli(clock.mono_now(), 5)?;
            let reference = format!("{}/caller", context.identity.host);
            let checkpoint: GraphCheckpoint = serde_json::from_value(json!({
                "schema_version":1,"graph_id":"g-lost-receipt","plan_digest":"fixture",
                "nodes":[{"id":"parent","state":"running","worktree":null,"attempts":[{
                    "launched":true,"key":{"graph_id":"g-lost-receipt","node_id":"parent","route_index":0},
                    "route":{"alias":"senior"},"state":"running","attempt_ref":reference,
                    "outcome":null,"launch_actor":"fixture"
                }]}]
            }))?;
            let open = || {
                FileJournal::open(
                    &context.roots.state,
                    context.identity.project,
                    "g-lost-receipt",
                    budget,
                    &cancel,
                    &clock,
                )
            };
            {
                let mut journal = open()?;
                journal.bind(GraphExecution {
                    review_base: None,
                    extra: Default::default(),
                    integration_base: None,
                    directory: project.clone(),
                    document: serde_json::from_value(json!({
                        "schema_version":1,"graph_id":"g-lost-receipt","template_library":{},
                        "nodes":[{"id":"parent","access":"read_only","prompt":"Go","route":{"alias":"senior"}}]
                    }))?,
                })?;
                journal.save(&checkpoint)?;
            }
            let request = json!({"op":"add","id":"child","use":"task","brief":"Read", "idempotency_key":"lost-reply"});
            let receipt = serve(&context, "caller", request.clone(), budget, &cancel, &clock)?;
            let settled = {
                let mut journal = open()?;
                let mut checkpoint = journal.load("g-lost-receipt")?.ok_or("checkpoint")?;
                for node in &mut checkpoint.nodes {
                    node.state = terminal;
                    for attempt in &mut node.attempts {
                        attempt.state = AttemptState::Terminal;
                    }
                }
                checkpoint.cancelled = terminal == NodeState::Skipped;
                journal.save(&checkpoint)?;
                checkpoint
            };
            assert!(
                FileJournal::pending_graph_ids(&context.roots.state, context.identity.project)?
                    .is_empty()
            );
            let replay = serve(&context, "caller", request.clone(), budget, &cancel, &clock)?;
            assert_eq!(replay["node"], receipt["node"]);
            assert_eq!(replay["revision"], receipt["revision"]);
            assert_eq!(replay["replayed"], true);
            assert_eq!(open()?.load("g-lost-receipt")?, Some(settled));
            let mut changed = request.clone();
            changed["brief"] = json!("Different");
            assert!(
                matches!(serve(&context, "caller", changed, budget, &cancel, &clock),
                Err(ServiceError::Cli { code, .. }) if code == "idempotency_conflict")
            );
            let mut fresh = request;
            fresh["idempotency_key"] = json!("new-request");
            assert!(
                matches!(serve(&context, "caller", fresh, budget, &cancel, &clock),
                Err(error) if error.code() == "graph_relay_unavailable")
            );
        }
        Ok(())
    }

    #[test]
    fn mixed_legacy_state_allows_receipt_replay_but_refuses_new_child_admission()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_fabric_runtime::graph::{GraphCheckpoint, GraphExecution};
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        let project = home.join("project");
        std::fs::create_dir_all(&project)?;
        let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
        inputs.state_dir = Some(root.path().join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let context = Context::open(roots, &home, &project)?;
        let cancel = CancelToken::new();
        let clock = provenant_os::clock::SystemClock::new();
        let budget = WaitBudget::cli(clock.mono_now(), 5)?;
        let reference = format!("{}/caller", context.identity.host);
        let checkpoint: GraphCheckpoint = serde_json::from_value(json!({
            "schema_version":1,"graph_id":"current","plan_digest":"fixture",
            "nodes":[{"id":"parent","state":"running","worktree":null,"attempts":[{
                "launched":true,"key":{"graph_id":"current","node_id":"parent","route_index":0},
                "route":{"alias":"senior"},"state":"running","attempt_ref":reference,
                "outcome":null,"launch_actor":"fixture"
            }]}]
        }))?;
        {
            let mut journal = FileJournal::open(
                &context.roots.state,
                context.identity.project,
                "current",
                budget,
                &cancel,
                &clock,
            )?;
            journal.bind(GraphExecution {
                review_base: None, extra: Default::default(), integration_base: None,
                directory: project.clone(),
                document: serde_json::from_value(json!({
                    "schema_version":1,"graph_id":"current","template_library":{},
                    "nodes":[{"id":"parent","access":"read_only","prompt":"Go","route":{"alias":"senior"}}]
                }))?,
            })?;
            journal.save(&checkpoint)?;
        }
        let request = json!({"op":"add","id":"child","use":"task","brief":"Read","idempotency_key":"receipt"});
        let receipt = serve(&context, "caller", request.clone(), budget, &cancel, &clock)?;
        std::fs::remove_file(
            context
                .roots
                .state
                .as_path()
                .join("graph-template-cutover-complete"),
        )?;
        {
            let mut journal = FileJournal::open(
                &context.roots.state,
                context.identity.project,
                "legacy",
                budget,
                &cancel,
                &clock,
            )?;
            journal.bind(GraphExecution {
                review_base: None, extra: Default::default(), integration_base: None,
                directory: project.clone(),
                document: serde_json::from_value(json!({"schema_version":1,"graph_id":"legacy","nodes":[{"id":"old","access":"read_only","prompt":"Old","route":{"alias":"senior"}}]}))?,
            })?;
            journal.save(&serde_json::from_value(json!({"schema_version":1,"graph_id":"legacy","plan_digest":"fixture","nodes":[{"id":"old","state":"pending","worktree":null,"attempts":[]}]}))?)?;
        }
        assert!(
            provenant_fabric_runtime::graph::cutover::pending_guard(&context.roots.state).is_err()
        );
        let before = FileJournal::read_checkpoint(
            &context.roots.state,
            context.identity.project,
            "current",
        )?;
        let replay = serve(&context, "caller", request.clone(), budget, &cancel, &clock)?;
        assert_eq!(replay["node"], receipt["node"]);
        assert_eq!(replay["revision"], receipt["revision"]);
        assert_eq!(replay["replayed"], true);
        let mut fresh = request;
        fresh["idempotency_key"] = json!("new-child");
        for request in [
            fresh,
            json!({"op":"edit","id":"current","patch":{"schema_version":1,"idempotency_key":"new-edit","nodes":[{"id":"another","use":"task","brief":"More"}]}}),
        ] {
            let error = serve(&context, "caller", request, budget, &cancel, &clock)
                .expect_err("new relay work must be refused");
            assert_eq!(error.code(), "UPGRADE_DRAIN_REQUIRED");
            let ServiceError::Facade(error) = error else {
                return Err("expected a facade refusal".into());
            };
            assert!(
                error
                    .diagnostic()
                    .detail
                    .unwrap_or_default()
                    .contains("/legacy")
            );
            assert_eq!(
                FileJournal::read_checkpoint(
                    &context.roots.state,
                    context.identity.project,
                    "current"
                )?,
                before
            );
        }
        Ok(())
    }

    #[test]
    fn lane_edit_refuses_skip_and_replace_outside_its_subtree()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_fabric_runtime::graph::{GraphCheckpoint, GraphDocument, GraphExecution};
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        let project = home.join("project");
        std::fs::create_dir_all(&project)?;
        let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
        inputs.state_dir = Some(root.path().join("state"));
        let context = Context::open(
            inputs.resolve(&provenant_os::fs::SystemPathResolver)?,
            &home,
            &project,
        )?;
        let cancel = CancelToken::new();
        let clock = provenant_os::clock::SystemClock::new();
        let budget = WaitBudget::cli(clock.mono_now(), 5)?;
        let document: GraphDocument = serde_json::from_value(json!({
            "schema_version":1,"graph_id":"edit-scope",
            "nodes":[
                {"id":"writer","access":"read_only","prompt":"Write","route":{"alias":"senior"}},
                {"id":"review","access":"read_only","prompt":"Review","route":{"alias":"senior"}}
            ]
        }))?;
        let checkpoint: GraphCheckpoint = serde_json::from_value(json!({
            "schema_version":1,"graph_id":"edit-scope","plan_digest":"fixture",
            "nodes":[
                {"id":"writer","state":"running","worktree":null,"attempts":[{
                    "launched":true,"key":{"graph_id":"edit-scope","node_id":"writer","route_index":0},
                    "route":{"alias":"senior"},"state":"running",
                    "attempt_ref":format!("{}/caller",context.identity.host),"outcome":null,"launch_actor":"fixture"
                }]},
                {"id":"review","state":"pending","worktree":null,"attempts":[]}
            ]
        }))?;
        {
            let mut journal = FileJournal::open(
                &context.roots.state,
                context.identity.project,
                "edit-scope",
                budget,
                &cancel,
                &clock,
            )?;
            journal.bind(GraphExecution {
                review_base: None,
                extra: Default::default(),
                integration_base: None,
                directory: project.clone(),
                document,
            })?;
            journal.save(&checkpoint)?;
        }
        for edit in [
            json!({"skip":["review"]}),
            json!({"replace":[{"id":"review","access":"read_only","prompt":"Bypass review","route":{"alias":"senior"}}]}),
        ] {
            let mut patch = edit;
            patch["schema_version"] = json!(1);
            let result = serve(
                &context,
                "caller",
                json!({"op":"edit","id":"edit-scope","patch":patch}),
                budget,
                &cancel,
                &clock,
            );
            assert!(matches!(result, Err(ServiceError::Cli { code, .. }) if code == "edit_scope"));
            let saved = FileJournal::read_checkpoint(
                &context.roots.state,
                context.identity.project,
                "edit-scope",
            )?
            .ok_or("missing checkpoint")?;
            assert_eq!(saved, checkpoint);
        }
        Ok(())
    }

    #[test]
    fn crash_after_the_fence_is_recovered_by_the_next_serve()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_fabric_runtime::graph::{GraphCheckpoint, GraphExecution};
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        let project = home.join("project");
        std::fs::create_dir_all(&project)?;
        let git = |args: &[&str]| -> Result<String, Box<dyn std::error::Error>> {
            let output = provenant_os::fixture::git_command()
                .current_dir(&project)
                .args(args)
                .output()?;
            assert!(output.status.success());
            Ok(String::from_utf8(output.stdout)?.trim().to_owned())
        };
        git(&["init", "-q"])?;
        git(&["commit", "--allow-empty", "-qm", "base"])?;
        let source = git(&["rev-parse", "HEAD"])?;
        let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
        inputs.state_dir = Some(root.path().join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let context = Context::open(roots, &home, &project)?;
        let cancel = CancelToken::new();
        let clock = provenant_os::clock::SystemClock::new();
        let budget = WaitBudget::cli(clock.mono_now(), 5)?;
        let mut journal = FileJournal::open(
            &context.roots.state,
            context.identity.project,
            "g-crash",
            budget,
            &cancel,
            &clock,
        )?;
        let document = serde_json::from_value(json!({
            "schema_version":1, "graph_id":"g-crash",
            "nodes":[{"id":"a","access":"read_only","prompt":"Go"}]
        }))?;
        journal.bind(GraphExecution {
            review_base: None,
            extra: Default::default(),
            integration_base: None,
            directory: project.clone(),
            document,
        })?;
        // The crash: the fence was checkpointed, then the process died before
        // any Git effect.
        let path = ".worktrees/g-crash-a--w";
        let mut checkpoint: GraphCheckpoint = serde_json::from_value(
            json!({"schema_version":1, "graph_id":"g-crash", "plan_digest":"fixture", "nodes":[]}),
        )?;
        relay::fence_provisioning(&mut checkpoint, "a--w", path, &source);
        journal.save(&checkpoint)?;
        assert!(!project.join(path).exists());
        // Process death releases the journal lock. Recover using a fresh host.
        drop(journal);
        let mut journal = FileJournal::open(
            &context.roots.state,
            context.identity.project,
            "g-crash",
            budget,
            &cancel,
            &clock,
        )?;
        let mut reloaded = journal.load("g-crash")?.expect("checkpoint");
        recover_provisioning(&context, &mut journal, &mut reloaded, &cancel)?;
        assert_eq!(git(&["-C", path, "rev-parse", "HEAD"])?, source);
        assert!(relay::pending_provisioning(&reloaded).is_empty());
        let saved = journal.load("g-crash")?.expect("checkpoint");
        assert!(relay::pending_provisioning(&saved).is_empty());

        // Git finished, but the host crashed before saving the cleared fence.
        // Reusing the same path must not allocate another tree or branch.
        relay::fence_provisioning(&mut reloaded, "a--w", path, &source);
        journal.save(&reloaded)?;
        drop(journal);
        let before = git(&["worktree", "list", "--porcelain"])?;
        let mut journal = FileJournal::open(
            &context.roots.state,
            context.identity.project,
            "g-crash",
            budget,
            &cancel,
            &clock,
        )?;
        let mut reloaded = journal.load("g-crash")?.expect("checkpoint");
        recover_provisioning(&context, &mut journal, &mut reloaded, &cancel)?;
        assert_eq!(git(&["worktree", "list", "--porcelain"])?, before);
        assert_eq!(git(&["-C", path, "rev-parse", "HEAD"])?, source);
        let saved = journal.load("g-crash")?.expect("checkpoint");
        assert!(relay::pending_provisioning(&saved).is_empty());
        Ok(())
    }
}

#[cfg(test)]
mod panel_tests {
    use super::*;

    #[test]
    fn relay_review_resolver_uses_graph_risk_and_tags() {
        let review = provenant_routing::RoutingConfig::default().review;
        let mut document: provenant_fabric_runtime::graph::GraphDocument =
            serde_json::from_value(json!({
                "schema_version": 1, "graph_id": "panel-risk",
                "nodes": [{"id": "anchor", "access": "read_only", "prompt": "Graph task", "route": {"alias": "senior"}}],
                "context": {"risk_tier": "routine"},
            }))
            .unwrap();
        let routine = launch_resolvers(&review, &document).unwrap();
        assert!(
            routine["review-panel"]
                .iter()
                .any(|lens| lens == "correctness-spec")
        );
        assert!(!routine["review-panel"].iter().any(|lens| lens == "tests"));
        document.context.as_mut().unwrap().risk_tier = Some("crucial".into());
        document
            .extra
            .insert("review".into(), json!({"risk_tags": ["security"]}));
        let crucial = launch_resolvers(&review, &document).unwrap();
        assert!(crucial["review-panel"].iter().any(|lens| lens == "tests"));
        assert!(
            crucial["review-panel"]
                .iter()
                .any(|lens| lens == "security-privacy")
        );
        let library = TemplateLibrary::bundled();
        let limits = provenant_fabric_runtime::config::GraphLimits::default();
        let mut checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint =
            serde_json::from_value(json!({
                "schema_version": 1, "graph_id": "panel-risk", "plan_digest": "fixture", "nodes": [],
            })).unwrap();
        let patch = serde_json::from_value(json!({
            "schema_version": 1,
            "expected_revision": 0,
            "idempotency_key": "review-child",
            "nodes": [{"id": "review", "use": "review-panel"}],
        }))
        .unwrap();
        provenant_fabric_runtime::graph::patch::admit_patch(
            &mut LiveGraph {
                document: &document,
                checkpoint: &mut checkpoint,
            },
            None,
            &patch,
            &PatchContext {
                library: &library,
                resolvers: &crucial,
                limits: &limits,
                scan: &|_| Ok(()),
                journal: None,
            },
        )
        .expect("relay resolvers expand the complete review composite");
    }
}
