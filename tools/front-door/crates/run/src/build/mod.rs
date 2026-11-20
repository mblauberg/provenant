//! Shared build-strategy runtime seam.
//!
//! Strategies and their persisted rows live here so lane admission, verification
//! and landing can use one dispatch point and one outcome ledger.

pub mod command;
pub mod context;
pub mod hygiene;
pub mod lane;
pub mod ledger;
pub mod local;
pub mod pathmap;
pub mod paths;
pub mod queue;
pub mod report;
pub mod rust;
pub mod slots;
pub mod snapshot;
pub mod store;

pub use context::BuildContext;
use provenant_config::{BuildPort, PortSource};
use provenant_core::CancelToken;
use provenant_core::consumer_env;
use provenant_supervisor::census::{ProcessObservation, SystemProcessObserver};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};

/// A parsed setting together with its declaration layer and diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved<T> {
    pub value: T,
    pub source: PortSource,
    pub warnings: Vec<String>,
}

/// Strategies available in this slice. `wrapper` deliberately resolves to
/// lane_seed until the separate wrapper strategy is implemented.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyName {
    Slots,
    LaneSeed,
    Lane,
    Command,
}

/// The resolved strategy and frozen configuration used for one job.
#[derive(Clone, Debug, PartialEq)]
pub struct Strategy {
    pub name: StrategyName,
    pub source: PortSource,
    pub port: BuildPort,
}

/// Preparation done by the lane writer before starting its provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LanePreparation {
    pub seed_target: bool,
}

impl Strategy {
    /// Whether launch should seed Cargo's target directory for this strategy.
    pub fn lane_preparation(&self) -> LanePreparation {
        LanePreparation {
            seed_target: matches!(self.name, StrategyName::LaneSeed | StrategyName::Slots),
        }
    }
}

/// Resolve a configured strategy without refusing a job for an unknown value.
pub fn strategy(port: &BuildPort) -> Resolved<Strategy> {
    use provenant_config::BuildStrategyName as ConfigStrategy;
    let resolved = port.strategy_name();
    let mut source = resolved.source;
    let mut warnings = resolved.warnings;
    let name = match resolved.value {
        ConfigStrategy::Slots => StrategyName::Slots,
        ConfigStrategy::LaneSeed => StrategyName::LaneSeed,
        ConfigStrategy::Lane => StrategyName::Lane,
        ConfigStrategy::Command => StrategyName::Command,
        ConfigStrategy::Wrapper => {
            warnings.push(format!(
                "BUILD_STRATEGY_UNAVAILABLE: build.strategy=wrapper (source: {}); using lane_seed",
                source_name(source)
            ));
            StrategyName::LaneSeed
        }
    };
    if !warnings.is_empty() && port.strategy_name().source == PortSource::Builtin {
        source = PortSource::Builtin;
    }
    Resolved {
        value: Strategy {
            name,
            source,
            port: port.clone(),
        },
        source,
        warnings,
    }
}

fn source_name(source: PortSource) -> &'static str {
    match source {
        PortSource::Builtin => "default",
        PortSource::Detected => "detected",
        PortSource::Host => "host",
        PortSource::HostProject => "host_project",
        PortSource::Project => "project",
    }
}

/// Queue precedence, ordered from most to least urgent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Interactive,
    Test,
    Train,
    Prewarm,
}

/// The origin that owns the job outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Lane,
    Chair,
    Landing,
    Maintenance,
}

/// The kind of work being run through the build runtime.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Intercepted { stack: Stack, subcommand: String },
    Tier { name: String },
    Gate { name: String },
    Maintenance { canary: bool },
}

/// Build stack supported by this cut-over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stack {
    Rust,
}

/// Where a job's source tree comes from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotSource {
    /// A checkout and its uncommitted and untracked non-ignored changes.
    WorkingTree { worktree: PathBuf, git: GitBinding },
    /// An exact commit in the project repository.
    Commit {
        repo_common_dir: PathBuf,
        commit: String,
    },
}

/// The real Git executable and the object/ref view used for a tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitBinding {
    pub git: PathBuf,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
    pub object_dir: Option<PathBuf>,
    pub index_file: PathBuf,
    pub config_global: Option<PathBuf>,
}

/// One command step in a job; `cwd_rel` is relative to the execution root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub cwd_rel: PathBuf,
    pub argv: Vec<OsString>,
}

/// Whether output is inherited or retained in bounded buffers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    Stream,
    Capture { limit_bytes: usize },
}

/// A resolved remote or local base used by verification and snapshots.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseRef {
    pub remote_ref: String,
    pub commit: String,
}

/// The root and optional commit/slot exposed to a job prelude.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecSite {
    pub root: PathBuf,
    pub head: Option<String>,
    pub slot: Option<u64>,
}

/// A single command job, including the caller's original environment.
pub struct Job {
    pub id: String,
    pub project_key: String,
    pub project: PathBuf,
    pub origin: Origin,
    pub owner: String,
    pub kind: JobKind,
    pub priority: Priority,
    pub source: SnapshotSource,
    pub verify_base: Option<BaseRef>,
    pub steps: Vec<Step>,
    pub stop_on_failure: bool,
    pub env: Vec<(OsString, OsString)>,
    pub output: Output,
    pub timeout: Option<Duration>,
    pub sandboxed: bool,
    pub prelude: Option<JobPrelude>,
}

/// Job hook run after materialisation and before the first step.
pub type JobPrelude =
    Box<dyn FnOnce(&ExecSite) -> Result<Vec<(OsString, OsString)>, BuildError> + Send>;

/// Build runtime failures that callers can turn into a local fallback or a
/// stable command exit.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("cancelled")]
    Cancelled,
    #[error("cancelled by signal {0}")]
    CancelledSignal(i32),
    #[error("superseded by a newer identical build request")]
    Superseded,
    #[error("build pool unavailable: {0}")]
    Unavailable(String),
    #[error("infrastructure failure ({kind:?}): {detail}")]
    Infra { kind: InfraKind, detail: String },
    #[error("build configuration: {0}")]
    Config(String),
    #[error("unsupported here: {0}")]
    Unsupported(String),
}

/// Infrastructure failure category recorded by the ledger.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InfraKind {
    Disk,
    Git,
    CrashSignal,
    SlotLayout,
    Clock,
}

/// Why a job ran locally while the slots strategy was selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Exception {
    Sandboxed,
    UnsupportedArgv,
    OffloadOff,
    PoolUnavailable,
    Overflow,
    AllQuarantined,
    StrategyUnavailable,
    NoSnapshot,
}

/// Reset applied after a job.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reset {
    None,
    Light,
    Full,
    Quarantine,
}

/// Process completion encoded as exactly `{"code":n}` or `{"signal":n}`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExitStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
}

impl ExitStatus {
    pub fn code(code: i32) -> Self {
        Self {
            code: Some(code),
            signal: None,
        }
    }

    pub fn signal(signal: i32) -> Self {
        Self {
            code: None,
            signal: Some(signal),
        }
    }

    pub fn process_code(self) -> i32 {
        self.code.unwrap_or_else(|| 128 + self.signal.unwrap_or(0))
    }
}

/// Slot-vs-local placement recorded in each job outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecPlace {
    Slot,
    Local,
}

/// Bounded stdout and stderr retained by Capture mode.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Captured {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub stdout_total_bytes: u64,
    pub stderr_total_bytes: u64,
}

/// Slot-local changes collected after a command; S1 supplies the census.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotChanges {
    pub count: u64,
    pub sample: Vec<String>,
}

/// One row of `state/build/jobs.jsonl`. Field order is a persisted-format pin.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JobOutcome {
    pub schema: u32,
    pub job_id: String,
    pub project_key: String,
    pub owner: String,
    pub origin: Origin,
    pub kind: JobKind,
    pub priority: Priority,
    pub strategy: StrategyName,
    pub exec: ExecPlace,
    pub exception: Option<Exception>,
    pub slot: Option<u64>,
    pub snapshot: Option<String>,
    pub prev_snapshot: Option<String>,
    pub base: Option<String>,
    pub enqueued_unix_ms: u64,
    pub started_unix_ms: u64,
    pub finished_unix_ms: u64,
    pub waited_ms: u64,
    pub wall_ms: u64,
    pub exit: Option<ExitStatus>,
    pub timed_out: bool,
    pub cancelled: bool,
    pub reset: Reset,
    pub infra: Option<InfraKind>,
    pub units_fresh: Option<u64>,
    pub units_rebuilt: Option<u64>,
    pub rebuilt_packages: Vec<String>,
    pub peak_rss_bytes: Option<u64>,
    pub slot_changes: Option<SlotChanges>,
    pub warnings: Vec<String>,
}

/// State and host information frozen for one runtime invocation.
pub struct RunContext {
    pub state: PathBuf,
    pub home: PathBuf,
    pub strategy: Strategy,
    pub cancel: CancelToken,
}

/// Result returned to an admit, verify or landing caller.
#[derive(Clone, Debug, PartialEq)]
pub struct JobResult {
    pub outcome: JobOutcome,
    pub step_exits: Vec<Option<ExitStatus>>,
    pub captured: Option<Captured>,
}

/// Internal dispatch failure with its persisted outcome retained for the ledger.
pub(crate) type DispatchFailure = (BuildError, Box<JobOutcome>);

/// The single dispatch point for build jobs.
pub fn run(job: Job, ctx: &RunContext) -> Result<JobResult, BuildError> {
    let started = ledger::now_unix_ms();
    let enqueued = started;
    let base = outcome_seed(&job, ctx, enqueued);
    let scratch = env_path(&job.env, "PROVENANT_ADMISSION_WAIT_DIR");
    let result = dispatch(job, ctx, base, started);
    let (result, outcome) = match result {
        Ok(result) => {
            let outcome = result.outcome.clone();
            (Ok(result), outcome)
        }
        Err((error, outcome)) => {
            let mut outcome = *outcome;
            if matches!(error, BuildError::Cancelled) {
                outcome.cancelled = true;
            }
            (Err(error), outcome)
        }
    };
    ledger::append(&outcome, &ctx.state, scratch.as_deref());
    result
}

fn dispatch(
    mut job: Job,
    ctx: &RunContext,
    mut outcome: JobOutcome,
    started: u64,
) -> Result<JobResult, DispatchFailure> {
    outcome.strategy = ctx.strategy.name;
    outcome.base = job.verify_base.as_ref().map(|base| base.commit.clone());
    outcome.enqueued_unix_ms = started;
    outcome.started_unix_ms = ledger::now_unix_ms();

    for warning in strategy(&ctx.strategy.port).warnings {
        warn_and_record(&mut outcome, &warning);
    }
    let offload = ctx.strategy.port.offload_policy();
    for warning in offload.warnings {
        warn_and_record(&mut outcome, &warning);
    }
    let overflow = ctx.strategy.port.overflow_policy();
    for warning in overflow.warnings {
        warn_and_record(&mut outcome, &warning);
    }
    let aging = ctx.strategy.port.queue_aging_duration();
    for warning in aging.warnings {
        warn_and_record(&mut outcome, &warning);
    }
    let nested = env_value(&job.env, "PROVENANT_BUILD_NESTED") == Some("1");
    let strategy = ctx.strategy.name;
    if nested {
        return local::execute(&mut job, ctx, outcome, ExecPlace::Local, None, 0);
    }

    if matches!(
        strategy,
        StrategyName::Lane | StrategyName::LaneSeed | StrategyName::Slots | StrategyName::Command
    ) {
        if strategy == StrategyName::Slots && job.sandboxed {
            warn_and_record(
                &mut outcome,
                "build slots unavailable in a sandboxed lane; building locally",
            );
            outcome.exception = Some(Exception::Sandboxed);
            clear_admission_marker(&mut job);
            return local::execute(&mut job, ctx, outcome, ExecPlace::Local, None, 0);
        }
        if strategy == StrategyName::Slots && offload.value == provenant_config::BuildOffload::Off {
            outcome.exception = Some(Exception::OffloadOff);
            clear_admission_marker(&mut job);
            return local::execute(&mut job, ctx, outcome, ExecPlace::Local, None, 0);
        }

        if strategy == StrategyName::Slots {
            if let rust::RustClass::Local { exception } = rust::classify(&job) {
                outcome.exception = Some(exception);
                warn_and_record(
                    &mut outcome,
                    &format!(
                        "Rust command is not eligible for build slots ({exception:?}); running locally"
                    ),
                );
                clear_admission_marker(&mut job);
                return local::execute(&mut job, ctx, outcome, ExecPlace::Local, None, 0);
            }
            let (priority, warnings) = effective_job_priority(&job, &ctx.strategy.port);
            job.priority = priority;
            outcome.priority = priority;
            for warning in warnings {
                warn_and_record(&mut outcome, &warning);
            }
        }
        let request = queue::request_for_job(&job, ctx);
        if strategy == StrategyName::Slots
            && request.limit > 0
            && request.excluded.len() as u64 == request.limit
        {
            clear_admission_marker(&mut job);
            outcome.exception = Some(Exception::AllQuarantined);
            warn_and_record(
                &mut outcome,
                "all build slots are quarantined; run provenant build reset; building locally",
            );
            return local::execute(&mut job, ctx, outcome, ExecPlace::Local, None, 0);
        }
        if request.limit == 0 {
            clear_admission_marker(&mut job);
            return execute_local_strategy(&mut job, ctx, outcome, 0);
        }
        match queue::admit(&request) {
            Ok(queue::Admitted::Held {
                guard,
                index,
                waited,
            }) => {
                let _held_marker = queue::mark_held(&job, ctx, index);
                if let Some(marker) = process_marker() {
                    set_admission_marker(&mut job, &marker);
                } else {
                    clear_admission_marker(&mut job);
                    warn_and_record(
                        &mut outcome,
                        "could not identify build holder process; nested build calls may not reuse its slot",
                    );
                }
                outcome.waited_ms = u64::try_from(waited.as_millis()).unwrap_or(u64::MAX);
                if strategy == StrategyName::Slots {
                    outcome.slot = Some(index);
                    let prelude = job.prelude.take();
                    let execution = if let Some(prelude) = prelude {
                        match slots::stage(&job, ctx, index) {
                            Ok(mut staged) => match prelude(&staged.site) {
                                Ok(extra) => {
                                    staged.add_env(extra);
                                    slots::execute_staged(&job, ctx, index, staged)
                                }
                                Err(error) => {
                                    outcome.infra = Some(InfraKind::SlotLayout);
                                    outcome.finished_unix_ms = ledger::now_unix_ms();
                                    return Err((error, Box::new(outcome)));
                                }
                            },
                            Err(stage_failure) => {
                                job.prelude = Some(prelude);
                                outcome.exception = Some(stage_failure.exception);
                                outcome.infra = stage_failure.infra.or(outcome.infra);
                                if stage_failure.reset != Reset::None {
                                    outcome.reset = stage_failure.reset;
                                }
                                outcome.slot = None;
                                warn_and_record(
                                    &mut outcome,
                                    &format!(
                                        "build slot staging failed before spawn: {}",
                                        stage_failure.detail
                                    ),
                                );
                                Err(stage_failure.exception)
                            }
                        }
                    } else {
                        slots::execute(&job, ctx, index)
                    };
                    match execution {
                        Ok(execution) => {
                            drop(guard);
                            drop(_held_marker);
                            return slots::finish_slot_execution(execution, outcome);
                        }
                        Err(exception) => {
                            outcome.exception = Some(exception);
                            outcome.slot = None;
                            warn_and_record(
                                &mut outcome,
                                &format!(
                                    "build slot unavailable, building without one: {exception:?}"
                                ),
                            );
                        }
                    }
                }
                outcome.slot = None;
                let waited = outcome.waited_ms;
                return execute_local_strategy(&mut job, ctx, outcome, waited);
            }
            Ok(queue::Admitted::Overflow { waited }) => {
                clear_admission_marker(&mut job);
                outcome.waited_ms = u64::try_from(waited.as_millis()).unwrap_or(u64::MAX);
                outcome.exception = Some(Exception::Overflow);
                let warning = "bounded wait ended with every build slot busy; building without one. Cargo manages its own build locks";
                warn_and_record(&mut outcome, warning);
                return execute_local_strategy(
                    &mut job,
                    ctx,
                    outcome,
                    waited.as_millis().min(u128::from(u64::MAX)) as u64,
                );
            }
            Err(BuildError::Cancelled) => {
                outcome.cancelled = true;
                outcome.finished_unix_ms = ledger::now_unix_ms();
                return Err((BuildError::Cancelled, Box::new(outcome)));
            }
            Err(BuildError::CancelledSignal(signal)) => {
                outcome.cancelled = true;
                outcome.finished_unix_ms = ledger::now_unix_ms();
                return Err((BuildError::CancelledSignal(signal), Box::new(outcome)));
            }
            Err(BuildError::Superseded) => {
                outcome.finished_unix_ms = ledger::now_unix_ms();
                return Err((BuildError::Superseded, Box::new(outcome)));
            }
            Err(error) => {
                clear_admission_marker(&mut job);
                outcome.exception = Some(Exception::PoolUnavailable);
                warn_and_record(
                    &mut outcome,
                    &format!("build slot unavailable, building without one: {error}"),
                );
                return execute_local_strategy(&mut job, ctx, outcome, 0);
            }
        }
    }

    local::execute(&mut job, ctx, outcome, ExecPlace::Local, None, 0)
}

fn execute_local_strategy(
    job: &mut Job,
    ctx: &RunContext,
    outcome: JobOutcome,
    waited: u64,
) -> Result<JobResult, DispatchFailure> {
    if ctx.strategy.name == StrategyName::Command {
        command::execute(job, ctx, outcome)
    } else {
        local::execute(job, ctx, outcome, ExecPlace::Local, None, waited)
    }
}

fn effective_job_priority(job: &Job, port: &BuildPort) -> (Priority, Vec<String>) {
    let class = match &job.kind {
        JobKind::Intercepted { subcommand, .. } if subcommand == "check" => "check",
        JobKind::Intercepted { subcommand, .. } if subcommand == "clippy" => "clippy",
        JobKind::Intercepted { subcommand, .. } if subcommand == "test" => "test",
        JobKind::Intercepted { subcommand, .. } if subcommand == "nextest" => "nextest",
        JobKind::Tier { .. } => "tier",
        JobKind::Gate { .. } => "gate",
        JobKind::Maintenance { .. } => "maintenance",
        _ => "other",
    };
    let resolved = port.priority_for(class);
    let priority = match resolved.value {
        provenant_config::BuildPriority::Interactive => Priority::Interactive,
        provenant_config::BuildPriority::Test => Priority::Test,
        provenant_config::BuildPriority::Train => Priority::Train,
        provenant_config::BuildPriority::Prewarm => Priority::Prewarm,
    };
    (priority, resolved.warnings)
}

pub(crate) fn outcome_seed(job: &Job, ctx: &RunContext, now: u64) -> JobOutcome {
    JobOutcome {
        schema: 1,
        job_id: job.id.clone(),
        project_key: job.project_key.clone(),
        owner: job.owner.clone(),
        origin: job.origin,
        kind: job.kind.clone(),
        priority: job.priority,
        strategy: ctx.strategy.name,
        exec: ExecPlace::Local,
        exception: None,
        slot: None,
        snapshot: None,
        prev_snapshot: None,
        base: None,
        enqueued_unix_ms: now,
        started_unix_ms: now,
        finished_unix_ms: now,
        waited_ms: 0,
        wall_ms: 0,
        exit: None,
        timed_out: false,
        cancelled: false,
        reset: Reset::None,
        infra: None,
        units_fresh: None,
        units_rebuilt: None,
        rebuilt_packages: Vec::new(),
        peak_rss_bytes: None,
        slot_changes: None,
        warnings: Vec::new(),
    }
}

fn warn_and_record(outcome: &mut JobOutcome, warning: &str) {
    outcome.warnings.push(warning.to_owned());
}

fn env_value<'a>(env: &'a [(OsString, OsString)], name: &str) -> Option<&'a str> {
    env.iter()
        .find(|(key, _)| key == name)
        .and_then(|(_, value)| value.to_str())
}

fn env_path(env: &[(OsString, OsString)], name: &str) -> Option<PathBuf> {
    env.iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| PathBuf::from(value))
}

fn process_marker() -> Option<String> {
    let pid = i64::from(std::process::id());
    match SystemProcessObserver.observe(pid) {
        ProcessObservation::Present(fact) if !fact.zombie => {
            Some(format!("{}:{}", pid, fact.identity.native_start))
        }
        _ => None,
    }
}

fn set_admission_marker(job: &mut Job, marker: &str) {
    let upper = OsString::from("PROVENANT_ADMISSION_SLOT_BUILD");
    let lower = OsString::from("PROVENANT_ADMISSION_SLOT_build");
    let slot = OsString::from(consumer_env::ADMISSION_SLOT);
    let class = OsString::from(consumer_env::ADMISSION_CLASS);
    job.env
        .retain(|(key, _)| key != &upper && key != &lower && key != &slot && key != &class);
    job.env.push((upper, OsString::from(marker)));
    job.env.push((lower, OsString::from(marker)));
    job.env
        .push((slot, OsString::from(format!("build:{marker}"))));
    job.env.push((class, OsString::from("build")));
}

fn clear_admission_marker(job: &mut Job) {
    job.env.retain(|(key, value)| match key.to_str() {
        Some("PROVENANT_ADMISSION_SLOT_BUILD" | "PROVENANT_ADMISSION_SLOT_build") => false,
        Some(consumer_env::ADMISSION_SLOT) => !value.to_string_lossy().starts_with("build:"),
        Some(consumer_env::ADMISSION_CLASS) => value.to_str() != Some("build"),
        _ => true,
    });
}

/// Apply a setting to caller-visible environment without adding shims.
pub(crate) fn set_env(
    env: &mut Vec<(OsString, OsString)>,
    key: impl Into<OsString>,
    value: impl Into<OsString>,
) {
    let key = key.into();
    env.retain(|(existing, _)| existing != &key);
    env.push((key, value.into()));
}

/// Project root, also useful to the command and queue adapters.
pub(crate) fn job_root(job: &Job) -> &Path {
    match &job.source {
        SnapshotSource::WorkingTree { worktree, .. } => worktree,
        SnapshotSource::Commit { .. } => job
            .env
            .iter()
            .find(|(key, _)| key == "PROVENANT_BUILD_LOCAL_ROOT")
            .map(|(_, value)| Path::new(value))
            .unwrap_or(&job.project),
    }
}
