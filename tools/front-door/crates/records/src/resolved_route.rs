//! Routing's immutable decision, shared with providers and run.
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedRoute {
    pub adapter: String,
    /// Full provider/model path; never split beyond the adapter separator.
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
}

/// An explicit fallback has the same access and disclosure requirements as the
/// primary. Run receives the decision, rather than reconstructing candidates.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteAlternative {
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
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum RouteReadiness {
    Ready {
        availability_ref: String,
        expires_unix_seconds: i64,
    },
    Unavailable {
        reason: RouteExclusionReason,
    },
}
/// A resolved candidate is always confidential; the resolver verifies the
/// referenced policy evidence. Deserializing a projection never verifies it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteConfidentiality {
    pub evidence_ref: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteExclusion {
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
    AuthRequired,
    UnverifiedModel,
}
