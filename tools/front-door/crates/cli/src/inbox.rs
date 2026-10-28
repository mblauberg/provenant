//! Human inbox grammar and thin CLI adapter over Fabric's service.
use crate::{Globals, RenderMode, fabric::Context};
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
        host: registered.record.host,
        project: registered.record.project_id,
        session: registered.record.host_session_id.as_str().into(),
        actor: registered.record.chair_actor,
    })
}

/// Compose Run's host lookup with the inbox owner without a runtime dependency.
pub fn resolve_work(
    root: &provenant_core::StateRoot,
    project: provenant_core::ProjectId,
    work: &RequestWork,
    clock: &dyn Clock,
    budget: WaitBudget,
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
        &provenant_run::Request {
            clock,
            budget,
            cancel,
        },
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
#[command(args_conflicts_with_subcommands = true)]
pub struct Inbox {
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

pub enum HumanCommand {
    Ask(Ask),
    Answer(Answer),
    Inbox(Inbox),
}
impl HumanCommand {
    fn input(self) -> Result<InboxInput, Error> {
        Ok(match self {
            Self::Ask(args) => InboxInput::Ask {
                session: args.session,
                request: provenant_records::decode(
                    read_file(&args.file)?.as_slice(),
                    provenant_records::Limits {
                        max_bytes: 32768,
                        max_depth: 16,
                        max_members: 1024,
                    },
                )
                .map_err(|_| Error::Input("invalid request JSON; provide request content only"))?,
                operation_id: args.operation_id,
            },
            Self::Answer(args) => InboxInput::Answer {
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
                channel: match args.channel {
                    Channel::Chat => InteractionChannel::Chat,
                    Channel::Console => InteractionChannel::Console,
                    Channel::PhoneRemote => InteractionChannel::PhoneRemote,
                    Channel::Cli => InteractionChannel::Cli,
                    Channel::Unknown => InteractionChannel::Unknown,
                },
                operation_id: args.operation_id,
            },
            Self::Inbox(Inbox {
                command: Some(command),
                ..
            }) => match command {
                InboxCommand::Close {
                    id,
                    host: _,
                    state,
                    reason,
                    expect_revision,
                    operation_id,
                } => InboxInput::Close {
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
                    id: id.id,
                    answer_id: answer,
                    session,
                    operation_id: Some(operation_id),
                },
            },
            Self::Inbox(args) => InboxInput::List {
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

fn execute(
    command: HumanCommand,
    roots: provenant_core::Roots,
    home: &Path,
    cwd: &Path,
) -> Result<InboxResult, Error> {
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
    let own = provenant_config::lookup_host(&roots).map_err(crate::fabric::EdgeError::from)?;
    let selected = host.clone().or_else(|| qualified.map(|id| id.to_string()));
    let context = if selected
        .as_deref()
        .is_some_and(|name| !is_local_host(name, own))
    {
        Context::open_read_only(roots, home, cwd)?
    } else {
        Context::open(roots, home, cwd)?
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
            provenant_fabric_runtime::hosts::PeerAction::Inbox { input },
            budget,
            &provenant_core::CancelToken::new(),
        );
        return match reply {
            Ok(provenant_fabric_runtime::hosts::PeerResult::Inbox(
                provenant_fabric_runtime::inbox::InboxReply::Ok { result },
            )) => Ok(*result),
            Ok(provenant_fabric_runtime::hosts::PeerResult::Inbox(
                provenant_fabric_runtime::inbox::InboxReply::Error { code, message },
            )) => Err(remote_error(code, message, operation_id.clone())),
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
        budget,
        &provenant_core::CancelToken::new(),
        &|work, call| {
            resolve_work(
                &context.roots.state,
                context.identity.project,
                work,
                call.clock,
                call.budget,
                call.cancel,
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
        Ok(data) => CommandResult::new(path, Outcome::Ok, Some(data)),
        Err(error) => {
            let (status, code) = error.outcome_code();
            let mut result = CommandResult::new(path, status, None);
            result.error = Some(CommandError {
                code: code.into(),
                message: error.to_string(),
                cause: None,
                repair: match &error {
                    Error::Remote { operation_id: Some(operation_id), .. } => Some(format!(
                        "Retry the same request with --operation-id {operation_id}; preserve its original content and revision."
                    )),
                    Error::Inbox(InboxError::Mutation { operation_id, source }) if matches!(source.code(),"busy"|"cancelled"|"inbox_unavailable") => Some(format!(
                        "Retry the same request with --operation-id {operation_id}; preserve its original content and revision."
                    )),
                    Error::Inbox(error) if matches!(error.code(),"conflict"|"operation_conflict")=>Some("Inspect `provenant inbox --state all --json` before submitting another operation; preserve the authoritative answer and its revision.".into()),
                    Error::Inbox(error) if error.code() == "work_mismatch" => Some("Correct the work repository or head to match the selected run before retrying.".into()),
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
    let mode = if globals.json {
        RenderMode::Json
    } else {
        RenderMode::Human {
            text: &human,
            read_only: matches!(
                result.data,
                Some(InboxResult::Item { .. } | InboxResult::Listed { .. })
            ),
        }
    };
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
