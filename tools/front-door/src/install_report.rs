//! Parsing of a child `provenant` report for `upgrade --verify`: the
//! innermost typed error from its JSON envelopes and the graph and smoke
//! verdicts. Pure text functions; the host rendering stays in `install.rs`.

/// Cap for one host's reason in a refusal. The full per-host view stays in the
/// human output; only this bounded summary travels in `error.message`.
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

/// The innermost `data.error` of an envelope: a machine code and its one-line
/// message. A wrapper envelope may carry a deeper one inside its message; the
/// deeper reason wins, so no wrapper chain reaches the operator.
pub(super) fn innermost_error(value: &serde_json::Value) -> Option<(String, String)> {
    let error = value.get("data")?.get("error")?;
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
    let reason = child_envelopes(output)
        .into_iter()
        .filter_map(|value| innermost_error(&value))
        .next_back()
        .map(|(code, message)| format!("{code}: {message}"))
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
