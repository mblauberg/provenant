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
use provenant_records::warning::{Warning, warn};
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

fn same_path_at_checkout(candidate: &str, expected: &str, cwd: &std::path::Path) -> bool {
    let candidate = std::path::Path::new(candidate);
    let candidate = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        cwd.join(candidate)
    };
    let expected = std::path::Path::new(expected);
    candidate == expected
        || candidate
            .canonicalize()
            .ok()
            .zip(expected.canonicalize().ok())
            .is_some_and(|(candidate, expected)| candidate == expected)
}

fn verify_live_sealed_worktree(
    context: &Context,
    sealed: &provenant_records::custody::Subject,
) -> Result<String, ServiceError> {
    let identity = sealed.worktree_identity().ok_or_else(|| {
        refused(
            "writer_parent_required",
            "sealed parent worktree identity is invalid",
        )
    })?;
    let current = crate::fabric::inspect_checkout_subject(
        &context.roots,
        std::path::Path::new(&identity.path),
    )
    .map_err(|error| refused("writer_parent_required", error.to_string()))?;
    if !crate::run::nested_authority::same_worktree_incarnation(Some(sealed), current.as_ref()) {
        return Err(refused(
            "writer_parent_required",
            "parent worktree incarnation changed after launch",
        ));
    }
    Ok(identity.path)
}

/// A row the scheduler settled as skipped takes no writer effect: no binding,
/// no provisioning, no owner stamping. The checkpoint state is authoritative,
/// so effect sites never re-read a plan's `when` predicate.
fn skipped_row(checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint, id: &str) -> bool {
    use provenant_fabric_runtime::graph::NodeState;
    checkpoint
        .nodes
        .iter()
        .any(|row| row.id == id && row.state == NodeState::Skipped)
}

/// Existing nested writer checkouts require the owning parent's exact sealed
/// incarnation. New checkouts are provisioned independently. Bind the verified
/// directory now so a later removal/recreation cannot substitute another tree
/// between relay admission and shared occupancy acquisition.
fn bind_shared_writers(
    context: &Context,
    checkpoint: &mut provenant_fabric_runtime::graph::GraphCheckpoint,
    sealed: Option<&provenant_records::custody::Subject>,
) -> Result<(), ServiceError> {
    use provenant_fabric_runtime::graph::{AccessMode, WriterWorktreeBinding};
    let Some(expansion) = checkpoint.expansions.last() else {
        return Ok(());
    };
    let writers = expansion
        .nodes
        .iter()
        .chain(expansion.patch.iter().flat_map(|patch| &patch.replaced))
        .filter(|node| {
            node.access == AccessMode::WorktreeWrite
                && !node.provision_worktree
                && !skipped_row(checkpoint, &node.id)
        })
        .cloned()
        .collect::<Vec<_>>();
    for node in writers {
        let sealed = sealed.ok_or_else(|| {
            refused(
                "writer_parent_required",
                "an existing writer checkout needs its owning lane's authority",
            )
        })?;
        let path = verify_live_sealed_worktree(context, sealed)?;
        if !node
            .worktree
            .as_deref()
            .is_some_and(|candidate| same_path_at_checkout(candidate, &path, &context.cwd))
        {
            return Err(refused(
                "writer_parent_required",
                "a nested writer may share only its owning lane's worktree; provision a new checkout for independent work",
            ));
        }
        let request = provenant_run::resolve_writer(
            context.roots.state.as_path(),
            &context.cwd,
            std::path::Path::new(&path),
        )
        .map_err(|error| refused("writer_parent_required", error.to_string()))?;
        let writer = provenant_run::prepare_writer(
            &request,
            context.roots.state.as_path(),
            context.identity.project,
        )
        .map_err(|error| refused("writer_parent_required", error.to_string()))?;
        // Revalidate the authority after preparation, which can run Git probes.
        verify_live_sealed_worktree(context, sealed)?;
        writer
            .worktree
            .repo()
            .revalidate_identity()
            .map_err(|error| refused("writer_parent_required", error.to_string()))?;
        let identity = writer.worktree.identity();
        let progress = checkpoint
            .nodes
            .iter_mut()
            .find(|row| row.id == node.id)
            .ok_or_else(|| refused("invalid_graph", "shared writer progress missing"))?;
        progress.writer_binding = Some(WriterWorktreeBinding {
            extra: Default::default(),
            shared_worktree: Some(sealed.clone()),
            source_directory_identity: Some(provenant_records::run::SourceIdentity {
                device: identity.device,
                inode: identity.inode,
            }),
            worktree: path,
            head: writer.source_commit().to_string(),
        });
        if node.source.is_none() && node.continues.is_none() && progress.source_commit.is_none() {
            progress.source_commit = Some(writer.source_commit().to_string());
        }
        progress.writer_binding_pending = false;
    }
    Ok(())
}

/// Replay and fresh admission expose the same durable advisory warnings to
/// the owning lane, alongside the child receipt or patch revision.
fn admission_value(
    mut value: Value,
    checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
) -> Value {
    if let Some(revision) = value["revision"].as_u64() {
        let warnings = checkpoint
            .expansions
            .iter()
            .filter(|expansion| {
                expansion
                    .patch
                    .as_ref()
                    .is_some_and(|patch| patch.revision == revision)
            })
            .flat_map(|expansion| expansion.floor_warnings.iter())
            .take(64)
            .cloned()
            .collect::<Vec<_>>();
        if !warnings.is_empty() {
            value["warnings"] = json!(warnings);
        }
    }
    value
}

fn provisioning_owner_digest(
    graph: &str,
    node: &str,
) -> Result<provenant_core::Digest, ServiceError> {
    provenant_records::CanonicalV1::record_digest("nested-worktree-owner", 1, &(graph, node))
        .map_err(|error| refused("invalid_graph", error.to_string()))
}

/// Assign lifetime creation authority before examining source readiness or
/// active scheduling. A deferred writer must not fall back to host authority.
fn bind_admitted_provisioning_owners(
    checkpoint: &mut provenant_fabric_runtime::graph::GraphCheckpoint,
) -> Result<(), ServiceError> {
    use provenant_fabric_runtime::graph::{AccessMode, NodeState};
    let writers = checkpoint
        .expansions
        .last()
        .into_iter()
        .flat_map(|expansion| {
            expansion
                .nodes
                .iter()
                .chain(expansion.patch.iter().flat_map(|patch| &patch.replaced))
        })
        .filter(|node| {
            node.access == AccessMode::WorktreeWrite
                && node.provision_worktree
                && !skipped_row(checkpoint, &node.id)
        })
        .filter_map(|node| {
            node.worktree
                .as_ref()
                .map(|path| (node.id.clone(), path.clone()))
        })
        .collect::<Vec<_>>();
    let mut by_path = std::collections::BTreeMap::new();
    for (id, path) in writers {
        let logical = relay::provisioning_owner(checkpoint, &id);
        let owner = match by_path.get(&path) {
            Some(owner) => *owner,
            None => {
                let owner = provisioning_owner_digest(&checkpoint.graph_id, &logical)?;
                by_path.insert(path.clone(), owner);
                owner
            }
        };
        for row in &mut checkpoint.nodes {
            if row.state == NodeState::Skipped {
                continue;
            }
            if row.id == id || row.id == logical || row.worktree.as_deref() == Some(path.as_str()) {
                if row.provisioned_owner.is_some_and(|prior| prior != owner) {
                    return Err(refused(
                        "writer_parent_required",
                        "nested provisioning belongs to another lane",
                    ));
                }
                row.provisioned_owner = Some(owner);
            }
        }
    }
    Ok(())
}

fn bind_provisioning_owner(
    checkpoint: &mut provenant_fabric_runtime::graph::GraphCheckpoint,
    node: &str,
) -> Result<(), ServiceError> {
    let path = checkpoint
        .pending_writer_provisioning
        .get(node)
        .map(|pending| pending.path.clone());
    let recorded = checkpoint
        .nodes
        .iter()
        .filter(|row| {
            row.id == node
                || path
                    .as_ref()
                    .is_some_and(|path| row.worktree.as_ref() == Some(path))
        })
        .find_map(|row| row.provisioned_owner);
    let owner = recorded.unwrap_or(provisioning_owner_digest(&checkpoint.graph_id, node)?);
    if let Some(pending) = checkpoint.pending_writer_provisioning.get_mut(node) {
        pending.owner = Some(owner);
        for row in &mut checkpoint.nodes {
            if row.id == node || row.worktree.as_deref() == Some(pending.path.as_str()) {
                if row.provisioned_owner.is_some_and(|prior| prior != owner) {
                    return Err(refused(
                        "writer_parent_required",
                        "nested provisioning belongs to another lane",
                    ));
                }
                row.provisioned_owner = Some(owner);
            }
        }
    }
    Ok(())
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

/// The deadline and cancellation context of one relay call.
pub struct RelayCall<'a> {
    pub budget: WaitBudget,
    pub cancel: &'a CancelToken,
    pub clock: &'a dyn Clock,
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
    serve_authorised(
        context,
        attempt,
        arguments,
        RelayCall {
            budget,
            cancel,
            clock,
        },
        None,
        false,
        read,
    )
}

/// Serve a lane graph operation using the exact parent worktree incarnation
/// sealed in its host handoff. This prevents a stale graph path from granting
/// shared checkout authority after that path has been recreated.
pub fn serve_with_read_and_parent(
    context: &Context,
    attempt: &str,
    arguments: Value,
    call: RelayCall<'_>,
    sealed_parent_worktree: Option<&provenant_records::custody::Subject>,
    read: &impl Fn(Value) -> Result<Value, ServiceError>,
) -> Result<Value, ServiceError> {
    serve_authorised(
        context,
        attempt,
        arguments,
        call,
        sealed_parent_worktree,
        true,
        read,
    )
}

#[allow(
    clippy::print_stderr,
    reason = "warn operator when policy is unavailable and defaults are used"
)]
fn serve_authorised(
    context: &Context,
    attempt: &str,
    arguments: Value,
    call: RelayCall<'_>,
    sealed_parent_worktree: Option<&provenant_records::custody::Subject>,
    nested: bool,
    read: &impl Fn(Value) -> Result<Value, ServiceError>,
) -> Result<Value, ServiceError> {
    let RelayCall {
        budget,
        cancel,
        clock,
    } = call;
    fn contains_provision_flag(value: &Value) -> bool {
        match value {
            Value::Object(fields) => {
                fields.contains_key("provision_worktree")
                    || fields.values().any(contains_provision_flag)
            }
            Value::Array(items) => items.iter().any(contains_provision_flag),
            _ => false,
        }
    }
    if nested
        && matches!(arguments["op"].as_str(), Some("add" | "edit"))
        && contains_provision_flag(&arguments)
    {
        return Err(refused(
            "writer_parent_required",
            "worktree provisioning is derived by the host; omit provision_worktree",
        ));
    }
    let mutating_input = matches!(arguments["op"].as_str(), Some("add" | "edit" | "run"));
    let policy = if mutating_input {
        match crate::config_reads::read_for(&context.roots, "run admission") {
            Ok(config) => config.domains.policy.resolve_with_knobs(
                &context.identity.project.to_string(),
                &config.knobs,
                &Default::default(),
            ),
            Err(error) => {
                let error = provenant_records::diagnostic::safe_text(&error.to_string());
                warn(Warning::new(
                    provenant_records::warning::CONFIG_UNAVAILABLE,
                    format!(
                        "graph admission policy unavailable ({error}); using compiled defaults"
                    ),
                    provenant_records::warning::detected_source(
                        provenant_records::warning::CONFIG_UNAVAILABLE,
                    ),
                ));
                Default::default()
            }
        }
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
            let mut value = admission_value(serde_json::to_value(receipt)?, &checkpoint);
            merge_warnings(&mut value, &warnings);
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
            .unwrap_or(&graph_id)
            .to_owned();
        let local = requested
            .strip_prefix(&format!("{graph_id}/"))
            .unwrap_or(&requested);
        let node = local.split_once('#').map_or(local, |(node, _)| node);
        // Inside a lane a target names the calling graph's own nodes, as
        // `NAME[/NODE[#N]]` or a bare `NODE[#N]` (cli.md "Targets"). Attempt
        // IDs are not targets, so they resolve to nothing here.
        if requested == graph_id || checkpoint.nodes.iter().any(|row| row.id == node) {
            let selected = if requested == graph_id {
                graph_id.clone()
            } else {
                format!("{graph_id}/{local}")
            };
            input["id"] = selected.into();
            return read(input);
        }
        // A lane may also read any other graph launched by its own chair
        // session. The submitting chair is sealed on the graph's custody
        // record at submission; compare it with the calling graph's chair.
        // Project scope is implicit: the chair index is read in this project.
        let target_graph = requested
            .split_once('/')
            .map_or(requested.as_str(), |(graph, _)| graph);
        let outside = || {
            refused(
                "not_found",
                "the target graph is outside the calling lane's chair session",
            )
        };
        if target_graph.is_empty() || target_graph == graph_id {
            return Err(outside());
        }
        let chair_call = provenant_fabric_runtime::chairs::Request { clock, cancel };
        #[allow(clippy::disallowed_methods)] // Native read-only store open deadline.
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_micros(budget.remaining(clock.mono_now()).as_micros());
        let index = provenant_fabric_runtime::chairs::ChairIndex::open_read_only(
            &context.roots.state,
            deadline,
        )
        .ok()
        .flatten()
        .ok_or_else(outside)?;
        let session_of = |graph: &str| {
            index
                .read(project, graph, &chair_call)
                .ok()
                .flatten()
                .and_then(|record| record.holder.chair().cloned())
                .and_then(|chair| chair.session)
                .filter(|session| !session.is_empty())
        };
        let own_session = session_of(&graph_id);
        if own_session.is_none() || own_session != session_of(target_graph) {
            return Err(outside());
        }
        // Reads stay read-only; the host reader resolves the full target in
        // the sibling graph. Steer, stop and retry still resolve only inside
        // the calling graph in the branches below.
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
            let mut value = admission_value(serde_json::to_value(receipt)?, &checkpoint);
            merge_warnings(&mut value, &warnings);
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
                    let mut value = admission_value(serde_json::to_value(receipt)?, &checkpoint);
                    merge_warnings(&mut value, &warnings);
                    Ok(value)
                }
                None => Err(not_a_node()),
            };
        }
        (None, _) => return Err(not_a_node()),
    };
    let value = match op {
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
                let mut value = admission_value(serde_json::to_value(receipt)?, &checkpoint);
                merge_warnings(&mut value, &warnings);
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
            // The host decides one spelling of every model a child route adds,
            // so relay children reach launch with the same full ids as edits.
            let catalogue = crate::route::effective_catalogue(&context.roots).ok();
            let patch_context = PatchContext {
                catalogue: catalogue.as_ref(),
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
                if nested && let Some(parent_path) = parent_path.as_deref() {
                    let sealed = sealed_parent_worktree.ok_or_else(|| {
                        refused(
                            "writer_parent_required",
                            "host handoff has no parent worktree authority",
                        )
                    })?;
                    let path = verify_live_sealed_worktree(context, sealed)?;
                    if !same_path_at_checkout(parent_path, &path, &context.cwd) {
                        return Err(refused(
                            "writer_parent_required",
                            "parent worktree differs from host handoff",
                        ));
                    }
                }
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
                let mut value = admission_value(serde_json::to_value(receipt)?, &checkpoint);
                merge_warnings(&mut value, &warnings);
                return Ok(value);
            }
            if nested {
                bind_admitted_provisioning_owners(&mut checkpoint)?;
            }
            if request.needs_worktree_provisioning()
                && !skipped_row(&checkpoint, &receipt.node)
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
                if nested {
                    bind_provisioning_owner(&mut checkpoint, &receipt.node)?;
                    bind_shared_writers(context, &mut checkpoint, sealed_parent_worktree)?;
                }
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
            } else if nested {
                bind_shared_writers(context, &mut checkpoint, sealed_parent_worktree)?;
            }
            provision_added_writers(
                context,
                &mut journal,
                &mut checkpoint,
                &[],
                budget,
                cancel,
                nested,
            )?;
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
            let catalogue = crate::route::effective_catalogue(&context.roots).ok();
            let admitted = provenant_fabric_runtime::graph::patch::admit_patch(
                &mut LiveGraph {
                    document: &document,
                    checkpoint: &mut checkpoint,
                },
                Some(&parent),
                &patch,
                &PatchContext {
                    catalogue: catalogue.as_ref(),
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
            if nested && !admitted.replayed {
                bind_admitted_provisioning_owners(&mut checkpoint)?;
                bind_shared_writers(context, &mut checkpoint, sealed_parent_worktree)?;
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
                nested,
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
    let mut value = admission_value(value, &checkpoint);
    if let Some(warnings) = value.get("warnings").and_then(Value::as_array)
        && let Some(owner) = checkpoint.nodes.iter_mut().find(|row| row.id == parent)
    {
        for warning in warnings.iter().filter_map(Value::as_str) {
            if owner.warnings.len() < 64 && !owner.warnings.iter().any(|old| old == warning) {
                owner.warnings.push(warning.to_owned());
            }
        }
    }
    journal.save(&checkpoint).map_err(journal_error)?;
    merge_warnings(&mut value, &warnings);
    Ok(value)
}

/// Add this request's advisory input warnings to any durable admission
/// warnings already on the reply, sorted and deduplicated.
fn merge_warnings(value: &mut Value, extra: &[provenant_records::warning::Warning]) {
    if extra.is_empty() {
        return;
    }
    let mut all: Vec<String> = value
        .get("warnings")
        .and_then(Value::as_array)
        .map(|existing| {
            existing
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    all.extend(extra.iter().map(|warning| warning.message.clone()));
    all.sort();
    all.dedup();
    value["warnings"] = json!(all);
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
    nested: bool,
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
        if nested {
            bind_provisioning_owner(checkpoint, &owner)?;
        }
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
        if skipped_row(checkpoint, &node) {
            // A skipped node takes no writer effect. Drop a fence left by an
            // earlier admission so recovery cannot create its worktree.
            relay::clear_provisioning(checkpoint, &node);
            continue;
        }
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
            Some(branch) => provision_writer_checked(
                context,
                &path,
                branch,
                &source,
                checkpoint
                    .pending_writer_provisioning
                    .get(&node)
                    .and_then(|pending| pending.owner.as_ref()),
                launch,
                cancel,
            ),
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
                WorktreeError::RegistryLockTimeout { .. }
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
                    | WorktreeError::WorktreeExists { .. }
                    | WorktreeError::UnrelatedBase(..)
                    | WorktreeError::BranchCollision { .. }
                    | WorktreeError::RemoteOnlyBranch { .. }
                    | WorktreeError::BaseNotFound { .. }
                    | WorktreeError::BaseUnverified { .. }
                    | WorktreeError::StaleRegistration
                    | WorktreeError::CreateFailed {
                        artifacts_absent: true,
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
                progress.advance_code = Some("launch_refused".into());
                progress.advance_owner_code = Some(error.code().into());
                progress.advance_note = Some(format!(
                    "writer provisioning refused [{}]: {}; correct the cause, then retry with `provenant retry {}/{node}`",
                    error.code(),
                    error.to_string().chars().take(512).collect::<String>(),
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
    catalogue: Option<&provenant_routing::Catalogue>,
) -> Result<
    (
        &'static str,
        Value,
        Vec<provenant_records::warning::Warning>,
    ),
    String,
> {
    let mut warnings = Vec::new();
    if tier.is_array() {
        if effort.is_some() {
            return Err(format!(
                "node {id}: route lists cannot be combined with effort"
            ));
        }
        let mut routes = tier.clone();
        for route in routes.as_array_mut().into_iter().flatten() {
            if let Some(warning) = provenant_routing::canonical_route_value(route, catalogue) {
                warnings.push(warning);
            }
        }
        return Ok(("routes", routes, warnings));
    }
    let mut route = match tier {
        Value::String(name) if !name.is_empty() => {
            if name.contains('/') {
                // An exact tier is one full catalogue id. The model id itself
                // may contain '/', so only parse it once and keep its spelling.
                let (text, embedded_effort) = name
                    .split_once('@')
                    .map_or((name.as_str(), None), |(text, effort)| (text, Some(effort)));
                let model = provenant_routing::ModelId::parse(text)
                    .map_err(|_| format!("node {id}: invalid exact-model tier {name}"))?;
                if embedded_effort == Some("") {
                    return Err(format!("node {id}: invalid exact-model tier {name}"));
                }
                if embedded_effort.is_some() && effort.is_some() {
                    return Err(format!(
                        "node {id}: exact-model tier and effort both specify effort"
                    ));
                }
                let mut route = json!({"adapter":model.adapter(),"model":model.as_str()});
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
    // Tiers built from an adapter and a relative model must reach resolution
    // as one full id, just like an admission route. Alias-only routes are
    // untouched. Spec: docs/specs/fabric.md#one-model-spelling-per-route
    if let Some(warning) = provenant_routing::canonical_route_value(&mut route, catalogue) {
        warnings.push(warning);
    }
    if let Some(effort) = effort {
        route["effort"] = effort.clone();
    }
    let alias_only = route.get("alias").is_some() && route.get("model").is_none();
    if read_only && alias_only {
        Ok(("routes", json!([route.clone(), route]), warnings))
    } else {
        Ok(("route", route, warnings))
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
        None,
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
    // Review panel tiers lower with the same effective catalogue the host
    // uses at submit, so a discovered model is never treated as uncatalogued.
    let catalogue = crate::route::effective_catalogue(roots).ok();
    launch_resolvers_with(
        review,
        document,
        pinned_diff,
        catalogue.as_ref(),
        |slot, family| crate::route::resolve_review_family(roots, &slot.tier, family, &exclude),
    )
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
        Some(launch) => library.with(Layer::Launch, &launch.templates)?,
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
    let library = library.with(Layer::Launch, &patch.templates)?;
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
    catalogue: Option<&provenant_routing::Catalogue>,
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
        let (key, mut route, warnings) = tier_route(&slot.lens, &slot.tier, None, true, catalogue)?;
        for warning in warnings {
            provenant_records::warning::warn(warning);
        }
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
    provision_child_with_budget(context, path, source, None, budget, cancel)
}

/// Provision against an already-started launch deadline; never renew it.
pub fn provision_child_with_budget(
    context: &Context,
    path: &str,
    source: &str,
    owner: Option<&provenant_core::Digest>,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<(), ServiceError> {
    let branch = context
        .port_table
        .managed_writer_branch(std::path::Path::new(path))
        .map(str::to_owned)
        .ok_or_else(|| refused("writer_worktree", "invalid managed worktree path"))?;
    provision_writer_checked(context, path, &branch, source, owner, budget, cancel)
        .map(|_| ())
        .map_err(provisioning_refusal)
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
    provision_writer_checked(context, path, branch, source, None, budget, cancel)
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
    if let WorktreeError::WorktreeExists { reason, .. } = &error {
        // `Display` names the branch and path but drops the typed reason,
        // which is the actionable part of an existing-checkout refusal. Keep
        // it in the refusal the lane sees, as for a managed-path mismatch.
        return ServiceError::Facade(FacadeError::DispatchRefused {
            reason: format!("{error}: {reason}"),
            disk: None,
            fact: None,
        });
    }
    let fact = match &error {
        WorktreeError::Dirty(paths) => Some(DispatchRefusalFact::DirtyWriter {
            paths: provenant_records::diagnostic::safe_text(paths),
        }),
        WorktreeError::DetachedWorktree { .. } | WorktreeError::DetachedPrimary => {
            Some(DispatchRefusalFact::DetachedWriter)
        }
        WorktreeError::CheckedOut => Some(DispatchRefusalFact::CheckedOutWriter),
        WorktreeError::RegistryLockTimeout { holder } => {
            return ServiceError::Facade(FacadeError::WriterLockBusy { holder: *holder });
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
    // A declared root may be parent-relative or absolute; compare canonical
    // paths so `../outside/<branch>` matches the registered checkout path.
    let canonical = |value: std::path::PathBuf| std::fs::canonicalize(&value).unwrap_or(value);
    let requested = canonical(project_root.join(path));
    let expected = canonical(project_root.join(expected));
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
    owner: Option<&provenant_core::Digest>,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<std::path::PathBuf, provenant_confinement::worktrees::WorktreeError> {
    use provenant_confinement::worktrees;
    let clock = provenant_os::clock::SystemClock::new();
    if budget.remaining(clock.mono_now()).as_micros() == 0 {
        return Err(worktrees::WorktreeError::RegistryLockTimeout {
            holder: worktrees::recorded_writer_holder(
                context.roots.state.as_path(),
                context.identity.project,
                &context.project_root.join(path),
            ),
        });
    }
    worktrees::with_context(
        &context.roots,
        &context.home,
        &context.cwd,
        &clock,
        cancel,
        |worktree_context| {
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
            if budget.remaining(clock.mono_now()).as_micros() == 0 {
                return Err(worktrees::WorktreeError::RegistryLockTimeout {
                    holder: worktrees::writer_holder(worktree_context.project_state, &expected),
                });
            }
            provision_in_context(&worktree_context, path, branch, source, owner, cancel)?;
            Ok(expected)
        },
    )
}

fn provision_in_context(
    context: &provenant_confinement::worktrees::WorktreeContext<'_>,
    path: &str,
    branch: &str,
    source: &str,
    owner: Option<&provenant_core::Digest>,
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
        if let Some(owner) = owner {
            let receipt = tree
                .repo()
                .private_dir()
                .read_stable(std::ffi::OsStr::new("provenant-provision-owner"), 256)
                .map_err(|_| WorktreeError::UnknownWriter)?;
            if receipt != owner.to_string().as_bytes() {
                return Err(WorktreeError::UnknownWriter);
            }
        }
        let _writer = worktrees::cache_writer_guard(context.project_state, tree.path()).map_err(
            |occupancy| match occupancy {
                worktrees::Occupancy::Held => WorktreeError::WriterBusy {
                    holder: worktrees::writer_holder(context.project_state, tree.path()).or_else(
                        || {
                            worktrees::load_owner(&tree)
                                .ok()
                                .flatten()
                                .and_then(|owner| owner.attempt_id)
                        },
                    ),
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
        warn_lane_worktree_prepare(
            context,
            &ports,
            tree.repo().private_dir(),
            tree.path(),
            cancel,
        );
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
        if let Some(owner) = owner {
            let tree = provenant_confinement::verify_worktree(context.repo, &created.path)?;
            tree.repo()
                .private_dir()
                .atomic_write(
                    std::ffi::OsStr::new("provenant-provision-owner"),
                    ".provision-owner-",
                    owner.to_string().as_bytes(),
                    provenant_os::fs::PublishMode::NoReplace,
                )?
                .require_durable()
                .map_err(|_| WorktreeError::RegistrationUnconfirmed)?;
            tree.repo().revalidate_identity()?;
        }
        let tree = provenant_confinement::verify_worktree(context.repo, &created.path)?;
        warn_lane_worktree_prepare(
            context,
            &ports,
            tree.repo().private_dir(),
            tree.path(),
            cancel,
        );
        Ok(())
    })
}

fn warn_lane_worktree_prepare(
    context: &provenant_confinement::worktrees::WorktreeContext<'_>,
    ports: &provenant_config::PortTable,
    private_git: &provenant_os::fs::Directory,
    path: &std::path::Path,
    cancel: &CancelToken,
) {
    let env = provenant_run::build::prepare::lane_environment(context.home);
    if let Err(error) = provenant_run::build::prepare::run_if_changed(
        private_git,
        &ports.build.prepare,
        path,
        &env,
        WaitBudget::up_to(
            context.clock.mono_now(),
            provenant_core::DurationMicros::from_micros(30_000_000),
        ),
        cancel,
        context.clock,
    ) {
        provenant_fabric_runtime::custody::warning(format_args!(
            "lane worktree preparation warning (build.prepare): {error}; worktree provisioning continues"
        ));
    }
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
    provenant_fabric_runtime::relay::graph_authenticated(
        &provenant_providers::LaneRuntime::fabric_socket(attempt),
        serde_json::to_value(op)?,
        cancel,
        within,
        crate::run::nested_authority::server_is_current_ancestor,
    )
}

/// Which child settlements end a lane-side wait. This mirrors the `wait`
/// verb's `--all` / `--any` / `--fail-fast` selection without importing the
/// facade's private `WaitMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildWaitMode {
    All,
    Any,
    FailFast,
}

impl ChildWaitMode {
    /// Whether the children observed in one poll satisfy this wait's ending
    /// condition. A failed child is one that settled without `succeeded`.
    fn done(self, children: &[Value]) -> bool {
        let settled = |child: &Value| child["settled"] == true;
        let failed = |child: &Value| settled(child) && child["state"] != "succeeded";
        match self {
            Self::All => children.iter().all(settled),
            Self::Any => children.iter().any(settled),
            Self::FailFast => children.iter().any(failed) || children.iter().all(settled),
        }
    }
}

/// Wait for this lane's children, yielding its graph concurrency slot. Intake
/// calls this after adding children when a wait is requested. Returns the
/// child states at the point the mode's ending condition was met, including
/// failures, for the caller to report. `observe` sees each poll's children so
/// the caller can stream transitions under `--follow`.
pub fn wait_for_children(
    cancel: &CancelToken,
    within: std::time::Duration,
    interval: std::time::Duration,
    mode: ChildWaitMode,
    observe: &mut impl FnMut(&[Value]),
) -> Result<Value, ServiceError> {
    let clock = provenant_os::clock::SystemClock::new();
    let started = clock.mono_now();
    while_waiting(cancel, within, || {
        poll_children(
            cancel,
            PollTiming { within, interval },
            &clock,
            started,
            mode,
            observe,
            |remaining| from_lane(&RelayOp::Children, cancel, remaining),
        )
    })
}

/// The shared deadline and paced-reread interval for one child poll loop.
struct PollTiming {
    within: std::time::Duration,
    interval: std::time::Duration,
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
    timing: PollTiming,
    clock: &dyn Clock,
    started: provenant_core::MonoTime,
    mode: ChildWaitMode,
    observe: &mut impl FnMut(&[Value]),
    mut poll: impl FnMut(std::time::Duration) -> Result<Value, ServiceError>,
) -> Result<Value, ServiceError> {
    loop {
        if cancel.is_cancelled() {
            return Err(refused("cancelled", "child wait cancelled"));
        }
        let remaining = timing
            .within
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
        observe(children);
        if mode.done(children) {
            return Ok(value);
        }
        let elapsed = std::time::Duration::from_micros(
            clock
                .mono_now()
                .as_micros()
                .saturating_sub(started.as_micros()),
        );
        let remaining = timing.within.saturating_sub(elapsed);
        pause_child_poll(cancel, remaining.min(timing.interval));
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
    library.load_dir(Layer::Host, user)?;
    library.load_dir(Layer::Project, repository)?;
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
#[path = "graph_relay_tests.rs"]
mod graph_relay_tests;
