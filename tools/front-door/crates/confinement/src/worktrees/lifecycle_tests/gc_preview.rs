use super::*;

#[test]
fn gc_lazy_inventory_still_refuses_a_dirty_candidate() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let lane = create(
        &context,
        WorktreeCreate {
            branch: "dirty-candidate".into(),
            base: None,
        },
        &cancel(),
    )?;
    fs::write(lane.path.join("untracked"), "keep")?;
    let lanes = list_for_gc(&context, &cancel())?;
    let report = collect_lazy(
        &context,
        &lanes,
        &WorktreeGcPolicy {
            upstream: None,
            terminal: &|_| true,
            no_live_lanes: &|_| true,
            regenerable: &[],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: true,
            foreign_merged_clean: false,
        },
        &cancel(),
    )?;
    assert!(
        report
            .worktrees
            .retained
            .iter()
            .any(|entry| entry.branch == "dirty-candidate" && entry.reason == RetainReason::Dirty)
    );
    assert!(lane.path.join("untracked").exists());
    Ok(())
}

#[test]
fn gc_lazy_candidate_is_retained_when_its_git_probe_fails() -> Result<(), Box<dyn Error>> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct RejectConfig<'a> {
        inner: &'a dyn Executor,
        rejected: AtomicUsize,
    }
    impl Executor for RejectConfig<'_> {
        fn execute(
            &self,
            options: provenant_os::exec::ExecOptions<'_>,
        ) -> Result<ExecOutcome, provenant_os::exec::ExecError> {
            if options.argv.iter().any(|arg| arg == "config") {
                self.rejected.fetch_add(1, Ordering::SeqCst);
                return Err(provenant_os::exec::ExecError::InvalidLimit);
            }
            self.inner.execute(options)
        }
    }
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let mut context = test.context();
    let lane = create(
        &context,
        WorktreeCreate {
            branch: "probe-failure".into(),
            base: None,
        },
        &cancel(),
    )?;
    let git = RejectConfig {
        inner: context.executor,
        rejected: AtomicUsize::new(0),
    };
    context.executor = &git;
    let observation = list(&context, &cancel())?
        .into_iter()
        .find(|entry| entry.path == lane.path)
        .ok_or("missing worktree")?;
    assert!(matches!(
        observation.occupancy,
        Occupancy::Unknown { scope, evidence, source: OccupancyUnknownSource::Custody }
            if scope == lane.path && evidence.contains("worktree verification failed:")
    ));
    let lanes = list_for_gc(&context, &cancel())?;
    let report = collect_lazy(
        &context,
        &lanes,
        &WorktreeGcPolicy {
            upstream: None,
            terminal: &|_| true,
            no_live_lanes: &|_| true,
            regenerable: &[],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: false,
            foreign_merged_clean: false,
        },
        &cancel(),
    )?;
    assert!(git.rejected.load(Ordering::SeqCst) > 0);
    assert!(lane.path.exists());
    assert!(report.worktrees.removed.is_empty());
    assert!(
        report
            .worktrees
            .retained
            .iter()
            .any(|entry| entry.branch == "probe-failure")
    );
    Ok(())
}

#[test]
fn gc_lazy_candidate_never_runs_nested_git_filters() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let lane = create(
        &context,
        WorktreeCreate {
            branch: "lazy-gitlink".into(),
            base: None,
        },
        &cancel(),
    )?;
    let marker = fixture._temp.path().join("lazy-filter-ran");
    let poison = add_poisoned_gitlink(&fixture, &lane.path, "sub", &marker)?;
    assert_raw_git_runs_poison_filter(&fixture, &poison)?;
    poison.arm()?;
    let lanes = list_for_gc(&context, &cancel())?;
    let report = collect_lazy(
        &context,
        &lanes,
        &WorktreeGcPolicy {
            upstream: None,
            terminal: &|_| true,
            no_live_lanes: &|_| true,
            regenerable: &[],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: true,
            foreign_merged_clean: false,
        },
        &cancel(),
    )?;
    assert!(!marker.exists());
    assert!(
        report
            .worktrees
            .retained
            .iter()
            .any(|entry| entry.branch == "lazy-gitlink" && entry.reason == RetainReason::Dirty)
    );
    assert!(lane.path.exists());
    Ok(())
}

#[test]
#[allow(clippy::disallowed_methods)] // Compare fixture mtimes to the filesystem clock.
fn gc_unknown_writer_only_prunes_idle_unheld_output() -> Result<(), Box<dyn Error>> {
    for scenario in [
        "preview",
        "idle",
        "aged",
        "unowned",
        "recent",
        "index",
        "live",
        "unknown",
        "no-output",
        "unconfigured",
    ] {
        let fixture = Fixture::new()?;
        fs::write(fixture.root.join(".gitignore"), "target/\n")?;
        fixture.run(&["add", ".gitignore"])?;
        fixture.run(&["commit", "-m", "ignore output"])?;
        let test = TestContext::new(&fixture)?;
        let mut context = test.context();
        let path = fixture._temp.path().join("foreign-output");
        fixture.run(&[
            "worktree",
            "add",
            "-b",
            "foreign-output",
            path.to_str().ok_or("path")?,
        ])?;
        fs::create_dir(path.join("target"))?;
        fs::write(path.join("target/artifact"), "output")?;
        fs::write(path.join("untracked"), "source")?;
        if scenario == "no-output" {
            fs::remove_dir_all(path.join("target"))?;
        }
        if matches!(scenario, "index" | "aged") {
            let old = std::time::SystemTime::now() - std::time::Duration::from_secs(86_400);
            let tree = verify_worktree(context.repo, &path)?;
            for item in [
                path.join("target/artifact"),
                path.join("target"),
                path.join("untracked"),
                path.join(".gitignore"),
                path.clone(),
                tree.repo().private_git_dir().join("HEAD"),
            ] {
                fs::File::open(item)?.set_times(fs::FileTimes::new().set_modified(old))?;
            }
            if scenario == "aged" {
                fs::File::open(tree.repo().private_git_dir().join("index"))?
                    .set_times(fs::FileTimes::new().set_modified(old))?;
            }
        }
        let mut lane = list(&context, &cancel())?
            .into_iter()
            .find(|lane| lane.path == path)
            .ok_or("missing tree")?;
        if scenario != "unowned" {
            lane.occupancy =
                Occupancy::custody_unknown(lane.path.clone(), "fixture unknown writer");
        } else {
            assert!(lane.is_unowned());
            assert!(lane.occupancy.is_available());
        }
        let live = NamedLive;
        let unknown = UnknownWorkers;
        if scenario == "live" {
            context.workers = &live;
        }
        if scenario == "unknown" {
            context.workers = &unknown;
        }
        let regenerable = ["target".into()];
        let report = collect(
            &context,
            &[lane],
            &WorktreeGcPolicy {
                upstream: None,
                terminal: &|_| false,
                no_live_lanes: &|_| true,
                regenerable: if scenario == "unconfigured" {
                    &[]
                } else {
                    &regenerable
                },
                keep: &[],
                written_before_ns: match scenario {
                    "recent" => 0,
                    "index" | "aged" => (std::time::SystemTime::now()
                        - std::time::Duration::from_secs(3600))
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_nanos() as i128,
                    _ => i128::MAX,
                },
                dry_run: matches!(scenario, "preview" | "no-output" | "unconfigured"),
                foreign_merged_clean: false,
            },
            &cancel(),
        )?;
        assert!(path.is_dir(), "{scenario}");
        assert_eq!(fs::read_to_string(path.join("untracked"))?, "source");
        assert_eq!(
            path.join("target/artifact").exists(),
            !matches!(scenario, "idle" | "aged" | "unowned" | "no-output"),
            "{scenario}"
        );
        if scenario == "unconfigured" {
            assert!(report.idle_output.iter().any(|entry| {
                entry.path == path && entry.reason == "no regenerable directories configured"
            }));
        }
        if scenario == "no-output" {
            assert!(report.idle_output.iter().any(|entry| {
                entry.path == path && entry.reason == "no eligible ignored output"
            }));
        }
        if scenario == "preview" {
            assert!(
                report.idle_output.iter().any(
                    |entry| entry.path == path.join("target") && entry.reason == "would_remove"
                )
            );
        }
        if matches!(scenario, "idle" | "aged" | "unowned") {
            assert!(
                report
                    .idle_output
                    .iter()
                    .any(|entry| entry.path == path.join("target") && entry.removed)
            );
        }
        if matches!(scenario, "recent" | "index" | "live" | "unknown") {
            assert!(report.idle_output.iter().all(|entry| !entry.removed));
        }
    }
    Ok(())
}

#[test]
fn gc_native_output_preview_creates_no_writer_state() -> Result<(), Box<dyn Error>> {
    for existing_slot in [false, true] {
        let fixture = Fixture::new()?;
        fs::write(fixture.root.join(".gitignore"), ".worktrees/\ntarget/\n")?;
        fixture.run(&["add", ".gitignore"])?;
        fixture.run(&["commit", "-m", "ignore build output"])?;
        let test = TestContext::new(&fixture)?;
        let context = test.context();
        let path = fixture.root.join(".worktrees/native");
        fixture.run(&[
            "worktree",
            "add",
            "-b",
            "native",
            path.to_str().ok_or("path")?,
        ])?;
        fs::create_dir(path.join("target"))?;
        fs::write(path.join("target/artifact"), "cached output")?;
        let slot = context
            .project_state
            .current_path()?
            .join(state_dir_name(&path));
        if existing_slot {
            writer_directory(context.project_state, &path)?;
        }
        assert!(
            cache_writer_probe(context.project_state, &path)
                .map_err(|_| "probe failed")?
                .is_none()
        );
        let lanes = list(&context, &cancel())?;
        let regenerable = ["target".into()];
        let policy = WorktreeGcPolicy {
            upstream: None,
            terminal: &|_| false,
            no_live_lanes: &|_| true,
            regenerable: &regenerable,
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: true,
            foreign_merged_clean: false,
        };
        let preview = collect(&context, &lanes, &policy, &cancel())?;
        assert_eq!(preview.idle_output.len(), 1);
        assert_eq!(preview.idle_output[0].reason, "would_remove");
        assert!(path.join("target/artifact").is_file());
        assert_eq!(slot.exists(), existing_slot);
        assert!(!slot.join(WRITER_LOCK).exists());
        let applied = collect(
            &context,
            &lanes,
            &WorktreeGcPolicy {
                dry_run: false,
                ..policy
            },
            &cancel(),
        )?;
        assert_eq!(applied.idle_output.len(), 1);
        assert!(!path.join("target").exists());
        assert!(slot.join(WRITER_LOCK).is_file());
        let held = cache_writer_guard(context.project_state, &path).map_err(|_| "guard failed")?;
        assert!(matches!(
            cache_writer_probe(context.project_state, &path),
            Err(Occupancy::Held)
        ));
        drop(held);
    }
    Ok(())
}

#[test]
fn finished_writer_record_does_not_block_its_worktree_shell() -> Result<(), Box<dyn Error>> {
    struct Shell;
    impl WorkerObserver for Shell {
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
    let path = fixture.root.join(".worktrees/finished-writer");
    fixture.run(&[
        "worktree",
        "add",
        "-b",
        "finished-writer",
        path.to_str().ok_or("path")?,
    ])?;
    let slot = writer_directory(context.project_state, &path)?;
    let previous = provenant_supervisor::custody::Scope {
        run: provenant_core::RunId::from_bytes([21; 16]),
        attempt: provenant_core::AttemptId::from_bytes([22; 16]),
        generation: 1,
    };
    let clock = context.clock;
    let acquire = |scope| {
        provenant_supervisor::fixture::acquire_writer(
            slot.try_clone()?,
            scope,
            WaitBudget::nonblocking(clock.mono_now()),
            &cancel(),
            clock,
        )
    };
    let first = acquire(previous)?;
    let tree = verify_worktree(&fixture.repo, &path)?;
    let prior = WorktreeOwner {
        attempt_id: Some(previous.attempt),
        operation_id: Some("previous-writer".parse()?),
        branch: "finished-writer".into(),
        base: fixture.head()?.to_string(),
        host: provenant_core::HostId::from_bytes([3; 16]),
        created_at: context.clock.wall_now()?,
        origin: WorktreeOrigin::Lane,
        label: None,
        reference: None,
    };
    save_owner(&tree, &prior)?;
    let record = slot
        .current_path()?
        .join(previous.attempt.to_string())
        .join("reservation.json");
    let bytes = fs::read(&record)?;
    drop(first);
    context.workers = &Shell;
    let lane = list(&context, &cancel())?
        .into_iter()
        .find(|lane| lane.path == path)
        .ok_or("lane")?;
    assert!(matches!(lane.occupancy, Occupancy::Observed { .. }));
    let owner = WorktreeOwner {
        attempt_id: None,
        operation_id: Some("finished-writer-claim".parse()?),
        branch: "finished-writer".into(),
        base: lane.head.to_string(),
        host: provenant_core::HostId::from_bytes([3; 16]),
        created_at: context.clock.wall_now()?,
        origin: WorktreeOrigin::Lane,
        label: None,
        reference: None,
    };
    save_lane_owner_when_free(
        &context,
        &tree,
        &owner,
        lane.head,
        Some(&prior),
        true,
        &cancel(),
    )?;
    let successor = provenant_supervisor::custody::Scope {
        run: provenant_core::RunId::from_bytes([23; 16]),
        attempt: provenant_core::AttemptId::from_bytes([24; 16]),
        generation: 1,
    };
    let _second = acquire(successor)?;
    assert_eq!(fs::read(record)?, bytes);
    Ok(())
}

#[test]
fn retained_import_fences_a_released_writer_slot() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let path = fixture.root.join(".worktrees/native");
    fixture.run(&[
        "worktree",
        "add",
        "-b",
        "native",
        path.to_str().ok_or("path")?,
    ])?;
    let slot = writer_directory(context.project_state, &path)?;
    let scope = provenant_supervisor::custody::Scope {
        run: provenant_core::RunId::from_bytes([11; 16]),
        attempt: provenant_core::AttemptId::from_bytes([12; 16]),
        generation: 1,
    };
    let lease = provenant_supervisor::fixture::acquire_writer(
        slot.try_clone()?,
        scope,
        WaitBudget::nonblocking(context.clock.mono_now()),
        &cancel(),
        context.clock,
    )?;
    let attempt = slot.current_path()?.join(scope.attempt.to_string());
    let reservation = fs::read(attempt.join("reservation.json"))?;
    fs::write(attempt.join("import-retained.json"), "retained")?;
    drop(lease);
    let listing = provenant_supervisor::custody::list_writer_reservations(context.project_state)?;
    assert!(listing.issues.is_empty());
    assert!(
        listing
            .reservations
            .iter()
            .any(|record| record.scope == scope)
    );

    for refusal in [
        cache_writer_probe(context.project_state, &path).map(|_| ()),
        cache_writer_guard(context.project_state, &path).map(|_| ()),
    ] {
        assert!(matches!(refusal,
            Err(Occupancy::Unknown { scope, evidence, source: OccupancyUnknownSource::Custody })
                if scope == path && evidence.contains("retained import fence")
        ));
    }
    let lane = list(&context, &cancel())?
        .into_iter()
        .find(|lane| lane.path == path)
        .ok_or("lane")?;
    assert!(matches!(&lane.occupancy,
        Occupancy::Unknown { scope, evidence, source: OccupancyUnknownSource::Custody }
            if scope == &path && evidence.contains("retained import fence")
    ));
    fs::write(path.join("uncommitted"), "preserve unfinished work")?;
    let tree = verify_worktree(&fixture.repo, &path)?;
    let new_owner = WorktreeOwner {
        attempt_id: None,
        operation_id: Some("retained-import-claim".parse()?),
        branch: "native".into(),
        base: lane.head.to_string(),
        host: provenant_core::HostId::from_bytes([3; 16]),
        created_at: context.clock.wall_now()?,
        origin: WorktreeOrigin::Lane,
        label: None,
        reference: None,
    };
    assert!(matches!(
        save_lane_owner_when_free(
            &context,
            &tree,
            &new_owner,
            lane.head,
            None,
            false,
            &cancel()
        ),
        Err(WorktreeError::UnknownOccupancy { path: scope, detail })
            if scope == path && detail.contains("retained import fence")
    ));
    assert!(load_owner(&tree)?.is_none());
    assert_eq!(fs::read(attempt.join("reservation.json"))?, reservation);
    assert_eq!(fs::read(attempt.join("import-retained.json"))?, b"retained");
    let entries = context.facts.read_inventory(&context, &cancel())?;
    let entry = entries
        .iter()
        .find(|entry| entry.path == path)
        .ok_or("entry")?;
    for preview in [false, true] {
        assert!(matches!(
            lock_worktree(&context, entry, &cancel(), preview),
            Err(WorktreeError::UnknownOccupancy { path: scope, detail })
                if scope == path && detail.contains("retained import fence")
        ));
    }
    fs::create_dir(path.join("target"))?;
    fs::write(path.join("target/artifact"), "retained")?;
    let lanes = list(&context, &cancel())?;
    let regenerable = ["target".into()];
    let policy = WorktreeGcPolicy {
        upstream: None,
        terminal: &|_| true,
        no_live_lanes: &|_| true,
        regenerable: &regenerable,
        keep: &[],
        written_before_ns: i128::MAX,
        dry_run: false,
        foreign_merged_clean: false,
    };
    collect(&context, &lanes, &policy, &cancel())?;
    assert!(path.join("target/artifact").is_file());
    assert_eq!(
        fs::read_to_string(path.join("uncommitted"))?,
        "preserve unfinished work"
    );
    assert_eq!(fs::read(attempt.join("reservation.json"))?, reservation);
    assert_eq!(fs::read(attempt.join("import-retained.json"))?, b"retained");

    // Settlement can retain historical evidence after clearing the reservation.
    fs::remove_file(attempt.join("reservation.json"))?;
    assert!(cache_writer_guard(context.project_state, &path).is_ok());
    Ok(())
}

#[test]
fn git_inventory_accepts_unregistered_and_recreated_worktrees() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let path = fixture.root.join(".worktrees/native");
    fixture.run(&[
        "worktree",
        "add",
        "-b",
        "native",
        path.to_str().ok_or("path")?,
    ])?;
    let name = state_dir_name(&path);
    assert!(
        context
            .project_state
            .open_verified_directory(&name)
            .is_err()
    );
    let inventory = || {
        Ok(
            run_git(&context, &fixture.repo, GitRead::WorktreeList, &cancel())?
                .stdout
                .bytes,
        )
    };
    let bind = |branch: &str| -> Result<_, WorktreeError> {
        let tree = verify_worktree(&fixture.repo, &path)?;
        writer_custody(
            context.project_state,
            &tree,
            tree.path(),
            &GitRefName::branch(branch).map_err(|_| WorktreeError::InvalidRequest)?,
            fixture.head().map_err(|_| WorktreeError::InvalidOutput)?,
            inventory,
        )
    };
    assert_eq!(
        list(&context, &cancel())?
            .iter()
            .find(|tree| tree.path == path)
            .ok_or("tree")?
            .occupancy,
        Occupancy::Available
    );
    assert!(
        context
            .project_state
            .open_verified_directory(&name)
            .is_err(),
        "listing creates no state"
    );
    let (state, _) = bind("native")?;
    // Readers locate custody from the persisted path alone. One that now
    // resolves elsewhere, such as a checkout moved behind a symlink, would
    // reserve a slot no reader looks at.
    let moved = fixture._temp.path().join("moved-root");
    std::os::unix::fs::symlink(&fixture.root, &moved)?;
    let alias = moved.join(".worktrees/native");
    let tree = verify_worktree(&fixture.repo, &alias)?;
    assert!(matches!(
        writer_custody(
            context.project_state,
            &tree,
            &alias,
            &GitRefName::branch("native").map_err(|_| "branch")?,
            fixture.head()?,
            inventory,
        ),
        Err(WorktreeError::IdentityChanged)
    ));
    drop(tree);
    assert!(
        state
            .metadata_at(OsStr::new("identity.json"), false)
            .is_err()
    );
    assert!(matches!(bind("other"), Err(WorktreeError::IdentityChanged)));
    // Legacy identity and registry reservations are never membership gates.
    fs::write(
        state.current_path()?.join("identity.json"),
        b"invalid old schema and inode",
    )?;
    fs::write(
        state.current_path()?.join("reservation.json"),
        b"legacy registry data",
    )?;
    fixture.run(&["worktree", "remove", path.to_str().ok_or("path")?])?;
    fixture.run(&[
        "worktree",
        "add",
        "-b",
        "recreated",
        path.to_str().ok_or("path")?,
    ])?;
    bind("recreated")?;
    let outcome = remove(
        &context,
        WorktreeRemove::Name {
            name: "recreated".into(),
            discard: false,
            integration: None,
            expected_head: None,
        },
        &cancel(),
    )?;
    assert_eq!(outcome.removed.len(), 1);
    // Reusing a removed path has no tombstone/schema/branch refusal.
    create(
        &context,
        WorktreeCreate {
            branch: "native".into(),
            base: None,
        },
        &cancel(),
    )?;
    Ok(())
}

#[test]
fn writer_lock_busy_is_distinct_from_observed_live_process() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let info = create(
        &context,
        WorktreeCreate {
            branch: "busy".into(),
            base: None,
        },
        &cancel(),
    )?;
    let state = writer_directory(context.project_state, &info.path)?;
    let before = state.metadata_at(OsStr::new(WRITER_LOCK), false)?;
    let held = state.try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive)?;
    let request = || WorktreeRemove::Name {
        name: "busy".into(),
        discard: false,
        integration: None,
        expected_head: None,
    };
    let error = remove(&context, request(), &cancel())
        .err()
        .ok_or("removed busy tree")?;
    assert!(matches!(error, WorktreeError::LockBusy));
    assert_eq!(
        list(&context, &cancel())?
            .iter()
            .find(|tree| tree.path == info.path)
            .ok_or("tree")?
            .occupancy,
        Occupancy::Held
    );
    let report = collect(
        &context,
        &list(&context, &cancel())?,
        &WorktreeGcPolicy {
            upstream: Some(fixture.head()?),
            terminal: &|_| true,
            no_live_lanes: &|_| true,
            regenerable: &[],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: true,
            foreign_merged_clean: false,
        },
        &cancel(),
    )?;
    assert!(
        report
            .worktrees
            .retained
            .iter()
            .any(|tree| tree.reason == RetainReason::LockBusy)
    );
    drop(held);
    let live = WorktreeContext {
        workers: &NamedLive,
        ..test.context()
    };
    assert!(matches!(
        remove(&live, request(), &cancel()),
        Err(WorktreeError::LiveProcess(_))
    ));
    remove(&context, request(), &cancel())?;
    assert!(before.same_identity(&state.metadata_at(OsStr::new(WRITER_LOCK), false)?));
    Ok(())
}

#[test]
fn foreign_gc_removes_recent_declared_output() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fs::write(fixture.root.join(".gitignore"), "target/\n")?;
    fixture.run(&["add", ".gitignore"])?;
    fixture.run(&["commit", "-m", "ignore output"])?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let path = fixture._temp.path().join("foreign");
    fixture.run(&[
        "worktree",
        "add",
        "-b",
        "foreign",
        path.to_str().ok_or("path")?,
    ])?;
    fs::create_dir(path.join("target"))?;
    fs::write(path.join("target/artifact"), "recent output")?;
    let lanes = list(&context, &cancel())?;
    let report = collect(
        &context,
        &lanes,
        &WorktreeGcPolicy {
            upstream: Some(fixture.head()?),
            terminal: &|_| false,
            no_live_lanes: &|_| true,
            regenerable: &["target".into()],
            keep: &[],
            written_before_ns: 0,
            dry_run: false,
            foreign_merged_clean: true,
        },
        &cancel(),
    )?;
    assert_eq!(report.worktrees.removed.len(), 1);
    assert!(!path.exists());
    assert!(!branch_exists(&fixture, "foreign"));
    Ok(())
}

#[test]
fn source_fast_forward_advances_clean_tree_and_preserves_ignored_output()
-> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fs::write(fixture.root.join(".gitignore"), "target/\n")?;
    fixture.run(&["add", ".gitignore"])?;
    fixture.run(&["commit", "-m", "ignore build output"])?;
    let test = TestContext::new(&fixture)?;
    let mut context = test.context();
    let info = create(
        &context,
        WorktreeCreate {
            branch: "source-writer".into(),
            base: None,
        },
        &cancel(),
    )?;
    fs::create_dir(info.path.join("target"))?;
    fs::write(info.path.join("target/cache"), "retained output")?;
    fs::write(fixture.root.join("source-file"), "pinned source")?;
    fixture.run(&["add", "source-file"])?;
    fixture.run(&["commit", "-m", "combined source"])?;
    let target = fixture.head()?;
    let hooks = fixture.repo.common_git_dir().join("hooks");
    fs::create_dir_all(&hooks)?;
    fs::write(hooks.join("post-merge"), "#!/bin/sh\n: > hook-ran\n")?;
    fs::set_permissions(hooks.join("post-merge"), fs::Permissions::from_mode(0o700))?;
    let session = CensusSession::new(&test.workers, &test.clock);
    context.workers = &session;
    fast_forward(&context, &info.path, target, &cancel())?;
    assert_eq!(
        session.observed_at(),
        None,
        "mutation must invalidate the custody census"
    );
    let actual = list(&context, &cancel())?
        .into_iter()
        .find(|entry| entry.path == info.path)
        .ok_or("missing source writer")?;
    assert_eq!(actual.head, target);
    assert_eq!(actual.dirty, WorktreeCondition::Ignored);
    assert_eq!(
        fs::read_to_string(info.path.join("source-file"))?,
        "pinned source"
    );
    assert_eq!(
        fs::read_to_string(info.path.join("target/cache"))?,
        "retained output"
    );
    assert!(!info.path.join("hook-ran").exists());
    fast_forward(&context, &info.path, target, &cancel())?;
    Ok(())
}

#[test]
fn batch_worktree_on_a_private_branch_follows_an_integration_ref_by_fast_forward()
-> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let start = fixture.head()?;
    fixture.run(&["branch", "batch/graph"])?;
    let batch = create(
        &context,
        WorktreeCreate {
            branch: "batch/work".into(),
            base: None,
        },
        &cancel(),
    )?;
    let lane = create(
        &context,
        WorktreeCreate {
            branch: "lane".into(),
            base: None,
        },
        &cancel(),
    )?;
    run_in_worktree(
        &fixture,
        &lane.path,
        &["commit", "--allow-empty", "-m", "lane work"],
    )?;
    let entry = |path: &Path| {
        list(&context, &cancel())?
            .into_iter()
            .find(|entry| entry.path == path)
            .ok_or_else(|| Box::<dyn Error>::from("missing worktree"))
    };
    let lane_head = entry(&lane.path)?.head;
    // Delivery advances the integration ref without touching any checkout.
    fixture.run(&[
        "update-ref",
        "refs/heads/batch/graph",
        &lane_head.to_string(),
        &start.to_string(),
    ])?;
    let before = entry(&batch.path)?;
    assert_eq!(before.head, start);
    assert_eq!(before.dirty, WorktreeCondition::Clean);
    fast_forward(&context, &batch.path, lane_head, &cancel())?;
    assert!(
        context.facts.take_inventory(&context).is_none(),
        "a Git mutation must not leave the earlier inventory snapshot reusable"
    );
    let after = entry(&batch.path)?;
    assert_eq!(after.head, lane_head);
    assert_eq!(after.dirty, WorktreeCondition::Clean);
    assert_eq!(after.branch.as_deref(), Some("refs/heads/batch/work"));
    Ok(())
}

#[test]
fn fallback_writer_after_a_partial_commit_continues_past_the_pin() -> Result<(), Box<dyn Error>> {
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
    let pin = fixture.head()?;
    fast_forward(&context, &info.path, pin, &cancel())?;
    run_in_worktree(
        &fixture,
        &info.path,
        &["commit", "--allow-empty", "-m", "partial work"],
    )?;
    let ahead = context
        .facts
        .read_inventory(&context, &cancel())?
        .into_iter()
        .find(|entry| entry.path == info.path)
        .ok_or("missing writer")?
        .head;
    assert_ne!(ahead, pin);
    fast_forward(&context, &info.path, pin, &cancel())?;
    let after = context
        .facts
        .read_inventory(&context, &cancel())?
        .into_iter()
        .find(|entry| entry.path == info.path)
        .ok_or("missing writer")?
        .head;
    assert_eq!(after, ahead);
    Ok(())
}
