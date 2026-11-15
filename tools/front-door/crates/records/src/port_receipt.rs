//! Versioned evidence for repository command ports.
use crate::FileDigest;
pub use crate::ports::PortSource;
use provenant_core::{ActorId, AttemptId, ProjectId};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

pub const PORT_COMMAND_RECEIPT_SCHEMA_VERSION: u16 = 2;
pub const DEFAULT_PORT_COMMAND_RECEIPT_AGE_DAYS: u32 = 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortCommandDisposition {
    Succeeded,
    Unknown,
    Refused,
    TimedOut,
    Cancelled,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortCommandBackend {
    RepositoryCommand,
}

/// Retention is attached to each persisted receipt so collection uses the
/// lifetime selected by its owner when it was written.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PortReceiptLifetime {
    /// Admission evidence waits for terminal attempt state and a proven
    /// release at the same lease epoch.
    AttemptTerminalAndRelease {
        attempt_id: AttemptId,
        project_id: ProjectId,
        actor_id: ActorId,
        lease_epoch: u64,
    },
    /// Other port evidence expires after its declared age.
    AgeDays { days: u32 },
}

/// One content-free command receipt. Stored data is decoded by the bounded
/// records codec and this type accepts exactly its current schema version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortCommandReceipt {
    #[serde(deserialize_with = "schema_version")]
    schema_version: u16,
    pub owner: String,
    pub backend: PortCommandBackend,
    pub source: PortSource,
    pub phase: String,
    pub request_key_digest: FileDigest,
    pub command_digest: FileDigest,
    pub outcome: PortCommandDisposition,
    pub exit_code: Option<i64>,
    pub elapsed_micros: Option<u64>,
    pub stdout_digest: Option<FileDigest>,
    pub capture_complete: bool,
    pub timed_out: bool,
    pub cancelled: bool,
    pub recorded_at_unix_seconds: u64,
    pub lifetime: PortReceiptLifetime,
}

impl PortCommandReceipt {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        owner: String,
        source: PortSource,
        phase: String,
        request_key_digest: FileDigest,
        command_digest: FileDigest,
        outcome: PortCommandDisposition,
        exit_code: Option<i64>,
        elapsed_micros: Option<u64>,
        stdout_digest: Option<FileDigest>,
        capture_complete: bool,
        timed_out: bool,
        cancelled: bool,
        recorded_at_unix_seconds: u64,
        lifetime: PortReceiptLifetime,
    ) -> Self {
        Self {
            schema_version: PORT_COMMAND_RECEIPT_SCHEMA_VERSION,
            owner,
            backend: PortCommandBackend::RepositoryCommand,
            source,
            phase,
            request_key_digest,
            command_digest,
            outcome,
            exit_code,
            elapsed_micros,
            stdout_digest,
            capture_complete,
            timed_out,
            cancelled,
            recorded_at_unix_seconds,
            lifetime,
        }
    }

    pub const fn schema_version(&self) -> u16 {
        self.schema_version
    }
}

fn schema_version<'de, D>(deserializer: D) -> Result<u16, D::Error>
where
    D: Deserializer<'de>,
{
    let version = u16::deserialize(deserializer)?;
    if version == PORT_COMMAND_RECEIPT_SCHEMA_VERSION {
        Ok(version)
    } else {
        Err(D::Error::custom(
            "unsupported port command receipt schema_version",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn sample() -> PortCommandReceipt {
        PortCommandReceipt::new(
            "admission".into(),
            PortSource::Project,
            "release".into(),
            crate::CanonicalV1::file_digest(b"attempt:1"),
            crate::CanonicalV1::file_digest(b"command"),
            PortCommandDisposition::Succeeded,
            Some(0),
            Some(1),
            None,
            true,
            false,
            false,
            42,
            PortReceiptLifetime::AttemptTerminalAndRelease {
                attempt_id: AttemptId::from_bytes([1; 16]),
                project_id: ProjectId::from_bytes([2; 16]),
                actor_id: ActorId::from_bytes([3; 16]),
                lease_epoch: 1,
            },
        )
    }

    #[test]
    fn receipt_round_trips_through_bounded_decoder() {
        let original = sample();
        let encoded = crate::encode(&original).expect("receipt encodes");
        let decoded: PortCommandReceipt = crate::decode(
            Cursor::new(encoded),
            crate::Limits {
                max_bytes: 4096,
                max_depth: 8,
                max_members: 32,
            },
        )
        .expect("receipt decodes");
        assert_eq!(decoded, original);
    }

    #[test]
    fn receipt_decoder_rejects_unknown_schema_version() {
        let mut value = serde_json::to_value(sample()).expect("receipt serialises");
        value["schema_version"] = serde_json::json!(3);
        let error = crate::decode::<PortCommandReceipt, _>(
            Cursor::new(serde_json::to_vec(&value).expect("fixture serialises")),
            crate::Limits {
                max_bytes: 4096,
                max_depth: 8,
                max_members: 32,
            },
        )
        .expect_err("unknown versions are refused");
        assert!(matches!(error, crate::DecodeError::Typed { .. }));
    }
}
