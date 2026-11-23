//! One sourced, versioned declaration for repository integration ports.
use provenant_records::FileDigest;
use provenant_records::warning::{CONFIG_MALFORMED, Warning, warn};
pub use provenant_records::{
    CommandExitCodes, DEFAULT_PORT_COMMAND_RECEIPT_AGE_DAYS, ExitDisposition, Layer,
    PORT_PRECEDENCE, PortCommand, PortCommandBackend, PortCommandDisposition, PortCommandReceipt,
    PortReceiptLifetime, port_layer_rank,
};
use provenant_records::{Source, Sourced};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs, io,
    io::Read,
    path::{Component, Path, PathBuf},
    time::Duration,
};

pub const PORTS_CONFIG: &str = ".provenant/ports.json";
pub const PORTS_SCHEMA_VERSION: u16 = 3;

mod evidence;
mod forge;
mod request;
pub use evidence::{
    PortCommandPublication, PortCommandReceiptCollectionLock, PortCommandReceiptEntry,
    PortCommandReceiptInventory, PortCommandReceiptProof, PortEvidenceError, PortReceiptIssue,
    port_command_receipt_proof, port_command_receipts, record_port_command,
    remove_port_command_receipt, try_lock_port_command_receipts,
};
pub use forge::{
    Adapter, Forge, ForgeOperation, ForgePort, ForgeResponse, HeadersObservation, PortError,
    PortExecution, REST_API_VERSION_HEADER, dispatch_port_backend, land_backend,
};
pub use request::Request;
const MAX_PORTS_BYTES: u64 = 65_536;
const BRANCH_TOKEN: &str = "<branch>";

/// Hash a repository's canonical common Git directory into a stable project
/// key. Linked checkouts share that directory and therefore share this key.
/// This is the single definition used by the host-project configuration layer.
pub fn project_key(common_git_dir: &Path) -> String {
    provenant_core::hex(&Sha256::digest(
        common_git_dir.as_os_str().as_encoded_bytes(),
    ))
}

/// Derive the project key from a checkout directory alone, for callers that
/// hold a path rather than a verified repository handle. The read-only marker
/// resolver parses the `.git` file and its `commondir`, then canonicalises the
/// result so the digest matches one computed from a verified handle.
pub fn project_key_for(project: &Path) -> Option<String> {
    let marker = provenant_os::git::repository_marker(project)?;
    let common_dir = marker.common_dir?;
    let canonical = fs::canonicalize(common_dir).ok()?;
    Some(project_key(&canonical))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortBackendKind {
    Builtin,
    Command,
    None,
}

/// Typed result from executing a repository command port. The bounded process
/// capture is retained as evidence; callers decide whether to render it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortCommandExecution {
    pub command_digest: FileDigest,
    pub disposition: PortCommandDisposition,
    pub outcome: Option<provenant_os::exec::ExecOutcome>,
}

impl PortCommandExecution {
    /// Represent a command that could not be started while retaining the exact
    /// declaration whose execution was attempted.
    pub fn failed(command: &PortCommand) -> Self {
        Self {
            command_digest: command.digest(),
            disposition: PortCommandDisposition::Failed,
            outcome: None,
        }
    }
}

// Project declarations do not grant access to ambient credential variables.
const COMMAND_CONTEXT_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "TMPDIR",
    "TMP",
    "TEMP",
    "LANG",
    "LANGUAGE",
    "TZ",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    "LC_COLLATE",
    "LC_TIME",
    "LC_NUMERIC",
    "LC_MONETARY",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
];

pub fn port_command_environment(
    ambient: impl IntoIterator<Item = (OsString, OsString)>,
    explicit: &[(OsString, OsString)],
) -> Vec<(OsString, OsString)> {
    let mut environment: Vec<_> = ambient
        .into_iter()
        .filter(|(name, _)| {
            name.to_str()
                .is_some_and(|name| COMMAND_CONTEXT_ENV.contains(&name))
        })
        .collect();
    for (name, value) in explicit {
        environment.retain(|(existing, _)| existing != name);
        environment.push((name.clone(), value.clone()));
    }
    environment
}

/// Run a declared argv port with process-group cleanup and bounded output.
/// Additional arguments and environment values are appended by the owning
/// consumer; configured argv is never interpreted by a shell.
pub fn execute_port_command(
    executor: &dyn provenant_os::exec::Executor,
    command: &PortCommand,
    cwd: &Path,
    arguments: &[OsString],
    environment: &[(OsString, OsString)],
    timeout: Duration,
    cancel: &provenant_core::CancelToken,
) -> PortCommandExecution {
    execute_port_command_with_input(
        executor,
        command,
        cwd,
        arguments,
        environment,
        None,
        timeout,
        cancel,
    )
}

/// Execute a declared command with optional operation input, preserving the same
/// filtered environment, declared exit outcomes and bounded capture.
#[allow(
    clippy::too_many_arguments,
    reason = "command execution carries one bounded operation context"
)]
pub fn execute_port_command_with_input(
    executor: &dyn provenant_os::exec::Executor,
    command: &PortCommand,
    cwd: &Path,
    arguments: &[OsString],
    environment: &[(OsString, OsString)],
    input: Option<&[u8]>,
    timeout: Duration,
    cancel: &provenant_core::CancelToken,
) -> PortCommandExecution {
    use provenant_os::exec::{ExecOptions, StreamPolicy, Termination};
    let command_digest = command.digest();
    if !command.is_valid() {
        return PortCommandExecution {
            command_digest,
            disposition: PortCommandDisposition::Failed,
            outcome: None,
        };
    }
    let mut argv: Vec<_> = command.argv.iter().map(OsString::from).collect();
    argv.extend_from_slice(arguments);
    let ambient = COMMAND_CONTEXT_ENV
        .iter()
        .filter_map(|name| std::env::var_os(*name).map(|value| (OsString::from(*name), value)));
    let env = port_command_environment(ambient, environment);
    let mut options = ExecOptions::new(&argv, cwd, timeout);
    options.stdin = input;
    options.env = Some(&env);
    options.stdout = StreamPolicy::Capture {
        limit: 8 * 1024 * 1024,
    };
    options.stderr = StreamPolicy::Tail { limit: 64 * 1024 };
    options.process_group = true;
    options.cleanup_process_group = true;
    options.group_watchdog = true;
    options.cancel = Some(cancel);
    let Ok(outcome) = executor.execute(options) else {
        return PortCommandExecution {
            command_digest,
            disposition: PortCommandDisposition::Failed,
            outcome: None,
        };
    };
    let disposition = if outcome.timed_out {
        PortCommandDisposition::TimedOut
    } else if outcome.cancelled {
        PortCommandDisposition::Cancelled
    } else {
        match outcome.termination {
            Termination::Exit(code)
                if i32::try_from(code)
                    .ok()
                    .is_some_and(|code| command.exit_codes.success.contains(&code)) =>
            {
                PortCommandDisposition::Succeeded
            }
            Termination::Exit(code)
                if i32::try_from(code)
                    .ok()
                    .is_some_and(|code| command.exit_codes.unknown.contains(&code)) =>
            {
                PortCommandDisposition::Unknown
            }
            Termination::Exit(_) if command.exit_codes.other == ExitDisposition::Unknown => {
                PortCommandDisposition::Unknown
            }
            Termination::Exit(_) => PortCommandDisposition::Refused,
            Termination::Signal(_) => PortCommandDisposition::Failed,
        }
    };
    PortCommandExecution {
        command_digest,
        disposition,
        outcome: Some(outcome),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PortBackend {
    Builtin,
    Command {
        command: PortCommand,
    },
    #[serde(rename = "none")]
    None,
}

impl PortBackend {
    fn kind(&self) -> PortBackendKind {
        match self {
            Self::Builtin => PortBackendKind::Builtin,
            Self::Command { .. } => PortBackendKind::Command,
            Self::None => PortBackendKind::None,
        }
    }

    fn valid(&self) -> bool {
        match self {
            Self::Command { command } => command.is_valid(),
            Self::Builtin | Self::None => true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Port {
    pub source: Layer,
    pub backend: PortBackend,
}

/// The one declaration of the Git context every forge operation shares.
///
/// An empty `remote` or `protected` means "detect": [`crate::forge::ForgeContext`]
/// resolves the value from the repository when no layer declared it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitPort {
    pub source: Layer,
    pub backend: PortBackend,
    /// The remote name. Empty means detect it from the branch upstream or the
    /// sole remote.
    pub remote: String,
    /// Branch names that must never be rewritten or retired. Empty means the
    /// default branch plus the forge-reported protected branches.
    pub protected: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VerifyPort {
    pub source: Layer,
    pub backend: PortBackend,
    pub base: String,
    pub workspace: PathBuf,
    pub remote: String,
    pub load_per_cpu: f64,
    pub test_threads: Option<u32>,
    pub package_paths: Vec<PackagePath>,
    pub quarantine_command: Vec<String>,
    /// Tier names are repository-defined strings, not a closed numeric enum.
    pub tiers: BTreeMap<String, VerifyTier>,
    pub default_tier: String,
    pub integrations: Vec<Integration>,
    pub runners: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub affected: VerifyAffectedPort,
    /// Cheap repository checks a writer runs before pushing. They need no
    /// build, so a lane may run them even though it otherwise only formats.
    #[serde(default)]
    pub static_checks: Vec<VerifyStaticCheck>,
}

/// One no-build check declared under `verify.static_checks` of a port layer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyStaticCheck {
    pub argv: Vec<String>,
    #[serde(default = "default_static_check_cwd")]
    pub cwd: std::path::PathBuf,
    #[serde(default = "default_static_check_timeout")]
    pub timeout_seconds: u64,
}

fn default_static_check_cwd() -> std::path::PathBuf {
    std::path::PathBuf::from(".")
}

fn default_static_check_timeout() -> u64 {
    60
}

impl VerifyStaticCheck {
    /// A declaration is usable when it has a command and a positive time cap.
    pub fn is_valid(&self) -> bool {
        self.argv.first().is_some_and(|program| !program.is_empty())
            && self.timeout_seconds > 0
            && crate::ports::relative(&self.cwd, true)
    }
}

/// Configuration for skipping verification tiers whose inputs are unaffected.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyAffectedPort {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub global_paths: Vec<String>,
    #[serde(default = "default_verify_affected_full_every")]
    pub full_every: u64,
    #[serde(default = "default_verify_affected_full_after")]
    pub full_after: String,
}

fn default_verify_affected_full_every() -> u64 {
    10
}

fn default_verify_affected_full_after() -> String {
    "24h".to_owned()
}

impl Default for VerifyAffectedPort {
    fn default() -> Self {
        Self {
            enabled: false,
            global_paths: Vec::new(),
            full_every: default_verify_affected_full_every(),
            full_after: default_verify_affected_full_after(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifySelection {
    Packages,
    Integrations,
    All,
}

/// A command run from the effective verify workspace joined with `cwd`.
/// The whole argv elements `{packages}`, `{integration_args}`, and
/// `{quarantine_args}` expand to zero or more arguments; no shell splitting occurs.
/// A plain string is shorthand for the table form: it is split into argv
/// elements with shell-word rules, leaving `stack` and `cwd` at their defaults.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TierCommand {
    #[serde(default)]
    pub stack: String,
    #[serde(default = "tier_command_cwd")]
    pub cwd: PathBuf,
    #[serde(default)]
    pub argv: Vec<String>,
}

fn tier_command_cwd() -> PathBuf {
    PathBuf::from(".")
}

/// Split a declared command string into argv elements as a shell would tokenise
/// it, without running one. Quotes group an element and a backslash escapes the
/// next character; an unterminated quote or trailing escape is malformed.
fn split_tier_command_words(command: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut started = false;
    for ch in command.chars() {
        if escaped {
            word.push(ch);
            escaped = false;
        } else if let Some(delimiter) = quote {
            if delimiter == '"' && ch == '\\' {
                // Inside double quotes a backslash escapes the next character,
                // as it does outside quotes; single quotes take it literally.
                escaped = true;
                started = true;
            } else if ch == delimiter {
                quote = None;
            } else {
                word.push(ch);
            }
        } else {
            match ch {
                '\\' => {
                    escaped = true;
                    started = true;
                }
                '\'' | '"' => {
                    quote = Some(ch);
                    started = true;
                }
                ch if ch.is_whitespace() => {
                    if started {
                        words.push(std::mem::take(&mut word));
                        started = false;
                    }
                }
                ch => {
                    word.push(ch);
                    started = true;
                }
            }
        }
    }
    if quote.is_some() || escaped {
        return None;
    }
    if started {
        words.push(word);
    }
    Some(words)
}

#[derive(Deserialize)]
struct TierCommandTable {
    #[serde(default)]
    stack: String,
    #[serde(default = "tier_command_cwd")]
    cwd: PathBuf,
    #[serde(default)]
    argv: Vec<String>,
}

struct TierCommandVisitor;

impl<'de> serde::de::Visitor<'de> for TierCommandVisitor {
    type Value = TierCommand;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a command string or a tier command table")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        let argv = split_tier_command_words(value)
            .ok_or_else(|| E::custom("command string has an unbalanced quote or escape"))?;
        Ok(TierCommand {
            stack: String::new(),
            cwd: tier_command_cwd(),
            argv,
        })
    }

    fn visit_map<M>(self, map: M) -> Result<Self::Value, M::Error>
    where
        M: serde::de::MapAccess<'de>,
    {
        let table =
            TierCommandTable::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
        Ok(TierCommand {
            stack: table.stack,
            cwd: table.cwd,
            argv: table.argv,
        })
    }
}

impl<'de> Deserialize<'de> for TierCommand {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(TierCommandVisitor)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyTier {
    pub commands: Vec<TierCommand>,
    pub selection: VerifySelection,
    pub warn_for_host_load: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackagePath {
    pub paths: Vec<String>,
    pub package: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Integration {
    pub paths: Vec<String>,
    pub package: String,
    pub binary: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LandBackend {
    Builtin,
    /// The records owner declares the command and unknown exit code. Exit 0
    /// means landed; other nonzero exits refuse unless the declared unknown code.
    Command {
        command: provenant_records::landing::RepositoryCommand,
    },
    #[serde(rename = "none")]
    None,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandPort {
    pub source: Layer,
    pub backend: LandBackend,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeLayoutPort {
    pub source: Layer,
    pub backend: PortBackend,
    pub roots: Vec<PathBuf>,
    pub default: PathBuf,
}

impl WorktreeLayoutPort {
    pub fn unavailable_reason(&self) -> Option<&'static str> {
        port_unavailable_reason(self)
    }

    /// Resolve the declared default path for a validated Git branch.
    pub fn path_for_branch(&self, branch: &str) -> io::Result<PathBuf> {
        if let Some(reason) = self.unavailable_reason() {
            return Err(io::Error::new(io::ErrorKind::Unsupported, reason));
        }
        if branch.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "worktree layout is unavailable",
            ));
        }
        let template = self.default.to_string_lossy();
        let rendered = template.replace(BRANCH_TOKEN, &branch.replace('/', "-"));
        let branch_component = template
            .split('/')
            .position(|component| component.contains(BRANCH_TOKEN))
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid worktree template")
            })?;
        if rendered.len() > 4096
            || rendered.split('/').enumerate().any(|(index, component)| {
                component.len() > if index == branch_component { 240 } else { 255 }
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "worktree layout path is too long",
            ));
        }
        Ok(PathBuf::from(rendered))
    }

    /// Return the branch slug matched by the declared default template under
    /// any configured root. Secondary roots reuse that same template tail.
    pub fn branch_component<'a>(&self, relative: &'a Path) -> Option<&'a str> {
        if !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
            || !matches!(self.backend, PortBackend::Builtin)
        {
            return None;
        }
        let template_root = self
            .roots
            .iter()
            .filter(|root| self.default.starts_with(root))
            .max_by_key(|root| root.components().count())?;
        let template = self.default.strip_prefix(template_root).ok()?.to_str()?;
        let (prefix, suffix) = template.split_once(BRANCH_TOKEN)?;
        self.roots.iter().find_map(|root| {
            let actual = relative.strip_prefix(root).ok()?.to_str()?;
            let branch = actual.strip_prefix(prefix)?.strip_suffix(suffix)?;
            (!branch.is_empty() && !branch.contains('/')).then_some(branch)
        })
    }

    pub fn contains_worktree(&self, relative: &Path) -> bool {
        self.branch_component(relative).is_some()
    }
}

fn default_build_strategy() -> String {
    "slots".to_owned()
}
fn default_build_offload() -> String {
    "auto".to_owned()
}
fn default_build_overflow() -> String {
    "auto".to_owned()
}
fn default_build_clean() -> String {
    "untracked".to_owned()
}
fn default_build_snapshot_retention() -> String {
    "7d".to_owned()
}

fn default_build_queue_aging() -> String {
    "120s".to_owned()
}

fn default_build_canary_every() -> String {
    "24h".to_owned()
}

fn default_build_slot_idle() -> String {
    "7d".to_owned()
}

fn default_build_pre_warm_budget() -> u64 {
    1
}

fn default_build_priority() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("check".to_owned(), "interactive".to_owned()),
        ("clippy".to_owned(), "interactive".to_owned()),
        ("test".to_owned(), "test".to_owned()),
        ("nextest".to_owned(), "test".to_owned()),
        ("tier".to_owned(), "test".to_owned()),
        ("gate".to_owned(), "train".to_owned()),
        ("maintenance".to_owned(), "prewarm".to_owned()),
        ("other".to_owned(), "interactive".to_owned()),
    ])
}

fn default_build_soak() -> BuildSoakPort {
    BuildSoakPort::default()
}

/// The build port declares how the chair's host-owned warm check runs. Only
/// the shape is declared here: a consumer repository may override any leaf in
/// a lower layer and higher layers override them independently. Lanes never
/// compile, so this port has no lane strategy, wrapper or seed leaves.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BuildPort {
    pub source: Layer,
    #[serde(default)]
    pub prepare: Vec<Vec<String>>,
    #[serde(default = "default_build_strategy")]
    pub strategy: String,
    #[serde(default = "default_build_offload")]
    pub offload: String,
    #[serde(default = "default_build_overflow")]
    pub overflow: String,
    #[serde(default = "default_build_clean")]
    pub clean: String,
    #[serde(default)]
    pub stacks: Vec<String>,
    #[serde(default = "default_build_snapshot_retention")]
    pub snapshot_retention: String,
    #[serde(default = "default_build_priority")]
    pub priority: BTreeMap<String, String>,
    #[serde(default = "default_build_queue_aging")]
    pub queue_aging: String,
    #[serde(default = "default_build_pre_warm_budget")]
    pub prewarm_budget: u64,
    #[serde(default = "default_build_canary_every")]
    pub canary_every: String,
    #[serde(default = "default_build_slot_idle")]
    pub slot_idle: String,
    #[serde(default = "default_build_soak")]
    pub soak: BuildSoakPort,
    /// Computed per-leaf source labels, populated from the port layer walk.
    /// This is frozen into lane build contexts and is overwritten when a
    /// configuration layer is loaded, so declarations cannot claim a source.
    #[serde(default)]
    pub leaf_sources: BTreeMap<String, Layer>,
}

/// Soak thresholds used by the build report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BuildSoakPort {
    #[serde(default = "default_soak_min_runs")]
    pub min_runs: u64,
    #[serde(default = "default_soak_min_days")]
    pub min_days: u64,
    #[serde(default = "default_soak_interactive_wait_p95")]
    pub interactive_wait_p95: String,
    #[serde(default = "default_soak_test_wait_p95")]
    pub test_wait_p95: String,
    #[serde(default = "default_soak_rss_fraction")]
    pub rss_fraction: f64,
    #[serde(default = "default_soak_local_exception_fraction")]
    pub local_exception_fraction: f64,
    #[serde(default = "default_soak_churn_clean_fraction")]
    pub churn_clean_fraction: f64,
}

fn default_soak_min_runs() -> u64 {
    50
}
fn default_soak_min_days() -> u64 {
    7
}
fn default_soak_interactive_wait_p95() -> String {
    "60s".to_owned()
}
fn default_soak_test_wait_p95() -> String {
    "300s".to_owned()
}
fn default_soak_rss_fraction() -> f64 {
    0.75
}
fn default_soak_local_exception_fraction() -> f64 {
    0.05
}
fn default_soak_churn_clean_fraction() -> f64 {
    0.99
}

impl Default for BuildSoakPort {
    fn default() -> Self {
        Self {
            min_runs: default_soak_min_runs(),
            min_days: default_soak_min_days(),
            interactive_wait_p95: default_soak_interactive_wait_p95(),
            test_wait_p95: default_soak_test_wait_p95(),
            rss_fraction: default_soak_rss_fraction(),
            local_exception_fraction: default_soak_local_exception_fraction(),
            churn_clean_fraction: default_soak_churn_clean_fraction(),
        }
    }
}

/// A typed value resolved from a build leaf. Invalid values fall back to the
/// builtin value, with a warning that names both the leaf and its source.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedBuildValue<T> {
    pub value: T,
    pub source: Layer,
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildStrategyName {
    Slots,
}
/// The chair's one warm-check strategy. Build strategies are not adapters:
/// this table is the one place their names are spelled.
const BUILD_STRATEGY_NAMES: [(&str, BuildStrategyName); 1] = [("slots", BuildStrategyName::Slots)];
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildOverflow {
    Auto,
    Queue,
    Local,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildClean {
    None,
    Untracked,
    Ignored,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildOffload {
    Auto,
    Off,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum BuildPriority {
    Interactive,
    Test,
    Train,
    Prewarm,
}

impl BuildPort {
    /// Return the exact source for a dotted build leaf. The field-level source
    /// remains useful for callers that have only an older bare BuildPort.
    pub fn source_for_leaf(&self, leaf: &str) -> Layer {
        let full_path = if leaf.starts_with("build.") {
            leaf.to_owned()
        } else {
            format!("build.{leaf}")
        };
        self.leaf_sources
            .get(&full_path)
            .copied()
            .unwrap_or(self.source)
    }

    fn invalid<T>(&self, leaf: &str, raw: &str, default: T, reason: &str) -> ResolvedBuildValue<T> {
        let configured_source = self.source_for_leaf(leaf);
        ResolvedBuildValue {
            value: default,
            source: Layer::Default,
            warnings: vec![format!(
                "invalid build.{leaf} value {raw:?} from {}; using builtin value (default): {reason}",
                port_source_layer(configured_source)
            )],
        }
    }

    fn string_value<T: Copy>(
        &self,
        leaf: &str,
        raw: &str,
        default: T,
        parse: impl FnOnce(&str) -> Option<T>,
    ) -> ResolvedBuildValue<T> {
        match parse(raw) {
            Some(value) => ResolvedBuildValue {
                value,
                source: self.source_for_leaf(leaf),
                warnings: Vec::new(),
            },
            None => self.invalid(leaf, raw, default, "unknown value"),
        }
    }

    /// Resolve `build.strategy`, warning and falling back to the chair's
    /// `slots` strategy for unknown names.
    pub fn strategy_name(&self) -> ResolvedBuildValue<BuildStrategyName> {
        self.string_value(
            "strategy",
            &self.strategy,
            BuildStrategyName::Slots,
            |value| {
                BUILD_STRATEGY_NAMES
                    .iter()
                    .find(|(name, _)| *name == value)
                    .map(|(_, strategy)| *strategy)
            },
        )
    }

    /// Resolve `build.overflow`, warning and falling back to `auto`.
    pub fn overflow_policy(&self) -> ResolvedBuildValue<BuildOverflow> {
        self.string_value(
            "overflow",
            &self.overflow,
            BuildOverflow::Auto,
            |value| match value {
                "auto" => Some(BuildOverflow::Auto),
                "queue" => Some(BuildOverflow::Queue),
                "local" => Some(BuildOverflow::Local),
                _ => None,
            },
        )
    }

    /// Resolve `build.clean`, warning and falling back to `untracked`.
    pub fn clean_policy(&self) -> ResolvedBuildValue<BuildClean> {
        self.string_value(
            "clean",
            &self.clean,
            BuildClean::Untracked,
            |value| match value {
                "none" => Some(BuildClean::None),
                "untracked" => Some(BuildClean::Untracked),
                "ignored" => Some(BuildClean::Ignored),
                _ => None,
            },
        )
    }

    /// Resolve `build.offload`, warning and falling back to `auto`.
    pub fn offload_policy(&self) -> ResolvedBuildValue<BuildOffload> {
        self.string_value(
            "offload",
            &self.offload,
            BuildOffload::Auto,
            |value| match value {
                "auto" => Some(BuildOffload::Auto),
                "off" => Some(BuildOffload::Off),
                _ => None,
            },
        )
    }

    /// Resolve the configured queue priority for a class. Missing classes use
    /// their builtin level; malformed levels warn and use that builtin.
    pub fn priority_for(&self, class: &str) -> ResolvedBuildValue<BuildPriority> {
        let default = match class {
            "test" | "nextest" | "tier" => BuildPriority::Test,
            "gate" => BuildPriority::Train,
            "maintenance" => BuildPriority::Prewarm,
            _ => BuildPriority::Interactive,
        };
        let Some(raw) = self.priority.get(class) else {
            return ResolvedBuildValue {
                value: default,
                source: Layer::Default,
                warnings: Vec::new(),
            };
        };
        self.string_value(
            &format!("priority.{class}"),
            raw,
            default,
            |value| match value {
                "interactive" => Some(BuildPriority::Interactive),
                "test" => Some(BuildPriority::Test),
                "train" => Some(BuildPriority::Train),
                "prewarm" => Some(BuildPriority::Prewarm),
                _ => None,
            },
        )
    }

    fn duration(&self, leaf: &str, raw: &str, default: &str) -> ResolvedBuildValue<Duration> {
        match crate::gc::parse_gc_idle_duration(raw) {
            Ok(seconds) => ResolvedBuildValue {
                value: Duration::from_secs(seconds),
                source: self.source_for_leaf(leaf),
                warnings: Vec::new(),
            },
            Err(reason) => {
                let fallback = crate::gc::parse_gc_idle_duration(default)
                    .map(Duration::from_secs)
                    .unwrap_or_default();
                self.invalid(leaf, raw, fallback, reason)
            }
        }
    }

    pub fn snapshot_retention_duration(&self) -> ResolvedBuildValue<Duration> {
        self.duration("snapshot_retention", &self.snapshot_retention, "7d")
    }

    pub fn queue_aging_duration(&self) -> ResolvedBuildValue<Duration> {
        self.duration("queue_aging", &self.queue_aging, "120s")
    }

    pub fn canary_every_duration(&self) -> ResolvedBuildValue<Duration> {
        self.duration("canary_every", &self.canary_every, "24h")
    }

    pub fn slot_idle_duration(&self) -> ResolvedBuildValue<Duration> {
        self.duration("slot_idle", &self.slot_idle, "7d")
    }

    pub fn soak_interactive_wait_p95_duration(&self) -> ResolvedBuildValue<Duration> {
        self.duration(
            "soak.interactive_wait_p95",
            &self.soak.interactive_wait_p95,
            "60s",
        )
    }

    pub fn soak_test_wait_p95_duration(&self) -> ResolvedBuildValue<Duration> {
        self.duration("soak.test_wait_p95", &self.soak.test_wait_p95, "300s")
    }

    fn sync_leaf_sources(&mut self, provenance: &BTreeMap<String, Layer>) {
        let leaves = [
            "strategy",
            "prepare",
            "offload",
            "overflow",
            "clean",
            "stacks",
            "snapshot_retention",
            "priority.check",
            "priority.clippy",
            "priority.test",
            "priority.nextest",
            "priority.tier",
            "priority.gate",
            "priority.maintenance",
            "priority.other",
            "queue_aging",
            "prewarm_budget",
            "canary_every",
            "slot_idle",
            "soak.min_runs",
            "soak.min_days",
            "soak.interactive_wait_p95",
            "soak.test_wait_p95",
            "soak.rss_fraction",
            "soak.local_exception_fraction",
            "soak.churn_clean_fraction",
        ];
        self.leaf_sources = leaves
            .into_iter()
            .map(|leaf| {
                let path = format!("build.{leaf}");
                let source = provenance.get(&path).copied().unwrap_or(Layer::Default);
                (path, source)
            })
            .collect();
    }
}

impl PortEntry for BuildPort {
    fn source(&self) -> Layer {
        self.source
    }
    fn set_source(&mut self, source: Layer) {
        self.source = source;
    }
    fn backend_kind(&self) -> PortBackendKind {
        PortBackendKind::Builtin
    }
    fn valid(&self) -> bool {
        let declared = |argv: &[String]| argv.first().is_some_and(|program| !program.is_empty());
        self.prepare.iter().all(|argv| declared(argv))
    }
}

trait PortEntry {
    fn source(&self) -> Layer;
    fn set_source(&mut self, source: Layer);
    fn backend_kind(&self) -> PortBackendKind;
    fn valid(&self) -> bool;
    fn backend_unavailable_reason(&self) -> Option<&'static str> {
        (self.backend_kind() == PortBackendKind::None)
            .then_some("the effective declaration disables this port")
    }
}

fn port_unavailable_reason(port: &impl PortEntry) -> Option<&'static str> {
    if !port.valid() {
        Some("the port declaration is invalid")
    } else {
        port.backend_unavailable_reason()
    }
}

impl PortEntry for Port {
    fn source(&self) -> Layer {
        self.source
    }
    fn set_source(&mut self, source: Layer) {
        self.source = source;
    }
    fn backend_kind(&self) -> PortBackendKind {
        self.backend.kind()
    }
    fn valid(&self) -> bool {
        self.backend.valid()
    }
}
impl PortEntry for GitPort {
    fn source(&self) -> Layer {
        self.source
    }
    fn set_source(&mut self, source: Layer) {
        self.source = source;
    }
    fn backend_kind(&self) -> PortBackendKind {
        self.backend.kind()
    }
    fn valid(&self) -> bool {
        self.backend.valid()
            && (self.remote.is_empty()
                || (relative(Path::new(&self.remote), false)
                    && !self.remote.contains(BRANCH_TOKEN)))
            && self
                .protected
                .iter()
                .all(|branch| !branch.is_empty() && !branch.contains(BRANCH_TOKEN))
    }
}

impl PortEntry for VerifyPort {
    fn source(&self) -> Layer {
        self.source
    }
    fn set_source(&mut self, source: Layer) {
        self.source = source;
    }
    fn backend_kind(&self) -> PortBackendKind {
        self.backend.kind()
    }
    fn valid(&self) -> bool {
        self.backend.valid()
            && relative(&self.workspace, true)
            // An empty base means "infer from the remote HEAD"; the consumer
            // that needs a commit resolves it before reading that ref.
            && !self.remote.is_empty()
            && self.load_per_cpu.is_finite()
            && self.load_per_cpu > 0.0
            && self.test_threads.is_none_or(|threads| threads > 0)
            && (self.tiers.is_empty() || self.tiers.contains_key(&self.default_tier))
            && self.tiers.iter().all(|(name, tier)| !name.is_empty() && !tier.commands.is_empty())
            && self.static_checks.iter().all(VerifyStaticCheck::is_valid)
    }
}
impl PortEntry for LandPort {
    fn source(&self) -> Layer {
        self.source
    }
    fn set_source(&mut self, source: Layer) {
        self.source = source;
    }
    fn backend_kind(&self) -> PortBackendKind {
        match &self.backend {
            LandBackend::Builtin => PortBackendKind::Builtin,
            LandBackend::Command { .. } => PortBackendKind::Command,
            LandBackend::None => PortBackendKind::None,
        }
    }
    fn valid(&self) -> bool {
        match &self.backend {
            LandBackend::Command { command } => command.valid(),
            LandBackend::Builtin | LandBackend::None => true,
        }
    }
}
impl PortEntry for WorktreeLayoutPort {
    fn backend_unavailable_reason(&self) -> Option<&'static str> {
        match self.backend {
            PortBackend::Builtin => None,
            PortBackend::Command { .. } => {
                Some("worktree layout command backend is not implemented")
            }
            PortBackend::None => Some("the effective declaration disables this port"),
        }
    }
    fn source(&self) -> Layer {
        self.source
    }
    fn set_source(&mut self, source: Layer) {
        self.source = source;
    }
    fn backend_kind(&self) -> PortBackendKind {
        self.backend.kind()
    }
    fn valid(&self) -> bool {
        self.backend.valid()
            && !self.roots.is_empty()
            && self
                .roots
                .iter()
                .all(|root| relative(root, false) && !root.to_string_lossy().contains(BRANCH_TOKEN))
            && self
                .roots
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                == self.roots.len()
            && relative(&self.default, false)
            && self.default.to_string_lossy().matches(BRANCH_TOKEN).count() == 1
            && self.roots.iter().any(|root| self.default.starts_with(root))
    }
}

fn relative(path: &Path, allow_current: bool) -> bool {
    !path.as_os_str().is_empty()
        && path.components().all(|component| {
            matches!(component, Component::Normal(_))
                || (allow_current && component == Component::CurDir)
        })
}

macro_rules! declare_ports {
    ($($field:ident: $kind:ty),+ $(,)?) => {
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        pub struct PortTable {
            pub schema_version: u16,
            $(pub $field: $kind),+,
            /// Highest layer that set each leaf. Derived from the same walk
            /// that labels ports, excluded from every serialised layer.
            #[serde(skip)]
            provenance: BTreeMap<String, Layer>,
            /// Every layer that declared each leaf, lowest first, with its
            /// exact origin. The last candidate is the effective winner and
            /// the rest are shadowed; this is the per-leaf source an
            /// explanation prints. Derived state, never read from a file.
            #[serde(skip)]
            sourced_leaves: BTreeMap<String, Vec<Sourced<serde_json::Value>>>,
        }

        impl PortTable {
            pub const PORT_NAMES: &'static [&'static str] = &[$(stringify!($field)),+];

            fn set_sources(&mut self, source: Layer) {
                $(self.$field.set_source(source);)+
            }

            /// Derive each port's label from the highest layer that declared
            /// any of its leaves, so the label and `provenance` cannot disagree.
            fn sync_sources_from_provenance(&mut self) {
                $({
                    let name = stringify!($field);
                    let mut highest = Layer::Default;
                    for (path, source) in &self.provenance {
                        if (path == name || path.starts_with(&format!("{name}.")))
                            && port_layer_rank(*source) > port_layer_rank(highest)
                        {
                            highest = *source;
                        }
                    }
                    self.$field.set_source(highest);
                })+
                self.build.sync_leaf_sources(&self.provenance);
            }

            pub fn entries(&self) -> impl Iterator<Item = (&'static str, Layer, PortBackendKind, bool, Option<&'static str>)> + '_ {
                [$(
                    (stringify!($field), self.$field.source(), self.$field.backend_kind(), self.$field.valid(), port_unavailable_reason(&self.$field)),
                )+].into_iter()
            }
        }
    }
}

declare_ports! {
    forge: Port,
    git: GitPort,
    verify: VerifyPort,
    land: LandPort,
    ci_observation: Port,
    worktree_layout: WorktreeLayoutPort,
    naming: Port,
    post_create: Port,
    tracker: Port,
    release: Port,
    admission: Port,
    build: BuildPort,
}

/// One choice `provenant init` made for a project's port table, with the exact
/// place it came from. The source names detection's origin (the remote, the
/// remote HEAD, the stack scan), not the project file it will be written to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InitChoice {
    /// Dotted port leaf this choice declares, such as `git.remote`.
    pub key: &'static str,
    /// The detected value, ready to place in a project overlay.
    pub value: serde_json::Value,
    /// Where the resolver got the value.
    pub source: Source,
}

/// The minimal project declaration and the choices behind it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct InitPlan {
    /// The `.provenant/ports.json` document. Stack detection already re-derives
    /// `verify.tiers` and `verify.default_tier` on every read, so those leaves
    /// are left to it; the document holds only choices a later read cannot
    /// reconstruct on its own.
    pub document: serde_json::Value,
    /// Every choice, in declaration order, with its source.
    pub choices: Vec<InitChoice>,
}

/// Build the minimal project declaration from one resolved forge context and
/// port-table stack detection.
///
/// The remote, its protected branches, the verify base and remote, and the
/// detected toolchain names are not derived by the read path: without a project
/// file they fall back to a compiled default or a use-time inference. Init pins
/// exactly those choices. Detection-owned leaves are reported but not written,
/// so the file never restates what detection will supply again anyway.
pub fn init_plan(
    forge: &crate::forge::ForgeContext,
    detection: &crate::detect::Detection,
) -> InitPlan {
    let remote = forge.remote.value.clone();
    let protected = forge.protected.value.clone();
    let base = format!("{remote}/{}", forge.default_branch.value);
    let stacks: Vec<serde_json::Value> = detection
        .stacks
        .iter()
        .map(|stack| serde_json::Value::String(stack.stack.clone()))
        .collect();

    let mut git = serde_json::Map::new();
    git.insert("remote".into(), serde_json::json!(remote.clone()));
    git.insert("protected".into(), serde_json::json!(protected.clone()));
    let mut verify = serde_json::Map::new();
    verify.insert("remote".into(), serde_json::json!(remote.clone()));
    verify.insert("base".into(), serde_json::json!(base.clone()));
    let mut build = serde_json::Map::new();
    build.insert("stacks".into(), serde_json::Value::Array(stacks.clone()));
    let mut document = serde_json::Map::new();
    document.insert(
        "schema_version".into(),
        serde_json::json!(PORTS_SCHEMA_VERSION),
    );
    document.insert("git".into(), serde_json::Value::Object(git));
    document.insert("verify".into(), serde_json::Value::Object(verify));
    document.insert("build".into(), serde_json::Value::Object(build));

    let choices = vec![
        InitChoice {
            key: "git.remote",
            value: serde_json::json!(remote.clone()),
            source: forge.remote.source.clone(),
        },
        InitChoice {
            key: "git.protected",
            value: serde_json::json!(protected),
            source: forge.protected.source.clone(),
        },
        InitChoice {
            key: "verify.remote",
            value: serde_json::json!(remote),
            source: forge.remote.source.clone(),
        },
        InitChoice {
            key: "verify.base",
            value: serde_json::json!(base),
            source: forge.default_branch.source.clone(),
        },
        InitChoice {
            key: "build.stacks",
            value: serde_json::Value::Array(stacks),
            source: Source {
                layer: Layer::Detected,
                origin: Some("port-table stack detection".to_owned()),
            },
        },
    ];
    InitPlan {
        document: serde_json::Value::Object(document),
        choices,
    }
}

/// An error in one port-table layer always names its file and the key that
/// caused it, so an operator can fix that declaration without guessing which
/// Git setting failed.
fn layer_error(path: &Path, key: &str, message: impl std::fmt::Display) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{}: {key}: {message}", path.display()),
    )
}

/// A retired layer spelling is a refusal, not a value to decode or silently
/// upgrade: install rewrites persisted spellings once, and a repository file
/// was never in that migration's scope. Name the new spelling in the refusal.
fn reject_retired_sources(path: &Path, value: &serde_json::Value) -> io::Result<()> {
    fn retired(path: &Path, key: &str) -> io::Error {
        layer_error(
            path,
            key,
            format!(
                "retired source spelling \"builtin\"; use \"{}\" instead",
                Layer::Default.as_str()
            ),
        )
    }
    fn walk(value: &serde_json::Value, prefix: &str, path: &Path) -> io::Result<()> {
        match value {
            serde_json::Value::Object(map) => {
                for (name, child) in map {
                    let key = if prefix.is_empty() {
                        name.clone()
                    } else {
                        format!("{prefix}.{name}")
                    };
                    if name == "source" && child.as_str() == Some("builtin") {
                        return Err(retired(path, &key));
                    }
                    if key == "build.leaf_sources"
                        && let Some(leaves) = child.as_object()
                    {
                        for (leaf, source) in leaves {
                            if source.as_str() == Some("builtin") {
                                return Err(retired(path, &format!("{key}.{leaf}")));
                            }
                        }
                    }
                    walk(child, &key, path)?;
                }
                Ok(())
            }
            serde_json::Value::Array(items) => {
                for child in items {
                    walk(child, prefix, path)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    walk(value, "", path)
}

/// A refused port file is reported through the same typed sink as any other
/// configuration warning, so `--json` never carries a raw stderr line.
fn warn_malformed_port_file(path: &Path, error: &io::Error) {
    let file = path.display().to_string();
    warn(
        Warning::new(
            CONFIG_MALFORMED,
            format!("{file}: malformed port declaration ignored: {error}"),
        )
        .subject(file)
        .repair(format!(
            "correct the declaration in {path:?}, then rerun the command"
        ))
        .once_per_change(),
    );
}

/// Deep-merge an overlay's object fields over the lower layer. Objects merge
/// recursively, every other JSON shape replaces the lower value, and unknown
/// keys are retained so the decoder can warn about them.
fn merge_layer(target: &mut serde_json::Value, overlay: &serde_json::Value) {
    match (target, overlay) {
        (serde_json::Value::Object(base), serde_json::Value::Object(changes)) => {
            for (key, value) in changes {
                match base.get_mut(key) {
                    Some(existing) => merge_layer(existing, value),
                    None => {
                        base.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (slot, value) => *slot = value.clone(),
    }
}

/// Find the first dotted key whose JSON shape differs from the compiled
/// template. Used only to name the key in a decode error; the decoder still
/// owns acceptance of the merged value.
fn first_type_mismatch(
    template: &serde_json::Value,
    value: &serde_json::Value,
    prefix: &str,
) -> Option<String> {
    if value.is_null() {
        return None;
    }
    match (template, value) {
        (serde_json::Value::Object(base), serde_json::Value::Object(over)) => {
            for (key, child) in over {
                if let Some(expected) = base.get(key)
                    && let Some(found) =
                        first_type_mismatch(expected, child, &format!("{prefix}{key}"))
                {
                    return Some(found);
                }
            }
            None
        }
        (serde_json::Value::Array(_), serde_json::Value::Array(_))
        | (serde_json::Value::String(_), serde_json::Value::String(_))
        | (serde_json::Value::Number(_), serde_json::Value::Number(_))
        | (serde_json::Value::Bool(_), serde_json::Value::Bool(_)) => None,
        _ => Some(prefix.trim_end_matches('.').to_owned()),
    }
}

/// The layer walk's two derived products: the winning layer per leaf, kept
/// for the per-port fold, and every declared candidate with its exact origin
/// in application order (lowest layer first).
#[derive(Clone, Default)]
struct LeafWalk {
    layers: BTreeMap<String, Layer>,
    candidates: BTreeMap<String, Vec<Sourced<serde_json::Value>>>,
}

impl LeafWalk {
    fn record(&mut self, path: &str, layer: Layer, origin: &str, value: serde_json::Value) {
        self.layers.insert(path.to_owned(), layer);
        self.candidates
            .entry(path.to_owned())
            .or_default()
            .push(Sourced {
                value,
                source: Source {
                    layer,
                    origin: Some(origin.to_owned()),
                },
            });
    }

    fn remove_prefix(&mut self, prefix: &str) {
        self.layers.retain(|key, _| !key.starts_with(prefix));
        self.candidates.retain(|key, _| !key.starts_with(prefix));
    }
}

/// Record the leaves an overlay declares against its layer. Objects recurse
/// (tier names are object keys); arrays and scalars are leaves. The synthetic
/// `source` label and `schema_version` are not configuration leaves. Each
/// declaration becomes one candidate in the leaf's source history, so an
/// explanation can name both the winner and the layers it shadowed.
fn record_leaf_sources(
    value: &serde_json::Value,
    layer: Layer,
    origin_base: &str,
    prefix: &str,
    walk: &mut LeafWalk,
) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                // Skip synthetic metadata. A tier name is a repository-defined
                // key and must stay in the walk.
                if path == "schema_version"
                    || (key == "source" && !prefix.contains('.'))
                    || path == "build.leaf_sources"
                    || path.starts_with("build.leaf_sources.")
                {
                    continue;
                }
                record_leaf_sources(child, layer, origin_base, &path, walk);
            }
        }
        _ => {
            if !prefix.is_empty() {
                walk.record(
                    prefix,
                    layer,
                    &format!("{origin_base}#{prefix}"),
                    value.clone(),
                );
            }
        }
    }
}

fn apply_layer(
    table: &mut serde_json::Value,
    walk: &mut LeafWalk,
    path: &Path,
    source: Layer,
    on_unknown: &mut impl FnMut(&[u8], &provenant_records::InputWarning),
    on_malformed: &mut impl FnMut(&Path, &io::Error),
) -> io::Result<()> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error)
            if error.kind() == io::ErrorKind::NotFound
                || (source == Layer::HostProject
                    && error.kind() == io::ErrorKind::NotADirectory) =>
        {
            return Ok(());
        }
        Err(error) => return Err(layer_error(path, "file", error)),
    };
    let mut bytes = Vec::new();
    file.take(MAX_PORTS_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| layer_error(path, "file", error))?;
    if bytes.len() as u64 > MAX_PORTS_BYTES {
        return Err(layer_error(path, "file", "port table exceeds size limit"));
    }
    let mut overlay: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|error| layer_error(path, "json", error))?;
    reject_retired_sources(path, &overlay)?;
    if let Some(tiers) = overlay
        .pointer_mut("/verify/tiers")
        .and_then(serde_json::Value::as_object_mut)
    {
        tiers.retain(|name, tier| {
            let Some(commands) = tier.get("commands").and_then(serde_json::Value::as_array) else {
                return true;
            };
            for (index, value) in commands.iter().enumerate() {
                let decoded = serde_json::from_value::<TierCommand>(value.clone());
                let reason = match decoded {
                    Err(error) => Some(format!("invalid command: {error}")),
                    Ok(command) if command.argv.is_empty() => Some("empty argv".into()),
                    Ok(command) if command.argv[0].is_empty() => Some("empty program".into()),
                    Ok(command) if !relative(&command.cwd, true) => {
                        Some("cwd must be relative and contain no .. component".into())
                    }
                    Ok(_) => None,
                };
                if let Some(reason) = reason {
                    on_malformed(
                        path,
                        &layer_error(
                            path,
                            &format!("verify.tiers.{name}.commands[{index}]"),
                            reason,
                        ),
                    );
                    return false;
                }
            }
            true
        });
    }
    // The origin names the exact declaration: a file plus the dotted leaf.
    // The project layer uses its stable relative path so the wire origin
    // matches the documented `.provenant/ports.json#verify.remote` spelling.
    let origin_base = match source {
        Layer::Default => "builtin".to_owned(),
        Layer::Detected => "detected".to_owned(),
        Layer::Project => PORTS_CONFIG.to_owned(),
        _ => path.display().to_string(),
    };
    record_leaf_sources(&overlay, source, &origin_base, "", walk);
    let object = overlay
        .as_object_mut()
        .ok_or_else(|| layer_error(path, "<root>", "port table must be an object"))?;
    // schema_version is optional in an overlay: it defaults to the current
    // version. A mismatch warns but does not refuse a compatible shape.
    match object
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
    {
        None => {
            object.insert(
                "schema_version".into(),
                serde_json::json!(PORTS_SCHEMA_VERSION),
            );
        }
        Some(version) if version != u64::from(PORTS_SCHEMA_VERSION) => {
            let file = path.display().to_string();
            warn(
                Warning::new(
                    CONFIG_MALFORMED,
                    format!(
                        "{file}: schema_version: expected {}, found {version}; using the current schema",
                        PORTS_SCHEMA_VERSION
                    ),
                )
                .subject(file.clone())
                .repair(format!(
                    "set \"schema_version\": {PORTS_SCHEMA_VERSION} in {file}"
                ))
                .once_per_change(),
            );
            object.insert(
                "schema_version".into(),
                serde_json::json!(PORTS_SCHEMA_VERSION),
            );
        }
        Some(_) => {}
    }
    let template = table.clone();
    let target = table
        .as_object_mut()
        .ok_or_else(|| layer_error(path, "<root>", "compiled port table must be an object"))?;
    for (name, value) in object.iter_mut() {
        if name == "schema_version" {
            continue;
        }
        if PortTable::PORT_NAMES.contains(&name.as_str()) {
            if !value.is_object() {
                return Err(layer_error(path, name, "port entry must be an object"));
            }
            let existing = target
                .get_mut(name)
                .ok_or_else(|| layer_error(path, name, "unknown port"))?;
            merge_layer(existing, value);
        } else {
            target.insert(name.clone(), value.clone());
        }
    }
    let source_name = path.display().to_string();
    let merged = serde_json::to_vec(table).map_err(io::Error::other)?;
    let (decoded, warnings) = provenant_records::decode_input::<PortTable>(&source_name, &merged)
        .map_err(|error| {
        let key =
            first_type_mismatch(&template, table, "").unwrap_or_else(|| "port table".to_owned());
        layer_error(path, &key, error)
    })?;
    for warning in &warnings {
        on_unknown(&bytes, warning);
    }
    *table = serde_json::to_value(decoded).map_err(io::Error::other)?;
    Ok(())
}

/// Name the layer a port declaration came from.
pub fn port_source_layer(source: Layer) -> &'static str {
    source.as_str()
}

/// Every dotted leaf of a port table, derived from the compiled declaration so
/// a new port key is covered without editing this list. Synthetic `source`
/// labels and `schema_version` are not configuration leaves.
fn port_leaf_paths(value: &serde_json::Value, prefix: &str, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                if key == "schema_version" || (key == "source" && !prefix.contains('.')) {
                    continue;
                }
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                port_leaf_paths(child, &path, out);
            }
        }
        _ => {
            if !prefix.is_empty() {
                out.push(prefix.to_owned());
            }
        }
    }
}

/// Read at most `MAX_PORTS_BYTES` from a path, mirroring the port layer
/// reader's bound. Missing, unreadable and oversized files are treated as
/// absent so an advisory warning never has to read an unbounded file.
fn read_bounded(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    let file = fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_PORTS_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= MAX_PORTS_BYTES).then_some(bytes)
}

/// Whether a dotted port-leaf path is present in a parsed TOML document. Both
/// implicit tables and inline tables are followed, and numeric segments are
/// valid keys: `verify.tiers` is keyed `"0".."3"`.
fn toml_has_path(table: &toml_edit::Table, dotted: &str) -> bool {
    let parts: Vec<&str> = dotted.split('.').collect();
    let Some((first, rest)) = parts.split_first() else {
        return false;
    };
    table
        .get(first)
        .is_some_and(|item| toml_item_has_path(item, rest))
}

/// Continue a `toml_has_path` walk from a `toml_edit::Item`. `as_table_like`
/// covers ordinary tables, implicit dotted-key tables and inline tables; a
/// value at the final segment is itself a present leaf.
fn toml_item_has_path(item: &toml_edit::Item, rest: &[&str]) -> bool {
    let Some((part, tail)) = rest.split_first() else {
        return item.is_value() || item.as_table_like().is_some();
    };
    item.as_table_like()
        .and_then(|table| table.get(part))
        .is_some_and(|child| toml_item_has_path(child, tail))
}

/// Advisory warnings for port declarations an operator placed where no port
/// reader will ever see them. A port table in `config.toml`, or a host-project
/// layer filed under the obsolete 64-digit target-seed key rather than the
/// ProjectId binding directory, is silently ignored today. Both cases warn and
/// name the file and the key; neither refuses the command that asked for the check.
pub fn port_declaration_warnings(project: Option<&Path>, host_state: &Path) -> Vec<String> {
    let mut warnings = Vec::new();
    let Some(project) = project else {
        return warnings;
    };

    let config_path = project.join(".provenant").join("config.toml");
    if let Some(bytes) = read_bounded(&config_path)
        && let Ok(text) = std::str::from_utf8(&bytes)
        && let Ok(document) = toml_edit::Document::parse(text)
    {
        let table = serde_json::to_value(PortTable::builtin()).unwrap_or_default();
        let mut leaves = Vec::new();
        port_leaf_paths(&table, "", &mut leaves);
        for leaf in leaves {
            if toml_has_path(document.as_table(), &leaf) {
                warnings.push(format!(
                    "{}: {leaf}: port declarations are read from {PORTS_CONFIG} (or a host ports layer), not from .provenant/config.toml; the key is ignored",
                    config_path.display(),
                ));
            }
        }
    }

    // The host-project ports layer is read from the ProjectId binding
    // directory reported by `config path` (host_project_dir), not from the
    // obsolete 64-digit target-seed key used by older releases. A ports.json
    // filed under that key is never read. Resolve the correct directory through
    // the single binding helper rather than recomputing the layout here, and
    // name it in the warning so an operator can move the file.
    if let Some(dir) = crate::host_project_dir(host_state, project)
        && let Some(key) = project_key_for(project)
    {
        let misplaced = host_state.join("projects").join(&key).join("ports.json");
        if misplaced.is_file() {
            let keys = read_bounded(&misplaced)
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                .map(|value| {
                    let mut leaves = Vec::new();
                    port_leaf_paths(&value, "", &mut leaves);
                    leaves
                })
                .unwrap_or_default();
            let declared = if keys.is_empty() {
                "port declarations".to_owned()
            } else {
                keys.join(", ")
            };
            warnings.push(format!(
                "{}: {declared} (target-seed key {key}): host-project ports layers use the binding directory {}; move this file to {} (config path prints it)",
                misplaced.display(),
                dir.display(),
                dir.join("ports.json").display(),
            ));
        }
    }
    warnings
}

/// Infer an unset `verify.base` from the symbolic target of
/// `refs/remotes/<remote>/HEAD`. Returns the `<remote>/<branch>` base and a
/// source string naming the ref it came from.
pub fn infer_verify_base(remote: &str, remote_head_target: &str) -> Option<(String, String)> {
    if remote.is_empty() || remote_head_target.is_empty() {
        return None;
    }
    let base = remote_head_target.strip_prefix("refs/remotes/")?.to_owned();
    let (head_remote, branch) = base.split_once('/')?;
    if head_remote.is_empty() || branch.is_empty() {
        return None;
    }
    Some((base, format!("refs/remotes/{remote}/HEAD")))
}

/// The refusal when no layer declares `verify.base` and no remote HEAD resolves.
pub fn verify_base_unset_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "verify.base is unset and could not be inferred from the remote HEAD; set verify.base in {PORTS_CONFIG} or host ports.json, for example {{\"verify\":{{\"base\":\"origin/main\"}}}}"
        ),
    )
}

impl PortTable {
    #[allow(clippy::expect_used)] // Compiled asset; its schema is a tested invariant.
    pub fn builtin() -> Self {
        let mut table: Self = serde_json::from_str(include_str!("../assets/ports.json"))
            .expect("compiled port table asset must match its schema");
        table.set_sources(Layer::Default);
        table.sync_sources_from_provenance();
        table
    }

    /// The highest layer that set each leaf, keyed by dotted path.
    pub fn provenance(&self) -> &BTreeMap<String, Layer> {
        &self.provenance
    }

    /// Every layer that declared one dotted leaf, lowest first, with its
    /// origin. The final candidate is the effective winner; the rest are the
    /// layers it shadowed. An unknown leaf has no candidates.
    pub fn sourced_leaf(&self, leaf: &str) -> &[Sourced<serde_json::Value>] {
        self.sourced_leaves
            .get(leaf)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Every leaf's source history, keyed by dotted path.
    pub fn sourced_leaves(&self) -> &BTreeMap<String, Vec<Sourced<serde_json::Value>>> {
        &self.sourced_leaves
    }

    /// Resolve the affected-verification full-run interval with its exact
    /// layer source. Invalid durations warn and fall back to the builtin 24h.
    pub fn verify_affected_full_after_duration(&self) -> ResolvedBuildValue<Duration> {
        let leaf = "verify.affected.full_after";
        let source = self.provenance.get(leaf).copied().unwrap_or(Layer::Default);
        match crate::gc::parse_gc_idle_duration(&self.verify.affected.full_after) {
            Ok(seconds) => ResolvedBuildValue {
                value: Duration::from_secs(seconds),
                source,
                warnings: Vec::new(),
            },
            Err(reason) => ResolvedBuildValue {
                value: Duration::from_secs(24 * 60 * 60),
                source: Layer::Default,
                warnings: vec![format!(
                    "invalid {leaf} value {:?} from {}; using builtin value (default): {reason}",
                    self.verify.affected.full_after,
                    port_source_layer(source)
                )],
            },
        }
    }

    /// Resolve the configured path for a managed writer branch.
    pub fn managed_writer_path(&self, branch: &str) -> io::Result<PathBuf> {
        self.worktree_layout.path_for_branch(branch)
    }

    /// Identify a managed writer branch under any declared layout root.
    pub fn managed_writer_branch<'a>(&self, relative: &'a Path) -> Option<&'a str> {
        self.worktree_layout.branch_component(relative)
    }

    /// Resolve each configured worktree root beneath its owning project or
    /// Git common directory for host path protection.
    pub fn worktree_roots_under(&self, root: &Path) -> Vec<PathBuf> {
        self.worktree_layout
            .roots
            .iter()
            .map(|relative| root.join(relative))
            .collect()
    }

    pub fn validate(&self) -> io::Result<()> {
        if self.schema_version != PORTS_SCHEMA_VERSION
            || self.entries().any(|(_, _, _, valid, _)| !valid)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid port table",
            ));
        }
        Ok(())
    }

    /// Resolve the effective table by applying explicit host entries to the
    /// compiled declarations, then explicit project entries. Omitted ports
    /// inherit their lower layer and retain that layer's source.
    pub fn read_for(project: Option<&Path>, host_state: &Path) -> io::Result<Self> {
        let table = Self::read_for_inspection(project, host_state)?;
        table.validate()?;
        Ok(table)
    }

    /// Resolve only the project declaration over compiled defaults when no
    /// validated host state root is available to supply host-level ports.
    pub fn read_project_only(project: &Path) -> io::Result<Self> {
        let table = Self::read_layers(
            Some(project),
            None,
            |bytes, warning| {
                let _ = crate::loader::warn_unknown_input_once(project, bytes, warning);
            },
            warn_malformed_port_file,
        )?;
        table.validate()?;
        Ok(table)
    }

    /// Inspect well-typed declarations without hiding semantically invalid ports.
    pub fn read_for_inspection(project: Option<&Path>, host_state: &Path) -> io::Result<Self> {
        Self::read_for_inspection_with(project, host_state, |bytes, warning| {
            let _ = crate::loader::warn_unknown_input_once(host_state, bytes, warning);
        })
    }

    fn read_for_inspection_with(
        project: Option<&Path>,
        host_state: &Path,
        mut on_unknown: impl FnMut(&[u8], &provenant_records::InputWarning),
    ) -> io::Result<Self> {
        Self::read_layers(
            project,
            Some(host_state),
            &mut on_unknown,
            warn_malformed_port_file,
        )
    }

    fn read_layers(
        project: Option<&Path>,
        host_state: Option<&Path>,
        mut on_unknown: impl FnMut(&[u8], &provenant_records::InputWarning),
        mut on_malformed: impl FnMut(&Path, &io::Error),
    ) -> io::Result<Self> {
        let mut document = serde_json::to_value(Self::builtin()).map_err(io::Error::other)?;
        let mut walk = LeafWalk::default();
        record_leaf_sources(&document, Layer::Default, "builtin", "", &mut walk);
        if let Some(project) = project {
            let detected = crate::detect::detect(project);
            if !detected.stacks.is_empty() {
                let default_tier = detected.default_tier.unwrap_or_default();
                document["verify"]["default_tier"] =
                    serde_json::Value::String(default_tier.clone());
                walk.record(
                    "verify.default_tier",
                    Layer::Detected,
                    "detected#verify.default_tier",
                    serde_json::Value::String(default_tier),
                );
                document["verify"]["tiers"] = serde_json::json!({});
                walk.remove_prefix("verify.tiers.");
                for (name, commands) in detected.tiers {
                    let selection =
                        if name == "2" || commands.iter().any(|command| command.stack != "rust") {
                            "all"
                        } else {
                            "packages"
                        };
                    document["verify"]["tiers"][&name] = serde_json::json!({
                        "selection": selection,
                        "warn_for_host_load": false,
                        "commands": commands,
                    });
                    record_leaf_sources(
                        &document["verify"]["tiers"][&name],
                        Layer::Detected,
                        "detected",
                        &format!("verify.tiers.{name}"),
                        &mut walk,
                    );
                }
            }
        }
        if let Some(host_state) = host_state {
            apply_layer(
                &mut document,
                &mut walk,
                &host_state.join("ports.json"),
                Layer::Host,
                &mut on_unknown,
                &mut on_malformed,
            )?;
            // A consumer repository can be configured entirely from host
            // state: its binding layer sits between host and project. The
            // directory is the ProjectId binding directory named by `config
            // path`; a missing binding or file skips this layer silently.
            if let Some(dir) =
                project.and_then(|project| crate::host_project_dir(host_state, project))
            {
                apply_layer(
                    &mut document,
                    &mut walk,
                    &dir.join("ports.json"),
                    Layer::HostProject,
                    &mut on_unknown,
                    &mut on_malformed,
                )?;
            }
        }
        if let Some(project) = project {
            let path = project.join(PORTS_CONFIG);
            let mut candidate = document.clone();
            let mut candidate_walk = walk.clone();
            let mut warnings = Vec::new();
            let applied = apply_layer(
                &mut candidate,
                &mut candidate_walk,
                &path,
                Layer::Project,
                &mut |bytes, warning| warnings.push((bytes.to_vec(), warning.clone())),
                &mut on_malformed,
            );
            match applied {
                Ok(()) => {
                    document = candidate;
                    walk = candidate_walk;
                    for (bytes, warning) in warnings {
                        on_unknown(&bytes, &warning);
                    }
                }
                // Project overlays are optional. A malformed one must not stop
                // a command that does not need verify; the warning names its
                // file and key.
                Err(error) => on_malformed(&path, &error),
            }
        }
        let mut table: Self = serde_json::from_value(document).map_err(io::Error::other)?;
        table.provenance = walk.layers;
        table.sourced_leaves = walk.candidates;
        table.sync_sources_from_provenance();
        Ok(table)
    }
}

impl Default for PortTable {
    fn default() -> Self {
        Self::builtin()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_records::warning::capture;

    #[test]
    fn declared_port_rows_and_backend_shapes_round_trip() {
        let table = PortTable::builtin();
        let value = serde_json::to_value(&table).unwrap();
        let entries: Vec<_> = table.entries().collect();
        let declared: std::collections::BTreeSet<_> =
            PortTable::PORT_NAMES.iter().copied().collect();
        let observed: std::collections::BTreeSet<_> = entries.iter().map(|entry| entry.0).collect();
        assert_eq!(observed, declared);
        for (name, source, backend, _, _) in entries {
            let entry = value
                .get(name)
                .and_then(serde_json::Value::as_object)
                .unwrap();
            assert_eq!(
                entry.get("source"),
                Some(&serde_json::to_value(source).unwrap()),
                "{name} source"
            );
            if name != "build" {
                // build declares no backend; it is builtin by construction.
                let expected_backend = match backend {
                    PortBackendKind::Builtin => "builtin",
                    PortBackendKind::Command => "command",
                    PortBackendKind::None => "none",
                };
                assert_eq!(
                    entry.get("backend").and_then(|value| value.get("kind")),
                    Some(&serde_json::json!(expected_backend)),
                    "{name} backend"
                );
            }
        }
        assert_eq!(serde_json::from_value::<PortTable>(value).unwrap(), table);

        let cases = [
            PortBackend::Builtin,
            PortBackend::Command {
                command: PortCommand {
                    argv: vec!["verify".into()],
                    exit_codes: CommandExitCodes {
                        success: vec![0],
                        unknown: vec![75],
                        other: ExitDisposition::Refused,
                    },
                },
            },
            PortBackend::None,
        ];
        for backend in cases {
            let mut layout = table.worktree_layout.clone();
            layout.backend = backend.clone();
            assert_eq!(
                layout.unavailable_reason().is_none(),
                layout.path_for_branch("feature/topic").is_ok()
            );
            let value = serde_json::to_value(&backend).unwrap();
            assert_eq!(
                serde_json::from_value::<PortBackend>(value).unwrap(),
                backend
            );
        }

        let land_cases = [
            LandBackend::Builtin,
            LandBackend::Command {
                command: provenant_records::landing::RepositoryCommand {
                    argv: vec!["provenant-land".into()],
                    unknown_exit_code: 75,
                    timeout_seconds: 30,
                },
            },
            LandBackend::None,
        ];
        for backend in land_cases {
            let value = serde_json::to_value(&backend).unwrap();
            assert_eq!(
                serde_json::from_value::<LandBackend>(value).unwrap(),
                backend
            );
        }

        let selection_cases = [
            VerifySelection::Packages,
            VerifySelection::Integrations,
            VerifySelection::All,
        ];
        for selection in selection_cases {
            let value = serde_json::to_value(selection).unwrap();
            assert_eq!(
                serde_json::from_value::<VerifySelection>(value).unwrap(),
                selection
            );
        }

        let command = provenant_records::landing::RepositoryCommand {
            argv: vec!["land".into()],
            unknown_exit_code: 75,
            timeout_seconds: 30,
        };
        assert_eq!(
            command.outcome(0),
            provenant_records::landing::RepositoryCommandOutcome::Landed,
        );
        assert_eq!(
            command.outcome(1),
            provenant_records::landing::RepositoryCommandOutcome::Refused,
        );
        assert_eq!(
            command.outcome(75),
            provenant_records::landing::RepositoryCommandOutcome::Unknown,
        );

        let source_cases = [
            Layer::Default,
            Layer::Detected,
            Layer::Host,
            Layer::HostProject,
            Layer::Project,
        ];
        for source in source_cases {
            let value = serde_json::to_value(source).unwrap();
            assert_eq!(serde_json::from_value::<Layer>(value).unwrap(), source);
        }
    }

    #[test]
    fn worktree_layout_resolves_default_and_custom_roots_from_one_declaration() {
        let mut table = PortTable::builtin();
        assert!(table.validate().is_ok());
        {
            let custom = &mut table.worktree_layout;
            custom.roots = vec![
                PathBuf::from(".worktrees"),
                PathBuf::from(".claude/worktrees"),
            ];
            custom.default = PathBuf::from(".claude/worktrees/<branch>");
            assert_eq!(
                custom.path_for_branch("feature/topic").unwrap(),
                PathBuf::from(".claude/worktrees/feature-topic"),
            );
            assert_eq!(
                custom.branch_component(Path::new(".claude/worktrees/feature-topic")),
                Some("feature-topic"),
            );
            assert!(custom.contains_worktree(Path::new(".worktrees/other")));
            assert!(
                custom
                    .branch_component(Path::new(".claude/worktrees/deep/name"))
                    .is_none()
            );
            custom.roots = vec![PathBuf::from("trees"), PathBuf::from("legacy")];
            custom.default = PathBuf::from("trees/topic-<branch>/checkout");
            let path = custom.path_for_branch("feature/topic").unwrap();
            assert_eq!(path, PathBuf::from("trees/topic-feature-topic/checkout"));
            assert_eq!(custom.branch_component(&path), Some("feature-topic"));
            assert_eq!(
                custom.branch_component(Path::new("legacy/topic-feature-topic/checkout")),
                Some("feature-topic"),
            );
        }
        assert!(table.validate().is_ok());
        table.worktree_layout.roots = vec![PathBuf::from("trees/<branch>")];
        table.worktree_layout.default = PathBuf::from("trees/<branch>/checkout");
        assert!(table.validate().is_err());
        table = PortTable::builtin();
        table.worktree_layout.default = PathBuf::from("../outside/<branch>");
        assert!(table.validate().is_err());
    }

    #[test]
    fn command_environment_inherits_execution_context_only() {
        let ambient = COMMAND_CONTEXT_ENV
            .iter()
            .map(|name| (OsString::from(*name), OsString::from("context")))
            .chain([
                ("GH_TOKEN".into(), "credential".into()),
                ("SSH_AUTH_SOCK".into(), "agent".into()),
            ]);
        let result: BTreeMap<_, _> =
            port_command_environment(ambient, &[("PATH".into(), "explicit".into())])
                .into_iter()
                .collect();
        for name in COMMAND_CONTEXT_ENV {
            assert!(result.contains_key(&OsString::from(*name)));
        }
        assert_eq!(
            result.get(&OsString::from("PATH")),
            Some(&OsString::from("explicit"))
        );
        assert!(!result.contains_key(&OsString::from("GH_TOKEN")));
        assert!(!result.contains_key(&OsString::from("SSH_AUTH_SOCK")));
    }

    #[test]
    fn inspection_reports_invalid_ports_without_executing_them() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join(".provenant")).unwrap();
        std::fs::write(directory.path().join(PORTS_CONFIG), r#"{"schema_version":3,"naming":{"backend":{"kind":"command","command":{"argv":[],"exit_codes":{"success":[0],"unknown":[],"other":"refused"}}}}}"#).unwrap();
        let table =
            PortTable::read_for_inspection(Some(directory.path()), directory.path()).unwrap();
        let naming = table.entries().find(|entry| entry.0 == "naming").unwrap();
        assert_eq!(naming.1, Layer::Project);
        assert!(!naming.3);
        assert!(naming.4.is_some());
        assert!(PortTable::read_for(Some(directory.path()), directory.path()).is_err());
        let mut invalid = PortTable::builtin();
        invalid.worktree_layout.default = "../outside/{branch}".into();
        assert!(
            invalid
                .worktree_layout
                .path_for_branch("feature/topic")
                .is_err()
        );
        for (_, _, _, valid, reason) in invalid
            .entries()
            .filter(|entry| entry.0 == "worktree_layout")
        {
            assert!(!valid);
            assert!(reason.is_some());
        }
    }

    #[test]
    fn port_config_unknown_root_and_nested_keys_warn_with_their_layer_source() {
        let directory = tempfile::tempdir().unwrap();
        let host = directory.path().join("host");
        let project = directory.path().join("project");
        std::fs::create_dir_all(&host).unwrap();
        std::fs::create_dir_all(project.join(".provenant")).unwrap();
        let builtin = serde_json::to_value(PortTable::builtin()).unwrap();

        let mut host_overlay = serde_json::json!({
            "schema_version": PORTS_SCHEMA_VERSION,
            "verify": builtin["verify"].clone(),
            "future_port": {"enabled": true},
            "build_admission": {"backend": {"kind": "builtin"}},
        });
        host_overlay["verify"]["backend"] = serde_json::json!({"kind": "none"});
        host_overlay["verify"]["test_threads"] = serde_json::Value::Null;
        host_overlay["verify"]["future_nested"] = serde_json::json!({"enabled": true});
        std::fs::write(
            host.join("ports.json"),
            serde_json::to_vec(&host_overlay).unwrap(),
        )
        .unwrap();

        let mut project_overlay = serde_json::json!({
            "schema_version": PORTS_SCHEMA_VERSION,
            "naming": builtin["naming"].clone(),
        });
        project_overlay["naming"]["future_nested"] = serde_json::json!("ignored");
        std::fs::write(
            project.join(PORTS_CONFIG),
            serde_json::to_vec(&project_overlay).unwrap(),
        )
        .unwrap();

        let mut warnings = Vec::new();
        let table = PortTable::read_for_inspection_with(Some(&project), &host, |_, warning| {
            warnings.push((warning.source.clone(), warning.key.clone()))
        })
        .unwrap();
        assert_eq!(table.verify.backend, PortBackend::None);
        assert_eq!(table.verify.source, Layer::Host);
        assert_eq!(table.naming.source, Layer::Project);
        let encoded = serde_json::to_value(&table).unwrap();
        assert!(encoded.get("future_port").is_none());
        assert!(encoded["verify"].get("future_nested").is_none());
        assert!(encoded["naming"].get("future_nested").is_none());

        let host_source = host.join("ports.json").display().to_string();
        let project_source = project.join(PORTS_CONFIG).display().to_string();
        assert_eq!(
            warnings
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>(),
            std::collections::BTreeSet::from([
                (host_source.clone(), "future_port".to_owned()),
                (host_source.clone(), "build_admission".to_owned()),
                (host_source.clone(), "verify.future_nested".to_owned()),
                (project_source, "naming.future_nested".to_owned()),
            ])
        );

        let malformed = serde_json::json!({
            "schema_version": PORTS_SCHEMA_VERSION,
            "verify": false,
        });
        std::fs::write(
            project.join(PORTS_CONFIG),
            serde_json::to_vec(&malformed).unwrap(),
        )
        .unwrap();
        // A malformed project overlay is optional: it is skipped with a
        // warning that names the file and the offending key.
        let mut malformed_warnings = Vec::new();
        let table = PortTable::read_layers(
            Some(&project),
            Some(&host),
            |_, _| {},
            |path, error| malformed_warnings.push((path.to_path_buf(), error.to_string())),
        )
        .unwrap();
        assert_eq!(table.verify.backend, PortBackend::None);
        assert_eq!(malformed_warnings.len(), 1);
        assert!(malformed_warnings[0].0.ends_with(PORTS_CONFIG));
        assert!(
            malformed_warnings[0].1.contains("verify"),
            "{malformed_warnings:?}"
        );

        // A malformed host layer is a real boundary: it still refuses, but
        // names its file and key instead of masquerading as a Git error.
        std::fs::write(
            host.join("ports.json"),
            serde_json::to_vec(&malformed).unwrap(),
        )
        .unwrap();
        let message = PortTable::read_for_inspection(Some(&project), &host)
            .unwrap_err()
            .to_string();
        assert!(message.contains("ports.json"), "{message}");
        assert!(message.contains("verify"), "{message}");
        assert!(!message.contains("Git configuration"), "{message}");
    }

    #[test]
    fn partial_overlay_deep_merges_fields_and_keeps_lower_sources() {
        let temp = tempfile::tempdir().unwrap();
        let host = temp.path().join("host");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&host).unwrap();
        std::fs::create_dir_all(project.join(".provenant")).unwrap();

        // schema_version is optional in a layer; the host sets a non-default
        // field and a tier override, the project sets only verify.base.
        let host_overlay = serde_json::json!({
            "verify": {
                "test_threads": 4,
                "tiers": {"2": {"warn_for_host_load": false}},
            },
        });
        std::fs::write(
            host.join("ports.json"),
            serde_json::to_vec(&host_overlay).unwrap(),
        )
        .unwrap();
        let project_overlay = serde_json::json!({"verify": {"base": "upstream/master"}});
        std::fs::write(
            project.join(PORTS_CONFIG),
            serde_json::to_vec(&project_overlay).unwrap(),
        )
        .unwrap();

        let table = PortTable::read_for(Some(&project), &host).unwrap();
        assert_eq!(table.verify.base, "upstream/master");
        assert_eq!(table.verify.source, Layer::Project);
        assert_eq!(table.verify.test_threads, Some(4));
        assert!(!table.verify.tiers["2"].warn_for_host_load);
        assert_eq!(table.verify.remote, "origin");
        assert_eq!(table.verify.default_tier, "0");
        assert_eq!(table.land.source, Layer::Default);

        // The same field-level merge applies to a host overlay over builtins.
        std::fs::remove_file(project.join(PORTS_CONFIG)).unwrap();
        let table = PortTable::read_for(None, &host).unwrap();
        assert_eq!(table.verify.source, Layer::Host);
        assert_eq!(table.verify.test_threads, Some(4));
        assert_eq!(table.verify.base, "");
    }

    #[test]
    fn invalid_tier_command_warns_and_falls_back() {
        for (cwd, argv, reason) in [
            (".", vec![], "empty argv"),
            (".", vec![""], "empty program"),
            ("/absolute", vec!["true"], "cwd"),
            ("a/../b", vec!["true"], "cwd"),
        ] {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir(root.path().join(".provenant")).unwrap();
            let path = root.path().join(PORTS_CONFIG);
            std::fs::write(
                &path,
                serde_json::to_vec(&serde_json::json!({"verify":{"tiers":{
                    "0":{"commands":[{"cwd":cwd,"argv":argv}],"selection":"all"},
                    "1":{"commands":[{"argv":["valid"]}]}
                }}}))
                .unwrap(),
            )
            .unwrap();
            let mut warnings = Vec::new();
            let table = PortTable::read_layers(
                Some(root.path()),
                None,
                |_, _| {},
                |file, error| {
                    warnings.push((file.to_owned(), error.to_string()));
                },
            )
            .unwrap();
            assert!(table.validate().is_ok());
            assert_eq!(
                table.verify.tiers["0"],
                PortTable::builtin().verify.tiers["0"]
            );
            assert_eq!(
                table.provenance()["verify.tiers.0.commands"],
                Layer::Default
            );
            assert_eq!(table.verify.tiers["1"].commands[0].argv, ["valid"]);
            assert_eq!(warnings.len(), 1);
            assert_eq!(warnings[0].0, path);
            assert!(warnings[0].1.contains("verify.tiers.0") && warnings[0].1.contains(reason));
        }
    }

    // Spec: docs/specs/state.md#one-sourced-port-table
    #[test]
    fn tier_command_string_and_table_forms_decode_equal() {
        let text = "cargo fmt {packages} -- --check";
        let argv = ["cargo", "fmt", "{packages}", "--", "--check"];
        let from_string: TierCommand = serde_json::from_value(serde_json::json!(text)).unwrap();
        let from_table: TierCommand =
            serde_json::from_value(serde_json::json!({"argv": argv})).unwrap();
        assert_eq!(from_string, from_table);
        assert_eq!(
            from_string,
            serde_json::from_value::<TierCommand>(serde_json::json!({
                "stack": "",
                "cwd": ".",
                "argv": argv,
            }))
            .unwrap()
        );
        assert_eq!(from_string.argv, argv);
        assert!(from_string.stack.is_empty());
        assert_eq!(from_string.cwd, PathBuf::from("."));

        // The pre-decode filter accepts the shorthand, so it survives the
        // layer walk and serialises back in the table shape.
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".provenant")).unwrap();
        let path = root.path().join(PORTS_CONFIG);
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "verify": {"tiers": {
                    "0": {"commands": [text], "selection": "packages"},
                }},
            }))
            .unwrap(),
        )
        .unwrap();
        let mut warnings = Vec::new();
        let table = PortTable::read_layers(
            Some(root.path()),
            None,
            |_, _| {},
            |file, error| warnings.push((file.to_owned(), error.to_string())),
        )
        .unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(table.verify.tiers["0"].commands, [from_string]);
        assert_eq!(
            table.provenance()["verify.tiers.0.commands"],
            Layer::Project
        );
        let value = serde_json::to_value(&table).unwrap();
        assert!(value["verify"]["tiers"]["0"]["commands"][0].is_object());
        // Provenance is derived from the layer walk and never serialised, so
        // the decoded table carries the same ports but not the labels' history.
        let decoded = serde_json::from_value::<PortTable>(value.clone()).unwrap();
        assert_eq!(decoded.verify, table.verify);
        assert_eq!(serde_json::to_value(&decoded).unwrap(), value);
    }

    // Spec: docs/specs/state.md#one-sourced-port-table
    #[test]
    fn tier_command_string_honours_quotes_and_escapes() {
        let words = |text: &str| -> Vec<String> {
            serde_json::from_value::<TierCommand>(serde_json::json!(text))
                .unwrap()
                .argv
        };
        // Double quotes honour a backslash before a quote or another backslash;
        // single quotes keep a backslash literally.
        assert_eq!(
            words(r#"tool "say \"hi\"" "back\\slash" 'a\b' 'single "quote"'"#),
            [
                "tool",
                "say \"hi\"",
                "back\\slash",
                "a\\b",
                "single \"quote\""
            ]
        );
        // A doubled backslash immediately before the closing quote is a literal
        // backslash, not an escaped delimiter.
        assert_eq!(words(r#"tool "ends\\""#), ["tool", "ends\\"]);
        assert_eq!(words(r"tool back\\slash"), ["tool", "back\\slash"]);
    }

    // Spec: docs/specs/state.md#one-sourced-port-table
    #[test]
    fn unterminated_tier_command_string_warns_and_falls_back() {
        for text in [
            "cargo fmt 'unbalanced",
            "cargo fmt \"unbalanced",
            "cargo fmt trailing\\",
            "cargo fmt \"trailing\\",
        ] {
            assert!(
                serde_json::from_value::<TierCommand>(serde_json::json!(text)).is_err(),
                "{text}"
            );
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir(root.path().join(".provenant")).unwrap();
            let path = root.path().join(PORTS_CONFIG);
            std::fs::write(
                &path,
                serde_json::to_vec(&serde_json::json!({
                    "verify": {"tiers": {
                        "0": {"commands": [text, "cargo check"], "selection": "all"},
                        "1": {"commands": ["cargo check", {"argv": ["true"]}]},
                    }},
                }))
                .unwrap(),
            )
            .unwrap();
            let mut warnings = Vec::new();
            let table = PortTable::read_layers(
                Some(root.path()),
                None,
                |_, _| {},
                |file, error| warnings.push((file.to_owned(), error.to_string())),
            )
            .unwrap();
            assert!(table.validate().is_ok());
            assert_eq!(
                table.verify.tiers["0"],
                PortTable::builtin().verify.tiers["0"]
            );
            assert_eq!(table.verify.tiers["1"].commands[0].argv, ["cargo", "check"]);
            assert_eq!(table.verify.tiers["1"].commands[1].argv, ["true"]);
            assert_eq!(warnings.len(), 1, "{warnings:?}");
            assert_eq!(warnings[0].0, path);
            assert!(
                warnings[0].1.contains("verify.tiers.0.commands[0]"),
                "{warnings:?}"
            );
        }
    }

    #[test]
    fn malformed_command_drops_only_its_tier_from_each_layer() {
        for command in [serde_json::json!({}), serde_json::json!({"argv": 42})] {
            for host in [false, true] {
                let root = tempfile::tempdir().unwrap();
                let state = tempfile::tempdir().unwrap();
                let path = if host {
                    state.path().join("ports.json")
                } else {
                    std::fs::create_dir(root.path().join(".provenant")).unwrap();
                    root.path().join(PORTS_CONFIG)
                };
                std::fs::write(
                    &path,
                    serde_json::to_vec(&serde_json::json!({
                        "verify": {"base":"upstream/main", "tiers":{"1":{"commands":[command]}}}
                    }))
                    .unwrap(),
                )
                .unwrap();
                let mut warnings = Vec::new();
                let table = PortTable::read_layers(
                    Some(root.path()),
                    Some(state.path()),
                    |_, _| {},
                    |file, error| warnings.push((file.to_owned(), error.to_string())),
                )
                .unwrap();
                assert_eq!(table.verify.base, "upstream/main");
                assert_eq!(
                    table.verify.tiers["1"],
                    PortTable::builtin().verify.tiers["1"]
                );
                assert_eq!(
                    table.provenance()["verify.tiers.1.commands"],
                    Layer::Default
                );
                assert_eq!(warnings.len(), 1);
                assert_eq!(warnings[0].0, path);
                assert!(warnings[0].1.contains("verify.tiers.1.commands[0]"));
            }
        }
    }

    #[test]
    fn detected_selection_respects_stack_mix() {
        for (rust, node, expected0, expected1) in [
            (
                true,
                false,
                VerifySelection::Packages,
                VerifySelection::Packages,
            ),
            (false, true, VerifySelection::All, VerifySelection::All),
            (true, true, VerifySelection::All, VerifySelection::All),
        ] {
            let root = tempfile::tempdir().unwrap();
            if rust {
                std::fs::write(root.path().join("Cargo.toml"), "[workspace]\n").unwrap();
            }
            if node {
                std::fs::write(
                    root.path().join("package.json"),
                    r#"{"scripts":{"lint":"ok","test":"ok"}}"#,
                )
                .unwrap();
            }
            let table = PortTable::read_layers(
                Some(root.path()),
                None,
                |_, _| {},
                |_, error| panic!("{error}"),
            )
            .unwrap();
            assert_eq!(table.verify.tiers["0"].selection, expected0);
            assert_eq!(table.verify.tiers["1"].selection, expected1);
            if rust {
                assert_eq!(table.verify.tiers["2"].selection, VerifySelection::All);
            } else {
                assert!(!table.verify.tiers.contains_key("2"));
            }
            assert!(!table.verify.tiers.contains_key("3"));
        }
    }

    #[test]
    fn detection_without_commands_leaves_no_builtin_tiers() {
        let root = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("package.json"), "{}").unwrap();
        let table = PortTable::read_for(Some(root.path()), state.path()).unwrap();
        assert!(table.verify.tiers.is_empty());
        assert!(table.verify.default_tier.is_empty());
        assert_eq!(table.provenance()["verify.default_tier"], Layer::Detected);
    }

    #[test]
    fn detected_tiers_replace_builtin_and_configured_wins() {
        let root = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("package.json"),
            r#"{"scripts":{"lint":"ok","test":"ok"}}"#,
        )
        .unwrap();
        std::fs::create_dir(root.path().join(".provenant")).unwrap();
        std::fs::write(
            root.path().join(PORTS_CONFIG),
            r#"{"verify":{"tiers":{"1":{"commands":[{"argv":["custom"]}]}}}}"#,
        )
        .unwrap();
        let table = PortTable::read_for(Some(root.path()), state.path()).unwrap();
        assert_eq!(table.verify.tiers["0"].commands[0].stack, "node");
        assert_eq!(table.verify.tiers["1"].commands[0].argv, ["custom"]);
        assert!(!table.verify.tiers.contains_key("2"));
        assert!(!table.verify.tiers.contains_key("3"));
        assert_eq!(
            table.provenance()["verify.tiers.0.commands"],
            Layer::Detected
        );
        assert_eq!(
            table.provenance()["verify.tiers.1.commands"],
            Layer::Project
        );
    }

    #[test]
    fn builtin_verify_tiers_are_neutral_and_base_is_inferred() {
        let builtin = PortTable::builtin();
        assert!(builtin.verify.base.is_empty());
        for tier in builtin.verify.tiers.values() {
            for command in &tier.commands {
                assert!(
                    command.argv.iter().all(|arg| !arg.contains("scripts/")),
                    "{command:?}"
                );
                assert_eq!(command.argv.first().map(String::as_str), Some("cargo"));
            }
        }
    }

    #[test]
    fn verify_base_inference_uses_the_remote_head_symref_target() {
        let inferred = infer_verify_base("origin", "refs/remotes/origin/master").unwrap();
        assert_eq!(inferred.0, "origin/master");
        assert_eq!(inferred.1, "refs/remotes/origin/HEAD");
        let inferred = infer_verify_base("upstream", "refs/remotes/upstream/main").unwrap();
        assert_eq!(inferred.0, "upstream/main");
        assert_eq!(inferred.1, "refs/remotes/upstream/HEAD");
        assert!(infer_verify_base("origin", "").is_none());
        assert!(infer_verify_base("origin", "refs/heads/main").is_none());
        assert!(infer_verify_base("", "refs/remotes/origin/main").is_none());
        assert!(
            verify_base_unset_error()
                .to_string()
                .contains(".provenant/ports.json")
        );
    }

    #[test]
    fn command_digest_binds_argv_and_each_exit_disposition() {
        let base = PortCommand {
            argv: vec!["admit".into(), "--phase".into()],
            exit_codes: CommandExitCodes {
                success: vec![0],
                unknown: vec![75],
                other: ExitDisposition::Refused,
            },
        };
        assert_eq!(base.digest(), base.digest());
        let mut variants = Vec::new();
        let mut changed = base.clone();
        changed.argv[0] = "another-admit".into();
        variants.push(changed);
        let mut changed = base.clone();
        changed.exit_codes.success = vec![0, 2];
        variants.push(changed);
        let mut changed = base.clone();
        changed.exit_codes.unknown = vec![75, 76];
        variants.push(changed);
        let mut changed = base.clone();
        changed.exit_codes.other = ExitDisposition::Unknown;
        variants.push(changed);
        assert!(
            variants
                .iter()
                .all(|command| command.digest() != base.digest())
        );
    }

    #[test]
    fn every_declared_port_accepts_layered_overrides_without_relabelling_omissions() {
        let temp = tempfile::tempdir().unwrap();
        let host = temp.path().join("host");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&host).unwrap();
        std::fs::create_dir_all(project.join(".provenant")).unwrap();
        let builtin = serde_json::to_value(PortTable::builtin()).unwrap();

        for &name in PortTable::PORT_NAMES {
            for serialised_source in [None, Some(Layer::Default)] {
                let mut entry = builtin[name].clone();
                if name != "build" {
                    // build has no backend field; its layer override is prepare.
                    entry["backend"] = serde_json::json!({"kind": "none"});
                }
                let object = entry.as_object_mut().unwrap();
                object.remove("source");
                if let Some(source) = serialised_source {
                    object.insert("source".into(), serde_json::to_value(source).unwrap());
                }
                let mut overlay = serde_json::json!({"schema_version": PORTS_SCHEMA_VERSION});
                overlay[name] = entry;
                let bytes = serde_json::to_vec(&overlay).unwrap();
                std::fs::write(host.join("ports.json"), &bytes).unwrap();

                for (project_overlay, expected_source) in [
                    (None, Layer::Host),
                    (
                        Some(serde_json::json!({"schema_version": PORTS_SCHEMA_VERSION})),
                        Layer::Host,
                    ),
                    (Some(overlay.clone()), Layer::Project),
                ] {
                    if let Some(document) = &project_overlay {
                        std::fs::write(
                            project.join(PORTS_CONFIG),
                            serde_json::to_vec(document).unwrap(),
                        )
                        .unwrap();
                    }
                    let project_path = project_overlay.as_ref().map(|_| project.as_path());
                    let table = PortTable::read_for(project_path, &host).unwrap();
                    for (port, source, _, _, _) in table.entries() {
                        assert_eq!(
                            source,
                            if port == name {
                                expected_source
                            } else {
                                Layer::Default
                            },
                            "{port}: overlay for {name} must not relabel omitted ports"
                        );
                    }
                    let mut expected = builtin.clone();
                    expected[name] = overlay[name].clone();
                    expected[name]["source"] = serde_json::to_value(expected_source).unwrap();
                    if name == "build" {
                        // This overlay restates every build leaf, so each leaf
                        // legitimately reports the layer that declared it.
                        if let Some(sources) = expected["build"]["leaf_sources"].as_object_mut() {
                            for source in sources.values_mut() {
                                *source = serde_json::to_value(expected_source).unwrap();
                            }
                        }
                    }
                    assert_eq!(serde_json::to_value(&table).unwrap(), expected);
                }
            }
        }
    }

    #[test]
    fn build_defaults_are_slots_and_generic() {
        let table = PortTable::builtin();
        let build = &table.build;
        assert_eq!(build.source, Layer::Default);
        assert!(build.prepare.is_empty());
        assert_eq!(build.strategy, "slots");
        assert_eq!(build.offload, "auto");
        assert_eq!(build.overflow, "auto");
        assert_eq!(build.clean, "untracked");
        assert!(build.stacks.is_empty());
        assert_eq!(build.snapshot_retention, "7d");
        assert_eq!(build.priority, default_build_priority());
        assert_eq!(build.queue_aging, "120s");
        assert_eq!(build.prewarm_budget, 1);
        assert_eq!(build.canary_every, "24h");
        assert_eq!(build.slot_idle, "7d");
        assert_eq!(build.soak, BuildSoakPort::default());
        assert_eq!(build.strategy_name().value, BuildStrategyName::Slots);
        assert_eq!(build.overflow_policy().value, BuildOverflow::Auto);
        assert_eq!(build.clean_policy().value, BuildClean::Untracked);
        assert_eq!(build.offload_policy().value, BuildOffload::Auto);
        assert_eq!(build.priority_for("test").value, BuildPriority::Test);
        assert_eq!(build.queue_aging_duration().value, Duration::from_secs(120));
        let (_, source, backend, valid, reason) =
            table.entries().find(|entry| entry.0 == "build").unwrap();
        assert_eq!(source, Layer::Default);
        assert_eq!(backend, PortBackendKind::Builtin);
        assert!(valid);
        assert!(reason.is_none());
    }

    #[test]
    fn build_prepare_layers_project_over_host() {
        let temp = tempfile::tempdir().unwrap();
        let host = temp.path().join("host");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&host).unwrap();
        std::fs::create_dir_all(project.join(".provenant")).unwrap();
        let host_prepare = vec![vec!["node".to_owned(), "host-prepare.mjs".to_owned()]];
        let project_prepare = vec![vec!["node".to_owned(), "project-prepare.mjs".to_owned()]];

        std::fs::write(
            host.join("ports.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": PORTS_SCHEMA_VERSION,
                "build": {"prepare": host_prepare},
            }))
            .unwrap(),
        )
        .unwrap();

        // No project layer: the host value and its source survive.
        let table = PortTable::read_for(None, &host).unwrap();
        assert_eq!(table.build.prepare, host_prepare);
        assert_eq!(table.build.source, Layer::Host);

        // A project layer that sets build.prepare wins and reports its source.
        std::fs::write(
            project.join(PORTS_CONFIG),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": PORTS_SCHEMA_VERSION,
                "build": {"prepare": project_prepare},
            }))
            .unwrap(),
        )
        .unwrap();
        let table = PortTable::read_for(Some(project.as_path()), &host).unwrap();
        assert_eq!(table.build.prepare, project_prepare);
        assert_eq!(table.build.source, Layer::Project);

        assert_eq!(table.build.source_for_leaf("strategy"), Layer::Default);
        assert_eq!(table.build.source_for_leaf("offload"), Layer::Default);
        // Arrays replace rather than concatenate across layers.
        assert_eq!(table.build.prepare.len(), 1);

        // A project layer that omits build keeps the host declaration.
        std::fs::write(
            project.join(PORTS_CONFIG),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": PORTS_SCHEMA_VERSION,
            }))
            .unwrap(),
        )
        .unwrap();
        let table = PortTable::read_for(Some(project.as_path()), &host).unwrap();
        assert_eq!(table.build.prepare, host_prepare);
        assert_eq!(table.build.source, Layer::Host);

        // With no layer setting build at all, it stays builtin and empty.
        let empty_host = temp.path().join("empty-host");
        std::fs::create_dir_all(&empty_host).unwrap();
        let table = PortTable::read_for(None, &empty_host).unwrap();
        assert_eq!(table.build.source, Layer::Default);
        assert!(table.build.prepare.is_empty());
    }

    #[test]
    fn invalid_prepare_argv_is_invalid_port() {
        let mut table = PortTable::builtin();
        table.build.prepare.push(Vec::new());
        assert!(table.validate().is_err());
        let (_, _, _, valid, reason) = table.entries().find(|entry| entry.0 == "build").unwrap();
        assert!(!valid);
        assert!(reason.is_some());

        let mut table = PortTable::builtin();
        table.build.prepare.push(vec![String::new()]);
        assert!(table.validate().is_err());

        // A declared program is accepted; arguments after it are free-form.
        let mut table = PortTable::builtin();
        table.build.prepare.push(vec![
            "node".to_owned(),
            "tools/ensure-generated.mjs".to_owned(),
        ]);
        assert!(table.validate().is_ok());
    }

    fn checkout_with_key(temp: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let host = temp.join("host");
        let project = temp.join("project");
        std::fs::create_dir_all(&host).unwrap();
        std::fs::create_dir_all(project.join(".git")).unwrap();
        std::fs::create_dir_all(project.join(".provenant")).unwrap();
        let dir = crate::host_project_dir(&host, &project)
            .expect("a checkout resolves its host project directory");
        (host, project, dir)
    }

    #[test]
    fn host_project_layer_order_for_ports() {
        let temp = tempfile::tempdir().unwrap();
        let (host, project, keyed) = checkout_with_key(temp.path());
        std::fs::create_dir_all(&keyed).unwrap();

        // builtin < host < host-project < project: each leaf overrides the
        // layer below it, and the leaf reports the layer that set it.
        std::fs::write(
            host.join("ports.json"),
            serde_json::to_vec(&serde_json::json!({
                "verify": {"base": "host/main", "remote": "host-remote"},
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            keyed.join("ports.json"),
            serde_json::to_vec(&serde_json::json!({
                "verify": {"base": "host-project/main"},
                "build": {"strategy": "slots"},
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            project.join(PORTS_CONFIG),
            serde_json::to_vec(&serde_json::json!({
                "verify": {"base": "project/main"},
            }))
            .unwrap(),
        )
        .unwrap();

        let table = PortTable::read_for(Some(&project), &host).unwrap();
        assert_eq!(table.verify.base, "project/main");
        assert_eq!(table.verify.remote, "host-remote");
        assert_eq!(table.build.strategy, "slots");

        let provenance = table.provenance();
        assert_eq!(provenance.get("verify.base"), Some(&Layer::Project));
        assert_eq!(provenance.get("verify.remote"), Some(&Layer::Host));
        assert_eq!(provenance.get("build.strategy"), Some(&Layer::HostProject));
        assert_eq!(provenance.get("build.prepare"), Some(&Layer::Default));
        assert_eq!(
            provenance.get("verify.tiers.0.commands"),
            Some(&Layer::Default)
        );

        // Port labels agree with the leaves they own.
        assert_eq!(table.verify.source, Layer::Project);
        assert_eq!(table.build.source, Layer::HostProject);
        assert_eq!(table.land.source, Layer::Default);
    }

    #[test]
    fn provenance_reports_highest_layer_per_leaf() {
        let temp = tempfile::tempdir().unwrap();
        let (host, project, keyed) = checkout_with_key(temp.path());
        std::fs::create_dir_all(&keyed).unwrap();
        std::fs::write(
            host.join("ports.json"),
            serde_json::to_vec(&serde_json::json!({
                "build": {"prepare": [["host-prepare"]], "offload": "remote"},
                "verify": {"base": "host/main"},
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            keyed.join("ports.json"),
            serde_json::to_vec(&serde_json::json!({
                "build": {"prepare": [["host-project-prepare"]]},
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            project.join(PORTS_CONFIG),
            serde_json::to_vec(&serde_json::json!({
                "build": {"clean": "none"},
            }))
            .unwrap(),
        )
        .unwrap();

        let table = PortTable::read_for(Some(&project), &host).unwrap();
        let provenance = table.provenance();
        assert_eq!(provenance.get("build.prepare"), Some(&Layer::HostProject));
        assert_eq!(provenance.get("build.offload"), Some(&Layer::Host));
        assert_eq!(provenance.get("build.clean"), Some(&Layer::Project));
        assert_eq!(provenance.get("build.strategy"), Some(&Layer::Default));
        assert_eq!(provenance.get("verify.base"), Some(&Layer::Host));
        assert_eq!(table.build.source, Layer::Project);

        // The table's full provenance remains derived state; only the frozen
        // build context carries the per-build-leaf map needed by lane jobs.
        let encoded = serde_json::to_value(&table).unwrap();
        assert!(encoded.get("provenance").is_none());
        assert!(encoded["build"]["leaf_sources"].is_object());
        assert_eq!(
            serde_json::from_value::<PortTable>(encoded)
                .unwrap()
                .provenance()
                .len(),
            0
        );
    }

    // Spec: docs/specs/state.md#every-setting-reports-its-layer
    #[test]
    fn sourced_leaves_report_the_winner_origin_and_shadowed_layers() {
        let temp = tempfile::tempdir().unwrap();
        let (host, project, keyed) = checkout_with_key(temp.path());
        std::fs::create_dir_all(&keyed).unwrap();
        std::fs::write(
            host.join("ports.json"),
            serde_json::to_vec(&serde_json::json!({
                "verify": {"remote": "host-remote"},
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            project.join(PORTS_CONFIG),
            serde_json::to_vec(&serde_json::json!({
                "verify": {"remote": "project-remote"},
            }))
            .unwrap(),
        )
        .unwrap();

        let table = PortTable::read_for(Some(&project), &host).unwrap();
        let candidates = table.sourced_leaf("verify.remote");
        // The compiled default, the host declaration and the project override.
        assert_eq!(candidates.len(), 3);
        let winner = candidates.last().unwrap();
        assert_eq!(winner.value, serde_json::json!("project-remote"));
        assert_eq!(winner.source.layer, Layer::Project);
        assert_eq!(
            winner.source.origin.as_deref(),
            Some(".provenant/ports.json#verify.remote")
        );
        let shadowed: Vec<_> = candidates[..candidates.len() - 1]
            .iter()
            .map(|candidate| (&candidate.value, candidate.source.layer))
            .collect();
        assert_eq!(shadowed[1].0, &serde_json::json!("host-remote"));
        assert_eq!(shadowed[1].1, Layer::Host);
        assert_eq!(shadowed[0].1, Layer::Default);
        assert!(table.sourced_leaf("absent.leaf").is_empty());
    }

    #[test]
    fn typed_build_values_warn_with_leaf_source_and_fall_back_to_builtins() {
        let mut build = PortTable::builtin().build;
        build.strategy = "future_strategy".into();
        build.overflow = "future_overflow".into();
        build.clean = "future_clean".into();
        build.queue_aging = "tomorrow".into();
        build
            .leaf_sources
            .insert("build.strategy".into(), Layer::Project);
        build
            .leaf_sources
            .insert("build.overflow".into(), Layer::Host);
        build
            .leaf_sources
            .insert("build.clean".into(), Layer::Project);
        build
            .leaf_sources
            .insert("build.queue_aging".into(), Layer::Host);

        let strategy = build.strategy_name();
        assert_eq!(strategy.value, BuildStrategyName::Slots);
        assert_eq!(strategy.source, Layer::Default);
        assert!(strategy.warnings[0].contains("build.strategy"));
        assert!(strategy.warnings[0].contains("project"));

        let overflow = build.overflow_policy();
        assert_eq!(overflow.value, BuildOverflow::Auto);
        assert!(overflow.warnings[0].contains("host"));

        let clean = build.clean_policy();
        assert_eq!(clean.value, BuildClean::Untracked);
        assert!(clean.warnings[0].contains("project"));

        let aging = build.queue_aging_duration();
        assert_eq!(aging.value, Duration::from_secs(120));
        assert!(aging.warnings[0].contains("build.queue_aging"));
        assert!(aging.warnings[0].contains("host"));
    }

    #[test]
    fn host_project_absent_is_silent() {
        let temp = tempfile::tempdir().unwrap();
        let host = temp.path().join("host");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&host).unwrap();
        std::fs::create_dir_all(project.join(".provenant")).unwrap();
        std::fs::write(
            host.join("ports.json"),
            serde_json::to_vec(&serde_json::json!({"build": {"strategy": "slots"}})).unwrap(),
        )
        .unwrap();

        // No key (a directory with no checkout marker) skips the layer.
        let table = PortTable::read_for(Some(&project), &host).unwrap();
        assert_eq!(table.build.strategy, "slots");
        assert_eq!(table.build.source, Layer::Host);
        assert!(
            table
                .provenance()
                .values()
                .all(|source| *source != Layer::HostProject)
        );

        // A key with no layer directory also skips it silently.
        std::fs::create_dir_all(project.join(".git")).unwrap();
        let table = PortTable::read_for(Some(&project), &host).unwrap();
        assert_eq!(table.build.strategy, "slots");
        assert_eq!(table.build.source, Layer::Host);
        assert!(
            table
                .provenance()
                .values()
                .all(|source| *source != Layer::HostProject)
        );
        std::fs::write(host.join("projects"), "not a directory").unwrap();
        let table = PortTable::read_for(Some(&project), &host).unwrap();
        assert_eq!(table.build.strategy, "slots");
        assert_eq!(table.build.source, Layer::Host);
    }

    #[test]
    fn project_key_moved_and_reexported_is_stable() {
        let fixed = Path::new("/repo/project/.git");
        assert_eq!(
            project_key(fixed),
            "e76e734a733dbce36a72e56fc8d5cff62d989351bb7e0aa357eb627686706354"
        );
        assert_eq!(project_key(fixed), project_key(fixed));
    }

    #[test]
    fn port_declaration_warnings_rule_over_every_port_leaf() {
        let temp = tempfile::tempdir().unwrap();
        let (host, project, binding) = checkout_with_key(temp.path());
        let config = project.join(".provenant/config.toml");
        let config_text = config.display().to_string();

        // A clean config.toml declares no port leaves, so nothing is warned.
        std::fs::write(&config, "# no port declarations here\n").unwrap();
        assert!(
            port_declaration_warnings(Some(&project), &host).is_empty(),
            "a config with no port declarations must not warn"
        );

        // Rule: every compiled PortTable leaf, placed in config.toml, warns
        // naming its own dotted key and no other. The leaves come from the
        // declaration, so a new port key is covered without editing this test.
        let table = serde_json::to_value(PortTable::builtin()).unwrap_or_default();
        let mut leaves = Vec::new();
        port_leaf_paths(&table, "", &mut leaves);
        assert!(!leaves.is_empty(), "the compiled port table has leaves");
        for leaf in &leaves {
            std::fs::write(&config, format!("{leaf} = true\n")).unwrap();
            let warnings = port_declaration_warnings(Some(&project), &host);
            let config_warnings: Vec<_> = warnings
                .iter()
                .filter(|warning| warning.contains(&config_text))
                .collect();
            assert!(
                config_warnings
                    .iter()
                    .any(|warning| warning.contains(leaf.as_str())),
                "leaf {leaf} did not warn: {warnings:?}"
            );
            assert_eq!(
                config_warnings.len(),
                1,
                "leaf {leaf} should warn exactly once: {config_warnings:?}"
            );
        }

        // An inline table is a declaration too, and must warn on its leaf.
        std::fs::write(&config, "build = { prepare = [[\"node\", \"gen.mjs\"]] }\n").unwrap();
        let warnings = port_declaration_warnings(Some(&project), &host);
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains(&config_text) && warning.contains("build.prepare")),
            "{warnings:?}"
        );
        assert_eq!(
            PortTable::read_for(Some(&project), &host)
                .unwrap()
                .build
                .prepare,
            Vec::<Vec<String>>::new(),
            "a misplaced declaration is advisory and leaves the table untouched"
        );

        // A host-project layer under the obsolete 64-digit target-seed key
        // names the file and the keys it actually holds, not a hard-coded leaf,
        // and points at the binding directory as the correct path.
        std::fs::write(&config, "# no port declarations here\n").unwrap();
        let key = project_key_for(&project).unwrap();
        let obsolete = host.join("projects").join(&key);
        std::fs::create_dir_all(&obsolete).unwrap();
        std::fs::write(
            obsolete.join("ports.json"),
            serde_json::to_vec(&serde_json::json!({
                "build": {"prepare": [["node", "tools/ensure-generated.mjs"]]},
                "verify": {"base": "origin/main"},
            }))
            .unwrap(),
        )
        .unwrap();
        let warnings = port_declaration_warnings(Some(&project), &host);
        assert!(
            warnings.iter().any(|warning| {
                warning.contains("ports.json")
                    && warning.contains(&key)
                    && warning.contains(&binding.display().to_string())
                    && warning.contains("build.prepare")
                    && warning.contains("verify.base")
            }),
            "{warnings:?}"
        );

        // The binding directory is the correct path: a layer there has no
        // misplacement warning and is read.
        std::fs::create_dir_all(&binding).unwrap();
        std::fs::rename(obsolete.join("ports.json"), binding.join("ports.json")).unwrap();
        let warnings = port_declaration_warnings(Some(&project), &host);
        let misplaced_path = obsolete.join("ports.json").display().to_string();
        assert!(
            !warnings
                .iter()
                .any(|warning| warning.contains(&misplaced_path)),
            "{warnings:?}"
        );
        assert_eq!(
            PortTable::read_for(Some(&project), &host)
                .unwrap()
                .build
                .source,
            Layer::HostProject
        );
    }

    /// Spec: docs/specs/cli.md#warnings-are-typed-values
    #[test]
    fn an_older_schema_warning_uses_the_sink_and_names_the_file_and_change() {
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir(project.path().join(".provenant")).unwrap();
        let file = project.path().join(PORTS_CONFIG);
        std::fs::write(&file, r#"{"schema_version":2}"#).unwrap();

        let (_, warnings) = capture(|| {
            PortTable::read_for_inspection(Some(project.path()), project.path()).unwrap()
        });
        let warning = warnings
            .iter()
            .find(|warning| warning.code.as_ref() == CONFIG_MALFORMED.as_str())
            .expect("typed schema warning");
        let path = file.display().to_string();
        assert!(warning.message.contains(&path), "{:?}", warning.message);
        assert!(warning.message.contains("found 2"), "{:?}", warning.message);
        let repair = warning.repair.as_deref().expect("repair");
        assert!(repair.contains(&path), "{repair}");
        assert!(
            repair.contains(&format!("schema_version\": {PORTS_SCHEMA_VERSION}")),
            "{repair}"
        );
    }

    /// Spec: docs/specs/state.md#format-changes-migrate-once-at-install
    #[test]
    fn a_retired_source_spelling_is_refused_with_the_new_spelling() {
        for (json, key) in [
            (
                r#"{"schema_version":3,"verify":{"source":"builtin"}}"#,
                "verify.source",
            ),
            (
                r#"{"schema_version":3,"build":{"leaf_sources":{"build.strategy":"builtin"}}}"#,
                "build.leaf_sources.build.strategy",
            ),
        ] {
            let project = tempfile::tempdir().unwrap();
            std::fs::create_dir(project.path().join(".provenant")).unwrap();
            let file = project.path().join(PORTS_CONFIG);
            std::fs::write(&file, json).unwrap();

            let (_, warnings) = capture(|| {
                PortTable::read_for_inspection(Some(project.path()), project.path()).unwrap()
            });
            let warning = warnings
                .iter()
                .find(|warning| warning.message.contains("builtin"))
                .expect("the retired spelling was refused with a typed warning");
            let path = file.display().to_string();
            assert!(warning.message.contains(&path), "{:?}", warning.message);
            assert!(warning.message.contains(key), "{:?}", warning.message);
            assert!(
                warning
                    .message
                    .contains(&format!("\"{}\"", Layer::Default.as_str())),
                "{:?}",
                warning.message
            );
            assert_eq!(warning.code.as_ref(), CONFIG_MALFORMED.as_str());
        }
    }
}
