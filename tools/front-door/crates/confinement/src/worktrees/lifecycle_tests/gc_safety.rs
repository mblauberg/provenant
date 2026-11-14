use super::*;

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
    let removed = remove(
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
        let error = remove(&context, request(false), &cancel())
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
            let removed = remove(&context, request(true), &cancel())?;
            assert_eq!(removed.removed.len(), 1);
            assert_ne!(removed.removed[0].head, info.head);
            assert!(removed.pruned_refs.is_empty());
            assert!(!info.path.exists());
        }
    }
    Ok(())
}

#[test]
fn ignored_paths_refusal_names_regenerable_setting() {
    let message = WorktreeError::IgnoredPaths("target, .build".into()).with_repair();
    assert!(message.contains("target, .build"));
    assert!(message.contains("gc.projects.<id>.regenerable"));
    assert!(message.contains("provenant fabric identity"));
}

#[test]
fn gc_idle_output_requires_ignored_idle_and_free_writer() -> Result<(), Box<dyn Error>> {
    for scenario in ["idle", "recent", "lease", "tracked", "untracked", "lane"] {
        let fixture = Fixture::new()?;
        fs::write(
            fixture.root.join(".gitignore"),
            if scenario == "untracked" {
                ".env\n"
            } else {
                "target/\n.env\n"
            },
        )?;
        if scenario == "tracked" {
            fs::create_dir(fixture.root.join("target"))?;
            fs::write(fixture.root.join("target/source"), "tracked")?;
            fixture.run(&["add", "-f", "target/source"])?;
        }
        fixture.run(&["add", ".gitignore"])?;
        fixture.run(&["commit", "-m", "output rules"])?;
        let test_context = TestContext::new(&fixture)?;
        let context = test_context.context();
        let created = create(
            &context,
            WorktreeCreate {
                branch: "idle-output".into(),
                base: None,
            },
            &cancel(),
        )?;
        fs::create_dir_all(created.path.join("target"))?;
        fs::write(created.path.join("target/cache"), "output")?;
        let lanes = list(&context, &cancel())?;
        if scenario == "untracked" {
            // Call the pruning owner directly so the early dirty-lane gate
            // cannot mask the non-ignored output protection.
            let entries = context.facts.read_inventory(&context, &cancel())?;
            let entry = entries
                .iter()
                .find(|entry| entry.path == created.path)
                .ok_or("worktree")?;
            let locked = lock_worktree_for_mutation(&context, entry, &cancel())?;
            let refused = gc::prepare_removal(
                &context,
                locked.verified.repo(),
                &["target".into()],
                &[],
                &cancel(),
            );
            assert!(
                matches!(refused, Err(WorktreeError::IgnoredPaths(ref paths)) if paths.contains("target"))
            );
            assert_eq!(
                fs::read_to_string(created.path.join("target/cache"))?,
                "output"
            );
        }
        let state = context
            .project_state
            .open_verified_directory(&state_dir_name(&created.path))?;
        let lease = if scenario == "lease" {
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
                mode: CollectionMode::Worktrees,
                proof: None,
                upstream_reference: None,
                integration_patterns: &[],
                excluded_paths: &[],
                upstream: None,
                terminal: &|_| false,
                no_live_lanes: &|_| scenario != "lane",
                regenerable: &["target".into()],
                keep: &[],
                written_before_ns: if scenario == "recent" { 0 } else { i128::MAX },
                dry_run: false,
            },
            &cancel(),
        )?;
        assert_eq!(
            created.path.join("target/cache").exists(),
            scenario != "idle",
            "{scenario}"
        );
        assert_eq!(
            report.idle_output.iter().filter(|row| row.removed).count(),
            usize::from(scenario == "idle"),
            "{scenario}"
        );
        assert!(created.path.is_dir());
        drop(lease);
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
    for scenario in [
        "default",
        "recent",
        "unknown",
        "unmerged",
        "live-lane",
        "dirty",
        "preview",
        "apply",
    ] {
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
        if scenario == "unmerged" {
            fs::write(path.join("new-work"), b"new work")?;
            run_in_worktree(&fixture, &path, &["add", "new-work"])?;
            run_in_worktree(
                &fixture,
                &path,
                &["commit", "--allow-empty", "-m", "unmerged"],
            )?;
        }
        if scenario == "dirty" {
            fs::write(path.join("notes"), "keep")?;
        }
        let unknown = UnknownWorkers;
        if scenario == "unknown" {
            context.workers = &unknown;
        }
        if scenario == "apply" {
            acknowledge_foreign_preview(&context)?;
        }
        let lanes = list(&context, &cancel())?;
        let report = collect(
            &context,
            &lanes,
            &WorktreeGcPolicy {
                include_foreign: scenario != "default",
                retirement_port: Some(&retirement),
                retirement_feeds: Some(&retirement),
                mode: CollectionMode::Worktrees,
                proof: Some(&evidence),
                upstream_reference: None,
                integration_patterns: &[],
                excluded_paths: &[],
                upstream: Some(fixture.head()?),
                terminal: &|_| false,
                no_live_lanes: &|_| scenario != "live-lane",
                regenerable: &[],
                keep: &[],
                written_before_ns: if scenario == "recent" { 0 } else { i128::MAX },
                dry_run: scenario == "preview",
            },
            &cancel(),
        )?;
        let worktree_subject = provenant_records::custody::Subject::new(
            provenant_records::custody::SubjectKind::Worktree,
            path.to_string_lossy().into_owned(),
        );
        if scenario == "preview" {
            assert!(
                report
                    .worktrees
                    .removed
                    .iter()
                    .any(|tree| tree.path == path)
            );
            assert!(report.removals.is_empty());
        }
        let expected = match scenario {
            "unknown" | "live-lane" => Some(RetainReason::UnknownWriter),
            "dirty" => Some(RetainReason::Dirty),
            _ => None,
        };
        if let Some(reason) = expected {
            assert!(
                report
                    .worktrees
                    .retained
                    .iter()
                    .any(|retained| { retained.branch == "native" && retained.reason == reason })
            );
        }
        if scenario == "apply" {
            assert!(!path.exists(), "applied foreign retirement left its tree");
            assert!(!branch_exists(&fixture, "native"));
            assert!(report.removals.iter().any(|removal| {
                removal.subject == worktree_subject
                    && matches!(
                        &removal.outcome,
                        provenant_records::custody::RemovalOutcome::Removed { .. }
                    )
            }));
        } else {
            assert!(
                path.exists(),
                "{scenario}: preview or protected tree was deleted"
            );
        }
        assert_eq!(
            context
                .project_state
                .open_verified_directory(&state_dir_name(&path))
                .is_ok(),
            !matches!(scenario, "default" | "preview" | "live-lane" | "unmerged"),
            "exclusion state after {scenario}"
        );
        assert_eq!(branch_exists(&fixture, "native"), scenario != "apply");
        if scenario == "preview" {
            assert_eq!(report.worktrees.pruned_refs, vec!["native"]);
        }
    }
    Ok(())
}

#[test]
fn gc_output_pruning_holds_writer_but_not_repository_lock() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fs::write(fixture.root.join(".gitignore"), "target/\n")?;
    fixture.run(&["add", ".gitignore"])?;
    fixture.run(&["commit", "-m", "ignore output"])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let mut lane = gc_merged_worktree(&fixture, &context, "gc-output-lock")?;
    let target = lane.path.join("target");
    fs::create_dir(&target)?;
    fs::write(target.join("artifact"), "output")?;
    lane.dirty = WorktreeCondition::Ignored;
    let state = context
        .project_state
        .open_verified_directory(&state_dir_name(&lane.path))?;
    let observations = std::cell::Cell::new(None);
    let calls = std::cell::Cell::new(0);
    let terminal = |_: &Path| {
        let count = calls.get() + 1;
        calls.set(count);
        if count == 1 {
            let repository_free = context
                .repo
                .common_dir()
                .try_lock_existing(OsStr::new(REPOSITORY_LOCK), LockMode::Exclusive)
                .is_ok();
            let writer_held = matches!(
                state.try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive),
                Err(FsError::LockTimeout { .. })
            );
            observations.set(Some((repository_free, writer_held)));
        }
        true
    };
    let report = collect(
        &context,
        std::slice::from_ref(&lane),
        &WorktreeGcPolicy {
            include_foreign: false,
            retirement_port: None,
            retirement_feeds: None,
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
    assert_eq!(observations.get(), Some((true, true)));
    assert!(!target.exists());
    assert_eq!(report.output.len(), 1);
    assert!(report.output[0].removed);
    Ok(())
}

#[test]
fn gc_foreign_rechecks_changes_before_removal() -> Result<(), Box<dyn Error>> {
    for change in ["ignored", "head"] {
        let fixture = Fixture::new()?;
        fs::write(fixture.root.join(".gitignore"), ".env\n")?;
        fixture.run(&["add", ".gitignore"])?;
        fixture.run(&["commit", "-m", "ignore local environment"])?;
        let test_context = TestContext::new(&fixture)?;
        let context = test_context.context();
        let lane = gc_merged_worktree(&fixture, &context, "native-recheck")?;
        let path = lane.path.clone();
        let retirement = HostRetirementFixture::new(&test_context)?;
        retirement.seed_lane(&context, &lane)?;
        let upstream = fixture.head()?;
        let evidence = gc_landing_evidence(&fixture, upstream)?;
        let lanes = list(&context, &cancel())?;
        let changed = std::cell::Cell::new(false);
        let calls = std::cell::Cell::new(0);
        let no_live = |_: &Path| {
            let count = calls.get() + 1;
            calls.set(count);
            let repository_held = context
                .repo
                .common_dir()
                .try_lock_existing(OsStr::new(REPOSITORY_LOCK), LockMode::Exclusive)
                .is_err();
            if !changed.get()
                && ((change == "ignored" && repository_held) || (change == "head" && count == 3))
            {
                changed.set(true);
                if change == "ignored" {
                    fs::write(path.join(".env"), "new secret").unwrap();
                } else {
                    run_in_worktree(
                        &fixture,
                        &path,
                        &["commit", "--allow-empty", "-m", "concurrent work"],
                    )
                    .unwrap();
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
        let reason = if change == "ignored" {
            RetainReason::Dirty
        } else {
            RetainReason::Changed
        };
        assert!(
            report.worktrees.retained.iter().any(|retained| {
                retained.branch == "native-recheck" && retained.reason == reason
            })
        );
        if change == "ignored" {
            assert_eq!(fs::read_to_string(path.join(".env"))?, "new secret");
        }
    }
    Ok(())
}

#[test]
fn gc_foreign_prunes_under_writer_then_locks_repository() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fs::write(fixture.root.join(".gitignore"), "target/\n")?;
    fixture.run(&["add", ".gitignore"])?;
    fixture.run(&["commit", "-m", "ignore output"])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let mut lane = gc_merged_worktree(&fixture, &context, "foreign-output-lock")?;
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
    let observations = std::cell::RefCell::new(Vec::new());
    let no_live_lanes = |_: &Path| {
        if state
            .try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive)
            .is_err()
        {
            observations.borrow_mut().push(
                context
                    .repo
                    .common_dir()
                    .try_lock_existing(OsStr::new(REPOSITORY_LOCK), LockMode::Exclusive)
                    .is_ok(),
            );
        }
        true
    };
    let report = collect(
        &context,
        std::slice::from_ref(&lane),
        &WorktreeGcPolicy {
            upstream: Some(upstream),
            terminal: &|_| false,
            no_live_lanes: &no_live_lanes,
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
    let subject = provenant_records::custody::Subject::new(
        provenant_records::custody::SubjectKind::Worktree,
        lane.path.to_string_lossy().into_owned(),
    );
    assert!(report.removals.iter().any(|removal| {
        removal.subject == subject
            && matches!(
                &removal.outcome,
                provenant_records::custody::RemovalOutcome::Removed { .. }
            )
    }));
    assert!(!lane.path.exists());
    assert!(!branch_exists(&fixture, "foreign-output-lock"));
    let observed = observations.borrow();
    assert_eq!(observed.first(), Some(&true));
    assert_eq!(observed.last(), Some(&false));
    Ok(())
}

#[test]
fn gc_failed_prune_retains_once_and_continues() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let first = gc_merged_worktree(&fixture, &context, "gc-prune-failed")?;
    let second = gc_merged_worktree(&fixture, &context, "gc-prune-next")?;
    let retirement = HostRetirementFixture::new(&test_context)?;
    retirement.seed_lane(&context, &first)?;
    retirement.seed_lane(&context, &second)?;
    let upstream = fixture.head()?;
    let evidence = gc_landing_evidence(&fixture, upstream)?;
    let calls = std::cell::Cell::new(0);
    let no_live_lanes = |path: &Path| {
        if path == first.path {
            calls.set(calls.get() + 1);
            calls.get() != 2
        } else {
            true
        }
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
            terminal: &|_| true,
            no_live_lanes: &no_live_lanes,
            regenerable: &["target".to_owned()],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: false,
        },
        &cancel(),
    )?;
    assert!(
        calls.get() >= 2,
        "the live lane observation is rechecked at the mutation fence"
    );
    assert!(first.path.is_dir());
    assert!(branch_exists(&fixture, "gc-prune-failed"));
    assert!(report.worktrees.retained.iter().any(|retained| {
        retained.branch == "gc-prune-failed" && retained.reason == RetainReason::UnknownWriter
    }));
    let first_subject = provenant_records::custody::Subject::new(
        provenant_records::custody::SubjectKind::Worktree,
        first.path.to_string_lossy().into_owned(),
    );
    let second_subject = provenant_records::custody::Subject::new(
        provenant_records::custody::SubjectKind::Worktree,
        second.path.to_string_lossy().into_owned(),
    );
    assert!(report.removals.iter().any(|removal| {
        removal.subject == first_subject
            && matches!(
                &removal.outcome,
                provenant_records::custody::RemovalOutcome::Kept {
                    cause: provenant_records::custody::RemovalUncertainty::CustodyUnknown
                }
            )
    }));
    assert!(report.removals.iter().any(|removal| {
        removal.subject == second_subject
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
    let created = gc_merged_worktree(&fixture, &context, "gc-advanced-generation")?;
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
    let worktree = Subject::new(
        SubjectKind::Worktree,
        fs::canonicalize(&lane.path)?.to_string_lossy().into_owned(),
    );
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
            include_foreign: false,
            retirement_port: None,
            retirement_feeds: None,
            mode: CollectionMode::Worktrees,
            proof: None,
            upstream_reference: None,
            integration_patterns: &[],
            excluded_paths: &[],
            upstream: Some(fixture.head()?),
            terminal: &|_| true,
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
    let mut lane = gc_merged_worktree(&fixture, &context, "gc-recent-merged")?;
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
    let subject = provenant_records::custody::Subject::new(
        provenant_records::custody::SubjectKind::Worktree,
        lane.path.to_string_lossy().into_owned(),
    );
    assert!(applied.removals.iter().any(|removal| {
        removal.subject == subject
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
fn gc_merge_proof_requires_containment_in_the_pinned_default() -> Result<(), Box<dyn Error>> {
    // A concurrent test's fork can briefly inherit a just-released lock
    // descriptor; rebuild the whole fixture rather than weaken zero-wait locks.
    for _ in 0..5 {
        match merge_proof_scenario() {
            Err(error) if format!("{error:?}").contains("LockTimeout") => {}
            result => return result,
        }
    }
    merge_proof_scenario()
}

#[test]
fn gc_census_never_runs_under_the_repository_lock() -> Result<(), Box<dyn Error>> {
    for scenario in ["managed", "managed-preview", "foreign", "foreign-preview"] {
        let fixture = Fixture::new()?;
        let test_context = TestContext::new(&fixture)?;
        let mut context = test_context.context();
        let mut lane = gc_merged_worktree(&fixture, &context, "gc-census-lock")?;
        let retirement = HostRetirementFixture::new(&test_context)?;
        retirement.seed_lane(&context, &lane)?;
        let upstream = fixture.head()?;
        let evidence = gc_landing_evidence(&fixture, upstream)?;
        let probe = LockProbeWorkers {
            common: context.repo.common_dir(),
            repository_held: Default::default(),
        };
        context.workers = &probe;
        let foreign = scenario.starts_with("foreign");
        if foreign {
            lane.occupancy =
                Occupancy::custody_unknown(lane.path.clone(), "fixture unknown writer");
        }
        let report = collect(
            &context,
            std::slice::from_ref(&lane),
            &WorktreeGcPolicy {
                include_foreign: foreign,
                retirement_port: Some(&retirement),
                retirement_feeds: Some(&retirement),
                mode: CollectionMode::Worktrees,
                proof: Some(&evidence),
                upstream_reference: None,
                integration_patterns: &[],
                excluded_paths: &[],
                upstream: Some(upstream),
                terminal: &|_| !foreign,
                no_live_lanes: &|_| true,
                regenerable: &[],
                keep: &[],
                written_before_ns: i128::MAX,
                dry_run: scenario.ends_with("preview"),
            },
            &cancel(),
        )?;
        if scenario.ends_with("preview") {
            assert!(
                report
                    .worktrees
                    .removed
                    .iter()
                    .any(|tree| tree.path == lane.path)
            );
            assert!(lane.path.is_dir());
        } else {
            let subject = provenant_records::custody::Subject::new(
                provenant_records::custody::SubjectKind::Worktree,
                lane.path.to_string_lossy().into_owned(),
            );
            assert!(report.removals.iter().any(|removal| {
                removal.subject == subject
                    && matches!(
                        &removal.outcome,
                        provenant_records::custody::RemovalOutcome::Removed { .. }
                    )
            }));
            assert!(!lane.path.exists());
        }
        let observed = probe.repository_held.lock().unwrap().clone();
        assert!(!observed.is_empty(), "{scenario}");
        assert!(
            observed.iter().all(|held| !held),
            "{scenario}: census under the repository lock {observed:?}"
        );
    }
    Ok(())
}

#[test]
fn inventory_census_runs_after_repository_snapshot_lock_is_released() -> Result<(), Box<dyn Error>>
{
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let mut context = test.context();
    let probe = LockProbeWorkers {
        common: context.repo.common_dir(),
        repository_held: Default::default(),
    };
    context.workers = &probe;

    // `collect` and the CLI's project-GC discovery both obtain their candidate
    // set through this inventory path before applying GC policy.
    let inventory = list(&context, &cancel())?;
    assert!(!inventory.is_empty());
    assert_eq!(
        *probe.repository_held.lock().unwrap(),
        vec![false],
        "native census must run after the Git registration snapshot lock drops"
    );
    Ok(())
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
