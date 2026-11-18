use super::*;

// Spec: docs/specs/supervisor.md#shared-occupancy-removal-never-forces
#[test]
fn gc_keeps_worktree_until_every_shared_holder_leaves() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let lane = gc_merged_worktree(&test, &context, "shared-gc")?;
    let retirement = HostRetirementFixture::new(&test)?;
    retirement.seed_lane(&context, &lane)?;
    let upstream = fixture.head()?;
    let evidence = gc_landing_evidence(&fixture, upstream)?;
    let slot = context
        .project_state
        .open_verified_directory(&state_dir_name(&lane.path))?;
    let mut holders = vec![
        slot.try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Shared)?,
        slot.try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Shared)?,
    ];
    let policy = WorktreeGcPolicy {
        upstream: Some(upstream),
        terminal: &|_| true,
        no_live_lanes: &|_| true,
        regenerable: &[],
        keep: &[],
        written_before_ns: i128::MAX,
        dry_run: false,
        include_foreign: true,
        retirement_port: Some(&retirement),
        retirement_feeds: Some(&retirement),
        mode: CollectionMode::Worktrees,
        proof: Some(&evidence),
        upstream_reference: None,
        integration_patterns: &[],
        excluded_paths: &[],
    };
    while !holders.is_empty() {
        let report = collect(&context, std::slice::from_ref(&lane), &policy, &cancel())?;
        assert!(
            lane.path.is_dir(),
            "GC removed a worktree with {} shared holders",
            holders.len()
        );
        assert!(report.worktrees.removed.is_empty());
        drop(holders.pop());
    }
    let report = collect(&context, std::slice::from_ref(&lane), &policy, &cancel())?;
    assert!(
        !lane.path.exists(),
        "zero-holder worktree should now be removable"
    );
    assert!(
        report
            .worktrees
            .removed
            .iter()
            .any(|removed| removed.path == lane.path)
    );
    Ok(())
}

#[test]
fn remove_registered_detached_worktree() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let info = create(
        &context,
        WorktreeCreate {
            branch: "detached".into(),
            base: None,
        },
        &cancel(),
    )?;
    run_in_worktree(&fixture, &info.path, &["checkout", "--detach"])?;
    let removed = remove_with_port(
        &test,
        &context,
        WorktreeRemove::Name {
            name: "detached".into(),
            discard: false,
            integration: None,
            expected_head: Some(info.head),
        },
        &cancel(),
    )?;
    assert_eq!(removed.removed.len(), 1);
    assert_eq!(removed.removed[0].branch, None);
    assert_eq!(removed.removed[0].head, info.head);
    assert!(!info.path.exists());
    assert!(removed.pruned_refs.is_empty());
    assert!(read_branch_tips(&context, &cancel())?.contains_key(&GitRefName::branch("detached")?));
    Ok(())
}

#[test]
fn retirement_preserves_unique_tips_before_removing_the_worktree() -> Result<(), Box<dyn Error>> {
    for detached in [false, true] {
        for unique in [false, true] {
            for discard in [false, true] {
                let fixture = Fixture::new()?;
                let test = TestContext::new(&fixture)?;
                let context = test.context();
                let branch = format!(
                    "custody-{}-{unique}",
                    if detached { "detached" } else { "branch" }
                );
                let created = create(
                    &context,
                    WorktreeCreate {
                        branch: branch.clone(),
                        base: None,
                    },
                    &cancel(),
                )?;
                if unique {
                    run_in_worktree(
                        &fixture,
                        &created.path,
                        &["commit", "--allow-empty", "-m", "unique worktree tip"],
                    )?;
                }
                let tip_output = fixture.run(&[
                    "-C",
                    created.path.to_str().ok_or("worktree path")?,
                    "rev-parse",
                    "HEAD",
                ])?;
                let tip = GitOid::parse(std::str::from_utf8(&tip_output.stdout.bytes)?.trim())?;
                if detached {
                    run_in_worktree(&fixture, &created.path, &["checkout", "--detach"])?;
                    fixture.run(&["branch", "-D", &branch])?;
                }
                let entries = context.facts.fresh_inventory(&context, &cancel())?;
                let entry = entries
                    .iter()
                    .find(|entry| canonicalize(&entry.path).ok().as_ref() == Some(&created.path))
                    .ok_or("worktree vanished before retirement")?;
                let mut archive_bundle = None;
                let removed = super::super::lifecycle::remove_tree_when_before_delete(
                    &context,
                    entry,
                    tip,
                    &cancel(),
                    &|_| Ok(()),
                    Some(&Default::default()),
                    discard,
                    |_, _, archive| {
                        if discard {
                            let archive = archive.ok_or(WorktreeError::IdentityChanged)?;
                            assert!(archive.path.join("tip.bundle").is_file());
                            archive_bundle = Some(archive.path.join("tip.bundle"));
                        } else if unique {
                            let reference = GitRefName::preserved_tip(tip.to_string())
                                .map_err(|_| WorktreeError::InvalidRequest)?;
                            assert_eq!(ref_tip(&context, &reference, &cancel())?, Some(tip));
                            let record = RefLedger::for_context(&context)?
                                .load(&RefEndpoint::Local, &reference)?
                                .ok_or(WorktreeError::RegistrationUnconfirmed)?;
                            assert_eq!(record.tip, tip);
                        }
                        Ok(())
                    },
                )?;
                assert_eq!(removed.info.head, tip);
                drop(removed);
                if !detached {
                    fixture.run(&["branch", "-D", &branch])?;
                }
                if discard {
                    let bundle = archive_bundle.ok_or("discard did not publish its bundle")?;
                    let heads = fixture.run(&[
                        "bundle",
                        "list-heads",
                        bundle.to_str().ok_or("bundle path")?,
                    ])?;
                    assert!(
                        String::from_utf8_lossy(&heads.stdout.bytes).contains(&tip.to_string())
                    );
                } else if unique {
                    let reference = GitRefName::preserved_tip(tip.to_string())?;
                    assert_eq!(ref_tip(&context, &reference, &cancel())?, Some(tip));
                } else if !unique {
                    assert!(is_ancestor(&context, tip, fixture.head()?, &cancel())?);
                }
            }
        }
    }
    Ok(())
}

#[test]
fn detached_removal_keeps_dirty_unmerged_and_live_work() -> Result<(), Box<dyn Error>> {
    for scenario in ["dirty", "unmerged", "live", "changed-head"] {
        let fixture = Fixture::new()?;
        let test = TestContext::new(&fixture)?;
        let mut context = test.context();
        let info = create(
            &context,
            WorktreeCreate {
                branch: "detached".into(),
                base: None,
            },
            &cancel(),
        )?;
        run_in_worktree(&fixture, &info.path, &["checkout", "--detach"])?;
        match scenario {
            "dirty" => fs::write(info.path.join("notes"), "preserve")?,
            "unmerged" | "changed-head" => run_in_worktree(
                &fixture,
                &info.path,
                &["commit", "--allow-empty", "-m", "detached work"],
            )?,
            "live" => context.workers = &NamedLive,
            _ => {}
        }
        let request = |discard| WorktreeRemove::Name {
            name: "detached".into(),
            discard,
            integration: None,
            expected_head: (scenario == "changed-head").then_some(info.head),
        };
        let error = remove_with_port(&test, &context, request(false), &cancel())
            .err()
            .ok_or("removed protected tree")?;
        assert!(
            match scenario {
                "dirty" =>
                    matches!(error, WorktreeError::Dirty(ref paths) if paths.contains("notes")),
                "unmerged" => matches!(error, WorktreeError::DetachedUnmerged { .. }),
                "live" => matches!(error, WorktreeError::LiveProcess(_)),
                "changed-head" => matches!(error, WorktreeError::IdentityChanged),
                _ => false,
            },
            "{scenario}: {error}"
        );
        assert!(error.repair().is_some(), "{scenario}: {error}");
        assert!(info.path.exists());
        if scenario == "unmerged" {
            let removed = remove_with_port(&test, &context, request(true), &cancel())?;
            assert_eq!(removed.removed.len(), 1);
            assert_ne!(removed.removed[0].head, info.head);
            assert!(removed.pruned_refs.is_empty());
            assert!(!info.path.exists());
        }
    }
    Ok(())
}

#[test]
fn gc_idle_output_requires_ignored_idle_and_free_writer() -> Result<(), Box<dyn Error>> {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Case {
        Idle,
        Recent,
        Lease,
        Tracked,
        Untracked,
        Lane,
    }
    let fixture = Fixture::new()?;
    fs::write(fixture.root.join(".gitignore"), "target/\n.env\n")?;
    fixture.run(&["add", ".gitignore"])?;
    fixture.run(&["commit", "-m", "output rules"])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let created = gc_owned_lane(&test_context, &context, "idle-output")?;
    for case in [
        Case::Idle,
        Case::Recent,
        Case::Lease,
        Case::Tracked,
        Case::Untracked,
        Case::Lane,
    ] {
        // The lane is retained in every case. Reset only this fixture's
        // tracked-rule variants before exercising the next output policy.
        if case == Case::Untracked {
            fs::write(created.path.join(".gitignore"), ".env\n")?;
            run_in_worktree(&fixture, &created.path, &["add", ".gitignore"])?;
            run_in_worktree(
                &fixture,
                &created.path,
                &["commit", "-m", "untracked output"],
            )?;
        }
        if case == Case::Tracked {
            fs::create_dir(created.path.join("target"))?;
            fs::write(created.path.join("target/source"), "tracked")?;
            run_in_worktree(&fixture, &created.path, &["add", "-f", "target/source"])?;
            run_in_worktree(&fixture, &created.path, &["commit", "-m", "tracked output"])?;
        }
        fs::create_dir_all(created.path.join("target"))?;
        fs::write(created.path.join("target/cache"), "output")?;
        let lanes = list_for_gc_matching(&context, &cancel(), |path, _| path == created.path)?;
        let state = context
            .project_state
            .open_verified_directory(&state_dir_name(&created.path))?;
        let lease = if case == Case::Lease {
            Some(state.try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive)?)
        } else {
            None
        };
        let report = collect(
            &context,
            &lanes,
            &WorktreeGcPolicy {
                include_foreign: false,
                retirement_port: None,
                retirement_feeds: None,
                // Output retention alone: the lane itself is never retired.
                mode: CollectionMode::Outputs,
                proof: None,
                upstream_reference: None,
                integration_patterns: &[],
                excluded_paths: &[],
                upstream: None,
                terminal: &|_| false,
                no_live_lanes: &|_| case != Case::Lane,
                regenerable: &["target".into()],
                keep: &[],
                written_before_ns: if case == Case::Recent { 0 } else { i128::MAX },
                dry_run: false,
            },
            &cancel(),
        )?;
        assert_eq!(
            created.path.join("target/cache").exists(),
            case != Case::Idle,
            "{case:?}"
        );
        let target_removed = report
            .idle_output
            .iter()
            .any(|row| row.path == created.path.join("target") && row.removed);
        assert_eq!(target_removed, case == Case::Idle, "{case:?}");
        assert!(created.path.is_dir());
        drop(lease);
        if created.path.join("target").exists() {
            fs::remove_dir_all(created.path.join("target"))?;
        }
        if matches!(case, Case::Tracked | Case::Untracked) {
            run_in_worktree(
                &fixture,
                &created.path,
                &["reset", "--hard", &created.head.to_string()],
            )?;
        }
    }
    Ok(())
}

#[test]
fn gc_protects_nested_repositories_under_ignored_directories() -> Result<(), Box<dyn Error>> {
    for linked in [false, true] {
        for parent_output in [false, true] {
            let fixture = Fixture::new()?;
            fs::write(fixture.root.join(".gitignore"), ".claude/\ntarget/\n")?;
            fixture.run(&["add", ".gitignore"])?;
            fixture.run(&["commit", "-m", "ignore native worktrees"])?;
            let test_context = TestContext::new(&fixture)?;
            let context = test_context.context();
            let created = create(
                &context,
                WorktreeCreate {
                    branch: "nested-host".into(),
                    base: None,
                },
                &cancel(),
            )?;
            let parent = created.path.join(if parent_output {
                "target"
            } else {
                ".claude/worktrees"
            });
            fs::create_dir_all(&parent)?;
            let nested = parent.join("native");
            if linked {
                fixture.run(&[
                    "worktree",
                    "add",
                    "-b",
                    "nested-native",
                    nested.to_str().ok_or("path")?,
                ])?;
            } else {
                fixture.run(&[
                    "clone",
                    fixture.root.to_str().ok_or("root")?,
                    nested.to_str().ok_or("path")?,
                ])?;
            }
            fs::create_dir(nested.join("build"))?;
            fs::write(nested.join("build/source"), "committed")?;
            run_in_worktree(&fixture, &nested, &["add", "-f", "build/source"])?;
            run_in_worktree(&fixture, &nested, &["commit", "-m", "nested source"])?;
            fs::write(nested.join("build/source"), "uncommitted edit")?;
            let report = collect(
                &context,
                &list(&context, &cancel())?,
                &WorktreeGcPolicy {
                    upstream: None,
                    terminal: &|_| false,
                    no_live_lanes: &|_| true,
                    regenerable: &["build".into(), "target".into()],
                    keep: &[],
                    written_before_ns: i128::MAX,
                    dry_run: false,
                    include_foreign: false,
                    retirement_port: None,
                    retirement_feeds: None,
                    mode: CollectionMode::Worktrees,
                    proof: None,
                    upstream_reference: None,
                    integration_patterns: &[],
                    excluded_paths: &[],
                },
                &cancel(),
            )?;
            assert_eq!(
                fs::read_to_string(nested.join("build/source"))?,
                "uncommitted edit"
            );
            assert!(report.idle_output.iter().all(|entry| !entry.removed));
        }
    }
    Ok(())
}

#[test]
fn remove_declared_ignored_output_but_names_undeclared_env() -> Result<(), Box<dyn Error>> {
    for secret in [false, true] {
        let fixture = Fixture::new()?;
        fs::write(fixture.root.join(".gitignore"), "target/\n.env\n")?;
        fixture.run(&["add", ".gitignore"])?;
        fixture.run(&["commit", "-m", "output rules"])?;
        let test_context = TestContext::new(&fixture)?;
        let context = test_context.context();
        let created = create(
            &context,
            WorktreeCreate {
                branch: "remove-output".into(),
                base: None,
            },
            &cancel(),
        )?;
        fs::create_dir(created.path.join("target"))?;
        fs::write(created.path.join("target/artifact"), "output")?;
        if secret {
            fs::write(created.path.join(".env"), "private")?;
        }
        let retirement = HostRetirementFixture::new(&test_context)?;
        let result = remove_with_retirement(
            &context,
            WorktreeRemove::Name {
                name: "remove-output".into(),
                discard: false,
                integration: None,
                expected_head: None,
            },
            &cancel(),
            &provenant_config::ProjectGcSettings {
                regenerable: vec!["target".into()],
                ..Default::default()
            },
            &retirement,
            provenant_core::ActorId::from_bytes([1; 16]),
        );
        if secret {
            let error = result.err().ok_or("removed ignored secret")?;
            assert!(error.to_string().contains(".env"), "{error}");
            assert_eq!(fs::read_to_string(created.path.join(".env"))?, "private");
            assert!(
                created.path.join("target/artifact").exists(),
                "preview must refuse before pruning"
            );
        } else {
            assert_eq!(result?.removed.len(), 1);
            assert!(!created.path.exists());
        }
    }
    Ok(())
}

#[test]
fn remove_lane_scratch_without_declaration() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fs::write(fixture.root.join(".gitignore"), ".provenant/scratch/\n")?;
    fixture.run(&["add", ".gitignore"])?;
    fixture.run(&["commit", "-m", "scratch rules"])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let created = create(
        &context,
        WorktreeCreate {
            branch: "remove-scratch".into(),
            base: None,
        },
        &cancel(),
    )?;
    fs::create_dir_all(created.path.join(".provenant/scratch/graphs"))?;
    fs::write(created.path.join(".provenant/scratch/graphs/x.json"), "{}")?;
    let retirement = HostRetirementFixture::new(&test_context)?;
    let result = remove_with_retirement(
        &context,
        WorktreeRemove::Name {
            name: "remove-scratch".into(),
            discard: false,
            integration: None,
            expected_head: None,
        },
        &cancel(),
        &provenant_config::ProjectGcSettings::default(),
        &retirement,
        provenant_core::ActorId::from_bytes([1; 16]),
    )?;
    assert_eq!(result.removed.len(), 1);
    assert!(!created.path.exists());
    Ok(())
}

#[test]
fn gc_foreign_cleanup_respects_exclusions_census_and_durable_ownership()
-> Result<(), Box<dyn Error>> {
    #[derive(Clone, Copy)]
    enum Retention {
        Unasserted,
        None,
        UnknownWriter,
        Dirty,
    }
    #[derive(Clone, Copy)]
    struct Case {
        include_foreign: bool,
        unmerged: bool,
        recent: bool,
        census_unknown: bool,
        live_lane: bool,
        dirty: bool,
        preview: bool,
        acknowledged: bool,
        expected_path: bool,
        expected_branch: bool,
        expected_state: bool,
        expected_retention: Retention,
    }
    const CASES: [Case; 8] = [
        Case {
            include_foreign: false,
            unmerged: false,
            recent: false,
            census_unknown: false,
            live_lane: false,
            dirty: false,
            preview: false,
            acknowledged: false,
            expected_path: true,
            expected_branch: true,
            expected_state: false,
            expected_retention: Retention::None,
        },
        Case {
            include_foreign: true,
            unmerged: true,
            recent: true,
            census_unknown: false,
            live_lane: false,
            dirty: false,
            preview: false,
            acknowledged: false,
            expected_path: true,
            expected_branch: true,
            expected_state: true,
            expected_retention: Retention::Unasserted,
        },
        Case {
            include_foreign: true,
            unmerged: false,
            recent: false,
            census_unknown: true,
            live_lane: false,
            dirty: false,
            preview: false,
            acknowledged: false,
            expected_path: true,
            expected_branch: true,
            expected_state: true,
            expected_retention: Retention::UnknownWriter,
        },
        Case {
            include_foreign: true,
            unmerged: true,
            recent: false,
            census_unknown: false,
            live_lane: false,
            dirty: false,
            preview: false,
            acknowledged: false,
            expected_path: false,
            expected_branch: true,
            expected_state: true,
            expected_retention: Retention::Unasserted,
        },
        Case {
            include_foreign: true,
            unmerged: false,
            recent: false,
            census_unknown: false,
            live_lane: true,
            dirty: false,
            preview: false,
            acknowledged: false,
            expected_path: true,
            expected_branch: true,
            expected_state: false,
            expected_retention: Retention::UnknownWriter,
        },
        Case {
            include_foreign: true,
            unmerged: false,
            recent: false,
            census_unknown: false,
            live_lane: false,
            dirty: true,
            preview: false,
            acknowledged: false,
            expected_path: true,
            expected_branch: true,
            expected_state: true,
            expected_retention: Retention::Dirty,
        },
        Case {
            include_foreign: true,
            unmerged: false,
            recent: false,
            census_unknown: false,
            live_lane: false,
            dirty: false,
            preview: true,
            acknowledged: false,
            expected_path: true,
            expected_branch: true,
            expected_state: false,
            expected_retention: Retention::None,
        },
        Case {
            include_foreign: true,
            unmerged: false,
            recent: false,
            census_unknown: false,
            live_lane: false,
            dirty: false,
            preview: false,
            acknowledged: true,
            expected_path: false,
            expected_branch: false,
            expected_state: true,
            expected_retention: Retention::None,
        },
    ];
    for case in CASES {
        let fixture = Fixture::new()?;
        let test_context = TestContext::new(&fixture)?;
        let mut context = test_context.context();
        let retirement = HostRetirementFixture::new(&test_context)?;
        let evidence = gc_landing_evidence(&fixture, fixture.head()?)?;
        let path = fixture._temp.path().join("native");
        fixture.run(&[
            "worktree",
            "add",
            "-b",
            "native",
            path.to_str().ok_or("path")?,
        ])?;
        // Recency only protects work that is not already landed.
        if case.unmerged || case.recent {
            fs::write(path.join("new-work"), b"new work")?;
            run_in_worktree(&fixture, &path, &["add", "new-work"])?;
            run_in_worktree(
                &fixture,
                &path,
                &["commit", "--allow-empty", "-m", "unmerged"],
            )?;
        }
        if case.dirty {
            fs::write(path.join("notes"), "keep")?;
        }
        let unknown = UnknownWorkers;
        if case.census_unknown {
            context.workers = &unknown;
        }
        if case.acknowledged {
            acknowledge_foreign_preview(&context)?;
        }
        let lanes = list(&context, &cancel())?;
        let report = collect(
            &context,
            &lanes,
            &WorktreeGcPolicy {
                include_foreign: case.include_foreign,
                retirement_port: Some(&retirement),
                retirement_feeds: Some(&retirement),
                mode: CollectionMode::Worktrees,
                proof: Some(&evidence),
                upstream_reference: None,
                integration_patterns: &[],
                excluded_paths: &[],
                upstream: Some(fixture.head()?),
                terminal: &|_| false,
                no_live_lanes: &|_| !case.live_lane,
                regenerable: &[],
                keep: &[],
                written_before_ns: if case.recent { 0 } else { i128::MAX },
                dry_run: case.preview,
            },
            &cancel(),
        )?;
        if case.preview {
            assert!(
                report
                    .worktrees
                    .removed
                    .iter()
                    .any(|tree| tree.path == path)
            );
            assert!(report.removals.is_empty());
        }
        let retained = report
            .worktrees
            .retained
            .iter()
            .find(|retained| retained.branch == "native")
            .map(|retained| retained.reason);
        match case.expected_retention {
            Retention::Unasserted => {}
            Retention::None => assert!(retained.is_none()),
            Retention::UnknownWriter => assert_eq!(retained, Some(RetainReason::UnknownWriter)),
            Retention::Dirty => assert_eq!(retained, Some(RetainReason::Dirty)),
        }
        if case.acknowledged {
            assert!(report.removals.iter().any(|removal| {
                worktree_subject_matches_path(&removal.subject, &path)
                    && matches!(
                        &removal.outcome,
                        provenant_records::custody::RemovalOutcome::Removed { .. }
                    )
            }));
        }
        assert_eq!(path.exists(), case.expected_path);
        assert_eq!(branch_exists(&fixture, "native"), case.expected_branch);
        assert_eq!(
            context
                .project_state
                .open_verified_directory(&state_dir_name(&path))
                .is_ok(),
            case.expected_state
        );
        if case.preview {
            assert!(
                report
                    .worktrees
                    .pruned_refs
                    .iter()
                    .any(|name| name == "native")
            );
        }
    }
    Ok(())
}

#[test]
fn gc_output_pruning_holds_writer_custody() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fs::write(fixture.root.join(".gitignore"), "target/\n")?;
    fixture.run(&["add", ".gitignore"])?;
    fixture.run(&["commit", "-m", "ignore output"])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let mut lane = gc_merged_worktree(&test_context, &context, "gc-output-lock")?;
    let target = lane.path.join("target");
    fs::create_dir(&target)?;
    fs::write(target.join("artifact"), "output")?;
    lane.dirty = WorktreeCondition::Ignored;
    let retirement = HostRetirementFixture::new(&test_context)?;
    retirement.seed_lane(&context, &lane)?;
    let state = context
        .project_state
        .open_verified_directory(&state_dir_name(&lane.path))?;
    let observations = std::cell::Cell::new(None);
    let calls = std::cell::Cell::new(0);
    let terminal = |_: &Path| {
        let count = calls.get() + 1;
        calls.set(count);
        if count == 1 {
            let writer_held = matches!(
                state.try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive),
                Err(FsError::LockTimeout { .. })
            );
            observations.set(Some(writer_held));
        }
        true
    };
    let report = collect(
        &context,
        std::slice::from_ref(&lane),
        &WorktreeGcPolicy {
            include_foreign: false,
            retirement_port: Some(&retirement),
            retirement_feeds: Some(&retirement),
            mode: CollectionMode::Worktrees,
            proof: None,
            upstream_reference: None,
            integration_patterns: &[],
            excluded_paths: &[],
            upstream: None,
            terminal: &terminal,
            no_live_lanes: &|_| true,
            regenerable: &["target".to_owned()],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: false,
        },
        &cancel(),
    )?;
    assert_eq!(observations.get(), Some(true));
    assert!(!target.exists());
    assert_eq!(report.output.len(), 1);
    assert!(report.output[0].removed);
    Ok(())
}

#[test]
fn landing_output_pruning_keeps_the_writer_lease_through_retirement() -> Result<(), Box<dyn Error>>
{
    let fixture = Fixture::new()?;
    fs::write(fixture.root.join(".gitignore"), "target/\n")?;
    fixture.run(&["add", ".gitignore"])?;
    fixture.run(&["commit", "-m", "ignore output"])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let mut lane = gc_merged_worktree(&test_context, &context, "landing-output-lease")?;
    let target = lane.path.join("target");
    fs::create_dir(&target)?;
    fs::write(target.join("artifact"), "output")?;
    lane.dirty = WorktreeCondition::Ignored;
    let retirement = HostRetirementFixture::new(&test_context)?;
    retirement.seed_lane(&context, &lane)?;
    let state = context
        .project_state
        .open_verified_directory(&state_dir_name(&lane.path))?;
    let output_observation = std::cell::Cell::new(None);
    let retirement_observation = std::cell::Cell::new(None);
    let terminal = |_: &Path| {
        let writer_held = matches!(
            state.try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive),
            Err(FsError::LockTimeout { .. })
        );
        output_observation.set(Some(writer_held));
        true
    };
    let no_live_lanes = |candidate: &Path| {
        if candidate == lane.path && !target.exists() {
            let writer_admitted = state
                .try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive)
                .is_ok();
            retirement_observation.set(Some((!target.exists(), !writer_admitted)));
            !writer_admitted
        } else {
            true
        }
    };
    let report = collect_selected(
        &context,
        std::slice::from_ref(&lane),
        &WorktreeGcPolicy {
            include_foreign: false,
            retirement_port: Some(&retirement),
            retirement_feeds: Some(&retirement),
            mode: CollectionMode::LandingCleanup,
            proof: None,
            upstream_reference: None,
            integration_patterns: &[],
            excluded_paths: &[],
            upstream: None,
            terminal: &terminal,
            no_live_lanes: &no_live_lanes,
            regenerable: &["target".to_owned()],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: false,
        },
        &cancel(),
    )?;

    assert_eq!(output_observation.get(), Some(true));
    assert_eq!(retirement_observation.get(), Some((true, true)));
    assert!(!lane.path.exists());
    assert!(report.output[0].removed);
    assert_eq!(report.worktrees.removed.len(), 1);
    Ok(())
}

#[test]
fn gc_foreign_rechecks_changes_before_removal() -> Result<(), Box<dyn Error>> {
    #[derive(Clone, Copy)]
    enum Change {
        Ignored,
        Head,
    }
    for change in [Change::Ignored, Change::Head] {
        let fixture = Fixture::new()?;
        fs::write(fixture.root.join(".gitignore"), ".env\n")?;
        fixture.run(&["add", ".gitignore"])?;
        fixture.run(&["commit", "-m", "ignore local environment"])?;
        let test_context = TestContext::new(&fixture)?;
        let context = test_context.context();
        let lane = gc_merged_worktree(&test_context, &context, "native-recheck")?;
        let path = lane.path.clone();
        let retirement = HostRetirementFixture::new(&test_context)?;
        retirement.seed_lane(&context, &lane)?;
        let upstream = fixture.head()?;
        let evidence = gc_landing_evidence(&fixture, upstream)?;
        let lanes = list(&context, &cancel())?;
        let changed = std::cell::Cell::new(false);
        let no_live = |candidate: &Path| {
            if !changed.get() && candidate == path {
                changed.set(true);
                match change {
                    Change::Ignored => fs::write(path.join(".env"), "new secret").unwrap(),
                    Change::Head => run_in_worktree(
                        &fixture,
                        &path,
                        &["commit", "--allow-empty", "-m", "concurrent work"],
                    )
                    .unwrap(),
                }
            }
            true
        };
        let report = collect(
            &context,
            &lanes,
            &WorktreeGcPolicy {
                upstream: Some(upstream),
                terminal: &|_| false,
                no_live_lanes: &no_live,
                regenerable: &[],
                keep: &[],
                written_before_ns: i128::MAX,
                dry_run: false,
                include_foreign: true,
                retirement_port: Some(&retirement),
                retirement_feeds: Some(&retirement),
                mode: CollectionMode::Worktrees,
                proof: Some(&evidence),
                upstream_reference: None,
                integration_patterns: &[],
                excluded_paths: &[],
            },
            &cancel(),
        )?;
        assert!(changed.get());
        assert!(path.is_dir());
        let reason = if matches!(change, Change::Ignored) {
            RetainReason::Dirty
        } else {
            RetainReason::Changed
        };
        assert!(
            report.worktrees.retained.iter().any(|retained| {
                retained.branch == "native-recheck" && retained.reason == reason
            })
        );
        if matches!(change, Change::Ignored) {
            assert_eq!(fs::read_to_string(path.join(".env"))?, "new secret");
        }
    }
    Ok(())
}

#[test]
fn gc_foreign_prunes_while_writer_custody_is_held() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fs::write(fixture.root.join(".gitignore"), "target/\n")?;
    fixture.run(&["add", ".gitignore"])?;
    fixture.run(&["commit", "-m", "ignore output"])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let mut lane = gc_merged_worktree(&test_context, &context, "foreign-output-lock")?;
    let retirement = HostRetirementFixture::new(&test_context)?;
    retirement.seed_lane(&context, &lane)?;
    let upstream = fixture.head()?;
    let evidence = gc_landing_evidence(&fixture, upstream)?;
    fs::create_dir(lane.path.join("target"))?;
    fs::write(lane.path.join("target/artifact"), "output")?;
    lane.occupancy = Occupancy::custody_unknown(lane.path.clone(), "fixture unknown writer");
    let state = context
        .project_state
        .open_verified_directory(&state_dir_name(&lane.path))?;
    let target = lane.path.join("target");
    let prune_boundary = std::cell::Cell::new(None);
    let terminal = |candidate: &Path| {
        if candidate == lane.path {
            let writer_held = matches!(
                state.try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive),
                Err(FsError::LockTimeout { .. })
            );
            // `terminal` is called immediately before collect_lane_output walks
            // and prunes this candidate, at the effect boundary under test.
            prune_boundary.set(Some((writer_held, target.exists())));
        }
        false
    };
    let report = collect(
        &context,
        std::slice::from_ref(&lane),
        &WorktreeGcPolicy {
            upstream: Some(upstream),
            terminal: &terminal,
            no_live_lanes: &|_| true,
            regenerable: &["target".into()],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: false,
            include_foreign: true,
            retirement_port: Some(&retirement),
            retirement_feeds: Some(&retirement),
            mode: CollectionMode::Worktrees,
            proof: Some(&evidence),
            upstream_reference: None,
            integration_patterns: &[],
            excluded_paths: &[],
        },
        &cancel(),
    )?;
    assert!(report.removals.iter().any(|removal| {
        worktree_subject_matches_path(&removal.subject, &lane.path)
            && matches!(
                &removal.outcome,
                provenant_records::custody::RemovalOutcome::Removed { .. }
            )
    }));
    assert!(!lane.path.exists());
    assert!(!branch_exists(&fixture, "foreign-output-lock"));
    assert_eq!(prune_boundary.get(), Some((true, true)));
    assert!(!target.exists());
    assert!(
        report
            .idle_output
            .iter()
            .any(|output| output.path == target && output.removed)
    );
    Ok(())
}

#[test]
fn gc_failed_prune_retains_once_and_continues() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let first = gc_merged_worktree(&test_context, &context, "gc-prune-failed")?;
    let second = gc_merged_worktree(&test_context, &context, "gc-prune-next")?;
    let retirement = HostRetirementFixture::new(&test_context)?;
    retirement.seed_lane(&context, &first)?;
    retirement.seed_lane(&context, &second)?;
    let upstream = fixture.head()?;
    let evidence = gc_landing_evidence(&fixture, upstream)?;
    let output_walk_started = std::cell::Cell::new(false);
    let post_output_eligibility_passed_without_custody = std::cell::Cell::new(false);
    let mutation_boundary_rejected = std::cell::Cell::new(false);
    let first_state = context
        .project_state
        .open_verified_directory(&state_dir_name(&first.path))?;
    let no_live_lanes = |path: &Path| {
        if path != first.path {
            return true;
        }
        let writer_held = matches!(
            first_state.try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive),
            Err(FsError::LockTimeout { .. })
        );
        if output_walk_started.get() {
            if !writer_held {
                // Worktree GC releases its initial lease before starting the
                // final fenced removal. Observe that unlocked eligibility
                // pass before refusing under the removal lease below.
                post_output_eligibility_passed_without_custody.set(true);
            } else if post_output_eligibility_passed_without_custody.get() {
                // This is the final eligibility callback under the newly
                // acquired writer lease, immediately before Git mutation.
                // Refuse here to model custody becoming unknown at the
                // effect boundary, after initial eligibility passed.
                mutation_boundary_rejected.set(true);
                return false;
            }
        }
        true
    };
    let terminal = |candidate: &Path| {
        if candidate == first.path {
            // Output collection begins after the initial eligibility check
            // while GC holds its first lease; this marks the later removal
            // eligibility callbacks without relying on repository locking.
            output_walk_started.set(true);
        }
        true
    };
    let report = collect(
        &context,
        &[first.clone(), second.clone()],
        &WorktreeGcPolicy {
            include_foreign: false,
            retirement_port: Some(&retirement),
            retirement_feeds: Some(&retirement),
            mode: CollectionMode::Worktrees,
            proof: Some(&evidence),
            upstream_reference: None,
            integration_patterns: &[],
            excluded_paths: &[],
            upstream: Some(upstream),
            terminal: &terminal,
            no_live_lanes: &no_live_lanes,
            regenerable: &["target".to_owned()],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: false,
        },
        &cancel(),
    )?;
    assert!(output_walk_started.get());
    assert!(post_output_eligibility_passed_without_custody.get());
    assert!(mutation_boundary_rejected.get());
    assert!(first.path.is_dir());
    assert!(branch_exists(&fixture, "gc-prune-failed"));
    assert!(report.worktrees.retained.iter().any(|retained| {
        retained.branch == "gc-prune-failed" && retained.reason == RetainReason::UnknownWriter
    }));
    let first_removals: Vec<_> = report
        .removals
        .iter()
        .filter(|removal| {
            worktree_subject_matches_path(&removal.subject, &first.path)
                && removal.tip == first.head
        })
        .collect();
    assert_eq!(first_removals.len(), 1, "{first_removals:?}");
    assert!(matches!(
        &first_removals[0].outcome,
        provenant_records::custody::RemovalOutcome::Kept {
            cause: provenant_records::custody::RemovalUncertainty::CustodyUnknown
        }
    ));
    assert!(report.removals.iter().any(|removal| {
        worktree_subject_matches_path(&removal.subject, &second.path)
            && matches!(
                &removal.outcome,
                provenant_records::custody::RemovalOutcome::Removed { .. }
            )
    }));
    assert!(!second.path.exists());
    assert!(!branch_exists(&fixture, "gc-prune-next"));
    Ok(())
}

#[test]
fn gc_refreshes_the_owned_ref_generation_before_recording_retirement() -> Result<(), Box<dyn Error>>
{
    let fixture = Fixture::new()?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let created = gc_merged_worktree(&test_context, &context, "gc-advanced-generation")?;
    let branch = GitRefName::branch("gc-advanced-generation")?;
    let ledger = RefLedger::for_context(&context)?;
    let original = ledger
        .load(&RefEndpoint::Local, &branch)?
        .ok_or("missing original ref generation")?;

    run_in_worktree(
        &fixture,
        &created.path,
        &["commit", "--allow-empty", "-m", "advance lane tip"],
    )?;
    fixture.run(&[
        "merge",
        "--no-ff",
        "gc-advanced-generation",
        "-m",
        "merge advanced GC fixture",
    ])?;
    let lane = list(&context, &cancel())?
        .into_iter()
        .find(|lane| lane.path == created.path)
        .ok_or("missing advanced GC worktree")?;
    assert!(is_ancestor(
        &context,
        lane.head,
        fixture.head()?,
        &cancel(),
    )?);
    let owner = lane
        .owner
        .as_ref()
        .map_err(|_| "invalid owner record")?
        .as_ref()
        .ok_or("missing worktree owner")?;
    assert_eq!(
        owner
            .reference
            .as_ref()
            .map(|reference| reference.generation),
        Some(original.generation)
    );

    let retirement = HostRetirementFixture::new(&test_context)?;
    let worktree = acquire_worktree_subject_at(&context, &lane.path)?;
    let upstream = fixture.head()?;
    let evidence = gc_landing_evidence(&fixture, upstream)?;
    let report = collect(
        &context,
        std::slice::from_ref(&lane),
        &WorktreeGcPolicy {
            include_foreign: false,
            retirement_port: Some(&retirement),
            retirement_feeds: Some(&retirement),
            mode: CollectionMode::Worktrees,
            proof: Some(&evidence),
            upstream_reference: None,
            integration_patterns: &[],
            excluded_paths: &[],
            upstream: Some(upstream),
            terminal: &|_| true,
            no_live_lanes: &|_| true,
            regenerable: &[],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: false,
        },
        &cancel(),
    )?;

    let current = ledger
        .load(&RefEndpoint::Local, &branch)?
        .ok_or("missing refreshed ref generation")?;
    assert!(current.generation > original.generation);
    assert_eq!(current.tip, lane.head);
    assert_eq!(current.origin, original.origin);
    let recorded = retirement
        .retirement_for_subject(context.binding.project_id(), &worktree)?
        .ok_or("missing durable retirement")?;
    assert_eq!(recorded.reference.generation, current.generation);
    assert_eq!(recorded.expected_tip, lane.head);
    assert!(
        report
            .worktrees
            .removed
            .iter()
            .any(|tree| tree.path == lane.path)
    );
    assert!(!lane.path.exists());
    assert!(!branch_exists(&fixture, "gc-advanced-generation"));
    Ok(())
}

#[test]
fn gc_reclaims_unmanaged_output_but_keeps_worktree() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fs::write(fixture.root.join(".gitignore"), "target/\n")?;
    fixture.run(&["add", ".gitignore"])?;
    fixture.run(&["commit", "-m", "ignore output"])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let unmanaged = fixture._temp.path().join("unmanaged");
    fixture.run(&[
        "worktree",
        "add",
        "-b",
        "gc-unmanaged",
        unmanaged.to_str().ok_or("non-UTF8 worktree path")?,
    ])?;
    run_in_worktree(
        &fixture,
        &unmanaged,
        &["commit", "--allow-empty", "-m", "gc-unmanaged"],
    )?;
    fixture.run(&["merge", "--no-ff", "gc-unmanaged", "-m", "merge GC fixture"])?;
    let target = unmanaged.join("target");
    fs::create_dir(&target)?;
    fs::write(target.join("artifact"), "keep")?;
    let canonical = canonicalize(&unmanaged)?;
    let mut lane = list(&context, &cancel())?
        .into_iter()
        .find(|lane| lane.path == canonical)
        .ok_or("missing unmanaged fixture worktree")?;
    lane.dirty = WorktreeCondition::Ignored;
    let report = collect(
        &context,
        &[lane],
        &WorktreeGcPolicy {
            include_foreign: true,
            retirement_port: None,
            retirement_feeds: None,
            // Output retention alone reclaims the output and keeps the tree.
            mode: CollectionMode::Outputs,
            proof: None,
            upstream_reference: None,
            integration_patterns: &[],
            excluded_paths: &[],
            upstream: Some(fixture.head()?),
            terminal: &|_| false,
            no_live_lanes: &|_| true,
            regenerable: &["target".to_owned()],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: false,
        },
        &cancel(),
    )?;
    assert!(!target.join("artifact").exists());
    assert!(unmanaged.is_dir());
    assert!(branch_exists(&fixture, "gc-unmanaged"));
    assert!(report.worktrees.removed.is_empty());
    assert!(report.worktrees.retained.is_empty());
    assert!(report.output.is_empty());
    assert_eq!(report.idle_output.len(), 1);
    assert_eq!(report.idle_output[0].path, target);
    assert!(report.idle_output[0].removed);
    Ok(())
}

#[test]
fn gc_merged_tree_previews_and_removes_declared_output() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fs::write(fixture.root.join(".gitignore"), "target/\n")?;
    fixture.run(&["add", ".gitignore"])?;
    fixture.run(&["commit", "-m", "ignore output"])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let mut lane = gc_merged_worktree(&test_context, &context, "gc-recent-merged")?;
    let retirement = HostRetirementFixture::new(&test_context)?;
    retirement.seed_lane(&context, &lane)?;
    let upstream = fixture.head()?;
    let evidence = gc_landing_evidence(&fixture, upstream)?;
    fs::create_dir(lane.path.join("target"))?;
    fs::write(lane.path.join("target/artifact"), "recent")?;
    lane.dirty = WorktreeCondition::Ignored;
    let settings = WorktreeGcPolicy {
        upstream: Some(upstream),
        terminal: &|_| true,
        no_live_lanes: &|_| true,
        regenerable: &["target".into()],
        keep: &[],
        written_before_ns: i128::MAX,
        dry_run: true,
        include_foreign: false,
        retirement_port: Some(&retirement),
        retirement_feeds: Some(&retirement),
        mode: CollectionMode::Worktrees,
        proof: Some(&evidence),
        upstream_reference: None,
        integration_patterns: &[],
        excluded_paths: &[],
    };
    let preview = collect(&context, &[lane.clone()], &settings, &cancel())?;
    assert!(
        preview
            .worktrees
            .removed
            .iter()
            .any(|tree| tree.path == lane.path)
    );
    assert!(branch_exists(&fixture, "gc-recent-merged"));
    assert!(lane.path.join("target/artifact").exists());
    let applied = collect(
        &context,
        &[lane.clone()],
        &WorktreeGcPolicy {
            dry_run: false,
            ..settings
        },
        &cancel(),
    )?;
    assert!(applied.removals.iter().any(|removal| {
        worktree_subject_matches_path(&removal.subject, &lane.path)
            && matches!(
                &removal.outcome,
                provenant_records::custody::RemovalOutcome::Removed { .. }
            )
    }));
    assert!(!lane.path.exists());
    assert!(!branch_exists(&fixture, "gc-recent-merged"));
    Ok(())
}

#[test]
fn gc_retires_only_lanes_contained_in_a_retained_ref() -> Result<(), Box<dyn Error>> {
    merge_proof_scenario()
}

#[test]
fn locked_registration_stays_unknown_when_census_reports_live() -> Result<(), Box<dyn Error>> {
    struct LiveWorkers;
    impl WorkerObserver for LiveWorkers {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            roots
                .iter()
                .cloned()
                .map(|root| (root, Occupancy::Observed { process: None }))
                .collect()
        }
    }

    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let mut context = test.context();
    let path = fixture.root.join(".worktrees/locked-live");
    fixture.run(&[
        "worktree",
        "add",
        "-b",
        "locked-live",
        path.to_str().ok_or("path")?,
    ])?;
    fixture.run(&["worktree", "lock", path.to_str().ok_or("path")?])?;
    context.workers = &LiveWorkers;

    let listed = list(&context, &cancel())?
        .into_iter()
        .find(|entry| entry.path == path)
        .ok_or("locked linked worktree missing from inventory")?;
    assert!(matches!(listed.occupancy, Occupancy::Unknown { .. }));
    Ok(())
}
