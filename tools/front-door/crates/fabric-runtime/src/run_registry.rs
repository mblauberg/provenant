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
    pub fn get(self) -> u64 { self.0 }
    pub fn new(value: u64) -> Option<Self> { (value > 1).then_some(Self(value)) }
}
impl<'de> Deserialize<'de> for ProcessId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Number::deserialize(deserializer)?;
        let integer = value.as_u64().or_else(|| {
            let value = value.as_f64()?;
            (value.fract() == 0.0 && (2.0..18_446_744_073_709_551_616.0).contains(&value))
                .then_some(value as u64)
        });
        integer.and_then(Self::new).ok_or_else(|| serde::de::Error::custom("process id must be an integer greater than one in u64 range"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchemaVersion;
impl Serialize for SchemaVersion {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> { serializer.serialize_u8(1) }
}
impl<'de> Deserialize<'de> for SchemaVersion {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Number::deserialize(deserializer)?;
        if value.as_u64() == Some(1) || value.as_f64() == Some(1.0) { Ok(Self) }
        else { Err(serde::de::Error::custom("unsupported run record schema version")) }
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
        assert_eq!(serde_json::to_value(record).unwrap(), json!({"schema_version":1,"kind":"dispatch",
            "owner_pid":12,"owner_pgid":13,"task_id":null,"workspace":17,"extra":{"future":true},"run_dir":"/actual"}));
    }
    #[test]
    fn owner_read_contract_refuses_only_validated_fields() {
        let base = json!({"schema_version":1,"kind":"batch","owner_pid":2,"owner_pgid":3});
        assert!(OwnerRecord::from_value(base.clone(), "/actual").is_ok());
        for (key, value) in [("schema_version",json!(2)),("schema_version",json!(true)),
            ("owner_pid",json!(1)),("owner_pgid",json!(0)),("owner_pid",json!(2.5)),
            ("owner_pid",json!("12")),("kind",json!("provider"))] {
            let mut invalid = base.clone(); invalid[key] = value;
            assert!(OwnerRecord::from_value(invalid, "/actual").is_err(), "{key}");
        }
    }
}
