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
