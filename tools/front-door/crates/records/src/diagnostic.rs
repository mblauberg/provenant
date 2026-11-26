//! Safe, bounded diagnostic facts shared by command, graph and transport edges.
pub use crate::Layer;
use crate::Precedence;
use serde::{Deserialize, Serialize};
use std::{fmt, path::Path};

/// The one rendering of a layer in warnings, receipts and explanations. Family
/// source enums no longer carry their own `as_str` or `description`.
/// Spec: docs/specs/state.md#every-setting-reports-its-layer
impl fmt::Display for Layer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.description())
    }
}

/// Maximum UTF-8 bytes in each projected field, after terminal escaping.
pub const MAX_DIAGNOSTIC_BYTES: usize = 1024;

/// The hard boundary protected by a typed refusal, if any.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoundaryClass {
    #[default]
    None,
    LaneGit,
    Credentials,
    Integrity,
    Authority,
}

/// Closed preparation failures; paths and provider output never enter receipts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanFailure {
    InvalidRoots,
    UnconfinedExtensions,
    ProtectedOverlap,
    InvalidPath,
    CredentialRoot,
    AnchorMismatch,
    BroadRoot,
    Permission,
    ReadOnly,
    SourceHead,
    Home,
}

impl PlanFailure {
    pub fn detail(self) -> &'static str {
        match self {
            Self::InvalidRoots => "confinement plan roots are invalid or overlapping",
            Self::UnconfinedExtensions => {
                "read-only confinement requires clean configuration and denied extensions"
            }
            Self::ProtectedOverlap => "confinement permission overlaps a protected path",
            Self::InvalidPath => "confinement permission path is invalid",
            Self::CredentialRoot => "confinement permission would expose a credential root",
            Self::AnchorMismatch => "confinement path anchor is missing or changed",
            Self::BroadRoot => "confinement permission root is too broad",
            Self::Permission => "confinement permission validation refused the launch",
            Self::ReadOnly => "read-only source plan validation refused the launch",
            Self::SourceHead => "source HEAD validation refused the launch",
            Self::Home => "provider home plan validation refused the launch",
        }
    }
}

/// Closed confinement refusal facts. No paths, provider output or OS text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfinementGuard {
    CacheSweepDepthExceeded,
    CacheSweepFailed,
    ConfinementFilesystemFailed,
    CredentialRootRefused,
    GitControlEntriesExceeded,
    GitControlHardLinked,
    GitHeadReadFailed,
    GitWriteAuthorityMissing,
    GrantLimitExceeded,
    InvalidPermissionPath,
    InvalidPlanRoots,
    InvalidPrivateHomeRecipe,
    InvalidProfileFixture,
    InvalidReadOnlyPolicy,
    NestedConfinementUnsupported,
    PathAnchorChanged,
    PathNotUtf8,
    PermissionNotRecorded,
    PermissionRootTooBroad,
    PinnedSourceRequiresGit,
    PrivateHomeConflict,
    PrivateHomeDurabilityUnconfirmed,
    PrivateHomeFilesystemFailed,
    PrivateHomeLimitExceeded,
    PrivateHomeReadFailed,
    PrivateHomeStateUnsupported,
    ProcessPolicyMissing,
    ProfileLimitExceeded,
    ProfileTransportFailed,
    ProjectBindingDirectoryUnsafe,
    ProjectBindingMismatch,
    ProjectBindingMissing,
    ProtectedPathOverlap,
    ReadOnlyPreparationFailed,
    ReadOnlyRunRefused,
    RepositoryAuthorityRevalidationFailed,
    SandboxBackendUnavailable,
    SandboxExecutableMissing,
    SandboxExecutableUntrusted,
    SandboxExecutionFailed,
    SandboxProbeCancelled,
    SandboxProbeFailed,
    SourceHeadInvalid,
    SourceHeadMissing,
    SourceHeadMoved,
    UnconfinedExtensions,
    UnsafePrivateHomeEntry,
    UnsupportedSandboxPlatform,
}

impl ConfinementGuard {
    pub fn project(self, diagnostic: Diagnostic) -> Diagnostic {
        // Unit variants serialise to their stable snake_case guard code.
        let Ok(serde_json::Value::String(code)) = serde_json::to_value(self) else {
            return diagnostic;
        };
        diagnostic
            .detail(format!("confinement plan: {}", code.replace('_', " ")))
            .next("repair the requested grants or host confinement setup before retrying")
    }
}

/// Closed continuation facts, safe to retain in public settlement reasons.
/// Values, paths and provider output never enter this projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinuationGuard {
    pub rule: ContinuationRule,
    pub field: ContinuationField,
    pub issue: ContinuationIssue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationRule {
    Request,
    RetainedRoute,
    Contract,
    NativeSession,
}

impl ContinuationRule {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Request => "CONTINUATION_CONTINUATION_REQUEST_AUTHORITY",
            Self::RetainedRoute => "CONTINUATION_CONTINUATION_FOR_AUTHORITY",
            Self::Contract => "CONTRACT_CONTINUATION_VALIDATE_AUTHORITY",
            Self::NativeSession => "PREPARATION_PREPARE_CANDIDATES_AUTHORITY",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationField {
    AppliedRoute,
    Provider,
    Model,
    Account,
    Effort,
    Session,
    Host,
    GrantDigest,
    NativeHome,
    SessionState,
    Rollout,
}

impl ContinuationField {
    pub const fn name(self) -> &'static str {
        match self {
            Self::AppliedRoute => "applied_route",
            Self::Provider => "provider",
            Self::Model => "model",
            Self::Account => "account",
            Self::Effort => "effort",
            Self::Session => "session",
            Self::Host => "host",
            Self::GrantDigest => "grant_digest",
            Self::NativeHome => "native_home",
            Self::SessionState => "session_state",
            Self::Rollout => "rollout",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationIssue {
    Missing,
    Invalid,
    Mismatch,
    Unavailable,
}

impl std::fmt::Display for ContinuationGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let issue = match self.issue {
            ContinuationIssue::Missing => "missing",
            ContinuationIssue::Invalid => "invalid",
            ContinuationIssue::Mismatch => "mismatched",
            ContinuationIssue::Unavailable => "unavailable",
        };
        write!(
            f,
            "{}: continuation field `{}` is {issue}",
            self.rule.code(),
            self.field.name()
        )
    }
}

impl ContinuationGuard {
    pub fn project(self, diagnostic: Diagnostic) -> Diagnostic {
        diagnostic
            .detail(self.to_string())
            .next("inspect the predecessor's applied route and retained session before retrying")
    }
}

/// The layers a pressure threshold accepts, lowest first. A shipped default
/// is beaten by a value the host configured. Admission *classes* use
/// `ADMISSION_PRECEDENCE` instead.
/// Spec: docs/specs/state.md#every-setting-reports-its-layer
pub const PRESSURE_PRECEDENCE: Precedence = Precedence(&[Layer::Default, Layer::Host]);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PressureLevel {
    Normal,
    Warning,
    Critical,
}
impl PressureLevel {
    pub fn name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PressureThreshold {
    Warning,
    Critical,
    Never,
}
impl PressureThreshold {
    pub fn name(self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::Critical => "critical",
            Self::Never => "never",
        }
    }
}

/// The ledger limit that actually prevented a lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneLimit {
    HostWriters,
    ProjectWriters,
    HostTotal,
    ProjectTotal,
}

impl LaneLimit {
    pub fn setting(self) -> &'static str {
        match self {
            Self::HostWriters => "domains.fabric.lanes.max_writer_lanes",
            Self::ProjectWriters => "domains.fabric.lanes.max_project_writer_lanes",
            Self::HostTotal => "domains.fabric.lanes.max_lanes",
            Self::ProjectTotal => "domains.fabric.lanes.max_project_lanes",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::HostWriters => "host writer lanes",
            Self::ProjectWriters => "project writer lanes",
            Self::HostTotal => "host lanes",
            Self::ProjectTotal => "project lanes",
        }
    }
}

/// The configuration family that supplied a pacing reserve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PacingReserveSetting {
    AdapterFiveHour,
    AdapterSevenDay,
    Account,
    Free,
}
/// Source of the reserve selected by routing. Adapter overrides need the
/// configuration loader's per-field origin before they are projected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PacingSettingSource {
    Configuration,
    ShippedDefault,
    Unresolved,
}
impl PacingSettingSource {
    pub fn description(self) -> &'static str {
        match self {
            Self::Configuration => "host configuration",
            Self::ShippedDefault => "shipped default",
            Self::Unresolved => "origin unavailable",
        }
    }
}

/// Details identifying the account, window and configuration behind a pacing hold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PacingHold {
    pub account: String,
    pub window: String,
    pub setting_path: String,
}

/// Origin of a lane-port block count in the handed-off plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortSettingSource {
    Configuration,
    ShippedDefault,
    RecordedPlan,
}
impl PortSettingSource {
    pub fn description(self) -> &'static str {
        match self {
            Self::Configuration => "host configuration",
            Self::ShippedDefault => "shipped default",
            Self::RecordedPlan => "recorded plan",
        }
    }
}

/// Configurable budgets for copying retained provider state. These never alter
/// credential, state-root, mount or worktree confinement checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationCopyLimit {
    Bytes,
    Entries,
    Depth,
}
impl ContinuationCopyLimit {
    pub fn setting(self) -> &'static str {
        match self {
            Self::Bytes => "domains.run.continuation_copy_limit_bytes",
            Self::Entries => "domains.run.continuation_copy_entries",
            Self::Depth => "domains.run.continuation_copy_depth",
        }
    }
}

/// A confinement boundary's named rule and display-sanitised refused path.
/// Boxed inside [`ResourceFact`] so the error variants that carry a resource
/// fact stay small.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfinementRule {
    pub rule: String,
    pub path: String,
}

/// Closed resource facts safe to project across host boundaries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResourceFact {
    Disk {
        free: u64,
        floor: u64,
        origin: Layer,
    },
    ArgvPromptLimit {
        limit: u64,
        origin: Layer,
    },
    ContinuationCopy {
        limit: u64,
        setting: ContinuationCopyLimit,
        origin: Layer,
        available_bytes: Option<u64>,
    },
    WriteGrantLimit {
        limit: u64,
        origin: Layer,
    },
    GrantLimit {
        limit: u64,
        origin: Layer,
    },
    ProfileLimit {
        bytes: u64,
        limit: u64,
        origin: Layer,
    },
    DependencyPreparation {
        configured: bool,
    },
    LanePorts {
        blocks: u32,
        origin: PortSettingSource,
    },
    PacingReserve {
        percent: u8,
        setting: PacingReserveSetting,
        origin: PacingSettingSource,
        #[serde(flatten)]
        hold: Box<PacingHold>,
    },
    MemoryPressure {
        level: PressureLevel,
        threshold: PressureThreshold,
        origin: Layer,
    },
    LaneCap {
        active: u64,
        limit: u64,
        lane_limit: LaneLimit,
        origin: Layer,
    },
    Processes {
        used: u64,
        limit: u64,
        reserved: u64,
    },
    /// A confinement boundary that named the rule and path it refused. The
    /// rule is a stable identifier; the path is display-sanitised by `project`.
    ConfinementRule(Box<ConfinementRule>),
}

impl ResourceFact {
    pub fn project(&self, diagnostic: Diagnostic) -> Diagnostic {
        match self {
            Self::Disk { free, floor, origin } => diagnostic
                .detail(format!("disk space: {free} bytes available; gc.critical_free is {floor} bytes (source: {})", origin.description()))
                .next("free disk space above gc.critical_free before retrying"),
            Self::ArgvPromptLimit { limit, origin } => diagnostic
                .detail(format!("argv prompt exceeds domains.run.argv_prompt_limit_bytes ({limit} bytes; source: {})", origin.description()))
                .next("shorten the prompt or adjust domains.run.argv_prompt_limit_bytes"),
            Self::ContinuationCopy { limit, setting, origin, available_bytes } => {
                let key = setting.setting();
                let space = available_bytes.map_or_else(String::new, |bytes| format!("; {bytes} disk bytes available"));
                diagnostic
                    .detail(format!("retained provider state exceeds its copy budget; {key} is {limit} (source: {}){space}", origin.description()))
                    .next(format!("reduce retained provider state, free disk space, or adjust {key} before retrying"))
            }
            Self::WriteGrantLimit { limit, origin } => diagnostic
                .detail(format!("encoded write grants exceed domains.run.write_grant_limit_bytes ({limit} bytes; source: {})", origin.description()))
                .next("reduce the write grants or adjust domains.run.write_grant_limit_bytes"),
            Self::GrantLimit { limit, origin } => diagnostic
                .detail(format!("grants exceed domains.run.grant_limit ({limit}; source: {})", origin.description()))
                .next("reduce granted roots or adjust domains.run.grant_limit"),
            Self::ProfileLimit { bytes, limit, origin } => diagnostic
                .detail(format!("sandbox profile is {bytes} bytes; domains.run.profile_limit_bytes is {limit} bytes (source: {})", origin.description()))
                .next("reduce granted roots or adjust domains.run.profile_limit_bytes"),
            Self::DependencyPreparation { configured } => diagnostic
                .detail(format!("dependency preparation failed; lanes.prepare_deps=true (source: {})", if *configured { "sealed profile" } else { "shipped default" }))
                .next("repair dependency preparation or set lanes.prepare_deps=false"),
            Self::LanePorts { blocks, origin } => diagnostic
                .detail(format!("no free lane port block; domains.fabric.lanes.lane_port_blocks is {blocks} (source: {})", origin.description()))
                .next("wait for a lane port block or raise or disable domains.fabric.lanes.lane_port_blocks"),
            Self::PacingReserve { percent, origin, hold, .. } => diagnostic
                .detail(format!("selected account is held by a {percent}% pacing reserve; account={}; window={}; {} (source: {})", safe_text(&hold.account), safe_text(&hold.window), safe_text(&hold.setting_path), origin.description()))
                .next(format!("wait for account {} and its {} pacing window or adjust {}", safe_text(&hold.account), safe_text(&hold.window), safe_text(&hold.setting_path))),
            Self::MemoryPressure { level, threshold, origin } => diagnostic
                .detail(format!("host memory pressure is {}; domains.fabric.lanes.memory_pressure_threshold is {} (source: {})", level.name(), threshold.name(), origin.description()))
                .next("reduce host memory pressure before retrying"),
            Self::LaneCap { active, limit, lane_limit, origin } => {
                let description = lane_limit.description();
                let setting = lane_limit.setting();
                diagnostic
                    .detail(format!("{active} of {limit} {description} are active; {setting} (source: {})", origin.description()))
                    .next(format!("wait for an active lane to finish or adjust {setting}"))
            }
            Self::Processes { used, limit, reserved } => diagnostic
                .detail(format!("host user process count is {used} of the operating system RLIMIT_NPROC soft limit {limit}; shipped reserved headroom is {reserved}"))
                .next("reduce host process use or raise the per-user process limit before retrying"),
            Self::ConfinementRule(rule) => diagnostic
                .detail(format!(
                    "confinement rule {} refused {}",
                    safe_text(&rule.rule),
                    safe_text(&rule.path)
                ))
                .next("inspect the named confinement rule and path before retrying"),
        }
    }
}

/// Provider failure facts that can be shown without copying captured text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderFailureFact {
    Quota { free_pool: bool },
    RateLimit,
    ModelUnavailable,
    Capacity,
    Denied,
    AuthRequired,
    EarlyStop,
    CodexIdentity,
    OpenCodeIdentity,
}

impl ProviderFailureFact {
    pub fn reason(self) -> &'static str {
        match self {
            Self::Quota { free_pool: true } => {
                "provider quota exhausted; free tier exhausted; use a paid route or wait for its allowance reset"
            }
            Self::Quota { free_pool: false } => {
                "provider quota exhausted; wait for the allowance to reset or select an eligible route"
            }
            Self::RateLimit => {
                "provider rate limited; wait for the allowance to reset before retrying"
            }
            Self::ModelUnavailable => {
                "provider model unavailable; check the selected model and choose an eligible route"
            }
            Self::Capacity => {
                "provider capacity: the selected model is overloaded; wait or choose an eligible route"
            }
            Self::Denied => {
                "provider denied an action; check account access and inspect the result"
            }
            Self::AuthRequired => {
                "provider requires login; sign in to the selected account and run `provenant quota tick`; if login cannot be checked, retry after readiness expires"
            }
            Self::EarlyStop => "provider stopped early; inspect the result and resume if needed",
            Self::CodexIdentity => {
                "Codex rollout identity could not be verified; inspect the local session evidence"
            }
            Self::OpenCodeIdentity => {
                "OpenCode session identity could not be verified; inspect the local session evidence"
            }
        }
    }
}

/// I/O facts whose field and reason are safe to disclose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IoFact {
    InputUnreadable,
}

impl IoFact {
    pub fn project(self, diagnostic: Diagnostic) -> Diagnostic {
        match self {
            Self::InputUnreadable => diagnostic
                .detail("input could not be read")
                .next("inspect the input source and retry"),
        }
    }
}

/// An explicitly recognised delivery command and its enforced output limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryCommand {
    GitWorktreeList,
    GitLog,
    GitStatus,
    GitFetch,
    GitPush,
    GitRevParse,
    GhPr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryLimit {
    pub stage: crate::run::WriterDeliveryStage,
    pub command: DeliveryCommand,
    pub limit: u64,
}

/// The path and source text of a local filesystem failure. Boxed so the fact,
/// and every error that carries one, stays small.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesystemDetail {
    pub path: Option<String>,
    pub source: Option<String>,
}

/// Typed facts retained in-process until an edge chooses its envelope. They are
/// never serialized as raw diagnostic strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagnosticFact {
    CredentialGrant {
        access: &'static str,
    },
    DeliveryLimit(DeliveryLimit),
    DeliveryStage(crate::run::WriterDeliveryStage),
    GenerationConflict(u64),
    WorktreeUnregistered,
    Filesystem {
        operation: &'static str,
        /// Where the operation failed and the source error's text, when known.
        /// `Diagnostic::fact` is serde-skipped, so this stays in-process only.
        detail: Option<Box<FilesystemDetail>>,
        /// The source error's fixed text, when it names no path or value.
        cause: Option<&'static str>,
    },
    DiskPressure {
        available: u64,
        floor: u64,
        reclaimed: u64,
        origin: Layer,
    },
    DiskUnobserved,
    PromptLimit {
        limit: u64,
    },
    Resource(ResourceFact),
}
impl DeliveryLimit {
    pub fn new(
        stage: crate::run::WriterDeliveryStage,
        tool: &str,
        command: &str,
        limit: usize,
    ) -> Option<Self> {
        let command = match (tool, command) {
            ("git", "worktree list") => DeliveryCommand::GitWorktreeList,
            ("git", "log") => DeliveryCommand::GitLog,
            ("git", "status") => DeliveryCommand::GitStatus,
            ("git", "fetch") => DeliveryCommand::GitFetch,
            ("git", "push") => DeliveryCommand::GitPush,
            ("git", "rev-parse") => DeliveryCommand::GitRevParse,
            ("gh", "pr") => DeliveryCommand::GhPr,
            _ => return None,
        };
        Some(Self {
            stage,
            command,
            limit: u64::try_from(limit).ok()?,
        })
    }
    pub fn detail(&self) -> String {
        let command = match self.command {
            DeliveryCommand::GitWorktreeList => "git worktree list",
            DeliveryCommand::GitLog => "git log",
            DeliveryCommand::GitStatus => "git status",
            DeliveryCommand::GitFetch => "git fetch",
            DeliveryCommand::GitPush => "git push",
            DeliveryCommand::GitRevParse => "git rev-parse",
            DeliveryCommand::GhPr => "gh pr",
        };
        format!(
            "host writer delivery failed at {:?}: {command} output exceeded the {}-byte host limit",
            self.stage, self.limit
        )
    }
}

/// Safe retained-writer recovery facts projected by the run owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetainedCustody {
    pub scope: crate::custody::Scope,
    pub fence: Option<String>,
    pub lane: crate::WorkKey,
    pub reconcile: String,
    pub abandon: String,
}

/// Only explicitly selected safe facts belong in these fields; source errors,
/// captured output, prompts and credentials are not safe facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custody: Option<RetainedCustody>,
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    #[serde(skip)]
    pub fact: Option<DiagnosticFact>,
}

/// Projection fields beside an envelope's established code and message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticFields {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}
impl From<&Diagnostic> for DiagnosticFields {
    fn from(value: &Diagnostic) -> Self {
        Self {
            category: value.category.clone(),
            detail: value.detail.clone(),
            next: value.next.clone(),
        }
    }
}

impl Diagnostic {
    pub fn new(code: impl AsRef<str>) -> Self {
        Self {
            extra: Default::default(),
            custody: None,
            code: safe_text(code.as_ref()),
            category: None,
            detail: None,
            next: None,
            fact: None,
        }
    }
    pub fn category(mut self, value: impl AsRef<str>) -> Self {
        self.category = Some(safe_text(value.as_ref()));
        self
    }
    pub fn detail(mut self, value: impl AsRef<str>) -> Self {
        self.detail = Some(safe_text(value.as_ref()));
        self
    }
    pub fn next(mut self, value: impl AsRef<str>) -> Self {
        self.next = Some(safe_text(value.as_ref()));
        self
    }
    pub fn fact(mut self, fact: DiagnosticFact) -> Self {
        self.fact = Some(fact);
        self
    }
}

/// Characters that could hide, reorder or break text on a terminal: Cc
/// controls plus the Cf, Zl and Zp categories and emoji tag characters
/// (U+E0001 and U+E0020..U+E007F), not the variation-selector ranges.
pub fn unsafe_display_character(character: char) -> bool {
    character.is_control()
        || matches!(character,
            '\u{ad}' | '\u{600}'..='\u{605}' | '\u{61c}' | '\u{6dd}' | '\u{70f}' | '\u{180e}'
            | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{206f}'
            | '\u{feff}' | '\u{fff9}'..='\u{fffb}' | '\u{e0001}' | '\u{e0020}'..='\u{e007f}')
}

/// The one display-sanitisation core: escape characters that could hide,
/// reorder or break text, then truncate on a UTF-8 boundary after escaping.
/// `preserve_lines` keeps `\n` and `\t` for multi-line operator causes; the
/// single-line form escapes them instead.
fn safe_display_text(value: &str, preserve_lines: bool, max_bytes: usize) -> String {
    let mut output = String::new();
    for character in value.chars() {
        let part = if preserve_lines && matches!(character, '\n' | '\t') {
            character.to_string()
        } else if unsafe_display_character(character) {
            character.escape_default().to_string()
        } else {
            character.to_string()
        };
        if output.len() + part.len() > max_bytes {
            break;
        }
        output.push_str(&part);
    }
    output
}

/// Escape terminal controls and truncate at a UTF-8 boundary after escaping.
pub fn safe_text(value: &str) -> String {
    safe_display_text(value, false, MAX_DIAGNOSTIC_BYTES)
}

/// Multi-line form for an operator cause: keeps line breaks and tabs while
/// escaping terminal controls, bounded at a UTF-8 boundary after escaping.
pub fn safe_multiline_text(value: &str, max_bytes: usize) -> String {
    safe_display_text(value, true, max_bytes)
}

/// Escape terminal controls without applying a bound, for callers that bound
/// the text afterwards and need to keep its tail (where a failure usually is).
/// Uses the same one display-sanitisation core as [`safe_text`].
pub fn escape_display_text(value: &str) -> String {
    safe_display_text(value, true, usize::MAX)
}

/// Destination is selected by the audience, not the transport. Local includes
/// authorised execution context; Remote denotes disclosure outside that authority
/// and needs a publication projection.
#[derive(Debug, Clone, Copy)]
pub enum Destination {
    Local,
    Remote,
}

/// A publication implementation must project for the selected audience before
/// sending bytes. Failure withholds that publication; it does not alter input.
pub trait PublicationProjection {
    type Error;
    fn project(&self, destination: Destination, raw: &[u8]) -> Result<Vec<u8>, Self::Error>;
}

/// Quote a path as one shell argument after applying destination disclosure.
pub fn shell_path(path: &Path, destination: Destination) -> Option<String> {
    if !matches!(destination, Destination::Local)
        || path.to_str()?.chars().any(unsafe_display_character)
    {
        return None;
    }
    let path = path.to_str()?;
    Some(format!("'{}'", path.replace('\'', "'\\''")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec: docs/specs/state.md#every-setting-reports-its-layer
    #[test]
    fn setting_layers_preserve_the_shipped_default_then_host_order() {
        use crate::{Source, Sourced};
        fn candidate(layer: Layer) -> Sourced<&'static str> {
            Sourced {
                value: layer.as_str(),
                source: Source {
                    layer,
                    origin: None,
                },
            }
        }
        let merge = PRESSURE_PRECEDENCE.merge([candidate(Layer::Default), candidate(Layer::Host)]);
        assert_eq!(
            merge.winner.as_ref().map(|winner| winner.value),
            Some("host")
        );
        assert_eq!(merge.shadowed.len(), 1);
        assert_eq!(merge.shadowed[0].value, "default");
    }

    fn every_route_exclusion_is_covered(cause: &crate::RouteExclusionReason) {
        use crate::RouteExclusionReason as Cause;
        match cause {
            Cause::SetupFailed
            | Cause::AccountPinned
            | Cause::AdmissionRefused
            | Cause::Disabled
            | Cause::MissingExecutable
            | Cause::ProviderUpdating
            | Cause::UnknownAvailability
            | Cause::StaleAvailability
            | Cause::UnknownAccount
            | Cause::UnresolvedAccount
            | Cause::DisclosureDenied
            | Cause::ConfinementUnavailable
            | Cause::UnsupportedEffort
            | Cause::Exhausted
            | Cause::PacingReserve
            | Cause::UnknownPartialHeadroom
            | Cause::AuthRequired
            | Cause::LoginExpired
            | Cause::UnverifiedModel
            | Cause::AccountMismatch
            | Cause::FreeTierRefused
            | Cause::ProviderRefused
            | Cause::InvalidAccountEvidence
            | Cause::AccountDoesNotOffer => {}
        }
    }

    #[test]
    fn route_exclusions_have_bounded_causes_and_actions() {
        use crate::RouteExclusionReason as Cause;
        for cause in Cause::ALL {
            every_route_exclusion_is_covered(cause);
            let diagnostic = cause.diagnostic("REFUSED");
            assert_eq!(diagnostic.code, "REFUSED");
            for text in [diagnostic.detail.unwrap(), diagnostic.next.unwrap()] {
                assert!(!text.is_empty());
                assert!(text.len() <= MAX_DIAGNOSTIC_BYTES);
                assert!(!text.chars().any(char::is_control));
            }
        }
    }

    #[test]
    // Spec: docs/specs/fabric.md#session-ready-steering
    fn skipped_route_projects_typed_readiness_without_source_text() {
        use crate::{
            RouteExclusionReason,
            run::{SkipKind, SkippedRoute},
        };
        let mut route = SkippedRoute {
            skipped_accounts: Vec::new(),
            extra: Default::default(),
            route: "fixture/model".into(),
            kind: SkipKind::Unavailable,
            cause: None,
            detail: "TOKEN_SENTINEL /private/source \u{1b}".into(),
            ..Default::default()
        };
        let initial = serde_json::to_string(&route.diagnostic()).unwrap();
        assert!(!initial.contains("TOKEN_SENTINEL"));
        assert!(!initial.contains("/private/source"));
        route.set_cause(RouteExclusionReason::StaleAvailability);
        assert!(route.detail.contains("stale"));
        assert!(route.detail.contains("provenant quota tick"));
        assert!(!route.detail.contains("TOKEN_SENTINEL"));
        assert!(route.detail.len() <= 2 * MAX_DIAGNOSTIC_BYTES + 2);
        let mut malformed = serde_json::to_value(&route).unwrap();
        malformed["guard_resource"] =
            serde_json::json!({"kind": "TOKEN_SENTINEL", "detail": "/private/source"});
        assert!(serde_json::from_value::<SkippedRoute>(malformed).is_err());
        route.confinement_guard = Some(ConfinementGuard::ProtectedPathOverlap);
        route.set_cause(RouteExclusionReason::ConfinementUnavailable);
        let projected = route.diagnostic();
        assert!(projected.detail.unwrap().contains("protected path overlap"));
        assert!(!route.detail.contains("TOKEN_SENTINEL"));
        let mut malformed = serde_json::to_value(&route).unwrap();
        malformed["confinement_guard"] = serde_json::json!("TOKEN_SENTINEL /private/source");
        assert!(serde_json::from_value::<SkippedRoute>(malformed).is_err());
        let guard = ContinuationGuard {
            rule: ContinuationRule::RetainedRoute,
            field: ContinuationField::Effort,
            issue: ContinuationIssue::Mismatch,
        };
        route.set_continuation_guard(guard);
        let encoded = serde_json::to_value(&route).unwrap();
        let restored: SkippedRoute = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(restored.continuation_guard, Some(guard));
        let reason = restored.describe();
        assert!(reason.contains(guard.rule.code()));
        assert!(reason.contains(guard.field.name()));
        assert!(!reason.contains("TOKEN_SENTINEL"));
        for field in ["rule", "field", "issue"] {
            let mut malformed = encoded.clone();
            malformed["continuation_guard"][field] =
                serde_json::json!("TOKEN_SENTINEL /private/source");
            assert!(serde_json::from_value::<SkippedRoute>(malformed).is_err());
        }
    }

    #[test]
    fn provider_failure_facts_have_bounded_safe_actions() {
        for fact in [
            ProviderFailureFact::Quota { free_pool: false },
            ProviderFailureFact::Quota { free_pool: true },
            ProviderFailureFact::RateLimit,
            ProviderFailureFact::ModelUnavailable,
            ProviderFailureFact::Capacity,
            ProviderFailureFact::Denied,
            ProviderFailureFact::AuthRequired,
            ProviderFailureFact::EarlyStop,
            ProviderFailureFact::CodexIdentity,
            ProviderFailureFact::OpenCodeIdentity,
        ] {
            let reason = fact.reason();
            assert!(reason.len() <= MAX_DIAGNOSTIC_BYTES);
            assert!(!reason.chars().any(char::is_control));
            assert!(reason.contains(';'));
        }
    }

    #[test]
    fn projection_has_code_and_optional_bounded_safe_fields() {
        let empty = serde_json::to_value(Diagnostic::new("INVALID")).unwrap();
        assert_eq!(empty, serde_json::json!({"code":"INVALID"}));
        let input = format!("secret-free\u{1b}[31m{}", "é".repeat(1000));
        let detail = Diagnostic::new("INVALID").detail(input).next("inspect");
        let text = detail.detail.unwrap();
        assert!(text.len() <= MAX_DIAGNOSTIC_BYTES);
        assert!(!text.chars().any(char::is_control));
        assert!(text.contains("\\u{1b}"));
        let bidi = safe_text("before\u{202e}after");
        assert!(bidi.contains("\\u{202e}"));
        assert!(!bidi.contains('\u{202e}'));
    }

    #[test]
    fn safe_multiline_text_preserves_lines_but_escapes_controls_and_bounds() {
        let input = "first\u{1b}[31m\n\tsecond\u{202e}third\nfourth";
        let safe = safe_multiline_text(input, MAX_DIAGNOSTIC_BYTES);
        assert!(safe.starts_with("first\\u{1b}[31m\n\tsecond\\u{202e}third\n"));
        assert!(safe.contains("fourth"));
        assert!(!safe.chars().any(|c| c == '\u{1b}' || c == '\u{202e}'));
        assert!(safe.len() <= MAX_DIAGNOSTIC_BYTES);
        // The single-line core escapes the same characters instead.
        assert!(!safe_text("a\nb").contains('\n'));
    }

    #[test]
    fn shell_paths_are_local_and_safely_quoted() {
        assert_eq!(
            shell_path(Path::new("/repo/a'b"), Destination::Local).as_deref(),
            Some("'/repo/a'\\''b'")
        );
        let bidi_path = Path::new("/repo/a\u{202e}b");
        assert_eq!(shell_path(bidi_path, Destination::Local), None);
    }

    #[test]
    fn delivery_limit_accepts_only_known_commands() {
        use crate::run::WriterDeliveryStage;
        let fact =
            DeliveryLimit::new(WriterDeliveryStage::Prepare, "git", "worktree list", 4096).unwrap();
        assert!(fact.detail().contains("git worktree list"));
        assert!(fact.detail().contains("4096-byte"));
        assert!(
            DeliveryLimit::new(WriterDeliveryStage::Prepare, "git", "token-sentinel", 4096)
                .is_none()
        );
        let projection = Diagnostic::new("IO").fact(DiagnosticFact::DeliveryLimit(fact));
        assert_eq!(
            serde_json::to_value(projection).unwrap(),
            serde_json::json!({"code": "IO"})
        );
    }
}
