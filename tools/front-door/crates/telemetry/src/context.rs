//! Provider context metering, Codex rollout enrichment and ceiling policy.
//! Environment, storage, and provider behavior are explicit seams.
use crate::{Error, Result};
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::{Map, Value};
use std::{
    fs,
    io::{self, BufRead, BufReader},
    path::{Path, PathBuf},
};

const DEFAULT_CEILING: u64 = 300_000;
const MIN_CEILING: u64 = 100_000;
const MAX_CEILING: u64 = 1_000_000;
const MAX_ROLLOUT_LINE_BYTES: usize = crate::MAX_JSON_BYTES;

pub type PlanArgvBuilder = dyn Fn(&Value) -> Vec<String>;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ContextSource {
    Observed,
    Estimated,
}

/// The context fields retained from a provider stream. Unmeasured fields are null.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ContextRecord {
    pub context_tokens: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub context_window_tokens: Option<u64>,
    pub context_percent: Option<f64>,
    pub source: Option<ContextSource>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Models {
    pub init: Option<String>,
    pub answered: Vec<String>,
    #[serde(rename = "final")]
    pub final_model: Option<String>,
    pub usage: Vec<String>,
}

/// Only values that the source reads from the process environment.
#[derive(Clone, Debug)]
pub struct ContextEnvironment {
    pub home: PathBuf,
    pub codex_home: Option<PathBuf>,
    pub claude_config_dir: Option<PathBuf>,
    pub product_root: Option<PathBuf>,
}

impl ContextEnvironment {
    pub fn new(home: PathBuf) -> Self {
        Self {
            home,
            codex_home: None,
            claude_config_dir: None,
            product_root: None,
        }
    }

    fn codex_home(&self) -> PathBuf {
        nonempty(self.codex_home.as_ref())
            .cloned()
            .unwrap_or_else(|| self.home.join(".codex"))
    }

    fn claude_config_dir(&self) -> PathBuf {
        nonempty(self.claude_config_dir.as_ref())
            .cloned()
            .unwrap_or_else(|| self.home.join(".claude"))
    }

    fn product_root(&self) -> PathBuf {
        nonempty(self.product_root.as_ref())
            .cloned()
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../.."))
    }
}

fn nonempty(path: Option<&PathBuf>) -> Option<&PathBuf> {
    path.filter(|path| !path.as_os_str().is_empty())
}

/// File access seam for local configuration and rollout records.
pub trait ContextStore {
    fn read_text(&self, path: &Path) -> Result<String>;
    fn read_lines(&self, path: &Path, visitor: &mut dyn FnMut(&str) -> Result<()>) -> Result<()>;
    fn walk(&self, root: &Path) -> Result<Vec<PathBuf>>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FileContextStore;

impl ContextStore for FileContextStore {
    fn read_text(&self, path: &Path) -> Result<String> {
        let bytes = fs::read(path).map_err(|source| Error::Io {
            path: path.to_owned(),
            source,
        })?;
        String::from_utf8(bytes)
            .map_err(|_| Error::Invalid(format!("invalid UTF-8 in {}", path.display())))
    }

    fn read_lines(&self, path: &Path, visitor: &mut dyn FnMut(&str) -> Result<()>) -> Result<()> {
        let file = fs::File::open(path).map_err(|source| Error::Io {
            path: path.to_owned(),
            source,
        })?;
        stream_lines(&mut BufReader::with_capacity(8192, file), path, visitor)
    }

    fn walk(&self, root: &Path) -> Result<Vec<PathBuf>> {
        let mut todo = vec![root.to_owned()];
        let mut found = Vec::new();
        while let Some(directory) = todo.pop() {
            let entries = match fs::read_dir(&directory) {
                Ok(entries) => entries,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(source) => {
                    return Err(Error::Io {
                        path: directory,
                        source,
                    });
                }
            };
            for entry in entries.flatten() {
                let path = entry.path();
                match entry.file_type() {
                    Ok(kind) if kind.is_dir() => todo.push(path),
                    Ok(_) => found.push(path),
                    Err(_) => {}
                }
            }
        }
        Ok(found)
    }
}

/// Visit universal-newline-delimited text a buffered chunk at a time. Memory
/// is bounded by the reader buffer and one JSON line. Oversized lines are
/// drained and skipped, as decode_json refuses them; rollout size is not capped.
fn stream_lines(
    reader: &mut impl BufRead,
    path: &Path,
    visitor: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<()> {
    let mut line = Vec::new();
    let mut pending_cr = false;
    let mut oversized = false;
    loop {
        let (consumed, complete, eof) = {
            let buffer = reader.fill_buf().map_err(|source| Error::Io {
                path: path.to_owned(),
                source,
            })?;
            if buffer.is_empty() {
                let mut complete = Vec::new();
                if !oversized && (pending_cr || !line.is_empty()) {
                    complete.push(std::mem::take(&mut line));
                }
                (0, complete, true)
            } else {
                let mut complete = Vec::new();
                for byte in buffer.iter().copied() {
                    if pending_cr {
                        pending_cr = false;
                        if oversized {
                            line.clear();
                            oversized = false;
                        } else {
                            complete.push(std::mem::take(&mut line));
                        }
                        if byte == b'\n' {
                            continue;
                        }
                    }
                    match byte {
                        b'\r' => pending_cr = true,
                        b'\n' => {
                            if oversized {
                                line.clear();
                                oversized = false;
                            } else {
                                complete.push(std::mem::take(&mut line));
                            }
                        }
                        _ if !oversized => {
                            if line.len() == MAX_ROLLOUT_LINE_BYTES {
                                line.clear();
                                oversized = true;
                            } else {
                                line.push(byte);
                            }
                        }
                        _ => {}
                    }
                }
                (buffer.len(), complete, false)
            }
        };
        if consumed != 0 {
            reader.consume(consumed);
        }
        for bytes in complete {
            let text = String::from_utf8(bytes)
                .map_err(|_| Error::Invalid(format!("invalid UTF-8 in {}", path.display())))?;
            visitor(&text)?;
        }
        if eof {
            return Ok(());
        }
    }
}

/// A provider implementation selected by one row in the static registry.
pub trait UsageProvider: Sync {
    fn adapter(&self) -> &'static str;
    fn observe(&self, meter: &mut Meter, event: &Value) -> Result<()>;
    fn controls_ceiling(&self) -> bool {
        false
    }
    fn compaction_point(
        &self,
        _model: Option<&Value>,
        _store: &dyn ContextStore,
        _environment: &ContextEnvironment,
    ) -> Option<Result<(Option<u64>, Option<&'static str>)>> {
        None
    }
}

struct Claude;
struct Codex;
struct Cursor;
struct OpenCode;
struct Agy;
struct Kiro;
static CLAUDE: Claude = Claude;
static CODEX: Codex = Codex;
static CURSOR: Cursor = Cursor;
static OPENCODE: OpenCode = OpenCode;
static AGY: Agy = Agy;
static KIRO: Kiro = Kiro;

/// Provider variants and their stream readers share a single source of truth.
static PROVIDERS: [&dyn UsageProvider; 6] = [&CLAUDE, &CODEX, &CURSOR, &OPENCODE, &AGY, &KIRO];

#[derive(Clone, Copy, Debug, Default)]
struct Totals {
    input: Option<u64>,
    output: Option<u64>,
    cached: Option<u64>,
    cache_write: Option<u64>,
    reasoning: Option<u64>,
}

pub struct Meter {
    adapter: String,
    provider: Option<&'static dyn UsageProvider>,
    value: ContextRecord,
    totals: Totals,
    models: Models,
}

impl Meter {
    pub fn new(adapter: impl Into<String>) -> Self {
        let adapter = adapter.into();
        let provider = provider_for(&adapter);
        Self {
            adapter,
            provider,
            value: ContextRecord::default(),
            totals: Totals::default(),
            models: Models::default(),
        }
    }

    /// Invalid or malformed provider fields do not fail an attempt.
    pub fn observe(&mut self, event: &Value) -> Result<()> {
        if event.is_object() {
            if let Some(provider) = self.provider {
                provider.observe(self, event)?;
            }
        }
        Ok(())
    }

    /// Preserve JSON object order for the source-visible Claude model list.
    pub fn observe_json_line(&mut self, line: &str) -> Result<()> {
        let event = crate::decode_json(line.as_bytes())?;
        let model_keys = ordered_model_usage_keys(line)?;
        self.observe(&event)?;
        if event.get("type").and_then(Value::as_str) == Some("result") {
            if let Some(keys) = model_keys {
                self.models.usage = keys;
            }
        }
        Ok(())
    }

    pub fn result(&self) -> ContextRecord {
        self.value.clone()
    }
    pub fn models(&self) -> Models {
        self.models.clone()
    }
    pub fn adapter(&self) -> &str {
        &self.adapter
    }

    /// The answering model, then a lone modelUsage key, then init as fallback.
    pub fn answering_model(&self) -> (Option<&str>, Option<&'static str>) {
        if !self.models.answered.is_empty() {
            return (
                self.models
                    .final_model
                    .as_deref()
                    .or_else(|| self.models.answered.last().map(String::as_str)),
                Some("claude:assistant.message.model"),
            );
        }
        if self.models.usage.len() == 1 {
            return (
                self.models.usage.first().map(String::as_str),
                Some("claude:result.modelUsage"),
            );
        }
        let source = self
            .models
            .init
            .as_deref()
            .filter(|model| !model.is_empty())
            .map(|_| "claude:init.model");
        (self.models.init.as_deref(), source)
    }

    fn add(
        &mut self,
        input: Option<u64>,
        output: Option<u64>,
        cached: Option<u64>,
        cache_write: Option<u64>,
        reasoning: Option<u64>,
    ) -> Result<()> {
        for (slot, increment) in [
            (&mut self.totals.input, input),
            (&mut self.totals.output, output),
            (&mut self.totals.cached, cached),
            (&mut self.totals.cache_write, cache_write),
            (&mut self.totals.reasoning, reasoning),
        ] {
            if let Some(increment) = increment {
                *slot = Some(
                    slot.unwrap_or(0)
                        .checked_add(increment)
                        .ok_or(Error::NumberOutOfRange)?,
                );
            }
        }
        Ok(())
    }

    fn claude_request(&mut self, usage: Option<&Value>) -> Result<()> {
        let Some(usage) = usage.filter(|usage| usage.is_object()) else {
            return Ok(());
        };
        let size = sum([
            integer(usage.get("input_tokens")),
            integer(usage.get("cache_creation_input_tokens")),
            integer(usage.get("cache_read_input_tokens")),
            integer(usage.get("output_tokens")),
        ])?;
        if let Some(size) = size {
            self.value.context_tokens = Some(size);
            self.value.source = Some(ContextSource::Observed);
        }
        Ok(())
    }
}

impl UsageProvider for Claude {
    fn adapter(&self) -> &'static str {
        "claude"
    }
    fn controls_ceiling(&self) -> bool {
        true
    }

    fn observe(&self, meter: &mut Meter, event: &Value) -> Result<()> {
        let kind = event.get("type").and_then(Value::as_str);
        if kind == Some("system") && event.get("subtype").and_then(Value::as_str) == Some("init") {
            if let Some(model) = event.get("model").and_then(Value::as_str) {
                if model != "<synthetic>" {
                    meter.models.init = Some(model.to_owned());
                }
            }
        }
        if kind == Some("assistant") {
            if let Some(message) = event.get("message").filter(|message| message.is_object()) {
                if let Some(model) = message.get("model").and_then(Value::as_str) {
                    if !model.is_empty() && model != "<synthetic>" {
                        if !meter.models.answered.iter().any(|seen| seen == model) {
                            meter.models.answered.push(model.to_owned());
                        }
                        if event.get("parent_tool_use_id").is_none_or(Value::is_null) {
                            meter.models.final_model = Some(model.to_owned());
                        }
                    }
                }
                meter.claude_request(message.get("usage"))?;
            }
        }
        if kind != Some("result") {
            return Ok(());
        }

        let empty = Value::Object(Map::new());
        let raw_usage = event.get("usage");
        let usage = if raw_usage.is_some_and(provenant_core::py::python_truth) {
            let Some(usage) = raw_usage.filter(|usage| usage.is_object()) else {
                return Ok(());
            };
            usage
        } else {
            &empty
        };
        meter.value.input_tokens = sum([
            integer(usage.get("input_tokens")),
            integer(usage.get("cache_creation_input_tokens")),
            integer(usage.get("cache_read_input_tokens")),
        ])?;
        meter.value.output_tokens = integer(usage.get("output_tokens"));
        meter.value.cached_input_tokens = integer(usage.get("cache_read_input_tokens"));
        meter.value.cache_write_tokens = integer(usage.get("cache_creation_input_tokens"));
        meter.value.reasoning_tokens = usage
            .get("output_tokens_details")
            .filter(|details| details.is_object())
            .and_then(|details| integer(details.get("thinking_tokens")));

        let iterations = usage.get("iterations");
        if iterations.is_some_and(provenant_core::py::python_truth) {
            match iterations {
                Some(Value::Array(items)) => {
                    if let Some(last) = items.iter().rfind(|item| item.is_object()) {
                        meter.claude_request(Some(last))?;
                    }
                }
                Some(Value::Object(_)) | Some(Value::String(_)) => {}
                _ => return Ok(()),
            }
        }
        let usage_models = event.get("modelUsage").filter(|models| models.is_object());
        meter.models.usage = usage_models
            .and_then(Value::as_object)
            .map(|models| models.keys().cloned().collect())
            .unwrap_or_default();
        let (answering, _) = meter.answering_model();
        meter.value.context_window_tokens = answering
            .and_then(|model| usage_models.and_then(|models| models.get(model)))
            .filter(|model_usage| model_usage.is_object())
            .and_then(|model_usage| integer(model_usage.get("contextWindow")));
        Ok(())
    }

    fn compaction_point(
        &self,
        model: Option<&Value>,
        store: &dyn ContextStore,
        environment: &ContextEnvironment,
    ) -> Option<Result<(Option<u64>, Option<&'static str>)>> {
        Some(claude_point(model, store, environment))
    }
}

impl UsageProvider for Codex {
    fn adapter(&self) -> &'static str {
        "codex"
    }
    fn controls_ceiling(&self) -> bool {
        true
    }
    fn observe(&self, meter: &mut Meter, event: &Value) -> Result<()> {
        if event.get("type").and_then(Value::as_str) == Some("turn.completed") {
            if let Some(usage) = event.get("usage").filter(|usage| usage.is_object()) {
                meter.value.input_tokens = integer(usage.get("input_tokens"));
                meter.value.output_tokens = integer(usage.get("output_tokens"));
                meter.value.cached_input_tokens = integer(usage.get("cached_input_tokens"));
                meter.value.cache_write_tokens = integer(usage.get("cache_write_input_tokens"));
                meter.value.reasoning_tokens = integer(usage.get("reasoning_output_tokens"));
                meter.value.context_tokens =
                    sum([meter.value.input_tokens, meter.value.output_tokens])?;
                meter.value.source = Some(ContextSource::Estimated);
            }
        }
        Ok(())
    }
    fn compaction_point(
        &self,
        model: Option<&Value>,
        store: &dyn ContextStore,
        environment: &ContextEnvironment,
    ) -> Option<Result<(Option<u64>, Option<&'static str>)>> {
        Some(codex_point(model, store, environment))
    }
}

impl UsageProvider for Cursor {
    fn adapter(&self) -> &'static str {
        "cursor"
    }
    fn observe(&self, meter: &mut Meter, event: &Value) -> Result<()> {
        if event.get("type").and_then(Value::as_str) == Some("result") {
            if let Some(usage) = event.get("usage").filter(|usage| usage.is_object()) {
                let total = sum([
                    integer(usage.get("inputTokens")),
                    integer(usage.get("cacheReadTokens")),
                    integer(usage.get("cacheWriteTokens")),
                ])?;
                meter.value.input_tokens = total;
                meter.value.output_tokens = integer(usage.get("outputTokens"));
                meter.value.cached_input_tokens = integer(usage.get("cacheReadTokens"));
                meter.value.cache_write_tokens = integer(usage.get("cacheWriteTokens"));
                meter.value.context_tokens = sum([total, meter.value.output_tokens])?;
                meter.value.source = Some(ContextSource::Estimated);
            }
        }
        Ok(())
    }
}

impl UsageProvider for OpenCode {
    fn adapter(&self) -> &'static str {
        "opencode"
    }
    fn observe(&self, meter: &mut Meter, event: &Value) -> Result<()> {
        if event.get("type").and_then(Value::as_str) != Some("step_finish") {
            return Ok(());
        }
        let Some(tokens) = event
            .get("part")
            .filter(|part| part.is_object())
            .and_then(|part| part.get("tokens"))
            .filter(|tokens| tokens.is_object())
        else {
            return Ok(());
        };
        let cache = tokens.get("cache").filter(|cache| cache.is_object());
        let read = cache.and_then(|cache| integer(cache.get("read")));
        let write = cache.and_then(|cache| integer(cache.get("write")));
        let input = sum([integer(tokens.get("input")), read, write])?;
        let output = integer(tokens.get("output"));
        let reasoning = integer(tokens.get("reasoning"));
        meter.add(input, output, read, write, reasoning)?;
        let total = integer(tokens.get("total"));
        meter.value.context_tokens = if total.is_some_and(|total| total != 0) {
            total
        } else {
            sum([integer(tokens.get("input")), output, read, write])?
        };
        meter.value.source = Some(ContextSource::Observed);
        meter.value.input_tokens = meter.totals.input;
        meter.value.output_tokens = meter.totals.output;
        meter.value.cached_input_tokens = meter.totals.cached;
        meter.value.cache_write_tokens = meter.totals.cache_write;
        meter.value.reasoning_tokens = meter.totals.reasoning;
        Ok(())
    }
}

impl UsageProvider for Agy {
    fn adapter(&self) -> &'static str {
        "agy"
    }
    fn observe(&self, meter: &mut Meter, event: &Value) -> Result<()> {
        if let Some(usage) = event
            .get("step_update")
            .filter(|step| step.is_object())
            .and_then(|step| step.get("usage"))
            .filter(|usage| usage.is_object())
        {
            meter.value.context_tokens = sum([
                integer(usage.get("input_tokens")),
                integer(usage.get("output_tokens")),
            ])?;
            meter.value.source = Some(ContextSource::Observed);
        }
        if event.get("event").and_then(Value::as_str) == Some("result") {
            if let Some(usage) = event
                .get("result")
                .filter(|result| result.is_object())
                .and_then(|result| result.get("usage"))
                .filter(|usage| usage.is_object())
            {
                meter.value.input_tokens = integer(usage.get("input_tokens"));
                meter.value.output_tokens = integer(usage.get("output_tokens"));
                meter.value.cached_input_tokens = integer(usage.get("cache_read_tokens"));
                meter.value.reasoning_tokens = integer(usage.get("thinking_tokens"));
            }
        }
        Ok(())
    }
}

impl UsageProvider for Kiro {
    fn adapter(&self) -> &'static str {
        "kiro"
    }
    fn observe(&self, meter: &mut Meter, event: &Value) -> Result<()> {
        if event.get("type").and_then(Value::as_str) != Some("metadata") {
            return Ok(());
        }
        let Some(percent) = event
            .get("data")
            .filter(|data| data.is_object())
            .and_then(|data| data.get("contextUsagePercentage"))
        else {
            return Ok(());
        };
        if percent.is_boolean() {
            return Ok(());
        }
        let Some(number) = percent.as_f64() else {
            return Ok(());
        };
        if (0.0..=100.0).contains(&number) {
            let rounded = format!("{number:.1}").parse::<f64>().unwrap_or(number);
            meter.value.context_percent = Some(rounded);
            meter.value.source = Some(ContextSource::Observed);
        }
        Ok(())
    }
}

fn provider_for(adapter: &str) -> Option<&'static dyn UsageProvider> {
    PROVIDERS
        .iter()
        .copied()
        .find(|provider| provider.adapter() == adapter)
}

fn controls_ceiling(adapter: &str) -> bool {
    provider_for(adapter).is_some_and(UsageProvider::controls_ceiling)
}

fn integer(value: Option<&Value>) -> Option<u64> {
    value?.as_number()?.as_u64()
}

fn sum(values: impl IntoIterator<Item = Option<u64>>) -> Result<Option<u64>> {
    let mut found = false;
    let mut total = 0_u64;
    for value in values.into_iter().flatten() {
        found = true;
        total = total.checked_add(value).ok_or(Error::NumberOutOfRange)?;
    }
    Ok(found.then_some(total))
}

pub fn usage_source(value: &ContextRecord) -> &'static str {
    if [
        value.input_tokens,
        value.output_tokens,
        value.cached_input_tokens,
        value.cache_write_tokens,
        value.reasoning_tokens,
    ]
    .into_iter()
    .any(|value| value.is_some())
    {
        "observed"
    } else {
        "missing"
    }
}

pub fn codex_rollout_effort(
    session: &str,
    environment: &ContextEnvironment,
    store: &dyn ContextStore,
) -> Result<Option<String>> {
    for path in codex_rollouts(session, environment, store) {
        let mut effort = None;
        let result = store.read_lines(&path, &mut |line| {
            if !line.contains("\"turn_context\"") {
                return Ok(());
            }
            let event = match crate::decode_json::<Value>(line.as_bytes()) {
                Ok(event) => event,
                Err(Error::NumberOutOfRange) => return Err(Error::NumberOutOfRange),
                Err(_) => return Ok(()),
            };
            let Some(event) = event.as_object() else {
                return Ok(());
            };
            let payload = event.get("payload");
            let payload = if payload.is_some_and(provenant_core::py::python_truth) {
                payload
            } else {
                None
            };
            let Some(payload) = payload.and_then(Value::as_object) else {
                return Ok(());
            };
            let value = if event.get("type").and_then(Value::as_str) == Some("turn_context") {
                payload.get("effort").and_then(Value::as_str)
            } else {
                None
            };
            if value.is_some_and(valid_effort) {
                effort = value.map(str::to_owned);
            }
            Ok(())
        });
        match result {
            Ok(()) => {}
            Err(Error::Io { .. }) => continue,
            Err(error) => return Err(error),
        }
        if effort.is_some() {
            return Ok(effort);
        }
    }
    Ok(None)
}

fn valid_effort(value: &str) -> bool {
    !value.is_empty() && value.len() <= 16 && value.bytes().all(|byte| byte.is_ascii_lowercase())
}

pub fn with_codex_rollout(
    value: &ContextRecord,
    session: &str,
    environment: &ContextEnvironment,
    store: &dyn ContextStore,
) -> Result<ContextRecord> {
    for path in codex_rollouts(session, environment, store) {
        let mut info: Option<Value> = None;
        let result = store.read_lines(&path, &mut |line| {
            if !line.contains("\"token_count\"") {
                return Ok(());
            }
            let event = match crate::decode_json::<Value>(line.as_bytes()) {
                Ok(event) => event,
                Err(Error::NumberOutOfRange) => return Err(Error::NumberOutOfRange),
                Err(_) => return Ok(()),
            };
            let Some(event) = event.as_object() else {
                return Ok(());
            };
            let payload = event.get("payload");
            let payload = if payload.is_some_and(provenant_core::py::python_truth) {
                payload
            } else {
                None
            };
            if payload.is_some_and(|payload| !payload.is_object()) {
                return Err(Error::Invalid(
                    "Codex token_count payload must be an object".into(),
                ));
            }
            let Some(payload) = payload.and_then(Value::as_object) else {
                return Ok(());
            };
            if payload.get("type").and_then(Value::as_str) == Some("token_count") {
                if let Some(candidate) = payload.get("info").filter(|info| info.is_object()) {
                    info = Some(candidate.clone());
                }
            }
            Ok(())
        });
        match result {
            Ok(()) => {}
            Err(Error::Io { .. }) => continue,
            Err(error) => return Err(error),
        }
        let Some(info) = info.filter(provenant_core::py::python_truth) else {
            continue;
        };
        let last = info.get("last_token_usage");
        let last = if last.is_some_and(provenant_core::py::python_truth) {
            last
        } else {
            None
        };
        if last.is_some_and(|last| !last.is_object()) {
            return Err(Error::Invalid(
                "Codex last_token_usage must be an object".into(),
            ));
        }
        let total = last.and_then(|last| integer(last.get("total_tokens")));
        let size = if total.is_some_and(|total| total != 0) {
            total
        } else {
            sum([
                last.and_then(|last| integer(last.get("input_tokens"))),
                last.and_then(|last| integer(last.get("output_tokens"))),
            ])?
        };
        let mut result = value.clone();
        if let Some(size) = size {
            result.context_tokens = Some(size);
            result.source = Some(ContextSource::Observed);
        }
        let window = integer(info.get("model_context_window"));
        if window.is_some_and(|window| window != 0) {
            result.context_window_tokens = window;
        }
        return Ok(result);
    }
    Ok(value.clone())
}

fn codex_rollouts(
    session: &str,
    environment: &ContextEnvironment,
    store: &dyn ContextStore,
) -> Vec<PathBuf> {
    if session.is_empty()
        || session.chars().count() > 128
        || !session
            .chars()
            .all(|c| c == '-' || c == '_' || c.is_alphanumeric())
    {
        return Vec::new();
    }
    let mut paths = match store.walk(&environment.codex_home().join("sessions")) {
        Ok(paths) => paths,
        Err(_) => return Vec::new(),
    };
    paths.retain(|path| {
        path.file_name().is_some_and(|name| {
            let name = name.to_string_lossy();
            name.contains(session) && name.ends_with(".jsonl")
        })
    });
    paths.sort();
    paths.reverse();
    paths
}

pub fn default_ceiling(environment: &ContextEnvironment, store: &dyn ContextStore) -> Result<u64> {
    let path = environment.product_root().join("config/model-routing.json");
    let text = match store.read_text(&path) {
        Ok(text) => text,
        Err(Error::NumberOutOfRange) => return Err(Error::NumberOutOfRange),
        Err(_) => return Ok(DEFAULT_CEILING),
    };
    let value = match crate::decode_json::<Value>(text.as_bytes()) {
        Ok(value) => value,
        Err(Error::NumberOutOfRange) => return Err(Error::NumberOutOfRange),
        Err(_) => return Ok(DEFAULT_CEILING),
    };
    let configured = value
        .get("context")
        .and_then(|context| integer(context.get("ceiling_tokens")));
    Ok(configured
        .filter(|value| *value != 0)
        .unwrap_or(DEFAULT_CEILING))
}

fn optional_json(path: &Path, store: &dyn ContextStore) -> Result<Option<Value>> {
    let text = match store.read_text(path) {
        Ok(text) => text,
        Err(Error::Io { .. } | Error::Invalid(_)) => return Ok(None),
        Err(error) => return Err(error),
    };
    match crate::decode_json::<Value>(text.as_bytes()) {
        Ok(value) => Ok(Some(value)),
        Err(Error::NumberOutOfRange) => Err(Error::NumberOutOfRange),
        Err(Error::InputTooLarge { .. } | Error::Json(_)) => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn clamp_ceiling(
    value: Option<&Value>,
    environment: &ContextEnvironment,
    store: &dyn ContextStore,
) -> Result<(u64, Option<String>)> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok((default_ceiling(environment, store)?, None));
    };
    let Some(requested) = python_float(value) else {
        let fallback = default_ceiling(environment, store)?;
        return Ok((
            fallback,
            Some(format!(
                "context_ceiling {} ignored; using {fallback}",
                provenant_core::py::python_value_repr(value)
            )),
        ));
    };
    if !requested.is_finite() {
        let fallback = default_ceiling(environment, store)?;
        return Ok((
            fallback,
            Some(format!(
                "context_ceiling {} ignored; using {fallback}",
                provenant_core::py::python_value_repr(value)
            )),
        ));
    }
    let rounded = requested.round_ties_even();
    let tokens = if rounded < MIN_CEILING as f64 {
        MIN_CEILING
    } else if rounded > MAX_CEILING as f64 {
        MAX_CEILING
    } else {
        rounded as u64
    };
    if rounded < MIN_CEILING as f64 || rounded > MAX_CEILING as f64 {
        let requested_text = if rounded == 0.0 {
            "0".to_owned()
        } else {
            format!("{rounded:.0}")
        };
        Ok((
            tokens,
            Some(format!(
                "context_ceiling {requested_text} clamped to {tokens}"
            )),
        ))
    } else {
        Ok((tokens, None))
    }
}

fn python_float(value: &Value) -> Option<f64> {
    match value {
        Value::Null => None,
        Value::Bool(value) => Some(if *value { 1.0 } else { 0.0 }),
        Value::Number(value) => value.as_f64(),
        Value::String(value) => parse_python_float(value),
        Value::Array(_) | Value::Object(_) => None,
    }
}

fn parse_python_float(value: &str) -> Option<f64> {
    // CPython float() accepts Unicode whitespace but not the extra control
    // separators that str.strip() treats as whitespace.
    let value = value.trim_matches(|character: char| {
        character.is_whitespace() && !(('\u{1c}'..='\u{1f}').contains(&character))
    });
    if value.is_empty() {
        return None;
    }
    let lower = value.to_ascii_lowercase();
    if matches!(lower.as_str(), "inf" | "+inf" | "infinity" | "+infinity") {
        return Some(f64::INFINITY);
    }
    if matches!(lower.as_str(), "-inf" | "-infinity") {
        return Some(f64::NEG_INFINITY);
    }
    if matches!(lower.as_str(), "nan" | "+nan" | "-nan") {
        return Some(f64::NAN);
    }
    let chars: Vec<char> = value
        .chars()
        .map(|character| {
            crate::pending::decimal_digit(character)
                .map(|digit| char::from(b'0' + digit))
                .unwrap_or(character)
        })
        .collect();
    let mut cleaned = String::with_capacity(value.len());
    for (index, character) in chars.iter().copied().enumerate() {
        if character == '_' {
            let before = index
                .checked_sub(1)
                .and_then(|i| chars.get(i))
                .is_some_and(|c| c.is_ascii_digit());
            let after = chars.get(index + 1).is_some_and(|c| c.is_ascii_digit());
            if !before || !after {
                return None;
            }
        } else {
            cleaned.push(character);
        }
    }
    cleaned.parse().ok()
}

const CLAUDE_WINDOWS: &[(&str, u64)] = &[
    ("opus", 1_000_000),
    ("opus-5.5", 1_000_000),
    ("claude-opus-5-5", 1_000_000),
    ("sonnet", 1_000_000),
    ("claude-sonnet-5-5", 1_000_000),
    ("claude-sonnet-5", 1_000_000),
    ("fable", 1_000_000),
    ("claude-fable-5-1", 1_000_000),
    ("haiku", 1_000_000),
    ("haiku-5.5", 1_000_000),
    ("claude-haiku-5-5", 1_000_000),
    ("claude-haiku-4-5", 200_000),
    ("claude-haiku-4-5-20251001", 200_000),
];

fn claude_point(
    model: Option<&Value>,
    store: &dyn ContextStore,
    environment: &ContextEnvironment,
) -> Result<(Option<u64>, Option<&'static str>)> {
    let name = model
        .filter(|model| provenant_core::py::python_truth(model))
        .map(provenant_core::py::python_str)
        .unwrap_or_default();
    let window = CLAUDE_WINDOWS
        .iter()
        .find_map(|(model, size)| (*model == name).then_some(*size))
        .or_else(|| name.ends_with("[1m]").then_some(1_000_000));
    let path = environment.claude_config_dir().join("settings.json");
    let settings = optional_json(&path, store)?.filter(Value::is_object);
    let settings = settings.as_ref().and_then(Value::as_object);
    let user = settings.and_then(|settings| integer(settings.get("autoCompactWindow")));
    let enabled = settings
        .and_then(|settings| settings.get("autoCompactEnabled"))
        .is_none_or(|enabled| enabled != &Value::Bool(false));
    if user.is_some_and(|user| user != 0)
        && enabled
        && window.is_none_or(|window| user.is_some_and(|user| user < window))
    {
        return Ok((user, Some("claude user settings autoCompactWindow")));
    }
    Ok((window, window.map(|_| "claude model window")))
}

fn codex_point(
    model: Option<&Value>,
    store: &dyn ContextStore,
    environment: &ContextEnvironment,
) -> Result<(Option<u64>, Option<&'static str>)> {
    // The Python expression is `model and ...`; falsy model values must never
    // match a cache entry whose slug is absent or falsy.
    let Some(model) = model.filter(|model| provenant_core::py::python_truth(model)) else {
        return Ok((None, None));
    };
    let path = environment.codex_home().join("models_cache.json");
    let cache = optional_json(&path, store)?.unwrap_or(Value::Null);
    let entries = cache.get("models");
    let entry = entries
        .filter(|entries| provenant_core::py::python_truth(entries))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.is_object())
        .find(|item| {
            item.get("slug")
                .is_some_and(|slug| provenant_core::py::python_json_eq(slug, model))
        });
    let window = entry.and_then(|entry| integer(entry.get("context_window")));
    let percent = entry.and_then(|entry| integer(entry.get("effective_context_window_percent")));
    if window.is_none_or(|value| value == 0) || percent.is_none_or(|value| value == 0) {
        return Ok((None, None));
    }
    let product = window
        .unwrap_or(0)
        .checked_mul(percent.unwrap_or(0))
        .ok_or(Error::NumberOutOfRange)?;
    Ok((
        Some(product / 100),
        Some("codex models_cache effective window"),
    ))
}

pub fn effective_ceiling(
    applied: &Value,
    environment: &ContextEnvironment,
    store: &dyn ContextStore,
) -> Result<u64> {
    if !provenant_core::py::python_truth(applied) {
        return default_ceiling(environment, store);
    }
    integer(applied.get("context_ceiling_tokens"))
        .filter(|n| *n != 0)
        .or_else(|| integer(applied.get("context_ceiling_requested")).filter(|n| *n != 0))
        .map(Ok)
        .unwrap_or_else(|| default_ceiling(environment, store))
}

pub fn apply_ceiling(
    plan: &mut Value,
    value: Option<&Value>,
    environment: &ContextEnvironment,
    store: &dyn ContextStore,
    argv: Option<&PlanArgvBuilder>,
) -> Result<()> {
    let object = plan
        .as_object()
        .ok_or_else(|| Error::Invalid("plan must be an object".into()))?;
    let adapter = object
        .get("adapter")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Invalid("plan.adapter must be a string".into()))?
        .to_owned();
    let model = object.get("model").cloned();
    if !object.get("applied").is_some_and(Value::is_object) {
        return Err(Error::Invalid("plan.applied must be an object".into()));
    }
    let (tokens, warning) = clamp_ceiling(value, environment, store)?;
    let point = provider_for(&adapter)
        .and_then(|provider| provider.compaction_point(model.as_ref(), store, environment))
        .transpose()?;
    let mut enforced = None;
    let mut state = "unsupported";
    let mut applied_tokens = Some(tokens);
    let mut source = None;
    let mut unknown = None;
    if let Some((point, point_source)) = point {
        if let Some(point) = point {
            if tokens < point {
                enforced = Some(tokens);
                state = "enforced";
            } else {
                state = "provider_default";
                applied_tokens = Some(point);
                source = point_source;
            }
        } else {
            state = "provider_default";
            applied_tokens = None;
            let name = model
                .as_ref()
                .map(provenant_core::py::python_str)
                .unwrap_or_else(|| "None".into());
            unknown = Some(format!(
                "context_ceiling not applied: {adapter} compaction point for {name} unknown"
            ));
        }
    }
    {
        let object = plan
            .as_object_mut()
            .ok_or_else(|| Error::Invalid("plan must be an object".into()))?;
        let applied = object
            .get_mut("applied")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| Error::Invalid("plan.applied must be an object".into()))?;
        applied.remove("context_ceiling_source");
        applied.insert("context_ceiling_requested".into(), Value::from(tokens));
        applied.insert("context_ceiling".into(), Value::from(state));
        applied.insert(
            "context_ceiling_tokens".into(),
            applied_tokens.map(Value::from).unwrap_or(Value::Null),
        );
        if let Some(source) = source {
            applied.insert("context_ceiling_source".into(), Value::from(source));
        }
        object.insert(
            "context_ceiling".into(),
            enforced.map(Value::from).unwrap_or(Value::Null),
        );
        if let Some(warning) = unknown {
            push_warning(object, &warning)?;
        }
        if let Some(warning) = warning {
            push_warning(object, &warning)?;
        }
    }
    if let Some(argv) = argv {
        let arguments = argv(plan);
        let object = plan
            .as_object_mut()
            .ok_or_else(|| Error::Invalid("plan must be an object".into()))?;
        object.insert(
            "argv".into(),
            Value::Array(arguments.into_iter().map(Value::from).collect()),
        );
    }
    Ok(())
}

fn push_warning(object: &mut Map<String, Value>, warning: &str) -> Result<()> {
    let warnings = object
        .get_mut("warnings")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| Error::Invalid("plan.warnings must be an array".into()))?;
    if !warnings.iter().any(|item| item.as_str() == Some(warning)) {
        warnings.push(Value::from(warning));
    }
    Ok(())
}

pub fn short(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}", tokens as f64 / 1_000_000.0)
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_owned()
            + "M"
    } else {
        format!("{}k", (tokens as f64 / 1000.0).round_ties_even() as u64)
    }
}

fn ordered_model_usage_keys(line: &str) -> Result<Option<Vec<String>>> {
    #[derive(Default)]
    struct RootKeys(Option<Vec<String>>);
    struct Keys(Vec<String>);
    impl<'de> Deserialize<'de> for RootKeys {
        fn deserialize<D: Deserializer<'de>>(
            deserializer: D,
        ) -> std::result::Result<Self, D::Error> {
            struct Visitor;
            impl<'de> de::Visitor<'de> for Visitor {
                type Value = RootKeys;
                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("a JSON object")
                }
                fn visit_map<A: de::MapAccess<'de>>(
                    self,
                    mut map: A,
                ) -> std::result::Result<RootKeys, A::Error> {
                    let mut out = RootKeys::default();
                    while let Some(key) = map.next_key::<String>()? {
                        if key == "modelUsage" {
                            out.0 = Some(map.next_value::<Keys>()?.0);
                        } else {
                            map.next_value::<de::IgnoredAny>()?;
                        }
                    }
                    Ok(out)
                }
            }
            deserializer.deserialize_map(Visitor)
        }
    }
    impl<'de> Deserialize<'de> for Keys {
        fn deserialize<D: Deserializer<'de>>(
            deserializer: D,
        ) -> std::result::Result<Self, D::Error> {
            struct Visitor;
            impl<'de> de::Visitor<'de> for Visitor {
                type Value = Keys;
                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("a JSON object")
                }
                fn visit_map<A: de::MapAccess<'de>>(
                    self,
                    mut map: A,
                ) -> std::result::Result<Keys, A::Error> {
                    let mut keys = Vec::new();
                    while let Some(key) = map.next_key::<String>()? {
                        if !keys.contains(&key) {
                            keys.push(key);
                        }
                        map.next_value::<de::IgnoredAny>()?;
                    }
                    Ok(Keys(keys))
                }
            }
            deserializer.deserialize_map(Visitor)
        }
    }
    let mut deserializer = serde_json::Deserializer::from_str(line);
    match RootKeys::deserialize(&mut deserializer) {
        Ok(keys) => {
            deserializer.end()?;
            Ok(keys.0)
        }
        Err(_) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{cell::Cell, fs, io::Write, path::Path};

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../..")
            .canonicalize()
            .expect("workspace root")
    }

    fn fixture_lines(name: &str) -> Vec<String> {
        fs::read_to_string(
            workspace_root()
                .join("tests/fixtures/fabric-context")
                .join(format!("{name}.jsonl")),
        )
        .expect("provider fixture")
        .lines()
        .map(str::to_owned)
        .collect()
    }

    #[test]
    fn recorded_adapter_fixtures_cover_context_sources_and_breakdowns() {
        let cases = [
            ("claude", "claude", 8039, "observed"),
            ("claude", "claude-direct-haiku", 4977, "observed"),
            ("codex", "codex", 16986, "estimated"),
            ("cursor", "cursor", 19124, "estimated"),
            ("opencode", "opencode", 17230, "observed"),
            ("agy", "agy", 15400, "observed"),
            ("kiro", "kiro", 0, "observed"),
        ];
        for (adapter, fixture, tokens, source) in cases {
            let mut meter = Meter::new(adapter);
            for line in fixture_lines(fixture) {
                meter.observe_json_line(&line).unwrap();
            }
            let result = meter.result();
            if fixture != "kiro" {
                assert_eq!(result.context_tokens, Some(tokens), "{fixture}");
            }
            assert_eq!(
                result.source.map(|v| match v {
                    ContextSource::Observed => "observed",
                    ContextSource::Estimated => "estimated",
                }),
                Some(source)
            );
        }
    }

    #[test]
    fn malformed_fields_are_ignored_and_opencode_steps_accumulate() {
        let mut meter = Meter::new("claude");
        meter
            .observe(
                &json!({"type":"result","usage":{"input_tokens":10,"output_tokens":4,
            "cache_creation_input_tokens":3,"cache_read_input_tokens":5,
            "output_tokens_details":{"thinking_tokens":true}}}),
            )
            .unwrap();
        assert_eq!(meter.result().input_tokens, Some(18));
        assert_eq!(meter.result().reasoning_tokens, None);

        let mut opencode = Meter::new("opencode");
        let event = json!({"type":"step_finish","part":{"tokens":{"input":10,"output":4,
            "reasoning":2,"cache":{"read":5,"write":3}}}});
        opencode.observe(&event).unwrap();
        opencode.observe(&event).unwrap();
        assert_eq!(opencode.result().context_tokens, Some(22));
        assert_eq!(opencode.result().input_tokens, Some(36));

        let mut unknown = Meter::new("copilot");
        unknown.observe(&Value::Null).unwrap();
        assert_eq!(unknown.result(), ContextRecord::default());
        assert_eq!(usage_source(&unknown.result()), "missing");
    }

    #[test]
    fn raw_events_keep_model_usage_key_order() {
        let mut meter = Meter::new("claude");
        meter.observe_json_line(r#"{"type":"result","usage":{},"modelUsage":{"z":{"contextWindow":10},"a":{"contextWindow":20}}}"#).unwrap();
        assert_eq!(meter.models().usage, vec!["z", "a"]);
        assert_eq!(meter.result().context_window_tokens, None);
    }

    #[test]
    fn rollout_enrichment_reads_only_the_explicit_codex_home() {
        let temp = tempfile::tempdir().unwrap();
        let codex = temp.path().join("codex");
        let day = codex.join("sessions/2026/10/23");
        fs::create_dir_all(&day).unwrap();
        fs::write(day.join("rollout-thread-1.jsonl"),
            "{\"type\":\"turn_context\",\"payload\":{\"effort\":\"low\"}}\n\
             {\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"last_token_usage\":{\"total_tokens\":0,\"input_tokens\":100,\"output_tokens\":20},\"model_context_window\":258400}}}\n").unwrap();
        let mut env = ContextEnvironment::new(temp.path().to_owned());
        env.codex_home = Some(codex);
        let store = FileContextStore;
        assert_eq!(
            codex_rollout_effort("thread-1", &env, &store).unwrap(),
            Some("low".into())
        );
        let result = with_codex_rollout(
            &ContextRecord {
                context_tokens: Some(5),
                source: Some(ContextSource::Estimated),
                ..ContextRecord::default()
            },
            "thread-1",
            &env,
            &store,
        )
        .unwrap();
        assert_eq!(result.context_tokens, Some(120));
        assert_eq!(result.context_window_tokens, Some(258400));
        assert_eq!(result.source, Some(ContextSource::Observed));
        assert_eq!(
            codex_rollout_effort("../escape", &env, &store).unwrap(),
            None
        );
    }

    struct NoTextRolloutStore {
        text_reads: Cell<usize>,
        files: FileContextStore,
    }

    impl ContextStore for NoTextRolloutStore {
        fn read_text(&self, _path: &Path) -> Result<String> {
            self.text_reads.set(self.text_reads.get() + 1);
            Err(Error::Invalid("rollouts must be read as lines".into()))
        }

        fn read_lines(
            &self,
            path: &Path,
            visitor: &mut dyn FnMut(&str) -> Result<()>,
        ) -> Result<()> {
            self.files.read_lines(path, visitor)
        }

        fn walk(&self, root: &Path) -> Result<Vec<PathBuf>> {
            self.files.walk(root)
        }
    }

    #[test]
    fn rollout_scans_large_file_through_line_reader_without_a_total_cap() {
        let temp = tempfile::tempdir().unwrap();
        let codex = temp.path().join("codex");
        let day = codex.join("sessions/2026/10/23");
        fs::create_dir_all(&day).unwrap();
        let path = day.join("rollout-thread-large.jsonl");
        let mut file = io::BufWriter::new(fs::File::create(&path).unwrap());
        for _ in 0..600_000 {
            file.write_all(b"{\"ignored\":true}\n").unwrap();
        }
        file.write_all(b"{\"type\":\"turn_context\",\"payload\":{\"effort\":\"high\"}}\n")
            .unwrap();
        file.write_all(b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"last_token_usage\":{\"input_tokens\":320},\"model_context_window\":258400}}}\n").unwrap();
        file.flush().unwrap();
        drop(file);

        let codex = fs::canonicalize(codex).unwrap();
        let mut environment = ContextEnvironment::new(temp.path().to_owned());
        environment.codex_home = Some(codex);
        let store = NoTextRolloutStore {
            text_reads: Cell::new(0),
            files: FileContextStore,
        };
        assert_eq!(
            codex_rollout_effort("large", &environment, &store).unwrap(),
            Some("high".into())
        );
        let record =
            with_codex_rollout(&ContextRecord::default(), "large", &environment, &store).unwrap();
        assert_eq!(record.context_tokens, Some(320));
        assert_eq!(record.context_window_tokens, Some(258400));
        assert_eq!(store.text_reads.get(), 0);
    }

    #[test]
    fn line_reader_handles_long_lines_and_python_universal_newlines() {
        let long = "x".repeat(20_475);
        let oversized = "y".repeat(MAX_ROLLOUT_LINE_BYTES + 1);
        let contents = format!("first\r{long}\r\n{oversized}\r\nlast");
        let mut reader = BufReader::with_capacity(7, io::Cursor::new(contents));
        let mut lines = Vec::new();
        stream_lines(&mut reader, Path::new("fixture"), &mut |line| {
            lines.push(line.to_owned());
            Ok(())
        })
        .unwrap();
        assert_eq!(lines, ["first", long.as_str(), "last"]);
    }

    fn fixture_policy() -> (tempfile::TempDir, ContextEnvironment, FileContextStore) {
        let temp = tempfile::tempdir().unwrap();
        let product = temp.path().join("product");
        let codex = temp.path().join("codex");
        let claude = temp.path().join("claude");
        fs::create_dir_all(product.join("config")).unwrap();
        fs::create_dir_all(&codex).unwrap();
        fs::create_dir_all(&claude).unwrap();
        fs::write(
            product.join("config/model-routing.json"),
            r#"{"context":{"ceiling_tokens":300000}}"#,
        )
        .unwrap();
        fs::write(codex.join("models_cache.json"),
            r#"{"models":[{"context_window":111000,"effective_context_window_percent":90},{"slug":null,"context_window":112000,"effective_context_window_percent":90},{"slug":false,"context_window":113000,"effective_context_window_percent":90},{"slug":0,"context_window":114000,"effective_context_window_percent":90},{"slug":"","context_window":115000,"effective_context_window_percent":90},{"slug":"gpt-6-luna","context_window":272000,"effective_context_window_percent":95},{"slug":"gpt-small","context_window":128000,"effective_context_window_percent":90}]}"#).unwrap();
        fs::write(
            claude.join("settings.json"),
            r#"{"autoCompactEnabled":true,"autoCompactWindow":200000}"#,
        )
        .unwrap();
        let env = ContextEnvironment {
            home: temp.path().to_owned(),
            codex_home: Some(codex),
            claude_config_dir: Some(claude),
            product_root: Some(product),
        };
        (temp, env, FileContextStore)
    }

    #[test]
    fn ceiling_policy_clamps_and_only_lowers_known_compaction_points() {
        let (_temp, env, store) = fixture_policy();
        assert_eq!(default_ceiling(&env, &store).unwrap(), 300000);
        assert_eq!(
            clamp_ceiling(Some(&json!(5000)), &env, &store).unwrap().0,
            100000
        );
        assert_eq!(
            clamp_ceiling(Some(&json!("bad")), &env, &store).unwrap().0,
            300000
        );
        assert_eq!(
            clamp_ceiling(Some(&json!(250000)), &env, &store).unwrap().0,
            250000
        );

        let mut codex =
            json!({"adapter":"codex","model":"gpt-6-luna","applied":{},"warnings":[],"argv":[]});
        apply_ceiling(&mut codex, Some(&json!(300000)), &env, &store, None).unwrap();
        assert_eq!(codex["applied"]["context_ceiling"], "provider_default");
        assert_eq!(codex["applied"]["context_ceiling_tokens"], 258400);
        let mut claude =
            json!({"adapter":"claude","model":"opus","applied":{},"warnings":[],"argv":[]});
        apply_ceiling(&mut claude, Some(&json!(150000)), &env, &store, None).unwrap();
        assert_eq!(claude["applied"]["context_ceiling"], "enforced");
        assert_eq!(claude["context_ceiling"], 150000);
        let mut unknown =
            json!({"adapter":"codex","model":"unlisted","applied":{},"warnings":[],"argv":[]});
        apply_ceiling(&mut unknown, None, &env, &store, None).unwrap();
        assert_eq!(unknown["applied"]["context_ceiling_tokens"], Value::Null);
        assert_eq!(unknown["warnings"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn falsy_codex_models_never_match_missing_or_falsy_cache_slugs() {
        let (_temp, env, store) = fixture_policy();
        for model in [
            Some(Value::Null),
            Some(json!(false)),
            Some(json!(0)),
            Some(json!("")),
            None,
        ] {
            let mut plan = json!({"adapter":"codex","applied":{},"warnings":[],"argv":[]});
            if let Some(model) = model {
                plan["model"] = model;
            }
            apply_ceiling(&mut plan, None, &env, &store, None).unwrap();
            assert_eq!(plan["applied"]["context_ceiling"], "provider_default");
            assert_eq!(plan["applied"]["context_ceiling_tokens"], Value::Null);
            assert_eq!(plan["warnings"].as_array().map(Vec::len), Some(1));
        }
    }

    const STREAM_ORACLE: &str = r#"
import importlib.util,json,sys
from pathlib import Path
root=Path(sys.argv[1])
spec=importlib.util.spec_from_file_location('context_usage',root/'skills/orchestrate/scripts/context_usage.py')
ctx=importlib.util.module_from_spec(spec);spec.loader.exec_module(ctx)
cases=json.load(sys.stdin)
out={}
for case in cases:
 meter=ctx.Meter(case['adapter'])
 for line in case['lines']: meter.observe(json.loads(line))
 value=meter.result()
 out[case['name']]={'value':value,'models':meter.models,'usage_source':ctx.usage_source(value)}
print(json.dumps(out))
"#;

    #[test]
    fn differential_streams_match_python_fixtures_and_malformed_rows() {
        let fixtures = [
            ("claude", "claude", "claude"),
            ("direct-haiku", "claude", "claude-direct-haiku"),
            ("codex", "codex", "codex"),
            ("cursor", "cursor", "cursor"),
            ("opencode", "opencode", "opencode"),
            ("agy", "agy", "agy"),
            ("kiro", "kiro", "kiro"),
        ];
        let mut cases: Vec<Value> = fixtures.into_iter().map(|(name, adapter, fixture)| {
            json!({"name":name,"adapter":adapter,"lines":fixture_lines(fixture)})
        }).collect();
        cases.push(json!({"name":"malformed","adapter":"claude","lines":[
            "null", "[]", "{\"type\":\"system\",\"subtype\":\"init\",\"model\":\"\"}",
            "{\"type\":\"assistant\",\"parent_tool_use_id\":null,\"message\":{\"model\":\"<synthetic>\",\"usage\":{\"input_tokens\":true}}}",
            "{\"type\":\"result\",\"usage\":{\"input_tokens\":\"bad\",\"output_tokens\":4,\"cache_creation_input_tokens\":3,\"output_tokens_details\":[],\"iterations\":\"ignored\"},\"modelUsage\":{\"z\":{},\"a\":{}}}"
        ]}));
        let opencode_step = r#"{"type":"step_finish","part":{"tokens":{"input":10,"output":4,"reasoning":2,"cache":{"read":5,"write":3}}}}"#;
        cases.push(json!({"name":"opencode-repeated","adapter":"opencode","lines":[opencode_step,opencode_step]}));
        let input = Value::Array(cases.clone());
        let Some(expected) = crate::differential::python(STREAM_ORACLE, &input) else {
            return;
        };
        let mut actual = Map::new();
        for case in cases {
            let name = case["name"].as_str().expect("name");
            let adapter = case["adapter"].as_str().expect("adapter");
            let mut meter = Meter::new(adapter);
            for line in case["lines"]
                .as_array()
                .expect("lines")
                .iter()
                .filter_map(Value::as_str)
            {
                meter.observe_json_line(line).unwrap();
            }
            let value = meter.result();
            actual.insert(
                name.to_owned(),
                json!({
                    "value":value, "models":meter.models(), "usage_source":usage_source(&value)
                }),
            );
        }
        crate::differential::equal(&Value::Object(actual), &expected);
    }

    const ROLLOUT_ORACLE: &str = r#"
import importlib.util,json,sys
from pathlib import Path
root=Path(sys.argv[1])
spec=importlib.util.spec_from_file_location('context_usage',root/'skills/orchestrate/scripts/context_usage.py')
ctx=importlib.util.module_from_spec(spec);spec.loader.exec_module(ctx)
cases=json.load(sys.stdin)
out={}
for case in cases:
 session=case['session']; env=case['env']
 out[case['name']]={'effort':ctx.codex_rollout_effort(session,env),
                    'value':ctx.with_codex_rollout(case['value'],session,env)}
print(json.dumps(out))
"#;

    #[test]
    fn differential_rollout_search_and_enrichment_match_python() {
        let temp = tempfile::tempdir().unwrap();
        let codex = temp.path().join("codex");
        let older = codex.join("sessions/2026/10/22");
        let latest = codex.join("sessions/2026/10/23");
        fs::create_dir_all(&older).unwrap();
        fs::create_dir_all(&latest).unwrap();
        fs::write(
            older.join("rollout-thread-1.jsonl"),
            "{\"type\":\"turn_context\",\"payload\":{\"effort\":\"medium\"}}\n{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"last_token_usage\":{\"total_tokens\":999},\"model_context_window\":1000}}}\n",
        )
        .unwrap();
        fs::write(
            latest.join("rollout-thread-1.jsonl"),
            concat!(
                "not-json\n[]\nnull\n",
                "{\"type\":\"turn_context\",\"payload\": bad}\n",
                "[\"turn_context\"]\n[\"token_count\"]\n",
                "{\"type\":\"event_msg\",\"payload\":null}\n",
                "{\"type\":\"turn_context\",\"payload\":{\"effort\":\"low\"}}\n",
                "{\"type\":\"turn_context\",\"payload\":{\"effort\":\"INVALID\"}}\n",
                "{\"type\":\"turn_context\",\"payload\":{\"effort\":\"high\"}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"last_token_usage\":{\"total_tokens\":111},\"model_context_window\":222}}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":\"bad\"}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"last_token_usage\":{\"total_tokens\":0,\"input_tokens\":300,\"output_tokens\":20},\"model_context_window\":258400}}}\n"
            ),
        )
        .unwrap();
        let codex = fs::canonicalize(codex).unwrap();
        let codex_text = codex.to_string_lossy().into_owned();
        let initial = json!({
            "context_tokens":42,"input_tokens":null,"output_tokens":null,
            "cached_input_tokens":null,"cache_write_tokens":null,"reasoning_tokens":null,
            "context_window_tokens":64,"context_percent":null,"source":"estimated"
        });
        let cases = vec![
            json!({"name":"latest","session":"thread-1","env":{"CODEX_HOME":codex_text},"value":initial}),
            json!({"name":"missing","session":"missing","env":{"CODEX_HOME":codex_text},"value":initial}),
            json!({"name":"invalid-session","session":"../thread-1","env":{"CODEX_HOME":codex_text},"value":initial}),
        ];
        let input = Value::Array(cases.clone());
        let Some(expected) = crate::differential::python(ROLLOUT_ORACLE, &input) else {
            return;
        };
        let environment = ContextEnvironment {
            home: temp.path().to_owned(),
            codex_home: Some(codex),
            claude_config_dir: None,
            product_root: None,
        };
        let store = FileContextStore;
        let mut actual = Map::new();
        for case in cases {
            let name = case["name"].as_str().expect("case name");
            let session = case["session"].as_str().expect("session");
            let value: ContextRecord =
                serde_json::from_value(case["value"].clone()).expect("context record");
            actual.insert(
                name.to_owned(),
                json!({
                    "effort":codex_rollout_effort(session,&environment,&store).unwrap(),
                    "value":with_codex_rollout(&value,session,&environment,&store).unwrap()
                }),
            );
        }
        crate::differential::equal(&Value::Object(actual), &expected);
    }


}
