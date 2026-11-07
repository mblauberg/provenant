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
    let guard = directory.lock(
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
    let node = &value["data"]["checkpoint"]["nodes"][0];
    assert_eq!(node["state"], "blocked", "{value}");
    assert_eq!(
        node["advance_error_code"], "launch_retry_exhausted",
        "{value}"
    );
    assert_eq!(node["attempts"].as_array().ok_or("attempts")?.len(), 1);
    drop(guard);
    let replay = rig.command(&[
        "run",
        "--file",
        document.to_str().ok_or("document")?,
        "--json",
    ])?;
    let replay: Value = serde_json::from_slice(&replay.stdout.bytes)?;
    assert_eq!(
        replay["data"]["checkpoint"]["nodes"][0]["state"], "blocked",
        "{replay}"
    );
    assert_eq!(
        replay["data"]["checkpoint"]["nodes"][0]["attempts"],
        node["attempts"]
    );
    Ok(())
}

#[test]
fn run_wait_refuses_a_held_writer_without_retry() -> Result {
    let rig = TestRig::new()?;
    let git = provenant_os::git::git_executable().ok_or("git")?;
    assert_eq!(
        code(&rig.execute(&git, &["commit", "--allow-empty", "-qm", "base"], None)?),
        0
    );
    assert_eq!(
        code(&rig.command(&["worktree", "create", "occupied", "--base", "HEAD", "--json"])?),
        0
    );
    let identity = rig.command(&["fabric", "identity", "--json"])?;
    let identity: Value = serde_json::from_slice(&identity.stdout.bytes)?;
    let project = identity["data"]["project"].as_str().ok_or("project")?;
    let worktree = std::fs::canonicalize(rig.project.join(".worktrees/occupied"))?;
    let slot = rig
        .state
        .join("worktrees")
        .join(project)
        .join(provenant_confinement::worktrees::state_dir_name(&worktree));
    let directory = Directory::open_anchor(&slot)?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let _writer = directory.lock(
        std::ffi::OsStr::new("writer.lock"),
        LockMode::Exclusive,
        WaitBudget::cli(clock.mono_now(), 10)?,
        &cancel,
        &clock,
    )?;
    let document = rig.root.join("occupied.json");
    std::fs::write(
        &document,
        serde_json::to_vec(&json!({
            "schema_version":2,"graph_id":"occupied-writer","nodes":[{
                "id":"writer","prompt":"write code","access":"worktree_write",
                "worktree":worktree,"write_scope":["."],
                "route":{"adapter":"codex","model":"gpt-6-luna"}
            }]
        }))?,
    )?;
    let output = rig.command(&[
        "run",
        "--file",
        document.to_str().ok_or("document")?,
        "--wait=10s",
        "--json",
    ])?;
    let value: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_ne!(code(&output), 0, "{value}");
    let node = &value["data"]["checkpoint"]["nodes"][0];
    assert_eq!(node["state"], "blocked", "{value}");
    assert_eq!(node["advance_error_code"], "launch_refused", "{value}");
    assert!(node["launch_retry"].is_null(), "{value}");
    assert_eq!(node["attempts"].as_array().ok_or("attempts")?.len(), 1);
    Ok(())
}
