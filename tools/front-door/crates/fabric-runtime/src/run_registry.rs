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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OwnerKind {
    Dispatch,
    Batch,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OwnerRecord {
    pub schema_version: SchemaVersion,
    pub kind: OwnerKind,
    pub run_dir: String,
    pub owner_pid: ProcessId,
    pub owner_pgid: ProcessId,
    /// Preserve unvalidated fields, including null and mistyped known fields.
    pub fields: crate::pending::OrderedMap<serde_json::Value>,
    wire_order: Vec<String>,
}
impl<'de> Deserialize<'de> for OwnerRecord {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut fields =
            crate::pending::OrderedMap::<serde_json::Value>::deserialize(deserializer)?;
        let mut wire_order: Vec<_> = fields.0.iter().map(|(key, _)| key.clone()).collect();
        if !wire_order.iter().any(|key| key == "run_dir") {
            wire_order.push("run_dir".into());
        }
        fn take<T: serde::de::DeserializeOwned, E: serde::de::Error>(
            fields: &mut crate::pending::OrderedMap<serde_json::Value>,
            key: &str,
        ) -> Result<T, E> {
            serde_json::from_value(
                fields
                    .remove(key)
                    .ok_or_else(|| E::custom(format!("missing {key}")))?,
            )
            .map_err(E::custom)
        }
        let schema_version = take(&mut fields, "schema_version")?;
        let kind = take(&mut fields, "kind")?;
        let owner_pid = take(&mut fields, "owner_pid")?;
        let owner_pgid = take(&mut fields, "owner_pgid")?;
        let run_dir = fields
            .remove("run_dir")
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        // JS object enumeration moves canonical array-index keys first.
        wire_order.sort_by_key(|key| {
            key.parse::<u32>()
                .ok()
                .filter(|n| *n != u32::MAX && n.to_string() == *key)
                .map_or((1, 0), |n| (0, n))
        });
        Ok(Self {
            schema_version,
            kind,
            run_dir,
            owner_pid,
            owner_pgid,
            fields,
            wire_order,
        })
    }
}
impl Serialize for OwnerRecord {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        for key in &self.wire_order {
            match key.as_str() {
                "schema_version" => map.serialize_entry(key, &self.schema_version)?,
                "kind" => map.serialize_entry(key, &self.kind)?,
                "run_dir" => map.serialize_entry(key, &self.run_dir)?,
                "owner_pid" => map.serialize_entry(key, &self.owner_pid)?,
                "owner_pgid" => map.serialize_entry(key, &self.owner_pgid)?,
                _ => {
                    if let Some(value) = self.fields.get(key) {
                        map.serialize_entry(key, value)?;
                    }
                }
            }
        }
        for (key, value) in &self.fields.0 {
            if !self.wire_order.contains(key) {
                map.serialize_entry(key, value)?;
            }
        }
        map.end()
    }
}
impl OwnerRecord {
    pub fn from_json(text: &str, run_dir: &str) -> Result<Self, serde_json::Error> {
        let mut record: Self = serde_json::from_str(text)?;
        record.run_dir = run_dir.to_owned();
        Ok(record)
    }
    /// Value inputs have already lost original JSON object insertion order.
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
    fn unsafe_integer_boundary_is_explicit_and_not_claimed_as_parity() {
        let wire =
            r#"{"schema_version":1,"kind":"batch","owner_pid":9007199254740993,"owner_pgid":12}"#;
        let Some(expected) = crate::test_support::node(
            "console.log(JSON.stringify(JSON.parse(process.argv[1]).owner_pid.toString()))",
            &[wire.into()],
        ) else {
            return;
        };
        assert_eq!(expected, "9007199254740992");
        assert_eq!(
            OwnerRecord::from_json(wire, "/actual")
                .unwrap()
                .owner_pid
                .get(),
            9_007_199_254_740_993
        );
        // RP-FR3-003: do not silently round an integer through f64. The
        // interface marker stays withdrawn until the records owner resolves it.
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
            r#"{"z":true,"run_dir":"wrong","schema_version":1,"kind":"batch","owner_pid":12,"owner_pgid":12,"a":null,"2":false,"1":true}"#,
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
 return JSON.stringify(JSON.parse(readFileSync(join(dir,'dispatch-owner.json'),'utf8')));
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
                OwnerRecord::from_json(text, dir)
                    .ok()
                    .map(|record| {
                        serde_json::Value::String(serde_json::to_string(&record).unwrap())
                    })
                    .unwrap_or(serde_json::Value::Null)
            })
            .collect();
        assert_eq!(serde_json::json!(actual), expected);
    }
    #[test]
    fn owner_extra_wire_discrepancies_wait_for_the_shared_lossless_codec() {
        // Deliberate mismatch evidence, not parity or a completed-unit claim.
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().to_str().unwrap();
        let prefix = r#"{"schema_version":1,"kind":"dispatch","owner_pid":12,"owner_pgid":12"#;
        let extras = [
            (r#""x":1.0"#, r#""x":1"#, r#""x":1.0"#),
            (r#""x":1e21"#, r#""x":1e+21"#, r#""x":1e+21"#),
            (r#""future":{"b":1,"a":2}"#, r#""future":{"b":1,"a":2}"#, r#""future":{"a":2,"b":1}"#),
        ];
        let fixtures: Vec<_> = extras.iter().map(|(extra, _, _)| format!("{prefix},{extra}}}")).collect();
        let Some(node) = crate::test_support::node(
            r#"import {readOwnerRecord,writeOwnerRecord} from './src/run-registry.ts';
import {writeFileSync,readFileSync} from 'node:fs'; import {join} from 'node:path';
const [dir,fixtures]=process.argv.slice(1);
console.log(JSON.stringify(JSON.parse(fixtures).map(text=>{
writeFileSync(join(dir,'dispatch-owner.json'),text); writeOwnerRecord(readOwnerRecord(dir));
return JSON.stringify(JSON.parse(readFileSync(join(dir,'dispatch-owner.json'),'utf8')));
})));"#,
            &[dir.into(), serde_json::to_string(&fixtures).unwrap()],
        ) else { return; };
        let directory = serde_json::to_string(dir).unwrap();
        let wire = |extra: &str| format!("{prefix},{extra},\"run_dir\":{directory}}}");
        let expected_node: Vec<_> = extras.iter().map(|(_, extra, _)| wire(extra)).collect();
        assert_eq!(node, serde_json::json!(expected_node));
        let rust: Vec<_> = fixtures.iter().map(|text| {
            serde_json::to_string(&OwnerRecord::from_json(text, dir).unwrap()).unwrap()
        }).collect();
        let expected_rust: Vec<_> = extras.iter().map(|(_, _, extra)| wire(extra)).collect();
        assert_eq!(rust, expected_rust);
        for (index, (rust, node)) in rust.iter().zip(expected_node).enumerate() {
            if index == 1 { assert_eq!(*rust, node); } else { assert_ne!(*rust, node); }
        }
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

#[derive(Debug, thiserror::Error)]
pub enum RecordReadError {
    #[error("run record exceeds byte limit {limit}")]
    TooLarge { limit: u64 },
}
/// Held-directory read using the newly landed supervisor filesystem owner.
/// As in readJson, missing, linked, non-file and malformed entries are absent.
/// The explicit bound is an unfinished boundary: callers must choose a limit
/// before this can replace the unbounded TypeScript reader.
pub fn read_json_at(
    directory: &dyn provenant_supervisor::fs::SafeDir,
    name: &std::ffi::OsStr,
    limit: u64,
) -> Result<Option<serde_json::Value>, RecordReadError> {
    use provenant_supervisor::fs::FileKind;
    use std::io::Read;
    let Ok(metadata) = directory.metadata_at(name, false) else {
        return Ok(None);
    };
    if metadata.kind != FileKind::Regular {
        return Ok(None);
    }
    let Ok(mut file) = directory.open_read(name) else {
        return Ok(None);
    };
    if !file
        .metadata()
        .is_ok_and(|metadata| metadata.kind == FileKind::Regular)
    {
        return Ok(None);
    }
    let mut bytes = vec![];
    if (&mut file)
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .is_err()
    {
        return Ok(None);
    }
    if bytes.len() as u64 > limit {
        return Err(RecordReadError::TooLarge { limit });
    }
    Ok(
        serde_json::from_str::<serde_json::Value>(&String::from_utf8_lossy(&bytes))
            .ok()
            .filter(serde_json::Value::is_object),
    )
}

#[cfg(test)]
mod safe_record_tests {
    use super::*;
    #[test]
    fn safe_record_reads_refuse_links_non_files_malformed_and_oversize_values() {
        use provenant_supervisor::fs::{Directory, SafeDir};
        use std::{ffi::OsStr, fs};
        let root = tempfile::tempdir().unwrap();
        let dir = Directory::open_anchor(root.path()).unwrap();
        let directory: &dyn SafeDir = &dir;
        fs::write(root.path().join("valid"), r#"{"x":1}"#).unwrap();
        fs::write(root.path().join("invalid"), "{").unwrap();
        fs::write(root.path().join("array"), "[]").unwrap();
        fs::create_dir(root.path().join("directory")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("valid", root.path().join("link")).unwrap();
        assert_eq!(
            read_json_at(directory, OsStr::new("valid"), 7).unwrap(),
            Some(serde_json::json!({"x":1}))
        );
        assert!(matches!(
            read_json_at(directory, OsStr::new("valid"), 6),
            Err(RecordReadError::TooLarge { limit: 6 })
        ));
        for name in ["missing", "invalid", "array", "directory", "link"] {
            assert_eq!(
                read_json_at(directory, OsStr::new(name), 100).unwrap(),
                None
            );
        }
    }
}
