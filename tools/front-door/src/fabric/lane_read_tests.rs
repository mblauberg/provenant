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
    let (parsed, warnings) = provenant_records::warning::capture(|| {
        provenant_cli::parse(arguments.into_iter().map(Into::into))
    });
    let actual = crate::dispatch(parsed, &crate::System, warnings);
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
            .find(|(key, _)| key == provenant_core::consumer_env::PARENT_ATTEMPT_ID)
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
        .map(|c| &c.profile as &dyn provenant_os::sys::profile::LimitSource);
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
    use provenant_records::custody::Scope;
    use provenant_records::{CanonicalV1, run::RouteRecord};
    use provenant_supervisor::custody::{ExitObservation, PublicationState, SettlementEvidence};
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
    // Every graph owns its attempts: the host checks each attempt's work key
    // against the graph node that names it, so graphs cannot share fixtures.
    let (success_number, failure_number) = match graph {
        "lane-read-children" => (51, 52),
        "lane-read-sibling" => (53, 54),
        "lane-read-foreign" => (55, 56),
        "lane-read-other-project" => (57, 58),
        _ => return Err(format!("no fixture attempts reserved for {graph}").into()),
    };
    let number = if success {
        success_number
    } else {
        failure_number
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
        provider_identity_conflict: false,
        provider_identity_evidence_incomplete: false,
        provider_identity_eof_fragment: false,
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
    let outcome = if success {
        provenant_records::run::Outcome::Succeeded
    } else {
        provenant_records::run::Outcome::Failed
    };
    let mut fact = provenant_records::settlement::classify::settle(
        &provenant_records::settlement::SettlementEvidence::fixture(
            provenant_records::settlement::LaneRole::Reader,
            outcome,
            None,
        ),
    );
    if !success {
        fact.reason = Some("fixture child provider failed".into());
    }
    store.publish_terminal(
        id,
        record.revision,
        &provenant_run::TerminalPublication {
            fact,
            evidence: None,
            started_at: Some(record.queued_at),
            finished_at: clock.wall_now()?,
            provider_ms: Some(1),
            result: if success {
                "child result reached parent"
            } else {
                "child provider failure details"
            },
            redacted: false,
            resolved_route: Some(route.clone()),
            applied_route: Some(route.clone()),
            observed_route: Some(route),
            usage: Default::default(),
            session: None,
        },
        &exit,
        &call,
    )?;
    Ok(id)
}

/// Seal the submitting chair on a graph's custody record, as graph submission
/// does. The lane read scope compares this session across graphs.
fn seal_graph_chair(
    context: &Context,
    project: provenant_core::ProjectId,
    graph: &str,
    session: &str,
) -> Result<()> {
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let chair_call = provenant_fabric_runtime::chairs::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let proposal = provenant_records::run::CanonicalRequest {
        extra: Default::default(),
        chair: Some(provenant_records::custody::Chair {
            host: context.identity.inbox.host,
            actor: context.identity.inbox.actor,
            session: Some(session.into()),
            process: None,
            extra: Default::default(),
        }),
        fresh_continuation: None,
        schema_version: 1,
        task: serde_json::from_value(json!({
            "prompt_digest": provenant_core::Digest::from_bytes([4; 32]),
            "route": {"kind":"model","provider":"fixture","model":"fixture/model"},
            "authority_digest": provenant_core::Digest::from_bytes([5; 32]),
            "provider_timeout_seconds": 30,
            "delivery_id": null
        }))?,
        parent: None,
        lane_depth: None,
        parent_attempt: None,
    };
    provenant_fabric_runtime::chairs::ChairIndex::open(&context.roots.state, &chair_call)?.submit(
        project,
        graph,
        &proposal,
        &chair_call,
    )?;
    Ok(())
}

/// Bind a graph whose settled nodes name their own settled attempts, so a lane
/// can read output, status and trace from it once its chair session is
/// admitted. The attempts' work keys must match this graph's nodes.
fn bind_sibling_graph(
    context: &Context,
    rig: &TestRig,
    project: provenant_core::ProjectId,
    directory: &std::path::Path,
    graph: &str,
) -> Result<()> {
    let success = settled_attempt(context, rig, true, graph)?;
    let failure = settled_attempt(context, rig, false, graph)?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 30)?;
    let mut journal = FileJournal::open(
        &context.roots.state,
        project,
        graph,
        budget,
        &cancel,
        &clock,
    )?;
    let reference = |id: AttemptId| format!("{}/{id}", context.identity.host);
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":2,"graph_id":graph,"template_library":{},"nodes":[
            {"id":"parent--success","access":"read_only","prompt":"child","route":{"alias":"junior"}},
            {"id":"parent--failure","access":"read_only","prompt":"child","route":{"alias":"junior"}}
        ]
    }))?;
    let mut checkpoint: GraphCheckpoint = serde_json::from_value(json!({
        "schema_version":2,"graph_id":graph,"plan_digest":"fixture",
        "nodes":[
            {"id":"parent--success","state":"succeeded","attempts":[{"key":{"graph_id":graph,"node_id":"parent--success","route_index":0,"attempt":1},"launched":true,"route":{"alias":"junior"},"state":"terminal","attempt_ref":reference(success),"launch_actor":"fixture","outcome":"succeeded"}]},
            {"id":"parent--failure","state":"failed","advance_error_code":"launch_refused","advance_owner_code":"command_unavailable","advance_error":"configure the fixture provider command","attempts":[{"key":{"graph_id":graph,"node_id":"parent--failure","route_index":0,"attempt":1},"launched":true,"route":{"alias":"junior"},"state":"terminal","attempt_ref":reference(failure),"launch_actor":"fixture","outcome":"task_failed"}]}
        ]
    }))?;
    journal.bind(GraphExecution {
        peer_origin_context: None,
        peer_submission_digest: None,
        requester_path: None,
        extra: Default::default(),
        bases: None,
        directory: directory.to_path_buf(),
        document,
    })?;
    journal.prepare_fixture_checkpoint(&mut checkpoint)?;
    journal.save(&checkpoint)?;
    Ok(())
}

// Spec: docs/specs/fabric.md#lane-graph-read-scope
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
    let mut checkpoint: GraphCheckpoint = serde_json::from_value(json!({
        "schema_version":2,"graph_id":graph,"plan_digest":"fixture",
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
        peer_origin_context: None,
        peer_submission_digest: None,
        requester_path: None,
        extra: Default::default(),
        bases: None,
        directory: rig.project.clone(),
        document,
    })?;
    journal.prepare_fixture_checkpoint(&mut checkpoint)?;
    journal.save(&checkpoint)?;
    drop(journal);
    // Two more graphs: a sibling from the same chair session, and one from a
    // different session. Both are otherwise readable by this host.
    seal_graph_chair(&context, context.identity.project, graph, "chair-a")?;
    let sibling_graph = "lane-read-sibling";
    bind_sibling_graph(
        &context,
        &rig,
        context.identity.project,
        &rig.project,
        sibling_graph,
    )?;
    seal_graph_chair(&context, context.identity.project, sibling_graph, "chair-a")?;
    let foreign_graph = "lane-read-foreign";
    bind_sibling_graph(
        &context,
        &rig,
        context.identity.project,
        &rig.project,
        foreign_graph,
    )?;
    seal_graph_chair(&context, context.identity.project, foreign_graph, "chair-b")?;
    // The chair index is project-scoped: a graph from another project stays
    // outside this lane even when the same session submitted it.
    let other_project = provenant_core::ProjectId::from_bytes([9; 16]);
    let other_project_graph = "lane-read-other-project";
    bind_sibling_graph(
        &context,
        &rig,
        other_project,
        &rig.project,
        other_project_graph,
    )?;
    seal_graph_chair(&context, other_project, other_project_graph, "chair-a")?;
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
    let relay = |arguments: serde_json::Value| {
        provenant_cli::fabric::graph_relay::serve_with_read(
            &context,
            &caller.to_string(),
            arguments,
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
    let read = |input| relay(json!({"op":"read","input":input}));
    // A child provider may work in another repository while remaining in
    // this graph. Host journal identity must outlive that cwd change.
    let foreign = rig.root.join("foreign-project");
    std::fs::create_dir_all(&foreign)?;
    let mut foreign_holder = holder.clone();
    foreign_holder.launch.as_mut().ok_or("host launch")?.cwd = foreign;
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
    // A graph from this lane's chair session is readable: full output, status
    // and trace, with no new flag or grant. Spec: docs/specs/fabric.md#lane-graph-read-scope
    let sibling_output =
        read(json!({"op":"output","id":format!("{sibling_graph}/parent--success")}))?;
    assert_eq!(
        sibling_output["bytes"]["data"], "child result reached parent",
        "{sibling_output}"
    );
    let sibling_status = read(json!({"op":"status","id":sibling_graph}))?;
    assert!(
        sibling_status["rows"]
            .as_array()
            .ok_or("sibling status rows")?
            .iter()
            .any(|row| row["node_id"] == "parent--success" && row["state"] == "succeeded")
    );
    let sibling_trace = read(json!({"op":"trace","id":sibling_graph}))?;
    assert!(
        !sibling_trace["events"]
            .as_array()
            .ok_or("sibling trace events")?
            .is_empty()
    );
    // A graph from another chair session stays not_found even though the host
    // can read it. Spec: docs/specs/fabric.md#lane-graph-read-scope
    let cross_session =
        read(json!({"op":"output","id":format!("{foreign_graph}/parent--success")}));
    assert!(
        matches!(
            &cross_session,
            Err(ServiceError::Facade(FacadeError::GraphRelayRefused { refusal_code, .. }))
                if refusal_code == "not_found"
        ),
        "cross-session graph must be not_found"
    );
    let cross_project = read(json!({
        "op": "output",
        "id": format!("{other_project_graph}/parent--success"),
    }));
    assert!(
        matches!(
            &cross_project,
            Err(ServiceError::Facade(FacadeError::GraphRelayRefused { refusal_code, .. }))
                if refusal_code == "not_found"
        ),
        "another project's graph must be not_found"
    );
    // Writes never follow the read scope: a target naming the sibling graph is
    // refused, and the sibling graph cannot be edited or stopped. Add and wait
    // bind the calling node, so they cannot name another graph at all.
    let read_sibling = format!("{sibling_graph}/parent--success");
    let edit = relay(json!({
        "op": "edit",
        "id": sibling_graph,
        "patch": {"schema_version": 1},
    }));
    assert!(
        matches!(
            &edit,
            Err(ServiceError::Facade(FacadeError::GraphRelayRefused { refusal_code, .. }))
                if refusal_code == "not_found"
        ),
        "editing a sibling graph must be refused: {edit:?}"
    );
    let stop = relay(json!({"op": "stop", "node": read_sibling}));
    assert!(
        matches!(
            &stop,
            Err(ServiceError::Facade(FacadeError::GraphRelayRefused { refusal_code, .. }))
                if refusal_code == "confinement_refused"
        ),
        "stopping a sibling graph's node must be refused: {stop:?}"
    );
    let relay = provenant_fabric_runtime::relay::HostRelay::start_with_graph(
        provenant_providers::LaneRuntime::fabric_socket(caller),
        Default::default(),
        |_, _, _| Err(ServiceError::FabricOwnerUnavailable),
        move |arguments, cancel, within| {
            let budget = provenant_fabric_runtime::relay::host_budget(&clock, within)?;
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
        (provenant_core::consumer_env::LANE_DEPTH.into(), "1".into()),
        (
            provenant_core::consumer_env::PARENT_ATTEMPT_ID.into(),
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
    // The native lane CLI path follows the same chair-session rule.
    // Spec: docs/specs/fabric.md#lane-graph-read-scope
    let sibling_result = lane_command(
        &rig,
        &[
            "output",
            &format!("{sibling_graph}/parent--success"),
            "--json",
        ],
        0,
    )?;
    assert_eq!(
        sibling_result["data"]["bytes"]["data"],
        "child result reached parent"
    );
    let denied = lane_command(
        &rig,
        &[
            "output",
            &format!("{foreign_graph}/parent--success"),
            "--json",
        ],
        3,
    )?;
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
            .map(|path| provenant_core::CredentialRoot {
                path: path.clone(),
                scope: provenant_core::CredentialScope::Subtree,
                source: "account registry fixture".into(),
                kind: provenant_core::CredentialKind::Store,
            }),
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
        credential_roots: policy.mandatory().credential_roots().clone(),
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
            confinement: provenant_confinement::permissions::ResolvedConfinementPolicy::default()
                .without_credentials(),
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
