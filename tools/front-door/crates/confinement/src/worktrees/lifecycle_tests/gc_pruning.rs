use super::*;

#[test]
fn interrupted_linked_retirement_replays_its_reference_step() -> Result<(), Box<dyn Error>> {
    use crate::worktrees::RetirementIntent;
    use provenant_records::custody::{
        RemovalEvidence, RetirementDecision, RetirementStep, RetirementStepState,
    };

    struct UnavailableForge<'a>(&'a HostRetirementFixture);
    impl RetirementFeeds for UnavailableForge<'_> {
        fn authority(&self) -> Result<provenant_core::ActorId, String> {
            self.0.authority()
        }

        fn custody(&self, lane: &WorktreeInfo) -> Result<(), String> {
            self.0.custody(lane)
        }

        fn forge(&self, _branch: &GitRefName) -> ForgeObservation {
            ForgeObservation::Unknown("fixture forge unavailable".into())
        }

        fn fresh_forge(&self, branch: &GitRefName) -> ForgeObservation {
            self.forge(branch)
        }

        fn integration_bases(&self) -> Vec<(String, GitOid)> {
            self.0.integration_bases()
        }

        fn remote_guard(
            &self,
            context: &WorktreeContext<'_>,
            branch: &str,
            tip: GitOid,
            min_days: u64,
        ) -> Result<(), String> {
            self.0.remote_guard(context, branch, tip, min_days)
        }
    }

    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let lane = gc_owned_lane(&test, &context, "d12-replay")?;
    let custody = HostRetirementFixture::new(&test)?;
    custody.seed_lane(&context, &lane)?;
    let owner = lane
        .owner
        .as_ref()
        .map_err(|error| io::Error::other(error.to_string()))?
        .as_ref()
        .ok_or("fixture lane has no owner")?;
    let reference = owner
        .reference
        .clone()
        .ok_or("fixture lane has no ref owner")?;
    let reference_name = GitRefName::new(&reference.reference)?;
    let worktree = acquire_worktree_subject_at(&context, &lane.path)?;
    let evidence = gc_landing_evidence(&fixture, lane.head)?;
    let decision = RetirementDecision::Discard {
        authority: custody.actor,
        evidence: RemovalEvidence::Landing { evidence },
    };
    let record = custody
        .begin_retirement(RetirementIntent {
            project: test.binding.project_id(),
            related_subjects: vec![worktree.clone()],
            reference,
            expected_tip: lane.head,
            decision: decision.clone(),
        })?
        .ok_or("fixture retirement has no durable owner")?;
    let retirement_id = record.id.clone();

    let path = lane
        .path
        .to_str()
        .ok_or("fixture worktree path is not UTF-8")?;
    let removed = fixture.run(&["worktree", "remove", "--", path])?;
    assert_eq!(removed.termination, Termination::Exit(0));
    custody.advance_retirement(
        &record,
        RetirementStep::Worktree { subject: worktree },
        RetirementStepState::Completed {
            evidence: decision.evidence().clone(),
        },
    )?;
    assert_eq!(
        ref_tip(&context, &reference_name, &cancel())?,
        Some(lane.head)
    );

    let unavailable = UnavailableForge(&custody);
    let pending = resume_pending_retirements(&context, &custody, &unavailable, &cancel())?;
    assert!(pending.iter().any(|record| {
        record.outcomes.iter().any(|outcome| {
            outcome.step == RetirementStep::Reference
                && !matches!(&outcome.state, RetirementStepState::Completed { .. })
        })
    }));
    assert_eq!(
        ref_tip(&context, &reference_name, &cancel())?,
        Some(lane.head)
    );

    let resumed = resume_pending_retirements(&context, &custody, &custody, &cancel())?;
    let completed = resumed
        .iter()
        .find(|record| record.id == retirement_id)
        .ok_or("pending retirement was not returned")?;
    assert!(
        completed
            .outcomes
            .iter()
            .all(|outcome| matches!(&outcome.state, RetirementStepState::Completed { .. }))
    );
    assert_eq!(ref_tip(&context, &reference_name, &cancel())?, None);

    fixture.run(&["branch", "d12-explicit-ref"])?;
    let explicit_name = GitRefName::branch("d12-explicit-ref")?;
    let explicit_owner = RefLedger::for_context(&context)?.observe_local_branch(
        &explicit_name,
        lane.head,
        &test.clock,
        &cancel(),
    )?;
    let explicit_decision = RetirementDecision::Discard {
        authority: custody.actor,
        evidence: RemovalEvidence::ExplicitDiscard { archive: None },
    };
    let explicit = custody
        .begin_retirement(RetirementIntent {
            project: test.binding.project_id(),
            related_subjects: Vec::new(),
            reference: explicit_owner.ownership(),
            expected_tip: lane.head,
            decision: explicit_decision,
        })?
        .ok_or("explicit ref retirement has no durable owner")?;
    let explicit_id = explicit.id;
    let explicit_resumed = resume_pending_retirements(&context, &custody, &custody, &cancel())?;
    let explicit_completed = explicit_resumed
        .iter()
        .find(|record| record.id == explicit_id)
        .ok_or("explicit ref retirement was not returned")?;
    assert!(
        explicit_completed
            .outcomes
            .iter()
            .all(|outcome| matches!(&outcome.state, RetirementStepState::Completed { .. }))
    );
    assert_eq!(ref_tip(&context, &explicit_name, &cancel())?, None);
    Ok(())
}

#[test]
fn removal_preparation_allows_shared_launch_lock() -> Result<(), Box<dyn Error>> {
    struct PreparationProbe<'a> {
        inner: &'a dyn Executor,
        common: &'a Directory,
        writer: &'a Directory,
        artifact: PathBuf,
        preparation_seen: std::sync::atomic::AtomicBool,
        mutation_seen: std::sync::atomic::AtomicBool,
    }
    impl Executor for PreparationProbe<'_> {
        fn execute(
            &self,
            options: provenant_os::exec::ExecOptions<'_>,
        ) -> Result<ExecOutcome, provenant_os::exec::ExecError> {
            use std::sync::atomic::Ordering;
            if options.argv.iter().any(|arg| arg == "status")
                && self.artifact.exists()
                && matches!(
                    self.writer
                        .try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive),
                    Err(FsError::LockTimeout { .. })
                )
            {
                // Preparation checks ignored output under writer custody. A
                // launch in another tree must still be able to take Shared.
                let launch = self
                    .common
                    .try_lock_existing(OsStr::new(REPOSITORY_LOCK), LockMode::Shared);
                assert!(
                    launch.is_ok(),
                    "launch blocked during output preparation: {launch:?}"
                );
                self.preparation_seen.store(true, Ordering::SeqCst);
            }
            if options.argv.iter().any(|arg| arg == "worktree")
                && options.argv.iter().any(|arg| arg == "remove")
            {
                assert!(matches!(
                    self.common
                        .try_lock_existing(OsStr::new(REPOSITORY_LOCK), LockMode::Shared),
                    Err(FsError::LockTimeout { .. })
                ));
                self.mutation_seen.store(true, Ordering::SeqCst);
            }
            self.inner.execute(options)
        }
    }
    let fixture = Fixture::new()?;
    fs::write(fixture.root.join(".git/info/exclude"), "target/\n")?;
    let test = TestContext::new(&fixture)?;
    let mut context = test.context();
    let info = gc_owned_lane(&test, &context, "removal-launch-lock")?;
    let retirement = HostRetirementFixture::new(&test)?;
    retirement.seed_lane(&context, &info)?;
    let artifact = info.path.join("target/artifact");
    fs::create_dir(info.path.join("target"))?;
    fs::write(&artifact, "output")?;
    let writer = context
        .project_state
        .open_verified_directory(&state_dir_name(&info.path))?;
    let probe = PreparationProbe {
        inner: context.executor,
        common: context.repo.common_dir(),
        writer: &writer,
        artifact,
        preparation_seen: Default::default(),
        mutation_seen: Default::default(),
    };
    context.executor = &probe;
    let workers = LockProbeWorkers {
        common: context.repo.common_dir(),
        repository_held: Default::default(),
    };
    context.workers = &workers;
    let result = remove_with_retirement(
        &context,
        WorktreeRemove::Name {
            name: "removal-launch-lock".into(),
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
        retirement.actor,
    )?;
    assert_eq!(result.removed.len(), 1);
    assert!(
        probe
            .preparation_seen
            .load(std::sync::atomic::Ordering::SeqCst)
    );
    assert!(
        probe
            .mutation_seen
            .load(std::sync::atomic::Ordering::SeqCst)
    );
    let observed = workers.repository_held.lock().unwrap();
    assert!(!observed.is_empty());
    assert!(
        observed.iter().all(|held| !held),
        "census held repository lock: {observed:?}"
    );
    Ok(())
}

#[test]
fn removal_refreshes_session_after_output_preparation() -> Result<(), Box<dyn Error>> {
    struct AppearingAfterPreparation {
        artifact: PathBuf,
        complete_required: bool,
    }
    impl WorkerObserver for AppearingAfterPreparation {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            let state = if self.artifact.exists() {
                Occupancy::Available
            } else {
                Occupancy::Observed { process: None }
            };
            roots
                .iter()
                .cloned()
                .map(|root| (root, state.clone()))
                .collect()
        }
        fn requires_complete_census(&self) -> bool {
            self.complete_required
        }
    }
    let fixture = Fixture::new()?;
    fs::write(fixture.root.join(".git/info/exclude"), "target/\n")?;
    let test = TestContext::new(&fixture)?;
    let mut context = test.context();
    let info = gc_owned_lane(&test, &context, "late-worker")?;
    let retirement = HostRetirementFixture::new(&test)?;
    retirement.seed_lane(&context, &info)?;
    let artifact = info.path.join("target/artifact");
    fs::create_dir(info.path.join("target"))?;
    fs::write(&artifact, "output")?;
    let observer = AppearingAfterPreparation {
        artifact: artifact.clone(),
        complete_required: false,
    };
    let session = CensusSession::new(&observer, &test.clock);
    context.workers = &session;
    let result = remove_with_retirement(
        &context,
        WorktreeRemove::Name {
            name: "late-worker".into(),
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
        retirement.actor,
    );
    assert!(
        matches!(
            result,
            Err(WorktreeError::LiveWriter | WorktreeError::LiveProcess(_))
        ),
        "{result:?}"
    );
    assert!(
        !artifact.exists(),
        "output preparation must precede the late observation"
    );
    assert!(info.path.is_dir());
    Ok(())
}

#[test]
fn removal_and_gc_sessions_detect_worker_after_preview() -> Result<(), Box<dyn Error>> {
    use std::sync::atomic::{AtomicBool, Ordering};
    #[derive(Clone, Copy)]
    enum EntryPoint {
        Removal { complete_required: bool },
        GarbageCollection,
        CustodyRefresh,
    }
    struct Appearing {
        live: AtomicBool,
        complete_required: bool,
    }
    impl WorkerObserver for Appearing {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            let state = if self.live.load(Ordering::SeqCst) {
                Occupancy::Observed { process: None }
            } else {
                Occupancy::Available
            };
            roots
                .iter()
                .cloned()
                .map(|root| (root, state.clone()))
                .collect()
        }
        fn requires_complete_census(&self) -> bool {
            self.complete_required
        }
    }
    for entry_point in [
        EntryPoint::Removal {
            complete_required: false,
        },
        EntryPoint::Removal {
            complete_required: true,
        },
        EntryPoint::GarbageCollection,
        EntryPoint::CustodyRefresh,
    ] {
        let fixture = Fixture::new()?;
        let test = TestContext::new(&fixture)?;
        let mut context = test.context();
        let branch = match entry_point {
            EntryPoint::Removal { .. } => "worker-appears",
            EntryPoint::GarbageCollection => "gc-worker-appears",
            EntryPoint::CustodyRefresh => "custody-worker-appears",
        };
        let lane = match entry_point {
            EntryPoint::Removal { .. } => Some(create(
                &context,
                WorktreeCreate {
                    branch: branch.into(),
                    base: None,
                },
                &cancel(),
            )?),
            EntryPoint::GarbageCollection => Some(gc_merged_worktree(&test, &context, branch)?),
            EntryPoint::CustodyRefresh => None,
        };
        let retirement = HostRetirementFixture::new(&test)?;
        if matches!(entry_point, EntryPoint::GarbageCollection) {
            retirement.seed_lane(&context, lane.as_ref().ok_or("GC case has a worktree")?)?;
        }
        let watched_root = lane
            .as_ref()
            .map_or_else(|| fixture.root.clone(), |lane| lane.path.clone());
        let observer = Appearing {
            live: AtomicBool::new(false),
            complete_required: match entry_point {
                EntryPoint::Removal { complete_required } => complete_required,
                EntryPoint::CustodyRefresh => true,
                EntryPoint::GarbageCollection => false,
            },
        };
        let session = CensusSession::new(&observer, &test.clock);
        assert_eq!(
            session.observe_roots(std::slice::from_ref(&watched_root))[&watched_root],
            Occupancy::Available
        );
        context.workers = &session;
        observer.live.store(true, Ordering::SeqCst);
        match entry_point {
            EntryPoint::Removal { .. } => {
                let error = remove_with_port(
                    &test,
                    &context,
                    WorktreeRemove::Name {
                        name: branch.into(),
                        discard: false,
                        integration: None,
                        expected_head: None,
                    },
                    &cancel(),
                )
                .err()
                .ok_or("removed occupied tree")?;
                assert!(matches!(
                    error,
                    WorktreeError::LiveWriter | WorktreeError::LiveProcess(_)
                ));
            }
            EntryPoint::GarbageCollection => {
                let evidence = gc_landing_evidence(&fixture, fixture.head()?)?;
                let report = collect(
                    &context,
                    std::slice::from_ref(lane.as_ref().ok_or("GC case has a worktree")?),
                    &WorktreeGcPolicy {
                        include_foreign: false,
                        retirement_port: Some(&retirement),
                        retirement_feeds: Some(&retirement),
                        mode: CollectionMode::Worktrees,
                        proof: Some(&evidence),
                        upstream_reference: None,
                        integration_patterns: &[],
                        excluded_paths: &[],
                        upstream: Some(fixture.head()?),
                        terminal: &|_| true,
                        no_live_lanes: &|_| true,
                        regenerable: &[],
                        keep: &[],
                        written_before_ns: i128::MAX,
                        dry_run: false,
                    },
                    &cancel(),
                )?;
                assert!(report.worktrees.retained.iter().any(|retained| {
                    retained.branch == branch && retained.reason == RetainReason::LiveWriter
                }));
                assert!(report.removals.iter().all(|removal| {
                    !matches!(
                        &removal.outcome,
                        provenant_records::custody::RemovalOutcome::Removed { .. }
                    )
                }));
            }
            EntryPoint::CustodyRefresh => {
                let error = lifecycle::refresh_census_under_custody(&context, &watched_root)
                    .expect_err("refresh must observe the live worker");
                assert!(matches!(
                    error,
                    WorktreeError::LiveWriter | WorktreeError::LiveProcess(_)
                ));
            }
        }
        if let Some(lane) = lane {
            assert!(lane.path.is_dir());
        }
    }
    Ok(())
}

#[test]
fn foreign_gc_session_requires_fresh_complete_census() -> Result<(), Box<dyn Error>> {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct BecomingUnknown(AtomicBool);
    impl WorkerObserver for BecomingUnknown {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            roots
                .iter()
                .cloned()
                .map(|root| {
                    let state = if self.0.load(Ordering::SeqCst) {
                        Occupancy::census_unknown(root.clone(), "fixture census unavailable")
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
    let path = fixture._temp.path().join("foreign-session");
    fixture.run(&[
        "worktree",
        "add",
        "-b",
        "foreign-session",
        path.to_str().ok_or("path")?,
    ])?;
    let lanes = list(&context, &cancel())?;
    let retirement = HostRetirementFixture::new(&test)?;
    let evidence = gc_landing_evidence(&fixture, fixture.head()?)?;
    let observer = BecomingUnknown(AtomicBool::new(false));
    let session = CensusSession::new(&observer, &test.clock);
    assert_eq!(
        session.observe_roots(std::slice::from_ref(&path))[&path],
        Occupancy::Available
    );
    context.workers = &session;
    observer.0.store(true, Ordering::SeqCst);
    let report = collect(
        &context,
        &lanes,
        &WorktreeGcPolicy {
            include_foreign: true,
            retirement_port: Some(&retirement),
            retirement_feeds: Some(&retirement),
            mode: CollectionMode::Worktrees,
            proof: Some(&evidence),
            upstream_reference: None,
            integration_patterns: &[],
            excluded_paths: &[],
            upstream: Some(fixture.head()?),
            terminal: &|_| false,
            no_live_lanes: &|_| true,
            regenerable: &[],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: false,
        },
        &cancel(),
    )?;
    assert!(report.worktrees.retained.iter().any(|retained| {
        retained.branch == "foreign-session" && retained.reason == RetainReason::UnknownWriter
    }));
    assert!(path.is_dir());
    Ok(())
}

#[test]
fn gc_managed_removal_needs_a_fresh_free_census_under_custody() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test_context = TestContext::new(&fixture)?;
    let mut context = test_context.context();
    let lane = gc_merged_worktree(&test_context, &context, "gc-census-flip")?;
    let upstream = fixture.head()?;
    let retirement = HostRetirementFixture::new(&test_context)?;
    retirement.seed_lane(&context, &lane)?;
    let evidence = gc_landing_evidence(&fixture, upstream)?;
    let flipping = FlippingWorkers {
        unknown: std::sync::atomic::AtomicBool::new(false),
    };
    let session = CensusSession::new(&flipping, &test_context.clock);
    assert!(session.observe_roots(std::slice::from_ref(&lane.path))[&lane.path].is_available());
    flipping
        .unknown
        .store(true, std::sync::atomic::Ordering::SeqCst);
    context.workers = &session;
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
    assert!(report.worktrees.retained.iter().any(|retained| {
        retained.branch == "gc-census-flip" && retained.reason == RetainReason::UnknownWriter
    }));
    assert!(lane.path.is_dir());
    assert!(branch_exists(&fixture, "gc-census-flip"));
    Ok(())
}

#[test]
fn gc_preview_and_apply_both_retain_a_managed_tree_with_an_unknown_census()
-> Result<(), Box<dyn Error>> {
    for dry_run in [true, false] {
        let fixture = Fixture::new()?;
        let test_context = TestContext::new(&fixture)?;
        let mut context = test_context.context();
        let lane = gc_merged_worktree(&test_context, &context, "gc-census-unknown")?;
        let retirement = HostRetirementFixture::new(&test_context)?;
        retirement.seed_lane(&context, &lane)?;
        let evidence = gc_landing_evidence(&fixture, fixture.head()?)?;
        let unknown = FlippingWorkers {
            unknown: std::sync::atomic::AtomicBool::new(true),
        };
        context.workers = &unknown;
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
                upstream: Some(fixture.head()?),
                terminal: &|_| true,
                no_live_lanes: &|_| true,
                regenerable: &[],
                keep: &[],
                written_before_ns: i128::MAX,
                dry_run,
            },
            &cancel(),
        )?;
        assert!(report.worktrees.retained.iter().any(|retained| {
            retained.branch == "gc-census-unknown" && retained.reason == RetainReason::UnknownWriter
        }));
        assert!(report.removals.iter().all(|removal| {
            !matches!(
                &removal.outcome,
                provenant_records::custody::RemovalOutcome::Removed { .. }
            )
        }));
        assert!(lane.path.is_dir());
    }
    Ok(())
}

#[test]
fn gc_dry_run_keeps_a_merged_foreign_worktree_when_foreign_cleanup_is_off()
-> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let lane = gc_merged_unowned_worktree(&fixture, &context, "gc-dry-unknown")?;
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
            terminal: &|_| false,
            no_live_lanes: &|_| true,
            regenerable: &[],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: true,
        },
        &cancel(),
    )?;
    // A merged checkout Provenant did not create is foreign: with foreign
    // cleanup off it is kept and reported as such, never removed.
    assert!(report.worktrees.removed.is_empty());
    assert!(report.removals.is_empty());
    let [candidate] = report.candidates.as_slice() else {
        return Err("expected exactly one candidate".into());
    };
    assert_eq!((candidate.class, candidate.action), ("foreign", "kept"));
    assert!(candidate.path.is_dir());
    Ok(())
}

#[test]
fn gc_dry_run_retains_preview_dirty_worktree() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fs::write(fixture.root.join(".gitignore"), "cache/\n")?;
    fixture.run(&["add", ".gitignore"])?;
    fixture.run(&["commit", "-m", "ignore cache"])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let mut lane = gc_merged_worktree(&test_context, &context, "gc-dry-dirty")?;
    // Ignored content outside the regenerable output keeps the worktree
    // dirty once the preview has pruned.
    fs::create_dir(lane.path.join("cache"))?;
    fs::write(lane.path.join("cache/artifact"), "keep")?;
    lane.dirty = WorktreeCondition::Ignored;
    let report = collect(
        &context,
        &[lane.clone()],
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
            dry_run: true,
        },
        &cancel(),
    )?;
    assert_eq!(report.worktrees.retained.len(), 1);
    assert_eq!(report.worktrees.retained[0].reason, RetainReason::Dirty);
    assert!(report.worktrees.removed.is_empty());
    assert!(lane.path.join("cache/artifact").is_file());
    Ok(())
}

#[test]
fn gc_settlement_error_retains_ref_and_continues() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test_context = TestContext::new(&fixture)?;
    let mut context = test_context.context();
    let first = gc_merged_worktree(&test_context, &context, "gc-settle-failed")?;
    let second = gc_merged_worktree(&test_context, &context, "gc-settle-next")?;
    let retirement = HostRetirementFixture::new(&test_context)?;
    retirement.seed_lane(&context, &first)?;
    retirement.seed_lane(&context, &second)?;
    let evidence = gc_landing_evidence(&fixture, fixture.head()?)?;
    let faulty = FailsFirstGcSettlement {
        executor: &fixture.executor,
        removed: std::sync::atomic::AtomicBool::new(false),
        failed: std::sync::atomic::AtomicBool::new(false),
    };
    context.executor = &faulty;
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
            upstream: Some(fixture.head()?),
            terminal: &|_| true,
            no_live_lanes: &|_| true,
            regenerable: &[],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: false,
        },
        &cancel(),
    )?;
    assert!(report.removals.iter().any(|removal| {
        worktree_subject_matches_path(&removal.subject, &first.path)
            && matches!(
                &removal.outcome,
                provenant_records::custody::RemovalOutcome::Removed { .. }
            )
    }));
    assert!(report.removals.iter().any(|removal| {
        worktree_subject_matches_path(&removal.subject, &second.path)
            && matches!(
                &removal.outcome,
                provenant_records::custody::RemovalOutcome::Removed { .. }
            )
    }));
    assert!(report.removals.iter().any(|removal| {
        matches!(
            &removal.outcome,
            provenant_records::custody::RemovalOutcome::Kept {
                cause: provenant_records::custody::RemovalUncertainty::RetirementIncomplete
            }
        )
    }));
    assert!(branch_exists(&fixture, "gc-settle-failed"));
    assert!(!branch_exists(&fixture, "gc-settle-next"));
    assert!(!first.path.exists());
    assert!(!second.path.exists());
    Ok(())
}

#[test]
fn timed_out_add_rolls_back_only_the_branch_it_created() -> Result<(), Box<dyn Error>> {
    #[derive(Clone, Copy)]
    struct Case {
        branch: &'static str,
        existed_before: bool,
    }
    for case in [
        Case {
            branch: "feature/slow-new",
            existed_before: false,
        },
        Case {
            branch: "feature/slow-existing",
            existed_before: true,
        },
    ] {
        let fixture = Fixture::new()?;
        if case.existed_before {
            fixture.run(&["branch", case.branch])?;
        }
        let test_context = TestContext::new(&fixture)?;
        let faulty = TimesOutWorktreeAdd(&fixture.executor);
        let mut context = test_context.context();
        context.executor = &faulty;
        let failed = create(
            &context,
            WorktreeCreate {
                branch: case.branch.to_owned(),
                base: Some("HEAD".to_owned()),
            },
            &cancel(),
        );
        let Err(WorktreeError::CreateFailed { cause, rolled_back }) = failed else {
            return Err(format!("expected CreateFailed, got {failed:?}").into());
        };
        assert!(rolled_back);
        assert!(matches!(
            *cause,
            WorktreeError::Utility(UtilityError::TimedOut)
        ));
        assert!(
            !fixture
                .root
                .join(".worktrees")
                .join(case.branch.replace('/', "-"))
                .exists()
        );
        assert_eq!(branch_exists(&fixture, case.branch), case.existed_before);
        let admin = fixture.repo.common_git_dir().join("worktrees");
        assert!(!admin.exists() || fs::read_dir(&admin)?.next().is_none());
        if !case.existed_before {
            let retried = create(
                &test_context.context(),
                WorktreeCreate {
                    branch: case.branch.to_owned(),
                    base: Some("HEAD".to_owned()),
                },
                &cancel(),
            )?;
            assert!(retried.path.is_dir());
        }
    }
    Ok(())
}

#[test]
fn create_fetches_a_remote_base_with_a_main_only_refspec() -> Result<(), Box<dyn Error>> {
    let source = Fixture::new()?;
    source.run(&["branch", "-M", "main"])?;
    source.run(&["checkout", "-b", "chore/cleanup-code-sweep"])?;
    source.run(&["commit", "--allow-empty", "-m", "remote-only commit"])?;
    let base = source.head()?;
    source.run(&["checkout", "main"])?;
    let mut fixture = Fixture::new()?;
    let clone = fixture._temp.path().join("clone");
    fixture.run(&[
        "clone",
        "--single-branch",
        "--branch",
        "main",
        source.root.to_str().ok_or("non-UTF8 source")?,
        clone.to_str().ok_or("non-UTF8 clone")?,
    ])?;
    fixture.root = clone;
    fixture.repo = fixture.runtime.open_repo(
        &fixture.executor,
        &fixture.root,
        fixture.budget()?,
        &cancel(),
    )?;
    let refspec = fixture.run(&["config", "--get-all", "remote.origin.fetch"])?;
    assert_eq!(
        refspec.stdout.bytes,
        b"+refs/heads/main:refs/remotes/origin/main\n"
    );
    assert!(
        fixture
            .run(&["for-each-ref", "refs/remotes/origin/chore/"])?
            .stdout
            .bytes
            .is_empty()
    );
    let test_context = TestContext::new(&fixture)?;

    let observes_fetch = ObservesFetch {
        executor: &fixture.executor,
        seen: std::sync::atomic::AtomicBool::new(false),
    };
    let mut context = test_context.context();
    context.executor = &observes_fetch;
    let created = create(
        &context,
        WorktreeCreate {
            branch: "chore/cleanup-code-sweep-2".to_owned(),
            base: Some("origin/chore/cleanup-code-sweep".to_owned()),
        },
        &cancel(),
    )?;

    assert_eq!(created.head, base);
    assert!(
        observes_fetch
            .seen
            .load(std::sync::atomic::Ordering::Relaxed)
    );
    let tracking = fixture.run(&[
        "for-each-ref",
        "refs/remotes/origin/chore/cleanup-code-sweep",
    ])?;
    assert!(tracking.stdout.bytes.is_empty());
    assert_eq!(
        fixture
            .run(&["config", "--get-all", "remote.origin.fetch"])?
            .stdout
            .bytes,
        refspec.stdout.bytes
    );
    Ok(())
}

#[test]
fn remote_base_uses_exact_advertisement_instead_of_tracking_ref() -> Result<(), Box<dyn Error>> {
    let source = Fixture::new()?;
    source.run(&["branch", "-M", "main"])?;
    let parent = source.head()?;
    source.run(&["commit", "--allow-empty", "-m", "advertised tip"])?;
    let expected = source.head()?;
    let mut fixture = Fixture::new()?;
    let clone = fixture._temp.path().join("clone");
    fixture.run(&[
        "clone",
        source.root.to_str().ok_or("non-UTF8 source")?,
        clone.to_str().ok_or("non-UTF8 clone")?,
    ])?;
    fixture.root = clone;
    fixture.repo = fixture.runtime.open_repo(
        &fixture.executor,
        &fixture.root,
        fixture.budget()?,
        &cancel(),
    )?;
    fixture.run(&["config", "user.name", "Fixture"])?;
    fixture.run(&["config", "user.email", "fixture@example.invalid"])?;
    fixture.repo = fixture.runtime.open_repo(
        &fixture.executor,
        &fixture.root,
        fixture.budget()?,
        &cancel(),
    )?;
    fixture.run(&["commit", "--allow-empty", "-m", "different tracking tip"])?;
    let wrong = fixture.head()?;
    assert_ne!(wrong, expected);
    fixture.run(&["update-ref", "refs/remotes/origin/main", &wrong.to_string()])?;
    fixture.repo = fixture.runtime.open_repo(
        &fixture.executor,
        &fixture.root,
        fixture.budget()?,
        &cancel(),
    )?;
    let test_context = TestContext::new(&fixture)?;
    for alias in [
        "remotes/origin/main",
        "origin",
        "@{u}",
        "main@{upstream}",
        "@{push}",
    ] {
        assert_eq!(
            pin_revision(
                &test_context.context(),
                &RepoFacts::default(),
                alias,
                &cancel()
            )?,
            expected,
            "{alias}"
        );
    }
    for expression in [
        "origin/main~0",
        "remotes/origin/main^0",
        "@{u}~0",
        "main@{upstream}^{commit}",
        "@{push}^0",
    ] {
        assert_eq!(
            pin_revision(
                &test_context.context(),
                &RepoFacts::default(),
                expression,
                &cancel()
            )?,
            expected,
            "{expression}"
        );
    }
    // Every spelling of a tracking root verifies against the advertisement
    // and applies its suffix to that, never to the stale local tip (whose
    // first parent is `expected`, not `parent`).
    for root in [
        "origin/main",
        "remotes/origin/main",
        "refs/remotes/origin/main",
        "origin/HEAD",
        "remotes/origin/HEAD",
        "refs/remotes/origin/HEAD",
        "origin",
        "@{u}",
        "@{push}",
    ] {
        for (suffix, oid) in [
            ("", expected),
            ("~1", parent),
            ("^", parent),
            ("^{commit}", expected),
            ("~1^{commit}", parent),
        ] {
            assert_eq!(
                pin_revision(
                    &test_context.context(),
                    &RepoFacts::default(),
                    &format!("{root}{suffix}"),
                    &cancel()
                )?,
                oid,
                "{root}{suffix}"
            );
        }
    }
    let (_, integration) = pin_integration(&test_context.context(), "origin/main", &cancel())?;
    assert_eq!(integration, expected);
    let (_, default_integration) =
        pin_integration(&test_context.context(), "origin/HEAD", &cancel())?;
    assert_eq!(default_integration, expected);
    let created = create(
        &test_context.context(),
        WorktreeCreate {
            branch: "feature/verified-base".to_owned(),
            base: Some("origin/main".to_owned()),
        },
        &cancel(),
    )?;
    assert_eq!(created.head, expected);
    let tracking = fixture.run(&["rev-parse", "refs/remotes/origin/main"])?;
    assert_eq!(
        String::from_utf8(tracking.stdout.bytes)?.trim(),
        wrong.to_string()
    );
    Ok(())
}

#[test]
fn tracking_base_the_remote_cannot_vouch_for_fails_closed() -> Result<(), Box<dyn Error>> {
    let mut fixture = Fixture::new()?;
    let head = fixture.head()?;
    fixture.run(&[
        "remote",
        "add",
        "origin",
        fixture.root.to_str().ok_or("non-UTF8 root")?,
    ])?;
    // A tracking ref whose remote is not configured, and one whose remote
    // is spelled in another case: Git resolves both locally.
    fixture.run(&["update-ref", "refs/remotes/gone/main", &head.to_string()])?;
    fixture.run(&["update-ref", "refs/remotes/origin/main", &head.to_string()])?;
    fixture.repo = fixture.runtime.open_repo(
        &fixture.executor,
        &fixture.root,
        fixture.budget()?,
        &cancel(),
    )?;
    let test_context = TestContext::new(&fixture)?;
    let mut unverifiable = vec!["gone/main", "gone/main~1", "remotes/gone/main^{commit}"];
    // On a case-insensitive filesystem `Origin/main` finds origin/main.
    let probe = fixture._temp.path().join("CaseProbe");
    fs::write(&probe, b"")?;
    let insensitive = fixture._temp.path().join("caseprobe").exists();
    if insensitive {
        unverifiable.extend(["Origin/main", "Origin/main~1", "remotes/Origin/main"]);
    }
    for base in unverifiable {
        let error = pin_revision(
            &test_context.context(),
            &RepoFacts::default(),
            base,
            &cancel(),
        )
        .err()
        .ok_or_else(|| format!("{base} must not fall back to the local tracking OID"))?;
        assert!(
            matches!(&error, WorktreeError::BaseUnverified { base: named } if named == base),
            "{base}: {error}"
        );
    }
    Ok(())
}

#[test]
fn unresolved_base_error_names_the_base() -> Result<(), Box<dyn Error>> {
    let mut fixture = Fixture::new()?;
    fixture.run(&[
        "remote",
        "add",
        "origin",
        fixture.root.to_str().ok_or("non-UTF8 root")?,
    ])?;
    fixture.repo = fixture.runtime.open_repo(
        &fixture.executor,
        &fixture.root,
        fixture.budget()?,
        &cancel(),
    )?;
    let test_context = TestContext::new(&fixture)?;
    let error = create(
        &test_context.context(),
        WorktreeCreate {
            branch: "feature/missing-base".to_owned(),
            base: Some("origin/missing".to_owned()),
        },
        &cancel(),
    )
    .expect_err("missing remote branch must fail");
    assert!(matches!(
        error,
        WorktreeError::BaseNotFound { ref base } if base == "origin/missing"
    ));

    let error = create(
        &test_context.context(),
        WorktreeCreate {
            branch: "feature/missing-base".to_owned(),
            base: Some("unknown/branch".to_owned()),
        },
        &cancel(),
    )
    .expect_err("missing base must fail");
    assert_eq!(
        error.to_string(),
        "base unknown/branch did not resolve to a commit"
    );
    Ok(())
}

#[test]
fn create_skips_prunable_worktree_registrations() -> Result<(), Box<dyn Error>> {
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
    let listing = fixture.run(&["worktree", "list", "--porcelain"])?;
    assert!(String::from_utf8_lossy(&listing.stdout.bytes).contains("prunable"));

    let created = create(
        &context,
        WorktreeCreate {
            branch: "feature/after-prunable".to_owned(),
            base: None,
        },
        &cancel(),
    )?;

    assert!(created.path.is_dir());
    assert!(!stale.path.exists());
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
fn create_with_prunable_registration_for_same_branch() -> Result<(), Box<dyn Error>> {
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
    let error = create(
        &context,
        WorktreeCreate {
            branch: "scratch/stale".to_owned(),
            base: None,
        },
        &cancel(),
    )
    .expect_err("stale registration must fail with prune hint");
    assert!(matches!(error, WorktreeError::StaleRegistration));
    assert!(error.to_string().contains("git worktree prune"), "{error}");

    fixture.run(&["commit", "--allow-empty", "-m", "advance HEAD"])?;
    let head = fixture.head()?;
    fixture.run(&["update-ref", "-d", "refs/heads/scratch/stale"])?;
    fixture.run(&["branch", "scratch/stale", &head.to_string()])?;
    let error_recreated = create(
        &context,
        WorktreeCreate {
            branch: "scratch/stale".to_owned(),
            base: None,
        },
        &cancel(),
    )
    .expect_err("recreated branch with stale registration must fail with prune hint");
    assert!(matches!(error_recreated, WorktreeError::StaleRegistration));
    Ok(())
}
