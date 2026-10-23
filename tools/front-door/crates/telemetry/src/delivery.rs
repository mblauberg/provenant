//! Receipt and sidecar metadata policy. Discovery and file cursors stay with the ledger owner.
use crate::{costs, parse_time, Event, HostIdentity, Result, Sink, Usage};
use rusqlite::{params, types::Value as SqlValue, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

const RECEIPT_FILENAMES: [&str; 3] = ["RUN.json", "repairs.jsonl", "findings.jsonl"];

/// The existing Fabric/project owner supplies its discovered files. Implementations must return
/// only regular, non-symlink files from the requested name; path roots and Git common-dir lookup
/// stay with that owner because telemetry is not allowed to depend on supervisor under R2.
pub trait DeliveryPathSource {
    fn fabric_paths(&mut self, name: &str) -> Result<Vec<PathBuf>>;
}

/// Find delivery documents by asking the project owner for each supported sidecar and returning
/// the sorted unique union, matching `telemetry_delivery.paths`.
pub fn paths(source: &mut impl DeliveryPathSource) -> Result<Vec<PathBuf>> {
    let mut found = BTreeSet::new();
    for name in RECEIPT_FILENAMES {
        found.extend(source.fabric_paths(name)?);
    }
    Ok(found.into_iter().collect())
}

/// Metadata from the opened regular file. The filesystem adapter retains the discovery identity
/// and must refuse links or replacement between metadata and `read_verified`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReceiptFileMetadata {
    pub device: u64,
    pub inode: u64,
    pub size: u64,
    pub mtime_ns: i64,
    /// CPython's `stat.st_mtime` value, in seconds, for its fromtimestamp fallback.
    pub mtime_seconds: f64,
}

/// Filesystem boundary for receipt reads. `read_verified` must read at most `limit` bytes and
/// confirm that the opened file is still the same regular file described by `expected`.
pub trait DeliveryFilesystem {
    fn metadata(&mut self, path: &Path) -> Result<ReceiptFileMetadata>;
    fn read_verified(
        &mut self,
        path: &Path,
        expected: ReceiptFileMetadata,
        limit: usize,
    ) -> Result<Vec<u8>>;
    fn resolve(&mut self, path: &Path) -> Result<PathBuf>;
}

/// The receipt's size/mtime cursor. A store commits this together with the delivery upsert in one
/// transaction, as the source does inside its ledger transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiptCursor {
    pub path: PathBuf,
    pub size: u64,
    pub mtime_ns: i64,
    pub offset: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeliveryReportRow {
    pub run_id: Option<String>,
    pub started_at: Option<String>,
    pub risk_tier: Option<String>,
    pub chair_family: Option<String>,
    pub writer_model: Option<String>,
    pub review_verdicts: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct AcceptedCostInputs {
    pub deliveries: Vec<Delivery>,
    pub linked_lanes: Vec<Usage>,
    pub merged_commits: BTreeSet<String>,
}

/// Small typed storage seam for delivery receipts and their reports. Implementations own SQL,
/// transactions and cursor serialization; policy below decides when to call each operation.
pub trait DeliveryStore {
    fn receipt_cursor(&mut self, path: &Path) -> Result<Option<ReceiptCursor>>;
    fn commit_receipt(&mut self, delivery: Delivery, cursor: ReceiptCursor) -> Result<()>;
    fn cursor_paths(&mut self, source: &str) -> Result<Vec<PathBuf>>;
    fn delete_cursor(&mut self, source: &str, path: &Path) -> Result<()>;
    fn meta_value(&mut self, key: &str) -> Result<Option<String>>;
    fn replace_meta(&mut self, key: &str, value: &str) -> Result<()>;
    fn delivery_report_rows(&mut self) -> Result<Vec<DeliveryReportRow>>;
    /// Return delivery rows ordered by run_dir and linked Fabric usage in rowid order, with
    /// commits already known to the changes ledger. The policy join remains in this crate.
    fn accepted_cost_inputs(&mut self) -> Result<AcceptedCostInputs>;
}

/// SQLite adapter for the ledger operations owned by this module. File discovery and safe reads
/// remain injected through `DeliveryPathSource` and `DeliveryFilesystem`.
pub struct SqliteDeliveryStore<'a> {
    connection: &'a mut Connection,
}

impl<'a> SqliteDeliveryStore<'a> {
    pub fn new(connection: &'a mut Connection) -> Self {
        Self { connection }
    }
}

#[derive(Clone, Debug)]
pub struct ReceiptSqlFields {
    pub run_id: SqlValue,
    pub risk_tier: SqlValue,
    pub chair_family: SqlValue,
    pub merge_commit: SqlValue,
    pub writer_model: SqlValue,
}

fn text_sql(value: Option<String>) -> SqlValue {
    value.map(SqlValue::Text).unwrap_or(SqlValue::Null)
}

fn sqlite_integer(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| crate::Error::NumberOutOfRange)
}

fn sqlite_value(value: Option<&Value>, field: &str) -> Result<SqlValue> {
    let Some(value) = value else {
        return Ok(SqlValue::Null);
    };
    match value {
        Value::Null => Ok(SqlValue::Null),
        Value::Bool(value) => Ok(SqlValue::Integer(i64::from(*value))),
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                Ok(SqlValue::Integer(value))
            } else if let Some(value) = number.as_u64() {
                Ok(SqlValue::Integer(sqlite_integer(value)?))
            } else if let Some(value) = number.as_f64() {
                Ok(SqlValue::Real(value))
            } else {
                Err(crate::Error::NumberOutOfRange)
            }
        }
        Value::String(value) => Ok(SqlValue::Text(value.clone())),
        Value::Array(_) | Value::Object(_) => Err(crate::Error::Invalid(format!(
            "delivery receipt field {field} cannot be bound to SQLite"
        ))),
    }
}

/// SQLite converts scalar values bound to TEXT-affinity receipt columns before reading them back.
fn sqlite_text(value: Option<&Value>, field: &str) -> Result<Option<String>> {
    let bound = sqlite_value(value, field)?;
    match bound {
        SqlValue::Null => Ok(None),
        SqlValue::Text(value) => Ok(Some(value)),
        SqlValue::Integer(value) => Ok(Some(value.to_string())),
        value @ SqlValue::Real(_) => {
            // SQLite's formatting is observable for f64 values such as 1.0 and -0.0.
            let connection = Connection::open_in_memory()?;
            Ok(Some(connection.query_row(
                "SELECT CAST(?1 AS TEXT)",
                [value],
                |row| row.get(0),
            )?))
        }
        SqlValue::Blob(_) => Err(crate::Error::Invalid(format!(
            "delivery receipt field {field} cannot be bound as a SQLite blob"
        ))),
    }
}

fn sqlite_path(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| crate::Error::Invalid("delivery path is not valid UTF-8".into()))
}

impl DeliveryStore for SqliteDeliveryStore<'_> {
    fn receipt_cursor(&mut self, path: &Path) -> Result<Option<ReceiptCursor>> {
        let path_text = sqlite_path(path)?;
        self.connection
            .query_row(
                "SELECT size, mtime, offset FROM cursors WHERE source='delivery' AND path=?1",
                [path_text],
                |row| {
                    let size: i64 = row.get(0)?;
                    let mtime_ns: i64 = row.get(1)?;
                    let offset: i64 = row.get(2)?;
                    Ok((size, mtime_ns, offset))
                },
            )
            .optional()?
            .map(|(size, mtime_ns, offset)| {
                Ok(ReceiptCursor {
                    path: path.to_owned(),
                    size: u64::try_from(size).map_err(|_| crate::Error::NumberOutOfRange)?,
                    mtime_ns,
                    offset: u64::try_from(offset).map_err(|_| crate::Error::NumberOutOfRange)?,
                })
            })
            .transpose()
    }

    fn commit_receipt(&mut self, delivery: Delivery, cursor: ReceiptCursor) -> Result<()> {
        let cursor_path = sqlite_path(&cursor.path)?;
        let size = sqlite_integer(cursor.size)?;
        let offset = sqlite_integer(cursor.offset)?;
        let review_value = serde_json::to_value(&delivery.review_verdicts)?;
        // serde_json::Map's default sorted keys match json.dumps(sort_keys=True).
        let review_verdicts = serde_json::to_string(&review_value)?;
        let repair_cycles = match delivery.raw_repair_cycles {
            Some(value) => value,
            None => match delivery.repair_cycles {
                Some(value) => SqlValue::Integer(sqlite_integer(value)?),
                None => SqlValue::Null,
            },
        };
        let text_fields = delivery
            .raw_receipt_fields
            .unwrap_or_else(|| ReceiptSqlFields {
                run_id: text_sql(delivery.run_id.clone()),
                risk_tier: text_sql(delivery.risk_tier.clone()),
                chair_family: text_sql(delivery.chair_family.clone()),
                merge_commit: text_sql(delivery.merge_commit.clone()),
                writer_model: text_sql(delivery.writer_model.clone()),
            });
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO deliveries
             (run_dir,host,run_id,risk_tier,chair_family,repair_cycles,acceptance_basis,accepted,
              merge_commit,ingested_at,started_at,writer_model,review_verdicts)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
             ON CONFLICT(run_dir) DO UPDATE SET host=excluded.host,run_id=excluded.run_id,
             risk_tier=excluded.risk_tier,chair_family=excluded.chair_family,
             repair_cycles=excluded.repair_cycles,acceptance_basis=excluded.acceptance_basis,
             accepted=excluded.accepted,merge_commit=excluded.merge_commit,
             ingested_at=excluded.ingested_at,started_at=excluded.started_at,
             writer_model=excluded.writer_model,review_verdicts=excluded.review_verdicts",
            params![
                delivery.run_dir,
                delivery.host,
                text_fields.run_id,
                text_fields.risk_tier,
                text_fields.chair_family,
                repair_cycles,
                delivery.acceptance_basis,
                i64::from(delivery.accepted),
                text_fields.merge_commit,
                delivery.ingested_at,
                delivery.started_at,
                text_fields.writer_model,
                review_verdicts,
            ],
        )?;
        transaction.execute(
            "INSERT OR REPLACE INTO cursors(source,path,size,mtime,offset,state)
             VALUES ('delivery',?1,?2,?3,?4,'{}')",
            params![cursor_path, size, cursor.mtime_ns, offset],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn cursor_paths(&mut self, source: &str) -> Result<Vec<PathBuf>> {
        let mut statement = self
            .connection
            .prepare("SELECT path FROM cursors WHERE source=?1")?;
        let paths = statement
            .query_map([source], |row| row.get::<_, String>(0))?
            .map(|path| path.map(PathBuf::from))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(paths)
    }

    fn delete_cursor(&mut self, source: &str, path: &Path) -> Result<()> {
        self.connection.execute(
            "DELETE FROM cursors WHERE source=?1 AND path=?2",
            params![source, sqlite_path(path)?],
        )?;
        Ok(())
    }

    fn meta_value(&mut self, key: &str) -> Result<Option<String>> {
        Ok(self
            .connection
            .query_row("SELECT value FROM meta WHERE key=?1", [key], |row| {
                row.get(0)
            })
            .optional()?)
    }

    fn replace_meta(&mut self, key: &str, value: &str) -> Result<()> {
        self.connection.execute(
            "INSERT OR REPLACE INTO meta(key,value) VALUES (?1,?2)",
            params![key, value],
        )?;
        Ok(())
    }

    fn delivery_report_rows(&mut self) -> Result<Vec<DeliveryReportRow>> {
        let mut statement = self.connection.prepare(
            "SELECT run_id,started_at,risk_tier,chair_family,writer_model,review_verdicts
             FROM deliveries ORDER BY started_at,run_id",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok(DeliveryReportRow {
                    run_id: row.get(0)?,
                    started_at: row.get(1)?,
                    risk_tier: row.get(2)?,
                    chair_family: row.get(3)?,
                    writer_model: row.get(4)?,
                    review_verdicts: row.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn accepted_cost_inputs(&mut self) -> Result<AcceptedCostInputs> {
        let deliveries = {
            let mut statement = self.connection.prepare(
                "SELECT run_dir,host,run_id,risk_tier,chair_family,repair_cycles,acceptance_basis,
                        accepted,merge_commit,ingested_at,started_at,writer_model
                 FROM deliveries ORDER BY run_dir",
            )?;
            let rows = statement
                .query_map([], |row| {
                    let repair_cycles: Option<SqlValue> = row.get(5)?;
                    let repair_count = match &repair_cycles {
                        Some(SqlValue::Integer(value)) if *value >= 0 => u64::try_from(*value).ok(),
                        _ => None,
                    };
                    Ok(Delivery {
                        run_dir: row.get(0)?,
                        host: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                        run_id: row.get(2)?,
                        risk_tier: row.get(3)?,
                        chair_family: row.get(4)?,
                        repair_cycles: repair_count,
                        raw_repair_cycles: repair_cycles,
                        acceptance_basis: row.get(6)?,
                        accepted: row.get::<_, i64>(7)? != 0,
                        merge_commit: row.get(8)?,
                        ingested_at: row.get::<_, Option<String>>(9)?.unwrap_or_default(),
                        started_at: row.get(10)?,
                        writer_model: row.get(11)?,
                        ..Delivery::default()
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        let linked_lanes = {
            let mut statement = self.connection.prepare(
                "SELECT source,event_id,host,adapter,provider,model,input_tokens,
                        cache_read_tokens,cache_write_tokens,output_tokens,reasoning_tokens,
                        delivery_run_id
                 FROM usage WHERE source='fabric' AND delivery_run_id IS NOT NULL ORDER BY rowid",
            )?;
            let rows = statement
                .query_map([], |row| {
                    Ok(Usage {
                        source: row.get(0)?,
                        event_id: row.get(1)?,
                        host: row.get(2)?,
                        adapter: row.get(3)?,
                        provider: row.get(4)?,
                        model: row.get(5)?,
                        input_tokens: numeric_column(row, 6)?,
                        cache_read_tokens: numeric_column(row, 7)?,
                        cache_write_tokens: numeric_column(row, 8)?,
                        output_tokens: numeric_column(row, 9)?,
                        reasoning_tokens: numeric_column(row, 10)?,
                        delivery_run_id: row.get(11)?,
                        ..Usage::default()
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        let merged_commits = {
            let mut statement = self.connection.prepare(
                "SELECT DISTINCT merge_commit FROM changes WHERE merge_commit IS NOT NULL",
            )?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<BTreeSet<_>, _>>()?;
            rows
        };
        Ok(AcceptedCostInputs {
            deliveries,
            linked_lanes,
            merged_commits,
        })
    }
}

fn numeric_column(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<Option<crate::Numeric>> {
    match row.get_ref(index)? {
        rusqlite::types::ValueRef::Null => Ok(None),
        rusqlite::types::ValueRef::Integer(value) => {
            Ok(Some(crate::Numeric::Integer(i128::from(value))))
        }
        rusqlite::types::ValueRef::Real(value) => Ok(Some(crate::Numeric::Float(value))),
        rusqlite::types::ValueRef::Text(_) | rusqlite::types::ValueRef::Blob(_) => {
            Err(rusqlite::Error::InvalidColumnType(
                index,
                "token count".into(),
                row.get_ref(index)?.data_type(),
            ))
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Review {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_family: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lenses: Option<Value>,
    pub verdict: Value,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Delivery {
    pub run_dir: String,
    pub host: String,
    pub run_id: Option<String>,
    pub risk_tier: Option<String>,
    pub chair_family: Option<String>,
    pub repair_cycles: Option<u64>,
    /// The original SQLite bind value retained for INTEGER-affinity coercion.
    #[serde(skip)]
    pub raw_repair_cycles: Option<SqlValue>,
    /// Original SQLite bind values retained for TEXT-affinity coercion in the ledger adapter.
    #[serde(skip)]
    pub raw_receipt_fields: Option<ReceiptSqlFields>,
    pub acceptance_basis: Option<String>,
    pub accepted: bool,
    pub merge_commit: Option<String>,
    pub ingested_at: String,
    pub started_at: Option<String>,
    pub writer_model: Option<String>,
    pub review_verdicts: Vec<Review>,
}
/// Earliest recorded time, excluding authority and risk overrides. No wall-clock access.
pub fn run_timestamp(receipt: &Value, mtime: jiff::Timestamp) -> String {
    fn visit(value: &Value, times: &mut Vec<jiff::Timestamp>) {
        match value {
            Value::Object(values) => {
                for (key, item) in values {
                    if matches!(key.as_str(), "authority" | "risk_override") {
                        continue;
                    }
                    if key == "at" || key == "timestamp" || key.ends_with("_at") {
                        if let Some(t) = item.as_str().and_then(|s| parse_time(s).ok()) {
                            times.push(t);
                        }
                    } else {
                        visit(item, times);
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    visit(item, times);
                }
            }
            _ => {}
        }
    }
    let mut times = Vec::new();
    visit(receipt, &mut times);
    crate::pending::iso(times.into_iter().min().unwrap_or(mtime)).replace('Z', "+00:00")
}
pub fn receipt(
    receipt: &Value,
    run_dir: String,
    host: String,
    now: String,
    mtime: jiff::Timestamp,
) -> Result<Delivery> {
    let object = receipt
        .as_object()
        .ok_or_else(|| crate::Error::Invalid("delivery receipt must be a JSON object".into()))?;
    let basis_value = object
        .get("delivery_acceptance")
        .and_then(Value::as_object)
        .and_then(|acceptance| acceptance.get("basis"));
    if basis_value.is_some_and(|value| matches!(value, Value::Array(_) | Value::Object(_))) {
        return Err(crate::Error::Invalid(
            "delivery acceptance basis is not hashable".into(),
        ));
    }
    let basis = basis_value
        .and_then(Value::as_str)
        .filter(|s| matches!(*s, "checks-and-review" | "project-landing" | "user"));
    let canonical = match object
        .get("software_delivery")
        .filter(|value| crate::pending::python_truthy(value))
    {
        Some(Value::Object(software_delivery)) => software_delivery.get("canonical_artifact_id"),
        Some(_) => {
            return Err(crate::Error::Invalid(
                "delivery receipt software_delivery must be an object".into(),
            ));
        }
        None => None,
    };
    let artifacts: Vec<&Value> = match object.get("artifacts") {
        None => Vec::new(),
        Some(Value::Array(items)) => items.iter().collect(),
        Some(Value::Object(_) | Value::String(_)) => Vec::new(),
        Some(_) => {
            return Err(crate::Error::Invalid(
                "delivery receipt artifacts is not iterable".into(),
            ));
        }
    };
    let artifact = artifacts.into_iter().find(|item| {
        item.is_object()
            && crate::pending::python_equal(
                item.get("id").unwrap_or(&Value::Null),
                canonical.unwrap_or(&Value::Null),
            )
    });
    let review_items: Vec<&Value> = match object
        .get("reviews")
        .filter(|value| crate::pending::python_truthy(value))
    {
        None => Vec::new(),
        Some(Value::Array(items)) => items.iter().collect(),
        Some(Value::Object(_) | Value::String(_)) => Vec::new(),
        Some(_) => {
            return Err(crate::Error::Invalid(
                "delivery receipt reviews is not iterable".into(),
            ));
        }
    };
    let review_verdicts = review_items
        .into_iter()
        .filter_map(|r| {
            if !r.is_object() {
                return None;
            }
            let verdict = r
                .get("verdict")
                .filter(|v| crate::pending::python_truthy(v))
                .or_else(|| r.get("status"))
                .filter(|v| !v.is_null())?;
            Some(Review {
                id: r.get("id").cloned(),
                role: r.get("role").cloned(),
                provider_family: r.get("provider_family").cloned(),
                model: r.get("model").cloned(),
                lenses: r.get("lenses").cloned(),
                verdict: verdict.clone(),
            })
        })
        .collect();
    let repair_cycles = object.get("repair_cycles").and_then(Value::as_u64);
    let raw_repair_cycles = object
        .get("repair_cycles")
        .map(|value| sqlite_value(Some(value), "repair_cycles"))
        .transpose()?;
    let merge_commit = artifact
        .and_then(|item| item.get("git_revision"))
        .and_then(Value::as_object)
        .and_then(|revision| revision.get("commit"));
    let raw_receipt_fields = ReceiptSqlFields {
        run_id: sqlite_value(object.get("run_id"), "run_id")?,
        risk_tier: sqlite_value(object.get("risk_tier"), "risk_tier")?,
        chair_family: sqlite_value(object.get("chair_family"), "chair_family")?,
        merge_commit: sqlite_value(merge_commit, "git_revision.commit")?,
        writer_model: sqlite_value(object.get("writer_model"), "writer_model")?,
    };
    Ok(Delivery {
        run_dir,
        host,
        run_id: sqlite_text(object.get("run_id"), "run_id")?,
        risk_tier: sqlite_text(object.get("risk_tier"), "risk_tier")?,
        chair_family: sqlite_text(object.get("chair_family"), "chair_family")?,
        repair_cycles,
        raw_repair_cycles,
        raw_receipt_fields: Some(raw_receipt_fields),
        acceptance_basis: basis.map(str::to_owned),
        accepted: basis.is_some(),
        merge_commit: sqlite_text(merge_commit, "git_revision.commit")?,
        ingested_at: now,
        started_at: Some(run_timestamp(receipt, mtime)),
        writer_model: sqlite_text(object.get("writer_model"), "writer_model")?,
        review_verdicts,
    })
}

fn path_text(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| crate::Error::Invalid("delivery path is not valid UTF-8".into()))
}

fn owning_directory(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

/// Ingest one RUN.json using its size/mtime cursor. Identity, clock and persistence are explicit;
/// the filesystem implementation must provide the verified snapshot required by the ingest owner.
pub fn ingest_receipt(
    store: &mut impl DeliveryStore,
    filesystem: &mut impl DeliveryFilesystem,
    path: &Path,
    host: String,
    now: String,
) -> Result<bool> {
    let metadata = filesystem.metadata(path)?;
    if store
        .receipt_cursor(path)?
        .is_some_and(|cursor| cursor.size == metadata.size && cursor.mtime_ns == metadata.mtime_ns)
    {
        return Ok(false);
    }
    let bytes = filesystem.read_verified(path, metadata, crate::MAX_JSON_BYTES)?;
    if bytes.len() > crate::MAX_JSON_BYTES {
        return Err(crate::Error::InputTooLarge {
            limit: crate::MAX_JSON_BYTES,
        });
    }
    let document: Value = crate::decode_json(&bytes)?;
    let mtime = crate::pending::timestamp_seconds(metadata.mtime_seconds)?;
    let mut delivery = receipt(&document, String::new(), host, now, mtime)?;
    delivery.run_dir = path_text(&filesystem.resolve(owning_directory(path))?)?;
    let cursor = ReceiptCursor {
        path: path.to_owned(),
        size: metadata.size,
        mtime_ns: metadata.mtime_ns,
        offset: metadata.size,
    };
    store.commit_receipt(delivery, cursor)?;
    Ok(true)
}

/// Re-read sidecars whenever RUN.json changes, including acceptance and review lineage. This
/// preserves the source's literal parent-path comparison for cursors and its `missing` marker for
/// any stat failure.
pub fn prepare(
    store: &mut impl DeliveryStore,
    filesystem: &mut impl DeliveryFilesystem,
    path: &Path,
) -> Result<()> {
    let parent = owning_directory(path);
    let receipt_path = parent.join("RUN.json");
    let signature = filesystem
        .metadata(&receipt_path)
        .ok()
        .map(|metadata| format!("{}:{}", metadata.size, metadata.mtime_ns))
        .unwrap_or_else(|| "missing".into());
    let key = format!("delivery_receipt:{}", path_text(parent)?);
    let previous = store.meta_value(&key)?;
    if previous.as_deref().is_some_and(|value| value != signature) {
        for saved_path in store.cursor_paths("delivery")? {
            if owning_directory(&saved_path) == parent {
                store.delete_cursor("delivery", &saved_path)?;
            }
        }
    }
    store.replace_meta(&key, &signature)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DeliverySummary {
    pub run_id: Option<String>,
    pub started_at: Option<String>,
    pub risk_tier: Option<String>,
    pub chair_family: Option<String>,
    pub writer_model: Option<String>,
    pub review_verdicts: Value,
}

/// Select structured receipt outcomes in SQL-equivalent order and apply the same inclusive lower
/// bound as the Python report. A corrupt stored review JSON value is intentionally rendered as an
/// empty list; valid JSON of any shape passes through as Python's `json.loads` would.
pub fn report_deliveries_from_rows(
    rows: &[DeliveryReportRow],
    since: Option<&str>,
) -> Result<Vec<DeliverySummary>> {
    let boundary = since
        .filter(|value| !value.is_empty())
        .map(parse_time)
        .transpose()?;
    let mut rows = rows.to_vec();
    rows.sort_by(|left, right| {
        (&left.started_at, &left.run_id).cmp(&(&right.started_at, &right.run_id))
    });
    let mut output = Vec::new();
    for row in rows {
        if let Some(boundary) = boundary {
            let Some(started_at) = row.started_at.as_deref() else {
                continue;
            };
            if started_at.is_empty() {
                continue;
            }
            if parse_time(started_at)? < boundary {
                continue;
            }
        }
        let review_verdicts = row
            .review_verdicts
            .as_deref()
            .and_then(|encoded| serde_json::from_str(encoded).ok())
            .unwrap_or_else(|| json!([]));
        output.push(DeliverySummary {
            run_id: row.run_id,
            started_at: row.started_at,
            risk_tier: row.risk_tier,
            chair_family: row.chair_family,
            writer_model: row.writer_model,
            review_verdicts,
        });
    }
    Ok(output)
}

pub fn report_deliveries(
    store: &mut impl DeliveryStore,
    since: Option<&str>,
) -> Result<Vec<DeliverySummary>> {
    report_deliveries_from_rows(&store.delivery_report_rows()?, since)
}

/// Read the store's typed accepted-cost inputs and apply the explicit delivery-ID and host-alias
/// policy. Price/catalogue loading stays with the costs owner and is supplied as data.
pub fn accepted_cost_report(
    store: &mut impl DeliveryStore,
    identity: &HostIdentity,
    since: Option<&str>,
    prices: &costs::Prices,
    adapters: &costs::Catalogue,
) -> Result<Vec<AcceptedCosts>> {
    let input = store.accepted_cost_inputs()?;
    cost_per_accepted(
        &input.deliveries,
        &input.linked_lanes,
        &input.merged_commits,
        identity,
        since,
        prices,
        adapters,
    )
}

/// Render the legacy accepted-delivery report at the command boundary. The returned string
/// includes the newline that Python's `print` adds; callers own stdout and the zero exit status.
pub fn render_accepted(rows: &[AcceptedCosts], as_json: bool) -> Result<String> {
    if as_json {
        return Ok(format!("{}\n", accepted_json(rows)));
    }
    let mut output = String::from(
        "risk_tier  harness_digest  runs  accepted  merged  cost_usd_lower_bound  usage_rows  unpriced_rows  cost_per_accepted_usd_lower_bound  cost_basis\n",
    );
    for row in rows {
        let shown_cost = row
            .cost_usd_lower_bound
            .map(|cost| format!("{cost:.9}"))
            .unwrap_or_else(|| "-".into());
        let per_accepted = row
            .cost_per_accepted_usd_lower_bound
            .map(python_float)
            .unwrap_or_else(|| "None".into());
        output.push_str(&format!(
            "{}  {}  {}  {}  {}  {}  {}  {}  {}  {}\n",
            row.risk_tier
                .as_deref()
                .filter(|value| !value.is_empty())
                .unwrap_or("-"),
            row.harness_digest
                .as_deref()
                .filter(|value| !value.is_empty())
                .unwrap_or("-"),
            row.runs,
            row.accepted,
            row.merged,
            shown_cost,
            row.usage_rows,
            row.unpriced_rows,
            per_accepted,
            row.cost_basis,
        ));
    }
    Ok(output)
}

fn accepted_json(rows: &[AcceptedCosts]) -> String {
    fn string(value: Option<&str>) -> String {
        value
            .map(|value| crate::pending::python_json(&Value::String(value.into())))
            .unwrap_or_else(|| "null".into())
    }
    fn float(value: Option<f64>) -> String {
        match value {
            None => "null".into(),
            Some(value) if value.is_nan() => "NaN".into(),
            Some(value) if value == f64::INFINITY => "Infinity".into(),
            Some(value) if value == f64::NEG_INFINITY => "-Infinity".into(),
            Some(value) => serde_json::Number::from_f64(value)
                .map(|number| crate::pending::python_json(&Value::Number(number)))
                .unwrap_or_else(|| "null".into()),
        }
    }
    let mut output = String::from("[");
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        output.push_str("\n  {\n");
        let fields = [
            format!("    \"risk_tier\": {}", string(row.risk_tier.as_deref())),
            format!(
                "    \"harness_digest\": {}",
                string(row.harness_digest.as_deref())
            ),
            format!("    \"runs\": {}", row.runs),
            format!("    \"accepted\": {}", row.accepted),
            format!("    \"merged\": {}", row.merged),
            format!(
                "    \"cost_usd_lower_bound\": {}",
                float(row.cost_usd_lower_bound)
            ),
            format!("    \"usage_rows\": {}", row.usage_rows),
            format!("    \"unpriced_rows\": {}", row.unpriced_rows),
            format!(
                "    \"cost_per_accepted_usd_lower_bound\": {}",
                float(row.cost_per_accepted_usd_lower_bound)
            ),
            format!("    \"cost_basis\": {}", string(Some(&row.cost_basis))),
        ];
        output.push_str(&fields.join(",\n"));
        output.push_str("\n  }");
    }
    if !rows.is_empty() {
        output.push('\n');
    }
    output.push(']');
    output
}

fn python_float(value: f64) -> String {
    if value.is_nan() {
        return "nan".into();
    }
    if value == f64::INFINITY {
        return "inf".into();
    }
    if value == f64::NEG_INFINITY {
        return "-inf".into();
    }
    serde_json::Number::from_f64(value)
        .map(|number| crate::pending::python_str(&Value::Number(number)))
        .unwrap_or_else(|| value.to_string())
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SidecarState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous: Option<PreviousRepair>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreviousRepair {
    pub source_ref: String,
    pub attrs: Value,
}
fn failures(row: &Value) -> Result<Vec<String>> {
    let object = row.as_object().ok_or_else(|| {
        crate::Error::Invalid("delivery sidecar row must be a JSON object".into())
    })?;
    let items = match object.get("failing_before") {
        None => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        Some(Value::Object(items)) => items.keys().cloned().collect(),
        Some(Value::String(items)) => items.chars().map(|item| item.to_string()).collect(),
        Some(_) => {
            return Err(crate::Error::Invalid(
                "delivery sidecar failing_before is not iterable".into(),
            ));
        }
    };
    Ok(items
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
}

fn review_items(receipt: &Value) -> Result<Vec<&Value>> {
    let object = receipt
        .as_object()
        .ok_or_else(|| crate::Error::Invalid("delivery receipt must be a JSON object".into()))?;
    match object.get("reviews") {
        None => Ok(Vec::new()),
        Some(Value::Array(items)) => Ok(items.iter().collect()),
        Some(Value::Object(_) | Value::String(_)) => Ok(Vec::new()),
        Some(_) => Err(crate::Error::Invalid(
            "delivery receipt reviews is not iterable".into(),
        )),
    }
}
fn failure_hash(failing: &[String]) -> String {
    // json.dumps(..., separators=(',', ':')) still escapes non-ASCII.
    let compact = Value::Array(failing.iter().cloned().map(Value::String).collect());
    let encoded = match &compact {
        Value::Array(a) => format!(
            "[{}]",
            a.iter()
                .map(crate::pending::python_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        _ => String::new(),
    };
    crate::pending::hex(&Sha256::digest(encoded.as_bytes()))[..12].into()
}
fn after(mut attrs: Value, failing: &[String]) -> Value {
    attrs["failing_after_count"] = json!(failing.len());
    attrs["failing_after"] = json!(&failing[..failing.len().min(8)]);
    attrs["failing_after_hash"] = json!(failure_hash(failing));
    attrs
}
/// The caller supplies the owning receipt and resolved source ref, avoiding hidden filesystem reads.
pub fn record(
    sink: &mut impl Sink,
    filename: &str,
    row: &Value,
    state: &mut SidecarState,
    receipt: &Value,
    source_ref: String,
) -> Result<()> {
    if filename == "repairs.jsonl" {
        let row_object = row.as_object().ok_or_else(|| {
            crate::Error::Invalid("delivery sidecar row must be a JSON object".into())
        })?;
        let failing = failures(row)?;
        if let Some(previous) = &state.previous {
            sink.update_event(
                "repair.cycle",
                &previous.source_ref,
                after(previous.attrs.clone(), &failing),
            )?;
        }
        let cycle = row_object.get("cycle").unwrap_or(&Value::Null);
        let attrs = json!({"repair_cycle":cycle,"failing_before_count":failing.len(),"failing_before":&failing[..failing.len().min(8)],"failing_before_hash":failure_hash(&failing)});
        sink.event(Event {
            kind: "repair.cycle".into(),
            ts: sqlite_text(row.get("at"), "at")?,
            source_ref: source_ref.clone(),
            attrs: attrs.clone(),
            ..Event::default()
        })?;
        let receipt_object = receipt.as_object().ok_or_else(|| {
            crate::Error::Invalid("delivery receipt must be a JSON object".into())
        })?;
        if crate::pending::python_equal(
            cycle,
            receipt_object.get("repair_cycles").unwrap_or(&Value::Null),
        ) && receipt_object.contains_key("delivery_acceptance")
        {
            sink.update_event("repair.cycle", &source_ref, after(attrs.clone(), &[]))?;
        }
        state.previous = Some(PreviousRepair { source_ref, attrs });
    } else if filename == "findings.jsonl" {
        let row_object = row.as_object().ok_or_else(|| {
            crate::Error::Invalid("delivery sidecar row must be a JSON object".into())
        })?;
        let row_review_id = row_object.get("review_id").unwrap_or(&Value::Null);
        let review = review_items(receipt)?.into_iter().find(|review| {
            review.is_object()
                && crate::pending::python_equal(
                    review.get("id").unwrap_or(&Value::Null),
                    row_review_id,
                )
        });
        let mut attrs = serde_json::Map::new();
        for key in [
            "review_id",
            "finding",
            "disposition",
            "severity",
            "duplicate_of",
        ] {
            if let Some(v) = row_object.get(key).filter(|v| !v.is_null()) {
                attrs.insert(key.into(), v.clone());
            }
        }
        for (key, source) in [
            ("reviewer_family", "provider_family"),
            ("model", "model"),
            ("reviewer_id", "reviewer_id"),
        ] {
            if let Some(v) = review
                .and_then(|r| r.get(source))
                .filter(|v| crate::pending::python_truthy(v))
                .or_else(|| row_object.get(key))
                .filter(|v| !v.is_null())
            {
                attrs.insert(key.into(), v.clone());
            }
        }
        let attrs = Value::Object(attrs);
        sink.event(Event {
            kind: "review.finding".into(),
            ts: sqlite_text(row.get("at"), "at")?,
            source_ref: source_ref.clone(),
            attrs: attrs.clone(),
            ..Event::default()
        })?;
        sink.update_event("review.finding", &source_ref, attrs)?;
    }
    Ok(())
}
pub fn compact(
    sink: &mut impl Sink,
    provider: &str,
    row: &Value,
    session: Option<&str>,
    path: &Path,
    offset: u64,
) -> Result<()> {
    if !matches!(provider, "claude" | "codex") {
        return Ok(());
    }
    let object = row.as_object().ok_or_else(|| {
        crate::Error::Invalid("provider compaction row must be a JSON object".into())
    })?;
    let row_type = object.get("type").unwrap_or(&Value::Null);
    let (attrs, session) = if provider == "claude"
        && row_type == "system"
        && object.get("subtype").unwrap_or(&Value::Null) == "compact_boundary"
    {
        let meta = object
            .get("compactMetadata")
            .filter(|value| crate::pending::python_truthy(value));
        let meta = match meta {
            None => None,
            Some(Value::Object(meta)) => Some(meta),
            Some(_) => {
                return Err(crate::Error::Invalid(
                    "Claude compactMetadata must be an object".into(),
                ));
            }
        };
        (
            json!({
                "trigger":meta.and_then(|meta| meta.get("trigger")),
                "pre_tokens":meta.and_then(|meta| meta.get("preTokens")),
                "post_tokens":meta.and_then(|meta| meta.get("postTokens"))
            }),
            sqlite_text(object.get("sessionId"), "sessionId")?,
        )
    } else if provider == "codex" && row_type == "compacted" {
        (json!({}), session.map(str::to_owned))
    } else {
        return Ok(());
    };
    sink.event(Event {
        kind: "session.compaction".into(),
        ts: sqlite_text(object.get("timestamp"), "timestamp")?,
        source_ref: format!("{}#{offset}", path.display()),
        session,
        attrs,
        ..Event::default()
    })
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Compactions {
    pub lane: String,
    pub compactions: u64,
}
/// A ledger owner joins compaction events to usage lanes before passing these rows.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct CompactionRow {
    pub lane: String,
    pub event_id: String,
    pub ts: Option<String>,
}
pub fn report(
    rows: &[CompactionRow],
    since: Option<&str>,
    until: Option<&str>,
) -> Result<Vec<Compactions>> {
    let low = since
        .filter(|s| !s.is_empty())
        .map(parse_time)
        .transpose()?;
    let high = until
        .filter(|s| !s.is_empty())
        .map(parse_time)
        .transpose()?;
    let mut counts = BTreeMap::<String, u64>::new();
    // Match SELECT DISTINCT lane,event_id,ts, including repeated joined usage rows.
    for row in rows.iter().collect::<BTreeSet<_>>() {
        let lane = row.lane.clone();
        let stamp = row.ts.as_deref();
        if low.is_some() || high.is_some() {
            let Some(stamp) = stamp else {
                continue;
            };
            let stamp = parse_time(stamp)?;
            if low.is_some_and(|l| stamp < l) || high.is_some_and(|h| stamp >= h) {
                continue;
            }
        }
        *counts.entry(lane).or_default() += 1;
    }
    Ok(counts
        .into_iter()
        .map(|(lane, compactions)| Compactions { lane, compactions })
        .collect())
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AcceptedCosts {
    pub risk_tier: Option<String>,
    pub harness_digest: Option<String>,
    pub runs: usize,
    pub accepted: usize,
    pub merged: usize,
    pub cost_usd_lower_bound: Option<f64>,
    pub usage_rows: u64,
    pub unpriced_rows: u64,
    pub cost_per_accepted_usd_lower_bound: Option<f64>,
    pub cost_basis: String,
}
/// Costs join only explicit delivery IDs. Matching run names do not imply lineage.
pub fn cost_per_accepted(
    deliveries: &[Delivery],
    lanes: &[Usage],
    merged_commits: &BTreeSet<String>,
    identity: &HostIdentity,
    since: Option<&str>,
    prices: &costs::Prices,
    adapters: &costs::Catalogue,
) -> Result<Vec<AcceptedCosts>> {
    #[derive(Default)]
    struct Group {
        runs: BTreeSet<String>,
        accepted: BTreeSet<String>,
        merged: BTreeSet<String>,
        cost: Option<f64>,
        rows: u64,
        unpriced: u64,
    }
    let boundary = since
        .filter(|s| !s.is_empty())
        .map(parse_time)
        .transpose()?;
    let mut groups = BTreeMap::<Option<String>, Group>::new();
    let mut group_order = Vec::<Option<String>>::new();
    let mut run_groups = BTreeMap::new();
    let mut deliveries: Vec<_> = deliveries.iter().collect();
    deliveries.sort_by_key(|d| &d.run_dir);
    for run in deliveries {
        if let Some(boundary) = boundary {
            let Some(started_at) = run.started_at.as_deref().filter(|value| !value.is_empty())
            else {
                continue;
            };
            if parse_time(started_at)? < boundary {
                continue;
            }
        }
        let tier = run.risk_tier.clone();
        if !groups.contains_key(&tier) {
            group_order.push(tier.clone());
        }
        let g = groups.entry(tier.clone()).or_default();
        g.runs.insert(run.run_dir.clone());
        if run.accepted {
            g.accepted.insert(run.run_dir.clone());
        }
        if run
            .merge_commit
            .as_ref()
            .is_some_and(|c| !c.is_empty() && merged_commits.contains(c))
        {
            g.merged.insert(run.run_dir.clone());
        }
        run_groups.insert(run.run_id.clone(), tier);
    }
    let mut dedup = Vec::<&Usage>::new();
    let mut positions = BTreeMap::new();
    for row in lanes
        .iter()
        .filter(|r| r.source == "fabric" && r.delivery_run_id.is_some())
    {
        let key = (&row.source, &row.event_id, identity.canonical(&row.host));
        if let Some(&index) = positions.get(&key) {
            if row.host == identity.current {
                dedup[index] = row;
            }
        } else {
            positions.insert(key, dedup.len());
            dedup.push(row);
        }
    }
    for row in dedup {
        let Some(tier) = run_groups.get(&row.delivery_run_id) else {
            continue;
        };
        let Some(group) = groups.get_mut(tier) else {
            continue;
        };
        let mut row = row.clone();
        row.model = costs::normalise_model(
            row.adapter.as_deref(),
            row.provider.as_deref(),
            row.model.as_deref(),
            adapters,
        );
        let cost = costs::estimated_cost(&row, &prices.entry(&row)?);
        group.rows += 1;
        group.unpriced += u64::from(cost.is_none());
        group.cost = Some(group.cost.unwrap_or(0.0) + cost.unwrap_or(0.0));
    }
    let mut ordered_groups: Vec<_> = group_order
        .into_iter()
        .filter_map(|risk_tier| groups.remove(&risk_tier).map(|group| (risk_tier, group)))
        .collect();
    ordered_groups.sort_by(|(left, _), (right, _)| {
        left.as_deref()
            .unwrap_or("")
            .cmp(right.as_deref().unwrap_or(""))
    });
    Ok(ordered_groups
        .into_iter()
        .map(|(risk_tier, g)| AcceptedCosts {
            risk_tier,
            harness_digest: None,
            runs: g.runs.len(),
            accepted: g.accepted.len(),
            merged: g.merged.len(),
            cost_usd_lower_bound: g.cost,
            usage_rows: g.rows,
            unpriced_rows: g.unpriced,
            cost_per_accepted_usd_lower_bound: g
                .cost
                .filter(|_| !g.accepted.is_empty())
                .map(|c| c / g.accepted.len() as f64),
            cost_basis: if g.cost.is_some() {
                "linked"
            } else {
                "unlinked"
            }
            .into(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receipt_uses_earliest_time_and_canonical_artifact() {
        let input = json!({"created_at":"2026-10-21T00:00:00Z","authority":{"at":"2000-01-01"},"risk_override":{"at":"1999-01-01"},"writer_model":"model","artifacts":[{"id":"other","git_revision":{"commit":"bad"}},{"id":"canonical","git_revision":{"commit":"good"}}],"software_delivery":{"canonical_artifact_id":"canonical"},"delivery_acceptance":{"basis":"checks-and-review"},"reviews":[{"id":"r","status":"pass","secret":"drop"}]});
        let run = receipt(
            &input,
            "run".into(),
            "host".into(),
            "now".into(),
            parse_time("2026-10-23").unwrap(),
        )
        .unwrap();
        assert_eq!(run.started_at.as_deref(), Some("2026-10-21T00:00:00+00:00"));
        assert_eq!(run.merge_commit.as_deref(), Some("good"));
        assert!(run.accepted);
        assert!(!serde_json::to_string(&run.review_verdicts)
            .unwrap()
            .contains("secret"));
    }
    #[test]
    fn receipt_uses_run_file_mtime_when_no_receipt_time_is_valid() {
        let mtime = parse_time("2026-10-23T12:34:56Z").unwrap();
        let run = receipt(
            &json!({"created_at":"not a timestamp"}),
            "run".into(),
            "host".into(),
            "now".into(),
            mtime,
        )
        .unwrap();
        assert_eq!(run.started_at.as_deref(), Some("2026-10-23T12:34:56+00:00"));
    }
    #[test]
    fn receipt_artifact_ids_use_python_numeric_equality() {
        let run = receipt(
            &json!({
                "software_delivery":{"canonical_artifact_id":1.0},
                "artifacts":[{"id":1,"git_revision":{"commit":"canonical"}}]
            }),
            "run".into(),
            "host".into(),
            "now".into(),
            parse_time("2026-10-23").unwrap(),
        )
        .unwrap();
        assert_eq!(run.merge_commit.as_deref(), Some("canonical"));
    }
    #[test]
    fn unlinked_runs_never_appear_free() {
        let run = Delivery {
            run_dir: "run".into(),
            run_id: Some("r".into()),
            accepted: true,
            ..Delivery::default()
        };
        let report = cost_per_accepted(
            &[run],
            &[],
            &BTreeSet::new(),
            &HostIdentity::new("h".into(), BTreeSet::new(), &BTreeSet::new()),
            None,
            &costs::Prices::default(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(report[0].cost_usd_lower_bound, None);
        assert_eq!(report[0].cost_basis, "unlinked");
    }
    #[test]
    fn accepted_since_skips_missing_or_empty_start_times() {
        let deliveries = [
            Delivery {
                run_dir: "empty".into(),
                started_at: Some(String::new()),
                ..Delivery::default()
            },
            Delivery {
                run_dir: "missing".into(),
                started_at: None,
                ..Delivery::default()
            },
        ];
        let report = cost_per_accepted(
            &deliveries,
            &[],
            &BTreeSet::new(),
            &HostIdentity::new("h".into(), BTreeSet::new(), &BTreeSet::new()),
            Some("2026-01-01"),
            &costs::Prices::default(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert!(report.is_empty());
    }
    #[test]
    fn accepted_tier_sort_keeps_python_order_for_null_and_empty_keys() {
        let deliveries = [
            Delivery {
                run_dir: "a-empty".into(),
                risk_tier: Some(String::new()),
                ..Delivery::default()
            },
            Delivery {
                run_dir: "b-null".into(),
                risk_tier: None,
                ..Delivery::default()
            },
        ];
        let report = cost_per_accepted(
            &deliveries,
            &[],
            &BTreeSet::new(),
            &HostIdentity::new("h".into(), BTreeSet::new(), &BTreeSet::new()),
            None,
            &costs::Prices::default(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(report[0].risk_tier.as_deref(), Some(""));
        assert_eq!(report[1].risk_tier, None);
    }
    #[test]
    fn repair_cycle_match_uses_python_numeric_equality() {
        let mut sink = crate::ingest::tests::Capture::default();
        record(
            &mut sink,
            "repairs.jsonl",
            &json!({"cycle":2,"failing_before":[]}),
            &mut SidecarState::default(),
            &json!({"repair_cycles":2.0,"delivery_acceptance":{}}),
            "repair-ref".into(),
        )
        .unwrap();
        assert_eq!(sink.events.len(), 1);
        assert_eq!(sink.updates.len(), 1);
    }
    #[test]
    fn hashes_are_sorted_unique_and_ascii_escaped() {
        assert_eq!(
            failure_hash(&failures(&json!({"failing_before":["b","a","a",4]})).unwrap()),
            failure_hash(&["a".into(), "b".into()])
        );
        assert_eq!(failure_hash(&[]), "4f53cda18c2b");
    }
    #[test]
    fn review_regression_linked_costs_follow_first_seen_order() {
        let run = Delivery {
            run_dir: "dir".into(),
            run_id: Some("run".into()),
            accepted: true,
            ..Delivery::default()
        };
        let rows: Vec<_> = [("c", 1), ("b", 2), ("a", 3)]
            .into_iter()
            .map(|(id, n)| Usage {
                source: "fabric".into(),
                event_id: id.into(),
                host: "h".into(),
                delivery_run_id: Some("run".into()),
                adapter: Some("a".into()),
                model: Some("m".into()),
                input_tokens: Some((n * 100_000).into()),
                cache_read_tokens: Some(0.into()),
                cache_write_tokens: Some(0.into()),
                output_tokens: Some(0.into()),
                ..Usage::default()
            })
            .collect();
        let prices = costs::Prices {
            models: BTreeMap::from([(
                "a/m".into(),
                costs::Price {
                    usd_per_million: BTreeMap::from([("input".into(), json!(1))]),
                    ..costs::Price::default()
                },
            )]),
            ..costs::Prices::default()
        };
        let report = cost_per_accepted(
            &[run],
            &rows,
            &BTreeSet::new(),
            &HostIdentity::new("h".into(), BTreeSet::new(), &BTreeSet::new()),
            None,
            &prices,
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&report[0]).unwrap()["cost_usd_lower_bound"].to_string(),
            "0.6000000000000001"
        );
        assert_eq!(
            report[0].cost_per_accepted_usd_lower_bound,
            Some(0.6000000000000001)
        );
    }

    #[test]
    fn review_regression_missing_and_null_ids_match() {
        let run = receipt(
            &json!({"artifacts":[{"git_revision":{"commit":"abc"}}]}),
            "dir".into(),
            "host".into(),
            "now".into(),
            parse_time("2026-01-01").unwrap(),
        )
        .unwrap();
        assert_eq!(run.merge_commit.as_deref(), Some("abc"));
    }
    #[test]
    fn review_regression_review_missing_and_null_ids_match() {
        for (review, finding) in [
            (
                json!({"provider_family":"family"}),
                json!({"review_id":null}),
            ),
            (json!({"id":null,"provider_family":"family"}), json!({})),
        ] {
            let mut sink = crate::ingest::tests::Capture::default();
            record(
                &mut sink,
                "findings.jsonl",
                &finding,
                &mut SidecarState::default(),
                &json!({"reviews":[review]}),
                "ref".into(),
            )
            .unwrap();
            assert_eq!(sink.events[0].attrs["reviewer_family"], "family");
        }
    }

    #[test]
    fn review_regression_empty_since_is_ignored() {
        let row = CompactionRow {
            lane: "lane".into(),
            event_id: "id".into(),
            ts: Some("2026-01-01".into()),
        };
        let rows = [row.clone(), row];
        assert_eq!(
            report(&rows, Some(""), Some("")).unwrap(),
            report(&rows, None, None).unwrap()
        );
        assert_eq!(report(&rows, None, None).unwrap()[0].compactions, 1);
        let run = Delivery {
            run_dir: "dir".into(),
            ..Delivery::default()
        };
        let identity = HostIdentity::new("h".into(), BTreeSet::new(), &BTreeSet::new());
        assert_eq!(
            cost_per_accepted(
                std::slice::from_ref(&run),
                &[],
                &BTreeSet::new(),
                &identity,
                Some(""),
                &costs::Prices::default(),
                &BTreeMap::new()
            )
            .unwrap(),
            cost_per_accepted(
                &[run],
                &[],
                &BTreeSet::new(),
                &identity,
                None,
                &costs::Prices::default(),
                &BTreeMap::new()
            )
            .unwrap()
        );
    }

    #[derive(Default)]
    struct FakePaths(BTreeMap<String, Vec<PathBuf>>, Vec<String>);
    impl DeliveryPathSource for FakePaths {
        fn fabric_paths(&mut self, name: &str) -> Result<Vec<PathBuf>> {
            self.1.push(name.to_owned());
            Ok(self.0.get(name).cloned().unwrap_or_default())
        }
    }

    #[derive(Default)]
    struct FakeFilesystem {
        metadata: BTreeMap<PathBuf, ReceiptFileMetadata>,
        bytes: BTreeMap<PathBuf, Vec<u8>>,
        resolved: BTreeMap<PathBuf, PathBuf>,
        reads: usize,
    }
    impl DeliveryFilesystem for FakeFilesystem {
        fn metadata(&mut self, path: &Path) -> Result<ReceiptFileMetadata> {
            self.metadata
                .get(path)
                .copied()
                .ok_or_else(|| crate::Error::Io {
                    path: path.to_owned(),
                    source: std::io::Error::new(std::io::ErrorKind::NotFound, "fixture missing"),
                })
        }

        fn read_verified(
            &mut self,
            path: &Path,
            expected: ReceiptFileMetadata,
            limit: usize,
        ) -> Result<Vec<u8>> {
            self.reads += 1;
            if self.metadata(path)? != expected {
                return Err(crate::Error::Invalid(
                    "fixture changed after discovery".into(),
                ));
            }
            let bytes = self
                .bytes
                .get(path)
                .cloned()
                .ok_or_else(|| crate::Error::Io {
                    path: path.to_owned(),
                    source: std::io::Error::new(std::io::ErrorKind::NotFound, "fixture missing"),
                })?;
            if bytes.len() > limit {
                return Err(crate::Error::InputTooLarge { limit });
            }
            Ok(bytes)
        }

        fn resolve(&mut self, path: &Path) -> Result<PathBuf> {
            Ok(self
                .resolved
                .get(path)
                .cloned()
                .unwrap_or_else(|| path.to_owned()))
        }
    }

    #[derive(Default)]
    struct FakeStore {
        cursors: BTreeMap<(String, PathBuf), ReceiptCursor>,
        deliveries: BTreeMap<String, Delivery>,
        metadata: BTreeMap<String, String>,
        deleted: Vec<(String, PathBuf)>,
        report_rows: Vec<DeliveryReportRow>,
        accepted: AcceptedCostInputs,
    }
    impl DeliveryStore for FakeStore {
        fn receipt_cursor(&mut self, path: &Path) -> Result<Option<ReceiptCursor>> {
            Ok(self
                .cursors
                .get(&("delivery".into(), path.to_owned()))
                .cloned())
        }

        fn commit_receipt(&mut self, delivery: Delivery, cursor: ReceiptCursor) -> Result<()> {
            self.deliveries.insert(delivery.run_dir.clone(), delivery);
            self.cursors
                .insert(("delivery".into(), cursor.path.clone()), cursor);
            Ok(())
        }

        fn cursor_paths(&mut self, source: &str) -> Result<Vec<PathBuf>> {
            Ok(self
                .cursors
                .keys()
                .filter(|(saved_source, _)| saved_source == source)
                .map(|(_, path)| path.clone())
                .collect())
        }

        fn delete_cursor(&mut self, source: &str, path: &Path) -> Result<()> {
            self.deleted.push((source.into(), path.to_owned()));
            self.cursors.remove(&(source.into(), path.to_owned()));
            Ok(())
        }

        fn meta_value(&mut self, key: &str) -> Result<Option<String>> {
            Ok(self.metadata.get(key).cloned())
        }

        fn replace_meta(&mut self, key: &str, value: &str) -> Result<()> {
            self.metadata.insert(key.into(), value.into());
            Ok(())
        }

        fn delivery_report_rows(&mut self) -> Result<Vec<DeliveryReportRow>> {
            Ok(self.report_rows.clone())
        }

        fn accepted_cost_inputs(&mut self) -> Result<AcceptedCostInputs> {
            Ok(self.accepted.clone())
        }
    }

    fn metadata_for(bytes: &[u8]) -> ReceiptFileMetadata {
        ReceiptFileMetadata {
            device: 1,
            inode: 2,
            size: bytes.len() as u64,
            mtime_ns: 30,
            mtime_seconds: 1_766_000_000.25,
        }
    }

    fn sqlite_ledger() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE deliveries (
                    run_dir TEXT PRIMARY KEY, host TEXT, run_id TEXT, risk_tier TEXT,
                    chair_family TEXT, repair_cycles INTEGER, acceptance_basis TEXT,
                    accepted INTEGER NOT NULL DEFAULT 0, merge_commit TEXT, ingested_at TEXT,
                    writer_model TEXT, review_verdicts TEXT NOT NULL DEFAULT '[]', started_at TEXT
                 );
                 CREATE TABLE cursors (
                    source TEXT NOT NULL, path TEXT NOT NULL, size INTEGER NOT NULL,
                    mtime INTEGER NOT NULL, offset INTEGER NOT NULL, state TEXT NOT NULL,
                    PRIMARY KEY(source,path)
                 );
                 CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE changes (merge_commit TEXT);
                 CREATE TABLE usage (
                    source TEXT NOT NULL, event_id TEXT NOT NULL, host TEXT NOT NULL,
                    adapter TEXT, provider TEXT, model TEXT, input_tokens INTEGER,
                    cache_read_tokens INTEGER, cache_write_tokens INTEGER, output_tokens INTEGER,
                    reasoning_tokens INTEGER, delivery_run_id TEXT
                 );",
            )
            .unwrap();
        connection
    }

    #[test]
    fn discovery_returns_sorted_unique_supported_delivery_paths() {
        let a = PathBuf::from("/project/a/RUN.json");
        let b = PathBuf::from("/project/b/RUN.json");
        let mut source = FakePaths(
            BTreeMap::from([
                ("RUN.json".into(), vec![b.clone(), a.clone()]),
                ("repairs.jsonl".into(), vec![a.clone()]),
                (
                    "findings.jsonl".into(),
                    vec![PathBuf::from("/project/c/findings.jsonl")],
                ),
            ]),
            Vec::new(),
        );
        assert_eq!(
            paths(&mut source).unwrap(),
            vec![
                PathBuf::from("/project/a/RUN.json"),
                PathBuf::from("/project/b/RUN.json"),
                PathBuf::from("/project/c/findings.jsonl"),
            ]
        );
        assert_eq!(
            source.1.iter().map(String::as_str).collect::<Vec<_>>(),
            vec!["RUN.json", "repairs.jsonl", "findings.jsonl"]
        );
    }

    #[test]
    fn receipt_cursor_skips_unchanged_files_and_commits_changed_receipts() {
        let path = PathBuf::from("/project/.agent-run/runs/run/RUN.json");
        let bytes = br#"{"run_id":"r","risk_tier":"substantial","created_at":"2026-01-02T00:00:00Z","authority":{"at":"2000-01-01"},"software_delivery":{"canonical_artifact_id":"source"},"artifacts":[{"id":"source","git_revision":{"commit":"abc"}}],"delivery_acceptance":{"basis":"user"},"reviews":[{"id":"review","status":"pass","model":"m","secret":"drop"}]}"#.to_vec();
        let metadata = metadata_for(&bytes);
        let mut filesystem = FakeFilesystem::default();
        filesystem.metadata.insert(path.clone(), metadata);
        filesystem.bytes.insert(path.clone(), bytes);
        filesystem.resolved.insert(
            PathBuf::from("/project/.agent-run/runs/run"),
            PathBuf::from("/project/.agent-run/runs/run"),
        );
        let mut store = FakeStore::default();
        assert!(ingest_receipt(
            &mut store,
            &mut filesystem,
            &path,
            "host".into(),
            "now".into()
        )
        .unwrap());
        assert_eq!(filesystem.reads, 1);
        let delivery = store
            .deliveries
            .get("/project/.agent-run/runs/run")
            .unwrap();
        assert_eq!(delivery.run_id.as_deref(), Some("r"));
        assert_eq!(
            delivery.started_at.as_deref(),
            Some("2026-01-02T00:00:00+00:00")
        );
        assert_eq!(delivery.merge_commit.as_deref(), Some("abc"));
        assert!(delivery.accepted);
        assert_eq!(delivery.review_verdicts[0].verdict, "pass");
        assert_eq!(
            store.cursors[&("delivery".into(), path.clone())].offset,
            metadata.size
        );
        assert!(!ingest_receipt(
            &mut store,
            &mut filesystem,
            &path,
            "host".into(),
            "later".into()
        )
        .unwrap());
        assert_eq!(filesystem.reads, 1);
        assert_eq!(store.deliveries.values().next().unwrap().ingested_at, "now");
    }

    #[test]
    fn invalid_receipt_does_not_advance_cursor_or_upsert_delivery() {
        let path = PathBuf::from("/project/run/RUN.json");
        let bytes = b"{not-json".to_vec();
        let mut filesystem = FakeFilesystem::default();
        filesystem
            .metadata
            .insert(path.clone(), metadata_for(&bytes));
        filesystem.bytes.insert(path.clone(), bytes);
        let mut store = FakeStore::default();

        assert!(ingest_receipt(
            &mut store,
            &mut filesystem,
            &path,
            "host".into(),
            "now".into()
        )
        .is_err());
        assert!(store.cursors.is_empty());
        assert!(store.deliveries.is_empty());
    }

    #[test]
    fn malformed_receipt_shapes_fail_before_store_effects_and_match_python() {
        for (index, document) in [
            json!([]),
            json!({"artifacts":null}),
            json!({"delivery_acceptance":{"basis":[]}}),
        ]
        .into_iter()
        .enumerate()
        {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("RUN.json");
            std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
            let path = std::fs::canonicalize(path).unwrap();
            let input = json!({"path":path});
            let source = format!(
                "{}\nimport json\nfrom pathlib import Path\nd=load('telemetry_delivery')\nclass Cursor:\n def fetchone(self): return None\nclass DB:\n def __init__(self): self.writes=0\n def execute(self,q,args):\n  if q.startswith('INSERT'): self.writes+=1\n  return Cursor()\ndb=DB()\nt.host_id=lambda:'host'\nt.now=lambda:'now'\ntry:\n d.ingest_receipt(t,db,Path(input['path']))\n failed=False\nexcept (AttributeError,TypeError):\n failed=True\nprint(json.dumps({{'failed':failed,'writes':db.writes}}))",
                crate::differential::LOAD
            );
            if let Some(expected) = crate::differential::python(&source, &input) {
                crate::differential::equal(&expected, &json!({"failed":true,"writes":0}));
            } else {
                return;
            }

            let bytes = std::fs::read(&path).unwrap();
            let metadata = metadata_for(&bytes);
            let mut filesystem = FakeFilesystem::default();
            filesystem.metadata.insert(path.clone(), metadata);
            filesystem.bytes.insert(path.clone(), bytes);
            filesystem.resolved.insert(
                path.parent().unwrap().to_owned(),
                path.parent().unwrap().to_owned(),
            );
            let mut store = FakeStore::default();
            let error = ingest_receipt(
                &mut store,
                &mut filesystem,
                &path,
                "host".into(),
                "now".into(),
            )
            .unwrap_err();
            assert!(
                matches!(error, crate::Error::Invalid(_)),
                "case {index}: {error}"
            );
            assert!(store.cursors.is_empty());
            assert!(store.deliveries.is_empty());
        }
    }

    #[test]
    fn malformed_sidecar_shapes_fail_before_writing_and_match_python() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("repairs.jsonl");
        let rows = vec![
            json!([]),
            json!({"failing_before":null}),
            json!({"failing_before":1}),
        ];
        let input = json!({"path":path,"rows":rows});
        let source = format!(
            "{}\nimport json\nfrom pathlib import Path\nd=load('telemetry_delivery')\nclass T:\n def __init__(self): self.writes=0\n def put_event(self,*args,**kwargs): self.writes+=1\nclass DB:\n def execute(self,*args): raise AssertionError('unexpected write')\nt=T()\nresults=[]\nfor row in input['rows']:\n try:\n  d.record(t,DB(),row,{{}},Path(input['path']),1)\n  results.append({{'failed':False,'writes':t.writes}})\n except (AttributeError,TypeError):\n  results.append({{'failed':True,'writes':t.writes}})\nprint(json.dumps(results))",
            crate::differential::LOAD
        );
        let Some(expected) = crate::differential::python(&source, &input) else {
            return;
        };

        let mut actual = Vec::new();
        for row in rows {
            let mut sink = crate::ingest::tests::Capture::default();
            let result = record(
                &mut sink,
                "repairs.jsonl",
                &row,
                &mut SidecarState::default(),
                &json!({}),
                "repair-ref".into(),
            );
            assert!(matches!(result, Err(crate::Error::Invalid(_))));
            assert!(sink.events.is_empty());
            assert!(sink.updates.is_empty());
            actual.push(json!({"failed":true,"writes":0}));
        }
        crate::differential::equal(&json!(actual), &expected);
    }

    #[test]
    fn sidecar_timestamp_uses_sqlite_scalar_text_coercion() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("repairs.jsonl");
        let row = json!({"at":1.0,"failing_before":[]});
        let input = json!({"path":path,"row":row});
        let source = format!(
            "{}\nimport json,sqlite3\nfrom pathlib import Path\nd=load('telemetry_delivery')\ndb=sqlite3.connect(':memory:')\ndb.execute('CREATE TABLE events(ts TEXT)')\nclass T:\n def put_event(self,db,kind,ts,source_ref,**kwargs): db.execute('INSERT INTO events VALUES (?)',(ts,))\nt=T()\nd.record(t,db,input['row'],{{}},Path(input['path']),1)\nprint(json.dumps(db.execute('SELECT ts FROM events').fetchone()[0]))",
            crate::differential::LOAD
        );
        let Some(expected) = crate::differential::python(&source, &input) else {
            return;
        };
        let mut sink = crate::ingest::tests::Capture::default();
        record(
            &mut sink,
            "repairs.jsonl",
            &input["row"],
            &mut SidecarState::default(),
            &json!({}),
            "repair-ref".into(),
        )
        .unwrap();
        crate::differential::equal(&json!(sink.events[0].ts), &expected);
        assert_eq!(sink.events[0].ts.as_deref(), Some("1.0"));
    }

    #[test]
    fn sqlite_receipt_keeps_python_scalar_binding_and_text_affinity() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("RUN.json");
        let receipt_value = json!({
            "run_id":7,
            "risk_tier":1,
            "chair_family":false,
            "repair_cycles":2.5,
            "writer_model":1.0,
            "software_delivery":{"canonical_artifact_id":true},
            "artifacts":[{"id":1,"git_revision":{"commit":true}}],
            "delivery_acceptance":{"basis":"user"}
        });
        let bytes = serde_json::to_vec(&receipt_value).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let path = std::fs::canonicalize(path).unwrap();
        let input = json!({"path":path});
        let source = format!(
            "{}\nimport json,sqlite3\nfrom pathlib import Path\nd=load('telemetry_delivery')\nclass T:\n def host_id(self): return 'host'\n def now(self): return 'now'\nt=T()\ndb=sqlite3.connect(':memory:')\ndb.row_factory=sqlite3.Row\ndb.executescript('CREATE TABLE deliveries(run_dir TEXT PRIMARY KEY,host TEXT,run_id TEXT,risk_tier TEXT,chair_family TEXT,repair_cycles INTEGER,acceptance_basis TEXT,accepted INTEGER,merge_commit TEXT,ingested_at TEXT,started_at TEXT,writer_model TEXT,review_verdicts TEXT); CREATE TABLE cursors(source TEXT,path TEXT,size INTEGER,mtime INTEGER,offset INTEGER,state TEXT,PRIMARY KEY(source,path));')\nd.ingest_receipt(t,db,Path(input['path']))\nr=db.execute('SELECT run_id,risk_tier,chair_family,repair_cycles,acceptance_basis,accepted,merge_commit,writer_model FROM deliveries').fetchone()\nprint(json.dumps(list(r)))",
            crate::differential::LOAD
        );
        let Some(expected) = crate::differential::python(&source, &input) else {
            return;
        };

        let metadata = std::fs::metadata(&path).unwrap();
        let modified = metadata
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        let mut filesystem = FakeFilesystem::default();
        filesystem.metadata.insert(
            path.clone(),
            ReceiptFileMetadata {
                device: 1,
                inode: 1,
                size: metadata.len(),
                mtime_ns: i64::try_from(modified.as_nanos()).unwrap(),
                mtime_seconds: modified.as_secs_f64(),
            },
        );
        filesystem.bytes.insert(path.clone(), bytes);
        let parent = path.parent().unwrap().to_owned();
        filesystem.resolved.insert(parent.clone(), parent);
        let mut connection = sqlite_ledger();
        {
            let mut store = SqliteDeliveryStore::new(&mut connection);
            ingest_receipt(
                &mut store,
                &mut filesystem,
                &path,
                "host".into(),
                "now".into(),
            )
            .unwrap();
        }
        let actual: Vec<Value> = connection
            .query_row(
                "SELECT run_id,risk_tier,chair_family,repair_cycles,acceptance_basis,accepted,merge_commit,writer_model FROM deliveries",
                [],
                |row| {
                    Ok(vec![
                        row.get::<_, Option<String>>(0)?.map_or(Value::Null, Value::String),
                        row.get::<_, Option<String>>(1)?.map_or(Value::Null, Value::String),
                        row.get::<_, Option<String>>(2)?.map_or(Value::Null, Value::String),
                        row.get::<_, Option<f64>>(3)?.map_or(Value::Null, |value| json!(value)),
                        row.get::<_, Option<String>>(4)?.map_or(Value::Null, Value::String),
                        json!(row.get::<_, i64>(5)?),
                        row.get::<_, Option<String>>(6)?.map_or(Value::Null, Value::String),
                        row.get::<_, Option<String>>(7)?.map_or(Value::Null, Value::String),
                    ])
                },
            )
            .unwrap();
        crate::differential::equal(&json!(actual), &expected);
        assert_eq!(
            json!(actual),
            json!(["7", "1", "0", 2.5, "user", 1, "1", "1.0"])
        );
    }

    #[test]
    fn sqlite_accepted_costs_join_sibling_delivery_lanes_and_deduplicate_host_aliases() {
        let mut connection = sqlite_ledger();
        connection.execute(
            "INSERT INTO deliveries(run_dir,host,run_id,risk_tier,accepted,merge_commit,started_at) VALUES ('/runs/a','new','run-a','substantial',1,'merge-a','2026-10-20T00:00:00+00:00')",
            [],
        ).unwrap();
        connection
            .execute("INSERT INTO changes(merge_commit) VALUES ('merge-a')", [])
            .unwrap();
        let insert_usage = |connection: &Connection,
                            event_id: &str,
                            host: &str,
                            run_id: &str,
                            tokens: i64| {
            connection.execute(
                "INSERT INTO usage(source,event_id,host,adapter,provider,model,input_tokens,cache_read_tokens,cache_write_tokens,output_tokens,reasoning_tokens,delivery_run_id) VALUES ('fabric',?1,?2,'a','p','m',?3,0,0,0,NULL,?4)",
                params![event_id, host, tokens, run_id],
            ).unwrap();
        };
        insert_usage(&connection, "writer", "old", "run-a", 100_000);
        insert_usage(&connection, "writer", "new", "run-a", 500_000);
        insert_usage(&connection, "reviewer", "new", "run-a", 500_000);
        insert_usage(&connection, "orphan", "new", "other-run", 900_000);

        let prices = costs::Prices {
            models: BTreeMap::from([(
                "a/m".into(),
                costs::Price {
                    usd_per_million: BTreeMap::from([
                        ("input".into(), json!(12.5)),
                        ("cached_input".into(), json!(0)),
                        ("cache_write".into(), json!(0)),
                        ("output".into(), json!(0)),
                    ]),
                    ..costs::Price::default()
                },
            )]),
            ..costs::Prices::default()
        };
        let identity = HostIdentity::new(
            "new".into(),
            BTreeSet::from(["old".into()]),
            &BTreeSet::new(),
        );
        let mut store = SqliteDeliveryStore::new(&mut connection);
        let actual =
            accepted_cost_report(&mut store, &identity, None, &prices, &BTreeMap::new()).unwrap();

        let input = json!({});
        let source = format!(
            "{}\nimport json,sqlite3\nd=load('telemetry_delivery')\nclass Costs:\n def load_prices(self,t): return {{}},{{}}\n def catalogue(self,t): return {{}}\n def normalise_model(self,adapter,provider,model,adapters): return model\n def price_entry(self,row,prices,free_patterns): return {{'usd_per_million':{{'input':12.5,'cached_input':0,'cache_write':0,'output':0}}}}\n def estimated_cost(self,t,row,prices,entry):\n  value=0.0\n  for name,rate in [('input_tokens','input'),('cache_read_tokens','cached_input'),('cache_write_tokens','cache_write'),('output_tokens','output')]:\n   count=row[name]\n   price=entry['usd_per_million'][rate]\n   if count is None or count>0 and price is None: return None\n   value+=count*(price or 0)/1000000\n  return value\nt.host_id=lambda:'new'\nt.load_hosts=lambda db:None\nt.host_aliases=lambda:{{'new','old'}}\nt.same_host=lambda host,aliases=None:'new' if host in ('new','old') else host\nt.costs_owner=lambda:Costs()\ndb=sqlite3.connect(':memory:')\ndb.row_factory=sqlite3.Row\ndb.executescript('CREATE TABLE deliveries(run_dir,host,run_id,risk_tier,accepted,merge_commit,started_at); CREATE TABLE changes(merge_commit); CREATE TABLE usage(source,event_id,host,adapter,provider,model,input_tokens,cache_read_tokens,cache_write_tokens,output_tokens,reasoning_tokens,delivery_run_id);')\ndb.execute(\"INSERT INTO deliveries VALUES ('/runs/a','new','run-a','substantial',1,'merge-a','2026-10-20T00:00:00+00:00')\")\ndb.execute(\"INSERT INTO changes VALUES ('merge-a')\")\ndb.executemany('INSERT INTO usage VALUES (?,?,?,?,?,?,?,?,?,?,?,?)',[('fabric','writer','old','a','p','m',100000,0,0,0,None,'run-a'),('fabric','writer','new','a','p','m',500000,0,0,0,None,'run-a'),('fabric','reviewer','new','a','p','m',500000,0,0,0,None,'run-a'),('fabric','orphan','new','a','p','m',900000,0,0,0,None,'other-run')])\nprint(json.dumps(d.cost_per_accepted(t,db)))",
            crate::differential::LOAD
        );
        let Some(expected) = crate::differential::python(&source, &input) else {
            return;
        };
        crate::differential::equal(&serde_json::to_value(&actual).unwrap(), &expected);
        assert_eq!(actual.len(), 1);
        assert_eq!(actual[0].usage_rows, 2);
        assert_eq!(actual[0].merged, 1);
        assert_eq!(actual[0].cost_usd_lower_bound, Some(12.5));
        assert_eq!(actual[0].cost_per_accepted_usd_lower_bound, Some(12.5));
    }

    #[test]
    fn sidecar_prepare_invalidates_only_sibling_delivery_cursors() {
        let sidecar = PathBuf::from("/project/run/repairs.jsonl");
        let sibling = PathBuf::from("/project/run/findings.jsonl");
        let other = PathBuf::from("/project/other/findings.jsonl");
        let mut store = FakeStore::default();
        store
            .metadata
            .insert("delivery_receipt:/project/run".into(), "2:9".into());
        for path in [&sidecar, &sibling, &other] {
            store.cursors.insert(
                ("delivery".into(), path.to_path_buf()),
                ReceiptCursor {
                    path: path.to_path_buf(),
                    size: 1,
                    mtime_ns: 1,
                    offset: 1,
                },
            );
        }
        let mut filesystem = FakeFilesystem::default();
        filesystem.metadata.insert(
            PathBuf::from("/project/run/RUN.json"),
            ReceiptFileMetadata {
                size: 3,
                mtime_ns: 10,
                ..metadata_for(&[])
            },
        );
        prepare(&mut store, &mut filesystem, &sidecar).unwrap();
        assert_eq!(
            store.deleted,
            vec![
                ("delivery".into(), sibling.clone()),
                ("delivery".into(), sidecar.clone())
            ]
        );
        assert!(store.cursors.contains_key(&("delivery".into(), other)));
        assert_eq!(store.metadata["delivery_receipt:/project/run"], "3:10");
    }

    #[test]
    fn delivery_reports_filter_dates_order_rows_and_recover_bad_review_json() {
        let rows = [
            DeliveryReportRow {
                run_id: Some("new".into()),
                started_at: Some("2026-01-02T00:00:00Z".into()),
                risk_tier: Some("routine".into()),
                chair_family: None,
                writer_model: Some("model".into()),
                review_verdicts: Some("not json".into()),
            },
            DeliveryReportRow {
                run_id: Some("old".into()),
                started_at: Some("2025-12-31T23:59:59Z".into()),
                risk_tier: None,
                chair_family: Some("openai".into()),
                writer_model: None,
                review_verdicts: Some("[]".into()),
            },
            DeliveryReportRow {
                run_id: Some("boundary".into()),
                started_at: Some("2026-01-01T00:00:00Z".into()),
                risk_tier: None,
                chair_family: None,
                writer_model: None,
                review_verdicts: Some("{\"kept\":true}".into()),
            },
            DeliveryReportRow {
                run_id: Some("unstamped".into()),
                started_at: None,
                risk_tier: None,
                chair_family: None,
                writer_model: None,
                review_verdicts: None,
            },
        ];
        let bounded = report_deliveries_from_rows(&rows, Some("2026-01-01")).unwrap();
        assert_eq!(
            bounded
                .iter()
                .map(|row| row.run_id.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("boundary"), Some("new")]
        );
        assert_eq!(bounded[0].review_verdicts, json!({"kept":true}));
        assert_eq!(bounded[1].review_verdicts, json!([]));
        let unbounded = report_deliveries_from_rows(&rows, Some("")).unwrap();
        assert_eq!(unbounded.len(), 4);
        assert_eq!(unbounded[0].run_id.as_deref(), Some("unstamped"));
    }

    #[test]
    fn receipt_ingestion_matches_python_source() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("RUN.json");
        let receipt_value = json!({
            "run_id":"oracle-run",
            "risk_tier":"substantial",
            "chair_family":"openai",
            "writer_model":"gpt-6-luna",
            "created_at":"2026-10-20T00:00:00Z",
            "authority":{"at":"2000-01-01T00:00:00Z"},
            "software_delivery":{"canonical_artifact_id":"chosen"},
            "artifacts":[{"id":"other","git_revision":{"commit":"wrong"}},
                         {"id":"chosen","git_revision":{"commit":"right"}}],
            "delivery_acceptance":{"basis":"checks-and-review"},
            "reviews":[
                {"id":"one","status":"pass","role":"targeted","model":"m","lenses":["privacy"],"secret":"drop"},
                {"id":"two","verdict":"","status":"failed","provider_family":"anthropic"},
                {"id":"three","verdict":null}
            ]
        });
        std::fs::write(&path, serde_json::to_vec(&receipt_value).unwrap()).unwrap();
        let path = std::fs::canonicalize(path).unwrap();
        let file_metadata = std::fs::metadata(&path).unwrap();
        let modified = file_metadata
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        let mtime_seconds = modified.as_secs_f64();
        let mtime_ns = i64::try_from(modified.as_nanos()).unwrap();
        let expected_source = format!(
            "{}\nd=load('telemetry_delivery')\nclass Cursor:\n def fetchone(self): return None\nclass DB:\n def execute(self,q,args):\n  if q.startswith('INSERT INTO deliveries') or q.startswith('INSERT OR REPLACE INTO deliveries'):\n   captured['values']=args\n  return Cursor()\ncaptured={{}}\nt.host_id=lambda:'fixture-host'\nt.now=lambda:'fixed-now'\nd.ingest_receipt(t,DB(),Path(input['path']))\na=captured['values']\nprint(json.dumps({{'run_dir':a[0],'host':a[1],'run_id':a[2],'risk_tier':a[3],'chair_family':a[4],'repair_cycles':a[5],'acceptance_basis':a[6],'accepted':bool(a[7]),'merge_commit':a[8],'ingested_at':a[9],'started_at':a[10],'writer_model':a[11],'review_verdicts':json.loads(a[12])}}))",
            crate::differential::LOAD
        );
        let Some(expected) = crate::differential::python(&expected_source, &json!({"path":path}))
        else {
            return;
        };

        let mut filesystem = FakeFilesystem::default();
        filesystem.metadata.insert(
            path.clone(),
            ReceiptFileMetadata {
                device: 1,
                inode: 2,
                size: file_metadata.len(),
                mtime_ns,
                mtime_seconds,
            },
        );
        filesystem
            .bytes
            .insert(path.clone(), std::fs::read(&path).unwrap());
        let directory = path.parent().unwrap().to_owned();
        filesystem.resolved.insert(directory.clone(), directory);
        let mut store = FakeStore::default();
        ingest_receipt(
            &mut store,
            &mut filesystem,
            &path,
            "fixture-host".into(),
            "fixed-now".into(),
        )
        .unwrap();
        let delivery = store.deliveries.values().next().unwrap();
        crate::differential::equal(&serde_json::to_value(delivery).unwrap(), &expected);
    }

    #[test]
    fn delivery_report_selection_matches_python_source() {
        let rows = vec![
            DeliveryReportRow {
                run_id: Some("later".into()),
                started_at: Some("2026-01-03T00:00:00Z".into()),
                risk_tier: Some("routine".into()),
                chair_family: None,
                writer_model: Some("m".into()),
                review_verdicts: Some("invalid".into()),
            },
            DeliveryReportRow {
                run_id: Some("at".into()),
                started_at: Some("2026-01-02T00:00:00Z".into()),
                risk_tier: None,
                chair_family: Some("openai".into()),
                writer_model: None,
                review_verdicts: Some("{\"kept\":true}".into()),
            },
            DeliveryReportRow {
                run_id: Some("old".into()),
                started_at: Some("2026-01-01T23:59:59Z".into()),
                risk_tier: None,
                chair_family: None,
                writer_model: None,
                review_verdicts: None,
            },
            DeliveryReportRow {
                run_id: Some("missing".into()),
                started_at: None,
                risk_tier: None,
                chair_family: None,
                writer_model: None,
                review_verdicts: None,
            },
            DeliveryReportRow {
                run_id: Some("empty-stamp".into()),
                started_at: Some(String::new()),
                risk_tier: None,
                chair_family: None,
                writer_model: None,
                review_verdicts: None,
            },
        ];
        let input = json!({
            "rows": rows.iter().map(|row| json!([
                row.run_id, row.started_at, row.risk_tier, row.chair_family,
                row.writer_model, row.review_verdicts
            ])).collect::<Vec<_>>(),
            "since":"2026-01-02"
        });
        let source = format!(
            "{}\nimport sqlite3\nd=load('telemetry_delivery')\ndb=sqlite3.connect(':memory:')\ndb.row_factory=sqlite3.Row\ndb.execute('CREATE TABLE deliveries(run_id,started_at,risk_tier,chair_family,writer_model,review_verdicts)')\ndb.executemany('INSERT INTO deliveries VALUES (?,?,?,?,?,?)',input['rows'])\nprint(json.dumps(d.report_deliveries(t,db,input['since'])))",
            crate::differential::LOAD
        );
        let Some(expected) = crate::differential::python(&source, &input) else {
            return;
        };
        let actual = report_deliveries_from_rows(&rows, Some("2026-01-02")).unwrap();
        crate::differential::equal(&serde_json::to_value(actual).unwrap(), &expected);
    }

    #[test]
    fn accepted_report_rendering_matches_python_source() {
        let inputs = AcceptedCostInputs {
            deliveries: vec![Delivery {
                run_dir: "run".into(),
                host: "fixture-host".into(),
                run_id: Some("r".into()),
                risk_tier: Some("substantial".into()),
                accepted: true,
                ..Delivery::default()
            }],
            ..AcceptedCostInputs::default()
        };
        let mut input = json!({
            "deliveries":[["run","fixture-host","r","substantial",1,"now"]],
            "json":true
        });
        let source = format!(
            "{}\nimport contextlib,io,sqlite3\nd=load('telemetry_delivery')\nclass Costs:\n def load_prices(self,t): return {{}},{{}}\n def catalogue(self,t): return {{}}\nt.host_id=lambda:'fixture-host'\nt.load_hosts=lambda db:None\nt.host_aliases=lambda:set()\nt.same_host=lambda value,aliases=None:value\nt.costs_owner=lambda:Costs()\ndb=sqlite3.connect(':memory:')\ndb.row_factory=sqlite3.Row\ndb.execute('CREATE TABLE deliveries(run_dir,host,run_id,risk_tier,chair_family,repair_cycles,acceptance_basis,accepted,merge_commit,ingested_at,started_at,writer_model,review_verdicts)')\ndb.executemany('INSERT INTO deliveries(run_dir,host,run_id,risk_tier,accepted,ingested_at) VALUES (?,?,?,?,?,?)',input['deliveries'])\ndb.execute('CREATE TABLE changes(merge_commit)')\ndb.execute('CREATE TABLE usage(source,event_id,host,delivery_run_id)')\nout=io.StringIO()\nwith contextlib.redirect_stdout(out): d.run_accepted(t,db,None,input['json'])\nprint(json.dumps(out.getvalue()))",
            crate::differential::LOAD
        );
        let Some(expected) = crate::differential::python(&source, &input) else {
            return;
        };
        let mut store = FakeStore {
            accepted: inputs,
            ..FakeStore::default()
        };
        let identity = HostIdentity::new("fixture-host".into(), BTreeSet::new(), &BTreeSet::new());
        let rows = accepted_cost_report(
            &mut store,
            &identity,
            None,
            &costs::Prices::default(),
            &BTreeMap::new(),
        )
        .unwrap();
        let actual = render_accepted(&rows, true).unwrap();
        crate::differential::equal(&json!(actual), &expected);

        input["json"] = json!(false);
        let Some(expected_text) = crate::differential::python(&source, &input) else {
            return;
        };
        let actual_text = render_accepted(&rows, false).unwrap();
        crate::differential::equal(&json!(actual_text), &expected_text);
    }
}
