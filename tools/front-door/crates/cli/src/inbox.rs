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

#[derive(Debug, Args)]
pub struct Ask {
    /// JSON request content; ownership comes from the current Fabric identity
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
    pub id: HumanRequestId,
    #[arg(long, value_enum)]
    pub action: Action,
    #[arg(long)]
    pub option: Option<String>,
    /// Read the exact answer text from standard input (at most 4 KiB)
    #[arg(long)]
    pub text_stdin: bool,
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
    pub id: Option<HumanRequestId>,
    #[arg(long, value_enum, default_value = "open")]
    pub state: Filter,
    #[arg(long)]
    pub session: Option<String>,
    /// This slice supports the local owner only; remote targets are refused
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
        id: HumanRequestId,
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
        id: HumanRequestId,
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
                id: args.id,
                action: match args.action {
                    Action::Choose => AnswerAction::Choose,
                    Action::Reply => AnswerAction::Reply,
                    Action::Confirm => AnswerAction::Confirm,
                    Action::Override => AnswerAction::Override,
                },
                option_id: args.option,
                text: if args.text_stdin {
                    Some(read_text()?)
                } else {
                    None
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
                    state,
                    reason,
                    expect_revision,
                    operation_id,
                } => InboxInput::Close {
                    id,
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
                    answer,
                    session,
                    operation_id,
                } => InboxInput::Acknowledge {
                    id,
                    answer_id: answer,
                    session,
                    operation_id: Some(operation_id),
                },
            },
            Self::Inbox(args) => InboxInput::List {
                id: args.id,
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
}
impl Error {
    fn outcome_code(&self) -> (Outcome, &'static str) {
        match self {
            Self::Inbox(error) => (
                match error.code() {
                    "invalid_input" | "not_found" => Outcome::Invalid,
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
    )?;
    String::from_utf8(bytes)
        .map_err(|_| Error::Input("answer text must be UTF-8 and at most 4 KiB"))
}
fn execute(
    command: HumanCommand,
    roots: provenant_core::Roots,
    home: &Path,
    cwd: &Path,
) -> Result<InboxResult, Error> {
    // Refuse remote selection before opening any local store.
    let host = match &command {
        HumanCommand::Inbox(args) => args.host.clone(),
        _ => None,
    };
    let input = command.input()?;
    if input.is_mutation() && crate::fabric::in_lane() {
        return Err(InboxError::LaneMutation.into());
    }
    input.validate()?;
    let context = Context::open(roots, home, cwd)?;
    if host.is_some_and(|host| host != context.identity.host.to_string()) {
        return Err(InboxError::UnsupportedOwner.into());
    }
    let clock = provenant_os::clock::SystemClock;
    let budget = WaitBudget::cli(clock.mono_now(), 5)?;
    Ok(provenant_fabric_runtime::inbox::execute(
        &context.roots.state,
        &context.identity,
        &context.config,
        context.in_lane,
        &clock,
        input,
        budget,
        &provenant_core::CancelToken::new(),
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
                    Error::Inbox(InboxError::Mutation { operation_id, .. }) => Some(format!(
                        "Retry the same request with --operation-id {operation_id}; preserve its original content and revision."
                    )),
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
