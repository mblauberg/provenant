//! Bounded record codecs, canonical digest bytes, and shared serialized facts.
#![forbid(unsafe_code)]

mod artifact_ref;
mod attempt_fact;
mod canonical;
mod json;
mod jsonc;
mod path_wire;
mod yaml;

pub use artifact_ref::ArtifactRef;
pub use attempt_fact::{
    AttemptFact, AttemptOutcome, OutputRef, ReviewerFact, ReviewerProvenance, RouteFact, Usage,
    UsageCounts,
};
pub use canonical::{CanonicalBytes, CanonicalError, CanonicalV1, FileDigest, record_digest};
pub use json::{DecodeError, EncodeError, Limits, RecordHeader, decode, decode_v1, encode};
pub use jsonc::{JsoncDocument, JsoncToken, JsoncTokenKind, decode_jsonc};
pub use path_wire::{PathWire, PathWireError};
pub use yaml::{
    Span as YamlSpan, SpannedYaml, YamlError, YamlErrorKind, YamlValue, parse_frontmatter,
    parse_sidecar,
};
