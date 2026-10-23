//! Typed durable run records. Storage and process observation belong to later owners.
use serde::{Deserialize, Serialize};

pub const OWNER_RECORD_NAME: &str = "dispatch-owner.json";
pub const PROVIDER_RECORD_NAME: &str = "dispatch-provider.json";
pub const DEFAULT_RETENTION_HOURS: u64 = 168;

/// A JavaScript integral process identifier greater than one. Integral floats
/// are accepted because JSON.parse does not distinguish `12` from `12.0`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ProcessId(u64);
impl ProcessId {
    pub fn get(self) -> u64 {
        self.0
    }
    pub fn new(value: u64) -> Option<Self> {
        (value > 1).then_some(Self(value))
    }
}
impl<'de> Deserialize<'de> for ProcessId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Number::deserialize(deserializer)?;
        let integer = value.as_u64().or_else(|| {
            let value = value.as_f64()?;
            (value.fract() == 0.0 && (2.0..18_446_744_073_709_551_616.0).contains(&value))
                .then_some(value as u64)
        });
        integer.and_then(Self::new).ok_or_else(|| {
            serde::de::Error::custom("process id must be an integer greater than one in u64 range")
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchemaVersion;
impl Serialize for SchemaVersion {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(1)
    }
}
impl<'de> Deserialize<'de> for SchemaVersion {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Number::deserialize(deserializer)?;
        if value.as_u64() == Some(1) || value.as_f64() == Some(1.0) {
            Ok(Self)
        } else {
            Err(serde::de::Error::custom(
                "unsupported run record schema version",
            ))
        }
    }
}

fn read_run_dir<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value.as_str().unwrap_or_default().to_owned())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OwnerKind {
    Dispatch,
    Batch,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OwnerRecord {
    pub schema_version: SchemaVersion,
    pub kind: OwnerKind,
    #[serde(default, deserialize_with = "read_run_dir")]
    pub run_dir: String,
    pub owner_pid: ProcessId,
    pub owner_pgid: ProcessId,
    /// readOwnerRecord validates only schema, kind and owner IDs. Retain all
    /// other fields verbatim, including missing/null/mistyped known fields.
    #[serde(flatten)]
    pub fields: serde_json::Map<String, serde_json::Value>,
}

impl OwnerRecord {
    pub fn from_value(value: serde_json::Value, run_dir: &str) -> Result<Self, serde_json::Error> {
        let mut record: Self = serde_json::from_value(value)?;
        record.run_dir = run_dir.to_owned();
        Ok(record)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderRecord {
    pub run_token: String,
    pub provider_pid: u64,
    pub provider_pgid: u64,
    pub provider_started_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecordedRun {
    #[serde(flatten)]
    pub owner: OwnerRecord,
    pub run_id: String,
    pub running: bool,
    pub orphaned: bool,
    pub provider: Option<ProviderRecord>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn owner_read_contract_preserves_unvalidated_fields_and_overwrites_directory() {
        let value = json!({"schema_version":1.0,"kind":"dispatch","owner_pid":12.0,"owner_pgid":13,
            "task_id":null,"workspace":17,"extra":{"future":true},"run_dir":false});
        let record = OwnerRecord::from_value(value, "/actual").unwrap();
        assert_eq!(record.owner_pid.get(), 12);
        assert_eq!(
            serde_json::to_value(record).unwrap(),
            json!({"schema_version":1,"kind":"dispatch",
            "owner_pid":12,"owner_pgid":13,"task_id":null,"workspace":17,"extra":{"future":true},"run_dir":"/actual"})
        );
    }
    #[test]
    fn owner_read_contract_refuses_only_validated_fields() {
        let base = json!({"schema_version":1,"kind":"batch","owner_pid":2,"owner_pgid":3});
        assert!(OwnerRecord::from_value(base.clone(), "/actual").is_ok());
        for (key, value) in [
            ("schema_version", json!(2)),
            ("schema_version", json!(true)),
            ("owner_pid", json!(1)),
            ("owner_pgid", json!(0)),
            ("owner_pid", json!(2.5)),
            ("owner_pid", json!("12")),
            ("kind", json!("provider")),
        ] {
            let mut invalid = base.clone();
            invalid[key] = value;
            assert!(
                OwnerRecord::from_value(invalid, "/actual").is_err(),
                "{key}"
            );
        }
    }
    #[test]
    fn node_differential_owner_records_cover_missing_null_invalid_and_extra_fields() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().to_str().unwrap();
        let fixtures = [
            r#"{"schema_version":1,"kind":"dispatch","owner_pid":12,"owner_pgid":12}"#,
            r#"{"schema_version":1.0,"kind":"batch","owner_pid":12.0,"owner_pgid":12.0,"task_id":null,"batch_id":null}"#,
            r#"{"schema_version":1,"kind":"dispatch","owner_pid":1,"owner_pgid":12}"#,
            r#"{"schema_version":1,"kind":"dispatch","owner_pid":12,"owner_pgid":12,"run_dir":false,"workspace":17,"future":{"x":[1,true,null]}}"#,
            r#"{"schema_version":2,"kind":"dispatch","owner_pid":12,"owner_pgid":12}"#,
        ];
        let script = r#"import {readOwnerRecord,writeOwnerRecord} from './src/run-registry.ts';
import {writeFileSync,readFileSync} from 'node:fs';
import {join} from 'node:path';
const [dir, fixtures] = process.argv.slice(1);
console.log(JSON.stringify(JSON.parse(fixtures).map(text => {
 writeFileSync(join(dir,'dispatch-owner.json'),text);
 const record=readOwnerRecord(dir);
 if(record===undefined) return null;
 writeOwnerRecord(record);
 return JSON.parse(readFileSync(join(dir,'dispatch-owner.json'),'utf8'));
})));"#;
        let Some(expected) = crate::test_support::node(
            script,
            &[dir.into(), serde_json::to_string(&fixtures).unwrap()],
        ) else {
            return;
        };
        let actual: Vec<_> = fixtures
            .iter()
            .map(|text| {
                OwnerRecord::from_value(serde_json::from_str(text).unwrap(), dir)
                    .ok()
                    .map(|record| serde_json::to_value(record).unwrap())
                    .unwrap_or(serde_json::Value::Null)
            })
            .collect();
        assert_eq!(serde_json::json!(actual), expected);
    }
}

/// A provider record is rebuilt from validated fields; extras are intentionally
/// discarded here, unlike owner records which spread their original object.
pub fn provider_record(value: serde_json::Value, run_token: &str) -> Option<ProviderRecord> {
    let record = value.as_object()?;
    if record.get("run_token")?.as_str()? != run_token {
        return None;
    }
    let pid: ProcessId = serde_json::from_value(record.get("provider_pid")?.clone()).ok()?;
    let pgid: ProcessId = serde_json::from_value(record.get("provider_pgid")?.clone()).ok()?;
    Some(ProviderRecord {
        run_token: run_token.into(),
        provider_pid: pid.get(),
        provider_pgid: pgid.get(),
        provider_started_at: record
            .get("provider_started_at")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
    })
}

pub const FAILED_RECEIPT_STATUSES: &[&str] = &[
    "failed",
    "quarantined",
    "partial",
    "usage_limited",
    "rate_limited",
    "auth_required",
    "model_unavailable",
    "permission_blocked",
    "stalled",
    "startup_timeout",
    "writer_changed",
    "timed_out",
    "interrupted",
    "lost",
    "rejected",
    "tool_missing",
];
/// Filesystem and liveness facts are observations, not inferred process death.
/// An unavailable liveness probe must be supplied as alive, as in observedAlive.
pub struct PruneFacts<'a> {
    pub name: &'a str,
    pub directory: bool,
    pub symlink: bool,
    pub keep: bool,
    pub run_json: bool,
    pub receipt: Option<&'a serde_json::Map<String, serde_json::Value>>,
    pub owner_alive: bool,
    pub host_alive: bool,
    pub provider_alive: bool,
    pub newest_mtime_ms: f64,
}
fn status_string(value: &serde_json::Value) -> Option<&str> {
    match value {
        serde_json::Value::String(value) => Some(value),
        serde_json::Value::Array(values) if values.len() == 1 => status_string(&values[0]),
        _ => None,
    }
}
pub fn should_prune(
    facts: &PruneFacts<'_>,
    now_ms: f64,
    retention_hours: f64,
    retention_configured: bool,
) -> bool {
    use serde_json::Value;
    if !facts.directory || facts.symlink || facts.keep || facts.run_json {
        return false;
    }
    let status = facts.receipt.and_then(|receipt| receipt.get("status"));
    if status
        .and_then(Value::as_str)
        .is_some_and(|status| matches!(status, "active" | "input_required"))
        || facts.receipt.and_then(|receipt| receipt.get("resumable")) == Some(&Value::Bool(true))
    {
        return false;
    }
    let successful = status.is_some_and(crate::success_status::is_success_status)
        || status.and_then(Value::as_str) == Some("cancelled");
    let failed = status
        .and_then(status_string)
        .is_some_and(|status| FAILED_RECEIPT_STATUSES.contains(&status));
    if facts.name.starts_with("runs/") && !successful && !failed {
        return false;
    }
    if facts.owner_alive || facts.host_alive || facts.provider_alive {
        return false;
    }
    let cutoff = if failed && !retention_configured {
        now_ms - 14.0 * 86_400_000.0
    } else {
        now_ms - retention_hours * 3_600_000.0
    };
    facts.newest_mtime_ms <= cutoff
}

#[cfg(test)]
mod lifecycle_policy_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn provider_normalisation_matches_node_for_extra_null_invalid_and_float_fields() {
        let root = tempfile::tempdir().unwrap();
        let fixtures = json!([
            {"run_token":"t","provider_pid":12.0,"provider_pgid":13,"provider_started_at":null,"extra":17},
            {"run_token":"t","provider_pid":12,"provider_pgid":13,"provider_started_at":false},
            {"run_token":"t","provider_pid":1,"provider_pgid":13},
            {"run_token":"wrong","provider_pid":12,"provider_pgid":13}
        ]);
        let script = r#"import {readProviderRecord} from './src/run-registry.ts';import {writeFileSync} from 'node:fs';import {join} from 'node:path';const [root,values]=process.argv.slice(1);console.log(JSON.stringify(JSON.parse(values).map(value=>{writeFileSync(join(root,'dispatch-provider.json'),JSON.stringify(value));return readProviderRecord(root,'t')})));"#;
        let Some(expected) = crate::test_support::node(
            script,
            &[root.path().to_str().unwrap().into(), fixtures.to_string()],
        ) else {
            return;
        };
        assert_eq!(
            json!(fixtures
                .as_array()
                .unwrap()
                .iter()
                .map(|value| provider_record(value.clone(), "t"))
                .collect::<Vec<_>>()),
            expected
        );
    }
    #[test]
    fn retention_respects_live_owners_resumability_failed_window_and_exact_cutoff() {
        let receipt = json!({"status":"ok"});
        let mut facts = PruneFacts {
            name: "runs/20261023-0000-dispatch-test",
            directory: true,
            symlink: false,
            keep: false,
            run_json: false,
            receipt: receipt.as_object(),
            owner_alive: false,
            host_alive: false,
            provider_alive: false,
            newest_mtime_ms: 0.0,
        };
        let now = 168.0 * 3_600_000.0;
        assert!(should_prune(&facts, now, 168.0, false));
        facts.newest_mtime_ms = 1.0;
        assert!(!should_prune(&facts, now, 168.0, false));
        facts.newest_mtime_ms = 0.0;
        facts.host_alive = true;
        assert!(!should_prune(&facts, now, 168.0, false));
        facts.host_alive = false;
        let failed = json!({"status":"failed"});
        facts.receipt = failed.as_object();
        assert!(!should_prune(&facts, now, 168.0, false));
        assert!(should_prune(&facts, now, 168.0, true));
        let resumable = json!({"status":"ok","resumable":true});
        facts.receipt = resumable.as_object();
        assert!(!should_prune(&facts, now, 168.0, true));
        facts.receipt = None;
        assert!(!should_prune(&facts, now, 168.0, true));
    }
}
