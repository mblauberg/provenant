//! Bounded record codecs, canonical digest bytes, and shared serialized facts.
#![forbid(unsafe_code)]

mod adapters;
pub use adapters::{ADAPTER_SPECS, AdapterSpec, QuotaProbeKind};
mod artifact_ref;
mod attempt_fact;
mod attempt_file;
mod canonical;
mod finding;
mod install;
mod json;
mod jsonc;
mod path_wire;
mod source;
mod trace;
mod yaml;

pub use artifact_ref::ArtifactRef;
pub use attempt_fact::{
    AttemptFact, AttemptOutcome, OutputRef, ReviewerFact, ReviewerProvenance, RouteFact,
    TranscriptRef, Usage,
};
pub use attempt_file::{AttemptFile, AttemptFileClass};
pub use canonical::{CanonicalBytes, CanonicalError, CanonicalV1, FileDigest, record_digest};
pub use finding::Finding;
pub use json::{DecodeError, EncodeError, Limits, RecordHeader, decode, decode_v1, encode};
pub use jsonc::{JsoncDocument, JsoncToken, JsoncTokenKind, decode_jsonc};
pub use path_wire::{PathWire, PathWireError};
pub use source::{SourceDigest, SourceDigestError, SourceInput, source_digest};
pub use trace::Trace;
pub use yaml::{
    Span as YamlSpan, SpannedYaml, YamlError, YamlErrorKind, YamlValue, parse_frontmatter,
    parse_sidecar,
};

pub mod inbox;
pub mod workflow;

pub use install::{
    AbsolutePath, Harness, InstallReceipt, OwnedEntry, OwnedValue, ReleaseFile, ReleaseManifest,
    ResolvedHarnessTarget, VersionInfo,
};

mod command_result;
pub use command_result::{
    CommandError, CommandResult, HelpArgument, HelpCatalogue, HelpCommand, HelpGroup, Outcome,
};

mod resolved_route;
pub use resolved_route::{
    ResolvedRoute, RouteAlternative, RouteAlternatives, RouteConfidentiality, RouteEligibility,
    RouteExclusion, RouteExclusionReason, RouteReadiness, RouteSelectionCandidate,
    RouteSelectionReceipt, RouteSelectionRedraw, TooManyAlternatives,
};

pub mod result_capture;
pub mod run;
