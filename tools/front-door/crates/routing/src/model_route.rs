//! Stable route request and decision records shared by routing clients.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteStatus {
    Ok,
    UnknownAdapter,
    AdapterDisabled,
    AdapterAccountDefaultOnly,
    AdapterDefaultModelInvalid,
    AdapterFamilyMismatch,
    AccountDefaultConflictsWithCompatibility,
    AliasUnavailable,
    BrokerPreferenceConfigInvalid,
    FamilyNotAssuranceEligible,
    LeadFamilyRequired,
    ModelFamilyUnknown,
    ModelRequiredForBroker,
    ModelNotFound,
    InvalidEffort,
    NoCandidateAvailable,
    Rejected,
    RiskTierModelMismatch,
    RiskTierOverrideRequired,
    RiskTierOverrideUnavailable,
    RoleEffortConfigInvalid,
    SameFamilyForbidden,
    RouteInvalid,
    RouteRenamed,
    TaskClassCapabilityUnverified,
    #[default]
    InvalidInput,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Default for RouteDecision {
    fn default() -> Self {
        Self {
            schema_version: 1,
            status: RouteStatus::InvalidInput,
            adapter: None,
            role: None,
            task_class: None,
            route_source: None,
            alias: None,
            asked_model: None,
            resolved_model: None,
            catalog_model: None,
            model_family: None,
            family_source: None,
            requested_effort: None,
            effort: None,
            notes: Vec::new(),
            warnings: Vec::new(),
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

    use super::{RouteDecision, RouteRequest, RouteStatus};

    #[test]
    fn route_requests_and_decisions_are_typed_wire_records() {
        let request: RouteRequest =
            serde_json::from_value(json!({"model":"codex/gpt-6-luna","role":"worker"})).unwrap();
        assert_eq!(request.model.as_deref(), Some("codex/gpt-6-luna"));
        let decision = RouteDecision {
            schema_version: 1,
            status: RouteStatus::Ok,
            adapter: Some("codex".into()),
            ..Default::default()
        };
        let wire = serde_json::to_value(decision).unwrap();
        assert_eq!(wire["schema_version"], 1);
        assert_eq!(wire["status"], "ok");
        assert_eq!(wire["adapter"], "codex");
    }
}
