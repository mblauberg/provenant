// Integration fixtures use real UTC timestamps in their filesystem records.
#![allow(clippy::disallowed_methods)]

//! Claude Code hooks that report and keep session state, and that route
//! worktree creation and removal through the worktree owner.
mod support;
use provenant_core::{CancelToken, Clock, ProjectId, RootInputs, WaitBudget};
use provenant_os::clock::SystemClock;
use provenant_workflow::session::{HostSessionId, SessionStore};
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
    use provenant_core::{ActorId, AttemptId, Digest, OperationId, RunId, TaskId};
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
                extra: Default::default(),
                child_attempt_id_version: None,
                chair: None,
                fresh_continuation: None,
                schema_version: 1,
                tasks: vec![run::TaskRequest {
                    context: None,
                    extra: Default::default(),
                    build_heavy: false,
                    title: None,
                    base: None,
                    head: None,
                    base_ref: None,
                    profile: Default::default(),
                    task_id,
                    prompt_digest: CanonicalV1::file_digest(b"existing lane"),
                    route: run::RequestedRoute::Named {
                        extra: Default::default(),
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
fn session_start_warns_once_only_when_disk_is_below_headroom() -> Result {
    let mut rig = TestRig::new()?;
    binding(&rig)?;
    for source in ["startup", "compact"] {
        let hook = if source == "compact" {
            "compact"
        } else {
            "session-start"
        };
        let output = run_hook(&rig, hook, &start(&rig, "disk-session", source))?;
        assert_eq!(code(&output), 0);
        assert!(!context(&output)?.contains("Disk pressure:"));
    }
    fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[gc]\nfree_floor='18446744073709551615B'\n",
    )?;
    let (free, total) = provenant_os::fs::filesystem_bytes_at(&rig.state)?;
    let pressure = free < total / 2;
    for source in ["startup", "compact"] {
        let hook = if source == "compact" {
            "compact"
        } else {
            "session-start"
        };
        let output = run_hook(&rig, hook, &start(&rig, "disk-session", source))?;
        assert_eq!(code(&output), 0);
        let text = context(&output)?;
        let lines: Vec<_> = text
            .lines()
            .filter(|line| line.starts_with("Disk pressure:"))
            .collect();
        assert_eq!(lines.len(), usize::from(pressure), "{text}");
        if let Some(line) = lines.first() {
            assert!(line.contains("provenant disk report"));
            assert!(line.contains("provenant gc"));
            assert!(line.len() <= 160);
        }
    }
    for (hook, event) in [("pre-compact", "PreCompact"), ("stop", "Stop")] {
        let input = json!({"hook_event_name":event,"session_id":"disk-session","cwd":rig.project});
        let output = run_hook(&rig, hook, &input)?;
        assert_eq!(code(&output), 0);
        assert!(!String::from_utf8_lossy(&output.stdout.bytes).contains("Disk pressure:"));
        assert!(!String::from_utf8_lossy(&output.stderr.bytes).contains("Disk pressure:"));
    }
    rig.env.push(("PROVENANT_LANE_DEPTH".into(), "1".into()));
    for (hook, source) in [("session-start", "startup"), ("compact", "compact")] {
        let output = run_hook(&rig, hook, &start(&rig, "disk-session", source))?;
        assert_eq!(code(&output), 0);
        assert!(!context(&output)?.contains("Disk pressure:"));
        assert!(!String::from_utf8_lossy(&output.stderr.bytes).contains("Disk pressure:"));
    }
    Ok(())
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
fn hints_from_a_hook_on_a_custom_store_carry_its_state_dir() -> Result {
    let rig = TestRig::new()?;
    save(
        &store(&rig, "custom-session")?,
        "Chair notes: delivered PR #7.",
    )?;
    let state = rig.state.to_str().ok_or("state path is not UTF-8")?;
    let input = serde_json::to_vec(&start(&rig, "custom-session", "resume"))?;
    let output = rig.execute(
        Path::new(BINARY),
        &["--state-dir", state, "hook", "claude", "session-start"],
        Some(&input),
    )?;
    assert_eq!(code(&output), 0, "{:?}", output.stderr.bytes);
    assert!(output.stdout.bytes.len() <= 8192);
    let text = context(&output)?;
    let prefix = format!("provenant --state-dir '{state}'");
    assert!(
        text.contains(&format!(
            "Save: {prefix} workflow session save custom-session --file <checkpoint.md> --client claude"
        )),
        "{text}"
    );
    assert!(text.contains(&format!("`{prefix} lanes`")), "{text}");
    // No command the hook prints may name the default store.
    for line in text.lines().filter(|line| line.contains("provenant ")) {
        assert!(line.contains("--state-dir"), "{line}");
    }
    Ok(())
}

#[test]
fn a_damaged_autosave_leaves_the_chair_notes_visible_at_session_start() -> Result {
    let rig = TestRig::new()?;
    save(
        &store(&rig, "damaged-session")?,
        "Chair notes: user approved #1829 only.",
    )?;
    let directory = checkpoint_file(&rig, "damaged-session")?
        .parent()
        .map(Path::to_path_buf)
        .ok_or("session directory")?;
    fs::write(directory.join("autosave.md"), [0xff, 0xfe])?;
    let output = run_hook(
        &rig,
        "session-start",
        &start(&rig, "damaged-session", "resume"),
    )?;
    assert_eq!(code(&output), 0, "{:?}", output.stderr.bytes);
    assert!(output.stdout.bytes.len() <= 8192);
    let text = context(&output)?;
    assert!(text.contains("user approved #1829 only"), "{text}");
    assert!(text.contains("\nAutosave unavailable: "), "{text}");
    assert!(text.contains("Checkpoint present"), "{text}");
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

fn autosave_text(store: &SessionStore) -> Result<String> {
    Ok(store.read_autosave()?.ok_or("autosave missing")?.markdown)
}
fn checkpoint_file(rig: &TestRig, session: &str) -> Result<PathBuf> {
    let (_, project) = binding(rig)?;
    Ok(rig.state.join(format!(
        "workflow/projects/{project}/sessions/{session}/checkpoint.md"
    )))
}

#[test]
fn autosave_never_writes_the_chair_file_and_output_shows_both_parts() -> Result {
    let rig = TestRig::new()?;
    let lane = create_lane(&rig)?;
    let checkpoint = store(&rig, "keep-session")?;
    let chair = "# Chair\nAuthority: user approved #1829 only. PR #9 is mine.\n\n\n";
    save(&checkpoint, chair)?;
    let file = checkpoint_file(&rig, "keep-session")?;
    let before = (fs::read(&file)?, fs::metadata(&file)?.modified()?);
    let input =
        json!({"hook_event_name":"PreCompact","session_id":"keep-session","cwd":rig.project});
    for _ in 0..2 {
        let output = run_hook(&rig, "pre-compact", &input)?;
        assert_eq!(code(&output), 0, "{:?}", output.stderr.bytes);
        assert!(String::from_utf8_lossy(&output.stderr.bytes).contains("auto-saved"));
        // The chair's file is byte-identical and was never rewritten.
        assert_eq!(fs::read(&file)?, before.0);
        assert_eq!(fs::metadata(&file)?.modified()?, before.1);
        assert_eq!(checkpoint_text(&checkpoint)?, chair);
        let saved = autosave_text(&checkpoint)?;
        assert!(
            saved.contains(&lane) && saved.contains("Auto-saved: "),
            "{saved}"
        );
    }
    // A chair who copied the saved state into their notes keeps it, too.
    let copied = format!("{chair}copy:\n{}", autosave_text(&checkpoint)?);
    save(&checkpoint, &copied)?;
    run_hook(&rig, "pre-compact", &input)?;
    assert_eq!(checkpoint_text(&checkpoint)?, copied);
    // The report shows the chair's text, then the saved state, then it is
    // bounded as before.
    let output = run_hook(&rig, "compact", &start(&rig, "keep-session", "compact"))?;
    let text = context(&output)?;
    assert!(text.contains("Checkpoint present"), "{text}");
    let notes = text
        .find("Authority: user approved #1829 only")
        .ok_or("notes")?;
    let state = text
        .rfind("## Provenant session state")
        .ok_or("state file")?;
    assert!(notes < state, "{text}");
    assert!(text.contains("Lanes in flight (1"), "{text}");
    assert!(output.stdout.bytes.len() <= 8192);
    // A chair save replaces only the chair's file; the autosave file stays.
    save(&checkpoint, "fresh notes")?;
    assert_eq!(checkpoint_text(&checkpoint)?, "fresh notes");
    assert!(autosave_text(&checkpoint)?.contains(&lane));
    Ok(())
}

#[test]
fn a_legacy_block_in_the_checkpoint_stays_chair_text() -> Result {
    let rig = TestRig::new()?;
    binding(&rig)?;
    let checkpoint = store(&rig, "legacy-session")?;
    let legacy = "notes\n\n<!-- provenant:session-state:begin -->\nBlock-digest: abc\n## Provenant session state\nAuto-saved: old\n<!-- provenant:session-state:end -->\n";
    save(&checkpoint, legacy)?;
    let input =
        json!({"hook_event_name":"PreCompact","session_id":"legacy-session","cwd":rig.project});
    assert_eq!(code(&run_hook(&rig, "pre-compact", &input)?), 0);
    assert_eq!(checkpoint_text(&checkpoint)?, legacy);
    assert!(autosave_text(&checkpoint)?.contains("Auto-saved: "));
    assert!(!autosave_text(&checkpoint)?.contains("Auto-saved: old"));
    let output = run_hook(&rig, "compact", &start(&rig, "legacy-session", "compact"))?;
    let text = context(&output)?;
    assert!(text.contains("Auto-saved: old"), "{text}");
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
    // Only the hook's file exists; the chair has no checkpoint.
    assert!(checkpoint.read()?.is_none());
    assert!(autosave_text(&checkpoint)?.starts_with("## Provenant session state"));
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
    assert!(checkpoint.read()?.is_none() && checkpoint.read_autosave()?.is_none());
    // A fresh checkpoint is left alone.
    save(&checkpoint, "notes")?;
    run_hook(&rig, "stop", &input)?;
    assert_eq!(checkpoint_text(&checkpoint)?, "notes");
    assert!(checkpoint.read_autosave()?.is_none());
    // A quiet one gets the state saved beside it, and the chair's file stays.
    let file = checkpoint_file(&rig, "stop-session")?;
    fs::File::options()
        .write(true)
        .open(&file)?
        .set_modified(SystemTime::now() - Duration::from_secs(20 * 60))?;
    let modified = fs::metadata(&file)?.modified()?;
    let output = run_hook(&rig, "stop", &input)?;
    assert_eq!(code(&output), 0);
    assert!(output.stdout.bytes.is_empty());
    assert_eq!(checkpoint_text(&checkpoint)?, "notes");
    assert_eq!(fs::metadata(&file)?.modified()?, modified);
    assert!(autosave_text(&checkpoint)?.contains("Auto-saved: "));
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
    assert!(checkpoint.read_autosave()?.is_none());
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
fn worktree_remove_distinguishes_missing_paths_from_other_errors() -> Result {
    let rig = TestRig::new()?;
    let missing = rig.project.join("missing");
    let gone = run_hook(&rig, "worktree-remove", &removal_input(&rig, &missing))?;
    assert_eq!(code(&gone), 0);

    let parent = rig.project.join("private-name");
    fs::write(&parent, "file")?;
    let output = run_hook(
        &rig,
        "worktree-remove",
        &removal_input(&rig, &parent.join("child")),
    )?;
    assert_eq!(code(&output), 2);
    let stderr = String::from_utf8(output.stderr.bytes)?;
    assert!(stderr.contains("NotADirectory"), "{stderr}");
    assert!(!stderr.contains("private-name"), "{stderr}");
    Ok(())
}

#[test]
fn worktree_hook_refuses_a_colliding_branch() -> Result {
    let rig = TestRig::new()?;
    commit_file(&rig)?;
    git(&rig, &["branch", "claude/Bright-fox"])?;
    let output = run_hook(&rig, "worktree-create", &worktree_input(&rig, "bright-fox"))?;
    assert_eq!(code(&output), 2);
    let stderr = String::from_utf8(output.stderr.bytes)?;
    assert!(stderr.contains("collides"), "{stderr}");
    assert!(!rig.project.join(".worktrees/claude-bright-fox").exists());
    Ok(())
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

fn made_tree(rig: &TestRig, name: &str) -> Result<PathBuf> {
    let made = run_hook(rig, "worktree-create", &worktree_input(rig, name))?;
    assert_eq!(
        code(&made),
        0,
        "{}",
        String::from_utf8_lossy(&made.stderr.bytes)
    );
    Ok(PathBuf::from(
        String::from_utf8(made.stdout.bytes)?.trim_end(),
    ))
}

#[test]
fn worktree_remove_keeps_a_listed_clean_tree_the_hook_did_not_make() -> Result {
    let rig = TestRig::new()?;
    commit_file(&rig)?;
    let path = made_tree(&rig, "mine")?;
    let record = state_record(&rig)?;
    // Same tree, but with no state record: it reads as a person's own.
    fs::remove_file(&record)?;
    let output = run_hook(&rig, "worktree-remove", &removal_input(&rig, &path))?;
    assert_eq!(code(&output), 2);
    let stderr = String::from_utf8_lossy(&output.stderr.bytes);
    assert!(stderr.contains("was not created by this hook"), "{stderr}");
    assert!(path.is_dir());
    // A tree made by hand and listed by the owner is kept too.
    let hand = rig.project.join(".worktrees").join("by-hand");
    let hand = hand.to_str().ok_or("path")?;
    git(&rig, &["worktree", "add", "-q", "-b", "by-hand", hand])?;
    let output = run_hook(
        &rig,
        "worktree-remove",
        &removal_input(&rig, Path::new(hand)),
    )?;
    assert_eq!(code(&output), 2);
    assert!(Path::new(hand).is_dir());
    Ok(())
}

#[test]
fn worktree_remove_accepts_an_owned_tree_after_a_commit() -> Result {
    let rig = TestRig::new()?;
    commit_file(&rig)?;
    let path = made_tree(&rig, "moved")?;
    let path_str = path.to_str().ok_or("path")?;
    git(
        &rig,
        &[
            "-C",
            path_str,
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "more",
        ],
    )?;
    let output = run_hook(&rig, "worktree-remove", &removal_input(&rig, &path))?;
    // The owner may keep an unmerged branch, but the hook does not refuse it.
    let stderr = String::from_utf8_lossy(&output.stderr.bytes);
    assert!(!stderr.contains("was not created by this hook"), "{stderr}");
    Ok(())
}

fn state_record(rig: &TestRig) -> Result<PathBuf> {
    let dir = rig.state.join("hooks").join("claude-worktrees");
    let mut records: Vec<_> = fs::read_dir(&dir)?
        .filter_map(std::result::Result::ok)
        .collect();
    assert_eq!(records.len(), 1, "{dir:?}");
    Ok(records.remove(0).path())
}

#[test]
fn ownership_is_specific_to_one_creation_and_ends_with_the_tree() -> Result {
    let rig = TestRig::new()?;
    commit_file(&rig)?;
    // Happy path: the record exists in state and in the admin directory, and
    // the same name reuses the tree; removal by the hook deletes the record.
    let path = made_tree(&rig, "owned")?;
    let record = state_record(&rig)?;
    assert_eq!(
        code(&run_hook(
            &rig,
            "worktree-create",
            &worktree_input(&rig, "owned")
        )?),
        0
    );
    let removed = run_hook(&rig, "worktree-remove", &removal_input(&rig, &path))?;
    assert_eq!(code(&removed), 0);
    assert!(!record.exists(), "state record outlived the tree");
    // A tree recreated by hand at the same path and branch is not adopted,
    // though the state record of the first creation is still on disk.
    let path = made_tree(&rig, "stale")?;
    let stale = state_record(&rig)?;
    git(
        &rig,
        &["worktree", "remove", "--force", ".worktrees/claude-stale"],
    )?;
    assert!(stale.is_file());
    git(
        &rig,
        &[
            "worktree",
            "add",
            "-q",
            ".worktrees/claude-stale",
            "claude/stale",
        ],
    )?;
    let stderr = refused_create(&rig, "stale")?;
    assert!(stderr.contains("was not created by this hook"), "{stderr}");
    assert!(path.is_dir());
    fs::remove_file(&stale)?;
    // A record whose token differs from the tree's marker is refused.
    let path = made_tree(&rig, "token")?;
    let record = state_record(&rig)?;
    let mut value: Value = serde_json::from_slice(&fs::read(&record)?)?;
    value["token"] = json!("00000000000000000000000000000000");
    fs::write(&record, serde_json::to_vec(&value)?)?;
    let stderr = refused_create(&rig, "token")?;
    assert!(stderr.contains("was not created by this hook"), "{stderr}");
    // A tree whose HEAD has moved since the hook made it is not adopted.
    fs::remove_file(&record)?;
    fs::remove_dir_all(rig.state.join("hooks"))?;
    let _ = path;
    let moved = made_tree(&rig, "moved")?;
    git(
        &rig,
        &[
            "-C",
            moved.to_str().ok_or("path")?,
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "x",
        ],
    )?;
    let stderr = refused_create(&rig, "moved")?;
    assert!(stderr.contains("was not created by this hook"), "{stderr}");
    Ok(())
}

fn refused_create(rig: &TestRig, name: &str) -> Result<String> {
    let output = run_hook(rig, "worktree-create", &worktree_input(rig, name))?;
    assert_eq!(code(&output), 2, "{name}");
    assert!(output.stdout.bytes.is_empty(), "{name}");
    Ok(String::from_utf8(output.stderr.bytes)?)
}

#[test]
fn worktree_create_never_reuses_primary_unowned_misplaced_or_dirty_trees() -> Result {
    let rig = TestRig::new()?;
    commit_file(&rig)?;
    // The chair's own checkout holds the branch.
    git(&rig, &["checkout", "-q", "-b", "claude/primary-held"])?;
    let stderr = refused_create(&rig, "primary-held")?;
    assert!(stderr.contains("primary checkout"), "{stderr}");
    git(&rig, &["checkout", "-q", "-"])?;
    // A tree at the hook's exact path that the owner never registered.
    git(
        &rig,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "claude/unowned",
            ".worktrees/claude-unowned",
        ],
    )?;
    let stderr = refused_create(&rig, "unowned")?;
    assert!(stderr.contains("was not created by this hook"), "{stderr}");
    // A tree on the branch somewhere the hook would not have put it.
    let elsewhere = rig.root.join("elsewhere");
    git(
        &rig,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "claude/misplaced",
            elsewhere.to_str().ok_or("path")?,
        ],
    )?;
    let stderr = refused_create(&rig, "misplaced")?;
    assert!(stderr.contains("not at the path"), "{stderr}");
    // The hook's own tree is reused when clean and refused once it has work.
    let made = run_hook(&rig, "worktree-create", &worktree_input(&rig, "mine"))?;
    assert_eq!(code(&made), 0);
    let path = PathBuf::from(String::from_utf8(made.stdout.bytes)?.trim_end());
    fs::write(path.join("scratch.txt"), "work")?;
    let stderr = refused_create(&rig, "mine")?;
    assert!(stderr.contains("uncommitted or ignored"), "{stderr}");
    assert!(path.join("scratch.txt").is_file());
    Ok(())
}

#[test]
fn worktree_remove_keeps_a_tree_with_only_ignored_output_and_says_why() -> Result {
    // Removal policy belongs to the worktree owner (#1816); until it clears
    // ignored build output, the hook leaves the tree and exits 2 with the reason.
    let rig = TestRig::new()?;
    fs::write(rig.project.join(".gitignore"), "target/\n")?;
    git(&rig, &["add", ".gitignore"])?;
    commit_file(&rig)?;
    let made = run_hook(&rig, "worktree-create", &worktree_input(&rig, "built"))?;
    assert_eq!(code(&made), 0);
    let path = PathBuf::from(String::from_utf8(made.stdout.bytes)?.trim_end());
    fs::create_dir(path.join("target"))?;
    fs::write(path.join("target/out.o"), "object")?;
    let refused = run_hook(&rig, "worktree-remove", &removal_input(&rig, &path))?;
    assert_eq!(code(&refused), 2);
    assert!(refused.stdout.bytes.is_empty());
    let stderr = String::from_utf8(refused.stderr.bytes)?;
    assert!(
        stderr.starts_with("worktree-remove: refused:")
            && stderr.contains("undeclared ignored paths: target/"),
        "{stderr}"
    );
    assert!(path.join("target/out.o").is_file());
    Ok(())
}
