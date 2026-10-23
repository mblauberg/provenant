use super::*;

pub fn claude(plan: &Plan) -> AdapterProfile {
    let mut argv = vec![
        "-p".into(),
        if plan.claude_api_key {
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
    if let Some(session) = &plan.resume_session {
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
    if let Some(model) = &plan.model {
        argv.extend(["--model".into(), model.clone()]);
    }
    if let Some(effort) = &plan.effort {
        argv.extend(["--effort".into(), effort.clone()]);
    }
    if let Some(ceiling) = plan.context_ceiling {
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
}
