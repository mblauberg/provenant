use super::*;

#[test]
fn source_fast_forward_refuses_dirty_diverged_or_leased_tree() -> Result<(), Box<dyn Error>> {
    for scenario in ["dirty", "diverged", "leased"] {
        let fixture = Fixture::new()?;
        let test = TestContext::new(&fixture)?;
        let context = test.context();
        let info = create(
            &context,
            WorktreeCreate {
                branch: "source-writer".into(),
                base: None,
            },
            &cancel(),
        )?;
        fixture.run(&["commit", "--allow-empty", "-m", "combined source"])?;
        let target = fixture.head()?;
        let state = context
            .project_state
            .open_verified_directory(&state_dir_name(&info.path))?;
        let lease = if scenario == "leased" {
            Some(state.try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive)?)
        } else {
            None
        };
        if scenario == "dirty" {
            fs::write(info.path.join("keep"), "uncommitted")?;
        } else if scenario == "diverged" {
            run_in_worktree(
                &fixture,
                &info.path,
                &["commit", "--allow-empty", "-m", "diverged writer"],
            )?;
        }
        let before = context
            .facts
            .read_inventory(&context, &cancel())?
            .into_iter()
            .find(|entry| entry.path == info.path)
            .ok_or("missing writer")?
            .head;
        let error = fast_forward(&context, &info.path, target, &cancel())
            .err()
            .ok_or_else(|| format!("{scenario} source writer advanced"))?;
        match scenario {
            "dirty" => {
                assert!(matches!(error, WorktreeError::Dirty(ref paths) if paths.contains("keep")))
            }
            "diverged" => assert!(
                matches!(error, WorktreeError::SourceDiverged { current, target: requested } if current == before && requested == target)
            ),
            "leased" => assert!(matches!(error, WorktreeError::LockBusy)),
            _ => unreachable!(),
        }
        let after = context
            .facts
            .read_inventory(&context, &cancel())?
            .into_iter()
            .find(|entry| entry.path == info.path)
            .ok_or("missing writer")?
            .head;
        assert_eq!(after, before, "{scenario}");
        if scenario == "dirty" {
            assert_eq!(fs::read_to_string(info.path.join("keep"))?, "uncommitted");
        }
        drop(lease);
    }
    Ok(())
}

#[test]
fn source_fast_forward_preserves_ignored_files_that_become_tracked() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fs::write(fixture.root.join(".gitignore"), "cache\n")?;
    fixture.run(&["add", ".gitignore"])?;
    fixture.run(&["commit", "-m", "ignore local cache"])?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let info = create(
        &context,
        WorktreeCreate {
            branch: "source-writer".into(),
            base: None,
        },
        &cancel(),
    )?;
    fs::write(info.path.join("cache"), "keep ignored content")?;
    fs::write(fixture.root.join("cache"), "tracked source content")?;
    fixture.run(&["add", "-f", "cache"])?;
    fixture.run(&["commit", "-m", "track cache path"])?;
    let target = fixture.head()?;
    assert!(matches!(
        fast_forward(&context, &info.path, target, &cancel()),
        Err(WorktreeError::SourceAdvanceFailed { target: requested }) if requested == target
    ));
    let after = context
        .facts
        .read_inventory(&context, &cancel())?
        .into_iter()
        .find(|entry| entry.path == info.path)
        .ok_or("missing writer")?
        .head;
    assert_eq!(after, info.head);
    assert_eq!(
        fs::read_to_string(info.path.join("cache"))?,
        "keep ignored content"
    );
    Ok(())
}

#[test]
fn occupancy_keeps_scoped_census_evidence_and_held_lease_priority() -> Result<(), Box<dyn Error>> {
    let scope = Path::new("/fixture/worktree");
    assert!(matches!(
        combine_occupancy(scope, Occupancy::custody_unknown(scope.to_path_buf(), "fixture"), Occupancy::Observed { process: None }),
        Occupancy::Unknown { scope: actual, source: OccupancyUnknownSource::Custody, .. } if actual == scope
    ));
    assert_eq!(
        combine_occupancy(
            scope,
            Occupancy::Held,
            Occupancy::Observed { process: None }
        ),
        Occupancy::Held
    );
    assert!(matches!(
        combine_occupancy(scope, Occupancy::Available, Occupancy::Held),
        Occupancy::Unknown { scope: actual, source: OccupancyUnknownSource::Census, .. } if actual == scope
    ));
    struct Incomplete;
    struct Contended;
    impl WorkerObserver for Contended {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            roots
                .iter()
                .cloned()
                .map(|path| (path, Occupancy::Held))
                .collect()
        }
    }
    impl WorkerObserver for Incomplete {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            roots
                .iter()
                .cloned()
                .map(|path| {
                    (
                        path.clone(),
                        Occupancy::census_unknown(path, "fixture census unavailable"),
                    )
                })
                .collect()
        }
        fn observe_roots_with_details(&self, roots: &[PathBuf]) -> WorkerObservations {
            (
                self.observe_roots(roots),
                roots
                    .iter()
                    .cloned()
                    .map(|path| (path, "fixture census incomplete".into()))
                    .collect(),
            )
        }
    }
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let mut context = test.context();
    let info = create(
        &context,
        WorktreeCreate {
            branch: "occupancy-evidence".into(),
            base: None,
        },
        &cancel(),
    )?;
    context.workers = &Incomplete;
    let observation = list(&context, &cancel())?
        .into_iter()
        .find(|entry| entry.path == info.path)
        .ok_or("missing worktree")?;
    assert!(matches!(
        observation.occupancy,
        Occupancy::Unknown { scope, evidence, source: OccupancyUnknownSource::Census }
            if scope == info.path && evidence == "fixture census incomplete"
    ));
    let _held = cache_writer_guard(context.project_state, &info.path).map_err(|_| "lease")?;
    let observation = list(&context, &cancel())?
        .into_iter()
        .find(|entry| entry.path == info.path)
        .ok_or("missing worktree")?;
    assert_eq!(observation.occupancy, Occupancy::Held);
    drop(_held);
    context.workers = &Contended;
    let observation = list(&context, &cancel())?
        .into_iter()
        .find(|entry| entry.path == info.path)
        .ok_or("missing worktree")?;
    assert!(matches!(
        observation.occupancy,
        Occupancy::Unknown { scope, evidence, source: OccupancyUnknownSource::Census }
            if scope == info.path && evidence.contains("without a held writer lease")
    ));
    let tree = verify_worktree(&fixture.repo, &info.path)?;
    let owner = WorktreeOwner {
        attempt_id: None,
        operation_id: Some("occupancy-evidence".parse()?),
        branch: "occupancy-evidence".into(),
        base: info.head.to_string(),
        host: provenant_core::HostId::from_bytes([3; 16]),
        created_at: context.clock.wall_now()?,
        origin: WorktreeOrigin::Lane,
        label: None,
        reference: None,
    };
    save_lane_owner_when_free(&context, &tree, &owner, info.head, None, false, &cancel())?;
    assert_eq!(load_owner(&tree)?, Some(owner.clone()));
    let registered = list(&context, &cancel())?
        .into_iter()
        .find(|entry| entry.path == info.path)
        .ok_or("missing registered worktree")?;
    assert_eq!(
        registered.owner.as_ref().ok().and_then(Option::as_ref),
        Some(&owner)
    );
    assert_eq!(serde_json::to_value(&registered)?["owner"], "registered");
    let owner_path = tree.repo().private_git_dir().join("provenant-owner.json");
    let saved = fs::read(&owner_path)?;
    fs::write(&owner_path, b"invalid owner record")?;
    context.workers = &FreeWorkers;
    let observation = list(&context, &cancel())?
        .into_iter()
        .find(|entry| entry.path == info.path)
        .ok_or("missing worktree")?;
    assert!(matches!(observation.occupancy, Occupancy::Unknown { .. }));
    assert!(
        observation
            .owner
            .as_ref()
            .is_err_and(|evidence| evidence.contains("owner record could not be read"))
    );
    assert_eq!(serde_json::to_value(&observation)?["owner"], "unknown");
    let policy = WorktreeGcPolicy {
        upstream: Some(info.head),
        terminal: &|_| true,
        no_live_lanes: &|_| true,
        regenerable: &[],
        keep: &[],
        written_before_ns: i128::MAX,
        dry_run: false,
        foreign_merged_clean: false,
    };
    collect(
        &context,
        std::slice::from_ref(&observation),
        &policy,
        &cancel(),
    )?;
    assert!(info.path.exists());
    assert_eq!(fs::read(&owner_path)?, b"invalid owner record");
    assert!(matches!(
        observation.occupancy,
        Occupancy::Unknown { scope, evidence, source: OccupancyUnknownSource::Custody }
            if scope == info.path && evidence.contains("owner record could not be read")
    ));
    fs::write(&owner_path, saved)?;
    fixture.run(&["worktree", "lock", info.path.to_str().ok_or("path")?])?;
    let observation = list(&context, &cancel())?
        .into_iter()
        .find(|entry| entry.path == info.path)
        .ok_or("missing worktree")?;
    assert!(matches!(
        observation.occupancy,
        Occupancy::Unknown { scope, evidence, source: OccupancyUnknownSource::Custody }
            if scope == info.path && evidence.contains("locked")
    ));
    Ok(())
}

#[test]
fn held_writer_lease_takes_precedence_over_live_census() -> Result<(), Box<dyn Error>> {
    struct Live;
    impl WorkerObserver for Live {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            roots
                .iter()
                .cloned()
                .map(|path| (path, Occupancy::Observed { process: None }))
                .collect()
        }
    }
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let mut context = test.context();
    let info = create(
        &context,
        WorktreeCreate {
            branch: "lease-priority".into(),
            base: None,
        },
        &cancel(),
    )?;
    let tree = verify_worktree(&fixture.repo, &info.path)?;
    let attempt = provenant_core::AttemptId::from_bytes([42; 16]);
    let owner = WorktreeOwner {
        attempt_id: Some(attempt),
        operation_id: Some("lease-priority".parse()?),
        branch: "lease-priority".into(),
        base: info.head.to_string(),
        host: provenant_core::HostId::from_bytes([3; 16]),
        created_at: context.clock.wall_now()?,
        origin: WorktreeOrigin::Lane,
        label: None,
        reference: None,
    };
    save_owner(&tree, &owner)?;
    let _held = cache_writer_guard(context.project_state, &info.path).map_err(|_| "lease")?;
    context.workers = &Live;
    let listed = list(&context, &cancel())?
        .into_iter()
        .find(|entry| entry.path == info.path)
        .ok_or("missing worktree")?;
    assert_eq!(listed.occupancy, Occupancy::Held);
    assert_eq!(
        load_owner(&tree)?.and_then(|owner| owner.attempt_id),
        Some(attempt)
    );
    let next = WorktreeOwner {
        attempt_id: None,
        operation_id: Some("next-lease".parse()?),
        ..owner.clone()
    };
    assert!(matches!(
        save_lane_owner_when_free(
            &context,
            &tree,
            &next,
            info.head,
            Some(&owner),
            true,
            &cancel()
        ),
        Err(WorktreeError::LockBusy)
    ));
    drop(_held);
    assert!(matches!(
        save_lane_owner_when_free(
            &context,
            &tree,
            &next,
            info.head,
            Some(&owner),
            false,
            &cancel()
        ),
        Err(WorktreeError::WriterBusy { holder: Some(holder) }) if holder == attempt
    ));
    save_lane_owner_when_free(
        &context,
        &tree,
        &next,
        info.head,
        Some(&owner),
        true,
        &cancel(),
    )?;
    assert_eq!(load_owner(&tree)?, Some(next));
    Ok(())
}

#[test]
fn competing_initial_lane_claims_publish_only_one_owner() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let info = create(
        &context,
        WorktreeCreate {
            branch: "competing-claims".into(),
            base: None,
        },
        &cancel(),
    )?;
    let tree = verify_worktree(&fixture.repo, &info.path)?;
    let first = WorktreeOwner {
        attempt_id: None,
        operation_id: Some("competing-first".parse()?),
        branch: "competing-claims".into(),
        base: info.head.to_string(),
        host: provenant_core::HostId::from_bytes([3; 16]),
        created_at: context.clock.wall_now()?,
        origin: WorktreeOrigin::Lane,
        label: None,
        reference: None,
    };
    let second = WorktreeOwner {
        operation_id: Some("competing-second".parse()?),
        ..first.clone()
    };
    let barrier = std::sync::Barrier::new(2);
    let claim = |owner: &WorktreeOwner| {
        barrier.wait();
        save_lane_owner_when_free(&context, &tree, owner, info.head, None, false, &cancel())
    };
    let results = std::thread::scope(|scope| {
        let first_result = scope.spawn(|| claim(&first));
        let second_result = scope.spawn(|| claim(&second));
        Ok::<_, Box<dyn Error>>((
            first_result
                .join()
                .map_err(|_| "first claim thread panicked")?,
            second_result
                .join()
                .map_err(|_| "second claim thread panicked")?,
        ))
    })?;
    assert!(
        matches!(
            (&results.0, &results.1),
            (Ok(()), Err(WorktreeError::IdentityChanged))
                | (Err(WorktreeError::IdentityChanged), Ok(()))
        ),
        "both claims must not succeed: {results:?}"
    );
    let owner = load_owner(&tree)?.ok_or("missing winning owner")?;
    assert!(owner == first || owner == second);
    Ok(())
}

#[test]
fn stale_owner_update_cannot_overwrite_a_successor_claim() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let info = create(
        &context,
        WorktreeCreate {
            branch: "owner-update-race".into(),
            base: None,
        },
        &cancel(),
    )?;
    let tree = verify_worktree(&fixture.repo, &info.path)?;
    let first = WorktreeOwner {
        attempt_id: Some(provenant_core::AttemptId::from_bytes([4; 16])),
        operation_id: Some("first-operation".parse()?),
        branch: "owner-update-race".into(),
        base: info.head.to_string(),
        host: provenant_core::HostId::from_bytes([3; 16]),
        created_at: context.clock.wall_now()?,
        origin: WorktreeOrigin::Lane,
        label: None,
        reference: None,
    };
    save_lane_owner_when_free(&context, &tree, &first, info.head, None, false, &cancel())?;
    let successor = WorktreeOwner {
        attempt_id: None,
        operation_id: Some("successor-operation".parse()?),
        ..first.clone()
    };
    save_lane_owner_when_free(
        &context,
        &tree,
        &successor,
        info.head,
        Some(&first),
        true,
        &cancel(),
    )?;
    let stale_update = WorktreeOwner {
        attempt_id: Some(provenant_core::AttemptId::from_bytes([5; 16])),
        ..first.clone()
    };
    assert!(matches!(
        update_owner_when_unchanged(&context, &tree, &first, &stale_update, &cancel()),
        Err(WorktreeError::IdentityChanged)
    ));
    assert_eq!(load_owner(&tree)?, Some(successor.clone()));
    let current_update = WorktreeOwner {
        attempt_id: Some(provenant_core::AttemptId::from_bytes([6; 16])),
        ..successor.clone()
    };
    let held = cache_writer_guard(context.project_state, &info.path).map_err(|_| "lease")?;
    assert!(matches!(
        update_owner_when_unchanged(&context, &tree, &successor, &current_update, &cancel()),
        Err(WorktreeError::WriterBusy { holder: None })
    ));
    assert_eq!(load_owner(&tree)?, Some(successor.clone()));
    drop(held);
    update_owner_when_unchanged(&context, &tree, &successor, &current_update, &cancel())?;
    // A replay that already reached the exact target remains idempotent,
    // including after the guardian has acquired writer custody.
    let _held = cache_writer_guard(context.project_state, &info.path).map_err(|_| "lease")?;
    update_owner_when_unchanged(&context, &tree, &successor, &current_update, &cancel())?;
    assert_eq!(load_owner(&tree)?, Some(current_update));
    Ok(())
}

#[test]
fn lane_owner_reassignment_holds_custody_and_preserves_adopted_owners() -> Result<(), Box<dyn Error>>
{
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let info = create(
        &context,
        WorktreeCreate {
            branch: "lane-owner".into(),
            base: None,
        },
        &cancel(),
    )?;
    let tree = verify_worktree(&fixture.repo, &info.path)?;
    let mut owner = WorktreeOwner {
        attempt_id: Some(provenant_core::AttemptId::from_bytes([4; 16])),
        operation_id: Some("first-owner".parse()?),
        branch: "lane-owner".into(),
        base: info.head.to_string(),
        host: provenant_core::HostId::from_bytes([3; 16]),
        created_at: context.clock.wall_now()?,
        origin: WorktreeOrigin::Lane,
        label: None,
        reference: None,
    };
    save_lane_owner_when_free(&context, &tree, &owner, info.head, None, false, &cancel())?;
    let competing = WorktreeOwner {
        operation_id: Some("competing-owner".parse()?),
        ..owner.clone()
    };
    assert!(matches!(
        save_lane_owner_when_free(
            &context,
            &tree,
            &competing,
            info.head,
            None,
            false,
            &cancel()
        ),
        Err(WorktreeError::IdentityChanged)
    ));
    assert_eq!(load_owner(&tree)?, Some(owner.clone()));
    assert!(matches!(
        remove_created_by_operation(
            &context,
            "lane-owner".into(),
            info.head,
            &"competing-owner".parse()?,
            &cancel()
        ),
        Err(WorktreeError::IdentityChanged)
    ));
    assert!(info.path.exists());
    assert_eq!(load_owner(&tree)?, Some(owner.clone()));
    let state = context
        .project_state
        .open_verified_directory(&state_dir_name(&info.path))?;
    let held = state.lock(
        OsStr::new(WRITER_LOCK),
        LockMode::Exclusive,
        WaitBudget::nonblocking(context.clock.mono_now()),
        &cancel(),
        context.clock,
    )?;
    let next = WorktreeOwner {
        attempt_id: None,
        operation_id: Some("next-owner".parse()?),
        ..owner.clone()
    };
    assert!(matches!(
        save_lane_owner_when_free(
            &context,
            &tree,
            &next,
            info.head,
            Some(&owner),
            true,
            &cancel()
        ),
        Err(WorktreeError::LockBusy)
    ));
    assert_eq!(load_owner(&tree)?, Some(owner.clone()));
    drop(held);
    assert!(matches!(
        save_lane_owner_when_free(
            &context,
            &tree,
            &next,
            info.head,
            Some(&owner),
            false,
            &cancel()
        ),
        Err(WorktreeError::WriterBusy { holder }) if holder == owner.attempt_id
    ));
    assert_eq!(load_owner(&tree)?, Some(owner.clone()));
    let pending = WorktreeOwner {
        attempt_id: None,
        ..owner.clone()
    };
    save_owner(&tree, &pending)?;
    save_lane_owner_when_free(
        &context,
        &tree,
        &next,
        info.head,
        Some(&pending),
        false,
        &cancel(),
    )?;
    assert_eq!(load_owner(&tree)?, Some(next.clone()));
    save_owner(&tree, &owner)?;
    fs::write(info.path.join("dirty"), "keep")?;
    let error = save_lane_owner_when_free(
        &context,
        &tree,
        &next,
        info.head,
        Some(&owner),
        true,
        &cancel(),
    )
    .err()
    .ok_or("dirty worktree owner saved")?;
    assert!(matches!(&error, WorktreeError::Dirty(paths) if paths.contains("dirty")));
    assert!(error.to_string().contains("dirty"));
    fs::remove_file(info.path.join("dirty"))?;
    // Another writer moves the branch after the caller observed and validated
    // `info.head`: the stale observation must not be published.
    run_in_worktree(
        &fixture,
        &info.path,
        &["commit", "--allow-empty", "-m", "moved after observation"],
    )?;
    assert!(matches!(
        save_lane_owner_when_free(
            &context,
            &tree,
            &next,
            info.head,
            Some(&owner),
            true,
            &cancel()
        ),
        Err(WorktreeError::IdentityChanged)
    ));
    assert_eq!(load_owner(&tree)?, Some(owner.clone()));
    let head = list(&context, &cancel())?
        .into_iter()
        .find(|entry| entry.path == info.path)
        .ok_or("missing lane")?
        .head;
    assert_ne!(head, info.head);
    save_lane_owner_when_free(&context, &tree, &next, head, Some(&owner), true, &cancel())?;
    let stale = WorktreeOwner {
        operation_id: Some("stale-owner".parse()?),
        ..next.clone()
    };
    assert!(matches!(
        save_lane_owner_when_free(&context, &tree, &stale, head, Some(&owner), true, &cancel()),
        Err(WorktreeError::IdentityChanged)
    ));
    assert_eq!(load_owner(&tree)?, Some(next.clone()));
    owner.origin = WorktreeOrigin::Adopted;
    owner.label = Some("manual".into());
    save_owner(&tree, &owner)?;
    assert!(
        save_lane_owner_when_free(&context, &tree, &next, head, Some(&owner), false, &cancel())
            .is_err()
    );
    assert_eq!(load_owner(&tree)?, Some(owner));
    Ok(())
}

#[test]
fn ignored_paths_are_accepted_and_dirty_paths_are_named_in_error() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fs::write(
        fixture.root.join(".git/info/exclude"),
        "target/\n.provenant/\n",
    )?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let info = create(
        &context,
        WorktreeCreate {
            branch: "ignored-test".into(),
            base: None,
        },
        &cancel(),
    )?;
    let tree = verify_worktree(&fixture.repo, &info.path)?;
    let owner = WorktreeOwner {
        attempt_id: Some(provenant_core::AttemptId::from_bytes([7; 16])),
        operation_id: Some("op-ignored".parse()?),
        branch: "ignored-test".into(),
        base: info.head.to_string(),
        host: provenant_core::HostId::from_bytes([8; 16]),
        created_at: context.clock.wall_now()?,
        origin: WorktreeOrigin::Lane,
        label: None,
        reference: None,
    };

    // 1. Acceptance 1: A worktree containing only ignored paths (such as target/
    // or .provenant/scratch/graphs/) is accepted as clean.
    fs::create_dir_all(info.path.join("target/debug"))?;
    fs::write(info.path.join("target/debug/output.bin"), "binary")?;
    fs::create_dir_all(info.path.join(".provenant/scratch/graphs"))?;
    fs::write(
        info.path.join(".provenant/scratch/graphs/preset-1.json"),
        "{}",
    )?;
    save_lane_owner_when_free(&context, &tree, &owner, info.head, None, false, &cancel())?;
    assert_eq!(load_owner(&tree)?, Some(owner.clone()));

    // 2. Acceptance 2: A dirty worktree's error names its paths.
    let next_owner = WorktreeOwner {
        attempt_id: Some(provenant_core::AttemptId::from_bytes([9; 16])),
        operation_id: Some("op-dirty".parse()?),
        ..owner.clone()
    };
    fs::write(info.path.join("uncommitted.rs"), "fn uncommitted() {}")?;
    let error = save_lane_owner_when_free(
        &context,
        &tree,
        &next_owner,
        info.head,
        Some(&owner),
        true,
        &cancel(),
    )
    .err()
    .ok_or("dirty tree owner saved")?;
    match &error {
        WorktreeError::Dirty(paths) => {
            assert!(
                paths.contains("uncommitted.rs"),
                "expected path in dirty list: {paths}"
            );
        }
        other => panic!("expected WorktreeError::Dirty, got: {other}"),
    }
    let error_text = error.to_string();
    assert!(
        error_text.contains("worktree is dirty: uncommitted.rs"),
        "{error_text}"
    );
    let with_repair = error.with_repair();
    assert!(
        with_repair.contains("worktree is dirty: uncommitted.rs")
            && with_repair.contains("run `git -C PATH status`"),
        "{with_repair}"
    );

    // Verify up to 12 dirty paths listed and remaining counted.
    for n in 0..15 {
        fs::write(info.path.join(format!("dirty_{n:02}.txt")), "dirty")?;
    }
    let many_error = save_lane_owner_when_free(
        &context,
        &tree,
        &next_owner,
        info.head,
        Some(&owner),
        true,
        &cancel(),
    )
    .err()
    .ok_or("many dirty tree owner saved")?;
    match &many_error {
        WorktreeError::Dirty(paths) => {
            assert!(
                paths.contains("and 4 more"),
                "expected count of remaining paths: {paths}"
            );
        }
        other => panic!("expected WorktreeError::Dirty, got: {other}"),
    }

    Ok(())
}

#[test]
fn owned_cleanup_rechecks_regenerated_output_and_keep_paths() -> Result<(), Box<dyn Error>> {
    for keep in [false, true] {
        let fixture = Fixture::new()?;
        fs::write(fixture.root.join(".git/info/exclude"), "build/\nsaved/\n")?;
        let test = TestContext::new(&fixture)?;
        let context = test.context();
        let info = create(
            &context,
            WorktreeCreate {
                branch: "regenerated".into(),
                base: None,
            },
            &cancel(),
        )?;
        let attempt = provenant_core::AttemptId::from_bytes([7; 16]);
        let tree = verify_worktree(&fixture.repo, &info.path)?;
        save_owner(
            &tree,
            &WorktreeOwner {
                attempt_id: Some(attempt),
                operation_id: Some("regenerated-create".parse()?),
                branch: "regenerated".into(),
                base: info.head.to_string(),
                host: provenant_core::HostId::from_bytes([3; 16]),
                created_at: context.clock.wall_now()?,
                origin: WorktreeOrigin::Lane,
                label: None,
                reference: None,
            },
        )?;
        let output = info.path.join("saved/new");
        fs::create_dir_all(info.path.join("build"))?;
        fs::write(info.path.join("build/old"), "initial output")?;
        let calls = std::cell::Cell::new(0);
        let terminal = |_: &Path| {
            calls.set(calls.get() + 1);
            if calls.get() == 2 {
                // Recreate ignored output after collection, before final removal.
                assert!(!info.path.join("build/old").exists());
                assert!(fs::create_dir_all(output.parent().unwrap_or(&info.path)).is_ok());
                assert!(fs::write(&output, "regenerated output").is_ok());
            }
            true
        };
        let kept = if keep {
            vec!["saved/new".into()]
        } else {
            vec![]
        };
        let policy = WorktreeGcPolicy {
            upstream: Some(info.head),
            terminal: &terminal,
            no_live_lanes: &|_| true,
            regenerable: &["build".into(), "saved".into()],
            keep: &kept,
            written_before_ns: i128::MAX,
            foreign_merged_clean: false,
            dry_run: false,
        };
        let result = prune_owned(
            &context,
            &info.path,
            attempt,
            info.head,
            info.head,
            &policy,
            &cancel(),
        );
        assert_eq!(
            calls.get(),
            if keep { 2 } else { 3 },
            "terminality is rechecked after output preparation"
        );
        if keep {
            assert!(matches!(result, Err(WorktreeError::IgnoredPaths(_))));
            assert_eq!(fs::read_to_string(&output)?, "regenerated output");
        } else {
            assert_eq!(result?.removed.len(), 1);
            assert!(!info.path.exists());
        }
    }
    Ok(())
}

#[test]
fn owned_cleanup_refuses_dirty_unmerged_and_adopted_trees() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let base = fixture.head()?;
    let info = create(
        &context,
        WorktreeCreate {
            branch: "owned".into(),
            base: None,
        },
        &cancel(),
    )?;
    let tree = verify_worktree(&fixture.repo, &info.path)?;
    let attempt = provenant_core::AttemptId::from_bytes([7; 16]);
    let mut owner = WorktreeOwner {
        attempt_id: Some(attempt),
        operation_id: Some("owned-create".parse()?),
        branch: "owned".into(),
        base: base.to_string(),
        host: provenant_core::HostId::from_bytes([3; 16]),
        created_at: context.clock.wall_now()?,
        origin: WorktreeOrigin::Lane,
        label: None,
        reference: None,
    };
    save_owner(&tree, &owner)?;
    let policy = WorktreeGcPolicy {
        upstream: Some(base),
        terminal: &|_| true,
        no_live_lanes: &|_| true,
        regenerable: &[],
        keep: &[],
        written_before_ns: i128::MAX,
        foreign_merged_clean: false,
        dry_run: false,
    };
    fs::write(info.path.join("dirty"), "keep")?;
    assert!(
        prune_owned(
            &context,
            &info.path,
            attempt,
            base,
            base,
            &policy,
            &cancel()
        )
        .is_err()
    );
    fs::remove_file(info.path.join("dirty"))?;
    owner.origin = WorktreeOrigin::Adopted;
    save_owner(&tree, &owner)?;
    assert!(
        prune_owned(
            &context,
            &info.path,
            attempt,
            base,
            base,
            &policy,
            &cancel()
        )
        .is_err()
    );
    owner.origin = WorktreeOrigin::Lane;
    save_owner(&tree, &owner)?;
    let denied = WorktreeGcPolicy {
        no_live_lanes: &|_| false,
        ..policy
    };
    assert!(
        prune_owned(
            &context,
            &info.path,
            attempt,
            base,
            base,
            &denied,
            &cancel()
        )
        .is_err()
    );
    assert!(info.path.exists());
    fixture.run(&[
        "-C",
        info.path.to_str().ok_or("path")?,
        "commit",
        "--allow-empty",
        "-m",
        "unmerged lane commit",
    ])?;
    let head = list(&context, &cancel())?
        .into_iter()
        .find(|lane| lane.path == info.path)
        .ok_or("lane missing")?
        .head;
    assert!(
        prune_owned(
            &context,
            &info.path,
            attempt,
            head,
            base,
            &policy,
            &cancel()
        )
        .is_err()
    );
    let pushed_policy = WorktreeGcPolicy {
        upstream: None,
        ..policy
    };
    let removed = prune_owned(
        &context,
        &info.path,
        attempt,
        head,
        head,
        &pushed_policy,
        &cancel(),
    )?;
    assert_eq!(removed.removed.len(), 1);
    assert!(!info.path.exists());
    assert!(removed.pruned_refs.is_empty());
    assert!(
        removed
            .retained
            .iter()
            .any(|retained| retained.branch == "owned")
    );
    assert!(read_branch_tips(&context, &cancel())?.contains_key(&GitRefName::branch("owned")?));
    Ok(())
}

#[test]
fn transient_held_path_is_reobserved_but_persistent_holder_refuses() -> Result<(), Box<dyn Error>> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Holder {
        calls: AtomicUsize,
        persistent: bool,
    }
    impl WorkerObserver for Holder {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            roots
                .iter()
                .cloned()
                .map(|root| {
                    let state = if self.persistent || call < 4 {
                        Occupancy::Observed {
                            process: Some(ObservedProcess::new(
                                42,
                                Some("holder".into()),
                                ProcessEvidenceKind::OtherHeldPath,
                            )),
                        }
                    } else {
                        Occupancy::Available
                    };
                    (root, state)
                })
                .collect()
        }
    }
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let mut context = test.context();
    let info = create(
        &context,
        WorktreeCreate {
            branch: "held-path-retry".into(),
            base: None,
        },
        &cancel(),
    )?;
    let entries = context.facts.fresh_inventory(&context, &cancel())?;
    let entry = entries
        .iter()
        .find(|entry| entry.path == info.path)
        .ok_or("entry")?;
    let transient = Holder {
        calls: AtomicUsize::new(0),
        persistent: false,
    };
    context.workers = &transient;
    assert!(matches!(
        lock_worktree_for_mutation(&context, entry, &cancel()),
        Err(WorktreeError::LiveWriter | WorktreeError::LiveProcess(_))
    ));
    assert_eq!(transient.calls.load(Ordering::SeqCst), 1);
    transient.calls.store(0, Ordering::SeqCst);
    fast_forward(&context, &info.path, info.head, &cancel())?;
    assert_eq!(transient.calls.load(Ordering::SeqCst), 5);
    let persistent = Holder {
        calls: AtomicUsize::new(0),
        persistent: true,
    };
    context.workers = &persistent;
    context.lock_budget = WaitBudget::up_to(
        context.clock.mono_now(),
        provenant_core::DurationMicros::from_micros(350_000),
    );
    assert!(matches!(
        fast_forward(&context, &info.path, info.head, &cancel()),
        Err(WorktreeError::LiveWriter | WorktreeError::LiveProcess(_))
    ));
    assert!(persistent.calls.load(Ordering::SeqCst) >= 1);
    Ok(())
}

#[test]
fn dozen_writer_worktrees_claim_concurrently_without_registry_exclusion()
-> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let mut claims = Vec::new();
    for index in 0..12 {
        let branch = format!("parallel-writer-{index}");
        let info = create(
            &context,
            WorktreeCreate {
                branch: branch.clone(),
                base: Some(fixture.head()?.to_string()),
            },
            &cancel(),
        )?;
        claims.push((
            verify_worktree(&fixture.repo, &info.path)?,
            WorktreeOwner {
                attempt_id: None,
                operation_id: Some(format!("parallel-writer-{index}").parse()?),
                reference: None,
                branch,
                base: info.head.to_string(),
                host: provenant_core::HostId::from_bytes([3; 16]),
                created_at: context.clock.wall_now()?,
                origin: WorktreeOrigin::Lane,
                label: None,
            },
            info.head,
        ));
    }
    // A repository mutation on another tree must not delay these claims.
    let _registry = context
        .repo
        .common_dir()
        .try_lock_existing(OsStr::new(REPOSITORY_LOCK), LockMode::Exclusive)?;
    let barrier = std::sync::Barrier::new(claims.len());
    std::thread::scope(|scope| -> Result<(), Box<dyn Error>> {
        let handles: Vec<_> = claims
            .iter()
            .map(|(tree, owner, head)| {
                let context = &context;
                let barrier = &barrier;
                scope.spawn(move || {
                    let executor = provenant_os::exec::SystemExecutor::new(1).unwrap();
                    let facts = RepoFacts::default();
                    let local = WorktreeContext {
                        executor: &executor,
                        facts: &facts,
                        ..*context
                    };
                    barrier.wait();
                    save_lane_owner_when_free(&local, tree, owner, *head, None, false, &cancel())
                })
            })
            .collect();
        for handle in handles {
            handle.join().map_err(|_| "writer claim panicked")??;
        }
        Ok(())
    })?;
    for (tree, owner, _) in claims {
        assert_eq!(load_owner(&tree)?, Some(owner));
    }
    Ok(())
}

#[test]
fn writer_git_preparation_leaves_the_shared_registry_free() -> Result<(), Box<dyn Error>> {
    struct Probe<'a> {
        inner: &'a dyn Executor,
        registry: &'a Directory,
        calls: std::sync::atomic::AtomicUsize,
        slot: Directory,
        claiming: std::sync::atomic::AtomicBool,
    }
    impl Executor for Probe<'_> {
        fn execute(
            &self,
            options: provenant_os::exec::ExecOptions<'_>,
        ) -> Result<ExecOutcome, provenant_os::exec::ExecError> {
            drop(
                self.registry
                    .try_lock_existing(OsStr::new(REPOSITORY_LOCK), LockMode::Exclusive)
                    .expect("writer Git must leave the shared registry free"),
            );
            if self.claiming.load(std::sync::atomic::Ordering::SeqCst) {
                assert!(
                    matches!(
                        self.slot
                            .try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive),
                        Err(FsError::LockTimeout { .. })
                    ),
                    "ownership HEAD checks must hold writer custody"
                );
            }
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.inner.execute(options)
        }
    }
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let mut context = test.context();
    let info = create(
        &context,
        WorktreeCreate {
            branch: "writer-git-lock".into(),
            base: None,
        },
        &cancel(),
    )?;
    let tree = verify_worktree(&fixture.repo, &info.path)?;
    let probe = Probe {
        inner: context.executor,
        registry: context.repo.common_dir(),
        calls: Default::default(),
        slot: context
            .project_state
            .open_verified_directory(&state_dir_name(&info.path))?,
        claiming: std::sync::atomic::AtomicBool::new(true),
    };
    context.executor = &probe;
    let owner = WorktreeOwner {
        attempt_id: None,
        operation_id: Some("writer-git-lock".parse()?),
        reference: None,
        branch: "writer-git-lock".into(),
        base: info.head.to_string(),
        host: provenant_core::HostId::from_bytes([3; 16]),
        created_at: context.clock.wall_now()?,
        origin: WorktreeOrigin::Lane,
        label: None,
    };
    save_lane_owner_when_free(&context, &tree, &owner, info.head, None, false, &cancel())?;
    let updated = WorktreeOwner {
        label: Some("updated".into()),
        ..owner.clone()
    };
    update_owner_when_unchanged(&context, &tree, &owner, &updated, &cancel())?;
    probe
        .claiming
        .store(false, std::sync::atomic::Ordering::SeqCst);
    fixture.run(&["commit", "--allow-empty", "-m", "source"])?;
    fast_forward(&context, &info.path, fixture.head()?, &cancel())?;
    assert!(probe.calls.load(std::sync::atomic::Ordering::SeqCst) > 0);
    Ok(())
}
