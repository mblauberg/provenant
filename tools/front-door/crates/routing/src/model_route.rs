//! Stable route request and decision records shared by routing clients.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use thiserror::Error;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteStatus {
    Ok,
    UnknownAdapter,
    AdapterDisabled,
    AdapterAccountDefaultOnly,
    AdapterCompatibilityInvalid,
    AdapterCompatibilityUnavailable,
    AdapterCompatibilityUnknown,
    AdapterDefaultModelInvalid,
    AdapterEffortMismatch,
    AdapterEffortUnresolved,
    AdapterFamilyMismatch,
    AdapterFamilyForbidden,
    AdapterModelForbidden,
    AccountDefaultConflictsWithCompatibility,
    AliasUnavailable,
    BrokerPreferenceConfigInvalid,
    CapabilityDiscoveryFailed,
    CapabilityModelUnavailable,
    CapabilitySnapshotStale,
    CapabilitySnapshotUntrusted,
    EffortCapabilityUnverified,
    EffortUnsupported,
    EffortVerified,
    EndpointAdapterUnsupported,
    EndpointConfigInvalid,
    EndpointTokenMissing,
    EffortPolicyConfigInvalid,
    FamilyNotAssuranceEligible,
    LeadFamilyRequired,
    ModelFamilyUnknown,
    ModelAmbiguous,
    ModelNotFound,
    ModelRequiredForBroker,
    NoEffortAvailable,
    InvalidEffort,
    NoCandidateAvailable,
    Rejected,
    RiskTierModelMismatch,
    RiskTierAliasMismatch,
    RiskTierConfigInvalid,
    RiskTierEffortAboveCeiling,
    RiskTierOverrideRequired,
    RiskTierOverrideUnavailable,
    RiskTierRoleMismatch,
    RouteInputConflict,
    RouteInputMissing,
    RoleEffortConfigInvalid,
    SameFamilyForbidden,
    RouteInvalid,
    RouteRenamed,
    TaskClassCapabilityUnverified,
    TaskClassEffortBelowFloor,
    TaskClassEffortConflict,
    TaskClassEffortConfigInvalid,
    TaskClassEffortUnresolved,
    TaskClassModelConflict,
    TaskClassConfigInvalid,
    UnknownAlias,
    UnknownEndpoint,
    UnknownTaskClass,
    #[default]
    InvalidInput,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum NullableField<T> {
    #[default]
    Absent,
    Null,
    Value(T),
}

impl<T: Serialize> Serialize for NullableField<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Absent | Self::Null => serializer.serialize_none(),
            Self::Value(value) => value.serialize(serializer),
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for NullableField<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Option::<T>::deserialize(deserializer).map(|value| value.map_or(Self::Null, Self::Value))
    }
}

fn is_absent<T>(field: &NullableField<T>) -> bool {
    matches!(field, NullableField::Absent)
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RouteRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_override_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RouteDecision {
    pub schema_version: u32,
    pub status: RouteStatus,
    #[serde(default, skip_serializing_if = "is_absent")]
    pub adapter: NullableField<String>,
    #[serde(default, skip_serializing_if = "is_absent")]
    pub role: NullableField<String>,
    #[serde(default, skip_serializing_if = "is_absent")]
    pub task_class: NullableField<String>,
    #[serde(default, skip_serializing_if = "is_absent")]
    pub route_source: NullableField<String>,
    #[serde(default, skip_serializing_if = "is_absent")]
    pub alias: NullableField<String>,
    #[serde(default, skip_serializing_if = "is_absent")]
    pub asked_model: NullableField<String>,
    #[serde(default, skip_serializing_if = "is_absent")]
    pub resolved_model: NullableField<String>,
    #[serde(default, skip_serializing_if = "is_absent")]
    pub catalog_model: NullableField<String>,
    #[serde(default, skip_serializing_if = "is_absent")]
    pub model_family: NullableField<String>,
    #[serde(default, skip_serializing_if = "is_absent")]
    pub family_source: NullableField<String>,
    #[serde(default, skip_serializing_if = "is_absent")]
    pub requested_effort: NullableField<String>,
    #[serde(default, skip_serializing_if = "is_absent")]
    pub effort: NullableField<String>,
    #[serde(default, skip_serializing_if = "is_absent")]
    pub notes: NullableField<Vec<String>>,
    #[serde(default, skip_serializing_if = "is_absent")]
    pub warnings: NullableField<Vec<String>>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Default for RouteDecision {
    fn default() -> Self {
        Self {
            schema_version: 1,
            status: RouteStatus::InvalidInput,
            adapter: NullableField::Absent,
            role: NullableField::Absent,
            task_class: NullableField::Absent,
            route_source: NullableField::Absent,
            alias: NullableField::Absent,
            asked_model: NullableField::Absent,
            resolved_model: NullableField::Absent,
            catalog_model: NullableField::Absent,
            model_family: NullableField::Absent,
            family_source: NullableField::Absent,
            requested_effort: NullableField::Absent,
            effort: NullableField::Absent,
            notes: NullableField::Absent,
            warnings: NullableField::Absent,
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Error)]
pub enum RouteError {
    #[error("invalid route request: {0}")]
    InvalidRequest(String),
    #[error("route catalogue is malformed")]
    InvalidCatalogue,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{NullableField, RouteDecision, RouteRequest, RouteStatus};

    #[test]
    fn route_requests_and_decisions_are_typed_wire_records() {
        let request: RouteRequest =
            serde_json::from_value(json!({"model":"codex/gpt-6-luna","role":"worker"})).unwrap();
        assert_eq!(request.model.as_deref(), Some("codex/gpt-6-luna"));
        let decision = RouteDecision {
            schema_version: 1,
            status: RouteStatus::Ok,
            adapter: NullableField::Value("codex".into()),
            notes: NullableField::Value(Vec::new()),
            warnings: NullableField::Value(Vec::new()),
            ..Default::default()
        };
        let wire = serde_json::to_value(decision).unwrap();
        assert_eq!(wire["schema_version"], 1);
        assert_eq!(wire["status"], "ok");
        assert_eq!(wire["adapter"], "codex");
        assert_eq!(wire["notes"], json!([]));
        assert_eq!(wire["warnings"], json!([]));
        assert!(wire.get("role").is_none());
        let explicit_null = RouteDecision {
            role: NullableField::Null,
            notes: NullableField::Null,
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_value(explicit_null).unwrap()["role"],
            json!(null)
        );
    }

    #[test]
    fn refusal_statuses_from_the_python_contract_deserialize() {
        let effort: RouteDecision = serde_json::from_value(json!({
            "schema_version":1,
            "status":"effort_unsupported"
        }))
        .unwrap();
        let ambiguous: RouteDecision = serde_json::from_value(json!({
            "schema_version":1,
            "status":"model_ambiguous"
        }))
        .unwrap();
        assert_eq!(effort.status, RouteStatus::EffortUnsupported);
        assert_eq!(ambiguous.status, RouteStatus::ModelAmbiguous);
    }
}
