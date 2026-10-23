//! Shared delivery run invariants used by the receipt producer and checkpoint writer.
//!
//! Filesystem access is an explicit host boundary. The former Python helpers
//! read the live risk override artifact themselves; callers must provide the
//! confinement-owned reader before that check can be completed.

use provenant_core::py::{python_fromisoformat, python_truth};
use provenant_records::delivery_run_shape::ReceiptValue;
use serde_json::Value;
use std::path::Path;

pub const RISKS: [&str; 4] = ["routine", "substantial", "crucial", "terminal"];

#[derive(Debug, thiserror::Error)]
pub enum InvariantError {
    #[error("{0}")]
    Refused(String),
    #[error("unhashable type: '{kind}'")]
    Unhashable { kind: &'static str },
    #[error("risk policy JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Python integer is outside the i64/u64 range")]
    NumberOutOfRange,
}

impl InvariantError {
    pub fn error_type(&self) -> &'static str {
        if matches!(self, Self::Unhashable { .. }) {
            "TypeError"
        } else {
            "ReceiptError"
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskFactor {
    pub name: String,
    /// Retains the policy JSON's insertion order for first-invalid diagnostics.
    pub values: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskPolicy {
    pub factors: Vec<RiskFactor>,
}

impl RiskPolicy {
    /// Decode the source policy while retaining Python dict iteration order.
    pub fn from_json(text: &str) -> Result<Self, InvariantError> {
        let value = ReceiptValue::from_json_str(text).map_err(|error| match error {
            provenant_records::checked_json::DecodeError::NumberOutOfRange(_) => {
                InvariantError::NumberOutOfRange
            }
            provenant_records::checked_json::DecodeError::Json(error) => {
                InvariantError::Json(error)
            }
        })?;
        let tier_order = get(&value, "tier_order");
        let expected = RISKS
            .iter()
            .map(|risk| ReceiptValue::String((*risk).to_owned()))
            .collect::<Vec<_>>();
        if tier_order != Some(&ReceiptValue::Array(expected)) {
            return Err(InvariantError::Refused("risk policy is invalid".into()));
        }
        let factors = get(&value, "factors")
            .and_then(as_object)
            .ok_or_else(|| InvariantError::Refused("risk policy is invalid".into()))?;
        let mut parsed = Vec::with_capacity(factors.len());
        for (name, choices) in factors {
            let choices = as_object(choices)
                .ok_or_else(|| InvariantError::Refused("risk policy is invalid".into()))?;
            let mut values = Vec::with_capacity(choices.len());
            for (choice, risk) in choices {
                let risk = as_str(risk)
                    .ok_or_else(|| InvariantError::Refused("risk policy is invalid".into()))?;
                values.push((choice.clone(), risk.to_owned()));
            }
            parsed.push(RiskFactor {
                name: name.clone(),
                values,
            });
        }
        Ok(Self { factors: parsed })
    }
}

/// Supplies the policy and live artifact bytes through the owning product and
/// confinement layers. Implementations must resolve the product root per call.
pub trait InvariantHost {
    fn risk_policy(&self) -> Result<RiskPolicy, InvariantError>;

    /// Return the live bytes only after workspace and artifact scope checks.
    fn read_scoped_artifact(
        &self,
        run: &Value,
        workspace: &Path,
        relative_path: &str,
    ) -> Result<Option<Vec<u8>>, InvariantError>;
}

pub fn digest_bytes(raw: &[u8]) -> String {
    provenant_records::digest_bytes(raw)
}

pub fn derive_risk(
    assessment: &Value,
    policy: &RiskPolicy,
) -> Result<&'static str, InvariantError> {
    let assessment = assessment
        .as_object()
        .ok_or_else(|| InvariantError::Refused("risk-assessment must be an object".into()))?;
    if assessment.len() != policy.factors.len()
        || policy
            .factors
            .iter()
            .any(|factor| !assessment.contains_key(&factor.name))
    {
        return Err(InvariantError::Refused(
            "risk-assessment must cover every policy factor".into(),
        ));
    }
    let mut highest = 0_usize;
    for factor in &policy.factors {
        let selected = assessment.get(&factor.name).ok_or_else(|| {
            InvariantError::Refused("risk-assessment must cover every policy factor".into())
        })?;
        let choice = selected.as_str().ok_or_else(|| match selected {
            Value::Array(_) => InvariantError::Unhashable { kind: "list" },
            Value::Object(_) => InvariantError::Unhashable { kind: "dict" },
            _ => InvariantError::Refused(format!("risk-assessment.{} is invalid", factor.name)),
        })?;
        let tier = factor
            .values
            .iter()
            .find(|(key, _)| key == choice)
            .map(|(_, tier)| tier.as_str())
            .ok_or_else(|| {
                InvariantError::Refused(format!("risk-assessment.{} is invalid", factor.name))
            })?;
        let index = RISKS
            .iter()
            .position(|risk| *risk == tier)
            .ok_or_else(|| InvariantError::Refused(format!("'{tier}' is not in tuple")))?;
        highest = highest.max(index);
    }
    Ok(RISKS[highest])
}

pub fn validate_override(value: &Value, derived: &str) -> Result<(), InvariantError> {
    if value.get("status") != Some(&Value::String("approved".into()))
        || !truth_field(value.get("approved_by"))
        || !truth_field(value.get("evidence"))
        || !truth_field(value.get("reason"))
    {
        return Err(InvariantError::Refused(format!(
            "risk tier below derived {derived} requires an approved human override"
        )));
    }
    Ok(())
}

/// Validate immutable risk and timestamp constraints. `now_utc` is an explicit
/// UTC ISO timestamp so this policy is deterministic in tests and callers.
pub fn ensure_immutable_risk(
    run: &Value,
    workspace: &Path,
    now_utc: &str,
    host: &impl InvariantHost,
) -> Result<(), InvariantError> {
    let initial = run.get("initial_risk_tier").and_then(Value::as_str);
    let current = run.get("risk_tier").and_then(Value::as_str);
    let Some(initial) = initial.filter(|value| RISKS.contains(value)) else {
        return Err(InvariantError::Refused(
            "risk tier is immutable after init".into(),
        ));
    };
    if current != Some(initial) {
        return Err(InvariantError::Refused(
            "risk tier is immutable after init".into(),
        ));
    }
    let policy = host.risk_policy()?;
    let derived = derive_risk(run.get("risk_assessment").unwrap_or(&Value::Null), &policy)?;
    let override_value = run.get("risk_override");
    let initial_index = RISKS
        .iter()
        .position(|risk| *risk == initial)
        .ok_or_else(|| InvariantError::Refused("risk tier is immutable after init".into()))?;
    let derived_index = RISKS
        .iter()
        .position(|risk| *risk == derived)
        .ok_or_else(|| InvariantError::Refused("risk policy is invalid".into()))?;
    if initial_index < derived_index {
        let Some(value) = override_value.and_then(Value::as_object) else {
            return Err(InvariantError::Refused(
                "approved human risk override is missing".into(),
            ));
        };
        validate_override(&Value::Object(value.clone()), derived)?;
    }
    if let Some(value) = override_value
        .filter(|value| value.get("status") == Some(&Value::String("approved".into())))
    {
        validate_override(value, derived)?;
        let evidence_id = value.get("evidence");
        let linked = run
            .get("evidence")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|item| {
                item.is_object()
                    && item.get("id") == evidence_id
                    && item.get("kind") == Some(&Value::String("human".into()))
                    && item.get("status") == Some(&Value::String("pass".into()))
                    && item.get("gate") == Some(&Value::String("risk-override".into()))
            })
            .collect::<Vec<_>>();
        if linked.len() != 1 {
            return Err(InvariantError::Refused(
                "approved human risk override evidence is missing".into(),
            ));
        }
        let artifact_id = linked[0].get("artifact_id");
        let artifacts = run
            .get("artifacts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|item| item.is_object() && item.get("id") == artifact_id)
            .collect::<Vec<_>>();
        if artifacts.len() != 1
            || !truth_field(artifacts[0].get("path"))
            || !artifacts[0].get("path").is_some_and(Value::is_string)
        {
            return Err(InvariantError::Refused(
                "approved human risk override artifact is missing".into(),
            ));
        }
        let path = artifacts[0]
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                InvariantError::Refused("approved human risk override artifact is missing".into())
            })?;
        let raw = host
            .read_scoped_artifact(run, workspace, path)?
            .unwrap_or_default();
        let live_digest = digest_bytes(&raw);
        if raw.is_empty()
            || artifacts[0].get("digest").and_then(Value::as_str) != Some(live_digest.as_str())
        {
            return Err(InvariantError::Refused(
                "risk override artifact digest does not match live bytes".into(),
            ));
        }
    }
    let corrections = run
        .get("human_corrections")
        .and_then(Value::as_array)
        .ok_or_else(|| InvariantError::Refused("human_corrections must be a list".into()))?;
    for (index, correction) in corrections.iter().enumerate() {
        if !correction.is_object() {
            return Err(InvariantError::Refused(format!(
                "human_corrections[{index}] must be an object"
            )));
        }
        let field = format!("human_corrections[{index}].at");
        reject_future_timestamp(correction.get("at"), &field, now_utc)?;
    }
    Ok(())
}

pub fn ensure_run_open(run: &Value) -> Result<(), InvariantError> {
    if is_closed(run)? {
        Err(InvariantError::Refused("closed run is immutable".into()))
    } else {
        Ok(())
    }
}

pub fn is_closed(run: &Value) -> Result<bool, InvariantError> {
    let ordered = ReceiptValue::from_json_str(&serde_json::to_string(run)?)
        .map_err(|error| InvariantError::Refused(error.to_string()))?;
    provenant_records::delivery_run_shape::run_closed(&ordered)
        .map_err(|error| InvariantError::Refused(error.to_string()))
}

fn reject_future_timestamp(
    value: Option<&Value>,
    field: &str,
    now_utc: &str,
) -> Result<(), InvariantError> {
    let Some(value) = value
        .and_then(Value::as_str)
        .filter(|text| text.ends_with('Z'))
    else {
        return Err(InvariantError::Refused(format!(
            "{field} must be a UTC timestamp"
        )));
    };
    let parsed = python_fromisoformat(&value[..value.len() - 1])
        .map_err(|_| InvariantError::Refused(format!("{field} must be an ISO UTC timestamp")))?;
    let now = python_fromisoformat(now_utc.trim_end_matches('Z'))
        .map_err(|_| InvariantError::Refused("now must be an ISO UTC timestamp".into()))?;
    let parsed = make_utc(parsed);
    let now = make_utc(now);
    let parsed_micros = parsed
        .timestamp()
        .map_err(|_| InvariantError::Refused(format!("{field} must be an ISO UTC timestamp")))?
        .as_microsecond();
    let now_micros = now
        .timestamp()
        .map_err(|_| InvariantError::Refused("now must be an ISO UTC timestamp".into()))?
        .as_microsecond();
    let tolerance = 5_i64 * 60 * 1_000_000;
    let cutoff = now_micros
        .checked_add(tolerance)
        .ok_or(InvariantError::NumberOutOfRange)?;
    if parsed_micros > cutoff {
        return Err(InvariantError::Refused(format!(
            "{field} exceeds the future timestamp tolerance"
        )));
    }
    Ok(())
}

fn make_utc(value: provenant_core::py::PythonDateTime) -> provenant_core::py::PythonDateTime {
    provenant_core::py::PythonDateTime {
        offset_microseconds: Some(value.offset_microseconds.unwrap_or(0)),
        ..value
    }
}

fn truth_field(value: Option<&Value>) -> bool {
    value.is_some_and(python_truth)
}

fn get<'a>(value: &'a ReceiptValue, key: &str) -> Option<&'a ReceiptValue> {
    match value {
        ReceiptValue::Object(fields) => fields
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value),
        _ => None,
    }
}

fn as_object(value: &ReceiptValue) -> Option<&[(String, ReceiptValue)]> {
    match value {
        ReceiptValue::Object(fields) => Some(fields),
        _ => None,
    }
}

fn as_str(value: &ReceiptValue) -> Option<&str> {
    match value {
        ReceiptValue::String(value) => Some(value),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_derivation_and_override_truthiness() {
        let text = include_str!("../../../../../config/risk-policy.json");
        let policy = RiskPolicy::from_json(text).unwrap();
        let assessment: Value = serde_json::from_str(
            r#"{"blast_radius":"local","reversibility":"easy","data_sensitivity":"public","migration":"none","oracle_quality":"strong","external_effects":"none","critical_surface":"none"}"#,
        )
        .unwrap();
        assert_eq!(derive_risk(&assessment, &policy).unwrap(), "routine");
        let override_value = serde_json::json!({"status":"approved","approved_by":["chair"],"evidence":"E1","reason":"reason"});
        validate_override(&override_value, "crucial").unwrap();
    }

    #[test]
    fn run_open_matches_shared_record_predicate() {
        let open = serde_json::json!({"human_gates":{"release":{"status":"pending"}},"observation":{"status":"pending"}});
        ensure_run_open(&open).unwrap();
        let closed = serde_json::json!({"human_gates":{"release":{"status":"approved"}},"observation":{"status":"pass"}});
        assert_eq!(
            ensure_run_open(&closed).unwrap_err().to_string(),
            "closed run is immutable"
        );
    }

    #[test]
    fn future_correction_tolerance_is_inclusive() {
        reject_future_timestamp(
            Some(&Value::String("2026-07-10T00:05:00Z".into())),
            "at",
            "2026-07-10T00:00:00Z",
        )
        .unwrap();
        assert!(reject_future_timestamp(
            Some(&Value::String("2026-07-10T00:05:00.000001Z".into())),
            "at",
            "2026-07-10T00:00:00Z",
        )
        .is_err());
    }

    #[test]
    fn run_closure_matches_python_for_open_and_settled_shapes() {
        let cases = serde_json::json!([
            {"human_gates":{"release":{"status":"pending"}},"observation":{"status":"pending"}},
            {"human_gates":{"release":{"status":"approved"}},"observation":{"status":"pass"}},
            {"human_gates":{"release":{"status":"approved"}},"observation":{"status":"not_applicable"}},
            {"human_gates":{"release":{"status":"approved"}},"observation":{"status":"failed"}}
        ]);
        let expected =
            crate::test_support::oracle_script("oracle_receipts.py", "run_closed", &cases)
                .expect("required by PROVENANT_PORT_DIFF_REQUIRED");
        let actual = cases
            .as_array()
            .unwrap()
            .iter()
            .map(|run| is_closed(run).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(serde_json::json!(actual), expected);
    }
}
