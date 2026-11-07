use super::*;

#[test]
fn integration_refuses_default_fallbacks_remote_head_and_primary_branch()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture.git(&["branch", "-m", "primary-live"])?;
    for target in [
        "main",
        "master",
        "primary-live",
        "origin/main",
        "origin/primary-live",
    ] {
        fixture.request =
            HostDeliveryContext::destination(DeliveryMode::Integrate(target.into()), "lane")?
                .ok_or("destination")?;
        assert!(
            matches!(fixture.prepare(), Err(RunError::ProtectedDeliveryBranch)),
            "{target}"
        );
    }
    fixture.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/trunk",
    ])?;
    for target in ["trunk", "origin/trunk"] {
        fixture.request =
            HostDeliveryContext::destination(DeliveryMode::Integrate(target.into()), "lane")?
                .ok_or("destination")?;
        assert!(
            matches!(fixture.prepare(), Err(RunError::ProtectedDeliveryBranch)),
            "{target}"
        );
    }
    fixture.git(&["branch", "integration"])?;
    fixture.request =
        HostDeliveryContext::destination(DeliveryMode::Integrate("integration".into()), "lane")?
            .ok_or("destination")?;
    let host = fixture.prepare()?;
    fixture.commit(&fixture.writer.worktree, "lane change")?;
    fixture.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/integration",
    ])?;
    let head = host.head(&fixture.writer)?;
    assert!(matches!(
        host.integrate(&fixture.writer, &fixture.request, head),
        Err(RunError::Delivery(WriterDeliveryStage::Integrate))
    ));
    fixture.git(&[
        "symbolic-ref",
        "refs/heads/alias",
        "refs/heads/primary-live",
    ])?;
    fixture.request =
        HostDeliveryContext::destination(DeliveryMode::Integrate("alias".into()), "lane")?
            .ok_or("destination")?;
    assert!(matches!(
        fixture.prepare(),
        Err(RunError::DeliveryPrepare {
            step: "integration target",
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
    fixture.request =
        HostDeliveryContext::destination(DeliveryMode::Integrate("integration".into()), "lane")?
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
        host.integrate(&fixture.writer, &fixture.request, head),
        Err(RunError::Delivery(WriterDeliveryStage::Integrate))
    ));
    Ok(())
}

#[test]
fn integration_refuses_the_default_branch_of_a_non_origin_remote()
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
    fixture.request =
        HostDeliveryContext::destination(DeliveryMode::Integrate("release".into()), "lane")?
            .ok_or("destination")?;
    assert!(matches!(
        fixture.prepare(),
        Err(RunError::ProtectedDeliveryBranch)
    ));
    assert!(matches!(
        HostDeliveryContext::validate_integration_target(&fixture.writer.project, "release"),
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
        Err(RunError::Delivery(WriterDeliveryStage::Push))
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
        Err(RunError::Delivery(WriterDeliveryStage::Push))
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
        let path = fixture.root.join("attempt");
        std::fs::create_dir(&path)?;
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
                stage: WriterDeliveryStage::Integrate, ref extra
            } if extra.get("reason").and_then(serde_json::Value::as_str).is_some_and(|reason| reason.contains("parent_busy"))));
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
    fixture.request =
        HostDeliveryContext::destination(DeliveryMode::Integrate("integration".into()), "lane")?
            .ok_or("destination")?;
    let first = fixture.prepare()?;
    let second_writer = WriterRequest {
        extra: Default::default(),
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
            first.integrate(&fixture.writer, &fixture.request, first_head)
        });
        let two = scope.spawn(|| {
            barrier.wait();
            second.integrate(&second_writer, &fixture.request, second_head)
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
            matches!(error, RunError::Delivery(WriterDeliveryStage::Integrate)),
            "{error}"
        );
        first.integrate(&fixture.writer, &fixture.request, first_head)?;
    }
    if let Err(error) = two {
        assert!(
            matches!(error, RunError::Delivery(WriterDeliveryStage::Integrate)),
            "{error}"
        );
        second.integrate(&second_writer, &fixture.request, second_head)?;
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
    assert!(capped.len() <= 60_000 && capped.ends_with("[Commit summary truncated.]\n"));
    fixture.commit(
        &fixture.writer.worktree,
        &format!("Large commit\n\n{}", "\u{1f600}".repeat(20_000)),
    )?;
    let body = host.commit_body(
        &fixture.writer,
        host.head(&fixture.writer)?,
        "Run fixture\nAttempt fixture",
    )?;
    assert!(body.len() <= 60_000 && body.contains("[Commit summary truncated.]"));
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
    )?
    .ok_or("destination")?;
    let host = fixture.prepare()?;
    fixture.commit(&fixture.writer.worktree, "remote lane")?;
    let head = host.head(&fixture.writer)?;
    host.integrate(&fixture.writer, &fixture.request, head)?;
    assert_eq!(
        host.command(
            &fixture.writer.worktree,
            "git",
            &["rev-parse", "refs/remotes/origin/integration"],
            WriterDeliveryStage::Verify
        )?,
        head.to_string()
    );
    host.integrate(&fixture.writer, &fixture.request, head)?;
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
    )?
    .ok_or("destination")?;
    let host = fixture.prepare()?;
    let original = host.head(&fixture.writer)?;
    fixture.commit(&fixture.writer.worktree, "lane change")?;
    let head = host.head(&fixture.writer)?;
    assert!(matches!(
        host.integrate(&fixture.writer, &fixture.request, head),
        Err(RunError::Delivery(WriterDeliveryStage::Integrate))
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
    fixture.request =
        HostDeliveryContext::destination(DeliveryMode::Integrate("integration".into()), "lane")?
            .ok_or("destination")?;
    let host = fixture.prepare()?;
    fixture.commit(&fixture.writer.worktree, "lane one")?;
    let head = host.head(&fixture.writer)?;
    host.integrate(&fixture.writer, &fixture.request, head)?;
    assert_eq!(
        host.command(
            &fixture.writer.project,
            "git",
            &["rev-parse", "integration"],
            WriterDeliveryStage::Verify
        )?,
        head.to_string()
    );
    host.integrate(&fixture.writer, &fixture.request, head)?;
    // Divergent commits with a common tree still require a two-parent merge.
    fixture.commit(&fixture.writer.project, "integration side")?;
    fixture.git(&["branch", "-f", "integration", "HEAD"])?;
    fixture.commit(&fixture.writer.worktree, "lane two")?;
    let head = host.head(&fixture.writer)?;
    host.integrate(&fixture.writer, &fixture.request, head)?;
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
    fixture.request =
        HostDeliveryContext::destination(DeliveryMode::Integrate("integration".into()), "lane")?
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
    host.integrate(&fixture.writer, &fixture.request, first)?;
    assert_eq!(rev(&fixture, "integration")?, first.to_string());
    // A head the target already contains integrates nothing and is not an error.
    host.integrate(&fixture.writer, &fixture.request, first)?;
    // The target moved elsewhere: the round is refused instead of merged.
    fixture.commit(&fixture.writer.project, "someone else")?;
    fixture.git(&["branch", "-f", "integration", "HEAD"])?;
    let moved = rev(&fixture, "integration")?;
    fixture.commit(&fixture.writer.worktree, "round two")?;
    let second = host.head(&fixture.writer)?;
    let refused = host
        .integrate(&fixture.writer, &fixture.request, second)
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
    fixture.request =
        HostDeliveryContext::destination(DeliveryMode::Integrate("integration".into()), "lane")?
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
        host.integrate(&fixture.writer, &fixture.request, head),
        Err(RunError::IntegrationConflict { child_head }) if child_head == head
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
        host.integrate(&fixture.writer, &fixture.request, head)
            .is_err()
    );
    std::fs::remove_file(fixture.writer.worktree.join("dirty"))?;
    let primary = std::fs::read_to_string(fixture.writer.project.join(".git/HEAD"))?
        .trim()
        .trim_start_matches("ref: refs/heads/")
        .to_owned();
    let request = HostDeliveryContext::destination(DeliveryMode::Integrate(primary), "lane")?
        .ok_or("destination")?;
    assert!(matches!(
        HostDeliveryContext::new(fixture.environment.clone()).prepare(&fixture.writer, &request),
        Err(RunError::ProtectedDeliveryBranch)
    ));
    Ok(())
}

#[test]
fn concise_delivery_preserves_alias_contract_and_refuses_injection() -> Result<()> {
    assert!(HostDeliveryContext::destination(DeliveryMode::None, "")?.is_none());
    let push = HostDeliveryContext::destination(DeliveryMode::Push, "lane")?
        .ok_or(RunError::Validation("push"))?;
    assert_eq!(
        (&*push.remote, &*push.branch, push.open_pr),
        ("origin", "lane", false)
    );
    assert!(
        HostDeliveryContext::destination(DeliveryMode::Pr, "lane")?
            .is_some_and(|request| request.open_pr)
    );
    let remote = HostDeliveryContext::destination(
        DeliveryMode::Integrate("origin/integration".into()),
        "lane",
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
    input.request.tasks[0].writer = Some(fixture.writer.clone());
    input.request.tasks[0].deliver = Some(fixture.request.clone());
    store.create(&input, &call)?;
    let id = input.attempts[0].attempt_id;
    let host = fixture.prepare()?;
    let head = host.head(&fixture.writer)?;
    let attempt = store.attempt_dir(input.run_id, id, false)?;
    artifacts::seal(
        &attempt,
        "writer-terminal.json",
        &db::bytes(&writer::WriterTerminal {
            schema_version: 1,
            head,
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
    publication.outcome = Outcome::ReviewRequired;
    publication.reason = Some("protected changes require review".into());
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
    let WriterDeliveryState::Failed { stage, extra } = &receipt.state else {
        panic!("expected a failed delivery, got {:?}", receipt.state);
    };
    assert_eq!(*stage, WriterDeliveryStage::Head);
    let reason = extra
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .ok_or("reason")?;
    let expected = provenant_confinement::utility::UtilityError::Truncated { stream: "stdout" };
    assert!(reason.contains(&expected.to_string()), "{reason}");
    Ok(())
}
