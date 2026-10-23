//! Synthetic reference evaluation builder and artifact binding policy.
//!
//! The source reads a versioned evaluation template and writes its artifacts
//! beneath caller-selected workspaces. Ordered JSON is retained through the
//! shared `ReceiptValue` record type. Writes are delegated to a confinement
//! host because the supervisor safe filesystem API is not yet exposed here.

use provenant_core::py::python_fromisoformat;
use provenant_records::delivery_run_shape::ReceiptValue;
use serde_json::Value;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum ReferenceEvaluationError {
    #[error("{0}")]
    Invalid(String),
    #[error("Python integer is outside the i64/u64 range")]
    NumberOutOfRange,
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("reference artifact host failed: {0}")]
    Host(String),
}

pub trait ReferenceArtifactHost {
    /// Writes bytes relative to the supplied artifact root after validating
    /// containment and artifact scope with the confinement owner.
    fn write_artifact(
        &self,
        artifact_root: &Path,
        relative_path: &str,
        payload: &[u8],
    ) -> Result<(), ReferenceEvaluationError>;
}

pub fn artifact(artifact_id: &str, suffix: &str) -> ReceiptValue {
    object(vec![
        ("id", text(artifact_id)),
        ("path", text(format!("evidence/{artifact_id}.{suffix}"))),
        (
            "media_type",
            text(if suffix == "json" {
                "application/json"
            } else {
                "text/plain"
            }),
        ),
        ("digest", text(format!("sha256:{}", "f".repeat(64)))),
        ("owner", text("evaluation-chair")),
        ("retention", text("project-policy")),
        ("data_policy", text("synthetic")),
    ])
}

/// Build one synthetic reference evaluation from the exact source template.
/// Python's range semantics are preserved for zero and negative counts.
pub fn make_reference_evaluation(
    template_json: &str,
    delivery_run_id: &str,
    repetitions: i64,
    sample_size: i64,
    evaluation_id: &str,
    status: &str,
    time_offset_minutes: i64,
) -> Result<ReceiptValue, ReferenceEvaluationError> {
    let mut value = ReceiptValue::from_json_str(template_json).map_err(|error| match error {
        provenant_records::checked_json::DecodeError::NumberOutOfRange(_) => {
            ReferenceEvaluationError::NumberOutOfRange
        }
        provenant_records::checked_json::DecodeError::Json(error) => {
            ReferenceEvaluationError::Json(error)
        }
    })?;
    set(&mut value, "evaluation_id", text(evaluation_id))?;
    set(
        get_mut(&mut value, "decision")?,
        "enclosing_delivery_run_id",
        text(delivery_run_id),
    )?;
    set(&mut value, "created_at", text("2026-07-10T00:02:00Z"))?;
    set(
        get_mut(&mut value, "plan")?,
        "frozen_at",
        text("2026-07-10T00:02:00Z"),
    )?;
    set(&mut value, "status", text("pass"))?;
    set(&mut value, "updated_at", text("2026-07-10T00:05:00Z"))?;

    let extra_artifacts = [
        "preflight",
        "route",
        "input",
        "output",
        "judgement",
        "aggregate",
    ]
    .into_iter()
    .map(|id| artifact(id, "json"))
    .collect::<Vec<_>>();
    array_mut(get_mut(&mut value, "artifacts")?)?.extend(extra_artifacts);

    let case_ids = if sample_size > 0 {
        (1..=sample_size)
            .map(|index| format!("case-{index:03}"))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let cases = case_ids
        .iter()
        .enumerate()
        .map(|(index, case_id)| {
            object(vec![
                ("id", text(case_id)),
                ("category", text("quality")),
                ("critical", ReceiptValue::Bool(index == 0)),
            ])
        })
        .collect::<Vec<_>>();
    let seeds = if repetitions > 0 {
        (1..=repetitions)
            .map(|index| {
                index
                    .checked_mul(101)
                    .ok_or(ReferenceEvaluationError::NumberOutOfRange)
                    .map(integer)
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };
    let shard = object(vec![
        ("id", text("all")),
        ("case_ids", strings_owned(&case_ids)),
    ]);
    let schedule = get_mut(get_mut(&mut value, "plan")?, "schedule")?;
    set(schedule, "cases", ReceiptValue::Array(cases))?;
    set(schedule, "repetitions", integer(repetitions))?;
    set(schedule, "seeds", ReceiptValue::Array(seeds))?;
    set(schedule, "shards", ReceiptValue::Array(vec![shard]))?;

    let preflight = object(vec![
        ("id", text("fixture-schema")),
        ("status", text("pass")),
        ("started_at", text("2026-07-10T00:03:00Z")),
        ("completed_at", text("2026-07-10T00:03:05Z")),
        ("evidence_artifact_id", text("preflight")),
        ("exit_code", integer(0)),
        ("reason", text("")),
    ]);
    set(
        &mut value,
        "preflight",
        ReceiptValue::Array(vec![preflight]),
    )?;

    let mut attempts = Vec::new();
    let mut case_results = Vec::new();
    let mut judgements = Vec::new();
    if repetitions > 0 {
        for (arm_id, score) in [("candidate", 0.9_f64), ("control", 0.85_f64)] {
            for repetition in 1..=repetitions {
                let attempt_id = format!("attempt-{arm_id}-{repetition}");
                let arm = find_in_array(get(get(&value, "plan")?, "arms")?, "id", arm_id)?;
                let manifest_id = string(get(arm, "manifest_artifact_id")?)?;
                let manifest = find_in_array(get(&value, "artifacts")?, "id", manifest_id)?;
                let seed = repetition
                    .checked_mul(101)
                    .ok_or(ReferenceEvaluationError::NumberOutOfRange)?;
                attempts.push(attempt_record(AttemptRecord {
                    attempt_id: &attempt_id,
                    arm_id,
                    repetition,
                    seed,
                    plan_digest: string(get(get(&value, "plan")?, "digest")?)?,
                    shared_runtime_digest: string(get(
                        get(&value, "plan")?,
                        "shared_runtime_digest",
                    )?)?,
                    arm_manifest_digest: string(get(manifest, "digest")?)?,
                    arm_configuration_digest: string(get(arm, "configuration_digest")?)?,
                }));
                for case_id in &case_ids {
                    case_results.push(object(vec![
                        ("attempt_id", text(&attempt_id)),
                        ("case_id", text(case_id)),
                        ("status", text("pass")),
                        ("scores", object(vec![("quality", float(score)?)])),
                        ("evidence_artifact_id", text("output")),
                        ("evidence_unavailable_reason", text("")),
                        ("reason", text("")),
                    ]));
                    judgements.push(object(vec![
                        (
                            "id",
                            text(format!("judgement-{arm_id}-{repetition}-{case_id}")),
                        ),
                        ("grader_id", text("ground-truth")),
                        ("attempt_id", text(&attempt_id)),
                        ("case_id", text(case_id)),
                        ("outcome", text("pass")),
                        ("scores", object(vec![("quality", float(score)?)])),
                        ("evidence_artifact_id", text("judgement")),
                    ]));
                }
            }
        }
    }
    set(&mut value, "attempts", ReceiptValue::Array(attempts))?;
    set(
        &mut value,
        "case_results",
        ReceiptValue::Array(case_results),
    )?;
    set(
        &mut value,
        "graders",
        ReceiptValue::Array(vec![grader_record()]),
    )?;
    set(&mut value, "judgements", ReceiptValue::Array(judgements))?;
    set(&mut value, "adjudications", ReceiptValue::Array(Vec::new()))?;

    let planned_cases = checked_product(&[2, repetitions, sample_size])?;
    let planned_attempts = checked_product(&[2, repetitions])?;
    let candidate_rows = checked_product(&[repetitions, sample_size])?;
    let results = object(vec![
        (
            "accounting",
            object(vec![
                ("planned", integer(planned_cases)),
                ("passed", integer(planned_cases)),
                ("failed", integer(0)),
                ("omitted", integer(0)),
                ("skipped", integer(0)),
                ("excluded", integer(0)),
                ("timed_out", integer(0)),
                ("invalid", integer(0)),
                ("tool_errors", integer(0)),
            ]),
        ),
        (
            "attempt_accounting",
            object(vec![
                ("planned", integer(planned_attempts)),
                ("base_planned", integer(planned_attempts)),
                ("retries", integer(0)),
                ("succeeded", integer(planned_attempts)),
                ("timed_out", integer(0)),
                ("invalid_output", integer(0)),
                ("tool_errors", integer(0)),
                ("skipped", integer(0)),
                ("excluded", integer(0)),
            ]),
        ),
        (
            "metrics",
            ReceiptValue::Array(vec![object(vec![
                ("name", text("quality")),
                ("target_arm_id", text("candidate")),
                ("numerator", float(0.9 * candidate_rows as f64)?),
                ("denominator", integer(candidate_rows)),
                ("value", float(0.9)?),
                (
                    "comparisons",
                    ReceiptValue::Array(vec![object(vec![
                        ("arm_id", text("control")),
                        ("numerator", float(0.85 * candidate_rows as f64)?),
                        ("denominator", integer(candidate_rows)),
                        ("value", float(0.85)?),
                        ("delta", float(0.05)?),
                        ("passed", ReceiptValue::Bool(true)),
                    ])]),
                ),
                ("passed", ReceiptValue::Bool(true)),
                ("evidence_artifact_id", text("aggregate")),
            ])]),
        ),
        ("failure_artifact_ids", ReceiptValue::Array(Vec::new())),
    ]);
    set(&mut value, "results", results)?;
    set(&mut value, "conclusion", default_conclusion())?;

    if status == "fail" || status == "incomplete" {
        set(&mut value, "status", text(status))?;
        let preflight = array_mut(get_mut(&mut value, "preflight")?)?
            .first_mut()
            .ok_or_else(|| {
                ReferenceEvaluationError::Invalid("preflight template row missing".into())
            })?;
        set(preflight, "status", text("fail"))?;
        set(preflight, "exit_code", integer(1))?;
        for attempt in array_mut(get_mut(&mut value, "attempts")?)? {
            set(attempt, "status", text("skipped"))?;
            set(attempt, "output_artifact_id", text(""))?;
            set(attempt, "reason", text("deterministic preflight failed"))?;
            set(get_mut(attempt, "lineage")?, "actual_model", text(""))?;
            set(get_mut(attempt, "lineage")?, "effective_effort", text(""))?;
        }
        for result in array_mut(get_mut(&mut value, "case_results")?)? {
            set(result, "status", text("skipped"))?;
            set(result, "scores", object(Vec::new()))?;
            set(result, "reason", text("deterministic preflight failed"))?;
            set(result, "evidence_artifact_id", text(""))?;
            set(
                result,
                "evidence_unavailable_reason",
                text("generator did not run"),
            )?;
        }
        set(&mut value, "graders", ReceiptValue::Array(Vec::new()))?;
        set(&mut value, "judgements", ReceiptValue::Array(Vec::new()))?;
        let results = get_mut(&mut value, "results")?;
        let accounting = get_mut(results, "accounting")?;
        set(accounting, "passed", integer(0))?;
        set(accounting, "skipped", integer(planned_cases))?;
        let attempts_accounting = get_mut(results, "attempt_accounting")?;
        set(attempts_accounting, "succeeded", integer(0))?;
        set(attempts_accounting, "skipped", integer(planned_attempts))?;
        let metric = array_mut(get_mut(results, "metrics")?)?
            .first_mut()
            .ok_or_else(|| ReferenceEvaluationError::Invalid("quality metric missing".into()))?;
        set(metric, "numerator", float(0.0)?)?;
        set(metric, "value", float(0.0)?)?;
        set(metric, "passed", ReceiptValue::Bool(false))?;
        let comparison = array_mut(get_mut(metric, "comparisons")?)?
            .first_mut()
            .ok_or_else(|| {
                ReferenceEvaluationError::Invalid("quality comparison missing".into())
            })?;
        set(comparison, "numerator", float(0.0)?)?;
        set(comparison, "value", float(0.0)?)?;
        set(comparison, "delta", float(0.0)?)?;
        set(comparison, "passed", ReceiptValue::Bool(true))?;
        let conclusion = get_mut(&mut value, "conclusion")?;
        set(conclusion, "status", text(status))?;
        set(conclusion, "passed_gates", ReceiptValue::Array(Vec::new()))?;
        set(conclusion, "failed_gates", strings(&["fixture-schema"]))?;
        set(conclusion, "evidence_artifact_ids", strings(&["preflight"]))?;
    }
    if status != "pass" && status != "fail" && status != "incomplete" {
        return Err(ReferenceEvaluationError::Invalid(
            "reference evaluation status must be pass, fail or incomplete".into(),
        ));
    }
    if time_offset_minutes != 0 {
        shift_timestamps(&mut value, time_offset_minutes)?;
    }
    Ok(value)
}

struct AttemptRecord<'a> {
    attempt_id: &'a str,
    arm_id: &'a str,
    repetition: i64,
    seed: i64,
    plan_digest: &'a str,
    shared_runtime_digest: &'a str,
    arm_manifest_digest: &'a str,
    arm_configuration_digest: &'a str,
}

fn attempt_record(record: AttemptRecord<'_>) -> ReceiptValue {
    object(vec![
        ("id", text(record.attempt_id)),
        ("arm_id", text(record.arm_id)),
        ("family", text("synthetic")),
        ("repetition", integer(record.repetition)),
        ("seed", integer(record.seed)),
        ("shard_id", text("all")),
        ("status", text("success")),
        (
            "started_at",
            text(format!("2026-07-10T00:03:{:02}Z", record.repetition * 10)),
        ),
        (
            "completed_at",
            text(format!(
                "2026-07-10T00:03:{:02}Z",
                record.repetition * 10 + 5
            )),
        ),
        ("retry_of", text("")),
        ("reason", text("")),
        ("plan_digest", text(record.plan_digest)),
        ("shared_runtime_digest", text(record.shared_runtime_digest)),
        ("arm_manifest_digest", text(record.arm_manifest_digest)),
        (
            "arm_configuration_digest",
            text(record.arm_configuration_digest),
        ),
        ("route_receipt_artifact_id", text("route")),
        ("input_artifact_id", text("input")),
        ("output_artifact_id", text("output")),
        (
            "lineage",
            object(vec![
                ("adapter", text("synthetic-runner")),
                ("adapter_version", text("1")),
                ("endpoint_provider", text("local")),
                ("provider_family", text("synthetic")),
                ("requested_model", text("model-v1")),
                ("actual_model", text("model-v1")),
                ("requested_effort", text("standard")),
                ("effective_effort", text("standard")),
                ("capability_source", text("frozen route receipt")),
                ("session_id", text(record.attempt_id)),
                ("substitution_reason", text("")),
            ]),
        ),
        (
            "usage",
            object(vec![("unavailable_reason", text("synthetic runner"))]),
        ),
    ])
}

fn grader_record() -> ReceiptValue {
    object(vec![
        ("id", text("ground-truth")),
        ("type", text("ground-truth")),
        ("rubric_artifact_id", text("rubric")),
        ("independent_of_generators", ReceiptValue::Bool(true)),
        ("blinded", ReceiptValue::Bool(true)),
        ("conflict", text("none")),
        ("started_at", text("2026-07-10T00:04:00Z")),
        ("completed_at", text("2026-07-10T00:04:20Z")),
        ("input_artifact_id", text("output")),
        ("output_artifact_id", text("judgement")),
        (
            "usage",
            object(vec![(
                "unavailable_reason",
                text("deterministic ground-truth grader"),
            )]),
        ),
        (
            "lineage",
            object(vec![
                ("adapter", text("fixture-checker")),
                ("adapter_version", text("1")),
                ("endpoint_provider", text("local")),
                ("provider_family", text("ground-truth")),
                ("requested_model", text("rules-v1")),
                ("actual_model", text("rules-v1")),
                ("requested_effort", text("not-applicable")),
                ("effective_effort", text("not-applicable")),
                ("capability_source", text("pinned local checker")),
                ("session_id", text("grader-ground-truth")),
                ("substitution_reason", text("")),
                ("route_receipt_artifact_id", text("")),
            ]),
        ),
    ])
}

fn default_conclusion() -> ReceiptValue {
    object(vec![
        ("status", text("pass")),
        ("machine_only", ReceiptValue::Bool(true)),
        ("passed_gates", strings(&["quality"])),
        ("failed_gates", ReceiptValue::Array(Vec::new())),
        ("limitations", strings(&["synthetic reference fixture"])),
        ("evidence_artifact_ids", strings(&["aggregate"])),
    ])
}

/// Shift every Z-suffixed spelling accepted by CPython's datetime.fromisoformat.
fn shift_timestamps(
    value: &mut ReceiptValue,
    minutes: i64,
) -> Result<(), ReferenceEvaluationError> {
    match value {
        ReceiptValue::Object(fields) => {
            for (_, child) in fields {
                shift_timestamps(child, minutes)?;
            }
        }
        ReceiptValue::Array(items) => {
            for child in items {
                shift_timestamps(child, minutes)?;
            }
        }
        ReceiptValue::String(text) if text.ends_with('Z') => {
            if let Some(shifted) = shift_iso_seconds(text, minutes)? {
                *text = shifted;
            }
        }
        _ => {}
    }
    Ok(())
}

fn shift_iso_seconds(
    value: &str,
    minutes: i64,
) -> Result<Option<String>, ReferenceEvaluationError> {
    let Some(with_offset) = value.strip_suffix('Z') else {
        return Ok(None);
    };
    let parsed = python_fromisoformat(&format!("{with_offset}+00:00")).ok();
    let Some(parsed) = parsed else {
        return Ok(None);
    };
    let original = parsed
        .timestamp()
        .map_err(|_| ReferenceEvaluationError::Invalid("invalid ISO timestamp".into()))?
        .as_microsecond();
    let shifted = minutes
        .checked_mul(60_000_000)
        .and_then(|delta| original.checked_add(delta))
        .ok_or_else(|| {
            ReferenceEvaluationError::Invalid("timestamp shift is outside datetime range".into())
        })?;
    const MICROS_PER_DAY: i64 = 86_400_000_000;
    if !(-62_135_596_800_000_000..253_402_300_800_000_000).contains(&shifted) {
        return Err(ReferenceEvaluationError::Invalid(
            "timestamp shift is outside datetime range".into(),
        ));
    }
    let day_index = shifted.div_euclid(MICROS_PER_DAY);
    let within_day = shifted.rem_euclid(MICROS_PER_DAY);
    let (year, month, day) = civil_from_days(day_index);
    if !(1..=9999).contains(&year) {
        return Err(ReferenceEvaluationError::Invalid(
            "timestamp shift is outside datetime range".into(),
        ));
    }
    let seconds = within_day / 1_000_000;
    let micros = within_day % 1_000_000;
    let suffix = if micros == 0 {
        String::new()
    } else {
        format!(".{micros:06}")
    };
    Ok(Some(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}{suffix}Z",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60,
    )))
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

pub fn materialise_evaluation(
    value: &mut ReceiptValue,
    artifact_root: &Path,
    host: &impl ReferenceArtifactHost,
) -> Result<(), ReferenceEvaluationError> {
    let artifact_rows = array(get(value, "artifacts")?)?.to_vec();
    for item in &artifact_rows {
        let id = string(get(item, "id")?)?;
        let path = string(get(item, "path")?)?;
        let plan_id = string(get(get(value, "plan")?, "artifact_id")?)?;
        let payload = if id == plan_id {
            let mut frozen = get(value, "plan")?.clone();
            remove(&mut frozen, "artifact_id");
            remove(&mut frozen, "digest");
            let mut encoded = String::new();
            write_compact_sorted_python_json(&frozen, &mut encoded)?;
            let mut bytes = encoded.into_bytes();
            bytes.push(b'\n');
            bytes
        } else {
            format!("artifact:{id}\n").into_bytes()
        };
        host.write_artifact(artifact_root, path, &payload)?;
        set_by_key(
            get_mut(value, "artifacts")?,
            "id",
            id,
            "digest",
            text(provenant_records::digest_bytes(&payload)),
        )?;
    }
    let artifacts = array(get(value, "artifacts")?)?;
    let plan_id = string(get(get(value, "plan")?, "artifact_id")?)?;
    let plan_artifact = find_in_slice(artifacts, "id", plan_id)?;
    let plan_digest = string(get(plan_artifact, "digest")?)?.to_owned();
    set(get_mut(value, "plan")?, "digest", text(&plan_digest))?;
    let arms = array(get(get(value, "plan")?, "arms")?)?.to_vec();
    let arms_value = ReceiptValue::Array(arms);
    let artifact_rows = array(get(value, "artifacts")?)?.to_vec();
    for attempt in array_mut(get_mut(value, "attempts")?)? {
        let arm_id = string(get(attempt, "arm_id")?)?.to_owned();
        let arm = find_in_array(&arms_value, "id", &arm_id)?;
        let manifest_id = string(get(arm, "manifest_artifact_id")?)?;
        let manifest = find_in_slice(&artifact_rows, "id", manifest_id)?;
        let manifest_digest = string(get(manifest, "digest")?)?.to_owned();
        set(attempt, "plan_digest", text(&plan_digest))?;
        set(attempt, "arm_manifest_digest", text(&manifest_digest))?;
    }
    Ok(())
}

fn set_by_key(
    value: &mut ReceiptValue,
    search_key: &str,
    search_value: &str,
    key: &str,
    replacement: ReceiptValue,
) -> Result<(), ReferenceEvaluationError> {
    let rows = array_mut(value)?;
    let index = rows
        .iter()
        .rposition(|row| {
            get(row, search_key)
                .ok()
                .and_then(|value| string(value).ok())
                == Some(search_value)
        })
        .ok_or_else(|| {
            ReferenceEvaluationError::Invalid(format!("artifact {search_value} is missing"))
        })?;
    set(&mut rows[index], key, replacement)
}

#[derive(Debug, Clone, Copy)]
pub struct EvaluationBindingOptions<'a> {
    pub evaluation_template: &'a str,
    pub workspace_root: &'a Path,
    pub artifact_root: &'a Path,
    pub binding_index: usize,
    pub repetitions: i64,
    pub sample_size: i64,
    pub time_offset_minutes: i64,
}

pub fn materialise_evaluation_binding(
    run: &mut ReceiptValue,
    options: &EvaluationBindingOptions<'_>,
    host: &impl ReferenceArtifactHost,
) -> Result<ReceiptValue, ReferenceEvaluationError> {
    let binding = array(get(get(run, "assurance")?, "evaluations")?)?
        .get(options.binding_index)
        .ok_or_else(|| {
            ReferenceEvaluationError::Invalid("evaluation binding index out of range".into())
        })?;
    let status = match string(get(binding, "status")?)? {
        "complete" => "pass",
        "failed" => "fail",
        "incomplete" => "incomplete",
        _ => {
            return Err(ReferenceEvaluationError::Invalid(
                "only complete or terminal evaluation bindings have artifacts".into(),
            ));
        }
    };
    let evaluation_artifact_id = string(get(binding, "evaluation_artifact_id")?)?.to_owned();
    let evaluation_id = string(get(binding, "evaluation_id")?)?.to_owned();
    let run_id = string(get(run, "run_id")?)?.to_owned();
    let selected_artifact_index = artifact_index(run, &evaluation_artifact_id)?;
    let artifact = array(get(run, "artifacts")?)?
        .get(selected_artifact_index)
        .ok_or_else(|| {
            ReferenceEvaluationError::Invalid(format!(
                "unknown artifact id: {evaluation_artifact_id}"
            ))
        })?
        .clone();
    let artifact_path = string(get(&artifact, "path")?)?.to_owned();
    let mut evaluation = make_reference_evaluation(
        options.evaluation_template,
        &run_id,
        options.repetitions,
        options.sample_size,
        &evaluation_id,
        status,
        options.time_offset_minutes,
    )?;
    materialise_evaluation(&mut evaluation, options.artifact_root, host)?;
    let mut payload = pretty_json(&evaluation)?.into_bytes();
    payload.push(b'\n');
    let relative = options.workspace_root.join(&artifact_path);
    let relative = relative.strip_prefix(options.workspace_root).map_err(|_| {
        ReferenceEvaluationError::Invalid("evaluation artifact escapes workspace".into())
    })?;
    host.write_artifact(
        options.workspace_root,
        &relative.to_string_lossy(),
        &payload,
    )?;
    let digest = provenant_records::digest_bytes(&payload);
    set(
        array_mut(get_mut(run, "artifacts")?)?
            .get_mut(selected_artifact_index)
            .ok_or_else(|| {
                ReferenceEvaluationError::Invalid(format!(
                    "unknown artifact id: {evaluation_artifact_id}"
                ))
            })?,
        "digest",
        text(&digest),
    )?;
    let binding = array_mut(get_mut(get_mut(run, "assurance")?, "evaluations")?)?
        .get_mut(options.binding_index)
        .ok_or_else(|| {
            ReferenceEvaluationError::Invalid("evaluation binding index out of range".into())
        })?;
    set(binding, "evaluation_digest", text(&digest))?;
    let plan_digest = string(get(get(&evaluation, "plan")?, "digest")?)?.to_owned();
    set(binding, "plan_digest", text(&plan_digest))?;
    let evidence_id = string(get(binding, "evidence_id")?)?.to_owned();
    if let Some(linked) = array_mut(get_mut(run, "evidence")?)?
        .iter_mut()
        .find(|item| {
            get(item, "id").ok().and_then(|value| string(value).ok()) == Some(evidence_id.as_str())
        })
    {
        if string(get(linked, "kind")?)? == "deterministic" {
            set(get_mut(linked, "result")?, "receipt_digest", text(&digest))?;
        }
    }
    if let Some(bundle_index) = array(get(run, "artifacts")?)?.iter().rposition(|item| {
        get(item, "id").ok().and_then(|value| string(value).ok()) == Some("evidence-bundle")
    }) {
        let mut bundle = array(get(run, "artifacts")?)?
            .get(bundle_index)
            .ok_or_else(|| {
                ReferenceEvaluationError::Invalid("evidence-bundle artifact is missing".into())
            })?
            .clone();
        materialise_deterministic_evidence_bundle(run, &mut bundle, options.workspace_root, host)?;
        let bundle_digest = string(get(&bundle, "digest")?)?.to_owned();
        set(
            array_mut(get_mut(run, "artifacts")?)?
                .get_mut(bundle_index)
                .ok_or_else(|| {
                    ReferenceEvaluationError::Invalid("evidence-bundle artifact is missing".into())
                })?,
            "digest",
            text(&bundle_digest),
        )?;
    }
    Ok(evaluation)
}

pub fn materialise_deterministic_evidence_bundle(
    run: &mut ReceiptValue,
    artifact: &mut ReceiptValue,
    workspace_root: &Path,
    host: &impl ReferenceArtifactHost,
) -> Result<(), ReferenceEvaluationError> {
    let artifact_id = string(get(artifact, "id")?)?.to_owned();
    let mut checks = Vec::new();
    for item in array(get(run, "evidence")?)? {
        if string(get(item, "kind").unwrap_or(&ReceiptValue::Null))? != "deterministic"
            || string(get(item, "artifact_id").unwrap_or(&ReceiptValue::Null))? != artifact_id
        {
            continue;
        }
        checks.push(object(vec![
            ("id", get(item, "id")?.clone()),
            ("gate", get(item, "gate")?.clone()),
            ("status", get(item, "status")?.clone()),
            ("method", get(item, "method")?.clone()),
            ("source_paths", get(item, "source_paths")?.clone()),
            ("exit_code", get(get(item, "result")?, "exit_code")?.clone()),
        ]));
    }
    let bundle = object(vec![
        ("schema_version", integer(1)),
        ("contract", text("deterministic-evidence-bundle")),
        ("checks", ReceiptValue::Array(checks)),
    ]);
    let bytes = crate::pending::python_json(&to_serde_value(&bundle)?).into_bytes();
    let mut bytes = bytes;
    bytes.push(b'\n');
    let path = string(get(artifact, "path")?)?.to_owned();
    host.write_artifact(workspace_root, &path, &bytes)?;
    let digest = provenant_records::digest_bytes(&bytes);
    set(artifact, "digest", text(&digest))?;
    for item in array_mut(get_mut(run, "evidence")?)? {
        if string(get(item, "kind").unwrap_or(&ReceiptValue::Null))? == "deterministic"
            && string(get(item, "artifact_id").unwrap_or(&ReceiptValue::Null))? == artifact_id
        {
            set(get_mut(item, "result")?, "receipt_digest", text(&digest))?;
        }
    }
    let approval_id = string(get(get(run, "authority")?, "evidence")?)?.to_owned();
    if let Some(approval) = array_mut(get_mut(run, "evidence")?)?
        .iter_mut()
        .find(|item| {
            get(item, "id").ok().and_then(|value| string(value).ok()) == Some(approval_id.as_str())
        })
    {
        if string(get(approval, "artifact_id")?)? == artifact_id {
            set(get_mut(run, "authority")?, "evidence_digest", text(&digest))?;
        }
    }
    Ok(())
}

/// Materialise the complete synthetic delivery fixture. Every file write is
/// delegated to the confinement-owned artifact host.
pub fn materialise_reference_run(
    run: &mut ReceiptValue,
    evaluation_template: &str,
    workspace_root: &Path,
    evaluation_repetitions: i64,
    evaluation_sample_size: i64,
    host: &impl ReferenceArtifactHost,
) -> Result<(), ReferenceEvaluationError> {
    let design_artifact_id = string(get(get(run, "design")?, "artifact_id")?)?.to_owned();
    let design_index = artifact_index(run, &design_artifact_id)?;
    let design_artifact = array(get(run, "artifacts")?)?
        .get(design_index)
        .ok_or_else(|| ReferenceEvaluationError::Invalid("design artifact is missing".into()))?
        .clone();
    let design_path = string(get(&design_artifact, "path")?)?.to_owned();
    let design_digest = write_delivery_artifact(
        &design_artifact,
        workspace_root,
        b"approved synthetic reference intent\n",
        host,
    )?;
    set(
        array_mut(get_mut(run, "artifacts")?)?
            .get_mut(design_index)
            .ok_or_else(|| {
                ReferenceEvaluationError::Invalid("design artifact is missing".into())
            })?,
        "digest",
        text(&design_digest),
    )?;
    set(get_mut(run, "intent")?, "digest", text(&design_digest))?;
    set(get_mut(run, "design")?, "digest", text(&design_digest))?;

    let bindings = array(get(get(run, "assurance")?, "evaluations")?)?.to_vec();
    for (index, binding) in bindings.iter().enumerate() {
        if string(get(binding, "status")?)? == "planned" {
            continue;
        }
        let artifact_id = string(get(binding, "evaluation_artifact_id")?)?.to_owned();
        let artifact_path = string(get(
            find_in_slice(array(get(run, "artifacts")?)?, "id", &artifact_id)?,
            "path",
        )?)?;
        let artifact_root = artifact_root(workspace_root, artifact_path);
        let options = EvaluationBindingOptions {
            evaluation_template,
            workspace_root,
            artifact_root: &artifact_root,
            binding_index: index,
            repetitions: evaluation_repetitions,
            sample_size: evaluation_sample_size,
            time_offset_minutes: 0,
        };
        materialise_evaluation_binding(run, &options, host)?;
    }

    let bundle_index = artifact_index(run, "evidence-bundle")?;
    let mut bundle = array(get(run, "artifacts")?)?
        .get(bundle_index)
        .ok_or_else(|| {
            ReferenceEvaluationError::Invalid("evidence-bundle artifact is missing".into())
        })?
        .clone();
    materialise_deterministic_evidence_bundle(run, &mut bundle, workspace_root, host)?;
    let bundle_digest = string(get(&bundle, "digest")?)?.to_owned();
    set(
        array_mut(get_mut(run, "artifacts")?)?
            .get_mut(bundle_index)
            .ok_or_else(|| {
                ReferenceEvaluationError::Invalid("evidence-bundle artifact is missing".into())
            })?,
        "digest",
        text(&bundle_digest),
    )?;

    for (family, reviewer_id, adapter) in [
        ("openai", "reference-openai-reviewer", "native-subagent"),
        ("anthropic", "reference-anthropic-reviewer", "claude-code"),
    ] {
        let id = format!("review-route-{family}");
        let index = artifact_index(run, &id)?;
        let artifact = array(get(run, "artifacts")?)?
            .get(index)
            .ok_or_else(|| ReferenceEvaluationError::Invalid(format!("artifact {id} is missing")))?
            .clone();
        let path = string(get(&artifact, "path")?)?.to_owned();
        let receipt = serde_json::json!({
            "status": "ok",
            "adapter": adapter,
            "reviewer_id": reviewer_id,
            "resolved_model": "runtime-resolved",
            "model_family": family,
            "provider_family": family,
            "cross_family": family == "anthropic",
            "certification_eligible": true
        });
        let mut payload = crate::pending::python_json(&receipt).into_bytes();
        payload.push(b'\n');
        let digest = write_delivery_artifact(&artifact, workspace_root, &payload, host)?;
        set(
            array_mut(get_mut(run, "artifacts")?)?
                .get_mut(index)
                .ok_or_else(|| {
                    ReferenceEvaluationError::Invalid(format!("artifact {id} is missing"))
                })?,
            "digest",
            text(&digest),
        )?;
        for evidence in array_mut(get_mut(run, "evidence")?)? {
            if get(evidence, "route_receipt")
                .ok()
                .and_then(|route| get(route, "path").ok())
                .and_then(|value| string(value).ok())
                == Some(path.as_str())
            {
                set(get_mut(evidence, "route_receipt")?, "digest", text(&digest))?;
            }
        }
        for review in array_mut(get_mut(run, "reviews")?)? {
            if get(review, "provider_family")
                .ok()
                .and_then(|value| string(value).ok())
                == Some(family)
                && get(review, "status")
                    .ok()
                    .and_then(|value| string(value).ok())
                    == Some("pass")
            {
                set(review, "route_receipt_digest", text(&digest))?;
            }
        }
    }
    let _ = design_path;
    Ok(())
}

fn artifact_index(run: &ReceiptValue, id: &str) -> Result<usize, ReferenceEvaluationError> {
    array(get(run, "artifacts")?)?
        .iter()
        .rposition(|item| get(item, "id").ok().and_then(|value| string(value).ok()) == Some(id))
        .ok_or_else(|| ReferenceEvaluationError::Invalid(format!("unknown artifact id: {id}")))
}

fn artifact_root(workspace_root: &Path, artifact_path: &str) -> std::path::PathBuf {
    workspace_root.join(
        Path::new(artifact_path)
            .parent()
            .unwrap_or_else(|| Path::new("")),
    )
}

fn write_delivery_artifact(
    artifact: &ReceiptValue,
    workspace_root: &Path,
    payload: &[u8],
    host: &impl ReferenceArtifactHost,
) -> Result<String, ReferenceEvaluationError> {
    let path = string(get(artifact, "path")?)?;
    host.write_artifact(workspace_root, path, payload)?;
    Ok(provenant_records::digest_bytes(payload))
}

fn pretty_json(value: &ReceiptValue) -> Result<String, ReferenceEvaluationError> {
    let mut output = String::new();
    write_pretty_json(value, 0, &mut output)?;
    Ok(output)
}

fn write_pretty_json(
    value: &ReceiptValue,
    depth: usize,
    output: &mut String,
) -> Result<(), ReferenceEvaluationError> {
    match value {
        ReceiptValue::Null => output.push_str("null"),
        ReceiptValue::Bool(false) => output.push_str("false"),
        ReceiptValue::Bool(true) => output.push_str("true"),
        ReceiptValue::Number(number) if number.is_f64() => output.push_str(
            &provenant_core::py::python_str(&Value::Number(number.clone())),
        ),
        ReceiptValue::Number(number) => output.push_str(&number.to_string()),
        ReceiptValue::String(value) => output.push_str(&python_json_string(value)?),
        ReceiptValue::Array(items) if items.is_empty() => output.push_str("[]"),
        ReceiptValue::Array(items) => {
            output.push_str("[\n");
            for (index, item) in items.iter().enumerate() {
                write_indent(output, depth + 1);
                write_pretty_json(item, depth + 1, output)?;
                if index + 1 < items.len() {
                    output.push(',');
                }
                output.push('\n');
            }
            write_indent(output, depth);
            output.push(']');
        }
        ReceiptValue::Object(fields) if fields.is_empty() => output.push_str("{}"),
        ReceiptValue::Object(fields) => {
            output.push_str("{\n");
            for (index, (key, item)) in fields.iter().enumerate() {
                write_indent(output, depth + 1);
                output.push_str(&python_json_string(key)?);
                output.push_str(": ");
                write_pretty_json(item, depth + 1, output)?;
                if index + 1 < fields.len() {
                    output.push(',');
                }
                output.push('\n');
            }
            write_indent(output, depth);
            output.push('}');
        }
    }
    Ok(())
}

fn write_indent(output: &mut String, depth: usize) {
    output.push_str(&"  ".repeat(depth));
}

fn python_json_string(value: &str) -> Result<String, ReferenceEvaluationError> {
    let encoded = serde_json::to_string(value)?;
    let mut output = String::with_capacity(encoded.len());
    for character in encoded.chars() {
        if character <= '\u{7e}' {
            output.push(character);
        } else if character as u32 <= 0xffff {
            output.push_str(&format!("\\u{:04x}", character as u32));
        } else {
            let scalar = character as u32 - 0x1_0000;
            let high = 0xd800 + (scalar >> 10);
            let low = 0xdc00 + (scalar & 0x3ff);
            output.push_str(&format!("\\u{high:04x}\\u{low:04x}"));
        }
    }
    Ok(output)
}

fn write_compact_sorted_python_json(
    value: &ReceiptValue,
    output: &mut String,
) -> Result<(), ReferenceEvaluationError> {
    match value {
        ReceiptValue::Null => output.push_str("null"),
        ReceiptValue::Bool(false) => output.push_str("false"),
        ReceiptValue::Bool(true) => output.push_str("true"),
        ReceiptValue::Number(number) if number.is_f64() => output.push_str(
            &provenant_core::py::python_str(&Value::Number(number.clone())),
        ),
        ReceiptValue::Number(number) => output.push_str(&number.to_string()),
        ReceiptValue::String(value) => output.push_str(&python_json_string(value)?),
        ReceiptValue::Array(items) => {
            output.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write_compact_sorted_python_json(item, output)?;
            }
            output.push(']');
        }
        ReceiptValue::Object(fields) => {
            let mut sorted = fields.iter().collect::<Vec<_>>();
            sorted.sort_by(|left, right| left.0.cmp(&right.0));
            output.push('{');
            for (index, (key, item)) in sorted.into_iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                output.push_str(&python_json_string(key)?);
                output.push(':');
                write_compact_sorted_python_json(item, output)?;
            }
            output.push('}');
        }
    }
    Ok(())
}

fn to_serde_value(value: &ReceiptValue) -> Result<Value, ReferenceEvaluationError> {
    Ok(serde_json::from_str(&serde_json::to_string(value)?)?)
}

fn get<'a>(
    value: &'a ReceiptValue,
    key: &str,
) -> Result<&'a ReceiptValue, ReferenceEvaluationError> {
    match value {
        ReceiptValue::Object(fields) => fields
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
            .ok_or_else(|| {
                ReferenceEvaluationError::Invalid(format!("required field is missing: {key}"))
            }),
        _ => Err(ReferenceEvaluationError::Invalid(
            "expected JSON object".into(),
        )),
    }
}

fn get_mut<'a>(
    value: &'a mut ReceiptValue,
    key: &str,
) -> Result<&'a mut ReceiptValue, ReferenceEvaluationError> {
    match value {
        ReceiptValue::Object(fields) => fields
            .iter_mut()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
            .ok_or_else(|| {
                ReferenceEvaluationError::Invalid(format!(
                    "required template field is missing: {key}"
                ))
            }),
        _ => Err(ReferenceEvaluationError::Invalid(
            "expected JSON object".into(),
        )),
    }
}

fn set(
    value: &mut ReceiptValue,
    key: &str,
    replacement: ReceiptValue,
) -> Result<(), ReferenceEvaluationError> {
    match value {
        ReceiptValue::Object(fields) => {
            if let Some((_, current)) = fields.iter_mut().find(|(name, _)| name == key) {
                *current = replacement;
            } else {
                fields.push((key.to_owned(), replacement));
            }
            Ok(())
        }
        _ => Err(ReferenceEvaluationError::Invalid(
            "expected JSON object".into(),
        )),
    }
}

fn remove(value: &mut ReceiptValue, key: &str) {
    if let ReceiptValue::Object(fields) = value {
        fields.retain(|(name, _)| name != key);
    }
}

fn array(value: &ReceiptValue) -> Result<&[ReceiptValue], ReferenceEvaluationError> {
    match value {
        ReceiptValue::Array(items) => Ok(items),
        _ => Err(ReferenceEvaluationError::Invalid(
            "expected JSON list".into(),
        )),
    }
}

fn array_mut(value: &mut ReceiptValue) -> Result<&mut Vec<ReceiptValue>, ReferenceEvaluationError> {
    match value {
        ReceiptValue::Array(items) => Ok(items),
        _ => Err(ReferenceEvaluationError::Invalid(
            "expected JSON list".into(),
        )),
    }
}

fn find_in_array<'a>(
    value: &'a ReceiptValue,
    key: &str,
    id: &str,
) -> Result<&'a ReceiptValue, ReferenceEvaluationError> {
    array(value)?
        .iter()
        .find(|item| get(item, key).ok().and_then(|value| string(value).ok()) == Some(id))
        .ok_or_else(|| ReferenceEvaluationError::Invalid(format!("unknown {key}: {id}")))
}

fn find_in_slice<'a>(
    values: &'a [ReceiptValue],
    key: &str,
    id: &str,
) -> Result<&'a ReceiptValue, ReferenceEvaluationError> {
    values
        .iter()
        .find(|item| get(item, key).ok().and_then(|value| string(value).ok()) == Some(id))
        .ok_or_else(|| ReferenceEvaluationError::Invalid(format!("unknown {key}: {id}")))
}

fn string(value: &ReceiptValue) -> Result<&str, ReferenceEvaluationError> {
    match value {
        ReceiptValue::String(value) => Ok(value),
        _ => Err(ReferenceEvaluationError::Invalid("expected string".into())),
    }
}

fn integer(value: i64) -> ReceiptValue {
    ReceiptValue::Number(value.into())
}

fn float(value: f64) -> Result<ReceiptValue, ReferenceEvaluationError> {
    serde_json::Number::from_f64(value)
        .map(ReceiptValue::Number)
        .ok_or_else(|| ReferenceEvaluationError::Invalid("non-finite JSON number".into()))
}

fn text(value: impl Into<String>) -> ReceiptValue {
    ReceiptValue::String(value.into())
}

fn strings(values: &[&str]) -> ReceiptValue {
    ReceiptValue::Array(values.iter().map(|value| text(*value)).collect())
}

fn strings_owned(values: &[String]) -> ReceiptValue {
    ReceiptValue::Array(values.iter().map(|value| text(value.clone())).collect())
}

fn object(values: Vec<(&str, ReceiptValue)>) -> ReceiptValue {
    ReceiptValue::Object(
        values
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}

fn checked_product(values: &[i64]) -> Result<i64, ReferenceEvaluationError> {
    values
        .iter()
        .try_fold(1_i64, |product, value| product.checked_mul(*value))
        .ok_or(ReferenceEvaluationError::NumberOutOfRange)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct MemoryHost {
        workspace_root: std::path::PathBuf,
        files: RefCell<Vec<(String, Vec<u8>)>>,
    }

    impl Default for MemoryHost {
        fn default() -> Self {
            Self {
                workspace_root: std::path::PathBuf::from("."),
                files: RefCell::new(Vec::new()),
            }
        }
    }

    impl ReferenceArtifactHost for MemoryHost {
        fn write_artifact(
            &self,
            artifact_root: &Path,
            relative_path: &str,
            payload: &[u8],
        ) -> Result<(), ReferenceEvaluationError> {
            let target = artifact_root.join(relative_path);
            let relative = target
                .strip_prefix(&self.workspace_root)
                .map_err(|_| {
                    ReferenceEvaluationError::Invalid(
                        "artifact path escapes memory workspace".into(),
                    )
                })?
                .to_string_lossy()
                .replace('\\', "/");
            self.files.borrow_mut().push((relative, payload.to_vec()));
            Ok(())
        }
    }

    fn payload_map(host: &MemoryHost) -> serde_json::Map<String, Value> {
        host.files
            .borrow()
            .iter()
            .map(|(path, bytes)| {
                (
                    path.clone(),
                    Value::String(bytes.iter().map(|byte| format!("{byte:02x}")).collect()),
                )
            })
            .collect()
    }

    fn json_differences(actual: &Value, expected: &Value, path: &str, output: &mut Vec<String>) {
        match (actual, expected) {
            (Value::Object(actual), Value::Object(expected)) => {
                for key in actual.keys().chain(expected.keys()) {
                    if actual.get(key) != expected.get(key) {
                        match (actual.get(key), expected.get(key)) {
                            (Some(left), Some(right)) => {
                                json_differences(left, right, &format!("{path}.{key}"), output)
                            }
                            (left, right) => output
                                .push(format!("{path}.{key}: actual={left:?} expected={right:?}")),
                        }
                    }
                }
            }
            (Value::Array(actual), Value::Array(expected)) => {
                if actual.len() != expected.len() {
                    output.push(format!(
                        "{path}.length: actual={} expected={}",
                        actual.len(),
                        expected.len()
                    ));
                }
                for (index, (left, right)) in actual.iter().zip(expected).enumerate() {
                    if left != right {
                        json_differences(left, right, &format!("{path}[{index}]"), output);
                    }
                }
            }
            _ => output.push(format!("{path}: actual={actual:?} expected={expected:?}")),
        }
    }

    fn template() -> &'static str {
        include_str!("../../../../../skills/evaluate/templates/EVALUATION.template.json")
    }

    #[test]
    fn reference_builder_matches_python_oracle_for_outcomes_and_offsets() {
        let cases = serde_json::json!([
            {"run_id":"delivery-1","evaluation_id":"EVAL-1","status":"pass","repetitions":2,"sample_size":3,"offset":0},
            {"run_id":"delivery-2","evaluation_id":"EVAL-2","status":"fail","repetitions":1,"sample_size":1,"offset":-1440},
            {"run_id":"delivery-3","evaluation_id":"EVAL-3","status":"incomplete","repetitions":0,"sample_size":0,"offset":60}
        ]);
        let expected =
            crate::test_support::oracle_script("oracle_receipts.py", "reference_builder", &cases)
                .expect("required by PROVENANT_PORT_DIFF_REQUIRED");
        let actual = cases
            .as_array()
            .unwrap()
            .iter()
            .map(|case| {
                let row = make_reference_evaluation(
                    template(),
                    case["run_id"].as_str().unwrap_or_default(),
                    case["repetitions"].as_i64().unwrap_or_default(),
                    case["sample_size"].as_i64().unwrap_or_default(),
                    case["evaluation_id"].as_str().unwrap_or_default(),
                    case["status"].as_str().unwrap_or_default(),
                    case["offset"].as_i64().unwrap_or_default(),
                )
                .unwrap();
                serde_json::to_value(row).unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(serde_json::json!(actual), expected);
    }

    #[test]
    fn non_terminal_status_is_refused_and_iso_timestamp_rollover_is_supported() {
        assert!(make_reference_evaluation(template(), "r", 1, 1, "e", "planned", 0).is_err());
        assert_eq!(
            shift_iso_seconds("2026-07-10T00:05:00Z", -10)
                .unwrap()
                .as_deref(),
            Some("2026-07-09T23:55:00Z")
        );
        assert_eq!(
            shift_iso_seconds("2026-07-10 00:02:00.123Z", 60)
                .unwrap()
                .as_deref(),
            Some("2026-07-10T01:02:00.123000Z")
        );
    }

    #[test]
    fn evaluation_artifact_materialisation_matches_python_bytes_and_hashes() {
        let cases = serde_json::json!([
            {"run_id":"delivery-1","evaluation_id":"EVAL-1","status":"pass","repetitions":1,"sample_size":2,"offset":0},
            {"run_id":"delivery-2","evaluation_id":"EVAL-2","status":"fail","repetitions":1,"sample_size":1,"offset":30}
        ]);
        let expected = crate::test_support::oracle_script(
            "oracle_receipts.py",
            "evaluation_materialise",
            &cases,
        )
        .expect("required by PROVENANT_PORT_DIFF_REQUIRED");
        let actual = cases
            .as_array()
            .unwrap()
            .iter()
            .map(|case| {
                let mut evaluation = make_reference_evaluation(
                    template(),
                    case["run_id"].as_str().unwrap_or_default(),
                    case["repetitions"].as_i64().unwrap_or_default(),
                    case["sample_size"].as_i64().unwrap_or_default(),
                    case["evaluation_id"].as_str().unwrap_or_default(),
                    case["status"].as_str().unwrap_or_default(),
                    case["offset"].as_i64().unwrap_or_default(),
                )
                .unwrap();
                let host = MemoryHost::default();
                materialise_evaluation(&mut evaluation, Path::new("."), &host).unwrap();
                serde_json::json!({
                    "evaluation": to_serde_value(&evaluation).unwrap(),
                    "files": payload_map(&host)
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(serde_json::json!(actual), expected);
    }

    #[test]
    fn custom_template_ascii_plan_bytes_and_iso_timestamp_match_python() {
        let mut custom_template: Value = serde_json::from_str(template()).unwrap();
        custom_template["plan"]["dataset"]["provenance"]["source"] = Value::String("雪".into());
        custom_template["plan"]["custom_metric"] = serde_json::json!(1.0e-7);
        custom_template["plan"]["custom_metric_upper"] = serde_json::json!(1.0e16);
        custom_template["custom_timestamp"] = Value::String("2026-07-10 00:02:00.123Z".into());
        let cases = serde_json::json!([{
            "run_id":"delivery-custom",
            "evaluation_id":"EVAL-CUSTOM",
            "status":"pass",
            "repetitions":1,
            "sample_size":1,
            "offset":60,
            "template":custom_template
        }]);
        let expected = crate::test_support::oracle_script(
            "oracle_receipts.py",
            "evaluation_materialise",
            &cases,
        )
        .expect("required by PROVENANT_PORT_DIFF_REQUIRED");
        let case = cases.as_array().and_then(|items| items.first()).unwrap();
        let template_json = serde_json::to_string(&case["template"]).unwrap();
        let mut evaluation = make_reference_evaluation(
            &template_json,
            case["run_id"].as_str().unwrap_or_default(),
            case["repetitions"].as_i64().unwrap_or_default(),
            case["sample_size"].as_i64().unwrap_or_default(),
            case["evaluation_id"].as_str().unwrap_or_default(),
            case["status"].as_str().unwrap_or_default(),
            case["offset"].as_i64().unwrap_or_default(),
        )
        .unwrap();
        let host = MemoryHost::default();
        materialise_evaluation(&mut evaluation, Path::new("."), &host).unwrap();
        let actual = serde_json::json!({
            "evaluation":to_serde_value(&evaluation).unwrap(),
            "files":payload_map(&host)
        });
        assert_eq!(serde_json::json!([actual]), expected);
    }

    #[test]
    fn deterministic_bundle_materialisation_matches_python_digest_bindings() {
        let cases = serde_json::json!([
            {
                "run": {
                    "evidence": [
                        {"id":"check-1","kind":"deterministic","gate":"tests","status":"pass","method":"check","source_paths":["src"],"artifact_id":"bundle","result":{"exit_code":0,"receipt_digest":"old"}},
                        {"id":"approval","kind":"human","gate":"approve","status":"pass","method":"approve","source_paths":[],"artifact_id":"bundle"}
                    ],
                    "authority":{"evidence":"approval","evidence_digest":"old"}
                },
                "artifact":{"id":"bundle","path":"evidence/bundle.json","digest":"old"}
            }
        ]);
        let expected =
            crate::test_support::oracle_script("oracle_receipts.py", "bundle_builder", &cases)
                .expect("required by PROVENANT_PORT_DIFF_REQUIRED");
        let case = cases.as_array().and_then(|items| items.first()).unwrap();
        let mut run = ReceiptValue::from_json_str(&case["run"].to_string()).unwrap();
        let mut artifact = ReceiptValue::from_json_str(&case["artifact"].to_string()).unwrap();
        let host = MemoryHost::default();
        materialise_deterministic_evidence_bundle(&mut run, &mut artifact, Path::new("."), &host)
            .unwrap();
        let payload_hex = payload_map(&host)
            .get("evidence/bundle.json")
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            serde_json::json!([{
                "run": to_serde_value(&run).unwrap(),
                "artifact": to_serde_value(&artifact).unwrap(),
                "payload_hex": payload_hex
            }]),
            expected
        );
    }

    #[test]
    fn pretty_wire_keeps_insertion_order_and_python_ascii_escapes() {
        let value = object(vec![
            ("last_first", text("雪🙂")),
            ("second", ReceiptValue::Array(vec![integer(1), text("ok")])),
        ]);
        assert_eq!(
            pretty_json(&value).unwrap(),
            "{\n  \"last_first\": \"\\u96ea\\ud83d\\ude42\",\n  \"second\": [\n    1,\n    \"ok\"\n  ]\n}"
        );
    }

    #[test]
    fn complete_reference_run_materialisation_matches_python() {
        let cases = serde_json::json!([
            {"profile":"agent-product","repetitions":1,"sample_size":2}
        ]);
        let expected = crate::test_support::oracle_script(
            "oracle_receipts.py",
            "reference_run_materialise",
            &cases,
        )
        .expect("required by PROVENANT_PORT_DIFF_REQUIRED");
        let expected_case = expected.as_array().and_then(|items| items.first()).unwrap();
        let mut run = ReceiptValue::from_json_str(&expected_case["before"].to_string()).unwrap();
        let host = MemoryHost::default();
        materialise_reference_run(
            &mut run,
            template(),
            Path::new("."),
            cases[0]["repetitions"].as_i64().unwrap_or_default(),
            cases[0]["sample_size"].as_i64().unwrap_or_default(),
            &host,
        )
        .unwrap();
        let actual_run = to_serde_value(&run).unwrap();
        let mut differences = Vec::new();
        json_differences(&actual_run, &expected_case["run"], "run", &mut differences);
        assert!(differences.is_empty(), "run mismatches: {differences:?}");
        assert_eq!(Value::Object(payload_map(&host)), expected_case["files"]);
    }
}
