use super::*;

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
                            return Err(Box::new(target_fault(ResolveError::NotFound {
                                target: target.to_string(),
                                candidates: Vec::new(),
                            })));
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
            return Err(Box::new(target_fault(ResolveError::NotFound {
                target: target.to_string(),
                candidates: Vec::new(),
            })));
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
    let rows = local_status_rows(target, edge, session, cancel, |clock| {
        let left = wait_remaining(deadline, cancel)?;
        Ok(WaitBudget::up_to(
            clock.mono_now(),
            provenant_core::DurationMicros::from_micros(
                u64::try_from(left.as_micros()).unwrap_or(u64::MAX),
            ),
        ))
    })?;
    wait_remaining(deadline, cancel)?;
    Ok(rows)
}

/// The one final snapshot a wait takes after its deadline: the target's rows
/// from the local store, read without blocking. Remote sources get nothing
/// past the original budget, so a lane's relay read is refused and peer
/// enrichment sees an expired budget.
pub(crate) fn final_status_rows(
    target: &provenant_cli::Target,
    edge: &provenant_cli::run::Edge<'_>,
    session: &mut Option<Context>,
) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    if provenant_cli::fabric::in_lane() {
        return Err(FacadeError::Timeout.into());
    }
    local_status_rows(target, edge, session, &CancelToken::new(), |clock| {
        Ok(WaitBudget::up_to(
            clock.mono_now(),
            provenant_core::DurationMicros::from_micros(0),
        ))
    })
}

/// A target's rows from this host's graph checkpoint, enriched within the
/// peer budget `peer_budget` chooses once the local read is done.
fn local_status_rows(
    target: &provenant_cli::Target,
    edge: &provenant_cli::run::Edge<'_>,
    session: &mut Option<Context>,
    cancel: &CancelToken,
    peer_budget: impl FnOnce(&SystemClock) -> Result<WaitBudget, FacadeError>,
) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let context = match session {
        Some(context) => context,
        None => session.insert(read_context(edge)?.ok_or_else(|| {
            target_fault(ResolveError::NotFound {
                target: target.to_string(),
                candidates: Vec::new(),
            })
        })?),
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
    let budget = peer_budget(&clock)?;
    enrich_status_rows_within(&mut rows, context, &clock, cancel, budget)?;
    Ok(rows)
}

pub(super) fn missing_graph_context(error: &(dyn std::error::Error + 'static)) -> bool {
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
        let graph_directory = if input["op"] == "prepare_child" {
            // The admitted parent launch owns this path. A lane cannot choose
            // another repository or worktree for host-side object preparation.
            launch.cwd.as_path()
        } else {
            launch.graph_directory.as_deref().unwrap_or(&launch.cwd)
        };
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
        if input["op"] == "prepare_child" {
            return Ok(serde_json::to_value(
                factory.child_request(&input["input"])?,
            )?);
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
            final_result: false,
            path: false,
            offset: input["offset"].as_u64().unwrap_or(0),
            tail: input["tail"].as_u64(),
            tail_lines: input["tail_lines"].as_u64(),
            max_bytes: input["max_bytes"].as_u64().unwrap_or(65536).clamp(1, 65536),
        };
        let edge = provenant_cli::run::Edge {
            roots: &factory.context.roots,
            home: &factory.home,
            assets: &factory.assets,
            cwd: &factory.cwd,
        };
        let reply = host_graph_output(
            &command,
            &edge,
            &factory,
            budget,
            cancel,
            input["full"].as_bool() == Some(true),
        )?;
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
