//! Receipt producer command policy behind explicit store and filesystem seams.
//!
//! The source CLI and receipt store are external adapters. This module owns
//! command validation and record mutations while the host owns locking,
//! contained reads, writes and JSONL publication.

use provenant_core::py::python_repr;
use provenant_records::delivery_run_shape::ReceiptValue;
use serde_json::{Number, Value};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ReceiptCommandError {
    #[error("{0}")]
    Refused(String),
    #[error("Python integer is outside the i64/u64 range")]
    NumberOutOfRange,
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub trait ReceiptCommandHost {
    fn require_identifier(&self, value: &str, field: &str) -> Result<String, ReceiptCommandError>;
    fn safe_workspace_path(
        &self,
        workspace: &Path,
        value: &str,
        field: &str,
    ) -> Result<(PathBuf, String), ReceiptCommandError>;
    fn ensure_allowed_source_target(
        &self,
        run: &Value,
        workspace: &Path,
        target: &Path,
    ) -> Result<(), ReceiptCommandError>;
    fn ensure_allowed_artifact_target(
        &self,
        run: &Value,
        workspace: &Path,
        target: &Path,
    ) -> Result<(), ReceiptCommandError>;
    fn read_file(&self, path: &Path) -> Result<Option<Vec<u8>>, ReceiptCommandError>;
    fn path_exists(&self, path: &Path) -> Result<bool, ReceiptCommandError>;
    fn path_is_file(&self, path: &Path) -> Result<bool, ReceiptCommandError>;
    fn digest_bytes(&self, bytes: &[u8]) -> String;
    fn utc_now(&self) -> String;
    fn profile_judgement_gate(&self, profile: &str) -> Result<String, ReceiptCommandError>;
    fn ensure_new_evidence_id(
        &self,
        run: &Value,
        evidence_id: &str,
    ) -> Result<(), ReceiptCommandError>;
    fn append_jsonl(&self, path: &Path, row: &ReceiptValue) -> Result<(), ReceiptCommandError>;
    fn resolve_run_dir(&self, value: &Path) -> Result<(PathBuf, PathBuf), ReceiptCommandError>;
    fn show_locked(&self, run_dir: &Path) -> Result<ReceiptValue, ReceiptCommandError>;
    fn mutate_run<F>(&self, run_dir: &Path, apply: F) -> Result<Value, ReceiptCommandError>
    where
        F: FnOnce(&mut Value, &Path, &Path) -> Result<Value, ReceiptCommandError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointArgs {
    pub run_dir: PathBuf,
    pub current_slice: String,
    pub next_action: String,
    pub in_flight: Vec<String>,
    pub artifacts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObservationArgs {
    pub run_dir: PathBuf,
    pub evidence_id: String,
    pub gate: String,
    pub artifact_id: String,
    pub measured_value: f64,
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewArgs {
    pub run_dir: PathBuf,
    pub review_id: String,
    pub role: String,
    pub artifact: Option<String>,
    pub route_receipt: Option<String>,
    pub reviewer_id: Option<String>,
    pub adapter: String,
    pub provider_family: String,
    pub model: Option<String>,
    pub lenses: Vec<String>,
    pub status: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairArgs {
    pub run_dir: PathBuf,
    pub reason: String,
    pub failing: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingArgs {
    pub run_dir: PathBuf,
    pub review: String,
    pub finding: String,
    pub disposition: String,
    pub severity: Option<String>,
    pub duplicate_of: Option<String>,
    pub reviewer_family: Option<String>,
    pub model: Option<String>,
    pub reviewer_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArgumentSpec {
    pub name: &'static str,
    pub required: bool,
    pub repeated: bool,
}

/// Declarative parser contract for the crate's future CLI edge. The parser
/// itself remains at that edge, where `clap` owns argv and help rendering.
pub const REVIEW_ARGUMENTS: &[ArgumentSpec] = &[
    ArgumentSpec {
        name: "run-dir",
        required: true,
        repeated: false,
    },
    ArgumentSpec {
        name: "id",
        required: true,
        repeated: false,
    },
    ArgumentSpec {
        name: "role",
        required: true,
        repeated: false,
    },
    ArgumentSpec {
        name: "artifact",
        required: false,
        repeated: false,
    },
    ArgumentSpec {
        name: "route-receipt",
        required: false,
        repeated: false,
    },
    ArgumentSpec {
        name: "reviewer-id",
        required: false,
        repeated: false,
    },
    ArgumentSpec {
        name: "adapter",
        required: true,
        repeated: false,
    },
    ArgumentSpec {
        name: "provider-family",
        required: true,
        repeated: false,
    },
    ArgumentSpec {
        name: "model",
        required: false,
        repeated: false,
    },
    ArgumentSpec {
        name: "lens",
        required: true,
        repeated: true,
    },
    ArgumentSpec {
        name: "status",
        required: false,
        repeated: false,
    },
    ArgumentSpec {
        name: "reason",
        required: false,
        repeated: false,
    },
];

pub fn profile_judgement_gate(
    profile: &str,
    registry: &Value,
) -> Result<String, ReceiptCommandError> {
    let gates = py_index(registry, "profiles")
        .and_then(|profiles| py_index(profiles, profile))
        .and_then(|profile| py_index(profile, "required_evidence"))
        .and_then(|required| py_index(required, "judgement"))
        .map_err(|error| {
            ReceiptCommandError::Refused(format!(
                "delivery profile registry is unreadable: {error}"
            ))
        })?
        .as_array();
    let Some(first) = gates
        .and_then(|items| items.first())
        .and_then(Value::as_str)
    else {
        return Err(ReceiptCommandError::Refused(format!(
            "profile {profile} has no judgement evidence gate"
        )));
    };
    Ok(first.to_owned())
}

fn py_index<'a>(value: &'a Value, key: &str) -> Result<&'a Value, String> {
    match value {
        Value::Object(fields) => fields.get(key).ok_or_else(|| python_repr(key)),
        Value::Array(_) => Err("list indices must be integers or slices, not str".into()),
        Value::String(_) => Err("string indices must be integers, not 'str'".into()),
        Value::Null => Err("'NoneType' object is not subscriptable".into()),
        Value::Bool(_) => Err("'bool' object is not subscriptable".into()),
        Value::Number(number) if number.is_i64() || number.is_u64() => {
            Err("'int' object is not subscriptable".into())
        }
        Value::Number(_) => Err("'float' object is not subscriptable".into()),
    }
}

pub fn profile_judgement_gate_file(
    profile: &str,
    profile_path: &Path,
) -> Result<String, ReceiptCommandError> {
    let raw = std::fs::read(profile_path).map_err(|source| ReceiptCommandError::Io {
        path: profile_path.into(),
        source,
    })?;
    let registry = serde_json::from_slice(&raw).map_err(|error| {
        ReceiptCommandError::Refused(format!("delivery profile registry is unreadable: {error}"))
    })?;
    profile_judgement_gate(profile, &registry)
}

pub fn add_review_artifact(
    run: &mut Value,
    workspace: &Path,
    artifact_id: &str,
    path: &str,
    host: &impl ReceiptCommandHost,
) -> Result<(Value, String, String, Vec<u8>), ReceiptCommandError> {
    host.require_identifier(artifact_id, "review artifact id")?;
    let (target, relative) = host.safe_workspace_path(workspace, path, "review artifact path")?;
    host.ensure_allowed_source_target(run, workspace, &target)?;
    host.ensure_allowed_artifact_target(run, workspace, &target)?;
    let raw = if host.path_is_file(&target)? {
        host.read_file(&target)?.unwrap_or_default()
    } else {
        Vec::new()
    };
    if raw.is_empty() {
        return Err(ReceiptCommandError::Refused(format!(
            "review artifact must be existing and non-empty: {path}"
        )));
    }
    let digest = host.digest_bytes(&raw);
    if let Some(existing) = run
        .get("artifacts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|item| {
            item.is_object() && item.get("id").and_then(Value::as_str) == Some(artifact_id)
        })
    {
        if existing.get("path").and_then(Value::as_str) != Some(relative.as_str())
            || existing.get("digest").and_then(Value::as_str) != Some(digest.as_str())
        {
            return Err(ReceiptCommandError::Refused(format!(
                "review artifact id conflicts with live bytes: {artifact_id}"
            )));
        }
        return Ok((existing.clone(), relative, digest, raw));
    }
    let media_type = if target.extension().and_then(|s| s.to_str()) == Some("json") {
        "application/json"
    } else {
        "text/markdown"
    };
    let item = serde_json::json!({
        "id": artifact_id,
        "path": relative,
        "media_type": media_type,
        "artifact_type": "evidence",
        "digest": digest,
        "class": "evidence",
        "owner": "delivery-chair",
        "retention": "risk-policy"
    });
    let artifacts = run
        .get_mut("artifacts")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| ReceiptCommandError::Refused("artifacts must be a list".into()))?;
    artifacts.push(item.clone());
    Ok((item, relative, digest, raw))
}

pub fn command_checkpoint_set(
    args: &CheckpointArgs,
    host: &impl ReceiptCommandHost,
) -> Result<Value, ReceiptCommandError> {
    let mut artifacts = args.artifacts.clone();
    let run_dir = args.run_dir.clone();
    host.mutate_run(&run_dir, |run, _run_dir, workspace| {
        let checkpoint = run
            .get_mut("checkpoint")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| ReceiptCommandError::Refused("checkpoint must be an object".into()))?;
        let generation = checkpoint.get("generation");
        let generation = nonnegative_integer(generation).ok_or_else(|| {
            ReceiptCommandError::Refused(
                "checkpoint.generation must be a non-negative integer".into(),
            )
        })?;
        let in_flight = dedup_strings(&args.in_flight);
        if in_flight.iter().any(String::is_empty) {
            return Err(ReceiptCommandError::Refused(
                "in-flight IDs must be non-empty".into(),
            ));
        }
        let existing = checkpoint
            .get("artifact_paths")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                ReceiptCommandError::Refused("checkpoint.artifact_paths must be a list".into())
            })?;
        let mut additions = Vec::with_capacity(artifacts.len());
        for artifact in artifacts.drain(..) {
            let (target, relative) =
                host.safe_workspace_path(workspace, &artifact, "checkpoint artifact")?;
            if !host.path_is_file(&target)? {
                return Err(ReceiptCommandError::Refused(format!(
                    "checkpoint artifact does not exist: {artifact}"
                )));
            }
            additions.push(relative);
        }
        let existing_paths = existing
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let mut artifact_paths = existing_paths;
        artifact_paths.extend(additions);
        let artifact_paths = dedup_strings(&artifact_paths)
            .into_iter()
            .map(Value::String)
            .collect::<Vec<_>>();
        let generation = generation
            .checked_add(1)
            .ok_or(ReceiptCommandError::NumberOutOfRange)?;
        checkpoint.insert("generation".into(), Value::Number(generation.into()));
        checkpoint.insert(
            "current_slice".into(),
            Value::String(args.current_slice.clone()),
        );
        checkpoint.insert(
            "next_action".into(),
            Value::String(args.next_action.clone()),
        );
        checkpoint.insert("in_flight".into(), serde_json::to_value(in_flight)?);
        checkpoint.insert("artifact_paths".into(), Value::Array(artifact_paths));
        Ok(serde_json::json!({"generation": generation}))
    })
}

pub fn artifact_corroborates_measurement(value: &Value, gate: &str, measured_value: f64) -> bool {
    if !measured_value.is_finite()
        || !value.is_object()
        || value.get("gate").and_then(Value::as_str) != Some(gate)
    {
        return false;
    }
    let Some(recorded) = value.get("measured_value") else {
        return false;
    };
    if recorded.is_boolean() || !recorded.is_number() {
        return false;
    }
    Number::from_f64(measured_value).is_some_and(|measured| {
        provenant_core::py::python_json_eq(recorded, &Value::Number(measured))
    })
}

pub fn command_evidence_observation(
    args: &ObservationArgs,
    host: &impl ReceiptCommandHost,
) -> Result<Value, ReceiptCommandError> {
    let evidence_id = host.require_identifier(&args.evidence_id, "evidence id")?;
    if !args.measured_value.is_finite() {
        return Err(ReceiptCommandError::Refused(
            "observation measured value must be finite".into(),
        ));
    }
    let run_dir = args.run_dir.clone();
    host.mutate_run(&run_dir, |run, _run_dir, workspace| {
        host.ensure_new_evidence_id(run, &evidence_id)?;
        let artifact = find_artifact(run, &args.artifact_id)?;
        let path = artifact
            .get("path")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
            .ok_or_else(|| {
                ReceiptCommandError::Refused(
                    "observation evidence must use a local artifact".into(),
                )
            })?;
        let (target, _) =
            host.safe_workspace_path(workspace, path, "observation evidence artifact")?;
        let raw = if host.path_is_file(&target)? {
            host.read_file(&target)?.unwrap_or_default()
        } else {
            Vec::new()
        };
        if raw.is_empty()
            || artifact.get("digest").and_then(Value::as_str)
                != Some(host.digest_bytes(&raw).as_str())
        {
            return Err(ReceiptCommandError::Refused(
                "observation evidence artifact must match existing non-empty bytes".into(),
            ));
        }
        let artifact_content = serde_json::from_slice(&raw).unwrap_or(Value::Null);
        if !artifact_corroborates_measurement(&artifact_content, &args.gate, args.measured_value) {
            return Err(ReceiptCommandError::Refused(format!(
                "observation evidence artifact does not corroborate measured value {}",
                python_float_g(args.measured_value)
            )));
        }
        let mut sources = Vec::new();
        for source in &args.sources {
            let (source_target, relative) =
                host.safe_workspace_path(workspace, source, "observation evidence source")?;
            host.ensure_allowed_source_target(run, workspace, &source_target)?;
            if !host.path_exists(&source_target)? {
                return Err(ReceiptCommandError::Refused(format!(
                    "observation evidence source does not exist: {source}"
                )));
            }
            sources.push(relative);
        }
        let row = serde_json::json!({
            "id": evidence_id,
            "kind": "observation",
            "gate": args.gate,
            "status": "pass",
            "method": "recorded observation measurement",
            "artifact_id": args.artifact_id,
            "source_paths": dedup_strings(&sources),
            "observed_at": host.utc_now(),
            "measured_value": args.measured_value,
        });
        run.get_mut("evidence")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| ReceiptCommandError::Refused("evidence must be a list".into()))?
            .push(row);
        Ok(serde_json::json!({"evidence_id": evidence_id, "status": "pass"}))
    })
}

pub fn command_review_add(
    args: &ReviewArgs,
    host: &impl ReceiptCommandHost,
) -> Result<Value, ReceiptCommandError> {
    let review_id = host.require_identifier(&args.review_id, "review id")?;
    if args.adapter.is_empty() {
        return Err(ReceiptCommandError::Refused(
            "review requires non-empty --adapter".into(),
        ));
    }
    if args.provider_family.is_empty() {
        return Err(ReceiptCommandError::Refused(
            "review requires non-empty --provider-family".into(),
        ));
    }
    if args.status == "pass" {
        let missing = [
            ("artifact", args.artifact.as_deref()),
            ("route_receipt", args.route_receipt.as_deref()),
            ("reviewer_id", args.reviewer_id.as_deref()),
            ("model", args.model.as_deref()),
        ]
        .into_iter()
        .filter_map(|(name, value)| {
            value
                .filter(|value| !value.is_empty())
                .is_none()
                .then_some(name)
        })
        .collect::<Vec<_>>();
        if !missing.is_empty() {
            let flags = missing
                .iter()
                .map(|option| format!("--{}", option.replace('_', "-")))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(ReceiptCommandError::Refused(format!(
                "passing review requires {flags}"
            )));
        }
        host.require_identifier(
            args.reviewer_id.as_deref().unwrap_or_default(),
            "reviewer-id",
        )?;
    } else if args.reason.is_empty() {
        return Err(ReceiptCommandError::Refused(format!(
            "{} review requires --reason",
            args.status
        )));
    }
    let run_dir = args.run_dir.clone();
    host.mutate_run(&run_dir, |run, _run_dir, workspace| {
        if ["reviews", "evidence"].iter().any(|field| {
            run.get(*field)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .any(|item| item.is_object() && item.get("id").and_then(Value::as_str) == Some(review_id.as_str()))
        }) {
            return Err(ReceiptCommandError::Refused(format!("review id already exists: {review_id}")));
        }
        if args.lenses.is_empty() || args.lenses.iter().any(|lens| provenant_core::py::python_strip(lens).is_empty()) {
            return Err(ReceiptCommandError::Refused("review requires at least one non-empty lens".into()));
        }
        let lenses = dedup_strings(&args.lenses);
        if args.status != "pass" {
            let row = serde_json::json!({
                "id": review_id,
                "role": args.role,
                "provider_family": args.provider_family,
                "adapter": args.adapter,
                "model": args.model.as_deref().filter(|value| !value.is_empty()).unwrap_or(""),
                "reviewer_id": args.reviewer_id.as_deref().filter(|value| !value.is_empty()).unwrap_or(""),
                "independent_of_authorship": true,
                "lenses": lenses,
                "status": args.status,
                "evidence_id": "",
                "reason": args.reason,
                "route_receipt_digest": ""
            });
            run.get_mut("reviews").and_then(Value::as_array_mut)
                .ok_or_else(|| ReceiptCommandError::Refused("reviews must be a list".into()))?
                .push(row);
            return Ok(serde_json::json!({"review_id": review_id, "evidence_id": ""}));
        }
        let review_artifact_id = format!("{review_id}.artifact");
        let route_artifact_id = format!("{review_id}.route");
        let (review_artifact, review_path, review_digest, _review_raw) = add_review_artifact(
            run, workspace, &review_artifact_id, args.artifact.as_deref().unwrap_or_default(), host,
        )?;
        let (route_artifact, route_path, route_digest, route_raw) = add_review_artifact(
            run, workspace, &route_artifact_id, args.route_receipt.as_deref().unwrap_or_default(), host,
        )?;
        let route: Value = serde_json::from_slice(&route_raw).map_err(|error| {
            ReceiptCommandError::Refused(format!("route receipt must be readable JSON: {error}"))
        })?;
        let adapter = args.adapter.as_str();
        let reviewer_id = args.reviewer_id.as_deref().unwrap_or_default();
        let model = args.model.as_deref().unwrap_or_default();
        let family = route.get("provider_family").and_then(Value::as_str);
        let certification = route.get("certification_eligible");
        let chair_family = run.get("chair_family").and_then(Value::as_str);
        if route.get("status").and_then(Value::as_str) != Some("ok")
            || route.get("adapter").and_then(Value::as_str) != Some(adapter)
            || route.get("reviewer_id").and_then(Value::as_str) != Some(reviewer_id)
            || route.get("resolved_model").or_else(|| route.get("model")).and_then(Value::as_str) != Some(model)
            || route.get("model_family").and_then(Value::as_str) != Some(args.provider_family.as_str())
            || family.is_none_or(|family| provenant_core::py::python_strip(family).is_empty())
            || !(certification == Some(&Value::Bool(true))
                || (certification == Some(&Value::Bool(false))
                    && chair_family.is_some_and(|chair| !chair.is_empty())
                    && family == chair_family))
        {
            return Err(ReceiptCommandError::Refused("route receipt identity does not match review lineage".into()));
        }
        let family = provenant_core::py::python_strip(family.unwrap_or_default());
        if Some(family) != chair_family && route.get("cross_family") != Some(&Value::Bool(true)) {
            return Err(ReceiptCommandError::Refused(
                "cross-family review requires a cross-family route receipt".into(),
            ));
        }
        let profile = run.get("profile").and_then(Value::as_str).unwrap_or_default();
        let judgement_gate = host.profile_judgement_gate(profile)?;
        let evidence = serde_json::json!({
            "id": review_id,
            "kind": "judgement",
            "gate": judgement_gate,
            "status": "pass",
            "method": format!("independent review artifact by {reviewer_id}"),
            "artifact_id": review_artifact.get("id").cloned().unwrap_or(Value::Null),
            "source_paths": [review_path, route_path],
            "model_lineage": {"adapter": args.adapter, "provider_family": args.provider_family, "model": args.model},
            "reviewer_id": reviewer_id,
            "review_artifact_digest": review_digest,
            "route_receipt": {"path": route_path, "digest": route_digest},
            "recorded_at": host.utc_now()
        });
        run.get_mut("evidence").and_then(Value::as_array_mut)
            .ok_or_else(|| ReceiptCommandError::Refused("evidence must be a list".into()))?
            .push(evidence);
        let review = serde_json::json!({
            "id": review_id,
            "role": args.role,
            "provider_family": args.provider_family,
            "adapter": args.adapter,
            "model": args.model,
            "reviewer_id": args.reviewer_id,
            "independent_of_authorship": true,
            "lenses": lenses,
            "status": "pass",
            "evidence_id": review_id,
            "reason": "",
            "route_receipt_digest": route_digest
        });
        run.get_mut("reviews").and_then(Value::as_array_mut)
            .ok_or_else(|| ReceiptCommandError::Refused("reviews must be a list".into()))?
            .push(review);
        let _ = route_artifact;
        Ok(serde_json::json!({"review_id": review_id, "evidence_id": review_id}))
    })
}

fn find_artifact<'a>(run: &'a Value, artifact_id: &str) -> Result<&'a Value, ReceiptCommandError> {
    run.get("artifacts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|item| item.get("id").and_then(Value::as_str) == Some(artifact_id))
        .ok_or_else(|| ReceiptCommandError::Refused(format!("unknown artifact id: {artifact_id}")))
}

fn nonnegative_integer(value: Option<&Value>) -> Option<u64> {
    let value = value?;
    if value.is_boolean() {
        return None;
    }
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|value| u64::try_from(value).ok()))
}

fn dedup_strings(values: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    values
        .iter()
        .filter(|value| seen.insert((*value).clone()))
        .cloned()
        .collect()
}

fn python_float_g(value: f64) -> String {
    if !value.is_finite() {
        return value.to_string();
    }
    let sign = if value.is_sign_negative() { "-" } else { "" };
    if value == 0.0 {
        return format!("{sign}0");
    }
    let scientific = format!("{:.5e}", value.abs());
    let Some((mantissa, exponent)) = scientific.split_once('e') else {
        return value.to_string();
    };
    let Ok(exponent) = exponent.parse::<i32>() else {
        return value.to_string();
    };
    let digits = mantissa.replace('.', "");
    let digits = digits.trim_end_matches('0');
    if !(-4..6).contains(&exponent) {
        let significand = if digits.len() <= 1 {
            digits.to_owned()
        } else {
            format!("{}.{}", &digits[..1], &digits[1..])
        };
        let exponent_sign = if exponent >= 0 { '+' } else { '-' };
        return format!(
            "{sign}{significand}e{exponent_sign}{:02}",
            exponent.unsigned_abs()
        );
    }
    let point = exponent + 1;
    if point <= 0 {
        format!("{sign}0.{}{}", "0".repeat((-point) as usize), digits)
    } else if point as usize >= digits.len() {
        format!(
            "{sign}{digits}{}",
            "0".repeat(point as usize - digits.len())
        )
    } else {
        let (whole, fraction) = digits.split_at(point as usize);
        format!("{sign}{whole}.{fraction}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measurement_comparison_uses_python_integer_float_equality() {
        assert!(artifact_corroborates_measurement(
            &serde_json::json!({"gate":"latency","measured_value":1}),
            "latency",
            1.0,
        ));
        assert!(!artifact_corroborates_measurement(
            &serde_json::json!({"gate":"latency","measured_value":true}),
            "latency",
            1.0,
        ));
        assert!(!artifact_corroborates_measurement(
            &serde_json::json!({"gate":"latency","measured_value":1}),
            "latency",
            f64::NAN,
        ));
    }

    #[test]
    fn deduplication_keeps_first_occurrence() {
        assert_eq!(
            dedup_strings(&["a".into(), "b".into(), "a".into()]),
            ["a", "b"]
        );
    }

    #[test]
    fn measured_value_diagnostic_uses_python_general_format() {
        assert_eq!(python_float_g(-0.0), "-0");
        assert_eq!(python_float_g(0.0001), "0.0001");
        assert_eq!(python_float_g(0.00001), "1e-05");
        assert_eq!(python_float_g(123.456789), "123.457");
        assert_eq!(python_float_g(1_000_000.0), "1e+06");
    }

    #[test]
    fn profile_and_measurement_helpers_match_python() {
        let cases = serde_json::json!([
            {"profile":"software","gate_name":"latency","measured_value":1.0,"artifact":{"gate":"latency","measured_value":1}},
            {"profile":"research","gate_name":"coverage","measured_value":0.5,"artifact":{"gate":"coverage","measured_value":0.5}},
            {"profile":"analysis","gate_name":"quality","measured_value":1.0,"artifact":{"gate":"quality","measured_value":true}}
        ]);
        let expected =
            crate::test_support::oracle_script("oracle_receipts.py", "command_helpers", &cases)
                .expect("required by PROVENANT_PORT_DIFF_REQUIRED");
        let registry: Value =
            serde_json::from_str(include_str!("../../../../../config/delivery-profiles.json"))
                .unwrap();
        let actual = cases.as_array().unwrap().iter().map(|case| {
            serde_json::json!({
                "gate": profile_judgement_gate(case["profile"].as_str().unwrap_or_default(), &registry).unwrap(),
                "corroborates": artifact_corroborates_measurement(
                    &case["artifact"],
                    case["gate_name"].as_str().unwrap_or_default(),
                    case["measured_value"].as_f64().unwrap_or_default(),
                )
            })
        }).collect::<Vec<_>>();
        assert_eq!(serde_json::json!(actual), expected);
    }

    #[test]
    fn profile_registry_errors_match_python_keyerror_and_gate_distinction() {
        let cases = serde_json::json!([
            {"profile":"software","registry":{"profiles":{}}},
            {"profile":"software","registry":{"profiles":{"software":{"required_evidence":{}}}}},
            {"profile":"software","registry":{"profiles":{"software":{"required_evidence":{"judgement":[]}}}}},
            {"profile":"software","registry":{"profiles":[]}}
        ]);
        let expected = crate::test_support::oracle_script(
            "oracle_receipts.py",
            "command_profile_errors",
            &cases,
        )
        .expect("required by PROVENANT_PORT_DIFF_REQUIRED");
        let actual = cases
            .as_array()
            .unwrap()
            .iter()
            .map(|case| {
                match profile_judgement_gate(
                    case["profile"].as_str().unwrap_or_default(),
                    &case["registry"],
                ) {
                    Ok(gate) => serde_json::json!({"gate":gate}),
                    Err(error) => serde_json::json!({"error":error.to_string()}),
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(serde_json::json!(actual), expected);
    }
}
