//! Wire contracts for Fabric records. Filesystem transactions, identity resolution,
//! routing policy and orphan recovery stay with their respective future owners.
use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use thiserror::Error;

/// Preserve the distinction between an absent field, explicit null and a value.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Field<T> {
    #[default]
    Missing,
    Null,
    Value(T),
}
impl<T> Field<T> {
    pub fn is_missing(&self) -> bool {
        matches!(self, Self::Missing)
    }
    pub fn as_ref(&self) -> Option<&T> {
        self.value()
    }
    pub fn value(&self) -> Option<&T> {
        if let Self::Value(value) = self {
            Some(value)
        } else {
            None
        }
    }
}
impl<T> From<Option<T>> for Field<T> {
    fn from(value: Option<T>) -> Self {
        value.map_or(Self::Null, Self::Value)
    }
}
impl<T: Serialize> Serialize for Field<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Value(value) => value.serialize(serializer),
            _ => serializer.serialize_none(),
        }
    }
}
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Field<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Option::<T>::deserialize(deserializer)?.map_or(Self::Null, Self::Value))
    }
}

pub const TERMINAL_STATUSES: &[&str] = &[
    "ok",
    "partial",
    "quarantined",
    "failed",
    "usage_limited",
    "rate_limited",
    "auth_required",
    "model_unavailable",
    "permission_blocked",
    "stalled",
    "startup_timeout",
    "writer_changed",
    "timed_out",
    "cancelled",
    "interrupted",
    "lost",
    "rejected",
    "tool_missing",
    "input_required",
];

#[derive(Debug, Error)]
pub enum RecordError {
    #[error("invalid record JSON")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Decode(#[from] crate::DecodeError),
    #[error("record I/O failed at {path}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("lock must not be hard-linked")]
    HardLinkedLock,
    #[error("index must not be hard-linked")]
    HardLinkedIndex,
    #[error("record store is redirected: {0}")]
    RedirectedStore(std::path::PathBuf),
}

/// The projection shared by terminal indexing and recovery. Unowned fields stay
/// opaque so their eventual owners can round-trip retained rows without loss.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AttemptRow {
    pub run_id: String,
    pub task_id: String,
    pub attempt: u64,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub state: Field<String>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub status: Field<String>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub ended_at: Field<String>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub provenance: Field<Provenance>,
    #[serde(flatten)]
    pub retained: BTreeMap<String, Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Provenance {
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub requested: Field<RequestedRoute>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub resolved_model: Field<String>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub observed_model: Field<String>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub effort_applied: Field<String>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub line: Field<String>,
    #[serde(flatten)]
    pub retained: BTreeMap<String, Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RequestedRoute {
    pub adapter: String,
    #[serde(flatten)]
    pub retained: BTreeMap<String, Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct IndexEntry {
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub parent: Field<Value>,
    pub run_id: String,
    pub task_id: String,
    pub attempt: u64,
    pub dir: String,
    pub status: String,
    pub line: String,
    pub result_sha256: Option<String>,
    pub ended_at: String,
    #[serde(flatten)]
    pub retained: BTreeMap<String, Value>,
}
impl IndexEntry {
    /// Durable deduplication includes the attempt, so recovery is idempotent
    /// without hiding a subsequent attempt for the same run and task.
    pub fn key(&self) -> (&str, &str, u64) {
        (&self.run_id, &self.task_id, self.attempt)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Scope {
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub account: Field<String>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub host: Field<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Cooldown {
    pub adapter: String,
    pub model: String,
    #[serde(flatten)]
    pub scope: Scope,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub status: Field<String>,
    pub cooling_until: String,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub source_run: Field<String>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub recorded_at: Field<String>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub signature: Field<String>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub backoff_seconds: Field<serde_json::Number>,
    #[serde(flatten)]
    pub retained: BTreeMap<String, Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Cooldowns {
    pub schema: String,
    pub cooldowns: BTreeMap<String, Cooldown>,
    #[serde(flatten)]
    pub retained: BTreeMap<String, Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RouteOutcome {
    pub status: String,
    pub task_class: String,
    pub at: String,
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub warning: Field<String>,
    #[serde(flatten)]
    pub retained: BTreeMap<String, Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RouteHealth {
    pub adapter: String,
    pub model: String,
    pub task_class: String,
    pub recent: Vec<RouteOutcome>,
    #[serde(flatten)]
    pub retained: BTreeMap<String, Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RouteHealthDocument {
    pub schema: String,
    pub routes: BTreeMap<String, RouteHealth>,
    #[serde(flatten)]
    pub retained: BTreeMap<String, Value>,
}
impl Cooldowns {
    pub fn decode(bytes: &[u8]) -> Result<Self, RecordError> {
        Ok(serde_json::from_value(crate::checked_json::from_slice(
            bytes,
        )?)?)
    }
}
impl RouteHealthDocument {
    pub fn decode(bytes: &[u8]) -> Result<Self, RecordError> {
        Ok(serde_json::from_value(crate::checked_json::from_slice(
            bytes,
        )?)?)
    }
}

/// Python policy APIs accept and return sparse row dictionaries. Full wire
/// projections above describe current writers, while policy retains legacy rows.
pub type RecordRow = BTreeMap<String, Value>;
pub type CooldownRows = BTreeMap<String, RecordRow>;

/// Python datetimes may carry a fixed offset or be naive. `Timestamp` alone
/// would discard that distinction and the original offset of parse_time.
#[derive(Clone, Debug)]
pub enum RecordDateTime {
    Naive(jiff::civil::DateTime),
    Aware(jiff::Zoned),
}

/// Joined read_route_health result, distinct from a persisted route entry.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ActiveRouteHealth {
    pub adapter: String,
    pub model: String,
    pub task_class: String,
    pub recent: Vec<RecordRow>,
    pub recent_failures: u64,
    pub rate_limits: u64,
    pub cooling_until: String,
}

/// Implementation belongs to fabric_records.py#unit-01. JSON maps preserve
/// sparse digest rows and mixed integer/float pacing values without coercion.
pub trait FabricRecordPolicy {
    fn timestamp(&self, value: &RecordDateTime) -> Result<String, RecordError>;
    fn parse_time(&self, value: &str) -> Result<RecordDateTime, RecordError>;
    fn cooldown_path(&self) -> std::path::PathBuf;
    fn route_health_path(&self) -> std::path::PathBuf;
    fn active_cooldowns(
        &self,
        values: &CooldownRows,
        at: Option<&RecordDateTime>,
        scopes: Option<&BTreeMap<String, Scope>>,
    ) -> Result<CooldownRows, RecordError>;
    fn render_digest(&self, row: &RecordRow) -> Result<String, RecordError>;
    fn pacing_policy(&self) -> Result<BTreeMap<String, serde_json::Number>, RecordError>;
}

/// Store contract for later Fabric implementation units. Writes hold the
/// descriptor-bound lock across read/modify/publish and fsync both staged bytes
/// and the containing directory. Readers may fall back without lock acquisition
/// but must not persist migration/pruning in that mode. Redirected stores and
/// multiply-linked locks/indexes are refused. There is no path-based fallback.
/// Python unit-02 calls: None selects ambient paths/time. Store implementation
/// must retain Python's read fallback, row mutation, locking and publication.
pub trait FabricRecordStore {
    fn read_cooldowns(
        &self,
        path: Option<&std::path::Path>,
        at: Option<&RecordDateTime>,
        scopes: Option<&BTreeMap<String, Scope>>,
    ) -> Result<CooldownRows, RecordError>;
    fn read_route_health(
        &self,
        path: Option<&std::path::Path>,
        cooldowns_path: Option<&std::path::Path>,
        at: Option<&RecordDateTime>,
    ) -> Result<BTreeMap<String, ActiveRouteHealth>, RecordError>;
    fn write_cooldown(
        &self,
        row: &mut RecordRow,
        path: Option<&std::path::Path>,
        at: Option<&RecordDateTime>,
    ) -> Result<(), RecordError>;
    fn write_route_health(
        &self,
        row: &RecordRow,
        path: Option<&std::path::Path>,
        at: Option<&RecordDateTime>,
    ) -> Result<(), RecordError>;
    fn clear_cooldowns(
        &self,
        selector: Option<&str>,
        path: Option<&std::path::Path>,
        account: Option<&str>,
    ) -> Result<Vec<String>, RecordError>;
    /// Includes index projection, run-local result hashing and key deduplication.
    fn append_index(
        &self,
        row: &RecordRow,
        run_dir: &std::path::Path,
        root: Option<&std::path::Path>,
    ) -> Result<(), RecordError>;
}

/// Python's compact ensure_ascii JSON string encoding, used in opaque keys.
fn ascii_json(text: &str) -> Result<String, serde_json::Error> {
    let encoded = serde_json::to_string(text)?;
    let mut result = String::new();
    for ch in encoded.chars() {
        if ch >= '\u{007f}' {
            for unit in ch.encode_utf16(&mut [0; 2]) {
                result.push_str(&format!("\\u{unit:04x}"));
            }
        } else {
            result.push(ch);
        }
    }
    Ok(result)
}
pub fn cooldown_key(adapter: &str, model: &str, scope: &Scope) -> Result<String, RecordError> {
    let account = scope
        .account
        .value()
        .map(|s| ascii_json(s))
        .transpose()?
        .unwrap_or_else(|| "null".into());
    let host = scope
        .host
        .value()
        .map(|s| ascii_json(s))
        .transpose()?
        .unwrap_or_else(|| "null".into());
    Ok(format!(
        "[{},{account},{host},{}]",
        ascii_json(adapter)?,
        ascii_json(model)?
    ))
}

#[cfg(test)]
mod tests;
