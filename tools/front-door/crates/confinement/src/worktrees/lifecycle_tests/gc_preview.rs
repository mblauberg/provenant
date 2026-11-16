use super::*;

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
    let lane = gc_owned_lane(&test, &context, "probe-failure")?;
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
    let report = collect(
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
    assert!(git.rejected.load(Ordering::SeqCst) > 0);
    assert!(lane.path.exists());
    assert!(report.worktrees.removed.is_empty());
    // An unverifiable lane has no readable owner: it is kept, never removed.
    assert!(
        report
            .candidates
            .iter()
            .any(|row| row.path == lane.path && row.action == "kept")
    );
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum GcVerdict {
    Removed,
    Retained(RetainReason),
    Absent,
}

fn gc_verdict(report: &WorktreeGcReport, lane: &WorktreeInfo) -> GcVerdict {
    if report
        .worktrees
        .removed
        .iter()
        .any(|entry| entry.path == lane.path)
    {
        return GcVerdict::Removed;
    }
    let branch = lane
        .branch
        .as_deref()
        .and_then(|name| name.strip_prefix("refs/heads/"))
        .unwrap_or_default();
    report
        .worktrees
        .retained
        .iter()
        .find(|entry| entry.branch == branch)
        .map_or(GcVerdict::Absent, |entry| GcVerdict::Retained(entry.reason))
}

/// Preview and apply reach one verdict for every worktree state, and neither
/// runs a nested repository's Git filters.
#[test]
fn gc_preview_verdict_matches_apply_for_every_worktree_state() -> Result<(), Box<dyn Error>> {
    for state in ["clean", "untracked", "poisoned-gitlink"] {
        let fixture = Fixture::new()?;
        let test = TestContext::new(&fixture)?;
        let context = test.context();
        let lane = gc_owned_lane(&test, &context, &format!("verdict-{state}"))?;
        let marker = fixture._temp.path().join("verdict-filter-ran");
        let poison = match state {
            "untracked" => {
                fs::write(lane.path.join("untracked"), "keep")?;
                None
            }
            "poisoned-gitlink" => {
                let poison = add_poisoned_gitlink(&fixture, &lane.path, "sub", &marker)?;
                assert_raw_git_runs_poison_filter(&fixture, &poison)?;
                Some(poison)
            }
            _ => None,
        };
        let retirement = HostRetirementFixture::new(&test)?;
        retirement.seed_lane(&context, &lane)?;
        let run = |dry_run: bool| -> Result<WorktreeGcReport, Box<dyn Error>> {
            if let Some(poison) = &poison {
                poison.arm()?;
            }
            let lanes = list_for_gc(&context, &cancel())?;
            Ok(collect(
                &context,
                &lanes,
                &WorktreeGcPolicy {
                    upstream: None,
                    terminal: &|_| true,
                    no_live_lanes: &|_| true,
                    regenerable: &[],
                    keep: &[],
                    written_before_ns: i128::MAX,
                    dry_run,
                    include_foreign: false,
                    retirement_port: Some(&retirement),
                    retirement_feeds: Some(&retirement),
                    mode: CollectionMode::Worktrees,
                    proof: None,
                    upstream_reference: None,
                    integration_patterns: &[],
                    excluded_paths: &[],
                },
                &cancel(),
            )?)
        };
        let preview = gc_verdict(&run(true)?, &lane);
        assert!(lane.path.exists(), "{state}: a preview deleted the tree");
        let applied = gc_verdict(&run(false)?, &lane);
        assert_eq!(preview, applied, "{state}: preview and apply disagree");
        assert_ne!(preview, GcVerdict::Absent, "{state}: no verdict reported");
        if state != "clean" {
            assert!(
                matches!(preview, GcVerdict::Retained(_)),
                "{state}: {preview:?}"
            );
            assert!(lane.path.exists(), "{state}: retained tree was removed");
        }
        assert!(!marker.exists(), "{state}: a nested Git filter ran");
    }
    Ok(())
}

#[test]
fn gc_unknown_writer_only_prunes_idle_unheld_output() -> Result<(), Box<dyn Error>> {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Case {
        Preview,
        Idle,
        Aged,
        Unowned,
        Recent,
        Live,
        Unknown,
        NoOutput,
        Unconfigured,
    }
    for case in [
        Case::Preview,
        Case::Idle,
        Case::Aged,
        Case::Unowned,
        Case::Recent,
        Case::Live,
        Case::Unknown,
        Case::NoOutput,
        Case::Unconfigured,
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
        if case == Case::NoOutput {
            fs::remove_dir_all(path.join("target"))?;
        }
        if case == Case::Aged {
            let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1);
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
            fs::File::open(tree.repo().private_git_dir().join("index"))?
                .set_times(fs::FileTimes::new().set_modified(old))?;
        }
        let mut lane = list(&context, &cancel())?
            .into_iter()
            .find(|lane| lane.path == path)
            .ok_or("missing tree")?;
        if case != Case::Unowned {
            lane.occupancy =
                Occupancy::custody_unknown(lane.path.clone(), "fixture unknown writer");
        } else {
            assert!(lane.is_unowned());
            assert!(lane.occupancy.is_available());
        }
        let live = NamedLive;
        let unknown = UnknownWorkers;
        if case == Case::Live {
            context.workers = &live;
        }
        if case == Case::Unknown {
            context.workers = &unknown;
        }
        let retirement = HostRetirementFixture::new(&test)?;
        let regenerable = ["target".into()];
        let report = collect(
            &context,
            &[lane],
            &WorktreeGcPolicy {
                upstream: None,
                terminal: &|_| false,
                no_live_lanes: &|_| true,
                regenerable: if case == Case::Unconfigured {
                    &[]
                } else {
                    &regenerable
                },
                keep: &[],
                written_before_ns: match case {
                    Case::Recent => 0,
                    Case::Aged => 2_000_000_000,
                    _ => i128::MAX,
                },
                dry_run: matches!(case, Case::Preview | Case::NoOutput | Case::Unconfigured),
                include_foreign: true,
                retirement_port: Some(&retirement),
                retirement_feeds: Some(&retirement),
                mode: CollectionMode::Worktrees,
                proof: None,
                upstream_reference: None,
                integration_patterns: &[],
                excluded_paths: &[],
            },
            &cancel(),
        )?;
        assert!(path.is_dir(), "{case:?}");
        assert_eq!(fs::read_to_string(path.join("untracked"))?, "source");
        assert_eq!(
            path.join("target/artifact").exists(),
            !matches!(
                case,
                Case::Idle | Case::Aged | Case::Unowned | Case::NoOutput
            ),
            "{case:?}"
        );
        if case == Case::Preview {
            assert!(
                report
                    .idle_output
                    .iter()
                    .any(|entry| entry.path == path.join("target") && !entry.removed)
            );
        }
        if matches!(case, Case::Idle | Case::Aged | Case::Unowned) {
            assert!(
                report
                    .idle_output
                    .iter()
                    .any(|entry| entry.path == path.join("target") && entry.removed)
            );
        }
        if matches!(case, Case::Recent | Case::Live | Case::Unknown) {
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
        let retirement = HostRetirementFixture::new(&test)?;
        let regenerable = ["target".into()];
        let policy = WorktreeGcPolicy {
            upstream: None,
            terminal: &|_| false,
            no_live_lanes: &|_| true,
            regenerable: &regenerable,
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: true,
            include_foreign: true,
            retirement_port: Some(&retirement),
            retirement_feeds: Some(&retirement),
            mode: CollectionMode::Worktrees,
            proof: None,
            upstream_reference: None,
            integration_patterns: &[],
            excluded_paths: &[],
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
    let scope = provenant_records::custody::Scope {
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
            Err(Occupancy::Unknown { scope, source: OccupancyUnknownSource::Custody, .. })
                if scope == path
        ));
    }
    let lane = list(&context, &cancel())?
        .into_iter()
        .find(|lane| lane.path == path)
        .ok_or("lane")?;
    assert!(matches!(&lane.occupancy,
        Occupancy::Unknown { scope, source: OccupancyUnknownSource::Custody, .. }
            if scope == &path
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
        include_foreign: false,
        retirement_port: None,
        retirement_feeds: None,
        mode: CollectionMode::Worktrees,
        proof: None,
        upstream_reference: None,
        integration_patterns: &[],
        excluded_paths: &[],
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
fn unsettled_reservation_fences_a_slot_after_its_lock_disappears() -> Result<(), Box<dyn Error>> {
    // Spec: docs/specs/supervisor.md#one-writer-removal-never-forces
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let path = fixture.root.join(".worktrees/unsettled");
    let slot = writer_directory(context.project_state, &path)?;
    let scope = provenant_records::custody::Scope {
        attempt: provenant_core::AttemptId::from_bytes([76; 16]),
        generation: 1,
    };
    let lease = provenant_supervisor::fixture::acquire_writer(
        slot.try_clone()?,
        scope,
        WaitBudget::nonblocking(context.clock.mono_now()),
        &cancel(),
        context.clock,
    )?;
    let reservation = slot
        .current_path()?
        .join(scope.attempt.to_string())
        .join("reservation.json");
    let evidence = fs::read(&reservation)?;
    assert_eq!(
        disk_writer_state(context.project_state, &path),
        Occupancy::Held,
        "a live kernel lease remains positively busy"
    );
    drop(lease);
    fs::remove_file(slot.current_path()?.join(WRITER_LOCK))?;

    assert!(matches!(
        disk_writer_state(context.project_state, &path),
        Occupancy::Unknown { evidence, .. }
            if evidence.contains(&scope.attempt.to_string())
                && evidence.contains("provenant reconcile")
    ));
    for result in [
        cache_writer_probe(context.project_state, &path).map(|_| ()),
        cache_writer_guard(context.project_state, &path).map(|_| ()),
    ] {
        assert!(matches!(
            result,
            Err(Occupancy::Unknown { evidence, .. })
                if evidence.contains(&scope.attempt.to_string())
                    && evidence.contains("provenant reconcile")
        ));
    }
    assert_eq!(fs::read(reservation)?, evidence);
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
    // Unrecognised identity and reservation files are never membership gates.
    fs::write(
        state.current_path()?.join("identity.json"),
        b"invalid identity data",
    )?;
    fs::write(
        state.current_path()?.join("reservation.json"),
        b"unrecognised reservation data",
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
    let outcome = remove_with_port(
        &test,
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
    let info = gc_owned_lane(&test, &context, "busy")?;
    let state = writer_directory(context.project_state, &info.path)?;
    let before = state.metadata_at(OsStr::new(WRITER_LOCK), false)?;
    let held = state.try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive)?;
    let request = || WorktreeRemove::Name {
        name: "busy".into(),
        discard: false,
        integration: None,
        expected_head: None,
    };
    let error = remove_with_port(&test, &context, request(), &cancel())
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
        remove_with_port(&test, &live, request(), &cancel()),
        Err(WorktreeError::LiveProcess(_))
    ));
    remove_with_port(&test, &context, request(), &cancel())?;
    assert!(before.same_identity(&state.metadata_at(OsStr::new(WRITER_LOCK), false)?));
    Ok(())
}

#[test]
fn foreign_gc_retires_a_landed_unowned_worktree_with_a_typed_outcome() -> Result<(), Box<dyn Error>>
{
    let fixture = Fixture::new()?;
    fs::write(fixture.root.join(".gitignore"), "target/\n")?;
    fixture.run(&["add", ".gitignore"])?;
    fixture.run(&["commit", "-m", "ignore output"])?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let retirement = HostRetirementFixture::new(&test)?;
    let upstream = fixture.head()?;
    let evidence = gc_landing_evidence(&fixture, upstream)?;
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
    let canonical = std::fs::canonicalize(&path)?;
    let lanes = list(&context, &cancel())?;
    let report = collect(
        &context,
        &lanes,
        &WorktreeGcPolicy {
            upstream: Some(upstream),
            terminal: &|_| false,
            no_live_lanes: &|_| true,
            regenerable: &["target".into()],
            keep: &[],
            written_before_ns: 0,
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
    // A landed foreign lane is retired with its declared output, with the
    // integration-target evidence recorded against the worktree subject.
    assert!(report.removals.iter().any(|removal| {
        worktree_subject_matches_path(&removal.subject, &canonical)
            && matches!(
                &removal.outcome,
                provenant_records::custody::RemovalOutcome::Removed { .. }
            )
    }));
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
        .head
        .ok_or(WorktreeError::InvalidOutput)?;
    assert_ne!(ahead, pin);
    fast_forward(&context, &info.path, pin, &cancel())?;
    let after = context
        .facts
        .read_inventory(&context, &cancel())?
        .into_iter()
        .find(|entry| entry.path == info.path)
        .ok_or("missing writer")?
        .head
        .ok_or(WorktreeError::InvalidOutput)?;
    assert_eq!(after, ahead);
    Ok(())
}
