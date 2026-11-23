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
    let first_attempt = first.attempt_id;
    let first_record = store.create(&first, &call)?;
    let provider = WriterProvider {
        state: context.state_root.as_path().to_owned(),
        home: fs::canonicalize(&rig.home)?,
        project: context.project_id,
        mode: "instruction-edit",
    };
    let holder = holder_context(&rig, &roots, &context);
    let started = start_writer(&store, first_record.attempt_id, &provider, &holder, &call)?;
    assert!(
        matches!(started.as_slice(), [provenant_run::Started::Launched(id)] if *id == first_attempt),
        "{started:?}"
    );
    let first_terminal = await_terminal(&store, first_attempt, &call)?;
    // The fixture edits without committing: no plain success, and the dirty
    // path is named for the chair.
    assert_terminal_outcome(&first_terminal, provenant_run::Outcome::Uncommitted)?;
    assert!(
        matches!(
            &first_terminal.state,
            provenant_run::AttemptState::Terminal { reason: Some(reason), .. }
                if reason.contains("uncommitted edits") && reason.contains("AGENTS.md")
        ),
        "{:?}",
        first_terminal.state
    );
    assert_eq!(
        fs::read(path.join("AGENTS.md"))?,
        b"writer instructions, edited\n"
    );
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
    let second_attempt = second.attempt_id;
    let second_record = store.create(&second, &call)?;
    let started = start_writer(&store, second_record.attempt_id, &provider, &holder, &call)?;
    assert!(
        matches!(started.as_slice(), [provenant_run::Started::Launched(id)] if *id == second_attempt),
        "{started:?}"
    );
    let second_terminal = await_terminal(&store, second_attempt, &call)?;
    assert_terminal_outcome(&second_terminal, provenant_run::Outcome::Succeeded)?;
    wait_for_writer_release(&store)?;
    // A settled attempt remains a report root even after its lane is gone.
    git(&rig, &["worktree", "remove", "--force", path_text])?;
    assert!(!path.exists());
    let attempt_root = context
        .state_root
        .as_path()
        .join("attempts")
        .join(first_attempt.to_string());
    let report_roots =
        crate::gc_support::project_settled_attempt_roots(&roots, context.project_id, &cancel);
    assert!(report_roots.contains(&attempt_root), "{report_roots:?}");
    struct Dweller(PathBuf);
    impl provenant_confinement::worktrees::WorkerObserver for Dweller {
        fn observe_roots(
            &self,
            roots: &[PathBuf],
        ) -> std::collections::BTreeMap<PathBuf, provenant_confinement::worktrees::Occupancy>
        {
            roots
                .iter()
                .cloned()
                .map(|root| (root, provenant_confinement::worktrees::Occupancy::Available))
                .collect()
        }
        fn live_process(
            &self,
            root: &Path,
        ) -> Option<provenant_confinement::worktrees::ObservedProcess> {
            (root == self.0).then(|| {
                provenant_confinement::worktrees::ObservedProcess::cwd(4242, Some("fixture".into()))
            })
        }
    }
    let report = provenant_confinement::worktrees::held_report(
        &Dweller(attempt_root),
        &report_roots,
        &|| true,
    );
    assert!(report.processes.iter().any(|process| process.pid == 4242));
    cleanup.disarm();
    Ok(())
}

pub(super) fn shared_holders_both_launch_and_removal_refused_while_holder_is_live() -> Result {
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
    let first_attempt = first.attempt_id;
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
    let started = start_writer(
        &store,
        first_record.attempt_id,
        &first_provider,
        &holder,
        &call,
    )?;
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
    let second_attempt = second.attempt_id;
    let second_record = store.create(&second, &call)?;
    let second_provider = WriterProvider {
        state: context.state_root.as_path().to_owned(),
        home: fs::canonicalize(&rig.home)?,
        project: context.project_id,
        mode: "edit",
    };
    let started = start_writer(
        &store,
        second_record.attempt_id,
        &second_provider,
        &holder,
        &call,
    )?;
    // Worktree occupancy is a shared lease: both holders launch concurrently.
    assert!(
        matches!(started.as_slice(), [provenant_run::Started::Launched(id)] if *id == second_attempt),
        "{started:?}"
    );

    // Removal and GC refuse while any holder is live (exclusive removal custody).
    let refused = command(&rig, &["worktree", "remove", "writer", "--json"])?;
    let refusal = json(&refused)?;
    assert_eq!(refusal["status"], "refused", "{refusal}");
    assert_eq!(refusal["error"]["code"], "WORKTREE_LOCK_BUSY", "{refusal}");

    let second_terminal = await_terminal(&store, second_attempt, &call)?;
    assert_terminal_outcome(&second_terminal, provenant_run::Outcome::Succeeded)?;

    fs::write(&release, b"release")?;
    let first_terminal = await_terminal(&store, first_attempt, &call)?;
    assert_terminal_outcome(&first_terminal, provenant_run::Outcome::Uncommitted)?;
    wait_for_writer_release(&store)?;
    cleanup.disarm();
    Ok(())
}

pub(super) fn writer_settlement_merges_upstream_advance() -> Result {
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
    let writer = provenant_run::resolve_writer(context.state_root.as_path(), &rig.project, &path)?;
    let prepared = writer_run(
        writer,
        "upstream-settlement",
        101,
        "merge upstream and writer changes",
    )?;
    let attempt = prepared.attempt_id;
    let record = store.create(&prepared, &call)?;

    // Advance origin/main after the writer worktree exists.
    git(&rig, &["checkout", "-qb", "upstream"])?;
    fs::write(rig.project.join("upstream.txt"), b"upstream change")?;
    git(&rig, &["add", "upstream.txt"])?;
    git(&rig, &["commit", "-qm", "Upstream change"])?;
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
    let started = start_writer(&store, record.attempt_id, &provider, &holder, &call)?;
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

    let scratch = context
        .state_root
        .as_path()
        .join("attempts")
        .join(attempt.to_string())
        .join("tmp/candidate-0");
    let lane = |args: &[&str]| lane_git(&rig, &context, attempt, &path, args);
    fs::write(path.join("writer.txt"), b"writer change")?;
    lane(&["add", "writer.txt"])?;
    lane(&["commit", "-qm", "Writer change"])?;
    lane(&["merge", "--no-edit", "origin/main"])?;
    let lane_head = lane(&["rev-parse", "HEAD"])?;

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
    let diagnostics = format!(
        "settled record reason: {:?}; settlement progress: {progress}",
        value["state"]["reason"]
    );
    assert_eq!(
        value["state"]["outcome"], "succeeded",
        "unexpected writer terminal record: {value:#}; {diagnostics}"
    );
    assert_eq!(value["head"], lane_head, "{diagnostics}");
    assert_eq!(
        git(
            &rig,
            &[
                "-C",
                path.to_str().ok_or("worktree path")?,
                "rev-parse",
                "HEAD",
            ],
        )?,
        lane_head,
        "{diagnostics}"
    );
    assert!(path.join("upstream.txt").is_file(), "{diagnostics}");
    assert_eq!(
        fs::read(path.join("upstream.txt"))?,
        b"upstream change",
        "{diagnostics}"
    );
    assert!(path.join("writer.txt").is_file(), "{diagnostics}");
    assert_eq!(
        fs::read(path.join("writer.txt"))?,
        b"writer change",
        "{diagnostics}"
    );
    wait_for_writer_release(&store)?;
    cleanup.disarm();
    Ok(())
}

pub(super) fn writer_terminal_retains_sequencer_state() -> Result {
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
    let attempt = prepared.attempt_id;
    let record = store.create(&prepared, &call)?;
    let provider = WriterProvider {
        state: context.state_root.as_path().to_owned(),
        home: fs::canonicalize(&rig.home)?,
        project: context.project_id,
        mode: "control",
    };
    let holder = holder_context(&rig, &roots, &context);
    let started = start_writer(&store, record.attempt_id, &provider, &holder, &call)?;
    assert!(matches!(started.as_slice(), [provenant_run::Started::Launched(id)] if *id == attempt));
    // Spec: docs/specs/run-settlement.md — an unfinished operation in the
    // lane's own Git is retained and named; settlement publishes the committed
    // tip instead of refusing the attempt.
    let terminal = await_terminal(&store, attempt, &call)?;
    assert_terminal_outcome(&terminal, provenant_run::Outcome::Uncommitted)?;
    assert!(
        matches!(
            terminal.settlement_receipts,
            Some(provenant_run::SettlementReceipts {
                git_import: provenant_run::EffectDisposition::Done,
                ..
            })
        ),
        "unexpected settlement receipts: {:?}",
        terminal.settlement_receipts
    );
    assert!(
        writer
            .worktree
            .repo()
            .private_git_dir()
            .join("CHERRY_PICK_HEAD")
            .is_file(),
        "settlement must not reset or clean the lane's unfinished operation"
    );
    assert!(path.join(".git").is_file());
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
    let attempt = prepared.attempt_id;
    let record = store.create(&prepared, &call)?;
    let release = path.join(".writer-fixture-release");
    let _release_on_drop = ReleaseOnDrop(release.clone());
    let provider = WriterProvider {
        state: context.state_root.as_path().to_owned(),
        home: fs::canonicalize(&rig.home)?,
        project: context.project_id,
        mode: "hold",
    };
    let started = start_writer(&store, record.attempt_id, &provider, &holder, &call)?;
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
    // Rewind the lane's shared Git view, keeping the host ref separate until
    // settlement verifies and publishes it.
    let previous = git(&rig, &["-C", path_text, "rev-parse", "HEAD^"])?;
    lane_git(
        &rig,
        &context,
        attempt,
        &path,
        &["reset", "--hard", &previous],
    )?;
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
    let attempt = prepared.attempt_id;
    let record = store.create(&prepared, &call)?;
    let provider = WriterProvider {
        state: context.state_root.as_path().to_owned(),
        home: fs::canonicalize(&rig.home)?,
        project: context.project_id,
        mode: "negative",
    };
    let started = start_writer(
        &store,
        record.attempt_id,
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

pub(super) fn writer_reconcile_keeps_unfinished_cherry_pick_and_releases() -> Result {
    if !writer_backend_available()? {
        return Ok(());
    }
    let rig = repository()?;
    ok(
        &rig,
        &["worktree", "create", "writer", "--base", "HEAD", "--json"],
    )?;
    fs::write(
        rig.project.join(".fixture-crash-before-settlement"),
        b"crash before settlement",
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
    let attempt = prepared.attempt_id;
    let record = store.create(&prepared, &call)?;
    let provider = WriterProvider {
        state: context.state_root.as_path().to_owned(),
        home: fs::canonicalize(&rig.home)?,
        project: context.project_id,
        mode: "edit",
    };
    let started = start_writer(
        &store,
        record.attempt_id,
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
    // The exit record is published before the guardian crashes at the
    // settlement handoff. Wait until that guardian process has actually gone:
    // the kernel then releases its locks while the crashed attempt's
    // reservation is still retained, so reconciliation sees a settled slot
    // rather than a live, stalled guardian. `fence.live` cannot decide this:
    // the fixture records the synthetic fixture boot while the fence listing
    // observes the real boot, so `live` is false from the boot mismatch before
    // the guardian exits. Observe the guardian pid recorded in the reservation;
    // it is a real host process.
    let guardian_pid = {
        let fence = store
            .writer_fences()?
            .into_iter()
            .find(|fence| fence.scope.attempt == attempt)
            .ok_or("crashed writer guardian reservation missing")?;
        let slot = fence
            .custody_directory()
            .ok_or("crashed writer guardian custody slot missing")?;
        let slot = provenant_os::fs::Directory::open_anchor(slot)?;
        provenant_supervisor::custody::reservation_guardian(&slot, fence.scope)?
            .ok_or("crashed writer guardian identity missing")?
            .pid
    };
    provenant_test_support::wait_for(
        "crashed writer guardian gone",
        Duration::from_secs(8),
        || provenant_test_support::process_is_gone(&guardian_pid.to_string()).unwrap_or(false),
        |gone| *gone,
    )?;
    // The operation remains unfinished when the durable exit is recovered.
    fs::write(
        writer
            .worktree
            .repo()
            .private_git_dir()
            .join("CHERRY_PICK_HEAD"),
        b"unfinished cherry-pick",
    )?;
    let found = store.reconcile_for_fixture(record.attempt_id, &provider, &call)?;
    // The lane owns its unfinished operation: recovery publishes the committed
    // tip, leaves the in-progress state for a person to resume, settles once
    // and releases custody. The terminal outcome follows the lane's own
    // uncommitted edits, not the retained operation.
    assert!(matches!(
        found.as_slice(),
        [(reconciled_attempt, provenant_run::Reconciled::WriterLeaseReleased { locks })]
            if *reconciled_attempt == attempt && !locks.is_empty()
    ));
    let terminal = store.record(attempt, &call)?;
    assert_terminal_outcome(&terminal, provenant_run::Outcome::Uncommitted)?;
    assert!(
        matches!(
            terminal.settlement_receipts,
            Some(provenant_run::SettlementReceipts {
                git_import: provenant_run::EffectDisposition::Done,
                ..
            })
        ),
        "unexpected settlement receipts: {:?}",
        terminal.settlement_receipts
    );
    assert!(
        writer
            .worktree
            .repo()
            .private_git_dir()
            .join("CHERRY_PICK_HEAD")
            .is_file(),
        "settlement must not reset or clean the lane's unfinished operation"
    );
    assert!(attempt_path.join("release.json").is_file());
    assert!(store.writer_fences()?.is_empty());
    cleanup.disarm();
    Ok(())
}

pub(super) fn recreated_worktree_accepts_writer_after_settlement() -> Result {
    use provenant_os::fs::Directory;
    use provenant_records::custody::Scope;
    use provenant_supervisor::custody::WriterBinding;

    let rig = repository()?;
    ok(&rig, &["worktree", "create", "reused", "--json"])?;
    let path = rig.project.join(".worktrees/reused");
    let (_, context) = writer_context(&rig)?;
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = provenant_core::CancelToken::new();
    let scope = Scope {
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
