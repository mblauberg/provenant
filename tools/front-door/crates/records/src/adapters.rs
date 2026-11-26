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
pub enum ModelObservation {
    Stream,
    Rollout,
    SessionDatabase,
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

pub const ROW_SOURCE: &str = "crates/records/src/adapters.rs";
pub const ROW_REASON: &str = "declared by the adapter row";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Declaration<T> {
    pub value: T,
    pub reason: &'static str,
    pub source: &'static str,
}
impl<T> Declaration<T> {
    pub const fn row(value: T) -> Self {
        Self {
            value,
            reason: ROW_REASON,
            source: ROW_SOURCE,
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
impl Declaration<bool> {
    pub const fn supported() -> Self {
        Self::row(true)
    }
    pub const fn unsupported(reason: &'static str, source: &'static str) -> Self {
        Self::because(false, reason, source)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalEvent {
    Weak,
    None,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageAttribution {
    Turn,
    Session,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetainedArtifact {
    pub parent: &'static str,
    pub name: &'static str,
    pub scrub: &'static [&'static str],
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retention {
    pub transcripts: Declaration<bool>,
    pub artifacts: &'static [RetainedArtifact],
    /// Sessions formerly stored in host configuration now require private attempt state.
    pub relocated_session_state: bool,
    /// Copy only session files when retaining private state.
    pub copy_sessions_only: bool,
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
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    pub instruction: Option<&'static str>,
    pub hook_file: Option<&'static str>,
    /// Provider CLI option that loads a generated hook settings document.
    pub hook_settings_arg: Option<&'static str>,
    pub hooks: Declaration<bool>,
    pub hook_protocol: Option<HookProtocol>,
    pub hook_events: &'static [HookEvent],
    pub answer_delivery: Declaration<bool>,
    /// Whether a pre-compaction response reaches the model's context.
    pub precompact_context: Declaration<bool>,
    pub instructions: Declaration<bool>,
    /// The adapter declares its instruction documents as an array in its config.
    pub instruction_array: bool,
    pub source: &'static str,
}

const CHAIR_HOOKS_NOT_APPLICABLE: &str =
    "Provenant chair switching is enabled only for adapters declaring chair_switch";
const CHAIR_HOOKS_SOURCE: &str = "AdapterSpec.chair_switch in crates/records/src/adapters.rs";
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
pub enum StreamContract {
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
    pub stream: StreamContract,
    pub guardian_environment: &'static [&'static str],
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
    pub skills: Option<SkillLocation>,
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
    pub drop_unsupported_effort: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct AdapterAccounts {
    /// Environment override and home-relative default for an implicitly discovered account.
    pub default_account_directory: Option<(&'static str, &'static str)>,
    /// Credential policy of the declared default directory at its home-relative path.
    pub default_account_root_kind: &'static provenant_core::CredentialKind,
    pub registered_account: bool,
    /// A registered account's directory is the client's configuration
    /// directory, which account settings sync keeps in step. False where it is
    /// something else: agy's is the account's keychain home.
    pub account_config_dir: bool,
    /// Fixed home-relative prefix for registered account directories, if required.
    pub account_directory_prefix: Option<&'static str>,
    /// Prefix of a registered key derived from the signed-in identity, for
    /// adapters that namespace their account keys.
    pub account_key_prefix: Option<&'static str>,
    /// Namespace carried inside a provider identity string and folded into the
    /// registered key by `account_key`.
    pub account_identity_namespace: Option<&'static str>,
    /// Registered account configuration supports shared settings overlays.
    pub shared_account_settings: bool,
    /// Registered accounts use an isolated login keychain rooted in their home.
    pub isolated_login_keychain: bool,
    /// Synthetic account label and key for an adapter using its local login.
    pub local_account: (&'static str, &'static str),
    pub setup_root: SetupRoot,
    pub quota_probe: QuotaProbeKind,
    /// Missing monthly observations constrain allowance pacing for this adapter.
    pub monthly_allowance: bool,
    pub quota_credits: bool,
    pub grouped_quota_pools: bool,
    pub retain_quota_accounts: bool,
    pub quota_identity_check: bool,
    pub peer_bootstrap: bool,
    /// A snapshot governs only this model prefix; None governs every model.
    pub quota_model_prefix: Option<&'static str>,
    /// Registered plans partition model namespaces independently of account keys.
    pub account_model_plans: &'static [AccountModelPlan],
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
    pub local_login: bool,
    pub login_reprobe: LoginReprobe,
    pub login_observation: LoginObservation,
    /// The login recheck identifies the signed-in account.
    pub login_check_names_account: bool,
    /// How a registered account's identity is read from its directory.
    pub account_identity_probe: AccountIdentityProbe,
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
    pub login_hint: &'static str,
    /// Account-scoped interactive login command, with {directory} shell quoted.
    pub login_template: Option<&'static str>,
    /// Account-scoped interactive command for refreshing an expired login.
    /// `{directory}` is shell quoted by the caller.
    pub login_refresh_template: Option<&'static str>,
    /// Lowercase provider stderr fragments indicating a missing or expired login.
    pub auth_markers: &'static [&'static str],
}

#[derive(Debug, Clone, Copy)]
pub struct AdapterSpec {
    pub id: Harness,
    pub identity: AdapterIdentity,
    pub launch: AdapterLaunch,
    pub accounts: AdapterAccounts,
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
    pub model: Declaration<bool>,
    pub effort: Declaration<bool>,
    pub image_input: Declaration<bool>,
    pub resume: Declaration<bool>,
    pub write: Declaration<bool>,
    pub network: &'static [NetworkAccess],
    pub default_network: NetworkAccess,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub launch: LaunchCapabilities,
    pub control: ControlCapabilities,
    pub terminal_event: Declaration<TerminalEvent>,
    pub answering_model: Declaration<ModelObservation>,
    pub usage_attribution: Declaration<UsageAttribution>,
    pub live_input: Declaration<bool>,
}
#[derive(Debug, Clone, Copy, Default)]
pub struct LaunchRequest<'a> {
    pub model: Option<&'a str>,
    pub effort: Option<&'a str>,
    pub image_input: bool,
    pub resume: bool,
    pub network: Option<NetworkAccess>,
    pub write: bool,
    pub skill_location: bool,
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
    pub fn decision(self) -> LaunchAction {
        match self {
            Self::Model | Self::Effort | Self::ImageInput => LaunchAction::Degrade,
            _ => LaunchAction::Refuse,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchDecision {
    pub adapter: &'static str,
    pub field: LaunchField,
    pub action: LaunchAction,
    pub reason: &'static str,
    pub source: &'static str,
}
impl std::fmt::Display for LaunchDecision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} does not support {}: {}; source={}",
            self.adapter,
            self.field.name(),
            self.reason,
            self.source
        )
    }
}
impl std::error::Error for LaunchDecision {}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchCheck {
    pub decisions: Vec<LaunchDecision>,
}
impl LaunchCheck {
    pub fn refusal(&self) -> Option<&LaunchDecision> {
        self.decisions
            .iter()
            .find(|d| d.action == LaunchAction::Refuse)
    }
    pub fn degraded(&self, field: LaunchField) -> Option<&LaunchDecision> {
        self.decisions
            .iter()
            .find(|d| d.field == field && d.action == LaunchAction::Degrade)
    }
}

impl AdapterSpec {
    /// Whether account entries describe metadata for one shared local login
    /// instead of separate registered credential trees.
    pub fn uses_shared_local_login(&self) -> bool {
        !self.accounts.registered_account && self.login.local_login
    }

    /// Whether `key` names the synthetic entry of an unregistered local login.
    /// Only an adapter that has both registered accounts and a local login (agy)
    /// has one: it runs on the user's own login, so nothing is registered, probed
    /// or pinned, and registered keys (`agy-<fingerprint>`) never collide with it.
    pub fn is_unregistered_local(&self, key: &str) -> bool {
        self.accounts.registered_account && self.login.local_login && key == "local"
    }
    /// Check a request against the declared table before any side effect.
    pub fn check_launch(&self, request: &LaunchRequest<'_>) -> LaunchCheck {
        let caps = &self.capabilities.launch;
        let mut decisions = Vec::new();
        let mut add = |field: LaunchField, reason, source| {
            decisions.push(LaunchDecision {
                adapter: self.identity.name,
                field,
                action: field.decision(),
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
                ROW_SOURCE,
            );
        }
        if request.write && !caps.write.value {
            add(LaunchField::Write, caps.write.reason, caps.write.source);
        }
        if request.skill_location && self.launch.skills.is_none() {
            add(
                LaunchField::SkillLocation,
                "the adapter declares no skill location",
                ROW_SOURCE,
            );
        }
        LaunchCheck { decisions }
    }
    pub fn account_plan(&self, plan: Option<&str>) -> Option<&AccountModelPlan> {
        self.accounts
            .account_model_plans
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
        match (
            self.accounts.grouped_quota_pools,
            model_pool,
            window.split_once('.'),
        ) {
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
        self.accounts.account_key_prefix
    }
    /// Build a registered key from a provider identity string.
    pub fn account_key(&self, identity: &str) -> String {
        let Some(prefix) = self.accounts.account_key_prefix else {
            return identity.to_owned();
        };
        let identity = self
            .account_identity_namespace
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
            stream: StreamContract::ProviderJsonl,
            guardian_environment: &[],
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
            skills: Some(SkillLocation {
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
            drop_unsupported_effort: false,
        },
        accounts: AdapterAccounts {
            default_account_directory: None,
            default_account_root_kind: &provenant_core::CredentialKind::Store,
            registered_account: true,
            account_config_dir: true,
            account_directory_prefix: Some(".claude-"),
            account_key_prefix: None,
            account_identity_namespace: None,
            shared_account_settings: true,
            isolated_login_keychain: false,
            local_account: ("default", "local"),
            setup_root: SetupRoot::None,
            quota_probe: QuotaProbeKind::HttpGet,
            monthly_allowance: false,
            quota_credits: true,
            grouped_quota_pools: false,
            retain_quota_accounts: false,
            quota_identity_check: false,
            peer_bootstrap: true,
            quota_model_prefix: None,
            account_model_plans: &[],
            free_pool: None,
            refusal_reset_source: "provider reset",
            refusal_revalidation_seconds: None,
            pacing_keys: None,
            allowance_prefix: None,
            allowance_windows: &[],
        },
        login: AdapterLogin {
            local_login: false,
            login_reprobe: LoginReprobe::AccountIdentity,
            login_observation: LoginObservation::Reprobe,
            login_check_names_account: true,
            account_identity_probe: AccountIdentityProbe::CliAuthStatus,
            authentication: None,
            native_auth_warning: NativeAuthWarning::Always,
            credential_files: &[".credentials.json", "settings.json"],
            credential_auxiliary_files: &["provenant-lane-token", "provenant-oauth-refresh.lock"],
            credential_profile_sibling: true,
            credential_host_keychain: true,
            login_hint: "claude auth login",
            login_template: Some("CLAUDE_CONFIG_DIR={directory} claude auth login"),
            login_refresh_template: Some("CLAUDE_CONFIG_DIR={directory} claude"),
            auth_markers: &["not logged in", "authentication_failed"],
        },
        install: Some(HarnessInstall {
            home: ".claude",
            lane_home: ("CLAUDE_CONFIG_DIR", ""),
            config: ".claude.json",
            default_home_config: Some(".claude.json"),
            instruction: Some("CLAUDE.md"),
            hook_file: Some("settings.json"),
            hook_settings_arg: Some("--settings"),
            hooks: Declaration::supported(),
            hook_protocol: Some(HookProtocol::CommandGroups { timeout_unit: HookTimeoutUnit::Seconds }),
            hook_events: CLAUDE_HOOK_EVENTS,
            answer_delivery: Declaration::supported(),
            precompact_context: Declaration::supported(),
            instructions: Declaration::supported(),
            instruction_array: false,
            source: "https://docs.anthropic.com/en/docs/claude-code/settings; https://docs.anthropic.com/en/docs/claude-code/hooks",
        }),
        capabilities: Capabilities {
            launch: LaunchCapabilities {
                model: Declaration::supported(),
                default_network: NetworkAccess::Open,
                effort: Declaration::supported(),
                resume: Declaration::supported(),
                network: ALL_NETWORK,
                image_input: Declaration::supported(),
                write: Declaration::supported(),
            },
            control: ControlCapabilities {
                lane: ControlSupport::ContinuationOnly,
                chair: ControlSupport::MailOnly,
            },
            terminal_event: Declaration::row(TerminalEvent::Weak),
            answering_model: Declaration::row(ModelObservation::Stream),
            usage_attribution: Declaration::row(UsageAttribution::Turn),
            live_input: Declaration::unsupported("no live mid-run input is implemented for this adapter", ROW_SOURCE),
        },
        retention: Retention { transcripts: Declaration::supported(), artifacts: &[] , relocated_session_state: true, copy_sessions_only: false, transcript_account_root: true },
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
            stream: StreamContract::ProviderJsonl,
            guardian_environment: &[],
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
            skills: Some(SkillLocation {
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
            drop_unsupported_effort: false,
        },
        accounts: AdapterAccounts {
            default_account_directory: Some(provenant_core::CODEX_ACCOUNT_DIRECTORY),
            default_account_root_kind: &provenant_core::CredentialKind::CodexHome,
            registered_account: true,
            account_config_dir: true,
            account_directory_prefix: None,
            account_key_prefix: Some("codex-"),
            account_identity_namespace: None,
            shared_account_settings: false,
            isolated_login_keychain: false,
            local_account: ("default", "local"),
            setup_root: SetupRoot::None,
            quota_probe: QuotaProbeKind::Command,
            monthly_allowance: false,
            quota_credits: true,
            grouped_quota_pools: false,
            retain_quota_accounts: false,
            quota_identity_check: true,
            peer_bootstrap: true,
            quota_model_prefix: None,
            account_model_plans: &[],
            free_pool: None,
            refusal_reset_source: "provider reset",
            refusal_revalidation_seconds: None,
            pacing_keys: None,
            allowance_prefix: None,
            allowance_windows: &[],
        },
        login: AdapterLogin {
            local_login: false,
            login_reprobe: LoginReprobe::Codex,
            login_observation: LoginObservation::Reprobe,
            login_check_names_account: false,
            account_identity_probe: AccountIdentityProbe::AuthJson,
            authentication: Some("login"),
            native_auth_warning: NativeAuthWarning::WhenMismatch,
            credential_files: &["auth.json", "config.toml"],
            credential_auxiliary_files: &[],
            credential_profile_sibling: false,
            credential_host_keychain: false,
            login_hint: "codex login",
            login_template: Some("CODEX_HOME={directory} codex login"),
            login_refresh_template: None,
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
            instruction: Some("AGENTS.md"),
            hook_file: Some("hooks.json"),
            hook_settings_arg: None,
            hooks: Declaration::supported(),
            hook_protocol: Some(HookProtocol::CommandGroups { timeout_unit: HookTimeoutUnit::Seconds }),
            hook_events: CODEX_HOOK_EVENTS,
            answer_delivery: Declaration::supported(),
            precompact_context: Declaration::unsupported("Codex PreCompact accepts only a systemMessage UI notice, not model context", "https://developers.openai.com/codex/hooks"),
            instructions: Declaration::supported(),
            instruction_array: false,
            source: "https://learn.chatgpt.com/docs/hooks; https://developers.openai.com/codex/guides/agents-md",
        }),
        capabilities: Capabilities {
            launch: LaunchCapabilities {
                model: Declaration::supported(),
                default_network: NetworkAccess::Open,
                effort: Declaration::supported(),
                resume: Declaration::supported(),
                network: ALL_NETWORK,
                image_input: Declaration::supported(),
                write: Declaration::supported(),
            },
            control: ControlCapabilities {
                lane: ControlSupport::ContinuationOnly,
                chair: ControlSupport::MailOnly,
            },
            terminal_event: Declaration::row(TerminalEvent::Weak),
            answering_model: Declaration::row(ModelObservation::Rollout),
            usage_attribution: Declaration::because(UsageAttribution::Session, "codex usage totals are session-wide unless marked per attempt", "crates/run/src/holder.rs"),
            live_input: Declaration::unsupported("no live mid-run input is implemented for this adapter", ROW_SOURCE),
        },
        retention: Retention { transcripts: Declaration::supported(), artifacts: &[] , relocated_session_state: false, copy_sessions_only: true, transcript_account_root: false },
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
            stream: StreamContract::ProviderJsonl,
            guardian_environment: &[],
            argv_prompt: true,
            shell_prefix: None,
            context_directory_arg: None,
            ignore_files: &[".cursorignore"],
            indexing_ignore_files: &[".cursorindexingignore"],
            compaction_env: None,
            client_env_markers: None,
            skills: Some(SkillLocation {
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
            drop_unsupported_effort: true,
        },
        accounts: AdapterAccounts {
            default_account_directory: None,
            default_account_root_kind: &provenant_core::CredentialKind::Store,
            registered_account: false,
            account_config_dir: false,
            account_directory_prefix: None,
            account_key_prefix: Some("cursor-"),
            account_identity_namespace: None,
            shared_account_settings: false,
            isolated_login_keychain: false,
            local_account: ("default", "local"),
            setup_root: SetupRoot::Home,
            quota_probe: QuotaProbeKind::HttpGet,
            monthly_allowance: true,
            quota_credits: false,
            grouped_quota_pools: false,
            retain_quota_accounts: false,
            quota_identity_check: true,
            peer_bootstrap: false,
            quota_model_prefix: None,
            account_model_plans: &[],
            free_pool: None,
            refusal_reset_source: "provider reset",
            refusal_revalidation_seconds: None,
            pacing_keys: None,
            allowance_prefix: None,
            allowance_windows: &[],
        },
        login: AdapterLogin {
            local_login: true,
            login_reprobe: LoginReprobe::Cursor,
            login_observation: LoginObservation::Reprobe,
            login_check_names_account: false,
            account_identity_probe: AccountIdentityProbe::Vscdb,
            authentication: Some("login"),
            native_auth_warning: NativeAuthWarning::WhenMismatch,
            credential_files: &["state.vscdb", "state.vscdb-wal", "state.vscdb-shm", "cli-config.json"],
            credential_auxiliary_files: &[],
            credential_profile_sibling: false,
            credential_host_keychain: false,
            login_hint: "agent login",
            login_template: None,
            login_refresh_template: None,
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
            instruction: None,
            hook_file: Some("hooks.json"),
            hook_settings_arg: None,
            hooks: Declaration::supported(),
            hook_protocol: Some(HookProtocol::CursorCommands { timeout_unit: HookTimeoutUnit::Seconds }),
            hook_events: CURSOR_HOOK_EVENTS,
            answer_delivery: Declaration::unsupported("Cursor preToolUse does not accept the additionalContext response used by Provenant answer delivery", "https://cursor.com/docs/hooks#pretooluse"),
            precompact_context: Declaration::unsupported("Cursor preCompact accepts only user_message output shown to the user, not model context", "https://cursor.com/docs/hooks#precompact"),
            instructions: Declaration::unsupported("Cursor global instructions are settings-managed; AGENTS.md is project-root scoped", "https://docs.cursor.com/context/rules-for-ai"),
            instruction_array: false,
            source: "https://docs.cursor.com/hooks; https://docs.cursor.com/context/rules-for-ai",
        }),
        capabilities: Capabilities {
            launch: LaunchCapabilities {
                model: Declaration::supported(),
                default_network: NetworkAccess::Open,
                effort: Declaration::supported(),
                resume: Declaration::supported(),
                network: ALL_NETWORK,
                image_input: Declaration::unsupported("cursor does not provide image input control", ROW_SOURCE),
                write: Declaration::supported(),
            },
            control: ControlCapabilities {
                lane: ControlSupport::ContinuationOnly,
                chair: ControlSupport::MailOnly,
            },
            terminal_event: Declaration::row(TerminalEvent::Weak),
            answering_model: Declaration::row(ModelObservation::Stream),
            usage_attribution: Declaration::row(UsageAttribution::Turn),
            live_input: Declaration::unsupported("no live mid-run input is implemented for this adapter", ROW_SOURCE),
        },
        retention: Retention { transcripts: Declaration::supported(), artifacts: &[RetainedArtifact { parent: ".cursor", name: "chats", scrub: &[] }] , relocated_session_state: true, copy_sessions_only: false, transcript_account_root: false },
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
            stream: StreamContract::ProviderJsonl,
            guardian_environment: &[],
            argv_prompt: true,
            shell_prefix: None,
            context_directory_arg: None,
            ignore_files: &[],
            indexing_ignore_files: &[],
            compaction_env: None,
            client_env_markers: None,
            skills: Some(SkillLocation {
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
            drop_unsupported_effort: false,
        },
        accounts: AdapterAccounts {
            default_account_directory: None,
            default_account_root_kind: &provenant_core::CredentialKind::Store,
            registered_account: true,
            account_config_dir: false,
            account_directory_prefix: None,
            account_key_prefix: Some("agy-google-"),
            account_identity_namespace: Some("google-"),
            shared_account_settings: false,
            isolated_login_keychain: true,
            local_account: ("default", "local"),
            setup_root: SetupRoot::None,
            quota_probe: QuotaProbeKind::Command,
            monthly_allowance: false,
            quota_credits: false,
            grouped_quota_pools: true,
            retain_quota_accounts: true,
            quota_identity_check: false,
            peer_bootstrap: false,
            quota_model_prefix: None,
            account_model_plans: &[],
            free_pool: None,
            refusal_reset_source: "provider reset",
            refusal_revalidation_seconds: Some(300),
            pacing_keys: None,
            allowance_prefix: None,
            allowance_windows: &[],
        },
        login: AdapterLogin {
            local_login: true,
            login_reprobe: LoginReprobe::AccountIdentity,
            login_observation: LoginObservation::Reprobe,
            login_check_names_account: true,
            account_identity_probe: AccountIdentityProbe::KeychainLog,
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
            login_hint: "agy auth login",
            login_template: Some("HOME={directory} agy` and use `/login"),
            login_refresh_template: None,
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
            instruction: Some("AGENTS.md"),
            hook_file: Some("config/hooks.json"),
            hook_settings_arg: None,
            hooks: Declaration::supported(),
            hook_protocol: Some(HookProtocol::NamedGroups { timeout_unit: HookTimeoutUnit::Seconds }),
            hook_events: AGY_HOOK_EVENTS,
            answer_delivery: Declaration::unsupported("Antigravity PreToolUse returns a tool decision and argument overwrite, not injected context", "https://antigravity.google/docs/hooks#input-and-output-contract"),
            precompact_context: Declaration::unsupported("Antigravity exposes no PreCompact event", "https://antigravity.google/docs/hooks#supported-events"),
            instructions: Declaration::supported(),
            instruction_array: false,
            source: "https://antigravity.google/docs/hooks; https://antigravity.google/docs/customization-discovery",
        }),
        capabilities: Capabilities {
            launch: LaunchCapabilities {
                model: Declaration::supported(),
                default_network: NetworkAccess::Open,
                effort: Declaration::supported(),
                resume: Declaration::supported(),
                network: ALL_NETWORK,
                image_input: Declaration::unsupported("agy does not provide image input control", ROW_SOURCE),
                write: Declaration::supported(),
            },
            control: ControlCapabilities {
                lane: ControlSupport::ContinuationOnly,
                chair: ControlSupport::MailOnly,
            },
            terminal_event: Declaration::row(TerminalEvent::Weak),
            answering_model: Declaration::row(ModelObservation::Stream),
            usage_attribution: Declaration::row(UsageAttribution::Turn),
            live_input: Declaration::unsupported("no live mid-run input is implemented for this adapter", ROW_SOURCE),
        },
        retention: Retention { transcripts: Declaration::unsupported("transcript layout not established; the payload is kept as history", "crates/run/src/retention.rs"), artifacts: &[] , relocated_session_state: false, copy_sessions_only: false, transcript_account_root: false },
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
            stream: StreamContract::ProviderJsonl,
            guardian_environment: &["XDG_DATA_HOME"],
            argv_prompt: false,
            shell_prefix: None,
            context_directory_arg: None,
            ignore_files: &[],
            indexing_ignore_files: &[],
            compaction_env: None,
            client_env_markers: None,
            skills: Some(SkillLocation {
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
            drop_unsupported_effort: true,
        },
        accounts: AdapterAccounts {
            default_account_directory: None,
            default_account_root_kind: &provenant_core::CredentialKind::Store,
            registered_account: false,
            account_config_dir: false,
            account_directory_prefix: None,
            account_key_prefix: None,
            account_identity_namespace: None,
            shared_account_settings: false,
            isolated_login_keychain: false,
            local_account: ("go", "go"),
            setup_root: SetupRoot::XdgDataHome,
            quota_probe: QuotaProbeKind::LocalLedger,
            monthly_allowance: false,
            quota_credits: true,
            grouped_quota_pools: false,
            retain_quota_accounts: false,
            quota_identity_check: false,
            peer_bootstrap: false,
            quota_model_prefix: Some("opencode-go/"),
            account_model_plans: &[
                AccountModelPlan { plan: "go", model_prefix: "opencode-go/", quota: AccountQuota::LocalLedger },
                AccountModelPlan { plan: "zen", model_prefix: "opencode/", quota: AccountQuota::ObservedRefusals },
            ],
            free_pool: Some("opencode-free"),
            refusal_reset_source: "provider retry-after",
            refusal_revalidation_seconds: None,
            pacing_keys: Some(("opencode_free", "opencode_paid")),
            allowance_prefix: Some("opencode-go/"),
            allowance_windows: &["five_hour", "seven_day", "thirty_day"],
        },
        login: AdapterLogin {
            local_login: true,
            login_reprobe: LoginReprobe::ExecutableOnly,
            login_observation: LoginObservation::ExecutableOnly,
            login_check_names_account: false,
            account_identity_probe: AccountIdentityProbe::None,
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
            login_hint: "opencode auth login",
            login_template: None,
            login_refresh_template: None,
            auth_markers: &["no api key found", "run opencode auth login"],
        },
        install: Some(HarnessInstall {
            home: ".config/opencode",
            lane_home: ("XDG_CONFIG_HOME", "opencode"),
            config: "opencode.jsonc",
            default_home_config: None,
            instruction: Some("AGENTS.md"),
            hook_file: Some("plugins/provenant/index.ts"),
            hook_settings_arg: None,
            hooks: Declaration::supported(),
            hook_protocol: Some(HookProtocol::OpenCodePlugin),
            hook_events: OPENCODE_HOOK_EVENTS,
            answer_delivery: Declaration::unsupported("OpenCode tool.execute.before hooks cannot inject additional context into the active session", "https://opencode.ai/docs/build/plugins/server#tool-execution"),
            precompact_context: Declaration::supported(),
            instructions: Declaration::supported(),
            instruction_array: true,
            source: "https://opencode.ai/docs/build/plugins/; https://opencode.ai/docs/instructions",
        }),
        capabilities: Capabilities {
            launch: LaunchCapabilities {
                model: Declaration::supported(),
                default_network: NetworkAccess::Open,
                effort: Declaration::supported(),
                resume: Declaration::supported(),
                network: ALL_NETWORK,
                image_input: Declaration::unsupported("opencode does not provide image input control", ROW_SOURCE),
                write: Declaration::supported(),
            },
            control: ControlCapabilities {
                lane: ControlSupport::ContinuationOnly,
                chair: ControlSupport::MailOnly,
            },
            terminal_event: Declaration::row(TerminalEvent::Weak),
            answering_model: Declaration::row(ModelObservation::SessionDatabase),
            usage_attribution: Declaration::row(UsageAttribution::Turn),
            live_input: Declaration::unsupported("no live mid-run input is implemented for this adapter", ROW_SOURCE),
        },
        retention: Retention { transcripts: Declaration::supported(), artifacts: &[RetainedArtifact { parent: "data", name: "opencode", scrub: &["auth.json"] }] , relocated_session_state: true, copy_sessions_only: false, transcript_account_root: false },
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
            stream: StreamContract::Raw,
            guardian_environment: &[],
            argv_prompt: false,
            shell_prefix: None,
            context_directory_arg: None,
            ignore_files: &[],
            indexing_ignore_files: &[],
            compaction_env: None,
            client_env_markers: None,
            skills: None,
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
            drop_unsupported_effort: false,
        },
        accounts: AdapterAccounts {
            default_account_directory: None,
            default_account_root_kind: &provenant_core::CredentialKind::Store,
            registered_account: false,
            account_config_dir: false,
            account_directory_prefix: None,
            account_key_prefix: None,
            account_identity_namespace: None,
            shared_account_settings: false,
            isolated_login_keychain: false,
            local_account: ("default", "local"),
            setup_root: SetupRoot::None,
            quota_probe: QuotaProbeKind::None,
            monthly_allowance: false,
            quota_credits: false,
            grouped_quota_pools: false,
            retain_quota_accounts: false,
            quota_identity_check: false,
            peer_bootstrap: false,
            quota_model_prefix: None,
            account_model_plans: &[],
            free_pool: None,
            refusal_reset_source: "provider reset",
            refusal_revalidation_seconds: None,
            pacing_keys: None,
            allowance_prefix: None,
            allowance_windows: &[],
        },
        login: AdapterLogin {
            local_login: false,
            login_reprobe: LoginReprobe::None,
            login_observation: LoginObservation::None,
            login_check_names_account: false,
            account_identity_probe: AccountIdentityProbe::None,
            authentication: None,
            native_auth_warning: NativeAuthWarning::None,
            credential_files: &[],
            credential_auxiliary_files: &[],
            credential_profile_sibling: false,
            credential_host_keychain: false,
            login_hint: "",
            login_template: None,
            login_refresh_template: None,
            auth_markers: &[],
        },
        install: None,
        capabilities: Capabilities {
            launch: LaunchCapabilities {
                model: Declaration::unsupported("command does not provide model control", ROW_SOURCE),
                default_network: NetworkAccess::Open,
                effort: Declaration::unsupported("command does not provide effort control", ROW_SOURCE),
                resume: Declaration::unsupported("command does not provide resume control", ROW_SOURCE),
                network: ALL_NETWORK,
                image_input: Declaration::unsupported("command does not provide image input control", ROW_SOURCE),
                write: Declaration::unsupported("command does not provide write control", ROW_SOURCE),
            },
            control: ControlCapabilities {
                lane: ControlSupport::Unsupported,
                chair: ControlSupport::Unsupported,
            },
            terminal_event: Declaration::because(TerminalEvent::None, "process exit is the contract for raw output", ROW_SOURCE),
            answering_model: Declaration::because(ModelObservation::Stream, "configured commands report no model; stream is the neutral default", ROW_SOURCE),
            usage_attribution: Declaration::row(UsageAttribution::Turn),
            live_input: Declaration::unsupported("no live mid-run input is implemented for this adapter", ROW_SOURCE),
        },
        retention: Retention { transcripts: Declaration::unsupported("transcript layout not established; the payload is kept as history", "crates/run/src/retention.rs"), artifacts: &[] , relocated_session_state: false, copy_sessions_only: false, transcript_account_root: false },
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
            stream: StreamContract::ProviderJsonl,
            guardian_environment: &[],
            argv_prompt: false,
            shell_prefix: None,
            context_directory_arg: None,
            ignore_files: &[],
            indexing_ignore_files: &[],
            compaction_env: None,
            client_env_markers: None,
            skills: None,
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
            drop_unsupported_effort: false,
        },
        accounts: AdapterAccounts {
            default_account_directory: None,
            default_account_root_kind: &provenant_core::CredentialKind::Store,
            registered_account: false,
            account_config_dir: false,
            account_directory_prefix: None,
            account_key_prefix: None,
            account_identity_namespace: None,
            shared_account_settings: false,
            isolated_login_keychain: false,
            local_account: ("default", "local"),
            setup_root: SetupRoot::None,
            quota_probe: QuotaProbeKind::None,
            monthly_allowance: false,
            quota_credits: false,
            grouped_quota_pools: false,
            retain_quota_accounts: false,
            quota_identity_check: false,
            peer_bootstrap: false,
            quota_model_prefix: None,
            account_model_plans: &[],
            free_pool: None,
            refusal_reset_source: "provider reset",
            refusal_revalidation_seconds: None,
            pacing_keys: None,
            allowance_prefix: None,
            allowance_windows: &[],
        },
        login: AdapterLogin {
            local_login: false,
            login_reprobe: LoginReprobe::None,
            login_observation: LoginObservation::None,
            login_check_names_account: false,
            account_identity_probe: AccountIdentityProbe::None,
            authentication: None,
            native_auth_warning: NativeAuthWarning::None,
            credential_files: &[],
            credential_auxiliary_files: &[],
            credential_profile_sibling: false,
            credential_host_keychain: false,
            login_hint: "",
            login_template: None,
            login_refresh_template: None,
            auth_markers: &[],
        },
        install: None,
        capabilities: Capabilities {
            launch: LaunchCapabilities {
                model: Declaration::unsupported("host-native does not provide model control", ROW_SOURCE),
                default_network: NetworkAccess::Open,
                effort: Declaration::unsupported("host-native does not provide effort control", ROW_SOURCE),
                resume: Declaration::unsupported("host-native does not provide resume control", ROW_SOURCE),
                network: ALL_NETWORK,
                image_input: Declaration::unsupported("host-native does not provide image input control", ROW_SOURCE),
                write: Declaration::supported(),
            },
            control: ControlCapabilities {
                lane: ControlSupport::Unsupported,
                chair: ControlSupport::Unsupported,
            },
            terminal_event: Declaration::row(TerminalEvent::Weak),
            answering_model: Declaration::row(ModelObservation::Stream),
            usage_attribution: Declaration::row(UsageAttribution::Turn),
            live_input: Declaration::unsupported("no live mid-run input is implemented for this adapter", ROW_SOURCE),
        },
        retention: Retention { transcripts: Declaration::unsupported("transcript layout not established; the payload is kept as history", "crates/run/src/retention.rs"), artifacts: &[] , relocated_session_state: false, copy_sessions_only: false, transcript_account_root: false },
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
            stream: StreamContract::ProviderJsonl,
            guardian_environment: &[],
            argv_prompt: false,
            shell_prefix: None,
            context_directory_arg: None,
            ignore_files: &[],
            indexing_ignore_files: &[],
            compaction_env: None,
            client_env_markers: None,
            skills: None,
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
            drop_unsupported_effort: false,
        },
        accounts: AdapterAccounts {
            default_account_directory: None,
            default_account_root_kind: &provenant_core::CredentialKind::Store,
            registered_account: false,
            account_config_dir: false,
            account_directory_prefix: None,
            account_key_prefix: None,
            account_identity_namespace: None,
            shared_account_settings: false,
            isolated_login_keychain: false,
            local_account: ("fixture", "work"),
            setup_root: SetupRoot::None,
            quota_probe: QuotaProbeKind::None,
            monthly_allowance: false,
            quota_credits: false,
            grouped_quota_pools: false,
            retain_quota_accounts: false,
            quota_identity_check: false,
            peer_bootstrap: false,
            quota_model_prefix: None,
            account_model_plans: &[],
            free_pool: None,
            refusal_reset_source: "provider reset",
            refusal_revalidation_seconds: None,
            pacing_keys: None,
            allowance_prefix: None,
            allowance_windows: &[],
        },
        login: AdapterLogin {
            local_login: true,
            login_reprobe: LoginReprobe::None,
            login_observation: LoginObservation::None,
            login_check_names_account: false,
            account_identity_probe: AccountIdentityProbe::None,
            authentication: None,
            native_auth_warning: NativeAuthWarning::None,
            credential_files: &[],
            credential_auxiliary_files: &[],
            credential_profile_sibling: false,
            credential_host_keychain: false,
            login_hint: "",
            login_template: None,
            login_refresh_template: None,
            auth_markers: &[],
        },
        install: None,
        capabilities: Capabilities {
            launch: LaunchCapabilities {
                model: Declaration::supported(),
                default_network: NetworkAccess::Open,
                effort: Declaration::unsupported("fixture-provider does not provide effort control", ROW_SOURCE),
                resume: Declaration::unsupported("fixture-provider does not provide resume control", ROW_SOURCE),
                network: ALL_NETWORK,
                image_input: Declaration::unsupported("fixture-provider does not provide image input control", ROW_SOURCE),
                write: Declaration::unsupported("fixture-provider does not provide write control", ROW_SOURCE),
            },
            control: ControlCapabilities {
                lane: ControlSupport::Unsupported,
                chair: ControlSupport::Unsupported,
            },
            terminal_event: Declaration::row(TerminalEvent::Weak),
            answering_model: Declaration::row(ModelObservation::Stream),
            usage_attribution: Declaration::row(UsageAttribution::Turn),
            live_input: Declaration::unsupported("no live mid-run input is implemented for this adapter", ROW_SOURCE),
        },
        retention: Retention { transcripts: Declaration::unsupported("transcript layout not established; the payload is kept as history", "crates/run/src/retention.rs"), artifacts: &[] , relocated_session_state: false, copy_sessions_only: false, transcript_account_root: false },
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_and_session_facts_are_consistent() {
        for spec in ADAPTER_SPECS {
            assert!(
                !spec.retention.transcript_account_root || spec.accounts.registered_account,
                "{}",
                spec.identity.name
            );
            assert!(
                spec.accounts.account_directory_prefix.is_none()
                    || spec.accounts.registered_account,
                "{}",
                spec.identity.name
            );
            assert!(
                !spec.accounts.shared_account_settings
                    || (spec.accounts.registered_account && spec.accounts.account_config_dir),
                "{}",
                spec.identity.name
            );
            assert!(
                !spec.accounts.isolated_login_keychain || spec.login.local_login,
                "{}",
                spec.identity.name
            );
        }
    }

    #[test]
    fn host_native_declares_a_fresh_named_writer_contract() {
        let spec = AdapterSpec::get("host-native").unwrap();
        assert!(spec.identity.binary.is_empty() && !spec.accounts.registered_account);
        assert_eq!(
            spec.capabilities.launch.default_network,
            NetworkAccess::Open
        );
        assert!(
            spec.check_launch(&LaunchRequest {
                write: true,
                network: Some(NetworkAccess::Open),
                ..Default::default()
            })
            .decisions
            .is_empty()
        );
        for (request, field) in [
            (
                LaunchRequest {
                    model: Some("claude/model"),
                    ..Default::default()
                },
                LaunchField::Model,
            ),
            (
                LaunchRequest {
                    effort: Some("high"),
                    ..Default::default()
                },
                LaunchField::Effort,
            ),
            (
                LaunchRequest {
                    resume: true,
                    ..Default::default()
                },
                LaunchField::Resume,
            ),
        ] {
            assert_eq!(spec.check_launch(&request).decisions[0].field, field);
        }
    }

    #[test]
    fn managed_harness_capabilities_are_sourced_and_coherent() {
        for spec in ADAPTER_SPECS {
            let Some(install) = spec.install else {
                continue;
            };
            assert!(!install.home.is_empty(), "{}", spec.identity.name);
            assert!(!install.config.is_empty(), "{}", spec.identity.name);
            assert!(!install.source.is_empty(), "{}", spec.identity.name);
            assert_eq!(
                install.hooks.value,
                install.hook_file.is_some(),
                "{}",
                spec.identity.name
            );
            assert_eq!(
                install.instructions.value,
                install.instruction.is_some(),
                "{}",
                spec.identity.name
            );
            for support in [
                install.hooks,
                install.instructions,
                install.answer_delivery,
                install.precompact_context,
            ] {
                if !support.value {
                    let Declaration { reason, source, .. } = support;
                    assert!(!reason.trim().is_empty(), "{}", spec.identity.name);
                    assert!(!source.trim().is_empty(), "{}", spec.identity.name);
                }
            }
            assert_eq!(
                spec.capabilities.launch.default_network,
                NetworkAccess::Open,
                "{}",
                spec.identity.name
            );
            for network in ALL_NETWORK {
                assert!(
                    spec.capabilities.launch.network.contains(network),
                    "{}",
                    spec.identity.name
                );
            }
            assert!(
                spec.capabilities.launch.write.value,
                "managed providers share worktree-write capability"
            );
        }
    }

    #[test]
    fn adapter_specs_are_unique_and_match_typed_identities() {
        for spec in ADAPTER_SPECS {
            assert_eq!(
                ADAPTER_SPECS
                    .iter()
                    .filter(|row| row.name == spec.identity.name)
                    .count(),
                1
            );
            assert!(std::ptr::eq(spec.id.spec(), spec));
            assert!(std::ptr::eq(
                AdapterSpec::get(spec.identity.name).unwrap(),
                spec
            ));
            assert_eq!(serde_json::to_value(spec.id).unwrap(), spec.identity.name);
            assert_eq!(
                serde_json::from_value::<Harness>(serde_json::json!(spec.identity.name)).unwrap(),
                spec.id
            );
        }
        assert!(AdapterSpec::get("unknown").is_none());
    }
}

#[cfg(test)]
mod account_tests {
    use super::*;

    #[test]
    fn only_registered_adapters_declare_config_directory_accounts() {
        for spec in ADAPTER_SPECS {
            assert!(
                !spec.accounts.account_config_dir || spec.accounts.registered_account,
                "{}",
                spec.identity.name
            );
        }
        assert!(AdapterSpec::get("codex").unwrap().account_config_dir);
        assert!(!AdapterSpec::get("agy").unwrap().account_config_dir);
    }

    #[test]
    fn unregistered_local_accounts_follow_registered_local_login_facts() {
        for spec in ADAPTER_SPECS {
            let has_local_account = spec.accounts.registered_account && spec.login.local_login;
            for (key, expected) in [("local", has_local_account), ("fixture-account", false)] {
                assert_eq!(
                    spec.is_unregistered_local(key),
                    expected,
                    "{}: {key}",
                    spec.identity.name
                );
            }
        }
    }
}

#[cfg(test)]
mod account_plan_tests {
    #[test]
    fn registered_plan_partitions_follow_the_adapter_declaration() {
        for spec in super::ADAPTER_SPECS {
            for plan in spec.accounts.account_model_plans {
                let model = format!("{}fixture", plan.model_prefix);
                assert!(spec.account_serves_model(Some(plan.plan), &model));
                assert_eq!(
                    spec.is_free_plan(Some(plan.plan)),
                    plan.quota == super::AccountQuota::ObservedRefusals
                );
                assert!(spec.account_serves_model(None, &model));
                assert!(spec.account_serves_model(Some("unrecognised"), &model));
                for other in spec
                    .account_model_plans
                    .iter()
                    .filter(|other| other.plan != plan.plan)
                {
                    assert!(!spec.account_serves_model(Some(other.plan), &model));
                }
            }
        }
    }
}

#[cfg(test)]
mod launch_tests {
    use super::*;

    #[test]
    fn command_has_no_model_effort_session_or_write_capability() {
        let command = AdapterSpec::get("command").unwrap();
        assert!(!command.registered_account);
        assert_eq!(command.quota_probe, QuotaProbeKind::None);
        assert_eq!(
            command.capabilities.launch.default_network,
            NetworkAccess::Open
        );
        assert!(
            command
                .check_launch(&LaunchRequest::default())
                .decisions
                .is_empty()
        );
        let launch = command.capabilities.launch;
        for declaration in [
            launch.model,
            launch.effort,
            launch.image_input,
            launch.resume,
            launch.write,
        ] {
            assert!(!declaration.value, "{}", command.name);
            let Declaration { reason, source, .. } = declaration;
            assert!(!reason.trim().is_empty(), "{}", command.name);
            assert!(!source.trim().is_empty(), "{}", command.name);
        }
        assert_eq!(
            command.capabilities.usage_attribution.value,
            UsageAttribution::Turn
        );
        for (request, field) in [
            (
                LaunchRequest {
                    model: Some("placeholder"),
                    ..Default::default()
                },
                LaunchField::Model,
            ),
            (
                LaunchRequest {
                    effort: Some("high"),
                    ..Default::default()
                },
                LaunchField::Effort,
            ),
            (
                LaunchRequest {
                    resume: true,
                    ..Default::default()
                },
                LaunchField::Resume,
            ),
            (
                LaunchRequest {
                    write: true,
                    ..Default::default()
                },
                LaunchField::Write,
            ),
        ] {
            let check = command.check_launch(&request);
            let decision = &check.decisions[0];
            assert_eq!(decision.field, field);
            assert_eq!(decision.action, field.decision());
        }
    }

    #[test]
    fn launch_check_reports_all_gaps_in_field_order() {
        let spec = AdapterSpec {
            launch: AdapterLaunch {
                skills: None,
                ..ADAPTER_SPECS[0].launch
            },
            capabilities: Capabilities {
                launch: LaunchCapabilities {
                    model: Declaration::unsupported("no model", ROW_SOURCE),
                    effort: Declaration::unsupported("no effort", ROW_SOURCE),
                    image_input: Declaration::unsupported("no images", ROW_SOURCE),
                    resume: Declaration::unsupported("no resume", ROW_SOURCE),
                    network: &[],
                    write: Declaration::unsupported("no write", ROW_SOURCE),
                    ..ADAPTER_SPECS[0].capabilities.launch
                },
                ..ADAPTER_SPECS[0].capabilities
            },
            ..ADAPTER_SPECS[0]
        };
        let check = spec.check_launch(&LaunchRequest {
            model: Some("model"),
            effort: Some("high"),
            image_input: true,
            resume: true,
            network: Some(NetworkAccess::Open),
            write: true,
            skill_location: true,
        });
        let expected = [
            LaunchField::Model,
            LaunchField::Effort,
            LaunchField::ImageInput,
            LaunchField::Resume,
            LaunchField::Network,
            LaunchField::Write,
            LaunchField::SkillLocation,
        ];
        assert_eq!(
            check.decisions.iter().map(|d| d.field).collect::<Vec<_>>(),
            expected
        );
        for decision in &check.decisions {
            assert_eq!(decision.action, decision.field.decision());
            assert!(!decision.reason.is_empty() && !decision.source.is_empty());
        }
        assert_eq!(check.refusal().map(|d| d.field), Some(LaunchField::Resume));
        assert_eq!(
            check.degraded(LaunchField::ImageInput).map(|d| d.field),
            Some(LaunchField::ImageInput)
        );
    }

    #[test]
    fn launch_table_is_checked_per_adapter() {
        // (adapter, effort, resume, network, write, refused field)
        let effort = Some("high");
        let cases = [
            ("claude", effort, None),
            ("codex", effort, None),
            ("agy", effort, None),
            ("opencode", effort, None),
            ("cursor", effort, None),
            ("cursor", None, None),
        ];
        for (name, effort, refused) in cases {
            let spec = AdapterSpec::get(name).unwrap();
            let request = LaunchRequest {
                model: None,
                effort,
                resume: true,
                network: Some(NetworkAccess::Open),
                write: true,
                ..Default::default()
            };
            assert_eq!(
                spec.check_launch(&request)
                    .decisions
                    .first()
                    .map(|e| e.field),
                refused,
                "{name}"
            );
        }
    }

    #[test]
    fn refusal_names_the_adapter_and_field() {
        let decision = AdapterSpec::get("command")
            .unwrap()
            .check_launch(&LaunchRequest {
                effort: Some("low"),
                ..Default::default()
            })
            .decisions
            .remove(0);
        let text = decision.to_string();
        assert!(
            text.starts_with("command does not support effort: "),
            "{text}"
        );
        assert!(text.contains(decision.reason), "{text}");
        assert!(
            text.contains(&format!("source={}", decision.source)),
            "{text}"
        );
    }

    #[test]
    fn declared_gaps_decide_by_field() {
        for spec in ADAPTER_SPECS {
            let capabilities = spec.capabilities.launch;
            let cases = [
                (
                    LaunchRequest {
                        model: Some("fixture/model"),
                        ..Default::default()
                    },
                    capabilities.model.value,
                    LaunchField::Model,
                ),
                (
                    LaunchRequest {
                        effort: Some("high"),
                        ..Default::default()
                    },
                    capabilities.effort.value,
                    LaunchField::Effort,
                ),
                (
                    LaunchRequest {
                        image_input: true,
                        ..Default::default()
                    },
                    capabilities.image_input.value,
                    LaunchField::ImageInput,
                ),
                (
                    LaunchRequest {
                        resume: true,
                        ..Default::default()
                    },
                    capabilities.resume.value,
                    LaunchField::Resume,
                ),
                (
                    LaunchRequest {
                        write: true,
                        ..Default::default()
                    },
                    capabilities.write.value,
                    LaunchField::Write,
                ),
                (
                    LaunchRequest {
                        skill_location: true,
                        ..Default::default()
                    },
                    spec.launch.skills.is_some(),
                    LaunchField::SkillLocation,
                ),
            ];
            for (request, supported, field) in cases {
                if !supported {
                    let check = spec.check_launch(&request);
                    let decision = &check.decisions[0];
                    assert_eq!(decision.field, field, "{}", spec.identity.name);
                    assert_eq!(decision.action, field.decision(), "{}", spec.identity.name);
                    let text = decision.to_string();
                    assert!(
                        text.starts_with(&format!("{} does not support ", spec.identity.name)),
                        "{text}"
                    );
                    assert!(text.contains(field.name()), "{text}");
                    assert!(text.contains(decision.reason), "{text}");
                    assert!(
                        text.contains(&format!("source={}", decision.source)),
                        "{text}"
                    );
                }
            }
            for network in ALL_NETWORK {
                if !capabilities.network.contains(network) {
                    let decision = &spec
                        .check_launch(&LaunchRequest {
                            network: Some(*network),
                            ..Default::default()
                        })
                        .decisions[0];
                    assert_eq!(
                        decision.field,
                        LaunchField::Network,
                        "{}",
                        spec.identity.name
                    );
                    assert_eq!(
                        decision.action,
                        LaunchAction::Refuse,
                        "{}",
                        spec.identity.name
                    );
                }
            }
        }
    }
}
