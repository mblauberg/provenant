use super::*;

pub fn copilot(plan: &Plan) -> Result<AdapterProfile, BuildError> {
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
    if let Some(model) = plan.model.as_ref().filter(|value| !value.is_empty()) {
        argv.extend(["--model".into(), model.clone()]);
    }
    if let Some(effort) = plan.effort.as_ref().filter(|value| !value.is_empty()) {
        argv.extend(["--effort".into(), effort.clone()]);
    }
    profile("copilot", argv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::agy::tests::plan;

    #[test]
    fn copilot_read_only_uses_plan_mode() {
        assert!(copilot(&plan(Mode::ReadOnly))
            .unwrap()
            .argv
            .windows(2)
            .any(|v| v == ["--mode", "plan"]));
    }
}
