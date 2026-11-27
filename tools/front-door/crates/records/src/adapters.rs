//! Closed adapter identities and shared facts. Mechanics stay in their modules.
use crate::{
    UsageScope, UsageSemantics,
    control::{ControlCapabilities, ControlSupport},
    run::NetworkAccess,
};
use serde::{Deserialize, Serialize};

/// One release-owned instruction asset shared by native harness files.
pub const INSTRUCTION_ASSET: &str = "instructions/AGENTS.md";

/// Latest command output retained by the guardian and adapter.
pub const COMMAND_OUTPUT_TAIL_BYTES: usize = 65_536;

// Export the implementation bindings from the same rows as the adapter facts.
// The callback is supplied by the composition crate, keeping mechanics there.
// Pass the dollar token through to generate the callback macro on stable Rust.
macro_rules! export_adapter_bindings {
    ($d:tt; $name:ident; [$($out:tt)*];) => {
        #[macro_export]
        macro_rules! $name {
            ($d callback:ident) => { $d callback! { $($out)* } };
        }
    };
    ($d:tt; $name:ident; [$($out:tt)*]; [$(#[$cfg:meta])*] [$module:ident : $order:literal] $($tail:tt)*) => {
        export_adapter_bindings!($d; $name; [$($out)* $(#[$cfg])* $module : $order,]; $($tail)*);
    };
    ($d:tt; $name:ident; [$($out:tt)*]; [$($cfg:tt)*] [] $($tail:tt)*) => {
        export_adapter_bindings!($d; $name; [$($out)*]; $($tail)*);
    };
}

// Define the wire identity, facts and implementation binding in one row.
// A binding names its implementation module and dispatch order; equal orders
// retain row order. Delegated entries need no implementation binding.
macro_rules! adapter_specs {
    ($d:tt; $( $(#[$cfg:meta])* $variant:ident [$($serde:meta),*] ($($module:ident : $order:literal)?) { $($fields:tt)* } ),* $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        #[non_exhaustive]
        pub enum Harness {
            $( $(#[$cfg])* $(#[$serde])* $variant, )*
        }
        impl Harness {
            pub fn spec(self) -> &'static AdapterSpec {
                &ADAPTER_SPECS[self as usize]
            }
        }
        pub static ADAPTER_SPECS: &[AdapterSpec] = &[
            $( $(#[$cfg])* AdapterSpec { id: Harness::$variant, $($fields)* }, )*
        ];
        export_adapter_bindings!($d; adapter_implementation_bindings; []; $( [$(#[$cfg])*] [$($module : $order)?] )*);
    };
}

/// Wire format of a provider CLI model listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelListingFormat {
    CodexJson,
    AgyText,
    OpencodeText,
    CursorText,
}

/// How an adapter's own executable is updated. The mechanics of resolving a
/// package-manager install stay in the update worker; this is only the declared
/// per-adapter choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateCommand {
    /// No self-update is known for this adapter.
    None,
    /// A subcommand of the adapter's own executable.
    Subcommand(&'static str),
    /// The executable is installed by Homebrew or by a global npm install.
    HomebrewOrNpm {
        formula: &'static str,
        module: &'static str,
        package: &'static str,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaProbeKind {
    None,
    HttpGet,
    Command,
    LocalLedger,
}

/// How a provider receives host GitHub authentication for an open-network launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GithubAuth {
    Environment,
    ShellEnvironment,
}

/// How preparation rechecks a recorded login failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginReprobe {
    None,
    Codex,
    Cursor,
    AccountIdentity,
    ExecutableOnly,
}

/// How an adapter reports the identity of a registered account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountIdentityProbe {
    None,
    KeychainLog,
    AuthJson,
    Vscdb,
    CliAuthStatus,
}

/// When a launch under host access warns about native provider authentication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAuthWarning {
    None,
    WhenMismatch,
    Always,
}

/// How availability records supply fresh login evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginObservation {
    None,
    AccountAvailability,
    Reprobe,
    ExecutableOnly,
}

/// Additional host root required by the adapter's setup directory creation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupRoot {
    None,
    Home,
    XdgDataHome,
}

/// How a model-less named entry is prepared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedEntry {
    None,
    Command,
    DelegatedAgent,
}

/// Evidence used to identify the model after a provider finishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnsweringModelSource {
    Stream,
    Rollout,
    SessionDatabase,
    /// The adapter reports no answering model at all.
    Unreported,
}

/// Location of a provider's user skill tree relative to the host home or an
/// absolute environment override. The fallback is relative to the home.
/// Host variables that identify a running native client session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientEnvMarkers {
    /// Variable whose value must be exactly `1`.
    pub equals_one: Option<&'static str>,
    /// Variables whose non-empty presence is sufficient.
    pub presence: &'static [&'static str],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkillLocation {
    pub environment: Option<&'static str>,
    pub fallback: &'static str,
    pub suffix: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountQuota {
    LocalLedger,
    ObservedRefusals,
}
#[derive(Debug, Clone, Copy)]
pub struct AccountModelPlan {
    pub plan: &'static str,
    pub model_prefix: &'static str,
    pub quota: AccountQuota,
}

pub const ADAPTER_ROW_SOURCE: &str = "crates/records/src/adapters.rs";
pub const ADAPTER_ROW_REASON: &str = "declared by the adapter row";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdapterFact<T> {
    pub value: T,
    pub reason: &'static str,
    pub source: &'static str,
}
impl<T> AdapterFact<T> {
    pub const fn row(value: T) -> Self {
        Self {
            value,
            reason: ADAPTER_ROW_REASON,
            source: ADAPTER_ROW_SOURCE,
        }
    }
    pub const fn because(value: T, reason: &'static str, source: &'static str) -> Self {
        Self {
            value,
            reason,
            source,
        }
    }
}
impl AdapterFact<bool> {
    pub const fn supported() -> Self {
        Self::row(true)
    }
    pub const fn unsupported(reason: &'static str, source: &'static str) -> Self {
        Self::because(false, reason, source)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalEventStrength {
    /// The provider's terminal event states success or failure, and it
    /// outranks a clean exit.
    Strong,
    Weak,
    Absent,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageAttribution {
    Attempt,
    Session,
}
/// How the provider session id is learned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionReference {
    /// The decoder emits a provider session event.
    Reported,
    /// The session id is inferred from other evidence.
    Inferred,
    /// No provider session id is available.
    Absent,
}
/// A known adapter limitation, shown in diagnostics and the capability matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caveat {
    pub code: &'static str,
    pub note: &'static str,
    pub source: &'static str,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetainedArtifact {
    pub parent: &'static str,
    pub name: &'static str,
    pub scrub: &'static [&'static str],
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retention {
    pub transcript_layout: AdapterFact<bool>,
    pub artifacts: &'static [RetainedArtifact],
    /// Sessions formerly stored in host configuration now require private attempt state.
    pub relocated_session_state: bool,
    /// Copy only session files when retaining private state.
    pub sessions_only: bool,
    /// Setup may mirror a transcript into the registered account directory.
    pub transcript_account_root: bool,
}

/// Wire format used by a native harness hook registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookProtocol {
    CommandGroups { timeout_unit: HookTimeoutUnit },
    CursorCommands { timeout_unit: HookTimeoutUnit },
    NamedGroups { timeout_unit: HookTimeoutUnit },
    OpenCodePlugin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookTimeoutUnit {
    Seconds,
    Milliseconds,
}

// Keep semantic identities, CLI spellings and the invariant's domain in one
// declaration: adding an action adds one row, not parallel enum/match lists.
macro_rules! hook_actions {
    ($( $action:ident => $command:literal ),+ $(,)?) => {
        /// Provenant hook handler independent of a provider's wire event name.
        /// Ordered by declaration so it can key the managed-hook sets that
        /// decide which retired leaves transfer ownership.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
        pub enum HookAction { $( $action, )+ }
        impl HookAction {
            pub const ALL: &'static [Self] = &[ $( Self::$action, )+ ];
            pub const fn command(self) -> &'static str {
                match self { $( Self::$action => $command, )+ }
            }
            /// Answer delivery is the only use of a tool hook. Which native
            /// event carries it is per adapter; neither event grants policy.
            pub const fn is_answer_delivery(self) -> bool {
                matches!(self, Self::PreToolUse | Self::PostToolUse)
            }
            /// The action declared to replace `self` when a release retires the
            /// native event that carried it. Answer delivery is the only
            /// identity that has moved between events; other actions have no
            /// replacement and are simply dropped when retired. A retired leaf's
            /// ownership transfers to this action and no other.
            pub const fn declared_replacement(self) -> Option<Self> {
                match self {
                    Self::PreToolUse => Some(Self::PostToolUse),
                    Self::PostToolUse => Some(Self::PreToolUse),
                    _ => None,
                }
            }
        }
    };
}
hook_actions! {
    PreToolUse => "pre-tool-use",
    PostToolUse => "post-tool-use",
    PreCompact => "pre-compact",
    Compact => "compact",
    ChairPrompt => "chair-prompt",
    ChairStart => "chair-start",
    ChairStop => "chair-stop",
    SessionStart => "session-start",
    Stop => "stop",
    WorktreeCreate => "worktree-create",
    WorktreeRemove => "worktree-remove",
}

/// Native mapping or a sourced provider limitation for one Provenant action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    Supported {
        action: HookAction,
        event: &'static str,
        matcher: Option<&'static str>,
        timeout_seconds: Option<u64>,
    },
    Unsupported {
        action: HookAction,
        reason: &'static str,
        source: &'static str,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeCapabilities {
    pub hooks: AdapterFact<bool>,
    pub answer_delivery: AdapterFact<bool>,
    /// Whether a pre-compaction response reaches the model's context.
    pub precompact_context: AdapterFact<bool>,
    pub instructions: AdapterFact<bool>,
}

/// Managed harness paths and native integration capabilities. Wire formats stay
/// in config; this is the single declaration of which integrations exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HarnessInstall {
    pub home: &'static str,
    /// Environment variable and suffix that bind a provider launch to its
    /// isolated native configuration home.
    pub lane_home: (&'static str, &'static str),
    pub config: &'static str,
    /// Config relative to the user home when using the default harness home.
    pub default_home_config: Option<&'static str>,
    pub instruction_file: Option<&'static str>,
    pub hook_file: Option<&'static str>,
    /// Provider CLI option that loads a generated hook settings document.
    pub hook_settings_arg: Option<&'static str>,
    pub native: NativeCapabilities,
    pub hook_protocol: Option<HookProtocol>,
    pub hook_events: &'static [HookEvent],
    /// The adapter declares its instruction documents as an array in its config.
    pub instruction_array: bool,
    pub source: &'static str,
}

const CHAIR_HOOKS_NOT_APPLICABLE: &str =
    "Provenant chair switching is enabled only for adapters declaring chair_switch";
const CHAIR_HOOKS_SOURCE: &str =
    "AdapterSpec.launch.chair_switch in crates/records/src/adapters.rs";
const CLAUDE_HOOK_EVENTS: &[HookEvent] = &[
    // Claude Code treats a failed PreToolUse hook as a refusal of the tool.
    // Answer delivery offers untrusted inbox data and needs no such authority,
    // so it runs after the tool through PostToolUse's additionalContext.
    HookEvent::Supported {
        action: HookAction::PostToolUse,
        event: "PostToolUse",
        matcher: None,
        timeout_seconds: None,
    },
    HookEvent::Supported {
        action: HookAction::PreCompact,
        event: "PreCompact",
        matcher: None,
        timeout_seconds: Some(5),
    },
    HookEvent::Supported {
        action: HookAction::Compact,
        event: "SessionStart",
        matcher: Some("compact"),
        timeout_seconds: Some(5),
    },
    HookEvent::Supported {
        action: HookAction::ChairPrompt,
        event: "UserPromptSubmit",
        matcher: None,
        timeout_seconds: Some(10),
    },
    HookEvent::Supported {
        action: HookAction::ChairStart,
        event: "SessionStart",
        matcher: None,
        timeout_seconds: Some(5),
    },
    HookEvent::Supported {
        action: HookAction::ChairStop,
        event: "Stop",
        matcher: None,
        timeout_seconds: Some(5),
    },
    HookEvent::Supported {
        action: HookAction::SessionStart,
        event: "SessionStart",
        matcher: Some("startup|resume|clear"),
        timeout_seconds: Some(5),
    },
    HookEvent::Supported {
        action: HookAction::Stop,
        event: "Stop",
        matcher: None,
        timeout_seconds: Some(5),
    },
    HookEvent::Supported {
        action: HookAction::WorktreeCreate,
        event: "WorktreeCreate",
        matcher: None,
        timeout_seconds: Some(60),
    },
    HookEvent::Supported {
        action: HookAction::WorktreeRemove,
        event: "WorktreeRemove",
        matcher: None,
        timeout_seconds: Some(60),
    },
];
const CODEX_HOOK_EVENTS: &[HookEvent] = &[
    // Codex keeps its declared PreToolUse answer-delivery mapping: this release
    // has no verified non-blocking Codex event that carries additionalContext.
    HookEvent::Supported {
        action: HookAction::PreToolUse,
        event: "PreToolUse",
        matcher: None,
        timeout_seconds: None,
    },
    HookEvent::Supported {
        action: HookAction::PreCompact,
        event: "PreCompact",
        matcher: None,
        timeout_seconds: Some(5),
    },
    HookEvent::Supported {
        action: HookAction::Compact,
        event: "SessionStart",
        matcher: Some("compact"),
        timeout_seconds: Some(5),
    },
    HookEvent::Unsupported {
        action: HookAction::ChairPrompt,
        reason: CHAIR_HOOKS_NOT_APPLICABLE,
        source: CHAIR_HOOKS_SOURCE,
    },
    HookEvent::Unsupported {
        action: HookAction::ChairStart,
        reason: CHAIR_HOOKS_NOT_APPLICABLE,
        source: CHAIR_HOOKS_SOURCE,
    },
    HookEvent::Unsupported {
        action: HookAction::ChairStop,
        reason: CHAIR_HOOKS_NOT_APPLICABLE,
        source: CHAIR_HOOKS_SOURCE,
    },
    HookEvent::Supported {
        action: HookAction::SessionStart,
        event: "SessionStart",
        matcher: Some("startup|resume|clear|fork"),
        timeout_seconds: Some(5),
    },
    HookEvent::Supported {
        action: HookAction::Stop,
        event: "Stop",
        matcher: None,
        timeout_seconds: Some(5),
    },
    HookEvent::Unsupported {
        action: HookAction::WorktreeCreate,
        reason: "Codex has no native worktree lifecycle hook event",
        source: "https://learn.chatgpt.com/docs/hooks#where-codex-looks-for-hooks",
    },
    HookEvent::Unsupported {
        action: HookAction::WorktreeRemove,
        reason: "Codex has no native worktree lifecycle hook event",
        source: "https://learn.chatgpt.com/docs/hooks#where-codex-looks-for-hooks",
    },
];
const CURSOR_HOOK_EVENTS: &[HookEvent] = &[
    HookEvent::Unsupported {
        action: HookAction::PreToolUse,
        reason: "Cursor preToolUse does not accept Provenant's additionalContext answer response",
        source: "https://cursor.com/docs/hooks#pretooluse",
    },
    HookEvent::Supported {
        action: HookAction::PreCompact,
        event: "preCompact",
        matcher: None,
        timeout_seconds: Some(5),
    },
    HookEvent::Unsupported {
        action: HookAction::Compact,
        reason: "Cursor exposes preCompact but no post-compaction event",
        source: "https://cursor.com/docs/hooks#supported-hooks",
    },
    HookEvent::Unsupported {
        action: HookAction::ChairPrompt,
        reason: CHAIR_HOOKS_NOT_APPLICABLE,
        source: CHAIR_HOOKS_SOURCE,
    },
    HookEvent::Unsupported {
        action: HookAction::ChairStart,
        reason: CHAIR_HOOKS_NOT_APPLICABLE,
        source: CHAIR_HOOKS_SOURCE,
    },
    HookEvent::Unsupported {
        action: HookAction::ChairStop,
        reason: CHAIR_HOOKS_NOT_APPLICABLE,
        source: CHAIR_HOOKS_SOURCE,
    },
    HookEvent::Supported {
        action: HookAction::SessionStart,
        event: "sessionStart",
        matcher: None,
        timeout_seconds: Some(5),
    },
    HookEvent::Supported {
        action: HookAction::Stop,
        event: "stop",
        matcher: None,
        timeout_seconds: Some(5),
    },
    HookEvent::Unsupported {
        action: HookAction::WorktreeCreate,
        reason: "Cursor has no native worktree lifecycle hook event",
        source: "https://cursor.com/docs/hooks#supported-hooks",
    },
    HookEvent::Unsupported {
        action: HookAction::WorktreeRemove,
        reason: "Cursor has no native worktree lifecycle hook event",
        source: "https://cursor.com/docs/hooks#supported-hooks",
    },
];
const AGY_HOOK_EVENTS: &[HookEvent] = &[
    HookEvent::Unsupported {
        action: HookAction::PreToolUse,
        reason: "Antigravity PreToolUse returns a tool decision and argument overwrite, not Provenant's additionalContext answer response",
        source: "https://antigravity.google/docs/hooks#input-and-output-contract",
    },
    HookEvent::Unsupported {
        action: HookAction::PreCompact,
        reason: "Antigravity exposes no pre-compaction event",
        source: "https://antigravity.google/docs/hooks#supported-events",
    },
    HookEvent::Unsupported {
        action: HookAction::Compact,
        reason: "Antigravity exposes no post-compaction event",
        source: "https://antigravity.google/docs/hooks#supported-events",
    },
    HookEvent::Unsupported {
        action: HookAction::ChairPrompt,
        reason: CHAIR_HOOKS_NOT_APPLICABLE,
        source: CHAIR_HOOKS_SOURCE,
    },
    HookEvent::Unsupported {
        action: HookAction::ChairStart,
        reason: CHAIR_HOOKS_NOT_APPLICABLE,
        source: CHAIR_HOOKS_SOURCE,
    },
    HookEvent::Unsupported {
        action: HookAction::ChairStop,
        reason: CHAIR_HOOKS_NOT_APPLICABLE,
        source: CHAIR_HOOKS_SOURCE,
    },
    HookEvent::Supported {
        action: HookAction::SessionStart,
        event: "SessionStart",
        matcher: None,
        timeout_seconds: Some(5),
    },
    HookEvent::Supported {
        action: HookAction::Stop,
        event: "Stop",
        matcher: None,
        timeout_seconds: Some(5),
    },
    HookEvent::Unsupported {
        action: HookAction::WorktreeCreate,
        reason: "Antigravity exposes no worktree lifecycle event",
        source: "https://antigravity.google/docs/hooks#supported-events",
    },
    HookEvent::Unsupported {
        action: HookAction::WorktreeRemove,
        reason: "Antigravity exposes no worktree lifecycle event",
        source: "https://antigravity.google/docs/hooks#supported-events",
    },
];
const OPENCODE_HOOK_EVENTS: &[HookEvent] = &[
    HookEvent::Unsupported {
        action: HookAction::PreToolUse,
        reason: "OpenCode tool.execute.before hooks cannot inject additional context into the active session",
        source: "https://opencode.ai/docs/build/plugins/server#tool-execution",
    },
    HookEvent::Supported {
        action: HookAction::PreCompact,
        event: "session.compaction",
        matcher: None,
        timeout_seconds: Some(5),
    },
    HookEvent::Supported {
        action: HookAction::Compact,
        event: "session.compaction.ended",
        matcher: None,
        timeout_seconds: Some(5),
    },
    HookEvent::Unsupported {
        action: HookAction::ChairPrompt,
        reason: CHAIR_HOOKS_NOT_APPLICABLE,
        source: CHAIR_HOOKS_SOURCE,
    },
    HookEvent::Unsupported {
        action: HookAction::ChairStart,
        reason: CHAIR_HOOKS_NOT_APPLICABLE,
        source: CHAIR_HOOKS_SOURCE,
    },
    HookEvent::Unsupported {
        action: HookAction::ChairStop,
        reason: CHAIR_HOOKS_NOT_APPLICABLE,
        source: CHAIR_HOOKS_SOURCE,
    },
    HookEvent::Supported {
        action: HookAction::SessionStart,
        event: "session.context",
        matcher: None,
        timeout_seconds: Some(5),
    },
    HookEvent::Supported {
        action: HookAction::Stop,
        event: "session.idle",
        matcher: None,
        timeout_seconds: Some(5),
    },
    HookEvent::Unsupported {
        action: HookAction::WorktreeCreate,
        reason: "OpenCode exposes worktree strategy transforms but no generic worktree lifecycle callback",
        source: "https://opencode.ai/docs/build/plugins/server#worktrees",
    },
    HookEvent::Unsupported {
        action: HookAction::WorktreeRemove,
        reason: "OpenCode exposes worktree strategy transforms but no generic worktree lifecycle callback",
        source: "https://opencode.ai/docs/build/plugins/server#worktrees",
    },
];

/// Framing declared by the adapter for provider stream decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamFraming {
    ProviderJsonl,
    Raw,
}

#[derive(Debug, Clone, Copy)]
pub struct AdapterIdentity {
    pub name: &'static str,
    /// Provider family used by declared route constraints.
    pub family: &'static str,
    pub binary: &'static str,
    /// How the adapter's own executable updates itself.
    pub update: UpdateCommand,
    /// Format of a supported CLI model listing.
    pub model_listing: Option<ModelListingFormat>,
    /// Default provider setting for the models this adapter can select.
    pub default_models: Option<&'static [&'static str]>,
    /// Namespace an adapter adds to model ids. Stripped before comparing a
    /// requested id with an account's offer list.
    pub model_id_prefix: Option<&'static str>,
    /// CLI argv for a model-listing probe. Absence excludes the adapter from
    /// model discovery entirely.
    pub model_discovery: Option<&'static [&'static str]>,
    pub namespaced_models: bool,
    /// The account reports the models it offers; a launch naming another model is refused.
    pub offered_models: bool,
    /// Preparation mechanism for a model-less named entry.
    pub named_entry: NamedEntry,
}

#[derive(Debug, Clone, Copy)]
pub struct AdapterLaunch {
    pub framing: StreamFraming,
    pub config_environment: &'static [&'static str],
    pub argv_prompt: bool,
    /// Shell wrapper path and environment selector, relative to scratch.
    pub shell_prefix: Option<(&'static str, &'static str)>,
    /// Provider argument granting file tools access to captured Git context.
    pub context_directory_arg: Option<&'static str>,
    /// Checkout-root files, in gitignore syntax, through which the provider
    /// hides paths from its own agent, including the files of a parent
    /// checkout that contains the lane's worktree. A lane worktree under a
    /// listed pattern cannot be read or written by the lane, so doctor and
    /// launch preparation warn (`provenant_cli::ignore_files`); they never
    /// edit the file.
    pub ignore_files: &'static [&'static str],
    /// Checkout-root files through which the provider only skips paths when
    /// indexing; they never hide a path from the lane's agent. The ignore-file
    /// warning names them as the place to keep worktrees out of indexing.
    pub indexing_ignore_files: &'static [&'static str],
    pub compaction_env: Option<&'static str>,
    /// Host variables identifying this adapter's native client session.
    pub client_env_markers: Option<ClientEnvMarkers>,
    pub skill_location: Option<SkillLocation>,
    /// Home-relative installation roots that can supply linked skills.
    /// They describe discovery provenance, not an eligibility allowlist.
    pub skill_install_roots: &'static [&'static str],
    /// Supports launching a transferred interactive session.
    pub session_handoff: bool,
    /// Refresh an account token through the authorised credential broker at launch.
    pub launch_token_refresh: bool,
    /// The adapter's plan uses the Codex clean/inherited config policy.
    pub codex_policy: bool,
    pub github_auth: Option<GithubAuth>,
    /// Use the requested route as the observed route when a command exits.
    pub exit_route: bool,
    /// Config-deprecation notices can arrive on stderr as well as the event
    /// stream; the host folds both into one warning per attempt.
    pub stderr_config_notices: bool,
    /// Classify exit-time stderr for quota refusals.
    pub stderr_quota: bool,
    /// Parse stdout error diagnostics for provider failure explanations.
    pub stdout_error_diagnostic: bool,
    /// Remove a duplicated complete answer from the provider stream.
    pub deduplicate_result: bool,
    /// Oversized command-execution frames are ignorable evidence for this adapter.
    pub oversize_command_frames: bool,
    /// Plain text events carry the final answer for this adapter.
    pub answer_from_text: bool,
    /// Diagnose absent usage from structured result events.
    pub result_event_usage: bool,
    /// The declared meaning of this adapter's usage reports. The accumulator
    /// reads it; no reader guesses scope, cache inclusion or liveness.
    pub usage: UsageSemantics,
    /// Provider-specific explanation when writer token budgets cannot stop it.
    pub token_budget_notice: Option<&'static str>,
    pub chair_switch: bool,
    /// Drop a valid but unrecorded model effort with a route warning.
    pub drop_unrecorded_effort: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct AdapterAccounts {
    /// Environment override and home-relative default for an implicitly discovered account.
    pub default_directory: Option<(&'static str, &'static str)>,
    /// Credential policy of the declared default directory at its home-relative path.
    pub default_root_kind: &'static provenant_core::CredentialKind,
    pub registered: bool,
    /// A registered account's directory is the client's configuration
    /// directory, which account settings sync keeps in step. False where it is
    /// something else: agy's is the account's keychain home.
    pub config_dir: bool,
    /// Fixed home-relative prefix for registered account directories, if required.
    pub directory_prefix: Option<&'static str>,
    /// Prefix of a registered key derived from the signed-in identity, for
    /// adapters that namespace their account keys.
    pub key_prefix: Option<&'static str>,
    /// Namespace carried inside a provider identity string and folded into the
    /// registered key by `account_key`.
    pub identity_namespace: Option<&'static str>,
    /// Registered account configuration supports shared settings overlays.
    pub shared_settings: bool,
    /// Registered accounts use an isolated login keychain rooted in their home.
    pub isolated_login_keychain: bool,
    /// Synthetic account label and key for an adapter using its local login.
    pub local_entry: (&'static str, &'static str),
    pub setup_root: SetupRoot,
    pub peer_bootstrap: bool,
    /// Registered plans partition model namespaces independently of account keys.
    pub model_plans: &'static [AccountModelPlan],
}

#[derive(Debug, Clone, Copy)]
pub struct AdapterQuota {
    pub probe: QuotaProbeKind,
    /// Missing monthly observations constrain allowance pacing for this adapter.
    pub monthly_allowance: bool,
    pub credits: bool,
    pub grouped_pools: bool,
    pub retain_accounts: bool,
    pub identity_check: bool,
    /// A snapshot governs only this model prefix; None governs every model.
    pub model_prefix: Option<&'static str>,
    pub free_pool: Option<&'static str>,
    /// Where a recorded quota refusal's expiry comes from, as the quota view names it.
    pub refusal_reset_source: &'static str,
    /// Maximum age of provider refusal evidence before revalidation.
    pub refusal_revalidation_seconds: Option<i64>,
    pub pacing_keys: Option<(&'static str, &'static str)>,
    pub allowance_prefix: Option<&'static str>,
    /// Local allowance windows governing models under allowance_prefix.
    pub allowance_windows: &'static [&'static str],
}

#[derive(Debug, Clone, Copy)]
pub struct AdapterLogin {
    pub local: bool,
    pub reprobe: LoginReprobe,
    pub observation: LoginObservation,
    /// The login recheck identifies the signed-in account.
    pub check_names_account: bool,
    /// How a registered account's identity is read from its directory.
    pub identity_probe: AccountIdentityProbe,
    pub authentication: Option<&'static str>,
    /// When a host-access launch warns about the adapter's native login.
    pub native_auth_warning: NativeAuthWarning,
    /// Files observed by metadata only, relative to the account directory.
    pub credential_files: &'static [&'static str],
    /// Additional Provenant-owned files protected within registered stores.
    pub credential_auxiliary_files: &'static [&'static str],
    /// Also watch the account directory's sibling JSON profile.
    pub credential_profile_sibling: bool,
    /// Also watch the host login keychain on macOS.
    pub credential_host_keychain: bool,
    pub hint: &'static str,
    /// Account-scoped interactive login command, with {directory} shell quoted.
    pub template: Option<&'static str>,
    /// Account-scoped interactive command for refreshing an expired login.
    /// `{directory}` is shell quoted by the caller.
    pub refresh_template: Option<&'static str>,
    /// Lowercase provider stderr fragments indicating a missing or expired login.
    pub auth_markers: &'static [&'static str],
}

#[derive(Debug, Clone, Copy)]
pub struct AdapterSpec {
    pub id: Harness,
    pub identity: AdapterIdentity,
    pub launch: AdapterLaunch,
    pub accounts: AdapterAccounts,
    pub quota: AdapterQuota,
    pub login: AdapterLogin,
    pub install: Option<HarnessInstall>,
    pub retention: Retention,
    pub capabilities: Capabilities,
}

const ALL_NETWORK: &[NetworkAccess] = &[
    NetworkAccess::Provider,
    NetworkAccess::Local,
    NetworkAccess::Open,
];

/// Declared launch capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaunchCapabilities {
    pub model: AdapterFact<bool>,
    pub effort: AdapterFact<bool>,
    pub image_input: AdapterFact<bool>,
    pub resume: AdapterFact<bool>,
    pub write: AdapterFact<bool>,
    pub network: &'static [NetworkAccess],
    pub default_network: NetworkAccess,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub launch: LaunchCapabilities,
    pub control: ControlCapabilities,
    pub terminal_event: AdapterFact<TerminalEventStrength>,
    pub answering_model: AdapterFact<AnsweringModelSource>,
    pub usage_attribution: AdapterFact<UsageAttribution>,
    pub live_input: AdapterFact<bool>,
    pub session_reference: AdapterFact<SessionReference>,
    pub caveats: &'static [Caveat],
}
#[derive(Debug, Clone, Default)]
pub struct LaunchRequest<'a> {
    pub model: Option<&'a str>,
    pub effort: Option<&'a str>,
    pub image_input: bool,
    pub resume: bool,
    pub network: Option<NetworkAccess>,
    pub write: bool,
    pub skill_location: bool,
    pub gap_policy: GapPolicy,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchField {
    Model,
    Effort,
    ImageInput,
    Resume,
    Network,
    Write,
    SkillLocation,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchAction {
    Degrade,
    Refuse,
}
impl LaunchField {
    pub fn name(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Effort => "effort",
            Self::ImageInput => "image input",
            Self::Resume => "resume",
            Self::Network => "network",
            Self::Write => "write",
            Self::SkillLocation => "skill location",
        }
    }
    /// The fields whose gap action an owner may set.
    pub const DEGRADABLE: [Self; 3] = [Self::Model, Self::Effort, Self::ImageInput];
    /// The policy rule that sets this field's gap action.
    pub const fn policy_code(self) -> Option<&'static str> {
        match self {
            Self::Model => Some("LAUNCH_MODEL_UNSUPPORTED"),
            Self::Effort => Some("LAUNCH_EFFORT_UNSUPPORTED"),
            Self::ImageInput => Some("LAUNCH_IMAGE_INPUT_UNSUPPORTED"),
            _ => None,
        }
    }
    /// The `[[adapter_capabilities]] on_gap` key for this field.
    pub const fn on_gap_key(self) -> Option<&'static str> {
        match self {
            Self::Model => Some("model"),
            Self::Effort => Some("effort"),
            Self::ImageInput => Some("image_input"),
            _ => None,
        }
    }
    pub fn default_action(self) -> LaunchAction {
        match self {
            Self::Model | Self::Effort | Self::ImageInput => LaunchAction::Degrade,
            _ => LaunchAction::Refuse,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchGap {
    pub adapter: &'static str,
    pub field: LaunchField,
    pub action: LaunchAction,
    pub reason: &'static str,
    pub source: &'static str,
    /// Whether the caller should surface this gap as a warning. Refusals always
    /// warn; a degrades-only gap can be silenced by `off`.
    pub warn: bool,
    /// The layer and setting that chose the action. A refuse-only field
    /// always carries the compiled default: no setting can change it.
    pub action_source: crate::Source,
}
impl std::fmt::Display for LaunchGap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} does not support {}: {}; source={}{}",
            self.adapter,
            self.field.name(),
            self.reason,
            self.source,
            self.action_suffix()
        )
    }
}
impl std::error::Error for LaunchGap {}
impl LaunchGap {
    /// The setting that chose a degradable field's action, or, for the
    /// compiled default, the two settings that can change it. A refuse-only
    /// field names none: no setting changes it.
    fn action_suffix(&self) -> String {
        let (Some(code), Some(key)) = (self.field.policy_code(), self.field.on_gap_key()) else {
            return String::new();
        };
        if self.action_source.layer == crate::Layer::Default {
            let action = match (self.action, self.warn) {
                (LaunchAction::Refuse, _) => GapAction::Refuse,
                (_, true) => GapAction::Warn,
                (_, false) => GapAction::Off,
            };
            return format!(
                "; {} by default; set policy.{code} or [[adapter_capabilities]] on_gap.{key}",
                action.as_str()
            );
        }
        format!(
            "; action set by {} ({})",
            self.action_source
                .origin
                .as_deref()
                .unwrap_or("an unnamed setting"),
            self.action_source.layer.description()
        )
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchCheck {
    pub gaps: Vec<LaunchGap>,
}
impl LaunchCheck {
    pub fn refusal(&self) -> Option<&LaunchGap> {
        self.gaps.iter().find(|d| d.action == LaunchAction::Refuse)
    }
    pub fn degraded(&self, field: LaunchField) -> Option<&LaunchGap> {
        self.gaps
            .iter()
            .find(|d| d.field == field && d.action == LaunchAction::Degrade)
    }
}

/// Why a fact was switched off by an owner entry rather than by the row.
pub const OVERLAY_REASON: &str = "disabled by an [[adapter_capabilities]] entry; `provenant adapter capabilities` names its layer and reason";
pub const OVERLAY_SOURCE: &str = "config.toml#adapter_capabilities";

/// Capabilities an owner may narrow through `[[adapter_capabilities]]`.
/// Widening a row's fact is not expressible: only `disable` is accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OverridableCapability {
    Model,
    Effort,
    ImageInput,
    Resume,
    Write,
    LiveInput,
}
impl OverridableCapability {
    pub const ALL: &'static [Self] = &[
        Self::Model,
        Self::Effort,
        Self::ImageInput,
        Self::Resume,
        Self::Write,
        Self::LiveInput,
    ];
    pub const fn key(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Effort => "effort",
            Self::ImageInput => "image_input",
            Self::Resume => "resume",
            Self::Write => "write",
            Self::LiveInput => "live_input",
        }
    }
    pub fn parse(key: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|item| item.key() == key)
    }
}

/// What happens when a requested capability is absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GapAction {
    #[default]
    Warn,
    Off,
    Refuse,
}

impl GapAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Off => "off",
            Self::Refuse => "refuse",
        }
    }
}

/// One degradable field's action, where it was set, and the lower-precedence
/// candidates it shadowed (lowest first), so a gap and `--explain` can name
/// every setting that took part.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GapRule {
    pub action: GapAction,
    pub source: crate::Source,
    pub shadowed: Vec<crate::Sourced<GapAction>>,
}

/// The effective gap actions. Resume, network, write and skill location have no
/// rule: they always keep their default refusal.
///
/// One resolver, [`CapabilityOverlay::gap_policy`], decides each degradable
/// field. Precedence, lowest first: the compiled default; then
/// `policy.LAUNCH_<FIELD>_UNSUPPORTED`, itself layered host, project and
/// dispatch by the policy table; then the adapter's `[[adapter_capabilities]]`
/// `on_gap.<field>`, host then project. The per-adapter entry is the narrower
/// setting, so it wins over the policy rule for every layer.
/// Spec: docs/specs/run-launch.md#capability-overrides-and-gap-policy
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GapPolicy {
    pub model: GapRule,
    pub effort: GapRule,
    pub image_input: GapRule,
}
impl GapPolicy {
    pub fn rule(&self, field: LaunchField) -> Option<&GapRule> {
        match field {
            LaunchField::Model => Some(&self.model),
            LaunchField::Effort => Some(&self.effort),
            LaunchField::ImageInput => Some(&self.image_input),
            LaunchField::Resume
            | LaunchField::Network
            | LaunchField::Write
            | LaunchField::SkillLocation => None,
        }
    }
    fn rule_mut(&mut self, field: LaunchField) -> Option<&mut GapRule> {
        match field {
            LaunchField::Model => Some(&mut self.model),
            LaunchField::Effort => Some(&mut self.effort),
            LaunchField::ImageInput => Some(&mut self.image_input),
            LaunchField::Resume
            | LaunchField::Network
            | LaunchField::Write
            | LaunchField::SkillLocation => None,
        }
    }
    fn set(&mut self, field: LaunchField, rule: GapRule) {
        if let Some(slot) = self.rule_mut(field) {
            *slot = rule;
        }
    }
}

/// One `[[adapter_capabilities]]` row from a host or project layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterOverride {
    pub adapter: String,
    pub disable: Vec<OverridableCapability>,
    pub reason: Option<String>,
    pub on_gap: Vec<(LaunchField, GapAction)>,
    pub source: crate::Source,
}

/// Effective owner entries; a later layer replaces the whole entry by adapter.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CapabilityOverlay {
    pub entries: Vec<AdapterOverride>,
}
impl CapabilityOverlay {
    pub fn entry(&self, adapter: &str) -> Option<&AdapterOverride> {
        self.entries.iter().find(|row| row.adapter == adapter)
    }

    /// The one resolver for degradable gap actions; [`GapPolicy`] states the
    /// precedence. The policy rule's action is the floor, named by its layer
    /// and key; an adapter entry's `on_gap` replaces it and records it as
    /// shadowed. Other fields have no rule.
    pub fn gap_policy(&self, adapter: &str, policy: &crate::policy::Policy) -> GapPolicy {
        let mut gap = GapPolicy::default();
        let entry = self.entry(adapter);
        for field in LaunchField::DEGRADABLE {
            let (Some(code), Some(key)) = (field.policy_code(), field.on_gap_key()) else {
                continue;
            };
            let effective = policy.rule(code);
            let action = match effective.rule.strength {
                crate::policy::Strength::Warn => GapAction::Warn,
                crate::policy::Strength::Off => GapAction::Off,
                crate::policy::Strength::Refuse => GapAction::Refuse,
            };
            let mut rule = GapRule {
                action,
                source: crate::Source {
                    layer: effective.source.layer(),
                    origin: Some(format!("policy.{code}")),
                },
                shadowed: Vec::new(),
            };
            if let Some((entry, action)) = entry.and_then(|entry| {
                entry
                    .on_gap
                    .iter()
                    .find(|(gap_field, _)| *gap_field == field)
                    .map(|(_, action)| (entry, *action))
            }) {
                let place = entry
                    .source
                    .origin
                    .as_deref()
                    .unwrap_or("[[adapter_capabilities]]");
                let floor = std::mem::replace(
                    &mut rule,
                    GapRule {
                        action,
                        source: crate::Source {
                            layer: entry.source.layer,
                            origin: Some(format!(
                                "{place}[adapter={}].on_gap.{key}",
                                entry.adapter
                            )),
                        },
                        shadowed: Vec::new(),
                    },
                );
                rule.shadowed.push(crate::Sourced {
                    value: floor.action,
                    source: floor.source,
                });
            }
            gap.set(field, rule);
        }
        gap
    }
}

impl AdapterSpec {
    /// Whether account entries describe metadata for one shared local login
    /// instead of separate registered credential trees.
    pub fn uses_shared_local_login(&self) -> bool {
        !self.accounts.registered && self.login.local
    }

    /// Whether `key` names the synthetic entry of an unregistered local login.
    /// Only an adapter that has both registered accounts and a local login (agy)
    /// has one: it runs on the user's own login, so nothing is registered, probed
    /// or pinned, and registered keys (`agy-<fingerprint>`) never collide with it.
    pub fn is_unregistered_local(&self, key: &str) -> bool {
        self.accounts.registered && self.login.local && key == "local"
    }
    /// A copy whose disabled facts are unsupported with the overlay's own
    /// reason and source. It never turns a fact on and changes nothing else.
    pub fn with_overlay(&self, overlay: &CapabilityOverlay) -> AdapterSpec {
        let Some(entry) = overlay.entry(self.identity.name) else {
            return *self;
        };
        if entry.disable.is_empty() {
            return *self;
        }
        let mut spec = *self;
        for capability in &entry.disable {
            match capability {
                OverridableCapability::Model => {
                    spec.capabilities.launch.model =
                        AdapterFact::unsupported(OVERLAY_REASON, OVERLAY_SOURCE)
                }
                OverridableCapability::Effort => {
                    spec.capabilities.launch.effort =
                        AdapterFact::unsupported(OVERLAY_REASON, OVERLAY_SOURCE)
                }
                OverridableCapability::ImageInput => {
                    spec.capabilities.launch.image_input =
                        AdapterFact::unsupported(OVERLAY_REASON, OVERLAY_SOURCE)
                }
                OverridableCapability::Resume => {
                    spec.capabilities.launch.resume =
                        AdapterFact::unsupported(OVERLAY_REASON, OVERLAY_SOURCE)
                }
                OverridableCapability::Write => {
                    spec.capabilities.launch.write =
                        AdapterFact::unsupported(OVERLAY_REASON, OVERLAY_SOURCE)
                }
                OverridableCapability::LiveInput => {
                    spec.capabilities.live_input =
                        AdapterFact::unsupported(OVERLAY_REASON, OVERLAY_SOURCE)
                }
            }
        }
        spec
    }

    /// Check a request against the declared table before any side effect.
    pub fn check_launch(&self, request: &LaunchRequest<'_>) -> LaunchCheck {
        let caps = &self.capabilities.launch;
        let mut gaps = Vec::new();
        let mut add = |field: LaunchField, reason, source| {
            let rule = request.gap_policy.rule(field);
            let action = match rule {
                Some(GapRule {
                    action: GapAction::Refuse,
                    ..
                }) => LaunchAction::Refuse,
                Some(_) => LaunchAction::Degrade,
                None => field.default_action(),
            };
            let warn = action == LaunchAction::Refuse
                || rule.is_some_and(|rule| rule.action == GapAction::Warn);
            gaps.push(LaunchGap {
                adapter: self.identity.name,
                field,
                action,
                warn,
                action_source: rule.map(|rule| rule.source.clone()).unwrap_or_default(),
                reason,
                source,
            })
        };
        for (requested, field, declaration) in [
            (request.model.is_some(), LaunchField::Model, caps.model),
            (request.effort.is_some(), LaunchField::Effort, caps.effort),
            (
                request.image_input,
                LaunchField::ImageInput,
                caps.image_input,
            ),
            (request.resume, LaunchField::Resume, caps.resume),
        ] {
            if requested && !declaration.value {
                add(field, declaration.reason, declaration.source);
            }
        }
        if request
            .network
            .is_some_and(|mode| !caps.network.contains(&mode))
        {
            add(
                LaunchField::Network,
                "network mode is outside the adapter's sandbox profiles",
                ADAPTER_ROW_SOURCE,
            );
        }
        if request.write && !caps.write.value {
            add(LaunchField::Write, caps.write.reason, caps.write.source);
        }
        if request.skill_location && self.launch.skill_location.is_none() {
            add(
                LaunchField::SkillLocation,
                "the adapter declares no skill location",
                ADAPTER_ROW_SOURCE,
            );
        }
        LaunchCheck { gaps }
    }
    pub fn account_plan(&self, plan: Option<&str>) -> Option<&AccountModelPlan> {
        self.accounts
            .model_plans
            .iter()
            .find(|row| plan.is_some_and(|plan| row.plan.eq_ignore_ascii_case(plan.trim())))
    }
    pub fn is_free_plan(&self, plan: Option<&str>) -> bool {
        self.account_plan(plan)
            .is_some_and(|row| row.quota == AccountQuota::ObservedRefusals)
    }
    /// A registered plan narrows the models an account serves. A missing or
    /// unrecognised plan narrows nothing: the provider decides, and quota stays
    /// unknown.
    pub fn account_serves_model(&self, plan: Option<&str>, model: &str) -> bool {
        self.account_plan(plan)
            .is_none_or(|row| model.starts_with(row.model_prefix))
    }
    /// Whether a quota window governs a model drawing on `model_pool`. Only
    /// adapters with grouped quota pools separate windows by their `group.`
    /// prefix. Ungrouped windows, an unset model pool and other adapters keep
    /// every window, which is the conservative reading.
    pub fn window_serves_pool(&self, window: &str, model_pool: Option<&str>) -> bool {
        match (self.quota.grouped_pools, model_pool, window.split_once('.')) {
            (true, Some(pool), Some((group, _))) => group == pool,
            _ => true,
        }
    }
    pub fn get(name: &str) -> Option<&'static Self> {
        ADAPTER_SPECS.iter().find(|spec| spec.identity.name == name)
    }
    /// Strip this adapter's declared model-id namespace, when it has one.
    pub fn normalise_model_id<'a>(&self, id: &'a str) -> &'a str {
        self.identity
            .model_id_prefix
            .and_then(|prefix| id.strip_prefix(prefix))
            .unwrap_or(id)
    }
    /// Prefix a registered key built from a provider identity carries.
    pub fn registered_key_prefix(&self) -> Option<&'static str> {
        self.accounts.key_prefix
    }
    /// Build a registered key from a provider identity string.
    pub fn account_key(&self, identity: &str) -> String {
        let Some(prefix) = self.accounts.key_prefix else {
            return identity.to_owned();
        };
        let identity = self
            .accounts
            .identity_namespace
            .and_then(|namespace| identity.strip_prefix(namespace))
            .unwrap_or(identity);
        format!("{prefix}{identity}")
    }
}

/// Strip whichever declared namespace a model id carries. Namespaces are
/// distinct, so the registry alone decides how ids are normalised.
pub fn normalise_model_id(id: &str) -> &str {
    ADAPTER_SPECS
        .iter()
        .filter_map(|spec| spec.identity.model_id_prefix)
        .find_map(|prefix| id.strip_prefix(prefix))
        .unwrap_or(id)
}

// The installed Provenant release carries the common portfolio used by each
// provider. Linked skill targets outside these roots remain eligible too.
const SKILL_INSTALL_ROOTS: &[&str] = &[concat!(
    provenant_core::default_state_suffix!(),
    "/releases"
)];

// Keep the rows in Harness order. CLI installation also uses this order.
adapter_specs! { $;
    Claude [] (claude: 1) {
        identity: AdapterIdentity {
            name: "claude",
            family: "anthropic",
            binary: "claude",
            update: UpdateCommand::Subcommand("update"),
            model_listing: None,
            default_models: None,
            model_id_prefix: None,
            model_discovery: None,
            namespaced_models: false,
            offered_models: false,
            named_entry: NamedEntry::None,
        },
        launch: AdapterLaunch {
            framing: StreamFraming::ProviderJsonl,
            config_environment: &[],
            argv_prompt: false,
            shell_prefix: Some(("bin/claude-shell", "CLAUDE_CODE_SHELL_PREFIX")),
            context_directory_arg: Some("--add-dir"),
            ignore_files: &[],
            indexing_ignore_files: &[],
            compaction_env: Some("CLAUDE_CODE_AUTO_COMPACT_WINDOW"),
            client_env_markers: Some(ClientEnvMarkers {
                equals_one: Some("CLAUDECODE"),
                presence: &["CLAUDE_CODE_SESSION_ID", "CLAUDE_SESSION_ID"],
            }),
            skill_location: Some(SkillLocation {
                environment: None,
                fallback: ".claude",
                suffix: "skills",
            }),
            skill_install_roots: SKILL_INSTALL_ROOTS,
            session_handoff: true,
            launch_token_refresh: true,
            codex_policy: false,
            github_auth: Some(GithubAuth::Environment),
            exit_route: false,
            stderr_config_notices: false,
            stderr_quota: false,
            stdout_error_diagnostic: false,
            deduplicate_result: false,
            oversize_command_frames: false,
            answer_from_text: true,
            result_event_usage: true,
            usage: UsageSemantics::new(UsageScope::Cumulative, false, true),
            token_budget_notice: None,
            chair_switch: true,
            drop_unrecorded_effort: false,
        },
        accounts: AdapterAccounts {
            default_directory: None,
            default_root_kind: &provenant_core::CredentialKind::Store,
            registered: true,
            config_dir: true,
            directory_prefix: Some(".claude-"),
            key_prefix: None,
            identity_namespace: None,
            shared_settings: true,
            isolated_login_keychain: false,
            local_entry: ("default", "local"),
            setup_root: SetupRoot::None,
            peer_bootstrap: true,
            model_plans: &[],
        },
        quota: AdapterQuota {
            probe: QuotaProbeKind::HttpGet,
            monthly_allowance: false,
            credits: true,
            grouped_pools: false,
            retain_accounts: false,
            identity_check: false,
            model_prefix: None,
            free_pool: None,
            refusal_reset_source: "provider reset",
            refusal_revalidation_seconds: None,
            pacing_keys: None,
            allowance_prefix: None,
            allowance_windows: &[],
        },
        login: AdapterLogin {
            local: false,
            reprobe: LoginReprobe::AccountIdentity,
            observation: LoginObservation::Reprobe,
            check_names_account: true,
            identity_probe: AccountIdentityProbe::CliAuthStatus,
            authentication: None,
            native_auth_warning: NativeAuthWarning::Always,
            credential_files: &[".credentials.json", "settings.json"],
            credential_auxiliary_files: &["provenant-lane-token", "provenant-oauth-refresh.lock"],
            credential_profile_sibling: true,
            credential_host_keychain: true,
            hint: "claude auth login",
            template: Some("CLAUDE_CONFIG_DIR={directory} claude auth login"),
            refresh_template: Some("CLAUDE_CONFIG_DIR={directory} claude"),
            auth_markers: &["not logged in", "authentication_failed"],
        },
        install: Some(HarnessInstall {
            home: ".claude",
            lane_home: ("CLAUDE_CONFIG_DIR", ""),
            config: ".claude.json",
            default_home_config: Some(".claude.json"),
            instruction_file: Some("CLAUDE.md"),
            hook_file: Some("settings.json"),
            hook_settings_arg: Some("--settings"),
            native: NativeCapabilities {
                hooks: AdapterFact::supported(),
                answer_delivery: AdapterFact::supported(),
                precompact_context: AdapterFact::supported(),
                instructions: AdapterFact::supported(),
            },
            hook_protocol: Some(HookProtocol::CommandGroups { timeout_unit: HookTimeoutUnit::Seconds }),
            hook_events: CLAUDE_HOOK_EVENTS,
            instruction_array: false,
            source: "https://docs.anthropic.com/en/docs/claude-code/settings; https://docs.anthropic.com/en/docs/claude-code/hooks",
        }),
        capabilities: Capabilities {
            launch: LaunchCapabilities {
                model: AdapterFact::supported(),
                default_network: NetworkAccess::Open,
                effort: AdapterFact::supported(),
                resume: AdapterFact::supported(),
                network: ALL_NETWORK,
                image_input: AdapterFact::supported(),
                write: AdapterFact::supported(),
            },
            control: ControlCapabilities {
                lane: ControlSupport::ContinuationOnly,
                chair: ControlSupport::MailOnly,
            },
            terminal_event: AdapterFact::row(TerminalEventStrength::Weak),
            answering_model: AdapterFact::row(AnsweringModelSource::Stream),
            usage_attribution: AdapterFact::row(UsageAttribution::Attempt),
            live_input: AdapterFact::unsupported("no live mid-run input is implemented for this adapter", ADAPTER_ROW_SOURCE),
            session_reference: AdapterFact::row(SessionReference::Reported),
            caveats: &[],
        },
        retention: Retention { transcript_layout: AdapterFact::supported(), artifacts: &[] , relocated_session_state: true, sessions_only: false, transcript_account_root: true },
    },
    Codex [] (codex: 0) {
        identity: AdapterIdentity {
            name: "codex",
            family: "openai",
            binary: "codex",
            update: UpdateCommand::HomebrewOrNpm {
                formula: "codex",
                module: "@openai/codex",
                package: "@openai/codex@latest",
            },
            model_listing: Some(ModelListingFormat::CodexJson),
            default_models: None,
            model_id_prefix: None,
            model_discovery: Some(&["debug", "models"]),
            namespaced_models: false,
            offered_models: false,
            named_entry: NamedEntry::None,
        },
        launch: AdapterLaunch {
            framing: StreamFraming::ProviderJsonl,
            config_environment: &[],
            argv_prompt: false,
            shell_prefix: None,
            context_directory_arg: None,
            ignore_files: &[],
            indexing_ignore_files: &[],
            compaction_env: None,
            client_env_markers: Some(ClientEnvMarkers {
                equals_one: None,
                presence: &["CODEX_THREAD_ID"],
            }),
            skill_location: Some(SkillLocation {
                environment: None,
                fallback: ".agents",
                suffix: "skills",
            }),
            skill_install_roots: SKILL_INSTALL_ROOTS,
            session_handoff: false,
            launch_token_refresh: false,
            codex_policy: true,
            github_auth: Some(GithubAuth::ShellEnvironment),
            exit_route: false,
            stderr_config_notices: true,
            stderr_quota: false,
            stdout_error_diagnostic: false,
            deduplicate_result: false,
            oversize_command_frames: true,
            answer_from_text: false,
            result_event_usage: false,
            usage: UsageSemantics::new(UsageScope::Step, true, true),
            token_budget_notice: Some("Codex writer token budgets are checked at completed-turn boundaries; a turn cannot be interrupted mid-flight"),
            chair_switch: false,
            drop_unrecorded_effort: false,
        },
        accounts: AdapterAccounts {
            default_directory: Some(provenant_core::CODEX_ACCOUNT_DIRECTORY),
            default_root_kind: &provenant_core::CredentialKind::CodexHome,
            registered: true,
            config_dir: true,
            directory_prefix: None,
            key_prefix: Some("codex-"),
            identity_namespace: None,
            shared_settings: false,
            isolated_login_keychain: false,
            local_entry: ("default", "local"),
            setup_root: SetupRoot::None,
            peer_bootstrap: true,
            model_plans: &[],
        },
        quota: AdapterQuota {
            probe: QuotaProbeKind::Command,
            monthly_allowance: false,
            credits: true,
            grouped_pools: false,
            retain_accounts: false,
            identity_check: true,
            model_prefix: None,
            free_pool: None,
            refusal_reset_source: "provider reset",
            refusal_revalidation_seconds: None,
            pacing_keys: None,
            allowance_prefix: None,
            allowance_windows: &[],
        },
        login: AdapterLogin {
            local: false,
            reprobe: LoginReprobe::Codex,
            observation: LoginObservation::Reprobe,
            check_names_account: false,
            identity_probe: AccountIdentityProbe::AuthJson,
            authentication: Some("login"),
            native_auth_warning: NativeAuthWarning::WhenMismatch,
            credential_files: &["auth.json", "config.toml"],
            credential_auxiliary_files: &[],
            credential_profile_sibling: false,
            credential_host_keychain: false,
            hint: "codex login",
            template: Some("CODEX_HOME={directory} codex login"),
            refresh_template: None,
            auth_markers: &[
                "please log out and sign in again",
                "refresh token expired",
                "not logged in",
            ],
        },
        install: Some(HarnessInstall {
            home: ".codex",
            lane_home: ("CODEX_HOME", ""),
            config: "config.toml",
            default_home_config: None,
            instruction_file: Some("AGENTS.md"),
            hook_file: Some("hooks.json"),
            hook_settings_arg: None,
            native: NativeCapabilities {
                hooks: AdapterFact::supported(),
                answer_delivery: AdapterFact::supported(),
                precompact_context: AdapterFact::unsupported("Codex PreCompact accepts only a systemMessage UI notice, not model context", "https://developers.openai.com/codex/hooks"),
                instructions: AdapterFact::supported(),
            },
            hook_protocol: Some(HookProtocol::CommandGroups { timeout_unit: HookTimeoutUnit::Seconds }),
            hook_events: CODEX_HOOK_EVENTS,
            instruction_array: false,
            source: "https://learn.chatgpt.com/docs/hooks; https://developers.openai.com/codex/guides/agents-md",
        }),
        capabilities: Capabilities {
            launch: LaunchCapabilities {
                model: AdapterFact::supported(),
                default_network: NetworkAccess::Open,
                effort: AdapterFact::supported(),
                resume: AdapterFact::supported(),
                network: ALL_NETWORK,
                image_input: AdapterFact::supported(),
                write: AdapterFact::supported(),
            },
            control: ControlCapabilities {
                lane: ControlSupport::ContinuationOnly,
                chair: ControlSupport::MailOnly,
            },
            terminal_event: AdapterFact::row(TerminalEventStrength::Weak),
            answering_model: AdapterFact::row(AnsweringModelSource::Rollout),
            usage_attribution: AdapterFact::because(UsageAttribution::Session, "codex usage totals are session-wide unless marked per attempt", "crates/run/src/holder.rs"),
            live_input: AdapterFact::unsupported("no live mid-run input is implemented for this adapter", ADAPTER_ROW_SOURCE),
            session_reference: AdapterFact::row(SessionReference::Reported),
            caveats: &[],
        },
        retention: Retention { transcript_layout: AdapterFact::supported(), artifacts: &[] , relocated_session_state: false, sessions_only: true, transcript_account_root: false },
    },
    Cursor [] (cursor: 2) {
        identity: AdapterIdentity {
            name: "cursor",
            family: "cursor",
            binary: "cursor-agent",
            update: UpdateCommand::None,
            model_listing: Some(ModelListingFormat::CursorText),
            default_models: Some(&["auto", "composer-*", "grok-*"]),
            model_id_prefix: None,
            model_discovery: Some(&["models"]),
            namespaced_models: false,
            offered_models: false,
            named_entry: NamedEntry::None,
        },
        launch: AdapterLaunch {
            framing: StreamFraming::ProviderJsonl,
            config_environment: &[],
            argv_prompt: true,
            shell_prefix: None,
            context_directory_arg: None,
            ignore_files: &[".cursorignore"],
            indexing_ignore_files: &[".cursorindexingignore"],
            compaction_env: None,
            client_env_markers: None,
            skill_location: Some(SkillLocation {
                environment: None,
                fallback: ".cursor",
                suffix: "skills",
            }),
            skill_install_roots: SKILL_INSTALL_ROOTS,
            session_handoff: false,
            launch_token_refresh: false,
            codex_policy: false,
            github_auth: None,
            exit_route: false,
            stderr_config_notices: false,
            stderr_quota: false,
            stdout_error_diagnostic: false,
            deduplicate_result: false,
            oversize_command_frames: false,
            answer_from_text: true,
            result_event_usage: false,
            usage: UsageSemantics::new(UsageScope::Step, false, false),
            token_budget_notice: None,
            chair_switch: false,
            drop_unrecorded_effort: true,
        },
        accounts: AdapterAccounts {
            default_directory: None,
            default_root_kind: &provenant_core::CredentialKind::Store,
            registered: false,
            config_dir: false,
            directory_prefix: None,
            key_prefix: Some("cursor-"),
            identity_namespace: None,
            shared_settings: false,
            isolated_login_keychain: false,
            local_entry: ("default", "local"),
            setup_root: SetupRoot::Home,
            peer_bootstrap: false,
            model_plans: &[],
        },
        quota: AdapterQuota {
            probe: QuotaProbeKind::HttpGet,
            monthly_allowance: true,
            credits: false,
            grouped_pools: false,
            retain_accounts: false,
            identity_check: true,
            model_prefix: None,
            free_pool: None,
            refusal_reset_source: "provider reset",
            refusal_revalidation_seconds: None,
            pacing_keys: None,
            allowance_prefix: None,
            allowance_windows: &[],
        },
        login: AdapterLogin {
            local: true,
            reprobe: LoginReprobe::Cursor,
            observation: LoginObservation::Reprobe,
            check_names_account: false,
            identity_probe: AccountIdentityProbe::Vscdb,
            authentication: Some("login"),
            native_auth_warning: NativeAuthWarning::WhenMismatch,
            credential_files: &["state.vscdb", "state.vscdb-wal", "state.vscdb-shm", "cli-config.json"],
            credential_auxiliary_files: &[],
            credential_profile_sibling: false,
            credential_host_keychain: false,
            hint: "agent login",
            template: None,
            refresh_template: None,
            auth_markers: &[
                "authentication required. please run agent login",
                "not logged in",
            ],
        },
        install: Some(HarnessInstall {
            home: ".cursor",
            lane_home: ("HOME", ".cursor"),
            config: "mcp.json",
            default_home_config: None,
            instruction_file: None,
            hook_file: Some("hooks.json"),
            hook_settings_arg: None,
            native: NativeCapabilities {
                hooks: AdapterFact::supported(),
                answer_delivery: AdapterFact::unsupported("Cursor preToolUse does not accept the additionalContext response used by Provenant answer delivery", "https://cursor.com/docs/hooks#pretooluse"),
                precompact_context: AdapterFact::unsupported("Cursor preCompact accepts only user_message output shown to the user, not model context", "https://cursor.com/docs/hooks#precompact"),
                instructions: AdapterFact::unsupported("Cursor global instructions are settings-managed; AGENTS.md is project-root scoped", "https://docs.cursor.com/context/rules-for-ai"),
            },
            hook_protocol: Some(HookProtocol::CursorCommands { timeout_unit: HookTimeoutUnit::Seconds }),
            hook_events: CURSOR_HOOK_EVENTS,
            instruction_array: false,
            source: "https://docs.cursor.com/hooks; https://docs.cursor.com/context/rules-for-ai",
        }),
        capabilities: Capabilities {
            launch: LaunchCapabilities {
                model: AdapterFact::supported(),
                default_network: NetworkAccess::Open,
                effort: AdapterFact::supported(),
                resume: AdapterFact::supported(),
                network: ALL_NETWORK,
                image_input: AdapterFact::unsupported("cursor does not provide image input control", ADAPTER_ROW_SOURCE),
                write: AdapterFact::supported(),
            },
            control: ControlCapabilities {
                lane: ControlSupport::ContinuationOnly,
                chair: ControlSupport::MailOnly,
            },
            terminal_event: AdapterFact::row(TerminalEventStrength::Weak),
            answering_model: AdapterFact::row(AnsweringModelSource::Stream),
            usage_attribution: AdapterFact::row(UsageAttribution::Attempt),
            live_input: AdapterFact::unsupported("no live mid-run input is implemented for this adapter", ADAPTER_ROW_SOURCE),
            session_reference: AdapterFact::row(SessionReference::Reported),
            caveats: &[],
        },
        retention: Retention { transcript_layout: AdapterFact::supported(), artifacts: &[RetainedArtifact { parent: ".cursor", name: "chats", scrub: &[] }] , relocated_session_state: true, sessions_only: false, transcript_account_root: false },
    },
    Agy [] (agy: 3) {
        identity: AdapterIdentity {
            name: "agy",
            family: "google",
            binary: "agy",
            update: UpdateCommand::None,
            model_listing: Some(ModelListingFormat::AgyText),
            default_models: None,
            model_id_prefix: Some("agy/"),
            model_discovery: Some(&["models"]),
            namespaced_models: false,
            offered_models: true,
            named_entry: NamedEntry::None,
        },
        launch: AdapterLaunch {
            framing: StreamFraming::ProviderJsonl,
            config_environment: &[],
            argv_prompt: true,
            shell_prefix: None,
            context_directory_arg: None,
            ignore_files: &[],
            indexing_ignore_files: &[],
            compaction_env: None,
            client_env_markers: None,
            skill_location: Some(SkillLocation {
                environment: None,
                fallback: ".gemini",
                suffix: "skills",
            }),
            skill_install_roots: SKILL_INSTALL_ROOTS,
            session_handoff: false,
            launch_token_refresh: false,
            codex_policy: false,
            github_auth: None,
            exit_route: false,
            stderr_config_notices: false,
            stderr_quota: false,
            stdout_error_diagnostic: false,
            deduplicate_result: true,
            oversize_command_frames: false,
            answer_from_text: false,
            result_event_usage: false,
            usage: UsageSemantics::new(UsageScope::Step, false, false),
            token_budget_notice: None,
            chair_switch: false,
            drop_unrecorded_effort: false,
        },
        accounts: AdapterAccounts {
            default_directory: None,
            default_root_kind: &provenant_core::CredentialKind::Store,
            registered: true,
            config_dir: false,
            directory_prefix: None,
            key_prefix: Some("agy-google-"),
            identity_namespace: Some("google-"),
            shared_settings: false,
            isolated_login_keychain: true,
            local_entry: ("default", "local"),
            setup_root: SetupRoot::None,
            peer_bootstrap: false,
            model_plans: &[],
        },
        quota: AdapterQuota {
            probe: QuotaProbeKind::Command,
            monthly_allowance: false,
            credits: false,
            grouped_pools: true,
            retain_accounts: true,
            identity_check: false,
            model_prefix: None,
            free_pool: None,
            refusal_reset_source: "provider reset",
            refusal_revalidation_seconds: Some(300),
            pacing_keys: None,
            allowance_prefix: None,
            allowance_windows: &[],
        },
        login: AdapterLogin {
            local: true,
            reprobe: LoginReprobe::AccountIdentity,
            observation: LoginObservation::Reprobe,
            check_names_account: true,
            identity_probe: AccountIdentityProbe::KeychainLog,
            authentication: Some("keychain"),
            native_auth_warning: NativeAuthWarning::WhenMismatch,
            credential_files: &[
                "state.vscdb",
                "state.vscdb-wal",
                "Library/Keychains/login.keychain-db",
            ],
            credential_auxiliary_files: &[],
            credential_profile_sibling: false,
            credential_host_keychain: false,
            hint: "agy auth login",
            template: Some("HOME={directory} agy` and use `/login"),
            refresh_template: None,
            auth_markers: &[
                "authentication required. please login to continue.",
                "authentication required. please log in",
            ],
        },
        install: Some(HarnessInstall {
            home: ".gemini",
            lane_home: ("HOME", ".gemini"),
            config: "config/mcp_config.json",
            default_home_config: None,
            instruction_file: Some("AGENTS.md"),
            hook_file: Some("config/hooks.json"),
            hook_settings_arg: None,
            native: NativeCapabilities {
                hooks: AdapterFact::supported(),
                answer_delivery: AdapterFact::unsupported("Antigravity PreToolUse returns a tool decision and argument overwrite, not injected context", "https://antigravity.google/docs/hooks#input-and-output-contract"),
                precompact_context: AdapterFact::unsupported("Antigravity exposes no PreCompact event", "https://antigravity.google/docs/hooks#supported-events"),
                instructions: AdapterFact::supported(),
            },
            hook_protocol: Some(HookProtocol::NamedGroups { timeout_unit: HookTimeoutUnit::Seconds }),
            hook_events: AGY_HOOK_EVENTS,
            instruction_array: false,
            source: "https://antigravity.google/docs/hooks; https://antigravity.google/docs/customization-discovery",
        }),
        capabilities: Capabilities {
            launch: LaunchCapabilities {
                model: AdapterFact::supported(),
                default_network: NetworkAccess::Open,
                effort: AdapterFact::supported(),
                resume: AdapterFact::supported(),
                network: ALL_NETWORK,
                image_input: AdapterFact::unsupported("agy does not provide image input control", ADAPTER_ROW_SOURCE),
                write: AdapterFact::supported(),
            },
            control: ControlCapabilities {
                lane: ControlSupport::ContinuationOnly,
                chair: ControlSupport::MailOnly,
            },
            terminal_event: AdapterFact::row(TerminalEventStrength::Weak),
            answering_model: AdapterFact::row(AnsweringModelSource::Stream),
            usage_attribution: AdapterFact::row(UsageAttribution::Attempt),
            live_input: AdapterFact::unsupported("no live mid-run input is implemented for this adapter", ADAPTER_ROW_SOURCE),
            session_reference: AdapterFact::row(SessionReference::Reported),
            caveats: &[],
        },
        retention: Retention { transcript_layout: AdapterFact::unsupported("transcript layout not established; the payload is kept as history", "crates/run/src/retention.rs"), artifacts: &[] , relocated_session_state: false, sessions_only: false, transcript_account_root: false },
    },
    Opencode [] (opencode: 4) {
        identity: AdapterIdentity {
            name: "opencode",
            family: "opencode",
            binary: "opencode",
            update: UpdateCommand::Subcommand("upgrade"),
            model_listing: Some(ModelListingFormat::OpencodeText),
            default_models: None,
            model_id_prefix: Some("opencode/"),
            model_discovery: Some(&["models"]),
            namespaced_models: true,
            offered_models: false,
            named_entry: NamedEntry::None,
        },
        launch: AdapterLaunch {
            framing: StreamFraming::ProviderJsonl,
            config_environment: &["XDG_DATA_HOME"],
            argv_prompt: false,
            shell_prefix: None,
            context_directory_arg: None,
            ignore_files: &[],
            indexing_ignore_files: &[],
            compaction_env: None,
            client_env_markers: None,
            skill_location: Some(SkillLocation {
                environment: Some("XDG_CONFIG_HOME"),
                fallback: ".config",
                suffix: "opencode/skills",
            }),
            skill_install_roots: SKILL_INSTALL_ROOTS,
            session_handoff: false,
            launch_token_refresh: false,
            codex_policy: false,
            github_auth: None,
            exit_route: false,
            stderr_config_notices: false,
            stderr_quota: true,
            stdout_error_diagnostic: true,
            deduplicate_result: false,
            oversize_command_frames: false,
            answer_from_text: false,
            result_event_usage: false,
            usage: UsageSemantics::new(UsageScope::Step, false, false),
            token_budget_notice: None,
            chair_switch: false,
            drop_unrecorded_effort: true,
        },
        accounts: AdapterAccounts {
            default_directory: None,
            default_root_kind: &provenant_core::CredentialKind::Store,
            registered: false,
            config_dir: false,
            directory_prefix: None,
            key_prefix: None,
            identity_namespace: None,
            shared_settings: false,
            isolated_login_keychain: false,
            local_entry: ("go", "go"),
            setup_root: SetupRoot::XdgDataHome,
            peer_bootstrap: false,
            model_plans: &[
                AccountModelPlan { plan: "go", model_prefix: "opencode-go/", quota: AccountQuota::LocalLedger },
                AccountModelPlan { plan: "zen", model_prefix: "opencode/", quota: AccountQuota::ObservedRefusals },
            ],
        },
        quota: AdapterQuota {
            probe: QuotaProbeKind::LocalLedger,
            monthly_allowance: false,
            credits: true,
            grouped_pools: false,
            retain_accounts: false,
            identity_check: false,
            model_prefix: Some("opencode-go/"),
            free_pool: Some("opencode-free"),
            refusal_reset_source: "provider retry-after",
            refusal_revalidation_seconds: None,
            pacing_keys: Some(("opencode_free", "opencode_paid")),
            allowance_prefix: Some("opencode-go/"),
            allowance_windows: &["five_hour", "seven_day", "thirty_day"],
        },
        login: AdapterLogin {
            local: true,
            reprobe: LoginReprobe::ExecutableOnly,
            observation: LoginObservation::ExecutableOnly,
            check_names_account: false,
            identity_probe: AccountIdentityProbe::None,
            authentication: Some("login"),
            native_auth_warning: NativeAuthWarning::None,
            credential_files: &[
                "auth.json",
                "opencode.db",
                "opencode.db-wal",
                "opencode.json",
            ],
            credential_auxiliary_files: &[],
            credential_profile_sibling: false,
            credential_host_keychain: false,
            hint: "opencode auth login",
            template: None,
            refresh_template: None,
            auth_markers: &["no api key found", "run opencode auth login"],
        },
        install: Some(HarnessInstall {
            home: ".config/opencode",
            lane_home: ("XDG_CONFIG_HOME", "opencode"),
            config: "opencode.jsonc",
            default_home_config: None,
            instruction_file: Some("AGENTS.md"),
            hook_file: Some("plugins/provenant/index.ts"),
            hook_settings_arg: None,
            native: NativeCapabilities {
                hooks: AdapterFact::supported(),
                answer_delivery: AdapterFact::unsupported("OpenCode tool.execute.before hooks cannot inject additional context into the active session", "https://opencode.ai/docs/build/plugins/server#tool-execution"),
                precompact_context: AdapterFact::supported(),
                instructions: AdapterFact::supported(),
            },
            hook_protocol: Some(HookProtocol::OpenCodePlugin),
            hook_events: OPENCODE_HOOK_EVENTS,
            instruction_array: true,
            source: "https://opencode.ai/docs/build/plugins/; https://opencode.ai/docs/instructions",
        }),
        capabilities: Capabilities {
            launch: LaunchCapabilities {
                model: AdapterFact::supported(),
                default_network: NetworkAccess::Open,
                effort: AdapterFact::supported(),
                resume: AdapterFact::supported(),
                network: ALL_NETWORK,
                image_input: AdapterFact::unsupported("opencode does not provide image input control", ADAPTER_ROW_SOURCE),
                write: AdapterFact::supported(),
            },
            control: ControlCapabilities {
                lane: ControlSupport::ContinuationOnly,
                chair: ControlSupport::MailOnly,
            },
            terminal_event: AdapterFact::row(TerminalEventStrength::Weak),
            answering_model: AdapterFact::row(AnsweringModelSource::SessionDatabase),
            usage_attribution: AdapterFact::row(UsageAttribution::Attempt),
            live_input: AdapterFact::unsupported("no live mid-run input is implemented for this adapter", ADAPTER_ROW_SOURCE),
            session_reference: AdapterFact::row(SessionReference::Reported),
            caveats: &[],
        },
        retention: Retention { transcript_layout: AdapterFact::supported(), artifacts: &[RetainedArtifact { parent: "data", name: "opencode", scrub: &["auth.json"] }] , relocated_session_state: true, sessions_only: false, transcript_account_root: false },
    },
    Command [] (command: 5) {
        identity: AdapterIdentity {
            name: "command",
            family: "command",
            binary: "",
            update: UpdateCommand::None,
            model_listing: None,
            default_models: None,
            model_id_prefix: None,
            model_discovery: None,
            namespaced_models: false,
            offered_models: false,
            named_entry: NamedEntry::Command,
        },
        launch: AdapterLaunch {
            framing: StreamFraming::Raw,
            config_environment: &[],
            argv_prompt: false,
            shell_prefix: None,
            context_directory_arg: None,
            ignore_files: &[],
            indexing_ignore_files: &[],
            compaction_env: None,
            client_env_markers: None,
            skill_location: None,
            skill_install_roots: &[],
            session_handoff: false,
            launch_token_refresh: false,
            codex_policy: false,
            github_auth: None,
            exit_route: true,
            stderr_config_notices: false,
            stderr_quota: false,
            stdout_error_diagnostic: false,
            deduplicate_result: false,
            oversize_command_frames: false,
            answer_from_text: false,
            result_event_usage: false,
            usage: UsageSemantics::new(UsageScope::Step, false, false),
            token_budget_notice: None,
            chair_switch: false,
            drop_unrecorded_effort: false,
        },
        accounts: AdapterAccounts {
            default_directory: None,
            default_root_kind: &provenant_core::CredentialKind::Store,
            registered: false,
            config_dir: false,
            directory_prefix: None,
            key_prefix: None,
            identity_namespace: None,
            shared_settings: false,
            isolated_login_keychain: false,
            local_entry: ("default", "local"),
            setup_root: SetupRoot::None,
            peer_bootstrap: false,
            model_plans: &[],
        },
        quota: AdapterQuota {
            probe: QuotaProbeKind::None,
            monthly_allowance: false,
            credits: false,
            grouped_pools: false,
            retain_accounts: false,
            identity_check: false,
            model_prefix: None,
            free_pool: None,
            refusal_reset_source: "provider reset",
            refusal_revalidation_seconds: None,
            pacing_keys: None,
            allowance_prefix: None,
            allowance_windows: &[],
        },
        login: AdapterLogin {
            local: false,
            reprobe: LoginReprobe::None,
            observation: LoginObservation::None,
            check_names_account: false,
            identity_probe: AccountIdentityProbe::None,
            authentication: None,
            native_auth_warning: NativeAuthWarning::None,
            credential_files: &[],
            credential_auxiliary_files: &[],
            credential_profile_sibling: false,
            credential_host_keychain: false,
            hint: "",
            template: None,
            refresh_template: None,
            auth_markers: &[],
        },
        install: None,
        capabilities: Capabilities {
            launch: LaunchCapabilities {
                model: AdapterFact::unsupported("command does not provide model control", ADAPTER_ROW_SOURCE),
                default_network: NetworkAccess::Open,
                effort: AdapterFact::unsupported("command does not provide effort control", ADAPTER_ROW_SOURCE),
                resume: AdapterFact::unsupported("command does not provide resume control", ADAPTER_ROW_SOURCE),
                network: ALL_NETWORK,
                image_input: AdapterFact::unsupported("command does not provide image input control", ADAPTER_ROW_SOURCE),
                write: AdapterFact::unsupported("command does not provide write control", ADAPTER_ROW_SOURCE),
            },
            control: ControlCapabilities {
                lane: ControlSupport::Unsupported,
                chair: ControlSupport::Unsupported,
            },
            terminal_event: AdapterFact::because(TerminalEventStrength::Absent, "process exit is the contract for raw output", ADAPTER_ROW_SOURCE),
            answering_model: AdapterFact::because(AnsweringModelSource::Unreported, "configured commands report no model", ADAPTER_ROW_SOURCE),
            usage_attribution: AdapterFact::row(UsageAttribution::Attempt),
            live_input: AdapterFact::unsupported("no live mid-run input is implemented for this adapter", ADAPTER_ROW_SOURCE),
            session_reference: AdapterFact::because(SessionReference::Absent, "raw output carries no session id", ADAPTER_ROW_SOURCE),
            caveats: &[],
        },
        retention: Retention { transcript_layout: AdapterFact::unsupported("transcript layout not established; the payload is kept as history", "crates/run/src/retention.rs"), artifacts: &[] , relocated_session_state: false, sessions_only: false, transcript_account_root: false },
    },
    // A named, optional host feature. Preparation delegates to its provider;
    // decoding, accounts and confinement remain owned by that provider.
    HostNative [serde(rename = "host-native")] () {
        identity: AdapterIdentity {
            name: "host-native",
            family: "host-native",
            binary: "",
            update: UpdateCommand::None,
            model_listing: None,
            default_models: None,
            model_id_prefix: None,
            model_discovery: None,
            namespaced_models: false,
            offered_models: false,
            named_entry: NamedEntry::DelegatedAgent,
        },
        launch: AdapterLaunch {
            framing: StreamFraming::ProviderJsonl,
            config_environment: &[],
            argv_prompt: false,
            shell_prefix: None,
            context_directory_arg: None,
            ignore_files: &[],
            indexing_ignore_files: &[],
            compaction_env: None,
            client_env_markers: None,
            skill_location: None,
            skill_install_roots: &[],
            session_handoff: false,
            launch_token_refresh: false,
            codex_policy: false,
            github_auth: None,
            exit_route: false,
            stderr_config_notices: false,
            stderr_quota: false,
            stdout_error_diagnostic: false,
            deduplicate_result: false,
            oversize_command_frames: false,
            answer_from_text: false,
            result_event_usage: false,
            usage: UsageSemantics::new(UsageScope::Step, false, false),
            token_budget_notice: None,
            chair_switch: false,
            drop_unrecorded_effort: false,
        },
        accounts: AdapterAccounts {
            default_directory: None,
            default_root_kind: &provenant_core::CredentialKind::Store,
            registered: false,
            config_dir: false,
            directory_prefix: None,
            key_prefix: None,
            identity_namespace: None,
            shared_settings: false,
            isolated_login_keychain: false,
            local_entry: ("default", "local"),
            setup_root: SetupRoot::None,
            peer_bootstrap: false,
            model_plans: &[],
        },
        quota: AdapterQuota {
            probe: QuotaProbeKind::None,
            monthly_allowance: false,
            credits: false,
            grouped_pools: false,
            retain_accounts: false,
            identity_check: false,
            model_prefix: None,
            free_pool: None,
            refusal_reset_source: "provider reset",
            refusal_revalidation_seconds: None,
            pacing_keys: None,
            allowance_prefix: None,
            allowance_windows: &[],
        },
        login: AdapterLogin {
            local: false,
            reprobe: LoginReprobe::None,
            observation: LoginObservation::None,
            check_names_account: false,
            identity_probe: AccountIdentityProbe::None,
            authentication: None,
            native_auth_warning: NativeAuthWarning::None,
            credential_files: &[],
            credential_auxiliary_files: &[],
            credential_profile_sibling: false,
            credential_host_keychain: false,
            hint: "",
            template: None,
            refresh_template: None,
            auth_markers: &[],
        },
        install: None,
        capabilities: Capabilities {
            launch: LaunchCapabilities {
                model: AdapterFact::unsupported("host-native does not provide model control", ADAPTER_ROW_SOURCE),
                default_network: NetworkAccess::Open,
                effort: AdapterFact::unsupported("host-native does not provide effort control", ADAPTER_ROW_SOURCE),
                resume: AdapterFact::unsupported("host-native does not provide resume control", ADAPTER_ROW_SOURCE),
                network: ALL_NETWORK,
                image_input: AdapterFact::unsupported("host-native does not provide image input control", ADAPTER_ROW_SOURCE),
                write: AdapterFact::supported(),
            },
            control: ControlCapabilities {
                lane: ControlSupport::Unsupported,
                chair: ControlSupport::Unsupported,
            },
            terminal_event: AdapterFact::row(TerminalEventStrength::Weak),
            answering_model: AdapterFact::row(AnsweringModelSource::Stream),
            usage_attribution: AdapterFact::row(UsageAttribution::Attempt),
            live_input: AdapterFact::unsupported("no live mid-run input is implemented for this adapter", ADAPTER_ROW_SOURCE),
            session_reference: AdapterFact::because(SessionReference::Absent, "the delegated adapter reports no provider session id", ADAPTER_ROW_SOURCE),
            caveats: &[],
        },
        retention: Retention { transcript_layout: AdapterFact::unsupported("transcript layout not established; the payload is kept as history", "crates/run/src/retention.rs"), artifacts: &[] , relocated_session_state: false, sessions_only: false, transcript_account_root: false },
    },
    #[cfg(feature = "registry-fixture")]
    FixtureProvider [serde(rename = "fixture-provider")] () {
        identity: AdapterIdentity {
            name: "fixture-provider",
            family: "fixture",
            binary: "fixture-provider",
            update: UpdateCommand::None,
            model_listing: None,
            default_models: None,
            model_id_prefix: None,
            model_discovery: None,
            namespaced_models: false,
            offered_models: false,
            named_entry: NamedEntry::None,
        },
        launch: AdapterLaunch {
            framing: StreamFraming::ProviderJsonl,
            config_environment: &[],
            argv_prompt: false,
            shell_prefix: None,
            context_directory_arg: None,
            ignore_files: &[],
            indexing_ignore_files: &[],
            compaction_env: None,
            client_env_markers: None,
            skill_location: None,
            skill_install_roots: &[],
            session_handoff: false,
            launch_token_refresh: false,
            codex_policy: false,
            github_auth: None,
            exit_route: true,
            stderr_config_notices: false,
            stderr_quota: false,
            stdout_error_diagnostic: false,
            deduplicate_result: false,
            oversize_command_frames: false,
            answer_from_text: false,
            result_event_usage: false,
            usage: UsageSemantics::new(UsageScope::Step, false, false),
            token_budget_notice: None,
            chair_switch: false,
            drop_unrecorded_effort: false,
        },
        accounts: AdapterAccounts {
            default_directory: None,
            default_root_kind: &provenant_core::CredentialKind::Store,
            registered: false,
            config_dir: false,
            directory_prefix: None,
            key_prefix: None,
            identity_namespace: None,
            shared_settings: false,
            isolated_login_keychain: false,
            local_entry: ("fixture", "work"),
            setup_root: SetupRoot::None,
            peer_bootstrap: false,
            model_plans: &[],
        },
        quota: AdapterQuota {
            probe: QuotaProbeKind::None,
            monthly_allowance: false,
            credits: false,
            grouped_pools: false,
            retain_accounts: false,
            identity_check: false,
            model_prefix: None,
            free_pool: None,
            refusal_reset_source: "provider reset",
            refusal_revalidation_seconds: None,
            pacing_keys: None,
            allowance_prefix: None,
            allowance_windows: &[],
        },
        login: AdapterLogin {
            local: true,
            reprobe: LoginReprobe::None,
            observation: LoginObservation::None,
            check_names_account: false,
            identity_probe: AccountIdentityProbe::None,
            authentication: None,
            native_auth_warning: NativeAuthWarning::None,
            credential_files: &[],
            credential_auxiliary_files: &[],
            credential_profile_sibling: false,
            credential_host_keychain: false,
            hint: "",
            template: None,
            refresh_template: None,
            auth_markers: &[],
        },
        install: None,
        capabilities: Capabilities {
            launch: LaunchCapabilities {
                model: AdapterFact::supported(),
                default_network: NetworkAccess::Open,
                effort: AdapterFact::unsupported("fixture-provider does not provide effort control", ADAPTER_ROW_SOURCE),
                resume: AdapterFact::unsupported("fixture-provider does not provide resume control", ADAPTER_ROW_SOURCE),
                network: ALL_NETWORK,
                image_input: AdapterFact::unsupported("fixture-provider does not provide image input control", ADAPTER_ROW_SOURCE),
                write: AdapterFact::unsupported("fixture-provider does not provide write control", ADAPTER_ROW_SOURCE),
            },
            control: ControlCapabilities {
                lane: ControlSupport::Unsupported,
                chair: ControlSupport::Unsupported,
            },
            terminal_event: AdapterFact::row(TerminalEventStrength::Weak),
            answering_model: AdapterFact::row(AnsweringModelSource::Stream),
            usage_attribution: AdapterFact::row(UsageAttribution::Attempt),
            live_input: AdapterFact::unsupported("no live mid-run input is implemented for this adapter", ADAPTER_ROW_SOURCE),
            session_reference: AdapterFact::because(SessionReference::Absent, "the delegated adapter reports no provider session id", ADAPTER_ROW_SOURCE),
            caveats: &[],
        },
        retention: Retention { transcript_layout: AdapterFact::unsupported("transcript layout not established; the payload is kept as history", "crates/run/src/retention.rs"), artifacts: &[] , relocated_session_state: false, sessions_only: false, transcript_account_root: false },
    },
}

#[cfg(test)]
#[path = "adapters_tests.rs"]
mod adapters_tests;
