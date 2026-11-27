use super::*;
use provenant_fabric_runtime::runs::SubmissionReceipt;
use provenant_records::warning::PEER_UNREACHABLE;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

fn console_tree_command(
    max_depth: u64,
    max_nodes: u64,
    detail: bool,
    subtree: Option<String>,
) -> provenant_cli::console::Console {
    provenant_cli::console::Console::Tree {
        max_depth,
        max_nodes,
        detail,
        hosts: None,
        subtree,
    }
}

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
            crate::REPAIR_IO,
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
        PeerAction::Gc { .. } => false,
        PeerAction::CancelSubmission { stop, .. } => !stop,
        PeerAction::Inbox {
            input:
                provenant_fabric_runtime::inbox::InboxInput::Steer {
                    graph_context: Some(_),
                    ..
                },
            ..
        } => false,
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

fn prepare_peer_project_roots(
    globals: &provenant_cli::Globals,
    roots: provenant_core::Roots,
    common: &Path,
    read_only: bool,
    product: crate::roots::Product,
) -> Result<(provenant_core::Roots, provenant_config::ProjectBinding), Box<dyn std::error::Error>> {
    let location = provenant_config::ProjectLocation::new(common.to_path_buf())?;
    let existing = provenant_config::lookup_project(&roots, &location)?;
    if read_only && existing.is_none() {
        return Err(PeerError::Project.into());
    }

    // Resolve the verified layout and root protection before allocating a
    // project binding or host identity in the supplied state root.
    let (project_root, git_metadata) =
        Context::workspace_for_common_location(&roots, common, existing.as_ref())?;
    let workspace = match git_metadata.as_deref() {
        Some(common) => crate::roots::Workspace::VerifiedLayout {
            root: &project_root,
            git_metadata: Some(common),
        },
        None => crate::roots::Workspace::At(&project_root),
    };
    let resolved = crate::roots::resolve(globals, workspace, product)?;

    let binding = match existing {
        Some(binding) => binding,
        None => provenant_config::bind_project(&resolved.roots, location)?,
    };
    if !read_only && provenant_config::lookup_host(&resolved.roots)?.is_none() {
        provenant_config::bind_host(&resolved.roots)?;
    }
    Ok((resolved.roots, binding))
}

fn validate_graph_steer(
    factory: &Factory,
    owner: &BinaryRunOwner<'_>,
    request: &PeerRequest,
    input: &provenant_fabric_runtime::inbox::InboxInput,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<(), FacadeError> {
    use provenant_fabric_runtime::inbox::InboxInput;
    let InboxInput::Steer {
        attempt: requested_attempt,
        operation_id: Some(_),
        graph_context: Some(graph_context),
        ..
    } = input
    else {
        return Err(FacadeError::field("graph_context"));
    };
    let actor = graph_context.launch_actor;
    if request.agent != format!("graph-{actor}")
        || request.actor().ok() != Some(factory.context.identity.inbox.actor)
    {
        return Err(FacadeError::ConfinementRefused { resource: None });
    }
    let launch_operation = &graph_context.launch_operation;
    let origin = &graph_context.origin_work_key;
    let receipt = peer_retained_receipt(factory, launch_operation, actor)?.ok_or(
        FacadeError::LaunchUnknown {
            operation_id: (*launch_operation).clone(),
        },
    )?;
    let expected_graph = peer_graph_name(actor, launch_operation)?;
    if receipt.operation_id != *launch_operation
        || receipt.targets.len() != 1
        || !receipt.peer_graph.as_ref().is_some_and(|graph| {
            graph.host == factory.context.identity.host && graph.graph_id == expected_graph
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
        let peer_graph = fields.peer_graph.as_ref();
        if record.attempt_id != target.attempt
            || record.verified_owner != Some(factory.context.identity.inbox.actor)
            || record.project_id != factory.context.identity.project
            || record.key.project() != factory.context.identity.project
            || record.key.graph() != expected_graph
            || record.key.node() != origin.node()
            || !peer_origin_matches(&fields, origin)
            || !peer_graph
                .is_some_and(|graph| graph.host == local && graph.graph_id == expected_graph)
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

pub(super) fn validate_graph_output(
    factory: &Factory,
    owner: &BinaryRunOwner<'_>,
    request: &PeerRequest,
    audience: &provenant_fabric_runtime::hosts::OutputAudience,
    input: &OutputInput,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<(), FacadeError> {
    let provenant_fabric_runtime::hosts::OutputAudience::GraphExecution {
        launch_actor: actor,
        launch_operation: operation,
        origin_work_key: origin,
    } = audience
    else {
        return Err(FacadeError::ConfinementRefused { resource: None });
    };
    if request.agent != format!("graph-{actor}")
        || request.actor().ok() != Some(factory.context.identity.inbox.actor)
    {
        return Err(FacadeError::ConfinementRefused { resource: None });
    }
    let requested = match input {
        OutputInput::Read {
            target,
            part: OutputPart::Result,
            ..
        } => target,
        _ => return Err(FacadeError::ConfinementRefused { resource: None }),
    };
    let receipt = peer_retained_receipt(factory, operation, *actor)?.ok_or_else(|| {
        FacadeError::LaunchUnknown {
            operation_id: operation.clone(),
        }
    })?;
    let graph = peer_graph_name(*actor, operation)?;
    let local = factory.context.identity.host;
    if receipt.operation_id != *operation
        || receipt.targets.len() != 1
        || receipt.targets.first() != Some(requested)
        || requested.host != Some(local)
        || !receipt
            .peer_graph
            .as_ref()
            .is_some_and(|g| g.host == local && g.graph_id == graph)
    {
        return Err(FacadeError::ConfinementRefused { resource: None });
    }
    let record = owner.snapshot(requested.clone(), budget, cancel)?;
    let fields = peer_graph_fields(factory, requested.attempt)?;
    if record.attempt_id != requested.attempt
        || record.verified_owner != Some(factory.context.identity.inbox.actor)
        || record.project_id != factory.context.identity.project
        || record.key.project() != factory.context.identity.project
        || record.key.graph() != graph
        || record.key.node() != origin.node()
        || !peer_origin_matches(&fields, origin)
        || !fields
            .peer_graph
            .as_ref()
            .is_some_and(|g| g.host == local && g.graph_id == graph)
    {
        return Err(FacadeError::ConfinementRefused { resource: None });
    }
    Ok(())
}

fn attach_output_graph_fields(
    factory: &Factory,
    data: &mut OutputResult,
) -> Result<(), FacadeError> {
    let (OutputResult::Chunk { peer, target, .. } | OutputResult::Artifact { peer, target, .. }) =
        data;
    let mut fields = peer_graph_fields(factory, target.attempt)?;
    fields.peer_subtree = None;
    peer.merge(fields);
    Ok(())
}

// Project the complete verified payload before slicing it into peer pages.
fn project_peer_output(
    owner: &mut BinaryRunOwner<'_>,
    input: &OutputInput,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<OutputResult, FacadeError> {
    let projector = provenant_confinement::publication::Projector::ambient()
        .map_err(|_| FacadeError::io("peer publication projection failed"))?;
    project_peer_output_with(input, &projector, |read| {
        let mut data = owner.output_for_peer(read, budget, cancel)?;
        attach_output_graph_fields(owner.factory, &mut data)?;
        Ok(data)
    })
}

fn project_peer_output_with(
    input: &OutputInput,
    projector: &provenant_confinement::publication::Projector,
    mut read_page: impl FnMut(OutputInput) -> Result<OutputResult, FacadeError>,
) -> Result<OutputResult, FacadeError> {
    const CAP: u64 = 16 * 1024 * 1024;
    let failure = || FacadeError::io("peer publication projection failed");
    let (requested_offset, requested_max, events) = match input {
        OutputInput::Read {
            offset,
            max_bytes,
            part,
            ..
        } => (*offset, *max_bytes, *part == OutputPart::Events),
        OutputInput::Fetch { max_bytes, .. } => (0, *max_bytes, false),
    };
    if matches!(input, OutputInput::Read { .. })
        && !(1..=provenant_records::run::OUTPUT_LIMIT).contains(&requested_max)
    {
        return Err(super::run_error(provenant_run::RunError::TooLarge {
            limit: provenant_records::run::OUTPUT_LIMIT,
        }));
    }
    let mut read = input.clone();
    if let OutputInput::Read {
        offset, max_bytes, ..
    } = &mut read
    {
        if events && *offset != 0 {
            return Err(failure());
        }
        *offset = 0;
        *max_bytes = provenant_records::run::OUTPUT_LIMIT;
    }
    let mut first = None;
    let mut raw = Vec::new();
    let mut cursor = 0;
    let mut binding = None;
    loop {
        let page = read_page(read.clone())?;
        let (bytes, done, next) = match &page {
            OutputResult::Chunk {
                bytes,
                eof,
                offset,
                next_offset,
                digest,
                length,
                ..
            } => {
                if *offset != cursor
                    || length.is_some_and(|len| len > CAP)
                    || (!events && binding.is_some_and(|d| d != *digest))
                {
                    return Err(failure());
                }
                binding = Some(*digest);
                (bytes, *eof, *next_offset)
            }
            OutputResult::Artifact { bytes, length, .. } => {
                if length.is_some_and(|len| len > CAP) {
                    return Err(failure());
                }
                (bytes, true, 0)
            }
        };
        let page_bytes = match bytes {
            OutputBytes::Utf8(text) => text.as_bytes().to_vec(),
            OutputBytes::Base64(text) => {
                provenant_fabric_runtime::handoff::decode_base64(text).ok_or_else(failure)?
            }
        };
        raw.extend_from_slice(&page_bytes);
        if raw.len() as u64 > CAP {
            return Err(failure());
        }
        if first.is_none() {
            first = Some(page);
        }
        if done {
            break;
        }
        if events || next <= cursor {
            return Err(failure());
        }
        cursor = next;
        if let OutputInput::Read { offset, .. } = &mut read {
            *offset = cursor;
        } else {
            return Err(failure());
        }
    }
    let mut value = first.ok_or_else(failure)?;
    let text = String::from_utf8(raw).map_err(|_| failure())?;
    match &mut value {
        OutputResult::Chunk { bytes, .. } | OutputResult::Artifact { bytes, .. } => {
            *bytes = OutputBytes::Utf8(text)
        }
    }
    // Only complete free text is projected. Wire tags, IDs, extensions and
    // artifact metadata are transport structure, never publication content.
    let mut output = value;
    let mut texts = Vec::new();
    match &mut output {
        OutputResult::Chunk {
            bytes: OutputBytes::Utf8(text),
            reason,
            planner_fallback,
            extra,
            ..
        } => {
            texts.push(text);
            provenant_fabric_runtime::hosts::output_lane_texts(extra, &mut texts);
            texts.extend(reason.iter_mut());
            texts.extend(planner_fallback.iter_mut());
        }
        OutputResult::Artifact {
            bytes: OutputBytes::Utf8(text),
            planner_fallback,
            extra,
            ..
        } => {
            texts.push(text);
            provenant_fabric_runtime::hosts::output_lane_texts(extra, &mut texts);
            texts.extend(planner_fallback.iter_mut());
        }
        _ => return Err(failure()),
    }
    provenant_confinement::publication::CrossHostTransport::new(projector)
        .texts(&mut texts)
        .map_err(|_| failure())?;
    match &mut output {
        OutputResult::Chunk {
            bytes,
            length,
            offset,
            next_offset,
            eof,
            digest,
            ..
        } => {
            let OutputBytes::Utf8(text) = bytes else {
                return Err(failure());
            };
            let all = text.as_bytes();
            *digest = provenant_core::Digest::from_bytes(
                *provenant_records::CanonicalV1::file_digest(all).bytes(),
            );
            if !events {
                let start = usize::try_from(requested_offset).map_err(|_| failure())?;
                if start > all.len() {
                    return Err(super::run_error(provenant_run::RunError::Validation(
                        "offset past EOF",
                    )));
                }
                let end = start
                    .saturating_add(usize::try_from(requested_max).map_err(|_| failure())?)
                    .min(all.len());
                *length = Some(all.len() as u64);
                *offset = start as u64;
                *next_offset = end as u64;
                *eof = end == all.len();
                *bytes = OutputBytes::from_bytes(all[start..end].to_vec());
            } else if all.len() as u64 > requested_max {
                return Err(failure());
            }
        }
        OutputResult::Artifact {
            bytes,
            length,
            digest,
            artifact,
            ..
        } => {
            let OutputBytes::Utf8(text) = bytes else {
                return Err(failure());
            };
            if text.len() as u64 > requested_max {
                return Err(FacadeError::OutputTooLarge);
            }
            let file_digest = provenant_records::CanonicalV1::file_digest(text.as_bytes());
            *length = Some(text.len() as u64);
            *digest = provenant_core::Digest::from_bytes(*file_digest.bytes());
            artifact.digest = Some(file_digest);
        }
    }
    Ok(output)
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
            let projector = provenant_confinement::publication::Projector::ambient()?;
            let mut reply =
                provenant_fabric_runtime::hosts::encode_peer_response(refusal, &projector)?;
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
            let state = host.roots.state.clone();
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
        // A Create request names a fixture that does not exist on this host
        // yet. Build and bind it before project-root preparation needs it;
        // Remove still sees it until its own arm deletes it. The root is
        // validated on both sides: an absolute or `..` root is a real removal
        // boundary and is refused here rather than ever reaching `remove_dir`.
        let mut smoke_remove_root: Option<PathBuf> = None;
        let smoke_created = match &request.action {
            PeerAction::SmokeFixture {
                op: SmokeFixtureOp::Create { nonce, root, .. },
                ..
            } => {
                let root =
                    provenant_cli::smoke::validate_root(root).map_err(|_| PeerError::Project)?;
                let expected = crate::smoke::fixture_root(&home, &root)
                    .join(nonce)
                    .join(".git");
                if common != expected {
                    return Err(PeerError::Project.into());
                }
                Some(crate::smoke::create_fixture(
                    host.roots.state.as_path(),
                    &home,
                    nonce,
                    &root,
                )?)
            }
            PeerAction::SmokeFixture {
                op: SmokeFixtureOp::Remove { nonce },
                ..
            } => {
                // Remove carries no root: derive it from the home-relative
                // locator path, which ProjectLocator::resolve already proved
                // has only normal components, then apply the same validator.
                let fixture_dir = common
                    .parent()
                    .and_then(Path::parent)
                    .ok_or(PeerError::Project)?;
                let relative = fixture_dir
                    .strip_prefix(&home)
                    .map_err(|_| PeerError::Project)?;
                let root = provenant_cli::smoke::validate_root(&relative.to_string_lossy())
                    .map_err(|_| PeerError::Project)?;
                let expected = crate::smoke::fixture_root(&home, &root)
                    .join(nonce)
                    .join(".git");
                if common != expected {
                    return Err(PeerError::Project.into());
                }
                smoke_remove_root = Some(root);
                None
            }
            _ => None,
        };
        if provenant_os::fs::canonicalize(&common)? != common {
            return Err(PeerError::Project.into());
        }
        let (project_roots, binding) = prepare_peer_project_roots(
            globals,
            host.roots,
            &common,
            peer_read_only(&request.action),
            crate::roots::Product::Required,
        )?;
        let assets = host.assets;
        // An answer without an operation ID is refused below, after peer validation,
        // and must not initialise writable project state on its way there.
        let mut context = Context::open_for_project(project_roots, &home, &binding)?;
        let cwd = context.project_root.clone();
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
            PeerAction::SmokeFixture { op, .. } => {
                let state = peer_factory.context.roots.state.as_path();
                let refused = || PeerResult::Error {
                    data: provenant_fabric_runtime::hosts::PeerResultError {
                        extra: Default::default(),
                        error: FacadeError::invalid(InvalidInputDetail::Field(
                            "smoke_fixture".into(),
                        )),
                    },
                    extra: Default::default(),
                };
                match op {
                    SmokeFixtureOp::Create { .. } => match smoke_created {
                        Some(commits) => PeerResult::SmokeFixture {
                            data: SmokeFixtureReceipt {
                                extra: Default::default(),
                                base: Some(commits.base),
                                head: Some(commits.head),
                                recipe: Some(crate::smoke::FIXTURE_RECIPE),
                                reason: None,
                            },
                            extra: Default::default(),
                        },
                        None => refused(),
                    },
                    SmokeFixtureOp::Remove { nonce } => match smoke_remove_root.as_deref() {
                        Some(root) => match crate::smoke::remove_fixture(state, &home, nonce, root)
                        {
                            Ok(()) => PeerResult::SmokeFixture {
                                data: SmokeFixtureReceipt {
                                    extra: Default::default(),
                                    base: None,
                                    head: None,
                                    recipe: None,
                                    reason: Some("removed".into()),
                                },
                                extra: Default::default(),
                            },
                            Err(_) => refused(),
                        },
                        None => refused(),
                    },
                }
            }
            PeerAction::Inbox { input, .. } => {
                use provenant_fabric_runtime::inbox::{InboxInput, InboxReply};
                let graph_steer = matches!(
                    input,
                    InboxInput::Steer {
                        graph_context: Some(_),
                        ..
                    }
                );
                let validation = if graph_steer {
                    validate_graph_steer(&peer_factory, &owner, &request, input, budget, &cancel)
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
                            error: FacadeError::field("limit"),
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
                subtree,
                ..
            } => {
                let command =
                    console_tree_command(*max_depth, *max_nodes, *detail, subtree.clone());
                if *max_nodes > context.config.limits.page_rows {
                    PeerResult::Error {
                        data: provenant_fabric_runtime::hosts::PeerResultError {
                            extra: Default::default(),
                            error: FacadeError::field("max_nodes"),
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
                    error(FacadeError::field("limit"))
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
                            error: FacadeError::field("id"),
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
                            error: FacadeError::field("targets"),
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
            PeerAction::Run {
                input,
                peer_subtree_after,
                ..
            } => {
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
                            attempt.peer.graph_custody_released = Some(released);
                            // The sealed settlement handoff, so the chair never
                            // re-derives it from the result text.
                            attempt.peer.graph_handoff =
                                Some(store.handoff(attempt).map_err(run_error)?);
                        }
                        if let provenant_fabric_runtime::runs::RunResult::Status {
                            attempt, ..
                        } = &mut value
                        {
                            let after = peer_subtree_after.unwrap_or(0);
                            attempt.peer.merge(peer_graph_fields_page(
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
            PeerAction::Submit {
                input,
                graph_context,
                ..
            } => peer_result(
                (|| {
                    let actor = graph_context.launch_actor;
                    if request.agent != format!("graph-{actor}") {
                        return Err(FacadeError::ConfinementRefused { resource: None });
                    }
                    let mut attempt = graph_context.attempt.clone();
                    // A sender checkout is local to its host. Ignore it rather
                    // than decode it as a binder path on this host (#3744).
                    if attempt.directory.take().is_some() {
                        super::interface::warn_operator(
                            "ignored a sender-local attempt directory; the peer binds its own source checkout",
                        );
                    }
                    // A peer creates a writer from the retained branch and
                    // base in its own clone. Host placement and filesystem
                    // grants never cross the boundary.
                    if attempt.host.is_some()
                        || !attempt.write_roots.is_empty()
                        || !attempt.capabilities.is_empty()
                    {
                        return Err(FacadeError::ConfinementRefused { resource: None });
                    }
                    let SubmitInput::Submit { task, host, .. } = &**input;
                    let caller_path = task
                        .worktree
                        .as_deref()
                        .is_some_and(|path| !path.is_empty());
                    let peer_writer = task.access
                        == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite
                        && !caller_path
                        && task.new_worktree.is_some()
                        && attempt.new_worktree.is_some();
                    if host.is_some()
                        || task
                            .context
                            .as_ref()
                            .is_some_and(|context| context.graph_template_launch)
                        || (task.access != provenant_fabric_runtime::graph::AccessMode::ReadOnly
                            && !peer_writer)
                        || caller_path
                        || (task.access == provenant_fabric_runtime::graph::AccessMode::ReadOnly
                            && task.new_worktree.is_some())
                    {
                        return Err(FacadeError::ConfinementRefused { resource: None });
                    }
                    let retry = graph_context.retry_limit;
                    let network = graph_context.network;
                    // A writer gets its own worktree from provisioning; only a
                    // reader binds the peer's source checkout.
                    let access = if peer_writer {
                        None
                    } else {
                        Some(super::peer_source_binding::bind_reader_source(
                            context.roots.state.as_path(),
                            &cwd,
                            network,
                        )?)
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
                        access.and_then(|access| access.network).or(network),
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
            PeerAction::Output {
                audience, input, ..
            } => peer_result(
                (|| {
                    if matches!(
                        audience,
                        provenant_fabric_runtime::hosts::OutputAudience::GraphExecution { .. }
                    ) {
                        validate_graph_output(
                            &peer_factory,
                            &owner,
                            &request,
                            audience,
                            input,
                            budget,
                            &cancel,
                        )?;
                    }
                    project_peer_output(&mut owner, input, budget, &cancel)
                })(),
                |data| PeerResult::Output {
                    data: Box::new(data),
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
                            error: FacadeError::field("targets"),
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
                data: crate::session_handoff::receive(
                    &crate::session_handoff::Target {
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
                match crate::hosts::local(
                    &home,
                    &context.roots,
                    &provenant_confinement::worktrees::SystemWorkerObserver,
                )
                .and_then(|report| {
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
                graph_context,
                ..
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
                            if request.agent.starts_with("graph-") != graph_context.is_some() {
                                return Err(FacadeError::ConfinementRefused { resource: None });
                            }
                            let receipt = if let Some(graph_context) = graph_context {
                                let actor = graph_context.launch_actor;
                                if request.agent != format!("graph-{actor}") {
                                    return Err(FacadeError::ConfinementRefused { resource: None });
                                }
                                let origin = &graph_context.origin_work_key;
                                let expected_graph = peer_graph_name(actor, operation_id)?;
                                let receipt =
                                    peer_retained_receipt(&peer_factory, operation_id, actor)?;
                                if let Some(receipt) = &receipt {
                                    if receipt.operation_id != *operation_id
                                        || receipt.targets.len() != 1
                                        || !receipt.peer_graph.as_ref().is_some_and(|graph| {
                                            graph.host == context.identity.host
                                                && graph.graph_id == expected_graph
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
                                        || !peer_origin_matches(&fields, origin)
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
            PeerAction::Gc {
                dry_run,
                apply,
                scheduled,
                pressure,
                free_floor,
                idle,
                ..
            } => peer_result(
                (|| -> Result<serde_json::Value, FacadeError> {
                    let command = provenant_cli::Gc {
                        common: provenant_cli::CommonArgs::default(),
                        scheduled: *scheduled,
                        pressure: *pressure,
                        free_floor: *free_floor,
                        dry_run: *dry_run,
                        apply: *apply,
                        finished_branch: None,
                        idle: *idle,
                    };
                    let globals = provenant_cli::Globals {
                        json: true,
                        full: false,
                        state_dir: Some(peer_factory.context.roots.state.as_path().to_owned()),
                        project: Some(cwd.clone()),
                    };
                    crate::worktrees::gc_for_peer(&command, &globals, workers).map_err(|reason| {
                        FacadeError::DispatchRefused {
                            reason,
                            disk: None,
                            fact: None,
                        }
                    })
                })(),
                |data| PeerResult::Gc {
                    data,
                    extra: Default::default(),
                },
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
                                context.local_observation.clone(),
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
        let projector = provenant_confinement::publication::Projector::ambient()?;
        let mut bytes =
            provenant_fabric_runtime::hosts::encode_peer_response(response, &projector)?;
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
    // Even an older sender's document is embedded only through the peer-bound
    // projection: host-local paths never reach this host's journal (#3744).
    let attempt = attempt.peer_bound();
    let facts = provenant_confinement::worktrees::RepoFacts::default();
    let submission_digest = provenant_records::CanonicalV1::record_digest(
        "peer-submission",
        1,
        &(&input, actor, &attempt, network, retry),
    )
    .map_err(|_| FacadeError::StateCorrupt)?;
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
    let route = match &task.route {
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
    let mut route: provenant_fabric_runtime::graph::GraphRoute =
        serde_json::from_value(route).map_err(|_| FacadeError::StateCorrupt)?;
    route.required_family = task.route.required_family().map(str::to_owned);
    route.peer_requested_route = Some(task.route.clone());
    route.peer_network = network;
    route.peer_retry_limit = retry;
    route.peer_source_head = task.source_head.clone();
    let origin = task
        .context
        .clone()
        .ok_or_else(|| FacadeError::field("context"))?;
    let node = origin
        .node_id
        .clone()
        .ok_or_else(|| FacadeError::field("context.node_id"))?;
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
                .ok_or_else(|| FacadeError::field("knobs"))
                .and_then(|value| {
                    serde_json::from_str::<Vec<String>>(value)
                        .map_err(|_| FacadeError::field("knobs"))
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
    let mode = match task.access {
        provenant_fabric_runtime::graph::AccessMode::ReadOnly => "read_only",
        provenant_fabric_runtime::graph::AccessMode::WorktreeWrite => "worktree_write",
    };
    let peer_writer = task.access == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite;
    let worktree = if peer_writer {
        // The peer creates this worktree in its own clone from the branch and
        // base carried in `attempt.new_worktree`. Resolve the path against the
        // peer's own managed layout, never against a caller-supplied path.
        attempt
            .new_worktree
            .as_ref()
            .and_then(|new| {
                factory
                    .context
                    .port_table
                    .managed_writer_path(&new.branch)
                    .ok()
                    .map(|path| factory.context.project_root.join(path))
            })
            .map(|path| path.to_string_lossy().into_owned())
    } else {
        None
    };
    let mut document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version":if task.subject.is_some() { 2 } else { 1 },
        "graph_id":graph,"template_library":{},"exclude":exclude,
        "nodes":[{"id":node,"access":mode,"prompt":task.prompt,"title":task.title,
          "worktree":worktree,"route":route,"attempt":attempt,"context":origin,
          "require_skill_location":task.require_skill_location,
          "settlement_retries":retry}],
    }))
    .map_err(|_| FacadeError::StateCorrupt)?;
    // Preserve the origin's semantic role so review launches use the same
    // comparison-base path as local graph nodes, including exact model routes.
    document.nodes[0].role = origin.role.clone();
    // Carry the sender-staged fixture bytes, not a sender path, so the peer
    // places exactly the same fixture in its own candidate scratch.
    document.nodes[0].fixture_bytes = task.fixture.clone();
    // A reader compares against the initiator's pinned base. A peer-owned
    // writer starts from its freshly provisioned worktree and has no range.
    document.review_base = if peer_writer { None } else { task.base.clone() };
    // The initiator's admitted subject travels with the task so this host's
    // graph tasks carry the same provenance beside their pinned head.
    document.subject = task.subject.clone();
    document.peer_origin_work_key = Some(origin_work_key.clone());
    // A reader reads the initiator's admitted subject: the peer receives
    // task.head and never substitutes its own checkout HEAD, so both hosts
    // read the same commit (spec invariant 3). A clone that lacks the commit
    // refuses at materialisation instead of reading a different tree. A
    // peer-owned writer provisions its worktree from the branch and base
    // instead, so a source binding would conflict with that intent.
    document.nodes[0].source = if peer_writer {
        None
    } else {
        task.head
            .clone()
            .map(|head| provenant_fabric_runtime::graph::SourceBinding::Commit { commit: head })
    };
    let review_base = if peer_writer {
        None
    } else {
        let resolved = task.base.as_deref().and_then(|base| {
            factory
                .resolve_base(&factory.cwd, base, &facts, cancel)
                .ok()
        });
        if resolved.is_none() && task.base.is_some() {
            super::interface::warn_operator(
                "peer reader could not resolve the initiator's review base in its own checkout; review ranges fall back to the source commit",
            );
        }
        resolved
    };
    document.network = network.map(|network| match network {
        provenant_cli::Switch::On => provenant_fabric_runtime::graph::GraphNetwork::On,
        provenant_cli::Switch::Off => provenant_fabric_runtime::graph::GraphNetwork::Off,
    });
    // The sender stamps this document's schema from its fields before it
    // crosses. The peer rebuilds the document from the submitted task and sets
    // a review base (a schema-2 field) on it, so it must derive the same schema
    // or its own dispatch admits a version-1 document carrying version-2
    // fields and refuses with GraphSchema (#3752).
    document.schema_version = document.schema_version.max(u64::from(
        provenant_fabric_runtime::graph::minimum_schema_version(&document),
    ));
    provenant_fabric_runtime::graph::patch::submission(
        &mut document,
        &operation_id.to_string(),
        &factory.context.config.graph.limits,
        factory.context.port_table.worktree_layout.clone(),
    )
    .map_err(super::interface::graph_patch_error)?;
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
        && bound.peer_submission_digest.as_ref() != Some(&submission_digest)
    {
        return Err(FacadeError::Conflict);
    }
    journal
        .bind(GraphExecution {
            bases: GraphBaseBinding::new(review_base, None),
            directory: factory.cwd.clone(),
            peer_origin_context: Some(origin.clone()),
            peer_submission_digest: Some(submission_digest),
            requester_path: Some(factory.requester_lane_path()?),
            extra: Default::default(),
            document: document.clone(),
        })
        .map_err(|_| FacadeError::Conflict)?;
    let mut owner = BoundGraphOwner {
        writer_launch_budget: None,
        admission_timeout: None,
        repo_facts: facts,
        launch_warnings: Vec::new(),
        factory,
        integration_base: GraphBaseBinding::new(review_base, None),
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
        let origin = fields
            .peer_origin_work_key
            .as_ref()
            .ok_or(FacadeError::StateCorrupt)?;
        if current.project_id != factory.context.identity.project
            || current.key.project() != current.project_id
            || current.verified_owner != Some(actor)
            || !peer_origin_matches(&fields, origin)
            || !fields.peer_graph.as_ref().is_some_and(|peer_graph| {
                peer_graph.host == factory.context.identity.host && peer_graph.graph_id == graph
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
        extra: Default::default(),
        operation_id,
        targets: accepted.targets,
        worktree: None,
        peer_graph: Some(provenant_records::run::PeerGraph {
            host: factory.context.identity.host,
            graph_id: graph.to_owned(),
        }),
    })
}

pub(crate) fn peer_graph_fields(
    factory: &Factory,
    attempt: provenant_core::AttemptId,
) -> Result<provenant_records::run::PeerGraphFields, FacadeError> {
    peer_graph_fields_page(factory, attempt, 0)
}

pub(crate) fn peer_graph_fields_page(
    factory: &Factory,
    attempt: provenant_core::AttemptId,
    after: usize,
) -> Result<provenant_records::run::PeerGraphFields, FacadeError> {
    use provenant_fabric_runtime::graph::{FileJournal, relay};
    use provenant_records::run::{
        PeerGraph, PeerGraphAttempt, PeerGraphCheckpoint, PeerGraphFields, PeerGraphNode,
        PeerGraphRoute, PeerGraphSubtree,
    };
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
        return Ok(PeerGraphFields::default());
    };
    let collected = || -> Result<PeerGraphFields, FacadeError> {
        // Terminal checkpoints can be collected before their RunStore attempts.
        // Retain an explicit pointer so the origin can keep its own durable row.
        if !matches!(
            record.state,
            provenant_records::run::AttemptState::Terminal { .. }
        ) {
            return Err(FacadeError::StateCorrupt);
        }
        let origin = FileJournal::peer_origin(
            &factory.context.roots.state,
            factory.context.identity.project,
            graph,
        )
        .map_err(graph_journal_error)?
        .ok_or(FacadeError::StateCorrupt)?;
        Ok(PeerGraphFields {
            peer_graph: Some(PeerGraph {
                host: factory.context.identity.host,
                graph_id: graph.to_owned(),
            }),
            peer_graph_collected: Some(true),
            peer_origin_work_key: Some(origin),
            ..Default::default()
        })
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
        return Ok(PeerGraphFields::default());
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
    let Some(origin) = execution.peer_origin_context.as_ref() else {
        return Ok(PeerGraphFields::default());
    };
    // Peer frames are bounded. This metadata projection omits captured results,
    // prompts, local paths and expansion fragments; output travels separately.
    let mut rows = Vec::new();
    let mut bytes = 0;
    if after > checkpoint.nodes.len() {
        return Err(FacadeError::field("after"));
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
        let row = PeerGraphNode {
            id: node.id.clone(),
            state: node.state,
            worktree: None,
            attempts: selected
                .into_iter()
                .map(|attempt| PeerGraphAttempt {
                    launched: attempt.launched,
                    key: attempt.key.clone(),
                    route: PeerGraphRoute {
                        extra: Default::default(),
                        adapter: attempt.route.adapter.clone(),
                        model: attempt.route.model.clone(),
                        alias: attempt.route.alias.clone(),
                        name: attempt.route.name.clone(),
                        effort: attempt.route.effort.clone(),
                        account: attempt.route.account.clone(),
                        required_family: attempt.route.required_family.clone(),
                    },
                    state: attempt.state,
                    attempt_ref: attempt.attempt_ref.clone(),
                    outcome: attempt.outcome,
                    launch_actor: attempt.launch_actor.clone(),
                })
                .collect(),
        };
        let size = serde_json::to_vec(&row)
            .map_err(|_| FacadeError::StateCorrupt)?
            .len();
        if bytes + size > MAX_FRAME / 4 {
            if rows.is_empty() {
                return Err(FacadeError::invalid(
                    provenant_fabric_runtime::coordination::facade::InvalidInputDetail::FieldTooLarge {
                        field: "nodes".into(),
                        limit: MAX_FRAME / 4,
                    },
                ));
            }
            next_node = Some(index);
            break;
        }
        bytes += size;
        rows.push(row);
    }
    let snapshot = graph_snapshot(checkpoint);
    let subtree = PeerGraphSubtree {
        status: snapshot.status,
        outcome: snapshot.outcome,
        next_node,
        checkpoint: PeerGraphCheckpoint {
            schema_version: 1,
            graph_id: graph.to_owned(),
            plan_digest: snapshot.checkpoint.plan_digest.clone(),
            cancelled: snapshot.checkpoint.cancelled,
            nodes: rows,
        },
    };
    Ok(PeerGraphFields {
        peer_graph: Some(PeerGraph {
            host: factory.context.identity.host,
            graph_id: graph.to_owned(),
        }),
        peer_origin_context: Some(origin.clone()),
        peer_origin_work_key: Some(
            execution
                .document
                .peer_origin_work_key
                .clone()
                .ok_or(FacadeError::StateCorrupt)?,
        ),
        peer_subtree: Some(subtree),
        ..Default::default()
    })
}

pub(super) fn peer_origin_matches(
    fields: &provenant_records::run::PeerGraphFields,
    expected: &provenant_records::WorkKey,
) -> bool {
    fields.peer_origin_work_key.as_ref() == Some(expected)
}

pub(super) fn checked_output_origin(
    mut output: OutputResult,
    expected: &provenant_records::WorkKey,
) -> Result<OutputResult, FacadeError> {
    let (OutputResult::Chunk { peer, .. } | OutputResult::Artifact { peer, .. }) = &mut output;
    if !peer_origin_matches(peer, expected) {
        return Err(FacadeError::StateCorrupt);
    }
    // Graph identity authenticates the peer read. Output length and planner
    // fallback remain owned by the output envelope for the CLI projection.
    *peer = Default::default();
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
        graph_context,
        submission,
        operation_id,
        stop,
        ..
    } = &request.action
    else {
        return Err(FacadeError::field("action"));
    };
    // Use the same authenticated launch namespace as Submit. request.actor()
    // names the transport agent, rather than the retained graph launch actor.
    let actor = graph_context.launch_actor;
    if request.agent != format!("graph-{actor}") {
        return Err(FacadeError::ConfinementRefused { resource: None });
    }
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
    // preserves nodes marked detached from graph cancellation.
    Ok(checkpoint.cancelled
        && checkpoint.nodes.iter().all(|node| {
            node.detached_from_graph_cancel
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
    if fields.peer_graph_collected == Some(true) {
        return Ok(());
    }
    let Some(graph) = fields
        .peer_graph
        .as_ref()
        .map(|graph| graph.graph_id.as_str())
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

/// One host's contribution to a `--host all` fan-out.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HostReading {
    pub host: String,
    pub status: provenant_records::Outcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The `data` envelope of an aggregated command: one section per host.
#[derive(Debug, serde::Serialize)]
pub struct HostAggregate {
    pub hosts: Vec<HostReading>,
    /// An unreachable peer accompanied at least one successful host.
    pub partial: bool,
}

/// One host's request outcome before it is folded into the envelope.
#[derive(Debug)]
pub enum HostResult {
    Ok(serde_json::Value),
    Unreachable,
    Failed(String),
}

/// Classify a transport failure. Only a refusal or a malformed frame is a
/// host error; every "could not reach the peer" cause is unreachability.
pub fn host_result_from_peer(result: Result<serde_json::Value, PeerError>) -> HostResult {
    match result {
        Ok(value) => HostResult::Ok(value),
        Err(error) => match error {
            PeerError::Unavailable
            | PeerError::Failed(_)
            | PeerError::Deadline
            | PeerError::Version
            | PeerError::ProtocolMismatch { .. }
            | PeerError::SourceHost => HostResult::Unreachable,
            PeerError::Refused { error } => HostResult::Failed(error.to_string()),
            other => HostResult::Failed(other.to_string()),
        },
    }
}

/// Compose one result per host into a single envelope. An unreachable peer
/// adds `PEER_UNREACHABLE` and `partial: true` only when another host answered;
/// an all-down fan-out stays a plain failure. The process status is the worst
/// of the reachable readings, so a failing host still fails the command.
pub fn aggregate_host_readings(
    command: &str,
    readings: Vec<(String, HostResult)>,
) -> provenant_records::CommandResult<HostAggregate> {
    let mut hosts = Vec::with_capacity(readings.len());
    let mut unreachable = Vec::new();
    let mut succeeded = false;
    for (host, reading) in readings {
        match reading {
            HostResult::Ok(data) => {
                succeeded = true;
                hosts.push(HostReading {
                    host,
                    status: provenant_records::Outcome::Ok,
                    data: Some(data),
                    error: None,
                });
            }
            HostResult::Unreachable => {
                unreachable.push(host.clone());
                hosts.push(HostReading {
                    host,
                    status: provenant_records::Outcome::Unavailable,
                    data: None,
                    error: Some(PEER_UNREACHABLE.as_str().to_owned()),
                });
            }
            HostResult::Failed(reason) => hosts.push(HostReading {
                host,
                status: provenant_records::Outcome::Error,
                data: None,
                error: Some(reason),
            }),
        }
    }
    let partial = succeeded && !unreachable.is_empty();
    let status = if partial {
        hosts
            .iter()
            .filter(|reading| reading.status != provenant_records::Outcome::Unavailable)
            .map(|reading| reading.status)
            .max_by_key(|status| status.exit_code())
            .unwrap_or(provenant_records::Outcome::Unavailable)
    } else {
        hosts
            .iter()
            .map(|reading| reading.status)
            .max_by_key(|status| status.exit_code())
            .unwrap_or(provenant_records::Outcome::Ok)
    };
    let mut result =
        provenant_records::CommandResult::new(command, status, HostAggregate { hosts, partial });
    if partial {
        result
            .warnings
            .push(provenant_records::warning::Warning::new(
                PEER_UNREACHABLE,
                format!(
                    "{} did not answer; showing the hosts that did",
                    unreachable.join(", ")
                ),
                provenant_records::warning::detected_source(PEER_UNREACHABLE),
            ));
    }
    result
}

/// One human section per host. Compact output keeps the per-host status line
/// and its body; `--full` is handled by the renderer around this envelope.
pub fn host_sections(aggregate: &HostAggregate) -> String {
    aggregate
        .hosts
        .iter()
        .map(|reading| {
            let body = match (&reading.data, &reading.error) {
                (Some(data), _) => provenant_cli::terse(data).trim_end().to_owned(),
                (None, Some(error)) => error.clone(),
                (None, None) => String::new(),
            };
            format!("{}: {:?} {}", reading.host, reading.status, body)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Emit a fan-out through the one envelope path, so JSON and human output
/// share the host sections, warning and exit code.
pub fn emit_host_aggregate(
    command: &str,
    readings: Vec<(String, HostResult)>,
    globals: &provenant_cli::Globals,
) -> u8 {
    let result = aggregate_host_readings(command, readings);
    let human = host_sections(&result.data);
    crate::emit(&result, globals, &human, true)
}

#[cfg(test)]
include!("peer_tests.rs");
