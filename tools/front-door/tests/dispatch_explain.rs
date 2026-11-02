mod support;

use std::path::Path;
use support::{Result, TestRig, code};

#[test]
fn explain_for_model_and_route_previews_without_creating_attempts() -> Result {
    for (selector, expected) in [("--model", "codex/gpt-6-luna"), ("--route", "junior")] {
        let rig = TestRig::new()?;
        let prompt = rig.root.join("prompt.md");
        std::fs::write(&prompt, "Preview this task")?;
        let output = rig.execute(
            Path::new(env!("CARGO_BIN_EXE_provenant")),
            &[
                "run",
                selector,
                expected,
                "--prompt-file",
                prompt.to_str().ok_or("prompt path")?,
                "--mode",
                "read-only",
                "--deliver",
                "none",
                "--explain",
            ],
            None,
        )?;

        assert_eq!(
            code(&output),
            0,
            "{}",
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        let text = String::from_utf8(output.stdout.bytes)?;
        assert!(text.contains(expected), "{text}");
        assert!(
            !rig.state.join("runs").exists(),
            "explain created run state for {selector}: {text}"
        );
    }
    Ok(())
}
