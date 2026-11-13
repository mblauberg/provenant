//! CLI-owned grammar with mounted owner subtrees.
use crate::metadata::CatalogueArg;
use clap::{Args, Parser, Subcommand, ValueEnum, builder::TypedValueParser};
use provenant_core::{GitOid, OperationId, RunId, TaskId};
pub use provenant_records::Harness;
use std::{ffi::OsString, path::PathBuf, str::FromStr};

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
    #[command(add = crate::metadata::CoreCommand(0, "provenant run 'Meet the acceptance criteria' --use task"))]
    Run(Box<GraphRunArgs>),
    /// Show graph, node and attempt status
    #[command(add = crate::metadata::CoreCommand(1, "provenant status"))]
    Status(Status),
    /// Report graph attempt efficiency
    #[command(hide = true)]
    Stats(GraphStats),
    /// Merge on green CI and prune the merged worktree
    #[command(add = crate::metadata::CoreCommand(8, "provenant land 1947 --wait"))]
    Land {
        /// Pull request numbers in landing order
        #[arg(catalog_u64_range = 1..)]
        numbers: Vec<u64>,
        /// Use batch landing even for one pull request
        #[arg(long)]
        batch: bool,
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
    /// Read graph, node or attempt output
    #[command(add = crate::metadata::CoreCommand(2, "provenant output ID"))]
    Output(Output),
    /// Show a graph's recorded revisions and attempt timeline
    Trace {
        /// Graph identifier
        graph: String,
    },
    /// Stop a graph, node or attempt subtree
    #[command(name = "stop", add = crate::metadata::CoreCommand(4, "provenant stop ID"))]
    Stop(Cancel),
    /// Retry a graph, node or attempt
    #[command(name = "retry", add = crate::metadata::CoreCommand(5, "provenant retry ID"))]
    Retry(Resume),
    /// Queue a steering message for a lane when its provider supports control
    #[command(add = crate::metadata::CoreCommand(3, "provenant steer ID --text-stdin < message.md"))]
    Steer(crate::inbox::Steer),
    /// Apply a patch to unstarted graph nodes
    #[command(add = crate::metadata::CoreCommand(6, "provenant edit ID --file patch.json"))]
    Edit(Edit),
    /// List or show templates from all layers
    #[command(add = crate::metadata::CoreCommand(7, "provenant templates"))]
    Templates(Templates),
    /// Deliver a settled writer attempt
    #[command(subcommand)]
    Lane(Lane),
    /// Settle open runs and report uncertain custody
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
    /// Wait for CI and report pass or fail with the pull request head
    Watch {
        #[arg(catalog_u64_range = 1..)]
        number: u64,
    },
    /// Open a pull request with the repository template filled from receipts
    Open(Box<PrOpen>),
    /// Print one line per pull request: state, head, checks, merge and review
    Status {
        /// Pull requests to report; this account's open ones when omitted
        #[arg(catalog_u64_range = 1..)]
        numbers: Vec<u64>,
        /// Block until the snapshot changes (default 5 minutes; maximum 4 hours)
        #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "5m", catalog_duration())]
        wait: Option<u64>,
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
    /// Rewrite retired routing timing keys to `schedule` and remove retired
    /// fabric limits in place after a timestamped backup.
    Migrate {
        /// Report what would change without writing
        #[arg(long)]
        dry_run: bool,
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
    /// Place attempts on this peer name or host ID
    #[arg(long)]
    pub host: Option<String>,
    /// Use this exact adapter/model instead of a route tier; a bare model name works when one provider offers it
    #[arg(long, conflicts_with = "tier")]
    pub model: Option<String>,
    /// Override model reasoning effort
    #[arg(long)]
    pub effort: Option<String>,
    /// Account label for routing and quota accounting
    #[arg(long)]
    pub account: Option<String>,
    /// Stable operation identity for replay
    #[arg(long, hide = true, value_parser = explicit_operation_id)]
    pub operation_id: Option<OperationId>,
    /// Brief for the selected template
    pub brief: Option<String>,
    /// Select a layered template [default: task]
    #[arg(long = "use")]
    pub template: Option<String>,
    /// Override the template's route tier
    #[arg(long, visible_alias = "route")]
    pub tier: Option<String>,
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
    /// Bind a template parameter or attempt.FIELD (NAME=TOML_VALUE); repeatable
    #[arg(long = "set")]
    pub set: Vec<String>,
    /// Read a graph document (JSON or TOML), or a plain text brief
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
    /// Wait for settlement (default 1 hour; use --wait=30m for a duration).
    /// Writer binding can block beyond this wait: each binding gets at least
    /// domains.fabric.graph.writer_launch_seconds (default 55s). Expiry leaves work running.
    #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "1h", catalog_duration())]
    pub wait: Option<u64>,
    /// Raw CLI evidence that a duration after bare `--wait` became the brief.
    #[arg(skip)]
    pub bare_wait_value: Option<String>,
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

    /// A duration consumed as the positional brief after a bare `--wait`.
    pub fn ambiguous_wait_brief(&self) -> Option<&str> {
        self.bare_wait_value.as_deref()
    }
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
        // Graph and node names are resolved by the graph owner before lane IDs.
        if parts.len() <= 3
            && parts.iter().all(|part| {
                !part.is_empty()
                    && part.len() <= 200
                    && part.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
                    })
            })
            && parts.iter().any(|part| {
                part.len() < 8
                    || part.len() > 32
                    || part.bytes().any(|byte| byte.is_ascii_uppercase())
                    || !part.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        {
            return Ok(Self {
                selection: Some(text.to_owned()),
                run: None,
                task: None,
                attempt: None,
            });
        }
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
                selection: Some(text.to_owned()),
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
    /// Stream a bounded page for the SessionStart collector.
    #[arg(long, hide = true)]
    pub session_stream: bool,
    /// Show this attempt ID or RUN[/TASK[/ATTEMPT]] (repeatable)
    #[arg(long)]
    pub id: Vec<Target>,
    /// List, wait on or watch a configured peer's own lanes, by label or host ID
    #[arg(long, conflicts_with_all = ["id", "cursor"])]
    pub host: Option<String>,
    /// Keep lanes submitted by this session (its agent label)
    #[arg(long, value_parser = session_label, conflicts_with = "all")]
    pub session: Option<String>,
    /// Show every lane, newest first
    #[arg(long)]
    pub all: bool,
    /// Maximum lanes per page (1 to 100)
    #[arg(long, default_value = "50", catalog_u64_range = 1..=100)]
    pub limit: u64,
    /// Continue from a previous page's cursor
    #[arg(long, conflicts_with_all = ["wait", "watch"])]
    pub cursor: Option<String>,
    /// Wait for settlement (seconds, or a duration such as 30m). An oversized
    /// unseen default backlog is summarised with its count and newest 100 lanes.
    #[arg(long, conflicts_with = "watch", catalog_duration())]
    pub wait: Option<u64>,
    /// Wait for any or all lanes to settle
    #[arg(
        long,
        default_value = "any",
        catalog_requires = "wait",
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
        catalog_duration()
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
    /// Reviewer or writer provenance for a settled attempt
    Receipt,
    /// The provider's own transcript, tailed in place
    Transcript,
}
#[derive(Debug, Clone, Args)]
pub struct Output {
    /// Attempt ID, RUN/ATTEMPT, HOST/ATTEMPT or RUN/TASK/ATTEMPT; a RUN or RUN/TASK reads its newest settled attempt, and receipt needs exactly one
    pub attempt: Attempt,
    /// Read a configured peer's own attempt, by label or host ID
    #[arg(long)]
    pub host: Option<String>,
    /// Which part to read
    #[arg(long, value_enum, default_value = "result")]
    pub part: Part,
    /// Write the selected range to FILE, replacing it, instead of printing it
    #[arg(long, value_name = "FILE", conflicts_with_all = ["path", "trace"])]
    pub to: Option<PathBuf>,
    /// Print the attempt's artifact path for this part instead of its content
    #[arg(long, conflicts_with_all = ["to", "trace", "offset", "tail", "max_bytes"])]
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
    #[arg(long, value_parser = reason, catalog_bounds = (0, 256))]
    pub reason: Option<String>,
    /// Wait up to this many seconds for cancellation to settle
    #[arg(long, catalog_duration())]
    pub wait: Option<u64>,
}
#[derive(Debug, Args)]
#[group(id = "resume_prompt", multiple = false)]
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
    /// Retry only the named node rather than recovering its graph
    #[arg(long, conflicts_with = "no_fallback")]
    pub node: Option<String>,
    /// Replace the target brief
    #[arg(long, conflicts_with_all = ["prompt_file", "prompt_stdin"])]
    pub brief: Option<String>,
    /// Raise a graph limit or budget (KEY=VALUE); may be repeated
    #[arg(long, value_name = "KEY=VALUE")]
    pub extend: Vec<String>,
    /// Graph ID, node ID, GRAPH/NODE, attempt ID, HOST/ATTEMPT, RUN/ATTEMPT or RUN/TASK/ATTEMPT
    #[arg(value_name = "TARGET")]
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
    #[arg(long, catalog_duration())]
    pub wait: Option<u64>,
}
#[derive(Debug, Args)]
pub struct Reconcile {
    /// Reconcile only this run (repeatable)
    #[arg(long)]
    pub id: Vec<Target>,
    /// Abandon settlement of this writer attempt, preserving its evidence
    #[arg(long, catalog_requires = "reason", conflicts_with = "id")]
    pub abandon: Option<Attempt>,
    /// Why the chair is abandoning settlement
    #[arg(long, catalog_requires = "abandon", catalog_bounds = (0, 256))]
    pub reason: Option<String>,
}
#[derive(Debug, Subcommand)]
pub enum Lane {
    /// Re-run host delivery, requiring the settled receipt's clean, exact head
    Deliver {
        attempt: Attempt,
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
    /// Restrict the trace to one run.
    #[arg(long)]
    pub run: Option<Target>,
    /// Also read the attempts of runs placed on peers, one SSH status read per peer
    #[arg(long)]
    pub peers: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum StatsBy {
    Route,
    Pr,
    Issue,
    Graph,
    Node,
    Pattern,
    Preset,
    Role,
}
#[derive(Debug, Args)]
pub struct Stats {
    /// Include offline post-merge outcomes and stratified ranking
    #[arg(long, conflicts_with_all = ["usage", "by"])]
    pub outcomes: bool,
    /// Include advisory expiring suggestions from offline pipeline evidence
    #[arg(long, conflicts_with_all = ["usage", "by"])]
    pub suggest: bool,
    #[command(flatten)]
    pub pipeline: PipelineOptions,
    /// Count local CLI verbs and MCP tools, including unused installed units.
    #[arg(long)]
    pub usage: bool,
    /// Count from this date or timestamp, or a span such as 1d ago
    #[arg(long, value_parser = since)]
    pub since: Option<jiff::Timestamp>,
    /// Count up to this date or timestamp
    #[arg(long, value_parser = timestamp)]
    pub until: Option<jiff::Timestamp>,
    /// Restrict usage and cost to one run.
    #[arg(long)]
    pub run_id: Option<Target>,
    /// Group supervised attempts by route, work context or merged pull request; attempts with no value form an `unattributed` group
    #[arg(long, value_enum, conflicts_with = "usage")]
    pub by: Option<StatsBy>,
    /// Include per-attempt detail
    #[arg(long, conflicts_with = "usage")]
    pub detail: bool,
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
    /// Resume preparation of an admitted local run in a separate process.
    Launch {
        run: RunId,
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
    use super::{Attempt, Target, since};
    use provenant_core::{AttemptId, RunId, TaskId};

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
        // A graph lane ID names its graph and node, which need not be hexadecimal.
        let lane: Attempt = format!("g/parent--child/{attempt}").parse().unwrap();
        assert_eq!(
            lane.0.selection.as_deref(),
            Some(format!("g/parent--child/{attempt}").as_str())
        );
        for invalid in [
            "",
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
pub(crate) fn status_wait_seconds(value: &str) -> Result<u64, &'static str> {
    // Zero is the parser's sentinel for the configured default, never a duration.
    if value == "configured" {
        Ok(0)
    } else {
        duration_seconds(value)
    }
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
#[derive(Debug, Args)]
pub struct GraphStats {
    /// Graph ID, node or attempt selector
    pub id: Option<String>,
    /// Group efficiency by node, requested tier or template
    #[arg(long, value_parser = ["node", "tier", "template"], default_value = "template")]
    pub by: String,
}

#[derive(Clone, Debug, Args)]
pub struct Status {
    /// Stream a bounded attempt inventory page for the session collector.
    #[arg(long, hide = true, conflicts_with_all = ["stats", "wait", "id", "target", "follow"])]
    pub session_stream: bool,
    /// Report attempt efficiency without counting composite aggregates
    #[arg(long, conflicts_with = "wait")]
    pub stats: bool,
    /// Group efficiency by node, requested tier or template
    #[arg(long, requires = "stats", value_parser = ["node", "tier", "template"], default_value = "template")]
    pub by: String,
    /// Wait for mixed targets (no value uses domains.wait.timeout_seconds)
    #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "configured", catalog_status_wait(), conflicts_with_all = ["all", "cursor"])]
    pub wait: Option<u64>,
    /// Observe lane:ID, graph:ID, operation:ID, pr:NUMBER or land:NUMBER (repeatable)
    #[arg(long)]
    pub target: Vec<String>,
    /// Return when all targets are terminal (use `any` for the first transition)
    #[arg(long, catalog_requires = "wait", value_enum, default_value = "all")]
    pub until: Condition,
    /// Stream transitions until all targets are terminal or the wait expires
    #[arg(long, catalog_requires = "wait")]
    pub follow: bool,
    /// Show a run, task, attempt or graph ID, or operation:ID for a refusal
    pub id: Option<String>,
    /// Maximum full records per page
    #[arg(long, hide = true, default_value = "50", catalog_u64_range = 1..=100)]
    pub limit: u64,
    /// Continue a full-record page from its cursor
    #[arg(long, hide = true)]
    pub cursor: Option<u64>,
    /// Include full records and older lanes
    #[arg(long)]
    pub all: bool,
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
    /// Graph ID
    pub id: String,
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
