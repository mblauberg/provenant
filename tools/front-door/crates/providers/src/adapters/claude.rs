use super::*;

/// Python's complete API-key predicate, with the environment value injected.
/// `anthropic_api_key` is the caller's lookup of `ANTHROPIC_API_KEY`.
pub fn uses_api_key(route: &serde_json::Value, anthropic_api_key: Option<&str>) -> bool {
    anthropic_api_key.is_some_and(|key| !key.is_empty())
        || route
            .get("endpoint_base_url")
            .is_some_and(provenant_core::py::python_truth)
}

pub fn claude(plan: &Plan, anthropic_api_key: Option<&str>) -> AdapterProfile {
    let mut argv = vec![
        "-p".into(),
        if uses_api_key(
            &serde_json::json!({"endpoint_base_url": plan.endpoint_base_url}),
            anthropic_api_key,
        ) {
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
    fn api_key_predicate_matches_python_with_injected_environment_value() {
        let routes = serde_json::json!([
            {}, {"endpoint_base_url": null}, {"endpoint_base_url": ""},
            {"endpoint_base_url": "https://example.invalid"},
            {"endpoint_base_url": false}, {"endpoint_base_url": true},
            {"endpoint_base_url": 0}, {"endpoint_base_url": -1},
            {"endpoint_base_url": []}, {"endpoint_base_url": [0]},
            {"endpoint_base_url": {}}, {"endpoint_base_url": {"x": 0}}
        ]);
        for api_key in [None, Some(""), Some("fixture-key")] {
            let actual: Vec<_> = routes
                .as_array()
                .unwrap()
                .iter()
                .map(|route| uses_api_key(route, api_key))
                .collect();
            let Some(expected): Option<Vec<bool>> = crate::oracle::python(
                "import json,sys; from skills.orchestrate.scripts.adapters.claude import uses_api_key; print(json.dumps([uses_api_key(r) for r in json.load(sys.stdin)]))",
                &routes, api_key,
            ) else { return; };
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn claude_read_only_rewrites_boundary_and_writer_uses_edit_permissions() {
        let mut read_plan = plan(Mode::ReadOnly);
        read_plan.boundary_prompt = "Use file-reading tools only.".into();
        let read = claude(&read_plan, None);
        assert!(read.argv.windows(2).any(|args| args
            == [
                "--system-prompt",
                "Shell commands that do not modify files are allowed."
            ]));
        let writer = claude(&plan(Mode::WorktreeWrite), None);
        assert!(writer.argv.contains(&"acceptEdits".into()));
        assert!(writer.argv.contains(&"/repo".into()));
    }

    #[test]
    fn empty_endpoint_does_not_select_api_key_mode() {
        let mut p = plan(Mode::ReadOnly);
        p.endpoint_base_url = Some(serde_json::json!(""));
        let profile = claude(&p, None);
        assert!(profile.argv.contains(&"--safe-mode".into()));
        assert!(!profile.argv.contains(&"--bare".into()));
    }
}
