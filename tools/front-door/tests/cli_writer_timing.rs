//! CLI admission while the worktree registry is contended.
mod support;

use provenant_core::{CancelToken, Clock, WaitBudget};
use provenant_os::{
    clock::SystemClock,
    fs::{Directory, LockMode},
};
use serde_json::{Value, json};
use support::{Result, TestRig, code};

#[test]
#[ignore = "held registry lock still returns successful waiting after --wait expires"]
fn run_wait_reports_the_held_registry_lock() -> Result {
    let rig = TestRig::new()?;
    let git = provenant_os::git::git_executable().ok_or("git")?;
    let commit = rig.execute(&git, &["commit", "--allow-empty", "-qm", "base"], None)?;
    assert_eq!(code(&commit), 0, "{commit:?}");
    let made = rig.command(&["worktree", "create", "timing", "--base", "HEAD", "--json"])?;
    assert_eq!(code(&made), 0, "{made:?}");
    let identity = rig.command(&["fabric", "identity", "--json"])?;
    let identity: Value = serde_json::from_slice(&identity.stdout.bytes)?;
    let project = identity["data"]["project"].as_str().ok_or("project")?;
    let registry = rig.state.join("worktrees").join(project);
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 10)?;
    let directory = Directory::open_anchor(&registry)?;
    let _guard = directory.lock(
        std::ffi::OsStr::new("worktrees.lock"),
        LockMode::Exclusive,
        budget,
        &cancel,
        &clock,
    )?;
    let document = rig.root.join("timing.json");
    std::fs::write(
        &document,
        serde_json::to_vec(&json!({
            "schema_version":2, "graph_id":"cli-lock-timing", "nodes":[{
                "id":"writer", "prompt":"write code", "access":"worktree_write",
                "worktree":rig.project.join(".worktrees/timing"), "write_scope":["."],
                "route":{"adapter":"codex","model":"gpt-6-luna"}
            }]
        }))?,
    )?;
    let output = rig.command(&[
        "run",
        "--file",
        document.to_str().ok_or("document")?,
        "--wait=1s",
        "--json",
    ])?;
    let value: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_ne!(code(&output), 0, "{value}");
    let wire = serde_json::to_string(&value)?;
    assert!(wire.contains("writer_lock_busy"), "{value}");
    assert!(wire.contains("writer lock"), "{value}");
    Ok(())
}
