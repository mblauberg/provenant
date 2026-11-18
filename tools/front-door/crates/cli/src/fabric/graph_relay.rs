//! Host side of a lane's graph frames.
//!
//! The relay serves one attempt, so the host finds the running node that owns
//! that attempt and acts as it: the lane never names its graph or parent.
//! Admission takes the graph's journal lock, goes through
//! `provenant_fabric_runtime::graph::relay#admit_child` and saves the
//! checkpoint before the receipt is returned, so a lost reply is answered by
//! replaying the same idempotency key.
use super::Context;
use provenant_core::{CancelToken, Clock, Pause, WaitBudget};
use provenant_fabric_runtime::graph::{
    FileJournal, GraphJournal,
    patch::{LiveGraph, PatchContext},
    relay::{self, RelayOp},
    template::{Layer, TemplateLibrary},
};
use provenant_fabric_runtime::relay::{FacadeError, ServiceError};
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
        "cycle" => Some(Cause::Cycle),
        "conflict" | "stale_revision" | "idempotency_conflict" => {
            return ServiceError::Cli {
                code: code.into(),
                message: FacadeError::GraphRelayRefused {
                    refusal_code: code.into(),
                    detail: Some(InvalidInputDetail::GraphRelayCause(Cause::Conflict)),
                }
                .diagnostic()
                .detail
                .unwrap_or_default(),
                data: Value::Null,
            };
        }
        "invalid" if message.ends_with("writer worktree") => Some(Cause::WriterWorktree),
        "invalid"
            if message == "writer composite children require explicit per-node work bindings" =>
        {
            Some(Cause::WriterComposite)
        }
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
    if let Some(candidate) = error.candidate {
        return ServiceError::Facade(FacadeError::graph_candidate(*candidate));
    }
    match error.admission {
        Some(admission) => ServiceError::Facade(FacadeError::graph_admission(*admission)),
        None => refused(error.code, error.message),
    }
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
    let mutating_input = matches!(arguments["op"].as_str(), Some("add" | "edit" | "run"));
    let policy = if mutating_input {
        let config = crate::config_reads::read_for(&context.roots, "run admission")
            .map_err(|_| journal_error(()))?;
        config.domains.policy.resolve_with_knobs(
            &context.identity.project.to_string(),
            &config.knobs,
            &Default::default(),
        )
    } else {
        Default::default()
    };
    let mut warnings = if mutating_input {
        provenant_fabric_runtime::graph::refusal::input_warnings(&arguments, &policy)
    } else {
        Vec::new()
    };
    let mut op: RelayOp = serde_json::from_value(arguments)
        .map_err(|error| refused("invalid_input", format!("invalid graph frame: {error}")))?;
    // The lane has already started the launch allowance. This confirmed
    // remainder includes queue time; never renew it at the host.
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
    let mut run_request = None;
    if let RelayOp::Run { request_id, input } = op {
        let identity = relay::RunRequestIdentity::for_input(&input).map_err(journal_error)?;
        if request_id != identity.id {
            return Err(refused(
                "invalid_input",
                "run request identity does not match launch input",
            ));
        }
        let request = (identity.id, identity.digest);
        let checkpoint = FileJournal::read_status_checkpoint(state, project, &graph_id)
            .map_err(journal_error)?
            .ok_or_else(not_a_node)?;
        let parent =
            relay::historical_parent_of_attempt(&checkpoint, &reference).ok_or_else(not_a_node)?;
        if let Some(receipt) = replay_run_request(&checkpoint, parent, &request)? {
            let mut value = serde_json::to_value(receipt)?;
            if !warnings.is_empty() {
                value["warnings"] = json!(warnings);
            }
            return Ok(value);
        }
        if relay::parent_of_attempt(&checkpoint, &reference).is_none() {
            return Err(not_a_node());
        }
        // Host lookup runs only for an active parent, outside the journal lock.
        // Normal Add admission below checks the parent again after preparation.
        let prepared = read(json!({"op":"prepare_child","input":input}))?;
        let child = serde_json::from_value(prepared)
            .map_err(|_| refused("invalid_input", "invalid host-prepared child"))?;
        run_request = Some(request);
        op = RelayOp::Add(child);
    }
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
        // Inside a lane a target names the calling graph's own nodes, as
        // `NAME[/NODE[#N]]` or a bare `NODE[#N]` (cli.md "Targets"). Attempt
        // IDs are not targets, so they resolve to nothing here.
        let selected = if requested == graph_id {
            graph_id.clone()
        } else {
            let local = requested
                .strip_prefix(&format!("{graph_id}/"))
                .unwrap_or(requested);
            let node = local.split_once('#').map_or(local, |(node, _)| node);
            checkpoint
                .nodes
                .iter()
                .find(|row| row.id == node)
                .ok_or_else(|| refused("not_found", "the target is outside the calling lane's graph; use NAME/NODE from its launch receipt"))?;
            format!("{graph_id}/{local}")
        };
        input["id"] = selected.into();
        return read(input);
    }
    let mut journal = match FileJournal::open(state, project, &graph_id, budget, cancel, clock) {
        Ok(journal) => journal,
        Err(error)
            if error.is_lock_timeout() && matches!(&op, RelayOp::Wait { waiting: false }) =>
        {
            return Ok(json!({"graph_id": graph_id, "waiting": true, "reclaiming": true}));
        }
        Err(error) => return Err(journal_error(error)),
    };
    let mut checkpoint = journal
        .load(&graph_id)
        .map_err(journal_error)?
        .ok_or_else(not_a_node)?;
    if let Some(request) = &run_request {
        let parent =
            relay::historical_parent_of_attempt(&checkpoint, &reference).ok_or_else(not_a_node)?;
        // Preparation runs without the lock; another caller may have admitted
        // this raw request in the meantime, with different resolved pins.
        if let Some(receipt) = replay_run_request(&checkpoint, parent, request)? {
            let mut value = serde_json::to_value(receipt)?;
            if !warnings.is_empty() {
                value["warnings"] = json!(warnings);
            }
            return Ok(value);
        }
    }
    let document = FileJournal::execution(state, project, &graph_id)
        .map_err(journal_error)?
        .ok_or_else(not_a_node)?
        .document;
    // Reconcile older Git effects without making their refusal a refusal of
    // this unrelated operation. Unresolved effects retain their fence.
    let _ = recover_provisioning(context, &mut journal, &mut checkpoint, &[], budget, cancel)?;
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
                Some(receipt) => {
                    if let Some(request) = &run_request {
                        retain_run_request(&mut checkpoint, &receipt, request)?;
                        journal.save(&checkpoint).map_err(journal_error)?;
                    }
                    let mut value = serde_json::to_value(receipt)?;
                    if !warnings.is_empty() {
                        value["warnings"] = json!(warnings);
                    }
                    Ok(value)
                }
                None => Err(not_a_node()),
            };
        }
        (None, _) => return Err(not_a_node()),
    };
    let mut value = match op {
        RelayOp::Read { .. } | RelayOp::Run { .. } => {
            unreachable!("host requests are resolved before journal locking")
        }
        RelayOp::Add(request) => {
            if let Some(receipt) = relay::replay_child(&document, &checkpoint, &parent, &request)
                .map_err(patch_refused)?
            {
                if let Some(request) = &run_request {
                    retain_run_request(&mut checkpoint, &receipt, request)?;
                    journal.save(&checkpoint).map_err(journal_error)?;
                }
                let mut value = serde_json::to_value(receipt)?;
                if !warnings.is_empty() {
                    value["warnings"] = json!(warnings);
                }
                return Ok(value);
            }
            let library = template_library(&context.home, &context.cwd)
                .map_err(|error| refused("invalid_template", error))?;
            let config = crate::config_reads::read_for(&context.roots, "run")
                .map_err(|error| refused("invalid_config", error.to_string()))?;
            let review_subjects = relay_child_review_subjects(&request, &library)
                .map_err(|error| refused("invalid_template", error))?;
            let resolvers = launch_resolvers_for_relay(
                &context.roots,
                &context.cwd,
                &config.domains.routing.review,
                &document,
                &checkpoint,
                &review_subjects,
            )
            .map_err(|error| refused("invalid_review", error))?;
            let patch_context = PatchContext {
                journal: None,
                library: &library,
                resolvers: &resolvers,

                limits: &context.config.graph.limits,
                worktree_layout: context.port_table.worktree_layout.clone(),
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
                    &checkpoint
                        .bases
                        .and_then(|bases| bases.integration)
                        .map(|base| base.to_string()),
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
                        checkpoint
                            .bases
                            .and_then(|bases| bases.integration)
                            .map(|base| base.to_string())
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
            if let Some(expansion) = checkpoint.expansions.last() {
                warnings.extend(provenant_fabric_runtime::graph::refusal::input_warnings(
                    &serde_json::to_value(expansion)?,
                    &policy,
                ));
            }
            if let Some(request) = &run_request {
                retain_run_request(&mut checkpoint, &receipt, request)?;
            }
            if receipt.replayed {
                if run_request.is_some() {
                    journal.save(&checkpoint).map_err(journal_error)?;
                }
                // Nothing changed; the earlier admission is already journalled.
                let mut value = serde_json::to_value(receipt)?;
                if !warnings.is_empty() {
                    value["warnings"] = json!(warnings);
                }
                return Ok(value);
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
                // Reserve journal persistence and response delivery within the
                // confirmed caller deadline, as the edit path does.
                let now = clock.mono_now();
                let remaining = budget.remaining(now).as_micros();
                let reserve = 1_000_000.min(remaining / 2);
                let provisioning_budget = WaitBudget::up_to(
                    now,
                    provenant_core::DurationMicros::from_micros(remaining - reserve),
                );
                if let Some(error) = recover_provisioning(
                    context,
                    &mut journal,
                    &mut checkpoint,
                    std::slice::from_ref(&receipt.node),
                    provisioning_budget,
                    cancel,
                )? {
                    return Err(error);
                }
            }
            provision_added_writers(context, &mut journal, &mut checkpoint, &[], budget, cancel)?;
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
            let config = crate::config_reads::read_for(&context.roots, "run")
                .map_err(|error| refused("invalid_config", error.to_string()))?;
            let review_subjects = relay_patch_review_subjects(&patch, &library)
                .map_err(|error| refused("invalid_template", error))?;
            let resolvers = launch_resolvers_for_relay(
                &context.roots,
                &context.cwd,
                &config.domains.routing.review,
                &document,
                &checkpoint,
                &review_subjects,
            )
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

                    limits: &context.config.graph.limits,
                    worktree_layout: context.port_table.worktree_layout.clone(),
                },
            )
            .map_err(patch_refused)?;
            if let Some(expansion) = checkpoint.expansions.last() {
                warnings.extend(provenant_fabric_runtime::graph::refusal::input_warnings(
                    &serde_json::to_value(expansion)?,
                    &policy,
                ));
            }
            // Leave time to persist a refusal and deliver it to the caller.
            // The relay's transport reserve alone does not cover journal I/O.
            let now = clock.mono_now();
            let remaining = budget.remaining(now).as_micros();
            let reserve = 1_000_000.min(remaining / 2);
            let provisioning_budget = WaitBudget::up_to(
                now,
                provenant_core::DurationMicros::from_micros(remaining - reserve),
            );
            provision_added_writers(
                context,
                &mut journal,
                &mut checkpoint,
                &admitted.added,
                provisioning_budget,
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
    if !warnings.is_empty() {
        warnings.sort();
        warnings.dedup();
        value["warnings"] = json!(warnings);
    }
    Ok(value)
}

fn replay_run_request(
    checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
    parent: &str,
    request: &(String, String),
) -> Result<Option<relay::ChildReceipt>, ServiceError> {
    for stored in checkpoint
        .expansions
        .iter()
        .filter_map(|expansion| expansion.patch.as_ref())
        .filter_map(|record| record.child_receipt.as_ref())
        .filter(|stored| stored.receipt.parent == parent)
    {
        if let Some(identity) = stored
            .run_request
            .as_ref()
            .filter(|identity| identity.id == request.0)
        {
            if identity.digest != request.1 {
                return Err(refused(
                    "idempotency_conflict",
                    "run request identity was used for different input",
                ));
            }
            let mut receipt = stored.receipt.clone();
            receipt.replayed = true;
            return Ok(Some(receipt));
        }
    }
    Ok(None)
}

fn retain_run_request(
    checkpoint: &mut provenant_fabric_runtime::graph::GraphCheckpoint,
    receipt: &relay::ChildReceipt,
    request: &(String, String),
) -> Result<(), ServiceError> {
    let stored = checkpoint
        .expansions
        .iter_mut()
        .filter_map(|expansion| expansion.patch.as_mut())
        .filter(|record| record.revision == receipt.revision)
        .filter_map(|record| record.child_receipt.as_mut())
        .find(|stored| stored.receipt.node == receipt.node)
        .ok_or_else(|| refused("invalid_graph", "host-prepared child receipt missing"))?;
    stored.run_request = Some(relay::RunRequestIdentity {
        id: request.0.clone(),
        digest: request.1.clone(),
    });
    Ok(())
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
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<(), ServiceError> {
    use provenant_fabric_runtime::graph::{AccessMode, NodeState, SourceBinding};
    let pending = relay::pending_provisioning(checkpoint);
    let mut fresh = Vec::new();
    // A planned implementation's writer is provisioned, refused and retried
    // as its boundary, the logical child; an inactive owner keeps it unborn.
    let inactive = |id: &str| {
        checkpoint.nodes.iter().any(|progress| {
            progress.id == id
                && (!matches!(progress.state, NodeState::Pending | NodeState::Running)
                    // Recovery owns an already-started launch. An
                    // unrelated add must not provision it again.
                    || progress.launch_retry.is_some()
                    || !progress.attempts.is_empty())
        })
    };
    let writers: Vec<_> = checkpoint
        .expansions
        .iter()
        .flat_map(|expansion| &expansion.nodes)
        .filter(|node| node.access == AccessMode::WorktreeWrite && node.provision_worktree)
        .map(|node| (relay::provisioning_owner(checkpoint, &node.id), node))
        .filter(|(owner, node)| {
            (added.is_empty() || added.contains(&node.id) || added.contains(owner))
                && !pending
                    .iter()
                    .any(|(id, _, _)| id == &node.id || id == owner)
                && !checkpoint.nodes.iter().any(|progress| {
                    progress.worktree == node.worktree && progress.writer_binding.is_some()
                })
                && !inactive(&node.id)
                && !inactive(owner)
        })
        .map(|(owner, node)| (owner, node.clone()))
        .collect();
    for (owner, node) in writers {
        let Some(path) = node.worktree.as_deref() else {
            continue;
        };
        let source = if let Some(source) = checkpoint
            .nodes
            .iter()
            .find(|progress| progress.id == owner)
            .and_then(|progress| progress.provisioned_source.as_deref())
        {
            source.to_owned()
        } else {
            match node.source {
                Some(SourceBinding::Commit { commit }) => commit,
                // These sources become available only when dependencies settle.
                Some(_) => continue,
                None => match checkpoint
                    .bases
                    .and_then(|bases| bases.integration)
                    .map(|base| base.to_string())
                {
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
            .find(|progress| progress.id == owner)
        {
            progress.provisioned_source = Some(source.clone());
        }
        relay::fence_provisioning(checkpoint, &owner, path, &source);
        fresh.push(owner);
    }
    if !relay::pending_provisioning(checkpoint).is_empty() {
        journal.save(checkpoint).map_err(journal_error)?;
        if let Some(error) =
            recover_provisioning(context, journal, checkpoint, &fresh, budget, cancel)?
        {
            return Err(error);
        }
    }
    Ok(())
}

/// Provision newly fenced writers and reconcile older, uncertain Git effects.
/// Only a fresh admission can settle on a refusal before mutation: an older
/// fence needs successful reconciliation, regardless of a later refusal.
fn recover_provisioning(
    context: &Context,
    journal: &mut FileJournal,
    checkpoint: &mut provenant_fabric_runtime::graph::GraphCheckpoint,
    fresh: &[String],
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<Option<ServiceError>, ServiceError> {
    let pending = relay::pending_provisioning(checkpoint);
    if pending.is_empty() {
        return Ok(None);
    }
    let mut refusal = None;
    for (node, path, source) in pending {
        // Older fences predate the durable child-spec creation marker.
        relay::fence_provisioning(checkpoint, &node, &path, &source);
        let clock = provenant_os::clock::SystemClock;
        let now = clock.mono_now();
        let wall = clock
            .wall_now()
            .map_err(|_| ServiceError::Facade(FacadeError::StateCorrupt))?;
        let prior = checkpoint
            .nodes
            .iter()
            .find(|progress| progress.id == node)
            .and_then(|progress| progress.launch_retry.as_ref())
            .and_then(|retry| retry.deadline);
        let recovering = !fresh.contains(&node);
        let active = checkpoint
            .nodes
            .iter()
            .find(|progress| progress.id == node)
            .is_some_and(|progress| {
                matches!(
                    progress.state,
                    provenant_fabric_runtime::graph::NodeState::Pending
                        | provenant_fabric_runtime::graph::NodeState::Running
                ) && progress.cancelled_from.is_none()
            });
        if active
            && checkpoint
                .nodes
                .iter()
                .find(|progress| progress.id == node)
                .and_then(|progress| progress.launch_retry.as_ref())
                .and_then(|retry| retry.wake_at)
                .is_some_and(|wake| wall < wake)
        {
            continue;
        }
        // Reconciliation is a new bounded operation, not another share of an
        // expired launch. Keep the original launch deadline for diagnostics.
        let launch = match prior {
            _ if recovering => budget,
            Some(end) => WaitBudget::up_to(
                now,
                provenant_core::DurationMicros::from_micros(
                    end.as_micros().saturating_sub(wall.as_micros()).max(0) as u64,
                ),
            )
            .cap_at(budget.deadline().as_mono_time()),
            None => budget,
        };
        if active
            && prior.is_none()
            && let Some(progress) = checkpoint
                .nodes
                .iter_mut()
                .find(|progress| progress.id == node)
        {
            progress.launch_retry = Some(provenant_fabric_runtime::graph::LaunchRetry {
                extra: Default::default(),
                policy: Default::default(),
                failures: 0,
                deadline: Some(
                    wall.checked_add(launch.remaining(now))
                        .map_err(|_| ServiceError::Facade(FacadeError::StateCorrupt))?,
                ),
                wake_at: None,
                reason: None,
            });
            journal.save(checkpoint).map_err(journal_error)?;
        }
        let branch = context
            .port_table
            .managed_writer_branch(std::path::Path::new(&path))
            .map(str::to_owned);
        let result = match branch.as_deref() {
            Some(branch) => {
                provision_writer_checked(context, &path, branch, &source, launch, cancel)
            }
            None => Err(provenant_confinement::worktrees::WorktreeError::InvalidRequest),
        };
        if let Err(error) = result {
            if !active {
                // Cancellation and settlement end launch admission, but a
                // prior Git effect still needs reconciliation under its fence.
                continue;
            }
            use provenant_confinement::worktrees::WorktreeError;
            let failed_at = clock
                .wall_now()
                .map_err(|_| ServiceError::Facade(FacadeError::StateCorrupt))?;
            let contention = matches!(
                &error,
                WorktreeError::RegistryLockTimeout
                    | WorktreeError::WriterBusy { .. }
                    | WorktreeError::LockBusy
            );
            // Fresh contention proves this call did not mutate Git. An older
            // fence may still hold uncertain effects, so retain it even when
            // its original launch expires and reconciliation must continue.
            if contention && (!recovering || prior.is_some_and(|end| failed_at < end)) {
                let error = provisioning_refusal(error);
                let ServiceError::Facade(facade) = &error else {
                    return Err(error);
                };
                let index = checkpoint
                    .nodes
                    .iter()
                    .position(|progress| progress.id == node)
                    .ok_or(ServiceError::Facade(FacadeError::StateCorrupt))?;
                let deferred = provenant_fabric_runtime::graph::defer_launch_contention(
                    checkpoint,
                    index,
                    facade,
                    Some(failed_at),
                    context.config.graph.writer_provision_seconds,
                    Default::default(),
                )
                .map_err(|_| ServiceError::Facade(FacadeError::StateCorrupt))?;
                if !deferred {
                    continue;
                }
                if !recovering
                    && checkpoint.nodes[index].state
                        == provenant_fabric_runtime::graph::NodeState::Blocked
                {
                    relay::clear_provisioning(checkpoint, &node);
                }
                refusal.get_or_insert(error);
                continue;
            }
            if recovering {
                // This call did not resolve the effects left by the earlier
                // call. Keep the fence and let other relay work proceed.
                continue;
            }
            // These failures precede any Git mutation, or prove its rollback.
            // Uncertain effects keep the fence for exact-path/source recovery.
            let definitive = matches!(
                &error,
                WorktreeError::Dirty(_)
                    | WorktreeError::DetachedPrimary
                    | WorktreeError::DetachedWorktree { .. }
                    | WorktreeError::CheckedOut
                    | WorktreeError::InvalidRequest
                    | WorktreeError::BranchCollision { .. }
                    | WorktreeError::BranchExistsWithBase { .. }
                    | WorktreeError::RemoteOnlyBranch { .. }
                    | WorktreeError::BaseNotFound { .. }
                    | WorktreeError::BaseUnverified { .. }
                    | WorktreeError::StaleRegistration
                    | WorktreeError::CreateFailed {
                        rolled_back: true,
                        ..
                    }
            );
            let error = provisioning_refusal(error);
            if definitive {
                let progress = checkpoint
                    .nodes
                    .iter_mut()
                    .find(|progress| progress.id == node)
                    .ok_or(ServiceError::Facade(FacadeError::StateCorrupt))?;
                progress.state = provenant_fabric_runtime::graph::NodeState::Blocked;
                progress.advance_error_code = Some("launch_refused".into());
                progress.advance_owner_code = Some(error.code().into());
                progress.advance_error = Some(format!(
                    "writer provisioning refused [{}]; correct the cause, then retry with `provenant retry {}/{node}`",
                    error.code(),
                    checkpoint.graph_id,
                ));
                progress.last_launch_retry = progress.launch_retry.take().map(|mut retry| {
                    retry.wake_at = None;
                    retry
                });
                relay::clear_provisioning(checkpoint, &node);
                refusal.get_or_insert(error);
                continue;
            }
            // Earlier definitive results in this batch still need to settle
            // durably, while this uncertain effect retains its recovery fence.
            journal.save(checkpoint).map_err(journal_error)?;
            return Err(error);
        }
        relay::clear_provisioning(checkpoint, &node);
    }
    journal.save(checkpoint).map_err(journal_error)?;
    Ok(refusal)
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

/// Resolve dynamic panels at the host using declared review context and an
/// optional subject/base range already pinned by the caller.
pub fn launch_resolvers(
    review: &provenant_routing::ReviewConfig,
    document: &provenant_fabric_runtime::graph::GraphDocument,
) -> Result<std::collections::BTreeMap<String, Vec<String>>, String> {
    launch_resolvers_with(
        review,
        document,
        ReviewDiffSelection::NotRequested,
        |_, family| {
            Err(format!(
                "required provider family {}: route eligibility context unavailable",
                family.unwrap_or("unknown")
            ))
        },
    )
}

/// Resolve family-constrained panels against host eligibility before expansion.
/// The optional pinned range contributes path floors and measured size.
pub fn launch_resolvers_at_roots(
    roots: &provenant_core::Roots,
    cwd: &std::path::Path,
    review: &provenant_routing::ReviewConfig,
    document: &provenant_fabric_runtime::graph::GraphDocument,
    review_range: Option<(&str, &str)>,
) -> Result<std::collections::BTreeMap<String, Vec<String>>, String> {
    let pinned_diff = match review_range.map(|(base, head)| {
        let git = provenant_os::git::git_executable().ok_or("git is unavailable")?;
        provenant_run::review_diff_context_at(&git, cwd, base, head)
            .map_err(|error| error.to_string())
    }) {
        None => ReviewDiffSelection::NotRequested,
        Some(Ok(diff)) => ReviewDiffSelection::Captured(vec![diff]),
        Some(Err(_)) => ReviewDiffSelection::Incomplete(Vec::new()),
    };
    launch_resolvers_at_roots_with_selection(roots, cwd, review, document, pinned_diff)
}

/// The host review configuration with the checkout's own floors layered over
/// it, from `[domains.routing.review.floors]` in `.provenant/config.toml`.
fn with_repository_floors(
    review: &provenant_routing::ReviewConfig,
    checkout: &std::path::Path,
) -> Result<provenant_routing::ReviewConfig, String> {
    let Some(text) = provenant_config::read_declaration(checkout).map_err(|e| e.to_string())?
    else {
        return Ok(review.clone());
    };
    let config: toml_edit::DocumentMut = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| e.to_string())?;
    let floors = config
        .get("domains")
        .and_then(|v| v.get("routing"))
        .and_then(|v| v.get("review"))
        .and_then(|v| v.get("floors"))
        .map(|v| {
            toml_edit::de::from_str::<
                std::collections::BTreeMap<String, provenant_routing::ReviewFloor>,
            >(&v.to_string())
            .map_err(|e| e.to_string())
        })
        .transpose()?;
    match floors {
        Some(floors) => review.with_project_floors(floors),
        None => Ok(review.clone()),
    }
}

fn launch_resolvers_at_roots_with_selection(
    roots: &provenant_core::Roots,
    cwd: &std::path::Path,
    review: &provenant_routing::ReviewConfig,
    document: &provenant_fabric_runtime::graph::GraphDocument,
    pinned_diff: ReviewDiffSelection,
) -> Result<std::collections::BTreeMap<String, Vec<String>>, String> {
    let exclude = document.exclude.clone();
    let review = &with_repository_floors(review, cwd)?;
    launch_resolvers_with(review, document, pinned_diff, |slot, family| {
        crate::route::resolve_review_family(roots, &slot.tier, family, &exclude)
    })
}

/// Resolve relayed panels against the source each child or patch actually
/// declares. Dynamic or absent subjects stay incomplete so configured
/// unreadable-path floors apply; the relay never substitutes the caller's
/// source or the host's current HEAD.
fn launch_resolvers_for_relay(
    roots: &provenant_core::Roots,
    cwd: &std::path::Path,
    review: &provenant_routing::ReviewConfig,
    document: &provenant_fabric_runtime::graph::GraphDocument,
    checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
    subjects: &[Option<provenant_fabric_runtime::graph::SourceBinding>],
) -> Result<std::collections::BTreeMap<String, Vec<String>>, String> {
    let pinned_diff = relay_review_diffs(cwd, checkpoint, subjects);
    launch_resolvers_at_roots_with_selection(roots, cwd, review, document, pinned_diff)
}

fn relay_child_review_subjects(
    request: &provenant_fabric_runtime::graph::relay::ChildRequest,
    library: &TemplateLibrary,
) -> Result<Vec<Option<provenant_fabric_runtime::graph::SourceBinding>>, String> {
    let launch = request.launch.as_ref();
    let library = match launch {
        Some(launch) => library.with(Layer::Inline, &launch.templates)?,
        None => library.clone(),
    };
    let node = match launch {
        Some(launch) => launch.node.clone(),
        None => json!({"id": request.id.clone(), "use": request.template.clone()}),
    };
    let mut subjects = Vec::new();
    collect_relay_review_subjects(&node, None, &library, &mut Vec::new(), &mut subjects)?;
    Ok(subjects)
}

fn relay_patch_review_subjects(
    patch: &provenant_fabric_runtime::graph::patch::Patch,
    library: &TemplateLibrary,
) -> Result<Vec<Option<provenant_fabric_runtime::graph::SourceBinding>>, String> {
    let library = library.with(Layer::Inline, &patch.templates)?;
    let mut subjects = Vec::new();
    for node in patch.nodes.iter().chain(&patch.replace) {
        collect_relay_review_subjects(node, None, &library, &mut Vec::new(), &mut subjects)?;
    }
    Ok(subjects)
}

fn collect_relay_review_subjects(
    node: &Value,
    inherited: Option<&provenant_fabric_runtime::graph::SourceBinding>,
    library: &TemplateLibrary,
    expansion: &mut Vec<String>,
    subjects: &mut Vec<Option<provenant_fabric_runtime::graph::SourceBinding>>,
) -> Result<(), String> {
    let own = node.get("source").filter(|source| !source.is_null());
    let effective = own
        .and_then(|source| serde_json::from_value(source.clone()).ok())
        .or_else(|| own.is_none().then(|| inherited.cloned()).flatten());
    if node.get("over").and_then(Value::as_str) == Some("@review-panel") {
        subjects.push(effective.clone());
    }
    if let Some(name) = node.get("use").and_then(Value::as_str) {
        let template = library.resolve(name)?;
        if template.composite_behaviour
            == Some(provenant_fabric_runtime::graph::template::CompositeBehaviour::ReviewPanel)
        {
            subjects.push(effective);
            return Ok(());
        }
        if !template.nodes.is_empty() {
            if expansion.len() >= provenant_fabric_runtime::graph::template::MAX_TEMPLATE_DEPTH
                || expansion.iter().any(|active| active == name)
            {
                return Err(format!("template expansion cycle or depth at {name}"));
            }
            expansion.push(name.to_owned());
            for child in &template.nodes {
                collect_relay_review_subjects(
                    child,
                    effective.as_ref(),
                    library,
                    expansion,
                    subjects,
                )?;
            }
            expansion.pop();
        }
    }
    for pointer in ["/composite/nodes", "/loop/body"] {
        if let Some(children) = node.pointer(pointer).and_then(Value::as_array) {
            for child in children {
                collect_relay_review_subjects(
                    child,
                    effective.as_ref(),
                    library,
                    expansion,
                    subjects,
                )?;
            }
        }
    }
    Ok(())
}

fn relay_review_diffs(
    cwd: &std::path::Path,
    checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
    subjects: &[Option<provenant_fabric_runtime::graph::SourceBinding>],
) -> ReviewDiffSelection {
    if subjects.is_empty() {
        return ReviewDiffSelection::NotRequested;
    }
    let Some(base) = checkpoint
        .bases
        .and_then(|bases| bases.review)
        .map(|base| base.to_string())
    else {
        return ReviewDiffSelection::Incomplete(Vec::new());
    };
    let Some(git) = provenant_os::git::git_executable() else {
        return ReviewDiffSelection::Incomplete(Vec::new());
    };
    let mut incomplete = false;
    let mut diffs = Vec::new();
    for subject in subjects {
        let Some(subject) = subject
            .as_ref()
            .and_then(|binding| relay_review_subject_commit(binding, checkpoint))
        else {
            incomplete = true;
            continue;
        };
        match provenant_run::review_diff_context_at(&git, cwd, &base, &subject) {
            Ok(diff) => diffs.push(diff),
            Err(_) => incomplete = true,
        }
    }
    if incomplete {
        ReviewDiffSelection::Incomplete(diffs)
    } else {
        ReviewDiffSelection::Captured(diffs)
    }
}

fn relay_review_subject_commit(
    binding: &provenant_fabric_runtime::graph::SourceBinding,
    checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
) -> Option<String> {
    use provenant_fabric_runtime::graph::SourceBinding;
    match binding {
        SourceBinding::Commit { commit } => Some(commit.clone()),
        SourceBinding::Node { node } => checkpoint
            .nodes
            .iter()
            .find(|progress| {
                progress.id == *node
                    && progress.state == provenant_records::graph::NodeState::Succeeded
            })
            .and_then(|progress| progress.outputs.as_ref())
            .and_then(|outputs| outputs.commit.clone()),
        // A head binding follows a moving upstream writer chain. This relay
        // has no immutable delivered-head binding for it, so keep the
        // configured fallback floors instead of using an earlier source pin.
        SourceBinding::Head { .. } => None,
        // The graph's integration base is only the admission-time branch
        // base. Runtime binds Integration from the branch head and records
        // that immutable source in integration_pin; without it, stay unknown.
        SourceBinding::Integration => checkpoint.integration_pin.clone(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReviewDiffSelection {
    NotRequested,
    Captured(Vec<provenant_run::ReviewDiffContext>),
    Incomplete(Vec<provenant_run::ReviewDiffContext>),
}

fn combine_review_diffs(
    diffs: &[provenant_run::ReviewDiffContext],
) -> (Vec<String>, Option<String>, Option<String>) {
    let paths = diffs
        .iter()
        .flat_map(|diff| diff.paths.iter().cloned())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let metadata_complete = !diffs.is_empty()
        && diffs
            .iter()
            .all(|diff| diff.numstat.is_some() && diff.tree.is_some());
    let numstat = metadata_complete.then(|| {
        diffs.iter().fold(String::new(), |mut combined, diff| {
            combined.push_str(diff.numstat.as_deref().unwrap_or_default());
            combined
        })
    });
    let tree = metadata_complete.then(|| {
        diffs.iter().fold(String::new(), |mut combined, diff| {
            combined.push_str(diff.tree.as_deref().unwrap_or_default());
            combined
        })
    });
    (paths, numstat, tree)
}

fn launch_resolvers_with(
    review: &provenant_routing::ReviewConfig,
    document: &provenant_fabric_runtime::graph::GraphDocument,
    pinned_diff: ReviewDiffSelection,
    eligible: impl FnMut(&provenant_routing::ResolvedSlot, Option<&str>) -> Result<String, String>,
) -> Result<std::collections::BTreeMap<String, Vec<String>>, String> {
    let mut request = document.review.clone().unwrap_or_default();
    if let Some(context) = &document.context {
        request.risk_tier = request.risk_tier.or_else(|| context.risk_tier.clone());
        request.kind = request.kind.or_else(|| context.task_kind.clone());
    }
    let (fallback_review, paths, measured_size, incomplete_context) = match pinned_diff {
        ReviewDiffSelection::NotRequested => (None, Vec::new(), None, false),
        ReviewDiffSelection::Captured(diffs) => {
            let (paths, numstat, tree) = combine_review_diffs(&diffs);
            if let (Some(numstat), Some(tree)) = (numstat, tree) {
                let measured_size = (request.kind.as_deref() != Some("integration"))
                    .then_some(())
                    .and_then(|()| (!numstat.is_empty()).then_some(numstat.as_str()))
                    .map(|numstat| review.size.classify_numstat(numstat, &tree));
                (None, paths, measured_size, false)
            } else {
                let (fallback, mut paths_with_floors) = review.unreadable_paths();
                paths_with_floors.extend(paths);
                paths_with_floors.sort();
                paths_with_floors.dedup();
                (Some(fallback), paths_with_floors, None, true)
            }
        }
        ReviewDiffSelection::Incomplete(diffs) => {
            let (known_paths, _, _) = combine_review_diffs(&diffs);
            let (fallback, mut paths) = review.unreadable_paths();
            paths.extend(known_paths);
            paths.sort();
            paths.dedup();
            (Some(fallback), paths, None, true)
        }
    };
    let selection_review = fallback_review.as_ref().unwrap_or(review);
    let applied_floors = selection_review.path_floors(&paths);
    let mut hard_review_coverage = false;
    let mut hard_floor_tier: Option<String> = None;
    let mut hard_floor_lenses = Vec::new();
    let risk_rank = |tier: &str| match tier {
        "routine" => 0,
        "substantial" => 1,
        "crucial" => 2,
        "terminal" => 3,
        _ => 0,
    };
    // Only a floor the project declared `hard = true` refuses; every other
    // class warns about a shortfall and names the missing lenses.
    for path in &paths {
        let floors = selection_review.hard_path_floors(std::slice::from_ref(path));
        if floors.classes.is_empty() {
            continue;
        }
        hard_review_coverage = true;
        let tier = floors.tier.as_deref().unwrap_or("routine");
        if hard_floor_tier
            .as_deref()
            .is_none_or(|current| risk_rank(tier) > risk_rank(current))
        {
            hard_floor_tier = Some(tier.to_owned());
        }
        for lens in floors.lenses {
            if !hard_floor_lenses.contains(&lens) {
                hard_floor_lenses.push(lens);
            }
        }
    }
    let mut panel = selection_review.resolve_panel(&request, measured_size, &paths, false)?;
    let coverage_minimum = hard_floor_tier
        .as_deref()
        .or(applied_floors.tier.as_deref())
        .and_then(|tier| selection_review.select.by_risk.get(tier))
        .and_then(|shape| selection_review.shapes.get(shape))
        .or_else(|| selection_review.shapes.get(&panel.shape))
        .map(|shape| shape.reviewers.iter().map(|reviewer| reviewer.count).sum())
        .unwrap_or(panel.slots.len());
    if (!panel.minimum_family_counts.is_empty()
        || panel
            .slots
            .iter()
            .any(|slot| slot.required_family.is_some()))
        && let Some(tier) = document.review_tier_override.as_ref()
    {
        for slot in &mut panel.slots {
            slot.tier = serde_json::Value::String(tier.clone());
        }
    }
    panel.resolve_families(eligible)?;
    if incomplete_context {
        panel.warnings.push(
            "pinned review path or size metadata was incomplete; configured unreadable-path floors were applied"
                .into(),
        );
    }
    let mut resolvers = std::collections::BTreeMap::new();
    let single = panel.slots.len() == 1;
    let frame = !single && panel.frame;
    let verifiers = if single { 0 } else { panel.verifiers };
    let synthesis = !single && panel.synthesis == provenant_routing::ReviewSynthesis::Model;
    resolvers.extend(provenant_fabric_runtime::graph::when::review_stages(
        frame,
        verifiers > 0,
        synthesis,
    ));
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
        let advice = panel.lens_advice(
            lens,
            matches!(request.risk_tier.as_deref(), Some("crucial" | "terminal")),
        );
        resolvers.insert(format!("review-advice-{lens}"), vec![advice]);
    }
    if !panel.minimum_family_counts.is_empty()
        || panel
            .slots
            .iter()
            .any(|slot| slot.required_family.is_some())
    {
        resolvers.insert(
            "review-family-resolution".into(),
            vec![serde_json::to_string(&panel).map_err(|error| error.to_string())?],
        );
    }
    let mut lenses = Vec::with_capacity(panel.slots.len());
    let mut routes = Vec::with_capacity(panel.slots.len());
    for slot in panel.slots {
        let (key, mut route) = tier_route(&slot.lens, &slot.tier, None, true)?;
        if let Some(family) = slot
            .satisfied_family
            .as_ref()
            .or(slot.required_family.as_ref())
        {
            if let Some(routes) = route.as_array_mut() {
                for route in routes {
                    route["required_family"] = json!(family);
                }
            } else {
                route["required_family"] = json!(family);
            }
        }
        let mut fields = serde_json::Map::new();
        fields.insert(key.into(), route);
        routes.push(serde_json::to_string(&fields).map_err(|error| error.to_string())?);
        lenses.push(slot.lens);
    }
    resolvers.insert("review-panel".into(), lenses);
    resolvers.insert("review-panel-routes".into(), routes);
    resolvers.insert(
        "review-coverage-floor".into(),
        vec![
            serde_json::to_string(&json!({
                "hard": hard_review_coverage,
                "minimum": coverage_minimum,
                "required_lenses": hard_floor_lenses,
            }))
            .map_err(|error| error.to_string())?,
        ],
    );
    Ok(resolvers)
}

/// Create a fenced writer checkout, or verify its clean recovery at the captured source.
pub fn provision_child(
    context: &Context,
    path: &str,
    source: &str,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<(), ServiceError> {
    let clock = provenant_os::clock::SystemClock;
    let budget = writer_launch_budget(context, &clock, budget);
    provision_child_with_budget(context, path, source, budget, cancel)
}

/// Provision against an already-started launch deadline; never renew it.
pub fn provision_child_with_budget(
    context: &Context,
    path: &str,
    source: &str,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<(), ServiceError> {
    let branch = context
        .port_table
        .managed_writer_branch(std::path::Path::new(path))
        .map(str::to_owned)
        .ok_or_else(|| refused("writer_worktree", "invalid managed worktree path"))?;
    provision_writer(context, path, &branch, source, budget, cancel)
}

/// Provision a graph-owned writer at its checkpointed source commit.
pub fn provision_writer(
    context: &Context,
    path: &str,
    branch: &str,
    source: &str,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<(), ServiceError> {
    provision_writer_checked(context, path, branch, source, budget, cancel)
        .map(|_| ())
        .map_err(provisioning_refusal)
}

fn provisioning_refusal(error: provenant_confinement::worktrees::WorktreeError) -> ServiceError {
    use provenant_confinement::worktrees::WorktreeError;
    use provenant_fabric_runtime::coordination::facade::DispatchRefusalFact;
    if matches!(&error, WorktreeError::ManagedPathMismatch { .. }) {
        let diagnostic = provenant_run::RunError::Writer(error)
            .diagnostic(provenant_records::diagnostic::Destination::Local);
        return ServiceError::Facade(FacadeError::DispatchRefused {
            fact: None,
            disk: None,
            reason: format!(
                "{}; use `--branch` to create a writer in the configured layout, or select an existing checkout with `--worktree`",
                diagnostic.detail.unwrap_or_else(|| {
                    "requested writer path does not match the configured project worktree layout"
                        .into()
                })
            ),
        });
    }
    let fact = match &error {
        WorktreeError::Dirty(_) => Some(DispatchRefusalFact::DirtyWriter),
        WorktreeError::DetachedWorktree { .. } | WorktreeError::DetachedPrimary => {
            Some(DispatchRefusalFact::DetachedWriter)
        }
        WorktreeError::CheckedOut => Some(DispatchRefusalFact::CheckedOutWriter),
        WorktreeError::RegistryLockTimeout => {
            return ServiceError::Facade(FacadeError::WriterLockBusy);
        }
        WorktreeError::WriterBusy { holder } => {
            return ServiceError::Facade(FacadeError::WriterBusy { holder: *holder });
        }
        WorktreeError::LockBusy => {
            return ServiceError::Facade(FacadeError::WriterBusy { holder: None });
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
}

/// Start one independent provisioning allowance; later locks and retries reuse it.
/// The caller's reader wait never extends or shortens writer provisioning.
pub fn writer_launch_budget(
    context: &Context,
    clock: &dyn Clock,
    _caller: WaitBudget,
) -> WaitBudget {
    let now = clock.mono_now();
    let floor = provenant_core::DurationMicros::from_micros(
        context
            .config
            .graph
            .writer_provision_seconds
            .saturating_mul(1_000_000),
    );
    WaitBudget::up_to(now, floor)
}

/// Resolve a validated checkpoint path against the project root that owns it.
fn resolve_managed_writer_path(
    ports: &provenant_config::PortTable,
    project_root: &std::path::Path,
    path: &str,
    branch: &str,
) -> Result<std::path::PathBuf, provenant_confinement::worktrees::WorktreeError> {
    use provenant_confinement::worktrees::WorktreeError;
    if !project_root.is_absolute() {
        return Err(WorktreeError::InvalidRequest);
    }
    let expected = ports
        .managed_writer_path(branch)
        .map_err(WorktreeError::PortConfiguration)?;
    let requested = project_root.join(path);
    let expected = project_root.join(expected);
    if requested == expected {
        Ok(expected)
    } else {
        Err(WorktreeError::ManagedPathMismatch {
            requested,
            expected,
        })
    }
}

/// Preserve the typed worktree failure for the graph launch owner.
pub fn provision_writer_checked(
    context: &Context,
    path: &str,
    branch: &str,
    source: &str,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<std::path::PathBuf, provenant_confinement::worktrees::WorktreeError> {
    use provenant_confinement::worktrees;
    let clock = provenant_os::clock::SystemClock::new();
    if budget.remaining(clock.mono_now()).as_micros() == 0 {
        return Err(worktrees::WorktreeError::RegistryLockTimeout);
    }
    worktrees::with_context(
        &context.roots,
        &context.home,
        &context.cwd,
        &clock,
        cancel,
        |worktree_context| {
            if budget.remaining(clock.mono_now()).as_micros() == 0 {
                return Err(worktrees::WorktreeError::RegistryLockTimeout);
            }
            let worktree_context = worktrees::WorktreeContext {
                lock_budget: budget,
                ..*worktree_context
            };
            let layout = worktree_context.layout;
            let ports = provenant_config::PortTable::read_for(
                layout.project(),
                context.roots.state.as_path(),
            )
            .map_err(worktrees::WorktreeError::PortConfiguration)?;
            let expected = resolve_managed_writer_path(&ports, layout.root(), path, branch)?;
            provision_in_context(&worktree_context, path, branch, source, cancel)?;
            Ok(expected)
        },
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
    let layout = context.layout;
    let ports = provenant_config::PortTable::read_for(layout.project(), &context.host_state()?)
        .map_err(worktrees::WorktreeError::PortConfiguration)?;
    let expected = resolve_managed_writer_path(&ports, layout.root(), path, branch)?;
    // Inspect registration and custody without holding sibling worktrees open
    // for unrelated Git probes. The selected target's cleanliness is checked
    // below, under its writer lease.
    if let Some(existing) =
        worktrees::list_for_gc_matching(context, cancel, |path, _| path == expected)?
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
    .and_then(|created| {
        // A failed earlier add may have left only a branch. The general
        // create path accepts descendants of its base; this fence requires
        // the checkpointed commit exactly, even for a newly recreated path.
        if created.path != expected
            || created.head.to_string() != source
            || created.branch.as_deref() != Some(format!("refs/heads/{branch}").as_str())
        {
            return Err(WorktreeError::IdentityChanged);
        }
        Ok(())
    })
}

/// Lane side: send `op` to the host relay of the attempt this lane runs as.
/// `provenant run` and `fabric_run` inside a lane call this to add a child.
pub fn from_lane(
    op: &RelayOp,
    cancel: &CancelToken,
    within: std::time::Duration,
) -> Result<Value, ServiceError> {
    let attempt = std::env::var(provenant_core::consumer_env::PARENT_ATTEMPT_ID)
        .ok()
        .and_then(|id| provenant_core::AttemptId::parse(&id).ok())
        .ok_or_else(|| refused("graph_relay_unavailable", "not running inside a lane"))?;
    provenant_fabric_runtime::relay::graph(
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
    interval: std::time::Duration,
) -> Result<Value, ServiceError> {
    let clock = provenant_os::clock::SystemClock::new();
    let started = clock.mono_now();
    while_waiting(cancel, within, || {
        poll_children(cancel, within, &clock, started, interval, |remaining| {
            from_lane(&RelayOp::Children, cancel, remaining)
        })
    })
}

pub fn child_wait_observation(cadence: &crate::wait::LocalCadence) -> String {
    format!(
        "paced reread {}; relay socket has no remote event subscription",
        cadence.every()
    )
}

#[allow(clippy::disallowed_methods)] // Real-time boundary: paced child polling.
fn pause_child_poll(cancel: &CancelToken, delay: std::time::Duration) {
    let deadline = std::time::Instant::now() + delay;
    while !cancel.is_cancelled() {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        Pause::wait(
            &provenant_core::ThreadPause,
            provenant_core::DurationMicros::from_micros(
                u64::try_from(remaining.as_micros()).unwrap_or(u64::MAX),
            ),
            cancel,
        );
    }
}

fn poll_children(
    cancel: &CancelToken,
    within: std::time::Duration,
    clock: &dyn Clock,
    started: provenant_core::MonoTime,
    interval: std::time::Duration,
    mut poll: impl FnMut(std::time::Duration) -> Result<Value, ServiceError>,
) -> Result<Value, ServiceError> {
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
        let elapsed = std::time::Duration::from_micros(
            clock
                .mono_now()
                .as_micros()
                .saturating_sub(started.as_micros()),
        );
        let remaining = within.saturating_sub(elapsed);
        pause_child_poll(cancel, remaining.min(interval));
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
        &repository.join(provenant_config::repository_files::TEMPLATES),
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
    let clock = provenant_os::clock::SystemClock::new();
    let started = clock.mono_now();
    with_waiting(
        |waiting| {
            let cleanup = CancelToken::new();
            wait_for_admission(|| {
                // Yield retries share the observation deadline. Reclaiming the
                // parent's slot is mandatory cleanup with its own allowance.
                let remaining = if waiting {
                    let elapsed = std::time::Duration::from_micros(
                        clock
                            .mono_now()
                            .as_micros()
                            .saturating_sub(started.as_micros()),
                    );
                    let remaining = within.saturating_sub(elapsed);
                    if remaining.is_zero() {
                        return Err(refused("timeout", "child wait timed out"));
                    }
                    remaining
                } else {
                    within
                };
                from_lane(
                    &RelayOp::Wait { waiting },
                    if waiting { cancel } else { &cleanup },
                    remaining,
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
                        provenant_os::clock::SystemClock.mono_now(),
                        std::time::Duration::from_millis(1),
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
        // Time spent yielding the slot consumes the same settlement deadline.
        let clock = provenant_core::TestClock::new(
            provenant_core::WallTime::from_micros(0).unwrap(),
            provenant_core::MonoTime::from_micros(0),
        );
        let started = clock.mono_now();
        frames.clear();
        let result = with_waiting(
            |waiting| {
                frames.push(waiting);
                if waiting {
                    clock
                        .wait_until(provenant_core::MonoTime::from_micros(2_000_000))
                        .unwrap();
                }
                Ok(())
            },
            || {
                poll_children(
                    &CancelToken::new(),
                    std::time::Duration::from_secs(1),
                    &clock,
                    started,
                    std::time::Duration::from_millis(1),
                    |_| panic!("must not poll after yield consumed the deadline"),
                )
            },
        );
        assert!(matches!(result, Err(ServiceError::Cli { code, .. }) if code == "timeout"));
        assert_eq!(frames, [true, false]);
    }
}

#[cfg(test)]
mod provisioning_tests {
    use super::*;

    // Spec: docs/specs/fabric.md#cli-and-relay-surfaces
    #[test]
    fn writer_provisioning_contention_keeps_its_fence_until_the_same_checkout_is_free()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_fabric_runtime::graph::{
            GraphCheckpoint, GraphDocument, GraphExecution, LaunchRetry, LaunchRetryPolicy,
            NodeState,
        };
        use provenant_os::fs::{Directory, LockMode};
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
        let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
        inputs.state_dir = Some(root.path().join("state"));
        let context = Context::open(
            inputs.resolve(&provenant_os::fs::SystemPathResolver)?,
            &home,
            &project,
        )?;
        let clock = provenant_os::clock::SystemClock;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(clock.mono_now(), 5)?;
        let path = ".worktrees/waiting-writer";
        provision_writer(&context, path, "waiting-writer", &source, budget, &cancel)?;
        let slot = context
            .roots
            .state
            .as_path()
            .join("worktrees")
            .join(context.identity.project.to_string())
            .join(provenant_confinement::worktrees::state_dir_name(
                // Slots are keyed by the canonical checkout path.
                &project.canonicalize()?.join(path),
            ));
        let slot = Directory::open_anchor(&slot)?;
        let held =
            slot.try_lock_existing(std::ffi::OsStr::new("writer.lock"), LockMode::Exclusive)?;
        let document: GraphDocument = serde_json::from_value(json!({
            "schema_version":1,"graph_id":"provision-contention","nodes":[{
                "id":"writer","access":"worktree_write","worktree":path,"prompt":"Write","route":{"alias":"junior"}
            }]
        }))?;
        let mut checkpoint: GraphCheckpoint = serde_json::from_value(json!({
            "schema_version":2,"graph_id":"provision-contention","plan_digest":"fixture","nodes":[{
                "id":"writer","state":"pending","worktree":path,"attempts":[]
            }]
        }))?;
        let deadline = clock
            .wall_now()?
            .checked_add(provenant_core::DurationMicros::from_seconds(60)?)?;
        checkpoint.nodes[0].launch_retry = Some(LaunchRetry {
            extra: Default::default(),
            policy: LaunchRetryPolicy {
                limit: 0,
                first_seconds: 0,
                max_seconds: 0,
                ..Default::default()
            },
            failures: 0,
            deadline: Some(deadline),
            wake_at: None,
            reason: None,
        });
        relay::fence_provisioning(&mut checkpoint, "writer", path, &source);
        let mut journal = FileJournal::open(
            &context.roots.state,
            context.identity.project,
            "provision-contention",
            budget,
            &cancel,
            &clock,
        )?;
        journal.bind(GraphExecution {
            peer_origin_context: None,
            peer_submission_digest: None,
            requester_path: None,
            extra: Default::default(),
            bases: None,
            directory: project.clone(),
            document,
        })?;
        journal.prepare_fixture_checkpoint(&mut checkpoint)?;
        journal.save(&checkpoint)?;
        let error = recover_provisioning(
            &context,
            &mut journal,
            &mut checkpoint,
            &["writer".into()],
            budget,
            &cancel,
        )?
        .ok_or("contention diagnostic")?;
        assert_eq!(error.code(), "writer_busy");
        assert_eq!(checkpoint.nodes[0].state, NodeState::Running);
        assert!(checkpoint.nodes[0].attempts.is_empty());
        assert_eq!(
            checkpoint.nodes[0]
                .launch_retry
                .as_ref()
                .and_then(|retry| retry.deadline),
            Some(deadline)
        );
        assert_eq!(
            relay::pending_provisioning(&checkpoint),
            vec![("writer".into(), path.into(), source.clone())]
        );
        assert_eq!(
            journal.load("provision-contention")?,
            Some(checkpoint.clone())
        );
        drop(held);
        assert!(
            recover_provisioning(
                &context,
                &mut journal,
                &mut checkpoint,
                &[],
                WaitBudget::cli(clock.mono_now(), 5)?,
                &cancel
            )?
            .is_none()
        );
        assert!(relay::pending_provisioning(&checkpoint).is_empty());
        assert_eq!(git(&["-C", path, "rev-parse", "HEAD"])?, source);
        assert_eq!(checkpoint.nodes[0].state, NodeState::Running);
        Ok(())
    }

    #[test]
    fn relay_refusal_cause_reaches_the_lane_without_raw_source_text()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let socket = root.path().join("relay/socket");
        let _host = provenant_fabric_runtime::relay::HostRelay::start_with_graph(
            socket.clone(),
            Default::default(),
            |_, _, _| Err(ServiceError::FabricOwnerUnavailable),
            |_, _, _| Err(refused("writer_parent_required", "PROMPT_SENTINEL")),
        )?;
        let error = provenant_fabric_runtime::relay::graph(
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
        let canonical_root = root.path().canonicalize()?;
        let home = canonical_root.join("home");
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
        let state = root.path().join("state");
        provenant_os::fixture::private_state_dir(&state)?;
        inputs.state_dir = Some(state);
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        provenant_config::bind_project(
            &roots,
            provenant_config::ProjectLocation::new(project.join(".git"))?,
        )?;
        let context = Context::open(roots, &home, &project)?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(provenant_os::clock::SystemClock.mono_now(), 55)?;
        // Explicit top-level writer branches use the same managed path rules
        // as worktree creation, preserving slashes in the Git branch itself.
        provision_writer(
            &context,
            ".worktrees/topic-fix",
            "topic/fix",
            &source,
            budget,
            &cancel,
        )?;
        let branch = provenant_os::fixture::git_command()
            .current_dir(project.join(".worktrees/topic-fix"))
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()?;
        assert!(branch.status.success());
        assert_eq!(String::from_utf8(branch.stdout)?.trim(), "topic/fix");
        assert!(
            provision_writer(
                &context,
                ".worktrees/topic-fix",
                "other",
                &source,
                budget,
                &cancel
            )
            .is_err()
        );
        let path = ".worktrees/graph-parent--child";
        // Busy registry access is a retryable condition. It must not extend
        // the caller's deadline or wait for the other writer to finish.
        let clock = provenant_os::clock::SystemClock;
        let directory = provenant_os::fs::Directory::open_anchor(&project.join(".git"))?;
        let held = directory.lock(
            std::ffi::OsStr::new(provenant_confinement::worktrees::REPOSITORY_LOCK),
            provenant_os::fs::LockMode::Exclusive,
            WaitBudget::nonblocking(clock.mono_now()),
            &cancel,
            &clock,
        )?;
        let busy = provision_child_with_budget(
            &context,
            path,
            &source,
            WaitBudget::nonblocking(clock.mono_now()),
            &cancel,
        );
        assert!(matches!(
            busy,
            Err(ServiceError::Facade(FacadeError::WriterLockBusy))
        ));
        assert!(!project.join(path).exists());
        drop(held);
        // The caller retries once the registry is available.
        provision_child_with_budget(
            &context,
            path,
            &source,
            WaitBudget::cli(clock.mono_now(), 1)?,
            &cancel,
        )?;
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
                    // The fixture's TMPDIR may be an alias. Lock the same
                    // canonical checkout identity that production verifies.
                    let canonical_worktree = provenant_os::fs::canonicalize(&project.join(path))?;
                    let _held = provenant_confinement::worktrees::cache_writer_guard(
                        ctx.project_state,
                        &canonical_worktree,
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

    // Spec: docs/specs/fabric.md#cli-and-relay-surfaces
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
        let bases = provenant_fabric_runtime::graph::GraphBaseBinding::new(
            None,
            Some(provenant_core::GitOid::parse(&source)?),
        )
        .ok_or("integration base")?;
        let mut checkpoint: GraphCheckpoint = serde_json::from_value(json!({
            "schema_version":2,"graph_id":"g-writers","plan_digest":"fixture",
            "nodes":[{"id":"parent","state":"running","worktree":null,"attempts":[{
                "launched":true,"key":{"graph_id":"g-writers","node_id":"parent","route_index":0, "attempt": 1},
                "route":{"alias":"senior"},"state":"running","attempt_ref":reference,"outcome":null,"launch_actor":"fixture"
            }]}]
        }))?;
        checkpoint.bases = Some(bases);
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
                peer_origin_context: None,
                peer_submission_digest: None,
                requester_path: None,
                extra: Default::default(),
                bases: Some(bases),
                directory: project.clone(),
                document,
            })?;
            journal.prepare_fixture_checkpoint(&mut checkpoint)?;
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
        let request = json!({"op":"add","id":"writer","use":"implement","prompt":"Write","writer":true,"write_scope":["src"],"idempotency_key":"write"});
        let receipt = serve(&context, "caller", request.clone(), budget, &cancel, &clock)?;
        let path = receipt["worktree"].as_str().ok_or("missing worktree")?;
        assert_eq!(git(&["-C", path, "rev-parse", "HEAD"])?, source);
        let replay = serve(&context, "caller", request, budget, &cancel, &clock)?;
        assert_eq!(replay["node"], receipt["node"]);
        assert_eq!(replay["replayed"], true);
        let edit = json!({"op":"edit","id":"g-writers","patch":{"schema_version":1,"idempotency_key":"edit-writer","nodes":[{
            "id":"edited","use":"implement","prompt":"Edit","write_scope":["other"]
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

    // Spec: docs/specs/fabric.md#cli-and-relay-surfaces
    // Spec: docs/specs/routing.md#pinned-review-selection
    #[test]
    fn lost_child_receipt_replays_while_active_and_after_graph_settlement()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_fabric_runtime::graph::{
            AttemptState, GraphCheckpoint, GraphExecution, NodeState,
        };
        for (terminal, concurrent) in [(NodeState::Succeeded, false), (NodeState::Skipped, true)] {
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
            let base = git(&["rev-parse", "HEAD"])?;
            git(&["commit", "--allow-empty", "-qm", "review subject"])?;
            let subject = git(&["rev-parse", "HEAD"])?;
            let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
            inputs.state_dir = Some(root.path().join("state"));
            let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
            let context = Context::open(roots, &home, &project)?;
            let cancel = CancelToken::new();
            let clock = provenant_os::clock::SystemClock::new();
            let budget = WaitBudget::cli(clock.mono_now(), 5)?;
            let reference = format!("{}/caller", context.identity.host);
            let mut checkpoint: GraphCheckpoint = serde_json::from_value(json!({
                "schema_version":2,"graph_id":"g-lost-receipt","plan_digest":"fixture",
                "nodes":[{"id":"parent","state":"running","worktree":null,"attempts":[{
                    "launched":true,"key":{"graph_id":"g-lost-receipt","node_id":"parent","route_index":0, "attempt": 1},
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
                    peer_origin_context: None,
                    peer_submission_digest: None,

                    requester_path: None,
                    extra: Default::default(),
                    bases: None,
                    directory: project.clone(),
                    document: serde_json::from_value(json!({
                        "schema_version":1,"graph_id":"g-lost-receipt","template_library":{},
                        "nodes":[{"id":"parent","access":"read_only","prompt":"Go","route":{"alias":"senior"}}]
                    }))?,
                })?;
                journal.prepare_fixture_checkpoint(&mut checkpoint)?;
                journal.save(&checkpoint)?;
            }
            let request = json!({
                "op":"add","id":"child","use":"task","prompt":"password=example-lost-reply-3391", "idempotency_key":"lost-reply",
                "launch":{"node":{"use":"task","prompt":format!("Review {base}...{subject}"),
                    "source":{"type":"commit","commit":subject}}}
            });
            let input = if concurrent {
                json!({"template":"review","subject":"HEAD","review_base":base})
            } else {
                json!({"template":"review","pr":7})
            };
            let identity = relay::RunRequestIdentity::for_input(&input)?;
            let run = json!({"op":"run","request_id":identity.id,"input":input});
            let prepare = |input: Value| -> Result<Value, ServiceError> {
                assert_eq!(input["op"], "prepare_child");
                if concurrent {
                    assert_eq!(input["input"]["subject"], "HEAD");
                    assert_eq!(input["input"]["review_base"], base);
                } else {
                    assert_eq!(input["input"]["pr"], 7);
                }
                // Host lookup/preparation must not hold the graph journal lock.
                drop(open().map_err(journal_error)?);
                let mut prepared = request.clone();
                prepared.as_object_mut().unwrap().remove("op");
                Ok(prepared)
            };
            let racing_prepare = |input| {
                // Another caller admits the same raw request while this caller
                // is preparing outside the journal lock. Its pins win.
                serve_with_read(
                    &context,
                    "caller",
                    run.clone(),
                    budget,
                    &cancel,
                    &clock,
                    &prepare,
                )?;
                let mut moved = prepare(input)?;
                moved["launch"]["node"]["source"]["commit"] = json!(base);
                Ok(moved)
            };
            let receipt = serve_with_read(
                &context,
                "caller",
                run.clone(),
                budget,
                &cancel,
                &clock,
                &|input| {
                    if concurrent {
                        racing_prepare(input)
                    } else {
                        prepare(input)
                    }
                },
            )?;
            git(&["commit", "--allow-empty", "-qm", "moved review ref"])?;
            let moved_subject = git(&["rev-parse", "HEAD"])?;
            assert_ne!(moved_subject, subject);
            let before_live_replay = open()?.load("g-lost-receipt")?;
            let admitted = before_live_replay
                .as_ref()
                .ok_or("checkpoint")?
                .expansions
                .iter()
                .flat_map(|expansion| &expansion.nodes)
                .find(|node| node.id == receipt["node"].as_str().unwrap())
                .ok_or("child")?;
            assert_eq!(
                serde_json::to_value(&admitted.source)?,
                request["launch"]["node"]["source"]
            );
            assert_eq!(
                admitted.prompt,
                request["launch"]["node"]["prompt"].as_str().unwrap()
            );
            let mut changed_wait = run.clone();
            changed_wait["input"]["wait_seconds"] = json!(5);
            let live_replay = serve_with_read(
                &context,
                "caller",
                changed_wait,
                budget,
                &cancel,
                &clock,
                &|input| {
                    let mut moved = prepare(input)?;
                    moved["launch"]["node"]["source"]["commit"] = json!(moved_subject);
                    panic!(
                        "a matching receipt must bypass lookup even when the review ref moved: {moved}"
                    );
                },
            )?;
            let mut expected_replay = receipt.clone();
            expected_replay["replayed"] = json!(true);
            assert_eq!(live_replay, expected_replay);
            // Reopen the checkpoint and context, as after a host restart, with
            // forge lookup unavailable. Both pins and the receipt must survive.
            let reloaded_context = Context::open(context.roots.clone(), &home, &project)?;
            let unavailable = |_| Err(refused("forge_unavailable", "fixture lookup unavailable"));
            let reloaded_replay = serve_with_read(
                &reloaded_context,
                "caller",
                run.clone(),
                budget,
                &cancel,
                &clock,
                &unavailable,
            )?;
            assert_eq!(reloaded_replay, expected_replay);
            assert_eq!(open()?.load("g-lost-receipt")?, before_live_replay);
            let mut fresh_run = run.clone();
            fresh_run["input"]["idempotency_key"] = json!("fresh-live-run");
            fresh_run["request_id"] =
                json!(relay::RunRequestIdentity::for_input(&fresh_run["input"])?.id);
            assert!(matches!(
                serve_with_read(&context, "caller", fresh_run.clone(), budget, &cancel, &clock, &unavailable),
                Err(error) if error.code() == "forge_unavailable"
            ));
            let fresh_receipt = serve_with_read(
                &context,
                "caller",
                fresh_run,
                budget,
                &cancel,
                &clock,
                &|input| {
                    let mut prepared = prepare(input)?;
                    prepared["id"] = json!("fresh-child");
                    prepared["idempotency_key"] = json!("fresh-live-run");
                    prepared["launch"]["node"]["source"]["commit"] = json!(moved_subject);
                    Ok(prepared)
                },
            )?;
            assert_eq!(fresh_receipt["replayed"], false);
            let fresh_checkpoint = open()?.load("g-lost-receipt")?.ok_or("checkpoint")?;
            let fresh_child = fresh_checkpoint
                .expansions
                .iter()
                .flat_map(|expansion| &expansion.nodes)
                .find(|node| node.id == fresh_receipt["node"].as_str().unwrap())
                .ok_or("fresh child")?;
            assert_eq!(
                serde_json::to_value(&fresh_child.source)?,
                json!({"type":"commit","commit":moved_subject})
            );
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
            {
                let warnings = replay["warnings"]
                    .as_array()
                    .ok_or("visible replay warnings")?;
                assert!(
                    warnings
                        .iter()
                        .any(|w| w.to_string().contains("prompt looks like a credential"))
                );
                assert!(
                    warnings
                        .iter()
                        .all(|w| !w.to_string().contains("example-lost-reply-3391"))
                );
            }
            assert_eq!(replay["replayed"], true);
            let unavailable = |_| panic!("settled run must not prepare or fetch");
            let prepared_replay = serve_with_read(
                &context,
                "caller",
                run.clone(),
                budget,
                &cancel,
                &clock,
                &unavailable,
            )?;
            assert_eq!(prepared_replay["node"], replay["node"]);
            assert_eq!(prepared_replay["revision"], replay["revision"]);
            let mut changed_run = run.clone();
            changed_run["input"]["pr"] = json!(8);
            assert!(
                matches!(serve_with_read(&context, "caller", changed_run, budget, &cancel, &clock, &unavailable), Err(error) if error.code() == "invalid_input")
            );
            let mut fresh_run = run;
            fresh_run["input"]["idempotency_key"] = json!("fresh-run");
            fresh_run["request_id"] =
                json!(relay::RunRequestIdentity::for_input(&fresh_run["input"])?.id);
            assert!(
                matches!(serve_with_read(&context, "caller", fresh_run, budget, &cancel, &clock, &unavailable), Err(error) if error.code() == "graph_relay_unavailable")
            );
            assert_eq!(open()?.load("g-lost-receipt")?, Some(settled));
            let mut changed = request.clone();
            changed["prompt"] = json!("Different");
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

    // Spec: docs/specs/fabric.md#cli-and-relay-surfaces
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
        let mut checkpoint: GraphCheckpoint = serde_json::from_value(json!({
            "schema_version":2,"graph_id":"edit-scope","plan_digest":"fixture",
            "nodes":[
                {"id":"writer","state":"running","worktree":null,"attempts":[{
                    "launched":true,"key":{"graph_id":"edit-scope","node_id":"writer","route_index":0, "attempt": 1},
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
                peer_origin_context: None,
                peer_submission_digest: None,
                requester_path: None,
                extra: Default::default(),
                bases: None,
                directory: project.clone(),
                document,
            })?;
            journal.prepare_fixture_checkpoint(&mut checkpoint)?;
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
            assert_eq!(
                result
                    .expect_err("edits outside the subtree must be refused")
                    .code(),
                "edit_scope"
            );
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

    // Spec: docs/specs/fabric.md#graph-admission-and-refusal-causes
    #[test]
    fn crash_after_the_fence_is_recovered_by_the_next_serve()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_fabric_runtime::graph::{
            GraphCheckpoint, GraphDocument, GraphExecution, NodeState,
        };
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
        let budget = WaitBudget::cli(clock.mono_now(), 30)?;
        let mut journal = FileJournal::open(
            &context.roots.state,
            context.identity.project,
            "g-crash",
            budget,
            &cancel,
            &clock,
        )?;
        let path = ".worktrees/g-crash-a--w";
        let document: GraphDocument = serde_json::from_value(json!({
            "schema_version":1, "graph_id":"g-crash",
            "nodes":[
                {"id":"a","access":"read_only","prompt":"Go","route":{"alias":"senior"}},
                {"id":"a--w","access":"worktree_write","worktree":path,"prompt":"Write","route":{"alias":"junior"}}
            ]
        }))?;
        journal.bind(GraphExecution {
            peer_origin_context: None,
            peer_submission_digest: None,
            requester_path: None,
            extra: Default::default(),
            bases: None,
            directory: project.clone(),
            document: document.clone(),
        })?;
        // The crash: the fence was checkpointed, then the process died before
        // any Git effect.
        let reference = format!("{}/caller", context.identity.host);
        let mut checkpoint: GraphCheckpoint = serde_json::from_value(json!({
            "schema_version":2, "graph_id":"g-crash", "plan_digest":"fixture", "nodes":[
                {"id":"a","state":"running","worktree":null,"attempts":[{
                    "launched":true,"key":{"graph_id":"g-crash","node_id":"a","route_index":0, "attempt": 1},
                    "route":{"alias":"senior"},"state":"running","attempt_ref":reference,
                    "outcome":null,"launch_actor":"fixture"
                }]},
                {"id":"a--w","state":"pending","worktree":path,"attempts":[]}
            ]
        }))?;
        relay::fence_provisioning(&mut checkpoint, "a--w", path, &source);
        journal.prepare_fixture_checkpoint(&mut checkpoint)?;
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
        assert!(
            recover_provisioning(&context, &mut journal, &mut reloaded, &[], budget, &cancel)?
                .is_none()
        );
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
        assert!(
            recover_provisioning(&context, &mut journal, &mut reloaded, &[], budget, &cancel)?
                .is_none()
        );
        assert_eq!(git(&["worktree", "list", "--porcelain"])?, before);
        assert_eq!(git(&["-C", path, "rev-parse", "HEAD"])?, source);
        let saved = journal.load("g-crash")?.expect("checkpoint");
        assert!(relay::pending_provisioning(&saved).is_empty());

        // A crash after Git succeeds leaves uncertain effects even when the
        // original launch deadline has expired. A later pre-mutation refusal
        // cannot settle that fence, and must not block unrelated relay reads.
        reloaded.nodes[1]
            .launch_retry
            .as_mut()
            .ok_or("launch deadline")?
            .deadline = Some(provenant_core::WallTime::from_micros(0)?);
        relay::fence_provisioning(&mut reloaded, "a--w", path, &source);
        journal.save(&reloaded)?;
        drop(journal);
        use provenant_os::fs::{Directory, LockMode};
        let directory = Directory::open_anchor(&project.join(".git"))?;
        let held = directory.lock(
            std::ffi::OsStr::new(provenant_confinement::worktrees::REPOSITORY_LOCK),
            LockMode::Exclusive,
            WaitBudget::nonblocking(clock.mono_now()),
            &cancel,
            &clock,
        )?;
        let restarted = Context::open(context.roots.clone(), &home, &project)?;
        let short = WaitBudget::up_to(
            clock.mono_now(),
            provenant_core::DurationMicros::from_micros(100_000),
        );
        serve(
            &restarted,
            "caller",
            json!({"op":"children"}),
            short,
            &cancel,
            &clock,
        )?;
        let saved = FileJournal::read_checkpoint(
            &restarted.roots.state,
            restarted.identity.project,
            "g-crash",
        )?
        .ok_or("checkpoint")?;
        assert_eq!(
            relay::pending_provisioning(&saved),
            vec![("a--w".into(), path.into(), source.clone())]
        );
        assert_eq!(saved.nodes[1].state, NodeState::Pending);
        assert!(saved.nodes[1].advance_owner_code.is_none());
        drop(held);

        // A mismatched source is another refusal before mutation; it cannot
        // reconcile the original checkout either.
        git(&["-C", path, "commit", "--allow-empty", "-qm", "later"])?;
        serve(
            &restarted,
            "caller",
            json!({"op":"children"}),
            budget,
            &cancel,
            &clock,
        )?;
        let saved = FileJournal::read_checkpoint(
            &restarted.roots.state,
            restarted.identity.project,
            "g-crash",
        )?
        .ok_or("checkpoint")?;
        assert_eq!(
            relay::pending_provisioning(&saved),
            vec![("a--w".into(), path.into(), source.clone())]
        );
        git(&["-C", path, "reset", "--hard", &source])?;

        // With the lock free and identity restored, a fresh bounded recovery
        // budget verifies the exact checkout despite the expired launch.
        let budget = WaitBudget::cli(clock.mono_now(), 5)?;
        serve(
            &restarted,
            "caller",
            json!({"op":"children"}),
            budget,
            &cancel,
            &clock,
        )?;
        let saved = FileJournal::read_checkpoint(
            &restarted.roots.state,
            restarted.identity.project,
            "g-crash",
        )?
        .ok_or("checkpoint")?;
        assert!(relay::pending_provisioning(&saved).is_empty());
        assert_eq!(saved.nodes[1].state, NodeState::Pending);
        assert_eq!(git(&["worktree", "list", "--porcelain"])?, before);
        assert_eq!(git(&["-C", path, "rev-parse", "HEAD"])?, source);

        // A partial add may leave only a branch. General worktree creation
        // accepts a descendant base; recovery must still require exact source.
        let mut journal = FileJournal::open(
            &restarted.roots.state,
            restarted.identity.project,
            "g-crash",
            budget,
            &cancel,
            &clock,
        )?;
        let mut saved = journal.load("g-crash")?.ok_or("checkpoint")?;
        relay::fence_provisioning(&mut saved, "a--w", path, &source);
        journal.save(&saved)?;
        drop(journal);
        git(&["-C", path, "commit", "--allow-empty", "-qm", "later"])?;
        git(&["worktree", "remove", path])?;
        let budget = WaitBudget::cli(clock.mono_now(), 5)?;
        serve(
            &restarted,
            "caller",
            json!({"op":"children"}),
            budget,
            &cancel,
            &clock,
        )?;
        let saved = FileJournal::read_checkpoint(
            &restarted.roots.state,
            restarted.identity.project,
            "g-crash",
        )?
        .ok_or("checkpoint")?;
        assert_eq!(
            relay::pending_provisioning(&saved),
            vec![("a--w".into(), path.into(), source.clone())]
        );
        git(&["-C", path, "reset", "--hard", &source])?;
        serve(
            &restarted,
            "caller",
            json!({"op":"children"}),
            WaitBudget::cli(clock.mono_now(), 5)?,
            &cancel,
            &clock,
        )?;
        let saved = FileJournal::read_checkpoint(
            &restarted.roots.state,
            restarted.identity.project,
            "g-crash",
        )?
        .ok_or("checkpoint")?;
        assert!(relay::pending_provisioning(&saved).is_empty());
        assert_eq!(git(&["worktree", "list", "--porcelain"])?, before);
        Ok(())
    }
}

#[cfg(test)]
mod panel_tests {
    use super::*;

    // Spec: docs/specs/routing.md#pinned-review-selection
    #[test]
    fn relayed_review_panels_use_their_explicit_source_not_the_callers()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_fabric_runtime::graph::{GraphBaseBinding, GraphCheckpoint, GraphDocument};

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
        git(&["commit", "--allow-empty", "-qm", "review base"])?;
        let base = git(&["rev-parse", "HEAD"])?;

        let release = project.join(".github/workflows/release.yml");
        let credentials = project.join("credentials/identity.json");
        std::fs::create_dir_all(release.parent().ok_or("release parent")?)?;
        std::fs::create_dir_all(credentials.parent().ok_or("credentials parent")?)?;
        std::fs::write(&release, "name: Release\n")?;
        std::fs::write(&credentials, "{}\n")?;
        git(&[
            "add",
            ".github/workflows/release.yml",
            "credentials/identity.json",
        ])?;
        git(&["commit", "-qm", "touch configured protected paths"])?;
        let protected_subject = git(&["rev-parse", "HEAD"])?;

        git(&["checkout", "-q", "-b", "docs-only", &base])?;
        let guide = project.join("docs/guide.md");
        std::fs::create_dir_all(guide.parent().ok_or("docs parent")?)?;
        std::fs::write(&guide, "Review guide\n")?;
        git(&["add", "docs/guide.md"])?;
        git(&["commit", "-qm", "touch docs only"])?;
        let docs_subject = git(&["rev-parse", "HEAD"])?;

        let mut root_inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
        root_inputs.state_dir = Some(root.path().join("state"));
        let roots = root_inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let review = provenant_routing::RoutingConfig::default().review;
        let crucial_reviewers: usize = review.shapes[&review.select.by_risk["crucial"]]
            .reviewers
            .iter()
            .map(|group| group.count)
            .sum();
        let library = TemplateLibrary::bundled();
        let limits = provenant_fabric_runtime::config::GraphLimits::default();
        let worktree_layout = provenant_config::PortTable::builtin().worktree_layout;

        for (caller_subject, child_subject, deep) in [
            (&protected_subject, &protected_subject, true),
            (&docs_subject, &docs_subject, false),
            (&docs_subject, &protected_subject, true),
        ] {
            let document: GraphDocument = serde_json::from_value(json!({
                "schema_version": 2,
                "graph_id": "relay-review",
                "review_base": base,
                "nodes": [{
                    "id": "caller",
                    "access": "read_only",
                    "prompt": "Caller",
                    "route": {"alias": "senior"},
                    "source": {"type": "commit", "commit": caller_subject}
                }]
            }))?;
            let mut checkpoint: GraphCheckpoint = serde_json::from_value(json!({
                "schema_version": 2,
                "graph_id": "relay-review",
                "plan_digest": "fixture",
                "nodes": [{
                    "id": "caller",
                    "state": "running",
                    "source_commit": caller_subject,
                    "attempts": []
                }]
            }))?;
            checkpoint.bases =
                GraphBaseBinding::new(Some(provenant_core::GitOid::parse(&base)?), None);

            let patch: provenant_fabric_runtime::graph::patch::Patch =
                serde_json::from_value(json!({
                    "schema_version": 1,
                    "expected_revision": 0,
                    "idempotency_key": "nested-review",
                    "nodes": [{
                        "id": "review",
                        "use": "review-panel",
                        "source": {"type": "commit", "commit": child_subject}
                    }]
                }))?;
            let subjects = relay_patch_review_subjects(&patch, &library)?;
            let add_request: provenant_fabric_runtime::graph::relay::ChildRequest =
                serde_json::from_value(json!({
                    "id": "review",
                    "use": "task",
                    "prompt": "Review the selected subject",
                    "idempotency_key": "nested-review",
                    "launch": {
                        "node": {
                            "id": "review",
                            "use": "review-panel",
                            "source": {"type": "commit", "commit": child_subject}
                        }
                    }
                }))?;
            assert_eq!(
                relay_child_review_subjects(&add_request, &library)?,
                subjects
            );
            let resolvers = launch_resolvers_for_relay(
                &roots,
                &project,
                &review,
                &document,
                &checkpoint,
                &subjects,
            )?;
            let lenses = &resolvers["review-panel"];
            if deep {
                assert!(lenses.len() >= crucial_reviewers, "{lenses:?}");
                for lens in ["correctness-spec", "reliability", "security-privacy"] {
                    assert!(lenses.iter().any(|selected| selected == lens), "{lenses:?}");
                }
                for stage in ["review-frame", "review-verify", "review-synthesis"] {
                    assert_eq!(resolvers[stage], vec!["true"]);
                }
            } else {
                assert_eq!(lenses.len(), 1, "{lenses:?}");
                for stage in ["review-frame", "review-verify", "review-synthesis"] {
                    assert_eq!(resolvers[stage], vec!["false"]);
                }
            }

            let mut document = document;
            document.template_resolvers = resolvers.clone();
            let admitted = provenant_fabric_runtime::graph::patch::admit_patch(
                &mut LiveGraph {
                    document: &document,
                    checkpoint: &mut checkpoint,
                },
                Some("caller"),
                &patch,
                &PatchContext {
                    library: &library,
                    resolvers: &resolvers,
                    limits: &limits,

                    journal: None,
                    worktree_layout: worktree_layout.clone(),
                },
            )?;
            assert!(!admitted.added.is_empty());
            let expanded = &checkpoint.expansions.last().ok_or("relay expansion")?.nodes;
            let review_leaves: Vec<_> = expanded
                .iter()
                .filter(|node| node.review_lens.is_some())
                .collect();
            assert_eq!(
                review_leaves.len() >= crucial_reviewers,
                deep,
                "{} leaves",
                review_leaves.len()
            );
        }

        let document: GraphDocument = serde_json::from_value(json!({
            "schema_version": 2,
            "graph_id": "relay-review-unresolved",
            "review_base": base,
            "nodes": [{
                "id": "caller",
                "access": "read_only",
                "prompt": "Caller",
                "route": {"alias": "senior"},
                "source": {"type": "commit", "commit": docs_subject}
            }]
        }))?;
        let mut checkpoint: GraphCheckpoint = serde_json::from_value(json!({
            "schema_version": 2,
            "graph_id": "relay-review-unresolved",
            "plan_digest": "fixture",
            "nodes": [{
                "id": "caller",
                "state": "running",
                "source_commit": docs_subject,
                "attempts": []
            }, {
                "id": "writer",
                "state": "running",
                "source_commit": docs_subject,
                "attempts": []
            }]
        }))?;
        checkpoint.bases = GraphBaseBinding::new(
            Some(provenant_core::GitOid::parse(&base)?),
            Some(provenant_core::GitOid::parse(&docs_subject)?),
        );

        let missing_add: provenant_fabric_runtime::graph::relay::ChildRequest =
            serde_json::from_value(json!({
                "id": "missing",
                "use": "review-panel",
                "prompt": "Review the selected subject",
                "idempotency_key": "missing-source"
            }))?;
        let dynamic_add: provenant_fabric_runtime::graph::relay::ChildRequest =
            serde_json::from_value(json!({
                "id": "dynamic",
                "use": "task",
                "prompt": "Review the selected subject",
                "idempotency_key": "dynamic-source",
                "launch": {
                    "node": {
                        "id": "dynamic",
                        "use": "review-panel",
                        "source": {"type": "head", "node": "writer"}
                    }
                }
            }))?;
        let integration_add: provenant_fabric_runtime::graph::relay::ChildRequest =
            serde_json::from_value(json!({
                "id": "integration",
                "use": "task",
                "prompt": "Review the selected subject",
                "idempotency_key": "integration-source",
                "launch": {
                    "node": {
                        "id": "integration",
                        "use": "review-panel",
                        "source": {"type": "integration"}
                    }
                }
            }))?;
        let missing = relay_child_review_subjects(&missing_add, &library)?;
        let dynamic = relay_child_review_subjects(&dynamic_add, &library)?;
        let integration = relay_child_review_subjects(&integration_add, &library)?;
        assert!(checkpoint.integration_pin.is_none());
        assert!(
            checkpoint
                .bases
                .and_then(|bases| bases.integration)
                .is_some()
        );
        for subjects in [&missing, &dynamic, &integration] {
            let resolvers = launch_resolvers_for_relay(
                &roots,
                &project,
                &review,
                &document,
                &checkpoint,
                subjects,
            )?;
            assert!(resolvers["review-panel"].len() >= crucial_reviewers);
            for stage in ["review-frame", "review-verify", "review-synthesis"] {
                assert_eq!(resolvers[stage], vec!["true".to_owned()]);
            }
            assert!(
                resolvers["review-advice-correctness-spec"][0]
                    .contains("configured unreadable-path floors were applied")
            );
        }

        let mixed_patch: provenant_fabric_runtime::graph::patch::Patch = serde_json::from_value(
            json!({
                "schema_version": 1,
                "expected_revision": 0,
                "idempotency_key": "mixed-review-sources",
                "nodes": [
                    {"id": "docs-review", "use": "review-panel", "source": {"type": "commit", "commit": docs_subject}},
                    {"id": "writer-review", "use": "review-panel", "source": {"type": "head", "node": "writer"}}
                ]
            }),
        )?;
        let mixed_subjects = relay_patch_review_subjects(&mixed_patch, &library)?;
        let ReviewDiffSelection::Incomplete(mixed_diffs) =
            relay_review_diffs(&project, &checkpoint, &mixed_subjects)
        else {
            return Err("mixed sources should retain the known diff and unresolved head".into());
        };
        assert_eq!(
            combine_review_diffs(&mixed_diffs).0,
            vec!["docs/guide.md".to_owned()]
        );
        let mixed_resolvers = launch_resolvers_for_relay(
            &roots,
            &project,
            &review,
            &document,
            &checkpoint,
            &mixed_subjects,
        )?;
        assert!(mixed_resolvers["review-panel"].len() >= crucial_reviewers);
        assert!(
            mixed_resolvers["review-advice-correctness-spec"][0]
                .contains("configured unreadable-path floors were applied")
        );
        Ok(())
    }

    // Spec: docs/specs/fabric.md#cli-and-relay-surfaces
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
        document.review = Some(provenant_routing::ReviewRequest {
            risk_tags: vec!["security".into()],
            ..Default::default()
        });
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
                "schema_version": 2, "graph_id": "panel-risk", "plan_digest": "fixture", "nodes": [],
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

                journal: None,

                worktree_layout: provenant_config::PortTable::builtin().worktree_layout,
            },
        )
        .expect("relay resolvers expand the complete review composite");
    }

    // Spec: docs/specs/routing.md#pinned-review-selection
    #[test]
    // Spec: docs/specs/fabric.md#composite-settlement
    fn incomplete_pinned_review_metadata_uses_configured_unreadable_path_floors() {
        let mut review = provenant_routing::RoutingConfig::default().review;
        // Keep a non-default floor in the fixture so this catches a fallback
        // that silently drops project-configured coverage.
        review.floors.insert(
            "project-private".into(),
            provenant_routing::ReviewFloor {
                paths: vec!["docs/private/**".into()],
                tier: "routine".into(),
                lenses: vec!["tests".into()],
                hard: false,
            },
        );
        let configured_floor_lenses = review
            .floors
            .values()
            .flat_map(|floor| floor.lenses.iter().cloned())
            .collect::<std::collections::BTreeSet<_>>();
        assert!(configured_floor_lenses.contains("tests"));
        let document = serde_json::from_value(json!({
            "schema_version": 1, "graph_id": "incomplete-review-context",
            "nodes": [{"id": "anchor", "access": "read_only", "prompt": "Review", "route": {"alias": "senior"}}],
            "review": {"kind": "docs", "risk_tier": "routine"}
        }))
        .unwrap();
        let incomplete_size =
            ReviewDiffSelection::Captured(vec![provenant_run::ReviewDiffContext {
                paths: vec!["docs/guide.md".into()],
                numstat: None,
                tree: None,
            }]);

        for selection in [ReviewDiffSelection::Incomplete(Vec::new()), incomplete_size] {
            let resolvers =
                launch_resolvers_with(&review, &document, selection, |_, _| Ok("fixture".into()))
                    .unwrap();
            let lenses = &resolvers["review-panel"];
            for lens in &configured_floor_lenses {
                assert!(
                    lenses.contains(lens),
                    "missing configured floor lens {lens}: {lenses:?}"
                );
            }
            for lens in ["correctness-spec", "reliability", "security-privacy"] {
                assert!(lenses.iter().any(|selected| selected == lens), "{lenses:?}");
            }
            assert_eq!(resolvers["review-frame"], vec!["true".to_owned()]);
            assert_eq!(resolvers["review-verify"], vec!["true".to_owned()]);
            assert_eq!(resolvers["review-synthesis"], vec!["true".to_owned()]);
            let floor: Value =
                serde_json::from_str(&resolvers["review-coverage-floor"][0]).unwrap();
            assert_eq!(floor["hard"], false, "bundled floors never refuse");
            assert!(floor["required_lenses"].as_array().unwrap().is_empty());
            assert!(
                resolvers["review-advice-correctness-spec"][0]
                    .contains("configured unreadable-path floors were applied")
            );
        }

        let empty_diff = ReviewDiffSelection::Captured(vec![provenant_run::ReviewDiffContext {
            paths: Vec::new(),
            numstat: Some(String::new()),
            tree: Some(String::new()),
        }]);
        let quick =
            launch_resolvers_with(&review, &document, empty_diff, |_, _| Ok("fixture".into()))
                .unwrap();
        assert!(
            quick
                .values()
                .flatten()
                .all(|value| !value.contains("configured unreadable-path floors were applied"))
        );
        assert!(!quick["review-panel"].iter().any(|lens| lens == "tests"));
        let quick_floor: Value = serde_json::from_str(&quick["review-coverage-floor"][0]).unwrap();
        assert_eq!(quick_floor["hard"], false);

        let boundary_diff = || {
            ReviewDiffSelection::Captured(vec![provenant_run::ReviewDiffContext {
                paths: vec!["credentials/token.json".into(), "src/unmatched.rs".into()],
                numstat: Some(
                    concat!("1\t0\tcredentials/token.json\0", "1\t0\tsrc/unmatched.rs\0").into(),
                ),
                tree: Some("tools/front-door/Cargo.toml".into()),
            }])
        };
        // A bundled boundary floor adds its lens, never deep, and never refuses.
        let bundled = launch_resolvers_with(&review, &document, boundary_diff(), |_, _| {
            Ok("fixture".into())
        })
        .unwrap();
        let floor: Value = serde_json::from_str(&bundled["review-coverage-floor"][0]).unwrap();
        assert_eq!(floor["hard"], false);
        assert!(floor["required_lenses"].as_array().unwrap().is_empty());
        assert!(
            bundled["review-panel"]
                .iter()
                .any(|lens| lens == "security-privacy")
        );
        let deep_reviewers: usize = review.shapes["deep"]
            .reviewers
            .iter()
            .map(|g| g.count)
            .sum();
        assert!(floor["minimum"].as_u64().unwrap() < deep_reviewers as u64);

        // Only a floor the project declares hard refuses, and only its own
        // lenses are required: the unknown class's lens is not.
        let mut project = review.clone();
        project.floors.insert(
            "project-boundary".into(),
            provenant_routing::ReviewFloor {
                paths: vec!["credentials/**".into()],
                tier: "crucial".into(),
                lenses: vec!["correctness-spec".into(), "adversarial".into()],
                hard: true,
            },
        );
        project.floors.get_mut("unknown").unwrap().lenses = vec!["tests".into()];
        let protected = launch_resolvers_with(&project, &document, boundary_diff(), |_, _| {
            Ok("fixture".into())
        })
        .unwrap();
        let floor: Value = serde_json::from_str(&protected["review-coverage-floor"][0]).unwrap();
        assert_eq!(floor["hard"], true);
        let required = floor["required_lenses"].as_array().unwrap();
        assert!(required.iter().any(|lens| lens == "adversarial"));
        assert!(!required.iter().any(|lens| lens == "tests"));
        let hard_reviewers: usize = project.shapes[&project.select.by_risk["crucial"]]
            .reviewers
            .iter()
            .map(|g| g.count)
            .sum();
        assert_eq!(floor["minimum"], hard_reviewers);

        let library = TemplateLibrary::bundled();
        let compiled = provenant_fabric_runtime::graph::template::Compiler {
            library: &library,
            resolvers: &protected,
        }
        .compile(&json!({"id":"protected-panel", "use":"review-panel", "min_ok":1}))
        .unwrap();
        let root = compiled.composite.as_ref().unwrap();
        assert!(root.hard_review_coverage);
        assert!(root.required_review_lenses.contains(&"adversarial".into()));
        let shape = root.nodes[0].composite.as_ref().unwrap();
        let lenses = shape.nodes.iter().find(|node| node.id == "lenses").unwrap();
        let lens_floor = lenses.composite.as_ref().unwrap();
        assert!(lens_floor.hard_review_coverage);
        assert_eq!(lens_floor.min_successful, Some(hard_reviewers));
        assert!(
            lens_floor
                .required_review_lenses
                .contains(&"adversarial".into())
        );

        let docs_policy = launch_resolvers_with(
            &review,
            &document,
            ReviewDiffSelection::Captured(vec![provenant_run::ReviewDiffContext {
                paths: vec!["docs/guide.md".into()],
                numstat: Some("1\t0\tdocs/guide.md\0".into()),
                tree: Some("tools/front-door/Cargo.toml".into()),
            }]),
            |_, _| Ok("fixture".into()),
        )
        .unwrap();
        let floor: Value = serde_json::from_str(&docs_policy["review-coverage-floor"][0]).unwrap();
        assert_eq!(floor["hard"], false);
    }

    #[test]
    fn review_family_constraints_use_eligible_pools_and_survive_template_expansion()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_routing::{
            Catalogue, RouteChoice, RouteName, RouteRequest, RoutingConfig, RoutingContext,
        };
        let catalogue = Catalogue::bundled()?;
        let config = RoutingConfig::default();
        let family = provenant_records::AdapterSpec::get("codex")
            .ok_or("adapter")?
            .family;
        let named_family = provenant_records::AdapterSpec::get("claude")
            .ok_or("adapter")?
            .family;
        let availability: Vec<_> = ["codex", "claude"]
            .into_iter()
            .map(|adapter| provenant_providers::AvailabilityRecord {
                schema_version: 1,
                adapter: adapter.into(),
                executable: format!("/fixture/{adapter}"),
                version: "fixture".into(),
                last_update: None,
                fixture_ref: "test".into(),
                catalogue_revision: catalogue.revision().to_owned(),
                account_key: Some("fixture-account".into()),
                plan: None,
                observed_models: vec!["test".into()],
                observed_unix_seconds: 80,
                expires_unix_seconds: 120,
            })
            .collect();
        let access = ["codex", "claude"].map(|adapter| provenant_routing::AccessContext {
            adapter,
            disclosure_ref: "fixture",
            confinement_ref: "fixture",
        });
        let context = RoutingContext {
            policy_account_hint: None,
            choice_context: None,
            selection_evidence: None,
            catalogue: &catalogue,
            config: &config,
            registry: provenant_providers::REGISTRY,
            availability: &availability,
            quota: &[],
            capacity_cooldowns: &[],
            accounts: &[],
            quota_policies: &[],
            readiness: None,
            access: &access,
            now: provenant_core::WallTime::from_micros(100_000_000)?,
            host: "family-fixture",
        };
        let document = serde_json::from_value(json!({
            "schema_version": 1, "graph_id": "family-panel",
            "nodes": [{"id": "anchor", "access": "read_only", "prompt": "Task", "route": {"alias": "senior"}}],
            "review": {
                "shape": "standard", "minimum_family_counts": {(family): 2},
                "lenses": [
                    {"name": "correctness", "tier": "senior"},
                    {"name": "tests", "tier": "senior"},
                    {"name": "contract", "tier": "senior", "required_family": named_family}
                ]
            }
        }))?;
        let resolvers = launch_resolvers_with(
            &config.review,
            &document,
            ReviewDiffSelection::NotRequested,
            |slot, required| {
                let result = provenant_routing::resolve_for_account_with_seed_excluding_family(
                    &RouteRequest {
                        intent: Default::default(),
                        choice: RouteChoice::NamedRoute(
                            RouteName::parse(slot.tier.as_str().unwrap()).unwrap(),
                        ),
                        effort: None,
                        confidential: false,
                        fallback: provenant_routing::Fallback::None,
                    },
                    &context,
                    None,
                    "family-panel",
                    0,
                    &[],
                    &[],
                    required,
                )
                .map_err(|error| error.to_string())?;
                Ok(provenant_records::AdapterSpec::get(&result.adapter)
                    .unwrap()
                    .family
                    .to_owned())
            },
        )?;
        let record: provenant_routing::PanelResolution =
            serde_json::from_str(&resolvers["review-family-resolution"][0])?;
        assert_eq!(
            record
                .slots
                .iter()
                .filter(|slot| slot.satisfied_family.as_deref() == Some(family))
                .count(),
            2
        );
        assert_eq!(
            record.slots[2].satisfied_family.as_deref(),
            Some(named_family)
        );
        let library = TemplateLibrary::bundled();
        let compiled = provenant_fabric_runtime::graph::template::Compiler {
            library: &library,
            resolvers: &resolvers,
        }
        .compile(&json!({
            "id": "review", "use": "review", "over": "@review-panel", "with": {"lens": "${item}"}
        }))
        .map_err(|error| format!("{error:?}"))?;
        for (member, slot) in compiled
            .composite
            .ok_or("panel")?
            .nodes
            .iter()
            .zip(&record.slots)
        {
            let wire = serde_json::to_value(member)?;
            let routes = wire
                .get("routes")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_else(|| vec![wire["route"].clone()]);
            assert!(!routes.is_empty());
            for route in routes {
                assert_eq!(
                    route["required_family"].as_str(),
                    slot.satisfied_family.as_deref()
                );
            }
        }
        let refused = launch_resolvers_with(
            &config.review,
            &document,
            ReviewDiffSelection::NotRequested,
            |_, _| Err("candidate account unavailable; excluded by policy".into()),
        )
        .unwrap_err();
        assert!(refused.contains(named_family));
        assert!(refused.contains("count 1"));
        assert!(refused.contains("candidate account unavailable; excluded by policy"));
        Ok(())
    }
}
