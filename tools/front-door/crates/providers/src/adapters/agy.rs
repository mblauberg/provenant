use super::*;

pub fn agy(plan: &Plan) -> AdapterProfile {
    let mut argv = vec![
        "--output-format".into(),
        "stream-json".into(),
        "--disable-slash-commands".into(),
        "--print-timeout".into(),
        format!("{}s", plan.timeout_seconds.max(1)),
    ];
    if plan.mode.is_writer() {
        argv.push("--dangerously-skip-permissions".into());
    } else if plan.agy_sandbox {
        argv.push("--sandbox".into());
    }
    if let Some(session) = plan
        .resume_session
        .as_ref()
        .filter(|value| !value.is_empty())
    {
        argv.extend(["--conversation".into(), session.clone()]);
    }
    if let Some(model) = plan.model.as_ref().filter(|value| !value.is_empty()) {
        argv.extend(["--model".into(), model.clone()]);
    }
    if let Some(effort) = plan.effort.as_ref().filter(|value| !value.is_empty()) {
        argv.extend(["--effort".into(), effort.clone()]);
    }
    for directory in &plan.add_dirs {
        argv.extend(["--add-dir".into(), directory.clone()]);
    }
    argv.extend([
        "--print".into(),
        format!("{}\nTask:\n{}", plan.boundary_prompt, plan.prompt),
    ]);
    profile(
        "agy",
        argv,
        Metadata {
            transport: PromptTransport::Argv,
            stdin: StdinMode::Null,
            output: "stream-json",
            effort_flag: Some("--effort"),
            session_keys: &["session_id", "conversation_id", "conversationId"],
            model_source: Some("agy:init.model|log"),
        },
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn plan(mode: Mode) -> Plan {
        Plan {
            mode,
            prompt: "do it".into(),
            boundary_prompt: "Read only".into(),
            timeout_seconds: 0,
            agy_sandbox: true,
            resume_session: None,
            model: Some("model-x".into()),
            effort: Some("high".into()),
            session_id: "session-1".into(),
            cwd: "/repo".into(),
            add_dirs: vec!["/shared".into()],
            claude_api_key: false,
            endpoint_base_url: None,
            context_ceiling: Some(1000),
        }
    }

    #[test]
    fn agy_clamps_timeout_and_selects_mode_permission() {
        let read = agy(&plan(Mode::ReadOnly));
        assert_eq!(
            &read.argv[..7],
            &[
                "agy",
                "--output-format",
                "stream-json",
                "--disable-slash-commands",
                "--print-timeout",
                "1s",
                "--sandbox"
            ]
        );
        let writer = agy(&plan(Mode::WorktreeWrite));
        assert!(writer
            .argv
            .contains(&"--dangerously-skip-permissions".into()));
        assert_eq!(writer.argv.last().unwrap(), "Read only\nTask:\ndo it");
    }
}
