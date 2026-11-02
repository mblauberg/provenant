//! `output` modes. The wire keeps one `data` object per mode with no tag, so
//! existing consumers keep reading it; the schema tells the modes apart by
//! their closed member sets.
use super::schema::{
    any, array, boolean, choices, constant, nullable, object, one_of, string, unsigned,
};
use serde::Serialize;
use serde_json::Value;

/// How chunk bytes are carried: lossless in JSON either way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "encoding", content = "data", rename_all = "snake_case")]
pub enum ChunkBytes {
    Utf8(String),
    Base64(String),
}

/// `--part` names an artifact: result, prompt, stderr or transcript.
pub const ARTIFACT_PARTS: &[&str] = &["result", "prompt", "stderr", "transcript"];

/// A byte range of an attempt artifact.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OutputChunk {
    pub attempt_id: String,
    pub state: String,
    pub reason: Option<String>,
    pub part: String,
    pub offset: u64,
    pub next_offset: u64,
    pub length: u64,
    pub eof: bool,
    pub clipped: bool,
    pub complete: bool,
    pub digest: Option<String>,
    pub bytes: ChunkBytes,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tail: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tail_clamped: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_files: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_files_total: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_error: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_path: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected: Option<Value>,
}

/// `data` of `output`, by mode.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum OutputData {
    /// `--part trace`: the launch trace and grants.
    Trace {
        attempt_id: String,
        trace: Value,
        grants: Value,
    },
    /// `--path`: where the artifact lives, not its bytes.
    Path {
        attempt_id: String,
        part: String,
        path: String,
    },
    /// `--part events`: state-change events from a cursor.
    Events {
        attempt_id: String,
        events: Vec<Value>,
    },
    /// `--part receipt`: the reviewer receipt.
    Receipt(Box<ReviewerReceipt>),
    Chunk(Box<OutputChunk>),
}

/// The reviewer receipt of a settled review attempt.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReviewerReceipt {
    pub run_id: String,
    pub task_id: String,
    pub attempt_id: String,
    pub adapter: Value,
    pub model: Value,
    pub effort: Value,
    pub family: Value,
    pub role: Value,
    pub resolved_from: Value,
    pub receipt: String,
    pub reviewer: Value,
    pub reviewer_provenance: Value,
}

pub fn chunk_bytes_schema() -> Value {
    one_of(vec![
        object(&[("encoding", constant("utf8")), ("data", string())], &[]),
        object(
            &[
                ("encoding", constant("base64")),
                ("data", super::schema::base64()),
            ],
            &[],
        ),
    ])
}

pub fn output_chunk_schema() -> Value {
    object(
        &[
            ("attempt_id", string()),
            ("state", string()),
            ("reason", nullable(string())),
            ("part", choices(ARTIFACT_PARTS)),
            ("offset", unsigned()),
            ("next_offset", unsigned()),
            ("length", unsigned()),
            ("eof", boolean()),
            ("clipped", boolean()),
            ("complete", boolean()),
            ("digest", nullable(string())),
            ("bytes", chunk_bytes_schema()),
        ],
        &[
            ("tail", unsigned()),
            ("tail_clamped", boolean()),
            ("changed_files", any()),
            ("changed_files_total", any()),
            ("provider_error", nullable(string())),
            ("result_path", nullable(string())),
            ("selected", any()),
        ],
    )
}

pub fn reviewer_receipt_schema() -> Value {
    let a = any();
    object(
        &[
            ("run_id", string()),
            ("task_id", string()),
            ("attempt_id", string()),
            ("adapter", a.clone()),
            ("model", a.clone()),
            ("effort", a.clone()),
            ("family", a.clone()),
            ("role", a.clone()),
            ("resolved_from", a.clone()),
            ("receipt", string()),
            ("reviewer", a.clone()),
            ("reviewer_provenance", a),
        ],
        &[],
    )
}

pub fn output_data_schema() -> Value {
    one_of(vec![
        object(
            &[
                ("attempt_id", string()),
                ("trace", any()),
                ("grants", any()),
            ],
            &[],
        ),
        object(
            &[
                ("attempt_id", string()),
                ("part", choices(ARTIFACT_PARTS)),
                ("path", string()),
            ],
            &[],
        ),
        object(&[("attempt_id", string()), ("events", array(any()))], &[]),
        reviewer_receipt_schema(),
        output_chunk_schema(),
    ])
}
