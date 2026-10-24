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
    #[arg(long, global = true, action = clap::ArgAction::Append, num_args = 0, default_missing_value = "true")]
    pub json: Vec<bool>,
    #[arg(long, global = true, value_parser = clap::builder::PathBufValueParser::new().try_map(absolute))]
    pub state_dir: Vec<PathBuf>,
    #[arg(long, global = true)]
    pub project: Vec<PathBuf>,
}
#[derive(Debug, Subcommand)]
pub enum Command {
    Help {
        command: Vec<String>,
    },
    Root,
    #[command(subcommand)]
    Accounts(crate::accounts::Accounts),
    #[command(subcommand)]
    Hosts(Hosts),
    #[command(subcommand)]
    Config(Config),
    #[command(subcommand)]
    Fabric(crate::fabric::Fabric),
    Install(Install),
    Uninstall(Uninstall),
    #[command(subcommand)]
    Route(crate::route::Route),
    Dispatch(Box<Dispatch>),
    Lanes(Lanes),
    Output(Output),
    Cancel(Cancel),
    Resume(Resume),
    Reconcile(Reconcile),
    #[command(subcommand)]
    Telemetry(Telemetry),
    #[command(subcommand)]
    Quota(crate::quota::Quota),
    #[command(subcommand)]
    Chair(Chair),
    #[command(subcommand)]
    Worktree(Worktree),
    #[command(subcommand)]
    Workflow(crate::workflow::Workflow),
    #[command(subcommand)]
    Check(Check),
    #[command(subcommand)]
    Hook(Hook),
    #[command(subcommand, hide = true)]
    Internal(Internal),
}
#[derive(Debug, Subcommand)]
pub enum Chair {
    /// Checkpoint and continue under an allowed registered Claude account.
    Switch {
        #[arg(long)]
        to: Option<String>,
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
        name: String,
        #[arg(allow_negative_numbers = true)]
        value: String,
    },
    /// Remove a knob override so the profile preset or default applies.
    Unset { name: String },
}

pub use crate::install::{Install, Uninstall};
pub use crate::route::{Model, RouteRequest};
#[derive(Debug, Args)]
pub struct Adapter {
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
    #[arg(long, value_enum, default_value = "read-only")]
    pub mode: Mode,
    #[arg(long)]
    pub worktree: Option<PathBuf>,
    #[arg(long)]
    pub read_root: Vec<PathBuf>,
    #[arg(long, value_enum, default_value = "off")]
    pub network: Switch,
    #[arg(long)]
    pub capability: Vec<String>,
}
#[derive(Debug, Args)]
#[group(id = "dispatch_input", required = true, multiple = false)]
pub struct DispatchInput {
    #[arg(long)]
    pub prompt_file: Option<PathBuf>,
    #[arg(long)]
    pub prompt_stdin: bool,
    #[arg(long)]
    pub manifest: Option<PathBuf>,
}
#[derive(Debug, Args)]
pub struct Dispatch {
    #[command(flatten)]
    pub input: DispatchInput,
    #[arg(long)]
    pub operation_id: OperationId,
    #[arg(long)]
    pub task_id: Option<TaskId>,
    #[arg(long)]
    pub delivery_id: Option<DeliveryId>,
    /// Fetch and deliver this writer's final head on the host.
    #[arg(long, requires_all = ["deliver_branch", "worktree"])]
    pub deliver_remote: Option<String>,
    #[arg(long, requires = "deliver_remote")]
    pub deliver_branch: Option<String>,
    /// Create a pull request after the host verifies the pushed head.
    #[arg(long, requires = "deliver_remote")]
    pub open_pr: bool,
    #[command(flatten)]
    pub route: RouteRequest,
    #[command(flatten)]
    pub access: Access,
    #[arg(long, default_value = "3600", default_value_if("mode", "worktree-write", "10800"), value_parser = clap::value_parser!(u64).range(1..=14400))]
    pub provider_timeout: u64,
    #[arg(long, requires = "manifest", conflicts_with_all = ["prompt_file", "prompt_stdin"], value_parser = clap::value_parser!(u64).range(1..=8))]
    pub concurrency: Option<u64>,
    /// Override an effective harness knob for this dispatch (NAME=VALUE).
    #[arg(long = "knob")]
    pub knobs: Vec<String>,
    #[arg(long)]
    pub host: Option<String>,
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
    #[arg(long, conflicts_with = "all")]
    pub id: Vec<Target>,
    #[arg(long)]
    pub all: bool,
    #[arg(long, default_value = "50", value_parser = clap::value_parser!(u64).range(1..=100))]
    pub limit: u64,
    #[arg(long, conflicts_with = "wait")]
    pub cursor: Option<String>,
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=14400))]
    pub wait: Option<u64>,
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
    pub attempt: Attempt,
    #[arg(long, value_enum, default_value = "result")]
    pub part: Part,
    #[arg(long, default_value = "0", conflicts_with = "tail")]
    pub offset: u64,
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=500))]
    pub tail: Option<u64>,
    #[arg(long, default_value = "65536", value_parser = clap::value_parser!(u64).range(1..=65536))]
    pub max_bytes: u64,
}
#[derive(Debug, Args)]
pub struct Cancel {
    pub target: Target,
    #[arg(long)]
    pub operation_id: OperationId,
    #[arg(long, value_parser = reason)]
    pub reason: Option<String>,
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=14400))]
    pub wait: Option<u64>,
}
#[derive(Debug, Args)]
#[group(id = "resume_prompt", required = true, multiple = false)]
pub struct Prompt {
    #[arg(long)]
    pub prompt_file: Option<PathBuf>,
    #[arg(long)]
    pub prompt_stdin: bool,
}
#[derive(Debug, Args)]
pub struct Resume {
    pub attempt: Attempt,
    #[arg(long)]
    pub operation_id: OperationId,
    #[command(flatten)]
    pub prompt: Prompt,
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=14400))]
    pub wait: Option<u64>,
}
#[derive(Debug, Args)]
pub struct Reconcile {
    #[arg(long)]
    pub id: Vec<RunId>,
    #[arg(long, requires = "id")]
    pub abandon_uncertain: bool,
}
#[derive(Debug, Subcommand)]
pub enum Telemetry {
    Stats(Stats),
}
#[derive(Debug, Args)]
pub struct Stats {
    /// Count local CLI verbs and MCP tools, including unused installed units.
    #[arg(long)]
    pub usage: bool,
    #[arg(long, value_parser = stats_timestamp)]
    pub since: Option<jiff::Timestamp>,
    #[arg(long, value_parser = timestamp)]
    pub until: Option<jiff::Timestamp>,
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
    Create {
        branch: String,
        #[arg(long)]
        base: Option<String>,
    },
    List,
    Remove(Remove),
}
#[derive(Debug, Args)]
pub struct Remove {
    #[arg(required_unless_present = "merged", conflicts_with_all = ["merged", "integration", "branch", "since"])]
    pub name: Option<String>,
    #[arg(long, requires_all = ["integration", "remove_scope"])]
    pub merged: bool,
    #[arg(long, requires = "merged")]
    pub integration: Option<String>,
    #[arg(long, requires = "merged", conflicts_with = "since")]
    pub branch: Vec<String>,
    #[arg(long, requires = "merged")]
    pub since: Option<GitOid>,
}
#[derive(Debug, Args)]
pub struct Page {
    #[arg(long, default_value = "50", value_parser = clap::value_parser!(u64).range(1..=100))]
    pub limit: u64,
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
    #[command(subcommand)]
    Claude(ClaudeHook),
    #[command(subcommand)]
    Git(GitHook),
}
#[derive(Debug, Subcommand)]
pub enum ClaudeHook {
    #[command(name = crate::hooks::CHAIR_PROMPT)]
    ChairPrompt,
    #[command(name = crate::hooks::CHAIR_START)]
    ChairStart,
    #[command(name = crate::hooks::PRE_TOOL_USE)]
    PreToolUse,
    #[command(name = crate::hooks::PRE_COMPACT)]
    PreCompact,
    #[command(name = crate::hooks::COMPACT)]
    Compact,
}
#[derive(Debug, Subcommand)]
pub enum GitHook {
    #[command(name = crate::hooks::PRE_PUSH)]
    PrePush { remote: String, location: String },
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
