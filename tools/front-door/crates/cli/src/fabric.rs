//! Fabric's clap subtree and edge composition. Runtime owns coordination policy.
mod context;

pub(crate) use context::private_dir;
/// Shared identity/config setup for CLI leaves and the binary MCP transport.
pub use context::{Context, PeerFailure, current_session, facade_outcome, in_lane, publish};

/// Trusted project and actor identity for sibling leaves such as workflow.
pub(crate) fn identity(
    roots: provenant_core::Roots,
    home: &std::path::Path,
    cwd: &std::path::Path,
) -> Result<(provenant_core::Roots, provenant_core::ProjectId, ActorId), EdgeError> {
    let ctx = context::Context::open(roots, home, cwd)?;
    Ok((ctx.roots, ctx.identity.project, ctx.identity.inbox.actor))
}

use crate::Globals;
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
    /// Serve the Fabric MCP transport on standard input and output
    Mcp {
        /// Harness the client is
        #[arg(long, value_parser = crate::install::harness_parser())]
        client: Option<provenant_records::Harness>,
    },
    /// Show this agent's project and inbox identity
    Identity,
    #[command(hide = true)]
    Peer {
        /// Handle the request in this process; never forward to the peer agent
        #[arg(long)]
        in_process: bool,
    },
    /// Serve peer requests from the login session (macOS LaunchAgent)
    #[command(hide = true)]
    PeerAgent,
    /// Run and track task graphs
    #[command(subcommand)]
    Graph(Graph),
    /// Send and read inbox messages
    #[command(subcommand)]
    Mail(Mail),
    /// Create, claim and finish shared tasks
    #[command(subcommand)]
    Task(Task),
    /// Read and add to the activity log
    #[command(subcommand)]
    Events(Events),
    /// Claim issues, paths and worktrees
    #[command(subcommand)]
    Claim(Claim),
    /// Fence and land pushes to a shared reference
    #[command(subcommand)]
    Landing(Landing),
}
#[derive(Debug, Subcommand)]
pub enum Graph {
    /// Run a task graph document
    Run {
        /// Project-local graph file; bare names default to .provenant/scratch/graphs (or .agent-run/graphs)
        #[arg(long)]
        document: PathBuf,
    },
    /// Show a task graph's status
    Status {
        /// Graph ID
        id: String,
    },
    /// Cancel a task graph
    Cancel {
        /// Graph ID
        id: String,
    },
    /// Resume a task graph from its document
    Resume {
        /// Project-local graph document
        #[arg(long)]
        document: PathBuf,
        /// Raise a limit, budget or loop allowance, or decide a paused loop (KEY=VALUE); may be repeated
        #[arg(long, value_name = "KEY=VALUE")]
        extend: Vec<String>,
    },
}

#[derive(Debug, Args)]
pub struct Operation {
    /// Caller-chosen ID that makes retries idempotent
    #[arg(long)]
    pub operation_id: OperationId,
}
#[derive(Debug, Args)]
pub struct Lease {
    /// Lease duration in seconds (1 to 3600)
    #[arg(long, default_value = "300", value_parser = clap::value_parser!(u64).range(1..=3600))]
    pub lease_seconds: u64,
}
#[derive(Debug, Args)]
pub struct SequencePage {
    /// Return entries after this sequence number
    #[arg(long, value_parser = clap::value_parser!(i64).range(0..))]
    pub after: Option<i64>,
    /// Maximum entries (1 to 100)
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
    /// Send a message to one or more inboxes
    Send {
        /// Inbox label on this host, or HOST_ID/LABEL.
        #[arg(long, required = true)]
        to: Vec<String>,
        /// Message kind
        #[arg(long, value_enum, default_value = "note")]
        kind: Kind,
        /// Message body
        #[arg(long)]
        body: String,
        /// Message this replies to
        #[arg(long)]
        reply_to: Option<provenant_core::MessageId>,
        /// Task this message concerns
        #[arg(long)]
        task: Option<TaskId>,
        #[command(flatten)]
        operation: Operation,
    },
    /// Read inbox messages
    Read(SequencePage),
    /// Mark messages read up to a sequence number
    MarkRead {
        /// Last sequence number to mark read
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
    /// Fence generation
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    generation: u64,
    /// Fence token (32 lowercase hex digits)
    #[arg(long)]
    token: Token,
}
#[derive(Debug, Args)]
pub struct TaskFence {
    /// Task ID
    id: TaskId,
    #[command(flatten)]
    fence: Fence,
}
impl TaskFence {
    fn proof(self) -> TaskProof {
        TaskProof {
            extra: Default::default(),
            task: self.id,
            generation: self.fence.generation,
            token: self.fence.token.0,
        }
    }
}
#[derive(Debug, Subcommand)]
pub enum Task {
    /// Create a shared task
    Create {
        /// Task objective
        #[arg(long)]
        objective: String,
        /// Note to attach
        #[arg(long)]
        note: Option<String>,
        #[command(flatten)]
        operation: Operation,
    },
    /// List shared tasks
    List {
        /// Only tasks in this state
        #[arg(long, value_enum)]
        state: Option<State>,
        /// Return tasks after this ID
        #[arg(long)]
        after: Option<TaskId>,
        /// Maximum tasks (1 to 100)
        #[arg(long, default_value = "100", value_parser = clap::value_parser!(u64).range(1..=100))]
        limit: u64,
    },
    /// Claim a task under a lease
    Claim {
        /// Task ID
        id: TaskId,
        #[command(flatten)]
        lease: Lease,
        #[command(flatten)]
        operation: Operation,
    },
    /// Update a claimed task
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
    /// Finish a claimed task
    Finish {
        #[command(flatten)]
        fence: TaskFence,
        /// Final state
        #[arg(long, value_enum, default_value = "done")]
        state: FinishState,
        #[command(flatten)]
        operation: Operation,
    },
    /// Release a claimed task
    Release {
        #[command(flatten)]
        fence: TaskFence,
        #[command(flatten)]
        operation: Operation,
    },
}
#[derive(Debug, Subcommand)]
pub enum Events {
    /// Read activity log entries
    Read {
        #[command(flatten)]
        page: SequencePage,
        /// Wait up to this many seconds for new entries
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..=provenant_core::MAX_LANE_RUNTIME_SECONDS))]
        wait: Option<u64>,
    },
    /// Add a note to the activity log
    Note {
        /// Note text
        #[arg(long)]
        note: String,
        #[command(flatten)]
        operation: Operation,
    },
}
#[derive(Debug, Args)]
#[group(id = "claim_scope", required = true, multiple = false)]
pub struct Scope {
    /// Claim an issue by key
    #[arg(long)]
    issue: Option<String>,
    /// Claim this path (repeatable)
    #[arg(long)]
    path: Vec<PathBuf>,
    /// Claim this worktree
    #[arg(long)]
    worktree: Option<PathBuf>,
}
impl Scope {
    fn value(self) -> Result<WorkScope, EdgeError> {
        if let Some(key) = self.issue {
            return Ok(WorkScope::Issue {
                extra: Default::default(),
                key,
            });
        }
        if let Some(path) = self.worktree {
            return Ok(WorkScope::Worktree {
                extra: Default::default(),
                path: context::canonical(&path)?,
            });
        }
        let paths = self
            .path
            .iter()
            .map(|path| context::canonical(path))
            .collect::<Result<_, _>>()?;
        Ok(WorkScope::Paths {
            extra: Default::default(),
            paths,
        })
    }
}
#[derive(Debug, Args)]
pub struct ClaimFence {
    /// Claim ID
    id: ClaimId,
    #[command(flatten)]
    fence: Fence,
}
impl ClaimFence {
    fn proof(self) -> ClaimProof {
        ClaimProof {
            extra: Default::default(),
            id: self.id,
            generation: self.fence.generation,
            token: self.fence.token.0,
        }
    }
}
#[derive(Debug, Subcommand)]
pub enum Claim {
    /// Acquire a work claim
    Acquire {
        #[command(flatten)]
        scope: Scope,
        #[command(flatten)]
        lease: Lease,
        #[command(flatten)]
        operation: Operation,
    },
    /// Renew a work claim
    Renew {
        #[command(flatten)]
        fence: ClaimFence,
        #[command(flatten)]
        lease: Lease,
        #[command(flatten)]
        operation: Operation,
    },
    /// Verify a work claim is still held
    Verify(ClaimFence),
    /// Release a work claim
    Release {
        #[command(flatten)]
        fence: ClaimFence,
        #[command(flatten)]
        operation: Operation,
    },
    /// List work claims
    List {
        /// Return claims after this ID
        #[arg(long)]
        after: Option<ClaimId>,
        /// Maximum claims (1 to 100)
        #[arg(long, default_value = "100", value_parser = clap::value_parser!(u64).range(1..=100))]
        limit: u64,
    },
}
#[derive(Debug, Args)]
pub struct LandingFence {
    /// Git reference being landed
    #[arg(long)]
    reference: String,
    /// Landing lease ID
    #[arg(long)]
    id: ClaimId,
    /// Lease generation
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
            extra: Default::default(),
            reference: self.reference,
            id: self.id,
            generation: self.generation,
            token: token.0,
        })
    }
}
#[derive(Debug, Subcommand)]
pub enum Landing {
    /// Push the candidate under a landing lease
    Push {
        #[command(flatten)]
        fence: LandingFence,
        #[command(flatten)]
        operation: Operation,
    },
    /// Settle an interrupted landing push
    Reconcile {
        #[command(flatten)]
        fence: LandingFence,
        #[command(flatten)]
        operation: Operation,
    },
    /// Acquire a landing lease on a reference
    Acquire {
        #[arg(long)]
        reference: String,
        /// Commit the reference must currently point to
        #[arg(long)]
        expected: GitOid,
        /// Commit to land
        #[arg(long)]
        candidate: GitOid,
        #[command(flatten)]
        lease: Lease,
        #[command(flatten)]
        operation: Operation,
    },
    /// Renew a landing lease
    Renew {
        #[command(flatten)]
        fence: LandingFence,
        #[command(flatten)]
        lease: Lease,
        #[command(flatten)]
        operation: Operation,
    },
    /// Verify a landing lease is still held
    Verify(LandingFence),
    /// Release a landing lease
    Release {
        #[command(flatten)]
        fence: LandingFence,
        #[command(flatten)]
        operation: Operation,
    },
    /// Wait for a reference's landing generation to advance
    Wait {
        /// Git reference to watch
        #[arg(long)]
        reference: String,
        /// Generation to wait beyond
        #[arg(long)]
        generation: u64,
        /// Seconds to wait (1 to 14400)
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..=provenant_core::MAX_LANE_RUNTIME_SECONDS))]
        wait: u64,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum EdgeError {
    #[error(transparent)]
    Exec(#[from] provenant_os::exec::ExecError),
    #[error(transparent)]
    Coordination(#[from] CoordinationError),
    #[error(transparent)]
    Identity(#[from] provenant_fabric_runtime::identity::IdentityError),
    #[error(transparent)]
    Config(#[from] provenant_config::ConfigError),
    #[error(transparent)]
    Fs(#[from] provenant_os::fs::FsError),
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
    #[error("Git executable is unavailable")]
    GitUnavailable,
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
            Self::Peer { .. } => return "fabric peer".into(),
            Self::PeerAgent => return "fabric peer-agent".into(),
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
    if ctx.in_lane && !cmd.read_only() {
        return Err(CoordinationError::LaneMutation.into());
    }
    if matches!(cmd, Fabric::Identity) {
        return Ok((json(ctx.identity)?, false));
    }
    let clock = provenant_os::clock::SystemClock::new();
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
        | Fabric::Graph(_)
        | Fabric::Peer { .. }
        | Fabric::PeerAgent
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
                    extra: Default::default(),
                    recipients,
                    kind,
                    body,
                    artifacts: vec![],
                    reply_to: reply_to.map(|id| MessageRef::Local {
                        extra: Default::default(),
                        id,
                    }),
                    task: task.map(|id| TaskRef::Local {
                        extra: Default::default(),
                        id,
                    }),
                }),
                operation,
            )?
        }
        Fabric::Mail(Mail::Read(page)) => {
            ctx.page(page.limit)?;
            json(service.mail(
                MailInput::Read {
                    extra: Default::default(),
                    after: page.after,
                    limit: page.limit,
                },
                budget,
                &cancel,
            )?)?
        }
        Fabric::Mail(Mail::MarkRead { through, operation }) => mutate(
            Mutation::Mail(MailInput::MarkRead {
                extra: Default::default(),
                through,
            }),
            operation,
        )?,
        Fabric::Task(Task::Create {
            objective,
            note,
            operation,
        }) => {
            ctx.texts(&objective, note.as_deref())?;
            mutate(
                Mutation::Task(TaskInput::Create {
                    extra: Default::default(),
                    objective,
                    note,
                }),
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
                    extra: Default::default(),
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
                extra: Default::default(),
                id,
                lease_seconds: ctx.lease(lease)?,
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
                    extra: Default::default(),
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
                extra: Default::default(),
                proof: fence.proof(),
                state: state.into(),
            }),
            operation,
        )?,
        Fabric::Task(Task::Release { fence, operation }) => mutate(
            Mutation::Task(TaskInput::Release {
                extra: Default::default(),
                proof: fence.proof(),
            }),
            operation,
        )?,
        Fabric::Events(Events::Read { page, wait }) => {
            ctx.page(page.limit)?;
            let result = service.events(
                EventsInput::Read {
                    extra: Default::default(),
                    after: page.after,
                    limit: page.limit,
                    wait_seconds: wait,
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
                    extra: Default::default(),
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
                extra: Default::default(),
                scope: scope.value()?,
                lease_seconds: ctx.lease(lease)?,
            }),
            operation,
        )?,
        Fabric::Claim(Claim::Renew {
            fence,
            lease,
            operation,
        }) => mutate(
            Mutation::Claim(ClaimInput::Renew {
                extra: Default::default(),
                proof: fence.proof(),
                lease_seconds: ctx.lease(lease)?,
            }),
            operation,
        )?,
        Fabric::Claim(Claim::Verify(fence)) => json(service.claim(
            ClaimInput::Verify {
                extra: Default::default(),
                proof: fence.proof(),
            },
            budget,
            &cancel,
        )?)?,
        Fabric::Claim(Claim::Release { fence, operation }) => mutate(
            Mutation::Claim(ClaimInput::Release {
                extra: Default::default(),
                proof: fence.proof(),
            }),
            operation,
        )?,
        Fabric::Claim(Claim::List { after, limit }) => {
            ctx.page(limit)?;
            json(service.claim(
                ClaimInput::List {
                    extra: Default::default(),
                    after,
                    limit,
                },
                budget,
                &cancel,
            )?)?
        }
        Fabric::Landing(Landing::Acquire {
            reference,
            expected,
            candidate,
            lease,
            operation,
        }) => mutate(
            Mutation::Landing(LandingInput::Acquire {
                extra: Default::default(),
                reference,
                expected,
                candidate,
                lease_seconds: ctx.lease(lease)?,
            }),
            operation,
        )?,
        Fabric::Landing(Landing::Renew {
            fence,
            lease,
            operation,
        }) => mutate(
            Mutation::Landing(LandingInput::Renew {
                extra: Default::default(),
                proof: fence.proof()?,
                lease_seconds: ctx.lease(lease)?,
            }),
            operation,
        )?,
        Fabric::Landing(Landing::Verify(fence)) => json(service.landing(
            LandingInput::Verify {
                extra: Default::default(),
                proof: fence.proof()?,
            },
            budget,
            &cancel,
        )?)?,
        Fabric::Landing(Landing::Release { fence, operation }) => mutate(
            Mutation::Landing(LandingInput::Release {
                extra: Default::default(),
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
                    extra: Default::default(),
                    reference,
                    generation,
                    wait_seconds: wait,
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
                    coordination_outcome(
                        &provenant_fabric_runtime::coordination::facade::FacadeError::from(error),
                    ),
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
        crate::terse(&result.data)
    } else {
        String::new()
    };
    let mode = globals.render_mode(&human, read_only, None);
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

/// The command outcome for a typed coordination error. Store upgrades and peer
/// versions are refusals here, as on the facade edge (`context::facade_outcome`).
pub fn coordination_outcome(
    error: &provenant_fabric_runtime::coordination::facade::FacadeError,
) -> Outcome {
    use provenant_fabric_runtime::coordination::facade::FacadeError;
    match error {
        FacadeError::InvalidInput | FacadeError::NotFound => Outcome::Invalid,
        FacadeError::StaleFence
        | FacadeError::Conflict
        | FacadeError::LaneCoordinationRefused
        | FacadeError::UpgradeRefused
        | FacadeError::UpgradeRequired { .. }
        | FacadeError::PeerVersion
        | FacadeError::PeerProtocolMismatch { .. } => Outcome::Refused,
        FacadeError::Timeout => Outcome::Timeout,
        FacadeError::Cancelled => Outcome::Cancelled,
        _ => Outcome::Error,
    }
}

#[cfg(test)]
mod outcome_tests {
    use super::*;

    #[test]
    fn upgrade_and_peer_version_refusals_match_on_both_edges() {
        use provenant_fabric_runtime::coordination::facade::FacadeError;
        for error in [FacadeError::UpgradeRefused, FacadeError::PeerVersion] {
            assert_eq!(coordination_outcome(&error), Outcome::Refused);
            assert_eq!(facade_outcome(&error).0, Outcome::Refused);
        }
    }
}
