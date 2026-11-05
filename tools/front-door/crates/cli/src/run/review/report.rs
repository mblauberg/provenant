//! Verdict totals and the human-readable review text.
use super::*;

pub(super) fn aggregate_verdicts(data: &Value) -> Value {
    let mut counts = json!({"approve": 0, "approve_with_nits": 0, "request_changes": 0, "inconclusive": 0, "unknown": 0, "not_submitted": 0});
    if let Some(rows) = data["reviewers"].as_array() {
        for row in rows {
            let verdict = row["verdict"].as_str().unwrap_or("unknown");
            let key = if row["state"] == "not_submitted" {
                "not_submitted"
            } else {
                match verdict {
                    "approve" | "approve_with_nits" | "request_changes" | "inconclusive" => verdict,
                    _ => "unknown",
                }
            };
            counts[key] = json!(counts[key].as_u64().unwrap_or(0) + 1);
        }
    }
    counts
}
pub(super) fn review_text(data: &Value) -> String {
    let mut text = format!(
        "review {} ({}; size={}; kind={}; risk={})\n",
        data["shape"].as_str().unwrap_or("unknown"),
        data["selection_source"].as_str().unwrap_or("unknown"),
        data["size"].as_str().unwrap_or("unknown"),
        data["kind"].as_str().unwrap_or("unknown"),
        data["risk"].as_str().unwrap_or("unknown")
    );
    if let Some(reasons) = data["lens_reasons"]
        .as_array()
        .filter(|rows| !rows.is_empty())
    {
        text.push_str(&format!(
            "  lens reasons: {}\n",
            reasons
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for warning in data["warnings"].as_array().into_iter().flatten() {
        text.push_str(&format!("  warning: {}\n", warning.as_str().unwrap_or("")));
    }
    if let Some(tags) = data["risk_tags"].as_array().filter(|tags| !tags.is_empty()) {
        text.push_str(&format!(
            "  risk tags: {}\n",
            tags.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(rows) = data["reviewers"].as_array() {
        for row in rows {
            if row["state"] == "planned" {
                text.push_str(&format!(
                    "  lens={} pool={}\n",
                    row["lens"].as_str().unwrap_or("general"),
                    row["pool"].as_str().unwrap_or("unknown")
                ));
                continue;
            }
            let model = row["applied_route"]["model"]
                .as_str()
                .unwrap_or("unresolved");
            let effort = row["applied_route"]["effort"]
                .as_str()
                .map(|e| format!("@{e}"))
                .unwrap_or_default();
            text.push_str(&format!(
                "  lens={} run={} operation={} {}{} state={} verdict={} output={}{}\n",
                row["lens"].as_str().unwrap_or("general"),
                row["run_id"].as_str().unwrap_or("unknown"),
                row["operation_id"].as_str().unwrap_or("unknown"),
                model,
                effort,
                row["state"].as_str().unwrap_or("pending"),
                row["verdict"].as_str().unwrap_or("unknown"),
                structured_path_text(&row["result_path"])
                    .as_deref()
                    .unwrap_or("none"),
                if row["substitution"].as_bool() == Some(true) {
                    " (family substitution)"
                } else {
                    ""
                }
            ));
            if let Some(id) = row["run_id"].as_str()
                && matches!(
                    row["state"].as_str(),
                    Some("queued" | "starting" | "running" | "settling" | "submitted")
                )
            {
                text.push_str(&format!("    collect: provenant lanes --id {id} --wait 4m; cancel: provenant cancel {id}\n"));
            }
        }
    }
    if data["reviewers"]
        .as_array()
        .is_some_and(|rows| rows.iter().any(|row| row["state"] == "not_submitted"))
    {
        text.push_str("  Retry the same review command to submit remaining reviewers and reattach to existing runs.\n");
    }
    if data["reviewers"].as_array().is_some_and(|rows| {
        rows.iter().any(|row| {
            matches!(
                row["state"].as_str(),
                Some(
                    "failed"
                        | "rejected"
                        | "cancelled"
                        | "timed_out"
                        | "rate_limited"
                        | "quota_limited"
                        | "auth_required"
                        | "model_unavailable"
                        | "permission_denied"
                        | "quarantined"
                        | "lost"
                        | "uncommitted"
                )
            )
        })
    }) {
        text.push_str("  Retry the same review command to resubmit failed or cancelled reviewers and reattach to the others.\n");
    }
    text
}
