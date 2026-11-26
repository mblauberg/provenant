use super::*;

// Spec: docs/specs/run.md#stop-intent
#[test]
fn stopped_head_push_uses_launch_binding_and_keeps_dirty_paths()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let request = HostDeliveryContext::destination(
        DeliveryMode::Push,
        "lane",
        &crate::forge_for("origin", "main", &[]),
    )?
    .ok_or("stop-publish destination")?;
    let host = HostDeliveryContext::new(fixture.environment.clone())
        .with_host_state(&fixture.root.join("state"))
        .prepare_stop_publish(&fixture.writer, &request)?;
    let (attempt_path, _) = crate::tests::scratch_layout(
        &fixture.root.join("state"),
        provenant_core::AttemptId::from_bytes([0x53; 16]),
    )?;
    provenant_os::fixture::private_state_dir(&attempt_path)?;
    let attempt = provenant_os::fs::Directory::open_anchor(&attempt_path)?;
    let scope = provenant_records::custody::Scope {
        attempt: provenant_core::AttemptId::from_bytes([0x53; 16]),
        generation: 1,
    };
    host.seal_stop_publish_binding(&attempt, scope)?;

    std::fs::write(fixture.writer.worktree.join("committed.txt"), "committed\n")?;
    fixture.lane_git(&["add", "committed.txt"])?;
    fixture.commit(&fixture.writer.worktree, "stopped writer commit")?;
    let head = fixture.prepare()?.head(&fixture.writer)?;
    std::fs::write(
        fixture.writer.worktree.join("unfinished.txt"),
        "keep dirty\n",
    )?;

    let stopped = CancelToken::new();
    stopped.cancel();
    assert!(
        host.clone()
            .cancellable(&stopped)
            .push_stopped_head(&fixture.writer, &attempt, scope, head)
            .is_err(),
        "stop publication must propagate the settlement cancellation token"
    );
    host.push_stopped_head(&fixture.writer, &attempt, scope, head)?;

    let remote = fixture.root.join("remote.git");
    let remote = remote.to_str().ok_or("bare remote path")?;
    let remote_head = String::from_utf8(provenant_os::fixture::run_git(
        &fixture.writer.worktree,
        &["--git-dir", remote, "rev-parse", "refs/heads/lane"],
    )?)?;
    assert_eq!(remote_head.trim(), head.to_string());
    let status = String::from_utf8(provenant_os::fixture::run_git(
        &fixture.writer.worktree,
        &["status", "--porcelain", "--untracked-files=all"],
    )?)?;
    assert!(status.contains("unfinished.txt"), "{status}");
    Ok(())
}

// Spec: docs/specs/run.md#stop-intent
#[test]
fn stop_publish_preparation_leaves_the_launch_pr_delivery_untouched()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture.request.open_pr = true;
    let git = provenant_os::git::git_executable().ok_or("git")?;
    let wrapper = fixture.root.join("git-wrapper");
    fixture.executable(
        &wrapper,
        &format!(
            "#!/bin/sh\nif [ \"$5\" = remote ] && [ \"$6\" = get-url ]; then printf 'https://github.com/fixture/repo.git\\n'; exit 0; fi\nexec '{}' \"$@\"\n",
            git.display()
        ),
    )?;
    fixture.executable(&fixture.root.join("gh"), "#!/bin/sh\nexit 0\n")?;
    fixture
        .environment
        .push(("PROVENANT_GIT".into(), wrapper.into_os_string()));
    fixture.environment.retain(|(key, _)| key != "PATH");
    fixture.environment.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", fixture.root.display()).into(),
    ));
    let launch = fixture.prepare()?;
    let (git_before, gh_before, binding_before, stop_before, environment_before) =
        launch.launch_state_for_test();
    assert!(gh_before.is_some(), "an open_pr launch prepares gh");
    assert!(binding_before.is_some() && stop_before.is_none());

    let request = HostDeliveryContext::destination(
        DeliveryMode::Push,
        "lane",
        &crate::forge_for("origin", "main", &[]),
    )?
    .ok_or("stop-publish destination")?;
    let stopped = launch
        .clone()
        .prepare_stop_publish(&fixture.writer, &request)?;
    let (git_after, gh_after, binding_after, stop_after, environment_after) =
        stopped.launch_state_for_test();
    assert_eq!(gh_after, gh_before, "launch gh unchanged");
    assert_eq!(git_after, git_before);
    assert!(binding_after == binding_before, "launch binding unchanged");
    assert_eq!(environment_after, environment_before);
    let stop_after = stop_after.ok_or("stop-publish binding present")?;
    assert!(Some(stop_after) != binding_after, "bindings are distinct");
    Ok(())
}

// Spec: docs/specs/run-launch.md#writer-binding
#[test]
fn launch_and_stop_publication_share_one_remote_refresh()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    let git = provenant_os::git::git_executable().ok_or("git")?;
    let wrapper = fixture.root.join("git-wrapper");
    let refreshes = fixture.root.join("remote-refreshes");
    fixture.executable(
        &wrapper,
        &format!(
            "#!/bin/sh\nfor arg in \"$@\"; do if [ \"$arg\" = ls-remote ]; then printf '%s\\n' \"$*\" >> '{}'; break; fi; done\nexec '{}' \"$@\"\n",
            refreshes.display(),
            git.display()
        ),
    )?;
    fixture
        .environment
        .push(("PROVENANT_GIT".into(), wrapper.into_os_string()));

    let launch = fixture.prepare()?;
    let request = HostDeliveryContext::destination(
        DeliveryMode::Push,
        "lane",
        &crate::forge_for("origin", "main", &[]),
    )?
    .ok_or("stop-publish destination")?;
    let stopped = launch.prepare_stop_publish(&fixture.writer, &request)?;
    assert!(
        stopped.has_stop_publish(),
        "stop publication stays bound without a second fetch"
    );

    let log = std::fs::read_to_string(&refreshes).unwrap_or_default();
    assert_eq!(
        log.lines().count(),
        1,
        "delivery and stop publication share the launch's remote refresh: {log}"
    );
    Ok(())
}

// Regression for #3888 F1: a local-integration launch deliberately records an
// empty remote. Stop publication must still name its own real remote, and the
// writer's stop-work evidence must be gathered against that remote rather than
// treating the sealed sentinel as a corrupt name.
#[test]
fn local_integration_stop_publication_uses_the_stop_remote()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture.request = WriterDeliveryRequest {
        extra: Default::default(),
        mode: DeliveryMode::Integrate("main".into()),
        remote: String::new(),
        branch: "main".into(),
        open_pr: false,
        fast_forward_only: false,
    };
    let launch = fixture.prepare()?;
    let stop_request = HostDeliveryContext::destination(
        DeliveryMode::Push,
        "lane",
        &crate::forge_for("origin", "", &[]),
    )?
    .ok_or("stop-publish destination")?;
    let host = launch.prepare_stop_publish(&fixture.writer, &stop_request)?;
    assert_eq!(host.delivery_remote()?, None);
    assert_eq!(
        host.stop_publish_remote()?,
        Some(provenant_config::RemoteName::new("origin")?)
    );

    // A tracking ref at the verified head makes the ahead count zero, so the
    // summary's evidence comes from the stop remote without needing a push.
    fixture.git(&["update-ref", "refs/remotes/origin/lane", "HEAD"])?;
    fixture.terminal_via_with(
        "origin",
        None,
        TerminalFixtureOptions::default(),
        Some(crate::writer::StopWorkPolicy {
            trigger: provenant_records::run::StopWorkTrigger::ExplicitCancellation,
            publish: Some(&host),
            writer: &fixture.writer,
            route: None,
            publish_granted: false,
        }),
        || Ok(()),
    )?;
    let attempt = provenant_os::fs::Directory::open_anchor(&fixture.attempt_layout()?.0)?;
    let terminal: writer::WriterTerminal = db::decode(
        &artifacts::read_sealed(&attempt, "writer-terminal.json", RECORD_LIMIT)?
            .ok_or("writer terminal")?,
    )?;
    let summary = terminal.stop_work.as_ref().ok_or("stop-work summary")?;
    assert_eq!(
        summary.ahead,
        Some(0),
        "the stop-publish remote must name the ahead count: {summary:?}"
    );
    assert_eq!(
        summary.push,
        provenant_records::run::StopWorkPush::NotGranted
    );
    Ok(())
}

// The same local-integration delivery with no stop-publish binding has no
// remote at all: settlement seals a stop-work summary with no ahead evidence,
// without refusing the empty sealed sentinel.
#[test]
fn local_integration_without_stop_publish_leaves_the_stop_remote_absent()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture.git(&["remote", "remove", "origin"])?;
    fixture.request = WriterDeliveryRequest {
        extra: Default::default(),
        mode: DeliveryMode::Integrate("main".into()),
        remote: String::new(),
        branch: "main".into(),
        open_pr: false,
        fast_forward_only: false,
    };
    let launch = fixture.prepare()?;
    assert_eq!(launch.delivery_remote()?, None);
    assert_eq!(launch.stop_publish_remote()?, None);

    fixture.terminal_via_with(
        "origin",
        None,
        TerminalFixtureOptions::default(),
        Some(crate::writer::StopWorkPolicy {
            trigger: provenant_records::run::StopWorkTrigger::ExplicitCancellation,
            publish: None,
            writer: &fixture.writer,
            route: None,
            publish_granted: false,
        }),
        || Ok(()),
    )?;
    let attempt = provenant_os::fs::Directory::open_anchor(&fixture.attempt_layout()?.0)?;
    let terminal: writer::WriterTerminal = db::decode(
        &artifacts::read_sealed(&attempt, "writer-terminal.json", RECORD_LIMIT)?
            .ok_or("writer terminal")?,
    )?;
    assert_eq!(
        terminal
            .stop_work
            .as_ref()
            .ok_or("stop-work summary")?
            .ahead,
        None
    );
    Ok(())
}

#[test]
fn integration_refuses_default_fallbacks_remote_head_and_primary_branch()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture.git(&["branch", "-m", "primary-live"])?;
    let forge = crate::forge_for("origin", "main", &[]);
    // Protection now comes from the one resolved context: its default branch
    // and the primary checkout. The old compiled-in `master` fallback is gone.
    // Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
    for target in ["main", "origin/main", "primary-live", "origin/primary-live"] {
        fixture.request = HostDeliveryContext::destination(
            DeliveryMode::Integrate(target.into()),
            "lane",
            &forge,
        )?
        .ok_or("destination")?;
        assert!(
            matches!(
                fixture.prepare_with_forge(&forge),
                Err(RunError::ProtectedDeliveryBranch)
            ),
            "{target}"
        );
    }
    fixture.request =
        HostDeliveryContext::destination(DeliveryMode::Integrate("master".into()), "lane", &forge)?
            .ok_or("destination")?;
    assert!(
        fixture.prepare_with_forge(&forge).is_ok(),
        "master is not a compiled-in default any more"
    );

    // A declared protected branch is refused by the delivery guard itself,
    // not only by admission.
    let declared = crate::forge_for("origin", "main", &["release"]);
    fixture.request = HostDeliveryContext::destination(
        DeliveryMode::Integrate("release".into()),
        "lane",
        &declared,
    )?
    .ok_or("destination")?;
    assert!(matches!(
        fixture.prepare_with_forge(&declared),
        Err(RunError::ProtectedDeliveryBranch)
    ));

    // A remote HEAD names a default only for the context that resolved it.
    fixture.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/trunk",
    ])?;
    let trunk = crate::forge_for("origin", "trunk", &[]);
    for target in ["trunk", "origin/trunk"] {
        fixture.request = HostDeliveryContext::destination(
            DeliveryMode::Integrate(target.into()),
            "lane",
            &trunk,
        )?
        .ok_or("destination")?;
        assert!(
            matches!(
                fixture.prepare_with_forge(&trunk),
                Err(RunError::ProtectedDeliveryBranch)
            ),
            "{target}"
        );
    }

    // Integration re-checks protection with the context it is given.
    fixture.git(&["branch", "integration"])?;
    fixture.request = HostDeliveryContext::destination(
        DeliveryMode::Integrate("integration".into()),
        "lane",
        &forge,
    )?
    .ok_or("destination")?;
    let host = fixture.prepare_with_forge(&forge)?;
    fixture.commit(&fixture.writer.worktree, "lane change")?;
    fixture.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/integration",
    ])?;
    let head = host.head(&fixture.writer)?;
    let integration = crate::forge_for("origin", "integration", &[]);
    assert!(matches!(
        host.integrate(&fixture.writer, &fixture.request, head, Some(&integration)),
        Err(RunError::Delivery {
            stage: WriterDeliveryStage::Integrate,
            ..
        })
    ));
    fixture.git(&[
        "symbolic-ref",
        "refs/heads/alias",
        "refs/heads/primary-live",
    ])?;
    fixture.request =
        HostDeliveryContext::destination(DeliveryMode::Integrate("alias".into()), "lane", &forge)?
            .ok_or("destination")?;
    assert!(matches!(
        fixture.prepare_with_forge(&forge),
        Err(RunError::DeliveryPrepare {
            step: DeliveryPrepareStep::IntegrationTarget,
            ..
        })
    ));
    Ok(())
}

#[test]
fn integration_rechecks_primary_checkout_after_preparation()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture.git(&["branch", "integration"])?;
    fixture.request = HostDeliveryContext::destination(
        DeliveryMode::Integrate("integration".into()),
        "lane",
        &crate::forge_for("origin", "main", &[]),
    )?
    .ok_or("destination")?;

    // Preparation caches the initial primary checkout listing in these facts.
    let facts = provenant_confinement::worktrees::RepoFacts::default();
    let host = HostDeliveryContext::new(fixture.environment.clone())
        .with_repo_facts(&facts)
        .prepare(&fixture.writer, &fixture.request)?;

    fixture.commit(&fixture.writer.worktree, "provider output")?;
    fixture.git(&["checkout", "integration"])?;

    let head = host.head(&fixture.writer)?;
    assert!(matches!(
        host.integrate(
            &fixture.writer,
            &fixture.request,
            head,
            Some(&crate::forge_for("origin", "main", &[]))
        ),
        Err(RunError::Delivery {
            stage: WriterDeliveryStage::Integrate,
            ..
        })
    ));
    Ok(())
}

#[test]
fn integration_protection_follows_the_resolved_forge_not_remote_heads()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture.git(&["branch", "-m", "primary-live"])?;
    fixture.git(&[
        "remote",
        "add",
        "upstream",
        fixture.root.join("remote.git").to_str().ok_or("remote")?,
    ])?;
    for (remote, branch) in [("origin", "trunk"), ("upstream", "release")] {
        fixture.git(&[
            "symbolic-ref",
            &format!("refs/remotes/{remote}/HEAD"),
            &format!("refs/remotes/{remote}/{branch}"),
        ])?;
    }
    fixture.git(&["branch", "release"])?;
    fixture.git(&["config", "branch.release.remote", "upstream"])?;
    fixture.git(&["config", "branch.release.merge", "refs/heads/release"])?;

    // The resolved context is origin's. A different remote's HEAD never
    // decides protection for this command, and neither does the branch's own
    // upstream configuration.
    // Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
    let origin = crate::forge_for("origin", "trunk", &[]);
    fixture.request = HostDeliveryContext::destination(
        DeliveryMode::Integrate("release".into()),
        "lane",
        &origin,
    )?
    .ok_or("destination")?;
    assert!(fixture.prepare_with_forge(&origin).is_ok());
    assert!(
        HostDeliveryContext::validate_integration_target(
            &fixture.writer.project,
            "release",
            &origin
        )
        .is_ok()
    );

    // Declaring the branch protected in that one context refuses it at both
    // admission and delivery.
    let protected = crate::forge_for("origin", "trunk", &["release"]);
    fixture.request = HostDeliveryContext::destination(
        DeliveryMode::Integrate("release".into()),
        "lane",
        &protected,
    )?
    .ok_or("destination")?;
    assert!(matches!(
        fixture.prepare_with_forge(&protected),
        Err(RunError::ProtectedDeliveryBranch)
    ));
    assert!(matches!(
        HostDeliveryContext::validate_integration_target(
            &fixture.writer.project,
            "release",
            &protected
        ),
        Err(RunError::ProtectedDeliveryBranch)
    ));
    Ok(())
}

#[test]
fn graph_publication_recovers_deleted_marked_ref_and_refuses_conflicts()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let host = fixture.prepare()?;
    let head = host.head(&fixture.writer)?;
    let publication = host.graph_publication_branch(&fixture.writer.project, head)?;
    let reference = format!("refs/heads/{publication}");
    fixture.git(&["update-ref", "-d", &reference, &head.to_string()])?;
    assert_eq!(
        host.graph_publication_branch(&fixture.writer.project, head)?,
        publication
    );
    assert_eq!(
        host.command(
            &fixture.writer.project,
            "git",
            &["rev-parse", "--verify", &reference],
            WriterDeliveryStage::Push
        )?,
        head.to_string()
    );

    fixture.commit(&fixture.writer.worktree, "other graph head")?;
    let other = host.head(&fixture.writer)?;
    fixture.git(&[
        "update-ref",
        &reference,
        &other.to_string(),
        &head.to_string(),
    ])?;
    assert!(matches!(
        host.graph_publication_branch(&fixture.writer.project, head),
        Err(RunError::Delivery {
            stage: WriterDeliveryStage::Push,
            ..
        })
    ));
    assert_eq!(
        host.command(
            &fixture.writer.project,
            "git",
            &["rev-parse", "--verify", &reference],
            WriterDeliveryStage::Push
        )?,
        other.to_string()
    );

    let common = host.common_git_dir(&fixture.writer.project, WriterDeliveryStage::Push)?;
    let directory = provenant_os::fs::Directory::open_anchor(&common)?;
    artifacts::seal(
        &directory,
        &format!("provenant-graph-publication-{other}.json"),
        publication.as_bytes(),
        RECORD_LIMIT,
    )?;
    assert!(matches!(
        host.graph_publication_branch(&fixture.writer.project, other),
        Err(RunError::Delivery {
            stage: WriterDeliveryStage::Push,
            ..
        })
    ));
    let absent = format!("refs/heads/provenant/graph/{other}");
    assert!(
        host.command(
            &fixture.writer.project,
            "git",
            &["show-ref", "--verify", "--quiet", &absent],
            WriterDeliveryStage::Push
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn graph_publication_recovers_a_ref_created_before_its_marker()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let host = fixture.prepare()?;
    let head = host.head(&fixture.writer)?;
    let publication = format!("provenant/graph/{head}");
    // Simulate a crash after update-ref but before the ownership marker.
    fixture.git(&["branch", &publication, &head.to_string()])?;
    let common = host.common_git_dir(&fixture.writer.project, WriterDeliveryStage::Push)?;
    let directory = provenant_os::fs::Directory::open_anchor(&common)?;
    let marker = format!("provenant-graph-publication-{head}.json");
    assert!(artifacts::read_sealed(&directory, &marker, RECORD_LIMIT)?.is_none());
    assert_eq!(
        host.graph_publication_branch(&fixture.writer.project, head)?,
        publication
    );
    assert_eq!(
        artifacts::read_sealed(&directory, &marker, RECORD_LIMIT)?.as_deref(),
        Some(publication.as_bytes())
    );
    assert_eq!(
        host.graph_publication_branch(&fixture.writer.project, head)?,
        publication
    );

    fixture.commit(&fixture.writer.worktree, "different graph head")?;
    let head = host.head(&fixture.writer)?;
    let publication = format!("provenant/graph/{head}");
    fixture.git(&["branch", &publication])?;
    assert!(
        host.graph_publication_branch(&fixture.writer.project, head)
            .is_err()
    );
    fixture.git(&["branch", "-d", &publication])?;
    fixture.git(&[
        "symbolic-ref",
        &format!("refs/heads/{publication}"),
        "refs/heads/missing",
    ])?;
    assert!(
        host.graph_publication_branch(&fixture.writer.project, head)
            .is_err()
    );
    fixture.git(&[
        "symbolic-ref",
        "--delete",
        &format!("refs/heads/{publication}"),
    ])?;
    assert_eq!(
        host.graph_publication_branch(&fixture.writer.project, head)?,
        publication
    );
    assert_eq!(
        host.graph_publication_branch(&fixture.writer.project, head)?,
        publication
    );
    Ok(())
}

#[test]
fn delivery_records_dirty_failure_and_checked_out_integration_after_start()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    for dirty in [true, false] {
        let mut fixture = PrepareFixture::new()?;
        fixture.git(&["branch", "integration"])?;
        fixture.request = HostDeliveryContext::destination(
            DeliveryMode::Integrate("integration".into()),
            "lane",
            &crate::forge_for("origin", "main", &[]),
        )?
        .ok_or("destination")?;
        let host = fixture.prepare()?;
        fixture.commit(&fixture.writer.worktree, "lane change")?;
        if dirty {
            std::fs::write(fixture.writer.worktree.join("dirty"), "keep")?;
        } else {
            fixture.git(&[
                "worktree",
                "add",
                fixture.root.join("checked-out").to_str().ok_or("tree")?,
                "integration",
            ])?;
        }
        let (path, _) = fixture.attempt_layout()?;
        let attempt = provenant_os::fs::Directory::open_anchor(&path)?;
        let receipt = deliver_once(
            &host,
            &fixture.writer,
            &fixture.request,
            &attempt,
            "Run fixture\nAttempt fixture",
        )?;
        assert!(matches!(receipt.state, WriterDeliveryState::Failed { .. }));
        if !dirty {
            assert!(matches!(receipt.state, WriterDeliveryState::Failed {
                stage: WriterDeliveryStage::Integrate, ref reason, ..
            } if reason.as_deref().is_some_and(|reason| reason.contains("parent_busy"))));
        }
        let raw =
            artifacts::read_sealed(&attempt, "delivery.json", RECORD_LIMIT)?.ok_or("delivery")?;
        assert_eq!(db::decode::<WriterDeliveryReceipt>(&raw)?, receipt);
        assert!(artifacts::read_sealed(&attempt, "delivery-started.json", RECORD_LIMIT)?.is_some());
    }
    Ok(())
}

#[test]
fn two_lanes_contend_for_integration_without_losing_either_head()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture.git(&["branch", "integration"])?;
    fixture.request = HostDeliveryContext::destination(
        DeliveryMode::Integrate("integration".into()),
        "lane",
        &crate::forge_for("origin", "main", &[]),
    )?
    .ok_or("destination")?;
    let first = fixture.prepare()?;
    let second_writer = WriterRequest {
        extra: fixture.writer.extra.clone(),
        custody_project: fixture.writer.custody_project,
        project: fixture.writer.project.clone(),
        worktree: fixture.root.join("second-lane"),
    };
    fixture.git(&[
        "worktree",
        "add",
        "-qb",
        "second-lane",
        second_writer.worktree.to_str().ok_or("tree")?,
    ])?;
    let second = HostDeliveryContext::new(fixture.environment.clone())
        .prepare(&second_writer, &fixture.request)?;
    fixture.commit(&fixture.writer.worktree, "first lane")?;
    fixture.commit(&second_writer.worktree, "second lane")?;
    let first_head = first.head(&fixture.writer)?;
    let second_head = second.head(&second_writer)?;
    let barrier = std::sync::Barrier::new(2);
    let (one, two) = std::thread::scope(|scope| {
        let one = scope.spawn(|| {
            barrier.wait();
            first.integrate(
                &fixture.writer,
                &fixture.request,
                first_head,
                Some(&crate::forge_for("origin", "main", &[])),
            )
        });
        let two = scope.spawn(|| {
            barrier.wait();
            second.integrate(
                &second_writer,
                &fixture.request,
                second_head,
                Some(&crate::forge_for("origin", "main", &[])),
            )
        });
        (
            one.join().expect("first lane thread"),
            two.join().expect("second lane thread"),
        )
    });
    // Optimistic publication can refuse the loser. Explicit host pickup uses
    // the new destination tip and preserves both contributions.
    if let Err(error) = one {
        assert!(
            matches!(
                error,
                RunError::Delivery {
                    stage: WriterDeliveryStage::Integrate,
                    ..
                }
            ),
            "{error}"
        );
        first.integrate(
            &fixture.writer,
            &fixture.request,
            first_head,
            Some(&crate::forge_for("origin", "main", &[])),
        )?;
    }
    if let Err(error) = two {
        assert!(
            matches!(
                error,
                RunError::Delivery {
                    stage: WriterDeliveryStage::Integrate,
                    ..
                }
            ),
            "{error}"
        );
        second.integrate(
            &second_writer,
            &fixture.request,
            second_head,
            Some(&crate::forge_for("origin", "main", &[])),
        )?;
    }
    for head in [first_head, second_head] {
        first.command(
            &fixture.writer.project,
            "git",
            &[
                "merge-base",
                "--is-ancestor",
                &head.to_string(),
                "integration",
            ],
            WriterDeliveryStage::Verify,
        )?;
    }
    Ok(())
}

#[test]
fn graph_pr_body_uses_its_recorded_base_despite_primary_checkout_changes()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture.commit(&fixture.writer.worktree, "Unrelated integration history")?;
    let host = fixture.prepare()?;
    let base = host.head(&fixture.writer)?;
    fixture.commit(&fixture.writer.worktree, "Graph work")?;
    fixture.git(&["branch", "integration", "lane"])?;
    fixture.commit(&fixture.writer.project, "Unrelated primary checkout")?;
    // Only credential/hosting boundaries are synthetic. Git publication uses
    // the real repository and a local bare remote, with no network access.
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
    fixture.executable(&gh, "#!/bin/sh\nif [ \"$1\" = repo ]; then printf '{\"defaultBranchRef\":{\"name\":\"main\"}}\\n'; exit 0; fi\nif [ \"$2\" = list ]; then printf '[]\\n'; exit 0; fi\ncd \"$(dirname \"$0\")\" || exit 1\ncat > graph-body\nprintf 'https://github.com/fixture/repo/pull/1\\n'\n")?;
    fixture
        .environment
        .push(("PROVENANT_GIT".into(), wrapper.into_os_string()));
    fixture.environment.retain(|(key, _)| key != "PATH");
    fixture.environment.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", fixture.root.display()).into(),
    ));
    let url = HostDeliveryContext::new(fixture.environment.clone()).deliver_graph_pr(
        &fixture.writer.project,
        "integration",
        base,
        "Graph fixture\nRun fixture\nAttempt fixture",
        &crate::forge_for("origin", "main", &[]),
    )?;
    assert_eq!(url, "https://github.com/fixture/repo/pull/1");
    let body = std::fs::read_to_string(fixture.root.join("graph-body"))?;
    assert!(
        body.contains("Graph work") && body.contains("Attempt fixture"),
        "{body}"
    );
    assert!(
        !body.contains("Unrelated integration history")
            && !body.contains("Unrelated primary checkout")
            && !body.contains("Initial"),
        "{body}"
    );
    Ok(())
}

#[test]
fn queue_publication_branch_is_stable_for_graph_identity()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let host = fixture.prepare()?;
    let first = host.graph_publication_branch_for(&fixture.writer.project, "queue-item-3972")?;
    let replay = host.graph_publication_branch_for(&fixture.writer.project, "queue-item-3972")?;
    let other = host.graph_publication_branch_for(&fixture.writer.project, "queue-item-3973")?;

    assert_eq!(first, replay);
    assert_ne!(first, other);
    assert!(first.starts_with("provenant/graph/"));
    Ok(())
}

#[test]
fn queue_publication_pushes_the_formatter_head_without_moving_integration_branch()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let base = fixture.prepare()?.head(&fixture.writer)?;
    fixture.git(&["branch", "integration", "lane"])?;
    let integration_before = fixture
        .git_text(&["rev-parse", "refs/heads/integration"])?
        .parse::<GitOid>()?;
    fixture.commit(&fixture.writer.worktree, "formatter commit")?;
    let writer_head = fixture
        .git_text(&[
            "-C",
            fixture.writer.worktree.to_str().ok_or("writer worktree")?,
            "rev-parse",
            "HEAD",
        ])?
        .parse::<GitOid>()?;
    assert_ne!(writer_head, integration_before);

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
    fixture.executable(
        &wrapper,
        &format!(
            "#!/bin/sh\nif [ \"$5\" = remote ] && [ \"$6\" = get-url ]; then printf 'https://github.com/fixture/repo.git\\n'; exit 0; fi\nexec '{}' \"$@\"\n",
            git.display()
        ),
    )?;
    fixture.executable(
        &fixture.root.join("gh"),
        "#!/bin/sh\nif [ \"$1\" = repo ]; then printf '{\"defaultBranchRef\":{\"name\":\"main\"}}\\n'; exit 0; fi\nif [ \"$2\" = list ]; then printf '[]\\n'; exit 0; fi\ncd \"$(dirname \"$0\")\" || exit 1\ncat > graph-body\nprintf 'https://github.com/fixture/repo/pull/1\\n'\n",
    )?;
    let mut environment = fixture.environment.clone();
    environment.push(("PROVENANT_GIT".into(), wrapper.into_os_string()));
    environment.retain(|(key, _)| key != "PATH");
    environment.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", fixture.root.display()).into(),
    ));

    let receipt = HostDeliveryContext::new(environment).deliver_graph_pr_at_head(
        &fixture.writer.project,
        "lane",
        base,
        writer_head,
        "queue-item-3972",
        None,
        "Queue delivery after formatting",
        &crate::forge_for("origin", "main", &[]),
    )?;
    let remote = fixture.root.join("remote.git");
    let remote = remote.to_str().ok_or("bare remote")?;
    let remote_head = fixture.git_text(&[
        "--git-dir",
        remote,
        "rev-parse",
        &format!("refs/heads/{}", receipt.branch),
    ])?;
    let integration_after = fixture
        .git_text(&["rev-parse", "refs/heads/integration"])?
        .parse::<GitOid>()?;

    assert_eq!(receipt.head, writer_head);
    assert_eq!(remote_head.parse::<GitOid>()?, writer_head);
    assert_eq!(integration_after, integration_before);
    Ok(())
}

#[test]
fn adopted_graph_publication_updates_the_existing_pr_under_its_head_lease()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let expected_remote_head = fixture
        .git_text(&["rev-parse", "refs/heads/lane"])?
        .parse::<GitOid>()?;
    fixture.git(&["push", "origin", "lane:refs/heads/lane"])?;
    fixture.commit(&fixture.writer.worktree, "repair adopted pull request")?;
    let expected_head = fixture
        .git_text(&[
            "-C",
            fixture.writer.worktree.to_str().ok_or("writer worktree")?,
            "rev-parse",
            "HEAD",
        ])?
        .parse::<GitOid>()?;
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
    fixture.executable(
        &wrapper,
        &format!(
            "#!/bin/sh\nif [ \"$5\" = remote ] && [ \"$6\" = get-url ]; then printf 'https://github.com/fixture/repo.git\\n'; exit 0; fi\nexec '{}' \"$@\"\n",
            git.display()
        ),
    )?;
    let gh = fixture.root.join("gh");
    fixture.executable(
        &gh,
        &format!(
            r#"#!/bin/sh
cd "$(dirname "$0")" || exit 1
if [ "$1" = repo ]; then printf '{{"defaultBranchRef":{{"name":"main"}}}}\n'; exit 0; fi
if [ "$1" = pr ] && [ "$2" = view ]; then
  count=0
  [ ! -f views ] || count=$(cat views)
  count=$((count + 1))
  printf '%s\n' "$count" > views
  if [ "$count" -eq 1 ]; then oid={old_head}; else oid={new_head}; fi
  printf '{{"url":"https://github.com/fixture/repo/pull/17","state":"OPEN","headRefName":"lane","headRefOid":"%s","headRepository":{{"nameWithOwner":"fixture/repo"}},"baseRefName":"main","baseRepository":{{"nameWithOwner":"fixture/repo"}}}}\n' "$oid"
  exit 0
fi
if [ "$1" = pr ] && [ "$2" = create ]; then touch created; exit 91; fi
exit 1
"#,
            old_head = expected_remote_head,
            new_head = expected_head,
        ),
    )?;
    let mut environment = fixture.environment.clone();
    environment.push(("PROVENANT_GIT".into(), wrapper.into_os_string()));
    environment.retain(|(key, _)| key != "PATH");
    environment.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", fixture.root.display()).into(),
    ));

    let url = "https://github.com/fixture/repo/pull/17";
    let receipt = HostDeliveryContext::new(environment.clone()).update_adopted_graph_pr_at_head(
        &fixture.writer.project,
        expected_head,
        expected_remote_head,
        &GraphAdoptionTarget {
            repository: "fixture/repo",
            pull_request: 17,
            pull_request_url: url,
            branch: "lane",
            base_repository: "fixture/repo",
            base_branch: "main",
        },
        &crate::forge_for("origin", "main", &[]),
    )?;

    let remote_head = fixture.git_text(&[
        "--git-dir",
        fixture
            .root
            .join("remote.git")
            .to_str()
            .ok_or("bare remote")?,
        "rev-parse",
        "refs/heads/lane",
    ])?;
    assert_eq!(receipt.branch, "lane");
    assert_eq!(receipt.head, expected_head);
    assert_eq!(receipt.pull_request, url);
    assert_eq!(remote_head.parse::<GitOid>()?, expected_head);
    assert!(!fixture.root.join("created").exists());
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("views"))?.trim(),
        "2"
    );
    let target = GraphAdoptionTarget {
        repository: "fixture/repo",
        pull_request: 17,
        pull_request_url: url,
        branch: "lane",
        base_repository: "fixture/repo",
        base_branch: "main",
    };
    let replay = HostDeliveryContext::new(environment.clone()).update_adopted_graph_pr_at_head(
        &fixture.writer.project,
        expected_head,
        expected_remote_head,
        &target,
        &crate::forge_for("origin", "main", &[]),
    )?;
    assert_eq!(replay, receipt);

    fixture.commit(&fixture.writer.worktree, "concurrent update")?;
    let concurrent_head = fixture.git_text(&["rev-parse", "refs/heads/lane"])?;
    fixture.git(&["push", "origin", "lane:refs/heads/lane"])?;
    assert!(
        HostDeliveryContext::new(environment)
            .update_adopted_graph_pr_at_head(
                &fixture.writer.project,
                expected_head,
                expected_remote_head,
                &target,
                &crate::forge_for("origin", "main", &[]),
            )
            .is_err()
    );
    let remote_after_refusal = fixture.git_text(&[
        "--git-dir",
        fixture
            .root
            .join("remote.git")
            .to_str()
            .ok_or("bare remote")?,
        "rev-parse",
        "refs/heads/lane",
    ])?;
    assert_eq!(remote_after_refusal, concurrent_head);
    assert!(!fixture.root.join("created").exists());
    Ok(())
}

#[test]
fn graph_pr_refreshes_forged_target_and_refuses_failed_fetch()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    let base = fixture.prepare()?.head(&fixture.writer)?;
    fixture.commit(&fixture.writer.worktree, "Graph work")?;
    fixture.git(&["push", "origin", "lane:refs/heads/integration"])?;
    fixture.git(&[
        "update-ref",
        "refs/remotes/origin/integration",
        &base.to_string(),
    ])?;
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
    fixture.executable(&gh, "#!/bin/sh\nif [ \"$1\" = repo ]; then printf '{\"defaultBranchRef\":{\"name\":\"main\"}}\\n'; exit 0; fi\nif [ \"$2\" = list ]; then printf '[]\\n'; exit 0; fi\ncd \"$(dirname \"$0\")\" || exit 1\ncat > graph-body\nprintf 'https://github.com/fixture/repo/pull/1\\n'\n")?;
    fixture
        .environment
        .push(("PROVENANT_GIT".into(), wrapper.into_os_string()));
    fixture.environment.retain(|(key, _)| key != "PATH");
    fixture.environment.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", fixture.root.display()).into(),
    ));
    HostDeliveryContext::new(fixture.environment.clone()).deliver_graph_pr(
        &fixture.writer.project,
        "origin/integration",
        base,
        "Graph fixture",
        &crate::forge_for("origin", "main", &[]),
    )?;
    assert!(std::fs::read_to_string(fixture.root.join("graph-body"))?.contains("Graph work"));

    fixture.git(&[
        "update-ref",
        "refs/remotes/origin/integration",
        &base.to_string(),
    ])?;
    std::fs::write(
        fixture
            .writer
            .project
            .join(".git/refs/remotes/origin/integration.lock"),
        "stale lock",
    )?;
    assert!(
        HostDeliveryContext::new(fixture.environment.clone())
            .deliver_graph_pr(
                &fixture.writer.project,
                "origin/integration",
                base,
                "Graph fixture",
                &crate::forge_for("origin", "main", &[]),
            )
            .is_err()
    );
    Ok(())
}

#[test]
fn commit_pr_body_is_bounded_and_contains_commit_messages()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let host = fixture.prepare()?;
    fixture.commit(&fixture.writer.worktree, "Commit subject\n\nCommit body")?;
    let body = host.commit_body(
        &fixture.writer,
        host.head(&fixture.writer)?,
        "Run fixture\nAttempt fixture",
    )?;
    assert!(
        body.contains("Commit subject")
            && body.contains("Commit body")
            && body.contains("Attempt fixture")
    );
    let capped = cap_pr_body("\u{1f600}".repeat(30_000));
    assert!(capped.len() <= PR_BODY_LIMIT && capped.ends_with(PR_BODY_TRUNCATION_NOTE));
    fixture.commit(
        &fixture.writer.worktree,
        &format!("Large commit\n\n{}", "\u{1f600}".repeat(20_000)),
    )?;
    let body = host.commit_body(
        &fixture.writer,
        host.head(&fixture.writer)?,
        "Run fixture\nAttempt fixture",
    )?;
    assert!(body.len() <= PR_BODY_LIMIT && body.ends_with(PR_BODY_TRUNCATION_NOTE.trim_end()));
    fixture.commit(&fixture.writer.worktree, &"s".repeat(60_000))?;
    let body = host.commit_body(
        &fixture.writer,
        host.head(&fixture.writer)?,
        "Run fixture\nAttempt fixture",
    )?;
    assert_eq!(
        body,
        format!(
            "Run fixture\nAttempt fixture\n\n{}",
            PR_BODY_TRUNCATION_NOTE.trim()
        )
    );
    Ok(())
}

#[test]
fn remote_integration_refreshes_the_leased_destination()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture.git(&["push", "origin", "HEAD:refs/heads/integration"])?;
    fixture.request = HostDeliveryContext::destination(
        DeliveryMode::Integrate("origin/integration".into()),
        "lane",
        &crate::forge_for("origin", "main", &[]),
    )?
    .ok_or("destination")?;
    let host = fixture.prepare()?;
    fixture.commit(&fixture.writer.worktree, "remote lane")?;
    let head = host.head(&fixture.writer)?;
    host.integrate(
        &fixture.writer,
        &fixture.request,
        head,
        Some(&crate::forge_for("origin", "main", &[])),
    )?;
    assert_eq!(
        host.command(
            &fixture.writer.worktree,
            "git",
            &["rev-parse", "refs/remotes/origin/integration"],
            WriterDeliveryStage::Verify
        )?,
        head.to_string()
    );
    host.integrate(
        &fixture.writer,
        &fixture.request,
        head,
        Some(&crate::forge_for("origin", "main", &[])),
    )?;
    Ok(())
}

#[test]
fn remote_integration_refuses_a_symbolic_destination()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture.git(&["push", "origin", "HEAD:refs/heads/main"])?;
    fixture.git(&[
        "--git-dir",
        fixture.root.join("remote.git").to_str().ok_or("remote")?,
        "symbolic-ref",
        "refs/heads/integration",
        "refs/heads/main",
    ])?;
    fixture.request = HostDeliveryContext::destination(
        DeliveryMode::Integrate("origin/integration".into()),
        "lane",
        &crate::forge_for("origin", "main", &[]),
    )?
    .ok_or("destination")?;
    let host = fixture.prepare()?;
    let original = host.head(&fixture.writer)?;
    fixture.commit(&fixture.writer.worktree, "lane change")?;
    let head = host.head(&fixture.writer)?;
    assert!(matches!(
        host.integrate(
            &fixture.writer,
            &fixture.request,
            head,
            Some(&crate::forge_for("origin", "main", &[]))
        ),
        Err(RunError::Delivery {
            stage: WriterDeliveryStage::Integrate,
            ..
        })
    ));
    let remote = host.command(
        &fixture.writer.project,
        "git",
        &["ls-remote", "origin", "refs/heads/main"],
        WriterDeliveryStage::Verify,
    )?;
    assert_eq!(
        remote.split_whitespace().next(),
        Some(original.to_string().as_str())
    );
    Ok(())
}

#[test]
fn local_integration_fast_forwards_merges_and_replays()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture.git(&["branch", "integration"])?;
    fixture.request = HostDeliveryContext::destination(
        DeliveryMode::Integrate("integration".into()),
        "lane",
        &crate::forge_for("origin", "main", &[]),
    )?
    .ok_or("destination")?;
    let host = fixture.prepare()?;
    fixture.commit(&fixture.writer.worktree, "lane one")?;
    let head = host.head(&fixture.writer)?;
    host.integrate(
        &fixture.writer,
        &fixture.request,
        head,
        Some(&crate::forge_for("origin", "main", &[])),
    )?;
    assert_eq!(
        host.command(
            &fixture.writer.project,
            "git",
            &["rev-parse", "integration"],
            WriterDeliveryStage::Verify
        )?,
        head.to_string()
    );
    host.integrate(
        &fixture.writer,
        &fixture.request,
        head,
        Some(&crate::forge_for("origin", "main", &[])),
    )?;
    // Divergent commits with a common tree still require a two-parent merge.
    fixture.commit(&fixture.writer.project, "integration side")?;
    fixture.git(&["branch", "-f", "integration", "HEAD"])?;
    fixture.commit(&fixture.writer.worktree, "lane two")?;
    let head = host.head(&fixture.writer)?;
    host.integrate(
        &fixture.writer,
        &fixture.request,
        head,
        Some(&crate::forge_for("origin", "main", &[])),
    )?;
    let parents = host.command(
        &fixture.writer.project,
        "git",
        &["rev-list", "--parents", "-n", "1", "integration"],
        WriterDeliveryStage::Verify,
    )?;
    assert_eq!(parents.split_whitespace().count(), 3);
    assert!(parents.ends_with(&head.to_string()));
    Ok(())
}

#[test]
fn fast_forward_only_integration_advances_but_never_merges()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture.git(&["branch", "integration"])?;
    fixture.request = HostDeliveryContext::destination(
        DeliveryMode::Integrate("integration".into()),
        "lane",
        &crate::forge_for("origin", "main", &[]),
    )?
    .ok_or("destination")?;
    fixture.request.fast_forward_only = true;
    let host = fixture.prepare()?;
    let rev = |fixture: &PrepareFixture, name: &str| {
        host.command(
            &fixture.writer.project,
            "git",
            &["rev-parse", name],
            WriterDeliveryStage::Verify,
        )
    };
    // A descendant of the target fast-forwards it.
    fixture.commit(&fixture.writer.worktree, "round one")?;
    let first = host.head(&fixture.writer)?;
    host.integrate(
        &fixture.writer,
        &fixture.request,
        first,
        Some(&crate::forge_for("origin", "main", &[])),
    )?;
    assert_eq!(rev(&fixture, "integration")?, first.to_string());
    // A head the target already contains integrates nothing and is not an error.
    host.integrate(
        &fixture.writer,
        &fixture.request,
        first,
        Some(&crate::forge_for("origin", "main", &[])),
    )?;
    // The target moved elsewhere: the round is refused instead of merged.
    fixture.commit(&fixture.writer.project, "someone else")?;
    fixture.git(&["branch", "-f", "integration", "HEAD"])?;
    let moved = rev(&fixture, "integration")?;
    fixture.commit(&fixture.writer.worktree, "round two")?;
    let second = host.head(&fixture.writer)?;
    let refused = host
        .integrate(
            &fixture.writer,
            &fixture.request,
            second,
            Some(&crate::forge_for("origin", "main", &[])),
        )
        .unwrap_err();
    assert!(matches!(refused, RunError::IntegrationHeadMoved));
    assert!(refused.to_string().contains("integration_head_moved"));
    assert!(refused.to_string().contains("rerun"));
    assert_eq!(rev(&fixture, "integration")?, moved);
    Ok(())
}

#[test]
fn local_integration_conflict_dirty_and_checked_out_refuse_without_partial_state()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    std::fs::write(fixture.writer.project.join("conflict"), "integration")?;
    fixture.git(&["add", "conflict"])?;
    fixture.commit(&fixture.writer.project, "integration conflict")?;
    fixture.git(&["branch", "integration"])?;
    fixture.request = HostDeliveryContext::destination(
        DeliveryMode::Integrate("integration".into()),
        "lane",
        &crate::forge_for("origin", "main", &[]),
    )?
    .ok_or("destination")?;
    let host = fixture.prepare()?;
    let original = host.command(
        &fixture.writer.project,
        "git",
        &["rev-parse", "integration"],
        WriterDeliveryStage::Verify,
    )?;
    std::fs::write(fixture.writer.worktree.join("conflict"), "lane")?;
    fixture.lane_git(&["add", "conflict"])?;
    fixture.commit(&fixture.writer.worktree, "lane conflict")?;
    let head = host.head(&fixture.writer)?;
    assert!(matches!(
        host.integrate(&fixture.writer, &fixture.request, head, Some(&crate::forge_for("origin", "main", &[]))),
        Err(RunError::IntegrationConflict { child_head, conflicting_files })
            if child_head == head && conflicting_files == ["conflict"]
    ));
    assert_eq!(
        host.command(
            &fixture.writer.project,
            "git",
            &["rev-parse", "integration"],
            WriterDeliveryStage::Verify
        )?,
        original
    );
    assert_eq!(host.head(&fixture.writer)?, head);
    assert!(!fixture.writer.project.join(".git/MERGE_HEAD").exists());
    std::fs::write(fixture.writer.worktree.join("dirty"), "keep")?;
    assert!(
        host.integrate(
            &fixture.writer,
            &fixture.request,
            head,
            Some(&crate::forge_for("origin", "main", &[]))
        )
        .is_err()
    );
    std::fs::remove_file(fixture.writer.worktree.join("dirty"))?;
    let primary = std::fs::read_to_string(fixture.writer.project.join(".git/HEAD"))?
        .trim()
        .trim_start_matches("ref: refs/heads/")
        .to_owned();
    let request = HostDeliveryContext::destination(
        DeliveryMode::Integrate(primary),
        "lane",
        &crate::forge_for("origin", "main", &[]),
    )?
    .ok_or("destination")?;
    assert!(matches!(
        HostDeliveryContext::new(fixture.environment.clone()).prepare(&fixture.writer, &request),
        Err(RunError::ProtectedDeliveryBranch)
    ));
    Ok(())
}

#[test]
fn concise_delivery_preserves_alias_contract_and_refuses_injection() -> Result<()> {
    assert!(
        HostDeliveryContext::destination(
            DeliveryMode::None,
            "",
            &crate::forge_for("origin", "main", &[])
        )?
        .is_none()
    );
    let push = HostDeliveryContext::destination(
        DeliveryMode::Push,
        "lane",
        &crate::forge_for("origin", "main", &[]),
    )?
    .ok_or(RunError::Validation("push"))?;
    assert_eq!(
        (&*push.remote, &*push.branch, push.open_pr),
        ("origin", "lane", false)
    );
    assert!(
        HostDeliveryContext::destination(
            DeliveryMode::Pr,
            "lane",
            &crate::forge_for("origin", "main", &[])
        )?
        .is_some_and(|request| request.open_pr)
    );
    let remote = HostDeliveryContext::destination(
        DeliveryMode::Integrate("origin/integration".into()),
        "lane",
        &crate::forge_for("origin", "main", &[]),
    )?
    .ok_or(RunError::Validation("integration"))?;
    assert_eq!(
        (&*remote.remote, &*remote.branch),
        ("origin", "integration")
    );
    for invalid in [
        "integrate:",
        "integrate:--all",
        "integrate:main..other",
        "integrate:refs/heads/../main",
        "pr:main",
    ] {
        assert!(invalid.parse::<DeliveryMode>().is_err(), "{invalid}");
    }
    Ok(())
}

#[test]
fn a_late_host_limit_fails_the_receipt_at_its_stage_and_keeps_the_reason()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    struct Free;
    impl provenant_confinement::worktrees::WorkerObserver for Free {
        fn observe_roots(
            &self,
            roots: &[std::path::PathBuf],
        ) -> std::collections::BTreeMap<
            std::path::PathBuf,
            provenant_confinement::worktrees::Occupancy,
        > {
            roots
                .iter()
                .cloned()
                .map(|root| (root, provenant_confinement::worktrees::Occupancy::Available))
                .collect()
        }
    }
    use provenant_core::{CancelToken, Clock, RootInputs};
    use provenant_os::clock::SystemClock;
    let mut fixture = PrepareFixture::new()?;
    let mut execution = tests::context(&fixture.root)?;
    let mut inputs = RootInputs::new(fixture.root.join("home"), fixture.root.join("checkout"));
    inputs.state_dir = Some(execution.state_root.as_path().into());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    std::fs::create_dir_all(roots.state.as_path())?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        roots.state.as_path(),
        std::fs::Permissions::from_mode(0o700),
    )?;
    let clock = SystemClock;
    let cancel = CancelToken::new();
    execution.project_id = provenant_confinement::worktrees::with_context(
        &roots,
        &fixture.root.join("home"),
        &fixture.writer.project,
        &clock,
        &cancel,
        |context| {
            let context = provenant_confinement::worktrees::WorktreeContext {
                workers: &Free,
                ..*context
            };
            let owner = provenant_confinement::worktrees::WorktreeOwner {
                attempt_id: None,
                operation_id: None,
                branch: "lane".into(),
                base: String::new(),
                host: provenant_core::HostId::from_bytes([3; 16]),
                created_at: clock
                    .wall_now()
                    .map_err(|_| provenant_confinement::worktrees::WorktreeError::InvalidOutput)?,
                origin: provenant_confinement::worktrees::WorktreeOrigin::Adopted,
                label: None,
                reference: None,
            };
            let head = provenant_confinement::run_utility(
                context.executor,
                provenant_confinement::GitRead::RevParseHead,
                context.repo,
                context.utility,
                context.utility_budget,
                &cancel,
            )?;
            let owner = provenant_confinement::worktrees::WorktreeOwner {
                base: String::from_utf8_lossy(&head.stdout.bytes).trim().into(),
                ..owner
            };
            provenant_confinement::worktrees::save_owner(
                &provenant_confinement::repo::verify_worktree(
                    context.repo,
                    &fixture.writer.worktree,
                )?,
                &owner,
            )?;
            Ok(context.binding.project_id())
        },
    )?;
    let call = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let store = RunStore::open(&execution, &call)?;
    let mut input = tests::input();
    input.request.task.writer = Some(fixture.writer.clone());
    input.request.task.deliver = Some(fixture.request.clone());
    store.create(&input, &call)?;
    let id = input.attempt_id;
    let host = fixture.prepare()?;
    let head = host.head(&fixture.writer)?;
    let attempt = store.attempt_dir(id, false)?;
    artifacts::seal(
        &attempt,
        "writer-terminal.json",
        &db::bytes(&writer::WriterTerminal {
            schema_version: 1,
            head,
            stop_work: None,
            warnings: Vec::new(),
        })?,
        RECORD_LIMIT,
    )?;
    let exit = tests::exit();
    tests::seal_observation(&store, &input, &exit)?;
    let lock = attempt.lock(
        std::ffi::OsStr::new(provenant_records::AttemptFile::CustodyLock.name()),
        provenant_os::fs::LockMode::Exclusive,
        call.lock_budget(),
        &cancel,
        &clock,
    )?;
    drop(lock);
    let mut publication = tests::publication();
    publication.fact.outcome = Outcome::ReviewRequired;
    publication.fact.reason = Some("protected changes require review".into());
    store.publish_terminal(id, 1, &publication, &exit, &call)?;
    // The first `log` marks the commit body as built; after it, `rev-parse
    // --verify HEAD` (the push stage's head check) overruns its bound.
    let git = fixture.root.join("late-limit-git");
    let flag = fixture.root.join("late-limit-enabled");
    let payload = fixture.root.join("late-limit-output");
    let limit = provenant_os::git::REPO_FACT_SCALAR_LIMIT;
    std::fs::write(&payload, vec![b'a'; limit + 1])?;
    let real_git = provenant_os::git::git_executable().ok_or("git executable")?;
    fixture.executable(
        &git,
        &format!(
            "#!/bin/sh\ncase \"$*\" in\n*' log '*) : > '{flag}';;\n*'rev-parse --verify HEAD'*) if [ -e '{flag}' ]; then cat '{payload}'; exit 0; fi;;\nesac\nexec '{real}' \"$@\"\n",
            flag = flag.display(),
            payload = payload.display(),
            real = real_git.display()
        ),
    )?;
    fixture
        .environment
        .push(("PROVENANT_GIT".into(), git.into_os_string()));
    let deliver = |accept_review| {
        store.deliver(
            id,
            None,
            HostDeliveryContext::new(fixture.environment.clone()),
            accept_review,
            &call,
        )
    };
    let receipt = deliver(true)?;
    assert!(flag.exists(), "the commit body was never built");
    let WriterDeliveryState::Failed { stage, reason, .. } = &receipt.state else {
        panic!("expected a failed delivery, got {:?}", receipt.state);
    };
    assert_eq!(*stage, WriterDeliveryStage::Head);
    let reason = reason.as_deref().ok_or("reason")?;
    let expected = provenant_confinement::utility::UtilityError::Truncated { stream: "stdout" };
    assert!(reason.contains(&expected.to_string()), "{reason}");
    Ok(())
}
