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
    Config(Config),
    #[command(subcommand)]
    Fabric(crate::fabric::Fabric),
    Install(Install),
    Uninstall(Uninstall),
    #[command(subcommand)]
    Route(Route),
    Dispatch(Box<Dispatch>),
    Lanes(Lanes),
    Output(Output),
    Cancel(Cancel),
    Resume(Resume),
    Reconcile(Reconcile),
    #[command(subcommand)]
    Telemetry(Telemetry),
    #[command(subcommand)]
    Quota(Quota),
    #[command(subcommand)]
    Worktree(Worktree),
    #[command(subcommand)]
    Check(Check),
    #[command(subcommand)]
    Hook(Hook),
    #[command(subcommand, hide = true)]
    Internal(Internal),
}
#[derive(Debug, Subcommand)]
pub enum Config {
    Show,
}

pub use crate::install::{Install, InstallCheck, Uninstall};
#[derive(Debug, Subcommand)]
pub enum Route {
    List(Adapter),
    Resolve(RouteRequest),
}
#[derive(Debug, Args)]
pub struct Adapter {
    #[arg(long)]
    pub adapter: Option<String>,
}
#[derive(Debug, Args)]
pub struct RouteRequest {
    #[arg(long, conflicts_with = "route")]
    pub model: Option<Model>,
    #[arg(long)]
    pub route: Option<String>,
    #[arg(long)]
    pub effort: Option<String>,
    #[arg(long, value_delimiter = ',', action = clap::ArgAction::Set)]
    pub fallback: Vec<Model>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model(pub String);
impl FromStr for Model {
    type Err = &'static str;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if text
            .split_once('/')
            .is_none_or(|(adapter, id)| adapter.is_empty() || id.is_empty())
        {
            return Err("model must be ADAPTER/ID");
        }
        Ok(Self(text.into()))
    }
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
    #[command(flatten)]
    pub route: RouteRequest,
    #[command(flatten)]
    pub access: Access,
    #[arg(long, default_value = "3600", default_value_if("mode", "worktree-write", "10800"), value_parser = clap::value_parser!(u64).range(1..=14400))]
    pub provider_timeout: u64,
    #[arg(long, default_value = "4", requires = "manifest", conflicts_with_all = ["prompt_file", "prompt_stdin"], value_parser = clap::value_parser!(u64).range(1..=8))]
    pub concurrency: u64,
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
    #[arg(long, value_parser = timestamp)]
    pub since: Option<jiff::Timestamp>,
    #[arg(long, value_parser = timestamp)]
    pub until: Option<jiff::Timestamp>,
    #[arg(long)]
    pub detail: bool,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Trigger {
    Timer,
    Panel,
    Manual,
    Wake,
    Power,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Power {
    Ac,
    Battery,
    Low,
}
#[derive(Debug, Subcommand)]
pub enum Quota {
    Show(Adapter),
    Tick {
        #[command(flatten)]
        adapter: Adapter,
        #[arg(long, value_enum, default_value = "manual")]
        trigger: Trigger,
        #[arg(long, value_enum, default_value = "ac")]
        power: Power,
    },
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
#[derive(Debug, Subcommand)]
pub enum Check {
    Config(Page),
    Install(InstallCheck),
    Harness(Page),
    Release {
        #[arg(long, requires = "head")]
        base: Option<GitOid>,
        #[arg(long, requires = "base")]
        head: Option<GitOid>,
        #[command(flatten)]
        page: Page,
    },
    Writing {
        #[arg(required = true, num_args = 1..=256)]
        paths: Vec<PathBuf>,
        #[arg(long, value_enum, default_value = "general")]
        rules: Rules,
        #[command(flatten)]
        page: Page,
    },
}
#[derive(Debug, Subcommand)]
pub enum Hook {
    #[command(subcommand)]
    Claude(ClaudeHook),
    #[command(subcommand)]
    Git(GitHook),
}
#[derive(Debug, Subcommand)]
pub enum ClaudeHook {
    PreToolUse,
    PreCompact,
    Compact,
}
#[derive(Debug, Subcommand)]
pub enum GitHook {
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

fn absolute(value: PathBuf) -> Result<PathBuf, &'static str> {
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
