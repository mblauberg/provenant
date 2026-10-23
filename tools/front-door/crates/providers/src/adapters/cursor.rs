use super::*;

pub fn cursor(plan: &Plan) -> Result<AdapterProfile, BuildError> {
    let mut argv = vec![
        "-p".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--trust".into(),
        "--approve-mcps".into(),
    ];
    if plan.mode.is_writer() {
        argv.push("--force".into());
    } else {
        argv.extend([
            "--mode".into(),
            "ask".into(),
            "--sandbox".into(),
            "enabled".into(),
        ]);
    }
    if let Some(session) = plan
        .resume_session
        .as_ref()
        .filter(|value| !value.is_empty())
    {
        argv.extend(["--resume".into(), session.clone()]);
    }
    if let Some(model) = plan.model.as_ref().filter(|value| !value.is_empty()) {
        argv.extend(["--model".into(), model.clone()]);
    }
    argv.push(plan.prompt.clone());
    profile("cursor", argv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::agy::tests::plan;

    #[test]
    fn cursor_read_only_uses_ask_sandbox_and_open_pipe() {
        assert!(
            cursor(&plan(Mode::ReadOnly))
                .unwrap()
                .argv
                .windows(2)
                .any(|v| v == ["--sandbox", "enabled"])
        );
        assert_eq!(
            cursor(&plan(Mode::ReadOnly)).unwrap().stdin,
            StdinMode::Pipe
        );
    }
}
