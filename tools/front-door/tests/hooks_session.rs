// Integration fixtures use real UTC timestamps in their filesystem records.
#![allow(clippy::disallowed_methods)]

//! Claude Code hooks that report and keep session state, and that route
//! worktree creation and removal through the worktree owner.
mod support;
use provenant_core::{CancelToken, Clock, ProjectId, RootInputs, WaitBudget};
use provenant_os::clock::SystemClock;
use provenant_workflow::session::{HostSessionId, STATE_BEGIN, SessionStore};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};
use support::{Result, TestRig, code};

const BINARY: &str = env!("CARGO_BIN_EXE_provenant");

fn binding(rig: &TestRig) -> Result<(provenant_core::Roots, ProjectId)> {
    fs::create_dir_all(&rig.state)?;
    fs::set_permissions(&rig.state, fs::Permissions::from_mode(0o700))?;
    let mut inputs = RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let common = fs::canonicalize(rig.project.join(".git"))?;
    let bound =
        provenant_config::bind_project(&roots, provenant_config::ProjectLocation::new(common)?)?;
    Ok((roots, bound.project_id()))
}
fn store(rig: &TestRig, session: &str) -> Result<SessionStore> {
    let (roots, project) = binding(rig)?;
    Ok(SessionStore::new(
        roots.state,
        project,
        HostSessionId::parse(session)?,
    ))
}
fn save(store: &SessionStore, text: &str) -> Result {
    let clock = SystemClock::new();
    store.save(
        text.as_bytes(),
        WaitBudget::cli(clock.mono_now(), 1)?,
        &CancelToken::new(),
        &clock,
    )?;
    Ok(())
}
fn run_hook(rig: &TestRig, name: &str, input: &Value) -> Result<provenant_os::exec::ExecOutcome> {
    rig.execute(
        Path::new(BINARY),
        &["hook", "claude", name],
        Some(&serde_json::to_vec(input)?),
    )
}
fn start(rig: &TestRig, session: &str, source: &str) -> Value {
    json!({"hook_event_name":"SessionStart","session_id":session,"cwd":rig.project,"source":source})
}
fn context(output: &provenant_os::exec::ExecOutcome) -> Result<String> {
    let value: Value = serde_json::from_slice(&output.stdout.bytes)?;
    Ok(value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .ok_or("additionalContext")?
        .to_owned())
}
fn checkpoint_text(store: &SessionStore) -> Result<String> {
    Ok(store.read()?.ok_or("checkpoint missing")?.markdown)
}
/// One queued lane in the project's run store.
fn create_lane(rig: &TestRig) -> Result<String> {
    use provenant_core::{
        ActorId, AttemptId, Digest, OperationId, RunId, TaskId, WaitBudget as Budget,
    };
    use provenant_records::CanonicalV1;
    use provenant_run as run;
    let (roots, project_id) = binding(rig)?;
    let context = run::ExecutionContext {
        state_root: roots.state,
        project_id,
        actor_id: ActorId::from_bytes([1; 16]),
        read_prompts: false,
    };
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = run::Request {
        clock: &clock,
        budget: Budget::cli(clock.mono_now(), 10)?,
        cancel: &cancel,
    };
    let store = run::RunStore::open(&context, &call)?;
    let (run_id, task_id, attempt_id) = (
        RunId::from_bytes([2; 16]),
        TaskId::from_bytes([3; 16]),
        AttemptId::from_bytes([4; 16]),
    );
    store.create(
        &run::PreparedRun {
            run_id,
            operation_id: OperationId::parse("hook-lane")?,
            request: run::CanonicalRequest {
                schema_version: 1,
                tasks: vec![run::TaskRequest {
                    base: None,
                    base_ref: None,
                    profile: Default::default(),
                    task_id,
                    prompt_digest: CanonicalV1::file_digest(b"existing lane"),
                    route: run::RequestedRoute::Named {
                        name: "utility".into(),
                        effort: None,
                    },
                    account: None,
                    fallback: vec![],
                    authority_digest: Digest::from_bytes([5; 32]),
                    writer: None,
                    deliver: None,
                    provider_timeout_seconds: 60,
                    delivery_id: None,
                    access: Default::default(),
                }],
                concurrency: 1,
                parent: None,
                lane_depth: Some(1),
                parent_attempt: None,
            },
            attempts: vec![run::NewAttempt {
                attempt_id,
                task_id,
                prompt: "existing lane".into(),
            }],
            created_at: clock.wall_now()?,
        },
        &call,
    )?;
    // Keep the writer connection alive until the lane is read.
    std::mem::forget(store);
    Ok(format!("{run_id}/{task_id}/{attempt_id}"))
}

#[test]
fn restart_hook_reports_in_flight_lanes_and_prs_named_by_the_chair() -> Result {
    let rig = TestRig::new()?;
    let lane = create_lane(&rig)?;
    let checkpoint = store(&rig, "restart-session")?;
    save(
        &checkpoint,
        "Chair notes: delivered PR #7; user approved #1829 only.",
    )?;
    let output = run_hook(
        &rig,
        "session-start",
        &start(&rig, "restart-session", "resume"),
    )?;
    assert_eq!(code(&output), 0, "{:?}", output.stderr.bytes);
    assert!(output.stdout.bytes.len() <= 8192);
    let text = context(&output)?;
    assert!(text.contains("Lanes in flight (1"), "{text}");
    assert!(text.contains(&lane), "{text}");
    assert!(text.contains("utility"), "{text}");
    // `pr status` may not exist yet, or may need GitHub; the PR is still named.
    assert!(text.contains("#7"), "{text}");
    assert!(text.contains("user approved #1829 only"), "{text}");
    Ok(())
}

#[test]
fn restart_hook_ignores_other_start_sources_and_bad_input() -> Result {
    let rig = TestRig::new()?;
    save(&store(&rig, "restart-session")?, "notes")?;
    for input in [
        start(&rig, "restart-session", "compact"),
        start(&rig, "restart-session", "other"),
        json!({"hook_event_name":"PreCompact","session_id":"restart-session","cwd":rig.project}),
        json!({"hook_event_name":"SessionStart","session_id":"BAD ID","cwd":rig.project,"source":"startup"}),
    ] {
        let output = run_hook(&rig, "session-start", &input)?;
        assert_eq!(code(&output), 0);
        assert!(output.stdout.bytes.is_empty(), "{input}");
    }
    Ok(())
}

#[test]
fn pre_compact_keeps_the_chair_text_and_replaces_only_its_own_block() -> Result {
    let rig = TestRig::new()?;
    let lane = create_lane(&rig)?;
    let checkpoint = store(&rig, "keep-session")?;
    let chair = "# Chair\nAuthority: user approved #1829 only. PR #9 is mine.";
    save(&checkpoint, chair)?;
    let input =
        json!({"hook_event_name":"PreCompact","session_id":"keep-session","cwd":rig.project});
    for _ in 0..2 {
        let output = run_hook(&rig, "pre-compact", &input)?;
        assert_eq!(code(&output), 0, "{:?}", output.stderr.bytes);
        let text = checkpoint_text(&checkpoint)?;
        assert!(text.starts_with(chair), "{text}");
        assert_eq!(text.matches(STATE_BEGIN).count(), 1, "{text}");
        assert!(text.contains(&lane), "{text}");
        assert!(text.contains("Chair-saved-unix: "), "{text}");
        assert!(String::from_utf8_lossy(&output.stderr.bytes).contains("auto-saved"));
    }
    // The post-compaction report shows the chair's text and a fresh digest,
    // not the saved block twice.
    let output = run_hook(&rig, "compact", &start(&rig, "keep-session", "compact"))?;
    let text = context(&output)?;
    assert!(text.contains("Checkpoint present"), "{text}");
    assert!(
        text.contains("Authority: user approved #1829 only"),
        "{text}"
    );
    assert!(text.contains("Lanes in flight (1"), "{text}");
    assert!(!text.contains(STATE_BEGIN), "{text}");
    assert!(output.stdout.bytes.len() <= 8192);
    // A chair save replaces the whole file, block included.
    save(&checkpoint, "fresh notes")?;
    assert!(!checkpoint_text(&checkpoint)?.contains(STATE_BEGIN));
    Ok(())
}

#[test]
fn pre_compact_without_a_checkpoint_saves_state_and_says_notes_are_missing() -> Result {
    let rig = TestRig::new()?;
    binding(&rig)?;
    let checkpoint = store(&rig, "new-session")?;
    let input =
        json!({"hook_event_name":"PreCompact","session_id":"new-session","cwd":rig.project});
    let output = run_hook(&rig, "pre-compact", &input)?;
    assert_eq!(code(&output), 0);
    assert!(checkpoint_text(&checkpoint)?.starts_with(STATE_BEGIN));
    let output = run_hook(&rig, "session-start", &start(&rig, "new-session", "resume"))?;
    let text = context(&output)?;
    assert!(text.contains("only automatically saved"), "{text}");
    assert!(
        text.contains("saving needed") || text.contains("still need saving"),
        "{text}"
    );
    Ok(())
}

#[test]
fn stop_hook_refreshes_only_an_existing_quiet_checkpoint() -> Result {
    let rig = TestRig::new()?;
    let checkpoint = store(&rig, "stop-session")?;
    let input = json!({"hook_event_name":"Stop","session_id":"stop-session","cwd":rig.project,"stop_hook_active":false});
    // No checkpoint: a session that never saved one does not get one.
    let output = run_hook(&rig, "stop", &input)?;
    assert_eq!(code(&output), 0);
    assert!(output.stdout.bytes.is_empty());
    assert!(checkpoint.read()?.is_none());
    // A fresh checkpoint is left alone.
    save(&checkpoint, "notes")?;
    run_hook(&rig, "stop", &input)?;
    assert_eq!(checkpoint_text(&checkpoint)?, "notes");
    // A quiet one gets the block appended after the chair's text.
    let (_, project) = binding(&rig)?;
    let file: PathBuf = rig.state.join(format!(
        "workflow/projects/{project}/sessions/stop-session/checkpoint.md"
    ));
    fs::File::options()
        .write(true)
        .open(&file)?
        .set_modified(SystemTime::now() - Duration::from_secs(20 * 60))?;
    let output = run_hook(&rig, "stop", &input)?;
    assert_eq!(code(&output), 0);
    assert!(output.stdout.bytes.is_empty());
    let text = checkpoint_text(&checkpoint)?;
    assert!(text.starts_with("notes\n\n"), "{text}");
    assert!(text.contains(STATE_BEGIN));
    Ok(())
}

#[test]
fn lane_sessions_report_but_never_write_checkpoint_state() -> Result {
    let mut rig = TestRig::new()?;
    rig.env
        .push((provenant_run::LANE_DEPTH_ENV.into(), "1".into()));
    let checkpoint = store(&rig, "lane-session")?;
    save(&checkpoint, "notes")?;
    let input =
        json!({"hook_event_name":"PreCompact","session_id":"lane-session","cwd":rig.project});
    let output = run_hook(&rig, "pre-compact", &input)?;
    assert_eq!(code(&output), 0);
    assert_eq!(checkpoint_text(&checkpoint)?, "notes");
    Ok(())
}

fn git(rig: &TestRig, args: &[&str]) -> Result<String> {
    let output = rig.execute(
        &provenant_os::git::git_executable().ok_or("Git unavailable")?,
        args,
        None,
    )?;
    assert_eq!(
        code(&output),
        0,
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    Ok(String::from_utf8(output.stdout.bytes)?.trim().into())
}
fn commit_file(rig: &TestRig) -> Result {
    fs::write(rig.project.join("file"), "one")?;
    git(rig, &["add", "file"])?;
    git(
        rig,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "base",
        ],
    )?;
    Ok(())
}
fn worktree_input(rig: &TestRig, name: &str) -> Value {
    json!({"hook_event_name":"WorktreeCreate","session_id":"wt-session","cwd":rig.project,"name":name})
}
fn removal_input(rig: &TestRig, path: &Path) -> Value {
    json!({"hook_event_name":"WorktreeRemove","session_id":"wt-session","cwd":rig.project,"worktree_path":path})
}

#[test]
fn worktree_hooks_create_and_remove_through_the_worktree_owner() -> Result {
    let rig = TestRig::new()?;
    commit_file(&rig)?;
    let output = run_hook(&rig, "worktree-create", &worktree_input(&rig, "bright-fox"))?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    let printed = String::from_utf8(output.stdout.bytes)?;
    // Only the path, then a newline, as the host reads it.
    assert_eq!(printed.lines().count(), 1, "{printed:?}");
    let path = PathBuf::from(printed.trim_end());
    assert!(path.is_absolute() && path.is_dir(), "{path:?}");
    assert!(path.ends_with(".worktrees/claude-bright-fox"), "{path:?}");
    // The owner lists it with the branch the hook chose.
    let listed = rig.command(&["--json", "worktree", "list"])?;
    let listed: Value = serde_json::from_slice(&listed.stdout.bytes)?;
    assert!(
        listed["data"]
            .as_array()
            .ok_or("list")?
            .iter()
            .any(|entry| entry["branch"] == "refs/heads/claude/bright-fox"),
        "{listed}"
    );
    // A reused name finds the same worktree rather than failing.
    let again = run_hook(&rig, "worktree-create", &worktree_input(&rig, "bright-fox"))?;
    assert_eq!(code(&again), 0);
    assert_eq!(
        String::from_utf8(again.stdout.bytes)?.trim_end(),
        printed.trim_end()
    );
    // Removal goes through the owner, which refuses dirty trees and keeps them.
    fs::write(path.join("scratch.txt"), "work")?;
    let refused = run_hook(&rig, "worktree-remove", &removal_input(&rig, &path))?;
    assert_eq!(code(&refused), 2);
    assert!(refused.stdout.bytes.is_empty());
    assert!(path.is_dir());
    fs::remove_file(path.join("scratch.txt"))?;
    let removed = run_hook(&rig, "worktree-remove", &removal_input(&rig, &path))?;
    assert_eq!(
        code(&removed),
        0,
        "{}",
        String::from_utf8_lossy(&removed.stderr.bytes)
    );
    assert!(!path.exists());
    // A path that is already gone is nothing to remove.
    let gone = run_hook(&rig, "worktree-remove", &removal_input(&rig, &path))?;
    assert_eq!(code(&gone), 0);
    Ok(())
}

#[test]
fn worktree_hooks_refuse_bad_names_the_primary_and_foreign_paths() -> Result {
    let rig = TestRig::new()?;
    commit_file(&rig)?;
    for name in ["", "../escape", "a/b", "-flag", "has space"] {
        let output = run_hook(&rig, "worktree-create", &worktree_input(&rig, name))?;
        assert_eq!(code(&output), 2, "{name:?}");
        assert!(output.stdout.bytes.is_empty(), "{name:?}");
    }
    assert!(!rig.project.join(".worktrees").exists());
    let wrong_event = json!({"hook_event_name":"WorktreeRemove","cwd":rig.project,"name":"x"});
    assert_eq!(code(&run_hook(&rig, "worktree-create", &wrong_event)?), 2);
    // The primary checkout and a directory the owner does not list stay put.
    let foreign = rig.root.join("foreign");
    fs::create_dir(&foreign)?;
    for path in [rig.project.clone(), foreign.clone()] {
        let output = run_hook(&rig, "worktree-remove", &removal_input(&rig, &path))?;
        assert_eq!(code(&output), 2, "{path:?}");
        assert!(path.is_dir());
    }
    Ok(())
}
