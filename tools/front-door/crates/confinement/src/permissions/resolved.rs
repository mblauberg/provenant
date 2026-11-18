//! One effective provider policy, shared by admission and enforcement.
use super::{NetworkAccess, PathGrant, PlanPermissions, PolicySource};

/// Authentication is always bounded by the host credential inventory and the
/// selected mechanism. Disabling it cannot disable the mandatory exclusions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AuthenticationAccess {
    #[default]
    Selected,
    SelectedWithoutKeychain,
    Disabled,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NetworkPolicy {
    #[default]
    Open,
    Local,
    Provider,
    Offline,
}

impl From<NetworkAccess> for NetworkPolicy {
    fn from(value: NetworkAccess) -> Self {
        match value {
            NetworkAccess::Open => Self::Open,
            NetworkAccess::Local => Self::Local,
            NetworkAccess::Provider => Self::Provider,
        }
    }
}

/// Every contributing restriction retains its owner; defaults confer no
/// exemption from credential, Git, state or read-only boundaries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfinementSource {
    /// The network selection sealed in the permission plan.
    Plan,
    Host(std::path::PathBuf),
    Provider,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResolvedConfinementPolicy {
    authentication: AuthenticationAccess,
    network: NetworkPolicy,
    sources: Vec<ConfinementSource>,
    deny_read: Vec<PathGrant>,
    deny_write: Vec<PathGrant>,
}

impl ResolvedConfinementPolicy {
    pub fn without_credentials(mut self) -> Self {
        self.authentication = AuthenticationAccess::Disabled;
        self.record(ConfinementSource::Provider);
        self
    }

    pub fn without_keychain(mut self) -> Self {
        if self.authentication != AuthenticationAccess::Disabled {
            self.authentication = AuthenticationAccess::SelectedWithoutKeychain;
        }
        self.record(ConfinementSource::Provider);
        self
    }

    pub fn offline(mut self) -> Self {
        self.network = NetworkPolicy::Offline;
        self.record(ConfinementSource::Provider);
        self
    }

    /// Host ceilings, lane selection and adapter preferences meet here. This
    /// operation is idempotent so admission and rendering use identical data.
    pub fn resolve_for(&self, plan: &PlanPermissions) -> Self {
        let mut resolved = self.clone();
        resolved.restrict(plan.network);
        if plan.network != NetworkAccess::Open {
            resolved.record(ConfinementSource::Plan);
        }
        if let Some(policy) = &plan.policy {
            resolved.restrict(policy.optional().network);
            resolved.deny_read.clone_from(&policy.optional().deny_read);
            resolved
                .deny_write
                .clone_from(&policy.optional().deny_write);
            if (policy.optional().network != NetworkAccess::Open
                || !resolved.deny_read.is_empty()
                || !resolved.deny_write.is_empty())
                && let PolicySource::Host(path) = policy.source()
            {
                resolved.record(ConfinementSource::Host(path.clone()));
            }
        }
        resolved
    }

    fn restrict(&mut self, ceiling: NetworkAccess) {
        let current = match self.network {
            NetworkPolicy::Offline => return,
            NetworkPolicy::Open => NetworkAccess::Open,
            NetworkPolicy::Local => NetworkAccess::Local,
            NetworkPolicy::Provider => NetworkAccess::Provider,
        };
        self.network = super::policy::network_ceiling(current, ceiling).into();
    }

    fn record(&mut self, source: ConfinementSource) {
        if !self.sources.contains(&source) {
            self.sources.push(source);
        }
    }

    pub fn network(&self) -> NetworkPolicy {
        self.network
    }

    pub fn sources(&self) -> &[ConfinementSource] {
        &self.sources
    }

    pub fn deny_read(&self) -> &[PathGrant] {
        &self.deny_read
    }

    pub fn deny_write(&self) -> &[PathGrant] {
        &self.deny_write
    }

    pub fn credentials_permitted(&self) -> bool {
        self.authentication != AuthenticationAccess::Disabled
    }

    pub fn keychain_permitted(&self) -> bool {
        self.authentication == AuthenticationAccess::Selected
    }

    pub fn github_permitted(&self) -> bool {
        self.credentials_permitted() && self.network == NetworkPolicy::Open
    }
}
