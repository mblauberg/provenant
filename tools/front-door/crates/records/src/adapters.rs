//! Closed adapter identities and shared facts. Mechanics stay in their modules.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Harness {
    Claude,
    Codex,
    Cursor,
    Agy,
    Opencode,
}
impl Harness {
    pub fn spec(self) -> &'static AdapterSpec {
        &ADAPTER_SPECS[self as usize]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaProbeKind {
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
}
impl AdapterSpec {
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
pub static ADAPTER_SPECS: &[AdapterSpec; 5] = &[
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
    },
];

#[cfg(test)]
mod tests {
    use super::*;

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
