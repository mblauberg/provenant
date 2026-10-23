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
    AttemptFact, AttemptOutcome, OutputRef, ReviewerFact, RouteFact, UsageCounts,
};
pub use canonical::{record_digest, CanonicalError, CanonicalV1, FileDigest};
pub use json::{decode, decode_v1, encode, DecodeError, EncodeError, Limits, RecordHeader};
pub use jsonc::{decode_jsonc, JsoncDocument, JsoncToken, JsoncTokenKind};
pub use path_wire::{PathWire, PathWireError};
pub use yaml::{
    parse_frontmatter, parse_sidecar, Span as YamlSpan, SpannedYaml, YamlError, YamlErrorKind,
    YamlValue,
};
