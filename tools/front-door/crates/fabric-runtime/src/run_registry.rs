//! Typed durable run records. Storage and process observation belong to later owners.
use crate::field::Field;
use serde::{Deserialize, Serialize};

pub const OWNER_RECORD_NAME: &str = "dispatch-owner.json";
pub const PROVIDER_RECORD_NAME: &str = "dispatch-provider.json";
pub const DEFAULT_RETENTION_HOURS: u64 = 168;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OwnerKind {
    Dispatch,
    Batch,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OwnerRecord {
    pub schema_version: u8,
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
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub task_id: Field<String>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub batch_id: Field<String>,
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
    fn records_use_source_wire_names_and_preserve_optional_presence() {
        let record = OwnerRecord {
            schema_version: 1,
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
            task_id: Field::Missing,
            batch_id: Field::Null,
        };
        assert_eq!(
            serde_json::to_value(record).unwrap(),
            json!({
                "schema_version":1,"kind":"dispatch","run_dir":"/run","workspace":"/project",
                "run_token":"token","owner_pid":12,"owner_pgid":12,"owner_started_at":null,
                "host_pid":4,"host_started_at":"start","started_at":"now","owner_stdout":"stdout.log",
                "owner_stderr":"stderr.log","batch_id":null
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
}
