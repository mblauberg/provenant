#[path = "../tests/support/mod.rs"]
pub(super) mod support;
use provenant_core::Clock;
use provenant_supervisor::exec::{ExecOutcome, Executor};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::Write,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};
use support::{Result, TestRig, code};

// Only the harness=false target compiles these observations. The installed
// binary has no fixture flag, feature or environment override.
pub(crate) struct FreeWorkers;
impl provenant_confinement::worktrees::WorkerObserver for FreeWorkers {
    fn observe_roots(
        &self,
        roots: &[std::path::PathBuf],
    ) -> std::collections::BTreeMap<std::path::PathBuf, provenant_confinement::worktrees::WorkerState>
    {
        roots
            .iter()
            .cloned()
            .map(|root| (root, provenant_confinement::worktrees::WorkerState::Free))
            .collect()
    }
}

pub(crate) fn suite(args: &[std::ffi::OsString]) -> std::io::Result<()> {
    provenant_supervisor::fixture::run_libtest(
        args,
        &[
            "lane_prepare_isolates_nested_state",
            "read_only_dispatch_applies_bound_git_and_project_writes",
            "writer_dispatch_uses_lane_cache_and_narrow_provider_state",
            "writer_dispatch_edits_a_linked_worktree",
            "second_writer_is_refused_while_first_holds_the_worktree",
            "writer_terminal_refuses_sequencer_state",
            "writer_production_plan_denies_outside_writes",
            "writer_reconcile_refuses_unfinished_cherry_pick_and_releases",
            "create_list_and_remove_a_managed_worktree",
            "remove_a_managed_worktree_deletes_its_legacy_lane_cache",
            "merged_pruning_keeps_dirty_unmerged_and_out_of_scope_worktrees",
            "merged_pruning_deletes_a_merged_branch_without_a_worktree",
            "remove_by_name_follows_the_branch_merge_state",
            "remove_by_name_deletes_a_branch_fully_contained_in_integration",
            "remove_resolves_directory_names_and_remote_integration_refs",
            "escaping_branch_is_invalid_and_writes_no_checkout_outside_worktree_root",
            "dispatch_into_an_unregistered_worktree_names_the_create_command",
            "dispatch_into_an_unregistered_worktree_beside_registered_ones_names_the_create_command",
        ],
        |name| {
            let result = match name {
                "lane_prepare_isolates_nested_state" => lane_prepare_isolates_nested_state(),
                "read_only_dispatch_applies_bound_git_and_project_writes" => {
                    crate::compose_tests::read_only_dispatch_applies_bound_git_and_project_writes()
                }
                "writer_dispatch_uses_lane_cache_and_narrow_provider_state" => {
                    crate::compose_tests::writer_dispatch_uses_lane_cache_and_narrow_provider_state(
                    )
                }
                "writer_dispatch_edits_a_linked_worktree" => {
                    writer_dispatch_edits_a_linked_worktree()
                }
                "second_writer_is_refused_while_first_holds_the_worktree" => {
                    second_writer_is_refused_while_first_holds_the_worktree()
                }
                "writer_terminal_refuses_sequencer_state" => {
                    writer_terminal_refuses_sequencer_state()
                }
                "writer_production_plan_denies_outside_writes" => {
                    writer_production_plan_denies_outside_writes()
                }
                "writer_reconcile_refuses_unfinished_cherry_pick_and_releases" => {
                    writer_reconcile_refuses_unfinished_cherry_pick_and_releases()
                }
                "create_list_and_remove_a_managed_worktree" => {
                    create_list_and_remove_a_managed_worktree()
                }
                "remove_a_managed_worktree_deletes_its_legacy_lane_cache" => {
                    remove_a_managed_worktree_deletes_its_legacy_lane_cache()
                }
                "merged_pruning_keeps_dirty_unmerged_and_out_of_scope_worktrees" => {
                    merged_pruning_keeps_dirty_unmerged_and_out_of_scope_worktrees()
                }
                "merged_pruning_deletes_a_merged_branch_without_a_worktree" => {
                    merged_pruning_deletes_a_merged_branch_without_a_worktree()
                }
                "remove_by_name_follows_the_branch_merge_state" => {
                    remove_by_name_follows_the_branch_merge_state()
                }
                "remove_by_name_deletes_a_branch_fully_contained_in_integration" => {
                    remove_by_name_deletes_a_branch_fully_contained_in_integration()
                }
                "remove_resolves_directory_names_and_remote_integration_refs" => {
                    remove_resolves_directory_names_and_remote_integration_refs()
                }
                "dispatch_into_an_unregistered_worktree_names_the_create_command" => {
                    dispatch_into_an_unregistered_worktree_names_the_create_command()
                }
                "dispatch_into_an_unregistered_worktree_beside_registered_ones_names_the_create_command" => {
                    dispatch_into_an_unregistered_worktree_beside_registered_ones_names_the_create_command()
                }
                _ => escaping_branch_is_invalid_and_writes_no_checkout_outside_worktree_root(),
            };
            result.map_err(|error| std::io::Error::other(error.to_string()))
        },
    )
}

fn command(rig: &TestRig, args: &[&str]) -> Result<ExecOutcome> {
    let mut argv = vec![
        std::ffi::OsString::from("--state-dir"),
        rig.state.clone().into_os_string(),
    ];
    argv.extend(args.iter().map(std::ffi::OsString::from));
    rig.execute_os(&std::env::current_exe()?, &argv, None)
}

pub(super) fn git(rig: &TestRig, args: &[&str]) -> Result<String> {
    let output = rig.execute(Path::new("/usr/bin/git"), args, None)?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    Ok(String::from_utf8(output.stdout.bytes)?.trim().to_owned())
}

pub(super) fn repository() -> Result<TestRig> {
    let rig = TestRig::new()?;
    git(&rig, &["config", "user.name", "Fixture"])?;
    git(&rig, &["config", "user.email", "fixture@example.invalid"])?;
    git(&rig, &["checkout", "-q", "-b", "main"])?;
    fs::write(rig.project.join(".gitignore"), ".worktrees/\n")?;
    for name in [
        "AGENTS.md",
        "nested/AGENTS.md",
        ".claude/settings.json",
        ".github/workflows/check.yml",
    ] {
        let path = rig.project.join(name);
        fs::create_dir_all(path.parent().ok_or("fixture parent")?)?;
        fs::write(path, b"baseline instruction")?;
    }
    git(&rig, &["add", "."])?;
    git(&rig, &["commit", "-qm", "Initial"])?;
    Ok(rig)
}

pub(super) fn writer_backend_available() -> Result<bool> {
    if !cfg!(target_os = "macos") {
        return Ok(false);
    }
    let executor = provenant_supervisor::exec::SystemExecutor::new(1)?;
    let cancel = provenant_core::CancelToken::new();
    let backend = provenant_confinement::permissions::system_backend(
        &provenant_confinement::permissions::BackendContext {
            safe_fs: &provenant_supervisor::fs::LocalFs,
            executor: &executor,
            cancel: &cancel,
            #[cfg(target_os = "macos")]
            env: &[],
        },
    );
    match backend {
        Ok(_) => Ok(true),
        Err(error) if std::env::var_os("PROVENANT_CONFINEMENT_REQUIRED").is_none() => {
            writeln!(std::io::stderr(), "writer fixture skipped: {error}")?;
            Ok(false)
        }
        Err(error) => Err(error.into()),
    }
}

fn writer_dispatch_edits_a_linked_worktree() -> Result {
    if !writer_backend_available()? {
        return Ok(());
    }
    let rig = repository()?;
    ok(
        &rig,
        &["worktree", "create", "writer", "--base", "HEAD", "--json"],
    )?;
    let path = rig.project.join(".worktrees/writer");
    let (roots, context) = writer_context(&rig)?;
    let cleanup = writer_process_cleanup(&context);
    let store = open_writer_store(&context)?;
    let clock = provenant_supervisor::clock::SystemClock::new();
    let cancel = provenant_core::CancelToken::new();
    let call = writer_call(&clock, &cancel)?;
    let absolute =
        provenant_run::resolve_writer(context.state_root.as_path(), &rig.project, &path)?;
    let first = writer_run(absolute, "writer-first", 41, "write the fixture file")?;
    let first_attempt = first.attempts[0].attempt_id;
    let first_record = store.create(&first, &call)?;
    let provider = WriterProvider {
        state: context.state_root.as_path().to_owned(),
        home: fs::canonicalize(&rig.home)?,
        project: context.project_id,
        mode: "edit",
    };
    let holder = holder_context(&rig, &roots, &context);
    let started = start_writer(&store, first_record.run_id, &provider, &holder, &call)?;
    assert!(
        matches!(started.as_slice(), [provenant_run::Started::Launched(id)] if *id == first_attempt),
        "{started:?}"
    );
    let first_terminal = await_terminal(&store, first_attempt, &call)?;
    assert_terminal_outcome(&first_terminal, provenant_run::Outcome::Succeeded)?;
    assert_eq!(
        fs::read_to_string(path.join("writer-edited"))?,
        "writer fixture completed"
    );
    wait_for_writer_release(&store)?;

    // Fabric supplies absolute paths; the CLI also accepts repo-relative ones.
    let relative = provenant_run::resolve_writer(
        context.state_root.as_path(),
        &rig.project,
        Path::new(".worktrees/writer"),
    )?;
    let second = writer_run(relative, "writer-released", 42, "write after release")?;
    let second_attempt = second.attempts[0].attempt_id;
    let second_record = store.create(&second, &call)?;
    let started = start_writer(&store, second_record.run_id, &provider, &holder, &call)?;
    assert!(
        matches!(started.as_slice(), [provenant_run::Started::Launched(id)] if *id == second_attempt),
        "{started:?}"
    );
    let second_terminal = await_terminal(&store, second_attempt, &call)?;
    assert_terminal_outcome(&second_terminal, provenant_run::Outcome::Succeeded)?;
    wait_for_writer_release(&store)?;
    cleanup.disarm();
    Ok(())
}

fn second_writer_is_refused_while_first_holds_the_worktree() -> Result {
    if !writer_backend_available()? {
        return Ok(());
    }
    let rig = repository()?;
    ok(
        &rig,
        &["worktree", "create", "writer", "--base", "HEAD", "--json"],
    )?;
    let path = rig.project.join(".worktrees/writer");
    let (roots, context) = writer_context(&rig)?;
    let cleanup = writer_process_cleanup(&context);
    let store = open_writer_store(&context)?;
    let clock = provenant_supervisor::clock::SystemClock::new();
    let cancel = provenant_core::CancelToken::new();
    let call = writer_call(&clock, &cancel)?;
    let relative = provenant_run::resolve_writer(
        context.state_root.as_path(),
        &rig.project,
        Path::new(".worktrees/writer"),
    )?;
    let first = writer_run(relative.clone(), "writer-held", 51, "hold the writer lease")?;
    let first_attempt = first.attempts[0].attempt_id;
    let first_record = store.create(&first, &call)?;
    let release = path.join(".writer-fixture-release");
    let ready = path.join(".writer-fixture-ready");
    let _release_on_drop = ReleaseOnDrop(release.clone());
    let _ = fs::remove_file(&release);
    let _ = fs::remove_file(&ready);
    let first_provider = WriterProvider {
        state: context.state_root.as_path().to_owned(),
        home: fs::canonicalize(&rig.home)?,
        project: context.project_id,
        mode: "hold",
    };
    let holder = holder_context(&rig, &roots, &context);
    let started = start_writer(&store, first_record.run_id, &first_provider, &holder, &call)?;
    assert!(
        matches!(started.as_slice(), [provenant_run::Started::Launched(id)] if *id == first_attempt),
        "{started:?}"
    );
    wait_for_path(&ready, Duration::from_secs(8))?;

    let second = writer_run(relative, "writer-contender", 52, "contend for writer lease")?;
    let second_attempt = second.attempts[0].attempt_id;
    let second_record = store.create(&second, &call)?;
    let second_provider = WriterProvider {
        state: context.state_root.as_path().to_owned(),
        home: fs::canonicalize(&rig.home)?,
        project: context.project_id,
        mode: "edit",
    };
    let started = start_writer(
        &store,
        second_record.run_id,
        &second_provider,
        &holder,
        &call,
    )?;
    assert!(matches!(
        started.as_slice(),
        [provenant_run::Started::Refused { attempt, kind: provenant_run::RefusalKind::WorktreeBusy, .. }]
            if *attempt == second_attempt
    ));

    fs::write(&release, b"release")?;
    let first_terminal = await_terminal(&store, first_attempt, &call)?;
    assert_terminal_outcome(&first_terminal, provenant_run::Outcome::Succeeded)?;
    assert!(matches!(
        store.record(second_attempt, &call)?.state,
        provenant_run::AttemptState::Terminal {
            outcome: provenant_run::Outcome::Rejected,
            ..
        }
    ));
    wait_for_writer_release(&store)?;
    cleanup.disarm();
    Ok(())
}

fn writer_terminal_refuses_sequencer_state() -> Result {
    if !writer_backend_available()? {
        return Ok(());
    }
    let rig = repository()?;
    ok(
        &rig,
        &["worktree", "create", "writer", "--base", "HEAD", "--json"],
    )?;
    let path = rig.project.join(".worktrees/writer");
    let (roots, context) = writer_context(&rig)?;
    let cleanup = writer_process_cleanup(&context);
    let store = open_writer_store(&context)?;
    let clock = provenant_supervisor::clock::SystemClock::new();
    let cancel = provenant_core::CancelToken::new();
    let call = writer_call(&clock, &cancel)?;
    let request = provenant_run::resolve_writer(context.state_root.as_path(), &rig.project, &path)?;
    let writer =
        provenant_run::prepare_writer(&request, context.state_root.as_path(), context.project_id)?;
    // Simulate an unfinished Git operation without widening the stub's grant.
    fs::write(
        writer
            .worktree
            .repo()
            .private_git_dir()
            .join("CHERRY_PICK_HEAD"),
        b"fixture sequencer state",
    )?;
    let prepared = writer_run(request, "writer-sequencer", 61, "create sequencer state")?;
    let attempt = prepared.attempts[0].attempt_id;
    let record = store.create(&prepared, &call)?;
    let provider = WriterProvider {
        state: context.state_root.as_path().to_owned(),
        home: fs::canonicalize(&rig.home)?,
        project: context.project_id,
        mode: "control",
    };
    let holder = holder_context(&rig, &roots, &context);
    let started = start_writer(&store, record.run_id, &provider, &holder, &call)?;
    assert!(matches!(started.as_slice(), [provenant_run::Started::Launched(id)] if *id == attempt));
    let terminal = await_terminal(&store, attempt, &call)?;
    let provenant_run::AttemptState::Terminal {
        outcome, reason, ..
    } = terminal.state
    else {
        return Err("writer attempt was not terminal".into());
    };
    assert_eq!(outcome, provenant_run::Outcome::Failed);
    assert!(
        reason
            .as_deref()
            .is_some_and(|text| text.contains("writer terminal check"))
    );
    assert!(path.join(".git").is_file());
    let quarantine = context
        .state_root
        .as_path()
        .join("runs")
        .join(record.run_id.to_string())
        .join("attempts")
        .join(attempt.to_string())
        .join("quarantine");
    let manifest: Value = serde_json::from_slice(&fs::read(quarantine.join("manifest.json"))?)?;
    assert_eq!(manifest["review_required"], true);
    let evidence: Value = serde_json::from_slice(&fs::read(quarantine.join("evidence.json"))?)?;
    assert_eq!(evidence["review_required"], true);
    wait_for_writer_release(&store)?;

    // The same terminal boundary must refuse a rewind of the recorded base.
    fs::remove_file(
        writer
            .worktree
            .repo()
            .private_git_dir()
            .join("CHERRY_PICK_HEAD"),
    )?;
    let path_text = path.to_str().ok_or("fixture worktree encoding")?;
    git(&rig, &["-C", path_text, "reset", "--hard", "HEAD"])?;
    fs::write(path.join("source-base"), b"new base")?;
    git(&rig, &["-C", path_text, "add", "source-base"])?;
    git(&rig, &["-C", path_text, "commit", "-qm", "Writer base"])?;
    let request = provenant_run::resolve_writer(context.state_root.as_path(), &rig.project, &path)?;
    let prepared = writer_run(
        request,
        "writer-rewind",
        91,
        "hold while the base is rewritten",
    )?;
    let attempt = prepared.attempts[0].attempt_id;
    let record = store.create(&prepared, &call)?;
    let release = path.join(".writer-fixture-release");
    let _release_on_drop = ReleaseOnDrop(release.clone());
    let provider = WriterProvider {
        state: context.state_root.as_path().to_owned(),
        home: fs::canonicalize(&rig.home)?,
        project: context.project_id,
        mode: "hold",
    };
    let started = start_writer(&store, record.run_id, &provider, &holder, &call)?;
    assert!(
        matches!(started.as_slice(), [provenant_run::Started::Launched(id)] if *id == attempt),
        "{started:?}"
    );
    wait_for_path(&path.join(".writer-fixture-ready"), Duration::from_secs(8))?;
    git(&rig, &["-C", path_text, "reset", "--mixed", "HEAD^"])?;
    fs::write(release, b"release")?;
    let terminal = await_terminal(&store, attempt, &call)?;
    assert_terminal_outcome(&terminal, provenant_run::Outcome::Failed)?;
    assert!(
        matches!(terminal.state, provenant_run::AttemptState::Terminal { reason: Some(reason), .. } if reason.contains("no longer descends from its recorded base"))
    );
    wait_for_writer_release(&store)?;
    cleanup.disarm();
    Ok(())
}

fn writer_production_plan_denies_outside_writes() -> Result {
    if !writer_backend_available()? {
        return Ok(());
    }
    let rig = repository()?;
    for name in ["writer", "sibling"] {
        ok(
            &rig,
            &["worktree", "create", name, "--base", "HEAD", "--json"],
        )?;
    }
    let path = rig.project.join(".worktrees/writer");
    std::os::unix::fs::symlink(&rig.project, path.join("escape"))?;
    let (roots, context) = writer_context(&rig)?;
    let cleanup = writer_process_cleanup(&context);
    let store = open_writer_store(&context)?;
    let clock = provenant_supervisor::clock::SystemClock::new();
    let cancel = provenant_core::CancelToken::new();
    let call = writer_call(&clock, &cancel)?;
    let request = provenant_run::resolve_writer(context.state_root.as_path(), &rig.project, &path)?;
    let prepared = writer_run(
        request,
        "writer-negative",
        71,
        "probe the production write boundary",
    )?;
    let attempt = prepared.attempts[0].attempt_id;
    let record = store.create(&prepared, &call)?;
    let provider = WriterProvider {
        state: context.state_root.as_path().to_owned(),
        home: fs::canonicalize(&rig.home)?,
        project: context.project_id,
        mode: "negative",
    };
    let started = start_writer(
        &store,
        record.run_id,
        &provider,
        &holder_context(&rig, &roots, &context),
        &call,
    )?;
    assert!(
        matches!(started.as_slice(), [provenant_run::Started::Launched(id)] if *id == attempt),
        "{started:?}"
    );
    assert_terminal_outcome(
        &await_terminal(&store, attempt, &call)?,
        provenant_run::Outcome::Succeeded,
    )?;
    for target in [
        rig.project.join("negative-write"),
        rig.project.join(".worktrees/sibling/negative-write"),
        rig.project.join(".git/hooks/negative-hook"),
    ] {
        assert!(!target.exists(), "{}", target.display());
    }
    wait_for_writer_release(&store)?;
    cleanup.disarm();
    Ok(())
}

/// Crash only this fixture guardian after durable exit, before Run publication.
pub(crate) struct FixtureHolder<'a>(pub provenant_run::RunHolder<'a>);
impl provenant_supervisor::Holder for FixtureHolder<'_> {
    fn admit(
        &self,
        context: &[u8],
        attempt: &provenant_supervisor::fs::Directory,
    ) -> provenant_supervisor::custody::Result<()> {
        self.0.admit(context, attempt)
    }
    fn terminal(
        &self,
        context: &[u8],
        attempt: &provenant_supervisor::fs::Directory,
        exit: &provenant_supervisor::custody::ExitObservation,
        recovery: &provenant_supervisor::custody::Recovery,
    ) -> provenant_supervisor::custody::Result<()> {
        let parsed: provenant_run::HolderContext = serde_json::from_slice(context)
            .map_err(|_| provenant_supervisor::custody::CustodyError::InvalidRecord)?;
        if parsed
            .roots
            .checkout
            .join(".fixture-crash-terminal")
            .exists()
        {
            std::process::exit(70);
        }
        self.0.terminal(context, attempt, exit, recovery)
    }
}

fn writer_reconcile_refuses_unfinished_cherry_pick_and_releases() -> Result {
    if !writer_backend_available()? {
        return Ok(());
    }
    let rig = repository()?;
    ok(
        &rig,
        &["worktree", "create", "writer", "--base", "HEAD", "--json"],
    )?;
    fs::write(
        rig.project.join(".fixture-crash-terminal"),
        b"crash after exit",
    )?;
    let path = rig.project.join(".worktrees/writer");
    let (roots, context) = writer_context(&rig)?;
    let cleanup = writer_process_cleanup(&context);
    let store = open_writer_store(&context)?;
    let clock = provenant_supervisor::clock::SystemClock::new();
    let cancel = provenant_core::CancelToken::new();
    let call = writer_call(&clock, &cancel)?;
    let request = provenant_run::resolve_writer(context.state_root.as_path(), &rig.project, &path)?;
    let writer =
        provenant_run::prepare_writer(&request, context.state_root.as_path(), context.project_id)?;
    let prepared = writer_run(request, "writer-recovered", 81, "recover an exited writer")?;
    let attempt = prepared.attempts[0].attempt_id;
    let record = store.create(&prepared, &call)?;
    let provider = WriterProvider {
        state: context.state_root.as_path().to_owned(),
        home: fs::canonicalize(&rig.home)?,
        project: context.project_id,
        mode: "edit",
    };
    let started = start_writer(
        &store,
        record.run_id,
        &provider,
        &holder_context(&rig, &roots, &context),
        &call,
    )?;
    assert!(
        matches!(started.as_slice(), [provenant_run::Started::Launched(id)] if *id == attempt),
        "{started:?}"
    );
    let attempt_path = context
        .state_root
        .as_path()
        .join("runs")
        .join(record.run_id.to_string())
        .join("attempts")
        .join(attempt.to_string());
    wait_for_path(
        &attempt_path.join("process-exit.json"),
        Duration::from_secs(8),
    )?;
    assert!(!matches!(
        store.record(attempt, &call)?.state,
        provenant_run::AttemptState::Terminal { .. }
    ));
    // Incomplete quarantine must leave the attempt retryable, not seal a
    // terminal record whose missing manifest can never be released.
    let snapshot_path = attempt_path.join("quarantine/snapshot.json");
    let baseline = fs::read(&snapshot_path)?;
    fs::write(&snapshot_path, b"{}")?;
    let uncertain = store.reconcile(record.run_id, &provider, &call)?;
    assert!(
        matches!(uncertain.as_slice(), [(id, provenant_run::Reconciled::Uncertain { .. })] if *id == attempt)
    );
    assert!(!matches!(
        store.record(attempt, &call)?.state,
        provenant_run::AttemptState::Terminal { .. }
    ));
    assert!(!attempt_path.join("quarantine/manifest.json").exists());
    fs::write(snapshot_path, baseline)?;
    // The operation remains unfinished when the durable exit is recovered.
    fs::write(
        writer
            .worktree
            .repo()
            .private_git_dir()
            .join("CHERRY_PICK_HEAD"),
        b"unfinished cherry-pick",
    )?;
    let found = store.reconcile(record.run_id, &provider, &call)?;
    assert_eq!(found, vec![(attempt, provenant_run::Reconciled::Finished)]);
    let terminal = store.record(attempt, &call)?;
    assert_terminal_outcome(&terminal, provenant_run::Outcome::Failed)?;
    assert!(
        matches!(terminal.state, provenant_run::AttemptState::Terminal { reason: Some(reason), .. } if reason.contains("in-progress Git operation"))
    );
    assert!(attempt_path.join("release.json").is_file());
    assert!(store.writer_fences()?.is_empty());
    cleanup.disarm();
    Ok(())
}

pub(super) fn writer_process_cleanup(
    context: &provenant_run::ExecutionContext,
) -> provenant_supervisor::fixture::ProcessCleanupGuard {
    let root = context
        .state_root
        .as_path()
        .join("worktrees")
        .join(context.project_id.to_string());
    provenant_supervisor::fixture::ProcessCleanupGuard::new(&root)
}

fn start_writer(
    store: &provenant_run::RunStore,
    run: provenant_core::RunId,
    provider: &dyn provenant_run::Providers,
    holder: &dyn Fn(&provenant_run::AttemptRecord) -> provenant_run::Result<Vec<u8>>,
    call: &provenant_run::Request<'_>,
) -> Result<Vec<provenant_run::Started>> {
    Ok(store.start_with_writer_lease(
        run,
        provider,
        &|record, _| holder(record),
        &|directory, scope, binding| {
            provenant_supervisor::fixture::acquire_writer_with_binding(
                directory,
                scope,
                binding,
                provenant_core::WaitBudget::nonblocking(call.clock.mono_now()),
                call.cancel,
                call.clock,
            )
        },
        None,
        call,
    )?)
}

pub(super) fn writer_context(
    rig: &TestRig,
) -> Result<(provenant_core::Roots, provenant_run::ExecutionContext)> {
    let mut inputs = provenant_core::RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let roots = inputs.resolve(&provenant_supervisor::fs::LocalFs)?;
    let location = provenant_config::ProjectLocation::new(rig.project.join(".git"))?;
    let binding =
        provenant_config::bind_project(&roots, location, &provenant_supervisor::fs::LocalFs)?;
    let context = provenant_run::ExecutionContext {
        state_root: roots.state.clone(),
        project_id: binding.project_id(),
        actor_id: provenant_core::ActorId::from_bytes([62; 16]),
        read_prompts: true,
    };
    Ok((roots, context))
}

fn writer_call<'a>(
    clock: &'a provenant_supervisor::clock::SystemClock,
    cancel: &'a provenant_core::CancelToken,
) -> Result<provenant_run::Request<'a>> {
    Ok(provenant_run::Request {
        clock,
        budget: provenant_core::WaitBudget::cli(clock.mono_now(), 30)?,
        cancel,
    })
}

pub(super) fn open_writer_store(
    context: &provenant_run::ExecutionContext,
) -> Result<provenant_run::RunStore> {
    let clock = provenant_supervisor::clock::SystemClock::new();
    let cancel = provenant_core::CancelToken::new();
    let call = writer_call(&clock, &cancel)?;
    let mut store = provenant_run::RunStore::open(context, &call)?;
    store.set_admission_policy(provenant_run::AdmissionPolicy {
        startup_spacing_ms: 0,
        ..Default::default()
    });
    Ok(store)
}

fn writer_run(
    writer: provenant_run::WriterRequest,
    operation: &str,
    seed: u8,
    prompt: &str,
) -> Result<provenant_run::PreparedRun> {
    let task = provenant_core::TaskId::from_bytes([seed; 16]);
    let attempt = provenant_core::AttemptId::from_bytes([seed.wrapping_add(1); 16]);
    let prompt_digest = provenant_records::CanonicalV1::file_digest(prompt.as_bytes());
    let clock = provenant_supervisor::clock::SystemClock::new();
    Ok(provenant_run::PreparedRun {
        run_id: provenant_core::RunId::from_bytes([seed.wrapping_add(2); 16]),
        operation_id: provenant_core::OperationId::parse(operation)?,
        request: provenant_run::CanonicalRequest {
            schema_version: 1,
            tasks: vec![provenant_run::TaskRequest {
                account: None,
                profile: provenant_run::HarnessProfile::default(),
                task_id: task,
                prompt_digest,
                route: provenant_run::RequestedRoute::Model {
                    provider: "claude".into(),
                    model: "claude/test-model".into(),
                    effort: None,
                },
                fallback: Vec::new(),
                authority_digest: provenant_core::Digest::from_bytes([seed; 32]),
                writer: Some(writer),
                provider_timeout_seconds: 20,
                delivery_id: None,
                deliver: None,
                access: Default::default(),
            }],
            concurrency: 1,
            parent: None,
            lane_depth: Some(1),
            parent_attempt: None,
        },
        attempts: vec![provenant_run::NewAttempt {
            attempt_id: attempt,
            task_id: task,
            prompt: prompt.into(),
        }],
        created_at: clock.wall_now()?,
    })
}

struct WriterProvider {
    state: PathBuf,
    home: PathBuf,
    project: provenant_core::ProjectId,
    mode: &'static str,
}
impl provenant_run::Providers for WriterProvider {
    fn prepare(
        &self,
        task: &provenant_run::TaskRequest,
        _prompt: &str,
        paths: &provenant_run::LaunchPaths,
        resume: Option<&provenant_run::AttemptRecord>,
    ) -> std::result::Result<provenant_run::PreparedLaunch, String> {
        if resume.is_some() {
            return Err("writer fixture does not resume".into());
        }
        let request = task.writer.as_ref().ok_or("writer request missing")?;
        let writer = provenant_run::prepare_writer(request, &self.state, self.project)
            .map_err(|error| error.to_string())?;
        let executable =
            fs::canonicalize(std::env::current_exe().map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        let argv = vec![
            executable.clone().into_os_string(),
            OsString::from("--writer-provider"),
            OsString::from(self.mode),
            writer.worktree.path().as_os_str().to_owned(),
        ];
        let plan = provenant_cli::run::writer_fixture_plan(
            self.project,
            writer.worktree.path(),
            &self.home,
            paths,
            &writer,
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        )?;
        let profile = provenant_confinement::permissions::ProviderProfile {
            spec: provenant_confinement::permissions::ProviderProfileSpec {
                authentication: vec![provenant_confinement::permissions::AuthenticationProfile {
                    mechanism: "login".into(),
                    // A public executable stands in for a login file; no live
                    // credential is read and this adds no write permission.
                    roots: vec![provenant_confinement::permissions::PathGrant {
                        path: executable.clone(),
                        scope: provenant_confinement::permissions::PathScope::Exact,
                    }],
                    in_place_writes: Vec::new(),
                }],
                process_executables: vec![provenant_confinement::permissions::PathGrant {
                    path: executable,
                    scope: provenant_confinement::permissions::PathScope::Exact,
                }],
                ..Default::default()
            },
            observations: writer_observations(),
        };
        let route = fixture_route();
        Ok(provenant_run::PreparedLaunch {
            skipped_routes: Vec::new(),
            argv,
            environment: Vec::new(),
            stdin: Vec::new(),
            secret_input: None,
            cwd: writer.worktree.path().to_owned(),
            route,
            confinement: Some(provenant_run::LaunchConfinement {
                plan,
                profile,
                writer: Some(writer),
                home: self.home.clone(),
            }),
            observed_head: None,
        })
    }

    fn decode(
        &self,
        route: &provenant_run::RouteRecord,
        stdout: &[u8],
        exit: &provenant_supervisor::custody::ExitObservation,
        provider_state: &std::path::Path,
    ) -> std::result::Result<provenant_run::Decoded, String> {
        let registry = provenant_cli::run::Registry::default();
        provenant_run::Providers::decode(&registry, route, stdout, exit, provider_state)
    }
}

fn fixture_route() -> provenant_run::RouteRecord {
    provenant_run::RouteRecord {
        provider: Some("claude".into()),
        model: Some("claude/test-model".into()),
        effort: None,
        account_label: None,
        ..Default::default()
    }
}

fn writer_observations() -> BTreeMap<
    provenant_confinement::permissions::FixtureOperation,
    provenant_confinement::permissions::FixtureOutcome,
> {
    use provenant_confinement::permissions::{
        FixtureOperation as Operation, FixtureOutcome as Outcome,
    };
    [
        (Operation::DeclaredRead, Outcome::Allowed),
        (Operation::UndeclaredRead, Outcome::Denied),
        (Operation::UndeclaredWrite, Outcome::Denied),
        (Operation::SecretRead, Outcome::Denied),
        (Operation::ProtectedWrite, Outcome::Denied),
        (Operation::OtherNetwork, Outcome::Denied),
        (Operation::RuntimeRead, Outcome::Allowed),
        (Operation::AuthenticationRead, Outcome::Allowed),
        (Operation::ProtectedInstructionRead, Outcome::Allowed),
        (Operation::DeclaredProcessExec, Outcome::Allowed),
        (Operation::UndeclaredProcessExec, Outcome::Denied),
        (Operation::ProcessFork, Outcome::Denied),
        (Operation::WorktreeWrite, Outcome::Allowed),
        (Operation::GitIndexWrite, Outcome::Allowed),
        (Operation::GitHeadWrite, Outcome::Allowed),
        (Operation::GitObjectsWrite, Outcome::Allowed),
        (Operation::SelectedRefWrite, Outcome::Allowed),
        (Operation::GitConfigWrite, Outcome::Denied),
        (Operation::OtherRefWrite, Outcome::Denied),
    ]
    .into()
}

fn holder_context<'a>(
    rig: &'a TestRig,
    roots: &'a provenant_core::Roots,
    context: &'a provenant_run::ExecutionContext,
) -> impl Fn(&provenant_run::AttemptRecord) -> provenant_run::Result<Vec<u8>> + 'a {
    let seed = provenant_run::RootSeed {
        home: rig.home.clone(),
        checkout: rig.project.clone(),
        state_root: roots.state.as_path().to_owned(),
    };
    move |attempt| {
        provenant_run::HolderContext {
            schema_version: 1,
            roots: seed.clone(),
            project_id: attempt.project_id,
            actor_id: context.actor_id,
            run_id: attempt.run_id,
            attempt_id: attempt.attempt_id,
            host_delivery: None,
            launch: None,
            sealed: None,
        }
        .encode()
    }
}

fn await_terminal(
    store: &provenant_run::RunStore,
    attempt: provenant_core::AttemptId,
    call: &provenant_run::Request<'_>,
) -> Result<provenant_run::AttemptRecord> {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        let record = store.record(attempt, call)?;
        if matches!(record.state, provenant_run::AttemptState::Terminal { .. }) {
            return Ok(record);
        }
        if Instant::now() >= deadline {
            return Err("writer attempt did not reach terminal state".into());
        }
        thread::yield_now();
    }
}

fn assert_terminal_outcome(
    record: &provenant_run::AttemptRecord,
    expected: provenant_run::Outcome,
) -> Result {
    let provenant_run::AttemptState::Terminal { outcome, .. } = &record.state else {
        return Err("writer attempt was not terminal".into());
    };
    assert_eq!(*outcome, expected);
    Ok(())
}

pub(super) fn wait_for_writer_release(store: &provenant_run::RunStore) -> Result {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !store.writer_fences()?.is_empty() {
        if Instant::now() >= deadline {
            return Err("writer reservation was not released after terminal publication".into());
        }
        thread::yield_now();
    }
    Ok(())
}

fn wait_for_path(path: &Path, timeout: Duration) -> Result {
    let deadline = Instant::now() + timeout;
    while !path.exists() {
        if Instant::now() >= deadline {
            return Err("writer provider did not reach its hold point".into());
        }
        thread::yield_now();
    }
    Ok(())
}

fn pause(duration: Duration) {
    let deadline = Instant::now() + duration;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        thread::park_timeout(left);
    }
}

struct ReleaseOnDrop(PathBuf);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        let _ = fs::write(&self.0, b"release");
    }
}

/// Test-only child entry point. The parent launch test invokes the same binary
/// under the real supervisor and confinement plan; production builds have no
/// route to this fixture.
pub(crate) fn provider(args: &[OsString]) -> Result {
    if args.first().map(OsString::as_os_str) != Some(std::ffi::OsStr::new("--writer-provider")) {
        return Err("writer provider fixture switch missing".into());
    }
    let mode = args
        .get(1)
        .and_then(|value| value.to_str())
        .ok_or("writer provider fixture mode missing")?;
    let worktree = args
        .get(2)
        .map(PathBuf::from)
        .ok_or("writer provider worktree missing")?;
    match mode {
        "edit" => {}
        "control" => {
            for name in [
                "nested/AGENTS.md",
                ".claude/settings.json",
                ".github/workflows/check.yml",
            ] {
                fs::write(worktree.join(name), b"changed control")?;
            }
        }
        "negative" => {
            let primary = worktree
                .parent()
                .and_then(Path::parent)
                .ok_or("fixture primary")?;
            for target in [
                primary.join("negative-write"),
                primary.join(".worktrees/sibling/negative-write"),
                worktree.join("escape/negative-write"),
                primary.join(".git/config"),
                primary.join(".git/hooks/negative-hook"),
            ] {
                let error = fs::write(&target, b"forbidden").err().ok_or_else(|| {
                    format!("write escaped production plan: {}", target.display())
                })?;
                assert_eq!(
                    error.kind(),
                    std::io::ErrorKind::PermissionDenied,
                    "{}: {error}",
                    target.display()
                );
            }
        }
        "hold" => {
            let ready = worktree.join(".writer-fixture-ready");
            let release = worktree.join(".writer-fixture-release");
            fs::write(&ready, b"ready")?;
            let deadline = Instant::now() + Duration::from_secs(10);
            while !release.exists() {
                if Instant::now() >= deadline {
                    return Err("writer fixture hold timed out".into());
                }
                thread::yield_now();
            }
        }
        _ => return Err("unknown writer provider fixture mode".into()),
    }
    fs::write(worktree.join("writer-edited"), "writer fixture completed")?;
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    for event in [
        serde_json::json!({"type":"system","subtype":"init","session_id":"writer-fixture-session"}),
        serde_json::json!({"type":"assistant","message":{"model":"test-model","role":"assistant","content":[{"type":"text","text":"writer fixture completed"}]},"session_id":"writer-fixture-session"}),
        serde_json::json!({"type":"result","session_id":"writer-fixture-session","subtype":"success","is_error":false,"result":"writer fixture completed"}),
    ] {
        serde_json::to_writer(&mut output, &event)?;
        output.write_all(b"\n")?;
    }
    Ok(())
}

fn json(output: &ExecOutcome) -> Result<Value> {
    Ok(serde_json::from_slice(&output.stdout.bytes)?)
}

pub(super) fn ok(rig: &TestRig, args: &[&str]) -> Result<Value> {
    let output = command(rig, args)?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stdout.bytes)
    );
    let value = json(&output)?;
    assert_eq!(value["status"], "ok");
    Ok(value)
}

fn prepare_lane_cache(rig: &TestRig, path: &Path, branch: &str) -> Result<PathBuf> {
    use provenant_confinement::{
        permissions::{WorktreeWriteAuthority, prepare_writer_directories},
        repo::{GitRefName, verify_worktree},
        utility::{UtilityBudget, UtilityRuntime},
    };
    use provenant_supervisor::{exec::SystemExecutor, fs::LocalFs};

    let runtime = UtilityRuntime::new(
        std::sync::Arc::new(LocalFs),
        fs::canonicalize("/usr/bin/git")?,
        rig.state.join("utility-home"),
    )?;
    let executor = SystemExecutor::new(1)?;
    let repo = runtime.open_repo(
        &executor,
        &rig.project,
        UtilityBudget::bounded(Duration::from_secs(5))?,
        &provenant_core::CancelToken::new(),
    )?;
    let worktree = verify_worktree(&repo, path)?;
    let branch = GitRefName::new(format!("refs/heads/{branch}"))?;
    let authority = WorktreeWriteAuthority::for_worktree(&worktree, &branch, &rig.home)?;
    prepare_writer_directories(&LocalFs, &worktree, &branch, &authority, &rig.home)?;
    let cache = authority.cargo_cache().path.clone();
    fs::write(cache.join("artifact"), b"lane build output")?;
    Ok(cache)
}

fn create_list_and_remove_a_managed_worktree() -> Result {
    let rig = repository()?;
    let base = git(&rig, &["rev-parse", "HEAD"])?;
    let created = ok(
        &rig,
        &[
            "worktree",
            "create",
            "feature/topic",
            "--base",
            "HEAD",
            "--json",
        ],
    )?;
    assert_eq!(created["command"], "worktree create");
    assert_eq!(created["data"]["head"], base);
    assert!(rig.project.join(".worktrees/feature-topic/.git").is_file());
    let listed = ok(&rig, &["worktree", "list", "--json"])?;
    let trees = listed["data"]
        .as_array()
        .ok_or("worktree list is not an array")?;
    assert_eq!(trees.len(), 2);
    assert!(
        trees
            .iter()
            .any(|tree| tree["branch"] == "refs/heads/feature/topic" && tree["dirty"] == "clean")
    );
    let cache = prepare_lane_cache(
        &rig,
        &rig.project.join(".worktrees/feature-topic"),
        "feature/topic",
    )?;
    ok(&rig, &["worktree", "create", "feature/gone", "--json"])?;
    let gone = rig.project.join(".worktrees/feature-gone");
    let orphan = prepare_lane_cache(&rig, &gone, "feature/gone")?;
    git(
        &rig,
        &["worktree", "remove", gone.to_str().ok_or("worktree path")?],
    )?;
    assert!(!gone.exists());
    assert!(orphan.is_dir());
    let removed = ok(&rig, &["worktree", "remove", "feature/topic", "--json"])?;
    assert_eq!(
        removed["data"]["removed"]
            .as_array()
            .ok_or("missing removals")?
            .len(),
        1
    );
    assert!(!rig.project.join(".worktrees/feature-topic").exists());
    assert!(!cache.exists(), "orphaned lane cache remains after removal");
    assert!(
        orphan.is_dir(),
        "removing one worktree must preserve other orphaned caches"
    );
    assert_eq!(
        fs::read(orphan.join("artifact"))?,
        b"lane build output",
        "removing one worktree must preserve other orphaned caches"
    );
    assert_eq!(
        ok(&rig, &["worktree", "list", "--json"])?["data"]
            .as_array()
            .ok_or("missing list")?
            .len(),
        1
    );
    ok(&rig, &["gc", "--json"])?;
    assert!(!orphan.exists(), "gc must still sweep orphaned caches");
    rig.assert_legacy_unchanged()
}

fn remove_a_managed_worktree_deletes_its_legacy_lane_cache() -> Result {
    let rig = repository()?;
    ok(&rig, &["worktree", "create", "legacy", "--json"])?;
    let worktree = rig.project.join(".worktrees/legacy");
    let cache = prepare_lane_cache(&rig, &worktree, "legacy")?;
    fs::write(
        cache.join(".provenant-worktree-owner"),
        [
            b"provenant-lane-cargo-owner-v1\n".as_slice(),
            fs::canonicalize(&worktree)?.as_os_str().as_encoded_bytes(),
        ]
        .concat(),
    )?;
    ok(&rig, &["worktree", "remove", "legacy", "--json"])?;
    assert!(!worktree.exists());
    assert!(!cache.exists(), "legacy lane cache remains after removal");
    rig.assert_legacy_unchanged()
}

fn branch_commit(rig: &TestRig, branch: &str) -> Result<std::path::PathBuf> {
    ok(rig, &["worktree", "create", branch, "--json"])?;
    let path = rig.project.join(".worktrees").join(branch);
    fs::write(path.join(branch), branch)?;
    let text = path.to_str().ok_or("non-text fixture path")?;
    git(rig, &["-C", text, "add", branch])?;
    git(rig, &["-C", text, "commit", "-qm", branch])?;
    Ok(path)
}

fn merged_pruning_keeps_dirty_unmerged_and_out_of_scope_worktrees() -> Result {
    for since_scope in [false, true] {
        let rig = repository()?;
        let old = branch_commit(&rig, "old")?;
        git(&rig, &["merge", "--no-ff", "-qm", "Merge old", "old"])?;
        let bound = git(&rig, &["rev-parse", "HEAD"])?;
        let merged = branch_commit(&rig, "merged")?;
        git(&rig, &["merge", "--no-ff", "-qm", "Merge clean", "merged"])?;
        let dirty = branch_commit(&rig, "dirty")?;
        git(&rig, &["merge", "--no-ff", "-qm", "Merge dirty", "dirty"])?;
        fs::write(dirty.join("precious"), "uncommitted work")?;
        let unmerged = branch_commit(&rig, "unmerged")?;
        let mut args = vec![
            "worktree",
            "remove",
            "--merged",
            "--integration",
            "main",
            "--json",
        ];
        if since_scope {
            args.extend(["--since", &bound]);
        } else {
            args.extend([
                "--branch", "merged", "--branch", "dirty", "--branch", "unmerged",
            ]);
        }
        let result = ok(&rig, &args)?;
        assert_eq!(result["data"]["pruned_refs"], serde_json::json!(["merged"]));
        assert!(!merged.exists());
        assert!(old.is_dir());
        assert!(unmerged.is_dir());
        assert_eq!(
            fs::read_to_string(dirty.join("precious"))?,
            "uncommitted work"
        );
        assert_eq!(git(&rig, &["rev-parse", "old"])?.len(), 40);
        assert_eq!(git(&rig, &["rev-parse", "dirty"])?.len(), 40);
        assert_eq!(git(&rig, &["rev-parse", "unmerged"])?.len(), 40);
        let refused = command(&rig, &["worktree", "remove", "dirty", "--json"])?;
        assert_eq!(code(&refused), 1);
        assert_eq!(json(&refused)?["status"], "refused");
        assert!(dirty.join("precious").is_file());
        rig.assert_legacy_unchanged()?;
    }
    Ok(())
}

fn remove_by_name_deletes_a_branch_fully_contained_in_integration() -> Result {
    let rig = repository()?;
    // At the integration tip: nothing unique, so the name removal deletes it.
    ok(
        &rig,
        &["worktree", "create", "at-tip", "--base", "HEAD", "--json"],
    )?;
    let at_tip = rig.project.join(".worktrees/at-tip");
    let removed = ok(&rig, &["worktree", "remove", "at-tip", "--json"])?;
    assert_eq!(
        removed["data"]["pruned_refs"],
        serde_json::json!(["at-tip"])
    );
    assert!(!at_tip.exists());
    assert!(!has_branch(&rig, "at-tip")?);

    // An ancestor of a later integration tip: its commit is contained in main.
    let old = git(&rig, &["rev-parse", "HEAD"])?;
    ok(
        &rig,
        &["worktree", "create", "behind", "--base", &old, "--json"],
    )?;
    git(
        &rig,
        &["commit", "--allow-empty", "-qm", "advance integration"],
    )?;
    let removed = ok(&rig, &["worktree", "remove", "behind", "--json"])?;
    assert_eq!(
        removed["data"]["pruned_refs"],
        serde_json::json!(["behind"])
    );
    assert!(!has_branch(&rig, "behind")?);
    rig.assert_legacy_unchanged()
}

fn remove_resolves_directory_names_and_remote_integration_refs() -> Result {
    let rig = repository()?;
    // The worktree directory is the branch with `/` replaced by `-`.
    ok(
        &rig,
        &[
            "worktree",
            "create",
            "fix/shipped",
            "--base",
            "HEAD",
            "--json",
        ],
    )?;
    let shipped = rig.project.join(".worktrees/fix-shipped");
    let shipped_text = shipped.to_str().ok_or("non-text fixture path")?;
    fs::write(shipped.join("shipped.txt"), "work")?;
    git(&rig, &["-C", shipped_text, "add", "shipped.txt"])?;
    git(&rig, &["-C", shipped_text, "commit", "-qm", "shipped"])?;
    // Local main stays behind; the work is only on the remote-tracking ref.
    let tip = git(&rig, &["rev-parse", "fix/shipped"])?;
    git(&rig, &["update-ref", "refs/remotes/origin/main", &tip])?;

    let removed = ok(
        &rig,
        &[
            "worktree",
            "remove",
            "fix-shipped",
            "--integration",
            "origin/main",
            "--json",
        ],
    )?;
    assert_eq!(
        removed["data"]["pruned_refs"],
        serde_json::json!(["fix/shipped"])
    );
    assert!(!shipped.exists());
    assert!(!has_branch(&rig, "fix/shipped")?);

    // The merged sweep accepts the same remote-tracking integration ref.
    let swept = branch_commit(&rig, "swept")?;
    let swept_tip = git(&rig, &["rev-parse", "swept"])?;
    // A merge of the branch into the remote's main: contained, and not on the
    // first-parent history, so the sweep may delete it.
    let head = git(&rig, &["rev-parse", "HEAD"])?;
    let merge_tree = git(&rig, &["rev-parse", &format!("{head}^{{tree}}")])?;
    let next = git(
        &rig,
        &[
            "commit-tree",
            &merge_tree,
            "-p",
            &head,
            "-p",
            &swept_tip,
            "-m",
            "merge swept",
        ],
    )?;
    git(&rig, &["update-ref", "refs/remotes/origin/main", &next])?;
    let stale = git(&rig, &["rev-parse", "HEAD"])?;
    git(
        &rig,
        &[
            "worktree",
            "remove",
            "--force",
            swept.to_str().ok_or("path")?,
        ],
    )?;
    let result = ok(
        &rig,
        &[
            "worktree",
            "remove",
            "--merged",
            "--integration",
            "origin/main",
            "--branch",
            "swept",
            "--json",
        ],
    )?;
    assert_eq!(result["data"]["pruned_refs"], serde_json::json!(["swept"]));
    assert!(!has_branch(&rig, "swept")?);
    assert_eq!(git(&rig, &["rev-parse", "HEAD"])?, stale);
    rig.assert_legacy_unchanged()
}

fn has_branch(rig: &TestRig, name: &str) -> Result<bool> {
    let reference = format!("refs/heads/{name}");
    Ok(!git(rig, &["for-each-ref", reference.as_str()])?.is_empty())
}

fn remove_by_name_follows_the_branch_merge_state() -> Result {
    let rig = repository()?;
    let merged = branch_commit(&rig, "merged")?;
    git(&rig, &["merge", "--no-ff", "-qm", "Merge merged", "merged"])?;
    let unmerged = branch_commit(&rig, "unmerged")?;
    let dirty = branch_commit(&rig, "dirty")?;
    let dirty_head = git(&rig, &["rev-parse", "dirty"])?;
    fs::write(dirty.join("precious"), "uncommitted work")?;

    let refused = command(
        &rig,
        &["worktree", "remove", "dirty", "--discard", "--json"],
    )?;
    assert_eq!(code(&refused), 1);
    assert_eq!(json(&refused)?["status"], "refused");
    assert!(has_branch(&rig, "dirty")?);
    assert!(dirty.join("precious").is_file());

    let removed = ok(&rig, &["worktree", "remove", "merged", "--json"])?;
    assert_eq!(
        removed["data"]["pruned_refs"],
        serde_json::json!(["merged"])
    );
    assert!(!merged.exists());
    assert!(!has_branch(&rig, "merged")?);

    let kept = ok(&rig, &["worktree", "remove", "unmerged", "--json"])?;
    assert!(
        kept["data"]["pruned_refs"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );
    assert_eq!(kept["data"]["retained"][0]["branch"], "unmerged");
    assert_eq!(kept["data"]["retained"][0]["reason"], "not_merged");
    assert!(!unmerged.exists());
    assert!(has_branch(&rig, "unmerged")?);

    fs::remove_file(dirty.join("precious"))?;
    let discarded = ok(
        &rig,
        &["worktree", "remove", "dirty", "--discard", "--json"],
    )?;
    assert_eq!(discarded["data"]["discarded"][0]["branch"], "dirty");
    assert_eq!(discarded["data"]["discarded"][0]["head"], dirty_head);
    assert!(!dirty.exists());
    assert!(!has_branch(&rig, "dirty")?);
    rig.assert_legacy_unchanged()
}

fn merged_pruning_deletes_a_merged_branch_without_a_worktree() -> Result {
    let rig = repository()?;
    let stale = branch_commit(&rig, "stale")?;
    let stale_text = stale.to_str().ok_or("non-text fixture path")?;
    git(&rig, &["worktree", "remove", "--force", stale_text])?;
    git(&rig, &["merge", "--no-ff", "-qm", "Merge stale", "stale"])?;
    let pruned = ok(
        &rig,
        &[
            "worktree",
            "remove",
            "--merged",
            "--integration",
            "main",
            "--branch",
            "stale",
            "--json",
        ],
    )?;
    assert_eq!(pruned["data"]["pruned_refs"], serde_json::json!(["stale"]));
    assert!(!has_branch(&rig, "stale")?);
    rig.assert_legacy_unchanged()
}

fn dispatch_into_an_unregistered_worktree_names_the_create_command() -> Result {
    unregistered_worktree_dispatch_is_refused(false)
}

// Once another worktree is registered the project's state directory exists,
// so the missing record is the foreign worktree's own.
fn dispatch_into_an_unregistered_worktree_beside_registered_ones_names_the_create_command() -> Result
{
    unregistered_worktree_dispatch_is_refused(true)
}

fn unregistered_worktree_dispatch_is_refused(register_sibling: bool) -> Result {
    let rig = repository()?;
    if register_sibling {
        ok(
            &rig,
            &["worktree", "create", "managed", "--base", "HEAD", "--json"],
        )?;
    }
    git(
        &rig,
        &["worktree", "add", "-qb", "foreign", ".worktrees/foreign"],
    )?;
    let prompt = rig.root.join("prompt.txt");
    fs::write(&prompt, "hello")?;
    let refused = command(
        &rig,
        &[
            "dispatch",
            "--prompt-file",
            prompt.to_str().ok_or("prompt path")?,
            "--model",
            "codex/id",
            "--mode",
            "worktree-write",
            "--worktree",
            ".worktrees/foreign",
            "--operation-id",
            "foreign",
            "--json",
        ],
    )?;
    let value = json(&refused)?;
    assert_eq!(code(&refused), 1, "{value}");
    assert_eq!(value["status"], "refused", "{value}");
    assert_eq!(value["error"]["code"], "WORKTREE_UNREGISTERED", "{value}");
    let message = value["error"]["message"].as_str().ok_or("message")?;
    assert!(
        message.contains("provenant worktree create --base <branch> <branch>"),
        "{message}"
    );
    Ok(())
}

fn escaping_branch_is_invalid_and_writes_no_checkout_outside_worktree_root() -> Result {
    let rig = repository()?;
    let output = rig.command(&["worktree", "create", "../escape", "--json"])?;
    assert_eq!(code(&output), 2);
    assert_eq!(json(&output)?["status"], "invalid");
    assert!(!rig.project.join("escape").exists());
    assert!(!rig.root.join("escape").exists());
    assert!(!rig.project.join(".worktrees").exists());
    assert_eq!(git(&rig, &["branch", "--format=%(refname:short)"])?, "main");
    rig.assert_legacy_unchanged()
}

#[cfg(target_os = "macos")]
fn clone_rust_toolchain(rig: &TestRig, home: &Path) -> Result {
    // Build-time rustup identity locates tools only. The child HOME and all
    // mutable state stay in the rig. APFS clones avoid a toolchain-size copy.
    let rustup = option_env!("RUSTUP_HOME")
        .map(PathBuf::from)
        .or_else(|| option_env!("HOME").map(|home| PathBuf::from(home).join(".rustup")))
        .ok_or("build-time Rust home")?;
    let toolchain = option_env!("RUSTUP_TOOLCHAIN").ok_or("build-time Rust toolchain")?;
    let source = rustup.join("toolchains").join(toolchain);
    let target = home.join(".rustup/toolchains/fixture");
    let output = rig.execute_os(
        Path::new("/bin/cp"),
        &[
            "-cR".into(),
            source.into_os_string(),
            target.clone().into_os_string(),
        ],
        None,
    )?;
    assert_eq!(
        code(&output),
        0,
        "toolchain clone: {}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    for tool in ["cargo", "rustc"] {
        std::os::unix::fs::symlink(
            target.join("bin").join(tool),
            home.join(".cargo/bin").join(tool),
        )?;
    }
    Ok(())
}

fn lane_prepare_isolates_nested_state() -> Result {
    use provenant_confinement::permissions::PathScope;
    let rig = repository()?;
    ok(
        &rig,
        &["worktree", "create", "writer", "--base", "HEAD", "--json"],
    )?;
    ok(
        &rig,
        &[
            "worktree",
            "create",
            "nested-child",
            "--base",
            "HEAD",
            "--json",
        ],
    )?;
    let worktree = rig.project.join(".worktrees/writer");
    // The repository's Cargo config names a checked-in rustc wrapper.
    fs::create_dir_all(worktree.join(".cargo"))?;
    fs::write(
        worktree.join(".cargo/config.toml"),
        "[build]\nrustc-wrapper = \".cargo/rustc-wrapper.sh\"\n",
    )?;
    fs::write(
        worktree.join(".cargo/rustc-wrapper.sh"),
        "#!/bin/sh\nexec \"$@\"\n",
    )?;
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            worktree.join(".cargo/rustc-wrapper.sh"),
            fs::Permissions::from_mode(0o755),
        )?;
    }
    let worktree_arg = worktree.to_str().ok_or("worktree path")?;
    git(&rig, &["-C", worktree_arg, "add", ".cargo"])?;
    git(
        &rig,
        &[
            "-C",
            worktree_arg,
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "-q",
            "-m",
            "wrapper",
        ],
    )?;
    let (roots, context) = writer_context(&rig)?;
    let identity = command(&rig, &["fabric", "identity", "--json"])?;
    assert_eq!(
        code(&identity),
        0,
        "{}",
        String::from_utf8_lossy(&identity.stderr.bytes)
    );
    let home = rig.root.join("fixture-home");
    fs::create_dir_all(home.join(".claude-fixture"))?;
    fs::create_dir_all(home.join(".codex"))?;
    fs::write(
        home.join(".codex/auth.json"),
        br#"{"tokens":{"account_id":"fixture-one"}}"#,
    )?;
    for path in [".cargo/bin", ".cargo/registry", ".rustup/toolchains"] {
        fs::create_dir_all(home.join(path))?;
    }
    fs::write(home.join(".rustup/settings.toml"), b"fixture settings")?;
    for secret in ["credentials.toml", "credentials"] {
        fs::write(home.join(".cargo").join(secret), b"synthetic credential")?;
    }
    // Cargo fails when its user config exists but is unreadable, so lanes
    // read it; registry tokens belong in the denied credentials files.
    fs::write(home.join(".cargo/config.toml"), b"[net]\nretry = 1\n")?;
    fs::create_dir_all(home.join("Library/Keychains"))?;
    fs::write(
        home.join("Library/Keychains/login.keychain-db"),
        b"synthetic credential",
    )?;
    // Clone immutable tools into the isolated home. No test reads host
    // provider configuration, login state or Cargo credentials.
    #[cfg(target_os = "macos")]
    clone_rust_toolchain(&rig, &home)?;
    let executable = rig.root.join("provider/bin/codex");
    fs::create_dir_all(executable.parent().ok_or("provider parent")?)?;
    fs::copy(std::env::current_exe()?, &executable)?;
    // Production preparation needs registered, freshly observed accounts.
    let home = fs::canonicalize(&home)?;
    let executor = provenant_supervisor::exec::SystemExecutor::new(1)?;
    let mut accounts = String::new();
    let mut claude_key = String::new();
    for (adapter, label, directory) in [
        ("codex", "default", home.join(".codex")),
        ("claude", "fixture", home.join(".claude-fixture")),
    ] {
        let key = provenant_providers::accounts::detect(
            &executor,
            adapter,
            &executable,
            &home,
            "fixture".as_ref(),
            &directory,
        )?;
        if adapter == "claude" {
            claude_key.clone_from(&key);
        }
        accounts.push_str(&format!(
            "[domains.accounts.entries.{adapter}-{label}]\nadapter={adapter:?}\nlabel={label:?}\nkey={key:?}\ndirectory={:?}\n",
            directory.to_str().ok_or("account path")?
        ));
    }
    let config =
        |depth: u8| format!("schema_version=1\n[knobs]\n'lanes.max_depth'={depth}\n{accounts}");
    fs::write(rig.state.join("config.toml"), config(1))?;
    for (adapter, writer) in [
        ("codex", false),
        ("codex", true),
        ("claude", false),
        ("claude", true),
    ] {
        let request =
            provenant_run::resolve_writer(roots.state.as_path(), &rig.project, &worktree)?;
        let mut prepared = writer_run(
            request,
            &format!("lane-prepare-{adapter}-{writer}"),
            match (adapter, writer) {
                ("codex", false) => 91,
                ("codex", true) => 92,
                ("claude", false) => 93,
                _ => 94,
            },
            "fixture prompt",
        )?;
        let task = &mut prepared.request.tasks[0];
        if !writer {
            task.writer = None;
        }
        // Catalogue models only: production refuses unverified exact IDs.
        task.route = provenant_run::RequestedRoute::Model {
            provider: "claude".into(),
            model: "claude/claude-sonnet-5".into(),
            effort: None,
        };
        let attempt = roots.state.as_path().join(format!(
            "runs/{}/attempts/{}",
            prepared.run_id, prepared.attempts[0].attempt_id
        ));
        let paths = provenant_run::LaunchPaths {
            attempt_id: prepared.attempts[0].attempt_id,
            lane_depth: 1,
            run_id: prepared.run_id,
            operation_id: prepared.operation_id.clone(),
            task_index: 0,
            deadline_unix_ms: 4_000_000_000_000,
            scratch: attempt.join("scratch"),
            provider_state: attempt.join("provider-state"),
            temporary: attempt.join("tmp"),
        };
        // The active release root is its own directory, never the project.
        let release = rig.state.with_file_name("release");
        fs::create_dir_all(&release)?;
        let registry = provenant_cli::run::Registry {
            roots: Some(&roots),
            home: Some(&home),
            assets: Some(&release),
            cwd: Some(&rig.project),
            state_root: Some(roots.state.as_path()),
            project_id: Some(context.project_id),
            host: None,
        };
        // Simulate a partially written first candidate. Fallback must have
        // independent setup paths rather than failing on the same auth entry.
        task.fallback.push(provenant_run::RouteRecord {
            provider: Some(adapter.into()),
            model: Some(
                if adapter == "codex" {
                    "codex/gpt-6-luna"
                } else {
                    "claude/claude-haiku-5-5"
                }
                .into(),
            ),
            ..Default::default()
        });
        let clock = provenant_supervisor::clock::SystemClock::new();
        let cancel = provenant_core::CancelToken::new();
        let call = writer_call(&clock, &cancel)?;
        let store = provenant_run::RunStore::open(&context, &call)?;
        store.create(&prepared, &call)?;
        let task = &prepared.request.tasks[0];
        for path in [&paths.scratch, &paths.provider_state, &paths.temporary] {
            fs::create_dir_all(path)?;
        }
        let staging = &paths.temporary;
        // Setup repairs partial files in place (#1461), so a non-empty directory
        // fails the Claude primary on its settings path; a Codex fallback must
        // also not collide with a partial candidate-0 auth entry.
        for poison in [
            staging.join("candidate-0/provenant-settings.json/partial"),
            paths.provider_state.join("candidate-0/auth.json"),
        ] {
            fs::create_dir_all(poison.parent().ok_or("candidate parent")?)?;
            fs::write(&poison, b"partial setup")?;
        }
        // Bound to the registered account, as the host loader binds it.
        let token = provenant_providers::claude::refresh_for_account(
            &FixtureCredential,
            &provenant_providers::claude::Login {
                executable: &executable,
                home: &home,
                user: "fixture".as_ref(),
                config_dir: &home.join(".claude-fixture"),
            },
            &provenant_providers::identity::RegisteredAccount::new(
                "claude",
                &claude_key,
                home.join(".claude-fixture"),
            )?,
            &|| 1000,
            2000,
        )?;
        // Prepared below lanes.max_depth, so its children may launch; the
        // limit then drops to exercise refusal at dispatch time.
        fs::write(rig.state.join("config.toml"), config(2))?;
        let mut launch = provenant_cli::run::prepare_fixture(
            &registry,
            task,
            "fixture prompt",
            &paths,
            &executable,
            Some(&token),
        )
        .map_err(|e| format!("prepare {adapter} writer={writer}: {e}"))?;
        assert_eq!(launch.route.provider.as_deref(), Some(adapter));
        fs::write(rig.state.join("config.toml"), config(1))?;
        assert!(
            launch
                .environment
                .iter()
                .any(|(key, value)| key == provenant_run::LANE_DEPTH_ENV && value == "1")
        );
        let confinement = launch.confinement.as_ref().ok_or("confinement")?;
        let selected_git = fs::canonicalize(crate::roots::git_binary().ok_or("host Git")?)?;
        let scratch_git = confinement.plan.permissions().scratch.path.join("bin/git");
        if adapter == "claude" && writer {
            // Claude's env scrub drops GIT_CONFIG_GLOBAL, so scratch git wraps
            // the selected Git and sets it itself.
            let wrapper = fs::read_to_string(&scratch_git)?;
            assert!(wrapper.contains("GIT_CONFIG_GLOBAL=/dev/null"));
            assert!(wrapper.contains(&format!("exec '{}'", selected_git.display())));
            assert!(!fs::symlink_metadata(&scratch_git)?.file_type().is_symlink());
        } else {
            assert_eq!(fs::read_link(&scratch_git)?, selected_git);
        }
        assert!(
            launch
                .environment
                .iter()
                .any(|(key, value)| key == "PROVENANT_GIT" && value == selected_git.as_os_str())
        );
        assert!(
            confinement
                .profile
                .spec
                .process_executables
                .iter()
                .any(|grant| grant.path == selected_git && grant.scope == PathScope::Exact)
        );
        let permissions = confinement.plan.permissions();
        assert_eq!(permissions.scratch.path, staging.join("candidate-1"));

        if adapter == "codex" {
            assert!(
                fs::symlink_metadata(
                    permissions
                        .private_state
                        .as_ref()
                        .ok_or("private state")?
                        .path
                        .join("auth.json")
                )?
                .file_type()
                .is_symlink()
            );
        } else {
            let generated = permissions.scratch.path.join("provenant-mcp.json");
            let mcp: Value = serde_json::from_slice(&fs::read(generated)?)?;
            assert_eq!(
                mcp["mcpServers"]["fabric"]["env"]["PROVENANT_GIT"],
                selected_git.to_str().ok_or("Git path")?
            );
            assert_eq!(
                mcp["mcpServers"]["fabric"]["env"][provenant_run::LANE_DEPTH_ENV],
                "1"
            );
            let settings: Value = serde_json::from_slice(&fs::read(
                permissions.scratch.path.join("provenant-settings.json"),
            )?)?;
            assert!(
                settings["hooks"]
                    .as_object()
                    .is_some_and(|hooks| !hooks.is_empty())
            );
        }
        for grant in &confinement.profile.spec.state_writes {
            assert!(
                !grant.overlaps(&permissions.scratch),
                "scratch overlaps {grant:?}"
            );
            assert!(
                !grant.overlaps(permissions.private_state.as_ref().ok_or("private state")?),
                "private state overlaps {grant:?}"
            );
            for denied in [
                attempt.join("quarantine"),
                attempt.join("quarantine-result.json"),
                attempt.join("custody.lock"),
                roots
                    .state
                    .as_path()
                    .join("runs/other/attempts/other/custody.lock"),
            ] {
                assert!(
                    !(grant.path == denied
                        || (grant.scope == PathScope::Subtree && denied.starts_with(&grant.path))),
                    "custody writable through {grant:?}"
                );
            }
        }
        let frames = concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":999,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{},\"clientInfo\":{\"name\":\"fixture\",\"version\":\"1\"}}}\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"fabric_submit\",\"arguments\":{\"op\":\"submission\",\"operation_id\":\"nested-depth\",\"tasks\":[{\"prompt\":\"read only\",\"route\":{\"adapter\":\"codex\",\"model\":\"gpt-6-luna\"},\"timeout_ms\":1000}],\"concurrency\":1}}}\n"
        );
        let mut options = provenant_supervisor::exec::ExecOptions::new(
            &launch.argv,
            &launch.cwd,
            Duration::from_secs(10),
        );
        options.env = Some(&launch.environment);
        options.stdin = Some(frames.as_bytes());
        options.stdout = provenant_supervisor::exec::StreamPolicy::Capture { limit: 64 * 1024 };
        options.stderr = provenant_supervisor::exec::StreamPolicy::Capture { limit: 64 * 1024 };
        let output = provenant_supervisor::exec::SystemExecutor::new(1)?.execute(options)?;
        assert_eq!(
            code(&output),
            0,
            "{}",
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        let replies = output
            .stdout
            .bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(serde_json::from_slice::<Value>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let result = &replies
            .iter()
            .find(|reply| reply["id"] == 1)
            .ok_or("MCP reply")?["result"];
        assert_eq!(mcp_error_code(result), "lane_depth", "{result}");
        assert!(!roots.state.as_path().join("tmp/results").exists());
        assert!(
            rig.state
                .join("children")
                .join(paths.attempt_id.to_string())
                .join("tmp/results")
                .is_dir()
        );
        assert!(!worktree.join("provenant-mcp.json").exists());
        assert!(!worktree.join("provenant-settings.json").exists());
        assert_eq!(
            git(
                &rig,
                &[
                    "-C",
                    worktree.to_str().ok_or("worktree path")?,
                    "status",
                    "--porcelain"
                ]
            )?,
            ""
        );
        // Raise the host limit only for a second call. It must create records
        // in the configured child store without launching in the lane.
        if adapter == "codex" && !writer {
            fs::write(rig.state.join("config.toml"), config(2))?;
            let mut options = provenant_supervisor::exec::ExecOptions::new(
                &launch.argv,
                &launch.cwd,
                Duration::from_secs(10),
            );
            options.env = Some(&launch.environment);
            options.stdin = Some(frames.as_bytes());
            let output = provenant_supervisor::exec::SystemExecutor::new(1)?.execute(options)?;
            assert_eq!(
                code(&output),
                0,
                "{}",
                String::from_utf8_lossy(&output.stderr.bytes)
            );
            assert!(
                rig.state
                    .join("children")
                    .join(paths.attempt_id.to_string())
                    .join("run.sqlite3")
                    .exists()
            );
            let mut inputs = provenant_core::RootInputs::new(home.clone(), rig.project.clone());
            inputs.state_dir = Some(
                rig.state
                    .join("children")
                    .join(paths.attempt_id.to_string()),
            );
            let queued_context = provenant_run::ExecutionContext {
                state_root: inputs.resolve(&provenant_supervisor::fs::LocalFs)?.state,
                ..context.clone()
            };
            let queued = provenant_run::RunStore::open(&queued_context, &call)?;
            assert_eq!(
                queued.queued_runs(&call)?.len(),
                1,
                "lane launched its own child"
            );
            fs::write(rig.state.join("config.toml"), config(1))?;
        }
        let parent_context = provenant_run::HolderContext {
            schema_version: 1,
            roots: provenant_run::RootSeed {
                home: home.clone(),
                checkout: rig.project.clone(),
                state_root: rig.state.clone(),
            },
            project_id: context.project_id,
            actor_id: context.actor_id,
            run_id: prepared.run_id,
            attempt_id: paths.attempt_id,
            host_delivery: None,
            launch: Some(provenant_run::HostLaunchContext {
                configuration_root: rig.state.clone(),
                cwd: launch.cwd.clone(),
                executables: [("codex".into(), executable.clone())].into(),
                git: selected_git.clone(),
                codex_home: None,
                fixture_state_write: true,
            }),
            sealed: Some(provenant_run::SealedTask {
                request_digest: prepared.request.digest()?,
                task_id: task.task_id,
            }),
        };
        verify_lane_profile(
            &mut launch,
            &attempt,
            roots.state.as_path(),
            frames.as_bytes(),
            &config,
            &parent_context,
        )
        .map_err(|e| format!("confined {adapter} writer={writer}: {e}"))?;
    }
    Ok(())
}

/// Fake provider forwards no provider environment to its MCP child, only a
/// baseline PATH as real providers do.
pub(crate) fn lane_provider() -> Result {
    if std::env::var(provenant_run::LANE_DEPTH_ENV).as_deref() == Ok("2") {
        let mut prompt = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut prompt)?;
        let state = PathBuf::from(std::env::var_os("CODEX_HOME").ok_or("child CODEX_HOME")?);
        fs::write(
            state.join("fake-child-launched"),
            b"confined child executed",
        )?;
        writeln!(
            std::io::stdout(),
            "{{\"type\":\"thread.started\",\"thread_id\":\"fixture-child\"}}"
        )?;
        if prompt.contains("wait for host cancellation") {
            thread::park_timeout(Duration::from_secs(5));
        }
        return Ok(());
    }
    use provenant_supervisor::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor};
    let mut entry = serde_json::Map::new();
    for arg in std::env::args().filter(|arg| arg.starts_with("mcp_servers.fabric.")) {
        let value: Value = toml_edit::de::from_str(&arg)?;
        entry.extend(
            value["mcp_servers"]["fabric"]
                .as_object()
                .ok_or("MCP config")?
                .clone(),
        );
    }
    if entry.is_empty() {
        let args: Vec<_> = std::env::args_os().collect();
        let index = args
            .iter()
            .position(|arg| arg == "--mcp-config")
            .ok_or("MCP config flag")?;
        let config: Value =
            serde_json::from_slice(&fs::read(args.get(index + 1).ok_or("MCP config path")?)?)?;
        entry = config["mcpServers"]["fabric"]
            .as_object()
            .ok_or("MCP entry")?
            .clone();
    }
    let mut argv = vec![OsString::from(
        entry["command"].as_str().ok_or("MCP command")?,
    )];
    argv.extend(
        entry["args"]
            .as_array()
            .ok_or("MCP args")?
            .iter()
            .map(|arg| arg.as_str().map(OsString::from).ok_or("MCP arg"))
            .collect::<std::result::Result<Vec<_>, _>>()?,
    );
    let env = entry["env"]
        .as_object()
        .ok_or("MCP env")?
        .iter()
        .map(|(key, value)| {
            Ok((
                OsString::from(key),
                OsString::from(value.as_str().ok_or("MCP env value")?),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    // A real provider hands its MCP child a baseline PATH. Mirror that with
    // this fake's own directory first, so the nested dispatch resolves it.
    let mut env = env;
    if env.iter().all(|(key, _)| key != "PATH") {
        let mut directories = vec![
            std::env::current_exe()?
                .parent()
                .ok_or("provider bin")?
                .to_path_buf(),
        ];
        directories.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        env.push(("PATH".into(), std::env::join_paths(directories)?));
    }
    let mut input = Vec::new();
    std::io::Read::read_to_end(&mut std::io::stdin(), &mut input)?;
    let cwd = std::env::current_dir()?;
    let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(8));
    options.env = Some(&env);
    options.stdin = Some(&input);
    options.stdout = StreamPolicy::Capture { limit: 64 * 1024 };
    options.stderr = StreamPolicy::Capture { limit: 64 * 1024 };
    let output = SystemExecutor::new(1)?
        .execute(options)
        .map_err(|e| format!("MCP spawn: {e:?}"))?;
    std::io::stdout().write_all(&output.stdout.bytes)?;
    std::io::stderr().write_all(&output.stderr.bytes)?;
    if code(&output) != 0 {
        return Err("configured MCP child failed".into());
    }
    let stripped = env
        .iter()
        .filter(|(key, _)| {
            key != provenant_run::LANE_DEPTH_ENV && key != provenant_run::PARENT_ATTEMPT_ENV
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(8));
    options.env = Some(&stripped);
    options.stdin = Some(&input);
    let stripped_output = SystemExecutor::new(1)?.execute(options)?;
    assert_eq!(
        code(&stripped_output),
        0,
        "{}",
        String::from_utf8_lossy(&stripped_output.stderr.bytes)
    );
    let reply = stripped_output
        .stdout
        .bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(serde_json::from_slice::<Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(
        reply
            .iter()
            .find(|reply| reply["id"] == 1)
            .map(|reply| mcp_error_code(&reply["result"]))
            .ok_or("stripped MCP reply")?,
        "invalid_input"
    );
    // Exercise the same publisher used by CLI overflow under confinement.
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME")?);
    let assets = PathBuf::from(std::env::var_os("PROVENANT_ROOT").ok_or("assets")?);
    let mut inputs = provenant_core::RootInputs::new(home, assets);
    inputs.state_dir = Some(PathBuf::from(entry["args"][1].as_str().ok_or("state arg")?));
    let roots = inputs.resolve(&provenant_supervisor::fs::LocalFs)?;
    provenant_cli::fabric::publish(
        &provenant_records::CommandResult::new(
            "fixture",
            provenant_records::Outcome::Ok,
            "overflow",
        ),
        &roots,
    )?;
    Ok(())
}

fn verify_lane_profile(
    launch: &mut provenant_run::PreparedLaunch,
    attempt: &Path,
    state: &Path,
    frames: &[u8],
    config: &dyn Fn(u8) -> String,
    parent: &provenant_run::HolderContext,
) -> Result {
    use provenant_confinement::permissions::{
        AccessMode, AnchoredDirectory, Anchors, BackendContext, FixtureOperation, FixtureOutcome,
        prepare_writer_directories, required_anchors, system_backend,
    };
    use provenant_supervisor::{
        exec::{ExecOptions, StreamPolicy, SystemExecutor},
        fs::LocalFs,
    };
    let executor = SystemExecutor::new(1)?;
    let cancel = provenant_core::CancelToken::new();
    let backend = match system_backend(&BackendContext {
        safe_fs: &LocalFs,
        executor: &executor,
        cancel: &cancel,
        #[cfg(target_os = "macos")]
        env: &[],
    }) {
        Ok(backend) => backend,
        Err(error) if std::env::var_os("PROVENANT_CONFINEMENT_REQUIRED").is_none() => {
            writeln!(std::io::stderr(), "lane profile execution skipped: {error}")?;
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    let confinement = launch.confinement.as_mut().ok_or("confinement")?;
    // Synthetic fake-provider evidence exercises the compiler only. This does
    // not activate Codex or replace its missing live state-write recording.
    let profile = &mut confinement.profile;
    profile
        .observations
        .insert(FixtureOperation::StateWrite, FixtureOutcome::Allowed);
    let writer = confinement
        .writer
        .as_ref()
        .map(|w| (&w.worktree, &w.branch));
    // Run creates the writer's ref parents and lane cache under custody.
    if let (Some((worktree, branch)), AccessMode::WorktreeWrite(authority)) =
        (writer, &confinement.plan.permissions().mode)
    {
        prepare_writer_directories(&LocalFs, worktree, branch, authority, &confinement.home)?;
    }
    let directories = required_anchors(confinement.plan.permissions(), profile, writer)
        .iter()
        .map(|path| AnchoredDirectory::open_canonical(&LocalFs, path))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let anchors = Anchors {
        home: &confinement.home,
        directories: &directories,
        safe_fs: &LocalFs,
        worktree: writer,
    };
    let compiled = confinement
        .plan
        .compile(backend.as_ref(), profile, &anchors)?;
    // Start the same host holder used by internal supervise. This manually
    // compiled parent has no guardian of its own, so its fixture holder has no
    // admission or terminal work; all child handoff/custody remains real.
    struct ParentFixture;
    impl provenant_supervisor::Holder for ParentFixture {
        fn admit(
            &self,
            _: &[u8],
            _: &provenant_supervisor::fs::Directory,
        ) -> provenant_supervisor::custody::Result<()> {
            Ok(())
        }
        fn terminal(
            &self,
            _: &[u8],
            _: &provenant_supervisor::fs::Directory,
            _: &provenant_supervisor::custody::ExitObservation,
            _: &provenant_supervisor::custody::Recovery,
        ) -> provenant_supervisor::custody::Result<()> {
            Ok(())
        }
    }
    use provenant_supervisor::Holder;
    let host = provenant_cli::run::HostHolder::new(&ParentFixture);
    host.admit(
        &parent.encode()?,
        &provenant_supervisor::fs::Directory::open_anchor(attempt)?,
    )?;
    let shell_writer = launch
        .argv
        .iter()
        .any(|arg| arg == "Read,Grep,Glob,Edit,Write,Bash");
    // Native shell proof, separate from live provider/Bash/token-scrub proof.
    if cfg!(target_os = "macos") && shell_writer {
        let scratch = &confinement.plan.permissions().scratch.path;
        let project = scratch.join("cargo-probe");
        fs::create_dir_all(project.join("src"))?;
        fs::write(
            project.join("Cargo.toml"),
            "[package]\nname = 'lane-probe'\nversion = '0.1.0'\nedition = '2024'\n",
        )?;
        fs::write(
            project.join("src/lib.rs"),
            "#[test] fn writer_can_test() { assert_eq!(2 + 2, 4); }\n",
        )?;
        let argv = ["/bin/sh", "-c", concat!(
            "if /usr/bin/security help >/dev/null 2>&1; then exit 11; fi; ",
            "if /bin/cat \"$HOME/Library/Keychains/login.keychain-db\" >/dev/null 2>&1; then exit 12; fi; ",
            "for f in credentials.toml credentials; do ",
            "if /bin/cat \"$HOME/.cargo/$f\" >/dev/null 2>&1; then exit 13; fi; done; ",
            "if GIT_CONFIG_GLOBAL=/nonexistent/gitconfig git config --global --list >/dev/null 2>&1; then :; else exit 14; fi; ",
            "git -c user.name=lane -c user.email=lane@example.test commit -q --allow-empty -m probe && ",
            "test \"$(command -v git)\" = \"$TMPDIR/bin/git\" && ",
            "git --version && git status --porcelain && ",
            "cargo test --lib --offline --manifest-path \"$TMPDIR/cargo-probe/Cargo.toml\""
        )].map(OsString::from);
        let command = compiled.command(&argv, &provenant_core::AttemptId::from_bytes([0; 16]))?;
        let mut options = ExecOptions::new(&command, &launch.cwd, Duration::from_secs(30));
        options.env = Some(&launch.environment);
        let output = executor.execute(options)?;
        assert_eq!(
            code(&output),
            0,
            "shell probe: {}",
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        assert!(String::from_utf8_lossy(&output.stdout.bytes).contains("1 passed"));
    }
    // Start the configured MCP inside the compiled profile too: an
    // unconfined fake provider cannot detect ungranted startup writes.
    let mcp_argv = compiled.command(
        &launch.argv,
        &provenant_core::AttemptId::from_bytes([0; 16]),
    )?;
    let mut options = ExecOptions::new(&mcp_argv, &launch.cwd, Duration::from_secs(10));
    options.env = Some(&launch.environment);
    options.stdin = Some(frames);
    let output = executor.execute(options)?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    let replies = output
        .stdout
        .bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(serde_json::from_slice::<Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(
        replies
            .iter()
            .find(|reply| reply["id"] == 1)
            .map(|reply| mcp_error_code(&reply["result"]))
            .ok_or("confined MCP reply")?,
        "lane_depth"
    );
    // Admission below the limit must also open the isolated child store under
    // the profile, rather than only exercising the early depth refusal.
    let child = launch
        .environment
        .iter()
        .find(|(key, _)| key == provenant_core::RUN_STATE_DIRECTORY_ENV)
        .map(|(_, value)| PathBuf::from(value))
        .ok_or("child store environment")?;
    let run_count = || -> Result<usize> {
        match fs::read_dir(child.join("runs")) {
            Ok(entries) => Ok(entries.collect::<std::io::Result<Vec<_>>>()?.len()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(error.into()),
        }
    };
    // Startup spacing is host-wide: the child would otherwise wait out the
    // default 20 s after the parent's own start, past this test's deadline.
    // Clear only the last-start time; the lease counts stay real.
    let ledger = state.join("admission.sqlite3");
    if ledger.exists() {
        let db = rusqlite::Connection::open(&ledger)?;
        db.busy_timeout(Duration::from_secs(5))?;
        db.execute("DELETE FROM spacing", [])?;
    }
    fs::write(state.join("config.toml"), config(2))?;
    let mut inputs =
        provenant_core::RootInputs::new(confinement.home.clone(), parent.roots.checkout.clone());
    inputs.state_dir = Some(child.clone());
    let child_context = provenant_run::ExecutionContext {
        state_root: inputs.resolve(&LocalFs)?.state,
        project_id: parent.project_id,
        actor_id: parent.actor_id,
        read_prompts: true,
    };
    let clock = provenant_supervisor::clock::SystemClock::new();
    let call = writer_call(&clock, &cancel)?;
    // Admission is not launch: the host decides from its sealed parent task.
    let admit = |frames: &str| -> Result<provenant_core::AttemptId> {
        let before = run_count()?;
        let mut options = ExecOptions::new(&mcp_argv, &launch.cwd, Duration::from_secs(10));
        options.env = Some(&launch.environment);
        options.stdin = Some(frames.as_bytes());
        let nested = executor.execute(options)?;
        assert_eq!(
            code(&nested),
            0,
            "{}",
            String::from_utf8_lossy(&nested.stderr.bytes)
        );
        assert!(
            run_count()? > before,
            "confined child admission created no run"
        );
        // The child must be admitted as a target, not merely leave a record.
        let accepted = nested
            .stdout
            .bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(serde_json::from_slice::<Value>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let targets = &accepted
            .iter()
            .find(|reply| reply["id"] == 1)
            .ok_or("confined child reply")?["result"]["structuredContent"]["targets"];
        assert_eq!(
            targets.as_array().map(Vec::len),
            Some(1),
            "confined child was not admitted: {targets}"
        );
        Ok(provenant_core::AttemptId::parse(
            targets[0]["attempt"].as_str().ok_or("child attempt id")?,
        )?)
    };
    let nested_frames =
        String::from_utf8(frames.to_vec())?.replace("nested-depth", "confined-child");
    if confinement.writer.is_some() {
        // A writer lane cannot hand a writer child any registered worktree:
        // the host refuses the proposal before any provider launches.
        let proposal = admit(&nested_frames.replace("confined-child", "confined-writer").replace(
            "\"timeout_ms\":1000",
            "\"timeout_ms\":1000,\"access\":\"worktree_write\",\"worktree\":\".worktrees/nested-child\"",
        ))?;
        let store = provenant_run::RunStore::open(&child_context, &call)?;
        let refused = await_terminal(&store, proposal, &call)?;
        assert_terminal_outcome(&refused, provenant_run::Outcome::Rejected)?;
        assert_ne!(refused.provider_launched, Some(true));
        let provenant_run::AttemptState::Terminal { reason, .. } = &refused.state else {
            return Err("writer proposal was not terminal".into());
        };
        assert!(
            reason
                .as_deref()
                .is_some_and(|reason| reason.contains("a lane cannot propose a writer child")),
            "{reason:?}"
        );
    }
    let cancel_child =
        launch.route.provider.as_deref() == Some("codex") && confinement.writer.is_none();
    let nested_frames = if cancel_child {
        nested_frames
            .replace("read only", "wait for host cancellation")
            .replace("\"timeout_ms\":1000", "\"timeout_ms\":10000")
    } else {
        nested_frames
    };
    // The host must reach the confined fake child, including when a Claude
    // parent cannot read Codex credentials.
    let attempt_id = admit(&nested_frames)?;
    let store = provenant_run::RunStore::open(&child_context, &call)?;
    let forged = provenant_core::ActorId::from_bytes([66; 16]);
    if cancel_child {
        let run = store.record(attempt_id, &call)?.run_id;
        wait_for_path(
            &child.join(format!(
                "runs/{run}/attempts/{attempt_id}/provider-state/candidate-0/fake-child-launched"
            )),
            Duration::from_secs(8),
        )?;
        // The lane can write its child's store and attempt directory. It
        // marks the child finished, renames the requester in its launch
        // summary and plants a stop the guardian ignores. The host must still
        // stop the child, from the scope and requester it held at hand-off.
        let attempt = child.join(format!("runs/{run}/attempts/{attempt_id}"));
        let mut finished = store.record(attempt_id, &call)?;
        let mut sibling = finished.clone();
        finished.state = provenant_run::AttemptState::Terminal {
            outcome: provenant_run::Outcome::Rejected,
            exit_code: None,
            finished_at: finished.queued_at,
            reason: Some("rejected, says the lane".into()),
        };
        finished.started_at = None;
        let mut result = serde_json::to_value(&finished.prompt)?;
        result["path"]["path"] = format!("attempts/{attempt_id}/result.md").into();
        finished.result = Some(serde_json::from_value(result)?);
        rusqlite::Connection::open(child.join("run.sqlite3"))?.execute(
            "UPDATE attempts SET record=?1, terminal=1 WHERE id=?2",
            rusqlite::params![
                provenant_records::CanonicalV1::bytes(&finished)?.as_bytes(),
                attempt_id.to_string()
            ],
        )?;
        let path = attempt.join("launch.json");
        let mut summary: Value = serde_json::from_slice(&fs::read(&path)?)?;
        assert_eq!(summary["requester"], serde_json::to_value(parent.actor_id)?);
        summary["requester"] = serde_json::to_value(forged)?;
        fs::remove_file(&path)?;
        fs::write(&path, serde_json::to_vec(&summary)?)?;
        let scope = &summary["scope"];
        let planted = serde_json::json!({
            "schema_version": 1,
            "scope": scope,
            "request_id": format!("{run}.{attempt_id}.{}.cancel", scope["generation"]),
            "requester": forged,
            "reason": "explicit_cancellation",
        });
        fs::write(attempt.join("cancel.json"), serde_json::to_vec(&planted)?)?;
        // Scan reads of lane-written rows once ended the host's launcher
        // before its stop. First an undecodable attempt row, then a queued
        // sibling whose run the lane renames, so the owner check refuses it.
        let db = rusqlite::Connection::open(child.join("run.sqlite3"))?;
        let owner: Vec<u8> = db.query_row(
            "SELECT record FROM runs WHERE id=?1",
            [run.to_string()],
            |row| row.get(0),
        )?;
        let (project, task): (String, String) = db.query_row(
            "SELECT project, task FROM attempts WHERE id=?1",
            [attempt_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let garbage = provenant_core::AttemptId::from_bytes([67; 16]);
        db.execute(
            "INSERT INTO attempts(id,run,project,task,number,revision,terminal,record) VALUES (?1,?2,?3,?4,2,1,0,x'00')",
            rusqlite::params![garbage.to_string(), run.to_string(), project, task],
        )?;
        // Several of the launcher's 100 ms scans see it.
        pause(Duration::from_millis(500));
        db.execute("DELETE FROM attempts WHERE id=?1", [garbage.to_string()])?;
        sibling.attempt_id = provenant_core::AttemptId::from_bytes([68; 16]);
        sibling.state = provenant_run::AttemptState::Queued;
        sibling.generation = 0;
        sibling.attempt_number = 2;
        sibling.revision = 1;
        sibling.started_at = None;
        let mut prompt = serde_json::to_value(&sibling.prompt)?;
        prompt["path"]["path"] = prompt["path"]["path"]
            .as_str()
            .ok_or("prompt path")?
            .replace(&attempt_id.to_string(), &sibling.attempt_id.to_string())
            .into();
        sibling.prompt = serde_json::from_value(prompt)?;
        let mut renamed = store.run(run, &call)?;
        renamed.actor_id = forged;
        db.execute_batch("BEGIN")?;
        db.execute(
            "INSERT INTO attempts(id,run,project,task,number,revision,terminal,record) VALUES (?1,?2,?3,?4,2,1,0,?5)",
            rusqlite::params![
                sibling.attempt_id.to_string(),
                run.to_string(),
                project,
                task,
                provenant_records::CanonicalV1::bytes(&sibling)?.as_bytes()
            ],
        )?;
        db.execute(
            "UPDATE runs SET record=?1 WHERE id=?2",
            rusqlite::params![
                provenant_records::CanonicalV1::bytes(&renamed)?.as_bytes(),
                run.to_string()
            ],
        )?;
        db.execute_batch("COMMIT")?;
        // Several of the launcher's 100 ms scans see it.
        pause(Duration::from_millis(500));
        // The launcher reports the refused read, after it stops the child.
        let reported = host.provider_exited(
            &parent.encode()?,
            Some(provenant_supervisor::machine::StopReason::ExplicitCancellation),
        );
        // The forged terminal record stands, so the guardian's own exit is
        // the evidence. Unstopped, the child would run to its wall deadline.
        wait_for_path(&attempt.join("process-exit.json"), Duration::from_secs(8))?;
        let exit: Value = serde_json::from_slice(&fs::read(attempt.join("process-exit.json"))?)?;
        assert_eq!(exit["stop_reason"], "explicit_cancellation", "{exit}");
        let Err(provenant_supervisor::custody::CustodyError::Io(reported)) = reported else {
            return Err(format!("launcher hid the refused read: {reported:?}").into());
        };
        assert!(
            reported.to_string().contains("names another owner"),
            "{reported}"
        );
        db.execute(
            "DELETE FROM attempts WHERE id=?1",
            [sibling.attempt_id.to_string()],
        )?;
        db.execute(
            "UPDATE runs SET record=?1 WHERE id=?2",
            rusqlite::params![owner, run.to_string()],
        )?;
    }
    let record = await_terminal(&store, attempt_id, &call)?;
    let provenant_run::AttemptState::Terminal { reason, .. } = &record.state else {
        return Err("child attempt was not terminal".into());
    };
    let reason = reason.as_deref().unwrap_or_default();
    // Host setup links the registered credential. The parent's profile has
    // no cross-provider credential read.
    assert!(!reason.contains("setup failed"), "{reason}");
    if launch.route.provider.as_deref() == Some("codex") {
        let candidates = child.join(format!(
            "runs/{}/attempts/{attempt_id}/provider-state",
            record.run_id
        ));
        let linked = fs::read_dir(&candidates)?
            .map(|entry| Ok(fs::read_link(entry?.path().join("auth.json")).ok()))
            .collect::<Result<Vec<_>>>()?;
        assert_eq!(
            linked.into_iter().flatten().collect::<Vec<_>>(),
            vec![confinement.home.join(".codex/auth.json")],
            "{reason}"
        );
    }
    if !cancel_child {
        assert_eq!(record.provider_launched, Some(true), "{reason}");
    }
    // The lane's operation keeps its own actor. The host launches for, and
    // records as owner, the requester sealed in the parent's holder context.
    let actor: String = rusqlite::Connection::open(child.join("run.sqlite3"))?.query_row(
        "SELECT actor FROM operations WHERE run=?1",
        [record.run_id.to_string()],
        |row| row.get(0),
    )?;
    assert_ne!(actor, parent.actor_id.to_string());
    assert_eq!(store.run(record.run_id, &call)?.actor_id, parent.actor_id);
    assert_eq!(record.verified_owner, Some(parent.actor_id));
    if !cancel_child {
        let summary: Value = serde_json::from_slice(&fs::read(child.join(format!(
            "runs/{}/attempts/{attempt_id}/launch.json",
            record.run_id
        )))?)?;
        assert_eq!(summary["requester"], serde_json::to_value(parent.actor_id)?);
    }
    let provider_state = child.join(format!(
        "runs/{}/attempts/{attempt_id}/provider-state",
        record.run_id
    ));
    assert!(
        provider_state
            .join("candidate-0/fake-child-launched")
            .exists(),
        "{reason}"
    );
    host.provider_exited(&parent.encode()?, None)?;
    fs::write(state.join("config.toml"), config(1))?;
    // The child store's git-home is the in-lane fabric context's own, not the
    // host's: its preparation keeps its git home under the host root.
    assert!(child.join("tmp/git-home").is_dir());
    // Host probes leave no directory or lease behind once the child launched.
    let probes: Vec<_> = fs::read_dir(state)?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| name.starts_with("probe-"))
        .collect();
    assert!(probes.is_empty(), "{probes:?}");
    let denied = [
        attempt.join("custody.lock"),
        attempt.join("reserve"),
        attempt.join("quarantine/probe"),
        attempt.join("quarantine-result.json"),
        attempt.join("process.json"),
        state.join("runs/other/attempts/other/process.json"),
    ];
    for path in &denied {
        fs::create_dir_all(path.parent().ok_or("custody parent")?)?;
        fs::write(path, b"sealed custody")?;
    }
    let marker = confinement
        .plan
        .permissions()
        .scratch
        .path
        .join("allowed-write");
    let mut argv: Vec<OsString> = vec!["/bin/sh".into(), "-c".into(),
        "printf allowed > \"$1\" || exit 10; shift; for target in \"$@\"; do if printf changed > \"$target\"; then exit 20; fi; done".into(), "probe".into(), marker.as_os_str().to_owned()];
    argv.extend(denied.iter().map(|path| path.as_os_str().to_owned()));
    argv = compiled.command(&argv, &provenant_core::AttemptId::from_bytes([0; 16]))?;
    let mut options = ExecOptions::new(&argv, &launch.cwd, Duration::from_secs(5));
    options.env = Some(&launch.environment);
    options.stderr = StreamPolicy::Capture { limit: 4096 };
    let output = executor.execute(options)?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    assert_eq!(fs::read(marker)?, b"allowed");
    for path in denied {
        assert_eq!(fs::read(path)?, b"sealed custody");
    }
    Ok(())
}

/// The credential process boundary returns synthetic bytes and runs nothing.
struct FixtureCredential;
impl Executor for FixtureCredential {
    fn execute(
        &self,
        options: provenant_supervisor::exec::ExecOptions<'_>,
    ) -> std::result::Result<ExecOutcome, provenant_supervisor::exec::ExecError> {
        // `auth status` answers the identity check; anything else reads the credential.
        let bytes: &[u8] = if options.argv.iter().any(|arg| arg == "status") {
            br#"{"loggedIn":true,"email":"fixture@example.test"}"#
        } else {
            br#"{"claudeAiOauth":{"accessToken":"fixture-token","expiresAt":4000000000000}}"#
        };
        Ok(ExecOutcome {
            termination: provenant_supervisor::exec::Termination::Exit(0),
            stdout: provenant_supervisor::exec::CapturedStream {
                bytes: bytes.to_vec(),
                ..Default::default()
            },
            stderr: Default::default(),
            timed_out: false,
            drain_truncated: false,
            overflow_stopped: false,
            cancelled: false,
            elapsed_micros: 0,
        })
    }
}

/// Identity and submit report errors in text; other tools keep structured codes.
fn mcp_error_code(result: &Value) -> &str {
    result["structuredContent"]["code"]
        .as_str()
        .unwrap_or_else(|| {
            result["content"][0]["text"]
                .as_str()
                .and_then(|text| text.split_once(':'))
                .map_or("", |(code, _)| code)
        })
}
