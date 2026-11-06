//! Read and control graph targets through the public command surface.
use super::*;
use provenant_fabric_runtime::graph::{AttemptState, FileJournal, GraphRun, NodeState};
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

    fn stop_target(
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
        let _ = parent;
        Err(FacadeError::invalid(
            "stopping a graph node subtree with immutable attempt history requires runtime support; stop the whole graph instead",
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
    json!({"id":id,"kind":kind,"graph_id":target.graph,"node_id":node,"attempt_id":attempt,"state":state,"reason":reason})
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
                .or(run.checkpoint.pause_reason.as_deref()),
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
        for attempt in &node.attempts {
            let Some(reference) = &attempt.attempt_ref else {
                continue;
            };
            if target.attempt.as_ref().is_some_and(|id| id != reference) {
                continue;
            }
            rows.push(row(
                reference.clone(),
                "attempt",
                target,
                Some(&node.id),
                Some(reference),
                json!(attempt.state),
                None,
            ));
        }
    }
    rows
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
        node: bool,
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
    let name = match command {
        GraphCommand::Status { .. } => "status",
        GraphCommand::Stop { .. } => "stop",
        GraphCommand::Retry { .. } => "retry",
    };
    let result = (|| -> Result<Option<(Value, String, bool)>, Box<dyn std::error::Error>> {
        let factory = if matches!(command, GraphCommand::Status { .. }) {
            match read_factory(globals, workers) {
                Ok(factory) => factory,
                Err(error) if missing_graph_context(error.as_ref()) => return Ok(None),
                Err(error) => return Err(error),
            }
        } else {
            factory(globals, workers)?
        };
        let Some(target) = factory.graph_target(selector)? else {
            return Ok(None);
        };
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 55)?;
        let status = || {
            factory.graph(
                GraphInput::Status {
                    extra: Default::default(),
                    id: target.graph.clone(),
                },
                budget,
                &cancel,
            )
        };
        let mut timed_out = false;
        let run = match &command {
            GraphCommand::Status {
                wait: Some(seconds),
            } => {
                let observed =
                    wait_graph(&factory.clock, &factory.backoff, *seconds, &cancel, status)?;
                timed_out =
                    observed.status == provenant_fabric_runtime::graph::GraphStatus::Pending;
                observed
            }
            GraphCommand::Stop { reason, wait } => {
                let stopped = factory.stop_target(&target, reason.clone(), budget, &cancel)?;
                if let Some(seconds) = wait {
                    let wait_budget = WaitBudget::cli(factory.clock.mono_now(), *seconds)?;
                    loop {
                        let observed = status()?;
                        let live = observed
                            .checkpoint
                            .nodes
                            .iter()
                            .flat_map(|node| &node.attempts)
                            .any(|attempt| attempt.state != AttemptState::Terminal);
                        let remaining = wait_budget.remaining(factory.clock.mono_now());
                        if !live || remaining.as_micros() == 0 {
                            timed_out = live;
                            break observed;
                        }
                        provenant_core::Pause::wait(
                            &factory.backoff,
                            provenant_core::DurationMicros::from_micros(
                                250_000.min(remaining.as_micros()),
                            ),
                            &cancel,
                        );
                    }
                } else {
                    stopped
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
                    return Err(FacadeError::invalid("graph retry accepts --extend; prompt source and provider continuation options apply only to attempts").into());
                }
                let before = status()?;
                if !before.checkpoint.cancelled
                    && (before.status == provenant_fabric_runtime::graph::GraphStatus::Complete
                        || target.node.as_ref().is_some_and(|id| {
                            before.checkpoint.nodes.iter().any(|node| {
                                &node.id == id
                                    && !matches!(
                                        node.state,
                                        NodeState::Pending | NodeState::Running
                                    )
                                    && node.expansion_pause.is_none()
                            })
                        }))
                {
                    return Err(FacadeError::invalid("retrying a settled graph node requires node restart support; its recorded attempts were preserved").into());
                }
                if *node || brief.is_some() {
                    return Err(FacadeError::invalid(
                        "node redo and brief replacement require a graph patch",
                    )
                    .into());
                }
                let resumed = factory.graph(
                    GraphInput::Resume {
                        extra: Default::default(),
                        document: target.graph.clone().into(),
                        extend: extend.to_vec(),
                    },
                    budget,
                    &cancel,
                )?;
                if let Some(seconds) = wait {
                    let observed =
                        wait_graph(&factory.clock, &factory.backoff, *seconds, &cancel, status)?;
                    timed_out =
                        observed.status == provenant_fabric_runtime::graph::GraphStatus::Pending;
                    observed
                } else {
                    resumed
                }
            }
            _ => status()?,
        };
        let rows = status_rows(&run, &target);
        let human = rows
            .iter()
            .map(|row| {
                format!(
                    "{} {} {}\n",
                    row["kind"].as_str().unwrap_or(""),
                    row["id"].as_str().unwrap_or(""),
                    row["state"].as_str().unwrap_or("")
                )
            })
            .collect::<String>();
        let data = json!({"rows":rows,"timed_out":timed_out});
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
    let result = (|| -> Result<Option<Value>, Box<dyn std::error::Error>> {
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
        let references = checkpoint
            .nodes
            .iter()
            .filter(|node| target.node.as_ref().is_none_or(|id| id == &node.id))
            .flat_map(|node| &node.attempts)
            .filter(|attempt| attempt.state != AttemptState::Terminal)
            .filter_map(|attempt| attempt.attempt_ref.as_ref())
            .filter(|reference| target.attempt.as_ref().is_none_or(|id| id == *reference))
            .collect::<Vec<_>>();
        if references.is_empty() {
            return Err(FacadeError::invalid("target has no live attempt to steer").into());
        }
        let text = provenant_os::sys::hook_io::read_stdin(
            4096,
            Instant::now() + std::time::Duration::from_secs(5),
        )?;
        let text = String::from_utf8(text)?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 55)?;
        let mut facade = BoundFacade { factory: &factory };
        let mut receipts = Vec::new();
        for reference in references {
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
                text: text.clone(),
                operation_id,
            };
            receipts.push(facade.inbox_owner(input, Some(host.into()), budget, &cancel)?);
        }
        Ok(Some(json!({"receipts":receipts})))
    })();
    match result {
        Ok(None) => None,
        Ok(Some(data)) => Some(crate::emit(
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
                    rows.extend(status_rows(&graph_snapshot(checkpoint), &target))
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

pub(crate) fn unified_graph_output(
    command: &provenant_cli::Output,
    edge: &provenant_cli::run::Edge<'_>,
    globals: &provenant_cli::Globals,
    workers: &'static dyn WorkerObserver,
) -> Option<provenant_cli::run::Reply> {
    let result = (|| -> Result<Option<provenant_cli::run::Reply>, Box<dyn std::error::Error>> {
        let factory = match read_factory(globals, workers) {
            Ok(factory) => factory,
            Err(error) if missing_graph_context(error.as_ref()) => return Ok(Default::default()),
            Err(error) => return Err(error),
        };
        let Some(target) = factory.graph_target(&target_text(&command.attempt.0))? else {
            return Ok(None);
        };
        let checkpoint = FileJournal::read_status_checkpoint(
            &factory.context.roots.state,
            factory.context.identity.project,
            &target.graph,
        )?
        .ok_or(FacadeError::NotFound)?;
        if command.trace {
            let events = FileJournal::read_events(
                &factory.context.roots.state,
                factory.context.identity.project,
                &target.graph,
            )?;
            let events = events
                .into_iter()
                .filter(|event| {
                    target.node.as_ref().is_none_or(|node| {
                        provenant_records::CanonicalV1::record_digest("fabric-graph-event", 1, node)
                            .is_ok_and(|digest| event.subject == digest.to_string())
                    })
                })
                .collect::<Vec<_>>();
            let human = events
                .iter()
                .map(serde_json::to_string)
                .collect::<Result<Vec<_>, _>>()?
                .join("\n")
                + "\n";
            return Ok(Some(provenant_cli::run::Reply {
                result: provenant_records::CommandResult::new(
                    "output",
                    provenant_records::Outcome::Ok,
                    json!({"graph_id": target.graph,"events":events}),
                ),
                human,
                summary: None,
                read_only: true,
            }));
        }
        let references = checkpoint
            .nodes
            .iter()
            .filter(|node| target.node.as_ref().is_none_or(|id| id == &node.id))
            .filter_map(|node| {
                if let Some(reference) = &target.attempt {
                    return node
                        .attempts
                        .iter()
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
        if references.is_empty() {
            return Ok(Some(provenant_cli::run::Reply {
                result: provenant_records::CommandResult::new(
                    "output",
                    provenant_records::Outcome::Ok,
                    json!({"graph_id":target.graph,"outputs":[]}),
                ),
                human: "no attempt output yet\n".into(),
                summary: None,
                read_only: true,
            }));
        }
        // A single target uses exactly the attempt reader, including binary
        // ranges, paths and destination writes.
        if references.len() == 1 {
            let mut selected = command.clone();
            selected.attempt = references[0].parse().map_err(FacadeError::invalid)?;
            return Ok(Some(provenant_cli::run::output(selected, edge)));
        }
        let mut outputs = Vec::new();
        let mut human = String::new();
        let mut bytes = Vec::new();
        let mut outcome = provenant_records::Outcome::Ok;
        for reference in references {
            let mut selected = command.clone();
            selected.to = None;
            selected.attempt = reference.parse().map_err(FacadeError::invalid)?;
            let reply = provenant_cli::run::output(selected, edge);
            if reply.result.status != provenant_records::Outcome::Ok {
                if command.to.is_some() {
                    return Err(FacadeError::invalid(
                        "selected output could not be read; destination was preserved",
                    )
                    .into());
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
                    return Err(FacadeError::invalid(
                        "--to requires byte output; destination was preserved",
                    )
                    .into());
                }
                _ => {}
            }
            outputs.push(reply.result);
        }
        if let Some(path) = &command.to {
            std::fs::write(path, &bytes)?;
            human = format!("wrote {} bytes to {}\n", bytes.len(), path.display());
        }
        Ok(Some(provenant_cli::run::Reply {
            result: provenant_records::CommandResult::new(
                "output",
                outcome,
                json!({"graph_id":target.graph,"outputs":outputs}),
            ),
            human,
            summary: None,
            read_only: true,
        }))
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
            for attempt in &node.attempts {
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
    Ok(Some(status_rows(&graph_snapshot(checkpoint), &target)))
}

#[cfg(test)]
mod tests {
    use super::*;
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
    }

    #[test]
    fn unsupported_subtree_stop_preserves_the_checkpoint_and_trace()
    -> Result<(), Box<dyn std::error::Error>> {
        struct FreeWorkers;
        impl WorkerObserver for FreeWorkers {
            fn observe_roots(
                &self,
                roots: &[PathBuf],
            ) -> std::collections::BTreeMap<PathBuf, provenant_confinement::worktrees::WorkerState>
            {
                roots
                    .iter()
                    .cloned()
                    .map(|path| (path, provenant_confinement::worktrees::WorkerState::Free))
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
        let before = checkpoint("g");
        journal.save(&before)?;
        drop(journal);
        let events = FileJournal::read_events(&roots.state, project, "g")?;
        let error = factory
            .stop_target(
                &GraphTarget {
                    graph: "g".into(),
                    node: Some("parent".into()),
                    attempt: None,
                },
                None,
                budget,
                &cancel,
            )
            .unwrap_err();
        assert!(matches!(error, FacadeError::InvalidInput { .. }));
        assert_eq!(
            FileJournal::read_checkpoint(&roots.state, project, "g")?,
            Some(before)
        );
        assert_eq!(
            FileJournal::read_events(&roots.state, project, "g")?,
            events
        );
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

fn missing_graph_context(error: &(dyn std::error::Error + 'static)) -> bool {
    matches!(
        error.downcast_ref::<provenant_cli::fabric::EdgeError>(),
        Some(provenant_cli::fabric::EdgeError::Invalid)
    )
}
