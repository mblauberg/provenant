use super::*;
use provenant_fabric_runtime::runs::SubmissionReceipt;
use std::collections::HashSet;

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
        | PeerAction::Doctor { .. }
        | PeerAction::Receipt { .. } => true,
        PeerAction::CancelSubmission { stop, .. } => !stop,
        PeerAction::Inbox { extra, .. }
            if extra.get("graph_steer") == Some(&serde_json::Value::Bool(true)) =>
        {
            false
        }
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

fn validate_graph_steer(
    factory: &Factory,
    owner: &BinaryRunOwner<'_>,
    request: &PeerRequest,
    input: &provenant_fabric_runtime::inbox::InboxInput,
    extra: &provenant_records::ExtraFields,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<(), FacadeError> {
    use provenant_fabric_runtime::inbox::InboxInput;
    let InboxInput::Steer {
        attempt: requested_attempt,
        operation_id: Some(_),
        ..
    } = input
    else {
        return Err(FacadeError::InvalidInput { detail: None });
    };
    let actor = extra
        .get("graph_launch_actor")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| value.parse::<provenant_core::ActorId>().ok())
        .ok_or(FacadeError::ConfinementRefused { resource: None })?;
    if request.agent != format!("graph-{actor}")
        || request.actor().ok() != Some(factory.context.identity.inbox.actor)
    {
        return Err(FacadeError::ConfinementRefused { resource: None });
    }
    let launch_operation = extra
        .get("graph_launch_operation")
        .cloned()
        .and_then(|value| serde_json::from_value::<provenant_core::OperationId>(value).ok())
        .ok_or(FacadeError::ConfinementRefused { resource: None })?;
    let origin = extra
        .get("graph_origin_work_key")
        .cloned()
        .and_then(|value| serde_json::from_value::<provenant_records::WorkKey>(value).ok())
        .ok_or(FacadeError::ConfinementRefused { resource: None })?;
    let receipt = peer_retained_receipt(factory, &launch_operation, actor)?.ok_or(
        FacadeError::LaunchUnknown {
            operation_id: launch_operation.clone(),
        },
    )?;
    let expected_graph = peer_graph_name(actor, &launch_operation)?;
    if receipt.operation_id != launch_operation
        || receipt.targets.len() != 1
        || !receipt.extra.get("peer_graph").is_some_and(|graph| {
            graph["host"] == serde_json::json!(factory.context.identity.host)
                && graph["graph_id"] == serde_json::json!(expected_graph)
        })
    {
        return Err(FacadeError::StateCorrupt);
    }
    let mut target = receipt
        .targets
        .into_iter()
        .next()
        .ok_or(FacadeError::StateCorrupt)?;
    let local = factory.context.identity.host;
    if target.host != Some(local) {
        return Err(FacadeError::StateCorrupt);
    }
    let mut seen = HashSet::new();
    let mut previous: Option<provenant_records::run::AttemptRecord> = None;
    for _ in 0..MAX_GRAPH_STEER_SUPERSEDED {
        if !seen.insert(target.attempt) {
            return Err(FacadeError::StateCorrupt);
        }
        let record = owner.snapshot(target.clone(), budget, cancel)?;
        let fields = peer_graph_fields(factory, record.attempt_id)?;
        let peer_graph = fields.get("peer_graph");
        if record.attempt_id != target.attempt
            || record.verified_owner != Some(factory.context.identity.inbox.actor)
            || record.project_id != factory.context.identity.project
            || record.key.project() != factory.context.identity.project
            || record.key.graph() != expected_graph
            || record.key.node() != origin.node()
            || !peer_origin_matches(&fields, &origin)
            || !peer_graph.is_some_and(|graph| {
                graph["host"] == serde_json::json!(local)
                    && graph["graph_id"] == serde_json::json!(expected_graph)
            })
            || !record.context.as_ref().is_some_and(|context| {
                context.graph_id.as_deref() == Some(expected_graph.as_str())
                    && context.node_id.as_deref() == Some(record.key.node())
                    && super::steer::context_attempt_matches(
                        context.attempt,
                        record.key.attempt(),
                        previous.is_some(),
                    )
            })
        {
            return Err(FacadeError::StateCorrupt);
        }
        if let Some(previous) = &previous
            && (record.retry_of != Some(previous.attempt_id)
                || !super::steer::successor_key_matches(&previous.key, &record.key)
                || record.project_id != previous.project_id)
        {
            return Err(FacadeError::StateCorrupt);
        }
        match record.superseded_by {
            Some(next)
                if matches!(
                    record.state,
                    provenant_records::run::AttemptState::Terminal { .. }
                ) =>
            {
                previous = Some(record);
                target.attempt = next;
            }
            Some(_) => return Err(FacadeError::StateCorrupt),
            None if !matches!(
                record.state,
                provenant_records::run::AttemptState::Terminal { .. }
            ) =>
            {
                if target.attempt != *requested_attempt {
                    return Err(FacadeError::StateCorrupt);
                }
                return Ok(());
            }
            None => return Err(FacadeError::StateCorrupt),
        }
    }
    Err(FacadeError::StateCorrupt)
}

const MAX_GRAPH_STEER_SUPERSEDED: usize = 32;

fn peer_inbox_execute(
    factory: &Factory,
    home: &Path,
    cwd: &Path,
    input: provenant_fabric_runtime::inbox::InboxInput,
    cancel: &CancelToken,
    graph_steer: bool,
) -> Result<provenant_fabric_runtime::inbox::InboxResult, provenant_fabric_runtime::inbox::InboxError>
{
    use provenant_fabric_runtime::inbox::InboxInput;
    let context = &factory.context;
    if graph_steer
        && !matches!(
            input,
            InboxInput::Steer {
                operation_id: Some(_),
                ..
            }
        )
    {
        return Err(provenant_fabric_runtime::inbox::InboxError::Invalid(
            "peer operation",
        ));
    }
    provenant_fabric_runtime::inbox::execute(
        &context.roots.state,
        &context.identity,
        &context.config,
        context.in_lane,
        &factory.clock,
        input,
        cancel,
        &|work, call| {
            provenant_cli::inbox::resolve_work(
                &context.roots.state,
                context.identity.project,
                work,
                call.clock,
                call.cancel,
            )
        },
        &|input, _| {
            if graph_steer && !matches!(input, InboxInput::Steer { .. }) {
                return Err(provenant_fabric_runtime::inbox::InboxError::Invalid(
                    "control target",
                ));
            }
            provenant_cli::inbox::resolve_control(
                &provenant_cli::run::Edge {
                    roots: &context.roots,
                    home,
                    assets: &factory.assets,
                    cwd,
                },
                &context.identity,
                input,
            )
        },
    )
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
            PeerAction::Inbox { input, extra } => {
                use provenant_fabric_runtime::inbox::{InboxInput, InboxReply};
                let graph_steer = extra.get("graph_steer") == Some(&serde_json::Value::Bool(true));
                let validation = if graph_steer {
                    validate_graph_steer(
                        &peer_factory,
                        &owner,
                        &request,
                        input,
                        extra,
                        budget,
                        &cancel,
                    )
                    .err()
                } else {
                    None
                };
                if let Some(error) = validation {
                    PeerResult::Inbox {
                        data: InboxReply::Error {
                            extra: Default::default(),
                            code: error.static_code().into(),
                            message: "graph steer owner could not verify the accepted live attempt"
                                .into(),
                        },
                        extra: Default::default(),
                    }
                } else if !graph_steer
                    && matches!(
                        input,
                        InboxInput::Close { .. }
                            | InboxInput::Acknowledge { .. }
                            | InboxInput::Steer { .. }
                            | InboxInput::ChairMessage { .. }
                    )
                {
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
                        peer_inbox_execute(
                            &peer_factory,
                            &home,
                            &cwd,
                            input.clone(),
                            &cancel,
                            graph_steer,
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
                match provenant_cli::console::read_detail(&context.roots, id) {
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
            PeerAction::CancelSubmission { .. } => peer_result(
                peer_cancel_submission_request(&peer_factory, &request, budget, &cancel),
                |settled| PeerResult::SubmissionCancelled {
                    settled,
                    extra: Default::default(),
                },
            ),
            PeerAction::Run { input, extra } => {
                let observed = owner
                    .run(input.clone(), budget, &cancel)
                    .and_then(|mut value| {
                        // Validate and bind the parent's cancel operation before
                        // mutating any descendant; conflicts leave the subtree alone.
                        if let RunInput::Cancel { target, .. } = input {
                            peer_cancel_subtree(&peer_factory, target.attempt, budget, &cancel)?;
                        }
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
                            // The sealed settlement handoff, so the chair never
                            // re-derives it from the result text.
                            attempt.extra.insert(
                                "graph_handoff".into(),
                                serde_json::json!(store.handoff(attempt).map_err(run_error)?),
                            );
                        }
                        if let provenant_fabric_runtime::runs::RunResult::Status {
                            attempt, ..
                        } = &mut value
                        {
                            let after = extra
                                .get("peer_subtree_after")
                                .map(|value| {
                                    value
                                        .as_u64()
                                        .and_then(|value| usize::try_from(value).ok())
                                        .ok_or(FacadeError::InvalidInput { detail: None })
                                })
                                .transpose()?
                                .unwrap_or(0);
                            attempt.extra.extend(peer_graph_fields_page(
                                &peer_factory,
                                attempt.attempt_id,
                                after,
                            )?);
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
                        || attempt.new_worktree.is_some()
                        || !attempt.write_roots.is_empty()
                        || !attempt.capabilities.is_empty()
                    {
                        return Err(FacadeError::ConfinementRefused { resource: None });
                    }
                    let SubmitInput::Submit { task, host, .. } = &**input;
                    if host.is_some()
                        || task
                            .context
                            .as_ref()
                            .is_some_and(|context| context.graph_template_launch)
                        || task.access != provenant_fabric_runtime::graph::AccessMode::ReadOnly
                        || task.worktree.is_some()
                        || task.new_worktree.is_some()
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
                    let mut input = SubmitInput::clone(input);
                    let SubmitInput::Submit { task, .. } = &mut input;
                    {
                        let context = task
                            .context
                            .as_mut()
                            .filter(|context| {
                                context.graph_id.as_ref().is_some_and(|id| !id.is_empty())
                                    && context.node_id.as_ref().is_some_and(|id| !id.is_empty())
                            })
                            .ok_or_else(|| {
                                FacadeError::invalid(InvalidInputDetail::Field(
                                    "graph context".into(),
                                ))
                            })?;
                        // The receiving host records provenance only after validating
                        // the authenticated graph launch envelope above.
                        context.graph_template_launch = true;
                    }
                    peer_submit_graph(
                        &peer_factory,
                        input,
                        actor,
                        attempt,
                        access.network,
                        retry,
                        budget,
                        &cancel,
                    )
                })(),
                |data| PeerResult::Submitted {
                    data,
                    extra: Default::default(),
                },
            ),
            PeerAction::Output { input, .. } => peer_result(
                owner
                    .output(input.clone(), budget, &cancel)
                    .and_then(|mut data| {
                        let (OutputResult::Chunk { extra, target, .. }
                        | OutputResult::Artifact { extra, target, .. }) = &mut data;
                        let mut fields = peer_graph_fields(&peer_factory, target.attempt)?;
                        fields.remove("peer_subtree");
                        extra.extend(fields);
                        Ok(data)
                    }),
                |data| PeerResult::Output {
                    data,
                    extra: Default::default(),
                },
            ),
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
                    execution: owner.capabilities().submit,
                    release: env!("CARGO_PKG_VERSION").into(),
                    run_capabilities: owner.capabilities(),
                    readiness: provenant_fabric_runtime::hosts::PeerReadiness::reporting(),
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
            PeerAction::Receipt {
                operation_id,
                extra,
            } => peer_result(
                (|| {
                    let store_request = provenant_fabric_store::Request {
                        clock: &clock,
                        cancel: &cancel,
                    };
                    let deadline = std::time::Instant::now()
                        + std::time::Duration::from_micros(
                            budget.remaining(clock.mono_now()).as_micros(),
                        );
                    let stored =
                        match provenant_fabric_store::Store::open_read_only(
                            &context.roots.state,
                            deadline,
                        )
                        .map_err(|error| FacadeError::from(&error))?
                        {
                            Some(store) => store
                                .coordination_receipt_with_input::<Mutation, serde_json::Value>(
                                    context.identity.project,
                                    request.actor().map_err(|_| {
                                        FacadeError::ConfinementRefused { resource: None }
                                    })?,
                                    operation_id,
                                    &store_request,
                                )
                                .map_err(|error| FacadeError::from(&error))?,
                            None => None,
                        };
                    Ok(match stored {
                        Some((input, value)) => peer_result(
                            CoordinationResult::from_stored(&input, value)
                                .map_err(|_| FacadeError::StateCorrupt),
                            |data| PeerResult::Coordination {
                                data,
                                extra: Default::default(),
                            },
                        ),
                        None => {
                            let graph_actor = request
                                .agent
                                .strip_prefix("graph-")
                                .and_then(|actor| actor.parse::<provenant_core::ActorId>().ok());
                            let receipt = if let Some(actor) = graph_actor {
                                if extra
                                    .get("graph_launch_actor")
                                    .and_then(serde_json::Value::as_str)
                                    .is_some_and(|value| value != actor.to_string())
                                {
                                    return Err(FacadeError::ConfinementRefused { resource: None });
                                }
                                let origin = extra
                                    .get("graph_origin_work_key")
                                    .cloned()
                                    .map(|value| {
                                        serde_json::from_value::<provenant_records::WorkKey>(value)
                                            .map_err(|_| FacadeError::InvalidInput { detail: None })
                                    })
                                    .transpose()?;
                                let expected_graph = peer_graph_name(actor, operation_id)?;
                                let receipt =
                                    peer_retained_receipt(&peer_factory, operation_id, actor)?;
                                if let (Some(receipt), Some(origin)) = (&receipt, origin) {
                                    if receipt.operation_id != *operation_id
                                        || receipt.targets.len() != 1
                                        || !receipt.extra.get("peer_graph").is_some_and(|graph| {
                                            graph["host"]
                                                == serde_json::json!(context.identity.host)
                                                && graph["graph_id"]
                                                    == serde_json::json!(expected_graph)
                                        })
                                    {
                                        return Err(FacadeError::StateCorrupt);
                                    }
                                    let target =
                                        receipt.targets.first().ok_or(FacadeError::StateCorrupt)?;
                                    let record = owner.snapshot(target.clone(), budget, &cancel)?;
                                    let fields = peer_graph_fields(&peer_factory, target.attempt)?;
                                    if record.project_id != context.identity.project
                                        || record.key.project() != context.identity.project
                                        || record.key.graph() != expected_graph
                                        || record.key.node() != origin.node()
                                        || record.verified_owner
                                            != Some(context.identity.inbox.actor)
                                        || !peer_origin_matches(&fields, &origin)
                                        || !record.context.as_ref().is_some_and(|context| {
                                            context.graph_id.as_deref()
                                                == Some(expected_graph.as_str())
                                                && context.node_id.as_deref()
                                                    == Some(record.key.node())
                                                && super::steer::context_attempt_matches(
                                                    context.attempt,
                                                    record.key.attempt(),
                                                    false,
                                                )
                                        })
                                    {
                                        return Err(FacadeError::StateCorrupt);
                                    }
                                }
                                match receipt {
                                    Some(receipt) => Ok(Some(receipt)),
                                    None => owner.retained_graph_receipt(operation_id, &cancel),
                                }
                            } else {
                                Ok(None)
                            };
                            peer_result(
                                receipt.and_then(|receipt| {
                                    receipt.ok_or_else(|| FacadeError::LaunchUnknown {
                                        operation_id: operation_id.clone(),
                                    })
                                }),
                                |data| PeerResult::Submitted {
                                    data,
                                    extra: Default::default(),
                                },
                            )
                        }
                    })
                })(),
                |result| result,
            ),
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

/// The receiver owns a graph for each authenticated remote submission. Its
/// journal lock spans launch, just as it does for an ordinary local graph.
#[allow(clippy::too_many_arguments)]
pub(crate) fn peer_submit_graph(
    factory: &Factory,
    input: SubmitInput,
    actor: provenant_core::ActorId,
    attempt: provenant_fabric_runtime::graph::AttemptSpec,
    network: Option<provenant_records::run::NetworkAccess>,
    retry: Option<u8>,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<SubmissionReceipt, FacadeError> {
    use provenant_fabric_runtime::graph::{FileJournal, GraphDocument, GraphExecution};
    use provenant_records::run::RequestedRoute;
    let facts = provenant_confinement::worktrees::RepoFacts::default();
    let submission_digest = provenant_records::CanonicalV1::record_digest(
        "peer-submission",
        1,
        &(&input, actor, &attempt, network, retry),
    )
    .map_err(|_| FacadeError::StateCorrupt)?
    .to_string();
    let SubmitInput::Submit {
        operation_id,
        origin_work_key,
        task,
        knobs,
        ..
    } = input;
    let origin_work_key = origin_work_key.ok_or(FacadeError::StateCorrupt)?;
    let task = &task;
    let graph = peer_graph_name(actor, &operation_id)?;
    let mut route = match &task.route {
        RequestedRoute::Adapter { provider, name, .. } => {
            serde_json::json!({"adapter":provider,"name":name})
        }
        RequestedRoute::Model {
            provider,
            model,
            effort,
            ..
        } => serde_json::json!({"adapter":provider,"model":model,"effort":effort}),
        RequestedRoute::Named { name, effort, .. }
        | RequestedRoute::Review { name, effort, .. } => {
            serde_json::json!({"alias":name,"effort":effort})
        }
    };
    route["peer_requested_route"] =
        serde_json::to_value(&task.route).map_err(|_| FacadeError::StateCorrupt)?;
    route["peer_network"] = serde_json::to_value(network).map_err(|_| FacadeError::StateCorrupt)?;
    route["peer_retry_limit"] = serde_json::json!(retry);
    route["peer_source_head"] = serde_json::json!(task.source_head);
    let origin = task
        .context
        .clone()
        .ok_or(FacadeError::InvalidInput { detail: None })?;
    let node = origin
        .node_id
        .clone()
        .ok_or(FacadeError::InvalidInput { detail: None })?;
    if origin.graph_id.as_deref() != Some(origin_work_key.graph())
        || node != origin_work_key.node()
        || origin
            .attempt
            .is_some_and(|attempt| attempt != origin_work_key.attempt())
    {
        return Err(FacadeError::StateCorrupt);
    }
    let mut attempt = attempt;
    attempt.provider_timeout = attempt.provider_timeout.or(Some(task.timeout_seconds));
    attempt.account = task.account.clone().or(attempt.account);
    attempt.build_heavy |= task.build_heavy;
    let exclude = knobs
        .iter()
        .map(|knob| {
            knob.strip_prefix("routing.graph_exclude=")
                .ok_or(FacadeError::InvalidInput { detail: None })
                .and_then(|value| {
                    serde_json::from_str::<Vec<String>>(value)
                        .map_err(|_| FacadeError::InvalidInput { detail: None })
                })
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect();
    let network = network.map(|reach| match reach {
        provenant_records::run::NetworkAccess::Provider => provenant_cli::Switch::Off,
        _ => provenant_cli::Switch::On,
    });
    let mut document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version":1,"graph_id":graph,"template_library":{},"exclude":exclude,
        "nodes":[{"id":node,"access":"read_only","prompt":task.prompt,"title":task.title,
          "route":route,"attempt":attempt,"context":origin,"require_skill_location":task.require_skill_location,
          "settlement_retries":retry}],
    })).map_err(|_| FacadeError::StateCorrupt)?;
    // Preserve the origin's semantic role so review launches use the same
    // comparison-base path as local graph nodes, including exact model routes.
    document.nodes[0].role = origin.role.clone();
    document.review_base = task.base.clone();
    document.extra.insert(
        "peer_origin_work_key".into(),
        serde_json::json!(origin_work_key),
    );
    document.nodes[0].source =
        task.head.as_ref().map(
            |head| provenant_fabric_runtime::graph::SourceBinding::Commit {
                commit: head.clone(),
            },
        );
    let review_base = task
        .base
        .as_deref()
        .map(|base| factory.resolve_base(&factory.cwd, base, &facts, cancel))
        .transpose()?;
    if let Some(network) = network {
        document.extra.insert(
            "network".into(),
            serde_json::json!(if matches!(network, provenant_cli::Switch::On) {
                "on"
            } else {
                "off"
            }),
        );
    }
    provenant_fabric_runtime::graph::patch::submission(
        &mut document,
        &operation_id.to_string(),
        &factory.context.config.graph.limits,
    )
    .map_err(|_| FacadeError::InvalidInput { detail: None })?;
    let mut journal = FileJournal::open(
        &factory.context.roots.state,
        factory.context.identity.project,
        &graph,
        budget,
        cancel,
        &factory.clock,
    )
    .map_err(graph_journal_error)?;
    if let Some(bound) = FileJournal::execution(
        &factory.context.roots.state,
        factory.context.identity.project,
        &graph,
    )
    .map_err(graph_journal_error)?
        && bound
            .extra
            .get("peer_submission_digest")
            .and_then(serde_json::Value::as_str)
            != Some(submission_digest.as_str())
    {
        return Err(FacadeError::Conflict);
    }
    journal
        .bind(GraphExecution {
            review_base,
            integration_base: None,
            directory: factory.cwd.clone(),
            extra: [
                (
                    "peer_origin_context".into(),
                    serde_json::json!({"graph_id":origin.graph_id,"node_id":origin.node_id}),
                ),
                ("peer_operation_id".into(), serde_json::json!(operation_id)),
                (
                    "peer_submission_digest".into(),
                    serde_json::json!(submission_digest),
                ),
            ]
            .into(),
            document: document.clone(),
        })
        .map_err(|_| FacadeError::Conflict)?;
    let mut owner = BoundGraphOwner {
        writer_launch_budget: None,
        admission_timeout: None,
        repo_facts: facts,
        launch_warnings: Vec::new(),
        factory,
        integration_base: None,
        integration_branch: None,
        network,
        capability_overrides: Default::default(),
        exclude,
        budget,
        cancel,
        prices: None,
        tick: false,
    };
    let run = provenant_fabric_runtime::graph::advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &factory.context.config.graph.limits,
        &factory.context.config.graph.budget,
    )
    .map_err(|error| FacadeError::io(format!("advance peer graph: {error}")))?;
    peer_graph_receipt(factory, operation_id, &graph, &run.checkpoint)
}

/// The receiver's graph for one origin submission. A digest prefix keeps the
/// name within the 48-byte graph-name bound that chair associations enforce.
pub(crate) fn peer_graph_name(
    actor: provenant_core::ActorId,
    operation_id: &provenant_core::OperationId,
) -> Result<String, FacadeError> {
    let digest =
        provenant_records::CanonicalV1::record_digest("peer-subgraph", 1, &(actor, operation_id))
            .map_err(|_| FacadeError::StateCorrupt)?;
    Ok(format!(
        "peer-{}",
        &provenant_core::hex(digest.as_bytes())[..40]
    ))
}

pub(super) fn peer_graph_receipt(
    factory: &Factory,
    operation_id: provenant_core::OperationId,
    graph: &str,
    checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
) -> Result<SubmissionReceipt, FacadeError> {
    if checkpoint.graph_id != graph {
        return Err(FacadeError::StateCorrupt);
    }
    let root = checkpoint
        .nodes
        .first()
        .and_then(|node| node.attempts.last())
        .ok_or_else(|| FacadeError::LaunchUnknown {
            operation_id: operation_id.clone(),
        })?;
    if root.key.graph_id != graph {
        return Err(FacadeError::StateCorrupt);
    }
    let actor = root
        .launch_actor
        .parse::<provenant_core::ActorId>()
        .map_err(|_| FacadeError::StateCorrupt)?;
    let cancel = CancelToken::new();
    let owner = BinaryRunOwner {
        factory,
        allow_remote: false,
        actor: Some(actor),
        chair: None,
    };
    let session = owner.read_session(&cancel).map_err(|error| match error {
        FacadeError::NotFound => FacadeError::LaunchUnknown {
            operation_id: operation_id.clone(),
        },
        error => error,
    })?;
    let call = owner.call(&cancel);
    let launch_operation = graph_operation("graph-attempt-operation", &root.key)?;
    let operation = session
        .store
        .operation(&launch_operation, &call)
        .map_err(run_error)?
        .ok_or_else(|| FacadeError::LaunchUnknown {
            operation_id: operation_id.clone(),
        })?;
    if operation.operation_id != launch_operation
        || !matches!(
            operation.state,
            provenant_run::OperationState::Accepted
                | provenant_run::OperationState::Terminal
                | provenant_run::OperationState::Blocked
        )
    {
        return Err(FacadeError::LaunchUnknown {
            operation_id: operation_id.clone(),
        });
    }
    let admitted = session
        .store
        .operation_attempt(&launch_operation, &call)
        .map_err(run_error)?;
    let key = root
        .key
        .work_key(factory.context.identity.project)
        .map_err(|_| FacadeError::StateCorrupt)?;
    let request = session.store.request(admitted, &call).map_err(run_error)?;
    if !request.task.context.as_ref().is_some_and(|context| {
        context.graph_template_launch
            && context.graph_id.as_deref() == Some(graph)
            && context.node_id.as_deref() == Some(root.key.node_id.as_str())
            && context.attempt == Some(root.key.attempt)
    }) {
        return Err(FacadeError::StateCorrupt);
    }
    let record = session.store.record(admitted, &call).map_err(run_error)?;
    if record.project_id != factory.context.identity.project
        || record.key != key
        || record.verified_owner != Some(actor)
        || !record.context.as_ref().is_some_and(|context| {
            context.graph_template_launch
                && context.graph_id.as_deref() == Some(graph)
                && context.node_id.as_deref() == Some(root.key.node_id.as_str())
                && context.attempt == Some(root.key.attempt)
        })
    {
        return Err(FacadeError::StateCorrupt);
    }
    let accepted = owner.receipt(&session, launch_operation, &record, &[], &call)?;
    if accepted.operation_id != graph_operation("graph-attempt-operation", &root.key)?
        || accepted.targets.len() != 1
    {
        return Err(FacadeError::StateCorrupt);
    }
    let mut target = accepted
        .targets
        .first()
        .cloned()
        .ok_or(FacadeError::StateCorrupt)?;
    if target.host != Some(factory.context.identity.host) || target.attempt != admitted {
        return Err(FacadeError::StateCorrupt);
    }
    let checkpoint_target = root
        .attempt_ref
        .as_deref()
        .map(|reference| {
            let (host, attempt) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
            Ok::<_, FacadeError>(AttemptTarget {
                extra: Default::default(),
                host: Some(host.parse().map_err(|_| FacadeError::StateCorrupt)?),
                attempt: attempt.parse().map_err(|_| FacadeError::StateCorrupt)?,
            })
        })
        .transpose()?;
    if checkpoint_target
        .as_ref()
        .is_some_and(|saved| saved.host != Some(factory.context.identity.host))
    {
        return Err(FacadeError::StateCorrupt);
    }
    let mut checkpoint_target_seen = checkpoint_target.is_none();
    let mut predecessor: Option<provenant_records::run::AttemptRecord> = None;
    let mut seen = std::collections::HashSet::new();
    for _ in 0..MAX_GRAPH_STEER_SUPERSEDED {
        if !seen.insert(target.attempt) {
            return Err(FacadeError::StateCorrupt);
        }
        let current = owner.snapshot(
            target.clone(),
            WaitBudget::nonblocking(factory.clock.mono_now()),
            &cancel,
        )?;
        let fields = peer_graph_fields(factory, target.attempt)?;
        let origin: provenant_records::WorkKey = fields
            .get("peer_origin_work_key")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .ok_or(FacadeError::StateCorrupt)?;
        if current.project_id != factory.context.identity.project
            || current.key.project() != current.project_id
            || current.verified_owner != Some(actor)
            || !peer_origin_matches(&fields, &origin)
            || !fields.get("peer_graph").is_some_and(|peer_graph| {
                peer_graph["host"] == serde_json::json!(factory.context.identity.host)
                    && peer_graph["graph_id"] == serde_json::json!(graph)
            })
            || !current.context.as_ref().is_some_and(|context| {
                context.graph_id.as_deref() == Some(graph)
                    && context.node_id.as_deref() == Some(current.key.node())
                    && super::steer::context_attempt_matches(
                        context.attempt,
                        current.key.attempt(),
                        predecessor.is_some(),
                    )
            })
        {
            return Err(FacadeError::StateCorrupt);
        }
        if let Some(previous) = &predecessor
            && (current.retry_of != Some(previous.attempt_id)
                || previous.superseded_by != Some(current.attempt_id)
                || !super::steer::successor_key_matches(&previous.key, &current.key)
                || current.project_id != previous.project_id)
        {
            return Err(FacadeError::StateCorrupt);
        }
        if checkpoint_target
            .as_ref()
            .is_some_and(|saved| saved.host == target.host && saved.attempt == target.attempt)
        {
            checkpoint_target_seen = true;
        }
        match current.superseded_by {
            Some(next)
                if matches!(
                    current.state,
                    provenant_records::run::AttemptState::Terminal { .. }
                ) =>
            {
                predecessor = Some(current);
                target.attempt = next;
            }
            Some(_) => return Err(FacadeError::StateCorrupt),
            None => {
                if !checkpoint_target_seen {
                    return Err(FacadeError::StateCorrupt);
                }
                break;
            }
        }
    }
    if !checkpoint_target_seen {
        return Err(FacadeError::StateCorrupt);
    }
    Ok(SubmissionReceipt {
        operation_id,
        targets: accepted.targets,
        worktree: None,
        extra: [(
            "peer_graph".into(),
            serde_json::json!({"host":factory.context.identity.host,"graph_id":graph}),
        )]
        .into(),
    })
}

pub(crate) fn peer_graph_fields(
    factory: &Factory,
    attempt: provenant_core::AttemptId,
) -> Result<provenant_records::ExtraFields, FacadeError> {
    peer_graph_fields_page(factory, attempt, 0)
}

pub(crate) fn peer_graph_fields_page(
    factory: &Factory,
    attempt: provenant_core::AttemptId,
    after: usize,
) -> Result<provenant_records::ExtraFields, FacadeError> {
    use provenant_fabric_runtime::graph::{FileJournal, relay};
    let cancel = CancelToken::new();
    let owner = BinaryRunOwner {
        factory,
        allow_remote: false,
        actor: None,
        chair: None,
    };
    let record = owner.snapshot(
        AttemptTarget {
            extra: Default::default(),
            host: None,
            attempt,
        },
        WaitBudget::nonblocking(factory.clock.mono_now()),
        &cancel,
    )?;
    let Some(graph) = record
        .context
        .as_ref()
        .and_then(|context| context.graph_id.as_deref())
        .filter(|id| id.starts_with("peer-"))
    else {
        return Ok(Default::default());
    };
    let collected = || -> Result<provenant_records::ExtraFields, FacadeError> {
        // Terminal checkpoints can be collected before their RunStore attempts.
        // Retain an explicit pointer so the origin can keep its own durable row.
        if !matches!(
            record.state,
            provenant_records::run::AttemptState::Terminal { .. }
        ) {
            return Err(FacadeError::StateCorrupt);
        }
        Ok([
            (
                "peer_graph".into(),
                serde_json::json!({"host":factory.context.identity.host,"graph_id":graph}),
            ),
            ("peer_graph_collected".into(), serde_json::json!(true)),
            (
                "peer_origin_work_key".into(),
                serde_json::json!(
                    FileJournal::peer_origin(
                        &factory.context.roots.state,
                        factory.context.identity.project,
                        graph,
                    )
                    .map_err(graph_journal_error)?
                    .ok_or(FacadeError::StateCorrupt)?
                ),
            ),
        ]
        .into())
    };
    let Some(checkpoint) = FileJournal::read_checkpoint(
        &factory.context.roots.state,
        factory.context.identity.project,
        graph,
    )
    .map_err(graph_journal_error)?
    else {
        return collected();
    };
    let reference = format!("{}/{}", factory.context.identity.host, attempt);
    if relay::historical_parent_of_attempt(&checkpoint, &reference).is_none() {
        return Ok(Default::default());
    }
    let Some(execution) = FileJournal::execution(
        &factory.context.roots.state,
        factory.context.identity.project,
        graph,
    )
    .map_err(graph_journal_error)?
    else {
        return collected();
    };
    let Some(origin) = execution.extra.get("peer_origin_context") else {
        return Ok(Default::default());
    };
    // Peer frames are bounded. This metadata projection omits captured results,
    // prompts, local paths and expansion fragments; output travels separately.
    let mut rows = Vec::new();
    let mut bytes = 0;
    if after > checkpoint.nodes.len() {
        return Err(FacadeError::InvalidInput { detail: None });
    }
    let mut next_node = None;
    for (index, node) in checkpoint.nodes.iter().enumerate().skip(after) {
        let mut selected = node.attempts.last().into_iter().collect::<Vec<_>>();
        if let Some(terminal) = node.attempts.iter().rev().find(|attempt| {
            attempt.state == provenant_fabric_runtime::graph::AttemptState::Terminal
                && attempt.attempt_ref.is_some()
        }) && selected
            .first()
            .is_none_or(|latest| latest.key != terminal.key)
        {
            selected.push(terminal);
        }
        let row = serde_json::json!({"id":node.id,"state":node.state,"worktree":null,
            "attempts":selected.into_iter().map(|attempt|serde_json::json!({
                "launched":attempt.launched,"key":attempt.key,"route":{"adapter":attempt.route.adapter,"model":attempt.route.model,"alias":attempt.route.alias,"name":attempt.route.name,"effort":attempt.route.effort,"account":attempt.route.account},
                "state":attempt.state,"attempt_ref":attempt.attempt_ref,"outcome":attempt.outcome,
                "launch_actor":attempt.launch_actor})).collect::<Vec<_>>()});
        let size = serde_json::to_vec(&row)
            .map_err(|_| FacadeError::StateCorrupt)?
            .len();
        if bytes + size > MAX_FRAME / 4 {
            if rows.is_empty() {
                return Err(FacadeError::InvalidInput { detail: None });
            }
            next_node = Some(index);
            break;
        }
        bytes += size;
        rows.push(row);
    }
    let snapshot = graph_snapshot(checkpoint);
    let subtree = serde_json::json!({"status":snapshot.status,"outcome":snapshot.outcome,
        "next_node":next_node,"checkpoint":{"schema_version":1,"graph_id":graph,
        "plan_digest":snapshot.checkpoint.plan_digest,"cancelled":snapshot.checkpoint.cancelled,"nodes":rows}});
    Ok([
        (
            "peer_graph".into(),
            serde_json::json!({"host":factory.context.identity.host,"graph_id":graph}),
        ),
        ("peer_origin_context".into(), origin.clone()),
        (
            "peer_origin_work_key".into(),
            execution
                .document
                .extra
                .get("peer_origin_work_key")
                .cloned()
                .ok_or(FacadeError::StateCorrupt)?,
        ),
        ("peer_subtree".into(), subtree),
    ]
    .into())
}

pub(super) fn peer_origin_matches(
    extra: &provenant_records::ExtraFields,
    expected: &provenant_records::WorkKey,
) -> bool {
    extra
        .get("peer_origin_work_key")
        .and_then(|value| serde_json::from_value::<provenant_records::WorkKey>(value.clone()).ok())
        .as_ref()
        == Some(expected)
}

pub(super) fn checked_output_origin(
    mut output: OutputResult,
    expected: &provenant_records::WorkKey,
) -> Result<OutputResult, FacadeError> {
    let (OutputResult::Chunk { extra, .. } | OutputResult::Artifact { extra, .. }) = &mut output;
    if !peer_origin_matches(extra, expected) {
        return Err(FacadeError::StateCorrupt);
    }
    // These authenticate the peer read; the CLI output schema contains lane
    // fields only. Consume transport metadata before rendering the payload.
    for field in [
        "peer_graph",
        "peer_origin_context",
        "peer_origin_work_key",
        "peer_graph_collected",
        "peer_subtree",
    ] {
        extra.remove(field);
    }
    Ok(output)
}

pub(crate) fn peer_cancel_operation(
    submission: &provenant_core::OperationId,
) -> Result<provenant_core::OperationId, FacadeError> {
    let digest =
        provenant_records::CanonicalV1::record_digest("peer-cancel-submission", 1, submission)
            .map_err(|_| FacadeError::StateCorrupt)?;
    format!("graph-{}", provenant_core::hex(digest.as_bytes()))
        .parse()
        .map_err(|_| FacadeError::StateCorrupt)
}

pub(crate) fn peer_cancel_submission_request(
    factory: &Factory,
    request: &PeerRequest,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<bool, FacadeError> {
    let PeerAction::CancelSubmission {
        submission,
        operation_id,
        stop,
        ..
    } = &request.action
    else {
        return Err(FacadeError::InvalidInput { detail: None });
    };
    // Use the same authenticated launch namespace as Submit. request.actor()
    // names the transport agent, rather than the retained graph launch actor.
    let actor = request
        .agent
        .strip_prefix("graph-")
        .and_then(|actor| actor.parse().ok())
        .ok_or(FacadeError::ConfinementRefused { resource: None })?;
    // Bind the cancellation to its submission rather than allowing callers to
    // reuse a cancellation identity for a different receiver graph.
    if *operation_id != peer_cancel_operation(submission)? {
        return Err(FacadeError::Conflict);
    }
    peer_cancel_submission(factory, actor, submission, *stop, budget, cancel)
}

pub(crate) fn peer_cancel_submission(
    factory: &Factory,
    actor: provenant_core::ActorId,
    submission: &provenant_core::OperationId,
    stop: bool,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<bool, FacadeError> {
    let graph = peer_graph_name(actor, submission)?;
    // Absence is not proof of cancellation: the accepted submission may still
    // be arriving. The origin retains its fence and retries after reconnect.
    let checkpoint = provenant_fabric_runtime::graph::FileJournal::read_checkpoint(
        &factory.context.roots.state,
        factory.context.identity.project,
        &graph,
    )
    .map_err(graph_journal_error)?
    .ok_or(FacadeError::NotFound)?;
    let checkpoint = if stop {
        factory
            .stop_target(
                &unified::GraphTarget {
                    graph,
                    node: None,
                    attempt: None,
                },
                Some("Cancelled by origin".into()),
                budget,
                cancel,
            )?
            .checkpoint
    } else {
        checkpoint
    };
    // Settled attached nodes prove that their attempts and custody have drained,
    // including children of a terminal parent. Receiver cancellation deliberately
    // preserves nodes marked by graph::relay::DETACHED_FROM_GRAPH_CANCEL.
    Ok(checkpoint.cancelled
        && checkpoint.nodes.iter().all(|node| {
            node.extra.get("detached_from_graph_cancel") == Some(&serde_json::Value::Bool(true))
                || !matches!(
                    node.state,
                    provenant_fabric_runtime::graph::NodeState::Pending
                        | provenant_fabric_runtime::graph::NodeState::Running
                )
        }))
}

pub(crate) fn peer_cancel_subtree(
    factory: &Factory,
    attempt: provenant_core::AttemptId,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<(), FacadeError> {
    let fields = peer_graph_fields(factory, attempt)?;
    if fields
        .get("peer_graph_collected")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        return Ok(());
    }
    let Some(graph) = fields
        .get("peer_graph")
        .and_then(|v| v["graph_id"].as_str())
    else {
        return Ok(());
    };
    let checkpoint = provenant_fabric_runtime::graph::FileJournal::read_checkpoint(
        &factory.context.roots.state,
        factory.context.identity.project,
        graph,
    )
    .map_err(graph_journal_error)?
    .ok_or(FacadeError::NotFound)?;
    let reference = format!("{}/{}", factory.context.identity.host, attempt);
    let node = provenant_fabric_runtime::graph::relay::historical_parent_of_attempt(
        &checkpoint,
        &reference,
    )
    .ok_or(FacadeError::NotFound)?;
    factory.stop_target(
        &unified::GraphTarget {
            graph: graph.into(),
            node: Some(node.into()),
            attempt: None,
        },
        Some("Cancelled by origin".into()),
        budget,
        cancel,
    )?;
    Ok(())
}

pub(super) fn peer_retained_receipt(
    factory: &Factory,
    operation_id: &provenant_core::OperationId,
    actor: provenant_core::ActorId,
) -> Result<Option<SubmissionReceipt>, FacadeError> {
    use provenant_fabric_runtime::graph::FileJournal;
    let graph = peer_graph_name(actor, operation_id)?;
    FileJournal::read_checkpoint(
        &factory.context.roots.state,
        factory.context.identity.project,
        &graph,
    )
    .map_err(graph_journal_error)?
    .map(|checkpoint| peer_graph_receipt(factory, operation_id.clone(), &graph, &checkpoint))
    .transpose()
}
