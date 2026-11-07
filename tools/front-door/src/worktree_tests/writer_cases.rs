use super::*;

pub(super) fn writer_dispatch_edits_a_linked_worktree() -> Result {
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
    let clock = provenant_os::clock::SystemClock::new();
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
        mode: "instruction-edit",
    };
    let holder = holder_context(&rig, &roots, &context);
    let started = start_writer(&store, first_record.run_id, &provider, &holder, &call)?;
    assert!(
        matches!(started.as_slice(), [provenant_run::Started::Launched(id)] if *id == first_attempt),
        "{started:?}"
    );
    let first_terminal = await_terminal(&store, first_attempt, &call)?;
    // The fixture edits without committing: no plain success, and the dirty
    // path is named for the chair.
    assert_terminal_outcome(&first_terminal, provenant_run::Outcome::Uncommitted)?;
    assert!(matches!(
        &first_terminal.state,
        provenant_run::AttemptState::Terminal { reason: Some(reason), .. }
            if reason.contains("uncommitted edits") && reason.contains("AGENTS.md")
    ));
    assert_eq!(fs::read(path.join("AGENTS.md"))?, b"writer instructions\n");
    assert_eq!(
        fs::read_to_string(path.join("writer-edited"))?,
        "writer fixture completed"
    );
    wait_for_writer_release(&store)?;
    let path_text = path.to_str().ok_or("fixture worktree encoding")?;
    git(
        &rig,
        &["-C", path_text, "add", "writer-edited", "AGENTS.md"],
    )?;
    git(&rig, &["-C", path_text, "commit", "-qm", "Writer edit"])?;

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

pub(super) fn second_writer_is_refused_while_first_holds_the_worktree() -> Result {
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
    let clock = provenant_os::clock::SystemClock::new();
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
    provenant_test_support::wait_for(
        &format!("first writer hold point: {}", ready.display()),
        Duration::from_secs(8),
        || ready.exists(),
        |exists| *exists,
    )?;

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
    assert_terminal_outcome(&first_terminal, provenant_run::Outcome::Uncommitted)?;
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

pub(super) fn writer_protected_changes_wait_for_review_and_ignore_upstream() -> Result {
    if !writer_backend_available()? {
        return Ok(());
    }
    for own_change in [true, false] {
        let rig = repository()?;
        ok(
            &rig,
            &["worktree", "create", "writer", "--base", "HEAD", "--json"],
        )?;
        let path = rig.project.join(".worktrees/writer");
        let (roots, context) = writer_context(&rig)?;
        let cleanup = writer_process_cleanup(&context);
        let store = open_writer_store(&context)?;
        let clock = provenant_os::clock::SystemClock::new();
        let cancel = provenant_core::CancelToken::new();
        let call = writer_call(&clock, &cancel)?;
        let writer =
            provenant_run::resolve_writer(context.state_root.as_path(), &rig.project, &path)?;
        let prepared = writer_run(writer, "protected-review", 101, "hold for protected commit")?;
        let attempt = prepared.attempts[0].attempt_id;
        let record = store.create(&prepared, &call)?;
        // Upstream advances after the worktree exists and before launch, so the
        // lane's private Git view carries it while local main stays at the base.
        git(&rig, &["checkout", "-qb", "upstream"])?;
        fs::write(
            rig.project.join(".github/workflows/check.yml"),
            b"upstream workflow",
        )?;
        fs::write(rig.project.join(".github/new.yml"), b"upstream addition")?;
        fs::remove_file(rig.project.join("nested/AGENTS.md"))?;
        git(&rig, &["add", "-A"])?;
        git(&rig, &["commit", "-qm", "Upstream protected changes"])?;
        git(&rig, &["update-ref", "refs/remotes/origin/main", "HEAD"])?;
        git(
            &rig,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        )?;
        git(&rig, &["checkout", "-q", "main"])?;
        let release = path.join(".writer-fixture-release");
        let _release_on_drop = ReleaseOnDrop(release.clone());
        let provider = WriterProvider {
            state: context.state_root.as_path().to_owned(),
            home: fs::canonicalize(&rig.home)?,
            project: context.project_id,
            mode: "hold",
        };
        let holder = holder_context(&rig, &roots, &context);
        let started = start_writer(&store, record.run_id, &provider, &holder, &call)?;
        assert!(
            matches!(started.as_slice(), [provenant_run::Started::Launched(id)] if *id == attempt),
            "{started:?}"
        );
        let ready = path.join(".writer-fixture-ready");
        provenant_test_support::wait_for(
            &format!("writer hold: {}", ready.display()),
            Duration::from_secs(8),
            || ready.exists(),
            |exists| *exists,
        )?;
        // The lane commits to its private Git view. A commit made in the shared
        // repository during the attempt is a competing host update that
        // settlement never sees.
        let scratch = context
            .state_root
            .as_path()
            .join("runs")
            .join(record.run_id.to_string())
            .join("attempts")
            .join(attempt.to_string())
            .join("tmp/candidate-0");
        let lane = |args: &[&str]| lane_git(&rig, &scratch, &path, args);
        fs::write(path.join("source.txt"), b"writer source")?;
        lane(&["add", "source.txt"])?;
        lane(&["commit", "-qm", "Writer source"])?;
        lane(&["merge", "--no-edit", "origin/main"])?;
        if own_change {
            fs::write(path.join(".github/workflows/check.yml"), b"writer workflow")?;
            lane(&["add", ".github/workflows/check.yml"])?;
            lane(&["commit", "-qm", "Writer protected change"])?;
        }
        let head = lane(&["rev-parse", "HEAD"])?;
        fs::write(&release, b"release")?;
        let terminal = await_terminal(&store, attempt, &call)?;
        let value = serde_json::to_value(&terminal)?;
        let progress = fs::read_to_string(
            scratch
                .parent()
                .and_then(Path::parent)
                .ok_or("attempt directory")?
                .join("settlement-progress.json"),
        )
        .unwrap_or_else(|error| error.to_string());
        assert_eq!(
            value["state"]["outcome"], "succeeded",
            "unexpected writer terminal record: {value:#}; settlement progress: {progress}"
        );
        assert_eq!(value["head"], head);
        assert_eq!(value["protected_paths"], serde_json::json!([]));
        assert_eq!(
            git(
                &rig,
                &[
                    "-C",
                    path.to_str().ok_or("worktree path")?,
                    "rev-parse",
                    "HEAD"
                ]
            )?,
            head
        );
        assert_eq!(
            fs::read(path.join(".github/workflows/check.yml"))?,
            if own_change {
                b"writer workflow".as_slice()
            } else {
                b"upstream workflow".as_slice()
            }
        );
        wait_for_writer_release(&store)?;
        cleanup.disarm();
    }
    Ok(())
}

pub(super) fn writer_terminal_refuses_sequencer_state() -> Result {
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
    let clock = provenant_os::clock::SystemClock::new();
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
    assert_eq!(manifest["disposition"], "writer_settled");
    wait_for_writer_release(&store)?;

    // A private rewind may publish through CAS.
    fs::remove_file(
        writer
            .worktree
            .repo()
            .private_git_dir()
            .join("CHERRY_PICK_HEAD"),
    )?;
    let path_text = path.to_str().ok_or("fixture worktree encoding")?;
    git(&rig, &["-C", path_text, "reset", "--hard", "HEAD"])?;
    // The rewind target is the trusted origin default, as required by the
    // publication ancestry guard, rather than an arbitrary older lane tip.
    let origin_default = git(&rig, &["-C", path_text, "rev-parse", "HEAD"])?;
    git(
        &rig,
        &["update-ref", "refs/remotes/origin/main", &origin_default],
    )?;
    git(
        &rig,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    )?;
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
    let ready = path.join(".writer-fixture-ready");
    provenant_test_support::wait_for(
        &format!("sequencer writer hold point: {}", ready.display()),
        Duration::from_secs(8),
        || ready.exists(),
        |exists| *exists,
    )?;
    // A lane now rewinds its private ref. Rewinding the shared ref instead is
    // a competing host update, which correctly blocks CAS publication.
    let previous = git(&rig, &["-C", path_text, "rev-parse", "HEAD^"])?;
    let scratch = context
        .state_root
        .as_path()
        .join("runs")
        .join(record.run_id.to_string())
        .join("attempts")
        .join(attempt.to_string())
        .join("tmp/candidate-0");
    lane_git(&rig, &scratch, &path, &["reset", "--hard", &previous])?;
    fs::write(release, b"release")?;
    let terminal = await_terminal(&store, attempt, &call)?;
    assert_terminal_outcome(&terminal, provenant_run::Outcome::Succeeded)?;
    assert_eq!(
        git(&rig, &["-C", path_text, "rev-parse", "HEAD"])?,
        previous
    );
    wait_for_writer_release(&store)?;
    cleanup.disarm();
    Ok(())
}

pub(super) fn writer_production_plan_denies_outside_writes() -> Result {
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
    let clock = provenant_os::clock::SystemClock::new();
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
        provenant_run::Outcome::Uncommitted,
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

pub(super) fn writer_reconcile_refuses_unfinished_cherry_pick_and_releases() -> Result {
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
    let clock = provenant_os::clock::SystemClock::new();
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
    let process_exit = attempt_path.join("process-exit.json");
    provenant_test_support::wait_for(
        &format!("reconciled writer process exit: {}", process_exit.display()),
        Duration::from_secs(8),
        || process_exit.exists(),
        |exists| *exists,
    )?;
    assert!(!matches!(
        store.record(attempt, &call)?.state,
        provenant_run::AttemptState::Terminal { .. }
    ));
    // Temporarily hide the private objects to force a retryable import failure.
    let objects = attempt_path.join("tmp/candidate-0/git-objects");
    let hidden_objects = attempt_path.join("tmp/candidate-0/git-objects-hidden");
    fs::rename(&objects, &hidden_objects)?;
    let settling = store.reconcile_for_fixture(record.run_id, &provider, &call)?;
    assert!(
        matches!(settling.as_slice(), [(id, provenant_run::Reconciled::Settling { .. })] if *id == attempt),
        "{settling:?}"
    );
    assert_terminal_outcome(
        &store.record(attempt, &call)?,
        provenant_run::Outcome::Unsettled,
    )?;
    assert!(!attempt_path.join("quarantine/manifest.json").exists());
    fs::rename(hidden_objects, objects)?;
    // The operation remains unfinished when the durable exit is recovered.
    fs::write(
        writer
            .worktree
            .repo()
            .private_git_dir()
            .join("CHERRY_PICK_HEAD"),
        b"unfinished cherry-pick",
    )?;
    // The backoff itself has unit coverage. Emulate its expiry so this fixture
    // can exercise recovery of the private objects without a real-time wait.
    let progress = attempt_path.join("settlement-progress.json");
    let mut journal: Value = serde_json::from_slice(&fs::read(&progress)?)?;
    journal["retry_after_ms"] = 0.into();
    fs::write(progress, serde_json::to_vec(&journal)?)?;
    let found = store.reconcile_for_fixture(record.run_id, &provider, &call)?;
    // The sealed unsettled decision stays; the retry reaches explicit refusal
    // of the unfinished operation, releases custody and removes temporary files.
    assert_eq!(
        found,
        vec![(attempt, provenant_run::Reconciled::TemporaryRemoved)]
    );
    let terminal = store.record(attempt, &call)?;
    assert_terminal_outcome(&terminal, provenant_run::Outcome::Unsettled)?;
    assert!(matches!(
        terminal.settlement_receipts,
        Some(provenant_run::SettlementReceipts {
            git_import: provenant_run::EffectDisposition::Failed { ref reason, retryable: false },
            ..
        }) if reason.contains("in-progress Git operation")
    ));
    assert!(attempt_path.join("release.json").is_file());
    assert!(store.writer_fences()?.is_empty());
    cleanup.disarm();
    Ok(())
}

pub(super) fn recreated_worktree_accepts_writer_after_settlement() -> Result {
    use provenant_os::fs::Directory;
    use provenant_supervisor::custody::{Scope, WriterBinding};

    let rig = repository()?;
    ok(&rig, &["worktree", "create", "reused", "--json"])?;
    let path = rig.project.join(".worktrees/reused");
    let (_, context) = writer_context(&rig)?;
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = provenant_core::CancelToken::new();
    let scope = Scope {
        run: provenant_core::RunId::from_bytes([31; 16]),
        attempt: provenant_core::AttemptId::from_bytes([32; 16]),
        generation: 1,
    };
    let state = rig
        .state
        .join("worktrees")
        .join(context.project_id.to_string())
        .join(provenant_confinement::worktrees::state_dir_name(&path));
    let acquire = |scope| -> Result<_> {
        let request =
            provenant_run::resolve_writer(context.state_root.as_path(), &rig.project, &path)?;
        let writer = provenant_run::prepare_writer(
            &request,
            context.state_root.as_path(),
            context.project_id,
        )?;
        let binding = WriterBinding::from_verified(
            writer.worktree.repo().root_dir(),
            writer.worktree.repo().private_dir(),
            writer.branch.as_str(),
            provenant_core::GitOid::parse(&git(&rig, &["rev-parse", "HEAD"])?)?,
        )?;
        Ok(provenant_supervisor::fixture::acquire_writer_with_binding(
            Directory::open_anchor(&state)?,
            scope,
            binding,
            provenant_core::WaitBudget::nonblocking(clock.mono_now()),
            &cancel,
            &clock,
        )?)
    };
    let old_tree = Directory::open_anchor(&path)?;
    drop(acquire(scope)?);
    assert!(acquire(scope).is_err_and(|error| is_fenced(error.as_ref())));
    provenant_supervisor::custody::acquire_recovery(
        Directory::open_anchor(&state)?,
        scope,
        provenant_core::WaitBudget::nonblocking(clock.mono_now()),
        &cancel,
        &clock,
    )?
    .withdraw_prepared()?;
    ok(&rig, &["worktree", "remove", "reused", "--json"])?;
    ok(&rig, &["worktree", "create", "reused", "--json"])?;
    // The settled run cannot carry a later generation onto a new worktree.
    assert!(
        acquire(Scope {
            generation: 2,
            ..scope
        })
        .is_err_and(|error| is_fenced(error.as_ref()))
    );
    let lease = acquire(Scope {
        run: provenant_core::RunId::from_bytes([33; 16]),
        attempt: provenant_core::AttemptId::from_bytes([34; 16]),
        ..scope
    })?;
    lease.withdraw_unlaunched()?;
    // The first run cannot return to the path by readmitting its attempt
    // once another run has owned it.
    assert!(
        acquire(Scope {
            generation: 2,
            ..scope
        })
        .is_err_and(|error| is_fenced(error.as_ref()))
    );
    drop(old_tree);
    Ok(())
}
