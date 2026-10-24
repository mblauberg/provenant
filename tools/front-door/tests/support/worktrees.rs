#[path = "mod.rs"]
mod support;
use provenant_core::Clock;
use provenant_supervisor::exec::ExecOutcome;
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
            "writer_dispatch_edits_a_linked_worktree",
            "second_writer_is_refused_while_first_holds_the_worktree",
            "writer_terminal_refuses_sequencer_state",
            "writer_production_plan_denies_outside_writes",
            "writer_reconcile_refuses_unfinished_cherry_pick_and_releases",
            "create_list_and_remove_a_managed_worktree",
            "merged_pruning_keeps_dirty_unmerged_and_out_of_scope_worktrees",
            "escaping_branch_is_invalid_and_writes_no_checkout_outside_worktree_root",
        ],
        |name| {
            let result = match name {
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
                "merged_pruning_keeps_dirty_unmerged_and_out_of_scope_worktrees" => {
                    merged_pruning_keeps_dirty_unmerged_and_out_of_scope_worktrees()
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

fn git(rig: &TestRig, args: &[&str]) -> Result<String> {
    let output = rig.execute(Path::new("/usr/bin/git"), args, None)?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    Ok(String::from_utf8(output.stdout.bytes)?.trim().to_owned())
}

fn repository() -> Result<TestRig> {
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

fn writer_backend_available() -> Result<bool> {
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

fn writer_process_cleanup(
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
        holder,
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
        call,
    )?)
}

fn writer_context(
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

fn open_writer_store(context: &provenant_run::ExecutionContext) -> Result<provenant_run::RunStore> {
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
            }],
            concurrency: 1,
            parent: None,
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
            paths,
            &writer,
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
            argv,
            environment: Vec::new(),
            stdin: Vec::new(),
            cwd: writer.worktree.path().to_owned(),
            route,
            confinement: Some(provenant_run::LaunchConfinement {
                plan,
                profile,
                writer: Some(writer),
            }),
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

fn wait_for_writer_release(store: &provenant_run::RunStore) -> Result {
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

fn ok(rig: &TestRig, args: &[&str]) -> Result<Value> {
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
    let removed = ok(&rig, &["worktree", "remove", "feature/topic", "--json"])?;
    assert_eq!(
        removed["data"]["removed"]
            .as_array()
            .ok_or("missing removals")?
            .len(),
        1
    );
    assert!(!rig.project.join(".worktrees/feature-topic").exists());
    assert_eq!(
        ok(&rig, &["worktree", "list", "--json"])?["data"]
            .as_array()
            .ok_or("missing list")?
            .len(),
        1
    );
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
