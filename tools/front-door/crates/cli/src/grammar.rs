//! CLI-owned grammar with mounted owner subtrees.
use crate::metadata::CatalogueArg;
use clap::{Args, Parser, Subcommand, ValueEnum, builder::TypedValueParser};
use provenant_core::{AttemptId, GitOid, OperationId};
pub use provenant_records::Harness;
use std::{ffi::OsString, path::PathBuf};

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
    pub full: bool,
    pub state_dir: Option<PathBuf>,
    pub project: Option<PathBuf>,
}
#[derive(Debug, Args)]
pub(crate) struct RawGlobals {
    /// Print machine-readable JSON output
    #[arg(long, global = true, action = clap::ArgAction::Append, num_args = 0, default_missing_value = "true")]
    pub json: Vec<bool>,
    /// Show all human-readable detail instead of a compact summary
    #[arg(long, global = true, action = clap::ArgAction::Append, num_args = 0, default_missing_value = "true")]
    pub full: Vec<bool>,
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
    /// Read a fresh resident-memory sample of an owned lane process tree
    ProcessTree {
        /// PID of the supervised command inside this lane
        #[arg(long, catalog_u32_range = 1..=i32::MAX as i64)]
        root: u32,
    },
    /// Post a human decision, assumption or question
    Ask(crate::inbox::Ask),
    /// List, inspect, close and acknowledge human requests
    Inbox(Box<crate::inbox::Inbox>),
    /// Answer a request using the revision you saw
    Answer(Box<crate::inbox::Answer>),
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
    /// Start work from a template or graph document
    #[command(add = crate::metadata::CoreCommand(0, "provenant run task 'Meet the acceptance criteria'"))]
    Run(Box<GraphRunArgs>),
    /// Show graph, node and attempt status
    #[command(add = crate::metadata::CoreCommand(1, "provenant status"))]
    Status(Status),
    /// Wait for graph work or pull requests to settle
    #[command(add = crate::metadata::CoreCommand(10, "provenant wait fix-login"))]
    Wait(Wait),
    /// Merge on green CI and prune the merged worktree
    #[command(add = crate::metadata::CoreCommand(8, "provenant land 1947 --wait"))]
    Land {
        /// Pull request numbers in landing order
        #[arg(catalog_u64_range = 1..)]
        numbers: Vec<u64>,
        /// Compute the candidate and decisions without publishing
        #[arg(long)]
        shadow: bool,
        /// Apply the named landing risk tier
        #[arg(long, value_parser = parse_landing_tier)]
        tier: Option<String>,
        /// Pin a reviewed head (any unambiguous object name) instead of the current PR head
        #[arg(long)]
        head: Option<String>,
        /// Wait for this head's CI (default 1 hour; maximum 4 hours)
        #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "1h", catalog_duration())]
        wait: Option<u64>,
        /// Initial CI poll interval in seconds (overrides domains.pr.land.poll_seconds, default 30)
        #[arg(long, catalog_u64_range = 1..)]
        poll: Option<u64>,
        /// Show the resolved landing policy without landing
        #[arg(long)]
        explain: bool,
        /// Land a delta no review covers, recording this reason and who accepted it
        #[arg(long, value_name = "REASON")]
        accept_delta: Option<String>,
    },
    /// Read an attempt's output
    #[command(add = crate::metadata::CoreCommand(2, "provenant output fix-login/review"))]
    Output(Output),
    /// Show a graph's recorded revisions and attempt timeline
    Trace(GraphTrace),
    /// Stop a graph, node or attempt subtree
    #[command(name = "stop", add = crate::metadata::CoreCommand(4, "provenant stop fix-login"))]
    Stop(Cancel),
    /// Retry a graph, node or attempt
    #[command(name = "retry", add = crate::metadata::CoreCommand(5, "provenant retry fix-login"))]
    Retry(Resume),
    /// Queue a steering message for a lane when its provider supports control
    #[command(add = crate::metadata::CoreCommand(3, "provenant steer fix-login/implement --brief-file - < message.md"))]
    Steer(crate::inbox::Steer),
    /// Apply a patch to unstarted graph nodes
    #[command(add = crate::metadata::CoreCommand(6, "provenant edit fix-login --file patch.json"))]
    Edit(Edit),
    /// List or show templates from all layers
    #[command(add = crate::metadata::CoreCommand(7, "provenant templates"))]
    Templates(Templates),
    /// Deliver a settled writer attempt
    #[command(subcommand)]
    Lane(Lane),
    /// Settle open attempts and report uncertain custody
    Reconcile(Reconcile),
    /// Report local usage and cost
    #[command(subcommand)]
    Telemetry(Telemetry),
    /// Record metadata for a native sub-agent attempt
    #[command(subcommand)]
    Native(Native),
    /// Show and refresh provider quota
    #[command(add = crate::metadata::CoreCommand(9, "provenant quota"))]
    Quota {
        #[command(subcommand)]
        command: Option<crate::quota::Quota>,
    },
    /// Read the local console catalogue
    #[command(subcommand)]
    Console(crate::console::Console),
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
    /// Advance due graphs and run scheduled host tasks
    Tick,
    /// Inspect disk use without changing files
    #[command(subcommand)]
    Disk(Disk),
    /// Hand a brief to a provider cloud session and collect its branch
    #[command(subcommand)]
    Cloud(crate::cloud::Cloud),
    /// Check reviews and keep receipts and session checkpoints
    #[command(subcommand)]
    Workflow(crate::workflow::Workflow),
    /// Inspect issue specifications for stale or misleading references
    #[command(subcommand)]
    Tracker(Tracker),
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
pub enum Tracker {
    /// Advise on issue specifications without blocking work
    Lint {
        /// Issue numbers to inspect
        #[arg(catalog_u64_range = 1..)]
        numbers: Vec<u64>,
        /// Read an issue body from a local file
        #[arg(long)]
        file: Option<PathBuf>,
        /// Kind of local issue (task, feature, bug or epic)
        #[arg(long, default_value = "task", value_parser = ["task", "feature", "bug", "epic"])]
        kind: String,
    },
}
#[derive(Debug, Subcommand)]
pub enum Preset {
    /// List available presets
    List,
    /// Show effective preset values and parameters, or the role library; --full includes origins
    Show {
        /// Preset name, or with --roles a role name to show
        name: Option<String>,
        /// List the role library (cards, tiers, families, references and origins)
        #[arg(long)]
        roles: bool,
    },
    /// Explain when to use a preset, its cost, and its typed parameters
    Explain {
        /// Explain one preset, or omit to explain the current rule selection
        name: Option<String>,
        /// Risk tier supplied to selection rules
        #[arg(long)]
        risk: Option<String>,
        /// Task kind supplied to selection rules
        #[arg(long)]
        kind: Option<String>,
        /// Changed or declared path supplied to selection rules (repeatable)
        #[arg(long = "path")]
        paths: Vec<String>,
        /// Prompt size band supplied to selection rules
        #[arg(long)]
        size: Option<String>,
        /// Mark the work as ready, bypassing the readiness route
        #[arg(long)]
        ready: bool,
        /// Route underspecified work through the configured readiness rule
        #[arg(long, conflicts_with = "ready")]
        underspecified: bool,
    },
}
#[derive(Debug, Args)]
pub struct Review {
    /// Git commit to review
    pub target: String,
    /// File containing the review brief
    #[arg(long)]
    pub brief_file: PathBuf,
    /// Override the selected review shape
    #[arg(long)]
    pub shape: Option<String>,
    /// Risk tier used to select a shape
    #[arg(long)]
    pub risk: Option<String>,
    /// Task kind used to select a shape
    #[arg(long)]
    pub kind: Option<String>,
    /// Add a conditional review lens (security, concurrency, or terminal)
    #[arg(long = "risk-tag", value_parser = clap::builder::PossibleValuesParser::new(provenant_routing::risk_tag_names()))]
    pub risk_tags: Vec<String>,
    /// Git range whose changed paths select the shape
    #[arg(long)]
    pub range: Option<String>,
    /// Keep a provider, account or exact model out of every reviewer pool
    /// (repeatable); see `provenant route resolve --help`
    #[arg(long = "exclude", value_name = "SELECTOR")]
    pub exclude: Vec<String>,
    /// Show the selected panel without dispatching reviewers
    #[arg(long)]
    pub explain: bool,
    /// Planner front-end: auto, on or off
    #[arg(long, value_parser = ["auto", "on", "off"])]
    pub plan: Option<String>,
    /// Start a new graph even if this review already ran
    #[arg(long)]
    pub fresh: bool,
    /// Overall wait limit (seconds or a duration such as 30m)
    #[arg(long, default_value = "3600", catalog_duration())]
    pub wait: u64,
}
#[derive(Debug, Subcommand)]
pub enum Pr {
    /// List retained local attempt contributions and missing peer coverage
    Contributions {
        #[arg(catalog_u64_range = 1..)]
        number: u64,
        /// Retry host binding to the observed PR branch and head; close ended bindings
        #[arg(long)]
        bind: bool,
    },
    /// Wait for CI and report pass or fail with each pull request head
    Watch {
        #[arg(required = true, catalog_u64_range = 1..)]
        numbers: Vec<u64>,
    },
    /// Open a pull request with the repository template filled from receipts
    Open(Box<PrOpen>),
    /// Print one line per pull request: state, head, checks, merge and review
    Status {
        /// Pull requests to report; this account's open ones when omitted
        #[arg(catalog_u64_range = 1..)]
        numbers: Vec<u64>,
    },
}
#[derive(Debug, Args)]
pub struct PrOpen {
    /// The writer attempt that made the change, as NAME[/NODE[#N]]
    #[arg(long, value_name = "TARGET")]
    pub from_attempt: Target,
    /// An independent review attempt (repeatable); the attempt's delivery
    /// receipt supplies them when omitted
    #[arg(long, value_name = "TARGET")]
    pub review: Vec<Target>,
    /// Markdown for the summary section; its first line titles the pull request
    #[arg(long)]
    pub summary_file: PathBuf,
    /// Issue the pull request closes (repeatable)
    #[arg(long, catalog_u64_range = 1..)]
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
    /// Read and update a chair session's structured summary.
    #[command(subcommand)]
    Summary(ChairSummary),
    /// Append and read a chair session's authored updates.
    #[command(subcommand)]
    Log(ChairLog),
    /// Queue a message for an exact registered chair session.
    Message(crate::inbox::ChairMessage),
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
pub enum ChairSummary {
    /// Show the current structured summary.
    Show {
        /// Chair session ID
        session: String,
    },
    /// Compare two retained summary revisions.
    Diff {
        /// Chair session ID
        session: String,
        /// Earlier summary revision
        #[arg(long, catalog_u64_range = 1..)]
        from: u64,
        /// Later summary revision (defaults to current)
        #[arg(long, catalog_u64_range = 1..)]
        to: Option<u64>,
    },
    /// Update selected fields from a JSON patch.
    Update {
        /// Chair session ID
        session: String,
        /// JSON patch file, or - for standard input
        #[arg(long)]
        file: String,
        /// Expected summary revision; use 0 to create a summary
        #[arg(long)]
        expect_revision: u64,
        /// Stable retry key; generated when omitted. The same key with the same
        /// request replays the original result, and with another is refused.
        #[arg(long, value_parser = explicit_operation_id)]
        operation_id: Option<OperationId>,
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
        #[arg(long, catalog_requires = "issued_at", value_parser = explicit_operation_id)]
        operation_id: Option<OperationId>,
        /// Original operation creation time in UTC Unix seconds; reuse for retries
        #[arg(long, catalog_requires = "operation_id")]
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
        #[arg(long, default_value_t = 50, catalog_u64_range = 1..=100)]
        limit: u64,
    },
}
#[derive(Debug, Subcommand)]
pub enum Hosts {
    /// Probe sign-in without inference or credential access.
    Doctor {
        /// Configured peer to probe; the local host when omitted
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
        return Err("host must be a bounded peer name");
    }
    Ok(value.into())
}
#[derive(Debug, Subcommand)]
pub enum Config {
    /// List configuration warnings with their source file and key.
    Check(Page),
    /// Print the effective configuration and knobs with their source layer
    /// and the configuration revision.
    Show,
    /// List each editable field with its type, bounds, default, effective
    /// value, source layer and whether a restart is needed.
    Schema {
        /// Include advanced configuration fields
        #[arg(long)]
        all: bool,
    },
    /// Validate a JSON patch of `{"field": value}` (null clears a field) and
    /// commit all of it or none, if the configuration is still at the revision.
    Apply {
        /// JSON patch file
        #[arg(long, value_name = "PATCH.json")]
        file: PathBuf,
        /// Revision from `config show`; a stale revision is refused
        #[arg(long, value_name = "REV")]
        expect_revision: String,
        /// Validate and report the change without writing
        #[arg(long)]
        dry_run: bool,
    },
    /// List the bounded history of applied configuration changes, newest first.
    History {
        /// Only entries since this RFC3339 time or a span such as 1d ago
        #[arg(long, value_parser = since)]
        since: Option<jiff::Timestamp>,
        /// Maximum entries (1 to 500)
        #[arg(long, default_value = "50", catalog_u64_range = 1..=500)]
        limit: u64,
    },
    /// Set `profile` (balanced or economy) or a registry knob such
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
#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum Mode {
    ReadOnly,
    WorktreeWrite,
}
#[derive(Debug, Clone, Copy, ValueEnum, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Switch {
    On,
    Off,
}
#[derive(Debug, Args)]
pub struct Access {
    /// Access mode: read-only or worktree-write [default: read-only]
    #[arg(long, value_enum)]
    pub mode: Option<Mode>,
    /// Checkout to read or write; a writer can create an owned worktree for a missing branch name
    #[arg(long)]
    pub worktree: Option<PathBuf>,
    /// Extra directory the lane may read (repeatable)
    #[arg(long, catalog_bounds = (0, 32))]
    pub read_root: Vec<PathBuf>,
    /// Image file to attach to the lane (repeatable)
    #[arg(long, catalog_bounds = (0, 32))]
    pub attach: Vec<PathBuf>,
    /// Extra directory the lane may write, outside Git metadata (repeatable)
    #[arg(long, catalog_bounds = (0, 32))]
    pub write_root: Vec<PathBuf>,
    /// Network access [default: on]
    #[arg(long, value_enum)]
    pub network: Option<Switch>,
    /// Grant a named capability: postgres (repeatable)
    #[arg(long, catalog_bounds = (0, 16))]
    pub capability: Vec<String>,
    /// Remove a project-default capability (repeatable)
    #[arg(long, catalog_bounds = (0, 16))]
    pub no_capability: Vec<String>,
}
impl Access {
    /// The effective mode. An omitted `--mode` is read-only.
    pub fn mode(&self) -> Mode {
        self.mode.unwrap_or(Mode::ReadOnly)
    }
}
#[derive(Debug, Args)]
pub struct GraphRunArgs {
    /// Template to run (see `provenant templates`)
    #[arg(required_unless_present = "file", conflicts_with = "file")]
    pub template: Option<String>,
    /// Brief for the template
    #[arg(conflicts_with_all = ["file", "brief_file", "brief_text"])]
    pub brief: Option<String>,
    /// Brief for the template (alternative to the positional brief)
    #[arg(long = "brief", value_name = "TEXT", conflicts_with_all = ["file", "brief", "brief_file"])]
    pub brief_text: Option<String>,
    /// Handle for the graph: `status`, `wait` and `output` take it. The same
    /// input under the same name finds its graph; changed input starts NAME-2.
    #[arg(long, value_parser = graph_name)]
    pub name: Option<String>,
    /// Read the brief from a file, or from standard input given `-`
    #[arg(long, conflicts_with_all = ["file", "brief", "brief_text"])]
    pub brief_file: Option<PathBuf>,
    /// Place attempts on this peer name or host ID
    #[arg(long)]
    pub host: Option<String>,
    /// Use this exact adapter/model instead of a route; a bare model name works when one provider offers it
    #[arg(long, conflicts_with = "route")]
    pub model: Option<String>,
    /// Override model reasoning effort
    #[arg(long)]
    pub effort: Option<String>,
    /// Account label for routing and quota accounting
    #[arg(long)]
    pub account: Option<String>,
    /// Route tier for every node (junior, senior, principal), or a role map such
    /// as writer=junior,review=senior; a role not named keeps the template's tier
    #[arg(long)]
    pub route: Option<String>,
    /// Access for a plain task; writer targets imply worktree-write
    #[arg(long, value_parser = ["read-only", "worktree-write"])]
    pub mode: Option<String>,
    /// Existing writer worktree
    #[arg(long, conflicts_with = "branch")]
    pub worktree: Option<PathBuf>,
    /// Create an owned writer worktree on this branch
    #[arg(long, requires = "base")]
    pub branch: Option<String>,
    /// Git base for a new writer branch
    #[arg(long, requires = "branch")]
    pub base: Option<String>,
    /// Paths the writer may change, relative to its worktree; repeatable
    #[arg(long = "write-scope")]
    pub write_scope: Vec<String>,
    /// Copy the node over comma-separated items, or a resolver such as @review-panel
    #[arg(long, value_delimiter = ',')]
    pub over: Vec<String>,
    /// Bind a template parameter or attempt.FIELD (NAME=VALUE), typed by its
    /// declaration: a string as written, a number or boolean as that type, a
    /// list or table as TOML; repeatable
    #[arg(long = "set")]
    pub set: Vec<String>,
    /// Read a graph document (JSON or TOML)
    #[arg(long)]
    pub file: Option<PathBuf>,
    /// Let this child outlive parent and graph cancellation (inside a lane only)
    #[arg(long)]
    pub detached: bool,
    /// Run the lane under the macOS sandbox (sets attempt.sandbox = "on")
    #[arg(long, conflicts_with = "no_sandbox")]
    pub sandbox: bool,
    /// Run the lane unsandboxed (attempt.sandbox = "off"); overrides route,
    /// network and parent floors where Provenant decides them
    #[arg(long)]
    pub no_sandbox: bool,
    /// Explain the compiled graph without starting work
    #[arg(long)]
    pub explain: bool,
}

/// A graph name, checked where it is typed so a bad one is a usage error.
fn graph_name(text: &str) -> Result<String, String> {
    match provenant_fabric_runtime::graph::template::graph_name_fault(text) {
        None => Ok(text.to_owned()),
        Some(fault) => Err(fault.to_owned()),
    }
}

impl GraphRunArgs {
    /// The `attempt.sandbox` value `--sandbox` or `--no-sandbox` selects.
    pub fn sandbox_override(&self) -> Option<&'static str> {
        if self.sandbox {
            Some("on")
        } else if self.no_sandbox {
            Some("off")
        } else {
            None
        }
    }
}

pub use crate::target::{Resolved, Target, Want};
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Part {
    Result,
    Prompt,
    Stderr,
    Events,
    /// Reviewer or writer provenance for a settled attempt
    Receipt,
    /// The provider's own transcript, tailed in place
    Transcript,
}
#[derive(Clone, Debug, Args)]
pub struct Output {
    /// Attempt to read, as NAME[/NODE[#N]]; a node reads its newest settled attempt
    #[arg(value_name = "TARGET")]
    pub target: Target,
    /// Which part to read
    #[arg(long, value_enum, default_value = "result")]
    pub part: Part,
    /// Write the selected range to FILE, replacing it, instead of printing it
    #[arg(long, value_name = "FILE", conflicts_with = "path")]
    pub to: Option<PathBuf>,
    /// Print the attempt's artifact path for this part instead of its content
    #[arg(long, conflicts_with_all = ["to", "offset", "tail", "max_bytes"])]
    pub path: bool,
    /// Byte offset to start from
    #[arg(long, default_value = "0", conflicts_with = "tail")]
    pub offset: u64,
    /// Read the last N lines instead of an offset (above 500 is clamped to 500)
    #[arg(long, catalog_u64_range = 1..)]
    pub tail: Option<u64>,
    /// Maximum bytes to return
    #[arg(long, default_value = "65536", catalog_u64_range = 1..=65536)]
    pub max_bytes: u64,
}
#[derive(Debug, Args)]
pub struct Cancel {
    /// Graph, node or attempt to stop, as NAME[/NODE[#N]]
    #[arg(value_name = "TARGET")]
    pub target: Target,
    /// Reason to record (up to 256 bytes)
    #[arg(long, value_parser = reason, catalog_bounds = (0, 256))]
    pub reason: Option<String>,
}
#[derive(Debug, Args)]
pub struct Resume {
    /// Graph, node or attempt to retry, as NAME[/NODE[#N]]
    #[arg(value_name = "TARGET")]
    pub target: Target,
    /// Replace the node's brief; a graph target needs exactly one node
    #[arg(long, conflicts_with_all = ["extend", "brief_file"])]
    pub brief: Option<String>,
    /// Read a replacement brief from a file, or from standard input given `-`
    #[arg(long, conflicts_with = "extend")]
    pub brief_file: Option<PathBuf>,
    /// Raise a graph limit or budget (KEY=VALUE) on a graph target; may be repeated
    #[arg(long, value_name = "KEY=VALUE")]
    pub extend: Vec<String>,
}
#[derive(Debug, Args)]
pub struct Reconcile {
    /// Reconcile only these graphs, nodes or attempts (NAME[/NODE[#N]])
    #[arg(value_name = "TARGET", conflicts_with = "abandon")]
    pub targets: Vec<Target>,
    /// Abandon settlement of this writer attempt, preserving its evidence
    #[arg(long, value_name = "TARGET", catalog_requires = "reason")]
    pub abandon: Option<Target>,
    /// Why the chair is abandoning settlement
    #[arg(long, catalog_requires = "abandon", catalog_bounds = (0, 256))]
    pub reason: Option<String>,
}
#[derive(Debug, Subcommand)]
pub enum Lane {
    /// Re-run host delivery, requiring the settled receipt's clean, exact head
    Deliver {
        /// Settled writer attempt, as NAME[/NODE[#N]]
        #[arg(value_name = "TARGET")]
        target: Target,
        /// Override the recorded delivery mode (default push when none recorded)
        #[arg(long)]
        deliver: Option<provenant_records::run::DeliveryMode>,
        /// Accept the settled writer's protected-path review as chair
        #[arg(long)]
        accept_review: bool,
    },
}
#[derive(Debug, Subcommand)]
pub enum Native {
    /// Begin tracking a native tool call; do not pass its prompt
    Start {
        #[arg(long)]
        id: String,
        #[arg(long)]
        session: String,
        #[arg(long)]
        alias: String,
        #[arg(long)]
        model: String,
        #[arg(long)]
        effort: Option<String>,
    },
    /// Settle a tracked call and associate its native agent file ID
    End {
        #[arg(long)]
        id: String,
        #[arg(long)]
        session: String,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long, value_enum)]
        settlement: NativeSettlement,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum NativeSettlement {
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Subcommand)]
pub enum Telemetry {
    /// Summarise local attempts and usage
    Stats(Stats),
    /// Write attempts as a Chrome Trace Event file for ui.perfetto.dev
    Trace(Trace),
    /// Maintain the telemetry price sheet
    Prices {
        #[command(subcommand)]
        command: Prices,
    },
    /// Record native chair and sub-agent token usage per account
    Ingest(Ingest),
    /// Report quota mileage from quota history and ingested usage
    Mileage(Mileage),
    /// Compare each account's API-equivalent lane usage with its plan price and quota readings
    Value(crate::telemetry_args::TelemetryValue),
    /// Report API-equivalent cost from the usage ledger, for one attempt or grouped
    Cost(crate::telemetry_args::TelemetryCost),
    /// Compare experiment arms using offline pipeline outcomes
    #[command(subcommand)]
    Experiments(Experiments),
}
#[derive(Debug, Subcommand)]
pub enum Experiments {
    /// Show one experiment's stratified arm comparison
    Show(ExperimentShow),
}
#[derive(Debug, Args)]
pub struct ExperimentShow {
    /// Historical experiment name recorded in offline outcomes
    pub id: String,
    #[command(flatten)]
    pub pipeline: PipelineOptions,
}
#[derive(Debug, Subcommand)]
pub enum Prices {
    /// Fetch market prices and write a proposed diff; never edits the sheet
    Refresh(PricesRefresh),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum PriceSource {
    Openrouter,
    Litellm,
}
#[derive(Debug, Args)]
pub struct PricesRefresh {
    /// Primary source; the other is the cross-check (default openrouter)
    #[arg(long, value_enum, default_value = "openrouter")]
    pub source: PriceSource,
    /// Read the OpenRouter models document from this file instead of fetching it
    #[arg(long)]
    pub openrouter_file: Option<PathBuf>,
    /// Read the LiteLLM price document from this file instead of fetching it
    #[arg(long)]
    pub litellm_file: Option<PathBuf>,
    /// Proposal file (default `telemetry-prices.proposed.json`)
    #[arg(long, default_value = "telemetry-prices.proposed.json")]
    pub out: PathBuf,
}
#[derive(Debug, Args)]
pub struct Ingest {
    /// Ingest only this provider's sessions (only `claude` writes native transcripts)
    #[arg(long)]
    pub provider: Option<String>,
    /// Ingest transcripts written, and messages stamped, from this date or timestamp, or a span such as 1d ago
    #[arg(long, value_parser = since)]
    pub since: Option<jiff::Timestamp>,
}
#[derive(Debug, Args)]
pub struct Mileage {
    /// Account label or key
    #[arg(long)]
    pub account: Option<String>,
    /// Quota window, such as five_hour or weekly (default every window)
    #[arg(long)]
    pub window: Option<String>,
    /// Count only usage of this exact model; quota deltas still cover every model, so mileage is understated
    #[arg(long)]
    pub model: Option<String>,
    /// Use readings and usage from this date or timestamp, or a span such as 1d ago
    #[arg(long, value_parser = since)]
    pub since: Option<jiff::Timestamp>,
    /// Runs needed on each side before a capacity trend is judged
    #[arg(long, default_value_t = 10, catalog_u32_range = 2..=10_000)]
    pub min_runs: u32,
    /// Fall in capacity, in percent, that is flagged rather than inconclusive
    #[arg(long, default_value_t = 20, catalog_u32_range = 1..=99)]
    pub fall_percent: u32,
    /// Days in the recent side of a capacity step change
    #[arg(long, default_value_t = 7, catalog_u32_range = 1..=90)]
    pub recent_days: u32,
}
#[derive(Debug, Args)]
pub struct Trace {
    /// Output file (default `provenant-trace.json`)
    #[arg(long, default_value = "provenant-trace.json")]
    pub out: std::path::PathBuf,
    /// Include attempts finished from this date or timestamp, or a span such as 1d ago
    #[arg(long, value_parser = since)]
    pub since: Option<jiff::Timestamp>,
    /// Restrict the trace to one graph, node or attempt (NAME[/NODE[#N]])
    #[arg(value_name = "TARGET")]
    pub target: Option<Target>,
    /// Also read the attempts of runs placed on peers, one SSH status read per peer
    #[arg(long)]
    pub peers: bool,
}
#[derive(Debug, Args)]
pub struct Stats {
    /// Include offline post-merge outcomes and stratified ranking
    #[arg(long, conflicts_with = "usage")]
    pub outcomes: bool,
    /// Include advisory expiring suggestions from offline pipeline evidence
    #[arg(long, conflicts_with = "usage")]
    pub suggest: bool,
    #[command(flatten)]
    pub pipeline: PipelineOptions,
    /// Count local CLI verbs, including unused installed units.
    #[arg(long)]
    pub usage: bool,
    /// Count from this date or timestamp, or a span such as 1d ago
    #[arg(long, value_parser = since)]
    pub since: Option<jiff::Timestamp>,
    /// Count up to this date or timestamp
    #[arg(long, value_parser = timestamp)]
    pub until: Option<jiff::Timestamp>,
}

#[derive(Debug, Args)]
pub struct PipelineOptions {
    /// Local main-line ref to analyse; no fetch is performed
    #[arg(long, default_value = "main")]
    pub main_ref: String,
    /// Recorded gh main-branch run list JSON; absent means CI not_measured
    #[arg(long)]
    pub ci_file: Option<PathBuf>,
    /// Days after a merge to attribute escaped defects
    #[arg(long, default_value_t = 14, catalog_u32_range = 1..=3650)]
    pub outcome_days: u32,
    /// Independent graphs and mature merges required per arm
    #[arg(long, default_value_t = 20, catalog_u32_range = 1..=100_000)]
    pub min_samples: u32,
    /// Days until a suggestion expires
    #[arg(long, default_value_t = 14, catalog_u32_range = 1..=3650)]
    pub expiry_days: u32,
}
#[derive(Debug, Subcommand)]
pub enum Worktree {
    /// Build and publish a host-owned Cargo target seed from origin/main
    SeedBuild,
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
    /// Explain a worktree's state and suggest one repair command
    Doctor {
        /// Checkout path, relative to the selected project when not absolute
        path: std::path::PathBuf,
    },
    /// Stream advisory worktree inventory for the SessionStart collector.
    #[command(hide = true)]
    SessionList,
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
    /// Minimum idle age for standalone caches and ignored output; merged trees are immediate
    #[arg(long, default_value = "7", catalog_u64_range = 1..=3650)]
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
    #[arg(long, catalog_requires_all = ["integration", "remove_scope"])]
    pub merged: bool,
    /// Integration ref to check merges against: a branch or a remote-tracking
    /// ref such as origin/main (NAME: fetched upstream default, falling back
    /// to the primary's branch when unavailable)
    #[arg(long)]
    pub integration: Option<String>,
    /// Limit to this branch (repeatable)
    #[arg(long, catalog_requires = "merged", conflicts_with = "since")]
    pub branch: Vec<String>,
    /// Limit to worktrees created since this commit
    #[arg(long, catalog_requires = "merged")]
    pub since: Option<GitOid>,
}
#[derive(Debug, Args)]
pub struct Page {
    /// Maximum results per page (1 to 100)
    #[arg(long, default_value = "50", catalog_u64_range = 1..=100)]
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
    /// Ingest native usage when the chair turn ends
    #[command(name = crate::hooks::CHAIR_STOP)]
    ChairStop,
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
    /// Resume preparation of an admitted local attempt in a separate process.
    Launch {
        attempt: AttemptId,
        /// Seconds to wait when every candidate provider is updating.
        #[arg(long)]
        provider_lease_wait: Option<u64>,
    },
    Settle {
        /// Private durable-settlement resume request from the host.
        request: String,
    },
    ProviderUpdates,
    TickGc,
    /// Collect a terminal lane after settlement publishes its cleanup intent.
    LaneCleanup {
        request: String,
    },
    /// Detached cargo-seed refresh queued by the daily collector.
    TickSeed,
    /// Take a host build slot, then run the given cargo command (the lane's cargo shim). The slot directory, count and wait come from the host's environment (`PROVENANT_BUILD_SLOT_STATE`, `PROVENANT_BUILD_SLOTS`, `PROVENANT_BUILD_WAIT_SECS`), never from arguments.
    BuildSlot {
        /// The real cargo, then its arguments.
        #[arg(last = true, required = true, allow_hyphen_values = true)]
        command: Vec<OsString>,
    },
    Supervise {
        /// Private S2 handoff values, validated and adopted only by supervisor.
        #[arg(hide = true, num_args = 5..=6, catalog_u64_range = 3..=2147483647)]
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
pub(crate) fn reason(value: &str) -> Result<String, &'static str> {
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
/// A positive span (seconds, s, m, h or d) ago, a date or an RFC3339 timestamp.
pub(crate) fn since(value: &str) -> Result<jiff::Timestamp, String> {
    if value
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_digit())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b's' | b'm' | b'h' | b'd'))
    {
        let seconds = i64::try_from(
            span_seconds(value).ok_or("expected a positive span (N, Ns, Nm, Nh or Nd)")?,
        )
        .map_err(|_| "expected a positive span (N, Ns, Nm, Nh or Nd)".to_owned())?;
        if seconds <= 0 {
            return Err("span must be positive".into());
        }
        use provenant_core::Clock;
        return provenant_os::clock::SystemClock::new()
            .wall_now()
            .map_err(|error| error.to_string())?
            .as_timestamp()
            .checked_sub(jiff::SignedDuration::from_secs(seconds))
            .map_err(|error| error.to_string());
    }
    timestamp(value).map_err(str::to_owned)
}

#[cfg(test)]
mod target_tests {
    use super::since;

    #[test]
    fn since_accepts_spans() {
        use provenant_core::Clock;
        let clock = provenant_os::clock::SystemClock::new();
        let before = clock.wall_now().unwrap().as_timestamp();
        let start = since("86400").unwrap();
        let after = clock.wall_now().unwrap().as_timestamp();
        let duration = jiff::SignedDuration::from_secs(86_400);
        assert!(start >= before.checked_sub(duration).unwrap());
        assert!(start <= after.checked_sub(duration).unwrap());
        for (value, seconds) in [
            ("24h", 86_400),
            ("5s", 5),
            ("1m", 60),
            ("2d", 172_800),
            ("7d", 604_800),
        ] {
            let before = clock.wall_now().unwrap().as_timestamp();
            let result = since(value).unwrap();
            let after = clock.wall_now().unwrap().as_timestamp();
            let offset = jiff::SignedDuration::from_secs(seconds);
            assert!(result >= before.checked_sub(offset).unwrap(), "{value}");
            assert!(result <= after.checked_sub(offset).unwrap(), "{value}");
        }
        for invalid in ["0", "-1", ""] {
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
}

fn explicit_operation_id(value: &str) -> Result<OperationId, String> {
    OperationId::parse(value).map_err(|error| error.to_string())
}

fn span_seconds(value: &str) -> Option<u64> {
    let (digits, multiplier) = match value.as_bytes().last() {
        Some(b's') => (&value[..value.len() - 1], 1),
        Some(b'm') => (&value[..value.len() - 1], 60),
        Some(b'h') => (&value[..value.len() - 1], 3600),
        Some(b'd') => (&value[..value.len() - 1], 86400),
        _ => (value, 1),
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u64>().ok()?.checked_mul(multiplier)
}
pub(crate) fn duration_seconds(value: &str) -> Result<u64, &'static str> {
    span_seconds(value)
        .filter(|n| (1..=provenant_core::MAX_LANE_RUNTIME_SECONDS).contains(n))
        .ok_or("duration must be 1s to 4h; run `provenant help` for examples")
}
#[derive(Clone, Debug, Args)]
pub struct Status {
    /// Stream a bounded attempt inventory page for the session collector,
    /// paged with --all, --limit and --cursor like the graph listing.
    #[arg(long, hide = true, conflicts_with = "targets")]
    pub session_stream: bool,
    /// Graphs, nodes or attempts to show (NAME[/NODE[#N]]); every graph when omitted
    #[arg(value_name = "TARGET")]
    pub targets: Vec<Target>,
    /// Include every settled graph, paged with --limit and --cursor
    #[arg(long)]
    pub all: bool,
    /// Most rows per page; unsettled graphs are always shown
    #[arg(long, default_value = "20", catalog_u64_range = 1..)]
    pub limit: u64,
    /// Continue from a previous page's cursor
    #[arg(long)]
    pub cursor: Option<u64>,
}
/// `wait`: block until graph work or pull requests settle.
#[derive(Clone, Debug, Args)]
pub struct Wait {
    /// Graphs, nodes or attempts (NAME[/NODE[#N]]); this project's running
    /// graphs when neither a target nor --pr is given
    #[arg(value_name = "TARGET")]
    pub targets: Vec<Target>,
    /// Pull request whose required checks, merge or close ends the wait (repeatable)
    #[arg(long = "pr", value_name = "NUMBER", catalog_u64_range = 1..)]
    pub prs: Vec<u64>,
    /// Return at the first target to settle or need input
    #[arg(long)]
    pub any: bool,
    /// Print one line per transition until the wait ends
    #[arg(long)]
    pub follow: bool,
    /// Wait budget (default domains.wait.timeout_seconds; at most 4 hours)
    #[arg(long, value_name = "SPAN", catalog_duration())]
    pub timeout: Option<u64>,
}
/// `trace`: a graph's revisions and attempt timeline.
#[derive(Debug, Args)]
pub struct GraphTrace {
    /// Graph, node or attempt, as NAME[/NODE[#N]]
    #[arg(value_name = "TARGET")]
    pub target: Target,
}

fn parse_landing_tier(value: &str) -> Result<String, String> {
    serde_json::from_value::<provenant_records::workflow::RiskTier>(serde_json::Value::String(
        value.into(),
    ))
    .map(|_| value.to_owned())
    .map_err(|_| "expected routine, substantial, crucial or terminal".into())
}

#[derive(Debug, Args)]
pub struct Edit {
    /// Graph name
    #[arg(value_name = "NAME", value_parser = edit_graph_name)]
    pub name: String,
    /// Patch document (JSON or TOML)
    #[arg(long)]
    pub file: PathBuf,
}
#[derive(Debug, Args)]
pub struct Templates {
    /// Template to show; omit to list effective templates
    pub name: Option<String>,
    /// Document containing inline templates (JSON or TOML)
    #[arg(long)]
    pub file: Option<PathBuf>,
}
/// A bare graph name: a target with no node.
fn edit_graph_name(value: &str) -> Result<String, String> {
    let target = Target::parse(value).map_err(|error| error.to_string())?;
    if target.node.is_some() {
        return Err(
            "edit takes a graph NAME, not a node; patch nodes inside the --file document".into(),
        );
    }
    Ok(target.name)
}
