//! Read and control graph targets through the public command surface.
use super::*;
use provenant_cli::target::{ResolveError, Resolved, Want};
use provenant_fabric_runtime::graph::{AttemptState, FileJournal, GraphRun, JournalError};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub(crate) struct GraphTarget {
    pub graph: String,
    pub node: Option<String>,
    /// `HOST/ATTEMPT` of the one selected attempt.
    pub attempt: Option<String>,
}

/// Resolve `target` by reading only its own graph's checkpoint.
pub(crate) fn resolve(
    root: &provenant_core::StateRoot,
    project: provenant_core::ProjectId,
    target: &provenant_cli::Target,
    want: Want,
) -> Result<Resolved, ResolveError<JournalError>> {
    provenant_cli::target::resolve(target, want, |name| {
        FileJournal::read_status_checkpoint(root, project, name)
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
            Err(ResolveError::NotFound(_)) => Ok(None),
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
            Err(provenant_fabric_runtime::graph::JournalError::Filesystem(
                provenant_os::fs::FsError::LockTimeout { .. },
            )) => {
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
        .map_err(|_| FacadeError::InvalidInput { detail: None })?;
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
            first_error.get_or_insert(FacadeError::InvalidInput { detail: None });
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
    let reason = reason.map(|text| provenant_fabric_runtime::hosts::plain_text(text, 1024));
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
            rows.push(row(
                format!("{}/{}", target.graph, node.id),
                "node",
                target,
                Some(&node.id),
                None,
                json!(node.state),
                node.advance_error
                    .as_deref()
                    .or(node.skip_reason.as_deref()),
            ));
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
            "work_key",
            "branch",
            "pr",
            "ci",
            "result_path",
            "provider_error",
            "handoff",
            "open_asks",
            "result_summary",
            "evidence_error",
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
            row["branch"] = json!(
                node.and_then(|node| node.outputs.as_ref())
                    .and_then(|out| out.branch.as_ref())
                    .or_else(|| node
                        .and_then(|node| node.land.as_ref())
                        .map(|land| &land.branch))
            );
            row["pr"] = node
                .and_then(|node| node.land.as_ref())
                .and_then(|land| land.pull_request)
                .map_or_else(
                    || json!(run.checkpoint.pull_request),
                    |number| json!(number),
                );
            row["ci"] = json!(node.and_then(|node| node.ci.as_ref()));
            row["handoff"] = json!(node.and_then(|node| node.handoff.as_ref()));
            row["result_summary"] = json!(
                node.and_then(|node| node.outputs.as_ref())
                    .and_then(|out| out.result.as_ref())
                    .map(|result| provenant_fabric_runtime::hosts::plain_text(
                        &result.excerpt,
                        200
                    ))
            );
            row["result_path"] = json!(
                node.and_then(|node| node.outputs.as_ref())
                    .and_then(|out| out.result.as_ref())
                    .and_then(|result| result.path.as_ref())
            );
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
    let next = graphs
        .iter()
        .chain(lines.iter().map(|(row, _)| row))
        .min_by_key(|row| priority(row))
        .map(|row| plain(&row["next"]))
        .unwrap_or_default();
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
            continue;
        }
        let name = attempt.map_or_else(|| plain(&node["id"]), |attempt| plain(&attempt["id"]));
        let model = attempt.map_or_else(|| "-".to_owned(), row_model);
        human.push_str(&status_line(&name, node, Some(&model)));
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
    if !next.is_empty() {
        human.push_str(&provenant_cli::bounded_text(
            &format!("next: {next}"),
            provenant_cli::limits::SUMMARY_ROW_BYTES,
        ));
        human.push('\n');
    }
    human
}

fn plain(value: &Value) -> String {
    provenant_fabric_runtime::hosts::plain_text(value.as_str().unwrap_or(""), 1024)
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
/// its note when there is one. At most `SUMMARY_ROW_BYTES`.
pub(crate) fn status_line(name: &str, row: &Value, model: Option<&str>) -> String {
    let mut line = format!("{name} {}", plain(&row["state"]));
    if let Some(model) = model {
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
        line.push_str(&format!(": {}", plain(note)));
    }
    let mut line = provenant_cli::bounded_text(&line, provenant_cli::limits::SUMMARY_ROW_BYTES);
    line.push('\n');
    line
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

/// Enrich graph attempt rows from identity-checked host records within the
/// caller's budget. The run owner retains typed terminal causes.
pub(crate) fn enrich_status_rows_within(
    rows: &mut [Value],
    context: &Context,
    clock: &SystemClock,
    cancel: &CancelToken,
    peer_budget: WaitBudget,
) -> Result<(), FacadeError> {
    status_keys(rows, context.identity.project)?;
    let execution = provenant_run::ExecutionContext {
        state_root: context.roots.state.clone(),
        project_id: context.identity.project,
        actor_id: context.identity.inbox.actor,
        read_prompts: false,
    };
    let call = provenant_run::Request { clock, cancel };
    let store = provenant_run::RunStore::open_read_only(&execution, &call).map_err(run_error)?;
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
        let (record, waits) = if host == context.identity.host {
            let Some(store) = &store else {
                continue;
            };
            match store.record(attempt, &call) {
                Ok(record) => {
                    let waits = store.lane_waits(std::slice::from_ref(&record), &call);
                    (record, waits)
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
                },
                peer_budget,
                cancel,
            );
            let response = match response {
                Ok(response) => response,
                Err(failure) => {
                    row["evidence_error"] = json!(provenant_fabric_runtime::hosts::plain_text(
                        &failure.error.to_string(),
                        200
                    ));
                    continue;
                }
            };
            match response {
                PeerResult::Run { data, .. } => match *data {
                    RunResult::Status {
                        attempt: record,
                        extra,
                    } if record.attempt_id == attempt => {
                        let waits = extra
                            .get("lane_waits")
                            .cloned()
                            .map(serde_json::from_value)
                            .transpose()
                            .map_err(|_| FacadeError::StateCorrupt)?
                            .unwrap_or_default();
                        (record, waits)
                    }
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
        let peer_graph = record.extra.get("peer_graph");
        let origin_matches = host != context.identity.host
            && peer_graph.is_some_and(|graph| {
                graph["host"] == serde_json::json!(host)
                    && graph["graph_id"].as_str().is_some_and(|id| !id.is_empty())
            })
            && super::peer::peer_origin_matches(&record.extra, &expected);
        if (host != context.identity.host && !origin_matches)
            || (host == context.identity.host && record.key != expected)
        {
            return Err(FacadeError::StateCorrupt);
        }
        if retain_collected_peer_row(row, &record, host, context.identity.host) {
            continue;
        }
        for field in ["peer_graph", "peer_subtree"] {
            if let Some(value) = record.extra.get(field) {
                row[field] = value.clone();
            }
        }
        let mut projected = provenant_cli::run::project_lane(
            &record,
            &waits,
            context.roots.state.as_path(),
            clock.wall_now().ok(),
            host == context.identity.host,
        );
        if host == context.identity.host
            && let Some(store) = &store
        {
            let evidence = provenant_cli::run::status_evidence(store, &execution, &record, &call);
            projected.provider_error = evidence["provider_error"].as_str().map(str::to_owned);
            projected.handoff =
                (!evidence["handoff"].is_null()).then(|| evidence["handoff"].clone());
        }
        let fields = serde_json::to_value(projected).map_err(|_| FacadeError::StateCorrupt)?;
        let reference = row["attempt_id"].clone();
        let graph_state = row["state"].clone();
        let attempt_number = row["attempt_number"].clone();
        let result_path = row["result_path"].clone();
        row.as_object_mut()
            .ok_or(FacadeError::StateCorrupt)?
            .extend(fields.as_object().ok_or(FacadeError::StateCorrupt)?.clone());
        row["attempt_id"] = reference;
        row["attempt_number"] = attempt_number;
        if row["result_path"].is_null() {
            row["result_path"] = result_path;
        }
        row["graph_state"] = graph_state;
        project_attempt_failure(row);
    }
    project_node_waits(rows);
    project_node_failures(rows);
    for row in rows.iter_mut() {
        if let Some(object) = row.as_object_mut() {
            object.remove("latest_attempt");
        }
    }
    Ok(())
}

fn project_attempt_failure(row: &mut Value) {
    if row["terminal_cause"].is_string() {
        row["cause_code"] = row["terminal_cause"].clone();
        if row["reason"].is_string() {
            row["next"] = row["reason"].clone();
        }
    }
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
                    row["terminal_cause"].clone(),
                    row["reason"].clone(),
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
        if let Some((state, cause, reason)) = failures.get(&key)
            && state == "terminal"
        {
            if !cause.is_null() {
                row["cause_code"] = cause.clone();
            }
            if !reason.is_null() {
                row["reason"] = reason.clone();
                row["next"] = reason.clone();
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
    let Some(graph) = record.extra.get("peer_graph") else {
        return false;
    };
    if peer == local
        || record.extra.get("peer_graph_collected") != Some(&json!(true))
        || !matches!(
            record.state,
            provenant_records::run::AttemptState::Terminal { .. }
        )
        || graph["host"] != json!(peer)
        || graph["graph_id"]
            .as_str()
            .is_none_or(|id| !id.starts_with("peer-"))
        || record
            .context
            .as_ref()
            .and_then(|context| context.graph_id.as_deref())
            != graph["graph_id"].as_str()
    {
        return false;
    }
    row["peer_graph"] = graph.clone();
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

/// A target that names nothing, or names it ambiguously, as its envelope
/// reports it. It carries its own outcome so the caller does not re-derive it.
#[derive(Debug)]
pub(crate) struct TargetFault {
    pub outcome: provenant_records::Outcome,
    pub code: &'static str,
    pub message: String,
}

impl std::fmt::Display for TargetFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for TargetFault {}

pub(crate) fn target_fault(error: ResolveError<JournalError>) -> TargetFault {
    let (outcome, code) = match &error {
        ResolveError::NotFound(_) => (provenant_records::Outcome::Invalid, "NOT_FOUND"),
        ResolveError::Ambiguous { .. } => (provenant_records::Outcome::Invalid, "AMBIGUOUS_TARGET"),
        ResolveError::Read(_) => (provenant_records::Outcome::Error, "GRAPH_READ"),
    };
    TargetFault {
        outcome,
        code,
        message: error.to_string(),
    }
}

fn is_target_not_found(error: &(dyn std::error::Error + 'static)) -> bool {
    if matches!(
        error.downcast_ref::<FacadeError>(),
        Some(FacadeError::NotFound)
    ) {
        return true;
    }
    if matches!(
        error.downcast_ref::<FacadeError>(),
        Some(FacadeError::GraphRelayRefused { refusal_code, .. })
            if refusal_code == "NOT_FOUND"
    ) {
        return true;
    }
    if let Some(service) = error.downcast_ref::<provenant_fabric_runtime::relay::ServiceError>() {
        return match service {
            provenant_fabric_runtime::relay::ServiceError::Facade(error) => {
                matches!(error, FacadeError::NotFound)
                    || matches!(
                        error,
                        FacadeError::GraphRelayRefused { refusal_code, .. }
                            if refusal_code == "NOT_FOUND"
                    )
            }
            provenant_fabric_runtime::relay::ServiceError::Mutation { source, .. } => {
                is_target_not_found(source.as_ref())
            }
            provenant_fabric_runtime::relay::ServiceError::Cli { code, .. } => code == "NOT_FOUND",
            _ => false,
        };
    }
    false
}

fn command_target_fault(
    error: &(dyn std::error::Error + 'static),
    target: Option<&str>,
) -> Option<(provenant_records::Outcome, &'static str, String)> {
    if let Some(fault) = error.downcast_ref::<TargetFault>() {
        return Some((fault.outcome, fault.code, fault.message.clone()));
    }
    is_target_not_found(error).then(|| {
        let message = target.map_or_else(
            || format!("NOT_FOUND: {error}"),
            |target| format!("NOT_FOUND: {target}"),
        );
        (provenant_records::Outcome::Invalid, "NOT_FOUND", message)
    })
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

fn resolve_checkpoint(
    root: &provenant_core::StateRoot,
    project: provenant_core::ProjectId,
    target: &provenant_cli::Target,
    want: Want,
) -> Result<(Resolved, provenant_fabric_runtime::graph::GraphCheckpoint), TargetFault> {
    let mut read = None;
    let resolved = provenant_cli::target::resolve(target, want, |name| {
        let checkpoint = FileJournal::read_status_checkpoint(root, project, name)?;
        read.clone_from(&checkpoint);
        Ok(checkpoint)
    })
    .map_err(target_fault)?;
    let checkpoint =
        read.ok_or_else(|| target_fault(ResolveError::NotFound(target.to_string())))?;
    Ok((resolved, checkpoint))
}

pub(crate) fn command_fail(
    name: &str,
    error: &(dyn std::error::Error + 'static),
    globals: &provenant_cli::Globals,
) -> u8 {
    match command_target_fault(error, None) {
        Some((outcome, code, message)) => crate::fail(name, outcome, code, &message, globals),
        None => crate::fail(
            name,
            provenant_records::Outcome::Error,
            "graph_target",
            &error.to_string(),
            globals,
        ),
    }
}

pub(crate) enum GraphCommand<'a> {
    Status,
    Stop {
        reason: Option<&'a str>,
    },
    Retry {
        extend: &'a [String],
        brief: Option<&'a str>,
        brief_file: Option<&'a std::path::Path>,
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
    let result = (|| -> Result<(Value, String), Box<dyn std::error::Error>> {
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
                        Box::new(target_fault(ResolveError::NotFound(target.to_string())))
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
        if let Some(factory) = factory.as_ref().filter(|factory| !factory.context.in_lane) {
            for target in targets {
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
        for (selector, graph) in selectors {
            let mut input = json!({"op":name,"id":selector});
            match &command {
                GraphCommand::Status => {}
                GraphCommand::Stop { reason } => {
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
            let Some(factory) = &factory else {
                let mut data = lane_graph_read(input)?;
                let rows = data["rows"].as_array().ok_or(FacadeError::StateCorrupt)?;
                human.push_str(&render_graph_status(rows));
                data["digests"] = json!(super::graph_status::node_digests(rows));
                documents.push(data);
                continue;
            };
            let budget = WaitBudget::cli(
                factory.clock.mono_now(),
                factory.context.config.graph.launch_wait_seconds,
            )?;
            let mut data = factory.graph_document(input, budget, &cancel).map_err(|error| {
                if matches!(command, GraphCommand::Retry { .. })
                    && matches!(error, FacadeError::InvalidInput { detail: Some(InvalidInputDetail::GraphReason(_)) } | FacadeError::GraphRelayRefused { detail: Some(InvalidInputDetail::GraphReason(_)), .. })
                {
                    FacadeError::invalid(InvalidInputDetail::GraphRelayCause(
                        provenant_fabric_runtime::coordination::facade::GraphRelayCause::RetryInvalidConfiguration,
                    ))
                } else {
                    error
                }
            })?;
            if let (Some(rows), Some(graph)) = (data["rows"].as_array_mut(), graph)
                && !factory.context.in_lane
            {
                localise_worktree_actions(factory, graph, rows);
            }
            let rows = data["rows"].as_array().ok_or(FacadeError::StateCorrupt)?;
            human.push_str(&render_graph_status(rows));
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
        Ok((data, human))
    })();
    match result {
        Ok((data, human)) => crate::emit(
            &provenant_records::CommandResult::new(name, provenant_records::Outcome::Ok, data),
            globals,
            &human,
            false,
        ),
        Err(error) => command_fail(name, error.as_ref(), globals),
    }
}

/// Queue steering for one live attempt. Steering never reaches a settled
/// attempt, and its text is bounded at 4 KiB.
pub(crate) fn unified_graph_steer(
    args: &provenant_cli::inbox::Steer,
    globals: &provenant_cli::Globals,
    workers: &'static dyn WorkerObserver,
) -> u8 {
    enum SteerResult {
        Queued(Value),
        Refused(&'static str),
        Unsupported(provenant_records::control::ControlReceipt),
    }
    let result = (|| -> Result<SteerResult, Box<dyn std::error::Error>> {
        let factory = factory(globals, workers)?;
        let (resolved, checkpoint) =
            resolve_with_checkpoint(&factory, &args.target, Want::Attempt { settled: false })?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(
            factory.clock.mono_now(),
            factory.context.config.graph.launch_wait_seconds,
        )?;
        let target = match super::steer::verified_target(
            &factory,
            &resolved,
            &checkpoint,
            budget,
            &cancel,
        ) {
            Ok(Some(target)) => target,
            Ok(None) | Err(_) => {
                return Ok(SteerResult::Refused(
                    "steering requires one verified, accepted, current live attempt",
                ));
            }
        };
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
        let mut facade = BoundFacade { factory: &factory };
        let (host, id) = target
            .reference
            .split_once('/')
            .ok_or(FacadeError::StateCorrupt)?;
        let operation_id = match args.operation_id.as_ref() {
            Some(operation) => {
                let digest = provenant_records::CanonicalV1::record_digest(
                    "graph-steer",
                    1,
                    &(operation, &target.reference),
                )
                .map_err(|_| FacadeError::StateCorrupt)?;
                Some(provenant_core::OperationId::parse(&format!(
                    "steer-{}",
                    provenant_core::hex(digest.as_bytes())
                ))?)
            }
            None if target.target.host != Some(factory.context.identity.host) => {
                let bytes = provenant_os::fs::entropy_128()?;
                Some(provenant_core::OperationId::parse(&format!(
                    "steer-{}",
                    provenant_core::hex(&bytes)
                ))?)
            }
            None => None,
        };
        let extra = if target.target.host != Some(factory.context.identity.host) {
            [
                ("graph_steer".into(), json!(true)),
                (
                    "graph_launch_actor".into(),
                    json!(target.launch_actor.to_string()),
                ),
                (
                    "graph_launch_operation".into(),
                    serde_json::to_value(&target.launch_operation)?,
                ),
                (
                    "graph_origin_work_key".into(),
                    serde_json::to_value(&target.work_key)?,
                ),
            ]
            .into()
        } else {
            Default::default()
        };
        let input = provenant_fabric_runtime::inbox::InboxInput::Steer {
            extra,
            attempt: id.parse()?,
            text,
            operation_id,
        };
        let result = match facade.inbox_owner(input, Some(host.into()), budget, &cancel) {
            Ok(result) => result,
            Err(provenant_fabric_runtime::relay::ServiceError::InboxOwner { .. }) => {
                return Ok(SteerResult::Refused(
                    "steering requires one verified, accepted, current live attempt",
                ));
            }
            Err(error) => return Err(Box::new(error)),
        };
        match result {
            provenant_fabric_runtime::inbox::InboxResult::Control { receipt }
                if receipt.state == provenant_records::control::ControlState::Unsupported =>
            {
                Ok(SteerResult::Unsupported(receipt))
            }
            provenant_fabric_runtime::inbox::InboxResult::Control { receipt } => {
                Ok(SteerResult::Queued(json!({"receipts":[receipt]})))
            }
            _ => Err(FacadeError::StateCorrupt.into()),
        }
    })();
    match result {
        Ok(SteerResult::Queued(data)) => crate::emit(
            &provenant_records::CommandResult::new("steer", provenant_records::Outcome::Ok, data),
            globals,
            "steering queued\n",
            false,
        ),
        Ok(SteerResult::Refused(message)) => crate::fail(
            "steer",
            provenant_records::Outcome::Refused,
            "steer_target_refused",
            message,
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
}

/// Every unsettled graph, always; then settled graphs newest first, within
/// the last day unless `all`, up to `limit` rows in all. `all` pages through
/// the settled rows with `cursor`, an offset into them.
pub(crate) fn select_listing(
    graphs: Vec<ListedGraph>,
    now_micros: i64,
    all: bool,
    limit: usize,
    cursor: Option<u64>,
) -> Listing {
    let window = provenant_cli::limits::LISTING_SETTLED_SECONDS * 1_000_000;
    let (mut active, mut settled): (Vec<_>, Vec<_>) =
        graphs.into_iter().partition(|graph| !graph.settled);
    active.sort_by(|a, b| {
        b.changed_micros
            .cmp(&a.changed_micros)
            .then(a.name.cmp(&b.name))
    });
    settled.sort_by(|a, b| {
        b.changed_micros
            .cmp(&a.changed_micros)
            .then(a.name.cmp(&b.name))
    });
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
    // Keep settled pages advancing even when active work alone fills the
    // normal display allowance. Active graphs still appear on every page.
    let room = if all {
        limit.max(1)
    } else {
        limit.saturating_sub(active.len())
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
    let mut rows = active;
    rows.extend(shown);
    Listing {
        rows,
        omitted,
        next,
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
                });
            }
            Err(error) => return Err(error),
        };
        let root = &factory.context.roots.state;
        let project = factory.context.identity.project;
        let mut graphs = Vec::new();
        for name in FileJournal::graph_ids(root, project)? {
            match FileJournal::read_status_checkpoint(root, project, &name) {
                Ok(Some(checkpoint)) => {
                    use provenant_fabric_runtime::graph::{GraphOutcome, GraphStatus};
                    let run = graph_snapshot(checkpoint);
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
        let mut listing = select_listing(
            graphs,
            now,
            command.all,
            usize::try_from(command.limit).unwrap_or(usize::MAX),
            command.cursor,
        );
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 55)?;
        for graph in &mut listing.rows {
            let Some(checkpoint) = FileJournal::read_status_checkpoint(root, project, &graph.name)
                .ok()
                .flatten()
            else {
                continue;
            };
            let target = GraphTarget {
                graph: graph.name.clone(),
                node: None,
                attempt: None,
            };
            let mut rows = status_rows(&graph_snapshot(checkpoint), &target);
            // Unavailable attempt/peer evidence must not hide the graph. The
            // retained projection still explicitly marks external facts stale.
            if let Err(error) = enrich_status_rows_within(
                &mut rows,
                &factory.context,
                &factory.clock,
                &cancel,
                budget,
            ) {
                graph.reason = Some(provenant_fabric_runtime::hosts::plain_text(
                    &format!(
                        "{}; status evidence unavailable: {error}",
                        graph.reason.as_deref().unwrap_or("")
                    ),
                    1024,
                ));
            }
            graph.digests = super::graph_status::node_digests(&rows);
        }
        Ok(listing)
    })();
    match result {
        Ok(listing) => {
            let now = SystemClock::new()
                .wall_now()
                .map_or(i64::MAX, provenant_core::WallTime::as_micros);
            let mut human = String::from("NAME STATE TEMPLATE NODES AGE\n");
            for graph in &listing.rows {
                let age = graph
                    .changed_micros
                    .map_or_else(|| "-".to_owned(), |changed| age_text(now - changed));
                human.push_str(&format!(
                    "{} {} {} {} {age}\n",
                    graph.name,
                    graph.state,
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
                human = "no graphs\n".into();
            }
            for (state, count) in &listing.omitted {
                human.push_str(&format!(
                    "{count} {state} graphs not shown; --all lists them\n"
                ));
            }
            if let Some(cursor) = listing.next {
                human.push_str(&format!(
                    "next: provenant status --all --limit {} --cursor {cursor}\n",
                    command.limit
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
            crate::emit(
                &provenant_records::CommandResult::new(
                    "status",
                    provenant_records::Outcome::Ok,
                    json!({"graphs":rows,"omitted":listing.omitted,"next_cursor":listing.next}),
                ),
                globals,
                &human,
                true,
            )
        }
        Err(error) => command_fail("status", error.as_ref(), globals),
    }
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
        let factory = read_factory(globals, workers)?;
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
                    Box::new(target_fault(ResolveError::NotFound(
                        command.target.to_string(),
                    )))
                } else {
                    error
                }
            })?;
            let budget = WaitBudget::cli(
                factory.clock.mono_now(),
                factory.context.config.graph.launch_wait_seconds,
            )?;
            host_graph_output(command, edge, &factory, budget, &CancelToken::new())
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
    let result = (|| -> Result<provenant_cli::run::Reply, Box<dyn std::error::Error>> {
        if command.path {
            return Err("lane reads return content; omit --path".into());
        }
        let input = json!({"op":"output", "id":command.target.to_string(),
            "render":true,"part":part,"offset":command.offset,"tail":command.tail,"max_bytes":command.max_bytes});
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
) -> Result<provenant_cli::run::Reply, Box<dyn std::error::Error>> {
    let (resolved, checkpoint) =
        match resolve_with_checkpoint(factory, &command.target, Want::Attempt { settled: true }) {
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
    let Some(reference) = resolved.attempt_ref.clone() else {
        let fault = TargetFault {
            outcome: provenant_records::Outcome::Invalid,
            code: "NOT_FOUND",
            message: format!("NOT_FOUND: {} has not launched", resolved.target()),
        };
        if command.target.attempt.is_none()
            && let Some(reply) = unlaunched_failure_output(command, factory, budget, cancel)?
        {
            return Ok(reply);
        }
        return Err(fault.into());
    };
    let failure_rows = if command.target.attempt.is_none() {
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
    let reply = graph_attempt_output(selected, &reference, edge, factory, budget, cancel)?;
    Ok(with_graph_failures(reply, &failure_rows))
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
    ))
}

fn output_fail(
    outcome: provenant_records::Outcome,
    code: &str,
    message: &str,
) -> provenant_cli::run::Reply {
    let mut result = provenant_records::CommandResult::new("output", outcome, json!({}));
    result.error = Some(provenant_records::CommandError {
        code: code.into(),
        message: message.into(),
        cause: None,
        repair: None,
        diagnostic: None,
    });
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
            summary.next = None;
        }
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

/// Resolve every target before a wait starts. Inside a lane the host performs
/// one status read for validation and the wait's initial snapshot.
pub(crate) fn check_targets(
    targets: &[provenant_cli::Target],
    edge: &provenant_cli::run::Edge<'_>,
    deadline: std::time::Instant,
    cancel: &CancelToken,
    mut observed: impl FnMut(&provenant_cli::Target, Vec<Value>),
) -> Result<(), Box<dyn std::error::Error>> {
    if targets.is_empty() {
        return Ok(());
    }
    wait_remaining(deadline, cancel)?;
    if provenant_cli::fabric::in_lane() {
        for target in targets {
            let left = wait_remaining(deadline, cancel)?;
            let operation = provenant_fabric_runtime::graph::relay::RelayOp::Read {
                input: json!({"op":"status","id":target.to_string()}),
            };
            let value =
                match provenant_cli::fabric::graph_relay::from_lane(&operation, cancel, left) {
                    Ok(value) => value,
                    Err(error) => {
                        let error = super::interface::relay_error(error);
                        if is_target_not_found(&error) {
                            return Err(Box::new(target_fault(ResolveError::NotFound(
                                target.to_string(),
                            ))));
                        }
                        return Err(Box::new(error));
                    }
                };
            wait_remaining(deadline, cancel)?;
            observed(
                target,
                value["rows"]
                    .as_array()
                    .ok_or(FacadeError::StateCorrupt)?
                    .clone(),
            );
        }
        return Ok(());
    }
    let context = read_context(edge)?;
    for target in targets {
        wait_remaining(deadline, cancel)?;
        let Some(context) = &context else {
            return Err(Box::new(target_fault(ResolveError::NotFound(
                target.to_string(),
            ))));
        };
        resolve(
            &context.roots.state,
            context.identity.project,
            target,
            Want::Work,
        )
        .map_err(target_fault)?;
        wait_remaining(deadline, cancel)?;
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct WaitGraph {
    pub target: provenant_cli::Target,
    pub error: Option<String>,
    pub rows: Vec<Value>,
}

/// The project's running graphs: what a wait with no target waits for. A
/// paused graph waits on its owner, so it is left out.
pub(crate) fn unsettled_graphs(
    edge: &provenant_cli::run::Edge<'_>,
    deadline: std::time::Instant,
    cancel: &CancelToken,
) -> Result<Vec<WaitGraph>, Box<dyn std::error::Error>> {
    wait_remaining(deadline, cancel)?;
    let context = read_context(edge)?;
    wait_remaining(deadline, cancel)?;
    let Some(context) = context else {
        return Ok(Vec::new());
    };
    let root = &context.roots.state;
    let project = context.identity.project;
    let mut graphs = Vec::new();
    for name in FileJournal::graph_ids(root, project)? {
        wait_remaining(deadline, cancel)?;
        let (error, rows) = match FileJournal::read_status_checkpoint(root, project, &name) {
            Ok(Some(checkpoint)) => {
                let run = graph_snapshot(checkpoint);
                if run.status != provenant_fabric_runtime::graph::GraphStatus::Pending {
                    continue;
                }
                let target = GraphTarget {
                    graph: name.clone(),
                    node: None,
                    attempt: None,
                };
                let mut rows = status_rows(&run, &target);
                status_keys(&mut rows, project)?;
                (None, rows)
            }
            Ok(_) => continue,
            Err(error) => (Some(error.to_string()), Vec::new()),
        };
        graphs.push(WaitGraph {
            target: provenant_cli::Target {
                name,
                node: None,
                attempt: None,
            },
            error,
            rows,
        });
    }
    wait_remaining(deadline, cancel)?;
    Ok(graphs)
}

/// Every wait phase spends the same wall-clock deadline and cancellation token.
#[allow(
    clippy::disallowed_methods,
    reason = "wait transport owns its real-time deadline"
)]
fn wait_remaining(
    deadline: std::time::Instant,
    cancel: &CancelToken,
) -> Result<std::time::Duration, FacadeError> {
    let left = deadline.saturating_duration_since(std::time::Instant::now());
    if cancel.is_cancelled() {
        Err(FacadeError::Cancelled)
    } else if left.is_zero() {
        Err(FacadeError::Timeout)
    } else {
        Ok(left)
    }
}

/// The status rows one target names, read afresh: the wait verb's reader.
/// `session` is opened on first use; a caller that polls keeps it, because
/// opening resolves the repository and runs git.
pub(crate) fn read_status_rows(
    target: &provenant_cli::Target,
    edge: &provenant_cli::run::Edge<'_>,
    session: &mut Option<Context>,
    deadline: std::time::Instant,
    cancel: &CancelToken,
) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let left = wait_remaining(deadline, cancel)?;
    if provenant_cli::fabric::in_lane() {
        let value = provenant_cli::fabric::graph_relay::from_lane(
            &provenant_fabric_runtime::graph::relay::RelayOp::Read {
                input: json!({"op":"status","id":target.to_string()}),
            },
            cancel,
            left,
        )
        .map_err(super::interface::relay_error)?;
        wait_remaining(deadline, cancel)?;
        return Ok(value["rows"]
            .as_array()
            .ok_or(FacadeError::StateCorrupt)?
            .clone());
    }
    let context = match session {
        Some(context) => context,
        None => session.insert(
            read_context(edge)?
                .ok_or_else(|| target_fault(ResolveError::NotFound(target.to_string())))?,
        ),
    };
    let (resolved, checkpoint) = resolve_checkpoint(
        &context.roots.state,
        context.identity.project,
        target,
        Want::Work,
    )?;
    let target = GraphTarget {
        graph: resolved.graph,
        node: resolved.node,
        attempt: resolved.attempt_ref,
    };
    let mut rows = status_rows(&graph_snapshot(checkpoint), &target);
    let clock = SystemClock::new();
    let left = wait_remaining(deadline, cancel)?;
    let budget = WaitBudget::up_to(
        clock.mono_now(),
        provenant_core::DurationMicros::from_micros(
            u64::try_from(left.as_micros()).unwrap_or(u64::MAX),
        ),
    );
    enrich_status_rows_within(&mut rows, context, &clock, cancel, budget)?;
    wait_remaining(deadline, cancel)?;
    Ok(rows)
}

fn missing_graph_context(error: &(dyn std::error::Error + 'static)) -> bool {
    matches!(
        error.downcast_ref::<provenant_cli::fabric::EdgeError>(),
        Some(provenant_cli::fabric::EdgeError::Invalid)
    )
}

// Stdin's bounded I/O API takes a real-time deadline at this transport edge.
#[allow(
    clippy::disallowed_methods,
    reason = "sample the real-time stdin transport deadline"
)]
/// The relay has already bound this input to the calling attempt's graph.
pub(crate) fn read_lane_graph(
    holder: &provenant_run::HolderContext,
    input: Value,
    cancel: &CancelToken,
    within: std::time::Duration,
    workers: &'static dyn WorkerObserver,
) -> Result<Value, ServiceError> {
    let read = || -> Result<Value, Box<dyn std::error::Error>> {
        if cancel.is_cancelled() {
            return Err(FacadeError::Cancelled.into());
        }
        if within <= std::time::Duration::from_millis(100) {
            return Err(FacadeError::Timeout.into());
        }

        let clock = SystemClock::new();
        let budget = provenant_fabric_runtime::relay::host_budget(&clock, within)?;
        let launch = holder
            .launch
            .as_ref()
            .ok_or("host launch context missing")?;
        let roots = crate::roots::child(
            &holder.roots.home,
            &holder.roots.checkout,
            launch.configuration_root.clone(),
        )?;
        let graph_directory = launch.graph_directory.as_deref().unwrap_or(&launch.cwd);
        let context = Context::open_inspection(roots, &holder.roots.home, graph_directory)?;
        if context.identity.project != holder.project_id {
            return Err("host graph project mismatch".into());
        }
        let factory = Factory {
            project_attribution: false,
            context,
            clock: SystemClock::new(),
            backoff: ThreadPause,
            executor: SystemExecutor::new(4)?,
            home: holder.roots.home.clone(),
            cwd: graph_directory.to_path_buf(),
            assets: holder.roots.checkout.clone(),
            host_launch: Some(launch.clone()),
            workers,
        };
        if cancel.is_cancelled() {
            return Err(FacadeError::Cancelled.into());
        }
        if budget.remaining(factory.clock.mono_now()).as_micros() == 0 {
            return Err(FacadeError::Timeout.into());
        }
        if input["op"] != "output" || input["trace"] == true {
            return Ok(factory.graph_document(input.clone(), budget, cancel)?);
        }
        let part = match input["part"].as_str().unwrap_or("result") {
            "result" => provenant_cli::Part::Result,
            "prompt" => provenant_cli::Part::Prompt,
            "stderr" => provenant_cli::Part::Stderr,
            "events" => provenant_cli::Part::Events,
            "receipt" => provenant_cli::Part::Receipt,
            "transcript" => provenant_cli::Part::Transcript,
            _ => return Err(FacadeError::invalid(InvalidInputDetail::Field("part".into())).into()),
        };
        // A malformed target is the caller's input, not a host I/O failure.
        let target = input["id"]
            .as_str()
            .and_then(|id| id.parse().ok())
            .ok_or_else(|| FacadeError::invalid(InvalidInputDetail::Field("id".into())))?;
        let command = provenant_cli::Output {
            target,
            part,
            to: None,
            path: false,
            offset: input["offset"].as_u64().unwrap_or(0),
            tail: input["tail"].as_u64(),
            max_bytes: input["max_bytes"].as_u64().unwrap_or(65536).clamp(1, 65536),
        };
        let edge = provenant_cli::run::Edge {
            roots: &factory.context.roots,
            home: &factory.home,
            assets: &factory.assets,
            cwd: &factory.cwd,
        };
        let reply = host_graph_output(&command, &edge, &factory, budget, cancel)?;
        if input["render"] == true {
            Ok(json!({"result":reply.result,"human":reply.human}))
        } else {
            Ok(reply.result.data)
        }
    };
    read().map_err(|error| match error.downcast::<FacadeError>() {
        Ok(error) => ServiceError::Facade(*error),
        Err(error) => match error.downcast::<TargetFault>() {
            Ok(fault) if fault.code == "NOT_FOUND" => ServiceError::Facade(FacadeError::NotFound),
            Ok(_) | Err(_) => ServiceError::Facade(FacadeError::io("read calling lane graph")),
        },
    })
}

#[cfg(test)]
#[allow(
    dead_code,
    unused_imports,
    reason = "the custom worktree driver omits libtest functions"
)]
mod tests {
    use super::*;
    use provenant_fabric_runtime::graph::NodeState;
    use provenant_fabric_runtime::graph::{
        GraphCheckpoint, GraphDocument, GraphExecution, GraphJournal,
    };

    #[test]
    fn retry_brief_validates_the_resolved_target_and_single_node_before_input() {
        let graph: provenant_cli::Target = "work".parse().unwrap();
        let rows = [
            json!({"id":"work","kind":"graph"}),
            json!({"id":"work/inspect","kind":"node"}),
        ];
        assert!(validate_retry_brief_target(&graph, &rows).is_ok());
        let multiple_nodes = [
            json!({"id":"work","kind":"graph"}),
            json!({"id":"work/inspect","kind":"node"}),
            json!({"id":"work/test","kind":"node"}),
        ];
        assert!(validate_retry_brief_target(&graph, &multiple_nodes).is_err());

        let node: provenant_cli::Target = "work/inspect".parse().unwrap();
        assert!(validate_retry_brief_target(&node, &rows).is_ok());
        assert!(validate_retry_brief_target(&node, &[]).is_err());
    }

    #[test]
    fn unreadable_attempt_keeps_other_status_rows() {
        let mut rows = [
            json!({"kind":"attempt", "state":"running"}),
            json!({"kind":"graph", "state":"running"}),
        ];
        mark_unreadable_attempt(
            &mut rows[0],
            "attempt-id",
            &provenant_records::DecodeError::Typed { line: 2, column: 3 },
        );
        assert_eq!(rows[0]["state"], "unreadable");
        assert!(
            rows[0]["reason"]
                .as_str()
                .unwrap()
                .contains("run store record for attempt attempt-id: decode error")
        );
        assert_eq!(rows[1]["state"], "running");
    }

    fn checkpoint(graph: &str) -> GraphCheckpoint {
        serde_json::from_value(json!({"schema_version":1,"graph_id":graph,"plan_digest":"digest","nodes":[
            {"id":"parent","state":"running","worktree":null,"attempts":[
                {"launched":true,"key":{"graph_id":graph,"node_id":"parent","route_index":0, "attempt": 1},"route":{"alias":"senior"},"state":"running","attempt_ref":"11111111111111111111111111111111/22222222222222222222222222222222","launch_actor":"actor"}
            ]},
            {"id":"parent--child","state":"pending","worktree":null,"attempts":[]},
            {"id":"parent--keep","state":"pending","worktree":null,"attempts":[]},
            {"id":"other","state":"pending","worktree":null,"attempts":[]}
        ]})).unwrap()
    }

    fn document(graph: &str) -> GraphDocument {
        serde_json::from_value(json!({"schema_version":1,"graph_id":graph,"nodes":[
            {"id":"parent","access":"read_only","prompt":"parent","route":{"alias":"senior"}},
            {"id":"parent--child","access":"read_only","prompt":"child","route":{"alias":"senior"}},
            {"id":"parent--keep","access":"read_only","prompt":"keep","route":{"alias":"senior"},"detached":true},
            {"id":"other","access":"read_only","prompt":"other","route":{"alias":"senior"}}
        ]})).unwrap()
    }

    #[test]
    fn graph_peer_status_wait_preserves_disk_sample_and_remedy()
    -> Result<(), Box<dyn std::error::Error>> {
        let id = "11111111111111111111111111111111";
        let digest = format!("sha256:{}", "ab".repeat(32));
        let attempt: provenant_records::run::AttemptRecord = serde_json::from_value(json!({
            "schema_version":1,"attempt_id":id,"project_id":id,
            "key":{"project":id,"graph":"graph","node":"node","attempt":1},"attempt_number":1,"revision":1,"generation":1,
            "requested_route":{"kind":"named","name":"senior"},
            "authority_digest":digest,"queued_at":"2026-10-29T00:00:00.000000Z",
            "state":{"state":"starting"},"usage":{},"read_only_writable":[],
            "protected_paths":[],"trace":{},
            "prompt":{"path":{"encoding":"utf8","path":"prompt.md"},
                "digest":digest,"bytes":0,"complete":true,"redacted":false}
        }))?;
        let sample = "host capacity: disk space: 123 bytes available; gc.free_floor is 456 bytes (source: explicit)";
        let waits = vec![provenant_records::run::LaneWait {
            extra: Default::default(),
            attempt_id: attempt.attempt_id,
            lane_class: provenant_records::run::LaneClass::Writer,
            waiting_for: sample.into(),
        }];
        let mut extra = provenant_records::ExtraFields::default();
        extra.insert("lane_waits".into(), serde_json::to_value(&waits)?);
        let response = RunResult::Status {
            extra,
            attempt: attempt.clone(),
        };
        let decoded: RunResult = serde_json::from_value(serde_json::to_value(response)?)?;
        let RunResult::Status {
            attempt: peer_attempt,
            extra,
        } = decoded
        else {
            return Err("status response changed kind".into());
        };
        let peer_waits: Vec<provenant_records::run::LaneWait> = serde_json::from_value(
            extra
                .get("lane_waits")
                .ok_or("peer disk waits missing")?
                .clone(),
        )?;
        for (record, waits) in [(&attempt, &waits), (&peer_attempt, &peer_waits)] {
            let projected =
                provenant_cli::run::project_lane(record, waits, Path::new("/unused"), None, false);
            assert_eq!(projected.waiting_for.as_deref(), Some(sample));
            let reason = projected.reason.ok_or("disk remedy missing")?;
            assert!(reason.contains(sample));
            assert!(reason.contains("provenant gc --apply"));
        }
        Ok(())
    }

    fn listed(name: &str, settled: bool, changed_micros: i64) -> ListedGraph {
        ListedGraph {
            name: name.into(),
            state: if settled { "succeeded" } else { "pending" }.into(),
            settled,
            template: None,
            nodes: 1,
            created_micros: Some(changed_micros),
            changed_micros: Some(changed_micros),
            reason: None,
            digests: Vec::new(),
            open_asks: Vec::new(),
        }
    }

    #[test]
    fn the_listing_never_hides_active_work_and_counts_what_it_omits() {
        let day = provenant_cli::limits::LISTING_SETTLED_SECONDS * 1_000_000;
        let now = 10 * day;
        for (active, recent, old, limit, all) in [
            (25_usize, 3_usize, 2_usize, 20, false),
            (2, 30, 4, 20, false),
            (0, 3, 40, 20, true),
            (5, 0, 0, 1, false),
        ] {
            let mut graphs = Vec::new();
            graphs.extend((0..active).map(|n| listed(&format!("a{n}"), false, now - 2 * day)));
            graphs.extend(
                (0..recent)
                    .zip(0_i64..)
                    .map(|(n, age)| listed(&format!("r{n}"), true, now - age)),
            );
            graphs.extend(
                (0..old)
                    .zip(0_i64..)
                    .map(|(n, age)| listed(&format!("o{n}"), true, now - 2 * day - age)),
            );
            let listing = select_listing(graphs, now, all, limit, None);
            let shown_active = listing.rows.iter().filter(|row| !row.settled).count();
            let shown_settled = listing.rows.len() - shown_active;
            assert_eq!(shown_active, active, "every active graph is listed");
            assert_eq!(
                listing.omitted.values().sum::<usize>(),
                recent + old - shown_settled,
                "the omission count is the settled rows not shown"
            );
            assert!(
                if all {
                    shown_settled <= limit
                } else {
                    shown_settled == 0 || listing.rows.len() <= limit
                },
                "settled rows respect the page allowance while every active graph stays visible"
            );
        }
        // `--all` pages through every settled graph, newest first.
        let graphs: Vec<_> = (0..5).map(|n| listed(&format!("s{n}"), true, n)).collect();
        let first = select_listing(graphs.clone(), 3 * day, true, 2, None);
        assert_eq!(first.rows[0].name, "s4");
        let second = select_listing(graphs, 3 * day, true, 2, first.next);
        assert_eq!(second.rows[0].name, "s2");
    }

    #[test]
    fn a_stop_cause_is_one_bounded_line_that_names_it() {
        let row = json!({"state":"failed","cause_code":"budget_exhausted",
            "reason":"x".repeat(4096),"waiting_for":null});
        let line = status_line("fix-login/writer#2", &row, Some("codex/gpt"));
        assert_eq!(line.lines().count(), 1);
        assert!(line.trim_end().len() <= provenant_cli::limits::SUMMARY_ROW_BYTES);
        assert!(line.contains("cause=budget_exhausted"), "{line}");
    }

    #[test]
    fn unified_status_uses_one_row_contract_and_selects_attempts() {
        // A projection may omit earlier rows; each displayed handle must still
        // resolve to the WorkKey and reference that produced it.
        let mut projected = checkpoint("g");
        projected.nodes[0].attempts[0].key.attempt = 7;
        let run = graph_snapshot(projected);
        let graph = GraphTarget {
            graph: "g".into(),
            node: None,
            attempt: None,
        };
        let rows = status_rows(&run, &graph);
        let keys = rows[0].as_object().unwrap().keys().collect::<Vec<_>>();
        assert!(
            rows.iter()
                .all(|row| row.as_object().unwrap().keys().collect::<Vec<_>>() == keys)
        );
        assert_eq!(rows[0]["kind"], "graph");
        assert_eq!(rows[1]["id"], "g/parent");
        for row in rows.iter().filter(|row| row["kind"] == "attempt") {
            let target = provenant_cli::Target::parse(row["id"].as_str().unwrap()).unwrap();
            let resolved =
                provenant_cli::target::resolve(&target, Want::Attempt { settled: false }, |_| {
                    Ok::<_, std::convert::Infallible>(Some(run.checkpoint.clone()))
                })
                .unwrap();
            assert_eq!(resolved.attempt_ref.as_deref(), row["attempt_id"].as_str());
            assert_eq!(
                resolved.number.map(|number| number.get()),
                Some(run.checkpoint.nodes[0].attempts[0].key.attempt)
            );
        }
        let attempt = GraphTarget {
            graph: "g".into(),
            node: Some("parent".into()),
            attempt: Some(
                "11111111111111111111111111111111/22222222222222222222222222222222".into(),
            ),
        };
        let rows = status_rows(&run, &attempt);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["kind"], "attempt");
        assert_eq!(rows[0]["node_id"], "parent");

        let mut checkpoint = checkpoint("g");
        let old = checkpoint.nodes[0].attempts[0].clone();
        let mut latest = old.clone();
        latest.key.attempt += 1;
        latest.attempt_ref = None;
        checkpoint.nodes[0].attempts.push(latest);
        let rows = status_rows(&graph_snapshot(checkpoint), &graph);
        let old = rows.iter().find(|row| row["kind"] == "attempt").unwrap();
        assert_eq!(old["latest_attempt"], false);
    }

    #[test]
    fn graph_status_keeps_admission_reclaim_visible_on_the_running_parent() {
        let target = GraphTarget {
            graph: "g".into(),
            node: None,
            attempt: None,
        };
        let mut rows = status_rows(&graph_snapshot(checkpoint("g")), &target);
        let wait = "admission: reclaiming host and project capacity or startup spacing";
        let attempt = rows
            .iter_mut()
            .find(|row| row["kind"] == "attempt")
            .unwrap();
        attempt["graph_state"] = json!("running");
        attempt["waiting_for"] = json!(wait);
        project_node_waits(&mut rows);
        assert_eq!(rows[1]["waiting_for"], wait);
        let reading = render_graph_status(&rows);
        // The node line (`NAME/NODE#N STATE MODEL`) carries the wait, and the
        // next step is the wait verb on that node.
        let parent = reading
            .lines()
            .find(|line| line.starts_with("g/parent#"))
            .unwrap();
        assert!(
            parent.contains(" running ") && parent.ends_with(wait),
            "{reading}"
        );
        assert_eq!(rows[1]["next"], "provenant wait g/parent");

        // A historical attempt's wait must not leak onto a later live parent.
        rows[1]["waiting_for"] = Value::Null;
        let attempt = rows
            .iter_mut()
            .find(|row| row["kind"] == "attempt")
            .unwrap();
        attempt["state"] = json!("terminal");
        project_node_waits(&mut rows);
        assert!(rows[1]["waiting_for"].is_null());
    }

    #[test]
    fn graph_status_reading_keeps_failure_cause_and_command_within_budget() {
        let mut checkpoint = checkpoint("g");
        checkpoint.nodes[0].state = NodeState::Failed;
        checkpoint.nodes[0].advance_error_code = Some("AUTH_REQUIRED".into());
        checkpoint.nodes[0].advance_error =
            Some(format!("credentials unavailable {}", "界".repeat(4096)));
        let target = GraphTarget {
            graph: "g".into(),
            node: None,
            attempt: None,
        };
        let rows = status_rows(&graph_snapshot(checkpoint), &target);
        // One graph row, then many node and attempt rows.
        let mut many = rows.clone();
        for _ in 0..100 {
            many.extend(rows.iter().filter(|row| row["kind"] != "graph").cloned());
        }
        let reading = render_graph_status(&many);
        // The owner summary bound (cli.md "Agent output"): the graph line, at
        // most LISTING_ROWS node lines, the omission count and `next`, each
        // line within SUMMARY_ROW_BYTES.
        use provenant_cli::limits::{LISTING_ROWS, SUMMARY_ROW_BYTES};
        let lines: Vec<_> = reading.lines().collect();
        let omissions = lines
            .iter()
            .filter(|line| line.contains(" nodes not shown;"))
            .count();
        let next = usize::from(lines.last().is_some_and(|line| line.starts_with("next: ")));
        assert!(
            lines.len() <= 1 + LISTING_ROWS + omissions + next,
            "{reading}"
        );
        assert!(
            reading.lines().all(|line| line.len() <= SUMMARY_ROW_BYTES),
            "{reading}"
        );
        // Failed work is listed first, after the graph line.
        let first_node = reading.lines().nth(1).unwrap();
        assert!(first_node.starts_with("g/parent"), "{reading}");
        assert!(first_node.contains(" failed "), "{reading}");
        assert!(first_node.contains("AUTH_REQUIRED"), "{reading}");
        assert!(
            reading.ends_with("next: provenant retry g/parent\n"),
            "{reading}"
        );
        // Per-state omission counts are checked at the CLI boundary; do not
        // pin the human omission sentence here.
        assert_eq!(rows[1]["cause_code"], "AUTH_REQUIRED");
        assert_eq!(rows[1]["next"], "provenant retry g/parent");
    }

    #[test]
    fn graph_failure_rows_prefer_owner_code_and_fall_back_for_old_checkpoints() {
        let mut checkpoint = checkpoint("g");
        checkpoint.nodes[0].state = NodeState::Blocked;
        checkpoint.nodes[0].advance_error_code = Some("launch_refused".into());
        checkpoint.nodes[0].advance_owner_code = Some("invalid_input".into());
        let target = GraphTarget {
            graph: "g".into(),
            node: None,
            attempt: None,
        };
        let rows = status_rows(&graph_snapshot(checkpoint.clone()), &target);
        assert_eq!(rows[1]["cause_code"], "invalid_input");
        let mut old = serde_json::to_value(checkpoint).unwrap();
        old["nodes"][0]
            .as_object_mut()
            .unwrap()
            .remove("advance_owner_code");
        let old = serde_json::from_value(old).unwrap();
        let rows = status_rows(&graph_snapshot(old), &target);
        assert_eq!(rows[1]["cause_code"], "launch_refused");
    }

    #[test]
    fn graph_failure_rows_preserve_the_owner_cause_and_action() {
        let mut checkpoint = checkpoint("g");
        checkpoint.nodes[0].state = NodeState::Blocked;
        checkpoint.nodes[0].advance_error_code = Some("launch_retry_exhausted".into());
        checkpoint.nodes[0].advance_owner_code = Some("LAUNCH_UNKNOWN".into());
        checkpoint.nodes[0].advance_error = Some("[LAUNCH_UNKNOWN] inspect the receipt for operation op-123 before retrying with the same operation ID".into());
        let target = GraphTarget {
            graph: "g".into(),
            node: None,
            attempt: None,
        };
        let rows = status_rows(&graph_snapshot(checkpoint), &target);
        let node = &rows[1];
        assert_eq!(node["cause_code"], "LAUNCH_UNKNOWN");
        assert!(node["next"].as_str().unwrap().contains("op-123"));
        assert!(
            !node["next"]
                .as_str()
                .unwrap()
                .starts_with("provenant retry")
        );
        let reply = with_graph_failures(
            provenant_cli::run::Reply {
                result: provenant_records::CommandResult::new(
                    "output",
                    provenant_records::Outcome::Ok,
                    json!({"outputs":[]}),
                ),
                human: String::new(),
                summary: Some(provenant_cli::Summary::verbatim("attempt output", None)),
                read_only: true,
            },
            std::slice::from_ref(node),
        );
        assert_eq!(
            reply.result.data["graph_failures"][0]["reason"],
            node["reason"]
        );
        assert!(reply.human.contains("op-123"));
        assert!(
            reply
                .summary
                .unwrap()
                .lines
                .iter()
                .any(|line| line.contains("op-123"))
        );

        let mut rows = vec![
            json!({"kind":"node", "graph_id":"g", "node_id":"a", "state":"failed", "reason":null, "cause_code":"adapter_unavailable"}),
            json!({"kind":"attempt", "graph_id":"g", "node_id":"a", "graph_state":"terminal", "latest_attempt":true, "terminal_cause":"command_unavailable", "reason":"configure the provider command"}),
        ];
        project_attempt_failure(&mut rows[1]);
        assert_eq!(rows[1]["cause_code"], "command_unavailable");
        assert_eq!(rows[1]["next"], "configure the provider command");
        project_node_failures(&mut rows);
        assert_eq!(rows[0]["cause_code"], "command_unavailable");
        assert_eq!(rows[0]["reason"], "configure the provider command");
        assert_eq!(rows[0]["next"], "configure the provider command");
        assert!(render_graph_status(&rows[..1]).contains("configure the provider command"));

        // An older terminal attempt must not supply the cause for a newer
        // attempt whose state has not settled or whose record was collected.
        rows[0]["reason"] = Value::Null;
        rows[0]["cause_code"] = json!("adapter_unavailable");
        rows[1]["latest_attempt"] = json!(false);
        rows.push(json!({"kind":"attempt", "graph_id":"g", "node_id":"a", "graph_state":"starting", "latest_attempt":true, "reason":null}));
        project_node_failures(&mut rows);
        assert!(rows[0]["reason"].is_null());
        assert_eq!(rows[0]["cause_code"], "adapter_unavailable");
    }

    #[test]
    fn recovered_attempt_failure_does_not_recommend_retrying_successful_work() {
        let rows = vec![
            json!({"kind":"graph","id":"g","state":"complete","outcome":"succeeded","next":"provenant output g"}),
            json!({"kind":"node","id":"g/a","state":"succeeded","next":"provenant output g/a"}),
            json!({"kind":"attempt","id":"old","state":"terminal","outcome":"task_failed","next":"provenant retry g/a"}),
        ];
        let reading = render_graph_status(&rows);
        assert!(reading.starts_with("g complete"), "{reading}");
        assert!(reading.ends_with("next: provenant output g\n"));
    }

    #[test]
    fn subtree_stop_preserves_detached_children_and_unrelated_nodes()
    -> Result<(), Box<dyn std::error::Error>> {
        struct FreeWorkers;
        impl WorkerObserver for FreeWorkers {
            fn observe_roots(
                &self,
                roots: &[PathBuf],
            ) -> std::collections::BTreeMap<PathBuf, provenant_confinement::worktrees::Occupancy>
            {
                roots
                    .iter()
                    .cloned()
                    .map(|path| (path, provenant_confinement::worktrees::Occupancy::Available))
                    .collect()
            }
        }
        let rig = crate::worktree_tests::support::TestRig::new()?;
        let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
        let context = Context::open(roots.clone(), &rig.home, &rig.project)?;
        let project = context.identity.project;
        let factory = Factory {
            context,
            project_attribution: false,
            clock: SystemClock::new(),
            backoff: ThreadPause,
            executor: SystemExecutor::new(1)?,
            home: rig.home.clone(),
            cwd: rig.project.clone(),
            assets: rig.project.clone(),
            host_launch: None,
            workers: &FreeWorkers,
        };
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let mut journal =
            FileJournal::open(&roots.state, project, "g", budget, &cancel, &factory.clock)?;
        journal.bind(GraphExecution {
            extra: Default::default(),
            review_base: None,
            integration_base: None,
            directory: rig.project.clone(),
            document: document("g"),
        })?;
        let mut before = checkpoint("g");
        before.nodes[0].attempts.clear();
        journal.save(&before)?;
        drop(journal);
        let events = FileJournal::read_events(&roots.state, project, "g")?;
        let stopped = factory.stop_target(
            &GraphTarget {
                graph: "g".into(),
                node: Some("parent".into()),
                attempt: None,
            },
            None,
            budget,
            &cancel,
        )?;
        assert_eq!(stopped.checkpoint.nodes[0].state, NodeState::Skipped);
        assert_eq!(stopped.checkpoint.nodes[1].state, NodeState::Skipped);
        assert_eq!(stopped.checkpoint.nodes[2], before.nodes[2]);
        assert_eq!(stopped.checkpoint.nodes[3], before.nodes[3]);
        let mut durable = stopped.checkpoint.clone();
        durable.extra.remove("cancellation_request_receipt");
        assert_eq!(
            FileJournal::read_checkpoint(&roots.state, project, "g")?,
            Some(durable)
        );
        assert_ne!(
            FileJournal::read_events(&roots.state, project, "g")?,
            events
        );
        Ok(())
    }

    #[test]
    fn pending_attempt_status_and_numbered_stop_keep_one_launch()
    -> Result<(), Box<dyn std::error::Error>> {
        if super::super::peer_tests::host_test(
            "fabric::unified::tests::pending_attempt_status_and_numbered_stop_keep_one_launch",
        )? {
            return Ok(());
        }
        struct FreeWorkers;
        impl WorkerObserver for FreeWorkers {
            fn observe_roots(
                &self,
                roots: &[PathBuf],
            ) -> std::collections::BTreeMap<PathBuf, provenant_confinement::worktrees::Occupancy>
            {
                roots
                    .iter()
                    .cloned()
                    .map(|path| (path, provenant_confinement::worktrees::Occupancy::Available))
                    .collect()
            }
        }
        // Medium: real host journal and admitted run store, with only provider
        // launch deferred. The `NAME/NODE#1` target controls that run.
        {
            let rig = crate::worktree_tests::repository()?;
            let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
            let factory = Factory {
                context: Context::open(roots.clone(), &rig.home, &rig.project)?,
                project_attribution: false,
                clock: SystemClock::new(),
                backoff: ThreadPause,
                executor: SystemExecutor::new(1)?,
                home: rig.home.clone(),
                cwd: rig.project.clone(),
                assets: rig.project.clone(),
                host_launch: None,
                workers: &FreeWorkers,
            };
            let cancel = CancelToken::new();
            let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
            let input = json!({
                "op":"run", "brief":"Inspect the parser", "name":"stop-by-attempt",
                "document":{"use":"task", "routes":[
                    {"adapter":"fixture", "model":"fixture/first"},
                    {"adapter":"fixture", "model":"fixture/fallback"}
                ]}
            });
            let run = provenant_cli::run::with_deferred_provider_launch(|| {
                factory.graph_document(input.clone(), budget, &cancel)
            })?;
            assert_eq!(run["status"], "pending");
            assert_eq!(run["timed_out"], false, "submission did not request a wait");
            let graph = run["checkpoint"]["graph_id"]
                .as_str()
                .ok_or("graph ID missing")?;
            let reference = run["checkpoint"]["nodes"][0]["attempts"][0]["attempt_ref"]
                .as_str()
                .ok_or("accepted attempt missing")?;
            let id = reference.rsplit('/').next().ok_or("attempt ID missing")?;
            let node_id = run["checkpoint"]["nodes"][0]["id"]
                .as_str()
                .ok_or("node ID missing")?;
            let selector = format!("{graph}/{node_id}#1");
            for selected in [graph, selector.as_str()] {
                let status = factory.graph_document(
                    json!({"op":"status", "id":selected}),
                    budget,
                    &cancel,
                )?;
                assert_eq!(status["timed_out"], false, "status did not request a wait");
                assert!(
                    status["rows"]
                        .as_array()
                        .ok_or("status rows missing")?
                        .iter()
                        .any(|row| row["kind"] == "attempt" && row["attempt_id"] == reference)
                );
            }
            let stopped = factory.graph_document(
                json!({"op":"stop", "id":selector, "reason":"stop the task"}),
                budget,
                &cancel,
            )?;
            assert_eq!(stopped["timed_out"], false);
            let checkpoint = FileJournal::read_checkpoint(
                &roots.state,
                factory.context.identity.project,
                graph,
            )?
            .ok_or("stopped checkpoint missing")?;
            let node = checkpoint.nodes.first().ok_or("stopped node missing")?;
            assert_eq!(node.state, NodeState::Skipped);
            assert_eq!(node.extra.get("subtree_cancel"), Some(&json!(node.id)));
            assert_eq!(node.attempts.len(), 1);
            assert_eq!(node.attempts[0].attempt_ref.as_deref(), Some(reference));
            let owner = BinaryRunOwner {
                factory: &factory,
                allow_remote: false,
                actor: None,
                chair: None,
            };
            let store = owner.store(&cancel)?;
            let record = store.record(id.parse()?, &owner.call(&cancel))?;
            assert!(matches!(
                record.state,
                provenant_run::AttemptState::Terminal {
                    outcome: provenant_run::Outcome::Cancelled,
                    ..
                }
            ));
            // Replaying the named launch returns the stopped graph and starts
            // nothing: its unused second route stays unused after the slot is
            // freed.
            let replayed = provenant_cli::run::with_deferred_provider_launch(|| {
                factory.graph_document(input, budget, &cancel)
            })?;
            let after: GraphCheckpoint = serde_json::from_value(replayed["checkpoint"].clone())?;
            assert_eq!(after.nodes[0].state, NodeState::Skipped);
            assert_eq!(after.nodes[0].attempts, node.attempts);
            let events =
                FileJournal::read_events(&roots.state, factory.context.identity.project, graph)?;
            // A reservation and its later saved reference both emit `started`.
            // Count route identities, rather than observations of the same one.
            let started: std::collections::BTreeSet<_> = events
                .iter()
                .filter(|event| {
                    event.kind == provenant_fabric_runtime::graph::GraphEventKind::Attempt
                        && event.code == "started"
                })
                .map(|event| {
                    (
                        event.subject.as_str(),
                        event
                            .details
                            .as_ref()
                            .and_then(|details| details["route_index"].as_u64()),
                    )
                })
                .collect();
            assert_eq!(
                started,
                std::collections::BTreeSet::from([(node.id.as_str(), Some(0))])
            );
        }
        Ok(())
    }

    #[test]
    fn a_numbered_target_reaches_its_recorded_attempt_through_the_journal()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let base = temp.path().canonicalize()?;
        let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("assets"));
        inputs.state_dir = Some(base.join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        std::fs::create_dir_all(roots.state.as_path())?;
        let project = provenant_core::ProjectId::from_bytes([2; 16]);
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        for graph in ["g", "h"] {
            let budget = WaitBudget::cli(clock.mono_now(), 30)?;
            let mut journal =
                FileJournal::open(&roots.state, project, graph, budget, &cancel, &clock)?;
            journal.bind(GraphExecution {
                extra: Default::default(),
                review_base: None,
                integration_base: None,
                directory: base.clone(),
                document: document(graph),
            })?;
            let mut checkpoint = checkpoint(graph);
            if graph == "h" {
                checkpoint.nodes[0].attempts.clear();
            }
            journal.save(&checkpoint)?;
        }
        let target = |text: &str| text.parse::<provenant_cli::Target>();
        let g = "11111111111111111111111111111111";
        let a = "22222222222222222222222222222222";
        // `NAME/NODE#1` reaches the one recorded attempt through the journal,
        // and steering reaches the same attempt while it is live.
        let attempt = resolve(
            &roots.state,
            project,
            &target("g/parent#1")?,
            Want::Attempt { settled: false },
        )?;
        assert_eq!(
            attempt.attempt_ref.as_deref(),
            Some(format!("{g}/{a}").as_str())
        );
        let checkpoint =
            FileJournal::read_status_checkpoint(&roots.state, project, "g")?.ok_or("checkpoint")?;
        assert_eq!(
            provenant_cli::inbox::live_steer_reference(&checkpoint, &attempt),
            Ok(format!("{g}/{a}").as_str())
        );
        // A node with no recorded attempt has no `#1`, and an unknown graph
        // is not found.
        assert!(matches!(
            resolve(&roots.state, project, &target("h/parent#1")?, Want::Work),
            Err(ResolveError::NotFound(_))
        ));
        assert!(matches!(
            resolve(&roots.state, project, &target("missing")?, Want::Work),
            Err(ResolveError::NotFound(_))
        ));
        Ok(())
    }

    #[test]
    fn lane_not_found_is_an_invalid_target_result() {
        let error = ServiceError::Facade(FacadeError::NotFound);
        let reply = output_error(&error);

        assert_eq!(reply.result.status, provenant_records::Outcome::Invalid);
        assert_eq!(
            reply.result.error.as_ref().map(|error| error.code.as_str()),
            Some("NOT_FOUND")
        );
        assert_eq!(provenant_cli::exit_status(&reply.result.status), 2);
    }
}
