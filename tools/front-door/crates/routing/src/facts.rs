use crate::{Catalogue, ModelId, RoutingError};
use provenant_providers::{AvailabilityRecord, VerifiedLineage};
use provenant_quota::QuotaSnapshot;
use serde::{Deserialize, Serialize};

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
