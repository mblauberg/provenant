//! Typed durable run records. Storage and process observation belong to later owners.
use serde::{Deserialize, Serialize};

pub const OWNER_RECORD_NAME: &str = "dispatch-owner.json";
pub const PROVIDER_RECORD_NAME: &str = "dispatch-provider.json";
pub const DEFAULT_RETENTION_HOURS: u64 = 168;

fn deserialize_optional_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    String::deserialize(deserializer).map(Some)
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
        let version = u8::deserialize(deserializer)?;
        if version == 1 {
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OwnerRecord {
    pub schema_version: SchemaVersion,
    pub kind: OwnerKind,
    pub run_dir: String,
    pub workspace: String,
    pub run_token: String,
    pub owner_pid: u64,
    pub owner_pgid: u64,
    pub owner_started_at: Option<String>,
    pub host_pid: u64,
    pub host_started_at: Option<String>,
    pub started_at: String,
    pub owner_stdout: String,
    pub owner_stderr: String,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_string",
        skip_serializing_if = "Option::is_none"
    )]
    pub task_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_string",
        skip_serializing_if = "Option::is_none"
    )]
    pub batch_id: Option<String>,
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
    use serde_json::{Value, json};

    #[test]
    fn records_use_source_wire_names_and_preserve_optional_presence() {
        let record = OwnerRecord {
            schema_version: SchemaVersion,
            kind: OwnerKind::Dispatch,
            run_dir: "/run".into(),
            workspace: "/project".into(),
            run_token: "token".into(),
            owner_pid: 12,
            owner_pgid: 12,
            owner_started_at: None,
            host_pid: 4,
            host_started_at: Some("start".into()),
            started_at: "now".into(),
            owner_stdout: "stdout.log".into(),
            owner_stderr: "stderr.log".into(),
            task_id: None,
            batch_id: None,
        };
        assert_eq!(
            serde_json::to_value(record).unwrap(),
            json!({
                "schema_version":1,"kind":"dispatch","run_dir":"/run","workspace":"/project",
                "run_token":"token","owner_pid":12,"owner_pgid":12,"owner_started_at":null,
                "host_pid":4,"host_started_at":"start","started_at":"now","owner_stdout":"stdout.log",
                "owner_stderr":"stderr.log"
            })
        );
    }

    #[test]
    fn recorded_run_flattens_owner_fields_and_keeps_null_provider() {
        let wire = json!({
            "schema_version":1,"kind":"batch","run_dir":"/run","workspace":"/project",
            "run_token":"token","owner_pid":12,"owner_pgid":12,"owner_started_at":null,
            "host_pid":4,"host_started_at":null,"started_at":"now","owner_stdout":"out",
            "owner_stderr":"err","run_id":"mcp-token","running":true,"orphaned":false,"provider":null
        });
        let record: RecordedRun = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(record).unwrap(), wire);
    }

    #[test]
    fn records_reject_unknown_schema_versions_and_nullable_optional_strings() {
        let base = json!({
            "schema_version":1,"kind":"dispatch","run_dir":"/run","workspace":"/project",
            "run_token":"token","owner_pid":12,"owner_pgid":12,"owner_started_at":null,
            "host_pid":4,"host_started_at":null,"started_at":"now","owner_stdout":"out","owner_stderr":"err"
        });
        let mut invalid = base.clone();
        invalid["schema_version"] = json!(2);
        assert!(serde_json::from_value::<OwnerRecord>(invalid).is_err());
        let mut invalid = base;
        invalid["task_id"] = Value::Null;
        assert!(serde_json::from_value::<OwnerRecord>(invalid).is_err());
    }
}
