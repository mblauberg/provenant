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
    #[serde(flatten)]
    pub lane: super::LaneRow,
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
    pub selected: Option<Value>,
}

/// `data` of `output`, by mode.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum OutputData {
    /// `--trace`: the launch trace and grants.
    Trace {
        #[serde(flatten)]
        lane: super::LaneRow,
        trace: Value,
        grants: Value,
    },
    /// `--path`: where the artifact lives, not its bytes.
    Path {
        #[serde(flatten)]
        lane: super::LaneRow,
        part: String,
        path: crate::StructuredPathWire,
    },
    /// `--part events`: state-change events from a cursor.
    Events {
        #[serde(flatten)]
        lane: super::LaneRow,
        events: Vec<Value>,
    },
    /// `--part receipt`: the reviewer receipt.
    Receipt(Box<ReviewerReceipt>),
    Chunk(Box<OutputChunk>),
}

/// The reviewer receipt of a settled review attempt.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReviewerReceipt {
    #[serde(flatten)]
    pub lane: super::LaneRow,
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
    with_lane(object(
        &[
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
            (
                "changed_files",
                nullable(array(nullable(super::schema::worktree_path()))),
            ),
            ("changed_files_total", any()),
            ("selected", any()),
        ],
    ))
}

pub fn reviewer_receipt_schema() -> Value {
    let a = any();
    with_lane(object(
        &[
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
    ))
}

fn with_lane(mut schema: Value) -> Value {
    let lane = super::lane::lane_row_schema();
    if let (Some(properties), Some(lane_properties)) = (
        schema["properties"].as_object_mut(),
        lane["properties"].as_object(),
    ) {
        properties.extend(lane_properties.clone());
        properties.insert("to".into(), super::schema::worktree_path());
        properties.insert("written".into(), unsigned());
        properties.insert("tail_text".into(), string());
        properties.insert("tail".into(), unsigned());
        properties.insert("tail_clamped".into(), boolean());
        properties.insert("selected".into(), any());
        properties.insert("planner_fallback".into(), any());
        properties.insert("host".into(), string());
    }
    if let (Some(required), Some(lane_required)) = (
        schema["required"].as_array_mut(),
        lane["required"].as_array(),
    ) {
        for field in lane_required {
            if !required.contains(field) {
                required.push(field.clone());
            }
        }
    }
    schema
}

pub fn output_data_schema() -> Value {
    one_of(vec![
        with_lane(object(&[("trace", any()), ("grants", any())], &[])),
        with_lane(object(
            &[
                ("part", choices(ARTIFACT_PARTS)),
                ("path", super::schema::worktree_path()),
            ],
            &[],
        )),
        with_lane(object(&[("events", array(any()))], &[])),
        reviewer_receipt_schema(),
        output_chunk_schema(),
    ])
}
