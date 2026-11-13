//! Real lane CLI processes and relay transport over host-owned graph/read owners.
#![allow(clippy::disallowed_methods)]
#![allow(
    unused_imports,
    dead_code,
    reason = "the custom worktree driver omits libtest functions"
)]
use super::*;
use crate::worktree_tests::support::{Result, TestRig, code};
use provenant_core::AttemptId;
use provenant_fabric_runtime::graph::{
    FileJournal, GraphCheckpoint, GraphDocument, GraphExecution, GraphJournal,
};
use provenant_os::exec::Executor;
use serde_json::{Value, json};

#[test]
fn lane_reader_process() -> Result {
    let Some(arguments) = std::env::var_os("PROVENANT_TEST_LANE_READ_ARGS") else {
        return Ok(());
    };
    if let Some(journals) = std::env::var_os("PROVENANT_TEST_JOURNAL_DENIED") {
        assert!(
            std::fs::read_dir(journals).is_err(),
            "native lane must not read graph journals"
        );
    }
    let arguments: Vec<String> = serde_json::from_str(arguments.to_str().ok_or("arguments")?)?;
    let expected = std::env::var("PROVENANT_TEST_LANE_READ_EXIT")?.parse::<u8>()?;
    let actual = crate::dispatch(
        provenant_cli::parse(arguments.into_iter().map(Into::into)),
        &crate::System,
    );
    assert_eq!(actual, expected);
    Ok(())
}

fn lane_command(rig: &TestRig, arguments: &[&str], expected: u8) -> Result<Value> {
    lane_command_with_launch(rig, arguments, expected, None)
}

fn lane_command_with_launch(
    rig: &TestRig,
    arguments: &[&str],
    expected: u8,
    launch: Option<&provenant_confinement::permissions::ConfinedLaunch>,
) -> Result<Value> {
    let mut environment = rig.env.clone();
    environment.push((
        "PROVENANT_TEST_LANE_READ_ARGS".into(),
        serde_json::to_string(arguments)?.into(),
    ));
    environment.push((
        "PROVENANT_TEST_LANE_READ_EXIT".into(),
        expected.to_string().into(),
    ));
    let executable = std::env::current_exe()?;
    let argv = vec![
        executable.into_os_string(),
        "--exact".into(),
        "fabric::lane_read_tests::lane_reader_process".into(),
        "--nocapture".into(),
    ];
    let confined = if let Some(launch) = launch {
        environment.push((
            "PROVENANT_TEST_JOURNAL_DENIED".into(),
            rig.state.join("fabric/graphs").into_os_string(),
        ));
        let attempt = rig
            .env
            .iter()
            .find(|(key, _)| key == provenant_run::PARENT_ATTEMPT_ENV)
            .and_then(|(_, value)| value.to_str())
            .ok_or("lane parent attempt")?
            .parse()?;
        Some(launch.command(
            &argv,
            &attempt,
            &provenant_os::fs::Directory::open_anchor(&rig.state)?,
        )?)
    } else {
        None
    };
    let mut options = provenant_os::exec::ExecOptions::new(
        confined.as_ref().map(|c| &c.argv[..]).unwrap_or(&argv),
        &rig.project,
        std::time::Duration::from_secs(30),
    );
    options.inherited_profile = confined
        .as_ref()
        .map(|c| &c.profile as &dyn provenant_os::sys::profile::ProfileSource);
    options.env = Some(&environment);
    let output = SystemExecutor::new(1)?.execute(options)?;
    assert_eq!(
        code(&output),
        0,
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout.bytes),
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    output
        .stdout
        .bytes
        .split(|byte| *byte == b'\n')
        .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
        .find(|row| row.get("data").is_some())
        .ok_or_else(|| {
            format!(
                "CLI JSON missing: {}",
                String::from_utf8_lossy(&output.stdout.bytes)
            )
            .into()
        })
}

fn settled_attempt(
    context: &Context,
    rig: &TestRig,
    success: bool,
    graph: &str,
) -> Result<AttemptId> {
    use provenant_records::{CanonicalV1, run::RouteRecord};
    use provenant_supervisor::custody::{
        ExitObservation, PublicationState, Scope, SettlementEvidence,
    };
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = provenant_run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let execution = provenant_run::ExecutionContext {
        state_root: context.roots.state.clone(),
        project_id: context.identity.project,
        actor_id: context.identity.inbox.actor,
        read_prompts: false,
    };
    let store = provenant_run::RunStore::open(&execution, &call)?;
    let number = if graph != "lane-read-children" {
        54
    } else if success {
        51
    } else {
        52
    };
    let id = AttemptId::from_bytes([number; 16]);
    let prompt = "fixture child";
    let request = serde_json::from_value(json!({
        "schema_version":1,"parent":null,
        "task":{"prompt_digest":CanonicalV1::file_digest(prompt.as_bytes()),
            "context":{"graph_id":graph,"node_id":if success {"parent--success"} else {"parent--failure"},"attempt":1},
            "route":{"kind":"model","provider":"codex","model":"gpt-6-luna","effort":null},
            "authority_digest":provenant_core::Digest::from_bytes([7;32]),
            "provider_timeout_seconds":30,"delivery_id":null}
    }))?;
    store.create(
        &provenant_run::PreparedRun {
            attempt_id: id,
            operation_id: format!("lane-read-{number}").parse()?,
            request,
            prompt: prompt.into(),
            created_at: clock.wall_now()?,
        },
        &call,
    )?;
    let record = store.record(id, &call)?;
    let exit = ExitObservation {
        launch_refusal: None,
        publication: PublicationState::Durable,
        scope: Scope {
            attempt: id,
            generation: record.generation,
        },
        termination: Some(provenant_os::exec::Termination::Exit(if success {
            0
        } else {
            1
        })),
        never_spawned: false,
        stop_reason: None,
        delivered_signals: Vec::new(),
        reaped: Vec::new(),
        omitted_reaped: 0,
        spared: Vec::new(),
        cancel_delivered_before_exit: None,
        omitted_spared: 0,
        warnings: Vec::new(),
        omitted_warnings: 0,
        terminal_observed: true,
        unresolved_cleanup: false,
        unresolved_pids: Vec::new(),
        settlement_evidence: SettlementEvidence::DarwinCensusAndCustodyLease,
        stdout: Default::default(),
        stderr: Default::default(),
    };
    let mut sealed_exit = serde_json::to_value(&exit)?;
    let fields = sealed_exit.as_object_mut().ok_or("exit object")?;
    fields.remove("publication");
    fields.insert("schema_version".into(), 1.into());
    let directory =
        provenant_os::fs::Directory::open_anchor(&rig.state.join("attempts").join(id.to_string()))?;
    directory
        .atomic_write(
            std::ffi::OsStr::new("process-exit.json"),
            ".fixture-",
            &serde_json::to_vec(&sealed_exit)?,
            provenant_os::fs::PublishMode::NoReplace,
        )?
        .require_durable()?;
    let route = RouteRecord {
        provider: Some("codex".into()),
        model: Some("gpt-6-luna".into()),
        ..Default::default()
    };
    store.publish_terminal(
        id,
        record.revision,
        &provenant_run::TerminalPublication {
            stop: None,
            terminal_cause: None,
            terminal_note: None,
            outcome: if success {
                provenant_records::run::Outcome::Succeeded
            } else {
                provenant_records::run::Outcome::Failed
            },
            started_at: Some(record.queued_at),
            finished_at: clock.wall_now()?,
            provider_ms: Some(1),
            reason: if success {
                None
            } else {
                Some("fixture child provider failed".into())
            },
            result: if success {
                "child result reached parent"
            } else {
                "child provider failure details"
            },
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
        &call,
    )?;
    Ok(id)
}

#[test]
fn parent_reads_child_result_failure_status_and_trace_through_host_relay() -> Result {
    let name = "fabric::lane_read_tests::parent_reads_child_result_failure_status_and_trace_through_host_relay";
    if super::peer_tests::host_test(name)? {
        return Ok(());
    }
    let mut rig = TestRig::new()?;
    let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
    let context = Context::open(roots, &rig.home, &rig.project)?;
    let success = settled_attempt(&context, &rig, true, "lane-read-children")?;
    let failure = settled_attempt(&context, &rig, false, "lane-read-children")?;
    let caller = AttemptId::from_bytes(provenant_os::fs::entropy_128()?);
    let graph = "lane-read-children";
    let reference = |id| format!("{}/{id}", context.identity.host);
    let checkpoint: GraphCheckpoint = serde_json::from_value(json!({
        "schema_version":1,"graph_id":graph,"plan_digest":"fixture",
        "nodes":[
            {"id":"parent","state":"running","attempts":[{"key":{"graph_id":graph,"node_id":"parent","route_index":0, "attempt": 1},"launched":true,"route":{"alias":"senior"},"state":"running","attempt_ref":reference(caller),"launch_actor":"fixture"}]},
            {"id":"parent--success","state":"succeeded","attempts":[{"key":{"graph_id":graph,"node_id":"parent--success","route_index":0, "attempt": 1},"launched":true,"route":{"alias":"junior"},"state":"terminal","attempt_ref":reference(success),"launch_actor":"fixture","outcome":"succeeded"}]},
            {"id":"parent--failure","state":"failed","advance_error_code":"launch_refused","advance_owner_code":"command_unavailable","advance_error":"configure the fixture provider command","attempts":[{"key":{"graph_id":graph,"node_id":"parent--failure","route_index":0, "attempt": 1},"launched":true,"route":{"alias":"junior"},"state":"terminal","attempt_ref":reference(failure),"launch_actor":"fixture","outcome":"task_failed"}]}
        ]
    }))?;
    let document: GraphDocument = serde_json::from_value(
        json!({"schema_version":2,"graph_id":graph,"template_library":{},"nodes":[
            {"id":"parent","access":"read_only","prompt":"parent","route":{"alias":"senior"}},
            {"id":"parent--success","access":"read_only","prompt":"child","route":{"alias":"junior"}},
            {"id":"parent--failure","access":"read_only","prompt":"child","route":{"alias":"junior"}}
        ]}),
    )?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 30)?;
    let mut journal = FileJournal::open(
        &context.roots.state,
        context.identity.project,
        graph,
        budget,
        &cancel,
        &clock,
    )?;
    journal.bind(GraphExecution {
        review_base: None,
        extra: Default::default(),
        integration_base: None,
        directory: rig.project.clone(),
        document,
    })?;
    journal.save(&checkpoint)?;
    drop(journal);
    let mut holder = provenant_run::HolderContext {
        schema_version: 1,
        roots: provenant_run::RootSeed {
            home: rig.home.clone(),
            checkout: rig.project.clone(),
            state_root: rig.state.clone(),
        },
        project_id: context.identity.project,
        actor_id: context.identity.inbox.actor,

        attempt_id: caller,
        host_delivery: None,
        sealed: None,
        launch: Some(provenant_run::HostLaunchContext::from_environment(
            &rig.state,
            &rig.project,
            provenant_os::git::git_executable().ok_or("Git unavailable")?,
        )),
    };
    // Expired and cancelled reads refuse before opening even a host context.
    let missing_launch = provenant_run::HolderContext {
        launch: None,
        ..holder.clone()
    };
    assert!(matches!(
        super::read_lane_graph(
            &missing_launch,
            json!({"op":"status","id":graph}),
            &cancel,
            std::time::Duration::from_millis(100),
            &provenant_confinement::worktrees::SystemWorkerObserver
        ),
        Err(ServiceError::Facade(FacadeError::Timeout))
    ));
    let cancelled = CancelToken::new();
    cancelled.cancel();
    assert!(matches!(
        super::read_lane_graph(
            &missing_launch,
            json!({"op":"status","id":graph}),
            &cancelled,
            std::time::Duration::from_secs(30),
            &provenant_confinement::worktrees::SystemWorkerObserver
        ),
        Err(ServiceError::Facade(FacadeError::Cancelled))
    ));
    holder.launch.as_mut().ok_or("host launch")?.graph_directory = Some(rig.project.clone());
    // Exercise the real host reader even when the enclosing lane cannot bind
    // a second attempt socket. The native/CLI half below runs on configured hosts.
    let read = |input| {
        provenant_cli::fabric::graph_relay::serve_with_read(
            &context,
            &caller.to_string(),
            json!({"op":"read","input":input}),
            budget,
            &cancel,
            &clock,
            &|input| {
                super::read_lane_graph(
                    &holder,
                    input,
                    &cancel,
                    std::time::Duration::from_secs(30),
                    &provenant_confinement::worktrees::SystemWorkerObserver,
                )
            },
        )
    };
    // A child provider may work in another repository while remaining in
    // this graph. Host journal identity must outlive that cwd change.
    let foreign = rig.root.join("foreign-project");
    std::fs::create_dir_all(&foreign)?;
    let mut foreign_holder = holder.clone();
    foreign_holder.launch.as_mut().ok_or("host launch")?.cwd = foreign;
    struct InertHolder;
    impl provenant_supervisor::Holder for InertHolder {
        fn admit(
            &self,
            _: &[u8],
            _: &provenant_os::fs::Directory,
        ) -> provenant_supervisor::custody::Result<()> {
            Ok(())
        }
        fn terminal(
            &self,
            _: &[u8],
            _: &provenant_os::fs::Directory,
            _: &provenant_supervisor::custody::ExitObservation,
            _: &provenant_supervisor::custody::Recovery,
        ) -> provenant_supervisor::custody::Result<()> {
            Ok(())
        }
    }
    use provenant_supervisor::Holder;
    let host_holder = provenant_cli::run::HostHolder::new(&InertHolder);
    assert!(!host_holder.has_live_children(&serde_json::to_vec(&foreign_holder)?)?);
    let foreign_read = super::read_lane_graph(
        &foreign_holder,
        json!({"op":"output","id":"lane-read-children/parent--success"}),
        &cancel,
        std::time::Duration::from_secs(30),
        &provenant_confinement::worktrees::SystemWorkerObserver,
    )?;
    assert_eq!(foreign_read["bytes"]["data"], "child result reached parent");
    // A malformed target is the caller's input, never a host I/O failure.
    assert!(matches!(
        super::read_lane_graph(
            &holder,
            json!({"op":"output","id":"parent--success"}),
            &cancel,
            std::time::Duration::from_secs(30),
            &provenant_confinement::worktrees::SystemWorkerObserver,
        ),
        Err(ServiceError::Facade(FacadeError::InvalidInput { .. }))
    ));
    let result = read(json!({"op":"output","id":"parent--success"}))?;
    assert_eq!(
        result["bytes"]["data"], "child result reached parent",
        "{result}"
    );
    // Inside a lane, targets name the calling graph's own nodes; attempt IDs
    // are not targets (cli.md "Targets").
    for selector in [
        "lane-read-children/parent--success",
        "lane-read-children/parent--success#1",
        "parent--success#1",
    ] {
        let output = read(json!({"op":"output","id":selector}))?;
        assert_eq!(
            output["bytes"]["data"], "child result reached parent",
            "{output}"
        );
    }
    for selector in [success.to_string(), reference(success)] {
        assert!(read(json!({"op":"output","id":selector})).is_err());
    }
    let failure_result = read(json!({"op":"output","id":"parent--failure"}))?;
    assert_eq!(
        failure_result["bytes"]["data"],
        "child provider failure details"
    );
    assert_eq!(failure_result["state"], "failed");
    assert_eq!(
        failure_result["graph_failures"][0]["reason"],
        "configure the fixture provider command"
    );
    assert_eq!(
        failure_result["graph_failures"][0]["cause_code"],
        "command_unavailable"
    );
    let status = read(json!({"op":"status","id":graph}))?;
    assert!(
        status["rows"]
            .as_array()
            .ok_or("host status rows")?
            .iter()
            .any(|row| row["node_id"] == "parent--failure" && row["state"] == "failed")
    );
    let trace = read(json!({"op":"trace","id":graph}))?;
    assert!(
        !trace["events"]
            .as_array()
            .ok_or("host trace events")?
            .is_empty()
    );
    for selector in ["parent--success", "lane-read-children/parent--success"] {
        let trace = read(json!({"op":"trace","id":selector}))?;
        let events = trace["events"].as_array().ok_or("child attempt trace")?;
        assert!(!events.is_empty());
        assert!(
            events
                .iter()
                .all(|event| event["subject"] == "parent--success")
        );
    }
    assert!(read(json!({"op":"output","id":"another-graph/parent"})).is_err());
    let relay = provenant_fabric_mcp::lane::HostRelay::start_with_graph(
        provenant_providers::LaneRuntime::fabric_socket(caller),
        Default::default(),
        |_, _, _| Err(ServiceError::FabricOwnerUnavailable),
        move |arguments, cancel, within| {
            let budget = provenant_fabric_mcp::lane::host_budget(&clock, within)?;
            provenant_cli::fabric::graph_relay::serve_with_read(
                &context,
                &caller.to_string(),
                arguments,
                budget,
                cancel,
                &clock,
                &|input| {
                    super::read_lane_graph(
                        &holder,
                        input,
                        cancel,
                        within,
                        &provenant_confinement::worktrees::SystemWorkerObserver,
                    )
                },
            )
        },
    );
    let relay = match relay {
        Ok(relay) => relay,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            provenant_test_support::skip_sandbox_dependent_test(format!(
                "host graph reads verified; lane CLI/native graph read fixture cannot bind an attempt relay inside this lane: {error}"
            ))?;
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    let child_state = rig.state.join("children").join(caller.to_string());
    std::fs::create_dir_all(&child_state)?;
    rig.env.extend([
        (
            provenant_core::RUN_STATE_DIRECTORY_ENV.into(),
            child_state.clone().into_os_string(),
        ),
        (provenant_run::LANE_DEPTH_ENV.into(), "1".into()),
        (
            provenant_run::PARENT_ATTEMPT_ENV.into(),
            caller.to_string().into(),
        ),
    ]);
    let result = lane_command(
        &rig,
        &["output", "lane-read-children/parent--success", "--json"],
        0,
    )?;
    assert_eq!(
        result["data"]["bytes"]["data"],
        "child result reached parent"
    );
    let destination = rig.root.join("collected-child-result.txt");
    let written = lane_command(
        &rig,
        &[
            "output",
            "lane-read-children/parent--success",
            "--to",
            destination.to_str().ok_or("output destination")?,
            "--json",
        ],
        0,
    )?;
    assert_eq!(std::fs::read(&destination)?, b"child result reached parent");
    assert_eq!(
        written["data"]["written"],
        "child result reached parent".len()
    );
    let path: provenant_records::StructuredPathWire =
        serde_json::from_value(written["data"]["to"].clone())?;
    assert_eq!(path.to_path_buf()?, std::fs::canonicalize(&destination)?);
    let failure = lane_command(
        &rig,
        &["output", "lane-read-children/parent--failure", "--json"],
        0,
    )?;
    assert_eq!(
        failure["data"]["bytes"]["data"],
        "child provider failure details"
    );
    assert_eq!(failure["data"]["state"], "failed");
    assert_eq!(
        failure["data"]["graph_failures"][0]["reason"],
        "configure the fixture provider command"
    );
    assert_eq!(
        failure["data"]["graph_failures"][0]["cause_code"],
        "command_unavailable"
    );
    let status = lane_command(&rig, &["status", graph, "--json"], 0)?;
    let rows = status["data"]["rows"].as_array().ok_or("status rows")?;
    assert!(
        rows.iter()
            .any(|row| row["node_id"] == "parent--failure" && row["state"] == "failed")
    );
    let waited = lane_command(
        &rig,
        &["wait", "lane-read-children", "--timeout", "1s", "--json"],
        124,
    )?;
    assert_eq!(waited["data"]["timed_out"], true);
    assert!(
        waited["data"]["targets"][0]["data"]["rows"]
            .as_array()
            .ok_or("wait status rows")?
            .iter()
            .any(|row| row["node_id"] == "parent--failure" && row["state"] == "failed")
    );
    let trace = lane_command(&rig, &["trace", graph, "--json"], 0)?;
    assert!(
        !trace["data"]["events"]
            .as_array()
            .ok_or("trace events")?
            .is_empty()
    );
    let denied = lane_command(&rig, &["output", "another-graph/parent", "--json"], 3)?;
    assert!(
        denied["error"]["message"]
            .as_str()
            .is_some_and(|reason| reason.contains("not_found")),
        "{denied}"
    );
    assert_eq!(std::fs::read_dir(child_state)?.count(), 0);
    if let Some(launch) = native_lane(&rig, caller)? {
        let result = lane_command_with_launch(
            &rig,
            &["output", "lane-read-children/parent--success", "--json"],
            0,
            Some(&launch),
        )?;
        assert_eq!(
            result["data"]["bytes"]["data"],
            "child result reached parent"
        );
        let failure = lane_command_with_launch(
            &rig,
            &["output", "lane-read-children/parent--failure", "--json"],
            0,
            Some(&launch),
        )?;
        assert_eq!(
            failure["data"]["graph_failures"][0]["cause_code"],
            "command_unavailable"
        );
        lane_command_with_launch(&rig, &["status", graph, "--json"], 0, Some(&launch))?;
        lane_command_with_launch(&rig, &["trace", graph, "--json"], 0, Some(&launch))?;
    }
    drop(relay);
    Ok(())
}

fn native_lane(
    rig: &TestRig,
    caller: AttemptId,
) -> Result<Option<provenant_confinement::permissions::ConfinedLaunch>> {
    use provenant_confinement::permissions::*;
    let executor = SystemExecutor::new(1)?;
    let cancel = CancelToken::new();
    let backend = match system_backend(&BackendContext {
        executor: &executor,
        cancel: &cancel,
        #[cfg(target_os = "macos")]
        env: &rig.env,
    }) {
        Ok(backend) => backend,
        Err(error) => {
            provenant_test_support::skip_sandbox_dependent_test(format!(
                "native lane graph read check unavailable: {error}"
            ))?;
            return Ok(None);
        }
    };
    let scratch = rig.root.join("native-lane-scratch");
    // Lane scratch is private to the user, as the launcher creates it.
    provenant_os::fixture::private_state_dir(&scratch)?;
    let grant = |path, scope| PathGrant { path, scope };
    let executable = std::fs::canonicalize(std::env::current_exe()?)?;
    let git = std::fs::canonicalize(provenant_os::git::git_executable().ok_or("Git unavailable")?)?;
    let mut inputs = provenant_core::RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let lookup = |name: &str| {
        rig.env
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };
    let mut credentials = host_credential_roots(lookup);
    let accounts: provenant_providers::accounts::Settings =
        provenant_config::read_domain(roots.state.as_path(), "accounts")?;
    credentials.extend(
        accounts
            .entries
            .values()
            .flat_map(|entry| std::iter::once(&entry.directory).chain(&entry.directories))
            .filter(|path| path.as_path() != rig.home)
            .map(|path| grant(path.clone(), PathScope::Subtree)),
    );
    let github: Vec<_> = host_github_roots(lookup)
        .into_iter()
        .map(|path| grant(path, PathScope::Subtree))
        .collect();
    let mandatory =
        MandatoryRoots::for_host(&roots, &rig.home, &rig.project, &credentials, &github, &[])?;
    let mut policy = provenant_confinement::run_config::RunConfig::read(roots.state.as_path())?
        .policy_for_launch(roots.state.as_path(), mandatory);
    policy.resolve_optional_aliases(resolve_boundary_path)?;
    let plan = PlanPermissions {
        policy: Some(policy),
        read_only: None,
        mode: AccessMode::ReadOnly,
        scratch: grant(scratch, PathScope::Subtree),
        private_state: None,
        read_roots: vec![grant(rig.project.clone(), PathScope::Subtree)],
        protected_roots: Vec::new(),
        secret_roots: vec![grant(rig.state.join("fabric/graphs"), PathScope::Subtree)],
        selected_authentication: None,
        include_continuation: false,
        write_roots: Vec::new(),
        network: NetworkAccess::Provider,
        nested_confinement: false,
    };
    let profile = ProviderProfile {
        spec: ProviderProfileSpec {
            process_executables: vec![
                grant(executable.clone(), PathScope::Exact),
                grant(git.clone(), PathScope::Exact),
            ],
            allow_process_fork: true,
            deny_credential_access: true,
            deny_keychain_access: true,
            runtime_reads: vec![
                grant(executable, PathScope::Exact),
                grant(git, PathScope::Exact),
            ],
            fabric_socket: Some(grant(
                provenant_providers::LaneRuntime::fabric_socket(caller),
                PathScope::Exact,
            )),
            ..Default::default()
        },
        observations: Default::default(),
    };
    let anchors = required_anchors(&plan, &profile, None)
        .into_iter()
        .map(AnchoredDirectory::open_canonical)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(Some(backend.compile(
        &plan,
        &profile,
        &Anchors {
            home: &rig.home,
            directories: &anchors,
            worktree: None,
        },
    )?))
}
