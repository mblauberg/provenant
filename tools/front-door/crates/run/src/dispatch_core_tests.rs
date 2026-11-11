use super::*;

#[test]
fn cancellation_keeps_a_genuine_launch_preparation_failure() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    let id = run.attempts[0].attempt_id;
    store.create(&run, &call)?;
    let started = store.start(
        run.run_id,
        &Ready,
        &|record, _| {
            let attempt = store.attempt_dir(record.run_id, record.attempt_id, false)?;
            artifacts::seal(
                &attempt,
                "cancel.json",
                &crate::db::bytes(&serde_json::json!({
                    "scope": RunStore::scope(record)
                }))?,
                RECORD_LIMIT,
            )?;
            Err(RunError::Validation("fixture preparation broke"))
        },
        &call,
    )?;
    assert!(matches!(started.as_slice(), [Started::Refused { .. }]));
    let record = store.record(id, &call)?;
    assert!(matches!(record.state, AttemptState::Terminal {
        outcome: Outcome::Cancelled, ref reason, ..
    } if reason.as_deref().is_some_and(|reason| reason.contains("fixture preparation broke"))));
    let AttemptState::Terminal { extra, .. } = &record.state else {
        return Err("cancelled attempt is not terminal".into());
    };
    assert_eq!(
        extra.get("terminal_cause"),
        Some(&serde_json::to_value(TerminalCause::HandoffRefused)?)
    );
    assert_eq!(record.provider_launched, Some(false));
    Ok(())
}
#[test]
fn publication_legacy_result_orphan_retains_manifest_failure() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let input = input();
    store.create(&input, &call)?;
    let id = input.attempts[0].attempt_id;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let record = store.claim(id, &clock, &call)?.ok_or("claim")?;
    let attempt = store.attempt_dir(input.run_id, id, false)?;
    artifacts::seal(&attempt, "stdout.log", b"answer", CAPTURE_LIMIT)?;
    artifacts::seal(&attempt, "result.md", b"legacy result", RESULT_LIMIT)?;
    let archive = artifacts::subdir(&attempt, std::ffi::OsStr::new("quarantine"), true)?;
    artifacts::seal(
        &archive,
        "manifest.json",
        b"conflicting evidence",
        RECORD_LIMIT,
    )?;
    let observed = ExitObservation {
        scope: RunStore::scope(&record),
        ..exit()
    };
    seal_exit(&store, &observed)?;
    let providers = Fake {
        observed: Some(route()),
        ..Fake::default()
    };
    let record = store.finish(id, &attempt, &observed, &providers, &call)?;
    assert!(matches!(
        record.settlement_receipts.unwrap().manifest_conflict,
        EffectDisposition::Failed {
            retryable: true,
            ..
        }
    ));
    assert_eq!(
        artifacts::read_sealed(&attempt, "result.md", RESULT_LIMIT)?.unwrap(),
        b"legacy result"
    );
    Ok(())
}
#[test]
fn terminal_dependency_waits_for_custody_release() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let input = input();
    store.create(&input, &call)?;
    let mut record = store.record(input.attempts[0].attempt_id, &call)?;
    record.generation = 1;
    record.state = AttemptState::Terminal {
        extra: Default::default(),
        outcome: Outcome::Succeeded,
        exit_code: Some(0),
        finished_at: record.queued_at,
        reason: None,
    };
    record.provider_launched = Some(false);
    assert!(
        store.custody_released(&record, &call)?,
        "a preflight refusal needs no custody lock"
    );
    record.provider_launched = Some(true);
    let directory = store.attempt_dir(record.run_id, record.attempt_id, false)?;
    assert!(
        store.custody_released(&record, &call).is_err(),
        "missing custody is not release evidence"
    );
    let custody = directory.try_lock(
        std::ffi::OsStr::new("custody.lock"),
        provenant_os::fs::LockMode::Exclusive,
    )?;
    assert!(!store.custody_released(&record, &call)?);
    drop(custody);
    assert!(store.custody_released(&record, &call)?);
    let worktrees = store
        .writer_registry
        .join("worktrees")
        .join(record.project_id.to_string());
    let broken = worktrees.join("broken").join(record.attempt_id.to_string());
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&broken)?;
    std::fs::write(broken.join("reservation.json"), "{")?;
    assert_eq!(store.writer_fence_listing()?.issues.len(), 1);
    assert!(
        store.custody_released(&record, &call)?,
        "another worktree's unreadable reservation is not this attempt's"
    );
    std::fs::remove_dir_all(&broken)?;
    let lease = provenant_supervisor::custody::acquire_reader(
        directory.try_clone()?,
        RunStore::scope(&record),
        call.lock_budget(),
        call.cancel,
        call.clock,
    )?;
    drop(lease);
    assert!(
        !store.custody_released(&record, &call)?,
        "a free lock does not clear a retained reader fence"
    );
    record.state = AttemptState::Running {
        extra: Default::default(),
    };
    assert!(!store.custody_released(&record, &call)?);
    Ok(())
}
#[test]
fn unreadable_reservation_in_the_writers_directory_holds_custody() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let mut input = input();
    let worktree = temp.path().join("lane");
    input.request.tasks[0].writer = Some(WriterRequest {
        extra: Default::default(),
        project: temp.path().to_path_buf(),
        worktree: worktree.clone(),
    });
    store.create(&input, &call)?;
    let mut record = store.record(input.attempts[0].attempt_id, &call)?;
    record.generation = 1;
    record.state = AttemptState::Terminal {
        extra: Default::default(),
        outcome: Outcome::Succeeded,
        exit_code: Some(0),
        finished_at: record.queued_at,
        reason: None,
    };
    record.provider_launched = Some(false);
    let worktrees = store
        .writer_registry
        .join("worktrees")
        .join(record.project_id.to_string());
    let unreadable = |name: &std::ffi::OsStr| -> TestResult {
        let directory = worktrees.join(name).join(record.attempt_id.to_string());
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&directory)?;
        std::fs::write(directory.join("reservation.json"), "{")?;
        Ok(())
    };
    unreadable(std::ffi::OsStr::new("other"))?;
    assert_eq!(store.writer_fence_listing()?.issues.len(), 1);
    assert!(
        store.custody_released(&record, &call)?,
        "another worktree's unreadable reservation is not this attempt's"
    );
    let own = provenant_confinement::worktrees::state_dir_name(&worktree);
    let own_slot = worktrees.join(&own);
    let predecessor = own_slot.join(AttemptId::from_bytes([99; 16]).to_string());
    std::fs::create_dir_all(&predecessor)?;
    std::fs::write(
        predecessor.join("reservation.json"),
        b"malformed predecessor",
    )?;
    assert!(
        store.custody_released(&record, &call)?,
        "predecessor evidence is separate"
    );
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&own_slot, std::fs::Permissions::from_mode(0o0))?;
    let observed = store.custody_released(&record, &call);
    std::fs::set_permissions(&own_slot, std::fs::Permissions::from_mode(0o700))?;
    assert!(!observed?, "an inaccessible own slot cannot prove release");
    std::fs::remove_file(predecessor.join("reservation.json"))?;
    unreadable(&own)?;
    assert_eq!(store.writer_fence_listing()?.issues.len(), 2);
    assert!(
        !store.custody_released(&record, &call)?,
        "this writer's unreadable reservation keeps custody held"
    );
    Ok(())
}
#[test]
fn nested_lane_depth_stops_at_the_configured_maximum() {
    assert!(matches!(next_lane_depth(0, 2), Ok(1)));
    assert!(matches!(next_lane_depth(1, 2), Ok(2)));
    assert!(matches!(
        next_lane_depth(2, 2),
        Err(RunError::DepthExceeded {
            depth: 3,
            max_depth: 2
        })
    ));
}
#[test]
fn lane_continuation_respects_sealed_parent_or_open_fallback() -> TestResult {
    for bound in [
        None,
        Some(NetworkAccess::Provider),
        Some(NetworkAccess::Local),
        Some(NetworkAccess::Open),
    ] {
        for child_store in [false, true] {
            let temp = tempfile::tempdir()?;
            let (clock, cancel) = (clock(), CancelToken::new());
            let call = call(&clock, &cancel);
            let mut store = RunStore::open(&context(temp.path())?, &call)?;
            if child_store {
                store = store.with_writer_registry(&temp.path().join("host"));
            }
            if let Some(network) = bound {
                store = store.with_sealed_parent_network(network);
            }
            let mut proposal = input();
            if !child_store {
                proposal.request.parent = Some(AttemptId::from_bytes([37; 16]));
                proposal.request.parent_attempt = Some(AttemptId::from_bytes([38; 16]));
            }
            let task = &proposal.request.tasks[0];
            assert_eq!(
                store
                    .effective_launch_task(&proposal.request, task)
                    .access
                    .network(false),
                bound.unwrap_or(NetworkAccess::Open)
            );
            let mut narrow = task.clone();
            narrow.access.network = Some(NetworkAccess::Provider);
            assert_eq!(
                store
                    .effective_launch_task(&proposal.request, &narrow)
                    .access
                    .network(false),
                NetworkAccess::Provider
            );
        }
    }
    {
        let expected = NetworkAccess::Open;
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?
            .with_writer_registry(&temp.path().join("host"));
        let prior = input();
        store.create(&prior, &call)?;
        let mut proposal = input();
        proposal.run_id = RunId::from_bytes([35; 16]);
        proposal.operation_id = "lane-continuation".parse()?;
        proposal.attempts[0].attempt_id = AttemptId::from_bytes([36; 16]);
        proposal.request.parent = Some(prior.attempts[0].attempt_id);
        proposal.request.tasks[0].access.network = Some(NetworkAccess::Provider);
        let mut narrower = proposal.request.tasks[0].clone();
        narrower.access.network = Some(NetworkAccess::Provider);
        narrower.access.database_ipc = true;
        assert!(
            !store
                .effective_launch_task(&proposal.request, &narrower)
                .access
                .database_ipc
        );
        assert_eq!(
            store
                .effective_launch_task(&proposal.request, &narrower)
                .access
                .network(false),
            NetworkAccess::Provider
        );
        // A lane-writable request cannot bypass the check by omitting its parent lane.
        proposal.request.parent_attempt = None;
        let run = store.create(&proposal, &call)?;
        let expected_request = proposal.request.clone();
        proposal.request.tasks[0].access.network = Some(NetworkAccess::Open);
        proposal.request.parent = None;
        rewrite_child_request(&store, run.run_id, &proposal.request, &call)?;
        let fake = Fake {
            refuse: true,
            ..Default::default()
        };
        assert!(matches!(
            store.start_checked(
                run.run_id,
                &expected_request,
                &fake,
                &|_, _| unreachable!(),
                &call
            ),
            Err(RunError::Validation(
                "request changed after launch validation"
            ))
        ));
        assert_eq!(fake.prepared.get(), 0);
        store.start(run.run_id, &fake, &|_, _| unreachable!(), &call)?;
        assert_eq!(fake.network.get(), Some(expected));
    }
    Ok(())
}
#[test]
fn priority_selection_failure_uses_compiled_default_and_starts_siblings() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let mut prepared = two_tasks(2);
    prepared.request.tasks[0].build_heavy = true;
    store.create(&prepared, &call)?;
    let first = prepared.attempts[0].attempt_id;
    let sibling = prepared.attempts[1].attempt_id;
    let held = RefCell::new(Vec::new());
    let holder = |record: &AttemptRecord, _| {
        held.borrow_mut()
            .push((record.attempt_id, record.requested_priority));
        Err(RunError::Validation("fixture holder"))
    };
    let started = store.start(
        prepared.run_id,
        &FirstPriorityFailure(prepared.attempts[0].task_id),
        &holder,
        &call,
    )?;
    assert!(matches!(started.as_slice(), [
        Started::Refused { attempt, kind: RefusalKind::Handoff, .. },
        Started::Refused { attempt: second, kind: RefusalKind::Handoff, .. },
    ] if *attempt == first && *second == sibling));
    assert_eq!(
        *held.borrow(),
        [
            (
                first,
                Some(provenant_records::run::DEFAULT_BUILD_HEAVY_PRIORITY)
            ),
            (sibling, Some(5))
        ]
    );
    let index = WorkIndex::open(store.writer_registry(), &call)?;
    let selector = |attempt| WorkSelector {
        project: store.context.project_id,
        run: prepared.run_id,
        task: None,
        attempt: Some(attempt),
        repository: None,
        head: None,
    };
    assert_eq!(
        index.validate(&selector(first), &call)?,
        WorkCoverage::Verified
    );
    assert_eq!(
        index.validate(&selector(sibling), &call)?,
        WorkCoverage::Verified
    );
    assert_eq!(terminal(&store, first, &call)?, Outcome::Rejected);
    Ok(())
}
#[test]
fn launch_records_plan_write_grants_and_git_digest() -> TestResult {
    use provenant_records::run::WriteSource;
    for writer in [false, true] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let context = context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let prepared = input();
        store.create(&prepared, &call)?;
        let id = prepared.attempts[0].attempt_id;
        assert!(matches!(
            store.start(
                prepared.run_id,
                &Confined { writer, tool_writes: vec![] },
                &|_, _| Err(RunError::Validation("fixture holder")),
                &call,
            )?.as_slice(),
            [Started::Refused { attempt, kind: RefusalKind::Handoff, .. }]
                if *attempt == id
        ));
        drop(store);
        let store = RunStore::open(&context, &call)?;
        let record = store.record(id, &call)?;
        let mut expected = vec![
            (
                WriteSource::StateWrite,
                "/fixture/state/session",
                PathScope::Exact,
            ),
            (
                WriteSource::WriteRoot,
                "/fixture/shared",
                PathScope::Subtree,
            ),
            (WriteSource::Scratch, "/fixture/scratch", PathScope::Subtree),
            (
                WriteSource::PrivateState,
                "/fixture/private",
                PathScope::Subtree,
            ),
        ];
        if writer {
            expected.push((WriteSource::LaneCache, "/fixture/cache", PathScope::Subtree));
        }
        let expected = expected
            .into_iter()
            .map(|(source, path, scope)| {
                Ok(WriteGrantRecord {
                    extra: Default::default(),
                    source,
                    path: provenant_records::AbsolutePath::new(std::path::Path::new(path))?,
                    scope,
                })
            })
            .collect::<std::result::Result<Vec<_>, Box<dyn std::error::Error>>>()?;
        assert_eq!(record.write_grants, expected);
        assert_eq!(
            record.git_grants_digest,
            writer.then(|| provenant_core::Digest::from_bytes([7; 32]))
        );
    }
    Ok(())
}
#[test]
fn oversized_write_grants_reach_handoff_without_stalling_sibling() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let prepared = two_tasks(2);
    store.create(&prepared, &call)?;
    let id = prepared.attempts[0].attempt_id;
    let sibling = prepared.attempts[1].attempt_id;
    let provider = FirstOversized {
        first: prepared.attempts[0].task_id,
        oversized: Confined {
            writer: false,
            tool_writes: (0..128)
                .map(|i| PathGrant {
                    path: format!("/fixture/tools/{i}/{}", "x".repeat(3000)).into(),
                    scope: PathScope::Subtree,
                })
                .collect(),
        },
        ordinary: Confined {
            writer: false,
            tool_writes: vec![],
        },
    };
    let held = RefCell::new(Vec::new());
    let holder = |record: &AttemptRecord, _| {
        held.borrow_mut().push(record.attempt_id);
        Err(RunError::Validation("fixture holder"))
    };
    let started = store.start(prepared.run_id, &provider, &holder, &call)?;
    assert!(matches!(
        started.as_slice(),
        [
            Started::Refused { attempt: first, kind: RefusalKind::Handoff, .. },
            Started::Refused { attempt: second, kind: RefusalKind::Handoff, .. },
        ] if *first == id && *second == sibling
    ));
    // Both attempts reached the fixture handoff.
    assert_eq!(*held.borrow(), [id, sibling]);
    drop(store);
    let store = RunStore::open(&context, &call)?;
    let record = store.record(id, &call)?;
    assert!(matches!(
        record.state,
        AttemptState::Terminal {
            outcome: Outcome::Rejected,
            ..
        }
    ));
    assert!(
        provenant_records::CanonicalV1::bytes(&record.write_grants)?
            .as_bytes()
            .len()
            > provenant_records::run::MAX_WRITE_GRANT_BYTES as usize
    );
    assert!(
        store
            .start(prepared.run_id, &provider, &|_, _| unreachable!(), &call)?
            .is_empty()
    );
    Ok(())
}
#[test]
fn host_launch_uses_the_recorded_requester() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let prepared = input();
    let record = store.create(&prepared, &call)?;
    let requester = store.run(record.run_id, &call)?.actor_id;
    let seen = Cell::new(None);
    let started = store.start(
        record.run_id,
        &Ready,
        &|_, actor| {
            seen.set(Some(actor));
            Err(RunError::Validation("holder observed"))
        },
        &call,
    )?;
    assert!(matches!(started.as_slice(), [Started::Refused { .. }]));
    assert_eq!(seen.get(), Some(requester));
    Ok(())
}
#[test]
fn memory_pressure_threshold_controls_admission_and_waits() -> TestResult {
    use provenant_os::sys::resources::MemoryPressure;
    struct Pressured(MemoryPressure);
    impl Resources for Pressured {
        fn guardian_identity(&self) -> Option<provenant_supervisor::census::ProcessIdentity> {
            crate::tests::HostFacts::new(100).guardian_identity()
        }
        fn observe_process(&self, pid: i64) -> provenant_supervisor::census::ProcessObservation {
            crate::tests::HostFacts::new(100).observe_process(pid)
        }
        fn memory(&self) -> std::io::Result<MemorySample> {
            HostFacts::new(100).memory()
        }
        fn pressure(
            &self,
        ) -> std::io::Result<Option<provenant_os::sys::resources::MemoryPressure>> {
            Ok(Some(self.0))
        }
        fn disk(&self, _: &Directory) -> std::io::Result<u64> {
            Ok(u64::MAX)
        }
    }
    for (threshold, level, writer, defers) in [
        (None, MemoryPressure::Warning, true, false),
        (None, MemoryPressure::Critical, false, true),
        (None, MemoryPressure::Normal, true, false),
        (Some("warning"), MemoryPressure::Warning, false, true),
        (Some("warning"), MemoryPressure::Critical, true, true),
        (Some("warning"), MemoryPressure::Normal, false, false),
        (Some("never"), MemoryPressure::Critical, true, false),
    ] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let mut store = RunStore::open(&context(temp.path())?, &call)?;
        if let Some(threshold) = threshold {
            std::fs::create_dir_all(&store.writer_registry)?;
            std::fs::write(
                store.writer_registry.join("config.toml"),
                format!(
                    "schema_version = 1\n[domains.fabric.lanes]\nmemory_pressure_threshold = '{threshold}'\n"
                ),
            )?;
        }
        store.set_admission_policy(AdmissionPolicy {
            admission_seconds: 1,
            ..AdmissionPolicy::default()
        });
        let run = lane(54, writer);
        let id = starting(&store, &run, &call)?;
        let attempt = store.attempt_dir(run.run_id, id, false)?;
        let waits =
            store.lane_waits_with_resources(&[store.record(id, &call)?], &call, &Pressured(level));
        assert_eq!(
            waits[0]
                .waiting_for
                .contains(&format!("memory pressure {level:?}")),
            defers
        );
        let result = store.admit(id, &attempt, &Pressured(level), &clock, &cancel);
        if !defers {
            assert_eq!(
                result?.state,
                AttemptState::Running {
                    extra: Default::default()
                }
            );
        } else {
            let Err(RunError::Admission(AdmissionError::MemoryPressure {
                level: actual,
                threshold: selected,
                origin,
            })) = result
            else {
                return Err("memory pressure was not refused".into());
            };
            assert_eq!(format!("{actual:?}"), format!("{level:?}"));
            assert_eq!(
                origin,
                if threshold.is_some() {
                    provenant_records::diagnostic::SettingSource::Configuration
                } else {
                    provenant_records::diagnostic::SettingSource::ShippedDefault
                }
            );
            assert_eq!(
                selected,
                if threshold == Some("warning") {
                    provenant_records::diagnostic::PressureThreshold::Warning
                } else {
                    provenant_records::diagnostic::PressureThreshold::Critical
                }
            );
        }
    }
    Ok(())
}
#[test]
fn admission_refuses_near_the_user_process_limit() -> TestResult {
    use provenant_os::sys::resources::ProcessBudget;
    struct Processes(ProcessBudget);
    impl Resources for Processes {
        fn guardian_identity(&self) -> Option<provenant_supervisor::census::ProcessIdentity> {
            crate::tests::HostFacts::new(100).guardian_identity()
        }
        fn observe_process(&self, pid: i64) -> provenant_supervisor::census::ProcessObservation {
            crate::tests::HostFacts::new(100).observe_process(pid)
        }
        fn memory(&self) -> std::io::Result<MemorySample> {
            HostFacts::new(100).memory()
        }
        fn process_budget(&self) -> std::io::Result<ProcessBudget> {
            Ok(self.0)
        }
        fn disk(&self, _: &Directory) -> std::io::Result<u64> {
            Ok(u64::MAX)
        }
    }
    for (used, limit, full) in [(2601, 2666, true), (2666, 2666, true), (100, 2666, false)] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let mut store = RunStore::open(&context(temp.path())?, &call)?;
        store.set_admission_policy(AdmissionPolicy {
            admission_seconds: 1,
            ..AdmissionPolicy::default()
        });
        let run = lane(55, true);
        let id = starting(&store, &run, &call)?;
        let attempt = store.attempt_dir(run.run_id, id, false)?;
        let resources = Processes(ProcessBudget { used, limit });
        let waits = store.lane_waits_with_resources(&[store.record(id, &call)?], &call, &resources);
        assert_eq!(
            waits[0]
                .waiting_for
                .contains(&format!("user process count {used} of {limit}")),
            full
        );
        let result = store.admit(id, &attempt, &resources, &clock, &cancel);
        if full {
            assert!(matches!(
                result,
                Err(RunError::Admission(AdmissionError::Processes { used: u, limit: l }))
                    if u == used && l == limit
            ));
        } else {
            assert_eq!(
                result?.state,
                AttemptState::Running {
                    extra: Default::default()
                }
            );
        }
    }
    Ok(())
}
#[test]
fn pressure_errors_and_low_advisory_memory_do_not_refuse() -> TestResult {
    use provenant_os::sys::resources::MemoryPressure;
    struct Probe {
        available: u64,
        error: bool,
        pressure: MemoryPressure,
    }
    impl Resources for Probe {
        fn guardian_identity(&self) -> Option<provenant_supervisor::census::ProcessIdentity> {
            crate::tests::HostFacts::new(100).guardian_identity()
        }
        fn observe_process(&self, pid: i64) -> provenant_supervisor::census::ProcessObservation {
            crate::tests::HostFacts::new(100).observe_process(pid)
        }
        fn memory(&self) -> std::io::Result<MemorySample> {
            HostFacts::new(self.available).memory()
        }
        fn pressure(
            &self,
        ) -> std::io::Result<Option<provenant_os::sys::resources::MemoryPressure>> {
            if self.error {
                Err(std::io::Error::other("unavailable"))
            } else {
                Ok(Some(self.pressure))
            }
        }
        fn disk(&self, _: &Directory) -> std::io::Result<u64> {
            Ok(u64::MAX)
        }
    }
    for (available, error, threshold, pressure, admitted) in [
        (100, true, None, MemoryPressure::Normal, true),
        (4, false, None, MemoryPressure::Normal, true),
        (4, false, Some("never"), MemoryPressure::Critical, true),
    ] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let mut store = RunStore::open(&context(temp.path())?, &call)?;
        if let Some(threshold) = threshold {
            std::fs::create_dir_all(&store.writer_registry)?;
            std::fs::write(
                store.writer_registry.join("config.toml"),
                format!(
                    "schema_version = 1\n[domains.fabric.lanes]\nmemory_pressure_threshold = '{threshold}'\n"
                ),
            )?;
        }
        store.set_admission_policy(AdmissionPolicy {
            admission_seconds: 1,
            ..AdmissionPolicy::default()
        });
        let run = lane(58, true);
        let id = starting(&store, &run, &call)?;
        let attempt = store.attempt_dir(run.run_id, id, false)?;
        let result = store.admit(
            id,
            &attempt,
            &Probe {
                available,
                error,
                pressure,
            },
            &clock,
            &cancel,
        );
        assert_eq!(result.is_ok(), admitted);
    }
    Ok(())
}
#[test]
fn out_of_range_build_settings_do_not_block_admission() -> TestResult {
    for (assignment, explicit_policy) in [
        ("cargo_build_jobs = -2", false),
        ("cargo_build_jobs = 'invalid'", false),
        ("max_concurrent_builds = 0", true),
    ] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let mut store = RunStore::open(&context(temp.path())?, &call)?;
        if explicit_policy {
            store.set_admission_policy(AdmissionPolicy {
                startup_spacing_ms: 0,
                admission_seconds: 1,
                ..AdmissionPolicy::default()
            });
        }
        let run = lane(69, true);
        let id = starting(&store, &run, &call)?;
        let attempt = store.attempt_dir(run.run_id, id, false)?;
        std::fs::write(
            store.writer_registry.join("config.toml"),
            format!("schema_version=1\n[domains.fabric.lanes]\n{assignment}\n"),
        )?;
        let result = store.admit(id, &attempt, &HostFacts::new(100), &clock, &cancel);
        if explicit_policy {
            assert_eq!(
                result?.state,
                AttemptState::Running {
                    extra: Default::default()
                }
            );
        } else {
            assert!(result.is_ok(), "{assignment}");
        }
    }
    Ok(())
}
#[test]
fn guardian_identity_unknown_retries_and_names_expiry() -> TestResult {
    struct Observation {
        remaining: Cell<u32>,
    }
    impl Resources for Observation {
        fn observe_process(&self, pid: i64) -> provenant_supervisor::census::ProcessObservation {
            crate::tests::HostFacts::new(100).observe_process(pid)
        }
        fn memory(&self) -> std::io::Result<MemorySample> {
            HostFacts::new(100).memory()
        }
        fn disk(&self, _: &Directory) -> std::io::Result<u64> {
            Ok(u64::MAX)
        }
        fn guardian_identity(&self) -> Option<provenant_supervisor::census::ProcessIdentity> {
            let remaining = self.remaining.get();
            if remaining > 0 {
                self.remaining.set(remaining - 1);
                None
            } else {
                crate::tests::HostFacts::new(100).guardian_identity()
            }
        }
    }
    for (failures, admitted) in [(2, true), (100, false)] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let mut store = RunStore::open(&context(temp.path())?, &call)?;
        store.set_admission_policy(AdmissionPolicy {
            startup_spacing_ms: 0,
            admission_seconds: 1,
            ..AdmissionPolicy::default()
        });
        let run = lane(70, true);
        let id = starting(&store, &run, &call)?;
        let attempt = store.attempt_dir(run.run_id, id, false)?;
        let result = store.admit(
            id,
            &attempt,
            &Observation {
                remaining: Cell::new(failures),
            },
            &clock,
            &cancel,
        );
        if admitted {
            assert_eq!(
                result?.state,
                AttemptState::Running {
                    extra: Default::default()
                }
            );
        } else {
            assert!(matches!(
                result,
                Err(RunError::Admission(AdmissionError::GuardianIdentity))
            ));
        }
    }
    Ok(())
}
#[test]
fn idle_writers_are_admitted_beyond_the_build_limit() -> TestResult {
    // `max_concurrent_builds` gates builds inside lanes, never writer lanes.
    for limit in [None, Some(1)] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        if let Some(limit) = limit {
            std::fs::write(
                store.writer_registry.join("config.toml"),
                format!(
                    "schema_version=1\n[domains.fabric.lanes]\nmax_concurrent_builds = {limit}\n"
                ),
            )?;
        }
        for seed in 1..=70 {
            let run = lane(seed, true);
            let id = starting(&store, &run, &call)?;
            let dir = store.attempt_dir(run.run_id, id, false)?;
            assert_eq!(
                store
                    .admit(id, &dir, &HostFacts::new(100), &clock, &cancel)?
                    .state,
                AttemptState::Running {
                    extra: Default::default()
                },
                "writer {seed} with limit {limit:?}"
            );
        }
    }
    Ok(())
}
#[test]
fn the_writer_cap_refuses_at_its_limit_and_names_it() -> TestResult {
    for (config, scope) in [
        ("max_writer_lanes = 3\nmax_project_writer_lanes = 3", "host"),
        (
            "max_writer_lanes = 8\nmax_project_writer_lanes = 3",
            "project",
        ),
    ] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let mut store = RunStore::open(&context(temp.path())?, &call)?;
        store.set_admission_policy(AdmissionPolicy {
            startup_spacing_ms: 0,
            admission_seconds: 1,
            ..AdmissionPolicy::default()
        });
        std::fs::write(
            store.writer_registry.join("config.toml"),
            format!("schema_version=1\n[domains.fabric.lanes]\n{config}\n"),
        )?;
        let facts = super::tests::HostFacts::new(100);
        let mut last = None;
        for seed in 80..84 {
            let run = lane(seed, true);
            let id = starting(&store, &run, &call)?;
            let dir = store.attempt_dir(run.run_id, id, false)?;
            last = Some(store.admit(id, &dir, &facts, &clock, &cancel));
            if seed < 83 {
                assert_eq!(
                    last.take().ok_or("admitted")??.state,
                    AttemptState::Running {
                        extra: Default::default()
                    },
                    "writer {seed} is inside the cap"
                );
            }
        }
        match last.ok_or("a fourth writer")? {
            Err(
                error @ RunError::Admission(AdmissionError::LaneCap {
                    limit: 3,
                    kind: refused,
                    origin,
                    ..
                }),
            ) => {
                assert_eq!(error.code(), "LANE_CAP");
                assert_eq!(
                    refused,
                    if scope == "host" {
                        provenant_records::diagnostic::LaneLimit::HostWriters
                    } else {
                        provenant_records::diagnostic::LaneLimit::ProjectWriters
                    }
                );
                assert_eq!(
                    origin,
                    provenant_records::diagnostic::SettingSource::Configuration
                );
                // The message names the configuration key to raise.
                let key = if scope == "project" {
                    "max_project_writer_lanes"
                } else {
                    "max_writer_lanes"
                };
                assert!(error.to_string().contains(key), "{error}");
            }
            other => return Err(format!("the fourth writer is refused: {other:?}").into()),
        }
        assert!(
            facts.observations() > 0,
            "cap checks use the injected process table"
        );
    }
    Ok(())
}
#[test]
fn policy_refusal_skips_provider_preparation_and_preserves_sibling_progress() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let batch = two_tasks(2);
    store.create(&batch, &call)?;
    let providers = Fake {
        refuse: true,
        policy_task: Some(batch.request.tasks[0].task_id),
        ..Default::default()
    };
    let started = store.start(batch.run_id, &providers, &|_, _| unreachable!(), &call)?;
    assert_eq!(providers.prepared.get(), 1);
    assert!(matches!(started.as_slice(), [
        Started::Refused { attempt: first, kind: RefusalKind::Policy, .. },
        Started::Refused { attempt: second, kind: RefusalKind::Preparation, .. },
    ] if *first == batch.attempts[0].attempt_id && *second == batch.attempts[1].attempt_id));
    for attempt in &batch.attempts {
        assert_eq!(
            terminal(&store, attempt.attempt_id, &call)?,
            Outcome::Rejected
        );
    }
    Ok(())
}
#[test]
fn policy_resource_refusal_retains_fact_and_skips_preparation() -> TestResult {
    use provenant_records::diagnostic::{ResourceFact, SettingSource};
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let request = input();
    store.create(&request, &call)?;
    let fact = ResourceFact::Disk {
        free: 1,
        floor: 2,
        origin: SettingSource::Configuration,
    };
    let provider = Fake {
        policy_task: Some(request.request.tasks[0].task_id),
        policy_resource: Some(fact),
        ..Default::default()
    };
    let started = store.start(request.run_id, &provider, &|_, _| unreachable!(), &call)?;
    assert!(
        matches!(started.as_slice(), [Started::Refused { kind: RefusalKind::PolicyResource(actual), .. }] if *actual == fact)
    );
    assert_eq!(provider.prepared.get(), 0);
    let diagnostic = RefusalKind::PolicyResource(fact).diagnostic().unwrap();
    assert_eq!(diagnostic.code, "DISK_FLOOR");
    let detail = diagnostic.detail.unwrap();
    assert!(detail.contains("gc.free_floor"));
    assert!(detail.contains("host configuration"));
    let inventory =
        include_str!("../assets/launch-guards.toml").parse::<toml_edit::DocumentMut>()?;
    assert!(
        inventory["guard"]
            .as_array_of_tables()
            .unwrap()
            .iter()
            .any(|row| {
                row.get("site").and_then(|v| v.as_str())
                    == Some("run/src/launch.rs#Providers::check_policy")
                    && row.get("code").and_then(|v| v.as_str()) == Some(diagnostic.code.as_str())
                    && row.get("boundary").and_then(|v| v.as_str()) == Some("configurable_default")
                    && row
                        .get("setting")
                        .and_then(|v| v.as_str())
                        .is_some_and(|setting| detail.contains(setting))
            })
    );
    Ok(())
}
#[test]
fn a_stop_at_the_runtime_deadline_settles_as_a_typed_timeout() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    store.create(&run, &call)?;
    let id = run.attempts[0].attempt_id;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let dir = store.attempt_dir(run.run_id, id, false)?;
    // A hung provider's partial output, then the guardian's stop and kill.
    artifacts::seal(&dir, "stdout.log", b"half an answer", CAPTURE_LIMIT)?;
    let mut observed = exit();
    observed.scope = RunStore::scope(&store.record(id, &call)?);
    observed.termination = Some(Termination::Signal(9));
    observed.stop_reason = Some(provenant_supervisor::machine::StopReason::WallDeadline);
    seal_exit(&store, &observed)?;
    let fake = Fake {
        observed: Some(route()),
        outcome: Some(Outcome::Failed),
        ..Fake::default()
    };
    let record = store.finish(id, &dir, &observed, &fake, &call)?;
    let AttemptState::Terminal {
        outcome, reason, ..
    } = record.state
    else {
        return Err("not terminal".into());
    };
    assert_eq!(outcome, Outcome::TimedOut);
    assert_eq!(
        reason.as_deref(),
        Some("runtime deadline exceeded: provider stopped after 3600s")
    );
    Ok(())
}

#[test]
fn dozen_writers_share_admission_without_default_limits_or_spacing() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let mut attempts = Vec::new();
    for seed in 1..=12 {
        attempts.push(starting(&store, &lane(seed, true), &call)?);
    }
    let barrier = std::sync::Barrier::new(attempts.len());
    std::thread::scope(|scope| -> TestResult {
        let handles: Vec<_> = attempts
            .iter()
            .map(|id| {
                let context = &context;
                let barrier = &barrier;
                scope.spawn(move || -> std::result::Result<(), String> {
                    let (clock, cancel) = (crate::tests::clock(), CancelToken::new());
                    let call = crate::tests::call(&clock, &cancel);
                    let store = RunStore::open(context, &call).map_err(|error| error.to_string());
                    barrier.wait();
                    let store = store?;
                    let record = store
                        .claim_observed(*id, &clock, &call, &HostFacts::new(100))
                        .map_err(|error| error.to_string())?
                        .ok_or(
                            "writer admission waited despite free worktree and default policy",
                        )?;
                    assert!(matches!(record.state, AttemptState::Running { .. }));
                    Ok(())
                })
            })
            .collect();
        for handle in handles {
            handle
                .join()
                .map_err(|_| "writer admission panicked")?
                .map_err(|error| format!("writer admission: {error}"))?;
        }
        Ok(())
    })?;
    for id in attempts {
        assert!(matches!(
            store.record(id, &call)?.state,
            AttemptState::Running { .. }
        ));
    }
    Ok(())
}

#[test]
fn ledger_observes_without_locking_and_reclaims_without_a_new_guardian() -> TestResult {
    struct Probe {
        ledger: std::path::PathBuf,
        run_store: std::path::PathBuf,
        facts: HostFacts,
        guardian: Cell<bool>,
    }
    impl Probe {
        fn require_free_ledger(&self) {
            for path in [&self.ledger, &self.run_store] {
                let db = rusqlite::Connection::open(path).unwrap();
                db.busy_timeout(std::time::Duration::ZERO).unwrap();
                db.execute_batch("BEGIN IMMEDIATE; ROLLBACK;")
                    .expect("resource observation must leave admission unlocked");
            }
        }
    }
    impl Resources for Probe {
        fn memory(&self) -> std::io::Result<MemorySample> {
            self.facts.memory()
        }
        fn disk(&self, attempt: &Directory) -> std::io::Result<u64> {
            self.facts.disk(attempt)
        }
        fn guardian_identity(&self) -> Option<provenant_supervisor::census::ProcessIdentity> {
            self.require_free_ledger();
            self.guardian
                .get()
                .then(|| self.facts.guardian_identity())
                .flatten()
        }
        fn observe_process(&self, pid: i64) -> provenant_supervisor::census::ProcessObservation {
            self.require_free_ledger();
            self.facts.observe_process(pid)
        }
    }
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let id = starting(&store, &lane(88, true), &call)?;
    let raw: Vec<u8> = store.db.query_row(
        "SELECT record FROM admission WHERE attempt=?1",
        [id.to_string()],
        |row| row.get(0),
    )?;
    let ticket: AdmissionTicket = crate::db::decode(&raw)?;
    let ledger = crate::ledger::open(&store.writer_registry, &call)?;
    let probe = Probe {
        ledger: store.writer_registry.join("admission.sqlite3"),
        run_store: store.context.state_root.as_path().join("run.sqlite3"),
        facts: HostFacts::new(100),
        guardian: Cell::new(true),
    };
    let caps = crate::ledger::LaneCaps::of(&Default::default());
    let claim = || {
        let owner = crate::ledger::prepare_lease(
            &ledger,
            store.context.state_root.as_path(),
            &ticket,
            caps,
            &clock,
            &probe,
        )?;
        crate::ledger::lease(
            &ledger,
            store.context.state_root.as_path(),
            &ticket,
            caps,
            &clock,
            &call,
            &owner,
        )
    };
    assert!(claim()?.is_some());
    // Retry and relay reclaim preserve the original guardian identity.
    probe.guardian.set(false);
    assert!(claim()?.is_some());
    crate::ledger::yield_lease(&ledger, store.context.state_root.as_path(), id, &call)?;
    assert!(claim()?.is_some());
    probe.guardian.set(true);
    let other = starting(&store, &lane(89, true), &call)?;
    assert!(
        store
            .claim_observed(other, &clock, &call, &probe)?
            .is_some()
    );
    assert!(store.set_child_wait_observed(other, true, &call, &probe)?);
    probe.guardian.set(false);
    assert!(store.set_child_wait_observed(other, false, &call, &probe)?);
    Ok(())
}

/// Listing must touch directories for the rows it shows, not for the retained
/// history of the host. The count is of directory enumerations, not time.
#[test]
fn listing_scans_are_bounded_by_the_page_not_by_retained_history() -> TestResult {
    let mut scans = Vec::new();
    for retained in [3_usize, 300] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let input = input();
        store.create(&input, &call)?;
        let mut record = store.record(input.attempts[0].attempt_id, &call)?;
        record.state = AttemptState::Terminal {
            extra: Default::default(),
            outcome: Outcome::Succeeded,
            exit_code: Some(0),
            finished_at: record.queued_at,
            reason: None,
        };
        record.provider_launched = Some(false);
        // Retained history: other runs' attempt directories and other
        // worktrees' reservations, none of which belong to the listed row.
        let worktrees = store
            .writer_registry
            .join("worktrees")
            .join(record.project_id.to_string());
        for index in 0..retained {
            let attempt = format!("{index:032x}");
            std::fs::create_dir_all(
                temp.path()
                    .join(format!("runs/{attempt}/attempts/{attempt}")),
            )?;
            let slot = worktrees.join(format!("slot-{index}")).join(&attempt);
            std::fs::create_dir_all(&slot)?;
            std::fs::write(slot.join("reservation.json"), "{")?;
        }
        let before = provenant_os::fs::directory_scans();
        let page = store.lanes(
            &LaneQuery {
                targets: vec![],
                all: true,
                limit: 1,
                after: 0,
                newest_first: true,
                owner: None,
                unseen_after: None,
            },
            &call,
        )?;
        assert_eq!(page.lanes.len(), 1);
        assert!(store.custody_released(&record, &call)?);
        let attempt = store.attempt_dir(record.run_id, record.attempt_id, false)?;
        let _ = store.supervisor_stalled(&attempt, RunStore::scope(&record), &call);
        scans.push(provenant_os::fs::directory_scans() - before);
    }
    assert_eq!(scans[0], scans[1], "scans grew with retained history");
    assert_eq!(
        scans[1], 0,
        "a displayed reader row needs no directory scan"
    );
    Ok(())
}
