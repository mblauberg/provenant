use super::*;

#[test]
fn remove_reports_stale_registration_by_branch_and_directory() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let stale = create(
        &context,
        WorktreeCreate {
            branch: "scratch/stale".to_owned(),
            base: None,
        },
        &cancel(),
    )?;
    fs::remove_dir_all(&stale.path)?;
    for name in ["scratch/stale", "scratch-stale"] {
        let error = remove(
            &context,
            WorktreeRemove::Name {
                name: name.to_owned(),
                discard: false,
                integration: None,
                expected_head: None,
            },
            &cancel(),
        )
        .expect_err("stale registration must be reported");
        assert!(matches!(error, WorktreeError::StaleRegistration));
    }
    assert!(branch_exists(&fixture, "scratch/stale"));
    assert!(
        context
            .facts
            .read_inventory(&context, &cancel())?
            .iter()
            .any(|entry| entry.path == stale.path && entry.prunable)
    );
    Ok(())
}

#[test]
fn prune_treats_a_prunable_branch_as_not_checked_out() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let stale = create(
        &context,
        WorktreeCreate {
            branch: "scratch/stale".to_owned(),
            base: None,
        },
        &cancel(),
    )?;
    run_in_worktree(
        &fixture,
        &stale.path,
        &["commit", "--allow-empty", "-m", "stale work"],
    )?;
    fixture.run(&[
        "merge",
        "--no-ff",
        "scratch/stale",
        "-m",
        "merge stale work",
    ])?;
    fs::remove_dir_all(&stale.path)?;
    let entries = context.facts.read_inventory(&context, &cancel())?;
    let branch = GitRefName::branch("scratch/stale")?;
    assert!(matching_worktree(&entries, &branch, &fixture.root)?.is_none());
    assert!(!checked_out_elsewhere(&entries, &branch, None)?);
    assert!(!checked_out_elsewhere(
        &entries,
        &branch,
        Some(&fixture.root)
    )?);
    let integration = primary_integration(&context, &cancel())?.0;
    let result = remove(
        &context,
        WorktreeRemove::Merged {
            integration: integration.as_str().to_owned(),
            scope: PruneScope::Branches(vec!["scratch/stale".to_owned()]),
        },
        &cancel(),
    )?;
    assert_eq!(result.pruned_refs, vec!["scratch/stale"]);
    assert!(result.retained.is_empty());
    assert!(!branch_exists(&fixture, "scratch/stale"));
    Ok(())
}

#[test]
fn creation_refuses_a_shared_writable_worktree_parent() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fs::create_dir(fixture.root.join(".worktrees"))?;
    fs::set_permissions(
        fixture.root.join(".worktrees"),
        fs::Permissions::from_mode(0o777),
    )?;
    let test_context = TestContext::new(&fixture)?;
    assert!(matches!(
        create(
            &test_context.context(),
            WorktreeCreate {
                branch: "feature/refused".to_owned(),
                base: None
            },
            &cancel()
        ),
        Err(WorktreeError::UnknownWriter)
    ));
    assert!(
        !fixture
            .root
            .join(".worktrees")
            .join("feature-refused")
            .exists()
    );
    Ok(())
}

#[test]
fn worktree_operations_require_the_repo_project_binding() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let mut test_context = TestContext::new(&fixture)?;
    let other_fixture = Fixture::new()?;
    test_context.binding = TestContext::new(&other_fixture)?.binding;
    assert!(matches!(
        list(&test_context.context(), &cancel()),
        Err(WorktreeError::Repository(RepoError::InvalidRegistration))
    ));
    Ok(())
}

#[test]
fn clean_linked_worktree_reports_and_retains_in_progress_git_operations()
-> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let cancellation = cancel();
    let created = create(
        &context,
        WorktreeCreate {
            branch: "feature/sequencer".into(),
            base: None,
        },
        &cancellation,
    )?;
    let verified = verify_worktree(&fixture.repo, &created.path)?;
    check_writer_terminal(verified.repo())?;
    for (marker, directory) in [
        ("CHERRY_PICK_HEAD", false),
        ("REVERT_HEAD", false),
        ("REBASE_HEAD", false),
        ("rebase-merge", true),
        ("rebase-apply", true),
        ("MERGE_HEAD", false),
        ("sequencer", true),
    ] {
        let marker_path = verified.repo().private_git_dir().join(marker);
        if directory {
            fs::create_dir(&marker_path)?;
        } else {
            fs::write(&marker_path, format!("{}\n", created.head))?;
        }
        assert!(
            matches!(
                check_writer_terminal(verified.repo()),
                Err(WorktreeError::GitOperationInProgress)
            ),
            "{marker}"
        );
        if marker == "MERGE_HEAD" {
            let expected = fs::read(&marker_path)?;
            check_writer_terminal_with_merge(verified.repo(), &expected)?;
            assert!(matches!(
                check_writer_terminal_with_merge(verified.repo(), b"different"),
                Err(WorktreeError::GitOperationInProgress)
            ));
            let other = verified.repo().private_git_dir().join("CHERRY_PICK_HEAD");
            fs::write(&other, &expected)?;
            assert!(matches!(
                check_writer_terminal_with_merge(verified.repo(), &expected),
                Err(WorktreeError::GitOperationInProgress)
            ));
            fs::remove_file(other)?;
        }
        check_writer_terminal(&fixture.repo)?;
        let listed = list(&context, &cancellation)?;
        let entry = listed
            .iter()
            .find(|entry| entry.path == created.path)
            .ok_or("worktree missing")?;
        assert_eq!(
            entry.dirty,
            WorktreeCondition::GitOperationInProgress,
            "{marker}"
        );
        assert!(
            matches!(
                remove(
                    &context,
                    WorktreeRemove::Name {
                        name: "feature/sequencer".into(),
                        discard: false,
                        integration: None,
                        expected_head: None,
                    },
                    &cancellation,
                ),
                Err(WorktreeError::GitOperationInProgress)
            ),
            "{marker}"
        );
        assert!(created.path.is_dir());
        if directory {
            fs::remove_dir(&marker_path)?;
        } else {
            fs::remove_file(&marker_path)?;
        }
    }
    check_writer_terminal(verified.repo())?;
    let listed = list(&context, &cancellation)?;
    assert!(
        listed
            .iter()
            .any(|entry| entry.path == created.path && entry.dirty == WorktreeCondition::Clean)
    );
    remove(
        &context,
        WorktreeRemove::Name {
            name: "feature/sequencer".into(),
            discard: false,
            integration: None,
            expected_head: None,
        },
        &cancellation,
    )?;
    Ok(())
}

#[test]
fn create_refuses_aliases_of_local_and_remote_branches() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fixture.run(&["branch", "Feature/Topic"])?;
    fixture.run(&["update-ref", "refs/remotes/origin/Remote/Topic", "HEAD"])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    for (branch, existing) in [
        ("feature/topic", "refs/heads/Feature/Topic"),
        ("remote/topic", "refs/remotes/origin/Remote/Topic"),
    ] {
        let error = create(
            &context,
            WorktreeCreate {
                branch: branch.into(),
                base: None,
            },
            &cancel(),
        );
        assert!(
            matches!(error, Err(WorktreeError::BranchCollision { existing: ref found }) if found == existing),
            "unexpected result for {branch}: {error:?}"
        );
        assert!(
            !fixture
                .root
                .join(".worktrees")
                .join(branch.replace('/', "-"))
                .exists()
        );
    }
    Ok(())
}

#[test]
fn create_list_remove_keeps_branch_and_refuses_primary() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fixture.run(&["branch", "-M", "main"])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let cancellation = cancel();

    let created = create(
        &context,
        WorktreeCreate {
            branch: "feature/lifecycle".to_owned(),
            base: None,
        },
        &cancellation,
    )?;
    assert!(
        created
            .path
            .starts_with(canonicalize(&fixture.root)?.join(".worktrees"))
    );
    assert_eq!(created.head, fixture.head()?);
    assert_eq!(created.dirty, WorktreeCondition::Clean);
    assert!(created.occupancy.is_available());

    let listed = list(&context, &cancellation)?;
    assert!(listed.iter().any(|entry| {
        entry.path == created.path
            && entry.branch.as_deref() == Some("refs/heads/feature/lifecycle")
            && entry.dirty == WorktreeCondition::Clean
            && entry.occupancy.is_available()
    }));
    assert!(matches!(
        remove(
            &context,
            WorktreeRemove::Name {
                name: "main".to_owned(),
                discard: false,
                integration: None,
                expected_head: None,
            },
            &cancellation
        ),
        Err(WorktreeError::Primary)
    ));

    let unknown_context = WorktreeContext {
        workers: &UnknownWorkers,
        ..test_context.context()
    };
    // An unobservable process with a free lease and a clean tree is removed.
    remove(
        &unknown_context,
        WorktreeRemove::Name {
            name: "feature/lifecycle".to_owned(),
            discard: false,
            integration: None,
            expected_head: None,
        },
        &cancellation,
    )?;
    assert!(!created.path.exists());
    Ok(())
}

#[test]
fn ignored_files_prevent_worktree_removal() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fixture.run(&["branch", "-M", "main"])?;
    fs::write(fixture.root.join(".gitignore"), "ignored.txt\n")?;
    fixture.run(&["add", ".gitignore"])?;
    fixture.run(&["commit", "-m", "ignore fixture file"])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let cancellation = cancel();
    let created = create(
        &context,
        WorktreeCreate {
            branch: "feature/ignored".to_owned(),
            base: None,
        },
        &cancellation,
    )?;
    fs::write(created.path.join("ignored.txt"), "keep me\n")?;
    let condition = |cancellation: &CancelToken| -> Result<_, Box<dyn Error>> {
        Ok(list(&context, cancellation)?
            .into_iter()
            .find(|entry| entry.path == created.path)
            .map(|entry| entry.dirty))
    };
    assert_eq!(condition(&cancellation)?, Some(WorktreeCondition::Ignored));

    assert!(matches!(
        remove(
            &context,
            WorktreeRemove::Name {
                name: "feature/ignored".to_owned(),
                discard: false,
                integration: None,
                expected_head: None,
            },
            &cancellation,
        ),
        Err(WorktreeError::IgnoredPaths(ref paths)) if paths.contains("ignored.txt")
    ));
    assert_eq!(
        fs::read_to_string(created.path.join("ignored.txt"))?,
        "keep me\n"
    );
    fs::write(created.path.join("notes.txt"), "draft\n")?;
    assert_eq!(condition(&cancellation)?, Some(WorktreeCondition::Dirty));
    Ok(())
}

#[test]
fn named_removal_refuses_a_head_that_moved_after_the_callers_check() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fixture.run(&["branch", "-M", "main"])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let cancellation = cancel();
    let created = create(
        &context,
        WorktreeCreate {
            branch: "feature/raced".to_owned(),
            base: None,
        },
        &cancellation,
    )?;
    // The caller verified the worktree at its creation head, then a clean
    // commit landed before removal started.
    let verified = created.head;
    run_in_worktree(
        &fixture,
        &created.path,
        &["commit", "--allow-empty", "-m", "after the check"],
    )?;
    let branch = GitRefName::branch("feature/raced")?;
    let moved = read_branch_tips(&context, &cancellation)?
        .get(&branch)
        .copied()
        .ok_or("branch tip")?;
    assert_ne!(moved, verified);
    let request = |expected_head| WorktreeRemove::Name {
        name: "feature/raced".to_owned(),
        discard: false,
        integration: None,
        expected_head,
    };
    assert!(matches!(
        remove(&context, request(Some(verified)), &cancellation),
        Err(WorktreeError::IdentityChanged)
    ));
    assert!(created.path.exists(), "the checkout must survive");
    assert_eq!(
        read_branch_tips(&context, &cancellation)?.get(&branch),
        Some(&moved)
    );
    // Verifying the head it is really at lets the removal proceed.
    let outcome = remove(&context, request(Some(moved)), &cancellation)?;
    assert_eq!(outcome.removed.len(), 1);
    assert!(!created.path.exists());
    Ok(())
}

#[test]
fn merged_pruning_is_branch_scoped_and_keeps_unselected_refs() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fixture.run(&["branch", "-M", "main"])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let cancellation = cancel();
    let merged = create(
        &context,
        WorktreeCreate {
            branch: "feature/merged".to_owned(),
            base: None,
        },
        &cancellation,
    )?;
    fs::write(merged.path.join("proof.txt"), "merged fixture\n")?;
    run_in_worktree(&fixture, &merged.path, &["add", "proof.txt"])?;
    run_in_worktree(&fixture, &merged.path, &["commit", "-m", "feature commit"])?;
    fixture.run(&["merge", "--no-ff", "feature/merged", "-m", "merge feature"])?;

    let unselected = create(
        &context,
        WorktreeCreate {
            branch: "feature/unselected".to_owned(),
            base: None,
        },
        &cancellation,
    )?;
    let removed = remove(
        &context,
        WorktreeRemove::Merged {
            integration: "main".to_owned(),
            scope: PruneScope::Branches(vec!["feature/merged".to_owned()]),
        },
        &cancellation,
    )?;
    assert_eq!(removed.pruned_refs, vec!["feature/merged"]);
    assert_eq!(removed.removed.len(), 1);
    assert!(!merged.path.exists());
    assert!(unselected.path.exists());
    let tips = read_branch_tips(&context, &cancellation)?;
    assert!(!tips.contains_key(&GitRefName::branch("feature/merged")?));
    assert!(tips.contains_key(&GitRefName::branch("feature/unselected")?));
    Ok(())
}

#[test]
fn merged_pruning_keeps_old_ancestor_and_unmerged_ignored_files() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fixture.run(&["branch", "-M", "main"])?;
    fs::write(fixture.root.join(".gitignore"), "ignored.txt\n")?;
    fixture.run(&["add", ".gitignore"])?;
    fixture.run(&["commit", "-m", "ignore fixture file"])?;
    let old_base = fixture.head()?;
    fixture.run(&[
        "commit",
        "--allow-empty",
        "-m",
        "advance before branch creation",
    ])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let cancellation = cancel();
    let old = create(
        &context,
        WorktreeCreate {
            branch: "feature/old-ancestor".to_owned(),
            base: Some(old_base.to_string()),
        },
        &cancellation,
    )?;
    let unmerged = create(
        &context,
        WorktreeCreate {
            branch: "feature/unmerged".to_owned(),
            base: None,
        },
        &cancellation,
    )?;
    fs::write(unmerged.path.join("unique.txt"), "branch-only commit\n")?;
    run_in_worktree(&fixture, &unmerged.path, &["add", "unique.txt"])?;
    run_in_worktree(
        &fixture,
        &unmerged.path,
        &["commit", "-m", "unique branch commit"],
    )?;
    fs::write(old.path.join("ignored.txt"), "old branch data\n")?;
    fs::write(unmerged.path.join("ignored.txt"), "unmerged branch data\n")?;
    fixture.run(&["commit", "--allow-empty", "-m", "advance integration"])?;

    let outcome = remove(
        &context,
        WorktreeRemove::Merged {
            integration: "main".to_owned(),
            scope: PruneScope::Branches(vec![
                "feature/old-ancestor".to_owned(),
                "feature/unmerged".to_owned(),
            ]),
        },
        &cancellation,
    )?;
    assert!(outcome.removed.is_empty());
    assert!(outcome.pruned_refs.is_empty());
    assert_eq!(outcome.retained.len(), 2);
    assert!(outcome.retained.iter().any(|retained| {
        retained.branch == "feature/old-ancestor" && retained.reason == RetainReason::NoUniqueCommit
    }));
    assert!(old.path.exists());
    assert!(unmerged.path.exists());
    assert_eq!(
        fs::read_to_string(old.path.join("ignored.txt"))?,
        "old branch data\n"
    );
    assert_eq!(
        fs::read_to_string(unmerged.path.join("ignored.txt"))?,
        "unmerged branch data\n"
    );
    Ok(())
}

#[test]
fn create_accepts_uninitialised_gitlinks_and_still_rejects_initialised_ones()
-> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fixture.run(&["branch", "-M", "main"])?;
    let head = fixture.head()?;
    fixture.run(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("160000,{head},sub"),
    ])?;
    fixture.run(&["commit", "-m", "tracked but uninitialised gitlink"])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let created = create(
        &context,
        WorktreeCreate {
            branch: "feature/uninitialised-gitlink".to_owned(),
            base: None,
        },
        &cancel(),
    )?;
    assert_eq!(created.dirty, WorktreeCondition::Clean);
    assert!(list(&context, &cancel())?.iter().any(|entry| {
        entry.branch.as_deref() == Some("refs/heads/feature/uninitialised-gitlink")
            && entry.dirty == WorktreeCondition::Clean
    }));

    let marker = fixture._temp.path().join("initialised-filter-ran");
    add_poisoned_gitlink(&fixture, &created.path, "sub", &marker)?;
    assert!(list(&context, &cancel())?.iter().any(|entry| {
        entry.branch.as_deref() == Some("refs/heads/feature/uninitialised-gitlink")
            && entry.dirty == WorktreeCondition::Unknown
    }));
    Ok(())
}

#[test]
fn gitlinks_make_remove_and_prune_require_review() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fixture.run(&["branch", "-M", "main"])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let cancellation = cancel();
    let direct = create(
        &context,
        WorktreeCreate {
            branch: "feature/direct-gitlink".to_owned(),
            base: None,
        },
        &cancellation,
    )?;
    let direct_marker = fixture._temp.path().join("direct-filter-ran");
    let direct_poison = add_poisoned_gitlink(&fixture, &direct.path, "sub", &direct_marker)?;
    assert_raw_git_runs_poison_filter(&fixture, &direct_poison)?;

    let merged = create(
        &context,
        WorktreeCreate {
            branch: "feature/merged-gitlink".to_owned(),
            base: None,
        },
        &cancellation,
    )?;
    fs::write(merged.path.join("merged.txt"), "merged feature\n")?;
    run_in_worktree(&fixture, &merged.path, &["add", "merged.txt"])?;
    run_in_worktree(&fixture, &merged.path, &["commit", "-m", "feature commit"])?;
    fixture.run(&[
        "merge",
        "--no-ff",
        "feature/merged-gitlink",
        "-m",
        "merge feature",
    ])?;
    let merged_marker = fixture._temp.path().join("merged-filter-ran");
    let merged_poison = add_poisoned_gitlink(&fixture, &merged.path, "sub", &merged_marker)?;
    assert_raw_git_runs_poison_filter(&fixture, &merged_poison)?;

    direct_poison.arm()?;
    merged_poison.arm()?;
    let listed = list(&context, &cancellation)?;
    assert!(
        !direct_marker.exists(),
        "worktree listing ran a nested clean filter"
    );
    assert!(
        !merged_marker.exists(),
        "worktree listing ran a nested clean filter"
    );
    for branch in ["feature/direct-gitlink", "feature/merged-gitlink"] {
        assert!(listed.iter().any(|entry| {
            entry.branch.as_deref() == Some(format!("refs/heads/{branch}").as_str())
                && entry.dirty == WorktreeCondition::Unknown
        }));
    }
    direct_poison.arm()?;
    merged_poison.arm()?;
    assert!(matches!(
        remove(
            &context,
            WorktreeRemove::Name {
                name: "feature/direct-gitlink".to_owned(),
                discard: false,
                integration: None,
                expected_head: None,
            },
            &cancellation,
        ),
        Err(WorktreeError::UnknownWriter)
    ));
    assert!(
        !direct_marker.exists(),
        "worktree removal ran a nested clean filter"
    );
    direct_poison.arm()?;
    merged_poison.arm()?;
    let pruned = remove(
        &context,
        WorktreeRemove::Merged {
            integration: "main".to_owned(),
            scope: PruneScope::Branches(vec!["feature/merged-gitlink".to_owned()]),
        },
        &cancellation,
    )?;
    assert!(pruned.removed.is_empty());
    assert!(pruned.pruned_refs.is_empty());
    assert_eq!(pruned.retained.len(), 1);
    assert_eq!(pruned.retained[0].reason, RetainReason::UnknownWriter);
    assert!(
        !merged_marker.exists(),
        "worktree pruning ran a nested clean filter"
    );
    assert!(direct.path.join("sub/.git").exists());
    assert!(merged.path.join("sub/.git").exists());
    assert!(!direct_marker.exists());
    assert!(!merged_marker.exists());
    Ok(())
}

#[test]
fn delete_ref_compare_and_swap_preserves_a_moved_ref() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fixture.run(&["branch", "-M", "main"])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let cancellation = cancel();
    let stale_oid = fixture.head()?;
    fixture.run(&["branch", "feature/race"])?;
    fixture.run(&["commit", "--allow-empty", "-m", "advance integration"])?;
    let moved_oid = fixture.head()?;
    fixture.run(&["branch", "-f", "feature/race", &moved_oid.to_string()])?;

    let outcome = execute_git_mutation(
        &fixture.executor,
        &fixture.repo,
        &fixture.runtime,
        GitMutation::DeleteRef {
            name: GitRefName::branch("feature/race")?,
            expected_old_oid: stale_oid,
        },
        fixture.budget()?,
        &cancellation,
    )?;
    assert!(matches!(outcome.termination, Termination::Exit(code) if code != 0));
    let tips = read_branch_tips(&context, &cancellation)?;
    assert_eq!(
        tips.get(&GitRefName::branch("feature/race")?),
        Some(&moved_oid)
    );
    Ok(())
}

#[test]
fn detached_worktree_names_the_head_and_both_fixes() -> Result<(), Box<dyn std::error::Error>> {
    let head = GitOid::parse("0123456789abcdef0123456789abcdef01234567")?;
    let message = WorktreeError::DetachedWorktree { head }.to_string();
    for part in [
        "0123456789abcdef0123456789abcdef01234567",
        "git switch -c",
        "--mode read-only",
    ] {
        assert!(message.contains(part), "{message}");
    }
    Ok(())
}

#[test]
fn writer_busy_names_the_holder_only_when_known() {
    let holder = AttemptId::from_bytes([7; 16]);
    let known = WorktreeError::WriterBusy {
        holder: Some(holder),
    }
    .to_string();
    assert!(
        known.starts_with("worktree writer is busy, held by attempt "),
        "{known}"
    );
    assert_eq!(
        WorktreeError::WriterBusy { holder: None }.to_string(),
        "worktree writer is busy"
    );
}

#[test]
fn explicit_discard_publishes_recoverable_data_before_removing_work() -> Result<(), Box<dyn Error>>
{
    let fixture = Fixture::new()?;
    let owner = TestContext::new(&fixture)?;
    let context = owner.context();
    let created = create(
        &context,
        WorktreeCreate {
            branch: "scratch/archive".into(),
            base: None,
        },
        &cancel(),
    )?;
    run_in_worktree(
        &fixture,
        &created.path,
        &["commit", "--allow-empty", "-m", "unique work"],
    )?;
    fs::write(created.path.join("private-note"), b"untracked evidence")?;
    let result = remove(
        &context,
        WorktreeRemove::Name {
            name: "scratch/archive".into(),
            discard: true,
            integration: None,
            expected_head: None,
        },
        &cancel(),
    )?;
    let archive = result.archives.first().ok_or("discard had no archive")?;
    assert!(archive.path.join("tip.bundle").is_file());
    assert!(archive.path.join("manifest.json").is_file());
    let tar = fs::read(archive.path.join("untracked.tar"))?;
    assert!(
        tar.windows(b"untracked evidence".len())
            .any(|bytes| bytes == b"untracked evidence")
    );
    assert!(!created.path.exists());
    assert!(!branch_exists(&fixture, "scratch/archive"));
    Ok(())
}
