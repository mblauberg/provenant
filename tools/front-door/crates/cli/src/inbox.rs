//! Human inbox grammar and thin CLI adapter over Fabric's service.
use crate::{Globals, fabric::Context};
use clap::{Args, Subcommand, ValueEnum};
use provenant_core::{Clock, HumanAnswerId, HumanRequestId, OperationId, WaitBudget};
use provenant_fabric_runtime::inbox::{InboxError, InboxInput, InboxResult};
use provenant_records::{CommandError, CommandResult, Outcome, inbox::*};
use std::{
    io::{self, Read},
    path::{Path, PathBuf},
};

/// A request ID may be qualified with the owning host ID, separated by ':'.
#[derive(Debug, Clone)]
pub struct RequestRef {
    host: Option<provenant_core::HostId>,
    id: HumanRequestId,
}
impl std::str::FromStr for RequestRef {
    type Err = &'static str;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (host, id) = match value.split_once(':') {
            Some((host, id)) => (
                Some(provenant_core::HostId::parse(host).map_err(|_| "invalid owner host ID")?),
                id,
            ),
            None => (None, value),
        };
        Ok(Self {
            host,
            id: HumanRequestId::parse(id).map_err(|_| "invalid request ID")?,
        })
    }
}

/// Resolve a delivery target from the workflow owner's registered chair.
pub fn resolve_delivery_target(
    root: &provenant_core::StateRoot,
    identity: &provenant_fabric_runtime::identity::Identity,
    session: &str,
) -> Result<AnswerTarget, InboxError> {
    use provenant_workflow::session::{HostSessionId, SessionError, SessionStore};
    let id = HostSessionId::parse(session).map_err(|_| InboxError::Invalid("session"))?;
    let registered = SessionStore::new(root.clone(), identity.project, id)
        .with_chair(identity.host, identity.inbox.actor)
        .discovery()
        .map_err(|error| match error {
            SessionError::DiscoveryIdentity => InboxError::Conflict,
            _ => InboxError::OwnerUnavailable,
        })?
        .ok_or(InboxError::Conflict)?;
    Ok(AnswerTarget {
        extra: Default::default(),
        host: registered.record.host,
        project: registered.record.project_id,
        session: registered.record.host_session_id.as_str().into(),
        actor: registered.record.chair_actor,
    })
}

/// Resolve where a control request goes. A lane's adapter comes from its run
/// record alone; a chair is the exactly registered session of this host and
/// project. Callers cannot name the recipient.
pub fn resolve_control(
    edge: &crate::run::Edge<'_>,
    identity: &provenant_fabric_runtime::identity::Identity,
    input: &InboxInput,
) -> Result<provenant_fabric_runtime::inbox::ControlRoute, InboxError> {
    use provenant_fabric_runtime::inbox::ControlRoute;
    use provenant_records::control::LaunchKind;
    use provenant_workflow::session::{HostSessionId, SessionStore};
    match input {
        InboxInput::Steer { attempt, .. } => {
            let provider = crate::run::attempt_provider(edge, *attempt).map_err(|fail| {
                if fail.kind == provenant_run::ErrorKind::NotFound {
                    InboxError::Store(provenant_fabric_runtime::StoreError::NotFound)
                } else {
                    InboxError::WorkLookup(fail.message)
                }
            })?;
            Ok(ControlRoute {
                provider: provider
                    .and_then(|name| provenant_records::AdapterSpec::get(&name))
                    .map(|spec| spec.id),
                launch: LaunchKind::Lane,
                registered: false,
                target: attempt.to_string(),
            })
        }
        InboxInput::ChairMessage { session, .. } => {
            let id = HostSessionId::parse(session).map_err(|_| InboxError::Invalid("session"))?;
            let found = SessionStore::new(edge.roots.state.clone(), identity.project, id)
                .discovery()
                .map_err(|_| InboxError::OwnerUnavailable)?
                .ok_or(InboxError::Store(
                    provenant_fabric_runtime::StoreError::NotFound,
                ))?;
            let record = found.record;
            if record.host != identity.host || record.project_id != identity.project {
                return Err(InboxError::Conflict);
            }
            Ok(ControlRoute {
                provider: None,
                launch: LaunchKind::Chair,
                registered: true,
                target: format!("{}/{}/{}", record.host, record.project_id, session),
            })
        }
        _ => Err(InboxError::Invalid("control target")),
    }
}

/// Compose Run's host lookup with the inbox owner without a runtime dependency.
pub fn resolve_work(
    root: &provenant_core::StateRoot,
    project: provenant_core::ProjectId,
    work: &RequestWork,
    clock: &dyn Clock,
    cancel: &provenant_core::CancelToken,
) -> Result<WorkCoverage, InboxError> {
    provenant_run::validate_work(
        root,
        &provenant_run::WorkSelector {
            project,
            run: work.run_id,
            task: work.task_id,
            attempt: work.attempt_id,
            repository: work.repository.clone(),
            head: work.head,
        },
        &provenant_run::Request { clock, cancel },
    )
    .map_err(|error| match error {
        provenant_run::RunError::Contention => {
            InboxError::Store(provenant_fabric_runtime::StoreError::Busy)
        }
        provenant_run::RunError::Cancelled => {
            InboxError::Store(provenant_fabric_runtime::StoreError::Cancelled)
        }
        other => InboxError::WorkLookup(other.to_string()),
    })
}

#[derive(Debug, Args)]
pub struct Ask {
    /// JSON request content, including work links, replacement and deadline; ownership comes from Fabric
    #[arg(long)]
    pub file: PathBuf,
    /// Explicit originating chair session; never inferred from the agent label
    #[arg(long)]
    pub session: String,
    #[arg(long)]
    pub operation_id: Option<OperationId>,
}
#[derive(Debug, Args)]
pub struct Answer {
    #[arg(long)]
    pub all_projects: bool,
    pub id: RequestRef,
    /// Owner host label or host ID
    #[arg(long)]
    pub host: Option<String>,
    #[arg(long, value_enum)]
    pub action: Action,
    #[arg(long)]
    pub option: Option<String>,
    /// Read piped answer text (at most 4 KiB; input must finish within 5 seconds)
    #[arg(long)]
    pub text_stdin: bool,
    /// Read a UTF-8 reply composed in a file (at most 4 KiB)
    #[arg(long, conflicts_with = "text_stdin")]
    pub text_file: Option<PathBuf>,
    #[arg(long, value_parser=clap::value_parser!(u64).range(1..))]
    pub expect_revision: u64,
    #[arg(long, value_enum)]
    pub channel: Channel,
    #[arg(long)]
    pub operation_id: Option<OperationId>,
}
#[derive(Debug, Args)]
pub struct Steer {
    /// Attempt ID
    pub attempt: provenant_core::AttemptId,
    /// Read the message from standard input (at most 4 KiB; input must finish within 5 seconds)
    #[arg(long, required = true)]
    pub text_stdin: bool,
    #[arg(long)]
    pub operation_id: Option<OperationId>,
}
#[derive(Debug, Args)]
pub struct ChairMessage {
    /// Registered chair session ID
    pub session: String,
    /// Read the message from standard input (at most 4 KiB; input must finish within 5 seconds)
    #[arg(long, required = true)]
    pub text_stdin: bool,
    #[arg(long, value_enum)]
    pub channel: Channel,
    #[arg(long)]
    pub operation_id: Option<OperationId>,
}
#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct Inbox {
    #[arg(long)]
    pub all_projects: bool,
    #[command(subcommand)]
    pub command: Option<InboxCommand>,
    pub id: Option<RequestRef>,
    #[arg(long, value_enum, default_value = "open")]
    pub state: Filter,
    #[arg(long)]
    pub session: Option<String>,
    /// Owner host label or host ID
    #[arg(long)]
    pub host: Option<String>,
    #[arg(long,default_value="50",value_parser=clap::value_parser!(u64).range(1..=100))]
    pub limit: u64,
    #[arg(long)]
    pub cursor: Option<String>,
}
#[derive(Debug, Subcommand)]
pub enum InboxCommand {
    /// Close without treating silence as approval
    Close {
        #[arg(long)]
        all_projects: bool,
        id: RequestRef,
        /// Owner host label or host ID
        #[arg(long)]
        host: Option<String>,
        #[arg(long, value_enum)]
        state: Closure,
        #[arg(long)]
        reason: String,
        #[arg(long,value_parser=clap::value_parser!(u64).range(1..))]
        expect_revision: u64,
        #[arg(long)]
        operation_id: Option<OperationId>,
    },
    /// Record consumption by the exact owning chair session and actor
    Acknowledge {
        id: RequestRef,
        /// Owner host label or host ID
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        answer: HumanAnswerId,
        #[arg(long)]
        session: String,
        #[arg(long)]
        operation_id: OperationId,
    },
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Action {
    Choose,
    Reply,
    Confirm,
    Override,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Channel {
    Chat,
    Console,
    #[value(name = "phone_remote")]
    PhoneRemote,
    Cli,
    Unknown,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Filter {
    Open,
    All,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Closure {
    Withdrawn,
    Superseded,
    Expired,
}

fn interaction_channel(channel: Channel) -> InteractionChannel {
    match channel {
        Channel::Chat => InteractionChannel::Chat,
        Channel::Console => InteractionChannel::Console,
        Channel::PhoneRemote => InteractionChannel::PhoneRemote,
        Channel::Cli => InteractionChannel::Cli,
        Channel::Unknown => InteractionChannel::Unknown,
    }
}

pub enum HumanCommand {
    Ask(Ask),
    Answer(Answer),
    Inbox(Inbox),
    Steer(Steer),
    ChairMessage(ChairMessage),
}
impl HumanCommand {
    fn input(self) -> Result<InboxInput, Error> {
        Ok(match self {
            Self::Ask(args) => InboxInput::Ask {
                extra: Default::default(),
                session: args.session,
                request: Box::new(request_content(read_file(&args.file)?.as_slice())?),
                operation_id: args.operation_id,
            },
            Self::Answer(args) => InboxInput::Answer {
                extra: Default::default(),
                id: args.id.id,
                action: match args.action {
                    Action::Choose => AnswerAction::Choose,
                    Action::Reply => AnswerAction::Reply,
                    Action::Confirm => AnswerAction::Confirm,
                    Action::Override => AnswerAction::Override,
                },
                option_id: args.option,
                text: match (args.text_stdin, args.text_file) {
                    (true, None) => Some(read_text()?),
                    (false, Some(path)) => Some(read_text_file(&path)?),
                    (false, None) => None,
                    (true, Some(_)) => {
                        return Err(Error::Input("choose only one answer text source"));
                    }
                },
                expect_revision: args.expect_revision,
                channel: interaction_channel(args.channel),
                operation_id: args.operation_id,
            },
            Self::Steer(args) => InboxInput::Steer {
                extra: Default::default(),
                attempt: args.attempt,
                text: read_text()?,
                operation_id: args.operation_id,
            },
            Self::ChairMessage(args) => InboxInput::ChairMessage {
                extra: Default::default(),
                session: args.session,
                text: read_text()?,
                channel: interaction_channel(args.channel),
                operation_id: args.operation_id,
            },
            Self::Inbox(Inbox {
                command: Some(command),
                ..
            }) => match command {
                InboxCommand::Close {
                    all_projects: _,
                    id,
                    host: _,
                    state,
                    reason,
                    expect_revision,
                    operation_id,
                } => InboxInput::Close {
                    extra: Default::default(),
                    id: id.id,
                    state: match state {
                        Closure::Withdrawn => RequestState::Withdrawn,
                        Closure::Superseded => RequestState::Superseded,
                        Closure::Expired => RequestState::Expired,
                    },
                    reason,
                    expect_revision,
                    operation_id,
                },
                InboxCommand::Acknowledge {
                    id,
                    host: _,
                    answer,
                    session,
                    operation_id,
                } => InboxInput::Acknowledge {
                    extra: Default::default(),
                    id: id.id,
                    answer_id: answer,
                    session,
                    operation_id: Some(operation_id),
                },
            },
            Self::Inbox(args) => InboxInput::List {
                extra: Default::default(),
                id: args.id.map(|reference| reference.id),
                session: args.session,
                state: match args.state {
                    Filter::Open => provenant_fabric_runtime::inbox::InboxFilter::Open,
                    Filter::All => provenant_fabric_runtime::inbox::InboxFilter::All,
                },
                limit: args.limit,
                cursor: args
                    .cursor
                    .map(TryInto::try_into)
                    .transpose()
                    .map_err(Error::Input)?,
            },
        })
    }
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)]
    Setup(#[from] crate::fabric::EdgeError),
    #[error(transparent)]
    Inbox(#[from] InboxError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Time(#[from] provenant_core::TimeError),
    #[error("{0}")]
    Input(&'static str),
    #[error("{0}")]
    NoProject(String),
    #[error("{message}")]
    Remote {
        code: String,
        message: String,
        operation_id: Option<OperationId>,
    },
}
impl Error {
    fn outcome_code(&self) -> (Outcome, &'static str) {
        match self {
            Self::Remote { code, .. } => (
                Outcome::Error,
                match code.as_str() {
                    "owner_local_only" => "owner_local_only",
                    "unsupported_owner" => "unsupported_owner",
                    "owner_unavailable" => "owner_unavailable",
                    "ambiguous_outcome" => "ambiguous_outcome",
                    "conflict" => "conflict",
                    "operation_conflict" => "operation_conflict",
                    "not_found" => "not_found",
                    "invalid_input" => "invalid_input",
                    "cursor_expired" => "cursor_expired",
                    "request_limit" => "request_limit",
                    "busy" => "busy",
                    "cancelled" => "cancelled",
                    "work_mismatch" => "work_mismatch",
                    _ => "inbox_unavailable",
                },
            ),
            Self::Inbox(error) => (
                match error.code() {
                    "invalid_input" | "not_found" | "work_mismatch" => Outcome::Invalid,
                    "conflict"
                    | "operation_conflict"
                    | "cursor_expired"
                    | "lane_coordination_refused"
                    | "unsupported_owner"
                    | "request_limit" => Outcome::Refused,
                    "cancelled" => Outcome::Cancelled,
                    _ => Outcome::Error,
                },
                error.code(),
            ),
            Self::Input(_) => (Outcome::Invalid, "invalid_input"),
            Self::NoProject(_) => (Outcome::Error, "no_project"),
            _ => (Outcome::Error, "inbox_unavailable"),
        }
    }
}
fn read_file(path: &Path) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(32769)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 32768 {
        return Err(Error::Input("request exceeds 32 KiB"));
    }
    Ok(bytes)
}

/// Authored files are strict even though stored and peer request records retain
/// newer fields. Ownership is supplied by Fabric rather than request content.
fn request_content(bytes: &[u8]) -> Result<RequestDraft, Error> {
    let invalid = || Error::Input("invalid request JSON; provide request content only");
    let value: serde_json::Value = provenant_records::decode(
        bytes,
        provenant_records::Limits {
            max_bytes: 32768,
            max_depth: 16,
            max_members: 1024,
        },
    )
    .map_err(|_| invalid())?;
    let known = |value: &serde_json::Value, fields: &[&str]| {
        value
            .as_object()
            .is_none_or(|object| object.keys().all(|key| fields.contains(&key.as_str())))
    };
    if !known(
        &value,
        &[
            "supersedes",
            "deadline",
            "work",
            "kind",
            "title",
            "body",
            "blocking",
            "options",
            "recommended_option_id",
            "proceeding_with",
        ],
    ) || !known(&value["supersedes"], &["id", "expect_revision"])
        || !known(
            &value["work"],
            &["run_id", "task_id", "attempt_id", "repository", "head"],
        )
        || !known(&value["work"]["repository"], &["relative_to_root"])
        || !known(
            &value["work"]["repository"]["relative_to_root"],
            &["encoding", "path"],
        )
        || value["options"].as_array().is_some_and(|options| {
            options
                .iter()
                .any(|option| !known(option, &["id", "label", "description"]))
        })
    {
        return Err(invalid());
    }
    serde_json::from_value(value).map_err(|_| invalid())
}

#[allow(clippy::disallowed_methods)]
fn read_text() -> Result<String, Error> {
    let bytes = provenant_os::sys::hook_io::read_stdin(
        ANSWER_TEXT_BYTES,
        std::time::Instant::now() + std::time::Duration::from_secs(5),
    )
    .map_err(|error| match error.kind() {
        io::ErrorKind::TimedOut => Error::Input(
            "piped answer input timed out after 5 seconds; compose a reply with --text-file",
        ),
        io::ErrorKind::FileTooLarge => Error::Input("piped answer exceeds 4 KiB"),
        _ => Error::Io(error),
    })?;
    String::from_utf8(bytes)
        .map_err(|_| Error::Input("answer text must be UTF-8 and at most 4 KiB"))
}
fn read_text_file(path: &Path) -> Result<String, Error> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take((ANSWER_TEXT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > ANSWER_TEXT_BYTES {
        return Err(Error::Input("answer file exceeds 4 KiB"));
    }
    String::from_utf8(bytes).map_err(|_| Error::Input("answer file must be UTF-8"))
}
fn is_local_host(name: &str, own: Option<provenant_core::HostId>) -> bool {
    name == "local" || own.is_some_and(|id| name == id.to_string())
}

fn remote_error(code: String, message: String, operation_id: Option<OperationId>) -> Error {
    let operation_id =
        operation_id.filter(|_| matches!(code.as_str(), "ambiguous_outcome" | "owner_unavailable"));
    let message = match &operation_id {
        Some(id) => format!("inbox operation {id}: {message}"),
        None => message,
    };
    Error::Remote {
        code,
        message,
        operation_id,
    }
}

/// Operations that make sense without a working project: listing, and acting on
/// one request by ID.
fn host_wide_input(input: &InboxInput) -> bool {
    matches!(
        input,
        InboxInput::List { .. } | InboxInput::Answer { .. } | InboxInput::Close { .. }
    )
}

enum HostWide {
    Done(InboxResult),
    /// The owning project's context, with the input to run in it.
    Project(Box<Context>, InboxInput),
}

fn no_project(cwd: &Path, cause: impl std::fmt::Display) -> Error {
    Error::NoProject(format!(
        "No project could be resolved from {}: {cause}",
        cwd.display()
    ))
}

fn project_label(common_dir: &Path) -> Option<String> {
    common_dir
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .map(str::to_owned)
}

/// Serve an inbox call whose working directory is in no project. A list reads
/// the open requests of every project on this host, each tagged with its
/// project in `projects`; an action by request ID runs in the owning project.
fn host_wide(
    input: InboxInput,
    roots: provenant_core::Roots,
    home: &Path,
    cwd: &Path,
) -> Result<HostWide, Error> {
    use provenant_fabric_runtime::inbox::{host_open_requests, locate_request};
    let clock = provenant_os::clock::SystemClock;
    let cancel = provenant_core::CancelToken::new();
    let bindings =
        provenant_config::registered_projects(&roots).map_err(crate::fabric::EdgeError::from)?;
    match input {
        InboxInput::List {
            id: None,
            session,
            state,
            limit,
            cursor,
            ..
        } => {
            let mut project_ids: Vec<_> = bindings.iter().map(|b| b.project_id()).collect();
            project_ids.sort();
            project_ids.dedup();
            let (items, next_cursor) = host_open_requests(
                &roots.state,
                &project_ids,
                session.as_deref(),
                state,
                limit,
                cursor,
                &provenant_fabric_store::Request {
                    clock: &clock,
                    cancel: &cancel,
                },
            )?;
            let truncated = next_cursor.is_some();
            let projects: serde_json::Map<_, _> = items
                .iter()
                .map(|item| item.project_id)
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .map(|project| {
                    let binding = bindings.iter().find(|b| b.project_id() == project);
                    (
                        project.to_string(),
                        serde_json::json!({
                            "label": binding.and_then(|b| project_label(b.common_dir())),
                            "path": binding.map(|b| b.common_dir().display().to_string()),
                        }),
                    )
                })
                .collect();
            let mut extra = provenant_records::ExtraFields::default();
            extra.insert("projects".into(), projects.into());
            Ok(HostWide::Done(InboxResult::Listed {
                extra,
                items,
                next_cursor,
                truncated,
                coverage: "local_only; registered projects on this host; peer inboxes are unknown"
                    .into(),
            }))
        }
        input => {
            let id = match &input {
                InboxInput::List { id: Some(id), .. }
                | InboxInput::Answer { id, .. }
                | InboxInput::Close { id, .. } => *id,
                _ => {
                    return Err(Error::Input(
                        "--all-projects is not supported for this operation",
                    ));
                }
            };
            let mut projects: Vec<_> = bindings.iter().map(|b| b.project_id()).collect();
            projects.sort();
            projects.dedup();
            let owner = locate_request(&roots.state, &projects, id, &clock, &cancel)?.ok_or(
                InboxError::Store(provenant_fabric_runtime::StoreError::NotFound),
            )?;
            let binding = bindings
                .iter()
                .find(|b| b.project_id() == owner)
                .ok_or_else(|| no_project(cwd, "owning project is not registered"))?;
            let context = Context::open_for_project(roots, home, binding)?;
            Ok(HostWide::Project(Box::new(context), input))
        }
    }
}

fn execute(
    command: HumanCommand,
    roots: provenant_core::Roots,
    home: &Path,
    cwd: &Path,
) -> Result<InboxResult, Error> {
    let all_projects = match &command {
        HumanCommand::Answer(args) => args.all_projects,
        HumanCommand::Inbox(args) => match &args.command {
            Some(InboxCommand::Close { all_projects, .. }) => *all_projects,
            None => args.all_projects,
            _ => false,
        },
        _ => false,
    };
    let host = match &command {
        HumanCommand::Answer(args) => args.host.clone(),
        HumanCommand::Inbox(args) => match &args.command {
            Some(InboxCommand::Close { host, .. } | InboxCommand::Acknowledge { host, .. }) => {
                host.clone()
            }
            None => args.host.clone(),
        },
        _ => None,
    };
    let qualified = match &command {
        HumanCommand::Answer(args) => args.id.host,
        HumanCommand::Inbox(args) => match &args.command {
            Some(InboxCommand::Close { id, .. } | InboxCommand::Acknowledge { id, .. }) => id.host,
            None => args.id.as_ref().and_then(|id| id.host),
        },
        _ => None,
    };
    let mut input = command.input()?;
    if input.is_mutation() && crate::fabric::in_lane() {
        return Err(InboxError::LaneMutation.into());
    }
    input.validate()?;
    // Without host state nothing is remote; opening the context reports it.
    let own = match provenant_config::lookup_host(&roots) {
        Ok(own) => own,
        Err(_) if host.is_none() && qualified.is_none() && host_wide_input(&input) => None,
        Err(error) => return Err(crate::fabric::EdgeError::from(error).into()),
    };
    let selected = host.clone().or_else(|| qualified.map(|id| id.to_string()));
    let remote_owner = selected
        .as_deref()
        .is_some_and(|name| !is_local_host(name, own));
    let context = if all_projects {
        if remote_owner {
            return Err(Error::Input(
                "--all-projects is only available for the local host",
            ));
        }
        match host_wide(input, roots, home, cwd)? {
            HostWide::Done(result) => return Ok(result),
            HostWide::Project(context, next) => {
                input = next;
                *context
            }
        }
    } else {
        let opened = if remote_owner {
            Context::open_read_only(roots.clone(), home, cwd)
        } else {
            Context::open(roots.clone(), home, cwd)
        };
        match opened {
            Ok(context) => context,
            Err(crate::fabric::EdgeError::Utility(
                provenant_confinement::utility::UtilityError::InvalidOperationValue,
            )) if cwd == Path::new("/") => {
                return Err(no_project(
                    cwd,
                    "the filesystem root cannot identify a project",
                ));
            }
            Err(error) => return Err(error.into()),
        }
    };
    if let (Some(name), Some(qualified)) = (&host, qualified) {
        let selected = if is_local_host(name, Some(context.identity.host)) {
            Some(context.identity.host)
        } else {
            context.peer_host(name)
        };
        if selected != Some(qualified) {
            return Err(Error::Input("request owner does not match --host"));
        }
    }
    let host = host.or_else(|| qualified.map(|host| host.to_string()));
    let clock = provenant_os::clock::SystemClock;
    let budget = WaitBudget::cli(clock.mono_now(), 5)?;
    if let Some(host) = host.filter(|host| !is_local_host(host, Some(context.identity.host))) {
        if matches!(
            input,
            InboxInput::Close { .. } | InboxInput::Acknowledge { .. }
        ) {
            return Err(Error::Remote {
                code: "owner_local_only".into(),
                message: format!(
                    "inbox owner {host}: run this operation on the owner host or from the owning chair session"
                ),
                operation_id: None,
            });
        }
        input.freeze_operation()?;
        let operation_id = input.operation_id().cloned();
        let target = context.peer_host(&host).ok_or_else(|| {
            remote_error(
                "owner_unavailable".into(),
                format!("inbox owner {host} is not configured"),
                operation_id.clone(),
            )
        })?;
        let reply = context.peer(
            &clock,
            target,
            provenant_fabric_runtime::hosts::PeerAction::Inbox {
                extra: Default::default(),
                input,
            },
            budget,
            &provenant_core::CancelToken::new(),
        );
        return match reply {
            Ok(provenant_fabric_runtime::hosts::PeerResult::Inbox {
                data: provenant_fabric_runtime::inbox::InboxReply::Ok { result, .. },
                ..
            }) => Ok(*result),
            Ok(provenant_fabric_runtime::hosts::PeerResult::Inbox {
                data: provenant_fabric_runtime::inbox::InboxReply::Error { code, message, .. },
                ..
            }) => Err(remote_error(code, message, operation_id.clone())),
            Ok(_) => Err(Error::Remote {
                code: "unsupported_owner".into(),
                message: "peer does not support inbox operations".into(),
                operation_id: None,
            }),
            Err(failure) => {
                use provenant_fabric_runtime::coordination::facade::FacadeError;
                let code = match failure.error {
                    FacadeError::LaunchUnknown { .. } => "ambiguous_outcome",
                    FacadeError::PeerVersion | FacadeError::PeerProtocolMismatch { .. } => {
                        "unsupported_owner"
                    }
                    _ => "owner_unavailable",
                };
                Err(remote_error(
                    code.into(),
                    failure.detail.unwrap_or_else(|| failure.error.to_string()),
                    operation_id,
                ))
            }
        };
    }
    Ok(provenant_fabric_runtime::inbox::execute(
        &context.roots.state,
        &context.identity,
        &context.config,
        context.in_lane,
        &clock,
        input,
        &provenant_core::CancelToken::new(),
        &|work, call| {
            resolve_work(
                &context.roots.state,
                context.identity.project,
                work,
                call.clock,
                call.cancel,
            )
        },
        // Assets only redirect lane state, which a mutation never reaches.
        &|input, _| {
            resolve_control(
                &crate::run::Edge {
                    roots: &context.roots,
                    home,
                    assets: home,
                    cwd,
                },
                &context.identity,
                input,
            )
        },
    )?)
}
pub fn run(
    command: HumanCommand,
    path: &str,
    globals: &Globals,
    roots: &provenant_core::Roots,
    home: &Path,
    cwd: &Path,
) -> u8 {
    let result = match execute(command, roots.clone(), home, cwd) {
        Ok(InboxResult::Control { receipt })
            if receipt.state == provenant_records::control::ControlState::Unsupported =>
        {
            // Refused before any side effect; the receipt still names the reason.
            let message = receipt.refusal_message();
            let mut result = CommandResult::new(
                path,
                Outcome::Refused,
                Some(InboxResult::Control { receipt }),
            );
            result.error = Some(CommandError {
                code: provenant_records::control::CONTROL_UNSUPPORTED.into(),
                message,
                cause: None,
                repair: Some(
                    "Read `provenant help --all --json` provider_controls for what each provider supports; use resume to continue a lane."
                        .into(),
                ),
            });
            result
        }
        Ok(data) => CommandResult::new(path, Outcome::Ok, Some(data)),
        Err(error) => {
            let (status, code) = error.outcome_code();
            let mut result = CommandResult::new(path, status, None);
            result.error = Some(CommandError {
                code: code.into(),
                message: error.to_string(),
                cause: match &error { Error::NoProject(cause) => Some(cause.clone()), _ => None },
                repair: match &error {
                    Error::Remote { operation_id: Some(operation_id), .. } => Some(format!(
                        "Retry the same request with --operation-id {operation_id}; preserve its original content and revision."
                    )),
                    Error::Inbox(InboxError::Mutation { operation_id, source }) if matches!(source.code(),"busy"|"cancelled"|"inbox_unavailable") => Some(format!(
                        "Retry the same request with --operation-id {operation_id}; preserve its original content and revision."
                    )),
                    Error::Inbox(error) if matches!(error.code(),"conflict"|"operation_conflict")=>Some("Inspect `provenant inbox --state all --json` before submitting another operation; preserve the authoritative answer and its revision.".into()),
                    Error::Inbox(error) if error.code() == "work_mismatch" => Some("Correct the work repository or head to match the selected run before retrying.".into()),
                    Error::NoProject(_) => Some("Run from a project directory, or use --all-projects to access registered projects on this host.".into()),
                    Error::Inbox(InboxError::CursorExpired) => {
                        Some("Restart the list without --cursor.".into())
                    }
                    _ => None,
                },
            });
            result
        }
    };
    let human = match result
        .data
        .as_ref()
        .map(serde_json::to_string_pretty)
        .transpose()
    {
        Ok(text) => text.unwrap_or_default(),
        Err(_) => return 3,
    };
    let mode = globals.render_mode(
        &human,
        matches!(
            result.data,
            Some(InboxResult::Item { .. } | InboxResult::Listed { .. })
        ),
        None,
    );
    match crate::render_or_publish(
        &result,
        mode,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
        |value| crate::fabric::publish(value, roots),
    ) {
        Ok(status) => crate::exit_status(&status),
        Err(_) => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authored_requests_reject_unknown_fields_at_every_depth() {
        let value = serde_json::json!({
            "kind": "decision", "title": "Repair", "body": "Choose a repair",
            "supersedes": {"id": "01".repeat(16), "expect_revision": 1},
            "work": {
                "run_id": "02".repeat(16),
                "repository": {"relative_to_root": {"encoding": "utf8", "path": "repo"}}
            },
            "options": [{"id": "repair", "label": "Repair", "description": "Fix the check"}]
        });
        assert!(request_content(&serde_json::to_vec(&value).unwrap()).is_ok());
        for path in [
            "",
            "/supersedes",
            "/work",
            "/work/repository",
            "/work/repository/relative_to_root",
            "/options/0",
        ] {
            let mut forged = value.clone();
            forged
                .pointer_mut(path)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("owner_host".into(), serde_json::json!("03".repeat(16)));
            assert!(
                request_content(&serde_json::to_vec(&forged).unwrap()).is_err(),
                "{path}"
            );
        }
    }

    #[test]
    fn remote_uncertain_answer_retains_operation_id() {
        let operation = OperationId::parse("retry-answer").expect("valid operation");
        for code in ["ambiguous_outcome", "owner_unavailable"] {
            let error = remote_error(code.into(), "peer failed".into(), Some(operation.clone()));
            assert!(error.to_string().contains("retry-answer"));
            assert!(
                matches!(error, Error::Remote { operation_id: Some(id), .. } if id == operation)
            );
        }
    }
}
