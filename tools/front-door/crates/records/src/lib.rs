//! Bounded record codecs, canonical digest bytes, and shared serialized facts.
#![forbid(unsafe_code)]

mod adapters;
pub use adapters::{
    ADAPTER_SPECS, AccountIdentityProbe, AccountModelPlan, AccountQuota, AdapterSpec,
    COMMAND_OUTPUT_TAIL_BYTES, CapabilitySupport, ClientEnvMarkers, GithubAuth, HarnessInstall,
    HookAction, HookEvent, HookProtocol, HookTimeoutUnit, INSTRUCTION_ASSET, LaunchCapabilities,
    LaunchField, LaunchRequest, LoginObservation, LoginReprobe, ModelListingFormat,
    ModelObservation, NamedEntry, NativeAuthWarning, QuotaProbeKind, SetupRoot, SkillLocation,
    StreamContract, UnsupportedLaunch, UpdateCommand, normalise_model_id,
};

/// The instruction content bound into this product build and projected into
/// each isolated harness configuration.
pub const INSTRUCTION_CONTENT: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../GLOBAL-AGENTS.md"
));

mod artifact_ref;
mod attempt_fact;
mod attempt_file;
mod canonical;
pub mod control;
pub mod custody;
pub mod diagnostic;
mod extra_de;
mod finding;
pub mod handoff;
mod input;
mod install;
mod json;
mod jsonc;
pub mod migrate;
mod path_wire;
pub mod policy;
pub mod port_receipt;
pub mod ports;
pub mod provenance;
mod source;
pub mod telemetry_context;
mod trace;
pub mod usage;
pub mod usage_identity;
pub mod warning;

pub use artifact_ref::ArtifactRef;
pub use attempt_fact::{
    AttemptFact, AttemptOutcome, OutputRef, ReviewerFact, ReviewerProvenance, RouteFact,
    TranscriptRef, Usage,
};
pub use attempt_file::{AttemptFile, AttemptFileClass};
pub use canonical::{
    CanonicalBytes, CanonicalError, CanonicalV1, FileDigest, record_digest, stored_record_digest,
};
pub use finding::Finding;
pub use input::{InputWarning, decode_input};
pub use json::{
    DecodeError, EncodeError, Limits, RecordHeader, decode, decode_stored_record,
    decode_stored_record_v1, decode_v1, encode,
};
pub use jsonc::{JsoncDocument, JsoncToken, JsoncTokenKind, decode_jsonc};
pub use migrate::{Migration, MigrationError, MigrationReport};
pub use path_wire::{PathWire, PathWireError, StructuredPathWire};
pub use port_receipt::{
    DEFAULT_PORT_COMMAND_RECEIPT_AGE_DAYS, PORT_COMMAND_RECEIPT_SCHEMA_VERSION, PortCommandBackend,
    PortCommandDisposition, PortCommandReceipt, PortReceiptLifetime,
};
pub use ports::{CommandExitCodes, ExitDisposition, PORT_PRECEDENCE, PortCommand, port_layer_rank};
pub use provenance::{Layer, Merge, Precedence, Source, Sourced};
pub use source::{SourceDigest, SourceDigestError, SourceInput, source_digest};
pub use trace::Trace;
pub use usage::{Cost, CostOrigin, Tokens, UsageScope, UsageSemantics, UsageSource};
pub use usage_identity::{
    AccountIdentity, Identity, IdentityEpoch, IdentityEvidenceSource, UnknownIdentityReason,
};
pub use warning::{Dedup, Sink, SinkGuard, Warning, WarningCode, capture, install, warn};

pub mod inbox;
pub mod landing;
pub mod workflow;

pub use install::{
    AbsolutePath, Harness, INSTALL_RECEIPT_SCHEMA_VERSION, InstallReceipt, LIVE_RECORD_FORMAT,
    OwnedEntry, OwnedValue, ReleaseFile, ReleaseManifest, ResolvedHarnessTarget,
    STATE_FORMAT_VERSION, VersionInfo,
};

pub mod command_result;
pub mod wire;
pub use command_result::{
    COMMAND_RESULT_MAX_BYTES, CommandError, CommandResult, HelpArgument, HelpCatalogue,
    HelpCommand, HelpErrorCode, HelpGroup, Outcome,
};

mod resolved_route;
pub mod route_selection;
pub use resolved_route::{
    ResolvedRoute, RouteAlternative, RouteAlternatives, RouteConfidentiality, RouteEligibility,
    RouteExclusion, RouteExclusionReason, RouteReadiness, RouteSelectionCandidate,
    RouteSelectionReceipt, RouteSelectionRedraw, SkippedAccount, TooManyAlternatives,
};

pub mod lane_contract;
pub mod result_capture;
pub mod run;

// Format-2 records (#3078). Callers adopt them in later slices.
pub mod finding_decision;
pub mod graph;
pub mod limit;
pub mod pause;
pub mod stop;
pub mod work_key;
pub use finding_decision::{FindingDecision, FindingDisposition};
pub use limit::{
    BudgetAction, BudgetGrace, BudgetPolicy, CeilingError, LIMIT_PRECEDENCE, LaneLimits, Limit,
    LimitStrength, TokenBudgetUsage,
};
pub use pause::{Pause, PauseCause};
pub use stop::{Continuation, NextStep, OnStop, STOP_TABLE, Stop, StopCause, StopFacts, StopRule};
pub use work_key::WorkKey;
#[cfg(test)]
mod format2_tests;
pub use run::{
    LANE_ACCESS_PRECEDENCE, LaneAccess, LaneAccessReceipt, SANDBOX_PRECEDENCE, SandboxConstraint,
    UserSkillsMode,
};

/// Fields from newer writers, retained in canonical record bytes.
pub type ExtraFields = std::collections::BTreeMap<String, serde_json::Value>;
