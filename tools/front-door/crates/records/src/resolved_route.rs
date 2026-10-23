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
    DisclosureDenied,
    ConfinementUnavailable,
    UnsupportedEffort,
    Exhausted,
    AuthRequired,
    UnverifiedModel,
}

#[derive(Debug, thiserror::Error)]
#[error("resolved alternative {index} is unavailable")]
pub struct RouteSelectionError {
    pub index: u64,
}
impl ResolvedRoute {
    /// Project an already resolved alternative for an attempt. Run chooses the
    /// ordinal within its retry budget; it never rebuilds or resolves a model.
    /// The returned attempt has no implicit further fallback chain.
    pub fn select_alternative(&self, index: u64) -> Result<Self, RouteSelectionError> {
        let selected = usize::try_from(index)
            .ok()
            .and_then(|index| self.alternatives.0.get(index))
            .ok_or(RouteSelectionError { index })?;
        Ok(Self {
            adapter: selected.adapter.clone(),
            model: selected.model.clone(),
            effort: selected.effort.clone(),
            catalogue_revision: self.catalogue_revision.clone(),
            account_key: selected.account_key.clone(),
            host: selected.host.clone(),
            alternatives: RouteAlternatives::default(),
            exclusions: self.exclusions.clone(),
            eligibility: selected.eligibility.clone(),
            readiness: selected.readiness.clone(),
            confidentiality: selected.confidentiality.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selecting_explicit_alternatives_preserves_decisions_and_catalogue()
    -> Result<(), Box<dyn std::error::Error>> {
        let alternative = |model: &str, effort: &str| RouteAlternative {
            adapter: "fixture".into(),
            model: model.into(),
            effort: Some(effort.into()),
            account_key: Some(format!("account-{model}")),
            host: format!("host-{model}"),
            eligibility: RouteEligibility::Eligible,
            readiness: RouteReadiness::Ready {
                availability_ref: format!("availability-{model}"),
                expires_unix_seconds: 100,
            },
            confidentiality: RouteConfidentiality {
                evidence_ref: format!("disclosure-{model}"),
            },
        };
        let alternatives = vec![alternative("first", "high"), alternative("second", "low")];
        let route = ResolvedRoute {
            adapter: "primary".into(),
            model: "primary".into(),
            effort: None,
            catalogue_revision: "revision".into(),
            account_key: None,
            host: "primary".into(),
            alternatives: RouteAlternatives::new(alternatives.clone())?,
            exclusions: vec![RouteExclusion {
                adapter: "excluded".into(),
                model: "excluded".into(),
                reason: RouteExclusionReason::Disabled,
            }],
            eligibility: RouteEligibility::Eligible,
            readiness: RouteReadiness::Unavailable {
                reason: RouteExclusionReason::Disabled,
            },
            confidentiality: RouteConfidentiality {
                evidence_ref: "primary".into(),
            },
        };
        for (index, expected) in alternatives.iter().enumerate() {
            let selected = route.select_alternative(u64::try_from(index)?)?;
            assert_eq!(
                (
                    &selected.adapter,
                    &selected.model,
                    &selected.effort,
                    &selected.account_key,
                    &selected.host
                ),
                (
                    &expected.adapter,
                    &expected.model,
                    &expected.effort,
                    &expected.account_key,
                    &expected.host
                )
            );
            assert_eq!(selected.catalogue_revision, route.catalogue_revision);
            assert_eq!(selected.exclusions, route.exclusions);
            assert_eq!(selected.eligibility, expected.eligibility);
            assert_eq!(selected.readiness, expected.readiness);
            assert_eq!(selected.confidentiality, expected.confidentiality);
            assert!(selected.alternatives.as_slice().is_empty());
        }
        assert!(route.select_alternative(2).is_err());
        assert!(route.select_alternative(u64::MAX).is_err());
        Ok(())
    }
}
