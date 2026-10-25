//! Frozen CLI-owned grammar. Owner subtrees mount only when their owners land.
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
    },
    /// Print the resolved release root
    Root,
    /// Register and list provider accounts
    #[command(subcommand)]
    Accounts(crate::accounts::Accounts),
    /// Check remote host sign-in
    #[command(subcommand)]
    Hosts(Hosts),
    /// Show and edit configuration
    #[command(subcommand)]
    Config(Config),
    /// Coordinate messages, tasks, claims and landings between agents
    #[command(subcommand)]
    Fabric(crate::fabric::Fabric),
    /// Install the release for chosen harnesses
    Install(Install),
    /// Remove installed harness integrations
    Uninstall(Uninstall),
    /// Inspect and edit model routing
    #[command(subcommand)]
    Route(crate::route::Route),
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
    /// Settle open runs and report uncertain custody
    Reconcile(Reconcile),
    /// Report local usage and cost
    #[command(subcommand)]
    Telemetry(Telemetry),
    /// Show and refresh provider quota
    #[command(subcommand)]
    Quota(crate::quota::Quota),
    /// Manage the chair session
    #[command(subcommand)]
    Chair(Chair),
    /// Create, list and remove worktrees
    #[command(subcommand)]
    Worktree(Worktree),
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
pub enum Chair {
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
    Show {
        /// Apply this task class's preset overrides (for example `mechanical`).
        #[arg(long)]
        task_class: Option<String>,
    },
    /// Set `profile` (balanced, economy or rigorous) or a registry knob such
    /// as `lanes.max_concurrency`; `null` clears a nullable knob.
    Set {
        /// Setting name, such as `profile`
        name: String,
        /// New value, or `null` to clear a nullable knob
        #[arg(allow_negative_numbers = true)]
        value: String,
    },
    /// Remove a knob override so the profile preset or default applies.
    Unset {
        /// Setting name to clear
        name: String,
    },
}

pub use crate::install::{Install, Uninstall};
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
    /// Worktree the lane may write to
    #[arg(long)]
    pub worktree: Option<PathBuf>,
    /// Extra directory the lane may read (repeatable)
    #[arg(long)]
    pub read_root: Vec<PathBuf>,
    /// Extra directory the lane may write, outside Git metadata (repeatable)
    #[arg(long)]
    pub write_root: Vec<PathBuf>,
    /// Network access [default: on for worktree-write, off for read-only]
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
#[command(group(clap::ArgGroup::new("writer_target").args(["worktree", "branch"])))]
pub struct Dispatch {
    #[command(flatten)]
    pub input: DispatchInput,
    /// Caller-chosen ID that makes retries idempotent
    #[arg(long)]
    pub operation_id: OperationId,
    /// Fabric task this lane serves
    #[arg(long)]
    pub task_id: Option<TaskId>,
    /// Delivery this lane belongs to
    #[arg(long)]
    pub delivery_id: Option<DeliveryId>,
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
    #[command(flatten)]
    pub access: Access,
    /// Provider timeout in seconds
    #[arg(long, default_value = "3600", default_value_if("mode", "worktree-write", "10800"), value_parser = clap::value_parser!(u64).range(1..=14400))]
    pub provider_timeout: u64,
    /// Manifest lanes to run at once (1 to 8)
    #[arg(long, requires = "manifest", conflicts_with_all = ["prompt_file", "prompt_stdin"], value_parser = clap::value_parser!(u64).range(1..=8))]
    pub concurrency: Option<u64>,
    /// Override an effective harness knob for this dispatch (NAME=VALUE).
    #[arg(long = "knob")]
    pub knobs: Vec<String>,
    /// Configured peer (label or host ID) to run the dispatch on
    #[arg(long)]
    pub host: Option<String>,
    /// New branch for a peer writer; the peer creates its worktree
    #[arg(long, requires_all = ["host", "base"])]
    pub branch: Option<String>,
    /// Commit or remote-tracking ref the peer writer's branch starts from
    #[arg(long, requires = "branch")]
    pub base: Option<String>,
    /// Wait up to this many seconds for the lane to settle
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=14400))]
    pub wait: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub run: RunId,
    pub task: Option<TaskId>,
    pub attempt: Option<provenant_core::AttemptId>,
}
impl FromStr for Target {
    type Err = &'static str;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let mut parts = text.split('/');
        let run = parts
            .next()
            .ok_or("expected RUN[/TASK[/ATTEMPT]]")?
            .parse()
            .map_err(|_| "invalid run ID")?;
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
        Ok(Self { run, task, attempt })
    }
}
#[derive(Debug, Clone)]
pub struct Attempt(pub Target);
impl FromStr for Attempt {
    type Err = &'static str;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let target: Target = text.parse()?;
        if target.attempt.is_none() {
            return Err("expected full RUN/TASK/ATTEMPT");
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
            Self::Cancelled => "cancelled",
            Self::InputRequired => "input_required",
        }
    }
}
#[derive(Debug, Args)]
pub struct Lanes {
    /// Show only this RUN[/TASK[/ATTEMPT]] (repeatable)
    #[arg(long, conflicts_with = "all")]
    pub id: Vec<Target>,
    /// Show every lane
    #[arg(long)]
    pub all: bool,
    /// Maximum lanes per page (1 to 100)
    #[arg(long, default_value = "50", value_parser = clap::value_parser!(u64).range(1..=100))]
    pub limit: u64,
    /// Continue from a previous page's cursor
    #[arg(long, conflicts_with = "wait")]
    pub cursor: Option<String>,
    /// Wait up to this many seconds for a lane condition
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=14400))]
    pub wait: Option<u64>,
    /// Wait for any or all lanes to settle
    #[arg(long, default_value = "any", requires = "wait", value_enum)]
    pub condition: Condition,
    /// Keep lanes in any of these states.
    #[arg(long = "state", conflicts_with = "wait")]
    pub states: Vec<LaneState>,
    /// Keep lanes queued since a duration (such as `24h`) or RFC3339 time.
    #[arg(long, value_parser = lanes_since, conflicts_with = "wait")]
    pub since: Option<jiff::Timestamp>,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Part {
    Result,
    Prompt,
    Stderr,
    Events,
}
#[derive(Debug, Args)]
pub struct Output {
    /// Attempt as RUN/TASK/ATTEMPT
    pub attempt: Attempt,
    /// Which part to read
    #[arg(long, value_enum, default_value = "result")]
    pub part: Part,
    /// Byte offset to start from
    #[arg(long, default_value = "0", conflicts_with = "tail")]
    pub offset: u64,
    /// Read the last N lines instead of an offset
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=500))]
    pub tail: Option<u64>,
    /// Maximum bytes to return
    #[arg(long, default_value = "65536", value_parser = clap::value_parser!(u64).range(1..=65536))]
    pub max_bytes: u64,
}
#[derive(Debug, Args)]
pub struct Cancel {
    /// Target as RUN[/TASK[/ATTEMPT]]
    pub target: Target,
    /// Caller-chosen ID that makes retries idempotent
    #[arg(long)]
    pub operation_id: OperationId,
    /// Reason to record (up to 256 bytes)
    #[arg(long, value_parser = reason)]
    pub reason: Option<String>,
    /// Wait up to this many seconds for cancellation to settle
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=14400))]
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
    /// Attempt as RUN/TASK/ATTEMPT
    pub attempt: Attempt,
    /// Caller-chosen ID that makes retries idempotent
    #[arg(long)]
    pub operation_id: OperationId,
    #[command(flatten)]
    pub prompt: Prompt,
    /// Wait up to this many seconds for the attempt to settle
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=14400))]
    pub wait: Option<u64>,
}
#[derive(Debug, Args)]
pub struct Reconcile {
    /// Reconcile only this run (repeatable)
    #[arg(long)]
    pub id: Vec<RunId>,
    /// Abandon uncertain custody (not available in this build)
    #[arg(long, requires = "id")]
    pub abandon_uncertain: bool,
}
#[derive(Debug, Subcommand)]
pub enum Telemetry {
    /// Summarise local attempts, usage and cost
    Stats(Stats),
}
#[derive(Debug, Args)]
pub struct Stats {
    /// Count local CLI verbs and MCP tools, including unused installed units.
    #[arg(long)]
    pub usage: bool,
    /// Count from this date, timestamp or day count such as `7d`
    #[arg(long, value_parser = stats_timestamp)]
    pub since: Option<jiff::Timestamp>,
    /// Count up to this date or timestamp
    #[arg(long, value_parser = timestamp)]
    pub until: Option<jiff::Timestamp>,
    /// Restrict usage and cost to one run.
    #[arg(long)]
    pub run_id: Option<RunId>,
    /// Include per-attempt detail
    #[arg(long, conflicts_with = "usage")]
    pub detail: bool,
}
fn stats_timestamp(value: &str) -> Result<jiff::Timestamp, String> {
    if let Some(days) = value.strip_suffix('d') {
        let seconds = days
            .parse::<i64>()
            .ok()
            .filter(|days| *days > 0)
            .and_then(|days| days.checked_mul(86_400))
            .ok_or_else(|| "since must be a positive day count or timestamp".to_owned())?;
        return jiff::Timestamp::now()
            .checked_sub(jiff::SignedDuration::from_secs(seconds))
            .map_err(|error| error.to_string());
    }
    timestamp(value).map_err(str::to_owned)
}
#[derive(Debug, Subcommand)]
pub enum Worktree {
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
    /// Remove a worktree, or all merged ones
    Remove(Remove),
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
    /// ref such as origin/main (NAME: the primary's branch when omitted)
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
    /// Prepare the session before compaction
    #[command(name = crate::hooks::PRE_COMPACT)]
    PreCompact,
    /// Restore chair context after compaction
    #[command(name = crate::hooks::COMPACT)]
    Compact,
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
    Supervise {
        /// Private S2 handoff values, validated and adopted only by supervisor.
        #[arg(hide = true, num_args = 5, value_parser = clap::value_parser!(u64).range(3..=2147483647))]
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
fn lanes_since(value: &str) -> Result<jiff::Timestamp, String> {
    if let Some((amount, unit)) = value.split_at_checked(value.len().saturating_sub(1))
        && matches!(unit, "s" | "m" | "h" | "d")
        && amount.bytes().all(|byte| byte.is_ascii_digit())
    {
        let amount = amount
            .parse::<i64>()
            .map_err(|_| "invalid duration".to_owned())?;
        if amount <= 0 {
            return Err("duration must be positive".into());
        }
        let seconds = amount
            .checked_mul(match unit {
                "s" => 1,
                "m" => 60,
                "h" => 3_600,
                "d" => 86_400,
                _ => return Err("duration must use s, m, h or d".into()),
            })
            .ok_or_else(|| "duration is too large".to_owned())?;
        return jiff::Timestamp::now()
            .checked_sub(jiff::SignedDuration::from_secs(seconds))
            .map_err(|error| error.to_string());
    }
    timestamp(value).map_err(str::to_owned)
}
