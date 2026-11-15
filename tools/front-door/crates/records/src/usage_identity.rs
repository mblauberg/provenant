//! Typed account attribution shared by run, provider and telemetry records.
use crate::ExtraFields;
use serde::{Deserialize, Serialize};

macro_rules! tagged_enum_kinds {
    ($enum:ident, $kind:ident { $($variant:ident),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
        pub enum $kind { $($variant),+ }

        impl $enum {
            pub const ALL: &'static [$kind] = &[$($kind::$variant),+];

            pub fn kind(&self) -> $kind {
                match self {
                    $(Self::$variant { .. } => $kind::$variant),+
                }
            }
        }
    };
}

macro_rules! simple_enum_all {
    ($enum:ident { $($variant:ident),+ $(,)? }) => {
        impl $enum {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            pub const fn variant(self) -> Self {
                match self {
                    $(Self::$variant => Self::$variant),+
                }
            }
        }
    };
}

/// Account attribution recorded with usage. Unknown identity, epoch and source
/// stay explicit until a provider identity port supplies verified evidence.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AccountIdentity {
    #[serde(flatten)]
    pub extra: ExtraFields,
    pub identity: Identity,
    pub epoch: IdentityEpoch,
    pub evidence_source: IdentityEvidenceSource,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Identity {
    Known {
        provider: String,
        account_key: String,
        #[serde(flatten)]
        extra: ExtraFields,
    },
    Unknown {
        reason: UnknownIdentityReason,
        #[serde(flatten)]
        extra: ExtraFields,
    },
}
tagged_enum_kinds!(Identity, IdentityKind { Known, Unknown });

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum IdentityEpoch {
    Known {
        id: String,
        #[serde(flatten)]
        extra: ExtraFields,
    },
    Unknown {
        #[serde(flatten)]
        extra: ExtraFields,
    },
}
tagged_enum_kinds!(IdentityEpoch, IdentityEpochKind { Known, Unknown });

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityEvidenceSource {
    Provider,
    Unknown,
}
simple_enum_all!(IdentityEvidenceSource { Provider, Unknown });

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownIdentityReason {
    ProviderIdentityUnavailable,
    ConflictingProviderIdentity,
    NoProviderAccount,
}
simple_enum_all!(UnknownIdentityReason {
    ProviderIdentityUnavailable,
    ConflictingProviderIdentity,
    NoProviderAccount,
});

impl AccountIdentity {
    /// Record verified provider identity only when its identity epoch is known.
    pub fn known(provider: &str, account_key: &str, epoch: &str) -> Self {
        Self {
            extra: Default::default(),
            identity: Identity::Known {
                provider: provider.into(),
                account_key: account_key.into(),
                extra: Default::default(),
            },
            epoch: IdentityEpoch::Known {
                id: epoch.into(),
                extra: Default::default(),
            },
            evidence_source: IdentityEvidenceSource::Provider,
        }
    }

    /// Verified provider identity when no native login epoch is available.
    pub fn provider_observation(provider: &str, account_key: &str) -> Self {
        Self {
            extra: Default::default(),
            identity: Identity::Known {
                provider: provider.into(),
                account_key: account_key.into(),
                extra: Default::default(),
            },
            epoch: IdentityEpoch::Unknown {
                extra: Default::default(),
            },
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
            extra: Default::default(),
            identity: Identity::Unknown {
                reason: UnknownIdentityReason::ConflictingProviderIdentity,
                extra: Default::default(),
            },
            epoch: IdentityEpoch::Unknown {
                extra: Default::default(),
            },
            evidence_source: IdentityEvidenceSource::Provider,
        }
    }

    /// A usage event that is not attached to a provider account.
    pub fn no_provider_account() -> Self {
        Self::unknown_for(UnknownIdentityReason::NoProviderAccount)
    }

    fn unknown_for(reason: UnknownIdentityReason) -> Self {
        Self {
            extra: Default::default(),
            identity: Identity::Unknown {
                reason,
                extra: Default::default(),
            },
            epoch: IdentityEpoch::Unknown {
                extra: Default::default(),
            },
            evidence_source: IdentityEvidenceSource::Unknown,
        }
    }

    /// Stable grouping key; unknown identities remain in the visible gap group.
    pub fn account_key(&self) -> String {
        match &self.identity {
            Identity::Known {
                provider,
                account_key,
                ..
            } => format!("{provider}/{account_key}"),
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
    use std::collections::BTreeSet;

    #[test]
    fn account_identity_declarations_cover_their_enum_variants() {
        let cases = [
            AccountIdentity::known("provider", "account", "epoch"),
            AccountIdentity::provider_observation("provider", "account"),
            AccountIdentity::unknown(),
            AccountIdentity::conflicting_provider_observations(),
            AccountIdentity::no_provider_account(),
        ];
        let identities = cases
            .iter()
            .map(|case| case.identity.kind())
            .collect::<BTreeSet<_>>();
        assert_eq!(identities, Identity::ALL.iter().copied().collect());
        let epochs = cases
            .iter()
            .map(|case| case.epoch.kind())
            .collect::<BTreeSet<_>>();
        assert_eq!(epochs, IdentityEpoch::ALL.iter().copied().collect());
        let sources = cases
            .iter()
            .map(|case| case.evidence_source)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            sources,
            IdentityEvidenceSource::ALL.iter().copied().collect()
        );
        let reasons = cases
            .iter()
            .filter_map(|case| match &case.identity {
                Identity::Known { .. } => None,
                Identity::Unknown { reason, .. } => Some(*reason),
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            reasons,
            UnknownIdentityReason::ALL.iter().copied().collect()
        );

        for source in IdentityEvidenceSource::ALL {
            assert_eq!(source.variant(), *source);
        }
        for reason in UnknownIdentityReason::ALL {
            assert_eq!(reason.variant(), *reason);
        }
    }
}
