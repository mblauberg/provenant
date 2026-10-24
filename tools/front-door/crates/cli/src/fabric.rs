//! Fabric's clap subtree and edge composition. Runtime owns coordination policy.
mod context;

/// Shared identity/config setup for CLI leaves and the binary MCP transport.
pub use context::{Context, publish};

/// Trusted project and actor identity for sibling leaves such as workflow.
pub(crate) fn identity(
    roots: provenant_core::Roots,
    home: &std::path::Path,
    cwd: &std::path::Path,
) -> Result<(provenant_core::Roots, provenant_core::ProjectId, ActorId), EdgeError> {
    let ctx = context::Context::open(roots, home, cwd)?;
    Ok((ctx.roots, ctx.identity.project, ctx.identity.inbox.actor))
}

use crate::{Globals, RenderMode};
use clap::{Args, Subcommand, ValueEnum};
use provenant_core::{
    ActorId, CancelToken, ClaimId, Clock, GitOid, OperationId, TaskId, WaitBudget,
};
use provenant_fabric_runtime::coordination::*;
use provenant_records::{CommandError, CommandResult, Outcome};
use serde::Serialize;
use serde_json::Value;
use std::{io, path::PathBuf, str::FromStr};

#[derive(Debug, Subcommand)]
pub enum Fabric {
    Mcp {
        #[arg(long, value_parser = crate::install::harness_parser())]
        client: Option<provenant_records::Harness>,
    },
    Identity,
    #[command(subcommand)]
    Host(Host),
    #[command(hide = true)]
    Peer,
    #[command(subcommand)]
    Graph(Graph),
    #[command(subcommand)]
    Mail(Mail),
    #[command(subcommand)]
    Task(Task),
    #[command(subcommand)]
    Events(Events),
    #[command(subcommand)]
    Claim(Claim),
    #[command(subcommand)]
    Landing(Landing),
}
#[derive(Debug, Subcommand)]
pub enum Host {
    List,
}
#[derive(Debug, Subcommand)]
pub enum Graph {
    Run {
        #[arg(long)]
        document: PathBuf,
    },
    Status {
        id: String,
    },
    Cancel {
        id: String,
    },
    Resume {
        #[arg(long)]
        document: PathBuf,
    },
}

#[derive(Debug, Args)]
pub struct Operation {
    #[arg(long)]
    pub operation_id: OperationId,
}
#[derive(Debug, Args)]
pub struct Lease {
    #[arg(long, default_value = "300", value_parser = clap::value_parser!(u64).range(1..=3600))]
    pub lease_seconds: u64,
}
#[derive(Debug, Args)]
pub struct SequencePage {
    #[arg(long, value_parser = clap::value_parser!(i64).range(0..))]
    pub after: Option<i64>,
    #[arg(long, default_value = "100", value_parser = clap::value_parser!(u64).range(1..=100))]
    pub limit: u64,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Kind {
    Request,
    Response,
    Note,
}
#[derive(Debug, Subcommand)]
pub enum Mail {
    Send {
        /// Inbox label on this host, or HOST_ID/LABEL.
        #[arg(long, required = true)]
        to: Vec<String>,
        #[arg(long, value_enum, default_value = "note")]
        kind: Kind,
        #[arg(long)]
        body: String,
        #[arg(long)]
        reply_to: Option<provenant_core::MessageId>,
        #[arg(long)]
        task: Option<TaskId>,
        #[command(flatten)]
        operation: Operation,
    },
    Read(SequencePage),
    MarkRead {
        #[arg(value_parser = clap::value_parser!(i64).range(0..))]
        through: i64,
        #[command(flatten)]
        operation: Operation,
    },
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum FinishState {
    Done,
    Cancelled,
}
impl From<FinishState> for TaskState {
    fn from(value: FinishState) -> Self {
        match value {
            FinishState::Done => Self::Done,
            FinishState::Cancelled => Self::Cancelled,
        }
    }
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum State {
    Open,
    Done,
    Cancelled,
}
impl From<State> for TaskState {
    fn from(value: State) -> Self {
        match value {
            State::Open => Self::Open,
            State::Done => Self::Done,
            State::Cancelled => Self::Cancelled,
        }
    }
}
#[derive(Clone)]
pub struct Token([u8; 16]);
impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}
impl FromStr for Token {
    type Err = &'static str;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let bytes = provenant_core::unhex(value).ok_or("token must be 32 lowercase hex digits")?;
        let bytes = bytes
            .try_into()
            .map_err(|_| "token must be 32 lowercase hex digits")?;
        Ok(Self(bytes))
    }
}
#[derive(Debug, Args)]
pub struct Fence {
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    generation: u64,
    #[arg(long)]
    token: Token,
}
#[derive(Debug, Args)]
pub struct TaskFence {
    id: TaskId,
    #[command(flatten)]
    fence: Fence,
}
impl TaskFence {
    fn proof(self) -> TaskProof {
        TaskProof {
            task: self.id,
            generation: self.fence.generation,
            token: self.fence.token.0,
        }
    }
}
#[derive(Debug, Subcommand)]
pub enum Task {
    Create {
        #[arg(long)]
        objective: String,
        #[arg(long)]
        note: Option<String>,
        #[command(flatten)]
        operation: Operation,
    },
    List {
        #[arg(long, value_enum)]
        state: Option<State>,
        #[arg(long)]
        after: Option<TaskId>,
        #[arg(long, default_value = "100", value_parser = clap::value_parser!(u64).range(1..=100))]
        limit: u64,
    },
    Claim {
        id: TaskId,
        #[command(flatten)]
        lease: Lease,
        #[command(flatten)]
        operation: Operation,
    },
    Update {
        #[command(flatten)]
        fence: TaskFence,
        #[arg(long)]
        objective: String,
        #[arg(long)]
        note: Option<String>,
        #[command(flatten)]
        operation: Operation,
    },
    Finish {
        #[command(flatten)]
        fence: TaskFence,
        #[arg(long, value_enum, default_value = "done")]
        state: FinishState,
        #[command(flatten)]
        operation: Operation,
    },
    Release {
        #[command(flatten)]
        fence: TaskFence,
        #[command(flatten)]
        operation: Operation,
    },
}
#[derive(Debug, Subcommand)]
pub enum Events {
    Read {
        #[command(flatten)]
        page: SequencePage,
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..=14400))]
        wait: Option<u64>,
    },
    Note {
        #[arg(long)]
        note: String,
        #[command(flatten)]
        operation: Operation,
    },
}
#[derive(Debug, Args)]
#[group(id = "claim_scope", required = true, multiple = false)]
pub struct Scope {
    #[arg(long)]
    issue: Option<String>,
    #[arg(long)]
    path: Vec<PathBuf>,
    #[arg(long)]
    worktree: Option<PathBuf>,
}
impl Scope {
    fn value(self) -> Result<WorkScope, EdgeError> {
        if let Some(key) = self.issue {
            return Ok(WorkScope::Issue { key });
        }
        if let Some(path) = self.worktree {
            return Ok(WorkScope::Worktree {
                path: context::canonical(&path)?,
            });
        }
        let paths = self
            .path
            .iter()
            .map(|path| context::canonical(path))
            .collect::<Result<_, _>>()?;
        Ok(WorkScope::Paths { paths })
    }
}
#[derive(Debug, Args)]
pub struct ClaimFence {
    id: ClaimId,
    #[command(flatten)]
    fence: Fence,
}
impl ClaimFence {
    fn proof(self) -> ClaimProof {
        ClaimProof {
            id: self.id,
            generation: self.fence.generation,
            token: self.fence.token.0,
        }
    }
}
#[derive(Debug, Subcommand)]
pub enum Claim {
    Acquire {
        #[command(flatten)]
        scope: Scope,
        #[command(flatten)]
        lease: Lease,
        #[command(flatten)]
        operation: Operation,
    },
    Renew {
        #[command(flatten)]
        fence: ClaimFence,
        #[command(flatten)]
        lease: Lease,
        #[command(flatten)]
        operation: Operation,
    },
    Verify(ClaimFence),
    Release {
        #[command(flatten)]
        fence: ClaimFence,
        #[command(flatten)]
        operation: Operation,
    },
    List {
        #[arg(long)]
        after: Option<ClaimId>,
        #[arg(long, default_value = "100", value_parser = clap::value_parser!(u64).range(1..=100))]
        limit: u64,
    },
}
#[derive(Debug, Args)]
pub struct LandingFence {
    #[arg(long)]
    reference: String,
    #[arg(long)]
    id: ClaimId,
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    generation: u64,
}
#[derive(Debug, thiserror::Error)]
#[error("invalid landing token")]
pub struct LandingTokenError;

impl From<LandingTokenError> for EdgeError {
    fn from(_: LandingTokenError) -> Self {
        Self::Invalid
    }
}

impl LandingFence {
    /// The fence secret never enters argv or clap's diagnostics.
    pub fn proof(self) -> Result<LandingProof, LandingTokenError> {
        let token = std::env::var("PROVENANT_LANDING_TOKEN")
            .ok()
            .and_then(|value| value.parse::<Token>().ok())
            .ok_or(LandingTokenError)?;
        Ok(LandingProof {
            reference: self.reference,
            id: self.id,
            generation: self.generation,
            token: token.0,
        })
    }
}
#[derive(Debug, Subcommand)]
pub enum Landing {
    Push {
        #[command(flatten)]
        fence: LandingFence,
        #[command(flatten)]
        operation: Operation,
    },
    Reconcile {
        #[command(flatten)]
        fence: LandingFence,
        #[command(flatten)]
        operation: Operation,
    },
    Acquire {
        #[arg(long)]
        reference: String,
        #[arg(long)]
        expected: GitOid,
        #[arg(long)]
        candidate: GitOid,
        #[command(flatten)]
        lease: Lease,
        #[command(flatten)]
        operation: Operation,
    },
    Renew {
        #[command(flatten)]
        fence: LandingFence,
        #[command(flatten)]
        lease: Lease,
        #[command(flatten)]
        operation: Operation,
    },
    Verify(LandingFence),
    Release {
        #[command(flatten)]
        fence: LandingFence,
        #[command(flatten)]
        operation: Operation,
    },
    Wait {
        #[arg(long)]
        reference: String,
        #[arg(long)]
        generation: u64,
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..=14400))]
        wait: u64,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum EdgeError {
    #[error(transparent)]
    Exec(#[from] provenant_supervisor::exec::ExecError),
    #[error(transparent)]
    Coordination(#[from] CoordinationError),
    #[error(transparent)]
    Identity(#[from] provenant_fabric_runtime::identity::IdentityError),
    #[error(transparent)]
    Config(#[from] provenant_config::ConfigError),
    #[error(transparent)]
    Fs(#[from] provenant_supervisor::fs::FsError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Root(#[from] provenant_core::RootError),
    #[error(transparent)]
    Utility(#[from] provenant_confinement::utility::UtilityError),
    #[error(transparent)]
    Time(#[from] provenant_core::TimeError),
    #[error(transparent)]
    Encode(#[from] serde_json::Error),
    #[error("invalid Fabric input")]
    Invalid,
}
fn json(value: impl Serialize) -> Result<Value, EdgeError> {
    Ok(serde_json::to_value(value)?)
}
fn recipient(value: &str, host: provenant_core::HostId) -> Result<ActorId, EdgeError> {
    let (host, label) = match value.split_once('/') {
        Some((host, label)) => (host.parse().map_err(|_| EdgeError::Invalid)?, label),
        None => (host, value),
    };
    Ok(provenant_fabric_runtime::identity::Address::new(host, label)?.actor)
}
impl Fabric {
    fn path(&self) -> String {
        let (domain, verb) = match self {
            Self::Mcp { .. } => return "fabric mcp".into(),
            Self::Identity => return "fabric identity".into(),
            Self::Host(_) => return "fabric host list".into(),
            Self::Peer => return "fabric peer".into(),
            Self::Graph(Graph::Run { .. }) => return "fabric graph run".into(),
            Self::Graph(Graph::Cancel { .. }) => return "fabric graph cancel".into(),
            Self::Graph(Graph::Resume { .. }) => return "fabric graph resume".into(),
            Self::Graph(Graph::Status { .. }) => return "fabric graph status".into(),
            Self::Mail(v) => (
                "mail",
                match v {
                    Mail::Send { .. } => "send",
                    Mail::Read(_) => "read",
                    Mail::MarkRead { .. } => "mark-read",
                },
            ),
            Self::Task(v) => (
                "task",
                match v {
                    Task::Create { .. } => "create",
                    Task::List { .. } => "list",
                    Task::Claim { .. } => "claim",
                    Task::Update { .. } => "update",
                    Task::Finish { .. } => "finish",
                    Task::Release { .. } => "release",
                },
            ),
            Self::Events(v) => (
                "events",
                match v {
                    Events::Read { .. } => "read",
                    Events::Note { .. } => "note",
                },
            ),
            Self::Claim(v) => (
                "claim",
                match v {
                    Claim::Acquire { .. } => "acquire",
                    Claim::Renew { .. } => "renew",
                    Claim::Verify(_) => "verify",
                    Claim::Release { .. } => "release",
                    Claim::List { .. } => "list",
                },
            ),
            Self::Landing(v) => (
                "landing",
                match v {
                    Landing::Push { .. } => "push",
                    Landing::Reconcile { .. } => "reconcile",
                    Landing::Acquire { .. } => "acquire",
                    Landing::Renew { .. } => "renew",
                    Landing::Verify(_) => "verify",
                    Landing::Release { .. } => "release",
                    Landing::Wait { .. } => "wait",
                },
            ),
        };
        format!("fabric {domain} {verb}")
    }
    fn wait(&self) -> u64 {
        match self {
            Self::Events(Events::Read { wait: Some(s), .. })
            | Self::Landing(Landing::Wait { wait: s, .. }) => *s,
            _ => 55,
        }
    }
    fn read_only(&self) -> bool {
        matches!(
            self,
            Self::Identity
                | Self::Mail(Mail::Read(_))
                | Self::Task(Task::List { .. })
                | Self::Events(Events::Read { .. })
                | Self::Claim(Claim::Verify(_) | Claim::List { .. })
                | Self::Landing(Landing::Verify(_) | Landing::Wait { .. })
        )
    }
}
fn execute(
    cmd: Fabric,
    roots: provenant_core::Roots,
    home: &std::path::Path,
    cwd: &std::path::Path,
) -> Result<(Value, bool), EdgeError> {
    let seconds = cmd.wait();
    let ctx = context::Context::open(roots, home, cwd)?;
    if matches!(cmd, Fabric::Identity) {
        return Ok((json(ctx.identity)?, false));
    }
    let clock = provenant_supervisor::clock::SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), seconds)?;
    let backoff = ThreadWait;
    let service = CoordinationService::for_identity(
        &ctx.roots.state,
        &ctx.identity,
        &ctx.config,
        &ctx.home,
        &ctx.cwd,
        &ctx.executor,
        &clock,
        &backoff,
        budget,
        &cancel,
    )?;
    let mutate = |input, operation: Operation| {
        json(service.mutate(input, &operation.operation_id, budget, &cancel)?)
    };
    let value = match cmd {
        Fabric::Identity
        | Fabric::Mcp { .. }
        | Fabric::Host(_)
        | Fabric::Graph(_)
        | Fabric::Peer
        | Fabric::Landing(Landing::Push { .. } | Landing::Reconcile { .. }) => {
            return Err(EdgeError::Invalid);
        }
        Fabric::Mail(Mail::Send {
            to,
            kind,
            body,
            reply_to,
            task,
            operation,
        }) => {
            ctx.text(&body)?;
            let recipients = to
                .iter()
                .map(|v| recipient(v, ctx.identity.host))
                .collect::<Result<_, _>>()?;
            let kind = match kind {
                Kind::Request => MessageKind::Request,
                Kind::Response => MessageKind::Response,
                Kind::Note => MessageKind::Note,
            };
            mutate(
                Mutation::Mail(MailInput::Send {
                    recipients,
                    kind,
                    body,
                    artifacts: vec![],
                    reply_to: reply_to.map(|id| MessageRef::Local { id }),
                    task: task.map(|id| TaskRef::Local { id }),
                }),
                operation,
            )?
        }
        Fabric::Mail(Mail::Read(page)) => {
            ctx.page(page.limit)?;
            json(service.mail(
                MailInput::Read {
                    after: page.after,
                    limit: page.limit,
                },
                budget,
                &cancel,
            )?)?
        }
        Fabric::Mail(Mail::MarkRead { through, operation }) => {
            mutate(Mutation::Mail(MailInput::MarkRead { through }), operation)?
        }
        Fabric::Task(Task::Create {
            objective,
            note,
            operation,
        }) => {
            ctx.texts(&objective, note.as_deref())?;
            mutate(
                Mutation::Task(TaskInput::Create { objective, note }),
                operation,
            )?
        }
        Fabric::Task(Task::List {
            state,
            after,
            limit,
        }) => {
            ctx.page(limit)?;
            json(service.task(
                TaskInput::List {
                    state: state.map(Into::into),
                    after,
                    limit,
                },
                budget,
                &cancel,
            )?)?
        }
        Fabric::Task(Task::Claim {
            id,
            lease,
            operation,
        }) => mutate(
            Mutation::Task(TaskInput::Claim {
                id,
                lease_ms: ctx.lease(lease)?,
            }),
            operation,
        )?,
        Fabric::Task(Task::Update {
            fence,
            objective,
            note,
            operation,
        }) => {
            ctx.texts(&objective, note.as_deref())?;
            mutate(
                Mutation::Task(TaskInput::Update {
                    proof: fence.proof(),
                    objective,
                    note,
                }),
                operation,
            )?
        }
        Fabric::Task(Task::Finish {
            fence,
            state,
            operation,
        }) => mutate(
            Mutation::Task(TaskInput::Finish {
                proof: fence.proof(),
                state: state.into(),
            }),
            operation,
        )?,
        Fabric::Task(Task::Release { fence, operation }) => mutate(
            Mutation::Task(TaskInput::Release {
                proof: fence.proof(),
            }),
            operation,
        )?,
        Fabric::Events(Events::Read { page, wait }) => {
            ctx.page(page.limit)?;
            let result = service.events(
                EventsInput::Read {
                    after: page.after,
                    limit: page.limit,
                    wait_ms: wait.map(|s| s * 1000),
                },
                budget,
                &cancel,
            )?;
            let timed_out = matches!(
                result,
                EventsResult::Read {
                    timed_out: true,
                    ..
                }
            );
            return Ok((json(result)?, timed_out));
        }
        Fabric::Events(Events::Note { note, operation }) => {
            ctx.text(&note)?;
            mutate(
                Mutation::Events(EventsInput::Note {
                    note,
                    dedup_key: None,
                }),
                operation,
            )?
        }
        Fabric::Claim(Claim::Acquire {
            scope,
            lease,
            operation,
        }) => mutate(
            Mutation::Claim(ClaimInput::Acquire {
                scope: scope.value()?,
                lease_ms: ctx.lease(lease)?,
            }),
            operation,
        )?,
        Fabric::Claim(Claim::Renew {
            fence,
            lease,
            operation,
        }) => mutate(
            Mutation::Claim(ClaimInput::Renew {
                proof: fence.proof(),
                lease_ms: ctx.lease(lease)?,
            }),
            operation,
        )?,
        Fabric::Claim(Claim::Verify(fence)) => json(service.claim(
            ClaimInput::Verify {
                proof: fence.proof(),
            },
            budget,
            &cancel,
        )?)?,
        Fabric::Claim(Claim::Release { fence, operation }) => mutate(
            Mutation::Claim(ClaimInput::Release {
                proof: fence.proof(),
            }),
            operation,
        )?,
        Fabric::Claim(Claim::List { after, limit }) => {
            ctx.page(limit)?;
            json(service.claim(ClaimInput::List { after, limit }, budget, &cancel)?)?
        }
        Fabric::Landing(Landing::Acquire {
            reference,
            expected,
            candidate,
            lease,
            operation,
        }) => mutate(
            Mutation::Landing(LandingInput::Acquire {
                reference,
                expected,
                candidate,
                lease_ms: ctx.lease(lease)?,
            }),
            operation,
        )?,
        Fabric::Landing(Landing::Renew {
            fence,
            lease,
            operation,
        }) => mutate(
            Mutation::Landing(LandingInput::Renew {
                proof: fence.proof()?,
                lease_ms: ctx.lease(lease)?,
            }),
            operation,
        )?,
        Fabric::Landing(Landing::Verify(fence)) => json(service.landing(
            LandingInput::Verify {
                proof: fence.proof()?,
            },
            budget,
            &cancel,
        )?)?,
        Fabric::Landing(Landing::Release { fence, operation }) => mutate(
            Mutation::Landing(LandingInput::Release {
                proof: fence.proof()?,
            }),
            operation,
        )?,
        Fabric::Landing(Landing::Wait {
            reference,
            generation,
            wait,
        }) => {
            let result = service.landing(
                LandingInput::Wait {
                    reference,
                    generation,
                    wait_ms: wait * 1000,
                },
                budget,
                &cancel,
            )?;
            let timed_out = matches!(
                result,
                LandingResult::Waited {
                    timed_out: true,
                    ..
                }
            );
            return Ok((json(result)?, timed_out));
        }
    };
    Ok((value, false))
}

pub fn run(
    cmd: Fabric,
    globals: &Globals,
    roots: &provenant_core::Roots,
    home: &std::path::Path,
    cwd: &std::path::Path,
) -> u8 {
    let command = cmd.path();
    let read_only = cmd.read_only();
    let mut result = match execute(cmd, roots.clone(), home, cwd) {
        Ok((data, timeout)) => CommandResult::new(
            &command,
            if timeout {
                Outcome::Timeout
            } else {
                Outcome::Ok
            },
            data,
        ),
        Err(error) => {
            let (status, code) = match &error {
                EdgeError::Coordination(error) => (
                    match error.code() {
                        "invalid_input" | "not_found" => Outcome::Invalid,
                        "stale_fence" | "conflict" => Outcome::Refused,
                        "timeout" => Outcome::Timeout,
                        "cancelled" => Outcome::Cancelled,
                        _ => Outcome::Error,
                    },
                    error.code(),
                ),
                EdgeError::Identity(_) | EdgeError::Invalid | EdgeError::Root(_) => {
                    (Outcome::Invalid, "invalid_input")
                }
                EdgeError::Config(_) => (Outcome::Invalid, "config"),
                _ => (Outcome::Error, "io"),
            };
            let mut result = CommandResult::new(&command, status, Value::Null);
            result.error = Some(CommandError {
                code: code.into(),
                message: error.to_string(),
                cause: None,
                repair: None,
            });
            result
        }
    };
    let human = if result.error.is_none() {
        serde_json::to_string_pretty(&result.data)
            .map(|s| format!("{s}\n"))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let mode = if globals.json {
        RenderMode::Json
    } else {
        RenderMode::Human {
            text: &human,
            read_only,
        }
    };
    // Runtime pages/mutation receipts are bounded well below the ordinary cap.
    match crate::render_or_publish(
        &result,
        mode,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
        |value| context::publish(value, roots),
    ) {
        Ok(status) => crate::exit_status(&status),
        Err(_) => {
            result.status = Outcome::Error;
            3
        }
    }
}
