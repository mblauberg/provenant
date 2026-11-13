#[cfg(test)]
#[test]
fn peer_remote_base_pins_a_commit_reachable_from_the_remote()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::worktrees::{self, Occupancy, WorkerObserver};
    use std::collections::BTreeMap;
    struct Free;
    impl WorkerObserver for Free {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            roots
                .iter()
                .cloned()
                .map(|path| (path, Occupancy::Available))
                .collect()
        }
    }
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let project = temp.path().join("project");
    let remote = temp.path().join("remote.git");
    for path in [&home, &state, &project, &remote] {
        std::fs::create_dir_all(path)?;
    }
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;
    provenant_os::fixture::run_git(&remote, &["init", "--bare", "-q"])?;
    provenant_os::fixture::run_git(&project, &["init", "-q"])?;
    provenant_os::fixture::run_git(&project, &["commit", "--allow-empty", "-qm", "base"])?;
    provenant_os::fixture::run_git(
        &project,
        &[
            "remote",
            "add",
            "origin",
            remote.to_str().ok_or("remote path")?,
        ],
    )?;
    provenant_os::fixture::run_git(&project, &["push", "origin", "HEAD:refs/heads/main"])?;
    let initial = String::from_utf8(provenant_os::fixture::run_git(
        &project,
        &["rev-parse", "HEAD"],
    )?)?;
    let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
    inputs.state_dir = Some(state);
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    worktrees::with_context_using_workers(
        &roots,
        &home,
        &project,
        &clock,
        &cancel,
        &Free,
        |context| {
            let pinned = remote_base(
                context,
                "HEAD",
                std::time::Duration::from_secs(30),
                true,
                &cancel,
            )
            .map_err(|_| worktrees::WorktreeError::InvalidOutput)?;
            assert_eq!(pinned.to_string(), initial.trim());
            Ok(())
        },
    )?;
    let absent = temp.path().join("absent.git");
    provenant_os::fixture::run_git(
        &project,
        &[
            "remote",
            "add",
            "a-unavailable",
            absent.to_str().ok_or("absent path")?,
        ],
    )?;
    worktrees::with_context_using_workers(
        &roots,
        &home,
        &project,
        &clock,
        &cancel,
        &Free,
        |context| {
            assert_eq!(
                remote_base(
                    context,
                    "HEAD",
                    std::time::Duration::from_secs(30),
                    false,
                    &cancel
                )
                .map_err(|_| worktrees::WorktreeError::InvalidOutput)?
                .to_string(),
                initial.trim()
            );
            Ok(())
        },
    )?;
    provenant_os::fixture::run_git(&project, &["remote", "remove", "a-unavailable"])?;
    // Fetch leaves a deleted remote-tracking ref behind; it must not make the
    // old tip appear to remain advertised by the remote.
    provenant_os::fixture::run_git(&remote, &["update-ref", "-d", "refs/heads/main"])?;
    worktrees::with_context_using_workers(
        &roots,
        &home,
        &project,
        &clock,
        &cancel,
        &Free,
        |context| {
            assert!(matches!(
                remote_base(
                    context,
                    "HEAD",
                    std::time::Duration::from_secs(30),
                    true,
                    &cancel
                ),
                Err(FacadeError::BaseUnreachable)
            ));
            Ok(())
        },
    )?;
    provenant_os::fixture::run_git(&remote, &["update-ref", "refs/heads/main", initial.trim()])?;
    let limited = worktrees::RepoFacts::with_limits(provenant_os::exec::DEFAULT_CAPTURE_LIMIT, 1)?;
    worktrees::with_context_using_workers(
        &roots,
        &home,
        &project,
        &clock,
        &cancel,
        &Free,
        |context| {
            let context = &worktrees::WorktreeContext {
                facts: &limited,
                ..*context
            };
            let error = remote_base(
                context,
                "HEAD",
                std::time::Duration::from_secs(30),
                false,
                &cancel,
            )
            .expect_err("bounded fact output must be reported");
            assert!(matches!(
                &error,
                FacadeError::Io { operation, .. }
                    if operation == provenant_fabric_runtime::coordination::facade::REPOSITORY_FACT_LIMIT_OPERATION
            ));
            let wire = serde_json::to_value(&error)
                .map_err(|_| worktrees::WorktreeError::InvalidOutput)?;
            assert_eq!(wire["operation"], "repository fact output limit exceeded");
            let cancelled = CancelToken::new();
            cancelled.cancel();
            assert!(matches!(
                remote_base(
                    context,
                    "HEAD",
                    std::time::Duration::from_secs(30),
                    false,
                    &cancelled
                ),
                Err(FacadeError::Cancelled)
            ));
            Ok(())
        },
    )?;
    // A replacement object must not make a local-only root look reachable
    // from the real remote-tracking commit.
    let tree = String::from_utf8(provenant_os::fixture::run_git(
        &project,
        &["rev-parse", "HEAD^{tree}"],
    )?)?;
    let local = String::from_utf8(provenant_os::fixture::run_git(
        &project,
        &["commit-tree", tree.trim(), "-m", "local only"],
    )?)?;
    let replacement = String::from_utf8(provenant_os::fixture::run_git(
        &project,
        &[
            "commit-tree",
            tree.trim(),
            "-p",
            local.trim(),
            "-m",
            "poisoned ancestry",
        ],
    )?)?;
    provenant_os::fixture::run_git(&project, &["update-ref", "HEAD", local.trim()])?;
    provenant_os::fixture::run_git(&project, &["replace", initial.trim(), replacement.trim()])?;
    let contains = format!("--contains={}", local.trim());
    let poisoned = provenant_os::fixture::run_git(
        &project,
        &[
            "for-each-ref",
            "--format=%(refname)",
            &contains,
            "refs/remotes",
        ],
    )?;
    assert!(
        !poisoned.is_empty(),
        "fixture must poison ordinary reachability"
    );
    worktrees::with_context_using_workers(
        &roots,
        &home,
        &project,
        &clock,
        &cancel,
        &Free,
        |context| {
            assert!(matches!(
                remote_base(
                    context,
                    "HEAD",
                    std::time::Duration::from_secs(30),
                    false,
                    &cancel
                ),
                Err(FacadeError::BaseUnreachable)
            ));
            provenant_os::fixture::run_git(&project, &["replace", "-d", initial.trim()])
                .map_err(|_| worktrees::WorktreeError::InvalidOutput)?;
            let grafts = project.join(".git/info/grafts");
            std::fs::write(&grafts, format!("{} {}\n", initial.trim(), local.trim()))
                .map_err(|_| worktrees::WorktreeError::InvalidOutput)?;
            let poisoned = provenant_os::fixture::run_git(
                &project,
                &[
                    "for-each-ref",
                    "--format=%(refname)",
                    &contains,
                    "refs/remotes",
                ],
            )
            .map_err(|_| worktrees::WorktreeError::InvalidOutput)?;
            assert!(
                !poisoned.is_empty(),
                "fixture must poison ordinary reachability"
            );
            assert!(matches!(
                remote_base(
                    context,
                    "HEAD",
                    std::time::Duration::from_secs(30),
                    false,
                    &cancel
                ),
                Err(FacadeError::BaseUnreachable)
            ));
            std::fs::remove_file(grafts).map_err(|_| worktrees::WorktreeError::InvalidOutput)?;
            std::fs::rename(&project, temp.path().join("moved-project"))
                .map_err(|_| worktrees::WorktreeError::InvalidOutput)?;
            std::fs::create_dir(&project).map_err(|_| worktrees::WorktreeError::InvalidOutput)?;
            assert!(matches!(
                remote_base(
                    context,
                    "HEAD",
                    std::time::Duration::from_secs(30),
                    true,
                    &cancel
                ),
                Err(FacadeError::BaseUnreachable)
            ));
            Ok(())
        },
    )?;
    Ok(())
}

#[cfg(test)]
#[test]
fn graph_document_decoder_checks_header_before_typed_graph() {
    for version in [1, 2] {
        let source = format!(r#"{{"schema_version":{version},"graph_id":"test","nodes":[]}}"#);
        let document = decode_graph_document(source.as_bytes()).unwrap();
        assert_eq!(document.schema_version, version);
    }
    assert!(matches!(
        decode_graph_document(br#"{"schema_version":3,"nodes":"future shape"}"#),
        Err(FacadeError::UpgradeRequired { version: 3, .. })
    ));
    assert!(decode_graph_document(br#"{"schema_version":1,"schema_version":2}"#).is_err());
    assert!(decode_graph_document(br#"{"schema_version":1,"nodes":"invalid"}"#).is_err());
    assert!(decode_graph_document(br#"{"graph_id":"test","nodes":[]}"#).is_err());
}

#[cfg(test)]
#[test]
fn cli_failure_uses_stderr_without_structured_json() {
    let error = cli_failure(b"not json", b"specific failure\nadditional detail\n");
    assert!(matches!(error, ServiceError::Cli { code, message, data }
        if code == "CLI_FAILED" && message == "specific failure" && data.is_null()));
}

#[cfg(test)]
#[test]
fn scratch_prepare_lock_timeout_keeps_a_fixed_cause() {
    use provenant_confinement::worktrees::WorktreeError;
    let error = scratch_prepare_error(WorktreeError::Filesystem(
        provenant_os::fs::FsError::LockTimeout {
            path: "info/provenant-exclude.lock".into(),
            timeout: std::time::Duration::ZERO,
        },
    ));
    assert!(error.to_string().contains("prepare project scratch"));
    assert!(error.to_string().contains(".provenant/scratch"));
    assert_eq!(error.code(), "io");
    assert!(error.cause().is_some());
    let projection = error.diagnostic();
    assert_eq!(projection.detail.as_deref(), error.cause());
    assert!(matches!(error, FacadeError::Io { path: Some(path), .. }
        if path == ".provenant/scratch"));
}

#[test]
fn prompt_limit_refusal_keeps_safe_field_and_existing_code() {
    let error = run_error(provenant_run::RunError::PromptTooLarge { limit: 1_048_576 });
    let projection = error.diagnostic();
    assert_eq!(projection.code, "invalid_input");
    let detail = projection.detail.unwrap();
    assert_eq!(
        detail,
        "invalid input field `prompt`: exceeds 1048576 bytes"
    );
    assert_eq!(
        projection.next.as_deref(),
        Some("reduce the prompt input before retrying")
    );
}

#[test]
fn dirty_writer_refusal_keeps_safe_cause_and_action_on_the_wire() {
    let error = run_error(provenant_run::RunError::Writer(
        provenant_confinement::worktrees::WorktreeError::Dirty("TOKEN_SENTINEL".into()),
    ));
    let decoded: FacadeError =
        serde_json::from_slice(&serde_json::to_vec(&error).unwrap()).unwrap();
    let diagnostic = decoded.diagnostic();
    assert!(
        diagnostic
            .detail
            .as_deref()
            .unwrap()
            .contains("uncommitted")
    );
    assert!(diagnostic.next.as_deref().unwrap().contains("commit"));
    assert!(
        !serde_json::to_string(&diagnostic)
            .unwrap()
            .contains("TOKEN_SENTINEL")
    );

    let error = run_error(provenant_run::RunError::Writer(
        provenant_confinement::worktrees::WorktreeError::CheckedOut,
    ));
    let decoded: FacadeError =
        serde_json::from_slice(&serde_json::to_vec(&error).unwrap()).unwrap();
    let diagnostic = decoded.diagnostic();
    assert!(
        diagnostic
            .detail
            .as_deref()
            .unwrap()
            .contains("checked out")
    );
    let next = diagnostic.next.as_deref().unwrap();
    assert!(next.contains("provenant worktree list"));
    assert!(next.contains("--worktree"));
}

#[test]
fn memory_pressure_refusal_keeps_source_and_resource_action() {
    use provenant_records::diagnostic::{PressureLevel, PressureThreshold, SettingSource};
    let error = run_error(provenant_run::RunError::Admission(
        provenant_run::AdmissionError::MemoryPressure {
            level: PressureLevel::Critical,
            threshold: PressureThreshold::Warning,
            origin: SettingSource::Configuration,
        },
    ));
    let projection = error.diagnostic();
    assert_eq!(projection.code, "confinement_refused");
    assert!(projection.detail.unwrap().contains("host configuration"));
    assert!(projection.next.unwrap().contains("memory"));

    let fail = provenant_cli::run::Fail::from(provenant_run::RunError::Admission(
        provenant_run::AdmissionError::MemoryPressure {
            level: PressureLevel::Critical,
            threshold: PressureThreshold::Warning,
            origin: SettingSource::Configuration,
        },
    ));
    let error = dispatch_refused(fail);
    let wire = serde_json::to_vec(&error).unwrap();
    let decoded: FacadeError = serde_json::from_slice(&wire).unwrap();
    let projection = decoded.diagnostic();
    assert_eq!(projection.code, "dispatch_refused");
    assert!(projection.detail.unwrap().contains("host configuration"));
    assert!(projection.next.unwrap().contains("memory"));
}

#[test]
fn lane_cap_refusal_retains_the_setting_through_both_adapters() {
    use provenant_records::diagnostic::{LaneLimit, SettingSource};
    let failure = || {
        provenant_run::RunError::Admission(provenant_run::AdmissionError::LaneCap {
            active: 3,
            limit: 3,
            kind: LaneLimit::ProjectWriters,
            origin: SettingSource::Configuration,
        })
    };
    for error in [run_error(failure()), dispatch_refused(failure().into())] {
        let decoded: FacadeError =
            serde_json::from_slice(&serde_json::to_vec(&error).unwrap()).unwrap();
        let projection = decoded.diagnostic();
        assert!(
            projection
                .detail
                .as_deref()
                .unwrap()
                .contains("max_project_writer_lanes")
        );
        assert!(
            projection
                .detail
                .as_deref()
                .unwrap()
                .contains("host configuration")
        );
        assert!(
            projection
                .next
                .as_deref()
                .unwrap()
                .contains("max_project_writer_lanes")
        );
    }
}

#[test]
fn process_limit_retains_io_code_and_safe_resource_facts() {
    let failure = || {
        provenant_run::RunError::Admission(provenant_run::AdmissionError::Processes {
            used: 950,
            limit: 1000,
        })
    };
    for (error, code) in [
        (run_error(failure()), "io"),
        (dispatch_refused(failure().into()), "dispatch_refused"),
    ] {
        let decoded: FacadeError =
            serde_json::from_slice(&serde_json::to_vec(&error).unwrap()).unwrap();
        let projection = decoded.diagnostic();
        assert_eq!(projection.code, code);
        let detail = projection.detail.unwrap();
        assert!(detail.contains("RLIMIT_NPROC"));
        assert!(detail.contains("950"));
        assert!(detail.contains("1000"));
        assert!(detail.contains("100"));
        assert!(projection.next.unwrap().contains("process"));
    }
}

#[test]
fn tick_wakes_graph_without_deadline_but_respects_future_deadline()
-> Result<(), Box<dyn std::error::Error>> {
    let mut checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint =
        serde_json::from_value(serde_json::json!({
            "schema_version": 1, "graph_id": "waiter", "plan_digest": "plan", "nodes": [
                {"id":"branch", "worktree":null, "state":"running", "attempts":[{
                    "key":{"graph_id":"waiter", "node_id":"branch", "route_index":0, "attempt": 1},
                    "route":{"adapter":"fixture", "model":"fixture/model"},
                    "state":"running", "launched":true, "launch_actor":"actor"
                }]}
            ]
        }))?;
    let now = provenant_core::WallTime::from_micros(1_000_000)?;
    assert!(graph_wake_due(&checkpoint, now));
    checkpoint.nodes[0].state = provenant_fabric_runtime::graph::NodeState::Pending;
    checkpoint.wake_at = Some(provenant_core::WallTime::from_micros(2_000_000)?);
    assert!(!graph_wake_due(&checkpoint, now));
    assert!(graph_wake_due(&checkpoint, checkpoint.wake_at.unwrap()));
    Ok(())
}

#[cfg(test)]
#[allow(dead_code)] // The worktree driver includes these helpers without libtest.
pub(crate) mod peer_tests {
    use super::*;
    #[allow(unused_imports)]
    // The worktree driver includes this module without libtest functions.
    use provenant_confinement::worktrees;
    use provenant_core::AttemptId;
    use provenant_run::{AttemptState, Outcome, RunStore};

    fn graph_test_context(
        key: &provenant_fabric_runtime::graph::AttemptKey,
    ) -> provenant_records::run::WorkContext {
        // Graph advance stamps the work key's one-based launch order.
        provenant_records::run::WorkContext {
            graph_id: Some(key.graph_id.clone()),
            node_id: Some(key.node_id.clone()),
            attempt: Some(key.attempt),
            ..Default::default()
        }
    }

    pub(super) fn host_test(name: &str) -> Result<bool, Box<dyn std::error::Error>> {
        if !provenant_cli::fabric::in_lane() {
            return Ok(false);
        }
        use provenant_os::exec::{ExecOptions, Executor, Termination};
        let argv = vec![
            std::env::current_exe()?.into_os_string(),
            "--exact".into(),
            name.into(),
            "--nocapture".into(),
        ];
        let removed = [
            provenant_core::RUN_STATE_DIRECTORY_ENV,
            provenant_run::LANE_DEPTH_ENV,
            provenant_run::PARENT_ATTEMPT_ENV,
        ];
        let fixture_home = tempfile::tempdir()?;
        let isolate_home = name
            .ends_with("explicit_writer_defers_tracking_ref_refresh_while_collection_is_busy")
            || name.ends_with("settlement_defers_adopted_writer_cleanup");
        let mut environment: Vec<_> = std::env::vars_os()
            .filter(|(key, _)| {
                !removed.iter().any(|name| key == name) && !(isolate_home && key == "HOME")
            })
            .collect();
        if isolate_home {
            environment.push(("HOME".into(), fixture_home.path().as_os_str().to_owned()));
        }
        let cwd = std::env::current_dir()?;
        // The graph-controls fixture now provisions and replays four variants.
        let timeout = if name.ends_with("graph_run_controls_reach_the_sealed_attempt") {
            120
        } else {
            30
        };
        let mut options = ExecOptions::new(&argv, &cwd, std::time::Duration::from_secs(timeout));
        options.env = Some(&environment);
        let output = SystemExecutor::new(1)?.execute(options)?;
        // Keep native-backend skip evidence visible through host isolation.
        use std::io::Write;
        std::io::stderr().lock().write_all(&output.stderr.bytes)?;
        assert_eq!(
            output.termination,
            Termination::Exit(0),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout.bytes),
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        Ok(true)
    }

    struct FreeWorkers;
    impl WorkerObserver for FreeWorkers {
        fn observe_roots(
            &self,
            roots: &[PathBuf],
        ) -> std::collections::BTreeMap<PathBuf, provenant_confinement::worktrees::Occupancy>
        {
            roots
                .iter()
                .cloned()
                .map(|root| (root, provenant_confinement::worktrees::Occupancy::Available))
                .collect()
        }
    }

    mod loop_repair_tests {
        include!("loop_repair_tests.rs");
    }

    mod tick_tests {
        include!("tick_tests.rs");
    }

    mod peer_graph_tests {
        include!("peer_graph_tests.rs");
    }

    pub(crate) struct SettlementFixture {
        pub(crate) rig: crate::worktree_tests::support::TestRig,
        pub(crate) roots: provenant_core::Roots,
        pub(crate) store: RunStore,
        pub(crate) settled: provenant_run::HolderContext,
        pub(crate) worktree: PathBuf,
    }

    fn publish_success(
        rig: &crate::worktree_tests::support::TestRig,
        store: &RunStore,
        record: &provenant_run::AttemptRecord,
        head: provenant_core::GitOid,
        call: &provenant_run::Request<'_>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        publish_outcome(rig, store, record, head, Outcome::Succeeded, call)
    }

    fn publish_outcome(
        rig: &crate::worktree_tests::support::TestRig,
        store: &RunStore,
        record: &provenant_run::AttemptRecord,
        head: provenant_core::GitOid,
        outcome: Outcome,
        call: &provenant_run::Request<'_>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        publish_routed(rig, store, record, head, outcome, "fixture/model", call)
    }

    fn publish_routed(
        rig: &crate::worktree_tests::support::TestRig,
        store: &RunStore,
        record: &provenant_run::AttemptRecord,
        head: provenant_core::GitOid,
        outcome: Outcome,
        model: &str,
        call: &provenant_run::Request<'_>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let attempt = rig
            .state
            .join("attempts")
            .join(record.attempt_id.to_string());
        let directory = provenant_os::fs::Directory::open_anchor(&attempt)?;
        use provenant_supervisor::custody::{
            ExitObservation, PublicationState, Scope, SettlementEvidence,
        };
        let exit = ExitObservation {
            publication: PublicationState::Durable,
            scope: Scope {
                attempt: record.attempt_id,
                generation: record.generation,
            },
            termination: Some(provenant_os::exec::Termination::Exit(0)),
            never_spawned: false,
            launch_refusal: None,
            stop_reason: None,
            delivered_signals: vec![],
            reaped: vec![],
            omitted_reaped: 0,
            spared: vec![],
            cancel_delivered_before_exit: None,
            omitted_spared: 0,
            warnings: vec![],
            omitted_warnings: 0,
            terminal_observed: false,
            unresolved_cleanup: false,
            unresolved_pids: vec![],
            settlement_evidence: SettlementEvidence::DarwinCensusAndCustodyLease,
            stdout: Default::default(),
            stderr: Default::default(),
        };
        let mut exit_wire = serde_json::to_value(&exit)?;
        let object = exit_wire.as_object_mut().ok_or("exit object")?;
        object.remove("publication");
        object.insert("schema_version".into(), 1.into());
        for (name, wire) in [
            ("process-exit.json", exit_wire),
            (
                "writer-terminal.json",
                serde_json::json!({"schema_version":1,"head":head,"protected_paths":[]}),
            ),
        ] {
            directory
                .atomic_write(
                    std::ffi::OsStr::new(name),
                    ".fixture-",
                    &serde_json::to_vec(&wire)?,
                    provenant_os::fs::PublishMode::NoReplace,
                )?
                .require_durable()?;
        }
        drop(directory.lock(
            std::ffi::OsStr::new("custody.lock"),
            provenant_os::fs::LockMode::Exclusive,
            call.lock_budget(),
            call.cancel,
            call.clock,
        )?);
        let route = provenant_run::RouteRecord {
            provider: model.split_once('/').map(|(adapter, _)| adapter.into()),
            model: Some(model.into()),
            ..Default::default()
        };
        store.publish_terminal(
            record.attempt_id,
            record.revision,
            &provenant_run::TerminalPublication {
                stop: None,
                terminal_cause: None,
                terminal_note: None,
                outcome,
                finished_at: call.clock.wall_now()?,
                started_at: None,
                provider_ms: None,
                reason: None,
                result: "OK",
                complete: true,
                redacted: false,
                resolved_route: Some(route.clone()),
                applied_route: Some(route.clone()),
                observed_route: Some(route),
                usage: Default::default(),
                session: None,
                partial_signals: Vec::new(),
            },
            &exit,
            call,
        )?;
        Ok(())
    }

    impl SettlementFixture {
        pub(crate) fn resumed() -> Result<Self, Box<dyn std::error::Error>> {
            Self::resumed_as("settlement-resume")
        }

        /// A settled lane whose head is one commit past its base.
        pub(crate) fn resumed_with_lane_commit() -> Result<Self, Box<dyn std::error::Error>> {
            Self::build("settlement-resume", None, "lane", true)
        }

        fn resumed_as(operation: &str) -> Result<Self, Box<dyn std::error::Error>> {
            Self::settled_fixture(operation, None)
        }

        fn settled_fixture(
            operation: &str,
            graph_id: Option<&str>,
        ) -> Result<Self, Box<dyn std::error::Error>> {
            Self::build(operation, graph_id, "lane", false)
        }

        /// A settled attempt admitted under `graph`'s `node`, attempt 1: the
        /// work key a graph binding this attempt to that node must carry.
        pub(crate) fn for_node(
            graph: &str,
            node: &str,
        ) -> Result<Self, Box<dyn std::error::Error>> {
            Self::build(&format!("settled-{graph}"), Some(graph), node, false)
        }

        /// Dispatch another attempt in this store under `graph`'s `node`,
        /// attempt 1, from the fixture's lane head. The caller publishes its
        /// terminal state.
        pub(crate) fn dispatch_keyed(
            &self,
            operation: &str,
            graph: &str,
            node: &str,
            call: &provenant_run::Request<'_>,
        ) -> Result<
            (provenant_run::AttemptRecord, provenant_core::GitOid),
            Box<dyn std::error::Error>,
        > {
            let head = self
                .store
                .record(self.settled.attempt_id, call)?
                .head
                .ok_or("head")?;
            let run = provenant_run::Service::new(&self.store).dispatch(
                provenant_run::DispatchRequest {
                    chair: None,
                    operation_id: operation.parse()?,
                    lane_depth: 1,
                    parent_attempt: None,
                    task: provenant_run::DispatchTask {
                        source_head: None,
                        context: Some(provenant_records::run::WorkContext {
                            graph_id: Some(graph.into()),
                            node_id: Some(node.into()),
                            attempt: Some(1),
                            ..Default::default()
                        }),
                        build_heavy: false,
                        title: None,
                        prompt: "reply OK".into(),
                        base: Some(head.to_string()),
                        pinned_base: None,
                        head: None,
                        profile: Default::default(),
                        route: provenant_run::RequestedRoute::Model {
                            extra: Default::default(),
                            provider: "fixture".into(),
                            model: "fixture/model".into(),
                            effort: None,
                        },
                        account: None,
                        timeout_seconds: 30,
                        delivery_id: None,
                        deliver: None,
                        worktree: Some(self.worktree.clone()),
                        access: Default::default(),
                    },
                },
                &self.rig.project,
                call,
            )?;
            Ok((self.store.record(run.attempt_id, call)?, head))
        }

        fn build(
            operation: &str,
            graph_id: Option<&str>,
            node: &str,
            lane_commit: bool,
        ) -> Result<Self, Box<dyn std::error::Error>> {
            use provenant_confinement::worktrees::{
                self, WorktreeCreate, WorktreeOrigin, WorktreeOwner,
            };
            let rig = crate::worktree_tests::support::TestRig::new()?;
            let git = provenant_os::git::git_executable().ok_or("git")?;
            for args in [
                vec!["config", "user.name", "Fixture"],
                vec!["config", "user.email", "fixture@example.invalid"],
                vec!["commit", "--allow-empty", "-qm", "base"],
            ] {
                let output = rig.execute(&git, &args, None)?;
                assert_eq!(crate::worktree_tests::support::code(&output), 0);
            }
            let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
            // A nested-lane fixture may only look up an existing binding.
            if std::env::var_os("PROVENANT_PANEL_ROUTING_FIXTURE").is_some() {
                provenant_config::bind_project(
                    &roots,
                    provenant_config::ProjectLocation::new(rig.project.join(".git"))?,
                )?;
            }
            let identity = Context::open(roots.clone(), &rig.home, &rig.project)?.identity;
            let clock = SystemClock;
            let cancel = CancelToken::new();
            let call = provenant_run::Request {
                clock: &clock,
                cancel: &cancel,
            };
            let context = provenant_run::ExecutionContext {
                state_root: roots.state.clone(),
                project_id: identity.project,
                actor_id: identity.inbox.actor,
                read_prompts: true,
            };
            let store =
                RunStore::open(&context, &call)?.with_writer_registry(roots.state.as_path());
            let info = worktrees::with_context(
                &roots,
                &rig.home,
                &rig.project,
                &clock,
                &cancel,
                |context| {
                    worktrees::create(
                        &worktrees::WorktreeContext {
                            workers: &FreeWorkers,
                            ..*context
                        },
                        WorktreeCreate {
                            branch: "resumed-lane".into(),
                            base: Some("HEAD".into()),
                        },
                        &cancel,
                    )
                },
            )?;
            let run = provenant_run::Service::new(&store).dispatch(
                provenant_run::DispatchRequest {
                    chair: None,
                    operation_id: if graph_id.is_some() {
                        operation
                    } else {
                        "settlement-original"
                    }
                    .parse()?,
                    lane_depth: 1,
                    parent_attempt: None,
                    task: provenant_run::DispatchTask {
                        source_head: None,
                        context: Some(provenant_records::run::WorkContext {
                            graph_id: Some(graph_id.unwrap_or("lane-resumed").to_owned()),
                            node_id: Some(node.into()),
                            attempt: Some(1),
                            ..Default::default()
                        }),
                        build_heavy: false,
                        title: None,
                        prompt: "reply OK".into(),
                        base: Some(info.head.to_string()),
                        pinned_base: None,
                        head: None,
                        profile: Default::default(),
                        route: provenant_run::RequestedRoute::Model {
                            extra: Default::default(),
                            provider: "fixture".into(),
                            model: "fixture/model".into(),
                            effort: None,
                        },
                        account: None,
                        timeout_seconds: 30,
                        delivery_id: None,
                        deliver: None,
                        worktree: Some(info.path.clone()),
                        access: Default::default(),
                    },
                },
                &rig.project,
                &call,
            )?;
            let prior = run.attempt_id;
            let prior_operation = store.attempt_operation(prior, &call)?.operation_id;
            worktrees::with_context(
                &roots,
                &rig.home,
                &rig.project,
                &clock,
                &cancel,
                |context| {
                    let tree = provenant_confinement::verify_worktree(context.repo, &info.path)?;
                    worktrees::save_owner(
                        &tree,
                        &WorktreeOwner {
                            attempt_id: Some(prior),
                            operation_id: Some(prior_operation.clone()),
                            branch: "resumed-lane".into(),
                            base: info.head.to_string(),
                            host: identity.host,
                            created_at: clock
                                .wall_now()
                                .map_err(|_| worktrees::WorktreeError::InvalidRequest)?,
                            origin: WorktreeOrigin::Lane,
                            label: None,
                            reference: None,
                        },
                    )?;
                    Ok(())
                },
            )?;
            let head = if lane_commit {
                let lane = info.path.to_str().ok_or("lane path")?;
                let output = rig.execute(
                    &git,
                    &["-C", lane, "commit", "--allow-empty", "-qm", "lane work"],
                    None,
                )?;
                assert_eq!(crate::worktree_tests::support::code(&output), 0);
                let output = rig.execute(&git, &["-C", lane, "rev-parse", "HEAD"], None)?;
                String::from_utf8(output.stdout.bytes)?
                    .trim()
                    .parse::<provenant_core::GitOid>()?
            } else {
                info.head
            };
            if graph_id.is_some() {
                let record = store.record(prior, &call)?;
                publish_success(&rig, &store, &record, head, &call)?;
                let settled = provenant_run::HolderContext {
                    schema_version: 1,
                    roots: provenant_run::RootSeed {
                        home: rig.home.clone(),
                        checkout: rig.project.clone(),
                        state_root: rig.state.clone(),
                    },
                    project_id: identity.project,
                    actor_id: identity.inbox.actor,
                    attempt_id: record.attempt_id,
                    host_delivery: None,
                    launch: None,
                    sealed: None,
                };
                return Ok(Self {
                    rig,
                    roots,
                    store,
                    settled,
                    worktree: info.path,
                });
            }
            store.withdraw(
                prior,
                AttemptState::Queued {
                    extra: Default::default(),
                },
                Outcome::Cancelled,
                "fixture",
                clock.wall_now()?,
                &call,
            )?;
            let resume = provenant_run::Resume {
                extend: Vec::new(),
                prior,
                expected_generation: 0,
                operation_id: operation.parse()?,

                attempt_id: AttemptId::from_bytes([31; 16]),
                prompt: "continue".into(),
                created_at: clock.wall_now()?,
            };
            store.resume_fresh(&resume, "fixture", &call)?;
            let record = store.record(resume.attempt_id, &call)?;
            publish_success(&rig, &store, &record, head, &call)?;
            let settled = provenant_run::HolderContext {
                schema_version: 1,
                roots: provenant_run::RootSeed {
                    home: rig.home.clone(),
                    checkout: rig.project.clone(),
                    state_root: rig.state.clone(),
                },
                project_id: identity.project,
                actor_id: identity.inbox.actor,
                attempt_id: record.attempt_id,
                host_delivery: None,
                launch: None,
                sealed: None,
            };
            Ok(Self {
                rig,
                roots,
                store,
                settled,
                worktree: info.path,
            })
        }
    }

    fn integrate_graph(
        fixture: &SettlementFixture,
        id: &str,
        target: &str,
    ) -> Result<
        (
            Result<provenant_fabric_runtime::graph::GraphRun, FacadeError>,
            bool,
        ),
        Box<dyn std::error::Error>,
    > {
        use provenant_fabric_runtime::graph::FileJournal;
        let cancel = CancelToken::new();
        let factory = Factory {
            context: Context::open(
                fixture.roots.clone(),
                &fixture.rig.home,
                &fixture.rig.project,
            )?,
            project_attribution: false,
            clock: SystemClock,
            backoff: ThreadPause,
            executor: SystemExecutor::new(1)?,
            home: fixture.rig.home.clone(),
            cwd: fixture.rig.project.clone(),
            assets: fixture.rig.project.clone(),
            host_launch: None,
            mcp_project: None,
            workers: &FreeWorkers,
        };
        let document = fixture.rig.project.join(format!("{id}-graph.json"));
        let route = serde_json::json!({"adapter":"fixture","model":"fixture/model"});
        std::fs::write(
            &document,
            serde_json::to_vec(&serde_json::json!({
                "schema_version":1, "graph_id":id,
                "nodes":[
                    {"id":"one","prompt":"reply OK","access":"worktree_write",
                        "worktree":fixture.worktree,"delivery":format!("integrate:{target}"),
                        "route":route},
                    // An unrelated writer on the same worktree that never integrates.
                    {"id":"two","prompt":"reply OK","access":"worktree_write",
                        "worktree":fixture.worktree,"depends_on":["one"],
                        "route":route}
                ]
            }))?,
        )?;
        let result = factory.graph(
            GraphInput::Run {
                extra: Default::default(),
                document,
            },
            WaitBudget::cli(factory.clock.mono_now(), 30)?,
            &cancel,
        );
        let admitted =
            FileJournal::read_checkpoint(&fixture.roots.state, fixture.settled.project_id, id)?
                .is_some();
        Ok((result, admitted))
    }

    pub(crate) fn writer_graph_output_preserves_lens_diff_base()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test("writer_graph_output_preserves_lens_diff_base")? {
            return Ok(());
        }
        use provenant_fabric_runtime::graph::{self, AttemptOwner};
        struct Seed(String);
        impl AttemptOwner for Seed {
            type Error = std::convert::Infallible;
            fn actor(&self) -> String {
                "fixture".into()
            }
            fn observe_or_start(
                &mut self,
                _: &graph::AttemptKey,
                _: graph::NodeInput<'_>,
                _: &graph::GraphRoute,
                _: graph::AccessMode,
                _: Option<&str>,
                _: graph::Replay<'_>,
            ) -> Result<graph::AttemptObservation, Self::Error> {
                Ok(graph::AttemptObservation::Pending {
                    attempt_ref: Some(self.0.clone()),
                })
            }
        }
        let fixture = SettlementFixture::resumed()?;
        let factory = Factory {
            context: Context::open(
                fixture.roots.clone(),
                &fixture.rig.home,
                &fixture.rig.project,
            )?,
            project_attribution: false,
            clock: SystemClock,
            backoff: ThreadPause,
            executor: SystemExecutor::new(1)?,
            home: fixture.rig.home.clone(),
            cwd: fixture.rig.project.clone(),
            assets: fixture.rig.project.clone(),
            host_launch: None,
            mcp_project: None,
            workers: &FreeWorkers,
        };
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let call = provenant_run::Request {
            clock: &factory.clock,
            cancel: &cancel,
        };
        let git = provenant_os::git::git_executable().ok_or("git")?;
        let node_source = provenant_run::resolve_git_base(&git, &fixture.rig.project, "HEAD")?
            .parse::<provenant_core::GitOid>()?;

        // 1. Dispatch writer attempt 1 on fixture.worktree with base = node_source.
        let dispatch = provenant_run::Service::new(&fixture.store).dispatch(
            provenant_run::DispatchRequest {
                chair: None,
                operation_id: "writer-output-op".parse()?,
                lane_depth: 1,
                parent_attempt: None,
                task: provenant_run::DispatchTask {
                    source_head: None,
                    context: Some(provenant_records::run::WorkContext {
                        // The work key the graph's writer node binds below.
                        graph_id: Some("writer-output-lens-graph".into()),
                        node_id: Some("writer".into()),
                        attempt: Some(1),
                        ..Default::default()
                    }),
                    build_heavy: false,
                    title: Some("writer".into()),
                    prompt: "implement slice".into(),
                    base: Some(node_source.to_string()),
                    pinned_base: None,
                    head: Some(node_source.to_string()),
                    profile: Default::default(),
                    route: provenant_run::RequestedRoute::Model {
                        extra: Default::default(),
                        provider: "claude".into(),
                        model: "claude/test-model".into(),
                        effort: None,
                    },
                    account: None,
                    timeout_seconds: 30,
                    delivery_id: None,
                    deliver: None,
                    worktree: Some(fixture.worktree.clone()),
                    access: Default::default(),
                },
            },
            &fixture.rig.project,
            &call,
        )?;
        let attempt_1 = dispatch.attempt_id;
        let attempt_1_record = fixture.store.record(attempt_1, &call)?;
        // This graph adopts the fixture's idle lane. Bind its owner to the
        // writer attempt rather than the unrelated resume fixture lineage.
        let writer = provenant_run::prepare_writer(
            &provenant_run::WriterRequest {
                extra: Default::default(),
                project: fixture.rig.project.clone(),
                worktree: fixture.worktree.clone(),
            },
            fixture.roots.state.as_path(),
            fixture.settled.project_id,
        )?;
        let mut lane_owner = worktrees::load_owner(&writer.worktree)?.ok_or("lane owner")?;
        lane_owner.attempt_id = Some(attempt_1);
        lane_owner.operation_id = Some(
            fixture
                .store
                .attempt_operation(attempt_1, &call)?
                .operation_id,
        );
        worktrees::save_owner(&writer.worktree, &lane_owner)?;

        let worktree_str = fixture.worktree.to_str().ok_or("worktree path")?;

        // The writer commits a subject distinct from its graph source.
        std::fs::write(
            fixture.worktree.join("writer-edited"),
            "writer fixture completed",
        )?;
        std::fs::write(fixture.worktree.join("first.txt"), b"first edit\n")?;
        let output = fixture.rig.execute(
            &git,
            &["-C", worktree_str, "add", "first.txt", "writer-edited"],
            None,
        )?;
        assert_eq!(crate::worktree_tests::support::code(&output), 0);
        let output = fixture.rig.execute(
            &git,
            &["-C", worktree_str, "commit", "-m", "first commit"],
            None,
        )?;
        assert_eq!(crate::worktree_tests::support::code(&output), 0);
        let head_1 = provenant_run::resolve_git_base(&git, &fixture.worktree, "HEAD")?
            .parse::<provenant_core::GitOid>()?;

        publish_outcome(
            &fixture.rig,
            &fixture.store,
            &attempt_1_record,
            head_1,
            Outcome::Succeeded,
            &call,
        )?;

        // 2. Set up graph with writer node and dependent lens node.
        let graph_id = "writer-output-lens-graph";
        let mut document: graph::GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "graph_id": graph_id,
            "nodes": [
                {
                    "id": "writer",
                    "prompt": "write",
                    "access": "worktree_write",
                    "worktree": fixture.worktree,
                    "source": { "type": "commit", "commit": node_source.to_string() },
                    "route": { "adapter": "fixture", "model": "fixture/model" }
                },
                {
                    "id": "lens",
                    "prompt": "review",
                    "access": "read_only",
                    "depends_on": ["writer"],
                    "source": { "type": "node", "node": "writer" },
                    "route": { "adapter": "fixture", "model": "fixture/model" }
                }
            ]
        }))?;

        // This fixture creates a current graph; legacy pending executions must
        // still be drained before the host admits a new dependent attempt.
        document.extra.insert(
            "template_library".into(),
            serde_json::to_value(graph::TemplateLibrary::bundled().snapshot())?,
        );

        let mut journal = graph::FileJournal::open(
            &fixture.roots.state,
            fixture.settled.project_id,
            graph_id,
            budget,
            &cancel,
            &factory.clock,
        )?;
        journal.bind(graph::GraphExecution {
            extra: Default::default(),
            integration_base: Some(node_source),
            review_base: None,
            directory: fixture.rig.project.clone(),
            document: document.clone(),
        })?;

        // Seed attempt_1 for the writer node in the graph checkpoint.
        let mut seed = Seed(format!("{}/{}", factory.context.identity.host, attempt_1));
        graph::advance_with_budget(
            &document,
            &mut seed,
            &mut journal,
            &Default::default(),
            &Default::default(),
        )?;

        // Now advance with BoundGraphOwner.
        let mut owner = BoundGraphOwner {
            writer_launch_budget: None,
            admission_timeout: None,
            launch_warnings: Vec::new(),
            repo_facts: Default::default(),
            factory: &factory,
            integration_base: Some(BoundGraphBases {
                integration: Some(node_source),
                review: None,
            }),
            integration_branch: None,
            network: None,
            capability_overrides: Default::default(),
            exclude: Vec::new(),
            budget,
            cancel: &cancel,
            prices: None,
            tick: false,
        };
        let run = graph::advance_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &Default::default(),
            &Default::default(),
        )?;

        // 3. Verification:
        // The graph observes the writer output directly.
        let writer_progress = &run.checkpoint.nodes[0];
        assert_eq!(writer_progress.state, graph::NodeState::Succeeded);
        assert_eq!(
            writer_progress
                .outputs
                .as_ref()
                .and_then(|o| o.commit.as_ref()),
            Some(&head_1.to_string())
        );

        // - Lens node was submitted, and its diff base stays node_source (not head_1)
        let lens_progress = &run.checkpoint.nodes[1];
        assert_ne!(
            lens_progress.state,
            graph::NodeState::Blocked,
            "{lens_progress:?}"
        );
        let lens_attempt_ref = lens_progress
            .attempts
            .first()
            .and_then(|a| a.attempt_ref.as_ref())
            .ok_or("lens attempt_ref")?;
        let (_, lens_attempt_id) = lens_attempt_ref.split_once('/').ok_or("attempt split")?;
        let lens_record = fixture.store.record(lens_attempt_id.parse()?, &call)?;
        let lens_request = fixture.store.request(lens_record.attempt_id, &call)?;
        let lens_task = lens_request.task;

        let node_source_str = node_source.to_string();
        let head_1_str = head_1.to_string();
        assert_eq!(
            lens_task.base.as_deref(),
            Some(node_source_str.as_str()),
            "lens diff base must stay the node source"
        );
        assert_ne!(
            lens_task.base.as_deref(),
            Some(head_1_str.as_str()),
            "lens diff base must not advance to the writer output"
        );
        assert_eq!(
            lens_task.head.as_deref(),
            Some(head_1_str.as_str()),
            "lens subject commit must be the writer output"
        );

        Ok(())
    }
    mod peer_graph_lifecycle {
        include!("peer_graph_lifecycle.rs");
    }
    mod peer_graph_settlement {
        include!("peer_graph_settlement.rs");
    }
    mod peer_graph_routing {
        include!("peer_graph_routing.rs");
    }
    mod peer_graph_submission {
        include!("peer_graph_submission.rs");
    }
    mod peer_writer_delivery {
        include!("peer_writer_delivery.rs");
    }
}

#[test]
fn fallback_cause_uses_only_unanimous_typed_facts() {
    use provenant_records::{
        RouteExclusionReason as Cause,
        run::{SkipKind, SkippedRoute},
    };
    let route = |cause| SkippedRoute {
        extra: Default::default(),
        route: "/private/token".into(),
        kind: SkipKind::Unavailable,
        cause,
        detail: "prompt token \u{1b} /private/secret".into(),
    };
    let first = route(Some(Cause::StaleAvailability));
    let first: SkippedRoute = serde_json::from_slice(&serde_json::to_vec(&first).unwrap()).unwrap();
    assert_eq!(
        unanimous_preparation_cause(std::slice::from_ref(&first)),
        Some(Cause::StaleAvailability)
    );
    assert_eq!(
        unanimous_preparation_cause(&[first.clone(), route(None)]),
        None
    );
    assert_eq!(
        unanimous_preparation_cause(&[first, route(Some(Cause::AuthRequired))]),
        None
    );
    assert_eq!(unanimous_preparation_cause(&[]), None);
}

#[test]
fn untyped_dispatch_refusal_retains_projection_or_message_without_secrets() {
    use provenant_records::diagnostic::Diagnostic;
    let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    for projection in [
        None,
        Some(Box::new(Diagnostic::new("fixture").detail(format!(
            "setup script failed: missing dependency {secret}"
        )))),
    ] {
        let fail = provenant_cli::run::Fail {
            status: provenant_records::Outcome::Refused,
            kind: provenant_run::ErrorKind::Refused,
            message: format!("setup script failed: missing dependency {secret}"),
            detail: None,
            projection,
        };
        let error = dispatch_refused(fail);
        let wire = serde_json::to_string(&error).unwrap();
        let decoded: FacadeError = serde_json::from_str(&wire).unwrap();
        let diagnostic = decoded.diagnostic();
        assert!(
            diagnostic
                .detail
                .unwrap()
                .contains("setup script failed: missing dependency")
        );
        assert!(!wire.contains(secret));
        assert!(!diagnostic.next.unwrap().contains("receipt"));
    }
}

#[test]
fn confinement_refusal_keeps_actual_cause_through_cli_and_dispatch() {
    use provenant_confinement::permissions::PermissionError;
    let fail = provenant_cli::run::Fail::from(provenant_run::RunError::Confinement(
        PermissionError::AnchorMismatchAt {
            path: "/fixture/worktree".into(),
            stage: "setup anchor changed ghp_abcdefghijklmnopqrstuvwxyz0123456789",
        },
    ));
    assert!(fail.message.contains("setup anchor changed"));
    assert!(
        !fail
            .message
            .contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789")
    );
    let error = dispatch_refused(fail);
    let decoded: FacadeError =
        serde_json::from_slice(&serde_json::to_vec(&error).unwrap()).unwrap();
    assert!(
        decoded
            .diagnostic()
            .detail
            .unwrap()
            .contains("setup anchor changed")
    );
}

#[cfg(test)]
#[test]
fn failed_worktree_add_retains_safe_operation_and_lock_facts_through_relay()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::worktrees::{
        self, GitMutationOperation, Occupancy, WorkerObserver, WorktreeCreate,
    };
    use std::collections::BTreeMap;
    struct Free;
    impl WorkerObserver for Free {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            roots
                .iter()
                .cloned()
                .map(|path| (path, Occupancy::Available))
                .collect()
        }
    }
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("home");
    let project = temp.path().join("project");
    let state = temp.path().join("state");
    for path in [&home, &project, &state] {
        std::fs::create_dir_all(path)?;
    }
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;
    provenant_os::fixture::run_git(&project, &["init", "-q"])?;
    provenant_os::fixture::run_git(&project, &["commit", "--allow-empty", "-qm", "base"])?;
    // A real Git ref lock gives deterministic mutation failure without a timeout.
    std::fs::write(
        project.join(".git/refs/heads/refused-lane.lock"),
        b"credential-sentinel",
    )?;
    let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
    inputs.state_dir = Some(state);
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let error = worktrees::with_context_using_workers(
        &roots,
        &home,
        &project,
        &clock,
        &cancel,
        &Free,
        |context| {
            worktrees::create(
                context,
                WorktreeCreate {
                    branch: "refused-lane".into(),
                    base: Some("HEAD".into()),
                },
                &cancel,
            )
        },
    )
    .expect_err("Git ref lock must refuse worktree addition");
    assert_eq!(
        error.mutation_failure(),
        Some((GitMutationOperation::WorktreeAdd, true))
    );
    let facade = graph_writer_initialisation_error("HEAD", error);
    let relayed = interface::relay_error(ServiceError::Facade(facade));
    assert!(matches!(
        relayed,
        FacadeError::GitMutationFailed {
            operation: GitMutationOperation::WorktreeAdd,
            lock_failure: true,
        }
    ));
    let wire = serde_json::to_string(&relayed)?;
    assert!(!wire.contains(&project.to_string_lossy().to_string()));
    assert!(!wire.contains("credential-sentinel"));
    let restored: FacadeError = serde_json::from_str(&wire)?;
    let diagnostic = restored.diagnostic();
    assert_eq!(restored.code(), "io");
    assert!(diagnostic.detail.as_deref().is_some_and(
        |detail| detail.contains("git worktree add") && detail.contains("lock failure: true")
    ));
    let untyped = interface::relay_error(ServiceError::Cli {
        code: "io".into(),
        message: "credential-sentinel".into(),
        data: serde_json::Value::Null,
    });
    let untyped_wire = serde_json::to_string(&untyped)?;
    assert!(!untyped_wire.contains("credential-sentinel"));
    assert_eq!(
        untyped.diagnostic().detail.as_deref(),
        Some("graph operation failed")
    );
    assert!(
        !diagnostic
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("graph input")
    );
    Ok(())
}
