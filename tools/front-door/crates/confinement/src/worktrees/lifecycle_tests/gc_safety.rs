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
                foreign_merged_clean: false,
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
                    foreign_merged_clean: false,
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
        let result = remove_with_output(
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
fn gc_foreign_merged_clean_is_opt_in_immediate_and_requires_complete_census()
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
        let path = fixture._temp.path().join("native");
        fixture.run(&[
            "worktree",
            "add",
            "-b",
            "native",
            path.to_str().ok_or("path")?,
        ])?;
        if scenario == "unmerged" {
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
        let lanes = list(&context, &cancel())?;
        let report = collect(
            &context,
            &lanes,
            &WorktreeGcPolicy {
                foreign_merged_clean: scenario != "default",
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
        assert_eq!(
            report.worktrees.removed.len(),
            usize::from(matches!(scenario, "preview" | "apply" | "recent")),
            "{scenario}: {report:?}"
        );
        let expected = match scenario {
            "unknown" | "live-lane" => Some(RetainReason::UnknownWriter),
            "unmerged" => Some(RetainReason::NotMerged),
            "dirty" => Some(RetainReason::Dirty),
            _ => None,
        };
        assert_eq!(
            report.worktrees.retained,
            expected
                .into_iter()
                .map(|reason| RetainedBranch {
                    branch: "native".into(),
                    reason,
                    detail: (scenario == "unknown").then(|| "fixture census unavailable".into()),
                })
                .collect::<Vec<_>>(),
            "{scenario}"
        );
        assert_eq!(
            path.exists(),
            !matches!(scenario, "apply" | "recent"),
            "{scenario}"
        );
        assert_eq!(
            context
                .project_state
                .open_verified_directory(&state_dir_name(&path))
                .is_ok(),
            !matches!(scenario, "default" | "preview" | "live-lane" | "unmerged"),
            "exclusion state after {scenario}"
        );
        assert_eq!(
            branch_exists(&fixture, "native"),
            !matches!(scenario, "apply" | "recent")
        );
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
        &[lane],
        &WorktreeGcPolicy {
            foreign_merged_clean: false,
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
        let path = fixture._temp.path().join("native-recheck");
        fixture.run(&[
            "worktree",
            "add",
            "-b",
            "native-recheck",
            path.to_str().ok_or("path")?,
        ])?;
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
                upstream: Some(fixture.head()?),
                terminal: &|_| false,
                no_live_lanes: &no_live,
                regenerable: &[],
                keep: &[],
                written_before_ns: i128::MAX,
                dry_run: false,
                foreign_merged_clean: true,
            },
            &cancel(),
        )?;
        assert!(changed.get());
        assert!(path.is_dir());
        assert!(report.worktrees.removed.is_empty());
        assert_eq!(
            report.worktrees.retained,
            vec![RetainedBranch {
                branch: "native-recheck".into(),
                reason: if change == "ignored" {
                    RetainReason::Dirty
                } else {
                    RetainReason::Changed
                },
                detail: None,
            }],
            "{report:?}"
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
        &[lane],
        &WorktreeGcPolicy {
            upstream: Some(fixture.head()?),
            terminal: &|_| false,
            no_live_lanes: &no_live_lanes,
            regenerable: &["target".into()],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: false,
            foreign_merged_clean: true,
        },
        &cancel(),
    )?;
    assert_eq!(report.worktrees.removed.len(), 1, "{report:?}");
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
    let calls = std::cell::Cell::new(0);
    let terminal = |path: &Path| {
        if path == first.path {
            calls.set(calls.get() + 1);
            false
        } else {
            true
        }
    };
    let report = collect(
        &context,
        &[first.clone(), second.clone()],
        &WorktreeGcPolicy {
            foreign_merged_clean: false,
            upstream: Some(fixture.head()?),
            terminal: &terminal,
            no_live_lanes: &|_| true,
            regenerable: &["target".to_owned()],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: false,
        },
        &cancel(),
    )?;
    assert!(
        calls.get() >= 2,
        "prune and removal both recheck terminality"
    );
    assert!(first.path.is_dir());
    assert!(branch_exists(&fixture, "gc-prune-failed"));
    assert_eq!(report.worktrees.retained.len(), 1, "{report:?}");
    assert_eq!(report.worktrees.retained[0].branch, "gc-prune-failed");
    assert_eq!(report.worktrees.removed.len(), 1);
    assert_eq!(report.worktrees.removed[0].path, second.path);
    assert_eq!(report.worktrees.pruned_refs, vec!["gc-prune-next"]);
    assert!(!second.path.exists());
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
            foreign_merged_clean: false,
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
fn gc_merged_tree_previews_and_removes_recent_declared_output() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fs::write(fixture.root.join(".gitignore"), "target/\n")?;
    fixture.run(&["add", ".gitignore"])?;
    fixture.run(&["commit", "-m", "ignore output"])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let mut lane = gc_merged_worktree(&fixture, &context, "gc-recent-merged")?;
    fs::create_dir(lane.path.join("target"))?;
    fs::write(lane.path.join("target/artifact"), "recent")?;
    lane.dirty = WorktreeCondition::Ignored;
    let settings = WorktreeGcPolicy {
        upstream: Some(fixture.head()?),
        terminal: &|_| true,
        no_live_lanes: &|_| true,
        regenerable: &["target".into()],
        keep: &[],
        written_before_ns: 0,
        dry_run: true,
        foreign_merged_clean: false,
    };
    let preview = collect(&context, &[lane.clone()], &settings, &cancel())?;
    assert_eq!(preview.worktrees.removed.len(), 1);
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
    assert_eq!(applied.worktrees.removed.len(), 1);
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
fn gc_managed_fallback_keeps_fresh_trees_at_the_tip_or_first_parent() -> Result<(), Box<dyn Error>>
{
    // Two ways a managed tree reaches the foreign fallback: no terminal lane,
    // or a writer reported unknown. Neither may bypass the managed protections.
    for entry in ["no-terminal-lane", "unknown-writer"] {
        for shape in ["at-tip", "first-parent"] {
            let fixture = Fixture::new()?;
            let test_context = TestContext::new(&fixture)?;
            let context = test_context.context();
            let name = format!("gc-fallback-{entry}-{shape}");
            let created = create(
                &context,
                WorktreeCreate {
                    branch: name.clone(),
                    base: None,
                },
                &cancel(),
            )?;
            if shape == "first-parent" {
                run_in_worktree(
                    &fixture,
                    &created.path,
                    &["commit", "--allow-empty", "-m", "lane"],
                )?;
                fixture.run(&["merge", "--ff-only", &name])?;
                fixture.run(&["commit", "--allow-empty", "-m", "main moves on"])?;
            }
            let mut lane = list(&context, &cancel())?
                .into_iter()
                .find(|lane| lane.path == created.path)
                .ok_or("missing lane")?;
            if entry == "unknown-writer" {
                lane.occupancy =
                    Occupancy::custody_unknown(lane.path.clone(), "fixture unknown writer");
            }
            let report = collect(
                &context,
                &[lane],
                &WorktreeGcPolicy {
                    foreign_merged_clean: true,
                    upstream: Some(fixture.head()?),
                    terminal: &|_| entry == "unknown-writer",
                    no_live_lanes: &|_| true,
                    regenerable: &[],
                    keep: &[],
                    written_before_ns: i128::MAX,
                    dry_run: false,
                },
                &cancel(),
            )?;
            let expected = if shape == "at-tip" {
                RetainReason::AtIntegrationTip
            } else {
                RetainReason::NoUniqueCommit
            };
            assert_eq!(
                report.worktrees.retained,
                vec![RetainedBranch {
                    branch: name.clone(),
                    reason: expected,
                    detail: None,
                }],
                "{entry} {shape}: {report:?}"
            );
            assert!(report.worktrees.removed.is_empty(), "{entry} {shape}");
            assert!(created.path.is_dir() && branch_exists(&fixture, &name));
        }
    }
    Ok(())
}

#[test]
fn gc_census_never_runs_under_the_repository_lock() -> Result<(), Box<dyn Error>> {
    for scenario in ["managed", "managed-preview", "foreign", "foreign-preview"] {
        let fixture = Fixture::new()?;
        let test_context = TestContext::new(&fixture)?;
        let mut context = test_context.context();
        let mut lane = gc_merged_worktree(&fixture, &context, "gc-census-lock")?;
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
            &[lane],
            &WorktreeGcPolicy {
                foreign_merged_clean: foreign,
                upstream: Some(fixture.head()?),
                terminal: &|_| !foreign,
                no_live_lanes: &|_| true,
                regenerable: &[],
                keep: &[],
                written_before_ns: i128::MAX,
                dry_run: scenario.ends_with("preview"),
            },
            &cancel(),
        )?;
        assert_eq!(report.worktrees.removed.len(), 1, "{scenario}: {report:?}");
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
