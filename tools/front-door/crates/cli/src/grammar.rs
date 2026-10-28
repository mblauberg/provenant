//! CLI-owned grammar with mounted owner subtrees.
use clap::{Args, Parser, Subcommand, ValueEnum, builder::TypedValueParser};
use provenant_core::{DeliveryId, GitOid, OperationId, RunId, TaskId};
pub use provenant_records::Harness;
use std::{path::PathBuf, str::FromStr};

#[derive(Debug, Parser)]
#[command(name = "provenant", disable_help_subcommand = true)]
pub(crate) struct RawInvocation {
    #[command(flatten)]
    pub globals: RawGlobals,
    #[command(subcommand)]
    pub command: Command,
}
#[derive(Debug)]
pub struct Invocation {
    pub globals: Globals,
    pub command: Command,
    /// Space-separated subcommand path, such as `quota tick`.
    pub leaf: String,
}
#[derive(Debug, Default)]
pub struct Globals {
    pub json: bool,
    pub state_dir: Option<PathBuf>,
    pub project: Option<PathBuf>,
}
#[derive(Debug, Args)]
pub(crate) struct RawGlobals {
    /// Print machine-readable JSON output
    #[arg(long, global = true, action = clap::ArgAction::Append, num_args = 0, default_missing_value = "true")]
    pub json: Vec<bool>,
    /// Use this absolute directory for state
    #[arg(long, global = true, value_parser = clap::builder::PathBufValueParser::new().try_map(absolute))]
    pub state_dir: Vec<PathBuf>,
    /// Use this directory as the project
    #[arg(long, global = true)]
    pub project: Vec<PathBuf>,
}
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show help for a command
    Help {
        /// Command path to describe, such as `quota tick`
        command: Vec<String>,
        /// Include advanced commands and options
        #[arg(long)]
        all: bool,
    },
    /// Print the resolved release root
    Root,
    /// Post a human decision, assumption or question
    Ask(crate::inbox::Ask),
    /// List, inspect, close and acknowledge human requests
    Inbox(crate::inbox::Inbox),
    /// Answer a request using the revision you saw
    Answer(crate::inbox::Answer),
    /// Register and list provider accounts
    #[command(subcommand)]
    Accounts(crate::accounts::Accounts),
    /// List hosts and their readiness; `doctor` checks remote sign-in
    Hosts {
        #[command(subcommand)]
        command: Option<Hosts>,
    },
    /// Show and edit configuration
    #[command(subcommand)]
    Config(Config),
    /// Coordinate messages, tasks, claims and landings between agents
    #[command(subcommand)]
    Fabric(crate::fabric::Fabric),
    /// Install the release for chosen harnesses
    Install(Install),
    /// Upgrade the recorded installation on this host and Fabric peers
    Upgrade(Upgrade),
    /// Remove installed harness integrations
    Uninstall(Uninstall),
    /// Inspect and edit model routing
    #[command(subcommand)]
    Route(crate::route::Route),
    /// Dispatch, wait and read the result
    Run(Box<Dispatch>),
    /// Show active and recent lanes, newest first
    Status(Status),
    /// Merge on green CI and prune the merged worktree
    Land {
        #[arg(value_parser = clap::value_parser!(u64).range(1..))]
        number: u64,
        /// Pin a reviewed head instead of using the current PR head
        #[arg(long, hide = true)]
        head: Option<GitOid>,
    },
    /// Start a provider lane or manifest of lanes
    Dispatch(Box<Dispatch>),
    /// List lanes or wait on their state
    Lanes(Lanes),
    /// Read a lane attempt's output
    Output(Output),
    /// Cancel a run, task or attempt
    Cancel(Cancel),
    /// Resume an attempt with a new prompt
    Resume(Resume),
    /// Deliver a settled writer attempt
    #[command(subcommand)]
    Lane(Lane),
    /// Settle open runs and report uncertain custody
    Reconcile(Reconcile),
    /// Report local usage and cost
    #[command(subcommand)]
    Telemetry(Telemetry),
    /// Show and refresh provider quota
    Quota {
        #[command(subcommand)]
        command: Option<crate::quota::Quota>,
    },
    /// Manage the chair session
    #[command(subcommand)]
    Chair(Chair),
    /// Create, list and remove worktrees
    #[command(subcommand)]
    Worktree(Worktree),
    /// Watch pull request CI and merge a verified head
    #[command(subcommand)]
    Pr(Pr),
    /// Collect lane caches, ignored output, merged worktrees and expired captures
    Gc(Gc),
    /// Inspect disk use without changing files
    #[command(subcommand)]
    Disk(Disk),
    /// Mine, verify and run benchmark tasks
    #[command(subcommand)]
    Bench(crate::bench::Bench),
    /// Check reviews and keep receipts and session checkpoints
    #[command(subcommand)]
    Workflow(crate::workflow::Workflow),
    /// Run fast repository checks
    #[command(subcommand)]
    Check(Check),
    /// Run harness and git hook handlers
    #[command(subcommand)]
    Hook(Hook),
    #[command(subcommand, hide = true)]
    Internal(Internal),
}
#[derive(Debug, Subcommand)]
pub enum Pr {
    /// Wait for CI and report pass or fail with the pull request head
    Watch {
        #[arg(value_parser = clap::value_parser!(u64).range(1..))]
        number: u64,
    },
    /// Open a pull request with the repository template filled from receipts
    Open(Box<PrOpen>),
    /// Print one line per pull request: state, head, checks, merge and review
    Status {
        /// Pull requests to report; this account's open ones when omitted
        #[arg(value_parser = clap::value_parser!(u64).range(1..))]
        numbers: Vec<u64>,
    },
}
#[derive(Debug, Args)]
pub struct PrOpen {
    /// The writer attempt that made the change
    #[arg(long)]
    pub from_attempt: Attempt,
    /// An independent review attempt (repeatable); the attempt's delivery
    /// receipt supplies them when omitted
    #[arg(long)]
    pub review: Vec<Attempt>,
    /// Markdown for the summary section; its first line titles the pull request
    #[arg(long)]
    pub summary_file: PathBuf,
    /// Issue the pull request closes (repeatable)
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    pub issue: Vec<u64>,
    /// Branch to open from; the attempt's delivered branch when omitted
    #[arg(long)]
    pub branch: Option<String>,
    /// Branch to merge into; the repository default when omitted
    #[arg(long)]
    pub base: Option<String>,
    /// Open as a draft
    #[arg(long)]
    pub draft: bool,
    /// Print the title and body without opening anything
    #[arg(long)]
    pub dry_run: bool,
}
#[derive(Debug, Subcommand)]
pub enum Chair {
    /// Append and read a chair session's authored updates.
    #[command(subcommand)]
    Log(ChairLog),
    /// Checkpoint and continue under an allowed registered Claude account.
    Switch {
        /// Registered Claude account to switch to
        #[arg(long)]
        to: Option<String>,
        /// Report the switch without performing it
        #[arg(long)]
        dry_run: bool,
        /// Source session; required for real runs unless CLAUDE_SESSION_ID is set.
        #[arg(long)]
        session_id: Option<String>,
    },
}
#[derive(Debug, Subcommand)]
pub enum ChairLog {
    /// Append an update from a JSON file, or standard input with `-`.
    Append {
        /// Chair session ID
        #[arg(long)]
        session: String,
        /// JSON containing markdown, kind, channel and optional links
        #[arg(long)]
        file: String,
        /// Stable retry key; generated when omitted
        #[arg(long, requires = "issued_at", value_parser = explicit_operation_id)]
        operation_id: Option<OperationId>,
        /// Original operation creation time in UTC Unix seconds; reuse for retries
        #[arg(long, requires = "operation_id")]
        issued_at: Option<i64>,
    },
    /// List a bounded page of updates in sequence order.
    List {
        /// Chair session ID
        #[arg(long)]
        session: String,
        /// Return entries after this sequence
        #[arg(long)]
        after: Option<u64>,
        /// Maximum entries in this page
        #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u64).range(1..=100))]
        limit: u64,
    },
}
#[derive(Debug, Subcommand)]
pub enum Hosts {
    /// Probe sign-in without inference or credential access.
    Doctor {
        /// SSH alias to probe; all configured hosts when omitted
        #[arg(value_parser = host_alias)]
        host: Option<String>,
    },
}
fn host_alias(value: &str) -> Result<String, &'static str> {
    if value.is_empty()
        || value.len() > 128
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err("host must be a bounded SSH alias");
    }
    Ok(value.into())
}
#[derive(Debug, Subcommand)]
pub enum Config {
    /// Print the effective configuration and knobs with their source layer.
    Show,
    /// Set `profile` (balanced, economy or rigorous) or a registry knob such
    /// as `lanes.max_concurrency`.
    Set {
        /// Setting name, such as `profile`
        name: String,
        /// New value
        #[arg(allow_negative_numbers = true)]
        value: String,
    },
    /// Remove a knob override so the profile preset or default applies.
    Unset {
        /// Setting name to clear
        name: String,
    },
}

pub use crate::install::{Install, Uninstall, Upgrade};
pub use crate::route::{Model, RouteRequest};
#[derive(Debug, Args)]
pub struct Adapter {
    /// Restrict to this adapter
    #[arg(long)]
    pub adapter: Option<String>,
}
#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum Mode {
    ReadOnly,
    WorktreeWrite,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Switch {
    On,
    Off,
}
#[derive(Debug, Args)]
pub struct Access {
    /// Access mode: read-only or worktree-write
    #[arg(long, value_enum, default_value = "read-only")]
    pub mode: Mode,
    /// Writer worktree; local run creates an owned worktree for a missing branch name
    #[arg(long)]
    pub worktree: Option<PathBuf>,
    /// Extra directory the lane may read (repeatable)
    #[arg(long)]
    pub read_root: Vec<PathBuf>,
    /// Extra directory the lane may write, outside Git metadata (repeatable)
    #[arg(long)]
    pub write_root: Vec<PathBuf>,
    /// Network access [default: on]
    #[arg(long, value_enum)]
    pub network: Option<Switch>,
    /// Grant a named capability: postgres (repeatable)
    #[arg(long)]
    pub capability: Vec<String>,
}
#[derive(Debug, Args)]
#[group(id = "dispatch_input", required = true, multiple = false)]
pub struct DispatchInput {
    /// Read the prompt from this file
    #[arg(long)]
    pub prompt_file: Option<PathBuf>,
    /// Read the prompt from standard input
    #[arg(long)]
    pub prompt_stdin: bool,
    /// Dispatch the tasks in this manifest file
    #[arg(long)]
    pub manifest: Option<PathBuf>,
}
#[derive(Debug, Args)]
#[command(group(clap::ArgGroup::new("writer_target").args(["worktree", "branch", "worktree_branch"])))]
pub struct Dispatch {
    /// Use the build-heavy scheduling priority
    #[arg(long)]
    pub build_heavy: bool,
    #[command(flatten)]
    pub input: DispatchInput,
    /// Caller-chosen ID that makes retries idempotent
    #[arg(long, default_value = "", value_parser = operation_id, hide = true, hide_default_value = true)]
    pub operation_id: OperationId,
    /// Short human title for this task
    #[arg(long, conflicts_with = "manifest")]
    pub title: Option<String>,
    /// Fabric task this lane serves
    #[arg(long)]
    pub task_id: Option<TaskId>,
    /// Delivery this lane belongs to
    #[arg(long)]
    pub delivery_id: Option<DeliveryId>,
    /// Host delivery: none, push, integrate:<branch> (local), or pr (origin)
    #[arg(long, conflicts_with_all = ["deliver_remote", "deliver_branch", "open_pr"])]
    pub deliver: Option<provenant_records::run::DeliveryMode>,
    /// Fetch and deliver this writer's final head on the host.
    #[arg(long, requires_all = ["deliver_branch", "writer_target"])]
    pub deliver_remote: Option<String>,
    /// Branch to push the writer's head to
    #[arg(long, requires = "deliver_remote")]
    pub deliver_branch: Option<String>,
    /// Create a pull request after the host verifies the pushed head.
    #[arg(long, requires = "deliver_remote")]
    pub open_pr: bool,
    #[command(flatten)]
    pub route: RouteRequest,
    /// Pin a registered account (key or label) instead of resolving one
    #[arg(long)]
    pub account: Option<String>,
    #[command(flatten)]
    pub access: Access,
    /// Provider timeout in seconds
    #[arg(long, default_value = "3600", default_value_if("mode", "worktree-write", "10800"), value_parser = clap::value_parser!(u64).range(1..=14400))]
    pub provider_timeout: u64,
    /// Manifest lanes to run at once (1 to 16)
    #[arg(long, requires = "manifest", conflicts_with_all = ["prompt_file", "prompt_stdin"], value_parser = clap::value_parser!(u64).range(1..=16))]
    pub concurrency: Option<u64>,
    /// Override an effective harness knob for this dispatch (NAME=VALUE).
    #[arg(long = "knob")]
    pub knobs: Vec<String>,
    /// Configured peer (label or host ID) to run the dispatch on
    #[arg(long)]
    pub host: Option<String>,
    /// New writer branch; run creates an owned worktree locally or on --host
    #[arg(long, requires_all = ["host", "base"])]
    pub branch: Option<String>,
    /// Create or reuse a clean local writer worktree owned by this lane
    #[arg(long, conflicts_with_all = ["worktree", "branch", "host", "manifest"])]
    pub worktree_branch: Option<String>,
    /// Chair-verified full commit SHA to review (must exist on the executing host)
    #[arg(long)]
    pub head: Option<String>,
    /// Git base for attempt history and a new writer branch
    #[arg(long)]
    pub base: Option<String>,
    /// Wait for settlement (seconds, or a duration such as 30m)
    #[arg(long, value_parser = duration_seconds)]
    pub wait: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// Deferred selection when at least one ID is a unique hexadecimal prefix.
    pub selection: Option<String>,
    pub run: Option<RunId>,
    pub task: Option<TaskId>,
    pub attempt: Option<provenant_core::AttemptId>,
}
impl FromStr for Target {
    type Err = &'static str;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let parts: Vec<_> = text.split('/').collect();
        if parts.len() > 3
            || parts.iter().any(|part| {
                !(8..=32).contains(&part.len())
                    || !part.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        {
            return Err(
                "expected 8+ hexadecimal ID characters; run `provenant status --all` to find IDs",
            );
        }
        if parts.iter().any(|part| part.len() < 32) {
            return Ok(Self {
                selection: Some(text.to_ascii_lowercase()),
                run: None,
                task: None,
                attempt: None,
            });
        }
        if !text.contains('/') {
            return Ok(Self {
                selection: None,
                run: None,
                task: None,
                attempt: Some(text.parse().map_err(|_| "invalid attempt or run ID")?),
            });
        }
        let mut parts = text.split('/');
        let run = Some(
            parts
                .next()
                .ok_or("expected RUN[/TASK[/ATTEMPT]]")?
                .parse()
                .map_err(|_| "invalid run ID")?,
        );
        let task = parts
            .next()
            .map(str::parse)
            .transpose()
            .map_err(|_| "invalid task ID")?;
        let attempt = parts
            .next()
            .map(str::parse)
            .transpose()
            .map_err(|_| "invalid attempt ID")?;
        if parts.next().is_some() {
            return Err("expected RUN[/TASK[/ATTEMPT]]");
        }
        Ok(Self {
            selection: None,
            run,
            task,
            attempt,
        })
    }
}
/// One attempt: a bare ID, `RUN/ATTEMPT`, `HOST/ATTEMPT` (a graph
/// `attempt_ref`) or `RUN/TASK/ATTEMPT`. In the two-part form the qualifier
/// is parsed into `Target::run` and resolved against stored runs and known
/// hosts later; `task` is `None` only there.
#[derive(Debug, Clone)]
pub struct Attempt(pub Target);
impl Attempt {
    /// Whether the target is the two-part `QUALIFIER/ATTEMPT` form.
    pub fn is_qualified_pair(&self) -> bool {
        self.0
            .selection
            .as_ref()
            .is_some_and(|selection| selection.split('/').count() == 2)
            || self.0.run.is_some() && self.0.task.is_none() && self.0.attempt.is_some()
    }
}
impl FromStr for Attempt {
    type Err = &'static str;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        const EXPECTED: &str = "expected ATTEMPT, RUN/ATTEMPT, HOST/ATTEMPT or RUN/TASK/ATTEMPT";
        let target: Target = text.parse().map_err(|_| EXPECTED)?;
        if target.selection.is_some() {
            return Ok(Self(target));
        }
        let mut parts = text.split('/');
        if let (Some(qualifier), Some(attempt), None) = (parts.next(), parts.next(), parts.next()) {
            return Ok(Self(Target {
                selection: None,
                run: Some(qualifier.parse().map_err(|_| "invalid run or host ID")?),
                task: None,
                attempt: Some(attempt.parse().map_err(|_| "invalid attempt ID")?),
            }));
        }
        let target: Target = text.parse().map_err(|_| EXPECTED)?;
        if target.attempt.is_none() {
            return Err(EXPECTED);
        }
        Ok(Self(target))
    }
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Condition {
    Any,
    All,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum LaneState {
    Queued,
    Starting,
    Running,
    Settling,
    Succeeded,
    ReviewRequired,
    Failed,
    Rejected,
    TimedOut,
    RateLimited,
    QuotaLimited,
    AuthRequired,
    ModelUnavailable,
    PermissionDenied,
    Quarantined,
    Lost,
    Uncommitted,
    Cancelled,
    InputRequired,
}
impl LaneState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Settling => "settling",
            Self::Succeeded => "succeeded",
            Self::ReviewRequired => "review_required",
            Self::Failed => "failed",
            Self::Rejected => "rejected",
            Self::TimedOut => "timed_out",
            Self::RateLimited => "rate_limited",
            Self::QuotaLimited => "quota_limited",
            Self::AuthRequired => "auth_required",
            Self::ModelUnavailable => "model_unavailable",
            Self::PermissionDenied => "permission_denied",
            Self::Quarantined => "quarantined",
            Self::Lost => "lost",
            Self::Uncommitted => "uncommitted",
            Self::Cancelled => "cancelled",
            Self::InputRequired => "input_required",
        }
    }
}
#[derive(Debug, Clone, Args)]
pub struct Lanes {
    /// Show this attempt ID or RUN[/TASK[/ATTEMPT]] (repeatable)
    #[arg(long)]
    pub id: Vec<Target>,
    /// List, wait on or watch a configured peer's own lanes, by label or host ID
    #[arg(long, conflicts_with_all = ["id", "cursor"])]
    pub host: Option<String>,
    /// Keep lanes submitted by this session (its agent label)
    #[arg(long, value_parser = session_label, conflicts_with = "all")]
    pub session: Option<String>,
    /// Show every lane
    #[arg(long)]
    pub all: bool,
    /// Maximum lanes per page (1 to 100)
    #[arg(long, default_value = "50", value_parser = clap::value_parser!(u64).range(1..=100))]
    pub limit: u64,
    /// Continue from a previous page's cursor
    #[arg(long, conflicts_with_all = ["wait", "watch"])]
    pub cursor: Option<String>,
    /// Wait for settlement (seconds, or a duration such as 30m)
    #[arg(long, conflicts_with = "watch", value_parser = duration_seconds)]
    pub wait: Option<u64>,
    /// Wait for any or all lanes to settle
    #[arg(
        long,
        default_value = "any",
        requires = "wait",
        conflicts_with = "watch",
        value_enum
    )]
    pub condition: Condition,
    /// Print one line per lane state change until every selected lane has
    /// settled or this many seconds pass (default 14400)
    #[arg(
        long,
        num_args = 0..=1,
        default_missing_value = "14400",
        value_name = "SECONDS",
        conflicts_with_all = ["all", "states", "cursor"],
        value_parser = clap::value_parser!(u64).range(1..=14400)
    )]
    pub watch: Option<u64>,
    /// Keep lanes in any of these states.
    #[arg(long = "state", conflicts_with_all = ["wait", "watch"])]
    pub states: Vec<LaneState>,
    /// Keep lanes queued within this many seconds, or since an RFC3339 time.
    /// With --wait or --watch, count lanes that settled since then.
    #[arg(long, value_parser = since)]
    pub since: Option<jiff::Timestamp>,
}
fn session_label(value: &str) -> Result<String, &'static str> {
    provenant_fabric_runtime::identity::validate_label(value)
        .map(|()| value.to_owned())
        .map_err(|_| "session must be an agent label of 1 to 128 letters, digits, dots, underscores or hyphens")
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Part {
    Result,
    Prompt,
    Stderr,
    Events,
    /// The provider's own transcript, tailed in place
    Transcript,
}
#[derive(Debug, Args)]
pub struct Output {
    /// Attempt ID, or RUN/ATTEMPT, HOST/ATTEMPT or RUN/TASK/ATTEMPT
    pub attempt: Attempt,
    /// Read a configured peer's own attempt, by label or host ID
    #[arg(long)]
    pub host: Option<String>,
    /// Which part to read
    #[arg(long, value_enum, default_value = "result")]
    pub part: Part,
    /// Byte offset to start from
    #[arg(long, default_value = "0", conflicts_with = "tail")]
    pub offset: u64,
    /// Read the last N lines instead of an offset (above 500 is clamped to 500)
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    pub tail: Option<u64>,
    /// Maximum bytes to return
    #[arg(long, default_value = "65536", value_parser = clap::value_parser!(u64).range(1..=65536))]
    pub max_bytes: u64,
    /// Print the attempt's trace and its parent chain instead of a part
    #[arg(long, conflicts_with_all = ["part", "offset", "tail", "max_bytes"])]
    pub trace: bool,
}
#[derive(Debug, Args)]
pub struct Cancel {
    /// Attempt ID or RUN[/TASK[/ATTEMPT]]
    pub target: Target,
    /// Caller-chosen ID that makes retries idempotent
    #[arg(long, default_value = "", value_parser = operation_id, hide = true, hide_default_value = true)]
    pub operation_id: OperationId,
    /// Reason to record (up to 256 bytes)
    #[arg(long, value_parser = reason)]
    pub reason: Option<String>,
    /// Wait up to this many seconds for cancellation to settle
    #[arg(long, value_parser = duration_seconds)]
    pub wait: Option<u64>,
}
#[derive(Debug, Args)]
#[group(id = "resume_prompt", required = true, multiple = false)]
pub struct Prompt {
    /// Read the prompt from this file
    #[arg(long)]
    pub prompt_file: Option<PathBuf>,
    /// Read the prompt from standard input
    #[arg(long)]
    pub prompt_stdin: bool,
}
#[derive(Debug, Args)]
pub struct Resume {
    /// Attempt ID, or RUN/ATTEMPT or RUN/TASK/ATTEMPT
    pub attempt: Attempt,
    /// Caller-chosen ID that makes retries idempotent
    #[arg(long, default_value = "", value_parser = operation_id, hide = true, hide_default_value = true)]
    pub operation_id: OperationId,
    /// Refuse instead of starting fresh when provider continuation is unavailable
    #[arg(long)]
    pub no_fallback: bool,
    #[command(flatten)]
    pub prompt: Prompt,
    /// Wait up to this many seconds for the attempt to settle
    #[arg(long, value_parser = duration_seconds)]
    pub wait: Option<u64>,
}
#[derive(Debug, Args)]
pub struct Reconcile {
    /// Reconcile only this run (repeatable)
    #[arg(long)]
    pub id: Vec<Target>,
}
#[derive(Debug, Subcommand)]
pub enum Lane {
    /// Re-run host delivery, requiring the settled receipt's clean, exact head
    Deliver {
        attempt: Attempt,
        /// Override the recorded delivery mode (default push when none recorded)
        #[arg(long)]
        deliver: Option<provenant_records::run::DeliveryMode>,
    },
}
#[derive(Debug, Subcommand)]
pub enum Telemetry {
    /// Summarise local attempts, usage and cost
    Stats(Stats),
    /// Write attempts as a Chrome Trace Event file for ui.perfetto.dev
    Trace(Trace),
}
#[derive(Debug, Args)]
pub struct Trace {
    /// Output file (default `provenant-trace.json`)
    #[arg(long, default_value = "provenant-trace.json")]
    pub out: std::path::PathBuf,
    /// Include attempts finished from this date or timestamp, or this many seconds ago
    #[arg(long, value_parser = since)]
    pub since: Option<jiff::Timestamp>,
    /// Restrict the trace to one run.
    #[arg(long)]
    pub run: Option<Target>,
    /// Also read the attempts of runs placed on peers, one SSH status read per peer
    #[arg(long)]
    pub peers: bool,
}
#[derive(Debug, Args)]
pub struct Stats {
    /// Report native Claude subagents from one session's subagents directory instead of supervised attempts
    #[arg(long, conflicts_with_all = ["usage", "since", "until", "run_id"])]
    pub claude_subagents: Option<PathBuf>,
    /// Count local CLI verbs and MCP tools, including unused installed units.
    #[arg(long)]
    pub usage: bool,
    /// Count from this date or timestamp, or this many seconds ago
    #[arg(long, value_parser = since)]
    pub since: Option<jiff::Timestamp>,
    /// Count up to this date or timestamp
    #[arg(long, value_parser = timestamp)]
    pub until: Option<jiff::Timestamp>,
    /// Restrict usage and cost to one run.
    #[arg(long)]
    pub run_id: Option<Target>,
    /// Include per-attempt detail
    #[arg(long, conflicts_with = "usage")]
    pub detail: bool,
}
#[derive(Debug, Subcommand)]
pub enum Worktree {
    /// Record custody and ownership for an existing linked worktree
    Adopt {
        /// Existing linked worktree in this project
        path: PathBuf,
        /// Human or session owner label
        #[arg(long, default_value = "manual")]
        owner: Option<String>,
    },
    /// Create a worktree on a new branch
    Create {
        /// New branch name
        branch: String,
        /// Base ref; the current head when omitted
        #[arg(long)]
        base: Option<String>,
    },
    /// List worktrees
    List,
    /// Remove a worktree or merged ones, allowing declared ignored output
    Remove(Remove),
}
#[derive(Debug, Subcommand)]
pub enum Disk {
    /// Report disk use and reclaim classifications without changing files
    Report,
}
#[derive(Debug, Args)]
pub struct Gc {
    /// Run for registered projects from the scheduled per-user agent
    #[arg(long, hide = true)]
    pub scheduled: bool,
    /// Collect pressure classes without fetching or removing worktrees
    #[arg(long, hide = true)]
    pub pressure: bool,
    /// Keep at least this much free space (20G, or plain GiB)
    #[arg(long, value_name = "SIZE", value_parser = provenant_config::parse_cache_size)]
    pub free_floor: Option<u64>,
    /// Report what would be removed without deleting anything
    #[arg(long, conflicts_with = "apply")]
    pub dry_run: bool,
    /// Also delete merged worktrees and ignored declared output (otherwise previewed)
    #[arg(long)]
    pub apply: bool,
    /// Minimum idle age for caches, ignored output and opt-in foreign worktrees
    #[arg(long, default_value = "7", value_parser = clap::value_parser!(u64).range(1..=3650))]
    pub idle_days: u64,
}
#[derive(Debug, Args)]
pub struct Remove {
    /// Worktree to remove
    #[arg(required_unless_present = "merged", conflicts_with_all = ["merged", "branch", "since"])]
    pub name: Option<String>,
    /// With NAME: also delete the branch when it is not merged (clean worktree only)
    #[arg(long, conflicts_with = "merged")]
    pub discard: bool,
    /// Remove worktrees already merged into the integration branch
    #[arg(long, requires_all = ["integration", "remove_scope"])]
    pub merged: bool,
    /// Integration ref to check merges against: a branch or a remote-tracking
    /// ref such as origin/main (NAME: fetched upstream default, falling back
    /// to the primary's branch when unavailable)
    #[arg(long)]
    pub integration: Option<String>,
    /// Limit to this branch (repeatable)
    #[arg(long, requires = "merged", conflicts_with = "since")]
    pub branch: Vec<String>,
    /// Limit to worktrees created since this commit
    #[arg(long, requires = "merged")]
    pub since: Option<GitOid>,
}
#[derive(Debug, Args)]
pub struct Page {
    /// Maximum results per page (1 to 100)
    #[arg(long, default_value = "50", value_parser = clap::value_parser!(u64).range(1..=100))]
    pub limit: u64,
    /// Continue from a previous page's cursor
    #[arg(long)]
    pub cursor: Option<String>,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Rules {
    General,
    Academic,
    Legal,
}
pub use crate::check::Check;
#[derive(Debug, Subcommand)]
pub enum Hook {
    /// Handle Claude Code hook events
    #[command(subcommand)]
    Claude(ClaudeHook),
    /// Handle git hook events
    #[command(subcommand)]
    Git(GitHook),
}
#[derive(Debug, Subcommand)]
pub enum ClaudeHook {
    /// Handle the chair user-prompt hook
    #[command(name = crate::hooks::CHAIR_PROMPT)]
    ChairPrompt,
    /// Handle the chair session-start hook
    #[command(name = crate::hooks::CHAIR_START)]
    ChairStart,
    /// Check a tool call before it runs
    #[command(name = crate::hooks::PRE_TOOL_USE)]
    PreToolUse,
    /// Save lane, PR and worktree state into the checkpoint before compaction
    #[command(name = crate::hooks::PRE_COMPACT)]
    PreCompact,
    /// Restore the checkpoint and report lanes and pull requests after compaction
    #[command(name = crate::hooks::COMPACT)]
    Compact,
    /// Report the checkpoint, lanes and pull requests when a session starts, resumes or clears
    #[command(name = crate::hooks::SESSION_START)]
    SessionStart,
    /// Refresh an existing checkpoint's lane, PR and worktree state when a turn ends
    #[command(name = crate::hooks::STOP)]
    Stop,
    /// Create a Claude Code worktree through `provenant worktree create`
    #[command(name = crate::hooks::WORKTREE_CREATE)]
    WorktreeCreate,
    /// Remove a Claude Code worktree through `provenant worktree remove`
    #[command(name = crate::hooks::WORKTREE_REMOVE)]
    WorktreeRemove,
}
#[derive(Debug, Subcommand)]
pub enum GitHook {
    /// Check a push before it happens
    #[command(name = crate::hooks::PRE_PUSH)]
    PrePush {
        /// Remote name
        remote: String,
        /// Remote location
        location: String,
    },
}
#[derive(Debug, Subcommand)]
pub enum Internal {
    ProviderUpdates,
    Supervise {
        /// Private S2 handoff values, validated and adopted only by supervisor.
        #[arg(hide = true, num_args = 5..=6, value_parser = clap::value_parser!(u64).range(3..=2147483647))]
        descriptors: Vec<u64>,
    },
}

pub(crate) fn absolute(value: PathBuf) -> Result<PathBuf, &'static str> {
    let path = value;
    if !path.is_absolute() {
        return Err("expected an absolute path");
    }
    Ok(path)
}
fn reason(value: &str) -> Result<String, &'static str> {
    if value.len() > 256 {
        return Err("reason exceeds 256 UTF-8 bytes");
    }
    Ok(value.into())
}
fn timestamp(value: &str) -> Result<jiff::Timestamp, &'static str> {
    if value.len() == 10
        && let Ok(date) = value.parse::<jiff::civil::Date>()
    {
        return date
            .to_zoned(jiff::tz::TimeZone::UTC)
            .map(|v| v.timestamp())
            .map_err(|_| "invalid date");
    }
    value
        .parse()
        .map_err(|_| "expected a date or RFC3339 timestamp with an offset")
}
/// A positive whole number of seconds ago, a date or an RFC3339 timestamp.
fn since(value: &str) -> Result<jiff::Timestamp, String> {
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        let seconds = value
            .parse::<i64>()
            .map_err(|_| "invalid duration".to_owned())?;
        if seconds <= 0 {
            return Err("duration must be positive".into());
        }
        return jiff::Timestamp::now()
            .checked_sub(jiff::SignedDuration::from_secs(seconds))
            .map_err(|error| error.to_string());
    }
    timestamp(value).map_err(str::to_owned)
}

#[cfg(test)]
mod target_tests {
    use super::{Attempt, Target, since};
    use provenant_core::{AttemptId, RunId, TaskId};

    #[test]
    fn since_accepts_whole_seconds_only() {
        let before = jiff::Timestamp::now();
        let start = since("86400").unwrap();
        let after = jiff::Timestamp::now();
        let duration = jiff::SignedDuration::from_secs(86_400);
        assert!(start >= before.checked_sub(duration).unwrap());
        assert!(start <= after.checked_sub(duration).unwrap());
        for invalid in ["0", "24h", "5s", "1m", "2d", "7d", "-1", ""] {
            assert!(since(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn since_accepts_timestamps() {
        // Parsed only, never compared with the clock.
        let fixed = "2026-10-26T10:00:00Z"; // timebomb: allow parse-only fixture
        assert_eq!(
            since(fixed).unwrap(),
            fixed.parse::<jiff::Timestamp>().unwrap()
        );
    }

    #[test]
    fn attempt_accepts_bare_and_qualified_ids() {
        let run = RunId::from_bytes([1; 16]);
        let task = TaskId::from_bytes([2; 16]);
        let attempt = AttemptId::from_bytes([3; 16]);
        let bare: Attempt = attempt.to_string().parse().unwrap();
        assert_eq!(bare.0.run, None);
        assert_eq!(bare.0.attempt, Some(attempt));
        let qualified: Attempt = format!("{run}/{task}/{attempt}").parse().unwrap();
        assert_eq!(qualified.0.run, Some(run));
        assert_eq!(qualified.0.task, Some(task));
        assert_eq!(qualified.0.attempt, Some(attempt));
        // A graph `attempt_ref` is `HOST/ATTEMPT`; `RUN/ATTEMPT` has the same shape.
        let pair: Attempt = format!("{run}/{attempt}").parse().unwrap();
        assert!(pair.is_qualified_pair());
        assert_eq!(pair.0.run, Some(run));
        assert_eq!(pair.0.task, None);
        assert_eq!(pair.0.attempt, Some(attempt));
        assert!(!qualified.is_qualified_pair());
        assert!(!bare.is_qualified_pair());
        assert!(format!("{run}/").parse::<Attempt>().is_err());
        assert!(format!("/{attempt}").parse::<Attempt>().is_err());
        for invalid in [
            "",
            "invalid",
            &format!("{run}//{attempt}"),
            &format!("{run}/{task}/{attempt}/extra"),
        ] {
            assert!(invalid.parse::<Target>().is_err(), "{invalid}");
        }
    }
}

fn explicit_operation_id(value: &str) -> Result<OperationId, String> {
    OperationId::parse(value).map_err(|error| error.to_string())
}

/// Generate at parse time so help remains deterministic.
fn operation_id(value: &str) -> Result<OperationId, String> {
    if !value.is_empty() {
        return OperationId::parse(value).map_err(|error| error.to_string());
    }
    let bytes = provenant_os::fs::entropy_128().map_err(|error| error.to_string())?;
    OperationId::parse(&format!(
        "cli-{}",
        provenant_core::AttemptId::from_bytes(bytes)
    ))
    .map_err(|error| error.to_string())
}
fn duration_seconds(value: &str) -> Result<u64, &'static str> {
    let (digits, multiplier) = match value.as_bytes().last() {
        Some(b's') => (&value[..value.len() - 1], 1),
        Some(b'm') => (&value[..value.len() - 1], 60),
        Some(b'h') => (&value[..value.len() - 1], 3600),
        _ => (value, 1),
    };
    digits
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(multiplier))
        .filter(|n| (1..=14400).contains(n))
        .ok_or("wait must be 1s to 4h; run `provenant help run` for examples")
}
#[derive(Debug, Args)]
pub struct Status {
    /// Show this run, task or attempt ID, including settled lanes
    pub id: Option<Target>,
    /// Maximum full records per page
    #[arg(long, hide = true, default_value = "50", value_parser = clap::value_parser!(u64).range(1..=100))]
    pub limit: u64,
    /// Continue a full-record page from its cursor
    #[arg(long, hide = true)]
    pub cursor: Option<u64>,
    /// Include full records and older lanes
    #[arg(long)]
    pub all: bool,
}
