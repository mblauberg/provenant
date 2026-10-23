//! Runtime capability discovery for the configured provider adapters.
//!
//! The original command owns both provider-specific discovery and its shared
//! process/output plumbing. This port keeps provider differences in a table of
//! small adapters and makes the process host and output writer explicit seams.
use crate::{
    casefold::fold,
    ordered_json::{OrderedObject, OrderedValue},
    probe::load_json,
};
use provenant_core::py::{python_repr, python_splitlines, python_strip};
use provenant_records::DecodeError as CheckedJsonError;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::Duration,
};
use thiserror::Error;

pub const OUTPUT_LIMIT_BYTES: usize = 1_048_576;
const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];
const EFFORT_CHOICES: &[&str] = &["high", "low", "max", "medium", "xhigh"];
const AGY_EFFORTS: &[&str] = &["low", "medium", "high"];
const USAGE: &str = "usage: capabilities.py [-h] [--out OUT] [--bin BIN] [--alias ALIAS]\n                       [--effort {high,low,max,medium,xhigh}]\n                       {agy,claude,codex}";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessRequest {
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub timeout: Duration,
    pub output_limit_bytes: usize,
    pub merge_stderr: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessResult {
    pub returncode: i32,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

/// Bounded process execution is owned by the caller (the routing crate cannot
/// depend on the supervisor). Implementations must honor every request bound.
pub trait CapabilityProcessHost {
    fn run_bounded(&mut self, request: &ProcessRequest) -> Result<ProcessResult, String>;
}

/// Captured output is routed through one writer so CLI and file effects can be
/// tested without touching the host filesystem or terminal.
pub trait CapabilityOutputWriter {
    fn stdout(&mut self, text: &str) -> Result<(), String>;
    fn stderr(&mut self, text: &str) -> Result<(), String>;
    fn write_file(&mut self, path: &Path, text: &str, owner_only: bool) -> Result<(), String>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureArgs {
    pub provider: String,
    pub out: Option<PathBuf>,
    pub binary: Option<String>,
    pub alias: Option<String>,
    pub effort: Option<String>,
}

#[derive(Debug, Error)]
pub enum CapabilityError {
    #[error("{0}")]
    Discovery(String),
    #[error("{0}")]
    Output(String),
    #[error(transparent)]
    Codec(#[from] CheckedJsonError),
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("{message}")]
pub struct ArgumentError {
    message: String,
}

impl From<String> for ArgumentError {
    fn from(message: String) -> Self {
        Self { message }
    }
}

impl From<&str> for ArgumentError {
    fn from(message: &str) -> Self {
        Self {
            message: message.to_owned(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct DiscoveryContext<'a> {
    /// Caller-provided UTC ISO timestamp, replacing datetime.now in the source.
    pub observed_at: &'a str,
    /// Equivalent to pathlib.Path.cwd() in the source command.
    pub cwd: &'a Path,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probe {
    pub stdout: String,
    pub stderr: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderSpec {
    pub name: &'static str,
    pub default_bin: &'static str,
    pub needs_route: bool,
    pub output_required: bool,
    pub restrict_output: bool,
}

/// Provider implementations own only command construction and normalization;
/// the registry and capture path own all shared validation and rendering.
pub trait ProviderAdapter: Sync {
    fn spec(&self) -> ProviderSpec;
    fn discover(
        &self,
        binary: &str,
        args: &CaptureArgs,
        host: &mut dyn CapabilityProcessHost,
        context: &DiscoveryContext<'_>,
    ) -> Result<(OrderedValue, Option<String>), CapabilityError>;
}

struct AgyProvider;
struct ClaudeProvider;
struct CodexProvider;

static AGY: AgyProvider = AgyProvider;
static CLAUDE: ClaudeProvider = ClaudeProvider;
static CODEX: CodexProvider = CodexProvider;

/// The table is the single source for supported provider names and behavior.
pub static PROVIDERS: [&dyn ProviderAdapter; 3] = [&AGY, &CLAUDE, &CODEX];

impl ProviderAdapter for CodexProvider {
    fn spec(&self) -> ProviderSpec {
        ProviderSpec {
            name: "codex",
            default_bin: "codex",
            needs_route: false,
            output_required: false,
            restrict_output: false,
        }
    }

    fn discover(
        &self,
        binary: &str,
        _args: &CaptureArgs,
        host: &mut dyn CapabilityProcessHost,
        context: &DiscoveryContext<'_>,
    ) -> Result<(OrderedValue, Option<String>), CapabilityError> {
        let label = "codex debug models";
        let probe = run_cli(
            host,
            vec![binary.into(), "debug".into(), "models".into()],
            Duration::from_secs(10),
            label,
            context.cwd,
        )?;
        let raw = load_provider_json(&probe.stdout)?;
        Ok((
            normalise_codex(&raw, context.observed_at)?,
            warning_from(&probe, label),
        ))
    }
}

impl ProviderAdapter for AgyProvider {
    fn spec(&self) -> ProviderSpec {
        ProviderSpec {
            name: "agy",
            default_bin: "agy",
            needs_route: false,
            output_required: false,
            restrict_output: false,
        }
    }

    fn discover(
        &self,
        binary: &str,
        _args: &CaptureArgs,
        host: &mut dyn CapabilityProcessHost,
        context: &DiscoveryContext<'_>,
    ) -> Result<(OrderedValue, Option<String>), CapabilityError> {
        let label = "agy models";
        let probe = run_cli(
            host,
            vec![binary.into(), "models".into()],
            Duration::from_secs(60),
            label,
            context.cwd,
        )?;
        Ok((
            normalise_agy(&probe.stdout, context.observed_at)?,
            warning_from(&probe, label),
        ))
    }
}

impl ProviderAdapter for ClaudeProvider {
    fn spec(&self) -> ProviderSpec {
        ProviderSpec {
            name: "claude",
            default_bin: "claude",
            needs_route: true,
            output_required: true,
            restrict_output: true,
        }
    }

    fn discover(
        &self,
        binary: &str,
        args: &CaptureArgs,
        host: &mut dyn CapabilityProcessHost,
        context: &DiscoveryContext<'_>,
    ) -> Result<(OrderedValue, Option<String>), CapabilityError> {
        let alias = args.alias.as_deref().unwrap_or_default();
        let effort = args.effort.as_deref().unwrap_or_default();
        let auth = claude_json(
            host,
            vec![binary.into(), "auth".into(), "status".into()],
            Duration::from_secs(5),
            context.cwd,
        )?;
        validate_claude_auth(&auth)?;
        let result = claude_json(
            host,
            vec![
                binary.into(),
                "-p".into(),
                "--safe-mode".into(),
                "--no-session-persistence".into(),
                "--permission-mode".into(),
                "plan".into(),
                "--tools".into(),
                "".into(),
                "--model".into(),
                alias.into(),
                "--effort".into(),
                effort.into(),
                "--output-format".into(),
                "json".into(),
                "Reply exactly OK.".into(),
            ],
            Duration::from_secs(30),
            context.cwd,
        )?;
        Ok((
            normalise_claude(&auth, &result, alias, effort, context.observed_at)?,
            None,
        ))
    }
}

fn provider(name: &str) -> Option<&'static dyn ProviderAdapter> {
    PROVIDERS
        .iter()
        .copied()
        .find(|candidate| candidate.spec().name == name)
}

fn discovery(message: impl Into<String>) -> CapabilityError {
    CapabilityError::Discovery(message.into())
}

fn load_provider_json(raw: &str) -> Result<OrderedValue, CapabilityError> {
    match load_json(raw) {
        Ok(value) => Ok(value),
        Err(error) => {
            // The source's object_pairs_hook exposes this one custom error
            // without parser location details. Keep that compatibility wording.
            let duplicate = match &error {
                CheckedJsonError::Json(source) => source.to_string(),
                CheckedJsonError::NumberOutOfRange(_) => String::new(),
            };
            if let Some((_, duplicate)) = duplicate.split_once("duplicate JSON member: ") {
                let key = duplicate.split(" at line ").next().unwrap_or(duplicate);
                Err(discovery(format!("duplicate JSON member: {key}")))
            } else {
                Err(CapabilityError::Codec(error))
            }
        }
    }
}

fn run_cli(
    host: &mut dyn CapabilityProcessHost,
    command: Vec<String>,
    timeout: Duration,
    label: &str,
    cwd: &Path,
) -> Result<Probe, CapabilityError> {
    let seconds = timeout.as_secs();
    let result = host
        .run_bounded(&ProcessRequest {
            command,
            cwd: cwd.to_path_buf(),
            timeout,
            output_limit_bytes: OUTPUT_LIMIT_BYTES,
            merge_stderr: false,
        })
        .map_err(discovery)?;
    let stderr = python_strip(&result.stderr).to_owned();
    if result.timed_out {
        let detail = if stderr.is_empty() {
            String::new()
        } else {
            format!(": {stderr}")
        };
        return Err(discovery(format!(
            "{label} timed out after {seconds} seconds{detail}"
        )));
    }
    if result.returncode != 0 {
        let detail = scrubbed_failure_detail(&result.stdout, &stderr);
        let detail = if detail.is_empty() {
            String::new()
        } else {
            format!(": {detail}")
        };
        return Err(discovery(format!(
            "{label} exited {}{detail}",
            result.returncode
        )));
    }
    Ok(Probe {
        stdout: result.stdout,
        stderr,
    })
}

fn claude_json(
    host: &mut dyn CapabilityProcessHost,
    command: Vec<String>,
    timeout: Duration,
    cwd: &Path,
) -> Result<OrderedValue, CapabilityError> {
    let probe = run_cli(host, command, timeout, "command", cwd)?;
    if probe.stderr.contains("Warning: Unknown --effort value") {
        return Err(discovery("Claude CLI rejected the requested effort"));
    }
    load_provider_json(&probe.stdout)
}

fn warning_from(probe: &Probe, label: &str) -> Option<String> {
    (!probe.stderr.is_empty()).then(|| format!("{label} warned: {}", probe.stderr))
}

fn scrubbed_failure_detail(stdout: &str, stderr: &str) -> String {
    let stdout = python_strip(stdout);
    let stderr = python_strip(stderr);
    let combined = if stderr.is_empty() {
        stdout.to_owned()
    } else if stdout.is_empty() {
        stderr.to_owned()
    } else {
        format!("{stderr}\n{stdout}")
    };
    let lower = combined.to_lowercase();
    if contains_org_access_denial(&lower) {
        return "provider access denied; subscription access disabled by organisation policy"
            .into();
    }
    if !stderr.is_empty() {
        return stderr.to_owned();
    }
    let lower_stdout = stdout.to_lowercase();
    ["401", "403", "429"]
        .iter()
        .filter_map(|status| {
            find_bounded_status(&lower_stdout, status).map(|index| (index, *status))
        })
        .min_by_key(|(index, _)| *index)
        .map(|(_, status)| format!("provider returned HTTP {status}"))
        .unwrap_or_default()
}

fn contains_org_access_denial(text: &str) -> bool {
    let disabled_after_access = text
        .match_indices("subscription access")
        .any(|(start, _)| text[start..].contains("disabled"));
    if disabled_after_access {
        return true;
    }
    ["organization", "organisation"].iter().any(|word| {
        text.match_indices(word).any(|(start, _)| {
            let after = &text[start + word.len()..];
            after
                .match_indices("disabled")
                .any(|(disabled, _)| after[disabled..].contains("subscription access"))
        })
    })
}

fn find_bounded_status(text: &str, status: &str) -> Option<usize> {
    text.match_indices(status).find_map(|(start, _)| {
        let before = text[..start].chars().next_back();
        let after = text[start + status.len()..].chars().next();
        (!before.is_some_and(|c| c.is_alphanumeric() || c == '_')
            && !after.is_some_and(|c| c.is_alphanumeric() || c == '_'))
        .then_some(start)
    })
}

/// Convert parsed codex catalogue input into the stable snapshot contract.
pub fn normalise_codex(
    raw: &OrderedValue,
    observed_at: &str,
) -> Result<OrderedValue, CapabilityError> {
    let entries = raw
        .get("models")
        .and_then(|value| match value {
            OrderedValue::Array(entries) => Some(entries),
            _ => None,
        })
        .ok_or_else(|| discovery("catalogue root must contain a models list"))?;
    let mut models = OrderedObject::default();
    let mut seen = BTreeSet::new();
    for item in entries {
        let item = item
            .as_object()
            .ok_or_else(|| discovery("catalogue model entry must be an object"))?;
        let slug = item
            .get("slug")
            .and_then(OrderedValue::as_str)
            .filter(|value| !python_strip(value).is_empty())
            .ok_or_else(|| discovery("catalogue model slug must be a non-empty string"))?;
        let levels = item
            .get("supported_reasoning_levels")
            .and_then(|value| match value {
                OrderedValue::Array(levels) => Some(levels),
                _ => None,
            })
            .ok_or_else(|| discovery("catalogue reasoning levels must be a list"))?;
        if levels.is_empty() {
            return Err(discovery("catalogue reasoning levels must not be empty"));
        }
        let mut efforts = Vec::with_capacity(levels.len());
        for level in levels {
            let level = level
                .as_object()
                .ok_or_else(|| discovery("catalogue reasoning-level entry must be an object"))?;
            let effort = level
                .get("effort")
                .and_then(OrderedValue::as_str)
                .filter(|value| !python_strip(value).is_empty())
                .ok_or_else(|| {
                    discovery("catalogue reasoning effort must be a non-empty string")
                })?;
            efforts.push(OrderedValue::String(effort.to_lowercase()));
        }
        let normalized_slug = fold(slug);
        if !seen.insert(normalized_slug.clone()) {
            return Err(discovery(
                "catalogue model slugs must be unique when case-folded",
            ));
        }
        let mut model = OrderedObject::default();
        model.insert("resolved_model".into(), OrderedValue::String(slug.into()));
        model.insert("supported_efforts".into(), OrderedValue::Array(efforts));
        models.insert(normalized_slug, OrderedValue::Object(model));
    }
    if models.0.is_empty() {
        return Err(discovery("catalogue contains no usable model entries"));
    }
    let mut snapshot = base_snapshot("codex debug models", observed_at);
    snapshot.insert("models".into(), OrderedValue::Object(models));
    Ok(OrderedValue::Object(snapshot))
}

/// Convert agy's dispatchable id listing into family and effort capabilities.
pub fn normalise_agy(raw: &str, observed_at: &str) -> Result<OrderedValue, CapabilityError> {
    let mut models = OrderedObject::default();
    for line in python_splitlines(raw) {
        if python_strip(line).is_empty() {
            continue;
        }
        let first = line.split_once('\t').map_or(line, |(model, _)| model);
        let model_id = python_strip(first);
        let (family, effort) = effort_suffix(model_id);
        let family = family.unwrap_or(model_id);
        if models.get(family).is_none() {
            let mut entry = OrderedObject::default();
            entry.insert("resolved_model".into(), OrderedValue::String(family.into()));
            entry.insert("supported_efforts".into(), OrderedValue::Array(Vec::new()));
            entry.insert("dispatchable_ids".into(), OrderedValue::Array(Vec::new()));
            models.insert(family.into(), OrderedValue::Object(entry));
        }
        let Some(OrderedValue::Object(entry)) = models.get(family).cloned() else {
            return Err(discovery("agy internal model table was malformed"));
        };
        let mut entry = entry;
        if let Some(OrderedValue::Array(ids)) = entry
            .0
            .iter_mut()
            .find(|(key, _)| key == "dispatchable_ids")
            .map(|(_, value)| value)
        {
            ids.push(OrderedValue::String(model_id.into()));
        }
        if let Some(effort) = effort {
            if let Some(OrderedValue::Array(efforts)) = entry
                .0
                .iter_mut()
                .find(|(key, _)| key == "supported_efforts")
                .map(|(_, value)| value)
            {
                if !efforts.iter().any(|value| value.as_str() == Some(effort)) {
                    efforts.push(OrderedValue::String(effort.into()));
                }
            }
        }
        models.insert(family.into(), OrderedValue::Object(entry));
    }
    if models.0.is_empty() {
        return Err(discovery("agy models returned no model ids"));
    }

    for (_, value) in &mut models.0 {
        let OrderedValue::Object(entry) = value else {
            return Err(discovery("agy internal model table was malformed"));
        };
        if let Some(OrderedValue::Array(efforts)) = entry
            .0
            .iter_mut()
            .find(|(key, _)| key == "supported_efforts")
            .map(|(_, value)| value)
        {
            efforts.sort_by_key(|effort| {
                AGY_EFFORTS
                    .iter()
                    .position(|candidate| effort.as_str() == Some(candidate))
                    .unwrap_or(AGY_EFFORTS.len())
            });
        }
        if let Some(OrderedValue::Array(ids)) = entry
            .0
            .iter_mut()
            .find(|(key, _)| key == "dispatchable_ids")
            .map(|(_, value)| value)
        {
            ids.sort_by(|left, right| {
                left.as_str()
                    .unwrap_or_default()
                    .cmp(right.as_str().unwrap_or_default())
            });
        }
    }

    let mut routable = OrderedObject::default();
    let mut effortless = Vec::new();
    let mut google = Vec::new();
    let mut foreign = Vec::new();
    for (family, entry) in &models.0 {
        let efforts = entry
            .get("supported_efforts")
            .and_then(|value| match value {
                OrderedValue::Array(items) => Some(items),
                _ => None,
            });
        if efforts.is_some_and(|items| !items.is_empty()) {
            routable.insert(family.clone(), entry.clone());
            if family.starts_with("gemini") {
                google.push(family.clone());
            }
        } else {
            effortless.push(family.clone());
        }
        if contains_foreign_family(family) {
            foreign.push(family.clone());
        }
    }
    if routable.0.is_empty() {
        return Err(discovery("agy models listed no effort-controllable model"));
    }
    let mut snapshot = base_snapshot("agy models", observed_at);
    snapshot.insert("models".into(), OrderedValue::Object(routable));
    snapshot.insert("effortless_models".into(), sorted_strings(effortless));
    snapshot.insert("google_models".into(), sorted_strings(google));
    snapshot.insert("foreign_models".into(), sorted_strings(foreign));
    Ok(OrderedValue::Object(snapshot))
}

/// Validate a Claude subscription canary and emit only scrubbed provenance.
pub fn normalise_claude(
    auth: &OrderedValue,
    result: &OrderedValue,
    alias: &str,
    effort: &str,
    observed_at: &str,
) -> Result<OrderedValue, CapabilityError> {
    let subscription = validate_claude_auth(auth)?;
    let usage = result.get("modelUsage").and_then(OrderedValue::as_object);
    if result.get("type").and_then(OrderedValue::as_str) != Some("result")
        || result.get("subtype").and_then(OrderedValue::as_str) != Some("success")
        || result.get("is_error") != Some(&OrderedValue::Bool(false))
        || result.get("result").and_then(OrderedValue::as_str) != Some("OK")
        || usage.is_none()
    {
        return Err(discovery(
            "Claude canary returned an ambiguous or unsuccessful result",
        ));
    }
    let normalized_alias = fold(alias);
    let matching_models: Vec<&str> = usage
        .into_iter()
        .flat_map(|items| items.0.iter())
        .filter_map(|(model, _)| {
            let normalized_model = fold(model);
            if !normalized_model.starts_with("claude-") {
                return None;
            }
            let matches = normalized_model == normalized_alias
                || (!normalized_alias.contains('-')
                    && normalized_model
                        .split('-')
                        .any(|part| part == normalized_alias));
            matches.then_some(model.as_str())
        })
        .collect();
    let resolved_model = match matching_models.as_slice() {
        [model] => *model,
        _ => {
            return Err(discovery(
                "Claude canary did not identify one primary runtime model",
            ));
        }
    };

    let mut provenance = OrderedObject::default();
    provenance.insert(
        "kind".into(),
        OrderedValue::String("subscription_runtime_canary".into()),
    );
    provenance.insert(
        "auth_method".into(),
        OrderedValue::String("claude.ai".into()),
    );
    provenance.insert(
        "subscription_type".into(),
        OrderedValue::String(subscription.into()),
    );
    let mut entry = OrderedObject::default();
    entry.insert(
        "resolved_model".into(),
        OrderedValue::String(resolved_model.into()),
    );
    entry.insert(
        "requested_effort".into(),
        OrderedValue::String(effort.into()),
    );
    entry.insert("effort_verified".into(), OrderedValue::Bool(false));
    let mut models = OrderedObject::default();
    models.insert(fold(alias), OrderedValue::Object(entry));
    let mut snapshot = base_snapshot("claude subscription canary", observed_at);
    snapshot.insert("provenance".into(), OrderedValue::Object(provenance));
    snapshot.insert("models".into(), OrderedValue::Object(models));
    Ok(OrderedValue::Object(snapshot))
}

fn validate_claude_auth(auth: &OrderedValue) -> Result<&str, CapabilityError> {
    if auth.get("loggedIn") != Some(&OrderedValue::Bool(true))
        || auth.get("authMethod").and_then(OrderedValue::as_str) != Some("claude.ai")
    {
        return Err(discovery(
            "Claude subscription authentication is unavailable",
        ));
    }
    auth.get("subscriptionType")
        .and_then(OrderedValue::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| discovery("Claude subscription authentication is unavailable"))
}

fn base_snapshot(source: &str, observed_at: &str) -> OrderedObject {
    let mut snapshot = OrderedObject::default();
    snapshot.insert("schema_version".into(), OrderedValue::Number(1.into()));
    snapshot.insert("source".into(), OrderedValue::String(source.into()));
    snapshot.insert(
        "observed_at".into(),
        OrderedValue::String(observed_at.into()),
    );
    snapshot
}

fn sorted_strings(mut values: Vec<String>) -> OrderedValue {
    values.sort();
    OrderedValue::Array(values.into_iter().map(OrderedValue::String).collect())
}

fn effort_suffix(model_id: &str) -> (Option<&str>, Option<&'static str>) {
    for effort in AGY_EFFORTS {
        let suffix = format!("-{effort}");
        if let Some(family) = model_id
            .strip_suffix(&suffix)
            .filter(|family| !family.is_empty())
        {
            return (Some(family), Some(*effort));
        }
    }
    (None, None)
}

fn contains_foreign_family(family: &str) -> bool {
    let family = family.to_lowercase();
    ["claude", "gpt", "llama", "mistral", "qwen", "oss"]
        .iter()
        .any(|marker| family.contains(marker))
}

/// Validate the small argparse surface used by this command.
pub fn parse_args(argv: &[String]) -> Result<Option<CaptureArgs>, ArgumentError> {
    let mut provider_name = None;
    let mut out = None;
    let mut binary = None;
    let mut alias = None;
    let mut effort = None;
    let mut options_enabled = true;
    let mut i = 0;
    while i < argv.len() {
        let token = argv[i].as_str();
        if options_enabled && (token == "-h" || token == "--help") {
            return Ok(None);
        }
        if options_enabled && token == "--" {
            options_enabled = false;
            i += 1;
            continue;
        }
        if options_enabled && looks_like_option(token) {
            let (option, inline) = token
                .split_once('=')
                .map_or((token, None), |(a, b)| (a, Some(b)));
            let option = resolve_option(option)
                .map_err(|candidates| {
                    ArgumentError::from(format!(
                        "ambiguous option: {option} could match {}",
                        candidates.join(", ")
                    ))
                })?
                .ok_or_else(|| format!("unrecognized arguments: {token}"))?;
            let value = if let Some(value) = inline {
                value.to_owned()
            } else {
                i += 1;
                argv.get(i)
                    .filter(|next| !looks_like_option(next))
                    .cloned()
                    .ok_or_else(|| format!("argument {option}: expected one argument"))?
            };
            match option {
                "--out" => {
                    out = Some(if value.is_empty() {
                        PathBuf::from(".")
                    } else {
                        PathBuf::from(value)
                    })
                }
                "--bin" => binary = Some(value),
                "--alias" => alias = Some(value),
                "--effort" => {
                    if !EFFORTS.contains(&value.as_str()) {
                        return Err(format!(
                            "argument --effort: invalid choice: {} (choose from {})",
                            python_repr(&value),
                            EFFORT_CHOICES
                                .iter()
                                .map(|v| python_repr(v))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                        .into());
                    }
                    effort = Some(value);
                }
                _ => return Err(format!("unrecognized arguments: {token}").into()),
            }
        } else if provider_name.is_none() {
            if provider(token).is_none() {
                return Err(format!(
                    "argument provider: invalid choice: {} (choose from {})",
                    python_repr(token),
                    PROVIDERS
                        .iter()
                        .map(|item| python_repr(item.spec().name))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
                .into());
            }
            provider_name = Some(token.to_owned());
        } else {
            return Err(format!("unrecognized arguments: {token}").into());
        }
        i += 1;
    }
    let Some(provider_name) = provider_name else {
        return Err("the following arguments are required: provider".into());
    };
    let adapter = provider(&provider_name).ok_or_else(|| "unknown provider".to_owned())?;
    let spec = adapter.spec();
    if spec.output_required && out.is_none() {
        return Err(format!("--out is required for {}", spec.name).into());
    }
    if spec.needs_route {
        if alias
            .as_deref()
            .is_none_or(|value| python_strip(value).is_empty())
        {
            return Err("--alias must be non-empty".into());
        }
        if effort.is_none() {
            return Err(format!("--effort is required for {}", spec.name).into());
        }
    } else if alias.is_some() || effort.is_some() {
        return Err(format!("--alias and --effort do not apply to {}", spec.name).into());
    }
    Ok(Some(CaptureArgs {
        provider: provider_name,
        out,
        binary,
        alias,
        effort,
    }))
}

fn looks_like_option(token: &str) -> bool {
    token.starts_with('-') && token != "-" && !is_negative_number(token)
}

fn is_negative_number(token: &str) -> bool {
    let Some(number) = token.strip_prefix('-') else {
        return false;
    };
    if !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()) {
        return true;
    }
    let Some((whole, fraction)) = number.split_once('.') else {
        return false;
    };
    (whole.is_empty() || whole.bytes().all(|byte| byte.is_ascii_digit()))
        && !fraction.is_empty()
        && fraction.bytes().all(|byte| byte.is_ascii_digit())
}

fn resolve_option(option: &str) -> Result<Option<&'static str>, Vec<&'static str>> {
    const OPTIONS: &[&str] = &["--out", "--bin", "--alias", "--effort"];
    let options = OPTIONS;
    if options.contains(&option) {
        return Ok(options
            .iter()
            .copied()
            .find(|candidate| *candidate == option));
    }
    resolve_option_from(option, options)
}

fn resolve_option_from<'a>(
    option: &str,
    options: &'a [&'a str],
) -> Result<Option<&'a str>, Vec<&'a str>> {
    let mut matches = options
        .iter()
        .copied()
        .filter(|candidate| candidate.starts_with(option));
    let Some(first) = matches.next() else {
        return Ok(None);
    };
    let mut candidates = vec![first];
    candidates.extend(matches);
    match candidates.as_slice() {
        [only] => Ok(Some(only)),
        _ => Err(candidates),
    }
}

fn help_text() -> &'static str {
    "usage: capabilities.py [-h] [--out OUT] [--bin BIN] [--alias ALIAS]\n                       [--effort {high,low,max,medium,xhigh}]\n                       {agy,claude,codex}\n\nCapture a runtime capability snapshot for one provider.\n\npositional arguments:\n  {agy,claude,codex}\n\noptions:\n  -h, --help            show this help message and exit\n  --out OUT\n  --bin BIN             provider executable; defaults to the provider name\n  --alias ALIAS\n  --effort {high,low,max,medium,xhigh}\n"
}

/// Run command policy through injected process and output owners.
pub fn run(
    argv: &[String],
    host: &mut dyn CapabilityProcessHost,
    writer: &mut dyn CapabilityOutputWriter,
    context: &DiscoveryContext<'_>,
) -> Result<i32, CapabilityError> {
    let args = match parse_args(argv) {
        Ok(Some(args)) => args,
        Ok(None) => {
            writer
                .stdout(help_text())
                .map_err(CapabilityError::Output)?;
            return Ok(0);
        }
        Err(error) => {
            writer
                .stderr(&format!("{USAGE}\ncapabilities.py: error: {error}\n"))
                .map_err(CapabilityError::Output)?;
            return Ok(2);
        }
    };
    let adapter = provider(&args.provider).ok_or_else(|| discovery("unknown provider"))?;
    let spec = adapter.spec();
    let binary = args
        .binary
        .as_deref()
        .filter(|binary| !binary.is_empty())
        .unwrap_or(spec.default_bin);
    let (snapshot, warning) = match adapter.discover(binary, &args, host, context) {
        Ok(result) => result,
        Err(error) => {
            writer
                .stderr(&format!("capability discovery failed: {error}\n"))
                .map_err(CapabilityError::Output)?;
            return Ok(1);
        }
    };
    if let Some(warning) = warning {
        writer
            .stderr(&format!("{warning}\n"))
            .map_err(CapabilityError::Output)?;
    }
    let encoded = python_json_dumps(&snapshot);
    if let Some(path) = &args.out {
        writer
            .write_file(path, &encoded, spec.restrict_output)
            .map_err(CapabilityError::Output)?;
    } else {
        writer.stdout(&encoded).map_err(CapabilityError::Output)?;
    }
    Ok(0)
}

fn python_json_dumps(value: &OrderedValue) -> String {
    format!("{}\n", crate::pending::json_dumps(value, true, Some(2)))
}

#[cfg(test)]
mod tests;
