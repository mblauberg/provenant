//! `land` against a fake `gh` and real Git remotes, worktrees and branches.
use super::worktree_tests::support::{Result, TestRig, code};
use super::worktree_tests::{command, git, json, repository};
use std::{
    fs,
    io::{self, Write},
    os::unix::fs::symlink,
    path::{Path, PathBuf},
};

const NUMBER: &str = "1668";

/// The fake `gh` the driver re-enters as: a passing, unchanged pull request.
pub(crate) fn fake_gh(args: &[String]) -> Result<u8> {
    let call = args.join(" ");
    let dir = std::env::var_os("FIXTURE_GH_DIR")
        .map(PathBuf::from)
        .unwrap_or_default();
    writeln!(
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("gh-calls"))?,
        "{call}"
    )?;
    match call.as_str() {
        "pr view 1668 --json headRefOid" => {
            io::stdout().write_all(&fs::read(dir.join("head.json"))?)?
        }
        "pr view 1668 --json state,baseRefName,headRefName,isCrossRepository" => {
            io::stdout().write_all(&fs::read(dir.join("view.json"))?)?;
        }
        "pr checks 1668 --watch" => {
            fs::write(dir.join("watched"), "")?;
        }
        "pr checks 1668 --json bucket" => {
            if dir.join("checks-fail").exists() {
                return Ok(8);
            }
            io::stdout().write_all(br#"[{"bucket":"pass"}]"#)?;
        }
        value if value.starts_with("pr merge 1668 --merge --match-head-commit ") => {
            fs::write(dir.join("merge-args"), args.join("\n") + "\n")?;
        }
        _ => return Err(format!("unexpected gh arguments: {call}").into()),
    }
    Ok(0)
}

struct Landing {
    rig: TestRig,
    worktree: PathBuf,
    /// The branch head the pull request merged.
    head: String,
    /// `main` before the merge, which the local checkout still has.
    stale: String,
}

fn git_in(rig: &TestRig, dir: &Path, args: &[&str]) -> Result<String> {
    let dir = dir.to_str().ok_or("path")?;
    let mut argv = vec!["-C", dir];
    argv.extend_from_slice(args);
    git(rig, &argv)
}

/// A project whose `feat` worktree was merged into `origin/main` by a merge
/// commit, with the local `main` still before it.
fn landing() -> Result<Landing> {
    landing_with_git_dir(false)
}

fn landing_with_git_dir(separate: bool) -> Result<Landing> {
    let mut rig = repository()?;
    if separate {
        git(
            &rig,
            &[
                "init",
                "-q",
                "--separate-git-dir",
                rig.root.join("separate.git").to_str().ok_or("path")?,
            ],
        )?;
    }
    let admin = rig.project.join(git(&rig, &["rev-parse", "--git-dir"])?);
    let bin = rig.root.join("bin");
    fs::create_dir(&bin)?;
    symlink(std::env::current_exe()?, bin.join("gh"))?;
    let (_, path) = rig
        .env
        .iter_mut()
        .find(|(key, _)| key == "PATH")
        .ok_or("PATH")?;
    *path = format!("{}:/usr/bin:/bin", bin.display()).into();
    fs::write(
        admin.join("info/exclude"),
        "head.json\nview.json\ngh-calls\nwatched\nmerge-args\nchecks-fail\n",
    )?;
    fs::write(rig.project.join(".gitignore"), ".worktrees/\n**/target/\n")?;
    git(&rig, &["add", ".gitignore"])?;
    git(&rig, &["commit", "-qm", "ignore build output"])?;
    let stale = git(&rig, &["rev-parse", "HEAD"])?;
    let origin = rig.root.join("origin.git");
    git(
        &rig,
        &["init", "-q", "--bare", origin.to_str().ok_or("path")?],
    )?;
    git(
        &rig,
        &["remote", "add", "origin", origin.to_str().ok_or("path")?],
    )?;
    git(&rig, &["push", "-q", "origin", "main"])?;
    let created = command(&rig, &["worktree", "create", "feat", "--json"])?;
    assert_eq!(
        code(&created),
        0,
        "{}",
        String::from_utf8_lossy(&created.stdout.bytes)
    );
    let worktree = rig.project.join(".worktrees/feat");
    fs::write(worktree.join("feature.txt"), "feature\n")?;
    git_in(&rig, &worktree, &["add", "feature.txt"])?;
    git_in(&rig, &worktree, &["commit", "-qm", "feature"])?;
    let head = git_in(&rig, &worktree, &["rev-parse", "HEAD"])?;
    git(
        &rig,
        &["merge", "-q", "--no-ff", "-m", "merge feat", "feat"],
    )?;
    git(&rig, &["push", "-q", "origin", "main"])?;
    git(&rig, &["reset", "-q", "--hard", &stale])?;
    fs::write(
        rig.project.join("head.json"),
        format!(r#"{{"headRefOid":"{head}"}}"#),
    )?;
    fs::write(
        rig.project.join("view.json"),
        r#"{"state":"MERGED","baseRefName":"main","headRefName":"feat","isCrossRepository":false}"#,
    )?;
    Ok(Landing {
        rig,
        worktree,
        head,
        stale,
    })
}

fn land(landing: &Landing) -> Result<serde_json::Value> {
    let output = command(&landing.rig, &["land", NUMBER, "--json"])?;
    let value = json(&output)?;
    assert!(
        code(&output) == 0 || value["data"]["merged"] == true,
        "{value}"
    );
    assert_eq!(value["data"]["head"], landing.head);
    Ok(value)
}

fn status<'a>(value: &'a serde_json::Value, step: &str) -> Result<&'a str> {
    value["data"]["steps"]
        .as_array()
        .and_then(|steps| steps.iter().find(|entry| entry["step"] == step))
        .and_then(|entry| entry["status"].as_str())
        .ok_or_else(|| format!("no {step} step in {value}").into())
}

fn branch_exists(rig: &TestRig, branch: &str) -> Result<bool> {
    Ok(!git(rig, &["branch", "--list", branch])?.is_empty())
}

pub(crate) fn pr_land_merges_then_fetches_fast_forwards_and_prunes() -> Result {
    let landing = landing()?;
    let rig = &landing.rig;
    // Build output the worktree owns, which would otherwise block removal.
    fs::create_dir_all(landing.worktree.join("crate/target/debug"))?;
    fs::write(
        landing.worktree.join("crate/target/debug/artifact"),
        "built",
    )?;
    let output = command(rig, &["land", NUMBER, "--head", &landing.head, "--json"])?;
    let value = json(&output)?;
    assert_eq!(code(&output), 0, "{value}");
    assert_eq!(value["data"]["merged"], true);
    for step in ["fetch", "fast_forward", "worktree", "branch"] {
        assert_eq!(status(&value, step)?, "done", "{step}: {value}");
    }
    // The merge went through the existing repository workflow at this head.
    let merge = fs::read_to_string(rig.project.join("merge-args"))?;
    assert!(
        merge.contains(&format!("--match-head-commit\n{}", landing.head)),
        "{merge}"
    );
    assert!(rig.project.join("watched").exists());
    assert_eq!(
        git(rig, &["rev-parse", "main"])?,
        git(rig, &["rev-parse", "origin/main"])?
    );
    assert_ne!(git(rig, &["rev-parse", "main"])?, landing.stale);
    assert!(!landing.worktree.exists());
    assert!(!branch_exists(rig, "feat")?);
    // Landing again finds nothing left to prune, and the text form names each step.
    let text = command(rig, &["land", NUMBER])?;
    assert_eq!(code(&text), 0);
    let text = String::from_utf8(text.stdout.bytes)?;
    assert!(
        text.starts_with(&format!("merged #{NUMBER} {}\n", landing.head)),
        "{text}"
    );
    assert!(text.contains("  worktree skipped"), "{text}");
    assert!(text.contains("  branch skipped"), "{text}");
    Ok(())
}

pub(crate) fn pr_land_prunes_the_selected_merged_worktree() -> Result {
    let mut landing = landing()?;
    // Git retains registrations of unrelated checkouts that disappeared.
    let missing = landing.rig.project.join(".worktrees/disappeared");
    git(
        &landing.rig,
        &[
            "worktree",
            "add",
            "--detach",
            missing.to_str().ok_or("path")?,
        ],
    )?;
    fs::remove_dir_all(&missing)?;
    landing.rig.env.push((
        "FIXTURE_GH_DIR".into(),
        landing.rig.project.clone().into_os_string(),
    ));
    let path = landing.worktree.to_str().ok_or("path")?;
    let output = command(&landing.rig, &["--project", path, "land", NUMBER, "--json"])?;
    let value = json(&output)?;
    assert_eq!(code(&output), 0, "{value}");
    assert_eq!(status(&value, "worktree")?, "done", "{value}");
    assert_eq!(status(&value, "branch")?, "done", "{value}");
    assert!(!landing.worktree.exists());
    assert!(!branch_exists(&landing.rig, "feat")?);
    Ok(())
}

// The shell remains parked in the lane while its child runs land. This uses
// the real observer, including the parent shell in the native census.
#[allow(clippy::disallowed_methods)] // Real child cwd exercises the native worker observer.
pub(crate) fn pr_land_from_lane_ignores_only_its_callers() -> Result {
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    for another_worker in [false, true] {
        let mut landing = landing()?;
        let main = landing.rig.project.clone();
        landing
            .rig
            .env
            .push(("FIXTURE_GH_DIR".into(), main.clone().into_os_string()));
        landing
            .rig
            .env
            .push(("FIXTURE_SYSTEM_WORKERS".into(), "1".into()));
        let worker = if another_worker {
            Some(Child(
                std::process::Command::new("/bin/sh")
                    .args(["-c", "read line"])
                    .current_dir(&landing.worktree)
                    .stdin(std::process::Stdio::piped())
                    .spawn()?,
            ))
        } else {
            None
        };
        landing.rig.project = landing.worktree.clone();
        let executable = std::env::current_exe()?;
        let state = landing.rig.state.to_str().ok_or("state path")?;
        let output = landing.rig.execute(
            Path::new("/bin/sh"),
            &[
                "-c",
                "\"$@\" & wait $!",
                "land-shell",
                executable.to_str().ok_or("path")?,
                "--state-dir",
                state,
                "land",
                NUMBER,
                "--json",
            ],
            None,
        )?;
        let value = json(&output)?;
        assert_eq!(code(&output), if another_worker { 1 } else { 0 }, "{value}");
        assert_eq!(
            status(&value, "worktree")?,
            if another_worker { "refused" } else { "done" },
            "{value}"
        );
        assert_eq!(landing.worktree.exists(), another_worker, "{value}");
        if another_worker {
            assert!(
                value.to_string().contains("worktree writer is live"),
                "{value}"
            );
        }
        drop(worker);
        landing.rig.project = main;
        assert_eq!(branch_exists(&landing.rig, "feat")?, another_worker);
    }
    Ok(())
}

pub(crate) fn pr_land_canonicalises_quoted_worktree_paths() -> Result {
    let mut landing = landing()?;
    let alias = landing.rig.root.join("alias\twith quotes\"");
    symlink(&landing.rig.project, &alias)?;
    fs::write(
        landing.rig.project.join(".git/worktrees/feat/gitdir"),
        format!("{}/.worktrees/feat/.git\n", alias.display()),
    )?;
    landing.rig.env.push((
        "FIXTURE_GH_DIR".into(),
        landing.rig.project.clone().into_os_string(),
    ));
    let selected = alias.join(".worktrees/feat");
    let output = command(
        &landing.rig,
        &[
            "--project",
            selected.to_str().ok_or("path")?,
            "land",
            NUMBER,
            "--json",
        ],
    )?;
    let value = json(&output)?;
    assert_eq!(code(&output), 0, "{value}");
    assert_eq!(status(&value, "worktree")?, "done", "{value}");
    assert!(!landing.worktree.exists());
    Ok(())
}

pub(crate) fn pr_land_recovers_only_abandoned_prepared_writer_leases() -> Result {
    use provenant_core::Clock;
    use provenant_supervisor::custody::{Scope, WriterBinding};
    for case in ["abandoned", "held", "malformed", "wrong_binding"] {
        let landing = landing()?;
        let (_, context) = super::worktree_tests::writer_context(&landing.rig)?;
        let clock = provenant_os::clock::SystemClock;
        let cancel = provenant_core::CancelToken::new();
        let request = provenant_run::resolve_writer(
            context.state_root.as_path(),
            &landing.rig.project,
            &landing.worktree,
        )?;
        let writer = provenant_run::prepare_writer(
            &request,
            context.state_root.as_path(),
            context.project_id,
        )?;
        let binding = WriterBinding::from_verified(
            writer.worktree.repo().root_dir(),
            writer.worktree.repo().private_dir(),
            if case == "wrong_binding" {
                "refs/heads/other"
            } else {
                writer.branch.as_str()
            },
            provenant_core::GitOid::parse(&landing.head)?,
        )?;
        let state = landing
            .rig
            .state
            .join("worktrees")
            .join(context.project_id.to_string())
            .join(provenant_confinement::worktrees::state_dir_name(
                &landing.worktree,
            ));
        let lease = provenant_supervisor::fixture::acquire_writer_with_binding(
            provenant_os::fs::Directory::open_anchor(&state)?,
            Scope {
                run: provenant_core::RunId::from_bytes([81; 16]),
                attempt: provenant_core::AttemptId::from_bytes([82; 16]),
                generation: 1,
            },
            binding,
            provenant_core::WaitBudget::nonblocking(clock.mono_now()),
            &cancel,
            &clock,
        )?;
        let lease = if case == "held" {
            Some(lease)
        } else {
            drop(lease);
            None
        };
        if case == "malformed" {
            fs::write(state.join("reservation.json"), b"malformed reservation")?;
        }
        let reservation = fs::read(state.join("reservation.json"))?;
        let value = land(&landing)?;
        let removed = case == "abandoned";
        assert_eq!(
            status(&value, "worktree")?,
            if removed { "done" } else { "refused" },
            "{case}: {value}"
        );
        assert_eq!(landing.worktree.exists(), !removed);
        assert_eq!(branch_exists(&landing.rig, "feat")?, !removed);
        if removed {
            assert!(!state.join("reservation.json").exists());
            assert!(state.join("settlement.json").exists());
        } else {
            assert_eq!(fs::read(state.join("reservation.json"))?, reservation);
        }
        drop(lease);
    }
    Ok(())
}

/// Terminal records may live in a child store while custody remains in the
/// host registry. Exercise land through the production recursive snapshot.
pub(crate) fn pr_land_recovers_terminal_writer_leases_in_parent_and_child_stores() -> Result {
    use provenant_core::{Clock, WaitBudget};
    use provenant_os::fs::Directory;
    use provenant_run::{AttemptState, RouteRecord, RunStore, TerminalPublication};
    use provenant_supervisor::custody::{
        ExitObservation, PublicationState, Scope, SettlementEvidence, WriterBinding,
    };
    use std::os::unix::fs::OpenOptionsExt;
    for child in [false, true] {
        let landing = landing()?;
        let rig = &landing.rig;
        let (roots, mut context) = super::worktree_tests::writer_context(rig)?;
        let clock = provenant_os::clock::SystemClock;
        let cancel = provenant_core::CancelToken::new();
        let call = provenant_run::Request {
            clock: &clock,
            budget: WaitBudget::cli(clock.mono_now(), 10)?,
            cancel: &cancel,
        };
        // Keep a normal host store beside the nested store, as the snapshot
        // sees it during a real dispatch from a lane.
        drop(RunStore::open(&context, &call)?);
        if child {
            context.state_root = crate::roots::child(
                &rig.home,
                &rig.project,
                rig.state.join("children").join("retained-writer"),
            )?
            .state;
        }
        let store = RunStore::open(&context, &call)?.with_writer_registry(&rig.state);
        let request = provenant_run::resolve_writer(&rig.state, &rig.project, &landing.worktree)?;
        let input =
            super::worktree_tests::writer_run(request.clone(), "land-terminal", 91, "done")?;
        store.create(&input, &call)?;
        let id = input.attempts[0].attempt_id;
        let mut running = store.record(id, &call)?;
        running.revision += 1;
        running.generation = 1;
        running.state = AttemptState::Running;
        running.started_at = Some(clock.wall_now()?);
        running.provider_launched = Some(true);
        // The provider seam advances admission without launching a backend;
        // terminal publication below still uses Run's canonical transaction.
        rusqlite::Connection::open(context.state_root.as_path().join("run.sqlite3"))?.execute(
            "UPDATE attempts SET revision=?1,record=?2 WHERE id=?3",
            rusqlite::params![
                i64::try_from(running.revision)?,
                provenant_records::CanonicalV1::bytes(&running)?.as_bytes(),
                id.to_string()
            ],
        )?;
        let scope = Scope {
            run: input.run_id,
            attempt: id,
            generation: running.generation,
        };
        let attempt = context
            .state_root
            .as_path()
            .join(format!("runs/{}/attempts/{id}", input.run_id));
        let exit = ExitObservation {
            publication: PublicationState::Durable,
            scope,
            termination: Some(provenant_os::exec::Termination::Exit(0)),
            never_spawned: false,
            stop_reason: None,
            delivered_signals: vec![],
            reaped: vec![],
            omitted_reaped: 0,
            spared: vec![],
            omitted_spared: 0,
            warnings: vec![],
            omitted_warnings: 0,
            terminal_observed: true,
            unresolved_cleanup: false,
            unresolved_pids: vec![],
            settlement_evidence: SettlementEvidence::DarwinCensusAndCustodyLease,
            stdout: Default::default(),
            stderr: Default::default(),
        };
        let mut evidence = serde_json::to_value(&exit)?;
        evidence
            .as_object_mut()
            .ok_or("exit fixture")?
            .remove("publication");
        evidence["schema_version"] = 1.into();
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(attempt.join("process-exit.json"))?;
        file.write_all(&serde_json::to_vec(&evidence)?)?;
        file.sync_all()?;
        drop(file);
        let route = RouteRecord {
            provider: Some("claude".into()),
            model: Some("claude/test-model".into()),
            ..Default::default()
        };
        store.publish_terminal(
            id,
            running.revision,
            &TerminalPublication {
                outcome: provenant_run::Outcome::Succeeded,
                started_at: running.started_at,
                finished_at: clock.wall_now()?,
                provider_ms: Some(1),
                reason: None,
                result: "landed writer",
                complete: true,
                redacted: false,
                resolved_route: Some(route.clone()),
                applied_route: Some(route.clone()),
                observed_route: Some(route),
                usage: Default::default(),
                session: None,
            },
            &exit,
            &call,
        )?;
        let attempt_directory = Directory::open_anchor(&attempt)?;
        attempt_directory.mkdir(std::ffi::OsStr::new("quarantine"), 0o700)?;
        let archive =
            attempt_directory.open_verified_directory(std::ffi::OsStr::new("quarantine"))?;
        let manifest = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1, "scope": scope, "kind": "quarantine_manifest",
        }))?;
        archive
            .atomic_write(
                std::ffi::OsStr::new("manifest.json"),
                ".manifest-",
                &manifest,
                provenant_os::fs::PublishMode::NoReplace,
            )?
            .require_durable()?;
        let writer = provenant_run::prepare_writer(&request, &rig.state, context.project_id)?;
        let state = rig
            .state
            .join("worktrees")
            .join(context.project_id.to_string())
            .join(provenant_confinement::worktrees::state_dir_name(
                &landing.worktree,
            ));
        let binding = WriterBinding::from_verified(
            writer.worktree.repo().root_dir(),
            writer.worktree.repo().private_dir(),
            writer.branch.as_str(),
            provenant_core::GitOid::parse(&landing.head)?,
        )?;
        drop(provenant_supervisor::fixture::acquire_writer_with_binding(
            Directory::open_anchor(&state)?,
            scope,
            binding,
            WaitBudget::nonblocking(clock.mono_now()),
            &cancel,
            &clock,
        )?);
        let reservation = state.join("reservation.json");
        let mut fence: serde_json::Value = serde_json::from_slice(&fs::read(&reservation)?)?;
        let metadata = Directory::open_anchor(&attempt)?.metadata()?;
        fence["launch_state"] = "never_spawned".into();
        fence["unresolved_cleanup"] = false.into();
        fence["census_incomplete"] = false.into();
        fence["attempt_device"] = metadata.device.into();
        fence["attempt_inode"] = metadata.inode.into();
        fs::write(&reservation, serde_json::to_vec(&fence)?)?;
        assert!(
            crate::gc_support::lane_states(&roots, &rig.home, &rig.project, context.project_id)?
                .get(&landing.worktree)
                == Some(&true)
        );
        let value = land(&landing)?;
        assert_eq!(
            status(&value, "worktree")?,
            "done",
            "child={child}: {value}"
        );
        assert_eq!(status(&value, "branch")?, "done", "child={child}: {value}");
        assert!(!landing.worktree.exists());
        assert!(!branch_exists(rig, "feat")?);
        assert!(!reservation.exists());
        assert!(state.join("settlement.json").is_file());
        assert!(attempt.join("release.json").is_file());
        assert_eq!(
            fs::read(attempt.join("quarantine/manifest.json"))?,
            manifest
        );
        assert!(matches!(
            store.record(id, &call)?.state,
            AttemptState::Terminal { .. }
        ));
    }
    Ok(())
}

pub(crate) fn pr_land_primary_with_separate_git_directory() -> Result {
    for from_lane in [false, true] {
        let mut landing = landing_with_git_dir(true)?;
        let main = landing.rig.project.clone();
        landing
            .rig
            .env
            .push(("FIXTURE_GH_DIR".into(), main.clone().into_os_string()));
        landing
            .rig
            .env
            .push(("FIXTURE_SYSTEM_WORKERS".into(), "1".into()));
        if from_lane {
            landing.rig.project = landing.worktree.clone();
        }
        let value = land(&landing)?;
        assert_eq!(status(&value, "worktree")?, "done", "{value}");
        assert_eq!(status(&value, "branch")?, "done", "{value}");
        assert!(!landing.worktree.exists());
        landing.rig.project = main;
        assert!(!branch_exists(&landing.rig, "feat")?);
    }
    Ok(())
}

pub(crate) fn pr_land_refuses_each_unsafe_step_and_reports_the_rest() -> Result {
    // A checkout of the base with local changes is not fast-forwarded.
    let landing = landing()?;
    fs::write(landing.rig.project.join("stray.txt"), "mine\n")?;
    let output = command(
        &landing.rig,
        &["land", NUMBER, "--head", &landing.head, "--json"],
    )?;
    let value = json(&output)?;
    assert_eq!(code(&output), 1, "{value}");
    assert_eq!(value["data"]["merged"], true);
    assert_eq!(status(&value, "fast_forward")?, "refused");
    assert_eq!(status(&value, "worktree")?, "done");
    assert_eq!(
        git(&landing.rig, &["rev-parse", "main"])?,
        landing.stale,
        "a dirty checkout keeps its base"
    );
    assert!(landing.rig.project.join("stray.txt").exists());

    // A worktree with uncommitted work is kept, and so is its branch.
    let mut landing = self::landing()?;
    fs::write(landing.worktree.join("notes.txt"), "unsaved\n")?;
    fs::create_dir_all(landing.worktree.join("target"))?;
    fs::write(landing.worktree.join("target/artifact"), "built")?;
    landing.rig.env.push((
        "FIXTURE_GH_DIR".into(),
        landing.rig.project.clone().into_os_string(),
    ));
    let output = command(
        &landing.rig,
        &[
            "--project",
            landing.worktree.to_str().ok_or("path")?,
            "land",
            NUMBER,
            "--json",
        ],
    )?;
    let value = json(&output)?;
    assert_eq!(code(&output), 1, "{value}");
    assert_eq!(status(&value, "worktree")?, "refused", "{value}");
    assert_eq!(status(&value, "branch")?, "refused", "{value}");
    assert!(landing.worktree.join("notes.txt").exists());
    assert!(
        landing.worktree.join("target/artifact").exists(),
        "a dirty worktree keeps its build output"
    );
    assert!(branch_exists(&landing.rig, "feat")?);

    // Commits the pull request never merged keep the branch.
    let mut landing = self::landing()?;
    fs::write(landing.worktree.join("later.txt"), "later\n")?;
    git_in(&landing.rig, &landing.worktree, &["add", "later.txt"])?;
    git_in(&landing.rig, &landing.worktree, &["commit", "-qm", "later"])?;
    landing.rig.env.push((
        "FIXTURE_GH_DIR".into(),
        landing.rig.project.clone().into_os_string(),
    ));
    let output = command(
        &landing.rig,
        &[
            "--project",
            landing.worktree.to_str().ok_or("path")?,
            "land",
            NUMBER,
            "--json",
        ],
    )?;
    let value = json(&output)?;
    assert_eq!(code(&output), 1, "{value}");
    assert_eq!(status(&value, "worktree")?, "refused", "{value}");
    assert_eq!(status(&value, "branch")?, "refused", "{value}");
    assert!(
        landing.worktree.join("later.txt").exists(),
        "the checkout holding unmerged commits survives"
    );
    assert!(branch_exists(&landing.rig, "feat")?);
    Ok(())
}

pub(crate) fn pr_land_leaves_a_local_namesake_of_a_fork_branch_alone() -> Result {
    // The merged pull request came from a fork's `feat`, so the local `feat`
    // branch and its clean, fully merged worktree are somebody else's.
    let landing = landing()?;
    fs::write(
        landing.rig.project.join("view.json"),
        r#"{"state":"MERGED","baseRefName":"main","headRefName":"feat","isCrossRepository":true}"#,
    )?;
    let value = land(&landing)?;
    assert_eq!(value["data"]["merged"], true, "{value}");
    assert_eq!(status(&value, "fast_forward")?, "done", "{value}");
    assert_eq!(status(&value, "worktree")?, "skipped", "{value}");
    assert_eq!(status(&value, "branch")?, "skipped", "{value}");
    assert!(
        value.to_string().contains("another repository"),
        "the report says why: {value}"
    );
    assert!(landing.worktree.exists());
    assert!(branch_exists(&landing.rig, "feat")?);
    Ok(())
}

pub(crate) fn pr_land_leaves_a_reused_branch_name_alone() -> Result {
    // Someone recreated `feat` at the base after the pull request's `feat` was
    // merged. The base contains that tip, but it is not the merged head, so the
    // branch and its clean checkout are not the pull request's to remove.
    let landing = landing()?;
    git_in(
        &landing.rig,
        &landing.worktree,
        &["reset", "-q", "--hard", "origin/main"],
    )?;
    let reused = git(&landing.rig, &["rev-parse", "feat"])?;
    assert_ne!(reused, landing.head);
    let value = land(&landing)?;
    assert_eq!(value["data"]["merged"], true, "{value}");
    assert_eq!(status(&value, "fast_forward")?, "done", "{value}");
    assert_eq!(status(&value, "worktree")?, "skipped", "{value}");
    assert_eq!(status(&value, "branch")?, "skipped", "{value}");
    assert!(
        value
            .to_string()
            .contains("not the merged pull request head"),
        "the report says why: {value}"
    );
    assert!(landing.worktree.exists());
    assert_eq!(git(&landing.rig, &["rev-parse", "feat"])?, reused);
    Ok(())
}

pub(crate) fn pr_land_deletes_an_unregistered_branch_only_at_the_merged_head() -> Result {
    // No worktree is registered for `feat`. After the cleanup check passes,
    // another process moves `feat` to the base, which also contains it. The
    // deletion is pinned to the merged head, so it leaves that branch alone.
    let landing = landing()?;
    unregister(&landing)?;
    let moved = git(&landing.rig, &["rev-parse", "origin/main"])?;
    let (status, detail) = race(&landing, &["branch", "-f", "feat", "origin/main"])?;
    assert_eq!(status, "refused", "{detail}");
    assert_eq!(git(&landing.rig, &["rev-parse", "feat"])?, moved);

    // Undisturbed, the same branch at the merged head is deleted.
    let landing = self::landing()?;
    unregister(&landing)?;
    let (status, detail) = race(&landing, &[])?;
    assert_eq!(status, "done", "{detail}");
    assert!(!branch_exists(&landing.rig, "feat")?);
    Ok(())
}

pub(crate) fn pr_land_keeps_a_branch_held_by_a_paused_rebase() -> Result {
    // The rebase leaves its worktree on a detached HEAD, so only the rebase
    // state records that `feat` is still in use.
    let landing = landing()?;
    let held = hold(&landing, "rebase")?;
    assert_kept(&landing, race(&landing, &[])?, "rebased")?;

    // Once the rebase is aborted nothing holds the branch, so it is deleted.
    git_in(&landing.rig, &held, &["rebase", "--abort"])?;
    git_in(&landing.rig, &held, &["checkout", "-q", "--detach"])?;
    assert_deleted(&landing)
}

pub(crate) fn pr_land_keeps_a_branch_held_by_a_bisect() -> Result {
    let landing = landing()?;
    let held = hold(&landing, "bisect")?;
    assert_kept(&landing, race(&landing, &[])?, "bisected")?;

    git_in(&landing.rig, &held, &["bisect", "reset"])?;
    git_in(&landing.rig, &held, &["checkout", "-q", "--detach"])?;
    assert_deleted(&landing)
}

pub(crate) fn pr_land_keeps_a_branch_held_in_a_checkout_that_went_away() -> Result {
    // The paused rebase lives in the worktree's administrative directory, so
    // losing the checkout directory does not release the branch.
    let landing = landing()?;
    let held = hold(&landing, "rebase")?;
    fs::rename(&held, landing.rig.root.join("moved-away"))?;
    assert_kept(&landing, race(&landing, &[])?, "rebased")
}

pub(crate) fn pr_land_refuses_when_a_worktree_entry_cannot_be_read() -> Result {
    use std::os::unix::fs::PermissionsExt;
    let landing = landing()?;
    hold(&landing, "rebase")?;
    let entry = landing.rig.project.join(".git/worktrees/held");
    fs::set_permissions(&entry, fs::Permissions::from_mode(0o000))?;
    let outcome = race(&landing, &[]);
    fs::set_permissions(&entry, fs::Permissions::from_mode(0o755))?;
    assert_kept(&landing, outcome?, "cannot read")
}

pub(crate) fn pr_land_refuses_when_worktree_state_is_malformed() -> Result {
    // A truncated rebase record or an unparseable HEAD must refuse, not read
    // as a worktree that holds nothing.
    let landing = landing()?;
    hold(&landing, "rebase")?;
    let entry = landing.rig.project.join(".git/worktrees/held");
    let head_name = ["rebase-merge", "rebase-apply"]
        .iter()
        .map(|dir| entry.join(dir).join("head-name"))
        .find(|path| path.is_file())
        .ok_or("no rebase head-name")?;
    let original = fs::read(&head_name)?;
    for malformed in ["", "refs/\n"] {
        fs::write(&head_name, malformed)?;
        assert_kept(&landing, race(&landing, &[])?, "cannot parse")?;
    }
    fs::write(&head_name, original)?;

    let head = entry.join("HEAD");
    let original = fs::read(&head)?;
    fs::write(&head, "abc\n")?;
    let outcome = race(&landing, &[]);
    fs::write(&head, original)?;
    assert_kept(&landing, outcome?, "cannot parse HEAD")?;

    let bisect = self::landing()?;
    hold(&bisect, "bisect")?;
    let start = bisect.rig.project.join(".git/BISECT_START");
    let start = if start.is_file() {
        start
    } else {
        bisect.rig.project.join(".git/worktrees/held/BISECT_START")
    };
    fs::write(&start, ":\n")?;
    assert_kept(&bisect, race(&bisect, &[])?, "cannot parse BISECT_START")
}

/// The `branch` step refused, naming `reason`, and `feat` is still at the
/// merged head.
fn assert_kept(landing: &Landing, (status, detail): (String, String), reason: &str) -> Result {
    assert_eq!(status, "refused", "{detail}");
    assert!(detail.contains(reason), "{detail}");
    assert_eq!(git(&landing.rig, &["rev-parse", "feat"])?, landing.head);
    Ok(())
}

/// With nothing holding `feat`, the cleanup deletes it.
fn assert_deleted(landing: &Landing) -> Result {
    let (status, detail) = race(landing, &[])?;
    assert_eq!(status, "done", "{detail}");
    assert!(!branch_exists(&landing.rig, "feat")?);
    Ok(())
}

/// A second worktree that has `feat` mid-`operation` on a detached HEAD, with
/// the registered `feat` worktree dropped.
fn hold(landing: &Landing, operation: &str) -> Result<PathBuf> {
    unregister(landing)?;
    let held = landing.rig.root.join("held");
    git(
        &landing.rig,
        &[
            "worktree",
            "add",
            "-q",
            held.to_str().ok_or("path")?,
            "feat",
        ],
    )?;
    if operation == "rebase" {
        git_in(
            &landing.rig,
            &held,
            &[
                "-c",
                "sequence.editor=sed -i.bak s/^pick/edit/",
                "rebase",
                "-q",
                "-i",
                "HEAD~1",
            ],
        )?;
    } else {
        let first = git_in(
            &landing.rig,
            &held,
            &["rev-list", "--max-parents=0", "HEAD"],
        )?;
        git_in(&landing.rig, &held, &["bisect", "start", "HEAD", &first])?;
    }
    let listing = git(&landing.rig, &["worktree", "list", "--porcelain"])?;
    assert!(
        !listing.contains("branch refs/heads/feat"),
        "the worktree must be detached: {listing}"
    );
    Ok(held)
}

/// The `branch` step when `between` runs after the cleanup check.
fn race(landing: &Landing, between: &[&str]) -> Result<(String, String)> {
    let mut argv = vec!["--land-race", landing.head.as_str()];
    argv.extend_from_slice(between);
    let argv: Vec<_> = argv.into_iter().map(std::ffi::OsString::from).collect();
    let output = landing
        .rig
        .execute_os(&std::env::current_exe()?, &argv, None)?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    let text = String::from_utf8(output.stdout.bytes)?;
    let (status, detail) = text.split_once('\n').ok_or("race output")?;
    Ok((status.to_owned(), detail.trim().to_owned()))
}

/// Drop the `feat` worktree and keep its branch.
fn unregister(landing: &Landing) -> Result {
    let path = landing.worktree.to_str().ok_or("path")?;
    git(&landing.rig, &["worktree", "remove", "--force", path])?;
    Ok(())
}

/// The message of a refused `pr open`, which is a usage error.
fn open_refusal(rig: &TestRig, args: &[&str]) -> Result<String> {
    let mut argv = vec!["pr", "open"];
    argv.extend_from_slice(args);
    argv.push("--json");
    let output = command(rig, &argv)?;
    assert_eq!(
        code(&output),
        2,
        "{}",
        String::from_utf8_lossy(&output.stdout.bytes)
    );
    let value = json(&output)?;
    Ok(value["error"]["message"]
        .as_str()
        .ok_or("error message")?
        .to_owned())
}

pub(crate) fn pr_open_refuses_what_the_receipts_cannot_support() -> Result {
    let rig = repository()?;
    let summary = rig.root.join("summary.md");
    let summary_arg = summary.to_str().ok_or("path")?;
    fs::write(
        &summary,
        "# feat(cli): add the thing\n\nIt adds the thing.\n",
    )?;
    let unknown = "ab".repeat(16);
    let args = [
        "--from-attempt",
        &unknown,
        "--summary-file",
        summary_arg,
        "--dry-run",
    ];
    // No template, then an empty summary, then an attempt nobody ran.
    assert!(open_refusal(&rig, &args)?.contains("pull request template"));
    fs::create_dir_all(rig.project.join(".github"))?;
    fs::write(
        rig.project.join(".github/pull_request_template.md"),
        include_str!("../../../.github/pull_request_template.md"),
    )?;
    fs::write(&summary, "\n")?;
    assert!(open_refusal(&rig, &args)?.contains("summary file is empty"));
    fs::write(
        &summary,
        "# feat(cli): add the thing\n\nIt adds the thing.\n",
    )?;
    assert!(!open_refusal(&rig, &args)?.is_empty());

    // A lane that never ran cannot back a pull request.
    let dispatched = rig.execute_os(
        &std::env::current_exe()?,
        &[
            "--state-dir".into(),
            rig.state.clone().into_os_string(),
            "dispatch".into(),
            "--model".into(),
            "codex/gpt-6-luna".into(),
            "--operation-id".into(),
            "pr-open-fixture".into(),
            "--prompt-stdin".into(),
            "--json".into(),
        ],
        Some(b"never launched"),
    )?;
    let attempt = json(&dispatched)?["data"]["lanes"][0]["attempt_id"]
        .as_str()
        .ok_or("attempt")?
        .to_owned();
    let message = open_refusal(
        &rig,
        &[
            "--from-attempt",
            &attempt,
            "--summary-file",
            summary_arg,
            "--dry-run",
        ],
    )?;
    assert!(
        message.contains("only a succeeded or review_required attempt"),
        "{message}"
    );
    // A malformed attempt ID is a usage error before anything is read.
    let invalid = command(
        &rig,
        &[
            "pr",
            "open",
            "--from-attempt",
            "nope",
            "--summary-file",
            summary_arg,
        ],
    )?;
    assert_eq!(code(&invalid), 2);
    Ok(())
}

pub(crate) fn pr_land_changes_nothing_when_the_merge_does_not_happen() -> Result {
    for flag in ["checks-fail", "moved"] {
        let landing = landing()?;
        let rig = &landing.rig;
        if flag == "moved" {
            fs::write(
                rig.project.join("head.json"),
                r#"{"headRefOid":"2222222222222222222222222222222222222222"}"#,
            )?;
        } else {
            fs::write(rig.project.join(flag), "")?;
        }
        let output = command(rig, &["land", NUMBER, "--head", &landing.head, "--json"])?;
        assert_ne!(code(&output), 0, "{flag}");
        let value = json(&output)?;
        assert_eq!(value["data"]["merged"], false, "{value}");
        assert!(!rig.project.join("merge-args").exists(), "{flag}");
        assert!(landing.worktree.exists(), "{flag}");
        assert!(branch_exists(rig, "feat")?, "{flag}");
        assert_eq!(git(rig, &["rev-parse", "main"])?, landing.stale, "{flag}");
    }
    // The current head is the default, and a pending merge cleans nothing.
    let landing = landing()?;
    fs::write(
        landing.rig.project.join("view.json"),
        r#"{"state":"OPEN","baseRefName":"main","headRefName":"feat","isCrossRepository":false}"#,
    )?;
    let value = land(&landing)?;
    assert_eq!(value["data"]["merged"], false);
    assert!(landing.worktree.exists());
    Ok(())
}
