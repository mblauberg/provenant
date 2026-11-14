//! Typed account attribution shared by run, provider and telemetry records.
use serde::{Deserialize, Serialize};

/// Account attribution recorded with usage. Unknown identity, epoch and source
/// stay explicit until a provider identity port supplies verified evidence.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AccountIdentity {
    pub identity: Identity,
    pub epoch: IdentityEpoch,
    pub evidence_source: IdentityEvidenceSource,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Identity {
    Known { provider: String, account_key: String },
    Unknown { reason: UnknownIdentityReason },
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum IdentityEpoch {
    Known { id: String },
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityEvidenceSource {
    Provider,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownIdentityReason {
    ProviderIdentityUnavailable,
    ConflictingProviderIdentity,
    NoProviderAccount,
}

impl AccountIdentity {
    /// Record verified provider identity only when its identity epoch is known.
    pub fn known(provider: &str, account_key: &str, epoch: &str) -> Self {
        Self {
            identity: Identity::Known {
                provider: provider.into(),
                account_key: account_key.into(),
            },
            epoch: IdentityEpoch::Known { id: epoch.into() },
            evidence_source: IdentityEvidenceSource::Provider,
        }
    }

    /// Verified provider identity when no native login epoch is available.
    pub fn provider_observation(provider: &str, account_key: &str) -> Self {
        Self {
            identity: Identity::Known {
                provider: provider.into(),
                account_key: account_key.into(),
            },
            epoch: IdentityEpoch::Unknown,
            evidence_source: IdentityEvidenceSource::Provider,
        }
    }

    /// The provider identity could not be verified for this observation.
    pub fn unknown() -> Self {
        Self::unknown_for(UnknownIdentityReason::ProviderIdentityUnavailable)
    }

    /// Conflicting provider observations prevent attribution across a session transcript.
    pub fn conflicting_provider_observations() -> Self {
        Self {
            identity: Identity::Unknown {
                reason: UnknownIdentityReason::ConflictingProviderIdentity,
            },
            epoch: IdentityEpoch::Unknown,
            evidence_source: IdentityEvidenceSource::Provider,
        }
    }

    /// A usage event that is not attached to a provider account.
    pub fn no_provider_account() -> Self {
        Self::unknown_for(UnknownIdentityReason::NoProviderAccount)
    }

    fn unknown_for(reason: UnknownIdentityReason) -> Self {
        Self {
            identity: Identity::Unknown { reason },
            epoch: IdentityEpoch::Unknown,
            evidence_source: IdentityEvidenceSource::Unknown,
        }
    }

    /// Stable grouping key; unknown identities remain in the visible gap group.
    pub fn account_key(&self) -> String {
        match &self.identity {
            Identity::Known { provider, account_key } => format!("{provider}/{account_key}"),
            Identity::Unknown { .. } => "unattributed".into(),
        }
    }

    pub fn is_unknown(&self) -> bool {
        matches!(&self.identity, Identity::Unknown { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_account_identity_case_round_trips() {
        let cases = [
            AccountIdentity::known("claude", "opaque-account", "epoch-1"),
            AccountIdentity::provider_observation("claude", "account-without-epoch"),
            AccountIdentity::unknown(),
            AccountIdentity::no_provider_account(),
            AccountIdentity::conflicting_provider_observations(),
        ];
        for identity in cases {
            let encoded = serde_json::to_vec(&identity).unwrap();
            let decoded: AccountIdentity = serde_json::from_slice(&encoded).unwrap();
            assert_eq!(decoded, identity);
        }
    }
}
