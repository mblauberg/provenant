//! Human inbox grammar and thin CLI adapter over Fabric's service.
use crate::metadata::CatalogueArg;
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
    #[arg(long, catalog_u64_range = 1..)]
    pub expect_revision: u64,
    #[arg(long, value_enum)]
    pub channel: Channel,
    #[arg(long)]
    pub operation_id: Option<OperationId>,
    /// With --all-projects and a remote --host: the project key (as in a listed
    /// request's project_key) that selects the local project to route through
    #[arg(long)]
    pub project_key: Option<String>,
}
#[derive(Debug, Args)]
pub struct Steer {
    /// Attempt to steer, as NAME[/NODE[#N]]; a node steers its live attempt
    #[arg(value_name = "TARGET")]
    pub target: crate::Target,
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
    #[arg(long,default_value="50",catalog_u64_range = 1..=100)]
    pub limit: u64,
    #[arg(long)]
    pub cursor: Option<String>,
    /// With --all-projects (list only): also read these peers' first pages;
    /// `all` or comma-separated host labels
    #[arg(long)]
    pub hosts: Option<String>,
    /// With --all-projects and a remote --host: the project key (as in a listed
    /// request's project_key) that selects the local project to route through
    #[arg(long)]
    pub project_key: Option<String>,
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
        #[arg(long,catalog_u64_range = 1..)]
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
                    "OWNER_LOCAL_ONLY" => "OWNER_LOCAL_ONLY",
                    "UNSUPPORTED_OWNER" => "UNSUPPORTED_OWNER",
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
                    | "UNSUPPORTED_OWNER"
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

fn owner_local_only(host: &str) -> Error {
    Error::Remote {
        code: "OWNER_LOCAL_ONLY".into(),
        message: format!(
            "inbox owner {host}: run this operation on the owner host or from the owning chair session"
        ),
        operation_id: None,
    }
}

/// The peers a `--hosts` value names, as sorted `(label, host)` pairs, and the
/// peer deadline in seconds. `local` is skipped; an unknown label is refused.
fn chosen_peers(
    roots: &provenant_core::Roots,
    hosts: &str,
) -> Result<(Vec<(String, provenant_core::HostId)>, u64), Error> {
    let config = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)
        .map_err(crate::fabric::EdgeError::from)?
        .domains
        .fabric;
    let mut chosen: Vec<(String, provenant_core::HostId)> = Vec::new();
    for name in hosts.split(',') {
        let selected: Vec<_> = if name == "all" {
            config
                .peers
                .iter()
                .map(|(l, p)| (l.clone(), p.host))
                .collect()
        } else if name == "local" {
            continue;
        } else if let Some(peer) = config.peers.get(name) {
            vec![(name.to_owned(), peer.host)]
        } else if let Some((label, peer)) = provenant_core::HostId::parse(name)
            .ok()
            .and_then(|id| config.peers.iter().find(|(_, p)| p.host == id))
        {
            vec![(label.clone(), peer.host)]
        } else {
            return Err(Error::Input("unknown peer host"));
        };
        for entry in selected {
            if !chosen.iter().any(|(_, host)| *host == entry.1) {
                chosen.push(entry);
            }
        }
    }
    chosen.sort();
    Ok((chosen, config.limits.peer_seconds))
}

/// Why one attempt at a peer did not produce a page.
#[derive(Clone, Copy, PartialEq)]
enum Miss {
    /// The peer does not know that project, so another may be tried.
    ProjectUnknown,
    /// An invalid-input refusal: the page size exceeds the peer's `page_rows`,
    /// or the request itself is unusable. The caller tells the two apart.
    Invalid,
    Timeout,
    Error,
}

fn classify_miss(failure: &crate::fabric::PeerFailure) -> Miss {
    use provenant_fabric_runtime::coordination::facade::FacadeError;
    let detail = failure.detail.as_deref().unwrap_or_default();
    match failure.error {
        FacadeError::InvalidInput { .. } => Miss::Invalid,
        FacadeError::Timeout => Miss::Timeout,
        _ if detail.contains(&provenant_fabric_runtime::hosts::PeerError::Project.to_string()) => {
            Miss::ProjectUnknown
        }
        _ if detail.contains("timed out") => Miss::Timeout,
        _ => Miss::Error,
    }
}

/// Add the first pages of the chosen peers to a local listing. A peer is
/// addressed through a registered project, which it must also know, so each
/// project is tried until one is answered. The attempts of one peer share a
/// single `peer_seconds` budget; a peer that does not answer inside it is
/// reported unreachable, with a reason, and never fails the listing.
#[allow(clippy::disallowed_methods)] // The real-time deadline bounds the peer reads.
fn fan_out(
    roots: &provenant_core::Roots,
    home: &Path,
    hosts: &str,
    state: provenant_fabric_runtime::inbox::InboxFilter,
    limit: u64,
    local: InboxResult,
) -> Result<InboxResult, Error> {
    use crate::console::peer_inbox::{InboxObservation, PeerInbox, compose_inbox};
    use provenant_core::Clock;
    use provenant_fabric_runtime::{
        coordination::facade::FacadeError,
        hosts::{PeerAction, PeerResult},
    };
    let (chosen, seconds) = chosen_peers(roots, hosts)?;
    let bindings =
        provenant_config::registered_projects(roots).map_err(crate::fabric::EdgeError::from)?;
    let budget_for = |remaining: std::time::Duration| {
        let whole = remaining
            .as_secs()
            .saturating_add(u64::from(remaining.subsec_nanos() > 0));
        let clock = provenant_os::clock::SystemClock::new();
        WaitBudget::cli(clock.mono_now(), whole.clamp(1, seconds.max(1)))
    };
    let read = |host: provenant_core::HostId| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
        let (mut unknown, mut other) = (0usize, None);
        for binding in &bindings {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                other = Some(Miss::Timeout);
                break;
            }
            let clock = provenant_os::clock::SystemClock::new();
            let (Ok(context), Ok(budget)) = (
                Context::open_for_project(roots.clone(), home, binding),
                budget_for(remaining),
            ) else {
                continue;
            };
            let ask = |limit: u64, budget, remaining| {
                context.peer_within(
                    &clock,
                    host,
                    PeerAction::ConsoleInbox {
                        extra: Default::default(),
                        state,
                        limit,
                    },
                    budget,
                    &provenant_core::CancelToken::new(),
                    remaining,
                )
            };
            match ask(limit, budget, remaining) {
                Ok(PeerResult::Console { data, .. }) => return InboxObservation::Page(data),
                Ok(_) => return InboxObservation::Refused("invalid_page"),
                Err(failure)
                    if matches!(
                        failure.error,
                        FacadeError::PeerVersion | FacadeError::PeerProtocolMismatch { .. }
                    ) =>
                {
                    return InboxObservation::Unsupported;
                }
                Err(failure) => match classify_miss(&failure) {
                    Miss::Invalid => {
                        // The refusal carries no cause, so ask for the smallest
                        // page: if that is served, only the page size was refused.
                        let left = deadline.saturating_duration_since(std::time::Instant::now());
                        let served = limit > 1
                            && !left.is_zero()
                            && budget_for(left).is_ok_and(|budget| {
                                matches!(ask(1, budget, left), Ok(PeerResult::Console { .. }))
                            });
                        return InboxObservation::Refused(if served {
                            "limit_refused"
                        } else {
                            "invalid_request"
                        });
                    }
                    Miss::ProjectUnknown => unknown += 1,
                    miss => {
                        // A timeout outranks a generic error in the report.
                        other = Some(match (other, miss) {
                            (Some(Miss::Timeout), _) | (_, Miss::Timeout) => Miss::Timeout,
                            _ => Miss::Error,
                        });
                    }
                },
            }
        }
        match other {
            Some(Miss::Timeout) => InboxObservation::Unreachable("timeout"),
            Some(_) => InboxObservation::Unreachable("error"),
            None if unknown != 0 => InboxObservation::Refused("project_unknown"),
            None => InboxObservation::Unreachable("no_project"),
        }
    };
    let peers: Vec<PeerInbox> = std::thread::scope(|scope| {
        let workers: Vec<_> = chosen
            .iter()
            .map(|(_, host)| scope.spawn(|| read(*host)))
            .collect();
        chosen
            .iter()
            .zip(workers)
            .map(|((label, host), worker)| PeerInbox {
                host: *host,
                label: label.clone(),
                observation: worker
                    .join()
                    .unwrap_or(InboxObservation::Unreachable("error")),
            })
            .collect()
    });
    compose_inbox(local, peers).map_err(Error::Input)
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

type HostListing = (
    Vec<HumanRequest>,
    Option<provenant_fabric_runtime::inbox::InboxCursor>,
    serde_json::Map<String, serde_json::Value>,
);

/// This host's requests across the given registered projects, with each
/// listed project's label and path. Shared by the local listing and the peer
/// action, so both read the same rows.
fn host_listing(
    roots: &provenant_core::Roots,
    bindings: &[provenant_config::ProjectBinding],
    session: Option<&str>,
    state: provenant_fabric_runtime::inbox::InboxFilter,
    limit: u64,
    cursor: Option<provenant_fabric_runtime::inbox::InboxCursor>,
) -> Result<HostListing, InboxError> {
    let clock = provenant_os::clock::SystemClock;
    let cancel = provenant_core::CancelToken::new();
    let mut project_ids: Vec<_> = bindings.iter().map(|b| b.project_id()).collect();
    project_ids.sort();
    project_ids.dedup();
    let (items, next_cursor) = provenant_fabric_runtime::inbox::host_open_requests(
        &roots.state,
        &project_ids,
        session,
        state,
        limit,
        cursor,
        &provenant_fabric_store::Request {
            clock: &clock,
            cancel: &cancel,
        },
    )?;
    let projects = items
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
    Ok((items, next_cursor, projects))
}

/// The first page of this host's requests for `PeerAction::ConsoleInbox`:
/// `{items, projects, truncated}`. It reads existing state only.
pub fn console_inbox_page(
    roots: &provenant_core::Roots,
    state: provenant_fabric_runtime::inbox::InboxFilter,
    limit: u64,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let bindings = provenant_config::registered_projects(roots)?;
    let (items, next_cursor, projects) = host_listing(roots, &bindings, None, state, limit, None)?;
    Ok(serde_json::json!({"items":items,"projects":projects,"truncated":next_cursor.is_some()}))
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
    use provenant_fabric_runtime::inbox::locate_request;
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
            let (items, next_cursor, projects) =
                host_listing(&roots, &bindings, session.as_deref(), state, limit, cursor)?;
            let truncated = next_cursor.is_some();
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

/// The `HOST/ATTEMPT` reference of the resolved attempt while it is live.
/// Steering reaches only a running attempt; a settled one is refused.
pub fn live_steer_reference<'a>(
    checkpoint: &'a provenant_fabric_runtime::graph::GraphCheckpoint,
    resolved: &crate::target::Resolved,
) -> Result<&'a str, &'static str> {
    use provenant_fabric_runtime::graph::AttemptState;
    let reference = resolved
        .attempt_ref
        .as_deref()
        .ok_or("attempt has not launched yet; retry after launch resolves")?;
    checkpoint
        .nodes
        .iter()
        .filter(|node| resolved.node.as_deref() == Some(node.id.as_str()))
        .flat_map(|node| node.recorded_attempts())
        .find(|attempt| attempt.attempt_ref.as_deref() == Some(reference))
        .filter(|attempt| attempt.state != AttemptState::Terminal)
        .and_then(|attempt| attempt.attempt_ref.as_deref())
        .ok_or("target attempt has settled; steering reaches only a live attempt")
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
    let (hosts, project_key, paged, session) = match &command {
        HumanCommand::Answer(args) => (None, args.project_key.clone(), false, None),
        HumanCommand::Inbox(args) if args.command.is_none() => (
            args.hosts.clone(),
            args.project_key.clone(),
            args.cursor.is_some(),
            args.session.clone(),
        ),
        _ => (None, None, false, None),
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
    if hosts.is_some()
        && (!all_projects || remote_owner || !matches!(input, InboxInput::List { id: None, .. }))
    {
        return Err(Error::Input(
            "--hosts needs --all-projects and a listing of this host; it cannot combine with a remote --host",
        ));
    }
    if hosts.is_some() && session.is_some() {
        // A peer's page is not filtered by session, so the listing would mix scopes.
        return Err(Error::Input(
            "--hosts cannot combine with --session: peer pages are not session-filtered",
        ));
    }
    if project_key.is_some() && !(all_projects && remote_owner) {
        return Err(Error::Input(
            "--project-key needs --all-projects and a remote --host",
        ));
    }
    // The value is checked on every page, though only the first page reads peers.
    if let Some(hosts) = &hosts {
        chosen_peers(&roots, hosts)?;
    }
    let list_page = match &input {
        InboxInput::List { state, limit, .. } => Some((*state, *limit)),
        _ => None,
    };
    let context = if all_projects {
        if remote_owner {
            if matches!(input, InboxInput::Close { .. }) {
                return Err(owner_local_only(selected.as_deref().unwrap_or_default()));
            }
            let key = project_key.ok_or(Error::Input(
                "--all-projects with a remote owner needs --project-key",
            ))?;
            let bindings = provenant_config::registered_projects(&roots)
                .map_err(crate::fabric::EdgeError::from)?;
            let mut matching = bindings.iter().filter(|binding| {
                provenant_fabric_runtime::identity::CrossHostProjectKey::from_home(
                    home,
                    binding.common_dir(),
                )
                .is_ok_and(|found| found.as_str() == key)
            });
            let binding = matching
                .next()
                .ok_or_else(|| no_project(cwd, "no registered project has that --project-key"))?;
            if matching.next().is_some() {
                return Err(Error::Input(
                    "--project-key matches more than one registered project",
                ));
            }
            Context::open_for_project(roots.clone(), home, binding)?
        } else {
            match host_wide(input, roots.clone(), home, cwd)? {
                HostWide::Done(result) => {
                    return match (hosts, list_page) {
                        (Some(hosts), Some((state, limit))) if !paged => {
                            fan_out(&roots, home, &hosts, state, limit, result)
                        }
                        _ => Ok(result),
                    };
                }
                HostWide::Project(context, next) => {
                    input = next;
                    *context
                }
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
            return Err(owner_local_only(&host));
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
                code: "UNSUPPORTED_OWNER".into(),
                message: "peer does not support inbox operations".into(),
                operation_id: None,
            }),
            Err(failure) => {
                use provenant_fabric_runtime::coordination::facade::FacadeError;
                let code = match failure.error {
                    FacadeError::LaunchUnknown { .. } => "ambiguous_outcome",
                    FacadeError::PeerVersion | FacadeError::PeerProtocolMismatch { .. } => {
                        "UNSUPPORTED_OWNER"
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
            diagnostic: None,
                code: provenant_records::control::CONTROL_UNSUPPORTED.into(),
                message,
                cause: None,
                repair: Some(
                    "Read `provenant help --all --json` provider_controls for what each provider supports; use `provenant retry` to continue a lane."
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
            diagnostic: None,
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
    fn steer_reaches_only_the_resolved_live_attempt() {
        use crate::target::{Target, Want, resolve};
        use provenant_fabric_runtime::graph::{AttemptState, GraphCheckpoint};
        let host = provenant_core::HostId::from_bytes([1; 16]);
        let reference =
            |n: u8| format!("{host}/{}", provenant_core::AttemptId::from_bytes([n; 16]));
        let attempt = |n: u8, state: &str| {
            serde_json::json!({
            "key":{"graph_id":"g", "node_id":"task", "route_index":0},
            "launched":true, "route":{"alias":"senior"}, "state":state,
            "attempt_ref":reference(n), "outcome":null, "launch_actor":"fixture"})
        };
        let mut checkpoint: GraphCheckpoint = serde_json::from_value(serde_json::json!({
            "schema_version":1, "graph_id":"g", "plan_digest":"digest",
            "nodes":[{"id":"task", "state":"running", "worktree":null,
                "attempts":[attempt(1, "terminal"), attempt(2, "running")]}]
        }))
        .unwrap();
        let steer = |checkpoint: &GraphCheckpoint, spelling: &str| {
            let resolved = resolve(
                &Target::parse(spelling).unwrap(),
                Want::Attempt { settled: false },
                |_| Ok::<_, ()>(Some(checkpoint.clone())),
            )
            .unwrap();
            live_steer_reference(checkpoint, &resolved).map(str::to_owned)
        };
        assert_eq!(steer(&checkpoint, "g"), Ok(reference(2)));
        assert_eq!(steer(&checkpoint, "g/task#2"), Ok(reference(2)));
        assert!(steer(&checkpoint, "g/task#1").is_err());
        checkpoint.nodes[0].attempts[1].state = AttemptState::Terminal;
        assert!(steer(&checkpoint, "g/task").is_err());
        checkpoint.nodes[0].attempts[1].attempt_ref = None;
        assert!(steer(&checkpoint, "g/task").is_err());
    }

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
