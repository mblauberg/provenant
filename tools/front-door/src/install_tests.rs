// The worktree driver includes this module without registering these tests.
#[allow(dead_code)]
#[test]
fn home_refusal_has_install_repair() {
    let result = super::home_error_result("install", "HOME must be absolute");
    let error = result.error.unwrap();
    assert_eq!(error.code, "HOME");
    assert!(error.repair.unwrap().contains("provenant install"));
}

/// An installed state root whose receipt records the given live-record format.
#[allow(dead_code)]
fn roots_with_receipt(
    root: &std::path::Path,
    live_record_format: Option<u32>,
) -> Result<provenant_core::Roots, Box<dyn std::error::Error>> {
    use provenant_core::RootInputs;
    use provenant_records::{AbsolutePath, FileDigest, InstallReceipt, SourceDigest};

    let home = root.join("home");
    let assets = root.join("assets");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&assets)?;
    let mut inputs = RootInputs::new(home, assets);
    inputs.state_dir = Some(root.join("state"));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;

    // The run store only opens a private state directory.
    let install = roots.state.as_path().join("install");
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700).recursive(true);
        builder.create(&install)?;
    }
    let receipt = InstallReceipt {
        extra: Default::default(),
        schema_version: provenant_records::INSTALL_RECEIPT_SCHEMA_VERSION,
        state_format_version: provenant_records::STATE_FORMAT_VERSION,
        live_record_format,
        source_digest: SourceDigest::from_bytes([1; 32]),
        manifest_digest: FileDigest::from_bytes([2; 32]),
        asset_digest: SourceDigest::from_bytes([3; 32]),
        target: "fixture".into(),
        toolchain: "fixture".into(),
        bin_dir: AbsolutePath::new(&root.join("bin"))?,
        harnesses: Vec::new(),
        harness_targets: Vec::new(),
        entries: Vec::new(),
        migrations: Vec::new(),
    };
    std::fs::write(
        install.join("receipt.json"),
        provenant_records::encode(&receipt)?,
    )?;
    Ok(roots)
}

/// Create one live attempt in `project` under the given installed state root.
#[allow(dead_code)]
fn create_live_attempt(
    roots: &provenant_core::Roots,
    project: provenant_core::ProjectId,
    attempt: provenant_core::AttemptId,
) -> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::{ActorId, CancelToken, Digest, OperationId, WallTime};
    use provenant_os::clock::SystemClock;
    use provenant_records::CanonicalV1;
    use provenant_run::{ExecutionContext, PreparedRun, Request, RunStore};

    let context = ExecutionContext {
        state_root: roots.state.clone(),
        project_id: project,
        actor_id: ActorId::from_bytes([2; 16]),
        read_prompts: false,
    };
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = Request {
        clock: &clock,
        cancel: &cancel,
    };
    RunStore::open(&context, &call)?.create(
        &PreparedRun {
            attempt_id: attempt,
            operation_id: OperationId::parse("live-record-warning")?,
            created_at: WallTime::from_micros(100)?,
            prompt: String::new(),
            request: serde_json::from_value(serde_json::json!({
                "schema_version": 1,
                "parent": null,
                "task": {
                    "context": {"graph_id": "live-record", "node_id": "lane", "attempt": 1},
                    "prompt_digest": CanonicalV1::file_digest(b""),
                    "route": {"kind": "model", "provider": "codex", "model": "codex/fixture"},
                    "authority_digest": Digest::from_bytes([0; 32]),
                    "provider_timeout_seconds": 20,
                    "delivery_id": null
                }
            }))?,
        },
        &call,
    )?;
    Ok(())
}

#[test]
fn preflight_warns_with_count_and_path_for_unregistered_legacy_custody()
-> Result<(), Box<dyn std::error::Error>> {
    use super::*;

    let tmp = tempfile::tempdir()?;
    let roots = roots_with_receipt(tmp.path(), None)?;
    let attempts = roots.state.as_path().join("worktrees/unregistered-project");
    let first = attempts.join("slot-a/attempt-a");
    let second = attempts.join("slot-b/attempt-b");
    std::fs::create_dir_all(&first)?;
    std::fs::create_dir_all(&second)?;
    for path in [&first, &second] {
        std::fs::write(path.join("guardian-lock.json"), br#"{"schema_version":1}"#)?;
    }

    let warning =
        warn_legacy_custody(&roots).expect("retained legacy custody produces a visible warning");
    assert!(warning.contains("2 attempt(s)"), "{warning}");
    assert!(
        [&first, &second]
            .iter()
            .any(|path| warning.contains(&path.join("guardian-lock.json").display().to_string())),
        "{warning}"
    );
    assert!(
        warning.contains("remain fenced but no longer block launches"),
        "{warning}"
    );
    Ok(())
}

#[test]
fn preflight_warns_and_names_live_attempts_when_record_format_changes()
-> Result<(), Box<dyn std::error::Error>> {
    use super::*;
    use provenant_core::{AttemptId, ProjectId};
    use provenant_os::clock::SystemClock;

    let tmp = tempfile::tempdir()?;
    let roots = roots_with_receipt(tmp.path(), Some(provenant_records::LIVE_RECORD_FORMAT - 1))?;
    let attempt = AttemptId::from_bytes([4; 16]);
    create_live_attempt(&roots, ProjectId::from_bytes([1; 16]), attempt)?;

    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let context = InstallContext {
        clock: &clock,
        budget: WaitBudget::cli(clock.mono_now(), 30)?,
        cancel: &cancel,
    };
    let warning = warn_live_record_change(&roots, &context)
        .expect("a format change with live attempts warns");
    let current = provenant_records::LIVE_RECORD_FORMAT;
    let change = format!("format {} → {current}", current - 1);
    assert!(warning.contains(&change), "{warning}");
    assert!(warning.contains("live attempts:"), "{warning}");
    assert!(warning.contains(&attempt.to_string()), "{warning}");
    assert!(warning.contains("drain them"), "{warning}");
    Ok(())
}

#[test]
fn preflight_warning_names_live_attempts_in_every_project() -> Result<(), Box<dyn std::error::Error>>
{
    use super::*;
    use provenant_core::{AttemptId, ProjectId};
    use provenant_os::clock::SystemClock;

    let tmp = tempfile::tempdir()?;
    let roots = roots_with_receipt(tmp.path(), Some(provenant_records::LIVE_RECORD_FORMAT - 1))?;
    let here = AttemptId::from_bytes([4; 16]);
    let elsewhere = AttemptId::from_bytes([5; 16]);
    create_live_attempt(&roots, ProjectId::from_bytes([1; 16]), here)?;
    create_live_attempt(&roots, ProjectId::from_bytes([9; 16]), elsewhere)?;

    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let context = InstallContext {
        clock: &clock,
        budget: WaitBudget::cli(clock.mono_now(), 30)?,
        cancel: &cancel,
    };
    let warning = warn_live_record_change(&roots, &context)
        .expect("a format change with live lanes in any project warns");
    assert!(warning.contains(&here.to_string()), "{warning}");
    assert!(warning.contains(&elsewhere.to_string()), "{warning}");
    Ok(())
}

#[test]
fn preflight_warning_reports_inspection_failure_instead_of_silence()
-> Result<(), Box<dyn std::error::Error>> {
    use super::*;
    use provenant_os::clock::SystemClock;

    let tmp = tempfile::tempdir()?;
    let roots = roots_with_receipt(tmp.path(), Some(provenant_records::LIVE_RECORD_FORMAT - 1))?;
    // A present but unreadable run database makes store inspection fail.
    let database = roots.state.as_path().join("run.sqlite3");
    std::fs::write(&database, b"not a database")?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o600))?;
    }

    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let context = InstallContext {
        clock: &clock,
        budget: WaitBudget::cli(clock.mono_now(), 30)?,
        cancel: &cancel,
    };
    let warning = warn_live_record_change(&roots, &context)
        .expect("an inspection failure must be a visible warning, not silence");
    assert!(
        warning.contains("could not inspect live attempts"),
        "{warning}"
    );
    Ok(())
}

#[test]
fn preflight_is_silent_when_format_matches() -> Result<(), Box<dyn std::error::Error>> {
    use super::*;
    use provenant_core::{AttemptId, ProjectId};
    use provenant_os::clock::SystemClock;

    let tmp = tempfile::tempdir()?;
    let roots = roots_with_receipt(tmp.path(), Some(provenant_records::LIVE_RECORD_FORMAT))?;
    create_live_attempt(
        &roots,
        ProjectId::from_bytes([1; 16]),
        AttemptId::from_bytes([4; 16]),
    )?;

    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let context = InstallContext {
        clock: &clock,
        budget: WaitBudget::cli(clock.mono_now(), 30)?,
        cancel: &cancel,
    };
    assert!(warn_live_record_change(&roots, &context).is_none());
    Ok(())
}

#[test]
fn preflight_is_silent_without_live_attempts() -> Result<(), Box<dyn std::error::Error>> {
    use super::*;
    use provenant_os::clock::SystemClock;

    let tmp = tempfile::tempdir()?;
    let roots = roots_with_receipt(tmp.path(), Some(provenant_records::LIVE_RECORD_FORMAT - 1))?;
    {
        use provenant_core::{ActorId, ProjectId};
        use provenant_run::{ExecutionContext, RunStore};

        let run_clock = SystemClock::new();
        let run_cancel = CancelToken::new();
        let run_call = provenant_run::Request {
            clock: &run_clock,
            cancel: &run_cancel,
        };
        RunStore::open(
            &ExecutionContext {
                state_root: roots.state.clone(),
                project_id: ProjectId::from_bytes([1; 16]),
                actor_id: ActorId::from_bytes([2; 16]),
                read_prompts: false,
            },
            &run_call,
        )?;
    }

    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let context = InstallContext {
        clock: &clock,
        budget: WaitBudget::cli(clock.mono_now(), 30)?,
        cancel: &cancel,
    };
    assert!(warn_live_record_change(&roots, &context).is_none());
    Ok(())
}

// Spec: docs/specs/state.md#releases-are-immutable
#[test]
fn verify_execs_the_installed_binary() -> Result<(), Box<dyn std::error::Error>> {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let home = root.join("home");
    let project = root.join("project");
    let state = root.join("state");
    for directory in [&home, &project, &state] {
        std::fs::create_dir(directory)?;
    }
    provenant_os::fixture::private_state_dir(&state)?;
    let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
    inputs.state_dir = Some(state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let environment = Resolved {
        assets: root.join("assets"),
        home: home.clone(),
        project: project.clone(),
        roots,
        claude_config: None,
        codex_home: None,
    };
    let mut targets = provenant_config::Targets::new(
        environment.roots.clone(),
        &home,
        &[],
        std::sync::Arc::new(provenant_records::policy::Policy::default()),
    );
    targets.bin_dir = root.join("bin");
    std::fs::create_dir(&targets.bin_dir)?;
    let program = installed_destination(&targets);
    let calls = root.join("verify-calls");
    std::fs::write(
        &program,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$0\" >> '{}'\nprintf '%s\\n' \"$*\" >> '{}'\n",
            calls.display(),
            calls.display()
        ),
    )?;
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))?;
    let runner = ChildRunner {
        executor: provenant_os::exec::SystemExecutor::new(1)?,
        environment: std::env::vars_os().collect(),
        cwd: home,
    };

    verify_installed_smoke(
        &runner,
        &environment,
        &targets,
        &cli::Upgrade {
            yes: false,
            from_source: None,
            hosts: "all".into(),
            verify: true,
            verify_only: false,
            dry_run: false,
            host: None,
            route: None,
            set: Vec::new(),
        },
    )?;

    let calls = std::fs::read_to_string(calls)?;
    let lines: Vec<_> = calls.lines().collect();
    assert_eq!(lines.len(), 2, "{calls}");
    assert_eq!(lines[0], program.to_str().ok_or("program path")?, "{calls}");
    assert!(
        lines[1].contains("upgrade --verify --verify-only"),
        "{calls}"
    );
    assert!(lines[1].contains("--json"), "{calls}");
    Ok(())
}

#[test]
fn verify_smoke_runs_outside_a_checkout_without_requiring_one()
-> Result<(), Box<dyn std::error::Error>> {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let home = root.join("home");
    // A plain directory, not a Git checkout: this is the cwd of a `upgrade
    // --verify` run from $HOME that used to make the smoke child refuse.
    let project = root.join("outside-checkout");
    let state = root.join("state");
    for directory in [&home, &project, &state] {
        std::fs::create_dir(directory)?;
    }
    provenant_os::fixture::private_state_dir(&state)?;
    let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
    inputs.state_dir = Some(state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let environment = Resolved {
        assets: root.join("assets"),
        home: home.clone(),
        project: project.clone(),
        roots,
        claude_config: None,
        codex_home: None,
    };
    let mut targets = provenant_config::Targets::new(
        environment.roots.clone(),
        &home,
        &[],
        std::sync::Arc::new(provenant_records::policy::Policy::default()),
    );
    targets.bin_dir = root.join("bin");
    std::fs::create_dir(&targets.bin_dir)?;
    let program = installed_destination(&targets);
    let calls = root.join("verify-calls");
    std::fs::write(
        &program,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n",
            calls.display()
        ),
    )?;
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))?;
    let runner = ChildRunner {
        executor: provenant_os::exec::SystemExecutor::new(1)?,
        environment: std::env::vars_os().collect(),
        cwd: home,
    };

    verify_installed_smoke(
        &runner,
        &environment,
        &targets,
        &cli::Upgrade {
            yes: false,
            from_source: None,
            hosts: "all".into(),
            verify: true,
            verify_only: false,
            dry_run: false,
            host: None,
            route: None,
            set: Vec::new(),
        },
    )?;

    let calls = std::fs::read_to_string(calls)?;
    assert!(calls.contains("upgrade --verify --verify-only"), "{calls}");
    assert!(calls.contains("--json"), "{calls}");
    // The verifier makes its own scratch checkout; passing this non-checkout
    // as --project would make the child refuse before the smoke could run.
    assert!(!calls.contains("--project"), "{calls}");
    Ok(())
}

// Spec: docs/specs/state.md#managed-install
#[test]
fn install_retires_the_cargo_target_seed_directory_once() -> Result<(), Box<dyn std::error::Error>>
{
    use super::*;

    let temp = tempfile::tempdir()?;
    let state = temp.path().join("state");
    assert!(
        retire_cargo_target_seeds(&state).is_none(),
        "a root with no retired directory is silent"
    );
    std::fs::create_dir(&state)?;
    assert!(retire_cargo_target_seeds(&state).is_none());

    let seeds = state.join(RETIRED_CARGO_TARGET_SEEDS);
    std::fs::create_dir_all(seeds.join("rustc"))?;
    std::fs::write(seeds.join("rustc/seed"), vec![b'x'; 2048])?;
    let report = retire_cargo_target_seeds(&state).expect("removal reports path and size");
    assert!(!seeds.exists(), "the retired directory is gone");
    assert!(report.contains(&seeds.display().to_string()), "{report}");
    assert!(report.contains("2048 bytes"), "{report}");
    assert!(
        retire_cargo_target_seeds(&state).is_none(),
        "the directory is reported once"
    );
    Ok(())
}

use super::*;

// The worktree driver includes this module without registering these tests.
#[allow(dead_code)]
fn smoke_program(
    root: &std::path::Path,
    status_summary: &str,
) -> Result<(std::path::PathBuf, std::path::PathBuf), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    let program = root.join("provenant");
    let calls = root.join("calls");
    let script = format!(
        r##"#!/bin/sh
printf '%s\n' "$*" >> '{calls}'
args="$*"
project=""
while [ $# -gt 0 ]; do
  case "$1" in
    --project) project="$2"; shift 2;;
    *) shift;;
  esac
done
nonce=$(sed -n 's/^nonce: //p' "$project/fixture.txt" 2>/dev/null || true)
summary="{status_summary}"
case "$args" in
  *' run smoke '*)
    printf '%s\n' '{{"schema_version":1,"command":"run","data":{{"status":"pending"}}}}'
    ;;
  *' wait '*)
    printf '%s\n' '{{"schema_version":1,"command":"wait","data":{{"status":"complete"}}}}'
    ;;
  *' status '*)
    printf '{{"schema_version":1,"command":"status","data":{{"rows":[{{"kind":"node","node_id":"read","state":"succeeded","result_summary":"%s"}},{{"kind":"node","node_id":"review-write","state":"succeeded","result_summary":"%s"}},{{"kind":"node","node_id":"write","state":"succeeded"}}]}}}}\n' "$summary" "$summary"
    ;;
  *) exit 77;;
esac
"##,
        calls = calls.display(),
        status_summary = status_summary
    );
    std::fs::write(&program, script)?;
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))?;
    Ok((program, calls))
}

#[allow(dead_code)]
fn smoke_runner(
    root: &std::path::Path,
    cwd: &std::path::Path,
) -> Result<ChildRunner, Box<dyn std::error::Error>> {
    Ok(ChildRunner {
        executor: provenant_os::exec::SystemExecutor::new(1)?,
        environment: std::env::vars_os().collect(),
        cwd: cwd.to_path_buf(),
    })
}

#[allow(dead_code)]
fn smoke_upgrade(host: Option<String>, set: Vec<String>) -> cli::Upgrade {
    cli::Upgrade {
        yes: false,
        from_source: None,
        hosts: "all".into(),
        verify: true,
        verify_only: false,
        dry_run: false,
        host,
        route: None,
        set,
    }
}

#[test]
fn smoke_cleanup_warnings_never_hide_or_replace_the_smoke_result() {
    use super::*;

    assert_eq!(
        crate::smoke::report_smoke_cleanup(Ok("smoke ok".to_owned()), Err("missing record".into())),
        Ok("smoke ok".to_owned())
    );
    let error = crate::smoke::report_smoke_cleanup::<String>(
        Err("node failed".to_owned()),
        Err("missing record".into()),
    )
    .expect_err("a failed smoke stays failed");
    assert!(error.contains("node failed"), "{error}");
    assert!(error.contains("missing record"), "{error}");
}

#[test]
// Spec: docs/specs/fabric.md#hermetic-smoke — the same nonce yields the same
// commits in two different homes, so a peer can reproduce the fixture.
fn smoke_fixture_is_deterministic_per_nonce() -> Result<(), Box<dyn std::error::Error>> {
    use super::*;

    let temp = tempfile::tempdir()?;
    let nonce = "0123456789ab";
    let mut commits = Vec::new();
    for name in ["home-a", "home-b"] {
        let home = temp.path().join(name);
        std::fs::create_dir(&home)?;
        let state = temp.path().join(format!("{name}-state"));
        provenant_os::fixture::private_state_dir(&state)?;
        commits.push(crate::smoke::create_fixture(&state, &home, nonce)?);
    }
    assert_eq!(commits[0], commits[1], "nonce must determine both OIDs");
    Ok(())
}

#[test]
// Spec: docs/specs/state.md#managed-install — a smoke run forgets its fixture
// binding and removes the directory on success, failure and refused launch.
fn smoke_removes_its_fixture_and_binding() -> Result<(), Box<dyn std::error::Error>> {
    use super::*;

    for (name, script) in [
        ("success", None),
        ("failure", Some("#!/bin/sh\nexit 42\n".to_owned())),
    ] {
        let temp = tempfile::tempdir()?;
        let home = temp.path().join("home");
        std::fs::create_dir(&home)?;
        let state = temp.path().join("state");
        provenant_os::fixture::private_state_dir(&state)?;
        let (program, calls) = match script {
            Some(_) => {
                let program = temp.path().join("provenant");
                let calls = temp.path().join("calls");
                std::fs::write(
                    &program,
                    format!(
                        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit 42\n",
                        calls.display()
                    ),
                )?;
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))?;
                (program, calls)
            }
            None => smoke_program(temp.path(), "nonce: $nonce")?,
        };
        let runner = smoke_runner(temp.path(), &home)?;
        let result = crate::smoke::run_lifecycle(
            &runner,
            &program,
            &state,
            &home,
            &smoke_upgrade(None, Vec::new()),
        );
        if name == "success" {
            result.map_err(|error| std::io::Error::other(format!("{name}: {error}")))?;
        } else {
            assert!(result.is_err(), "{name} must fail");
        }
        let recorded = std::fs::read_to_string(&calls).unwrap_or_default();
        let project = recorded
            .lines()
            .next()
            .and_then(|line| {
                line.split_once("--project ")
                    .map(|(_, rest)| rest.split_whitespace().next())
            })
            .map(std::path::PathBuf::from);
        let fixture =
            project.unwrap_or_else(|| crate::smoke::fixture_root(&home).join("000000000000"));
        assert!(!fixture.exists(), "{name}: fixture survived cleanup");
        let roots = crate::roots::child(&home, &home, state.clone())?;
        let bindings = provenant_config::registered_projects(&roots)?;
        assert!(
            bindings
                .iter()
                .all(|binding| !binding.common_dir().starts_with(&fixture)),
            "{name}: fixture binding survived cleanup: {bindings:?}"
        );
    }
    Ok(())
}

#[test]
// Spec: docs/specs/fabric.md#hermetic-smoke — the fixture replaces the caller
// checkout, however it was selected.
fn smoke_never_reads_the_caller_checkout() -> Result<(), Box<dyn std::error::Error>> {
    use super::*;

    let temp = tempfile::tempdir()?;
    let home = temp.path().join("home");
    std::fs::create_dir(&home)?;
    let state = temp.path().join("state");
    provenant_os::fixture::private_state_dir(&state)?;
    let (program, calls) = smoke_program(temp.path(), "nonce: $nonce")?;
    let repo = temp.path().join("caller");
    std::fs::create_dir(&repo)?;
    provenant_os::fixture::run_git(&repo, &["init", "-q", "-b", "main"])?;
    std::fs::write(repo.join("keep.txt"), "caller\n")?;
    provenant_os::fixture::run_git(&repo, &["add", "keep.txt"])?;
    provenant_os::fixture::run_git(&repo, &["commit", "-qm", "caller base"])?;
    std::fs::write(repo.join("keep.txt"), "caller head\n")?;
    provenant_os::fixture::run_git(&repo, &["commit", "-qam", "caller head"])?;
    let caller_head = String::from_utf8_lossy(&provenant_os::fixture::run_git(
        &repo,
        &["rev-parse", "HEAD"],
    )?)
    .trim()
    .to_owned();
    let detached = temp.path().join("detached");
    provenant_os::fixture::run_git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            detached.to_str().ok_or("path")?,
            "HEAD^",
        ],
    )?;
    let plain = temp.path().join("plain");
    std::fs::create_dir(&plain)?;
    for cwd in [repo.clone(), detached, plain] {
        std::fs::write(&calls, "")?;
        let runner = smoke_runner(temp.path(), &cwd)?;
        crate::smoke::run_lifecycle(
            &runner,
            &program,
            &state,
            &home,
            &smoke_upgrade(None, Vec::new()),
        )
        .map_err(|error| std::io::Error::other(error))?;
        let recorded = std::fs::read_to_string(&calls)?;
        let run = recorded.lines().next().ok_or("run argv")?;
        let project = run
            .split_once("--project ")
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .ok_or("project argument")?;
        assert!(
            std::path::Path::new(project).starts_with(crate::smoke::fixture_root(&home)),
            "{run} did not use the fixture root"
        );
        assert!(!run.contains(&caller_head), "{run} leaked the caller OID");
    }
    Ok(())
}

#[test]
// Spec: docs/specs/fabric.md#hermetic-smoke — a verdict without the nonce is
// a false pass and must fail verification.
fn smoke_fails_without_the_nonce() -> Result<(), Box<dyn std::error::Error>> {
    use super::*;

    let temp = tempfile::tempdir()?;
    let home = temp.path().join("home");
    std::fs::create_dir(&home)?;
    let state = temp.path().join("state");
    provenant_os::fixture::private_state_dir(&state)?;
    let (program, _) = smoke_program(temp.path(), "no nonce here")?;
    let runner = smoke_runner(temp.path(), &home)?;
    let error = crate::smoke::run_lifecycle(
        &runner,
        &program,
        &state,
        &home,
        &smoke_upgrade(None, Vec::new()),
    )
    .expect_err("the nonce is absent, so the smoke must fail");
    assert!(error.contains("read"), "{error}");
    Ok(())
}

#[test]
fn smoke_wait_and_status_name_the_fixture_project() -> Result<(), Box<dyn std::error::Error>> {
    use super::*;

    let temp = tempfile::tempdir()?;
    let home = temp.path().join("home");
    std::fs::create_dir(&home)?;
    let state = temp.path().join("state");
    provenant_os::fixture::private_state_dir(&state)?;
    let (program, calls) = smoke_program(temp.path(), "nonce: $nonce")?;
    let runner = smoke_runner(temp.path(), &home)?;
    let output = crate::smoke::run_lifecycle(
        &runner,
        &program,
        &state,
        &home,
        &smoke_upgrade(None, Vec::new()),
    )?;
    assert!(output.contains("complete"), "{output}");
    let calls = std::fs::read_to_string(calls)?;
    let lines: Vec<&str> = calls.lines().collect();
    assert_eq!(lines.len(), 3, "{calls}");
    assert!(lines[0].contains(" run smoke "), "{calls}");
    let project = lines[0]
        .split_once("--project ")
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .ok_or("run names its project")?;
    for line in &lines[1..] {
        assert!(line.contains(&format!("--project {project}")), "{line}");
    }
    Ok(())
}

#[test]
fn smoke_lifecycle_pins_the_review_to_the_fixture_head() -> Result<(), Box<dyn std::error::Error>> {
    use super::*;

    let temp = tempfile::tempdir()?;
    let home = temp.path().join("home");
    std::fs::create_dir(&home)?;
    let state = temp.path().join("state");
    provenant_os::fixture::private_state_dir(&state)?;
    let (program, calls) = smoke_program(temp.path(), "nonce: $nonce")?;
    let runner = smoke_runner(temp.path(), &home)?;
    crate::smoke::run_lifecycle(
        &runner,
        &program,
        &state,
        &home,
        &smoke_upgrade(None, Vec::new()),
    )?;
    let run = std::fs::read_to_string(calls)?
        .lines()
        .next()
        .ok_or("run argv")?
        .to_owned();
    let argument = |name: &str| -> Option<String> {
        run.split_whitespace()
            .collect::<Vec<_>>()
            .windows(2)
            .find(|pair| pair[0] == name)
            .map(|pair| pair[1].to_owned())
    };
    let head = argument("--base").ok_or("--base argument")?;
    assert_eq!(head.len(), 40, "{run}");
    let diff_base = run
        .split("--set diff_base=")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .ok_or("diff_base argument")?;
    let fixture_base = run
        .split("--set fixture_base=")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .ok_or("fixture_base argument")?;
    assert_eq!(diff_base, head, "{run}");
    assert_eq!(fixture_base.len(), 40, "{run}");
    assert_ne!(fixture_base, head, "{run}");
    Ok(())
}

#[test]
fn remote_smoke_carries_the_writer_branch_and_fixture_nonce()
-> Result<(), Box<dyn std::error::Error>> {
    use super::*;

    let temp = tempfile::tempdir()?;
    let home = temp.path().join("home");
    std::fs::create_dir(&home)?;
    let state = temp.path().join("state");
    provenant_os::fixture::private_state_dir(&state)?;
    let (program, calls) = smoke_program(temp.path(), "nonce: $nonce")?;
    let runner = smoke_runner(temp.path(), &home)?;
    crate::smoke::run_lifecycle(
        &runner,
        &program,
        &state,
        &home,
        &smoke_upgrade(None, Vec::new()),
    )?;
    let run = std::fs::read_to_string(calls)?
        .lines()
        .next()
        .ok_or("run argv")?
        .to_owned();
    assert!(run.contains(" run smoke "), "{run}");
    assert!(run.contains(" --branch smoke/"), "{run}");
    let project = run
        .split_once("--project ")
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .ok_or("project argument")?;
    let fixture_text = std::fs::read_to_string(std::path::Path::new(project).join("fixture.txt"))?;
    assert!(
        fixture_text.starts_with("nonce: ") && fixture_text.trim().len() == 19,
        "{fixture_text}"
    );
    Ok(())
}

#[test]
fn failed_smoke_node_is_reported_by_verify() -> Result<(), Box<dyn std::error::Error>> {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir()?;
    let home = temp.path().join("home");
    std::fs::create_dir(&home)?;
    let state = temp.path().join("state");
    provenant_os::fixture::private_state_dir(&state)?;
    let program = temp.path().join("provenant");
    std::fs::write(
        &program,
        "#!/bin/sh\ncase \" $* \" in *\" run smoke \"*) printf '%s\\n' '{\"node\":\"smoke/write\",\"state\":\"failed\"}' '{\"node\":\"smoke/retry-probe\",\"state\":\"failed\"}' '{\"node\":\"smoke/steer-probe\",\"state\":\"failed\"}'; exit 1;; esac\nexit 0\n",
    )?;
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))?;
    let runner = smoke_runner(temp.path(), &home)?;
    let error = crate::smoke::run_lifecycle(
        &runner,
        &program,
        &state,
        &home,
        &smoke_upgrade(None, Vec::new()),
    )
    .expect_err("a failed smoke node must fail verification");
    assert!(error.contains("\"node\":\"smoke/write\""), "{error}");
    assert!(error.contains("\"node\":\"smoke/retry-probe\""), "{error}");
    assert!(error.contains("\"node\":\"smoke/steer-probe\""), "{error}");
    Ok(())
}

#[test]
fn smoke_dry_run_previews_nodes_without_launching() -> Result<(), Box<dyn std::error::Error>> {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir()?;
    let home = temp.path().join("home");
    std::fs::create_dir(&home)?;
    let state = temp.path().join("state");
    provenant_os::fixture::private_state_dir(&state)?;
    let launcher = temp.path().join("provenant");
    let calls = temp.path().join("calls");
    std::fs::write(
        &launcher,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in *'run smoke --explain'*) printf '%s\\n' '{{\"nodes\":[{{\"id\":\"read\"}},{{\"id\":\"write\"}},{{\"id\":\"review-write\"}},{{\"id\":\"review-fixture\"}},{{\"id\":\"retry-probe\"}},{{\"id\":\"steer-probe\"}}]}}';; *) exit 77;; esac\n",
            calls.display()
        ),
    )?;
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))?;
    let runner = smoke_runner(temp.path(), &home)?;
    let output = super::preview_smoke(
        &runner,
        &launcher,
        &state,
        &home,
        &smoke_upgrade(None, Vec::new()),
    )?;
    let output = String::from_utf8(output)?;
    for node in [
        "read",
        "write",
        "review-write",
        "review-fixture",
        "retry-probe",
        "steer-probe",
    ] {
        assert!(
            output.contains(&format!("\"id\":\"{node}\"")),
            "{node}: {output}"
        );
    }
    let calls = std::fs::read_to_string(calls)?;
    assert_eq!(calls.lines().count(), 1, "{calls}");
    assert!(calls.contains("run smoke --explain"), "{calls}");
    Ok(())
}

#[test]
fn smoke_dry_run_keeps_a_user_diff_base_and_adds_fixture_base()
-> Result<(), Box<dyn std::error::Error>> {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir()?;
    let home = temp.path().join("home");
    std::fs::create_dir(&home)?;
    let state = temp.path().join("state");
    provenant_os::fixture::private_state_dir(&state)?;
    let launcher = temp.path().join("provenant");
    let calls = temp.path().join("calls");
    std::fs::write(
        &launcher,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in *'run smoke --explain'*) printf '%s\\n' '{{\"nodes\":[]}}';; *) exit 77;; esac\n",
            calls.display()
        ),
    )?;
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))?;
    let runner = smoke_runner(temp.path(), &home)?;
    let user_base = "2222222222222222222222222222222222222222";
    super::preview_smoke(
        &runner,
        &launcher,
        &state,
        &home,
        &smoke_upgrade(None, vec![format!("diff_base={user_base}")]),
    )?;
    let run = std::fs::read_to_string(calls)?
        .lines()
        .next()
        .ok_or("run argv")?
        .to_owned();
    assert_eq!(run.matches("--set diff_base=").count(), 1, "{run}");
    assert!(
        run.contains(&format!("--set diff_base={user_base}")),
        "{run}"
    );
    assert!(run.contains("--set fixture_base="), "{run}");
    Ok(())
}

#[test]
fn orphan_detection_keeps_default_environment_and_registered_targets()
-> Result<(), Box<dyn std::error::Error>> {
    use super::*;
    use provenant_records::{
        AbsolutePath, FileDigest, InstallReceipt, ResolvedHarnessTarget, SourceDigest,
    };
    let tmp = tempfile::tempdir()?;
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&project)?;
    let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
    inputs.state_dir = Some(tmp.path().join("state"));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let environment = Resolved {
        assets: home.clone(),
        home: home.clone(),
        project,
        roots: roots.clone(),
        claude_config: Some(home.join("environment")),
        codex_home: None,
    };
    let mut base = Targets::new(
        roots,
        &home,
        &[],
        std::sync::Arc::new(provenant_records::policy::Policy::default()),
    );
    base.accounts.push(provenant_config::AccountHome {
        harness: Harness::Claude,
        label: "registered".into(),
        home: home.join("registered"),
    });
    let target = |directory: &str| -> Result<ResolvedHarnessTarget, Box<dyn std::error::Error>> {
        let dir = home.join(directory);
        Ok(ResolvedHarnessTarget {
            extra: Default::default(),
            harness: Harness::Claude,
            skills_dir: AbsolutePath::new(&dir.join("skills"))?,
            config_file: AbsolutePath::new(&dir.join("settings.json"))?,
            hook_file: None,
            instruction: None,
            hooks_relinquished: false,
        })
    };
    let receipt = InstallReceipt {
        extra: Default::default(),
        schema_version: provenant_records::INSTALL_RECEIPT_SCHEMA_VERSION,
        state_format_version: provenant_records::STATE_FORMAT_VERSION,
        live_record_format: Some(provenant_records::LIVE_RECORD_FORMAT),
        source_digest: SourceDigest::from_bytes([1; 32]),
        manifest_digest: FileDigest::from_bytes([2; 32]),
        asset_digest: SourceDigest::from_bytes([3; 32]),
        target: "fixture".into(),
        toolchain: "fixture".into(),
        bin_dir: AbsolutePath::new(&home.join("bin"))?,
        harnesses: vec![Harness::Claude],
        harness_targets: vec![
            target(".claude")?,
            target("environment")?,
            target("registered")?,
            target("orphan")?,
        ],
        entries: vec![],
        migrations: Vec::new(),
    };
    let orphans = orphaned_targets(&environment, &base, &receipt);
    assert_eq!(orphans.len(), 1);
    assert_eq!(
        orphans[0].skills_dir.to_path_buf()?,
        home.join("orphan/skills")
    );
    Ok(())
}

#[test]
fn peer_agent_registry_name_uses_the_worktree_registry_key() {
    let path = std::path::Path::new("/worktrees/project-a");
    assert_eq!(
        super::peer_agent_registry_name(path),
        provenant_confinement::worktrees::state_dir_name(path)
    );
}

#[test]
fn post_shutdown_recovery_preserves_original_command() {
    use super::*;
    let mut reporter = PreviewReporter::default();
    assert!(reporter.stopped_report(Path::new("/state")).is_none());
    reporter.stopped("dev.provenant.peer");
    reporter.stopped("dev.provenant.tick");
    let report = reporter
        .stopped_report(Path::new("/state's data"))
        .expect("stopped agents");
    for name in ["dev.provenant.peer", "dev.provenant.tick"] {
        assert!(report.contains(name));
    }
    assert!(report.contains("'/state'\\''s data'"));
    assert!(report.contains("original install command"));
    assert!(!report.contains("--source"));
}

#[test]
fn peer_agent_removal_accepts_canonical_home_and_refuses_other_paths()
-> Result<(), Box<dyn std::error::Error>> {
    use super::*;
    use provenant_config::Disposition;
    use provenant_records::AbsolutePath;
    let tmp = tempfile::tempdir()?;
    let home = tmp.path().canonicalize()?.join("home");
    std::fs::create_dir(&home)?;
    let alias = tmp.path().join("home-alias");
    std::os::unix::fs::symlink(&home, &alias)?;
    let mut reporter = PreviewReporter {
        home: Some(alias),
        ..Default::default()
    };
    let mut action = Action {
        path: AbsolutePath::new(&provenant_fabric_runtime::peer_agent::plist(&home))?,
        disposition: Disposition::Remove,
        selector: "peer-agent".into(),
        before: None,
        after: None,
        identity: None,
        edits: Vec::new(),
    };
    assert!(
        reporter
            .before_remove_with(&action, |_| Err(io::Error::other("stop failed")))
            .is_err()
    );
    assert!(reporter.stopped_agents.is_empty());
    let report = reporter
        .stopped_report(Path::new("/state"))
        .expect("uncertain stop");
    assert!(report.contains("may still be running") && report.contains("dev.provenant.peer"));
    reporter.before_remove_with(&action, |_| Ok(false))?;
    assert!(reporter.stopped_agents.is_empty());
    let mut stops = 0;
    reporter.before_remove_with(&action, |cwd| {
        assert_eq!(cwd, home);
        stops += 1;
        Ok(true)
    })?;
    assert_eq!(stops, 1);
    action.path = AbsolutePath::new(&home.join("foreign.plist"))?;
    reporter.before_remove_with(&action, |cwd| {
        assert_eq!(cwd, home);
        stops += 1;
        Ok(true)
    })?;
    assert_eq!(stops, 2);
    action.before = Some(provenant_records::OwnedValue::File {
        extra: Default::default(),
        digest: provenant_records::CanonicalV1::file_digest(b"rendered plist\n"),
        mode: 0o600,
    });
    assert!(
        reporter
            .before_remove_with(&action, |_| {
                stops += 1;
                Ok(true)
            })
            .is_err()
    );
    assert_eq!(stops, 2);
    Ok(())
}

#[allow(
    dead_code,
    reason = "the custom worktree driver omits libtest functions"
)]
fn fixture_upgrade_host(host: &str, success: bool, output: &str) -> super::HostUpgrade {
    super::HostUpgrade {
        host: host.into(),
        success,
        output: output.into(),
        phase: super::HostPhase::Upgrade,
        app: None,
    }
}

#[allow(
    dead_code,
    reason = "the custom worktree driver omits libtest functions"
)]
fn fixture_smoke_host(success: bool, output: &str) -> super::HostUpgrade {
    super::HostUpgrade {
        host: "smoke".into(),
        success,
        output: output.into(),
        phase: super::HostPhase::Verify,
        app: None,
    }
}

/// A production-shaped `--json` envelope, serialised from the same typed
/// `CommandResult` the CLI emits. Fixtures must not hand-write the wire shape:
/// an envelope's error is top-level, never under `data`.
#[allow(
    dead_code,
    reason = "the custom worktree driver omits libtest functions"
)]
fn envelope(
    command: &str,
    status: provenant_records::Outcome,
    data: serde_json::Value,
    error: Option<provenant_records::CommandError>,
) -> String {
    let mut result = provenant_records::CommandResult::new(command, status, data);
    result.error = error;
    serde_json::to_string(&result).unwrap()
}

#[test]
fn remote_upgrade_script_resumes_and_retries_before_verifying() {
    let script = super::remote_upgrade_script("v2.9.4");
    assert!(script.starts_with("release_tag='v2.9.4'\n"), "{script}");
    assert!(script.contains("#!/bin/sh\n"), "{script}");

    // A dropped HTTP/2 stream must not abort the peer upgrade: resume the
    // partial file and retry inside a bounded budget.
    for flag in ["-C -", "--retry 5", "--retry-all-errors"] {
        assert!(script.contains(flag), "missing {flag} in {script}");
    }

    // Verification is downstream of the bounded download, and a checksum
    // mismatch still refuses: a partial file is never verified as complete.
    let download = script
        .find("download_artefact \"$archive\" \"$scratch/$archive\" package")
        .expect("bounded package download call");
    let verify = script
        .find("verify_artefact \"$archive\"")
        .expect("verification call");
    assert!(
        download < verify,
        "download {download} must precede verify {verify}"
    );
    // The checksum goes through the same bounded, resumable fetch rather than a
    // second single-shot curl that a dropped stream could abort.
    assert!(
        script.contains("download_artefact \"$artefact.sha256\" \"$scratch/checksum\" checksum"),
        "{script}"
    );
    // One helper body serves both artefacts: no copy of the retry loop remains.
    assert_eq!(
        script.matches("download_artefact() {").count(),
        1,
        "{script}"
    );
    assert!(script.contains("Release checksum mismatch"), "{script}");

    // A final failure names the bytes received and the elapsed seconds.
    assert!(
        script.contains("received %s bytes in %s seconds"),
        "{script}"
    );
}

#[test]
fn upgrade_refusal_names_each_failing_host_and_first_error_line() {
    let hosts = vec![
        fixture_upgrade_host("local", true, "install created 3 changes\n"),
        fixture_upgrade_host(
            "worker",
            false,
            "Command failed (Exit(17)): ssh: connection refused\nmore detail\n",
        ),
    ];
    let message = super::failed_hosts_message(&hosts);
    assert!(message.contains("1 host(s) failed:\n"), "{message}");
    assert!(
        message.contains("worker: ssh: connection refused\n"),
        "{message}"
    );
    // The successful host's detail and the executor wrapper never enter the
    // refusal; each failing host is named once.
    assert!(!message.contains("install created"), "{message}");
    assert!(!message.contains("Command failed"), "{message}");
    assert_eq!(message.matches("worker").count(), 1, "{message}");
    assert!(!message.contains("Some hosts failed"), "{message}");
}

#[test]
fn refusal_reports_the_innermost_error_from_a_verify_envelope() {
    // The real chain: the graph child's typed error envelope is embedded in the
    // verify child's `Command failed` report. The innermost code and message,
    // not the wrapper chain or a clipped raw envelope, must reach the refusal
    // and the host line.
    let child = envelope(
        "run",
        provenant_records::Outcome::Error,
        serde_json::json!({"graph": "smoke"}),
        Some(provenant_records::CommandError::new(
            "SMOKE_FAILED",
            "review gate returned VERDICT: FAIL",
            None,
            "fix the review gate",
        )),
    );
    let outer = envelope(
        "upgrade",
        provenant_records::Outcome::Error,
        serde_json::json!({"verify": "installed"}),
        Some(provenant_records::CommandError::new(
            "UPGRADE_VERIFY",
            format!("graph child failed: {child}"),
            None,
            "resolve the child failure",
        )),
    );
    let report = format!("Command failed (Exit(1)): {outer}\n");
    let hosts = vec![fixture_smoke_host(false, &report)];

    let view = super::render_upgrade_hosts(&hosts, false);
    let message = super::failed_hosts_message(&hosts);
    for rendered in [&view, &message] {
        assert!(
            rendered.contains("SMOKE_FAILED: review gate returned VERDICT: FAIL"),
            "{rendered}"
        );
        assert!(!rendered.contains("Command failed"), "{rendered}");
        assert!(!rendered.contains("UPGRADE_VERIFY"), "{rendered}");
        assert!(!rendered.contains("schema_version"), "{rendered}");
    }
}

#[test]
fn upgrade_failure_repair_points_at_json_per_host_detail() {
    // A failed envelope drops the human view, so the repair must send the
    // operator to the typed per-host detail, not to `--full`.
    let hosts = vec![fixture_smoke_host(false, "graph child failed\n")];
    let error = super::failed_upgrade_result(hosts).error.unwrap();
    assert_eq!(error.code, "UPGRADE_FAILED");
    let repair = error.repair.unwrap();
    assert!(
        repair.contains("provenant upgrade --verify --json"),
        "{repair}"
    );
    assert!(!repair.contains("--full"), "{repair}");
}

#[test]
fn refusal_bounds_each_reason_and_omits_success_detail() {
    let long = "x".repeat(2000);
    let report = envelope(
        "upgrade",
        provenant_records::Outcome::Error,
        serde_json::json!({"verify": "installed"}),
        Some(provenant_records::CommandError::new(
            "SMOKE_FAILED",
            long,
            None,
            "fix the smoke failure",
        )),
    );
    let hosts = vec![
        fixture_upgrade_host("local", true, "install created 3 changes\n"),
        fixture_smoke_host(false, &report),
    ];
    let message = super::failed_hosts_message(&hosts);
    let lines: Vec<_> = message.lines().collect();
    assert_eq!(lines.len(), 2, "{message}");
    assert_eq!(lines[0], "1 host(s) failed:", "{message}");
    let reason = lines[1];
    assert!(reason.starts_with("smoke: SMOKE_FAILED: "), "{reason}");
    assert!(!message.contains("install created"), "{message}");
    assert!(
        reason.len() <= "smoke: ".len() + super::report::HOST_REASON_LIMIT + 3,
        "unbounded reason ({}+ bytes): {reason}",
        reason.len()
    );
}

#[test]
fn upgrade_compact_human_leads_with_one_line_per_host_and_no_raw_json() {
    let notice =
        "  native_capability_unavailable: /cfg/a (instructions: unsupported (source: host))\n";
    let local = format!("{notice}{notice}{notice}install created 3 changes\n");
    // The verify child runs with --json and publishes the verdict as typed
    // data; the parent never re-parses clipped human envelopes.
    let smoke = format!(
        "{}\n",
        envelope(
            "upgrade",
            provenant_records::Outcome::Ok,
            serde_json::json!({
                "verify": "installed",
                "version": "2.9.1",
                "verdict": "VERDICT: PASS",
            }),
            None,
        )
    );
    let hosts = vec![
        fixture_upgrade_host("local", true, &local),
        fixture_upgrade_host(
            "worker",
            false,
            "Command failed (Exit(17)): ssh: connection refused\n",
        ),
        fixture_smoke_host(true, &smoke),
    ];
    let text = super::render_upgrade_hosts(&hosts, false);
    assert!(text.contains("local: upgraded\n"), "{text}");
    assert!(
        text.contains("worker: failed - ssh: connection refused\n"),
        "{text}"
    );
    assert!(text.contains("smoke: verified\n"), "{text}");
    assert!(text.contains("3 native capability notices"), "{text}");
    assert!(!text.contains("native_capability_unavailable:"), "{text}");
    assert!(!text.contains("\"schema_version\""), "{text}");
    assert!(text.contains("VERDICT: PASS"), "{text}");
}

#[test]
fn upgrade_full_human_keeps_capability_notices_and_smoke_envelopes() {
    let local = concat!(
        "  native_capability_unavailable: /cfg/a (instructions: unsupported)\n",
        "  native_capability_unavailable: /cfg/a (hooks: unsupported)\n"
    );
    let smoke = format!(
        "{}\n",
        envelope(
            "status",
            provenant_records::Outcome::Ok,
            serde_json::json!({"verdict": "VERDICT: PASS"}),
            None,
        )
    );
    let hosts = vec![
        fixture_upgrade_host("local", true, local),
        fixture_smoke_host(true, &smoke),
    ];
    let text = super::render_upgrade_hosts(&hosts, true);
    assert_eq!(
        text.matches("native_capability_unavailable:").count(),
        2,
        "{text}"
    );
    assert!(text.contains("\"schema_version\""), "{text}");
    assert!(!text.contains("use --full for detail"), "{text}");
}
