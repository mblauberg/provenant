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
    let runner = UpgradeRunner {
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
    let runner = UpgradeRunner {
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

// Spec: docs/specs/supervisor.md#tick-driven-settlement
#[test]
fn smoke_cleanup_forgets_the_scratch_project_binding() -> Result<(), Box<dyn std::error::Error>> {
    use super::*;
    use provenant_confinement::{UtilityBudget, UtilityRuntime};

    // $TMPDIR on a real macOS host is /var/folders, a symlink to
    // /private/var. The child binds the canonical common directory, so the
    // scratch path the parent holds and the binding id differ unless the
    // id is resolved through that canonical form.
    let temp = tempfile::tempdir()?;
    let real = temp.path().join("real");
    std::fs::create_dir(&real)?;
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&real, &link)?;
    let root = std::fs::canonicalize(&real)?;
    let mut inputs = provenant_core::RootInputs::new(root.join("home"), root.join("checkout"));
    inputs.state_dir = Some(root.join("state"));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    provenant_os::fixture::private_state_dir(roots.state.as_path())?;

    let scratch = crate::scratch_dir::ScratchDir::new_in(&link, "provenant-smoke-project-")?;
    provenant_os::fixture::run_git(scratch.path(), &["init", "-q"])?;
    std::fs::write(scratch.path().join("fixture.txt"), b"smoke fixture\n")?;
    // The warning #3812 saw comes from a scratch fixture's ports.json. It
    // must not be reachable through the host registry after cleanup.
    std::fs::write(scratch.path().join("ports.json"), b"not a port table\n")?;
    provenant_os::fixture::run_git(scratch.path(), &["add", "fixture.txt"])?;
    provenant_os::fixture::run_git(scratch.path(), &["commit", "-qm", "smoke fixture"])?;

    // Bind through the path the smoke child's `run` uses: `Context::open`
    // resolves the canonical common directory with the utility and binds
    // that location.
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    let git_home = roots.state.as_path().join("tmp/git-home");
    provenant_os::fixture::private_state_dir(&git_home)?;
    let runtime = UtilityRuntime::new(&git, &git_home, roots.state.as_path())?;
    let executor = provenant_os::exec::SystemExecutor::new(1)?;
    let discovered = runtime.project_location(
        &executor,
        scratch.path(),
        UtilityBudget::bounded(std::time::Duration::from_secs(5))?,
        &CancelToken::new(),
    )?;
    let binding = provenant_config::bind_project(&roots, discovered.location.clone())?;
    let record = provenant_config::project_binding_dir(roots.state.as_path(), binding.project_id())
        .join("project.json");
    assert!(
        record.is_file(),
        "the child path publishes {}",
        record.display()
    );

    let resolved = smoke_project_binding(roots.state.as_path(), scratch.path())?;
    assert_eq!(
        resolved,
        binding.project_id(),
        "host_project_dir must name the binding the child published"
    );

    // Cleanup cannot depend on the fixture still existing: the graph may
    // remove the scratch, or a preparation process may outlive the run.
    drop(scratch);
    forget_smoke_binding(roots.state.as_path(), resolved).map_err(std::io::Error::other)?;
    assert!(!record.exists(), "{} survived cleanup", record.display());
    assert!(provenant_config::lookup_project(&roots, &discovered.location)?.is_none());
    let registered = provenant_config::registered_projects(&roots)?;
    assert!(
        registered
            .iter()
            .all(|binding| !binding.common_dir().starts_with(&root)),
        "tick must not find a scratch fixture in the host registry: {registered:?}"
    );
    // A second cleanup is not an error: the smoke run may have created none.
    forget_smoke_binding(roots.state.as_path(), resolved).map_err(std::io::Error::other)?;
    Ok(())
}

#[test]
fn smoke_cleanup_reports_an_unresolved_binding() -> Result<(), Box<dyn std::error::Error>> {
    use super::*;

    let temp = tempfile::tempdir()?;
    let state = temp.path().join("state");
    provenant_os::fixture::private_state_dir(&state)?;
    let not_a_checkout = temp.path().join("not-a-checkout");
    std::fs::create_dir(&not_a_checkout)?;
    let error = smoke_project_binding(&state, &not_a_checkout)
        .expect_err("an unresolvable binding must be reported, never silent success");
    assert!(error.contains("cannot resolve"), "{error}");
    Ok(())
}

#[test]
fn smoke_cleanup_warnings_never_hide_or_replace_the_smoke_result() {
    use super::*;

    // A passed smoke stays passed, but the cleanup warning is part of a
    // failed smoke's report instead of being swallowed.
    assert_eq!(
        report_smoke_cleanup(Ok("smoke ok".to_owned()), Err("missing record".into())),
        Ok("smoke ok".to_owned())
    );
    let error =
        report_smoke_cleanup::<String>(Err("node failed".to_owned()), Err("missing record".into()))
            .expect_err("a failed smoke stays failed");
    assert!(error.contains("node failed"), "{error}");
    assert!(error.contains("missing record"), "{error}");
}

#[test]
fn smoke_wait_and_status_use_the_scratch_project() -> Result<(), Box<dyn std::error::Error>> {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir()?;
    let state = tmp.path().join("state");
    provenant_os::fixture::private_state_dir(&state)?;
    let home = tmp.path().join("home");
    std::fs::create_dir(&home)?;
    let scratch = tmp.path().join("scratch");
    std::fs::create_dir(&scratch)?;
    let calls = tmp.path().join("calls");
    let launcher = tmp.path().join("provenant");
    std::fs::write(
        &launcher,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \" $* \" in *' run smoke '*) printf '%s\\n' '{{\"status\":\"pending\"}}';; *' wait '*) printf '%s\\n' '{{\"status\":\"complete\"}}';; *' status '*) printf '%s\\n' '{{\"status\":\"complete\"}}';; *) exit 77;; esac\n",
            calls.display()
        ),
    )?;
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))?;
    let runner = UpgradeRunner {
        executor: provenant_os::exec::SystemExecutor::new(1)?,
        environment: std::env::vars_os().collect(),
        // The home directory is not the scratch project, so wait and
        // status only find the graph when they carry its --project.
        cwd: home.clone(),
    };
    let output = verify_smoke_with(
        &runner,
        &launcher,
        &state,
        &scratch,
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
    assert!(output.contains("complete"), "{output}");

    let calls = std::fs::read_to_string(&calls)?;
    let lines: Vec<&str> = calls.lines().collect();
    assert_eq!(lines.len(), 3, "{calls}");
    assert!(lines[0].contains(" run smoke "), "{calls}");
    let project = lines[0]
        .split_once("--project ")
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .expect("run names its project");
    for line in &lines[1..] {
        assert!(
            line.contains(&format!("--project {project}")),
            "wait and status must name the launched project: {line}"
        );
    }
    Ok(())
}

#[test]
fn smoke_verify_gives_the_review_a_non_empty_range() -> Result<(), Box<dyn std::error::Error>> {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir()?;
    let state = tmp.path().join("state");
    provenant_os::fixture::private_state_dir(&state)?;
    let scratch = tmp.path().join("scratch");
    std::fs::create_dir(&scratch)?;
    let calls = tmp.path().join("calls");
    let launcher = tmp.path().join("provenant");
    // The launch must name a base the live fixture can resolve to an
    // ancestor of HEAD; an empty range stops the run before any provider.
    std::fs::write(
        &launcher,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nargs=\"$*\"\nproject=\"\"\ndiff_base=\"\"\nwhile [ $# -gt 0 ]; do\n  case \"$1\" in\n    --project) project=\"$2\"; shift 2;;\n    --set) case \"$2\" in diff_base=*) diff_base=\"${{2#diff_base=}}\";; esac; shift 2;;\n    *) shift;;\n  esac\ndone\ncase \" $args \" in\n  *' run smoke '*)\n    [ -n \"$project\" ] || exit 90\n    [ -n \"$diff_base\" ] || exit 91\n    head=$(git -C \"$project\" rev-parse HEAD) || exit 92\n    base=$(git -C \"$project\" rev-parse \"$diff_base\") || exit 93\n    [ \"$base\" != \"$head\" ] || exit 94\n    [ \"$(git -C \"$project\" merge-base \"$base\" \"$head\")\" = \"$base\" ] || exit 95\n    printf '%s\\n' '{{\"status\":\"pending\"}}';;\n  *' wait '*) printf '%s\\n' '{{\"status\":\"complete\"}}';;\n  *' status '*) printf '%s\\n' '{{\"status\":\"complete\"}}';;\n  *) exit 77;;\nesac\n",
            calls.display()
        ),
    )?;
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))?;
    let runner = UpgradeRunner {
        executor: provenant_os::exec::SystemExecutor::new(1)?,
        environment: std::env::vars_os().collect(),
        cwd: tmp.path().to_owned(),
    };
    let output = verify_smoke_with(
        &runner,
        &launcher,
        &state,
        &scratch,
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
    assert!(output.contains("complete"), "{output}");
    let calls = std::fs::read_to_string(&calls)?;
    let run = calls.lines().next().ok_or("run call")?;
    assert!(run.contains(" run smoke "), "{calls}");
    assert!(run.contains(" --base HEAD "), "{calls}");
    assert!(run.contains(" --set diff_base="), "{calls}");
    Ok(())
}

#[test]
fn failed_smoke_node_is_reported_by_verify() -> Result<(), Box<dyn std::error::Error>> {
    // Spec: docs/specs/fabric.md#bundled-template-declarations — a failed
    // smoke node is reported by its own node id, including the retry-route
    // and steer-route probes.
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir()?;
    let state = tmp.path().join("state");
    std::fs::create_dir(&state)?;
    // The host state root is private in an installed host, and project
    // bindings refuse a public one.
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;
    let launcher = tmp.path().join("provenant");
    std::fs::write(
        &launcher,
        "#!/bin/sh\ncase \" $* \" in *\" run smoke \"*) printf '%s\\n' '{\"node\":\"smoke/write\",\"state\":\"failed\"}' '{\"node\":\"smoke/retry-route\",\"state\":\"failed\"}' '{\"node\":\"smoke/steer-route\",\"state\":\"failed\"}'; exit 1;; esac\nexit 0\n",
    )?;
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))?;
    let runner = UpgradeRunner {
        executor: provenant_os::exec::SystemExecutor::new(1)?,
        environment: std::env::vars_os().collect(),
        cwd: tmp.path().to_owned(),
    };
    let error = match verify_smoke_with(
        &runner,
        &launcher,
        &state,
        tmp.path(),
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
    ) {
        Err(error) => error,
        Ok(_) => {
            return Err(std::io::Error::other("failed smoke node unexpectedly succeeded").into());
        }
    };
    assert!(error.contains("\"node\":\"smoke/write\""));
    assert!(error.contains("\"node\":\"smoke/retry-route\""), "{error}");
    assert!(error.contains("\"node\":\"smoke/steer-route\""), "{error}");
    assert!(error.contains("failed"));
    Ok(())
}

#[test]
fn smoke_dry_run_previews_nodes_without_launching() -> Result<(), Box<dyn std::error::Error>> {
    // Spec: docs/specs/fabric.md#bundled-template-declarations — the dry run
    // lists every node the bundled smoke template declares, by node id.
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir()?;
    let state = tmp.path().join("state");
    std::fs::create_dir(&state)?;
    // The host state root is private in an installed host, and project
    // bindings refuse a public one.
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;
    let launcher = tmp.path().join("provenant");
    let calls = tmp.path().join("calls");
    std::fs::write(
        &launcher,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n[ \"$1\" = --project ] || exit 78\n[ \"$2\" != \"$PWD\" ] || exit 79\n[ -f \"$2/fixture.txt\" ] || exit 80\ngit -C \"$2\" rev-parse --verify HEAD >/dev/null || exit 81\ncase \"$*\" in *'run smoke --explain'*) printf '%s\\n' '{{\"nodes\":[{{\"id\":\"read\"}},{{\"id\":\"review\"}},{{\"id\":\"write\"}},{{\"id\":\"retry-route\"}},{{\"id\":\"steer-route\"}}]}}';; *) exit 77;; esac\n",
            calls.display()
        ),
    )?;
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))?;
    let runner = UpgradeRunner {
        executor: provenant_os::exec::SystemExecutor::new(1)?,
        environment: std::env::vars_os().collect(),
        cwd: tmp.path().to_owned(),
    };
    let output = preview_smoke(
        &runner,
        &launcher,
        &state,
        &cli::Upgrade {
            yes: false,
            from_source: None,
            hosts: "all".into(),
            verify: true,
            verify_only: false,
            dry_run: true,
            host: None,
            route: None,
            set: Vec::new(),
        },
    )?;
    let output = String::from_utf8(output)?;
    assert!(output.contains("\"read\""));
    assert!(output.contains("\"review\""));
    assert!(output.contains("\"write\""));
    assert!(output.contains("\"id\":\"retry-route\""), "{output}");
    assert!(output.contains("\"id\":\"steer-route\""), "{output}");
    let calls = std::fs::read_to_string(calls)?;
    assert_eq!(calls.lines().count(), 1, "{calls}");
    assert!(calls.contains("run smoke --explain"), "{calls}");
    Ok(())
}

#[test]
fn smoke_dry_run_pins_the_fixture_base_and_drops_a_user_base()
-> Result<(), Box<dyn std::error::Error>> {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir()?;
    let state = tmp.path().join("state");
    std::fs::create_dir(&state)?;
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;
    let launcher = tmp.path().join("provenant");
    let calls = tmp.path().join("calls");
    // The host owns the review range: the launch must carry the fixture's
    // own base (its first commit), never a caller-supplied one.
    std::fs::write(
        &launcher,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nargs=\"$*\"\nproject=\"\"\ndiff_base=\"\"\nwhile [ $# -gt 0 ]; do\n  case \"$1\" in\n    --project) project=\"$2\"; shift 2;;\n    --set) case \"$2\" in diff_base=*) diff_base=\"${{2#diff_base=}}\";; esac; shift 2;;\n    *) shift;;\n  esac\ndone\nexpected=$(git -C \"$project\" rev-parse HEAD^) || exit 90\n[ \"$diff_base\" = \"$expected\" ] || exit 91\ncase \"$args\" in\n  *'run smoke --explain'*) printf '%s\\n' '{{\"nodes\":[{{\"id\":\"read\"}},{{\"id\":\"review\"}},{{\"id\":\"write\"}}]}}';;\n  *) exit 77;;\nesac\n",
            calls.display()
        ),
    )?;
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))?;
    let runner = UpgradeRunner {
        executor: provenant_os::exec::SystemExecutor::new(1)?,
        environment: std::env::vars_os().collect(),
        cwd: tmp.path().to_owned(),
    };
    let output = preview_smoke(
        &runner,
        &launcher,
        &state,
        &cli::Upgrade {
            yes: false,
            from_source: None,
            hosts: "all".into(),
            verify: true,
            verify_only: false,
            dry_run: true,
            host: None,
            route: None,
            set: vec!["diff_base=2222222222222222222222222222222222222222".into()],
        },
    )?;
    assert!(String::from_utf8(output)?.contains("\"review\""));
    let calls = std::fs::read_to_string(calls)?;
    let run = calls.lines().next().ok_or("run call")?;
    assert!(run.contains("run smoke --explain"), "{calls}");
    assert_eq!(run.matches("--set diff_base=").count(), 1, "{calls}");
    assert!(
        !run.contains("2222222222222222222222222222222222222222"),
        "{calls}"
    );
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
        .find("download_package \"$archive\"")
        .expect("bounded download call");
    let verify = script
        .find("verify_artefact \"$archive\"")
        .expect("verification call");
    assert!(
        download < verify,
        "download {download} must precede verify {verify}"
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
