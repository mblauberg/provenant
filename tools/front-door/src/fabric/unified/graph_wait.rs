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
                        let error = super::super::interface::relay_error(error);
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
        .map_err(super::super::interface::relay_error)?;
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

/// The checkpoint, resolved target, context and peer budget behind one local
/// status read. The context stays in the caller's session so a polling waiter
/// resolves the repository only once.
fn local_status_parts<'a>(
    target: &provenant_cli::Target,
    edge: &provenant_cli::run::Edge<'_>,
    session: &'a mut Option<Context>,
    peer_budget: impl FnOnce(&SystemClock) -> Result<WaitBudget, FacadeError>,
) -> Result<
    (
        provenant_fabric_runtime::graph::GraphRun,
        GraphTarget,
        &'a Context,
        WaitBudget,
    ),
    Box<dyn std::error::Error>,
> {
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
    let clock = SystemClock::new();
    let budget = peer_budget(&clock)?;
    Ok((graph_snapshot(checkpoint), target, context, budget))
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
    let (run, target, context, budget) = local_status_parts(target, edge, session, peer_budget)?;
    let mut rows = status_rows(&run, &target);
    let clock = SystemClock::new();
    enrich_status_rows_within(&mut rows, context, &clock, cancel, budget)?;
    Ok(rows)
}

/// Re-drive the journal-locked advance when a terminal remote attempt says the
/// peer's work is done but this host's checkpoint is still unsettled. The read
/// keeps the peer error as a pending observation; only a successful terminal
/// reading authorises an advance. `advance` is injected so the decision is
/// deterministic and the call can be pinned without a tick or a peer process.
pub(crate) fn recover_after_remote_terminal(
    run: &provenant_fabric_runtime::graph::GraphRun,
    target: &GraphTarget,
    rows: &mut Vec<Value>,
    local_host: provenant_core::HostId,
    peer_error: Option<FacadeError>,
    advance: impl FnOnce() -> Result<provenant_fabric_runtime::graph::GraphRun, FacadeError>,
) -> Result<bool, FacadeError> {
    let remote_rows: Vec<Value> = rows
        .iter()
        .filter(|row| {
            row["kind"] == "attempt"
                && row["attempt_id"].as_str().is_some_and(|reference| {
                    reference.split_once('/').is_some_and(|(owner, _)| {
                        owner.parse::<provenant_core::HostId>() != Ok(local_host)
                    })
                })
        })
        .cloned()
        .collect();
    if remote_rows.is_empty() {
        return Ok(false);
    }
    let terminal =
        super::super::interface::observation::remote_status_reading(&remote_rows, peer_error)?;
    let advanced =
        super::super::interface::observation::wake_after_remote_terminal(run, terminal, advance)?;
    // Only a settled checkpoint replaces the peer-enriched reading: a partial
    // advance that merely relaunched work is reread on the waiter's next poll,
    // keeping this reading's digests and peer facts intact.
    let settled = &advanced != run
        && advanced
            .checkpoint
            .nodes
            .iter()
            .all(|node| node.state.settled());
    if settled {
        *rows = status_rows(&advanced, target);
    }
    Ok(settled)
}

/// The wait verb's graph read: same rows as `read_status_rows`, plus the
/// recovery a terminal remote reading justifies. A wait is still a reader of
/// work; the advance only retries a settlement the peer has already reported,
/// under the graph journal's own lock. Inside a lane the relay owns the read
/// and the peer owns advancement, so this path is host-only.
pub(crate) fn read_status_rows_with_recovery(
    target: &provenant_cli::Target,
    edge: &provenant_cli::run::Edge<'_>,
    session: &mut Option<Context>,
    deadline: std::time::Instant,
    cancel: &CancelToken,
    workers: &'static dyn provenant_confinement::worktrees::WorkerObserver,
) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    wait_remaining(deadline, cancel)?;
    if provenant_cli::fabric::in_lane() {
        return read_status_rows(target, edge, session, deadline, cancel);
    }
    let (run, resolved, context, budget) = local_status_parts(target, edge, session, |clock| {
        let left = wait_remaining(deadline, cancel)?;
        Ok(WaitBudget::up_to(
            clock.mono_now(),
            provenant_core::DurationMicros::from_micros(
                u64::try_from(left.as_micros()).unwrap_or(u64::MAX),
            ),
        ))
    })?;
    let clock = SystemClock::new();
    let mut rows = status_rows(&run, &resolved);
    // Peer failures stay in the reading as a pending observation; the recovery
    // helper turns a successful terminal reading into an advance.
    let peer_error = observe_status_rows_within(&mut rows, context, &clock, cancel, budget)?;
    let recovered = recover_after_remote_terminal(
        &run,
        &resolved,
        &mut rows,
        context.identity.host,
        peer_error,
        || {
            let factory = Factory {
                project_attribution: crate::usage::attribution_enabled(&context.roots),
                context: Context::open(edge.roots.clone(), edge.home, edge.cwd)
                    .map_err(|_| FacadeError::io("open graph recovery context"))?,
                clock: SystemClock::new(),
                backoff: provenant_core::ThreadPause,
                executor: SystemExecutor::new(1)
                    .map_err(|_| FacadeError::io("create graph recovery executor"))?,
                home: edge.home.to_path_buf(),
                cwd: edge.cwd.to_path_buf(),
                assets: edge.assets.to_path_buf(),
                host_launch: None,
                workers,
            };
            factory.graph(
                GraphInput::Resume {
                    extra: Default::default(),
                    document: run.checkpoint.graph_id.clone().into(),
                    extend: Vec::new(),
                },
                budget,
                cancel,
            )
        },
    );
    if matches!(recovered, Ok(true)) {
        // A successful advance retires the transport failure held from an
        // earlier attempt to settle the same graph.
        if FileJournal::read_advance_note(
            &context.roots.state,
            context.identity.project,
            &resolved.graph,
        )
        .is_ok_and(|note| note.is_some())
        {
            let _ = FileJournal::set_advance_note(
                &context.roots.state,
                context.identity.project,
                &resolved.graph,
                None,
            );
        }
    }
    wait_remaining(deadline, cancel)?;
    Ok(rows)
}

pub(super) fn missing_graph_context(error: &(dyn std::error::Error + 'static)) -> bool {
    matches!(
        error.downcast_ref::<provenant_cli::fabric::EdgeError>(),
        Some(provenant_cli::fabric::EdgeError::Invalid)
    )
}

/// Resolve the graph a queue item is bound to. A reader never creates the
/// graph: the item's claimed graph is the subject a wake can advance.
fn queue_item_graph(
    state: &provenant_core::StateRoot,
    project: provenant_core::ProjectId,
    queue: &str,
    item: &str,
) -> Option<String> {
    use super::super::queue_registry::QueueRegistry;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let registry = QueueRegistry::open(state.as_path()).ok()?;
    let queues = registry
        .queues(WaitBudget::nonblocking(clock.mono_now()), &cancel, &clock)
        .ok()?;
    queues
        .into_iter()
        .find(|record| record.project == project.to_string() && record.queue_id == queue)?
        .items
        .into_iter()
        .find(|candidate| candidate.identity.item == item)
        .map(|candidate| candidate.graph_id)
}

/// A `wait --pr N` reader also wakes the queue item bound to that PR, so an
/// adopted item advances even before its graph exists. Best effort: a reader
/// without a bound item stays a pure snapshot.
pub(crate) fn wake_pr_item(
    edge: &provenant_cli::run::Edge<'_>,
    project: provenant_core::ProjectId,
    pr: u64,
    deadline: provenant_core::Deadline,
    cancel: &CancelToken,
    workers: &'static dyn provenant_confinement::worktrees::WorkerObserver,
) -> crate::fabric::progress::WakeOutcome {
    use super::super::queue_registry::QueueRegistry;
    use crate::fabric::progress::WakeSubject;
    let clock = SystemClock::new();
    let registry = match QueueRegistry::open(edge.roots.state.as_path()) {
        Ok(registry) => registry,
        Err(_) => return crate::fabric::progress::WakeOutcome::Idle,
    };
    let queues = match registry.queues(WaitBudget::nonblocking(clock.mono_now()), cancel, &clock) {
        Ok(queues) => queues,
        Err(_) => return crate::fabric::progress::WakeOutcome::Idle,
    };
    let graph_id = queues
        .into_iter()
        .filter(|record| record.project == project.to_string())
        .flat_map(|record| record.items)
        .find_map(|item| {
            let adopted = item.adoption.as_ref()?;
            (adopted["pull_request"].as_u64() == Some(pr)).then_some(item.graph_id)
        });
    let Some(graph_id) = graph_id else {
        return crate::fabric::progress::WakeOutcome::Idle;
    };
    let now = clock.mono_now();
    let budget = WaitBudget::up_to(
        now,
        provenant_core::DurationMicros::from_micros(
            deadline.remaining(now).as_micros().min(u64::MAX as u128) as u64,
        ),
    );
    let factory = Factory {
        project_attribution: crate::usage::attribution_enabled(edge.roots),
        context: match Context::open(edge.roots.clone(), edge.home, edge.cwd) {
            Ok(context) => context,
            Err(_) => {
                return crate::fabric::progress::WakeOutcome::Skipped {
                    code: "WAKE_PR_CONTEXT_UNAVAILABLE".into(),
                };
            }
        },
        clock: SystemClock::new(),
        backoff: ThreadPause,
        executor: match SystemExecutor::new(1) {
            Ok(executor) => executor,
            Err(_) => {
                return crate::fabric::progress::WakeOutcome::Skipped {
                    code: "WAKE_PR_EXECUTOR_UNAVAILABLE".into(),
                };
            }
        },
        home: edge.home.to_path_buf(),
        cwd: edge.cwd.to_path_buf(),
        assets: edge.assets.to_path_buf(),
        host_launch: None,
        workers,
    };
    wake_in_host(
        &factory,
        budget,
        cancel,
        WakeSubject::Graph {
            project,
            id: graph_id,
        },
        crate::fabric::progress::WakeReason::PullRequest,
    )
}

/// Host-side wake. The reader owns the subject's host context, so it drives
/// the shared coordinator with a subject-scoped advance that reuses the
/// graph's existing recovery path. The frame grants no mutation authority.
fn wake_in_host(
    factory: &Factory,
    budget: WaitBudget,
    cancel: &CancelToken,
    subject: crate::fabric::progress::WakeSubject,
    reason: crate::fabric::progress::WakeReason,
) -> crate::fabric::progress::WakeOutcome {
    use crate::fabric::progress::{WakeClaim, WakeOutcome, WakeReason, WakeSource, wake_subject};
    use provenant_fabric_runtime::graph::{FileJournal, patch};

    let project = factory.context.identity.project;
    let graph_id = match &subject {
        crate::fabric::progress::WakeSubject::Graph { id, .. } => id.clone(),
        crate::fabric::progress::WakeSubject::QueueItem { queue, item, .. } => {
            match queue_item_graph(&factory.context.roots.state, project, queue, item) {
                Some(graph) => graph,
                None => {
                    return WakeOutcome::Skipped {
                        code: "WAKE_QUEUE_ITEM_NO_GRAPH".into(),
                    };
                }
            }
        }
    };

    struct HostWake<'a> {
        factory: &'a Factory,
        budget: WaitBudget,
        cancel: &'a CancelToken,
        graph_id: String,
        before: usize,
    }
    impl WakeSource for HostWake<'_> {
        fn reconcile(
            &mut self,
            _subject: &crate::fabric::progress::WakeSubject,
            _reason: WakeReason,
        ) -> Result<(), WakeOutcome> {
            Ok(())
        }
        fn claim(
            &mut self,
            _subject: &crate::fabric::progress::WakeSubject,
            _deadline: provenant_core::Deadline,
        ) -> WakeClaim {
            let root = &self.factory.context.roots.state;
            let project = self.factory.context.identity.project;
            // A held journal lock is the single-flight lease: report Busy now.
            if let Err(error) = FileJournal::try_open(root, project, &self.graph_id) {
                return if error.is_lock_timeout() {
                    WakeClaim::Busy {
                        holder: self.graph_id.clone(),
                    }
                } else {
                    WakeClaim::Skipped {
                        code: "WAKE_JOURNAL_UNAVAILABLE".into(),
                    }
                };
            }
            match FileJournal::read_checkpoint(root, project, &self.graph_id) {
                Ok(Some(checkpoint)) => {
                    self.before = checkpoint
                        .nodes
                        .iter()
                        .filter(|node| node.state.settled())
                        .count();
                    if checkpoint.nodes.iter().all(|node| node.state.settled()) {
                        WakeClaim::Idle
                    } else {
                        WakeClaim::Ready {
                            revision: patch::revision(&checkpoint),
                        }
                    }
                }
                Ok(None) => WakeClaim::Idle,
                Err(_) => WakeClaim::Skipped {
                    code: "WAKE_CHECKPOINT_UNREADABLE".into(),
                },
            }
        }
        fn live_revision(&self, _subject: &crate::fabric::progress::WakeSubject) -> u64 {
            match FileJournal::read_checkpoint(
                &self.factory.context.roots.state,
                self.factory.context.identity.project,
                &self.graph_id,
            ) {
                Ok(Some(checkpoint)) => patch::revision(&checkpoint),
                _ => u64::MAX,
            }
        }
        fn advance(
            &mut self,
            _subject: &crate::fabric::progress::WakeSubject,
            _reason: WakeReason,
            _deadline: provenant_core::Deadline,
        ) -> WakeOutcome {
            match self.factory.graph(
                GraphInput::Resume {
                    extra: Default::default(),
                    document: self.graph_id.clone().into(),
                    extend: Vec::new(),
                },
                self.budget,
                self.cancel,
            ) {
                Ok(next) => {
                    let after = next
                        .checkpoint
                        .nodes
                        .iter()
                        .filter(|node| node.state.settled())
                        .count();
                    WakeOutcome::Advanced {
                        transitions: after.saturating_sub(self.before),
                    }
                }
                Err(error) => WakeOutcome::Skipped {
                    code: error.code().to_owned(),
                },
            }
        }
    }

    wake_subject(
        subject,
        reason,
        budget.deadline(),
        &mut HostWake {
            factory,
            budget,
            cancel,
            graph_id,
            before: 0,
        },
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
        // Wake is a read-class frame. The relay has already applied the
        // sibling-session authorisation; the host runs one custody-checked
        // advance and grants no resume, retry or landing authority.
        if input["op"] == "wake" {
            let wire: provenant_cli::fabric::graph_relay::WakeSubjectRef =
                serde_json::from_value(input["subject"].clone())?;
            let subject = match wire {
                provenant_cli::fabric::graph_relay::WakeSubjectRef::Graph { project, id } => {
                    crate::fabric::progress::WakeSubject::Graph { project, id }
                }
                provenant_cli::fabric::graph_relay::WakeSubjectRef::QueueItem {
                    project,
                    queue,
                    item,
                } => crate::fabric::progress::WakeSubject::QueueItem {
                    project,
                    queue,
                    item,
                },
            };
            return Ok(wake_in_host(
                &factory,
                budget,
                cancel,
                subject,
                crate::fabric::progress::WakeReason::Lane,
            )
            .to_value());
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
            common: Default::default(),
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
