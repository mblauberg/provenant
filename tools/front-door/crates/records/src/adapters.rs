//! Closed adapter identities and shared facts. Mechanics stay in their modules.
use crate::run::NetworkAccess;
use serde::{Deserialize, Serialize};

/// Latest command output retained by the guardian and adapter.
pub const COMMAND_OUTPUT_TAIL_BYTES: usize = 65_536;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Harness {
    Claude,
    Codex,
    Cursor,
    Agy,
    Opencode,
    Command,
    #[serde(rename = "host-native")]
    HostNative,
}
impl Harness {
    pub fn spec(self) -> &'static AdapterSpec {
        &ADAPTER_SPECS[self as usize]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaProbeKind {
    None,
    HttpGet,
    Command,
    LocalLedger,
}

#[derive(Debug)]
pub struct AdapterSpec {
    pub id: Harness,
    pub name: &'static str,
    pub binary: &'static str,
    pub registered_account: bool,
    pub local_login: bool,
    pub argv_prompt: bool,
    pub authentication: Option<&'static str>,
    pub user_sessions: bool,
    pub quota_probe: QuotaProbeKind,
    pub quota_credits: bool,
    pub grouped_quota_pools: bool,
    pub retain_quota_accounts: bool,
    pub quota_identity_check: bool,
    pub peer_bootstrap: bool,
    /// A snapshot governs only this model prefix; None governs every model.
    pub quota_model_prefix: Option<&'static str>,
    /// Account and model prefix served by the free allowance.
    pub free_account: Option<(&'static str, &'static str)>,
    pub free_pool: Option<&'static str>,
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

/// Declared launch capabilities. A request outside them is refused before
/// admission, worktree preparation or receipt creation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaunchCapabilities {
    /// Accepts a model selector; false for configured command entries.
    pub model: bool,
    pub default_network: NetworkAccess,
    /// Takes a reasoning-effort control on the command line.
    pub effort: bool,
    /// Continues a recorded provider session.
    pub resume: bool,
    /// Accepts follow-up input into a live session. No adapter offers this yet.
    pub steering: bool,
    /// Network modes the adapter's sandbox profile can enforce.
    pub network: &'static [NetworkAccess],
    /// Accepts image input in a prompt. No adapter offers this yet.
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchField {
    Model,
    Effort,
    Resume,
    Network,
    Write,
}
impl LaunchField {
    pub fn name(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Effort => "effort",
            Self::Resume => "resume",
            Self::Network => "network",
            Self::Write => "write",
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
    pub fn account_serves_model(&self, account: &str, model: &str) -> bool {
        self.free_account.is_none_or(|(free_account, prefix)| {
            (account == free_account) == model.starts_with(prefix)
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

// Keep the rows in Harness order. CLI installation also uses this order.
pub static ADAPTER_SPECS: &[AdapterSpec; 7] = &[
    AdapterSpec {
        id: Harness::Claude,
        name: "claude",
        binary: "claude",
        registered_account: true,
        local_login: false,
        argv_prompt: false,
        authentication: None,
        user_sessions: true,
        quota_probe: QuotaProbeKind::HttpGet,
        quota_credits: true,
        grouped_quota_pools: false,
        retain_quota_accounts: false,
        quota_identity_check: false,
        peer_bootstrap: true,
        quota_model_prefix: None,
        free_account: None,
        free_pool: None,
        pacing_keys: None,
        launch: LaunchCapabilities {
            model: true,
            default_network: NetworkAccess::Open,
            effort: true,
            resume: true,
            steering: false,
            network: ALL_NETWORK,
            image_input: false,
            write: true,
        },
    },
    AdapterSpec {
        id: Harness::Codex,
        name: "codex",
        binary: "codex",
        registered_account: true,
        local_login: false,
        argv_prompt: false,
        authentication: Some("login"),
        user_sessions: false,
        quota_probe: QuotaProbeKind::Command,
        quota_credits: true,
        grouped_quota_pools: false,
        retain_quota_accounts: false,
        quota_identity_check: true,
        peer_bootstrap: true,
        quota_model_prefix: None,
        free_account: None,
        free_pool: None,
        pacing_keys: None,
        launch: LaunchCapabilities {
            model: true,
            default_network: NetworkAccess::Open,
            effort: true,
            resume: true,
            steering: false,
            network: ALL_NETWORK,
            image_input: false,
            write: true,
        },
    },
    AdapterSpec {
        id: Harness::Cursor,
        name: "cursor",
        binary: "cursor-agent",
        registered_account: false,
        local_login: true,
        argv_prompt: true,
        authentication: Some("login"),
        user_sessions: true,
        quota_probe: QuotaProbeKind::HttpGet,
        quota_credits: false,
        grouped_quota_pools: false,
        retain_quota_accounts: false,
        quota_identity_check: true,
        peer_bootstrap: false,
        quota_model_prefix: None,
        free_account: None,
        free_pool: None,
        pacing_keys: None,
        launch: LaunchCapabilities {
            model: true,
            default_network: NetworkAccess::Open,
            effort: false,
            resume: true,
            steering: false,
            network: ALL_NETWORK,
            image_input: false,
            write: true,
        },
    },
    AdapterSpec {
        id: Harness::Agy,
        name: "agy",
        binary: "agy",
        registered_account: false,
        local_login: true,
        argv_prompt: true,
        authentication: Some("keychain"),
        user_sessions: false,
        quota_probe: QuotaProbeKind::Command,
        quota_credits: false,
        grouped_quota_pools: true,
        retain_quota_accounts: true,
        quota_identity_check: false,
        peer_bootstrap: false,
        quota_model_prefix: None,
        free_account: None,
        free_pool: None,
        pacing_keys: None,
        launch: LaunchCapabilities {
            model: true,
            default_network: NetworkAccess::Open,
            effort: true,
            resume: true,
            steering: false,
            network: ALL_NETWORK,
            image_input: false,
            write: true,
        },
    },
    AdapterSpec {
        id: Harness::Opencode,
        name: "opencode",
        binary: "opencode",
        registered_account: false,
        local_login: true,
        argv_prompt: false,
        authentication: None,
        user_sessions: true,
        quota_probe: QuotaProbeKind::LocalLedger,
        quota_credits: true,
        grouped_quota_pools: false,
        retain_quota_accounts: false,
        quota_identity_check: false,
        peer_bootstrap: false,
        quota_model_prefix: Some("opencode-go/"),
        free_account: Some(("zen", "opencode/")),
        free_pool: Some("opencode-free"),
        pacing_keys: Some(("opencode_free", "opencode_paid")),
        launch: LaunchCapabilities {
            model: true,
            default_network: NetworkAccess::Open,
            effort: true,
            resume: true,
            steering: false,
            network: ALL_NETWORK,
            image_input: false,
            write: true,
        },
    },
    AdapterSpec {
        id: Harness::Command,
        name: "command",
        binary: "",
        registered_account: false,
        local_login: false,
        argv_prompt: false,
        authentication: None,
        user_sessions: false,
        quota_probe: QuotaProbeKind::None,
        quota_credits: false,
        grouped_quota_pools: false,
        retain_quota_accounts: false,
        quota_identity_check: false,
        peer_bootstrap: false,
        quota_model_prefix: None,
        free_account: None,
        free_pool: None,
        pacing_keys: None,
        launch: LaunchCapabilities {
            model: false,
            default_network: NetworkAccess::Open,
            effort: false,
            resume: false,
            steering: false,
            network: ALL_NETWORK,
            image_input: false,
            write: false,
        },
    },
    // A named, optional host feature. Preparation delegates to its provider;
    // decoding, accounts and confinement remain owned by that provider.
    AdapterSpec {
        id: Harness::HostNative,
        name: "host-native",
        binary: "",
        registered_account: false,
        local_login: false,
        argv_prompt: false,
        authentication: None,
        user_sessions: false,
        quota_probe: QuotaProbeKind::None,
        quota_credits: false,
        grouped_quota_pools: false,
        retain_quota_accounts: false,
        quota_identity_check: false,
        peer_bootstrap: false,
        quota_model_prefix: None,
        free_account: None,
        free_pool: None,
        pacing_keys: None,
        launch: LaunchCapabilities {
            model: false,
            default_network: NetworkAccess::Open,
            effort: false,
            resume: false,
            steering: false,
            network: ALL_NETWORK,
            image_input: false,
            write: true,
        },
    },
];

#[cfg(test)]
mod tests {
    use super::*;

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
mod free_account_tests {
    #[test]
    fn zen_account_does_not_serve_go_models() {
        let spec = super::AdapterSpec::get("opencode").unwrap();
        assert!(spec.account_serves_model("zen", "opencode/beam-free"));
        assert!(!spec.account_serves_model("go", "opencode/beam-free"));
        assert!(spec.account_serves_model("go", "opencode-go/mimo-v2.6-flash"));
        assert!(!spec.account_serves_model("zen", "opencode-go/mimo-v2.6-flash"));
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
