//! Parsing of a child `provenant` report for `upgrade --verify`: the
//! innermost typed error from its JSON envelopes and the graph and smoke
//! verdicts. Pure text functions; the host rendering stays in `install.rs`.

/// Cap for one host's reason in a refusal. The renderer drops human output when
/// an error is set, so the full per-host view lives in `--verify --json`; only
/// this bounded summary travels in `error.message`.
pub(super) const HOST_REASON_LIMIT: usize = 300;

/// The first meaningful line of a report, trimmed.
pub(super) fn first_meaningful_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("no error detail")
        .to_owned()
}

/// Parse the single-line JSON envelopes a child prints. An executor wraps a
/// non-zero exit in `Command failed` prose before the report reaches us, so the
/// envelope may be preceded by a prefix rather than starting the line.
pub(super) fn child_envelopes(text: &str) -> Vec<serde_json::Value> {
    let mut values = Vec::new();
    for line in text.lines() {
        let (Some(start), Some(end)) = (line.find('{'), line.rfind('}')) else {
            continue;
        };
        if end >= start
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(&line[start..=end])
        {
            values.push(value);
        }
    }
    values
}

/// The innermost top-level `error` of an envelope: a machine code and its
/// one-line message. A wrapper envelope may carry a deeper one inside its
/// message; the deeper reason wins, so no wrapper chain reaches the operator.
pub(super) fn innermost_error(value: &serde_json::Value) -> Option<(String, String)> {
    let error = value.get("error").filter(|error| !error.is_null())?;
    let code = error.get("code")?.as_str()?.to_owned();
    let message = error
        .get("message")
        .and_then(serde_json::Value::as_str)
        .or_else(|| error.get("cause").and_then(serde_json::Value::as_str))
        .unwrap_or_default();
    match child_envelopes(message)
        .into_iter()
        .filter_map(|v| innermost_error(&v))
        .next_back()
    {
        Some(deeper) => Some(deeper),
        None => Some((code, first_meaningful_line(message))),
    }
}

/// The typed reason from a failing child's report, falling back to its first
/// line only when no envelope carried an error.
pub(super) fn innermost_error_text(report: &str) -> (String, String) {
    child_envelopes(report)
        .into_iter()
        .filter_map(|value| innermost_error(&value))
        .next_back()
        .unwrap_or_else(|| {
            let line = first_meaningful_line(report);
            (
                "UPGRADE_VERIFY".to_owned(),
                unwrap_command_failure(&line).to_owned(),
            )
        })
}

/// Bound on the root cause's message inside a one-line host reason. The graph,
/// node, code and next command are written after this bound, so a long checker
/// note cannot clip the parts that tell the operator where to look.
const SMOKE_CAUSE_BYTES: usize = 140;

/// The shared graph cause paired with the graph that carried it. This is only
/// a projection of `provenant_fabric_runtime::graph::RootCause` for the
/// renderer: the selection itself lives in the one cause walk and is not
/// repeated here.
pub(super) struct SmokeCause {
    pub graph: String,
    pub node: String,
    pub code: String,
    pub message: String,
}

/// Read the newest checkpoint envelope with an actionable cause through the
/// shared root-cause walk. Both `run` and `status` envelopes carry the
/// checkpoint; a dependant whose only evidence is `upstream_blocked` is never
/// selected as the cause.
pub(super) fn smoke_root_cause(report: &str) -> Option<SmokeCause> {
    child_envelopes(report).into_iter().rev().find_map(|value| {
        let checkpoint = &value["data"]["checkpoint"];
        if !checkpoint.is_object() {
            return None;
        }
        let cause = provenant_fabric_runtime::graph::root_cause::root_cause_from_value(checkpoint)?;
        Some(SmokeCause {
            graph: checkpoint["graph_id"].as_str()?.to_owned(),
            node: cause.node,
            code: cause.code,
            message: cause.message,
        })
    })
}

/// The one-line verify summary: the graph, the node that actually failed, its
/// code and message, and the command that reads that graph. The cause is
/// bounded before assembly so the next command survives the host reason cap.
pub(super) fn smoke_failure_summary(report: &str) -> Option<String> {
    let cause = smoke_root_cause(report)?;
    let message = provenant_cli::bounded_text(&cause.message, SMOKE_CAUSE_BYTES);
    Some(format!(
        "smoke graph {} failed at {}: {}: {}; next: `provenant status {}`",
        cause.graph, cause.node, cause.code, message, cause.graph
    ))
}

/// Drop the executor's `Command failed (…): ` wrapper from a plain reason, so
/// even a non-envelope failure names the cause rather than the exit status.
pub(super) fn unwrap_command_failure(line: &str) -> &str {
    line.strip_prefix("Command failed (")
        .and_then(|rest| rest.find("): ").map(|end| &rest[end + 3..]))
        .unwrap_or(line)
}

/// The one-line reason a host failed. Typed envelopes give the innermost code
/// and message; otherwise fall back to the first meaningful line with its
/// executor wrapper removed. Never a `Command failed` chain or a raw envelope.
pub(super) fn first_error_line(output: &str) -> String {
    // A smoke graph checkpoint names the cause, not the dependant whose
    // `upstream_blocked` envelope is the innermost error in the report. When
    // the child carried that summary in its own envelope, no checkpoint is
    // present here and the ordinary innermost reason is the summary already.
    let reason = smoke_failure_summary(output)
        .or_else(|| {
            child_envelopes(output)
                .into_iter()
                .filter_map(|value| innermost_error(&value))
                .next_back()
                .map(|(code, message)| format!("{code}: {message}"))
        })
        .unwrap_or_else(|| {
            let line = first_meaningful_line(output);
            unwrap_command_failure(&line).to_owned()
        });
    provenant_cli::bounded_text(&reason, HOST_REASON_LIMIT)
}

/// Whether a captured line is a `--json` command envelope, not human text.
pub(super) fn is_envelope(line: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(line)
        .is_ok_and(|value| value.get("schema_version").is_some() && value.get("command").is_some())
}

/// Collect `VERDICT:` values from a smoke envelope's data at any depth, once.
pub(super) fn collect_verdicts(value: &serde_json::Value, verdicts: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => {
            if text.starts_with("VERDICT:") && !verdicts.iter().any(|seen| seen == text) {
                verdicts.push(text.clone());
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_verdicts(item, verdicts);
            }
        }
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                if key == "verdict" {
                    if let Some(text) = child.as_str()
                        && !verdicts.iter().any(|seen| seen == text)
                    {
                        verdicts.push(text.to_owned());
                    }
                } else {
                    collect_verdicts(child, verdicts);
                }
            }
        }
        _ => {}
    }
}

/// The graph verdict from the smoke child's `run`/`wait`/`status` envelopes.
/// The verdict is typed data (`data.verdict` or a review row's verdict), never
/// a line matched out of human prose.
pub(super) fn graph_verdict(output: &str) -> String {
    let mut verdicts = Vec::new();
    for value in child_envelopes(output) {
        if value.get("schema_version").is_some() && value.get("command").is_some() {
            collect_verdicts(&value["data"], &mut verdicts);
        }
    }
    verdicts.into_iter().last().unwrap_or_default()
}

/// The smoke host's compact detail: the verdict the verify child published as
/// typed data, one line. Raw envelopes are kept only under `--full`.
pub(super) fn smoke_verdict(output: &str) -> String {
    match child_envelopes(output)
        .into_iter()
        .find_map(|value| value["data"]["verdict"].as_str().map(str::to_owned))
        .filter(|verdict| !verdict.is_empty())
    {
        Some(verdict) => format!("  smoke graph: {verdict}\n"),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::{first_error_line, smoke_failure_summary, smoke_root_cause};

    #[test]
    fn upgrade_verify_root_cause_skips_upstream_blocked() {
        // The blocked dependant carries the innermost envelope, but only the
        // node that actually failed is actionable. The upgrade summary must
        // use the shared cause walk rather than selecting it again here.
        let dependant = serde_json::json!({
            "id": "task--review",
            "state": "blocked",
            "advance_code": "upstream_blocked",
            "advance_note": "blocked by task--read (Blocked)",
            "attempts": []
        });
        let cause = serde_json::json!({
            "id": "task--read",
            "state": "failed",
            "advance_code": "launch_refused",
            "advance_owner_code": "confinement_refused",
            "advance_note": "[confinement_refused] peer_project_unprojectable on /fixture/.git",
            "attempts": []
        });
        let envelope = serde_json::json!({
            "schema_version": 1,
            "command": "run",
            "data": {"checkpoint": {"graph_id": "smoke-4102", "nodes": [dependant, cause]}}
        });
        let report = format!("Command failed (Exit(1)): {envelope}\n");

        let selected = smoke_root_cause(&report).expect("the checkpoint names a cause");
        assert_eq!(selected.graph, "smoke-4102");
        assert_eq!(selected.node, "task--read");
        assert_eq!(selected.code, "confinement_refused");

        let summary = smoke_failure_summary(&report).expect("a cause yields a summary");
        for expected in [
            "smoke-4102",
            "failed at task--read",
            "confinement_refused",
            "peer_project_unprojectable",
            "next: `provenant status smoke-4102`",
        ] {
            assert!(
                summary.contains(expected),
                "{expected} missing from {summary}"
            );
        }
        assert!(!summary.contains("blocked by"), "{summary}");
        assert!(!summary.contains("upstream_blocked"), "{summary}");

        // The host reason reaches the operator through the same walk.
        let reason = first_error_line(&report);
        assert!(reason.contains("task--read"), "{reason}");
        assert!(!reason.contains("upstream_blocked"), "{reason}");
    }
}
