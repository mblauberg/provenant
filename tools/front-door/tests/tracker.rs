mod support;
use support::{Result, TestRig, code};

#[test]
fn tracker_lint_warns_and_exits_successfully() -> Result {
    let rig = TestRig::new()?;
    let issue = rig.project.join("issue.md");
    std::fs::write(
        &issue,
        "Outcome: export.\nAcceptance: exports records.\nVerify: test.\nSee missing.rs#Gone",
    )?;
    let output = rig.command(&["tracker", "lint", "--file", "issue.md"])?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    let stdout = String::from_utf8(output.stdout.bytes)?;
    assert!(stdout.contains("missing.rs#Gone"), "{stdout}");
    assert!(!stdout.contains("skipped"), "{stdout}");
    Ok(())
}
