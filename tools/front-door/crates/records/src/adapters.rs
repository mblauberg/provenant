//! Closed adapter identities and shared facts. Mechanics stay in their modules.
use crate::{
    control::{ControlCapabilities, ControlSupport},
    run::NetworkAccess,
};
use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkillLocation {
    pub environment: Option<&'static str>,
    pub fallback: &'static str,
    pub suffix: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct AccountModelPlan {
    pub plan: &'static str,
    pub model_prefix: &'static str,
    pub free: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct AdapterSpec {
    pub id: Harness,
    pub name: &'static str,
    /// Provider family used by declared route constraints.
    pub family: &'static str,
    pub binary: &'static str,
    pub skills: Option<SkillLocation>,
    /// Home-relative installation roots that can supply linked skills.
    /// They describe discovery provenance, not an eligibility allowlist.
    pub skill_install_roots: &'static [&'static str],
    /// Environment override and home-relative default for an implicitly discovered account.
    pub default_account_directory: Option<(&'static str, &'static str)>,
    pub registered_account: bool,
    /// A registered account's directory is the client's configuration
    /// directory, which account settings sync keeps in step. False where it is
    /// something else: agy's is the account's keychain home.
    pub account_config_dir: bool,
    /// Fixed home-relative prefix for registered account directories, if required.
    pub account_directory_prefix: Option<&'static str>,
    /// Registered account configuration supports shared settings overlays.
    pub shared_account_settings: bool,
    /// Registered accounts use an isolated login keychain rooted in their home.
    pub isolated_login_keychain: bool,
    /// Setup may mirror a transcript into the registered account directory.
    pub transcript_account_root: bool,
    pub setup_root: SetupRoot,
    pub local_login: bool,
    pub login_reprobe: LoginReprobe,
    pub login_observation: LoginObservation,
    /// The login recheck identifies the signed-in account.
    pub login_check_names_account: bool,
    /// Synthetic account label and key for an adapter using its local login.
    pub local_account: (&'static str, &'static str),
    pub argv_prompt: bool,
    pub authentication: Option<&'static str>,
    /// Files observed by metadata only, relative to the account directory.
    pub credential_files: &'static [&'static str],
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
    /// Shell wrapper path and environment selector, relative to scratch.
    pub shell_prefix: Option<(&'static str, &'static str)>,
    /// Provider argument granting file tools access to captured Git context.
    pub context_directory_arg: Option<&'static str>,
    pub compaction_env: Option<&'static str>,
    pub allowance_prefix: Option<&'static str>,
    /// Local allowance windows governing models under allowance_prefix.
    pub allowance_windows: &'static [&'static str],
    pub namespaced_models: bool,
    pub chair_switch: bool,
    /// The local hook can read pending answer offers for this adapter.
    pub local_offer_reader: bool,
    /// The account reports the models it offers; a launch naming another model is refused.
    pub offered_models: bool,
    pub github_auth: Option<GithubAuth>,
    /// Supports launching a transferred interactive session.
    pub session_handoff: bool,
    /// Refresh an account token through the authorised credential broker at launch.
    pub launch_token_refresh: bool,
    /// Remove a duplicated complete answer from the provider stream.
    pub deduplicate_result: bool,
    /// Diagnose absent usage from structured result events.
    pub result_event_usage: bool,
    /// Provider-specific explanation when writer token budgets cannot stop it.
    pub token_budget_notice: Option<&'static str>,
    /// Use the requested route as the observed route when a command exits.
    pub exit_route: bool,
    /// Classify exit-time stderr for quota refusals.
    pub stderr_quota: bool,
    /// Parse stdout error diagnostics for provider failure explanations.
    pub stdout_error_diagnostic: bool,
    /// Source of model identity after a run.
    pub model_observation: ModelObservation,
    /// Preparation mechanism for a model-less named entry.
    pub named_entry: NamedEntry,
    pub user_sessions: bool,
    /// Sessions formerly stored in host configuration now require private attempt state.
    pub relocated_session_state: bool,
    /// Copy only session files when retaining private state.
    pub copy_sessions_only: bool,
    pub quota_probe: QuotaProbeKind,
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
    /// Drop a valid but unrecorded model effort with a route warning.
    pub drop_unsupported_effort: bool,
    pub pacing_keys: Option<(&'static str, &'static str)>,
    /// What a launch may ask of this adapter. The one source for dispatch,
    /// `run`, resume and graph validation.
    pub launch: LaunchCapabilities,
}

const ALL_NETWORK: &[NetworkAccess] = &[
    NetworkAccess::Provider,
    NetworkAccess::Local,
    NetworkAccess::Open,
];

/// Declared launch capabilities. Unsupported enforced controls are refused
/// before admission; missing image input is advisory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaunchCapabilities {
    /// Accepts a model selector; false for configured command entries.
    pub model: bool,
    pub default_network: NetworkAccess,
    /// Takes a reasoning-effort control on the command line.
    pub effort: bool,
    /// Continues a recorded provider session.
    pub resume: bool,
    /// What a control message can do for this adapter, per launch kind.
    pub control: ControlCapabilities,
    /// Network modes the adapter's sandbox profile can enforce.
    pub network: &'static [NetworkAccess],
    /// Accepts image input in a prompt.
    pub image_input: bool,
    /// Runs with worktree-write access.
    pub write: bool,
}

/// The fields of a launch request an adapter can refuse.
#[derive(Debug, Clone, Copy, Default)]
pub struct LaunchRequest<'a> {
    pub model: Option<&'a str>,
    pub effort: Option<&'a str>,
    pub resume: bool,
    pub network: Option<NetworkAccess>,
    pub write: bool,
    pub skill_location: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchField {
    Model,
    Effort,
    Resume,
    Network,
    Write,
    SkillLocation,
}
impl LaunchField {
    pub fn name(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Effort => "effort",
            Self::Resume => "resume",
            Self::Network => "network",
            Self::Write => "write",
            Self::SkillLocation => "skill location",
        }
    }
    /// A fixed refusal for callers that cannot carry the adapter name.
    pub fn refusal(self) -> &'static str {
        match self {
            Self::Model => "model is not supported by the route's adapter",
            Self::Effort => "effort is not supported by the route's adapter",
            Self::Resume => "resume is not supported by the route's adapter",
            Self::Network => "network mode is not supported by the route's adapter",
            Self::Write => "worktree-write is not supported by the route's adapter",
            Self::SkillLocation => "skill location is not supported by the route's adapter",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedLaunch {
    pub adapter: &'static str,
    pub field: LaunchField,
}
impl std::fmt::Display for UnsupportedLaunch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} does not support {}", self.adapter, self.field.name())
    }
}
impl std::error::Error for UnsupportedLaunch {}

impl AdapterSpec {
    /// Whether `key` names the synthetic entry of an unregistered local login.
    /// Only an adapter that has both registered accounts and a local login (agy)
    /// has one: it runs on the user's own login, so nothing is registered, probed
    /// or pinned, and registered keys (`agy-<fingerprint>`) never collide with it.
    pub fn is_unregistered_local(&self, key: &str) -> bool {
        self.registered_account && self.local_login && key == "local"
    }
    /// Check a request against the declared table before any side effect.
    pub fn check_launch(&self, request: &LaunchRequest<'_>) -> Result<(), UnsupportedLaunch> {
        let caps = &self.launch;
        let field = if request.model.is_some() && !caps.model {
            Some(LaunchField::Model)
        } else if request.effort.is_some() && !caps.effort {
            Some(LaunchField::Effort)
        } else if request.resume && !caps.resume {
            Some(LaunchField::Resume)
        } else if request
            .network
            .is_some_and(|mode| !caps.network.contains(&mode))
        {
            Some(LaunchField::Network)
        } else if request.write && !caps.write {
            Some(LaunchField::Write)
        } else if request.skill_location && self.skills.is_none() {
            Some(LaunchField::SkillLocation)
        } else {
            None
        };
        field.map_or(Ok(()), |field| {
            Err(UnsupportedLaunch {
                adapter: self.name,
                field,
            })
        })
    }
    pub fn is_free_plan(&self, plan: Option<&str>) -> bool {
        self.account_model_plans.iter().any(|row| {
            row.free && plan.is_some_and(|plan| row.plan.eq_ignore_ascii_case(plan.trim()))
        })
    }
    pub fn account_serves_model(&self, plan: Option<&str>, model: &str) -> bool {
        self.account_model_plans.is_empty()
            || self.account_model_plans.iter().any(|row| {
                plan.is_some_and(|plan| row.plan.eq_ignore_ascii_case(plan.trim()))
                    && model.starts_with(row.model_prefix)
            })
    }
    /// Whether a quota window governs a model drawing on `model_pool`. Only
    /// adapters with grouped quota pools separate windows by their `group.`
    /// prefix. Ungrouped windows, an unset model pool and other adapters keep
    /// every window, which is the conservative reading.
    pub fn window_serves_pool(&self, window: &str, model_pool: Option<&str>) -> bool {
        match (self.grouped_quota_pools, model_pool, window.split_once('.')) {
            (true, Some(pool), Some((group, _))) => group == pool,
            _ => true,
        }
    }
    pub fn get(name: &str) -> Option<&'static Self> {
        ADAPTER_SPECS.iter().find(|spec| spec.name == name)
    }
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
        name: "claude",
        family: "anthropic",
        binary: "claude",
        skills: Some(SkillLocation {
            environment: None,
            fallback: ".claude",
            suffix: "skills",
        }),
        skill_install_roots: SKILL_INSTALL_ROOTS,
        default_account_directory: None,
        registered_account: true,
        account_config_dir: true,
        account_directory_prefix: Some(".claude-"),
        shared_account_settings: true,
        isolated_login_keychain: false,
        transcript_account_root: true,
        setup_root: SetupRoot::None,
        local_login: false,
        login_reprobe: LoginReprobe::AccountIdentity,
        login_observation: LoginObservation::Reprobe,
        login_check_names_account: true,
        local_account: ("default", "local"),
        argv_prompt: false,
        authentication: None,
        credential_files: &[".credentials.json", "settings.json"],
        credential_profile_sibling: true,
        credential_host_keychain: true,
        login_hint: "claude auth login",
        login_template: Some("CLAUDE_CONFIG_DIR={directory} claude auth login"),
        login_refresh_template: Some("CLAUDE_CONFIG_DIR={directory} claude"),
        auth_markers: &["not logged in", "authentication_failed"],
        shell_prefix: Some(("bin/claude-shell", "CLAUDE_CODE_SHELL_PREFIX")),
        context_directory_arg: Some("--add-dir"),
        compaction_env: Some("CLAUDE_CODE_AUTO_COMPACT_WINDOW"),
        allowance_prefix: None,
        allowance_windows: &[],
        namespaced_models: false,
        chair_switch: true,
        local_offer_reader: true,
        offered_models: false,
        github_auth: Some(GithubAuth::Environment),
        session_handoff: true,
        launch_token_refresh: true,
        deduplicate_result: false,
        result_event_usage: true,
        token_budget_notice: None,
        exit_route: false,
        stderr_quota: false,
        stdout_error_diagnostic: false,
        model_observation: ModelObservation::Stream,
        named_entry: NamedEntry::None,
        user_sessions: false,
        relocated_session_state: true,
        copy_sessions_only: false,
        quota_probe: QuotaProbeKind::HttpGet,
        quota_credits: true,
        grouped_quota_pools: false,
        retain_quota_accounts: false,
        quota_identity_check: false,
        peer_bootstrap: true,
        quota_model_prefix: None,
        account_model_plans: &[],
        free_pool: None,
        drop_unsupported_effort: false,
        pacing_keys: None,
        launch: LaunchCapabilities {
            model: true,
            default_network: NetworkAccess::Open,
            effort: true,
            resume: true,
            control: ControlCapabilities {
                lane: ControlSupport::ContinuationOnly,
                chair: ControlSupport::MailOnly,
            },
            network: ALL_NETWORK,
            image_input: true,
            write: true,
        },
    },
    Codex [] (codex: 0) {
        name: "codex",
        family: "openai",
        binary: "codex",
        skills: Some(SkillLocation {
            environment: None,
            fallback: ".agents",
            suffix: "skills",
        }),
        skill_install_roots: SKILL_INSTALL_ROOTS,
        default_account_directory: Some(("CODEX_HOME", ".codex")),
        registered_account: true,
        account_config_dir: true,
        account_directory_prefix: None,
        shared_account_settings: false,
        isolated_login_keychain: false,
        transcript_account_root: false,
        setup_root: SetupRoot::None,
        local_login: false,
        login_reprobe: LoginReprobe::Codex,
        login_observation: LoginObservation::Reprobe,
        login_check_names_account: false,
        local_account: ("default", "local"),
        argv_prompt: false,
        authentication: Some("login"),
        credential_files: &["auth.json", "config.toml"],
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
        shell_prefix: None,
        context_directory_arg: None,
        compaction_env: None,
        allowance_prefix: None,
        allowance_windows: &[],
        namespaced_models: false,
        chair_switch: false,
        local_offer_reader: false,
        offered_models: false,
        github_auth: Some(GithubAuth::ShellEnvironment),
        session_handoff: false,
        launch_token_refresh: false,
        deduplicate_result: false,
        result_event_usage: false,
        token_budget_notice: Some("Codex writer token budgets are checked at completed-turn boundaries; a turn cannot be interrupted mid-flight"),
        exit_route: false,
        stderr_quota: false,
        stdout_error_diagnostic: false,
        model_observation: ModelObservation::Rollout,
        named_entry: NamedEntry::None,
        user_sessions: false,
        relocated_session_state: false,
        copy_sessions_only: true,
        quota_probe: QuotaProbeKind::Command,
        quota_credits: true,
        grouped_quota_pools: false,
        retain_quota_accounts: false,
        quota_identity_check: true,
        peer_bootstrap: true,
        quota_model_prefix: None,
        account_model_plans: &[],
        free_pool: None,
        drop_unsupported_effort: false,
        pacing_keys: None,
        launch: LaunchCapabilities {
            model: true,
            default_network: NetworkAccess::Open,
            effort: true,
            resume: true,
            control: ControlCapabilities {
                lane: ControlSupport::ContinuationOnly,
                chair: ControlSupport::MailOnly,
            },
            network: ALL_NETWORK,
            image_input: true,
            write: true,
        },
    },
    Cursor [] (cursor: 2) {
        name: "cursor",
        family: "cursor",
        binary: "cursor-agent",
        skills: Some(SkillLocation {
            environment: None,
            fallback: ".cursor",
            suffix: "skills",
        }),
        skill_install_roots: SKILL_INSTALL_ROOTS,
        default_account_directory: None,
        registered_account: false,
        account_config_dir: false,
        account_directory_prefix: None,
        shared_account_settings: false,
        isolated_login_keychain: false,
        transcript_account_root: false,
        setup_root: SetupRoot::Home,
        local_login: true,
        login_reprobe: LoginReprobe::Cursor,
        login_observation: LoginObservation::Reprobe,
        login_check_names_account: false,
        local_account: ("default", "local"),
        argv_prompt: true,
        authentication: Some("login"),
        credential_files: &["state.vscdb", "state.vscdb-wal", "cli-config.json"],
        credential_profile_sibling: false,
        credential_host_keychain: false,
        login_hint: "agent login",
        login_template: None,
        login_refresh_template: None,
        auth_markers: &[
            "authentication required. please run agent login",
            "not logged in",
        ],
        shell_prefix: None,
        context_directory_arg: None,
        compaction_env: None,
        allowance_prefix: None,
        allowance_windows: &[],
        namespaced_models: false,
        chair_switch: false,
        local_offer_reader: false,
        offered_models: false,
        github_auth: None,
        session_handoff: false,
        launch_token_refresh: false,
        deduplicate_result: false,
        result_event_usage: false,
        token_budget_notice: None,
        exit_route: false,
        stderr_quota: false,
        stdout_error_diagnostic: false,
        model_observation: ModelObservation::Stream,
        named_entry: NamedEntry::None,
        user_sessions: false,
        relocated_session_state: true,
        copy_sessions_only: false,
        quota_probe: QuotaProbeKind::HttpGet,
        quota_credits: false,
        grouped_quota_pools: false,
        retain_quota_accounts: false,
        quota_identity_check: true,
        peer_bootstrap: false,
        quota_model_prefix: None,
        account_model_plans: &[],
        free_pool: None,
        drop_unsupported_effort: false,
        pacing_keys: None,
        launch: LaunchCapabilities {
            model: true,
            default_network: NetworkAccess::Open,
            effort: false,
            resume: true,
            control: ControlCapabilities {
                lane: ControlSupport::ContinuationOnly,
                chair: ControlSupport::MailOnly,
            },
            network: ALL_NETWORK,
            image_input: false,
            write: true,
        },
    },
    Agy [] (agy: 3) {
        name: "agy",
        family: "google",
        binary: "agy",
        skills: Some(SkillLocation {
            environment: None,
            fallback: ".gemini",
            suffix: "skills",
        }),
        skill_install_roots: SKILL_INSTALL_ROOTS,
        default_account_directory: None,
        registered_account: true,
        account_config_dir: false,
        account_directory_prefix: None,
        shared_account_settings: false,
        isolated_login_keychain: true,
        transcript_account_root: false,
        setup_root: SetupRoot::None,
        local_login: true,
        login_reprobe: LoginReprobe::AccountIdentity,
        login_observation: LoginObservation::Reprobe,
        login_check_names_account: true,
        local_account: ("default", "local"),
        argv_prompt: true,
        authentication: Some("keychain"),
        credential_files: &[
            "state.vscdb",
            "state.vscdb-wal",
            "Library/Keychains/login.keychain-db",
        ],
        credential_profile_sibling: false,
        credential_host_keychain: false,
        login_hint: "agy auth login",
        login_template: Some("HOME={directory} agy` and use `/login"),
        login_refresh_template: None,
        auth_markers: &[
            "authentication required. please login to continue.",
            "authentication required. please log in",
        ],
        shell_prefix: None,
        context_directory_arg: None,
        compaction_env: None,
        allowance_prefix: None,
        allowance_windows: &[],
        namespaced_models: false,
        chair_switch: false,
        local_offer_reader: false,
        offered_models: true,
        github_auth: None,
        session_handoff: false,
        launch_token_refresh: false,
        deduplicate_result: true,
        result_event_usage: false,
        token_budget_notice: None,
        exit_route: false,
        stderr_quota: false,
        stdout_error_diagnostic: false,
        model_observation: ModelObservation::Stream,
        named_entry: NamedEntry::None,
        user_sessions: false,
        relocated_session_state: false,
        copy_sessions_only: false,
        quota_probe: QuotaProbeKind::Command,
        quota_credits: false,
        grouped_quota_pools: true,
        retain_quota_accounts: true,
        quota_identity_check: false,
        peer_bootstrap: false,
        quota_model_prefix: None,
        account_model_plans: &[],
        free_pool: None,
        drop_unsupported_effort: false,
        pacing_keys: None,
        launch: LaunchCapabilities {
            model: true,
            default_network: NetworkAccess::Open,
            effort: true,
            resume: true,
            control: ControlCapabilities {
                lane: ControlSupport::ContinuationOnly,
                chair: ControlSupport::MailOnly,
            },
            network: ALL_NETWORK,
            image_input: false,
            write: true,
        },
    },
    Opencode [] (opencode: 4) {
        name: "opencode",
        family: "opencode",
        binary: "opencode",
        skills: Some(SkillLocation {
            environment: Some("XDG_CONFIG_HOME"),
            fallback: ".config",
            suffix: "opencode/skills",
        }),
        skill_install_roots: SKILL_INSTALL_ROOTS,
        default_account_directory: None,
        registered_account: false,
        account_config_dir: false,
        account_directory_prefix: None,
        shared_account_settings: false,
        isolated_login_keychain: false,
        transcript_account_root: false,
        setup_root: SetupRoot::XdgDataHome,
        local_login: true,
        login_reprobe: LoginReprobe::ExecutableOnly,
        login_observation: LoginObservation::ExecutableOnly,
        login_check_names_account: false,
        local_account: ("go", "go"),
        argv_prompt: false,
        authentication: Some("login"),
        credential_files: &[
            "auth.json",
            "opencode.db",
            "opencode.db-wal",
            "opencode.json",
        ],
        credential_profile_sibling: false,
        credential_host_keychain: false,
        login_hint: "opencode auth login",
        login_template: None,
        login_refresh_template: None,
        auth_markers: &["no api key found", "run opencode auth login"],
        shell_prefix: None,
        context_directory_arg: None,
        compaction_env: None,
        allowance_prefix: Some("opencode-go/"),
        allowance_windows: &["five_hour", "seven_day", "thirty_day"],
        namespaced_models: true,
        chair_switch: false,
        local_offer_reader: false,
        offered_models: false,
        github_auth: None,
        session_handoff: false,
        launch_token_refresh: false,
        deduplicate_result: false,
        result_event_usage: false,
        token_budget_notice: None,
        exit_route: false,
        stderr_quota: true,
        stdout_error_diagnostic: true,
        model_observation: ModelObservation::SessionDatabase,
        named_entry: NamedEntry::None,
        user_sessions: false,
        relocated_session_state: true,
        copy_sessions_only: false,
        quota_probe: QuotaProbeKind::LocalLedger,
        quota_credits: true,
        grouped_quota_pools: false,
        retain_quota_accounts: false,
        quota_identity_check: false,
        peer_bootstrap: false,
        quota_model_prefix: Some("opencode-go/"),
        account_model_plans: &[
            AccountModelPlan { plan: "go", model_prefix: "opencode-go/", free: false },
            AccountModelPlan { plan: "zen", model_prefix: "opencode/", free: true },
        ],
        free_pool: Some("opencode-free"),
        drop_unsupported_effort: true,
        pacing_keys: Some(("opencode_free", "opencode_paid")),
        launch: LaunchCapabilities {
            model: true,
            default_network: NetworkAccess::Open,
            effort: true,
            resume: true,
            control: ControlCapabilities {
                lane: ControlSupport::ContinuationOnly,
                chair: ControlSupport::MailOnly,
            },
            network: ALL_NETWORK,
            image_input: false,
            write: true,
        },
    },
    Command [] (command: 5) {
        name: "command",
        family: "command",
        binary: "",
        skills: None,
        skill_install_roots: &[],
        default_account_directory: None,
        registered_account: false,
        account_config_dir: false,
        account_directory_prefix: None,
        shared_account_settings: false,
        isolated_login_keychain: false,
        transcript_account_root: false,
        setup_root: SetupRoot::None,
        local_login: false,
        login_reprobe: LoginReprobe::None,
        login_observation: LoginObservation::None,
        login_check_names_account: false,
        local_account: ("default", "local"),
        argv_prompt: false,
        authentication: None,
        credential_files: &[],
        credential_profile_sibling: false,
        credential_host_keychain: false,
        login_hint: "",
        login_template: None,
        login_refresh_template: None,
        auth_markers: &[],
        shell_prefix: None,
        context_directory_arg: None,
        compaction_env: None,
        allowance_prefix: None,
        allowance_windows: &[],
        namespaced_models: false,
        chair_switch: false,
        local_offer_reader: false,
        offered_models: false,
        github_auth: None,
        session_handoff: false,
        launch_token_refresh: false,
        deduplicate_result: false,
        result_event_usage: false,
        token_budget_notice: None,
        exit_route: true,
        stderr_quota: false,
        stdout_error_diagnostic: false,
        model_observation: ModelObservation::Stream,
        named_entry: NamedEntry::Command,
        user_sessions: false,
        relocated_session_state: false,
        copy_sessions_only: false,
        quota_probe: QuotaProbeKind::None,
        quota_credits: false,
        grouped_quota_pools: false,
        retain_quota_accounts: false,
        quota_identity_check: false,
        peer_bootstrap: false,
        quota_model_prefix: None,
        account_model_plans: &[],
        free_pool: None,
        drop_unsupported_effort: false,
        pacing_keys: None,
        launch: LaunchCapabilities {
            model: false,
            default_network: NetworkAccess::Open,
            effort: false,
            resume: false,
            control: ControlCapabilities {
                lane: ControlSupport::Unsupported,
                chair: ControlSupport::Unsupported,
            },
            network: ALL_NETWORK,
            image_input: false,
            write: false,
        },
    },
    // A named, optional host feature. Preparation delegates to its provider;
    // decoding, accounts and confinement remain owned by that provider.
    HostNative [serde(rename = "host-native")] () {
        name: "host-native",
        family: "host-native",
        binary: "",
        skills: None,
        skill_install_roots: &[],
        default_account_directory: None,
        registered_account: false,
        account_config_dir: false,
        account_directory_prefix: None,
        shared_account_settings: false,
        isolated_login_keychain: false,
        transcript_account_root: false,
        setup_root: SetupRoot::None,
        local_login: false,
        login_reprobe: LoginReprobe::None,
        login_observation: LoginObservation::None,
        login_check_names_account: false,
        local_account: ("default", "local"),
        argv_prompt: false,
        authentication: None,
        credential_files: &[],
        credential_profile_sibling: false,
        credential_host_keychain: false,
        login_hint: "",
        login_template: None,
        login_refresh_template: None,
        auth_markers: &[],
        shell_prefix: None,
        context_directory_arg: None,
        compaction_env: None,
        allowance_prefix: None,
        allowance_windows: &[],
        namespaced_models: false,
        chair_switch: false,
        local_offer_reader: false,
        offered_models: false,
        github_auth: None,
        session_handoff: false,
        launch_token_refresh: false,
        deduplicate_result: false,
        result_event_usage: false,
        token_budget_notice: None,
        exit_route: false,
        stderr_quota: false,
        stdout_error_diagnostic: false,
        model_observation: ModelObservation::Stream,
        named_entry: NamedEntry::DelegatedAgent,
        user_sessions: false,
        relocated_session_state: false,
        copy_sessions_only: false,
        quota_probe: QuotaProbeKind::None,
        quota_credits: false,
        grouped_quota_pools: false,
        retain_quota_accounts: false,
        quota_identity_check: false,
        peer_bootstrap: false,
        quota_model_prefix: None,
        account_model_plans: &[],
        free_pool: None,
        drop_unsupported_effort: false,
        pacing_keys: None,
        launch: LaunchCapabilities {
            model: false,
            default_network: NetworkAccess::Open,
            effort: false,
            resume: false,
            control: ControlCapabilities {
                lane: ControlSupport::Unsupported,
                chair: ControlSupport::Unsupported,
            },
            network: ALL_NETWORK,
            image_input: false,
            write: true,
        },
    },
    #[cfg(feature = "registry-fixture")]
    FixtureProvider [serde(rename = "fixture-provider")] () {
        name: "fixture-provider",
        family: "fixture",
        binary: "fixture-provider",
        skills: None,
        skill_install_roots: &[],
        default_account_directory: None,
        registered_account: false,
        account_config_dir: false,
        account_directory_prefix: None,
        shared_account_settings: false,
        isolated_login_keychain: false,
        transcript_account_root: false,
        setup_root: SetupRoot::None,
        local_login: true,
        login_reprobe: LoginReprobe::None,
        login_observation: LoginObservation::None,
        login_check_names_account: false,
        local_account: ("fixture", "work"),
        argv_prompt: false,
        authentication: None,
        credential_files: &[],
        credential_profile_sibling: false,
        credential_host_keychain: false,
        login_hint: "",
        login_template: None,
        login_refresh_template: None,
        auth_markers: &[],
        shell_prefix: None,
        context_directory_arg: None,
        compaction_env: None,
        allowance_prefix: None,
        allowance_windows: &[],
        namespaced_models: false,
        chair_switch: false,
        local_offer_reader: false,
        offered_models: false,
        github_auth: None,
        session_handoff: false,
        launch_token_refresh: false,
        deduplicate_result: false,
        result_event_usage: false,
        token_budget_notice: None,
        exit_route: true,
        stderr_quota: false,
        stdout_error_diagnostic: false,
        model_observation: ModelObservation::Stream,
        named_entry: NamedEntry::None,
        user_sessions: false,
        relocated_session_state: false,
        copy_sessions_only: false,
        quota_probe: QuotaProbeKind::None,
        quota_credits: false,
        grouped_quota_pools: false,
        retain_quota_accounts: false,
        quota_identity_check: false,
        peer_bootstrap: false,
        quota_model_prefix: None,
        account_model_plans: &[],
        free_pool: None,
        drop_unsupported_effort: false,
        pacing_keys: None,
        launch: LaunchCapabilities {
            model: true,
            default_network: NetworkAccess::Open,
            effort: false,
            resume: false,
            control: ControlCapabilities {
                lane: ControlSupport::Unsupported,
                chair: ControlSupport::Unsupported,
            },
            network: ALL_NETWORK,
            image_input: false,
            write: false,
        },
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    export_adapter_bindings!($; test_adapter_bindings; [];
        [#[cfg(any())]] [disabled: 0]
        [] [enabled: 1]
        [] []
    );

    #[test]
    fn implementation_bindings_retain_conditional_guards() {
        macro_rules! names {
            ($( $(#[$cfg:meta])* $module:ident : $order:literal,)*) => {
                &[ $( $(#[$cfg])* (stringify!($module), $order), )* ]
            };
        }
        assert_eq!(test_adapter_bindings!(names), &[("enabled", 1)]);
    }

    #[test]
    fn account_and_session_policies_preserve_adapter_behavior() {
        for spec in ADAPTER_SPECS {
            assert_eq!(
                spec.transcript_account_root,
                spec.id == Harness::Claude,
                "{}",
                spec.name
            );
            assert_eq!(
                spec.setup_root,
                match spec.id {
                    Harness::Cursor => SetupRoot::Home,
                    Harness::Opencode => SetupRoot::XdgDataHome,
                    _ => SetupRoot::None,
                },
                "{}",
                spec.name
            );
            assert_eq!(
                spec.isolated_login_keychain,
                spec.id == Harness::Agy,
                "{}",
                spec.name
            );
            assert_eq!(
                spec.shared_account_settings,
                spec.id == Harness::Claude,
                "{}",
                spec.name
            );
            assert_eq!(
                spec.account_directory_prefix,
                (spec.id == Harness::Claude).then_some(".claude-"),
                "{}",
                spec.name
            );
            assert_eq!(
                spec.copy_sessions_only,
                spec.id == Harness::Codex,
                "{}",
                spec.name
            );
            assert_eq!(
                spec.relocated_session_state,
                matches!(
                    spec.id,
                    Harness::Claude | Harness::Cursor | Harness::Opencode
                ),
                "{}",
                spec.name
            );
        }
    }

    #[test]
    fn host_native_declares_a_fresh_named_writer_contract() {
        let spec = AdapterSpec::get("host-native").unwrap();
        assert!(spec.binary.is_empty() && !spec.registered_account);
        assert_eq!(spec.launch.default_network, NetworkAccess::Open);
        assert!(
            spec.check_launch(&LaunchRequest {
                write: true,
                network: Some(NetworkAccess::Open),
                ..Default::default()
            })
            .is_ok()
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
            assert_eq!(spec.check_launch(&request).unwrap_err().field, field);
        }
    }

    #[test]
    fn adapter_specs_are_unique_and_match_typed_identities() {
        for spec in ADAPTER_SPECS {
            assert_eq!(
                ADAPTER_SPECS
                    .iter()
                    .filter(|row| row.name == spec.name)
                    .count(),
                1
            );
            assert!(std::ptr::eq(spec.id.spec(), spec));
            assert!(std::ptr::eq(AdapterSpec::get(spec.name).unwrap(), spec));
            assert_eq!(serde_json::to_value(spec.id).unwrap(), spec.name);
            assert_eq!(
                serde_json::from_value::<Harness>(serde_json::json!(spec.name)).unwrap(),
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
                !spec.account_config_dir || spec.registered_account,
                "{}",
                spec.name
            );
        }
        assert!(AdapterSpec::get("codex").unwrap().account_config_dir);
        assert!(!AdapterSpec::get("agy").unwrap().account_config_dir);
    }

    #[test]
    fn only_agy_has_an_unregistered_local_account() {
        for spec in ADAPTER_SPECS {
            assert_eq!(
                spec.is_unregistered_local("local"),
                spec.name == "agy",
                "{}",
                spec.name
            );
            assert!(!spec.is_unregistered_local("agy-google-fixture"));
        }
    }
}

#[cfg(test)]
mod account_plan_tests {
    #[test]
    fn registered_plan_partitions_follow_the_adapter_declaration() {
        for spec in super::ADAPTER_SPECS {
            for plan in spec.account_model_plans {
                let model = format!("{}fixture", plan.model_prefix);
                assert!(spec.account_serves_model(Some(plan.plan), &model));
                assert_eq!(spec.is_free_plan(Some(plan.plan)), plan.free);
                assert!(!spec.account_serves_model(None, &model));
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
        assert_eq!(command.launch.default_network, NetworkAccess::Open);
        assert!(command.check_launch(&LaunchRequest::default()).is_ok());
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
            assert_eq!(command.check_launch(&request).unwrap_err().field, field);
        }
    }

    #[test]
    fn image_input_capability_matches_native_delivery() {
        for spec in ADAPTER_SPECS {
            assert_eq!(
                spec.launch.image_input,
                matches!(spec.name, "claude" | "codex"),
                "{}",
                spec.name
            );
        }
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
            ("cursor", effort, Some(LaunchField::Effort)),
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
                spec.check_launch(&request).err().map(|e| e.field),
                refused,
                "{name}"
            );
        }
    }

    #[test]
    fn refusal_names_the_adapter_and_field() {
        let error = AdapterSpec::get("cursor")
            .unwrap()
            .check_launch(&LaunchRequest {
                effort: Some("low"),
                ..Default::default()
            })
            .unwrap_err();
        assert_eq!(error.to_string(), "cursor does not support effort");
    }

    #[test]
    fn unsupported_modes_are_refused_by_field() {
        let narrow = AdapterSpec {
            launch: LaunchCapabilities {
                model: true,
                default_network: NetworkAccess::Open,
                resume: false,
                network: &[NetworkAccess::Provider],
                write: false,
                ..ADAPTER_SPECS[0].launch
            },
            ..ADAPTER_SPECS[0]
        };
        for (request, field) in [
            (
                LaunchRequest {
                    resume: true,
                    ..Default::default()
                },
                LaunchField::Resume,
            ),
            (
                LaunchRequest {
                    network: Some(NetworkAccess::Local),
                    ..Default::default()
                },
                LaunchField::Network,
            ),
            (
                LaunchRequest {
                    write: true,
                    ..Default::default()
                },
                LaunchField::Write,
            ),
        ] {
            assert_eq!(narrow.check_launch(&request).unwrap_err().field, field);
        }
    }
}
