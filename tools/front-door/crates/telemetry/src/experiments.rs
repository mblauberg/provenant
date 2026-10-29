//! Experiment views over the offline pipeline outcome and ranking report.
use serde_json::{Value, json};

const PREFIX: &str = "experiment:";

/// Select one experiment's arms and advisory comparisons from `pipeline_report`.
pub fn show(pipeline: &Value, experiment: &str) -> Result<Value, String> {
    if experiment.is_empty() || experiment.contains(':') {
        return Err("experiment ID must be non-empty and cannot contain ':'".into());
    }
    let marker = format!("{PREFIX}{experiment}:");
    let ranking = pipeline
        .get("ranking")
        .ok_or("pipeline outcome report has no ranking")?;
    let arms = ranking
        .get("arms")
        .and_then(Value::as_array)
        .ok_or("pipeline ranking has no arms")?;
    let selected: Vec<_> = arms
        .iter()
        .filter(|arm| arm.get("dimension").and_then(Value::as_str) == Some("pattern"))
        .filter(|arm| {
            arm.get("choice")
                .and_then(Value::as_str)
                .is_some_and(|choice| choice.starts_with(&marker))
        })
        .cloned()
        .collect();
    let suggestions: Vec<_> = ranking
        .get("suggestions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|suggestion| {
            ["preferred", "compared_with"].into_iter().all(|key| {
                suggestion
                    .get(key)
                    .and_then(Value::as_str)
                    .is_some_and(|choice| choice.starts_with(&marker))
            })
        })
        .cloned()
        .collect();
    Ok(json!({
        "experiment": experiment,
        "arms": selected,
        "suggestions": suggestions,
        "ci": pipeline.pointer("/outcomes/ci"),
        "unattributed_rows": pipeline.pointer("/outcomes/unattributed_rows"),
        "method": "offline pipeline outcomes; stratified by risk tier and task kind"
    }))
}

#[cfg(test)]
mod tests {
    use super::show;
    use serde_json::json;

    #[test]
    fn show_filters_arms_and_comparisons_to_the_experiment() {
        let report = json!({"outcomes":{"ci":"measured","unattributed_rows":2},"ranking":{"arms":[
            {"dimension":"pattern","choice":"experiment:retry:control","risk_tier":"routine"},
            {"dimension":"pattern","choice":"experiment:retry:variant","risk_tier":"routine"},
            {"dimension":"pattern","choice":"experiment:other:variant"}
        ],"suggestions":[
            {"preferred":"experiment:retry:variant","compared_with":"experiment:retry:control"},
            {"preferred":"experiment:other:variant","compared_with":"experiment:retry:control"}
        ]}});
        let result = show(&report, "retry").unwrap();
        assert_eq!(result["arms"].as_array().unwrap().len(), 2);
        assert_eq!(result["suggestions"].as_array().unwrap().len(), 1);
        assert_eq!(result["ci"], "measured");
    }
}
