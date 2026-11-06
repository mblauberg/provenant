use super::*;

pub fn hosts(globals: &provenant_cli::Globals, workers: &'static dyn WorkerObserver) -> u8 {
    let result = (|| -> Result<Vec<HostHealth>, Box<dyn std::error::Error>> {
        let factory = factory(globals, workers)?;
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        Ok(BoundFacade { factory: &factory }.hosts(
            HostsInput::Hosts {
                extra: Default::default(),
                after: None,
                limit: 100,
            },
            budget,
            &CancelToken::new(),
        )?)
    })();
    match result {
        Ok(data) => {
            let human = provenant_cli::terse(&serde_json::to_value(&data).unwrap_or_default());
            crate::emit(
                &provenant_records::CommandResult::new(
                    "hosts",
                    provenant_records::Outcome::Ok,
                    data,
                ),
                globals,
                &human,
                true,
            )
        }
        Err(error) => crate::fail(
            "hosts",
            provenant_records::Outcome::Error,
            "io",
            &error.to_string(),
            globals,
        ),
    }
}
fn peer_read_only(action: &PeerAction) -> bool {
    use provenant_fabric_runtime::inbox::InboxInput;
    match action {
        PeerAction::ConsoleBoard { .. }
        | PeerAction::ConsoleTree { .. }
        | PeerAction::ConsoleDetail { .. }
        | PeerAction::ConsoleInbox { .. }
        | PeerAction::Doctor { .. } => true,
        PeerAction::Inbox { input, .. } => !matches!(
            input,
            InboxInput::Answer {
                operation_id: Some(_),
                ..
            }
        ),
        _ => false,
    }
}

// Peer frames use bounded real-time stdin and stdout transport deadlines.
#[allow(clippy::disallowed_methods)]
pub fn peer(
    globals: &provenant_cli::Globals,
    in_process: bool,
    workers: &'static dyn WorkerObserver,
) -> u8 {
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        // A request names its own agent, so a lane could act as any actor.
        if provenant_cli::fabric::in_lane() {
            return Err(CoordinationError::LaneMutation.into());
        }
        let bytes = provenant_os::sys::hook_io::read_stdin(
            MAX_FRAME,
            Instant::now() + std::time::Duration::from_secs(30),
        )?;
        if let Some(refusal) = protocol_refusal(&bytes)? {
            let mut reply = serde_json::to_vec(&refusal)?;
            reply.push(b'\n');
            provenant_os::sys::hook_io::write_stdout(
                &reply,
                Instant::now() + std::time::Duration::from_secs(30),
            )?;
            return Ok(());
        }
        let host = crate::roots::resolve(
            globals,
            crate::roots::Workspace::Home,
            crate::roots::Product::Required,
        )?;
        let home = host.home;
        if !in_process {
            let state = host.roots.state;
            // Only an unreachable agent falls back; a sent request is never repeated.
            let reply =
                match provenant_fabric_runtime::peer_agent::forward(state.as_path(), &bytes)? {
                    provenant_fabric_runtime::peer_agent::Forward::Replied(reply) => Some(reply),
                    provenant_fabric_runtime::peer_agent::Forward::Unreachable => None,
                };
            if let Some(reply) = reply {
                provenant_os::sys::hook_io::write_stdout(
                    &reply,
                    Instant::now() + std::time::Duration::from_secs(30),
                )?;
                return Ok(());
            }
        }
        let request = decode_request(&bytes)?;
        let common = request.project.resolve(&home)?;
        if provenant_os::fs::canonicalize(&common)? != common {
            return Err(PeerError::Project.into());
        }
        let cwd = if common.file_name() == Some(std::ffi::OsStr::new(".git")) {
            common.parent().ok_or(PeerError::Project)?.to_path_buf()
        } else {
            common
        };
        let resolved = crate::roots::resolve(
            globals,
            crate::roots::Workspace::At(&cwd),
            crate::roots::Product::Required,
        )?;
        let assets = resolved.assets;
        // An answer without an operation ID is refused below, after peer validation,
        // and must not initialise writable project state on its way there.
        let mut context = if peer_read_only(&request.action) {
            Context::open_read_only(resolved.roots, &home, &cwd)?
        } else {
            Context::open(resolved.roots, &home, &cwd)?
        };
        request.validate(context.identity.host, &context.config, &home)?;
        if context.identity.federation.as_ref().map(|p| &p.key) != Some(&request.project.key) {
            return Err(PeerError::Project.into());
        }
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let seconds = if let PeerAction::SessionHandoff { budget_seconds, .. } = &request.action {
            provenant_fabric_runtime::handoff::session::peer_seconds(
                *budget_seconds,
                context.config.limits.peer_seconds,
            )
        } else {
            30
        };
        let budget = WaitBudget::cli(clock.mono_now(), seconds)?;
        let backoff = ThreadPause;
        context.identity.inbox =
            provenant_fabric_runtime::identity::Address::new(request.source_host, &request.agent)?;
        let peer_factory = Factory {
            project_attribution: crate::usage::attribution_enabled(&context.roots),
            context,
            clock: SystemClock::new(),
            backoff: ThreadPause,
            executor: SystemExecutor::new(4)?,
            home: home.clone(),
            cwd: cwd.clone(),
            assets,
            host_launch: None,
            mcp_project: None,
            workers,
        };
        let context = &peer_factory.context;
        let mut owner = BinaryRunOwner {
            factory: &peer_factory,
            allow_remote: false,
            actor: None,
            chair: None,
        };
        let result = match &request.action {
            PeerAction::Inbox { input, .. } => {
                use provenant_fabric_runtime::inbox::{InboxInput, InboxReply};
                if matches!(
                    input,
                    InboxInput::Close { .. }
                        | InboxInput::Acknowledge { .. }
                        | InboxInput::Steer { .. }
                        | InboxInput::ChairMessage { .. }
                ) {
                    PeerResult::Inbox {
                        data: InboxReply::Error {
                            extra: Default::default(),
                            code: "OWNER_LOCAL_ONLY".into(),
                            message: format!(
                                "inbox owner {}: run this operation on the owner host or from the owning chair session",
                                context.identity.host
                            ),
                        },
                        extra: Default::default(),
                    }
                } else {
                    let result = if matches!(input, InboxInput::Ask { .. })
                        || (input.is_mutation() && input.operation_id().is_none())
                    {
                        Err(provenant_fabric_runtime::inbox::InboxError::Invalid(
                            "peer operation",
                        ))
                    } else {
                        provenant_fabric_runtime::inbox::execute(
                            &context.roots.state,
                            &context.identity,
                            &context.config,
                            false,
                            &clock,
                            input.clone(),
                            &cancel,
                            &|work, call| {
                                provenant_cli::inbox::resolve_work(
                                    &context.roots.state,
                                    context.identity.project,
                                    work,
                                    call.clock,
                                    call.cancel,
                                )
                            },
                            // Peers carry no control requests; they are local-only.
                            &|_, _| {
                                Err(provenant_fabric_runtime::inbox::InboxError::UnsupportedOwner)
                            },
                        )
                    };
                    PeerResult::Inbox {
                        data: match result {
                            Ok(result) => InboxReply::Ok {
                                extra: Default::default(),
                                result: Box::new(result),
                            },
                            Err(error) => InboxReply::Error {
                                extra: Default::default(),
                                code: error.code().into(),
                                message: error.to_string(),
                            },
                        },
                        extra: Default::default(),
                    }
                }
            }
            PeerAction::ConsoleBoard {
                all,
                limit,
                selected,
                ..
            } => {
                let command = provenant_cli::console::Console::Board {
                    all: *all,
                    limit: *limit,
                    cursor: None,
                    hosts: None,
                };
                if *limit > context.config.limits.page_rows {
                    PeerResult::Error {
                        data: provenant_fabric_runtime::hosts::PeerResultError {
                            extra: Default::default(),
                            error: FacadeError::InvalidInput { detail: None },
                        },
                        extra: Default::default(),
                    }
                } else {
                    match provenant_cli::console::read(
                        &context.roots,
                        selected.then_some(context.identity.project),
                        &command,
                    ) {
                        Ok(value) => PeerResult::Console {
                            data: value,
                            extra: Default::default(),
                        },
                        Err(_) => PeerResult::Error {
                            data: provenant_fabric_runtime::hosts::PeerResultError {
                                extra: Default::default(),
                                error: FacadeError::StateCorrupt,
                            },
                            extra: Default::default(),
                        },
                    }
                }
            }
            PeerAction::ConsoleTree {
                max_depth,
                max_nodes,
                detail,
                selected,
                ..
            } => {
                let command = provenant_cli::console::Console::Tree {
                    max_depth: *max_depth,
                    max_nodes: *max_nodes,
                    detail: *detail,
                    hosts: None,
                };
                if *max_nodes > context.config.limits.page_rows {
                    PeerResult::Error {
                        data: provenant_fabric_runtime::hosts::PeerResultError {
                            extra: Default::default(),
                            error: FacadeError::InvalidInput { detail: None },
                        },
                        extra: Default::default(),
                    }
                } else {
                    match provenant_cli::console::read(
                        &context.roots,
                        selected.then_some(context.identity.project),
                        &command,
                    ) {
                        Ok(value) => PeerResult::Console {
                            data: value,
                            extra: Default::default(),
                        },
                        Err(_) => PeerResult::Error {
                            data: provenant_fabric_runtime::hosts::PeerResultError {
                                extra: Default::default(),
                                error: FacadeError::StateCorrupt,
                            },
                            extra: Default::default(),
                        },
                    }
                }
            }
            PeerAction::ConsoleInbox { state, limit, .. } => {
                let error = |error| PeerResult::Error {
                    data: provenant_fabric_runtime::hosts::PeerResultError {
                        extra: Default::default(),
                        error,
                    },
                    extra: Default::default(),
                };
                if *limit > context.config.limits.page_rows {
                    error(FacadeError::InvalidInput { detail: None })
                } else {
                    match provenant_cli::inbox::console_inbox_page(&context.roots, *state, *limit) {
                        Ok(data) => PeerResult::Console {
                            data,
                            extra: Default::default(),
                        },
                        Err(_) => error(FacadeError::StateCorrupt),
                    }
                }
            }
            PeerAction::ConsoleDetail { id, .. } => {
                let command = provenant_cli::console::Console::Detail { id: id.clone() };
                match provenant_cli::console::read(&context.roots, None, &command) {
                    Ok(value) => PeerResult::Console {
                        data: value,
                        extra: Default::default(),
                    },
                    Err(_) => PeerResult::Error {
                        data: provenant_fabric_runtime::hosts::PeerResultError {
                            extra: Default::default(),
                            error: FacadeError::InvalidInput { detail: None },
                        },
                        extra: Default::default(),
                    },
                }
            }
            PeerAction::RunStatuses { targets, .. } => {
                if !(1..=100).contains(&targets.len()) {
                    PeerResult::Error {
                        data: provenant_fabric_runtime::hosts::PeerResultError {
                            extra: Default::default(),
                            error: FacadeError::InvalidInput { detail: None },
                        },
                        extra: Default::default(),
                    }
                } else {
                    peer_result(owner.snapshots_local(targets, &cancel), |data| {
                        PeerResult::RunStatuses {
                            data,
                            extra: Default::default(),
                        }
                    })
                }
            }
            PeerAction::Run { input, .. } => {
                let observed = owner
                    .run(input.clone(), budget, &cancel)
                    .and_then(|mut value| {
                        if let provenant_fabric_runtime::runs::RunResult::Status { attempt, .. } =
                            &mut value
                            && matches!(
                                attempt.state,
                                provenant_records::run::AttemptState::Terminal { .. }
                            )
                        {
                            let store = owner.store(&cancel)?;
                            let call = owner.call(&cancel);
                            let released =
                                store.custody_released(attempt, &call).map_err(run_error)?;
                            attempt.extra.insert(
                                "graph_custody_released".into(),
                                serde_json::json!(released),
                            );
                        }
                        Ok(value)
                    });
                peer_result(observed, |value| PeerResult::Run {
                    data: Box::new(value),
                    extra: Default::default(),
                })
            }
            PeerAction::Submit { input, extra } => peer_result(
                (|| {
                    let actor = extra
                        .get("graph_launch_actor")
                        .and_then(serde_json::Value::as_str)
                        .and_then(|value| value.parse::<provenant_core::ActorId>().ok())
                        .ok_or(FacadeError::StateCorrupt)?;
                    if request.agent != format!("graph-{actor}") {
                        return Err(FacadeError::ConfinementRefused { resource: None });
                    }
                    let attempt = extra.get("graph_attempt").cloned().ok_or_else(|| {
                        FacadeError::invalid(InvalidInputDetail::Field("graph context".into()))
                    })?;
                    let attempt: provenant_fabric_runtime::graph::AttemptSpec =
                        serde_json::from_value(attempt)
                            .map_err(|_| FacadeError::InvalidInput { detail: None })?;
                    if attempt.host.is_some()
                        || attempt.placement.is_some()
                        || attempt.new_worktree.is_some()
                        || !attempt.write_roots.is_empty()
                        || !attempt.capabilities.is_empty()
                    {
                        return Err(FacadeError::ConfinementRefused { resource: None });
                    }
                    let SubmitInput::Submit { tasks, host, .. } = input;
                    if host.is_some()
                        || tasks.iter().any(|task| task.context.as_ref().is_some_and(|context| context.graph_template_launch))
                        || tasks.iter().any(|task| {
                            task.access != provenant_fabric_runtime::graph::AccessMode::ReadOnly
                                || task.worktree.is_some()
                                || task.new_worktree.is_some()
                        })
                    {
                        return Err(FacadeError::ConfinementRefused { resource: None });
                    }
                    let retry = extra
                        .get("graph_retry_limit")
                        .cloned()
                        .map(serde_json::from_value)
                        .transpose()
                        .map_err(|_| FacadeError::InvalidInput { detail: None })?
                        .flatten();
                    let network = extra
                        .get("graph_network")
                        .cloned()
                        .map(serde_json::from_value)
                        .transpose()
                        .map_err(|_| FacadeError::InvalidInput { detail: None })?
                        .flatten();
                    let source = provenant_run::resolve_reader_source(
                        context.roots.state.as_path(),
                        &cwd,
                        &cwd,
                    )
                    .map_err(run_error)?;
                    let directory = provenant_os::fs::Directory::open_anchor(&source)
                        .map_err(|_| FacadeError::ConfinementRefused { resource: None })?;
                    let metadata = directory
                        .metadata()
                        .map_err(|_| FacadeError::ConfinementRefused { resource: None })?;
                    let access = provenant_records::run::LaneAccess {
                        network,
                        source_root: Some(source),
                        source_identity: Some(provenant_records::run::SourceIdentity {
                            device: metadata.device,
                            inode: metadata.inode,
                        }),
                        ..Default::default()
                    };
                    let SubmitInput::Submit { operation_id, .. } = input;
                    let execution = provenant_run::ExecutionContext {
                        state_root: context.roots.state.clone(),
                        project_id: context.identity.project,
                        actor_id: context.identity.inbox.actor,
                        read_prompts: false,
                    };
                    let call = owner.call(&cancel);
                    let retained = provenant_run::RunStore::open_read_only(&execution, &call)
                        .map_err(run_error)?
                        .map(|store| store.operation(operation_id, &call))
                        .transpose()
                        .map_err(run_error)?
                        .flatten()
                        .is_some_and(|operation| !matches!(operation.state, provenant_run::OperationState::Prepared));
                    // Lost receipts replay their sealed request even when unrelated
                    // old work now needs draining. New operations still refuse.
                    if !retained {
                        crate::install::cutover_admission_guard(&context.roots.state)?;
                    }
                    let mut input = input.clone();
                    let SubmitInput::Submit { tasks, .. } = &mut input;
                    for task in tasks {
                        let context = task.context.as_mut().filter(|context| {
                            context.graph_id.as_ref().is_some_and(|id| !id.is_empty())
                                && context.node_id.as_ref().is_some_and(|id| !id.is_empty())
                        }).ok_or_else(|| FacadeError::invalid(InvalidInputDetail::Field("graph context".into())))?;
                        // The receiving host records provenance only after validating
                        // the authenticated graph launch envelope above.
                        context.graph_template_launch = true;
                    }
                    owner.submit_with_access(
                        input,
                        budget,
                        &cancel,
                        access,
                        retry,
                        Some(attempt),
                    )
                })(),
                |data| PeerResult::Submitted {
                    data,
                    extra: Default::default(),
                },
            ),
            PeerAction::Output { input, .. } => {
                peer_result(owner.output(input.clone(), budget, &cancel), |data| {
                    PeerResult::Output {
                        data,
                        extra: Default::default(),
                    }
                })
            }
            PeerAction::RunEvents { after, limit, .. } => {
                peer_result(owner.events(*after, *limit, &cancel), |data| {
                    PeerResult::RunEvents {
                        data,
                        extra: Default::default(),
                    }
                })
            }
            PeerAction::Lanes { all, limit, .. } => {
                peer_result(owner.lanes_local(*all, *limit, &cancel), |data| {
                    PeerResult::RunStatuses {
                        data,
                        extra: Default::default(),
                    }
                })
            }
            PeerAction::LaneWaits { targets, .. } => {
                if !(1..=100).contains(&targets.len()) {
                    PeerResult::Error {
                        data: provenant_fabric_runtime::hosts::PeerResultError {
                            extra: Default::default(),
                            error: FacadeError::InvalidInput { detail: None },
                        },
                        extra: Default::default(),
                    }
                } else {
                    peer_result(owner.waits_local(targets, &cancel), |data| {
                        PeerResult::LaneWaits {
                            data,
                            extra: Default::default(),
                        }
                    })
                }
            }
            PeerAction::SessionHandoff { input, .. } => PeerResult::SessionHandoff {
                data: crate::handoff::receive(
                    &crate::handoff::Target {
                        context,
                        home: &home,
                        cwd: &cwd,
                        workers,
                        source: request.source_host,
                        clock: &clock,
                        budget,
                    },
                    input,
                    &provenant_fabric_runtime::handoff::session::Terminal,
                ),
                extra: Default::default(),
            },
            PeerAction::Health { .. } => PeerResult::Health {
                data: provenant_fabric_runtime::hosts::PeerHealth {
                    extra: Default::default(),
                    host: context.identity.host,
                    available: true,
                    execution: Some(owner.capabilities().submit),
                    release: Some(env!("CARGO_PKG_VERSION").into()),
                    run_capabilities: Some(owner.capabilities()),
                    readiness: Some(provenant_fabric_runtime::hosts::PeerReadiness::reporting()),
                },
                extra: Default::default(),
            },
            PeerAction::Doctor { .. } => {
                match crate::hosts::local(&home, &context.roots).and_then(|report| {
                    serde_json::to_value(report)
                        .map_err(|error| Box::new(error) as Box<dyn std::error::Error>)
                }) {
                    Ok(data) => PeerResult::Doctor {
                        data,
                        extra: Default::default(),
                    },
                    Err(error) => PeerResult::Error {
                        data: provenant_fabric_runtime::hosts::PeerResultError {
                            extra: Default::default(),
                            error: FacadeError::io(format!(
                                "build the local doctor report: {error}"
                            )),
                        },
                        extra: Default::default(),
                    },
                }
            }
            PeerAction::LandingPush {
                proof,
                operation_id,
                reconcile,
                ..
            } => {
                if context
                    .config
                    .home_hosts
                    .get(request.project.key.as_str())
                    .is_some_and(|h| *h != context.identity.host)
                {
                    PeerResult::Error {
                        data: provenant_fabric_runtime::hosts::PeerResultError {
                            extra: Default::default(),
                            error: FacadeError::HomeUnavailable,
                        },
                        extra: Default::default(),
                    }
                } else {
                    let executor = SystemExecutor::new(1)?;
                    match landing_local(
                        context,
                        &executor,
                        proof.clone(),
                        operation_id,
                        *reconcile,
                        request.actor()?,
                        &clock,
                        &cancel,
                    ) {
                        Ok(value) => PeerResult::Push {
                            data: value,
                            extra: Default::default(),
                        },
                        Err(error) => PeerResult::Error {
                            data: provenant_fabric_runtime::hosts::PeerResultError {
                                extra: Default::default(),
                                error: landing_error(error.as_ref()),
                            },
                            extra: Default::default(),
                        },
                    }
                }
            }
            PeerAction::Receipt { operation_id, .. } => {
                let store_request = provenant_fabric_store::Request {
                    clock: &clock,
                    cancel: &cancel,
                };
                let store =
                    provenant_fabric_store::Store::open(&context.roots.state, &store_request)?;
                match store.coordination_receipt_with_input::<Mutation, serde_json::Value>(
                    context.identity.project,
                    request.actor()?,
                    operation_id,
                    &store_request,
                )? {
                    Some((input, value)) => peer_result(
                        CoordinationResult::from_stored(&input, value)
                            .map_err(|_| FacadeError::StateCorrupt),
                        |data| PeerResult::Coordination {
                            data,
                            extra: Default::default(),
                        },
                    ),
                    None => PeerResult::Error {
                        data: provenant_fabric_runtime::hosts::PeerResultError {
                            extra: Default::default(),
                            error: FacadeError::LaunchUnknown {
                                operation_id: operation_id.clone(),
                            },
                        },
                        extra: Default::default(),
                    },
                }
            }
            PeerAction::Coordination {
                input,
                operation_id,
                ..
            } => {
                // A peer never recursively forwards or elects another home.
                if context
                    .config
                    .home_hosts
                    .get(request.project.key.as_str())
                    .is_some_and(|h| *h != context.identity.host)
                {
                    PeerResult::Error {
                        data: provenant_fabric_runtime::hosts::PeerResultError {
                            extra: Default::default(),
                            error: FacadeError::HomeUnavailable,
                        },
                        extra: Default::default(),
                    }
                } else {
                    let coverage = if matches!(
                        input,
                        CoordinationInput::Events {
                            input: EventsInput::Read { .. },
                            extra: _
                        }
                    ) {
                        peer_factory.pull_events_from(false, budget, &cancel)
                    } else {
                        Ok(EventCoverage::default())
                    };
                    match coverage {
                        Ok(coverage) => {
                            let service = CoordinationService::open(
                                &context.roots.state,
                                context.identity.project,
                                request.actor()?,
                                &clock,
                                &backoff,
                                &cancel,
                            )?;
                            match input.execute(&service, operation_id.clone(), budget, &cancel) {
                                Ok(mut value) => {
                                    if let CoordinationResult::Events {
                                        result:
                                            EventsResult::Read {
                                                unknown_hosts,
                                                run_events_pending,
                                                ..
                                            },
                                        ..
                                    } = &mut value
                                    {
                                        unknown_hosts.extend(coverage.unknown_hosts);
                                        *run_events_pending |= coverage.more_events;
                                    }
                                    PeerResult::Coordination {
                                        data: value,
                                        extra: Default::default(),
                                    }
                                }
                                Err(error) => PeerResult::Error {
                                    data: provenant_fabric_runtime::hosts::PeerResultError {
                                        extra: Default::default(),
                                        error: FacadeError::from(&error),
                                    },
                                    extra: Default::default(),
                                },
                            }
                        }
                        Err(error) => PeerResult::Error {
                            data: provenant_fabric_runtime::hosts::PeerResultError {
                                extra: Default::default(),
                                error,
                            },
                            extra: Default::default(),
                        },
                    }
                }
            }
        };
        let response = PeerResponse {
            extra: Default::default(),
            version: 1,
            protocol: request.protocol,
            host: context.identity.host,
            project: request.project.key,
            result,
        };
        let mut bytes = serde_json::to_vec(&response)?;
        if bytes.len() >= MAX_FRAME {
            return Err(PeerError::Frame.into());
        }
        bytes.push(b'\n');
        provenant_os::sys::hook_io::write_stdout(
            &bytes,
            Instant::now() + std::time::Duration::from_secs(30),
        )?;
        Ok(())
    })();
    match result {
        Ok(()) => 0,
        Err(error) => {
            let _ = std::io::Write::write_all(
                &mut io::stderr().lock(),
                format!("provenant: fabric peer: {error}\n").as_bytes(),
            );
            3
        }
    }
}
