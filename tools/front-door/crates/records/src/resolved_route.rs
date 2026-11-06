//! Routing's immutable decision, shared with providers and run.
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedRoute {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub adapter: String,
    /// Provider-native model identifier, retaining all segments after the adapter separator.
    /// Empty for a model-less adapter and absent from its wire representation.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    pub effort: Option<String>,
    pub catalogue_revision: String,
    pub account_key: Option<String>,
    pub host: String,
    pub alternatives: RouteAlternatives,
    pub exclusions: Vec<RouteExclusion>,
    pub eligibility: RouteEligibility,
    pub readiness: RouteReadiness,
    pub confidentiality: RouteConfidentiality,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool_selection: Option<RouteSelectionReceipt>,
}

impl ResolvedRoute {
    /// Whether user skills discovery is enabled for this route (default: true).
    pub fn user_skills(&self) -> bool {
        self.extra
            .get("user_skills")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true)
    }

    /// Override user skills discovery for this route.
    pub fn set_user_skills(&mut self, enabled: bool) {
        self.extra
            .insert("user_skills".into(), serde_json::Value::Bool(enabled));
    }
}

/// Auditable outcome of a weighted pool selection. Factors use basis points,
/// with 10,000 representing 1.0.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteSelectionReceipt {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub pool: String,
    pub candidates: Vec<RouteSelectionCandidate>,
    pub roll: u64,
    pub total_weight: u64,
    pub pick: String,
    /// Why normal selection was used despite live capacity cooldowns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redraws: Vec<RouteSelectionRedraw>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteSelectionRedraw {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub rejected_pick: String,
    pub roll: u64,
    pub total_weight: u64,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteSelectionCandidate {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub model: String,
    pub weight: u8,
    pub factor_basis_points: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pacing_detail: Option<String>,
    pub effort: Option<String>,
    pub readiness: Option<RouteReadiness>,
    pub exclusion: Option<String>,
}

/// An explicit fallback has the same access and disclosure requirements as the
/// primary. Run receives the decision, rather than reconstructing candidates.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteAlternative {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub adapter: String,
    pub model: String,
    pub effort: Option<String>,
    pub account_key: Option<String>,
    pub host: String,
    pub eligibility: RouteEligibility,
    pub readiness: RouteReadiness,
    pub confidentiality: RouteConfidentiality,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct RouteAlternatives(Vec<RouteAlternative>);
#[derive(Debug, thiserror::Error)]
#[error("a resolved route permits at most two explicit alternatives")]
pub struct TooManyAlternatives;
impl RouteAlternatives {
    pub fn new(values: Vec<RouteAlternative>) -> Result<Self, TooManyAlternatives> {
        if values.len() > 2 {
            Err(TooManyAlternatives)
        } else {
            Ok(Self(values))
        }
    }
    pub fn as_slice(&self) -> &[RouteAlternative] {
        &self.0
    }
}
impl<'de> Deserialize<'de> for RouteAlternatives {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(Vec::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteEligibility {
    Eligible,
    UnverifiedModel,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RouteReadiness {
    Ready {
        #[serde(flatten)]
        extra: crate::ExtraFields,
        availability_ref: String,
        expires_unix_seconds: i64,
    },
    Unavailable {
        #[serde(flatten)]
        extra: crate::ExtraFields,
        reason: RouteExclusionReason,
    },
}
impl RouteReadiness {
    /// Readiness that was never observed. The receipt has no unknown status
    /// and an older reader rejects an unrecognised one, so unknown projects
    /// onto the existing unavailable status with its established reason.
    pub fn unknown() -> Self {
        Self::Unavailable {
            extra: Default::default(),
            reason: RouteExclusionReason::UnknownAvailability,
        }
    }
    /// Readiness whose evidence has expired; it projects like [`Self::unknown`].
    pub fn stale() -> Self {
        Self::Unavailable {
            extra: Default::default(),
            reason: RouteExclusionReason::StaleAvailability,
        }
    }
    /// Whether this is the projection of unknown readiness, never a verdict.
    pub fn is_unknown(&self) -> bool {
        matches!(
            self,
            Self::Unavailable {
                reason: RouteExclusionReason::UnknownAvailability
                    | RouteExclusionReason::StaleAvailability,
                ..
            }
        )
    }
}
/// A resolved candidate is always confidential; the resolver verifies the
/// referenced policy evidence. Deserializing a projection never verifies it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteConfidentiality {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub evidence_ref: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteExclusion {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub adapter: String,
    pub model: String,
    pub reason: RouteExclusionReason,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteExclusionReason {
    Disabled,
    MissingExecutable,
    UnknownAvailability,
    StaleAvailability,
    UnknownAccount,
    DisclosureDenied,
    ConfinementUnavailable,
    UnsupportedEffort,
    Exhausted,
    PacingReserve,
    /// Partial usage is a lower bound and its headroom is uncalibrated.
    UnknownPartialHeadroom,
    AuthRequired,
    /// The selected account's credential was observed past its expiry.
    LoginExpired,
    UnverifiedModel,
    /// Persisted evidence shows the executable's account differs from the registered one.
    AccountMismatch,
    /// A live refusal on the adapter's keyless free pool.
    FreeTierRefused,
    /// A live provider refusal scoped to the model or its verified pool.
    ProviderRefused,
    /// The account's persisted quota evidence is malformed, so only that account is excluded.
    InvalidAccountEvidence,
}
impl RouteExclusionReason {
    /// The typed owner's safe cause and next action for this exclusion.
    pub fn diagnostic_parts(&self) -> (&'static str, &'static str) {
        use crate::diagnostic::ProviderFailureFact;
        match self {
            Self::Disabled => (
                "the selected route is disabled",
                "enable an eligible route in the host configuration",
            ),
            Self::MissingExecutable => (
                "the selected adapter executable is unavailable",
                "install the adapter or select an available route",
            ),
            Self::UnknownAvailability => (
                "the selected account's readiness has not been verified",
                "run `provenant quota tick` or check the account login",
            ),
            Self::StaleAvailability => (
                "the selected account's provider availability observation is stale",
                "re-probe the selected account; if it remains unavailable, run `provenant quota tick` or check its login",
            ),
            Self::UnknownAccount => (
                "the selected account is not registered",
                "run `provenant accounts list` and select a registered account",
            ),
            Self::DisclosureDenied => (
                "the route cannot receive the requested data",
                "select a route permitted by the disclosure policy",
            ),
            Self::ConfinementUnavailable => (
                "the requested confinement is unavailable",
                "repair the host confinement setup before retrying",
            ),
            Self::UnsupportedEffort => (
                "the selected route does not support the requested effort",
                "choose a supported effort or another route",
            ),
            Self::Exhausted => (
                "the selected account's allowance is exhausted",
                "wait for the allowance to reset or select an eligible route",
            ),
            Self::PacingReserve => (
                "the selected account is held by its pacing reserve",
                "wait for the pacing window or adjust the configured reserve",
            ),
            Self::UnknownPartialHeadroom => (
                "partial usage does not establish available headroom",
                "refresh quota evidence or wait for the allowance to reset",
            ),
            Self::AuthRequired => (
                "the selected account requires login",
                ProviderFailureFact::AuthRequired.reason(),
            ),
            Self::LoginExpired => (
                "the selected account's login expired",
                ProviderFailureFact::AuthRequired.reason(),
            ),
            Self::UnverifiedModel => (
                "the selected model is not verified for this route",
                "select a verified model or refresh the catalogue",
            ),
            Self::AccountMismatch => (
                "the signed-in account differs from registration",
                "account mismatch; re-register the selected account",
            ),
            Self::FreeTierRefused => (
                "the provider refused the free tier",
                "free tier exhausted; use a paid route or wait for its allowance reset",
            ),
            Self::ProviderRefused => (
                "the provider refused this route or pool",
                "scoped provider refusal; wait or select an eligible route",
            ),
            Self::InvalidAccountEvidence => (
                "persisted account evidence is invalid",
                "repair or re-register the account before retrying",
            ),
        }
    }

    pub fn diagnostic(&self, code: &str) -> crate::diagnostic::Diagnostic {
        let (detail, next) = self.diagnostic_parts();
        crate::diagnostic::Diagnostic::new(code)
            .category("refused")
            .detail(detail)
            .next(next)
    }

    /// Whether persisted account evidence (mismatch, refusal or malformed
    /// records) excluded the route, as opposed to readiness, access or pacing.
    pub fn is_account_block(&self) -> bool {
        matches!(
            self,
            Self::AccountMismatch
                | Self::FreeTierRefused
                | Self::ProviderRefused
                | Self::InvalidAccountEvidence
        )
    }
}
