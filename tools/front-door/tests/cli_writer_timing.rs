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
#[allow(clippy::disallowed_methods)] // The external CLI fixture needs a real watchdog.
fn run_wait_admits_after_registry_lock_is_released() -> Result {
    use provenant_core::RootInputs;
    use provenant_fabric_runtime::graph::FileJournal;
    use std::time::{Duration, Instant};

    let executor = provenant_os::exec::SystemExecutor::new(1)?;
    let cancel = CancelToken::new();
    if let Err(error) = provenant_confinement::permissions::system_backend(
        &provenant_confinement::permissions::BackendContext {
            executor: &executor,
            cancel: &cancel,
            #[cfg(target_os = "macos")]
            env: &[],
        },
    ) {
        provenant_test_support::skip_sandbox_dependent_test(format!("writer fixture: {error}"))?;
        return Ok(());
    }
    let rig = TestRig::new()?;
    let _cleanup = rig.process_cleanup();
    let git = provenant_os::git::git_executable().ok_or("git")?;
    assert_eq!(
        code(&rig.execute(&git, &["commit", "--allow-empty", "-qm", "base"], None)?),
        0
    );
    assert_eq!(
        code(&rig.command(&["worktree", "create", "released", "--base", "HEAD", "--json"])?),
        0
    );
    let identity = rig.command(&["fabric", "identity", "--json"])?;
    let identity: Value = serde_json::from_slice(&identity.stdout.bytes)?;
    let project: provenant_core::ProjectId = identity["data"]["project"]
        .as_str()
        .ok_or("project")?
        .parse()?;
    let mut roots = RootInputs::new(rig.home.clone(), rig.project.clone());
    roots.state_dir = Some(rig.state.clone());
    let roots = roots.resolve(&provenant_os::fs::SystemPathResolver)?;
    let registry = rig.state.join("worktrees").join(project.to_string());
    let directory = Directory::open_anchor(&registry)?;
    let clock = SystemClock::new();
    let guard = directory.lock(
        std::ffi::OsStr::new("worktrees.lock"),
        LockMode::Exclusive,
        WaitBudget::cli(clock.mono_now(), 10)?,
        &cancel,
        &clock,
    )?;
    let document = rig.root.join("released.json");
    std::fs::write(
        &document,
        serde_json::to_vec(&json!({
            "schema_version":2,"graph_id":"released-lock","nodes":[{
                "id":"writer","prompt":"write code","access":"worktree_write",
                "worktree":rig.project.join(".worktrees/released"),"write_scope":["."],
                "route":{"adapter":"codex","model":"gpt-6-luna"}
            }]
        }))?,
    )?;
    let path = document.to_str().ok_or("document")?;
    std::thread::scope(|scope| -> Result {
        let worker = scope.spawn(|| {
            rig.command(&["run", "--file", path, "--wait=30s", "--json"])
                .map_err(|error| error.to_string())
        });
        let deadline = Instant::now() + Duration::from_secs(20);
        let observed = loop {
            if let Ok(Some(checkpoint)) =
                FileJournal::read_checkpoint(&roots.state, project, "released-lock")
                && checkpoint.nodes[0]
                    .launch_retry
                    .as_ref()
                    .is_some_and(|retry| retry.failures > 0)
            {
                break true;
            }
            if worker.is_finished() || Instant::now() >= deadline {
                break false;
            }
            std::thread::yield_now();
        };
        drop(guard);
        let output = worker
            .join()
            .map_err(|_| "CLI thread panicked")?
            .map_err(|error| format!("CLI: {error}"))?;
        assert!(observed, "CLI never recorded registry contention");
        let value: Value = serde_json::from_slice(&output.stdout.bytes)?;
        let node = &value["data"]["checkpoint"]["nodes"][0];
        assert_ne!(node["state"], "blocked", "{value}");
        let attempts = node["attempts"].as_array().ok_or("attempts")?;
        assert_eq!(attempts.len(), 1, "{value}");
        assert!(attempts[0]["attempt_ref"].is_string(), "{value}");
        Ok(())
    })
}

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
#[allow(clippy::disallowed_methods)] // Measure the real CLI process against its wait budget.
fn run_wait_refuses_a_held_writer_without_retry() -> Result {
    use std::time::{Duration, Instant};
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
    let started = Instant::now();
    let output = rig.command(&[
        "run",
        "--file",
        document.to_str().ok_or("document")?,
        "--wait=30s",
        "--json",
    ])?;
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "live writer refusal consumed the wait budget"
    );
    let value: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_ne!(code(&output), 0, "{value}");
    let node = &value["data"]["checkpoint"]["nodes"][0];
    assert_eq!(node["state"], "blocked", "{value}");
    assert_eq!(node["advance_error_code"], "launch_refused", "{value}");
    assert!(node["launch_retry"].is_null(), "{value}");
    assert_eq!(node["attempts"].as_array().ok_or("attempts")?.len(), 1);
    Ok(())
}
