use super::*;

pub fn copilot(plan: &Plan) -> AdapterProfile {
    let mut argv = vec![
        "-p".into(),
        plan.prompt.clone(),
        "--output-format".into(),
        "json".into(),
        "--no-ask-user".into(),
    ];
    if plan.mode.is_writer() {
        argv.push("--allow-all".into());
    } else {
        argv.extend([
            "--mode".into(),
            "plan".into(),
            "--disable-builtin-mcps".into(),
            "--available-tools=".into(),
            "--disallow-temp-dir".into(),
        ]);
    }
    if let Some(model) = &plan.model {
        argv.extend(["--model".into(), model.clone()]);
    }
    if let Some(effort) = &plan.effort {
        argv.extend(["--effort".into(), effort.clone()]);
    }
    profile(
        "copilot",
        argv,
        Metadata {
            transport: PromptTransport::Argv,
            stdin: StdinMode::Null,
            output: "json",
            effort_flag: Some("--effort"),
            session_keys: &["session_id"],
            model_source: None,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::agy::tests::plan;

    #[test]
    fn copilot_read_only_uses_plan_mode() {
        assert!(copilot(&plan(Mode::ReadOnly))
            .argv
            .windows(2)
            .any(|v| v == ["--mode", "plan"]));
    }
}
