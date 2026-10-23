//! Append-only tracker queue validation, idempotent replay and bounded digest.

use serde::{ser::SerializeMap, Serialize, Serializer};
use std::{
    collections::BTreeSet,
    io,
    path::{Path, PathBuf},
};
use thiserror::Error;

pub const EVENT_TYPES: &[&str] = &[
    "lane_followup",
    "pr_opened",
    "pr_landed",
    "lane_abandoned",
    "owner_decision",
    "run_sweep",
];
pub const REQUIRED_EVENT_FIELDS: &[&str] = &["id", "payload", "type"];
pub const REQUIRED_OUTCOME_FIELDS: &[&str] = &["status", "summary"];
pub const DEFAULT_MAX_DIGEST_LINES: i64 = 15;

pub type QueueValue = crate::pending::OrderedValue;

/// The queue-file store is the effect seam; policy functions can be tested
/// against a memory-backed or filesystem-backed implementation.
pub trait QueueStore {
    fn is_file(&self, path: &Path) -> bool;
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct OsQueueStore;

impl QueueStore for OsQueueStore {
    fn is_file(&self, path: &Path) -> bool {
        std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }

    fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
        std::fs::write(path, contents)
    }
}

#[derive(Debug, Error)]
pub enum QueueError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{path}:{line}: invalid JSON: {detail}")]
    InvalidJson {
        path: PathBuf,
        line: usize,
        detail: String,
    },
    #[error(transparent)]
    NumberOutOfRange(#[from] provenant_records::checked_json::NumberOutOfRange),
    #[error("{path}:{line}: event needs ['id', 'payload', 'type']")]
    EventFields { path: PathBuf, line: usize },
    #[error("{path}:{line}: unknown event type {value}")]
    UnknownType {
        path: PathBuf,
        line: usize,
        value: String,
    },
    #[error("{path}:{line}: id must be a non-empty string")]
    InvalidId { path: PathBuf, line: usize },
    #[error("{path}:{line}: outcome needs ['status', 'summary']")]
    OutcomeFields { path: PathBuf, line: usize },
    #[error("{path}: duplicate event id(s): {ids}")]
    DuplicateIds { path: PathBuf, ids: String },
    #[error("outcome needs ['status', 'summary']")]
    InvalidOutcome,
    #[error("{path}: unknown event id {id}")]
    UnknownId { path: PathBuf, id: String },
    #[error("handler for {id} must return ['status', 'summary']")]
    InvalidHandlerOutcome { id: String },
    #[error("event type has an unhashable {kind}")]
    UnhashableType { kind: &'static str },
    #[error("{0}")]
    Handler(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueueEvent {
    fields: Vec<(String, QueueValue)>,
}

impl QueueEvent {
    fn from_value(value: QueueValue) -> Option<Self> {
        let QueueValue::Object(object) = value else {
            return None;
        };
        Some(Self {
            fields: object
                .entries()
                .map(|(key, value)| (key.to_owned(), value.clone()))
                .collect(),
        })
    }

    pub fn get(&self, name: &str) -> Option<&QueueValue> {
        self.fields
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
    }

    pub fn id(&self) -> Option<&str> {
        self.get("id").and_then(QueueValue::as_str)
    }

    pub fn has_outcome(&self) -> bool {
        self.fields.iter().any(|(key, _)| key == "outcome")
    }

    fn with_outcome(&self, outcome: QueueValue) -> Self {
        let mut event = self.clone();
        if let Some((_, value)) = event.fields.iter_mut().find(|(key, _)| key == "outcome") {
            *value = outcome;
        } else {
            event.fields.push(("outcome".to_owned(), outcome));
        }
        event
    }
}

impl Serialize for QueueEvent {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.fields.len()))?;
        for (key, value) in &self.fields {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

fn ordered_decode(raw: &[u8]) -> Result<QueueValue, provenant_records::checked_json::DecodeError> {
    crate::pending::checked_ordered_decode(raw)
}

fn has_fields(value: &QueueValue, required: &[&str]) -> bool {
    let QueueValue::Object(object) = value else {
        return false;
    };
    let present: BTreeSet<_> = object.entries().map(|(key, _)| key).collect();
    required.iter().all(|key| present.contains(key))
}

fn python_repr(value: &QueueValue) -> String {
    match value {
        QueueValue::Null => "None".to_owned(),
        QueueValue::Bool(true) => "True".to_owned(),
        QueueValue::Bool(false) => "False".to_owned(),
        QueueValue::Number(number) => {
            provenant_core::py::python_str(&serde_json::Value::Number(number.clone()))
        }
        QueueValue::String(value) => provenant_core::py::python_repr(value),
        QueueValue::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(python_repr)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        QueueValue::Object(object) => format!(
            "{{{}}}",
            object
                .entries()
                .map(|(key, value)| format!(
                    "{}: {}",
                    provenant_core::py::python_repr(key),
                    python_repr(value)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn python_json(value: &QueueValue) -> String {
    match value {
        QueueValue::Null => "null".to_owned(),
        QueueValue::Bool(value) => value.to_string(),
        QueueValue::Number(value) => {
            provenant_core::py::python_str(&serde_json::Value::Number(value.clone()))
        }
        QueueValue::String(value) => {
            crate::pending::python_json(&serde_json::Value::String(value.clone()))
        }
        QueueValue::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(python_json)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        QueueValue::Object(values) => {
            let mut entries: Vec<_> = values.entries().collect();
            entries.sort_by(|left, right| left.0.cmp(right.0));
            format!(
                "{{{}}}",
                entries
                    .into_iter()
                    .map(|(key, value)| format!(
                        "{}: {}",
                        crate::pending::python_json(&serde_json::Value::String(key.to_owned())),
                        python_json(value)
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    }
}

fn sorted_json_event(event: &QueueEvent) -> String {
    let mut fields: Vec<_> = event.fields.iter().collect();
    fields.sort_by(|left, right| left.0.cmp(&right.0));
    format!(
        "{{{}}}",
        fields
            .into_iter()
            .map(|(key, value)| format!(
                "{}: {}",
                crate::pending::python_json(&serde_json::Value::String(key.clone())),
                python_json(value)
            ))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn load_events_inner(path: &Path, store: &impl QueueStore) -> Result<Vec<QueueEvent>, QueueError> {
    if !store.is_file(path) {
        return Ok(Vec::new());
    }
    let bytes = store.read(path).map_err(|source| QueueError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let content = String::from_utf8(bytes).map_err(|error| QueueError::InvalidJson {
        path: path.to_path_buf(),
        line: 1,
        detail: error.to_string(),
    })?;
    let mut events = Vec::new();
    for (line_index, raw) in provenant_core::py::python_splitlines(&content)
        .into_iter()
        .enumerate()
    {
        let line_number = line_index + 1;
        let line = provenant_core::py::python_strip(raw);
        if line.is_empty() {
            continue;
        }
        let value = ordered_decode(line.as_bytes()).map_err(|error| match error {
            provenant_records::checked_json::DecodeError::Json(error) => QueueError::InvalidJson {
                path: path.to_path_buf(),
                line: line_number,
                detail: error.to_string(),
            },
            provenant_records::checked_json::DecodeError::NumberOutOfRange(error) => {
                QueueError::NumberOutOfRange(error)
            }
        })?;
        let Some(event) = QueueEvent::from_value(value) else {
            return Err(QueueError::EventFields {
                path: path.to_path_buf(),
                line: line_number,
            });
        };
        if !REQUIRED_EVENT_FIELDS
            .iter()
            .all(|key| event.get(key).is_some())
        {
            return Err(QueueError::EventFields {
                path: path.to_path_buf(),
                line: line_number,
            });
        }
        let Some(event_type) = event.get("type") else {
            return Err(QueueError::EventFields {
                path: path.to_path_buf(),
                line: line_number,
            });
        };
        match event_type {
            QueueValue::Array(_) => return Err(QueueError::UnhashableType { kind: "list" }),
            QueueValue::Object(_) => return Err(QueueError::UnhashableType { kind: "dict" }),
            QueueValue::String(value) if EVENT_TYPES.contains(&value.as_str()) => {}
            value => {
                return Err(QueueError::UnknownType {
                    path: path.to_path_buf(),
                    line: line_number,
                    value: python_repr(value),
                });
            }
        }
        if !event
            .get("id")
            .is_some_and(|value| matches!(value, QueueValue::String(id) if !id.is_empty()))
        {
            return Err(QueueError::InvalidId {
                path: path.to_path_buf(),
                line: line_number,
            });
        }
        if let Some(outcome) = event.get("outcome") {
            if !matches!(outcome, QueueValue::Null) && !has_fields(outcome, REQUIRED_OUTCOME_FIELDS)
            {
                return Err(QueueError::OutcomeFields {
                    path: path.to_path_buf(),
                    line: line_number,
                });
            }
        }
        events.push(event);
    }
    let mut seen = BTreeSet::<String>::new();
    let mut duplicates = BTreeSet::<String>::new();
    for event in &events {
        if let Some(id) = event.id() {
            if !seen.insert(id.to_owned()) {
                duplicates.insert(id.to_owned());
            }
        }
    }
    if !duplicates.is_empty() {
        let ids = format!(
            "[{}]",
            duplicates
                .iter()
                .map(|id| provenant_core::py::python_repr(id))
                .collect::<Vec<_>>()
                .join(", ")
        );
        return Err(QueueError::DuplicateIds {
            path: path.to_path_buf(),
            ids,
        });
    }
    Ok(events)
}

pub fn load_events(path: &Path, store: &impl QueueStore) -> Result<Vec<QueueEvent>, QueueError> {
    load_events_inner(path, store)
}

pub fn pending(events: &[QueueEvent]) -> Vec<QueueEvent> {
    events
        .iter()
        .filter(|event| !event.has_outcome())
        .cloned()
        .collect()
}

fn write_events(
    path: &Path,
    events: &[QueueEvent],
    store: &impl QueueStore,
) -> Result<(), QueueError> {
    let mut contents = events
        .iter()
        .map(sorted_json_event)
        .collect::<Vec<_>>()
        .join("\n");
    if !contents.is_empty() {
        contents.push('\n');
    }
    store
        .write(path, contents.as_bytes())
        .map_err(|source| QueueError::Io {
            path: path.to_path_buf(),
            source,
        })
}

pub fn record_outcome(
    path: &Path,
    event_id: &str,
    outcome: &QueueValue,
    store: &impl QueueStore,
) -> Result<bool, QueueError> {
    if !has_fields(outcome, REQUIRED_OUTCOME_FIELDS) {
        return Err(QueueError::InvalidOutcome);
    }
    let mut events = load_events_inner(path, store)?;
    for event in &mut events {
        if event.id() == Some(event_id) {
            if event.has_outcome() {
                return Ok(false);
            }
            *event = event.with_outcome(outcome.clone());
            write_events(path, &events, store)?;
            return Ok(true);
        }
    }
    Err(QueueError::UnknownId {
        path: path.to_path_buf(),
        id: provenant_core::py::python_repr(event_id),
    })
}

pub trait ReplayHandler {
    type Error: std::fmt::Display;
    fn handle(&mut self, event: &QueueEvent) -> Result<QueueValue, Self::Error>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StewardCommand {
    Pending,
    Record {
        id: String,
        status: String,
        summary: String,
    },
    Digest {
        max_lines: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StewardOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Execute the inert policy behind the CLI's three public commands. Argument
/// parsing and process I/O remain with the eventual command adapter.
pub fn execute_command(
    path: &Path,
    command: StewardCommand,
    store: &impl QueueStore,
) -> StewardOutput {
    let result = (|| -> Result<StewardOutput, QueueError> {
        match command {
            StewardCommand::Pending => {
                let events = pending(&load_events_inner(path, store)?);
                let body = events
                    .iter()
                    .map(sorted_json_event)
                    .collect::<Vec<_>>()
                    .join(", ");
                Ok(StewardOutput {
                    exit_code: 0,
                    stdout: format!("[{body}]\n"),
                    stderr: String::new(),
                })
            }
            StewardCommand::Record {
                id,
                status,
                summary,
            } => {
                let mut outcome = crate::pending::OrderedMap::default();
                outcome.insert("status".to_owned(), QueueValue::String(status));
                outcome.insert("summary".to_owned(), QueueValue::String(summary));
                let changed = record_outcome(path, &id, &QueueValue::Object(outcome), store)?;
                Ok(StewardOutput {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: if changed {
                        String::new()
                    } else {
                        format!("tracker steward: {id} already recorded; idempotent no-op\n")
                    },
                })
            }
            StewardCommand::Digest { max_lines } => {
                let events: Vec<_> = load_events_inner(path, store)?
                    .into_iter()
                    .filter(QueueEvent::has_outcome)
                    .collect();
                Ok(StewardOutput {
                    exit_code: 0,
                    stdout: format!("{}\n", digest(&events, max_lines)?),
                    stderr: String::new(),
                })
            }
        }
    })();
    result.unwrap_or_else(|error| StewardOutput {
        exit_code: 2,
        stdout: String::new(),
        stderr: format!("tracker steward: {error}\n"),
    })
}

impl<F, E> ReplayHandler for F
where
    F: FnMut(&QueueEvent) -> Result<QueueValue, E>,
    E: std::fmt::Display,
{
    type Error = E;
    fn handle(&mut self, event: &QueueEvent) -> Result<QueueValue, Self::Error> {
        self(event)
    }
}

pub fn replay<H: ReplayHandler>(
    path: &Path,
    mut handler: H,
    store: &impl QueueStore,
) -> Result<Vec<QueueEvent>, QueueError> {
    let mut events = load_events_inner(path, store)?;
    let mut processed = Vec::new();
    for event in &mut events {
        if event.has_outcome() {
            continue;
        }
        let id = event.id().unwrap_or_default().to_owned();
        let outcome = handler
            .handle(event)
            .map_err(|error| QueueError::Handler(error.to_string()))?;
        if !has_fields(&outcome, REQUIRED_OUTCOME_FIELDS) {
            return Err(QueueError::InvalidHandlerOutcome { id });
        }
        *event = event.with_outcome(outcome);
        processed.push(event.clone());
    }
    if !processed.is_empty() {
        write_events(path, &events, store)?;
    }
    Ok(processed)
}

pub fn digest(processed: &[QueueEvent], max_lines: i64) -> Result<String, QueueError> {
    let mut lines = Vec::with_capacity(processed.len());
    for event in processed {
        let event_type = event.get("type").ok_or(QueueError::InvalidOutcome)?;
        let id = event.get("id").ok_or(QueueError::InvalidOutcome)?;
        let outcome = event.get("outcome").ok_or(QueueError::InvalidOutcome)?;
        let status = outcome
            .as_object_field("status")
            .ok_or(QueueError::InvalidOutcome)?;
        let summary = outcome
            .as_object_field("summary")
            .ok_or(QueueError::InvalidOutcome)?;
        let flagged = matches!(status, QueueValue::String(value) if value == "flagged");
        lines.push(format!(
            "{}{} {}: {}",
            if flagged { "! " } else { "" },
            display_value(event_type),
            display_value(id),
            display_value(summary),
        ));
    }
    if i64::try_from(lines.len()).unwrap_or(i64::MAX) <= max_lines {
        return Ok(lines.join("\n"));
    }
    let keep = max_lines.saturating_sub(1).max(0) as usize;
    let mut output = lines.into_iter().take(keep).collect::<Vec<_>>();
    let remaining = processed.len().saturating_sub(output.len());
    output.push(format!("... +{remaining} more"));
    Ok(output.join("\n"))
}

trait ObjectField {
    fn as_object_field(&self, key: &str) -> Option<&QueueValue>;
}

impl ObjectField for QueueValue {
    fn as_object_field(&self, key: &str) -> Option<&QueueValue> {
        match self {
            QueueValue::Object(object) => object
                .entries()
                .find_map(|(name, value)| (name == key).then_some(value)),
            _ => None,
        }
    }
}

fn display_value(value: &QueueValue) -> String {
    match value {
        QueueValue::String(value) => value.clone(),
        QueueValue::Null => "None".to_owned(),
        QueueValue::Bool(true) => "True".to_owned(),
        QueueValue::Bool(false) => "False".to_owned(),
        QueueValue::Number(number) => {
            provenant_core::py::python_str(&serde_json::Value::Number(number.clone()))
        }
        QueueValue::Array(_) | QueueValue::Object(_) => python_repr(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn oracle(cases: serde_json::Value) -> Option<serde_json::Value> {
        crate::test_support::oracle_script("oracle_session.py", "steward", &cases)
    }

    fn event(id: &str, event_type: &str) -> QueueValue {
        crate::pending::checked_ordered_decode(
            json!({"id":id,"type":event_type,"payload":{}})
                .to_string()
                .as_bytes(),
        )
        .unwrap()
    }

    #[test]
    fn replay_processes_only_pending_events_and_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("tracker-queue.jsonl");
        let initial = "{\"id\":\"evt-1\",\"type\":\"pr_opened\",\"payload\":{}}\n{\"id\":\"evt-2\",\"type\":\"run_sweep\",\"payload\":{}}\n";
        std::fs::write(&path, initial).unwrap();
        let oracle_path = temp.path().join("oracle-queue.jsonl");
        std::fs::write(&oracle_path, initial).unwrap();
        let expected = oracle(json!([{"op":"replay","path":oracle_path}]))
            .map(|value| value[0]["processed"].clone());
        let store = OsQueueStore;
        let mut calls = Vec::new();
        let processed = replay(
            &path,
            |event: &QueueEvent| {
                calls.push(event.id().unwrap_or_default().to_owned());
                Ok::<_, String>(crate::pending::checked_ordered_decode(
                    json!({"status":"done","summary":format!("handled {}",event.id().unwrap_or_default())}).to_string().as_bytes(),
                ).unwrap())
            },
            &store,
        )
        .unwrap();
        assert_eq!(calls, ["evt-1", "evt-2"]);
        assert_eq!(processed.len(), 2);
        assert!(replay(
            &path,
            |_event: &QueueEvent| -> Result<QueueValue, String> {
                panic!("already processed event was called")
            },
            &store,
        )
        .unwrap()
        .is_empty());
        let events = load_events(&path, &store).unwrap();
        assert!(pending(&events).is_empty());
        assert!(std::str::from_utf8(&store.read(&path).unwrap()).unwrap().starts_with("{\"id\": \"evt-1\", \"outcome\": {\"status\": \"done\", \"summary\": \"handled evt-1\"}"));
        if let Some(expected) = expected {
            assert_eq!(json!(processed), expected);
        }
    }

    #[test]
    fn outcome_recording_and_digest_preserve_idempotence_and_limits() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("queue.jsonl");
        std::fs::write(
            &path,
            "{\"id\":\"evt-1\",\"type\":\"owner_decision\",\"payload\":{}}\n",
        )
        .unwrap();
        let store = OsQueueStore;
        let outcome = crate::pending::checked_ordered_decode(
            br#"{"status":"flagged","summary":"possible duplicate of #42"}"#,
        )
        .unwrap();
        assert!(record_outcome(&path, "evt-1", &outcome, &store).unwrap());
        assert!(!record_outcome(&path, "evt-1", &outcome, &store).unwrap());
        let processed = load_events(&path, &store).unwrap();
        assert_eq!(
            digest(&processed, DEFAULT_MAX_DIGEST_LINES).unwrap(),
            "! owner_decision evt-1: possible duplicate of #42"
        );
        if let Some(expected) = oracle(json!([{"op":"digest","path":path,"max_lines":15}])) {
            assert_eq!(
                json!(digest(&processed, DEFAULT_MAX_DIGEST_LINES).unwrap()),
                expected[0]
            );
        }
        let many = (0..20)
            .map(|index| {
                let base = event(&format!("evt-{index}"), "run_sweep");
                let outcome = crate::pending::checked_ordered_decode(
                    json!({"status":"done","summary":format!("swept {index}")})
                        .to_string()
                        .as_bytes(),
                )
                .unwrap();
                QueueEvent::from_value(base).unwrap().with_outcome(outcome)
            })
            .collect::<Vec<_>>();
        let rendered = digest(&many, 15).unwrap();
        assert_eq!(rendered.lines().count(), 15);
        assert_eq!(rendered.lines().last(), Some("... +6 more"));
        assert_eq!(digest(&many, 0).unwrap(), "... +20 more");
    }

    #[test]
    fn digest_uses_python_string_rendering_for_list_and_dict_summaries() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("queue.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"id\":\"evt-list\",\"type\":\"run_sweep\",\"payload\":{},\"outcome\":{\"status\":\"done\",\"summary\":[1,\"two\",{\"a\":1}]}}\n",
                "{\"id\":\"evt-dict\",\"type\":\"owner_decision\",\"payload\":{},\"outcome\":{\"status\":\"flagged\",\"summary\":{\"a\":1,\"b\":\"two\"}}}\n"
            ),
        )
        .unwrap();
        let events = load_events(&path, &OsQueueStore).unwrap();
        let actual = digest(&events, DEFAULT_MAX_DIGEST_LINES).unwrap();
        assert_eq!(
            actual,
            "run_sweep evt-list: [1, 'two', {'a': 1}]\n! owner_decision evt-dict: {'a': 1, 'b': 'two'}"
        );
        if let Some(expected) = oracle(json!([{"op":"digest","path":path}])) {
            assert_eq!(json!(actual), expected[0]);
        }
    }

    #[test]
    fn malformed_and_duplicate_queue_entries_are_typed_refusals() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("queue.jsonl");
        let store = OsQueueStore;
        std::fs::write(&path, "{\"id\":\"dup\",\"type\":\"run_sweep\",\"payload\":{}}\n{\"id\":\"dup\",\"type\":\"run_sweep\",\"payload\":{}}\n").unwrap();
        assert!(matches!(
            load_events(&path, &store),
            Err(QueueError::DuplicateIds { .. })
        ));
        std::fs::write(
            &path,
            "{\"id\":\"evt-1\",\"type\":\"batch_wave\",\"payload\":{}}\n",
        )
        .unwrap();
        assert!(matches!(
            load_events(&path, &store),
            Err(QueueError::UnknownType { .. })
        ));
        std::fs::write(&path, "{bad json}\n").unwrap();
        assert!(matches!(
            load_events(&path, &store),
            Err(QueueError::InvalidJson { .. })
        ));
    }

    #[test]
    fn replay_skips_previously_recorded_events_and_cli_matches_python() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("queue.jsonl");
        let oracle_path = temp.path().join("oracle.jsonl");
        let initial = "{\"id\":\"evt-1\",\"type\":\"pr_landed\",\"payload\":{},\"outcome\":{\"status\":\"done\",\"summary\":\"closed #1\"}}\n{\"id\":\"evt-2\",\"type\":\"lane_followup\",\"payload\":{}}\n";
        std::fs::write(&path, initial).unwrap();
        std::fs::write(&oracle_path, initial).unwrap();
        let store = OsQueueStore;
        let mut calls = Vec::new();
        let processed = replay(
            &path,
            |event: &QueueEvent| {
                calls.push(event.id().unwrap_or_default().to_owned());
                Ok::<_, String>(
                    crate::pending::checked_ordered_decode(
                        br#"{"status":"flagged","summary":"needs owner call"}"#,
                    )
                    .unwrap(),
                )
            },
            &store,
        )
        .unwrap();
        assert_eq!(calls, ["evt-2"]);
        assert_eq!(processed.len(), 1);

        let _ = oracle(json!([{
            "op":"record",
            "path":oracle_path,
            "id":"evt-2",
            "outcome":{"status":"flagged","summary":"needs owner call"}
        }]));
        let actual = execute_command(&path, StewardCommand::Pending, &store);
        assert_eq!(actual.exit_code, 0);
        assert_eq!(actual.stderr, "");
        let expected = oracle(json!([{"op":"cli","path":oracle_path,"argv":["pending"]}])).unwrap();
        assert_eq!(
            json!({"exit_code":actual.exit_code,"stdout":actual.stdout,"stderr":actual.stderr}),
            expected[0]
        );

        let report = execute_command(&path, StewardCommand::Digest { max_lines: 15 }, &store);
        assert_eq!(
            report.stdout,
            "pr_landed evt-1: closed #1\n! lane_followup evt-2: needs owner call\n"
        );
        let bad = execute_command(
            &path,
            StewardCommand::Record {
                id: "missing".to_owned(),
                status: "done".to_owned(),
                summary: "none".to_owned(),
            },
            &store,
        );
        assert_eq!(bad.exit_code, 2);
        assert!(bad.stderr.starts_with("tracker steward: "));
    }
}
