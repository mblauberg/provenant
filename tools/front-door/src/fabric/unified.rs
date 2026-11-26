//! Read and control graph targets through the public command surface.
use super::*;
use crate::stranded_work::{StrandedWork, human_lines, summary_lines};
use provenant_cli::target::{ResolveError, Resolved, Want};
use provenant_fabric_runtime::graph::{
    AttemptState, FileJournal, GraphJournal, GraphRun, JournalError, NodeState,
    PendingSteerQueueError,
};
use serde_json::{Value, json};

#[path = "unified_target_fault.rs"]
mod target_faults;
use target_faults::{
    TargetFault, command_target_fault, is_target_not_found, outside_project_fault, target_fault,
};

mod graph_wait;
use graph_wait::missing_graph_context;
pub(crate) use graph_wait::{
    check_targets, final_status_rows, read_lane_graph, read_status_rows, unsettled_graphs,
};

#[derive(Clone, Debug)]
pub(crate) struct GraphTarget {
    pub graph: String,
    pub node: Option<String>,
    /// `HOST/ATTEMPT` of the one selected attempt.
    pub attempt: Option<String>,
}

/// The graph names a miss can suggest: every checkpoint, event and reserved
/// handle in this project.
pub(crate) fn graph_names(
    root: &provenant_core::StateRoot,
    project: provenant_core::ProjectId,
) -> Vec<String> {
    FileJournal::reserved_graph_ids(root, project).unwrap_or_default()
}

/// Resolve `target` by reading only its own graph's checkpoint.
pub(crate) fn resolve(
    root: &provenant_core::StateRoot,
    project: provenant_core::ProjectId,
    target: &provenant_cli::Target,
    want: Want,
) -> Result<Resolved, ResolveError<JournalError>> {
    provenant_cli::target::resolve(target, want, &graph_names(root, project), |name| {
        FileJournal::read_status_checkpoint(root, project, name)
    })
}

/// The newest `NAME-k` graph when a changed-input launch superseded `NAME`.
/// Only the suffix family the naming rule defines (`k >= 2`) counts. The old
/// graph stays addressable; callers warn and name the newer handle as the next
/// step rather than following it silently.
pub(crate) fn superseding_handle(
    root: &provenant_core::StateRoot,
    project: provenant_core::ProjectId,
    base: &str,
) -> Option<String> {
    let ids = FileJournal::reserved_graph_ids(root, project).ok()?;
    let mut newest: Option<(u64, String)> = None;
    for id in ids {
        let Some(digits) = id
            .strip_prefix(base)
            .and_then(|rest| rest.strip_prefix('-'))
        else {
            continue;
        };
        if digits.is_empty()
            || digits.starts_with('0')
            || !digits.bytes().all(|byte| byte.is_ascii_digit())
        {
            continue;
        }
        let Ok(k) = digits.parse::<u64>() else {
            continue;
        };
        if k >= 2 && newest.as_ref().is_none_or(|(best, _)| k > *best) {
            newest = Some((k, id));
        }
    }
    newest.map(|(_, id)| id)
}

/// The warning a superseded name adds to a human view, plus the typed step
/// pointing at the newer handle. The target is still read as asked; only the
/// pointer moves.
pub(crate) fn superseded_notice(
    name: &str,
    newest: &str,
    verb: &str,
) -> provenant_records::command_result::NextStep {
    provenant_records::warning::warn(
        provenant_records::warning::Warning::new(
            provenant_records::warning::NAME_ALIAS_USED,
            format!("graph `{name}` was superseded by `{newest}` (different input); reading the older graph"),
        )
        .subject(name)
        .repair(format!("provenant {verb} {newest}")),
    );
    provenant_records::command_result::NextStep::from_args(
        &["provenant", verb, newest],
        "read the newer graph handle",
    )
}

/// The human label of a registered project: its checkout's directory name, or
/// its stable project id when the path has no name to show.
fn project_label(binding: &provenant_config::ProjectBinding) -> String {
    binding
        .common_dir()
        .parent()
        .and_then(std::path::Path::file_name)
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| binding.project_id().to_string())
}

/// The registered projects other than `selected` whose journal holds a graph
/// that `target` names. The caller's own project wins at resolve time, so only
/// a miss there reaches this search. Sorted by project id for a stable list.
fn cross_project_matches(
    roots: &provenant_core::Roots,
    selected: provenant_core::ProjectId,
    target: &provenant_cli::Target,
) -> Result<Vec<provenant_config::ProjectBinding>, Box<dyn std::error::Error>> {
    let mut bindings = provenant_config::registered_projects(roots)?
        .into_iter()
        .filter(|binding| binding.project_id() != selected)
        .collect::<Vec<_>>();
    bindings.sort_by_key(|binding| binding.project_id().to_string());
    let mut matches = Vec::new();
    for binding in bindings {
        let project = binding.project_id();
        let Ok(names) = FileJournal::reserved_graph_ids(&roots.state, project) else {
            continue;
        };
        let resolved = provenant_cli::target::resolve(target, Want::Work, &names, |name| {
            FileJournal::read_status_checkpoint(&roots.state, project, name)
        });
        match resolved {
            Ok(_) => matches.push(binding),
            // A whole unreadable graph stays addressable here, as it does in
            // the caller's project: status names its retirement command.
            Err(ResolveError::Read(error)) if target.node.is_none() && error.is_unreadable() => {
                matches.push(binding)
            }
            Err(ResolveError::NotFound { .. }) => {}
            Err(error) => return Err(target_fault(error).into()),
        }
    }
    Ok(matches)
}

/// One note for a graph found outside the caller's project. It names the owning
/// project in both the typed warning and the human view, so a reader never
/// mistakes the rows for the caller's own graph.
fn cross_project_notice(
    target: &str,
    binding: &provenant_config::ProjectBinding,
) -> provenant_records::warning::Warning {
    let id = binding.project_id();
    provenant_records::warning::Warning::new(
        provenant_records::warning::CROSS_PROJECT_TARGET_FOUND,
        format!(
            "{target} was found in project {} ({id}); showing that project's graph",
            project_label(binding)
        ),
    )
    .subject(target)
}

/// A refusal that lists every project holding the name, rather than guessing
/// one. The caller can pick one with `--project PATH`.
fn cross_project_ambiguity(
    target: &provenant_cli::Target,
    matches: &[provenant_config::ProjectBinding],
) -> TargetFault {
    let projects = matches
        .iter()
        .map(|binding| format!("{} ({})", project_label(binding), binding.project_id()))
        .collect::<Vec<_>>()
        .join(", ");
    TargetFault {
        outcome: provenant_records::Outcome::Invalid,
        code: "AMBIGUOUS_TARGET",
        message: format!(
            "{target} matches graphs in several projects: {projects}; pass --project PATH to select one"
        ),
    }
}

/// A read-only facade bound to one registered project. Status reads the matched
/// graph through its owner's identity; only the project binding changes.
fn project_factory(
    factory: &Factory,
    binding: &provenant_config::ProjectBinding,
) -> Result<Factory, Box<dyn std::error::Error>> {
    let context = provenant_cli::fabric::Context::open_for_project(
        factory.context.roots.clone(),
        &factory.home,
        binding,
    )?;
    let cwd = context.project_root.clone();
    Ok(Factory {
        project_attribution: factory.project_attribution,
        context,
        clock: SystemClock::new(),
        backoff: ThreadPause,
        executor: SystemExecutor::new(1)?,
        home: factory.home.clone(),
        cwd,
        assets: factory.assets.clone(),
        host_launch: None,
        workers: factory.workers,
    })
}

impl Factory {
    /// The graph work a `NAME[/NODE[#N]]` selector names, or `None` when it
    /// names nothing, including a numbered attempt that has not launched.
    pub(crate) fn graph_target(&self, selector: &str) -> Result<Option<GraphTarget>, FacadeError> {
        let target = provenant_cli::Target::parse(selector)
            .map_err(|_| FacadeError::invalid(InvalidInputDetail::Field("id".into())))?;
        match resolve(
            &self.context.roots.state,
            self.context.identity.project,
            &target,
            Want::Work,
        ) {
            Ok(resolved) if resolved.number.is_some() && resolved.attempt_ref.is_none() => Ok(None),
            Ok(resolved) => Ok(Some(GraphTarget {
                graph: resolved.graph,
                node: resolved.node,
                attempt: resolved.attempt_ref,
            })),
            Err(ResolveError::NotFound { .. }) => Ok(None),
            Err(ResolveError::Ambiguous { .. }) => Err(FacadeError::Conflict),
            // A whole unreadable graph stays addressable so status can name
            // its retirement command and stop can retire it.
            Err(ResolveError::Read(error)) if target.node.is_none() && error.is_unreadable() => {
                Ok(Some(GraphTarget {
                    graph: target.name,
                    node: None,
                    attempt: None,
                }))
            }
            Err(ResolveError::Read(error)) => Err(graph_journal_error(error)),
        }
    }

    pub(crate) fn stop_target(
        &self,
        target: &GraphTarget,
        reason: Option<String>,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<GraphRun, FacadeError> {
        let Some(parent) = &target.node else {
            return self.graph(
                GraphInput::Cancel {
                    extra: Default::default(),
                    id: target.graph.clone(),
                    reason,
                },
                budget,
                cancel,
            );
        };
        use provenant_fabric_runtime::graph::GraphJournal;
        let request_id = FileJournal::request_cancellation(
            &self.context.roots.state,
            self.context.identity.project,
            provenant_fabric_runtime::graph::CancellationTarget {
                graph_id: &target.graph,
                node: Some(parent),
                reason: reason.as_deref(),
            },
            budget,
            cancel,
            &self.clock,
        )
        .map_err(graph_journal_error)?;
        let mut journal = match FileJournal::try_open(
            &self.context.roots.state,
            self.context.identity.project,
            &target.graph,
        ) {
            Ok(journal) => journal,
            Err(error) if error.is_lock_timeout() => {
                let checkpoint = FileJournal::read_status_checkpoint(
                    &self.context.roots.state,
                    self.context.identity.project,
                    &target.graph,
                )
                .map_err(graph_journal_error)?
                .ok_or(FacadeError::NotFound)?;
                return Ok(super::graph_execution::cancellation_receipt(
                    graph_snapshot(checkpoint),
                    request_id,
                ));
            }
            Err(error) => return Err(graph_journal_error(error)),
        };
        let execution = FileJournal::execution(
            &self.context.roots.state,
            self.context.identity.project,
            &target.graph,
        )
        .map_err(graph_journal_error)?
        .ok_or(FacadeError::NotFound)?;
        let mut checkpoint = journal
            .load(&target.graph)
            .map_err(graph_journal_error)?
            .ok_or(FacadeError::NotFound)?;
        journal
            .apply_cancellation(&execution.document, &mut checkpoint)
            .map_err(graph_journal_error)?;
        let subtree = provenant_fabric_runtime::graph::cancel_node_subtree(
            &execution.document,
            &mut checkpoint,
            parent,
            reason.as_deref(),
        )
        .map_err(|_| FacadeError::field("node"))?;
        // Persist the subtree fence before stopping any external process.
        journal.save(&checkpoint).map_err(graph_journal_error)?;
        let mut owner = BinaryRunOwner {
            factory: self,
            allow_remote: true,
            actor: None,
            chair: None,
        };
        let mut first_error = None;
        for node in &checkpoint.nodes {
            if &node.id != parent && !subtree.running.contains(&node.id) {
                continue;
            }
            for attempt in node
                .attempts
                .iter()
                .filter(|a| a.state != AttemptState::Terminal || self.remote_graph_attempt(a))
            {
                if let Err(error) = self.cancel_graph_attempt(&mut owner, attempt, budget, cancel) {
                    first_error.get_or_insert(error);
                }
            }
        }
        let mut graph_owner = BoundGraphOwner {
            writer_launch_budget: None,
            admission_timeout: None,
            launch_warnings: Vec::new(),
            repo_facts: Default::default(),
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
        if let Err(_error) = provenant_fabric_runtime::graph::settle_stopped_subtrees(
            &execution.document,
            &mut checkpoint,
            &mut graph_owner,
            &mut journal,
        ) {
            first_error.get_or_insert(FacadeError::field("node"));
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(super::graph_execution::cancellation_receipt(
            graph_snapshot(checkpoint),
            request_id,
        ))
    }
}

fn row(
    id: String,
    kind: &str,
    target: &GraphTarget,
    node: Option<&str>,
    attempt: Option<&str>,
    state: Value,
    reason: Option<&str>,
) -> Value {
    let reason = reason.map(whole_text);
    json!({"id":id,"kind":kind,"graph_id":target.graph,"node_id":node,"attempt_id":attempt,"state":state,"reason":reason,"waiting_for":null,"cause_code":null,"outcome":null,"next":null,"latest_attempt":null})
}

pub(crate) fn status_rows(run: &GraphRun, target: &GraphTarget) -> Vec<Value> {
    let mut rows = Vec::new();
    if target.node.is_none() {
        rows.push(row(
            target.graph.clone(),
            "graph",
            target,
            None,
            None,
            json!(run.status),
            run.checkpoint
                .cancel_reason
                .as_deref()
                .or(run.checkpoint.pause_reason())
                .or(run.advance_error.as_deref()),
        ));
    }
    for node in &run.checkpoint.nodes {
        if target.node.as_ref().is_some_and(|id| id != &node.id) {
            continue;
        }
        if target.attempt.is_none() {
            let (state, reason, waiting_for) = if node.state == NodeState::Pending
                && let Some(wait_reason) = &node.wait_reason
            {
                (
                    json!("waiting"),
                    Some(wait_reason.as_str()),
                    json!(wait_reason),
                )
            } else {
                (
                    json!(node.state),
                    node.advance_error
                        .as_deref()
                        .or(node.skip_reason.as_deref()),
                    Value::Null,
                )
            };
            let mut node_row = row(
                format!("{}/{}", target.graph, node.id),
                "node",
                target,
                Some(&node.id),
                None,
                state,
                reason,
            );
            node_row["waiting_for"] = waiting_for;
            rows.push(node_row);
        }
        for attempt in node.recorded_attempts() {
            let number = attempt.key.attempt;
            let Some(reference) = &attempt.attempt_ref else {
                continue;
            };
            if target.attempt.as_ref().is_some_and(|id| id != reference) {
                continue;
            }
            let mut attempt_row = row(
                format!("{}/{}#{number}", target.graph, node.id),
                "attempt",
                target,
                Some(&node.id),
                Some(reference),
                json!(attempt.state),
                None,
            );
            attempt_row["latest_attempt"] = json!(
                node.recorded_attempts()
                    .next_back()
                    .is_some_and(|last| std::ptr::eq(last, attempt))
            );
            rows.push(attempt_row);
        }
    }
    for row in &mut rows {
        // Checkpoint facts are projected for every row, including nodes that
        // never launched an attempt (blocked, skipped and admission failures).
        for field in [
            "attempt_number",
            "warnings",
            "work_key",
            "branch",
            "head",
            "pr",
            "ci",
            "result_path",
            "provider_error",
            "handoff",
            "open_asks",
            "result_summary",
            "evidence_error",
            "review_verdict",
            "findings_count",
            "findings_available",
            "review_partial",
            "delivery_loop",
            "escalation",
            "delivery_park",
            "wait_source",
            // The result command reuses the same review projection wait and
            // status read, carrying the typed findings themselves, not a second
            // parse of the sealed result artifact.
            "verdict",
            "findings",
            "partial",
        ] {
            row[field] = Value::Null;
        }
        let node = row["node_id"]
            .as_str()
            .and_then(|id| run.checkpoint.nodes.iter().find(|node| node.id == id));
        let attempt = node.and_then(|node| {
            if row["kind"] == "node" {
                node.recorded_attempts().next_back()
            } else {
                node.recorded_attempts()
                    .find(|attempt| attempt.attempt_ref.as_deref() == row["attempt_id"].as_str())
            }
        });
        let latest = node.and_then(|node| node.recorded_attempts().next_back());
        row["attempt_number"] = json!(attempt.map_or(1, |attempt| attempt.key.attempt));
        let current =
            row["kind"] == "node" || attempt.zip(latest).is_some_and(|(a, b)| std::ptr::eq(a, b));
        if current {
            if row["kind"] == "node" {
                row["warnings"] = json!(node.map(|node| &node.warnings));
            }
            row["branch"] = json!(
                node.and_then(|node| node.outputs.as_ref())
                    .and_then(|out| out.branch.as_ref())
                    .or_else(|| node
                        .and_then(|node| node.land.as_ref())
                        .map(|land| &land.branch))
            );
            // A review result reports the subject it actually reviewed, not
            // the checkout head the launch started from. ReviewSettlement is
            // the only checkpoint evidence that pins that subject; other nodes
            // keep their existing commit, land and writer-binding fallback.
            row["head"] = json!(
                node.and_then(|node| node.review_settlement.as_ref())
                    .map(|settlement| settlement.head.to_string())
                    .or_else(|| node
                        .and_then(|node| node.outputs.as_ref())
                        .and_then(|out| out.commit.clone()))
                    .or_else(|| node
                        .and_then(|node| node.land.as_ref())
                        .map(|land| land.head.clone()))
                    .or_else(|| node
                        .and_then(|node| node.writer_binding.as_ref())
                        .map(|binding| binding.head.clone()))
                    .or_else(|| node
                        .and_then(|node| node.delivery_park.as_ref())
                        .and_then(|park| park.head.clone()))
            );
            row["pr"] = node
                .and_then(|node| node.land.as_ref())
                .and_then(|land| land.pull_request)
                .map_or_else(
                    || json!(run.checkpoint.pull_request),
                    |number| json!(number),
                );
            row["ci"] = json!(node.and_then(|node| node.ci.as_ref()));
            if row["kind"] == "node" {
                row["delivery_loop"] = json!(node.and_then(|node| node.delivery_loop.as_ref()));
                row["escalation"] = json!(node.and_then(|node| node.escalation.as_ref()));
                row["delivery_park"] = json!(node.and_then(|node| node.delivery_park.as_ref()));
                if node.and_then(|node| node.wait_reason.as_ref()).is_some() {
                    row["wait_source"] = json!("graph checkpoint");
                }
            }
            row["handoff"] = json!(Ok::<_, provenant_records::handoff::HandoffError>(
                node.and_then(|node| node.handoff.as_ref())
            ));
            row["result_summary"] = json!(
                node.and_then(|node| node.outputs.as_ref())
                    .and_then(|out| out.result.as_ref())
                    .map(|result| whole_text(&result.excerpt))
            );
            row["result_path"] = json!(
                node.and_then(|node| node.outputs.as_ref())
                    .and_then(|out| out.result.as_ref())
                    .and_then(|result| result.path.as_ref())
            );
            row["review_verdict"] = json!(
                node.and_then(|node| node.outputs.as_ref())
                    .and_then(|outputs| outputs.verdict.as_ref())
            );
            row["findings_count"] = json!(
                node.and_then(|node| node.outputs.as_ref())
                    .and_then(|outputs| outputs.findings_available.map(|_| outputs.findings.len()))
            );
            row["findings_available"] = json!(
                node.and_then(|node| node.outputs.as_ref())
                    .and_then(|outputs| outputs.findings_available)
            );
            row["review_partial"] = json!(
                node.and_then(|node| node.outputs.as_ref())
                    .map(|outputs| outputs.partial)
            );
            row["verdict"] = row["review_verdict"].clone();
            row["findings"] = json!(
                node.and_then(|node| node.outputs.as_ref())
                    .map(|outputs| &outputs.findings)
            );
            row["partial"] = row["review_partial"].clone();
            row["open_asks"] = json!(
                run.checkpoint
                    .asks
                    .iter()
                    .filter(
                        |ask| ask.node.is_none() || ask.node.as_deref() == row["node_id"].as_str()
                    )
                    .collect::<Vec<_>>()
            );
        }
        row["outcome"] = if row["kind"] == "graph" {
            json!(run.outcome)
        } else {
            json!(attempt.and_then(|attempt| attempt.outcome))
        };
        row["cause_code"] = json!(node.and_then(|node| {
            node.advance_owner_code
                .as_deref()
                .or(node.advance_error_code.as_deref())
        }));
        if row["cause_code"].is_null()
            && let Some(stop) = attempt.and_then(|attempt| attempt.stop.as_ref())
        {
            row["cause_code"] = json!(stop);
        }
        if row["cause_code"].is_null() && !row["outcome"].is_null() {
            row["cause_code"] = row["outcome"].clone();
        }
        if row["cause_code"].is_null() {
            row["cause_code"] = json!(node.and_then(|node| {
                if node.gate.as_ref().is_some_and(|gate| !gate.passed) {
                    Some("gate_failed")
                } else if node.state == provenant_fabric_runtime::graph::NodeState::Blocked {
                    Some("dependency_blocked")
                } else {
                    node.loop_pause_code.as_deref()
                }
            }));
        }
        if row["kind"] == "graph"
            && run.advance_error.as_deref().is_some_and(|reason| {
                reason.starts_with(&provenant_fabric_runtime::graph::unreadable_graph_guidance(
                    &target.graph,
                ))
            })
        {
            row["state"] = json!("unreadable");
            row["next"] = json!(format!("provenant stop {}", target.graph));
            continue;
        }
        let state = row["state"].as_str().unwrap_or("");
        let selection = node.map_or_else(
            || target.graph.clone(),
            |node| format!("{}/{}", target.graph, node.id),
        );
        let failed_outcome = row["outcome"]
            .as_str()
            .is_some_and(|outcome| outcome != "succeeded");
        row["next"] = json!(match state {
            // A launch refusal's persisted diagnostic already contains its
            // owner's action. In particular an uncertain handoff must not be
            // presented as permission to start a fresh attempt.
            "blocked" | "failed"
                if matches!(
                    node.and_then(|node| node.advance_error_code.as_deref()),
                    Some(
                        "launch_refused" | "launch_retry_exhausted" | "writer_binding_unavailable"
                    )
                ) =>
                row["reason"]
                    .as_str()
                    .unwrap_or("inspect the launch receipt before retrying")
                    .to_owned(),
            "terminal" | "complete" if failed_outcome => format!("provenant retry {selection}"),
            "failed" | "blocked" | "paused" => format!("provenant retry {selection}"),
            "pending" | "running" | "starting" | "unresolved" =>
                format!("provenant wait {selection}"),
            _ => format!("provenant output {selection}"),
        });
    }
    rows
}

/// A bounded reading for agents: the graph line, then one line per node with
/// its newest attempt as `NAME/NODE#N STATE MODEL`, failed work first, at most
/// `LISTING_ROWS` nodes with an omission count, then the next command.
pub(crate) fn render_graph_status(rows: &[Value]) -> String {
    use provenant_cli::limits::LISTING_ROWS;
    let priority = |row: &&Value| match row["state"].as_str() {
        Some("failed" | "blocked" | "paused" | "unresolved" | "unreadable") => 0,
        Some("complete" | "succeeded" | "terminal")
            if row["outcome"]
                .as_str()
                .is_some_and(|outcome| outcome != "succeeded") =>
        {
            1
        }
        Some("pending" | "running" | "starting") => 2,
        _ => 3,
    };
    let graphs: Vec<_> = rows.iter().filter(|row| row["kind"] == "graph").collect();
    let nodes: Vec<_> = rows.iter().filter(|row| row["kind"] == "node").collect();
    // An attempt target selects no node rows: its attempt is the node line.
    let mut lines: Vec<_> = if nodes.is_empty() {
        rows.iter()
            .filter(|row| row["kind"] == "attempt")
            .map(|row| (row, Some(row)))
            .collect()
    } else {
        nodes
            .iter()
            .map(|node| {
                let newest = rows.iter().rfind(|row| {
                    row["kind"] == "attempt"
                        && row["graph_id"] == node["graph_id"]
                        && row["node_id"] == node["node_id"]
                });
                (*node, newest)
            })
            .collect()
    };
    lines.sort_by_key(|(row, _)| priority(row));
    let mut human = String::new();
    for graph in &graphs {
        human.push_str(&status_line(&plain(&graph["id"]), graph, None));
    }
    let digests = super::graph_status::node_digests(rows);
    for (node, attempt) in lines.iter().take(LISTING_ROWS) {
        if let Some(digest) = digests
            .iter()
            .find(|digest| json!(digest.key) == node["work_key"])
        {
            human.push_str(&digest.line);
            human.push('\n');
            append_status_warnings(&mut human, node);
            // The digest line replaces the plain status line, but the newest
            // attempt's liveness still belongs in the full human view.
            if let Some(attempt) = attempt {
                human.push_str(&liveness_detail(attempt));
            }
            continue;
        }
        let name = attempt.map_or_else(|| plain(&node["id"]), |attempt| plain(&attempt["id"]));
        let model = attempt.map_or_else(|| "-".to_owned(), row_model);
        human.push_str(&status_line(&name, node, Some(&model)));
        // Liveness detail belongs only to the full human view. The compact
        // view renders the summary instead of this text, and JSON carries the
        // same fields on the attempt row itself.
        if let Some(attempt) = attempt {
            human.push_str(&liveness_detail(attempt));
        }
    }
    if lines.len() > LISTING_ROWS {
        let mut omitted = std::collections::BTreeMap::<String, usize>::new();
        for (row, _) in lines.iter().skip(LISTING_ROWS) {
            *omitted.entry(plain(&row["state"])).or_default() += 1;
        }
        for (state, count) in omitted {
            human.push_str(&format!(
                "{count} {state} nodes not shown; --json lists every row\n"
            ));
        }
    }
    human
}

/// Why the status read suggests its one command, from the row the owner
/// already selected. A copied command is only useful when the reader knows the
/// state that made it the next step.
fn status_step_why(row: &Value) -> &'static str {
    match row["state"].as_str() {
        Some("failed" | "blocked" | "unresolved" | "unreadable") => "the graph failed",
        Some("paused") => "the graph needs input",
        Some("cancelled") => "the graph was cancelled",
        Some("complete" | "succeeded" | "terminal")
            if row["outcome"]
                .as_str()
                .is_some_and(|outcome| outcome != "succeeded") =>
        {
            "the graph did not succeed"
        }
        Some("complete" | "succeeded" | "terminal") => "the graph is complete",
        Some("pending" | "running" | "starting") => "the graph is still running",
        _ => "the graph needs attention",
    }
}

/// The one suggested command a graph status read offers, as a typed step. The
/// row projection already owns the command text; this only turns it into the
/// value the JSON envelope carries and the edge renders once.
pub(crate) fn graph_status_next(
    rows: &[Value],
) -> Option<provenant_records::command_result::NextStep> {
    let priority = |row: &&Value| match row["state"].as_str() {
        Some("failed" | "blocked" | "paused" | "unresolved" | "unreadable") => 0,
        Some("complete" | "succeeded" | "terminal")
            if row["outcome"]
                .as_str()
                .is_some_and(|outcome| outcome != "succeeded") =>
        {
            1
        }
        Some("pending" | "running" | "starting") => 2,
        _ => 3,
    };
    let graphs: Vec<_> = rows.iter().filter(|row| row["kind"] == "graph").collect();
    let nodes: Vec<_> = rows.iter().filter(|row| row["kind"] == "node").collect();
    let lines: Vec<_> = if nodes.is_empty() {
        rows.iter().filter(|row| row["kind"] == "attempt").collect()
    } else {
        nodes
    };
    let chosen = graphs
        .iter()
        .copied()
        .chain(lines.iter().copied())
        .min_by_key(|row| priority(row));
    let command = chosen.map(|row| plain(&row["next"])).unwrap_or_default();
    let why = chosen.map(status_step_why).unwrap_or_default();
    (!command.is_empty())
        .then(|| provenant_records::command_result::NextStep::from_command(&command, why))
}

fn plain(value: &Value) -> String {
    provenant_fabric_runtime::hosts::plain_text(value.as_str().unwrap_or(""), 1024)
}

/// Peer text sanitised but not abbreviated. Row builders keep whole facts;
/// only the compact renderer imposes `SUMMARY_ROW_BYTES`.
fn whole_text(text: &str) -> String {
    provenant_fabric_runtime::hosts::plain_text(text, usize::MAX)
}

/// Whether an attempt row is settled. Lane projections carry `finished_at` for
/// every terminal attempt; graph rows carry a terminal state. Everything else
/// — pending, running, starting, settling and the liveness-overridden
/// unattended/unknown states — is still worth a liveness line: a dead writer is
/// exactly the case where the last-event time matters most.
fn settled_attempt(attempt: &Value) -> bool {
    if !attempt["finished_at"].is_null() {
        return true;
    }
    matches!(
        attempt["state"].as_str(),
        Some(
            "succeeded"
                | "complete"
                | "terminal"
                | "failed"
                | "blocked"
                | "skipped"
                | "unreadable"
                | "unresolved"
        )
    )
}

/// One bounded detail line for an unsettled attempt in `status --full`: its
/// newest observed event, host-local attempt directory, masked current child
/// command and known wait reason. Absent observations are simply omitted, never
/// guessed.
fn liveness_detail(attempt: &Value) -> String {
    if settled_attempt(attempt) {
        return String::new();
    }
    let mut parts = Vec::new();
    if attempt["last_event_at"].is_string() {
        let time = plain(&attempt["last_event_at"]);
        let source = plain(&attempt["last_event_source"]);
        if source.is_empty() {
            parts.push(format!("last event {time}"));
        } else {
            parts.push(format!("last event {time} ({source})"));
        }
    }
    if let Ok(path) = serde_json::from_value::<provenant_records::StructuredPathWire>(
        attempt["attempt_directory"].clone(),
    )
    .and_then(|path| {
        path.to_path_buf()
            .map_err(|_| serde_json::Error::io(std::io::Error::other("host-only path")))
    }) {
        parts.push(format!(
            "attempt {}",
            plain(&json!(path.display().to_string()))
        ));
    }
    if attempt["child_command"].is_string() {
        parts.push(format!("command {}", plain(&attempt["child_command"])));
    }
    if attempt["waiting_for"].is_string() {
        parts.push(format!("wait {}", plain(&attempt["waiting_for"])));
    }
    if parts.is_empty() {
        return String::new();
    }
    let line = format!("  live: {}", parts.join("; "));
    format!(
        "{}\n",
        provenant_cli::bounded_text(&line, provenant_cli::limits::SUMMARY_ROW_BYTES * 2)
    )
}

/// The model an attempt row ran on, as `PROVIDER/MODEL`, or `-` when unknown.
fn row_model(row: &Value) -> String {
    ["observed_route", "applied_route"]
        .iter()
        .find_map(|field| {
            let route = &row[*field];
            let model = route["model"].as_str()?;
            Some(match route["provider"].as_str() {
                Some(provider) if !model.starts_with(&format!("{provider}/")) => {
                    format!("{provider}/{model}")
                }
                _ => model.to_owned(),
            })
        })
        .unwrap_or_else(|| "-".into())
}

/// One status line, `NAME STATE [MODEL]`, naming a stop or failure cause and
/// its note when there is one. The whole line is returned; the compact renderer
/// bounds it with `SUMMARY_ROW_BYTES`, while `--full` prints it whole.
pub(crate) fn status_line(name: &str, row: &Value, model: Option<&str>) -> String {
    let mut line = format!("{name} {}", plain(&row["state"]));
    if let Some(model) = model
        && (model != "-" || row["state"] != "waiting")
    {
        line.push(' ');
        line.push_str(model);
    }
    if !row["cause_code"].is_null() {
        line.push_str(&format!(" cause={}", plain(&row["cause_code"])));
    }
    let note = if row["reason"].is_null() {
        &row["waiting_for"]
    } else {
        &row["reason"]
    };
    if !note.is_null() {
        line.push_str(&format!(": {}", whole_text(note.as_str().unwrap_or(""))));
    }
    if let Some(work) = row.get("stop_work").filter(|value| !value.is_null()) {
        let head = work["head"].as_str().unwrap_or("unknown");
        let ahead = work["ahead"].as_u64();
        let push = work["push"] == "pushed";
        if let Some(ahead) = ahead.filter(|count| *count > 0) {
            line.push_str(&format!(
                " | {ahead} commits {} at {head}",
                if push { "pushed" } else { "unpushed" }
            ));
        } else if push && ahead.is_none() {
            line.push_str(&format!(" | committed work pushed at {head}"));
        }
        if let Some(dirty) = work["dirty_paths"].as_u64().filter(|count| *count > 0) {
            line.push_str(&format!(" | {dirty} dirty paths left uncommitted"));
        }
    }
    line.push('\n');
    append_status_warnings(&mut line, row);
    line
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum StateRank {
    Failed = 0,
    Blocked = 1,
    Attention = 2,
    Live = 3,
    Done = 4,
    Other = 5,
}

impl StateRank {
    pub(crate) fn of(state: &str) -> Self {
        match state {
            "failed" | "unreadable" | "unresolved" => Self::Failed,
            "blocked" => Self::Blocked,
            "paused" | "input_required" | "refused" | "attention" => Self::Attention,
            "running" | "starting" | "pending" | "launched" => Self::Live,
            "succeeded" | "complete" => Self::Done,
            _ => Self::Other,
        }
    }

    pub(crate) fn needs_attention(self) -> bool {
        matches!(self, Self::Failed | Self::Blocked | Self::Attention)
    }

    pub(crate) fn is_live(self) -> bool {
        matches!(self, Self::Live)
    }
}

fn ranked_tally<'a, I: Iterator<Item = &'a str>>(states: I) -> String {
    let mut counts_map = std::collections::BTreeMap::<&'a str, usize>::new();
    for state in states {
        *counts_map.entry(state).or_default() += 1;
    }
    let mut counts_vec: Vec<(&'a str, usize)> = counts_map.into_iter().collect();
    counts_vec.sort_by_key(|(s, _)| StateRank::of(s));
    provenant_cli::Summary::tally(&counts_vec)
}

pub(crate) fn status_summary(rows: &[Value]) -> provenant_cli::Summary {
    let nodes: Vec<_> = rows.iter().filter(|row| row["kind"] == "node").collect();
    let lines: Vec<(&Value, Option<&Value>)> = if nodes.is_empty() {
        rows.iter()
            .filter(|row| row["kind"] == "attempt")
            .map(|row| (row, Some(row)))
            .collect()
    } else {
        nodes
            .iter()
            .map(|node| {
                let newest = rows.iter().rfind(|row| {
                    row["kind"] == "attempt"
                        && row["graph_id"] == node["graph_id"]
                        && row["node_id"] == node["node_id"]
                });
                (*node, newest)
            })
            .collect()
    };

    let tally = ranked_tally(lines.iter().map(|(node, attempt)| {
        super::graph_status::lane_state(
            node["state"].as_str().unwrap_or("unknown"),
            attempt.and_then(|attempt| attempt["state"].as_str()),
            attempt
                .and_then(|attempt| attempt["presence"].as_str())
                .or_else(|| node["presence"].as_str()),
        )
    }));
    let headline = if tally.is_empty() {
        "no rows".to_string()
    } else {
        tally
    };

    let is_failed_attention_blocked = |node: &Value, attempt: Option<&Value>| -> bool {
        let state = node["state"].as_str().unwrap_or("");
        if StateRank::of(state).needs_attention() {
            return true;
        }
        if matches!(state, "complete" | "succeeded" | "terminal")
            && node["outcome"].as_str().is_some_and(|o| o != "succeeded")
        {
            return true;
        }
        if let Some(att) = attempt {
            let att_state = att["state"].as_str().unwrap_or("");
            if matches!(att_state, "failed" | "terminal")
                && att["outcome"]
                    .as_str()
                    .is_some_and(|o| o != "succeeded" && o != "ok")
            {
                return true;
            }
        }
        node["open_asks"].as_array().is_some_and(|a| !a.is_empty())
    };

    let is_live = |node: &Value, attempt: Option<&Value>| -> bool {
        let state = super::graph_status::lane_state(
            node["state"].as_str().unwrap_or(""),
            attempt.and_then(|attempt| attempt["state"].as_str()),
            attempt
                .and_then(|attempt| attempt["presence"].as_str())
                .or_else(|| node["presence"].as_str()),
        );
        StateRank::of(state).is_live()
    };

    let digests = super::graph_status::node_digests(rows);
    let format_row = |node: &Value, attempt: Option<&Value>| -> String {
        if let Some(digest) = digests.iter().find(|d| json!(d.key) == node["work_key"]) {
            digest.line.trim_end().to_string()
        } else {
            let name = attempt.map_or_else(|| plain(&node["id"]), |att| plain(&att["id"]));
            let model = attempt.map_or_else(|| "-".to_owned(), row_model);
            status_line(&name, node, Some(&model))
                .trim_end()
                .to_string()
        }
    };

    let mut summary_rows = Vec::new();
    let mut shown_indices = std::collections::BTreeSet::new();

    // 1) Every failed, attention or blocked row in full
    for (i, (node, attempt)) in lines.iter().enumerate() {
        if is_failed_attention_blocked(node, *attempt) {
            summary_rows.push(format_row(node, *attempt));
            shown_indices.insert(i);
        }
    }

    // 2) Then live rows up to the cap
    let live_cap = provenant_cli::limits::SUMMARY_ROWS;
    let live_slots = live_cap.saturating_sub(summary_rows.len());
    if live_slots > 0 {
        let mut added = 0;
        for (i, (node, attempt)) in lines.iter().enumerate() {
            if !shown_indices.contains(&i) && is_live(node, *attempt) {
                summary_rows.push(format_row(node, *attempt));
                shown_indices.insert(i);
                added += 1;
                if added >= live_slots {
                    break;
                }
            }
        }
    }

    let omitted = lines.len().saturating_sub(shown_indices.len());

    provenant_cli::Summary::bounded(headline, summary_rows, omitted)
}

fn append_status_warnings(line: &mut String, row: &Value) {
    if let Some(warnings) = row["warnings"].as_array() {
        for warning in warnings.iter().filter_map(Value::as_str) {
            line.push_str(&format!(
                "  warning: {}\n",
                provenant_cli::bounded_text(warning, 2048)
            ));
        }
    }
}

fn mark_unreadable_attempt(row: &mut Value, attempt: &str, error: &provenant_records::DecodeError) {
    row["state"] = json!("unreadable");
    row["reason"] = json!(format!(
        "run store record for attempt {attempt}: decode error: {error}"
    ));
}

fn status_keys(rows: &mut [Value], project: provenant_core::ProjectId) -> Result<(), FacadeError> {
    for row in rows.iter_mut() {
        if let (Some(graph), Some(node), Some(number)) = (
            row["graph_id"].as_str(),
            row["node_id"].as_str(),
            row["attempt_number"].as_u64(),
        ) {
            row["work_key"] = json!(
                provenant_records::WorkKey::new(
                    project,
                    graph,
                    node,
                    u32::try_from(number).map_err(|_| FacadeError::StateCorrupt)?
                )
                .map_err(|_| FacadeError::StateCorrupt)?
            );
        }
    }
    Ok(())
}

/// Host-wide facts shared by every evidence pass of one `status` command.
/// Reopening the store and rescanning admission, worktrees and the process
/// census once per graph made a fleet-wide `status --full` take minutes.
struct StatusFacts<'a> {
    execution: provenant_run::ExecutionContext,
    call: provenant_run::Request<'a>,
    store: Option<provenant_run::RunStore>,
    lane_holders: Vec<provenant_os::admission::HeldInfo>,
    worktrees:
        std::collections::BTreeMap<provenant_core::AttemptId, provenant_cli::run::LaneWorktree>,
    // A census is expensive and only useful when a local attempt is present;
    // the first local row asks for it and later rows share the one reading.
    local_census: std::cell::OnceCell<Result<provenant_supervisor::census::Census, ()>>,
}

impl<'a> StatusFacts<'a> {
    fn gather(
        context: &Context,
        clock: &'a SystemClock,
        cancel: &'a CancelToken,
    ) -> Result<Self, FacadeError> {
        let execution = provenant_run::ExecutionContext {
            state_root: context.roots.state.clone(),
            project_id: context.identity.project,
            actor_id: context.identity.inbox.actor,
            read_prompts: false,
        };
        let call = provenant_run::Request { clock, cancel };
        let store =
            provenant_run::RunStore::open_read_only(&execution, &call).map_err(run_error)?;
        Ok(Self {
            execution,
            call,
            store,
            lane_holders: provenant_os::admission::all_holders(context.roots.state.as_path()),
            worktrees: provenant_cli::run::worktree_index(context.project_root.as_path()),
            local_census: std::cell::OnceCell::new(),
        })
    }

    fn census(&self) -> Result<&provenant_supervisor::census::Census, ()> {
        cached_census(&self.local_census, || {
            provenant_supervisor::census::ProcessObserver::census(
                &provenant_supervisor::census::SystemProcessObserver,
                provenant_supervisor::census::MAX_CENSUS_ENTRIES,
            )
            .map_err(|_| ())
        })
        .as_ref()
        .map_err(|_| ())
    }
}

fn cached_census<T>(
    cell: &std::cell::OnceCell<Result<T, ()>>,
    read: impl FnOnce() -> Result<T, ()>,
) -> &Result<T, ()> {
    cell.get_or_init(read)
}

/// Enrich graph attempt rows from identity-checked host records using facts
/// gathered once for the whole listing. The run owner retains typed terminal
/// causes.
fn enrich_status_rows_with_facts(
    rows: &mut [Value],
    context: &Context,
    clock: &SystemClock,
    facts: &StatusFacts<'_>,
    peer_budget: WaitBudget,
) -> Result<(), FacadeError> {
    status_keys(rows, context.identity.project)?;
    let mut peer_executor = None;
    for row in rows.iter_mut() {
        if row["kind"] != "attempt" {
            continue;
        }
        let Some((host, attempt)) = row["attempt_id"].as_str().and_then(|id| id.split_once('/'))
        else {
            continue;
        };
        let host = host.parse().map_err(|_| FacadeError::StateCorrupt)?;
        let attempt = attempt.parse().map_err(|_| FacadeError::StateCorrupt)?;
        let (record, waits, relayed_presence) = if host == context.identity.host {
            let Some(store) = &facts.store else {
                continue;
            };
            match store.record(attempt, &facts.call) {
                Ok(record) => {
                    let waits = store.lane_waits(std::slice::from_ref(&record), &facts.call);
                    (record, waits, None)
                }
                // A collected run leaves the retained graph's own row readable.
                Err(provenant_run::RunError::NotFound) => continue,
                Err(provenant_run::RunError::Decode(error)) => {
                    mark_unreadable_attempt(row, &attempt.to_string(), &error);
                    continue;
                }
                Err(error) => return Err(run_error(error)),
            }
        } else {
            let executor = match &peer_executor {
                Some(executor) => executor,
                None => peer_executor.insert(
                    SystemExecutor::new(1)
                        .map_err(|_| FacadeError::io("create peer status executor"))?,
                ),
            };
            let response = context.peer_with(
                executor,
                clock,
                host,
                PeerAction::Run {
                    extra: Default::default(),
                    input: RunInput::Status {
                        extra: Default::default(),
                        target: AttemptTarget {
                            extra: Default::default(),
                            host: Some(host),
                            attempt,
                        },
                    },
                    peer_subtree_after: None,
                },
                peer_budget,
                facts.call.cancel,
            );
            let response = match response {
                Ok(response) => response,
                Err(failure) => {
                    row["evidence_error"] = json!(provenant_fabric_runtime::hosts::plain_text(
                        &failure.error.to_string(),
                        200
                    ));
                    project_remote_attempt_unknown(row);
                    continue;
                }
            };
            match response {
                PeerResult::Run { data, .. } => match *data {
                    RunResult::Status {
                        attempt: record,
                        presence,
                        lane_waits,
                        ..
                    } if record.attempt_id == attempt => (record, lane_waits, presence),
                    _ => return Err(FacadeError::StateCorrupt),
                },
                _ => return Err(FacadeError::StateCorrupt),
            }
        };
        // A peer owns a separate graph rooted at the submitted attempt. Its
        // authenticated status retains the origin binding beside that pointer.
        row["preparation_failure"] = json!(record.preparation_failure);
        row["settlement_reason"] = json!(record.settlement_reason);
        let target: provenant_cli::Target = row["id"]
            .as_str()
            .ok_or(FacadeError::StateCorrupt)?
            .parse()
            .map_err(|_| FacadeError::StateCorrupt)?;
        let expected = provenant_records::WorkKey::new(
            context.identity.project,
            target.name,
            target.node.ok_or(FacadeError::StateCorrupt)?,
            target.attempt.ok_or(FacadeError::StateCorrupt)?.get(),
        )
        .map_err(|_| FacadeError::StateCorrupt)?;
        let peer_graph = record.peer.peer_graph.as_ref();
        let origin_matches = host != context.identity.host
            && peer_graph.is_some_and(|graph| graph.host == host && !graph.graph_id.is_empty())
            && super::peer::peer_origin_matches(&record.peer, &expected);
        if (host != context.identity.host && !origin_matches)
            || (host == context.identity.host && record.key != expected)
        {
            return Err(FacadeError::StateCorrupt);
        }
        if retain_collected_peer_row(row, &record, host, context.identity.host) {
            continue;
        }
        if let Some(graph) = &record.peer.peer_graph {
            row["peer_graph"] = json!(graph);
        }
        if let Some(subtree) = &record.peer.peer_subtree {
            row["peer_subtree"] = json!(subtree);
        }
        let mut projected = if let Some(presence) = relayed_presence {
            // The peer observed its own writer with its own census. Use that
            // result directly: a fresh output event alone is not a heartbeat
            // and a reader here cannot observe the peer's process table.
            provenant_cli::run::project_lane_with_relayed_presence(
                &record,
                &waits,
                context.roots.state.as_path(),
                clock.wall_now().ok(),
                presence,
            )
        } else {
            let census = if host == context.identity.host {
                facts.census()
            } else {
                Err(())
            };
            provenant_cli::run::project_lane_with_admission(
                &record,
                &waits,
                context.roots.state.as_path(),
                clock.wall_now().ok(),
                host == context.identity.host && row["latest_attempt"] == true,
                census,
                &facts.lane_holders,
            )
        };
        if host == context.identity.host
            && let Some(store) = &facts.store
        {
            let evidence =
                provenant_cli::run::status_evidence(store, &facts.execution, &record, &facts.call);
            projected.provider_error = evidence["provider_error"].as_str().map(str::to_owned);
            projected.handoff =
                (!evidence["handoff"].is_null()).then(|| evidence["handoff"].clone());
        }
        let fields = serde_json::to_value(projected).map_err(|_| FacadeError::StateCorrupt)?;
        let reference = row["attempt_id"].clone();
        let graph_state = row["state"].clone();
        let attempt_number = row["attempt_number"].clone();
        let result_path = row["result_path"].clone();
        let retained_head = row["head"].clone();
        row.as_object_mut()
            .ok_or(FacadeError::StateCorrupt)?
            .extend(fields.as_object().ok_or(FacadeError::StateCorrupt)?.clone());
        if let Some(worktree) = facts.worktrees.get(&record.attempt_id) {
            row["worktree"] = serde_json::to_value(worktree).unwrap_or(Value::Null);
        }
        if row["head"].is_null() {
            row["head"] = retained_head;
        }
        row["attempt_id"] = reference;
        row["attempt_number"] = attempt_number;
        if row["result_path"].is_null() {
            row["result_path"] = result_path;
        }
        row["graph_state"] = graph_state;
        project_attempt_failure(row);
    }
    project_node_waits(rows);
    project_node_liveness(rows);
    project_node_failures(rows);
    for row in rows.iter_mut() {
        if let Some(object) = row.as_object_mut() {
            object.remove("latest_attempt");
        }
    }
    Ok(())
}

/// Enrich one graph's rows with freshly gathered facts. A listing with many
/// graphs gathers the facts once and calls `enrich_status_rows_with_facts` for
/// each graph instead of paying for these host-wide scans every time.
pub(crate) fn enrich_status_rows_within(
    rows: &mut [Value],
    context: &Context,
    clock: &SystemClock,
    cancel: &CancelToken,
    peer_budget: WaitBudget,
) -> Result<(), FacadeError> {
    let facts = StatusFacts::gather(context, clock, cancel)?;
    enrich_status_rows_with_facts(rows, context, clock, &facts, peer_budget)
}

/// Keep graph cause codes scalar even when a closed terminal cause has data.
/// Decode the declaration before selecting its tag: arbitrary object keys are
/// not safe diagnostics.
fn terminal_cause_code(value: &Value) -> Option<String> {
    let _: provenant_records::run::TerminalCause = serde_json::from_value(value.clone()).ok()?;
    match value {
        Value::String(code) => Some(code.clone()),
        Value::Object(tagged) => tagged.keys().next().cloned(),
        _ => None,
    }
}

fn project_attempt_failure(row: &mut Value) {
    if let Some(code) = terminal_cause_code(&row["terminal_cause"]) {
        row["cause_code"] = json!(code);
        if code == "handoff_refused" {
            if let Some(id) = row["id"].as_str() {
                row["next"] = json!(format!("provenant retry {id}"));
            }
        } else if row["reason"].is_string() {
            row["next"] = row["reason"].clone();
        }
    }
}

/// A peer error is no evidence that a checkpoint's Running writer is still
/// present. Keep its durable state and expose the unavailable observation on
/// the attempt so the current node cannot render as Running without proof.
fn project_remote_attempt_unknown(row: &mut Value) {
    if row["kind"] != "attempt" || row["state"] != "running" {
        return;
    }
    row["graph_state"] = row["state"].clone();
    row["recorded_state"] = row["state"].clone();
    row["state"] = json!("unknown");
    row["presence"] = json!("unknown");
    row["reason"] = json!(
        "remote writer presence is unavailable; retry status on the owning host when it is reachable"
    );
}

/// Surface the latest attempt's typed cause on its failed node as well. The
/// checkpoint owns launch failures; an accepted attempt owns its terminal cause.
fn project_node_failures(rows: &mut [Value]) {
    let failures: std::collections::BTreeMap<_, _> = rows
        .iter()
        .filter(|row| row["kind"] == "attempt" && row["latest_attempt"] == true)
        .filter_map(|row| {
            Some((
                (
                    row["graph_id"].as_str()?.to_owned(),
                    row["node_id"].as_str()?.to_owned(),
                ),
                (
                    row["graph_state"].clone(),
                    terminal_cause_code(&row["terminal_cause"]).map_or(Value::Null, Value::String),
                    row["reason"].clone(),
                    row["next"].clone(),
                ),
            ))
        })
        .collect();
    for row in rows
        .iter_mut()
        .filter(|row| row["kind"] == "node" && row["state"] == "failed" && row["reason"].is_null())
    {
        let key = (
            row["graph_id"].as_str().unwrap_or_default().to_owned(),
            row["node_id"].as_str().unwrap_or_default().to_owned(),
        );
        if let Some((state, cause, reason, next)) = failures.get(&key)
            && state == "terminal"
        {
            if !cause.is_null() {
                row["cause_code"] = cause.clone();
            }
            if !reason.is_null() {
                row["reason"] = reason.clone();
                row["next"] = if next.is_null() {
                    reason.clone()
                } else {
                    next.clone()
                };
            }
        }
    }
}

/// Collection on a peer must not invalidate the origin's retained checkpoint.
pub(super) fn retain_collected_peer_row(
    row: &mut Value,
    record: &provenant_records::run::AttemptRecord,
    peer: provenant_core::HostId,
    local: provenant_core::HostId,
) -> bool {
    let Some(graph) = record.peer.peer_graph.as_ref() else {
        return false;
    };
    if peer == local
        || record.peer.peer_graph_collected != Some(true)
        || !matches!(
            record.state,
            provenant_records::run::AttemptState::Terminal { .. }
        )
        || graph.host != peer
        || !graph.graph_id.starts_with("peer-")
        || record
            .context
            .as_ref()
            .and_then(|context| context.graph_id.as_deref())
            != Some(graph.graph_id.as_str())
    {
        return false;
    }
    row["peer_graph"] = json!(graph);
    row["peer_graph_collected"] = json!(true);
    true
}

/// Keep a live parent's wait visible before bounded status output reaches its
/// attempt history. The host's attempt record distinguishes child waits from
/// admission reclaims; the graph checkpoint only records the yielded slot.
fn project_node_waits(rows: &mut [Value]) {
    let waits: std::collections::BTreeMap<_, _> = rows
        .iter()
        .filter(|row| {
            row["kind"] == "attempt"
                && row["latest_attempt"] == true
                && row["state"] == "running"
                && row["graph_state"] == "running"
                && row["waiting_for"].is_string()
        })
        .filter_map(|row| {
            Some((
                (
                    row["graph_id"].as_str()?.to_owned(),
                    row["node_id"].as_str()?.to_owned(),
                ),
                row["waiting_for"].clone(),
            ))
        })
        .collect();
    for row in rows
        .iter_mut()
        .filter(|row| row["kind"] == "node" && row["state"] == "running")
    {
        let key = (
            row["graph_id"].as_str().unwrap_or_default().to_owned(),
            row["node_id"].as_str().unwrap_or_default().to_owned(),
        );
        if let Some(wait) = waits.get(&key) {
            row["waiting_for"] = wait.clone();
        }
    }
}

/// A checkpoint records the last durable node state. Project native liveness
/// onto its current node without changing the checkpoint or confusing an
/// unobservable writer with a live one.
fn project_node_liveness(rows: &mut [Value]) {
    let observations: std::collections::BTreeMap<_, _> = rows
        .iter()
        .filter(|row| {
            row["kind"] == "attempt"
                && row["latest_attempt"] == true
                && row["recorded_state"] == "running"
                && row["presence"].is_string()
                && row["presence"] != "live"
        })
        .filter_map(|row| {
            Some((
                (
                    row["graph_id"].as_str()?.to_owned(),
                    row["node_id"].as_str()?.to_owned(),
                ),
                (
                    row["state"].clone(),
                    row["presence"].clone(),
                    row["reason"].clone(),
                ),
            ))
        })
        .collect();
    for row in rows
        .iter_mut()
        .filter(|row| row["kind"] == "node" && row["state"] == "running")
    {
        let key = (
            row["graph_id"].as_str().unwrap_or_default().to_owned(),
            row["node_id"].as_str().unwrap_or_default().to_owned(),
        );
        let Some((state, presence, reason)) = observations.get(&key) else {
            continue;
        };
        row["recorded_state"] = json!("running");
        row["state"] = state.clone();
        row["presence"] = presence.clone();
        if reason.is_string() {
            row["reason"] = reason.clone();
            row["next"] = reason.clone();
        }
    }
}

/// Resolve `target` and keep the one checkpoint read it took.
fn resolve_with_checkpoint(
    factory: &Factory,
    target: &provenant_cli::Target,
    want: Want,
) -> Result<(Resolved, provenant_fabric_runtime::graph::GraphCheckpoint), TargetFault> {
    resolve_checkpoint(
        &factory.context.roots.state,
        factory.context.identity.project,
        target,
        want,
    )
}

/// Resolve graph output from one status snapshot so declared root topology and
/// checkpoint expansion ownership cannot come from different graph revisions.
fn resolve_output_with_checkpoint(
    factory: &Factory,
    target: &provenant_cli::Target,
) -> Result<(Resolved, provenant_fabric_runtime::graph::GraphCheckpoint), TargetFault> {
    let mut read = None;
    let names = graph_names(
        &factory.context.roots.state,
        factory.context.identity.project,
    );
    let resolved =
        provenant_cli::target::resolve_with_document(target, Want::Output, &names, |name| {
            let snapshot = FileJournal::read_status_snapshot(
                &factory.context.roots.state,
                factory.context.identity.project,
                name,
            )?;
            read = snapshot.as_ref().map(|(_, checkpoint)| checkpoint.clone());
            Ok(snapshot.map(|(execution, checkpoint)| (execution.document, checkpoint)))
        })
        .map_err(target_fault)?;
    let checkpoint = read.ok_or_else(|| {
        target_fault(ResolveError::NotFound {
            target: target.to_string(),
            candidates: Vec::new(),
        })
    })?;
    Ok((resolved, checkpoint))
}

fn resolve_checkpoint(
    root: &provenant_core::StateRoot,
    project: provenant_core::ProjectId,
    target: &provenant_cli::Target,
    want: Want,
) -> Result<(Resolved, provenant_fabric_runtime::graph::GraphCheckpoint), TargetFault> {
    let mut read = None;
    let names = graph_names(root, project);
    let resolved = provenant_cli::target::resolve(target, want, &names, |name| {
        let checkpoint = FileJournal::read_status_checkpoint(root, project, name)?;
        read.clone_from(&checkpoint);
        Ok(checkpoint)
    })
    .map_err(target_fault)?;
    let checkpoint = read.ok_or_else(|| {
        target_fault(ResolveError::NotFound {
            target: target.to_string(),
            candidates: Vec::new(),
        })
    })?;
    Ok((resolved, checkpoint))
}

pub(crate) fn command_fail(
    name: &str,
    error: &(dyn std::error::Error + 'static),
    globals: &provenant_cli::Globals,
) -> u8 {
    match command_target_fault(error, None) {
        Some((outcome, code, message)) => crate::fail(
            name,
            outcome,
            code,
            &message,
            "use a valid, unambiguous target in the owning project and rerun the command",
            globals,
        ),
        None => crate::fail(
            name,
            provenant_records::Outcome::Error,
            "graph_target",
            &error.to_string(),
            "check the reported graph read error and rerun the command",
            globals,
        ),
    }
}

pub(crate) enum GraphCommand<'a> {
    Status,
    Stop {
        reason: Option<&'a str>,
        queue_control: Option<provenant_cli::QueueControlAction>,
        queue_revision: Option<u64>,
        operation_id: Option<&'a provenant_core::OperationId>,
    },
    Retry {
        extend: &'a [String],
        brief: Option<&'a str>,
        brief_file: Option<&'a std::path::Path>,
        queue_control: Option<provenant_cli::QueueControlAction>,
        queue_revision: Option<u64>,
        operation_id: Option<&'a provenant_core::OperationId>,
    },
}

fn validate_retry_brief_target(
    target: &provenant_cli::Target,
    rows: &[Value],
) -> Result<(), &'static str> {
    let target_id = target.to_string();
    if !rows
        .iter()
        .any(|row| row["id"].as_str() == Some(target_id.as_str()))
    {
        return Err("retry brief target did not resolve to a current graph row");
    }
    if target.node.is_none() && rows.iter().filter(|row| row["kind"] == "node").count() != 1 {
        return Err("retry --brief requires a target with exactly one node");
    }
    Ok(())
}

/// Read one graph target through the host relay. Inside a lane the host
/// resolves the target (cli.md "Targets"), so the read needs neither host
/// configuration nor state, which a confined lane cannot open.
fn lane_graph_read(input: Value) -> Result<Value, FacadeError> {
    provenant_cli::fabric::graph_relay::from_lane(
        &provenant_fabric_runtime::graph::relay::RelayOp::Read { input },
        &CancelToken::new(),
        std::time::Duration::from_secs(55),
    )
    .map_err(super::interface::relay_error)
}

/// What a graph command reports before it is emitted: data, human text, the
/// owner's next steps and a superseded-name step that replaces them.
type GraphCommandReply = (
    Value,
    String,
    Vec<provenant_records::command_result::NextStep>,
    Option<provenant_records::command_result::NextStep>,
);

/// Read or control graph targets. Every target resolves before any is read
/// or changed, so one unknown target changes nothing. Inside a lane the host
/// resolves the target, and an empty list reads the lane's own graph.
pub(crate) fn unified_graph_command(
    targets: &[provenant_cli::Target],
    command: GraphCommand<'_>,
    globals: &provenant_cli::Globals,
    workers: &'static dyn WorkerObserver,
) -> u8 {
    let name = match command {
        GraphCommand::Status => "status",
        GraphCommand::Stop { .. } => "stop",
        GraphCommand::Retry { .. } => "retry",
    };
    let result = (|| -> Result<GraphCommandReply, Box<dyn std::error::Error>> {
        // A lane reads through the host, which holds the configuration.
        let lane_read = matches!(command, GraphCommand::Status) && provenant_cli::fabric::in_lane();
        let factory = if lane_read {
            None
        } else {
            Some(
                match command {
                    GraphCommand::Status => read_factory(globals, workers),
                    _ => factory(globals, workers),
                }
                .map_err(|error| match targets.first() {
                    Some(target) if missing_graph_context(error.as_ref()) => {
                        Box::new(target_fault(ResolveError::NotFound {
                            target: target.to_string(),
                            candidates: Vec::new(),
                        }))
                    }
                    _ => error,
                })?,
            )
        };
        let brief_source = match &command {
            GraphCommand::Retry {
                brief, brief_file, ..
            } => provenant_cli::brief::select_brief_source([
                brief.map(|text| provenant_cli::brief::BriefSource::Text(text.to_owned())),
                brief_file.map(|path| provenant_cli::brief::BriefSource::file(path.to_path_buf())),
            ])?,
            _ => None,
        };
        // One owning project per target when `status NAME` misses in the
        // caller's project. An empty target list reads the caller's own graph.
        let mut cross_project = vec![None; targets.len().max(1)];
        if let Some(factory) = factory.as_ref().filter(|factory| !factory.context.in_lane) {
            for (index, target) in targets.iter().enumerate() {
                match resolve(
                    &factory.context.roots.state,
                    factory.context.identity.project,
                    target,
                    Want::Work,
                ) {
                    // A whole unreadable graph still reports and stops: the
                    // graph path names its retirement command.
                    Err(ResolveError::Read(error))
                        if target.node.is_none() && error.is_unreadable() => {}
                    Err(not_found @ ResolveError::NotFound { .. })
                        if matches!(command, GraphCommand::Status) =>
                    {
                        let matches = cross_project_matches(
                            &factory.context.roots,
                            factory.context.identity.project,
                            target,
                        )?;
                        match matches.len() {
                            0 => return Err(target_fault(not_found).into()),
                            1 => cross_project[index] = matches.into_iter().next(),
                            _ => return Err(cross_project_ambiguity(target, &matches).into()),
                        }
                    }
                    result => {
                        result.map_err(target_fault)?;
                    }
                }
            }
        }
        if brief_source.is_some() && matches!(&command, GraphCommand::Retry { .. }) {
            let Some(factory) = &factory else {
                return Err(std::io::Error::other("retry brief needs graph state").into());
            };
            if targets.is_empty() {
                return Err(std::io::Error::other("retry brief needs a target").into());
            }
            for target in targets {
                let input = json!({"op":"status","id":target.to_string()});
                let budget = WaitBudget::cli(
                    factory.clock.mono_now(),
                    factory.context.config.graph.launch_wait_seconds,
                )?;
                let data = factory.graph_document(input, budget, &CancelToken::new())?;
                let rows = data["rows"].as_array().ok_or(FacadeError::StateCorrupt)?;
                validate_retry_brief_target(target, rows).map_err(std::io::Error::other)?;
            }
        }
        let resolved_brief = brief_source
            .map(|source| {
                provenant_cli::brief::read_brief(source, provenant_cli::brief::MAX_BRIEF_BYTES)
            })
            .transpose()?;
        let queue_control = match &command {
            GraphCommand::Stop {
                queue_control,
                queue_revision,
                operation_id,
                ..
            }
            | GraphCommand::Retry {
                queue_control,
                queue_revision,
                operation_id,
                ..
            } => queue_control
                .as_ref()
                .map(|action| (*action, *queue_revision, *operation_id)),
            GraphCommand::Status => None,
        };
        if let Some((action, revision, operation_id)) = queue_control {
            if targets.len() != 1 || revision.is_none() || operation_id.is_none() {
                return Err(std::io::Error::other(
                    "a queue control requires one target, --queue-revision and --operation-id",
                )
                .into());
            }
            let action_matches = matches!(
                (&command, action),
                (
                    GraphCommand::Stop { .. },
                    provenant_cli::QueueControlAction::Drop
                ) | (
                    GraphCommand::Retry { .. },
                    provenant_cli::QueueControlAction::Retry
                ) | (
                    GraphCommand::Retry { .. },
                    provenant_cli::QueueControlAction::Fix
                )
            );
            if !action_matches {
                return Err(std::io::Error::other(
                    "stop accepts queue control drop; retry accepts retry or fix",
                )
                .into());
            }
            if action == provenant_cli::QueueControlAction::Fix
                && resolved_brief.as_deref().is_none_or(str::is_empty)
            {
                return Err(
                    std::io::Error::other("queue fix requires --brief or --brief-file").into(),
                );
            }
        }
        let selectors: Vec<(String, Option<&str>)> = if targets.is_empty() {
            vec![(String::new(), None)]
        } else {
            targets
                .iter()
                .map(|target| (target.to_string(), Some(target.name.as_str())))
                .collect()
        };
        let cancel = CancelToken::new();
        let mut documents = Vec::new();
        let mut human = String::new();
        let mut next_steps: Vec<provenant_records::command_result::NextStep> = Vec::new();
        let mut superseded_next: Option<provenant_records::command_result::NextStep> = None;
        for (index, (selector, graph)) in selectors.into_iter().enumerate() {
            let owner = cross_project.get(index).and_then(Option::as_ref);
            let mut input = json!({"op":name,"id":selector});
            match &command {
                GraphCommand::Status => {}
                GraphCommand::Stop { reason, .. } => {
                    if let Some(reason) = reason {
                        input["reason"] = json!(reason);
                    }
                }
                GraphCommand::Retry { extend, .. } => {
                    input["extend"] = json!(extend);
                    if let Some(brief) = &resolved_brief {
                        input["brief"] = json!(brief);
                    }
                }
            }
            if let Some((action, revision, operation_id)) = queue_control {
                let action = match action {
                    provenant_cli::QueueControlAction::Accept => "accept",
                    provenant_cli::QueueControlAction::Fix => "fix",
                    provenant_cli::QueueControlAction::Retry => "retry",
                    provenant_cli::QueueControlAction::Drop => "drop",
                };
                input["queue_control"] = json!({
                    "action": action,
                    "expected_revision": revision,
                    "operation_id": operation_id,
                });
            }
            let Some(factory) = &factory else {
                let mut data = lane_graph_read(input)?;
                let rows = data["rows"].as_array().ok_or(FacadeError::StateCorrupt)?;
                human.push_str(&render_graph_status(rows));
                next_steps.extend(graph_status_next(rows));
                data["digests"] = json!(super::graph_status::node_digests(rows));
                documents.push(data);
                continue;
            };
            let project_reader = owner
                .map(|binding| project_factory(factory, binding))
                .transpose()?;
            let reader = project_reader.as_ref().unwrap_or(factory);
            let budget = WaitBudget::cli(
                reader.clock.mono_now(),
                reader.context.config.graph.launch_wait_seconds,
            )?;
            let mut data = reader.graph_document(input, budget, &cancel)?;
            if let Some(binding) = owner {
                let notice = cross_project_notice(&selector, binding);
                human.push_str(&format!("note: {}\n", notice.message));
                provenant_records::warning::warn(notice);
            }
            if let (Some(rows), Some(graph)) = (data["rows"].as_array_mut(), graph)
                && !reader.context.in_lane
            {
                localise_worktree_actions(reader, graph, rows);
            }
            let rows = data["rows"].as_array().ok_or(FacadeError::StateCorrupt)?;
            human.push_str(&render_graph_status(rows));
            next_steps.extend(graph_status_next(rows));
            if globals.full && matches!(command, GraphCommand::Status) {
                for row in rows.iter().filter(|row| row["kind"] == "attempt") {
                    if let Some(attempt) = row["attempt_id"].as_str().and_then(|id| id.parse().ok())
                    {
                        human.push_str(&provenant_cli::run::status_full_summary(row, attempt));
                    }
                }
            }
            if matches!(command, GraphCommand::Status)
                && let Some(name) = graph
                && let Some(newest) = superseding_handle(
                    &reader.context.roots.state,
                    reader.context.identity.project,
                    name,
                )
            {
                superseded_next.get_or_insert(superseded_notice(name, &newest, "status"));
            }
            data["digests"] = json!(super::graph_status::node_digests(rows));
            if matches!(command, GraphCommand::Stop { .. })
                && data["cancellation_requested"]["pending_acknowledgement"] == true
            {
                let request_id = data["cancellation_requested"]["request_id"]
                    .as_str()
                    .unwrap_or("unknown");
                human.push_str(&format!(
                    "cancellation requested; acknowledgement pending ({request_id})\n"
                ));
            }
            documents.push(data);
        }
        let data = match <[Value; 1]>::try_from(documents) {
            Ok([only]) => only,
            Err(documents) => json!({"targets":documents}),
        };
        Ok((data, human, next_steps, superseded_next))
    })();
    match result {
        Ok((data, human, next_steps, superseded_next)) => {
            let summary = if matches!(command, GraphCommand::Status) {
                let rows: Vec<Value> =
                    if let Some(targets) = data.get("targets").and_then(Value::as_array) {
                        targets
                            .iter()
                            .filter_map(|t| t.get("rows").and_then(Value::as_array))
                            .flatten()
                            .cloned()
                            .collect()
                    } else {
                        data.get("rows")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default()
                    };
                Some(status_summary(&rows))
            } else {
                None
            };
            // A superseded name points at the newer handle, not at the older
            // graph's own next step.
            let steps = superseded_next.map_or(next_steps, |step| vec![step]);
            crate::emit_summarised(
                &provenant_records::CommandResult::new(name, provenant_records::Outcome::Ok, data)
                    .with_next(steps),
                globals,
                &human,
                false,
                summary.as_ref().filter(|_| !globals.full),
            )
        }
        Err(error) => command_fail(name, error.as_ref(), globals),
    }
}

/// Outcomes of one `steer` command. Settled targets are handled by the sourced
/// `lanes.steer_settled` setting and emit success with these two variants.
enum SteerResult {
    Queued(Value),
    Pending(Value),
    /// `--explain`: the effective delivery mode and where each value came from.
    Explained {
        data: Value,
        human: String,
    },
    Refused(&'static str),
    NoTurn,
    /// A `--now` target has no retained session, so interrupting would strand
    /// the accepted text; the attempt stays running.
    SessionUnavailable(String),
    /// The target's graph will run no further attempt; no future turn can read
    /// the steering text, so it must be refused with a way to start a round.
    CompleteRefused {
        message: String,
        repair: String,
    },
    SettledQueued(Value),
    SettledWarned(Value),
    Unsupported(provenant_records::control::ControlReceipt),
}

/// Whether a steer can still be read by a future attempt. A node-scoped steer
/// needs a follow-up for that node, not merely a live sibling: a settled node
/// with no continuation of its own refuses even while another node runs. A
/// graph-scoped steer keeps the whole-graph "unfinished" test used by graph
/// status, including a delivery the graph still owes.
fn graph_steer_has_follow_up(
    checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
    resolved: &Resolved,
) -> bool {
    if checkpoint.cancelled {
        return false;
    }
    if checkpoint
        .nodes
        .iter()
        .filter(|node| {
            resolved
                .node
                .as_deref()
                .is_none_or(|id| node.id.as_str() == id)
        })
        .any(|node| {
            node.expansion_pause.is_some()
                || matches!(node.state, NodeState::Pending | NodeState::Running)
        })
    {
        return true;
    }
    resolved.node.is_none()
        && checkpoint.delivery_required
        && !checkpoint.delivered
        && checkpoint.successful()
}

/// What a steer aimed at an already settled lane does (`lanes.steer_settled`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SteerSettledMode {
    Queue,
    Warn,
}

/// The effective `lanes.steer_settled` value and the source that set it. An
/// unreadable host config is advisory: warn, use the shipped `queue` default
/// and keep the note for the follow-up rather than dropping the instruction.
fn steer_settled_setting(
    factory: &super::Factory,
) -> (SteerSettledMode, provenant_records::provenance::Source) {
    use provenant_records::provenance::{Layer, Source};

    let default = (
        SteerSettledMode::Queue,
        Source {
            layer: Layer::Default,
            origin: None,
        },
    );
    let config = match provenant_cli::config_reads::read_for(&factory.context.roots, "run") {
        Ok(config) => config,
        Err(error) => {
            let safe = provenant_records::diagnostic::safe_text(&error.to_string());
            provenant_records::warning::warn(provenant_records::warning::Warning::new(
                provenant_records::warning::CONFIG_UNAVAILABLE,
                format!("steer settled setting unavailable ({safe}); using default queue"),
            ));
            return default;
        }
    };
    let Some(knob) = config.knobs.get("lanes.steer_settled") else {
        return default;
    };
    let mode = match knob.value.as_str() {
        Some("warn") => SteerSettledMode::Warn,
        Some("queue") => SteerSettledMode::Queue,
        _ => {
            provenant_records::warning::warn(
                provenant_records::warning::Warning::new(
                    provenant_records::warning::CONFIG_MALFORMED,
                    "unrecognised lanes.steer_settled value; using default queue",
                )
                .source(knob.source.clone()),
            );
            SteerSettledMode::Queue
        }
    };
    (mode, knob.source.clone())
}

/// A settled steer target resolved: keep the note for its next follow-up or
/// warn and do nothing, by sourced setting. All work (reading the bounded note
/// and, in `queue`, writing the scoped shared note) happens inside the closure
/// so the process sink sees the warning before `emit` collects it.
fn settled_steer_result(
    factory: &super::Factory,
    args: &provenant_cli::inbox::Steer,
    resolved: &Resolved,
    checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
    already_normalised: Option<String>,
) -> Result<SteerResult, Box<dyn std::error::Error>> {
    use provenant_fabric_runtime::notes::{Host, Input, Scope};

    let target = args.target.to_string();
    let follow_up = match &resolved.node {
        Some(node) => format!("{}/{}", resolved.graph, node),
        None => resolved.graph.clone(),
    };
    // A settled target can keep a note only when another attempt for that same
    // target will run to read it. A live sibling does not read a node's note,
    // nor does a completed graph: the note would be written but nothing starts.
    // Refuse with a typed cause and a command that opens a new round.
    if !graph_steer_has_follow_up(checkpoint, resolved) {
        return Ok(SteerResult::CompleteRefused {
            message: format!(
                "{target} has no further attempt; steering would be accepted but never read"
            ),
            repair: format!(
                "start a new round on the same worktree with `provenant run implement --worktree WORKTREE --brief-file FILE` (or `provenant retry {follow_up} --brief-file FILE`)"
            ),
        });
    }
    let (mode, source) = steer_settled_setting(factory);
    match mode {
        SteerSettledMode::Warn => {
            provenant_records::warning::warn(
                provenant_records::warning::Warning::new(
                    provenant_records::warning::STEER_TARGET_SETTLED,
                    format!("{target} has settled; steering was ignored"),
                )
                .subject(target.clone())
                .source(source)
                .repair(format!(
                    "set lanes.steer_settled=queue or run `provenant retry {follow_up} --brief-file FILE`"
                )),
            );
            Ok(SteerResult::SettledWarned(serde_json::json!({
                "queued": false,
                "target": target,
            })))
        }
        SteerSettledMode::Queue => {
            let text = match already_normalised {
                Some(text) => text,
                None => {
                    let brief = provenant_cli::brief::select_brief_source([
                        args.brief
                            .clone()
                            .map(provenant_cli::brief::BriefSource::Text),
                        args.brief_file
                            .clone()
                            .map(provenant_cli::brief::BriefSource::file),
                    ])?
                    .ok_or_else(|| std::io::Error::other("steer requires a brief source"))?;
                    let raw = provenant_cli::brief::read_brief(
                        brief,
                        provenant_records::control::CONTROL_TEXT_BYTES,
                    )?;
                    provenant_fabric_runtime::graph::normalise_prompt_text(&raw)
                        .map_err(std::io::Error::other)?
                }
            };
            let scope = Scope::Subtree {
                graph: resolved.graph.clone(),
                node: resolved
                    .node
                    .clone()
                    .or_else(|| {
                        resolved
                            .work_key(factory.context.identity.project)
                            .map(|key| key.node().to_owned())
                    })
                    .unwrap_or_else(|| "root".to_owned()),
            };
            let host = Host::chair(factory.context.identity.project, None);
            let cancel = provenant_core::CancelToken::new();
            provenant_cli::notes::execute_host(
                &factory.context,
                &host,
                Input::Add {
                    text,
                    tags: vec!["steer".into(), "follow-up".into()],
                    scope,
                    resolves: None,
                },
                &factory.clock,
                &cancel,
            )?;
            Ok(SteerResult::SettledQueued(serde_json::json!({
                "queued": true,
                "target": target,
                "follow_up": follow_up,
            })))
        }
    }
}

/// Effective delivery for one steer. The shipped default queues; `--now` is a
/// per-call override, and `domains.fabric.steer.delivery` sets the host default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SteerMode {
    Queued,
    Now,
}

impl SteerMode {
    fn name(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Now => "now",
        }
    }
    fn from_config(value: provenant_fabric_runtime::config::SteerDelivery) -> Self {
        use provenant_fabric_runtime::config::SteerDelivery as D;
        match value {
            D::Queued => Self::Queued,
            D::Now => Self::Now,
        }
    }
}

/// The host's `domains.fabric.steer.delivery` setting and where it came from.
/// An unreadable host config is advisory: warn, use the shipped `queued`
/// default and keep steering rather than dropping the instruction.
fn steer_delivery_setting(factory: &super::Factory) -> (SteerMode, String) {
    const FIELD: &str = "domains.fabric.steer.delivery";
    let default = || {
        (
            SteerMode::Queued,
            provenant_cli::config_reads::setting_source(&Default::default(), FIELD),
        )
    };
    let config = match provenant_cli::config_reads::read_for(&factory.context.roots, "steer") {
        Ok(config) => config,
        Err(error) => {
            let safe = provenant_records::diagnostic::safe_text(&error.to_string());
            provenant_records::warning::warn(provenant_records::warning::Warning::new(
                provenant_records::warning::CONFIG_UNAVAILABLE,
                format!("steer delivery setting unavailable ({safe}); using default queued"),
            ));
            return default();
        }
    };
    (
        SteerMode::from_config(config.domains.fabric.steer.delivery),
        provenant_cli::config_reads::setting_source(&config.origins, FIELD),
    )
}

/// Queue steering for one attempt. Steering a settled lane is handled by
/// `settled_steer_result`; steering text is bounded at 4 KiB.
pub(crate) fn unified_graph_steer(
    args: &provenant_cli::inbox::Steer,
    globals: &provenant_cli::Globals,
    workers: &'static dyn WorkerObserver,
) -> u8 {
    let result = (|| -> Result<SteerResult, Box<dyn std::error::Error>> {
        let factory = factory(globals, workers)?;
        let (host_mode, source) = steer_delivery_setting(&factory);
        let mode = if args.now { SteerMode::Now } else { host_mode };
        if args.explain {
            let mode_source = if args.now {
                "command line (--now)".to_owned()
            } else {
                source
            };
            return Ok(SteerResult::Explained {
                data: json!({
                    "delivery": mode.name(),
                    "source": mode_source,
                    "host_default": host_mode.name(),
                }),
                human: format!(
                    "steer delivery: {} (source: {})\n",
                    mode.name(),
                    mode_source
                ),
            });
        }
        let now = mode == SteerMode::Now;
        let (resolved, checkpoint) =
            resolve_with_checkpoint(&factory, &args.target, Want::Attempt { settled: false })?;
        let project = factory.context.identity.project;
        let work_key = resolved
            .work_key(project)
            .ok_or(FacadeError::StateCorrupt)?;
        let operation_id = graph_steer_operation_id(args.operation_id.as_ref(), &work_key)?;
        let selected_settled = super::steer::selected_is_settled(&resolved, &checkpoint);
        if let Some(action) = args.queue_control {
            if action != provenant_cli::QueueControlAction::Fix {
                return Err(std::io::Error::other("steer accepts queue control fix only").into());
            }
            let queue_revision = args
                .queue_revision
                .ok_or_else(|| std::io::Error::other("queue fix requires --queue-revision"))?;
            let queue_operation_id = args
                .operation_id
                .clone()
                .ok_or_else(|| std::io::Error::other("queue fix requires --operation-id"))?;
            let source = provenant_cli::brief::select_brief_source([
                args.brief
                    .clone()
                    .map(provenant_cli::brief::BriefSource::Text),
                args.brief_file
                    .clone()
                    .map(provenant_cli::brief::BriefSource::file),
            ])?
            .ok_or_else(|| std::io::Error::other("steer requires a brief source"))?;
            let text = provenant_cli::brief::read_brief(
                source,
                provenant_records::control::CONTROL_TEXT_BYTES,
            )?;
            let text = provenant_fabric_runtime::graph::normalise_prompt_text(&text)
                .map_err(std::io::Error::other)?;
            provenant_fabric_runtime::graph::patch::scan_for_secrets(&text)
                .map_err(std::io::Error::other)?;
            let cancel = CancelToken::new();
            let budget = WaitBudget::cli(
                factory.clock.mono_now(),
                factory.context.config.graph.launch_wait_seconds,
            )?;
            let metadata = super::queue_controls::Metadata {
                action: super::queue_controls::Action::Fix,
                expected_revision: queue_revision,
                operation_id: queue_operation_id.clone(),
                review_node: None,
                review_head: None,
                graph_id: Some(resolved.graph.clone()),
            };
            let proof = (!selected_settled).then(|| {
                super::steer::verified_target(&factory, &resolved, &checkpoint, budget, &cancel)
            });
            if selected_settled
                || matches!(proof.as_ref(), Some(super::steer::TargetProof::Settled))
            {
                let result = factory.graph_document(
                    json!({
                        "op": "retry",
                        "id": args.target.to_string(),
                        "brief": text,
                        "queue_control": {
                            "action": "fix",
                            "expected_revision": queue_revision,
                            "operation_id": queue_operation_id,
                            "graph_id": resolved.graph,
                        }
                    }),
                    budget,
                    &cancel,
                )?;
                return Ok(SteerResult::Queued(
                    json!({"queue_control": result["queue_control"], "result": result}),
                ));
            }
            if matches!(proof.as_ref(), Some(super::steer::TargetProof::Conflict)) {
                return Ok(SteerResult::Refused(
                    "queue fix requires one verified current live attempt",
                ));
            }
            let node_id = resolved.node.as_deref().ok_or(FacadeError::StateCorrupt)?;
            let receipt = super::queue_controls::queue_live_fix(
                &factory,
                &resolved.graph,
                node_id,
                &metadata,
                (&text, &work_key),
                budget,
                &cancel,
            )?;
            return Ok(SteerResult::Pending(json!({
                "queued": true,
                "queue_control": receipt,
                "warning": "the graph tick will revalidate and deliver the accepted queue fix"
            })));
        }
        if selected_settled {
            return settled_steer_result(&factory, args, &resolved, &checkpoint, None);
        }
        let source = provenant_cli::brief::select_brief_source([
            args.brief
                .clone()
                .map(provenant_cli::brief::BriefSource::Text),
            args.brief_file
                .clone()
                .map(provenant_cli::brief::BriefSource::file),
        ])?
        .ok_or_else(|| std::io::Error::other("steer requires a brief source"))?;
        let text = provenant_cli::brief::read_brief(
            source,
            provenant_records::control::CONTROL_TEXT_BYTES,
        )?;
        let text = provenant_fabric_runtime::graph::normalise_prompt_text(&text)
            .map_err(std::io::Error::other)?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(
            factory.clock.mono_now(),
            factory.context.config.graph.launch_wait_seconds,
        )?;
        // Queue state and fast delivery share graph exclusion, so operation
        // replay cannot bypass a durable refusal or a still-pending intent.
        let mut journal = FileJournal::open(
            &factory.context.roots.state,
            project,
            &resolved.graph,
            budget,
            &cancel,
            &factory.clock,
        )?;
        let Some(mut current) = journal.load(&resolved.graph)? else {
            return Ok(SteerResult::Refused(
                "the graph target is no longer available",
            ));
        };
        if let Some(saved) = current.pending_steers.get(operation_id.as_str()) {
            if saved.text != text
                || saved.work_key != work_key
                || saved.requester_host != factory.context.identity.inbox.host
                || saved.requester_actor != factory.context.identity.inbox.actor
                || saved.requester_agent != factory.context.identity.inbox.agent
            {
                return Ok(SteerResult::Refused(
                    "steer operation ID was reused with different input",
                ));
            }
            if saved.refusal.is_some() {
                return Ok(SteerResult::Refused(
                    "the queued steer was refused; its accepted text is retained",
                ));
            }
            return Ok(SteerResult::Pending(
                json!({"queued":true,"warning":"owner verification is pending; the original queued operation will retry"}),
            ));
        }
        let send =
            |factory: &Factory,
             target: &super::steer::VerifiedTarget|
             -> Result<
                provenant_fabric_runtime::inbox::InboxResult,
                provenant_fabric_runtime::relay::ServiceError,
            > {
                let (host, id) = target
                    .reference
                    .split_once('/')
                    .ok_or(FacadeError::StateCorrupt)?;
                let graph_context = (target.target.host != Some(factory.context.identity.host))
                    .then(|| provenant_fabric_runtime::inbox::GraphSteerContext {
                        launch_actor: target.launch_actor,
                        launch_operation: target.launch_operation.clone(),
                        origin_work_key: target.work_key.clone(),
                    });
                let input = provenant_fabric_runtime::inbox::InboxInput::Steer {
                    extra: Default::default(),
                    graph_context,
                    attempt: id.parse().map_err(|_| FacadeError::StateCorrupt)?,
                    text: text.clone(),
                    operation_id: Some(operation_id.clone()),
                    now,
                };
                BoundFacade { factory }.inbox_owner(input, Some(host.into()), budget, &cancel)
            };
        match super::steer::verified_target(&factory, &resolved, &current, budget, &cancel) {
            super::steer::TargetProof::Settled => {
                return settled_steer_result(
                    &factory,
                    args,
                    &resolved,
                    &current,
                    Some(text.clone()),
                );
            }
            super::steer::TargetProof::Conflict => {
                return Ok(SteerResult::Refused(
                    "steering requires one verified, accepted, current live attempt",
                ));
            }
            super::steer::TargetProof::Verified(target) => match send(&factory, &target) {
                Ok(provenant_fabric_runtime::inbox::InboxResult::Control { receipt })
                    if receipt.state == provenant_records::control::ControlState::Unsupported =>
                {
                    return Ok(SteerResult::Unsupported(receipt));
                }
                Ok(provenant_fabric_runtime::inbox::InboxResult::Control { receipt }) => {
                    return Ok(SteerResult::Queued(json!({"receipts":[receipt]})));
                }
                Ok(_) => return Err(FacadeError::StateCorrupt.into()),
                Err(error) if super::steer::steer_delivery_unavailable(&error) => {}
                Err(error) if super::steer::steer_session_unavailable(&error) => {
                    return Ok(SteerResult::SessionUnavailable(error.to_string()));
                }
                Err(error) if error.code() == "steer_no_turn" => return Ok(SteerResult::NoTurn),
                Err(provenant_fabric_runtime::relay::ServiceError::InboxOwner { .. }) => {
                    return Ok(SteerResult::Refused(
                        "the verified inbox owner refused steering",
                    ));
                }
                Err(error) => return Err(Box::new(error)),
            },
            super::steer::TargetProof::Unavailable => {}
        }
        let context = provenant_fabric_runtime::inbox::GraphSteerContext {
            launch_actor: current
                .nodes
                .iter()
                .flat_map(|node| node.recorded_attempts())
                .find(|attempt| attempt.key.work_key(project).ok().as_ref() == Some(&work_key))
                .and_then(|attempt| attempt.launch_actor.parse().ok())
                .ok_or(FacadeError::StateCorrupt)?,
            launch_operation: super::graph_execution::graph_operation(
                "graph-attempt-operation",
                &current
                    .nodes
                    .iter()
                    .flat_map(|node| node.recorded_attempts())
                    .find(|attempt| attempt.key.work_key(project).ok().as_ref() == Some(&work_key))
                    .ok_or(FacadeError::StateCorrupt)?
                    .key,
            )?,
            origin_work_key: work_key.clone(),
        };
        let pending = provenant_fabric_runtime::graph::PendingGraphSteer {
            operation_id: operation_id.clone(),
            caller_operation_id: args.operation_id.clone(),
            work_key: work_key.clone(),
            graph_context: context,
            requester_host: factory.context.identity.inbox.host,
            requester_agent: factory.context.identity.inbox.agent.clone(),
            requester_actor: factory.context.identity.inbox.actor,
            text: text.clone(),
            now,
            refusal: None,
        };
        match current
            .pending_steers
            .enqueue(operation_id.to_string(), pending)
        {
            Ok(inserted) => {
                if current
                    .pending_steers
                    .get(operation_id.as_str())
                    .is_some_and(|saved| saved.refusal.is_some())
                {
                    return Ok(SteerResult::Refused(
                        "the queued steer was refused; its accepted text is retained",
                    ));
                }
                if inserted {
                    journal.save(&current)?;
                }
                Ok(SteerResult::Pending(json!({
                    "queued": true,
                    "warning": "owner placement is still confirming; the graph tick will retry steering"
                })))
            }
            Err(PendingSteerQueueError::TooMany | PendingSteerQueueError::TooLarge) => {
                Ok(SteerResult::Refused("graph steer queue is full"))
            }
            Err(PendingSteerQueueError::OperationConflict) => Ok(SteerResult::Refused(
                "steer operation ID was reused with different input",
            )),
            Err(PendingSteerQueueError::InvalidKey) => Err(FacadeError::StateCorrupt.into()),
        }
    })();
    match result {
        Ok(SteerResult::Explained { data, human }) => crate::emit(
            &provenant_records::CommandResult::new("steer", provenant_records::Outcome::Ok, data),
            globals,
            &human,
            false,
        ),
        Ok(SteerResult::Queued(data)) => crate::emit(
            &provenant_records::CommandResult::new("steer", provenant_records::Outcome::Ok, data),
            globals,
            "steering queued\n",
            false,
        ),
        Ok(SteerResult::Pending(data)) => crate::emit(
            &provenant_records::CommandResult::new(
                "steer",
                provenant_records::Outcome::Pending,
                data,
            ),
            globals,
            "steering queued; owner verification will retry\n",
            false,
        ),
        Ok(SteerResult::SettledQueued(data)) => crate::emit(
            &provenant_records::CommandResult::new("steer", provenant_records::Outcome::Ok, data),
            globals,
            "steering queued for the next follow-up\n",
            false,
        ),
        Ok(SteerResult::SettledWarned(data)) => crate::emit(
            &provenant_records::CommandResult::new("steer", provenant_records::Outcome::Ok, data),
            globals,
            "steering ignored; the target has settled\n",
            false,
        ),
        Ok(SteerResult::Refused(message)) => crate::fail(
            "steer",
            provenant_records::Outcome::Refused,
            "steer_target_refused",
            message,
            "amend the brief and retry with `provenant steer <target> --brief-file FILE`",
            globals,
        ),
        Ok(SteerResult::NoTurn) => crate::fail(
            "steer",
            provenant_records::Outcome::Refused,
            "steer_no_turn",
            "the attempt has no graph continuation turn for steering",
            "start a new graph round with the instruction in its brief",
            globals,
        ),
        Ok(SteerResult::SessionUnavailable(message)) => crate::fail(
            "steer",
            provenant_records::Outcome::Refused,
            "steer_session_unavailable",
            &message,
            "start a new round on the same worktree with `provenant run implement --worktree WORKTREE --brief-file FILE`",
            globals,
        ),
        Ok(SteerResult::CompleteRefused { message, repair }) => crate::fail(
            "steer",
            provenant_records::Outcome::Refused,
            "steer_graph_complete",
            &message,
            &repair,
            globals,
        ),
        Ok(SteerResult::Unsupported(receipt)) => {
            let error = provenant_cli::inbox::unsupported_control_error(&receipt);
            let human = error.message.clone();
            let mut result = provenant_records::CommandResult::new(
                "steer",
                provenant_records::Outcome::Refused,
                json!({"receipts":[receipt]}),
            );
            result.error = Some(error);
            crate::emit(&result, globals, &human, false)
        }
        Err(error) => command_fail("steer", error.as_ref(), globals),
    }
}

fn graph_steer_operation_id(
    caller: Option<&provenant_core::OperationId>,
    work_key: &provenant_records::WorkKey,
) -> Result<provenant_core::OperationId, FacadeError> {
    if caller.is_none() {
        let bytes = provenant_os::fs::entropy_128()
            .map_err(|_| FacadeError::io("create steer operation"))?;
        return provenant_core::OperationId::parse(&format!(
            "steer-{}",
            provenant_core::hex(&bytes)
        ))
        .map_err(|_| FacadeError::StateCorrupt);
    }
    let digest =
        provenant_records::CanonicalV1::record_digest("graph-steer", 2, &(caller, work_key))
            .map_err(|_| FacadeError::StateCorrupt)?;
    provenant_core::OperationId::parse(&format!("steer-{}", provenant_core::hex(digest.as_bytes())))
        .map_err(|_| FacadeError::StateCorrupt)
}

/// One graph in the default listing.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ListedGraph {
    pub name: String,
    pub state: String,
    /// Complete or cancelled: the graph will not change without a retry.
    pub settled: bool,
    pub template: Option<String>,
    pub nodes: usize,
    pub created_micros: Option<i64>,
    pub changed_micros: Option<i64>,
    pub reason: Option<String>,
    pub digests: Vec<super::graph_status::NodeDigest>,
    pub open_asks: Vec<Value>,
}

/// The rows a listing shows and how many settled rows it leaves out.
#[derive(Debug, PartialEq)]
pub(crate) struct Listing {
    pub rows: Vec<ListedGraph>,
    pub omitted: std::collections::BTreeMap<String, usize>,
    pub next: Option<u64>,
    pub admission_holders: Vec<provenant_os::admission::HeldInfo>,
    pub admission_waiters: Vec<provenant_os::admission::WaitInfo>,
    /// Uncommitted work in worktrees no live lane holds. Advisory only.
    pub stranded_work: Vec<StrandedWork>,
}

/// Every graph with work still running is shown: active graphs and settled
/// graphs whose digests report a live attempt, newest first. Only settled
/// graphs without a live lane are pageable history, also newest first, within
/// the last day unless `all` and up to the remaining display allowance. `all`
/// pages through those settled rows with `cursor`, an offset into them. The
/// omitted counts are built from the whole settled population, so a graph
/// outside the window or page is still counted.
pub(crate) fn select_listing(
    graphs: Vec<ListedGraph>,
    now_micros: i64,
    all: bool,
    limit: usize,
    cursor: Option<u64>,
) -> Listing {
    let window = provenant_cli::limits::LISTING_SETTLED_SECONDS * 1_000_000;
    // Liveness, not settlement, decides which graphs can be paged away. A
    // cancelled graph with a running lane belongs with active work on every
    // page, however old it is or how full the display allowance is.
    let (mut present, mut settled): (Vec<_>, Vec<_>) = graphs
        .into_iter()
        .partition(|graph| !graph.settled || graph_has_live_lane(graph));
    let by_age = |a: &ListedGraph, b: &ListedGraph| {
        b.changed_micros
            .cmp(&a.changed_micros)
            .then(a.name.cmp(&b.name))
    };
    present.sort_by(by_age);
    settled.sort_by(by_age);
    let mut omitted = std::collections::BTreeMap::<String, usize>::new();
    for graph in &settled {
        *omitted.entry(graph.state.clone()).or_default() += 1;
    }
    let eligible: Vec<_> = if all {
        settled
    } else {
        settled
            .into_iter()
            .filter(|graph| {
                graph
                    .changed_micros
                    .is_some_and(|changed| now_micros.saturating_sub(changed) <= window)
            })
            .collect()
    };
    // Keep settled pages advancing even when present work alone fills the
    // normal display allowance. Always-present graphs appear on every page.
    let room = if all {
        limit.max(1)
    } else {
        limit.saturating_sub(present.len())
    };
    let skip = if all {
        usize::try_from(cursor.unwrap_or(0)).unwrap_or(usize::MAX)
    } else {
        0
    };
    let shown: Vec<_> = eligible.iter().skip(skip).take(room).cloned().collect();
    let next = (all && skip.saturating_add(shown.len()) < eligible.len())
        .then(|| (skip + shown.len()) as u64);
    for graph in &shown {
        if let Some(count) = omitted.get_mut(&graph.state) {
            *count -= 1;
        }
    }
    omitted.retain(|_, count| *count > 0);
    let mut rows = present;
    rows.extend(shown);
    Listing {
        rows,
        omitted,
        next,
        admission_holders: Vec::new(),
        admission_waiters: Vec::new(),
        stranded_work: Vec::new(),
    }
}

/// `provenant status` with no target: every active graph, then recent
/// settled ones, with a count of what the page leaves out.
pub(crate) fn graph_listing(
    command: &provenant_cli::Status,
    globals: &provenant_cli::Globals,
    workers: &'static dyn WorkerObserver,
) -> u8 {
    let result = (|| -> Result<Listing, Box<dyn std::error::Error>> {
        let factory = match read_factory(globals, workers) {
            Ok(factory) => factory,
            Err(error) if missing_graph_context(error.as_ref()) => {
                return Ok(Listing {
                    rows: Vec::new(),
                    omitted: Default::default(),
                    next: None,
                    admission_holders: Vec::new(),
                    admission_waiters: Vec::new(),
                    stranded_work: Vec::new(),
                });
            }
            Err(error) => return Err(error),
        };
        let root = &factory.context.roots.state;
        let project = factory.context.identity.project;
        let mut graphs = Vec::new();
        // Every graph starts as a cheap checkpoint projection. Evidence is
        // added later only where it can change the page, so settled history
        // does not spend the shared wait budget. Keyed by name so the page
        // pass can find the rows for a graph after selection moves the graphs.
        let mut projections = std::collections::BTreeMap::<String, Vec<Value>>::new();
        for name in FileJournal::graph_ids(root, project)? {
            match FileJournal::read_status_checkpoint(root, project, &name) {
                Ok(Some(checkpoint)) => {
                    use provenant_fabric_runtime::graph::{GraphOutcome, GraphStatus};
                    let run = graph_snapshot(checkpoint);
                    let target = GraphTarget {
                        graph: name.clone(),
                        node: None,
                        attempt: None,
                    };
                    projections.insert(name.clone(), status_rows(&run, &target));
                    // A complete graph reads as how it ended.
                    let state = match run.outcome.filter(|_| run.status == GraphStatus::Complete) {
                        Some(outcome) => json!(outcome),
                        None => json!(run.status),
                    };
                    let reason = (run.outcome != Some(GraphOutcome::Succeeded))
                        .then(|| {
                            run.checkpoint
                                .cancel_reason
                                .as_deref()
                                .or(run.checkpoint.pause_reason())
                                .or(run.advance_error.as_deref())
                                .map(str::to_owned)
                        })
                        .flatten();
                    // An unreadable event log costs only the template and age.
                    let events = FileJournal::read_events(root, project, &name).unwrap_or_default();
                    graphs.push(ListedGraph {
                        settled: matches!(
                            run.status,
                            GraphStatus::Complete | GraphStatus::Cancelled
                        ),
                        state: state.as_str().unwrap_or("unknown").to_owned(),
                        template: events
                            .iter()
                            .find_map(|event| event.attempt.as_ref())
                            .map(|attempt| attempt.template.clone()),
                        nodes: run.checkpoint.nodes.len(),
                        created_micros: events.first().map(|event| event.timestamp_micros),
                        changed_micros: events.last().map(|event| event.timestamp_micros),
                        reason: reason.map(|reason| {
                            provenant_fabric_runtime::hosts::plain_text(&reason, 1024)
                        }),
                        digests: Vec::new(),
                        open_asks: run.checkpoint.asks.iter().map(|ask| json!(ask)).collect(),
                        name,
                    });
                }
                Ok(None) => {}
                Err(error) if error.is_unreadable() => graphs.push(ListedGraph {
                    reason: Some(
                        provenant_fabric_runtime::graph::unreadable_graph_explanation(
                            &name, &error,
                        ),
                    ),
                    name,
                    state: "unreadable".into(),
                    settled: false,
                    digests: Vec::new(),
                    open_asks: Vec::new(),
                    template: None,
                    nodes: 0,
                    created_micros: None,
                    changed_micros: None,
                }),
                Err(error) => return Err(error.into()),
            }
        }
        let now = factory.clock.wall_now()?.as_micros();
        let cancel = CancelToken::new();
        // One bounded, advisory reading: uncommitted work in worktrees with no
        // live lane. It never changes graph selection or refuses the command.
        let stranded_work = crate::stranded_work::survey(
            &factory.context.roots,
            &factory.home,
            &factory.cwd,
            workers,
            &factory.clock,
            &cancel,
            now,
        );
        let budget = WaitBudget::cli(factory.clock.mono_now(), 55)?;
        let (mut finished, gathered) = listing_with_evidence(
            graphs,
            &mut projections,
            (
                now,
                command.all || globals.full,
                usize::try_from(command.limit).unwrap_or(usize::MAX),
                command.cursor,
            ),
            || StatusFacts::gather(&factory.context, &factory.clock, &cancel),
            |rows, facts| {
                enrich_status_rows_with_facts(rows, &factory.context, &factory.clock, facts, budget)
            },
        );
        finished.stranded_work = stranded_work;
        if root.as_path().exists() {
            finished.admission_holders = listing_holders(
                gathered,
                |facts| facts.lane_holders,
                || provenant_os::admission::all_holders(root.as_path()),
            );
            finished.admission_waiters = provenant_os::admission::all_waiters(root.as_path());
        }
        Ok(finished)
    })();
    match result {
        Ok(listing) => {
            let now = SystemClock::new()
                .wall_now()
                .map_or(i64::MAX, provenant_core::WallTime::as_micros);
            let mut human = String::from("NAME STATE TEMPLATE NODES AGE\n");
            for graph in &listing.rows {
                let age = graph_age(now, graph);
                human.push_str(&format!(
                    "{} {} {} {} {age}\n",
                    graph.name,
                    listed_state(graph),
                    graph.template.as_deref().unwrap_or("-"),
                    graph.nodes
                ));
                for digest in graph
                    .digests
                    .iter()
                    .take(provenant_cli::limits::LISTING_ROWS)
                {
                    human.push_str(&format!("  {}\n", digest.line));
                }
                let mut omitted_nodes = std::collections::BTreeMap::<&str, usize>::new();
                for digest in graph
                    .digests
                    .iter()
                    .skip(provenant_cli::limits::LISTING_ROWS)
                {
                    *omitted_nodes.entry(&digest.state).or_default() += 1;
                }
                for (state, count) in omitted_nodes {
                    human.push_str(&format!(
                        "  {count} {state} nodes not shown; --json lists every digest\n"
                    ));
                }
                for ask in &graph.open_asks {
                    human.push_str(&format!(
                        "  open_ask={} node={}\n",
                        plain(&ask["ask_id"]),
                        plain(&ask["node"])
                    ));
                }
                if let Some(reason) = &graph.reason {
                    human.push_str(&format!(
                        "  {}\n",
                        provenant_cli::bounded_text(
                            reason,
                            provenant_cli::limits::SUMMARY_ROW_BYTES
                        )
                    ));
                }
            }
            if listing.rows.is_empty() {
                // An empty page with omitted settled graphs is not an empty
                // graph population; the omitted lines below name those graphs.
                human = if listing.omitted.is_empty() {
                    "no graphs\n".into()
                } else {
                    String::new()
                };
            }
            let live = live_lanes(&listing);
            if live > 0 {
                human.insert_str(0, &format!("{live} live lanes\n"));
            }
            let admission = admission_lines(&listing, now);
            if !admission.is_empty() {
                if !listing.rows.is_empty() {
                    human.push('\n');
                }
                for line in &admission {
                    human.push_str(line);
                    human.push('\n');
                }
            }
            let stranded = human_lines(&listing.stranded_work);
            if !stranded.is_empty() {
                if !human.is_empty() && !human.ends_with('\n') {
                    human.push('\n');
                }
                for line in stranded {
                    human.push_str(&line);
                    human.push('\n');
                }
            }
            let more = if command.all || globals.full {
                "--cursor"
            } else {
                "--full"
            };
            for (state, count) in &listing.omitted {
                human.push_str(&format!(
                    "{count} {state} graph(s) not shown; use {more} to list them\n"
                ));
            }
            let page_next = listing.next.map(|cursor| {
                provenant_records::command_result::NextStep::from_args(
                    &[
                        "provenant",
                        "status",
                        "--all",
                        "--limit",
                        &command.limit.to_string(),
                        "--cursor",
                        &cursor.to_string(),
                    ],
                    "read the next page of graphs",
                )
            });
            if let Ok(roots) = crate::roots::resolve(
                globals,
                crate::roots::Workspace::Home,
                crate::roots::Product::Optional,
            ) {
                let slots =
                    provenant_run::build::hygiene::status(&roots.home, roots.roots.state.as_path());
                let ok = slots.iter().filter(|slot| slot.health == "ok").count();
                let quarantined = slots
                    .iter()
                    .filter(|slot| slot.health == "quarantined")
                    .count();
                let bytes: u64 = slots.iter().map(|slot| slot.allocated_bytes).sum();
                human.push_str(&format!(
                    "build slots: {ok} ok, {quarantined} quarantined, {:.2} GiB\n",
                    bytes as f64 / 1_073_741_824.0
                ));
            }
            let rows: Vec<_> = listing
                .rows
                .iter()
                .map(|graph| {
                    json!({
                        "name":graph.name,"state":graph.state,"template":graph.template,
                        "nodes":graph.nodes,"created_micros":graph.created_micros,
                        "changed_micros":graph.changed_micros,"reason":graph.reason,
                        "digests":graph.digests,"open_asks":graph.open_asks,
                    })
                })
                .collect();
            let summary = listing_summary(&listing, command.limit, now);
            let mut data = json!({
                "graphs": rows,
                "omitted": listing.omitted,
                "next_cursor": listing.next,
                "live_lanes": live,
            });
            if !listing.admission_holders.is_empty() || !listing.admission_waiters.is_empty() {
                data["admission"] = json!({
                    "holders": listing.admission_holders,
                    "waiters": listing.admission_waiters,
                });
            }
            if !listing.stranded_work.is_empty() {
                data["stranded_work"] = json!(listing.stranded_work);
            }
            crate::emit_summarised(
                &provenant_records::CommandResult::new(
                    "status",
                    provenant_records::Outcome::Ok,
                    data,
                )
                .with_next(page_next.into_iter().collect()),
                globals,
                &human,
                true,
                (!globals.full).then_some(&summary),
            )
        }
        Err(error) => command_fail("status", error.as_ref(), globals),
    }
}

/// The `admission:` section of a status listing: the waiters, each with the
/// holders of its class, then the holders. Shared by the full human view and
/// the compact summary so both name the same queue.
fn admission_lines(listing: &Listing, now_micros: i64) -> Vec<String> {
    if listing.admission_waiters.is_empty() && listing.admission_holders.is_empty() {
        return Vec::new();
    }
    let now_sec = (now_micros.max(0) / 1_000_000) as u64;
    let mut lines = vec!["admission:".to_owned()];
    for waiter in &listing.admission_waiters {
        let elapsed = now_sec.saturating_sub(waiter.start_unix_seconds);
        let holders = listing
            .admission_holders
            .iter()
            .filter(|h| h.class == waiter.class)
            .map(provenant_cli::run::admission_holder_description)
            .collect::<Vec<_>>();
        if holders.is_empty() {
            lines.push(format!(
                "  {} waiter: {} ({}s)",
                waiter.class,
                admission_waiter_description(waiter),
                elapsed
            ));
        } else {
            lines.push(format!(
                "  {} waiter: {} ({}s; held by {})",
                waiter.class,
                admission_waiter_description(waiter),
                elapsed,
                holders.join(", ")
            ));
        }
    }
    for holder in &listing.admission_holders {
        let elapsed = now_sec.saturating_sub(holder.start_unix_seconds);
        lines.push(format!(
            "  {} holder: {} ({}s)",
            holder.class,
            provenant_cli::run::admission_holder_description(holder),
            elapsed
        ));
    }
    lines
}

/// A graph may still own a live lane when its checkpoint projection names an
/// attempt that has not reached a terminal state (`starting`, `running` or
/// `unresolved`). This is a cheap over-approximation: only enrichment decides
/// whether the lane is actually live, so it is safe to enrich every match and
/// still bound cost to the page plus the possibly-live graphs.
fn projection_has_live_attempt(rows: &[Value]) -> bool {
    rows.iter().any(|row| {
        row["kind"] == "attempt"
            && !row["attempt_id"].is_null()
            && row["state"]
                .as_str()
                .is_some_and(|state| state != "terminal")
    })
}

/// Append evidence failure to a graph's existing reason without hiding the
/// retained projection, which still marks external facts stale.
fn status_evidence_failure(reason: Option<&str>, error: &FacadeError) -> String {
    provenant_fabric_runtime::hosts::plain_text(
        &format!(
            "{}; status evidence unavailable: {error}",
            reason.unwrap_or("")
        ),
        1024,
    )
}

fn listing_holders<F, H>(
    facts: Option<Result<F, FacadeError>>,
    from_facts: impl FnOnce(F) -> H,
    read: impl FnOnce() -> H,
) -> H {
    match facts {
        Some(Ok(facts)) => from_facts(facts),
        _ => read(),
    }
}

/// Share one fallible host reading across the liveness and page passes. An
/// evidence failure remains attached to each affected graph, not the listing.
fn listing_with_evidence<F>(
    mut graphs: Vec<ListedGraph>,
    projections: &mut std::collections::BTreeMap<String, Vec<Value>>,
    selection: (i64, bool, usize, Option<u64>),
    mut gather: impl FnMut() -> Result<F, FacadeError>,
    enrich_rows: impl Fn(&mut [Value], &F) -> Result<(), FacadeError>,
) -> (Listing, Option<Result<F, FacadeError>>) {
    let (now, full, limit, cursor) = selection;
    let gathered = std::cell::OnceCell::new();
    let mut enrich = |_name: &str, rows: &mut [Value]| {
        let facts = gathered.get_or_init(&mut gather);
        match facts {
            Ok(facts) => enrich_rows(rows, facts),
            Err(error) => Err(error.clone()),
        }
    };
    let enriched = enrich_liveness_candidates(&mut graphs, projections, &mut enrich);
    let settled_count = graphs.iter().filter(|graph| graph.settled).count();
    let mut listing = select_listing(graphs, now, full, limit, cursor);
    for graph in &mut listing.rows {
        if enriched.contains(&graph.name) {
            continue;
        }
        let Some(rows) = projections.get_mut(&graph.name) else {
            continue;
        };
        if let Err(error) = enrich(&graph.name, rows) {
            graph.reason = Some(status_evidence_failure(graph.reason.as_deref(), &error));
        }
        graph.digests = super::graph_status::node_digests(rows);
    }
    (
        finish_listing(listing, settled_count, full),
        gathered.into_inner(),
    )
}

/// Resolve liveness only for the graphs that can change the page: active work
/// and settled graphs whose projection still names a non-terminal attempt. A
/// settled graph whose attempts are all terminal can never report a live lane,
/// so enriching it would only spend the shared wait budget on history that is
/// about to be omitted. Returns the names that received fresh evidence so the
/// page pass can skip rows that are already current.
fn enrich_liveness_candidates(
    graphs: &mut [ListedGraph],
    projections: &mut std::collections::BTreeMap<String, Vec<Value>>,
    mut enrich: impl FnMut(&str, &mut [Value]) -> Result<(), FacadeError>,
) -> std::collections::BTreeSet<String> {
    let mut enriched = std::collections::BTreeSet::new();
    for graph in graphs.iter_mut() {
        let Some(rows) = projections.get_mut(&graph.name) else {
            // No checkpoint: an unreadable graph has no attempt rows.
            continue;
        };
        if graph.settled && !projection_has_live_attempt(rows) {
            // Settled history with only terminal attempts. Digests from the
            // projection cannot report a live presence, so it is counted as
            // omitted without any evidence read.
            graph.digests = super::graph_status::node_digests(rows);
            continue;
        }
        if let Err(error) = enrich(&graph.name, rows) {
            graph.reason = Some(status_evidence_failure(graph.reason.as_deref(), &error));
        }
        graph.digests = super::graph_status::node_digests(rows);
        enriched.insert(graph.name.clone());
    }
    enriched
}

/// A graph still has live work when any of its digests reports a `live`
/// attempt presence. Settlement is a separate fact: a cancelled graph can
/// leave lanes running, and an unknown presence is not liveness.
fn graph_has_live_lane(graph: &ListedGraph) -> bool {
    graph
        .digests
        .iter()
        .any(|digest| digest.attempt_presence.as_deref() == Some("live"))
}

/// Live lanes read from the same digests that order the listing. A lane is
/// live only when its attempt presence is `live`, never from a graph's settled
/// state or an unknown presence.
fn live_lanes(listing: &Listing) -> usize {
    listing
        .rows
        .iter()
        .flat_map(|graph| graph.digests.iter())
        .filter(|digest| digest.attempt_presence.as_deref() == Some("live"))
        .count()
}

/// The state a graph is listed and tallied under. It is the state of its own
/// nodes and attempts, never the graph checkpoint's coarse status: an
/// unfinished graph reports `pending` there even while its attempt is live.
/// The most urgent node wins, with a live attempt breaking ties; an unenriched
/// graph (no digests) keeps its checkpoint state.
fn listed_state(graph: &ListedGraph) -> &str {
    graph
        .digests
        .iter()
        .map(|digest| {
            let state = digest.display_state();
            let rank = match state {
                // A presence label or a pre-admission wait is still live work,
                // even though those words are not node states.
                "unattended" | "settling" | "unknown" | "waiting" => StateRank::Live,
                state => StateRank::of(state),
            };
            (
                state,
                rank,
                std::cmp::Reverse(digest.attempt_presence.as_deref() == Some("live")),
            )
        })
        .min_by_key(|(_, rank, live)| (*rank, *live))
        .map_or(graph.state.as_str(), |(state, _, _)| state)
}

fn listing_summary(
    listing: &Listing,
    _command_limit: u64,
    now_micros: i64,
) -> provenant_cli::Summary {
    let tally = ranked_tally(listing.rows.iter().map(listed_state));
    let mut headline = if tally.is_empty() {
        "no graphs".to_string()
    } else {
        tally
    };
    let live = live_lanes(listing);
    if live > 0 {
        headline = format!("{live} live lanes; {headline}");
    }

    let mut summary_rows = Vec::new();
    let mut shown_indices = std::collections::BTreeSet::new();

    for (i, graph) in listing.rows.iter().enumerate() {
        if StateRank::of(listed_state(graph)).needs_attention() {
            let mut line = format!(
                "{} {} {} {}",
                graph.name,
                listed_state(graph),
                graph.template.as_deref().unwrap_or("-"),
                graph.nodes
            );
            if let Some(reason) = &graph.reason {
                line.push_str(&format!(": {reason}"));
            }
            summary_rows.push(line);
            shown_indices.insert(i);
        }
    }

    let live_cap = provenant_cli::limits::SUMMARY_ROWS;
    let live_slots = live_cap.saturating_sub(summary_rows.len());
    if live_slots > 0 {
        let mut added = 0;
        for (i, graph) in listing.rows.iter().enumerate() {
            if !shown_indices.contains(&i) && StateRank::of(listed_state(graph)).is_live() {
                summary_rows.push(format!(
                    "{} {} {} {}",
                    graph.name,
                    listed_state(graph),
                    graph.template.as_deref().unwrap_or("-"),
                    graph.nodes
                ));
                shown_indices.insert(i);
                added += 1;
                if added >= live_slots {
                    break;
                }
            }
        }
    }

    let omitted = listing.rows.len().saturating_sub(shown_indices.len());

    provenant_cli::Summary {
        lines: std::iter::once(headline)
            .chain(summary_rows)
            .chain(admission_lines(listing, now_micros))
            .chain(summary_lines(&listing.stranded_work))
            .collect(),
        omitted,
    }
}

fn admission_waiter_description(waiter: &provenant_os::admission::WaitInfo) -> String {
    provenant_cli::run::admission_actor_description(
        waiter.pid,
        waiter.attempt_id.as_deref(),
        waiter.lane.as_ref(),
    )
}

fn age_text(micros: i64) -> String {
    let seconds = micros.max(0) / 1_000_000;
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        3600..86400 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86400),
    }
}

fn graph_age(now_micros: i64, graph: &ListedGraph) -> String {
    graph
        .created_micros
        .map_or_else(|| "-".to_owned(), |created| age_text(now_micros - created))
}

/// Order a page with live lanes first and, outside `--full`, hide settled rows
/// behind one count. A settled graph still has its lanes shown when a digest
/// reports a live attempt: settlement and liveness are separate facts, and a
/// cancelled graph can leave lanes running. `select_listing` has already
/// classified by liveness and ordered each group by age.
pub(crate) fn finish_listing(mut listing: Listing, settled_count: usize, full: bool) -> Listing {
    listing
        .rows
        .sort_by_key(|graph| !graph_has_live_lane(graph));
    if !full {
        listing
            .rows
            .retain(|graph| !graph.settled || graph_has_live_lane(graph));
        listing.next = None;
        // The settled graphs still on the page are exactly the ones kept for
        // their live lanes. Everything else in the full settled population is
        // omitted, including graphs outside the window or past the page limit
        // that never reached this listing. Counting only dropped page rows
        // would make those older graphs vanish from the omitted total.
        let shown_settled = listing.rows.iter().filter(|graph| graph.settled).count();
        let omitted = settled_count.saturating_sub(shown_settled);
        listing.omitted.clear();
        if omitted > 0 {
            listing.omitted.insert("settled".to_owned(), omitted);
        }
    }
    listing
}

/// Read the retained graph journal without consulting live providers. A
/// numbered target keeps only that attempt's events.
pub(crate) fn unified_graph_trace(
    target: &provenant_cli::Target,
    globals: &provenant_cli::Globals,
    workers: &'static dyn WorkerObserver,
) -> u8 {
    let result = (|| -> Result<Value, Box<dyn std::error::Error>> {
        let id = match &target.node {
            Some(node) => format!("{}/{node}", target.name),
            None => target.name.clone(),
        };
        // A lane reads through the host, which holds the configuration.
        if provenant_cli::fabric::in_lane() {
            let mut data = lane_graph_read(json!({"op":"trace", "id":id}))?;
            add_cache_share(&mut data);
            return Ok(data);
        }
        let factory = read_factory(globals, workers).map_err(|error| {
            if missing_graph_context(error.as_ref()) {
                Box::new(outside_project_fault(target)) as Box<dyn std::error::Error>
            } else {
                error
            }
        })?;
        let attempt = if factory.context.in_lane {
            None
        } else {
            resolve(
                &factory.context.roots.state,
                factory.context.identity.project,
                target,
                Want::Work,
            )
            .map_err(target_fault)?
            .attempt_ref
        };
        let budget = WaitBudget::cli(
            factory.clock.mono_now(),
            factory.context.config.graph.launch_wait_seconds,
        )?;
        let mut data =
            factory.graph_document(json!({"op":"trace", "id":id}), budget, &CancelToken::new())?;
        if let (Some(reference), Some(events)) = (attempt, data["events"].as_array_mut()) {
            events.retain(|event| event["attempt"]["identity"] == reference.as_str());
        }
        add_cache_share(&mut data);
        Ok(data)
    })();
    match result {
        Ok(data) => {
            let human = data["events"]
                .as_array()
                .into_iter()
                .flatten()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n";
            let display_globals = provenant_cli::Globals {
                json: globals.json,
                full: true,
                state_dir: globals.state_dir.clone(),
                project: globals.project.clone(),
            };
            crate::emit(
                &provenant_records::CommandResult::new(
                    "trace",
                    provenant_records::Outcome::Ok,
                    data,
                ),
                &display_globals,
                &human,
                true,
            )
        }
        Err(error) => command_fail("trace", error.as_ref(), globals),
    }
}

fn add_cache_share(data: &mut Value) {
    if let Some(events) = data["events"].as_array_mut() {
        for event in events {
            if let Some(attempt) = event.get_mut("attempt").and_then(Value::as_object_mut) {
                let share = attempt
                    .get("cache_read_tokens")
                    .and_then(Value::as_u64)
                    .zip(attempt.get("input_tokens").and_then(Value::as_u64))
                    .and_then(|(cached, input)| {
                        (input > 0).then_some(cached as f64 / input as f64)
                    });
                attempt.insert("cache_read_share".into(), json!(share));
            }
        }
    }
}

/// Summarise the latest retained evidence through the same identity-checked status reader.
#[allow(clippy::disallowed_methods, reason = "bound the CLI read deadline")]
pub(crate) fn unified_graph_result(
    target: &provenant_cli::Target,
    edge: &provenant_cli::run::Edge<'_>,
) -> provenant_cli::run::Reply {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(55);
    let rows = match read_status_rows(target, edge, &mut None, deadline, &CancelToken::new()) {
        Ok(rows) => rows,
        Err(error) => return output_error(error.as_ref()),
    };
    result_reply(target, &rows)
}

/// Compact, deterministic severity tally for a review row's typed findings.
fn finding_severity_counts(findings: &[Value]) -> String {
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for finding in findings {
        if let Some(severity) = finding["severity"].as_str() {
            *counts.entry(severity).or_default() += 1;
        }
    }
    if counts.is_empty() {
        "none".to_owned()
    } else {
        counts
            .iter()
            .map(|(severity, count)| format!("{count} {severity}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn review_availability(row: &Value) -> &'static str {
    match row["findings_available"].as_bool() {
        Some(true) => "available",
        Some(false) => "incomplete",
        None => "availability unknown",
    }
}

fn result_reply(target: &provenant_cli::Target, rows: &[Value]) -> provenant_cli::run::Reply {
    let selected: Vec<_> = rows
        .iter()
        .filter(|row| {
            row["kind"] == "node"
                || (row["kind"] == "attempt"
                    && (target.attempt.is_some()
                        || !rows.iter().any(|other| {
                            other["kind"] == "attempt"
                                && other["node_id"] == row["node_id"]
                                && other["attempt_number"].as_u64() > row["attempt_number"].as_u64()
                        })))
        })
        .cloned()
        .collect();
    let mut human = String::new();
    let mut compact_rows = Vec::new();
    for row in &selected {
        if row["kind"] == "attempt"
            && selected
                .iter()
                .any(|other| other["kind"] == "node" && other["node_id"] == row["node_id"])
        {
            continue;
        }
        let attempt = selected
            .iter()
            .find(|other| other["kind"] == "attempt" && other["node_id"] == row["node_id"]);
        let field = |name: &str| {
            row[name]
                .as_str()
                .or_else(|| attempt.and_then(|a| a[name].as_str()))
                .unwrap_or("unknown")
        };
        let handoff = row["handoff"]
            .get("Ok")
            .filter(|h| !h.is_null())
            .unwrap_or_else(|| {
                attempt
                    .map(|a| a["handoff"].get("Ok").unwrap_or(&a["handoff"]))
                    .unwrap_or(&row["handoff"])
            });
        let checks = handoff["verify"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|s| provenant_fabric_runtime::hosts::plain_text(s, 100))
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "none recorded".into());
        let outcome = row["outcome"]
            .as_str()
            .or_else(|| attempt.and_then(|a| a["outcome"].as_str()))
            .or_else(|| row["state"].as_str())
            .unwrap_or("unknown");
        let cause = row["cause_code"]
            .as_str()
            .or_else(|| row["reason"].as_str())
            .or_else(|| attempt.and_then(|a| a["cause_code"].as_str()))
            .or_else(|| attempt.and_then(|a| a["reason"].as_str()));
        // `verdict` mirrors `review_verdict` on node rows; an attempt row
        // carries only the latter.
        let verdict = row["verdict"]
            .as_str()
            .or_else(|| row["review_verdict"].as_str())
            .or_else(|| attempt.and_then(|a| a["verdict"].as_str()))
            .or_else(|| attempt.and_then(|a| a["review_verdict"].as_str()));
        let result_path = super::graph_status::path_text(&row["result_path"])
            .or_else(|| attempt.and_then(|a| super::graph_status::path_text(&a["result_path"])));

        let mut parts = Vec::new();
        parts.push(format!(
            "{}: {}",
            provenant_cli::bounded_text(field("id"), 24),
            provenant_cli::bounded_text(outcome, 32)
        ));
        if let Some(cause) = cause {
            parts.push(format!("cause: {}", provenant_cli::bounded_text(cause, 64)));
        }
        if let Some(verdict) = verdict {
            parts.push(format!(
                "verdict: {}",
                provenant_cli::bounded_text(verdict, 32)
            ));
        }
        if let Some(items) = row["findings"].as_array() {
            parts.push(format!(
                "findings: {} ({})",
                provenant_cli::bounded_text(&finding_severity_counts(items), 64),
                review_availability(row)
            ));
        }
        if let Some(path) = result_path {
            parts.push(format!(
                "result: {}",
                provenant_cli::bounded_text(&path, 128)
            ));
        }
        let branch = field("branch");
        if branch != "unknown" && !branch.is_empty() {
            parts.push(format!(
                "branch: {}",
                provenant_cli::bounded_text(branch, 24)
            ));
        }
        let head = field("head");
        if head != "unknown" && !head.is_empty() {
            parts.push(format!("head: {}", provenant_cli::bounded_text(head, 64)));
        }
        if checks != "none recorded" {
            parts.push(format!(
                "verify: {}",
                provenant_cli::bounded_text(&checks, 40)
            ));
        }
        compact_rows.push(parts.join(" | "));
    }
    if compact_rows.is_empty() {
        compact_rows.push("no lane result yet".into());
    }
    for row in &selected {
        let id = row["id"].as_str().unwrap_or("unknown");
        let state = row["state"].as_str().unwrap_or("unknown");
        human.push_str(&format!("{id}: {state}\n"));
        for (label, value) in [
            ("branch", &row["branch"]),
            ("head", &row["head"]),
            ("result", &row["result_summary"]),
        ] {
            if !value.is_null() {
                human.push_str(&format!(
                    "  {label}: {}\n",
                    value
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| value.to_string())
                ));
            }
        }
        if let Some(verdict) = row["verdict"].as_str() {
            human.push_str(&format!(
                "  verdict: {}\n",
                provenant_cli::bounded_text(verdict, 128)
            ));
        }
        if let Some(items) = row["findings"].as_array() {
            human.push_str(&format!(
                "  findings: {} ({})\n",
                finding_severity_counts(items),
                review_availability(row)
            ));
            for finding in items {
                human.push_str(&format!(
                    "  {}\n",
                    provenant_cli::run::finding_human_line(finding)
                ));
            }
        }
        if row["partial"] == json!(true) {
            human.push_str("  partial: true\n");
        }
        let handoff = row["handoff"].get("Ok").unwrap_or(&row["handoff"]);
        for field in ["status", "note"] {
            if let Some(text) = handoff[field].as_str() {
                human.push_str(&format!(
                    "  {field}: {}\n",
                    provenant_fabric_runtime::hosts::plain_text(text, 4096)
                ));
            }
        }
        for field in ["verify", "remaining", "decisions"] {
            if let Some(items) = handoff[field].as_array() {
                for item in items.iter().filter_map(Value::as_str) {
                    human.push_str(&format!(
                        "  {field}: {}\n",
                        provenant_fabric_runtime::hosts::plain_text(item, 4096)
                    ));
                }
            }
        }
        if let Some(concerns) = handoff["spec_concerns"].as_array() {
            for concern in concerns {
                human.push_str(&format!("  spec concern: {concern}\n"));
            }
        }
    }
    if selected.is_empty() {
        human.push_str("no lane result yet\n");
    }
    provenant_cli::run::Reply {
        result: provenant_records::CommandResult::new(
            "result",
            provenant_records::Outcome::Ok,
            json!({"rows": selected}),
        ),
        human,
        summary: Some(provenant_cli::Summary::new("lane results", compact_rows)),
        read_only: true,
    }
}

/// Read one attempt's output. Inside a lane the host reads it, so no journal
/// reader is constructed here.
pub(crate) fn unified_graph_output(
    command: &provenant_cli::Output,
    edge: &provenant_cli::run::Edge<'_>,
    globals: &provenant_cli::Globals,
    workers: &'static dyn WorkerObserver,
) -> provenant_cli::run::Reply {
    if !provenant_cli::fabric::in_lane() {
        let read = || -> Result<provenant_cli::run::Reply, Box<dyn std::error::Error>> {
            let factory = read_factory(globals, workers).map_err(|error| {
                if missing_graph_context(error.as_ref()) {
                    Box::new(outside_project_fault(&command.target))
                } else {
                    error
                }
            })?;
            let budget = WaitBudget::cli(
                factory.clock.mono_now(),
                factory.context.config.graph.launch_wait_seconds,
            )?;
            host_graph_output(
                command,
                edge,
                &factory,
                budget,
                &CancelToken::new(),
                globals.full,
            )
        };
        return read().unwrap_or_else(|error| output_error(error.as_ref()));
    }
    let part = match command.part {
        provenant_cli::Part::Result => "result",
        provenant_cli::Part::Prompt => "prompt",
        provenant_cli::Part::Stderr => "stderr",
        provenant_cli::Part::Events => "events",
        provenant_cli::Part::Receipt => "receipt",
        provenant_cli::Part::Transcript => "transcript",
    };
    if command.final_result {
        let result = provenant_cli::run::collect_final(command, |page| {
            let input = json!({"op":"output", "id":page.target.to_string(),
                "render":true, "part":part, "offset":page.offset, "full":false,
                "max_bytes":page.max_bytes});
            let response = provenant_cli::fabric::graph_relay::from_lane(
                &provenant_fabric_runtime::graph::relay::RelayOp::Read { input },
                &CancelToken::new(),
                std::time::Duration::from_secs(55),
            )
            .map_err(|error| {
                provenant_cli::run::Fail::new(
                    provenant_records::Outcome::Error,
                    provenant_run::ErrorKind::Fabric,
                    error.to_string(),
                )
            })?;
            let result: provenant_records::CommandResult<Value> =
                serde_json::from_value(response["result"].clone()).map_err(|error| {
                    provenant_cli::run::Fail::new(
                        provenant_records::Outcome::Error,
                        provenant_run::ErrorKind::StateCorrupt,
                        error.to_string(),
                    )
                })?;
            if let Some(error) = result.error {
                return Err(provenant_cli::run::Fail::new(
                    result.status,
                    provenant_run::ErrorKind::Fabric,
                    error.message,
                ));
            }
            Ok((result.data, String::new()))
        });
        return provenant_cli::run::render_output(command.clone(), result, globals.full);
    }
    let result = (|| -> Result<provenant_cli::run::Reply, Box<dyn std::error::Error>> {
        if command.path {
            return Err("lane reads return content; omit --path".into());
        }
        let input = json!({"op":"output", "id":command.target.to_string(),
            "render":true,"part":part,"offset":command.offset,"tail":command.tail,
            "tail_lines":command.tail_lines,"full":globals.full,"max_bytes":command.max_bytes});
        let response = provenant_cli::fabric::graph_relay::from_lane(
            &provenant_fabric_runtime::graph::relay::RelayOp::Read { input },
            &CancelToken::new(),
            std::time::Duration::from_secs(55),
        )?;
        let mut reply = provenant_cli::run::Reply {
            result: serde_json::from_value(response["result"].clone())?,
            human: response["human"].as_str().unwrap_or_default().to_owned(),
            summary: None,
            read_only: true,
        };
        if let Some(path) = &command.to
            && reply.result.status == provenant_records::Outcome::Ok
        {
            let bytes = returned_output_bytes(&reply.result.data)?;
            std::fs::write(path, &bytes)?;
            let written_path = std::fs::canonicalize(path)?;
            reply.result.data["to"] = json!(provenant_records::StructuredPathWire::from_path(
                &written_path
            )?);
            reply.result.data["written"] = json!(bytes.len());
            reply.human = format!("wrote {} bytes to {}\n", bytes.len(), path.display());
        }
        Ok(reply)
    })();
    result.unwrap_or_else(|error| match error.downcast_ref::<ServiceError>() {
        Some(service) => {
            if let Some((outcome, code, message)) = command_target_fault(service, None) {
                return output_fail(outcome, code, &message);
            }
            let diagnostic = service.diagnostic();
            output_fail(
                provenant_records::Outcome::Error,
                "graph_target",
                &format!(
                    "{}: {}",
                    diagnostic.code,
                    diagnostic
                        .detail
                        .as_deref()
                        .unwrap_or("host graph read refused")
                ),
            )
        }
        None => output_error(error.as_ref()),
    })
}

fn returned_output_bytes(data: &Value) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let encoded = &data["bytes"];
    match (encoded["encoding"].as_str(), encoded["data"].as_str()) {
        (Some("utf8"), Some(text)) => Ok(text.as_bytes().to_vec()),
        (Some("base64"), Some(text)) => provenant_fabric_runtime::handoff::decode_base64(text)
            .ok_or_else(|| "invalid output encoding".into()),
        _ => Err(
            "--to needs a part that has bytes; select result, transcript, stderr or prompt".into(),
        ),
    }
}

/// The one attempt a target names, read through the ordinary attempt reader.
/// A target without `#N` also reports its node's failure, if it failed.
fn host_graph_output(
    command: &provenant_cli::Output,
    edge: &provenant_cli::run::Edge<'_>,
    factory: &Factory,
    budget: WaitBudget,
    cancel: &CancelToken,
    full: bool,
) -> Result<provenant_cli::run::Reply, Box<dyn std::error::Error>> {
    let (resolved, checkpoint) = match resolve_output_with_checkpoint(factory, &command.target) {
        Ok(found) => found,
        // A node that failed before it launched has no attempt to read;
        // its failure and remedy are still the answer.
        Err(fault) if command.target.attempt.is_none() && fault.code == "NOT_FOUND" => {
            return match unlaunched_failure_output(command, factory, budget, cancel)? {
                Some(reply) => Ok(reply),
                None => Err(fault.into()),
            };
        }
        Err(fault) => return Err(fault.into()),
    };
    let progress = checkpoint
        .nodes
        .iter()
        .find(|node| Some(node.id.as_str()) == resolved.node.as_deref())
        .ok_or(FacadeError::StateCorrupt)?;
    if !progress.children.is_empty() {
        return composite_graph_output(command, &resolved.graph, progress, full);
    }
    let Some(reference) = resolved.attempt_ref.clone() else {
        let fault = TargetFault {
            outcome: provenant_records::Outcome::Invalid,
            code: "NOT_FOUND",
            message: format!("{} has not launched", resolved.target()),
        };
        if command.target.attempt.is_none()
            && let Some(reply) = unlaunched_failure_output(command, factory, budget, cancel)?
        {
            return Ok(reply);
        }
        return Err(fault.into());
    };
    // A settled lane that already produced a result has answered the reader.
    // Appending the graph's failure rows buries that answer in stale status
    // lines, including "unreadable" lines for retained attempts whose records
    // can no longer be decoded (#3976). An unsettled lane or one with no result
    // still gets its failure diagnostics.
    let needs_failures = output_needs_graph_failures(command.target.attempt.is_some(), progress);
    let failure_rows = if needs_failures {
        node_failure_rows(
            factory,
            checkpoint,
            &resolved.graph,
            resolved.node.clone(),
            budget,
            cancel,
        )?
    } else {
        Vec::new()
    };
    let mut selected = command.clone();
    selected.target = resolved.target();
    let reply = graph_attempt_output(selected, &reference, edge, factory, budget, cancel, full)?;
    Ok(with_graph_failures(reply, &failure_rows))
}

fn composite_graph_output(
    command: &provenant_cli::Output,
    graph: &str,
    progress: &provenant_fabric_runtime::graph::NodeProgress,
    full: bool,
) -> Result<provenant_cli::run::Reply, Box<dyn std::error::Error>> {
    if command.final_result {
        return Ok(output_fail(
            provenant_records::Outcome::Invalid,
            "INVALID",
            "--final needs a lane attempt; a composite exposes only its declared result",
        ));
    }
    if command.path {
        return Ok(output_fail(
            provenant_records::Outcome::Invalid,
            "NOT_FOUND",
            "a composite has no attempt artifact path; read its declared result",
        ));
    }
    if !matches!(command.part, provenant_cli::Part::Result) {
        return Ok(output_fail(
            provenant_records::Outcome::Invalid,
            "NOT_FOUND",
            "a composite exposes its declared result, not an attempt prompt, stderr, transcript, events or receipt",
        ));
    }
    let Some(outputs) = progress.outputs.as_ref() else {
        return Ok(output_fail(
            provenant_records::Outcome::Invalid,
            "NOT_FOUND",
            &format!("{graph}/{} has no settled composite outputs", progress.id),
        ));
    };
    // A valid composite may export typed fields without declaring result text.
    let result = outputs.result.as_ref();
    let text = result.map_or("", |result| result.excerpt.as_str());
    let length = text.len();
    let tail = command.tail_selection();
    let mut start = if tail.is_some() {
        length.saturating_sub(command.max_bytes as usize)
    } else {
        usize::try_from(command.offset)
            .unwrap_or(usize::MAX)
            .min(length)
    };
    let end = start.saturating_add(command.max_bytes as usize).min(length);
    if let Some(tail) = tail {
        let window = &text.as_bytes()[start..end];
        let bounded_start = match tail {
            provenant_cli::Tail::Bytes(bytes) => {
                provenant_cli::run::tail_start_bytes(window, bytes)
            }
            provenant_cli::Tail::Lines(lines) => {
                provenant_cli::run::tail_start_lines(window, lines)
            }
        };
        start += bounded_start;
    }
    let selected_bytes = &text.as_bytes()[start..end];
    let (bytes, excerpt) =
        match provenant_fabric_runtime::coordination::facade::OutputBytes::from_bytes(
            selected_bytes.to_vec(),
        ) {
            provenant_fabric_runtime::coordination::facade::OutputBytes::Utf8(text) => (
                provenant_records::wire::ChunkBytes::Utf8(text.clone()),
                text,
            ),
            provenant_fabric_runtime::coordination::facade::OutputBytes::Base64(text) => (
                provenant_records::wire::ChunkBytes::Base64(text),
                String::from_utf8_lossy(selected_bytes).into_owned(),
            ),
        };
    let state = serde_json::to_value(progress.state)?
        .as_str()
        .unwrap_or("unknown")
        .to_owned();
    let selected = json!({
        "verdict": outputs.verdict,
        "findings": outputs.findings,
        "findings_available": outputs.findings_available,
        "partial": outputs.partial,
        "warnings": outputs.warnings,
        "branch": outputs.branch,
        "commit": outputs.commit,
        "remaining": outputs.remaining,
    });
    let data = provenant_records::wire::OutputData::Composite(Box::new(
        provenant_records::wire::CompositeChunk {
            graph: graph.to_owned(),
            node: progress.id.clone(),
            state,
            part: "result".into(),
            offset: start as u64,
            next_offset: end as u64,
            length: result
                .filter(|result| !result.truncated)
                .map(|_| length as u64),
            eof: result.is_none_or(|_| end == length),
            complete: result.is_none_or(|result| !result.truncated),
            digest: result.map(|result| result.digest.clone()),
            bytes,
            selected,
            tail: None,
            tail_clamped: None,
            to: None,
            written: None,
        },
    ));
    let data = serde_json::to_value(data)?;
    Ok(provenant_cli::run::render_output(
        command.clone(),
        Ok((data, excerpt.to_owned())),
        full,
    ))
}

/// Whether an `output` read still needs the graph's failure rows. A settled
/// lane with a declared result already carries its answer; re-reading stale
/// attempt evidence only adds noise to that page.
fn output_needs_graph_failures(
    attempt_given: bool,
    progress: &provenant_fabric_runtime::graph::NodeProgress,
) -> bool {
    let settled_with_result = progress.outputs.is_some() && progress.state.settled();
    !attempt_given && !settled_with_result
}

/// The failed or blocked node rows under a graph or node target.
fn node_failure_rows(
    factory: &Factory,
    checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint,
    graph: &str,
    node: Option<String>,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let target = GraphTarget {
        graph: graph.to_owned(),
        node,
        attempt: None,
    };
    let mut rows = status_rows(&graph_snapshot(checkpoint), &target);
    enrich_status_rows_within(&mut rows, &factory.context, &factory.clock, cancel, budget)?;
    localise_worktree_actions(factory, &target.graph, &mut rows);
    rows.retain(|row| {
        row["kind"] == "node" && matches!(row["state"].as_str(), Some("failed" | "blocked"))
    });
    Ok(rows)
}

/// An output reply for a one-node target whose node failed before launching
/// any attempt: no output, and the node's failure rows.
fn unlaunched_failure_output(
    command: &provenant_cli::Output,
    factory: &Factory,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<Option<provenant_cli::run::Reply>, Box<dyn std::error::Error>> {
    let Some(checkpoint) = FileJournal::read_status_checkpoint(
        &factory.context.roots.state,
        factory.context.identity.project,
        &command.target.name,
    )?
    else {
        return Ok(None);
    };
    let node = match (&command.target.node, checkpoint.nodes.as_slice()) {
        (Some(node), _) => node.clone(),
        (None, [only]) => only.id.clone(),
        (None, _) => return Ok(None),
    };
    let graph = command.target.name.clone();
    let rows = node_failure_rows(factory, checkpoint, &graph, Some(node), budget, cancel)?;
    if rows.is_empty() {
        return Ok(None);
    }
    let reply = provenant_cli::run::Reply {
        result: provenant_records::CommandResult::new(
            "output",
            provenant_records::Outcome::Ok,
            json!({"graph_id": graph, "outputs": []}),
        ),
        human: "no attempt output yet\n".into(),
        summary: None,
        read_only: true,
    };
    Ok(Some(with_graph_failures(reply, &rows)))
}

fn graph_attempt_output(
    command: provenant_cli::Output,
    reference: &str,
    edge: &provenant_cli::run::Edge<'_>,
    factory: &Factory,
    budget: WaitBudget,
    cancel: &CancelToken,
    full: bool,
) -> Result<provenant_cli::run::Reply, FacadeError> {
    if cancel.is_cancelled() || budget.remaining(factory.clock.mono_now()).as_micros() == 0 {
        return Err(FacadeError::Timeout);
    }
    let (host, attempt) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
    let host: provenant_core::HostId = host.parse().map_err(|_| FacadeError::StateCorrupt)?;
    let attempt: provenant_core::AttemptId =
        attempt.parse().map_err(|_| FacadeError::StateCorrupt)?;
    let mut owner = BinaryRunOwner {
        factory,
        allow_remote: true,
        actor: None,
        chair: None,
    };
    if host == factory.context.identity.host {
        let session = owner.read_session(cancel)?;
        return Ok(provenant_cli::run::output_from_session(
            command,
            edge,
            &session,
            attempt,
            &owner.call(cancel),
            full,
        ));
    }
    let origin = provenant_records::WorkKey::new(
        factory.context.identity.project,
        command.target.name.clone(),
        command
            .target
            .node
            .clone()
            .ok_or(FacadeError::StateCorrupt)?,
        command
            .target
            .attempt
            .ok_or(FacadeError::StateCorrupt)?
            .get(),
    )
    .map_err(|_| FacadeError::StateCorrupt)?;
    Ok(provenant_cli::run::output_from_host(
        command,
        host,
        attempt,
        |input| {
            owner
                .output(input, budget, cancel)
                .and_then(|output| super::peer::checked_output_origin(output, &origin))
                .map_err(|error| {
                    provenant_cli::run::Fail::new(
                        provenant_records::Outcome::Error,
                        provenant_run::ErrorKind::Fabric,
                        error.to_string(),
                    )
                })
        },
        full,
    ))
}

fn output_fail(
    outcome: provenant_records::Outcome,
    code: &str,
    message: &str,
) -> provenant_cli::run::Reply {
    let mut result = provenant_records::CommandResult::new("output", outcome, json!({}));
    result.error = Some(provenant_records::CommandError::new(
        code,
        message,
        None,
        "use a valid, unambiguous target and rerun `provenant output`",
    ));
    provenant_cli::run::Reply {
        result,
        human: String::new(),
        summary: None,
        read_only: true,
    }
}

fn output_error(error: &(dyn std::error::Error + 'static)) -> provenant_cli::run::Reply {
    match command_target_fault(error, None) {
        Some((outcome, code, message)) => output_fail(outcome, code, &message),
        None => output_fail(
            provenant_records::Outcome::Error,
            "graph_target",
            &error.to_string(),
        ),
    }
}

fn with_graph_failures(
    mut reply: provenant_cli::run::Reply,
    rows: &[Value],
) -> provenant_cli::run::Reply {
    if !rows.is_empty() {
        reply.result.data["graph_failures"] = json!(rows);
        let reading = render_graph_status(rows);
        reply.human.push_str(&reading);
        if let Some(summary) = &mut reply.summary {
            summary
                .lines
                .splice(0..0, reading.lines().map(str::to_owned));
        }
        // A graph-failure reading replaces the owner's suggested command: it
        // has its own diagnostics and must not point at a stale next step.
        reply.result.next.clear();
    }
    reply
}

/// Add a current local checkout observation only to CLI readings. Remote and
/// Relay callers retain the owner's path-free diagnostic.
fn localise_worktree_actions(factory: &Factory, graph: &str, rows: &mut [Value]) {
    if !rows.iter().any(|row| {
        row["kind"] == "node" && row["state"] == "failed" && row["cause_code"] != "writer_lock_busy"
    }) {
        return;
    }
    let Ok(Some(checkpoint)) = FileJournal::read_status_checkpoint(
        &factory.context.roots.state,
        factory.context.identity.project,
        graph,
    ) else {
        return;
    };
    // Rows expose the owner code; the checkpoint retains the binding phase.
    let binding_failed = |row: &Value| {
        checkpoint.nodes.iter().any(|node| {
            Some(node.id.as_str()) == row["node_id"].as_str()
                && node.advance_error_code.as_deref() == Some("writer_binding_unavailable")
                && node.advance_owner_code.as_deref() != Some("writer_lock_busy")
        })
    };
    if !rows.iter().any(binding_failed) {
        return;
    }
    let Ok(Some(execution)) = FileJournal::execution(
        &factory.context.roots.state,
        factory.context.identity.project,
        graph,
    ) else {
        return;
    };
    for row in rows
        .iter_mut()
        .filter(|row| row["kind"] == "node" && row["state"] == "failed" && binding_failed(row))
    {
        let Some(node) = execution
            .document
            .nodes
            .iter()
            .find(|node| Some(node.id.as_str()) == row["node_id"].as_str())
        else {
            continue;
        };
        let Some(attempt) = node
            .attempt
            .as_ref()
            .filter(|attempt| attempt.host.is_none())
        else {
            continue;
        };
        let Some(branch) = attempt.new_worktree.as_ref().map(|new| new.branch.as_str()) else {
            continue;
        };
        if let Some(action) = crate::graph_interface::local_branch_action(factory, branch) {
            row["next"] = json!(action);
        }
    }
}

/// The read-only graph context of `edge`, or `None` where no project graph
/// state exists yet.
fn read_context(edge: &provenant_cli::run::Edge<'_>) -> Result<Option<Context>, FacadeError> {
    match Context::open_read_only(edge.roots.clone(), edge.home, edge.cwd) {
        Ok(context) => Ok(Some(context)),
        Err(provenant_cli::fabric::EdgeError::Invalid) => Ok(None),
        Err(_) => Err(FacadeError::io("open graph context")),
    }
}

#[cfg(test)]
#[path = "unified_tests.rs"]
#[allow(
    dead_code,
    unused_imports,
    reason = "the custom worktree driver omits libtest functions"
)]
mod tests;
