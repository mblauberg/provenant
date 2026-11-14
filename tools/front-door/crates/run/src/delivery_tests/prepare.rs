use super::*;

#[test]
fn non_fact_host_commands_keep_their_limits() {
    let listing = provenant_os::exec::DEFAULT_CAPTURE_LIMIT;
    for args in [
        &["for-each-ref", "--format=%(refname)"][..],
        &[
            "--git-dir",
            "repo.git",
            "--work-tree",
            "tree",
            "for-each-ref",
        ],
        &[
            "-c",
            "remote.origin.uploadpack=git-upload-pack",
            "ls-remote",
            "origin",
        ],
    ] {
        assert_eq!(stdout_limit(false, args), listing, "{args:?}");
    }
    assert_eq!(stdout_limit(false, &["remote"]), 16 * 1024);
    assert_eq!(stdout_limit(true, &["log"]), 56_000);
}

#[test]
fn git_subcommand_skips_global_option_values() {
    for (args, expected) in [
        (&["worktree", "list", "--porcelain"][..], "worktree list"),
        (&["-c", "core.hooksPath=/dev/null", "remote"], "remote"),
        (
            &["-C", "relative/dir", "symbolic-ref", "--quiet"],
            "symbolic-ref",
        ),
        (
            &["--git-dir", "rel", "--work-tree", "tree", "status"],
            "status",
        ),
        (
            &["--git-dir=rel", "--work-tree=tree", "worktree", "list"],
            "worktree list",
        ),
        (
            &["-c", "a=b", "-C", "x", "worktree", "list"],
            "worktree list",
        ),
        (&["pr", "view", "1"], "pr"),
        (&[], "git"),
    ] {
        assert_eq!(git_subcommand(args), expected, "{args:?}");
    }
}

#[test]
fn prepare_refuses_unavailable_and_unsafe_credential_helpers()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let unavailable = fixture.root.join("missing/gh");
    let not_executable = fixture.root.join("not-executable");
    std::fs::write(&not_executable, "not executable")?;
    let lane_helper = fixture.writer.worktree.join("gh");
    fixture.executable(&lane_helper, "#!/bin/sh\nexit 0\n")?;
    let link = fixture.root.join("linked-gh");
    std::os::unix::fs::symlink(&lane_helper, &link)?;
    let opaque = fixture.root.join("gh;echo");
    fixture.executable(&opaque, "#!/bin/sh\nexit 0\n")?;
    for (helper, detail) in [
        (&unavailable, "gh: not found"),
        (&not_executable, "not-executable"),
        (&fixture.root, "unavailable"),
        (&lane_helper, "inside the writer worktree"),
        (&link, "inside the writer worktree"),
        (&opaque, "opaque shell credential helper"),
    ] {
        fixture.git(&[
            "config",
            "credential.helper",
            &format!("!{} auth git-credential", helper.display()),
        ])?;
        let error = fixture.prepare().expect_err("unsafe helper");
        assert!(
            matches!(
                error,
                RunError::DeliveryPrepare {
                    step: "credential helper",
                    ..
                }
            ),
            "{error}"
        );
        let reason = error.to_string();
        assert!(reason.contains(detail), "{reason}");
        assert!(!reason.contains("receipt"), "{reason}");
    }
    Ok(())
}

#[test]
fn host_delivery_fetches_commits_pushes_and_records_upstream()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let home = root.join("home");
    let project = root.join("project");
    let lane = root.join("lane");
    let remote = root.join("remote.git");
    for path in [&home, &project] {
        std::fs::create_dir(path)?;
    }
    let environment = vec![
        ("HOME".into(), home.into_os_string()),
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
    ];
    let host = HostDeliveryContext::new(environment.clone());
    let executor = SystemExecutor::new(1)?;
    let git_executable = provenant_os::git::git_executable().ok_or("git executable")?;
    let git =
        |cwd: &Path, args: &[&str]| -> std::result::Result<String, Box<dyn std::error::Error>> {
            let argv: Vec<OsString> = std::iter::once(git_executable.as_os_str().to_owned())
                .chain(args.iter().map(OsString::from))
                .collect();
            let mut options = ExecOptions::new(&argv, cwd, Duration::from_secs(5));
            options.env = Some(&environment);
            let output = executor.execute(options)?;
            assert_eq!(
                output.termination,
                Termination::Exit(0),
                "{}",
                String::from_utf8_lossy(&output.stderr.bytes)
            );
            Ok(String::from_utf8(output.stdout.bytes)?.trim().into())
        };
    git(&project, &["init", "-q"])?;
    git(&project, &["config", "user.name", "Fixture"])?;
    git(
        &project,
        &["config", "user.email", "fixture@example.invalid"],
    )?;
    git(&project, &["commit", "--allow-empty", "-qm", "Initial"])?;
    git(
        &project,
        &["init", "--bare", "-q", remote.to_str().ok_or("remote")?],
    )?;
    git(
        &project,
        &["remote", "add", "origin", remote.to_str().ok_or("remote")?],
    )?;
    // A narrowed fetch refspec that omits the delivered branch, as in a
    // main-only clone, must not defeat delivery.
    git(
        &project,
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/base:refs/remotes/origin/base",
        ],
    )?;
    git(
        &project,
        &[
            "config",
            "--add",
            "remote.origin.fetch",
            "+refs/heads/*:refs/heads/injected/*",
        ],
    )?;
    git(&project, &["push", "-q", "origin", "HEAD:refs/heads/base"])?;
    let initial = git(&project, &["rev-parse", "HEAD"])?;
    git(&project, &["update-ref", "-d", "refs/remotes/origin/base"])?;
    git(
        &project,
        &[
            "worktree",
            "add",
            "-qb",
            "lane",
            lane.to_str().ok_or("lane")?,
        ],
    )?;
    let store_root = root.join("store");
    let custody_project = registered_writer_project_id(
        &project,
        &store_root.join("home"),
        &store_root.join("checkout"),
        &store_root.join("state"),
    )?;
    let writer = WriterRequest {
        extra: writer_custody_extra(custody_project),
        project,
        worktree: lane.clone(),
    };
    let request = WriterDeliveryRequest {
        extra: Default::default(),
        mode: Default::default(),
        remote: "origin".into(),
        branch: "published/lane".into(),
        open_pr: false,
        fast_forward_only: false,
    };
    let host = host.prepare(&writer, &request)?;
    assert!(
        !git(
            &lane,
            &["for-each-ref", "--format=%(refname)", "refs/heads/injected"]
        )?
        .contains("injected")
    );
    assert_eq!(
        git(&lane, &["rev-parse", "refs/remotes/origin/base"])?,
        initial
    );
    // The writer's committed head, rather than any provider's prose, is delivered.
    std::fs::write(lane.join("change"), "committed by the writer")?;
    git(&lane, &["add", "change"])?;
    git(&lane, &["commit", "-qm", "Writer change"])?;
    git(&lane, &["config", "push.followTags", "true"])?;
    git(
        &lane,
        &["tag", "-a", "local-only", "-m", "Do not deliver this tag"],
    )?;
    let hook = writer.project.join(".git/hooks/pre-push");
    std::fs::write(&hook, "#!/bin/sh\nexit 42\n")?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700))?;
    let head = GitOid::parse(&git(&lane, &["rev-parse", "HEAD"])?)?;
    let head_text = head.to_string();
    let fixture = store_root;
    std::fs::create_dir_all(&fixture)?;
    let (clock, cancel) = (tests::clock(), provenant_core::CancelToken::new());
    let call = tests::call(&clock, &cancel);
    let store = RunStore::open(&tests::context(&fixture)?, &call)?;
    let mut input = tests::input();
    input.request.task.writer = Some(writer.clone());
    input.request.task.deliver = Some(request.clone());
    store.create(&input, &call)?;
    let id = input.attempt_id;
    let attempt = store.attempt_dir(id, false)?;
    // A lane that rewrites the stored destination after launch cannot steer
    // the host's push: the destination bound at prepare wins.
    let rewritten = WriterDeliveryRequest {
        branch: "main".into(),
        ..request.clone()
    };
    assert!(matches!(
        deliver_once(&host, &writer, &rewritten, &attempt, "fixture receipt"),
        Err(RunError::Validation(
            "delivery destination changed after launch"
        ))
    ));
    assert!(git(&lane, &["ls-remote", "origin", "refs/heads/main"])?.is_empty());
    let receipt = deliver_once(&host, &writer, &request, &attempt, "fixture receipt")?;
    assert_eq!(receipt.head, Some(head));
    assert!(matches!(
        receipt.state,
        WriterDeliveryState::Delivered {
            pull_request: None,
            ..
        }
    ));
    assert_eq!(
        git(&lane, &["rev-parse", "refs/remotes/origin/published/lane"])?,
        head_text.as_str()
    );
    assert_eq!(git(&lane, &["config", "branch.lane.remote"])?, "origin");
    assert_eq!(
        git(&lane, &["config", "branch.lane.merge"])?,
        "refs/heads/published/lane"
    );
    assert!(
        git(&lane, &["ls-remote", "origin", "refs/heads/published/lane"])?
            .starts_with(head_text.as_str())
    );
    assert!(git(&lane, &["ls-remote", "--tags", "origin"])?.is_empty());
    dispatch_tests::seal_exit(&store, &tests::exit())?;
    let record = store.publish_terminal(id, 1, &tests::publication(), &tests::exit(), &call)?;
    assert_eq!(record.delivery.as_ref(), Some(&receipt));
    assert_eq!(store.record(id, &call)?.head, Some(head));
    // A repeated terminal callback must not push a newer head or repeat PR creation.
    git(&lane, &["commit", "--allow-empty", "-qm", "Later"])?;
    assert_eq!(
        deliver_once(&host, &writer, &request, &attempt, "fixture receipt")?,
        receipt
    );
    assert!(
        git(&lane, &["ls-remote", "origin", "refs/heads/published/lane"])?
            .starts_with(head_text.as_str())
    );
    // A writer that made no commit reports `no_new_commits` and pushes nothing.
    let idle_lane = root.join("idle");
    git(
        &writer.project,
        &[
            "worktree",
            "add",
            "-qb",
            "idle",
            idle_lane.to_str().ok_or("idle")?,
        ],
    )?;
    let idle_writer = WriterRequest {
        extra: writer.extra.clone(),
        project: writer.project.clone(),
        worktree: idle_lane.clone(),
    };
    let idle_request = WriterDeliveryRequest {
        branch: "published/idle".into(),
        ..request.clone()
    };
    let idle_host =
        HostDeliveryContext::new(environment.clone()).prepare(&idle_writer, &idle_request)?;
    let mut idle = tests::input();
    idle.attempt_id = provenant_core::AttemptId::from_bytes([17; 16]);
    idle.operation_id = provenant_core::OperationId::parse("idle-delivery")?;
    idle.attempt_id = provenant_core::AttemptId::from_bytes([18; 16]);
    crate::tests::rekey(&mut idle);
    idle.request.task.writer = Some(idle_writer.clone());
    idle.request.task.deliver = Some(idle_request.clone());
    store.create(&idle, &call)?;
    let idle_attempt = store.attempt_dir(idle.attempt_id, false)?;
    let idle_receipt = deliver_once(
        &idle_host,
        &idle_writer,
        &idle_request,
        &idle_attempt,
        "fixture receipt",
    )?;
    assert_eq!(
        idle_receipt.state,
        WriterDeliveryState::NoNewCommits {
            extra: Default::default()
        }
    );
    assert_eq!(idle_receipt.head, Some(GitOid::parse(&initial)?));
    assert!(git(&lane, &["ls-remote", "origin", "refs/heads/published/idle"])?.is_empty());
    // An interrupted delivery is recorded during terminal recovery, with no push.
    let mut interrupted = tests::input();
    interrupted.attempt_id = provenant_core::AttemptId::from_bytes([14; 16]);
    interrupted.operation_id = provenant_core::OperationId::parse("interrupted-delivery")?;
    interrupted.attempt_id = provenant_core::AttemptId::from_bytes([16; 16]);
    crate::tests::rekey(&mut interrupted);
    interrupted.request.task.writer = Some(writer);
    interrupted.request.task.deliver = Some(WriterDeliveryRequest {
        branch: "interrupted".into(),
        ..request
    });
    store.create(&interrupted, &call)?;
    let id = interrupted.attempt_id;
    let attempt = store.attempt_dir(id, false)?;
    let pending = WriterDeliveryReceipt {
        branch: "interrupted".into(),
        state: WriterDeliveryState::Failed {
            extra: Default::default(),
            stage: WriterDeliveryStage::Interrupted,
        },
        ..receipt
    };
    artifacts::seal(
        &attempt,
        "delivery-started.json",
        &db::bytes(&pending)?,
        RECORD_LIMIT,
    )?;
    let mut exit = tests::exit();
    exit.scope.attempt = interrupted.attempt_id;
    exit.scope.attempt = id;
    dispatch_tests::seal_exit(&store, &exit)?;
    artifacts::seal(
        &attempt,
        "result.md",
        tests::publication().result.as_bytes(),
        RESULT_LIMIT,
    )?;
    let record = store.recover_result_only(id, &tests::publication(), &exit, &call)?;
    assert_eq!(record.delivery, Some(pending));
    assert!(git(&lane, &["ls-remote", "origin", "refs/heads/interrupted"])?.is_empty());
    Ok(())
}

#[test]
fn land_settlement_clears_terminal_writer_reservation()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    landed_writer_fence_fixture(LandedWriterFenceCase::Terminal)
}

#[test]
fn land_settlement_refuses_nonterminal_writer()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    landed_writer_fence_fixture(LandedWriterFenceCase::Nonterminal)
}

#[test]
fn land_settlement_refuses_mismatched_or_incomplete_writer_evidence()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    for case in [
        LandedWriterFenceCase::WrongScope,
        LandedWriterFenceCase::WrongWriter,
        LandedWriterFenceCase::WrongAttempt,
        LandedWriterFenceCase::MissingQuarantine,
    ] {
        landed_writer_fence_fixture(case)?;
    }
    Ok(())
}

#[test]
fn terminal_resumes_after_branch_publication_before_its_phase_checkpoint()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let review = fixture.terminal_via_inner("origin", None, true, false, false, || {
        std::fs::write(
            fixture.writer.worktree.join("resumed.txt"),
            "durable branch\n",
        )?;
        fixture.lane_git(&["add", "resumed.txt"])?;
        fixture.commit(&fixture.writer.worktree, "resumable publication")
    })?;
    assert!(!review);
    fixture.lane_git(&["cat-file", "-e", "HEAD:resumed.txt"])?;
    assert_eq!(
        std::fs::read(fixture.writer.worktree.join("resumed.txt"))?,
        b"durable branch\n"
    );
    assert!(fixture.root.join("attempt/writer-terminal.json").is_file());
    Ok(())
}

#[test]
fn terminal_attributes_only_the_pinned_head_after_branch_advances()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let review = fixture.terminal_via_inner("origin", None, true, true, false, || {
        std::fs::write(fixture.writer.worktree.join("writer.txt"), "writer\n")?;
        fixture.lane_git(&["add", "writer.txt"])?;
        fixture.commit(&fixture.writer.worktree, "writer")
    })?;
    assert!(!review);
    assert_eq!(
        std::fs::read(fixture.writer.worktree.join("AGENTS.md"))?,
        b"later writer\n"
    );
    Ok(())
}

#[test]
#[allow(clippy::disallowed_methods)]
fn automatic_integration_does_not_hold_application_locks_during_git()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    use provenant_os::fs::LockMode;
    use std::ffi::OsStr;
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut fixture = PrepareFixture::new()?;
    fixture.git(&["push", "origin", "HEAD:refs/heads/integration"])?;
    fixture.request = HostDeliveryContext::destination(
        DeliveryMode::Integrate("origin/integration".into()),
        "lane",
    )?
    .ok_or("destination")?;
    let native = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    let wrapper = fixture.root.join("blocked-fetch-git");
    let entered = fixture.root.join("fetch-entered");
    let fifo = fixture.root.join("fetch.fifo");
    let resume = fixture.root.join("fetch-resume");
    let armed = fixture.root.join("armed");
    let quote = |path: &Path| format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
    fixture.executable(&wrapper, &format!(
        "#!/bin/sh\nfor arg do\nif [ \"$arg\" = fetch ] && [ -e {} ] && [ ! -e {} ]; then\nmkfifo {} || exit 91\n: > {}\nIFS= read -r release < {} || exit 92\nfi\ndone\nexec {} \"$@\"\n",
        quote(&armed), quote(&resume), quote(&fifo), quote(&entered), quote(&fifo), quote(&native)
    ))?;
    fixture
        .environment
        .push(("PROVENANT_GIT".into(), wrapper.into_os_string()));
    let host = fixture.prepare()?;
    fixture.commit(&fixture.writer.worktree, "lane for optimistic integration")?;
    let head = host.head(&fixture.writer)?;
    let cancel = provenant_core::CancelToken::new();
    let host = host.cancellable(&cancel);
    let directory = Directory::open_anchor(&fixture.writer.project.join(".git"))?;
    let integrate = format!(
        "provenant-integrate-{}.lock",
        provenant_core::hex(
            provenant_records::CanonicalV1::file_digest(b"origin/integration").bytes()
        )
    );
    // Upgrades may leave the old lock inodes. New commands must not hold them.
    for name in [
        provenant_confinement::worktrees::REPOSITORY_LOCK,
        &integrate,
    ] {
        if directory.metadata_at(OsStr::new(name), false).is_err() {
            directory.create_new(OsStr::new(name))?;
        }
    }
    std::fs::write(&armed, b"ready")?;
    std::thread::scope(
        |scope| -> std::result::Result<(), Box<dyn std::error::Error>> {
            let worker = scope.spawn(|| host.integrate(&fixture.writer, &fixture.request, head));
            let pipe = provenant_test_support::wait_for(
                "integration fetch to wait for release",
                Duration::from_secs(5),
                || {
                    std::fs::OpenOptions::new()
                        .write(true)
                        .custom_flags(libc::O_NONBLOCK)
                        .open(&fifo)
                },
                |result| match result {
                    Ok(_) => true,
                    Err(error) => {
                        error.kind() != std::io::ErrorKind::NotFound
                            && error.raw_os_error() != Some(libc::ENXIO)
                    }
                },
            );
            let available = matches!(&pipe, Ok(Ok(_)))
                && [
                    provenant_confinement::worktrees::REPOSITORY_LOCK,
                    &integrate,
                ]
                .iter()
                .all(|name| {
                    directory
                        .try_lock_existing(OsStr::new(name), LockMode::Exclusive)
                        .is_ok()
                });
            let resume_file = std::fs::write(&resume, b"resume");
            let resumed = match pipe {
                Ok(Ok(mut pipe)) if resume_file.is_ok() => pipe.write_all(b"resume\n"),
                Ok(Err(error)) => Err(error),
                Err(error) => Err(std::io::Error::other(error)),
                _ => Err(std::io::Error::other("could not write resume marker")),
            };
            if resumed.is_err() {
                cancel.cancel();
            }
            let result = worker.join().map_err(|_| "integration panicked")?;
            resume_file?;
            resumed?;
            result?;
            assert!(
                available,
                "automatic integration held an application lock during Git"
            );
            Ok(())
        },
    )?;
    Ok(())
}

#[test]
#[allow(clippy::disallowed_methods)]
fn delivery_cancellation_stops_its_running_git_child()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    let native = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    let wrapper = fixture.root.join("blocked-delivery-git");
    let entered = fixture.root.join("delivery-entered");
    let armed = fixture.root.join("armed");
    let fifo = fixture.root.join("delivery.fifo");
    let quote = |path: &Path| format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
    fixture.executable(&wrapper, &format!("#!/bin/sh\nfor arg do\nif [ \"$arg\" = cat-file ] && [ -e {} ]; then\nmkfifo {} || exit 91\n: > {}\nexec /bin/cat {}\nfi\ndone\nexec {} \"$@\"\n", quote(&armed), quote(&fifo), quote(&entered), quote(&fifo), quote(&native)))?;
    fixture
        .environment
        .push(("PROVENANT_GIT".into(), wrapper.into_os_string()));
    let host = fixture.prepare()?;
    let cancel = provenant_core::CancelToken::new();
    let host = host.cancellable(&cancel);
    std::fs::write(&armed, b"ready")?;
    std::thread::scope(
        |scope| -> std::result::Result<(), Box<dyn std::error::Error>> {
            let worker = scope.spawn(|| host.head(&fixture.writer));
            let seen = provenant_test_support::wait_for(
                "delivery Git child",
                Duration::from_secs(5),
                || entered.exists(),
                |seen| *seen,
            )
            .is_ok();
            cancel.cancel();
            assert!(matches!(
                worker.join().map_err(|_| "delivery panicked")?,
                Err(RunError::Delivery(WriterDeliveryStage::Head))
            ));
            assert!(seen, "delivery never reached Git");
            Ok(())
        },
    )?;
    Ok(())
}

#[test]
fn delivery_cancelled_receipt_preserves_start_without_permanent_failure()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let host = fixture.prepare()?;
    let attempt_path = fixture.root.join("cancelled-attempt");
    std::fs::create_dir(&attempt_path)?;
    let attempt = Directory::open_anchor(&attempt_path)?;
    let cancel = provenant_core::CancelToken::new();
    cancel.cancel();
    let receipt = crate::delivery::deliver_once(
        &host.cancellable(&cancel),
        &fixture.writer,
        &fixture.request,
        &attempt,
        "cancelled test",
    )?;
    assert!(matches!(receipt.state, WriterDeliveryState::Failed { .. }));
    assert!(attempt_path.join("delivery-started.json").exists());
    assert!(attempt_path.join("delivery-cancelled.json").exists());
    assert!(!attempt_path.join("delivery.json").exists());
    assert_eq!(
        crate::delivery::deliver_once(
            &host,
            &fixture.writer,
            &fixture.request,
            &attempt,
            "recovery"
        )?,
        receipt
    );
    Ok(())
}

#[test]
fn publication_cleanup_never_removes_another_import_pin()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let execution = registered_landed_writer_fixture(&fixture)?;
    let (clock, cancel) = (tests::clock(), provenant_core::CancelToken::new());
    let call = tests::call(&clock, &cancel);
    let store = RunStore::open(&execution, &call)?;
    let mut input = tests::input();
    input.request.task.writer = Some(fixture.writer.clone());
    store.create(&input, &call)?;
    let id = input.attempt_id;
    dispatch_tests::seal_exit(&store, &tests::exit())?;
    let record = store.publish_terminal(id, 1, &tests::publication(), &tests::exit(), &call)?;
    let head = fixture.prepare()?.head(&fixture.writer)?;
    let mut launch = crate::prepare_writer(
        &fixture.writer,
        execution.state_root.as_path(),
        execution.project_id,
    )?;
    let (directory, binding) = launch.custody.take().ok_or("writer custody")?;
    let scope = provenant_supervisor::custody::Scope {
        attempt: provenant_core::AttemptId::from_bytes([99; 16]),
        // A live lease never has generation 0; the record's own scope does.
        generation: 1,
    };
    let lease = provenant_supervisor::fixture::acquire_writer_with_binding(
        directory,
        scope,
        binding,
        call.lock_budget(),
        &cancel,
        &clock,
    )?;
    let reference = format!("refs/provenant/import/{head}");
    fixture.git(&["update-ref", &reference, &head.to_string()])?;
    let attempt = store.attempt_dir(id, false)?;
    let mut driver = crate::settlement::Guard::claim(&attempt, RunStore::scope(&record), &clock)?
        .ok_or("old driver")?;
    driver.set_head(head)?;
    drop(driver);
    assert!(store.retry_git_cleanup(&record, &call).is_err());
    fixture.git(&["show-ref", "--verify", &reference])?;
    lease.withdraw_unlaunched()?;
    assert!(matches!(
        store
            .retry_git_cleanup(&record, &call)?
            .cleanup_custody_release,
        EffectDisposition::Done
    ));
    Ok(())
}

#[test]
fn host_push_delivery_refuses_a_non_fast_forward_and_leaves_the_branch()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let home = root.join("home");
    let project = root.join("project");
    let lane = root.join("lane");
    let remote = root.join("remote.git");
    for path in [&home, &project] {
        std::fs::create_dir(path)?;
    }
    let environment = vec![
        ("HOME".into(), home.into_os_string()),
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
    ];
    let host = HostDeliveryContext::new(environment.clone());
    let executor = SystemExecutor::new(1)?;
    let git_executable = provenant_os::git::git_executable().ok_or("git executable")?;
    let git =
        |cwd: &Path, args: &[&str]| -> std::result::Result<String, Box<dyn std::error::Error>> {
            let argv: Vec<OsString> = std::iter::once(git_executable.as_os_str().to_owned())
                .chain(args.iter().map(OsString::from))
                .collect();
            let mut options = ExecOptions::new(&argv, cwd, Duration::from_secs(5));
            options.env = Some(&environment);
            let output = executor.execute(options)?;
            assert_eq!(
                output.termination,
                Termination::Exit(0),
                "{}",
                String::from_utf8_lossy(&output.stderr.bytes)
            );
            Ok(String::from_utf8(output.stdout.bytes)?.trim().into())
        };
    let remote_path = remote.to_str().ok_or("remote")?;
    git(&project, &["init", "-q"])?;
    git(&project, &["config", "user.name", "Fixture"])?;
    git(
        &project,
        &["config", "user.email", "fixture@example.invalid"],
    )?;
    git(&project, &["commit", "--allow-empty", "-qm", "Initial"])?;
    git(&project, &["init", "--bare", "-q", remote_path])?;
    git(&project, &["remote", "add", "origin", remote_path])?;
    git(&project, &["push", "-q", "origin", "HEAD:refs/heads/base"])?;
    // Someone else moved the delivery branch to a commit the lane lacks.
    git(&project, &["commit", "--allow-empty", "-qm", "Elsewhere"])?;
    git(
        &project,
        &["push", "-q", "origin", "HEAD:refs/heads/published/lane"],
    )?;
    let elsewhere = git(&project, &["rev-parse", "HEAD"])?;
    git(
        &project,
        &[
            "worktree",
            "add",
            "-qb",
            "lane",
            lane.to_str().ok_or("lane")?,
            "HEAD~1",
        ],
    )?;
    let store_root = root.join("store");
    let custody_project = registered_writer_project_id(
        &project,
        &store_root.join("home"),
        &store_root.join("checkout"),
        &store_root.join("state"),
    )?;
    let writer = WriterRequest {
        extra: writer_custody_extra(custody_project),
        project,
        worktree: lane.clone(),
    };
    let request = WriterDeliveryRequest {
        extra: Default::default(),
        mode: DeliveryMode::Push,
        remote: "origin".into(),
        branch: "published/lane".into(),
        open_pr: false,
        fast_forward_only: false,
    };
    let host = host.prepare(&writer, &request)?;
    std::fs::write(lane.join("change"), "a repair on a stale base")?;
    git(&lane, &["add", "change"])?;
    git(&lane, &["commit", "-qm", "Repair"])?;
    let fixture = store_root;
    std::fs::create_dir_all(&fixture)?;
    let (clock, cancel) = (tests::clock(), provenant_core::CancelToken::new());
    let call = tests::call(&clock, &cancel);
    let store = RunStore::open(&tests::context(&fixture)?, &call)?;
    let mut input = tests::input();
    input.request.task.writer = Some(writer.clone());
    input.request.task.deliver = Some(request.clone());
    store.create(&input, &call)?;
    let attempt = store.attempt_dir(input.attempt_id, false)?;
    assert!(matches!(
        deliver_once(&host, &writer, &request, &attempt, "fixture receipt"),
        Err(RunError::Delivery(WriterDeliveryStage::Push))
    ));
    assert!(artifacts::read_sealed(&attempt, "delivery.json", RECORD_LIMIT)?.is_none());
    assert!(
        git(&lane, &["ls-remote", "origin", "refs/heads/published/lane"])?
            .starts_with(elsewhere.as_str())
    );
    Ok(())
}

#[test]
fn delivery_retry_reconciles_remote_before_push()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture.commit(&fixture.writer.worktree, "Lane work")?;
    let git = provenant_os::git::git_executable().ok_or("git")?;
    let wrapper = fixture.root.join("git-wrapper");
    let marker = fixture.root.join("refuse-push");
    fixture.executable(&wrapper, &format!(
        "#!/bin/sh\nfor arg in \"$@\"; do if [ \"$arg\" = push ] && [ -f '{}' ]; then exit 42; fi; done\nexec '{}' \"$@\"\n",
        marker.display(), git.display()
    ))?;
    fixture
        .environment
        .push(("PROVENANT_GIT".into(), wrapper.into_os_string()));
    let host = fixture.prepare()?;
    let head = host.head(&fixture.writer)?;
    host.push(&fixture.writer, &fixture.request, head, "fixture")?;
    std::fs::write(marker, "")?;
    // A second push is deliberately made to fail; reconciliation must reuse
    // the already-published head and still complete verification.
    host.push(&fixture.writer, &fixture.request, head, "fixture")?;
    Ok(())
}
