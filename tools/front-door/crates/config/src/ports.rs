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
/// An empty `remote`, `default_branch` or `protected` means "detect":
/// [`crate::forge::ForgeContext`] resolves the value from the repository when
/// no layer declared it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitPort {
    pub source: Layer,
    pub backend: PortBackend,
    /// The remote name. Empty means detect it from the branch upstream or the
    /// sole remote.
    pub remote: String,
    /// The default branch name. Empty means resolve it from
    /// `refs/remotes/<remote>/HEAD`, then the forge API. A declaration wins
    /// ahead of both detections.
    pub default_branch: String,
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
    /// The command approval carry runs to prove a delta is formatting-only.
    /// An absent leaf is filled by stack detection; `"none"` turns the carry
    /// off. A declared command is run exactly as written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<VerifyFormat>,
    /// Tier names are repository-defined strings, not a closed numeric enum.
    pub tiers: BTreeMap<String, VerifyTier>,
    pub default_tier: String,
    pub integrations: Vec<Integration>,
    pub runners: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub affected: VerifyAffectedPort,
    /// Commands and their scope for a writer lane, declared by the project.
    #[serde(default)]
    pub lane: Vec<VerifyLaneCheck>,
    /// Cheap repository checks a writer runs before pushing. They need no
    /// build and supplement the declared lane verification commands.
    /// Invalid entries are dropped with a typed warning at the writer preamble,
    /// not refused here: they are advice to the writer, not a launch gate.
    #[serde(default)]
    pub static_checks: Vec<VerifyStaticCheck>,
}

/// One lane verification command, with its tier and applicability.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyLaneCheck {
    pub tier: String,
    pub argv: Vec<String>,
    pub scope: String,
}

impl VerifyLaneCheck {
    pub fn is_valid(&self) -> bool {
        !self.tier.trim().is_empty()
            && !self.scope.trim().is_empty()
            && self.argv.first().is_some_and(|program| !program.is_empty())
    }
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

/// The command approval carry runs to prove a delta is formatting-only. It
/// takes the same string or table forms as a [`TierCommand`], plus the literal
/// string `"none"`, which turns format carry off. When no layer declares the
/// leaf, stack detection supplies a command (see [`crate::detect::detect`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum VerifyFormat {
    /// Explicitly turned off with `"format": "none"`.
    #[default]
    None,
    /// Run exactly this command from the verify workspace.
    Command(TierCommand),
}

impl Serialize for VerifyFormat {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::None => serializer.serialize_str("none"),
            Self::Command(command) => command.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for VerifyFormat {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        if value.as_str() == Some("none") {
            return Ok(Self::None);
        }
        serde_json::from_value(value)
            .map(Self::Command)
            .map_err(serde::de::Error::custom)
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

    /// Resolve a branch's allocation to the directory that anchors its declared
    /// root and the normal tail beneath that root. Roots may be absolute or
    /// parent-relative; `base` is the checkout (or common Git directory) those
    /// declarations are relative to. The anchor is returned as a path expression
    /// (`base.join(root)`), which callers canonicalise before opening it.
    pub fn allocation_for(&self, base: &Path, branch: &str) -> io::Result<(PathBuf, PathBuf)> {
        let rendered = self.path_for_branch(branch)?;
        let root = self
            .roots
            .iter()
            .filter(|root| rendered.starts_with(root))
            .max_by_key(|root| root.components().count())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid worktree template")
            })?;
        let tail = rendered
            .strip_prefix(root)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid worktree template"))?;
        Ok((base.join(root), tail.to_path_buf()))
    }

    /// Identify the branch of a worktree at an absolute `path` lying under any
    /// declared root resolved against `base`. This is the absolute-path
    /// counterpart of [`Self::branch_component`] and treats an outside root
    /// exactly like the default one.
    pub fn branch_for_path<'a>(&self, base: &Path, path: &'a Path) -> Option<&'a str> {
        if !path.is_absolute() || !matches!(self.backend, PortBackend::Builtin) {
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
            let resolved = resolve_declared_path(base, root)?;
            let tail = path.strip_prefix(&resolved).ok()?.to_str()?;
            let branch = tail.strip_prefix(prefix)?.strip_suffix(suffix)?;
            (!branch.is_empty() && !branch.contains('/')).then_some(branch)
        })
    }

    /// Return the branch slug matched by the declared default template under
    /// any configured root. Secondary roots reuse that same template tail.
    pub fn branch_component<'a>(&self, relative: &'a Path) -> Option<&'a str> {
        if !declared_layout_path(relative) || !matches!(self.backend, PortBackend::Builtin) {
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
fn default_build_target() -> String {
    "toolchain,worktree".to_owned()
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
    /// Comma-separated identities for the slot's warm Cargo target.
    /// Defaults to `toolchain,worktree`; `shared` keeps the legacy shared target.
    #[serde(default = "default_build_target")]
    pub target: String,
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
/// One identity component in the slot target directory key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildTargetKey {
    /// The resolved `rustc -vV` toolchain, never the ambiguous channel name.
    Toolchain,
    /// The source checkout identity, so workspace members are never reused
    /// across worktrees.
    Worktree,
}
impl BuildTargetKey {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Toolchain => "toolchain",
            Self::Worktree => "worktree",
        }
    }
}
pub const DEFAULT_BUILD_TARGET_KEYS: [BuildTargetKey; 2] =
    [BuildTargetKey::Toolchain, BuildTargetKey::Worktree];
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

    /// Resolve `build.target` into ordered, deduplicated target identities.
    /// `shared` (or an empty value) keeps the legacy shared target.
    /// Unknown components warn and fall back to the builtin key list.
    pub fn target_layout(&self) -> ResolvedBuildValue<Vec<BuildTargetKey>> {
        let raw = self.target.trim();
        let (keys, invalid) = if raw.is_empty() || raw == "shared" {
            (Vec::new(), None)
        } else {
            let mut keys: Vec<BuildTargetKey> = Vec::new();
            let mut invalid = None;
            for part in raw.split(',') {
                let part = part.trim();
                let key = match part {
                    "toolchain" => Some(BuildTargetKey::Toolchain),
                    "worktree" => Some(BuildTargetKey::Worktree),
                    "" => {
                        invalid = Some("an empty component".to_owned());
                        None
                    }
                    other => {
                        invalid = Some(format!("unknown component {other:?}"));
                        None
                    }
                };
                if let Some(key) = key
                    && !keys.contains(&key)
                {
                    keys.push(key);
                }
            }
            (keys, invalid)
        };
        match invalid {
            None => ResolvedBuildValue {
                value: keys,
                source: self.source_for_leaf("target"),
                warnings: Vec::new(),
            },
            Some(detail) => self.invalid(
                "target",
                &self.target,
                DEFAULT_BUILD_TARGET_KEYS.to_vec(),
                &format!("{detail}; use toolchain, worktree or shared"),
            ),
        }
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
            "target",
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
            && (self.default_branch.is_empty()
                || (relative(Path::new(&self.default_branch), false)
                    && !self.default_branch.contains(BRANCH_TOKEN)))
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
            && self.roots.iter().all(|root| {
                declared_layout_path(root) && !root.to_string_lossy().contains(BRANCH_TOKEN)
            })
            && self
                .roots
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                == self.roots.len()
            && declared_layout_path(&self.default)
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

/// A path a layout root or default template may declare. A root may be
/// absolute or parent-relative so worktrees can live beside or away from the
/// checkout; an empty component, a `.` component or NUL is never a valid
/// declaration. This is the one validator for paths that may leave the checkout.
fn declared_layout_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.as_os_str().as_encoded_bytes().contains(&0)
        && path
            .components()
            .enumerate()
            .all(|(index, component)| match component {
                Component::Prefix(_) => index == 0,
                Component::RootDir => index == 0 || index == 1,
                Component::ParentDir | Component::Normal(_) => true,
                Component::CurDir => false,
            })
}

/// Resolve a declared layout path against its anchor, removing `.` and `..`
/// lexically. `anchor` is absolute in production; a parent that escapes a
/// relative anchor is refused. Used to compare an absolute worktree path with
/// an outside root declared as `../sibling`.
fn resolve_declared_path(anchor: &Path, declared: &Path) -> Option<PathBuf> {
    let joined = if declared.is_absolute() {
        declared.to_path_buf()
    } else {
        anchor.join(declared)
    };
    let absolute = joined.is_absolute();
    let mut parts: Vec<OsString> = Vec::new();
    for component in joined.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => parts.push(name.to_os_string()),
            Component::ParentDir => match parts.last() {
                Some(last) if last.as_os_str() != std::ffi::OsStr::new("..") => {
                    parts.pop();
                }
                _ if absolute => return None,
                _ => parts.push(OsString::from("..")),
            },
        }
    }
    let mut resolved = PathBuf::new();
    if absolute {
        resolved.push("/");
    }
    for part in parts {
        resolved.push(part);
    }
    Some(resolved)
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
                    let candidates = self.provenance.iter()
                        .filter(|(path, _)| *path == name || path.starts_with(&format!("{name}.")))
                        .map(|(_, source)| Sourced {
                            value: *source,
                            source: Source { layer: *source, origin: None },
                        });
                    let merge = PORT_PRECEDENCE.merge(candidates);
                    provenant_records::warn_ignored(&merge.ignored, "port table");
                    self.$field.set_source(
                        merge.winner.map(|winner| winner.value).unwrap_or(Layer::Default),
                    );
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
    // A declared formatter is one setting whether written as a scalar command
    // or as a command object. Record the whole value at `verify.format` so its
    // source names the declaring layer, not an inner field's; detection records
    // the same leaf. Spec: docs/specs/state.md#every-setting-reports-its-layer
    if prefix == "verify.format" {
        walk.record(
            prefix,
            layer,
            &format!("{origin_base}#{prefix}"),
            value.clone(),
        );
        return;
    }
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

    /// The format command for approval carry, with the exact source that
    /// declared it. An absent leaf resolves to the built-in `none`, so a caller
    /// can tell an explicit refusal from a command it may run.
    /// Spec: docs/specs/state.md#every-setting-reports-its-layer
    pub fn verify_format(&self) -> Sourced<VerifyFormat> {
        let value = self.verify.format.clone().unwrap_or(VerifyFormat::None);
        let source = self
            .sourced_leaf("verify.format")
            .last()
            .map(|candidate| candidate.source.clone())
            .unwrap_or(Source {
                layer: Layer::Default,
                origin: None,
            });
        Sourced { value, source }
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
            .map(|declared| {
                resolve_declared_path(root, declared).unwrap_or_else(|| root.join(declared))
            })
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
                let detected_format = detected.format.clone();
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
                if let Some(command) = detected_format {
                    let value = serde_json::to_value(&command).map_err(io::Error::other)?;
                    document["verify"]["format"] = value.clone();
                    walk.record(
                        "verify.format",
                        Layer::Detected,
                        "detected#verify.format",
                        value,
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
        // The layer walk's derived state must be installed before the rebase
        // check: that check reads the winning layer of the `verify.format`
        // leaf, which lives in the leaf history. Spec: docs/specs/state.md#every-setting-reports-its-layer
        table.provenance = walk.layers;
        table.sourced_leaves = walk.candidates;
        // Detection declares project-root paths; commands run from the verify
        // workspace. Rebase a detected format command under the workspace that
        // won, so the carry starts where detection found the stack.
        if table
            .sourced_leaf("verify.format")
            .last()
            .is_some_and(|winner| winner.source.layer == Layer::Detected)
            && let Some(VerifyFormat::Command(command)) = &mut table.verify.format
        {
            let normal = |path: &Path| -> PathBuf {
                let parts: PathBuf = path
                    .components()
                    .filter(|part| !matches!(part, Component::CurDir))
                    .collect();
                if parts.as_os_str().is_empty() {
                    PathBuf::from(".")
                } else {
                    parts
                }
            };
            let cwd = normal(&command.cwd);
            let workspace = normal(&table.verify.workspace);
            if workspace != Path::new(".") {
                command.cwd = if cwd == workspace {
                    PathBuf::from(".")
                } else {
                    cwd.strip_prefix(&workspace)
                        .map(Path::to_path_buf)
                        .unwrap_or(cwd)
                };
            }
        }
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
#[path = "ports_tests.rs"]
mod tests;
