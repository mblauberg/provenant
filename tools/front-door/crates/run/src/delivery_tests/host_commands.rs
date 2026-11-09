use super::*;

#[test]
fn existing_pr_delivery_only_creates_after_positive_absence()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    let base = fixture.prepare()?.head(&fixture.writer)?;
    fixture.commit(&fixture.writer.worktree, "Graph work")?;
    let head = fixture.prepare()?.head(&fixture.writer)?;
    fixture.git(&["branch", "integration", "lane"])?;
    fixture.git(&[
        "config",
        &format!(
            "url.{}.insteadOf",
            fixture.root.join("remote.git").display()
        ),
        "https://github.com/fixture/repo.git",
    ])?;
    let git = provenant_os::git::git_executable().ok_or("git")?;
    let wrapper = fixture.root.join("git-wrapper");
    fixture.executable(&wrapper, &format!("#!/bin/sh\nif [ \"$5\" = remote ] && [ \"$6\" = get-url ]; then printf 'https://github.com/fixture/repo.git\\n'; exit 0; fi\nexec '{}' \"$@\"\n", git.display()))?;
    let gh = fixture.root.join("gh");
    fixture.executable(&gh, "#!/bin/sh\ncd \"$(dirname \"$0\")\" || exit 1\nif [ \"$1\" = repo ]; then printf '{\"defaultBranchRef\":{\"name\":\"main\"}}\\n'; exit 0; fi\nif [ \"$2\" = list ]; then [ ! -f lookup-fails ] || exit 1; cat lookup; exit 0; fi\nif [ \"$2\" = create ]; then echo create >> creates; printf 'https://github.com/fixture/repo/pull/2\\n'; exit 0; fi\nexit 1\n")?;
    fixture
        .environment
        .push(("PROVENANT_GIT".into(), wrapper.into_os_string()));
    fixture.environment.retain(|(key, _)| key != "PATH");
    fixture.environment.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", fixture.root.display()).into(),
    ));
    let url = "https://github.com/fixture/repo/pull/1";
    let other = "1111111111111111111111111111111111111111";
    fixture.request.open_pr = true;
    for graph in [true, false] {
        for (state, item_head, item_base, expected, creates) in [
            ("FAIL", head.to_string(), "main", "blocked", 0),
            ("NONE", head.to_string(), "main", "created", 1),
            ("OPEN", head.to_string(), "main", "reused", 0),
            ("OPEN", head.to_string(), "other", "blocked", 0),
            ("MERGED", head.to_string(), "main", "reused", 0),
            ("CLOSED", head.to_string(), "main", "blocked", 0),
            ("OPEN", other.to_string(), "main", "moved", 0),
        ] {
            let _ = std::fs::remove_file(fixture.root.join("creates"));
            let _ = std::fs::remove_file(fixture.root.join("lookup-fails"));
            if state == "FAIL" {
                std::fs::write(fixture.root.join("lookup-fails"), "")?;
            }
            let value = if state == "NONE" {
                serde_json::json!([])
            } else {
                serde_json::json!([{ "url": url, "state": state, "headRefOid": item_head, "baseRefName": item_base }])
            };
            std::fs::write(fixture.root.join("lookup"), serde_json::to_vec(&value)?)?;
            let result = if graph {
                HostDeliveryContext::new(fixture.environment.clone()).deliver_graph_pr(
                    &fixture.writer.project,
                    "integration",
                    base,
                    "Graph fixture",
                )
            } else {
                fixture
                    .prepare()?
                    .push(&fixture.writer, &fixture.request, head, "Lane fixture")
                    .map(|published| published.pull_request.expect("PR delivery returns a URL"))
            };
            if expected == "blocked" {
                let Err(RunError::DeliveryReconciliation { reason, next }) = &result else {
                    panic!("{state}: expected reconciliation evidence: {result:?}");
                };
                assert!(!reason.is_empty());
                assert!(!next.is_empty());
                if state == "CLOSED" {
                    assert!(next.contains(url), "{next}");
                }
            }
            match expected {
                "created" => assert_eq!(result?, "https://github.com/fixture/repo/pull/2"),
                "reused" => assert_eq!(result?, url),
                "moved" => assert!(matches!(
                    result,
                    Err(RunError::Delivery(WriterDeliveryStage::Verify))
                )),
                _ => assert!(result.is_err(), "{state} must block"),
            }
            let calls = std::fs::read_to_string(fixture.root.join("creates")).unwrap_or_default();
            assert_eq!(calls.lines().count(), creates, "{state}");
        }
    }
    Ok(())
}

#[test]
fn checked_out_parent_integration_defers_without_touching_edits()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    for (overlap, divergent) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut fixture = PrepareFixture::new()?;
        std::fs::write(fixture.writer.worktree.join("child"), "base")?;
        std::fs::write(fixture.writer.worktree.join("live-edit"), "base")?;
        fixture.lane_git(&["add", "child", "live-edit"])?;
        fixture.commit(&fixture.writer.worktree, "baseline")?;
        fixture.git(&["branch", "parent", "lane"])?;
        let parent = fixture.root.join("parent with \"quotes\"");
        fixture.git(&[
            "worktree",
            "add",
            parent.to_str().ok_or("parent")?,
            "parent",
        ])?;
        if divergent {
            std::fs::write(parent.join("parent-commit"), "parent commit")?;
            fixture.git(&[
                "-C",
                parent.to_str().ok_or("parent")?,
                "add",
                "parent-commit",
            ])?;
            fixture.commit(&parent, "parent commit")?;
        }
        fixture.request =
            HostDeliveryContext::destination(DeliveryMode::Integrate("parent".into()), "lane")?
                .ok_or("destination")?;
        let host = fixture.prepare()?;
        std::fs::write(fixture.writer.worktree.join("child"), "child work")?;
        std::fs::write(fixture.writer.worktree.join("live-edit"), "child work")?;
        fixture.lane_git(&["add", "child", "live-edit"])?;
        fixture.commit(&fixture.writer.worktree, "child work")?;
        std::fs::write(parent.join("staged"), "staged parent work")?;
        fixture.git(&["-C", parent.to_str().ok_or("parent")?, "add", "staged"])?;
        std::fs::write(parent.join("staged"), "unstaged parent work")?;
        std::fs::write(
            parent.join(if overlap { "child" } else { "untracked" }),
            "keep",
        )?;
        if overlap {
            fixture.git(&["-C", parent.to_str().ok_or("parent")?, "add", "child"])?;
        }
        let git_dir = host.command(
            &parent,
            "git",
            &[
                "rev-parse",
                "--resolve-git-dir",
                parent.join(".git").to_str().ok_or("git dir")?,
            ],
            WriterDeliveryStage::Verify,
        )?;
        let observe = |args: &[&str]| {
            host.command(
                &parent,
                "git",
                &[
                    &[
                        "--git-dir",
                        git_dir.as_str(),
                        "--work-tree",
                        parent.to_str().unwrap(),
                    ][..],
                    args,
                ]
                .concat(),
                WriterDeliveryStage::Verify,
            )
        };
        let before = observe(&["rev-parse", "HEAD"])?;
        let head = host.head(&fixture.writer)?;
        let index_path = std::path::Path::new(&git_dir).join("index");
        let index = std::fs::read(&index_path)?;
        // The parent's ordinary file writes do not honour Git's index lock.
        // Keep editing an overlapping path while delivery attempts to publish.
        let editing = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let started = std::sync::Barrier::new(2);
        let result = std::thread::scope(|scope| {
            let editing_ref = &editing;
            let parent_ref = &parent;
            let started_ref = &started;
            let editor = scope.spawn(move || -> std::io::Result<()> {
                started_ref.wait();
                while editing_ref.load(std::sync::atomic::Ordering::Relaxed) {
                    std::fs::write(parent_ref.join("live-edit"), "parent editing")?;
                    std::thread::yield_now();
                }
                std::fs::write(parent_ref.join("live-edit"), "parent final edit")
            });
            started.wait();
            let result = host.integrate(&fixture.writer, &fixture.request, head);
            editing.store(false, std::sync::atomic::Ordering::Relaxed);
            editor
                .join()
                .expect("editor thread")
                .expect("parent writes");
            result
        });
        let error = result.expect_err("checked-out parent must defer");
        assert!(
            matches!(error, RunError::IntegrationParentBusy { child_head } if child_head == head)
        );
        assert!(error.to_string().contains("retry delivery"));
        assert!(
            matches!(super::delivery::failed_state(&error), WriterDeliveryState::Failed { stage: WriterDeliveryStage::Integrate, ref extra } if extra.get("reason").and_then(serde_json::Value::as_str).is_some_and(|reason| reason.contains("parent_busy")))
        );
        assert_eq!(std::fs::read(&index_path)?, index);
        assert_eq!(
            std::fs::read_to_string(parent.join("live-edit"))?,
            "parent final edit"
        );
        assert_eq!(
            std::fs::read_to_string(parent.join("staged"))?,
            "unstaged parent work"
        );
        assert_eq!(observe(&["show", ":staged"])?, "staged parent work");
        let integrated = observe(&["rev-parse", "HEAD"])?;
        assert_eq!(integrated, before);
        assert_eq!(
            std::fs::read_to_string(parent.join("child"))?,
            if overlap { "keep" } else { "base" }
        );
        if overlap {
            assert_eq!(observe(&["show", ":child"])?, "keep");
        }
        // Once the parent explicitly relinquishes the branch, publication
        // updates only its ref and leaves the detached dirty checkout alone.
        fixture.git(&[
            "-C",
            parent.to_str().ok_or("parent")?,
            "checkout",
            "--detach",
        ])?;
        host.integrate(&fixture.writer, &fixture.request, head)?;
        let published = host.command(
            &fixture.writer.worktree,
            "git",
            &["rev-parse", "parent"],
            WriterDeliveryStage::Verify,
        )?;
        assert!(
            host.command(
                &fixture.writer.worktree,
                "git",
                &["merge-base", "--is-ancestor", &head.to_string(), &published],
                WriterDeliveryStage::Verify
            )
            .is_ok()
        );
        assert_eq!(
            std::fs::read_to_string(parent.join("child"))?,
            if overlap { "keep" } else { "base" }
        );
        assert_eq!(
            std::fs::read_to_string(parent.join(if overlap { "child" } else { "untracked" }))?,
            "keep"
        );
        assert_eq!(host.head(&fixture.writer)?, head);
    }
    Ok(())
}

#[test]
fn budget_commit_uses_bounded_git_without_running_hooks()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    let fixture = PrepareFixture::new()?;
    let sentinel = fixture.root.join("hook-ran");
    let hooks = fixture.writer.project.join(".git/hooks");
    fixture.git(&[
        "config",
        "core.hooksPath",
        hooks.to_str().ok_or("hook path")?,
    ])?;
    fixture.terminal_via_inner("origin", None, false, false, true, || {
        std::fs::write(fixture.writer.worktree.join("budget.txt"), "saved\n")?;
        let hook = fixture.writer.project.join(".git/hooks/pre-commit");
        std::fs::write(
            &hook,
            format!("#!/bin/sh\ntouch '{}'\nexit 1\n", sentinel.display()),
        )?;
        std::fs::set_permissions(hook, std::fs::Permissions::from_mode(0o700))?;
        Ok(())
    })?;
    assert!(!sentinel.exists());
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    let output = provenant_os::fixture::git_command_at(&git)
        .env_clear()
        .envs(fixture.environment.iter().map(|(k, v)| (k, v)))
        .current_dir(&fixture.writer.worktree)
        .args(["show", "HEAD:budget.txt"])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"saved\n");
    Ok(())
}

#[test]
fn writer_preparation_preserves_command_repository_fact_caps()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::{
        utility::UtilityError,
        worktrees::{RepoFacts, WorktreeError},
    };
    let fixture = PrepareFixture::new()?;
    let execution = registered_landed_writer_fixture(&fixture)?;
    let (clock, cancel) = (tests::clock(), CancelToken::new());
    let call = tests::call(&clock, &cancel);
    for (inventory, scalar) in [
        (provenant_os::exec::DEFAULT_CAPTURE_LIMIT, 1),
        (1, provenant_os::git::REPO_FACT_SCALAR_LIMIT),
    ] {
        let command_facts = RepoFacts::with_limits(inventory, scalar)?;
        let store = RunStore::open(&execution, &call)?.with_repo_facts(&command_facts);
        let result = crate::prepare_writer_with_facts(
            &fixture.writer,
            &store.writer_registry,
            execution.project_id,
            store.repo_facts(),
        );
        assert!(matches!(
            result,
            Err(RunError::Writer(WorktreeError::Utility(
                UtilityError::Truncated { stream: "stdout" }
            )))
        ));
    }
    // The fixture remains admissible; a cap failure never leaves writer custody.
    crate::prepare_writer(
        &fixture.writer,
        execution.state_root.as_path(),
        execution.project_id,
    )?;
    Ok(())
}

#[test]
fn host_delivery_repository_facts_use_command_caps()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::{
        utility::UtilityError,
        worktrees::{RepoFacts, WorktreeError},
    };
    let fixture = PrepareFixture::new()?;
    let host = fixture.prepare()?;
    let inventory = RepoFacts::with_limits(1, provenant_os::git::REPO_FACT_SCALAR_LIMIT)?;
    let scalar = RepoFacts::with_limits(provenant_os::exec::DEFAULT_CAPTURE_LIMIT, 1)?;
    let truncated = |error: RunError| {
        matches!(
            error,
            RunError::DeliveryFact {
                source: WorktreeError::Utility(UtilityError::Truncated { stream: "stdout" }),
                ..
            }
        )
    };
    let listing_error = host
        .clone()
        .with_repo_facts(&inventory)
        .command(
            &fixture.writer.worktree,
            "git",
            &["worktree", "list", "--porcelain"],
            WriterDeliveryStage::Integrate,
        )
        .unwrap_err();
    assert!(matches!(
        listing_error,
        RunError::DeliveryLimit {
            stage: WriterDeliveryStage::Integrate,
            tool: "git",
            command,
            limit: 1,
        } if command == "worktree list"
    ));
    for args in [
        &["rev-parse", "--absolute-git-dir"][..],
        &["rev-parse", "--verify", "HEAD"],
        &["rev-parse", "--verify", "HEAD~0"],
    ] {
        assert!(truncated(
            host.clone()
                .with_repo_facts(&scalar)
                .command(
                    &fixture.writer.worktree,
                    "git",
                    args,
                    WriterDeliveryStage::Head
                )
                .unwrap_err()
        ));
    }
    assert!(truncated(
        HostDeliveryContext::new(fixture.environment.clone())
            .with_repo_facts(&scalar)
            .prepare(&fixture.writer, &fixture.request)
            .unwrap_err()
    ));
    for error in [
        HostDeliveryContext::new(fixture.environment.clone())
            .with_repo_facts(&inventory)
            .prepare(&fixture.writer, &fixture.request)
            .unwrap_err(),
        HostDeliveryContext::new(fixture.environment.clone())
            .with_repo_facts(&inventory)
            .check_delivery_branch_with_facts(&fixture.writer.project, "unprotected")
            .unwrap_err(),
    ] {
        assert!(matches!(
            error,
            RunError::DeliveryLimit {
                tool: "git",
                command,
                limit: 1,
                ..
            } if command == "worktree list"
        ));
    }
    let head = host.head(&fixture.writer)?;
    let error = host
        .clone()
        .with_repo_facts(&inventory)
        .integrate(&fixture.writer, &fixture.request, head)
        .unwrap_err();
    assert!(
        matches!(
            &error,
            RunError::DeliveryLimit {
                stage: WriterDeliveryStage::Integrate,
                tool: "git",
                command,
                limit: 1,
            } if command == "worktree list"
        ),
        "{error:?}"
    );
    assert!(matches!(
        HostDeliveryContext::new(fixture.environment.clone())
            .with_repo_facts(&inventory)
            .deliver_graph_pr(&fixture.writer.project, "unprotected", head, "body")
            .unwrap_err(),
        RunError::DeliveryLimit {
            tool: "git",
            command,
            limit: 1,
            ..
        } if command == "worktree list"
    ));
    assert!(truncated(
        host.with_repo_facts(&scalar)
            .head(&fixture.writer)
            .unwrap_err()
    ));
    Ok(())
}
