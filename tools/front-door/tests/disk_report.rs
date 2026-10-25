//! Production command transport and the no-create contract.
mod support;
use support::{TestRig, code};

#[test]
fn disk_report_json_and_human_output_leave_missing_state_absent() -> support::Result {
    let rig = TestRig::new()?;
    let git = provenant_supervisor::git::git_executable().ok_or("Git unavailable")?;
    let commit = rig.execute(
        &git,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "initial",
        ],
        None,
    )?;
    assert_eq!(code(&commit), 0);
    let before = std::fs::read(rig.project.join(".git/index")).ok();
    let json = rig.command(&["disk", "report", "--json"])?;
    assert_eq!(
        code(&json),
        0,
        "{}",
        String::from_utf8_lossy(&json.stderr.bytes)
    );
    let value: serde_json::Value = serde_json::from_slice(&json.stdout.bytes)?;
    assert_eq!(value["command"], "disk report");
    assert_eq!(value["status"], "ok");
    assert_eq!(value["data"]["schema_version"], 1);
    assert_eq!(value["data"]["errors"], serde_json::json!([]));
    assert_eq!(value["data"]["totals"]["worktree"]["count"], 1);
    assert!(!rig.state.exists());
    let human = rig.command(&["disk", "report"])?;
    assert_eq!(code(&human), 0);
    assert!(String::from_utf8(human.stdout.bytes)?.contains("class=worktree"));
    assert!(!rig.state.exists());
    assert_eq!(std::fs::read(rig.project.join(".git/index")).ok(), before);
    rig.assert_legacy_unchanged()?;
    Ok(())
}
