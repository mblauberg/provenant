use super::*;

pub fn claude(plan: &Plan) -> AdapterProfile {
    let mut argv = vec![
        "-p".into(),
        if plan.claude_api_key
            || plan
                .endpoint_base_url
                .as_ref()
                .is_some_and(|url| !url.is_empty())
        {
            "--bare"
        } else {
            "--safe-mode"
        }
        .into(),
        "--strict-mcp-config".into(),
        "--disable-slash-commands".into(),
        "--permission-prompts".into(),
        "none".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
    ];
    if let Some(session) = plan
        .resume_session
        .as_ref()
        .filter(|value| !value.is_empty())
    {
        argv.extend(["--resume".into(), session.clone()]);
    } else {
        argv.extend(["--session-id".into(), plan.session_id.clone()]);
    }
    let boundary = if plan.mode.is_writer() {
        argv.extend([
            "--add-dir".into(),
            plan.cwd.clone(),
            "--permission-mode".into(),
            "acceptEdits".into(),
            "--allowedTools".into(),
            "Bash,Edit,Write,MultiEdit,NotebookEdit,Read,Grep,Glob".into(),
        ]);
        plan.boundary_prompt.clone()
    } else {
        argv.extend([
            "--permission-mode".into(),
            "default".into(),
            "--tools".into(),
            "Read,Grep,Glob,Bash".into(),
            "--allowedTools".into(),
            "Bash".into(),
        ]);
        plan.boundary_prompt.replace(
            "Use file-reading tools only.",
            "Shell commands that do not modify files are allowed.",
        )
    };
    argv.extend(["--system-prompt".into(), boundary]);
    for directory in &plan.add_dirs {
        argv.extend(["--add-dir".into(), directory.clone()]);
    }
    if let Some(model) = plan.model.as_ref().filter(|value| !value.is_empty()) {
        argv.extend(["--model".into(), model.clone()]);
    }
    if let Some(effort) = plan.effort.as_ref().filter(|value| !value.is_empty()) {
        argv.extend(["--effort".into(), effort.clone()]);
    }
    if let Some(ceiling) = plan.context_ceiling.filter(|value| *value > 0) {
        argv.extend(["--autocompact".into(), ceiling.to_string()]);
    }
    profile(
        "claude",
        argv,
        Metadata {
            transport: PromptTransport::Stdin,
            stdin: StdinMode::Prompt,
            output: "stream-json",
            effort_flag: Some("--effort"),
            session_keys: &["session_id"],
            model_source: Some("claude:init.model"),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::agy::tests::plan;

    #[test]
    fn claude_read_only_rewrites_boundary_and_writer_uses_edit_permissions() {
        let mut read_plan = plan(Mode::ReadOnly);
        read_plan.boundary_prompt = "Use file-reading tools only.".into();
        let read = claude(&read_plan);
        assert!(read.argv.windows(2).any(|args| args
            == [
                "--system-prompt",
                "Shell commands that do not modify files are allowed."
            ]));
        let writer = claude(&plan(Mode::WorktreeWrite));
        assert!(writer.argv.contains(&"acceptEdits".into()));
        assert!(writer.argv.contains(&"/repo".into()));
    }

    #[test]
    fn empty_endpoint_does_not_select_api_key_mode() {
        let mut p = plan(Mode::ReadOnly);
        p.endpoint_base_url = Some(String::new());
        let profile = claude(&p);
        assert!(profile.argv.contains(&"--safe-mode".into()));
        assert!(!profile.argv.contains(&"--bare".into()));
    }
}
