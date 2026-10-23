use super::*;

pub fn kiro(plan: &Plan) -> AdapterProfile {
    let mut argv = vec![
        "chat".into(),
        "--no-interactive".into(),
        "--agent-engine".into(),
        "v3".into(),
        "--output-format".into(),
        "stream-json".into(),
    ];
    if plan.mode.is_writer() {
        argv.push("--trust-all-tools".into());
    } else {
        argv.push("--trust-tools=fs_read,grep,glob".into());
    }
    if let Some(session) = plan
        .resume_session
        .as_ref()
        .filter(|value| !value.is_empty())
    {
        argv.extend(["--resume-id".into(), session.clone()]);
    }
    if let Some(model) = plan
        .model
        .as_ref()
        .filter(|model| !model.is_empty() && model.as_str() != "auto")
    {
        argv.extend(["--model".into(), model.clone()]);
    }
    if let Some(effort) = plan.effort.as_ref().filter(|value| !value.is_empty()) {
        argv.extend(["--effort".into(), effort.clone()]);
    }
    argv.push(format!("{}\n\n{}", plan.boundary_prompt, plan.prompt));
    profile(
        "kiro-cli",
        argv,
        Metadata {
            transport: PromptTransport::Argv,
            stdin: StdinMode::Null,
            output: "stream-json",
            effort_flag: Some("--effort"),
            session_keys: &[
                "sessionId",
                "session_id",
                "conversation_id",
                "conversationId",
            ],
            model_source: Some("kiro:stream"),
        },
    )
}

/// Whether the router's version-bound negative write probe is still fresh.
/// `now` is explicit so policy tests never depend on wall-clock timing.
pub fn read_only_guarantee(route: &serde_json::Value, now: jiff::Timestamp) -> &'static str {
    let valid = (|| {
        let probe = route.get("read_only_probe")?;
        let checked_at = probe.get("checked_at")?.as_str()?.replace('Z', "+00:00");
        let checked_at: jiff::Timestamp = checked_at.parse().ok()?;
        let age = now.as_nanosecond() - checked_at.as_nanosecond();
        Some(
            probe.get("cli_version")?.as_str()? == route.get("cli_version")?.as_str()?
                && !route.get("cli_version")?.as_str()?.is_empty()
                && (0..86_400_000_000_000i128).contains(&age)
                && probe.get("attempted_write")?.as_bool()?
                && probe.get("permission_denied")?.as_bool()?
                && !probe.get("file_created")?.as_bool()?,
        )
    })()
    .unwrap_or(false);
    if valid {
        "enforced"
    } else {
        "prompt_only"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::agy::tests::plan;

    #[test]
    fn kiro_uses_v3_and_omits_explicit_auto_model() {
        let mut p = plan(Mode::ReadOnly);
        p.model = Some("auto".into());
        let adapter = kiro(&p);
        assert!(adapter
            .argv
            .windows(2)
            .any(|v| v == ["--agent-engine", "v3"]));
        assert!(!adapter.argv.contains(&"--model".into()));
        assert_eq!(adapter.argv.last().unwrap(), "Read only\n\ndo it");
    }

    #[test]
    fn read_only_probe_requires_matching_cli_and_is_less_than_one_day_old() {
        let now: jiff::Timestamp = "2026-10-23T12:00:00Z".parse().unwrap();
        let route = serde_json::json!({
            "cli_version": "2.23.1",
            "read_only_probe": {
                "cli_version": "2.23.1",
                "checked_at": "2026-10-22T12:00:01Z",
                "attempted_write": true,
                "permission_denied": true,
                "file_created": false
            }
        });
        assert_eq!(read_only_guarantee(&route, now), "enforced");
        let stale = serde_json::json!({
            "cli_version": "2.23.1",
            "read_only_probe": {
                "cli_version": "2.23.1",
                "checked_at": "2026-10-22T12:00:00Z",
                "attempted_write": true,
                "permission_denied": true,
                "file_created": false
            }
        });
        assert_eq!(read_only_guarantee(&stale, now), "prompt_only");
    }
}
