//! Content-free observations available at an attempt boundary.
use provenant_core::WallTime;
use serde::{Deserialize, Serialize};

/// An observation is effective only at its named capture boundary. A missing
/// observation means unknown, never a default supplied by the reporting host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation<T> {
    pub value: T,
    pub source: String,
    pub effective_at: WallTime,
    pub scope: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HashedBytes {
    pub sha256: String,
    pub bytes: u64,
}

/// Stored inside the existing attempt trace, without a second identity index.
/// All fields are optional so pending and historical attempts remain readable.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TelemetryContext {
    pub provenant_version: Option<Observation<String>>,
    pub provenant_digest: Option<Observation<HashedBytes>>,
    pub release_source_digest: Option<Observation<String>>,
    pub instructions: Option<Observation<HashedBytes>>,
    pub harness_version: Option<Observation<String>>,
    pub harness_experiment: Option<Observation<String>>,
    pub provenant_experiment: Option<Observation<String>>,
    pub system_prompt: Option<Observation<HashedBytes>>,
    pub tool_schema: Option<Observation<HashedBytes>>,
    pub git_base: Option<Observation<provenant_core::GitOid>>,
    pub git_head: Option<Observation<provenant_core::GitOid>>,
}

impl TelemetryContext {
    pub const TRACE_KEY: &'static str = "telemetry_context";

    pub fn from_trace(trace: &crate::Trace) -> Option<Self> {
        serde_json::from_value(trace.extra.get(Self::TRACE_KEY)?.clone()).ok()
    }
}
