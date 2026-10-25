use crate::{Catalogue, ModelId, RoutingError};
use provenant_providers::{AvailabilityRecord, VerifiedLineage};
use provenant_quota::QuotaSnapshot;
use provenant_records::RouteExclusionReason;
#[cfg(test)]
use provenant_records::{ResolvedRoute, RouteReadiness};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::collections::BTreeMap;

/// Cached provider availability, independent of launch permissions.
#[derive(Debug, Clone, Serialize)]
pub struct AvailabilityView {
    pub adapter: String,
    pub record: Option<AvailabilitySummary>,
    pub age_seconds: Option<u64>,
    pub reason: Option<RouteExclusionReason>,
}
/// Public cached projection. Exact observations stay in the bounded input used
/// by resolve; listing their count keeps the unpaged CLI result bounded.
#[derive(Debug, Clone, Serialize)]
pub struct AvailabilitySummary {
    pub executable: String,
    pub version: String,
    pub fixture_ref: String,
    pub catalogue_revision: String,
    pub account_key: Option<String>,
    pub observed_model_count: u64,
    pub last_preflight_error: Option<provenant_providers::PreflightFailure>,
    pub observed_unix_seconds: i64,
    pub expires_unix_seconds: i64,
}
impl From<&AvailabilityRecord> for AvailabilitySummary {
    fn from(record: &AvailabilityRecord) -> Self {
        Self {
            executable: record.executable.clone(),
            version: record.version.clone(),
            fixture_ref: record.fixture_ref.clone(),
            catalogue_revision: record.catalogue_revision.clone(),
            account_key: record.account_key.clone(),
            observed_model_count: record.observed_models.len() as u64,
            last_preflight_error: record.last_preflight_error,
            observed_unix_seconds: record.observed_unix_seconds,
            expires_unix_seconds: record.expires_unix_seconds,
        }
    }
}
pub fn validate_availability(
    record: &AvailabilityRecord,
    adapter: &str,
) -> Result<(), RoutingError> {
    if record.schema_version != 1
        || record.adapter != adapter
        || record.executable.is_empty()
        || record.executable.len() > 4096
        || record.version.is_empty()
        || record.version.len() > 128
        || record.fixture_ref.is_empty()
        || record.fixture_ref.len() > 512
        || record.catalogue_revision.len() > 128
        || record.observed_unix_seconds >= record.expires_unix_seconds
        || record.observed_models.len() > 256
        || record
            .observed_models
            .iter()
            .any(|m| m.is_empty() || m.len() > 512 || m.chars().any(char::is_control))
        || record
            .account_key
            .as_deref()
            .is_some_and(|s| !crate::request::identifier(s))
    {
        return Err(RoutingError::Availability);
    }
    Ok(())
}

/// Composition supplies a model-scoped, current quota observation. No unknown or
/// stale observation can freshly exhaust a model or another account/pool.
pub struct QuotaObservation {
    pub model: ModelId,
    pub snapshot: QuotaSnapshot,
    pub expires_unix_seconds: i64,
}

/// Stored evidence is a projection only; it cannot construct a verified fact.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FamilyEvidenceRecord {
    pub schema_version: u64,
    pub model: ModelId,
    pub catalogue_revision: String,
    pub family: Option<String>,
    pub evidence_ref: String,
    pub observed_unix_seconds: i64,
    pub expires_unix_seconds: i64,
}
#[derive(Debug, Serialize)]
pub struct FamilyFact {
    record: FamilyEvidenceRecord,
}
impl FamilyFact {
    pub fn from_verified(
        model: ModelId,
        lineage: &VerifiedLineage,
        observed: i64,
        expires: i64,
        catalogue: &Catalogue,
    ) -> Result<Self, RoutingError> {
        if observed >= expires {
            return Err(RoutingError::Availability);
        }
        let family = catalogue.model(&model).and_then(|entry| {
            if lineage.adapter() == model.adapter()
                && lineage.observed_model() == Some(model.model())
                && lineage.catalogue_revision() == catalogue.revision
                && !lineage.evidence_ref().is_empty()
                && entry.family.as_deref() == lineage.family()
            {
                lineage.family().map(str::to_owned)
            } else {
                None
            }
        });
        Ok(Self {
            record: FamilyEvidenceRecord {
                schema_version: 1,
                model,
                catalogue_revision: catalogue.revision.clone(),
                family,
                evidence_ref: lineage.evidence_ref().into(),
                observed_unix_seconds: observed,
                expires_unix_seconds: expires,
            },
        })
    }
    pub fn record(&self) -> &FamilyEvidenceRecord {
        &self.record
    }
}

/// A snapshot can only be built by resolving against current, validated inputs;
/// deserialized ResolvedRoute projections cannot assert fresh availability.
#[cfg(test)]
pub(crate) struct RoutingSnapshot<'a> {
    catalogue_revision: String,
    now_unix_seconds: i64,
    routes: Vec<ResolvedRoute>,
    families: &'a [FamilyFact],
}
#[cfg(test)]
impl<'a> RoutingSnapshot<'a> {
    pub fn new(
        requests: &[crate::RouteRequest],
        context: &crate::RoutingContext<'_>,
        families: &'a [FamilyFact],
    ) -> Result<Self, RoutingError> {
        if requests.len() > 256 || families.len() > 256 {
            return Err(RoutingError::ReviewDemand);
        }
        let routes = requests
            .iter()
            .map(|r| crate::resolve(r, context))
            .collect::<Result<_, _>>()?;
        Ok(Self {
            catalogue_revision: context.catalogue.revision.clone(),
            now_unix_seconds: context.now.as_timestamp().as_second(),
            routes,
            families,
        })
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg(test)]
pub(crate) struct ReviewDemand {
    pub family_slots: BTreeMap<String, u64>,
}
#[derive(Debug, Serialize)]
#[cfg(test)]
pub(crate) struct ReviewAvailability {
    catalogue_revision: String,
    expires_unix_seconds: Option<i64>,
    families: BTreeMap<String, u64>,
    blockers: Vec<ReviewBlocker>,
    satisfied: bool,
}
#[cfg(test)]
impl ReviewAvailability {
    pub fn satisfied(&self) -> bool {
        self.satisfied
    }
}
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg(test)]
pub(crate) enum ReviewBlocker {
    Route {
        adapter: String,
        model: String,
        reason: RouteExclusionReason,
    },
    UnknownFamily {
        model: String,
    },
    FamilySlots {
        family: String,
        required: u64,
        available: u64,
    },
}
#[cfg(test)]
pub(crate) fn review_availability(
    snapshot: &RoutingSnapshot<'_>,
    demand: &ReviewDemand,
) -> Result<ReviewAvailability, RoutingError> {
    if demand.family_slots.is_empty()
        || demand.family_slots.len() > 16
        || demand
            .family_slots
            .iter()
            .any(|(f, n)| !crate::request::identifier(f) || !(1..=64).contains(n))
        || snapshot.routes.len() > 256
        || snapshot.families.len() > 256
    {
        return Err(RoutingError::ReviewDemand);
    }
    let mut families = BTreeMap::<String, u64>::new();
    let mut blockers = Vec::new();
    let mut expiry = None::<i64>;
    let mut seen = std::collections::BTreeSet::new();
    for route in &snapshot.routes {
        if !seen.insert((&route.adapter, &route.model)) {
            continue;
        }
        let ready_until = match &route.readiness {
            RouteReadiness::Ready {
                expires_unix_seconds,
                ..
            } if route.catalogue_revision == snapshot.catalogue_revision
                && *expires_unix_seconds > snapshot.now_unix_seconds =>
            {
                *expires_unix_seconds
            }
            _ => {
                let reason = match &route.readiness {
                    RouteReadiness::Unavailable { reason } => reason.clone(),
                    _ => RouteExclusionReason::StaleAvailability,
                };
                blockers.push(ReviewBlocker::Route {
                    adapter: route.adapter.clone(),
                    model: route.model.clone(),
                    reason,
                });
                continue;
            }
        };
        let fact = snapshot.families.iter().map(|f| f.record()).find(|f| {
            f.model.adapter() == route.adapter
                && f.model.model() == route.model
                && f.catalogue_revision == snapshot.catalogue_revision
                && f.observed_unix_seconds <= snapshot.now_unix_seconds
                && f.expires_unix_seconds > snapshot.now_unix_seconds
                && f.family.is_some()
        });
        if let Some(fact) = fact {
            if let Some(family) = &fact.family {
                *families.entry(family.clone()).or_default() += 1;
                let until = ready_until.min(fact.expires_unix_seconds);
                expiry = Some(expiry.map_or(until, |current| current.min(until)));
            }
        } else {
            blockers.push(ReviewBlocker::UnknownFamily {
                model: format!("{}/{}", route.adapter, route.model),
            });
        }
    }
    let mut satisfied = true;
    for (family, required) in &demand.family_slots {
        let available = families.get(family).copied().unwrap_or(0);
        if available < *required {
            satisfied = false;
            blockers.push(ReviewBlocker::FamilySlots {
                family: family.clone(),
                required: *required,
                available,
            });
        }
    }
    Ok(ReviewAvailability {
        catalogue_revision: snapshot.catalogue_revision.clone(),
        expires_unix_seconds: expiry,
        families,
        blockers,
        satisfied,
    })
}
