//! Cursor persistence and document/OpenCode source adapters.
//!
//! Filesystem ownership is injected. The current crate layering has no safe
//! no-follow open API: callers must obtain verified handles from the future
//! supervisor owner before calling these adapters.

use crate::{
    ingest::{self, Cursor, CursorStore, ParserState, Snapshot},
    pending, Error, Event, Numeric, Result, Sink, Usage,
};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{BufReader, Read},
    path::{Path, PathBuf},
};

/// The opened file and the stat values captured by the trusted opener.
pub struct VerifiedFile {
    pub stream: BufReader<File>,
    pub snapshot: Snapshot,
}

/// Safe owner seam for `open_verified`.
///
/// Implementations must refuse a final symlink, non-regular files and files
/// whose device/inode differs from `expected`. A supervisor-backed
/// implementation is blocked because R2 does not allow telemetry to depend on
/// supervisor.
pub trait VerifiedFileOpener {
    fn open_verified(&self, path: &Path, expected: Option<(u64, u64)>) -> Result<VerifiedFile>;
}

pub struct VerifiedOpenCodeDatabase {
    pub connection: Connection,
    pub snapshot: Snapshot,
}

/// The source owner must create a read-only SQLite connection only after
/// proving that its path still names the discovered regular-file inode. The
/// supervisor-backed implementation is blocked because R2 does not allow
/// telemetry to depend on supervisor.
pub trait VerifiedOpenCodeOpener {
    fn open_readonly(
        &self,
        path: &Path,
        expected: Option<(u64, u64)>,
    ) -> Result<VerifiedOpenCodeDatabase>;
}

/// Run the JSONL cursor policy against a handle returned by the verified-open
/// owner. This keeps path discovery and OS-specific no-follow behavior outside
/// the telemetry crate.
pub fn ingest_verified_jsonl(
    opener: &impl VerifiedFileOpener,
    store: &mut impl CursorStore,
    source: &str,
    path: &Path,
    expected: Option<(u64, u64)>,
    parse: impl FnMut(&Value, &mut ParserState, u64) -> Result<()>,
) -> Result<()> {
    let mut opened = opener.open_verified(path, expected)?;
    ingest::ingest_stream(
        store,
        source,
        path,
        &mut opened.stream,
        opened.snapshot,
        parse,
    )
}

/// A document handle with the metadata observed through the same path.
pub struct DocumentFile {
    pub stream: BufReader<File>,
    pub snapshot: Snapshot,
}

/// Document sources use `Path.stat` and `Path.read_text`, both of which follow
/// symlinks. Keep that policy distinct from verified JSONL sources.
pub trait DocumentFileOpener {
    fn open_following(&self, path: &Path) -> Result<DocumentFile>;
}

/// Read, reduce and cursor one document through the path-following document
/// seam. Returns `false` when size and mtime prove it is unchanged since the
/// last successful ingest.
pub fn ingest_document_file(
    opener: &impl DocumentFileOpener,
    cursors: &mut impl JsonCursorStore,
    sink: &mut impl DocumentSink,
    source: &str,
    path: &Path,
    identity: &str,
    receipt_error_class: Option<String>,
) -> Result<bool> {
    let mut opened = opener.open_following(path)?;
    let snapshot = opened.snapshot;
    let old = cursors.load_json(source, path)?;
    if old
        .as_ref()
        .is_some_and(|cursor| cursor.size == snapshot.size && cursor.mtime == snapshot.mtime_ns)
    {
        return Ok(false);
    }
    let mut bytes = Vec::new();
    opened
        .stream
        .by_ref()
        .take((crate::MAX_JSON_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|source| Error::Io {
            path: path.into(),
            source,
        })?;
    if bytes.len() > crate::MAX_JSON_BYTES {
        return Err(Error::InputTooLarge {
            limit: crate::MAX_JSON_BYTES,
        });
    }
    let row: Value = pending::decode_json(&bytes)?;
    ingest_document(sink, source, &row, path, identity, receipt_error_class)?;
    cursors.save_json(JsonCursor {
        source: source.into(),
        path: path_text(path)?,
        size: snapshot.size,
        mtime: snapshot.mtime_ns,
        offset: snapshot.size,
        state: json!({}),
    })?;
    Ok(true)
}

/// JSON form of a ledger cursor, also used for OpenCode's source-specific
/// watermark state.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JsonCursor {
    pub source: String,
    pub path: String,
    pub size: u64,
    pub mtime: i64,
    pub offset: u64,
    pub state: Value,
}

/// Storage contract shared by line cursors and source-specific cursors.
pub trait JsonCursorStore {
    fn load_json(&mut self, source: &str, path: &Path) -> Result<Option<JsonCursor>>;
    fn clear_file(&mut self, source: &str, path: &Path) -> Result<()>;
    fn save_json(&mut self, cursor: JsonCursor) -> Result<()>;
}

/// SQLite cursor owner. Use it inside the caller's ledger transaction.
pub struct SqliteCursorStore<'a> {
    connection: &'a Connection,
}

impl<'a> SqliteCursorStore<'a> {
    pub fn new(connection: &'a Connection) -> Self {
        Self { connection }
    }

    fn path_text(path: &Path) -> Result<&str> {
        path.to_str()
            .ok_or_else(|| Error::Invalid("telemetry cursor path is not valid UTF-8".into()))
    }
}

impl JsonCursorStore for SqliteCursorStore<'_> {
    fn load_json(&mut self, source: &str, path: &Path) -> Result<Option<JsonCursor>> {
        let path_text = Self::path_text(path)?;
        let row = self
            .connection
            .query_row(
                "SELECT source,path,size,mtime,offset,state FROM cursors WHERE source=?1 AND path=?2",
                params![source, path_text],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()
            .map_err(Error::Sqlite)?;
        row.map(|(source, path, size, mtime, offset, state)| {
            Ok(JsonCursor {
                source,
                path,
                size: u64::try_from(size)
                    .map_err(|_| Error::Invalid("negative telemetry cursor size".into()))?,
                mtime,
                offset: u64::try_from(offset)
                    .map_err(|_| Error::Invalid("negative telemetry cursor offset".into()))?,
                state: pending::decode_json(state.as_bytes())?,
            })
        })
        .transpose()
    }

    fn clear_file(&mut self, source: &str, path: &Path) -> Result<()> {
        let path = Self::path_text(path)?;
        let mut statement = self
            .connection
            .prepare("SELECT event_id,host FROM usage_files WHERE source=?1 AND path=?2")
            .map_err(Error::Sqlite)?;
        let rows = statement
            .query_map(params![source, path], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(Error::Sqlite)?;
        let mut owned = Vec::new();
        for row in rows {
            owned.push(row.map_err(Error::Sqlite)?);
        }
        drop(statement);
        self.connection
            .execute(
                "DELETE FROM usage_files WHERE source=?1 AND path=?2",
                params![source, path],
            )
            .map_err(Error::Sqlite)?;
        for (event_id, host) in owned {
            self.connection
                .execute(
                    "DELETE FROM usage WHERE source=?1 AND event_id=?2 AND host=?3 AND NOT EXISTS \
                     (SELECT 1 FROM usage_files f WHERE f.source=usage.source \
                     AND f.event_id=usage.event_id AND f.host=usage.host)",
                    params![source, event_id, host],
                )
                .map_err(Error::Sqlite)?;
        }
        self.connection
            .execute(
                "DELETE FROM observations WHERE source=?1 AND origin_path=?2",
                params![source, path],
            )
            .map_err(Error::Sqlite)?;
        Ok(())
    }

    fn save_json(&mut self, cursor: JsonCursor) -> Result<()> {
        let state = serde_json::to_string(&cursor.state)?;
        let size = i64::try_from(cursor.size).map_err(|_| Error::NumberOutOfRange)?;
        let offset = i64::try_from(cursor.offset).map_err(|_| Error::NumberOutOfRange)?;
        self.connection
            .execute(
                "INSERT OR REPLACE INTO cursors(source,path,size,mtime,offset,state) \
                 VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    cursor.source,
                    cursor.path,
                    size,
                    cursor.mtime,
                    offset,
                    state
                ],
            )
            .map_err(Error::Sqlite)?;
        Ok(())
    }
}

impl CursorStore for SqliteCursorStore<'_> {
    fn load(&mut self, source: &str, path: &Path) -> Result<Option<Cursor>> {
        self.load_json(source, path)?
            .map(|cursor| {
                Ok(Cursor {
                    source: cursor.source,
                    path: cursor.path,
                    size: cursor.size,
                    mtime: cursor.mtime,
                    offset: cursor.offset,
                    state: serde_json::from_value(cursor.state)?,
                })
            })
            .transpose()
    }

    fn clear_file(&mut self, source: &str, path: &Path) -> Result<()> {
        JsonCursorStore::clear_file(self, source, path)
    }

    fn save(&mut self, cursor: Cursor) -> Result<()> {
        self.save_json(JsonCursor {
            source: cursor.source,
            path: cursor.path,
            size: cursor.size,
            mtime: cursor.mtime,
            offset: cursor.offset,
            state: serde_json::to_value(cursor.state)?,
        })
    }
}

/// Every shape the document adapter accepts. Keeping this table next to the
/// reducers makes a new source an added row plus its reducer.
#[derive(Clone, Copy)]
struct DocumentVariant {
    source: &'static str,
    reducer: fn(&Value, &str, &str, Option<&str>) -> Result<DocumentEffects>,
}

const DOCUMENT_VARIANTS: &[DocumentVariant] = &[
    DocumentVariant {
        source: "fabric",
        reducer: reduce_fabric_document,
    },
    DocumentVariant {
        source: "route_health",
        reducer: reduce_route_document,
    },
    DocumentVariant {
        source: "cooldowns",
        reducer: reduce_cooldown_document,
    },
];

const PROVIDERS: &[(&str, &str)] = &[
    ("claude", "anthropic"),
    ("codex", "openai"),
    ("cursor", "cursor"),
    ("opencode", "opencode"),
    ("agy", "google"),
];

/// Effects produced by one document. `ObservationRow` carries database fields
/// not yet present in the shared observation record.
#[derive(Clone, Debug, Default)]
pub struct DocumentEffects {
    pub usage: Option<Usage>,
    pub observations: Vec<ObservationRow>,
    pub events: Vec<Event>,
    pub harness_stamp: Option<HarnessStamp>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ObservationRow {
    pub source: String,
    pub origin_path: String,
    pub provider: Option<String>,
    pub adapter: Option<String>,
    pub model: Option<String>,
    pub account: Option<String>,
    pub timestamp: Option<String>,
    pub run_id: Option<String>,
    pub status: Option<String>,
    pub reset_at: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct HarnessStamp {
    pub digest: String,
    pub preset: Option<String>,
    pub knobs: Value,
    pub first_seen: Option<String>,
}

/// Composition owners persist the full source rows and surrounding receipt
/// metadata that the generic `Sink` intentionally does not own.
pub trait DocumentSink: Sink {
    fn observation_row(&mut self, row: ObservationRow) -> Result<()>;
    fn harness_stamp(&mut self, row: HarnessStamp) -> Result<()>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceAdapter {
    FabricDocument,
    RouteHealthDocument,
    CooldownDocument,
    OpenCode,
    OtelJsonl,
    Delivery,
    ClaudeJsonl,
    CodexJsonl,
}

#[derive(Clone, Copy)]
struct SourceAdapterRow {
    source: &'static str,
    adapter: SourceAdapter,
    uses_registry: bool,
}

const SOURCE_ADAPTERS: &[SourceAdapterRow] = &[
    SourceAdapterRow {
        source: "fabric",
        adapter: SourceAdapter::FabricDocument,
        uses_registry: true,
    },
    SourceAdapterRow {
        source: "route_health",
        adapter: SourceAdapter::RouteHealthDocument,
        uses_registry: false,
    },
    SourceAdapterRow {
        source: "cooldowns",
        adapter: SourceAdapter::CooldownDocument,
        uses_registry: false,
    },
    SourceAdapterRow {
        source: "opencode",
        adapter: SourceAdapter::OpenCode,
        uses_registry: false,
    },
    SourceAdapterRow {
        source: "otel",
        adapter: SourceAdapter::OtelJsonl,
        uses_registry: false,
    },
    SourceAdapterRow {
        source: "delivery",
        adapter: SourceAdapter::Delivery,
        uses_registry: true,
    },
    SourceAdapterRow {
        source: "claude",
        adapter: SourceAdapter::ClaudeJsonl,
        uses_registry: false,
    },
    SourceAdapterRow {
        source: "codex",
        adapter: SourceAdapter::CodexJsonl,
        uses_registry: false,
    },
];

#[derive(Clone, Debug)]
pub struct DiscoveredPath {
    pub path: PathBuf,
    pub account_label: Option<String>,
    pub inode: Option<(u64, u64)>,
}

#[derive(Clone, Debug)]
pub struct SourcePlan {
    pub source: String,
    pub paths: Vec<DiscoveredPath>,
}

/// Boundary for the telemetry ledger owner and source registry. The
/// orchestrator owns per-source error/status/rebuild policy; this trait leaves
/// path discovery, owner assignment, transaction mechanics and row counting
/// with their existing owners.
pub trait IngestRuntime {
    /// Equivalent to `attribute_accounts` inside its own write transaction.
    fn attribute_accounts(&mut self) -> Result<()>;
    /// Equivalent to `owners`; implementations retain the per-source callback
    /// that maps an account label to the first file owner.
    fn load_owners(&mut self) -> Result<()>;
    fn source_paths(&mut self) -> Result<Vec<SourcePlan>>;
    /// Resolve current Fabric or delivery registry paths. Failure counts as a
    /// source error while the base paths from `source_paths` are still tried.
    fn registry_paths(&mut self, source: &str) -> Result<Vec<DiscoveredPath>>;
    /// Process one file atomically with the ledger, including an OpenCode,
    /// document, OTEL, delivery, Claude or Codex adapter.
    fn process_file(
        &mut self,
        adapter: SourceAdapter,
        source: &str,
        path: &DiscoveredPath,
    ) -> Result<()>;
    fn rebuild_paths(&mut self, source: &str) -> Result<Option<BTreeSet<String>>>;
    fn remove_unowned_source_rows(&mut self, source: &str) -> Result<()>;
    fn row_count(&mut self, source: &str) -> Result<u64>;
    fn now(&mut self) -> Result<String>;
    fn update_source(&mut self, source: &str, at: &str, status: &str, row_count: u64)
        -> Result<()>;
    fn commit_source(&mut self) -> Result<()>;
    fn source_rows(&mut self) -> Result<Vec<Value>>;
}

/// Run source ingestion policy with explicit time and ledger/discovery effects.
/// `process_file` owns each file transaction; failures remain local to the
/// source and only prevent that source's rebuild cleanup.
pub fn ingest_sources(runtime: &mut impl IngestRuntime) -> Result<Vec<Value>> {
    runtime.attribute_accounts()?;
    runtime.load_owners()?;
    let plans = runtime.source_paths()?;
    for mut plan in plans {
        let source_adapter = SOURCE_ADAPTERS
            .iter()
            .find(|row| row.source == plan.source)
            .ok_or_else(|| Error::Invalid(format!("unknown telemetry source: {}", plan.source)))?;
        let mut errors = 0_u64;
        if source_adapter.uses_registry {
            match runtime.registry_paths(&plan.source) {
                Ok(paths) => plan.paths = paths,
                Err(_) => errors += 1,
            }
        }
        let source_paths: BTreeSet<String> = plan
            .paths
            .iter()
            .map(|item| path_text(&item.path))
            .collect::<Result<_>>()?;
        for path in &plan.paths {
            if runtime
                .process_file(source_adapter.adapter, &plan.source, path)
                .is_err()
            {
                errors += 1;
            }
        }
        if let Some(rebuild_paths) = runtime.rebuild_paths(&plan.source)? {
            if errors == 0 && rebuild_paths.is_subset(&source_paths) {
                runtime.remove_unowned_source_rows(&plan.source)?;
            }
        }
        let count = runtime.row_count(&plan.source)?;
        let status = if errors > 0 {
            "error"
        } else if plan.paths.is_empty() {
            "absent"
        } else {
            "ok"
        };
        let now = runtime.now()?;
        runtime.update_source(&plan.source, &now, status, count)?;
        runtime.commit_source()?;
    }
    runtime.source_rows()
}

pub fn apply_document_effects(
    sink: &mut impl DocumentSink,
    origin_path: &Path,
    effects: DocumentEffects,
) -> Result<()> {
    if let Some(usage) = effects.usage {
        sink.usage(usage, origin_path)?;
    }
    for row in effects.observations {
        sink.observation_row(row)?;
    }
    for event in effects.events {
        sink.event(event)?;
    }
    if let Some(stamp) = effects.harness_stamp {
        sink.harness_stamp(stamp)?;
    }
    Ok(())
}

/// Reduce a Fabric receipt, route-health snapshot or cooldown document. The
/// caller supplies the canonical path identity and the shared receipt error
/// classifier so this adapter does not own host/discovery policy.
pub fn reduce_document(
    source: &str,
    row: &Value,
    origin_path: &Path,
    identity: &str,
    receipt_error_class: Option<String>,
) -> Result<DocumentEffects> {
    let variant = DOCUMENT_VARIANTS
        .iter()
        .find(|variant| variant.source == source)
        .ok_or_else(|| {
            Error::Invalid(format!("unsupported telemetry document source: {source}"))
        })?;
    obj_or_empty(row, "document")?;
    (variant.reducer)(
        row,
        &path_text(origin_path)?,
        identity,
        receipt_error_class.as_deref(),
    )
}

pub fn ingest_document(
    sink: &mut impl DocumentSink,
    source: &str,
    row: &Value,
    origin_path: &Path,
    identity: &str,
    receipt_error_class: Option<String>,
) -> Result<()> {
    let effects = reduce_document(source, row, origin_path, identity, receipt_error_class)?;
    apply_document_effects(sink, origin_path, effects)
}

fn path_text(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::Invalid("telemetry source path is not valid UTF-8".into()))
}

fn obj_or_empty<'a>(value: &'a Value, name: &str) -> Result<&'a Map<String, Value>> {
    value.as_object().ok_or_else(|| {
        Error::Invalid(format!(
            "telemetry document field `{name}` must be an object"
        ))
    })
}

fn truthy_field<'a>(object: &'a Value, name: &str) -> Option<&'a Value> {
    object
        .get(name)
        .filter(|value| pending::python_truthy(value))
}

fn py_text(value: Option<&Value>) -> Option<String> {
    value
        .filter(|value| !value.is_null())
        .map(pending::python_str)
}

fn text_if_truthy(value: Option<&Value>) -> Option<String> {
    value
        .filter(|value| pending::python_truthy(value))
        .map(pending::python_str)
}

fn first_truthy<'a>(values: impl IntoIterator<Item = Option<&'a Value>>) -> Option<&'a Value> {
    values
        .into_iter()
        .flatten()
        .find(|value| pending::python_truthy(value))
}

fn provider_for(adapter: Option<&Value>) -> Option<String> {
    adapter.map(|adapter| {
        let adapter = pending::python_str(adapter);
        PROVIDERS
            .iter()
            .find(|(known, _)| *known == adapter)
            .map(|(_, provider)| (*provider).to_owned())
            .unwrap_or(adapter)
    })
}

fn number(value: Option<&Value>) -> Option<Numeric> {
    value.and_then(crate::number)
}

fn reduced_round(value: Option<Numeric>) -> Result<Option<Numeric>> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        Numeric::Integer(n) => Ok(Some(Numeric::Integer(n))),
        Numeric::Float(n) if n.is_finite() => {
            let rounded = n.round_ties_even();
            if rounded < i64::MIN as f64 || rounded >= 18_446_744_073_709_551_616.0 {
                return Err(Error::NumberOutOfRange);
            }
            if rounded < 0.0 {
                Ok(Some(Numeric::Integer(rounded as i64 as i128)))
            } else if rounded >= i64::MAX as f64 {
                Ok(Some(Numeric::Integer(rounded as u64 as i128)))
            } else {
                Ok(Some(Numeric::Integer(rounded as i64 as i128)))
            }
        }
        Numeric::Float(_) => Ok(None),
    }
}

fn non_null_attrs(values: impl IntoIterator<Item = (String, Value)>) -> Value {
    let mut map = Map::new();
    for (key, value) in values {
        if !value.is_null() {
            map.insert(key, value);
        }
    }
    Value::Object(map)
}

fn reduce_fabric_document(
    row: &Value,
    path: &str,
    identity: &str,
    receipt_error_class: Option<&str>,
) -> Result<DocumentEffects> {
    let null = Value::Null;
    let provenance = truthy_field(row, "provenance").unwrap_or(&null);
    let provenance = if provenance.is_null() {
        None
    } else {
        Some(obj_or_empty(provenance, "provenance")?)
    };
    let requested = provenance
        .and_then(|value| value.get("requested"))
        .filter(|value| pending::python_truthy(value));
    let requested = if let Some(value) = requested {
        Some(obj_or_empty(value, "provenance.requested")?)
    } else {
        None
    };
    let adapter = requested.and_then(|value| value.get("adapter"));
    let usage_value = first_truthy([row.get("usage"), row.get("context")]);
    let usage_value = usage_value.unwrap_or(&null);
    let usage = if usage_value.is_null() {
        None
    } else {
        Some(obj_or_empty(usage_value, "usage/context")?)
    };
    let usage_get = |key: &str| usage.and_then(|value| value.get(key));
    let input = number(usage_get("input_tokens"));
    let cached = number(usage.and_then(|value| {
        value
            .get("cache_read_tokens")
            .or_else(|| value.get("cached_input_tokens"))
    }));
    let written = number(usage.and_then(|value| {
        value
            .get("cache_write_tokens")
            .or_else(|| row.get("cache_write_tokens"))
    }));
    let mut output = number(usage_get("output_tokens"));
    let reasoning = number(usage.and_then(|value| {
        value
            .get("reasoning_tokens")
            .or_else(|| row.get("reasoning_tokens"))
    }));
    let started = row
        .get("started_at")
        .and_then(Value::as_str)
        .and_then(ingest::stamp_ms);
    let ended = row
        .get("ended_at")
        .and_then(Value::as_str)
        .and_then(ingest::stamp_ms);
    let provider_ms = number(
        row.get("timing")
            .and_then(|value| value.get("phases"))
            .and_then(|value| value.get("provider")),
    );
    let requested_route_value = truthy_field(row, "requested_route");
    let requested_route = requested_route_value
        .map(|value| obj_or_empty(value, "requested_route"))
        .transpose()?;
    let parent = truthy_field(row, "parent");
    let parent_obj = parent
        .map(|value| obj_or_empty(value, "parent"))
        .transpose()?;
    if truthy_field(row, "usage").is_none() {
        if let Some(value) = output {
            if adapter.is_some_and(|value| {
                ["codex", "claude", "cursor"]
                    .iter()
                    .any(|name| pending::python_str(value) == *name)
            }) {
                output = Some(
                    value
                        .subtract(reasoning.unwrap_or(Numeric::ZERO))?
                        .non_negative(),
                );
            }
        }
    }
    let mut input_tokens = input;
    if truthy_field(row, "usage").is_none() {
        if let Some(value) = input {
            let subtract = cached
                .unwrap_or(Numeric::ZERO)
                .checked_add(written.unwrap_or(Numeric::ZERO))?;
            input_tokens = Some(value.subtract(subtract)?.non_negative());
        }
    }
    let mut parent_attempt = None;
    if let Some(parent) = parent_obj {
        parent_attempt = number(parent.get("attempt"));
    }
    let status = first_truthy([row.get("status"), row.get("state")]);
    let timestamp = first_truthy([row.get("ended_at"), row.get("started_at")]);
    let run_id = row.get("run_id");
    let task_id = row.get("task_id");
    let attempt = number(row.get("attempt"));
    let lane = ["run_id", "task_id", "attempt"]
        .iter()
        .map(|key| row.get(*key).map(pending::python_str).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("/");
    let task_class = first_truthy([
        row.get("task_class"),
        requested_route.and_then(|value| value.get("task_class")),
    ]);
    let classified_by = first_truthy([
        row.get("classified_by"),
        requested_route.and_then(|value| value.get("classified_by")),
    ]);
    let confidence = number(
        row.get("classifier_confidence")
            .or_else(|| requested_route.and_then(|value| value.get("classifier_confidence"))),
    );
    let shadow = row
        .get("shadow_answers")
        .or_else(|| requested_route.and_then(|value| value.get("shadow_answers")));
    let shadow_answers = shadow
        .filter(|value| !value.is_null())
        .map(pending::python_json);
    let route = text_if_truthy(requested_route.and_then(|value| value.get("alias")));
    let duration = match (started, ended) {
        (Some(start), Some(end)) if end >= start => {
            let rounded = (end - start).round_ties_even();
            if rounded < i64::MIN as f64 || rounded >= 18_446_744_073_709_551_616.0 {
                return Err(Error::NumberOutOfRange);
            }
            Some(if rounded >= i64::MAX as f64 {
                Numeric::Integer(rounded as u64 as i128)
            } else {
                Numeric::Integer(rounded as i64 as i128)
            })
        }
        _ => None,
    };
    let profile = row
        .get("profile")
        .filter(|value| value.is_object())
        .and_then(Value::as_object);
    let profile_digest = profile.and_then(|value| value.get("digest"));
    let profile_knobs = profile.and_then(|value| value.get("knobs"));
    let timestamp_text = py_text(timestamp);
    let usage_row = Usage {
        source: "fabric".into(),
        event_id: identity.to_owned(),
        session: py_text(row.get("session_id")),
        run_id: py_text(run_id),
        delivery_run_id: py_text(row.get("delivery_run_id")),
        parent_run_id: parent_obj.and_then(|value| py_text(value.get("run_id"))),
        parent_task_id: parent_obj.and_then(|value| py_text(value.get("task_id"))),
        parent_attempt,
        task_id: py_text(task_id),
        attempt,
        lane: Some(lane),
        graph_node: py_text(row.get("graph_node")),
        adapter: py_text(adapter),
        provider: text_if_truthy(provenance.and_then(|value| value.get("provider")))
            .or_else(|| provider_for(adapter)),
        model: first_truthy([
            provenance.and_then(|value| value.get("observed_model")),
            provenance.and_then(|value| value.get("resolved_model")),
            requested.and_then(|value| value.get("model")),
        ])
        .and_then(|value| py_text(Some(value))),
        requested_model: py_text(requested.and_then(|value| value.get("model"))),
        resolved_model: py_text(provenance.and_then(|value| value.get("resolved_model"))),
        observed_model: py_text(provenance.and_then(|value| value.get("observed_model"))),
        effort: text_if_truthy(provenance.and_then(|value| value.get("effort_applied")))
            .or_else(|| text_if_truthy(requested.and_then(|value| value.get("effort")))),
        account: text_if_truthy(row.get("account"))
            .or_else(|| text_if_truthy(provenance.and_then(|value| value.get("account")))),
        repository: text_if_truthy(row.get("workspace_root"))
            .or_else(|| text_if_truthy(row.get("cwd"))),
        revision: py_text(row.get("repository_revision")),
        timestamp: timestamp_text.clone(),
        input_tokens,
        cache_read_tokens: cached,
        cache_write_tokens: written,
        output_tokens: output,
        reasoning_tokens: reasoning,
        usage_source: first_truthy([
            usage.and_then(|value| value.get("usage_source")),
            row.get("usage_source"),
            usage.and_then(|value| value.get("source")),
        ])
        .and_then(|value| py_text(Some(value))),
        status: py_text(status),
        error_class: receipt_error_class.map(str::to_owned),
        task_class: py_text(task_class),
        route: route.clone(),
        duration_ms: duration,
        provider_ms: reduced_round(provider_ms)?,
        classified_by: py_text(classified_by),
        classifier_confidence: confidence.map(Numeric::as_f64),
        shadow_answers,
        ..Usage::default()
    };

    let mut effects = DocumentEffects {
        usage: Some(usage_row.clone()),
        ..DocumentEffects::default()
    };
    let ended_truthy = truthy_field(row, "ended_at").is_some();
    if ended_truthy && status.is_some_and(|value| pending::python_str(value) != "running") {
        if text_if_truthy(profile_digest).is_some() && profile_knobs.is_some_and(Value::is_object) {
            effects.harness_stamp = Some(HarnessStamp {
                digest: profile_digest.map(pending::python_str).unwrap_or_default(),
                preset: profile
                    .and_then(|value| value.get("preset"))
                    .and_then(|v| py_text(Some(v))),
                knobs: profile_knobs.cloned().unwrap_or_else(|| json!({})),
                first_seen: timestamp_text.clone(),
            });
        }
        let mut attrs = Map::new();
        for key in [
            "adapter",
            "effort",
            "attempt",
            "status",
            "error_class",
            "task_class",
            "route",
            "duration_ms",
            "classified_by",
            "classifier_confidence",
            "shadow_answers",
        ] {
            let value = match key {
                "error_class" => receipt_error_class.map(|value| Value::String(value.into())),
                "duration_ms" => duration.map(|value| json!(value)),
                "classifier_confidence" => confidence.map(|value| json!(value)),
                "shadow_answers" => shadow.filter(|value| !value.is_null()).cloned(),
                "adapter" => adapter.cloned(),
                "effort" => usage_row.effort.clone().map(Value::String),
                "attempt" => attempt.map(|value| json!(value)),
                "status" => status.cloned(),
                "task_class" => task_class.cloned(),
                "route" => route.clone().map(Value::String),
                "classified_by" => classified_by.cloned(),
                _ => None,
            };
            if let Some(value) = value {
                attrs.insert(key.into(), value);
            }
        }
        let mut fields = vec![
            (
                "profile",
                profile.and_then(|value| value.get("preset")).cloned(),
            ),
            ("profile_digest", profile_digest.cloned()),
            (
                "risk_tier",
                profile.and_then(|value| value.get("risk_tier")).cloned(),
            ),
            (
                "floored",
                profile.and_then(|value| value.get("floored")).cloned(),
            ),
            ("empty_patch", row.get("empty_patch").cloned()),
            ("retry_of", row.get("retry_of").cloned()),
            ("escalation_reason", row.get("escalation_reason").cloned()),
        ];
        for (key, value) in fields.drain(..) {
            if let Some(value) = value {
                attrs.insert(key.into(), value);
            }
        }
        let retry_of = row.get("retry_of").and_then(Value::as_str);
        if truthy_field(row, "escalation_reason").is_some() {
            if let Some(attempt_no) = retry_of
                .and_then(|text| text.strip_prefix("attempt-"))
                .filter(|suffix| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|suffix| suffix.parse::<i64>().ok())
            {
                attrs.insert(
                    "escalated_from".into(),
                    Value::String(format!(
                        "{}/{}/{}",
                        py_text(run_id).unwrap_or_default(),
                        py_text(task_id).unwrap_or_default(),
                        attempt_no
                    )),
                );
            }
        }
        if let Some(parent) = parent.filter(|value| pending::python_truthy(value)) {
            attrs.insert("parent".into(), parent.clone());
        }
        if let Some(children) = truthy_field(row, "children") {
            let children = children.as_array().ok_or_else(|| {
                Error::Invalid("telemetry receipt children must be an array".into())
            })?;
            let values = children
                .iter()
                .map(|child| {
                    let child = obj_or_empty(child, "children[]")?;
                    let mut result = Map::new();
                    for key in ["run_id", "task_id", "attempt", "status"] {
                        if let Some(value) = child.get(key) {
                            result.insert(key.into(), value.clone());
                        }
                    }
                    Ok(Value::Object(result))
                })
                .collect::<Result<Vec<_>>>()?;
            attrs.insert("children".into(), Value::Array(values));
        }
        effects.events.push(Event {
            kind: "lane.attempt".into(),
            ts: timestamp_text,
            source_ref: path.into(),
            session: usage_row.session.clone(),
            run_id: usage_row.run_id.clone(),
            lane: usage_row.lane.clone(),
            task_id: usage_row.task_id.clone(),
            provider: usage_row.provider.clone(),
            account: usage_row.account.clone(),
            model: usage_row.model.clone(),
            harness_rev: py_text(provenance.and_then(|value| value.get("harness_rev"))),
            attrs: Value::Object(attrs),
            refresh: true,
            ..Event::default()
        });
    }
    Ok(effects)
}

fn document_observations(source: &str, row: &Value, path: &str) -> Result<DocumentEffects> {
    let entries = if source == "route_health" {
        row.get("routes").cloned().unwrap_or_else(|| json!({}))
    } else {
        row.get("cooldowns").cloned().unwrap_or_else(|| json!({}))
    };
    let entries = obj_or_empty(
        &entries,
        if source == "route_health" {
            "routes"
        } else {
            "cooldowns"
        },
    )?;
    let mut effects = DocumentEffects::default();
    for entry in entries.values() {
        obj_or_empty(entry, "route/cooldown entry")?;
        let items: Vec<Value> = if source == "route_health" {
            let recent = entry
                .get("recent")
                .cloned()
                .unwrap_or_else(|| Value::Array(Vec::new()));
            recent
                .as_array()
                .ok_or_else(|| Error::Invalid("route-health recent must be an array".into()))?
                .to_vec()
        } else {
            vec![entry.clone()]
        };
        for item in &items {
            let item = obj_or_empty(item, "route-health recent entry")?;
            let adapter = entry.get("adapter");
            let status = item.get("status");
            let values = ObservationRow {
                source: source.into(),
                origin_path: path.into(),
                adapter: py_text(adapter),
                provider: provider_for(adapter),
                model: py_text(entry.get("model")),
                account: py_text(entry.get("account")),
                timestamp: text_if_truthy(item.get("at"))
                    .or_else(|| text_if_truthy(item.get("recorded_at"))),
                run_id: text_if_truthy(item.get("run_id"))
                    .or_else(|| text_if_truthy(item.get("source_run"))),
                status: py_text(status),
                reset_at: py_text(item.get("cooling_until")),
            };
            let kind = if source == "cooldowns" {
                Some("cooldown".to_owned())
            } else if status.is_some_and(|value| {
                matches!(
                    pending::python_str(value).as_str(),
                    "rate_limited" | "usage_limited"
                )
            }) {
                status.map(|value| format!("limit.{}", pending::python_str(value)))
            } else {
                None
            };
            effects.observations.push(values.clone());
            if let Some(kind) = kind {
                effects.events.push(Event {
                    kind,
                    ts: values.timestamp.clone(),
                    source_ref: path.into(),
                    run_id: values.run_id.clone(),
                    provider: values.provider.clone(),
                    account: values.account.clone(),
                    model: values.model.clone(),
                    attrs: non_null_attrs([
                        (
                            "adapter".into(),
                            values
                                .adapter
                                .clone()
                                .map(Value::String)
                                .unwrap_or(Value::Null),
                        ),
                        (
                            "status".into(),
                            values
                                .status
                                .clone()
                                .map(Value::String)
                                .unwrap_or(Value::Null),
                        ),
                        (
                            "reset_at".into(),
                            values
                                .reset_at
                                .clone()
                                .map(Value::String)
                                .unwrap_or(Value::Null),
                        ),
                    ]),
                    ..Event::default()
                });
            }
        }
    }
    Ok(effects)
}

fn reduce_route_document(
    row: &Value,
    path: &str,
    _: &str,
    _: Option<&str>,
) -> Result<DocumentEffects> {
    document_observations("route_health", row, path)
}

fn reduce_cooldown_document(
    row: &Value,
    path: &str,
    _: &str,
    _: Option<&str>,
) -> Result<DocumentEffects> {
    document_observations("cooldowns", row, path)
}

#[derive(Clone, Debug, PartialEq)]
pub struct OpenCodeTurn {
    pub provider: Option<Value>,
    pub model: Option<Value>,
    pub cwd: Option<Value>,
}

/// Provider/model/repository extraction shared by both OpenCode message layouts.
pub fn opencode_turn(row: &Value) -> Result<Option<OpenCodeTurn>> {
    obj_or_empty(row, "OpenCode message")?;
    if row.get("role").is_some_and(|role| role != "assistant") {
        return Ok(None);
    }
    let model = row.get("model").filter(|value| value.is_object());
    let provider = python_or(
        row.get("providerID"),
        model.and_then(|value| value.get("providerID")),
    );
    let model_id = python_or(row.get("modelID"), model.and_then(|value| value.get("id")));
    let cwd = match row.get("path") {
        Some(value) if pending::python_truthy(value) => obj_or_empty(value, "path")?.get("cwd"),
        _ => None,
    };
    Ok(Some(OpenCodeTurn {
        provider: provider.cloned(),
        model: model_id.cloned(),
        cwd: cwd.cloned(),
    }))
}

fn python_or<'a>(left: Option<&'a Value>, right: Option<&'a Value>) -> Option<&'a Value> {
    match left {
        Some(value) if pending::python_truthy(value) => Some(value),
        _ => right,
    }
}

fn sqlite_text(value: Option<&Value>, field: &str) -> Result<Option<String>> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let text = match value {
        Value::String(value) => value.clone(),
        Value::Bool(value) => if *value { "1" } else { "0" }.into(),
        Value::Number(value) => value.to_string(),
        Value::Null | Value::Array(_) | Value::Object(_) => {
            return Err(Error::Invalid(format!(
                "OpenCode {field} cannot be bound to SQLite as text"
            )));
        }
    };
    Ok(Some(text))
}

/// Fixed source variants for OpenCode's pre-2.0.30 and 2.0.30+ layouts.
#[derive(Clone, Copy, Debug)]
pub struct OpenCodeTable {
    pub name: &'static str,
    pub sql: &'static str,
}

pub const OPENCODE_TABLES: &[OpenCodeTable] = &[
    OpenCodeTable {
        name: "message",
        sql: "SELECT id,session_id,time_created,time_updated,CASE WHEN {migrated} THEN NULL ELSE data END,NULL FROM message WHERE time_updated>=?1 ORDER BY time_updated,id",
    },
    OpenCodeTable {
        name: "session_message",
        sql: "SELECT m.id,m.session_id,m.time_created,m.time_updated,CASE WHEN m.type='assistant' THEN m.data END,s.directory FROM session_message m LEFT JOIN session_v2 s ON s.id=m.session_id WHERE m.time_updated>=?1 ORDER BY m.time_updated,m.id",
    },
];

#[derive(Clone, Debug, Default)]
pub struct OpenCodeMessage {
    pub id: String,
    pub session: Option<String>,
    pub created: i64,
    pub updated: i64,
    pub data: Option<Value>,
    pub directory: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct OpenCodeTableRows {
    pub name: String,
    pub latest: i64,
    pub rows: Vec<OpenCodeMessage>,
}

#[derive(Clone, Debug, Default)]
pub struct OpenCodeState {
    pub max_updated: i64,
    pub tables: BTreeMap<String, i64>,
    pub file_id: Option<(u64, u64)>,
}

impl OpenCodeState {
    fn from_value(value: &Value) -> Result<Self> {
        if !value.is_object() {
            return Err(Error::Invalid(
                "OpenCode cursor state must be an object".into(),
            ));
        }
        let max_updated = cursor_mark(value.get("max_updated"), "max_updated")?;
        let mut tables = BTreeMap::new();
        if let Some(tables_value) = value.get("tables") {
            let map = tables_value
                .as_object()
                .ok_or_else(|| Error::Invalid("OpenCode cursor tables must be an object".into()))?;
            for (name, mark) in map {
                tables.insert(
                    name.clone(),
                    cursor_mark(Some(mark), &format!("tables.{name}"))?,
                );
            }
        }
        let file_id = value
            .get("file_id")
            .and_then(Value::as_array)
            .and_then(|parts| match parts.as_slice() {
                [device, inode] => Some((device.as_u64()?, inode.as_u64()?)),
                _ => None,
            });
        Ok(Self {
            max_updated,
            tables,
            file_id,
        })
    }

    fn marks(&self) -> BTreeMap<String, i64> {
        let mut marks = BTreeMap::from([("message".into(), self.max_updated)]);
        marks.extend(self.tables.clone());
        marks
    }

    fn to_value(&self) -> Value {
        json!({
            "max_updated": self.tables.get("message").copied().unwrap_or(0),
            "tables": self.tables,
            "file_id": self.file_id.map(|(device, inode)| json!([device, inode])),
        })
    }
}

fn cursor_mark(value: Option<&Value>, field: &str) -> Result<i64> {
    let Some(value) = value else {
        return Ok(0);
    };
    if let Some(mark) = value.as_i64() {
        return Ok(mark);
    }
    if let Some(mark) = value.as_u64() {
        return i64::try_from(mark).map_err(|_| Error::NumberOutOfRange);
    }
    // Python treats booleans as integers when comparing a cursor mark with a
    // SQLite timestamp, so retain that accepted JSON shape.
    if let Some(mark) = value.as_bool() {
        return Ok(if mark { 1 } else { 0 });
    }
    Err(Error::Invalid(format!(
        "OpenCode cursor {field} must be an integer"
    )))
}

fn read_opencode_row(row: &Row<'_>) -> rusqlite::Result<OpenCodeMessage> {
    let raw: Option<String> = row.get(4)?;
    let data = raw
        .map(|raw| {
            pending::decode_json(raw.as_bytes()).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    4,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        })
        .transpose()?;
    Ok(OpenCodeMessage {
        id: row.get(0)?,
        session: row.get(1)?,
        created: row.get(2)?,
        updated: row.get(3)?,
        data,
        directory: row.get(5)?,
    })
}

/// Read one OpenCode database snapshot. The caller must pass an already
/// verified, read-only connection; the source-path-to-connection step is
/// blocked because R2 does not allow telemetry to depend on supervisor.
pub fn read_opencode_snapshot(
    source: &mut Connection,
    marks: &BTreeMap<String, i64>,
    has_previous: bool,
    previous_file_id: Option<(u64, u64)>,
    file_id: (u64, u64),
) -> Result<(Vec<OpenCodeTableRows>, bool)> {
    let tx = source.transaction().map_err(Error::Sqlite)?;
    let present = {
        let mut statement = tx
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")
            .map_err(Error::Sqlite)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(Error::Sqlite)?;
        rows.collect::<std::result::Result<BTreeSet<_>, _>>()
            .map_err(Error::Sqlite)?
    };
    let tables: Vec<_> = OPENCODE_TABLES
        .iter()
        .filter(|table| present.contains(table.name))
        .collect();
    let mut latest = BTreeMap::new();
    for table in &tables {
        let sql = format!("SELECT MAX(time_updated) FROM {}", table.name);
        let maximum = tx
            .query_row(&sql, [], |row| row.get::<_, Option<i64>>(0))
            .map_err(Error::Sqlite)?
            .unwrap_or(0);
        latest.insert(table.name.to_owned(), maximum);
    }
    let old_marks: BTreeMap<String, i64> = marks.clone();
    let must_reset = has_previous
        && (previous_file_id != Some(file_id)
            || latest
                .iter()
                .any(|(name, maximum)| *maximum < old_marks.get(name).copied().unwrap_or(0)));
    let mut output = Vec::new();
    for table in tables {
        let watermark = if must_reset {
            0
        } else {
            old_marks.get(table.name).copied().unwrap_or(0)
        };
        let migrated = if table.name == "message" && present.contains("session_message") {
            "id IN (SELECT id FROM session_message)"
        } else {
            "0"
        };
        let sql = table.sql.replace("{migrated}", migrated);
        let mut statement = tx.prepare(&sql).map_err(Error::Sqlite)?;
        let rows = statement
            .query_map([watermark], read_opencode_row)
            .map_err(Error::Sqlite)?;
        let mut table_rows = Vec::new();
        for row in rows {
            table_rows.push(row.map_err(Error::Sqlite)?);
        }
        output.push(OpenCodeTableRows {
            name: table.name.into(),
            latest: latest.get(table.name).copied().unwrap_or(0),
            rows: table_rows,
        });
    }
    tx.commit().map_err(Error::Sqlite)?;
    Ok((output, must_reset))
}

/// Visit messages from a single source transaction without retaining the full
/// snapshot. The ledger owner calls `on_reset` before any row callback so file
/// replacement cleanup and new observations share its per-file transaction.
fn visit_opencode_snapshot(
    source: &mut Connection,
    marks: &BTreeMap<String, i64>,
    has_previous: bool,
    previous_file_id: Option<(u64, u64)>,
    file_id: (u64, u64),
    on_reset: impl FnOnce(bool) -> Result<()>,
    mut on_message: impl FnMut(&str, &OpenCodeMessage) -> Result<()>,
) -> Result<(BTreeMap<String, i64>, bool)> {
    let tx = source.transaction().map_err(Error::Sqlite)?;
    let present = {
        let mut statement = tx
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")
            .map_err(Error::Sqlite)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(Error::Sqlite)?;
        rows.collect::<std::result::Result<BTreeSet<_>, _>>()
            .map_err(Error::Sqlite)?
    };
    let tables: Vec<_> = OPENCODE_TABLES
        .iter()
        .filter(|table| present.contains(table.name))
        .collect();
    let mut latest = BTreeMap::new();
    for table in &tables {
        let sql = format!("SELECT MAX(time_updated) FROM {}", table.name);
        let maximum = tx
            .query_row(&sql, [], |row| row.get::<_, Option<i64>>(0))
            .map_err(Error::Sqlite)?
            .unwrap_or(0);
        latest.insert(table.name.to_owned(), maximum);
    }
    let must_reset = has_previous
        && (previous_file_id != Some(file_id)
            || latest
                .iter()
                .any(|(name, maximum)| *maximum < marks.get(name).copied().unwrap_or(0)));
    on_reset(must_reset)?;
    let mut next_marks = if must_reset {
        BTreeMap::new()
    } else {
        marks.clone()
    };
    for table in tables {
        let mut watermark = next_marks.get(table.name).copied().unwrap_or(0);
        let watermark_start = if must_reset { 0 } else { watermark };
        let migrated = if table.name == "message" && present.contains("session_message") {
            "id IN (SELECT id FROM session_message)"
        } else {
            "0"
        };
        let sql = table.sql.replace("{migrated}", migrated);
        {
            let mut statement = tx.prepare(&sql).map_err(Error::Sqlite)?;
            let rows = statement
                .query_map([watermark_start], read_opencode_row)
                .map_err(Error::Sqlite)?;
            for row in rows {
                let message = row.map_err(Error::Sqlite)?;
                watermark = watermark.max(message.updated);
                on_message(table.name, &message)?;
            }
        }
        next_marks.insert(table.name.into(), watermark);
    }
    tx.commit().map_err(Error::Sqlite)?;
    Ok((next_marks, must_reset))
}

/// Reduce one OpenCode JSON message to its metadata-only usage record.
pub fn opencode_usage(
    table: &str,
    message: &OpenCodeMessage,
    row: &Value,
    account: Option<&str>,
) -> Result<Option<Usage>> {
    obj_or_empty(row, "OpenCode message")?;
    let Some(turn) = opencode_turn(row)? else {
        return Ok(None);
    };
    let OpenCodeTurn {
        provider,
        model,
        cwd,
    } = turn;
    let tokens_value = truthy_field(row, "tokens").unwrap_or(&Value::Null);
    let tokens = if tokens_value.is_null() {
        None
    } else {
        Some(obj_or_empty(tokens_value, "tokens")?)
    };
    let cache_value = tokens
        .and_then(|tokens| tokens.get("cache"))
        .filter(|value| pending::python_truthy(value));
    let cache = cache_value
        .map(|value| obj_or_empty(value, "tokens.cache"))
        .transpose()?;
    let time = row
        .get("time")
        .filter(|value| pending::python_truthy(value));
    let created_from_time = time
        .map(|value| obj_or_empty(value, "time"))
        .transpose()?
        .and_then(|value| value.get("created"));
    let stamp = if let Some(value) = created_from_time.filter(|value| pending::python_truthy(value))
    {
        py_f64(Some(value))
            .ok_or_else(|| Error::Invalid("OpenCode message time.created must be numeric".into()))?
    } else {
        message.created as f64
    };
    let seconds = stamp / 1000.0;
    let timestamp = provenant_core::py::from_timestamp(seconds)
        .map_err(|error| Error::Timestamp(error.to_string()))?
        .to_jiff()
        .map_err(|error| Error::Timestamp(error.to_string()))?;
    let repository = match cwd.as_ref().filter(|value| pending::python_truthy(value)) {
        Some(value) => sqlite_text(Some(value), "repository")?,
        None => message.directory.clone(),
    };
    let account_value = account
        .filter(|value| !value.is_empty())
        .map(|value| Value::String(value.into()))
        .or_else(|| {
            row.get("account")
                .filter(|value| pending::python_truthy(value))
                .cloned()
        });
    let usage = Usage {
        source: "opencode".into(),
        event_id: message.id.clone(),
        session: message.session.clone(),
        adapter: Some("opencode".into()),
        provider: sqlite_text(provider.as_ref(), "provider")?,
        model: sqlite_text(model.as_ref(), "model")?,
        timestamp: Some(pending::iso(timestamp)),
        repository,
        account: sqlite_text(account_value.as_ref(), "account")?,
        input_tokens: tokens.and_then(|value| number(value.get("input"))),
        output_tokens: tokens.and_then(|value| number(value.get("output"))),
        reasoning_tokens: tokens.and_then(|value| number(value.get("reasoning"))),
        cache_read_tokens: cache.and_then(|value| number(value.get("read"))),
        cache_write_tokens: cache.and_then(|value| number(value.get("write"))),
        reported_cost: row
            .get("cost")
            .and_then(|value| number(Some(value)))
            .map(Numeric::as_f64),
        usage_source: Some(format!("{table}.tokens")),
        ..Usage::default()
    };
    Ok(Some(usage))
}

fn py_f64(value: Option<&Value>) -> Option<f64> {
    value.and_then(|value| {
        value.as_f64().or_else(|| {
            value
                .as_bool()
                .map(|boolean| if boolean { 1.0 } else { 0.0 })
        })
    })
}

/// Use the OpenCode reducer after the source owner has collected rows from a
/// single read-only transaction. The cursor state remains JSON-compatible
/// with the Python owner.
pub fn reduce_opencode_snapshot(
    previous: Option<&Value>,
    file_id: (u64, u64),
    rows: Vec<OpenCodeTableRows>,
    account: Option<&str>,
) -> Result<(Vec<Usage>, Value, bool)> {
    let saved = match previous {
        Some(value) => OpenCodeState::from_value(value)?,
        None => OpenCodeState::default(),
    };
    let mut marks = saved.marks();
    let replaced = previous.is_some()
        && (saved.file_id != Some(file_id)
            || rows
                .iter()
                .any(|table| table.latest < marks.get(&table.name).copied().unwrap_or(0)));
    if replaced {
        marks.clear();
    }
    let mut usages = Vec::new();
    for table in rows {
        let mut watermark = marks.get(&table.name).copied().unwrap_or(0);
        for message in table.rows {
            watermark = watermark.max(message.updated);
            let Some(row) = message.data.as_ref() else {
                continue;
            };
            if let Some(usage) = opencode_usage(&table.name, &message, row, account)? {
                usages.push(usage);
            }
        }
        marks.insert(table.name, watermark);
    }
    let state = OpenCodeState {
        max_updated: marks.get("message").copied().unwrap_or(0),
        tables: marks,
        file_id: Some(file_id),
    };
    let encoded = state.to_value();
    Ok((usages, encoded, replaced))
}

/// Ingest OpenCode rows from a connection that the caller opened read-only
/// after verifying the source file. The caller wraps this and its ledger sink
/// in the same ledger transaction, matching Python's per-file transaction.
pub fn ingest_opencode(
    source: &mut Connection,
    cursors: &mut impl JsonCursorStore,
    sink: &mut impl Sink,
    path: &Path,
    snapshot: Snapshot,
    account: Option<&str>,
) -> Result<()> {
    let old = cursors.load_json("opencode", path)?;
    let previous_value = old.as_ref().map(|cursor| &cursor.state);
    let previous = match previous_value {
        Some(value) => OpenCodeState::from_value(value)?,
        None => OpenCodeState::default(),
    };
    let file_id = (snapshot.device, snapshot.inode);
    let marks = previous.marks();
    let (marks, _) = visit_opencode_snapshot(
        source,
        &marks,
        old.is_some(),
        previous.file_id,
        file_id,
        |reset| {
            if reset {
                cursors.clear_file("opencode", path)?;
            }
            Ok(())
        },
        |table, message| {
            let Some(row) = message.data.as_ref() else {
                return Ok(());
            };
            if let Some(usage) = opencode_usage(table, message, row, account)? {
                sink.usage(usage, path)?;
            }
            Ok(())
        },
    )?;
    let state = OpenCodeState {
        max_updated: marks.get("message").copied().unwrap_or(0),
        tables: marks,
        file_id: Some(file_id),
    }
    .to_value();
    cursors.save_json(JsonCursor {
        source: "opencode".into(),
        path: path_text(path)?,
        size: snapshot.size,
        mtime: snapshot.mtime_ns,
        offset: 0,
        state,
    })
}

pub fn ingest_opencode_file(
    opener: &impl VerifiedOpenCodeOpener,
    cursors: &mut impl JsonCursorStore,
    sink: &mut impl Sink,
    path: &Path,
    expected: Option<(u64, u64)>,
    account: Option<&str>,
) -> Result<()> {
    let mut opened = opener.open_readonly(path, expected)?;
    ingest_opencode(
        &mut opened.connection,
        cursors,
        sink,
        path,
        opened.snapshot,
        account,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct IngestCapture {
        usages: Vec<Usage>,
    }

    impl Sink for IngestCapture {
        fn usage(&mut self, row: Usage, _: &Path) -> Result<()> {
            self.usages.push(row);
            Ok(())
        }

        fn observation(&mut self, _: crate::Observation) -> Result<()> {
            Ok(())
        }

        fn event(&mut self, _: Event) -> Result<()> {
            Ok(())
        }

        fn update_event(&mut self, _: &str, _: &str, _: Value) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn opencode_turn_handles_both_layouts_and_non_assistant_rows() {
        assert_eq!(
            opencode_turn(&json!({
                "providerID":"vendor",
                "modelID":"legacy",
                "path":{"cwd":"/repo"}
            }))
            .unwrap(),
            Some(OpenCodeTurn {
                provider: Some(json!("vendor")),
                model: Some(json!("legacy")),
                cwd: Some(json!("/repo"))
            })
        );
        assert_eq!(
            opencode_turn(&json!({
                "model":{"providerID":"vendor","id":"v2"},
                "role":"assistant"
            }))
            .unwrap()
            .and_then(|turn| turn.model),
            Some(json!("v2"))
        );
        assert_eq!(opencode_turn(&json!({"role":"user"})).unwrap(), None);
        assert!(matches!(
            opencode_turn(&json!({"path":"not an object"})),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn opencode_repository_fallback_uses_python_truthiness() {
        let message = OpenCodeMessage {
            id: "id".into(),
            created: 1000,
            directory: Some("/from-database".into()),
            ..OpenCodeMessage::default()
        };
        for cwd in [
            json!(false),
            json!(0),
            json!(0.0),
            json!(""),
            json!([]),
            json!({}),
        ] {
            let row = json!({"path":{"cwd":cwd},"tokens":{}});
            let usage = opencode_usage("message", &message, &row, None)
                .unwrap()
                .unwrap();
            assert_eq!(usage.repository.as_deref(), Some("/from-database"));
        }
        let row = json!({"path":{"cwd":true},"tokens":{}});
        let usage = opencode_usage("message", &message, &row, None)
            .unwrap()
            .unwrap();
        assert_eq!(usage.repository.as_deref(), Some("1"));
    }

    #[test]
    fn production_opencode_ingest_visits_rows_and_saves_watermark() {
        let mut source = Connection::open_in_memory().unwrap();
        source
            .execute_batch(
                "CREATE TABLE message(id TEXT,session_id TEXT,time_created INTEGER,time_updated INTEGER,data TEXT);\
                 INSERT INTO message VALUES('id','session',1000,7,'{\"role\":\"assistant\",\"path\":{\"cwd\":false},\"tokens\":{\"input\":3}}');",
            )
            .unwrap();
        let ledger = Connection::open_in_memory().unwrap();
        ledger
            .execute_batch(
                "CREATE TABLE cursors(source TEXT,path TEXT,size INTEGER,mtime INTEGER,offset INTEGER,state TEXT,PRIMARY KEY(source,path));",
            )
            .unwrap();
        let mut cursors = SqliteCursorStore::new(&ledger);
        let mut sink = IngestCapture::default();
        ingest_opencode(
            &mut source,
            &mut cursors,
            &mut sink,
            Path::new("fixture.sqlite"),
            Snapshot {
                device: 1,
                inode: 2,
                size: 12,
                mtime_ns: 34,
            },
            None,
        )
        .unwrap();
        assert_eq!(sink.usages.len(), 1);
        assert_eq!(sink.usages[0].repository.as_deref(), None);
        let state = cursors
            .load_json("opencode", Path::new("fixture.sqlite"))
            .unwrap()
            .unwrap();
        assert_eq!(state.state["tables"]["message"], 7);
    }

    #[test]
    fn opencode_watermarks_advance_for_skipped_rows_and_replacement_resets() {
        let previous = json!({"max_updated":100,"tables":{"message":100},"file_id":[1,2]});
        let rows = vec![OpenCodeTableRows {
            name: "message".into(),
            latest: 110,
            rows: vec![OpenCodeMessage {
                id: "skip".into(),
                updated: 110,
                data: None,
                ..OpenCodeMessage::default()
            }],
        }];
        let (_, state, reset) =
            reduce_opencode_snapshot(Some(&previous), (1, 2), rows, None).unwrap();
        assert!(!reset);
        assert_eq!(state["tables"]["message"], 110);

        let rows = vec![OpenCodeTableRows {
            name: "message".into(),
            latest: 1,
            rows: vec![],
        }];
        let (_, state, reset) =
            reduce_opencode_snapshot(Some(&previous), (1, 2), rows, None).unwrap();
        assert!(reset);
        assert_eq!(state["tables"]["message"], 0);
    }

    #[test]
    fn opencode_cursor_rejects_malformed_shapes_without_defaulting() {
        for value in [
            json!([]),
            json!({"max_updated":"bad"}),
            json!({"tables": []}),
            json!({"tables":{"message":"bad"}}),
        ] {
            assert!(matches!(
                OpenCodeState::from_value(&value),
                Err(Error::Invalid(_))
            ));
        }
        assert!(OpenCodeState::from_value(&Value::Null).is_err());
        assert_eq!(OpenCodeState::default().max_updated, 0);
    }

    #[test]
    fn opencode_turn_shape_errors_match_python() {
        const SOURCE: &str = r#"
import json, sys
sys.path.insert(0, sys.argv[1])
from scripts.telemetry_ingest import opencode_turn
row = json.load(sys.stdin)
try:
    turn = opencode_turn(row)
    print(json.dumps({'ok': True, 'value': turn}))
except (AttributeError, TypeError):
    print(json.dumps({'ok': False, 'error': 'shape'}))
"#;
        for row in [
            json!({"path": "not an object"}),
            json!({"path": []}),
            json!({"role": "user", "path": "not an object"}),
            json!({
                "providerID": "vendor",
                "modelID": "legacy",
                "path": {"cwd": "/repo"}
            }),
        ] {
            let Some(expected) = crate::differential::python(SOURCE, &row) else {
                return;
            };
            let actual = match opencode_turn(&row) {
                Ok(turn) => json!({
                    "ok": true,
                    "value": turn.map(|turn| json!([turn.provider, turn.model, turn.cwd]))
                }),
                Err(Error::Invalid(_)) => json!({"ok": false, "error": "shape"}),
                Err(error) => panic!("unexpected OpenCode turn error: {error}"),
            };
            assert_eq!(actual, expected, "row: {row}");
        }
    }

    #[test]
    fn malformed_opencode_cursor_shapes_match_python_errors() {
        const SOURCE: &str = r#"
import json, sqlite3, sys, tempfile
from pathlib import Path
from contextlib import contextmanager
sys.path.insert(0, sys.argv[1])
from scripts import telemetry_ingest

class Telemetry:
    @contextmanager
    def readonly(self, path):
        connection = sqlite3.connect(f'file:{path}?mode=ro', uri=True)
        try:
            yield connection
        finally:
            connection.close()

cases = json.load(sys.stdin)
results = []
for state in cases:
    with tempfile.TemporaryDirectory() as directory:
        source = Path(directory) / 'source.sqlite'
        connection = sqlite3.connect(source)
        connection.execute('CREATE TABLE message(id TEXT,session_id TEXT,time_created INTEGER,time_updated INTEGER,data TEXT)')
        connection.execute("INSERT INTO message VALUES('id','session',1,1,'{}')")
        connection.commit()
        connection.close()
        ledger = sqlite3.connect(':memory:')
        ledger.row_factory = sqlite3.Row
        ledger.executescript('''
            CREATE TABLE cursors(source TEXT,path TEXT,size INTEGER,mtime INTEGER,offset INTEGER,state TEXT,PRIMARY KEY(source,path));
            CREATE TABLE usage(source TEXT,event_id TEXT,host TEXT,PRIMARY KEY(source,event_id,host));
            CREATE TABLE usage_files(source TEXT,event_id TEXT,host TEXT,path TEXT,PRIMARY KEY(source,event_id,host,path));
            CREATE TABLE observations(source TEXT,origin_path TEXT);
        ''')
        ledger.execute('INSERT INTO cursors VALUES(?,?,?,?,?,?)', ('opencode', str(source), 0, 0, 0, json.dumps(state)))
        try:
            telemetry_ingest.ingest_opencode(Telemetry(), ledger, source)
            error = False
        except (AttributeError, TypeError):
            error = True
        current = ledger.execute('SELECT state FROM cursors WHERE source=? AND path=?', ('opencode', str(source))).fetchone()[0]
        results.append({'error': error, 'unchanged': current == json.dumps(state)})
print(json.dumps(results))
"#;
        let cases = vec![
            json!([]),
            json!({"max_updated":"bad"}),
            json!({"tables": []}),
            json!({"tables":{"message":"bad"}}),
        ];
        let Some(expected) = crate::differential::python(SOURCE, &json!(cases)) else {
            return;
        };
        assert_eq!(expected.as_array().map(Vec::len), Some(4));
        assert!(expected
            .as_array()
            .unwrap()
            .iter()
            .all(|result| { result["error"] == true && result["unchanged"] == true }));
        let actual: Vec<_> = cases
            .iter()
            .map(|value| {
                let error = OpenCodeState::from_value(value).is_err();
                json!({"error": error, "unchanged": true})
            })
            .collect();
        assert_eq!(Value::Array(actual), expected);
    }

    #[test]
    fn route_health_records_limit_event_and_keeps_observation_fields() {
        let effects = reduce_document(
            "route_health",
            &json!({"routes":{"x":{"adapter":"codex","model":"m","recent":[{"status":"rate_limited","run_id":"r","at":"stamp"}]}}}),
            Path::new("fixture.json"),
            "unused",
            None,
        )
        .unwrap();
        assert_eq!(effects.observations.len(), 1);
        assert_eq!(effects.observations[0].run_id.as_deref(), Some("r"));
        assert_eq!(effects.events[0].kind, "limit.rate_limited");
    }

    #[test]
    fn malformed_document_shapes_match_python_for_each_source() {
        const SOURCE: &str = r#"
import json, sys, tempfile
from pathlib import Path
sys.path.insert(0, sys.argv[1])
from scripts import telemetry_ingest

class Result:
    def fetchone(self):
        return None

class Database:
    def __init__(self):
        self.cursor_saved = False
    def execute(self, sql, params=()):
        if sql.startswith('INSERT OR REPLACE INTO cursors'):
            self.cursor_saved = True
        return Result()

cases = json.load(sys.stdin)
results = []
with tempfile.TemporaryDirectory() as directory:
    path = Path(directory) / 'document.json'
    for source, row in cases:
        path.write_text(json.dumps(row))
        database = Database()
        try:
            telemetry_ingest.ingest_document(None, database, source, path)
            error = False
        except (AttributeError, TypeError):
            error = True
        results.append({'error': error, 'cursor_saved': database.cursor_saved})
print(json.dumps(results))
"#;
        let mut cases = Vec::new();
        for source in ["fabric", "route_health", "cooldowns"] {
            for row in [Value::Null, json!([]), json!(1), json!("bad")] {
                cases.push(json!([source, row]));
            }
        }
        let Some(expected) = crate::differential::python(SOURCE, &json!(cases)) else {
            return;
        };
        let actual: Vec<_> = cases
            .iter()
            .map(|case| {
                let source = case[0].as_str().unwrap();
                let result = reduce_document(source, &case[1], Path::new("fixture"), "id", None);
                json!({"error": result.is_err(), "cursor_saved": false})
            })
            .collect();
        assert!(actual.iter().all(|result| result["error"] == true));
        assert!(actual.iter().all(|result| result["cursor_saved"] == false));
        assert_eq!(Value::Array(actual), expected);
    }

    #[test]
    fn sqlite_cursor_clear_keeps_rows_owned_by_another_file() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE cursors(source TEXT,path TEXT,size INTEGER,mtime INTEGER,offset INTEGER,state TEXT,PRIMARY KEY(source,path));\
                 CREATE TABLE usage(source TEXT,event_id TEXT,host TEXT,PRIMARY KEY(source,event_id,host));\
                 CREATE TABLE usage_files(source TEXT,event_id TEXT,host TEXT,path TEXT,PRIMARY KEY(source,event_id,host,path));\
                 CREATE TABLE observations(source TEXT,origin_path TEXT);\
                 INSERT INTO usage VALUES('claude','e','h');\
                 INSERT INTO usage_files VALUES('claude','e','h','one');\
                 INSERT INTO usage_files VALUES('claude','e','h','two');\
                 INSERT INTO observations VALUES('claude','one');",
            )
            .unwrap();
        let mut store = SqliteCursorStore::new(&connection);
        JsonCursorStore::clear_file(&mut store, "claude", Path::new("one")).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM usage", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM observations", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}
