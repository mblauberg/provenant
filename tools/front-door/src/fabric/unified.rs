//! Read and control graph targets through the public command surface.
use super::*;
use provenant_fabric_runtime::graph::{AttemptState, FileJournal, GraphRun};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub(crate) struct GraphTarget {
    pub graph: String,
    pub node: Option<String>,
    pub attempt: Option<String>,
}

pub(crate) fn target_text(target: &provenant_cli::Target) -> String {
    target.selection.clone().unwrap_or_else(|| {
        [
            target.run.map(|id| id.to_string()),
            target.task.map(|id| id.to_string()),
            target.attempt.map(|id| id.to_string()),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("/")
    })
}

impl Factory {
    pub(crate) fn graph_target(&self, selector: &str) -> Result<Option<GraphTarget>, FacadeError> {
        resolve_target(
            &self.context.roots.state,
            self.context.identity.project,
            selector,
        )
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
        let mut journal = FileJournal::open(
            &self.context.roots.state,
            self.context.identity.project,
            &target.graph,
            budget,
            cancel,
            &self.clock,
        )
        .map_err(graph_journal_error)?;
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
        Ok(graph_snapshot(checkpoint))
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
                .or(run.checkpoint.pause_reason.as_deref())
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
            let Some(reference) = &attempt.attempt_ref else {
                continue;
            };
            if target.attempt.as_ref().is_some_and(|id| id != reference) {
                continue;
            }
            let mut attempt_row = row(
                reference.clone(),
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
                format!("provenant status {selection} --wait"),
            _ => format!("provenant output {selection}"),
        });
    }
    rows
}

/// A bounded reading for agents, with failed work before healthy history.
/// Reserve the final command before filling the body so truncation cannot hide it.
pub(crate) fn render_graph_status(rows: &[Value]) -> String {
    fn text(value: &Value, limit: usize) -> String {
        let mut text =
            provenant_fabric_runtime::hosts::plain_text(value.as_str().unwrap_or(""), limit);
        let mut end = limit.min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text
    }
    let priority = |row: &&Value| {
        // Attempt rows include immutable failures from earlier retries. Current
        // node and graph states decide what needs action now.
        if row["kind"] == "attempt" {
            return 4;
        }
        match row["state"].as_str() {
            Some("failed" | "blocked" | "paused" | "unresolved") => 0,
            Some("complete")
                if row["outcome"]
                    .as_str()
                    .is_some_and(|outcome| outcome != "succeeded") =>
            {
                1
            }
            Some("pending" | "running" | "starting") => 2,
            _ => 3,
        }
    };
    let mut ordered: Vec<_> = rows.iter().collect();
    ordered.sort_by_key(priority);
    let next = ordered
        .first()
        .map(|row| text(&row["next"], 384))
        .unwrap_or_default();
    let footer = format!("next: {next}\n");
    let limit = 2048usize.saturating_sub(footer.len() + 32);
    let mut human = String::new();
    for (index, row) in ordered.iter().enumerate() {
        let mut line = format!(
            "{} {} {}",
            text(&row["kind"], 16),
            text(&row["id"], 160),
            text(&row["state"], 32)
        );
        if !row["outcome"].is_null() {
            line.push_str(&format!(" outcome={}", text(&row["outcome"], 48)));
        }
        if !row["cause_code"].is_null() {
            line.push_str(&format!(" cause={}", text(&row["cause_code"], 80)));
        }
        let reason = if row["reason"].is_null() {
            &row["waiting_for"]
        } else {
            &row["reason"]
        };
        if !reason.is_null() {
            line.push_str(&format!(": {}", text(reason, 240)));
        }
        line.push('\n');
        if human.len() + line.len() > limit {
            human.push_str(&format!("{} more rows\n", ordered.len() - index));
            break;
        }
        human.push_str(&line);
    }
    human.push_str(&footer);
    human
}

/// Enrich graph attempt rows from identity-checked host records. Checkpoint
/// rows describe graph progress; the run owner retains typed terminal causes.
pub(crate) fn enrich_status_rows(
    rows: &mut [Value],
    context: &Context,
    clock: &SystemClock,
    cancel: &CancelToken,
) -> Result<(), FacadeError> {
    let budget = WaitBudget::cli(clock.mono_now(), 30).map_err(|_| FacadeError::Timeout)?;
    enrich_status_rows_within(rows, context, clock, cancel, budget)
}

pub(crate) fn enrich_status_rows_within(
    rows: &mut [Value],
    context: &Context,
    clock: &SystemClock,
    cancel: &CancelToken,
    peer_budget: WaitBudget,
) -> Result<(), FacadeError> {
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
            let response = context
                .peer_with(
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
                )
                .map_err(|failure| failure.error)?;
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
        if retain_collected_peer_row(row, &record, host, context.identity.host) {
            continue;
        }
        row["preparation_failure"] = json!(record.preparation_failure);
        row["settlement_reason"] = json!(record.settlement_reason);
        let peer_origin = record.extra.get("peer_origin_context");
        let peer_graph = record.extra.get("peer_graph");
        let origin_matches = host != context.identity.host
            && peer_graph.is_some_and(|graph| {
                graph["host"] == serde_json::json!(host)
                    && graph["graph_id"].as_str().is_some_and(|id| !id.is_empty())
            })
            && peer_origin.is_some_and(|origin| {
                origin["graph_id"] == row["graph_id"] && origin["node_id"] == row["node_id"]
            });
        if !origin_matches
            && record.context.as_ref().is_none_or(|context| {
                context.graph_id.as_deref() != row["graph_id"].as_str()
                    || context.node_id.as_deref() != row["node_id"].as_str()
            })
        {
            return Err(FacadeError::StateCorrupt);
        }
        for field in ["peer_graph", "peer_subtree"] {
            if let Some(value) = record.extra.get(field) {
                row[field] = value.clone();
            }
        }
        let projected = provenant_cli::run::project_lane(
            &record,
            &waits,
            context.roots.state.as_path(),
            clock.wall_now().ok(),
            host == context.identity.host,
        );
        let fields = serde_json::to_value(projected).map_err(|_| FacadeError::StateCorrupt)?;
        let reference = row["attempt_id"].clone();
        let graph_state = row["state"].clone();
        row.as_object_mut()
            .ok_or(FacadeError::StateCorrupt)?
            .extend(fields.as_object().ok_or(FacadeError::StateCorrupt)?.clone());
        row["attempt_id"] = reference;
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

pub(crate) enum GraphCommand<'a> {
    Status {
        wait: Option<u64>,
    },
    Stop {
        reason: Option<String>,
        wait: Option<u64>,
    },
    Retry {
        extend: &'a [String],
        node: Option<&'a str>,
        brief: Option<&'a str>,
        legacy_prompt: bool,
        no_fallback: bool,
        wait: Option<u64>,
    },
}

/// None means this identifier belongs to the ordinary attempt store.
pub(crate) fn unified_graph_command(
    selector: &str,
    command: GraphCommand<'_>,
    globals: &provenant_cli::Globals,
    workers: &'static dyn WorkerObserver,
) -> Option<u8> {
    if provenant_cli::fabric::in_lane() && matches!(command, GraphCommand::Stop { .. }) {
        return None;
    }
    let name = match command {
        GraphCommand::Status { .. } => "status",
        GraphCommand::Stop { .. } => "stop",
        GraphCommand::Retry { .. } => "retry",
    };
    type CommandView = Option<(Value, String, bool)>;
    let result = (|| -> Result<CommandView, Box<dyn std::error::Error>> {
        let factory = if matches!(command, GraphCommand::Status { .. }) {
            match read_factory(globals, workers) {
                Ok(factory) => factory,
                Err(error) if missing_graph_context(error.as_ref()) => return Ok(None),
                Err(error) => return Err(error),
            }
        } else {
            factory(globals, workers)?
        };
        let Some(mut target) = (if factory.context.in_lane {
            Some(GraphTarget {
                graph: selector.to_owned(),
                node: None,
                attempt: None,
            })
        } else {
            factory.graph_target(selector)?
        }) else {
            return Ok(None);
        };
        if let GraphCommand::Retry { node: Some(id), .. } = &command {
            if target
                .node
                .as_deref()
                .is_some_and(|selected| selected != *id)
            {
                return Err(FacadeError::invalid(InvalidInputDetail::Field("node".into())).into());
            }
            target.node = Some((*id).to_owned());
        }
        let mut input = json!({"op":name,"id":selector});
        match &command {
            GraphCommand::Status { wait } => {
                if let Some(seconds) = wait {
                    input["wait_seconds"] = json!(seconds);
                }
            }
            GraphCommand::Stop { reason, wait } => {
                if let Some(reason) = reason {
                    input["reason"] = json!(reason);
                }
                if let Some(seconds) = wait {
                    input["wait_seconds"] = json!(seconds);
                }
            }
            GraphCommand::Retry {
                extend,
                node,
                brief,
                legacy_prompt,
                no_fallback,
                wait,
            } => {
                if *legacy_prompt || *no_fallback {
                    return Err(
                        FacadeError::invalid(InvalidInputDetail::Field("node".into())).into(),
                    );
                }
                if !extend.is_empty() && (node.is_some() || brief.is_some()) {
                    return Err(
                        FacadeError::invalid(InvalidInputDetail::Field("node".into())).into(),
                    );
                }
                input["extend"] = json!(extend);
                if let Some(node) = node {
                    input["node"] = json!(node);
                }
                if let Some(brief) = brief {
                    input["brief"] = json!(brief);
                }
                if let Some(seconds) = wait {
                    input["wait_seconds"] = json!(seconds);
                }
            }
        }
        let cancel = CancelToken::new();
        let wait_seconds = input["wait_seconds"].as_u64().unwrap_or(55).max(55);
        let budget = WaitBudget::cli(factory.clock.mono_now(), wait_seconds)?;
        let mut data = factory.graph_document(input, budget, &cancel)?;
        let timed_out = data["timed_out"].as_bool().unwrap_or(false);
        if let Some(rows) = data["rows"].as_array_mut()
            && !factory.context.in_lane
        {
            localise_worktree_actions(&factory, &target.graph, rows);
        }
        let rows = data["rows"].as_array().ok_or(FacadeError::StateCorrupt)?;
        let human = render_graph_status(rows);
        Ok(Some((data, human, timed_out)))
    })();
    match result {
        Ok(None) => None,
        Ok(Some((data, human, timed_out))) => Some(crate::emit(
            &provenant_records::CommandResult::new(
                name,
                if timed_out {
                    provenant_records::Outcome::Waiting
                } else {
                    provenant_records::Outcome::Ok
                },
                data,
            ),
            globals,
            &human,
            false,
        )),
        Err(error) => Some(crate::fail(
            name,
            provenant_records::Outcome::Error,
            "graph_target",
            &error.to_string(),
            globals,
        )),
    }
}

pub(crate) fn unified_graph_steer(
    args: &provenant_cli::inbox::Steer,
    globals: &provenant_cli::Globals,
    workers: &'static dyn WorkerObserver,
) -> Option<u8> {
    use provenant_fabric_mcp::tools::McpFacade;
    enum SteerResult {
        Delivered(Value),
        Refused(&'static str),
    }
    let result = (|| -> Result<Option<SteerResult>, Box<dyn std::error::Error>> {
        let factory = factory(globals, workers)?;
        let Some(target) = factory.graph_target(&args.attempt)? else {
            return Ok(None);
        };
        let checkpoint = FileJournal::read_status_checkpoint(
            &factory.context.roots.state,
            factory.context.identity.project,
            &target.graph,
        )?
        .ok_or(FacadeError::NotFound)?;
        let reference = match provenant_cli::inbox::live_steer_reference(
            &checkpoint,
            target.node.as_deref(),
            target.attempt.as_deref(),
        ) {
            Ok(reference) => reference,
            Err(message) => return Ok(Some(SteerResult::Refused(message))),
        };
        let text = read_steering_stdin()?;
        let text = String::from_utf8(text)?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 55)?;
        let mut facade = BoundFacade { factory: &factory };

        let (host, id) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
        let operation_id = args
            .operation_id
            .as_ref()
            .map(|operation| {
                let digest = provenant_records::CanonicalV1::record_digest(
                    "graph-steer",
                    1,
                    &(operation, reference),
                )
                .map_err(|_| FacadeError::StateCorrupt)?;
                provenant_core::OperationId::parse(&format!(
                    "steer-{}",
                    provenant_core::hex(digest.as_bytes())
                ))
                .map_err(|_| FacadeError::StateCorrupt)
            })
            .transpose()?;
        let input = provenant_fabric_runtime::inbox::InboxInput::Steer {
            extra: Default::default(),
            attempt: id.parse()?,
            text,
            operation_id,
        };
        let receipt = facade.inbox_owner(input, Some(host.into()), budget, &cancel)?;
        Ok(Some(SteerResult::Delivered(json!({"receipts":[receipt]}))))
    })();
    match result {
        Ok(None) => None,
        Ok(Some(SteerResult::Refused(message))) => Some(crate::fail(
            "steer",
            provenant_records::Outcome::Refused,
            "steer_target_refused",
            message,
            globals,
        )),
        Ok(Some(SteerResult::Delivered(data))) => Some(crate::emit(
            &provenant_records::CommandResult::new("steer", provenant_records::Outcome::Ok, data),
            globals,
            "steering queued\n",
            false,
        )),
        Err(error) => Some(crate::fail(
            "steer",
            provenant_records::Outcome::Error,
            "graph_target",
            &error.to_string(),
            globals,
        )),
    }
}

pub(crate) fn append_graph_status(
    reply: &mut provenant_cli::run::Reply,
    globals: &provenant_cli::Globals,
    workers: &'static dyn WorkerObserver,
) {
    let result = (|| -> Result<Vec<Value>, Box<dyn std::error::Error>> {
        let factory = match read_factory(globals, workers) {
            Ok(factory) => factory,
            Err(error) if missing_graph_context(error.as_ref()) => return Ok(Default::default()),
            Err(error) => return Err(error),
        };
        let mut rows = Vec::new();
        for graph in FileJournal::graph_ids(
            &factory.context.roots.state,
            factory.context.identity.project,
        )? {
            let target = GraphTarget {
                graph: graph.clone(),
                node: None,
                attempt: None,
            };
            match FileJournal::read_status_checkpoint(
                &factory.context.roots.state,
                factory.context.identity.project,
                &graph,
            ) {
                Ok(Some(checkpoint)) => {
                    let mut graph_rows = status_rows(&graph_snapshot(checkpoint), &target);
                    enrich_status_rows(
                        &mut graph_rows,
                        &factory.context,
                        &factory.clock,
                        &CancelToken::new(),
                    )?;
                    localise_worktree_actions(&factory, &graph, &mut graph_rows);
                    rows.extend(graph_rows);
                }
                Ok(None) => {}
                Err(error) if error.is_unreadable() => rows.push(row(
                    graph.clone(),
                    "graph",
                    &target,
                    None,
                    None,
                    json!("unreadable"),
                    Some(&error.to_string()),
                )),
                Err(error) => return Err(error.into()),
            }
        }
        Ok(rows)
    })();
    match result {
        Ok(rows) => {
            if !rows.is_empty() {
                reply.summary = None;
            }
            for row in &rows {
                reply.human.push_str(&format!(
                    "{} {} {}\n",
                    row["kind"].as_str().unwrap_or(""),
                    row["id"].as_str().unwrap_or(""),
                    row["state"].as_str().unwrap_or("")
                ));
            }
            if let Some(output) = reply.result.data["rows"].as_array_mut() {
                output.retain(|existing| {
                    !rows.iter().any(|graph_row| {
                        graph_row["kind"] == "attempt"
                            && existing["kind"] == "attempt"
                            && graph_row["attempt_id"]
                                .as_str()
                                .and_then(|id| id.rsplit('/').next())
                                == existing["attempt_id"].as_str()
                    })
                });
                output.extend(rows);
            }
        }
        Err(error) => {
            reply.result.data["graph_error"] = json!(error.to_string());
            reply
                .human
                .push_str(&format!("graph status unavailable: {error}\n"));
        }
    }
}

/// Read through the host before constructing any journal reader in a lane.
pub(crate) fn unified_graph_output(
    command: &provenant_cli::Output,
    edge: &provenant_cli::run::Edge<'_>,
    globals: &provenant_cli::Globals,
    workers: &'static dyn WorkerObserver,
) -> Option<provenant_cli::run::Reply> {
    if !provenant_cli::fabric::in_lane() {
        return match read_factory(globals, workers) {
            Ok(factory) => {
                let budget = WaitBudget::cli(factory.clock.mono_now(), 55).ok()?;
                host_graph_output(command, edge, &factory, budget, &CancelToken::new())
            }
            Err(error) if missing_graph_context(error.as_ref()) => None,
            Err(error) => Some(graph_output_error(error)),
        };
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
        if command.path || command.host.is_some() {
            return Err(
                "lane reads require a graph or child ID and return content; omit --path and --host"
                    .into(),
            );
        }
        let input = json!({"op":"output", "id":target_text(&command.attempt.0),
            "render":true,"trace":command.trace,"part":part,"offset":command.offset,"tail":command.tail,"max_bytes":command.max_bytes});
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
    Some(result.unwrap_or_else(|error| {
        let message = error
            .downcast_ref::<ServiceError>()
            .map(|error| {
                let diagnostic = error.diagnostic();
                format!(
                    "{}: {}",
                    diagnostic.code,
                    diagnostic
                        .detail
                        .as_deref()
                        .unwrap_or("host graph read refused")
                )
            })
            .unwrap_or_else(|| error.to_string());
        graph_output_error(message)
    }))
}

fn returned_output_bytes(data: &Value) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    if let Some(outputs) = data["outputs"].as_array() {
        let mut bytes = Vec::new();
        for output in outputs {
            bytes.extend(returned_output_bytes(&output["data"])?);
        }
        return Ok(bytes);
    }
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

fn host_graph_output(
    command: &provenant_cli::Output,
    edge: &provenant_cli::run::Edge<'_>,
    factory: &Factory,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Option<provenant_cli::run::Reply> {
    let result = (|| -> Result<Option<provenant_cli::run::Reply>, Box<dyn std::error::Error>> {
        if command.trace {
            let input = json!({"op":"output","id":target_text(&command.attempt.0),"trace":true});
            let data = match factory.graph_document(input, budget, cancel) {
                Ok(data) => data,
                Err(FacadeError::NotFound) => return Ok(None),
                Err(error) => return Err(error.into()),
            };
            let human = data["events"]
                .as_array()
                .into_iter()
                .flatten()
                .map(serde_json::to_string)
                .collect::<Result<Vec<_>, _>>()?
                .join("\n")
                + "\n";
            return Ok(Some(provenant_cli::run::Reply {
                result: provenant_records::CommandResult::new(
                    "output",
                    provenant_records::Outcome::Ok,
                    data,
                ),
                human,
                summary: None,
                read_only: true,
            }));
        }
        let Some(target) = factory.graph_target(&target_text(&command.attempt.0))? else {
            return Ok(None);
        };
        let checkpoint = FileJournal::read_status_checkpoint(
            &factory.context.roots.state,
            factory.context.identity.project,
            &target.graph,
        )?
        .ok_or(FacadeError::NotFound)?;
        let failure_rows = if target.attempt.is_none() {
            let mut rows = status_rows(&graph_snapshot(checkpoint.clone()), &target);
            enrich_status_rows_within(&mut rows, &factory.context, &factory.clock, cancel, budget)?;
            localise_worktree_actions(factory, &target.graph, &mut rows);
            rows.retain(|row| {
                row["kind"] == "node" && matches!(row["state"].as_str(), Some("failed" | "blocked"))
            });
            rows
        } else {
            Vec::new()
        };
        let mut references = checkpoint
            .nodes
            .iter()
            .filter(|node| target.node.as_ref().is_none_or(|id| id == &node.id))
            .filter_map(|node| {
                if let Some(reference) = &target.attempt {
                    return node
                        .recorded_attempts()
                        .find(|attempt| attempt.attempt_ref.as_ref() == Some(reference))
                        .and_then(|attempt| attempt.attempt_ref.clone());
                }
                node.attempts
                    .iter()
                    .rev()
                    .find(|attempt| {
                        attempt.state == AttemptState::Terminal && attempt.attempt_ref.is_some()
                    })
                    .or_else(|| {
                        node.attempts
                            .iter()
                            .rev()
                            .find(|attempt| attempt.attempt_ref.is_some())
                    })
                    .and_then(|attempt| attempt.attempt_ref.clone())
            })
            .collect::<Vec<_>>();
        if target.attempt.is_none() {
            // Follow receiver-owned graphs through the same forward SSH status
            // call. Explicit attempt output remains the ordinary byte reader.
            let owner = BinaryRunOwner {
                factory,
                allow_remote: true,
                actor: None,
                chair: None,
            };
            let mut seen: std::collections::BTreeSet<_> = references.iter().cloned().collect();
            let mut expanded = std::collections::BTreeSet::new();
            let mut cursor = 0;
            while cursor < references.len() {
                let reference = &references[cursor];
                cursor += 1;
                if !expanded.insert(reference.clone()) {
                    continue;
                }
                let (host, attempt) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
                let host = host.parse().map_err(|_| FacadeError::StateCorrupt)?;
                if host == factory.context.identity.host {
                    continue;
                }
                let record = owner.snapshot(
                    AttemptTarget {
                        extra: Default::default(),
                        host: Some(host),
                        attempt: attempt.parse().map_err(|_| FacadeError::StateCorrupt)?,
                    },
                    budget,
                    cancel,
                )?;
                let peer_attempt = record.attempt_id;
                let mut subtree = record.extra.get("peer_subtree").cloned();
                let mut after = 0;
                while let Some(page) = subtree {
                    for reference in peer_output_references(&page)? {
                        // This page covers these local attempts; only another
                        // host needs a further status request.
                        if reference
                            .split_once('/')
                            .is_some_and(|(peer, _)| peer == host.to_string())
                        {
                            expanded.insert(reference.clone());
                        }
                        if seen.insert(reference.clone()) {
                            references.push(reference);
                        }
                    }
                    let Some(next) = page["next_node"].as_u64() else {
                        if page["omitted_nodes"].as_u64().is_some_and(|n| n > 0) {
                            return Err(FacadeError::StateCorrupt.into());
                        }
                        break;
                    };
                    if next <= after {
                        return Err(FacadeError::StateCorrupt.into());
                    }
                    after = next;
                    subtree = match owner.peer(
                        host,
                        PeerAction::Run {
                            extra: [("peer_subtree_after".into(), json!(next))].into(),
                            input: RunInput::Status {
                                extra: Default::default(),
                                target: AttemptTarget {
                                    extra: Default::default(),
                                    host: Some(host),
                                    attempt: peer_attempt,
                                },
                            },
                        },
                        budget,
                        cancel,
                    )? {
                        PeerResult::Run { data, .. } => match *data {
                            RunResult::Status { attempt, .. }
                                if attempt.attempt_id == peer_attempt =>
                            {
                                Some(
                                    attempt
                                        .extra
                                        .get("peer_subtree")
                                        .cloned()
                                        .ok_or(FacadeError::StateCorrupt)?,
                                )
                            }
                            _ => return Err(FacadeError::StateCorrupt.into()),
                        },
                        _ => return Err(FacadeError::StateCorrupt.into()),
                    };
                }
            }
        }
        if references.is_empty() {
            return Ok(Some(with_graph_failures(
                provenant_cli::run::Reply {
                    result: provenant_records::CommandResult::new(
                        "output",
                        provenant_records::Outcome::Ok,
                        json!({"graph_id":target.graph,"outputs":[]}),
                    ),
                    human: "no attempt output yet\n".into(),
                    summary: None,
                    read_only: true,
                },
                &failure_rows,
            )));
        }
        // A single target uses exactly the attempt reader, including binary
        // ranges, paths and destination writes.
        if references.len() == 1 {
            let mut selected = command.clone();
            selected.attempt = references[0]
                .parse()
                .map_err(|_| FacadeError::InvalidInput { detail: None })?;
            return Ok(Some(with_graph_failures(
                graph_attempt_output(selected, edge, factory, budget, cancel)?,
                &failure_rows,
            )));
        }
        let mut outputs = Vec::new();
        let mut human = String::new();
        let mut bytes = Vec::new();
        let mut outcome = provenant_records::Outcome::Ok;
        for reference in references {
            if cancel.is_cancelled() || budget.remaining(factory.clock.mono_now()).as_micros() == 0
            {
                return Err(FacadeError::Timeout.into());
            }
            let mut selected = command.clone();
            selected.to = None;
            selected.attempt = reference
                .parse()
                .map_err(|_| FacadeError::InvalidInput { detail: None })?;
            let reply = graph_attempt_output(selected, edge, factory, budget, cancel)?;
            if reply.result.status != provenant_records::Outcome::Ok {
                if command.to.is_some() {
                    return Err(
                        FacadeError::invalid(InvalidInputDetail::Field("target".into())).into(),
                    );
                }
                outcome = reply.result.status;
            }
            human.push_str(&reply.human);
            let encoded = &reply.result.data["bytes"];
            match (encoded["encoding"].as_str(), encoded["data"].as_str()) {
                (Some("utf8"), Some(text)) => bytes.extend_from_slice(text.as_bytes()),
                (Some("base64"), Some(text)) => bytes.extend(
                    provenant_fabric_runtime::handoff::decode_base64(text)
                        .ok_or(FacadeError::StateCorrupt)?,
                ),
                _ if command.to.is_some() => {
                    return Err(
                        FacadeError::invalid(InvalidInputDetail::Field("target".into())).into(),
                    );
                }
                _ => {}
            }
            outputs.push(reply.result);
        }
        if let Some(path) = &command.to {
            std::fs::write(path, &bytes)?;
            human = format!("wrote {} bytes to {}\n", bytes.len(), path.display());
        }
        Ok(Some(with_graph_failures(
            provenant_cli::run::Reply {
                result: provenant_records::CommandResult::new(
                    "output",
                    outcome,
                    json!({"graph_id":target.graph,"outputs":outputs}),
                ),
                human,
                summary: None,
                read_only: true,
            },
            &failure_rows,
        )))
    })();
    match result {
        Ok(reply) => reply,
        Err(error) => Some(provenant_cli::run::Reply {
            result: provenant_records::CommandResult::new(
                "output",
                provenant_records::Outcome::Error,
                json!({"error":error.to_string()}),
            ),
            human: format!("{error}\n"),
            summary: None,
            read_only: true,
        }),
    }
}

fn graph_attempt_output(
    command: provenant_cli::Output,
    edge: &provenant_cli::run::Edge<'_>,
    factory: &Factory,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<provenant_cli::run::Reply, FacadeError> {
    if cancel.is_cancelled() || budget.remaining(factory.clock.mono_now()).as_micros() == 0 {
        return Err(FacadeError::Timeout);
    }
    let reference = target_text(&command.attempt.0);
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
        let target = provenant_cli::Target {
            selection: None,
            run: None,
            task: None,
            attempt: Some(attempt),
        };
        return Ok(provenant_cli::run::output_from_session(
            command,
            edge,
            &session,
            target,
            &owner.call(cancel),
        ));
    }
    Ok(provenant_cli::run::output_from_host(
        command,
        host,
        attempt,
        |input| {
            owner.output(input, budget, cancel).map_err(|error| {
                provenant_cli::run::Fail::new(
                    provenant_records::Outcome::Error,
                    provenant_run::ErrorKind::Fabric,
                    error.to_string(),
                )
            })
        },
    ))
}

fn graph_output_error(error: impl std::fmt::Display) -> provenant_cli::run::Reply {
    provenant_cli::run::Reply {
        result: provenant_records::CommandResult::new(
            "output",
            provenant_records::Outcome::Error,
            json!({"error":error.to_string()}),
        ),
        human: format!("{error}\n"),
        summary: None,
        read_only: true,
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
/// MCP callers retain the owner's path-free diagnostic.
fn localise_worktree_actions(factory: &Factory, graph: &str, rows: &mut [Value]) {
    if !rows.iter().any(|row| {
        row["kind"] == "node"
            && row["state"] == "failed"
            && row["cause_code"] == "writer_binding_unavailable"
    }) {
        return;
    }
    let Ok(Some(execution)) = FileJournal::execution(
        &factory.context.roots.state,
        factory.context.identity.project,
        graph,
    ) else {
        return;
    };
    for row in rows.iter_mut().filter(|row| {
        row["kind"] == "node"
            && row["state"] == "failed"
            && row["cause_code"] == "writer_binding_unavailable"
    }) {
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
            .filter(|attempt| attempt.host.is_none() && attempt.placement.is_none())
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

/// Select one output per peer node, with the same terminal preference as
/// local graph output. References remain host-qualified across graph borders.
fn peer_output_references(subtree: &Value) -> Result<Vec<String>, FacadeError> {
    let checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint =
        serde_json::from_value(subtree["checkpoint"].clone())
            .map_err(|_| FacadeError::StateCorrupt)?;
    Ok(checkpoint
        .nodes
        .iter()
        .filter_map(|node| {
            node.attempts
                .iter()
                .rev()
                .find(|attempt| {
                    attempt.state == AttemptState::Terminal && attempt.attempt_ref.is_some()
                })
                .or_else(|| {
                    node.attempts
                        .iter()
                        .rev()
                        .find(|attempt| attempt.attempt_ref.is_some())
                })
                .and_then(|attempt| attempt.attempt_ref.clone())
        })
        .collect())
}

fn resolve_target(
    root: &provenant_core::StateRoot,
    project: provenant_core::ProjectId,
    selector: &str,
) -> Result<Option<GraphTarget>, FacadeError> {
    let ids = FileJournal::graph_ids(root, project).map_err(graph_journal_error)?;
    if ids.iter().any(|id| id == selector) {
        return Ok(Some(GraphTarget {
            graph: selector.into(),
            node: None,
            attempt: None,
        }));
    }
    let mut matches = Vec::new();
    for graph in ids {
        let checkpoint = match FileJournal::read_status_checkpoint(root, project, &graph) {
            Ok(Some(checkpoint)) => checkpoint,
            Ok(None) => continue,
            Err(error) if error.is_unreadable() => continue,
            Err(error) => return Err(graph_journal_error(error)),
        };
        for node in &checkpoint.nodes {
            if selector == node.id || selector == format!("{graph}/{}", node.id) {
                matches.push(GraphTarget {
                    graph: graph.clone(),
                    node: Some(node.id.clone()),
                    attempt: None,
                });
            }
            for attempt in node.recorded_attempts() {
                if let Some(reference) = &attempt.attempt_ref
                    && (selector == reference || reference.rsplit('/').next() == Some(selector))
                {
                    matches.push(GraphTarget {
                        graph: graph.clone(),
                        node: Some(node.id.clone()),
                        attempt: Some(reference.clone()),
                    });
                }
            }
        }
    }
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.pop()),
        _ => Err(FacadeError::Conflict),
    }
}

pub(crate) fn read_status_rows(
    selector: &str,
    edge: &provenant_cli::run::Edge<'_>,
) -> Result<Option<Vec<Value>>, FacadeError> {
    if provenant_cli::fabric::in_lane() {
        let value = provenant_cli::fabric::graph_relay::from_lane(
            &provenant_fabric_runtime::graph::relay::RelayOp::Read {
                input: json!({"op":"status","id":selector}),
            },
            &CancelToken::new(),
            std::time::Duration::from_secs(55),
        )
        .map_err(super::interface::relay_error)?;
        return Ok(Some(
            value["rows"]
                .as_array()
                .ok_or(FacadeError::StateCorrupt)?
                .clone(),
        ));
    }
    let context = match Context::open_read_only(edge.roots.clone(), edge.home, edge.cwd) {
        Ok(context) => context,
        Err(provenant_cli::fabric::EdgeError::Invalid) => return Ok(None),
        Err(_) => return Err(FacadeError::io("open graph context")),
    };
    let Some(target) = resolve_target(&context.roots.state, context.identity.project, selector)?
    else {
        return Ok(None);
    };
    let checkpoint = FileJournal::read_status_checkpoint(
        &context.roots.state,
        context.identity.project,
        &target.graph,
    )
    .map_err(graph_journal_error)?
    .ok_or(FacadeError::NotFound)?;
    let mut rows = status_rows(&graph_snapshot(checkpoint), &target);
    enrich_status_rows(
        &mut rows,
        &context,
        &SystemClock::new(),
        &CancelToken::new(),
    )?;
    Ok(Some(rows))
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
fn read_steering_stdin() -> std::io::Result<Vec<u8>> {
    provenant_os::sys::hook_io::read_stdin(4096, Instant::now() + std::time::Duration::from_secs(5))
}

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
            mcp_project: None,
            workers,
        };
        let budget = provenant_fabric_mcp::lane::host_budget(&factory.clock, within)?;
        if input["op"] != "output" || input["trace"] == true && input["render"] != true {
            return Ok(factory.graph_document(input.clone(), budget, cancel)?);
        }
        let part = match input["part"].as_str().unwrap_or("result") {
            "result" => provenant_cli::Part::Result,
            "prompt" => provenant_cli::Part::Prompt,
            "stderr" => provenant_cli::Part::Stderr,
            "events" => provenant_cli::Part::Events,
            "receipt" => provenant_cli::Part::Receipt,
            "transcript" => provenant_cli::Part::Transcript,
            _ => return Err("invalid output part".into()),
        };
        let command = provenant_cli::Output {
            attempt: input["id"].as_str().ok_or("missing target")?.parse()?,
            host: None,
            part,
            to: None,
            path: false,
            offset: input["offset"].as_u64().unwrap_or(0),
            tail: input["tail"].as_u64(),
            max_bytes: input["max_bytes"].as_u64().unwrap_or(65536).clamp(1, 65536),
            trace: input["trace"] == true,
        };
        let edge = provenant_cli::run::Edge {
            roots: &factory.context.roots,
            home: &factory.home,
            assets: &factory.assets,
            cwd: &factory.cwd,
        };
        let reply = host_graph_output(&command, &edge, &factory, budget, cancel)
            .ok_or("graph target missing")?;
        if input["render"] == true {
            Ok(json!({"result":reply.result,"human":reply.human}))
        } else {
            Ok(reply.result.data)
        }
    };
    read().map_err(|error| match error.downcast::<FacadeError>() {
        Ok(error) => ServiceError::Facade(*error),
        Err(_) => ServiceError::Facade(FacadeError::io("read calling lane graph")),
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

    fn checkpoint(graph: &str) -> GraphCheckpoint {
        serde_json::from_value(json!({"schema_version":1,"graph_id":graph,"plan_digest":"digest","nodes":[
            {"id":"parent","state":"running","worktree":null,"attempts":[
                {"launched":true,"key":{"graph_id":graph,"node_id":"parent","route_index":0},"route":{"alias":"senior"},"state":"running","attempt_ref":"11111111111111111111111111111111/22222222222222222222222222222222","launch_actor":"actor"}
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
            "schema_version":1,"attempt_id":id,"run_id":id,"project_id":id,
            "task_id":id,"operation_id":"operation","attempt_number":1,"revision":1,"generation":1,
            "requested_route":{"kind":"named","name":"senior"},
            "authority_digest":digest,"queued_at":"2026-10-29T00:00:00.000000Z",
            "state":{"state":"starting"},"usage":{},
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

    #[test]
    fn peer_output_follows_host_qualified_subtree_attempts() {
        let mut checkpoint = checkpoint("receiver");
        let parent = checkpoint.nodes[0].attempts[0].clone();
        let mut child = parent.clone();
        child.attempt_ref =
            Some("33333333333333333333333333333333/44444444444444444444444444444444".into());
        child.state = AttemptState::Terminal;
        checkpoint.nodes[1].attempts.push(child.clone());
        // Prefer the child's completed output over an unfinished retry.
        let mut retry = child.clone();
        retry.state = AttemptState::Running;
        retry.attempt_ref =
            Some("33333333333333333333333333333333/55555555555555555555555555555555".into());
        checkpoint.nodes[1].attempts.push(retry);
        let subtree = serde_json::to_value(graph_snapshot(checkpoint)).unwrap();
        assert_eq!(
            peer_output_references(&subtree).unwrap(),
            vec![parent.attempt_ref.unwrap(), child.attempt_ref.unwrap(),]
        );
        assert!(matches!(
            peer_output_references(&json!({})),
            Err(FacadeError::StateCorrupt)
        ));
    }

    #[test]
    fn unified_status_uses_one_row_contract_and_selects_attempts() {
        let run = graph_snapshot(checkpoint("g"));
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
        assert!(reading.contains(&format!("node g/parent running: {wait}")));
        assert_eq!(rows[1]["next"], "provenant status g/parent --wait");

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
        let mut many = rows.clone();
        for _ in 0..100 {
            many.extend(rows.clone());
        }
        let reading = render_graph_status(&many);
        assert!(reading.len() <= 2048);
        assert!(reading.starts_with("node g/parent failed"));
        assert!(reading.contains("AUTH_REQUIRED"));
        assert!(reading.ends_with("next: provenant retry g/parent\n"));
        assert!(reading.contains("more rows"));
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
        assert!(reading.starts_with("graph g complete"));
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
            mcp_project: None,
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
        assert_eq!(
            FileJournal::read_checkpoint(&roots.state, project, "g")?,
            Some(stopped.checkpoint)
        );
        assert_ne!(
            FileJournal::read_events(&roots.state, project, "g")?,
            events
        );
        Ok(())
    }

    #[test]
    fn pending_attempt_status_and_stop_by_attempt_id_keep_one_launch()
    -> Result<(), Box<dyn std::error::Error>> {
        if super::super::peer_tests::host_test(
            "fabric::unified::tests::pending_attempt_status_and_stop_by_attempt_id_keep_one_launch",
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
        // launch deferred. Both public attempt-ID spellings control that run.
        for qualified in [false, true] {
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
                mcp_project: None,
                workers: &FreeWorkers,
            };
            let cancel = CancelToken::new();
            let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
            let input = json!({
                "op":"run", "brief":"Inspect the parser", "operation_id":"stop-by-attempt",
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
            let selector = if qualified { reference } else { id };
            for selected in [graph, selector] {
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
            // Replaying the original operation advances the stopped graph. Its
            // unused second route must remain unused after the slot is freed.
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
    fn unified_resolver_finds_graph_nodes_attempts_and_refuses_ambiguous_nodes()
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
        assert_eq!(
            resolve_target(&roots.state, project, "g")?.unwrap().node,
            None
        );
        assert_eq!(
            resolve_target(&roots.state, project, "g/parent")?
                .unwrap()
                .node
                .as_deref(),
            Some("parent")
        );
        assert_eq!(
            resolve_target(&roots.state, project, "22222222222222222222222222222222")?
                .unwrap()
                .graph,
            "g"
        );
        assert!(matches!(
            resolve_target(&roots.state, project, "parent"),
            Err(FacadeError::Conflict)
        ));
        assert!(resolve_target(&roots.state, project, "missing")?.is_none());
        Ok(())
    }
}
