//! Synthetic reference evaluation builder and artifact binding policy.
//!
//! The source reads a versioned evaluation template and writes its artifacts
//! beneath caller-selected workspaces. Ordered JSON is retained through the
//! shared `ReceiptValue` record type. Writes are delegated to a confinement
//! host because the supervisor safe filesystem API is not yet exposed here.

use provenant_core::py::python_fromisoformat;
use provenant_records::delivery_run_shape::ReceiptValue;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
